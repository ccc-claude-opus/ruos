//! Backend services for the machines (doc/v2.md): databases and other
//! software on this side, the desktop OS, each one a capability in the OS
//! (`sql:NAME`, `redis:NAME`). A machine's store forwards the requests of
//! the programs given that capability.
//!
//! - `sql:NAME`: SQLite by default (`STORE/sql/NAME.db`, made when first
//!   used; `rusqlite` with SQLite compiled in), or what `--sql NAME=URL`
//!   says: `sqlite:PATH` or `postgres://…` (`tokio-postgres`).
//! - `redis:NAME`: a Redis server, `--redis NAME=redis://HOST:PORT/`.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::{Value as Json, json};

pub struct Settings {
    /// NAME → URL.
    pub sql: Vec<(String, String)>,
    pub redis: Vec<(String, String)>,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

pub fn configure(s: Settings) {
    let _ = SETTINGS.set(s);
}

fn settings() -> &'static Settings {
    SETTINGS.get_or_init(|| Settings { sql: Vec::new(), redis: Vec::new() })
}

fn name_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= 32 && n.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
}

enum Db {
    Sqlite(rusqlite::Connection),
    Postgres(tokio_postgres::Client),
}

static DBS: Mutex<Option<HashMap<String, Db>>> = Mutex::new(None);

fn open(store: &Path, name: &str) -> Result<Db, String> {
    let url = settings().sql.iter().find(|(n, _)| n == name).map(|(_, u)| u.clone());
    match url.as_deref() {
        Some(u) if u.starts_with("postgres://") || u.starts_with("postgresql://") => {
            let rt = tokio::runtime::Handle::current();
            let (client, conn) = rt.block_on(tokio_postgres::connect(u, tokio_postgres::NoTls)).map_err(|e| format!("postgres: {e}"))?;
            rt.spawn(async move {
                if let Err(e) = conn.await {
                    eprintln!("ruos: postgres: {e}");
                }
            });
            Ok(Db::Postgres(client))
        }
        Some(u) => {
            let p = PathBuf::from(u.strip_prefix("sqlite:").unwrap_or(u));
            rusqlite::Connection::open(&p).map(Db::Sqlite).map_err(|e| format!("sqlite: {e}"))
        }
        None => {
            let dir = store.join("sql");
            let _ = std::fs::create_dir_all(&dir);
            rusqlite::Connection::open(dir.join(format!("{name}.db"))).map(Db::Sqlite).map_err(|e| format!("sqlite: {e}"))
        }
    }
}

fn sqlite_value(v: rusqlite::types::ValueRef) -> Json {
    use rusqlite::types::ValueRef::*;
    match v {
        Null => Json::Null,
        Integer(i) => json!(i),
        Real(f) => json!(f),
        Text(t) => json!(String::from_utf8_lossy(t)),
        Blob(b) => json!(format!("({} bytes)", b.len())),
    }
}

fn sqlite_param(j: &Json) -> rusqlite::types::Value {
    use rusqlite::types::Value;
    match j {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Integer(*b as i64),
        Json::Number(n) => n.as_i64().map(Value::Integer).unwrap_or_else(|| Value::Real(n.as_f64().unwrap_or(0.0))),
        Json::String(s) => Value::Text(s.clone()),
        other => Value::Text(other.to_string()),
    }
}

fn run_sqlite(c: &rusqlite::Connection, sql: &str, params: &[Json]) -> Result<Json, String> {
    let mut st = c.prepare(sql).map_err(|e| e.to_string())?;
    let ps: Vec<rusqlite::types::Value> = params.iter().map(sqlite_param).collect();
    if st.column_count() == 0 {
        let n = st.execute(rusqlite::params_from_iter(ps)).map_err(|e| e.to_string())?;
        return Ok(json!({"changed": n, "last_id": c.last_insert_rowid()}));
    }
    let cols: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = st.query(rusqlite::params_from_iter(ps)).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        out.push(Json::Array((0..cols.len()).map(|i| r.get_ref(i).map(sqlite_value).unwrap_or(Json::Null)).collect()));
    }
    Ok(json!({"columns": cols, "rows": out}))
}

/// PostgreSQL: values come back as text (its simple query protocol);
/// parameters are not supported yet.
fn run_postgres(c: &tokio_postgres::Client, sql: &str, params: &[Json]) -> Result<Json, String> {
    if !params.is_empty() {
        return Err(String::from("parameters are not supported with PostgreSQL yet"));
    }
    let msgs = tokio::runtime::Handle::current().block_on(c.simple_query(sql)).map_err(|e| e.to_string())?;
    let (mut cols, mut rows, mut changed) = (Vec::new(), Vec::new(), 0u64);
    for m in msgs {
        match m {
            tokio_postgres::SimpleQueryMessage::Row(r) => {
                if cols.is_empty() {
                    cols = r.columns().iter().map(|c| c.name().to_string()).collect();
                }
                rows.push(Json::Array((0..r.len()).map(|i| r.get(i).map_or(Json::Null, |s| json!(s))).collect()));
            }
            tokio_postgres::SimpleQueryMessage::CommandComplete(n) => changed = n,
            _ => {}
        }
    }
    Ok(if cols.is_empty() { json!({"changed": changed}) } else { json!({"columns": cols, "rows": rows}) })
}

/// Runs `sql` (with `params`, a JSON array) on database `name`: the JSON answer.
pub fn sql(store: &Path, name: &str, sql: &str, params: &str) -> Result<Json, String> {
    if !name_ok(name) {
        return Err(format!("bad database name {name:?}"));
    }
    let params: Vec<Json> = if params.trim().is_empty() { Vec::new() } else { serde_json::from_str(params).map_err(|e| format!("params: {e}"))? };
    let mut g = DBS.lock().unwrap_or_else(|e| e.into_inner());
    let dbs = g.get_or_insert_with(HashMap::new);
    if !dbs.contains_key(name) {
        let db = open(store, name)?;
        dbs.insert(name.to_string(), db);
    }
    match &dbs[name] {
        Db::Sqlite(c) => run_sqlite(c, sql, &params),
        Db::Postgres(c) => run_postgres(c, sql, &params),
    }
}

/// A result as a text table (the shell's `sql`).
pub fn as_text(j: &Json) -> String {
    let Some(cols) = j.get("columns").and_then(Json::as_array) else {
        return match j.get("changed") {
            Some(n) => format!("{n} row(s)\n"),
            None => format!("{j}\n"),
        };
    };
    let cell = |v: &Json| match v {
        Json::String(s) => s.clone(),
        Json::Null => String::from("NULL"),
        v => v.to_string(),
    };
    let names: Vec<String> = cols.iter().map(cell).collect();
    let rows: Vec<Vec<String>> = j["rows"].as_array().map(|r| r.iter().map(|r| r.as_array().map(|c| c.iter().map(cell).collect()).unwrap_or_default()).collect()).unwrap_or_default();
    let mut w: Vec<usize> = names.iter().map(|n| n.chars().count()).collect();
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            if i < w.len() {
                w[i] = w[i].max(c.chars().count().min(40));
            }
        }
    }
    let line = |r: &[String]| {
        let mut s = String::new();
        for (i, c) in r.iter().enumerate() {
            let c: String = c.chars().take(40).collect();
            let pad = w.get(i).copied().unwrap_or(0).saturating_sub(c.chars().count());
            s += &c;
            if i + 1 < r.len() {
                s += &" ".repeat(pad + 2);
            }
        }
        s.push('\n');
        s
    };
    let mut out = line(&names);
    for r in &rows {
        out += &line(r);
    }
    out += &format!("({} row{})\n", rows.len(), if rows.len() == 1 { "" } else { "s" });
    out
}

static REDIS: Mutex<Option<HashMap<String, redis::Connection>>> = Mutex::new(None);

fn redis_json(v: redis::Value) -> Json {
    use redis::Value::*;
    match v {
        Nil => Json::Null,
        Int(i) => json!(i),
        BulkString(b) => json!(String::from_utf8_lossy(&b)),
        SimpleString(s) => json!(s),
        Okay => json!("OK"),
        Array(a) | Set(a) => Json::Array(a.into_iter().map(redis_json).collect()),
        Double(f) => json!(f),
        Boolean(b) => json!(b),
        Map(m) => Json::Array(m.into_iter().flat_map(|(k, v)| [redis_json(k), redis_json(v)]).collect()),
        other => json!(format!("{other:?}")),
    }
}

/// A Redis command (`args`: a JSON array of strings) on server `name`.
pub fn redis(name: &str, args: &str) -> Result<Json, String> {
    let Some(url) = settings().redis.iter().find(|(n, _)| n == name).map(|(_, u)| u.clone()) else {
        return Err(format!("no Redis {name} on this server (ruos serve --redis {name}=redis://HOST:PORT/)"));
    };
    let args: Vec<String> = serde_json::from_str(args).map_err(|e| format!("args: {e}"))?;
    let (cmd, rest) = args.split_first().ok_or("no command")?;
    let mut g = REDIS.lock().unwrap_or_else(|e| e.into_inner());
    let conns = g.get_or_insert_with(HashMap::new);
    if !conns.contains_key(name) {
        let c = redis::Client::open(url.as_str()).and_then(|c| c.get_connection()).map_err(|e| format!("redis: {e}"))?;
        conns.insert(name.to_string(), c);
    }
    let c = conns.get_mut(name).unwrap();
    let mut q = redis::cmd(cmd);
    for a in rest {
        q.arg(a);
    }
    match q.query::<redis::Value>(c) {
        Ok(v) => Ok(redis_json(v)),
        Err(e) => {
            // A broken connection is made again next time.
            if e.is_connection_dropped() || e.is_io_error() {
                conns.remove(name);
            }
            Err(e.to_string())
        }
    }
}

/// A Redis answer as text (the shell's `redis`).
pub fn redis_text(j: &Json) -> String {
    match j {
        Json::Null => String::from("(nil)\n"),
        Json::String(s) => format!("{s}\n"),
        Json::Array(a) if a.is_empty() => String::from("(empty)\n"),
        Json::Array(a) => a.iter().enumerate().map(|(i, v)| format!("{}) {}", i + 1, redis_text(v))).collect(),
        other => format!("{other}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_round_trip() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        run_sqlite(&c, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)", &[]).unwrap();
        let r = run_sqlite(&c, "INSERT INTO t (name) VALUES (?1)", &[json!("ann")]).unwrap();
        assert_eq!(r["last_id"], 1);
        let r = run_sqlite(&c, "SELECT id, name FROM t", &[]).unwrap();
        assert_eq!(r["rows"][0][1], "ann");
        assert!(as_text(&r).contains("ann"));
    }
}
