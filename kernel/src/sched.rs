//! Native scheduler: one run queue shared by all CPUs, round robin,
//! preemption on timer ticks. Each CPU's own state has its own lock (and
//! its idle / reschedule flags are atomics), so a trap that does not switch
//! never touches the shared run queue's lock. The kernel is not preemptible: a CPU switches
//! threads only in `trap_exit`, at the end of a trap, on the interrupted
//! thread's kernel stack.
//!
//! A thread can be woken by another CPU before the CPU it ran on has
//! finished switching away from it. Such a thread is not queued (another
//! CPU could pick it and wait for it, while that CPU waits for one of its
//! own: a deadlock); it is marked `requeue`, and the CPU queues it in
//! `finish_switch`, once it is off the thread's stack. So no thread on the
//! run queue is on a CPU.
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::arch::{self, Context, MAX_CPUS};
use crate::sync::IrqLock;
use crate::task::{Run, Thread};

/// A CPU's own state (its lock is taken before the run queue's).
struct Cpu {
    current: Option<Arc<Thread>>,
    /// The thread switched away from; `finish_switch` lets it go.
    prev: Option<Arc<Thread>>,
}

struct Queue {
    run: VecDeque<Arc<Thread>>,
    /// (deadline, thread, its wait generation, result on timeout)
    sleepers: Vec<(u64, Arc<Thread>, u64, i64)>,
}

static CPUS: [IrqLock<Cpu>; MAX_CPUS] = [const { IrqLock::new(Cpu { current: None, prev: None }) }; MAX_CPUS];
static RESCHED: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
/// Online and running nothing.
static IDLE_NOW: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
static ONLINE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
static QUEUE: IrqLock<Queue> = IrqLock::new(Queue { run: VecDeque::new(), sleepers: Vec::new() });
/// Threads on the run queue (read without its lock).
static QUEUED: AtomicUsize = AtomicUsize::new(0);

/// Each CPU's boot thread context: it idles when nothing is ready.
static mut IDLE: [Context; MAX_CPUS] = [const { Context::ZERO }; MAX_CPUS];

/// This CPU takes part in scheduling.
pub fn cpu_online(cpu: usize) {
    IDLE_NOW[cpu].store(true, Ordering::Release);
    ONLINE[cpu].store(true, Ordering::Release);
}

pub fn current() -> Option<Arc<Thread>> {
    CPUS[arch::cpu_id()].lock().current.clone()
}

/// Asks an idle CPU (if any; another one first) to look at the run queue.
fn kick() {
    let me = arch::cpu_id();
    let idle = |c: usize| ONLINE[c].load(Ordering::Acquire) && IDLE_NOW[c].load(Ordering::Acquire);
    if let Some(c) = (0..MAX_CPUS).find(|&c| c != me && idle(c)).or_else(|| idle(me).then_some(me)) {
        arch::send_ipi(c);
    }
}

fn enqueue(t: Arc<Thread>) {
    QUEUE.lock().run.push_back(t);
    QUEUED.fetch_add(1, Ordering::Release);
    kick();
}

/// A new thread becomes ready.
pub fn start(t: Arc<Thread>) {
    t.set_run(Run::Ready);
    enqueue(t);
}

/// The caller is about to return `Sys::Block`; `wake` resumes it.
/// Returns the wait's generation.
pub fn block_current(t: &Arc<Thread>) -> u64 {
    let mut st = t.state.lock();
    st.run = Run::Blocked;
    st.wgen += 1;
    st.wgen
}

/// Is `t` still blocked in the wait `wgen`?
pub fn waiting(t: &Arc<Thread>, wgen: u64) -> bool {
    let st = t.state.lock();
    st.run == Run::Blocked && st.wgen == wgen
}

/// Wakes `t` with `value` after `timeout_ns` unless something else does first.
pub fn timeout(t: &Arc<Thread>, wgen: u64, timeout_ns: u64, value: i64) {
    if timeout_ns == u64::MAX {
        return;
    }
    let at = crate::time::uptime_ns().saturating_add(timeout_ns);
    QUEUE.lock().sleepers.push((at, t.clone(), wgen, value));
}

pub fn sleep(t: &Arc<Thread>, ns: u64) {
    let wgen = block_current(t);
    timeout(t, wgen, ns, 0);
}

/// Unblocks `t`; `v` is its system call's result.
pub fn wake(t: &Arc<Thread>, v: i64) {
    let wgen = t.state.lock().wgen;
    wake_gen(t, wgen, v);
}

/// Unblocks `t` if it is still in the wait `wgen`.
pub fn wake_gen(t: &Arc<Thread>, wgen: u64, v: i64) {
    let queue = {
        let mut st = t.state.lock();
        if st.run != Run::Blocked || st.wgen != wgen {
            return;
        }
        st.run = Run::Ready;
        arch::set_return(&t.arch, v);
        if t.on_cpu.load(Ordering::Acquire) {
            st.requeue = true;
            false
        } else {
            true
        }
    };
    QUEUE.lock().sleepers.retain(|(_, x, _, _)| !Arc::ptr_eq(x, t));
    if queue {
        enqueue(t.clone());
    }
}

/// The thread is dead: it never runs again. A CPU running it now is asked
/// to switch away.
pub fn remove(t: &Arc<Thread>) {
    t.set_run(Run::Dead);
    {
        let mut q = QUEUE.lock();
        let before = q.run.len();
        q.run.retain(|x| !Arc::ptr_eq(x, t));
        QUEUED.fetch_sub(before - q.run.len(), Ordering::Release);
        q.sleepers.retain(|(_, x, _, _)| !Arc::ptr_eq(x, t));
    }
    if t.on_cpu.load(Ordering::Acquire) {
        let c = t.cpu.load(Ordering::Relaxed);
        if c != arch::cpu_id() {
            RESCHED[c].store(true, Ordering::Release);
            arch::send_ipi(c);
        }
    }
}

pub fn yield_now() {
    RESCHED[arch::cpu_id()].store(true, Ordering::Release);
}

/// An inter-processor interrupt: look at the run queue.
pub fn ipi() {
    RESCHED[arch::cpu_id()].store(true, Ordering::Release);
}

/// Timer tick on this CPU: wake sleepers whose time has come; time slice over.
pub fn tick(now_ns: u64) {
    RESCHED[arch::cpu_id()].store(true, Ordering::Release);
    let due: Vec<_> = {
        let mut q = QUEUE.lock();
        let (due, rest) = core::mem::take(&mut q.sleepers).into_iter().partition(|(at, ..)| *at <= now_ns);
        q.sleepers = rest;
        due
    };
    for (_, t, wgen, v) in due {
        wake_gen(&t, wgen, v);
    }
}

/// Called at the end of every trap: switches threads if the current one
/// blocked or died, or its time slice is over and another is ready.
pub fn trap_exit() {
    let cpu = arch::cpu_id();
    let (from, to, next, prev_fp) = {
        let mut c = CPUS[cpu].lock();
        let cur_run = c.current.as_ref().map(|t| t.run());
        let stays = cur_run == Some(Run::Running);
        let queued = QUEUED.load(Ordering::Acquire) != 0;
        let resched = RESCHED[cpu].swap(false, Ordering::AcqRel);
        if stays && !(resched && queued) {
            return;
        }
        if cur_run.is_none() && !queued {
            return;
        }
        // Woken again before we got off it, and nothing else to do: go on.
        if cur_run == Some(Run::Ready) && !queued {
            let t = c.current.as_ref().unwrap();
            let mut st = t.state.lock();
            st.requeue = false;
            st.run = Run::Running;
            return;
        }
        let next = {
            let mut q = QUEUE.lock();
            let n = q.run.pop_front();
            if n.is_some() {
                QUEUED.fetch_sub(1, Ordering::Release);
            }
            n
        };
        if next.is_none() && stays {
            return;
        }
        let prev = c.current.take();
        if let Some(p) = &prev {
            if stays {
                // Preempted: queued by finish_switch, once we are off it.
                let mut st = p.state.lock();
                st.run = Run::Ready;
                st.requeue = true;
            }
        }
        if let Some(n) = &next {
            n.set_run(Run::Running);
        }
        IDLE_NOW[cpu].store(next.is_none(), Ordering::Release);
        c.current = next.clone();
        let from = match &prev {
            Some(p) => p.arch.context(),
            None => unsafe { &raw mut IDLE[cpu] },
        };
        let to = match &next {
            Some(n) => n.arch.context() as *const Context,
            None => unsafe { &raw const IDLE[cpu] },
        };
        let prev_fp = prev.as_ref().map(|p| p.arch.fp());
        c.prev = prev;
        (from, to, next, prev_fp)
    };
    // User FP registers: the kernel never uses them, so they still hold the
    // previous thread's; swap them with the next thread's.
    if let Some(f) = prev_fp {
        unsafe { arch::fp_save(f) };
    }
    if let Some(n) = &next {
        {
            let _st = n.state.lock();
            debug_assert!(!n.on_cpu.load(Ordering::Relaxed));
            n.on_cpu.store(true, Ordering::Release);
        }
        n.cpu.store(cpu, Ordering::Relaxed);
        n.task.running.fetch_add(1, Ordering::Relaxed);
        arch::set_thread_cpu(&n.arch, cpu);
        arch::activate(&n.task.aspace);
        unsafe { arch::fp_restore(n.arch.fp()) };
    }
    drop(next);
    unsafe { arch::switch(from, to) };
    finish_switch();
}

/// On the new thread's stack: the previous thread can go (to the run
/// queue if it was woken or preempted meanwhile, or away if it died).
#[unsafe(no_mangle)]
pub extern "C" fn finish_switch() {
    let prev = CPUS[arch::cpu_id()].lock().prev.take();
    let Some(p) = prev else { return };
    p.task.running.fetch_sub(1, Ordering::Relaxed);
    let (queue, dead) = {
        let mut st = p.state.lock();
        p.on_cpu.store(false, Ordering::Release);
        let q = st.requeue && st.run == Run::Ready;
        st.requeue = false;
        (q, st.run == Run::Dead)
    };
    if queue {
        enqueue(p.clone());
    }
    if dead {
        crate::task::try_release(&p.task);
    }
}
