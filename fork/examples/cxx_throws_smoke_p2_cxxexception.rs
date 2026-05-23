#![feature(rustc_attrs)]
#![allow(internal_features)]

use std::os::raw::c_char;
use std::os::raw::c_void;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
#[rustc_diagnostic_item = "CxxRawError"]
pub struct CxxRawError {
    pub kind: u32,
    pub message: *const c_char,
}

// User-facing ergonomic wrapper. Carries the same bytes
// as CxxRawError but has a Display impl, a typed kind, etc.
#[derive(Debug)]
pub struct CxxException {
    pub kind: ExceptionKind,
    pub message_ptr: *const c_char,
}

#[derive(Debug)]
pub enum ExceptionKind {
    Unknown,
    Runtime,
    Logic,
}

impl From<CxxRawError> for CxxException {
    fn from(raw: CxxRawError) -> Self {
        CxxException {
            kind: match raw.kind {
                1 => ExceptionKind::Runtime,
                2 => ExceptionKind::Logic,
                _ => ExceptionKind::Unknown,
            },
            message_ptr: raw.message,
        }
    }
}

// Inline runtime helper for the smoke test.
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
        kind: 1, // Runtime
        message: MSG.as_ptr() as *const c_char,
    }
}

unsafe extern "C" {
    #[rustc_cxx_throws]
    fn maybe_throws(x: i32) -> Result<i32, CxxException>;
}

fn main() {
    let ok = unsafe { maybe_throws(5) };
    match ok {
        Ok(v) => println!("ok({})", v),
        Err(e) => println!("err(kind={:?})", e.kind),
    }
    let err = unsafe { maybe_throws(-1) };
    match err {
        Ok(v) => println!("ok({})", v),
        Err(e) => println!("err(kind={:?})", e.kind),
    }
}
