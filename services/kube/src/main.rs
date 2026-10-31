//! kube: declarative pods, a small Kubernetes for a cluster of machines.
//! Every machine runs one; together they share the cluster's records on
//! the server (api.rs; a machine without a server is a cluster of one).
//! Clients `apply` what they want (records); then
//! - each kube keeps its node's record fresh (a lease), and one of them,
//!   the leader (a lease too: `leader`), runs the controller (control.rs):
//!   it decides which pods there are and puts each on a node;
//! - each kube's kubelet (kubelet.rs) runs the pods on its node in the
//!   machine's dock and writes how they are.
//!
//! - Deployment: N pods of an image. A new template (or a new version of
//!   its Config) is a new revision, rolled out one pod at a time.
//! - Service: pods of a Deployment with `serves = NAME` get `listen`, pods
//!   with `uses = [NAME]` get `svc:NAME` (libsys::svc); a connection is a
//!   channel end handed to a ready pod of NAME (on the same node). No
//!   addresses: what a pod may call is what it was given.
//! - Config: files and environment, a layer on the Deployment's image.
//! - Space: a quota a Deployment's pods share (on each node).
//!
//! Its capabilities: a dock client and a store client.
#![no_std]
#![no_main]

extern crate alloc;

mod api;
mod control;
mod funcs;
mod kubelet;
mod manifest;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::dock::Dock;
use libsys::store::Store;
use libsys::{Channel, Error, Handle, Port, clock_ns, ns, signal};

use api::{Api, Rec};
use control::{Controller, node_up};
use kubelet::{Kubelet, LINK, LISTEN, revision};
use manifest::{Object, Restart, Template};

libsys::entry!(main);

const SEC: u64 = 1_000_000_000;
/// Port keys: clients are 0.., dock's events EVENTS, the server's ingress
/// INGRESS, answers to its requests REPLY + i.
const EVENTS: u64 = 1 << 32;
const INGRESS: u64 = 1 << 35;
const REPLY: u64 = 1 << 36;
/// The records are read again this often…
const POLL: u64 = SEC;
/// …and dock's pods looked at, serving pods pinged, this often.
const TICK: u64 = 2 * SEC;
/// The leader's lease (ms).
const LEADER_LEASE: u64 = 6000;
/// Revisions kept per Deployment.
const REVISIONS: u32 = 10;

struct Kube {
    port: Port,
    dock: Dock,
    store: Store,
    api: Api,
    kubelet: Kubelet,
    control: Controller,
    clients: Vec<Option<Channel>>,
    leader: bool,
    /// Requests from outside for exposed Functions (doc/ingress.md), and
    /// the ones waiting for their answer: (request id, kube's end).
    ingress: Option<Channel>,
    replies: Vec<Option<(u64, Channel)>>,
}

/// How long ago server time `t` (ms) was.
fn age(api: &Api, t: u64) -> String {
    let s = api.now().saturating_sub(t) / 1000;
    match s {
        0..120 => format!("{s}s"),
        120..7200 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

impl Kube {
    fn add_client(&mut self, c: Channel) {
        self.clients.push(Some(c));
        let _ = self.port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, self.clients.len() as u64 - 1);
    }

    /// This node's record (its lease) and the leader's.
    fn heartbeat(&mut self) {
        let now = self.api.now();
        let created = self.api.get(&format!("nodes/{}", self.api.node)).filter(|r| r.field("session") == Some(&self.api.session)).map_or(now, |r| r.num("created"));
        let rec = format!("arch {}\nsession {}\npods {}\ncreated {created}\n", libsys::ARCH, self.api.session, self.kubelet.pods.len());
        let key = format!("nodes/{}", self.api.node);
        self.api.put(&key, None, &rec);
        let me = format!("node {}\n", self.api.node);
        let was = self.leader;
        self.leader = match self.api.get("leader").cloned() {
            None => matches!(self.api.put("leader", Some(0), &me), api::Write::Done),
            Some(r) if r.field("node") == Some(&self.api.node) => matches!(self.api.put("leader", Some(r.version), &me), api::Write::Done),
            Some(r) if now.saturating_sub(r.updated) >= LEADER_LEASE => {
                matches!(self.api.put("leader", Some(r.version), &me), api::Write::Done)
            }
            Some(_) => false,
        };
        if self.leader && !was && self.api.is_cluster() {
            libsys::println!("kube: {} is the leader", self.api.node);
        }
    }

    /// One turn of the loop: the controller (if this is the leader), then this node's pods.
    fn cycle(&mut self) {
        if self.leader {
            self.control.run(&mut self.api, &self.store);
        }
        self.kubelet.sync(&mut self.api);
    }

    /// What revision `rev` of Deployment `d` runs: its image, with its Config on top.
    fn image_for(&self, d: &str, rev: u32, t: &Template) -> Result<String, String> {
        let Some(cname) = &t.config else { return Ok(t.image.clone()) };
        let c = self.api.get(&format!("config/{cname}")).ok_or_else(|| format!("no config {cname}"))?;
        let Ok(Object::Config { files: cfiles, env, .. }) = manifest::parse(config_text(c)) else { return Err(format!("bad config {cname}")) };
        let e = |what: &str, err: Error| format!("{what}: {err:?}");
        let mut files = Vec::new();
        for (path, text) in &cfiles {
            let id = libsys::image::put(&self.store, text.as_bytes()).map_err(|err| e("store", err))?;
            files.push((path.clone(), Some((id, text.len() as u64))));
        }
        let layer = libsys::image::layer(&self.store, &files).map_err(|err| e("layer", err))?;
        let name = format!("kube-{d}:r{rev}");
        libsys::image::derive(&self.store, &t.image, &name, layer, None, &env).map_err(|err| e(&t.image, err))?;
        // Other nodes pull it from the registry.
        if self.api.is_cluster() {
            let _ = self.store.push(&name);
        }
        Ok(name)
    }

    /// A new revision of Deployment `d` from `manifest` (its text).
    fn new_revision(&mut self, d: &str, t: &Template, manifest: &str) -> Result<u32, String> {
        let cur = self.api.get(&format!("deploy/{d}")).map_or(0, |r| r.num("rev") as u32);
        let latest = self.api.prefix(&format!("rev/{d}/")).filter_map(|(k, _)| k.rsplit('/').next()?.parse::<u32>().ok()).max().unwrap_or(0);
        let rev = cur.max(latest) + 1;
        let image = self.image_for(d, rev, t)?;
        self.api.put(&format!("rev/{d}/{rev}"), None, &format!("image {image}\n---\n{manifest}"));
        if rev > REVISIONS {
            self.api.delete(&format!("rev/{d}/{}", rev - REVISIONS));
        }
        let now = self.api.now();
        self.api.update(&format!("deploy/{d}"), |old| {
            let replicas = old.and_then(|o| api::field(o, "replicas")).unwrap_or("1");
            let created = old.and_then(|o| api::field(o, "created")).map(String::from).unwrap_or_else(|| format!("{now}"));
            format!("replicas {replicas}\nrev {rev}\ncreated {created}\n")
        });
        if cur > 0 {
            self.api.event(d, &format!("Rolling out revision {rev}"));
        }
        Ok(rev)
    }

    fn set_replicas(&mut self, d: &str, n: u32) {
        self.api.update(&format!("deploy/{d}"), |old| {
            let old = old.unwrap_or("");
            format!("replicas {n}\nrev {}\ncreated {}\n", api::field(old, "rev").unwrap_or("0"), api::field(old, "created").unwrap_or("0"))
        });
    }

    fn current(&self, d: &str) -> Option<(u32, String, Template)> {
        let rev = self.api.get(&format!("deploy/{d}"))?.num("rev") as u32;
        let (image, t) = revision(&self.api, d, rev)?;
        Some((rev, image, t))
    }

    fn apply(&mut self, text: &str) -> Result<String, String> {
        match manifest::parse(text)? {
            Object::Space { name, .. } => {
                let key = format!("space/{name}");
                match self.api.get(&key) {
                    Some(r) if r.value == text => Ok(format!("space/{name} unchanged")),
                    Some(_) => Err(format!("space/{name} exists; a space's limits cannot change")),
                    None => {
                        self.api.put(&key, Some(0), text);
                        Ok(format!("space/{name} created"))
                    }
                }
            }
            Object::Config { name, .. } => {
                let key = format!("config/{name}");
                let (verb, version) = match self.api.get(&key) {
                    Some(r) if config_text(r) == text => return Ok(format!("config/{name} unchanged")),
                    Some(r) => ("configured", r.num("version") + 1),
                    None => ("created", 1),
                };
                self.api.put(&key, None, &format!("version {version}\n---\n{text}"));
                // Deployments using it get a new revision.
                let mut out = format!("config/{name} {verb}");
                let users: Vec<String> = self.api.prefix("deploy/").map(|(k, _)| String::from(&k[7..])).collect();
                for d in users {
                    let Some((rev, _, t)) = self.current(&d) else { continue };
                    if t.config.as_deref() != Some(name.as_str()) {
                        continue;
                    }
                    let m = self.api.get(&format!("rev/{d}/{rev}")).and_then(|r| r.value.split_once("---\n")).map(|(_, m)| String::from(m));
                    let rev = self.new_revision(&d, &t, &m.unwrap_or_default())?;
                    out += &format!("\ndeployment/{d} rolling out revision {rev}");
                }
                Ok(out)
            }
            Object::Function { name, .. } => {
                if self.api.get(&format!("deploy/{name}")).is_some() {
                    return Err(format!("{name} is a Deployment"));
                }
                let key = format!("fn/{name}");
                let (verb, version) = match self.api.get(&key) {
                    Some(r) if config_text(r) == text => return Ok(format!("function/{name} unchanged")),
                    Some(r) => ("configured", r.num("version") + 1),
                    None => ("created", 1),
                };
                self.api.put(&key, None, &format!("version {version}\n---\n{text}"));
                Ok(format!("function/{name} {verb}"))
            }
            Object::Deployment { name, replicas, template } => {
                if self.api.get(&format!("fn/{name}")).is_some() {
                    return Err(format!("{name} is a Function"));
                }
                if let Some(s) = &template.space
                    && self.api.get(&format!("space/{s}")).is_none()
                {
                    return Err(format!("no space {s} (apply a Space first)"));
                }
                if let Some(c) = &template.config
                    && self.api.get(&format!("config/{c}")).is_none()
                {
                    return Err(format!("no config {c} (apply a Config first)"));
                }
                let Some((_, _, cur)) = self.current(&name) else {
                    self.new_revision(&name, &template, text)?;
                    self.set_replicas(&name, replicas);
                    return Ok(format!("deployment/{name} created"));
                };
                let old = self.api.get(&format!("deploy/{name}")).map_or(0, |r| r.num("replicas") as u32);
                if cur == template && old == replicas {
                    return Ok(format!("deployment/{name} unchanged"));
                }
                self.set_replicas(&name, replicas);
                if cur == template {
                    return Ok(format!("deployment/{name} configured"));
                }
                let rev = self.new_revision(&name, &template, text)?;
                Ok(format!("deployment/{name} configured (rolling out revision {rev})"))
            }
        }
    }

    /// (pod, its record, its status) of every pod, of Deployment `d` if given.
    fn pods(&self, d: Option<&str>) -> Vec<(String, Rec, Option<Rec>)> {
        self.api
            .prefix("pod/")
            .filter(|(_, r)| d.is_none_or(|d| r.field("deploy") == Some(d)))
            .map(|(k, r)| (String::from(&k[4..]), r.clone(), self.api.get(&format!("status/{}", &k[4..])).cloned()))
            .collect()
    }

    fn get_pods(&self) -> String {
        let mut out = format!("{:<14}{:<7}{:<18}{:<10}{:<5}{:<6}{}\n", "NAME", "READY", "STATUS", "RESTARTS", "REV", "AGE", "NODE");
        for (name, r, s) in self.pods(None) {
            let node = r.field("node").unwrap_or("-");
            let s = s.filter(|s| s.field("node") == Some(node));
            let status = match s.as_ref().and_then(|s| s.field("phase")) {
                _ if node == "-" => "Unschedulable",
                Some("Running") => "Running",
                Some("BackOff") => "CrashLoopBackOff",
                Some("Done") if s.as_ref().is_some_and(|s| s.num("code") == 0) => "Completed",
                Some("Done") => "Error",
                _ => "Pending",
            };
            let ready = if s.as_ref().is_some_and(|s| s.num("ready") == 1) { "1/1" } else { "0/1" };
            let restarts = s.as_ref().map_or(0, |s| s.num("restarts"));
            let created = age(&self.api, r.num("created"));
            out += &format!("{:<14}{:<7}{:<18}{:<10}{:<5}{:<6}{}\n", name, ready, status, restarts, r.num("rev"), created, node);
        }
        out
    }

    fn ready_count(&self, d: &str) -> usize {
        self.pods(Some(d)).iter().filter(|(_, r, s)| s.as_ref().is_some_and(|s| s.num("ready") == 1 && s.field("node") == r.field("node"))).count()
    }

    fn get_deploys(&self) -> String {
        let mut out = format!("{:<10}{:<10}{:<7}{:<5}{:<16}{}\n", "NAME", "SPACE", "READY", "REV", "IMAGE", "AGE");
        for (k, r) in self.api.prefix("deploy/") {
            let d = &k[7..];
            let Some((rev, _, t)) = self.current(d) else { continue };
            let ready = format!("{}/{}", self.ready_count(d), r.num("replicas"));
            let space = t.space.as_deref().unwrap_or("default");
            out += &format!("{:<10}{:<10}{:<7}{:<5}{:<16}{}\n", d, space, ready, rev, t.image, age(&self.api, r.num("created")));
        }
        out
    }

    fn get_nodes(&self) -> String {
        let leader = self.api.get("leader").and_then(|r| r.field("node")).unwrap_or("");
        let mut out = format!("{:<12}{:<9}{:<10}{:<6}{}\n", "NAME", "ARCH", "STATUS", "PODS", "ROLE");
        for (k, r) in self.api.prefix("nodes/") {
            let n = &k[6..];
            let up = node_up(&self.api, n);
            let pods = self.api.prefix("pod/").filter(|(_, p)| p.field("node") == Some(n)).count();
            let role = if n == leader && up { "leader" } else { "-" };
            out += &format!("{:<12}{:<9}{:<10}{:<6}{}\n", n, r.field("arch").unwrap_or("?"), if up { "Ready" } else { "NotReady" }, pods, role);
        }
        out
    }

    fn get_services(&self) -> String {
        let mut uses: Vec<(String, Template)> = Vec::new();
        for (k, _) in self.api.prefix("deploy/") {
            if let Some((_, _, t)) = self.current(&k[7..]) {
                uses.push((String::from(&k[7..]), t));
            }
        }
        // Only services: not fn:, kv:, sql:, redis: or http.
        let mut names: Vec<&str> =
            uses.iter().flat_map(|(_, t)| t.serves.iter().chain(&t.uses).map(String::as_str)).filter(|u| !u.contains(':') && *u != "http").collect();
        names.sort();
        names.dedup();
        let mut out = format!("{:<10}{:<10}{:<8}{:<8}{}\n", "SERVICE", "READY", "USERS", "WAITING", "CONNECTIONS");
        for s in names {
            let (mut pods, mut ready, mut users) = (0, 0, 0);
            for (d, t) in &uses {
                if t.serves.as_deref() == Some(s) {
                    pods += self.pods(Some(d)).len();
                    ready += self.ready_count(d);
                }
                if t.uses.iter().any(|u| u == s) {
                    users += self.pods(Some(d)).len();
                }
            }
            let handed = self.kubelet.handed.iter().find(|(x, _)| x == s).map_or(0, |(_, n)| *n);
            out += &format!("{:<10}{:<10}{:<8}{:<8}{}\n", s, format!("{ready}/{pods}"), users, self.kubelet.waiting(s), handed);
        }
        out
    }

    /// Functions, and what each did on this node.
    fn get_fns(&self) -> String {
        let mut out =
            format!("{:<10}{:<10}{:<9}{:<6}{:<10}{:<9}{:<9}{:<7}{}\n", "FUNCTION", "IMAGE", "HERE", "BUSY", "REQUESTS", "COLD", "START", "AVG", "URL");
        let ms = |ns: u64, n: u64| if n == 0 { String::from("-") } else { format!("{}ms", ns / n / 1_000_000) };
        for (k, _) in self.api.prefix("fn/") {
            let f = &k[3..];
            let Some((_, spec)) = funcs::spec(&self.api, f) else { continue };
            let here = self.kubelet.insts.iter().filter(|x| x.func == f).count();
            let busy = self.kubelet.insts.iter().filter(|x| x.func == f && matches!(x.state, funcs::State::Busy { .. })).count();
            let (req, cold, cold_ns, n, busy_ns) =
                self.kubelet.fn_stat(f).map_or((0, 0, 0, 0, 0), |s| (s.requests, s.cold, s.cold_ns, s.answered, s.busy_ns));
            let url = match (&self.api.fn_base, spec.expose) {
                (Some(b), true) => format!("{b}/fn/{f}/"),
                (None, true) => format!("/fn/{f}/"),
                _ => String::from("-"),
            };
            out += &format!(
                "{:<10}{:<10}{:<9}{:<6}{:<10}{:<9}{:<9}{:<7}{}\n",
                f,
                spec.image,
                format!("{here}/{}", spec.max),
                busy,
                req,
                cold,
                ms(cold_ns, cold),
                ms(busy_ns, n),
                url
            );
        }
        out
    }

    fn describe_fn(&self, name: &str) -> Result<String, String> {
        let (version, s) = funcs::spec(&self.api, name).ok_or_else(|| format!("no function {name}"))?;
        let none = |v: &[String], sep: &str| if v.is_empty() { String::from("-") } else { v.join(sep) };
        let mut out = format!("Name:       {name} (Function, version {version})\n");
        out += &format!("Image:      {}\n", s.image);
        out += &format!("Args:       {}\n", none(&s.args, " "));
        out += &format!("Uses:       {}\n", none(&s.uses, ", "));
        out += &format!("Limits:     timeout {}s, max {} per node, idle {}s, reuse {}\n", s.timeout, s.max, s.idle, s.reuse);
        if s.expose {
            out += &format!("Exposed:    {}/fn/{name}/\n", self.api.fn_base.as_deref().unwrap_or(""));
        }
        out += &format!("Resources:  mem {}, tasks {} (per instance)\n", s.mem, s.tasks);
        if let Some(st) = self.kubelet.fn_stat(name) {
            out += &format!(
                "Here:       {} requests, {} cold starts, {} timeouts, {} waiting\n",
                st.requests,
                st.cold,
                st.timeouts,
                self.kubelet.fn_waiting(name)
            );
        }
        out += "Instances (this node):\n";
        for x in self.kubelet.insts.iter().filter(|x| x.func == name) {
            let st = match x.state {
                funcs::State::Starting => "starting",
                funcs::State::Idle => "idle",
                funcs::State::Busy { .. } => "busy",
            };
            out += &format!("  {:<14}{st}\n", x.name);
        }
        out += "Events:\n";
        if let Some(e) = self.api.get(&format!("events/{name}")) {
            for l in e.value.lines() {
                let (t, m) = l.split_once(' ').unwrap_or((l, ""));
                out += &format!("  {:<6}{m}\n", age(&self.api, t.parse().unwrap_or(0)));
            }
        }
        Ok(out)
    }

    fn get_configs(&self) -> String {
        let mut out = format!("{:<12}{:<8}{:<6}{}\n", "CONFIG", "VERSION", "FILES", "ENV");
        for (k, r) in self.api.prefix("config/") {
            let Ok(Object::Config { files, env, .. }) = manifest::parse(config_text(r)) else { continue };
            let env: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
            let env = if env.is_empty() { String::from("-") } else { env.join(",") };
            out += &format!("{:<12}{:<8}{:<6}{}\n", &k[7..], r.num("version"), files.len(), env);
        }
        out
    }

    fn describe(&self, name: &str) -> Result<String, String> {
        let dr = self.api.get(&format!("deploy/{name}")).ok_or_else(|| format!("no deployment {name}"))?;
        let (rev, image, t) = self.current(name).ok_or_else(|| format!("no revision of {name}"))?;
        let rolling = self.api.get(&format!("ctl/{name}")).is_some_and(|r| r.num("rolling") == 1);
        let none = |v: &[String], sep: &str| if v.is_empty() { String::from("-") } else { v.join(sep) };
        let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| String::from("-"));
        let restart = match t.restart {
            Restart::Always => "always",
            Restart::OnFailure => "on-failure",
            Restart::Never => "never",
        };
        let mut out = format!("Name:       {name}\n");
        out += &format!("Space:      {}\n", t.space.as_deref().unwrap_or("default"));
        out += &format!("Revision:   {rev}{}\n", if rolling { " (rolling out)" } else { "" });
        out += &format!("Image:      {}{}\n", t.image, if image != t.image { format!(" (runs {image})") } else { String::new() });
        out += &format!("Args:       {}\n", none(&t.args, " "));
        out += &format!("Replicas:   {} wanted, {} ready\n", dr.num("replicas"), self.ready_count(name));
        out += &format!("Restart:    {restart}\n");
        out += &format!("Resources:  mem {}, tasks {} (per pod)\n", t.mem, t.tasks);
        out += &format!("Labels:     {}\n", none(&t.labels, ", "));
        out += &format!("Serves:     {}\n", opt(&t.serves));
        out += &format!("Uses:       {}\n", none(&t.uses, ", "));
        out += &format!("Config:     {}\n", opt(&t.config));
        out += &format!("Node:       {}\n", t.node.as_deref().unwrap_or("any"));
        out += "Pods:\n";
        for (p, r, s) in self.pods(Some(name)) {
            let node = r.field("node").unwrap_or("-");
            let s = s.filter(|s| s.field("node") == Some(node));
            let reason = s.as_ref().and_then(|s| s.field("reason")).unwrap_or("");
            let state = match s.as_ref().and_then(|s| s.field("phase")) {
                _ if node == "-" => String::from("Unschedulable: no node fits"),
                Some("Running") if s.as_ref().is_some_and(|s| s.num("ready") == 1) => {
                    format!("Running for {}, ready", age(&self.api, s.as_ref().map_or(0, |s| s.num("started"))))
                }
                Some("Running") => format!("Running for {}, not ready", age(&self.api, s.as_ref().map_or(0, |s| s.num("started")))),
                Some("BackOff") => format!("{reason}; restart in {}s", s.as_ref().map_or(0, |s| s.num("wait"))),
                Some("Done") => format!("Done: {reason}"),
                _ if reason.is_empty() => String::from("Pending"),
                _ => format!("Pending: {reason}"),
            };
            let restarts = s.as_ref().map_or(0, |s| s.num("restarts"));
            out += &format!("  {:<14}rev {:<3}restarts {:<4}{:<9}{}\n", p, r.num("rev"), restarts, node, state);
        }
        out += "Events:\n";
        if let Some(e) = self.api.get(&format!("events/{name}")) {
            for l in e.value.lines() {
                let (t, m) = l.split_once(' ').unwrap_or((l, ""));
                out += &format!("  {:<6}{m}\n", age(&self.api, t.parse().unwrap_or(0)));
            }
        }
        Ok(out)
    }

    fn rollout(&mut self, what: &str, name: &str) -> Result<String, String> {
        let (rev, _, _) = self.current(name).ok_or_else(|| format!("no deployment {name}"))?;
        let want = self.api.get(&format!("deploy/{name}")).map_or(0, |r| r.num("replicas"));
        match what {
            "status" => {
                let pods = self.pods(Some(name));
                let new = pods.iter().filter(|(_, r, _)| r.num("rev") as u32 == rev).count();
                let ready = pods
                    .iter()
                    .filter(|(_, r, s)| r.num("rev") as u32 == rev && s.as_ref().is_some_and(|s| s.num("ready") == 1 && s.field("node") == r.field("node")))
                    .count();
                let old = pods.len() - new;
                let rolling = self.api.get(&format!("ctl/{name}")).is_some_and(|r| r.num("rolling") == 1);
                Ok(if !rolling && old == 0 && ready as u64 >= want {
                    format!("deployment/{name} revision {rev}: complete ({ready}/{want} ready)")
                } else {
                    format!("deployment/{name} revision {rev}: {ready}/{want} new pods ready, {old} old")
                })
            }
            "history" => {
                let mut revs: Vec<(u32, String)> = self
                    .api
                    .prefix(&format!("rev/{name}/"))
                    .filter_map(|(k, r)| Some((k.rsplit('/').next()?.parse().ok()?, String::from(r.field("image")?))))
                    .collect();
                revs.sort();
                let mut out = format!("{:<5}{}\n", "REV", "IMAGE");
                for (r, image) in revs {
                    out += &format!("{:<5}{image}\n", r);
                }
                Ok(out)
            }
            "undo" => {
                let back = self
                    .api
                    .prefix(&format!("rev/{name}/"))
                    .filter_map(|(k, _)| k.rsplit('/').next()?.parse::<u32>().ok())
                    .filter(|r| *r < rev)
                    .max()
                    .ok_or_else(|| format!("deployment/{name} has no earlier revision"))?;
                let (_, t) = revision(&self.api, name, back).ok_or("no such revision")?;
                let m = self.api.get(&format!("rev/{name}/{back}")).and_then(|r| r.value.split_once("---\n")).map(|(_, m)| String::from(m));
                let rev = self.new_revision(name, &t, &m.unwrap_or_default())?;
                Ok(format!("deployment/{name} rolling back: revision {rev} = revision {back}"))
            }
            _ => Err(String::from("rollout status|history|undo NAME")),
        }
    }

    fn command(&mut self, msg: &str, conn: Option<Handle>) -> Result<String, String> {
        let (line, body) = msg.split_once('\n').unwrap_or((msg, ""));
        let words: Vec<&str> = line.split_whitespace().collect();
        if let Some(h) = conn {
            // `connect SERVICE` / `connect fn:F` with a channel end: the caller
            // (the shell) calls a service or a Function; `connect kv:K`: the
            // channel end is for requests to KV namespace K.
            return match words.as_slice() {
                ["connect", s] => {
                    match s.split_once(':') {
                        Some(("fn", f)) => self.kubelet.fn_request(&mut self.api, f, h),
                        Some(("kv", k)) => self.kubelet.add_kv(k, h),
                        _ => self.kubelet.connect(s, h),
                    }
                    Ok(String::from("ok"))
                }
                _ => {
                    let _ = h.close();
                    Err(String::from("a handle only goes with connect SERVICE"))
                }
            };
        }
        let deploy = |k: &Kube, n: &str| if k.api.get(&format!("deploy/{n}")).is_some() { Ok(()) } else { Err(format!("no deployment {n}")) };
        match words.as_slice() {
            ["apply"] => self.apply(body),
            ["get"] => Ok(format!("{}\n{}", self.get_deploys(), self.get_pods())),
            ["get", "pods" | "pod" | "po"] => Ok(self.get_pods()),
            ["get", "deployments" | "deployment" | "deploy"] => Ok(self.get_deploys()),
            ["get", "nodes" | "node"] => Ok(self.get_nodes()),
            ["get", "services" | "service" | "svc"] => Ok(self.get_services()),
            ["get", "configs" | "config"] => Ok(self.get_configs()),
            ["get", "functions" | "function" | "fn"] => Ok(self.get_fns()),
            ["get", "spaces" | "space"] => self.dock.request("spaces").map_err(|e| format!("dock: {e:?}")),
            ["get", what] => Err(format!("unknown resource {what} (pods, deploy, nodes, svc, fn, configs, spaces)")),
            ["describe", name] if self.api.get(&format!("fn/{name}")).is_some() => self.describe_fn(name),
            ["describe", name] | ["describe", "deploy" | "deployment", name] => self.describe(name),
            ["rollout", what, name] => self.rollout(what, name),
            ["scale", name, n] => {
                let n: u32 = n.parse().map_err(|_| format!("bad count {n}"))?;
                deploy(self, name)?;
                self.set_replicas(name, n);
                Ok(format!("deployment/{name} scaled to {n}"))
            }
            // The record goes; each node's dock keeps the quota until it reboots.
            ["delete", "space", name] => {
                let users = self.api.prefix("deploy/").map(|(k, _)| String::from(&k[7..])).collect::<Vec<_>>();
                if users.iter().any(|d| self.current(d).is_some_and(|(_, _, t)| t.space.as_deref() == Some(*name))) {
                    return Err(format!("space {name} is in use"));
                }
                self.api.get(&format!("space/{name}")).ok_or_else(|| format!("no space {name}"))?;
                self.api.delete(&format!("space/{name}"));
                Ok(format!("space/{name} deleted"))
            }
            ["delete", "config", name] => {
                let users = self.api.prefix("deploy/").map(|(k, _)| String::from(&k[7..])).collect::<Vec<_>>();
                if users.iter().any(|d| self.current(d).is_some_and(|(_, _, t)| t.config.as_deref() == Some(*name))) {
                    return Err(format!("config {name} is in use"));
                }
                self.api.get(&format!("config/{name}")).ok_or_else(|| format!("no config {name}"))?;
                self.api.delete(&format!("config/{name}"));
                Ok(format!("config/{name} deleted"))
            }
            ["delete", name] if self.api.get(&format!("fn/{name}")).is_some() => {
                self.api.delete(&format!("fn/{name}"));
                self.api.delete(&format!("events/{name}"));
                Ok(format!("function/{name} deleted"))
            }
            ["delete", name] | ["delete", "deploy" | "deployment", name] => {
                deploy(self, name)?;
                self.api.delete(&format!("deploy/{name}"));
                let revs: Vec<String> = self.api.prefix(&format!("rev/{name}/")).map(|(k, _)| String::from(k)).collect();
                for k in revs {
                    self.api.delete(&k);
                }
                Ok(format!("deployment/{name} deleted"))
            }
            ["logs", pod] => {
                let r = self.api.get(&format!("pod/{pod}")).ok_or_else(|| format!("no pod {pod}"))?;
                let node = r.field("node").unwrap_or("-");
                if node != self.api.node {
                    return Err(format!("{pod} runs on {node}; its logs stay on that machine"));
                }
                let p = self.kubelet.pods.iter().find(|p| p.name == *pod).ok_or_else(|| format!("no pod {pod}"))?;
                let id = p.dock_id.ok_or_else(|| format!("{pod} has not run"))?;
                self.dock.request(&format!("logs {id}")).map_err(|e| format!("dock: {e:?}"))
            }
            _ => Err(String::from(
                "commands: apply, get [pods|deploy|nodes|svc|fn|configs|spaces], describe NAME, rollout status|history|undo NAME, scale NAME N, delete [config|space] NAME, logs POD",
            )),
        }
    }

    fn client_message(&mut self, i: usize) {
        let Some(c) = self.clients[i] else { return };
        let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES];
        let mut hs = [Handle(0); 1];
        loop {
            match c.read(&mut buf, &mut hs) {
                // Only a channel: another client.
                Ok((0, 1)) => self.add_client(Channel(hs[0])),
                Ok((n, nh)) => {
                    let msg = String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                    let r = match self.command(&msg, (nh == 1).then_some(hs[0])) {
                        Ok(r) => r,
                        Err(m) => format!("error {m}"),
                    };
                    // Act on it at once, so the next `get` already shows it.
                    self.cycle();
                    let _ = c.write(r.as_bytes(), &[]);
                }
                Err(Error::ShouldWait) => {
                    let _ = self.port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, i as u64);
                    return;
                }
                Err(_) => {
                    let _ = c.close();
                    self.clients[i] = None;
                    return;
                }
            }
        }
    }

    /// The server sent requests: `REQ ID NAME\n` + an HTTP envelope each.
    fn ingress_message(&mut self) {
        let Some(ing) = self.ingress else { return };
        let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES];
        loop {
            match ing.read(&mut buf, &mut []) {
                Ok((n, _)) => {
                    let msg = &buf[..n];
                    let Some(nl) = msg.iter().position(|&c| c == b'\n') else { continue };
                    let head = String::from_utf8_lossy(&msg[..nl]).into_owned();
                    let w: Vec<&str> = head.split(' ').collect();
                    let ["REQ", id, name] = w.as_slice() else { continue };
                    let Ok(id) = id.parse::<u64>() else { continue };
                    let name = String::from(*name);
                    if !funcs::spec(&self.api, &name).is_some_and(|(_, s)| s.expose) {
                        self.answer(id, b"HTTP 404\nContent-Type: text/plain; charset=utf-8\n\nno such function here, or it is not exposed\n");
                        continue;
                    }
                    let Ok((mine, theirs)) = Channel::create() else { continue };
                    if mine.write(&msg[nl + 1..], &[]).is_err() {
                        continue;
                    }
                    let i = kubelet::slot(&mut self.replies, (id, mine));
                    let _ = self.port.bind(mine.0, signal::READABLE | signal::PEER_CLOSED, REPLY + i as u64);
                    self.kubelet.fn_request(&mut self.api, &name, theirs.0);
                }
                Err(Error::ShouldWait) => {
                    let _ = self.port.bind(ing.0, signal::READABLE, INGRESS);
                    return;
                }
                Err(_) => return,
            }
        }
    }

    /// The instance answered request `replies[i]` (or its channel closed: timed out, failed).
    fn reply_message(&mut self, i: usize) {
        let Some((id, c)) = self.replies.get(i).copied().flatten() else { return };
        let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES];
        let answer = match c.read(&mut buf, &mut []) {
            Ok((n, _)) if buf[..n].starts_with(b"HTTP ") => Vec::from(&buf[..n]),
            // A plain answer (func::serve): text.
            Ok((n, _)) => {
                let mut a = Vec::from(&b"HTTP 200\nContent-Type: text/plain; charset=utf-8\n\n"[..]);
                a.extend_from_slice(&buf[..n]);
                a
            }
            Err(Error::ShouldWait) => {
                let _ = self.port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, REPLY + i as u64);
                return;
            }
            Err(_) => Vec::from(&b"HTTP 504\nContent-Type: text/plain; charset=utf-8\n\nthe function did not answer (timed out or failed)\n"[..]),
        };
        let _ = c.close();
        self.replies[i] = None;
        self.answer(id, &answer);
    }

    fn answer(&self, id: u64, answer: &[u8]) {
        if let Some(ing) = self.ingress {
            let mut m = format!("RESP {id}\n").into_bytes();
            m.extend_from_slice(&answer[..answer.len().min(libsys::MAX_MSG_BYTES - 64)]);
            let _ = ing.write(&m, &[]);
        }
    }

    fn event_message(&mut self, msg: &str) {
        let w: Vec<&str> = msg.split_whitespace().collect();
        let (id, code) = match w.as_slice() {
            ["event", "exit", id, _, code] => (id.parse().ok(), code.parse().ok()),
            ["event", "stop" | "rm", id, _] => (id.parse().ok(), None),
            _ => return,
        };
        if let Some(id) = id {
            self.kubelet.ended(&mut self.api, id, code);
        }
    }
}

/// A Config record's manifest (after its `version` line).
fn config_text(r: &Rec) -> &str {
    r.value.split_once("---\n").map_or(r.value.as_str(), |(_, m)| m)
}

fn main(_arg: usize) -> i64 {
    let (Some(dock), Some(store)) = (ns::get("dock"), ns::get("store")) else { return 1 };
    let dock = Dock(Channel(dock));
    let store = Store(Channel(store));
    let Ok(port) = Port::create() else { return 2 };
    // A second dock client for its events.
    let Ok((events, theirs)) = Channel::create() else { return 2 };
    if dock.0.write(b"", &[theirs.0]).is_err() || events.write(b"watch", &[]).is_err() {
        return 2;
    }
    let mut buf = alloc::vec![0u8; 4096];
    let _ = events.recv(&mut buf, &mut []);
    let mut api = Api::new(store);
    api.refresh();
    let mut k = Kube {
        port,
        dock,
        store,
        api,
        kubelet: Kubelet::new(port, dock, ns::get("backend").map(|h| Store(Channel(h))), libsys::local::Local::get()),
        control: Controller::new(),
        clients: Vec::new(),
        leader: false,
        ingress: ns::get("ingress").map(Channel),
        replies: Vec::new(),
    };
    if let Some(i) = k.ingress {
        let _ = k.port.bind(i.0, signal::READABLE, INGRESS);
    }
    if let Some(c) = ns::get("client") {
        k.add_client(Channel(c));
    }
    let _ = k.port.bind(events.0, signal::READABLE, EVENTS);
    k.heartbeat();
    k.cycle();
    let (mut poll_at, mut tick_at) = (clock_ns() + POLL, clock_ns() + TICK);
    loop {
        let now = clock_ns();
        let wake = k.kubelet.next_wake(poll_at.min(tick_at));
        match k.port.wait(wake.saturating_sub(now).max(1)) {
            Ok(p) if p.key == EVENTS => {
                while let Ok((n, _)) = events.read(&mut buf, &mut []) {
                    let msg = String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                    k.event_message(&msg);
                }
                let _ = k.port.bind(events.0, signal::READABLE, EVENTS);
            }
            Ok(p) if p.key >= REPLY => k.reply_message((p.key - REPLY) as usize),
            Ok(p) if p.key == INGRESS => k.ingress_message(),
            Ok(p) if p.key >= LINK => k.kubelet.link_message(&mut k.api, (p.key - LINK) as usize),
            Ok(p) if p.key >= LISTEN => k.kubelet.listen_message(&mut k.api, (p.key - LISTEN) as usize),
            Ok(p) => k.client_message(p.key as usize),
            Err(Error::TimedOut) => {}
            Err(_) => return 3,
        }
        let now = clock_ns();
        if now >= poll_at {
            k.api.refresh();
            k.heartbeat();
            poll_at = now + POLL;
        }
        if now >= tick_at {
            k.kubelet.tick(&mut k.api);
            tick_at = now + TICK;
        }
        k.cycle();
    }
}
