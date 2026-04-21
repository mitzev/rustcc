# rustcc — session restart (v1 shipped)

Last updated: **2026-04-21**, Opus 4.7 (1M ctx). **rustcc v1
milestone complete.**

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

## This session's additions (P09.31 + P09.32)

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

- [ ] Read this file + `fork/PATCHES.md` §§ P09.22–P09.32 +
      the "rustcc v1 milestone" marker after P09.32.
- [ ] Read the rewritten `fork/getting-started.html` as a
      GitHub project intro.
- [ ] Diff the new patches:
      `fork/patches/09-31-small-fixes-bundle.patch`
      `fork/patches/09-32-single-inheritance.patch`
- [ ] `cargo test --workspace` → 235/0.
- [ ] Verify v1 capstones:
      - `cd /tmp/p09-35-inherit && ./probe`
      - `cd /tmp/p09-36-dyncast && ./probe`
      - `cd /tmp/p09-30-class-kw && ./probe`
      - `cd /tmp/p09-28-swift-auto && ./probe`

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
