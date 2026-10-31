//! SQLite databases kept on this machine (`sql:NAME@local`): the same
//! requests as the backend's ([SQL, name, sql, params, format] → [OK,
//! answer] / [FAILED, why]). Each database is a `sqlited` program (WASI,
//! wasi-programs/sqlited: SQLite compiled with wasi-sdk) whose console and
//! files this service gives it: requests go to it a line at a time, its
//! files are the storage's `sql` tree (`/NAME.db`).
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libsys::console::{READ_LINE, WRITE};
use libsys::ns::Namespace;
use libsys::store::Store;
use libsys::{Channel, Handle};
use serde_json::Value as Json;

pub struct Db {
    pub task: Handle,
    /// This service's end of the program's console.
    pub con: Channel,
    /// The program asked for a line (READ_LINE) and has not had it.
    wants_line: bool,
    /// Requests not yet given to it: (client slot, line).
    queue: VecDeque<(usize, String)>,
    /// The client whose request it is working on.
    waiting: Option<usize>,
    out: Vec<u8>,
}

/// `s` as a JSON string.
fn quote(s: &str) -> String {
    Json::String(String::from(s)).to_string()
}

impl Db {
    /// Starts `sqlited NAME.db`; `fs` is the end of a channel this service
    /// serves (the `sql` tree).
    pub fn start(store: &Store, name: &str, fs: Channel) -> Option<Db> {
        let (m, len) = store.program("sqlited").ok()?;
        let (con, theirs) = Channel::create().ok()?;
        let file = format!("{name}.db");
        let ns = Namespace::new().add("console", theirs.0).add("fs", fs.0);
        let ns = libsys::args::add(ns, &["sqlited", &file], &[(String::from("PWD"), String::from("/"))]).ok()?;
        let task = store.spawn_module(m, len, "sqlited", &ns, None).ok();
        let _ = m.0.close();
        Some(Db { task: task?, con, wants_line: false, queue: VecDeque::new(), waiting: None, out: Vec::new() })
    }

    /// A request from client `slot`: `sql`, `params` (a JSON array or
    /// empty), `format`.
    pub fn ask(&mut self, slot: usize, sql: &str, params: &str, format: &str) {
        let params = if params.trim().is_empty() { "[]" } else { params };
        let line = format!("{{\"sql\":{},\"params\":{params},\"format\":{}}}", quote(sql), quote(format));
        self.queue.push_back((slot, line));
        self.pump();
    }

    fn pump(&mut self) {
        if self.wants_line && self.waiting.is_none()
            && let Some((slot, line)) = self.queue.pop_front()
        {
            let mut m = alloc::vec![READ_LINE];
            m.extend_from_slice(line.as_bytes());
            if self.con.write(&m, &[]).is_ok() {
                self.wants_line = false;
                self.waiting = Some(slot);
            }
        }
    }

    /// A message from the program's console: answers that are done, as
    /// (client slot, reply).
    pub fn from_program(&mut self, msg: &[u8]) -> Vec<(usize, Vec<u8>)> {
        let mut done = Vec::new();
        match msg.first() {
            Some(&WRITE) => {
                self.out.extend_from_slice(&msg[1..]);
                while let Some(i) = self.out.iter().position(|&c| c == b'\n') {
                    let line: Vec<u8> = self.out.drain(..=i).collect();
                    let Some(slot) = self.waiting.take() else { continue };
                    let j: Json = serde_json::from_slice(&line).unwrap_or(Json::Null);
                    let r = match (j["ok"].as_str(), j["err"].as_str()) {
                        (Some(ok), _) => [&[libsys::wire::OK][..], ok.as_bytes()].concat(),
                        (_, Some(e)) => [&[libsys::wire::FAILED][..], e.as_bytes()].concat(),
                        _ => [&[libsys::wire::FAILED][..], &line].concat(),
                    };
                    done.push((slot, r));
                }
            }
            Some(&READ_LINE) => self.wants_line = true,
            _ => {}
        }
        self.pump();
        done
    }

    /// The program ended: every client still waiting gets FAILED.
    pub fn ended(self) -> Vec<(usize, Vec<u8>)> {
        let why = b"sqlite ended (it starts again with the next request)";
        let fail = |s: usize| (s, [&[libsys::wire::FAILED][..], &why[..]].concat());
        let _ = self.con.close();
        let _ = self.task.close();
        self.waiting.into_iter().chain(self.queue.into_iter().map(|(s, _)| s)).map(fail).collect()
    }
}
