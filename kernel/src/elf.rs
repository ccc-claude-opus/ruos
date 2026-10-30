//! ELF64 program loader (native).
use abi::Error;

use crate::arch;
use crate::mm::aspace::AddrSpace;
use crate::mm::paging::Flags;
use crate::mm::PAGE_SIZE;

const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

fn u16_at(b: &[u8], o: usize) -> Result<u16, Error> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or(Error::InvalidArgs)
}

fn u32_at(b: &[u8], o: usize) -> Result<u32, Error> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or(Error::InvalidArgs)
}

fn u64_at(b: &[u8], o: usize) -> Result<u64, Error> {
    b.get(o..o + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap())).ok_or(Error::InvalidArgs)
}

/// Maps and fills every PT_LOAD segment; returns the entry point.
pub fn load(a: &AddrSpace, b: &[u8]) -> Result<u64, Error> {
    if b.get(..4) != Some(b"\x7fELF") || b.get(4) != Some(&2) || b.get(5) != Some(&1) {
        return Err(Error::InvalidArgs);
    }
    if u16_at(b, 18)? != arch::ELF_MACHINE {
        return Err(Error::NotSupported);
    }
    let entry = u64_at(b, 24)?;
    let phoff = u64_at(b, 32)? as usize;
    let phentsize = u16_at(b, 54)? as usize;
    let phnum = u16_at(b, 56)? as usize;
    for i in 0..phnum {
        let p = phoff + i * phentsize;
        if u32_at(b, p)? != PT_LOAD {
            continue;
        }
        let flags = u32_at(b, p + 4)?;
        let off = u64_at(b, p + 8)? as usize;
        let va = u64_at(b, p + 16)?;
        let filesz = u64_at(b, p + 32)? as usize;
        let memsz = u64_at(b, p + 40)?;
        let end = va.checked_add(memsz).ok_or(Error::InvalidArgs)?;
        if !arch::user_addr_ok(va) || !arch::user_addr_ok(end.max(1) - 1) || filesz as u64 > memsz {
            return Err(Error::InvalidArgs);
        }
        let mut f = Flags::USER;
        if flags & PF_R != 0 {
            f = f | Flags::READ;
        }
        if flags & PF_W != 0 {
            f = f | Flags::WRITE;
        }
        if flags & PF_X != 0 {
            f = f | Flags::EXEC;
        }
        let start = va & !(PAGE_SIZE - 1);
        let stop = (end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        a.map_zeroed(start, stop - start, f)?;
        a.write_raw(va, b.get(off..off + filesz).ok_or(Error::InvalidArgs)?)?;
    }
    if !arch::user_addr_ok(entry) {
        return Err(Error::InvalidArgs);
    }
    Ok(entry)
}
