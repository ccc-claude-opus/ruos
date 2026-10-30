//! Sv39 page table entries.
use crate::mm::paging::Flags;

pub const LEVELS: usize = 3;
/// Root entries of the user half (the rest are shared kernel mappings).
pub const USER_ROOT_ENTRIES: usize = 256;

const V: u64 = 1;
const R: u64 = 2;
const W: u64 = 4;
const X: u64 = 8;
const U: u64 = 0x10;
const G: u64 = 0x20;
const A: u64 = 0x40;
const D: u64 = 0x80;

/// Every level can hold a leaf (1 GB, 2 MB, 4 KB).
pub fn block_ok(_level: usize) -> bool {
    true
}

pub fn table(pa: u64) -> u64 {
    (pa >> 12) << 10 | V
}

pub fn leaf(pa: u64, f: Flags, _level: usize) -> u64 {
    let mut e = (pa >> 12) << 10 | V | A | D;
    if f.has(Flags::READ) {
        e |= R;
    }
    if f.has(Flags::WRITE) {
        e |= W | R;
    }
    if f.has(Flags::EXEC) {
        e |= X;
    }
    if f.has(Flags::USER) {
        e |= U;
    }
    if f.has(Flags::GLOBAL) {
        e |= G;
    }
    e
}

pub fn is_valid(e: u64) -> bool {
    e & V != 0
}

pub fn is_table(e: u64, _level: usize) -> bool {
    e & V != 0 && e & (R | W | X) == 0
}

pub fn addr(e: u64) -> u64 {
    (e >> 10 & ((1 << 44) - 1)) << 12
}

/// A user page, writable if `write`.
pub fn user_ok(e: u64, write: bool) -> bool {
    e & U != 0 && e & R != 0 && (!write || e & W != 0)
}
