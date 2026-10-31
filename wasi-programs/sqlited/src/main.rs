//! sqlited: SQLite for a database kept on this machine (`sql:NAME@local`,
//! doc/storage.md). The local service runs it with the database's files as
//! its directory: `sqlited FILE`. Each line on stdin is a request,
//! `{"sql": …, "params": [...], "format": "json"|"text"}`; each answer is a
//! line on stdout, `{"ok": ANSWER}` or `{"err": WHY}`, ANSWER being what
//! the backend's databases answer (ruos serve's backend.rs): JSON
//! `{"columns", "rows"}` / `{"changed", "last_id"}`, or a text table.
use std::io::{BufRead, Write};

use rusqlite::types::{Value, ValueRef};
use serde_json::{Value as Json, json};

fn value(v: ValueRef) -> Json {
    match v {
        ValueRef::Null => Json::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => json!(format!("({} bytes)", b.len())),
    }
}

fn param(j: &Json) -> Value {
    match j {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Integer(*b as i64),
        Json::Number(n) => n.as_i64().map(Value::Integer).unwrap_or_else(|| Value::Real(n.as_f64().unwrap_or(0.0))),
        Json::String(s) => Value::Text(s.clone()),
        other => Value::Text(other.to_string()),
    }
}

fn run(c: &rusqlite::Connection, sql: &str, params: &[Json]) -> Result<Json, String> {
    let mut st = c.prepare(sql).map_err(|e| e.to_string())?;
    let ps: Vec<Value> = params.iter().map(param).collect();
    if st.column_count() == 0 {
        let n = st.execute(rusqlite::params_from_iter(ps)).map_err(|e| e.to_string())?;
        return Ok(json!({"changed": n, "last_id": c.last_insert_rowid()}));
    }
    let cols: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = st.query(rusqlite::params_from_iter(ps)).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        out.push(Json::Array((0..cols.len()).map(|i| r.get_ref(i).map(value).unwrap_or(Json::Null)).collect()));
    }
    Ok(json!({"columns": cols, "rows": out}))
}

/// The backend's text table (backend.rs `as_text`).
fn as_text(j: &Json) -> String {
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

fn main() {
    let file = std::env::args().nth(1).unwrap_or_else(|| String::from("db.sqlite"));
    let mut out = std::io::stdout().lock();
    let answer = |out: &mut std::io::StdoutLock, j: Json| {
        let _ = writeln!(out, "{j}");
        let _ = out.flush();
    };
    // No file locks under WASI: the local service is the only user of the file.
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE;
    let c = match rusqlite::Connection::open_with_flags_and_vfs(&file, flags, c"unix-none") {
        Ok(c) => c,
        Err(e) => {
            answer(&mut out, json!({"err": format!("cannot open {file}: {e}")}));
            std::process::exit(1);
        }
    };
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let req: Json = match serde_json::from_str(&line) {
            Ok(j) => j,
            Err(e) => {
                answer(&mut out, json!({"err": format!("bad request: {e}")}));
                continue;
            }
        };
        let params: Vec<Json> = req["params"].as_array().cloned().unwrap_or_default();
        let r = run(&c, req["sql"].as_str().unwrap_or(""), &params);
        answer(&mut out, match r {
            Ok(j) if req["format"] == "text" => json!({"ok": as_text(&j)}),
            Ok(j) => json!({"ok": j.to_string()}),
            Err(e) => json!({"err": e}),
        });
    }
}
