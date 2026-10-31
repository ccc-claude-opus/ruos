//! The cluster's records, as kube sees them: on the server (ruos serve
//! keeps them for every machine; requests go through the store), or, on a
//! machine without a server, in kube's own memory (a cluster of one).
//! kube reads everything at the top of each loop (`refresh`) and works on
//! that copy; its writes go to both.
//!
//! Records (values are `field value` lines):
//! - `nodes/N`      a machine: `arch`, `session` (its lease: the record's time)
//! - `leader`       `node N`: the node running the controller (a lease too)
//! - `deploy/D`     a Deployment: `replicas`, `rev` (current), `created`
//! - `rev/D/R`      a revision: `image IMAGE`, `---`, its manifest
//! - `ctl/D`        the controller's: `next` (pod number), `rolling`
//! - `pod/P`        the controller's: `deploy`, `rev`, `node` (or `-`), `created`
//! - `status/P`     the node's: `node`, `phase`, `ready`, `restarts`, `started`, `reason`, `dock`
//! - `config/C`, `space/S`   manifests as applied
//! - `events/D`     `TIME message` lines
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::clock_ns;
use libsys::store::{KV_DEL, KV_HELLO, KV_LIST, KV_PUT, Store};

const OK: u8 = 0;

#[derive(Clone)]
pub struct Rec {
    pub version: u64,
    /// Server time (ms) of the last write.
    pub updated: u64,
    pub value: String,
}

impl Rec {
    /// The value of a `field value` line.
    pub fn field(&self, name: &str) -> Option<&str> {
        field(&self.value, name)
    }

    pub fn num(&self, name: &str) -> u64 {
        self.field(name).and_then(|v| v.parse().ok()).unwrap_or(0)
    }
}

pub fn field<'a>(value: &'a str, name: &str) -> Option<&'a str> {
    value.lines().find_map(|l| {
        let (k, v) = l.split_once(' ').unwrap_or((l, ""));
        (k == name).then_some(v)
    })
}

pub enum Write {
    Done,
    /// Someone else wrote it first (compare and swap).
    Conflict,
    Failed,
}

pub struct Api {
    store: Store,
    /// None: the records are on the server.
    local: Option<u64>,
    pub node: String,
    pub session: String,
    pub recs: BTreeMap<String, Rec>,
    /// Server time at `at` (clock_ns), to tell the time between refreshes.
    server_ms: u64,
    at: u64,
}

impl Api {
    pub fn new(store: Store) -> Api {
        let mut api = Api { store, local: None, node: String::new(), session: String::new(), recs: BTreeMap::new(), server_ms: 0, at: 0 };
        match api.store.raw(KV_HELLO, &[]) {
            Ok((OK, b)) => {
                let s = String::from_utf8_lossy(&b).into_owned();
                let w: Vec<&str> = s.split(' ').collect();
                api.node = String::from(*w.first().unwrap_or(&"node"));
                api.session = String::from(*w.get(1).unwrap_or(&""));
                api.set_time(w.get(2).and_then(|t| t.parse().ok()).unwrap_or(0));
            }
            _ => {
                api.local = Some(0);
                api.node = String::from("local");
                api.set_time(0);
            }
        }
        api
    }

    pub fn is_cluster(&self) -> bool {
        self.local.is_none()
    }

    fn set_time(&mut self, ms: u64) {
        self.server_ms = ms;
        self.at = clock_ns();
    }

    /// The server's time now, in ms.
    pub fn now(&self) -> u64 {
        self.server_ms + (clock_ns() - self.at) / 1_000_000
    }

    /// Reads every record again (nothing changes on a machine without a server).
    pub fn refresh(&mut self) -> bool {
        if self.local.is_some() {
            return true;
        }
        let Ok((OK, b)) = self.store.raw(KV_LIST, &[b""]) else { return false };
        let mut recs = BTreeMap::new();
        let mut rest = &b[..];
        let line = |r: &mut &[u8]| {
            let i = r.iter().position(|&c| c == b'\n')?;
            let l = String::from_utf8_lossy(&r[..i]).into_owned();
            *r = &r[i + 1..];
            Some(l)
        };
        let Some(now) = line(&mut rest) else { return false };
        while let Some(head) = line(&mut rest) {
            let w: Vec<&str> = head.split(' ').collect();
            let [k, v, u, n] = w.as_slice() else { break };
            let n: usize = n.parse().unwrap_or(0);
            if rest.len() < n + 1 {
                break;
            }
            let value = String::from_utf8_lossy(&rest[..n]).into_owned();
            rest = &rest[n + 1..];
            recs.insert(String::from(*k), Rec { version: v.parse().unwrap_or(0), updated: u.parse().unwrap_or(0), value });
        }
        self.recs = recs;
        self.set_time(now.parse().unwrap_or(0));
        true
    }

    pub fn get(&self, key: &str) -> Option<&Rec> {
        self.recs.get(key)
    }

    pub fn prefix<'a>(&'a self, p: &'a str) -> impl Iterator<Item = (&'a str, &'a Rec)> + 'a {
        self.recs.range(String::from(p)..).take_while(move |(k, _)| k.starts_with(p)).map(|(k, r)| (k.as_str(), r))
    }

    /// Writes `key`; `expect`: None = whatever is there, Some(0) = only if
    /// there is none, Some(v) = only if it is still version v.
    pub fn put(&mut self, key: &str, expect: Option<u64>, value: &str) -> Write {
        let now = self.now();
        let version = match &mut self.local {
            Some(last) => {
                let cur = self.recs.get(key).map(|r| r.version);
                let ok = match expect {
                    None => true,
                    Some(0) => cur.is_none(),
                    Some(v) => cur == Some(v),
                };
                if !ok {
                    return Write::Conflict;
                }
                *last += 1;
                *last
            }
            None => {
                let e = expect.map(|v| format!("{v}")).unwrap_or_default();
                match self.store.raw(KV_PUT, &[key.as_bytes(), e.as_bytes(), value.as_bytes()]) {
                    Ok((OK, b)) => String::from_utf8_lossy(&b).parse().unwrap_or(0),
                    Ok((_, b)) if b == b"conflict" => return Write::Conflict,
                    _ => return Write::Failed,
                }
            }
        };
        self.recs.insert(String::from(key), Rec { version, updated: now, value: String::from(value) });
        Write::Done
    }

    pub fn delete(&mut self, key: &str) {
        if self.recs.remove(key).is_some() && self.local.is_none() {
            let _ = self.store.raw(KV_DEL, &[key.as_bytes(), b""]);
        }
    }

    /// Changes `key` with `f` until no one else wrote it in between.
    pub fn update(&mut self, key: &str, mut f: impl FnMut(Option<&str>) -> String) -> bool {
        for _ in 0..4 {
            let (expect, value) = match self.recs.get(key) {
                Some(r) => (Some(r.version), f(Some(&r.value))),
                None => (Some(0), f(None)),
            };
            match self.put(key, expect, &value) {
                Write::Done => return true,
                Write::Conflict => _ = self.refresh(),
                Write::Failed => return false,
            }
        }
        false
    }

    /// Adds an event to Deployment `d` (the last 14 are kept).
    pub fn event(&mut self, d: &str, msg: &str) {
        let now = self.now();
        self.update(&format!("events/{d}"), |old| {
            let mut lines: Vec<&str> = old.unwrap_or("").lines().collect();
            if lines.last().and_then(|l| l.split_once(' ')).is_some_and(|(_, m)| m == msg) {
                return String::from(old.unwrap_or(""));
            }
            let new = format!("{now} {msg}");
            lines.push(&new);
            let skip = lines.len().saturating_sub(14);
            let mut out = String::new();
            for l in &lines[skip..] {
                out += l;
                out.push('\n');
            }
            out
        });
    }
}
