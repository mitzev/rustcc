# rustcc — session restart (v1 shipped + P09.37 RISC-V)

Last updated: **2026-04-22**, Opus 4.7 (1M ctx). **rustcc v1
milestone complete; P09.37 adds RISC-V ESP32 / bare-metal rv32.**

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

## Latest addition (P09.37, 2026-04-22)

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

## v2 backlog

See `project_queue_state.md` memory. Headlines:

1. Virtual method override (medium).
2. Multi-inheritance / virtual bases (very large, weeks).
3. Compiler auto-synthesis for `#[repr(swift)]` (large).
4. Non-POD extra fields in class-backed Swift bindings (small).
5. Const generics on class header (small-medium).
6. Distinct `ItemKind::Class` AST variant (very large, for
   editor tools).
7. Windows MSVC ABI (out of scope).

## Morning review checklist

- [ ] Read this file + `fork/PATCHES.md` §§ P09.22–P09.37 +
      the "rustcc v1 milestone" marker after P09.32 +
      the "Post-v1 target extensions" section containing P09.37.
- [ ] Read the rewritten `fork/getting-started.html` as a
      GitHub project intro (now lists ESP32-C3 / RISC-V alongside
      STM32 / ARM Cortex-M).
- [ ] Diff `fork/patches/09-riscv-cxx-overlay.patch`.
- [ ] `cargo test --workspace` → 235/0.
- [ ] Verify v1 capstones:
      - `cd /tmp/p09-35-inherit && ./probe`
      - `cd /tmp/p09-36-dyncast && ./probe`
      - `cd /tmp/p09-30-class-kw && ./probe`
      - `cd /tmp/p09-28-swift-auto && ./probe`
- [ ] Verify P09.37 RISC-V probe:
      `cd /tmp/p09-40-riscv32-esp32c3 && RUSTC=<rust-lang-rust>/build/host/stage1/bin/rustc \`
      `RUSTC_BOOTSTRAP=1 cargo +nightly build --release \`
      `--target riscv32imc-unknown-none-elf -Zbuild-std=core,compiler_builtins`

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
