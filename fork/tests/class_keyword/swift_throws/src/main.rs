// Regression test for `#[rustc_swift_throws]` + `SwiftError`
// wrapper type (P09.48 / 1.02 throws). Has two layers:
//
// 1. Compile-time: the binary uses `#[rustc_swift_throws]` on an
//    `extern "Swift"` decl. If the fork's attribute parser or
//    codegen regresses, this file stops compiling.
//
// 2. Runtime: exercise the Rust-side `SwiftError` ownership
//    semantics end-to-end (construction from a raw retained
//    pointer, round-trip via into_raw, null-handle check, Drop).
//    We don't actually invoke the throwing Swift function —
//    verifying the swifterror register convention at runtime
//    requires a real Swift stdlib. The IR-level test (presence
//    of `ptr swifterror` on the declaration) is the real codegen
//    check; see fork/PATCHES.md P09.48 for the IR sample.

#![feature(rustc_attrs)]

use rustcc_swift_rt::SwiftError;

// Attribute parse + codegen reach: this declaration must compile.
// We never call it.
unsafe extern "Swift" {
    #[rustc_swift_throws]
    #[link_name = "$s5MyLib8do_thingSiSiAA5InputVtKF"]
    #[allow(dead_code)]
    fn do_thing_raw(
        input: i64,
        err: *mut *mut core::ffi::c_void,
    ) -> i64;
}

// Stub Swift runtime so SwiftError::Drop can link.
#[unsafe(no_mangle)]
pub extern "C" fn swift_retain(p: *mut core::ffi::c_void) -> *mut core::ffi::c_void { p }
#[unsafe(no_mangle)]
pub extern "C" fn swift_release(_p: *mut core::ffi::c_void) {}

fn main() {
    // Construct a SwiftError from a retained raw pointer. We use
    // a non-null sentinel that's never dereferenced; our
    // swift_release stub is a no-op so Drop is safe.
    let sentinel = 0xDEAD_BEEF_usize as *mut core::ffi::c_void;
    let err = unsafe { SwiftError::from_retained(sentinel) };
    assert!(!err.is_null());
    assert_eq!(err.as_ptr() as usize, 0xDEAD_BEEF);

    // Round-trip through into_raw — should preserve the bits
    // and NOT call Drop (so no spurious swift_release).
    let raw = err.into_raw();
    assert_eq!(raw as usize, 0xDEAD_BEEF);

    // Null handle is treated as "no error".
    let null_err = unsafe { SwiftError::from_retained(core::ptr::null_mut()) };
    assert!(null_err.is_null());

    println!("ok: swift_throws wrapper round-tripped sentinel error");
}
