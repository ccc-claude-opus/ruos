//! `print!` / `println!`: each call is formatted into a buffer and written
//! at once, so lines from different tasks do not interleave. A program
//! whose namespace has `log` (a pod: dock keeps its output) writes there,
//! anything else to the kernel console.
use core::fmt::{self, Write};

struct Buf {
    b: [u8; 256],
    n: usize,
}

impl Write for Buf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &c in s.as_bytes() {
            if self.n == self.b.len() {
                out(&self.b);
                self.n = 0;
            }
            self.b[self.n] = c;
            self.n += 1;
        }
        Ok(())
    }
}

/// Writes `b` where `print!` goes (`log`, else the kernel console).
pub fn out(b: &[u8]) {
    match crate::ns::get("log") {
        Some(h) if crate::Channel(h).write(b, &[]).is_ok() => {}
        _ => crate::debug_write(b),
    }
}

pub fn print(args: fmt::Arguments) {
    let mut b = Buf { b: [0; 256], n: 0 };
    let _ = b.write_fmt(args);
    out(&b.b[..b.n]);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::fmt::print(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::fmt::print(format_args!("{}\n", format_args!($($arg)*))) };
}
