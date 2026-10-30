/* riscv64 kernel: loaded by OpenSBI at 0x80200000, runs at the top 2 GB. */
OUTPUT_ARCH(riscv)
ENTRY(_start)

KERNEL_PHYS = 0x80200000;
KERNEL_VIRT = 0xFFFFFFFF80200000;

SECTIONS
{
    . = KERNEL_VIRT;
    __kernel_start = .;
    .text : AT(KERNEL_PHYS) {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }
    . = ALIGN(4096);
    __text_end = .;
    .rodata : AT(ADDR(.rodata) - KERNEL_VIRT + KERNEL_PHYS) {
        *(.rodata .rodata.* .srodata .srodata.*)
    }
    . = ALIGN(4096);
    __rodata_end = .;
    .data : AT(ADDR(.data) - KERNEL_VIRT + KERNEL_PHYS) {
        *(.data.boot_pt)
        *(.data .data.* .sdata .sdata.*)
    }
    .bss (NOLOAD) : AT(ADDR(.bss) - KERNEL_VIRT + KERNEL_PHYS) {
        __bss_start = .;
        *(.sbss .sbss.* .bss .bss.*)
        __bss_end = .;
    }
    . = ALIGN(4096);
    __kernel_end = .;
    /DISCARD/ : { *(.eh_frame .eh_frame_hdr .comment) }
}
