//! snoop: tries to reach everything it can, to show that it can only use
//! the capabilities it was given. It reports its namespace, then tries every
//! handle number (a handle is an index into this task's own table, so there
//! is nothing to guess), then uses what it found. With a store it also
//! tries the backend's database `app` through it, which a plain store
//! client may not use; with `sql:app` (or any `sql:NAME`) it can.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use libsys::console::Console;
use libsys::store::Store;
use libsys::{Channel, Handle, cprintln, ns, println};

libsys::entry!(main);

fn main(_arg: usize) -> i64 {
    let names: Vec<&str> = ns::names().collect();
    println!("snoop: my names: {}", if names.is_empty() { alloc::string::String::from("(none)") } else { names.join(", ") });
    let held = (0..1024u32).filter(|&i| Handle(i).info().is_ok()).count();
    println!("snoop: handles 0..1023 tried, {} are mine", held);
    match ns::get("store") {
        Some(s) => match Store(Channel(s)).list() {
            Ok(l) => println!("snoop: the store lists {} images", l.split(|&c| c == b'\n').filter(|x| !x.is_empty()).count()),
            Err(e) => println!("snoop: the store said {:?}", e),
        },
        None => println!("snoop: no store: I cannot see a single image"),
    }
    if let Some(s) = ns::get("store") {
        match libsys::sql::Sql::with(Store(Channel(s)), "app").query("SELECT 1", "") {
            Ok(Ok(_)) => println!("snoop: the store let me use the database app"),
            Ok(Err(why)) => println!("snoop: the database app through the store: {why}"),
            Err(e) => println!("snoop: the database app through the store: {:?}", e),
        }
    }
    for db in names.iter().filter_map(|n| n.strip_prefix("sql:")) {
        let Some(sql) = libsys::sql::Sql::get(db) else { continue };
        match sql.query("SELECT 6 * 7 AS answer", "") {
            Ok(Ok(j)) => println!("snoop: sql:{db} answered {j}"),
            Ok(Err(why)) => println!("snoop: sql:{db} said {why}"),
            Err(e) => println!("snoop: sql:{db}: {:?}", e),
        }
    }
    match ns::get("console") {
        Some(c) => cprintln!(Console(Channel(c)), "snoop: written through my console capability"),
        None => println!("snoop: no console: only the kernel's debug output is left"),
    }
    0
}
