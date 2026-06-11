# freertos_cpp — rustcc C++ interop under FreeRTOS (executed on qemu)

Validates that rustcc's C++ machinery — `class`, subclassing, and
subclassing an **imported g++-compiled C++ base** — behaves under a
preemptive RTOS scheduler, not just in a bare `main()`. Two FreeRTOS
tasks split the work: one runs the dispatch checks (with `vTaskDelay`
between them to force real context switches through the port's
PendSV/trap machinery), the other receives each result over a queue
and verdicts.

The Rust side is byte-for-byte the proven
[`examples/bare_metal_arm`](../bare_metal_arm) crate (`[lib] path`
reuse — `Widget`, `Gauge : Widget`, imported `Sensor`,
`Reader : Sensor`), and the C++ side reuses its `caller.cpp` /
`sensor.cpp`. No heap from Rust; FreeRTOS heap_4 serves the kernel.

## Variants

| Variant | Core | FreeRTOS port | qemu machine | Run |
|---|---|---|---|---|
| **STM32-class** | Cortex-M4F (`thumbv7em-none-eabihf`) | `GCC/ARM_CM4F` | `mps2-an386` | `./run_arm.sh` |
| **ESP32-class** | rv32imac (`riscv32imac-unknown-none-elf`) | `GCC/RISC-V` + CLINT | `virt` | `./run_riscv.sh` |

Expected output:

```
FREERTOS CXX PROBE (ARM CM4):    PASS (105/4000/503/42 across tasks)
FREERTOS CXX PROBE (RISC-V rv32): PASS (105/4000/503/42 across tasks)
```

The four values: Rust base class virtual (105), Rust subclass override
through an opaque base pointer (4000), Rust override of an imported
g++-compiled base's virtual (503), and the NON-overridden inherited
slot dispatching back into the g++-compiled `Sensor::unit()` (42).

## ESP32 caveat (read before claiming SoC support)

The RISC-V variant runs the **upstream FreeRTOS machine-mode RISC-V
port on qemu's `virt` machine** — the same rv32 ISA and kernel the
ESP32-C3 uses, but *not* Espressif's esp-idf FreeRTOS build (different
interrupt controller, SoC peripherals, and Espressif's kernel
patches). It validates the architecture-level claim (rustcc classes +
vtables + cross-compiler dispatch + RTOS preemption on rv32);
SoC-level esp-idf integration is a separate exercise. Original-ESP32
(Xtensa LX6) is out of scope — upstream Rust has no Xtensa backend.

The STM32 variant has the same shape: qemu's `mps2-an386` is an ARM
reference board with the same Cortex-M4F core family STM32F4 parts
use; the kernel port (`ARM_CM4F`) is exactly the one STM32 projects
build.

## Prerequisites

- fork rustc stage1 (`RUSTC=` env or the default migration-tree path)
- `arm-none-eabi-gcc`, `riscv64-elf-gcc`, `qemu` (all via Homebrew)
- FreeRTOS-Kernel V11.2.0 — auto-cloned to `/tmp/FreeRTOS-Kernel`
  (override with `FREERTOS_KERNEL=`)

`libc_stub/` carries a minimal freestanding `string.h`/`stdlib.h` +
mem/str implementations: the Homebrew bare-metal cross compilers ship
no newlib, and the kernel needs `memset`/`memcpy`/`strlen`/`strcpy`.

Config notes: `configMAX_SYSCALL_INTERRUPT_PRIORITY` must keep its
LSB clear on qemu's mps2 (the NVIC implements all 8 priority bits);
the RISC-V port gets the qemu-virt CLINT addresses
(`configMTIME_BASE_ADDRESS 0x200BFF8`, `configMTIMECMP 0x2004000`).
