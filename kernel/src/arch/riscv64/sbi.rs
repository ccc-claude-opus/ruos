//! SBI calls into OpenSBI.
use core::arch::asm;

const EXT_TIME: usize = 0x5449_4D45;
const EXT_SRST: usize = 0x5352_5354;
const EXT_HSM: usize = 0x48_534D;
const EXT_IPI: usize = 0x73_5049;
const EXT_RFENCE: usize = 0x5246_4E43;

fn call(ext: usize, fid: usize, a0: usize, a1: usize) -> (isize, usize) {
    let (err, val);
    unsafe {
        asm!("ecall", inlateout("a0") a0 => err, inlateout("a1") a1 => val,
             in("a6") fid, in("a7") ext);
    }
    (err, val)
}

fn call4(ext: usize, fid: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> isize {
    let err;
    unsafe {
        asm!("ecall", inlateout("a0") a0 => err, inlateout("a1") a1 => _, in("a2") a2, in("a3") a3,
             in("a6") fid, in("a7") ext);
    }
    err
}

/// Starts `hart` at physical `start` with a1 = `opaque`.
pub fn hart_start(hart: usize, start: usize, opaque: usize) -> isize {
    call4(EXT_HSM, 0, hart, start, opaque, 0)
}

pub fn send_ipi(hart: usize) {
    call(EXT_IPI, 0, 1, hart);
}

pub fn remote_sfence_all() {
    call4(EXT_RFENCE, 1, 0, usize::MAX, 0, usize::MAX);
}

pub fn remote_fence_i() {
    call4(EXT_RFENCE, 0, 0, usize::MAX, 0, 0);
}

/// sfence.vma for one ASID on every hart.
pub fn remote_sfence_asid(asid: usize) {
    let r: isize;
    unsafe {
        asm!("ecall", inlateout("a0") 0usize => r, inlateout("a1") usize::MAX => _, in("a2") 0usize, in("a3") usize::MAX,
             in("a4") asid, in("a6") 2usize, in("a7") EXT_RFENCE);
    }
    let _ = r;
}

pub fn set_timer(t: u64) {
    call(EXT_TIME, 0, t as usize, 0);
}

/// System reset: shutdown, reason "system failure" when `fail`.
pub fn shutdown(fail: bool) -> ! {
    call(EXT_SRST, 0, 0, fail as usize);
    loop {
        unsafe { asm!("wfi") }
    }
}
