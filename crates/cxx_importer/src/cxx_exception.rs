//! v1.12 stretch 4: `CxxException` runtime type.
//!
//! Catches C++ exceptions thrown across the FFI boundary and
//! surfaces them as a Rust `Err(CxxException)`. See
//! [`fork/CXX-THROW-PLAN.md`](../../fork/CXX-THROW-PLAN.md) for
//! the full design.
//!
//! ## Phasing
//!
//! - **Phase 0** (this module, shipped v1.12): C++-side catch
//!   wrapper. The cxx_importer recognizes `[[rustcc::cxx_throws]]`
//!   on a function declaration and emits a C++ shim that catches
//!   `std::exception` (and `...` for non-std exceptions),
//!   converts the caught exception to a `CxxRawError` struct via
//!   `what()` text, and returns a tagged union to Rust. The Rust
//!   wrapper decodes into `Result<T, CxxException>`.
//!
//! - **Phase 1** (v1.13, requires fork rustc patches): Native
//!   `call → invoke` + landingpad in the codegen layer. No C++
//!   shim wrapper needed; the Rust side catches the raw
//!   exception via `__cxa_begin_catch` directly. Faster (no
//!   string copy of `what()`) and works across DSO boundaries
//!   where C++-side catch wrappers don't.
//!
//! - **Phase 2** (v1.13, MSVC): `catchpad`/`catchswitch` funclet
//!   EH on Windows MSVC targets. Same Rust API surface;
//!   different LLVM IR shape inside the fork rustc.
//!
//! - **Phase 3** (v1.14+): Type-specific catches
//!   (`[[rustcc::cxx_throws(MyType)]]`) + multi-catch dispatch.

use std::borrow::Cow;
use std::fmt;

/// A C++ exception caught at the FFI boundary.
///
/// Constructed by the Phase-0 C++-side shim's caught-exception
/// handler. Carries the `what()` text plus a kind tag so the
/// Rust caller can branch on broad categories (`std::exception`
/// vs `...`) without depending on the C++ side's RTTI surface.
#[derive(Debug, Clone)]
pub struct CxxException {
    /// Coarse classification. See [`CxxExceptionKind`].
    pub kind: CxxExceptionKind,
    /// The result of calling `what()` on the caught exception
    /// for `Std` kind, or a synthetic message for `Unknown`.
    /// May be empty if the C++ side returned a null pointer
    /// (rare — defensive).
    pub message: Cow<'static, str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CxxExceptionKind {
    /// A subclass of `std::exception`. `message` is the result of
    /// `what()`.
    Std,
    /// A non-`std::exception` C++ exception caught via `catch (...)`.
    /// `message` is the synthetic string `"non-std::exception
    /// C++ exception"`.
    Unknown,
}

impl CxxException {
    /// Construct from raw parts. Used by generated wrappers when
    /// decoding the FFI tagged-union return.
    ///
    /// # Safety
    ///
    /// The caller must ensure `message_ptr` is either null or a
    /// valid null-terminated C string with static lifetime
    /// (typically pointing into the C++ side's catch-handler
    /// scratch buffer — see the `CxxRawError` layout below).
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
        let kind = match kind_tag {
            CXX_EXC_STD => CxxExceptionKind::Std,
            _ => CxxExceptionKind::Unknown,
        };
        CxxException { kind, message }
    }

    /// Construct synthetically — for tests + the fallback path
    /// when no exception was caught.
    pub fn synthetic(kind: CxxExceptionKind, message: impl Into<Cow<'static, str>>) -> Self {
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
/// generated Rust wrapper around each `[[rustcc::cxx_throws]]`
/// function.
///
/// # Safety
///
/// `raw.message` must be a null-terminated C string with at
/// least the lifetime of this function call. The C++ shim is
/// responsible for keeping the underlying buffer alive — the
/// generated code copies the message via `from_raw` before
/// the C++ scratch storage is reused.
pub unsafe fn decode<T>(raw: CxxRawError, ok: T) -> Result<T, CxxException> {
    if raw.kind == CXX_EXC_OK {
        Ok(ok)
    } else {
        // SAFETY: caller's contract.
        Err(unsafe { CxxException::from_raw(raw.kind, raw.message) })
    }
}

/// Emit the C++ source for a single throw-aware shim wrapper.
///
/// Given the original function's signature and its
/// already-emitted shim name, produces a `extern "C"`
/// wrapper of shape:
///
/// ```cpp
/// extern "C" CxxRawError <wrapper_name>(<args>, T* out) noexcept {
///     try {
///         *out = <original_call>;
///         return { 0, nullptr };
///     } catch (const std::exception& e) {
///         thread_local std::string buf = e.what();
///         return { 1, buf.c_str() };
///     } catch (...) {
///         return { 2, "non-std::exception C++ exception" };
///     }
/// }
/// ```
///
/// (For void-returning functions the `T* out` parameter is
/// omitted and the body is just the call + tagged return.)
///
/// The thread-local `buf` keeps the `what()` text alive across
/// the FFI return — the Rust decoder copies it before the next
/// call into this wrapper. Catch-(...) returns a static string
/// (no buffer needed).
pub fn render_throws_shim_cpp(
    wrapper_name: &str,
    return_type_cpp: &str,
    param_decls: &[String],
    forward_args: &[String],
    original_callsite: &str,
) -> String {
    let mut src = String::new();
    let returns_void = return_type_cpp == "void" || return_type_cpp.is_empty();

    src.push_str(&format!("extern \"C\" CxxRawError {wrapper_name}(\n"));
    for p in param_decls {
        src.push_str(&format!("    {p},\n"));
    }
    if !returns_void {
        src.push_str(&format!("    {return_type_cpp}* __out\n"));
    }
    // Strip trailing `, ` left by no out-param + no params case.
    if src.ends_with(",\n") {
        src.truncate(src.len() - 2);
        src.push('\n');
    }
    src.push_str(") noexcept {\n");
    src.push_str("    try {\n");
    if returns_void {
        src.push_str(&format!("        {original_callsite}({});\n", forward_args.join(", ")));
    } else {
        src.push_str(&format!(
            "        *__out = {original_callsite}({});\n",
            forward_args.join(", ")
        ));
    }
    src.push_str("        return { 0, nullptr };\n");
    src.push_str("    } catch (const std::exception& __e) {\n");
    src.push_str("        thread_local static std::string __buf;\n");
    src.push_str("        __buf = __e.what();\n");
    src.push_str("        return { 1, __buf.c_str() };\n");
    src.push_str("    } catch (...) {\n");
    src.push_str(
        "        return { 2, \"non-std::exception C++ exception\" };\n",
    );
    src.push_str("    }\n");
    src.push_str("}\n");
    src
}

/// The C++ header definition for `CxxRawError`. Embedded in the
/// generated shim source once per translation unit.
pub const CXX_RAW_ERROR_HEADER: &str = r#"// CxxRawError tagged-union for [[rustcc::cxx_throws]] functions.
// Layout mirrors cxx_importer::cxx_exception::CxxRawError.
struct CxxRawError {
    unsigned int kind;
    const char* message;
};
"#;

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
        let raw = CxxRawError { kind: CXX_EXC_OK, message: std::ptr::null() };
        let r: Result<i32, _> = unsafe { decode(raw, 42) };
        assert_eq!(r.unwrap(), 42);
    }

    #[test]
    fn decode_err_returns_exception() {
        let msg = std::ffi::CString::new("xs").unwrap();
        let raw = CxxRawError { kind: CXX_EXC_STD, message: msg.as_ptr() };
        let r: Result<(), _> = unsafe { decode(raw, ()) };
        let err = r.unwrap_err();
        assert_eq!(err.kind, CxxExceptionKind::Std);
        assert_eq!(err.what(), "xs");
    }

    #[test]
    fn render_void_returning_shim() {
        let src = render_throws_shim_cpp(
            "__rustcc_throws_foo",
            "void",
            &["int x".to_string()],
            &["x".to_string()],
            "foo",
        );
        assert!(src.contains("extern \"C\" CxxRawError __rustcc_throws_foo"));
        assert!(src.contains("try {"));
        assert!(src.contains("foo(x);"));
        assert!(src.contains("return { 0, nullptr };"));
        assert!(src.contains("catch (const std::exception& __e)"));
        assert!(src.contains("__buf = __e.what();"));
        assert!(src.contains("catch (...)"));
    }

    #[test]
    fn render_int_returning_shim_uses_out_param() {
        let src = render_throws_shim_cpp(
            "__rustcc_throws_bar",
            "int",
            &["int x".to_string()],
            &["x".to_string()],
            "bar",
        );
        // Out parameter is appended after the regular args.
        assert!(src.contains("int* __out"));
        assert!(src.contains("*__out = bar(x);"));
    }
}
