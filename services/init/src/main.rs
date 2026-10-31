//! init: the first task. Handle 0 is the serial port, 1 the boot pack, 2
//! the root quota; then, with a server, the server connection and ingress. Starts the store service from the pack, runs the
//! kernel tests, then console, dock, kube and a shell (with `http`, the
//! store's web client); ends with the shell.
#![no_std]
#![no_main]

extern crate alloc;

use libsys::console::Console;
use libsys::ns::Namespace;
use libsys::store::Store;
use libsys::{Channel, Handle, Memory, ObjType, Quota, println};
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

/// The store service, started straight from the pack: the store, its `web`
/// client (the http capability, for the shell) and the ingress channel
/// from the server (for kube).
fn start_store(pack_mem: Memory) -> Option<(Store, Channel, Option<Channel>)> {
    let size = pack_mem.size().ok()?;
    let mut bytes = alloc::vec![0u8; size as usize];
    pack_mem.read(0, &mut bytes).ok()?;
    let pack = Pack::parse(&bytes, false).ok()?;
    let mut ns = Namespace::new();
    let mut ingress = None;
    let mut have_remote = false;
    // The server's channels follow the quota.
    let mut found = alloc::vec::Vec::new();
    for h in 3..64 {
        match Handle(h).info() {
            Ok((t, _)) => found.push((h, t)),
            Err(_) => break,
        }
    }
    for (h, t) in found {
        match t {
            // The server connection first (the store's), then ingress (kube's).
            ObjType::Channel if !have_remote => {
                ns = ns.add("remote", Handle(h));
                have_remote = true;
            }
            ObjType::Channel => ingress = Some(Channel(Handle(h))),
            _ => {}
        }
    }
    let (web, web_srv) = Channel::create().ok()?;
    from_pack(&pack, "store", ns.add("pack", pack_mem.0).add("web", web_srv.0)).map(|c| (Store(c), web, ingress))
}

/// A new client channel of the console.
fn console_client(con: &Console) -> Option<Handle> {
    let (a, b) = Channel::create().ok()?;
    con.connect(b.0).ok()?;
    Some(a.0)
}

fn main(_arg: usize) -> i64 {
    let Some((store, web, ingress)) = start_store(Memory(Handle(1))) else {
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
    // kube hands `http` on to the programs that use it.
    let Ok(kh) = libsys::http::Http(web).connect() else { return 1 };
    let mut ns = Namespace::new().add("dock", kd.0).add("store", ks.0.0).add("http", kh.0.0).add("client", kube_srv.0);
    if let Some(i) = ingress {
        ns = ns.add("ingress", i.0);
    }
    let Ok(kube_task) = store.spawn("kube", &ns, None) else { return 1 };

    let (Some(c), Ok(s)) = (console_client(&con), store.connect()) else { return 1 };
    let ns = Namespace::new().add("console", c).add("store", s.0.0).add("dock", dock.0).add("kube", kube.0).add("http", web.0);
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
