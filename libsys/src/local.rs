//! This machine's own storage (the `local` service, doc/storage.md): in
//! the browser (IndexedDB) instead of on the server. It gives out channels
//! that speak the same protocols as the server's: `kv:NAME` (libsys::kv),
//! `redis:NAME` (libsys::redis), `sql:NAME` (libsys::sql, SQLite) and
//! `fs:ROOT` (libsys::fs). A program given one cannot tell where its data
//! is: `uses = ["sql:app@local"]` puts `sql:app` in its namespace.
//!
//! Requests (text): `connect KIND:NAME` + a channel end → `ok` / `error WHY`
//! (the end now speaks that protocol); `dup` + a channel end → `ok` (another
//! client); `ls` → `KIND NAME [SIZE]` lines.
use alloc::format;
use alloc::string::String;

use crate::{Channel, Error, Handle, Result, ns};

#[derive(Clone, Copy)]
pub struct Local(pub Channel);

impl Local {
    /// `local` from the namespace.
    pub fn get() -> Option<Local> {
        ns::get("local").map(|h| Local(Channel(h)))
    }

    fn ask(&self, msg: &str, h: Option<Handle>) -> Result<String> {
        let hs: &[Handle] = match &h {
            Some(h) => core::slice::from_ref(h),
            None => &[],
        };
        self.0.write(msg.as_bytes(), hs)?;
        let mut buf = alloc::vec![0u8; crate::MAX_MSG_BYTES];
        let (n, _) = self.0.recv(&mut buf, &mut [])?;
        Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
    }

    /// A channel for `what` (`kv:NAME`, `redis:NAME`, `sql:NAME`, `fs:ROOT`).
    pub fn connect(&self, what: &str) -> core::result::Result<Channel, String> {
        let (mine, theirs) = Channel::create().map_err(|e| format!("{e:?}"))?;
        let r = self.ask(&format!("connect {what}"), Some(theirs.0)).map_err(|e| format!("local: {e:?}"))?;
        match r.strip_prefix("error ") {
            Some(why) => {
                let _ = mine.close();
                Err(String::from(why))
            }
            None => Ok(mine),
        }
    }

    /// Another client of the service.
    pub fn dup(&self) -> Result<Local> {
        let (mine, theirs) = Channel::create()?;
        match self.ask("dup", Some(theirs.0))?.as_str() {
            "ok" => Ok(Local(mine)),
            _ => Err(Error::InvalidArgs),
        }
    }

    /// What is kept: `KIND NAME [SIZE]` lines.
    pub fn list(&self) -> Result<String> {
        self.ask("ls", None)
    }
}
