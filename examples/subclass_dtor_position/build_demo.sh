#!/usr/bin/env bash
# Build + run the imported-C++-base subclass demo end-to-end.
#
#   1. cargo run --bin gen_bindings   -> target/gen-out/bindings.rs   (libclang)
#   2. fork rustc                     -> target/libsubclass_dtor_position.a (the `class`)
#   3. clang++/clang link + run       -> target/demo
#
# Env overrides: RUSTC (fork rustc), LIBCLANG_PATH.
set -euo pipefail
cd "$(dirname "$0")"

# libclang: explicit on macOS (Homebrew keeps it off the default path);
# Linux clang-sys autodiscovers from libclang-dev.
if [[ -z "${LIBCLANG_PATH:-}" && "$(uname)" == "Darwin" ]]; then
  export LIBCLANG_PATH="/opt/homebrew/opt/llvm/lib"
fi
CXX="${CXX:-clang++}"
CC="${CC:-clang}"
RUSTC="${RUSTC:-$HOME/rust-lang-rust-fork/build/host/stage1/bin/rustc}"

mkdir -p target

echo "==> 1. generate CppBase bindings (libclang)"
cargo run --bin gen_bindings --release

echo "==> 2. compile Rust subclass with the rustcc fork rustc"
"$RUSTC" --edition 2024 --crate-type staticlib --crate-name subclass_dtor_position \
    src/mywidget.rs -o target/libsubclass_dtor_position.a

echo "==> 3. compile C++ base + caller + runner, link, run"
"$CXX" -std=c++17 -c cpp/cppbase.cpp -o target/cppbase.o
"$CXX" -std=c++17 -c caller.cpp       -o target/caller.o
"$CC"   -c runner.c                    -o target/runner.o
"$CXX" target/runner.o target/caller.o target/cppbase.o \
    target/libsubclass_dtor_position.a -o target/demo

echo "==> run"
./target/demo
