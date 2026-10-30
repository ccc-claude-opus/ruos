//! peer: the other end of ktest's channel (`peer` in its namespace). Echoes messages;
//! a message carrying a memory handle gets "from peer" written into it
//! (and "mapped by peer" through a mapping, where mapping exists).
#![no_std]
#![no_main]

use libsys::{Channel, Error, Handle, Memory, ObjType, map, ns};

libsys::entry!(main);

fn main(_arg: usize) -> i64 {
    let Some(ch) = ns::get("peer").map(Channel) else { return 5 };
    let mut buf = [0u8; 256];
    let mut hs = [Handle(0); 4];
    loop {
        let (n, nh) = match ch.recv(&mut buf, &mut hs) {
            Ok(x) => x,
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 1,
        };
        if nh == 1 && hs[0].info().map(|x| x.0) == Ok(ObjType::Memory) {
            let m = Memory(hs[0]);
            if m.write(4096, b"from peer").is_err() {
                return 2;
            }
            match m.map(0, 3 * 4096, map::READ | map::WRITE) {
                Ok(p) => unsafe { core::ptr::copy_nonoverlapping(b"mapped by peer".as_ptr(), p.add(8192), 14) },
                Err(_) => {
                    let _ = m.write(8192, b"mapped by peer");
                }
            }
            let _ = hs[0].close();
            if ch.write(b"done", &[]).is_err() {
                return 3;
            }
        } else {
            let mut out = [0u8; 262];
            out[..6].copy_from_slice(b"echo: ");
            out[6..6 + n].copy_from_slice(&buf[..n]);
            if ch.write(&out[..6 + n], &[]).is_err() {
                return 4;
            }
        }
    }
}
