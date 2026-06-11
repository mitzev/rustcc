#!/usr/bin/env bash
# Build + run the Cortex-M FreeRTOS C++ interop probe under qemu.
# Env: RUSTC (fork stage1), FREERTOS_KERNEL (defaults to
# /tmp/FreeRTOS-Kernel, clone of FreeRTOS/FreeRTOS-Kernel V11.2.0).
#
# Core flavor (default = STM32-class Cortex-M4F):
#   CM_CPUFLAGS  gcc cpu/fpu flags
#   CM_TARGET    Rust target
#   CM_PORT      FreeRTOS port dir (ARM_CM4F | ARM_CM0)
#   CM_MACHINE   qemu -M machine
#   CM_TAG       target subdir tag
# See run_pico.sh for the Raspberry Pi Pico (RP2040, Cortex-M0+)
# wrapper.
set -euo pipefail
cd "$(dirname "$0")"

RUSTC="${RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}"
K="${FREERTOS_KERNEL:-/tmp/FreeRTOS-Kernel}"
if [[ ! -d "$K" ]]; then
  echo "==> cloning FreeRTOS-Kernel V11.2.0 to $K"
  git clone --depth 1 --branch V11.2.0 \
    https://github.com/FreeRTOS/FreeRTOS-Kernel "$K"
fi

CM_CPUFLAGS="${CM_CPUFLAGS:--mcpu=cortex-m4 -mthumb -mfpu=fpv4-sp-d16 -mfloat-abi=hard}"
CM_TARGET="${CM_TARGET:-thumbv7em-none-eabihf}"
CM_PORT="${CM_PORT:-ARM_CM4F}"
CM_MACHINE="${CM_MACHINE:-mps2-an386}"
CM_TAG="${CM_TAG:-arm}"
CM="arm-none-eabi-gcc $CM_CPUFLAGS -O2 -ffreestanding"
CMXX="arm-none-eabi-g++ $CM_CPUFLAGS -O2 -ffreestanding -fno-exceptions"
INC="-I. -Ilibc_stub -I$K/include -I$K/portable/GCC/$CM_PORT"

mkdir -p "target/$CM_TAG"
rm -f "target/$CM_TAG"/*.o

echo "==> Rust staticlib (fork rustc, $CM_TARGET)"
RUSTC="$RUSTC" RUSTC_BOOTSTRAP=1 cargo +nightly build --release \
    --target "$CM_TARGET" -Zbuild-std=core,compiler_builtins

echo "==> FreeRTOS kernel + glue (arm-none-eabi-gcc)"
for f in tasks list queue; do
  $CM $INC -c "$K/$f.c" -o "target/$CM_TAG/$f.o"
done
# Some ports split across several TUs (ARM_CM0: port.c + portasm.c
# + the MPU wrappers, empty under configENABLE_MPU=0) — compile all.
for pc in "$K/portable/GCC/$CM_PORT/"*.c; do
  $CM $INC -c "$pc" -o "target/$CM_TAG/port_$(basename "${pc%.c}").o"
done
$CM $INC -c "$K/portable/MemMang/heap_4.c"    -o target/$CM_TAG/heap_4.o
$CM $INC -c main_arm.c        -o target/$CM_TAG/main.o
$CM $INC -c libc_stub/tinylibc.c -o target/$CM_TAG/tinylibc.o

echo "==> C++ side (g++; shared with examples/bare_metal_arm)"
$CMXX -fno-rtti -c ../bare_metal_arm/caller.cpp -o target/$CM_TAG/caller.o
$CMXX          -c ../bare_metal_arm/sensor.cpp  -o target/$CM_TAG/sensor.o
$CM -c ../bare_metal_arm/rtti_stub.c            -o target/$CM_TAG/rtti_stub.o

echo "==> link firmware"
$CM -nostartfiles -nostdlib -T link_arm.ld \
    target/$CM_TAG/*.o \
    "target/$CM_TARGET/release/libfreertos_cpp.a" \
    -lgcc -o target/$CM_TAG/firmware.elf

if [[ "${GDB:-0}" == 1 ]]; then
  echo "==> qemu ($CM_MACHINE) HALTED, gdbserver on :1234"
  echo "    attach: arm-none-eabi-gdb target/$CM_TAG/firmware.elf \\"
  echo "            -ex 'target remote :1234' -ex 'break main' -ex continue"
  exec qemu-system-arm -M "$CM_MACHINE" -nographic -semihosting -s -S \
      -kernel target/$CM_TAG/firmware.elf
fi
echo "==> qemu ($CM_MACHINE)"
qemu-system-arm -M "$CM_MACHINE" -nographic -semihosting \
    -kernel target/$CM_TAG/firmware.elf &
QPID=$!
( sleep 30 && kill $QPID 2>/dev/null ) &
WD=$!
wait $QPID; EC=$?
kill $WD 2>/dev/null || true
exit $EC
