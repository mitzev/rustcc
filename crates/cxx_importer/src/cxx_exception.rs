//! v1.12 stretch 4: throw-lowering codegen helpers.
//!
//! Runtime types (`CxxException`, `CxxRawError`, `decode`) live in
//! the `cxx` runtime crate — see `cxx::exception`. This module is
//! the **codegen** side of the same story: the C++ source emitter
//! that wraps a throwing C++ function in a try/catch and a
//! tagged-union return, plus the C++ header text that defines
//! `CxxRawError` on the C++ side (matching the Rust `#[repr(C)]`
//! mirror in `cxx::CxxRawError`).
//!
//! See [`fork/CXX-THROW-PLAN.md`](../../fork/CXX-THROW-PLAN.md)
//! for the full design.
//!
//! ## Phasing
//!
//! - **Phase 0** (shipped v1.12): C++-side catch wrapper emitted
//!   by [`render_throws_shim_cpp`]. The cxx_importer recognizes
//!   throwing free functions (via `RustBindingsConfig::cxx_throws_functions`
//!   in v1.12.1; via `[[rustcc::cxx_throws]]` annotation in
//!   v1.12.2) and emits a Rust wrapper that returns
//!   `Result<T, ::cxx::CxxException>` instead of `T`.
//! - **Phase 1** (v1.13, fork rustc patches): native `call → invoke`
//!   + Itanium `__cxa_begin_catch` landingpad. No C++ shim
//!   wrapper; faster (no `what()` copy) and cross-DSO-correct.
//! - **Phase 2** (v1.13, MSVC): `catchpad`/`catchswitch` funclet
//!   EH on Windows MSVC targets.
//! - **Phase 3** (v1.14+): type-specific catches.

// Re-export the runtime side from the `cxx` crate so callers can
// stick to a single import path (`cxx_importer::CxxException`)
// even though the runtime crate is where the type physically
// lives.
pub use cxx::{
    decode_cxx_raw_error, CxxException, CxxExceptionKind, CxxRawError,
    CXX_EXC_OK, CXX_EXC_STD, CXX_EXC_TYPED_BASE, CXX_EXC_UNKNOWN,
};

/// Emit the C++ source for a single throw-aware shim wrapper.
///
/// Given the original function's signature and its already-emitted
/// shim name, produces an `extern "C"` wrapper of shape:
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
/// For void-returning functions the `T* out` parameter is omitted
/// and the body is just the call + tagged return.
///
/// The thread-local `buf` keeps the `what()` text alive across
/// the FFI return — the Rust decoder copies it before the next
/// call into this wrapper. `catch (...)` returns a static string
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
        src.push_str(&format!(
            "        {original_callsite}({});\n",
            forward_args.join(", ")
        ));
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

/// v1.12.7 (Phase 3): emit a typed-catch shim wrapper.
///
/// Identical to [`render_throws_shim_cpp`] except that the
/// emitted `try` block is followed by one `catch (const T&)`
/// arm per entry in `typed_catches`, ahead of the standard
/// `catch (const std::exception&)` / `catch (...)` arms. Each
/// typed arm sets the kind tag to `CXX_EXC_TYPED_BASE + index`,
/// so the Rust-side `CxxException::from_raw` decoder produces
/// `CxxExceptionKind::Typed(index)`.
///
/// Typed catches are restricted to types deriving from
/// `std::exception` (or any type with a `const char* what() const noexcept`
/// member) — the emitted arm dereferences the caught reference
/// to call `what()`. Non-`std::exception` types fall through to
/// `catch (...)` and surface as `Unknown`.
///
/// ```cpp
/// extern "C" CxxRawError <wrapper_name>(<args>, T* out) noexcept {
///     try {
///         *out = <call>;
///         return { 0, nullptr };
///     } catch (const TypeA& __e) {  // typed_catches[0]
///         thread_local static std::string __buf;
///         __buf = __e.what();
///         return { 16, __buf.c_str() };  // CXX_EXC_TYPED_BASE + 0
///     } catch (const TypeB& __e) {  // typed_catches[1]
///         thread_local static std::string __buf;
///         __buf = __e.what();
///         return { 17, __buf.c_str() };
///     } catch (const std::exception& __e) {
///         thread_local static std::string __buf;
///         __buf = __e.what();
///         return { 1, __buf.c_str() };
///     } catch (...) {
///         return { 2, "non-std::exception C++ exception" };
///     }
/// }
/// ```
///
/// Pass an empty `typed_catches` slice to fall back to the
/// behavior of [`render_throws_shim_cpp`] — useful when wiring
/// the typed renderer behind a single emitter entry point.
pub fn render_throws_shim_cpp_typed(
    wrapper_name: &str,
    return_type_cpp: &str,
    param_decls: &[String],
    forward_args: &[String],
    original_callsite: &str,
    typed_catches: &[String],
) -> String {
    use crate::cxx_exception::CXX_EXC_TYPED_BASE;

    let mut src = String::new();
    let returns_void = return_type_cpp == "void" || return_type_cpp.is_empty();

    src.push_str(&format!("extern \"C\" CxxRawError {wrapper_name}(\n"));
    for p in param_decls {
        src.push_str(&format!("    {p},\n"));
    }
    if !returns_void {
        src.push_str(&format!("    {return_type_cpp}* __out\n"));
    }
    if src.ends_with(",\n") {
        src.truncate(src.len() - 2);
        src.push('\n');
    }
    src.push_str(") noexcept {\n");
    src.push_str("    try {\n");
    if returns_void {
        src.push_str(&format!(
            "        {original_callsite}({});\n",
            forward_args.join(", ")
        ));
    } else {
        src.push_str(&format!(
            "        *__out = {original_callsite}({});\n",
            forward_args.join(", ")
        ));
    }
    src.push_str("        return { 0, nullptr };\n");

    // Typed catch arms — emit in source order so the index in
    // `Typed(N)` matches the user-supplied list position. C++
    // semantics: the first matching `catch` wins, so MORE-derived
    // types must be listed before LESS-derived types. We don't
    // enforce that here — it's the user's call.
    for (i, ty) in typed_catches.iter().enumerate() {
        let tag = CXX_EXC_TYPED_BASE + i as u32;
        src.push_str(&format!("    }} catch (const {ty}& __e) {{\n"));
        src.push_str("        thread_local static std::string __buf;\n");
        src.push_str("        __buf = __e.what();\n");
        src.push_str(&format!("        return {{ {tag}, __buf.c_str() }};\n"));
    }

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
/// generated shim source once per translation unit. Mirrors the
/// `#[repr(C)]` layout of `cxx::CxxRawError`.
pub const CXX_RAW_ERROR_HEADER: &str = r#"// CxxRawError tagged-union for throwing C++ functions.
// Layout mirrors cxx::CxxRawError.
struct CxxRawError {
    unsigned int kind;
    const char* message;
};
"#;

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn re_exported_runtime_types_compile() {
        // Smoke: types from the `cxx` crate are reachable through
        // the importer's `cxx_exception` re-export.
        let e = CxxException::synthetic(CxxExceptionKind::Std, "boom");
        assert_eq!(e.what(), "boom");
        assert_eq!(CXX_EXC_OK, 0);
    }

    #[test]
    fn typed_renderer_emits_typed_catch_arms() {
        let src = render_throws_shim_cpp_typed(
            "__rustcc_throws_div",
            "int",
            &["int __a".into(), "int __b".into()],
            &["__a".into(), "__b".into()],
            "div",
            &["MyError".into(), "std::runtime_error".into()],
        );
        // Each typed arm gets its own catch clause + an
        // incrementing tag starting at CXX_EXC_TYPED_BASE.
        assert!(
            src.contains("catch (const MyError& __e)"),
            "missing MyError arm; src:\n{src}"
        );
        assert!(
            src.contains("catch (const std::runtime_error& __e)"),
            "missing std::runtime_error arm; src:\n{src}"
        );
        // CXX_EXC_TYPED_BASE = 16, so MyError -> 16, std::runtime_error -> 17.
        assert!(
            src.contains("return { 16, __buf.c_str() };"),
            "missing typed tag 16; src:\n{src}"
        );
        assert!(
            src.contains("return { 17, __buf.c_str() };"),
            "missing typed tag 17; src:\n{src}"
        );
        // The fallback `std::exception` + `(...)` arms remain
        // after the typed arms, so unmatched-but-std exceptions
        // still surface as Std kind (1) and the catch-all fires
        // for anything else.
        let std_arm_idx = src
            .find("catch (const std::exception&")
            .expect("std::exception fallback");
        let last_typed_idx = src
            .rfind("return { 17, ")
            .expect("typed tag 17");
        assert!(
            last_typed_idx < std_arm_idx,
            "typed arms must come before std::exception fallback; src:\n{src}"
        );
        assert!(
            src.contains("catch (...)"),
            "missing catch-all; src:\n{src}"
        );
    }

    #[test]
    fn typed_renderer_with_empty_list_matches_plain_renderer() {
        let plain = render_throws_shim_cpp(
            "__rustcc_throws_foo",
            "int",
            &["int x".into()],
            &["x".into()],
            "foo",
        );
        let typed = render_throws_shim_cpp_typed(
            "__rustcc_throws_foo",
            "int",
            &["int x".into()],
            &["x".into()],
            "foo",
            &[],
        );
        assert_eq!(
            plain, typed,
            "empty typed list should produce identical output to plain renderer"
        );
    }

    #[test]
    fn from_raw_maps_typed_tag_back_to_typed_variant() {
        let msg = std::ffi::CString::new("typed boom").unwrap();
        // CXX_EXC_TYPED_BASE + 2 = 18 → Typed(2)
        let exc = unsafe {
            CxxException::from_raw(CXX_EXC_TYPED_BASE + 2, msg.as_ptr())
        };
        assert_eq!(exc.kind, CxxExceptionKind::Typed(2));
        assert_eq!(exc.what(), "typed boom");
    }
}
