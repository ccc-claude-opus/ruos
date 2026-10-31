//! guestbook: a guest book (sql:guestbook).
//!   GET  /            the page
//!   GET  /api/notes   the latest 50 notes, newest first
//!   POST /api/notes   {"name": …, "text": …} → {"id": N}
#![no_std]
#![no_main]

extern crate alloc;

use webapps::*;

libsys::entry!(main);

const BODY: &str = r##"<h1>留言板</h1><p class="sub">留言存在後端的 SQLite（<code>sql:guestbook</code>）。</p>
<form id="f" class="card"><input id="name" placeholder="名字" maxlength="40" required>
<textarea id="text" rows="3" placeholder="想說的話" maxlength="500" required></textarea>
<button>留言</button> <span id="msg" class="err"></span></form><div id="notes"></div>"##;

const SCRIPT: &str = r##"
async function load() {
  const notes = await api('GET', 'api/notes');
  document.getElementById('notes').innerHTML = notes.map((n) =>
    `<div class="card"><b>${esc(n.name)}</b> <span class="muted">${esc(n.at)}</span><div>${esc(n.text)}</div></div>`).join('') || '<p class="muted">還沒有留言。</p>';
}
document.getElementById('f').onsubmit = async (e) => {
  e.preventDefault();
  try {
    await api('POST', 'api/notes', { name: document.getElementById('name').value, text: document.getElementById('text').value });
    document.getElementById('text').value = ''; document.getElementById('msg').textContent = '';
    load();
  } catch (err) { document.getElementById('msg').textContent = err.message; }
};
load();
"##;

fn main(_arg: usize) -> i64 {
    let db = Db::get("guestbook");
    let _ = db.run("CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, name TEXT NOT NULL, text TEXT NOT NULL, at TEXT DEFAULT (datetime('now')))", &[]);
    serve_http(|req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => page("留言板", BODY, SCRIPT),
        ("GET", "/api/notes") => answer(db.rows("SELECT id, name, text, at FROM notes ORDER BY id DESC LIMIT 50", &[]).map(Value::from)),
        ("POST", "/api/notes") => {
            let b = body(req);
            let (Some(name), Some(text)) = (field(&b, "name", 40), field(&b, "text", 500)) else {
                return error(400, "name (≤ 40) and text (≤ 500) are needed");
            };
            answer(db.run("INSERT INTO notes (name, text) VALUES (?1, ?2)", &[json!(name), json!(text)]).map(|r| json!({ "id": r["last_id"] })))
        }
        _ => error(404, "no such page"),
    })
}
