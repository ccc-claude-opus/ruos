//! PL011 UART.
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

static BASE: AtomicU64 = AtomicU64::new(0);

const DR: u64 = 0x00;
const FR: u64 = 0x18;
const IMSC: u64 = 0x38;
const ICR: u64 = 0x44;
const FR_RXFE: u32 = 1 << 4;
const FR_TXFF: u32 = 1 << 5;
const INT_RX: u32 = 1 << 4;
const INT_RT: u32 = 1 << 6;

fn reg(off: u64) -> *mut u32 {
    (BASE.load(Ordering::Relaxed) + off) as *mut u32
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
        while read_volatile(reg(FR)) & FR_TXFF != 0 {}
        write_volatile(reg(DR), c as u32);
    }
}

pub fn enable_rx() {
    unsafe { write_volatile(reg(IMSC), INT_RX | INT_RT) }
}

/// Receive interrupt: drains the FIFO into the console input. The
/// interrupt is cleared first, so a byte that arrives meanwhile raises it again.
pub fn irq() {
    unsafe {
        write_volatile(reg(ICR), INT_RX | INT_RT);
        while read_volatile(reg(FR)) & FR_RXFE == 0 {
            crate::console::input_byte(read_volatile(reg(DR)) as u8);
        }
    }
}
