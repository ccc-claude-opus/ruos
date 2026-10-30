//! writer: uses its pod's file system (`fs`): shows /etc/motd, counts its
//! runs in /data/count, then removes /etc/motd. Run it, commit the pod,
//! run the new image: the count goes on and the motd is gone.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use libsys::fs::Fs;
use libsys::{Channel, ns, println};

libsys::entry!(main);

fn main(_arg: usize) -> i64 {
    let Some(fs) = ns::get("fs").map(|h| Fs(Channel(h))) else {
        println!("writer: no file system");
        return 1;
    };
    match fs.read("/etc/motd") {
        Ok(m) => println!("writer: motd: {}", core::str::from_utf8(&m).unwrap_or("?").trim_end()),
        Err(libsys::Error::NotFound) => println!("writer: no /etc/motd"),
        Err(e) => println!("writer: /etc/motd: {:?}", e),
    }
    let n: u64 = fs.read("/data/count").ok().and_then(|b| core::str::from_utf8(&b).ok()?.trim().parse().ok()).unwrap_or(0);
    let n = n + 1;
    if fs.write("/data/count", alloc::format!("{n}\n").as_bytes()).is_err() {
        return 2;
    }
    println!("writer: count {}", n);
    let _ = fs.delete("/etc/motd");
    // A file larger than one message: written and read in chunks, kept in
    // commits, read from an image layer when the next run finds it.
    const BIG: usize = 200 * 1024;
    let pattern = |i: usize| (i * 7 + i / 4096) as u8;
    match fs.read("/data/big") {
        Ok(b) if b.len() == BIG && b.iter().enumerate().all(|(i, &x)| x == pattern(i)) => {
            println!("writer: big file from before ok ({} bytes)", b.len())
        }
        Ok(_) => println!("writer: big file from before is wrong"),
        Err(_) => {
            let data: Vec<u8> = (0..BIG).map(pattern).collect();
            if let Err(e) = fs.write("/data/big", &data) {
                println!("writer: writing a big file: {:?}", e);
                return 3;
            }
            match fs.read("/data/big") {
                Ok(b) if b == data => println!("writer: big file ok ({} bytes)", b.len()),
                _ => println!("writer: big file read back wrong"),
            }
        }
    }
    let root: Vec<_> = fs.list("/").unwrap_or_default().into_iter().map(|e| e.name).collect();
    let etc = fs.list("/etc").unwrap_or_default().len();
    println!("writer: / has {}; /etc has {} entries", root.join(", "), etc);
    0
}
