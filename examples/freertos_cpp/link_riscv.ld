/* qemu -M virt -bios none: execution starts at 0x80000000. */
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

    .data : ALIGN(8) {
        __global_pointer$ = . + 0x800;
        *(.data*)
        *(.sdata*)
        . = ALIGN(4);
    } > RAM

    .bss (NOLOAD) : ALIGN(4) {
        __bss_start = .;
        *(.bss*)
        *(.sbss*)
        *(COMMON)
        . = ALIGN(4);
        __bss_end = .;
    } > RAM

    . = ALIGN(16);
    . = . + 0x40000;        /* 256 KiB boot/ISR stack */
    __stack_top = .;

    /DISCARD/ : { *(.eh_frame*) *(.comment) *(.riscv.attributes) }
}
