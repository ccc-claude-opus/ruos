//! The serial console as a kernel object (only init gets it; the console
//! service serves everyone else over channels).
use alloc::sync::Arc;

use abi::signal;

use crate::console;
use crate::signal::SignalCell;
use crate::sync::IrqLock;

pub struct Serial {
    pub sig: SignalCell,
}

static SERIAL: IrqLock<Option<Arc<Serial>>> = IrqLock::new(None);

pub fn get() -> Arc<Serial> {
    SERIAL.lock().get_or_insert_with(|| Arc::new(Serial { sig: SignalCell::new(signal::WRITABLE) })).clone()
}

/// New input arrived (interrupt context).
pub fn input_ready() {
    let s = SERIAL.lock().clone();
    if let Some(s) = s {
        s.sig.update(0, signal::READABLE);
    }
}

impl Serial {
    pub fn read(&self, buf: &mut [u8]) -> usize {
        let mut n = 0;
        while n < buf.len() {
            match console::read_byte() {
                Some(b) => {
                    buf[n] = b;
                    n += 1;
                }
                None => break,
            }
        }
        if console::input_empty() {
            self.sig.update(signal::READABLE, 0);
        }
        n
    }
}
