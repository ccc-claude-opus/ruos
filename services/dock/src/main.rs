//! dock: runs images as pods. A pod is a task started from an image with
//! its own quota (a child of `quota` in dock's namespace) and namespace
//! (a console client, a store client, and `fs`: its own overlay of the
//! image with a writable layer). Clients send text commands (libsys::dock).
//! A pod may carry labels and live in a space (a child quota that its own
//! quota comes out of); a client that sends `watch` is told when pods start
//! and end (kube uses both).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::console::Console;
use libsys::dock::parse_size;
use libsys::fs::Fs;
use libsys::wire::encode;
use libsys::ns::{self, Namespace};
use libsys::store::Store;
use libsys::{Channel, Error, Handle, Port, Quota, signal};

libsys::entry!(main);

/// dock → overlay, first: [CONFIG, key, image].
const CONFIG: u8 = 21;

/// `name` → `name:latest`.
fn objstore_ref(name: &str) -> String {
    if name.contains(':') { String::from(name) } else { format!("{name}:latest") }
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Running,
    Exited(i64),
    Stopped,
}

struct Pod {
    id: usize,
    name: String,
    image: String,
    task: Handle,
    quota: Quota,
    /// The pod's overlay (control channel).
    fs: Fs,
    /// Our end of the pod's `log` channel, and what came through it.
    log: Channel,
    output: Vec<u8>,
    /// `key=value`.
    labels: Vec<String>,
    /// Output is kept for `logs` but not shown on the console.
    quiet: bool,
    removed: bool,
    state: State,
    /// Clients waiting for the pod to end.
    waiters: Vec<usize>,
}

/// Port keys: clients are 0.., pods POD_KEY + index, their output LOG_KEY + index.
const POD_KEY: u64 = 1 << 32;
const LOG_KEY: u64 = 1 << 33;
/// Output kept per pod.
const MAX_OUTPUT: usize = 64 * 1024;

struct Dock {
    port: Port,
    store: Store,
    console: Console,
    quota: Quota,
    clients: Vec<Option<Channel>>,
    pods: Vec<Pod>,
    /// Clients told about pods starting and ending.
    watchers: Vec<usize>,
    /// Named quotas pods can be put in.
    spaces: Vec<(String, Quota)>,
}

impl Dock {
    fn add_client(&mut self, c: Channel) {
        self.clients.push(Some(c));
        let _ = self.port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, self.clients.len() as u64 - 1);
    }

    fn reply(&self, client: usize, text: &str) {
        if let Some(c) = self.clients[client] {
            let _ = c.write(text.as_bytes(), &[]);
        }
    }

    /// Tells the watchers: `event start|exit|stop|rm ID NAME [CODE]`.
    fn notify(&mut self, what: &str, k: usize) {
        let p = &self.pods[k];
        let msg = match p.state {
            State::Exited(c) if what == "exit" => format!("event exit {} {} {c}", p.id, p.name),
            _ => format!("event {what} {} {}", p.id, p.name),
        };
        self.watchers.retain(|&w| self.clients[w].is_some_and(|c| c.write(msg.as_bytes(), &[]).is_ok()));
    }

    fn find(&self, key: &str) -> Option<usize> {
        self.pods.iter().position(|p| !p.removed && (p.name == key || format!("{}", p.id) == key))
    }

    /// A pod printed something: keep it and show it.
    fn pod_output(&mut self, k: usize) {
        let mut buf = alloc::vec![0u8; libsys::MAX_MSG_BYTES];
        loop {
            match self.pods[k].log.read(&mut buf, &mut []) {
                Ok((n, _)) => {
                    if !self.pods[k].quiet {
                        let _ = self.console.write(&buf[..n]);
                    }
                    let out = &mut self.pods[k].output;
                    out.extend_from_slice(&buf[..n]);
                    if out.len() > MAX_OUTPUT {
                        out.drain(..out.len() - MAX_OUTPUT);
                    }
                }
                Err(Error::ShouldWait) => {
                    let _ = self.port.bind(self.pods[k].log.0, signal::READABLE | signal::PEER_CLOSED, LOG_KEY + k as u64);
                    return;
                }
                Err(_) => return,
            }
        }
    }

    /// An image from a Podfile in pod `k`'s file system: its `from` image
    /// with a layer of the files it names (from the same file system).
    fn build(&self, k: usize, podfile: &str) -> Result<String, String> {
        let fs = self.pods[k].fs;
        let e = |what: &str, err: Error| format!("{what}: {err:?}");
        let text = fs.read(podfile).map_err(|err| e(podfile, err))?;
        let pf = objstore::podfile::parse(core::str::from_utf8(&text).unwrap_or("")).map_err(|m| m)?;
        let Some(from) = &pf.from else { return Err(String::from("a Podfile in a pod needs `from`")) };
        if !pf.programs.is_empty() {
            return Err(String::from("[programs] are built on the host (podimg)"));
        }
        let mut files = Vec::new();
        for (dst, src) in &pf.files {
            let data = fs.read(src).map_err(|err| e(src, err))?;
            let id = libsys::image::put(&self.store, &data).map_err(|err| e("store", err))?;
            files.push((dst.clone(), Some((id, data.len() as u64))));
        }
        let layer = libsys::image::layer(&self.store, &files).map_err(|err| e("layer", err))?;
        let r = format!("{}:{}", pf.name, pf.tag);
        let env: Vec<(String, String)> = pf.env.clone().into_iter().collect();
        let entry = (!pf.entry.is_empty()).then_some(pf.entry.as_str());
        let id = libsys::image::derive(&self.store, from, &r, layer, entry, &env).map_err(|err| e("image", err))?;
        Ok(format!("ok built {r} {id} {} {from}", files.len()))
    }

    fn run(&mut self, args: &[&str]) -> Result<(usize, bool), String> {
        let (mut name, mut mem, mut tasks, mut detach, mut image) = (None, 32 << 20, 8, false, None);
        let (mut labels, mut quiet, mut space) = (Vec::new(), false, None);
        let mut i = 0;
        while i < args.len() {
            let val = || args.get(i + 1).copied().ok_or_else(|| format!("{} needs a value", args[i]));
            match args[i] {
                "-d" | "--detach" => detach = true,
                "--name" => {
                    name = Some(String::from(val()?));
                    i += 1;
                }
                "--mem" => {
                    mem = parse_size(val()?).ok_or("bad --mem")?;
                    i += 1;
                }
                "--tasks" => {
                    tasks = val()?.parse().map_err(|_| "bad --tasks")?;
                    i += 1;
                }
                "--label" => {
                    let l = val()?;
                    if !l.contains('=') {
                        return Err(format!("bad label {l} (KEY=VALUE)"));
                    }
                    labels.push(String::from(l));
                    i += 1;
                }
                "--space" => {
                    let s = val()?;
                    space = Some(self.spaces.iter().find(|(n, _)| n == s).map(|(_, q)| *q).ok_or_else(|| format!("no space {s}"))?);
                    i += 1;
                }
                "--quiet" => quiet = true,
                a if a.starts_with('-') => return Err(format!("unknown option {a}")),
                a => {
                    image = Some(a);
                    i += 1;
                    break;
                }
            }
            i += 1;
        }
        // The rest are the program's arguments (argv[0] is the entry's path).
        let image = image.ok_or("no image")?;
        let id = self.pods.len() + 1;
        let name = name.unwrap_or_else(|| format!("{image}-{id}"));
        if self.find(&name).is_some() {
            return Err(format!("a pod is already called {name}"));
        }
        let e = |what: &'static str| move |err: Error| format!("{what}: {err:?}");
        if self.store.index(image).is_err() {
            return Err(format!("no image {image}"));
        }
        let quota = space.unwrap_or(self.quota).child(mem, tasks, 2 * tasks).map_err(e("quota"))?;
        let (mine, theirs) = Channel::create().map_err(e("channel"))?;
        self.console.connect(theirs.0).map_err(e("console"))?;
        let st = self.store.connect().map_err(e("store"))?;
        // The pod's file system: an overlay of the image (charged to dock).
        let (fs, fs_srv) = Channel::create().map_err(e("channel"))?;
        let (ctl, ctl_srv) = Channel::create().map_err(e("channel"))?;
        // Writable layers outlive a boot (on the server): keys name the boot too.
        let key = format!("b{}-pod{id}", self.store.boot().unwrap_or(0));
        ctl.write(&encode(CONFIG, &[key.as_bytes(), image.as_bytes()]), &[]).map_err(e("overlay"))?;
        let ovs = self.store.connect().map_err(e("store"))?;
        let ons = Namespace::new().add("store", ovs.0.0).add("fs", fs_srv.0).add("control", ctl_srv.0);
        self.store.spawn("overlay", &ons, None).map_err(e("overlay"))?;
        let (log, log_pod) = Channel::create().map_err(e("channel"))?;
        let ns = Namespace::new().add("console", mine.0).add("store", st.0.0).add("fs", fs.0).add("log", log_pod.0);
        let man = libsys::image::manifest(&self.store, image).ok();
        let entry = man.as_ref().map(|m| m.entry.clone()).unwrap_or_else(|| String::from(image));
        let argv: Vec<&str> = core::iter::once(entry.as_str()).chain(args[i..].iter().copied()).collect();
        let env = man.map(|m| m.env).unwrap_or_default();
        let ns = libsys::args::add(ns, &argv, &env).map_err(e("args"))?;
        let task = match self.store.spawn(image, &ns, Some(quota.0)) {
            Ok(t) => t,
            Err(err) => {
                let _ = quota.0.close();
                return Err(match err {
                    Error::NotFound => format!("no image {image}"),
                    err => format!("{image}: {err:?}"),
                });
            }
        };
        let k = self.pods.len();
        let _ = self.port.bind(task, signal::TERMINATED, POD_KEY + k as u64);
        let _ = self.port.bind(log.0, signal::READABLE | signal::PEER_CLOSED, LOG_KEY + k as u64);
        self.pods.push(Pod {
            id,
            name,
            image: String::from(image),
            task,
            quota,
            fs: Fs(ctl),
            log,
            output: Vec::new(),
            labels,
            quiet,
            removed: false,
            state: State::Running,
            waiters: Vec::new(),
        });
        self.notify("start", k);
        Ok((k, detach))
    }

    fn ps(&self) -> String {
        let mut out = format!("{:<4}{:<10}{:<10}{:<12}{:<16}{}\n", "ID", "NAME", "IMAGE", "STATUS", "MEMORY", "TASKS");
        for p in self.pods.iter().filter(|p| !p.removed) {
            let st = match p.state {
                State::Running => String::from("running"),
                State::Exited(c) => format!("exited {c}"),
                State::Stopped => String::from("stopped"),
            };
            let q = p.quota.info().unwrap_or_default();
            let mib = |b: u64| b as f64 / (1 << 20) as f64;
            let mem = format!("{:.1}/{:.1} MiB", mib(q.mem_used), mib(q.mem_limit));
            out += &format!("{:<4}{:<10}{:<10}{:<12}{:<16}{}/{}\n", p.id, p.name, p.image, st, mem, q.tasks_used, q.tasks_limit);
        }
        out
    }

    /// One line per pod for programs: `ID NAME running|exited:CODE|stopped LABELS`.
    fn pods(&self) -> String {
        let mut out = String::new();
        for p in self.pods.iter().filter(|p| !p.removed) {
            let st = match p.state {
                State::Running => String::from("running"),
                State::Exited(c) => format!("exited:{c}"),
                State::Stopped => String::from("stopped"),
            };
            let labels = if p.labels.is_empty() { String::from("-") } else { p.labels.join(",") };
            out += &format!("{} {} {st} {labels}\n", p.id, p.name);
        }
        out
    }

    fn spaces(&self) -> String {
        let mut out = format!("{:<10}{:<18}{}\n", "SPACE", "MEMORY", "TASKS");
        for (name, q) in core::iter::once((&String::from("default"), &self.quota)).chain(self.spaces.iter().map(|(n, q)| (n, q))) {
            let q = q.info().unwrap_or_default();
            let mib = |b: u64| b as f64 / (1 << 20) as f64;
            out += &format!("{:<10}{:<18}{}/{}\n", name, format!("{:.1}/{:.1} MiB", mib(q.mem_used), mib(q.mem_limit)), q.tasks_used, q.tasks_limit);
        }
        out
    }

    fn command(&mut self, client: usize, line: &str) {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["run", args @ ..] => match self.run(args) {
                Ok((k, _)) => {
                    let p = &self.pods[k];
                    self.reply(client, &format!("ok {} {}", p.id, p.name));
                }
                Err(m) => self.reply(client, &format!("error {m}")),
            },
            ["ps"] => self.reply(client, &self.ps()),
            ["pods"] => self.reply(client, &self.pods()),
            ["watch"] => {
                self.watchers.push(client);
                self.reply(client, "ok watching");
            }
            ["spaces"] => self.reply(client, &self.spaces()),
            ["space", name, mem, tasks] => {
                let r = (|| {
                    if *name == "default" || self.spaces.iter().any(|(n, _)| n == name) {
                        return Err(format!("space {name} exists"));
                    }
                    let mem = parse_size(mem).ok_or("bad size")?;
                    let tasks: u64 = tasks.parse().map_err(|_| "bad task count")?;
                    let q = self.quota.child(mem, tasks, 2 * tasks).map_err(|e| format!("quota: {e:?}"))?;
                    self.spaces.push((String::from(*name), q));
                    Ok(())
                })();
                match r {
                    Ok(()) => self.reply(client, &format!("ok space {name}")),
                    Err(m) => self.reply(client, &format!("error {m}")),
                }
            }
            ["stop", key] => match self.find(key) {
                Some(k) if self.pods[k].state == State::Running => {
                    let _ = self.pods[k].task.kill();
                    self.pods[k].state = State::Stopped;
                    self.reply(client, &format!("ok stopped {}", self.pods[k].name));
                }
                Some(k) => self.reply(client, &format!("error {} is not running", self.pods[k].name)),
                None => self.reply(client, &format!("error no pod {key}")),
            },
            ["wait", key] => match self.find(key) {
                Some(k) => match self.pods[k].state {
                    State::Running => self.pods[k].waiters.push(client),
                    State::Exited(c) => self.reply(client, &format!("exit {c}")),
                    State::Stopped => self.reply(client, "stopped"),
                },
                None => self.reply(client, &format!("error no pod {key}")),
            },
            ["commit", key, image] => match self.find(key) {
                Some(k) => match self.pods[k].fs.commit(&objstore_ref(image)) {
                    Ok(r) => self.reply(client, &format!("ok committed {} {}", objstore_ref(image), r)),
                    Err(err) => self.reply(client, &format!("error commit: {err:?}")),
                },
                None => self.reply(client, &format!("error no pod {key}")),
            },
            ["push", image] => match self.store.push(image) {
                Ok(Ok(m)) => self.reply(client, &format!("ok {m}")),
                Ok(Err(why)) => self.reply(client, &format!("error {why}")),
                Err(err) => self.reply(client, &format!("error push: {err:?}")),
            },
            ["logs", key] => match self.find(key) {
                Some(k) => {
                    let p = &self.pods[k];
                    let mut out = String::new();
                    for l in core::str::from_utf8(&p.output).unwrap_or("").lines() {
                        out += &format!("[{}] {}\n", p.name, l);
                    }
                    self.reply(client, &out);
                }
                None => self.reply(client, &format!("error no pod {key}")),
            },
            ["rm", key] => match self.find(key) {
                Some(k) => {
                    let p = &mut self.pods[k];
                    if p.state == State::Running {
                        let _ = p.task.kill();
                    }
                    let _ = p.task.close();
                    let _ = p.quota.0.close();
                    let _ = p.fs.0.close();
                    let _ = p.log.close();
                    p.removed = true;
                    let n = p.name.clone();
                    self.notify("rm", k);
                    self.reply(client, &format!("ok removed {n}"));
                }
                None => self.reply(client, &format!("error no pod {key}")),
            },
            ["build", key, podfile] => match self.find(key) {
                Some(k) => match self.build(k, podfile) {
                    Ok(r) => self.reply(client, &r),
                    Err(m) => self.reply(client, &format!("error build: {m}")),
                },
                None => self.reply(client, &format!("error no pod {key}")),
            },
            ["write", key, path, ..] => match self.find(key) {
                // The rest of the line is the text (\n, \t and \\ escapes).
                Some(k) => {
                    let text = line.splitn(4, ' ').nth(3).unwrap_or("");
                    let mut out = String::new();
                    let mut esc = false;
                    for c in text.chars() {
                        match (esc, c) {
                            (true, 'n') => out.push('\n'),
                            (true, 't') => out.push('\t'),
                            (true, c) => out.push(c),
                            (false, '\\') => {
                                esc = true;
                                continue;
                            }
                            (false, c) => out.push(c),
                        }
                        esc = false;
                    }
                    match self.pods[k].fs.write(path, out.as_bytes()) {
                        Ok(()) => self.reply(client, "ok"),
                        Err(err) => self.reply(client, &format!("error write: {err:?}")),
                    }
                }
                None => self.reply(client, &format!("error no pod {key}")),
            },
            // A look inside a pod: its file system as the pod sees it.
            ["ls", key, dir @ ..] if dir.len() <= 1 => match self.find(key) {
                Some(k) => match self.pods[k].fs.list(dir.first().copied().unwrap_or("/")) {
                    Ok(es) => {
                        let mut out = String::new();
                        for e in es {
                            out += &format!("{} {:>8}  {}\n", if e.dir { "dir " } else { "file" }, e.size, e.name);
                        }
                        self.reply(client, &out)
                    }
                    Err(err) => self.reply(client, &format!("error ls: {err:?}")),
                },
                None => self.reply(client, &format!("error no pod {key}")),
            },
            ["cat", key, path] => match self.find(key) {
                Some(k) => match self.pods[k].fs.read(path) {
                    Ok(b) if b.len() > 16384 || b.contains(&0) => self.reply(client, &format!("({} bytes, not shown)\n", b.len())),
                    Ok(b) => self.reply(client, &String::from_utf8_lossy(&b)),
                    Err(err) => self.reply(client, &format!("error cat: {err:?}")),
                },
                None => self.reply(client, &format!("error no pod {key}")),
            },
            _ => self.reply(client, "error commands: run, ps, stop, wait, commit, push, logs, rm, build, write, ls, cat, pods, watch, space, spaces"),
        }
    }

    fn pod_ended(&mut self, k: usize) {
        if self.pods[k].removed {
            return;
        }
        let code = self.pods[k].task.wait().unwrap_or(-1);
        if self.pods[k].state == State::Running {
            self.pods[k].state = State::Exited(code);
        }
        let msg = match self.pods[k].state {
            State::Exited(c) => format!("exit {c}"),
            _ => String::from("stopped"),
        };
        for w in core::mem::take(&mut self.pods[k].waiters) {
            self.reply(w, &msg);
        }
        self.notify(if msg == "stopped" { "stop" } else { "exit" }, k);
    }
}

fn main(_arg: usize) -> i64 {
    let (Some(store), Some(console), Some(quota)) = (ns::get("store"), ns::get("console"), ns::get("quota")) else {
        return 1;
    };
    let Ok(port) = Port::create() else { return 2 };
    let mut d = Dock {
        port,
        store: Store(Channel(store)),
        console: Console(Channel(console)),
        quota: Quota(quota),
        clients: Vec::new(),
        pods: Vec::new(),
        watchers: Vec::new(),
        spaces: Vec::new(),
    };
    if let Some(c) = ns::get("client") {
        d.add_client(Channel(c));
    }
    loop {
        let Ok(p) = d.port.wait(u64::MAX) else { return 3 };
        if p.key >= LOG_KEY {
            d.pod_output((p.key - LOG_KEY) as usize);
            continue;
        }
        if p.key >= POD_KEY {
            d.pod_ended((p.key - POD_KEY) as usize);
            continue;
        }
        let i = p.key as usize;
        let Some(c) = d.clients[i] else { continue };
        let mut buf = [0u8; 1024];
        let mut hs = [Handle(0); 1];
        loop {
            match c.read(&mut buf, &mut hs) {
                Ok((n, nh)) => {
                    // A message with a channel: another client.
                    if nh == 1 {
                        d.add_client(Channel(hs[0]));
                        continue;
                    }
                    let line = String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                    d.command(i, &line);
                }
                Err(Error::ShouldWait) => {
                    let _ = d.port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, i as u64);
                    break;
                }
                Err(_) => {
                    let _ = c.close();
                    d.clients[i] = None;
                    break;
                }
            }
        }
    }
}
