//! The program heap: the `talc` allocator. When it runs out it claims a
//! fresh span: a mapped memory object natively, `memory.grow` on wasm32.
use core::alloc::Layout;

use talc::TalcLock;
use talc::base::Talc;
use talc::base::binning::Binning;
use talc::source::Source;

const CHUNK: usize = 256 * 1024;

fn round(n: usize, a: usize) -> usize {
    (n + a - 1) & !(a - 1)
}

/// New memory for the heap: (start, size). Allocates nothing itself.
#[cfg(not(target_arch = "wasm32"))]
fn grow(min: usize) -> Option<(usize, usize)> {
    let size = round(min.max(CHUNK), 4096);
    let m = crate::Memory::create(size as u64).ok()?;
    let p = m.map(0, size as u64, crate::map::READ | crate::map::WRITE).ok()?;
    let _ = m.0.close();
    Some((p as usize, size))
}

#[cfg(target_arch = "wasm32")]
fn grow(min: usize) -> Option<(usize, usize)> {
    let pages = round(min.max(CHUNK), 65536) / 65536;
    let old = core::arch::wasm32::memory_grow(0, pages);
    (old != usize::MAX).then(|| (old * 65536, pages * 65536))
}

#[derive(Debug)]
pub struct Grow;

unsafe impl Source for Grow {
    fn acquire<B: Binning>(talc: &mut Talc<Self, B>, layout: Layout) -> Result<(), ()> {
        let (base, size) = grow(layout.size() + layout.align() + 4096).ok_or(())?;
        unsafe { talc.claim(base as *mut u8, size) }.map(|_| ()).ok_or(())
    }
}

#[global_allocator]
static HEAP: TalcLock<spinning_top::RawSpinlock, Grow> = TalcLock::new(Grow);
