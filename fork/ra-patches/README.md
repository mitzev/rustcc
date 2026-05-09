# rust-analyzer fork patches

Parallel series to `fork/patches/` but against
`rust-lang/rust-analyzer` instead of `rust-lang/rust`. Delivers
the 1.02 #1 user-visible IDE deliverable: editor support for the
`class` keyword.

## Applying

One command (recommended):

```bash
./fork/ra-patches/build.sh
```

Clones rust-analyzer at the [pinned commit](PINNED_COMMIT), applies every `??-*.patch` in lexical order, and runs `cargo build --release -p rust-analyzer`. Pass `--apply-only` to skip the build and just verify patches apply cleanly.

Manual recipe if you prefer:

```bash
git clone --filter=blob:none \
  https://github.com/rust-lang/rust-analyzer.git ~/rust-analyzer-rustcc
cd ~/rust-analyzer-rustcc
git checkout $(cat /path/to/rustcc/fork/ra-patches/PINNED_COMMIT)
git am /path/to/rustcc/fork/ra-patches/*.patch
cargo build --release -p rust-analyzer
```

The resulting `target/release/rust-analyzer` binary is a drop-in replacement for upstream RA. Point VS Code / other editor at it via `rust-analyzer.server.path`.

## Pinned commit

`PINNED_COMMIT` records the rust-analyzer master commit the patch series was authored against. Bumped on demand when patches need to track upstream changes — the rebase is part of any per-quarterly maintenance pass. See [`PHASE-2-PLAN.md`](PHASE-2-PLAN.md) for the upstream-churn risk discussion.

## Series

| File | Scope |
|---|---|
| `01-ra-class-keyword.patch` | P09.45 — Phase 1 parser support (CLASS node + CLASS_MEMBER_LIST). Files with `class` items stop producing cascading parse errors. |
| (planned) `02-ra-class-id.patch` | Phase 2 B.1 — `ClassId`, `ClassData`, `Adt::Class` variant + db queries |
| (planned) `03-ra-class-lowering.patch` | Phase 2 B.2 — item-tree lowering produces `Class` instead of bailing |
| (planned) `04..07-ra-match-arms-*.patch` | Phase 2 B.3 — match-arm sweep across hir-def / hir-ty / ide / ide-assists |
| (planned) `08-ra-class-resolve.patch` | Phase 2 B.4 — class-specific method dispatch + inheritance walk |
| (planned) `09-ra-class-assists.patch` | Phase 2 B.5 — class-aware refactoring assists |

See [`PHASE-2-PLAN.md`](PHASE-2-PLAN.md) for the full Phase 2 design + sub-deliverable breakdown.

## Phase 1 status

Phase 1 (shipped in `01-ra-class-keyword.patch`):
- Parser accepts `class Name<Generics>? (: Base)? { fields; methods }`.
- `CLASS` syntax node with `CLASS_MEMBER_LIST` child containing `RECORD_FIELD` and `FN` (and other assoc-item) children.
- Non-exhaustive `match adt` sites in hir-expand, hir-def, hir/semantics, ide-assists, syntax updated with Phase-1-safe arms (mostly bail like union; extract fields where the existing logic can keep going).
- Inline parser test `class_item`. RA's own `cargo test -p parser` and `-p syntax` stay green (315/0 and 51/0).

What Phase 1 does NOT deliver:
- Class names aren't hoverable / findable / go-to-def-able.
- Methods declared inside `class` blocks don't surface for completion.
- `class Dog : Animal` doesn't connect Dog to Animal.

Phase 2 (in progress) closes those gaps via a first-class `Adt::Class` HIR variant. See `PHASE-2-PLAN.md`.

## Running the probe

With the built binary at `~/rust-analyzer/target/release/rust-analyzer`:

```bash
echo 'pub class Widget {
    x: i32,
    pub fn new(x: i32) -> Self { Self { x } }
}' | ~/rust-analyzer/target/release/rust-analyzer parse
```

Expected: a `CLASS@...` node with `CLASS_MEMBER_LIST` child, no
`ERROR@...` nodes.
