//! GICv2: the virtual timer's PPI and the console's SPI.
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::{PHYS_OFFSET, pl011};
use crate::boot::BootInfo;
use crate::dtb::{Reg, cells};

const COMPAT: &[&str] = &["arm,cortex-a15-gic", "arm,gic-400"];

static DIST: AtomicU64 = AtomicU64::new(0);
static CPU: AtomicU64 = AtomicU64::new(0);
static TIMER: AtomicU32 = AtomicU32::new(27);
static UART: AtomicU32 = AtomicU32::new(0);

const SPURIOUS: u32 = 1023;

pub fn find(info: &BootInfo) -> Option<(Reg, Reg)> {
    let mut r = info.fdt.find_compatible(COMPAT)?.reg();
    Some((r.next()?, r.next()?))
}

/// A three-cell GIC interrupt specifier → interrupt id.
fn id(spec: &[u32]) -> u32 {
    match spec[0] {
        0 => 32 + spec[1],
        _ => 16 + spec[1],
    }
}

fn dist(off: u64) -> *mut u32 {
    (DIST.load(Ordering::Relaxed) + off) as *mut u32
}

fn cpu(off: u64) -> *mut u32 {
    (CPU.load(Ordering::Relaxed) + off) as *mut u32
}

pub fn enable(id: u32) {
    unsafe {
        let byte = |off: u64, v: u8| {
            write_volatile((DIST.load(Ordering::Relaxed) + off + id as u64) as *mut u8, v)
        };
        byte(0x400, 0x80); // priority
        if id >= 32 {
            byte(0x800, 1); // target CPU 0
        }
        write_volatile(dist(0x100 + 4 * (id as u64 / 32)), 1 << (id % 32));
    }
}

pub fn init(info: &BootInfo) {
    let Some((d, c)) = find(info) else {
        println!("irq: no GIC");
        return;
    };
    DIST.store(d.base + PHYS_OFFSET, Ordering::Relaxed);
    CPU.store(c.base + PHYS_OFFSET, Ordering::Relaxed);
    // /timer interrupts: secure, non-secure, virtual, hypervisor.
    if let Some(v) = info.fdt.find_compatible(&["arm,armv8-timer"]).and_then(|n| n.prop("interrupts")) {
        let s: alloc::vec::Vec<u32> = cells(v).collect();
        if s.len() >= 9 {
            TIMER.store(id(&s[6..9]), Ordering::Relaxed);
        }
    }
    let uart = info.console.map(|c| id(&c.irq)).unwrap_or(0);
    UART.store(uart, Ordering::Relaxed);
    unsafe {
        write_volatile(dist(0), 0);
        enable(TIMER.load(Ordering::Relaxed));
        if uart != 0 {
            enable(uart);
            pl011::enable_rx();
        }
        write_volatile(dist(0), 1);
    }
    init_cpu();
    println!(
        "irq: GICv2 at {:#x}, timer irq {}, uart irq {}",
        d.base,
        TIMER.load(Ordering::Relaxed),
        uart
    );
}

pub const SGI_RESCHED: u32 = 1;

/// This CPU's interface, its (banked) timer interrupt and the IPI.
pub fn init_cpu() {
    enable(TIMER.load(Ordering::Relaxed));
    enable(SGI_RESCHED);
    unsafe {
        write_volatile(cpu(0x04), 0xf0); // priority mask
        write_volatile(cpu(0x00), 1);
    }
}

/// An IPI to CPU interface `target` (= CPU index on QEMU virt).
pub fn send_sgi(target: usize) {
    unsafe { write_volatile(dist(0xf00), (1 << (16 + target)) | SGI_RESCHED) };
}

pub fn irq() {
    loop {
        let iar = unsafe { read_volatile(cpu(0x0c)) };
        let id = iar & 0x3ff;
        if id == SPURIOUS {
            break;
        }
        if id == TIMER.load(Ordering::Relaxed) {
            super::timer_irq();
        } else if id < 16 {
            crate::sched::ipi();
        } else if id == UART.load(Ordering::Relaxed) {
            pl011::irq();
        } else {
            crate::device::dispatch(id);
        }
        unsafe { write_volatile(cpu(0x10), iar) };
    }
}

pub fn disable(id: u32) {
    unsafe { write_volatile(dist(0x180 + 4 * (id as u64 / 32)), 1 << (id % 32)) };
}

/// A three-cell interrupt specifier (from the DTB) → interrupt id.
pub fn number(spec: &[u32; 3]) -> u32 {
    id(spec)
}
