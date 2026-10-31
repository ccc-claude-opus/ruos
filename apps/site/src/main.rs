//! site: an example Function reached from outside (`expose = true`,
//! doc/ingress.md): a small web page and its API, at
//! http://127.0.0.1:8790/fn/site/ by default.
//!   GET  /                 the page (it calls the API below)
//!   GET  /api/hello?name=X `hello X from INSTANCE`
//!   POST /api/hit          counts a visit in kv:visits → `{"count": N, "instance": "…"}`
//!   GET  /api/sleep?s=N    answers after N seconds (to try `timeout`)
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;

use libsys::func::{Request, Response, serve_http};
use libsys::kv::Kv;
use libsys::args;

libsys::entry!(main);

const PAGE: &str = r#"<!doctype html>
<html lang="zh-Hant"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>ruos Function</title>
<style>body{font:16px/1.6 system-ui,sans-serif;max-width:32em;margin:3em auto;padding:0 1em}button{font:inherit}code{background:#eee;padding:0 4px}</style>
</head><body>
<h1>一個 ruos 函式的網頁</h1>
<p>這一頁是函式 <code>site</code> 回的：OS 跑在瀏覽器分頁裡，ruos serve 把這個請求送進其中一台機器，由 kube 交給一個 instance。</p>
<p><button id="hit">來訪 +1</button> <span id="out"></span></p>
<script>
document.getElementById('hit').onclick = async () => {
  const r = await fetch('api/hit', { method: 'POST' });
  const j = await r.json();
  document.getElementById('out').textContent = `第 ${j.count} 次（${j.instance}，在 ${r.headers.get('X-Ruos-Node')}）`;
};
</script>
</body></html>
"#;

fn main(_arg: usize) -> i64 {
    let (_, env) = args::load();
    let me = env.iter().find_map(|e| e.strip_prefix(b"POD=")).map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default();
    let visits = Kv::get_ns("visits");
    serve_http(|req: &Request| match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => Response::html(PAGE),
        ("GET", "/api/hello") => Response::text(200, &format!("hello {} from {me}\n", req.param("name").unwrap_or_else(|| String::from("there")))),
        ("POST", "/api/hit") => {
            let Some(kv) = visits else { return Response::text(500, "no kv:visits\n") };
            let n = kv.get("count").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) + 1;
            let _ = kv.put("count", &format!("{n}"));
            Response::json(&format!("{{\"count\": {n}, \"instance\": \"{me}\"}}"))
        }
        ("GET", "/api/sleep") => {
            let s = req.param("s").and_then(|s| s.parse::<u64>().ok()).unwrap_or(1);
            libsys::sleep_ms(s * 1000);
            Response::text(200, &format!("slept {s} s\n"))
        }
        // A plain `fn call site TEXT`.
        ("CALL", _) => Response::text(200, &format!("site ({me}) got {}\n", String::from_utf8_lossy(&req.body))),
        _ => Response::text(404, &format!("no {} {}\n", req.method, req.path)),
    })
}
