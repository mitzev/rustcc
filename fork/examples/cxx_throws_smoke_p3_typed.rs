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

const CXX_EXC_TYPED_BASE: u32 = 16;

#[derive(Debug)]
pub struct TypedError {
    pub which: TypedKind,
}

#[derive(Debug)]
pub enum TypedKind {
    Domain,
    Range,
    Unknown,
}

impl From<CxxRawError> for TypedError {
    fn from(raw: CxxRawError) -> Self {
        TypedError {
            which: if raw.kind >= CXX_EXC_TYPED_BASE {
                match raw.kind - CXX_EXC_TYPED_BASE {
                    0 => TypedKind::Domain,
                    1 => TypedKind::Range,
                    _ => TypedKind::Unknown,
                }
            } else {
                TypedKind::Unknown
            },
        }
    }
}

// Runtime helper stubs. The typed helper packs the selector
// (1-based clause index) into kind = CXX_EXC_TYPED_BASE +
// (selector - 1). For selector 0 (catch-all), kind =
// CXX_EXC_UNKNOWN.
unsafe extern "C" {
    fn __cxa_begin_catch(exc: *mut c_void) -> *mut c_void;
    fn __cxa_end_catch();
}

const CXX_EXC_UNKNOWN: u32 = 2;

#[no_mangle]
pub unsafe extern "C" fn __rustcc_cxx_catch_typed(
    exc_ptr: *mut c_void,
    selector: u32,
) -> CxxRawError {
    unsafe {
        let _obj = __cxa_begin_catch(exc_ptr);
        __cxa_end_catch();
    }
    static MSG: &[u8] = b"typed-catch\0";
    let kind = if selector == 0 {
        // Hmm, selector 0 shouldn't be reachable when the
        // landingpad has explicit clauses + catch-all — but be
        // defensive.
        CXX_EXC_UNKNOWN
    } else {
        // 1-based clause index. The last clause is the
        // catch-all; we map it to CXX_EXC_UNKNOWN by checking
        // whether selector lands in the "typed" range. Since
        // the typeinfo list has 2 entries (DomainError,
        // RangeError), selectors 1 and 2 are typed; 3 (the
        // catch-all) maps to UNKNOWN.
        match selector {
            1 => CXX_EXC_TYPED_BASE + 0, // DomainError
            2 => CXX_EXC_TYPED_BASE + 1, // RangeError
            _ => CXX_EXC_UNKNOWN,
        }
    };
    CxxRawError {
        kind,
        message: MSG.as_ptr() as *const c_char,
    }
}

// Stub for the untyped helper too (the call may still be
// emitted for non-typed call sites — but here we only call
// the typed one).
#[no_mangle]
pub unsafe extern "C" fn __rustcc_cxx_catch_unknown(
    _exc_ptr: *mut c_void,
) -> CxxRawError {
    static MSG: &[u8] = b"unknown\0";
    CxxRawError {
        kind: CXX_EXC_UNKNOWN,
        message: MSG.as_ptr() as *const c_char,
    }
}

unsafe extern "C" {
    #[rustc_cxx_throws]
    #[rustc_cxx_throws_typeinfos = "_ZTI11DomainError,_ZTI10RangeError"]
    fn maybe_throws_typed(x: i32) -> Result<i32, TypedError>;
}

fn main() {
    let ok = unsafe { maybe_throws_typed(5) };
    match ok {
        Ok(v) => println!("ok: {}", v),
        Err(e) => println!("err: {:?}", e.which),
    }
    let domain = unsafe { maybe_throws_typed(-1) };
    match domain {
        Ok(_) => println!("FAIL: -1 should throw"),
        Err(e) => println!("err: {:?}", e.which),
    }
    let range = unsafe { maybe_throws_typed(-2) };
    match range {
        Ok(_) => println!("FAIL: -2 should throw"),
        Err(e) => println!("err: {:?}", e.which),
    }
}
