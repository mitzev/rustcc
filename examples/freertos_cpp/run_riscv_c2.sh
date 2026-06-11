#!/usr/bin/env bash
# ESP32-C2 flavor: exact ISA match — RV32IMC (no atomic extension),
# single hart. Everything (Rust core, kernel, C++ side) is built
# rv32imc, and qemu's CPU has the A extension DISABLED so any stray
# atomic instruction faults instead of silently working.
set -euo pipefail
cd "$(dirname "$0")"
RV_MARCH=rv32imc_zicsr \
RV_TARGET=riscv32imc-unknown-none-elf \
RV_QEMU_CPU=rv32,a=false,zawrs=false \
RV_TAG=riscv-c2 \
exec ./run_riscv.sh
