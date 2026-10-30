//! Multi-level page tables shared by Sv39 and AArch64 4 KB granule; the
//! descriptor formats come from `arch::pte`.
use crate::arch::{phys_to_virt, pte};

use super::frame;

/// Mapping attributes, encoded by `arch::pte::leaf`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Flags(pub u32);

impl Flags {
    pub const READ: Flags = Flags(1);
    pub const WRITE: Flags = Flags(2);
    pub const EXEC: Flags = Flags(4);
    pub const USER: Flags = Flags(8);
    pub const DEVICE: Flags = Flags(16);
    pub const GLOBAL: Flags = Flags(32);

    pub fn has(self, f: Flags) -> bool {
        self.0 & f.0 == f.0
    }
}

impl core::ops::BitOr for Flags {
    type Output = Flags;
    fn bitor(self, o: Flags) -> Flags {
        Flags(self.0 | o.0)
    }
}

pub struct PageTable {
    pub root: u64,
    pub tables: usize,
}

fn shift(level: usize) -> u32 {
    12 + 9 * (pte::LEVELS - 1 - level) as u32
}

fn entries(pa: u64) -> &'static mut [u64; 512] {
    unsafe { &mut *(phys_to_virt(pa) as *mut [u64; 512]) }
}

impl PageTable {
    pub fn new() -> PageTable {
        PageTable::try_new().expect("page table")
    }

    pub fn try_new() -> Option<PageTable> {
        Some(PageTable { root: frame::alloc()?, tables: 1 })
    }

    /// Maps `[va, va + size)` to `[pa, pa + size)` with the largest blocks
    /// the alignment allows.
    pub fn map(&mut self, va: u64, pa: u64, size: u64, flags: Flags) {
        self.try_map(va, pa, size, flags).expect("page table");
    }

    pub fn try_map(&mut self, mut va: u64, mut pa: u64, size: u64, flags: Flags) -> Option<()> {
        let end = va + size;
        while va < end {
            let mut level = pte::LEVELS - 1;
            for l in 0..pte::LEVELS {
                let s = 1u64 << shift(l);
                if pte::block_ok(l) && va % s == 0 && pa % s == 0 && end - va >= s {
                    level = l;
                    break;
                }
            }
            self.set(va, pte::leaf(pa, flags, level), level)?;
            let s = 1u64 << shift(level);
            va += s;
            pa += s;
        }
        Some(())
    }

    fn set(&mut self, va: u64, entry: u64, level: usize) -> Option<()> {
        let mut t = self.root;
        for l in 0..level {
            let i = ((va >> shift(l)) & 511) as usize;
            let e = entries(t)[i];
            t = if pte::is_table(e, l) {
                pte::addr(e)
            } else {
                assert!(!pte::is_valid(e), "remapping a block at {:#x}", va);
                let n = frame::alloc()?;
                self.tables += 1;
                entries(t)[i] = pte::table(n);
                n
            };
        }
        let i = ((va >> shift(level)) & 511) as usize;
        entries(t)[i] = entry;
        Some(())
    }

    /// Removes the 4 KB mapping at `va` (the frame is not freed).
    pub fn clear(&mut self, va: u64) {
        let mut t = self.root;
        for l in 0..pte::LEVELS {
            let i = ((va >> shift(l)) & 511) as usize;
            let e = entries(t)[i];
            if !pte::is_valid(e) {
                return;
            }
            if !pte::is_table(e, l) {
                entries(t)[i] = 0;
                return;
            }
            t = pte::addr(e);
        }
    }

    /// The physical address and leaf entry for `va`.
    pub fn lookup(&self, va: u64) -> Option<(u64, u64)> {
        let mut t = self.root;
        for l in 0..pte::LEVELS {
            let e = entries(t)[((va >> shift(l)) & 511) as usize];
            if !pte::is_valid(e) {
                return None;
            }
            if !pte::is_table(e, l) {
                let m = (1u64 << shift(l)) - 1;
                return Some((pte::addr(e) & !m | (va & m), e));
            }
            t = pte::addr(e);
        }
        None
    }

    /// Frees the first `root_entries` subtrees of the root (their tables
    /// and 4 KB leaf frames) and clears those entries; the root stays.
    pub fn free_user(&self, root_entries: usize) {
        fn walk(t: u64, level: usize, n: usize) {
            for i in 0..n {
                let e = entries(t)[i];
                if !pte::is_valid(e) {
                    continue;
                }
                if pte::is_table(e, level) {
                    walk(pte::addr(e), level + 1, 512);
                    frame::free(pte::addr(e));
                } else if level == pte::LEVELS - 1 {
                    frame::free(pte::addr(e));
                }
            }
        }
        walk(self.root, 0, root_entries);
        entries(self.root)[..root_entries].fill(0);
    }

    /// Physical address `va` maps to.
    pub fn translate(&self, va: u64) -> Option<u64> {
        self.lookup(va).map(|(pa, _)| pa)
    }
}
