#![feature(rustc_attrs)]
#![allow(internal_features)]

use std::os::raw::c_char;
use std::os::raw::c_void;

#[repr(C)]
#[derive(Debug)]
pub struct CxxRawError {
    pub kind: u32,
    pub message: *const c_char,
}

// Stub the runtime helper inline. Real cxx crate runs
// __cxa_begin_catch/__cxa_end_catch on Itanium; for this
// smoke test we just acknowledge the catch.
unsafe extern "C" {
    fn __cxa_begin_catch(exc: *mut c_void) -> *mut c_void;
    fn __cxa_end_catch();
}

#[no_mangle]
pub unsafe extern "C" fn __rustcc_cxx_catch_unknown(
    exc_ptr: *mut c_void,
) -> CxxRawError {
    unsafe {
        let _obj = __cxa_begin_catch(exc_ptr);
        __cxa_end_catch();
    }
    static MSG: &[u8] = b"caught unknown\0";
    CxxRawError {
        kind: 42,
        message: MSG.as_ptr() as *const c_char,
    }
}

unsafe extern "C" {
    #[rustc_cxx_throws]
    fn maybe_throws(x: i32) -> Result<i32, CxxRawError>;
}

fn main() {
    let ok = unsafe { maybe_throws(5) };
    match ok {
        Ok(v) => println!("ok({})", v),
        Err(e) => println!("err(kind={})", e.kind),
    }
    let err = unsafe { maybe_throws(-1) };
    match err {
        Ok(v) => println!("ok({})", v),
        Err(e) => println!("err(kind={})", e.kind),
    }
}
