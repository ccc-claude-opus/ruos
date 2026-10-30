//! overlay: one pod's file system. Read-only layers of the pod's image
//! (objects from the store) under a writable layer (kept by the store, on
//! the server on wasm32). Namespace: `store`, `fs` (the pod's requests),
//! `control` (dock: first a CONFIG message, then COMMIT requests).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use libsys::fs::{
    COMMIT, CONNECT, DELETE, DIR_MARKER, LIST, MKDIR, OPEN_MEM, READ, READ_AT, RENAME, RMDIR, STAT, TRUNCATE, UNLINK, WRITE,
    WRITE_AT,
};
use libsys::image;
use libsys::store::Store;
use libsys::wire::{EXISTS, FAILED, IS_DIR, NOT_DIR, NOT_EMPTY, NOT_FOUND, OK, decode, encode, text};
use libsys::{Channel, Error, Handle, Port, ns, signal};
use objstore::{Id, Kind, Objects, Tree};

libsys::entry!(main);

/// dock → overlay, first: [CONFIG, key, image].
pub const CONFIG: u8 = 21;

/// Objects fetched from the store, kept for the overlay's lifetime.
struct Cache {
    store: Store,
    objects: RefCell<BTreeMap<Id, Box<[u8]>>>,
}

impl Objects for Cache {
    fn get(&self, id: &Id) -> Option<&[u8]> {
        if let Some(b) = self.objects.borrow().get(id) {
            // Boxes are never removed or moved: the bytes live as long as self.
            return Some(unsafe { &*(&**b as *const [u8]) });
        }
        let data = self.store.object(&format!("{id}")).ok()?.into_boxed_slice();
        let p = &*data as *const [u8];
        self.objects.borrow_mut().insert(*id, data);
        Some(unsafe { &*p })
    }
}

struct Overlay {
    cache: Cache,
    key: String,
    image: String,
    layers: Vec<Id>,
}

/// `/a//b/./c/..` → `/a/b`.
fn norm(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for x in p.split('/') {
        match x {
            "" | "." => {}
            ".." => _ = parts.pop(),
            x => parts.push(x),
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

fn child(dir: &str, name: &str) -> String {
    if dir == "/" { format!("/{name}") } else { format!("{dir}/{name}") }
}

impl Overlay {
    fn store(&self) -> &Store {
        &self.cache.store
    }

    fn uppers(&self) -> Vec<(String, Option<u64>)> {
        self.store().up_list(&self.key).unwrap_or_default()
    }

    /// Is `path` (or a directory above it) deleted in the writable layer?
    fn whited_out(&self, ups: &[(String, Option<u64>)], path: &str) -> bool {
        ups.iter().any(|(p, v)| v.is_none() && (p == path || path.starts_with(&format!("{p}/"))))
    }

    /// Where a file is: (writable layer?, size, object id in an image layer).
    fn find(&self, path: &str) -> Result<(bool, u64, Option<Id>), u8> {
        match self.store().up_stat(&self.key, path) {
            Ok(Some(Some(size))) => return Ok((true, size, None)),
            Ok(Some(None)) => return Err(NOT_FOUND),
            Ok(None) => {}
            Err(_) => return Err(FAILED),
        }
        if self.whited_out(&self.uppers(), path) {
            return Err(NOT_FOUND);
        }
        match self.cache.lookup(&self.layers, path) {
            Ok(e) if e.kind == Kind::File || e.kind == Kind::Exec => Ok((false, e.size, Some(e.id))),
            Ok(_) => Err(FAILED),
            Err(_) => Err(NOT_FOUND),
        }
    }

    fn read_at(&self, path: &str, off: u64, len: usize) -> Result<Vec<u8>, u8> {
        let len = len.min(libsys::store::MAX_INLINE);
        match self.find(path)? {
            (true, ..) => match self.store().up_read_at(&self.key, path, off, len) {
                Ok(Some(Some(d))) => Ok(d),
                _ => Err(FAILED),
            },
            (false, _, Some(id)) => self.store().object_at(&format!("{id}"), off, len).map_err(|_| FAILED),
            _ => Err(FAILED),
        }
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, u8> {
        let (_, size, _) = self.find(path)?;
        if size as usize > libsys::store::MAX_INLINE {
            return Err(FAILED);
        }
        self.read_at(path, 0, size as usize)
    }

    fn stat(&self, path: &str) -> Result<(bool, u64), u8> {
        if path.ends_with(DIR_MARKER) {
            return Err(NOT_FOUND);
        }
        match self.find(path) {
            Ok((_, size, _)) => Ok((false, size)),
            Err(NOT_FOUND | FAILED) if self.is_dir(path) => Ok((true, 0)),
            Err(e) => Err(e),
        }
    }

    fn is_dir(&self, path: &str) -> bool {
        path == "/"
            || self.find(&child(path, DIR_MARKER)).is_ok()
            || matches!(self.cache.lookup(&self.layers, path), Ok(e) if e.kind == Kind::Dir)
                && !self.whited_out(&self.uppers(), path)
            || !self.list(path).unwrap_or_default().is_empty()
    }

    /// A file only in an image layer is copied into the writable layer
    /// before it is changed in place.
    fn copy_up(&self, path: &str) -> Result<(), u8> {
        let (upper, size, _) = self.find(path)?;
        if upper {
            return Ok(());
        }
        let first = self.read_at(path, 0, size.min(libsys::store::MAX_INLINE as u64) as usize)?;
        self.store().up_write(&self.key, path, &first).map_err(|_| FAILED)?;
        let mut off = first.len() as u64;
        while off < size {
            let part = self.read_at(path, off, libsys::store::MAX_INLINE)?;
            if part.is_empty() {
                break;
            }
            self.store().up_write_at(&self.key, path, off, &part).map_err(|_| FAILED)?;
            off += part.len() as u64;
        }
        Ok(())
    }

    /// Whole-file writes make missing directories (WASI programs check
    /// for them first); only a directory itself cannot be written.
    fn not_dir(&self, path: &str) -> Result<(), u8> {
        match self.stat(path) {
            Ok((true, _)) => Err(IS_DIR),
            _ => Ok(()),
        }
    }

    /// A new (empty) file needs its directory.
    fn create_ok(&self, path: &str) -> Result<(), u8> {
        match self.stat(path) {
            Ok((true, _)) => Err(IS_DIR),
            Ok(_) => Ok(()),
            Err(_) => match self.stat(parent(path)) {
                Ok((true, _)) => Ok(()),
                Ok(_) => Err(NOT_DIR),
                Err(_) => Err(NOT_FOUND),
            },
        }
    }

    fn mkdir(&self, path: &str) -> Result<(), u8> {
        if self.stat(path).is_ok() {
            return Err(EXISTS);
        }
        match self.stat(parent(path)) {
            Ok((true, _)) => {}
            Ok(_) => return Err(NOT_DIR),
            Err(_) => return Err(NOT_FOUND),
        }
        self.store().up_write(&self.key, &child(path, DIR_MARKER), b"").map_err(|_| FAILED)
    }

    fn rmdir(&self, path: &str) -> Result<(), u8> {
        match self.stat(path)? {
            (true, _) if path != "/" => {}
            (true, _) => return Err(FAILED),
            _ => return Err(NOT_DIR),
        }
        if !self.list(path)?.is_empty() {
            return Err(NOT_EMPTY);
        }
        let m = child(path, DIR_MARKER);
        if self.find(&m).is_ok() {
            self.store().up_delete(&self.key, &m).map_err(|_| FAILED)?;
        }
        self.store().up_delete(&self.key, path).map_err(|_| FAILED)
    }

    fn unlink(&self, path: &str) -> Result<(), u8> {
        match self.stat(path)? {
            (true, _) => Err(IS_DIR),
            _ => self.store().up_delete(&self.key, path).map_err(|_| FAILED),
        }
    }

    fn truncate(&self, path: &str, size: u64) -> Result<(), u8> {
        let (dir, old) = self.stat(path)?;
        if dir {
            return Err(IS_DIR);
        }
        if size == 0 {
            return self.store().up_write(&self.key, path, b"").map_err(|_| FAILED);
        }
        self.copy_up(path)?;
        if size > old {
            return self.store().up_write_at(&self.key, path, size - 1, &[0]).map_err(|_| FAILED);
        }
        // Shrinking: rewrite the part that stays.
        let mut keep = Vec::with_capacity(size as usize);
        while (keep.len() as u64) < size {
            let n = (size as usize - keep.len()).min(libsys::store::MAX_INLINE);
            let part = self.read_at(path, keep.len() as u64, n)?;
            if part.is_empty() {
                break;
            }
            keep.extend_from_slice(&part);
        }
        self.write_all(path, &keep)
    }

    fn write_all(&self, path: &str, data: &[u8]) -> Result<(), u8> {
        let max = libsys::store::MAX_INLINE;
        self.store().up_write(&self.key, path, &data[..data.len().min(max)]).map_err(|_| FAILED)?;
        let mut off = max;
        while off < data.len() {
            let end = (off + max).min(data.len());
            self.store().up_write_at(&self.key, path, off as u64, &data[off..end]).map_err(|_| FAILED)?;
            off = end;
        }
        Ok(())
    }

    /// Copies a file (chunk by chunk) or a directory tree, then removes the source.
    fn rename(&self, from: &str, to: &str) -> Result<(), u8> {
        if from == to {
            return self.stat(from).map(|_| ());
        }
        if to.starts_with(&format!("{from}/")) || from == "/" {
            return Err(FAILED);
        }
        let (dir, size) = self.stat(from)?;
        match (dir, self.stat(to)) {
            (true, Ok((false, _))) => return Err(NOT_DIR),
            (false, Ok((true, _))) => return Err(IS_DIR),
            (true, Ok((true, _))) if !self.list(to)?.is_empty() => return Err(NOT_EMPTY),
            (true, Ok((true, _))) => self.rmdir(to)?,
            (_, Err(_)) => _ = self.create_ok(to)?,
            _ => {}
        }
        if dir {
            self.mkdir(to)?;
            for (name, _) in self.list(from)? {
                self.rename(&child(from, &name), &child(to, &name))?;
            }
            return self.rmdir(from);
        }
        let first = self.read_at(from, 0, size.min(libsys::store::MAX_INLINE as u64) as usize)?;
        self.store().up_write(&self.key, to, &first).map_err(|_| FAILED)?;
        let mut off = first.len() as u64;
        while off < size {
            let part = self.read_at(from, off, libsys::store::MAX_INLINE)?;
            if part.is_empty() {
                break;
            }
            self.store().up_write_at(&self.key, to, off, &part).map_err(|_| FAILED)?;
            off += part.len() as u64;
        }
        self.store().up_delete(&self.key, from).map_err(|_| FAILED)
    }

    fn exists(&self, path: &str) -> bool {
        self.stat(path).is_ok()
    }

    /// name → (is dir, size).
    fn list(&self, dir: &str) -> Result<BTreeMap<String, (bool, u64)>, u8> {
        let mut out = BTreeMap::new();
        let ups = self.uppers();
        let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
        if !self.whited_out(&ups, dir) {
            // Lower layers, bottom first: upper ones replace and hide.
            for l in &self.layers {
                let Some(mut t) = self.cache.get(l).and_then(|b| Tree::decode(b).ok()) else { continue };
                let mut ok = true;
                for p in &parts {
                    match t.get(p) {
                        Some(e) if e.kind == Kind::Dir => {
                            t = self.cache.get(&e.id).and_then(|b| Tree::decode(b).ok()).unwrap_or_default();
                        }
                        Some(e) if e.kind == Kind::Whiteout => {
                            out.clear();
                            ok = false;
                            break;
                        }
                        _ => {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    continue;
                }
                for e in t.entries {
                    match e.kind {
                        Kind::Whiteout => _ = out.remove(&e.name),
                        k => _ = out.insert(e.name, (k == Kind::Dir, e.size)),
                    }
                }
            }
        }
        let prefix = if dir == "/" { String::from("/") } else { format!("{dir}/") };
        // Whiteouts hide the image layers only: files written since (also
        // under a removed and recreated directory) show.
        let mut ups = ups;
        ups.sort_by_key(|(_, v)| v.is_some());
        for (p, v) in &ups {
            let Some(rest) = p.strip_prefix(&prefix) else { continue };
            let (name, deeper) = match rest.split_once('/') {
                Some((n, _)) => (n, true),
                None => (rest, false),
            };
            match (deeper, v) {
                (false, None) => _ = out.remove(name),
                (false, Some(size)) => _ = out.insert(String::from(name), (false, *size)),
                (true, Some(_)) => _ = out.entry(String::from(name)).or_insert((true, 0)),
                (true, None) => {}
            }
        }
        out.remove(DIR_MARKER);
        Ok(out)
    }

    /// The writable layer becomes a new layer on top of every
    /// architecture's manifest of the image, saved as `name`.
    fn commit(&self, name: &str) -> Result<String, Error> {
        let (mut files, mut removed) = (0, 0);
        let mut layer = Vec::new();
        for (p, v) in self.uppers() {
            let e = match v {
                Some(size) => {
                    // The file goes to the store as a memory object, in chunks.
                    let m = libsys::Memory::create(size.max(1))?;
                    let mut off = 0u64;
                    while off < size {
                        let n = ((size - off) as usize).min(libsys::store::MAX_INLINE);
                        let part = self.store().up_read_at(&self.key, &p, off, n)?.flatten().ok_or(Error::NotFound)?;
                        if part.is_empty() {
                            break;
                        }
                        m.write(off, &part)?;
                        off += part.len() as u64;
                    }
                    let id = Id::parse(&self.store().put_memory(m, size)?).ok_or(Error::InvalidArgs)?;
                    files += 1;
                    Some((id, size))
                }
                None => {
                    removed += 1;
                    None
                }
            };
            layer.push((p, e));
        }
        let l = image::layer(self.store(), &layer)?;
        let id = image::derive(self.store(), &self.image, name, l, None, &[])?;
        Ok(format!("{id} {files} {removed}"))
    }

    fn request(&self, msg: &[u8]) -> Vec<u8> {
        let ok = |b: &[u8]| encode(OK, &[b]);
        let Some(&op) = msg.first() else { return encode(FAILED, &[]) };
        match op {
            READ => match self.read(&norm(text(&decode(msg, 1).unwrap().1[0]))) {
                Ok(d) if d.len() <= libsys::store::MAX_INLINE => ok(&d),
                Ok(_) => encode(FAILED, &[b"too large"]),
                Err(e) => encode(e, &[]),
            },
            WRITE => {
                let f = decode(msg, 2).unwrap().1;
                let p = norm(text(&f[0]));
                if let Err(e) = self.not_dir(&p) {
                    return encode(e, &[]);
                }
                match self.store().up_write(&self.key, &p, &f[1]) {
                    Ok(()) => ok(b""),
                    Err(_) => encode(FAILED, &[]),
                }
            }
            STAT => match self.stat(&norm(text(&decode(msg, 1).unwrap().1[0]))) {
                Ok((dir, size)) => ok(format!("{} {}", if dir { "dir" } else { "file" }, size).as_bytes()),
                Err(e) => encode(e, &[]),
            },
            READ_AT => {
                let f = decode(msg, 3).unwrap().1;
                let (off, len) = (text(&f[1]).parse().unwrap_or(0), text(&f[2]).parse().unwrap_or(0));
                match self.read_at(&norm(text(&f[0])), off, len) {
                    Ok(d) => ok(&d),
                    Err(e) => encode(e, &[]),
                }
            }
            WRITE_AT => {
                let f = decode(msg, 3).unwrap().1;
                let off = text(&f[1]).parse().unwrap_or(0);
                let p = norm(text(&f[0]));
                if let Err(e) = self.copy_up(&p).or_else(|e| if e == NOT_FOUND { self.not_dir(&p) } else { Err(e) }) {
                    return encode(e, &[]);
                }
                match self.store().up_write_at(&self.key, &p, off, &f[2]) {
                    Ok(()) => ok(b""),
                    Err(_) => encode(FAILED, &[]),
                }
            }
            DELETE => {
                let p = norm(text(&decode(msg, 1).unwrap().1[0]));
                if !self.exists(&p) {
                    return encode(NOT_FOUND, &[]);
                }
                match self.store().up_delete(&self.key, &p) {
                    Ok(()) => ok(b""),
                    Err(_) => encode(FAILED, &[]),
                }
            }
            LIST => match self.list(&norm(text(&decode(msg, 1).unwrap().1[0]))) {
                Ok(m) => {
                    let mut s = String::new();
                    for (n, (dir, size)) in m {
                        s += &format!("{} {} {}\n", if dir { "dir" } else { "file" }, size, n);
                    }
                    ok(s.as_bytes())
                }
                Err(e) => encode(e, &[]),
            },
            MKDIR | RMDIR | UNLINK => {
                let p = norm(text(&decode(msg, 1).unwrap().1[0]));
                let r = match op {
                    MKDIR => self.mkdir(&p),
                    RMDIR => self.rmdir(&p),
                    _ => self.unlink(&p),
                };
                r.map_or_else(|e| encode(e, &[]), |_| ok(b""))
            }
            RENAME => {
                let f = decode(msg, 2).unwrap().1;
                self.rename(&norm(text(&f[0])), &norm(text(&f[1]))).map_or_else(|e| encode(e, &[]), |_| ok(b""))
            }
            TRUNCATE => {
                let f = decode(msg, 2).unwrap().1;
                let size = text(&f[1]).parse().unwrap_or(0);
                self.truncate(&norm(text(&f[0])), size).map_or_else(|e| encode(e, &[]), |_| ok(b""))
            }
            COMMIT => match self.commit(text(&decode(msg, 1).unwrap().1[0])) {
                Ok(r) => ok(r.as_bytes()),
                Err(e) => encode(FAILED, &[format!("{e:?}").as_bytes()]),
            },
            _ => encode(FAILED, &[b"unknown request"]),
        }
    }
}

fn main(_arg: usize) -> i64 {
    let (Some(store), Some(fs), Some(ctl)) = (ns::get("store"), ns::get("fs"), ns::get("control")) else { return 1 };
    let (fs, ctl) = (Channel(fs), Channel(ctl));
    let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES];
    let Ok((n, _)) = ctl.recv(&mut buf, &mut []) else { return 2 };
    let Some((CONFIG, f)) = decode(&buf[..n], 2) else { return 3 };
    let (key, image) = (String::from(text(&f[0])), String::from(text(&f[1])));
    let cache = Cache { store: Store(Channel(store)), objects: RefCell::new(BTreeMap::new()) };
    let layers = cache
        .store
        .index(&image)
        .ok()
        .and_then(|i| Id::parse(&i))
        .and_then(|i| {
            objstore::manifest_of(&cache, &i, libsys::ARCH).or_else(|_| objstore::manifest_of(&cache, &i, "wasm32")).ok()
        })
        .map(|m| m.layers)
        .unwrap_or_default();
    let ov = Overlay { cache, key, image, layers };
    let Ok(port) = Port::create() else { return 4 };
    // Clients: the pod's channel, more from CONNECT; dock's control (key 0).
    let mut chans: Vec<Option<Channel>> = alloc::vec![Some(ctl), Some(fs)];
    for (i, c) in chans.iter().enumerate() {
        let _ = port.bind(c.unwrap().0, signal::READABLE | signal::PEER_CLOSED, i as u64);
    }
    loop {
        let Ok(p) = port.wait(u64::MAX) else { return 5 };
        let i = p.key as usize;
        let Some(c) = chans[i] else { continue };
        loop {
            let mut hs = [Handle(0); 1];
            match c.read(&mut buf, &mut hs) {
                Ok((n, nh)) => {
                    if n == 1 && buf[0] == CONNECT && nh == 1 {
                        let k = chans.len();
                        chans.push(Some(Channel(hs[0])));
                        let _ = port.bind(hs[0], signal::READABLE | signal::PEER_CLOSED, k as u64);
                        continue;
                    }
                    for h in &hs[..nh] {
                        let _ = h.close();
                    }
                    if n > 0 && buf[0] == OPEN_MEM {
                        // An image file: the store's memory object, not a copy.
                        let p = norm(text(&buf[1..n]));
                        let r = match ov.find(&p) {
                            Ok((false, _, Some(id))) => ov.store().object_mem(&format!("{id}")).ok(),
                            _ => None,
                        };
                        let _ = match r {
                            Some((m, len)) => {
                                let mut msg = Vec::from(&[OK][..]);
                                msg.extend_from_slice(&len.to_le_bytes());
                                c.write(&msg, &[m.0])
                            }
                            None => c.write(&[FAILED], &[]),
                        };
                        continue;
                    }
                    let r = ov.request(&buf[..n].to_vec());
                    let _ = c.write(&r, &[]);
                }
                Err(Error::ShouldWait) => {
                    let _ = port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, i as u64);
                    break;
                }
                Err(_) => {
                    let _ = c.0.close();
                    chans[i] = None;
                    break;
                }
            }
        }
        // Clients may go; the overlay lives as long as dock wants it.
        if chans[0].is_none() {
            return 0;
        }
    }
}
