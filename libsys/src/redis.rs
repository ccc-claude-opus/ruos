//! A Redis of the backend (`redis:NAME` in the namespace; ruos serve's
//! backend.rs, `--redis NAME=URL`): a command and its arguments go to the
//! server through the store; the answer is JSON (a string, a number, null,
//! or an array).
use alloc::format;
use alloc::string::String;

use crate::sql::json_str;
use crate::store::{REDIS, Store};
use crate::{Channel, Result, ns};

const OK: u8 = 0;

#[derive(Clone)]
pub struct Redis {
    store: Store,
    name: String,
}

impl Redis {
    /// `redis:NAME` from the namespace.
    pub fn get(name: &str) -> Option<Redis> {
        ns::get(&format!("redis:{name}")).map(|h| Redis { store: Store(Channel(h)), name: String::from(name) })
    }

    /// Any store client that may use `redis:NAME` (the shell's backend).
    pub fn with(store: Store, name: &str) -> Redis {
        Redis { store, name: String::from(name) }
    }

    /// `args[0]` is the command (`GET`, `INCR`, …). Err(why) when Redis said no.
    pub fn cmd(&self, args: &[&str]) -> Result<core::result::Result<String, String>> {
        self.call(args, "json")
    }

    /// The answer as text.
    pub fn cmd_text(&self, args: &[&str]) -> Result<core::result::Result<String, String>> {
        self.call(args, "text")
    }

    fn call(&self, args: &[&str], format: &str) -> Result<core::result::Result<String, String>> {
        let mut j = String::from("[");
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                j.push(',');
            }
            j += &json_str(a);
        }
        j.push(']');
        let (st, b) = self.store.raw(REDIS, &[self.name.as_bytes(), j.as_bytes(), format.as_bytes()])?;
        let t = String::from_utf8_lossy(&b).into_owned();
        Ok(if st == OK { Ok(t) } else { Err(t) })
    }
}
