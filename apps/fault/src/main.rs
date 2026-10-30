//! fault: reads memory it does not own; the kernel must kill it.
#![no_std]
#![no_main]

use libsys::println;

libsys::entry!(main);

/// Kernel memory (native) or past the end of linear memory (wasm32).
#[cfg(target_arch = "riscv64")]
const ADDR: usize = 0xffff_ffc0_8020_0000;
#[cfg(target_arch = "aarch64")]
const ADDR: usize = 0xffff_0000_4008_0000;
#[cfg(target_arch = "wasm32")]
const ADDR: usize = 0xffff_fff0;

fn main(_arg: usize) -> i64 {
    println!("fault: reading {:#x}", ADDR);
    let v = unsafe { core::ptr::read_volatile(ADDR as *const u32) };
    println!("fault: read {:#x} (should not happen)", v);
    0
}
