# Patches: upstream `rustc` → forked `rustcc`

## How this document is organized

`fork/patches/` contains a nine-file series that applies cleanly in
order against `rust-lang/rust` at commit
`e22c616e4e87914135c1db261a03e0437255335e` (the SHA pinned in
`fork/build.sh`). Each file is the mechanical delivery for one
semantic cluster:

| File                                  | Scope                                                                |
|---------------------------------------|----------------------------------------------------------------------|
| `01-rustc-abi-cxx-crate.patch`        | Vendor `rustc_abi_cxx` into `compiler/`                              |
| `02-abi-plumbing.patch`               | `ExternAbi::Cpp` + callconv lowering + rustc_public bridge           |
| `03-attrs-and-repr.patch`             | `rustc_cxx_*` / `rustc_swift_*` attributes + `repr(cpp)` / `repr(swift)` |
| `04-parser-class-keyword.patch`       | `class` keyword + body-attribute acceptance                          |
| `05-middle-end-layout-bridge.patch`   | Middle-end layout bridge + ty hooks                                  |
| `06-symbol-mangling.patch`            | Itanium + Swift mangling dispatch                                    |
| `07-codegen.patch`                    | Codegen LLVM: vtable emission, ctor vptr-init, call lowering         |
| `08-cargo-lock.patch`                 | `Cargo.lock` refresh                                                 |
| `09-riscv-cxx-overlay.patch`          | RISC-V Itanium overlay (rv32 / rv64, P09.37 post-v1)                 |

The **P01 … P09.37 sections below** are the authoritative design
record. Each documents the intent, validation probe, and any
compiler-internal trade-offs for one unit of work. The numbering is
historical — it tracks the order the work was done, not the layout
of the current eight-file delivery. Read the P-sections when you
want to know *why* a change looks the way it does.

Verified against `rust-lang/rust` master at ~2025-04 (structurally
equivalent to nightly-2025-10-03, which our workspace pins). Line
numbers are approximate — look for the anchor text in each hunk
rather than trusting `:N:` if master has drifted.

---

## P01 — Add `cpp` symbol

**File:** `compiler/rustc_span/src/symbol.rs`

Anchor: the alphabetical block of reserved symbols, between
`coverage_attribute,` and `cr,`:

```
         coverage_attribute,
+        cpp,
         cr,
```

Needed so `rustc_attr_parsing` and everywhere downstream can refer
to the attribute name via `sym::cpp`.

---

## P02 — `ReprAttr::ReprCpp` variant

**File:** `compiler/rustc_hir/src/attrs/data_structures.rs`

Anchor: the `ReprAttr` enum definition (~line 168):

```rust
 #[derive(PartialEq, Debug, Encodable, Decodable, Copy, Clone, HashStable_Generic, PrintAttribute)]
 pub enum ReprAttr {
     ReprInt(IntType),
     ReprRust,
     ReprC,
+    ReprCpp,
     ReprPacked(Align),
     ReprSimd,
     ReprTransparent,
     ReprAlign(Align),
 }
```

The new variant behaves like `ReprC` at the attribute layer; its
distinct semantics kick in at layout + codegen time (P06, P08).

---

## P03 — Parse `#[repr(cpp)]`

**File:** `compiler/rustc_attr_parsing/src/attributes/repr.rs`

Anchor: the `parse_repr` fn body, next to the `sym::C` arm (~line 145):

```rust
     (Some(sym::Rust), ArgParser::NoArgs) => Some(ReprRust),
     (Some(sym::C), ArgParser::NoArgs) => Some(ReprC),
+    (Some(sym::cpp), ArgParser::NoArgs) => Some(ReprCpp),
     (Some(sym::simd), ArgParser::NoArgs) => Some(ReprSimd),
     (Some(sym::transparent), ArgParser::NoArgs) => Some(ReprTransparent),
```

Also extend the NameValue-rejection arm (~line 153) to include
`sym::cpp` so `#[repr(cpp = "...")]` yields the clean
`InvalidReprHintNoValue` diagnostic:

```rust
     (
         Some(
             name @ sym::Rust
             | name @ sym::C
+            | name @ sym::cpp
             | name @ sym::simd
             | name @ sym::transparent
             | name @ int_pat!(),
         ),
         ArgParser::NameValue(_),
     ) => {
```

---

## P04 — `ReprFlags::IS_CPP` + `ReprOptions::cpp()`

**File:** `compiler/rustc_abi/src/lib.rs`

Anchor: the `bitflags! { impl ReprFlags ... }` block (~line 83):

```rust
     bitflags! {
         impl ReprFlags: u8 {
             const IS_C               = 1 << 0;
             const IS_SIMD            = 1 << 1;
             const IS_TRANSPARENT     = 1 << 2;
             const IS_LINEAR          = 1 << 3;
             const RANDOMIZE_LAYOUT   = 1 << 4;
             const PASS_INDIRECTLY_IN_NON_RUSTIC_ABIS = 1 << 5;
             const IS_SCALABLE        = 1 << 6;
+            const IS_CPP             = 1 << 7;
```

Anchor: the `impl ReprOptions { pub fn c(&self) ... }` accessors
(~line 187):

```rust
     pub fn c(&self) -> bool {
         self.flags.contains(ReprFlags::IS_C)
     }
+
+    /// True when `#[repr(cpp)]` is present. Implies C-compatible
+    /// layout semantics *plus* the extended constraints of the
+    /// Itanium C++ ABI (sret for non-trivial-for-calls classes,
+    /// explicit `this`-pointer, etc.). The layout query routes
+    /// through `rustc_abi_cxx` when this is set.
+    pub fn cpp(&self) -> bool {
+        self.flags.contains(ReprFlags::IS_CPP)
+    }
```

`FIELD_ORDER_UNOPTIMIZABLE` doesn't need updating (IS_CPP implies
the same field-order constraint as IS_C, but we compute the layout
elsewhere so the flag set doesn't need to see it).

---

## P05 — Thread `ReprCpp` → `IS_CPP`

**File:** `compiler/rustc_middle/src/ty/mod.rs`

Anchor: the `ReprOptions::from_attrs` body, around the `attr::ReprC
=> ReprFlags::IS_C` arm (~line 1454):

```rust
             flags.insert(match *r {
                 attr::ReprRust => ReprFlags::empty(),
                 attr::ReprC => ReprFlags::IS_C,
+                attr::ReprCpp => ReprFlags::IS_CPP | ReprFlags::IS_C,
                 attr::ReprPacked(pack) => { ... }
                 ...
             });
```

Setting both `IS_CPP` and `IS_C` means every caller that asks
"C-compatible layout?" answers true for `#[repr(cpp)]` types
(field order preserved, no Rust-reordering), and the small set of
callers that need to distinguish C from C++ semantics check
`repr.cpp()` specifically.

---

## P06 — Layout delegation

**File:** `compiler/rustc_ty_utils/src/layout.rs`

This is the load-bearing change. Anchor: the `adt_def.repr()`
branch inside `layout_of_uncached` where ADT layouts get computed
(search for `def.is_struct()` and `.repr()`).

Pseudo-patch (exact syntax depends on current surrounding code):

```rust
     ty::Adt(def, args) if def.is_struct() || def.is_enum() || def.is_union() => {
         let repr = def.repr();
+        if repr.cpp() {
+            // Route to the rustcc Itanium-ABI layout calculator.
+            // `cx.tcx()` lifts to a rustcc::LayoutProvider adapter;
+            // the call surface is defined in the new
+            // `rustc_abi_cxx` dependency. The result is converted
+            // back to rustc's LayoutData via the helper in
+            // `rustc_ty_utils::layout::cxx_bridge`.
+            return cxx_bridge::layout_cpp(cx, def, args);
+        }
         // ... existing rustc layout path continues unchanged ...
     }
```

Two new sibling files:

- `compiler/rustc_ty_utils/src/layout/cxx_bridge.rs` — adapter
  that walks `def.all_fields()`, recursively layouts each field
  type via the normal `cx.layout_of()` (so nested `#[repr(cpp)]`
  types chain cleanly), and then calls into `rustc_abi_cxx::layout`
  with the `ClassDef` built from those field layouts. Translates
  the resulting `RecordLayout` back into rustc's `LayoutData`.
- `Cargo.toml` entry: depend on `rustc_abi_cxx` as a path dep
  pointing at our out-of-tree crate (or vendored copy). Upstream
  bootstrap infrastructure may need a corresponding entry in
  `src/bootstrap/` to build our crate as part of stage-0 → stage-1.

The bridge is the fiddliest piece. Structural risk: `LayoutData`'s
shape (offsets, scalars, variants) is an internal type that
changes shape across releases. The bridge has to be re-synced when
upstream refactors layout data. Nightly-2025-10-03 freezes a
specific layout shape that matches our work.

---

## P07 — `ExternAbi::Cpp` + mangler route

### P07a — ABI variant

**File:** `compiler/rustc_abi/src/extern_abi.rs`

Anchor: the `pub enum ExternAbi` definition (~line 20):

```rust
 pub enum ExternAbi {
     /* universal */
     C {
         unwind: bool,
     },
+    /// The Itanium C++ ABI. Enables sret for non-trivial-for-calls
+    /// classes, `this`-pointer prepending for methods, and name
+    /// mangling via `rustc_abi_cxx::mangle`. Only valid on targets
+    /// whose C++ runtime follows Itanium (Clang on Linux/Darwin/iOS).
+    Cpp {
+        unwind: bool,
+    },
     System {
         unwind: bool,
     },
```

The textual ABI string `"C++"` needs to be accepted in the
matching parser (usually in `compiler/rustc_parse/src/parser/ty.rs`
or wherever extern strings are interned). Search for the match on
`"C"` and add a sibling `"C++"` arm.

### P07b — Mangling override

**File:** `compiler/rustc_symbol_mangling/src/lib.rs`

Anchor: `compute_symbol_name` (~line 155). Add an early route:

```rust
 fn compute_symbol_name<'tcx>(
     tcx: TyCtxt<'tcx>,
     instance: Instance<'tcx>,
     compute_instantiating_crate: impl FnOnce() -> CrateNum,
 ) -> String {
     let def_id = instance.def_id();
+
+    // `extern "C++"` items get Itanium-mangled names via the
+    // rustcc mangler. We check the ABI on the instance's function
+    // signature; items without a function signature fall through.
+    if let Some(sig) = tcx.fn_sig_or_none(def_id) {
+        if matches!(sig.skip_binder().abi(), ExternAbi::Cpp { .. }) {
+            return rustc_symbol_mangling::itanium::mangle(tcx, instance);
+        }
+    }
```

New module `compiler/rustc_symbol_mangling/src/itanium.rs` calls
into `rustc_abi_cxx::mangle` with a `CxxTypeCtx` built from the
instance's signature.

---

## P08 — `CXX` calling convention

**Files:**
- `compiler/rustc_target/src/callconv/mod.rs` — add `CallConv::Cpp`
  dispatch in the public `fn_abi` builder.
- `compiler/rustc_target/src/callconv/x86_64.rs` (and analogues) —
  adjust `compute_abi_info` to emit sret for non-trivial-for-calls
  records when the ABI is `Cpp`. The rule is "any
  `#[repr(cpp)]` record with a user-declared `impl Drop` OR
  larger than 16 bytes returns via sret".
- `compiler/rustc_codegen_llvm/src/abi.rs` — emit the LLVM sret
  attribute + prepend `this` pointer when lowering a `CallConv::Cpp`
  instance.

This is the largest and most target-specific patch. Each supported
target (x86_64-linux, x86_64-darwin, aarch64-linux, aarch64-darwin)
needs its own `compute_abi_info` case. Scope for v1: limit to
x86_64 targets; aarch64 gets a `todo!()` with a clear diagnostic
until someone drives the ARM AAPCS64 Itanium variant.

---

## Out-of-scope for v1 patches

These are noted so the fork's first PR doesn't try to swallow them:

- **Virtual dispatch** (`virtual` methods on `#[repr(cpp)]` types) —
  requires vtable emission driven from `rustc_abi_cxx::vtable`, new
  codegen path for indirect calls, and a coherent interplay with
  Rust's trait-object machinery. Plan separately as part of v1.5.
- **`extern "C++" { ... }` blocks** — imports of C++ items. Lives in
  `rustc_ast_lowering` + `rustc_resolve`. Wire after the emission
  side is stable.
- **C++ template instantiation driven from Rust generics** — multi-
  crate concern, needs its own design pass.
- **Exception propagation** — out of v1 scope per
  `docs/exception_boundary.md`; terminate-on-throw is the contract.

## Where the `rustcc_codegen` adapter traits plug in

The traits under `crates/rustcc_codegen/src/lib.rs`
(`LayoutProvider`, `ManglerProvider`, `ShimResolver`) were
designed as the exact seam between rustc and our IR. The fork
implements them as thin adapters around `TyCtxt`:

- `LayoutProvider for TyCtxt<'tcx>` — P06's bridge.
- `ManglerProvider for TyCtxt<'tcx>` — P07b's itanium module.
- `ShimResolver for TyCtxt<'tcx>` — walks HIR / typeck for
  `extern "C++"` fn decls, used by `extern "C++" { ... }` block
  lowering (v1.5).

`lower_method_call` in `rustcc_codegen` is a ready-to-drop-in
helper the fork's codegen can call to produce `LoweredCall`s,
which `rustc_codegen_llvm::abi` then materializes into LLVM IR.
This means P08 is "adapter + LoweredCall → LLVM IR" rather than
"reimplement ABI from scratch".

## Patch-apply status

**P01–P09 verified end-to-end with a live Rust↔C++ binary.**

Rust code:

```rust
extern "C++" {
    fn cpp_add(a: i32, b: i32) -> i32;
    fn cpp_product(a: u64, b: u64) -> u64;
    fn cpp_select(flag: bool, yes: i32, no: i32) -> i32;
    fn cpp_ptr_len(s: *const u8) -> u64;
}
```

Linked against a Clang-compiled C++ library that provides the
bodies. Binary runs, returns exit code 42 — only reachable if
all four calls produced the expected value:

```
cpp_add(2, 3)            == 5    ✓
cpp_product(100, 50)     == 5000 ✓
cpp_select(true, 7, 99)  == 7    ✓
cpp_ptr_len(b"hi\0")     == 2    ✓
```

This is the first real Rust↔C++ binary built on the fork. All
four symbols resolve at link time via the Itanium names P08b
produces, and the C-compatible call convention (inherited by
`CanonAbi::Cpp` via P09's scaffold) gets scalar/pointer args
correctly across the boundary.

### What P09 actually does today

P09 introduces `CanonAbi::Cpp` as a distinct canonical ABI and
routes `ExternAbi::Cpp` to it via `abi_map::canonize_abi`. Every
downstream consumer currently treats it identically to
`CanonAbi::C`:

- `rustc_ast_passes::ast_validation` — no extra signature checks
- `rustc_hir_typeck::callee` — calls are allowed (not
  asm-only / interrupt / gpu-kernel)
- `rustc_codegen_llvm::abi` — LLVM `CCallConv` at the backend
- `rustc_public::unstable::convert::stable::abi` — stable-API
  clients see it as `CallConvention::C`

The distinct variant doesn't change any observable behavior yet
— the scaffold exists so future patches can add Itanium-specific
per-target lowering (sret for non-trivial-for-calls returns,
`this`-pointer prepending for methods, record-by-value caller-
destroys convention) at a well-defined hook point without
perturbing the plain `extern "C"` path.

### What P09 doesn't do yet

The cases that actually diverge from C on SysV AMD64 and still
need real codegen work:

- **sret for non-trivial-for-calls returns**. A C++ class with a
  user destructor returned by value is passed via an implicit
  output pointer. Rust's `extern "C"` only uses sret for
  aggregates > 16 bytes. The fork would need per-target
  `compute_cxx_abi_info` that overrides return-value handling
  when the returned type is declared with `#[repr(cpp)]` and has
  `impl Drop`.
- **`this`-pointer prepending for methods**. Once `#[repr(cpp)]
  impl T { fn method(&self) {...} }` gets fork codegen (today
  handled by the forwarders path), the method's extern-C lowering
  would need to prepend `this` as a hidden first argument.
- **Record-by-value param convention**. Itanium's caller-destroys
  rule (on x86-64 SysV) means the caller allocates a temp,
  passes a pointer, and destroys after return. Rust's stock
  `extern "C"` passes records by value in registers when they
  fit. Forwarders hit this with `ptr::read` hacks; a real fork
  would emit the ABI directly.

These are target-specific codegen patches — each one slots into
`CanonAbi::Cpp` handling in `rustc_target/src/callconv/<arch>.rs`
and `rustc_codegen_llvm/src/abi.rs`. Doable in a future session
but not tractable in one overnight push.

### P09.1 — non-trivial-for-calls + record mangling (done)

Completed overnight on 2026-04-20, extending P09 with the real
Itanium divergences for x86_64-non-Windows:

- `TyAbiInterface::is_cxx_non_trivial_for_calls` — new query that
  returns `true` when a `#[repr(cpp)]` ADT has a user-declared
  `Drop` impl. Implemented on `Ty<'tcx>` in
  `rustc_middle/src/ty/layout.rs` via `def.destructor(tcx)`.
  Patch: `09-1-non-trivial-for-calls.patch`.
- `compute_cxx_abi_info` in `rustc_target/src/callconv/x86_64.rs`
  — wraps the C-path `compute_abi_info` and, afterward, promotes
  any non-trivial-for-calls return or argument to indirect. Uses
  a new `ArgAbi::force_indirect` helper that bypasses the
  state-check in `make_indirect` (the C path may already have
  transitioned the arg to `Cast` mode). Dispatched from
  `callconv/mod.rs::adjust_for_foreign_abi` on `ExternAbi::Cpp`
  for x86_64 non-Windows. Patch: `09-1-callconv-cxx-abi.patch`.
- Itanium record mangling in `rustc_symbol_mangling/src/itanium.rs`
  — `rustc_ty_to_cxx` now registers `#[repr(cpp)]` ADTs as
  `CxxType::Record` with a minimal `ClassDef`. This makes
  parameters/returns of repr(cpp) type mangle as their class name
  (e.g. `6Widget`) rather than falling back to the unmangled Rust
  symbol. Folded into the existing `08-itanium-module.patch`.

**Verified**: probe at `/tmp/p09-sret/probe.rs` with an 8-byte
`Widget` struct + `impl Drop`. Emitted LLVM IR:

```llvm
declare void @_Z11make_widgetv(ptr sret([8 x i8]) align 8) ...
declare void @_Z14consume_widget6Widget(ptr align 8) ...
```

matches Clang's reference output exactly (mangled names and sret +
indirect ABI shape). Attribute differences (`dead_on_unwind
writable`, `noundef`) are codegen-level hints that don't affect
linkage or calling convention.

**End-to-end runtime**: `/tmp/p09-runtime/` links a Rust
staticlib + C++ `widget.cpp` + C runner into a single binary.
Rust calls `make_widget()` (sret return) and `consume_widget(w)`
(indirect arg). The C++ side stashes `w.handle` in a global; Rust
reads it back. Handle `0xDEADBEEF` round-trips cleanly, demo
exits 42. Workspace regression: 233/0.

### P09.2 — Tier 1 method mangling + inferred C++ ABI (done)

Completed 2026-04-20, on top of P09.1. Lets the user write:

```rust
#[repr(cpp)]
pub struct Widget { pub handle: u64 }
impl Widget {
    #[unsafe(no_mangle)]
    pub fn area(&self) -> u64 { self.handle * 2 }
    #[unsafe(no_mangle)]
    pub fn bump(&mut self, by: u64) { self.handle += by; }
}
```

and have the symbols emit with Itanium member-function mangling
and calling convention matching Clang's:

- `area(&self)` → `_ZNK6Widget4areaEv` (const-this)
- `bump(&mut self, u64)` → `_ZN6Widget4bumpEy` (non-const-this)

Matches exactly what `clang++` emits for
`uint64_t Widget::area() const` and `void Widget::bump(uint64_t)`.

Patches:

- `08-itanium-module.patch` — extends `cxx_impl_class` helper and
  adds method-symbol emission (`CxxSymbol::Method`) to
  `itanium.rs`. Strips the first input (self) when building the
  Cxx param vector; emits `CvQual::is_const = true` for
  `&self` methods.
- `09-2-abi-infer-cxx-methods.patch` — in
  `rustc_ty_utils/src/abi.rs::fn_abi_new_uncached`, computes an
  `effective_abi` that promotes impl methods on `#[repr(cpp)]`
  ADTs to `ExternAbi::Cpp` regardless of the method's nominal
  sig ABI. This routes them through
  `x86_64::compute_cxx_abi_info` so non-trivial record args /
  returns become indirect to match Clang.

**Verified end-to-end** (`/tmp/p09-methods/`): C++ file declares
`struct Widget { uint64_t handle; uint64_t area() const; void
bump(uint64_t); };` and calls the methods; Rust provides the
bodies. Links as a single binary, C++-originated calls and
Rust-originated calls both return 50 (10×2 + 15×2). Workspace
regression: 233/0.

### P09.3 — Reference types in signatures

Added `ty::Ref(_, pointee, mutbl)` handling to `rustc_ty_to_cxx`:
`&T` → `CxxType::Ref { kind: Lvalue, cv: { is_const: true } }`,
`&mut T` → same with `is_const: false`. This pairs with the
substitution table already in `rustc_abi_cxx::mangle` so repeated
types compress to `S_` / `S0_` / etc.

Verified against Clang on `/tmp/p09-refs/`:
`impl Widget { fn combine(&self, other: &Widget) -> u64 }` →
`_ZNK6Widget7combineERKS_` (matches
`uint64_t Widget::combine(const Widget& other) const`). Linked
with a C++ caller; returns 30 for `Widget{10}.combine(Widget{20})`.
Folded into `08-itanium-module.patch`.

### P09.5 — Module path → C++ namespace mangling

Added `cxx_namespace_path` helper in `itanium.rs` that walks
`tcx.opt_parent` from a `DefId` up to the crate root, collecting
`DefKind::Mod` ancestors as `NameSegment::Namespace` entries.
Applied in three places:

- Free `extern "C++"` fns — `CxxSymbol::Function::scope` is now
  the namespace path of the fn's owning module (was always empty).
- Inferred-C++ methods — class name path is prefixed with the
  namespace of where the class is defined.
- Record types in signatures — repr(cpp) ADTs in params/returns
  mangle with their owning module namespace.

Verified on `/tmp/p09-ns/`:

```rust
pub mod gfx { #[repr(cpp)] pub struct Widget { ... }
              impl Widget { pub fn area(&self) -> u64 { ... } } }
pub mod outer { pub mod inner {
    extern "C++" { pub fn global_fn(x: u64) -> u64; } } }
```

Mangling: `_ZNK3gfx6Widget4areaEv` and
`_ZN5outer5inner9global_fnEy` — matches Clang's output for the
equivalent C++ source with `namespace gfx { ... }` and
`namespace outer { namespace inner { ... } }`. End-to-end: Rust
calls C++-defined `outer::inner::global_fn(10)` and Rust-defined
`gfx::Widget{7}.area()` — returns 25 (14+11), binary exits 25.
Folded into `08-itanium-module.patch`. Workspace: 233/0.

### P09.4 — Auto-export inferred-C++ methods

In `rustc_codegen_ssa/src/codegen_attrs.rs::codegen_fn_attrs_provider`:
assoc fns on `impl`s of `#[repr(cpp)]` ADTs get
`CodegenFnAttrFlags::NO_MANGLE` implicitly. Purely a visibility
side-effect — the Itanium mangler still overrides the actual
symbol name via its earlier hook in `compute_symbol_name`. This
drops the `#[unsafe(no_mangle)]` requirement on every method.

```rust
#[repr(cpp)] pub struct Widget { pub handle: u64 }
impl Widget {
    // Before P09.4: needed #[unsafe(no_mangle)] to link from C++
    // After P09.4: auto-exported with Itanium mangling
    pub fn area(&self) -> u64 { self.handle * 2 }
    pub fn bump(&mut self, by: u64) { self.handle += by }
}
```

Verified: re-ran `/tmp/p09-methods/` probe with `no_mangle`
removed from both methods; `nm libprobe.a` shows `T __ZNK6Widget4areaEv`
and `T __ZN6Widget4bumpEy` (external, Itanium-mangled). C++-linked
demo still exits 42. Patch: `09-3-auto-export-cxx-methods.patch`.

### P09.6 — aarch64 Itanium ABI overlay (AAPCS + DarwinPCS)

`rustc_target/src/callconv/aarch64.rs`: added
`compute_cxx_abi_info` mirroring the x86_64 overlay. Runs the
standard aarch64 C-path classification (DarwinPCS on macOS,
AAPCS elsewhere), then promotes any `is_cxx_non_trivial_for_calls`
argument or return to indirect via `force_indirect`. Dispatched
from `callconv/mod.rs::adjust_for_foreign_abi` for non-Windows
aarch64 targets (Windows arm64 uses MSVC — out of scope, same
stance as Windows x64).

Verified via cross-compile on `/tmp/p09-tier2/aarch64-probe.rs`:

```llvm
@_Z11make_widgetv(ptr sret([8 x i8]) align 8 ...)
@_Z14consume_widget6Widget(ptr align 8 ...)
```

identical to `clang++ --target=arm64-apple-darwin`'s output
shape. Patch: `09-6-aarch64-cxx-abi.patch`. Runtime validation on
aarch64 hardware deferred to user.

### P09.7 — Hard diagnostics for unsupported C++ signature shapes

`rustc_symbol_mangling/src/itanium.rs`: `rustc_ty_to_cxx` now
returns `Result<TypeId, Ty<'tcx>>` where the `Err` variant carries
the offending type. `mangle` emits `tcx.dcx().span_err` pointing
at the fn definition with the type name. Applies equally to
explicit `extern "C++"` blocks and inferred-C++ methods.

Error text:

```
error: type `str` cannot appear in an `extern "C++"` signature
(or an inferred-C++ method on a `#[repr(cpp)]` type) — no
corresponding Itanium mangling is defined
 --> bad-sig.rs:7:5
```

Before P09.7 the mangler silently returned `None` and the default
Rust-mangled symbol flowed through, producing an opaque
`undefined symbol _RNvCs...` at link time with no hint at the
cause. Folded into `08-itanium-module.patch`.

### P09.8 — Arrays in C++ signatures

Added `ty::Array(elem, len)` handling in `rustc_ty_to_cxx`:
element type recursively lowered, length extracted via
`try_to_target_usize(tcx)`, emitted as `CxxType::Array`.

**Itanium array-CV normalization** (`rustc_abi_cxx/src/mangle.rs`):
per Itanium §5.1.5.2, cv-qualifiers can't attach to an array
type directly — they apply to the element. `&[u64; 4]` Rust →
`const uint64_t (&arr)[4]` C++ must mangle as `RA4_Ky` (ref →
array-of → const uint64), not `RKA4_y`. Added a special-case in
`emit_with_possible_cv` that pushes cv down into the array
element when the inner type is a `CxxType::Array`.

Verified: `fn checksum(&self, counts: &[u64; 4]) -> u64` mangles
as `_ZNK6Widget8checksumERA4_Ky`, identical to
`uint64_t Widget::checksum(const uint64_t (&counts)[4]) const`.
Folded into `08-itanium-module.patch` (compiler-side) and the
existing vendored `rustc_abi_cxx` (library-side; the fix also
lives at `rustcc/crates/rustc_abi_cxx/src/mangle.rs` for the
authoritative rustcc tree).

### P09.9 — `#[repr(cpp, I)]` enums (C++ `enum class E : I`)

Extended `ty::Adt` handling in `rustc_ty_to_cxx` to distinguish
enum ADTs: when `def.is_enum()` and `def.repr().cpp()` is true,
emit `CxxType::Enum { name, underlying, scoped: true }` where
`underlying` comes from `def.repr().int` (the explicit integer
repr hint). Maps `Integer::I{8,16,32,64,128}` to the `CxxType::Int`
with the sign from `IntegerType::Fixed`. Falls back to
`Err(ty)` for `IntegerType::Pointer` (target-dependent) and for
enums without any explicit int repr.

**Validation** (`rustc_passes/src/check_attr.rs::check_repr`):
tracks an `is_cpp` flag across the `#[repr(...)]` block. After
the loop, for `target == Target::Enum && is_cpp`:

1. If `int_reprs == 0` — hard error: `#[repr(cpp)]` enum requires
   an explicit integer hint like `#[repr(cpp, i32)]`.
2. If `!is_c_like_enum(item)` — hard error: C++ `enum class`
   cannot have tagged-union variants.

Also excluded `is_cpp` from the `CONFLICTING_REPR_HINTS` lint's
"c-like-enum with (C + int)" case, since `#[repr(cpp, I)]` is the
intended form, not a mistake.

Verified on `/tmp/p09-tier2/enums.rs`:
`#[repr(cpp, i32)] pub enum Color { Red = 1, Green = 2, Blue = 4 }`
plus `extern "C++" { fn paint(c: Color) -> u32; }` produces
`@_Z5paint5Color(i32)` — byte-identical to Clang's output for
`enum class Color : int32_t { ... }` with `uint32_t paint(Color c)`.
Error cases (data-carrying variant, missing int repr) both emit
pointed hard errors. Patch: `09-10-enum-validation.patch`, plus
itanium.rs folded into `08-itanium-module.patch`.

### P09.10 — Non-trivial `!Copy` records pass indirectly

Extended `TyAndLayout::is_cxx_non_trivial_for_calls` in
`rustc_middle/src/ty/layout.rs`: in addition to the Drop check,
returns true when the `#[repr(cpp)]` ADT is `!Copy`. Rust's
`Copy` bound is the closest approximation to C++'s "trivially
copyable" — any hand-rolled `Clone` or the absence of `Copy`
means C++ semantics require indirect passing.

Verified: `#[repr(cpp)] #[derive(Copy, Clone)] struct Trivial`
passes as `i64` (register); `#[repr(cpp)] struct NonTrivial`
(no Copy derive) passes as `ptr align 8` (indirect), matching
Clang's output for a C++ class with a user-defined copy ctor.
Patch: `09-12-non-trivial-copy.patch`.

### P09.11 — i686 (32-bit x86) Itanium ABI overlay

Added `compute_cxx_abi_info` in
`rustc_target/src/callconv/x86.rs` mirroring x86_64 and aarch64:
runs the stock x86 C-path classification, then promotes
non-trivial-for-calls args/returns to indirect via
`force_indirect`. Dispatched from `callconv/mod.rs` for
non-MSVC i686 targets (MSVC x86 has its own ABI — out of scope
along with Windows x64 / arm64). Patch:
`09-11-i686-cxx-abi.patch`.

### P09.12 — C++ ctor / dtor symbol emission

Full plumbing for `#[rustc_cxx_ctor]`:

- `rustc_span/src/symbol.rs` — `rustc_cxx_ctor` sym registered.
- `rustc_feature/src/builtin_attrs.rs` — attribute registered
  with `rustc_attr!`.
- `rustc_hir/src/attrs/data_structures.rs` — new
  `AttributeKind::RustcCxxCtor(Span)` variant.
- `rustc_hir/src/attrs/encode_cross_crate.rs` — encodes `No`
  (not exported across crates, consistent with related
  rustc-internal attrs).
- `rustc_attr_parsing/src/attributes/codegen_attrs.rs` —
  `RustcCxxCtorParser` implementing `NoArgsAttributeParser`,
  allowed on inherent methods only.
- `rustc_attr_parsing/src/context.rs` — parser registered.
- `rustc_passes/src/check_attr.rs` — attribute listed in the
  permitted-elsewhere block so lint passes don't reject it.

Ctor mangling in `itanium.rs::mangle`: when a method on a
`#[repr(cpp)]` impl carries `#[rustc_cxx_ctor]`, emit
`CxxSymbol::Ctor { class, variant: C1, sig }`. Ctors skip the
first-input (self) skip (there's no self param — `fn new(x: u32)
-> Self` has no self) and elide the return type (Itanium C++
ctors don't mangle their return).

Dtor mangling: a `Drop::drop` impl on a `#[repr(cpp)]` type is
detected via `is_cxx_drop_impl` — checks the impl is a trait
impl, the trait is `Drop` lang item, and the Self type is a
repr(cpp) ADT. Emits `CxxSymbol::Dtor { class, variant: D1 }`.
No attribute needed — every Drop impl on a repr(cpp) type
becomes the D1 (complete-object) destructor automatically.

```rust
#[repr(cpp)] pub struct Widget { pub handle: u64 }

impl Widget {
    #[rustc_cxx_ctor]
    pub fn new(h: u64) -> Self { Widget { handle: h } }
}

impl Drop for Widget { fn drop(&mut self) {} }
```

→ Rust emits `_ZN6WidgetC1Ey(sret, i64)` and
`_ZN6WidgetD1Ev(ptr)`. Matches Clang's output for
`Widget::Widget(uint64_t)` and `Widget::~Widget()`.

**End-to-end verified**: `/tmp/p09-tier3/ctor-dtor.rs` (Rust
side) + `/tmp/p09-tier3/cpp-uses-rust.cpp` (C++ constructs
`Widget(42)` in a C-linkage fn, returns `handle`) link and run;
demo exits 42. Patch: `09-13-rustc-cxx-ctor-attr.patch`, plus
itanium.rs updates folded into `08-itanium-module.patch`.

**Tier 3 scope notes**: vtables / virtual dispatch and Windows
MSVC ABI are explicitly deferred. The MSVC mangler uses a
different scheme (`?` prefix) and would need a new mangler
alongside Itanium, a project in itself.

### P09.13 — Single-inheritance virtual dispatch (validated, no patches)

Spike at `/tmp/p09-vtable/` proves that our existing fork —
with Tier 1/2/3 patches already in place — supports the scope
**(Rust-overrides-C++-virtual, single inheritance, no RTTI, C++
emits vtable, trait-impl pattern)** without additional compiler
changes. Verified end-to-end:

- **C++ side**: declares `class Observer { virtual uint64_t notify(uint64_t) = 0; virtual ~Observer() = default; };` and `class MyObserver : public Observer { ... };` with an out-of-line dtor as the "key function" so Clang emits MyObserver's vtable in the C++ TU.
- **Rust side**: `#[repr(cpp)] struct MyObserver { _base: Observer, value: u64 }` + `trait BaseOps { fn notify(&mut self, x: u64) -> u64 }` + `impl BaseOps for MyObserver { fn notify(&mut self, x: u64) -> u64 { self.value * x } }`.
- **Mangling**: Rust's method emits as `_ZN10MyObserver6notifyEy` — exactly what Clang's vtable slot references.
- **Runtime**: C++ `MyObserver obs(10); Observer* p = &obs; p->notify(5);` dispatches through the vtable, lands in Rust's impl, returns 50. Demo exits 50.

Two known caveats (out of scope for the spike):

1. **Key-function quirk on C++ side**: the derived class must have
   at least one non-inline virtual with a definition in C++ (we
   used an out-of-line empty dtor). Otherwise Clang won't emit
   the vtable. This is a Clang/Itanium requirement, not a
   rustcc limitation.
2. **Destructor variants D0/D2**: if Rust's Drop needs to run on
   scope exit or `delete`, we need D0 (deleting) and D2 (base)
   emission alongside D1. Currently Rust only emits D1 via the
   `Drop::drop` path; the vtable's D0 slot would have to be
   supplied by Clang's generated dtor (which won't call Rust's
   Drop). This is the next piece of vtable work.

No new patches. This section documents the fork's already-working
scope for single-inheritance polymorphism.

### P09.14 — Swift ABI plumbing (Phase 1a: `extern "Swift"` + `#[link_name]`)

Added `ExternAbi::Swift { unwind }` variant mirroring the
`ExternAbi::Cpp` pattern, including:

- `ExternAbi::Swift` variant in `rustc_abi/src/extern_abi.rs` with
  parser entry for `"Swift"` and `"Swift-unwind"`.
- `CanonAbi::Swift` variant in `rustc_abi/src/canon_abi.rs`; treated
  identically to `CanonAbi::C` for LLVM calling convention today.
- `ExternAbi::Swift { .. } → CanonAbi::Swift` mapping in
  `rustc_target/src/spec/abi_map.rs`.
- Exhaustive-match arms in: `rustc_ast_lowering/src/stability.rs`,
  `rustc_ast_passes/src/ast_validation.rs`,
  `rustc_hir_typeck/src/callee.rs`,
  `rustc_codegen_llvm/src/abi.rs`,
  `rustc_middle/src/ty/layout.rs`,
  `rustc_public/src/unstable/convert/stable/{ty,abi}.rs`.

Users can write:

```rust
unsafe extern "Swift" {
    #[link_name = "$s5MyLib3addyS2i_SitF"]
    fn swift_add(a: i64, b: i64) -> i64;
}
```

and Rust will call into the Swift function using the C calling
convention (correct for trivially-copyable scalars).

**Verified** on `/tmp/p09-swift-abi/`: `libMyLib.a` emits
`$s5MyLib3addyS2i_SitF` and `$s5MyLib5scale_2byS2d_SdtF`; Rust's
`extern "Swift" { ... }` with `#[link_name]` hooks up at link
time; `swift_add(20, 22) = 42` and `swift_scale(3.5, 2.0) = 7.0`.

Patch: `09-14-swift-abi-plumbing.patch`.

**Phase 1b (next)**: auto-mangler that eliminates the manual
`#[link_name]`. Requires implementing the Swift mangling grammar
for free functions with scalar args, taking the module name from
a `#[swift_symbol(module = "...")]` attribute. Swift's grammar
for functions is:

- `$s` — start marker
- `<module-len><module>` — defining module
- `<name-len><name>` — function name
- `y` — no generic signature
- `<params-tuple><result>t<fn-suffix>F` — type info

For scalar subsets, the mapping is: `Int → Si`, `Double → Sd`,
`Float → Sf`, `Bool → Sb`, `(T, T) → S2<t>` (compressed repetition),
`(T, U) → St_Su` (distinct types). Tested mapping:

| Rust sig | Swift source | Mangled |
|---|---|---|
| `fn(i64, i64) -> i64` | `func add(_ a: Int, _ b: Int) -> Int` | `$s<m>3addyS2i_SitF` |
| `fn(f64, f64) -> f64` | `func scale(_ x: Double, by k: Double) -> Double` | `$s<m>5scale_2byS2d_SdtF` |

Note the `_2by` segment in the second example — Swift encodes
parameter labels. `_` means unnamed, `<n><label>` means a named
parameter. For Phase 1b we'd need a Rust-side way to express
labels (probably `#[swift_symbol(labels = "_, by")]` or derived
from arg names).

**Phase 2**: full VWT-aware support for non-trivial Swift value
types. Requires reading the type metadata via `$s...Ma`, accessing
the value witness table at metadata offset `-1`, calling through
VWT function pointers for copy/destroy. Roughly 2–3 weeks of work
on top of the mangler.

### P09.15 — C++ destructor variants D0 and D2

Extended Rust-side codegen to emit the Itanium **D0** (deleting)
and **D2** (base-object) destructor variants as trampolines
alongside the existing **D1** (complete-object) destructor that
`impl Drop for T` already produces. With all three variants
emitted, a C++ `delete p` call through a base-class pointer
dispatches through the derived class's vtable and lands in Rust's
`Drop::drop`.

**Changes**:

- `rustc_symbol_mangling/src/itanium.rs` — factored the dtor
  mangling out of `mangle` into a reusable
  `mangle_dtor_variant(tcx, def_id, DtorVariant)` with public
  wrappers `mangle_dtor_d0` and `mangle_dtor_d2`. Also made
  `is_cxx_drop_impl` public so codegen can detect the case.
  `itanium` module promoted from `mod` to `pub mod`.
- `rustc_codegen_llvm/src/mono_item.rs` — added
  `maybe_add_cxx_dtor_variants` called after `predefine_fn` for
  any `Drop::drop` instance on a `#[repr(cpp)]` type. Emits:
  - **D2**: trampoline `{ call D1(this); ret }`
  - **D0**: trampoline `{ call D1(this); call operator_delete(this); ret }` where `operator_delete` is `_ZdlPv` (declared extern and resolved against libc++/libstdc++).

**Verified end-to-end** on `/tmp/p09-vtable/demo2`:

```
Observer* p = new MyObserver(10);
uint64_t r = p->notify(5);        // virtual dispatch → Rust notify = 50
delete p;                         // virtual dispatch → Rust D0 → D1 → Drop → operator delete
```

After `delete`, `rust_drop_count == 1` (Rust's `Drop::drop`
incremented it). Packed return `50 * 1000 + 1 = 50001`. Demo
exits 42 (success encoding). `nm` confirms all three dtor
symbols are external:

```
T __ZN10MyObserverD0Ev
T __ZN10MyObserverD1Ev
T __ZN10MyObserverD2Ev
```

**Caveat**: the C++ side still needs a "key function" (a
non-inline virtual with a C++ body) to force vtable emission.
In our probe we added `virtual uint64_t version() const` to
MyObserver for that purpose. This is a Clang/Itanium-level
requirement, not a rustcc limitation. The spike documentation
notes this in `getting-started.html`.

Patch: `09-15-cxx-dtor-d0-d2.patch`.

### P09.16 — Swift Phase 1b auto-mangler (scoped, not implemented)

Scoping follow-up to P09.14. Phase 1b eliminates the manual
`#[link_name]` dance for common Swift function calls.

**Work needed**:

1. **Attribute plumbing** (~150 LOC — mirrors `rustc_cxx_ctor`):
   - `rustc_span/src/symbol.rs`: `rustc_swift_symbol`
   - `rustc_feature/src/builtin_attrs.rs`: register
   - `rustc_hir/src/attrs/data_structures.rs`:
     `AttributeKind::RustcSwiftSymbol { module, name, labels }`
     (takes data, not a unit variant — uses `SingleAttributeParser`
     with arg parsing rather than `NoArgsAttributeParser`)
   - Parser in `rustc_attr_parsing/src/attributes/`
   - Exhaustive-match fallouts in `encode_cross_crate.rs`,
     `check_attr.rs`

2. **Swift mangler** (~400 LOC in a new `swift.rs` module):
   - Scalar type mapping: `Si`, `Su`, `Sd`, `Sf`, `Sb`,
     `s4Int8V`, `s5Int16V`, `s5Int32V`, `s5Int64V`, `s5UInt8V`,
     `s6UInt16V`, `s6UInt32V`, `s6UInt64V`
   - Compressed consecutive types: `S2i` for (Int, Int)
   - Parameter tuple with `_` separator + `t` terminator
   - Function terminator: `F`
   - Module-qualified entity prefix: `$s<len><module>`
   - Empty generic signature marker: `y`
   - Label encoding: `_` for unnamed, `<len><name>` for named

3. **Dispatch** in `compute_symbol_name`:
   - Similar structure to the existing `itanium::mangle` hook
   - `is_swift_abi(tcx, def_id)` predicate
   - Route to `swift::mangle` before falling through to the
     default mangling

**Known grammar ambiguity** (unresolved without reading Swift
source):

For `func add(_ a: Int, _ b: Int) -> Int` in `MyLib`, Swift
produces `$s5MyLib3addyS2i_SitF`. The segmentation of
`S2i_SitF` into "params type" and "result type" is not obvious
from the mangled name alone:

- Option A: `S2i` = (Int, Int) compressed params, `_Si` =
  ReturnType separator + Int, `t` = tuple terminator, `F` =
  function terminator.
- Option B: `Si` = first return type, `S2i_Si` = `S<n><t>` with
  `_` as between-elem separator (3-tuple of Ints).
- Option C: something else involving the `y` marker consuming
  label slots.

Needs to cross-check against `swift/lib/Demangling/Demangler.cpp`
before committing to one encoding. Mangling-by-guess produces
symbols that almost-but-don't match Swift's output, which is
silently worse than having no mangler at all.

**Estimated effort (once grammar resolved)**: 2–3 days of
focused work. **Deferred** — documented here for a future pass
with access to the Swift source tree.

**Update — grammar resolved empirically**, implementation shipped
below as Phase 1b. Approach: compile a battery of Swift functions
(nullary, unary, binary-unnamed, binary-named, ternary, tuple
return, Void return, all scalar types) with `swiftc
-module-name G -emit-library -static probe.swift`, extract symbols
via `nm`, cross-reference with `swift-demangle`. Derived the
following grammar:

```
MangledSymbol := $s <Module> <Name> <Labels>? <Sig> F
Module        := <Len><Chars>                        e.g. 5MyLib
Name          := <Len><Chars>
Labels        := y                                    // all unnamed
                | LabelSpec+                          // one per param
LabelSpec     := _ | <Len><Chars>
Sig           := <RetType> <Params>
RetType       := <Type>                               // y = Void
Params        := y                                    // ()
                | <Type>                              // single, no tuple
                | <T1>_<T2>[_<T3>...]t                // 2+ tuple
Type          := Si | Su | Sd | Sf | Sb | y | ...

Compression: consecutive identical single-letter Swift-stdlib
types collapse to S<N><letter>; the compression crosses the
RetType/Params boundary (e.g. `(Int,Int) -> Int` emits
`S2i_SitF` — return `Si` + first param `Si` collapse).

Labels are omitted when params is empty; emitted between name
and signature otherwise.
```

### P09.16 (implementation) — Swift auto-mangler

Ships Phase 1b. Users write:

```rust
unsafe extern "Swift" {
    #[rustc_swift_symbol = "MyLib"]
    fn add(a: i64, b: i64) -> i64;
}
```

and the link name mangles automatically as `$s5MyLib3addyS2i_SitF`
— byte-for-byte identical to what `swiftc -module-name MyLib`
emits for `public func add(_ a: Int, _ b: Int) -> Int`.

**Changes**:

- `rustc_span/src/symbol.rs` — `rustc_swift_symbol` sym.
- `rustc_feature/src/builtin_attrs.rs` — attribute registered.
- `rustc_hir/src/attrs/data_structures.rs` —
  `AttributeKind::RustcSwiftSymbol { module: Symbol, span: Span }`
  with the Swift module name as carried data.
- `rustc_hir/src/attrs/encode_cross_crate.rs` — encodes `Yes`
  (the attribute's module name affects the symbol name and must
  survive across crates for downstream users to link correctly).
- `rustc_attr_parsing/src/attributes/codegen_attrs.rs` —
  `RustcSwiftSymbolParser` implementing `SingleAttributeParser`
  (has-args parser since it takes `module = "..."`), allowed
  only on `ForeignFn`.
- `rustc_attr_parsing/src/context.rs` — parser registered.
- `rustc_passes/src/check_attr.rs` — attribute allowed-list arm.
- `rustc_symbol_mangling/src/swift.rs` — new module.
  `is_swift_abi` predicate gates routing; `mangle` builds the
  `$s<module><name>y<sig>F` form; `compress_consecutive` folds
  runs of identical stdlib types per the Itanium-style
  compression rule.
- `rustc_symbol_mangling/src/lib.rs` — dispatch branch in
  `compute_symbol_name` (after the itanium dispatch, before the
  default path).

**Supported types (Phase 1b)**:

| Rust   | Swift     | Code |
|--------|-----------|------|
| `i64`  | `Int`     | `Si` |
| `u64`  | `UInt`    | `Su` |
| `f64`  | `Double`  | `Sd` |
| `f32`  | `Float`   | `Sf` |
| `bool` | `Bool`    | `Sb` |
| `()`   | `Void`    | `y`  |

Fixed-width ints (`Int8`/`Int16`/`Int32`/`UInt8`/...) require
back-reference mangling (types after the first occurrence use
`A<N>` references); not in Phase 1b. Users with those types fall
back to Phase 1a's `#[link_name = "..."]`.

**Verified end-to-end** on `/tmp/p09-swift-auto/demo`:

```
add = 42, scale = 7, u1 = 7
```

Three symbols auto-mangled, all linked against a Swift-compiled
`libMyLib.a`. `nm` comparison against Swift's output: identical
symbol strings for all three functions. Workspace 233/0.

Patch: `09-16-swift-mangler.patch`.

**Phase 2 (still deferred)**: VWT-aware support for non-trivial
Swift value types. Would need the mangler to encode user-defined
Swift types with back-references, plus runtime machinery to copy
/destroy via `$s...Ma` metadata accessors and value-witness
tables. ~2–3 weeks of work — the mangler piece is now unblocked
by Phase 1b's grammar work.

### P09.17 — Swift Phase 2a: user types, `#[repr(swift)]`, swiftcc

Unblocks calling Swift functions that take or return user-defined
POD value types (Swift structs with only scalar/POD fields) from
Rust without a C++ shim.

**Changes**:

- **`#[repr(swift)]`** — new repr variant, mirroring
  `#[repr(cpp)]`. Marks a Rust type as representing a Swift
  value type. `ReprFlags` widened from `u8` to `u16` to
  accommodate the new `IS_SWIFT` bit. Layout in Phase 2a
  follows `repr(C)`; Phase 2b will route through Swift metadata.
  Touches: `rustc_span/src/symbol.rs` (sym::swift),
  `rustc_hir/src/attrs/data_structures.rs`
  (ReprAttr::ReprSwift), `rustc_attr_parsing/src/attributes/repr.rs`
  (parser arm), `rustc_abi/src/lib.rs`
  (IS_SWIFT flag + `ReprOptions::swift()`),
  `rustc_middle/src/ty/mod.rs` (ReprSwift → IS_SWIFT | IS_C),
  `rustc_passes/src/check_attr.rs` (validation arm).

- **`#[rustc_swift_type = "Module.Name"]`** — binds a Rust
  struct to a Swift qualified type name. Required alongside
  `#[repr(swift)]` for a type to be usable in `extern "Swift"`
  signatures.
  New `AttributeKind::RustcSwiftType { module, name, span }` +
  parser + cross-crate encode + check-attr allow-list.

- **`#[rustc_swift_labels = "_, by, from"]`** — per-param Swift
  labels for an `extern "Swift"` fn (comma-separated). `_` for
  unnamed, anything else is a named label. Omit the attribute
  or pass all-`_` for the Phase 1b `y`-shorthand.

- **Mangler extensions** (`rustc_symbol_mangling/src/swift.rs`):
  - `SwiftType::UserValue(module, name)` variant for user types.
  - Substitution table tracks user types as they appear; first
    occurrence emits `AA<len><name>V`, subsequent occurrences
    emit `A<letter>` back-ref (value-type slot = idx 3 for
    first user type, idx 6 for second, etc.; letters A-Z).
  - Scalars are pre-registered Swift shortcuts and do NOT
    consume substitution slots — they emit in full each time
    (with scalar-compression for consecutive runs via
    `compress_consecutive`).
  - Label encoding: `encode_labels` emits `_` or `<len><name>`
    per comma-separated token.

- **Swift calling convention** in LLVM backend
  (`rustc_codegen_llvm/src/abi.rs` + `llvm/ffi.rs`):
  - Added `CallConv::SwiftCallConv = 16` (matches LLVM's
    `CallingConv::Swift`).
  - Routed `CanonAbi::Swift` to `llvm::SwiftCallConv` (was
    previously `CCallConv` — caused struct-packing mismatch).

- **Swift ABI overlay** (`rustc_target/src/callconv/x86_64.rs`):
  `compute_swift_abi_info` runs the C-path lowering first, then
  for any `#[repr(swift)]` arg/return whose layout is
  `ScalarPair`, resets `PassMode::Pair` via a new
  `ArgAbi::force_swift_pair` helper. This prevents the C path
  from packing two-field POD structs into a single wide scalar
  (which Swift's CC doesn't understand — Swift spreads each
  scalar into its own register). Dispatched from
  `callconv/mod.rs::adjust_for_foreign_abi` for
  `ExternAbi::Swift` on x86_64.

- **Layout infrastructure**
  (`rustc_abi/src/layout/ty.rs` +
  `rustc_middle/src/ty/layout.rs`): new
  `TyAbiInterface::is_swift_repr` method on
  `TyAndLayout` returning `def.repr().swift()`.

**Verified end-to-end** (`/tmp/p09-repr-swift/`):

```rust
#[repr(swift)]
#[rustc_swift_type = "MyLib.Point"]
pub struct Point { pub x: f32, pub y: f32 }

unsafe extern "Swift" {
    #[rustc_swift_symbol = "MyLib"]
    fn magnitude(p: Point) -> f32;
    #[rustc_swift_symbol = "MyLib"]
    fn make_point(x: f32, y: f32) -> Point;
}
```

Mangled symbols: `$s5MyLib9magnitudeySfAA5PointVF` and
`$s5MyLib10make_pointyAA5PointVSf_SftF` — byte-identical to
`swiftc -module-name MyLib`'s output.

LLVM IR: `swiftcc { float, float } @...(float 3.0, float 4.0)`
for `make_point` (fields spread), `swiftcc float @...(float,
float)` for `magnitude` (POD struct spread into scalars).

Runtime: `magnitude(make_point(3, 4)) = 5`, demo exits 42.

Patch: `09-17-swift-phase2a.patch`. Workspace: 233/0.

**Phase 2b (deferred)**: VWT-aware support for non-trivial
Swift types (types with `deinit` or non-POD fields). Requires:
- Metadata accessor calls (`$s...Ma`) at runtime
- Value-witness-table indirection for copy/destroy
- Indirect-pass convention for non-trivial values
- Run-length compression of substitution back-refs for
  signatures with 3+ consecutive identical user types

### P09.18 — Phase 2b.1: Swift VWT runtime helpers (library path)

Ships a runtime helper crate at `crates/rustcc_swift_rt/`
(excluded from the workspace because it uses the fork's
`extern "Swift"` ABI that stable rustc rejects). Provides:

- `MetadataResponse`, `Metadata`, `ValueWitnessTable` struct
  definitions matching Swift's runtime layout
- `drop_swift_value<T>` / `clone_swift_value<T>` helpers that
  dispatch through the VWT (currently **segfaults** on
  destroy for types with class fields on x86_64-apple-darwin —
  ABI interaction between `extern "Swift"` fn pointers and
  swiftcc-declared VWT functions under debug)
- Documentation of the **working outlined-destroy pattern**:
  declare `$s<module><Type>VWOh` as extern "Swift" and call it
  directly from `Drop::drop`. Swift emits a per-type outlined
  helper that knows the layout statically and releases fields
  without needing runtime metadata lookup.

**Verified end-to-end** at `/tmp/p09-swift-nt/`: three Rust-side
`NT` drops (where `NT` is a Swift struct holding a class
reference) trigger three `Inner.deinit` calls in Swift →
deinit_count=3, demo exits 42.

**Phase 2b.2 (follow-up)**: debug the VWT-direct path for
types with class fields, then implement compiler-side
auto-synthesis of Drop/Clone so `#[repr(swift)]` alone implies
the non-trivial lifecycle impls.

### P09.19 — Swift back-ref run-length compression

Extended `rustc_symbol_mangling/src/swift.rs` with
`compress_backref_runs`: runs of 3+ consecutive identical
`A<letter>` back-refs collapse to `<first>_A<N><letter>`. The
first instance stays standalone, remaining N ≥ 2 identical
back-refs compress into one token with no internal `_`
separators. Pairs (`AX_AX`) don't compress because `A1X` is
the same length.

**Verified** on `/tmp/p09-swift-rle/`:

```swift
public func combine4(_ a: Point, _ b: Point, _ c: Point, _ d: Point) -> Point
```

Swift emits `$s5MyLib8combine4yAA5PointVAD_A3DtF`. With P09.19
our Rust-side auto-mangler produces the same symbol byte-for-byte
for `fn combine4(a: Point, b: Point, c: Point, d: Point) -> Point`
in an `extern "Swift"` block. `rust_entry` calls
`combine4(p, p, p, p)` with `p={1.0, 2.0}` and receives back
`{4.0, 8.0}`; demo exits 42.

Folded into `09-16-swift-mangler-module.patch` (the swift.rs
module is re-emitted with the compression helper + 6 new
`compress_backref_runs` unit tests). Workspace: 235/0.

### P09.20 — C++ operator overloading

Extended Rust-to-C++ method mangling to support operator
overloads via `#[rustc_cxx_operator = "Kind"]` on methods of
`#[repr(cpp)]` impls. The kind string names which operator:

| Attribute value | Itanium code | C++ operator |
|---|---|---|
| `"Plus"` | `pl` | `operator+` |
| `"Minus"` | `mi` | `operator-` |
| `"Mul"` | `ml` | `operator*` |
| `"Div"` | `dv` | `operator/` |
| `"Mod"` | `rm` | `operator%` |
| `"Eq"` | `eq` | `operator==` |
| `"Ne"` | `ne` | `operator!=` |
| `"Lt"` | `lt` | `operator<` |
| `"Le"` | `le` | `operator<=` |
| `"Gt"` | `gt` | `operator>` |
| `"Ge"` | `ge` | `operator>=` |
| `"Assign"` | `aS` | `operator=` |
| `"PlusAssign"` | `pL` | `operator+=` |
| `"Index"` | `ix` | `operator[]` |
| `"Deref"` | `dr` | `operator*` (unary) |
| `"Call"` | `cl` | `operator()` |
| `"PreIncr"` | `pp` | `operator++` |
| `"PreDecr"` | `mm` | `operator--` |

**Changes**:

- `rustc_span/src/symbol.rs`: `rustc_cxx_operator` sym.
- `rustc_feature/src/builtin_attrs.rs`: attribute registered.
- `rustc_hir/src/attrs/data_structures.rs`:
  `AttributeKind::RustcCxxOperator { kind: Symbol, span: Span }`.
- `rustc_hir/src/attrs/encode_cross_crate.rs`: encodes `Yes`
  (the kind affects the emitted symbol name and must travel
  across crates).
- `rustc_attr_parsing/src/attributes/codegen_attrs.rs`:
  `RustcCxxOperatorParser` implementing `SingleAttributeParser`
  with `NameValueStr` template. Allowed only on
  `Target::Method(MethodKind::Inherent)`.
- `rustc_attr_parsing/src/context.rs`: parser registered.
- `rustc_passes/src/check_attr.rs`: allow-list arm added.
- `rustc_symbol_mangling/src/itanium.rs`: new
  `cxx_operator_kind` helper parses the attribute's kind string
  into an `OperatorKind` from `rustc_abi_cxx`. When present on
  an impl method, `mangle` emits `CxxSymbol::Method` with
  `MethodName::Operator(kind)` instead of the named-method form.

**Verified on `/tmp/p09-cxx-op/`**:

```rust
#[repr(cpp)] pub struct Point { pub x: i32, pub y: i32 }
impl Point {
    #[rustc_cxx_operator = "Plus"]
    pub fn op_plus(&self, other: &Point) -> Point { ... }
    #[rustc_cxx_operator = "Eq"]
    pub fn op_eq(&self, other: &Point) -> bool { ... }
    #[rustc_cxx_operator = "Lt"]
    pub fn op_lt(&self, other: &Point) -> bool { ... }
    #[rustc_cxx_operator = "Minus"]
    pub fn op_minus(&self, other: &Point) -> Point { ... }
    #[rustc_cxx_operator = "Index"]
    pub fn op_index(&self, i: u32) -> i32 { ... }
}
```

Five symbols match Clang's output for the equivalent C++
operator overloads byte-for-byte (including Itanium substitution
`S_` for repeated `Point`). Patch: `09-20-cxx-operators.patch`,
plus itanium.rs folded into `08-itanium-module.patch`.
Workspace: 235/0.

### P09.21 — Swift class kind + ARC helpers

Extended `#[rustc_swift_type]` to accept a `:class` suffix
(e.g. `"MyLib.Counter:class"`) indicating a Swift reference-type
class. Mangler emits `AA<len><name>C` (`C` kind) instead of
`AA<len><name>V` (`V` kind) when the attribute specifies class.

**Changes**:

- `rustc_hir/src/attrs/data_structures.rs`:
  `AttributeKind::RustcSwiftType` gained an `is_class: bool`
  field.
- `rustc_attr_parsing/src/attributes/codegen_attrs.rs`:
  `RustcSwiftTypeParser` parses `"Module.Name:class"` or
  `"Module.Name:struct"`, defaulting to struct. Unknown suffixes
  after `:` fall through to default kind with the whole string
  treated as `Module.Name`.
- `rustc_symbol_mangling/src/swift.rs`:
  `SwiftType::UserClass(module, name)` variant added alongside
  `UserValue`. Both match the same emit path; only the final
  kind character differs (`C` vs `V`).
- Bug fix: `mangle` no longer emits the `y` labels marker when
  the fn has zero params. Swift elides labels in that case.
  Previously `make_counter() -> Counter` mangled as
  `$s5MyLib12make_counter**y**AA7CounterCyF` (extra `y`); now
  matches Swift's `$s5MyLib12make_counterAA7CounterCyF`.
- `crates/rustcc_swift_rt/src/lib.rs` (workspace-side, not fork
  patch): added `swift_retain` / `swift_release` extern
  declarations + `retain_swift_class` / `release_swift_class`
  convenience wrappers. These are extern "C" because the Swift
  stdlib exposes them with C linkage for interop consumers.

**Usage pattern for Phase 2c-previw (manual form)**:

```rust
use rustcc_swift_rt::swift_release;

#[repr(swift)]
#[rustc_swift_type = "MyLib.Counter:class"]
pub struct Counter {
    __ptr: *mut core::ffi::c_void,
}

impl Drop for Counter {
    fn drop(&mut self) {
        if !self.__ptr.is_null() {
            unsafe { swift_release(self.__ptr); }
        }
    }
}
```

Phase 2c will compiler-synthesize the Drop + Clone impls so
`#[rustc_swift_type = "...:class"]` alone is enough; for now
the ~6-line boilerplate is explicit.

**Verified on `/tmp/p09-swift-class/`**: three Swift class
instances created via `make_counter()` are released on Rust-side
Drop; all three deinits fire → count=3, demo exits 42.

Mangling match:
- Rust produces `$s5MyLib12make_counterAA7CounterCyF` —
  byte-identical to `swiftc`'s output.
- Rust produces `$s5MyLib12counter_readySiAA7CounterCF` for
  `fn counter_read(c: Counter) -> i64` — matches Swift.

Folded into `09-16-swift-mangler-module.patch`. Workspace 235/0.

### P09.22 — Foreign-fn ctor mangling + `#[rustc_cxx_wrapper]`

Delivers the `#5` ctor-emission unification half of the
"parser-level `class` + unified ctor/dtor" backlog pair. The
`#2` parser-level `class` keyword is deferred (see below);
this patch handles the compiler-side work that `#2` would
have leaned on.

**What it does**:

1. `#[rustc_cxx_ctor]` is now legal on a foreign fn in an
   `extern "C++"` block. The Itanium mangler reads the class
   name off the return type (which must be a `#[repr(cpp)]`
   ADT) and emits `_ZN<class>C1E<args>`. Output is
   byte-identical to Clang for the equivalent
   `Widget::Widget(args)` declaration.

2. New `#[rustc_cxx_wrapper]` attribute opts an inherent
   method on a `#[repr(cpp)]` type OUT of P09.3's auto-export
   path. Thin forwarders that call through to an
   `extern "C++"` decl of the same method would otherwise
   collide with the C++ side's definition (dup-symbol) or
   recurse into themselves (P09.3 auto-exports the wrapper
   body under the same Itanium name as the extern is trying
   to resolve).

3. `compute_symbol_name` now checks `attrs.symbol_name` BEFORE
   falling into the Itanium auto-mangler for `extern "C++"`
   items. Prior ordering silently swallowed `#[link_name]`
   overrides on C++ foreign fns — a bug that didn't bite until
   the `cxx_class_native!` wrapper-method pattern exposed it.

**Attribute plumbing (new `rustc_cxx_wrapper` attribute)**:

- `rustc_span/src/symbol.rs`: new `rustc_cxx_wrapper` sym.
- `rustc_feature/src/builtin_attrs.rs`: `rustc_attr!` entry.
- `rustc_hir/src/attrs/data_structures.rs`:
  `AttributeKind::RustcCxxWrapper(Span)` variant.
- `rustc_hir/src/attrs/encode_cross_crate.rs`: `No`.
- `rustc_attr_parsing/src/attributes/codegen_attrs.rs`:
  `RustcCxxWrapperParser` (`Target::Method(Inherent)`).
- `rustc_attr_parsing/src/context.rs`: registered.
- `rustc_passes/src/check_attr.rs`: allow-list.

**Codegen / mangling changes**:

- `rustc_attr_parsing/.../codegen_attrs.rs`:
  `RustcCxxCtorParser` gains `Allow(Target::ForeignFn)` in its
  allow list.
- `rustc_codegen_ssa/src/codegen_attrs.rs`: the P09.3 auto-
  export block skips methods tagged `#[rustc_cxx_wrapper]`.
- `rustc_symbol_mangling/src/itanium.rs`:
  - New `cxx_foreign_ctor_class` helper — detects foreign fns
    with `#[rustc_cxx_ctor]` and returns the class ADT from
    the return type.
  - `mangle`: `class_adt` is now
    `impl_class.or(foreign_ctor_class)`; `is_ctor` ORs the
    foreign path. CV qualifier computation gated on
    `!is_ctor` (ctors have no `this` param to inspect).
  - `is_cpp_abi` skips `rustc_cxx_wrapper`-tagged methods so
    they fall through to the default Rust mangler.
- `rustc_symbol_mangling/src/lib.rs`: `attrs.symbol_name`
  (set by `#[link_name]` / `#[export_name]`) now wins over
  the Itanium mangler in the `is_cpp_abi` block.

**Macro update (`crates/rustcc_macros/src/lib.rs`)**:

- New function-like macro `cxx_class_native!`. Same input
  syntax as `cxx_class!`, but emits:
  - `#[repr(cpp)]` struct (not `#[repr(C)]`).
  - `unsafe extern "C++" { ... }` block (not `extern "C"`).
  - Ctors: `#[rustc_cxx_ctor] fn new(args) -> Self;` — no
    sret trampoline, no `#[link_name]`.
  - Methods: `#[link_name = "_ZN..."]` on extern decl (until
    a follow-up adds per-method ctor-style compiler support)
    plus `#[rustc_cxx_wrapper] #[inline(always)]` on the
    inherent impl wrapper so P09.3 doesn't auto-export it.
  - No auto-generated `impl Drop`. Types are opaque handles;
    callers manage lifetime via `ManuallyDrop` or an explicit
    release method. Auto-Drop for the native path would need
    an opt-out attribute on `Drop::drop` (not an inherent
    method); deferred.

Because the macro emits `#[repr(cpp)]` + `extern "C++"`, the
native variant **only compiles on the rustcc fork**. Stable-rustc
users keep `cxx_class!` which emits the ABI-compatible
`#[repr(C)]` + `extern "C"` + manual `#[link_name]` form.

**Verified on `/tmp/p09-22-foreign-ctor/` and `/tmp/p09-22-native-macro/`**:

- Foreign-ctor probe: `#[rustc_cxx_ctor] fn new(v: i32) -> Widget;`
  mangles to `__ZN6WidgetC1Ei`, links against
  `clang++ widget.cpp`, `rust_entry()` returns 42.
- Native-macro probe:
  ```rust
  cxx_class_native! {
      #[size = 16] pub class Texture {
          #[ctor] fn new(w: u64, h: u64) -> Self;
          fn area(&self) -> u64;
          fn scale(&mut self, by: u64);
      }
  }
  ```
  produces symbols `__ZN7TextureC1Eyy`, `__ZNK7Texture4areaEv`,
  `__ZN7Texture5scaleEy` — all undefined on the Rust side,
  all resolved by `clang++` on the C++ side. Program prints
  `C++ Texture::Texture(4, 5)`, area=20, area=180 (after
  scale by 3), exits 0.

Patch: `09-22-native-ctor-path.patch` (249 lines, narrative
form — authoritative state is the cumulative `.rs` files).
Workspace: 235/0.

### P09.23 — Dtor unification: `#[rustc_cxx_drop_wrapper]` + auto-Drop for `cxx_class_native!`

Closes out the dtor half of the #5 unification work deferred by
P09.22. Until now, `cxx_class_native!` emitted no `impl Drop`:
users wrapped the handle in `ManuallyDrop` or called a manual
`release()`, because P09.15's auto-emission of the Itanium
D0/D1/D2 destructor variants would collide with the C++ side's
own destructor symbols at link time.

**What it does**:

1. New `#[rustc_cxx_drop_wrapper]` attribute — the trait-impl
   analogue of `#[rustc_cxx_wrapper]`. Applied to `Drop::drop`
   in an `impl Drop for T` where `T` is `#[repr(cpp)]`, it opts
   that method out of three compiler paths that would otherwise
   auto-export the drop body as a C++ destructor:
   - `rustc_symbol_mangling::itanium::is_cpp_abi` — so the Rust
     `drop` body is NOT mangled as `_ZN<class>D1Ev`.
   - `rustc_codegen_llvm::mono_item::maybe_add_cxx_dtor_variants`
     — so P09.15's D0/D2 trampolines are not emitted.
   - `rustc_codegen_ssa::codegen_attrs` — so `NO_MANGLE` is not
     set on the drop method (it uses ordinary Rust mangling).

2. `cxx_class_native!` now auto-generates `impl Drop for T`
   with `#[rustc_cxx_drop_wrapper]` on the drop method. The body
   is a thin forwarder: `unsafe { __T__drop(self as *mut T); }`
   where `__T__drop` is an `extern "C++" fn` with
   `#[link_name = "_ZN<class>D1Ev"]`. Users get ergonomic
   lifetime management symmetric with `cxx_class!` — no need
   for `ManuallyDrop`.

**Attribute plumbing (new `rustc_cxx_drop_wrapper` attribute,
same 8-file shape as P09.22's `rustc_cxx_wrapper`)**:

- `rustc_span/src/symbol.rs`: new `rustc_cxx_drop_wrapper` sym.
- `rustc_feature/src/builtin_attrs.rs`: `rustc_attr!` entry.
- `rustc_hir/src/attrs/data_structures.rs`:
  `AttributeKind::RustcCxxDropWrapper(Span)` variant.
- `rustc_hir/src/attrs/encode_cross_crate.rs`: `No`.
- `rustc_attr_parsing/src/attributes/codegen_attrs.rs`:
  `RustcCxxDropWrapperParser` (`Target::Method(TraitImpl)`).
- `rustc_attr_parsing/src/context.rs`: registered.
- `rustc_passes/src/check_attr.rs`: allow-list.

**Codegen / mangling reads (three sites)**:

- `rustc_symbol_mangling/src/itanium.rs:248`: `is_cpp_abi`
  early-returns `false` when the drop method has the attribute.
- `rustc_codegen_ssa/src/codegen_attrs.rs:377`: the P09.3
  auto-export NO_MANGLE block gains a second opt-out check.
- `rustc_codegen_llvm/src/mono_item.rs:187`: after the
  `is_cxx_drop_impl` filter, early-return when the attribute is
  present (no D0/D2 trampolines).

**Target note**: `Target::Method(MethodKind::TraitImpl)` — not
`Trait { body: true }`, which is for provided trait methods in
trait *definitions*. `TraitImpl` is "method in a trait impl
block", which is where `Drop::drop` lives.

**Macro update (`crates/rustcc_macros/src/lib.rs`)**:

- The deferred-Drop block in `expand_class_native` is replaced
  by real Drop generation. Covers both branches:
  - User-explicit `#[dtor]` method → `Drop::drop` calls the
    user-named extern decl.
  - No explicit dtor → auto-synthesize an extern decl with
    `#[link_name = "_ZN<class>D1Ev"]`, emit `impl Drop`
    calling through to it.

**Verified on `/tmp/p09-23-native-drop/`**:

```rust
cxx_class_native! {
    #[size = 4] #[align = 4]
    pub class Widget {
        #[ctor] fn new(v: i32) -> Self;
        fn value(&self) -> i32;
    }
}

pub extern "C" fn rust_entry() -> i32 {
    let w1 = Widget::new(10);
    let w2 = Widget::new(20);
    w1.value() + w2.value()
    // w1, w2 drop here → Rust Drop → C++ ~Widget()
}
```

Runner checks a C++ side counter incremented in `~Widget()`:

```
sum = 30
destroy_count = 2
OK
exit=0
```

`nm libp09_23_native_drop.a | grep _ZN6Widget` shows D1 and C1
as UNDEFINED references, not definitions — confirming Rust
imports them from the C++ side instead of trying to define
duplicates.

Patch: `09-23-drop-wrapper.patch`. Workspace: 235/0 (baseline
preserved across all 11 files touched).

### P09.24 — Rust-defines-polymorphic: `#[rustc_cxx_virtual]` + vtable / typeinfo / typeinfo-name emission

Delivers backlog item #6. A `#[repr(cpp)]` class can now declare
virtual methods in Rust, the compiler emits Itanium vtable and
RTTI globals, and C++ can dispatch through those virtuals via
base-pointer calls — all with Rust as the sole definer of the
class's metadata.

**What it does**:

1. New `#[rustc_cxx_virtual]` attribute (same 7-file plumbing
   shape as P09.22 / P09.23). Applied to an inherent method on
   a `#[repr(cpp)]` type, marks that method as a C++ virtual
   member function.

2. When any method on a polymorphic `#[repr(cpp)]` class is
   codegen'd, the compiler emits three Itanium-mangled globals
   with `LinkOnceODR` linkage + comdat (for across-CGU dedup):

   - `_ZTS<class>`: byte string `"<len><name>\0"` — the
     typeinfo name.
   - `_ZTI<class>`: struct `{ptr, ptr}` — RTTI record. First
     slot is `&_ZTVN10__cxxabiv117__class_type_infoE + 2*ptr_bytes`
     (the address point of libc++abi's `__class_type_info`
     vtable). Second slot points at `_ZTS<class>`.
   - `_ZTV<class>`: struct `{i64, ptr, fn_ptrs...}` — vtable.
     Slot 0 is offset_to_top (zero in single-inheritance).
     Slot 1 is `&_ZTI<class>`. Slots 2+ are function pointers
     to each virtual method.

   Per-CGU dedup via a new `cxx_emitted_vtables: RefCell<FxHashSet<DefId>>`
   field on `CodegenCx`.

3. New symbol-mangling helpers in `rustc_symbol_mangling::itanium`:
   `mangle_cxx_vtable_symbol`, `mangle_cxx_typeinfo_symbol`,
   `mangle_cxx_typeinfo_name_symbol`, `virtual_methods_on_class`,
   `is_polymorphic_cpp_class`. These drive both the mangling and
   the codegen-time metadata emission.

4. New codegen module `rustc_codegen_llvm::cxx_vtable` (~230
   lines). Hook in `predefine_fn`: on every method of a
   polymorphic class, call `maybe_emit_class_metadata(cx, def_id)`
   which dedups and emits the three globals.

**Vptr init**: The user is responsible for (a) declaring a
`__vptr: *const ()` as the first field of their polymorphic
`#[repr(cpp)]` struct, and (b) writing the ctor body that
stores `&_ZTV<class>[2]` (the vtable's address point) into
that field. The user references the compiler-emitted vtable via
an `unsafe extern "C" { #[link_name = "_ZTV<class>"] static V: [*const (); N]; }`
declaration.

Compiler-automatic vptr-init (inserting the store at the
start of every ctor on a polymorphic class) is deferred to a
follow-up patch — it requires MIR/THIR injection of a
synthetic statement that references a compiler-generated
extern static, which is structurally larger than the
emission work shipped here.

**Scope limits (v1)**:

- Single inheritance only. No virtual bases, no multi-
  inheritance. The vtable layout emitted is the Itanium
  primary sub-table with no secondary entries.
- Virtual method order = sort by `(impl DefId, method DefId)`.
  Users must keep their C++ header's virtual declaration order
  in sync; no automatic header parsing.
- `typeid()` works at the direct type. `dynamic_cast` across
  inheritance has not been validated.
- Non-generic methods only. Generic virtuals would need
  per-monomorphization vtable slots, which v1 doesn't emit.

**Verified on `/tmp/p09-24-vtable-emit/` and `/tmp/p09-24-polymorphic/`**:

- Emission probe: `nm libp09_24_vtable_emit.a` shows
  `_ZTV6Widget`, `_ZTI6Widget`, `_ZTS6Widget` as defined
  globals. Hex dump of the vtable's `__DATA,__const` shows
  slot 0 zero (offset-to-top), slot 1 relocated to
  `_ZTI6Widget`, slots 2–3 relocated to `Widget::foo` /
  `Widget::bar`. Typeinfo name bytes = `"6Widget\0"`.
- End-to-end probe: Rust defines a polymorphic `Widget`,
  Rust ctor stores `&WIDGET_VTABLE[2]` in `__vptr`, C++
  calls virtual methods through the pointer. Dispatch lands
  in Rust: `foo() = 107`, `bar() = 14`, exit 0.

Patch: `fork/patches/09-24-rust-polymorphic.patch` (narrative).
Workspace: 235/0 (baseline preserved).

### P09.25 — Auto vptr init + auto `__vptr` slot in polymorphic layouts

Completes P09.24's "Rust-defines-polymorphic" story. Users no
longer declare a manual `__vptr` field or reference the vtable
by an `unsafe extern "C"` static. The compiler:

1. Reserves `ptr_bytes` at offset 0 in the layout of any
   polymorphic `#[repr(cpp)]` class — all user field offsets
   shift accordingly. Done in `rustc_ty_utils/layout/cxx_bridge`.
2. Injects a store of the Itanium vtable address point
   (`&_ZTV<class>[2]`) into offset 0 of the return place at the
   start of every `#[rustc_cxx_ctor]` method body, before any
   MIR runs. New `BuilderMethods::maybe_emit_cxx_ctor_vptr_init`
   trait hook; default no-op, LLVM overrides.

Result — users write plain Rust:

```rust
#[repr(cpp)]
pub struct Widget { pub v: i32 }

impl Widget {
    #[rustc_cxx_ctor]
    pub fn new(v: i32) -> Self { Widget { v } }

    #[rustc_cxx_virtual]
    pub fn foo(&self) -> i32 { self.v + 100 }
}
```

and C++ dispatches into `foo` correctly via the vtable.

**Details**:

- `cxx_bridge::correct_layout` gains a polymorphism check that
  runs before the agree-and-shortcircuit path: even when
  Itanium's stock-corrected size matches Rust's, polymorphic
  classes need the vptr-slot reservation.
- The shift function bumps size by `ptr_bytes`, raises align to
  at least `ptr_align`, and forces `backend_repr = Memory`
  (otherwise the x86_64 classifier crashes on a `Scalar(i32)`
  layout that's actually 16 bytes).
- `BuilderMethods` gains a default-no-op trait method so only
  backends implementing the rustcc C++ ABI pay the cost.
  `codegen_mir` calls it once at function entry using the
  return place's storage pointer.
- LLVM backend declares `_ZTV<class>` as an external `ptr`,
  GEPs by `2 * ptr_bytes` to get the address point, and
  stores into `ret_place_ptr`.
- `rustc_target::callconv::x86_64::classify_arg` grows a
  one-line robustness fix: if any covered eightbyte is left
  unclassified after the classifier walks fields, return
  `Memory`. This catches the "hole at offset 0" case produced
  by our vptr-reserving layout and any other layout with gaps.

**Files touched** (4 in fork):

- `rustc_ty_utils/src/layout/cxx_bridge.rs` — shift helper,
  polymorphism check (~55 LOC added).
- `rustc_codegen_ssa/src/traits/builder.rs` — trait method
  (~15 LOC).
- `rustc_codegen_ssa/src/mir/mod.rs` — hook call (~6 LOC).
- `rustc_codegen_llvm/src/builder.rs` — override (~8 LOC).
- `rustc_codegen_llvm/src/cxx_vtable.rs` — `maybe_emit_vptr_init`
  helper (~45 LOC).
- `rustc_symbol_mangling/src/itanium.rs` — `has_cxx_ctor_attr`
  visibility bump to `pub` (1 char).
- `rustc_target/src/callconv/x86_64.rs` — classifier robustness
  (~10 LOC).

**Validation**:

- `/tmp/p09-25-auto-vptr/`: no manual `__vptr`, no extern static.
  `foo() = 107, bar() = 14 — OK`, exit 0.
- `/tmp/p09-24-polymorphic/` regression: still passes with the
  manual `__vptr` pattern (P09.25 is additive for that path —
  the manual field just occupies a redundant slot).
- `cargo test --workspace` → 235/0 preserved.

**Scope limits carried over from P09.24**: single inheritance
only; non-generic virtual methods only; user still keeps C++
header's virtual method order in sync with Rust's impl order.

Patch: `fork/patches/09-25-auto-vptr.patch`.

### P09.26 — Fix VWT slot order in `rustcc_swift_rt`

Workspace-only bug fix. Resolves the long-standing "VWT-direct
destroy segfaults on types with class fields" blocker.

**Diagnosis**: the `ValueWitnessTable` struct in
`rustcc_swift_rt` had its fields in the wrong order. Rust had
`destroy` at offset 0; Swift's actual VWT layout has
`initializeBufferWithCopyOfBuffer` at offset 0 and `destroy` at
offset 8. Every call to `(*vwt).destroy(value, metadata)` was
actually dispatching into
`initializeBufferWithCopyOfBuffer(dst=value, src=metadata)`.

For trivial types the init-buffer witness is a plain memcpy —
the mistake was silent. For types with class fields it does a
`swift_retain` on what it thinks is the source class ref; with
`src` actually being our metadata pointer, the retain operated
on a Metadata struct as if it were a HeapObject and crashed in
`swift::RefCounts<...>::incrementSlow` with
`EXC_BAD_ACCESS`. That symptom matches the documented
"swiftcc fn-pointer ABI" gloss — but the real cause had nothing
to do with swiftcc; it was a plain off-by-slot in the struct
declaration.

**Evidence**: compiled a tiny Swift file with a struct
containing a class field, emitted IR via `swiftc -emit-ir`, and
inspected the VWT constant literal `@"$s...VWV"`. Field order
in the literal:

```
slot 0: $s6verify2NTVwCP   (initializeBufferWithCopyOfBuffer)
slot 1: $s6verify2NTVwxx   (destroy)
slot 2: $s6verify2NTVwcp   (initializeWithCopy)
slot 3: $s6verify2NTVwca   (assignWithCopy)
slot 4: __swift_memcpy16_8 (initializeWithTake — trivial memcpy)
slot 5: $s6verify2NTVwta   (assignWithTake)
slot 6: $s6verify2NTVwet   (getEnumTagSinglePayload)
slot 7: $s6verify2NTVwst   (storeEnumTagSinglePayload)
```

Swift's own header (`include/swift/ABI/ValueWitness.def`)
declares the same order.

**Fix**: reordered the Rust struct to match — added the
missing `initialize_buffer_with_copy_of_buffer` slot at
offset 0; pushed `destroy` to offset 8; swapped
`initialize_with_take` and `assign_with_copy` (Swift does
assign before init-take among the second pair of pair-style
witnesses).

**Validation**:

- `/tmp/p09-vwt-investigate/` — new probe; copies
  `/tmp/p09-swift-nt` but replaces outlined-destroy with
  `drop_swift_value`. Before: segfault, exit 139. After:
  `deinit_count = 3`, exit 42.
- `/tmp/p09-swift-nt/` regression — outlined-destroy path
  still passes, exit 42.
- `cargo test --workspace` → 235/0 preserved.

**Files touched**: `crates/rustcc_swift_rt/src/lib.rs` —
reordered fields; top-level doc block updated to remove the
"currently broken" caveat. No compiler changes.

Patch: `fork/patches/09-26-vwt-slot-order.patch`.

### P09.27 — Coverage pass: aarch64 static audit + small-poly ABI + VWT Clone validation

No source changes. Three validation probes / audits confirming
that the post-P09.26 Rust-defines-polymorphic and Swift VWT
paths handle edge cases correctly.

**#13 aarch64 classifier**: static audit of
`rustc_target/src/callconv/aarch64.rs`. Unlike x86_64, the
aarch64 classifier doesn't inspect individual eightbytes — it
treats aggregates as opaque sized blocks and casts to
uniform i64 registers or sret's. The `.unwrap()` at line 29 is
in an unrelated HFA size-overflow check, not reachable for
polymorphic layouts. `compute_cxx_abi_info` has the same shape
as x86_64 (C-path then `force_indirect` for non-trivial cpp
types). Conclusion: aarch64-apple-darwin needs no code changes.
Skipped the ~10 min stage-1 aarch64 std build since the risk is
zero per the code review.

**#14 small polymorphic class**
(`/tmp/p09-small-poly/`): `#[repr(cpp)] struct Tiny {}` + one
virtual method. Ctor IR shows `ptr sret([16 x i8]) align 8`
— sret'd as expected. Exit 0. Minor note: `#[repr(cpp)]`'s
empty-class size=1 rule combined with our vptr-shift yields
16 bytes vs. Clang's 8 for the same shape (Clang doesn't
size-bump when a class has a vptr). Functional in our probe
(only `Tiny*` crosses the boundary); could be tightened in a
follow-up by having `cxx_bridge::correct_layout` skip the
empty-bump for polymorphic classes.

**#15 Swift VWT Clone** (`/tmp/p09-vwt-clone/`): calls
`clone_swift_value` on a `#[repr(swift)]` type with a class
field, observing `inner_deinit_count` between each step:

```
after make_nt: deinit=0
after clone  : deinit=0   (retain -> refcount 2)
after drop(b): deinit=0   (release -> refcount 1)
after drop(a): deinit=1   (release -> refcount 0 -> deinit)
OK, exit 0
```

Confirms the P09.26 slot-order fix works for the
`initialize_with_copy` path too, not just `destroy`.

Patch: `fork/patches/09-27-coverage-probes.patch`. Workspace
235/0 unchanged.

### P09.28 — Phase 2b.2: `swift_value!` macro auto-synthesizes Drop + Clone

Workspace-only. No compiler changes. Delivers the long-queued
Swift Phase 2b.2 ergonomic win: user-written `#[repr(swift)]`
types no longer need a hand-written metadata extern / Drop /
Clone block.

**Before (manual)**:

```rust
#[repr(swift)]
#[rustc_swift_type = "MyLib.NT"]
pub struct NT { pub x: u64, pub inner: *mut c_void }

unsafe extern "C" {
    #[link_name = "$s5MyLib2NTVMa"]
    fn nt_metadata(flags: usize) -> MetadataResponse;
}

impl Drop for NT {
    fn drop(&mut self) {
        unsafe { drop_swift_value(self as *mut NT, nt_metadata); }
    }
}
// + manual Clone impl calling clone_swift_value
```

**After (P09.28)**:

```rust
rustcc_macros::swift_value! {
    #[rustc_swift_type = "MyLib.NT"]
    pub struct NT { pub x: u64, pub inner: *mut c_void }
}
```

The macro emits:

- `#[repr(swift)]` on the struct (re-applied by the macro).
- `extern "C"` metadata accessor decl with the Itanium/Swift
  mangling: `$s<modlen><module><typelen><type>VMa` for value
  types, `$s...CMa` for classes.
- `impl Drop` forwarding to
  `rustcc_swift_rt::drop_swift_value` (value) or
  `release_swift_class` (class).
- `impl Clone` forwarding to
  `clone_swift_value` (value) or
  `retain_swift_class` + bitwise copy (class).

**Design choice — macro, not compiler synthesis**: the queue
state originally called Phase 2b.2 "compiler auto-synthesis",
which would need synthetic HIR trait-impl nodes — a pattern the
fork hasn't used. The macro path delivers the same ergonomic
win in ~200 LOC isolated to `rustcc_macros`, matching the
precedent set by P09.22's `cxx_class_native!`. Compiler
synthesis remains a future cleanup target.

**Scope (v1)**: ASCII module + type names only; non-generic
only; class bindings assume a single-field struct with the
class pointer at offset 0; metadata accessor signature assumed
to be `fn(usize) -> MetadataResponse`.

**Validation** (`/tmp/p09-28-swift-auto/`): value type with a
class field, exercises the auto Clone + Drop observing
`inner_deinit_count`:

```
after make_nt: deinit=0 (want 0)
after clone  : deinit=0 (want 0)   # auto Clone -> retain
after drop(b): deinit=0 (want 0)   # auto Drop -> release
after drop(a): deinit=1 (want 1)   # auto Drop -> final release
OK, exit 0
```

**Files**: `crates/rustcc_macros/src/lib.rs` (~200 lines added).
No compiler changes.

Patch: `fork/patches/09-28-swift-value-macro.patch`.
Workspace: 235/0 preserved.

### P09.29 — Cross-crate C++ interop fixes: encode ctor/wrapper attrs + tighten static-method mangling

Fixes two gaps surfaced by a two-crate polymorphic probe
(`/tmp/p09-crosscrate/`). Ships as a bundle because both bugs
sat on the same failing test and need to be fixed together for
cross-crate dispatch to work.

**Bug 1 — ctor attribute dropped at crate boundary**:
`#[rustc_cxx_ctor]`, `#[rustc_cxx_wrapper]`, and
`#[rustc_cxx_drop_wrapper]` were all `No` in
`encode_cross_crate`. Crate_b importing crate_a's Widget couldn't
see that `Widget::new` was a ctor, so it mangled the call as a
regular static method. Crate_a's body was under
`_ZN6WidgetC1Ei`; crate_b's call was `_ZN6Widget3newEi` —
unresolved link. Fix: flip all three variants to `Yes`.

**Bug 2 — static methods on `#[repr(cpp)]` classes strip arg 0**:
In `itanium::mangle`, `input_skip` was unconditionally 1 for any
inherent-method-on-cpp-class (to skip the implicit `self` in
Itanium arg-list mangling). For static methods (no self) with at
least one user arg, this stripped the first user arg, producing
`Widget::new()` (void) instead of `Widget::new(int)`. Didn't
surface in same-crate probes because `#[rustc_cxx_ctor]` methods
hit the `is_ctor` branch first. Cross-crate, combined with Bug 1
making crate_b misread `new` as a non-ctor static method, the
argv-stripping bug fired. Fix: gate `input_skip` on
`tcx.associated_item(def_id).is_method()` — only strip when the
fn actually has `self`.

**Files**:

- `rustc_hir/src/attrs/encode_cross_crate.rs` — 3 variants
  flipped from No to Yes.
- `rustc_symbol_mangling/src/itanium.rs` — `input_skip` check
  tightened.

**Validation**:

- `/tmp/p09-crosscrate/` — two-crate probe. Before: link error
  (`Widget::new()` unresolved). After: `foo() = 107, bar() = 14
  — OK (cross-crate), exit 0`.
- `/tmp/p09-25-auto-vptr/` regression — same-crate polymorphic
  path still passes.
- `cargo test --workspace` → 235/0 preserved.

Patch: `fork/patches/09-29-cross-crate-cxx-attrs.patch`.

### P09.30 — Parser-level `class` keyword (parse-time desugaring)

Closes backlog item #8 with the small-scope half of the design
tradeoff: `class` is recognized at item position and desugared
in place into a `#[repr(cpp)]` struct + an inherent `impl`.
Downstream compiler passes (HIR, resolve, typeck, codegen,
pretty-print, visitors) never see a distinct `class` AST
variant — nothing to teach them.

**Design choice**: the queue note estimated the full "new
`ItemKind::Class` variant across 14+ crates" version at
500–1000 LOC. Parse-time desugaring delivers the same surface
syntax in ~150 LOC contained in `rustc_parse` and `rustc_span`.

**Syntax**:

```rust
pub class Widget {
    v: i32,

    #[rustc_cxx_ctor]
    pub fn new(v: i32) -> Self { Widget { v } }

    #[rustc_cxx_virtual]
    pub fn foo(&self) -> i32 { self.v + 100 }
}
```

**Scope (v1)**:

- Fields must precede methods (matches the shape a human-written
  struct + impl would have).
- Fields don't support outer attributes — any `#`-prefixed member
  routes to the method path. Future patch can relax.
- Fields accept `pub` / `pub(...)` visibility.
- Methods pass through full attr/vis handling via
  `parse_assoc_item`.
- `class` remains usable as a regular identifier in non-item
  positions (weak keyword, same mechanism as `union`/`auto`).

**How it works**:

1. `rustc_span/src/symbol.rs`: `class` added to the weak-keyword
   list.
2. `rustc_parse/src/parser/mod.rs`: new field
   `pending_injected_item: Option<Box<ast::Item>>` on `Parser<'a>`.
   Size assert bumped 288 → 304.
3. `rustc_parse/src/parser/item.rs`:
   - `parse_item` returns `pending_injected_item` first if set.
   - `parse_item_kind` gains an `is_kw_followed_by_ident(kw::Class)`
     branch that calls `parse_cxx_class_item`.
   - `parse_cxx_class_item` consumes `class Ident { body }`, routes
     members to method-vs-field path by peeking leading token,
     builds the inherent impl, stashes in
     `pending_injected_item`, returns the struct's ItemKind.
     `#[repr(cpp)]` is prepended to the struct's outer attrs.

**Validation**:

- `/tmp/p09-30-class-kw/`: pure parser probe. No macro wrapper,
  no explicit `#[repr(cpp)]`, no separate impl. C++ calls
  virtuals through a `Widget*`. `foo() = 109, bar() = 18 — OK
  (class keyword), exit 0`.
- `/tmp/p09-30-class-ident/`: `class` still usable as an
  identifier in `let class = 42;` binding.
- `/tmp/p09-25-auto-vptr/` regression: same-crate polymorphic
  dispatch still passes.
- `cargo test --workspace` → 235/0 preserved.

**Files**:

- `compiler/rustc_span/src/symbol.rs` — 1 line
- `compiler/rustc_parse/src/parser/mod.rs` — 2 lines
- `compiler/rustc_parse/src/parser/item.rs` — ~120 lines (the
  new `parse_cxx_class_item` + small hook changes)

Patch: `fork/patches/09-30-class-keyword.patch`.

### P09.31 — v1 coverage bundle: empty-class size, field attrs, class-header generics + validation probes

Five queue items folded into one patch — each small,
thematically "v1 polish."

**Fixes**:

- **#5 empty polymorphic class size** (`cxx_bridge::correct_layout`):
  vptr-only polymorphic classes are now 8 bytes (matches Clang),
  not 16. The `#[repr(cpp)]` empty-class-size=1 rule is
  collapsed before the vptr shift when no user fields.
- **#6 class field attributes**
  (`rustc_parse::parser::item::parse_cxx_class_item`): `class`
  body now accepts `#[doc] / #[cfg] / ...` on fields. Added
  `skip_over_outer_attributes` token-scan helper; dispatch now
  peeks past attr groups before deciding field vs method.
- **#7 generics on class header**
  (parser + itanium + codegen_attrs): `pub class Pair<A, B> {
  … }` now emits `impl<A, B> Pair<A, B> { … }` with the right
  generic args. `is_cpp_abi` and the NO_MANGLE auto-export
  both skip when the impl or fn carries non-synthetic
  generics — Itanium has no expression for Rust type params,
  and NO_MANGLE would collide across monomorphizations.

**Validated (no code changes needed)**:

- **#1 non-trivial virtual signatures**:
  `/tmp/p09-35-virtsigs/`. Virtuals with multi-arg primitive
  signatures, `&Point` refs to another cpp class, and
  `*const Point` all mangle and dispatch correctly.
- **#9 multi-field Swift class-backed bindings**:
  `/tmp/p09-33-swift-multi/`. Existing `swift_value!`
  memcpy+retain handles POD extra fields. Non-POD extra
  fields (second class handle) remain a documented v2 item.

Workspace: 235/0. Patch: `fork/patches/09-31-small-fixes-bundle.patch`.

### P09.32 — Single inheritance of Rust polymorphic classes + dynamic_cast

The v1 capstone. Enables `pub class Dog : Animal { … }` syntax
on the fork.

**Scope (v1)**:

- Single inheritance only (one base per derived class).
- Derived MAY add new virtuals (go into vtable slots after
  base's).
- Derived MAY NOT override base virtuals — methods with the
  same name emit as new distinct virtuals. Call through a
  base pointer reaches base's impl. **Override deferred to
  v2.**
- `dynamic_cast<Derived*>(base_ptr)` and
  `dynamic_cast<Base*>(derived_ptr)` work via libc++abi's
  runtime typeinfo-chain walk.

**Pieces**:

- **New `#[rustc_cxx_base]` attribute** on fields — inserted
  automatically by the parser when it sees `class D : B { … }`,
  marks the synthesized `__base: B` field as the polymorphic
  base subobject. Same 7-file plumbing shape as prior
  `rustc_cxx_*` attrs.
- **Parser** accepts optional `: Base` after class ident;
  synthesizes first field `__base: Base`. Users initialize via
  the usual struct literal: `Self { __base: Base::new(…), … }`.
- **Layout** (`cxx_bridge::correct_layout`): detects
  `has_polymorphic_base(tcx, def)` and skips the P09.25 vptr
  shift — base subobject already provides the vptr slot at
  offset 0.
- **Itanium helpers**: `polymorphic_base_of_class` and
  `virtuals_on_chain` walk the inheritance chain. The vtable
  emitter uses the chain walk so base's virtual slots come
  first, giving stable slot indices across base and derived
  (key invariant for upcast to work).
- **Typeinfo** branches on base presence:
  - Root: 2-slot `__class_type_info` (as before).
  - Derived: 3-slot `__si_class_type_info` — RTTI vtable +
    typeinfo name + `&_ZTI<Base>`. The third slot drives
    libc++abi's dynamic_cast chain walk.
- **Ctor vptr write at return**
  (`rustc_codegen_ssa::mir::block::codegen_return_terminator`):
  hooks `maybe_emit_cxx_ctor_vptr_init` before every
  `bx.ret_*()`. Required because user-written `Self {
  __base: Base::new(…), … }` writes base's vptr into offset
  0 AFTER the P09.25 entry-point injection, clobbering the
  derived vptr. Injecting at return ensures the derived
  vptr is the last write. For non-derived classes this is
  redundant-but-harmless.

**Validation**:

- `/tmp/p09-35-inherit/` — Dog inherits Animal; C++ calls
  both base virtuals (via Dog* and upcast Animal*) and
  derived's new virtual. `legs=4 noise=7 bark=30
  up_legs=4 up_noise=7 — OK (inheritance), exit 0`.
- `/tmp/p09-36-dyncast/` — `dynamic_cast<Dog*>(animal_from_dog)`
  succeeds; `dynamic_cast<Dog*>(plain_animal)` returns
  nullptr. Exit 0.
- `/tmp/p09-25-auto-vptr/` and `/tmp/p09-30-class-kw/`
  regressions pass.
- `cargo test --workspace` → 235/0 preserved.

**Files touched**: ~12 files, ~180 LOC total. See the patch
file for per-file breakdown.

Patch: `fork/patches/09-32-single-inheritance.patch`.

### P09.33 — v1 naming cleanup: shorter public attribute and macro names

Public-facing rename pass before the v1 freeze.

| Old | New |
|---|---|
| `#[rustc_cxx_ctor]` | `#[constructor]` |
| `#[rustc_cxx_virtual]` | `#[cpp_virtual]` |
| `#[rustc_cxx_operator]` | `#[operator]` |
| `#[rustc_swift_type]` | `#[swift_type]` |
| `#[rustc_swift_symbol]` | `#[swift_symbol]` |
| `cxx_class_native!` | `native_cpp_class!` |

**What's not renamed**:

- **Internal attrs** — `rustc_cxx_wrapper`,
  `rustc_cxx_drop_wrapper`, `rustc_cxx_base`. Emitted by the
  `native_cpp_class!` macro and the `class` keyword's parser
  desugar; users don't write them.
- **`cxx_class!`** — stable-rustc-compatible macro, untouched.
- **`swift_value!`** — macro name unchanged; only its
  emission updates to `#[swift_type = "…"]`.

**Naming rationale**: `constructor` dodges the `ctor` crate;
`cpp_virtual` disambiguates from Rust's reserved `virtual`
keyword; `swift_type` / `swift_symbol` keep ABI parity;
`native_cpp_class!` avoids overlap with the `cpp` crate's
`cpp_class!`.

**Files touched** (5): `rustc_span/src/symbol.rs`,
`rustc_feature/src/builtin_attrs.rs`,
`rustc_attr_parsing/src/attributes/codegen_attrs.rs`, plus
`crates/rustcc_macros/src/lib.rs` for the macro rename and
emission updates.

**Validation**: all v1 capstone probes (`/tmp/p09-{25,28,30,35,36,crosscrate,…}/`)
rewritten with the new syntax and re-run. `cargo test --workspace`
→ 235/0 preserved.

Patch: `fork/patches/09-33-attribute-renames.patch`.

### P09.34 — Virtual method override

Promoted from v2 — the implementation was small enough (~25
LOC in `virtuals_on_chain`) to land in v1.

**Change**: the chain-walk that collects virtuals for a
derived class now resolves overrides by name-matching against
base slots. Derived virtuals with the same name as a base
virtual REPLACE the base's slot rather than appending a new
one. C++ callers dispatching through a base pointer reach the
derived's impl — normal C++ override semantics.

**Matching rule**: name-only. Signature-compat lint deferred
to v2.

**Validation**: `/tmp/p09-37-override/` — Dog overrides
Animal::speak(). Calls through `Animal*` on a Dog return 1003
(Dog's impl) not 70 (Animal's). Inherited `legs()` still
works, new `wag()` virtual at fresh slot. Regressions pass.

Patch: `fork/patches/09-34-virtual-override.patch`.

### P09.35 — Cross-compilation support

Fixes a correctness bug that made every C++ ABI path use the
HOST target's parameters instead of the actual `--target`
target.

**Root cause**: all four call sites in the fork constructed
`CxxTypeCtx` with `CxxTarget::host()`, whose impl uses
compile-time `#[cfg]` to detect the host platform. That
detection happens when stage-1 rustc is *built*, not when
it runs — so the host triple is baked in.

**Fix**: new `CxxTarget::from_rustc_triple(triple, ptr_width)`
constructor that reads the actual target triple + pointer
width from `sess.target`. The four call sites now pass
`sess.opts.target_triple.tuple()` and
`sess.target.pointer_width`. Unknown triples fall through to
a generic Itanium profile parameterized by the supplied
pointer width.

**Files**: `rustc_abi_cxx/src/target.rs` (new constructor +
doc updates), `rustc_symbol_mangling/src/itanium.rs` (3
sites), `rustc_ty_utils/src/layout/cxx_bridge.rs` (1 site).

**Validation** (`/tmp/p09-38-cross/`): `cargo +stage1 build
--target aarch64-apple-darwin` from an x86_64-apple-darwin
host. Output:

- `nm libp09_38_cross.a` shows `__ZN6WidgetC1Ei`,
  `__ZTI6Widget`, `__ZTV6Widget` all present with Itanium
  mangling appropriate for the aarch64 target.
- `clang++ -target arm64-apple-macosx11.0` links against the
  static archive into a Mach-O arm64 binary. The link is the
  ABI correctness test: C++'s expected symbols match Rust's
  emitted ones.
- IR inspection: `target triple = "arm64-apple-macosx11.0.0"`,
  correct aarch64 data layout.

Native x86_64 regressions all pass. `cargo test --workspace`
→ 235/0.

**Install note**: cross-compilation requires per-target std
artifacts in the stage1 sysroot:

```
./x.py build --stage 1 library --target aarch64-apple-darwin
# then copy rlib/rmeta/o files from
# build/<host>/stage1-std/<target>/dist/deps/ into
# build/host/stage1/lib/rustlib/<target>/lib/
```

Patch: `fork/patches/09-35-cross-compilation.patch`.

### P09.36 — Bare-metal ARM Cortex-M / STM32 (documentation-only)

Investigation result: **no compiler changes needed**. The
existing fork already emits correct ARM32 Itanium-ABI code
after P09.35's cross-compile fix. This patch is purely
documentation covering the integration path.

**Probe** (`/tmp/p09-39-arm32/`): a `#![no_std]` polymorphic
`#[repr(cpp)]` class built for `thumbv7em-none-eabihf`
(Cortex-M4F, STM32F4/L4/G4 family). LLVM IR confirms:

- `target triple = "thumbv7em-unknown-none-eabihf"`
- `target datalayout = "...p:32:32..."` (32-bit pointers)
- Ctor: void-return sret `(ptr sret([8 x i8]) align 4 %_0, i32 %v)`
- Vtable: `{ i32, ptr, ptr }` — 4-byte slot pointers
- Vptr GEP offset: `i32 8` (2 × 4-byte pointers = address point)

**Cross-compiler verification** with `arm-none-eabi-g++ 15.2.0`:

- C++ mangles `Widget::Widget(int32_t)` → `_ZN6WidgetC1Ei`
  (identical to Rust).
- Call-site disassembly: `bl _ZN6WidgetC1Ei` with `r0=this`,
  `r1=v`, return value ignored — matches Rust's sret+void
  byte-for-byte.
- Virtual dispatch: `ldr r3, [obj, #0]; ldr r3, [r3, #0];
  blx r3` — reads vptr from obj[0], reads first fn pointer,
  calls. Exact match with emission.

**Link-time knob** — `_ZTVN10__cxxabiv117__class_type_infoE`
appears undefined in the Rust static archive (libc++abi's
RTTI-base vtable). Bare-metal users provide a 3-line C stub
or link `libsupc++-nano`. Virtual dispatch works with just the
stub; `typeid` / `dynamic_cast` need real libc++abi.

**Partial-relocatable link**:

```
arm-none-eabi-ld -r caller.o rtti_stub.o \
  --whole-archive libp09_39_arm32.a -o probe.o
# probe.o: ELF 32-bit LSB relocatable, ARM, EABI5 version 1
# Zero unresolved symbols.
```

**Supported targets** (thumbv7em tested; extrapolated from
recipe): `thumbv7m-none-eabi` (M3), `thumbv7em-none-eabi` (M4),
`thumbv7em-none-eabihf` (M4F), `thumbv8m.base-none-eabi` (M23),
`thumbv8m.main-none-eabihf` (M33F).

**Docs updated**:

- `getting-started.html` §16 — build recipe, RTTI stub, link steps.
- Feature matrix — "Bare-metal ARM Cortex-M" row added as
  supported.
- Targets list — extends to `thumbv*m*` families.

**Key takeaway** (saved to memory): before estimating a "new
target support" effort, build the minimum-viable probe first.
What looked like a 2-4 day ARM32 ABI overlay turned out to be
zero-code after P09.35 made `CxxTarget` target-aware.

Patch: `fork/patches/09-36-arm32-baremetal.patch`.

---

## rustcc v1 milestone (2026-04-21)

With P09.36 shipped, the fork reaches a coherent v1 feature
set. Summary of what v1 delivers and what remains for v2:

### v1 supported

- `#[repr(cpp)]` struct layout (Itanium rules).
- `extern "C++"` free functions with Itanium mangling.
- Inherent methods on `#[repr(cpp)]` types (Tier-1 auto-mangled).
- `#[constructor]` — Rust-defined C++ constructors (P09.33
  rename of the former `#[rustc_cxx_ctor]`).
- `rustc_cxx_drop_wrapper` / `rustc_cxx_wrapper` — internal
  attrs emitted by macros/parser; opt methods out of P09.3's
  auto-export paths.
- `#[cpp_virtual]` + auto vptr slot + auto ctor vptr-init
  (`_ZTV` / `_ZTI` / `_ZTS` emission).
- **Single inheritance** — `class Derived : Base { … }`.
  `__si_class_type_info` chain for `dynamic_cast`.
- **Virtual method override** — derived `#[cpp_virtual]`
  methods with the same name as a base virtual replace the
  base's vtable slot (P09.34).
- **Cross-compilation** — `--target` different from host
  works for all the C++ ABI paths (P09.35). Native aarch64
  output from x86_64 dev machines, Linux → Darwin, etc.
- **Bare-metal ARM Cortex-M** — `thumbv7em`, `thumbv7m`,
  `thumbv8m.*` targets (STM32 family) compile and link
  correctly (P09.36). Requires a 3-line libc++abi stub
  unless `libsupc++-nano` is linked.
- **Parser-level `class` keyword** — ergonomic sugar,
  fields + methods + inheritance in one block.
- Cross-crate polymorphic classes — ctors, wrappers, virtuals
  all encode correctly across crate boundaries.
- `extern "Swift"` — Swift ABI calling convention + mangling
  for value types and classes.
- `swift_value!` macro — auto Drop/Clone for `#[repr(swift)]`.
- `cxx_class!` macro — stable-rustc-compatible C++ bindings.
- `cxx_class_native!` macro — fork-only native bindings.

### v2 deferred

- Multi-inheritance / virtual bases.
- Signature-compatibility lint for virtual overrides (v1
  matches overrides by name only; mismatched signatures are
  user error).
- True compiler-level auto-synthesis for `#[repr(swift)]`
  (current path uses the `swift_value!` macro wrapper).
- Multi-field class-backed Swift bindings with non-POD
  extra fields.
- Const generics on class headers.
- Parser-level distinct `ItemKind::Class` AST variant (current
  path is parse-time desugaring; an AST variant would give
  editor tools a distinct node).

### Deferred from this session

- **#2 parser-level `class` keyword** — **shipped as P09.30**
  via parse-time desugaring (Option B). `class Name { ... }` is
  recognized as a weak keyword at item position and desugared
  into `#[repr(cpp)]` struct + inherent impl during parsing.
  The full "new `ItemKind::Class` variant" design (Option A,
  the 500–1000 LOC estimate) remains available as a potential
  future upgrade if editor tooling ever wants to distinguish
  classes from structs at the AST level.
- **#5 ctor/dtor emission unification** — **fully shipped**.
  Ctors in P09.22 via foreign-fn `#[rustc_cxx_ctor]`; dtors in
  P09.23 via `#[rustc_cxx_drop_wrapper]` on `Drop::drop`.
  `cxx_class_native!` now emits an ergonomic `impl Drop` that
  forwards to the C++ destructor without colliding with
  P09.15's auto-exported D0/D1/D2.
- **#6 Rust-defines-polymorphic** — **shipped as P09.24**
  (vtable/typeinfo/typeinfo-name emission + user-driven vptr
  init). Compiler-automatic vptr-init in ctor prologues and
  auto-injection of the `__vptr` field in the struct layout
  remain as P09.25 work.
- **VWT-direct path debug** — **shipped as P09.26**. Root cause
  was a slot-order bug in `rustcc_swift_rt::ValueWitnessTable`,
  not a swiftcc calling-convention issue. Both the direct and
  outlined destroy paths now work.
- **Phase 2b.2 value-type Drop/Clone** — **shipped as P09.28**
  via the `swift_value!` macro. True compiler-level auto-synthesis
  (no wrapper macro) is still a potential future cleanup.
- **Phase 2c class auto-synthesis** — `swift_value!` already
  handles the class case in P09.28 (single-field handle
  pattern). Full-featured class bindings (multiple fields,
  custom deinit forwarding) remain future work.

**P01–P08 verified end-to-end.** `extern "C++"` blocks parse,
`ExternAbi::Cpp` threads through every compiler pass that
exhaustively matches ABIs (five iterations to find them all),
and items with that ABI get Itanium-mangled symbol names via
the vendored `rustc_abi_cxx::mangle`. Verified byte-for-byte
against Clang:

```
extern "C++" {
    fn cpp_thing(x: i32) -> i32;
    fn cpp_void();
    fn cpp_two(a: u64, b: *const u8) -> bool;
    fn cpp_float(x: f64) -> f32;
}
```

produces:

```
__Z9cpp_thingi    — cpp_thing(int)
__Z8cpp_voidv     — cpp_void()
__Z7cpp_twoyPKh   — cpp_two(unsigned long long, unsigned char const*)
__Z9cpp_floatd    — cpp_float(double)
```

— identical to what Clang emits for the equivalent C++ decls,
so a Rust crate declaring these and a C++ library that defines
them link cleanly.

**P01–P07 verified end-to-end.** P07 is a working bridge that
routes `#[repr(cpp)]` ADT layouts through `rustc_abi_cxx` (vendored
at `compiler/rustc_abi_cxx/`), with stage-1 rustc compiled against
the patched tree and exercised on a real program:

```
size_of::<Empty>()  = 1  // P07 override: empty repr(cpp) → 1 byte
align_of::<Empty>() = 1
size_of::<Point>()  = 8  // stock layout, unchanged by P07
size_of::<EmptyC>() = 0  // repr(C) path NOT affected by P07
```

The first two prove Itanium semantics reach the type system; the
third proves P07 is scoped correctly — stock `#[repr(C)]` empty
structs still report size 0.

**P01–P06 verified compiling** as well. `./x.py build --stage 1
compiler` against a full `rust-lang/rust` master clone with P01–P06
applied produces a working stage-1 rustc binary. Build time on
x86_64-apple-darwin with `download-ci-llvm = true`: ~9 minutes
end-to-end (~8 min to first compile error at P06's gap, then
~1 min for the incremental resume after fixing P06).

**Behavior confirmed:**

- Upstream rustc: rejects `#[repr(cpp)]` with `error[E0552]:
  unrecognized representation hint`.
- Forked stage-1 rustc: accepts `#[repr(cpp)]` and
  `#[repr(cpp, align(16))]` on structs. Attributes flow through
  parser, check_attr validator, and typeck without diagnostics
  from the patch surface. Layout/codegen haven't been exercised
  yet because P07–P09 still route `repr(cpp)` types through the
  stock Rust layout algorithm (which is safe but not yet Itanium-
  compliant — that's P07's delegation work).

**What was unexpected:** P06 wasn't in the original prose list —
it emerged when the build hit an exhaustive-match error in
`rustc_passes::check_attr`. The exhaustive match expects every
`ReprAttr` variant. Folded into the patch series as a required
prerequisite to P04–P05 having any effect; the prose-level list
above now reflects this.

P07–P09 are described in prose but not yet authored as diffs —
they require more structural work (new modules, Cargo wiring,
target-specific ABI code) than the simple additions P01–P06 are.

## Post-v1 target extensions

Shipped after the 2026-04-21 v1 milestone. These extend the
supported target matrix without touching v1 semantics.

### P09.37 — RISC-V ESP32 / bare-metal rv32 Itanium overlay

**File**: `compiler/rustc_target/src/callconv/riscv.rs` (+41 LOC),
`compiler/rustc_target/src/callconv/mod.rs` (+11 LOC).
**Patch**: `fork/patches/09-riscv-cxx-overlay.patch`.

### The problem

P09.35 made `CxxTarget` target-aware and P09.36 confirmed
bare-metal ARM Cortex-M worked zero-code. A follow-up probe on
`riscv32imc-unknown-none-elf` (ESP32-C3) found non-polymorphic
`#[repr(cpp)]` classes compiled cleanly, but adding a
`#[cpp_virtual]` method ICE'd the compiler:

```
thread 'rustc' panicked at compiler/rustc_target/src/callconv/riscv.rs:185:
type Widget has a first field with non-zero offset Size(4 bytes)
```

Upstream rv32's `should_use_fp_conv` walks fields by increasing
offset and asserted the first field starts at offset 0. Our
polymorphic class lays out as `{ vptr-hole, v: i32 }` — the hole
isn't a real Rust field, so the walker sees `v` at offset 4 and
panics. ARM didn't hit this because ARM's call-conv doesn't run
this probe.

### The fix

Two layered changes in `rustc_target/src/callconv/riscv.rs`:

1. **Defensive panic-to-None** in `should_use_fp_conv`. A layout
   with leading padding before the first field makes the type
   ineligible for the register-pair fp-conv optimization. The
   RISC-V psABI itself carves these out — "aggregates [...] with
   nontrivial copy constructors, destructors, or vtables" are
   passed by reference — so a graceful `return None` is correct
   and matches Clang/GCC behaviour on non-trivial classes.

2. **`riscv::compute_cxx_abi_info` overlay**, mirroring the
   x86_64 / aarch64 P09.6 / P09.11 overlays: run the psABI
   C-path `compute_abi_info`, then `force_indirect()` any type
   where `is_cxx_non_trivial_for_calls` is true. Wired into
   `adjust_for_foreign_abi` in `mod.rs` for
   `ExternAbi::Cpp + Arch::RiscV32 | Arch::RiscV64`.

### Validation

`/tmp/p09-40-riscv32-esp32c3/` — polymorphic Widget on
`riscv32imc-unknown-none-elf`:

- No ICE. Compiles clean.
- Ctor IR: `void @_ZN6WidgetC1Ei(ptr sret([8 x i8]) %_0, i32 %v)`.
  Matches Clang's Itanium output for `Widget::Widget(int)`.
- Vtable shape: `{ i32, ptr, ptr }` with 4-byte slots.
- Address-point offset: `gep vtable, i32 8` (offset-to-top + RTTI,
  2 × 4 bytes).
- Ctor body: `store vtable+8, %_0` at offset 0 (vptr init) and
  `store %v, %_0+4` (field init).
- Symbols: `_ZN6WidgetC1Ei`, `_ZNK6Widget3fooEv`, `_ZTV6Widget`,
  `_ZTI6Widget`, `_ZTS6Widget` — all Itanium-correct.

Regression checks: workspace `cargo test --workspace` → 235/0.
`examples/bare_metal_arm` (P09.36) still builds clean on
`thumbv7em-none-eabihf` — ARM Cortex-M path unaffected because
only the RiscV dispatch branch in `mod.rs` was changed.

### Coverage

Applies to rv32 and rv64 (one code change, both architectures):

- `riscv32imc-unknown-none-elf` — **ESP32-C3** (tested).
- `riscv32imac-unknown-none-elf` — **ESP32-C6 / H2**.
- `riscv32imafc-unknown-none-elf` — **ESP32-P4**.
- `riscv32imc-esp-espidf` / `riscv32imac-esp-espidf` /
  `riscv32imafc-esp-espidf` — ESP-IDF std targets.
- Any other rv32 / rv64 ELF target using the Itanium C++ ABI.

Xtensa ESP32 (S3, original ESP32) remains out of reach — no
upstream rustc Xtensa target.

### Takeaway for memory

Before estimating "new target support," probe end-to-end
including polymorphism. The same "zero-code path that carried ARM
Cortex-M" did not extend to rv32 — polymorphism exposed an
unimplemented psABI case in upstream that needed a ~50 LOC fork
patch. Probe → diagnose → fix was ~3 hours.

---

## Build & test

See [`build.sh`](build.sh) and [`VERIFY.md`](VERIFY.md). Expected
timeline:

- Clone + apply patches: **~10 min**.
- Stage-1 build (`./x.py build --stage 1`): **30–90 min** on a
  dev laptop, longer on CI.
- Bootstrapping through the rustc test suite: **hours**, and
  most UI tests are irrelevant; we care about a narrow slice of
  `tests/codegen/`, `tests/ui/abi/`, `tests/ui/layout/`.

A realistic first-fork session is: apply P01–P05 only (the
non-codegen patches), build, verify `rustc` accepts `#[repr(cpp)]`
without erroring, stop there. P06–P08 are the weeklong grind.
