//! Entry from QEMU's arm64 Image loader (x0 = DTB, MMU off, EL1 or EL2).
//!
//! Drops to EL1, turns on the MMU with static boot tables (the low 4 GB
//! both 1:1 in TTBR0 and at PHYS_OFFSET in TTBR1, the kernel's gigabyte at
//! the top), and jumps to the linked address.
use core::arch::global_asm;

use super::PHYS_OFFSET;
use crate::boot::BootInfo;
use crate::dtb::Fdt;

#[repr(C, align(4096))]
struct Table([u64; 512]);

const NORMAL: u64 = (1 << 10) | (3 << 8) | (1 << 2) | 1; // AF, inner shareable, MAIR 1, block
const DEVICE: u64 = (1 << 10) | (3 << 53) | 1; // AF, PXN | UXN, MAIR 0, block

/// L0: [0] → BOOT_L1_LOW, [511] → BOOT_L1_HIGH (filled in by `_start`).
#[unsafe(no_mangle)]
#[unsafe(link_section = ".data.boot_pt")]
static mut BOOT_L0: Table = Table([0; 512]);

#[unsafe(no_mangle)]
#[unsafe(link_section = ".data.boot_pt")]
static mut BOOT_L1_LOW: Table = {
    let mut t = [0u64; 512];
    t[0] = DEVICE;
    let mut i = 1;
    while i < 4 {
        t[i] = ((i as u64) << 30) | NORMAL;
        i += 1;
    }
    Table(t)
};

#[unsafe(no_mangle)]
#[unsafe(link_section = ".data.boot_pt")]
static mut BOOT_L1_HIGH: Table = {
    let mut t = [0u64; 512];
    t[511] = 0x4000_0000 | NORMAL;
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
    b 1f
    .word 0
    .quad 0x80000               // text_offset
    .quad __kernel_size
    .quad 2                     // little endian, 4 KB pages
    .quad 0, 0, 0
    .ascii "ARM\x64"
    .word 0
1:
    mov x19, x0
    mrs x20, CurrentEL
    mrs x9, CurrentEL
    lsr x9, x9, #2
    cmp x9, #2
    b.ne 2f
    // EL2: run EL1 in AArch64, let it use the counters, then drop.
    mov x9, #(1 << 31)
    msr hcr_el2, x9
    mov x9, #3
    msr cnthctl_el2, x9
    msr cntvoff_el2, xzr
    mov x9, #0x3c5              // EL1h, DAIF masked
    msr spsr_el2, x9
    adr x9, 2f
    msr elr_el2, x9
    eret
2:
    mov x9, #0x0800
    movk x9, #0x30d0, lsl #16   // SCTLR_EL1 RES1 bits, MMU off
    msr sctlr_el1, x9
    isb
    mov x9, #(3 << 20)          // CPACR_EL1.FPEN
    msr cpacr_el1, x9

    adrp x9, BOOT_L0
    add x9, x9, :lo12:BOOT_L0
    adrp x10, BOOT_L1_LOW
    add x10, x10, :lo12:BOOT_L1_LOW
    orr x10, x10, #3
    str x10, [x9]
    adrp x10, BOOT_L1_HIGH
    add x10, x10, :lo12:BOOT_L1_HIGH
    orr x10, x10, #3
    str x10, [x9, #(511 * 8)]

    mov x10, #0xff00            // MAIR: 0 = device nGnRnE, 1 = normal WB
    msr mair_el1, x10
    // TCR: T0SZ = T1SZ = 16, WB WA inner shareable walks, TG0 4 KB, TG1 4 KB
    mov x10, #0x3510
    movk x10, #0xb510, lsl #16
    mrs x11, id_aa64mmfr0_el1
    and x11, x11, #7
    orr x10, x10, x11, lsl #32  // IPS = PARange
    msr tcr_el1, x10
    msr ttbr0_el1, x9
    msr ttbr1_el1, x9
    isb
    tlbi vmalle1
    dsb ish
    isb
    mrs x10, sctlr_el1
    mov x11, #0x1005            // M, C, I
    orr x10, x10, x11
    msr sctlr_el1, x10
    isb
    adr x10, 3f
    ldr x10, [x10]
    br x10
    .balign 8
3:  .quad 4f

    .section .text
4:
    adrp x9, BOOT_STACK
    add x9, x9, :lo12:BOOT_STACK
    mov x10, #{stack}
    add sp, x9, x10
    adrp x9, __bss_start
    add x9, x9, :lo12:__bss_start
    adrp x10, __bss_end
    add x10, x10, :lo12:__bss_end
5:  cmp x9, x10
    b.hs 6f
    str xzr, [x9], #8
    b 5b
6:
    msr tpidr_el1, xzr
    mov x0, x19
    lsr x1, x20, #2
    bl aarch64_start
7:  wfi
    b 7b
    "#,
    stack = const STACK_SIZE,
);

// A secondary CPU (PSCI CPU_ON): x0 = CPU index, MMU off, EL1 or EL2. The
// boot tables are filled in already.
global_asm!(
    r#"
    .section .text.boot, "ax"
    .global _secondary
_secondary:
    mov x19, x0
    mrs x9, CurrentEL
    lsr x9, x9, #2
    cmp x9, #2
    b.ne 1f
    mov x9, #(1 << 31)
    msr hcr_el2, x9
    mov x9, #3
    msr cnthctl_el2, x9
    msr cntvoff_el2, xzr
    mov x9, #0x3c5
    msr spsr_el2, x9
    adr x9, 1f
    msr elr_el2, x9
    eret
1:
    mov x9, #0x0800
    movk x9, #0x30d0, lsl #16
    msr sctlr_el1, x9
    isb
    mov x9, #(3 << 20)
    msr cpacr_el1, x9
    adrp x9, BOOT_L0
    add x9, x9, :lo12:BOOT_L0
    mov x10, #0xff00
    msr mair_el1, x10
    mov x10, #0x3510
    movk x10, #0xb510, lsl #16
    mrs x11, id_aa64mmfr0_el1
    and x11, x11, #7
    orr x10, x10, x11, lsl #32
    msr tcr_el1, x10
    msr ttbr0_el1, x9
    msr ttbr1_el1, x9
    isb
    tlbi vmalle1
    dsb ish
    isb
    mrs x10, sctlr_el1
    mov x11, #0x1005
    orr x10, x10, x11
    msr sctlr_el1, x10
    isb
    adr x10, 2f
    ldr x10, [x10]
    br x10
    .balign 8
2:  .quad 3f

    .section .text
3:
    adrp x9, SECONDARY_STACKS
    add x9, x9, :lo12:SECONDARY_STACKS
    ldr x9, [x9, x19, lsl #3]
    mov sp, x9
    msr tpidr_el1, x19
    mov x0, x19
    bl aarch64_secondary
4:  wfi
    b 4b
    "#
);

/// Stack tops of the secondary CPUs, by index.
#[unsafe(no_mangle)]
pub static mut SECONDARY_STACKS: [u64; super::MAX_CPUS] = [0; super::MAX_CPUS];

unsafe extern "C" {
    fn _secondary();
}

/// Virtual address of the secondary entry.
pub fn secondary_entry() -> u64 {
    _secondary as *const () as u64
}

#[unsafe(no_mangle)]
extern "C" fn aarch64_start(dtb_pa: u64, entry_el: u64) -> ! {
    super::trap::set_entry_el(entry_el);
    let fdt = unsafe { Fdt::from_ptr((dtb_pa + PHYS_OFFSET) as *const u8) }
        .unwrap_or_else(|| super::psci::system_off(1));
    let info = BootInfo::new(fdt, dtb_pa);
    if let Some(c) = &info.console {
        super::pl011::init(c.reg.base + PHYS_OFFSET);
    }
    super::psci::init(&info);
    crate::kmain(&info);
    super::idle()
}
