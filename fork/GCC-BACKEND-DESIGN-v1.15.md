# rustc_codegen_gcc backend for rustcc — design (v1.15 phase)

Goal: the fork's C++-interop features work when Rust itself is
compiled by **GCC** (`-Zcodegen-backend=gcc`, libgccjit) — a pure-GCC
pipeline end to end (cg_gcc Rust object code + g++ C++ side), opening
GCC-only targets (AVR-class embedded, exotic ISAs, distros that build
everything with GCC).

Prereq landed in v1.14.x: g++/libstdc++ is already first-class for the
*C++ side* (toolchain-aware harnesses + `workspace-test-gcc` /
`subclass-e2e-gcc` CI legs). This phase moves the *Rust side* onto GCC.

## What is already backend-agnostic (free)

| Piece | Where | Why free |
|---|---|---|
| Layout (`#[repr(cpp)]`, vptr slot, base-at-0) | `rustc_ty_utils::layout::cxx_bridge` | pre-codegen |
| Itanium mangling + `#[link_name]` precedence | `rustc_symbol_mangling` | pre-codegen |
| Slot model / chain walk / override checks | `itanium.rs`, `check_attr` | pre-codegen |
| ctor-in-place MIR passes (v1.14) | `rustc_mir_transform` | MIR-level |
| Collector drop-glue rooting | `rustc_monomorphize::collector` | shared |
| CGU never-internalize for vtable drop glue | `rustc_monomorphize::partitioning` | shared |
| Member-fn-ptr ABI pass-by-value exemption | `rustc_target` callconv | shared |
| `CxxThrowsWrap` MIR rewrite (Result plumbing) | `rustc_mir_transform` | MIR-level |

The cg_gcc build is ALREADY green with the fork patches: the SSA trait
hooks have default/stub impls (`P09.62-gcc` stub `cxx_catch_landing_pad`
in `rustc_codegen_gcc/src/builder.rs:1619`, `P09.64` stub
`cxx_throws_catch_fn` in `context.rs:479`); `#[rustc_cxx_throws]` calls
reaching GCC codegen panic with a clear message, everything else
silently lacks vtables.

## What must be ported (LLVM-local today)

1. **Class metadata emission** — `rustc_codegen_llvm/src/cxx_vtable.rs`
   (848 lines incl. MSVC; the Itanium core is ~450):
   `maybe_emit_class_metadata` (driven from `mono_item.rs:85` when an
   instance of a polymorphic class codegens) → `_ZTV` (vtable array:
   offset-to-top, `_ZTI`, fn pointers incl. dtor pair + imported
   slots), `_ZTS` (type string), `_ZTI` (`__si_class_type_info` chain,
   external base `_ZTI` for imported bases).
2. **Ctor vptr init** — `maybe_emit_cxx_ctor_vptr_init` (SSA Builder
   trait method, called from `mir/block.rs:621` + `mir/mod.rs:308`):
   store the vtable address-point (`_ZTV + 2*ptr`) to offset 0 of the
   return place at ctor return.
3. **cxx_throws landing pads** — `cxx_catch_landing_pad` (catch-all +
   typed clauses + `llvm.eh.typeid.for` small-index translation) and
   the `cxx_throws_catch_fn`/typed helper declarations.

## gccjit mappings

| LLVM concept | gccjit equivalent | Notes |
|---|---|---|
| global const array of ptrs (`_ZTV`) | `context.new_array_type` + `new_global` + `global_set_initializer_rvalue` (array constructor of `get_address` rvalues) | shape: `{ isize, ptr, ptr… }` — use a struct or byte-equivalent array of pointers with the offset-to-top cast |
| `weak_odr` / vague linkage | **gap** — gccjit globals are only exported/internal/imported | mitigation: `context.add_top_level_asm(".weak <sym>")` per emitted metadata symbol; dedup per-CGU via the existing emission cache |
| address-point GEP const | `get_address` + pointer arithmetic rvalue | precompute `_ZTV + 16` (2 slots) as init-time const |
| `landingpad {ptr,i32} catch …` | `block.add_try_catch(try_block, catch_block)` + `__builtin_eh_pointer(0)` | gccjit `master` feature; personality via `set_personality_function` |
| `llvm.eh.typeid.for` typed selector | **no equivalent** | move type matching into the runtime helper: catch-all lands, helper does `__cxa_begin_catch`-side typeinfo comparison against the `#[rustc_cxx_throws_typeinfos]` list and returns the same small 1-based index. Personality already ran a catch-all match, so this is allowed (we re-implement the clause walk in the helper using `std::type_info::operator==`, which on Itanium is pointer-or-string compare). `cxx::native_invoke` grows `__rustcc_cxx_catch_typed_dyn(exc, *const *const c_void, len) -> CxxRawError` |
| MSVC funclets | n/a | GCC backend is Itanium-only; keep the LLVM-only gate for MSVC |

## Status (2026-06-11)

M1–M4 are **done** (patches 0047/0048): the pure-GCC pipeline
(`-Zcodegen-backend=gcc` Rust + g++ C++) passes the smoke probes
(base dispatch, placement-new + virtual call, subclass override
through an opaque base pointer) AND the full importer-driven
`subclass_cpp_base` demo — pure + concrete overrides, a new Rust
virtual, and the virtual destructor through the base pointer
(deleting-dtor thunk → drop glue → `_ZdlPv`). Verified in the amd64
container (CI libgccjit via `gcc.download-ci-gcc`); LLVM backend
re-verified on the same probes from the same stage1. CI: the
`subclass-e2e-cg-gcc` dispatch leg replicates it on a runner.
Discovered + fixed along the way: gccjit declare-then-define
ordering, `void*`/struct-stride constant arithmetic, and the
shared `transmute_scalar` strict type pre-check (see patch 0048).
Remaining: M5 (cxx_throws).

## Milestones

- **M1 — environment (~1 day)**: docker (aarch64-unknown-linux-gnu,
  Ubuntu 24.04, `libgccjit-14-dev`); in-tree build of the fork with
  `rust.codegen-backends = ["llvm","gcc"]` + `llvm.download-ci-llvm`;
  smoke: a `class` crate compiles under `-Zcodegen-backend=gcc`
  without ICE (vtable absent = expected at this stage).
- **M2 — class metadata (~3-4 days)**: `cxx_vtable_gcc.rs` in cg_gcc
  mirroring the Itanium half of cxx_vtable.rs; weak linkage via
  top-level asm; `nm`-level parity check against the LLVM backend's
  output for the probe class (same symbols, same section-ish shape).
- **M3 — vptr init (~1 day)**: builder impl storing the address-point;
  probe: C++ placement-new + virtual call lands in Rust (the
  bare-metal probe's `demo` shape, hosted).
- **M4 — pure-GCC e2e (~1-2 days)**: `subclass_cpp_base` +
  `subclass_dtor_position` demos compiled `-Zcodegen-backend=gcc`,
  C++ side g++, run in the container. Container CI recipe
  (dispatch-gated job).
- **M5 — cxx_throws (~1 week, riskiest)**: catch-all via
  `add_try_catch` + `__builtin_eh_pointer`; typed catches via the
  runtime-helper matching above; `cxx_throws_native` demo green under
  cg_gcc. Fallback if gccjit `master` APIs are unavailable in distro
  libgccjit: keep the panicking stub + document (catch-all requires
  building gccjit from GCC master).

## Risks

- **R1 vague linkage**: top-level-asm `.weak` is assembler-level —
  verify it composes with gccjit's own symbol emission order (it does
  for gcc: `.weak` may precede or follow the definition).
- **R2 gccjit feature level**: `add_try_catch` needs the gccjit crate
  `master` feature + a libgccjit built from recent GCC; Ubuntu 24.04's
  libgccjit-14 may lack the entrypoints → M5 may need a self-built
  libgccjit (container layer) or stays stubbed.
- **R3 cg_gcc aarch64 maturity**: the container is arm64; cg_gcc's
  best-supported target is x86_64-linux. If aarch64 misbehaves, switch
  the container to `--platform linux/amd64` (Rosetta emulation —
  slower but exercised upstream).
- **R4 dtor thunks**: the deleting-dtor (`D0`) thunk and `_ZThn`
  non-virtual thunks are emitted as LLVM functions today — port uses
  plain gccjit functions; no funclet complexity on Itanium.

## Out of scope (this phase)

- MSVC ABI on cg_gcc (GCC doesn't target the MSVC C++ ABI).
- Swift interop on cg_gcc (swiftcc calling convention support in GCC
  is absent; LLVM-only feature).
- rustc_codegen_cranelift (no C++ ABI ambitions there).
