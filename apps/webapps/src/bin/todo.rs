//! todo: a to-do list (sql:todo).
//!   GET    /               the page
//!   GET    /api/todos      all items: open ones first
//!   POST   /api/todos      {"title": …} → {"id": N}
//!   POST   /api/todos/ID   {"done": true|false}
//!   DELETE /api/todos/ID
#![no_std]
#![no_main]

extern crate alloc;

use webapps::*;

libsys::entry!(main);

const BODY: &str = r##"<h1>待辦清單</h1><p class="sub">存在後端的 SQLite（<code>sql:todo</code>）：換一個分頁、重開機器都還在。</p>
<form id="f" class="card"><input id="title" placeholder="要做的事" maxlength="100" required> <button>加入</button> <span id="msg" class="err"></span></form>
<ul id="list"></ul><p id="left" class="muted"></p>"##;

const SCRIPT: &str = r##"
async function load() {
  const items = await api('GET', 'api/todos');
  document.getElementById('list').innerHTML = items.map((t) =>
    `<li><label><input type="checkbox" data-id="${t.id}" ${t.done ? 'checked' : ''}> ${t.done ? `<s>${esc(t.title)}</s>` : esc(t.title)}</label>
     <button data-del="${t.id}" title="刪除">✕</button></li>`).join('');
  document.getElementById('left').textContent = `還有 ${items.filter((t) => !t.done).length} 件，共 ${items.length} 件`;
}
document.getElementById('list').onclick = async (e) => {
  const id = e.target.dataset.id, del = e.target.dataset.del;
  if (id) await api('POST', `api/todos/${id}`, { done: e.target.checked });
  if (del) await api('DELETE', `api/todos/${del}`);
  if (id || del) load();
};
document.getElementById('f').onsubmit = async (e) => {
  e.preventDefault();
  try { await api('POST', 'api/todos', { title: document.getElementById('title').value }); document.getElementById('title').value = ''; load(); }
  catch (err) { document.getElementById('msg').textContent = err.message; }
};
load();
"##;

fn main(_arg: usize) -> i64 {
    let db = Db::get("todo");
    let _ = db.run("CREATE TABLE IF NOT EXISTS todos (id INTEGER PRIMARY KEY, title TEXT NOT NULL, done INTEGER NOT NULL DEFAULT 0)", &[]);
    serve_http(|req| {
        let id = req.path.strip_prefix("/api/todos/").and_then(|s| s.parse::<i64>().ok());
        match (req.method.as_str(), req.path.as_str(), id) {
            ("GET", "/", _) => page("待辦清單", BODY, SCRIPT),
            ("GET", "/api/todos", _) => answer(db.rows("SELECT id, title, done FROM todos ORDER BY done, id", &[]).map(Value::from)),
            ("POST", "/api/todos", _) => match field(&body(req), "title", 100) {
                Some(t) => answer(db.run("INSERT INTO todos (title) VALUES (?1)", &[json!(t)]).map(|r| json!({ "id": r["last_id"] }))),
                None => error(400, "title (≤ 100) is needed"),
            },
            ("POST", _, Some(id)) => {
                let done = body(req)["done"].as_bool().unwrap_or(true);
                answer(db.run("UPDATE todos SET done = ?1 WHERE id = ?2", &[json!(done), json!(id)]).map(|r| json!({ "changed": r["changed"] })))
            }
            ("DELETE", _, Some(id)) => answer(db.run("DELETE FROM todos WHERE id = ?1", &[json!(id)]).map(|r| json!({ "changed": r["changed"] }))),
            _ => error(404, "no such page"),
        }
    })
}
