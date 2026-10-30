//! init: the first task. Handle 0 is the serial port, 1 the boot pack, 2
//! the root quota; then Mmio + Irq pairs of virtio devices (native) or the
//! channel to podsrv (wasm32 with a server). Starts the store service from the pack, runs the
//! kernel tests, then console, dock, kube and a shell; ends with the shell.
#![no_std]
#![no_main]

extern crate alloc;

use libsys::console::Console;
use libsys::net::Net;
use libsys::ns::Namespace;
use libsys::store::Store;
use libsys::{Channel, Handle, Memory, Mmio, ObjType, Quota, println};
use objstore::Pack;

libsys::entry!(main);

/// Starts `name` straight from the pack; returns a channel to it (its `client`).
fn from_pack(pack: &Pack, name: &str, ns: Namespace) -> Option<Channel> {
    let prog = pack.program(name, libsys::ARCH).ok()?;
    let m = Memory::create(prog.len() as u64).ok()?;
    m.write(0, prog).ok()?;
    let (mine, theirs) = Channel::create().ok()?;
    ns.add("client", theirs.0).spawn(m, prog.len() as u64, name, None).ok()?;
    let _ = m.0.close();
    Some(mine)
}

/// The drivers and the store service, started straight from the pack.
/// The store, and a client of the net service for the shell (if there is one).
fn start_store(pack_mem: Memory) -> Option<(Store, Option<Net>)> {
    let size = pack_mem.size().ok()?;
    let mut bytes = alloc::vec![0u8; size as usize];
    pack_mem.read(0, &mut bytes).ok()?;
    let pack = Pack::parse(&bytes, false).ok()?;
    let mut ns = Namespace::new();
    let mut net = None;
    // Devices (Mmio + Irq) or the server channel follow the quota. Take
    // stock first: starting a driver moves handles away.
    let mut found = alloc::vec::Vec::new();
    for h in 3..64 {
        match Handle(h).info() {
            Ok((t, _)) => found.push((h, t)),
            Err(_) => break,
        }
    }
    for (h, t) in found {
        match t {
            ObjType::Mmio => {
                let dev = || Namespace::new().add("mmio", Handle(h)).add("irq", Handle(h + 1));
                match Mmio(Handle(h)).info() {
                    Ok((_, _, virtio::BLOCK)) => {
                        if let Some(c) = from_pack(&pack, "blk", dev()) {
                            ns = ns.add("disk", c.0);
                        }
                    }
                    Ok((_, _, virtio::NET)) => {
                        if let Some(c) = from_pack(&pack, "net", dev()) {
                            net = Net(c).client().ok();
                            ns = ns.add("net", c.0);
                        }
                    }
                    _ => {}
                }
            }
            ObjType::Channel => ns = ns.add("remote", Handle(h)),
            _ => {}
        }
    }
    from_pack(&pack, "store", ns.add("pack", pack_mem.0)).map(|c| (Store(c), net))
}

/// A new client channel of the console.
fn console_client(con: &Console) -> Option<Handle> {
    let (a, b) = Channel::create().ok()?;
    con.connect(b.0).ok()?;
    Some(a.0)
}

fn main(_arg: usize) -> i64 {
    let Some((store, net)) = start_store(Memory(Handle(1))) else {
        println!("init: cannot start the store");
        return 1;
    };
    println!("init: started store");
    let ktest = store.connect().and_then(|s| store.spawn("ktest", &Namespace::new().add("store", s.0.0), None));
    match ktest.and_then(|t| t.wait()) {
        Ok(c) => println!("init: ktest exited with {}", c),
        Err(e) => println!("init: ktest: {:?}", e),
    }

    let Ok((con, con_srv)) = Channel::create() else { return 1 };
    let ns = Namespace::new().add("serial", Handle(0)).add("client", con_srv.0);
    let Ok(console_task) = store.spawn("console", &ns, None) else { return 1 };
    let con = Console(con);

    // Pods get at most 160 MiB, 32 tasks and 64 threads between them.
    let root = Quota(Handle(2));
    let Ok(pods) = root.child(160 << 20, 32, 64) else { return 1 };
    let Ok((dock, dock_srv)) = Channel::create() else { return 1 };
    let (Some(c), Ok(s)) = (console_client(&con), store.connect()) else { return 1 };
    let ns = Namespace::new().add("console", c).add("store", s.0.0).add("quota", pods.0).add("client", dock_srv.0);
    let Ok(dock_task) = store.spawn("dock", &ns, None) else { return 1 };

    // kube: Deployments made of dock pods; it has a dock client and a store client.
    let Ok((kd, kd_srv)) = Channel::create() else { return 1 };
    let Ok((kube, kube_srv)) = Channel::create() else { return 1 };
    if dock.write(b"", &[kd_srv.0]).is_err() {
        return 1;
    }
    // init's own dock client, to stop every pod when the shell ends.
    let Ok((own, own_srv)) = Channel::create() else { return 1 };
    if dock.write(b"", &[own_srv.0]).is_err() {
        return 1;
    }
    let Ok(ks) = store.connect() else { return 1 };
    let ns = Namespace::new().add("dock", kd.0).add("store", ks.0.0).add("client", kube_srv.0);
    let Ok(kube_task) = store.spawn("kube", &ns, None) else { return 1 };

    let (Some(c), Ok(s)) = (console_client(&con), store.connect()) else { return 1 };
    let ns = Namespace::new().add("console", c).add("store", s.0.0).add("dock", dock.0).add("kube", kube.0);
    let ns = match net {
        Some(n) => ns.add("net", n.0.0),
        None => ns,
    };
    let shell = match store.spawn("shell", &ns, None) {
        Ok(t) => t,
        Err(e) => {
            println!("init: shell: {:?}", e);
            return 1;
        }
    };
    let code = shell.wait().unwrap_or(-1);
    println!("init: shell exited ({})", code);
    let _ = kube_task.kill();
    let _ = kube_task.wait();
    // Pods hold console clients: the console ends only once they are gone.
    let _ = libsys::dock::Dock(own).request("shutdown");
    let _ = dock_task.kill();
    let _ = dock_task.wait();
    // The console ends once its last client is gone.
    let _ = con.0.close();
    let _ = console_task.wait();
    code
}
