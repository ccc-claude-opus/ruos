//! ruos kernel: a capability microkernel for riscv64, aarch64 and wasm32.
//!
//! The architecture code (`arch::*`) boots the machine, parses the DTB into a
//! [`boot::BootInfo`] and calls [`kmain`]; everything else is shared.
#![no_std]
#![no_main]

extern crate alloc;

#[macro_use]
mod console;
mod arch;
mod boot;
mod bootimg;
mod channel;
#[cfg(not(target_arch = "wasm32"))]
mod device;
mod dtb;
#[cfg(not(target_arch = "wasm32"))]
mod elf;
mod heap;
#[cfg(not(target_arch = "wasm32"))]
mod kstack;
mod memory;
#[cfg(not(target_arch = "wasm32"))]
mod mm;
mod object;
mod port;
mod quota;
mod random;
#[cfg_attr(target_arch = "wasm32", path = "arch/wasm32/sched.rs")]
mod sched;
mod selftest;
mod serial;
mod signal;
#[cfg(target_arch = "wasm32")]
mod snapshot;
mod sync;
mod syscall;
mod task;
mod time;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Timer interrupts per second.
pub const HZ: u64 = 100;

/// Common initialization. Returns with interrupts enabled; the native
/// architectures then idle in `arch::idle`, wasm32 returns to the host.
pub fn kmain(info: &boot::BootInfo) {
    println!("ruos {} ({})", VERSION, arch::NAME);
    println!("dtb: model \"{}\", {} bytes at {:#x}", info.model, info.dtb_size, info.dtb_pa);
    for r in info.memory() {
        println!("memory: {:#x}..{:#x} ({} MiB)", r.base, r.end(), r.size >> 20);
    }
    for r in info.reserved() {
        println!("reserved: {:#x}..{:#x}", r.base, r.end());
    }
    if let Some(c) = &info.console {
        println!("console: {} at {:#x}", c.compatible, c.reg.base);
    }
    heap::init();
    arch::init_memory(info);
    arch::init_traps();
    selftest::breakpoint();
    arch::init_irq(info);
    time::start(info);
    random::init(info);
    arch::start_cpus(info);
    task::start_init(info);
    arch::enable_interrupts();
}

#[panic_handler]
fn panic(pi: &core::panic::PanicInfo) -> ! {
    arch::disable_interrupts();
    console::force_unlock();
    println!("panic: {}", pi);
    arch::shutdown(1)
}
