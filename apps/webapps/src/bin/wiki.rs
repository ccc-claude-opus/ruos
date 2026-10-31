//! wiki: a wiki (sql:wiki); every save is kept as a revision.
//!   GET /                       the page (`#NAME` picks a wiki page)
//!   GET /api/pages              the pages, newest change first
//!   GET /api/page?name=NAME     {"name", "body", "updated", "revisions"} (404: none yet)
//!   PUT /api/page?name=NAME     {"body": …}
//!   GET /api/history?name=NAME  its revisions, newest first
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;

use webapps::*;

libsys::entry!(main);

const BODY: &str = r##"<h1>wiki</h1><p class="sub">頁面與每一次修改存在後端的 SQLite（<code>sql:wiki</code>）。<code>[[頁名]]</code> 是連結，<code># </code> 開頭是標題。</p>
<p><a href="#首頁">首頁</a> · <a href="#" id="all">所有頁面</a></p>
<div class="card"><h2 id="title"></h2><div id="view"></div><p id="meta" class="muted"></p><button id="edit">編輯</button></div>
<form id="f" class="card" hidden><textarea id="body" rows="10"></textarea><button>儲存</button> <button type="button" id="cancel">取消</button> <span id="msg" class="err"></span></form>"##;

const SCRIPT: &str = r##"
const $ = (id) => document.getElementById(id);
const name = () => decodeURIComponent(location.hash.slice(1)) || '首頁';
const render = (text) => text.split('\n').map((l) =>
  l.startsWith('# ') ? `<h3>${esc(l.slice(2))}</h3>` : `<p>${esc(l)}</p>`).join('')
  .replace(/\[\[([^\]]{1,60})\]\]/g, (_, n) => `<a href="#${encodeURIComponent(n)}">${n}</a>`);
async function show() {
  $('f').hidden = true;
  $('title').textContent = name();
  const r = await fetch(`api/page?name=${encodeURIComponent(name())}`);
  const p = r.ok ? await r.json() : null;
  $('view').innerHTML = p ? render(p.body) : '<p class="muted">這一頁還沒有內容，按「編輯」寫一點吧。</p>';
  $('body').value = p ? p.body : '';
  $('meta').textContent = p ? `第 ${p.revisions} 版，${p.updated}` : '';
}
$('all').onclick = async (e) => {
  e.preventDefault();
  const pages = await api('GET', 'api/pages');
  $('title').textContent = '所有頁面'; $('meta').textContent = '';
  $('view').innerHTML = '<ul>' + pages.map((p) => `<li><a href="#${encodeURIComponent(p.name)}">${esc(p.name)}</a> <span class="muted">${esc(p.updated)}</span></li>`).join('') + '</ul>';
};
$('edit').onclick = () => { $('f').hidden = false; $('body').focus(); };
$('cancel').onclick = () => { $('f').hidden = true; };
$('f').onsubmit = async (e) => {
  e.preventDefault();
  try { await api('PUT', `api/page?name=${encodeURIComponent(name())}`, { body: $('body').value }); $('msg').textContent = ''; show(); }
  catch (err) { $('msg').textContent = err.message; }
};
window.onhashchange = show;
show();
"##;

fn main(_arg: usize) -> i64 {
    let db = Db::get("wiki");
    let _ = db.run("CREATE TABLE IF NOT EXISTS revisions (id INTEGER PRIMARY KEY, name TEXT NOT NULL, body TEXT NOT NULL, at TEXT DEFAULT (datetime('now')))", &[]);
    // A page = its newest revision.
    let _ = db.run(
        "CREATE VIEW IF NOT EXISTS pages AS SELECT name, body, at AS updated, (SELECT COUNT(*) FROM revisions r2 WHERE r2.name = r.name) AS revisions \
         FROM revisions r WHERE id = (SELECT MAX(id) FROM revisions r3 WHERE r3.name = r.name)",
        &[],
    );
    serve_http(|req| {
        let name: Option<String> = param(req, "name").map(|n| String::from(n.trim())).filter(|n| !n.is_empty() && n.chars().count() <= 60);
        match (req.method.as_str(), req.path.as_str(), name) {
            ("GET", "/", _) => page("wiki", BODY, SCRIPT),
            ("GET", "/api/pages", _) => answer(db.rows("SELECT name, updated, revisions FROM pages ORDER BY updated DESC, name", &[]).map(Value::from)),
            ("GET", "/api/page", Some(n)) => match db.row("SELECT name, body, updated, revisions FROM pages WHERE name = ?1", &[json!(n)]) {
                Ok(Some(p)) => ok(p),
                Ok(None) => error(404, "no such page yet"),
                Err(why) => error(500, &why),
            },
            ("PUT", "/api/page", Some(n)) => match body(req)["body"].as_str() {
                Some(b) if b.len() <= 20000 => {
                    answer(db.run("INSERT INTO revisions (name, body) VALUES (?1, ?2)", &[json!(n), json!(b)]).map(|r| json!({ "revision": r["last_id"] })))
                }
                _ => error(400, "body (≤ 20000 bytes) is needed"),
            },
            ("GET", "/api/history", Some(n)) => answer(db.rows("SELECT id, at, length(body) AS bytes FROM revisions WHERE name = ?1 ORDER BY id DESC", &[json!(n)]).map(Value::from)),
            (_, "/api/page" | "/api/history", None) => error(400, "name (≤ 60) is needed"),
            _ => error(404, "no such page"),
        }
    })
}
