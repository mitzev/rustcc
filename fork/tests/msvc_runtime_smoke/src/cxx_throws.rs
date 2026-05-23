// v1.13.0 P09.67b: MSVC funclet smoke test for cxx_throws.
//
// Exercises the catch_switch + catch_pad funclet path under
// Wine. The C++ side (cpp/maybe_throws.cpp, compiled by
// build.rs into maybe_throws.lib) throws on negative input;
// the Rust side catches via #[rustc_cxx_throws] and reports
// the outcome via exit code.
//
// Exit code:
//   3 = both arms behaved correctly (Ok(10) + Err)
//   4 = happy path returned wrong value
//   5 = happy path returned Err
//   6 = throw path returned Ok
//   7 = throw path returned Err but unexpected kind

#![feature(rustc_attrs)]
#![allow(internal_features, dead_code)]
#![no_std]
#![no_main]

// MSVC C++ exception machinery + CRT symbols. libcmt (static
// CRT) drags in security cookie / UEF helpers from kernel32;
// _CxxThrowException + __CxxFrameHandler3 + __std_exception_*
// come from vcruntime; memset / free come from libcmt. xwin
// stages these libs under crt/lib/x86_64 and sdk/lib/um/x86_64.
// (The -L paths are in .cargo/config.toml.)
#[link(name = "kernel32")]
#[link(name = "vcruntime")]
#[link(name = "libcmt")]
#[link(name = "libcpmt")]
#[link(name = "msvcrt")]
#[link(name = "ucrt")]
unsafe extern "C" {}

use core::ffi::{c_char, c_void};

#[repr(C)]
#[derive(Copy, Clone)]
pub struct CxxRawError {
    pub kind: u32,
    pub message: *const c_char,
}

// Inline runtime helper. The MSVC catch_switch+catch_pad
// funclet calls this with the exception ptr; the funclet
// has already begun the catch (catch_pad), so we don't need
// __cxa_begin_catch / __cxa_end_catch on MSVC — the funclet
// scope handles it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rustcc_cxx_catch_unknown(
    _exc_ptr: *mut c_void,
) -> CxxRawError {
    static MSG: &[u8] = b"caught unknown\0";
    CxxRawError {
        kind: 2, // CXX_EXC_UNKNOWN
        message: MSG.as_ptr() as *const c_char,
    }
}

unsafe extern "C" {
    #[rustc_cxx_throws]
    fn maybe_throws(x: i32) -> Result<i32, CxxRawError>;
}

#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> i32 {
    // Happy path: 5 -> Ok(10)
    //
    // NOTE: we deliberately read `e.kind` (via `if e.kind == ?`)
    // even though we don't expect this arm to fire — otherwise
    // LLVM proves the catch path's helper return is unused and
    // optimizes the catchpad body down to `unreachable`. When
    // the catchpad is `unreachable`, runtime unwinding into it
    // crashes instead of returning our planned exit code.
    match unsafe { maybe_throws(5) } {
        Ok(v) => {
            if v != 10 {
                return 4;
            }
        }
        Err(e) => {
            // Force-read e.kind so the catchpad body is kept.
            if e.kind == 0xDEAD {
                return 99;
            }
            return 5;
        }
    }

    // Throw path: -1 -> Err
    match unsafe { maybe_throws(-1) } {
        Ok(_) => return 6,
        Err(e) => {
            if e.kind != 2 {
                return 7;
            }
        }
    }

    3
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
