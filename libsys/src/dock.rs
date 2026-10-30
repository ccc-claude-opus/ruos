//! The dock service's protocol: a request is a text command line, the
//! reply is text. Commands:
//!   run [-d] [--name N] [--mem SIZE] [--tasks N] [--label K=V]... [--space S] [--quiet]
//!       [--handle NAME]... IMAGE [ARGS]                 → `ok ID NAME`
//!       (each --handle names the next handle sent with the command; the
//!       pod's environment has POD=NAME)
//!   ps                                                  → a table
//!   pods                    for programs                → `ID NAME running|exited:CODE|stopped K=V,…|-` lines
//!   watch                   then events, one a message  → `ok watching`, `event start|exit|stop|rm ID NAME [CODE]`
//!   space NAME SIZE TASKS   a child quota for pods      → `ok space NAME`
//!   spaces                                              → a table
//!   shutdown                every running pod ends      → `ok stopped N`
//!   stop ID|NAME                                        → `ok stopped NAME`
//!   wait ID|NAME   (answered when the pod ends)         → `exit CODE` / `stopped`
//!   commit ID|NAME IMAGE[:TAG]                          → `ok committed REF ID FILES REMOVED`
//!   push IMAGE[:TAG]                                    → `ok pushed REF: N objects`
//!   logs ID|NAME            what the pod printed        → `[NAME] line` lines
//!   rm ID|NAME              stop it, forget it          → `ok removed NAME`
//!   write ID|NAME PATH TEXT a file in its file system   → `ok` (TEXT: \n \t \\)
//!   build ID|NAME PODFILE   an image from a Podfile in its file system
//!                           (`from` + `[files]` from that file system)
//!                                                       → `ok built REF ID FILES FROM`
//! Errors are `error MESSAGE`.
use alloc::string::String;

use crate::{Channel, Result};

#[derive(Clone, Copy)]
pub struct Dock(pub Channel);

impl Dock {
    pub fn request(&self, cmd: &str) -> Result<String> {
        self.request_with(cmd, &[])
    }

    /// A command with handles (they move to dock).
    pub fn request_with(&self, cmd: &str, handles: &[crate::Handle]) -> Result<String> {
        self.0.write(cmd.as_bytes(), handles)?;
        let mut buf = alloc::vec![0u8; crate::MAX_MSG_BYTES];
        let (n, _) = self.0.recv(&mut buf, &mut [])?;
        buf.truncate(n);
        Ok(String::from_utf8(buf).unwrap_or_default())
    }
}

/// `4M`, `512K`, `1G` or bytes.
pub fn parse_size(s: &str) -> Option<u64> {
    let (num, mul) = match s.as_bytes().last()? {
        b'K' | b'k' => (&s[..s.len() - 1], 1 << 10),
        b'M' | b'm' => (&s[..s.len() - 1], 1 << 20),
        b'G' | b'g' => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    num.parse::<u64>().ok().map(|n| n * mul)
}
