#!/usr/bin/env bash
# v1.09.2 MSVC runtime smoke test.
#
# Builds `fork/tests/msvc_runtime_smoke/` against the locally-built
# fork rustc, producing a real PE32+ Windows binary via lld-link +
# xwin's MSVC SDK. When Wine is installed, additionally runs the
# binary and checks exit code.
#
# Expects:
# - $RUSTCC_STAGE1 — path to the fork stage-1 rustc (defaults to
#   ~/rust-lang-rust-fork/build/host/stage1/bin/rustc).
# - `lld-link` on PATH (via `brew install lld`).
# - `xwin` SDK splatted to ~/.xwin (via `cargo install xwin &&
#   xwin --accept-license splat --output ~/.xwin`).
# - Optional: `wine64` on PATH for runtime validation.
#
# Usage: ./fork/tests/run_msvc_runtime.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SMOKE_DIR="$SCRIPT_DIR/msvc_runtime_smoke"

: "${RUSTCC_STAGE1:=$HOME/rust-lang-rust-fork/build/host/stage1}"

if [ ! -x "$RUSTCC_STAGE1/bin/rustc" ]; then
    echo "error: stage-1 rustc not found at $RUSTCC_STAGE1/bin/rustc" >&2
    echo "hint: set RUSTCC_STAGE1=<path> or run ./fork/build.sh first" >&2
    exit 1
fi

if ! command -v lld-link >/dev/null 2>&1; then
    echo "error: lld-link not on PATH (run 'brew install lld' on macOS)" >&2
    exit 1
fi

if [ ! -d "$HOME/.xwin/crt" ]; then
    echo "error: xwin SDK not splatted to ~/.xwin" >&2
    echo "hint: cargo install xwin && xwin --accept-license splat --output ~/.xwin" >&2
    exit 1
fi

# Register the stage-1 as a rustup toolchain if not already there.
if ! rustup toolchain list 2>/dev/null | grep -q "^rustcc-stage1\b"; then
    rustup toolchain link rustcc-stage1 "$RUSTCC_STAGE1"
fi

cd "$SMOKE_DIR"

echo "=> Building msvc_runtime_smoke for x86_64-pc-windows-msvc"
echo "   via fork rustc + -Zbuild-std=core,panic_abort"

cargo +rustcc-stage1 -Zbuild-std=core,panic_abort \
    build --target x86_64-pc-windows-msvc --release 2>&1

EXE="$SMOKE_DIR/target/x86_64-pc-windows-msvc/release/msvc_test.exe"
if [ ! -f "$EXE" ]; then
    echo "FAIL: .exe not produced" >&2
    exit 1
fi

echo
echo "=> .exe produced at $EXE"
file "$EXE"

# Optional: run via Wine if available. Exit code should be 3 (1+2).
#
# Modern macOS Wine (9.0+) ships a single `wine` binary that handles
# 64-bit natively — `wine64` no longer exists separately (Apple
# dropped 32-bit in Catalina). Prefer `wine`, fall back to `wine64`
# on Linux runners that still split them.
WINE_BIN=""
if command -v wine >/dev/null 2>&1; then
    WINE_BIN=wine
elif command -v wine64 >/dev/null 2>&1; then
    WINE_BIN=wine64
fi

if [ -n "$WINE_BIN" ]; then
    echo
    echo "=> Running under $WINE_BIN"
    # Suppress Wine's first-run chatter unless WINEDEBUG is set.
    : "${WINEDEBUG:=-all}"
    export WINEDEBUG
    set +e
    "$WINE_BIN" "$EXE"
    RC=$?
    set -e
    echo "   exit code: $RC (expected 3)"
    if [ "$RC" -eq 3 ]; then
        echo
        echo "PASS"
        exit 0
    fi
    echo
    echo "FAIL: expected exit code 3 from add(1,2), got $RC" >&2
    if [ "$WINE_BIN" = "wine" ] && [ "$RC" -ge 126 ]; then
        echo "hint: macOS may be blocking wine via Gatekeeper. Try:" >&2
        echo "      sudo xattr -dr com.apple.quarantine '/Applications/Wine Stable.app'" >&2
    fi
    exit 1
fi

echo
echo "PASS (compile-only — wine not installed, runtime check skipped)"
exit 0
