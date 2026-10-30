//! Native per-thread kernel state: a kernel stack whose top holds a 16-byte
//! slot (riscv64: the CPU it runs on) and below it the thread's user trap
//! frame, and the saved kernel context.
use alloc::boxed::Box;
use alloc::vec;
use core::cell::UnsafeCell;

use abi::Error;

use crate::arch::{self, Context};
use crate::task::Start;

const STACK_SIZE: usize = 16 * 1024;
const SLOT: u64 = 16;

pub struct ThreadArch {
    stack: Box<[u128]>,
    ctx: UnsafeCell<Context>,
    /// User floating point / SIMD registers while the thread is off a CPU.
    fp: UnsafeCell<arch::FpState>,
}

// The context is only touched by the scheduler with interrupts off.
unsafe impl Sync for ThreadArch {}
unsafe impl Send for ThreadArch {}

impl ThreadArch {
    pub fn new(start: &Start) -> Result<ThreadArch, Error> {
        let stack = vec![0u128; STACK_SIZE / 16].into_boxed_slice();
        let top = stack.as_ptr() as u64 + STACK_SIZE as u64;
        let ctx = arch::init_thread(top - SLOT, start);
        Ok(ThreadArch { stack, ctx: UnsafeCell::new(ctx), fp: UnsafeCell::new(arch::FpState::ZERO) })
    }

    /// Where the user trap frame ends (the per-thread slot starts).
    pub fn frame_top(&self) -> u64 {
        self.stack.as_ptr() as u64 + STACK_SIZE as u64 - SLOT
    }

    pub fn context(&self) -> *mut Context {
        self.ctx.get()
    }

    pub fn fp(&self) -> *mut arch::FpState {
        self.fp.get()
    }
}
