#!/usr/bin/env bash
# Runs all class-keyword regression probes under the fork's stage-1
# rustc. Each probe is a standalone Cargo project; we build + run
# each and check the exit status plus the expected output banner.
#
# Usage:
#   RUSTC=<path to stage-1 rustc> ./fork/tests/run.sh
#
# If $RUSTC is unset we default to the fork's standard build location
# ($HOME/rust-lang-rust-fork, where fork/build.sh clones + builds).

set -euo pipefail

: "${RUSTC:=$HOME/rust-lang-rust-fork/build/host/stage1/bin/rustc}"

if [[ ! -x "$RUSTC" ]]; then
  echo "error: stage-1 rustc not found at $RUSTC" >&2
  echo "hint: set RUSTC=<path> or run ./x.py build --stage 1 library" >&2
  exit 1
fi

tests_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
echo "Using rustc: $RUSTC"
echo "Tests dir:   $tests_dir"
echo

declare -a probes=(
  "class_keyword/basic"
  "class_keyword/inheritance"
  "class_keyword/type_generics"
  "class_keyword/const_generics"
  "class_keyword/keyword_modifiers"
  "class_keyword/base_member_access"
  "class_keyword/swift_nonpod"
  "class_keyword/swift_value_attr"
  "class_keyword/swift_throws"
  "class_keyword/swift_extern_call"
  "class_keyword/swift_value_type"
)

# Map of probe directory to expected stdout prefix (matches the
# banner line each probe's main() prints on success).
declare -a expect=(
  "ok: basic class sum"
  "ok: inheritance sum"
  "ok: type-generic class"
  "ok: const-generic class sum"
  "ok: keyword modifiers virtual/override/constructor"
  "ok: base member access x="
  "ok: non-POD extra cloned"
  "ok: swift_value built-in attr"
  "ok: swift_throws wrapper"
  "ok: extern Swift add="
  "ok: swift_value value-type"
)

pass=0
fail=0
for i in "${!probes[@]}"; do
  probe="${probes[$i]}"
  banner="${expect[$i]}"
  dir="$tests_dir/$probe"
  name="$(basename "$probe")"
  printf "==> %-22s ... " "$name"
  pushd "$dir" >/dev/null
  # Rebuild from scratch to guarantee we're testing the current
  # compiler + macro sources, not a stale target/ cache.
  cargo clean -q 2>/dev/null || true
  if ! RUSTC="$RUSTC" RUSTC_BOOTSTRAP=1 cargo +nightly build -q 2>/tmp/class-probe-err.txt; then
    echo "BUILD FAILED"
    cat /tmp/class-probe-err.txt
    fail=$((fail + 1))
    popd >/dev/null
    continue
  fi
  # The binary sits at target/debug/<crate_name>.
  crate_name="$(awk -F'"' '/^name =/ {print $2; exit}' Cargo.toml)"
  out="$(./target/debug/"$crate_name" 2>&1)"
  if [[ "$out" == "$banner"* ]]; then
    echo "OK"
    pass=$((pass + 1))
  else
    echo "FAIL (unexpected output)"
    echo "  expected prefix: $banner"
    echo "  got:             $out"
    fail=$((fail + 1))
  fi
  popd >/dev/null
done

echo
echo "Result: $pass passed, $fail failed"
exit $((fail > 0 ? 1 : 0))
