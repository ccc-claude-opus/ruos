//! Message encoding for the services: an operation byte, then fields
//! separated by NUL bytes; the last field is raw (it may hold anything).
//! Replies start with a status byte (`OK` or an error) the same way.
use alloc::vec::Vec;

pub const OK: u8 = 0;
pub const NOT_FOUND: u8 = 1;
pub const FAILED: u8 = 2;
pub const EXISTS: u8 = 3;
pub const NOT_EMPTY: u8 = 4;
pub const NOT_DIR: u8 = 5;
pub const IS_DIR: u8 = 6;

pub fn encode(op: u8, fields: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::from(&[op][..]);
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            v.push(0);
        }
        v.extend_from_slice(f);
    }
    v
}

/// (op, `n` fields); the last field takes the rest.
pub fn decode(msg: &[u8], n: usize) -> Option<(u8, Vec<&[u8]>)> {
    let (&op, mut rest) = msg.split_first()?;
    let mut f = Vec::new();
    for _ in 1..n {
        let i = rest.iter().position(|&c| c == 0)?;
        f.push(&rest[..i]);
        rest = &rest[i + 1..];
    }
    if n > 0 {
        f.push(rest);
    }
    Some((op, f))
}

pub fn text(b: &[u8]) -> &str {
    core::str::from_utf8(b).unwrap_or("")
}
