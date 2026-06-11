/* Cortex-M layout for qemu mps2-an386, FreeRTOS probe. Code/rodata
 * in the ZBT SRAM aliased at 0x0 (vector table at 0); data/bss (incl.
 * the FreeRTOS heap_4 array) in the 0x20000000 SRAM. MSP starts at
 * the top of SRAM (vector slot 0). */
ENTRY(Reset_Handler)

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
