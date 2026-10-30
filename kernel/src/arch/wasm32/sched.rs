//! wasm32 "scheduler": every thread runs in its own Worker, truly in
//! parallel, so there is nothing to switch. A blocked thread waits in
//! `Atomics.wait` until the kernel resumes it with its result.
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::arch::host;
use crate::sync::IrqLock;
use crate::task::{Run, Thread};

/// (deadline, thread, its wait generation, result on timeout)
static SLEEPERS: IrqLock<Vec<(u64, Arc<Thread>, u64, i64)>> = IrqLock::new(Vec::new());
static CURRENT: IrqLock<Option<Arc<Thread>>> = IrqLock::new(None);

#[allow(dead_code)]
pub fn current() -> Option<Arc<Thread>> {
    CURRENT.lock().clone()
}

pub fn set_current(t: Option<Arc<Thread>>) {
    *CURRENT.lock() = t;
}

/// Starts the thread's Worker.
pub fn start(t: Arc<Thread>) {
    t.set_run(Run::Running);
    let s = t.start;
    unsafe { host::thread_start(t.task.aspace.host(), t.id, s.entry as u32, s.stack as u32, s.arg as u32) };
}

pub fn block_current(t: &Arc<Thread>) -> u64 {
    let mut st = t.state.lock();
    st.run = Run::Blocked;
    st.wgen += 1;
    st.wgen
}

pub fn waiting(t: &Arc<Thread>, wgen: u64) -> bool {
    let st = t.state.lock();
    st.run == Run::Blocked && st.wgen == wgen
}

pub fn timeout(t: &Arc<Thread>, wgen: u64, timeout_ns: u64, value: i64) {
    if timeout_ns == u64::MAX {
        return;
    }
    let at = crate::time::uptime_ns().saturating_add(timeout_ns);
    SLEEPERS.lock().push((at, t.clone(), wgen, value));
}

pub fn sleep(t: &Arc<Thread>, ns: u64) {
    let wgen = block_current(t);
    timeout(t, wgen, ns, 0);
}

pub fn wake(t: &Arc<Thread>, v: i64) {
    let wgen = t.state.lock().wgen;
    wake_gen(t, wgen, v);
}

pub fn wake_gen(t: &Arc<Thread>, wgen: u64, v: i64) {
    {
        let mut st = t.state.lock();
        if st.run != Run::Blocked || st.wgen != wgen {
            return;
        }
        st.run = Run::Running;
    }
    SLEEPERS.lock().retain(|(_, x, _, _)| !Arc::ptr_eq(x, t));
    unsafe { host::thread_resume(t.id, v) };
}

pub fn remove(t: &Arc<Thread>) {
    t.set_run(Run::Dead);
    SLEEPERS.lock().retain(|(_, x, _, _)| !Arc::ptr_eq(x, t));
    unsafe { host::thread_kill(t.id) };
}

pub fn yield_now() {}

pub fn tick(now_ns: u64) {
    let due: Vec<_> = {
        let mut s = SLEEPERS.lock();
        let (due, rest) = core::mem::take(&mut *s).into_iter().partition(|(at, ..)| *at <= now_ns);
        *s = rest;
        due
    };
    for (_, t, wgen, v) in due {
        wake_gen(&t, wgen, v);
    }
}
