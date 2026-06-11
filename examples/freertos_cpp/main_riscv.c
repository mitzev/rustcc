/* ESP32-class FreeRTOS probe (RISC-V rv32, qemu `virt` + CLINT).
 *
 * Same two-task C++ interop check as main_arm.c. ESP32-C3 caveat:
 * this runs the upstream FreeRTOS RISC-V machine-mode port on qemu's
 * `virt` machine — the same rv32 ISA + kernel the ESP32-C3 runs, but
 * not Espressif's esp-idf build (different interrupt controller and
 * SoC peripherals). It validates the ARCHITECTURE-level claim: rustcc
 * classes, subclassing, and imported-C++-base dispatch are sound on
 * rv32 under a preemptive FreeRTOS scheduler.
 */
#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"

extern int demo(int v);
extern int demo_subclass(int v, int scale);
extern int demo_imported_override(int id, int off);
extern int demo_imported_base(int id, int off);

/* --- riscv semihosting ----------------------------------------------- */
static int sh(int op, void* arg) {
    register int a0 __asm__("a0") = op;
    register void* a1 __asm__("a1") = arg;
    __asm__ volatile(
        ".option push\n.option norvc\n.balign 16\n"
        "slli zero, zero, 0x1f\nebreak\nsrai zero, zero, 7\n"
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

/* --- probe tasks (identical logic to the ARM variant) ----------------- */
static QueueHandle_t xResults;

static void prvCheckTask(void* params) {
    (void)params;
    int r;
    r = demo(5);                      xQueueSend(xResults, &r, portMAX_DELAY);
    vTaskDelay(pdMS_TO_TICKS(2));
    r = demo_subclass(3, 4);          xQueueSend(xResults, &r, portMAX_DELAY);
    vTaskDelay(pdMS_TO_TICKS(2));
    r = demo_imported_override(7, 3); xQueueSend(xResults, &r, portMAX_DELAY);
    vTaskDelay(pdMS_TO_TICKS(2));
    r = demo_imported_base(7, 3);     xQueueSend(xResults, &r, portMAX_DELAY);
    vTaskDelete(NULL);
}

static void prvReportTask(void* params) {
    (void)params;
    static const int expected[4] = { 105, 4000, 503, 42 };
    int got[4];
    for (int i = 0; i < 4; i++) {
        if (xQueueReceive(xResults, &got[i], pdMS_TO_TICKS(5000)) != pdPASS) {
            sh_write0("FREERTOS CXX PROBE: FAIL (queue timeout)\n");
            sh_exit(2);
        }
    }
    for (int i = 0; i < 4; i++) {
        if (got[i] != expected[i]) {
            sh_write0("FREERTOS CXX PROBE: FAIL (value mismatch)\n");
            sh_exit(3);
        }
    }
    sh_write0("FREERTOS CXX PROBE (RISC-V rv32): PASS "
              "(105/4000/503/42 across tasks)\n");
    sh_exit(0);
}

int main(void) {
    xResults = xQueueCreate(8, sizeof(int));
    xTaskCreate(prvCheckTask, "check", configMINIMAL_STACK_SIZE * 2, NULL, 2, NULL);
    xTaskCreate(prvReportTask, "report", configMINIMAL_STACK_SIZE * 2, NULL, 1, NULL);
    vTaskStartScheduler();
    sh_write0("FREERTOS CXX PROBE: FAIL (scheduler returned)\n");
    sh_exit(4);
}

/* --- hooks ------------------------------------------------------------ */
void vApplicationStackOverflowHook(TaskHandle_t t, char* name) {
    (void)t; (void)name;
    sh_write0("FREERTOS CXX PROBE: FAIL (stack overflow)\n");
    sh_exit(5);
}
void vApplicationMallocFailedHook(void) {
    sh_write0("FREERTOS CXX PROBE: FAIL (malloc failed)\n");
    sh_exit(6);
}
void vAssertCalled(const char* file, int line) {
    char buf[160];
    char* p = buf;
    const char* pre = "FREERTOS CXX PROBE: FAIL (configASSERT at ";
    while (*pre) *p++ = *pre++;
    while (*file) *p++ = *file++;
    *p++ = ':';
    char tmp[12]; int n = 0;
    if (line == 0) tmp[n++] = '0';
    while (line > 0) { tmp[n++] = (char)('0' + line % 10); line /= 10; }
    while (n) *p++ = tmp[--n];
    *p++ = ')'; *p++ = '\n'; *p = 0;
    sh_write0(buf);
    sh_exit(7);
}

/* --- boot -------------------------------------------------------------- */
extern unsigned __bss_start, __bss_end;
extern void freertos_risc_v_trap_handler(void);

void _reset(void) {
    /* Single-RAM layout: no .data copy needed (LMA == VMA). */
    for (unsigned* p = &__bss_start; p < &__bss_end; ) *p++ = 0;
    /* The FreeRTOS machine-mode port owns the trap vector. */
    __asm__ volatile("csrw mtvec, %0"
                     :
                     : "r"((unsigned)&freertos_risc_v_trap_handler & ~3u));
    main();
    for (;;) {}
}

__attribute__((naked, section(".start"), used))
void _start(void) {
    __asm__ volatile(
        ".option push\n.option norelax\n"
        "la gp, __global_pointer$\n"
        ".option pop\n"
        "la sp, __stack_top\n"
        "j _reset\n");
}
