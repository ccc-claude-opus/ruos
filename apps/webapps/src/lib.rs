//! What the example sites share: the database (`sql:NAME`, rows as JSON
//! objects), request bodies and parameters, and the page around each site.
//! The pages draw themselves in the browser from the JSON APIs; a site's
//! program only answers requests and talks to its database.
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub use libsys::func::{Request, Response, serve_http};
use libsys::sql::Sql;
pub use serde_json::{Value, json};

/// `sql:NAME` from the namespace (the manifest's `uses`).
pub struct Db(Option<Sql>, &'static str);

impl Db {
    pub fn get(name: &'static str) -> Db {
        Db(Sql::get(name), name)
    }

    /// Runs `sql` with `params` (`?1`, `?2`…): the server's JSON answer.
    pub fn run(&self, sql: &str, params: &[Value]) -> Result<Value, String> {
        let Some(db) = &self.0 else { return Err(format!("no sql:{} (add it to the manifest's uses)", self.1)) };
        let p = serde_json::to_string(params).unwrap_or_default();
        match db.query(sql, &p) {
            Ok(Ok(t)) => serde_json::from_str(&t).map_err(|e| format!("bad answer: {e}")),
            Ok(Err(why)) => Err(why),
            Err(e) => Err(format!("store: {e:?}")),
        }
    }

    /// A query's rows as objects: `[{"col": value, …}, …]`.
    pub fn rows(&self, sql: &str, params: &[Value]) -> Result<Vec<Value>, String> {
        let v = self.run(sql, params)?;
        let cols: Vec<String> = v["columns"].as_array().map(|c| c.iter().map(|c| String::from(c.as_str().unwrap_or(""))).collect()).unwrap_or_default();
        let rows = v["rows"].as_array().cloned().unwrap_or_default();
        Ok(rows
            .into_iter()
            .map(|r| {
                let mut o = serde_json::Map::new();
                for (c, x) in cols.iter().zip(r.as_array().cloned().unwrap_or_default()) {
                    o.insert(c.clone(), x);
                }
                Value::Object(o)
            })
            .collect())
    }

    /// The first row, if any.
    pub fn row(&self, sql: &str, params: &[Value]) -> Result<Option<Value>, String> {
        Ok(self.rows(sql, params)?.into_iter().next())
    }
}

/// The request body as JSON (`Null` if it is not).
pub fn body(req: &Request) -> Value {
    serde_json::from_slice(&req.body).unwrap_or(Value::Null)
}

/// A string field of a JSON body, trimmed; None if missing, empty or longer
/// than `max` characters.
pub fn field(v: &Value, name: &str, max: usize) -> Option<String> {
    let s = v[name].as_str()?.trim();
    (!s.is_empty() && s.chars().count() <= max).then(|| String::from(s))
}

/// A query parameter, %-decoded.
pub fn param(req: &Request, name: &str) -> Option<String> {
    req.param(name).map(|v| decode(&v))
}

/// `%XX` → the byte (and the bytes → UTF-8, lossy).
pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn ok(v: Value) -> Response {
    Response::json(&v.to_string())
}

/// An error as JSON `{"error": why}`.
pub fn error(status: u16, why: &str) -> Response {
    let mut r = Response::json(&json!({ "error": why }).to_string());
    r.status = status;
    r
}

/// `Ok(v)` → 200 with v, `Err(why)` → 500.
pub fn answer(r: Result<Value, String>) -> Response {
    match r {
        Ok(v) => ok(v),
        Err(why) => error(500, &why),
    }
}

/// The instance's name (POD in the environment).
pub fn instance() -> String {
    let (_, env) = libsys::args::load();
    env.iter().find_map(|e| e.strip_prefix(b"POD=")).map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default()
}

/// A whole page: the site's `body` (HTML) and `script` (JS, with the
/// helpers `api(method, path, body)` → JSON and `esc(text)` → HTML).
pub fn page(title: &str, body: &str, script: &str) -> Response {
    Response::html(&format!(
        r#"<!doctype html>
<html lang="zh-Hant"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
body{{font:16px/1.6 system-ui,sans-serif;max-width:40em;margin:2em auto;padding:0 1em;color:#222;background:#fafafa}}
h1{{font-size:1.6em;margin-bottom:.2em}} .sub{{color:#777;margin-top:0}}
input,textarea,button,select{{font:inherit;padding:.3em .5em}} textarea{{width:100%;box-sizing:border-box}}
ul{{padding-left:1.2em}} li{{margin:.3em 0}} .muted{{color:#888;font-size:.9em}} .err{{color:#b00}}
.card{{background:#fff;border:1px solid #ddd;border-radius:6px;padding:.6em .9em;margin:.6em 0}}
code{{background:#eee;padding:0 4px}} a{{color:#0558b0}}
</style></head><body>
{body}
<p class="muted">這個網站是 ruos 裡的一個函式（kube 的 Function）回的；資料在後端（ruos serve）的資料庫。</p>
<script>
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}})[c]);
async function api(method, path, body) {{
  const r = await fetch(path, {{ method, headers: {{ 'Content-Type': 'application/json' }}, body: body === undefined ? undefined : JSON.stringify(body) }});
  const j = await r.json().catch(() => ({{ error: `HTTP ${{r.status}}` }}));
  if (!r.ok || j.error) throw new Error(j.error || `HTTP ${{r.status}}`);
  return j;
}}
{script}
</script>
</body></html>
"#
    ))
}
