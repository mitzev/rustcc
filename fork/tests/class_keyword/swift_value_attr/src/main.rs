// Regression test: `#[swift_value]` built-in attribute macro
// (P09.46, 1.02 #2). Drop + Clone + metadata extern are auto-
// synthesized by the compiler; users no longer need the
// `rustcc_macros::swift_value!` wrapper.
//
// Covers the class-backed non-POD extras case (the harder
// of the two expansion paths; P09.42's per-field Clone
// semantics are preserved).

#![feature(rustc_attrs)]

#[swift_value]
#[swift_type = "Foo.Bar:class"]
pub struct Bar {
    _ptr: *mut core::ffi::c_void,
    extra: Box<i32>,
}

// Stub Swift runtime symbols so the binary links without a real
// Swift stdlib. Null pointers skip the retain/release calls.
#[unsafe(no_mangle)]
pub extern "C" fn swift_retain(p: *mut core::ffi::c_void) -> *mut core::ffi::c_void {
    p
}
#[unsafe(no_mangle)]
pub extern "C" fn swift_release(_p: *mut core::ffi::c_void) {}

fn main() {
    let b = Bar {
        _ptr: core::ptr::null_mut(),
        extra: Box::new(42),
    };
    let c = b.clone();
    assert_eq!(*c.extra, 42);
    drop(c);
    assert_eq!(*b.extra, 42);
    drop(b);
    println!("ok: swift_value built-in attr expanded correctly");
}
