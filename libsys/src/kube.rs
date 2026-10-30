//! The kube service's protocol: text, like dock's. A request is a command
//! line; `apply` is followed by a newline and the manifest (TOML). Commands:
//!   apply\nMANIFEST         a Deployment or a Space        → `deployment/NAME created|configured|unchanged`
//!   get [pods|deploy|spaces]                               → tables
//!   describe NAME           a Deployment, with its events  → text
//!   scale NAME N                                           → `deployment/NAME scaled to N`
//!   delete NAME             the Deployment and its pods    → `deployment/NAME deleted`
//!   logs POD                what the pod printed           → `[POD] line` lines
//! Errors are `error MESSAGE`.
use alloc::string::String;

use crate::{Channel, Result};

#[derive(Clone, Copy)]
pub struct Kube(pub Channel);

impl Kube {
    pub fn request(&self, msg: &str) -> Result<String> {
        self.0.write(msg.as_bytes(), &[])?;
        let mut buf = alloc::vec![0u8; crate::MAX_MSG_BYTES];
        let (n, _) = self.0.recv(&mut buf, &mut [])?;
        buf.truncate(n);
        Ok(String::from_utf8(buf).unwrap_or_default())
    }
}
