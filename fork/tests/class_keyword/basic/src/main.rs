// Regression test: class keyword — basic syntax (P09.30 / P09.39).
// Fields + methods compile; methods resolve via the synthetic
// inherent impl; constructor + inherent method are callable.

pub class Widget {
    x: i32,
    y: i32,

    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    pub fn sum(&self) -> i32 {
        self.x + self.y
    }
}

fn main() {
    let w = Widget::new(3, 4);
    assert_eq!(w.sum(), 7);
    println!("ok: basic class sum = {}", w.sum());
}
