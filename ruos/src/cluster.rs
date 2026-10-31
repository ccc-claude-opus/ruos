//! The cluster's records (kube): a small key-value store that every
//! machine's kube shares, like Kubernetes' API server and etcd. Each record
//! has a version (from one counter, so a newer write has a larger number)
//! and the server time of its last write, which is how leases work: a node
//! or the leader that stops writing its record is gone once the time is
//! old enough. A write can say which version it expects (compare and swap).
//! Kept in `STORE/cluster/`, one file per record.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Rec {
    pub version: u64,
    pub updated: u64,
    pub value: Vec<u8>,
}

pub struct Kv {
    dir: PathBuf,
    recs: BTreeMap<String, Rec>,
    last: u64,
}

static KV: Mutex<Option<Kv>> = Mutex::new(None);

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Keys are lowercase names, digits and `-` `/`; a file name has `+` for `/`.
pub fn key_ok(k: &str) -> bool {
    !k.is_empty() && k.len() <= 128 && k.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"-/".contains(&c))
}

fn load(dir: &Path) -> Kv {
    let mut kv = Kv { dir: dir.to_path_buf(), recs: BTreeMap::new(), last: 0 };
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().replace('+', "/");
        let Ok(b) = fs::read(e.path()) else { continue };
        let Some(nl) = b.iter().position(|&c| c == b'\n') else { continue };
        let head = String::from_utf8_lossy(&b[..nl]).into_owned();
        let mut w = head.split(' ').map(|x| x.parse::<u64>().unwrap_or(0));
        let (version, updated) = (w.next().unwrap_or(0), w.next().unwrap_or(0));
        kv.last = kv.last.max(version);
        kv.recs.insert(name, Rec { version, updated, value: b[nl + 1..].to_vec() });
    }
    kv
}

/// Runs `f` on the records of the store in `store`.
pub fn with<T>(store: &Path, f: impl FnOnce(&mut Kv) -> T) -> T {
    let mut g = KV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = store.join("cluster");
    if g.as_ref().is_none_or(|kv| kv.dir != dir) {
        let _ = fs::create_dir_all(&dir);
        *g = Some(load(&dir));
    }
    f(g.as_mut().unwrap())
}

fn quote(s: &str) -> String {
    let mut o = String::from('"');
    for c in s.chars() {
        match c {
            '"' => o += "\\\"",
            '\\' => o += "\\\\",
            '\n' => o += "\\n",
            c if (c as u32) < 0x20 => o += &format!("\\u{:04x}", c as u32),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Every record, for the web page's cluster view (and where Functions are reached).
pub fn json(store: &Path, fn_port: Option<u16>) -> String {
    with(store, |kv| {
        let recs: Vec<String> = kv
            .recs
            .iter()
            .map(|(k, r)| {
                let v = String::from_utf8_lossy(&r.value);
                format!("{}:{{\"version\":{},\"updated\":{},\"value\":{}}}", quote(k), r.version, r.updated, quote(&v))
            })
            .collect();
        let port = fn_port.map_or(String::from("null"), |p| p.to_string());
        format!("{{\"now\":{},\"fn_port\":{port},\"records\":{{{}}}}}", now_ms(), recs.join(","))
    })
}

pub enum Put {
    Ok(u64),
    Conflict,
}

impl Kv {
    fn file(&self, key: &str) -> PathBuf {
        self.dir.join(key.replace('/', "+"))
    }

    fn check(&self, key: &str, expect: Option<u64>) -> bool {
        match (expect, self.recs.get(key)) {
            (None, _) => true,
            (Some(0), r) => r.is_none(),
            (Some(v), Some(r)) => r.version == v,
            (Some(_), None) => false,
        }
    }

    pub fn put(&mut self, key: &str, expect: Option<u64>, value: &[u8]) -> Put {
        if !self.check(key, expect) {
            return Put::Conflict;
        }
        self.last += 1;
        let rec = Rec { version: self.last, updated: now_ms(), value: value.to_vec() };
        let mut b = format!("{} {}\n", rec.version, rec.updated).into_bytes();
        b.extend_from_slice(value);
        let _ = fs::write(self.file(key), b);
        self.recs.insert(key.to_string(), rec);
        Put::Ok(self.last)
    }

    /// None: no such record; Some(false): a different version.
    pub fn delete(&mut self, key: &str, expect: Option<u64>) -> Option<bool> {
        self.recs.get(key)?;
        if !self.check(key, expect) {
            return Some(false);
        }
        self.recs.remove(key);
        let _ = fs::remove_file(self.file(key));
        Some(true)
    }

    pub fn list(&self, prefix: &str) -> impl Iterator<Item = (&String, &Rec)> {
        self.recs.range(prefix.to_string()..).take_while(move |(k, _)| k.starts_with(prefix))
    }
}
