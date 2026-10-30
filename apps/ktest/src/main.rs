//! ktest: the first task. For now it exercises the kernel (handles, threads,
//! preemption, spawning, fault isolation) and M3 (channels, ports, memory).
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use libsys::ns::Namespace;
use libsys::store::Store;
use libsys::{Channel, Error, Handle, Memory, ObjType, Port, Rights, map, ns, println, signal};

/// The store client from the namespace.
fn store() -> Store {
    Store(Channel(ns::get("store").expect("no store in the namespace")))
}

libsys::entry!(main);

static mut STACK: [u8; 16 * 1024] = [0; 16 * 1024];
static mut SPIN_STACK: [u8; 16 * 1024] = [0; 16 * 1024];
static STOP: AtomicBool = AtomicBool::new(false);
static SPINS: AtomicU64 = AtomicU64::new(0);

extern "C" fn worker(arg: usize) -> ! {
    println!("ktest: thread says hi (arg {})", arg);
    libsys::sleep_ms(10);
    libsys::thread_exit(7)
}

/// Never yields: the main thread only runs again if the timer preempts it.
extern "C" fn spinner(_: usize) -> ! {
    while !STOP.load(Ordering::Relaxed) {
        SPINS.fetch_add(1, Ordering::Relaxed);
    }
    libsys::thread_exit(0)
}

fn fail(what: &str) -> i64 {
    println!("ktest: FAILED: {}", what);
    1
}

fn main(_arg: usize) -> i64 {
    if m2() != 0 || m3() != 0 || m4() != 0 || m8() != 0 {
        return 1;
    }
    m9()
}

const FP_THREADS: usize = 8;
static mut FP_STACKS: [[u8; 16 * 1024]; FP_THREADS] = [[0; 16 * 1024]; FP_THREADS];
static FP_ITERS: AtomicU64 = AtomicU64::new(0);

/// A floating point series long enough to be preempted many times.
fn fp_work(seed: u64, n: u64) -> u64 {
    let mut x = seed as f64;
    for i in 0..n {
        x = core::hint::black_box(x * 1.000_000_1 + (i as f64) * 0.5);
        if x > 1e6 {
            x /= 3.0;
        }
    }
    x.to_bits()
}

extern "C" fn fp_worker(i: usize) -> ! {
    libsys::thread_exit((fp_work(i as u64 + 1, FP_ITERS.load(Ordering::Relaxed)) & 0x7fff_ffff) as i64)
}

/// More threads than CPUs, all in the middle of float work when the timer
/// switches them: each result must be exactly the one computed alone.
fn m9() -> i64 {
    let mut n = 1u64 << 12;
    loop {
        let t0 = libsys::clock_ns();
        fp_work(1, n);
        if libsys::clock_ns() - t0 >= 30_000_000 {
            break;
        }
        n *= 2;
    }
    FP_ITERS.store(n, Ordering::Relaxed);
    let mut ts = [Handle(0); FP_THREADS];
    for (i, t) in ts.iter_mut().enumerate() {
        match libsys::thread_create(fp_worker, unsafe { &mut (*(&raw mut FP_STACKS))[i] }, i) {
            Ok(h) => *t = h,
            Err(_) => return fail("fp threads"),
        }
    }
    for (i, t) in ts.iter().enumerate() {
        let want = (fp_work(i as u64 + 1, n) & 0x7fff_ffff) as i64;
        if t.wait() != Ok(want) {
            return fail("a thread's floating point registers changed under it");
        }
    }
    println!("ktest: fp ok ({} threads, {} preemptible iterations each)", FP_THREADS, n);
    println!("M9 OK");
    0
}

static mut SMP_STACKS: [[u8; 16 * 1024]; 4] = [[0; 16 * 1024]; 4];
static SMP_ITERS: AtomicU64 = AtomicU64::new(0);

fn work(n: u64) -> u64 {
    let mut x = 1u64;
    for i in 0..n {
        x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(i));
    }
    x
}

extern "C" fn smp_worker(_: usize) -> ! {
    work(SMP_ITERS.load(Ordering::Relaxed));
    libsys::thread_exit(0)
}

/// Four threads doing the same work as one must finish clearly faster
/// than four times one (CPUs on QEMU, Workers on wasm32).
fn m8() -> i64 {
    let mut n = 1u64 << 16;
    let t1 = loop {
        let t0 = libsys::clock_ns();
        work(n);
        let t = libsys::clock_ns() - t0;
        if t >= 100_000_000 {
            break t;
        }
        n *= 2;
    };
    SMP_ITERS.store(n, Ordering::Relaxed);
    let t0 = libsys::clock_ns();
    let mut ts = [Handle(0); 4];
    for (i, t) in ts.iter_mut().enumerate() {
        match libsys::thread_create(smp_worker, unsafe { &mut (*(&raw mut SMP_STACKS))[i] }, i) {
            Ok(h) => *t = h,
            Err(_) => return fail("smp threads"),
        }
    }
    for t in ts {
        if t.wait() != Ok(0) {
            return fail("smp wait");
        }
    }
    let t4 = libsys::clock_ns() - t0;
    let x10 = 40 * t1 / t4.max(1);
    println!("ktest: smp: 4 threads ran {}.{}x faster than one", x10 / 10, x10 % 10);
    if x10 < 15 {
        return fail("4 threads were not run in parallel");
    }
    println!("M8 OK");
    0
}

/// Images come from the store service (the object store).
fn m4() -> i64 {
    let (m, len) = match store().program("hello") {
        Ok(x) => x,
        Err(e) => {
            println!("ktest: store: {:?}", e);
            return 1;
        }
    };
    let mut magic = [0u8; 4];
    if m.read(0, &mut magic).is_err() || len < 64 || (&magic != b"\x7fELF" && &magic != b"\0asm") {
        return fail("program image");
    }
    if store().program("nonexistent") != Err(Error::NotFound) {
        return fail("missing image");
    }
    let list = store().list().unwrap_or_default();
    let n = list.split(|&c| c == b'\n').filter(|l| !l.is_empty()).count();
    println!("ktest: store ok ({} images, hello is {} bytes)", n, len);
    println!("M4 OK");
    0
}

fn m2() -> i64 {
    println!("ktest: hello from user mode ({})", libsys::ARCH);

    // Threads and handles.
    let t = match libsys::thread_create(worker, unsafe { &mut *(&raw mut STACK) }, 42) {
        Ok(t) => t,
        Err(e) => return fail(match e { Error::NotSupported => "threads not supported", _ => "thread_create" }),
    };
    let w = t.dup(Rights::WAIT | Rights::DUP).unwrap();
    match w.info() {
        Ok((ObjType::Thread, r)) if r == Rights::WAIT | Rights::DUP => {}
        _ => return fail("handle_info"),
    }
    if w.dup(Rights::ALL) != Err(Error::AccessDenied) {
        return fail("dup must not add rights");
    }
    w.close().unwrap();
    if w.info() != Err(Error::BadHandle) {
        return fail("closed handle still valid");
    }
    println!("ktest: handles ok (dup, info, close, bad handle)");
    match t.wait() {
        Ok(7) => println!("ktest: thread joined (exit 7)"),
        _ => return fail("thread wait"),
    }

    // Sleep.
    let t0 = libsys::clock_ns();
    libsys::sleep_ms(50);
    let ms = (libsys::clock_ns() - t0) / 1_000_000;
    if ms < 50 {
        return fail("sleep too short");
    }
    println!("ktest: slept {} ms", (ms / 10) * 10);

    // Preemption: a thread that never yields must not starve this one.
    let s = libsys::thread_create(spinner, unsafe { &mut *(&raw mut SPIN_STACK) }, 0).unwrap();
    for _ in 0..200 {
        if SPINS.load(Ordering::Relaxed) != 0 {
            break;
        }
        libsys::sleep_ms(5);
    }
    libsys::sleep_ms(30);
    STOP.store(true, Ordering::Relaxed);
    s.wait().unwrap();
    if SPINS.load(Ordering::Relaxed) == 0 {
        return fail("spinner never ran");
    }
    println!("ktest: preemption ok");

    // Tasks: a handle moves to the child.
    let gift = t.dup(Rights::WAIT | Rights::TRANSFER).unwrap();
    let hello = match store().spawn("hello", &Namespace::new().add("gift", gift), None) {
        Ok(h) => h,
        Err(e) => {
            println!("ktest: spawn hello: {:?}", e);
            return 1;
        }
    };
    // The moved handle's slot is free (or reused for `hello`).
    match gift.info() {
        Err(Error::BadHandle) => {}
        Ok((ObjType::Task, _)) if hello == gift => {}
        _ => return fail("transferred handle still here"),
    }
    println!("ktest: hello exited with {}", hello.wait().unwrap());
    if store().spawn("nonexistent", &Namespace::new(), None) != Err(Error::NotFound) {
        return fail("spawn of a missing image");
    }

    // A task that touches kernel memory is killed; init is not affected.
    let f = store().spawn("fault", &Namespace::new(), None).unwrap();
    match f.wait() {
        Ok(c) if c == Error::Killed.code() => println!("ktest: fault was killed"),
        r => {
            println!("ktest: fault: {:?}", r);
            return 1;
        }
    }
    println!("M2 OK");
    0
}

fn check(ok: bool, what: &str) -> Result<(), i64> {
    if ok { Ok(()) } else { Err(fail(what)) }
}

fn m3() -> i64 {
    match m3_tests() {
        Ok(()) => {
            println!("M3 OK");
            0
        }
        Err(c) => c,
    }
}

fn m3_tests() -> Result<(), i64> {
    let e = |what: &'static str| move |err: Error| {
        println!("ktest: {}: {:?}", what, err);
        1i64
    };

    // A channel within the task, carrying bytes and a handle.
    let (a, b) = Channel::create().map_err(e("channel_create"))?;
    let m = Memory::create(4096).map_err(e("memory_create"))?;
    a.write(b"ping", &[m.0]).map_err(e("channel_write"))?;
    check(m.0.info() == Err(Error::BadHandle) || m.0.info().map(|x| x.0) != Ok(ObjType::Memory), "handle moved")?;
    let mut buf = [0u8; 64];
    let mut hs = [Handle(0); 4];
    check(b.read(&mut [0u8; 2], &mut hs) == Err(Error::BufferTooSmall), "buffer too small")?;
    let (n, nh) = b.read(&mut buf, &mut hs).map_err(e("channel_read"))?;
    check(&buf[..n] == b"ping" && nh == 1 && hs[0].info().map(|x| x.0) == Ok(ObjType::Memory), "message")?;
    check(b.read(&mut buf, &mut hs) == Err(Error::ShouldWait), "empty channel")?;
    println!("ktest: channel ok (bytes and a handle)");

    // Waiting with a timeout.
    let t0 = libsys::clock_ns();
    check(libsys::wait_one(b.0, signal::READABLE, 20_000_000) == Err(Error::TimedOut), "wait_one timeout")?;
    check(libsys::clock_ns() - t0 >= 20_000_000, "timeout too short")?;
    println!("ktest: wait_one timeout ok");

    // Ports.
    let p = Port::create().map_err(e("port_create"))?;
    p.bind(b.0, signal::READABLE, 7).map_err(e("port_bind"))?;
    a.write(b"x", &[]).map_err(e("write"))?;
    let k = p.wait(u64::MAX).map_err(e("port_wait"))?;
    check(k.key == 7 && k.signals == signal::READABLE, "port signal packet")?;
    p.queue(9, 42).map_err(e("port_queue"))?;
    let k = p.wait(u64::MAX).map_err(e("port_wait"))?;
    check(k.key == 9 && k.data == 42, "port user packet")?;
    check(p.wait(10_000_000) == Err(Error::TimedOut), "port timeout")?;
    a.close().map_err(e("close"))?;
    check(libsys::wait_one(b.0, signal::PEER_CLOSED, 0) == Ok(signal::PEER_CLOSED), "peer closed")?;
    b.read(&mut buf, &mut hs).map_err(e("drain"))?;
    check(b.read(&mut buf, &mut hs) == Err(Error::PeerClosed), "read after close")?;
    println!("ktest: port ok (signals, user packets, timeout)");

    // Copy-on-write clones.
    let m = Memory::create(3 * 4096).map_err(e("memory_create"))?;
    m.write(4096, b"original").map_err(e("memory_write"))?;
    let c = m.clone_cow().map_err(e("memory_clone"))?;
    c.write(4096, b"CLONE!!!").map_err(e("memory_write"))?;
    let (mut x, mut y) = ([0u8; 8], [0u8; 8]);
    m.read(4096, &mut x).map_err(e("memory_read"))?;
    c.read(4096, &mut y).map_err(e("memory_read"))?;
    check(&x == b"original" && &y == b"CLONE!!!", "cow clone")?;
    println!("ktest: memory cow ok");

    // Another task: echo, and a memory handle it writes into.
    let (mine, theirs) = Channel::create().map_err(e("channel_create"))?;
    let peer = store().spawn("peer", &Namespace::new().add("peer", theirs.0), None).map_err(e("spawn peer"))?;
    mine.write(b"hello peer", &[]).map_err(e("write"))?;
    let (n, _) = mine.recv(&mut buf, &mut hs).map_err(e("recv"))?;
    check(&buf[..n] == b"echo: hello peer", "echo")?;
    let shared = Memory::create(3 * 4096).map_err(e("memory_create"))?;
    let gift = shared.0.dup(Rights::ALL).map_err(e("dup"))?;
    mine.write(b"memory", &[gift]).map_err(e("write"))?;
    let (n, _) = mine.recv(&mut buf, &mut hs).map_err(e("recv"))?;
    check(&buf[..n] == b"done", "peer done")?;
    let mut z = [0u8; 9];
    shared.read(4096, &mut z).map_err(e("read"))?;
    check(&z == b"from peer", "peer wrote memory")?;
    mine.close().map_err(e("close"))?;
    check(peer.wait() == Ok(0), "peer exit")?;
    println!("ktest: peer ok (echo, memory handle)");

    // Mapping (native): shared pages, copy-on-write on a write fault.
    match m.map(0, 3 * 4096, map::READ | map::WRITE) {
        Ok(p) => {
            unsafe {
                check(core::slice::from_raw_parts(p.add(4096), 8) == b"original", "mapped read")?;
                core::ptr::copy_nonoverlapping(b"via map!".as_ptr(), p.add(4096), 8);
            }
            m.read(4096, &mut x).map_err(e("memory_read"))?;
            check(&x == b"via map!", "mapped write visible")?;
            let snap = m.clone_cow().map_err(e("memory_clone"))?;
            unsafe { core::ptr::copy_nonoverlapping(b"changed!".as_ptr(), p.add(4096), 8) };
            snap.read(4096, &mut y).map_err(e("memory_read"))?;
            m.read(4096, &mut x).map_err(e("memory_read"))?;
            check(&y == b"via map!" && &x == b"changed!", "cow on write fault")?;
            let mut r = [0u8; 14];
            shared.map(0, 3 * 4096, map::READ).map(|q| unsafe {
                r.copy_from_slice(core::slice::from_raw_parts(q.add(8192), 14));
            }).map_err(e("map shared"))?;
            check(&r == b"mapped by peer", "peer's mapping")?;
            Memory::unmap(p, 3 * 4096).map_err(e("unmap"))?;
            println!("ktest: memory map ok (shared, cow on write fault)");
        }
        Err(Error::NotSupported) => println!("ktest: memory map not supported on {}", libsys::ARCH),
        Err(err) => return Err(e("memory_map")(err)),
    }
    Ok(())
}
