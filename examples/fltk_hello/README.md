# `examples/fltk_hello` — FLTK end-to-end probe

This crate validates the **`cxx_importer` → bindings emission** path
against a real, mid-sized C++ library: [FLTK 1.4.5](https://github.com/fltk/fltk).
It's a non-workspace crate (lives outside the main workspace's
member list) so its libclang dependency stays opt-in.

## What this demo proves

Run `cargo run --release` and watch the importer + emitter chew through
FLTK's main public headers:

```
=== Stats ===
  total classes imported : 13
  healthy                : 13
  poisoned               : 0

=== Bindings emission ===
  emitting 26 classes (13 requested + 13 forward-decl/transitive)
  bindings emitted ok: 3616 lines, 211568 bytes
  wrote /Users/.../examples/fltk_hello/generated_bindings.rs
```

The emitted `generated_bindings.rs` is **3,600+ lines of valid Rust** —
**every** Phase A + B + C feature gets exercised on real FLTK code:

| Phase | Feature | What FLTK exercises |
|-------|---------|---------------------|
| A     | Records, fields, methods | `Fl_Widget` (151 methods), `Fl_Group` (47), `Fl` (217) |
| A     | Single inheritance       | `Fl_Box : public Fl_Widget`, `Fl_Group : public Fl_Widget` |
| A     | Virtual methods + vtable | Most widget hierarchy methods are virtual |
| A     | Templates                | (skipped via libclang's TU traversal) |
| B     | M11.a static methods     | All 217 of `Fl::*` are static |
| B     | M12 `#define` capture    | `FL_RED`, `FL_NORMAL_LABEL`, `FL_UP_BOX` |
| B     | M13 forward-decl upgrade | `class Fl_Widget;` references in headers |
| B     | M14 heap shims           | `new Fl_Window(...)` widgets MUST be heap-allocated |
| C     | M15 fn-pointer types     | `Fl_Callback(Fl_Widget*, void*)` |
| C     | M16 enum bodies          | `enum Fl_Event { FL_NO_EVENT, ... }` (40+ variants) |
| C     | M17 type aliases         | `typedef unsigned int Fl_Color`, `using Fl_Callback = ...` |
| C     | M18 default-arg hints    | `Fl_Window(int w, int h, const char* title = nullptr)` |
| C     | M19 `CxxBase<T>` upcasts | `Fl_Box → Fl_Widget`, `Fl_Group → Fl_Widget`, ... |
| C     | M20 cstr_ergonomics      | `const char* label` etc. across the API |
| C     | M21 bitfield poison      | (FLTK doesn't use bitfields — clean path) |

Plus several emitter resilience features that landed during this
demo's bring-up:

- **Per-method skip-with-comment**: methods the v0 emitter can't
  render (copy/move ctors, conversion functions, virtuals without
  vtable_index, multi-ctors past the first) get a `///`-prefixed
  comment in the impl block instead of failing the whole class.
- **Class-scope inner record / anonymous-union skip**: Rust has no
  direct analog for `struct Outer { struct Inner; };` — the
  emitter drops the inner one with a comment.
- **`r#`-keyword escape**: FLTK has `widget->type()`, `widget->box()`,
  `widget->align()` — methods named after Rust keywords. The
  user-facing wrapper escapes those (the extern decl + Itanium
  symbol stay raw).
- **Synthetic-anonymous-name filter**: libclang reports anonymous
  enums as `(unnamed enum at /.../foo.h:42:1)`; the importer
  filters those out so they don't leak as invalid Rust idents.
- **Extern-decl dedup**: same logical method picked up twice
  (e.g. via in-class walk + post-pass `attach_methods_recursively`)
  used to produce two `__cxx_<class>_<method>` decls and trip
  Rust's "name defined multiple times" error. Now dedup'd.
- **Multi-header parse_all crash fix**: `Clang::new()` re-init
  per header has been observed to segfault libclang 17+ on macOS
  arm64. `Driver::parse_all` now hoists the `Clang` instance to
  the top of the loop and reuses it across all headers in one
  parse_all invocation.

## What this demo does NOT yet do

Running an actual window requires three more pieces, none of which
are blocked on the importer/emitter:

1. **Fork rustc available as a toolchain**. The bindings use
   `extern "C++"` which is fork-only (P09.50 sret routing). On
   this machine `rustup toolchain list` doesn't have a `rustcc`
   entry yet — `fork/build.sh` would build one but takes ~1h.
2. **`cxx` runtime crate dependency wired into Cargo.toml**.
   The bindings reference `::cxx::CxxBase`, `::cxx::CxxHeap`, and
   `::cxx::CxxDeletable`. Standalone `rustc generated_bindings.rs`
   reports "could not find `cxx`" — adding a `cxx = { path = ... }`
   dep resolves it.
3. **Shim compilation + FLTK link**. `cxx_importer::Driver::emit_shims`
   produces a C++ source file that compiles against the user's
   FLTK install. A real Cargo `build.rs` would invoke `clang++` on
   it, link `-lfltk -framework Cocoa`, and feed the resulting
   `.a` to rustc. The current demo stops short of that step
   because the resulting binary needs the fork rustc to compile.

The remaining gap to "click run, see a window" is **build-system
integration plumbing**, not interop semantics. Everything semantic
is already validated.

## Phase C → Phase D handoff

Concrete follow-ups discovered during this demo's bring-up:

- **M11.b**: free functions at TU/namespace scope (currently
  importer skips). Doesn't block FLTK Hello World — `Fl::run()`
  is a static method, not a free function — but `fl_message()`
  / `fl_color()` / `fl_message_title()` are free functions and
  would be needed for an interactive demo.
- **M22**: virtual methods that land in secondary vtables (multi-
  inheritance, virtual bases). FLTK keeps it simple — single
  inheritance from `Fl_Widget` — but `Fl_Window`'s ancestor chain
  involves `Fl_Group` which in turn might pick up secondary
  tables in some configurations. The skip-with-comment path
  catches these so the rest of the binding still works.
- **Build-system integration**: a real `cxx_importer::build`
  helper that drives `clang++` on the shims and produces a `.a`
  for the Cargo build. Would close the "run the demo" gap.

## Running it

```sh
cd examples/fltk_hello
brew install fltk            # one-time
cargo run --release          # ~8s first build, instant after

# To probe a different header set:
cargo run --release -- Fl_Box.H Fl_Widget.H

# To pit `Driver::parse_all` (shared USR cache) against a header
# (requires the multi-header parse_all crash fix that landed
# alongside this demo):
cargo run --release -- --driver Fl.H Fl_Window.H Fl_Box.H
```

The probe writes `generated_bindings.rs` next to the source —
inspect it to see the emitter output for a real C++ library.
