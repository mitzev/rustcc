# rustcc rustc fork

This directory holds the specification and patch set for the rustc
fork that would give rustcc's C++ interop its **native** entry points
— the paths that today we route around with the forwarders generator
(see `cxx_importer::rust_forwarders`).

## What "the fork" does

The fork is a modified `rustc` that, compared to upstream, additionally:

1. **Accepts `#[repr(cpp)]`** as a valid representation hint on
   structs, unions, and enums. Upstream rejects unknown reprs at
   attribute-validation time.
2. **Delegates layout** of `#[repr(cpp)]` items to
   `rustc_abi_cxx::CxxTypeCtx::layout` instead of running its own
   Rust layout algorithm. This is what makes Rust structs and C++
   classes sharable.
3. **Routes mangling** of `extern "C++"` items through
   `rustc_abi_cxx::mangle` so Rust emits Itanium-compliant symbols
   directly (no forwarder thunks needed).
4. **Recognizes the `CXX` calling convention** on `extern "C++"`
   fn signatures, handling sret returns, `this`-pointer prepending,
   and record-by-value param ABI to match Itanium rules.
5. **Emits dtor glue** that plays nicely with C++'s complete/base-
   object destructor split (`C1`/`C2` and `D1`/`D2`).

## What this directory contains

- [`PATCHES.md`](PATCHES.md) — the canonical specification. Lists
  every upstream file that needs changes, the shape of the change,
  and the rationale. This is the authoritative reference.
- [`patches/`](patches/) — unified-diff patches ready for `git apply`,
  one per logical concern. Kept as numbered sequence so `ls
  patches/*.patch | xargs -n1 git apply` in order Just Works.
- [`build.sh`](build.sh) — recipe for cloning rust-lang/rust at the
  pinned nightly, applying the patches, and running `./x.py build
  --stage 1` to get a working forked compiler.
- [`VERIFY.md`](VERIFY.md) — per-patch test recipe. Each patch has
  a minimal repro program that proves the upstream behavior (breaks)
  vs. the forked behavior (works).

## Relationship to the forwarders path

The forwarders generator under `cxx_importer::rust_forwarders` is a
**byte-for-byte equivalent** of what the fork will emit for the
happy-path subset (POD records, scalar/pointer/record-by-value
parameters, simple Drop types). Users can ship interop today via
forwarders and switch to the fork later without touching their
source code.

What the fork does that forwarders can't:

- Bridge `extern "C++"` function imports (no forwarder layer needed).
- Handle arbitrary Drop types for pass-by-value (fork can emit the
  exact Itanium destructor convention per target; forwarders rely
  on `ptr::read` + caller-destroys which falls over for `Drop`
  types).
- Virtual method dispatch (vtable codegen is a fork-only feature).
- Template instantiation driven from Rust generics (v1.5+).

## Status

**Shipped.** v1.0 went out 2026-04-21 with the original 11-patch
P09.22–P09.32 series applied end-to-end against the pinned
nightly. Eight tagged point releases since (v1.02 → v1.07) bring
the rustc-fork series to 15 patches (P09.50 / `15-cpp-abi-force-sret-record-ret.patch`
is the most recent — v1.03.0). Stage-1 tarballs for the four
prebuilt triples (aarch64-darwin, x86_64-darwin, aarch64-linux,
x86_64-linux) are published on the
[GitHub Releases page](https://github.com/mitzev/rustcc/releases);
see [`INSTALL.md`](INSTALL.md) for the fast-path install.

The fork rustc binaries haven't changed since v1.04.0 — every
post-v1.04 milestone lives in workspace crates (`crates/cxx_importer`,
`crates/rustc_abi_cxx`, `crates/cxx`, `crates/rustcc-cli`),
`tools/vscode-rustcc/`, or as patches against rust-analyzer
under [`ra-patches/`](ra-patches/) (12-patch Phase 2 series for
`class` IDE parity, applied at PINNED_COMMIT `45b868b19`).

For a fresh source build (~30–90 min on a fast machine), use
[`build.sh`](build.sh).
