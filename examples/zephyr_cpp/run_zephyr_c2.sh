#!/usr/bin/env bash
# ESP32-C2 (ESP8684) flavor — the RAK11161's second core. Same probe,
# built rv32**imc** (no atomic, no float) for the riscv32imc Rust
# target and run on qemu_riscv32 whose cpu@0 is overridden to rv32imc
# by boards/qemu_riscv32.overlay — so any stray atomic/float op traps
# instead of silently passing (ISA-exact, like the FreeRTOS C2 probe).
set -euo pipefail
cd "$(dirname "$0")"
ZEPHYR_BOARD=qemu_riscv32 \
RTARGET=riscv32imc-unknown-none-elf \
exec ./run_zephyr.sh
