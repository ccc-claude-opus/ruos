//! Functions on this node (doc/serverless.md): a request for Function F
//! (a channel sent through `fn:F`) goes to an idle instance of F here; if
//! there is none and fewer than `max` run, a new one is started (a cold
//! start) and the request waits for it to say ready. An instance takes one
//! request at a time and says `done` after answering; then it waits for
//! the next (or, with `reuse = false`, goes). One that takes longer than
//! `timeout` is stopped (the caller sees its channel close); one idle for
//! `idle` seconds is stopped too, down to none. Every node does this on
//! its own, for the requests made on it, like Cloudflare Workers running
//! at the place a request arrives; the leader takes no part.
//!
//! KV namespaces: `kv:K`'s requests become `data/K/HEX(key)` records on
//! the server (api.rs).
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::wire::{FAILED, NOT_FOUND, OK, decode};
use libsys::{Channel, Handle, clock_ns, kv, signal, svc};

use crate::api::Api;
use crate::kubelet::{Kubelet, LISTEN, secs, slot};
use crate::manifest::{self, FnSpec, Object};

const SEC: u64 = 1_000_000_000;
/// An instance that has not said ready by then is stopped.
const START_LIMIT: u64 = 15 * SEC;
/// A request waits at most this long for an instance.
const WAIT_LIMIT: u64 = 20 * SEC;

#[derive(Clone, Copy, PartialEq)]
pub enum State {
    Starting,
    Idle,
    /// Answering a request; stopped at `deadline`.
    Busy { deadline: u64 },
}

pub struct Inst {
    pub func: String,
    pub name: String,
    /// The Function record's version it was made from.
    version: u64,
    pub dock_id: Option<u64>,
    listen: usize,
    links: Vec<usize>,
    pub state: State,
    started: u64,
    /// When it entered its state.
    since: u64,
}

/// What Function `name` did on this node.
#[derive(Default)]
pub struct FnStat {
    pub name: String,
    pub requests: u64,
    pub cold: u64,
    pub cold_ns: u64,
    pub answered: u64,
    pub busy_ns: u64,
    pub timeouts: u64,
    /// Requests waiting for an instance: (channel end, since).
    waiting: Vec<(Handle, u64)>,
}

/// Function `name`'s record: (version, spec).
pub fn spec(api: &Api, name: &str) -> Option<(u64, FnSpec)> {
    let r = api.get(&format!("fn/{name}"))?;
    let text = r.value.split_once("---\n").map(|(_, m)| m)?;
    match manifest::parse(text) {
        Ok(Object::Function { spec, .. }) => Some((r.num("version"), spec)),
        _ => None,
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|c| format!("{c:02x}")).collect()
}

fn unhex(s: &str) -> Option<String> {
    let b: Option<Vec<u8>> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect();
    String::from_utf8(b?).ok()
}

/// A request on `kv:K` (libsys::kv), answered from the server's records.
pub fn kv_request(api: &mut Api, k: &str, msg: &[u8]) -> Vec<u8> {
    let reply = |st: u8, b: &[u8]| {
        let mut v = alloc::vec![st];
        v.extend_from_slice(b);
        v
    };
    let Some((op, f)) = decode(msg, match msg.first() {
        Some(&kv::PUT) => 2,
        _ => 1,
    }) else {
        return reply(FAILED, b"bad request");
    };
    let key = f[0];
    if op != kv::LIST && (key.is_empty() || key.len() > kv::MAX_KEY) {
        return reply(FAILED, b"bad key");
    }
    let rec = format!("data/{k}/{}", hex(key));
    match op {
        kv::GET => match api.data(&rec).into_iter().find(|(x, _)| *x == rec) {
            Some((_, v)) => reply(OK, v.as_bytes()),
            None => reply(NOT_FOUND, b""),
        },
        kv::PUT if f[1].len() <= kv::MAX_VALUE => {
            let v = String::from_utf8_lossy(f[1]);
            if api.set_data(&rec, Some(&v)) { reply(OK, b"") } else { reply(FAILED, b"not written") }
        }
        kv::DELETE => {
            api.set_data(&rec, None);
            reply(OK, b"")
        }
        kv::LIST => {
            let base = format!("data/{k}/");
            let mut out = String::new();
            for (x, _) in api.data(&base) {
                if let Some(key) = x.strip_prefix(&base).and_then(unhex)
                    && key.as_bytes().starts_with(f[0])
                {
                    out += &key;
                    out.push('\n');
                }
            }
            reply(OK, out.as_bytes())
        }
        _ => reply(FAILED, b"bad request"),
    }
}

impl Kubelet {
    fn stat(&mut self, name: &str) -> &mut FnStat {
        if let Some(i) = self.fns.iter().position(|s| s.name == name) {
            return &mut self.fns[i];
        }
        self.fns.push(FnStat { name: String::from(name), ..FnStat::default() });
        self.fns.last_mut().unwrap()
    }

    pub fn fn_stat(&self, name: &str) -> Option<&FnStat> {
        self.fns.iter().find(|s| s.name == name)
    }

    pub fn fn_waiting(&self, name: &str) -> usize {
        self.fn_stat(name).map_or(0, |s| s.waiting.len())
    }

    /// A request for Function `name`: `conn` is the channel end for the instance.
    pub fn fn_request(&mut self, api: &mut Api, name: &str, conn: Handle) {
        if spec(api, name).is_none() {
            let _ = conn.close();
            return;
        }
        let st = self.stat(name);
        st.requests += 1;
        st.waiting.push((conn, clock_ns()));
        self.dispatch(api, name);
    }

    /// Gives waiting requests to idle instances; starts instances for the rest.
    fn dispatch(&mut self, api: &mut Api, name: &str) {
        let Some((version, spec)) = spec(api, name) else { return };
        loop {
            let Some(i) = self.insts.iter().position(|x| x.func == name && x.state == State::Idle && x.version == version) else { break };
            let Some(&(conn, _)) = self.stat(name).waiting.first() else { break };
            let Some(l) = self.listens[self.insts[i].listen] else { break };
            if l.write(svc::CONN, &[conn]).is_err() {
                self.stop(api, i, None);
                continue;
            }
            self.stat(name).waiting.remove(0);
            let now = clock_ns();
            let x = &mut self.insts[i];
            x.state = State::Busy { deadline: now + spec.timeout * SEC };
            x.since = now;
        }
        let waiting = self.stat(name).waiting.len();
        let starting = self.insts.iter().filter(|x| x.func == name && x.state == State::Starting).count();
        let total = self.insts.iter().filter(|x| x.func == name).count();
        for _ in 0..waiting.saturating_sub(starting).min((spec.max as usize).saturating_sub(total)) {
            self.cold_start(api, name, version, &spec);
        }
    }

    fn cold_start(&mut self, api: &mut Api, func: &str, version: u64, spec: &FnSpec) {
        let Ok((mine, theirs)) = Channel::create() else { return };
        self.fn_next += 1;
        let name = format!("{func}-f{}", self.fn_next);
        let listen = slot(&mut self.listens, mine);
        let _ = self.port.bind(mine.0, signal::READABLE | signal::PEER_CLOSED, LISTEN + listen as u64);
        let (more, hs, links) = self.bindings(&spec.uses);
        let mut cmd =
            format!("run -d --quiet --name {name} --mem {} --tasks {} --label kube-fn={func} --handle listen{more} {}", spec.mem, spec.tasks, spec.image);
        for a in &spec.args {
            cmd += &format!(" {a}");
        }
        let mut handles = alloc::vec![theirs.0];
        handles.extend(hs);
        let r = self.dock.request_with(&cmd, &handles);
        let words: Vec<&str> = r.as_deref().unwrap_or("").split_whitespace().collect();
        let now = clock_ns();
        match words.as_slice() {
            ["ok", id, _] => self.insts.push(Inst {
                func: String::from(func),
                name,
                version,
                dock_id: id.parse().ok(),
                listen,
                links,
                state: State::Starting,
                started: now,
                since: now,
            }),
            _ => {
                self.drop_io(Some(listen), links);
                let why = r.map(|m| String::from(m.strip_prefix("error ").unwrap_or(&m).trim())).unwrap_or_default();
                api.event(func, &format!("FailedCreate {name}: {why}"));
            }
        }
    }

    /// Stops instance `i`; `why` goes into the Function's events.
    fn stop(&mut self, api: &mut Api, i: usize, why: Option<String>) {
        let x = self.insts.remove(i);
        if let Some(id) = x.dock_id {
            let _ = self.dock.request(&format!("rm {id}"));
        }
        self.drop_io(Some(x.listen), x.links);
        if let Some(m) = why {
            api.event(&x.func, &m);
        }
    }

    /// An instance said something on its `listen`.
    pub fn inst_said(&mut self, api: &mut Api, listen: usize, msg: &[u8]) {
        let Some(i) = self.insts.iter().position(|x| x.listen == listen) else { return };
        let now = clock_ns();
        let func = self.insts[i].func.clone();
        let reuse = spec(api, &func).is_some_and(|(v, s)| s.reuse && v == self.insts[i].version);
        match (self.insts[i].state, msg) {
            (State::Starting, m) if m == svc::READY => {
                let took = now - self.insts[i].started;
                let name = self.insts[i].name.clone();
                self.insts[i].state = State::Idle;
                self.insts[i].since = now;
                let st = self.stat(&func);
                st.cold += 1;
                st.cold_ns += took;
                api.event(&func, &format!("Cold start {name} ({} ms)", took / 1_000_000));
            }
            (State::Busy { .. }, m) if m == libsys::func::DONE => {
                let took = now - self.insts[i].since;
                let st = self.stat(&func);
                st.answered += 1;
                st.busy_ns += took;
                if reuse {
                    self.insts[i].state = State::Idle;
                    self.insts[i].since = now;
                } else {
                    self.stop(api, i, None);
                }
            }
            _ => {}
        }
        self.dispatch(api, &func);
    }

    /// Dock says a pod ended: true if it was a Function instance.
    pub fn inst_ended(&mut self, api: &mut Api, dock_id: u64) -> bool {
        let Some(i) = self.insts.iter().position(|x| x.dock_id == Some(dock_id)) else { return false };
        let (func, name) = (self.insts[i].func.clone(), self.insts[i].name.clone());
        self.insts[i].dock_id = None;
        self.stop(api, i, Some(format!("Instance {name} ended")));
        self.dispatch(api, &func);
        true
    }

    /// When an instance's request runs out of time.
    pub fn fn_wake(&self) -> u64 {
        self.insts.iter().filter_map(|x| if let State::Busy { deadline } = x.state { Some(deadline) } else { None }).min().unwrap_or(u64::MAX)
    }

    /// Timeouts, idle instances, Functions changed or gone, requests waiting too long.
    pub fn fn_tick(&mut self, api: &mut Api) {
        let now = clock_ns();
        let mut i = 0;
        while i < self.insts.len() {
            let x = &self.insts[i];
            let spec = spec(api, &x.func);
            let why = match (x.state, &spec) {
                (_, None) => Some(String::new()),
                (State::Busy { deadline }, Some((_, s))) if now >= deadline => {
                    let m = format!("Request to {} timed out ({}s)", x.name, s.timeout);
                    let func = x.func.clone();
                    self.stat(&func).timeouts += 1;
                    Some(m)
                }
                (State::Idle, Some((v, _))) if *v != x.version => Some(String::new()),
                (State::Idle, Some((_, s))) if now - x.since >= s.idle * SEC => Some(String::new()),
                (State::Starting, _) if now - x.started >= START_LIMIT => Some(format!("{} did not start in {}s", x.name, secs(START_LIMIT))),
                _ => None,
            };
            match why {
                Some(m) => {
                    let func = self.insts[i].func.clone();
                    let idle = self.insts[i].state == State::Idle;
                    self.stop(api, i, (!m.is_empty()).then_some(m));
                    if idle && !self.insts.iter().any(|x| x.func == func) && let Some((_, s)) = crate::funcs::spec(api, &func) {
                        api.event(&func, &format!("Scaled {func} to 0 (idle {}s)", s.idle));
                    }
                }
                None => i += 1,
            }
        }
        let names: Vec<String> = self.fns.iter().map(|s| s.name.clone()).collect();
        for name in names {
            let gone = spec(api, &name).is_none();
            let st = self.stat(&name);
            let (late, keep): (Vec<(Handle, u64)>, Vec<(Handle, u64)>) =
                core::mem::take(&mut st.waiting).into_iter().partition(|(_, t)| gone || now - t >= WAIT_LIMIT);
            st.waiting = keep;
            for (h, _) in late {
                let _ = h.close();
            }
            self.dispatch(api, &name);
        }
        // For the page's cluster view.
        let mut rec = String::new();
        for s in &self.fns {
            let n = self.insts.iter().filter(|x| x.func == s.name).count();
            let busy = self.insts.iter().filter(|x| x.func == s.name && matches!(x.state, State::Busy { .. })).count();
            rec += &format!("{} {n} {busy} {} {}\n", s.name, s.requests, s.cold);
        }
        let key = format!("fnstat/{}", api.node);
        if api.get(&key).map(|r| r.value.as_str()) != Some(rec.as_str()) {
            if rec.is_empty() {
                api.delete(&key);
            } else {
                api.put(&key, None, &rec);
            }
        }
    }
}
