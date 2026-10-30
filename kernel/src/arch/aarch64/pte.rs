//! AArch64 stage-1 descriptors, 4 KB granule, 48-bit VA (levels 0..3).
use crate::mm::paging::Flags;

pub const LEVELS: usize = 4;
/// User tables live in TTBR0: the whole root is theirs.
pub const USER_ROOT_ENTRIES: usize = 512;

const VALID: u64 = 1;
const TABLE: u64 = 3;
const PAGE: u64 = 3;
const BLOCK: u64 = 1;
/// MAIR index 0 = device nGnRnE, 1 = normal write-back.
const ATTR_NORMAL: u64 = 1 << 2;
const SH_INNER: u64 = 3 << 8;
const AP_RO: u64 = 1 << 7;
const AP_USER: u64 = 1 << 6;
const AF: u64 = 1 << 10;
const NG: u64 = 1 << 11;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
const ADDR: u64 = 0x0000_FFFF_FFFF_F000;

/// Level 0 has no blocks; 1 = 1 GB, 2 = 2 MB, 3 = 4 KB pages.
pub fn block_ok(level: usize) -> bool {
    level >= 1
}

pub fn table(pa: u64) -> u64 {
    pa | TABLE
}

pub fn leaf(pa: u64, f: Flags, level: usize) -> u64 {
    let mut e = pa | AF | if level == 3 { PAGE } else { BLOCK };
    if !f.has(Flags::DEVICE) {
        e |= ATTR_NORMAL | SH_INNER;
    }
    if !f.has(Flags::WRITE) {
        e |= AP_RO;
    }
    if f.has(Flags::USER) {
        e |= AP_USER | PXN;
        if !f.has(Flags::EXEC) {
            e |= UXN;
        }
    } else {
        e |= UXN;
        if !f.has(Flags::EXEC) {
            e |= PXN;
        }
    }
    if !f.has(Flags::GLOBAL) {
        e |= NG;
    }
    e
}

pub fn is_valid(e: u64) -> bool {
    e & VALID != 0
}

pub fn is_table(e: u64, level: usize) -> bool {
    level < 3 && e & 3 == TABLE
}

pub fn addr(e: u64) -> u64 {
    e & ADDR
}

/// A user page, writable if `write`.
pub fn user_ok(e: u64, write: bool) -> bool {
    e & AP_USER != 0 && (!write || e & AP_RO == 0)
}
