# rustcc — session restart (v1 + P09.37 RISC-V + P09.38 Pico + P09.39 ItemKind::Class)

Last updated: **2026-04-24**, Opus 4.7 (1M ctx). **rustcc v1
milestone complete. P09.37 adds RISC-V ESP32 / bare-metal rv32;
P09.38 extends ARM Cortex-M coverage to ARMv6-M (Raspberry Pi
Pico / RP2040); P09.39 ships 1.01 #7 — `ItemKind::Class` AST
variant, enabler for the 1.02 rust-analyzer fork.**

Workspace baseline: `cargo test --workspace` → **235 passed, 0 failed**.

## v1 scope

Eleven patches (P09.22 → P09.32) shipped across one long
user-directed session. The fork now supports:

- Full C++ interop: `#[repr(cpp)]` layout, extern "C++",
  ctors, dtors, virtuals + vtables, single inheritance,
  `dynamic_cast`, operator overloading, cross-crate.
- Full Swift interop: extern "Swift" calling convention,
  `#[repr(swift)]` values and classes, VWT-direct path,
  `swift_value!` macro for auto Drop/Clone.
- Three class-writing surfaces: `cxx_class!` (stable rustc),
  `cxx_class_native!` (fork-only macro), parser-level `class`
  keyword (fork-only sugar).

See `fork/getting-started.html` — rewritten as a GitHub
project intro with v1 feature matrix, v2 roadmap, and a
five-example gallery.

## Latest addition (P09.39, 2026-04-24)

**1.01 #7 shipped: `ItemKind::Class` AST variant.** The fork-only
`class` keyword is now a first-class AST node that survives
through name resolution and splits into `hir::ItemKind::Struct`
+ `hir::ItemKind::Impl` at AST → HIR lowering. Enables the 1.02
rust-analyzer fork to mirror rustc's AST instead of re-doing the
P09.30 parse-time desugar.

**Design (Option B1)**: parser emits `ItemKind::Class(Box<Class
{ ident, generics, fields, methods, impl_id, self_ty }>)`.
`AstOwner` grows a `ClassImpl` variant so both halves get
independent `lower_node` dispatch. `lower_class_impl_half`
produces the inherent impl using `lower_ty(class.self_ty)` for
the self type. Name resolution reuses `resolve_adt` (class's
generics are shared between fields and methods, so one rib
suffices). `def_collector` creates two DefIds per class (struct
at `item.id`, impl at `class.impl_id`).

**Patch**: `fork/patches/10-itemkind-class.patch` (+323 / −51
across 14 files; series is now 10 patches).

**Scope calibration**: paper estimate 500–1000 LOC, actual ~320
LOC. Still genuinely on the larger end of 1.01 work — the
non-trivial part was the two-DefId plumbing through def_collector,
build_reduced_graph, effective_visibilities, and ast_lowering.
"Probe before estimating" again useful.

**Validation**:
- stage-1 rustc builds clean in ~2 min (after the NodeId-assign
  bugfix — see below).
- `cargo test --workspace` → 235/0 (no regression from v1).
- `/tmp/p09-39-itemkind-class/`: basic class `Widget::sum() == 7`
  and single-inheritance `Derived::sum() == 15` both pass.

**Bugs hit during development (memory-worthy)**:
- `impl_id: DUMMY_NODE_ID` from parser must be walked through
  `visit_visitable!` so `rustc_expand::expand`'s `visit_id`
  replaces it with a real NodeId; forgetting to walk it panics
  in `ast_lowering::index_crate` with "must have def_id".
- Synthetic impl DefIds need explicit `feed_visibility` calls in
  `build_reduced_graph_for_item`; otherwise `tcx.visibility(impl_did)`
  bugs out with "not supported for this key".
- `rustc_passes::lang_items::visit_assoc_item` matches on the
  parent item's kind to determine `MethodKind`; needs a Class
  arm (→ `MethodKind::Inherent`).

## Prior addition (P09.38, 2026-04-22, documentation-only)

**Raspberry Pi Pico / ARMv6-M coverage.** Probe on
`thumbv6m-none-eabi` (RP2040 Cortex-M0+) yields identical
Itanium output to P09.36's `thumbv7em-none-eabihf`. Zero code
changes needed — ARM's call-conv in upstream rustc is shared
across ARMv6-M / v7-M / v8-M, and the fork's C++ ABI paths
don't touch target-specific instruction selection.

**Pico coverage matrix**:

| Board | Target | Status |
|---|---|---|
| Pico / Pico W (RP2040, M0+) | `thumbv6m-none-eabi` | P09.38 (new) |
| Pico 2 (RP2350, M33) | `thumbv8m.main-none-eabihf` | P09.36 |
| Pico 2 RISC-V (RP2350, Hazard3) | `riscv32imac-unknown-none-elf` | P09.37 |

**Validation**: `/tmp/p09-41-rp2040-pico/` — ctor
`void _ZN6WidgetC1Ei(ptr sret, i32)`, vtable `{ i32, ptr, ptr }`
with 4-byte slots, address-point offset 8. All Itanium-correct.

**Patch**: none (documentation-only). No zip regeneration needed.

## Prior addition (P09.37, 2026-04-22)

**RISC-V Itanium C++ ABI overlay.** Polymorphic `#[repr(cpp)]`
classes now compile on rv32 / rv64. Previously ICEd at
`rustc_target/src/callconv/riscv.rs:185` because upstream's
fp-conv probe doesn't expect vptr-style leading padding.

- `compute_cxx_abi_info` overlay for rv32/rv64, mirroring the
  x86_64 / aarch64 P09.6 / P09.11 overlays. Force-indirects any
  type that `is_cxx_non_trivial_for_calls`.
- Defensive `panic!` → `return None` in `should_use_fp_conv` so
  layouts with leading padding gracefully fall back to memory
  mode (matches the RISC-V psABI's own treatment of aggregates-
  with-vtables).

**Coverage**: all rv32 and rv64 ELF targets — notably
`riscv32imc-unknown-none-elf` (ESP32-C3),
`riscv32imac-unknown-none-elf` (ESP32-C6 / H2),
`riscv32imafc-unknown-none-elf` (ESP32-P4), and the
`riscv32im*c-esp-espidf` std targets.

**Not covered**: Xtensa ESP32 (S3, original ESP32) — no upstream
rustc Xtensa target.

**Validation**: `/tmp/p09-40-riscv32-esp32c3/` polymorphic Widget
builds; IR shows `void _ZN6WidgetC1Ei(ptr sret, i32)` matching
Clang's Itanium output. Vtable shape `{ i32, ptr, ptr }`, 4-byte
slots, address-point offset 8. Workspace 235/0.
`examples/bare_metal_arm` (P09.36) still builds clean — ARM
Cortex-M path unaffected.

**Patch**: `fork/patches/09-riscv-cxx-overlay.patch` (the 8-patch
series is now a 9-patch series).

## v1 session additions (P09.31 + P09.32)

**P09.31** — Coverage bundle (#1, #5, #6, #7, #9):

- **#5 empty-class size**: vptr-only polymorphic class is now
  8 bytes (was 16). Fix in `cxx_bridge::correct_layout`.
- **#6 class field attrs**: `#[doc]` / `#[cfg]` / etc. now
  accepted on class-body fields. New `skip_over_outer_attributes`
  token-scan helper in the parser.
- **#7 generics on class header**: `pub class Pair<A, B> { … }`
  emits the self-type with generic args. `is_cpp_abi` and
  NO_MANGLE skip C++ mangling for generic methods.
- **#1 non-trivial virtual signatures**: validated via probe,
  no code changes.
- **#9 multi-field Swift class bindings**: POD extras already
  work via `swift_value!`'s memcpy+overwrite; validated via
  probe. Non-POD extras remain v2.

**P09.32** — Single inheritance + `dynamic_cast` (#2, #4):

- New `#[rustc_cxx_base]` attribute on fields, auto-inserted
  by `class D : B { … }` parser sugar.
- Layout: derived classes skip the P09.25 vptr shift (base
  subobject already owns offset 0).
- Itanium helpers: `polymorphic_base_of_class`,
  `virtuals_on_chain` walk the inheritance chain.
- Vtable emission: chain-walk combines base + derived virtuals.
- Typeinfo emission: root uses `__class_type_info` (2 slots);
  derived uses `__si_class_type_info` (3 slots including
  base's `_ZTI`) — makes `dynamic_cast` work via libc++abi's
  runtime chain walk.
- Ctor vptr write at every return terminator. Required because
  user's `Self { __base: Base::new(…), … }` writes base's
  vptr AFTER the P09.25 entry-point injection.

Scope: single inheritance, derived adds new virtuals but
doesn't override. Override + multi-inheritance are v2.

Validation: `/tmp/p09-35-inherit/` and `/tmp/p09-36-dyncast/`
both pass end-to-end.

## Release-track backlog (1.01 / 1.02 / 1.1)

See `project_queue_state.md` memory. Post-P09.39 status:

**1.01 — polish + small extensions (remaining after #7 shipped)**:
1. Const generics on class header (small-medium).
2. Multi-field Swift bindings with non-POD extras (small-medium).
3. `rustc_cxx_*` attr-plumbing unification (small).
4. `/tmp/p09-*` probes → in-tree test crate (small).
5. Three-surface doc (small).
6. Additional target probes on demand (trivial each).
7. ~~Parser-level distinct `ItemKind::Class` AST variant~~ —
   **shipped as P09.39, 2026-04-24**.

**1.02 — user-visible class-keyword IDE support**:
1. rust-analyzer fork for `class` (weeks). Now unblocked: RA
   mirrors the `ItemKind::Class` shape from P09.39.
2. True compiler auto-synthesis for `#[repr(swift)]` (large).

**1.1 — multi-inheritance capstone**:
1. Multi-inheritance + virtual bases (very large).

Out of scope: Windows MSVC ABI.

## Morning review checklist

- [ ] Read this file + `fork/PATCHES.md` §§ P09.22–P09.39 +
      the "rustcc v1 milestone" marker after P09.32 +
      the "Post-v1 target extensions" section containing P09.37,
      P09.38, and P09.39.
- [ ] Read the rewritten `fork/getting-started.html` as a
      GitHub project intro (now lists ESP32-C3 / RISC-V alongside
      STM32 / ARM Cortex-M).
- [ ] Diff `fork/patches/09-riscv-cxx-overlay.patch` and
      `fork/patches/10-itemkind-class.patch`.
- [ ] `cargo test --workspace` → 235/0.
- [ ] Verify P09.39 probe:
      `cd /tmp/p09-39-itemkind-class && RUSTC=<rust-lang-rust>/build/host/stage1/bin/rustc \`
      `RUSTC_BOOTSTRAP=1 cargo +nightly build && ./target/debug/p09_39_probe`
      (→ "ok: inheritance sum = 15")
- [ ] Verify v1 capstones:
      - `cd /tmp/p09-35-inherit && ./probe`
      - `cd /tmp/p09-36-dyncast && ./probe`
      - `cd /tmp/p09-30-class-kw && ./probe`
      - `cd /tmp/p09-28-swift-auto && ./probe`
- [ ] Verify P09.37 RISC-V probe:
      `cd /tmp/p09-40-riscv32-esp32c3 && RUSTC=<rust-lang-rust>/build/host/stage1/bin/rustc \`
      `RUSTC_BOOTSTRAP=1 cargo +nightly build --release \`
      `--target riscv32imc-unknown-none-elf -Zbuild-std=core,compiler_builtins`
- [ ] Verify P09.38 Pico probe:
      `cd /tmp/p09-41-rp2040-pico && RUSTC=... cargo +nightly build --release \`
      `--target thumbv6m-none-eabi -Zbuild-std=core,compiler_builtins`

## Environment notes

- Stage-1 rustc: `<rust-lang-rust>/build/host/stage1/bin/rustc` (set
  `RUST_LANG_RUST=<path>` in your shell for the probe snippets below).
- After compiler changes, rebuild std:
  `./x.py build --stage 1 library`.
- Probes use `cargo +stage1 build` with a standalone
  `[workspace]` header.

## Session takeaways saved to memory

- **P09.26**: don't assume ABI when data layout matches —
  verify VWT slot order against canonical compiler's IR.
- **P09.29**: new compiler attrs need deliberate
  `encode_cross_crate` choices — "No by default" breaks
  downstream mangling.
- **P09.30**: scope-check design options — the Option A vs B
  decision saved 500+ LOC of work for the same surface.
- **P09.32**: P09.25's entry-point ctor vptr init is
  insufficient for derived classes because user code writes
  base's vptr into offset 0 afterward. Inject at return
  instead (or in addition). Pattern: any "must-be-last write"
  should hook return terminators, not function entries.
