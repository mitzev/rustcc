# zephyr_cpp — rustcc C++ interop under Zephyr RTOS (executed on qemu)

The Zephyr counterpart to [`examples/freertos_cpp`](../freertos_cpp):
the *same* validated rustcc machinery — the `class` keyword,
subclassing, and subclassing an **imported g++-compiled C++ base** —
now built by Zephyr's CMake/`west` build system and run on a Zephyr
image under qemu, rather than a hand-rolled FreeRTOS one.

The Rust side is **byte-for-byte** the proven
[`examples/bare_metal_arm`](../bare_metal_arm) crate (`Widget`,
`Gauge : Widget`, imported `Sensor`, `Reader : Sensor`); the C++ side
reuses its `caller.cpp` / `sensor.cpp` / `rtti_stub.c`. A Zephyr
`main()` runs the four dispatch checks and prints the verdict.

## Variants — both RAK11161 cores

| Variant | Core | Rust target | Zephyr board | Run |
|---|---|---|---|---|
| **STM32WLE5 side** | Cortex-M3 | `thumbv7m-none-eabi` | `qemu_cortex_m3` | `./run_zephyr.sh` |
| **ESP8684 / ESP32-C2 side** | rv32**imc** | `riscv32imc-unknown-none-elf` | `qemu_riscv32` (rv32imc overlay) | `./run_zephyr_c2.sh` |

Both print the same line (board name varies):

```
ZEPHYR CXX PROBE (qemu_cortex_m3): PASS (105/4000/503/42 — class, subclass, imported override + inherited)
ZEPHYR CXX PROBE (qemu_riscv32):   PASS (105/4000/503/42 — …)
```

The C2 flavor is **ISA-exact**: ESP32-C2 has no atomic (A) or float
(F/D) extensions, so `boards/qemu_riscv32.overlay` overrides the
board's `cpu@0` to `rv32imc`. Zephyr derives both the build ISA
(soft-float, software atomics) and the qemu `-cpu` from that property,
so a stray `amo*`/`lr`/`sc` would trap rather than silently pass —
verified statically too (`objdump` finds **zero** atomic instructions
in the image).

The four values: Rust base-class virtual (105), Rust subclass override
through an opaque base pointer (4000), Rust override of an imported
g++-compiled base's virtual (503), and the NON-overridden inherited
slot dispatching back into the g++-compiled `Sensor::unit()` (42).

## Build & run

Prereqs (one-time): a Zephyr **west workspace** + the **Zephyr SDK**
(ARM toolchain). The standard setup:

```sh
python3 -m venv ~/zephyr-venv && ~/zephyr-venv/bin/pip install west
~/zephyr-venv/bin/west init ~/zephyrproject && \
    cd ~/zephyrproject && ~/zephyr-venv/bin/west update
~/zephyr-venv/bin/west sdk install -t arm-zephyr-eabi   # ARM toolchain only
```

Then, from this directory:

```sh
RUSTC=<fork-stage1>/bin/rustc ./run_zephyr.sh      # STM32WLE5 side (Cortex-M3)
RUSTC=<fork-stage1>/bin/rustc ./run_zephyr_c2.sh   # ESP8684/ESP32-C2 side (rv32imc)
SKIP_QEMU=1 ./run_zephyr.sh                         # build only
```

Each core builds into its own `build/<board>/` dir. The C2 flavor
additionally needs the SDK's RISC-V toolchain
(`west sdk install -t riscv64-zephyr-elf`).

`run_zephyr.sh` builds the Rust `class` crate into
`libbare_metal_arm.a` for `thumbv7m-none-eabi` (Cortex-M3, soft-float,
via the fork rustc + `-Zbuild-std`), then `west build -b
qemu_cortex_m3` compiles the C++ side + Zephyr and links the Rust
archive (`-DRUSTCC_RUST_LIB=…`), and `west build -t run` executes it
under qemu.

## How the interop links

`prj.conf` enables C++17 (`CONFIG_CPP` / `CONFIG_STD_CPP17`); RTTI and
exceptions stay **off** — the cross-boundary RTTI rides the
fork-emitted `_ZTI` metadata + `rtti_stub.c`, exactly as the FreeRTOS
probe compiles `caller.cpp` with `-fno-rtti -fno-exceptions`. The
`CMakeLists.txt` adds `caller.cpp`/`sensor.cpp`/`rtti_stub.c` as app
sources and links the prebuilt Rust archive; `caller.cpp` references
the Rust `init_*` factories + the class ctor/vtable symbols, so the
linker pulls the fork-emitted class metadata in.

## Scope caveat (read before claiming SoC support)

`qemu_cortex_m3` models the **Cortex-M3 core**, not a specific vendor
SoC — the same architecture-level claim as the FreeRTOS probe (rustcc
classes + vtables + cross-compiler dispatch on an RTOS). It validates
that the fork's C++ interop builds and runs under Zephyr's toolchain
and kernel; board-specific bring-up (a real Nordic/NXP/ESP target) is
a separate exercise that reuses this exact Rust + C++ core.
