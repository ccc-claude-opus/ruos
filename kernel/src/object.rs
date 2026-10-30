//! Kernel objects and handle tables.
use alloc::sync::Arc;
use alloc::vec::Vec;

use abi::{Error, ObjType, Rights};

use crate::channel::Channel;
use crate::memory::Memory;
use crate::port::Port;
use crate::quota::Quota;
use crate::serial::Serial;
use crate::signal::SignalCell;
use crate::task::{Task, Thread};

#[derive(Clone)]
pub enum Object {
    Task(Arc<Task>),
    Thread(Arc<Thread>),
    Channel(Arc<Channel>),
    Port(Arc<Port>),
    Memory(Arc<Memory>),
    Serial(Arc<Serial>),
    Quota(Arc<Quota>),
    #[cfg(not(target_arch = "wasm32"))]
    Mmio(Arc<crate::device::Mmio>),
    #[cfg(not(target_arch = "wasm32"))]
    Irq(Arc<crate::device::Irq>),
}

impl Object {
    pub fn kind(&self) -> ObjType {
        match self {
            Object::Task(_) => ObjType::Task,
            Object::Thread(_) => ObjType::Thread,
            Object::Channel(_) => ObjType::Channel,
            Object::Port(_) => ObjType::Port,
            Object::Memory(_) => ObjType::Memory,
            Object::Serial(_) => ObjType::Serial,
            Object::Quota(_) => ObjType::Quota,
            #[cfg(not(target_arch = "wasm32"))]
            Object::Mmio(_) => ObjType::Mmio,
            #[cfg(not(target_arch = "wasm32"))]
            Object::Irq(_) => ObjType::Irq,
        }
    }

    /// The object's signals, if it has any.
    pub fn signals(&self) -> Option<&SignalCell> {
        match self {
            Object::Task(t) => Some(&t.sig),
            Object::Thread(t) => Some(&t.sig),
            Object::Channel(c) => Some(&c.sig),
            Object::Serial(s) => Some(&s.sig),
            #[cfg(not(target_arch = "wasm32"))]
            Object::Irq(i) => Some(&i.sig),
            _ => None,
        }
    }

    pub fn same(&self, o: &Object) -> bool {
        match (self, o) {
            (Object::Channel(a), Object::Channel(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

#[derive(Clone)]
pub struct Handle {
    pub obj: Object,
    pub rights: Rights,
}

/// A task's handles; the handle value is the slot index.
pub struct HandleTable {
    slots: Vec<Option<Handle>>,
}

/// Upper bound on handles per task (until quotas, M5).
const MAX_HANDLES: usize = 1024;

impl HandleTable {
    pub const fn new() -> Self {
        HandleTable { slots: Vec::new() }
    }

    pub fn add(&mut self, h: Handle) -> Result<u32, Error> {
        if let Some(i) = self.slots.iter().position(|s| s.is_none()) {
            self.slots[i] = Some(h);
            return Ok(i as u32);
        }
        if self.slots.len() >= MAX_HANDLES {
            return Err(Error::NoMemory);
        }
        self.slots.push(Some(h));
        Ok(self.slots.len() as u32 - 1)
    }

    /// (handle value, handle) of every slot in use.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn iter(&self) -> impl Iterator<Item = (u32, &Handle)> {
        self.slots.iter().enumerate().filter_map(|(i, s)| Some((i as u32, s.as_ref()?)))
    }

    pub fn get(&self, h: u64) -> Result<&Handle, Error> {
        self.slots.get(h as usize).and_then(|s| s.as_ref()).ok_or(Error::BadHandle)
    }

    /// The handle, if it has `rights`.
    pub fn get_with(&self, h: u64, rights: Rights) -> Result<&Handle, Error> {
        let x = self.get(h)?;
        if x.rights.has(rights) { Ok(x) } else { Err(Error::AccessDenied) }
    }

    pub fn remove(&mut self, h: u64) -> Result<Handle, Error> {
        self.slots.get_mut(h as usize).and_then(|s| s.take()).ok_or(Error::BadHandle)
    }

    /// Empties the table; the caller drops the handles outside the lock.
    pub fn take_all(&mut self) -> Vec<Handle> {
        core::mem::take(&mut self.slots).into_iter().flatten().collect()
    }
}
