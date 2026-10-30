//! worker: a pod that keeps running, for kube's Deployments. `worker`
//! runs until stopped and says so every 10 s; `worker SECONDS [CODE]`
//! ends after SECONDS with CODE (0 if not given): a job, or a crash.
//! It shows its configuration (MESSAGE in its environment,
//! /etc/message.txt in its files) and every 3 s calls each service it
//! may use (its `svc:` names), printing who answered.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;

use libsys::fs::Fs;
use libsys::{Channel, args, ns, println, signal, svc, wait_one};

libsys::entry!(main);

fn call(name: &str, pod: &str) -> String {
    let r = (|| {
        let c = svc::connect(name)?;
        c.write(format!("hello from {pod}").as_bytes(), &[])?;
        wait_one(c.0, signal::READABLE | signal::PEER_CLOSED, 3_000_000_000)?;
        let mut buf = [0u8; 256];
        let (n, _) = c.read(&mut buf, &mut [])?;
        let _ = c.close();
        Ok::<_, libsys::Error>(String::from_utf8_lossy(&buf[..n]).into_owned())
    })();
    match r {
        Ok(s) => format!("{name} said: {s}"),
        Err(e) => format!("{name}: {e:?}"),
    }
}

fn main(_arg: usize) -> i64 {
    let (argv, env) = args::load();
    let num = |i: usize| argv.get(i).and_then(|a| core::str::from_utf8(a).ok()?.parse::<i64>().ok());
    let (secs, code) = (num(1), num(2).unwrap_or(0));
    let var = |k: &str| env.iter().find_map(|e| e.strip_prefix(format!("{k}=").as_bytes())).map(|v| String::from_utf8_lossy(v).into_owned());
    let pod = var("POD").unwrap_or_else(|| String::from("worker"));
    println!("worker: started");
    if let Some(m) = var("MESSAGE") {
        println!("worker: MESSAGE={m}");
    }
    if let Some(m) = ns::get("fs").and_then(|h| Fs(Channel(h)).read("/etc/message.txt").ok()) {
        println!("worker: /etc/message.txt: {}", String::from_utf8_lossy(&m).trim_end());
    }
    let mut t = 0;
    loop {
        libsys::sleep_ms(1000);
        t += 1;
        if secs.is_some_and(|s| t >= s) {
            println!("worker: done after {t} s, exit {code}");
            return code;
        }
        if t % 3 == 0 {
            for s in svc::uses() {
                println!("worker: {}", call(s, &pod));
            }
        }
        if t % 10 == 0 {
            println!("worker: up {t} s");
        }
    }
}
