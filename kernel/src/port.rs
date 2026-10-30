//! Ports: queues of packets (bound signals or user packets) that threads
//! wait on.
use alloc::collections::VecDeque;
use alloc::sync::Arc;

use abi::{Error, Packet};

use crate::sched;
use crate::sync::IrqLock;
use crate::task::Thread;

const MAX_PACKETS: usize = 4096;

pub struct Port {
    s: IrqLock<PortState>,
}

struct PortState {
    packets: VecDeque<Packet>,
    waiters: VecDeque<(Arc<Thread>, u64, u64)>,
}

fn deliver(t: &Arc<Thread>, wgen: u64, out: u64, p: &Packet) {
    let bytes: [u8; 24] = unsafe { core::mem::transmute(*p) };
    let r = t.task.aspace.write(out, &bytes);
    sched::wake_gen(t, wgen, r.map(|_| 0).unwrap_or_else(|e| e.code()));
}

impl Port {
    pub fn new() -> Arc<Port> {
        Arc::new(Port { s: IrqLock::new(PortState { packets: VecDeque::new(), waiters: VecDeque::new() }) })
    }

    pub fn queue(&self, p: Packet) {
        let waiter = {
            let mut s = self.s.lock();
            loop {
                match s.waiters.pop_front() {
                    Some((t, wgen, out)) if sched::waiting(&t, wgen) => break Some((t, wgen, out)),
                    Some(_) => continue,
                    None => {
                        if s.packets.len() < MAX_PACKETS {
                            s.packets.push_back(p);
                        }
                        break None;
                    }
                }
            }
        };
        if let Some((t, wgen, out)) = waiter {
            deliver(&t, wgen, out, &p);
        }
    }

    /// A packet now, or the thread blocks (until `deadline_ns`).
    pub fn wait(&self, t: &Arc<Thread>, out: u64, timeout_ns: u64) -> Option<i64> {
        let mut s = self.s.lock();
        if let Some(p) = s.packets.pop_front() {
            drop(s);
            let bytes: [u8; 24] = unsafe { core::mem::transmute(p) };
            return Some(t.task.aspace.write(out, &bytes).map(|_| 0).unwrap_or_else(|e| e.code()));
        }
        if timeout_ns == 0 {
            return Some(Error::TimedOut.code());
        }
        let wgen = sched::block_current(t);
        s.waiters.push_back((t.clone(), wgen, out));
        drop(s);
        sched::timeout(t, wgen, timeout_ns, Error::TimedOut.code());
        None
    }
}
