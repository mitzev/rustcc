/* Minimal Cortex-M layout for qemu's mps2-an386 (Cortex-M4):
 * code/rodata in the ZBT SRAM aliased at 0x00000000 (the vector
 * table must sit at 0x0), data/bss in the 0x20000000 SRAM. */
ENTRY(_reset)

MEMORY {
    FLASH (rx)  : ORIGIN = 0x00000000, LENGTH = 4M
    RAM   (rwx) : ORIGIN = 0x20000000, LENGTH = 4M
}

SECTIONS {
    .text : {
        KEEP(*(.vectors))
        *(.text*)
        *(.rodata*)
        . = ALIGN(4);
    } > FLASH

    /* No exceptions on this firmware, but Rust core / g++ emit the
     * index tables anyway — park them in flash. */
    .ARM.exidx : { *(.ARM.exidx*) *(.ARM.extab*) } > FLASH

    .data : ALIGN(4) {
        __data_start = .;
        *(.data*)
        . = ALIGN(4);
        __data_end = .;
    } > RAM AT > FLASH
    __data_lma = LOADADDR(.data);

    .bss (NOLOAD) : ALIGN(4) {
        __bss_start = .;
        *(.bss*)
        *(COMMON)
        . = ALIGN(4);
        __bss_end = .;
    } > RAM

    /DISCARD/ : { *(.ARM.attributes) *(.comment) }
}
