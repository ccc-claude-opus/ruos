//! Entry from OpenSBI (a0 = hart id, a1 = DTB, MMU off, at 0x80200000).
//!
//! A static boot page table maps the image's gigabyte both 1:1 and at
//! KERNEL_VIRT, and the first 4 GB of physical memory at PHYS_OFFSET; the
//! entry code turns it on and jumps to the linked (upper) address.
use core::arch::global_asm;

use super::PHYS_OFFSET;
use crate::boot::BootInfo;
use crate::dtb::Fdt;

#[repr(C, align(4096))]
struct Table([u64; 512]);

/// V R W X A D: a readable / writable / executable leaf.
const LEAF: u64 = 0xcf;

#[unsafe(no_mangle)]
#[unsafe(link_section = ".data.boot_pt")]
static mut BOOT_PT: Table = {
    let mut t = [0u64; 512];
    t[2] = ((0x8000_0000u64 >> 12) << 10) | LEAF; // identity, 0x80000000
    t[510] = ((0x8000_0000u64 >> 12) << 10) | LEAF; // 0xFFFFFFFF80000000
    let mut i = 0;
    while i < 4 {
        t[256 + i] = (((i as u64) << 30 >> 12) << 10) | LEAF; // direct map
        i += 1;
    }
    Table(t)
};

const STACK_SIZE: usize = 64 * 1024;

#[unsafe(no_mangle)]
static mut BOOT_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

global_asm!(
    r#"
    .section .text.boot, "ax"
    .global _start
_start:
    la t0, BOOT_PT
    srli t0, t0, 12
    li t1, 8 << 60
    or t0, t0, t1
    csrw satp, t0
    sfence.vma
    la t0, 1f
    ld t0, 0(t0)
    jr t0
    .balign 8
1:  .dword 2f

    .section .text
2:
    la sp, BOOT_STACK
    li t0, {stack}
    add sp, sp, t0
    la t0, __bss_start
    la t1, __bss_end
3:  bgeu t0, t1, 4f
    sd zero, 0(t0)
    addi t0, t0, 8
    j 3b
4:
    li tp, 0
    call riscv_start
5:  wfi
    j 5b

    // A secondary hart (SBI hart_start): a0 = hart id, a1 = CPU index.
    .section .text.boot, "ax"
    .global _secondary
_secondary:
    la t0, BOOT_PT
    srli t0, t0, 12
    li t1, 8 << 60
    or t0, t0, t1
    csrw satp, t0
    sfence.vma
    la t0, 6f
    ld t0, 0(t0)
    jr t0
    .balign 8
6:  .dword 7f

    .section .text
7:
    la t0, SECONDARY_STACKS
    slli t1, a1, 3
    add t0, t0, t1
    ld sp, 0(t0)
    mv tp, a1
    mv a0, a1
    call riscv_secondary
8:  wfi
    j 8b
    "#,
    stack = const STACK_SIZE,
);

/// Stack tops of the secondary harts, by CPU index.
#[unsafe(no_mangle)]
pub static mut SECONDARY_STACKS: [u64; super::MAX_CPUS] = [0; super::MAX_CPUS];

#[unsafe(no_mangle)]
extern "C" fn riscv_start(hart: u64, dtb_pa: u64) -> ! {
    super::set_boot_hart(hart as usize);
    let fdt = unsafe { Fdt::from_ptr((dtb_pa + PHYS_OFFSET) as *const u8) }
        .unwrap_or_else(|| super::sbi::shutdown(true));
    let info = BootInfo::new(fdt, dtb_pa);
    if let Some(c) = &info.console {
        super::uart::init(c.reg.base + PHYS_OFFSET);
    }
    crate::kmain(&info);
    super::idle()
}

unsafe extern "C" {
    fn _secondary();
}

/// Virtual address of the secondary entry.
pub fn secondary_entry() -> u64 {
    _secondary as *const () as u64
}
