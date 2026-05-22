#!/usr/bin/env bash
# Run the v1.09.0 MSVC ABI smoke test.
#
# Builds `fork/tests/msvc_smoke/` with the host toolchain and
# verifies the cxx_importer pipeline produces MSVC-mangled
# `#[link_name]` attributes when configured for an MSVC target.
#
# Does NOT exercise the fork rustc patches (B.4) — those land in a
# separate sprint. This test only validates the Rust-side
# workspace pieces: mangler, layout, vtable, cxx_importer
# routing.
#
# Usage: ./fork/tests/run_msvc.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SMOKE_DIR="$SCRIPT_DIR/msvc_smoke"

if [ ! -d "$SMOKE_DIR" ]; then
    echo "error: $SMOKE_DIR not found" >&2
    exit 1
fi

echo "=> Building msvc_smoke against host toolchain"
echo "   (libclang parses with -target x86_64-pc-windows-msvc)"
echo

if ! cargo run --manifest-path "$SMOKE_DIR/Cargo.toml" 2>&1 | tee /tmp/msvc_smoke.out; then
    echo
    echo "FAIL: msvc_smoke run errored" >&2
    exit 1
fi

# Expect either "ok:" or "skip:" on the last line of summary
# output. Anything else is a failure.
if grep -qE "^(ok|skip):" /tmp/msvc_smoke.out; then
    echo
    echo "PASS"
    exit 0
fi

echo
echo "FAIL: no ok/skip summary found in output" >&2
exit 1
