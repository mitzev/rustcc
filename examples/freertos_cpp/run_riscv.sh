#!/usr/bin/env bash
# Build + run the ESP32-class (rv32, FreeRTOS RISC-V machine-mode
# port, qemu virt + CLINT) C++ interop probe. Env: RUSTC,
# FREERTOS_KERNEL (defaults to /tmp/FreeRTOS-Kernel).
set -euo pipefail
cd "$(dirname "$0")"

RUSTC="${RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}"
K="${FREERTOS_KERNEL:-/tmp/FreeRTOS-Kernel}"
if [[ ! -d "$K" ]]; then
  echo "==> cloning FreeRTOS-Kernel V11.2.0 to $K"
  git clone --depth 1 --branch V11.2.0 \
    https://github.com/FreeRTOS/FreeRTOS-Kernel "$K"
fi

RV="riscv64-elf-gcc -march=rv32imac_zicsr -mabi=ilp32 -O2 -ffreestanding"
RVXX="riscv64-elf-g++ -march=rv32imac_zicsr -mabi=ilp32 -O2 -ffreestanding -fno-exceptions"
PORT="$K/portable/GCC/RISC-V"
INC="-I. -Ilibc_stub -I$K/include -I$PORT \
     -I$PORT/chip_specific_extensions/RISCV_MTIME_CLINT_no_extensions"

mkdir -p target/riscv

echo "==> Rust staticlib (fork rustc, riscv32imac)"
RUSTC="$RUSTC" RUSTC_BOOTSTRAP=1 cargo +nightly build --release \
    --target riscv32imac-unknown-none-elf -Zbuild-std=core,compiler_builtins

echo "==> FreeRTOS kernel + RISC-V port"
for f in tasks list queue; do
  $RV $INC -c "$K/$f.c" -o "target/riscv/$f.o"
done
$RV $INC -c "$PORT/port.c"                 -o target/riscv/port.o
$RV $INC -c "$PORT/portASM.S"              -o target/riscv/portasm.o
$RV $INC -c "$K/portable/MemMang/heap_4.c" -o target/riscv/heap_4.o
$RV $INC -c main_riscv.c                   -o target/riscv/main.o
$RV $INC -c libc_stub/tinylibc.c           -o target/riscv/tinylibc.o

echo "==> C++ side (g++; shared with examples/bare_metal_arm)"
$RVXX -fno-rtti -c ../bare_metal_arm/caller.cpp -o target/riscv/caller.o
$RVXX           -c ../bare_metal_arm/sensor.cpp -o target/riscv/sensor.o
$RV -c ../bare_metal_arm/rtti_stub.c            -o target/riscv/rtti_stub.o

echo "==> link firmware"
$RV -nostartfiles -nostdlib -T link_riscv.ld \
    target/riscv/*.o \
    target/riscv32imac-unknown-none-elf/release/libfreertos_cpp.a \
    -lgcc -o target/riscv/firmware.elf

echo "==> qemu (virt, rv32)"
qemu-system-riscv32 -M virt -nographic -semihosting -bios none \
    -kernel target/riscv/firmware.elf &
QPID=$!
( sleep 30 && kill $QPID 2>/dev/null ) &
WD=$!
wait $QPID; EC=$?
kill $WD 2>/dev/null || true
exit $EC
