//! Kernel console: `print!` / `println!` to the boot UART (or the wasm host),
//! plus a small input ring filled by the UART receive interrupt.
use core::fmt::{self, Write};

use crate::arch;
use crate::sync::IrqLock;

struct Out;

impl Write for Out {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        arch::console_write(s.as_bytes());
        Ok(())
    }
}

static OUT: IrqLock<()> = IrqLock::new(());

pub fn print(args: fmt::Arguments) {
    let _g = OUT.lock();
    let _ = Out.write_fmt(args);
}

/// Raw bytes (user output), written in one piece.
pub fn write_bytes(b: &[u8]) {
    let _g = OUT.lock();
    arch::console_write(b);
}

pub fn force_unlock() {
    unsafe { OUT.force_unlock() }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::console::print(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::console::print(format_args!("{}\n", format_args!($($arg)*))) };
}

/// Typed-ahead input (a test script, a paste) waits here for the console service.
const IN_SIZE: usize = 64 * 1024;

struct Input {
    buf: [u8; IN_SIZE],
    head: usize,
    len: usize,
}

static INPUT: IrqLock<Input> = IrqLock::new(Input { buf: [0; IN_SIZE], head: 0, len: 0 });

/// Called from the receive interrupt.
pub fn input_byte(b: u8) {
    {
        let mut i = INPUT.lock();
        if i.len < IN_SIZE {
            let at = (i.head + i.len) % IN_SIZE;
            i.buf[at] = b;
            i.len += 1;
        }
    }
    crate::serial::input_ready();
}

pub fn input_empty() -> bool {
    INPUT.lock().len == 0
}

pub fn read_byte() -> Option<u8> {
    let mut i = INPUT.lock();
    if i.len == 0 {
        return None;
    }
    let b = i.buf[i.head];
    i.head = (i.head + 1) % IN_SIZE;
    i.len -= 1;
    Some(b)
}
