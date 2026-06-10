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

export LIBCLANG_PATH="${LIBCLANG_PATH:-/opt/homebrew/opt/llvm/lib}"
RUSTC="${RUSTC:-$HOME/rust-lang-rust-fork/build/host/stage1/bin/rustc}"

mkdir -p target

echo "==> 1. generate CppBase bindings (libclang)"
cargo run --bin gen_bindings --release

echo "==> 2. compile Rust subclass with the rustcc fork rustc"
"$RUSTC" --edition 2024 --crate-type staticlib --crate-name subclass_dtor_position \
    src/mywidget.rs -o target/libsubclass_dtor_position.a

echo "==> 3. compile C++ base + caller + runner, link, run"
clang++ -std=c++17 -c cpp/cppbase.cpp -o target/cppbase.o
clang++ -std=c++17 -c caller.cpp       -o target/caller.o
clang   -c runner.c                    -o target/runner.o
clang++ target/runner.o target/caller.o target/cppbase.o \
    target/libsubclass_dtor_position.a -o target/demo

echo "==> run"
./target/demo
