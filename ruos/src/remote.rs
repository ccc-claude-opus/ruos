//! The server end of the kernel's remote channel (wasm32). Each WebSocket
//! message is a store request in the store's own format (libsys::wire):
//! - OBJ_PUT / REF_SET: `dock push` puts images into the registry;
//! - UP_WRITE / UP_READ / UP_LIST / UP_DELETE: the pods' writable layers,
//!   kept on disk in `STORE/pods/SESSION/KEY/` (`files/` + `whiteouts`);
//!   and their parts: UP_READ_AT / UP_WRITE_AT / UP_STAT;
//! - OBJ_PUT_PART: a large object in parts (`objects/.part-ID` until it is
//!   complete and its hash checked);
//! - INDEX / OBJ_READ_AT: machines pull images they do not have (a ref's
//!   index id, then every object in parts);
//! - DISK_INFO / DISK_READ / DISK_WRITE: the machine's disk (wasm32 has no
//!   block device), `STORE/disks/MACHINE.img`, 16 MiB;
//! - KV_HELLO / KV_LIST / KV_PUT / KV_DEL: the cluster's records (cluster.rs);
//! - HTTP_FETCH / HTTP_READ: an HTTP request made here (http.rs, for curl),
//!   its body kept for the session and read in parts;
//! - SQL / REDIS: the backend's databases (backend.rs).
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::cluster::{self, Put};
use crate::http;
use crate::id::Id;

const OK: u8 = 0;
const NOT_FOUND: u8 = 1;
const FAILED: u8 = 2;
const OBJ_PUT: u8 = 5;
const REF_SET: u8 = 6;
const UP_WRITE: u8 = 7;
const UP_READ: u8 = 8;
const UP_LIST: u8 = 9;
const UP_DELETE: u8 = 10;
const UP_READ_AT: u8 = 20;
const UP_WRITE_AT: u8 = 21;
const UP_STAT: u8 = 22;
const INDEX: u8 = 13;
const OBJ_READ_AT: u8 = 23;
const OBJ_PUT_PART: u8 = 25;
const DISK_READ: u8 = 14;
const DISK_WRITE: u8 = 15;
const DISK_INFO: u8 = 16;
const KV_HELLO: u8 = 40;
const KV_LIST: u8 = 41;
const KV_PUT: u8 = 42;
const KV_DEL: u8 = 43;
const HTTP_FETCH: u8 = 44;
const HTTP_READ: u8 = 45;
const PART: usize = 60000;
const OBJ_SIZE: u8 = 49;
const SQL: u8 = 46;
const REDIS: u8 = 47;
const SECTOR: u64 = 512;
const DISK_SIZE: u64 = 16 << 20;

fn reply(status: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![status];
    v.extend_from_slice(payload);
    v
}

/// (op, `n` fields split at NULs; the last takes the rest).
fn decode(msg: &[u8], n: usize) -> Option<(u8, Vec<&[u8]>)> {
    let (&op, mut rest) = msg.split_first()?;
    let mut f = Vec::new();
    for _ in 1..n {
        let i = rest.iter().position(|&c| c == 0)?;
        f.push(&rest[..i]);
        rest = &rest[i + 1..];
    }
    f.push(rest);
    Some((op, f))
}

fn num(b: &[u8]) -> u64 {
    text(b).parse().unwrap_or(0)
}

fn text(b: &[u8]) -> &str {
    std::str::from_utf8(b).unwrap_or("")
}

fn safe_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)) && s != "." && s != ".."
}

/// `/a/b` → `a/b`, refusing `..` and the like.
fn rel(path: &str) -> Option<PathBuf> {
    let p = Path::new(path.trim_start_matches('/'));
    (p.components().all(|c| matches!(c, Component::Normal(_))) && !path.trim_start_matches('/').is_empty()).then(|| p.to_path_buf())
}

pub struct Session {
    pub store: PathBuf,
    pub id: String,
    /// Names the disk image (`?machine=NAME` on the WebSocket URL).
    pub machine: String,
    /// The body of the session's last HTTP_FETCH.
    pub fetched: std::sync::Mutex<Vec<u8>>,
}

impl Session {
    /// UP_READ_AT, UP_WRITE_AT, UP_STAT on the layer's files.
    fn up_part(&self, op: u8, msg: &[u8]) -> std::io::Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom, Write};
        let n = match op {
            UP_STAT => 2,
            _ => 4,
        };
        let f = decode(msg, n).unwrap().1;
        let (key, path) = (text(f[0]), text(f[1]));
        let (Some(d), Some(r)) = (self.layer(key), rel(path)) else { return Ok(reply(FAILED, b"bad path")) };
        let file = d.join("files").join(r);
        let whiteout = || self.whiteouts(key).iter().any(|x| x == path);
        match op {
            UP_STAT => Ok(match fs::metadata(&file) {
                Ok(m) if m.is_file() => reply(OK, format!("f{}", m.len()).as_bytes()),
                Ok(_) if whiteout() => reply(OK, b"w"),
                Ok(_) => reply(NOT_FOUND, b""),
                Err(_) if whiteout() => reply(OK, b"w"),
                Err(_) => reply(NOT_FOUND, b""),
            }),
            UP_READ_AT => {
                let (off, len) = (num(f[2]), num(f[3]).min(60000));
                match fs::File::open(&file).and_then(|f| if file.is_file() { Ok(f) } else { Err(std::io::ErrorKind::NotFound.into()) }) {
                    Ok(mut fh) => {
                        let size = fh.metadata()?.len();
                        let mut buf = vec![0u8; len.min(size.saturating_sub(off)) as usize];
                        fh.seek(SeekFrom::Start(off))?;
                        fh.read_exact(&mut buf)?;
                        let mut v = vec![OK, b'f'];
                        v.extend_from_slice(&buf);
                        Ok(v)
                    }
                    Err(_) if whiteout() => Ok(reply(OK, b"w")),
                    Err(_) => Ok(reply(NOT_FOUND, b"")),
                }
            }
            _ => {
                let off = num(f[2]);
                let mut w = self.whiteouts(key);
                w.retain(|x| x != path);
                fs::create_dir_all(file.parent().unwrap())?;
                if file.is_dir() {
                    // A removed directory that is now a file.
                    let _ = fs::remove_dir(&file);
                }
                let mut fh = fs::OpenOptions::new().create(true).write(true).truncate(false).open(&file)?;
                fh.seek(SeekFrom::Start(off))?;
                fh.write_all(f[3])?;
                self.set_whiteouts(key, &w)?;
                Ok(reply(OK, b""))
            }
        }
    }

    fn disk(&self) -> std::io::Result<fs::File> {
        let d = self.store.join("disks");
        fs::create_dir_all(&d)?;
        let f = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(d.join(format!("{}.img", self.machine)))?;
        if f.metadata()?.len() < DISK_SIZE {
            f.set_len(DISK_SIZE)?;
        }
        Ok(f)
    }

    fn layer(&self, key: &str) -> Option<PathBuf> {
        safe_name(key).then(|| self.store.join("pods").join(&self.id).join(key))
    }

    fn whiteouts(&self, key: &str) -> Vec<String> {
        self.layer(key)
            .and_then(|d| fs::read_to_string(d.join("whiteouts")).ok())
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default()
    }

    fn set_whiteouts(&self, key: &str, w: &[String]) -> std::io::Result<()> {
        let d = self.layer(key).ok_or(std::io::ErrorKind::InvalidInput)?;
        fs::create_dir_all(&d)?;
        fs::write(d.join("whiteouts"), w.iter().map(|x| format!("{x}\n")).collect::<String>())
    }

    pub fn handle(&self, msg: &[u8]) -> Vec<u8> {
        let Some(&op) = msg.first() else { return reply(FAILED, b"empty") };
        let r = match op {
            OBJ_PUT => {
                let data = &msg[1..];
                let id = Id::of(data);
                let p = self.store.join("objects").join(id.to_string());
                fs::create_dir_all(self.store.join("objects")).and_then(|_| fs::write(&p, data)).map(|_| reply(OK, id.to_string().as_bytes()))
            }
            INDEX => {
                let r = text(&msg[1..]);
                let r = if r.contains(':') { r.to_string() } else { format!("{r}:latest") };
                Ok(match r.split_once(':') {
                    Some((n, t)) if safe_name(n) && safe_name(t) => match fs::read_to_string(self.store.join("refs").join(n).join(t)) {
                        Ok(id) => reply(OK, id.trim().as_bytes()),
                        Err(_) => reply(NOT_FOUND, b""),
                    },
                    _ => reply(FAILED, b"bad ref"),
                })
            }
            OBJ_SIZE => {
                let f = decode(msg, 1).map(|(_, f)| text(f[0]).to_string()).unwrap_or_default();
                Ok(match Id::parse(&f).and_then(|id| crate::upstream::object(&self.store, &id)).and_then(|p| fs::metadata(p).ok()) {
                    Some(m) => reply(OK, format!("{}", m.len()).as_bytes()),
                    None => reply(NOT_FOUND, b""),
                })
            }
            OBJ_READ_AT => {
                use std::io::{Read, Seek, SeekFrom};
                let f = decode(msg, 3).unwrap().1;
                let (off, len) = (num(f[1]), num(f[2]).min(60000));
                match Id::parse(text(f[0])).map(|id| crate::upstream::object(&self.store, &id).unwrap_or_else(|| self.store.join("objects").join(id.to_string()))) {
                    Some(p) => (|| -> std::io::Result<Vec<u8>> {
                        let mut fh = match fs::File::open(&p) {
                            Ok(fh) => fh,
                            Err(_) => return Ok(reply(NOT_FOUND, b"")),
                        };
                        let size = fh.metadata()?.len();
                        let mut buf = vec![0u8; len.min(size.saturating_sub(off)) as usize];
                        fh.seek(SeekFrom::Start(off))?;
                        fh.read_exact(&mut buf)?;
                        let mut v = vec![OK];
                        v.extend_from_slice(&buf);
                        Ok(v)
                    })(),
                    None => Ok(reply(FAILED, b"bad id")),
                }
            }
            REF_SET => {
                let f = decode(msg, 2).unwrap().1;
                let r = text(f[0]);
                match (r.split_once(':'), Id::parse(text(f[1]))) {
                    (Some((n, t)), Some(id)) if safe_name(n) && safe_name(t) => {
                        let d = self.store.join("refs").join(n);
                        fs::create_dir_all(&d).and_then(|_| fs::write(d.join(t), format!("{id}\n"))).map(|_| reply(OK, b""))
                    }
                    _ => Ok(reply(FAILED, b"bad ref")),
                }
            }
            UP_WRITE => {
                let f = decode(msg, 3).unwrap().1;
                let (key, path) = (text(f[0]), text(f[1]));
                match (self.layer(key), rel(path)) {
                    (Some(d), Some(r)) => {
                        let p = d.join("files").join(r);
                        let mut w = self.whiteouts(key);
                        w.retain(|x| x != path);
                        if p.is_dir() {
                            let _ = fs::remove_dir(&p);
                        }
                        fs::create_dir_all(p.parent().unwrap())
                            .and_then(|_| fs::write(&p, f[2]))
                            .and_then(|_| self.set_whiteouts(key, &w))
                            .map(|_| reply(OK, b""))
                    }
                    _ => Ok(reply(FAILED, b"bad path")),
                }
            }
            UP_DELETE => {
                let f = decode(msg, 2).unwrap().1;
                let (key, path) = (text(f[0]), text(f[1]));
                match (self.layer(key), rel(path)) {
                    (Some(d), Some(r)) => {
                        let p = d.join("files").join(r);
                        if p.is_dir() {
                            let _ = fs::remove_dir(&p);
                        } else {
                            let _ = fs::remove_file(&p);
                        }
                        let mut w = self.whiteouts(key);
                        if !w.iter().any(|x| x == path) {
                            w.push(path.to_string());
                        }
                        self.set_whiteouts(key, &w).map(|_| reply(OK, b""))
                    }
                    _ => Ok(reply(FAILED, b"bad path")),
                }
            }
            UP_READ => {
                let f = decode(msg, 2).unwrap().1;
                let (key, path) = (text(f[0]), text(f[1]));
                match (self.layer(key), rel(path)) {
                    (Some(d), Some(r)) => Ok(match fs::read(d.join("files").join(&r)).and_then(|b| if d.join("files").join(&r).is_file() { Ok(b) } else { Err(std::io::ErrorKind::NotFound.into()) }) {
                        Ok(b) => {
                            let mut v = vec![OK, b'f'];
                            v.extend_from_slice(&b);
                            v
                        }
                        Err(_) if self.whiteouts(key).iter().any(|x| x == path) => reply(OK, b"w"),
                        Err(_) => reply(NOT_FOUND, b""),
                    }),
                    _ => Ok(reply(FAILED, b"bad path")),
                }
            }
            UP_LIST => {
                let key = text(&msg[1..]);
                let mut out = String::new();
                if let Some(d) = self.layer(key) {
                    let files = d.join("files");
                    let mut todo = vec![files.clone()];
                    let mut all = vec![];
                    while let Some(dir) = todo.pop() {
                        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
                            let p = e.path();
                            if p.is_dir() {
                                todo.push(p);
                            } else if let Ok(m) = e.metadata() {
                                all.push((format!("/{}", p.strip_prefix(&files).unwrap().display()), m.len()));
                            }
                        }
                    }
                    all.sort();
                    for (p, n) in all {
                        out += &format!("f {n} {p}\n");
                    }
                    for w in self.whiteouts(key) {
                        out += &format!("w 0 {w}\n");
                    }
                }
                Ok(reply(OK, out.as_bytes()))
            }
            UP_READ_AT | UP_WRITE_AT | UP_STAT => self.up_part(op, msg),
            OBJ_PUT_PART => {
                let f = decode(msg, 4).unwrap().1;
                let (off, total) = (num(f[1]), num(f[2]));
                match Id::parse(text(f[0])) {
                    Some(id) if off + f[3].len() as u64 <= total => {
                        use std::io::{Seek, SeekFrom, Write};
                        let dir = self.store.join("objects");
                        let part = dir.join(format!(".part-{id}"));
                        let r = fs::create_dir_all(&dir)
                            .and_then(|_| fs::OpenOptions::new().create(true).write(true).truncate(false).open(&part))
                            .and_then(|mut f2| f2.seek(SeekFrom::Start(off)).and_then(|_| f2.write_all(f[3])));
                        match r {
                            Err(e) => Err(e),
                            Ok(()) if off + f[3].len() as u64 == total => fs::read(&part).and_then(|data| {
                                if Id::of(&data) == id && data.len() as u64 == total {
                                    fs::rename(&part, dir.join(id.to_string())).map(|_| reply(OK, b""))
                                } else {
                                    let _ = fs::remove_file(&part);
                                    Ok(reply(FAILED, b"the parts do not hash to the id"))
                                }
                            }),
                            Ok(()) => Ok(reply(OK, b"")),
                        }
                    }
                    _ => Ok(reply(FAILED, b"bad part")),
                }
            }
            DISK_INFO if safe_name(&self.machine) => Ok(reply(OK, &(DISK_SIZE / SECTOR).to_le_bytes())),
            DISK_READ if safe_name(&self.machine) && msg.len() == 13 => {
                use std::io::{Read, Seek, SeekFrom};
                let sector = u64::from_le_bytes(msg[1..9].try_into().unwrap());
                let count = u32::from_le_bytes(msg[9..13].try_into().unwrap()) as u64;
                let mut buf = vec![0u8; (count.min(128) * SECTOR) as usize];
                self.disk()
                    .and_then(|mut f| f.seek(SeekFrom::Start(sector * SECTOR)).and_then(|_| f.read_exact(&mut buf)))
                    .map(|_| reply(OK, &buf))
            }
            DISK_WRITE if safe_name(&self.machine) && msg.len() > 9 => {
                use std::io::{Seek, SeekFrom, Write};
                let sector = u64::from_le_bytes(msg[1..9].try_into().unwrap());
                let data = &msg[9..];
                if sector * SECTOR + data.len() as u64 > DISK_SIZE {
                    Ok(reply(FAILED, b"past the end"))
                } else {
                    self.disk()
                        .and_then(|mut f| f.seek(SeekFrom::Start(sector * SECTOR)).and_then(|_| f.write_all(data)))
                        .map(|_| reply(OK, b""))
                }
            }
            KV_HELLO => {
                let fns = crate::fn_base().unwrap_or_else(|| String::from("-"));
                Ok(reply(OK, format!("{} {} {} {fns}", self.machine, self.id, cluster::now_ms()).as_bytes()))
            }
            KV_LIST => {
                let p = decode(msg, 1).map(|(_, f)| text(f[0]).to_string()).unwrap_or_default();
                Ok(cluster::with(&self.store, |kv| {
                    let mut out = format!("{}\n", cluster::now_ms()).into_bytes();
                    // KV namespaces' data (`data/…`) only when asked for: every
                    // node reads the full listing once a second.
                    for (k, r) in kv.list(&p).filter(|(k, _)| !p.is_empty() || !k.starts_with("data/")) {
                        out.extend_from_slice(format!("{k} {} {} {}\n", r.version, r.updated, r.value.len()).as_bytes());
                        out.extend_from_slice(&r.value);
                        out.push(b'\n');
                    }
                    reply(OK, &out)
                }))
            }
            KV_PUT | KV_DEL => {
                let n = if op == KV_PUT { 3 } else { 2 };
                match decode(msg, n) {
                    Some((_, f)) if cluster::key_ok(text(f[0])) => {
                        let expect = (!f[1].is_empty()).then(|| num(f[1]));
                        Ok(cluster::with(&self.store, |kv| match op {
                            KV_PUT => match kv.put(text(f[0]), expect, f[2]) {
                                Put::Ok(v) => reply(OK, format!("{v}").as_bytes()),
                                Put::Conflict => reply(FAILED, b"conflict"),
                            },
                            _ => match kv.delete(text(f[0]), expect) {
                                Some(true) => reply(OK, b""),
                                Some(false) => reply(FAILED, b"conflict"),
                                None => reply(NOT_FOUND, b""),
                            },
                        }))
                    }
                    _ => Ok(reply(FAILED, b"bad key")),
                }
            }
            HTTP_FETCH => match decode(msg, 4) {
                Some((_, f)) => Ok(match http::fetch(text(f[0]), text(f[1]), text(f[2]), f[3]) {
                    Ok(a) => {
                        let head = format!("{} {}\n{}", a.status, a.body.len(), a.headers);
                        *self.fetched.lock().unwrap_or_else(|e| e.into_inner()) = a.body;
                        reply(OK, head.as_bytes())
                    }
                    Err(m) => reply(FAILED, m.as_bytes()),
                }),
                None => Ok(reply(FAILED, b"bad request")),
            },
            HTTP_READ => {
                let off = decode(msg, 1).map(|(_, f)| num(f[0]) as usize).unwrap_or(0);
                let body = self.fetched.lock().unwrap_or_else(|e| e.into_inner());
                let part = body.get(off..).map(|b| &b[..b.len().min(PART)]).unwrap_or(&[]);
                Ok(reply(OK, part))
            }
            // [SQL, name, sql, params (JSON), format] → [OK, JSON or a text table]
            SQL => match decode(msg, 4) {
                Some((_, f)) => Ok(match crate::backend::sql(&self.store, text(f[0]), text(f[1]), text(f[2])) {
                    Ok(j) if text(f[3]) == "text" => reply(OK, crate::backend::as_text(&j).as_bytes()),
                    Ok(j) => reply(OK, j.to_string().as_bytes()),
                    Err(m) => reply(FAILED, m.as_bytes()),
                }),
                None => Ok(reply(FAILED, b"bad request")),
            },
            // [REDIS, name, args (JSON array), format] → [OK, JSON or text]
            REDIS => match decode(msg, 3) {
                Some((_, f)) => Ok(match crate::backend::redis(text(f[0]), text(f[1])) {
                    Ok(j) if text(f[2]) == "text" => reply(OK, crate::backend::redis_text(&j).as_bytes()),
                    Ok(j) => reply(OK, j.to_string().as_bytes()),
                    Err(m) => reply(FAILED, m.as_bytes()),
                }),
                None => Ok(reply(FAILED, b"bad request")),
            },
            _ => Ok(reply(FAILED, b"unknown request")),
        };
        r.unwrap_or_else(|e| reply(FAILED, e.to_string().as_bytes()))
    }
}
