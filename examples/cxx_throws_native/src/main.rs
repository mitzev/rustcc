// Phase 2 native-invoke integration test.
//
// Declares a throwing C++ function with `#[rustc_cxx_throws]`
// and verifies both arms of the resulting Result work
// end-to-end:
//
//   1. Happy path returns the C++ value (P09.66 ABI bridge)
//   2. Catch path lands the exception via
//      __rustcc_cxx_catch_unknown (P09.61–P09.65) and the
//      MIR pass auto-converts CxxRawError → CxxException
//      via the From impl (P09.69).
//
// Requires the fork rustc with patches 21–31 applied.

#![feature(rustc_attrs)]
#![allow(internal_features)]

use cxx::{CxxException, CxxExceptionKind};

unsafe extern "C" {
    #[rustc_cxx_throws]
    fn maybe_throws(x: i32) -> Result<i32, CxxException>;
}

fn main() {
    // Happy path: positive input returns x*2.
    let ok = unsafe { maybe_throws(5) };
    match ok {
        Ok(v) => {
            println!("ok: {}", v);
            assert_eq!(v, 10, "happy path should return 5 * 2 = 10");
        }
        Err(e) => panic!("unexpected err on happy path: {:?}", e.kind),
    }

    // Catch path: negative input throws std::runtime_error.
    let err = unsafe { maybe_throws(-1) };
    match err {
        Ok(v) => panic!("unexpected ok on throw path: {}", v),
        Err(e) => {
            println!("err: kind={:?}", e.kind);
            // The runtime helper returns CxxExceptionKind::Unknown
            // (CXX_EXC_UNKNOWN = 0) when it can't classify; the
            // From<CxxRawError> impl maps it through.
            // We're not asserting the specific kind here because
            // it depends on whether the runtime can decode
            // std::runtime_error specifically — we just check
            // that *some* error came through and the program
            // didn't abort.
            let _ = e.kind; // suppress unused
        }
    }

    println!("phase-2 integration test passed");
}

// Silence: CxxExceptionKind is only used for the match.
#[allow(dead_code)]
fn _force_link(_k: CxxExceptionKind) {}
