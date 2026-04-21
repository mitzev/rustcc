#!/usr/bin/env bash
# Copy the rustcc workspace's rustc_abi_cxx crate into a rust-lang/rust
# clone so it can be depended on as a compiler crate.
#
# Called by build.sh before applying P07. Kept as a script (not a
# patch) because the source corpus is substantial (~2200 lines) and
# mirroring-by-copy is cleaner than maintaining a patch with the
# whole body inline.
#
# Usage:
#   ./vendor_abi_cxx.sh <path-to-rust-lang-rust> <path-to-rustcc-workspace>

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <rust-lang-rust> <rustcc-workspace>" >&2
  exit 2
fi

CLONE_DIR="$1"
WS_ROOT="$2"

SRC_CRATE="$WS_ROOT/crates/rustc_abi_cxx"
DST_CRATE="$CLONE_DIR/compiler/rustc_abi_cxx"

if [[ ! -d "$SRC_CRATE/src" ]]; then
  echo "rustc_abi_cxx source not found at $SRC_CRATE/src" >&2
  exit 1
fi

echo "==> vendoring rustc_abi_cxx into $DST_CRATE"
mkdir -p "$DST_CRATE/src"
cp "$SRC_CRATE"/src/*.rs "$DST_CRATE/src/"

# Emit a minimal Cargo.toml matching rust-lang/rust's compiler-crate
# conventions (edition 2024, version 0.0.0, no dev-deps).
cat > "$DST_CRATE/Cargo.toml" <<'TOML'
[package]
name = "rustc_abi_cxx"
version = "0.0.0"
edition = "2024"

# Vendored from the rustcc workspace
# (https://github.com/mitzev/rustcc — `crates/rustc_abi_cxx/`). This
# copy is consumed by `rustc_ty_utils::layout::cxx_bridge` to route
# `#[repr(cpp)]` ADTs through the Itanium C++ ABI layout algorithm
# instead of Rust's stock C-compat path.
#
# Re-vendor by rerunning `./fork/scripts/vendor_abi_cxx.sh`. Source
# drift between the two trees is expected to be tiny (no runtime
# deps, no feature gates); a full mirror on each build is cheap and
# avoids a submodule dance.

[dependencies]
TOML

echo "==> vendor complete"
