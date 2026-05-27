# rustcc v1.13.1 — C++ ABI layout + copy-ctor bindings

**A bindings-generator + ABI-library release.** v1.13.1 closes
three verified gaps in the Rust→C++ import path. All changes are
in the host-side workspace crates (`rustc_abi_cxx`,
`cxx_importer`); **the fork rustc patches are unchanged from
v1.13.0**, so the prebuilt toolchain tarballs are identical to
v1.13.0's — if you already run the v1.13.0 fork rustc, you only
need to update the `rustcc`/`cxx_importer` crates.

## What ships

### A. Itanium bit-field layout — validated against clang

The Itanium `place_bitfield` allocator (originally landed as
M21.b but never golden-tested) is now validated against real
clang. The golden harness gained bit-field support:
`FieldDump.bits: Option<(bit_offset, bit_width)>`, a clang
record-layout parser that reads the `byte:startbit-endbit`
column, and a `bitfield` corpus fixture.

Confirmed matching clang for `unsigned int a:4, b:20, c:8;
char tail;` → `a@0:0-3, b@0:4-23, c@3:0-7, tail@4, sizeof=8`.

### B. Itanium `__attribute__((packed))`

The Itanium layout engine now honors packing: fields drop to
1-byte alignment (no inter-field padding) and the record's
field-derived alignment caps at 1. A per-field `alignas(N)`
still wins. New `CxxTypeCtx::set_packed` / `is_packed` (mirrors
the MSVC `pragma_pack` side-table) + a clang-validated `packed`
corpus fixture (`char a; int b; char c;` → sizeof 6, align 1).

### C. Copy constructor → `impl Clone`

A user-declared copy constructor `T(const T&)` now emits an
`impl ::core::clone::Clone for T` (previously a hard
`UnsupportedMethod` skip). The clone placement-copies `self`
into a fresh slot via the C++ copy ctor and returns by value —
the same construct-into-`MaybeUninit` pattern the regular ctor
uses, rendered as a postamble trait impl next to `Drop`.

Correct for trivially-relocatable types (the common case);
address-sensitive types still need pinned storage. Move ctor +
copy/move assignment remain skipped (narrowed error message) and
land next.

## Corrections to the v1.13.0 gap analysis

While scoping this release we re-verified the "remaining C++
integration" list against the code and found several items had
already shipped and were mis-listed as gaps:

- **Multiple inheritance + virtual (diamond) inheritance** —
  layout, secondary vtables, `this`-adjusting thunks, and
  vbase-offset vtable slots are implemented and clang-validated.
  The only remaining vbase piece is VTT / construction vtables
  (construction/destruction ordering when rustcc itself builds a
  virtual-base object).
- **Empty Base Optimization** — implemented for both Itanium and
  MSVC.
- **MSVC bit-fields + `#pragma pack`** — already implemented.
- **Cross-compilation (host ≠ target)** — implemented and
  Wine-validated. The fork rustc derives the C++ ABI from the
  session `--target` triple, not the build host
  (`rustc_ty_utils/layout/cxx_bridge.rs` and the Itanium
  mangler call `CxxTarget::from_rustc_triple(triple,
  pointer_width)`; `Target::host()` is only a tooling/test
  fallback). `cxx_importer::Build` likewise reads
  `CARGO_CFG_TARGET_*` (or an explicit `.target()`) and injects
  `-target <triple>` (+ MSVC compat flags) into libclang with a
  matching Rust-side ABI ctx. The MSVC-under-Wine smoke suite
  cross-compiles macOS/Linux host → `x86_64-pc-windows-msvc`
  end-to-end, and the release pipeline itself cross-builds the
  Windows toolchains. All 7 first-class triples are wired
  (Linux/macOS × x86_64/aarch64, Windows MSVC ×2, Windows GNU);
  unlisted triples fall through to a generic-Itanium default
  (correct pointer width, Linux-style long-double/wchar quirks).

The genuinely-remaining gaps after v1.13.1: move ctor +
copy/move assignment, templates beyond explicit instantiation
(NTTP / template-template / uninstantiated generics), member
pointers (partial), covariant-return thunks, VTT/construction
vtables, exotic-triple ABI quirks (non-first-class targets use
generic-Itanium defaults), and GCC-backend cxx_throws codegen.

## Test status

Full workspace green: 233 unit tests across all crates
(`rustc_abi_cxx` layout/vtable/mangle corpora incl. the new
`bitfield` + `packed` goldens; `cxx_importer` incl. the new
`copy_ctor_emits_clone_impl`); 104 libclang import tests pass.

## Toolchain

No fork patch changes. The `fork/patches/` set is identical to
v1.13.0 (patches 01–36). Prebuilt toolchain tarballs for
v1.13.1 are byte-identical to v1.13.0; users on the v1.13.0
toolchain need only update the workspace crates.
