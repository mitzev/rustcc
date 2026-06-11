#!/usr/bin/env bash
# Raspberry Pi Pico flavor: RP2040 = Cortex-M0+ (thumbv6m, no FPU).
# The binary is compiler-enforced ARMv6-M; qemu's mps2-an385 (M3)
# executes it as an ISA superset — same stand-in pattern as the
# ESP32-C2 flavor (RP2040 SoC peripherals need hardware/pico-sdk).
set -euo pipefail
cd "$(dirname "$0")"
CM_CPUFLAGS="-mcpu=cortex-m0plus -mthumb" \
CM_TARGET=thumbv6m-none-eabi \
CM_PORT=ARM_CM0 \
CM_MACHINE=mps2-an385 \
CM_TAG=pico \
exec ./run_arm.sh
