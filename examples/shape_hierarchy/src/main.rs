//! rustcc `class` keyword walkthrough — a small Shape hierarchy
//! demonstrating four features the fork adds on top of stock rustc:
//!
//! 1. **The `class` keyword** (P09.30 / P09.39) — syntactic sugar
//!    for a `#[repr(cpp)]` struct + inherent `impl` block, with
//!    body members laid out C++-style so the type is wire-
//!    compatible with an Itanium-C++-ABI class of the same shape.
//!
//! 2. **Single inheritance** via `class D : B { ... }` (P09.32) —
//!    the parser synthesizes a leading `__base: B` field with
//!    `#[rustc_cxx_base]`, and the compiler lays out the derived
//!    class with the base subobject at offset 0 plus a shared
//!    vtable-pointer slot.
//!
//! 3. **Method-modifier keywords** (v1.13.5) — `constructor fn`,
//!    `virtual fn`, and `override fn` inside a `class` body, instead
//!    of the `#[constructor]` / `#[cpp_virtual]` attributes. `override`
//!    is verified: it's a compile error if no base virtual of that
//!    name exists. (Identical machine code to the attribute forms.)
//!
//! 4. **Transparent base-member access** (v1.13.6) — a derived class
//!    reaches base fields/methods as `self.field` (via an
//!    auto-synthesized `Deref` to the `__base` subobject), so the
//!    `self.__base.` prefix is no longer needed for reads.
//!
//! The vtable isn't used for Rust-side method calls (Rust's
//! `shape.area()` is statically dispatched by the compiler), but it
//! IS used when C++ consumers call through a base pointer. See
//! `examples/virtual_override/` for the Rust-defines-class,
//! C++-calls-through-base-pointer round-trip; this example focuses on
//! the Rust-side API ergonomics.

#![feature(rustc_attrs)]
#![allow(internal_features)] // class/ctor/virtual attrs ride rustc_attrs

// Base class.
pub class Shape {
    tag: u32,

    pub constructor fn new(tag: u32) -> Self {
        Shape { tag }
    }

    // Base "area" returns 0.0. Derived classes override at the
    // vtable level — and for direct-typed Rust callers,
    // override at the impl-block level via the derived class's
    // own method of the same name.
    pub virtual fn area(&self) -> f64 {
        0.0
    }

    pub virtual fn name_tag(&self) -> u32 {
        self.tag
    }
}

// Rectangle — adds width/height, overrides area + name_tag.
pub class Rectangle : Shape {
    width: f64,
    height: f64,

    pub constructor fn new(tag: u32, width: f64, height: f64) -> Self {
        // Construction still names `__base` explicitly — the base
        // subobject has to be initialized. Transparent access is for
        // *reads* (`self.tag`), not for the struct literal.
        Rectangle {
            __base: Shape::new(tag),
            width,
            height,
        }
    }

    // `override fn` is verified against `Shape::area` (a base virtual
    // of the same name); it fills the base's vtable slot + the
    // Rust-level method.
    pub override fn area(&self) -> f64 {
        self.width * self.height
    }

    // `self.tag` reaches the base field transparently (v1.13.6) — it
    // resolves through the auto-`Deref` to the `__base` subobject the
    // parser inserted via the `: Shape` syntax.
    pub override fn name_tag(&self) -> u32 {
        self.tag + 10_000
    }
}

// Circle — adds radius, overrides area + name_tag.
pub class Circle : Shape {
    radius: f64,

    pub constructor fn new(tag: u32, radius: f64) -> Self {
        Circle {
            __base: Shape::new(tag),
            radius,
        }
    }

    pub override fn area(&self) -> f64 {
        core::f64::consts::PI * self.radius * self.radius
    }

    pub override fn name_tag(&self) -> u32 {
        self.tag + 20_000
    }
}

fn main() {
    println!("shape hierarchy demo — Rust-side method calls");
    println!("-----");

    // Construct each shape directly. Because Rust method
    // resolution is static, we call each type's own `area()`
    // and `name_tag()` by calling on the concrete type.
    let base = Shape::new(99);
    let rect = Rectangle::new(1, 3.0, 4.0);
    let rect2 = Rectangle::new(3, 2.5, 2.5);
    let circ = Circle::new(2, 5.0);

    println!(
        "  base (Shape)   : name_tag={:5}  area={:.4}",
        base.name_tag(),
        base.area()
    );
    println!(
        "  rect (Rectangle): name_tag={:5}  area={:.4}",
        rect.name_tag(),
        rect.area()
    );
    println!(
        "  rect2 (Rectangle): name_tag={:5}  area={:.4}",
        rect2.name_tag(),
        rect2.area()
    );
    println!(
        "  circ (Circle)   : name_tag={:5}  area={:.4}",
        circ.name_tag(),
        circ.area()
    );

    let total_area = base.area() + rect.area() + rect2.area() + circ.area();
    println!("-----");
    println!("total area: {total_area:.4}");

    // `rect.name_tag()` reads `self.tag` (a Shape field) transparently
    // through the derived class's auto-`Deref` — no `__base` in sight.

    // Sanity checks — if any of these panic, the class
    // inheritance + method-resolution machinery has regressed.
    assert!((rect.area() - 12.0).abs() < 1e-9, "3*4 = 12");
    assert!(
        (circ.area() - (core::f64::consts::PI * 25.0)).abs() < 1e-9,
        "pi*r^2 for r=5"
    );
    assert!((rect2.area() - 6.25).abs() < 1e-9, "2.5*2.5 = 6.25");
    assert_eq!(base.area(), 0.0);

    assert_eq!(rect.name_tag(), 10_001);
    assert_eq!(circ.name_tag(), 20_002);
    assert_eq!(rect2.name_tag(), 10_003);
    assert_eq!(base.name_tag(), 99);

    println!("ok: class keyword + inheritance + keyword modifiers + transparent base access");
    println!("(virtual dispatch through a base pointer is exercised in examples/virtual_override/)");
}
