//! ruos: a capability microkernel teaching OS with Docker-like pods. This
//! program is its host side: it serves the web page (with the
//! cross-origin isolation headers SharedArrayBuffer needs) and the wasm
//! kernel, keeps the image registry, is the other end of every machine's
//! remote channel, and boots the QEMU machines. A release carries all of
//! that inside (assets.rs); HTTP is `tiny_http`, the WebSocket `tungstenite`.
//!
//!   ruos [web] [--port N] [--no-open]    the OS in the browser (the default)
//!   ruos riscv64 | aarch64 [--smp N]     the OS in QEMU (qemu-system-* needed)
//!   ruos serve [--port N] [--bind ADDR] [--web DIR] [--kernel FILE] [--store DIR]
//!                                        the server alone (development: run.sh, test.sh)
//!
//! Registry (a podimg store directory):
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
//!   GET /v1/ingress/next?machine=NAME&session=ID, POST /v1/ingress/answer
//!                           requests from outside for Functions (ingress.rs)
//! The WebSockets and the ingress only take requests from this origin
//! (pages elsewhere, such as Functions' own pages, cannot use them).
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
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use tiny_http::{Header, Request, Response, Server};
use tungstenite::protocol::Role;
use tungstenite::{Message, WebSocket};

mod assets;
mod cluster;
mod http;
mod ingress;
mod id;
mod remote;

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
  ruos riscv64 | aarch64 [--smp N]     the OS in QEMU (qemu-system-riscv64 / -aarch64 needed)
  ruos serve [--port N] [--bind ADDR] [--web DIR] [--kernel FILE] [--store DIR]
             [--fn-port N] [--no-http] [--http-allow ADDR:PORT]...";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match args.first().map(String::as_str) {
        Some("web" | "serve" | "riscv64" | "aarch64") => args.remove(0),
        Some("-h" | "--help") => usage(""),
        _ => String::from("web"),
    };
    let mut port = if cmd == "serve" { 8780u16 } else { 8780 };
    let mut bind = "127.0.0.1".to_string();
    let mut cfg = Config { web: None, kernel: None, store: None };
    let (mut open, mut smp) = (true, 4u32);
    let mut http = http::Settings { enabled: true, allow: Vec::new() };
    let mut fn_port = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage(&format!("{a} needs a value")));
        match a.as_str() {
            "--port" => port = val().parse().unwrap_or_else(|_| usage("bad port")),
            "--bind" if cmd == "serve" => bind = val(),
            "--web" if cmd == "serve" => cfg.web = Some(PathBuf::from(val())),
            "--kernel" if cmd == "serve" => cfg.kernel = Some(PathBuf::from(val())),
            "--no-http" => http.enabled = false,
            "--fn-port" if cmd != "riscv64" && cmd != "aarch64" => fn_port = Some(val().parse().unwrap_or_else(|_| usage("bad --fn-port"))),
            "--http-allow" => http.allow.push(val().parse().unwrap_or_else(|_| usage("bad --http-allow (ADDR:PORT)"))),
            "--store" if cmd == "serve" => cfg.store = Some(PathBuf::from(val())),
            "--no-open" if cmd == "web" => open = false,
            "--smp" if cmd != "serve" && cmd != "web" => smp = val().parse().unwrap_or_else(|_| usage("bad --smp")),
            "-h" | "--help" => usage(""),
            _ => usage(&format!("unknown argument {a}")),
        }
    }
    http::configure(http);
    if cmd == "serve" {
        if cfg.web.is_none() && !assets::present() {
            cfg.web = Some(PathBuf::from("web"));
        }
        let server = listen(&bind, port);
        functions(&bind, fn_port.unwrap_or(if port == 0 { 0 } else { port.saturating_add(10) }), cfg.store.clone());
        return run_server(server, cfg);
    }
    if !assets::present() {
        eprintln!("ruos: this build carries no system (a release does; from the repository use ./run.sh)");
        std::process::exit(1);
    }
    let home = home();
    let store = install(&home);
    let cfg = Config { web: None, kernel: None, store: Some(store) };
    if cmd == "web" {
        let server = listen(&bind, port);
        functions(&bind, fn_port.unwrap_or(port.saturating_add(10)), cfg.store.clone());
        let url = format!("http://{}/", server.server_addr());
        println!("ruos: the OS is at {url} (Ctrl-C stops it); data in {}", home.display());
        if open {
            open_browser(&url);
        }
        return run_server(server, cfg);
    }
    // QEMU: the server on a free port, the machine reaches it as 10.0.2.100:80.
    let server = listen("127.0.0.1", 0);
    let addr = server.server_addr().to_string();
    thread::spawn(move || run_server(server, cfg));
    std::process::exit(qemu(&cmd, &home, &addr, smp));
}

fn usage(msg: &str) -> ! {
    if !msg.is_empty() {
        eprintln!("ruos: {msg}");
    }
    eprintln!("{USAGE}");
    std::process::exit(2)
}

fn listen(bind: &str, port: u16) -> Server {
    let server = Server::http((bind, port)).unwrap_or_else(|e| {
        eprintln!("ruos: {bind}:{port}: {e}");
        std::process::exit(1)
    });
    println!("ruos: listening on http://{}", server.server_addr());
    let _ = std::io::stdout().flush();
    server
}

static FN_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
static FN_BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// `http://ADDR:PORT` of the function port, if there is one (for kube's `get fn`).
pub fn fn_base() -> Option<String> {
    FN_BASE.get().cloned()
}

/// The function port: `/fn/NAME/…` to a machine's kube (ingress.rs).
fn functions(bind: &str, port: u16, store: Option<PathBuf>) {
    let Some(store) = store else { return };
    let server = match Server::http((bind, port)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ruos: functions: {bind}:{port}: {e} (no function port)");
            return;
        }
    };
    let addr = server.server_addr().to_string();
    if let Some(p) = addr.rsplit(':').next().and_then(|p| p.parse().ok()) {
        let _ = FN_PORT.set(p);
    }
    let _ = FN_BASE.set(format!("http://{addr}"));
    println!("ruos: functions on http://{addr}/fn/");
    let _ = std::io::stdout().flush();
    let store = Arc::new(store);
    thread::spawn(move || {
        for req in server.incoming_requests() {
            let store = store.clone();
            thread::spawn(move || function_request(req, &store));
        }
    });
}

/// Headers that belong to one connection, not to the request or answer.
fn hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection" | "keep-alive" | "transfer-encoding" | "upgrade" | "te" | "trailer" | "content-length" | "proxy-connection" | "proxy-authorization"
    )
}

fn plain(req: Request, status: u16, text: &str) {
    let r = Response::from_data(text.as_bytes().to_vec()).with_status_code(status).with_header(header("Content-Type", "text/plain; charset=utf-8"));
    let _ = req.respond(r);
}

fn function_request(mut req: Request, store: &Path) {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    if path == "/" {
        let mut out = String::from("Functions here (kube apply one with expose = true):\n");
        for n in ingress::list(store) {
            out += &format!("  /fn/{n}/\n");
        }
        return plain(req, 200, &out);
    }
    let Some(rest) = path.strip_prefix("/fn/") else { return plain(req, 404, "not found: Functions are at /fn/NAME/\n") };
    let (name, sub) = rest.split_once('/').map_or((rest, String::from("/")), |(n, s)| (n, format!("/{s}")));
    if !name_ok(name) {
        return plain(req, 404, "bad function name\n");
    }
    let Some(timeout) = ingress::exposed(store, name) else {
        return plain(req, 404, &format!("no function {name} (or it is not exposed: expose = true)\n"));
    };
    let mut body = Vec::new();
    let _ = req.as_reader().take(ingress::MAX_BODY as u64 + 1).read_to_end(&mut body);
    if body.len() > ingress::MAX_BODY {
        return plain(req, 413, &format!("a request is at most {} bytes\n", ingress::MAX_BODY));
    }
    // The HTTP envelope (doc/ingress.md).
    let mut env = format!("{} {sub}{}{query}\n", req.method().as_str(), if query.is_empty() { "" } else { "?" });
    for h in req.headers() {
        if !hop_by_hop(h.field.as_str().as_str()) {
            env += &format!("{}: {}\n", h.field.as_str(), h.value.as_str());
        }
    }
    env.push('\n');
    let mut env = env.into_bytes();
    env.extend_from_slice(&body);
    let t0 = std::time::Instant::now();
    let (machine, answer) = match ingress::call(store, name, timeout, &env) {
        Ok(a) => a,
        Err(ingress::Failed::NoMachine) => {
            eprintln!("ruos: fn {name}: no machine (503)");
            return plain(req, 503, "no machine is running: open the OS in a browser tab (or a machine) to serve Functions\n");
        }
        Err(ingress::Failed::TimedOut) => {
            eprintln!("ruos: fn {name}: no answer in time (504)");
            return plain(req, 504, "the function did not answer in time\n");
        }
    };
    // `HTTP STATUS\n` + header lines + `\n` + body, else all of it is text.
    let (status, headers, body) = match answer.strip_prefix(b"HTTP ") {
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
    eprintln!("ruos: fn {name} {} {sub} → {machine}: {status} ({} ms)", req.method().as_str(), t0.elapsed().as_millis());
    let mut r = Response::from_data(body).with_status_code(status).with_header(header("X-Ruos-Node", &machine));
    for (k, v) in headers {
        if !hop_by_hop(&k)
            && let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes())
        {
            r = r.with_header(h);
        }
    }
    let _ = req.respond(r);
}

/// A request from this server's own pages: no Origin, or this one; not from
/// a page of another site (or of the function port, which is the same site).
fn own_origin(req: &Request) -> bool {
    let get = |n: &'static str| req.headers().iter().find(|h| h.field.equiv(n)).map(|h| h.value.as_str().to_string());
    if get("Sec-Fetch-Site").is_some_and(|s| s != "same-origin" && s != "none") {
        return false;
    }
    // Programs that are not browsers send none (or an empty one: the native
    // machines' WebSocket client).
    match (get("Origin").filter(|o| !o.is_empty()), get("Host")) {
        (None, _) => true,
        (Some(o), Some(h)) => o == format!("http://{h}") || o == format!("https://{h}"),
        (Some(_), None) => false,
    }
}

fn run_server(server: Server, cfg: Config) {
    let cfg = Arc::new(cfg);
    for req in server.incoming_requests() {
        let cfg = cfg.clone();
        thread::spawn(move || serve(req, &cfg));
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
        if p.exists() && !(new_version && rel.starts_with("refs/")) {
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

/// Boots `arch` in QEMU with the kernel the binary carries, a 16 MiB disk
/// (kept in HOME) and the user network, which forwards 10.0.2.100:80 to
/// the server at `addr`. Returns QEMU's exit code.
fn qemu(arch: &str, home: &Path, addr: &str, smp: u32) -> i32 {
    let Some(kernel) = assets::get(&format!("kernel-{arch}.bin")) else {
        eprintln!("ruos: this build has no {arch} kernel");
        return 1;
    };
    let kfile = home.join(format!("kernel-{arch}.bin"));
    let disk = home.join(format!("disk-{arch}.img"));
    let ok = fs::write(&kfile, kernel).is_ok() && (disk.exists() || fs::write(&disk, vec![0u8; 16 << 20]).is_ok());
    if !ok {
        eprintln!("ruos: cannot write to {}", home.display());
        return 1;
    }
    let mut c = std::process::Command::new(format!("qemu-system-{arch}"));
    c.args(["-machine", "virt", "-m", "256M", "-nographic", "-smp", &smp.to_string()]);
    if arch == "riscv64" {
        c.args(["-bios", "default"]);
    } else {
        c.args(["-cpu", "cortex-a72"]);
    }
    c.arg("-kernel").arg(&kfile);
    c.args(["-global", "virtio-mmio.force-legacy=false"]);
    c.arg("-drive").arg(format!("file={},if=none,format=raw,id=hd", disk.display()));
    c.args(["-device", "virtio-blk-device,drive=hd"]);
    c.args(["-netdev", &format!("user,id=n0,guestfwd=tcp:10.0.2.100:80-tcp:{addr}"), "-device", "virtio-net-device,netdev=n0"]);
    println!("ruos: booting {arch} in QEMU (Ctrl-A x quits)");
    match c.status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("ruos: qemu-system-{arch}: {e} (is QEMU installed?)");
            1
        }
    }
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

/// Every response carries the cross-origin isolation headers
/// (SharedArrayBuffer + Atomics.wait need them).
fn respond(req: Request, status: u16, ctype: &str, cache: &str, body: Vec<u8>) {
    let r = Response::from_data(body)
        .with_status_code(status)
        .with_header(header("Content-Type", ctype))
        .with_header(header("Cross-Origin-Opener-Policy", "same-origin"))
        .with_header(header("Cross-Origin-Embedder-Policy", "require-corp"))
        .with_header(header("Cross-Origin-Resource-Policy", "same-origin"))
        .with_header(header("Cache-Control", cache));
    let _ = req.respond(r);
}

fn serve(mut req: Request, cfg: &Config) {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let path = path.split('#').next().unwrap_or("/");
    let method = req.method().as_str().to_string();
    if path.starts_with("/v1/remote") || path.starts_with("/v1/ingress") {
        if !own_origin(&req) {
            return respond(req, 403, "text/plain", "no-cache", b"only from this server's own pages\n".to_vec());
        }
    }
    if path == "/v1/ingress/answer" && method == "POST" {
        let mut body = Vec::new();
        let _ = req.as_reader().take(ingress::MAX_BODY as u64 + 4096).read_to_end(&mut body);
        let ok = ingress::answer(&body);
        return respond(req, if ok { 204 } else { 404 }, "text/plain", "no-cache", Vec::new());
    }
    if method != "GET" && method != "HEAD" {
        return respond(req, 405, "text/plain", "no-cache", b"method not allowed\n".to_vec());
    }
    if path == "/v1/remote" {
        return remote_channel(req, cfg, query);
    }
    if path == "/v1/ingress/next" {
        let q = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}="))).unwrap_or("").to_string();
        let (machine, session) = (q("machine"), q("session"));
        if !name_ok(&machine) || !name_ok(&session) {
            return respond(req, 400, "text/plain", "no-cache", b"machine and session\n".to_vec());
        }
        let session = format!("w-{session}");
        let msgs = ingress::next(&session, &machine);
        if msgs.is_empty() {
            return respond(req, 204, "text/plain", "no-cache", Vec::new());
        }
        let mut out = Vec::new();
        for m in &msgs {
            out.extend_from_slice(format!("{}\n", m.len()).as_bytes());
            out.extend_from_slice(m);
        }
        let r = Response::from_data(out).with_status_code(200).with_header(header("Content-Type", "application/octet-stream"));
        // The page went away while it waited: the requests go to the next poll.
        if req.respond(r).is_err() {
            ingress::put_back(&session, msgs);
        }
        return;
    }
    if path == "/v1/cluster" {
        let Some(store) = cfg.store.as_ref() else { return respond(req, 404, "text/plain", "no-cache", b"no store\n".to_vec()) };
        return respond(req, 200, "application/json", "no-cache", cluster::json(store, FN_PORT.get().copied()).into_bytes());
    }
    if let Some(r) = path.strip_prefix("/v1/") {
        return match registry(cfg, r) {
            Some((b, true)) => respond(req, 200, "application/octet-stream", "public, max-age=31536000, immutable", b),
            Some((b, false)) => respond(req, 200, "application/octet-stream", "no-cache", b),
            None => respond(req, 404, "text/plain", "no-cache", b"not found\n".to_vec()),
        };
    }
    match file(cfg, path) {
        Some((p, b)) => respond(req, 200, content_type(&p), "no-cache", b),
        None => respond(req, 404, "text/plain", "no-cache", b"not found\n".to_vec()),
    }
}

/// A machine's remote channel: each binary message is a store request.
fn remote_channel(req: Request, cfg: &Config, query: &str) {
    let key = req.headers().iter().find(|h| h.field.equiv("Sec-WebSocket-Key")).map(|h| h.value.to_string());
    let (Some(store), Some(key)) = (cfg.store.clone(), key) else {
        return respond(req, 400, "text/plain", "no-cache", b"a WebSocket to a store\n".to_vec());
    };
    let machine = query.split('&').find_map(|kv| kv.strip_prefix("machine=")).unwrap_or("default").to_string();
    // A machine connecting again names its session: same writable layers.
    let session = query.split('&').find_map(|kv| kv.strip_prefix("session=")).filter(|s| name_ok(s) && s.len() <= 64);
    let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
    let resp = Response::empty(101)
        .with_header(header("Upgrade", "websocket"))
        .with_header(header("Connection", "Upgrade"))
        .with_header(header("Sec-WebSocket-Accept", &accept));
    let stream = req.upgrade("websocket", resp);
    let mut ws = WebSocket::from_raw_socket(stream, Role::Server, None);
    let s = remote::Session {
        store,
        id: match session {
            Some(s) => format!("w-{s}"),
            None => format!("s{}-{}", std::process::id(), SESSIONS.fetch_add(1, Ordering::Relaxed)),
        },
        machine,
        fetched: Default::default(),
    };
    loop {
        match ws.read() {
            Ok(Message::Binary(b)) => {
                if ws.send(Message::Binary(s.handle(&b).into())).is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
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
        ["blobs", id] if hex_id(id) => fs::read(store.join("objects").join(id)).ok().map(|b| (b, true)),
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
