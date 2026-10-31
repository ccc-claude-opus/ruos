//! A Redis-like cache kept on this machine (`redis:NAME@local`): the same
//! requests as the backend's Redis ([REDIS, name, args as a JSON array,
//! format] → [OK, JSON or text] / [FAILED, why]), answered from memory.
//! After each change the whole cache is written to the file `/NAME` of the
//! storage's `cache` tree, and read back when the machine starts again.
//! Strings, lists, hashes and sets; keys may expire (wall clock).
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::{Value as Json, json};

use crate::files::Files;
use crate::storage::Storage;

enum Val {
    Str(String),
    List(VecDeque<String>),
    Hash(BTreeMap<String, String>),
    Set(BTreeSet<String>),
}

struct Entry {
    val: Val,
    /// Wall-clock ms; 0 = never.
    expires: u64,
}

pub struct Cache {
    name: String,
    keys: BTreeMap<String, Entry>,
}

fn now_ms() -> u64 {
    libsys::realtime_ns() / 1_000_000
}

const WRONGTYPE: &str = "WRONGTYPE: Operation against a key holding the wrong kind of value";

/// `*` and `?` (Redis KEYS patterns, without classes).
fn glob(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => glob(&p[1..], s) || (!s.is_empty() && glob(p, &s[1..])),
        (Some(b'?'), Some(_)) => glob(&p[1..], &s[1..]),
        (Some(a), Some(b)) if a == b => glob(&p[1..], &s[1..]),
        _ => false,
    }
}

/// The answer as text, like the backend's (`redis_text`).
pub fn as_text(j: &Json) -> String {
    match j {
        Json::Null => String::from("(nil)\n"),
        Json::String(s) => format!("{s}\n"),
        Json::Array(a) if a.is_empty() => String::from("(empty)\n"),
        Json::Array(a) => a.iter().enumerate().map(|(i, v)| format!("{}) {}", i + 1, as_text(v))).collect(),
        other => format!("{other}\n"),
    }
}

// The file: per key, a type byte, expiry (u64 LE), a count (u32 LE), then
// that many strings (u32 LE length + bytes); the key is the first string.
fn put_str(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u32).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}

impl Cache {
    pub fn open(s: &Storage, files: &mut Files, name: &str) -> Cache {
        let mut c = Cache { name: String::from(name), keys: BTreeMap::new() };
        if let Some(b) = files.load(s, &format!("/{name}")) {
            c.decode(&b);
        }
        c
    }

    fn decode(&mut self, b: &[u8]) {
        let mut at = 0;
        let mut take = |n: usize| -> Option<&[u8]> {
            let r = b.get(at..at + n)?;
            at += n;
            Some(r)
        };
        let now = now_ms();
        while let Some(t) = take(1).map(|x| x[0]) {
            let Some(expires) = take(8).map(|x| u64::from_le_bytes(x.try_into().unwrap())) else { break };
            let Some(n) = take(4).map(|x| u32::from_le_bytes(x.try_into().unwrap())) else { break };
            let mut strs = Vec::new();
            for _ in 0..n {
                let Some(len) = take(4).map(|x| u32::from_le_bytes(x.try_into().unwrap()) as usize) else { return };
                let Some(s) = take(len) else { return };
                strs.push(String::from_utf8_lossy(s).into_owned());
            }
            if strs.is_empty() || (expires != 0 && expires <= now) {
                continue;
            }
            let key = strs.remove(0);
            let val = match t {
                b's' => Val::Str(strs.pop().unwrap_or_default()),
                b'l' => Val::List(strs.into_iter().collect()),
                b'h' => Val::Hash(strs.chunks(2).filter(|p| p.len() == 2).map(|p| (p[0].clone(), p[1].clone())).collect()),
                _ => Val::Set(strs.into_iter().collect()),
            };
            self.keys.insert(key, Entry { val, expires });
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        for (k, e) in &self.keys {
            let (t, strs): (u8, Vec<&str>) = match &e.val {
                Val::Str(s) => (b's', alloc::vec![s.as_str()]),
                Val::List(l) => (b'l', l.iter().map(String::as_str).collect()),
                Val::Hash(h) => (b'h', h.iter().flat_map(|(a, b)| [a.as_str(), b.as_str()]).collect()),
                Val::Set(s) => (b'e', s.iter().map(String::as_str).collect()),
            };
            b.push(t);
            b.extend_from_slice(&e.expires.to_le_bytes());
            b.extend_from_slice(&(strs.len() as u32 + 1).to_le_bytes());
            put_str(&mut b, k);
            for s in strs {
                put_str(&mut b, s);
            }
        }
        b
    }

    /// Drops the expired keys.
    fn sweep(&mut self) {
        let now = now_ms();
        self.keys.retain(|_, e| e.expires == 0 || e.expires > now);
    }

    /// One REDIS request: (OK, answer) or (FAILED, why).
    pub fn request(&mut self, s: &Storage, files: &mut Files, args: &str, format: &str) -> Result<String, String> {
        let args: Vec<String> = serde_json::from_str(args).map_err(|e| format!("args: {e}"))?;
        let (cmd, rest) = args.split_first().ok_or("no command")?;
        let cmd = cmd.to_ascii_uppercase();
        self.sweep();
        let (j, changed) = self.run(&cmd, rest)?;
        if changed {
            files.save(s, &format!("/{}", self.name), &self.encode());
        }
        Ok(if format == "text" { as_text(&j) } else { j.to_string() })
    }

    fn list(&mut self, k: &str, make: bool) -> Result<Option<&mut VecDeque<String>>, String> {
        if make && !self.keys.contains_key(k) {
            self.keys.insert(String::from(k), Entry { val: Val::List(VecDeque::new()), expires: 0 });
        }
        match self.keys.get_mut(k).map(|e| &mut e.val) {
            Some(Val::List(l)) => Ok(Some(l)),
            Some(_) => Err(String::from(WRONGTYPE)),
            None => Ok(None),
        }
    }

    fn hash(&mut self, k: &str, make: bool) -> Result<Option<&mut BTreeMap<String, String>>, String> {
        if make && !self.keys.contains_key(k) {
            self.keys.insert(String::from(k), Entry { val: Val::Hash(BTreeMap::new()), expires: 0 });
        }
        match self.keys.get_mut(k).map(|e| &mut e.val) {
            Some(Val::Hash(h)) => Ok(Some(h)),
            Some(_) => Err(String::from(WRONGTYPE)),
            None => Ok(None),
        }
    }

    fn set(&mut self, k: &str, make: bool) -> Result<Option<&mut BTreeSet<String>>, String> {
        if make && !self.keys.contains_key(k) {
            self.keys.insert(String::from(k), Entry { val: Val::Set(BTreeSet::new()), expires: 0 });
        }
        match self.keys.get_mut(k).map(|e| &mut e.val) {
            Some(Val::Set(h)) => Ok(Some(h)),
            Some(_) => Err(String::from(WRONGTYPE)),
            None => Ok(None),
        }
    }

    fn string(&self, k: &str) -> Result<Option<&String>, String> {
        match self.keys.get(k).map(|e| &e.val) {
            Some(Val::Str(s)) => Ok(Some(s)),
            Some(_) => Err(String::from(WRONGTYPE)),
            None => Ok(None),
        }
    }

    /// Removes an empty list, hash or set (as Redis does).
    fn tidy(&mut self, k: &str) {
        let empty = match self.keys.get(k).map(|e| &e.val) {
            Some(Val::List(l)) => l.is_empty(),
            Some(Val::Hash(h)) => h.is_empty(),
            Some(Val::Set(s)) => s.is_empty(),
            _ => false,
        };
        if empty {
            self.keys.remove(k);
        }
    }

    /// (answer, whether the cache changed).
    fn run(&mut self, cmd: &str, a: &[String]) -> Result<(Json, bool), String> {
        let int = |s: &String| s.parse::<i64>().map_err(|_| String::from("ERR value is not an integer or out of range"));
        let arity = |n: usize, ok: bool| if ok { Ok(()) } else { Err(format!("ERR wrong number of arguments for '{}' command ({n} given)", cmd.to_lowercase())) };
        let n = a.len();
        Ok(match cmd {
            "PING" => (json!(a.first().map_or("PONG", |s| s.as_str())), false),
            "ECHO" => {
                arity(n, n == 1)?;
                (json!(a[0]), false)
            }
            "GET" => {
                arity(n, n == 1)?;
                (self.string(&a[0])?.map_or(Json::Null, |s| json!(s)), false)
            }
            "SET" => {
                arity(n, n >= 2)?;
                let (mut expires, mut nx, mut xx) = (0, false, false);
                let mut i = 2;
                while i < n {
                    match a[i].to_ascii_uppercase().as_str() {
                        "EX" if i + 1 < n => {
                            expires = now_ms() + int(&a[i + 1])?.max(0) as u64 * 1000;
                            i += 1;
                        }
                        "PX" if i + 1 < n => {
                            expires = now_ms() + int(&a[i + 1])?.max(0) as u64;
                            i += 1;
                        }
                        "NX" => nx = true,
                        "XX" => xx = true,
                        _ => return Err(String::from("ERR syntax error")),
                    }
                    i += 1;
                }
                let there = self.keys.contains_key(&a[0]);
                if (nx && there) || (xx && !there) {
                    return Ok((Json::Null, false));
                }
                self.keys.insert(a[0].clone(), Entry { val: Val::Str(a[1].clone()), expires });
                (json!("OK"), true)
            }
            "SETNX" => {
                arity(n, n == 2)?;
                if self.keys.contains_key(&a[0]) {
                    (json!(0), false)
                } else {
                    self.keys.insert(a[0].clone(), Entry { val: Val::Str(a[1].clone()), expires: 0 });
                    (json!(1), true)
                }
            }
            "MGET" => (Json::Array(a.iter().map(|k| match self.keys.get(k).map(|e| &e.val) {
                Some(Val::Str(s)) => json!(s),
                _ => Json::Null,
            }).collect()), false),
            "MSET" => {
                arity(n, n >= 2 && n % 2 == 0)?;
                for p in a.chunks(2) {
                    self.keys.insert(p[0].clone(), Entry { val: Val::Str(p[1].clone()), expires: 0 });
                }
                (json!("OK"), true)
            }
            "DEL" | "UNLINK" => {
                let k = a.iter().filter(|k| self.keys.remove(*k).is_some()).count();
                (json!(k), k > 0)
            }
            "EXISTS" => (json!(a.iter().filter(|k| self.keys.contains_key(*k)).count()), false),
            "INCR" | "DECR" | "INCRBY" | "DECRBY" => {
                let by = match cmd {
                    "INCR" => 1,
                    "DECR" => -1,
                    _ => {
                        arity(n, n == 2)?;
                        let b = int(&a[1])?;
                        if cmd == "DECRBY" { -b } else { b }
                    }
                };
                arity(n, n >= 1)?;
                let old = match self.string(&a[0])? {
                    Some(s) => int(s)?,
                    None => 0,
                };
                let new = old.checked_add(by).ok_or("ERR increment or decrement would overflow")?;
                let expires = self.keys.get(&a[0]).map_or(0, |e| e.expires);
                self.keys.insert(a[0].clone(), Entry { val: Val::Str(new.to_string()), expires });
                (json!(new), true)
            }
            "APPEND" => {
                arity(n, n == 2)?;
                let mut s = self.string(&a[0])?.cloned().unwrap_or_default();
                s += &a[1];
                let len = s.len();
                let expires = self.keys.get(&a[0]).map_or(0, |e| e.expires);
                self.keys.insert(a[0].clone(), Entry { val: Val::Str(s), expires });
                (json!(len), true)
            }
            "STRLEN" => {
                arity(n, n == 1)?;
                (json!(self.string(&a[0])?.map_or(0, |s| s.len())), false)
            }
            "EXPIRE" | "PEXPIRE" => {
                arity(n, n == 2)?;
                let t = int(&a[1])?.max(0) as u64;
                match self.keys.get_mut(&a[0]) {
                    Some(e) => {
                        e.expires = now_ms() + if cmd == "EXPIRE" { t * 1000 } else { t };
                        (json!(1), true)
                    }
                    None => (json!(0), false),
                }
            }
            "PERSIST" => {
                arity(n, n == 1)?;
                match self.keys.get_mut(&a[0]) {
                    Some(e) if e.expires != 0 => {
                        e.expires = 0;
                        (json!(1), true)
                    }
                    _ => (json!(0), false),
                }
            }
            "TTL" | "PTTL" => {
                arity(n, n == 1)?;
                let v = match self.keys.get(&a[0]) {
                    None => -2,
                    Some(e) if e.expires == 0 => -1,
                    Some(e) => {
                        let ms = e.expires.saturating_sub(now_ms()) as i64;
                        if cmd == "TTL" { (ms + 999) / 1000 } else { ms }
                    }
                };
                (json!(v), false)
            }
            "TYPE" => {
                arity(n, n == 1)?;
                let t = match self.keys.get(&a[0]).map(|e| &e.val) {
                    None => "none",
                    Some(Val::Str(_)) => "string",
                    Some(Val::List(_)) => "list",
                    Some(Val::Hash(_)) => "hash",
                    Some(Val::Set(_)) => "set",
                };
                (json!(t), false)
            }
            "KEYS" => {
                arity(n, n == 1)?;
                (Json::Array(self.keys.keys().filter(|k| glob(a[0].as_bytes(), k.as_bytes())).map(|k| json!(k)).collect()), false)
            }
            "DBSIZE" => (json!(self.keys.len()), false),
            "FLUSHDB" | "FLUSHALL" => {
                let any = !self.keys.is_empty();
                self.keys.clear();
                (json!("OK"), any)
            }
            "LPUSH" | "RPUSH" => {
                arity(n, n >= 2)?;
                let l = self.list(&a[0], true)?.unwrap();
                for v in &a[1..] {
                    if cmd == "LPUSH" { l.push_front(v.clone()) } else { l.push_back(v.clone()) }
                }
                (json!(l.len()), true)
            }
            "LPOP" | "RPOP" => {
                arity(n, n == 1)?;
                let v = match self.list(&a[0], false)? {
                    Some(l) => if cmd == "LPOP" { l.pop_front() } else { l.pop_back() },
                    None => None,
                };
                self.tidy(&a[0]);
                let changed = v.is_some();
                (v.map_or(Json::Null, |v| json!(v)), changed)
            }
            "LLEN" => {
                arity(n, n == 1)?;
                (json!(self.list(&a[0], false)?.map_or(0, |l| l.len())), false)
            }
            "LRANGE" | "LINDEX" => {
                arity(n, if cmd == "LRANGE" { n == 3 } else { n == 2 })?;
                let l: Vec<String> = self.list(&a[0], false)?.map(|l| l.iter().cloned().collect()).unwrap_or_default();
                let len = l.len() as i64;
                let at = |i: i64| if i < 0 { len + i } else { i };
                if cmd == "LINDEX" {
                    let i = at(int(&a[1])?);
                    (if (0..len).contains(&i) { json!(l[i as usize]) } else { Json::Null }, false)
                } else {
                    let (s, e) = (at(int(&a[1])?).max(0), at(int(&a[2])?).min(len - 1));
                    let v: Vec<Json> = if s > e { Vec::new() } else { l[s as usize..=e as usize].iter().map(|x| json!(x)).collect() };
                    (Json::Array(v), false)
                }
            }
            "HSET" => {
                arity(n, n >= 3 && n % 2 == 1)?;
                let h = self.hash(&a[0], true)?.unwrap();
                let added = a[1..].chunks(2).filter(|p| h.insert(p[0].clone(), p[1].clone()).is_none()).count();
                (json!(added), true)
            }
            "HGET" => {
                arity(n, n == 2)?;
                (self.hash(&a[0], false)?.and_then(|h| h.get(&a[1]).cloned()).map_or(Json::Null, |v| json!(v)), false)
            }
            "HDEL" => {
                arity(n, n >= 2)?;
                let k = match self.hash(&a[0], false)? {
                    Some(h) => a[1..].iter().filter(|f| h.remove(*f).is_some()).count(),
                    None => 0,
                };
                self.tidy(&a[0]);
                (json!(k), k > 0)
            }
            "HGETALL" | "HKEYS" | "HVALS" => {
                arity(n, n == 1)?;
                let h = self.hash(&a[0], false)?.cloned().unwrap_or_default();
                let v: Vec<Json> = match cmd {
                    "HKEYS" => h.keys().map(|k| json!(k)).collect(),
                    "HVALS" => h.values().map(|k| json!(k)).collect(),
                    _ => h.iter().flat_map(|(k, v)| [json!(k), json!(v)]).collect(),
                };
                (Json::Array(v), false)
            }
            "HLEN" => {
                arity(n, n == 1)?;
                (json!(self.hash(&a[0], false)?.map_or(0, |h| h.len())), false)
            }
            "HEXISTS" => {
                arity(n, n == 2)?;
                (json!(self.hash(&a[0], false)?.is_some_and(|h| h.contains_key(&a[1])) as i64), false)
            }
            "HINCRBY" => {
                arity(n, n == 3)?;
                let by = int(&a[2])?;
                let h = self.hash(&a[0], true)?.unwrap();
                let old = match h.get(&a[1]) {
                    Some(v) => v.parse::<i64>().map_err(|_| String::from("ERR hash value is not an integer"))?,
                    None => 0,
                };
                let new = old.checked_add(by).ok_or("ERR increment or decrement would overflow")?;
                h.insert(a[1].clone(), new.to_string());
                (json!(new), true)
            }
            "SADD" => {
                arity(n, n >= 2)?;
                let s = self.set(&a[0], true)?.unwrap();
                let k = a[1..].iter().filter(|m| s.insert((*m).clone())).count();
                (json!(k), k > 0)
            }
            "SREM" => {
                arity(n, n >= 2)?;
                let k = match self.set(&a[0], false)? {
                    Some(s) => a[1..].iter().filter(|m| s.remove(*m)).count(),
                    None => 0,
                };
                self.tidy(&a[0]);
                (json!(k), k > 0)
            }
            "SMEMBERS" => {
                arity(n, n == 1)?;
                (Json::Array(self.set(&a[0], false)?.map(|s| s.iter().map(|m| json!(m)).collect()).unwrap_or_default()), false)
            }
            "SISMEMBER" => {
                arity(n, n == 2)?;
                (json!(self.set(&a[0], false)?.is_some_and(|s| s.contains(&a[1])) as i64), false)
            }
            "SCARD" => {
                arity(n, n == 1)?;
                (json!(self.set(&a[0], false)?.map_or(0, |s| s.len())), false)
            }
            _ => return Err(format!("ERR unknown command '{cmd}' (this is ruos' own small Redis, kept in the browser)")),
        })
    }
}
