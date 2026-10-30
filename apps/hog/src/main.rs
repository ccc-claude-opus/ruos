//! hog: takes memory a mebibyte at a time until its quota says no, then
//! holds on to it (until it is stopped).
#![no_std]
#![no_main]

use libsys::{Error, Memory, println};

libsys::entry!(main);

fn main(_arg: usize) -> i64 {
    let mut n = 0;
    let err = loop {
        match Memory::create(1 << 20) {
            Ok(_) => n += 1,
            Err(e) => break e,
        }
        if n == 1024 {
            break Error::NoMemory;
        }
    };
    println!("hog: got {} MiB, then {:?}", n, err);
    loop {
        libsys::sleep_ms(1000);
    }
}
