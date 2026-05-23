//! v1.12 throw-lowering runtime types.
//!
//! Mirror of the FFI surface emitted by `cxx_importer`'s Phase 0
//! catch shim. Lives in the runtime crate (not `cxx_importer`) so
//! that downstream users only depend on `cxx` at runtime — they
//! pull in `cxx_importer` at build time to *generate* bindings,
//! but their actual binary only links against `cxx`.
//!
//! The C++ side of this contract — the `CxxRawError` struct
//! layout and the catch shim body — is rendered by
//! `cxx_importer::render_throws_shim_cpp`. See
//! `fork/CXX-THROW-PLAN.md` for the full design + phasing.

use std::borrow::Cow;
use std::fmt;

/// A C++ exception caught at the FFI boundary.
#[derive(Debug, Clone)]
pub struct CxxException {
    /// Coarse classification.
    pub kind: CxxExceptionKind,
    /// `what()` text for `Std`; synthetic message for `Unknown`;
    /// empty when the C++ side returned a null pointer (rare —
    /// defensive).
    pub message: Cow<'static, str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CxxExceptionKind {
    /// `std::exception` subclass — `message` is `what()`.
    Std,
    /// Caught via `catch (...)` — `message` is the
    /// synthetic `"non-std::exception C++ exception"`.
    Unknown,
    /// v1.12.7 (Phase 3): a typed catch matched a specific
    /// exception type listed in
    /// `[[clang::annotate("rustcc::cxx_throws(Type1, Type2, …)")]]`.
    /// The `u32` is the zero-based index into the annotation's
    /// type list (`0` = first type, `1` = second, …), so the
    /// Rust-side caller can dispatch into a typed enum variant
    /// once the multi-variant emission lands (tracked as the
    /// follow-on to v1.12.7).
    ///
    /// `message` is still the result of `what()` on the caught
    /// exception — Phase 3 typed catches are constrained to
    /// `std::exception` subclasses so this is always safe.
    Typed(u32),
}

impl CxxException {
    /// Construct from raw parts. Used by generated wrappers when
    /// decoding the FFI tagged-union return.
    ///
    /// # Safety
    ///
    /// `message_ptr` must be either null or a valid
    /// null-terminated C string that stays live for the duration
    /// of this call. The Phase 0 shim keeps the underlying
    /// buffer in a `thread_local std::string`; we copy out of it
    /// before this function returns.
    pub unsafe fn from_raw(
        kind_tag: u32,
        message_ptr: *const std::os::raw::c_char,
    ) -> Self {
        let message: Cow<'static, str> = if message_ptr.is_null() {
            Cow::Borrowed("")
        } else {
            // SAFETY: caller's contract.
            let cstr = unsafe { std::ffi::CStr::from_ptr(message_ptr) };
            Cow::Owned(cstr.to_string_lossy().into_owned())
        };
        // v1.12.7: typed catches use kind tags in the
        // `CXX_EXC_TYPED_BASE`-and-up range. `Typed(N)` where N
        // is `kind_tag - CXX_EXC_TYPED_BASE`. Tags 0, 1, 2 stay
        // reserved for OK / Std / Unknown.
        let kind = match kind_tag {
            CXX_EXC_OK => CxxExceptionKind::Unknown, // OK shouldn't reach here
            CXX_EXC_STD => CxxExceptionKind::Std,
            CXX_EXC_UNKNOWN => CxxExceptionKind::Unknown,
            n if n >= CXX_EXC_TYPED_BASE => {
                CxxExceptionKind::Typed(n - CXX_EXC_TYPED_BASE)
            }
            _ => CxxExceptionKind::Unknown,
        };
        CxxException { kind, message }
    }

    /// Construct synthetically — for tests + the fallback path
    /// when no exception was caught.
    pub fn synthetic(
        kind: CxxExceptionKind,
        message: impl Into<Cow<'static, str>>,
    ) -> Self {
        CxxException {
            kind,
            message: message.into(),
        }
    }

    /// The `what()` text. Always a `&str`; empty when the C++ side
    /// returned no message.
    pub fn what(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CxxException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            CxxExceptionKind::Std => write!(f, "C++ exception: {}", self.message),
            CxxExceptionKind::Unknown => write!(f, "non-std C++ exception"),
            CxxExceptionKind::Typed(idx) => {
                write!(f, "C++ exception (typed slot {idx}): {}", self.message)
            }
        }
    }
}

impl std::error::Error for CxxException {}

/// Tag values shared between the C++ shim and the Rust decoder.
/// `0` is the success tag (no exception was thrown).
pub const CXX_EXC_OK: u32 = 0;
/// `std::exception` subclass.
pub const CXX_EXC_STD: u32 = 1;
/// Caught via `catch (...)` — non-std exception.
pub const CXX_EXC_UNKNOWN: u32 = 2;
/// v1.12.7 (Phase 3): typed catch tags occupy the range
/// `[CXX_EXC_TYPED_BASE, u32::MAX)`. The C++ shim emits
/// `CXX_EXC_TYPED_BASE + index` where `index` is the 0-based
/// position of the matched type in the annotation's
/// `cxx_throws(T1, T2, …)` list.
pub const CXX_EXC_TYPED_BASE: u32 = 16;

/// C++-side tagged-union layout. The generated shim wraps the
/// original function and returns this struct by value. Layout
/// must match the C++ shim's `struct CxxRawError` exactly.
///
/// ```text
/// struct CxxRawError {
///     uint32_t kind;
///     const char* message;
/// };
/// ```
#[repr(C)]
pub struct CxxRawError {
    pub kind: u32,
    pub message: *const std::os::raw::c_char,
}

/// Decode the raw FFI return into a `Result`. Used by the
/// generated Rust wrapper around each throwing function when the
/// return type is `void` (the `T` slot is `()`); non-void
/// returns use a `MaybeUninit` out-param and check `kind`
/// inline.
///
/// # Safety
///
/// `raw.message` must be a null-terminated C string with at
/// least the lifetime of this call. The C++ shim is responsible
/// for keeping the underlying buffer alive — the generated code
/// copies the message via `from_raw` before the C++ scratch
/// storage is reused.
pub unsafe fn decode<T>(raw: CxxRawError, ok: T) -> Result<T, CxxException> {
    if raw.kind == CXX_EXC_OK {
        Ok(ok)
    } else {
        // SAFETY: caller's contract.
        Err(unsafe { CxxException::from_raw(raw.kind, raw.message) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_constructor_round_trips() {
        let e = CxxException::synthetic(CxxExceptionKind::Std, "boom");
        assert_eq!(e.what(), "boom");
        assert_eq!(e.kind, CxxExceptionKind::Std);
        assert_eq!(format!("{e}"), "C++ exception: boom");
    }

    #[test]
    fn unknown_kind_display() {
        let e = CxxException::synthetic(CxxExceptionKind::Unknown, "");
        assert_eq!(format!("{e}"), "non-std C++ exception");
    }

    #[test]
    fn decode_ok_returns_value() {
        let raw = CxxRawError {
            kind: CXX_EXC_OK,
            message: std::ptr::null(),
        };
        let r: Result<i32, _> = unsafe { decode(raw, 42) };
        assert_eq!(r.unwrap(), 42);
    }

    #[test]
    fn decode_err_returns_exception() {
        let msg = std::ffi::CString::new("xs").unwrap();
        let raw = CxxRawError {
            kind: CXX_EXC_STD,
            message: msg.as_ptr(),
        };
        let r: Result<(), _> = unsafe { decode(raw, ()) };
        let err = r.unwrap_err();
        assert_eq!(err.kind, CxxExceptionKind::Std);
        assert_eq!(err.what(), "xs");
    }
}
