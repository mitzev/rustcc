//! v1.12.5: Phase 1 native invoke runtime helpers (Itanium).
//!
//! These helpers are the **Rust runtime side** of the Phase 1
//! throw-lowering story. Phase 0 (shipped v1.12.0–v1.12.4) wraps
//! every throwing C++ function in a C++-side `try`/`catch` shim
//! that exports `CxxRawError` across the FFI boundary. Phase 1
//! eliminates the C++ shim wrapper: fork rustc lowers a
//! `#[rustc_cxx_throws]`-annotated `extern "C++"` call to LLVM's
//! `invoke` instruction with an Itanium catch landingpad. The
//! landingpad pulls the raw exception pointer from the personality
//! function's output (`{ ptr, i32 }`), then calls the helpers
//! below to turn that into a `CxxException` value that flows back
//! to the caller as `Err(_)`.
//!
//! ## ABI contract (Itanium)
//!
//! ```text
//!   ; emitted by fork rustc when an extern "C++" call is
//!   ; marked #[rustc_cxx_throws]:
//!   %ret = invoke <call-conv> <ty> @callee(<args>)
//!       to label %ok_path unwind label %catch_pad
//!   ok_path:
//!     ; ... wrap %ret into Result::Ok(ret) ...
//!     br label %done
//!   catch_pad:
//!     %lpad = landingpad { ptr, i32 } catch ptr null
//!     %exc_ptr = extractvalue { ptr, i32 } %lpad, 0
//!     %raw    = call { i32, ptr } @__rustcc_cxx_catch_unknown(ptr %exc_ptr)
//!     ; %raw is CxxRawError — kind: u32, message: *const c_char.
//!     ; The catch block then calls CxxException::from_raw(raw.kind,
//!     ; raw.message) and wraps into Result::Err(_).
//!     br label %done
//! ```
//!
//! Returning [`crate::CxxRawError`] (the Phase 0 FFI struct) keeps
//! the helper's signature ABI-stable across DSO boundaries. The
//! richer `CxxException` Rust type — which contains a `Cow<str>` —
//! is constructed by the catch-block's `from_raw` call after the
//! ABI hop, not across it.
//!
//! The `catch ptr null` clause is Itanium's catch-all (matches any
//! C++ exception type). Phase 3 (#34) extends this to typed
//! catches via per-type RTTI pointers.
//!
//! ## What's intentionally absent in v1.12.5 + v1.12.6
//!
//! - **`what()` extraction.** The catch-all path doesn't know the
//!   dynamic type of the thrown exception, so it can't safely call
//!   `what()`. Phase 3 (v1.12.8) wires up `dynamic_cast<std::exception*>`
//!   via a separate (`-lstdc++` or `-llibcxx_msvc`-linking) helper TU.
//!
//! ## Per-target shape (v1.12.6 cross-platform)
//!
//! - **Itanium** (`linux`, `macos`, non-msvc `windows`): the
//!   helper calls `__cxa_begin_catch` / `__cxa_end_catch` to take
//!   ownership of the exception inside the landingpad before
//!   returning the `CxxRawError`. libc++abi / libsupc++ are
//!   already in the user's link line because the personality fn
//!   needs them.
//!
//! - **MSVC C++ EH** (`target_env = "msvc"`): no `__cxa_*` calls.
//!   The funclet's `catchret` instruction automatically releases
//!   the exception object when the funclet returns, so the
//!   helper just builds the `CxxRawError` and lets the funclet
//!   machinery handle cleanup. The same C symbol name is
//!   exported on both targets so fork rustc codegen emits the
//!   same `call` instruction regardless of target — only the
//!   surrounding catchpad-vs-landingpad IR differs.
//!
//! ## Coexistence with Phase 0
//!
//! Phase 0 shims continue to work. The bindings emitter picks
//! Phase 1 emission only when the user opts in via
//! `RustBindingsConfig::native_invoke_throws = true` (v1.12.6
//! plumbing). Without that flag, the Phase 0 shim path stays the
//! default — no behavior change for existing users.

use crate::exception::{CxxException, CxxRawError, CXX_EXC_UNKNOWN};
use std::os::raw::c_void;

// Itanium-only externs. MSVC's funclet machinery handles
// exception lifetime through `catchret`, so we don't call the
// Itanium `__cxa_*` runtime there.
#[cfg(not(all(windows, target_env = "msvc")))]
unsafe extern "C" {
    /// Itanium C++ ABI personality-output handler. Takes the
    /// "exception object pointer" the personality function
    /// produced (LLVM `landingpad` slot 0), returns a pointer to
    /// the actual thrown C++ object. The caller is required to
    /// pair every successful call with `__cxa_end_catch`.
    ///
    /// Declared here rather than via `link(name = "...")` because
    /// the symbol lives in libc++abi / libsupc++ which the user's
    /// final link step is already pulling in (it's how the
    /// personality fn itself reaches the runtime).
    fn __cxa_begin_catch(exc_ptr: *mut c_void) -> *mut c_void;

    /// Pairs with `__cxa_begin_catch`. Releases the exception's
    /// refcount and re-enables further unwinding. Must be called
    /// exactly once per `__cxa_begin_catch`.
    fn __cxa_end_catch();
}

/// The fixed `what()`-substitute string for Phase 1's catch-all
/// path. Static lifetime so the returned `*const c_char` stays
/// valid for the lifetime of the program. Null-terminated.
static UNKNOWN_MESSAGE_CSTR: &[u8] = b"non-std::exception C++ exception\0";

/// Convert a raw exception pointer into a [`CxxRawError`]
/// carrying the `Unknown` kind tag and a fixed
/// `"non-std::exception C++ exception"` message.
///
/// **This is the v1.12.5 + v1.12.6 minimum** — the catch-all
/// path that loses the dynamic exception type. v1.12.8 (Phase 3)
/// adds a parallel `catch_std_exception` helper that does
/// `dynamic_cast<std::exception*>` and extracts `what()`.
///
/// # Per-target behavior
///
/// - **Itanium**: calls `__cxa_begin_catch` / `__cxa_end_catch`
///   to take + release ownership of the exception.
/// - **MSVC**: no `__cxa_*` calls — the funclet's `catchret`
///   instruction handles release. The helper just builds the
///   `CxxRawError`.
///
/// # Safety
///
/// `exc_ptr` must be the value of the personality function's
/// exception-object output:
///
/// - Itanium: `extractvalue { ptr, i32 } %landingpad_result, 0`
/// - MSVC: result of `llvm.eh.exceptionpointer.i8` on the
///   surrounding `catchpad` token
///
/// Passing any other pointer — null, already-caught, from a
/// different throw — is undefined behavior; the runtime will
/// likely abort via `std::terminate`.
///
/// The fork rustc codegen layer emits a `call` to this function
/// inside the catch landingpad / funclet, so user code never
/// invokes it directly.
#[no_mangle]
pub unsafe extern "C" fn __rustcc_cxx_catch_unknown(
    exc_ptr: *mut c_void,
) -> CxxRawError {
    // SAFETY: caller's contract — `exc_ptr` is the personality
    // output. On Itanium we round-trip through
    // `__cxa_begin_catch` / `__cxa_end_catch` to acquire +
    // release the exception. On MSVC the funclet handles
    // lifetime; we just consume `exc_ptr` (it stays live for
    // the duration of the funclet, which is our caller).
    //
    // The static `UNKNOWN_MESSAGE_CSTR` outlives the program, so
    // the returned `*const c_char` is always valid.
    #[cfg(not(all(windows, target_env = "msvc")))]
    {
        let _obj = unsafe { __cxa_begin_catch(exc_ptr) };
        unsafe { __cxa_end_catch() };
    }
    #[cfg(all(windows, target_env = "msvc"))]
    {
        // Consume to suppress unused-variable lint without
        // dropping the safety obligation on the caller side.
        let _ = exc_ptr;
    }
    CxxRawError {
        kind: CXX_EXC_UNKNOWN,
        message: UNKNOWN_MESSAGE_CSTR.as_ptr() as *const std::os::raw::c_char,
    }
}

/// P09.x (v1.15) / P09.68-gcc: typed-catch SELECTOR computation for
/// backends without LLVM's `llvm.eh.typeid.for` (the GCC backend).
///
/// The LLVM path lets the personality function match landingpad
/// clauses and translates the raw type-id into a small 1-based index.
/// gccjit's try/catch region is catch-all only, so the fork's GCC
/// backend calls THIS helper from the landing pad instead: walk the
/// `typeinfos` array (the `#[rustc_cxx_throws_typeinfos]` list, in
/// clause order) and return `i + 1` for the first entry matching the
/// in-flight exception's `std::type_info`, or `0` when nothing
/// matches (→ the catch-all path). The result feeds the same
/// `__rustcc_cxx_catch_typed(exn, selector)` contract as LLVM.
///
/// # Itanium layout contract
///
/// `exc_ptr` is the `_Unwind_Exception*` the personality produced.
/// Per the Itanium C++ ABI, it is embedded at the END of
/// `__cxa_exception`, whose `exceptionType: *const std::type_info`
/// field sits at a fixed negative offset on LP64: the fields between
/// it and `unwindHeader` are 4 pointers (dtor, unexpectedHandler,
/// terminateHandler, nextException), 2 ints (handlerCount,
/// handlerSwitchValue) and 4 pointers (actionRecord,
/// languageSpecificData, catchTemp, adjustedPtr) = 72 bytes, plus
/// the field itself → −80. Both libsupc++ and libc++abi share this
/// layout (libc++abi mirrors it deliberately for cross-runtime
/// compat). Type equality follows the runtimes' own rule: a name
/// starting with `'*'` compares by pointer identity, anything else
/// by `strcmp` (cross-DSO safe).
///
/// Non-Itanium or non-64-bit targets return 0 (typed catches degrade
/// to the catch-all `Unknown` path — never UB).
///
/// # Safety
///
/// `exc_ptr` must be a live personality-produced exception pointer;
/// `typeinfos` must point at `n` valid `_ZTI…` addresses.
#[no_mangle]
pub unsafe extern "C" fn __rustcc_cxx_match_typeinfo(
    exc_ptr: *mut c_void,
    typeinfos: *const *const c_void,
    n: u32,
) -> u32 {
    #[cfg(all(not(all(windows, target_env = "msvc")), target_pointer_width = "64"))]
    unsafe {
        if exc_ptr.is_null() || typeinfos.is_null() {
            return 0;
        }
        // exceptionType at unwindHeader − 80 (see layout contract).
        let thrown_ti =
            *((exc_ptr as *const u8).offset(-80) as *const *const c_void);
        if thrown_ti.is_null() {
            return 0;
        }
        // Itanium std::type_info: { vptr, const char* __name }.
        let name_of = |ti: *const c_void| -> *const u8 {
            *((ti as *const u8).add(8) as *const *const u8)
        };
        let thrown_name = name_of(thrown_ti);
        for i in 0..n {
            let want = *typeinfos.add(i as usize);
            if want.is_null() {
                continue;
            }
            if want == thrown_ti {
                return i + 1;
            }
            let want_name = name_of(want);
            if thrown_name.is_null() || want_name.is_null() {
                continue;
            }
            // '*'-prefixed names are pointer-unique by contract.
            if *thrown_name == b'*' || *want_name == b'*' {
                continue;
            }
            let mut a = thrown_name;
            let mut b = want_name;
            loop {
                let (ca, cb) = (*a, *b);
                if ca != cb {
                    break;
                }
                if ca == 0 {
                    return i + 1;
                }
                a = a.add(1);
                b = b.add(1);
            }
        }
        0
    }
    #[cfg(not(all(not(all(windows, target_env = "msvc")), target_pointer_width = "64")))]
    {
        let _ = (exc_ptr, typeinfos, n);
        0
    }
}

/// Convenience wrapper: same as [`__rustcc_cxx_catch_unknown`]
/// but returns a fully-constructed `Result::Err(CxxException)`.
/// Useful for hand-written FFI integrations that don't need the
/// raw-tagged-union shape — fork rustc codegen uses the raw
/// helper above so the catch landingpad's IR stays simple.
///
/// # Safety
///
/// Same as [`__rustcc_cxx_catch_unknown`].
#[inline]
pub unsafe fn catch_to_result_err<T>(
    exc_ptr: *mut c_void,
) -> Result<T, CxxException> {
    // SAFETY: caller's contract is identical to the helper.
    let raw = unsafe { __rustcc_cxx_catch_unknown(exc_ptr) };
    Err(unsafe { CxxException::from_raw(raw.kind, raw.message) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exception::CxxExceptionKind;

    // We can't drive `__cxa_begin_catch` synthetically — it
    // expects a real Itanium exception in flight. These tests
    // cover the type-level shape + static-string contents only;
    // the runtime round-trip ships as a fork-rustc bootstrap
    // integration test in v1.12.6 (`tests/native_invoke_phase1_runtime.rs`).

    #[test]
    fn unknown_helper_signature_compiles() {
        // Existence + signature check. The `#[no_mangle]`
        // symbol must be linkable from fork rustc's codegen
        // path; if this test compiles, the symbol is in the
        // crate's exported surface.
        let _: unsafe extern "C" fn(*mut std::os::raw::c_void) -> CxxRawError =
            __rustcc_cxx_catch_unknown;
    }

    #[test]
    fn unknown_message_is_null_terminated_and_matches_phase0() {
        // The static message must be null-terminated (FFI
        // contract) and identical to the Phase 0 `catch (...)`
        // arm's synthetic string — so both phases produce
        // indistinguishable `CxxException` values for the same
        // input.
        assert!(UNKNOWN_MESSAGE_CSTR.ends_with(b"\0"));
        let s = std::str::from_utf8(
            &UNKNOWN_MESSAGE_CSTR[..UNKNOWN_MESSAGE_CSTR.len() - 1],
        )
        .unwrap();
        assert_eq!(s, "non-std::exception C++ exception");
    }

    #[test]
    fn catch_to_result_err_constructs_err_variant() {
        // The wrapper is `unsafe` because the underlying helper
        // is — we don't actually invoke it. We construct the
        // Err directly via `from_raw` to verify the type
        // plumbing matches what the helper would produce.
        let msg_ptr = UNKNOWN_MESSAGE_CSTR.as_ptr() as *const std::os::raw::c_char;
        let exc = unsafe { CxxException::from_raw(CXX_EXC_UNKNOWN, msg_ptr) };
        assert_eq!(exc.kind, CxxExceptionKind::Unknown);
        assert_eq!(exc.message, "non-std::exception C++ exception");
    }
}
