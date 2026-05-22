# rustcc v1.09.0 — DRAFT (Windows MSVC C++ ABI Phase 2)

**Status: in-flight.** The workspace-side surface (mangler, layout,
vtable, cxx_importer routing) is implemented and cross-validated.
The fork rustc patches (B.4) are scoped in
[`fork/MSVC-PATCHES.md`](MSVC-PATCHES.md) and land in a follow-up
sprint after the rust-lang/rust tree is checked out.

## What's new in v1.09.0

The headline: **MSVC C++ ABI support** at the cxx_importer /
binding-emission layer. A downstream crate that targets
`x86_64-pc-windows-msvc` (or `aarch64-pc-windows-msvc`) via
`cxx_importer` gets MSVC-mangled `#[link_name]` attributes and
MSVC-flavored record layouts in its generated bindings.

The fork rustc itself routes through the Itanium codegen path on
Windows targets in this release. The patches that close that
gap (`16-msvc-abi-target-routing.patch` through
`22-msvc-dllexport-dllimport.patch`) are designed in
`fork/MSVC-PATCHES.md` and land in v1.09.1.

### B.1 — MSVC name mangler

New `rustc_abi_cxx::mangle_msvc` module implementing the
Microsoft Visual C++ name mangling scheme. Cross-validated
against `clang -target x86_64-pc-windows-msvc -fms-compatibility`
output for **48 golden symbols** across 7 corpus files:

| Corpus | Cases | Coverage |
|---|---|---|
| `mangle_basic` | 6 | ctor/dtor/method/free-fn |
| `mangle_types` | 19 | 12 builtin types + 7 ptr/ref variants |
| `mangle_nested` | 2 | nested namespaces, name back-refs |
| `mangle_operators` | 5 | `+`/`=`/`[]`/`==`/`<` with class-by-value returns |
| `mangle_inherit` | 6 | virtual methods (`U` access letter), virtual dtors |
| `mangle_substitutions` | 6 | type back-ref table behavior |
| `mangle_templates` | 3 | class-template specializations |
| Dispatcher round-trip | 1 | Itanium↔MSVC ABI routing |

Notable mangler features:
- Full `?Name@Scope@@<info>` form with x64 `E`-extended member functions
- Special-name prefixes: `??0` ctor, `??1` dtor, `??_7` vftable, `??_R0`/`??_R4` RTTI
- Operator codes (`??H` +, `??G` -, `??_2` new, `??_3` delete, etc.)
- Two-table back-reference compression (name table + type table)
  - Name table records each identifier in appearance order
  - Type table keyed by `TypeId`, consulted only at top-level
    parameter positions; nested types bypass it
- Class-by-value return inserts `?A` storage-class prefix
- Virtual access letter `U` (vs non-virtual `Q`)
- Implicit-virtual-dtor inheritance rule for derived classes

### B.2 — MSVC vtable layout

New `rustc_abi_cxx::vtable_msvc` module. Cross-validated for **2
hierarchies / 7 sub-tables** via `clang -fdump-vtable-layouts`.
Implements:
- No offset-to-top header (MSVC tucks it inside the COL)
- Complete Object Locator pointer at conceptual slot −1
- Per-base subobject vftables (vs Itanium primary/secondary)
- Single dtor slot pointing at the scalar deleting destructor
  (`??_GClass@@`)
- Override walking from most-derived to declaring class
- Destructor override matching across class names (`~A` vs `~B`
  recognized as overriding the same vfunction slot)

### B.3 — MSVC record layout

New `rustc_abi_cxx::layout_msvc` module. Cross-validated for **15
record layouts** via `clang -fdump-record-layouts`. Implements:
- No tail-padding reuse across non-POD bases (size 12 for
  `Derived : Base` where Itanium would yield 8)
- Restrictive empty-base optimization
- vptr at offset 0 + inheritance from primary base
- Multi-inheritance with per-base vptr (`VC : VA, VB` gives both
  bases their own vptr)
- `nv_size = round_up(size, align)` — the tail-padded form, not
  the raw byte count
- `#pragma pack(N)` honored: clamps every alignment requirement
  to `min(natural, N)` (1/2/4 packing all validated)

### B.5 — `cxx_importer` MSVC routing

`crates/cxx_importer/src/build.rs` now picks the right
`Target` based on Cargo's target_arch/target_os/target_env. New
constructors:
- `Target::x86_64_pc_windows_msvc()` → MSVC ABI
- `Target::aarch64_pc_windows_msvc()` → MSVC ABI
- `Target::x86_64_pc_windows_gnu()` → Itanium via mingw-w64

When the chosen target has `AbiFlavor::Msvc`, the libclang argv
auto-includes `-target <triple> -fms-compatibility
-fms-extensions` so the parse honors MS C++ extensions and
mangles per MSVC rules.

The `mangle()` / `layout()` / `vtable()` entry points on
`CxxTypeCtx` are now dispatchers — they route to the
appropriate per-ABI implementation based on
`target().abi_flavor`. Existing Itanium-only test consumers
that want to force a specific ABI use `mangle_itanium()` /
`layout_itanium()` / `vtable_itanium()`.

### Smoke test

`fork/tests/msvc_smoke/` builds a polymorphic `Widget` class
from a real C++ header through `cxx_importer` configured for
MSVC, runs the binding generator, and asserts the emitted
Rust source contains the expected `#[link_name]` attributes
(`??0Widget@@QEAA@H@Z` ctor, `??1Widget@@UEAA@XZ` virtual dtor)
plus the safe `pub fn` wrappers for virtual methods. Negative
control confirms no Itanium `_Z` symbols leak.

Runs in ~3 seconds on host. Doesn't need a Windows machine.

## Stats

| Component | LoC | Cross-validated cases |
|---|---|---|
| `Target::AbiFlavor` + 5 new targets | ~80 | — |
| `mangle_msvc.rs` | ~800 | 48 |
| `layout_msvc.rs` | ~450 | 15 |
| `vtable_msvc.rs` | ~350 | 2 hierarchies / 7 sub-tables |
| Dispatchers + cxx_importer wiring | ~150 | end-to-end smoke |
| Tests + corpus | ~1100 | — |
| Design docs (MSVC-PATCHES, MSVC-PLAN log) | ~700 | — |
| **Total this release** | **~3600** | **65 golden cases** |

Workspace test count: 200 (v1.08.0) → 309 (v1.09.0). Existing
Itanium-targeting tests unchanged — the dispatcher routes around
the MSVC code paths when `abi_flavor == Itanium`, which is still
the default for every Linux/macOS target.

## What's NOT in this release

- **B.4 fork rustc patches** (~6000 LoC over 7 patches). The
  workspace MSVC pipeline is complete, but the fork rustc still
  routes through the Itanium codegen path when invoked against
  a Windows MSVC target. See `fork/MSVC-PATCHES.md` for the
  per-patch design + ordering. Estimated 8 weeks focused work
  or 3 weeks agent-accelerated.

- **B.6 Windows CI matrix + release tarballs**. Waits for B.4
  to stabilize so the runner can compile + link a real Windows
  binary. The release matrix in `release.yml` will gain
  `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`
  triples in v1.09.1.

- **Visual Studio integration polish** (PDB debug symbols,
  clang-cl interop, `lld-link` for Windows on macOS dev
  loop). Stretch for v1.09.2.

Until B.4 lands, the recommended path for Windows users
remains WSL2 + the Debian/Ubuntu prebuilt path documented in
`fork/INSTALL.md`.

## Prebuilt binaries

Same 4 host triples as v1.04+. Each release ships two sets of
artifacts:

- `rustcc-<triple>.tar.xz` — fork rustc toolchain
- `rust-analyzer-rustcc-<triple>.tar.xz` — patched rust-analyzer

Stage-1 toolchain binaries in v1.09.0 are **bit-for-bit
identical to v1.08.0** — the fork rustc itself didn't change.
The version bump exists so users can pin
`rust-toolchain.toml` to a release that includes the v1.09.0
workspace-side MSVC surface (the `cxx_importer` routing change
and the mangle/layout/vtable backends).

```bash
TARGET=aarch64-apple-darwin
VERSION=v1.09.0
BASE="https://github.com/mitzev/rustcc/releases/download/$VERSION"

curl -fsSL -o rustcc.tar.xz "$BASE/rustcc-$TARGET.tar.xz"
curl -fsSL -o ra.tar.xz     "$BASE/rust-analyzer-rustcc-$TARGET.tar.xz"
mkdir -p "$HOME/.rustcc/$VERSION"
tar -xJf rustcc.tar.xz -C "$HOME/.rustcc/$VERSION"
tar -xJf ra.tar.xz     -C "$HOME/.rustcc/$VERSION"
rustup toolchain link rustcc "$HOME/.rustcc/$VERSION/stage1"
```

## What's next

`v1.09.1` — fork rustc MSVC patches B.4. Patches 16-22 per
`fork/MSVC-PATCHES.md`. Long pole is patch 20 (SEH personality +
funclet EH lowering); the rest are mechanical.

`v1.09.2` — Windows CI + release tarballs (B.6) + Visual Studio
integration polish.
