//! rustcc `class` keyword walkthrough — a small Shape hierarchy
//! demonstrating three features the fork adds on top of stock rustc:
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
//! 3. **Virtual vtable emission** (P09.24 / P09.34) —
//!    `#[cpp_virtual]` methods land in the class's vtable. The
//!    vtable isn't used for Rust-side method calls (Rust's
//!    `shape.area()` is statically dispatched by the compiler),
//!    but it IS used when C++ consumers call through a base
//!    pointer. See `examples/virtual_override/` for the
//!    Rust-defines-class, C++-calls-through-base-pointer
//!    round-trip; this example focuses on the Rust-side API
//!    ergonomics.

#![feature(rustc_attrs)]

// Base class.
pub class Shape {
    tag: u32,

    #[constructor]
    pub fn new(tag: u32) -> Self {
        Shape { tag }
    }

    // Base "area" returns 0.0. Derived classes override at the
    // vtable level — and for direct-typed Rust callers,
    // override at the impl-block level via the derived class's
    // own method of the same name.
    #[cpp_virtual]
    pub fn area(&self) -> f64 {
        0.0
    }

    #[cpp_virtual]
    pub fn name_tag(&self) -> u32 {
        self.tag
    }
}

// Rectangle — adds width/height, overrides area + name_tag.
pub class Rectangle : Shape {
    width: f64,
    height: f64,

    #[constructor]
    pub fn new(tag: u32, width: f64, height: f64) -> Self {
        Rectangle {
            __base: Shape::new(tag),
            width,
            height,
        }
    }

    // Overrides Shape::area at the vtable slot + Rust-level.
    #[cpp_virtual]
    pub fn area(&self) -> f64 {
        self.width * self.height
    }

    // Derived classes can reach the base's fields through
    // `self.__base.<field>` — that's the synthesized base
    // subobject the parser inserted via the `: Shape` syntax.
    #[cpp_virtual]
    pub fn name_tag(&self) -> u32 {
        self.__base.tag + 10_000
    }
}

// Circle — adds radius, overrides area + name_tag.
pub class Circle : Shape {
    radius: f64,

    #[constructor]
    pub fn new(tag: u32, radius: f64) -> Self {
        Circle {
            __base: Shape::new(tag),
            radius,
        }
    }

    #[cpp_virtual]
    pub fn area(&self) -> f64 {
        core::f64::consts::PI * self.radius * self.radius
    }

    #[cpp_virtual]
    pub fn name_tag(&self) -> u32 {
        self.__base.tag + 20_000
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

    // Fields are accessible through `self.<field>` inside the
    // class body; base fields via `self.__base.<field>`. From
    // outside the class, the fields are private by default
    // (same visibility rules as a plain `struct`), so we use
    // the getters instead of touching them directly.

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

    println!("ok: class keyword + inheritance + vtable emission all compile and run");
    println!("(virtual dispatch through a base pointer is exercised in examples/virtual_override/)");
}
