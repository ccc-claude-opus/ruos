//! The program's `sys.call`: system calls with pointer arguments moved into
//! its linear memory (and checked against it). THREAD_CREATE starts a
//! native thread with a new instance; mapping memory and devices is not
//! available to a wasm program (as on wasm32 itself).
use libsys::{Error, nr};

use crate::exec::{Host, Instance};
use crate::module::Trap;
use crate::{Start, spawn_thread};

pub struct Sys<'a> {
    pub start: &'a Start,
}

/// Native address of `len` bytes at program address `a` (0 if outside).
fn ptr(i: &Instance, a: u64, len: u64) -> Option<u64> {
    i.mem.at(a as u32 as u64, len).ok().map(|p| p as u64)
}

impl Host for Sys<'_> {
    fn call(&self, inst: &mut Instance, import: usize, a: &[u64]) -> Result<Option<u64>, Trap> {
        let (module, name) = &inst.m.imports[import];
        if module != "sys" || name != "call" || a.len() != 7 {
            return Err("unknown import");
        }
        let n = a[0] as u32 as usize;
        let mut x = [a[1], a[2], a[3], a[4], a[5], a[6]];
        let fault = Error::Fault.code() as u64;
        // Replaces argument `i` (a program address of `len` bytes).
        macro_rules! p {
            ($i:expr, $len:expr) => {
                match ptr(inst, x[$i], $len) {
                    Some(v) => x[$i] = v,
                    None => return Ok(Some(fault)),
                }
            };
        }
        match n {
            nr::DEBUG_WRITE | nr::SERIAL_WRITE | nr::SERIAL_READ => {
                let (i, l) = if n == nr::DEBUG_WRITE { (0, x[1]) } else { (1, x[2]) };
                p!(i, l);
            }
            nr::WAIT => p!(1, 8),
            nr::CHANNEL_CREATE => p!(0, 8),
            nr::CHANNEL_WRITE => {
                p!(1, x[2]);
                p!(3, 4 * x[4]);
            }
            nr::CHANNEL_READ | nr::CHANNEL_RECV => {
                p!(1, x[2]);
                p!(3, 4 * x[4]);
                p!(5, 8);
            }
            nr::WAIT_ONE => p!(3, 4),
            nr::PORT_WAIT => p!(2, 24),
            nr::MEMORY_READ | nr::MEMORY_WRITE => p!(2, x[3]),
            nr::QUOTA_INFO => p!(1, 48),
            nr::SPAWN => {
                let Some(q) = ptr(inst, x[0], 48) else { return Ok(Some(fault)) };
                let mut args: abi_args::SpawnArgs = unsafe { (q as *const abi_args::SpawnArgs).read_unaligned() };
                let (Some(name), Some(hs)) = (ptr(inst, args.name_ptr, args.name_len), ptr(inst, args.handles_ptr, 4 * args.handles_len))
                else {
                    return Ok(Some(fault));
                };
                args.name_ptr = name;
                args.handles_ptr = hs;
                x[0] = &args as *const _ as u64;
                return Ok(Some(raw(n, x) as u64));
            }
            nr::THREAD_CREATE => {
                let s = Start { m: self.start.m.clone(), mem: self.start.mem.clone(), entry: x[0] as u32, stack: x[1] as u32, arg: x[2] as u32 };
                return Ok(Some(match spawn_thread(s) {
                    Ok(h) => h.0 as u64,
                    Err(e) => e.code() as u64,
                }));
            }
            nr::MEMORY_MAP | nr::MEMORY_UNMAP | nr::MMIO_MAP | nr::MMIO_INFO | nr::IRQ_ACK | nr::MEMORY_CREATE_DMA
            | nr::MEMORY_PHYS => return Ok(Some(Error::NotSupported.code() as u64)),
            _ => {}
        }
        Ok(Some(raw(n, x) as u64))
    }
}

mod abi_args {
    pub use libsys::SpawnArgs;
}

fn raw(n: usize, x: [u64; 6]) -> i64 {
    libsys::syscall_raw(n, x)
}
