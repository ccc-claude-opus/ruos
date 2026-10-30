//! NS16550A UART (reg-shift 0).
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

static BASE: AtomicU64 = AtomicU64::new(0);

const RBR: u64 = 0;
const THR: u64 = 0;
const IER: u64 = 1;
const MCR: u64 = 4;
const LSR: u64 = 5;

fn reg(off: u64) -> *mut u8 {
    (BASE.load(Ordering::Relaxed) + off) as *mut u8
}

pub fn init(base: u64) {
    BASE.store(base, Ordering::Relaxed);
}

pub fn write(b: &[u8]) {
    if BASE.load(Ordering::Relaxed) == 0 {
        return;
    }
    for &c in b {
        if c == b'\n' {
            put(b'\r');
        }
        put(c);
    }
}

fn put(c: u8) {
    unsafe {
        while read_volatile(reg(LSR)) & 0x20 == 0 {}
        write_volatile(reg(THR), c);
    }
}

/// Enables the receive interrupt.
pub fn enable_rx() {
    unsafe {
        write_volatile(reg(MCR), 0x08); // OUT2 gates the interrupt line on a real 16550
        write_volatile(reg(IER), 0x01);
    }
}

/// Receive interrupt: drains the FIFO into the console input.
pub fn irq() {
    unsafe {
        while read_volatile(reg(LSR)) & 1 != 0 {
            crate::console::input_byte(read_volatile(reg(RBR)));
        }
    }
}
