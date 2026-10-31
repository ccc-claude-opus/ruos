//! Memory objects: page lists that tasks share by handle, read and write
//! with system calls, map (native) and clone copy-on-write. Pages are
//! reference counted; a page shared by clones is copied before a write.
use alloc::sync::Arc;
use alloc::vec::Vec;

use abi::Error;

use crate::sync::IrqLock;

pub const PAGE: usize = 4096;

/// Largest memory object.
const MAX_SIZE: u64 = 16 << 20;

/// One zero-initialized page: a frame (native) or a heap block (wasm32).
pub struct Page {
    ptr: *mut u8,
}

unsafe impl Send for Page {}
unsafe impl Sync for Page {}

impl Page {

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
        unsafe {
            alloc::alloc::dealloc(self.ptr, core::alloc::Layout::from_size_align(PAGE, PAGE).unwrap())
        };
    }
}


pub struct Memory {
    size: u64,
    _charge: Option<crate::quota::Charge>,
    pages: IrqLock<Vec<Arc<Page>>>,
}

impl Memory {
    fn with_pages(size: u64, pages: Vec<Arc<Page>>, charge: Option<crate::quota::Charge>) -> Arc<Memory> {
        Arc::new(Memory {
            size,
            _charge: charge,
            pages: IrqLock::new(pages),
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
        Ok(())
    }
}

