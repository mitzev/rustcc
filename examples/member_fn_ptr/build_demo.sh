#!/usr/bin/env bash
# Member-fn-pointer e2e: bindings (libclang) -> Rust staticlib (fork
# rustc) -> link with the C++ side -> run.
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
echo "==> 1. generate bindings"
cargo run --bin gen_bindings --release
echo "==> 2. build cxx rlib + Rust staticlib (fork rustc)"
# rustcc-fork feature: emits the CxxMemberFnPtr diagnostic item the
# fork's ABI overlay keys its triviality exemption on.
"$RUSTC" --edition 2021 --crate-type rlib --crate-name cxx \
    --cfg 'feature="rustcc-fork"' \
    ../../crates/cxx/src/lib.rs -o target/libcxx.rlib
"$RUSTC" --edition 2024 --crate-type staticlib --crate-name member_fn_ptr \
    --extern cxx=target/libcxx.rlib src/roundtrip.rs -o target/libmember_fn_ptr.a
echo "==> 3. compile C++, link, run"
"$CXX" -std=c++17 -c cpp/receiver.cpp -o target/receiver.o
"$CXX" -std=c++17 -c caller.cpp -o target/caller.o
"$CXX" target/caller.o target/receiver.o target/libmember_fn_ptr.a -o target/demo
./target/demo
