//! snoop: tries to reach everything it can, to show that it can only use
//! the capabilities it was given. It reports its namespace, then tries every
//! handle number (a handle is an index into this task's own table, so there
//! is nothing to guess), then uses what it found.
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
    match ns::get("console") {
        Some(c) => cprintln!(Console(Channel(c)), "snoop: written through my console capability"),
        None => println!("snoop: no console: only the kernel's debug output is left"),
    }
    0
}
