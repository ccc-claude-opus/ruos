//! shell: the teaching shell. Its file system is an overlay of the `root`
//! image with this boot's writable layer on top (a capability like any
//! other: the overlay's channel). The language is a small sh (lang.rs):
//! pipes, redirections, `;` `&&` `||`, variables, `$(…)`, `$((…))`, file
//! name patterns, if / for / while, scripts. Built-in commands (cmds.rs)
//! handle files, text and the system; another name runs that program from
//! /bin with a console. The capability commands show the shell's own
//! handles and namespace, start a program with only the capabilities named,
//! and look inside images and pods.
#![no_std]
#![no_main]

extern crate alloc;

mod cmds;
mod lang;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libsys::console::{Console, MAX_LINE};
use libsys::dock::Dock;
use libsys::fs::Fs;
use libsys::http::Http;
use libsys::kube::Kube;
use libsys::ns;
use libsys::store::Store;
use libsys::{Channel, cprint, cprintln};

libsys::entry!(main);

/// The image the shell's files come from (`save` commits back into it).
pub const ROOT: &str = "root";

pub struct Sh {
    pub con: Console,
    pub store: Store,
    pub dock: Option<Dock>,
    pub kube: Option<Kube>,
    pub http: Option<Http>,
    /// All of the backend: programs get parts of it (`run IMAGE sql:app`).
    pub backend: Option<Store>,
    pub fs: Option<Fs>,
    /// Keeps the overlay alive.
    _ctl: Option<Channel>,
    pub cwd: String,
    pub history: Vec<String>,
    pub vars: BTreeMap<String, String>,
    /// $0, $1, …
    pub args: Vec<String>,
    /// $? : the last command's status.
    pub status: i64,
    /// Output being collected for `$(…)`, innermost last.
    pub captures: Vec<Vec<u8>>,
    /// The command running now writes into a pipe, a file or `$(…)`.
    pub piped_out: bool,
    /// A command (`exit` in a script run by `.`) asked to end the shell.
    pub exit_request: Option<i64>,
}

/// `base` + `p`, without `.`, `..` or empty parts.
pub fn join(base: &str, p: &str) -> String {
    let mut parts: Vec<&str> = if p.starts_with('/') { Vec::new() } else { base.split('/').filter(|x| !x.is_empty()).collect() };
    for x in p.split('/') {
        match x {
            "" | "." => {}
            ".." => _ = parts.pop(),
            x => parts.push(x),
        }
    }
    format!("/{}", parts.join("/"))
}

impl Sh {
    pub fn path(&self, p: &str) -> String {
        join(&self.cwd, p)
    }

    pub fn fs(&self) -> Result<Fs, String> {
        self.fs.ok_or_else(|| String::from("no file system"))
    }

    /// A variable: the shell's, or $?, $#, $@, $0–$9.
    pub fn var(&self, name: &str) -> String {
        match name {
            "?" => self.status.to_string(),
            "#" => self.args.len().saturating_sub(1).to_string(),
            "@" | "*" => self.args.get(1..).map(|a| a.join(" ")).unwrap_or_default(),
            "PWD" => self.cwd.clone(),
            n if n.bytes().all(|c| c.is_ascii_digit()) => n.parse::<usize>().ok().and_then(|i| self.args.get(i)).cloned().unwrap_or_default(),
            n => self.vars.get(n).cloned().unwrap_or_default(),
        }
    }

    /// Runs what was typed; Some(code) to end the shell.
    fn line(&mut self, src: &str) -> Option<i64> {
        // dock takes the rest of the line as it is (its write has escapes).
        if let Some(rest) = src.trim().strip_prefix("dock ") {
            self.dock(rest.trim());
            return None;
        }
        match lang::parse(src) {
            Ok(l) => match self.run_list(&l) {
                lang::Flow::Exit(c) => Some(c),
                lang::Flow::Next => None,
            },
            Err(e) => {
                cprintln!(self.con, "shell: {}", lang::parse_error(e));
                self.status = 2;
                None
            }
        }
    }

    pub fn dock(&self, cmd: &str) {
        let Some(d) = self.dock else { return cprintln!(self.con, "dock: not available") };
        cmds::dock_cmd(&self.con, d, cmd);
    }
}

fn main(_arg: usize) -> i64 {
    let (Some(c), Some(s)) = (ns::get("console"), ns::get("store")) else { return 2 };
    let con = Console(Channel(c));
    let store = Store(Channel(s));
    // The files: the root image under this boot's writable layer.
    let key = format!("b{}-shell", store.boot().unwrap_or(0));
    let (fs, ctl) = match store.index(ROOT).and_then(|_| libsys::fs::mount(&store, &key, ROOT)) {
        Ok((f, c)) => (Some(f), Some(c)),
        Err(_) => (None, None),
    };
    let mut sh = Sh {
        con,
        store,
        dock: ns::get("dock").map(|d| Dock(Channel(d))),
        kube: ns::get("kube").map(|k| Kube(Channel(k))),
        http: Http::get(),
        backend: ns::get("backend").map(|h| Store(Channel(h))),
        fs,
        _ctl: ctl,
        cwd: String::from("/home"),
        history: Vec::new(),
        vars: BTreeMap::new(),
        args: alloc::vec![String::from("shell")],
        status: 0,
        captures: Vec::new(),
        piped_out: false,
        exit_request: None,
    };
    sh.vars.insert(String::from("HOME"), String::from("/home"));
    if sh.fs.is_none() {
        sh.cwd = String::from("/");
    }
    if let Some(m) = sh.fs.and_then(|fs| fs.read("/etc/motd").ok()) {
        let _ = con.write(&m);
    }
    cprintln!(con, "shell: ready ({}); type help", libsys::ARCH);
    let mut buf = [0u8; MAX_LINE];
    loop {
        cprint!(con, "{} $ ", sh.cwd);
        // Lines until the command is complete (a quote, if, for, while…).
        let mut src = String::new();
        loop {
            let n = match con.read_line(&mut buf) {
                Ok(n) => n,
                Err(_) => return 1,
            };
            src += core::str::from_utf8(&buf[..n]).unwrap_or("");
            if !matches!(lang::parse(&src), Err(lang::ParseError::Incomplete)) || src.trim().starts_with("dock ") {
                break;
            }
            src.push('\n');
            cprint!(con, "> ");
        }
        let src = src.trim().to_string();
        if src.is_empty() {
            continue;
        }
        // !N: a line from the history again.
        let src = match src.strip_prefix('!').map(|n| n.parse::<usize>()) {
            Some(Ok(n)) => match sh.history.get(n.wrapping_sub(1)) {
                Some(l) => {
                    cprintln!(con, "{}", l);
                    l.clone()
                }
                None => {
                    cprintln!(con, "shell: no line {} in the history", n);
                    continue;
                }
            },
            _ => src,
        };
        sh.history.push(src.clone());
        if let Some(code) = sh.line(&src) {
            return code;
        }
    }
}
