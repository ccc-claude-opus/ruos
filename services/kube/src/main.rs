//! kube: declarative pods, a small Kubernetes. Clients `apply` what they
//! want (a Deployment: N pods of an image; a Space: a quota for pods) and
//! kube keeps making it so: a control loop compares what is wanted with
//! what dock reports (its `watch` events, and a full `pods` list every
//! 2 s) and starts, restarts (with back-off) or removes pods. Everything
//! it does goes through its one capability, a dock client.
#![no_std]
#![no_main]

extern crate alloc;

mod manifest;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::dock::Dock;
use libsys::{Channel, Error, Handle, Port, clock_ns, ns, signal};

use manifest::{Object, Restart, Template};

libsys::entry!(main);

const SEC: u64 = 1_000_000_000;
/// Port keys: clients are 0.., dock's events EVENTS.
const EVENTS: u64 = 1 << 32;
/// A full look at dock's pods this often.
const RESYNC: u64 = 2 * SEC;
/// Back-off before a restart: 1 s, doubling up to 16 s; back to 1 s after a pod ran 10 s.
const BACKOFF_MIN: u64 = SEC;
const BACKOFF_MAX: u64 = 16 * SEC;
const RAN_LONG: u64 = 10 * SEC;
/// Events kept per Deployment.
const MAX_EVENTS: usize = 12;

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Not started yet, or dock could not start it (`reason`).
    Pending,
    Running,
    /// Ended, to be started again at `until`.
    BackOff,
    /// Ended for good (restart policy).
    Done(i64),
}

struct Pod {
    name: String,
    /// The pod in dock (kept after it ends, for its logs).
    dock_id: Option<u64>,
    phase: Phase,
    restarts: u32,
    created: u64,
    started: u64,
    until: u64,
    backoff: u64,
    /// Why it is Pending, or how it last ended.
    reason: String,
}

struct Deployment {
    name: String,
    replicas: u32,
    template: Template,
    pods: Vec<Pod>,
    /// The next pod's number (names are never reused).
    next: u32,
    created: u64,
    events: Vec<(u64, String)>,
}

impl Deployment {
    fn event(&mut self, msg: String) {
        if self.events.last().is_some_and(|(_, m)| *m == msg) {
            return;
        }
        self.events.push((clock_ns(), msg));
        if self.events.len() > MAX_EVENTS {
            self.events.remove(0);
        }
    }
}

struct Kube {
    port: Port,
    dock: Dock,
    clients: Vec<Option<Channel>>,
    deploys: Vec<Deployment>,
    /// (name, mem, tasks), made in dock.
    spaces: Vec<(String, String, u64)>,
}

fn age(t: u64) -> String {
    let s = clock_ns().saturating_sub(t) / SEC;
    match s {
        0..120 => format!("{s}s"),
        120..7200 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

fn secs(ns: u64) -> u64 {
    ns.div_ceil(SEC)
}

impl Kube {
    fn add_client(&mut self, c: Channel) {
        self.clients.push(Some(c));
        let _ = self.port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, self.clients.len() as u64 - 1);
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.deploys.iter().position(|d| d.name == name)
    }

    /// Removes the pod from dock (stops it if it runs).
    fn forget(&self, p: &mut Pod) {
        if let Some(id) = p.dock_id.take() {
            let _ = self.dock.request(&format!("rm {id}"));
        }
    }

    fn start(&mut self, di: usize, pi: usize) {
        let d = &self.deploys[di];
        let t = &d.template;
        let p = &d.pods[pi];
        let mut cmd = format!("run -d --quiet --name {} --mem {} --tasks {} --label kube={}", p.name, t.mem, t.tasks, d.name);
        if let Some(s) = &t.space {
            cmd += &format!(" --space {s}");
        }
        for l in &t.labels {
            cmd += &format!(" --label {l}");
        }
        cmd += &format!(" {}", t.image);
        for a in &t.args {
            cmd += &format!(" {a}");
        }
        // The pod's last run is still in dock (for its logs): its name is taken.
        if let Some(id) = self.deploys[di].pods[pi].dock_id.take() {
            let _ = self.dock.request(&format!("rm {id}"));
        }
        let r = self.dock.request(&cmd);
        let now = clock_ns();
        let d = &mut self.deploys[di];
        let p = &mut d.pods[pi];
        let words: Vec<&str> = r.as_deref().unwrap_or("").split_whitespace().collect();
        match words.as_slice() {
            ["ok", id, _] => {
                let first = p.phase == Phase::Pending && p.restarts == 0 && p.started == 0;
                if !first {
                    p.restarts += 1;
                }
                p.dock_id = id.parse().ok();
                p.phase = Phase::Running;
                p.started = now;
                p.reason.clear();
                let msg = if first { format!("Started pod {}", p.name) } else { format!("Restarted pod {} ({})", p.name, p.restarts) };
                d.event(msg);
            }
            _ => {
                let why = match &r {
                    Ok(m) => String::from(m.strip_prefix("error ").unwrap_or(m).trim()),
                    Err(e) => format!("dock: {e:?}"),
                };
                p.phase = Phase::Pending;
                p.until = now + p.backoff;
                p.backoff = (p.backoff * 2).min(BACKOFF_MAX);
                let msg = format!("FailedCreate pod {}: {why}", p.name);
                p.reason = why;
                d.event(msg);
            }
        }
    }

    /// Dock says the pod ended: `code`, or None when it was stopped or removed.
    fn ended(&mut self, dock_id: u64, code: Option<i64>) {
        let now = clock_ns();
        for d in &mut self.deploys {
            let Some(p) = d.pods.iter_mut().find(|p| p.dock_id == Some(dock_id) && p.phase == Phase::Running) else { continue };
            let how = match code {
                Some(c) => format!("exited with {c}"),
                None => String::from("was stopped"),
            };
            let again = match d.template.restart {
                Restart::Always => true,
                Restart::OnFailure => code != Some(0),
                Restart::Never => false,
            };
            p.reason = how.clone();
            if !again {
                p.phase = Phase::Done(code.unwrap_or(-1));
                let msg = format!("Pod {} {how}", p.name);
                d.event(msg);
                return;
            }
            if now - p.started >= RAN_LONG {
                p.backoff = BACKOFF_MIN;
            }
            p.phase = Phase::BackOff;
            p.until = now + p.backoff;
            let msg = format!("Pod {} {how}; restarting in {}s", p.name, secs(p.backoff));
            p.backoff = (p.backoff * 2).min(BACKOFF_MAX);
            d.event(msg);
            return;
        }
    }

    /// Makes what runs match what is wanted.
    fn reconcile(&mut self) {
        let now = clock_ns();
        for di in 0..self.deploys.len() {
            // Too many: the newest go.
            while self.deploys[di].pods.len() > self.deploys[di].replicas as usize {
                let mut p = self.deploys[di].pods.pop().unwrap();
                self.forget(&mut p);
                self.deploys[di].event(format!("Removed pod {} (scaled down)", p.name));
            }
            // Too few: new pods.
            while self.deploys[di].pods.len() < self.deploys[di].replicas as usize {
                let d = &mut self.deploys[di];
                d.next += 1;
                let name = format!("{}-{}", d.name, d.next);
                d.pods.push(Pod {
                    name,
                    dock_id: None,
                    phase: Phase::Pending,
                    restarts: 0,
                    created: now,
                    started: 0,
                    until: now,
                    backoff: BACKOFF_MIN,
                    reason: String::new(),
                });
            }
            for pi in 0..self.deploys[di].pods.len() {
                let p = &self.deploys[di].pods[pi];
                if matches!(p.phase, Phase::Pending | Phase::BackOff) && p.until <= now {
                    self.start(di, pi);
                }
            }
        }
    }

    /// When the loop has to look again (a pod to start), if before `limit`.
    fn next_wake(&self, limit: u64) -> u64 {
        let pods = self.deploys.iter().flat_map(|d| &d.pods);
        pods.filter(|p| matches!(p.phase, Phase::Pending | Phase::BackOff)).map(|p| p.until).fold(limit, u64::min)
    }

    /// Dock's view of every pod, in case an event was missed.
    fn resync(&mut self) {
        let Ok(list) = self.dock.request("pods") else { return };
        let mut seen: Vec<(u64, Option<Option<i64>>)> = Vec::new();
        for l in list.lines() {
            let w: Vec<&str> = l.split_whitespace().collect();
            let [id, _, st, _] = w.as_slice() else { continue };
            let Ok(id) = id.parse() else { continue };
            let state = match *st {
                "running" => None,
                "stopped" => Some(None),
                s => Some(s.strip_prefix("exited:").and_then(|c| c.parse().ok())),
            };
            seen.push((id, state));
        }
        let running: Vec<u64> =
            self.deploys.iter().flat_map(|d| &d.pods).filter(|p| p.phase == Phase::Running).filter_map(|p| p.dock_id).collect();
        for id in running {
            match seen.iter().find(|(i, _)| *i == id) {
                Some((_, None)) => {}
                Some((_, Some(code))) => self.ended(id, *code),
                None => self.ended(id, None),
            }
        }
    }

    fn event_message(&mut self, msg: &str) {
        let w: Vec<&str> = msg.split_whitespace().collect();
        match w.as_slice() {
            ["event", "exit", id, _, code] => {
                if let Ok(id) = id.parse() {
                    self.ended(id, code.parse().ok());
                }
            }
            ["event", "stop" | "rm", id, _] => {
                if let Ok(id) = id.parse() {
                    self.ended(id, None);
                }
            }
            _ => {}
        }
    }

    fn apply(&mut self, text: &str) -> Result<String, String> {
        match manifest::parse(text)? {
            Object::Space { name, mem, tasks } => match self.spaces.iter().find(|(n, _, _)| *n == name) {
                Some((_, m, t)) if *m == mem && *t == tasks => Ok(format!("space/{name} unchanged")),
                Some(_) => Err(format!("space/{name} exists; a space's limits cannot change")),
                None => {
                    let r = self.dock.request(&format!("space {name} {mem} {tasks}")).map_err(|e| format!("dock: {e:?}"))?;
                    if let Some(m) = r.strip_prefix("error ") {
                        return Err(String::from(m));
                    }
                    self.spaces.push((name.clone(), mem, tasks));
                    Ok(format!("space/{name} created"))
                }
            },
            Object::Deployment { name, replicas, template } => {
                if let Some(s) = &template.space
                    && !self.spaces.iter().any(|(n, _, _)| n == s)
                {
                    return Err(format!("no space {s} (apply a Space first)"));
                }
                let Some(di) = self.find(&name) else {
                    self.deploys.push(Deployment {
                        name: name.clone(),
                        replicas,
                        template,
                        pods: Vec::new(),
                        next: 0,
                        created: clock_ns(),
                        events: Vec::new(),
                    });
                    return Ok(format!("deployment/{name} created"));
                };
                let d = &mut self.deploys[di];
                if d.template == template && d.replicas == replicas {
                    return Ok(format!("deployment/{name} unchanged"));
                }
                d.replicas = replicas;
                if d.template == template {
                    return Ok(format!("deployment/{name} configured"));
                }
                // A new template: all the pods are replaced (k8s's Recreate).
                d.template = template;
                let mut old = core::mem::take(&mut d.pods);
                for p in &mut old {
                    self.forget(p);
                }
                self.deploys[di].event(String::from("New template: pods replaced"));
                Ok(format!("deployment/{name} configured (pods replaced)"))
            }
        }
    }

    fn get_pods(&self) -> String {
        let mut out = format!("{:<14}{:<7}{:<18}{:<10}{}\n", "NAME", "READY", "STATUS", "RESTARTS", "AGE");
        for p in self.deploys.iter().flat_map(|d| &d.pods) {
            let (ready, status) = match p.phase {
                Phase::Pending => ("0/1", "Pending"),
                Phase::Running => ("1/1", "Running"),
                Phase::BackOff => ("0/1", "CrashLoopBackOff"),
                Phase::Done(0) => ("0/1", "Completed"),
                Phase::Done(_) => ("0/1", "Error"),
            };
            out += &format!("{:<14}{:<7}{:<18}{:<10}{}\n", p.name, ready, status, p.restarts, age(p.created));
        }
        out
    }

    fn get_deploys(&self) -> String {
        let mut out = format!("{:<10}{:<10}{:<7}{:<16}{}\n", "NAME", "SPACE", "READY", "IMAGE", "AGE");
        for d in &self.deploys {
            let up = d.pods.iter().filter(|p| p.phase == Phase::Running).count();
            let space = d.template.space.as_deref().unwrap_or("default");
            out += &format!("{:<10}{:<10}{:<7}{:<16}{}\n", d.name, space, format!("{up}/{}", d.replicas), d.template.image, age(d.created));
        }
        out
    }

    fn describe(&self, name: &str) -> Result<String, String> {
        let d = &self.deploys[self.find(name).ok_or_else(|| format!("no deployment {name}"))?];
        let t = &d.template;
        let up = d.pods.iter().filter(|p| p.phase == Phase::Running).count();
        let none = |v: &[String], sep: &str| if v.is_empty() { String::from("-") } else { v.join(sep) };
        let restart = match t.restart {
            Restart::Always => "always",
            Restart::OnFailure => "on-failure",
            Restart::Never => "never",
        };
        let mut out = format!("Name:       {}\n", d.name);
        out += &format!("Space:      {}\n", t.space.as_deref().unwrap_or("default"));
        out += &format!("Image:      {}\n", t.image);
        out += &format!("Args:       {}\n", none(&t.args, " "));
        out += &format!("Replicas:   {} wanted, {} running\n", d.replicas, up);
        out += &format!("Restart:    {restart}\n");
        out += &format!("Resources:  mem {}, tasks {} (per pod)\n", t.mem, t.tasks);
        out += &format!("Labels:     {}\n", none(&t.labels, ", "));
        out += "Pods:\n";
        let now = clock_ns();
        for p in &d.pods {
            let state = match p.phase {
                Phase::Pending if p.reason.is_empty() => String::from("Pending"),
                Phase::Pending => format!("Pending: {}", p.reason),
                Phase::Running => format!("Running for {}", age(p.started)),
                Phase::BackOff => format!("{}; restart in {}s", p.reason, secs(p.until.saturating_sub(now))),
                Phase::Done(_) => format!("Done: {}", p.reason),
            };
            out += &format!("  {:<14}restarts {:<4}{}\n", p.name, p.restarts, state);
        }
        out += "Events:\n";
        for (t, m) in &d.events {
            out += &format!("  {:<6}{m}\n", age(*t));
        }
        Ok(out)
    }

    fn command(&mut self, msg: &str) -> Result<String, String> {
        let (line, body) = msg.split_once('\n').unwrap_or((msg, ""));
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["apply"] => self.apply(body),
            ["get"] => Ok(format!("{}\n{}", self.get_deploys(), self.get_pods())),
            ["get", "pods" | "pod" | "po"] => Ok(self.get_pods()),
            ["get", "deployments" | "deployment" | "deploy"] => Ok(self.get_deploys()),
            ["get", "spaces" | "space"] => self.dock.request("spaces").map_err(|e| format!("dock: {e:?}")),
            ["get", what] => Err(format!("unknown resource {what} (pods, deploy, spaces)")),
            ["describe", name] | ["describe", "deploy" | "deployment", name] => self.describe(name),
            ["scale", name, n] => {
                let n: u32 = n.parse().map_err(|_| format!("bad count {n}"))?;
                let di = self.find(name).ok_or_else(|| format!("no deployment {name}"))?;
                self.deploys[di].replicas = n;
                Ok(format!("deployment/{name} scaled to {n}"))
            }
            ["delete", name] | ["delete", "deploy" | "deployment", name] => {
                let di = self.find(name).ok_or_else(|| format!("no deployment {name}"))?;
                let mut d = self.deploys.remove(di);
                for p in &mut d.pods {
                    self.forget(p);
                }
                Ok(format!("deployment/{name} deleted"))
            }
            ["logs", pod] => {
                let p = self.deploys.iter().flat_map(|d| &d.pods).find(|p| p.name == *pod).ok_or_else(|| format!("no pod {pod}"))?;
                let id = p.dock_id.ok_or_else(|| format!("{pod} has not run"))?;
                self.dock.request(&format!("logs {id}")).map_err(|e| format!("dock: {e:?}"))
            }
            _ => Err(String::from("commands: apply, get [pods|deploy|spaces], describe NAME, scale NAME N, delete NAME, logs POD")),
        }
    }

    fn client_message(&mut self, i: usize) {
        let Some(c) = self.clients[i] else { return };
        let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES];
        let mut hs = [Handle(0); 1];
        loop {
            match c.read(&mut buf, &mut hs) {
                // A message with a channel: another client.
                Ok((_, 1)) => self.add_client(Channel(hs[0])),
                Ok((n, _)) => {
                    let msg = String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                    let r = match self.command(&msg) {
                        Ok(r) => r,
                        Err(m) => format!("error {m}"),
                    };
                    // Act on it at once, so the next `get` already shows it.
                    self.reconcile();
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
}

fn main(_arg: usize) -> i64 {
    let Some(dock) = ns::get("dock") else { return 1 };
    let dock = Dock(Channel(dock));
    let Ok(port) = Port::create() else { return 2 };
    // A second dock client for its events.
    let Ok((events, theirs)) = Channel::create() else { return 2 };
    if dock.0.write(b"", &[theirs.0]).is_err() || events.write(b"watch", &[]).is_err() {
        return 2;
    }
    let mut buf = alloc::vec![0u8; 4096];
    let _ = events.recv(&mut buf, &mut []);
    let mut k = Kube { port, dock, clients: Vec::new(), deploys: Vec::new(), spaces: Vec::new() };
    if let Some(c) = ns::get("client") {
        k.add_client(Channel(c));
    }
    let _ = k.port.bind(events.0, signal::READABLE, EVENTS);
    let mut resync_at = clock_ns() + RESYNC;
    loop {
        k.reconcile();
        let now = clock_ns();
        let wake = k.next_wake(resync_at);
        match k.port.wait(wake.saturating_sub(now).max(1)) {
            Ok(p) if p.key == EVENTS => {
                while let Ok((n, _)) = events.read(&mut buf, &mut []) {
                    let msg = String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                    k.event_message(&msg);
                }
                let _ = k.port.bind(events.0, signal::READABLE, EVENTS);
            }
            Ok(p) => k.client_message(p.key as usize),
            Err(Error::TimedOut) => {}
            Err(_) => return 3,
        }
        if clock_ns() >= resync_at {
            k.resync();
            resync_at = clock_ns() + RESYNC;
        }
    }
}
