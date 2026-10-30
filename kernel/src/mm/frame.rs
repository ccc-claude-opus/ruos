//! Physical page frames: a bump pointer over the usable regions plus a
//! free list of returned frames (linked through the direct map).
use crate::arch::phys_to_virt;
use crate::boot::Regions;
use crate::dtb::Reg;
use crate::sync::IrqLock;

use super::PAGE_SIZE;

struct Frames {
    regions: Regions,
    region: usize,
    next: u64,
    free: u64,
    total: u64,
    used: u64,
}

static FRAMES: IrqLock<Frames> = IrqLock::new(Frames {
    regions: Regions::new(),
    region: 0,
    next: 0,
    free: 0,
    total: 0,
    used: 0,
});

pub fn init(usable: Regions) {
    let mut f = FRAMES.lock();
    f.total = usable.iter().map(|r| r.size / PAGE_SIZE).sum();
    f.next = usable.iter().next().map(|r| r.base).unwrap_or(0);
    f.regions = usable;
}

/// A zeroed frame's physical address.
pub fn alloc() -> Option<u64> {
    let mut g = FRAMES.lock();
    let f = &mut *g;
    let pa = if f.free != 0 {
        let pa = f.free;
        f.free = unsafe { *(phys_to_virt(pa) as *const u64) };
        pa
    } else {
        loop {
            let r: Reg = *f.regions.iter().nth(f.region)?;
            if f.next >= r.base && f.next + PAGE_SIZE <= r.end() {
                let pa = f.next;
                f.next += PAGE_SIZE;
                break pa;
            }
            f.region += 1;
            let i = f.region;
            f.next = f.regions.iter().nth(i).map(|r| r.base).unwrap_or(0);
        }
    };
    f.used += 1;
    drop(g);
    unsafe { core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, PAGE_SIZE as usize) };
    Some(pa)
}

/// `n` zeroed, physically contiguous frames (for DMA); the first's address.
pub fn alloc_contiguous(n: u64) -> Option<u64> {
    let mut g = FRAMES.lock();
    let f = &mut *g;
    let pa = loop {
        let r: Reg = *f.regions.iter().nth(f.region)?;
        if f.next >= r.base && f.next + n * PAGE_SIZE <= r.end() {
            let pa = f.next;
            f.next += n * PAGE_SIZE;
            break pa;
        }
        f.region += 1;
        let i = f.region;
        f.next = f.regions.iter().nth(i).map(|r| r.base).unwrap_or(0);
    };
    f.used += n;
    drop(g);
    unsafe { core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, (n * PAGE_SIZE) as usize) };
    Some(pa)
}

pub fn free(pa: u64) {
    let mut f = FRAMES.lock();
    unsafe { *(phys_to_virt(pa) as *mut u64) = f.free };
    f.free = pa;
    f.used -= 1;
}

/// (used, total) frames.
pub fn stats() -> (u64, u64) {
    let f = FRAMES.lock();
    (f.used, f.total)
}
