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

/// v1.12.10: one shim's worth of input for the end-to-end
/// renderer. Users build a `Vec<ThrowsShimSpec>` (one per
/// throwing function) and pass it to [`render_all_throws_shims_cpp`],
/// which prefixes [`CXX_RAW_ERROR_HEADER`] + the right `#include`s
/// and emits the appropriate `render_throws_shim_cpp` /
/// `render_throws_shim_cpp_typed` body per entry.
///
/// The fields mirror `render_throws_shim_cpp`'s positional
/// arguments. `typed_catches` empty = use the plain renderer;
/// non-empty = route through the typed renderer.
#[derive(Clone, Debug)]
pub struct ThrowsShimSpec {
    /// Symbol name of the `extern "C"` shim (e.g.
    /// `"__rustcc_throws_do_divide"`). Must match the
    /// `#[link_name = "…"]` the bindings emitter produced on
    /// the Rust side for this function.
    pub wrapper_name: String,
    /// The original function's C++ return type, rendered as
    /// source (e.g., `"int"`, `"void"`, `"std::string"`). For
    /// non-void returns the renderer appends `*__out` as the
    /// out-param.
    pub return_type_cpp: String,
    /// Parameter declarations as they appear in the wrapper
    /// signature, including each `type name` pair
    /// (e.g., `["int __a", "int __b"]`). Order matters.
    pub param_decls: Vec<String>,
    /// Forward arguments to the wrapped call, in declaration
    /// order (e.g., `["__a", "__b"]`). Must match
    /// `param_decls.len()` for a non-variadic function.
    pub forward_args: Vec<String>,
    /// The C++ expression the shim's `try` block calls — a
    /// bare function name for TU-scope free fns, or a
    /// namespaced path for namespaced functions
    /// (e.g., `"ns::sub::do_thing"`).
    pub original_callsite: String,
    /// Phase 3 typed catches in source order. Empty = use the
    /// plain `render_throws_shim_cpp` path; non-empty = use
    /// `render_throws_shim_cpp_typed` with these C++ type names
    /// as the matched-catch clauses.
    pub typed_catches: Vec<String>,
    /// v1.12.17: when `true`, the renderer emits a ctor shim:
    /// the first param is `<Class>* __out` (no extra trailing
    /// out-param), the body is
    /// `new (__out) <Class>(<args>...)` instead of
    /// `*__out = <callsite>(<args>...)`, and the
    /// `original_callsite` field is treated as the
    /// **fully-qualified C++ class name** to placement-construct
    /// (e.g., `"ns::Foo"`).
    ///
    /// Set automatically by build.rs's
    /// `collect_throws_specs_for_ctors`; callers building specs
    /// by hand for ctor cases set this manually.
    pub is_ctor: bool,
}

/// v1.12.10: emit a full C++ shim source file for a batch of
/// throwing functions. Handles:
///
/// - One `#include` per entry in `headers` (typically the
///   header that declares the wrapped functions, plus
///   `<stdexcept>` / `<string>` / `<exception>` as needed for
///   the catch arms).
/// - The `CxxRawError` struct header injected once.
/// - One shim body per `ThrowsShimSpec`, dispatched to the
///   plain or typed renderer based on whether
///   `typed_catches.is_empty()`.
///
/// Output is suitable for piping directly into `clang++ -c …` —
/// it's a self-contained translation unit with no other build
/// steps needed beyond the user's normal C++ link line.
///
/// The output is deterministic: shims appear in the order they
/// were given. Callers that want sorted output (e.g., for
/// reproducible-build hashing) should sort `shims` before
/// calling.
pub fn render_all_throws_shims_cpp(
    headers: &[&str],
    shims: &[ThrowsShimSpec],
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    out.push_str("// Generated by rustcc cxx_importer::cxx_exception.\n");
    out.push_str("// Do not hand-edit — re-run `render_all_throws_shims_cpp`.\n");
    out.push_str("//\n");
    out.push_str(
        "// This file is the C++ counterpart to the Rust bindings the\n\
         // cxx_importer bindings emitter produced for the same set of\n\
         // `[[clang::annotate(\"rustcc::cxx_throws\")]]`-annotated\n\
         // functions. Compile and link alongside the Rust object.\n\n",
    );

    // User-supplied headers first — these declare the wrapped
    // functions. Then the standard C++ headers our catch arms
    // need (stdexcept for std::exception subclasses, string
    // for the thread_local buffer, exception for the bare
    // std::exception type that catch arms reference).
    for h in headers {
        let _ = writeln!(out, "#include \"{h}\"");
    }
    out.push_str("#include <exception>\n");
    out.push_str("#include <stdexcept>\n");
    out.push_str("#include <string>\n");
    out.push('\n');

    out.push_str(CXX_RAW_ERROR_HEADER);
    out.push('\n');

    for spec in shims {
        if spec.is_ctor {
            // v1.12.17: ctor shims use placement-new into the
            // Rust-owned out-slot. `original_callsite` is the
            // class FQN to construct.
            out.push_str(&render_throws_ctor_shim_cpp(
                &spec.wrapper_name,
                &spec.original_callsite,
                &spec.param_decls,
                &spec.forward_args,
                &spec.typed_catches,
            ));
        } else if spec.typed_catches.is_empty() {
            out.push_str(&render_throws_shim_cpp(
                &spec.wrapper_name,
                &spec.return_type_cpp,
                &spec.param_decls,
                &spec.forward_args,
                &spec.original_callsite,
            ));
        } else {
            out.push_str(&render_throws_shim_cpp_typed(
                &spec.wrapper_name,
                &spec.return_type_cpp,
                &spec.param_decls,
                &spec.forward_args,
                &spec.original_callsite,
                &spec.typed_catches,
            ));
        }
        out.push('\n');
    }
    out
}

/// v1.12.17: emit a throwing-ctor shim wrapper. Unlike the
/// non-ctor variants, the first parameter is `<Class>* __out`
/// (the Rust caller's `MaybeUninit::as_mut_ptr()`) and the body
/// placement-constructs into that slot rather than assigning
/// through it:
///
/// ```cpp
/// extern "C" CxxRawError __rustcc_throws_<Class>_new(
///     <Class>* __out,
///     <args>...
/// ) noexcept {
///     try {
///         new (__out) <Class>(<args>...);
///         return { 0, nullptr };
///     } catch (...) { ... }
/// }
/// ```
///
/// `class_fqn` is the C++ name to placement-construct
/// (e.g., `"ns::Foo"`). `param_decls` and `forward_args`
/// describe the ctor's user-facing parameters — the `__out`
/// slot is prepended by this function, not in the spec.
///
/// Pass an empty `typed_catches` slice for catch-all behavior.
fn render_throws_ctor_shim_cpp(
    wrapper_name: &str,
    class_fqn: &str,
    param_decls: &[String],
    forward_args: &[String],
    typed_catches: &[String],
) -> String {
    let mut src = String::new();
    src.push_str(&format!("extern \"C\" CxxRawError {wrapper_name}(\n"));
    src.push_str(&format!("    {class_fqn}* __out,\n"));
    for p in param_decls {
        src.push_str(&format!("    {p},\n"));
    }
    // Drop trailing comma.
    if src.ends_with(",\n") {
        src.truncate(src.len() - 2);
        src.push('\n');
    }
    src.push_str(") noexcept {\n");
    src.push_str("    try {\n");
    src.push_str(&format!(
        "        new (__out) {class_fqn}({});\n",
        forward_args.join(", ")
    ));
    src.push_str("        return { 0, nullptr };\n");

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

/// v1.12.11: collect the typed-catches list for every throwing
/// class method whose annotation is
/// `Annotation::CxxThrowsTyped(_)`. Returns a `BTreeMap` keyed
/// by `"<ClassFQN>::<method_name>"` — the same FQN form
/// `crate::rust_bindings`'s class-method emitter uses for
/// annotation lookups.
///
/// Walks each `ClassId` in `classes`, builds the C++ class FQN
/// from the class's `NestedName`, and then for each method
/// (excluding ctors and special members) looks up
/// `"<ClassFQN>::<method_name>"` in the annotation set. Methods
/// with plain `CxxThrows` (no type list) are NOT included —
/// they're handled by `render_throws_shim_cpp`.
///
/// Note on ctors: ctors are named after the class
/// (`"Class::Class"` in the annotation FQN form). Typed ctor
/// catches are recognized by this helper too — they land in
/// the result map under that doubled-name key.
pub fn collect_class_method_throws_catches(
    ctx: &rustc_abi_cxx::CxxTypeCtx,
    annotations: &crate::annotations::AnnotationSet,
    classes: &[rustc_abi_cxx::ClassId],
) -> std::collections::BTreeMap<String, Vec<String>> {
    use crate::annotations::Annotation;
    use rustc_abi_cxx::{MethodName, SpecialMember};

    let mut out: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();

    for &class_id in classes {
        let class = ctx.class(class_id);
        let class_fqn = nested_name_to_fqn(&class.name.0);

        // Compute the class's last segment for the ctor
        // annotation key — libclang names ctor cursors after
        // the class, so the FQN is `Class::Class` (or
        // `ns::sub::Class::Class` for namespaced classes).
        let class_short = class_fqn.rsplit("::").next().unwrap_or("").to_string();

        for method in &class.methods {
            let method_name_src: Option<String> = match (&method.special, &method.name) {
                (
                    Some(SpecialMember::DefaultCtor | SpecialMember::OtherCtor),
                    _,
                ) => Some(class_short.clone()),
                (None, MethodName::Ident(id)) => Some(id.0.clone()),
                _ => None, // operators / conversion / dtor / move-special
            };
            let Some(src) = method_name_src else {
                continue;
            };
            let fqn = format!("{class_fqn}::{src}");
            for ann in annotations.effective(&fqn) {
                if let Annotation::CxxThrowsTyped(types) = ann {
                    out.insert(fqn.clone(), types);
                    break;
                }
            }
        }
    }
    out
}

/// P09.70 / 1.13 throws Phase 2E: mangle a C++ type name to
/// its Itanium typeinfo symbol (the form
/// `_ZTI<len><name>` for global-namespace classes, with the
/// `St` (std) abbreviation prefix when the type lives in
/// `std::`).
///
/// Returns `None` when the type name uses syntax this helper
/// doesn't understand yet (nested namespaces beyond `std::`,
/// templates, references, qualifiers). Downstream code falls
/// back to omitting the typeinfo attribute in that case;
/// users who want typed catches on those types still have
/// the manual `#[rustc_cxx_throws_typeinfos = "..."]` escape
/// hatch.
///
/// Examples:
/// - `"DomainError"` → `Some("_ZTI11DomainError")`
/// - `"std::runtime_error"` → `Some("_ZTISt13runtime_error")`
/// - `"my_ns::Error"` → `None` (un-supported nesting)
pub fn itanium_typeinfo_symbol_for(type_name: &str) -> Option<String> {
    let trimmed = type_name.trim();
    if let Some(rest) = trimmed.strip_prefix("std::") {
        // No further `::` allowed in this MVP — defer nested
        // std types to a future patch.
        if rest.contains("::") || rest.contains('<') || rest.contains('&') {
            return None;
        }
        return Some(format!("_ZTISt{}{}", rest.len(), rest));
    }
    if trimmed.contains("::")
        || trimmed.contains('<')
        || trimmed.contains('&')
        || trimmed.is_empty()
    {
        return None;
    }
    Some(format!("_ZTI{}{}", trimmed.len(), trimmed))
}

/// P09.70 / 1.13 throws Phase 2E: mangle a C++ type name to
/// its MSVC TypeDescriptor name (the form `.?AV<name>@@` for
/// global-namespace classes, with namespace components
/// reversed and double-`@` terminator for nested types).
///
/// Returns `None` for unsupported shapes (same as
/// `itanium_typeinfo_symbol_for`).
///
/// Examples:
/// - `"DomainError"` → `Some(".?AVDomainError@@")`
/// - `"std::runtime_error"` → `Some(".?AVruntime_error@std@@")`
pub fn msvc_typedesc_name_for(type_name: &str) -> Option<String> {
    let trimmed = type_name.trim();
    if trimmed.is_empty() || trimmed.contains('<') || trimmed.contains('&') {
        return None;
    }
    // Reverse namespace components and join with `@`. The
    // final `@@` closes the qualified name.
    let parts: Vec<&str> = trimmed.split("::").collect();
    if parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let mut out = String::from(".?AV");
    // MSVC encodes from innermost type out: the LAST `::`
    // segment goes first, then walk back. We use `@` between
    // segments and `@@` to terminate.
    let mut rev: Vec<&str> = parts.into_iter().collect();
    rev.reverse();
    for (i, p) in rev.iter().enumerate() {
        out.push_str(p);
        if i + 1 < rev.len() {
            out.push('@');
        }
    }
    out.push_str("@@");
    Some(out)
}

/// P09.70: given a list of caught C++ type names, produce
/// the comma-separated strings for the
/// `#[rustc_cxx_throws_typeinfos]` and
/// `#[rustc_cxx_throws_msvc_typedescs]` attributes. Returns
/// `None` if any type in the list can't be mangled by the
/// helpers above (so we don't ship a partial list — better
/// to skip the attributes entirely than to mismatch
/// indices).
pub fn manglings_for_typed_catches(
    type_names: &[String],
) -> Option<(String, String)> {
    let mut itanium = Vec::with_capacity(type_names.len());
    let mut msvc = Vec::with_capacity(type_names.len());
    for ty in type_names {
        let i = itanium_typeinfo_symbol_for(ty)?;
        let m = msvc_typedesc_name_for(ty)?;
        itanium.push(i);
        msvc.push(m);
    }
    Some((itanium.join(","), msvc.join(",")))
}

/// P09.71 / 1.13 throws Phase 2F: render the v1.13.0
/// native-invoke attribute block for a throwing C++ function.
/// Emits the three rustc-fork-only attributes the fork's
/// codegen patches react to:
///
/// ```text
/// #[rustc_cxx_throws]
/// #[rustc_cxx_throws_typeinfos = "_ZTI...,_ZTI..."]
/// #[rustc_cxx_throws_msvc_typedescs = ".?AV...,.?AV..."]
/// ```
///
/// Returned string has a trailing newline so callers can
/// splice it directly above an `extern "C++" { fn ... }`
/// declaration. Each attribute line is prefixed with
/// `indent` to match the surrounding block.
///
/// When `typed_catches` is empty (untyped `cxx_throws`),
/// only `#[rustc_cxx_throws]` is emitted — catch-all
/// behavior is the Phase 1 default.
///
/// When any type in `typed_catches` can't be mangled by the
/// helpers, the typeinfo attributes are omitted (and the
/// caller falls back to the catch-all path). Use
/// [`manglings_for_typed_catches`] directly if you want to
/// detect this and fail-loud instead.
pub fn render_native_invoke_attr_block(
    typed_catches: &[String],
    indent: &str,
) -> String {
    let mut out = String::new();
    out.push_str(indent);
    out.push_str("#[rustc_cxx_throws]\n");
    if typed_catches.is_empty() {
        return out;
    }
    let Some((itanium_list, msvc_list)) = manglings_for_typed_catches(typed_catches)
    else {
        return out;
    };
    out.push_str(indent);
    out.push_str(&format!(
        "#[rustc_cxx_throws_typeinfos = \"{itanium_list}\"]\n",
    ));
    out.push_str(indent);
    out.push_str(&format!(
        "#[rustc_cxx_throws_msvc_typedescs = \"{msvc_list}\"]\n",
    ));
    out
}

/// Build the canonical FQN string from a `NestedName`'s
/// segments. Matches the form
/// `crate::rust_bindings::parent_path_to_fqn` produces, but
/// re-implemented here to keep the helper self-contained
/// (avoids exposing rust_bindings internals).
fn nested_name_to_fqn(segments: &[rustc_abi_cxx::NameSegment]) -> String {
    use rustc_abi_cxx::NameSegment;
    let mut parts = Vec::with_capacity(segments.len());
    for seg in segments {
        match seg {
            NameSegment::Namespace(id)
            | NameSegment::Class(id)
            | NameSegment::Enum(id) => parts.push(id.0.clone()),
            NameSegment::TemplateSpec { name, .. } => parts.push(name.0.clone()),
            NameSegment::AnonymousNamespace => parts.push("__anon".into()),
        }
    }
    parts.join("::")
}

/// v1.12.9: collect the typed-catches list for every throwing
/// free function in `free_fns` whose annotation is
/// `Annotation::CxxThrowsTyped(_)`. Returns a `BTreeMap` keyed
/// by the function's FQN (matching the lookup form
/// `crate::rust_bindings`'s free-fn emitter uses). Consumers
/// building the C++ shim feed the list straight to
/// [`render_throws_shim_cpp_typed`].
///
/// Functions with the plain `Annotation::CxxThrows` (no type
/// list) are NOT included — they're handled by
/// [`render_throws_shim_cpp`] which doesn't take a type list.
/// Likewise, throws-tagged functions reached only via the
/// `RustBindingsConfig::cxx_throws_functions` config knob are
/// untyped.
///
/// FQN format: `name` for TU-scope free fns, `ns::sub::name`
/// for namespaced ones. This matches the key
/// [`crate::AnnotationSet::effective`] uses.
pub fn collect_throws_catches(
    annotations: &crate::annotations::AnnotationSet,
    free_fns: &crate::free_fns::FreeFnSet,
) -> std::collections::BTreeMap<String, Vec<String>> {
    use crate::annotations::Annotation;
    use rustc_abi_cxx::NameSegment;

    let mut out: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for ff in &free_fns.entries {
        let fqn = if ff.parent.is_empty() {
            ff.name.0.clone()
        } else {
            let mut parts: Vec<String> = ff
                .parent
                .iter()
                .filter_map(|seg| match seg {
                    NameSegment::Namespace(id) => Some(id.0.clone()),
                    _ => None,
                })
                .collect();
            parts.push(ff.name.0.clone());
            parts.join("::")
        };
        for ann in annotations.effective(&fqn) {
            if let Annotation::CxxThrowsTyped(types) = ann {
                out.insert(fqn.clone(), types);
                break;
            }
        }
    }
    out
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
    fn itanium_typeinfo_for_global_class() {
        assert_eq!(
            itanium_typeinfo_symbol_for("DomainError"),
            Some("_ZTI11DomainError".to_string())
        );
        assert_eq!(
            itanium_typeinfo_symbol_for("RangeError"),
            Some("_ZTI10RangeError".to_string())
        );
    }

    #[test]
    fn itanium_typeinfo_for_std_class() {
        assert_eq!(
            itanium_typeinfo_symbol_for("std::runtime_error"),
            Some("_ZTISt13runtime_error".to_string())
        );
        assert_eq!(
            itanium_typeinfo_symbol_for("std::logic_error"),
            Some("_ZTISt11logic_error".to_string())
        );
    }

    #[test]
    fn itanium_typeinfo_rejects_unsupported_shapes() {
        assert_eq!(itanium_typeinfo_symbol_for(""), None);
        assert_eq!(itanium_typeinfo_symbol_for("a::b::C"), None);
        assert_eq!(itanium_typeinfo_symbol_for("std::vector<int>"), None);
        assert_eq!(itanium_typeinfo_symbol_for("Foo&"), None);
    }

    #[test]
    fn msvc_typedesc_for_global_class() {
        assert_eq!(
            msvc_typedesc_name_for("DomainError"),
            Some(".?AVDomainError@@".to_string())
        );
    }

    #[test]
    fn msvc_typedesc_for_std_class() {
        // MSVC: ".?AV" + reversed-namespace-components-with-@-sep
        // + "@@" terminator. For `std::runtime_error`, the
        // segments reversed give ["runtime_error", "std"], so the
        // result is ".?AVruntime_error@std@@".
        assert_eq!(
            msvc_typedesc_name_for("std::runtime_error"),
            Some(".?AVruntime_error@std@@".to_string())
        );
    }

    #[test]
    fn msvc_typedesc_for_nested_namespace() {
        // `my::lib::Error` segments reversed = [Error, lib, my].
        assert_eq!(
            msvc_typedesc_name_for("my::lib::Error"),
            Some(".?AVError@lib@my@@".to_string())
        );
    }

    #[test]
    fn manglings_for_typed_catches_pairs_lists() {
        let (itanium, msvc) = manglings_for_typed_catches(&[
            "DomainError".to_string(),
            "RangeError".to_string(),
        ])
        .expect("simple global types should mangle");
        assert_eq!(itanium, "_ZTI11DomainError,_ZTI10RangeError");
        assert_eq!(msvc, ".?AVDomainError@@,.?AVRangeError@@");
    }

    #[test]
    fn manglings_returns_none_on_any_unsupported() {
        let result = manglings_for_typed_catches(&[
            "DomainError".to_string(),
            "std::vector<int>".to_string(),
        ]);
        assert_eq!(result, None);
    }

    #[test]
    fn render_native_invoke_attr_block_untyped() {
        let s = render_native_invoke_attr_block(&[], "    ");
        assert_eq!(s, "    #[rustc_cxx_throws]\n");
    }

    #[test]
    fn render_native_invoke_attr_block_with_types() {
        let s = render_native_invoke_attr_block(
            &["DomainError".to_string(), "RangeError".to_string()],
            "    ",
        );
        assert!(s.contains("#[rustc_cxx_throws]\n"));
        assert!(s.contains(
            "#[rustc_cxx_throws_typeinfos = \"_ZTI11DomainError,_ZTI10RangeError\"]\n"
        ));
        assert!(s.contains(
            "#[rustc_cxx_throws_msvc_typedescs = \".?AVDomainError@@,.?AVRangeError@@\"]\n"
        ));
        // Indent prefix applied to every line.
        for line in s.lines() {
            assert!(line.starts_with("    "));
        }
    }

    #[test]
    fn render_native_invoke_attr_block_unsupported_falls_back_to_untyped() {
        // std::vector<int> can't be mangled — fall back to
        // emitting just `#[rustc_cxx_throws]` (catch-all).
        let s = render_native_invoke_attr_block(
            &["DomainError".to_string(), "std::vector<int>".to_string()],
            "",
        );
        assert_eq!(s, "#[rustc_cxx_throws]\n");
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
