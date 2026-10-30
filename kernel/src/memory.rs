//! Memory objects: page lists that tasks share by handle, read and write
//! with system calls, map (native) and clone copy-on-write. Pages are
//! reference counted; a page shared by clones is copied before a write.
use alloc::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use alloc::sync::Weak;
use alloc::vec::Vec;

use abi::Error;

use crate::sync::IrqLock;

pub const PAGE: usize = 4096;

/// Largest memory object.
const MAX_SIZE: u64 = if cfg!(target_arch = "wasm32") { 16 << 20 } else { 256 << 20 };

/// One zero-initialized page: a frame (native) or a heap block (wasm32).
pub struct Page {
    ptr: *mut u8,
    #[cfg(not(target_arch = "wasm32"))]
    pub pa: u64,
}

unsafe impl Send for Page {}
unsafe impl Sync for Page {}

impl Page {
    #[cfg(not(target_arch = "wasm32"))]
    fn new() -> Option<Page> {
        let pa = crate::mm::frame::alloc()?;
        Some(Page { ptr: crate::arch::phys_to_virt(pa) as *mut u8, pa })
    }

    #[cfg(target_arch = "wasm32")]
    fn new() -> Option<Page> {
        let l = core::alloc::Layout::from_size_align(PAGE, PAGE).unwrap();
        let ptr = unsafe { alloc::alloc::alloc_zeroed(l) };
        (!ptr.is_null()).then_some(Page { ptr })
    }

    fn bytes(&self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, PAGE) }
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        crate::mm::frame::free(self.pa);
        #[cfg(target_arch = "wasm32")]
        unsafe {
            alloc::alloc::dealloc(self.ptr, core::alloc::Layout::from_size_align(PAGE, PAGE).unwrap())
        };
    }
}

/// Where a memory object is mapped (native).
#[cfg(not(target_arch = "wasm32"))]
pub struct MapRef {
    pub aspace: Weak<crate::arch::AddrSpace>,
    pub va: u64,
    pub first: usize,
    pub n: usize,
    pub writable: bool,
}

pub struct Memory {
    size: u64,
    _charge: Option<crate::quota::Charge>,
    pages: IrqLock<Vec<Arc<Page>>>,
    #[cfg(not(target_arch = "wasm32"))]
    maps: IrqLock<Vec<MapRef>>,
}

impl Memory {
    fn with_pages(size: u64, pages: Vec<Arc<Page>>, charge: Option<crate::quota::Charge>) -> Arc<Memory> {
        Arc::new(Memory {
            size,
            _charge: charge,
            pages: IrqLock::new(pages),
            #[cfg(not(target_arch = "wasm32"))]
            maps: IrqLock::new(Vec::new()),
        })
    }

    /// A zeroed object, charged to `quota` (if any) for its lifetime.
    pub fn create(size: u64, quota: Option<&Arc<crate::quota::Quota>>) -> Result<Arc<Memory>, Error> {
        if size == 0 || size > MAX_SIZE {
            return Err(Error::InvalidArgs);
        }
        let n = size.div_ceil(PAGE as u64) as usize;
        let charge = match quota {
            Some(q) => Some(crate::quota::Charge::new(q, crate::quota::Res::Memory, (n * PAGE) as u64)?),
            None => None,
        };
        let mut pages = Vec::with_capacity(n);
        for _ in 0..n {
            pages.push(Arc::new(Page::new().ok_or(Error::NoMemory)?));
        }
        Ok(Memory::with_pages(n as u64 * PAGE as u64, pages, charge))
    }

    /// Physically contiguous pages (for DMA), charged to `quota`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn create_dma(size: u64, quota: &Arc<crate::quota::Quota>) -> Result<Arc<Memory>, Error> {
        if size == 0 || size > (4 << 20) {
            return Err(Error::InvalidArgs);
        }
        let n = size.div_ceil(PAGE as u64);
        let charge = crate::quota::Charge::new(quota, crate::quota::Res::Memory, n * PAGE as u64)?;
        let base = crate::mm::frame::alloc_contiguous(n).ok_or(Error::NoMemory)?;
        let pages = (0..n)
            .map(|i| {
                let pa = base + i * PAGE as u64;
                Arc::new(Page { ptr: crate::arch::phys_to_virt(pa) as *mut u8, pa })
            })
            .collect();
        Ok(Memory::with_pages(n * PAGE as u64, pages, Some(charge)))
    }

    /// Physical address of byte `off`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn phys(&self, off: u64) -> Result<u64, Error> {
        self.range(off, 1)?;
        Ok(self.pages.lock()[off as usize / PAGE].pa + off % PAGE as u64)
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    fn range(&self, off: u64, len: usize) -> Result<(), Error> {
        match off.checked_add(len as u64) {
            Some(end) if end <= self.size => Ok(()),
            _ => Err(Error::InvalidArgs),
        }
    }

    pub fn read(&self, off: u64, buf: &mut [u8]) -> Result<(), Error> {
        self.range(off, buf.len())?;
        let pages = self.pages.lock();
        let mut done = 0;
        while done < buf.len() {
            let a = off as usize + done;
            let n = (PAGE - a % PAGE).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&pages[a / PAGE].bytes()[a % PAGE..a % PAGE + n]);
            done += n;
        }
        Ok(())
    }

    pub fn write(&self, off: u64, buf: &[u8]) -> Result<(), Error> {
        self.range(off, buf.len())?;
        let mut done = 0;
        while done < buf.len() {
            let a = off as usize + done;
            let n = (PAGE - a % PAGE).min(buf.len() - done);
            self.make_private(a / PAGE)?;
            let pages = self.pages.lock();
            pages[a / PAGE].bytes()[a % PAGE..a % PAGE + n].copy_from_slice(&buf[done..done + n]);
            done += n;
        }
        Ok(())
    }

    /// A copy-on-write clone: both share the pages until one writes. It
    /// is charged in full to `quota` (the copies it may need).
    pub fn clone_cow(&self, quota: &Arc<crate::quota::Quota>) -> Result<Arc<Memory>, Error> {
        let charge = crate::quota::Charge::new(quota, crate::quota::Res::Memory, self.size)?;
        let pages = self.pages.lock().clone();
        let c = Memory::with_pages(self.size, pages, Some(charge));
        // Writable mappings of the original must now fault on write.
        #[cfg(not(target_arch = "wasm32"))]
        self.remap_all();
        Ok(c)
    }

    /// Gives page `i` its own frame if clones share it.
    pub fn make_private(&self, i: usize) -> Result<(), Error> {
        {
            let mut pages = self.pages.lock();
            let p = pages.get(i).ok_or(Error::InvalidArgs)?;
            if Arc::strong_count(p) > 1 {
                let n = Page::new().ok_or(Error::NoMemory)?;
                n.bytes().copy_from_slice(p.bytes());
                pages[i] = Arc::new(n);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.remap(i);
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Memory {
    pub fn pages(&self) -> usize {
        self.pages.lock().len()
    }

    /// Physical address of page `i` and whether only this object holds it.
    pub fn page(&self, i: usize) -> (u64, bool) {
        let pages = self.pages.lock();
        (pages[i].pa, Arc::strong_count(&pages[i]) == 1)
    }

    pub fn add_map(&self, m: MapRef) {
        self.maps.lock().push(m);
    }

    pub fn remove_map(&self, aspace: *const crate::arch::AddrSpace, va: u64) {
        self.maps.lock().retain(|m| !(m.aspace.as_ptr() == aspace && m.va == va));
    }

    /// Rewrites every mapping's entry for page `i`.
    fn remap(&self, i: usize) {
        let (pa, unique) = self.page(i);
        let maps: Vec<_> = self
            .maps
            .lock()
            .iter()
            .filter(|m| i >= m.first && i < m.first + m.n)
            .map(|m| (m.aspace.clone(), m.va + ((i - m.first) * PAGE) as u64, m.writable))
            .collect();
        for (a, va, w) in maps {
            if let Some(a) = a.upgrade() {
                a.set_pte(va, pa, w && unique);
            }
        }
    }

    fn remap_all(&self) {
        for i in 0..self.pages() {
            self.remap(i);
        }
    }
}
