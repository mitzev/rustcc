// Regression test: single inheritance (P09.32 / P09.39).
// `class D : B { ... }` synthesizes a leading `__base: B` field
// with `#[rustc_cxx_base]`; derived methods access base state
// through `self.__base`.

#![feature(rustc_attrs)]

pub class Base {
    x: i32,

    pub fn new(x: i32) -> Self {
        Self { x }
    }

    pub fn get_x(&self) -> i32 {
        self.x
    }
}

pub class Derived : Base {
    y: i32,

    pub fn new(x: i32, y: i32) -> Self {
        Self {
            __base: Base::new(x),
            y,
        }
    }

    pub fn sum(&self) -> i32 {
        self.__base.get_x() + self.y
    }
}

fn main() {
    let d = Derived::new(10, 5);
    assert_eq!(d.sum(), 15);
    println!("ok: inheritance sum = {}", d.sum());
}
