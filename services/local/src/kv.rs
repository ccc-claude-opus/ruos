//! A KV namespace kept on this machine (`kv:NAME@local`): the same
//! protocol as kube's (libsys::kv), a key per storage key `k NAME HEX(key)`.
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::kv::{DELETE, GET, LIST, MAX_KEY, MAX_VALUE, PUT};
use libsys::wire::{FAILED, NOT_FOUND, OK, decode};

use crate::storage::Storage;

fn hex(b: &[u8]) -> String {
    b.iter().map(|c| format!("{c:02x}")).collect()
}

fn unhex(s: &str) -> Option<String> {
    let b: Option<Vec<u8>> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect();
    String::from_utf8(b?).ok()
}

pub fn request(s: &Storage, name: &str, msg: &[u8]) -> Vec<u8> {
    let reply = |st: u8, b: &[u8]| [&[st][..], b].concat();
    let Some((op, f)) = decode(msg, if msg.first() == Some(&PUT) { 2 } else { 1 }) else {
        return reply(FAILED, b"bad request");
    };
    if op != LIST && (f[0].is_empty() || f[0].len() > MAX_KEY) {
        return reply(FAILED, b"bad key");
    }
    let key = format!("k {name} {}", hex(f[0]));
    let done = |r: libsys::Result<()>| if r.is_ok() { reply(OK, b"") } else { reply(FAILED, b"storage") };
    match op {
        GET => match s.get(&key) {
            Ok(Some(v)) => reply(OK, &v),
            Ok(None) => reply(NOT_FOUND, b""),
            Err(_) => reply(FAILED, b"storage"),
        },
        PUT if f[1].len() <= MAX_VALUE => done(s.put(&key, f[1])),
        DELETE => done(s.delete(&key)),
        LIST => {
            let base = format!("k {name} ");
            let mut out = String::new();
            for k in s.keys(&base).unwrap_or_default() {
                if let Some(key) = unhex(&k[base.len()..])
                    && key.as_bytes().starts_with(f[0])
                {
                    out += &key;
                    out.push('\n');
                }
            }
            reply(OK, out.as_bytes())
        }
        _ => reply(FAILED, b"bad request"),
    }
}
