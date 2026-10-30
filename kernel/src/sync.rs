//! Locks. Single hart for now: [`IrqLock`] masks interrupts while held, so an
//! interrupt handler can never spin on a lock its own hart holds.
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch;

pub struct IrqLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for IrqLock<T> {}

pub struct IrqGuard<'a, T> {
    lock: &'a IrqLock<T>,
    irq: bool,
}

impl<T> IrqLock<T> {
    pub const fn new(value: T) -> Self {
        IrqLock { locked: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }

    pub fn lock(&self) -> IrqGuard<'_, T> {
        let irq = arch::disable_interrupts();
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        IrqGuard { lock: self, irq }
    }

    /// Only for the panic path.
    pub unsafe fn force_unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

impl<T> Deref for IrqGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for IrqGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for IrqGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        if self.irq {
            arch::enable_interrupts();
        }
    }
}

/// The same lock as a `lock_api` mutex (for the `talc` allocator): the
/// interrupt state is kept in the lock while it is held.
pub struct RawIrqLock {
    locked: AtomicBool,
    irq: AtomicBool,
}

unsafe impl lock_api::RawMutex for RawIrqLock {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: RawIrqLock = RawIrqLock { locked: AtomicBool::new(false), irq: AtomicBool::new(false) };
    type GuardMarker = lock_api::GuardSend;

    fn lock(&self) {
        let irq = arch::disable_interrupts();
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        self.irq.store(irq, Ordering::Relaxed);
    }

    fn try_lock(&self) -> bool {
        let irq = arch::disable_interrupts();
        if self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            self.irq.store(irq, Ordering::Relaxed);
            true
        } else {
            if irq {
                arch::enable_interrupts();
            }
            false
        }
    }

    unsafe fn unlock(&self) {
        let irq = self.irq.load(Ordering::Relaxed);
        self.locked.store(false, Ordering::Release);
        if irq {
            arch::enable_interrupts();
        }
    }
}
