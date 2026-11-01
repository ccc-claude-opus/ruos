//! ruos: a capability microkernel teaching OS with Docker-like pods. This
//! program is its host side: it serves the web page (with the
//! cross-origin isolation headers SharedArrayBuffer needs) and the wasm
//! kernel, keeps the image registry, and is the other end of every
//! machine's remote channel: the OS runs in the browser, this is the
//! backend on the desktop OS. A release carries all of that inside
//! (assets.rs). HTTP and WebSockets are axum on tokio.
//!
//!   ruos [web] [--port N] [--no-open]    the OS in the browser (the default)
//!   ruos serve [--port N] [--bind ADDR] [--web DIR] [--kernel FILE] [--store DIR]
//!                                        the server alone (development: run.sh, test.sh)
//!
//! Registry (a podimg store directory; objects it lists in `upstream` but
//! lacks are fetched from `--upstream URL`, upstream.rs):
//!   GET /v1/blobs/ID        object bytes; cached forever (content addressed)
//!   GET /v1/refs/NAME/TAG   the image index id
//!   GET /v1/refs            `NAME:TAG ID` lines
//! Machines:
//!   WebSocket /v1/remote?machine=NAME&session=ID   the remote channel (remote.rs)
//!   GET /v1/pods            `SESSION/KEY` lines: the writable layers kept here
//!   GET /v1/pods/SESSION/KEY/files/PATH
//! Machines' HTTP requests (curl) are made here (http.rs): not to this
//! side's own network, unless `--http-allow ADDR:PORT`; `--no-http` turns
//! them off.
//!   GET /v1/cluster         the cluster's records (kube), JSON:
//!                           {"now": MS, "fn_port": N, "records": {KEY: {"version", "updated", "value"}}}
//!   WebSocket /v1/ingress?machine=NAME&session=ID  requests from outside
//!                           for Functions, and their answers (ingress.rs)
//! The WebSockets and the ingress only take requests from this origin
//! (pages elsewhere, such as Functions' own pages, cannot use them).
//!
//! Backend services (backend.rs), each a capability in the OS: `sql:NAME`
//! (SQLite in `STORE/sql/NAME.db`, or `--sql NAME=URL`), `redis:NAME`
//! (`--redis NAME=URL`).
//!
//! Functions (kube's `expose = true`) on their own port, `--fn-port N`
//! (default: the port + 10), so their pages are another origin:
//!   ANY /fn/NAME/PATH       the Function's answer (doc/ingress.md)
//!   GET /                   the exposed Functions
//!
//! `--port 0` picks a free port; the URL is printed as
//! `ruos: listening on http://ADDR:PORT`. Data (the registry, disks,
//! writable layers) lives in `$RUOS_HOME`, else `~/.ruos`.
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::Response;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};

mod assets;
mod backend;
mod cluster;
mod http;
mod ingress;
mod id;
mod remote;
mod upstream;

static SESSIONS: AtomicU64 = AtomicU64::new(1);

/// Where the page, the kernel and the registry come from: directories
/// (development), else what the binary carries.
struct Config {
    web: Option<PathBuf>,
    kernel: Option<PathBuf>,
    store: Option<PathBuf>,
}

const USAGE: &str = "usage:
  ruos [web] [--port N] [--no-open]    the OS in the browser (the default)
  ruos serve [--port N] [--bind ADDR] [--web DIR] [--kernel FILE] [--store DIR]
             [--fn-port N] [--no-http] [--http-allow ADDR:PORT]...
             [--sql NAME=sqlite:PATH|postgres://…]... [--redis NAME=redis://HOST:PORT/]...
             [--upstream URL|none]   where images the registry lists but lacks come from";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match args.first().map(String::as_str) {
        Some("web" | "serve") => args.remove(0),
        Some("-h" | "--help") => usage(""),
        _ => String::from("web"),
    };
    let mut port = if cmd == "serve" { 8780u16 } else { 8780 };
    let mut bind = "127.0.0.1".to_string();
    let mut cfg = Config { web: None, kernel: None, store: None };
    let mut open = true;
    let mut http = http::Settings { enabled: true, allow: Vec::new() };
    let mut fn_port = None;
    let mut services = backend::Settings { sql: Vec::new(), redis: Vec::new() };
    let mut up = Some(upstream::DEFAULT.to_string());
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage(&format!("{a} needs a value")));
        match a.as_str() {
            "--port" => port = val().parse().unwrap_or_else(|_| usage("bad port")),
            "--bind" if cmd == "serve" => bind = val(),
            "--web" if cmd == "serve" => cfg.web = Some(PathBuf::from(val())),
            "--kernel" if cmd == "serve" => cfg.kernel = Some(PathBuf::from(val())),
            "--no-http" => http.enabled = false,
            "--sql" | "--redis" => {
                let v = val();
                let Some((n, u)) = v.split_once('=') else { usage(&format!("{a} NAME=URL")) };
                let l = if a == "--sql" { &mut services.sql } else { &mut services.redis };
                l.push((n.to_string(), u.to_string()));
            }
            "--upstream" => up = Some(val()).filter(|u| u != "none"),
            "--fn-port" => fn_port = Some(val().parse().unwrap_or_else(|_| usage("bad --fn-port"))),
            "--http-allow" => http.allow.push(val().parse().unwrap_or_else(|_| usage("bad --http-allow (ADDR:PORT)"))),
            "--store" if cmd == "serve" => cfg.store = Some(PathBuf::from(val())),
            "--no-open" if cmd == "web" => open = false,
            "-h" | "--help" => usage(""),
            _ => usage(&format!("unknown argument {a}")),
        }
    }
    http::configure(http);
    backend::configure(services);
    upstream::configure(up);
    let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("ruos: tokio: {e}");
        std::process::exit(1)
    });
    if cmd == "serve" {
        if cfg.web.is_none() && !assets::present() {
            cfg.web = Some(PathBuf::from("web"));
        }
        let fn_port = fn_port.unwrap_or(if port == 0 { 0 } else { port.saturating_add(10) });
        return rt.block_on(run(&bind, port, fn_port, cfg, None));
    }
    if !assets::present() {
        eprintln!("ruos: this build carries no system (a release does; from the repository use ./run.sh)");
        std::process::exit(1);
    }
    let home = home();
    let store = install(&home);
    let cfg = Config { web: None, kernel: None, store: Some(store) };
    rt.block_on(run(&bind, port, fn_port.unwrap_or(port.saturating_add(10)), cfg, Some((home, open))));
}

fn usage(msg: &str) -> ! {
    if !msg.is_empty() {
        eprintln!("ruos: {msg}");
    }
    eprintln!("{USAGE}");
    std::process::exit(2)
}

async fn bind_or_exit(bind: &str, port: u16) -> tokio::net::TcpListener {
    tokio::net::TcpListener::bind((bind, port)).await.unwrap_or_else(|e| {
        eprintln!("ruos: {bind}:{port}: {e}");
        std::process::exit(1)
    })
}

/// The server: the OS's port, and the function port (ingress.rs). `web`:
/// the OS for a user (the data directory, and whether to open a browser).
async fn run(bind: &str, port: u16, fn_port: u16, cfg: Config, web: Option<(PathBuf, bool)>) {
    let main = bind_or_exit(bind, port).await;
    let addr = main.local_addr().map(|a| a.to_string()).unwrap_or_default();
    println!("ruos: listening on http://{addr}");
    if let Some(store) = cfg.store.clone() {
        match tokio::net::TcpListener::bind((bind, fn_port)).await {
            Ok(l) => {
                let fa = l.local_addr().map(|a| a.to_string()).unwrap_or_default();
                if let Some(p) = fa.rsplit(':').next().and_then(|p| p.parse().ok()) {
                    let _ = FN_PORT.set(p);
                }
                let _ = FN_BASE.set(format!("http://{fa}"));
                println!("ruos: functions on http://{fa}/fn/");
                let app = Router::new().fallback(function_request).layer(DefaultBodyLimit::max(ingress::MAX_BODY)).with_state(Arc::new(store));
                tokio::spawn(async move {
                    let _ = axum::serve(l, app).await;
                });
            }
            Err(e) => eprintln!("ruos: functions: {bind}:{fn_port}: {e} (no function port)"),
        }
    }
    if let Some((home, open)) = web {
        let url = format!("http://{addr}/");
        println!("ruos: the OS is at {url} (Ctrl-C stops it); data in {}", home.display());
        if open {
            open_browser(&url);
        }
    }
    let _ = std::io::stdout().flush();
    let app = Router::new()
        .route("/v1/remote", get(remote_ws))
        .route("/v1/ingress", get(ingress_ws))
        .route("/v1/cluster", get(cluster_json))
        .fallback(serve_get)
        .with_state(Arc::new(cfg));
    if let Err(e) = axum::serve(main, app).await {
        eprintln!("ruos: {e}");
    }
}

static FN_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
static FN_BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// `http://ADDR:PORT` of the function port, if there is one (for kube's `get fn`).
pub fn fn_base() -> Option<String> {
    FN_BASE.get().cloned()
}

/// Headers that belong to one connection, not to the request or answer.
fn hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection" | "keep-alive" | "transfer-encoding" | "upgrade" | "te" | "trailer" | "content-length" | "proxy-connection" | "proxy-authorization"
    )
}

fn plain(status: u16, text: &str) -> Response {
    let mut r = Response::new(Body::from(text.to_string()));
    *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    r.headers_mut().insert("content-type", HeaderValue::from_static("text/plain; charset=utf-8"));
    r
}

/// The function port: `/fn/NAME/…` to a machine's kube (ingress.rs).
async fn function_request(State(store): State<Arc<PathBuf>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let path = uri.path();
    let query = uri.query().unwrap_or("");
    if path == "/" {
        let mut out = String::from("Functions here (kube apply one with expose = true):\n");
        for n in ingress::list(&store) {
            out += &format!("  /fn/{n}/\n");
        }
        return plain(200, &out);
    }
    let Some(rest) = path.strip_prefix("/fn/") else { return plain(404, "not found: Functions are at /fn/NAME/\n") };
    let (name, sub) = rest.split_once('/').map_or((rest, String::from("/")), |(n, s)| (n, format!("/{s}")));
    if !name_ok(name) {
        return plain(404, "bad function name\n");
    }
    let Some(timeout) = ingress::exposed(&store, name) else {
        return plain(404, &format!("no function {name} (or it is not exposed: expose = true)\n"));
    };
    // The HTTP envelope (doc/ingress.md).
    let mut env = format!("{method} {sub}{}{query}\n", if query.is_empty() { "" } else { "?" });
    for (k, v) in &headers {
        if !hop_by_hop(k.as_str()) {
            env += &format!("{}: {}\n", k.as_str(), v.to_str().unwrap_or(""));
        }
    }
    env.push('\n');
    let mut env = env.into_bytes();
    env.extend_from_slice(&body);
    let t0 = std::time::Instant::now();
    let (machine, answer) = match ingress::call(&store, name, timeout, &env).await {
        Ok(a) => a,
        Err(ingress::Failed::NoMachine) => {
            eprintln!("ruos: fn {name}: no machine (503)");
            return plain(503, "no machine is running: open the OS in a browser tab (or a machine) to serve Functions\n");
        }
        Err(ingress::Failed::TimedOut) => {
            eprintln!("ruos: fn {name}: no answer in time (504)");
            return plain(504, "the function did not answer in time\n");
        }
        Err(ingress::Failed::Lost) => {
            eprintln!("ruos: fn {name}: the machine went away (502)");
            return plain(502, "the machine went away before it answered\n");
        }
    };
    // `HTTP STATUS\n` + header lines + `\n` + body, else all of it is text.
    let (status, hs, body) = match answer.strip_prefix(b"HTTP ") {
        Some(rest) => {
            let end = rest.windows(2).position(|w| w == b"\n\n").map_or(rest.len(), |i| i + 2);
            let head = String::from_utf8_lossy(&rest[..end]).into_owned();
            let mut lines = head.lines();
            let status = lines.next().and_then(|s| s.trim().parse::<u16>().ok()).unwrap_or(200);
            let hs: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
            (status, hs, rest[end.min(rest.len())..].to_vec())
        }
        None => (200, vec![(String::from("Content-Type"), String::from("text/plain; charset=utf-8"))], answer),
    };
    eprintln!("ruos: fn {name} {method} {sub} → {machine}: {status} ({} ms)", t0.elapsed().as_millis());
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    if let Ok(v) = HeaderValue::from_str(&machine) {
        r.headers_mut().insert("x-ruos-node", v);
    }
    for (k, v) in hs {
        if let (false, Ok(k), Ok(v)) = (hop_by_hop(&k), HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(&v)) {
            r.headers_mut().append(k, v);
        }
    }
    r
}

/// A request from this server's own pages: no Origin, or this one; not from
/// a page of another site (or of the function port, which is the same site).
fn own_origin(h: &HeaderMap) -> bool {
    let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(String::from);
    if get("sec-fetch-site").is_some_and(|s| s != "same-origin" && s != "none") {
        return false;
    }
    // Programs that are not browsers send none (or an empty one).
    match (get("origin").filter(|o| !o.is_empty()), get("host")) {
        (None, _) => true,
        (Some(o), Some(h)) => o == format!("http://{h}") || o == format!("https://{h}"),
        (Some(_), None) => false,
    }
}

/// `$RUOS_HOME`, else `~/.ruos`.
fn home() -> PathBuf {
    let h = std::env::var_os("RUOS_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| PathBuf::from(h).join(".ruos")))
        .unwrap_or_else(|| PathBuf::from(".ruos"));
    fs::create_dir_all(&h).unwrap_or_else(|e| {
        eprintln!("ruos: {}: {e}", h.display());
        std::process::exit(1)
    });
    h
}

/// The registry the binary carries, put in `HOME/store`: objects that are
/// not there yet, and (for a new version) the refs. Pushed images stay.
fn install(home: &Path) -> PathBuf {
    let store = home.join("store");
    let stamp = store.join(".version");
    let new_version = fs::read_to_string(&stamp).ok().as_deref() != Some(env!("CARGO_PKG_VERSION"));
    for (name, data) in assets::all().iter() {
        let Some(rel) = name.strip_prefix("store/") else { continue };
        let p = store.join(rel);
        if p.exists() && !(new_version && (rel.starts_with("refs/") || rel == "upstream")) {
            continue;
        }
        if let Some(d) = p.parent() {
            let _ = fs::create_dir_all(d);
        }
        if let Err(e) = fs::write(&p, *data) {
            eprintln!("ruos: {}: {e}", p.display());
            std::process::exit(1);
        }
    }
    let _ = fs::write(&stamp, env!("CARGO_PKG_VERSION"));
    store
}

fn open_browser(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(windows) {
        std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if cmd.is_err() {
        println!("ruos: open {url} in a browser");
    }
}

/// Every response carries the cross-origin isolation headers
/// (SharedArrayBuffer + Atomics.wait need them).
fn respond(status: u16, ctype: &str, cache: &str, body: Vec<u8>) -> Response {
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let h = r.headers_mut();
    for (k, v) in [
        ("content-type", ctype),
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-embedder-policy", "require-corp"),
        ("cross-origin-resource-policy", "same-origin"),
        ("cache-control", cache),
    ] {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(k, v);
        }
    }
    r
}

type Params = Query<std::collections::HashMap<String, String>>;

/// The page, the kernel and the registry.
async fn serve_get(State(cfg): State<Arc<Config>>, method: Method, uri: Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return respond(405, "text/plain", "no-cache", b"method not allowed\n".to_vec());
    }
    let path = uri.path().to_string();
    tokio::task::spawn_blocking(move || {
        if let Some(r) = path.strip_prefix("/v1/") {
            return match registry(&cfg, r) {
                Some((b, true)) => respond(200, "application/octet-stream", "public, max-age=31536000, immutable", b),
                Some((b, false)) => respond(200, "application/octet-stream", "no-cache", b),
                None => respond(404, "text/plain", "no-cache", b"not found\n".to_vec()),
            };
        }
        match file(&cfg, &path) {
            Some((p, b)) => respond(200, content_type(&p), "no-cache", b),
            None => respond(404, "text/plain", "no-cache", b"not found\n".to_vec()),
        }
    })
    .await
    .unwrap_or_else(|_| respond(500, "text/plain", "no-cache", Vec::new()))
}

async fn cluster_json(State(cfg): State<Arc<Config>>) -> Response {
    let Some(store) = cfg.store.clone() else { return respond(404, "text/plain", "no-cache", b"no store\n".to_vec()) };
    let j = tokio::task::spawn_blocking(move || cluster::json(&store, FN_PORT.get().copied())).await.unwrap_or_default();
    respond(200, "application/json", "no-cache", j.into_bytes())
}

/// A machine's remote channel: each binary message is a store request,
/// answered in turn (remote.rs).
async fn remote_ws(
    State(cfg): State<Arc<Config>>,
    headers: HeaderMap,
    Query(q): Params,
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    if !own_origin(&headers) {
        return respond(403, "text/plain", "no-cache", b"only from this server's own pages\n".to_vec());
    }
    let (Some(store), Ok(ws)) = (cfg.store.clone(), ws) else {
        return respond(400, "text/plain", "no-cache", b"a WebSocket to a store\n".to_vec());
    };
    let machine = q.get("machine").cloned().unwrap_or_else(|| String::from("default"));
    // A machine connecting again names its session: same writable layers.
    let session = q.get("session").filter(|s| name_ok(s) && s.len() <= 64);
    let s = Arc::new(remote::Session {
        store,
        id: match session {
            Some(s) => format!("w-{s}"),
            None => format!("s{}-{}", std::process::id(), SESSIONS.fetch_add(1, Ordering::Relaxed)),
        },
        machine,
        fetched: Default::default(),
    });
    ws.max_message_size(1 << 20).on_upgrade(move |mut socket: WebSocket| async move {
        while let Some(Ok(m)) = socket.recv().await {
            match m {
                Message::Binary(b) => {
                    let s = s.clone();
                    let Ok(r) = tokio::task::spawn_blocking(move || s.handle(&b)).await else { break };
                    if socket.send(Message::Binary(r.into())).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    })
}

/// A machine's ingress: requests for its Functions go out on it, the
/// answers come back on it (ingress.rs).
async fn ingress_ws(
    headers: HeaderMap,
    Query(q): Params,
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    if !own_origin(&headers) {
        return respond(403, "text/plain", "no-cache", b"only from this server's own pages\n".to_vec());
    }
    let (Some(machine), Some(session)) = (q.get("machine").cloned(), q.get("session").cloned()) else {
        return respond(400, "text/plain", "no-cache", b"machine and session\n".to_vec());
    };
    let Ok(ws) = ws else { return respond(400, "text/plain", "no-cache", b"a WebSocket\n".to_vec()) };
    if !name_ok(&machine) || !name_ok(&session) {
        return respond(400, "text/plain", "no-cache", b"machine and session\n".to_vec());
    }
    let session = format!("w-{session}");
    ws.on_upgrade(move |socket: WebSocket| async move {
        let (conn, mut reqs) = ingress::attach(&session, &machine);
        let (mut out, mut inp) = socket.split();
        loop {
            tokio::select! {
                r = reqs.recv() => match r {
                    Some(m) => if out.send(Message::Binary(m.into())).await.is_err() { break },
                    None => break,
                },
                m = inp.next() => match m {
                    Some(Ok(Message::Binary(b))) => { ingress::answer(&b); }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    _ => {}
                },
            }
        }
        ingress::detach(&session, conn);
    })
}

fn hex_id(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn name_ok(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.') && s != "." && s != ".."
}

/// A registry request → (body, immutable).
fn registry(cfg: &Config, r: &str) -> Option<(Vec<u8>, bool)> {
    let store = cfg.store.as_ref()?;
    let parts: Vec<&str> = r.split('/').collect();
    match parts.as_slice() {
        ["blobs", id] if hex_id(id) => upstream::object(store, &id::Id::parse(id)?).and_then(|p| fs::read(p).ok()).map(|b| (b, true)),
        ["refs", name, tag] if name_ok(name) && name_ok(tag) => {
            fs::read(store.join("refs").join(name).join(tag)).ok().map(|b| (b, false))
        }
        ["pods"] => {
            let mut out = String::new();
            for s in fs::read_dir(store.join("pods")).ok()?.flatten() {
                for k in fs::read_dir(s.path()).into_iter().flatten().flatten() {
                    out += &format!("{}/{}\n", s.file_name().to_string_lossy(), k.file_name().to_string_lossy());
                }
            }
            Some((out.into_bytes(), false))
        }
        ["pods", sess, key, "files", rest @ ..] if name_ok(sess) && name_ok(key) && rest.iter().all(|p| name_ok(p)) => {
            let mut p = store.join("pods").join(sess).join(key).join("files");
            for x in rest {
                p = p.join(x);
            }
            fs::read(p).ok().map(|b| (b, false))
        }
        ["refs"] => {
            let mut out = String::new();
            let mut names: Vec<_> = fs::read_dir(store.join("refs")).ok()?.flatten().collect();
            names.sort_by_key(|e| e.file_name());
            for n in names {
                let mut tags: Vec<_> = fs::read_dir(n.path()).into_iter().flatten().flatten().collect();
                tags.sort_by_key(|e| e.file_name());
                for t in tags {
                    let id = fs::read_to_string(t.path()).unwrap_or_default();
                    out += &format!("{}:{} {}\n", n.file_name().to_string_lossy(), t.file_name().to_string_lossy(), id.trim());
                }
            }
            Some((out.into_bytes(), false))
        }
        _ => None,
    }
}

/// A URL path as a relative file name (`/` → `index.html`); None for
/// `..` and absolute components.
fn relative(path: &str) -> Option<PathBuf> {
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() || rel.ends_with('/') { format!("{rel}index.html") } else { rel.to_string() };
    let rel = PathBuf::from(rel);
    rel.components().all(|c| matches!(c, Component::Normal(_))).then_some(rel)
}

/// URL path → file on disk: `/kernel.wasm` is the kernel, the rest comes
/// from the web directory.
fn resolve(cfg: &Config, path: &str) -> Option<PathBuf> {
    if path == "/kernel.wasm" {
        if let Some(k) = &cfg.kernel {
            return Some(k.clone());
        }
    }
    let p = cfg.web.as_ref()?.join(relative(path)?);
    p.is_file().then_some(p)
}

/// URL path → (name, bytes): from disk, else what the binary carries.
fn file(cfg: &Config, path: &str) -> Option<(PathBuf, Vec<u8>)> {
    if let Some(p) = resolve(cfg, path) {
        return fs::read(&p).ok().map(|b| (p, b));
    }
    let rel = relative(path)?;
    let name = if path == "/kernel.wasm" { String::from("kernel.wasm") } else { format!("web/{}", rel.to_string_lossy()) };
    assets::get(&name).map(|b| (rel, b.to_vec()))
}

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            web: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../web")),
            kernel: Some(PathBuf::from("/k.wasm")),
            store: None,
        }
    }

    #[test]
    fn paths() {
        let c = cfg();
        assert_eq!(resolve(&c, "/kernel.wasm"), Some(PathBuf::from("/k.wasm")));
        assert!(resolve(&c, "/").unwrap().ends_with("index.html"));
        assert!(resolve(&c, "/main.js").is_some());
        assert_eq!(resolve(&c, "/../Cargo.toml"), None);
        assert_eq!(resolve(&c, "/missing.js"), None);
    }

    #[test]
    fn registry_names() {
        assert!(hex_id(&"ab".repeat(32)));
        assert!(!hex_id("../objects"));
        assert!(!name_ok(".."));
        assert!(name_ok("hello"));
        assert_eq!(registry(&cfg(), "blobs/x"), None);
    }

    #[test]
    fn types() {
        assert_eq!(content_type(Path::new("a.wasm")), "application/wasm");
        assert_eq!(content_type(Path::new("a.js")), "text/javascript; charset=utf-8");
    }
}
