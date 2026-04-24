# shape_hierarchy — rustcc `class` keyword demo

A small polymorphic Shape hierarchy written entirely in Rust
using the rustcc fork's `class` keyword. Runs standalone — no
C++ side needed.

## What it exercises

| Feature | Source line | Patch |
|---|---|---|
| `class Name { fields; methods }` | `pub class Shape { ... }` | P09.30 / P09.39 |
| Single inheritance via `: Base` | `pub class Rectangle : Shape { ... }` | P09.32 |
| Base-subobject field access | `self.__base.tag` | P09.32 |
| Virtual methods | `#[cpp_virtual] pub fn area(...)` | P09.24 / P09.34 |
| Constructor method | `#[constructor] pub fn new(...)` | P09.22 / P09.33 |

## Running

```bash
cd examples/shape_hierarchy
RUSTC=/path/to/rust-lang-rust/build/host/stage1/bin/rustc \
  RUSTC_BOOTSTRAP=1 cargo +nightly build
./target/debug/shape_hierarchy
```

Expected output:

```
shape hierarchy demo — Rust-side method calls
-----
  base (Shape)   : name_tag=   99  area=0.0000
  rect (Rectangle): name_tag=10001  area=12.0000
  rect2 (Rectangle): name_tag=10003  area=6.2500
  circ (Circle)   : name_tag=20002  area=78.5398
-----
total area: 96.7898
ok: class keyword + inheritance + vtable emission all compile and run
```

## Why no polymorphic `Vec<Box<Shape>>` loop?

Rust's `shape.area()` is statically dispatched — the compiler
resolves the call against the receiver's declared type at
compile time. Even when you cast a `Box<Rectangle>` to a
`Box<Shape>`, calling `.area()` on the `Box<Shape>` binds to
`Shape::area`, not the Rectangle override. This is Rust's
default behavior and doesn't change with the `class` keyword.

The `#[cpp_virtual]` methods DO land in the class's vtable
(verifiable via `_ZTV<class>` in the emitted object file), but
the vtable is consumed by **C++ callers** that dispatch through
a base pointer. See `examples/virtual_override/` for the
round-trip where Rust defines the class and C++ calls through
a base pointer to hit the right override.

If you want Rust-level polymorphism across the class hierarchy,
the idiomatic pattern is to define a Rust trait that each class
implements and take `&dyn MyTrait` as the receiver:

```rust
trait Sized2D {
    fn area(&self) -> f64;
    fn name_tag(&self) -> u32;
}

impl Sized2D for Rectangle {
    fn area(&self) -> f64 { self.area() }
    fn name_tag(&self) -> u32 { self.name_tag() }
}
// ...then:
fn sum_areas(shapes: &[&dyn Sized2D]) -> f64 {
    shapes.iter().map(|s| s.area()).sum()
}
```

That's orthogonal to the `class` keyword — it's just regular
Rust trait objects — but the two compose: a `Rectangle` can
satisfy both Itanium-ABI virtual dispatch (for C++ consumers)
and Rust `dyn Trait` dispatch (for Rust consumers).

## What the linker pulls in

`build.rs` links `libc++` on macOS / `libstdc++` on Linux so
the Itanium `__cxxabiv1::__class_type_info` and
`__si_class_type_info` vtables referenced by the fork-emitted
`_ZTI*` / `_ZTV*` symbols resolve at link time.

## See also

- `examples/virtual_override/` — Rust-defined class called from
  C++ through a base pointer (demonstrates actual vtable
  dispatch).
- `examples/cpp_class_demo/` — the `#[cpp_class]` proc-macro
  form that compiles on stock rustc too.
- `fork/THREE-SURFACES.md` — when to use `class` vs
  `cxx_class!` vs `cxx_class_native!`.
