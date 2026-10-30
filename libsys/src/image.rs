//! Images in the OS: building them (a layer from files, and a new image
//! that is a base image with that layer on top, for every architecture it
//! has; used by commits and `dock build`) and looking inside them (the
//! shell's `image`, `ls`, `cat`).
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use objstore::{Entry, Id, Index, Kind, Manifest, Tree};

use crate::store::{MAX_INLINE, Store};
use crate::{Error, Memory, Result};

/// Stores `data` as an object (as a memory object when it is large).
pub fn put(store: &Store, data: &[u8]) -> Result<Id> {
    let id = if data.len() <= MAX_INLINE {
        store.put(data)?
    } else {
        let m = Memory::create(data.len() as u64)?;
        m.write(0, data)?;
        store.put_memory(m, data.len() as u64)?
    };
    Id::parse(&id).ok_or(Error::InvalidArgs)
}

#[derive(Default)]
struct Dir {
    entries: BTreeMap<String, Entry>,
    dirs: BTreeMap<String, Dir>,
}

fn store_dir(s: &Store, d: &Dir) -> Result<Id> {
    let mut t = Tree::default();
    for (n, sub) in &d.dirs {
        t.entries.push(Entry { name: n.clone(), kind: Kind::Dir, id: store_dir(s, sub)?, size: 0 });
    }
    t.entries.extend(d.entries.values().cloned());
    put(s, &t.encode())
}

/// A layer: `path` → (object, size) of a stored file, or None for a whiteout.
pub fn layer(store: &Store, files: &[(String, Option<(Id, u64)>)]) -> Result<Id> {
    let mut root = Dir::default();
    for (p, v) in files {
        let parts: Vec<&str> = p.split('/').filter(|x| !x.is_empty()).collect();
        let Some((last, dirs)) = parts.split_last() else { continue };
        let mut d = &mut root;
        for x in dirs {
            d = d.dirs.entry(String::from(*x)).or_default();
        }
        let e = match v {
            Some((id, size)) => Entry { name: String::from(*last), kind: Kind::File, id: *id, size: *size },
            None => Entry { name: String::from(*last), kind: Kind::Whiteout, id: Id([0; 32]), size: 0 },
        };
        d.entries.insert(String::from(*last), e);
    }
    store_dir(store, &root)
}

/// `base` with `layer` on top as image `name` (`name:tag`); every
/// architecture of `base` gets the layer. Returns the new index id.
pub fn derive(
    store: &Store,
    base: &str,
    name: &str,
    layer: Id,
    entry: Option<&str>,
    env: &[(String, String)],
) -> Result<String> {
    let old = store.object(&store.index(base)?)?;
    let old = Index::decode(&old).map_err(|_| Error::InvalidArgs)?;
    let bare = name.split(':').next().unwrap_or(name);
    let mut index = Index { name: String::from(bare), manifests: Vec::new() };
    for (arch, m) in &old.manifests {
        let mut man = Manifest::decode(&store.object(&alloc::format!("{m}"))?).map_err(|_| Error::InvalidArgs)?;
        man.layers.push(layer);
        if let Some(e) = entry {
            man.entry = String::from(e);
        }
        for (k, v) in env {
            man.env.retain(|(a, _)| a != k);
            man.env.push((k.clone(), v.clone()));
        }
        index.manifests.push((arch.clone(), put(store, &man.encode())?));
    }
    let id = store.put(&index.encode())?;
    store.set_ref(name, &id)?;
    Ok(id)
}

/// The manifest an image runs with here (this architecture's, else wasm32's).
pub fn manifest(store: &Store, image: &str) -> Result<Manifest> {
    let idx = Id::parse(&store.index(image)?).ok_or(Error::InvalidArgs)?;
    let x = Index::decode(&store.object(&alloc::format!("{idx}"))?).map_err(|_| Error::InvalidArgs)?;
    let m = x.for_arch(crate::ARCH).or_else(|| x.for_arch("wasm32")).ok_or(Error::NotFound)?;
    Manifest::decode(&store.object(&alloc::format!("{m}"))?).map_err(|_| Error::InvalidArgs)
}

/// An object as a tree.
pub fn tree(store: &Store, id: &Id) -> Result<Tree> {
    Tree::decode(&store.object(&alloc::format!("{id}"))?).map_err(|_| Error::InvalidArgs)
}

/// The entry at `path` in `layers` (bottom first; upper layers win, a
/// whiteout hides what is below). "/" is the top directory (no entry).
pub fn lookup(store: &Store, layers: &[Id], path: &str) -> Result<Entry> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    'layer: for l in layers.iter().rev() {
        let mut t = tree(store, l)?;
        for (i, part) in parts.iter().enumerate() {
            let Some(e) = t.get(part).cloned() else { continue 'layer };
            if e.kind == Kind::Whiteout {
                return Err(Error::NotFound);
            }
            if i + 1 == parts.len() {
                return Ok(e);
            }
            if e.kind != Kind::Dir {
                return Err(Error::NotFound);
            }
            t = tree(store, &e.id)?;
        }
    }
    Err(Error::NotFound)
}

/// Directory `dir` as the layers together make it (by name).
pub fn list(store: &Store, layers: &[Id], dir: &str) -> Result<Vec<Entry>> {
    let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    let mut out: BTreeMap<String, Entry> = BTreeMap::new();
    let mut found = parts.is_empty();
    'layer: for l in layers {
        let mut t = tree(store, l)?;
        for part in &parts {
            match t.get(part) {
                Some(e) if e.kind == Kind::Dir => t = tree(store, &e.id.clone())?,
                Some(e) if e.kind == Kind::Whiteout => {
                    out.clear();
                    found = false;
                    continue 'layer;
                }
                _ => continue 'layer,
            }
        }
        found = true;
        for e in t.entries {
            match e.kind {
                Kind::Whiteout => _ = out.remove(&e.name),
                _ => _ = out.insert(e.name.clone(), e),
            }
        }
    }
    if !found {
        return Err(Error::NotFound);
    }
    Ok(out.into_values().collect())
}
