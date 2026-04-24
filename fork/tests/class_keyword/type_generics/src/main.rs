// Regression test: type generics on class header (P09.41).
// Before P09.41, `class Pair<A, B>` ICEd at ast_lowering with
// "duplicate copy of DefId(Pair::A) in lctx.children" because
// struct and impl halves shared one generics value.

pub class Pair<A, B> {
    pub left: A,
    pub right: B,

    pub fn new(a: A, b: B) -> Self {
        Self { left: a, right: b }
    }
}

fn main() {
    let p: Pair<i32, &'static str> = Pair::new(3, "three");
    assert_eq!(p.left, 3);
    assert_eq!(p.right, "three");
    println!("ok: type-generic class left={}, right={}", p.left, p.right);
}
