//! Services (kube): a pod that serves gets `listen` in its namespace, a pod
//! that uses service NAME gets `svc:NAME`. There is no address: to talk to
//! a service a pod makes a new channel and sends one end through
//! `svc:NAME`; kube hands it to a ready pod of that service, and from then
//! on the two talk directly. A pod that did not ask for a service has no
//! way to reach it.
//!
//! On `listen` the pod says `ready` once it can take connections; kube
//! sends `ping` (the pod answers `pong`, its liveness) and `conn` with a
//! channel for each new connection.
use crate::{Channel, Error, Handle, Result, ns};

pub const READY: &[u8] = b"ready";
pub const PING: &[u8] = b"ping";
pub const PONG: &[u8] = b"pong";
pub const CONN: &[u8] = b"conn";
pub const CONNECT: &[u8] = b"connect";

/// A connection to service `name` (needs `svc:NAME` in the namespace).
pub fn connect(name: &str) -> Result<Channel> {
    let svc = ns::get(&alloc::format!("svc:{name}")).ok_or(Error::NotFound)?;
    let (mine, theirs) = Channel::create()?;
    Channel(svc).write(CONNECT, &[theirs.0])?;
    Ok(mine)
}

/// The services this pod may use (its `svc:` names).
pub fn uses() -> impl Iterator<Item = &'static str> {
    ns::names().filter_map(|n| n.strip_prefix("svc:"))
}

/// A serving pod's end of `listen`.
#[derive(Clone, Copy)]
pub struct Listener(pub Channel);

impl Listener {
    pub fn get() -> Option<Listener> {
        ns::get("listen").map(|h| Listener(Channel(h)))
    }

    pub fn ready(&self) -> Result<()> {
        self.0.write(READY, &[])
    }

    pub fn handle(&self) -> Handle {
        self.0.0
    }

    /// Answers pings; a new connection if one came, else None (nothing
    /// waiting) or an error (kube went away).
    pub fn poll(&self) -> Result<Option<Channel>> {
        let mut buf = [0u8; 16];
        let mut hs = [Handle(0); 1];
        loop {
            match self.0.read(&mut buf, &mut hs) {
                Ok((n, 1)) if &buf[..n] == CONN => return Ok(Some(Channel(hs[0]))),
                Ok((n, _)) if &buf[..n] == PING => self.0.write(PONG, &[])?,
                Ok(_) => {}
                Err(Error::ShouldWait) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
    }
}
