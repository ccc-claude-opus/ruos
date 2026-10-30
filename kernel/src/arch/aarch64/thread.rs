//! Threads on AArch64: the kernel context, switching, and user frames.
use core::arch::{asm, global_asm};

use super::trap::{FRAME_SIZE, Frame};
use crate::kstack::ThreadArch;
use crate::mm::aspace::AddrSpace;
use crate::mm::paging::PageTable;
use crate::task::Start;

/// Callee-saved registers (x19..x30) and sp of a thread switched out in the kernel.
#[repr(C)]
pub struct Context {
    x: [u64; 12],
    sp: u64,
}

impl Context {
    pub const ZERO: Context = Context { x: [0; 12], sp: 0 };
}

global_asm!(
    r#"
    .section .text
    .global arch_switch
arch_switch:
    stp x19, x20, [x0, #0]
    stp x21, x22, [x0, #16]
    stp x23, x24, [x0, #32]
    stp x25, x26, [x0, #48]
    stp x27, x28, [x0, #64]
    stp x29, x30, [x0, #80]
    mov x9, sp
    str x9, [x0, #96]
    ldp x19, x20, [x1, #0]
    ldp x21, x22, [x1, #16]
    ldp x23, x24, [x1, #32]
    ldp x25, x26, [x1, #48]
    ldp x27, x28, [x1, #64]
    ldp x29, x30, [x1, #80]
    ldr x9, [x1, #96]
    mov sp, x9
    ret

    .global new_thread
new_thread:
    bl finish_switch
    b trap_return
    "#
);

unsafe extern "C" {
    fn arch_switch(from: *mut Context, to: *const Context);
    fn new_thread();
}

pub unsafe fn switch(from: *mut Context, to: *const Context) {
    unsafe { arch_switch(from, to) }
}

/// A new thread's kernel stack: its user frame at the top (EL0t, interrupts
/// unmasked), and a context that "returns" through `trap_return`.
pub fn init_thread(top: u64, s: &Start) -> Context {
    let f = (top - FRAME_SIZE) as *mut Frame;
    unsafe {
        f.write(Frame { x: [0; 31], elr: s.entry, spsr: 0, sp_el0: s.stack });
        (*f).x[0] = s.arg;
    }
    let mut c = Context::ZERO;
    c.x[11] = new_thread as *const () as u64; // x30
    c.sp = f as u64;
    c
}

/// The result of a blocked system call (x0).
pub fn set_return(t: &ThreadArch, v: i64) {
    let f = (t.frame_top() - FRAME_SIZE) as *mut Frame;
    unsafe { (*f).x[0] = v as u64 };
}

/// The CPU id lives in TPIDR_EL1, out of user mode's reach.
pub fn set_thread_cpu(_t: &ThreadArch, _cpu: usize) {}

/// TTBR0 tables hold only user mappings.
pub fn init_user_table(_pt: &mut PageTable) {}

/// TTBR0 with the address space's ASID: no flush, unless it has none.
pub fn activate(a: &AddrSpace) {
    let asid = a.asid() as u64;
    unsafe { asm!("msr ttbr0_el1, {}", "isb", in(reg) a.root() | (asid << 48)) };
    if asid == 0 {
        unsafe { asm!("tlbi vmalle1", "dsb ish", "isb") };
    }
}

/// Drops every CPU's TLB entries of `asid`.
pub fn flush_asid(asid: u16) {
    unsafe { asm!("dsb ishst", "tlbi aside1is, {}", "dsb ish", "isb", in(reg) (asid as u64) << 48) };
}

/// After writing code: every CPU's instruction cache.
pub fn sync_icache() {
    unsafe { asm!("dsb ish", "ic ialluis", "dsb ish", "isb") };
}

/// 48-bit TTBR0 range.
pub fn user_addr_ok(va: u64) -> bool {
    va < 1 << 48
}

pub fn task_exited(a: &AddrSpace) {
    a.release();
}

/// Every CPU's TLB (inner shareable broadcast).
pub fn flush_tlb() {
    unsafe { asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb") };
}

/// q0..q31, fpcr, fpsr.
#[repr(C, align(16))]
pub struct FpState([u64; 66]);

impl FpState {
    pub const ZERO: FpState = FpState([0; 66]);
}

global_asm!(
    r#"
    .arch_extension fp
    .arch_extension simd
    .global fp_save_asm
fp_save_asm:
    stp q0, q1, [x0, #0]
    stp q2, q3, [x0, #32]
    stp q4, q5, [x0, #64]
    stp q6, q7, [x0, #96]
    stp q8, q9, [x0, #128]
    stp q10, q11, [x0, #160]
    stp q12, q13, [x0, #192]
    stp q14, q15, [x0, #224]
    stp q16, q17, [x0, #256]
    stp q18, q19, [x0, #288]
    stp q20, q21, [x0, #320]
    stp q22, q23, [x0, #352]
    stp q24, q25, [x0, #384]
    stp q26, q27, [x0, #416]
    stp q28, q29, [x0, #448]
    stp q30, q31, [x0, #480]
    mrs x9, fpcr
    mrs x10, fpsr
    add x11, x0, #512
    stp x9, x10, [x11]
    ret

    .global fp_restore_asm
fp_restore_asm:
    ldp q0, q1, [x0, #0]
    ldp q2, q3, [x0, #32]
    ldp q4, q5, [x0, #64]
    ldp q6, q7, [x0, #96]
    ldp q8, q9, [x0, #128]
    ldp q10, q11, [x0, #160]
    ldp q12, q13, [x0, #192]
    ldp q14, q15, [x0, #224]
    ldp q16, q17, [x0, #256]
    ldp q18, q19, [x0, #288]
    ldp q20, q21, [x0, #320]
    ldp q22, q23, [x0, #352]
    ldp q24, q25, [x0, #384]
    ldp q26, q27, [x0, #416]
    ldp q28, q29, [x0, #448]
    ldp q30, q31, [x0, #480]
    add x11, x0, #512
    ldp x9, x10, [x11]
    msr fpcr, x9
    msr fpsr, x10
    ret
    "#
);

unsafe extern "C" {
    fn fp_save_asm(f: *mut FpState);
    fn fp_restore_asm(f: *const FpState);
}

pub unsafe fn fp_save(f: *mut FpState) {
    unsafe { fp_save_asm(f) }
}

pub unsafe fn fp_restore(f: *const FpState) {
    unsafe { fp_restore_asm(f) }
}
