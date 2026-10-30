//! Kernel heap: the `talc` allocator behind a lock that masks interrupts.
//! Natively it is a static arena; on wasm32 it grows with `memory.grow`
//! (memory objects are heap pages there, so they need the room).
#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use talc::TalcLock;
    use talc::source::Claim;

    use crate::sync::RawIrqLock;

    const HEAP_SIZE: usize = 8 << 20;

    static mut ARENA: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

    #[global_allocator]
    static HEAP: TalcLock<RawIrqLock, Claim> = TalcLock::new(unsafe { Claim::array(&raw mut ARENA) });
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use core::alloc::Layout;

    use talc::TalcLock;
    use talc::base::Talc;
    use talc::base::binning::Binning;
    use talc::source::Source;

    use crate::sync::RawIrqLock;

    const CHUNK: usize = 1 << 20;

    #[derive(Debug)]
    pub struct Grow;

    unsafe impl Source for Grow {
        fn acquire<B: Binning>(talc: &mut Talc<Self, B>, layout: Layout) -> Result<(), ()> {
            let want = (layout.size() + layout.align() + 4096).max(CHUNK);
            let pages = want.div_ceil(65536);
            let old = core::arch::wasm32::memory_grow(0, pages);
            if old == usize::MAX {
                return Err(());
            }
            unsafe { talc.claim((old * 65536) as *mut u8, pages * 65536) }.map(|_| ()).ok_or(())
        }
    }

    #[global_allocator]
    static HEAP: TalcLock<RawIrqLock, Grow> = TalcLock::new(Grow);
}

/// The heap is claimed on first use.
pub fn init() {}
