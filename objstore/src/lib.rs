//! ruos object store: content-addressed objects (BLAKE3, the `blake3`
//! crate), directory
//! trees (Merkle trees), image manifests and multi-architecture indexes,
//! and packs (a bundle of objects + name references). `no_std` + `alloc`,
//! shared by the store service, init, podimg and podsrv.
//!
//! Object formats (text, one record per line):
//! - tree:     `file|exec|dir <id> <size> <name>`, sorted by name;
//!   a `whiteout - 0 <name>` entry hides a lower layer's name.
//! - manifest: `ruos manifest 1`, `arch A`, `entry PATH`, `layer ID`
//!   (bottom first), `env K=V`.
//! - index:    `ruos index 1`, `name N`, `manifest ARCH ID`.
//! - pack:     `ROSPACK1`, then `obj ID LEN` + LEN bytes + `\n` per
//!   object, `ref NAME:TAG ID`, `end`.
#![no_std]

extern crate alloc;

pub mod podfile;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

/// An object id: the BLAKE3 hash of its bytes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(pub [u8; 32]);

impl Id {
    pub fn of(data: &[u8]) -> Id {
        Id(*blake3::hash(data).as_bytes())
    }

    pub fn parse(s: &str) -> Option<Id> {
        let s = s.strip_prefix("b3:").unwrap_or(s);
        if s.len() != 64 {
            return None;
        }
        let mut b = [0u8; 32];
        for i in 0..32 {
            b[i] = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        Some(Id(b))
    }

    /// The first 12 hex digits.
    pub fn short(&self) -> String {
        let mut s = self.to_string();
        s.truncate(12);
        s
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for b in self.0 {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "b3:{}", self.short())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Exec,
    Dir,
    /// Hides the same name in lower layers.
    Whiteout,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Exec => "exec",
            Kind::Dir => "dir",
            Kind::Whiteout => "whiteout",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "file" => Kind::File,
            "exec" => Kind::Exec,
            "dir" => Kind::Dir,
            "whiteout" => Kind::Whiteout,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    /// Zero for whiteouts.
    pub id: Id,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    pub entries: Vec<Entry>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Format(&'static str),
    Missing(Id),
    BadHash(Id),
    NotFound,
}

pub type Result<T> = core::result::Result<T, Error>;

fn lines(b: &[u8]) -> Result<impl Iterator<Item = &str>> {
    Ok(core::str::from_utf8(b).map_err(|_| Error::Format("not utf-8"))?.lines().filter(|l| !l.is_empty()))
}

impl Tree {
    pub fn encode(&self) -> Vec<u8> {
        let mut e: Vec<&Entry> = self.entries.iter().collect();
        e.sort_by(|a, b| a.name.cmp(&b.name));
        let mut s = String::new();
        for x in e {
            let id = if x.kind == Kind::Whiteout { "-".to_string() } else { x.id.to_string() };
            s += &alloc::format!("{} {} {} {}\n", x.kind.word(), id, x.size, x.name);
        }
        s.into_bytes()
    }

    pub fn decode(b: &[u8]) -> Result<Tree> {
        let mut entries = Vec::new();
        for l in lines(b)? {
            let mut p = l.splitn(4, ' ');
            let kind = Kind::parse(p.next().unwrap_or("")).ok_or(Error::Format("tree kind"))?;
            let id = p.next().ok_or(Error::Format("tree id"))?;
            let id = if kind == Kind::Whiteout { Id([0; 32]) } else { Id::parse(id).ok_or(Error::Format("tree id"))? };
            let size = p.next().and_then(|x| x.parse().ok()).ok_or(Error::Format("tree size"))?;
            let name = p.next().ok_or(Error::Format("tree name"))?.to_string();
            entries.push(Entry { name, kind, id, size });
        }
        Ok(Tree { entries })
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    pub arch: String,
    pub entry: String,
    /// Bottom layer first.
    pub layers: Vec<Id>,
    pub env: Vec<(String, String)>,
}

impl Manifest {
    pub fn encode(&self) -> Vec<u8> {
        let mut s = alloc::format!("ruos manifest 1\narch {}\nentry {}\n", self.arch, self.entry);
        for l in &self.layers {
            s += &alloc::format!("layer {}\n", l);
        }
        for (k, v) in &self.env {
            s += &alloc::format!("env {}={}\n", k, v);
        }
        s.into_bytes()
    }

    pub fn decode(b: &[u8]) -> Result<Manifest> {
        let mut it = lines(b)?;
        if it.next() != Some("ruos manifest 1") {
            return Err(Error::Format("not a manifest"));
        }
        let mut m = Manifest::default();
        for l in it {
            let (k, v) = l.split_once(' ').ok_or(Error::Format("manifest line"))?;
            match k {
                "arch" => m.arch = v.to_string(),
                "entry" => m.entry = v.to_string(),
                "layer" => m.layers.push(Id::parse(v).ok_or(Error::Format("layer id"))?),
                "env" => {
                    let (a, b) = v.split_once('=').ok_or(Error::Format("env"))?;
                    m.env.push((a.to_string(), b.to_string()));
                }
                _ => {}
            }
        }
        Ok(m)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    pub name: String,
    pub manifests: Vec<(String, Id)>,
}

impl Index {
    pub fn encode(&self) -> Vec<u8> {
        let mut s = alloc::format!("ruos index 1\nname {}\n", self.name);
        for (a, id) in &self.manifests {
            s += &alloc::format!("manifest {} {}\n", a, id);
        }
        s.into_bytes()
    }

    pub fn decode(b: &[u8]) -> Result<Index> {
        let mut it = lines(b)?;
        if it.next() != Some("ruos index 1") {
            return Err(Error::Format("not an index"));
        }
        let mut x = Index::default();
        for l in it {
            let mut p = l.split(' ');
            match p.next() {
                Some("name") => x.name = p.next().unwrap_or("").to_string(),
                Some("manifest") => {
                    let a = p.next().ok_or(Error::Format("index arch"))?.to_string();
                    let id = Id::parse(p.next().unwrap_or("")).ok_or(Error::Format("index id"))?;
                    x.manifests.push((a, id));
                }
                _ => {}
            }
        }
        Ok(x)
    }

    pub fn for_arch(&self, arch: &str) -> Option<Id> {
        self.manifests.iter().find(|(a, _)| a == arch).map(|(_, id)| *id)
    }
}

/// Something that holds objects.
pub trait Objects {
    fn get(&self, id: &Id) -> Option<&[u8]>;

    fn need(&self, id: &Id) -> Result<&[u8]> {
        self.get(id).ok_or(Error::Missing(*id))
    }

    /// Looks `path` up through `layers` (bottom first; upper ones win).
    fn lookup(&self, layers: &[Id], path: &str) -> Result<Entry> {
        'layer: for l in layers.iter().rev() {
            let mut tree = Tree::decode(self.need(l)?)?;
            let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
            for (i, part) in parts.iter().enumerate() {
                let Some(e) = tree.get(part).cloned() else { continue 'layer };
                if e.kind == Kind::Whiteout {
                    return Err(Error::NotFound);
                }
                if i + 1 == parts.len() {
                    return Ok(e);
                }
                if e.kind != Kind::Dir {
                    return Err(Error::NotFound);
                }
                tree = Tree::decode(self.need(&e.id)?)?;
            }
        }
        Err(Error::NotFound)
    }
}

/// An in-memory set of objects with name references (`name:tag` → index).
#[derive(Default)]
pub struct Store {
    pub objects: BTreeMap<Id, Vec<u8>>,
    pub refs: BTreeMap<String, Id>,
}

impl Objects for Store {
    fn get(&self, id: &Id) -> Option<&[u8]> {
        self.objects.get(id).map(|v| v.as_slice())
    }
}

/// `name` → `name:latest`.
pub fn full_ref(name: &str) -> String {
    if name.contains(':') { name.to_string() } else { alloc::format!("{}:latest", name) }
}

impl Store {
    pub fn put(&mut self, data: Vec<u8>) -> Id {
        let id = Id::of(&data);
        self.objects.entry(id).or_insert(data);
        id
    }

    /// The manifest of image `name[:tag]` for `arch`.
    pub fn manifest(&self, name: &str, arch: &str) -> Result<Manifest> {
        manifest_of(self, self.refs.get(&full_ref(name)).ok_or(Error::NotFound)?, arch)
    }

    /// Bytes of a pack with every object and reference.
    pub fn pack(&self) -> Vec<u8> {
        self.pack_some(|_| true)
    }

    /// A pack of the objects `keep` accepts, and all references.
    pub fn pack_some(&self, keep: impl Fn(&Id) -> bool) -> Vec<u8> {
        let mut out = Vec::from(&b"ROSPACK1\n"[..]);
        for (id, data) in &self.objects {
            if keep(id) {
                out.extend_from_slice(alloc::format!("obj {} {}\n", id, data.len()).as_bytes());
                out.extend_from_slice(data);
                out.push(b'\n');
            }
        }
        for (r, id) in &self.refs {
            out.extend_from_slice(alloc::format!("ref {} {}\n", r, id).as_bytes());
        }
        out.extend_from_slice(b"end\n");
        out
    }

    /// Every object an image (index) needs on `arch` (its wasm32 variant
    /// if it has none for `arch`), itself included.
    pub fn closure(&self, index: &Id, arch: Option<&str>) -> Result<Vec<Id>> {
        let mut out = alloc::vec![*index];
        let x = Index::decode(self.need(index)?)?;
        let arch = arch.map(|a| if x.for_arch(a).is_some() { a } else { "wasm32" });
        for (a, m) in &x.manifests {
            out.push(*m);
            if arch.is_some_and(|w| w != a) {
                continue;
            }
            let man = Manifest::decode(self.need(m)?)?;
            for l in &man.layers {
                self.tree_closure(l, &mut out)?;
            }
        }
        Ok(out)
    }

    fn tree_closure(&self, t: &Id, out: &mut Vec<Id>) -> Result<()> {
        out.push(*t);
        for e in Tree::decode(self.need(t)?)?.entries {
            match e.kind {
                Kind::Dir => self.tree_closure(&e.id, out)?,
                Kind::File | Kind::Exec => out.push(e.id),
                Kind::Whiteout => {}
            }
        }
        Ok(())
    }
}

/// An index's manifest for `arch`.
pub fn manifest_of(o: &impl Objects, index: &Id, arch: &str) -> Result<Manifest> {
    let x = Index::decode(o.need(index)?)?;
    Manifest::decode(o.need(&x.for_arch(arch).ok_or(Error::NotFound)?)?)
}

/// A pack read in place.
pub struct Pack<'a> {
    pub objects: BTreeMap<Id, &'a [u8]>,
    pub refs: BTreeMap<String, Id>,
}

impl Objects for Pack<'_> {
    fn get(&self, id: &Id) -> Option<&[u8]> {
        self.objects.get(id).copied()
    }
}

impl<'a> Pack<'a> {
    /// Parses a pack; with `verify`, every object's hash is checked.
    pub fn parse(b: &'a [u8], verify: bool) -> Result<Pack<'a>> {
        let mut p = Pack { objects: BTreeMap::new(), refs: BTreeMap::new() };
        let rest = b.strip_prefix(b"ROSPACK1\n").ok_or(Error::Format("not a pack"))?;
        let mut at = 0;
        loop {
            let nl = rest[at..].iter().position(|&c| c == b'\n').ok_or(Error::Format("pack line"))?;
            let line = core::str::from_utf8(&rest[at..at + nl]).map_err(|_| Error::Format("pack line"))?;
            at += nl + 1;
            let mut w = line.split(' ');
            match w.next() {
                Some("obj") => {
                    let id = Id::parse(w.next().unwrap_or("")).ok_or(Error::Format("pack id"))?;
                    let len: usize = w.next().and_then(|x| x.parse().ok()).ok_or(Error::Format("pack len"))?;
                    let data = rest.get(at..at + len).ok_or(Error::Format("pack truncated"))?;
                    if verify && Id::of(data) != id {
                        return Err(Error::BadHash(id));
                    }
                    p.objects.insert(id, data);
                    at += len + 1;
                }
                Some("ref") => {
                    let r = w.next().ok_or(Error::Format("pack ref"))?.to_string();
                    let id = Id::parse(w.next().unwrap_or("")).ok_or(Error::Format("pack ref id"))?;
                    p.refs.insert(r, id);
                }
                Some("end") => return Ok(p),
                _ => return Err(Error::Format("pack record")),
            }
        }
    }

    pub fn manifest(&self, name: &str, arch: &str) -> Result<Manifest> {
        manifest_of(self, self.refs.get(&full_ref(name)).ok_or(Error::NotFound)?, arch)
    }

    /// The entry program of image `name` for `arch`.
    pub fn program(&self, name: &str, arch: &str) -> Result<&'a [u8]> {
        let m = self.manifest(name, arch)?;
        let e = self.lookup(&m.layers, &m.entry)?;
        self.objects.get(&e.id).copied().ok_or(Error::Missing(e.id))
    }
}

#[cfg(test)]
mod tests;
