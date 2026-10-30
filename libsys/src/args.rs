//! A program's arguments and environment: a memory object named
//! `exec:LEN` in its namespace holding `arg\0arg\0\0K=V\0K=V\0`.
use alloc::string::String;
use alloc::vec::Vec;

use crate::ns::{self, Namespace};
use crate::{Memory, Result};

pub fn encode(args: &[&str], env: &[(String, String)]) -> Vec<u8> {
    let mut b = Vec::new();
    for a in args {
        b.extend_from_slice(a.as_bytes());
        b.push(0);
    }
    b.push(0);
    for (k, v) in env {
        b.extend_from_slice(k.as_bytes());
        b.push(b'=');
        b.extend_from_slice(v.as_bytes());
        b.push(0);
    }
    b
}

/// `ns` with the arguments and environment added.
pub fn add(ns: Namespace, args: &[&str], env: &[(String, String)]) -> Result<Namespace> {
    let b = encode(args, env);
    let m = Memory::create(b.len() as u64)?;
    m.write(0, &b)?;
    Ok(ns.add(&alloc::format!("exec:{}", b.len()), m.0))
}

/// (arguments, `K=V` strings) as raw bytes, from the namespace.
pub fn load() -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let Some((h, len)) = ns::names().find_map(|n| Some((ns::get(n)?, n.strip_prefix("exec:")?.parse::<usize>().ok()?)))
    else {
        return (Vec::new(), Vec::new());
    };
    let mut b = alloc::vec![0u8; len];
    if Memory(h).read(0, &mut b).is_err() {
        return (Vec::new(), Vec::new());
    }
    let (args, env) = match b.windows(2).position(|w| w == [0, 0]) {
        Some(i) => (&b[..i + 1], &b[i + 2..]),
        None if b.first() == Some(&0) => (&b[..0], &b[1..]),
        None => (&b[..], &b[..0]),
    };
    let split = |s: &[u8]| s.split(|&c| c == 0).filter(|x| !x.is_empty()).map(Vec::from).collect::<Vec<_>>();
    (split(args), split(env))
}
