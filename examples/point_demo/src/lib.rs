// Example of the Rust-side surface rustcc exposes to C++.
//
// Won't compile under stable `rustc` because `#[repr(cpp)]` isn't
// recognized there. Runs through the rustcc driver's `rust-scan`
// phase instead, which parses this file with `rustcc_attr` and lowers
// the items into the IR. See the integration test in
// `crates/rustcc/tests/point_demo_flow.rs` for the full pipeline.

#[repr(cpp)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self {
        Point { x, y }
    }

    pub fn magnitude_sq(&self) -> i32 {
        self.x * self.x + self.y * self.y
    }

    pub fn translate(&mut self, dx: i32, dy: i32) {
        self.x += dx;
        self.y += dy;
    }
}

#[repr(cpp)]
pub struct Segment {
    pub start: Point,
    pub end: Point,
}

impl Segment {
    pub fn new(start: Point, end: Point) -> Self {
        Segment { start, end }
    }
}

// Rust-origin enum exposed to C++ as a scoped `enum class`. Default
// underlying type is `std::int32_t`; explicit discriminants are
// honored, and subsequent variants auto-increment from the last one.
#[repr(cpp)]
pub enum Orientation {
    North = 0,
    East = 90,
    South = 180,
    West = 270,
}

// User-defined Drop impl. Pre-fork the rust-stubs body just aborts;
// post-fork the rustc fork emits a real D1 dtor that runs this body
// when a `Point` goes out of scope on the C++ side too.
impl Drop for Point {
    fn drop(&mut self) {
        // Intentionally empty — the point of this impl is to signal
        // "not trivially destructible" to the pipeline.
    }
}
