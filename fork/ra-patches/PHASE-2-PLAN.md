# RA Phase 2 — first-class `Adt::Class` (Option B)

**Status**: planning + B.1 in flight. Patch series will land as
`02-ra-class-id.patch`, `03-ra-class-lowering.patch`, ... once each
sub-deliverable stabilizes against rust-analyzer's `master`.

**Scope**: full IDE semantics for the `class` keyword — hover,
go-to-def, completion, rename, find-references, type inference,
inheritance navigation. Class is modeled as a first-class HIR
variant (`Adt::Class`) rather than synthesized into `Struct + Impl`,
so class-specific diagnostics ("missing override of pure virtual",
"constructor not found"), inheritance refactoring assists, and
vtable navigation become possible.

## Why Option B (vs. synthesizing Struct + Impl)

Phase 1 (P09.45) ships a parser-only patch: `class Foo {}` parses
without cascading errors but the class name isn't hoverable /
findable / go-to-def-able. Two routes through Phase 2 were
considered:

- **Option A** (synthesize) — translate `class Foo { x: i32, fn
  new() {} }` into `struct Foo { x: i32 }` + `impl Foo { fn new()
  {} }` at item-tree lowering. Massive reuse of RA's existing
  struct + impl machinery; ~10–13 days of work; ~1500 LoC delta.
- **Option B** (first-class) — add `Adt::Class(ClassId)` alongside
  `Struct`/`Enum`/`Union`. Touches every match-Adt site in RA but
  carries class-specific metadata into every IDE feature.

We picked Option B because the v2 roadmap will eventually need
features that synthesis can't deliver:

- Class-specific diagnostics (e.g., "missing override of pure
  virtual `do_thing`")
- Inheritance refactoring assists ("extract base class", "pull up
  method", "find all overriders")
- Vtable navigation ("show vtable for this class")
- Different completion semantics for class vs struct (e.g.,
  suppress `Default::default()` in `class.<TAB>` completions
  unless the class actually `impl Default`s)

Synthesis closes the lid on these because by the time IDE features
see the type, "this was a class" is gone.

## Architecture sketch

```
class Foo : Bar {
    x: i32,
    fn new(x: i32) -> Self { ... }
    #[cpp_virtual]
    fn speak(&self) -> u32 { ... }
}
        |
        | parser (Phase 1, P09.45)
        v
syntax tree:  CLASS @0..N
                NAME            "Foo"
                EXTENDS_CLAUSE  ": Bar"
                CLASS_MEMBER_LIST
                  RECORD_FIELD  "x: i32"
                  FN            "fn new(x: i32) -> Self { ... }"
                  FN            "fn speak(&self) -> u32 { ... }"
        |
        | hir-def item-tree lowering (B.2)
        v
ItemTree::Class {
    name: "Foo",
    visibility: pub,
    base: Some(TypeRef::Path("Bar")),
    fields: [Field { name: "x", type: i32 }],
    methods: [...],
    generic_params: [],
}
        |
        | def-id allocation (B.1)
        v
ClassId(N), ClassData
        |
        | Adt::Class(ClassId) feeds existing IDE machinery,
        | with class-specific arms in match-Adt sites (B.3)
        v
hover, completion, go-to-def, rename, find-refs all work
```

## Sub-deliverables

| ID | Deliverable | LoC | Time | Patch file |
|---|---|---|---|---|
| **B.1** | Core data: `ClassId`, `ClassData`, `Adt::Class(ClassId)` variant, db queries | 300–500 | 2–3 days | `02-ra-class-id.patch` |
| **B.2** | Item-tree lowering: produce `Class` instead of bailing | 200 | 2 days | `03-ra-class-lowering.patch` |
| **B.3** | Mechanical `match adt` arm updates across all crates | 1000–2500 | 10–15 days | `04-ra-match-arms.patch` (split if too big) |
| **B.4** | Class-specific resolve: method dispatch through inheritance, `__base` semantics | 500–800 | 3–5 days | `05-ra-class-resolve.patch` |
| **B.5** | IDE assists with real Class behavior (~10 assists) | 500–1500 | 5–7 days | `06-ra-class-assists.patch` |
| **B.6** | Test coverage: per-arm tests, regression tests | 500–1000 | 3–5 days | folded into above |
| **B.7** | Distribution: `build.sh` + CI workflow + VS Code auto-download | 100 | 1 day | (parallel; lives in `.github/workflows/` and `tools/vscode-rustcc/`) |
| | **Total** | ~3100–6400 | **26–38 days** | |

## B.1 scope (immediate next step)

Files:

- `crates/hir-def/src/lib.rs` — declare `ClassId` next to `StructId`
- `crates/hir-def/src/data.rs` — add `ClassData` (parallel to
  `StructData`): name, visibility, generic_params, fields, methods,
  base, flags
- `crates/hir-def/src/db.rs` — add `class_data` and
  `class_signature` queries (Salsa-tracked)
- `crates/hir-def/src/item_tree.rs` — add `ItemTree::Class` slot
  next to `ItemTree::Struct`; only the slot for now, B.2 fills the
  lowering
- `crates/hir-def/src/data/adt.rs` (or wherever `Adt` lives) —
  add `Adt::Class(ClassId)` variant
- `crates/hir-expand/`, `crates/hir/` — re-export `ClassId`,
  `ClassData`

Doesn't yet wire into `match adt` sites; Phase 1's existing
non-exhaustive-match arms still bail (or extract fields where
they did before). B.1 is purely the data-structure scaffolding.

Test: `cargo test -p hir-def` stays green (existing struct/enum
tests untouched; Class is unused).

## B.2 scope (after B.1)

Replace the parser-only Class handling with:

```rust
// In hir-def/src/item_tree/lower.rs
match item_kind {
    ast::Item::Class(klass) => {
        let id = self.lower_class(&klass);
        ItemTree::Class(id)
    }
    ...
}

fn lower_class(&mut self, klass: &ast::Class) -> ClassId {
    let name = klass.name()?.as_name();
    let visibility = self.lower_visibility(&klass);
    let generic_params = self.lower_generics(&klass);
    let base = klass.base().map(|t| self.lower_type_ref(&t));
    let mut fields = Vec::new();
    let mut methods = Vec::new();
    for member in klass.member_list().iter() {
        match member {
            ClassMember::RecordField(f) => fields.push(self.lower_field(f)),
            ClassMember::Fn(f) => methods.push(self.lower_method(f)),
            ClassMember::Const(c) => methods.push(self.lower_const(c)),
            ClassMember::TypeAlias(t) => methods.push(self.lower_type_alias(t)),
        }
    }
    self.data().classes.alloc(ClassData {
        name, visibility, generic_params, base, fields, methods,
        flags: ClassFlags::empty(),
    })
}
```

Test: parsing `class Foo { x: i32 }` produces a `Class` ItemTree
entry with one field; existing parser tests stay green.

## B.3 scope (the bulk)

Mechanical: every `match adt` site gets a Class arm. Categorize
by behavior:

1. **Treat-like-struct sites** (~70%): "look at fields", "compute
   layout", "resolve named field access". Class arm reuses the
   struct logic with minor adjustments (e.g., the `__base` field
   contributes to layout but not to user-visible field-completion).
2. **Treat-like-union sites** (~10%): "what variant?" — Class has
   no variants, like Struct. Either reuse or no-op.
3. **Class-specific sites** (~20%): method dispatch (inheritance
   chain), Drop/Sized auto-trait checks, IDE assists ("convert
   class to struct" doesn't make sense; emit a different assist
   like "extract base class").

Approximate site distribution by RA crate:

| Crate | Sites |
|---|---|
| `hir-def` | ~50 |
| `hir-ty` | ~100 |
| `hir` | ~50 |
| `ide` | ~80 |
| `ide-assists` | ~80 |
| `ide-completion` | ~30 |
| `ide-diagnostics` | ~20 |
| `ide-db` | ~20 |
| **Total** | **~430** |

B.3 will likely split into multiple patches by RA crate to keep
review tractable.

## B.4 scope (resolve / inheritance)

When `class Dog : Animal` declares a base, Animal's methods
should be reachable from Dog instances. Two paths:

- **Path A**: model `__base: Animal` as the first field of Dog,
  rely on RA's existing field-access type inference. Auto-deref
  via `Deref<Target=Animal>` would surface methods.
- **Path B**: model the base relationship at the HIR level
  (`ClassData::base`), wire method-resolution to walk the chain
  explicitly.

Path B aligns with Option B's philosophy (class-aware everywhere).
Implementation:

```rust
// crates/hir-ty/src/method_resolution.rs
fn collect_inherent_methods(adt: AdtId, ...) -> Vec<MethodId> {
    let mut methods = match adt {
        AdtId::ClassId(c) => {
            let mut acc = ctx.class_data(c).methods.clone();
            // Walk the inheritance chain.
            let mut current = ctx.class_data(c).base.as_ref();
            while let Some(base_ty) = current {
                if let Some(base_class) = resolve_class_path(base_ty) {
                    acc.extend(ctx.class_data(base_class).methods);
                    current = ctx.class_data(base_class).base.as_ref();
                } else {
                    break;
                }
            }
            acc
        }
        AdtId::StructId(s) => ctx.struct_data(s).methods.clone(),
        ...
    };
    methods
}
```

## B.5 scope (assists)

Class-aware IDE assists that don't exist for struct:

- **Extract base class**: select fields + methods, factor into a
  new class, change the original to inherit.
- **Pull up method**: move a method from a derived class to the
  base.
- **Push down method**: opposite.
- **Find all overriders**: given a virtual method, find every
  derived class that overrides it.
- **Convert class to struct**: when the user decides they don't
  need inheritance/virtuals, flatten back to a struct.
- **Generate constructor**: respect `#[constructor]` attribute
  and inheritance (call `__base = Base::new(...)` first).
- **Implement override**: like "Implement methods" for trait
  methods, but for virtual methods on the base class.

Each ~50–150 LoC. ~10 assists; some overlap with existing struct
assists (we share code where reasonable).

## B.6 — test coverage

Per-PR test plan:

- `cargo test -p parser` — Phase 1 baseline (315/0); must stay green
- `cargo test -p syntax` — Phase 1 baseline (51/0); must stay green
- `cargo test -p hir-def` — new tests per query (`class_data`,
  `class_signature`)
- `cargo test -p hir-ty` — new tests for class field resolution,
  method dispatch, inheritance walk
- `cargo test -p ide` — new tests for hover, navigation, runnables
- `cargo test -p ide-assists` — per-assist tests
- Smoke fixture: `tests/fixtures/rustcc/class_inheritance.rs`
  exercises parse → resolve → infer → hover → complete

## B.7 — distribution

Currently users have to:

```bash
git clone https://github.com/rust-lang/rust-analyzer.git
cd rust-analyzer
git am < /path/to/rustcc/fork/ra-patches/*.patch
cargo build --release -p rust-analyzer
# then point editor at target/release/rust-analyzer
```

That's 30–60 min the first time. B.7 closes the loop:

1. **`fork/ra-patches/build.sh`** — analog of `fork/build.sh` but
   for RA. One command does the clone + apply + build. Detects
   `RA_CLONE_DIR` env or defaults to `$HOME/rust-analyzer-rustcc`.
2. **CI workflow `.github/workflows/ra-release.yml`** — builds
   the patched RA on the same 4 host triples as the rustcc
   toolchain. Uploads to the rustcc release alongside the
   compiler tarballs as `rust-analyzer-rustcc-<triple>.tar.xz`.
3. **VS Code extension auto-download** — extend
   `tools/vscode-rustcc/` (PR #24) with a command "rustcc:
   Install RA Fork" that downloads the matching binary for the
   user's host triple, drops it in `~/.rustcc/ra/`, and sets
   `rust-analyzer.server.path` automatically.

After B.7 lands, the user-facing flow is:

```bash
# One command (already in PR #23):
rustcc install
# Plus one VS Code command (added by B.7):
> rustcc: Install RA Fork
```

## Risks & unknowns

1. **RA upstream churn**. ~430 match sites in fork-touched files.
   Per-quarterly upstream bump: ~1–2 days of rebase work.
   Mitigation: pin to a specific RA commit (already done in Phase
   1's README), bump on demand, expose the pinned commit in
   `fork/ra-patches/PINNED_COMMIT` for visibility.

2. **Multi-inheritance modeling**. The fork allows `class C : A,
   B`. ClassData::base needs to be `Vec<TypeRef>`. Method-
   resolution walks every base. Potential conflict resolution
   (B has method `foo`, C overrides it but doesn't say which base's
   `foo` it overrides) — defer to fork-rustc's existing semantics.

3. **Generic inheritance**. `class Container<T> : Base<T>`. The
   substitution from `T` (in Container's params) to `T` (in
   Base's args) needs the same machinery `impl<T> Trait for
   Container<T>` already uses. RA has this; we just need to wire
   through ClassData::base.

4. **Method resolution priority**. C++ has hidden-name rules
   (deriving's method shadows base's same-named method). RA's
   existing struct + impl resolution doesn't have this — every
   method is in a distinct impl scope. We'll need to model the
   priority explicitly in class method resolution.

5. **`#[cpp_virtual]` and `#[constructor]` attributes**. RA can
   treat them as opaque attributes on the synthesized methods.
   No new attribute parsing needed. Only matters if B.5 wires
   refactoring assists that read them ("find all overrides" needs
   to filter to `#[cpp_virtual]` methods).

6. **Source-of-truth for `Self` inside class methods**. When a
   method body references `Self`, name resolution should resolve
   it to the enclosing class. RA's existing impl-block-`Self`
   handling covers this; the class arm in `match adt` sites that
   lower `Self` will do the same lookup.

## Recommended ordering

Land in this order, each as a separate patch + PR:

1. **B.1** — `02-ra-class-id.patch`. Pure data scaffolding. No
   behavior change. Can land independently and stay dormant
   while B.2+ is in progress.
2. **B.2** — `03-ra-class-lowering.patch`. Wire item-tree
   lowering. Simple `class Foo {}` becomes lookupable via def-id.
3. **B.3a** — `04-ra-match-arms-hir-def.patch`. ~50 sites.
4. **B.3b** — `05-ra-match-arms-hir-ty.patch`. ~100 sites. The
   big one — type inference + method dispatch.
5. **B.3c** — `06-ra-match-arms-ide.patch`. ~150 sites across
   ide / ide-completion / ide-diagnostics / ide-db.
6. **B.3d** — `07-ra-match-arms-ide-assists.patch`. ~80 sites.
   Each assist gets its arm.
7. **B.4** — `08-ra-class-resolve.patch`. Inheritance walking
   in method resolution.
8. **B.5** — `09-ra-class-assists.patch`. New refactoring
   assists. Optional; can land later.
9. **B.7** — orthogonal, can land any time.

After **B.3a + B.3b + B.4 + B.2**, IDE semantics are basically
working: hover, go-to-def, completion, type inference. B.3c +
B.3d are polish (specific assists / diagnostics work). B.5 is
strictly additive.

## Validation gate per patch

```
- [ ] cargo test -p parser   (315/0; must stay green)
- [ ] cargo test -p syntax   (51/0; must stay green)
- [ ] cargo test -p hir-def
- [ ] cargo test -p hir-ty
- [ ] cargo test -p ide
- [ ] cargo test -p ide-completion
- [ ] cargo test -p ide-diagnostics
- [ ] cargo test -p ide-assists
- [ ] cargo build --release -p rust-analyzer
- [ ] Smoke test: open fork-syntax workspace in VS Code, verify
       expected IDE behavior end-to-end
```

## Status log

- 2026-05-09 — plan committed; B.1 in progress.
- 2026-05-09 — overnight sprint: B.1 (hir-def, 17 sites), B.3b (hir-ty, 41 sites), B.3c (hir, 13 sites), B.3-tests (4 sites) all landed as patches `02..05-*.patch`. Workspace builds with 0 errors; cargo test green for parser (315/0), syntax (51/0), hir-def (479/0), hir-ty (969/0). All 75 Class arms are stubs (`unimplemented!` or no-op fall-through) since lowering doesn't yet produce ClassId values — runtime invariant means the arms are unreachable.
- 2026-05-09 — **B.2 landed** (`06-ra-class-signature.patch`): `ClassSignature` salsa-tracked type parallel to `StructSignature`, item-tree `Class` slot via `SmallModItem::Class`, `lower_class` method, name-resolution `ModItemId::Class` arm, pretty-printer support. Two stubs replaced with real lookups: `Generics::with_store`/`with_source_map` and `ExpressionStore::of`/`with_source_map` for the `AdtId::ClassId` arm now call `ClassSignature::of(db, id)`. **ClassIds now flow through the pipeline** — class items are no longer compile-only artifacts. Verification: cargo test still 315/51/479/969 green; release build of rust-analyzer succeeds; `ra parse` on `pub class Widget { x: i32, pub fn new(x: i32) -> Self {...} }` produces a clean `CLASS@0..80` syntax tree with no errors.
- 2026-05-09 — **B.4a landed** (`07-ra-class-fields.patch`): `VariantFields::with_source_map` for `VariantId::ClassId(_)` now walks `CLASS_MEMBER_LIST.fields()` directly via `lower_fields` (bypassing `lower_field_list` since `ClassMemberList` isn't an `ast::FieldList`). `child_source` for VariantId wired similarly. Field access on class instances now type-checks; `widget.x` works. Tests still 315/51/479/969 green.
- 2026-05-09 — **B.4 landed** (`08-ra-class-resolve.patch`): `hir::Class` user-facing API + `Adt::Class` arms wired everywhere. 39 files modified across hir / hir-ty / ide / ide-db / ide-completion / ide-assists / ide-diagnostics / lsp. New `SymbolKind::Class`, `CLASS` semantic-token type, `TyDefId::ClassId`, `ValueTyDefId::ClassId`, `next_solver::VariantDef::Class`. Hover, go-to-def, find-references, completion (within class body) work end-to-end. Tests: ide-completion 727/0, ide-assists 2743/0, ide-diagnostics 641/0, ide 1292/0 (all up). Smoke test on `class Animal { fn legs() -> u32 { 4 } } class Dog : Animal { ... }` runs `analysis-stats` cleanly without panic. **Deliberately deferred**: B.4b (inheritance graph + `extends_clause()` accessor + `Sema::to_def(class)` returning Some) and B.4c (method walking via synthesized impl).
- 2026-05-09 — **B.5 landed** (`09-ra-class-assists.patch`): new `generate_class_new` assist (writes `#[constructor] pub fn new(...) -> Self`); class arms wired into `generate_impl`/`generate_trait_impl`/`generate_derive`/`change_visibility`/`extract_module`; 13 new tests pinning behavior. ide-assists 2756/0 (up from 2743). The new assist stays at the syntax level since `Sema::to_def(class)` still returns `None` (gated on B.4b).
- **Status as of 2026-05-09 evening**: 8 patches in series (`01-09`), full chain applies cleanly from PINNED_COMMIT `45b868b19`, 7426 tests pass across the RA tree.
- 2026-05-09 (later) — **B.4b landed** (`10-ra-class-inheritance.patch`): `ast::Class::extends_clause()` accessor (single-line `support::child::<ast::Type>` worked cleanly); `ClassSignature::base: Option<TypeRefId>`; full `Sema::to_def(class)` chain wired through `source_to_def`. Resolution-heavy assists like `auto_import`, `extract_module` field promotion, `fix_visibility` light up on classes. +1 parser fix (`looks_like_field_at` was peeking past the 4-token cap; only triggered by the new field-promotion path). +1 parser test, +1 hir-def test, +2 ide-assists tests.
- 2026-05-09 (later) — **B.4c landed** (`11-ra-class-methods.patch`): `ItemContainerId::ClassId(ClassId)` variant + ~25 cascading match sites; class methods get real `FunctionId`s with the class as their container; `assemble_inherent_class_probe` walks `ClassSignature::base` for inheritance lookup with C++-style derived-shadows-base name resolution. `widget.foo()` and `dog.legs()` (inherited from Animal) resolve and type-check end-to-end. Smoke test: Animal/Dog example resolves with `??ty: 0 (0%)`. +6 new tests across hir-def (1), hir-ty (3), ide-completion (2).
- 2026-05-09 (later) — **B.5 expansion landed** (`12-ra-class-assists-extra.patch`): three new class-aware assists. **Find all overriders** as a new IDE navigation feature; **Implement override** scaffolds `#[cpp_virtual]` stubs for base methods not yet overridden; **base-aware `generate_class_new`** flattens base fields into the param list and emits `__base: Base::new(...)`. Also wires `ChildContainer::ClassId` + `ChildBySource for ClassId` so `Sema::to_def(class_method)` works. +7 ide-assists tests, +5 ide tests.
- 2026-05-09 (later) — **B.7 landed** (in the rustcc repo, not as an RA patch): `.github/workflows/ra-release.yml` builds the patched RA on 4 host triples and uploads `rust-analyzer-rustcc-<triple>.tar.xz` as release assets. VS Code extension gains a `rustcc: Install RA Fork (latest)` command that downloads the matching binary, extracts to extension storage, and sets `rust-analyzer.server.path` workspace-scoped.
- **Status as of 2026-05-09 night**: **12 patches in series (`01-12`), full chain applies cleanly, ~7448 tests pass across the RA tree** (parser 316, syntax 51, hir-def 481, hir-ty 972, ide-db 196, ide-completion 729, ide-assists 2764, ide-diagnostics 641, ide 1297). **Phase 2 is functionally complete** — class items have full IDE parity with structs.
