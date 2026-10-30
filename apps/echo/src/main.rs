//! echo: a service pod (a Deployment with `serves = "echo"`). Every
//! message on a connection comes back as `POD: message`, so a caller can
//! see which pod answered. `echo hang SECONDS` stops answering anything
//! (kube's pings too) after SECONDS: its liveness probe fails.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libsys::svc::Listener;
use libsys::{Channel, Error, Port, args, clock_ns, println, signal};

libsys::entry!(main);

const SEC: u64 = 1_000_000_000;

fn main(_arg: usize) -> i64 {
    let Some(l) = Listener::get() else {
        println!("echo: no listen (run me from a Deployment with serves = \"echo\")");
        return 1;
    };
    let (argv, env) = args::load();
    let pod = env.iter().find_map(|e| e.strip_prefix(b"POD=")).map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default();
    let arg = |i: usize| argv.get(i).map(|a| core::str::from_utf8(a).unwrap_or(""));
    let hang = match (arg(1), arg(2)) {
        (Some("hang"), Some(s)) => s.parse::<u64>().ok(),
        _ => None,
    };
    let Ok(port) = Port::create() else { return 2 };
    let mut conns: Vec<Option<Channel>> = Vec::new();
    let _ = port.bind(l.handle(), signal::READABLE | signal::PEER_CLOSED, 0);
    let _ = l.ready();
    println!("echo: ready");
    let start = clock_ns();
    let mut buf = alloc::vec![0u8; 4096];
    loop {
        if hang.is_some_and(|s| clock_ns() - start >= s * SEC) {
            println!("echo: hanging (no more answers)");
            loop {
                libsys::sleep_ms(60_000);
            }
        }
        let p = match port.wait(SEC) {
            Ok(p) => p,
            Err(Error::TimedOut) => continue,
            Err(_) => return 3,
        };
        if p.key == 0 {
            loop {
                match l.poll() {
                    Ok(Some(c)) => {
                        conns.push(Some(c));
                        let _ = port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, conns.len() as u64);
                    }
                    Ok(None) => break,
                    // kube is gone: so is the service.
                    Err(_) => return 0,
                }
            }
            let _ = port.bind(l.handle(), signal::READABLE | signal::PEER_CLOSED, 0);
            continue;
        }
        let k = p.key as usize - 1;
        let Some(c) = conns[k] else { continue };
        loop {
            match c.read(&mut buf, &mut []) {
                Ok((n, _)) => {
                    let reply = format!("{pod}: {}", String::from_utf8_lossy(&buf[..n]));
                    let _ = c.write(reply.as_bytes(), &[]);
                }
                Err(Error::ShouldWait) => {
                    let _ = port.bind(c.0, signal::READABLE | signal::PEER_CLOSED, p.key);
                    break;
                }
                Err(_) => {
                    let _ = c.close();
                    conns[k] = None;
                    break;
                }
            }
        }
    }
}
