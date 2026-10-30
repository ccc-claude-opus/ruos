//! kube: declarative pods, a small Kubernetes. Clients `apply` what they
//! want and kube keeps making it so: a control loop compares what is
//! wanted with what dock reports (its `watch` events, and a full `pods`
//! list every 2 s) and starts, restarts (with back-off) or removes pods.
//!
//! - Deployment: N pods of an image. A new template (or a new version of
//!   its Config) is a new revision, rolled out one pod at a time: a new pod
//!   first, an old one goes once the new one is ready.
//! - Service: pods of a Deployment with `serves = NAME` get `listen`, pods
//!   with `uses = [NAME]` get `svc:NAME` (libsys::svc). A connection sent
//!   through `svc:NAME` is handed to a ready pod of NAME, in turn; then the
//!   two pods talk directly. No addresses: what a pod may call is what it
//!   was given. Serving pods are pinged every 2 s; one that stops
//!   answering is restarted (liveness) and gets no connections (readiness).
//! - Config: files and environment; a Deployment using it runs its image
//!   with a layer of the files on top (a derived image in the store).
//! - Space: a quota its Deployments' pods share.
//!
//! Its capabilities: a dock client and a store client.
#![no_std]
#![no_main]

extern crate alloc;

mod manifest;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::dock::Dock;
use libsys::store::Store;
use libsys::{Channel, Error, Handle, Port, clock_ns, ns, signal, svc};

use manifest::{Object, Restart, Template};

libsys::entry!(main);

const SEC: u64 = 1_000_000_000;
/// Port keys: clients are 0.., dock's events EVENTS, pods' `listen`
/// LISTEN + i, pods' `svc:NAME` LINK + i.
const EVENTS: u64 = 1 << 32;
const LISTEN: u64 = 1 << 33;
const LINK: u64 = 1 << 34;
/// A full look at dock's pods, and pings to serving pods, this often.
const TICK: u64 = 2 * SEC;
/// A serving pod that has not answered for this long gets no connections…
const UNREADY_AFTER: u64 = 5 * SEC;
/// …and after this long it is restarted.
const DEAD_AFTER: u64 = 7 * SEC;
/// A pod that does not serve is ready once it has run this long.
const READY_AFTER: u64 = SEC;
/// A connection waits this long for a ready pod.
const CONNECT_WAIT: u64 = 5 * SEC;
/// Back-off before a restart: 1 s, doubling up to 16 s; back to 1 s after a pod ran 10 s.
const BACKOFF_MIN: u64 = SEC;
const BACKOFF_MAX: u64 = 16 * SEC;
const RAN_LONG: u64 = 10 * SEC;
/// Events kept per Deployment.
const MAX_EVENTS: usize = 14;

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
    /// The Deployment revision it runs.
    rev: u32,
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
    /// The service it is a pod of, its `listen` (an index in Kube.listens),
    /// whether it said ready, and when it last answered.
    serves: Option<String>,
    listen: Option<usize>,
    said_ready: bool,
    answered: u64,
    /// Its `svc:NAME` channels (indexes in Kube.links).
    links: Vec<usize>,
}

impl Pod {
    fn ready(&self, now: u64) -> bool {
        self.phase == Phase::Running
            && match self.serves {
                Some(_) => self.said_ready && now.saturating_sub(self.answered) < UNREADY_AFTER,
                None => now.saturating_sub(self.started) >= READY_AFTER,
            }
    }
}

struct Revision {
    rev: u32,
    template: Template,
    /// What the pods run: the template's image, or one derived with the Config.
    image: String,
}

struct Deployment {
    name: String,
    replicas: u32,
    /// The current revision is the last.
    revisions: Vec<Revision>,
    /// The Config version the current revision was made with.
    config_version: u32,
    pods: Vec<Pod>,
    /// The next pod's number (names are never reused).
    next: u32,
    created: u64,
    rolling: bool,
    events: Vec<(u64, String)>,
}

impl Deployment {
    fn cur(&self) -> &Revision {
        self.revisions.last().unwrap()
    }

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

struct Config {
    name: String,
    files: Vec<(String, String)>,
    env: Vec<(String, String)>,
    version: u32,
}

/// A connection waiting for a ready pod of `service`.
struct Waiting {
    service: String,
    conn: Handle,
    until: u64,
}

struct Kube {
    port: Port,
    dock: Dock,
    store: Store,
    clients: Vec<Option<Channel>>,
    deploys: Vec<Deployment>,
    /// (name, mem, tasks), made in dock.
    spaces: Vec<(String, String, u64)>,
    configs: Vec<Config>,
    /// kube's ends of pods' `listen` and `svc:NAME` channels.
    listens: Vec<Option<Channel>>,
    links: Vec<Option<(Channel, String)>>,
    waiting: Vec<Waiting>,
    /// Round robin over each service's pods.
    turn: usize,
    /// Connections handed over, per service.
    handed: Vec<(String, u64)>,
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

fn slot<T>(v: &mut Vec<Option<T>>, x: T) -> usize {
    match v.iter().position(Option::is_none) {
        Some(i) => {
            v[i] = Some(x);
            i
        }
        None => {
            v.push(Some(x));
            v.len() - 1
        }
    }
}

impl Kube {
    fn add_client(&mut self, c: Channel) {
        self.clients.push(Some(c));
        let _ = self.port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, self.clients.len() as u64 - 1);
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.deploys.iter().position(|d| d.name == name)
    }

    /// Closes kube's ends of a pod's channels (its `listen`, its `svc:`s).
    fn drop_io(&mut self, listen: Option<usize>, links: Vec<usize>) {
        if let Some(c) = listen.and_then(|i| self.listens[i].take()) {
            let _ = c.close();
        }
        for i in links {
            if let Some((c, _)) = self.links[i].take() {
                let _ = c.close();
            }
        }
    }

    /// Removes the pod from dock (stops it if it runs).
    fn forget(&mut self, mut p: Pod) {
        if let Some(id) = p.dock_id.take() {
            let _ = self.dock.request(&format!("rm {id}"));
        }
        self.drop_io(p.listen, p.links);
    }

    /// What revision `rev` of a Deployment runs: its image, with the Config on top.
    fn image_for(&self, deploy: &str, rev: u32, t: &Template) -> Result<String, String> {
        let Some(cname) = &t.config else { return Ok(t.image.clone()) };
        let c = self.configs.iter().find(|c| c.name == *cname).ok_or_else(|| format!("no config {cname}"))?;
        let e = |what: &str, err: Error| format!("{what}: {err:?}");
        let mut files = Vec::new();
        for (path, text) in &c.files {
            let id = libsys::image::put(&self.store, text.as_bytes()).map_err(|err| e("store", err))?;
            files.push((path.clone(), Some((id, text.len() as u64))));
        }
        let layer = libsys::image::layer(&self.store, &files).map_err(|err| e("layer", err))?;
        let name = format!("kube-{deploy}:r{rev}");
        libsys::image::derive(&self.store, &t.image, &name, layer, None, &c.env).map_err(|err| e(&t.image, err))?;
        Ok(name)
    }

    /// A new revision of Deployment `di` with `template`; its pods are rolled out.
    fn new_revision(&mut self, di: usize, template: Template) -> Result<u32, String> {
        let d = &self.deploys[di];
        let rev = d.revisions.last().map_or(1, |r| r.rev + 1);
        let image = self.image_for(&d.name.clone(), rev, &template)?;
        let version = template.config.as_ref().and_then(|c| self.configs.iter().find(|x| x.name == *c)).map_or(0, |c| c.version);
        let d = &mut self.deploys[di];
        d.revisions.push(Revision { rev, template, image });
        if d.revisions.len() > 10 {
            d.revisions.remove(0);
        }
        d.config_version = version;
        if !d.pods.is_empty() {
            d.rolling = true;
            d.event(format!("Rolling out revision {rev}"));
        }
        Ok(rev)
    }

    fn start(&mut self, di: usize, pi: usize) {
        // The pod's last run is still in dock (for its logs): its name is taken.
        let p = &mut self.deploys[di].pods[pi];
        let (old, listen, links) = (p.dock_id.take(), p.listen.take(), core::mem::take(&mut p.links));
        if let Some(id) = old {
            let _ = self.dock.request(&format!("rm {id}"));
        }
        self.drop_io(listen, links);
        let d = &self.deploys[di];
        let p = &d.pods[pi];
        let Some(r) = d.revisions.iter().find(|r| r.rev == p.rev) else { return };
        let t = &r.template;
        let mut cmd = format!("run -d --quiet --name {} --mem {} --tasks {} --label kube={}", p.name, t.mem, t.tasks, d.name);
        if let Some(s) = &t.space {
            cmd += &format!(" --space {s}");
        }
        for l in &t.labels {
            cmd += &format!(" --label {l}");
        }
        // The pod's channels: kube keeps one end of each.
        let mut handles = Vec::new();
        let mut listen = None;
        let mut links = Vec::new();
        let serves = t.serves.clone();
        let uses = t.uses.clone();
        if serves.is_some()
            && let Ok((mine, theirs)) = Channel::create()
        {
            let i = slot(&mut self.listens, mine);
            let _ = self.port.bind(mine.0, signal::READABLE | signal::PEER_CLOSED, LISTEN + i as u64);
            listen = Some(i);
            handles.push(theirs.0);
            cmd += " --handle listen";
        }
        for s in &uses {
            if let Ok((mine, theirs)) = Channel::create() {
                let i = slot(&mut self.links, (mine, s.clone()));
                let _ = self.port.bind(mine.0, signal::READABLE | signal::PEER_CLOSED, LINK + i as u64);
                links.push(i);
                handles.push(theirs.0);
                cmd += &format!(" --handle svc:{s}");
            }
        }
        let d = &self.deploys[di];
        let r = d.revisions.iter().find(|r| r.rev == d.pods[pi].rev).unwrap();
        cmd += &format!(" {}", r.image);
        for a in &r.template.args {
            cmd += &format!(" {a}");
        }
        let reply = self.dock.request_with(&cmd, &handles);
        let now = clock_ns();
        let words: Vec<&str> = reply.as_deref().unwrap_or("").split_whitespace().collect();
        if let ["ok", id, _] = words.as_slice() {
            let d = &mut self.deploys[di];
            let p = &mut d.pods[pi];
            let first = p.phase == Phase::Pending && p.restarts == 0 && p.started == 0;
            if !first {
                p.restarts += 1;
            }
            p.dock_id = id.parse().ok();
            p.phase = Phase::Running;
            p.started = now;
            p.answered = now;
            p.said_ready = false;
            p.serves = serves;
            p.listen = listen;
            p.links = links;
            p.reason.clear();
            let msg = if first { format!("Started pod {}", p.name) } else { format!("Restarted pod {} ({})", p.name, p.restarts) };
            d.event(msg);
            return;
        }
        self.drop_io(listen, links);
        let why = match &reply {
            Ok(m) => String::from(m.strip_prefix("error ").unwrap_or(m).trim()),
            Err(e) => format!("dock: {e:?}"),
        };
        let d = &mut self.deploys[di];
        let p = &mut d.pods[pi];
        p.phase = Phase::Pending;
        p.until = now + p.backoff;
        p.backoff = (p.backoff * 2).min(BACKOFF_MAX);
        let msg = format!("FailedCreate pod {}: {why}", p.name);
        p.reason = why;
        d.event(msg);
    }

    /// Dock says the pod ended: `code`, or None when it was stopped or removed.
    fn ended(&mut self, dock_id: u64, code: Option<i64>) {
        let now = clock_ns();
        let mut io = None;
        for d in &mut self.deploys {
            let Some(p) = d.pods.iter_mut().find(|p| p.dock_id == Some(dock_id) && p.phase == Phase::Running) else { continue };
            let how = match code {
                Some(c) => format!("exited with {c}"),
                None => String::from("was stopped"),
            };
            let rev = d.revisions.iter().find(|r| r.rev == p.rev).map_or(Restart::Always, |r| r.template.restart);
            let again = match rev {
                Restart::Always => true,
                Restart::OnFailure => code != Some(0),
                Restart::Never => false,
            };
            p.reason = how.clone();
            io = Some((p.listen.take(), core::mem::take(&mut p.links)));
            if !again {
                p.phase = Phase::Done(code.unwrap_or(-1));
                let msg = format!("Pod {} {how}", p.name);
                d.event(msg);
                break;
            }
            if now - p.started >= RAN_LONG {
                p.backoff = BACKOFF_MIN;
            }
            p.phase = Phase::BackOff;
            p.until = now + p.backoff;
            let msg = format!("Pod {} {how}; restarting in {}s", p.name, secs(p.backoff));
            p.backoff = (p.backoff * 2).min(BACKOFF_MAX);
            d.event(msg);
            break;
        }
        if let Some((l, links)) = io {
            self.drop_io(l, links);
        }
    }

    fn new_pod(&mut self, di: usize) {
        let now = clock_ns();
        let d = &mut self.deploys[di];
        d.next += 1;
        let rev = d.cur().rev;
        d.pods.push(Pod {
            name: format!("{}-{}", d.name, d.next),
            rev,
            dock_id: None,
            phase: Phase::Pending,
            restarts: 0,
            created: now,
            started: 0,
            until: now,
            backoff: BACKOFF_MIN,
            reason: String::new(),
            serves: None,
            listen: None,
            said_ready: false,
            answered: 0,
            links: Vec::new(),
        });
    }

    fn remove_pod(&mut self, di: usize, pi: usize, why: &str) {
        let p = self.deploys[di].pods.remove(pi);
        self.deploys[di].event(format!("Removed pod {} ({why})", p.name));
        self.forget(p);
    }

    /// Makes what runs match what is wanted. A rollout keeps every wanted
    /// pod available: one new pod more than wanted, then an old one goes
    /// once enough are ready.
    fn reconcile(&mut self) {
        let now = clock_ns();
        for di in 0..self.deploys.len() {
            let want = self.deploys[di].replicas as usize;
            let rev = self.deploys[di].cur().rev;
            let old = |d: &Deployment| d.pods.iter().filter(|p| p.rev != rev).count();
            if old(&self.deploys[di]) > 0 {
                // Old pods that are not ready help nobody.
                while let Some(pi) = self.deploys[di].pods.iter().position(|p| p.rev != rev && !p.ready(now)) {
                    self.remove_pod(di, pi, "old revision");
                }
                while old(&self.deploys[di]) > 0 && self.deploys[di].pods.len() > want + 1 {
                    let pi = self.deploys[di].pods.iter().position(|p| p.rev != rev).unwrap();
                    self.remove_pod(di, pi, "old revision");
                }
                let d = &self.deploys[di];
                let new = d.pods.len() - old(d);
                if new < want && d.pods.len() < want + 1 {
                    self.new_pod(di);
                }
                let d = &self.deploys[di];
                let ready = d.pods.iter().filter(|p| p.ready(now)).count();
                if ready > want
                    && let Some(pi) = d.pods.iter().position(|p| p.rev != rev)
                {
                    self.remove_pod(di, pi, "old revision");
                }
            } else {
                while self.deploys[di].pods.len() > want {
                    let pi = self.deploys[di].pods.len() - 1;
                    self.remove_pod(di, pi, "scaled down");
                }
                while self.deploys[di].pods.len() < want {
                    self.new_pod(di);
                }
            }
            for pi in 0..self.deploys[di].pods.len() {
                let p = &self.deploys[di].pods[pi];
                if matches!(p.phase, Phase::Pending | Phase::BackOff) && p.until <= now {
                    self.start(di, pi);
                }
            }
            let d = &mut self.deploys[di];
            if d.rolling && d.pods.iter().all(|p| p.rev == rev) && d.pods.iter().filter(|p| p.ready(now)).count() >= want {
                d.rolling = false;
                d.event(format!("Rollout of revision {rev} complete"));
            }
        }
        self.hand_waiting();
    }

    /// When the loop has to look again (a pod to start or becoming ready), if before `limit`.
    fn next_wake(&self, limit: u64) -> u64 {
        let pods = self.deploys.iter().flat_map(|d| &d.pods);
        let mut t = limit;
        for p in pods {
            match p.phase {
                Phase::Pending | Phase::BackOff => t = t.min(p.until),
                Phase::Running if p.serves.is_none() => {
                    let r = p.started + READY_AFTER;
                    if r > clock_ns() {
                        t = t.min(r);
                    }
                }
                _ => {}
            }
        }
        t
    }

    /// Dock's view of every pod, in case an event was missed; pings the
    /// serving pods and restarts the ones that stopped answering.
    fn tick(&mut self) {
        let now = clock_ns();
        if let Ok(list) = self.dock.request("pods") {
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
        let mut dead = Vec::new();
        for d in &mut self.deploys {
            let mut msgs = Vec::new();
            for p in &d.pods {
                let Some(l) = p.listen.and_then(|i| self.listens[i]) else { continue };
                if p.phase != Phase::Running {
                    continue;
                }
                if now.saturating_sub(p.answered) >= DEAD_AFTER {
                    msgs.push(format!("Liveness probe failed: {} did not answer for {}s", p.name, secs(DEAD_AFTER)));
                    dead.extend(p.dock_id);
                } else {
                    let _ = l.write(svc::PING, &[]);
                }
            }
            for m in msgs {
                d.event(m);
            }
        }
        for id in dead {
            let _ = self.dock.request(&format!("stop {id}"));
        }
        // Connections no pod took in time: the caller sees the channel close.
        let (late, keep): (Vec<Waiting>, Vec<Waiting>) = core::mem::take(&mut self.waiting).into_iter().partition(|w| w.until <= now);
        self.waiting = keep;
        for w in late {
            let _ = w.conn.close();
        }
    }

    /// Hands connection `conn` to a ready pod of `service`; false if there is none.
    fn hand(&mut self, service: &str, conn: Handle) -> bool {
        let now = clock_ns();
        let pods: Vec<usize> = self
            .deploys
            .iter()
            .flat_map(|d| &d.pods)
            .filter(|p| p.serves.as_deref() == Some(service) && p.ready(now))
            .filter_map(|p| p.listen)
            .collect();
        if pods.is_empty() {
            return false;
        }
        self.turn = self.turn.wrapping_add(1);
        let i = pods[self.turn % pods.len()];
        let Some(l) = self.listens[i] else { return false };
        if l.write(svc::CONN, &[conn]).is_err() {
            return false;
        }
        match self.handed.iter_mut().find(|(s, _)| s == service) {
            Some((_, n)) => *n += 1,
            None => self.handed.push((String::from(service), 1)),
        }
        true
    }

    fn connect(&mut self, service: &str, conn: Handle) {
        if !self.hand(service, conn) {
            self.waiting.push(Waiting { service: String::from(service), conn, until: clock_ns() + CONNECT_WAIT });
        }
    }

    fn hand_waiting(&mut self) {
        for w in core::mem::take(&mut self.waiting) {
            if !self.hand(&w.service, w.conn) {
                self.waiting.push(w);
            }
        }
    }

    /// A pod said something on its `listen`: ready, or pong.
    fn listen_message(&mut self, i: usize) {
        let Some(c) = self.listens[i] else { return };
        let mut buf = [0u8; 16];
        loop {
            match c.read(&mut buf, &mut []) {
                Ok((n, _)) => {
                    let now = clock_ns();
                    let Some(d) = self.deploys.iter_mut().find(|d| d.pods.iter().any(|p| p.listen == Some(i))) else { break };
                    let p = d.pods.iter_mut().find(|p| p.listen == Some(i)).unwrap();
                    p.answered = now;
                    if &buf[..n] == svc::READY && !p.said_ready {
                        p.said_ready = true;
                        let msg = format!("Pod {} is ready", p.name);
                        d.event(msg);
                    }
                }
                Err(Error::ShouldWait) => {
                    let _ = self.port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, LISTEN + i as u64);
                    return;
                }
                Err(_) => break,
            }
        }
    }

    /// A pod sent a connection through its `svc:NAME`.
    fn link_message(&mut self, i: usize) {
        let Some((c, service)) = self.links[i].clone() else { return };
        let mut buf = [0u8; 16];
        let mut hs = [Handle(0); 1];
        loop {
            match c.read(&mut buf, &mut hs) {
                Ok((n, 1)) if &buf[..n] == svc::CONNECT => self.connect(&service, hs[0]),
                Ok((_, nh)) => {
                    for h in &hs[..nh] {
                        let _ = h.close();
                    }
                }
                Err(Error::ShouldWait) => {
                    let _ = self.port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, LINK + i as u64);
                    return;
                }
                Err(_) => return,
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
            Object::Config { name, files, env } => {
                let verb = match self.configs.iter_mut().find(|c| c.name == name) {
                    Some(c) if c.files == files && c.env == env => return Ok(format!("config/{name} unchanged")),
                    Some(c) => {
                        c.files = files;
                        c.env = env;
                        c.version += 1;
                        "configured"
                    }
                    None => {
                        self.configs.push(Config { name: name.clone(), files, env, version: 1 });
                        "created"
                    }
                };
                // Deployments using it get a new revision.
                let mut out = format!("config/{name} {verb}");
                for di in 0..self.deploys.len() {
                    if self.deploys[di].cur().template.config.as_deref() == Some(name.as_str()) {
                        let t = self.deploys[di].cur().template.clone();
                        let rev = self.new_revision(di, t)?;
                        out += &format!("\ndeployment/{} rolling out revision {rev}", self.deploys[di].name);
                    }
                }
                Ok(out)
            }
            Object::Deployment { name, replicas, template } => {
                if let Some(s) = &template.space
                    && !self.spaces.iter().any(|(n, _, _)| n == s)
                {
                    return Err(format!("no space {s} (apply a Space first)"));
                }
                if let Some(c) = &template.config
                    && !self.configs.iter().any(|x| x.name == *c)
                {
                    return Err(format!("no config {c} (apply a Config first)"));
                }
                let Some(di) = self.find(&name) else {
                    self.deploys.push(Deployment {
                        name: name.clone(),
                        replicas,
                        revisions: Vec::new(),
                        config_version: 0,
                        pods: Vec::new(),
                        next: 0,
                        created: clock_ns(),
                        rolling: false,
                        events: Vec::new(),
                    });
                    let di = self.deploys.len() - 1;
                    if let Err(m) = self.new_revision(di, template) {
                        self.deploys.pop();
                        return Err(m);
                    }
                    return Ok(format!("deployment/{name} created"));
                };
                let d = &mut self.deploys[di];
                let same = d.cur().template == template;
                if same && d.replicas == replicas {
                    return Ok(format!("deployment/{name} unchanged"));
                }
                d.replicas = replicas;
                if same {
                    return Ok(format!("deployment/{name} configured"));
                }
                let rev = self.new_revision(di, template)?;
                Ok(format!("deployment/{name} configured (rolling out revision {rev})"))
            }
        }
    }

    fn get_pods(&self) -> String {
        let now = clock_ns();
        let mut out = format!("{:<14}{:<7}{:<18}{:<10}{:<5}{}\n", "NAME", "READY", "STATUS", "RESTARTS", "REV", "AGE");
        for p in self.deploys.iter().flat_map(|d| &d.pods) {
            let status = match p.phase {
                Phase::Pending => "Pending",
                Phase::Running => "Running",
                Phase::BackOff => "CrashLoopBackOff",
                Phase::Done(0) => "Completed",
                Phase::Done(_) => "Error",
            };
            let ready = if p.ready(now) { "1/1" } else { "0/1" };
            out += &format!("{:<14}{:<7}{:<18}{:<10}{:<5}{}\n", p.name, ready, status, p.restarts, p.rev, age(p.created));
        }
        out
    }

    fn get_deploys(&self) -> String {
        let now = clock_ns();
        let mut out = format!("{:<10}{:<10}{:<7}{:<5}{:<16}{}\n", "NAME", "SPACE", "READY", "REV", "IMAGE", "AGE");
        for d in &self.deploys {
            let up = d.pods.iter().filter(|p| p.ready(now)).count();
            let t = &d.cur().template;
            let space = t.space.as_deref().unwrap_or("default");
            let ready = format!("{up}/{}", d.replicas);
            out += &format!("{:<10}{:<10}{:<7}{:<5}{:<16}{}\n", d.name, space, ready, d.cur().rev, t.image, age(d.created));
        }
        out
    }

    fn get_services(&self) -> String {
        let now = clock_ns();
        let mut names: Vec<&str> = self.deploys.iter().flat_map(|d| d.revisions.iter().filter_map(|r| r.template.serves.as_deref())).collect();
        names.extend(self.links.iter().flatten().map(|(_, s)| s.as_str()));
        names.sort();
        names.dedup();
        let mut out = format!("{:<10}{:<10}{:<8}{:<8}{}\n", "SERVICE", "READY", "USERS", "WAITING", "CONNECTIONS");
        for s in names {
            let pods: Vec<&Pod> = self.deploys.iter().flat_map(|d| &d.pods).filter(|p| p.serves.as_deref() == Some(s)).collect();
            let ready = pods.iter().filter(|p| p.ready(now)).count();
            let users = self.links.iter().flatten().filter(|(_, x)| x == s).count();
            let waiting = self.waiting.iter().filter(|w| w.service == s).count();
            let handed = self.handed.iter().find(|(x, _)| x == s).map_or(0, |(_, n)| *n);
            out += &format!("{:<10}{:<10}{:<8}{:<8}{}\n", s, format!("{ready}/{}", pods.len()), users, waiting, handed);
        }
        out
    }

    fn get_configs(&self) -> String {
        let mut out = format!("{:<12}{:<8}{:<6}{}\n", "CONFIG", "VERSION", "FILES", "ENV");
        for c in &self.configs {
            let env: Vec<&str> = c.env.iter().map(|(k, _)| k.as_str()).collect();
            let env = if env.is_empty() { String::from("-") } else { env.join(",") };
            out += &format!("{:<12}{:<8}{:<6}{}\n", c.name, c.version, c.files.len(), env);
        }
        out
    }

    fn describe(&self, name: &str) -> Result<String, String> {
        let d = &self.deploys[self.find(name).ok_or_else(|| format!("no deployment {name}"))?];
        let r = d.cur();
        let t = &r.template;
        let now = clock_ns();
        let up = d.pods.iter().filter(|p| p.ready(now)).count();
        let none = |v: &[String], sep: &str| if v.is_empty() { String::from("-") } else { v.join(sep) };
        let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| String::from("-"));
        let restart = match t.restart {
            Restart::Always => "always",
            Restart::OnFailure => "on-failure",
            Restart::Never => "never",
        };
        let mut out = format!("Name:       {}\n", d.name);
        out += &format!("Space:      {}\n", t.space.as_deref().unwrap_or("default"));
        out += &format!("Revision:   {}{}\n", r.rev, if d.rolling { " (rolling out)" } else { "" });
        out += &format!("Image:      {}{}\n", t.image, if r.image != t.image { format!(" (runs {})", r.image) } else { String::new() });
        out += &format!("Args:       {}\n", none(&t.args, " "));
        out += &format!("Replicas:   {} wanted, {} ready\n", d.replicas, up);
        out += &format!("Restart:    {restart}\n");
        out += &format!("Resources:  mem {}, tasks {} (per pod)\n", t.mem, t.tasks);
        out += &format!("Labels:     {}\n", none(&t.labels, ", "));
        out += &format!("Serves:     {}\n", opt(&t.serves));
        out += &format!("Uses:       {}\n", none(&t.uses, ", "));
        out += &format!("Config:     {}\n", opt(&t.config));
        out += "Pods:\n";
        for p in &d.pods {
            let state = match p.phase {
                Phase::Pending if p.reason.is_empty() => String::from("Pending"),
                Phase::Pending => format!("Pending: {}", p.reason),
                Phase::Running if p.ready(now) => format!("Running for {}, ready", age(p.started)),
                Phase::Running => format!("Running for {}, not ready", age(p.started)),
                Phase::BackOff => format!("{}; restart in {}s", p.reason, secs(p.until.saturating_sub(now))),
                Phase::Done(_) => format!("Done: {}", p.reason),
            };
            out += &format!("  {:<14}rev {:<3}restarts {:<4}{}\n", p.name, p.rev, p.restarts, state);
        }
        out += "Events:\n";
        for (t, m) in &d.events {
            out += &format!("  {:<6}{m}\n", age(*t));
        }
        Ok(out)
    }

    fn rollout(&mut self, what: &str, name: &str) -> Result<String, String> {
        let di = self.find(name).ok_or_else(|| format!("no deployment {name}"))?;
        let now = clock_ns();
        match what {
            "status" => {
                let d = &self.deploys[di];
                let rev = d.cur().rev;
                let new = d.pods.iter().filter(|p| p.rev == rev).count();
                let ready = d.pods.iter().filter(|p| p.rev == rev && p.ready(now)).count();
                let old = d.pods.len() - new;
                Ok(if !d.rolling && old == 0 {
                    format!("deployment/{name} revision {rev}: complete ({ready}/{} ready)", d.replicas)
                } else {
                    format!("deployment/{name} revision {rev}: {ready}/{} new pods ready, {old} old", d.replicas)
                })
            }
            "history" => {
                let mut out = format!("{:<5}{}\n", "REV", "IMAGE");
                for r in &self.deploys[di].revisions {
                    out += &format!("{:<5}{}\n", r.rev, r.image);
                }
                Ok(out)
            }
            "undo" => {
                let d = &self.deploys[di];
                let n = d.revisions.len();
                if n < 2 {
                    return Err(format!("deployment/{name} has no earlier revision"));
                }
                let (back, t) = (d.revisions[n - 2].rev, d.revisions[n - 2].template.clone());
                let rev = self.new_revision(di, t)?;
                Ok(format!("deployment/{name} rolling back: revision {rev} = revision {back}"))
            }
            _ => Err(String::from("rollout status|history|undo NAME")),
        }
    }

    fn command(&mut self, msg: &str, conn: Option<Handle>) -> Result<String, String> {
        let (line, body) = msg.split_once('\n').unwrap_or((msg, ""));
        let words: Vec<&str> = line.split_whitespace().collect();
        if let Some(h) = conn {
            // `connect SERVICE` with a channel end: the caller (the shell) calls a service.
            return match words.as_slice() {
                ["connect", s] => {
                    self.connect(s, h);
                    Ok(String::from("ok"))
                }
                _ => {
                    let _ = h.close();
                    Err(String::from("a handle only goes with connect SERVICE"))
                }
            };
        }
        match words.as_slice() {
            ["apply"] => self.apply(body),
            ["get"] => Ok(format!("{}\n{}", self.get_deploys(), self.get_pods())),
            ["get", "pods" | "pod" | "po"] => Ok(self.get_pods()),
            ["get", "deployments" | "deployment" | "deploy"] => Ok(self.get_deploys()),
            ["get", "services" | "service" | "svc"] => Ok(self.get_services()),
            ["get", "configs" | "config"] => Ok(self.get_configs()),
            ["get", "spaces" | "space"] => self.dock.request("spaces").map_err(|e| format!("dock: {e:?}")),
            ["get", what] => Err(format!("unknown resource {what} (pods, deploy, svc, configs, spaces)")),
            ["describe", name] | ["describe", "deploy" | "deployment", name] => self.describe(name),
            ["rollout", what, name] => self.rollout(what, name),
            ["scale", name, n] => {
                let n: u32 = n.parse().map_err(|_| format!("bad count {n}"))?;
                let di = self.find(name).ok_or_else(|| format!("no deployment {name}"))?;
                self.deploys[di].replicas = n;
                Ok(format!("deployment/{name} scaled to {n}"))
            }
            ["delete", "config", name] => {
                if self.deploys.iter().any(|d| d.cur().template.config.as_deref() == Some(*name)) {
                    return Err(format!("config {name} is in use"));
                }
                let i = self.configs.iter().position(|c| c.name == *name).ok_or_else(|| format!("no config {name}"))?;
                self.configs.remove(i);
                Ok(format!("config/{name} deleted"))
            }
            ["delete", name] | ["delete", "deploy" | "deployment", name] => {
                let di = self.find(name).ok_or_else(|| format!("no deployment {name}"))?;
                let d = self.deploys.remove(di);
                for p in d.pods {
                    self.forget(p);
                }
                Ok(format!("deployment/{name} deleted"))
            }
            ["logs", pod] => {
                let p = self.deploys.iter().flat_map(|d| &d.pods).find(|p| p.name == *pod).ok_or_else(|| format!("no pod {pod}"))?;
                let id = p.dock_id.ok_or_else(|| format!("{pod} has not run"))?;
                self.dock.request(&format!("logs {id}")).map_err(|e| format!("dock: {e:?}"))
            }
            _ => Err(String::from(
                "commands: apply, get [pods|deploy|svc|configs|spaces], describe NAME, rollout status|history|undo NAME, scale NAME N, delete [config] NAME, logs POD",
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
    let (Some(dock), Some(store)) = (ns::get("dock"), ns::get("store")) else { return 1 };
    let dock = Dock(Channel(dock));
    let Ok(port) = Port::create() else { return 2 };
    // A second dock client for its events.
    let Ok((events, theirs)) = Channel::create() else { return 2 };
    if dock.0.write(b"", &[theirs.0]).is_err() || events.write(b"watch", &[]).is_err() {
        return 2;
    }
    let mut buf = alloc::vec![0u8; 4096];
    let _ = events.recv(&mut buf, &mut []);
    let mut k = Kube {
        port,
        dock,
        store: Store(Channel(store)),
        clients: Vec::new(),
        deploys: Vec::new(),
        spaces: Vec::new(),
        configs: Vec::new(),
        listens: Vec::new(),
        links: Vec::new(),
        waiting: Vec::new(),
        turn: 0,
        handed: Vec::new(),
    };
    if let Some(c) = ns::get("client") {
        k.add_client(Channel(c));
    }
    let _ = k.port.bind(events.0, signal::READABLE, EVENTS);
    let mut tick_at = clock_ns() + TICK;
    loop {
        k.reconcile();
        let now = clock_ns();
        let wake = k.next_wake(tick_at);
        match k.port.wait(wake.saturating_sub(now).max(1)) {
            Ok(p) if p.key == EVENTS => {
                while let Ok((n, _)) = events.read(&mut buf, &mut []) {
                    let msg = String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                    k.event_message(&msg);
                }
                let _ = k.port.bind(events.0, signal::READABLE, EVENTS);
            }
            Ok(p) if p.key >= LINK => k.link_message((p.key - LINK) as usize),
            Ok(p) if p.key >= LISTEN => k.listen_message((p.key - LISTEN) as usize),
            Ok(p) => k.client_message(p.key as usize),
            Err(Error::TimedOut) => {}
            Err(_) => return 3,
        }
        if clock_ns() >= tick_at {
            k.tick();
            tick_at = clock_ns() + TICK;
        }
    }
}
