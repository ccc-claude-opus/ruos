//! Supervisor traps. sscratch is 0 while in the kernel and holds the
//! current thread's kernel stack top while in user mode, so the entry code
//! can tell where it came from and find a stack.
use alloc::sync::Arc;
use core::arch::{asm, global_asm};

use super::plic;
use crate::sched;
use crate::syscall::{self, Sys};
use crate::task;

pub const FRAME_SIZE: u64 = 272;

#[repr(C)]
pub struct Frame {
    /// x0..x31 (x0 unused, x2 = interrupted sp).
    pub x: [u64; 32],
    pub sepc: u64,
    pub sstatus: u64,
}

global_asm!(
    r#"
    .section .text
    .balign 4
    .global trap_entry
    .global trap_return
trap_entry:
    csrrw sp, sscratch, sp
    bnez sp, 1f
    csrrw sp, sscratch, sp
    addi sp, sp, -272
    sd x1, 8(sp)
    sd x3, 24(sp)
    sd x4, 32(sp)
    sd x5, 40(sp)
    sd x6, 48(sp)
    sd x7, 56(sp)
    sd x8, 64(sp)
    sd x9, 72(sp)
    sd x10, 80(sp)
    sd x11, 88(sp)
    sd x12, 96(sp)
    sd x13, 104(sp)
    sd x14, 112(sp)
    sd x15, 120(sp)
    sd x16, 128(sp)
    sd x17, 136(sp)
    sd x18, 144(sp)
    sd x19, 152(sp)
    sd x20, 160(sp)
    sd x21, 168(sp)
    sd x22, 176(sp)
    sd x23, 184(sp)
    sd x24, 192(sp)
    sd x25, 200(sp)
    sd x26, 208(sp)
    sd x27, 216(sp)
    sd x28, 224(sp)
    sd x29, 232(sp)
    sd x30, 240(sp)
    sd x31, 248(sp)
    addi t0, sp, 272
    sd t0, 16(sp)
    j 2f
1:
    addi sp, sp, -272
    sd x1, 8(sp)
    sd x3, 24(sp)
    sd x4, 32(sp)
    sd x5, 40(sp)
    sd x6, 48(sp)
    sd x7, 56(sp)
    sd x8, 64(sp)
    sd x9, 72(sp)
    sd x10, 80(sp)
    sd x11, 88(sp)
    sd x12, 96(sp)
    sd x13, 104(sp)
    sd x14, 112(sp)
    sd x15, 120(sp)
    sd x16, 128(sp)
    sd x17, 136(sp)
    sd x18, 144(sp)
    sd x19, 152(sp)
    sd x20, 160(sp)
    sd x21, 168(sp)
    sd x22, 176(sp)
    sd x23, 184(sp)
    sd x24, 192(sp)
    sd x25, 200(sp)
    sd x26, 208(sp)
    sd x27, 216(sp)
    sd x28, 224(sp)
    sd x29, 232(sp)
    sd x30, 240(sp)
    sd x31, 248(sp)
    csrr t0, sscratch
    sd t0, 16(sp)
    csrw sscratch, zero
    ld tp, 272(sp)
2:
    csrr t0, sepc
    sd t0, 256(sp)
    csrr t0, sstatus
    sd t0, 264(sp)
    mv a0, sp
    call riscv_trap
trap_return:
    ld t0, 256(sp)
    csrw sepc, t0
    ld t0, 264(sp)
    csrw sstatus, t0
    andi t0, t0, 0x100
    bnez t0, 3f
    addi t0, sp, 272
    csrw sscratch, t0
3:
    ld x1, 8(sp)
    ld x3, 24(sp)
    ld x4, 32(sp)
    ld x5, 40(sp)
    ld x6, 48(sp)
    ld x7, 56(sp)
    ld x8, 64(sp)
    ld x9, 72(sp)
    ld x10, 80(sp)
    ld x11, 88(sp)
    ld x12, 96(sp)
    ld x13, 104(sp)
    ld x14, 112(sp)
    ld x15, 120(sp)
    ld x16, 128(sp)
    ld x17, 136(sp)
    ld x18, 144(sp)
    ld x19, 152(sp)
    ld x20, 160(sp)
    ld x21, 168(sp)
    ld x22, 176(sp)
    ld x23, 184(sp)
    ld x24, 192(sp)
    ld x25, 200(sp)
    ld x26, 208(sp)
    ld x27, 216(sp)
    ld x28, 224(sp)
    ld x29, 232(sp)
    ld x30, 240(sp)
    ld x31, 248(sp)
    ld sp, 16(sp)
    sret
    "#
);

unsafe extern "C" {
    fn trap_entry();
}

pub fn init() {
    let v = init_cpu();
    println!("trap: stvec at {:#x} (S-mode)", v);
}

/// This hart's trap vector.
pub fn init_cpu() -> usize {
    let v = trap_entry as *const () as usize;
    unsafe { asm!("csrw sscratch, zero", "csrw stvec, {}", in(reg) v) };
    v
}

const INTERRUPT: u64 = 1 << 63;
const SPP: u64 = 1 << 8;
const ECALL_U: u64 = 8;

#[unsafe(no_mangle)]
extern "C" fn riscv_trap(f: &mut Frame) {
    let cause: u64;
    let tval: u64;
    unsafe {
        asm!("csrr {}, scause", out(reg) cause);
        asm!("csrr {}, stval", out(reg) tval);
    }
    let user = f.sstatus & SPP == 0;
    match cause {
        c if c == INTERRUPT | 5 => super::timer_irq(),
        c if c == INTERRUPT | 9 => plic::irq(),
        c if c == INTERRUPT | 1 => {
            unsafe { asm!("csrci sip, 2") };
            sched::ipi();
        }
        ECALL_U => {
            f.sepc += 4;
            let t = sched::current().expect("ecall without a thread");
            let a = [f.x[10], f.x[11], f.x[12], f.x[13], f.x[14], f.x[15]];
            if let Sys::Ret(v) = syscall::dispatch(&t, f.x[17] as usize, a) {
                f.x[10] = v as u64;
            }
        }
        3 if !user => {
            crate::selftest::breakpoint_hit(f.sepc);
            // c.ebreak is 2 bytes, ebreak 4.
            let half = unsafe { (f.sepc as *const u16).read() };
            f.sepc += if half & 3 == 3 { 4 } else { 2 };
        }
        15 if user && sched::current().is_some_and(|t| t.task.aspace.fault(tval, true)) => {}
        _ if user => {
            let t: Arc<task::Thread> = sched::current().expect("user trap without a thread");
            let what = match cause {
                12 | 13 | 15 => "page fault",
                2 => "illegal instruction",
                _ => "exception",
            };
            task::fault(&t, format_args!("{} at {:#x}, pc {:#x}", what, tval, f.sepc));
        }
        _ => panic!("trap: scause {:#x} stval {:#x} sepc {:#x}", cause, tval, f.sepc),
    }
    sched::trap_exit();
}
