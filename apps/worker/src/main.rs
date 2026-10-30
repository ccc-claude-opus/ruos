//! worker: a pod that keeps running, for kube's Deployments. `worker`
//! runs until stopped and says so every 10 s; `worker SECONDS [CODE]`
//! ends after SECONDS with CODE (0 if not given): a job, or a crash.
#![no_std]
#![no_main]

extern crate alloc;

use libsys::{args, println};

libsys::entry!(main);

fn main(_arg: usize) -> i64 {
    let (argv, _) = args::load();
    let num = |i: usize| argv.get(i).and_then(|a| core::str::from_utf8(a).ok()?.parse::<i64>().ok());
    let (secs, code) = (num(1), num(2).unwrap_or(0));
    println!("worker: started");
    let mut t = 0;
    loop {
        libsys::sleep_ms(1000);
        t += 1;
        if secs.is_some_and(|s| t >= s) {
            println!("worker: done after {t} s, exit {code}");
            return code;
        }
        if t % 10 == 0 {
            println!("worker: up {t} s");
        }
    }
}
