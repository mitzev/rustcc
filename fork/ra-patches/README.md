# rust-analyzer fork patches

Parallel series to `fork/patches/` but against
`rust-lang/rust-analyzer` instead of `rust-lang/rust`. Delivers
the 1.02 #1 user-visible IDE deliverable: editor support for the
`class` keyword.

## Applying

```bash
git clone --filter=blob:none --depth=1 \
  https://github.com/rust-lang/rust-analyzer.git ~/rust-analyzer
cd ~/rust-analyzer
git am < /path/to/rustcc/fork/ra-patches/01-ra-class-keyword.patch
cargo build --release -p rust-analyzer
```

The resulting `target/release/rust-analyzer` binary is a drop-in
replacement for the upstream RA — point VS Code / other editor
at it by setting
`rust-analyzer.server.path` to that binary's path.

## Series

| File                        | Scope                                                |
|-----------------------------|------------------------------------------------------|
| `01-ra-class-keyword.patch` | P09.45 — Phase 1 parser support (CLASS node + CLASS_MEMBER_LIST). Files with `class` items stop producing cascading parse errors. Hover / go-to-def on the class name itself is deferred to Phase 2. |

## Phase 1 vs Phase 2

Phase 1 (shipped in this patch):
- Parser accepts `class Name<Generics>? (: Base)? { fields; methods }`.
- CLASS syntax node with CLASS_MEMBER_LIST child containing
  RECORD_FIELD and FN (and other assoc item) children.
- Non-exhaustive `match adt` sites in hir-expand, hir-def,
  hir/semantics, ide-assists, syntax updated with Phase-1-safe
  arms (mostly bail like union; extract fields where the
  existing logic can keep going).
- Inline parser test `class_item`. RA's own `cargo test -p
  parser` and `-p syntax` stay green (315/0 and 51/0).

Phase 2 (not in this patch):
- Synthesize a `Struct` + inherent `Impl` pair at the item-tree
  level so the class name becomes hoverable / findable /
  go-to-def-able.
- Wire method-inside-class call sites to the synthesized impl's
  methods so completion + signature help work.
- Potentially fork / extend rustc's `ItemKind::Class` name-
  resolution story into hir-def's own trait-method-resolution.

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
