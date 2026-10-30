//! A pod's file system (the overlay service's protocol and client):
//! whole-file reads and writes over the merged view of the image layers
//! and the pod's writable layer.
use alloc::string::String;
use alloc::vec::Vec;

use crate::wire::{EXISTS, FAILED, IS_DIR, NOT_DIR, NOT_EMPTY, NOT_FOUND, OK, encode, text};
use crate::{Channel, Error, Result};

/// Marks an explicit (possibly empty) directory in a layer; never listed.
pub const DIR_MARKER: &str = ".ruos-dir";

/// [READ, path] → [OK, bytes] / [NOT_FOUND].
pub const READ: u8 = 1;
/// [WRITE, path, bytes] → [OK].
pub const WRITE: u8 = 2;
/// [DELETE, path] → [OK] / [NOT_FOUND].
pub const DELETE: u8 = 3;
/// [LIST, dir] → [OK, `file|dir SIZE NAME` lines] / [NOT_FOUND].
pub const LIST: u8 = 4;
/// [STAT, path] → [OK, `file SIZE` / `dir 0`] / [NOT_FOUND].
pub const STAT: u8 = 5;
/// [READ_AT, path, offset, len] → [OK, bytes] (numbers in decimal).
pub const READ_AT: u8 = 6;
/// [WRITE_AT, path, offset, bytes] → [OK] (the file grows as needed).
pub const WRITE_AT: u8 = 7;
/// [MKDIR, path] → [OK] / [EXISTS] / [NOT_FOUND] (no parent).
pub const MKDIR: u8 = 8;
/// [RMDIR, path] → [OK] / [NOT_EMPTY] / [NOT_DIR] / [NOT_FOUND].
pub const RMDIR: u8 = 9;
/// [RENAME, from, to] → [OK] (a file or a whole directory; replaces a file at `to`).
pub const RENAME: u8 = 10;
/// [TRUNCATE, path, size] → [OK] (shrinks or zero-extends).
pub const TRUNCATE: u8 = 11;
/// [CONNECT] + a channel handle: serve another client on it (no reply).
pub const CONNECT: u8 = 12;
/// [UNLINK, path] → [OK] / [IS_DIR] / [NOT_FOUND]: removes a file.
pub const UNLINK: u8 = 13;
/// [OPEN_MEM, path] → [OK, length (u64 LE)] + a copy-on-write memory
/// object with the file (files of the image layers; others: [FAILED]).
pub const OPEN_MEM: u8 = 14;
/// Control, first message: [CONFIG, key, image] (the writable layer and the image).
pub const CONFIG: u8 = 21;
/// Control (dock → overlay): [COMMIT, name:tag] → [OK, `ID FILES REMOVED`].
pub const COMMIT: u8 = 20;

/// Bytes carried by one request.
pub const CHUNK: usize = 60000;

#[derive(Clone, Copy)]
pub struct Fs(pub Channel);

/// Starts an overlay of `image` whose writable layer is `key`: the file
/// system and the overlay's control channel (COMMIT; the overlay ends
/// when it closes).
pub fn mount(store: &crate::store::Store, key: &str, image: &str) -> Result<(Fs, Channel)> {
    use crate::ns::Namespace;
    let (fs, fs_srv) = Channel::create()?;
    let (ctl, ctl_srv) = Channel::create()?;
    ctl.write(&encode(CONFIG, &[key.as_bytes(), image.as_bytes()]), &[])?;
    let st = store.connect()?;
    let ns = Namespace::new().add("store", st.0.0).add("fs", fs_srv.0).add("control", ctl_srv.0);
    store.spawn("overlay", &ns, None)?.close()?;
    Ok((Fs(fs), ctl))
}

/// A directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

impl Fs {
    pub(crate) fn call(&self, op: u8, fields: &[&[u8]]) -> Result<Vec<u8>> {
        self.0.write(&encode(op, fields), &[])?;
        let mut buf = alloc::vec![0u8; abi::MAX_MSG_BYTES];
        let (n, _) = self.0.recv(&mut buf, &mut [])?;
        buf.truncate(n);
        match buf.first() {
            Some(&OK) => {
                buf.remove(0);
                Ok(buf)
            }
            Some(&NOT_FOUND) => Err(Error::NotFound),
            Some(&FAILED) => Err(Error::Fault),
            Some(&EXISTS) => Err(Error::Exists),
            Some(&NOT_EMPTY) => Err(Error::NotEmpty),
            Some(&NOT_DIR) => Err(Error::NotDir),
            Some(&IS_DIR) => Err(Error::IsDir),
            _ => Err(Error::InvalidArgs),
        }
    }

    /// The whole file, read a chunk at a time.
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let (dir, size) = self.stat(path)?;
        if dir {
            return Err(Error::InvalidArgs);
        }
        let mut out = Vec::with_capacity(size as usize);
        while (out.len() as u64) < size {
            let n = (size as usize - out.len()).min(CHUNK);
            let part = self.read_at(path, out.len() as u64, n)?;
            if part.is_empty() {
                break;
            }
            out.extend_from_slice(&part);
        }
        Ok(out)
    }

    /// Replaces the file, a chunk at a time.
    pub fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        let first = &data[..data.len().min(CHUNK)];
        self.call(WRITE, &[path.as_bytes(), first]).map(|_| ())?;
        let mut off = first.len();
        while off < data.len() {
            let n = (data.len() - off).min(CHUNK);
            self.write_at(path, off as u64, &data[off..off + n])?;
            off += n;
        }
        Ok(())
    }

    /// (is a directory, size).
    pub fn stat(&self, path: &str) -> Result<(bool, u64)> {
        let r = self.call(STAT, &[path.as_bytes()])?;
        let t = text(&r);
        let (kind, size) = t.split_once(' ').ok_or(Error::InvalidArgs)?;
        Ok((kind == "dir", size.parse().map_err(|_| Error::InvalidArgs)?))
    }

    pub fn read_at(&self, path: &str, off: u64, len: usize) -> Result<Vec<u8>> {
        let (o, l) = (alloc::format!("{off}"), alloc::format!("{len}"));
        self.call(READ_AT, &[path.as_bytes(), o.as_bytes(), l.as_bytes()])
    }

    pub fn write_at(&self, path: &str, off: u64, data: &[u8]) -> Result<()> {
        let o = alloc::format!("{off}");
        self.call(WRITE_AT, &[path.as_bytes(), o.as_bytes(), data]).map(|_| ())
    }

    pub fn delete(&self, path: &str) -> Result<()> {
        self.call(DELETE, &[path.as_bytes()]).map(|_| ())
    }

    /// Removes a file (not a directory).
    pub fn unlink(&self, path: &str) -> Result<()> {
        self.call(UNLINK, &[path.as_bytes()]).map(|_| ())
    }

    pub fn mkdir(&self, path: &str) -> Result<()> {
        self.call(MKDIR, &[path.as_bytes()]).map(|_| ())
    }

    /// Removes an empty directory.
    pub fn rmdir(&self, path: &str) -> Result<()> {
        self.call(RMDIR, &[path.as_bytes()]).map(|_| ())
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.call(RENAME, &[from.as_bytes(), to.as_bytes()]).map(|_| ())
    }

    pub fn truncate(&self, path: &str, size: u64) -> Result<()> {
        let n = alloc::format!("{size}");
        self.call(TRUNCATE, &[path.as_bytes(), n.as_bytes()]).map(|_| ())
    }

    /// A whole file in a memory object: (memory, length). Image files
    /// come from the store without copying; others are read in chunks.
    pub fn read_mem(&self, path: &str) -> Result<(crate::Memory, u64)> {
        self.0.write(&encode(OPEN_MEM, &[path.as_bytes()]), &[])?;
        let mut buf = [0u8; 16];
        let mut hs = [crate::Handle(0); 1];
        let (n, nh) = self.0.recv(&mut buf, &mut hs)?;
        if n >= 9 && buf[0] == OK && nh == 1 {
            return Ok((crate::Memory(hs[0]), u64::from_le_bytes(buf[1..9].try_into().unwrap())));
        }
        let (dir, size) = self.stat(path)?;
        if dir {
            return Err(Error::IsDir);
        }
        let m = crate::Memory::create(size.max(1))?;
        let mut off = 0;
        while off < size {
            let part = self.read_at(path, off, CHUNK)?;
            if part.is_empty() {
                break;
            }
            m.write(off, &part)?;
            off += part.len() as u64;
        }
        Ok((m, size))
    }

    /// Another client channel on the same file system.
    pub fn connect(&self) -> Result<Fs> {
        let (a, b) = Channel::create()?;
        self.0.write(&[CONNECT], &[b.0])?;
        Ok(Fs(a))
    }

    pub fn list(&self, dir: &str) -> Result<Vec<DirEntry>> {
        let r = self.call(LIST, &[dir.as_bytes()])?;
        Ok(text(&r)
            .lines()
            .filter_map(|l| {
                let mut w = l.splitn(3, ' ');
                let (k, size, name) = (w.next()?, w.next()?, w.next()?);
                Some(DirEntry { name: String::from(name), dir: k == "dir", size: size.parse().ok()? })
            })
            .collect())
    }

    /// Makes the pod's changes an image: (index id, files, whiteouts).
    pub fn commit(&self, name: &str) -> Result<String> {
        self.call(COMMIT, &[name.as_bytes()]).map(|r| String::from(text(&r)))
    }
}
