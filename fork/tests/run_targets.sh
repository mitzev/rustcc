#!/usr/bin/env bash
# P09.44: cross-target build probe. Compiles the polymorphic
# class probe under `targets_linux/` for each listed triple and
# checks that the emitted LLVM IR contains the expected Itanium
# symbols (ctor, typeinfo, type-string, vtable).
#
# Skipped by default from ./fork/tests/run.sh — this probe takes
# ~40s per target and isn't needed for every regression check.
# Run on demand when adding target coverage:
#
#   RUSTC=<stage1> ./fork/tests/run_targets.sh
#
# Or to probe a single target:
#
#   RUSTC=<stage1> ./fork/tests/run_targets.sh aarch64-unknown-linux-gnu

set -euo pipefail

: "${RUSTC:=$HOME/rust-lang-rust-fork/build/host/stage1/bin/rustc}"

if [[ ! -x "$RUSTC" ]]; then
  echo "error: stage-1 rustc not found at $RUSTC" >&2
  exit 1
fi

tests_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
probe_dir="$tests_dir/targets_linux"

default_targets=(
  "x86_64-unknown-linux-gnu"
  "aarch64-unknown-linux-gnu"
  "armv7-unknown-linux-gnueabihf"
  "riscv64gc-unknown-linux-gnu"
)

if [[ $# -gt 0 ]]; then
  targets=("$@")
else
  targets=("${default_targets[@]}")
fi

echo "Using rustc: $RUSTC"
echo "Probe dir:   $probe_dir"
echo

cd "$probe_dir"
# Start clean so we're not reading stale IR from a prior run.
rm -rf target

pass=0
fail=0
for target in "${targets[@]}"; do
  printf "==> %-34s ... " "$target"
  # Build at -O0 so LLVM's late-pass DCE doesn't drop the vtable
  # / typeinfo globals. call_foo devirtualizes in release mode
  # because there's only one Widget::foo in scope, which lets
  # LLVM delete both the vtable and the vptr init — we want to
  # observe the fork's codegen emission, not LLVM's eventual
  # cleanup.
  if ! RUSTC="$RUSTC" RUSTC_BOOTSTRAP=1 cargo +nightly rustc \
       --target "$target" -Zbuild-std=core,compiler_builtins \
       -- --emit=llvm-ir -C opt-level=0 >/tmp/targets-probe-err.txt 2>&1; then
    echo "BUILD FAILED"
    cat /tmp/targets-probe-err.txt
    fail=$((fail + 1))
    continue
  fi
  ir=$(find "target/$target/debug/deps" -name "targets_linux*.ll" 2>/dev/null | head -1)
  if [[ -z "$ir" ]]; then
    echo "NO IR EMITTED"
    fail=$((fail + 1))
    continue
  fi
  # Check the four Itanium symbols we care about.
  missing=()
  for sym in "_ZN6Widget3newEi" "_ZTI6Widget" "_ZTS6Widget" "_ZTV6Widget"; do
    if ! /usr/bin/grep -q "$sym" "$ir"; then
      missing+=("$sym")
    fi
  done
  if [[ ${#missing[@]} -eq 0 ]]; then
    echo "OK"
    pass=$((pass + 1))
  else
    echo "FAIL (missing: ${missing[*]})"
    fail=$((fail + 1))
  fi
done

echo
echo "Result: $pass passed, $fail failed"
exit $((fail > 0 ? 1 : 0))
