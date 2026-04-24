// Regression test: const generics on class header (P09.41).
// Before P09.41 / this fix, const-generic classes ICEd at
// `build_cxx_class_self_path` where the parser emitted
// `GenericArg::Type` for const params instead of
// `GenericArg::Const(AnonConst)`.

pub class Array<const N: usize> {
    pub data: [i32; N],

    pub fn sum(&self) -> i32 {
        let mut total = 0;
        let mut i = 0;
        while i < N {
            total += self.data[i];
            i += 1;
        }
        total
    }
}

fn main() {
    let a: Array<4> = Array { data: [1, 2, 3, 4] };
    assert_eq!(a.sum(), 10);
    println!("ok: const-generic class sum = {}", a.sum());
}
