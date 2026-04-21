//! End-to-end exercise for the `rustcc_macros` attribute macros.
//!
//! Compile-running this test proves:
//!
//! 1. `#[cpp_class]` expands to a form stable `rustc` accepts.
//! 2. The expanded struct has the layout we expect (`#[repr(C)]`).
//! 3. Regular Rust code can use the struct normally.

use rustcc_macros::cpp_class;

#[cpp_class]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[cpp_class]
pub struct Segment {
    pub start: Point,
    pub end: Point,
}

#[cpp_class]
pub enum Color {
    Red,
    Green,
    Blue,
}

#[test]
fn cpp_class_preserves_layout_and_construction() {
    let p = Point { x: 3, y: 4 };
    assert_eq!(p.x, 3);
    assert_eq!(p.y, 4);
    // `#[repr(C)]` guarantees this sizing; matches what rustcc's
    // layout engine reports for a 2×i32 struct.
    assert_eq!(std::mem::size_of::<Point>(), 8);
    assert_eq!(std::mem::align_of::<Point>(), 4);
}

#[test]
fn cpp_class_structs_compose_with_record_fields() {
    let s = Segment {
        start: Point { x: 0, y: 0 },
        end: Point { x: 1, y: 2 },
    };
    assert_eq!(s.end.y, 2);
    // 2 × Point (8 each) = 16.
    assert_eq!(std::mem::size_of::<Segment>(), 16);
}

#[test]
fn cpp_class_on_enum_compiles() {
    let c = Color::Green;
    match c {
        Color::Red => unreachable!(),
        Color::Green => (),
        Color::Blue => unreachable!(),
    }
}
