#!/usr/bin/env bash
# Build + run the ESP32-class (rv32, FreeRTOS RISC-V machine-mode
# port, qemu virt + CLINT) C++ interop probe. Env: RUSTC,
# FREERTOS_KERNEL (defaults to /tmp/FreeRTOS-Kernel).
#
# ISA flavor (default = ESP32-C3-class rv32imac):
#   RV_MARCH    gcc -march        (e.g. rv32imc_zicsr for ESP32-C2)
#   RV_TARGET   Rust target       (e.g. riscv32imc-unknown-none-elf)
#   RV_QEMU_CPU qemu -cpu string  (e.g. rv32,a=false to FAULT on any
#                                  stray atomic — C2 has no A ext)
#   RV_TAG      target subdir tag
# See run_riscv_c2.sh for the ESP32-C2 wrapper.
set -euo pipefail
cd "$(dirname "$0")"

RUSTC="${RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}"
K="${FREERTOS_KERNEL:-/tmp/FreeRTOS-Kernel}"
if [[ ! -d "$K" ]]; then
  echo "==> cloning FreeRTOS-Kernel V11.2.0 to $K"
  git clone --depth 1 --branch V11.2.0 \
    https://github.com/FreeRTOS/FreeRTOS-Kernel "$K"
fi

RV_MARCH="${RV_MARCH:-rv32imac_zicsr}"
RV_TARGET="${RV_TARGET:-riscv32imac-unknown-none-elf}"
RV_QEMU_CPU="${RV_QEMU_CPU:-rv32}"
RV_TAG="${RV_TAG:-riscv}"
RV="riscv64-elf-gcc -march=$RV_MARCH -mabi=ilp32 -O2 -ffreestanding"
RVXX="riscv64-elf-g++ -march=$RV_MARCH -mabi=ilp32 -O2 -ffreestanding -fno-exceptions"
PORT="$K/portable/GCC/RISC-V"
INC="-I. -Ilibc_stub -I$K/include -I$PORT \
     -I$PORT/chip_specific_extensions/RISCV_MTIME_CLINT_no_extensions"

mkdir -p "target/$RV_TAG"

echo "==> Rust staticlib (fork rustc, $RV_TARGET)"
RUSTC="$RUSTC" RUSTC_BOOTSTRAP=1 cargo +nightly build --release \
    --target "$RV_TARGET" -Zbuild-std=core,compiler_builtins

echo "==> FreeRTOS kernel + RISC-V port"
for f in tasks list queue; do
  $RV $INC -c "$K/$f.c" -o "target/$RV_TAG/$f.o"
done
$RV $INC -c "$PORT/port.c"                 -o target/$RV_TAG/port.o
$RV $INC -c "$PORT/portASM.S"              -o target/$RV_TAG/portasm.o
$RV $INC -c "$K/portable/MemMang/heap_4.c" -o target/$RV_TAG/heap_4.o
$RV $INC -c main_riscv.c                   -o target/$RV_TAG/main.o
$RV $INC -c libc_stub/tinylibc.c           -o target/$RV_TAG/tinylibc.o

echo "==> C++ side (g++; shared with examples/bare_metal_arm)"
$RVXX -fno-rtti -c ../bare_metal_arm/caller.cpp -o target/$RV_TAG/caller.o
$RVXX           -c ../bare_metal_arm/sensor.cpp -o target/$RV_TAG/sensor.o
$RV -c ../bare_metal_arm/rtti_stub.c            -o target/$RV_TAG/rtti_stub.o

echo "==> link firmware"
$RV -nostartfiles -nostdlib -T link_riscv.ld \
    target/$RV_TAG/*.o \
    "target/$RV_TARGET/release/libfreertos_cpp.a" \
    -lgcc -o target/$RV_TAG/firmware.elf

echo "==> qemu (virt, rv32)"
qemu-system-riscv32 -M virt -cpu "$RV_QEMU_CPU" -nographic -semihosting -bios none \
    -kernel "target/$RV_TAG/firmware.elf" &
QPID=$!
( sleep 30 && kill $QPID 2>/dev/null ) &
WD=$!
wait $QPID; EC=$?
kill $WD 2>/dev/null || true
exit $EC
