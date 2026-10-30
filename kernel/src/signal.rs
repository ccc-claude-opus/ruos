//! Object signals and their one-shot observers: a thread in `WAIT_ONE`
//! or a port binding (`PORT_BIND`). Signals are level triggered: an
//! observer fires as soon as any signal in its mask is set.
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use abi::Packet;

use crate::port::Port;
use crate::sched;
use crate::sync::IrqLock;
use crate::task::Thread;

pub enum Observer {
    /// Stores the observed signals at `out` and wakes the thread (if it is
    /// still in the same wait: `wgen`).
    Thread { t: Arc<Thread>, wgen: u64, mask: u32, out: u64 },
    Port { port: Weak<Port>, key: u64, mask: u32 },
    /// A thread in CHANNEL_RECV: woken with ShouldWait when a message (or
    /// the peer's end) arrives, and it reads again. (Reading for it here
    /// would run in whatever context set the signal, e.g. a handle being
    /// dropped under its task's handle table lock.)
    Recv { t: Arc<Thread>, wgen: u64 },
}

impl Observer {
    fn mask(&self) -> u32 {
        match self {
            Observer::Thread { mask, .. } | Observer::Port { mask, .. } => *mask,
            Observer::Recv { .. } => abi::signal::READABLE | abi::signal::PEER_CLOSED,
        }
    }

    fn stale(&self) -> bool {
        match self {
            Observer::Thread { t, wgen, .. } => !sched::waiting(t, *wgen),
            Observer::Port { port, .. } => port.strong_count() == 0,
            Observer::Recv { t, wgen, .. } => !sched::waiting(t, *wgen),
        }
    }

    fn fire(self, bits: u32) {
        match self {
            Observer::Thread { t, wgen, out, mask } => {
                if sched::waiting(&t, wgen) {
                    let r = t.task.aspace.write(out, &(bits & mask).to_le_bytes());
                    sched::wake_gen(&t, wgen, r.map(|_| 0).unwrap_or_else(|e| e.code()));
                }
            }
            Observer::Port { port, key, mask } => {
                if let Some(p) = port.upgrade() {
                    p.queue(Packet { key, signals: bits & mask, _pad: 0, data: 0 });
                }
            }
            Observer::Recv { t, wgen } => sched::wake_gen(&t, wgen, abi::Error::ShouldWait.code()),
        }
    }
}

pub struct SignalCell(IrqLock<(u32, Vec<Observer>)>);

impl SignalCell {
    pub const fn new(bits: u32) -> SignalCell {
        SignalCell(IrqLock::new((bits, Vec::new())))
    }

    pub fn get(&self) -> u32 {
        self.0.lock().0
    }

    pub fn update(&self, clear: u32, set: u32) {
        let (bits, fired) = {
            let mut s = self.0.lock();
            s.0 = (s.0 & !clear) | set;
            let bits = s.0;
            let (fired, keep): (Vec<Observer>, Vec<Observer>) =
                core::mem::take(&mut s.1).into_iter().partition(|o| o.mask() & bits != 0);
            s.1 = keep;
            (bits, fired)
        };
        for o in fired {
            o.fire(bits);
        }
    }

    /// Fires `o` now if its signals are set, else keeps it until they are.
    pub fn observe(&self, o: Observer) {
        let bits = {
            let mut s = self.0.lock();
            s.1.retain(|x| !x.stale());
            if s.0 & o.mask() == 0 {
                s.1.push(o);
                return;
            }
            s.0
        };
        o.fire(bits);
    }
}
