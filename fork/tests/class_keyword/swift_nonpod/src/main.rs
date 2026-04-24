// Regression test: class-backed swift_value! with non-POD extra
// fields (P09.42 / 1.01 #2). Before P09.42, Clone did a byte-wise
// memcpy that aliased Box/String/Vec extras → double-free on drop.

#![feature(rustc_attrs)]

rustcc_macros::swift_value! {
    #[swift_type = "Foo.Bar:class"]
    #[repr(swift)]
    pub struct Bar {
        _ptr: *mut core::ffi::c_void,
        extra: Box<i32>,
    }
}

// Stub Swift runtime symbols so the binary links without a real
// Swift stdlib. The probe uses null class pointers, so these
// never actually execute.
#[unsafe(no_mangle)]
pub extern "C" fn swift_retain(p: *mut core::ffi::c_void) -> *mut core::ffi::c_void { p }
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

    println!("ok: non-POD extra cloned without aliasing");
}
