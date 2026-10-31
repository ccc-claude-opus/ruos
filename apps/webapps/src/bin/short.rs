//! short: short URLs (sql:short).
//!   GET  /            the page
//!   POST /api/links   {"url": "https://…"} → {"code": "aB3xY9"}
//!   GET  /api/links   the latest 20 links with their hits
//!   GET  /s/CODE      302 to the link's URL (a hit is counted)
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;

use webapps::*;

libsys::entry!(main);

const BODY: &str = r##"<h1>短網址</h1><p class="sub">對照表存在後端的 SQLite（<code>sql:short</code>）；點短網址時，函式回 302 轉過去並記一次。</p>
<form id="f" class="card"><input id="url" type="url" placeholder="https://…" size="40" required> <button>縮短</button>
<div id="made"></div><span id="msg" class="err"></span></form><ul id="list"></ul>"##;

const SCRIPT: &str = r##"
const full = (code) => new URL(`s/${code}`, location.href).href;
async function load() {
  const links = await api('GET', 'api/links');
  document.getElementById('list').innerHTML = links.map((l) =>
    `<li><a href="s/${esc(l.code)}">${esc(full(l.code))}</a> → <span class="muted">${esc(l.url)}</span>（${l.hits} 次）</li>`).join('');
}
document.getElementById('f').onsubmit = async (e) => {
  e.preventDefault();
  try {
    const r = await api('POST', 'api/links', { url: document.getElementById('url').value });
    document.getElementById('made').innerHTML = `<p>短網址：<a href="s/${esc(r.code)}">${esc(full(r.code))}</a></p>`;
    document.getElementById('msg').textContent = '';
    load();
  } catch (err) { document.getElementById('msg').textContent = err.message; }
};
load();
"##;

const DIGITS: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

fn new_code() -> String {
    let mut r = [0u8; 6];
    libsys::random(&mut r);
    r.iter().map(|b| DIGITS[*b as usize % DIGITS.len()] as char).collect()
}

fn main(_arg: usize) -> i64 {
    let db = Db::get("short");
    let _ = db.run("CREATE TABLE IF NOT EXISTS links (code TEXT PRIMARY KEY, url TEXT NOT NULL, hits INTEGER NOT NULL DEFAULT 0, at TEXT DEFAULT (datetime('now')))", &[]);
    serve_http(|req| {
        if let (Some(code), "GET") = (req.path.strip_prefix("/s/"), req.method.as_str()) {
            return match db.row("SELECT url FROM links WHERE code = ?1", &[json!(code)]) {
                Ok(Some(l)) => {
                    let _ = db.run("UPDATE links SET hits = hits + 1 WHERE code = ?1", &[json!(code)]);
                    let mut r = Response::text(302, "");
                    r.headers.push((String::from("Location"), String::from(l["url"].as_str().unwrap_or("/"))));
                    r
                }
                Ok(None) => Response::text(404, "no such short URL\n"),
                Err(why) => error(500, &why),
            };
        }
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") => page("短網址", BODY, SCRIPT),
            ("GET", "/api/links") => answer(db.rows("SELECT code, url, hits FROM links ORDER BY at DESC, rowid DESC LIMIT 20", &[]).map(Value::from)),
            ("POST", "/api/links") => {
                let Some(url) = field(&body(req), "url", 2000).filter(|u| u.starts_with("http://") || u.starts_with("https://")) else {
                    return error(400, "url (http:// or https://) is needed");
                };
                // The same URL keeps its code.
                if let Ok(Some(l)) = db.row("SELECT code FROM links WHERE url = ?1", &[json!(url)]) {
                    return ok(json!({ "code": l["code"] }));
                }
                let mut last = String::new();
                for _ in 0..5 {
                    let code = new_code();
                    match db.run("INSERT INTO links (code, url) VALUES (?1, ?2)", &[json!(code), json!(url)]) {
                        Ok(_) => return ok(json!({ "code": code })),
                        Err(why) => last = why,
                    }
                }
                error(500, &last)
            }
            _ => error(404, "no such page"),
        }
    })
}
