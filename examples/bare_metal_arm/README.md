# bare_metal_arm

Polymorphic Rust class on bare-metal ARM Cortex-M — `#![no_std]`,
no heap, static storage. Demonstrates that the fork's C++-ABI code
path works unchanged on 32-bit ARM (P09.35 target-awareness + P09.36).

## What this proves

| Property | Confirmed |
|---|---|
| `#[repr(cpp)]` layout on 32-bit ARM | ✓ |
| Rust `_ZN6WidgetC1Ei` matches GCC's `Widget::Widget(int32_t)` mangling | ✓ |
| Calling convention: Rust sret+void return matches GCC call site | ✓ |
| Vtable shape `{ i32, ptr, ptr }` with 4-byte slots | ✓ |
| Vptr address-point offset `i32 8` (2 × 4 bytes) | ✓ |

## Prerequisites

- Forked `rustc` built and registered (see top-level README).
- `arm-none-eabi-gcc` / `arm-none-eabi-ld` (e.g. `brew install
  arm-none-eabi-gcc` on macOS).
- `thumbv7em-none-eabihf` std — build via `./x.py build --stage 1
  library --target thumbv7em-none-eabihf`, or use
  `-Zbuild-std=core,compiler_builtins` with a nightly cargo driver.

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

# C++ side:
arm-none-eabi-g++ -mcpu=cortex-m4 -mthumb -mfpu=fpv4-sp-d16 \
    -mfloat-abi=hard -O2 -ffreestanding -fno-exceptions -fno-rtti \
    -c caller.cpp -o caller.o

# RTTI stub (3 lines; satisfies libc++abi symbol without pulling
# in libsupc++). Omit if you link libsupc++-nano and want real
# typeid / dynamic_cast.
arm-none-eabi-gcc -mcpu=cortex-m4 -mthumb -c rtti_stub.c -o rtti_stub.o

# Partial-relocatable link — combine into a single firmware blob.
arm-none-eabi-ld -r \
    caller.o rtti_stub.o \
    target/thumbv7em-none-eabihf/release/libbare_metal_arm.a \
    -o firmware.o
```

Inspect with `arm-none-eabi-objdump -d firmware.o | grep -A8 foo` —
you should see a single-instruction load of the vtable pointer
followed by a tail-call through slot 0.

## Supported targets

Tested on `thumbv7em-none-eabihf` (Cortex-M4F). Extrapolates to the
rest of the family (no target-specific code in the fork's C++ ABI
paths):

- `thumbv7m-none-eabi` — Cortex-M3
- `thumbv7em-none-eabi` — Cortex-M4 (no FPU)
- `thumbv7em-none-eabihf` — Cortex-M4F
- `thumbv8m.base-none-eabi` — Cortex-M23
- `thumbv8m.main-none-eabihf` — Cortex-M33F

## Caveat

Virtual dispatch works with the weak RTTI stub. `typeid` and
`dynamic_cast` need the real `libc++abi` or `libsupc++-nano` linked
in — the stub's empty vtable will trip both.
