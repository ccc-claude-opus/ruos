//! ruos kernel: a capability microkernel, compiled to wasm32 and run in the
//! browser's kernel Worker (the host is web/kernel_host.js).
//!
//! The host hands it a DTB; `arch` parses it into a [`boot::BootInfo`] and
//! calls [`kmain`].
#![no_std]
#![no_main]

extern crate alloc;

#[macro_use]
mod console;
mod arch;
mod boot;
mod bootimg;
mod channel;
mod dtb;
mod heap;
mod memory;
mod object;
mod port;
mod quota;
mod random;
mod sched;
mod selftest;
mod serial;
mod signal;
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
    task::start_init();
    arch::enable_interrupts();
}

#[panic_handler]
fn panic(pi: &core::panic::PanicInfo) -> ! {
    arch::disable_interrupts();
    console::force_unlock();
    println!("panic: {}", pi);
    arch::shutdown(1)
}
