# rustcc — session restart (v1 + 1.01 closed + 1.02 #1 Phase 1 + 1.02 #2 + throws + Rank 1 adoption infra)

Last updated: **2026-04-25**, Opus 4.7 (1M ctx). **rustcc v1
milestone complete. Post-v1 shipped: P09.37 (RISC-V ESP32),
P09.38 (Raspberry Pi Pico), P09.39 (ItemKind::Class), P09.40-44
(1.01 batch closing items #1-#6), P09.45 (rust-analyzer fork for
`class` keyword — Phase 1 parser support, 1.02 #1 Phase 1),
P09.46 (`#[swift_value]` built-in attribute macro, 1.02 #2),
P09.47 (Rank 1 adoption infra), P09.48 (Swift throws support —
`#[rustc_swift_throws]` + LLVM `swifterror` + SwiftError runtime
wrapper).** 1.01 fully shipped. 1.02 #1 Phase 1 in. 1.02 #2
shipped. Swift throws shipped. Adoption friction reduced from
"30-90 min source build" to "3 min curl+extract" for published
triples. Next workable items: 1.02 #1 Phase 2 (RA HIR-level
resolution), 1.1 (multi-inheritance).

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

## Latest addition (P09.48, 2026-04-25)

**1.02 throws shipped.** `#[rustc_swift_throws]` is a new
attribute on `extern "Swift"` foreign fns that attaches LLVM's
`swifterror` to the last parameter, pinning it to the Swift-ABI
error register. The `SwiftError` type in `rustcc_swift_rt`
owns the retained error pointer with Drop semantics.

**Why it matters**: Swift throwing functions were the single
biggest Swift-interop gap I called out in the 1.02 scope
discussion. ~280 LOC closed the whole feature — much lower
than the memory's prior "large" estimate because most of the
work is existing LLVM machinery; rustcc just flips the right
attribute at the right arg slot.

**Patch**: `fork/patches/14-swift-throws.patch` (15 rustc
files, +100 / -2 LOC) plus `crates/rustcc_swift_rt/src/lib.rs`
(+106 LOC for SwiftError type). Series now 14 patches.

**Scope walkthrough**:
- `LLVMRustAttributeKind::SwiftError` new variant in both the
  C++ wrapper (`RustWrapper.cpp::fromRust`) and the Rust FFI
  enum (`rustc_codegen_llvm::llvm::ffi::AttributeKind`), mapping
  to LLVM's `Attribute::SwiftError`.
- `rustc_swift_throws` symbol + `RustcSwiftThrows(Span)` HIR
  variant + parser via P09.43's `rustcc_noargs_attr!` macro +
  `CodegenFnAttrFlags::SWIFT_ERROR_LAST_ARG` flag.
- `FnAbi::swift_error_last_arg: bool` — threads the flag from
  codegen_fn_attrs to the callconv layer. Populated in
  `rustc_ty_utils::abi::fn_abi_new_uncached`. Static-size
  assert bumped 80 → 88.
- `apply_attrs_llfn` + `apply_attrs_callsite` attach the LLVM
  SwiftError attribute at `Argument(i - 1)` after the normal
  arg loop.

**Validation**:
- Stage-1 library rebuilds clean.
- `/tmp/p09-48-swift-throws/` emits
  `declare swiftcc i64 @"..."(i64, ptr swifterror)` +
  matching call site — exactly what the Swift ABI wants.
- In-tree `fork/tests/class_keyword/swift_throws/` added;
  `fork/tests/run.sh` now 7/7.
- `cargo test --workspace` (rustcc): 235/0, unchanged.

**Memory-worthy**:
- LLVM's `SwiftError` attribute was already there — just
  needed wrapping. Every LLVM C++ attr has two touchpoints
  for rustc bridging: `LLVMRustAttributeKind` enum (both
  sides) + `fromRust` switch case. Pattern reusable for any
  attr not already bridged.
- Runtime round-trip needs matching `swiftcc + swifterror`
  on BOTH caller and callee. An `extern "C"` stub doesn't
  participate in the register convention and reads back the
  original null — the IR-level validation is the real codegen
  check, runtime integration is a Swift-runtime-side concern.

## Prior addition (P09.47 / Rank 1, 2026-04-25, infra-only)

**Shipped the prebuilt-binaries adoption path.** Rank 1 of the
ecosystem-adoption analysis: closes the biggest adoption
friction (30-90 min stage-1 source build → 3 min curl+extract)
for users on x86_64/aarch64 macOS and Linux.

**What's in**:
- `.github/workflows/release.yml` — builds stage-1 rustcc for
  4 targets on tag push (`v*`) or manual dispatch, tarballs
  `build/host/stage1/` + sha256, uploads as GitHub Release
  assets. Includes Linux disk-cleanup step (stage-1 needs
  ~12 GB; default runner has ~14 GB free) and a
  fork/build.sh → `./x.py build --stage 1 library` sequence
  so shipped toolchains can compile normal Rust programs.
- `.github/actions/install-rustcc/action.yml` — composite
  action downstream projects consume via
  `uses: rustcc/rustcc/.github/actions/install-rustcc@main`.
  Auto-detects runner OS+arch, resolves `latest` → tag,
  downloads + verifies sha256 + extracts + `rustup toolchain
  link`s. Supports pinned `version: v1.02.0` for reproducible
  CI.
- `fork/INSTALL.md` — canonical user install doc. Fast path
  (curl+extract+link+rust-toolchain.toml pin) vs source path
  (existing `./fork/build.sh`). Covers supported-triple
  matrix, CI integration, uninstall, troubleshooting.
- `fork/examples/ci-snippet.yml` — minimal downstream CI
  example; projects copy to their `.github/workflows/ci.yml`.
- `fork/getting-started.html` install section rewritten to
  lead with the fast path + `rust-toolchain.toml` pin pattern.

**Critical caveat**: the release workflow has only been
eyeball-reviewed, not tag-push-triggered. When you first push
a `v*` tag, expect possible surprises in:
- The `ubuntu-24.04-arm` runner label (GitHub's aarch64 Linux
  runners changed names mid-2024; fallback documented in the
  workflow as a comment).
- The `$CLONE_DIR/build/host/stage1/` path — if rust-lang/rust's
  stage output layout has changed on the pinned commit, the
  `tar -C $CLONE_DIR/build/host stage1` step will fail loudly
  with a directory listing in the logs.
- Archive size: stage-1 toolchain tarball is probably 150-300 MB
  compressed; GitHub Release asset limit is 2 GB so we have
  headroom but no data yet.

**Follow-ups** (not done in this batch — listed so they don't
get lost):
- Actually trigger a release by pushing `v1.02.0` and iterate
  on any workflow failures. The four prior post-v1 LOC-
  contributions (P09.40-46) make this a reasonable first
  release tag.
- Publish `rustcc_swift_rt` and `rustcc_macros` to crates.io
  (Rank 2 in the adoption plan). Blocked until there's a
  runtime-stubs variant for stock rustc users; otherwise the
  crates fail to compile on crates.io's CI.
- Rustup custom-channel manifest server (Rank 3) — only worth
  building after prebuilts have been validated for a few
  releases.

**How to push a first release tag**:
```bash
# In the rustcc repo, with all recent work committed:
git tag v1.02.0
git push origin v1.02.0
# Then watch Actions → "Release rustcc toolchain" for the 4
# matrix jobs; they'll upload to Releases > v1.02.0 > Assets.
# The release is created as draft; promote to public when all
# 4 matrix jobs succeed.
```

Or manually via `workflow_dispatch` (useful for a dry-run):
```bash
gh workflow run release.yml -f release_tag=v1.02.0-rc1
```

## Prior addition (P09.46, 2026-04-25)

**1.02 #2 shipped: `#[swift_value]` built-in attribute macro.**
The `rustcc_macros::swift_value!` proc-macro wrapper is
retired; users now write:

```rust
#[swift_value]
#[swift_type = "Foo.Bar:class"]
pub struct Bar {
    _ptr: *mut core::ffi::c_void,
    extra: Box<i32>,
}
```

and the compiler auto-synthesizes `impl Drop`, `impl Clone`
(with per-field Clone for non-POD extras on class-backed
types — keeps the P09.42 semantics), and the metadata
accessor `extern "C"` decl with Swift-mangled `link_name`.
`#[repr(swift)]` is added to the struct during expansion if
not already present.

**Scope (Option A — built-in attribute macro)**:
- `compiler/rustc_builtin_macros/src/swift_value.rs` (new,
  ~270 LOC): finds sibling `#[swift_type]`, parses the Swift
  binding, formats expansion as Rust source, re-parses via
  `new_parser_from_source_str`.
- `compiler/rustc_builtin_macros/src/lib.rs`:
  `swift_value: swift_value::expand` in `register_attr!`.
- `compiler/rustc_span/src/symbol.rs`: +`swift_value` symbol.
- `library/core/src/macros/mod.rs`: `pub macro
  swift_value($item:item)` stub with `#[rustc_builtin_macro]`.
- `library/core/src/prelude/v1.rs` + `library/std/src/prelude/v1.rs`:
  re-export so `#[swift_value]` works without a `use`
  statement (matches `#[global_allocator]`).

**Patch**: `fork/patches/13-swift-value-builtin.patch` (6
files, +421/0).

**Probe**: `fork/tests/class_keyword/swift_value_attr/`
(class-backed with `Box<i32>` extra). `fork/tests/run.sh` now
6/6.

**Validation**:
- Stage-1 library rebuilds clean (~2 min incrementally).
- `cargo test --workspace` → 235/0 (unchanged).
- `RUSTC=<stage1> ./fork/tests/run.sh` → 6/6.

**Design note**: I redesigned mid-implementation from
`#[swift_value = "Module.Type"]` (value form) to
`#[swift_value]` + sibling `#[swift_type = "..."]` because
the `pub macro swift_value($item:item)` stub signature in
core doesn't accept name-value input syntax. Siblings are
also more symmetric with how `#[swift_type]` already acts as
the canonical source of the Swift binding.

**Memory-worthy bugs**:
- `pub macro X($item:item)` stubs in `library/core/src/macros/mod.rs`
  reject name-value attribute input. Prefer the sibling-attr
  pattern (`#[X]` + `#[X_config = "..."]`) for anything that
  needs configuration. `#[test_case]` and `#[global_allocator]`
  follow this pattern too.
- Built-in attribute macros need a prelude entry in BOTH
  `core/prelude/v1.rs` AND `std/prelude/v1.rs`, otherwise
  they're "not found in this scope" under `#![no_std]` (or
  with std, depending on which prelude applies).
- Stubbed runtime symbols need `#[export_name]` (not
  `#[no_mangle]` + matching Rust name), because the synthesized
  extern decl uses the Swift-mangled symbol string
  `$s<modlen><mod><typelen><type>VMa` which isn't a valid Rust
  identifier.

## Prior addition (P09.45, 2026-04-24)

**1.02 #1 Phase 1 shipped: rust-analyzer fork for `class`.** Parser
now accepts `class Widget { ... }` as a first-class AST item
instead of producing a cascade of error nodes. Files with class
items go from "mostly broken in the editor" to "class body is
syntactically visible, surrounding items work normally".

**Scope — Phase 1 (parser only)**:
- `class` added to `CONTEXTUAL_KEYWORDS` in RA's grammar codegen.
- New `CLASS` syntax node + `CLASS_MEMBER_LIST` child in
  rust.ungram. Regenerated `generated/nodes.rs` and
  `syntax_kind/generated.rs`.
- `adt::klass` parser handles the header (generics, optional
  `: BaseType`, where-clause); `class_member_list` alternates
  record-field parsing and `item_or_macro` based on a
  `looks_like_field_at` peek.
- Non-exhaustive match arms updated in hir-expand, hir-def,
  hir/semantics, ide-assists (9 sites), syntax.

**Scope — Phase 2 (deferred)**:
- Class name isn't in item tree → no hover / go-to-def / workspace
  symbol on the class name itself.
- Methods inside the class body have FN nodes but no HIR
  representation → no method-call resolution.
- Autocomplete on class fields/methods doesn't fire.

Phase 2 plan: synthesize `Struct` + inherent `Impl` pair in
`hir-def/item_tree/lower.rs::lower_mod_item`, mirroring P09.39's
rustc-side AST → HIR lowering. Most ADT + impl resolution paths
then work "for free".

**Patch**: `fork/ra-patches/01-ra-class-keyword.patch`, against
`rust-lang/rust-analyzer`. 18 files touched (+1351/-182, mostly
regenerated codegen output).

**Validation**:
- `cargo test -p parser` (in ra repo) → 315/0 (+1 inline test).
- `cargo test -p syntax` → 51/0.
- Manual probe with generics + inheritance + trailing `fn` — zero
  ERROR nodes in parse tree.

**Install note**: users point their editor at the built
`target/release/rust-analyzer` binary via
`rust-analyzer.server.path`. See `fork/ra-patches/README.md`.

**Memory-worthy bugs**:
- The `// test <name>` inline-test extraction in RA's xtask
  reads all consecutive `//` lines after the `// test` line as
  test body. Long comment blocks above a `fn` need the `// test
  ...` / `// <code>` pair to be immediately adjacent, not
  separated by any other `//` comment. Manifested when my first
  codegen run captured the full docblock as the test case.
- AstId typing is strict: `source_ast_id_map.ast_id(class)`
  returns `AstId<ast::Class>`, which can't be directly stored
  under a `Struct` small-data slot. Phase 1 works around by
  emitting no item-tree entry for classes; Phase 2 will need a
  proper synthesized struct node or an `ItemTreeAstId<Class>`
  small-data variant.

## Prior addition (P09.44, 2026-04-24, documentation-only)

**1.01 #6 shipped: Linux/desktop target coverage.** Four
commonly-asked-for Linux triples probed, all zero-code — upstream
rustc handles the target-triple split at a layer below the
fork's Itanium overlays, and the overlays themselves are keyed
on **architecture**, not target:

| Target | Arch | Covered by |
|---|---|---|
| `x86_64-unknown-linux-gnu` | x86_64 | P09.6 / P09.11 overlay |
| `aarch64-unknown-linux-gnu` | aarch64 | P09.11 overlay |
| `armv7-unknown-linux-gnueabihf` | ARMv7-A | upstream + Itanium |
| `riscv64gc-unknown-linux-gnu` | rv64gc | P09.37 overlay |

**Probe**: `fork/tests/run_targets.sh` runs
`targets_linux/` against the four triples with
`-Zbuild-std=core,compiler_builtins -C opt-level=0` and grep-
checks for the expected Itanium symbols
(`_ZN6Widget3newEi`, `_ZTI6Widget`, `_ZTS6Widget`, `_ZTV6Widget`).
All four pass.

**Patch**: none (documentation-only).

**Why -O0 in the probe**: at release, LLVM devirtualizes
`call_foo`'s dispatch and DCE's the vtable + typeinfo globals —
useful for production but hides what the fork codegen actually
emits. `-C opt-level=0` preserves the raw emission so the probe
can verify ABI correctness rather than LLVM's eventual cleanup.

## Prior 1.01 batch (P09.40-P09.43, 2026-04-24)

After P09.39 shipped 1.01 #7, the remaining 1.01 backlog (items
#1–#5) was closed in one batch:

- **P09.40 (#5, three-surface doc)**. New `fork/THREE-SURFACES.md`
  reference explaining when to use `cxx_class!` vs
  `cxx_class_native!` vs parser `class`. Doc-only.
- **P09.41 (#1, generics on class)**. Fixed a P09.39 regression:
  any `class Foo<A>` (lifetime/type/const generic) ICEd at
  ast_lowering with "duplicate copy of DefId". Root cause: single
  `generics` field shared between struct + impl halves. Fix:
  parallel `impl_generics` on `ast::Class`, clone at parse time,
  two-phase `resolve_class` (struct rib for fields, impl rib for
  methods). 161 / −33 LOC across 7 rustc files.
- **P09.42 (#2, non-POD Swift extras)**. Class-backed `swift_value!`
  Clone now uses a `Self { ... }` struct literal with per-field
  `Clone::clone` for extras; previous memcpy-and-overwrite
  double-freed Box/String/Vec extras. ~85 LOC in
  crates/rustcc_macros.
- **P09.43 (#3, attr-plumbing unification)**. Collapsed five
  `NoArgsAttributeParser` impls in rustc_attr_parsing into one
  `rustcc_noargs_attr!` macro invocation each. Pure cleanup; ~30
  LOC down.
- **1.01 #4 (in-tree test crate)**. `fork/tests/class_keyword/`
  now holds five probes covering basic, inheritance, type
  generics, const generics, and non-POD Swift extras. Runner
  script `fork/tests/run.sh` rebuilds each under stage-1 rustc
  and checks output banners. Replaces the ephemeral /tmp probes.

**Validation across the batch**:
- `./x.py build --stage 1 library` clean.
- `cargo test --workspace` → 235/0 (unchanged from v1 baseline).
- `RUSTC=<stage1> ./fork/tests/run.sh` → 5/5 passing.

**Bugs surfaced during P09.41**:
- Type generics (`class Pair<A, B>`) were ICEing the same way
  const generics did. P09.39's regression affected ALL generics,
  not just const — const was just the first symptom I noticed.
  Worth remembering: when one generic form breaks, probe the
  others before declaring the fix scope.
- The `build_cxx_class_self_path` stub for const generics
  emitted `GenericArg::Type` wrapping a const-param path;
  typeck couldn't process it. Fixed to emit
  `GenericArg::Const(AnonConst { value: path_expr })`.

## Prior addition (P09.39, 2026-04-24)

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

See `project_queue_state.md` memory. **Post-P09.43, 1.01 is
closed.**

**1.01 — FULLY SHIPPED 2026-04-24**:
1. ~~Const generics on class header~~ — P09.41.
2. ~~Non-POD extras in swift_value!~~ — P09.42.
3. ~~`rustc_cxx_*` attr-plumbing unification~~ — P09.43.
4. ~~In-tree test crate for class probes~~ — `fork/tests/`.
5. ~~Three-surface doc~~ — P09.40.
6. ~~Additional Linux/desktop target probes~~ — P09.44
   (x86_64 / aarch64 / armv7 / rv64 Linux; all zero-code).
7. ~~Parser-level `ItemKind::Class` AST variant~~ — P09.39.

**1.02 — user-visible class-keyword IDE support**:
1. ~~rust-analyzer fork for `class` — Phase 1 parser support~~
   — P09.45. Phase 2 (HIR-level class→struct+impl synthesis for
   hover / go-to-def / completion) remains.
2. ~~True compiler auto-synthesis for `#[repr(swift)]`~~
   — P09.46. `#[swift_value]` built-in attribute macro
   retires the `swift_value!` proc macro.

**1.1 — multi-inheritance capstone**:
1. Multi-inheritance + virtual bases (very large).

Out of scope: Windows MSVC ABI.

## Morning review checklist

- [ ] Read this file + `fork/PATCHES.md` §§ P09.22–P09.46
      + Rank 1 latest-addition block above.
- [ ] Sanity-check the release workflow YAML
      (`.github/workflows/release.yml`) — look for mis-quoted
      strings, runner-label typos. The `workflow_dispatch`
      `release_tag` input is the dry-run knob.
- [ ] If comfortable: push a throwaway tag like `v1.02.0-rc1`
      and watch the 4 matrix jobs. Artifacts land in
      Actions → Run → Artifacts even if the Release-upload
      step fails, so you can inspect the tarballs without
      polluting the real Release page.
- [ ] Read `fork/THREE-SURFACES.md` for the surface-selection
      reference.
- [ ] `cargo test --workspace` → 235/0.
- [ ] `RUSTC=<stage1> ./fork/tests/run.sh` → 5/5 passing.
- [ ] Optional cross-target probe:
      `RUSTC=<stage1> ./fork/tests/run_targets.sh` → 4/4
      (~3 min total; skip on quick regression checks).
- [ ] Optional: diff the rustc-side post-v1 patches 10–12
      (`fork/patches/10-itemkind-class.patch`,
      `11-class-generics.patch`, `12-attr-plumbing-macro.patch`).
- [ ] Optional rust-analyzer probe: apply
      `fork/ra-patches/01-ra-class-keyword.patch` against a
      clone of rust-lang/rust-analyzer; `cargo test -p parser`
      → 315/0. Binary at `target/release/rust-analyzer`.
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
