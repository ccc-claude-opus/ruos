//! Physical memory and kernel page tables (native architectures).
pub mod aspace;
pub mod frame;
pub mod paging;

use crate::arch::{self, phys_to_virt};
use crate::boot::BootInfo;
use crate::dtb::Reg;
use paging::{Flags, PageTable};

pub const PAGE_SIZE: u64 = 4096;

unsafe extern "C" {
    static __kernel_start: u8;
    static __text_end: u8;
    static __rodata_end: u8;
    static __kernel_end: u8;
}

fn sym(s: &u8) -> u64 {
    s as *const u8 as u64
}

/// Physical range of the kernel image (including bss and boot stack).
pub fn kernel_phys() -> Reg {
    let (s, e) = unsafe { (sym(&__kernel_start), sym(&__kernel_end)) };
    Reg { base: s - arch::KERNEL_OFFSET, size: e - s }
}

/// Starts the frame allocator and builds the kernel's address space: the
/// image with per-section permissions, all RAM in the direct map, and the
/// device registers in `devices` (direct map, device memory).
pub fn init(info: &BootInfo, devices: &[Reg]) -> PageTable {
    frame::init(info.usable(kernel_phys()));
    let mut pt = PageTable::new();
    let g = Flags::GLOBAL | Flags::READ;
    let (ks, te, re, ke) = unsafe {
        (sym(&__kernel_start), sym(&__text_end), sym(&__rodata_end), sym(&__kernel_end))
    };
    let off = arch::KERNEL_OFFSET;
    pt.map(ks, ks - off, te - ks, g | Flags::EXEC);
    pt.map(te, te - off, re - te, g);
    pt.map(re, re - off, ke - re, g | Flags::WRITE);
    for r in info.memory() {
        pt.map(phys_to_virt(r.base), r.base, r.size, g | Flags::WRITE);
    }
    for d in devices {
        let b = d.base & !(PAGE_SIZE - 1);
        let e = (d.end() + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        pt.map(phys_to_virt(b), b, e - b, g | Flags::WRITE | Flags::DEVICE);
    }
    assert_eq!(pt.translate(ks), Some(ks - off), "kernel mapping");
    pt
}
