//! The console service's protocol and its client. Every request is one
//! channel message whose first byte is the operation.
use core::fmt;

use crate::{Channel, Handle, Result};

/// [WRITE, bytes...]: print.
pub const WRITE: u8 = 1;
/// [READ_LINE]: the reply is [READ_LINE, line bytes...] (no newline) once a
/// line has been typed.
pub const READ_LINE: u8 = 2;
/// [CONNECT] + one channel handle: serve another client on it.
pub const CONNECT: u8 = 3;
/// The reply [EOF] to READ_LINE: there is no more input (a program whose
/// input is a pipe, served by the shell). `read_line` gives PeerClosed.
pub const EOF: u8 = 4;

pub const MAX_LINE: usize = 256;

#[derive(Clone, Copy)]
pub struct Console(pub Channel);

impl Console {
    pub fn write(&self, s: &[u8]) -> Result<()> {
        let mut buf = [0u8; 1 + 512];
        for chunk in s.chunks(512) {
            buf[0] = WRITE;
            buf[1..1 + chunk.len()].copy_from_slice(chunk);
            self.0.write(&buf[..1 + chunk.len()], &[])?;
        }
        Ok(())
    }

    /// Reads one line into `buf`; returns its length.
    pub fn read_line(&self, buf: &mut [u8]) -> Result<usize> {
        self.0.write(&[READ_LINE], &[])?;
        let mut m = [0u8; 1 + MAX_LINE];
        let (n, _) = self.0.recv(&mut m, &mut [])?;
        if m[0] == EOF {
            return Err(crate::Error::PeerClosed);
        }
        let n = n.saturating_sub(1).min(buf.len());
        buf[..n].copy_from_slice(&m[1..1 + n]);
        Ok(n)
    }

    /// Gives the console another client endpoint.
    pub fn connect(&self, client: Handle) -> Result<()> {
        self.0.write(&[CONNECT], &[client])
    }

    pub fn print(&self, args: fmt::Arguments) {
        let mut w = Writer { c: *self, b: [0; 256], n: 0 };
        let _ = fmt::Write::write_fmt(&mut w, args);
        let _ = self.write(&w.b[..w.n]);
    }
}

struct Writer {
    c: Console,
    b: [u8; 256],
    n: usize,
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &x in s.as_bytes() {
            if self.n == self.b.len() {
                let _ = self.c.write(&self.b);
                self.n = 0;
            }
            self.b[self.n] = x;
            self.n += 1;
        }
        Ok(())
    }
}

/// `cprintln!(console, ...)`
#[macro_export]
macro_rules! cprintln {
    ($c:expr, $($arg:tt)*) => { $c.print(format_args!("{}\n", format_args!($($arg)*))) };
}

#[macro_export]
macro_rules! cprint {
    ($c:expr, $($arg:tt)*) => { $c.print(format_args!($($arg)*)) };
}
