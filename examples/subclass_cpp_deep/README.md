# subclass_cpp_deep — Rust subclass of a *deep* imported C++ chain

A Rust `class MyWidget : Widget` that subclasses the **deepest** level of
an imported C++ single-inheritance chain and overrides virtuals
introduced at **every** level:

```
Shape  (virtual ~Shape, virtual area)        <- grandparent
  └─ Drawable  (virtual z_order)             <- parent
       └─ Widget  (virtual handle)           <- direct imported base
            └─ MyWidget  (Rust class)        <- overrides area + z_order + handle
```

This is the companion to [`../subclass_cpp_base`](../subclass_cpp_base)
(a *shallow*, one-level subclass). It exercises the **deep/multi-level
inheritance** support added in v1.13.8: `cxx_importer` flattens the whole
primary vtable into a single `#[rustc_cxx_imported_vtable]` attribute on
`Widget`, so the Rust override can reach a grandparent's virtual two
levels up, and the chain root's virtual destructor drives cross-boundary
destruction.

## What it proves

Run `./build_demo.sh` and the C++ `caller.cpp`:

1. Dispatches `area()` through a `Shape*` (grandparent), `z_order()`
   through a `Drawable*` (parent), and `handle()` through a `Widget*`
   (direct base) — **all land in the Rust `override`s**.
2. `delete (Shape*)widget` (through the grandparent pointer) runs the
   Rust `Drop`, the full C++ destructor chain (`~Widget`/`~Drawable`/
   `~Shape`), and frees — each exactly once (balanced `g_shape_ctor` /
   `g_shape_dtor` / `g_derived_drop` counters).

Expected output:

```
area=1005 z_order=2005 handle=3005 only_mine=5 | shape_ctor=1 shape_dtor=1 derived_drop=1 -- DEEP 3-level subclass + virtual dtor cross-boundary OK
```

## Build

```sh
cd examples/subclass_cpp_deep
./build_demo.sh        # RUSTC defaults to the fork stage1 rustc; LIBCLANG_PATH to Homebrew LLVM
```

Requires the **rustcc fork** toolchain (the `class` keyword) and libclang
for the binding generation step. See `build_demo.sh` for the three
phases.

## Note on abstract leaves

Here `Shape::area` is *concrete*, so `Widget` (and the Rust `MyWidget`)
is a concrete type and `delete`-through-base works end to end. If the
deepest imported base were itself **abstract** (an unoverridden pure
virtual) *and* C++ owned/`delete`d the object, destruction would need
that base's base-object destructor (`D2`), which clang doesn't emit for a
Rust-only subclass — see the limitation in
[`../../fork/RELEASE-NOTES-v1.13.8.md`](../../fork/RELEASE-NOTES-v1.13.8.md).
Cross-boundary *dispatch* is unaffected by that caveat.
