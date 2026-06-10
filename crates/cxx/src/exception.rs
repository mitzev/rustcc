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

    /// v1.12.13: true when the caught exception derived from
    /// `std::exception` and was matched by the fallback arm of
    /// the C++ shim (i.e., the caller's annotation was the
    /// plain `cxx_throws` form, or the typed form's listed
    /// types didn't match — `std::exception` won the fallback).
    pub fn is_std(&self) -> bool {
        matches!(self.kind, CxxExceptionKind::Std)
    }

    /// v1.12.13: true when the caught exception was matched by
    /// the C++ shim's `catch (...)` arm — non-`std::exception`
    /// type (bare `int`, custom class with no `std::exception`
    /// base, etc.). Message is the synthetic
    /// `"non-std::exception C++ exception"` string.
    pub fn is_unknown(&self) -> bool {
        matches!(self.kind, CxxExceptionKind::Unknown)
    }

    /// v1.12.13: true when the caught exception matched a typed
    /// `catch (const T&)` arm. Without an `idx` filter, returns
    /// `true` for any typed match. See [`Self::is_typed_at`] for
    /// a position-specific check.
    pub fn is_typed(&self) -> bool {
        matches!(self.kind, CxxExceptionKind::Typed(_))
    }

    /// v1.12.13: true when the caught exception matched the
    /// typed arm at zero-based position `idx` in the
    /// `cxx_throws(T0, T1, …)` annotation list.
    pub fn is_typed_at(&self, idx: u32) -> bool {
        matches!(self.kind, CxxExceptionKind::Typed(n) if n == idx)
    }

    /// v1.12.13: the zero-based index of the typed-catch arm
    /// that matched, or `None` when the kind isn't `Typed(_)`.
    /// Useful for routing into a typed handler:
    ///
    /// ```ignore
    /// match e.typed_index() {
    ///     Some(0) => handle_domain_error(e),
    ///     Some(1) => handle_range_error(e),
    ///     _ => handle_fallback(e),
    /// }
    /// ```
    pub fn typed_index(&self) -> Option<u32> {
        match self.kind {
            CxxExceptionKind::Typed(n) => Some(n),
            _ => None,
        }
    }
}

/// Convert from the FFI-mirror `CxxRawError` to the
/// ergonomic `CxxException`. Used by the rustcc fork rustc's
/// `cxx_throws_wrap` MIR pass (P09.69) to lift the runtime
/// helper's return into the user's declared
/// `Result<T, CxxException>` shape.
///
/// Wraps the unsafe `CxxException::from_raw` with the trust
/// assumption that the `CxxRawError` was produced by
/// `__rustcc_cxx_catch_unknown` — which always returns a
/// valid (possibly-null) message pointer pointing to a
/// statically-allocated `c_char` string. The conversion is
/// safe to use in the From impl because that's the only path
/// that actually produces a `CxxRawError` in real use; any
/// hand-constructed instance is the caller's responsibility
/// to set up correctly.
impl From<CxxRawError> for CxxException {
    fn from(raw: CxxRawError) -> Self {
        // SAFETY: `__rustcc_cxx_catch_unknown` either returns
        // a null pointer or a pointer to a string with
        // 'static lifetime. The Phase 0 shim's path uses a
        // `thread_local std::string` whose buffer lives
        // through the call; from_raw copies out of it
        // immediately, so the lifetime constraint holds.
        unsafe { CxxException::from_raw(raw.kind, raw.message) }
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
#[cfg_attr(feature = "rustcc-fork", rustc_diagnostic_item = "CxxRawError")]
pub struct CxxRawError {
    // v1.13.10: crate-private. With public fields, SAFE code could
    // build `CxxRawError { message: 1 as *const _ }` and feed it to
    // the safe `From<CxxRawError> for CxxException` impl, which runs
    // `CStr::from_ptr` on it — UB from safe code. All out-of-crate
    // construction now goes through the unsafe [`Self::new`]; the
    // layout (repr(C): u32 + pointer) is unchanged, so the C++ shims
    // that return this struct by value across FFI are unaffected.
    pub(crate) kind: u32,
    pub(crate) message: *const std::os::raw::c_char,
}

impl CxxRawError {
    /// Build a raw error manually.
    ///
    /// # Safety
    /// `message` must be null or point to a NUL-terminated string that
    /// stays alive until the value is decoded (the `From` impl /
    /// [`CxxException::from_raw`] read it immediately).
    pub unsafe fn new(kind: u32, message: *const std::os::raw::c_char) -> Self {
        Self { kind, message }
    }

    /// The exception-kind tag (`CXX_EXC_OK` / `CXX_EXC_STD` / …).
    pub fn kind(&self) -> u32 {
        self.kind
    }

    /// The raw message pointer (possibly null).
    pub fn message_ptr(&self) -> *const std::os::raw::c_char {
        self.message
    }
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

    #[test]
    fn is_std_only_true_for_std_kind() {
        let std = CxxException::synthetic(CxxExceptionKind::Std, "x");
        assert!(std.is_std());
        assert!(!std.is_unknown());
        assert!(!std.is_typed());
        assert_eq!(std.typed_index(), None);

        let unk = CxxException::synthetic(CxxExceptionKind::Unknown, "");
        assert!(!unk.is_std());
        assert!(unk.is_unknown());

        let typed = CxxException::synthetic(CxxExceptionKind::Typed(2), "y");
        assert!(!typed.is_std());
        assert!(!typed.is_unknown());
        assert!(typed.is_typed());
        assert_eq!(typed.typed_index(), Some(2));
    }

    #[test]
    fn is_typed_at_position_match() {
        let typed = CxxException::synthetic(CxxExceptionKind::Typed(3), "");
        assert!(typed.is_typed_at(3));
        assert!(!typed.is_typed_at(0));
        assert!(!typed.is_typed_at(2));
        // Non-typed kinds match nothing.
        let std = CxxException::synthetic(CxxExceptionKind::Std, "");
        assert!(!std.is_typed_at(0));
    }

    #[test]
    fn typed_index_returns_position_for_typed_kinds_only() {
        assert_eq!(
            CxxException::synthetic(CxxExceptionKind::Typed(0), "").typed_index(),
            Some(0)
        );
        assert_eq!(
            CxxException::synthetic(CxxExceptionKind::Typed(7), "").typed_index(),
            Some(7)
        );
        assert_eq!(
            CxxException::synthetic(CxxExceptionKind::Std, "").typed_index(),
            None
        );
        assert_eq!(
            CxxException::synthetic(CxxExceptionKind::Unknown, "").typed_index(),
            None
        );
    }
}
