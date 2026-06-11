/* qemu-system-riscv32 -M virt -bios none: execution begins at the
 * start of RAM (0x80000000). Pin the entry stub there. */
ENTRY(_start)

MEMORY {
    RAM (rwx) : ORIGIN = 0x80000000, LENGTH = 64M
}

SECTIONS {
    .text : {
        KEEP(*(.start))
        *(.text*)
        *(.rodata*)
        *(.srodata*)
        . = ALIGN(4);
    } > RAM

    .data : ALIGN(4) {
        __data_start = .;
        *(.data*)
        *(.sdata*)
        . = ALIGN(4);
        __data_end = .;
    } > RAM
    /* Everything lives in one RAM; .data needs no LMA copy, but keep
     * the runner's copy loop valid: LMA == VMA makes it a no-op. */
    __data_lma = LOADADDR(.data);

    .bss (NOLOAD) : ALIGN(4) {
        __bss_start = .;
        *(.bss*)
        *(.sbss*)
        *(COMMON)
        . = ALIGN(4);
        __bss_end = .;
    } > RAM

    . = ALIGN(16);
    . = . + 0x40000;        /* 256 KiB stack */
    __stack_top = .;

    /DISCARD/ : { *(.eh_frame*) *(.comment) *(.riscv.attributes) }
}
