//! A task's address space (native): a page table whose lower half holds
//! the task's pages; all of them are owned and freed with it.
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use abi::Error;

use crate::arch::{self, phys_to_virt, pte};
use crate::memory::{MapRef, Memory};
use crate::sync::IrqLock;

use super::paging::{Flags, PageTable};
use super::{PAGE_SIZE, frame};

/// Address space ids: a bitmap of those in use (bit 0, "no ASID", is never
/// handed out). An address space without one is flushed on every switch.
static ASIDS: IrqLock<(u32, Vec<u64>)> = IrqLock::new((0, Vec::new()));

/// The CPU supports ASIDs 1..=max.
pub fn init_asids(max: u32) {
    let mut a = ASIDS.lock();
    a.0 = max;
    a.1 = alloc::vec![0; (max as usize + 64) / 64];
    a.1[0] = 1;
}

fn alloc_asid() -> u16 {
    let found = {
        let mut a = ASIDS.lock();
        let max = a.0 as usize;
        let i = (1..=max).find(|&i| a.1[i / 64] & (1 << (i % 64)) == 0);
        if let Some(i) = i {
            a.1[i / 64] |= 1 << (i % 64);
        }
        i
    };
    match found {
        Some(i) => {
            // Entries a previous owner left in any CPU's TLB.
            arch::flush_asid(i as u16);
            i as u16
        }
        None => 0,
    }
}

fn free_asid(i: u16) {
    if i != 0 {
        let mut a = ASIDS.lock();
        a.1[i as usize / 64] &= !(1 << (i % 64));
    }
}

/// Where `MEMORY_MAP` places mappings.
const MAP_BASE: u64 = 0x10_0000_0000;
const MAP_END: u64 = 0x30_0000_0000;

/// A mapped memory object.
struct Vma {
    va: u64,
    n: usize,
    first: usize,
    writable: bool,
    mem: Arc<Memory>,
}

pub struct AddrSpace {
    pt: IrqLock<PageTable>,
    asid: u16,
    /// Page table pages charged to the quota so far.
    tables_charged: IrqLock<u64>,
    /// The task's own pages (program and stack) are charged here.
    quota: Arc<crate::quota::Quota>,
    charged: IrqLock<u64>,
    vmas: IrqLock<Vec<Vma>>,
    next_map: IrqLock<u64>,
    me: IrqLock<Weak<AddrSpace>>,
    /// Device registers mapped here (not frames: never freed as such).
    devices: IrqLock<Vec<(u64, usize)>>,
}

impl AddrSpace {
    pub fn new(quota: &Arc<crate::quota::Quota>) -> Result<AddrSpace, Error> {
        quota.charge(crate::quota::Res::Memory, PAGE_SIZE)?;
        let Some(mut pt) = PageTable::try_new() else {
            quota.refund(crate::quota::Res::Memory, PAGE_SIZE);
            return Err(Error::NoMemory);
        };
        arch::init_user_table(&mut pt);
        Ok(AddrSpace {
            pt: IrqLock::new(pt),
            asid: alloc_asid(),
            tables_charged: IrqLock::new(1),
            quota: quota.clone(),
            charged: IrqLock::new(0),
            vmas: IrqLock::new(Vec::new()),
            next_map: IrqLock::new(MAP_BASE),
            me: IrqLock::new(Weak::new()),
            devices: IrqLock::new(Vec::new()),
        })
    }

    /// The `Arc` this lives in (memory objects keep weak references).
    pub fn set_self(&self, me: Weak<AddrSpace>) {
        *self.me.lock() = me;
    }

    /// Points `va` at `pa` (a memory object's page).
    pub fn set_pte(&self, va: u64, pa: u64, writable: bool) {
        let mut f = Flags::USER | Flags::READ;
        if writable {
            f = f | Flags::WRITE;
        }
        {
            let mut pt = self.pt.lock();
            pt.try_map(va, pa, PAGE_SIZE, f);
            let _ = self.charge_tables(&pt);
        }
        arch::flush_tlb();
    }

    /// Maps pages `[off, off + len)` of `mem`; returns the address.
    pub fn map_memory(&self, mem: &Arc<Memory>, off: u64, len: u64, writable: bool) -> Result<u64, Error> {
        if off % PAGE_SIZE != 0 || len == 0 || off.checked_add(len).is_none_or(|e| e > mem.size()) {
            return Err(Error::InvalidArgs);
        }
        let n = len.div_ceil(PAGE_SIZE) as usize;
        let va = {
            let mut next = self.next_map.lock();
            let va = *next;
            if va + n as u64 * PAGE_SIZE > MAP_END {
                return Err(Error::NoMemory);
            }
            *next = va + (n as u64 + 1) * PAGE_SIZE; // a guard page between mappings
            va
        };
        let first = (off / PAGE_SIZE) as usize;
        // All the pages, then one TLB shootdown (not one per page).
        {
            let mut pt = self.pt.lock();
            for i in 0..n {
                let (pa, unique) = mem.page(first + i);
                let mut f = Flags::USER | Flags::READ;
                if writable && unique {
                    f = f | Flags::WRITE;
                }
                pt.try_map(va + i as u64 * PAGE_SIZE, pa, PAGE_SIZE, f).ok_or(Error::NoMemory)?;
            }
            self.charge_tables(&pt)?;
        }
        arch::flush_tlb();
        mem.add_map(MapRef { aspace: self.me.lock().clone(), va, first, n, writable });
        self.vmas.lock().push(Vma { va, n, first, writable, mem: mem.clone() });
        Ok(va)
    }

    fn reserve(&self, pages: usize) -> Result<u64, Error> {
        let mut next = self.next_map.lock();
        let va = *next;
        if va + pages as u64 * PAGE_SIZE > MAP_END {
            return Err(Error::NoMemory);
        }
        *next = va + (pages as u64 + 1) * PAGE_SIZE;
        Ok(va)
    }

    /// Maps device registers `[pa, pa + size)`; returns the address of `pa`.
    pub fn map_device(&self, pa: u64, size: u64) -> Result<u64, Error> {
        let start = pa & !(PAGE_SIZE - 1);
        let n = ((pa + size).div_ceil(PAGE_SIZE) * PAGE_SIZE - start) / PAGE_SIZE;
        let va = self.reserve(n as usize)?;
        let f = Flags::USER | Flags::READ | Flags::WRITE | Flags::DEVICE;
        {
            let mut pt = self.pt.lock();
            pt.try_map(va, start, n * PAGE_SIZE, f).ok_or(Error::NoMemory)?;
            self.charge_tables(&pt)?;
        }
        self.devices.lock().push((va, n as usize));
        Ok(va + (pa - start))
    }

    pub fn unmap_memory(&self, va: u64, len: u64) -> Result<(), Error> {
        let vma = {
            let mut v = self.vmas.lock();
            let i = v.iter().position(|m| m.va == va && len.div_ceil(PAGE_SIZE) as usize == m.n);
            v.remove(i.ok_or(Error::InvalidArgs)?)
        };
        self.drop_vma(&vma);
        Ok(())
    }

    fn drop_vma(&self, v: &Vma) {
        {
            let mut pt = self.pt.lock();
            for i in 0..v.n {
                pt.clear(v.va + i as u64 * PAGE_SIZE);
            }
        }
        arch::flush_tlb();
        v.mem.remove_map(self as *const AddrSpace, v.va);
    }

    /// A write fault on a mapped memory object: copy the page if clones
    /// share it, then make it writable. False if `va` is not such a page.
    pub fn fault(&self, va: u64, write: bool) -> bool {
        if !write {
            return false;
        }
        let hit = self.vmas.lock().iter().find_map(|v| {
            let inside = va >= v.va && va < v.va + v.n as u64 * PAGE_SIZE;
            (inside && v.writable).then(|| (v.mem.clone(), v.first + ((va - v.va) / PAGE_SIZE) as usize))
        });
        let Some((mem, page)) = hit else { return false };
        mem.make_private(page).is_ok() && self.is_writable(va)
    }

    fn is_writable(&self, va: u64) -> bool {
        self.pt.lock().lookup(va).is_some_and(|(_, e)| pte::user_ok(e, true))
    }

    pub fn root(&self) -> u64 {
        self.pt.lock().root
    }

    pub fn asid(&self) -> u16 {
        self.asid
    }

    /// Charges page table pages created since the last call.
    fn charge_tables(&self, pt: &PageTable) -> Result<(), Error> {
        let mut c = self.tables_charged.lock();
        let n = pt.tables as u64;
        if n > *c {
            self.quota.charge(crate::quota::Res::Memory, (n - *c) * PAGE_SIZE)?;
            *c = n;
        }
        Ok(())
    }

    /// Maps fresh zeroed pages over `[va, va + size)` (page aligned).
    pub fn map_zeroed(&self, va: u64, size: u64, flags: Flags) -> Result<(), Error> {
        let mut pt = self.pt.lock();
        let mut a = va;
        while a < va + size {
            if pt.lookup(a).is_some() {
                return Err(Error::InvalidArgs);
            }
            self.quota.charge(crate::quota::Res::Memory, PAGE_SIZE)?;
            *self.charged.lock() += PAGE_SIZE;
            let f = frame::alloc().ok_or(Error::NoMemory)?;
            pt.try_map(a, f, PAGE_SIZE, flags).ok_or(Error::NoMemory)?;
            self.charge_tables(&pt)?;
            a += PAGE_SIZE;
        }
        Ok(())
    }

    /// Copies between kernel memory and user pages, page by page; `check`
    /// demands user access (with write permission when writing).
    fn access(&self, va: u64, len: usize, write: bool, check: bool, mut f: impl FnMut(*mut u8, usize, usize)) -> Result<(), Error> {
        let mut done = 0usize;
        while done < len {
            let a = va.checked_add(done as u64).ok_or(Error::Fault)?;
            if !arch::user_addr_ok(a) {
                return Err(Error::Fault);
            }
            let mut found = self.pt.lock().lookup(a);
            // A copy-on-write page the kernel writes to (e.g. a result).
            if check && write && found.is_some_and(|(_, e)| !pte::user_ok(e, true)) && self.fault(a, true) {
                found = self.pt.lock().lookup(a);
            }
            let (pa, e) = found.ok_or(Error::Fault)?;
            if check && !pte::user_ok(e, write) {
                return Err(Error::Fault);
            }
            let n = ((PAGE_SIZE - (a & (PAGE_SIZE - 1))) as usize).min(len - done);
            f(phys_to_virt(pa) as *mut u8, done, n);
            done += n;
        }
        Ok(())
    }

    pub fn read(&self, va: u64, buf: &mut [u8]) -> Result<(), Error> {
        let p = buf.as_mut_ptr();
        self.access(va, buf.len(), false, true, |k, off, n| unsafe { core::ptr::copy_nonoverlapping(k, p.add(off), n) })
    }

    pub fn write(&self, va: u64, buf: &[u8]) -> Result<(), Error> {
        let p = buf.as_ptr();
        self.access(va, buf.len(), true, true, |k, off, n| unsafe { core::ptr::copy_nonoverlapping(p.add(off), k, n) })
    }

    /// Writes regardless of page permissions (program loading).
    pub fn write_raw(&self, va: u64, buf: &[u8]) -> Result<(), Error> {
        let p = buf.as_ptr();
        self.access(va, buf.len(), true, false, |k, off, n| unsafe { core::ptr::copy_nonoverlapping(p.add(off), k, n) })
    }

    /// Loads an ELF program and maps its stack.
    pub fn load(&self, image: &[u8]) -> Result<crate::task::Start, Error> {
        let entry = crate::elf::load(self, image)?;
        let top = abi::layout::STACK_TOP;
        let size = abi::layout::STACK_SIZE;
        self.map_zeroed(top - size, size, Flags::USER | Flags::READ | Flags::WRITE)?;
        // The code was written through the data cache.
        arch::sync_icache();
        Ok(crate::task::Start { entry, stack: top, arg: 0 })
    }
}

impl AddrSpace {
    /// The task has ended: frees its pages and refunds them now, though
    /// the root table may still be active until the next switch (and the
    /// `Task` may live on in handles).
    pub fn release(&self) {
        // Mapped pages belong to their memory objects: unmap before freeing.
        let vmas = core::mem::take(&mut *self.vmas.lock());
        for v in &vmas {
            self.drop_vma(v);
        }
        for (va, n) in core::mem::take(&mut *self.devices.lock()) {
            let mut pt = self.pt.lock();
            for i in 0..n {
                pt.clear(va + i as u64 * PAGE_SIZE);
            }
        }
        arch::flush_tlb();
        self.pt.lock().free_user(pte::USER_ROOT_ENTRIES);
        let mut c = self.charged.lock();
        self.quota.refund(crate::quota::Res::Memory, *c);
        *c = 0;
        let mut t = self.tables_charged.lock();
        self.quota.refund(crate::quota::Res::Memory, *t * PAGE_SIZE);
        *t = 0;
    }
}

impl Drop for AddrSpace {
    fn drop(&mut self) {
        self.release();
        frame::free(self.pt.lock().root);
        free_asid(self.asid);
    }
}
