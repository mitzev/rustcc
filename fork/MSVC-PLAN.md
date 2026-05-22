# Windows MSVC C++ ABI — v1.09.0 plan

**Target**: `x86_64-pc-windows-msvc` (and `aarch64-pc-windows-msvc` if time permits) as a first-class rustcc target. Today the fork is Itanium-only — every layer of `rustc_abi_cxx`, the fork rustc patches, and `cxx_importer` assumes Itanium ABI semantics.

**Scope**: ~3–4 months focused work, ~10,000 LoC fork delta. Comparable in size to the original Itanium implementation. Best executed across multiple focused sprints with agent acceleration (like RA Phase 2).

## Why MSVC is a second ABI implementation, not a "Windows port"

MSVC diverges from Itanium at every layer rustcc touches:

| Layer | Itanium today | MSVC needs |
|---|---|---|
| Name mangling | `_ZN3Bar3fooEii` | `?foo@Bar@@QEAAHHH@Z` — totally different syntax, calling-convention encoded in name, separate back-ref table |
| vtable layout | Primary sub-table = `[vbase-offsets, OffsetToTop, Rtti, fn-ptrs]` | `vftable` = fn-ptrs only. RTTI lives in COL at index -1. Virtual bases use separate vbtable. |
| Destructor variants | D1 (complete) + D0 (deleting) — two slots | Single dtor slot. Deleting bit passed via hidden int parameter |
| Multiple inheritance | Secondary sub-tables per non-primary base | Each base subobject has its own vftable, laid out per-base not per-class |
| Record layout | Itanium rules (bitfield spillover, EBO permissive) | Stricter EBO, MSVC bitfield rules (no spillover), `#pragma pack` defaults |
| Exception handling | Unwind tables + `__gcc_personality_v0` + `_Unwind_Resume` | Funclet-based SEH + `__CxxFrameHandler3` + completely different IR shape |
| RTTI symbols | `_ZTI<name>`, `_ZTS<name>` | `??_R0?AV<name>@@@8` + R1/R2/R3/R4 class-hierarchy descriptors |
| `operator new` / `delete` | `_Znwm` / `_ZdlPv` | `??2@YAPEAX_K@Z` / `??3@YAXPEAX@Z` |
| `this` calling convention | RDI (x86_64-linux) / X0 (aarch64) | RCX (x86_64-msvc) |
| sret return | RDI implicit pointer (Itanium) | RCX implicit pointer (MSVC) |

## Component breakdown

### B.1 — `rustc_abi_cxx::mangle_msvc` (new module)

Parallel to existing `mangle.rs`. Reuses the `Symbol` enum at the top; dispatches on `Target::abi_flavor`.

Key sub-pieces:
- Calling-convention encoding (`Q`/`A`/`Y`/`R`/`S`) — embedded in the mangled name unlike Itanium
- Access-specifier letter (`QEAA` public mutable, `QEBA` public const, `Q` family for class methods, `A` family for free fns)
- Type encoding table (`H`=int, `I`=uint, `PEAX`=void*, `AEA*`=lvalue ref, etc.)
- Back-reference tables (separate tables for names and types, base-10 indices)
- Template-args block in different position than Itanium

**Property-test corpus**: Apple clang on Mac can emit MSVC-mangled COFF objects via `clang -target x86_64-pc-windows-msvc -c`. Write 50–100 known classes/methods/templates, compare our `mangle_msvc::Symbol → String` to `nm` output. Bit-exact match required.

**LoC**: 1500–2500. **Time**: 2–3 weeks.

### B.2 — `rustc_abi_cxx::vtable_msvc` (new module)

New entry types:
- `MsvcVftable { entries: Vec<FunctionPointer> }` — just function pointers
- `MsvcVbtable { entries: Vec<i32> }` — virtual-base offsets, used when class has virtual bases
- `MsvcCol { offset_to_top, type_descriptor, hierarchy_descriptor, ... }` — Complete Object Locator placed at vftable index -1

Per-base subobject vftables (not the Itanium primary/secondary sub-table structure). Vtordisp slots and adjustor thunks have MSVC-specific mangling.

Single dtor slot — the deleting variant is selected via parameter, not slot.

**Validate against**: `clang -target ...msvc -Xclang -fdump-vtable-layouts`.

**LoC**: 1000–1500. **Time**: 2 weeks.

### B.3 — `rustc_abi_cxx::layout` extensions

Most of the layout code can stay shared, but bitfield engine and EBO rules diverge. Add `Target::abi_flavor: AbiFlavor::{ Itanium, Msvc }` and branch:

- Bitfield packing: MSVC's "each bitfield occupies declared type's full width" — refactor M21.b's engine.
- Empty base optimization: MSVC stricter rules.
- Virtual base placement: MSVC's slightly different alignment.
- Default packing: track `Target::default_pack` and apply when no explicit `#[repr(packed(N))]`.

**LoC**: 600–1000. **Time**: 1–2 weeks.

### B.4 — Fork rustc patches (the bulk)

Every P09.* patch in `fork/patches/` is Itanium-flavored. MSVC parallels needed for:

- **P09.22–P09.32** (core class-keyword + repr(cpp) + vtable emission) — MSVC versions of `inject_vptr_init`, `emit_vtable`, etc.
- **P09.15** (dtor D0/D2 emission) — replace with single-dtor + deleting-bit-parameter convention
- **P09.34** (vtable slot override resolution) — needs MSVC vftable shape
- **P09.50** (aarch64 sret routing) — x86_64-msvc has its own sret rules (RCX = sret pointer, not RDI)

**Plus** SEH exception lowering. This is the single biggest sub-piece. Itanium's two-phase unwinding doesn't exist; MSVC uses funclets with `__CxxFrameHandler3`. LLVM already supports both via different lowering — the fork rustc needs to route `extern "C++"` items through the right path based on target triple.

**LoC**: ~6000+. **Time**: 4–8 weeks. SEH alone is 2–3 weeks.

### B.5 — `cxx_importer` MSVC paths

- libclang handles MSVC headers natively (`-target x86_64-pc-windows-msvc`); just needs target flag plumbing.
- Mangled symbols from libclang for MSVC builds use MSVC mangling — `#[link_name = "..."]` directives come from `mangle_msvc`.
- `cxx_shims.cpp` compiles with `cl.exe` or `clang-cl`. `cc-rs` already supports MSVC; just needs flag plumbing.
- Heap shim functions (`__cxx_*_new_heap_*`) are `extern "C"` and work cross-ABI.

**LoC**: 400–600. **Time**: 1 week.

### B.6 — Examples + CI

- Windows-flavored example: probably `examples/fltk_text_editor` cross-compiled, since FLTK has Windows builds. Alternatively WTL or a small DirectX example.
- CI: GitHub Actions `windows-latest` + `windows-arm64` runners.
- Release matrix in `release.yml` gains `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` triples.

**LoC**: ~500 (mostly yaml). **Time**: 1–2 weeks.

## Phasing

### Phase 1 — mingw-w64 cross-target (2–3 weeks)

Skip MSVC entirely. The `x86_64-pc-windows-gnu` triple uses mingw-w64's gcc/g++, which is **Itanium-flavored mangling** + DWARF unwinding + ELF-like behavior (not exactly Itanium but close enough that our existing implementation likely works after minor target-quirks tweaks).

Sub-deliverables:
1. Cross-toolchain setup in `fork/build.sh` (install `x86_64-w64-mingw32-gcc` on Linux runners)
2. `windows-gnu` quirks row in `Target` (wchar_t=16-bit, long_double=64-bit, dllimport/dllexport handling)
3. Validate `cargo +rustcc build --target x86_64-pc-windows-gnu` against the existing FLTK example
4. CI matrix entry on `windows-latest` with mingw via chocolatey

This is a **cheap insurance** investment: 2–3 weeks unblocks SOMETHING on Windows + flushes out hidden Linux-isms in our layout engine before we go invest months on MSVC.

### Phase 2 — MSVC ABI proper (3–4 months)

Per the breakdown above. Recommended ordering:

1. **B.1 (mangler)** first — most contained, deterministic to test on Mac via Apple clang's MSVC cross-target. ~3 weeks delivers a property-tested mangler with a 50–100 case corpus.
2. **B.2 + B.3 (vtable + layout)** — 3 weeks. Same Mac-side test loop: `clang -fdump-{vtable,record}-layouts` produces a reference.
3. **B.5 (cxx_importer paths)** — 1 week, can run in parallel with B.4.
4. **B.4 (fork rustc patches)** — 6–8 weeks. SEH lowering is the long pole.
5. **B.6 (CI + examples)** — 1–2 weeks, lands after B.4 stabilizes.

### Phase 3 — production polish (~1 month)

Visual Studio integration (LSP behaves; debug symbols round-trip through PDB; clang-cl interop), DirectX/WTL example, MSVC-flavored binding-skip diagnostics in `bindings.skips.json`.

## Total estimate

| Phase | LoC | Time |
|---|---|---|
| Phase 1 (mingw-w64) | ~500 | 2–3 wk |
| Phase 2 (MSVC proper) | ~9,500 | 3–4 mo |
| Phase 3 (polish) | ~1,000 | ~1 mo |
| **Total to v1.09.0** | **~11,000** | **~5 months** |

With agent acceleration (the pattern that compressed RA Phase 2's 26–38 day estimate into ~5 hours of compressed agent-driven work), wall-clock can compress significantly — possibly **2–3 months** if the agents handle the mechanical pieces of B.4's patch series well.

## What's testable on the Mac dev machine

Per the earlier evaluation:

✅ **Mangler correctness** — Apple clang + `nm` cross-compiles to MSVC and emits real MSVC-mangled COFF objects
✅ **Record layout** — clang `-Xclang -fdump-record-layouts -target x86_64-pc-windows-msvc`
✅ **Vtable layout** — clang `-Xclang -fdump-vtable-layouts`
✅ **COFF object emission**
❌ **Runtime execution** — needs Wine (`brew install wine`) or Windows CI runner
❌ **SEH at runtime** — needs Wine or VM

For development, ~80% of the work is testable on Mac without any extra installs. `brew install llvm` adds `llvm-undname` (demangler) and `lld-link` (LLVM's MSVC-style linker) for a richer dev loop.

## Open design questions to settle before Phase 2 starts

1. **Single `Target::abi_flavor` enum vs. parallel type hierarchy?** The mangler can dispatch on a flag (`AbiFlavor::Itanium` / `AbiFlavor::Msvc`). The vtable layout is shape-different enough that a parallel `MsvcVtable` type may be cleaner. Decision impacts the IR-side type surface.

2. **`#[repr(cpp)]` semantics on Windows MSVC targets** — does `#[repr(cpp)]` automatically select Itanium vs MSVC based on target triple? Or do we need a new `#[repr(cpp_msvc)]`? The former is more ergonomic; the latter is more honest.

3. **Cross-platform interop** — can a rustcc library compiled for `x86_64-unknown-linux-gnu` interop with a rustcc library compiled for `x86_64-pc-windows-msvc`? Answer: no, the binary layouts differ. But can a single .rs source compile to both targets? Should — `#[repr(cpp)]` dispatches based on target.

4. **SEH lowering: in-tree LLVM or external pass?** LLVM has native SEH support but it's gated behind `personality(__CxxFrameHandler3)`. The fork rustc needs to set this and emit cleanup blocks instead of unwind blocks. Investigate whether the existing `rustc_codegen_llvm` SEH path is usable or needs forking.

5. **dllimport / dllexport** — Windows shared libraries use explicit imports/exports. How does this interact with `extern "C++"` symbols? Probably requires a `#[cpp_link]` attribute decision.

## First step (when MSVC work starts)

Run the 1-week scoping sprint described in the earlier evaluation:

1. Try `cargo +rustcc build --target x86_64-pc-windows-gnu` against the FLTK example. If it Just Works, Phase 1 is half-done. If it fails, the error pattern tells us cost-of-entry for any Windows target.
2. Stand up `mangle_msvc.rs` MVP. ~3 days delivers mangling for 5–10 known C++ symbols correctly. Validates the substitution-table mechanic is no uglier than the Itanium one.

That gives firm numbers before any irreversible commitment.

## Status log

- 2026-05-09 — plan committed alongside the v1.08.0 docs refresh. v1.09.0 implementation tracked as task #3 in the project task list, blocked-by task #2 (v1.08.0 release).

- 2026-05-22 — **overnight sprint kickoff**. Implemented B.1 + B.2 + B.3 + B.5 across the workspace (no rust-lang/rust tree touched yet — that's the bulk of B.4 and waits for the next sprint that runs against the patched fork build). Concrete deliverables that landed in this sprint:
  - `Target::abi_flavor` (`AbiFlavor::{Itanium, Msvc}`) on every Target, with five new constructors: `x86_64_pc_windows_msvc`, `aarch64_pc_windows_msvc`, `x86_64_pc_windows_gnu` (mingw, Itanium-flavored), and Itanium constructors carry `Itanium` by default. Open question #1 settled in favor of the dispatcher pattern.
  - `mangle_msvc` module — ~800 LoC. Cross-validated against `clang -target x86_64-pc-windows-msvc` for **36 golden symbols** across 5 corpus files: ctor/dtor/method/free-fn, 12 builtin types + 7 pointer/reference variants, nested namespaces with back-refs, 5 operator overloads (incl. `?A`-prefixed class-by-value returns), 6 virtual-method symbols (with `U` access letter + implicit-virtual dtor inheritance). Bit-exact match.
  - `layout_msvc` module — ~400 LoC. Cross-validated against `clang -fdump-record-layouts` for **10 record layouts**: scalar fields, empty class, `alignas`, mixed-width padding, single inheritance with no tail-padding reuse, polymorphic class with vptr, derived-from-polymorphic, empty-base optimization.
  - `vtable_msvc` module — ~350 LoC. Cross-validated against `clang -fdump-vtable-layouts` for **2 hierarchies**: standalone A with f/g/dtor, B inheriting A with override of g + dtor + new virtual h. Implements MSVC's no-offset-to-top + COL-at-slot-minus-one + single-dtor + scalar-deleting-dtor variant convention.
  - `mangle`/`layout`/`vtable` dispatchers on `CxxTypeCtx` route on `target().abi_flavor`. Itanium entry points renamed `_itanium` and still exposed for tests + cross-ABI consumers.
  - `cxx_importer::Build::compile` auto-injects `-target <triple> -fms-compatibility -fms-extensions` into libclang argv when the chosen Target carries `AbiFlavor::Msvc`. Cargo target_env=`msvc` vs `gnu` routes correctly.
  - End-to-end smoke test: `cxx_importer/tests/bindings_msvc.rs` builds a hand-rolled `Foo { Foo(int); ~Foo(); int get() const; }` graph against `x86_64-pc-windows-msvc`, runs `generate_rust_bindings`, and asserts the emitted `#[link_name]`s are MSVC-mangled (`??0Foo@@QEAA@H@Z`, `??1Foo@@QEAA@XZ`, `?get@Foo@@QEBAHXZ`). Negative control test confirms Linux target still produces Itanium symbols.
  - New script `fork/tests/refresh-msvc-corpus.sh` regenerates all goldens from Apple clang's cross-target output.

  Total cross-validated golden cases: **48** (36 mangle + 10 layout + 2 vtable hierarchies expanded into 7 sub-tables). Workspace test count grew from ~200 to ~225 with 25 new MSVC-targeting tests; all green; existing Itanium tests unchanged.

  **Remaining for v1.09.0**: B.4 (the bulk — fork rustc patches: `inject_vptr_init`, SEH lowering, `__CxxFrameHandler3`, x86_64-msvc sret routing via RCX), B.6 (Windows CI matrix, release tarballs for windows-msvc triples), plus the design doc for the 16-msvc-abi.patch series that lands against the rust-lang/rust tree. None of these block v1.09.0 *announcement* of MSVC ABI support in cxx_importer-targeting consumers — they block actual rustc codegen for MSVC targets.
