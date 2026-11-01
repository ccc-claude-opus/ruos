//! A file system on the machine's storage (the fs protocol, libsys::fs):
//! one tree per `root`. Keys: `m ROOT PATH` → `file SIZE` / `dir`, and
//! `c ROOT N PATH` → the file's Nth chunk of CHUNK bytes (a chunk past the
//! end of the data, or one never written, reads as zeros). The tree is
//! kept in memory once read; chunks are cached.
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::fs::{DELETE, LIST, MKDIR, READ, READ_AT, RENAME, RMDIR, STAT, TRUNCATE, UNLINK, WRITE, WRITE_AT};
use libsys::wire::{EXISTS, FAILED, IS_DIR, NOT_DIR, NOT_EMPTY, NOT_FOUND, OK, decode, text};

use crate::storage::Storage;

const CHUNK: u64 = 32 * 1024;
/// Chunks kept in memory (4 MiB).
const CACHED: usize = 128;

#[derive(Clone, Copy, PartialEq)]
enum Node {
    File(u64),
    Dir,
}

pub struct Files {
    root: String,
    tree: BTreeMap<String, Node>,
    cache: BTreeMap<(String, u64), Vec<u8>>,
}

/// `/a/b/` → `/a/b`; `a` → `/a`; `..` and `.` resolved.
pub fn norm(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for s in p.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

fn parent(p: &str) -> &str {
    match p.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &p[..i],
    }
}

fn reply(st: u8, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + body.len());
    v.push(st);
    v.extend_from_slice(body);
    v
}

impl Files {
    /// The tree under `root` (read from storage).
    pub fn open(s: &Storage, root: &str) -> Files {
        let prefix = format!("m {root} ");
        let mut tree = BTreeMap::new();
        for k in s.keys(&prefix).unwrap_or_default() {
            let path = String::from(&k[prefix.len()..]);
            let node = match s.get(&k).ok().flatten() {
                Some(v) if v == b"dir" => Node::Dir,
                Some(v) => Node::File(text(&v).strip_prefix("file ").and_then(|n| n.parse().ok()).unwrap_or(0)),
                None => continue,
            };
            tree.insert(path, node);
        }
        Files { root: String::from(root), tree, cache: BTreeMap::new() }
    }

    fn node(&self, p: &str) -> Option<Node> {
        if p == "/" { Some(Node::Dir) } else { self.tree.get(p).copied() }
    }

    fn set(&mut self, s: &Storage, p: &str, n: Node) {
        let v = match n {
            Node::Dir => String::from("dir"),
            Node::File(size) => format!("file {size}"),
        };
        let _ = s.put(&format!("m {} {p}", self.root), v.as_bytes());
        self.tree.insert(String::from(p), n);
    }

    fn unset(&mut self, s: &Storage, p: &str) {
        if let Some(Node::File(size)) = self.tree.remove(p) {
            for i in 0..size.div_ceil(CHUNK) {
                let _ = s.delete(&self.chunk_key(p, i));
                self.cache.remove(&(String::from(p), i));
            }
        }
        let _ = s.delete(&format!("m {} {p}", self.root));
    }

    fn chunk_key(&self, p: &str, i: u64) -> String {
        format!("c {} {i} {p}", self.root)
    }

    fn chunk(&mut self, s: &Storage, p: &str, i: u64) -> Vec<u8> {
        let k = (String::from(p), i);
        if let Some(c) = self.cache.get(&k) {
            return c.clone();
        }
        let c = s.get(&self.chunk_key(p, i)).ok().flatten().unwrap_or_default();
        if self.cache.len() >= CACHED {
            self.cache.pop_first();
        }
        self.cache.insert(k, c.clone());
        c
    }

    fn put_chunk(&mut self, s: &Storage, p: &str, i: u64, c: Vec<u8>) {
        let _ = s.put(&self.chunk_key(p, i), &c);
        if self.cache.len() >= CACHED {
            self.cache.pop_first();
        }
        self.cache.insert((String::from(p), i), c);
    }

    /// Makes `p`'s missing parent directories.
    fn parents(&mut self, s: &Storage, p: &str) -> Result<(), u8> {
        let d = parent(p);
        match self.node(d) {
            Some(Node::Dir) => Ok(()),
            Some(Node::File(_)) => Err(NOT_DIR),
            None => {
                self.parents(s, d)?;
                self.set(s, d, Node::Dir);
                Ok(())
            }
        }
    }

    fn read_at(&mut self, s: &Storage, p: &str, off: u64, len: u64) -> Vec<u8> {
        let Some(Node::File(size)) = self.node(p) else { return Vec::new() };
        let end = (off + len).min(size);
        let mut out = Vec::new();
        let mut at = off;
        while at < end {
            let (i, o) = (at / CHUNK, (at % CHUNK) as usize);
            let n = ((CHUNK as usize - o) as u64).min(end - at) as usize;
            let c = self.chunk(s, p, i);
            let mut part = alloc::vec![0u8; n];
            if o < c.len() {
                let m = n.min(c.len() - o);
                part[..m].copy_from_slice(&c[o..o + m]);
            }
            out.extend_from_slice(&part);
            at += n as u64;
        }
        out
    }

    fn write_at(&mut self, s: &Storage, p: &str, off: u64, data: &[u8]) {
        let size = match self.node(p) {
            Some(Node::File(n)) => n,
            _ => 0,
        };
        let mut at = off;
        let mut d = data;
        while !d.is_empty() {
            let (i, o) = (at / CHUNK, (at % CHUNK) as usize);
            let n = (CHUNK as usize - o).min(d.len());
            let mut c = self.chunk(s, p, i);
            if c.len() < o + n {
                c.resize(o + n, 0);
            }
            c[o..o + n].copy_from_slice(&d[..n]);
            self.put_chunk(s, p, i, c);
            d = &d[n..];
            at += n as u64;
        }
        let new = size.max(off + data.len() as u64);
        if new != size || self.node(p).is_none() {
            self.set(s, p, Node::File(new));
        }
    }

    fn truncate(&mut self, s: &Storage, p: &str, to: u64) {
        let Some(Node::File(size)) = self.node(p) else { return };
        if to < size {
            for i in to.div_ceil(CHUNK)..size.div_ceil(CHUNK) {
                let _ = s.delete(&self.chunk_key(p, i));
                self.cache.remove(&(String::from(p), i));
            }
            // The last chunk kept loses its tail (it must read as zeros if the file grows again).
            if to % CHUNK != 0 {
                let i = to / CHUNK;
                let mut c = self.chunk(s, p, i);
                c.truncate((to % CHUNK) as usize);
                self.put_chunk(s, p, i, c);
            }
        }
        self.set(s, p, Node::File(to));
    }

    /// The direct children of directory `d`.
    fn children(&self, d: &str) -> Vec<(String, Node)> {
        let prefix = if d == "/" { String::from("/") } else { format!("{d}/") };
        self.tree
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .filter(|(k, _)| !k[prefix.len()..].contains('/'))
            .map(|(k, n)| (String::from(&k[prefix.len()..]), *n))
            .collect()
    }

    /// Everything under `p` (itself first).
    fn subtree(&self, p: &str) -> Vec<(String, Node)> {
        let prefix = format!("{p}/");
        let mut v: Vec<(String, Node)> = self.node(p).map(|n| (String::from(p), n)).into_iter().collect();
        v.extend(self.tree.range(prefix.clone()..).take_while(|(k, _)| k.starts_with(&prefix)).map(|(k, n)| (k.clone(), *n)));
        v
    }

    fn rename(&mut self, s: &Storage, from: &str, to: &str) -> u8 {
        if from == "/" || to.starts_with(&format!("{from}/")) {
            return FAILED;
        }
        let Some(n) = self.node(from) else { return NOT_FOUND };
        match (n, self.node(to)) {
            (_, Some(Node::Dir)) if to == "/" || !self.children(to).is_empty() => return NOT_EMPTY,
            (Node::File(_), Some(Node::Dir)) => return IS_DIR,
            (Node::Dir, Some(Node::File(_))) => return NOT_DIR,
            _ => {}
        }
        if let Err(st) = self.parents(s, to) {
            return st;
        }
        for (p, _) in self.subtree(to).into_iter().rev() {
            self.unset(s, &p);
        }
        for (p, n) in self.subtree(from) {
            let q = format!("{to}{}", &p[from.len()..]);
            match n {
                Node::Dir => self.set(s, &q, Node::Dir),
                Node::File(size) => {
                    for i in 0..size.div_ceil(CHUNK) {
                        let c = self.chunk(s, &p, i);
                        self.put_chunk(s, &q, i, c);
                    }
                    self.set(s, &q, Node::File(size));
                }
            }
        }
        for (p, _) in self.subtree(from).into_iter().rev() {
            self.unset(s, &p);
        }
        OK
    }

    /// One fs request (not CONNECT or OPEN_MEM): the reply.
    pub fn request(&mut self, s: &Storage, msg: &[u8]) -> Vec<u8> {
        let num = |b: &[u8]| text(b).parse::<u64>().ok();
        let op = msg.first().copied().unwrap_or(0);
        let n = match op {
            WRITE | RENAME | TRUNCATE => 2,
            READ_AT | WRITE_AT => 3,
            _ => 1,
        };
        let Some((_, f)) = decode(msg, n) else { return reply(FAILED, b"") };
        let p = norm(text(f[0]));
        match op {
            STAT => match self.node(&p) {
                Some(Node::File(size)) => reply(OK, format!("file {size}").as_bytes()),
                Some(Node::Dir) => reply(OK, b"dir 0"),
                None => reply(NOT_FOUND, b""),
            },
            READ => match self.node(&p) {
                Some(Node::File(size)) => {
                    let b = self.read_at(s, &p, 0, size.min(libsys::fs::CHUNK as u64));
                    reply(OK, &b)
                }
                Some(Node::Dir) => reply(IS_DIR, b""),
                None => reply(NOT_FOUND, b""),
            },
            READ_AT => match (self.node(&p), num(f[1]), num(f[2])) {
                (Some(Node::File(_)), Some(off), Some(len)) => {
                    let b = self.read_at(s, &p, off, len.min(libsys::fs::CHUNK as u64));
                    reply(OK, &b)
                }
                (Some(Node::Dir), ..) => reply(IS_DIR, b""),
                (None, ..) => reply(NOT_FOUND, b""),
                _ => reply(FAILED, b""),
            },
            WRITE | WRITE_AT => {
                if self.node(&p) == Some(Node::Dir) {
                    return reply(IS_DIR, b"");
                }
                if let Err(st) = self.parents(s, &p) {
                    return reply(st, b"");
                }
                if op == WRITE {
                    if self.node(&p).is_some() {
                        self.truncate(s, &p, 0);
                    }
                    self.write_at(s, &p, 0, f[1]);
                } else {
                    let Some(off) = num(f[1]) else { return reply(FAILED, b"") };
                    self.write_at(s, &p, off, f[2]);
                }
                reply(OK, b"")
            }
            TRUNCATE => match (self.node(&p), num(f[1])) {
                (Some(Node::File(_)), Some(n)) => {
                    self.truncate(s, &p, n);
                    reply(OK, b"")
                }
                (Some(Node::Dir), _) => reply(IS_DIR, b""),
                (None, _) => reply(NOT_FOUND, b""),
                _ => reply(FAILED, b""),
            },
            LIST => match self.node(&p) {
                Some(Node::Dir) => {
                    let mut out = String::new();
                    for (name, n) in self.children(&p) {
                        out += &match n {
                            Node::Dir => format!("dir 0 {name}\n"),
                            Node::File(size) => format!("file {size} {name}\n"),
                        };
                    }
                    reply(OK, out.as_bytes())
                }
                Some(Node::File(_)) => reply(NOT_DIR, b""),
                None => reply(NOT_FOUND, b""),
            },
            MKDIR => match (self.node(&p), self.node(parent(&p))) {
                (Some(_), _) => reply(EXISTS, b""),
                (None, Some(Node::Dir)) => {
                    self.set(s, &p, Node::Dir);
                    reply(OK, b"")
                }
                (None, Some(Node::File(_))) => reply(NOT_DIR, b""),
                (None, None) => reply(NOT_FOUND, b""),
            },
            RMDIR | DELETE | UNLINK => match self.node(&p) {
                None => reply(NOT_FOUND, b""),
                Some(Node::Dir) if op == UNLINK => reply(IS_DIR, b""),
                Some(Node::File(_)) if op == RMDIR => reply(NOT_DIR, b""),
                Some(Node::Dir) if p == "/" || !self.children(&p).is_empty() => reply(NOT_EMPTY, b""),
                Some(_) => {
                    self.unset(s, &p);
                    reply(OK, b"")
                }
            },
            RENAME => {
                let st = self.rename(s, &p, &norm(text(f[1])));
                reply(st, b"")
            }
            _ => reply(FAILED, b"unknown request"),
        }
    }

    /// The whole file, if it is one (the caches' and databases' own use).
    pub fn load(&mut self, s: &Storage, p: &str) -> Option<Vec<u8>> {
        match self.node(p)? {
            Node::File(size) => Some(self.read_at(s, p, 0, size)),
            Node::Dir => None,
        }
    }

    /// Replaces a whole file.
    pub fn save(&mut self, s: &Storage, p: &str, data: &[u8]) {
        let _ = self.parents(s, p);
        if self.node(p).is_some() {
            self.truncate(s, p, 0);
        }
        self.write_at(s, p, 0, data);
    }

    /// Removes `p` and everything under it (`/`: the whole tree). False if
    /// there was nothing.
    pub fn remove(&mut self, s: &Storage, p: &str) -> bool {
        let all: Vec<String> = if p == "/" {
            self.tree.keys().cloned().collect()
        } else {
            self.subtree(p).into_iter().map(|(k, _)| k).collect()
        };
        for k in all.iter().rev() {
            self.unset(s, k);
        }
        !all.is_empty()
    }

    /// Names and sizes of the files directly in `d`.
    pub fn files_in(&self, d: &str) -> Vec<(String, u64)> {
        self.children(d).into_iter().filter_map(|(n, x)| match x {
            Node::File(size) => Some((n, size)),
            Node::Dir => None,
        }).collect()
    }
}
