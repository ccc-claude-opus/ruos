//! Functions (kube's `kind = "Function"`, doc/serverless.md): a program
//! that answers requests. kube starts it when a request comes and no
//! instance is free, and stops it when it has been idle; one instance
//! takes one request at a time. A request is a channel with one message;
//! the answer is one message back. On `listen` the instance says `ready`
//! once, then `done` after each answer.
//!
//! The caller side: `call(name, request)` (needs `fn:NAME`).
use alloc::format;
use alloc::vec::Vec;

use crate::svc::{CONNECT, Listener};
use crate::{Channel, Error, Result, ns, signal, wait_one};

pub const DONE: &[u8] = b"done";

/// Serves requests with `f` until kube goes away. Returns the exit code.
pub fn serve(mut f: impl FnMut(&[u8]) -> Vec<u8>) -> i64 {
    let Some(l) = Listener::get() else {
        crate::println!("fn: no listen (run me as a kube Function)");
        return 1;
    };
    if l.ready().is_err() {
        return 1;
    }
    let mut buf = alloc::vec![0u8; crate::MAX_MSG_BYTES];
    loop {
        if wait_one(l.handle(), signal::READABLE | signal::PEER_CLOSED, u64::MAX).is_err() {
            return 0;
        }
        let conn = match l.poll() {
            Ok(Some(c)) => c,
            Ok(None) => continue,
            Err(_) => return 0,
        };
        let req = match wait_one(conn.0, signal::READABLE | signal::PEER_CLOSED, 10_000_000_000).and_then(|_| conn.read(&mut buf, &mut [])) {
            Ok((n, _)) => Vec::from(&buf[..n]),
            Err(_) => Vec::new(),
        };
        let answer = f(&req);
        let _ = conn.write(&answer, &[]);
        let _ = conn.close();
        if l.0.write(DONE, &[]).is_err() {
            return 0;
        }
    }
}

/// Calls Function `name` (needs `fn:NAME` in the namespace); waits up to
/// `timeout_ns` for the answer.
pub fn call(name: &str, request: &[u8], timeout_ns: u64) -> Result<Vec<u8>> {
    let f = ns::get(&format!("fn:{name}")).ok_or(Error::NotFound)?;
    let (mine, theirs) = Channel::create()?;
    Channel(f).write(CONNECT, &[theirs.0])?;
    mine.write(request, &[])?;
    wait_one(mine.0, signal::READABLE | signal::PEER_CLOSED, timeout_ns)?;
    let mut buf = alloc::vec![0u8; crate::MAX_MSG_BYTES];
    let (n, _) = mine.read(&mut buf, &mut [])?;
    let _ = mine.close();
    buf.truncate(n);
    Ok(buf)
}
