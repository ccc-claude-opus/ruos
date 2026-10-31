//! store: the object store. Its namespace has `pack` (the boot pack), a
//! first `client`, `disk` (the blk service) and/or `remote` (a channel to
//! podsrv). It serves programs, objects and refs, and the pods' writable
//! layers: in memory, or on the server when there is a remote. New objects
//! and refs go to the disk's log (disk.rs; the server's disk on wasm32)
//! and come back at the next boot. Every object of the pack is checked.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::store::*;
use libsys::wire::{decode, encode, text};
use core::cell::RefCell;

use libsys::net::{Net, WebSocket};
use libsys::{Channel, Error, Handle, Memory, Port, ns, println, signal};
use objstore::{Id, Index, Kind, Manifest, Objects, Pack, Tree, full_ref};

mod disk;

libsys::entry!(main);

/// The server: the kernel's remote channel (wasm32) or a WebSocket over
/// the network (native). Requests and answers are store messages.
enum Remote {
    Chan(Channel),
    Ws(RefCell<WebSocket>),
}

static LOST: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static DISK_FULL: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

impl Remote {
    fn call(&self, msg: &[u8]) -> Vec<u8> {
        let r = self.call_once(msg);
        if r.first() == Some(&FAILED) && r.ends_with(b"server gone") && !LOST.swap(true, core::sync::atomic::Ordering::Relaxed) {
            println!("store: the server connection is gone");
        }
        r
    }

    fn call_once(&self, msg: &[u8]) -> Vec<u8> {
        match self {
            Remote::Chan(r) => {
                if r.write(msg, &[]).is_err() {
                    return reply(FAILED, b"server gone");
                }
                let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES];
                match r.recv(&mut buf, &mut []) {
                    Ok((n, _)) => buf[..n].to_vec(),
                    Err(_) => reply(FAILED, b"server gone"),
                }
            }
            Remote::Ws(ws) => {
                let mut ws = ws.borrow_mut();
                let ws = &mut *ws;
                if ws.send(msg).is_err() {
                    return reply(FAILED, b"server gone");
                }
                ws.recv().unwrap_or_else(|_| reply(FAILED, b"server gone"))
            }
        }
    }
}

/// The registry, as QEMU's user network forwards it (`guestfwd`).
const REGISTRY: ([u8; 4], u16) = ([10, 0, 2, 100], 80);

struct Store<'a> {
    pack: Pack<'a>,
    objects: BTreeMap<Id, Vec<u8>>,
    refs: BTreeMap<String, Id>,
    /// key → path → file (None: whiteout).
    uppers: BTreeMap<String, BTreeMap<String, Option<Vec<u8>>>>,
    remote: Option<Remote>,
    log: Option<disk::Log>,
    /// Objects handed out as memory objects (OBJ_MEM): clones of these.
    mems: BTreeMap<Id, Memory>,
}

impl Objects for Store<'_> {
    fn get(&self, id: &Id) -> Option<&[u8]> {
        self.objects.get(id).map(|v| v.as_slice()).or_else(|| self.pack.get(id))
    }
}

/// A decimal field.
fn num(b: &[u8]) -> usize {
    text(b).parse().unwrap_or(0)
}

fn reply(status: u8, payload: &[u8]) -> Vec<u8> {
    encode(status, &[payload])
}

impl Store<'_> {
    fn index(&self, name: &str) -> Option<Id> {
        let r = full_ref(name);
        self.refs.get(&r).or_else(|| self.pack.refs.get(&r)).copied()
    }

    /// Sends a request to the server and waits for its answer.
    fn remote_call(&self, msg: &[u8]) -> Vec<u8> {
        match &self.remote {
            Some(r) => r.call(msg),
            None => reply(FAILED, b"no server"),
        }
    }

    /// The program for this architecture, or else its wasm32 build (which
    /// the caller runs with wasmrt).
    fn get_program(&self, name: &str, c: Channel) -> libsys::Result<()> {
        let man = |i: Id| {
            objstore::manifest_of(self, &i, libsys::ARCH).or_else(|_| objstore::manifest_of(self, &i, "wasm32")).ok()
        };
        let found = self.index(name).and_then(man).and_then(|m| {
            let e = self.lookup(&m.layers, &m.entry).ok()?;
            Some((self.get(&e.id)?, m.entry))
        });
        let Some((prog, entry)) = found else { return c.write(&[NOT_FOUND], &[]) };
        let m = Memory::create(prog.len() as u64)?;
        m.write(0, prog)?;
        let mut r = Vec::from(&[OK][..]);
        r.extend_from_slice(&(prog.len() as u64).to_le_bytes());
        r.extend_from_slice(entry.as_bytes());
        c.write(&r, &[m.0])
    }

    fn list(&self) -> Vec<u8> {
        let mut refs: BTreeMap<&String, &Id> = self.pack.refs.iter().collect();
        refs.extend(self.refs.iter());
        let mut out = String::new();
        for (r, id) in refs {
            let arches = self
                .get(id)
                .and_then(|b| Index::decode(b).ok())
                .map(|x| x.manifests.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>().join(","))
                .unwrap_or_default();
            out += &format!("{:<16} {}  {}\n", r, id.short(), arches);
        }
        reply(OK, out.as_bytes())
    }

    /// Every object of image `name` the store has (other architectures'
    /// programs are not in the boot pack; the registry has them already).
    fn closure(&self, index: Id) -> Vec<Id> {
        let mut seen = BTreeSet::new();
        let mut todo = alloc::vec![index];
        while let Some(id) = todo.pop() {
            let Some(b) = self.get(&id) else { continue };
            if !seen.insert(id) {
                continue;
            }
            if let Ok(x) = Index::decode(b) {
                todo.extend(x.manifests.iter().map(|(_, m)| *m));
            } else if let Ok(m) = Manifest::decode(b) {
                todo.extend(m.layers);
            } else if let Ok(t) = Tree::decode(b) {
                for e in t.entries {
                    if e.kind != Kind::Whiteout {
                        todo.push(e.id);
                    }
                }
            }
        }
        seen.into_iter().collect()
    }

    fn push(&self, name: &str) -> Vec<u8> {
        if self.remote.is_none() {
            return reply(FAILED, b"no registry connection");
        }
        let Some(index) = self.index(name) else { return reply(NOT_FOUND, b"") };
        let ids = self.closure(index);
        for id in &ids {
            let data = self.get(id).unwrap();
            // Large objects go in parts (a message carries at most 64 KiB).
            let rs: Vec<Vec<u8>> = if data.len() <= MAX_INLINE {
                alloc::vec![self.remote_call(&encode(OBJ_PUT, &[data]))]
            } else {
                let (idt, total) = (format!("{id}"), format!("{}", data.len()));
                data.chunks(MAX_INLINE)
                    .enumerate()
                    .map(|(i, c)| {
                        let off = format!("{}", i * MAX_INLINE);
                        self.remote_call(&encode(OBJ_PUT_PART, &[idt.as_bytes(), off.as_bytes(), total.as_bytes(), c]))
                    })
                    .collect()
            };
            if let Some(r) = rs.iter().find(|r| r.first() != Some(&OK)) {
                return reply(FAILED, format!("object {}: {}", id.short(), text(r.get(1..).unwrap_or(&[]))).as_bytes());
            }
        }
        let r = full_ref(name);
        let ok = self.remote_call(&encode(REF_SET, &[r.as_bytes(), format!("{index}").as_bytes()]));
        if ok.first() != Some(&OK) {
            return reply(FAILED, b"ref");
        }
        reply(OK, format!("pushed {r}: {} objects", ids.len()).as_bytes())
    }

    /// Brings image `name` from the server (images left out of the boot
    /// pack): its index, this architecture's (else wasm32's) manifest, the
    /// layers' trees and files, each hash-checked and logged; then the ref.
    fn pull(&mut self, name: &str) -> Option<Id> {
        self.remote.as_ref()?;
        let r = full_ref(name);
        let ans = self.remote_call(&encode(INDEX, &[r.as_bytes()]));
        if ans.first() != Some(&OK) {
            return None;
        }
        let index = Id::parse(text(&ans[1..]))?;
        let x = Index::decode(&self.fetch(&index)?).ok()?;
        let m = x.for_arch(libsys::ARCH).or_else(|| x.for_arch("wasm32"))?;
        let man = Manifest::decode(&self.fetch(&m)?).ok()?;
        let mut todo: Vec<Id> = man.layers.clone();
        while let Some(t) = todo.pop() {
            for e in Tree::decode(&self.fetch(&t)?).ok()?.entries {
                match e.kind {
                    Kind::Dir => todo.push(e.id),
                    Kind::File | Kind::Exec => _ = self.fetch(&e.id)?,
                    Kind::Whiteout => {}
                }
            }
        }
        if let Some(log) = &mut self.log {
            // A full disk only means the next boot pulls it again.
            let _ = log.append(format!("ref {r} {index}\n").as_bytes());
        }
        self.refs.insert(r, index);
        println!("store: pulled {name} ({} objects)", self.objects.len());
        Some(index)
    }

    /// An object, from the server if the store does not have it.
    fn fetch(&mut self, id: &Id) -> Option<Vec<u8>> {
        if let Some(b) = self.get(id) {
            return Some(b.to_vec());
        }
        let (idt, mut data) = (format!("{id}"), Vec::new());
        loop {
            let off = format!("{}", data.len());
            let r = self.remote_call(&encode(OBJ_READ_AT, &[idt.as_bytes(), off.as_bytes(), b"60000"]));
            if r.first() != Some(&OK) {
                return None;
            }
            data.extend_from_slice(&r[1..]);
            if r.len() - 1 < 60000 {
                break;
            }
        }
        if Id::of(&data) != *id {
            println!("store: object {} from the server does not match its hash", id.short());
            return None;
        }
        self.keep(data.clone(), false);
        Some(data)
    }

    /// An object as a copy-on-write memory object (one kept per object).
    fn object_mem(&mut self, id: &str, c: Channel) -> libsys::Result<()> {
        let Some(id) = Id::parse(id) else { return c.write(&[NOT_FOUND], &[]) };
        let Some(len) = self.get(&id).map(|b| b.len()) else { return c.write(&[NOT_FOUND], &[]) };
        if !self.mems.contains_key(&id) {
            let m = Memory::create(len.max(1) as u64)?;
            m.write(0, self.get(&id).unwrap())?;
            self.mems.insert(id, m);
        }
        let m = self.mems[&id].clone_cow()?;
        let mut r = Vec::from(&[OK][..]);
        r.extend_from_slice(&(len as u64).to_le_bytes());
        c.write(&r, &[m.0])
    }

    /// Keeps an object (and logs it on the disk).
    fn put_object(&mut self, data: Vec<u8>) -> Vec<u8> {
        self.keep(data, true)
    }

    /// Keeps an object and logs it on the disk. When the disk is full,
    /// `must_log` makes it an error (a commit); otherwise (an object pulled
    /// from the registry, which still has it) it stays in memory only.
    fn keep(&mut self, data: Vec<u8>, must_log: bool) -> Vec<u8> {
        let id = Id::of(&data);
        if self.get(&id).is_none() {
            if let Some(log) = &mut self.log {
                let mut rec = format!("obj {id} {}\n", data.len()).into_bytes();
                rec.extend_from_slice(&data);
                rec.push(b'\n');
                if log.append(&rec).is_err() {
                    if must_log {
                        return reply(FAILED, b"disk full");
                    }
                    if !DISK_FULL.swap(true, core::sync::atomic::Ordering::Relaxed) {
                        println!("store: the disk is full: pulled objects are kept in memory only");
                    }
                }
            }
            self.objects.insert(id, data);
        }
        reply(OK, format!("{id}").as_bytes())
    }

    fn handle(&mut self, msg: &[u8], c: Channel, extra: Option<Handle>) -> Option<Vec<u8>> {
        let op = *msg.first()?;
        let f = |n| decode(msg, n).map(|(_, f)| f).unwrap_or_default();
        Some(match op {
            GET => {
                let name = text(&f(1)[0]);
                if self.index(name).is_none() {
                    self.pull(name);
                }
                let _ = self.get_program(name, c);
                return None;
            }
            LIST => self.list(),
            OBJ_MEM => {
                let _ = self.object_mem(text(&f(1)[0]), c);
                return None;
            }
            OBJ_GET => match Id::parse(text(&f(1)[0])).and_then(|id| self.get(&id)) {
                Some(b) if b.len() <= MAX_INLINE => reply(OK, b),
                Some(_) => reply(FAILED, b"too large"),
                None => reply(NOT_FOUND, b""),
            },
            OBJ_PUT => self.put_object(f(1)[0].to_vec()),
            OBJ_PUT_MEM => {
                let Some(h) = extra else { return Some(reply(FAILED, b"no memory object")) };
                let len: usize = text(&f(1)[0]).parse().unwrap_or(0);
                let mut data = alloc::vec![0u8; len];
                let r = Memory(h).read(0, &mut data);
                let _ = h.close();
                match r {
                    Ok(()) => self.put_object(data),
                    Err(_) => reply(FAILED, b"cannot read the memory object"),
                }
            }
            OBJ_READ_AT => {
                let a = f(3);
                let (off, len) = (num(&a[1]), num(&a[2]));
                match Id::parse(text(&a[0])).and_then(|id| self.get(&id)) {
                    Some(b) => reply(OK, &b[off.min(b.len())..(off + len.min(MAX_INLINE)).min(b.len())]),
                    None => reply(NOT_FOUND, b""),
                }
            }
            REF_SET => {
                let a = f(2);
                match Id::parse(text(&a[1])) {
                    Some(id) => {
                        let r = full_ref(text(&a[0]));
                        if let Some(log) = &mut self.log {
                            if log.append(format!("ref {r} {id}\n").as_bytes()).is_err() {
                                return Some(reply(FAILED, b"disk full"));
                            }
                        }
                        self.refs.insert(r, id);
                        reply(OK, b"")
                    }
                    None => reply(FAILED, b"bad id"),
                }
            }
            BOOT => reply(OK, format!("{}", self.log.as_ref().map(|l| l.boot).unwrap_or(0)).as_bytes()),
            INDEX => match self.index(text(&f(1)[0])).or_else(|| self.pull(text(&f(1)[0]))) {
                Some(id) => reply(OK, format!("{id}").as_bytes()),
                None => reply(NOT_FOUND, b""),
            },
            UP_WRITE | UP_READ | UP_LIST | UP_DELETE | UP_READ_AT | UP_WRITE_AT | UP_STAT if self.remote.is_some() => {
                self.remote_call(msg)
            }
            // The cluster's records (kube) are on the server.
            KV_HELLO | KV_LIST | KV_PUT | KV_DEL => self.remote_call(msg),
            // Only from clients with the http capability (checked in main).
            HTTP_FETCH | HTTP_READ => self.remote_call(msg),
            UP_READ_AT => {
                let a = f(4);
                let (off, len) = (num(&a[2]), num(&a[3]).min(MAX_INLINE));
                match self.uppers.get(text(&a[0])).and_then(|u| u.get(text(&a[1]))) {
                    Some(Some(d)) => {
                        let mut r = Vec::from(&[OK, b'f'][..]);
                        r.extend_from_slice(&d[off.min(d.len())..(off + len).min(d.len())]);
                        r
                    }
                    Some(None) => reply(OK, b"w"),
                    None => reply(NOT_FOUND, b""),
                }
            }
            UP_WRITE_AT => {
                let a = f(4);
                let off = num(&a[2]);
                let file = self.uppers.entry(String::from(text(&a[0]))).or_default().entry(String::from(text(&a[1]))).or_insert(None);
                let d = file.get_or_insert_with(Vec::new);
                if d.len() < off + a[3].len() {
                    d.resize(off + a[3].len(), 0);
                }
                d[off..off + a[3].len()].copy_from_slice(&a[3]);
                reply(OK, b"")
            }
            UP_STAT => {
                let a = f(2);
                match self.uppers.get(text(&a[0])).and_then(|u| u.get(text(&a[1]))) {
                    Some(Some(d)) => reply(OK, format!("f{}", d.len()).as_bytes()),
                    Some(None) => reply(OK, b"w"),
                    None => reply(NOT_FOUND, b""),
                }
            }
            UP_WRITE => {
                let a = f(3);
                self.uppers.entry(String::from(text(&a[0]))).or_default().insert(String::from(text(&a[1])), Some(a[2].to_vec()));
                reply(OK, b"")
            }
            UP_DELETE => {
                let a = f(2);
                self.uppers.entry(String::from(text(&a[0]))).or_default().insert(String::from(text(&a[1])), None);
                reply(OK, b"")
            }
            UP_READ => {
                let a = f(2);
                match self.uppers.get(text(&a[0])).and_then(|u| u.get(text(&a[1]))) {
                    Some(Some(d)) => {
                        let mut r = Vec::from(&[OK, b'f'][..]);
                        r.extend_from_slice(d);
                        r
                    }
                    Some(None) => reply(OK, b"w"),
                    None => reply(NOT_FOUND, b""),
                }
            }
            UP_LIST => {
                let mut out = String::new();
                for (p, v) in self.uppers.get(text(&f(1)[0])).into_iter().flatten() {
                    out += &match v {
                        Some(d) => format!("f {} {}\n", d.len(), p),
                        None => format!("w 0 {}\n", p),
                    };
                }
                reply(OK, out.as_bytes())
            }
            PUSH => self.push(text(&f(1)[0])),
            CONNECT if extra.is_some() => return None,
            _ => reply(FAILED, b"unknown request"),
        })
    }
}

fn main(_arg: usize) -> i64 {
    let Some(mem) = ns::get("pack").map(Memory) else { return 6 };
    let Ok(size) = mem.size() else { return 1 };
    let mut bytes = alloc::vec![0u8; size as usize];
    if mem.read(0, &mut bytes).is_err() {
        return 2;
    }
    let _ = mem.0.close();
    // The memory object is page granular: the pack ends at its `end` line.
    let pack = match Pack::parse(&bytes, true) {
        Ok(p) => p,
        Err(e) => {
            println!("store: bad pack: {:?}", e);
            return 3;
        }
    };
    let chan = ns::get("remote").map(Channel);
    let remote = match (chan, ns::get("net")) {
        (Some(c), _) => Some(Remote::Chan(c)),
        (None, Some(n)) => {
            let (ip, port) = REGISTRY;
            match Net(Channel(n)).connect(ip, port).and_then(|s| WebSocket::open(s, "registry", "/v1/remote")) {
                Ok(ws) => {
                    println!("store: registry at {}.{}.{}.{}:{} over the network", ip[0], ip[1], ip[2], ip[3], port);
                    Some(Remote::Ws(RefCell::new(ws)))
                }
                Err(e) => {
                    println!("store: no registry on the network ({:?})", e);
                    None
                }
            }
        }
        _ => None,
    };
    println!(
        "store: {} objects, {} images, hashes verified{}",
        pack.objects.len(),
        pack.refs.len(),
        if remote.is_some() { "; writable layers on the server" } else { "" }
    );
    let mut st = Store { pack, objects: BTreeMap::new(), refs: BTreeMap::new(), uppers: BTreeMap::new(), remote, log: None, mems: BTreeMap::new() };
    let dev = match (ns::get("disk"), chan) {
        (Some(d), _) => Some(disk::Dev::Blk(libsys::blk::Disk(Channel(d)))),
        (None, Some(r)) => Some(disk::Dev::Remote(r)),
        _ => None,
    };
    if let Some(dev) = dev {
        match disk::Log::open(dev) {
            Ok((log, records)) => {
                let (mut objs, mut refs) = (0, 0);
                for r in records {
                    match r {
                        disk::Record::Obj(d) => {
                            st.objects.insert(Id::of(&d), d);
                            objs += 1;
                        }
                        disk::Record::Ref(r, id) => {
                            if let Some(id) = Id::parse(&id) {
                                st.refs.insert(r, id);
                                refs += 1;
                            }
                        }
                    }
                }
                println!(
                    "store: disk {} KiB, boot {}, {} objects and {} refs from earlier boots",
                    log.sectors / 2,
                    log.boot,
                    objs,
                    refs
                );
                st.log = Some(log);
            }
            Err(e) => println!("store: no disk log: {:?}", e),
        }
    }
    let Ok(port) = Port::create() else { return 4 };
    // (channel, may it make HTTP requests): clients that came from `web`
    // (and the clients they CONNECT) have the http capability.
    let mut clients: Vec<Option<(Channel, bool)>> = Vec::new();
    let add = |clients: &mut Vec<Option<(Channel, bool)>>, c: Channel, web: bool| {
        clients.push(Some((c, web)));
        let _ = port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, clients.len() as u64 - 1);
    };
    if let Some(c) = ns::get("client") {
        add(&mut clients, Channel(c), false);
    }
    if let Some(c) = ns::get("web") {
        add(&mut clients, Channel(c), true);
    }
    let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES];
    loop {
        let Ok(p) = port.wait(u64::MAX) else { return 5 };
        let i = p.key as usize;
        let Some((c, web)) = clients[i] else { continue };
        let mut hs = [Handle(0); 1];
        let mut alive = true;
        loop {
            match c.read(&mut buf, &mut hs) {
                Ok((n, nh)) => {
                    if buf[0] == CONNECT && nh == 1 {
                        add(&mut clients, Channel(hs[0]), web);
                        continue;
                    }
                    if matches!(buf[0], HTTP_FETCH | HTTP_READ) && !web {
                        let _ = c.write(&reply(FAILED, b"no http capability"), &[]);
                        continue;
                    }
                    let msg = buf[..n].to_vec();
                    if let Some(r) = st.handle(&msg, c, (nh == 1).then_some(hs[0])) {
                        let _ = c.write(&r, &[]);
                    }
                }
                Err(Error::ShouldWait) => break,
                Err(_) => {
                    let _ = c.close();
                    clients[i] = None;
                    alive = false;
                    break;
                }
            }
        }
        if alive {
            let _ = port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, i as u64);
        }
        if clients.iter().all(|c| c.is_none()) {
            return 0;
        }
    }
}
