//! EL1 exception vectors (kernel only for now).
use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU64, Ordering};

use super::gic;
use crate::sched;
use crate::syscall::{self, Sys};
use crate::task;

pub const FRAME_SIZE: u64 = 272;

#[repr(C)]
pub struct Frame {
    pub x: [u64; 31],
    pub elr: u64,
    pub spsr: u64,
    pub sp_el0: u64,
}

global_asm!(
    r#"
    .macro VEC kind
    .balign 128
    sub sp, sp, #272
    stp x0, x1, [sp]
    mov x0, #\kind
    b trap_common
    .endm

    .section .text
    .balign 2048
    .global vectors
vectors:
    VEC 0
    VEC 1
    VEC 2
    VEC 3
    VEC 4
    VEC 5
    VEC 6
    VEC 7
    VEC 8
    VEC 9
    VEC 10
    VEC 11
    VEC 12
    VEC 13
    VEC 14
    VEC 15

trap_common:
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]
    stp x6, x7, [sp, #48]
    stp x8, x9, [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    mrs x9, elr_el1
    mrs x10, spsr_el1
    mrs x11, sp_el0
    stp x30, x9, [sp, #240]
    stp x10, x11, [sp, #256]
    mov x1, sp
    bl aarch64_trap
    .global trap_return
trap_return:
    ldp x30, x9, [sp, #240]
    ldp x10, x11, [sp, #256]
    msr elr_el1, x9
    msr spsr_el1, x10
    msr sp_el0, x11
    ldp x2, x3, [sp, #16]
    ldp x4, x5, [sp, #32]
    ldp x6, x7, [sp, #48]
    ldp x8, x9, [sp, #64]
    ldp x10, x11, [sp, #80]
    ldp x12, x13, [sp, #96]
    ldp x14, x15, [sp, #112]
    ldp x16, x17, [sp, #128]
    ldp x18, x19, [sp, #144]
    ldp x20, x21, [sp, #160]
    ldp x22, x23, [sp, #176]
    ldp x24, x25, [sp, #192]
    ldp x26, x27, [sp, #208]
    ldp x28, x29, [sp, #224]
    ldp x0, x1, [sp]
    add sp, sp, #272
    eret
    "#
);

unsafe extern "C" {
    static vectors: u8;
}

static ENTRY_EL: AtomicU64 = AtomicU64::new(1);

pub fn set_entry_el(el: u64) {
    ENTRY_EL.store(el, Ordering::Relaxed);
}

pub fn init_cpu() {
    let v = &raw const vectors;
    unsafe { asm!("msr vbar_el1, {}", "isb", in(reg) v) };
}

pub fn init() {
    init_cpu();
    let v = &raw const vectors;
    println!("trap: EL1 vectors at {:#x} (entered at EL{})", v as u64, ENTRY_EL.load(Ordering::Relaxed));
}

/// Vector index: 0-3 current EL with SP0, 4-7 current EL with SPx,
/// 8-11 lower EL AArch64, 12-15 lower EL AArch32; within each group
/// sync, IRQ, FIQ, SError.
const SYNC: u64 = 4;
const IRQ: u64 = 5;
const USER_SYNC: u64 = 8;
const USER_IRQ: u64 = 9;
const EC_BRK: u64 = 0x3c;
const EC_SVC: u64 = 0x15;
const EC_IABT_LOW: u64 = 0x20;
const EC_DABT_LOW: u64 = 0x24;

#[unsafe(no_mangle)]
extern "C" fn aarch64_trap(kind: u64, f: &mut Frame) {
    match kind {
        IRQ | USER_IRQ => gic::irq(),
        USER_SYNC => {
            let (esr, far): (u64, u64);
            unsafe {
                asm!("mrs {}, esr_el1", out(reg) esr);
                asm!("mrs {}, far_el1", out(reg) far);
            }
            let t = sched::current().expect("user trap without a thread");
            match esr >> 26 {
                EC_SVC => {
                    let a = [f.x[0], f.x[1], f.x[2], f.x[3], f.x[4], f.x[5]];
                    if let Sys::Ret(v) = syscall::dispatch(&t, f.x[8] as usize, a) {
                        f.x[0] = v as u64;
                    }
                }
                // WnR (ISS bit 6): a write; maybe a copy-on-write page.
                EC_DABT_LOW if t.task.aspace.fault(far, esr & (1 << 6) != 0) => {}
                EC_DABT_LOW | EC_IABT_LOW => {
                    task::fault(&t, format_args!("page fault at {:#x}, pc {:#x}", far, f.elr))
                }
                ec => task::fault(&t, format_args!("exception {:#x} at pc {:#x}", ec, f.elr)),
            }
        }
        SYNC => {
            let (esr, far): (u64, u64);
            unsafe {
                asm!("mrs {}, esr_el1", out(reg) esr);
                asm!("mrs {}, far_el1", out(reg) far);
            }
            if esr >> 26 == EC_BRK {
                crate::selftest::breakpoint_hit(f.elr);
                f.elr += 4;
            } else {
                panic!("trap: esr {:#x} far {:#x} elr {:#x}", esr, far, f.elr);
            }
        }
        _ => panic!("trap: vector {} elr {:#x}", kind, f.elr),
    }
    sched::trap_exit();
}
