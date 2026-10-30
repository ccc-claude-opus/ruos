//! hello: reports what it was given (its namespace).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use libsys::{ns, println};

libsys::entry!(main);

fn main(_arg: usize) -> i64 {
    let names: Vec<&str> = ns::names().collect();
    let names = if names.is_empty() { alloc::string::String::from("(none)") } else { names.join(", ") };
    println!("hello: hi from a pod ({}); names: {}", libsys::ARCH, names);
    0
}
