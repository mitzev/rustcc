// Regression test: transparent base-member access (P09.x / v1.13.6).
//
// For `class D : B { ... }` the fork synthesizes `impl Deref for D`
// + `impl DerefMut for D` targeting the `__base` subobject, so base
// members are reachable as `self.member` (field read/write) and
// `self.method()` through Rust's existing autoderef — no `self.__base.`
// prefix needed. `&D` also upcasts to `&B`.
//
// This probe also exercises the v1.13.6 RTTI/layout fix: a polymorphic
// class deriving from a *non-polymorphic* base (which used to fail to
// link with "undefined symbol: typeinfo for Base") and a class adding
// the first virtual on top of a plain base (which used to miscompute
// the vptr offset).

#![feature(rustc_attrs)]
#![allow(internal_features)]

pub class Base {
    x: i32,

    constructor fn new(x: i32) -> Self { Self { x } }

    virtual fn get_x(&self) -> i32 { self.x }

    fn set_x(&mut self, v: i32) { self.x = v; }
}

pub class Derived : Base {
    extra: i32,

    constructor fn new(x: i32, e: i32) -> Self {
        Self { __base: Base::new(x), extra: e }
    }

    fn combined(&self) -> i32 {
        // transparent: base field `x` + base method `get_x` + own field
        self.x + self.get_x() + self.extra
    }
}

// Non-polymorphic base; the derived class adds the first virtual.
pub class Plain {
    a: i32,
    constructor fn new() -> Self { Self { a: 100 } }
    fn ga(&self) -> i32 { self.a }
}

pub class Poly : Plain {
    b: i32,
    constructor fn new() -> Self { Self { __base: Plain::new(), b: 25 } }
    virtual fn gb(&self) -> i32 { self.b }
}

fn takes_base(b: &Base) -> i32 {
    b.get_x()
}

fn main() {
    let mut d = Derived::new(10, 5);
    assert_eq!(d.x, 10); // base field via auto-Deref
    assert_eq!(d.get_x(), 10); // base method via auto-Deref
    assert_eq!(d.combined(), 25);
    d.x = 40; // base field write via auto-DerefMut
    d.set_x(7); // base &mut self method via auto-DerefMut
    assert_eq!(d.x, 7);
    assert_eq!(takes_base(&d), 7); // &Derived -> &Base upcast

    // Non-polymorphic-base + first-virtual-in-derived: links + correct.
    let p = Poly::new();
    assert_eq!(p.a, 100); // base field through auto-Deref
    assert_eq!(p.ga(), 100); // base method through auto-Deref
    assert_eq!(p.gb(), 25);
    assert_eq!(p.a + p.b, 125);

    println!("ok: base member access x={} total={}", d.x, d.combined());
}
