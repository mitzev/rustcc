# bare_metal_arm (+ RISC-V)

Polymorphic Rust classes — **including Rust subclasses of both a Rust
base and an imported C++ base** — on bare-metal ARM Cortex-M **and
RISC-V rv32**: `#![no_std]`, no heap, static storage, **executed under
qemu**, with the C++ side compiled by **GCC** (arm-none-eabi-g++ /
riscv64-elf-g++). Uses the `constructor` / `virtual` / `override`
keyword surface.

## What this proves

| Property | Confirmed |
|---|---|
| `#[repr(cpp)]` layout on 32-bit ARM | ✓ |
| Rust `_ZN6WidgetC1Ei` matches GCC's `Widget::Widget(int32_t)` mangling | ✓ |
| Calling convention: Rust sret+void return matches GCC call site | ✓ |
| Vtable shape `{ i32, ptr, ptr }` with 4-byte slots | ✓ |
| Vptr address-point offset `i32 8` (2 × 4 bytes) | ✓ |
| **Subclassing** `Gauge : Widget` (shared vptr, override by slot) heap-free | ✓ executed |
| C++ `Widget*` indirect dispatch (`ldr vptr; ldr slot; bx`) lands in the Rust `override fn` | ✓ executed |
| **Subclassing an imported C++ base** (`Reader : Sensor`, sensor.cpp GCC-compiled) heap-free — non-virtual dtor, so no `operator new`/`delete` anywhere | ✓ executed |
| Non-overridden slot in the Rust-emitted derived vtable dispatches back into GCC-compiled `Sensor::unit()` | ✓ executed |
| Runs on emulated Cortex-M4 (qemu `mps2-an386`, semihosting) | ✓ `PASS (demo=105 subclass=4000 imported=503 inherited=42)` |
| Same firmware logic on **RISC-V rv32imac** (qemu `virt`, P09.37 overlay) | ✓ same `PASS`, exit 0 |

## Prerequisites

- Forked `rustc` built and registered (see top-level README).
- `arm-none-eabi-gcc` / `arm-none-eabi-ld` (e.g. `brew install
  arm-none-eabi-gcc` on macOS).
- `thumbv7em-none-eabihf` std — build via `./x.py build --stage 1
  library --target thumbv7em-none-eabihf`, or use
  `-Zbuild-std=core,compiler_builtins` with a nightly cargo driver.
- For the runtime test: `qemu-system-arm` (`brew install qemu`).

## Build

```sh
cd examples/bare_metal_arm

# Drive via nightly cargo with stage-1 rustc override (stage-1
# cargo is older than library/std's edition-2024 requirement).
RUSTC=<rust-lang-rust>/build/host/stage1/bin/rustc \
RUSTC_BOOTSTRAP=1 \
cargo +nightly build --release \
    --target thumbv7em-none-eabihf \
    -Zbuild-std=core,compiler_builtins

# C++ side (caller + the imported Sensor base; sensor.cpp keeps RTTI
# on so GCC emits _ZTV6Sensor/_ZTI6Sensor for the Rust subclass to
# chain to):
arm-none-eabi-g++ -mcpu=cortex-m4 -mthumb -mfpu=fpv4-sp-d16 \
    -mfloat-abi=hard -O2 -ffreestanding -fno-exceptions -fno-rtti \
    -c caller.cpp -o caller.o
arm-none-eabi-g++ -mcpu=cortex-m4 -mthumb -mfpu=fpv4-sp-d16 \
    -mfloat-abi=hard -O2 -ffreestanding -fno-exceptions \
    -c sensor.cpp -o sensor.o

# RTTI stubs (weak __class_type_info / __si_class_type_info vtables;
# satisfy libc++abi symbols without pulling in libsupc++). Omit if
# you link libsupc++-nano and want real typeid / dynamic_cast.
arm-none-eabi-gcc -mcpu=cortex-m4 -mthumb -mfpu=fpv4-sp-d16 \
    -mfloat-abi=hard -c rtti_stub.c -o rtti_stub.o

# Freestanding runner (vector table + .data/.bss init + semihosting):
arm-none-eabi-gcc -mcpu=cortex-m4 -mthumb -mfpu=fpv4-sp-d16 \
    -mfloat-abi=hard -O2 -ffreestanding -c runner.c -o runner.o

# Full firmware ELF for qemu's mps2-an386 (Cortex-M4):
arm-none-eabi-gcc -mcpu=cortex-m4 -mthumb -mfpu=fpv4-sp-d16 \
    -mfloat-abi=hard -nostartfiles -nostdlib -T link.ld \
    runner.o caller.o sensor.o rtti_stub.o \
    target/thumbv7em-none-eabihf/release/libbare_metal_arm.a \
    -lgcc -o firmware.elf

# Execute on an emulated Cortex-M4:
qemu-system-arm -M mps2-an386 -nographic -semihosting -kernel firmware.elf
# -> BARE-METAL SUBCLASS: PASS (demo=105 subclass=4000 imported=503 inherited=42)
```

The four checks:

- `demo` placement-news the Rust base `Widget` from C++ and
  virtual-calls `foo()` (g++ devirtualizes to the direct
  `_ZNK6Widget3fooEv`).
- `demo_subclass` receives an opaque `Widget*` from the Rust factory
  `init_gauge` — which constructed a `Gauge : Widget` in **static
  storage** (no heap) — so g++ must dispatch indirectly:

  ```text
  bl   init_gauge      ; r0 = Widget* (really a Gauge)
  ldr  r3, [r0, #0]    ; load vptr
  ldr  r3, [r3, #0]    ; load slot 0 (foo)
  bx   r3              ; lands in Rust's `override fn foo`
  ```
- `demo_imported_override` does the same over the **imported C++
  base**: `Reader : Sensor` (sensor.cpp is GCC-compiled; the binding
  in `src/lib.rs` is hand-inlined importer output). `read()` through
  a `Sensor*` lands in the Rust override.
- `demo_imported_base` calls the NON-overridden `unit()` — the
  Rust-emitted derived vtable's slot 1 points straight at the
  GCC-compiled `_ZNK6Sensor4unitEv`. Heap-free is possible because
  `Sensor` has a non-virtual destructor (no `vdtor=1`, no
  `operator new`/`delete` anywhere; a virtual-dtor base needs the
  heap-backed deleting-dtor path — see the top-level README).

## RISC-V (rv32)

The same crate + C++ sources build unchanged for RISC-V (the fork's
P09.37 Itanium overlay); only the boot stub and semihosting sequence
differ (`runner_riscv.c`, `link_riscv.ld`):

```sh
RUSTC=<rust-lang-rust>/build/host/stage1/bin/rustc \
RUSTC_BOOTSTRAP=1 \
cargo +nightly build --release \
    --target riscv32imac-unknown-none-elf \
    -Zbuild-std=core,compiler_builtins

riscv64-elf-g++ -march=rv32imac -mabi=ilp32 -O2 -ffreestanding \
    -fno-exceptions -fno-rtti -c caller.cpp -o caller_rv.o
riscv64-elf-g++ -march=rv32imac -mabi=ilp32 -O2 -ffreestanding \
    -fno-exceptions -c sensor.cpp -o sensor_rv.o
riscv64-elf-gcc -march=rv32imac -mabi=ilp32 -O2 -ffreestanding \
    -c rtti_stub.c -o rtti_stub_rv.o
riscv64-elf-gcc -march=rv32imac -mabi=ilp32 -O2 -ffreestanding \
    -c runner_riscv.c -o runner_rv.o
riscv64-elf-gcc -march=rv32imac -mabi=ilp32 -nostartfiles -nostdlib \
    -T link_riscv.ld runner_rv.o caller_rv.o sensor_rv.o rtti_stub_rv.o \
    target/riscv32imac-unknown-none-elf/release/libbare_metal_arm.a \
    -lgcc -o firmware_riscv.elf

qemu-system-riscv32 -M virt -nographic -semihosting -bios none \
    -kernel firmware_riscv.elf
# -> BARE-METAL RISC-V SUBCLASS: PASS (demo=105 subclass=4000 imported=503 inherited=42)
```

(macOS: `brew install riscv64-elf-gcc qemu`.)

## Supported targets

Executed on `thumbv7em-none-eabihf` (Cortex-M4F, qemu mps2-an386) and
`riscv32imac-unknown-none-elf` (qemu virt). Extrapolates to the rest
of each family (no target-specific code in the fork's C++ ABI paths):

- `thumbv7m-none-eabi` — Cortex-M3
- `thumbv7em-none-eabi` — Cortex-M4 (no FPU)
- `thumbv7em-none-eabihf` — Cortex-M4F
- `thumbv8m.base-none-eabi` — Cortex-M23
- `thumbv8m.main-none-eabihf` — Cortex-M33F
- `riscv32imc / imac / imafc -unknown-none-elf` — rv32 MCU-class

## Caveat

Virtual dispatch works with the weak RTTI stub. `typeid` and
`dynamic_cast` need the real `libc++abi` or `libsupc++-nano` linked
in — the stub's empty vtable will trip both.
