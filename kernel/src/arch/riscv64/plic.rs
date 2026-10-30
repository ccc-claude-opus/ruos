//! Platform-level interrupt controller: the console's receive interrupt,
//! routed to this hart's S-mode context.
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

use super::{PHYS_OFFSET, uart};
use crate::boot::BootInfo;
use crate::dtb::{Reg, cells};

static BASE: AtomicU64 = AtomicU64::new(0);
static CONTEXT: AtomicU64 = AtomicU64::new(0);
static UART_IRQ: AtomicU64 = AtomicU64::new(0);

const COMPAT: &[&str] = &["sifive,plic-1.0.0", "riscv,plic0"];

pub fn find(info: &BootInfo) -> Option<Reg> {
    info.fdt.find_compatible(COMPAT)?.reg().next()
}

fn reg(off: u64) -> *mut u32 {
    (BASE.load(Ordering::Relaxed) + off) as *mut u32
}

pub fn init(info: &BootInfo) {
    let Some(node) = info.fdt.find_compatible(COMPAT) else {
        println!("irq: no PLIC");
        return;
    };
    let Some(r) = node.reg().next() else { return };
    // Context = index of the (boot hart's interrupt controller, 9) pair:
    // its S-mode external interrupt. The boot hart is not always hart 0.
    let ext: alloc::vec::Vec<u32> = node.prop("interrupts-extended").map(|v| cells(v).collect()).unwrap_or_default();
    let boot = super::boot_hart() as u64;
    let mut intc = None;
    if let Some(cpus) = info.fdt.find("/cpus") {
        cpus.children(|c| {
            if c.reg().next().map(|r| r.base) == Some(boot) {
                c.children(|ic| {
                    if ic.prop("interrupt-controller").is_some() {
                        intc = ic.prop_u32("phandle");
                    }
                });
            }
        });
    }
    let ctx = ext
        .chunks(2)
        .position(|p| p.len() == 2 && p[1] == 9 && intc.is_none_or(|h| p[0] == h))
        .unwrap_or(1) as u64;
    BASE.store(r.base + PHYS_OFFSET, Ordering::Relaxed);
    CONTEXT.store(ctx, Ordering::Relaxed);
    let irq = info.console.map(|c| c.irq[0]).unwrap_or(0) as u64;
    UART_IRQ.store(irq, Ordering::Relaxed);
    unsafe {
        write_volatile(reg(0x20_0000 + 0x1000 * ctx), 0); // threshold
        if irq != 0 {
            write_volatile(reg(4 * irq), 1); // priority
            let en = reg(0x2000 + 0x80 * ctx + (irq / 32) * 4);
            write_volatile(en, read_volatile(en) | 1 << (irq % 32));
            uart::enable_rx();
        }
        core::arch::asm!("csrs sie, {}", in(reg) 1u64 << 9);
    }
    println!("irq: PLIC at {:#x}, context {}, uart irq {}", r.base, ctx, irq);
}

/// External interrupt: claim, dispatch, complete.
pub fn irq() {
    let ctx = CONTEXT.load(Ordering::Relaxed);
    let claim = reg(0x20_0004 + 0x1000 * ctx);
    loop {
        let id = unsafe { read_volatile(claim) };
        if id == 0 {
            break;
        }
        if id as u64 == UART_IRQ.load(Ordering::Relaxed) {
            uart::irq();
        } else {
            crate::device::dispatch(id);
        }
        unsafe { write_volatile(claim, id) };
    }
}

fn set_enable(irq: u32, on: bool) {
    let ctx = CONTEXT.load(Ordering::Relaxed);
    unsafe {
        write_volatile(reg(4 * irq as u64), 1);
        let en = reg(0x2000 + 0x80 * ctx + (irq as u64 / 32) * 4);
        let bit = 1 << (irq % 32);
        let v = read_volatile(en);
        write_volatile(en, if on { v | bit } else { v & !bit });
    }
}

pub fn enable(irq: u32) {
    set_enable(irq, true)
}

pub fn disable(irq: u32) {
    set_enable(irq, false)
}
