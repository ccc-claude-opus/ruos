//! A KV namespace (kube's `uses = ["kv:NAME"]`, doc/serverless.md): keys
//! and text values kept in the cluster's records on the server, shared by
//! every program given `kv:NAME`, like Cloudflare Workers KV. A key is up
//! to 64 bytes; a value up to 32 KiB.
//!
//! Requests (libsys::wire): [GET, key] → [OK, value] / [NOT_FOUND];
//! [PUT, key, value] → [OK]; [DELETE, key] → [OK]; [LIST, prefix] → [OK, keys, one a line].
use alloc::string::String;
use alloc::vec::Vec;

use crate::wire::{FAILED, NOT_FOUND, OK, encode};
use crate::{Channel, Error, Result, ns};

pub const GET: u8 = 1;
pub const PUT: u8 = 2;
pub const DELETE: u8 = 3;
pub const LIST: u8 = 4;
pub const MAX_KEY: usize = 64;
pub const MAX_VALUE: usize = 32 * 1024;

#[derive(Clone, Copy)]
pub struct Kv(pub Channel);

impl Kv {
    /// `kv:NAME` from the namespace.
    pub fn get_ns(name: &str) -> Option<Kv> {
        ns::get(&alloc::format!("kv:{name}")).map(|h| Kv(Channel(h)))
    }

    fn call(&self, op: u8, fields: &[&[u8]]) -> Result<Vec<u8>> {
        self.0.write(&encode(op, fields), &[])?;
        let mut buf = alloc::vec![0u8; MAX_VALUE + 64];
        let (n, _) = self.0.recv(&mut buf, &mut [])?;
        match buf.first() {
            Some(&OK) => Ok(Vec::from(&buf[1..n])),
            Some(&NOT_FOUND) => Err(Error::NotFound),
            Some(&FAILED) | _ => Err(Error::InvalidArgs),
        }
    }

    pub fn get(&self, key: &str) -> Result<String> {
        self.call(GET, &[key.as_bytes()]).map(|v| String::from_utf8_lossy(&v).into_owned())
    }

    pub fn put(&self, key: &str, value: &str) -> Result<()> {
        self.call(PUT, &[key.as_bytes(), value.as_bytes()]).map(|_| ())
    }

    pub fn delete(&self, key: &str) -> Result<()> {
        self.call(DELETE, &[key.as_bytes()]).map(|_| ())
    }

    pub fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let b = self.call(LIST, &[prefix.as_bytes()])?;
        Ok(String::from_utf8_lossy(&b).lines().map(String::from).collect())
    }
}
