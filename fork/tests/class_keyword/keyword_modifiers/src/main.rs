// Regression test: C++-style method-modifier keywords inside a
// `class` body (P09.x / v1.13.5).
//
//   (pub)? (virtual | override | constructor)* fn …
//
// These are pure parser sugar that desugars to the existing
// attributes:
//   * `constructor`           -> #[constructor]
//   * `virtual`               -> #[cpp_virtual]
//   * `override`              -> #[cpp_virtual] + #[rustc_cxx_override]
//
// `virtual`/`override` are reserved Rust keywords; `constructor` is a
// *contextual* keyword — only a modifier when a method follows, so a
// field literally named `constructor` still parses as a field (see
// `Base::constructor` below). `override` additionally triggers the
// verify-override check: it is a hard compile error if the method
// does not override a base-class virtual (covered by the negative
// probes documented in fork/tests/run.sh comments).

#![feature(rustc_attrs)]
#![allow(internal_features)]

pub class Base {
    // Field literally named `constructor` — exercises the contextual
    // disambiguation (must NOT be treated as a modifier here).
    constructor: i32,
    val: i32,

    constructor fn new(v: i32) -> Self {
        Self { constructor: 0, val: v }
    }

    virtual fn poke(&self) -> i32 {
        self.val
    }

    pub virtual fn doubled(&self) -> i32 {
        self.val * 2
    }
}

pub class Derived : Base {
    extra: i32,

    constructor fn new(v: i32, e: i32) -> Self {
        Self { __base: Base::new(v), extra: e }
    }

    // Overrides `Base::poke` — name matches a base virtual, so the
    // verify-override check is satisfied.
    pub override fn poke(&self) -> i32 {
        self.__base.val + self.extra
    }
}

fn main() {
    let b = Base::new(10);
    assert_eq!(b.poke(), 10);
    assert_eq!(b.doubled(), 20);
    assert_eq!(b.constructor, 0); // the field, not a modifier

    let d = Derived::new(10, 5);
    assert_eq!(d.poke(), 15);

    println!("ok: keyword modifiers virtual/override/constructor");
}
