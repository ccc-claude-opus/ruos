//! local: this machine's own storage (doc/storage.md), kept by the browser
//! (IndexedDB, the kernel's remote channel 2) instead of the server. It
//! gives out channels that speak the server's protocols, so a program does
//! not know where its data is: `kv:NAME` (kv.rs), `redis:NAME` (cache.rs,
//! a small Redis), `sql:NAME` (sql.rs, SQLite as a WASI program) and
//! `fs:ROOT` (files.rs). Names: `storage` (the host's channel), `store`,
//! `client` (requests: libsys::local).
#![no_std]
#![no_main]

extern crate alloc;

mod cache;
mod files;
mod kv;
mod sql;
mod storage;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::store::{CONNECT, REDIS, SQL, Store};
use libsys::wire::{FAILED, decode, text};
use libsys::{Channel, Error, Handle, Port, ns, println, signal};

use cache::Cache;
use files::Files;
use sql::Db;
use storage::Storage;

libsys::entry!(main);

#[derive(Clone, PartialEq)]
enum Kind {
    /// libsys::local requests.
    Client,
    Fs(String),
    Kv(String),
    Redis(String),
    Sql(String),
    /// A database program's console (by database name).
    Console(String),
}

/// A database program ended (port key: TASK + its slot's key space).
const TASK: u64 = 1 << 32;

struct Local {
    storage: Storage,
    store: Store,
    port: Port,
    slots: Vec<Option<(Channel, Kind)>>,
    files: BTreeMap<String, Files>,
    caches: BTreeMap<String, Cache>,
    dbs: BTreeMap<String, Db>,
    /// Port key of each database program's task.
    tasks: BTreeMap<u64, String>,
    next_task: u64,
}

fn name_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= 32 && n.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
}

impl Local {
    fn add(&mut self, c: Channel, k: Kind) -> usize {
        let i = match self.slots.iter().position(Option::is_none) {
            Some(i) => i,
            None => {
                self.slots.push(None);
                self.slots.len() - 1
            }
        };
        self.slots[i] = Some((c, k));
        let _ = self.port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, i as u64);
        i
    }

    fn files(&mut self, root: &str) -> &mut Files {
        if !self.files.contains_key(root) {
            let f = Files::open(&self.storage, root);
            self.files.insert(String::from(root), f);
        }
        self.files.get_mut(root).unwrap()
    }

    fn reply(&self, slot: usize, r: &[u8]) {
        if let Some((c, _)) = &self.slots[slot] {
            let _ = c.write(r, &[]);
        }
    }

    /// What is kept: `KIND NAME [SIZE]` lines.
    fn ls(&mut self) -> String {
        let mut out = String::new();
        let mut last = String::new();
        for k in self.storage.keys("k ").unwrap_or_default() {
            let name = k.split(' ').nth(1).unwrap_or("");
            if name != last {
                out += &format!("kv {name}\n");
                last = String::from(name);
            }
        }
        for (n, size) in self.files("cache").files_in("/") {
            out += &format!("redis {n} {size}\n");
        }
        for (n, size) in self.files("sql").files_in("/") {
            if let Some(db) = n.strip_suffix(".db") {
                out += &format!("sql {db} {size}\n");
            }
        }
        for k in self.storage.keys("m ").unwrap_or_default() {
            let root = k.split(' ').nth(1).unwrap_or("");
            if root.starts_with("fs-") && !out.contains(&format!("fs {}\n", &root[3..])) {
                out += &format!("fs {}\n", &root[3..]);
            }
        }
        out
    }

    fn client(&mut self, msg: &[u8], hs: &[Handle]) -> String {
        let t = text(msg);
        match (t.split_once(' '), hs) {
            (Some(("connect", what)), [h]) => {
                let kind = match what.split_once(':') {
                    Some(("kv", n)) if name_ok(n) => Kind::Kv(String::from(n)),
                    Some(("redis", n)) if name_ok(n) => Kind::Redis(String::from(n)),
                    Some(("sql", n)) if name_ok(n) => Kind::Sql(String::from(n)),
                    Some(("fs", n)) if name_ok(n) => Kind::Fs(format!("fs-{n}")),
                    _ => {
                        let _ = h.close();
                        return format!("error cannot keep {what} here (kv:NAME, redis:NAME, sql:NAME, fs:NAME; names a-z 0-9 - _)");
                    }
                };
                self.add(Channel(*h), kind);
                String::from("ok")
            }
            (None, [h]) if t == "dup" => {
                self.add(Channel(*h), Kind::Client);
                String::from("ok")
            }
            (None, []) if t == "ls" => self.ls(),
            (Some(("rm", what)), []) => self.remove(what),
            _ => {
                for h in hs {
                    let _ = h.close();
                }
                String::from("error connect KIND:NAME | dup | ls | rm KIND:NAME")
            }
        }
    }

    /// Forgets `KIND:NAME` and everything kept for it.
    fn remove(&mut self, what: &str) -> String {
        let s = Storage(self.storage.0);
        let found = match what.split_once(':') {
            Some(("kv", n)) if name_ok(n) => {
                let keys = self.storage.keys(&format!("k {n} ")).unwrap_or_default();
                for k in &keys {
                    let _ = self.storage.delete(k);
                }
                !keys.is_empty()
            }
            Some(("redis", n)) if name_ok(n) => {
                self.caches.remove(n);
                self.files("cache").remove(&s, &format!("/{n}"))
            }
            Some(("sql", n)) if name_ok(n) => {
                // Its program first: it must not write the file again.
                if let Some(db) = self.dbs.get(n) {
                    let _ = db.task.kill();
                    self.tasks.retain(|_, x| x != n);
                    self.drop_db(n);
                }
                let sql = self.files("sql");
                let db = sql.remove(&s, &format!("/{n}.db"));
                for x in ["-journal", "-wal", "-shm"] {
                    sql.remove(&s, &format!("/{n}.db{x}"));
                }
                db
            }
            Some(("fs", n)) if name_ok(n) => self.files(&format!("fs-{n}")).remove(&s, "/"),
            _ => return format!("error cannot remove {what} (kv:NAME, redis:NAME, sql:NAME, fs:NAME)"),
        };
        if found { String::from("ok") } else { format!("error nothing kept for {what}") }
    }

    /// A store-protocol request on `redis:` / `sql:` (REDIS, SQL, CONNECT).
    fn store_request(&mut self, slot: usize, kind: &Kind, msg: &[u8], hs: &[Handle]) {
        let fail = |why: &str| [&[FAILED][..], why.as_bytes()].concat();
        match (msg.first(), kind) {
            (Some(&CONNECT), _) if hs.len() == 1 => {
                self.add(Channel(hs[0]), kind.clone());
            }
            (Some(&REDIS), Kind::Redis(name)) => {
                let r = match decode(msg, 3) {
                    Some((_, f)) if text(f[0]) == name.as_str() => {
                        if !self.caches.contains_key(name) {
                            let s = &self.storage;
                            let c = Cache::open(s, self.files.entry(String::from("cache")).or_insert_with(|| Files::open(s, "cache")), name);
                            self.caches.insert(name.clone(), c);
                        }
                        let files = self.files.get_mut("cache").unwrap();
                        match self.caches.get_mut(name).unwrap().request(&self.storage, files, text(f[1]), text(f[2])) {
                            Ok(a) => [&[libsys::wire::OK][..], a.as_bytes()].concat(),
                            Err(why) => fail(&why),
                        }
                    }
                    Some((_, f)) => fail(&format!("no redis:{} capability", text(f[0]))),
                    None => fail("bad request"),
                };
                self.reply(slot, &r);
            }
            (Some(&SQL), Kind::Sql(name)) => match decode(msg, 4) {
                Some((_, f)) if text(f[0]) == name.as_str() => {
                    if !self.dbs.contains_key(name) && !self.start_db(name) {
                        self.reply(slot, &fail("cannot start sqlite (its image, sqlited, comes from the server's registry the first time)"));
                        return;
                    }
                    self.dbs.get_mut(name).unwrap().ask(slot, text(f[1]), text(f[2]), text(f[3]));
                }
                Some((_, f)) => self.reply(slot, &fail(&format!("no sql:{} capability", text(f[0])))),
                None => self.reply(slot, &fail("bad request")),
            },
            _ => {
                for h in hs {
                    let _ = h.close();
                }
                self.reply(slot, &fail("not here: this channel is only for its own kv, cache or database"));
            }
        }
    }

    fn start_db(&mut self, name: &str) -> bool {
        let Ok((fs, fs_srv)) = Channel::create() else { return false };
        let Some(db) = Db::start(&self.store, name, fs_srv) else {
            let _ = fs.close();
            return false;
        };
        self.add(fs, Kind::Fs(String::from("sql")));
        self.add(db.con, Kind::Console(String::from(name)));
        let key = TASK + self.next_task;
        self.next_task += 1;
        let _ = self.port.bind(db.task, signal::TERMINATED, key);
        self.tasks.insert(key, String::from(name));
        self.dbs.insert(String::from(name), db);
        true
    }

    /// Requests on slot `i` until it has none (then waits on it again).
    fn serve(&mut self, i: usize) {
        let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES];
        loop {
            let Some((c, kind)) = self.slots[i].clone() else { return };
            let mut hs = [Handle(0); 1];
            let (n, nh) = match c.read(&mut buf, &mut hs) {
                Ok(r) => r,
                Err(Error::ShouldWait) => {
                    let _ = self.port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, i as u64);
                    return;
                }
                Err(_) => {
                    let _ = c.close();
                    self.slots[i] = None;
                    return;
                }
            };
            let (msg, hs) = (&buf[..n], &hs[..nh]);
            match &kind {
                Kind::Client => {
                    let r = self.client(msg, hs);
                    let _ = c.write(r.as_bytes(), &[]);
                }
                Kind::Fs(root) => {
                    if msg.first() == Some(&libsys::fs::CONNECT) && nh == 1 {
                        self.add(Channel(hs[0]), kind.clone());
                        continue;
                    }
                    for h in hs {
                        let _ = h.close();
                    }
                    // OPEN_MEM is for image files: these are not.
                    let r = if msg.first() == Some(&libsys::fs::OPEN_MEM) {
                        alloc::vec![FAILED]
                    } else {
                        let s = Storage(self.storage.0);
                        self.files(root).request(&s, msg)
                    };
                    let _ = c.write(&r, &[]);
                }
                Kind::Kv(name) => {
                    for h in hs {
                        let _ = h.close();
                    }
                    let r = kv::request(&self.storage, name, msg);
                    let _ = c.write(&r, &[]);
                }
                Kind::Redis(_) | Kind::Sql(_) => {
                    let m = Vec::from(msg);
                    let hs = Vec::from(hs);
                    self.store_request(i, &kind, &m, &hs);
                }
                Kind::Console(db) => {
                    let done = match self.dbs.get_mut(db) {
                        Some(d) => d.from_program(msg),
                        None => Vec::new(),
                    };
                    for (slot, r) in done {
                        self.reply(slot, &r);
                    }
                }
            }
        }
    }

    /// Database program `key` ended.
    fn db_ended(&mut self, key: u64) {
        let Some(name) = self.tasks.remove(&key) else { return };
        if self.dbs.contains_key(&name) {
            println!("local: sqlite for {name} ended");
            self.drop_db(&name);
        }
    }

    /// Forgets database `name`'s program: whoever is waiting gets FAILED.
    fn drop_db(&mut self, name: &str) {
        let Some(db) = self.dbs.remove(name) else { return };
        for s in self.slots.iter_mut() {
            if matches!(s, Some((_, Kind::Console(n))) if n == name) {
                *s = None;
            }
        }
        for (slot, r) in db.ended() {
            self.reply(slot, &r);
        }
    }
}

fn main(_arg: usize) -> i64 {
    let (Some(st), Some(store), Some(client), Ok(port)) = (ns::get("storage"), ns::get("store"), ns::get("client"), Port::create()) else {
        println!("local: needs storage, store and client");
        return 1;
    };
    let mut l = Local {
        storage: Storage(Channel(st)),
        store: Store(Channel(store)),
        port,
        slots: Vec::new(),
        files: BTreeMap::new(),
        caches: BTreeMap::new(),
        dbs: BTreeMap::new(),
        tasks: BTreeMap::new(),
        next_task: 0,
    };
    l.add(Channel(client), Kind::Client);
    loop {
        let Ok(p) = l.port.wait(u64::MAX) else { return 2 };
        if p.key >= TASK {
            l.db_ended(p.key);
        } else {
            l.serve(p.key as usize);
        }
        // Gone once nobody can reach it any more.
        if !l.slots.iter().any(|s| matches!(s, Some((_, Kind::Client)))) {
            return 0;
        }
    }
}
