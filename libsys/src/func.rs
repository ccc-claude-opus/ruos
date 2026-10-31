//! Functions (kube's `kind = "Function"`, doc/serverless.md): a program
//! that answers requests. kube starts it when a request comes and no
//! instance is free, and stops it when it has been idle; one instance
//! takes one request at a time. A request is a channel with one message;
//! the answer is one message back. On `listen` the instance says `ready`
//! once, then `done` after each answer.
//!
//! The caller side: `call(name, request)` (needs `fn:NAME`).
//!
//! A Function with `expose = true` also takes requests from outside
//! (doc/ingress.md): an HTTP envelope, `METHOD PATH?QUERY\n` + header lines
//! + `\n` + body; `serve_http` reads it and writes the answer the same way
//! (`HTTP STATUS\n` + header lines + `\n` + body).
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

/// A request from outside (doc/ingress.md): what `serve_http` gives the
/// function. A plain request (`fn call`) is method `CALL`, path `/`, the
/// text as body.
pub struct Request {
    pub method: alloc::string::String,
    /// After `/fn/NAME` (at least `/`).
    pub path: alloc::string::String,
    pub query: alloc::string::String,
    pub headers: Vec<(alloc::string::String, alloc::string::String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn parse(raw: &[u8]) -> Request {
        use alloc::string::{String, ToString};
        let plain = || Request { method: String::from("CALL"), path: String::from("/"), query: String::new(), headers: Vec::new(), body: Vec::from(raw) };
        let Some(end) = raw.windows(2).position(|w| w == b"\n\n") else { return plain() };
        let head = String::from_utf8_lossy(&raw[..end]).into_owned();
        let mut lines = head.lines();
        let Some((method, target)) = lines.next().and_then(|l| l.split_once(' ')) else { return plain() };
        if !target.starts_with('/') || !method.bytes().all(|c| c.is_ascii_uppercase()) {
            return plain();
        }
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let headers = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
        Request { method: method.to_string(), path: path.to_string(), query: query.to_string(), headers, body: Vec::from(&raw[end + 2..]) }
    }

    /// A header, by name (any case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// A query parameter (`a=1&b=2`; no %-decoding but `+` is a space).
    pub fn param(&self, name: &str) -> Option<alloc::string::String> {
        self.query.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == name).map(|(_, v)| v.replace('+', " ")))
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(alloc::string::String, alloc::string::String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Response {
        Response { status, headers: alloc::vec![(alloc::string::String::from("Content-Type"), alloc::string::String::from(content_type))], body: body.into() }
    }

    pub fn text(status: u16, body: &str) -> Response {
        Response::new(status, "text/plain; charset=utf-8", body.as_bytes())
    }

    pub fn html(body: &str) -> Response {
        Response::new(200, "text/html; charset=utf-8", body.as_bytes())
    }

    pub fn json(body: &str) -> Response {
        Response::new(200, "application/json", body.as_bytes())
    }

    /// `HTTP STATUS\n` + header lines + `\n` + body.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = format!("HTTP {}\n", self.status);
        for (k, v) in &self.headers {
            out += &format!("{k}: {v}\n");
        }
        out.push('\n');
        let mut b = out.into_bytes();
        b.extend_from_slice(&self.body);
        b
    }
}

/// Serves requests from outside (and `fn call`s) with `f`.
pub fn serve_http(mut f: impl FnMut(&Request) -> Response) -> i64 {
    serve(|raw| f(&Request::parse(raw)).encode())
}
