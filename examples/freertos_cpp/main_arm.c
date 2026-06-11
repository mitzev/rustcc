/* STM32-class FreeRTOS probe (Cortex-M4, qemu mps2-an386).
 *
 * Two FreeRTOS tasks exercise the rustcc C++ interop:
 *   - prvCheckTask runs the four dispatch checks (Rust class,
 *     Rust subclass via base ptr, Rust subclass of an IMPORTED
 *     g++-compiled base — override + inherited slot) and queues
 *     each result.
 *   - prvReportTask drains the queue, verifies, prints the verdict
 *     over semihosting and exits qemu.
 *
 * The point: the fork's vtables / ctor vptr install / cross-compiler
 * RTTI all behave under a preemptive RTOS scheduler with its own
 * context switching (PendSV) — not just in a bare main().
 */
#include "FreeRTOS.h"
#include "task.h"
#include "queue.h"

/* C++ side (examples/bare_metal_arm/{caller,sensor}.cpp). */
extern int demo(int v);
extern int demo_subclass(int v, int scale);
extern int demo_imported_override(int id, int off);
extern int demo_imported_base(int id, int off);

/* --- semihosting ---------------------------------------------------- */
static int sh(int op, void* arg) {
    register int r0 __asm__("r0") = op;
    register void* r1 __asm__("r1") = arg;
    __asm__ volatile("bkpt 0xAB" : "+r"(r0) : "r"(r1) : "memory");
    return r0;
}
static void sh_write0(const char* s) { sh(0x04, (void*)s); }
static void sh_exit(int code) {
    void* block[2] = { (void*)0x20026, (void*)(long)code };
    sh(0x20, block);
    for (;;) {}
}

/* --- the probe tasks ------------------------------------------------ */
static QueueHandle_t xResults;

static void prvCheckTask(void* params) {
    (void)params;
    int r;
    r = demo(5);                      xQueueSend(xResults, &r, portMAX_DELAY);
    vTaskDelay(pdMS_TO_TICKS(2));     /* force a few context switches */
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
    sh_write0("FREERTOS CXX PROBE (ARM CM4): PASS "
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

/* --- FreeRTOS hooks -------------------------------------------------- */
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

/* --- boot: vector table + reset -------------------------------------- */
extern unsigned __data_lma, __data_start, __data_end, __bss_start, __bss_end;
extern void SVC_Handler(void);
extern void PendSV_Handler(void);
extern void SysTick_Handler(void);

void Reset_Handler(void) {
    unsigned *src = &__data_lma, *dst = &__data_start;
    while (dst < &__data_end) *dst++ = *src++;
    for (dst = &__bss_start; dst < &__bss_end; ) *dst++ = 0;
    main();
    for (;;) {}
}

static void Default_Handler(void) {
    sh_write0("FREERTOS CXX PROBE: FAIL (unexpected fault/IRQ)\n");
    sh_exit(8);
}

__attribute__((section(".vectors"), used))
const void* const __vectors[16] = {
    (const void*)0x20400000,      /* initial SP: top of the 4 MB SRAM */
    (const void*)Reset_Handler,   /* 1  Reset */
    (const void*)Default_Handler, /* 2  NMI */
    (const void*)Default_Handler, /* 3  HardFault */
    (const void*)Default_Handler, /* 4  MemManage */
    (const void*)Default_Handler, /* 5  BusFault */
    (const void*)Default_Handler, /* 6  UsageFault */
    0, 0, 0, 0,                   /* 7-10 reserved */
    (const void*)SVC_Handler,     /* 11 SVCall */
    (const void*)Default_Handler, /* 12 DebugMon */
    0,                            /* 13 reserved */
    (const void*)PendSV_Handler,  /* 14 PendSV */
    (const void*)SysTick_Handler, /* 15 SysTick */
};
