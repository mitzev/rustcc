// RISC-V rv32 runner for qemu-system-riscv32 (machine `virt`,
// -bios none) using RISC-V semihosting. Same checks as runner.c —
// the Rust staticlib is identical, only boot + semihosting differ.
//
//   qemu-system-riscv32 -M virt -nographic -semihosting -bios none \
//       -kernel firmware_riscv.elf

extern int demo(int v);
extern int demo_subclass(int v, int scale);
extern int demo_imported_override(int id, int off);
extern int demo_imported_base(int id, int off);

// --- semihosting (riscv flavor: magic uncompressed 3-insn window) ---
static int sh(int op, void* arg) {
    register int a0 __asm__("a0") = op;
    register void* a1 __asm__("a1") = arg;
    __asm__ volatile(
        ".option push\n"
        ".option norvc\n"
        ".balign 16\n"
        "slli zero, zero, 0x1f\n"
        "ebreak\n"
        "srai zero, zero, 7\n"
        ".option pop\n"
        : "+r"(a0) : "r"(a1) : "memory");
    return a0;
}
static void sh_write0(const char* s) { sh(0x04, (void*)s); }
static void sh_exit(int code) {
    void* block[2] = { (void*)0x20026, (void*)(long)code };
    sh(0x20, block);
    for (;;) {}
}

static char* fmt_i32(char* p, int v) {
    if (v < 0) { *p++ = '-'; v = -v; }
    char tmp[12]; int n = 0;
    do { tmp[n++] = (char)('0' + v % 10); v /= 10; } while (v);
    while (n) *p++ = tmp[--n];
    return p;
}

extern unsigned __data_lma, __data_start, __data_end, __bss_start, __bss_end;

void _reset(void) {
    unsigned *src = &__data_lma, *dst = &__data_start;
    while (dst < &__data_end) *dst++ = *src++;
    for (dst = &__bss_start; dst < &__bss_end; ) *dst++ = 0;

    int a = demo(5);
    int b = demo_subclass(3, 4);
    int c = demo_imported_override(7, 3);
    int d = demo_imported_base(7, 3);

    if (a == 105 && b == 4000 && c == 503 && d == 42) {
        sh_write0("BARE-METAL RISC-V SUBCLASS: PASS "
                  "(demo=105 subclass=4000 imported=503 inherited=42)\n");
        sh_exit(0);
    }
    char msg[120], *p = msg;
    const char* pre = "BARE-METAL RISC-V SUBCLASS: FAIL demo=";
    while (*pre) *p++ = *pre++;
    p = fmt_i32(p, a);
    const char* s1 = " subclass=";
    while (*s1) *p++ = *s1++;
    p = fmt_i32(p, b);
    const char* s2 = " imported=";
    while (*s2) *p++ = *s2++;
    p = fmt_i32(p, c);
    const char* s3 = " inherited=";
    while (*s3) *p++ = *s3++;
    p = fmt_i32(p, d);
    *p++ = '\n'; *p = 0;
    sh_write0(msg);
    sh_exit(1);
}

// qemu -bios none starts executing at the start of RAM (0x80000000),
// where the linker script pins this entry stub.
__attribute__((naked, section(".start"), used))
void _start(void) {
    __asm__ volatile(
        "la sp, __stack_top\n"
        "j _reset\n");
}
