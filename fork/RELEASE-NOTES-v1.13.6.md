# rustcc v1.13.6 — transparent base-member access + RTTI/layout fixes

**A fork-toolchain release.** v1.13.6 makes inherited members reachable
directly on a derived `class` — `self.member` instead of
`self.__base.member` — and fixes a set of latent RTTI/layout bugs for
classes that inherit from a non-polymorphic base. The fork rustc patch
series grows by one (patch 38), so the toolchain tarballs are **rebuilt
from the patched compiler** (patches 01–38).

## What ships

### A. Transparent base-member access

For a derived class `class D : B { ... }`, the fork now synthesizes two
ordinary trait impls targeting the `__base` subobject:

```rust
impl Deref    for D { type Target = B; fn deref(&self)        -> &B     { &self.__base } }
impl DerefMut for D {                  fn deref_mut(&mut self) -> &mut B { &mut self.__base }
```

So base fields and methods are reachable directly through Rust's
existing autoderef — no `self.__base.` prefix:

```rust
let mut d = Derived::new(10, 5);
let _ = d.x;          // base FIELD       (was: d.__base.x)
let _ = d.get_x();    // base METHOD      (was: d.__base.get_x())
d.x = 7;             // base field WRITE  (via DerefMut)
takes_base(&d);       // &Derived -> &Base UPCAST coercion
```

- Works **transitively** up a multi-level chain (`C : B : A`) and
  through **generic** bases (`D<T> : G<T>`).
- A derived field **shadows** a base field of the same name (the
  derived one wins — matches C++ name-hiding); `self.__base.member`
  still works for explicit access.
- Zero runtime cost: it's plain autoderef, so the emitted code is just
  a field projection at offset 0 (the base subobject).

Implementation: the parser builds the two impls as complete AST items
stored on `ast::Class`; they flow through the **normal** def-collection
/ name-resolution / lowering paths (expansion fills their node ids,
`index_crate` registers them as owners via the Class walk,
`lower_item_ref` emits their `ItemId`s), so there is **no custom HIR
synthesis** — base access falls out of the autoderef machinery rustc
already has.

### B. RTTI + layout fixes for non-polymorphic bases

Three latent bugs are fixed for classes that inherit from a base with
no virtuals of its own:

- **`is_polymorphic_cpp_class` now recurses the base chain.** A
  `#[rustc_cxx_base]` to a *non-polymorphic* base no longer makes the
  derived class polymorphic. This fixes a link error — *"undefined
  symbol: typeinfo for Base"* — that fired whenever a derived class
  with a `constructor`/virtual inherited from a base that declared no
  virtuals (the derived emitted an `__si_class_type_info` referencing
  a `_ZTI` the base never emitted).
- **`has_polymorphic_base` (layout) now requires the base to be
  polymorphic.** A class that introduces the *first* virtual on top of
  a plain (non-polymorphic) base now correctly reserves its **own**
  vptr instead of assuming the base provides one — previously it
  miscomputed field offsets and produced garbage at runtime.
- **`emit_typeinfo` emits a non-polymorphic base's `_ZTS`/`_ZTI` on
  demand** (idempotently, recursing the chain), so a polymorphic
  derived class whose base is non-polymorphic links.

Net effect: all single-inheritance shapes — POD-base data inheritance,
polymorphic base, first-virtual-in-derived, and multi-level mixes —
compile, link, and run with correct layouts.

### C. Tests

- New runtime probe `fork/tests/class_keyword/base_member_access`
  exercises transparent field read/write, base method calls, `&D→&B`
  upcast, and the non-polymorphic-base + first-virtual-in-derived case.
  The class-keyword probe matrix is now **11/11** (6 cpp_class + 5
  swift).
- Validated additionally: multi-level inheritance, generic bases,
  field shadowing, and correct vptr offsets across all chain shapes.

## Editor support (known limitation)

rust-analyzer (fork) models `class` natively (not desugared to
struct + impl), and its class field/method resolution does not yet
walk the base chain. So the new transparent form (`self.base_member`)
may show an unresolved-field/method diagnostic **in the editor** even
though it compiles and runs. Workaround: use `self.__base.member`,
which resolves in both. A follow-up will teach the RA fork to resolve
the transparent form.

## Toolchain

Fork patch series grows to **patch 38
(`38-class-transparent-base-access.patch`)**: parser synthesis of the
Deref/DerefMut impls + the def-collection/resolve/lowering plumbing,
plus the RTTI/layout fixes in `rustc_symbol_mangling`,
`rustc_ty_utils` (layout bridge), and `rustc_codegen_llvm`. Patches
01–37 are unchanged. **Prebuilt tarballs for v1.13.6 are rebuilt from
the patched compiler** (5 triples).

## Test status

Full workspace green; the `fork/tests/class_keyword` probe matrix is
11/11.
