#!/usr/bin/env bash
# Build + run the DEEP imported-C++-base subclass demo end-to-end.
#
#   1. cargo run --bin gen_bindings   -> target/gen-out/bindings.rs   (libclang)
#   2. fork rustc                     -> target/libsubclass_cpp_deep.a (the `class`)
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

echo "==> 1. generate Shape/Drawable/Widget bindings (libclang)"
cargo run --bin gen_bindings --release

# The deep chain has C++->C++ inheritance among Shape/Drawable/Widget, so
# the importer emits `impl ::cxx::CxxBase<Parent> for Child` upcast impls
# in the bindings. Those reference the workspace `cxx` runtime crate, so
# build it as an rlib (with the fork rustc, for metadata compatibility)
# and feed it to the subclass compile via `--extern`. (The shallow
# single-class subclass_cpp_base demo has no C++->C++ inheritance and so
# needs no `cxx` dependency.)
echo "==> 2a. build the cxx runtime crate as an rlib (fork rustc)"
"$RUSTC" --edition 2021 --crate-type rlib --crate-name cxx \
    ../../crates/cxx/src/lib.rs -o target/libcxx.rlib

echo "==> 2b. compile Rust subclass with the rustcc fork rustc"
"$RUSTC" --edition 2024 --crate-type staticlib --crate-name subclass_cpp_deep \
    --extern cxx=target/libcxx.rlib \
    src/mywidget.rs -o target/libsubclass_cpp_deep.a

echo "==> 3. compile C++ chain + caller + runner, link, run"
"$CXX" -std=c++17 -c cpp/cppbase.cpp -o target/cppbase.o
"$CXX" -std=c++17 -c caller.cpp       -o target/caller.o
"$CC"   -c runner.c                    -o target/runner.o
"$CXX" target/runner.o target/caller.o target/cppbase.o \
    target/libsubclass_cpp_deep.a -o target/demo

echo "==> run"
./target/demo
