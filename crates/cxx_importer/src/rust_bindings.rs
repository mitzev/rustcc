//! Rust binding emitter — turns an imported `CxxTypeCtx` into Rust
//! source that lets Rust call into the imported C++ classes.
//!
//! # Where this fits
//!
//! [`super::shims`] generates `extern "C" noexcept` C++ trampolines —
//! the Rust → C++ direction at the C++ side. [`super::rust_forwarders`]
//! emits Rust-side thunks for `#[repr(cpp)]` Rust-origin classes — the
//! C++ → Rust direction. This module fills the third corner: given
//! libclang-imported C++ classes (in `ctx`), emit **Rust** source so
//! downstream Rust code can `use` and call them.
//!
//! # Scope
//!
//! - Two backends implemented:
//!   - [`BindingsBackend::DirectExternCpp`] (default; import direction):
//!     `#[repr(C)]` struct + `unsafe extern "C++"` decls + safe
//!     wrappers + `Drop`. Fork-required for the `extern "C++"` ABI
//!     string and the broadened `compute_cxx_abi_info` overlay
//!     (P09.50) that routes ADT returns through the right
//!     indirect-result register per-target.
//!   - [`BindingsBackend::NativeCppClassMacro`] (export direction):
//!     `::rustcc_macros::native_cpp_class! { … }` invocations. Used
//!     when Rust is the authoritative side and C++ links against
//!     compiler-emitted Itanium symbols. **Wrong direction** for
//!     importing existing C++ headers.
//!   - [`BindingsBackend::CxxClassMacro`] (stable rustc, import):
//!     **TODO** — would emit the pre-fork `cxx_class!` macro shape.
//! - Method shapes covered: ctors, dtors (auto-Drop), const +
//!   non-const instance methods, static methods (instance / static
//!   distinction is heuristic until libclang's `is_static_method`
//!   gets surfaced through the importer).
//! - Parameter / return types covered: scalars (`int*`, `float*`,
//!   `bool`, `void`), raw pointer/reference wrappings, and
//!   record-by-value (rendered as the bare class name; works for
//!   same-translation-unit refs). Operator names, templates,
//!   virtuals, and inheritance return clear errors.
//! - No namespace scoping yet. Imported classes flatten to top
//!   level. When namespace recovery lands, this emitter grows a
//!   `mod` tree.
//!
//! # Output shapes
//!
//! ## DirectExternCpp (default — import direction)
//!
//! ```ignore
//! #[repr(C)]
//! #[repr(align(4))]
//! pub struct Point {
//!     _opaque: [::core::mem::MaybeUninit<u8>; 8],
//! }
//!
//! unsafe extern "C++" {
//!     #[link_name = "_ZN5PointC1Eii"]
//!     fn __cxx_Point_ctor_0(this: *mut Point, arg0: i32, arg1: i32);
//!     #[link_name = "_ZN5PointD1Ev"]
//!     fn __cxx_Point_dtor(this: *mut Point);
//!     #[link_name = "_ZNK5Point5get_xEv"]
//!     fn __cxx_Point_get_x(this: *const Point) -> i32;
//! }
//!
//! impl Point {
//!     pub fn new(arg0: i32, arg1: i32) -> Self {
//!         unsafe {
//!             let mut __slot = ::core::mem::MaybeUninit::<Self>::uninit();
//!             __cxx_Point_ctor_0(__slot.as_mut_ptr(), arg0, arg1);
//!             __slot.assume_init()
//!         }
//!     }
//!     pub fn get_x(&self) -> i32 {
//!         unsafe { __cxx_Point_get_x(self as *const Self) }
//!     }
//! }
//!
//! impl ::core::ops::Drop for Point {
//!     fn drop(&mut self) {
//!         unsafe { __cxx_Point_dtor(self as *mut Self); }
//!     }
//! }
//! ```
//!
//! ## NativeCppClassMacro (export direction)
//!
//! ```ignore
//! ::rustcc_macros::native_cpp_class! {
//!     #[size = 8]
//!     #[align = 4]
//!     pub class Point {
//!         #[ctor] fn new(x: i32, y: i32) -> Self;
//!         fn get_x(&self) -> i32;
//!         fn translated(&self, dx: i32, dy: i32) -> Point;
//!     }
//! }
//! ```
//!
//! # Roadmap (out of scope for v0, tracked in design doc §14)
//!
//! Operator mapping, overload renaming, sidecar-YAML annotations,
//! template instantiation, `CxxBase` upcast emission, virtual-method
//! vtable indices, lifetime annotations — each lands in its own
//! release in cxx_importer's milestone series.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rustc_abi_cxx::{
    ClassId, CtorVariant, CxxType, CxxTypeCtx, DtorVariant, FloatKind, IntWidth,
    MethodDef, MethodName, NameSegment, SpecialMember, Symbol, TypeId, Virtuality,
};

/// Selects the surface the emitter writes against. The three backends
/// span two orthogonal axes:
///
/// - **Direction**: import (Rust calls into a C++ class compiled
///   separately) vs export (Rust authors a class that C++ links
///   against). `cxx_importer`'s primary purpose is the import
///   direction; export is the `crates/rustcc_macros` macro path.
/// - **Toolchain**: stable rustc vs the rustcc fork. The fork unlocks
///   `extern "C++"` and `#[repr(cpp)]`; without it, emission falls
///   back to `extern "C"` + manual sret trampolines that are correct
///   on x86_64 SysV only.
///
/// See module-level docs for the per-backend contract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BindingsBackend {
    /// **Default — import direction, fork-required.** Emit a
    /// `#[repr(C)]` opaque struct, an `unsafe extern "C++"` block
    /// of `#[link_name = "_ZN…"]`-tagged decls, safe `impl`
    /// wrappers, and a `Drop` impl forwarding to the C++ dtor. The
    /// fork's broadened `compute_cxx_abi_info` (P09.50) routes
    /// indirect-result pointers per-target so by-value record
    /// returns work on aarch64 too.
    #[default]
    DirectExternCpp,
    /// Export direction, fork-required: emit `native_cpp_class! {
    /// … }` invocations. Used when the *Rust* side is the
    /// authoritative class definition and C++ links against it via
    /// the macro's auto-emitted forwarders. Wrong direction for
    /// importing existing C++ headers — use `DirectExternCpp` for
    /// that.
    NativeCppClassMacro,
    /// Stock-rustc-friendly import direction: emit `cxx_class! { …
    /// }` invocations. Compiles on stable rustc at the cost of
    /// the explicit-`__sret` trampoline trick (correct on x86_64
    /// SysV only). **TODO**.
    CxxClassMacro,
}

/// Tunables passed to [`generate_rust_bindings`]. Defaults select
/// the fork-friendly macro backend, top-level emission, no extras.
#[derive(Clone, Debug, Default)]
pub struct RustBindingsConfig {
    pub backend: BindingsBackend,
    /// Wrap the entire output in `pub mod {name} { … }` if `Some`.
    /// Convenient when `include!`-ing into a `lib.rs` that wants
    /// imported types under a specific module path.
    pub crate_module: Option<String>,
    /// Tag every emitted item with `#[doc(hidden)]`. Useful when the
    /// emitted file is part of an internal layer the downstream
    /// crate re-exports selectively.
    pub doc_hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingsError {
    /// The requested backend isn't implemented in this crate version.
    UnsupportedBackend { backend: BindingsBackend, reason: String },
    /// A type appeared in a position the emitter can't render. v0
    /// covers scalars + pointer/reference wrappings only.
    UnsupportedType { where_: String, kind: String },
    /// A method shape the emitter can't render — operator-named,
    /// virtual, conversion, etc.
    UnsupportedMethod { where_: String, why: String },
    /// `ctx.layout(class_id)` failed for a class slated for emission.
    /// Most often: a forward-declared-only class still in the IR
    /// without a definition.
    LayoutFailed { class: String, detail: String },
}

impl core::fmt::Display for BindingsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for BindingsError {}

/// Emit Rust source bridging the imported classes named in `classes`.
/// The caller is expected to have populated `ctx` via
/// [`super::driver::Driver::parse_all`] (or hand-built it for tests).
pub fn generate_rust_bindings(
    ctx: &CxxTypeCtx,
    classes: &[ClassId],
    config: &RustBindingsConfig,
) -> Result<String, BindingsError> {
    match config.backend {
        BindingsBackend::NativeCppClassMacro => emit_native_macro(ctx, classes, config),
        BindingsBackend::DirectExternCpp => emit_direct_extern_cpp(ctx, classes, config),
        backend @ BindingsBackend::CxxClassMacro => Err(BindingsError::UnsupportedBackend {
            backend,
            reason: "v0 scaffold ships `NativeCppClassMacro` (export direction) and \
                     `DirectExternCpp` (import direction). The stable-rustc-friendly \
                     `CxxClassMacro` shape is tracked for a follow-up release."
                .into(),
        }),
    }
}

fn emit_native_macro(
    ctx: &CxxTypeCtx,
    classes: &[ClassId],
    config: &RustBindingsConfig,
) -> Result<String, BindingsError> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// Generated by rustcc cxx_importer::rust_bindings. Do not hand-edit.\n\
         // Backend: NativeCppClassMacro (fork-only).\n"
    );

    let indent = if config.crate_module.is_some() { "    " } else { "" };
    if let Some(modname) = config.crate_module.as_deref() {
        let _ = writeln!(out, "pub mod {modname} {{");
    }

    for &class_id in classes {
        let block = render_class_block(ctx, class_id, config, indent)?;
        out.push_str(&block);
        out.push('\n');
    }

    if config.crate_module.is_some() {
        let _ = writeln!(out, "}}");
    }
    Ok(out)
}

/// Emit the auditable raw form: a `#[repr(C)]` struct with opaque
/// storage sized to match the C++ class, an `unsafe extern "C++"`
/// block declaring each method with its Itanium-mangled symbol via
/// `#[link_name]`, an `impl` block of safe Rust wrappers, and a
/// `Drop` impl forwarding to the C++ destructor (if the class has
/// a user dtor).
///
/// This is the **import direction**: a Rust crate calls into a
/// C++ class compiled separately. The fork's broadened
/// `compute_cxx_abi_info` (P09.50) makes record-returning methods
/// route the indirect-result pointer per-target (rdi on x86_64
/// SysV, x8 on AAPCS64), so by-value return Just Works.
fn emit_direct_extern_cpp(
    ctx: &CxxTypeCtx,
    classes: &[ClassId],
    config: &RustBindingsConfig,
) -> Result<String, BindingsError> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// Generated by rustcc cxx_importer::rust_bindings. Do not hand-edit.\n\
         // Backend: DirectExternCpp (fork-required for `extern \"C++\"`).\n"
    );

    let initial_indent = if config.crate_module.is_some() { "    " } else { "" };
    if let Some(modname) = config.crate_module.as_deref() {
        let _ = writeln!(out, "pub mod {modname} {{");
    }

    // Group classes by their `NestedName` namespace prefix so the
    // emitter recovers the C++ scope structure as a Rust `mod`
    // tree. A flat list (no `Namespace` segments) collapses to the
    // root and emits at the top level — same shape the v0 emitter
    // produced before this change, with no namespace overhead.
    let tree = build_namespace_tree(ctx, classes)?;
    render_namespace_tree(ctx, &tree, &mut out, config, initial_indent)?;

    if config.crate_module.is_some() {
        let _ = writeln!(out, "}}");
    }
    Ok(out)
}

/// Tree of imported classes grouped by their C++ namespace prefix.
/// A class with `NestedName = [Namespace("ns"), Class("Foo")]` lands
/// at `root.children["ns"].classes` containing its `ClassId`.
#[derive(Default)]
struct NamespaceTree {
    /// Classes directly inside this scope.
    classes: Vec<ClassId>,
    /// Sub-namespaces at this scope, keyed by name.
    /// `BTreeMap` for deterministic emission order.
    children: BTreeMap<String, NamespaceTree>,
}

fn build_namespace_tree(
    ctx: &CxxTypeCtx,
    classes: &[ClassId],
) -> Result<NamespaceTree, BindingsError> {
    let mut root = NamespaceTree::default();
    for &class_id in classes {
        let class = ctx.class(class_id);
        let segments = &class.name.0;
        if segments.is_empty() {
            return Err(BindingsError::UnsupportedType {
                where_: format!("class {class_id:?}"),
                kind: "empty NestedName".into(),
            });
        }
        // Walk every segment except the final one, which is the
        // class identifier itself.
        let prefix = &segments[..segments.len() - 1];
        let mut node = &mut root;
        for seg in prefix {
            let key = match seg {
                NameSegment::Namespace(id) => id.0.clone(),
                NameSegment::AnonymousNamespace => "__anon".to_string(),
                other => {
                    return Err(BindingsError::UnsupportedType {
                        where_: format!(
                            "{}",
                            ident_of_class(class).unwrap_or_else(|| "<class>".into())
                        ),
                        kind: format!(
                            "v0 namespace tree only handles Namespace / \
                             AnonymousNamespace prefixes; got {other:?}"
                        ),
                    });
                }
            };
            node = node.children.entry(key).or_default();
        }
        node.classes.push(class_id);
    }
    Ok(root)
}

fn render_namespace_tree(
    ctx: &CxxTypeCtx,
    tree: &NamespaceTree,
    out: &mut String,
    config: &RustBindingsConfig,
    indent: &str,
) -> Result<(), BindingsError> {
    for &class_id in &tree.classes {
        let block = render_direct_extern_class(ctx, class_id, config, indent)?;
        out.push_str(&block);
        out.push('\n');
    }
    for (name, child) in &tree.children {
        let _ = writeln!(out, "{indent}pub mod {name} {{");
        let inner_indent = format!("{indent}    ");
        render_namespace_tree(ctx, child, out, config, &inner_indent)?;
        let _ = writeln!(out, "{indent}}}");
    }
    Ok(())
}

fn render_direct_extern_class(
    ctx: &CxxTypeCtx,
    class_id: ClassId,
    config: &RustBindingsConfig,
    indent: &str,
) -> Result<String, BindingsError> {
    let class = ctx.class(class_id);
    let class_name = ident_of_class(class).ok_or_else(|| BindingsError::UnsupportedType {
        where_: "class name".into(),
        kind: "anonymous or non-identifier-named class".into(),
    })?;
    let layout = ctx.layout(class_id).map_err(|e| BindingsError::LayoutFailed {
        class: class_name.clone(),
        detail: format!("{e:?}"),
    })?;

    // Collect methods up front — we need them for the extern block,
    // the impl block, and the Drop check. Rejecting on virtual,
    // operator, and conversion methods up front keeps later code
    // simple.
    let methods = ctx.class(class_id).methods.clone();
    for m in &methods {
        if m.virtuality != Virtuality::NonVirtual {
            return Err(BindingsError::UnsupportedMethod {
                where_: format!("{class_name}::{:?}", m.name),
                why: "virtual methods deferred until vtable-aware emission lands".into(),
            });
        }
    }

    let mut block = String::new();
    if config.doc_hidden {
        let _ = writeln!(block, "{indent}#[doc(hidden)]");
    }

    // 1. Struct decl. `#[repr(C)]` + explicit alignment matches the
    //    C++ class layout for POD shapes (single inheritance, no
    //    vtable). Polymorphic / virtual-base classes need
    //    `#[repr(cpp)]` and the full Itanium layout machinery —
    //    rejected at the top of this function.
    let _ = writeln!(block, "{indent}#[repr(C)]");
    let _ = writeln!(block, "{indent}#[repr(align({}))]", layout.align_bytes);
    let _ = writeln!(block, "{indent}pub struct {class_name} {{");
    let _ = writeln!(
        block,
        "{indent}    _opaque: [::core::mem::MaybeUninit<u8>; {}],",
        layout.size_bytes,
    );
    let _ = writeln!(block, "{indent}}}");
    let _ = writeln!(block);

    // 2. `unsafe extern "C++"` block declaring each method with its
    //    Itanium-mangled symbol via `#[link_name]`. The fork's ABI
    //    overlay routes records and ADTs through the right
    //    indirect-result register; pointer/scalar args are
    //    register-passed identically to C.
    let _ = writeln!(block, "{indent}unsafe extern \"C++\" {{");

    let mut ctor_seen = 0usize;
    let mut method_blocks: Vec<MethodEmission> = Vec::with_capacity(methods.len());
    let mut has_user_dtor = false;
    for method in &methods {
        let emission = classify_for_direct_extern(ctx, class_id, &class_name, method)?;
        if matches!(emission.kind, EmissionKind::Dtor) {
            has_user_dtor = true;
        }
        if matches!(emission.kind, EmissionKind::Ctor) {
            ctor_seen += 1;
        }
        // Emit the extern decl line.
        let _ = writeln!(
            block,
            "{indent}    #[link_name = \"{}\"]",
            emission.link_name,
        );
        let _ = writeln!(
            block,
            "{indent}    fn {ext}({decl}){ret};",
            ext = emission.extern_ident,
            decl = emission.extern_decl_params,
            ret = emission.extern_return_clause,
        );
        method_blocks.push(emission);
    }
    let _ = writeln!(block, "{indent}}}");
    let _ = writeln!(block);

    // 3. Inherent `impl` block with safe wrappers. Each method
    //    forwards to its extern decl with the appropriate `unsafe`
    //    block.
    let _ = writeln!(block, "{indent}impl {class_name} {{");
    let mut wrote_any_method = false;
    for emission in &method_blocks {
        if matches!(emission.kind, EmissionKind::Dtor) {
            // Dtor is exposed through `Drop`, not the impl block.
            continue;
        }
        let body = render_direct_extern_wrapper(emission, &class_name, indent);
        block.push_str(&body);
        wrote_any_method = true;
    }
    if !wrote_any_method {
        // Empty impl block stays well-formed; emit a placeholder so
        // grep finds the type.
    }
    let _ = writeln!(block, "{indent}}}");

    // 4. `Drop` impl. Always emitted when the class has a user dtor;
    //    otherwise skipped (default-Drop on the opaque storage is a
    //    no-op, which matches a trivial C++ dtor's semantics).
    if has_user_dtor {
        let dtor = method_blocks
            .iter()
            .find(|e| matches!(e.kind, EmissionKind::Dtor))
            .expect("user dtor seen");
        let _ = writeln!(block);
        let _ = writeln!(block, "{indent}impl ::core::ops::Drop for {class_name} {{");
        let _ = writeln!(block, "{indent}    fn drop(&mut self) {{");
        let _ = writeln!(
            block,
            "{indent}        unsafe {{ {ext}(self as *mut Self); }}",
            ext = dtor.extern_ident,
        );
        let _ = writeln!(block, "{indent}    }}");
        let _ = writeln!(block, "{indent}}}");
    }

    // Sanity check: more than one ctor would need disambiguator suffixes
    // on the wrapper names. v0 supports a single ctor per class.
    if ctor_seen > 1 {
        return Err(BindingsError::UnsupportedMethod {
            where_: class_name.clone(),
            why: format!(
                "{ctor_seen} constructors imported but the v0 emitter \
                 only supports a single ctor; overload disambiguation \
                 is tracked for a follow-up release"
            ),
        });
    }

    Ok(block)
}

/// Internal record describing one method's lowered shape: enough info
/// to render the extern decl, the wrapper, and the Drop impl from a
/// single classify pass.
struct MethodEmission {
    kind: EmissionKind,
    /// Source-level method name as it appears on the C++ class
    /// (e.g. `sum`, `new`). For ctors this is always `new`; for
    /// dtors it's not user-visible.
    rust_name: String,
    /// Internal extern fn name (e.g. `__cxx_Calc_sum`). Always
    /// unique per class.
    extern_ident: String,
    /// Symbol passed in `#[link_name = "…"]` — Itanium-mangled.
    link_name: String,
    /// Extern decl param list, including the implicit `this:
    /// *const/*mut Self` slot for instance methods + ctors + dtors.
    extern_decl_params: String,
    /// Extern decl return clause (`""` for void-returning, `"
    /// -> i32"` otherwise).
    extern_return_clause: String,
    /// Wrapper signature receiver: `&self` / `&mut self` / `""`
    /// (for static / ctor).
    wrapper_receiver: WrapperReceiver,
    /// Wrapper user-visible param list (excluding receiver).
    wrapper_params: String,
    /// Wrapper return type (e.g. `Self`, `i32`, `()`).
    wrapper_return: String,
    /// Argument expressions to forward to the extern call.
    forward_args: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EmissionKind {
    Ctor,
    Dtor,
    /// `&self` / `&mut self` instance method.
    Instance,
    /// No-self static method.
    Static,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WrapperReceiver {
    SelfConst,
    SelfMut,
    None,
    Ctor,
}

fn classify_for_direct_extern(
    ctx: &CxxTypeCtx,
    class_id: ClassId,
    class_name: &str,
    method: &MethodDef,
) -> Result<MethodEmission, BindingsError> {
    let arity = method.sig.params.len();

    // Build user-arg decls + forward expressions. These are shared
    // across method shapes (the `this` slot is added separately).
    let mut user_arg_decls = Vec::with_capacity(arity);
    let mut user_forward = Vec::with_capacity(arity);
    for (i, &ty_id) in method.sig.params.iter().enumerate() {
        let rust_ty = render_rust_type(
            ctx,
            ty_id,
            &format!("{class_name}::{:?} param {i}", method.name),
        )?;
        user_arg_decls.push(format!("arg{i}: {rust_ty}"));
        user_forward.push(format!("arg{i}"));
    }

    // Special-case ctor / dtor first.
    match &method.special {
        Some(SpecialMember::DefaultCtor | SpecialMember::OtherCtor) => {
            let mut decl = vec![format!("this: *mut {class_name}")];
            decl.extend(user_arg_decls.clone());
            let link = ctx.mangle(&Symbol::Ctor {
                class: class_id,
                variant: CtorVariant::C1,
                sig: method.sig.clone(),
            });
            return Ok(MethodEmission {
                kind: EmissionKind::Ctor,
                rust_name: "new".into(),
                extern_ident: format!("__cxx_{class_name}_ctor_0"),
                link_name: link,
                extern_decl_params: decl.join(", "),
                extern_return_clause: String::new(),
                wrapper_receiver: WrapperReceiver::Ctor,
                wrapper_params: user_arg_decls.join(", "),
                wrapper_return: "Self".into(),
                forward_args: user_forward.join(", "),
            });
        }
        Some(SpecialMember::Dtor) => {
            let link = ctx.mangle(&Symbol::Dtor {
                class: class_id,
                variant: DtorVariant::D1,
            });
            return Ok(MethodEmission {
                kind: EmissionKind::Dtor,
                rust_name: "drop".into(),
                extern_ident: format!("__cxx_{class_name}_dtor"),
                link_name: link,
                extern_decl_params: format!("this: *mut {class_name}"),
                extern_return_clause: String::new(),
                wrapper_receiver: WrapperReceiver::SelfMut,
                wrapper_params: String::new(),
                wrapper_return: "()".into(),
                forward_args: String::new(),
            });
        }
        Some(other) => {
            return Err(BindingsError::UnsupportedMethod {
                where_: format!("{class_name}::{:?}", method.name),
                why: format!(
                    "special member `{other:?}` not yet wired (v0 covers Ctor + Dtor + \
                     plain instance/static methods)"
                ),
            });
        }
        None => {}
    }

    // Operator + conversion-named methods deferred — tracked by
    // their own milestone in the design doc.
    let method_name = match &method.name {
        MethodName::Ident(id) => id.0.clone(),
        other => {
            return Err(BindingsError::UnsupportedMethod {
                where_: format!("{class_name}::{other:?}"),
                why: "operator + conversion-function names deferred (v0 takes \
                      identifier-named methods only)"
                    .into(),
            });
        }
    };

    // Render return type for both extern + wrapper. C++ `void` →
    // Rust `()`, surfaced in the wrapper as no return clause.
    let ret_rust =
        render_rust_type(ctx, method.sig.ret, &format!("{class_name}::{method_name} return"))?;

    let extern_ret_clause = if ret_rust == "()" {
        String::new()
    } else {
        format!(" -> {ret_rust}")
    };

    // Distinguish instance vs static. The current importer doesn't
    // mark static methods explicitly, so we infer: any non-special
    // method without a `cv` const flag and with `MethodDef::name`
    // outside the special-member list is treated as an instance
    // method by default. (Real static-method support arrives once
    // libclang's `is_static_method` flag gets surfaced.)
    let kind = EmissionKind::Instance;
    let receiver = if method.sig.cv.is_const {
        WrapperReceiver::SelfConst
    } else {
        WrapperReceiver::SelfMut
    };
    let this_ty = match receiver {
        WrapperReceiver::SelfConst => format!("this: *const {class_name}"),
        _ => format!("this: *mut {class_name}"),
    };
    let mut extern_decl = vec![this_ty];
    extern_decl.extend(user_arg_decls.clone());

    let link = ctx.mangle(&Symbol::Method {
        class: class_id,
        name: MethodName::Ident(rustc_abi_cxx::Ident(method_name.clone())),
        sig: method.sig.clone(),
    });

    Ok(MethodEmission {
        kind,
        rust_name: method_name.clone(),
        extern_ident: format!("__cxx_{class_name}_{method_name}"),
        link_name: link,
        extern_decl_params: extern_decl.join(", "),
        extern_return_clause: extern_ret_clause,
        wrapper_receiver: receiver,
        wrapper_params: user_arg_decls.join(", "),
        wrapper_return: ret_rust,
        forward_args: user_forward.join(", "),
    })
}

fn render_direct_extern_wrapper(
    emission: &MethodEmission,
    class_name: &str,
    block_indent: &str,
) -> String {
    let _ = class_name; // captured for future Self/ClassName disambiguation.
    let mut out = String::new();
    // The wrapper sits inside `impl Class { … }`, which is itself
    // indented at `block_indent`. So function heads are at
    // `block_indent + "    "`. We emit relative to that — the
    // current code calls this `indent` for brevity.
    let indent = format!("{block_indent}    ");
    let indent = indent.as_str();
    match emission.kind {
        EmissionKind::Ctor => {
            // Wrapper for ctors: allocate a stack temp, call the
            // C++ ctor, materialize the value via assume_init.
            let _ = writeln!(
                out,
                "{indent}pub fn {name}({params}) -> Self {{",
                name = emission.rust_name,
                params = emission.wrapper_params,
            );
            let _ = writeln!(
                out,
                "{indent}    unsafe {{",
            );
            let _ = writeln!(
                out,
                "{indent}        let mut __slot = ::core::mem::MaybeUninit::<Self>::uninit();",
            );
            if emission.forward_args.is_empty() {
                let _ = writeln!(
                    out,
                    "{indent}        {ext}(__slot.as_mut_ptr());",
                    ext = emission.extern_ident,
                );
            } else {
                let _ = writeln!(
                    out,
                    "{indent}        {ext}(__slot.as_mut_ptr(), {fwd});",
                    ext = emission.extern_ident,
                    fwd = emission.forward_args,
                );
            }
            let _ = writeln!(
                out,
                "{indent}        __slot.assume_init()",
            );
            let _ = writeln!(out, "{indent}    }}");
            let _ = writeln!(out, "{indent}}}");
        }
        EmissionKind::Instance => {
            let receiver_kw = match emission.wrapper_receiver {
                WrapperReceiver::SelfConst => "&self",
                WrapperReceiver::SelfMut => "&mut self",
                _ => unreachable!("instance method without self receiver"),
            };
            let self_cast = match emission.wrapper_receiver {
                WrapperReceiver::SelfConst => "self as *const Self",
                WrapperReceiver::SelfMut => "self as *mut Self",
                _ => unreachable!(),
            };
            let ret_clause = if emission.wrapper_return == "()" {
                String::new()
            } else {
                format!(" -> {}", emission.wrapper_return)
            };
            let head = if emission.wrapper_params.is_empty() {
                format!(
                    "{indent}pub fn {name}({recv}){ret} {{",
                    name = emission.rust_name,
                    recv = receiver_kw,
                    ret = ret_clause,
                )
            } else {
                format!(
                    "{indent}pub fn {name}({recv}, {params}){ret} {{",
                    name = emission.rust_name,
                    recv = receiver_kw,
                    params = emission.wrapper_params,
                    ret = ret_clause,
                )
            };
            let _ = writeln!(out, "{head}");
            if emission.forward_args.is_empty() {
                let _ = writeln!(
                    out,
                    "{indent}    unsafe {{ {ext}({this}) }}",
                    ext = emission.extern_ident,
                    this = self_cast,
                );
            } else {
                let _ = writeln!(
                    out,
                    "{indent}    unsafe {{ {ext}({this}, {fwd}) }}",
                    ext = emission.extern_ident,
                    this = self_cast,
                    fwd = emission.forward_args,
                );
            }
            let _ = writeln!(out, "{indent}}}");
        }
        EmissionKind::Static => {
            let ret_clause = if emission.wrapper_return == "()" {
                String::new()
            } else {
                format!(" -> {}", emission.wrapper_return)
            };
            let _ = writeln!(
                out,
                "{indent}pub fn {name}({params}){ret} {{",
                name = emission.rust_name,
                params = emission.wrapper_params,
                ret = ret_clause,
            );
            if emission.forward_args.is_empty() {
                let _ = writeln!(
                    out,
                    "{indent}    unsafe {{ {ext}() }}",
                    ext = emission.extern_ident,
                );
            } else {
                let _ = writeln!(
                    out,
                    "{indent}    unsafe {{ {ext}({fwd}) }}",
                    ext = emission.extern_ident,
                    fwd = emission.forward_args,
                );
            }
            let _ = writeln!(out, "{indent}}}");
        }
        EmissionKind::Dtor => {
            // Dtor goes into the Drop impl, not the inherent impl.
        }
    }
    out
}

fn render_class_block(
    ctx: &CxxTypeCtx,
    class_id: ClassId,
    config: &RustBindingsConfig,
    indent: &str,
) -> Result<String, BindingsError> {
    let class = ctx.class(class_id);
    let class_name = ident_of_class(class).ok_or_else(|| BindingsError::UnsupportedType {
        where_: "class name".into(),
        kind: "anonymous or non-identifier-named class".into(),
    })?;

    let layout = ctx.layout(class_id).map_err(|e| BindingsError::LayoutFailed {
        class: class_name.clone(),
        detail: format!("{e:?}"),
    })?;

    let mut block = String::new();
    if config.doc_hidden {
        let _ = writeln!(block, "{indent}#[doc(hidden)]");
    }
    let _ = writeln!(block, "{indent}::rustcc_macros::native_cpp_class! {{");
    let _ = writeln!(block, "{indent}    #[size = {}]", layout.size_bytes);
    let _ = writeln!(block, "{indent}    #[align = {}]", layout.align_bytes);
    let _ = writeln!(block, "{indent}    pub class {class_name} {{");

    for method in &class.methods {
        if method.virtuality != Virtuality::NonVirtual {
            return Err(BindingsError::UnsupportedMethod {
                where_: format!("{class_name}::{:?}", method.name),
                why: "virtual methods deferred until vtable-aware emission lands".into(),
            });
        }
        let line = render_method(ctx, &class_name, method)?;
        let _ = writeln!(block, "{indent}        {line}");
    }

    let _ = writeln!(block, "{indent}    }}");
    let _ = writeln!(block, "{indent}}}");
    Ok(block)
}

fn render_method(
    ctx: &CxxTypeCtx,
    class_name: &str,
    method: &MethodDef,
) -> Result<String, BindingsError> {
    // Special members the macro recognizes as ctor / dtor get the
    // matching attribute; everything else falls through to instance
    // / static method shape inferred from the receiver.
    let (attr, name) = match (&method.special, &method.name) {
        (Some(SpecialMember::DefaultCtor | SpecialMember::OtherCtor), _) => {
            ("#[ctor] ", "new".to_string())
        }
        (Some(SpecialMember::Dtor), _) => ("#[dtor] ", "drop".to_string()),
        (Some(other), _) => {
            return Err(BindingsError::UnsupportedMethod {
                where_: format!("{class_name}::{:?}", method.name),
                why: format!("special member `{other:?}` not yet wired (v0 covers ctor + dtor only)"),
            });
        }
        (None, MethodName::Ident(id)) => ("", id.0.clone()),
        (None, other) => {
            return Err(BindingsError::UnsupportedMethod {
                where_: format!("{class_name}::{other:?}"),
                why: "operator + conversion-function names deferred (v0 takes identifier names \
                      only)".into(),
            });
        }
    };

    let receiver = if matches!(method.special, Some(SpecialMember::DefaultCtor | SpecialMember::OtherCtor)) {
        // Ctors take user args only; the macro synthesizes the `this` slot.
        ""
    } else if matches!(method.special, Some(SpecialMember::Dtor)) {
        "&mut self"
    } else if method.sig.cv.is_const {
        "&self"
    } else {
        // No receiver field on MethodDef — disambiguate static vs
        // instance by the importer's own classification. The current
        // importer doesn't yet distinguish, so we pessimistically
        // default to non-const instance for non-const sig (the common
        // case). Static methods would need a flag the importer adds
        // later; until then, mark them via the class name appearing
        // in the method's nested name.
        "&mut self"
    };

    let mut params = Vec::with_capacity(method.sig.params.len());
    for (i, &ty_id) in method.sig.params.iter().enumerate() {
        let rust_ty = render_rust_type(ctx, ty_id, &format!("{class_name}::{name} param {i}"))?;
        params.push(format!("arg{i}: {rust_ty}"));
    }

    let mut signature = String::new();
    let _ = write!(signature, "{attr}fn {name}(");
    let mut needs_comma = false;
    if !receiver.is_empty() {
        signature.push_str(receiver);
        needs_comma = true;
    }
    for p in &params {
        if needs_comma {
            signature.push_str(", ");
        }
        signature.push_str(p);
        needs_comma = true;
    }
    signature.push(')');

    // Return type: ctors are special — the macro requires `-> Self`
    // (or `-> ClassName`), regardless of how the C++ ctor is typed
    // on the Rust side (ctors return void in C++ but produce a
    // `Self`-shaped temporary in Rust). Dtors return `()` (the
    // method `drop(&mut self)` in the macro).
    if matches!(method.special, Some(SpecialMember::DefaultCtor | SpecialMember::OtherCtor)) {
        signature.push_str(" -> Self");
    } else if matches!(method.special, Some(SpecialMember::Dtor)) {
        // No return — `()` is implied.
    } else {
        let ret_ty = render_rust_type(ctx, method.sig.ret, &format!("{class_name}::{name} return"))?;
        if ret_ty != "()" {
            signature.push_str(" -> ");
            signature.push_str(&ret_ty);
        }
    }
    signature.push(';');
    Ok(signature)
}

/// Map a `CxxType` (resolved via `ctx`) to a Rust type token suitable
/// for the macro's `Signature` parser. Mirrors
/// [`super::rust_forwarders::render_type_rust`] but tightened to the
/// scalar / pointer / reference / record subset this emitter ships.
fn render_rust_type(
    ctx: &CxxTypeCtx,
    ty: TypeId,
    where_: &str,
) -> Result<String, BindingsError> {
    Ok(match ctx.type_of(ty) {
        CxxType::Void => "()".into(),
        CxxType::Bool => "bool".into(),
        CxxType::Int { signed, width } => int_rust(*signed, *width).into(),
        CxxType::Float { kind } => match kind {
            FloatKind::F32 => "f32".into(),
            FloatKind::F64 => "f64".into(),
            other => {
                return Err(BindingsError::UnsupportedType {
                    where_: where_.into(),
                    kind: format!("{other:?}"),
                });
            }
        },
        CxxType::Ptr { pointee, cv } => {
            let inner = render_rust_type(ctx, *pointee, where_)?;
            if cv.is_const {
                format!("*const {inner}")
            } else {
                format!("*mut {inner}")
            }
        }
        CxxType::Ref { pointee, kind, cv } => {
            // C++ references map to raw pointers in v0 (the design
            // doc's caller-destroys / lifetimebound annotations land
            // later — until then we don't have the lifetime info to
            // promote to `&T` / `&mut T` safely).
            let _ = kind; // RefKind::LValue / RValue both lower the same.
            let inner = render_rust_type(ctx, *pointee, where_)?;
            if cv.is_const {
                format!("*const {inner}")
            } else {
                format!("*mut {inner}")
            }
        }
        CxxType::Record(class_id) => {
            let class = ctx.class(*class_id);
            ident_of_class(class).ok_or_else(|| BindingsError::UnsupportedType {
                where_: where_.into(),
                kind: "anonymous record".into(),
            })?
        }
        other => {
            return Err(BindingsError::UnsupportedType {
                where_: where_.into(),
                kind: format!("{other:?}"),
            });
        }
    })
}

fn int_rust(signed: bool, width: IntWidth) -> &'static str {
    match (signed, width) {
        (true, IntWidth::I8) => "i8",
        (false, IntWidth::I8) => "u8",
        (true, IntWidth::I16) => "i16",
        (false, IntWidth::I16) => "u16",
        (true, IntWidth::I32) => "i32",
        (false, IntWidth::I32) => "u32",
        (true, IntWidth::I64) => "i64",
        (false, IntWidth::I64) => "u64",
        (true, IntWidth::I128) => "i128",
        (false, IntWidth::I128) => "u128",
    }
}

fn ident_of_class(class: &rustc_abi_cxx::ClassDef) -> Option<String> {
    use rustc_abi_cxx::NameSegment;
    // v0: take the trailing segment as the class identifier.
    // Namespace recovery (proper `mod foo { class Bar }` nesting) is
    // tracked for a later release.
    class.name.0.last().and_then(|seg| match seg {
        NameSegment::Class(id) | NameSegment::Namespace(id) => Some(id.0.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_abi_cxx::{
        ClassDef, CvQual, FieldDef, FnSig, Ident, NameSegment, NestedName,
        RecordKind, Target,
    };

    fn point_ctx() -> (CxxTypeCtx, ClassId) {
        let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
        let i32_ = ctx.intern_type(CxxType::Int {
            signed: true,
            width: IntWidth::I32,
        });
        let void_ = ctx.intern_type(CxxType::Void);
        let id = ctx.define_rust_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Point".into()))]),
            bases: vec![],
            fields: vec![
                FieldDef {
                    name: Ident("x".into()),
                    ty: i32_,
                    explicit_align: None,
                },
                FieldDef {
                    name: Ident("y".into()),
                    ty: i32_,
                    explicit_align: None,
                },
            ],
            methods: vec![],
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        let point_ty = ctx.intern_type(CxxType::Record(id));
        // Point::new(x, y)
        ctx.class_mut(id).methods.push(MethodDef {
            name: MethodName::Ident(Ident("new".into())),
            sig: FnSig {
                params: vec![i32_, i32_],
                ret: void_,
                cv: CvQual::default(),
                ref_q: None,
                variadic: false,
                noexcept: true,
            },
            virtuality: Virtuality::NonVirtual,
            vtable_index: None,
            special: Some(SpecialMember::OtherCtor),
        });
        // Point::get_x() const -> i32
        ctx.class_mut(id).methods.push(MethodDef {
            name: MethodName::Ident(Ident("get_x".into())),
            sig: FnSig {
                params: vec![],
                ret: i32_,
                cv: CvQual { is_const: true, is_volatile: false },
                ref_q: None,
                variadic: false,
                noexcept: true,
            },
            virtuality: Virtuality::NonVirtual,
            vtable_index: None,
            special: None,
        });
        // Point::translated(dx, dy) const -> Point
        ctx.class_mut(id).methods.push(MethodDef {
            name: MethodName::Ident(Ident("translated".into())),
            sig: FnSig {
                params: vec![i32_, i32_],
                ret: point_ty,
                cv: CvQual { is_const: true, is_volatile: false },
                ref_q: None,
                variadic: false,
                noexcept: true,
            },
            virtuality: Virtuality::NonVirtual,
            vtable_index: None,
            special: None,
        });
        (ctx, id)
    }

    #[test]
    fn default_backend_is_direct_extern_cpp_for_import_direction() {
        // Sanity: the documented default is DirectExternCpp. Users
        // who want the export-direction macro must opt in
        // explicitly via `BindingsBackend::NativeCppClassMacro`.
        assert_eq!(
            BindingsBackend::default(),
            BindingsBackend::DirectExternCpp,
        );
    }

    #[test]
    fn direct_extern_cpp_emits_repr_c_struct_extern_block_and_drop() {
        let (ctx, id) = point_ctx();
        let src = generate_rust_bindings(
            &ctx,
            &[id],
            &RustBindingsConfig::default(),
        )
        .expect("emit");

        // Struct with explicit repr(C) + alignment + opaque storage
        // sized per `ctx.layout(id)`.
        assert!(
            src.contains("#[repr(C)]"),
            "expected #[repr(C)]:\n{src}"
        );
        assert!(
            src.contains("#[repr(align(4))]"),
            "expected #[repr(align(4))]:\n{src}"
        );
        assert!(
            src.contains(
                "_opaque: [::core::mem::MaybeUninit<u8>; 8]",
            ),
            "expected opaque storage sized 8 bytes:\n{src}"
        );
        // extern "C++" block with mangled link names. The mangling
        // doesn't depend on the test's host target, so this string
        // is stable across CI runners.
        assert!(
            src.contains("unsafe extern \"C++\""),
            "expected unsafe extern \"C++\" block:\n{src}"
        );
        assert!(
            src.contains("_ZN5PointC1Eii"),
            "expected ctor link_name:\n{src}"
        );
        assert!(
            src.contains("_ZNK5Point10translatedEii"),
            "expected translated() link_name:\n{src}"
        );
        // Wrapper signatures.
        assert!(
            src.contains("pub fn new(arg0: i32, arg1: i32) -> Self {"),
            "expected ctor wrapper signature:\n{src}"
        );
        assert!(
            src.contains("pub fn translated(&self, arg0: i32, arg1: i32) -> Point {"),
            "expected translated wrapper signature:\n{src}"
        );
        // No Drop impl unless the class has a user dtor — Point in
        // this synthetic ctx doesn't have one.
        assert!(
            !src.contains("impl ::core::ops::Drop for Point"),
            "synthetic Point has no user dtor; Drop impl shouldn't be emitted:\n{src}"
        );
    }

    #[test]
    fn native_macro_emits_class_block_with_size_align_and_methods() {
        let (ctx, id) = point_ctx();
        let cfg = RustBindingsConfig {
            backend: BindingsBackend::NativeCppClassMacro,
            ..RustBindingsConfig::default()
        };
        let src = generate_rust_bindings(&ctx, &[id], &cfg).expect("emit");

        // Macro path + class header.
        assert!(
            src.contains("::rustcc_macros::native_cpp_class!"),
            "expected native_cpp_class! invocation:\n{src}"
        );
        assert!(
            src.contains("pub class Point"),
            "expected `pub class Point`:\n{src}"
        );
        // Two i32 fields → 8 bytes / 4-byte align under Itanium POD layout.
        assert!(
            src.contains("#[size = 8]"),
            "expected #[size = 8]:\n{src}"
        );
        assert!(
            src.contains("#[align = 4]"),
            "expected #[align = 4]:\n{src}"
        );
        // Ctor + const method + record-return method present.
        assert!(
            src.contains("#[ctor] fn new(arg0: i32, arg1: i32) -> Self;"),
            "expected ctor line:\n{src}"
        );
        assert!(
            src.contains("fn get_x(&self) -> i32;"),
            "expected const method line:\n{src}"
        );
        assert!(
            src.contains("fn translated(&self, arg0: i32, arg1: i32) -> Point;"),
            "expected record-return method line:\n{src}"
        );
    }

    #[test]
    fn crate_module_wraps_emission() {
        let (ctx, id) = point_ctx();
        let cfg = RustBindingsConfig {
            crate_module: Some("widgets".into()),
            ..RustBindingsConfig::default()
        };
        let src = generate_rust_bindings(&ctx, &[id], &cfg).expect("emit");
        assert!(
            src.contains("pub mod widgets {"),
            "expected wrapping module:\n{src}"
        );
        assert!(
            src.contains("}\n"),
            "expected closing brace for module:\n{src}"
        );
    }

    #[test]
    fn namespaced_class_emits_pub_mod_wrapping_in_direct_extern_cpp() {
        // A class in a C++ namespace `ns` must emit as
        // `pub mod ns { #[repr(C)] pub struct Foo … }`. Mirrors
        // `imports_class_inside_single_namespace` in the libclang
        // suite — same `NestedName` shape, no clang dep needed
        // here.
        let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
        let i32_ = ctx.intern_type(CxxType::Int {
            signed: true,
            width: IntWidth::I32,
        });
        let id = ctx.define_rust_class(ClassDef {
            name: NestedName(vec![
                NameSegment::Namespace(Ident("ns".into())),
                NameSegment::Class(Ident("Foo".into())),
            ]),
            bases: vec![],
            fields: vec![FieldDef {
                name: Ident("x".into()),
                ty: i32_,
                explicit_align: None,
            }],
            methods: vec![],
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });

        let src = generate_rust_bindings(&ctx, &[id], &RustBindingsConfig::default())
            .expect("emit");

        assert!(
            src.contains("pub mod ns {"),
            "expected `pub mod ns` wrap:\n{src}"
        );
        assert!(
            src.contains("pub struct Foo"),
            "expected `pub struct Foo`:\n{src}"
        );
        // The class block should be indented one level past the
        // module wrapper.
        assert!(
            src.contains("    #[repr(C)]") || src.contains("\n    pub struct Foo"),
            "expected indented class block:\n{src}"
        );
    }

    #[test]
    fn cxx_class_macro_backend_returns_clear_unsupported_error() {
        let (ctx, id) = point_ctx();
        let cfg = RustBindingsConfig {
            backend: BindingsBackend::CxxClassMacro,
            ..RustBindingsConfig::default()
        };
        let err = generate_rust_bindings(&ctx, &[id], &cfg).unwrap_err();
        assert!(
            matches!(err, BindingsError::UnsupportedBackend { .. }),
            "expected UnsupportedBackend for CxxClassMacro, got {err:?}"
        );
    }
}
