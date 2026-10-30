/* aarch64 kernel: loaded by QEMU at 0x40080000 (arm64 Image), runs at the top 1 GB (TTBR1). */
OUTPUT_ARCH(aarch64)
ENTRY(_start)

KERNEL_PHYS = 0x40080000;
KERNEL_VIRT = 0xFFFFFFFFC0080000;

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
__kernel_size = __kernel_end - __kernel_start;
