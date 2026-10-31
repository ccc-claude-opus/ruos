//! Requests from outside for kube's Functions (doc/ingress.md). A request
//! to the function port, `/fn/NAME/…`, goes to a machine as one message on
//! its remote channel 1, `REQ ID NAME\n` + the request (an HTTP envelope),
//! and comes back as `RESP ID\n` + the answer.
//!
//! The server cannot send a machine anything on its own (the remote
//! channel's WebSocket is the machine asking and the server answering), so
//! each machine's kernel host keeps one long poll waiting,
//! `GET /v1/ingress/next?machine=M&session=S` (answered when there are
//! requests for it, or empty after 20 s), and posts each answer to
//! `POST /v1/ingress/answer`. A machine is there while it polls.
//!
//! Which machine: among those there, one that already has an instance of
//! the Function (the cluster's `fnstat/NODE` records) first, else the next
//! in turn.
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::cluster;

/// A poll waits this long for requests.
const POLL_WAIT: Duration = Duration::from_secs(20);
/// A machine that has not polled for this long is not there.
const GONE_AFTER: Duration = Duration::from_secs(3);
/// Time for a cold start, on top of the Function's timeout.
const START_GRACE: u64 = 15;
/// Requests and answers are one message on a channel.
pub const MAX_BODY: usize = 60000;

struct Machine {
    name: String,
    queue: VecDeque<Vec<u8>>,
    last: Instant,
    /// Polls waiting now.
    polling: u32,
}

#[derive(Default)]
struct State {
    /// By session.
    machines: HashMap<String, Machine>,
    pending: HashMap<u64, mpsc::Sender<Vec<u8>>>,
    next: u64,
    turn: usize,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static WAKE: Condvar = Condvar::new();

fn state() -> MutexGuard<'static, Option<State>> {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(State::default());
    }
    g
}

/// A machine's poll: its requests (waits up to 20 s for some).
pub fn next(session: &str, machine: &str) -> Vec<Vec<u8>> {
    let mut g = state();
    let s = g.as_mut().unwrap();
    s.machines.retain(|_, m| m.polling > 0 || m.last.elapsed() < Duration::from_secs(60));
    let m = s.machines.entry(session.to_string()).or_insert_with(|| Machine {
        name: machine.to_string(),
        queue: VecDeque::new(),
        last: Instant::now(),
        polling: 0,
    });
    m.polling += 1;
    let until = Instant::now() + POLL_WAIT;
    loop {
        let s = g.as_mut().unwrap();
        let m = s.machines.get_mut(session).unwrap();
        if !m.queue.is_empty() || Instant::now() >= until {
            m.polling -= 1;
            m.last = Instant::now();
            return m.queue.drain(..).collect();
        }
        let left = until.saturating_duration_since(Instant::now());
        g = WAKE.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// Requests a poll could not deliver (its connection was gone): for the next poll.
pub fn put_back(session: &str, msgs: Vec<Vec<u8>>) {
    let mut g = state();
    if let Some(m) = g.as_mut().unwrap().machines.get_mut(session) {
        for b in msgs.into_iter().rev() {
            m.queue.push_front(b);
        }
        // The page may be gone: let the others take new requests.
        m.last = Instant::now() - GONE_AFTER;
    }
}

/// An answer from a machine: `RESP ID\n` + the answer.
pub fn answer(body: &[u8]) -> bool {
    let Some(nl) = body.iter().position(|&c| c == b'\n') else { return false };
    let head = String::from_utf8_lossy(&body[..nl]).into_owned();
    let Some(id) = head.strip_prefix("RESP ").and_then(|i| i.trim().parse::<u64>().ok()) else { return false };
    let tx = state().as_mut().unwrap().pending.remove(&id);
    tx.is_some_and(|tx| tx.send(body[nl + 1..].to_vec()).is_ok())
}

/// A Function's record: Some(timeout in s) if it is exposed.
pub fn exposed(store: &Path, name: &str) -> Option<u64> {
    let text = cluster::with(store, |kv| kv.list(&format!("fn/{name}")).find(|(k, _)| **k == format!("fn/{name}")).map(|(_, r)| r.value.clone()))?;
    let text = String::from_utf8_lossy(&text).into_owned();
    let val = |k: &str| {
        text.lines().find_map(|l| {
            let (a, b) = l.split_once('=')?;
            (a.trim() == k).then(|| b.trim().trim_matches('"').to_string())
        })
    };
    (val("expose").as_deref() == Some("true")).then(|| val("timeout").and_then(|t| t.parse().ok()).unwrap_or(10))
}

/// Exposed Functions' names.
pub fn list(store: &Path) -> Vec<String> {
    let names: Vec<String> = cluster::with(store, |kv| kv.list("fn/").map(|(k, _)| k[3..].to_string()).collect());
    names.into_iter().filter(|n| exposed(store, n).is_some()).collect()
}

pub enum Failed {
    /// No machine is there.
    NoMachine,
    /// No answer in time.
    TimedOut,
}

/// Sends a request for Function `name` to a machine and waits for the answer:
/// (the machine, the answer).
pub fn call(store: &Path, name: &str, timeout: u64, request: &[u8]) -> Result<(String, Vec<u8>), Failed> {
    // Machines with an instance of it already (from the cluster's records).
    let warm: Vec<String> = cluster::with(store, |kv| {
        kv.list("fnstat/")
            .filter(|(_, r)| {
                String::from_utf8_lossy(&r.value).lines().any(|l| {
                    let w: Vec<&str> = l.split(' ').collect();
                    w.first() == Some(&name) && w.get(1).and_then(|n| n.parse::<u32>().ok()).unwrap_or(0) > 0
                })
            })
            .map(|(k, _)| k["fnstat/".len()..].to_string())
            .collect()
    });
    let (tx, rx) = mpsc::channel();
    let (id, machine) = {
        let mut g = state();
        let s = g.as_mut().unwrap();
        let mut there: Vec<(&String, &mut Machine)> =
            s.machines.iter_mut().filter(|(_, m)| m.polling > 0 || m.last.elapsed() < GONE_AFTER).collect();
        if there.is_empty() {
            return Err(Failed::NoMachine);
        }
        there.sort_by(|a, b| a.0.cmp(b.0));
        s.turn = s.turn.wrapping_add(1);
        let pick = there.iter().position(|(_, m)| warm.contains(&m.name)).unwrap_or(s.turn % there.len());
        s.next += 1;
        let id = s.next;
        let mut msg = format!("REQ {id} {name}\n").into_bytes();
        msg.extend_from_slice(request);
        let m = &mut there[pick].1;
        m.queue.push_back(msg);
        let machine = m.name.clone();
        s.pending.insert(id, tx);
        (id, machine)
    };
    WAKE.notify_all();
    match rx.recv_timeout(Duration::from_secs(timeout + START_GRACE)) {
        Ok(a) => Ok((machine, a)),
        Err(_) => {
            state().as_mut().unwrap().pending.remove(&id);
            Err(Failed::TimedOut)
        }
    }
}
