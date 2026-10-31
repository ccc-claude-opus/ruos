//! Requests from outside for kube's Functions (doc/ingress.md). A request
//! to the function port, `/fn/NAME/…`, goes to a machine as one message on
//! its ingress WebSocket (`/v1/ingress?machine=M&session=S`, the kernel's
//! remote channel 1), `REQ ID NAME\n` + the request (an HTTP envelope),
//! and comes back on it as `RESP ID\n` + the answer. A machine is there
//! while its ingress WebSocket is open.
//!
//! Which machine: one that already has an instance of the Function (the
//! cluster's `fnstat/NODE` records) first, else the next in turn.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::cluster;

/// Time for a cold start, on top of the Function's timeout.
const START_GRACE: u64 = 15;
/// Requests and answers are one message on a channel.
pub const MAX_BODY: usize = 60000;

struct Machine {
    name: String,
    /// Which connection (a machine that connects again replaces the old one).
    conn: u64,
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

#[derive(Default)]
struct State {
    /// By session.
    machines: HashMap<String, Machine>,
    /// Waiting for answers: (session, where the answer goes).
    pending: HashMap<u64, (String, oneshot::Sender<Vec<u8>>)>,
    next: u64,
    turn: usize,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

/// A machine's ingress WebSocket opened: (its connection id, the requests for it).
pub fn attach(session: &str, machine: &str) -> (u64, mpsc::UnboundedReceiver<Vec<u8>>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let conn = with(|s| {
        s.next += 1;
        s.machines.insert(session.to_string(), Machine { name: machine.to_string(), conn: s.next, tx });
        s.next
    });
    (conn, rx)
}

/// It closed: its requests that were not answered fail (502).
pub fn detach(session: &str, conn: u64) {
    with(|s| {
        if s.machines.get(session).is_some_and(|m| m.conn == conn) {
            s.machines.remove(session);
            s.pending.retain(|_, (sess, _)| sess != session);
        }
    });
}

/// An answer from a machine: `RESP ID\n` + the answer.
pub fn answer(body: &[u8]) -> bool {
    let Some(nl) = body.iter().position(|&c| c == b'\n') else { return false };
    let head = String::from_utf8_lossy(&body[..nl]).into_owned();
    let Some(id) = head.strip_prefix("RESP ").and_then(|i| i.trim().parse::<u64>().ok()) else { return false };
    let tx = with(|s| s.pending.remove(&id));
    tx.is_some_and(|(_, tx)| tx.send(body[nl + 1..].to_vec()).is_ok())
}

/// A Function's record: Some(timeout in s) if it is exposed.
pub fn exposed(store: &Path, name: &str) -> Option<u64> {
    let key = format!("fn/{name}");
    let text = cluster::with(store, |kv| kv.list(&key).find(|(k, _)| **k == key).map(|(_, r)| r.value.clone()))?;
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
    /// The machine went away before it answered.
    Lost,
}

/// Sends a request for Function `name` to a machine and waits for the answer:
/// (the machine, the answer).
pub async fn call(store: &Path, name: &str, timeout: u64, request: &[u8]) -> Result<(String, Vec<u8>), Failed> {
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
    let (tx, rx) = oneshot::channel();
    let (id, machine) = with(|s| {
        let mut there: Vec<(&String, &Machine)> = s.machines.iter().collect();
        if there.is_empty() {
            return Err(Failed::NoMachine);
        }
        there.sort_by(|a, b| a.0.cmp(b.0));
        s.turn = s.turn.wrapping_add(1);
        let pick = there.iter().position(|(_, m)| warm.contains(&m.name)).unwrap_or(s.turn % there.len());
        let (session, m) = (there[pick].0.clone(), there[pick].1);
        let machine = m.name.clone();
        s.next += 1;
        let id = s.next;
        let mut msg = format!("REQ {id} {name}\n").into_bytes();
        msg.extend_from_slice(request);
        if m.tx.send(msg).is_err() {
            return Err(Failed::Lost);
        }
        s.pending.insert(id, (session, tx));
        Ok((id, machine))
    })?;
    match tokio::time::timeout(Duration::from_secs(timeout + START_GRACE), rx).await {
        Ok(Ok(a)) => Ok((machine, a)),
        Ok(Err(_)) => Err(Failed::Lost),
        Err(_) => {
            with(|s| s.pending.remove(&id));
            Err(Failed::TimedOut)
        }
    }
}
