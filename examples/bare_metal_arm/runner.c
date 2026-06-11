// Minimal Cortex-M runner for qemu-system-arm (machine mps2-an386,
// Cortex-M4) using ARM semihosting for output + exit. Freestanding:
// no libc, no startup files — the vector table below is the whole
// boot story.
//
//   qemu-system-arm -M mps2-an386 -nographic -semihosting -kernel firmware.elf
//
// Expects to print "BARE-METAL SUBCLASS: PASS" and exit.

extern int demo(int v);                    // caller.cpp — placement-new base
extern int demo_subclass(int v, int scale); // caller.cpp — Rust factory, Widget*

// --- semihosting -----------------------------------------------------
static int sh(int op, void* arg) {
    register int r0 __asm__("r0") = op;
    register void* r1 __asm__("r1") = arg;
    __asm__ volatile("bkpt 0xAB" : "+r"(r0) : "r"(r1) : "memory");
    return r0;
}
static void sh_write0(const char* s) { sh(0x04, (void*)s); }
static void sh_exit(int code) {
    // SYS_EXIT_EXTENDED (0x20): {reason, subcode} — qemu propagates
    // the subcode as its own exit status.
    void* block[2] = { (void*)0x20026 /* ADP_Stopped_ApplicationExit */,
                       (void*)(long)code };
    sh(0x20, block);
    for (;;) {}
}

// --- tiny itoa for the failure message -------------------------------
static char* fmt_i32(char* p, int v) {
    if (v < 0) { *p++ = '-'; v = -v; }
    char tmp[12]; int n = 0;
    do { tmp[n++] = (char)('0' + v % 10); v /= 10; } while (v);
    while (n) *p++ = tmp[--n];
    return p;
}

// --- boot ------------------------------------------------------------
extern unsigned __data_lma, __data_start, __data_end, __bss_start, __bss_end;

void _reset(void) {
    // Copy .data from its flash LMA, zero .bss.
    unsigned *src = &__data_lma, *dst = &__data_start;
    while (dst < &__data_end) *dst++ = *src++;
    for (dst = &__bss_start; dst < &__bss_end; ) *dst++ = 0;

    int a = demo(5);             // Widget::foo  -> v + 100      = 105
    int b = demo_subclass(3, 4); // Gauge::foo override -> 4*1000 = 4000

    if (a == 105 && b == 4000) {
        sh_write0("BARE-METAL SUBCLASS: PASS (demo=105 subclass=4000)\n");
        sh_exit(0);
    }
    char msg[80], *p = msg;
    const char* pre = "BARE-METAL SUBCLASS: FAIL demo=";
    while (*pre) *p++ = *pre++;
    p = fmt_i32(p, a);
    const char* mid = " subclass=";
    while (*mid) *p++ = *mid++;
    p = fmt_i32(p, b);
    *p++ = '\n'; *p = 0;
    sh_write0(msg);
    sh_exit(1);
}

// Initial SP (top of mps2 SRAM block) + reset handler. Taking a
// function's address in Thumb mode sets bit 0, as the hardware wants.
__attribute__((section(".vectors"), used))
const void* const __vectors[2] = {
    (const void*)0x20080000,
    (const void*)_reset,
};
