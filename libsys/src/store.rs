//! The store service's protocol and client (`wire` encoding). The store
//! holds objects and refs, and the pods' writable layers ("uppers": a path
//! → file or whiteout map per key). On wasm32 the uppers live on the
//! server (podsrv) and objects can be pushed there; the requests the store
//! forwards are these same messages.
use alloc::string::String;
use alloc::vec::Vec;

use crate::ns::Namespace;
use crate::wire::{encode, text};
use crate::{Channel, Error, Handle, Memory, Result};

pub use crate::wire::{FAILED, NOT_FOUND, OK};

/// [GET, name] → [OK, image length (u64 LE), entry path] + the program in a
/// memory object; or [NOT_FOUND].
pub const GET: u8 = 1;
/// [LIST] → [OK, `name:tag id arches` lines].
pub const LIST: u8 = 2;
/// [CONNECT] + one channel handle: serve another client on it.
pub const CONNECT: u8 = 3;
/// [OBJ_GET, id] → [OK, bytes].
pub const OBJ_GET: u8 = 4;
/// [OBJ_PUT, bytes] → [OK, id].
pub const OBJ_PUT: u8 = 5;
/// [REF_SET, name:tag, id] → [OK].
pub const REF_SET: u8 = 6;
/// [UP_WRITE, key, path, bytes] → [OK].
pub const UP_WRITE: u8 = 7;
/// [UP_READ, key, path] → [OK, 'f' + bytes | 'w'] or [NOT_FOUND].
pub const UP_READ: u8 = 8;
/// [UP_LIST, key] → [OK, `f SIZE PATH` / `w 0 PATH` lines].
pub const UP_LIST: u8 = 9;
/// [UP_DELETE, key, path] → [OK] (a whiteout).
pub const UP_DELETE: u8 = 10;
/// [PUSH, name:tag] → [OK, `N objects`] or [FAILED, why].
pub const PUSH: u8 = 11;
/// [INDEX, name] → [OK, index id] or [NOT_FOUND].
pub const INDEX: u8 = 13;
/// To the server only (wasm32's disk): [DISK_READ, sector u64, count u32]
/// → [OK, data]; [DISK_WRITE, sector u64, data] → [OK]; [DISK_INFO] → [OK, sectors u64].
pub const DISK_READ: u8 = 14;
pub const DISK_WRITE: u8 = 15;
pub const DISK_INFO: u8 = 16;
/// [BOOT] → [OK, this boot's number (text)]: counts boots of a persistent store.
pub const BOOT: u8 = 17;
/// Parts of large files and objects (numbers are decimal text):
/// [UP_READ_AT, key, path, offset, len] → like UP_READ, but `len` bytes at `offset`.
pub const UP_READ_AT: u8 = 20;
/// [UP_WRITE_AT, key, path, offset, bytes] → [OK] (the file grows as needed).
pub const UP_WRITE_AT: u8 = 21;
/// [UP_STAT, key, path] → [OK, 'f' + size] / [OK, 'w'] / [NOT_FOUND].
pub const UP_STAT: u8 = 22;
/// [OBJ_READ_AT, id, offset, len] → [OK, bytes].
pub const OBJ_READ_AT: u8 = 23;
/// [OBJ_PUT_MEM, len] + a memory object holding it → [OK, id].
pub const OBJ_PUT_MEM: u8 = 24;
/// To the server only: [OBJ_PUT_PART, id, offset, total, bytes] → [OK];
/// the object is checked and kept once all its bytes are there.
pub const OBJ_PUT_PART: u8 = 25;
/// [OBJ_MEM, id] → [OK, length (u64 LE)] + a copy-on-write memory object
/// holding the object (the store keeps one per object it was asked for).
pub const OBJ_MEM: u8 = 26;

/// Largest object or file carried in one message.
pub const MAX_INLINE: usize = 60000;
/// The cluster's records on the server (kube), forwarded as they are:
/// [KV_HELLO] → [OK, `MACHINE SESSION NOW_MS FN_BASE`] (FN_BASE: `http://ADDR:PORT` of
/// the function port, or `-`);
pub const KV_HELLO: u8 = 40;
/// [KV_LIST, prefix] → [OK, `NOW_MS\n` + per record `KEY VERSION UPDATED_MS LEN\n` + value + `\n`]
/// (an empty prefix leaves out `data/…`, the KV namespaces);
pub const KV_LIST: u8 = 41;
/// [KV_PUT, key, expect, value] → [OK, `VERSION`] or [FAILED, `conflict`]
/// (expect: empty = any, 0 = must not exist, else the current version);
pub const KV_PUT: u8 = 42;
/// [KV_DEL, key, expect] → [OK] / [NOT_FOUND] / [FAILED, `conflict`].
pub const KV_DEL: u8 = 43;
/// HTTP made by the server (libsys::http), only for clients that came
/// from the store's `web` channel (the `http` capability):
/// [HTTP_FETCH, method, url, headers, body] → [OK, `STATUS LEN\n` + header lines] / [FAILED, why];
pub const HTTP_FETCH: u8 = 44;
/// [HTTP_READ, offset] → [OK, up to 60000 bytes of that body].
pub const HTTP_READ: u8 = 45;
/// The backend's databases (`sql:NAME`, `redis:NAME`; ruos serve's backend.rs):
/// [SQL, name, sql, params (a JSON array), format (`json` | `text`)] → [OK, the answer] / [FAILED, why];
pub const SQL: u8 = 46;
/// [REDIS, name, args (a JSON array of strings), format] → [OK, the answer] / [FAILED, why].
pub const REDIS: u8 = 47;
/// [GRANT, names, one a line] + a channel end: a new client that may use
/// those of the backend (`http`, `sql:NAME`, `redis:NAME`; `*` = all) that
/// this one may.
pub const GRANT: u8 = 48;

#[derive(Clone, Copy)]
pub struct Store(pub Channel);

impl Store {
    fn request(&self, req: &[u8], handles: &mut [Handle]) -> Result<(Vec<u8>, usize)> {
        self.0.write(req, &[])?;
        let mut buf = alloc::vec![0u8; abi::MAX_MSG_BYTES];
        let (n, nh) = self.0.recv(&mut buf, handles)?;
        buf.truncate(n);
        Ok((buf, nh))
    }

    /// The payload of an OK reply.
    fn call(&self, op: u8, fields: &[&[u8]]) -> Result<Vec<u8>> {
        let (mut r, _) = self.request(&encode(op, fields), &mut [])?;
        match r.first() {
            Some(&OK) => {
                r.remove(0);
                Ok(r)
            }
            Some(&NOT_FOUND) => Err(Error::NotFound),
            _ => Err(Error::InvalidArgs),
        }
    }

    pub fn object(&self, id: &str) -> Result<Vec<u8>> {
        self.call(OBJ_GET, &[id.as_bytes()])
    }

    /// Stores an object; returns its id.
    pub fn put(&self, data: &[u8]) -> Result<String> {
        if data.len() > MAX_INLINE {
            return Err(Error::InvalidArgs);
        }
        self.call(OBJ_PUT, &[data]).map(|r| String::from(text(&r)))
    }

    pub fn set_ref(&self, name: &str, id: &str) -> Result<()> {
        self.call(REF_SET, &[name.as_bytes(), id.as_bytes()]).map(|_| ())
    }

    /// How many times this store has booted (0 without a disk).
    /// A request whose answer is (status, payload): KV_* (FAILED with `no
    /// server` when the machine has no server), HTTP_*.
    pub fn raw(&self, op: u8, fields: &[&[u8]]) -> Result<(u8, Vec<u8>)> {
        let (mut r, _) = self.request(&encode(op, fields), &mut [])?;
        if r.is_empty() {
            return Err(Error::InvalidArgs);
        }
        let st = r.remove(0);
        Ok((st, r))
    }

    pub fn boot(&self) -> Result<u64> {
        self.call(BOOT, &[]).map(|r| text(&r).parse().unwrap_or(0))
    }

    /// The index id of image `name`.
    pub fn index(&self, name: &str) -> Result<String> {
        self.call(INDEX, &[name.as_bytes()]).map(|r| String::from(text(&r)))
    }

    /// `len` bytes at `offset` of a writable layer's file (None: not there;
    /// Some(None): a whiteout).
    pub fn up_read_at(&self, key: &str, path: &str, off: u64, len: usize) -> Result<Option<Option<Vec<u8>>>> {
        let (o, l) = (alloc::format!("{off}"), alloc::format!("{len}"));
        match self.call(UP_READ_AT, &[key.as_bytes(), path.as_bytes(), o.as_bytes(), l.as_bytes()]) {
            Ok(r) if r.first() == Some(&b'f') => Ok(Some(Some(r[1..].to_vec()))),
            Ok(_) => Ok(Some(None)),
            Err(Error::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn up_write_at(&self, key: &str, path: &str, off: u64, data: &[u8]) -> Result<()> {
        let o = alloc::format!("{off}");
        self.call(UP_WRITE_AT, &[key.as_bytes(), path.as_bytes(), o.as_bytes(), data]).map(|_| ())
    }

    /// Some(Some(size)): a file; Some(None): a whiteout; None: not there.
    pub fn up_stat(&self, key: &str, path: &str) -> Result<Option<Option<u64>>> {
        match self.call(UP_STAT, &[key.as_bytes(), path.as_bytes()]) {
            Ok(r) if r.first() == Some(&b'f') => Ok(Some(text(&r[1..]).parse().ok())),
            Ok(_) => Ok(Some(None)),
            Err(Error::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn object_at(&self, id: &str, off: u64, len: usize) -> Result<Vec<u8>> {
        let (o, l) = (alloc::format!("{off}"), alloc::format!("{len}"));
        self.call(OBJ_READ_AT, &[id.as_bytes(), o.as_bytes(), l.as_bytes()])
    }

    /// Stores the first `len` bytes of `m` as an object; returns its id.
    pub fn put_memory(&self, m: Memory, len: u64) -> Result<String> {
        let l = alloc::format!("{len}");
        self.0.write(&encode(OBJ_PUT_MEM, &[l.as_bytes()]), &[m.0])?;
        let mut buf = alloc::vec![0u8; 256];
        let (n, _) = self.0.recv(&mut buf, &mut [])?;
        if buf.first() != Some(&OK) {
            return Err(Error::InvalidArgs);
        }
        Ok(String::from(text(&buf[1..n])))
    }

    pub fn up_write(&self, key: &str, path: &str, data: &[u8]) -> Result<()> {
        self.call(UP_WRITE, &[key.as_bytes(), path.as_bytes(), data]).map(|_| ())
    }

    /// Some(Some(bytes)): a file; Some(None): a whiteout; None: not there.
    pub fn up_read(&self, key: &str, path: &str) -> Result<Option<Option<Vec<u8>>>> {
        match self.call(UP_READ, &[key.as_bytes(), path.as_bytes()]) {
            Ok(r) if r.first() == Some(&b'f') => Ok(Some(Some(r[1..].to_vec()))),
            Ok(_) => Ok(Some(None)),
            Err(Error::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn up_delete(&self, key: &str, path: &str) -> Result<()> {
        self.call(UP_DELETE, &[key.as_bytes(), path.as_bytes()]).map(|_| ())
    }

    /// (path, Some(size) for a file / None for a whiteout).
    pub fn up_list(&self, key: &str) -> Result<Vec<(String, Option<u64>)>> {
        let r = self.call(UP_LIST, &[key.as_bytes()])?;
        Ok(text(&r)
            .lines()
            .filter_map(|l| {
                let mut w = l.splitn(3, ' ');
                let (k, size, path) = (w.next()?, w.next()?, w.next()?);
                Some((String::from(path), if k == "f" { size.parse().ok() } else { None }))
            })
            .collect())
    }

    /// Sends image `name` to the registry: Ok(what was sent) or Err(why not).
    pub fn push(&self, name: &str) -> Result<core::result::Result<String, String>> {
        let (r, _) = self.request(&encode(PUSH, &[name.as_bytes()]), &mut [])?;
        let msg = String::from(text(r.get(1..).unwrap_or(&[])));
        Ok(if r.first() == Some(&OK) { Ok(msg) } else { Err(msg) })
    }

    /// The program of image `name` for this architecture: (memory, length).
    pub fn program(&self, name: &str) -> Result<(Memory, u64)> {
        let req = encode(GET, &[name.as_bytes()]);
        let mut hs = [Handle(0); 1];
        let (r, nh) = self.request(&req, &mut hs)?;
        match r.first() {
            Some(&OK) if nh == 1 && r.len() >= 9 => {
                Ok((Memory(hs[0]), u64::from_le_bytes(r[1..9].try_into().unwrap())))
            }
            Some(&NOT_FOUND) => Err(Error::NotFound),
            _ => Err(Error::InvalidArgs),
        }
    }

    /// An object as a (copy-on-write) memory object: (memory, length).
    pub fn object_mem(&self, id: &str) -> Result<(Memory, u64)> {
        let mut hs = [Handle(0); 1];
        let (r, nh) = self.request(&encode(OBJ_MEM, &[id.as_bytes()]), &mut hs)?;
        match r.first() {
            Some(&OK) if nh == 1 && r.len() >= 9 => Ok((Memory(hs[0]), u64::from_le_bytes(r[1..9].try_into().unwrap()))),
            Some(&NOT_FOUND) => Err(Error::NotFound),
            _ => Err(Error::InvalidArgs),
        }
    }

    /// Starts image `name` in namespace `ns`, charged to `quota` (None:
    /// the caller's).
    pub fn spawn(&self, name: &str, ns: &Namespace, quota: Option<Handle>) -> Result<Handle> {
        let (m, len) = self.program(name)?;
        let r = self.spawn_module(m, len, name, ns, quota);
        let _ = m.0.close();
        r
    }

    /// Starts a program image held in `m` (`len` bytes; the caller keeps `m`).
    pub fn spawn_module(&self, m: Memory, len: u64, name: &str, ns: &Namespace, quota: Option<Handle>) -> Result<Handle> {
        ns.spawn(m, len, name, quota)
    }

    /// `name:tag id arches` lines.
    pub fn list(&self) -> Result<Vec<u8>> {
        self.call(LIST, &[])
    }

    /// A new client channel for someone else.
    pub fn connect(&self) -> Result<Store> {
        let (a, b) = Channel::create()?;
        self.0.write(&[CONNECT], &[b.0])?;
        Ok(Store(a))
    }

    /// A client that may use only `names` of the backend (and only those
    /// this one may): the capability to hand to a program.
    pub fn grant(&self, names: &[&str]) -> Result<Store> {
        let (a, b) = Channel::create()?;
        let mut m = alloc::vec![GRANT];
        m.extend_from_slice(names.join("\n").as_bytes());
        self.0.write(&m, &[b.0])?;
        Ok(Store(a))
    }
}
