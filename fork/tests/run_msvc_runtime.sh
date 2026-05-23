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

# v1.13.0 P09.67b: pre-compile the throwing C++ stub for the
# cxx_throws MSVC smoke test. Can't use a build.rs because
# the rustcc-stage1 toolchain doesn't ship host std (stage-1
# is target-only); a build.rs would fail compiling natively.
# Instead we run clang-cl + llvm-lib here and inject the .lib
# search path / link directive via RUSTFLAGS.
XWIN="$HOME/.xwin"
CPP_OBJ="$SMOKE_DIR/target/cxx_throws_stub/maybe_throws.obj"
CPP_LIB="$SMOKE_DIR/target/cxx_throws_stub/maybe_throws.lib"
mkdir -p "$(dirname "$CPP_OBJ")"

# Locate clang-cl / llvm-lib — Homebrew puts them under
# /opt/homebrew/opt/llvm/bin on Apple Silicon and they're not
# on the default PATH. Allow either PATH-resolution or that
# known-good fallback.
CLANG_CL="$(command -v clang-cl || true)"
LLVM_LIB="$(command -v llvm-lib || true)"
HOMEBREW_LLVM_BIN="/opt/homebrew/opt/llvm/bin"
if [ -z "$CLANG_CL" ] && [ -x "$HOMEBREW_LLVM_BIN/clang-cl" ]; then
    CLANG_CL="$HOMEBREW_LLVM_BIN/clang-cl"
fi
if [ -z "$LLVM_LIB" ] && [ -x "$HOMEBREW_LLVM_BIN/llvm-lib" ]; then
    LLVM_LIB="$HOMEBREW_LLVM_BIN/llvm-lib"
fi

if [ -f "cpp/maybe_throws.cpp" ] && [ -n "$CLANG_CL" ]; then
    echo "=> Pre-compiling cpp/maybe_throws.cpp for x86_64-pc-windows-msvc"
    "$CLANG_CL" /c /EHsc /std:c++17 /MT \
        "/imsvc${XWIN}/crt/include" \
        "/imsvc${XWIN}/sdk/include/ucrt" \
        "/imsvc${XWIN}/sdk/include/um" \
        "/imsvc${XWIN}/sdk/include/shared" \
        "--target=x86_64-pc-windows-msvc" \
        "/Fo:${CPP_OBJ}" cpp/maybe_throws.cpp
    if [ -n "$LLVM_LIB" ]; then
        "$LLVM_LIB" "/OUT:${CPP_LIB}" "$CPP_OBJ"
    else
        lld-link /lib "/OUT:${CPP_LIB}" "$CPP_OBJ"
    fi
    EXTRA_RUSTFLAGS="-Lnative=$(dirname "$CPP_LIB") -lstatic=maybe_throws"
else
    EXTRA_RUSTFLAGS=""
fi

echo "=> Building msvc_runtime_smoke for x86_64-pc-windows-msvc"
echo "   via fork rustc + -Zbuild-std=core,panic_abort"

CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="$EXTRA_RUSTFLAGS" \
cargo +rustcc-stage1 -Zbuild-std=core,panic_abort \
    build --target x86_64-pc-windows-msvc --release 2>&1

BINS_DIR="$SMOKE_DIR/target/x86_64-pc-windows-msvc/release"

# Map of binary name → expected exit code.
declare -a TESTS=(
    "msvc_test:3"            # add(1,2) — basic add via mainCRTStartup
    "msvc_nonvirtual:17"     # class with ctor + method (no virtual)
    "msvc_polymorphic:17"    # class with #[cpp_virtual] method
    "msvc_virtual_dtor:100"  # patch 18: scalar deleting dtor side-effect
    "msvc_override:14"       # patch 17: derived class vtable override
    "msvc_cxx_throws:3"      # P09.67b: catch_switch funclet catches C++ throw
)

ALL_PASS=1
for entry in "${TESTS[@]}"; do
    NAME="${entry%%:*}"
    EXPECTED="${entry##*:}"
    EXE="$BINS_DIR/$NAME.exe"
    if [ ! -f "$EXE" ]; then
        echo "FAIL: $NAME — .exe not produced at $EXE" >&2
        ALL_PASS=0
        continue
    fi
    echo
    echo "=> $NAME"
    file "$EXE" | sed 's/^/   /'
done

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

if [ -z "$WINE_BIN" ]; then
    echo
    echo "PASS (compile-only — wine not installed, runtime checks skipped)"
    exit 0
fi

# Suppress Wine's first-run chatter unless WINEDEBUG is set.
: "${WINEDEBUG:=-all}"
export WINEDEBUG

echo
echo "=> Running each .exe under $WINE_BIN"
for entry in "${TESTS[@]}"; do
    NAME="${entry%%:*}"
    EXPECTED="${entry##*:}"
    EXE="$BINS_DIR/$NAME.exe"
    [ ! -f "$EXE" ] && continue
    set +e
    "$WINE_BIN" "$EXE" > /dev/null 2>&1
    RC=$?
    set -e
    if [ "$RC" -eq "$EXPECTED" ]; then
        echo "   $NAME -> $RC ✓"
    else
        echo "   $NAME -> $RC ✗ (expected $EXPECTED)" >&2
        ALL_PASS=0
        if [ "$WINE_BIN" = "wine" ] && [ "$RC" -ge 126 ]; then
            echo "   hint: macOS may be blocking wine via Gatekeeper. Try:" >&2
            echo "         sudo xattr -dr com.apple.quarantine '/Applications/Wine Stable.app'" >&2
        fi
    fi
done

echo
if [ "$ALL_PASS" -eq 1 ]; then
    echo "PASS"
    exit 0
fi
echo "FAIL" >&2
exit 1
