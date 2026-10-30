//! Threads on RISC-V: the kernel context, switching, and user frames.
use core::arch::{asm, global_asm};

use super::trap::{FRAME_SIZE, Frame};
use crate::kstack::ThreadArch;
use crate::mm::aspace::AddrSpace;
use crate::mm::paging::PageTable;
use crate::task::Start;

/// Callee-saved registers of a thread switched out in the kernel.
#[repr(C)]
pub struct Context {
    ra: u64,
    sp: u64,
    s: [u64; 12],
}

impl Context {
    pub const ZERO: Context = Context { ra: 0, sp: 0, s: [0; 12] };
}

global_asm!(
    r#"
    .section .text
    .global arch_switch
arch_switch:
    sd ra, 0(a0)
    sd sp, 8(a0)
    sd s0, 16(a0)
    sd s1, 24(a0)
    sd s2, 32(a0)
    sd s3, 40(a0)
    sd s4, 48(a0)
    sd s5, 56(a0)
    sd s6, 64(a0)
    sd s7, 72(a0)
    sd s8, 80(a0)
    sd s9, 88(a0)
    sd s10, 96(a0)
    sd s11, 104(a0)
    ld ra, 0(a1)
    ld sp, 8(a1)
    ld s0, 16(a1)
    ld s1, 24(a1)
    ld s2, 32(a1)
    ld s3, 40(a1)
    ld s4, 48(a1)
    ld s5, 56(a1)
    ld s6, 64(a1)
    ld s7, 72(a1)
    ld s8, 80(a1)
    ld s9, 88(a1)
    ld s10, 96(a1)
    ld s11, 104(a1)
    ret

    .global new_thread
new_thread:
    call finish_switch
    j trap_return
    "#
);

unsafe extern "C" {
    fn arch_switch(from: *mut Context, to: *const Context);
    fn new_thread();
}

pub unsafe fn switch(from: *mut Context, to: *const Context) {
    unsafe { arch_switch(from, to) }
}

const SSTATUS_SPIE: u64 = 1 << 5;
/// FS = Initial: user mode may use the FP registers.
const SSTATUS_FS_INITIAL: u64 = 1 << 13;

/// A new thread's kernel stack: its user frame below `top`, and a context
/// that "returns" through `new_thread` / `trap_return` into user mode.
pub fn init_thread(top: u64, s: &Start) -> Context {
    let f = (top - FRAME_SIZE) as *mut Frame;
    unsafe {
        f.write(Frame { x: [0; 32], sepc: s.entry, sstatus: SSTATUS_SPIE | SSTATUS_FS_INITIAL });
        (*f).x[2] = s.stack;
        (*f).x[10] = s.arg;
    }
    Context { ra: new_thread as *const () as u64, sp: f as u64, s: [0; 12] }
}

/// The result of a blocked system call (a0).
pub fn set_return(t: &ThreadArch, v: i64) {
    let f = (t.frame_top() - FRAME_SIZE) as *mut Frame;
    unsafe { (*f).x[10] = v as u64 };
}

/// The trap entry loads `tp` (the CPU) from the slot above the frame.
pub fn set_thread_cpu(t: &ThreadArch, cpu: usize) {
    unsafe { (t.frame_top() as *mut u64).write(cpu as u64) };
}

static mut KERNEL_ROOT: u64 = 0;

pub fn kernel_root() -> u64 {
    unsafe { KERNEL_ROOT }
}

pub fn set_kernel_root(root: u64) {
    unsafe { KERNEL_ROOT = root };
}

/// User tables share the kernel half of the root.
pub fn init_user_table(pt: &mut PageTable) {
    unsafe {
        let k = super::phys_to_virt(KERNEL_ROOT) as *const u64;
        let u = super::phys_to_virt(pt.root) as *mut u64;
        for i in 256..512 {
            *u.add(i) = *k.add(i);
        }
    }
}

/// satp with the address space's ASID: no flush, unless it has none.
pub fn activate(a: &AddrSpace) {
    let asid = a.asid() as u64;
    unsafe { asm!("csrw satp, {}", in(reg) (8u64 << 60) | (asid << 44) | (a.root() >> 12)) };
    if asid == 0 {
        unsafe { asm!("sfence.vma") };
    }
}

/// Drops every hart's TLB entries of `asid`.
pub fn flush_asid(asid: u16) {
    unsafe { asm!("sfence.vma x0, {}", in(reg) asid as u64) };
    super::sbi::remote_sfence_asid(asid as usize);
}

/// After writing code: every hart's instruction cache.
pub fn sync_icache() {
    unsafe { asm!("fence.i") };
    super::sbi::remote_fence_i();
}

/// How many ASID bits satp keeps (writes all ones, reads back).
pub fn asid_bits(root: u64) -> u32 {
    let r: u64;
    unsafe {
        asm!(
            "csrw satp, {p}",
            "csrr {r}, satp",
            "csrw satp, {k}",
            "sfence.vma",
            p = in(reg) (8u64 << 60) | (0xffffu64 << 44) | (root >> 12),
            k = in(reg) (8u64 << 60) | (root >> 12),
            r = out(reg) r,
        )
    };
    ((r >> 44) & 0xffff).count_ones()
}

/// Lower half of Sv39.
pub fn user_addr_ok(va: u64) -> bool {
    va < 0x40_0000_0000
}

pub fn task_exited(a: &AddrSpace) {
    a.release();
}

/// Every hart's TLB (SBI remote fence; all harts when the base is -1).
pub fn flush_tlb() {
    unsafe { asm!("sfence.vma") };
    super::sbi::remote_sfence_all();
}

/// f0..f31 and fcsr.
#[repr(C, align(16))]
pub struct FpState([u64; 34]);

impl FpState {
    pub const ZERO: FpState = FpState([0; 34]);
}

global_asm!(
    r#"
    .option push
    .option arch, +d
    .global fp_save_asm
fp_save_asm:
    li t0, 3 << 13
    csrs sstatus, t0
    fsd f0, 0(a0)
    fsd f1, 8(a0)
    fsd f2, 16(a0)
    fsd f3, 24(a0)
    fsd f4, 32(a0)
    fsd f5, 40(a0)
    fsd f6, 48(a0)
    fsd f7, 56(a0)
    fsd f8, 64(a0)
    fsd f9, 72(a0)
    fsd f10, 80(a0)
    fsd f11, 88(a0)
    fsd f12, 96(a0)
    fsd f13, 104(a0)
    fsd f14, 112(a0)
    fsd f15, 120(a0)
    fsd f16, 128(a0)
    fsd f17, 136(a0)
    fsd f18, 144(a0)
    fsd f19, 152(a0)
    fsd f20, 160(a0)
    fsd f21, 168(a0)
    fsd f22, 176(a0)
    fsd f23, 184(a0)
    fsd f24, 192(a0)
    fsd f25, 200(a0)
    fsd f26, 208(a0)
    fsd f27, 216(a0)
    fsd f28, 224(a0)
    fsd f29, 232(a0)
    fsd f30, 240(a0)
    fsd f31, 248(a0)
    frcsr t0
    sd t0, 256(a0)
    ret

    .global fp_restore_asm
fp_restore_asm:
    li t0, 3 << 13
    csrs sstatus, t0
    fld f0, 0(a0)
    fld f1, 8(a0)
    fld f2, 16(a0)
    fld f3, 24(a0)
    fld f4, 32(a0)
    fld f5, 40(a0)
    fld f6, 48(a0)
    fld f7, 56(a0)
    fld f8, 64(a0)
    fld f9, 72(a0)
    fld f10, 80(a0)
    fld f11, 88(a0)
    fld f12, 96(a0)
    fld f13, 104(a0)
    fld f14, 112(a0)
    fld f15, 120(a0)
    fld f16, 128(a0)
    fld f17, 136(a0)
    fld f18, 144(a0)
    fld f19, 152(a0)
    fld f20, 160(a0)
    fld f21, 168(a0)
    fld f22, 176(a0)
    fld f23, 184(a0)
    fld f24, 192(a0)
    fld f25, 200(a0)
    fld f26, 208(a0)
    fld f27, 216(a0)
    fld f28, 224(a0)
    fld f29, 232(a0)
    fld f30, 240(a0)
    fld f31, 248(a0)
    ld t0, 256(a0)
    fscsr t0
    ret
    .option pop
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
