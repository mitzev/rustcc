/* Minimal FreeRTOS config for the rustcc C++-interop probes.
 * Tuned for qemu's mps2-an386 (Cortex-M4, 25 MHz) and the RISC-V
 * qemu `virt` machine — nothing here is SoC-specific beyond the
 * clock rates. */
#ifndef FREERTOS_CONFIG_H
#define FREERTOS_CONFIG_H

#define configUSE_PREEMPTION                    1
#define configUSE_IDLE_HOOK                     0
#define configUSE_TICK_HOOK                     0
#ifdef __riscv
/* qemu -M virt CLINT runs mtime at 10 MHz. */
#define configCPU_CLOCK_HZ                      ( 10000000UL )
#define configMTIME_BASE_ADDRESS                ( 0x200BFF8UL )
#define configMTIMECMP_BASE_ADDRESS             ( 0x2004000UL )
#define configISR_STACK_SIZE_WORDS              ( 256 )
#else
/* qemu mps2-an386 SysTick clock. */
#define configCPU_CLOCK_HZ                      ( 25000000UL )
#endif
#define configTICK_RATE_HZ                      ( ( TickType_t ) 1000 )
#define configMAX_PRIORITIES                    ( 5 )
#define configMINIMAL_STACK_SIZE                ( ( unsigned short ) 256 )
#define configTOTAL_HEAP_SIZE                   ( ( size_t ) ( 24 * 1024 ) )
#define configMAX_TASK_NAME_LEN                 ( 10 )
#define configUSE_TRACE_FACILITY                0
#define configUSE_16_BIT_TICKS                  0
#define configIDLE_SHOULD_YIELD                 1
#define configUSE_MUTEXES                       0
#define configQUEUE_REGISTRY_SIZE               0
#define configCHECK_FOR_STACK_OVERFLOW          2
#define configUSE_RECURSIVE_MUTEXES             0
#define configUSE_MALLOC_FAILED_HOOK            1
#define configUSE_APPLICATION_TASK_TAG          0
#define configUSE_COUNTING_SEMAPHORES           0
#define configGENERATE_RUN_TIME_STATS           0
#define configUSE_TIMERS                        0
#define configUSE_TASK_NOTIFICATIONS            1
/* ARM_CM0 port (Raspberry Pi Pico flavor) requires these to be
 * stated explicitly; no MPU, no TrustZone on RP2040. */
#define configENABLE_MPU                        0
#define configENABLE_TRUSTZONE                  0
#define configENABLE_FPU                        0
#define configRUN_FREERTOS_SECURE_ONLY          0

/* No coroutines. */
#define configUSE_CO_ROUTINES                   0

/* Bare API set. */
#define INCLUDE_vTaskPrioritySet                0
#define INCLUDE_uxTaskPriorityGet               0
#define INCLUDE_vTaskDelete                     1
#define INCLUDE_vTaskCleanUpResources           0
#define INCLUDE_vTaskSuspend                    1
#define INCLUDE_vTaskDelayUntil                 0
#define INCLUDE_vTaskDelay                      1

/* Cortex-M interrupt priority config (ignored by the RISC-V port). */
#define configKERNEL_INTERRUPT_PRIORITY         ( 255 )
#define configMAX_SYSCALL_INTERRUPT_PRIORITY    ( 190 ) /* = 0xBE; LSB clear — qemu mps2 NVIC implements 8 prio bits */

extern void vAssertCalled( const char * file, int line );
#define configASSERT( x )                       \
    if( ( x ) == 0 ) { vAssertCalled( __FILE__, __LINE__ ); }

/* Map the port interrupt handlers onto the CMSIS-style names the
 * vector table uses. */
#define vPortSVCHandler     SVC_Handler
#define xPortPendSVHandler  PendSV_Handler
#define xPortSysTickHandler SysTick_Handler

#endif /* FREERTOS_CONFIG_H */
