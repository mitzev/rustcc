# rustcc v1.04.0

**The big "C++ → Rust import" release.** Closes Phase A, delivers all of Phase B (FLTK Hello-World minus free-function plumbing), and ships every Phase C milestone (M15–M21). Validated end-to-end against real FLTK 1.4.5 headers — the importer pulls in 13 classes / 26 with transitive forward-decls and emits 3,600+ lines of valid Rust bindings, all Phase A+B+C features exercised on real C++.

## What's new since v1.03.0

### Phase A — finish (M9 + M10)

- **M9 — diagnostics + poison nodes** (#6). `ImportError::UnsupportedFeature` gained a `span: Option<SourceSpan>` field; `unsupported_at(span)` constructor surfaces the file/line/col. The importer mints **poison nodes** for forward-only or recoverably-failed lowerings — empty `ClassDef` with a side-table `poison_reason` string. Bindings emitter renders poisoned classes as opaque `pub struct` with the reason in a `///` doc comment instead of aborting the whole TU.
- **M10 — incremental cache** (#6). `Driver::load_or_parse(cache_path, …)` short-circuits when a serialized `(CxxTypeCtx, AnnotationSet, ClassIds)` matches the SHA-256(crate version + libclang version + clang argv + per-file digests) key. New `cxx_importer/Cargo.toml` `cache` feature gates the serde + sha2 deps.

### Phase B — FLTK Hello-World (M11–M14)

- **M11.a — static methods on classes** (#7). `ctx.mark_method_static(class, idx)` side-table flags `Fl::run()`-style receiver-less methods. Bindings emitter routes them through a new `EmissionKind::Static` path with no `this` slot. (M11.b free-function support and M11.c static-data-member support are deferred to a follow-up.)
- **M12 — `#define` constant capture** (#7). New `cxx_importer::macros` module with `MacroSet` / `MacroConst` / `MacroValue`. Tokenize-and-parse approach (`Cursor_Evaluate` doesn't fire on macro defs). Captures `FL_RED`, `FL_NORMAL_LABEL`, `FL_UP_BOX`, etc. Emitted as `pub const NAME: T = VALUE;` at the top of generated bindings.
- **M13 — forward-decl upgrade-on-later-definition** (#7). When the importer sees a forward-only `class Fl_Widget;` it mints a poison node; if a later TU includes the full body, the placeholder gets upgraded in place via `class_mut` and `unpoison` instead of duplicating.
- **M14 — heap-allocation thunks + `CxxHeap<T>`** (#7). New `crates/cxx/src/heap.rs` with `CxxHeap<T: ?Sized + CxxDeletable>`. Per-class `__cxx_<class>_new_heap_<i>(args) -> *mut Class` and `__cxx_<class>_delete(p)` extern-C thunks paired with a `pub fn new_boxed(...) -> ::cxx::CxxHeap<Self>` wrapper. FLTK widgets MUST be heap-allocated (the parent tree owns by pointer); this is what makes `new Fl_Window(340, 180)` work.

### Phase C — FLTK useful subset (M15–M21)

All seven milestones in one PR (#8), plus an end-to-end FLTK validation in (#9):

- **M15 — function pointer types + `CxxCallback<F>`**. Importer collapses pointer-to-function-prototype to a bare `CxxType::Fn(FnSig)`. Renderer emits `Option<unsafe extern "C" fn(...) -> ret>`. New `::cxx::CxxCallback<F>` runtime helper boxes a Rust closure into the `(extern "C" fn, *mut c_void)` shape C++ APIs (e.g. FLTK's `Fl_Callback`) expect.
- **M16 — `enum class` + plain `enum` body lowering**. Two emission shapes selected per-enum: `#[repr(int)] pub enum` (scoped + unique discriminants) or `#[repr(transparent)] pub struct + assoc consts` (unscoped or aliasing variants — the safe default for flag-style enums). FLTK's 40+ `Fl_Event::FL_*` constants land cleanly through the second path.
- **M17 — type aliases (`using` / `typedef`)**. New `aliases.rs` module + `AliasSet` side-table. Renderer emits `pub type X = Y;` inside the matching `pub mod`. New entry point `import_header_with_extras` returns `(classes, ImportExtras { annotations, aliases, enums })`.
- **M18 — default-argument hint emission (count-only v0)**. Per-method side-table `ctx.default_arg_count(class, idx)` records the count of trailing parameters with C++ default values. Emitter prepends a `///` doc comment hint. Per-arity convenience wrappers (M18.b) tracked for a follow-up.
- **M19 — `CxxBase<T>` upcast emission**. One `impl ::cxx::CxxBase<Base> for Derived { ... }` per non-virtual base, with the offset baked in from `RecordLayout::base_offsets`. Offset-0 (single-inheritance) cases elide `.add(0)` for readability. Virtual bases stay deferred to M22.
- **M20 — configurable `c_char` rendering**. New `RustBindingsConfig::cstr_ergonomics: bool` (off by default, preserving the prior emission shape). When on, byte-int pointers render as `*[const|mut] ::core::ffi::c_char` so `CStr::as_ptr()` plugs in without casts.
- **M21 — bitfield-aware layout (probe-then-poison v0)**. `rustc_abi_cxx::layout` doesn't model Itanium bit-packing; until proper packing lands, the importer poisons any class containing a bitfield with a clear M21 reason. Emission produces an opaque `pub struct` + reason doc-comment instead of layout that mismatches at runtime. FLTK doesn't use bitfields so this is a clean path; tracked as M21.b.

### FLTK end-to-end probe (#9)

`examples/fltk_hello/` validates the import + emit pipeline against real FLTK 1.4.5 headers. Surfaces eight emitter-resilience fixes that emerged during the bring-up:

- `Driver::parse_all` Clang-instance reuse (libclang 17+ on macOS arm64 segfaults on per-header re-init).
- Per-method skip-with-comment (one bad method no longer fails the whole class).
- `CxxType::Enum` rendering (TU-scope by leaf name; class-scope by underlying int).
- Class-scope inner record / anonymous-union skip in the namespace tree.
- `r#`-keyword escape on user-facing `pub fn` names (FLTK has `widget->type()`, `box()`, `align()`, `try()`).
- Synthetic anonymous-name filter (`(unnamed enum at .../foo.h:42:1)` no longer leaks).
- Extern-decl dedup (two-pass importer no longer emits duplicate `__cxx_<class>_<method>` decls).
- Multi-ctor non-fatal (first ctor renders; later ones skip with a comment).

```
=== FLTK probe stats ===
  classes imported       : 13 healthy / 0 poisoned
  emitted bindings       : 26 classes, 3,616 lines, 211KB
```

## Breaking changes

None to user-facing APIs in `cxx_importer` or `rustc_abi_cxx`. The reorganization of `import_header_with_cache_and_aliases` (now a wrapper around the new `import_header_with_clang`) is `pub(crate)` only.

## Known gaps

These are deliberate v0 stops; tracked in `docs/cxx_importer.md` §15–§16:

- **M11.b / M11.c** — free functions + static data members at TU/namespace scope.
- **M22** — multi-inheritance with secondary vtables, virtual-base `this`-pointer adjustments. The skip-with-comment path catches these so the rest of the binding still works.
- **`cxx_importer::build`** — Cargo `build.rs` helper that drives `clang++` on the emitted shims and produces a `.a`. Closes the "click run, see a window" gap; not blocked on interop semantics.

## Prebuilt binaries

This release ships stage-1 toolchains for:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`

Install: see [`fork/INSTALL.md`](https://github.com/mitzev/rustcc/blob/main/fork/INSTALL.md). TL;DR:

```bash
TARGET=aarch64-apple-darwin   # pick yours
VERSION=v1.04.0
BASE="https://github.com/mitzev/rustcc/releases/download/$VERSION"
curl -fsSL -o rustcc.tar.xz "$BASE/rustcc-$TARGET.tar.xz"
mkdir -p "$HOME/.rustcc/$VERSION"
tar -xJf rustcc.tar.xz -C "$HOME/.rustcc/$VERSION"
rustup toolchain link rustcc "$HOME/.rustcc/$VERSION/stage1"
```

## Patch list cumulative through v1.04.0

The fork itself didn't change in v1.04.0 — all of Phases A+B+C live in `crates/cxx_importer`, `crates/rustc_abi_cxx`, and `crates/cxx`, none of which require the rustc fork to build. P09.50 (the v1.03.0 aarch64 sret fix) is still the most recent fork patch. See [`fork/PATCHES.md`](https://github.com/mitzev/rustcc/blob/main/fork/PATCHES.md) for the full list.

## Acknowledgements

The Phase B + Phase C sprint shipped under autonomous-merge mode, with each milestone landing as a self-contained commit and the user reviewing in batches. Eight successive green CI runs across Phase C (M15–M21) plus one for the FLTK probe (#9). Total Phase C delta: ~2,500 LoC; total Phase B delta: ~1,400 LoC.
