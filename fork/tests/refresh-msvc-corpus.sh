#!/usr/bin/env bash
# Regenerate the MSVC mangle corpus goldens from clang's cross-target output.
#
# Inputs:  crates/rustc_abi_cxx/tests/corpus_msvc/*.cpp
# Outputs: crates/rustc_abi_cxx/tests/corpus_msvc/*.mangle.golden
#
# Requires: a clang that supports `-target x86_64-pc-windows-msvc`. Apple
# clang (`/usr/bin/clang`) is sufficient on macOS; an LLVM-built clang
# works on Linux. The output is target-only (no link), so a Windows
# runtime is not needed.
#
# Usage:
#   ./fork/tests/refresh-msvc-corpus.sh             # refresh all
#   ./fork/tests/refresh-msvc-corpus.sh mangle_basic # one corpus file

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CORPUS_DIR="$REPO_ROOT/crates/rustc_abi_cxx/tests/corpus_msvc"
TARGET="x86_64-pc-windows-msvc"

if ! command -v clang >/dev/null 2>&1; then
    echo "error: clang not found on PATH" >&2
    exit 1
fi

# Sanity check: confirm the cross-target works at all.
echo "void _probe() {}" > /tmp/msvc_probe.cpp
if ! clang -target "$TARGET" -fms-compatibility -c /tmp/msvc_probe.cpp \
       -o /tmp/msvc_probe.o 2>/dev/null; then
    echo "error: clang doesn't support cross-compiling to $TARGET" >&2
    echo "On macOS: Apple clang 14+ has it; older needs 'brew install llvm'." >&2
    exit 1
fi
rm -f /tmp/msvc_probe.cpp /tmp/msvc_probe.o

cd "$CORPUS_DIR"

declare -a sources
if [ $# -gt 0 ]; then
    for arg in "$@"; do
        sources+=("$arg.cpp")
    done
else
    while IFS= read -r f; do
        sources+=("$f")
    done < <(ls -1 *.cpp 2>/dev/null)
fi

for src in "${sources[@]}"; do
    if [ ! -f "$src" ]; then
        echo "skip: $src (not found)" >&2
        continue
    fi
    base="${src%.cpp}"
    golden="$base.mangle.golden"
    obj="/tmp/${base}.msvc.o"

    echo "=> $src"
    clang -target "$TARGET" -fms-compatibility -c "$src" -o "$obj" 2>&1
    # Extract text-section symbols (defined functions). nm formats
    # vary between macOS and Linux; we match `T` (defined text) lines.
    symbols=$(nm "$obj" 2>/dev/null | awk '$2 == "T" { print $3 }' | sort)

    {
        echo "# Generated from clang -target $TARGET output."
        echo "# Do not hand-edit; regenerate via fork/tests/refresh-msvc-corpus.sh."
        echo "target $TARGET"
        while IFS= read -r sym; do
            [ -n "$sym" ] && echo "symbol $sym"
        done <<< "$symbols"
    } > "$golden"

    echo "   wrote $golden ($(wc -l < "$golden") lines)"
    rm -f "$obj"
done
