//! The node's half of kube (Kubernetes' kubelet): runs, in this machine's
//! dock, the pods the controller put on this node (`pod/P` with `node` =
//! this one), restarts them by their policy, gives them their service
//! channels, pings the serving ones, and writes how each is doing
//! (`status/P`). Connections to a service go to a ready pod of it on this
//! node: a channel cannot cross machines.
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::dock::Dock;
use libsys::{Channel, Error, Handle, Port, clock_ns, signal, svc};

use crate::api::Api;
use crate::manifest::{self, Object, Restart, Template};

const SEC: u64 = 1_000_000_000;
/// Port keys of pods' `listen` and `svc:NAME` channels.
pub const LISTEN: u64 = 1 << 33;
pub const LINK: u64 = 1 << 34;
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

#[derive(Clone, Copy, PartialEq)]
pub enum Phase {
    /// Not started yet, or dock could not start it (`reason`).
    Pending,
    Running,
    /// Ended, to be started again at `until`.
    BackOff,
    /// Ended for good (restart policy).
    Done(i64),
}

pub struct Pod {
    pub name: String,
    pub deploy: String,
    pub rev: u32,
    /// The pod in dock (kept after it ends, for its logs).
    pub dock_id: Option<u64>,
    pub phase: Phase,
    pub restarts: u32,
    /// clock_ns, and server time (for others to read).
    started: u64,
    started_ms: u64,
    until: u64,
    backoff: u64,
    pub reason: String,
    restart: Restart,
    /// The service it is a pod of, its `listen` (an index in listens),
    /// whether it said ready, and when it last answered.
    serves: Option<String>,
    listen: Option<usize>,
    said_ready: bool,
    answered: u64,
    /// Its `svc:NAME` channels (indexes in links).
    links: Vec<usize>,
    /// The status last written.
    written: String,
}

impl Pod {
    fn ready(&self, now: u64) -> bool {
        self.phase == Phase::Running
            && match self.serves {
                Some(_) => self.said_ready && now.saturating_sub(self.answered) < UNREADY_AFTER,
                None => now.saturating_sub(self.started) >= READY_AFTER,
            }
    }

    fn status(&self, node: &str, now: u64) -> String {
        let (phase, code) = match self.phase {
            Phase::Pending => ("Pending", 0),
            Phase::Running => ("Running", 0),
            Phase::BackOff => ("BackOff", 0),
            Phase::Done(c) => ("Done", c),
        };
        let wait = match self.phase {
            Phase::BackOff | Phase::Pending if self.until > now => (self.until - now).div_ceil(SEC),
            _ => 0,
        };
        format!(
            "node {node}\nphase {phase}\ncode {code}\nready {}\nrestarts {}\nstarted {}\nwait {wait}\ndock {}\nreason {}\n",
            self.ready(now) as u8,
            self.restarts,
            self.started_ms,
            self.dock_id.unwrap_or(0),
            self.reason
        )
    }
}

/// A connection waiting for a ready pod of `service`.
struct Waiting {
    service: String,
    conn: Handle,
    until: u64,
}

pub struct Kubelet {
    pub port: Port,
    pub dock: Dock,
    pub pods: Vec<Pod>,
    /// kubelet's ends of pods' `listen` and `svc:NAME` channels.
    listens: Vec<Option<Channel>>,
    links: Vec<Option<(Channel, String)>>,
    waiting: Vec<Waiting>,
    /// Round robin over each service's pods.
    turn: usize,
    /// Connections handed over, per service.
    pub handed: Vec<(String, u64)>,
    /// Spaces made in this machine's dock.
    spaces: Vec<String>,
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

fn secs(ns: u64) -> u64 {
    ns.div_ceil(SEC)
}

/// The image and template of revision `rev` of Deployment `d`.
pub fn revision(api: &Api, d: &str, rev: u32) -> Option<(String, Template)> {
    let r = api.get(&format!("rev/{d}/{rev}"))?;
    let image = String::from(r.field("image")?);
    let text = r.value.split_once("---\n").map(|(_, m)| m)?;
    match manifest::parse(text) {
        Ok(Object::Deployment { template, .. }) => Some((image, template)),
        _ => None,
    }
}

impl Kubelet {
    pub fn new(port: Port, dock: Dock) -> Kubelet {
        Kubelet {
            port,
            dock,
            pods: Vec::new(),
            listens: Vec::new(),
            links: Vec::new(),
            waiting: Vec::new(),
            turn: 0,
            handed: Vec::new(),
            spaces: Vec::new(),
        }
    }

    pub fn waiting(&self, service: &str) -> usize {
        self.waiting.iter().filter(|w| w.service == service).count()
    }

    /// Closes the kubelet's ends of a pod's channels.
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

    fn forget(&mut self, mut p: Pod) {
        if let Some(id) = p.dock_id.take() {
            let _ = self.dock.request(&format!("rm {id}"));
        }
        self.drop_io(p.listen, p.links);
    }

    /// The Space `s` in this machine's dock (each node has its own).
    fn space(&mut self, api: &Api, s: &str) {
        if self.spaces.iter().any(|x| x == s) {
            return;
        }
        let Some(r) = api.get(&format!("space/{s}")) else { return };
        if let Ok(Object::Space { name, mem, tasks }) = manifest::parse(&r.value) {
            let _ = self.dock.request(&format!("space {name} {mem} {tasks}"));
            self.spaces.push(name);
        }
    }

    fn start(&mut self, api: &mut Api, pi: usize) {
        let p = &mut self.pods[pi];
        let (old, listen, links) = (p.dock_id.take(), p.listen.take(), core::mem::take(&mut p.links));
        // The pod's last run is still in dock (for its logs): its name is taken.
        if let Some(id) = old {
            let _ = self.dock.request(&format!("rm {id}"));
        }
        self.drop_io(listen, links);
        let (name, deploy, rev) = (self.pods[pi].name.clone(), self.pods[pi].deploy.clone(), self.pods[pi].rev);
        let Some((image, t)) = revision(api, &deploy, rev) else {
            self.pods[pi].reason = format!("no revision {rev} of {deploy}");
            self.pods[pi].until = clock_ns() + BACKOFF_MAX;
            return;
        };
        if let Some(s) = &t.space {
            self.space(api, s);
        }
        let mut cmd = format!("run -d --quiet --name {name} --mem {} --tasks {} --label kube={deploy}", t.mem, t.tasks);
        if let Some(s) = &t.space {
            cmd += &format!(" --space {s}");
        }
        for l in &t.labels {
            cmd += &format!(" --label {l}");
        }
        // The pod's channels: the kubelet keeps one end of each.
        let mut handles = Vec::new();
        let mut listen = None;
        let mut links = Vec::new();
        if t.serves.is_some()
            && let Ok((mine, theirs)) = Channel::create()
        {
            let i = slot(&mut self.listens, mine);
            let _ = self.port.bind(mine.0, signal::READABLE | signal::PEER_CLOSED, LISTEN + i as u64);
            listen = Some(i);
            handles.push(theirs.0);
            cmd += " --handle listen";
        }
        for s in &t.uses {
            if let Ok((mine, theirs)) = Channel::create() {
                let i = slot(&mut self.links, (mine, s.clone()));
                let _ = self.port.bind(mine.0, signal::READABLE | signal::PEER_CLOSED, LINK + i as u64);
                links.push(i);
                handles.push(theirs.0);
                cmd += &format!(" --handle svc:{s}");
            }
        }
        cmd += &format!(" {image}");
        for a in &t.args {
            cmd += &format!(" {a}");
        }
        let reply = self.dock.request_with(&cmd, &handles);
        let now = clock_ns();
        let words: Vec<&str> = reply.as_deref().unwrap_or("").split_whitespace().collect();
        let p = &mut self.pods[pi];
        p.restart = t.restart;
        if let ["ok", id, _] = words.as_slice() {
            let first = p.phase == Phase::Pending && p.restarts == 0 && p.started == 0;
            if !first {
                p.restarts += 1;
            }
            p.dock_id = id.parse().ok();
            p.phase = Phase::Running;
            p.started = now;
            p.started_ms = api.now();
            p.answered = now;
            p.said_ready = false;
            p.serves = t.serves;
            p.listen = listen;
            p.links = links;
            p.reason.clear();
            let msg = if first { format!("Started pod {name} on {}", api.node) } else { format!("Restarted pod {name} ({})", p.restarts) };
            api.event(&deploy, &msg);
            return;
        }
        let why = match &reply {
            Ok(m) => String::from(m.strip_prefix("error ").unwrap_or(m).trim()),
            Err(e) => format!("dock: {e:?}"),
        };
        p.phase = Phase::Pending;
        p.until = now + p.backoff;
        p.backoff = (p.backoff * 2).min(BACKOFF_MAX);
        p.reason = why.clone();
        self.drop_io(listen, links);
        api.event(&deploy, &format!("FailedCreate pod {name}: {why}"));
    }

    /// Dock says the pod ended: `code`, or None when it was stopped or removed.
    pub fn ended(&mut self, api: &mut Api, dock_id: u64, code: Option<i64>) {
        let now = clock_ns();
        let Some(p) = self.pods.iter_mut().find(|p| p.dock_id == Some(dock_id) && p.phase == Phase::Running) else { return };
        let how = match code {
            Some(c) => format!("exited with {c}"),
            None => String::from("was stopped"),
        };
        let again = match p.restart {
            Restart::Always => true,
            Restart::OnFailure => code != Some(0),
            Restart::Never => false,
        };
        p.reason = how.clone();
        let io = (p.listen.take(), core::mem::take(&mut p.links));
        let (name, deploy) = (p.name.clone(), p.deploy.clone());
        let msg = if again {
            if now - p.started >= RAN_LONG {
                p.backoff = BACKOFF_MIN;
            }
            p.phase = Phase::BackOff;
            p.until = now + p.backoff;
            let m = format!("Pod {name} {how}; restarting in {}s", secs(p.backoff));
            p.backoff = (p.backoff * 2).min(BACKOFF_MAX);
            m
        } else {
            p.phase = Phase::Done(code.unwrap_or(-1));
            format!("Pod {name} {how}")
        };
        self.drop_io(io.0, io.1);
        api.event(&deploy, &msg);
        self.write_status(api);
    }

    /// Makes this node's pods match the records, then says how they are.
    pub fn sync(&mut self, api: &mut Api) {
        let now = clock_ns();
        let mine: Vec<(String, String, u32)> = api
            .prefix("pod/")
            .filter(|(_, r)| r.field("node") == Some(api.node.as_str()))
            .map(|(k, r)| (String::from(&k[4..]), String::from(r.field("deploy").unwrap_or("")), r.num("rev") as u32))
            .collect();
        // Pods taken away (scaled down, an old revision, deleted).
        while let Some(i) = self.pods.iter().position(|p| !mine.iter().any(|(n, _, _)| *n == p.name)) {
            let p = self.pods.remove(i);
            api.delete(&format!("status/{}", p.name));
            self.forget(p);
        }
        for (name, deploy, rev) in mine {
            if !self.pods.iter().any(|p| p.name == name) {
                self.pods.push(Pod {
                    name,
                    deploy,
                    rev,
                    dock_id: None,
                    phase: Phase::Pending,
                    restarts: 0,
                    started: 0,
                    started_ms: 0,
                    until: now,
                    backoff: BACKOFF_MIN,
                    reason: String::new(),
                    restart: Restart::Always,
                    serves: None,
                    listen: None,
                    said_ready: false,
                    answered: 0,
                    links: Vec::new(),
                    written: String::new(),
                });
            }
        }
        for pi in 0..self.pods.len() {
            let p = &self.pods[pi];
            if matches!(p.phase, Phase::Pending | Phase::BackOff) && p.until <= now {
                self.start(api, pi);
            }
        }
        self.hand_waiting();
        self.write_status(api);
    }

    fn write_status(&mut self, api: &mut Api) {
        let now = clock_ns();
        let node = api.node.clone();
        for p in &mut self.pods {
            let s = p.status(&node, now);
            if s != p.written {
                api.put(&format!("status/{}", p.name), None, &s);
                p.written = s;
            }
        }
    }

    /// When a pod has to start or becomes ready, if before `limit`.
    pub fn next_wake(&self, limit: u64) -> u64 {
        let now = clock_ns();
        let mut t = limit;
        for p in &self.pods {
            match p.phase {
                Phase::Pending | Phase::BackOff => t = t.min(p.until),
                Phase::Running if p.serves.is_none() && p.started + READY_AFTER > now => t = t.min(p.started + READY_AFTER),
                _ => {}
            }
        }
        t
    }

    /// Every 2 s: dock's view of every pod (in case an event was missed),
    /// pings to the serving pods, a restart for those that stopped answering.
    pub fn tick(&mut self, api: &mut Api) {
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
            let running: Vec<u64> = self.pods.iter().filter(|p| p.phase == Phase::Running).filter_map(|p| p.dock_id).collect();
            for id in running {
                match seen.iter().find(|(i, _)| *i == id) {
                    Some((_, None)) => {}
                    Some((_, Some(code))) => self.ended(api, id, *code),
                    None => self.ended(api, id, None),
                }
            }
        }
        let mut dead = Vec::new();
        for p in &self.pods {
            let Some(l) = p.listen.and_then(|i| self.listens[i]) else { continue };
            if p.phase != Phase::Running {
                continue;
            }
            if now.saturating_sub(p.answered) >= DEAD_AFTER {
                dead.push((p.deploy.clone(), p.dock_id, format!("Liveness probe failed: {} did not answer for {}s", p.name, secs(DEAD_AFTER))));
            } else {
                let _ = l.write(svc::PING, &[]);
            }
        }
        for (d, id, msg) in dead {
            api.event(&d, &msg);
            if let Some(id) = id {
                let _ = self.dock.request(&format!("stop {id}"));
            }
        }
        // Connections no pod took in time: the caller sees the channel close.
        let (late, keep): (Vec<Waiting>, Vec<Waiting>) = core::mem::take(&mut self.waiting).into_iter().partition(|w| w.until <= now);
        self.waiting = keep;
        for w in late {
            let _ = w.conn.close();
        }
        self.write_status(api);
    }

    /// Hands connection `conn` to a ready pod of `service` here; false if there is none.
    fn hand(&mut self, service: &str, conn: Handle) -> bool {
        let now = clock_ns();
        let pods: Vec<usize> = self.pods.iter().filter(|p| p.serves.as_deref() == Some(service) && p.ready(now)).filter_map(|p| p.listen).collect();
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

    pub fn connect(&mut self, service: &str, conn: Handle) {
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
    pub fn listen_message(&mut self, api: &mut Api, i: usize) {
        let Some(c) = self.listens[i] else { return };
        let mut buf = [0u8; 16];
        loop {
            match c.read(&mut buf, &mut []) {
                Ok((n, _)) => {
                    let Some(p) = self.pods.iter_mut().find(|p| p.listen == Some(i)) else { break };
                    p.answered = clock_ns();
                    if &buf[..n] == svc::READY && !p.said_ready {
                        p.said_ready = true;
                        let (d, msg) = (p.deploy.clone(), format!("Pod {} is ready", p.name));
                        api.event(&d, &msg);
                    }
                }
                Err(Error::ShouldWait) => {
                    let _ = self.port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, LISTEN + i as u64);
                    break;
                }
                Err(_) => break,
            }
        }
        self.hand_waiting();
        self.write_status(api);
    }

    /// A pod sent a connection through its `svc:NAME`.
    pub fn link_message(&mut self, i: usize) {
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
}
