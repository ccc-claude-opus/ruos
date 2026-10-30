//! The kernel seen from outside (wasm32: the page draws it): every task
//! with its handle table, and the quota tree, as JSON. An object's `id` is
//! its address, so two handles to the same object, and the two ends of a
//! channel (`peer`), can be matched up. This is a debugger's view: inside
//! the system nobody can list what they were not given.
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use core::fmt::Write;

use abi::Rights;

use crate::object::Object;
use crate::quota::Quota;
use crate::task;

fn addr<T>(a: &Arc<T>) -> usize {
    Arc::as_ptr(a) as *const () as usize
}

fn rights(r: Rights) -> String {
    [(Rights::DUP, 'd'), (Rights::TRANSFER, 't'), (Rights::WAIT, 'w'), (Rights::READ, 'r'), (Rights::WRITE, 'W'), (Rights::MANAGE, 'm')]
        .iter()
        .map(|&(b, c)| if r.has(b) { c } else { '-' })
        .collect()
}

/// A JSON string (names are the only text).
fn quoted(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                o.push('\\');
                o.push(c);
            }
            c if (c as u32) < 0x20 => _ = write!(o, "\\u{:04x}", c as u32),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// `{"uptime_ms": N, "tasks": [...], "quotas": [...]}`.
pub fn json() -> String {
    let mut quotas: BTreeMap<usize, Arc<Quota>> = BTreeMap::new();
    let add_quota = |q: &Arc<Quota>, quotas: &mut BTreeMap<usize, Arc<Quota>>| {
        let mut q = Some(q.clone());
        while let Some(x) = q {
            q = x.parent().cloned();
            quotas.insert(addr(&x), x);
        }
    };
    let mut out = String::new();
    let _ = write!(out, "{{\"uptime_ms\":{},\"tasks\":[", crate::arch::now_ns() / 1_000_000);
    for (i, t) in task::all().iter().enumerate() {
        let (threads, exit) = t.status();
        add_quota(&t.quota, &mut quotas);
        let _ = write!(
            out,
            "{}{{\"id\":{},\"name\":{},\"threads\":{},\"exit\":{},\"quota\":{},\"handles\":[",
            if i > 0 { "," } else { "" },
            t.id,
            quoted(&t.name),
            threads,
            exit.map_or(String::from("null"), |c| alloc::format!("{c}")),
            addr(&t.quota)
        );
        let table = t.handles.lock();
        for (j, (n, h)) in table.iter().enumerate() {
            let (kind, id, extra) = match &h.obj {
                Object::Task(x) => ("task", addr(x), alloc::format!(",\"task\":{}", x.id)),
                Object::Thread(x) => ("thread", addr(x), String::new()),
                Object::Channel(x) => {
                    let peer = match (x.peer(), x.is_remote()) {
                        (_, true) => String::from("\"host\""),
                        (Some(p), _) => alloc::format!("{}", addr(&p)),
                        (None, _) => String::from("null"),
                    };
                    ("channel", addr(x), alloc::format!(",\"peer\":{peer},\"queued\":{}", x.queued()))
                }
                Object::Port(x) => ("port", addr(x), String::new()),
                Object::Memory(x) => ("memory", addr(x), alloc::format!(",\"size\":{}", x.size())),
                Object::Serial(x) => ("serial", addr(x), String::new()),
                Object::Quota(x) => {
                    add_quota(x, &mut quotas);
                    ("quota", addr(x), String::new())
                }
            };
            let _ = write!(
                out,
                "{}{{\"h\":{},\"type\":\"{}\",\"rights\":\"{}\",\"id\":{}{}}}",
                if j > 0 { "," } else { "" },
                n,
                kind,
                rights(h.rights),
                id,
                extra
            );
        }
        out.push_str("]}");
    }
    out.push_str("],\"quotas\":[");
    for (i, (id, q)) in quotas.iter().enumerate() {
        let x = q.info();
        let lim = |v: u64| if v == u64::MAX { String::from("null") } else { alloc::format!("{v}") };
        let _ = write!(
            out,
            "{}{{\"id\":{},\"parent\":{},\"mem\":[{},{}],\"tasks\":[{},{}],\"threads\":[{},{}]}}",
            if i > 0 { "," } else { "" },
            id,
            q.parent().map_or(String::from("null"), |p| alloc::format!("{}", addr(p))),
            x.mem_used,
            lim(x.mem_limit),
            x.tasks_used,
            lim(x.tasks_limit),
            x.threads_used,
            lim(x.threads_limit)
        );
    }
    out.push_str("]}");
    out
}
