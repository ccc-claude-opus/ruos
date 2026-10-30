//! bench: how long the basic operations take on this machine.
#![no_std]
#![no_main]

extern crate alloc;

use libsys::ns::Namespace;
use libsys::store::Store;
use libsys::{Channel, Handle, Memory, map, ns, println};

libsys::entry!(main);

/// Runs `f` `n` times; nanoseconds per run.
fn time(n: u64, mut f: impl FnMut()) -> u64 {
    let t0 = libsys::clock_ns();
    for _ in 0..n {
        f();
    }
    (libsys::clock_ns() - t0) / n
}

static mut STACK: [u8; 16 * 1024] = [0; 16 * 1024];

/// Echoes every message on its channel (handle in `arg`) until it closes.
extern "C" fn echo(arg: usize) -> ! {
    let ch = Channel(Handle(arg as u32));
    let mut b = [0u8; 64];
    while let Ok((n, _)) = ch.recv(&mut b, &mut []) {
        if ch.write(&b[..n], &[]).is_err() {
            break;
        }
    }
    libsys::thread_exit(0)
}

fn round_trips(ch: Channel, n: u64) -> u64 {
    let mut b = [0u8; 64];
    time(n, || {
        let _ = ch.write(b"ping", &[]);
        let _ = ch.recv(&mut b, &mut []);
    })
}

fn work(n: u64) -> u64 {
    let mut x = 1u64;
    for i in 0..n {
        x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(i));
    }
    x
}

fn main(_arg: usize) -> i64 {
    let arch = libsys::ARCH;
    let sys = time(5000, || {
        libsys::clock_ns();
    });
    println!("bench: system call {} ns", sys);

    if let Ok((a, b)) = Channel::create() {
        if let Ok(t) = libsys::thread_create(echo, unsafe { &mut *(&raw mut STACK) }, b.0.0 as usize) {
            println!("bench: channel round trip, two threads {} us", round_trips(a, 500) / 1000);
            let _ = a.close();
            let _ = t.wait();
        }
    }

    if let Some(s) = ns::get("store").map(|h| Store(Channel(h))) {
        if let Ok((a, b)) = Channel::create() {
            if let Ok(t) = s.spawn("peer", &Namespace::new().add("peer", b.0), None) {
                println!("bench: channel round trip, two tasks {} us", round_trips(a, 500) / 1000);
                let _ = a.close();
                let _ = t.wait();
            }
        }
    }

    let m = time(100, || {
        if let Ok(h) = Memory::create(64 * 1024) {
            if let Ok(p) = h.map(0, 64 * 1024, map::READ | map::WRITE) {
                let _ = Memory::unmap(p, 64 * 1024);
            }
            let _ = h.0.close();
        }
    });
    if arch != "wasm32" || cfg!(not(target_arch = "wasm32")) {
        println!("bench: memory object 64 KiB create + map + unmap {} us", m / 1000);
    }

    let n = 2_000_000u64;
    let t0 = libsys::clock_ns();
    work(n);
    let ns_ = (libsys::clock_ns() - t0).max(1);
    println!("bench: compute {} K iterations/s ({})", n * 1_000_000 / ns_, arch);
    0
}
