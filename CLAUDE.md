# rustcc — guidance for Claude Code

This repo is **rustcc**, a fork of `rustc` that teaches the compiler
native C++ and Swift interop (Itanium / MSVC C++ ABI for `#[repr(cpp)]`
and the `class` keyword; swiftcc + value-witness tables for
`#[repr(swift)]`). When generating code here, follow these conventions.

## Two worlds — pick the right one first

This repo deliberately has two kinds of code. **Do not mix them up.**

- **Fork-toolchain code** — `examples/`, `fork/tests/`, and any project
  whose `rust-toolchain.toml` pins `channel = "rustcc"`. This code MAY
  use fork-only syntax: the **`class` keyword**, `extern "C++"`,
  `#[repr(cpp)]`, `#[repr(swift)]`, `#[cpp_virtual]`, `#[constructor]`,
  `#[rustc_cxx_throws]`, `#[swift_value]`.
- **Workspace infrastructure crates** — `crates/rustc_abi_cxx`,
  `crates/cxx_importer`, `crates/rustcc-cli`, `crates/cxx`, `xtask`,
  etc. These are **plain Rust libraries** built on the pinned stock
  nightly. **Never** introduce the `class` keyword or fork-only syntax
  here — they must compile without the fork. Use ordinary `struct` +
  `impl`.

If unsure which world a file is in: check for a `rust-toolchain.toml`
pinning `rustcc`, or a `#![feature(rustc_attrs)]` at the crate root →
fork code. Otherwise treat it as plain Rust.

## Default surface for a C++-interop type: the `class` keyword

When writing a *new Rust type that must be ABI-compatible with C++* on
the fork toolchain, prefer the **`class` keyword** — it's the lowest-
boilerplate surface and mirrors C++ at the declaration site. The crate
root needs:

```rust
#![feature(rustc_attrs)]
#![allow(internal_features)]   // class/ctor/virtual attrs ride rustc_attrs
```

Canonical form:

```rust
pub class Widget {
    x: i32,

    #[constructor]
    pub fn new(x: i32) -> Self { Self { x } }

    #[cpp_virtual]
    pub fn poke(&self) -> i32 { self.x }
}

// Single inheritance — the parser synthesizes a `__base` field:
pub class Derived : Base {
    extra: i32,

    #[constructor]
    pub fn new(x: i32, extra: i32) -> Self {
        Self { __base: Base::new(x), extra }
    }
}
```

Use **`cxx_class!`** (proc macro from `rustcc_macros`) *instead* when
the code must also compile on stock / nightly rustc (graceful
degradation, or a library that should work off-fork). All three
surfaces — `class` keyword, `cxx_class_native!`, `cxx_class!` — produce
identical machine code; see **`fork/THREE-SURFACES.md`** for the
decision table.

Do NOT force `class` onto types that are pure Rust and never cross into
C++ — use normal structs there.

## Memory semantics (so generated code is sound)

A Rust-defined `class` is an ordinary Rust value: owned, borrowed, and
dropped by Rust. `impl Drop` emits the C++ destructor (D0/D1/D2) and it
runs at scope end. It is **never `Copy`**. Rust moves are *bitwise* (no
C++ move-ctor call) — pin the value (`CxxStack!` / `CxxOwned<T>`) only
if its address is observed by C++ or it is self-referential. Imported
C++ classes are held via `CxxOwned` / `CxxStack` / `CxxShared`. Details:
**`docs/ownership_and_safety.md`**.

## Toolchain & build

- **Fork projects**: pin `rust-toolchain.toml` → `channel = "rustcc"`
  (then plain `cargo build` uses the fork), or use `cargo +rustcc`. The
  `rustcc` toolchain is rustup-linked to the fork stage1 at
  `~/rust-lang-rust-fork/build/host/stage1`.
- **Workspace crates**: `cargo test --workspace` on the pinned nightly.
  Some `cxx_importer` tests need libclang and the `libclang` / `build`
  feature (e.g. `cargo test -p cxx_importer --features build`).
- **Fork / `class` probes**: `./fork/tests/run.sh` (defaults `$RUSTC`
  to `~/rust-lang-rust-fork/build/host/stage1/bin/rustc`).
- Commit/push only when asked. If on `main`, branch first.

## Key docs (read before non-trivial work)

| Topic | Doc |
|---|---|
| Which class surface to use | `fork/THREE-SURFACES.md` |
| Rust types exposed to C++ | `docs/repr_cpp.md` |
| Itanium layout / mangling / vtables | `docs/rustc_abi_cxx.md` |
| Move / copy / drop / pinning | `docs/ownership_and_safety.md` |
| C++ → Rust binding generator | `docs/cxx_importer.md` |
| Catching C++ exceptions from Rust | `docs/cxx_throws.md` |
| Swift interop | `docs/swift.md` |
| Authoritative fork spec (per-patch) | `fork/PATCHES.md` |
