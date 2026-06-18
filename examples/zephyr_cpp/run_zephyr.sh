#!/usr/bin/env bash
# Build + run the Zephyr C++ interop probe under qemu.
#
# Prereqs (one-time): a west workspace + Zephyr SDK. See README.md.
#   ZEPHYR_BASE   defaults to ~/zephyrproject/zephyr
#   ZEPHYR_VENV   defaults to ~/zephyr-venv (where `west` lives)
#   RUSTC         fork stage1 rustc
#   ZEPHYR_BOARD  defaults to qemu_cortex_m3
#   SKIP_QEMU=1   build only (no qemu run)
set -euo pipefail
cd "$(dirname "$0")"

RUSTC="${RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}"
ZBASE="${ZEPHYR_BASE:-$HOME/zephyrproject/zephyr}"
VENV="${ZEPHYR_VENV:-$HOME/zephyr-venv}"
WEST="$VENV/bin/west"
BOARD="${ZEPHYR_BOARD:-qemu_cortex_m3}"
RTARGET="${RTARGET:-thumbv7m-none-eabi}"   # Cortex-M3, soft-float
BDIR="build/$BOARD"                        # per-board (the two cores don't collide)

echo "==> Rust class staticlib (fork rustc, $RTARGET)"
( cd ../bare_metal_arm
  RUSTC="$RUSTC" RUSTC_BOOTSTRAP=1 cargo +nightly build --release \
      --target "$RTARGET" -Zbuild-std=core,compiler_builtins )
RUST_LIB="$(cd ../bare_metal_arm && pwd)/target/$RTARGET/release/libbare_metal_arm.a"

export ZEPHYR_BASE="$ZBASE"
echo "==> west build ($BOARD)"
"$WEST" build -b "$BOARD" -p auto -d "$BDIR" . -- -DRUSTCC_RUST_LIB="$RUST_LIB"

if [[ "${SKIP_QEMU:-0}" == 1 ]]; then
  echo "(SKIP_QEMU=1 — built only)"
  exit 0
fi

echo "==> qemu run ($BOARD)"
LOG="$(pwd)/$BDIR/zephyr-run.log"
: > "$LOG"
"$WEST" build -d "$BDIR" -t run > "$LOG" 2>&1 &
WPID=$!
for _ in $(seq 1 40); do
  grep -q "ZEPHYR CXX PROBE" "$LOG" && break
  sleep 1
done
sleep 1
kill "$WPID" 2>/dev/null || true
pkill -f "qemu-system-arm" 2>/dev/null || true
echo "--- probe output ---"
grep "ZEPHYR CXX PROBE" "$LOG" || { echo "no probe line; full log:"; cat "$LOG"; exit 1; }
grep -q "PASS" "$LOG"
