//! The machine's own storage, as the host keeps it (web/local_store.js):
//! keys → bytes, one request at a time.
use alloc::string::String;
use alloc::vec::Vec;

use libsys::wire::{NOT_FOUND, OK, encode};
use libsys::{Channel, Error, Result};

const GET: u8 = 1;
const PUT: u8 = 2;
const DELETE: u8 = 3;
const LIST: u8 = 4;

pub struct Storage(pub Channel);

impl Storage {
    fn call(&self, op: u8, fields: &[&[u8]]) -> Result<(u8, Vec<u8>)> {
        self.0.write(&encode(op, fields), &[])?;
        let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES];
        let (n, _) = self.0.recv(&mut buf, &mut [])?;
        if n == 0 {
            return Err(Error::InvalidArgs);
        }
        buf.truncate(n);
        let st = buf.remove(0);
        Ok((st, buf))
    }

    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match self.call(GET, &[key.as_bytes()])? {
            (OK, v) => Ok(Some(v)),
            (NOT_FOUND, _) => Ok(None),
            _ => Err(Error::Fault),
        }
    }

    pub fn put(&self, key: &str, value: &[u8]) -> Result<()> {
        match self.call(PUT, &[key.as_bytes(), value])? {
            (OK, _) => Ok(()),
            _ => Err(Error::Fault),
        }
    }

    pub fn delete(&self, key: &str) -> Result<()> {
        match self.call(DELETE, &[key.as_bytes()])? {
            (OK, _) => Ok(()),
            _ => Err(Error::Fault),
        }
    }

    /// Every key that starts with `prefix`, in order.
    pub fn keys(&self, prefix: &str) -> Result<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        loop {
            let after = out.last().cloned().unwrap_or_default();
            let (st, page) = self.call(LIST, &[prefix.as_bytes(), after.as_bytes()])?;
            if st != OK {
                return Err(Error::Fault);
            }
            if page.is_empty() {
                return Ok(out);
            }
            out.extend(String::from_utf8_lossy(&page).split('\n').map(String::from));
        }
    }
}
