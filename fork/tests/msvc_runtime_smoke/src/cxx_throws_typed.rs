// P09.68-msvc: MSVC typed-catch smoke test.
//
// Calls the C++ side's `maybe_throws_typed(x)` which throws
// `DomainError` for x=-1 and `RangeError` for x=-2, returns
// x*2 otherwise. The Rust side declares two typed catches via
// `#[rustc_cxx_throws_msvc_typedescs]` and dispatches on the
// kind tag the codegen packs into CxxRawError.
//
// Exit code:
//   3 = all three arms behaved correctly
//   4 = happy path returned wrong value
//   5 = happy path returned Err
//   6 = domain throw returned Ok
//   7 = domain throw returned Err but wrong kind
//   8 = range throw returned Ok
//   9 = range throw returned Err but wrong kind

#![feature(rustc_attrs)]
#![allow(internal_features, dead_code)]
#![no_std]
#![no_main]

use core::ffi::{c_char, c_void};

#[repr(C)]
#[derive(Copy, Clone)]
pub struct CxxRawError {
    pub kind: u32,
    pub message: *const c_char,
}

// MSVC C runtime + helpers from kernel32 (link directives
// also keep libcmt's dependencies resolvable).
#[link(name = "kernel32")]
#[link(name = "vcruntime")]
#[link(name = "libcmt")]
#[link(name = "libcpmt")]
#[link(name = "msvcrt")]
#[link(name = "ucrt")]
unsafe extern "C" {}

// Unused on MSVC (the catch funclet synthesizes the
// CxxRawError inline) but the codegen layer still emits an
// external declaration for it. Provide a no-op stub so the
// linker is happy.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rustcc_cxx_catch_unknown(
    _exc_ptr: *mut c_void,
) -> CxxRawError {
    CxxRawError { kind: 2, message: core::ptr::null() }
}

const CXX_EXC_TYPED_BASE: u32 = 16;

unsafe extern "C" {
    // MSVC TypeDescriptor mangled names for `DomainError`
    // and `RangeError` at the global namespace. Itanium
    // typeinfos are provided too so the same declaration
    // works on both targets — codegen picks per-target.
    #[rustc_cxx_throws]
    #[rustc_cxx_throws_typeinfos = "_ZTI11DomainError,_ZTI10RangeError"]
    #[rustc_cxx_throws_msvc_typedescs = ".?AVDomainError@@,.?AVRangeError@@"]
    fn maybe_throws_typed(x: i32) -> Result<i32, CxxRawError>;
}

#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> i32 {
    // Happy path.
    match unsafe { maybe_throws_typed(5) } {
        Ok(v) => {
            if v != 10 {
                return 4;
            }
        }
        Err(e) => {
            if e.kind == 0xDEAD {
                return 99;
            }
            return 5;
        }
    }

    // Domain throw.
    match unsafe { maybe_throws_typed(-1) } {
        Ok(_) => return 6,
        Err(e) => {
            if e.kind != CXX_EXC_TYPED_BASE + 0 {
                return 7;
            }
        }
    }

    // Range throw.
    match unsafe { maybe_throws_typed(-2) } {
        Ok(_) => return 8,
        Err(e) => {
            if e.kind != CXX_EXC_TYPED_BASE + 1 {
                return 9;
            }
        }
    }

    3
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
