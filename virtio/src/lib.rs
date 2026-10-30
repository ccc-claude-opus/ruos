//! ruos glue for the `virtio-drivers` crate: its `Hal` on our DMA memory
//! objects, and MMIO transports for the devices init hands out.
#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use core::ptr::NonNull;

use libsys::{Memory, Mmio, map};
use spinning_top::Spinlock;
use virtio_drivers::transport::mmio::{MmioTransport, VirtIOHeader};
use virtio_drivers::{BufferDirection, Hal, PhysAddr};

pub use virtio_drivers;

/// virtio device ids.
pub const NET: u32 = 1;
pub const BLOCK: u32 = 2;

const PAGE: usize = 4096;

pub struct RosHal;

/// Bounce buffers of shared driver memory: (device address, our copy, length).
static BOUNCE: Spinlock<Vec<(u64, usize, usize)>> = Spinlock::new(Vec::new());

/// Bounce buffers not in use: (pages, device address, our address). The
/// kernel hands out contiguous pages from memory never used before (they
/// are not taken back), so the buffers are kept and reused, not unmapped.
static POOL: Spinlock<Vec<(usize, u64, usize)>> = Spinlock::new(Vec::new());

fn bounce(pages: usize) -> (u64, NonNull<u8>) {
    let hit = {
        let mut p = POOL.lock();
        p.iter().position(|x| x.0 == pages).map(|i| p.swap_remove(i))
    };
    match hit {
        Some((_, pa, va)) => (pa, NonNull::new(va as *mut u8).unwrap()),
        None => dma(pages),
    }
}

fn dma(pages: usize) -> (u64, NonNull<u8>) {
    let size = (pages * PAGE) as u64;
    let m = Memory::create_dma(size).expect("DMA memory");
    let va = m.map(0, size, map::READ | map::WRITE).expect("DMA map");
    let pa = m.phys(0).expect("DMA address");
    let _ = m.0.close();
    (pa, NonNull::new(va).unwrap())
}

unsafe impl Hal for RosHal {
    fn dma_alloc(pages: usize, _dir: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let (pa, va) = dma(pages);
        (pa as PhysAddr, va)
    }

    unsafe fn dma_dealloc(_pa: PhysAddr, va: NonNull<u8>, pages: usize) -> i32 {
        let _ = Memory::unmap(va.as_ptr(), (pages * PAGE) as u64);
        0
    }

    unsafe fn mmio_phys_to_virt(_pa: PhysAddr, _size: usize) -> NonNull<u8> {
        panic!("virtio: only MMIO transports from Mmio handles")
    }

    /// Driver memory is not physically contiguous: the device gets a copy.
    unsafe fn share(buffer: NonNull<[u8]>, dir: BufferDirection) -> PhysAddr {
        let len = buffer.len();
        let (pa, va) = bounce(len.div_ceil(PAGE));
        if dir != BufferDirection::DeviceToDriver {
            unsafe { core::ptr::copy_nonoverlapping(buffer.as_ptr() as *const u8, va.as_ptr(), len) };
        }
        BOUNCE.lock().push((pa, va.as_ptr() as usize, len));
        pa as PhysAddr
    }

    unsafe fn unshare(pa: PhysAddr, buffer: NonNull<[u8]>, dir: BufferDirection) {
        let (va, len) = {
            let mut b = BOUNCE.lock();
            let i = b.iter().position(|x| x.0 == pa as u64).expect("unshare of an unknown buffer");
            let (_, va, len) = b.swap_remove(i);
            (va, len)
        };
        if dir != BufferDirection::DriverToDevice {
            unsafe { core::ptr::copy_nonoverlapping(va as *const u8, buffer.as_ptr() as *mut u8, len.min(buffer.len())) };
        }
        POOL.lock().push((len.div_ceil(PAGE), pa as u64, va));
    }
}

/// The device's registers, mapped, as a transport.
pub fn transport(mmio: Mmio) -> libsys::Result<MmioTransport<'static>> {
    let regs = mmio.map()?;
    let (_, size, _) = mmio.info()?;
    let header = NonNull::new(regs as *mut VirtIOHeader).ok_or(libsys::Error::Fault)?;
    unsafe { MmioTransport::new(header, size as usize) }.map_err(|_| libsys::Error::NotSupported)
}
