#!/usr/bin/env bash
# Build + run the STM32-class (Cortex-M4) FreeRTOS C++ interop probe
# under qemu. Env: RUSTC (fork stage1), FREERTOS_KERNEL (defaults to
# /tmp/FreeRTOS-Kernel, clone of FreeRTOS/FreeRTOS-Kernel V11.2.0).
set -euo pipefail
cd "$(dirname "$0")"

RUSTC="${RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}"
K="${FREERTOS_KERNEL:-/tmp/FreeRTOS-Kernel}"
if [[ ! -d "$K" ]]; then
  echo "==> cloning FreeRTOS-Kernel V11.2.0 to $K"
  git clone --depth 1 --branch V11.2.0 \
    https://github.com/FreeRTOS/FreeRTOS-Kernel "$K"
fi

CM="arm-none-eabi-gcc -mcpu=cortex-m4 -mthumb -mfpu=fpv4-sp-d16 \
    -mfloat-abi=hard -O2 -ffreestanding"
CMXX="arm-none-eabi-g++ -mcpu=cortex-m4 -mthumb -mfpu=fpv4-sp-d16 \
    -mfloat-abi=hard -O2 -ffreestanding -fno-exceptions"
INC="-I. -Ilibc_stub -I$K/include -I$K/portable/GCC/ARM_CM4F"

mkdir -p target/arm

echo "==> Rust staticlib (fork rustc, thumbv7em hard-float)"
RUSTC="$RUSTC" RUSTC_BOOTSTRAP=1 cargo +nightly build --release \
    --target thumbv7em-none-eabihf -Zbuild-std=core,compiler_builtins

echo "==> FreeRTOS kernel + glue (arm-none-eabi-gcc)"
for f in tasks list queue; do
  $CM $INC -c "$K/$f.c" -o "target/arm/$f.o"
done
$CM $INC -c "$K/portable/GCC/ARM_CM4F/port.c" -o target/arm/port.o
$CM $INC -c "$K/portable/MemMang/heap_4.c"    -o target/arm/heap_4.o
$CM $INC -c main_arm.c        -o target/arm/main.o
$CM $INC -c libc_stub/tinylibc.c -o target/arm/tinylibc.o

echo "==> C++ side (g++; shared with examples/bare_metal_arm)"
$CMXX -fno-rtti -c ../bare_metal_arm/caller.cpp -o target/arm/caller.o
$CMXX          -c ../bare_metal_arm/sensor.cpp  -o target/arm/sensor.o
$CM -c ../bare_metal_arm/rtti_stub.c            -o target/arm/rtti_stub.o

echo "==> link firmware"
$CM -nostartfiles -nostdlib -T link_arm.ld \
    target/arm/*.o \
    target/thumbv7em-none-eabihf/release/libfreertos_cpp.a \
    -lgcc -o target/arm/firmware.elf

if [[ "${GDB:-0}" == 1 ]]; then
  echo "==> qemu (mps2-an386) HALTED, gdbserver on :1234"
  echo "    attach: arm-none-eabi-gdb target/arm/firmware.elf \\"
  echo "            -ex 'target remote :1234' -ex 'break main' -ex continue"
  exec qemu-system-arm -M mps2-an386 -nographic -semihosting -s -S \
      -kernel target/arm/firmware.elf
fi
echo "==> qemu (mps2-an386)"
qemu-system-arm -M mps2-an386 -nographic -semihosting \
    -kernel target/arm/firmware.elf &
QPID=$!
( sleep 30 && kill $QPID 2>/dev/null ) &
WD=$!
wait $QPID; EC=$?
kill $WD 2>/dev/null || true
exit $EC
