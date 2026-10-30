//! blk: the virtio-blk driver (`virtio-drivers`). Namespace: `mmio`,
//! `irq`, `client` (its user, the store). Serves libsys::blk requests one
//! at a time; the driver waits for each request by polling.
#![no_std]
#![no_main]

extern crate alloc;

use libsys::blk::{INFO, MAX_SECTORS, READ, SECTOR, WRITE};
use libsys::wire::{FAILED, OK};
use libsys::{Channel, Mmio, ns, println};
use virtio::RosHal;
use virtio::virtio_drivers::device::blk::VirtIOBlk;

libsys::entry!(main);

fn main(_arg: usize) -> i64 {
    let (Some(m), Some(c)) = (ns::get("mmio"), ns::get("client")) else { return 1 };
    let mut blk = match virtio::transport(Mmio(m)).ok().and_then(|t| VirtIOBlk::<RosHal, _>::new(t).ok()) {
        Some(b) => b,
        None => {
            println!("blk: no virtio-blk device");
            return 2;
        }
    };
    let sectors = blk.capacity();
    println!("blk: virtio-blk, {} KiB", sectors / 2);
    let client = Channel(c);
    let mut buf = alloc::vec![0u8; 1 + 8 + MAX_SECTORS * SECTOR];
    let mut data = alloc::vec![0u8; MAX_SECTORS * SECTOR];
    loop {
        let Ok((n, _)) = client.recv(&mut buf, &mut []) else { return 0 };
        let sector = u64::from_le_bytes(buf[1..9].try_into().unwrap_or([0; 8])) as usize;
        let reply = match buf[0] {
            INFO => {
                let mut r = alloc::vec![OK];
                r.extend_from_slice(&sectors.to_le_bytes());
                r
            }
            READ if n == 13 => {
                let count = (u32::from_le_bytes(buf[9..13].try_into().unwrap()) as usize).min(MAX_SECTORS);
                match blk.read_blocks(sector, &mut data[..count * SECTOR]) {
                    Ok(()) => {
                        let mut r = alloc::vec![OK];
                        r.extend_from_slice(&data[..count * SECTOR]);
                        r
                    }
                    Err(_) => alloc::vec![FAILED],
                }
            }
            WRITE if n > 9 && (n - 9) % SECTOR == 0 && (n - 9) / SECTOR <= MAX_SECTORS => {
                if blk.write_blocks(sector, &buf[9..n]).is_ok() { alloc::vec![OK] } else { alloc::vec![FAILED] }
            }
            _ => alloc::vec![FAILED],
        };
        let _ = client.write(&reply, &[]);
    }
}
