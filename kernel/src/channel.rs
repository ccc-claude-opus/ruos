//! Channels: a pair of endpoints; each message carries bytes and handles.
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use abi::{Error, signal};

use crate::object::Handle;
use crate::signal::SignalCell;
use crate::sync::IrqLock;

const MAX_QUEUE: usize = 1024;

pub struct Msg {
    pub data: Vec<u8>,
    pub handles: Vec<Handle>,
}

pub struct Channel {
    peer: IrqLock<Weak<Channel>>,
    /// The other end is the host (wasm32: ruos serve over a WebSocket):
    /// 0 = the server connection (the store's), 1 = ingress (kube's).
    remote: Option<u8>,
    queue: IrqLock<VecDeque<Msg>>,
    pub sig: SignalCell,
}

impl Channel {
    /// Messages waiting to be read here.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn queued(&self) -> usize {
        self.queue.lock().len()
    }

    pub fn pair() -> (Arc<Channel>, Arc<Channel>) {
        let new = || {
            Arc::new(Channel {
                peer: IrqLock::new(Weak::new()),
                remote: None,
                queue: IrqLock::new(VecDeque::new()),
                sig: SignalCell::new(signal::WRITABLE),
            })
        };
        let (a, b) = (new(), new());
        *a.peer.lock() = Arc::downgrade(&b);
        *b.peer.lock() = Arc::downgrade(&a);
        (a, b)
    }

    /// An endpoint whose peer is the host: its messages leave through
    /// `arch::remote_send`, the host's arrive through `deliver`.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn remote(i: u8) -> Arc<Channel> {
        Arc::new(Channel {
            peer: IrqLock::new(Weak::new()),
            remote: Some(i),
            queue: IrqLock::new(VecDeque::new()),
            sig: SignalCell::new(signal::WRITABLE),
        })
    }

    pub fn is_remote(&self) -> bool {
        self.remote.is_some()
    }

    /// A message from the host.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn deliver(&self, data: Vec<u8>) {
        self.queue.lock().push_back(Msg { data, handles: Vec::new() });
        self.sig.update(0, signal::READABLE);
    }

    pub fn peer(&self) -> Option<Arc<Channel>> {
        self.peer.lock().upgrade()
    }

    /// Queues `m` on the peer; gives it back if that is not possible.
    pub fn write(&self, m: Msg) -> Result<(), (Error, Msg)> {
        if let Some(i) = self.remote {
            // Only bytes can leave the machine.
            if !m.handles.is_empty() {
                return Err((Error::InvalidArgs, m));
            }
            return crate::arch::remote_send(i, &m.data).map_err(|e| (e, m));
        }
        let Some(p) = self.peer() else { return Err((Error::PeerClosed, m)) };
        {
            let mut q = p.queue.lock();
            if q.len() >= MAX_QUEUE {
                return Err((Error::ShouldWait, m));
            }
            q.push_back(m);
        }
        p.sig.update(0, signal::READABLE);
        Ok(())
    }

    /// Sizes of the next message.
    pub fn peek(&self) -> Result<(usize, usize), Error> {
        match self.queue.lock().front() {
            Some(m) => Ok((m.data.len(), m.handles.len())),
            None if self.peer().is_none() && self.remote.is_none() => Err(Error::PeerClosed),
            None => Err(Error::ShouldWait),
        }
    }

    pub fn pop(&self) -> Option<Msg> {
        let (m, empty) = {
            let mut q = self.queue.lock();
            let m = q.pop_front();
            (m, q.is_empty())
        };
        if empty {
            self.sig.update(signal::READABLE, 0);
        }
        m
    }

    /// Puts a message back at the front (the reader could not take its handles).
    pub fn unpop(&self, m: Msg) {
        self.queue.lock().push_front(m);
        self.sig.update(0, signal::READABLE);
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        if let Some(p) = self.peer.lock().upgrade() {
            p.sig.update(signal::WRITABLE, signal::PEER_CLOSED);
        }
    }
}
