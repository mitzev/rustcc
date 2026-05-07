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

use crate::annotations::{Annotation, AnnotationSet};
use crate::macros::{MacroSet, MacroValue};
use crate::name_mapping::{
    disambiguate_overloads, rust_name_for_operator, OverloadEntry,
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
    /// Emit M14 heap-allocation thunks: `pub fn new_boxed(...) ->
    /// ::cxx::CxxHeap<Self>` wrappers + an `unsafe impl
    /// ::cxx::CxxDeletable for <class>` per class with a ctor.
    /// Off by default because the generated source references the
    /// [`::cxx`] runtime crate, which the bare `include!`-style
    /// test path doesn't link in. Downstream users who depend on
    /// `cxx` can opt in for heap-rooted widget support
    /// (FLTK-style). Pairs with `Driver::emit_shims` — the C++
    /// side always emits the `__cxx_<class>_new_heap_<i>` and
    /// `__cxx_<class>_delete` thunks regardless, so users can
    /// also implement their own heap wrapper if they prefer.
    pub emit_heap_alloc: bool,
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
///
/// This entry point does not consult any annotations. To honor
/// `[[clang::annotate("rustcc::name=Foo")]]` overrides imported via
/// [`super::import::import_header_with_annotations`], use
/// [`generate_rust_bindings_with_annotations`] instead.
pub fn generate_rust_bindings(
    ctx: &CxxTypeCtx,
    classes: &[ClassId],
    config: &RustBindingsConfig,
) -> Result<String, BindingsError> {
    let empty = AnnotationSet::default();
    generate_rust_bindings_with_annotations(ctx, classes, &empty, config)
}

/// Emit Rust source consulting both `annotations` for per-entity
/// name overrides and `macros` (M12) for `#define` constants
/// captured by `cxx_importer::macros::collect_macros`. Each macro
/// entry becomes a `pub const NAME: T = VALUE;` at the top of the
/// generated module.
pub fn generate_rust_bindings_with_macros(
    ctx: &CxxTypeCtx,
    classes: &[ClassId],
    annotations: &AnnotationSet,
    macros: &MacroSet,
    config: &RustBindingsConfig,
) -> Result<String, BindingsError> {
    let mut out = generate_rust_bindings_with_annotations(
        ctx,
        classes,
        annotations,
        config,
    )?;
    if !macros.entries.is_empty() {
        let mut header = String::new();
        let _ = writeln!(
            header,
            "// M12: `#define` constants captured by `cxx_importer::macros::collect_macros`.",
        );
        for m in &macros.entries {
            let line = render_macro_const(m);
            header.push_str(&line);
        }
        header.push('\n');
        // Insert after the existing emitter's leading comment.
        // Splitting on the first blank line keeps both blocks
        // visually distinct.
        if let Some(idx) = out.find("\n\n") {
            out.insert_str(idx + 2, &header);
        } else {
            out.insert_str(0, &header);
        }
    }
    Ok(out)
}

fn render_macro_const(m: &crate::macros::MacroConst) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    match &m.value {
        MacroValue::SignedInteger(v) => {
            let _ = writeln!(s, "pub const {}: i64 = {v};", m.name);
        }
        MacroValue::UnsignedInteger(v) => {
            let _ = writeln!(s, "pub const {}: u64 = {v};", m.name);
        }
        MacroValue::Float(v) => {
            let _ = writeln!(s, "pub const {}: f64 = {v};", m.name);
        }
        MacroValue::String(v) => {
            let escaped = v.replace('\\', "\\\\").replace('"', "\\\"");
            let _ = writeln!(s, "pub const {}: &str = \"{escaped}\";", m.name);
        }
        MacroValue::Bool(v) => {
            let _ = writeln!(s, "pub const {}: bool = {v};", m.name);
        }
    }
    s
}

/// Emit Rust source consulting `annotations` for per-entity Rust-name
/// overrides. Currently honors:
///
/// - [`Annotation::Name`] on a class FQN → overrides the emitted
///   Rust struct identifier.
/// - [`Annotation::Name`] on a method FQN (`<class FQN>::<method>`) →
///   overrides the emitted Rust wrapper name (still routed through
///   the disambiguator, so collisions with sibling methods are
///   resolved deterministically).
/// - [`Annotation::Skip`] on a class FQN → omits the class from
///   the emission entirely.
///
/// Other annotation kinds (`Nullable`, `LifetimeBound`, etc.) are
/// recognized at parse time but not yet wired into the emission —
/// tracked per `docs/cxx_importer.md §5`.
pub fn generate_rust_bindings_with_annotations(
    ctx: &CxxTypeCtx,
    classes: &[ClassId],
    annotations: &AnnotationSet,
    config: &RustBindingsConfig,
) -> Result<String, BindingsError> {
    let empty_aliases = crate::aliases::AliasSet::default();
    let empty_enums = crate::enums::EnumSet::default();
    generate_rust_bindings_with_extras(
        ctx,
        classes,
        annotations,
        &empty_aliases,
        &empty_enums,
        config,
    )
}

/// Emit Rust source consulting `annotations` (M6), the M17 alias
/// side-table, and the M16 enum-body side-table. Each alias
/// becomes a `pub type {name} = {target};` line scoped under the
/// matching `pub mod`; each enum becomes either a `#[repr(int)]
/// pub enum` (scoped + unique discriminants) or a
/// `#[repr(transparent)] pub struct + assoc consts` (unscoped or
/// aliasing) at the same scope.
///
/// Aliases whose target type is not yet supported by the v0 type
/// renderer are dropped with a `// alias ... skipped` comment
/// rather than failing the emission. Enums whose underlying type
/// isn't a simple integer fall through the same way.
///
/// Currently DirectExternCpp is the only backend that honors
/// aliases / enums — the macro-based backends
/// (`NativeCppClassMacro`, `CxxClassMacro`) flatten everything to
/// the top level and would need their grammar widened to expose
/// `type` / `enum` items. Tracked for a follow-up.
pub fn generate_rust_bindings_with_extras(
    ctx: &CxxTypeCtx,
    classes: &[ClassId],
    annotations: &AnnotationSet,
    aliases: &crate::aliases::AliasSet,
    enums: &crate::enums::EnumSet,
    config: &RustBindingsConfig,
) -> Result<String, BindingsError> {
    match config.backend {
        BindingsBackend::NativeCppClassMacro => emit_native_macro(ctx, classes, config),
        BindingsBackend::DirectExternCpp => emit_direct_extern_cpp(
            ctx,
            classes,
            annotations,
            &aliases.entries,
            &enums.entries,
            config,
        ),
        BindingsBackend::CxxClassMacro => emit_cxx_class_macro(ctx, classes, config),
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
        let block = render_class_block(ctx, class_id, config, indent, MacroPath::Native)?;
        out.push_str(&block);
        out.push('\n');
    }

    if config.crate_module.is_some() {
        let _ = writeln!(out, "}}");
    }
    Ok(out)
}

/// Selects which workspace macro the class-block emission writes
/// against. Both macros share the same input grammar; the only
/// difference is the macro path token at the head of the block,
/// which means a single class-block renderer can drive both.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MacroPath {
    /// `::rustcc_macros::native_cpp_class!` — fork-only, expands
    /// to `extern "C++"` + `#[constructor]` + `#[repr(cpp)]`.
    Native,
    /// `::rustcc_macros::cxx_class!` — stable-rustc-friendly,
    /// expands to `extern "C"` + manual `__sret` trampolines +
    /// `#[repr(C, align(N))]`.
    Stable,
}

impl MacroPath {
    fn token(self) -> &'static str {
        match self {
            MacroPath::Native => "::rustcc_macros::native_cpp_class!",
            MacroPath::Stable => "::rustcc_macros::cxx_class!",
        }
    }

    fn comment_label(self) -> &'static str {
        match self {
            MacroPath::Native => "NativeCppClassMacro (fork-only).",
            MacroPath::Stable => "CxxClassMacro (stable-rustc-friendly).",
        }
    }
}

/// Stable-rustc-friendly macro emission. Writes one
/// `::rustcc_macros::cxx_class! { … }` invocation per imported
/// class. The macro itself expands to the pre-fork shape
/// (`extern "C"` + manual `__sret` trampolines + `#[repr(C,
/// align(N))]`), so the emitted source compiles on plain nightly
/// without the rustcc fork.
///
/// Identical class-block grammar to `NativeCppClassMacro` (size,
/// align, ctor / dtor / instance / static methods); the only
/// difference at our layer is the macro path.
fn emit_cxx_class_macro(
    ctx: &CxxTypeCtx,
    classes: &[ClassId],
    config: &RustBindingsConfig,
) -> Result<String, BindingsError> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// Generated by rustcc cxx_importer::rust_bindings. Do not hand-edit.\n\
         // Backend: CxxClassMacro (stable-rustc-friendly).\n"
    );

    let indent = if config.crate_module.is_some() { "    " } else { "" };
    if let Some(modname) = config.crate_module.as_deref() {
        let _ = writeln!(out, "pub mod {modname} {{");
    }

    for &class_id in classes {
        let block = render_class_block(ctx, class_id, config, indent, MacroPath::Stable)?;
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
    annotations: &AnnotationSet,
    aliases: &[crate::aliases::TypeAlias],
    enums: &[crate::enums::CxxEnumDef],
    config: &RustBindingsConfig,
) -> Result<String, BindingsError> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// Generated by rustcc cxx_importer::rust_bindings. Do not hand-edit.\n\
         // Backend: DirectExternCpp (fork-required for `extern \"C++\"`).\n"
    );

    // Filter classes annotated with `Skip` before tree building.
    let classes: Vec<ClassId> = classes
        .iter()
        .copied()
        .filter(|&cid| {
            !annotations
                .effective(&class_fqn_string(ctx, cid))
                .iter()
                .any(|a| matches!(a, Annotation::Skip))
        })
        .collect();

    let initial_indent = if config.crate_module.is_some() { "    " } else { "" };
    if let Some(modname) = config.crate_module.as_deref() {
        let _ = writeln!(out, "pub mod {modname} {{");
    }

    // Group classes by their `NestedName` namespace prefix so the
    // emitter recovers the C++ scope structure as a Rust `mod`
    // tree. A flat list (no `Namespace` segments) collapses to the
    // root and emits at the top level — same shape the v0 emitter
    // produced before this change, with no namespace overhead.
    // M17 aliases + M16 enum bodies get folded into the same tree
    // at their owning namespace nodes so they emit before the
    // class blocks at that scope.
    let tree = build_namespace_tree_with_extras(ctx, &classes, aliases, enums)?;
    render_namespace_tree(ctx, &tree, &mut out, annotations, config, initial_indent)?;

    if config.crate_module.is_some() {
        let _ = writeln!(out, "}}");
    }
    Ok(out)
}

/// Build the `::`-joined fully-qualified C++ name for a class.
/// Mirrors what the libclang-side `entity_fqn` builds during
/// import. Used as the lookup key for class- and method-level
/// annotations.
fn class_fqn_string(ctx: &CxxTypeCtx, class_id: ClassId) -> String {
    let class = ctx.class(class_id);
    let mut parts = Vec::with_capacity(class.name.0.len());
    for seg in &class.name.0 {
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

/// Tree of imported classes grouped by their C++ namespace prefix.
/// A class with `NestedName = [Namespace("ns"), Class("Foo")]` lands
/// at `root.children["ns"].classes` containing its `ClassId`.
#[derive(Default)]
struct NamespaceTree {
    /// Classes directly inside this scope.
    classes: Vec<ClassId>,
    /// M17 aliases (`typedef` / `using`) directly inside this
    /// scope. Stored as `(rust_ident, target_typeid)`. Emission
    /// renders them as `pub type {rust_ident} = {render(target)};`
    /// before the class blocks at the same node.
    aliases: Vec<(String, rustc_abi_cxx::TypeId)>,
    /// M16 enum bodies (`enum class Foo { ... }`) directly inside
    /// this scope. Emission renders each as either
    /// `#[repr(int)] pub enum` (scoped, unique discriminants) or
    /// `#[repr(transparent)] pub struct + assoc consts` (unscoped
    /// or aliasing variants).
    enums: Vec<crate::enums::CxxEnumDef>,
    /// Sub-namespaces at this scope, keyed by name.
    /// `BTreeMap` for deterministic emission order.
    children: BTreeMap<String, NamespaceTree>,
}

fn build_namespace_tree(
    ctx: &CxxTypeCtx,
    classes: &[ClassId],
) -> Result<NamespaceTree, BindingsError> {
    build_namespace_tree_with_extras(ctx, classes, &[], &[])
}

/// Same as [`build_namespace_tree`], but also folds M17 aliases
/// and M16 enum bodies into their owning namespace scopes. Each
/// alias / enum gets keyed by its `parent` segment list using
/// the same `Namespace` / `AnonymousNamespace` rules as classes.
fn build_namespace_tree_with_extras(
    ctx: &CxxTypeCtx,
    classes: &[ClassId],
    aliases: &[crate::aliases::TypeAlias],
    enums: &[crate::enums::CxxEnumDef],
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
    // M17: drop aliases under their owning namespace node. Anything
    // referencing a non-namespace prefix segment is silently
    // skipped — emitter ergonomics, not correctness.
    for alias in aliases {
        let mut node = &mut root;
        let mut prefix_ok = true;
        for seg in &alias.parent {
            let key = match seg {
                NameSegment::Namespace(id) => id.0.clone(),
                NameSegment::AnonymousNamespace => "__anon".to_string(),
                _ => {
                    prefix_ok = false;
                    break;
                }
            };
            node = node.children.entry(key).or_default();
        }
        if !prefix_ok {
            continue;
        }
        node.aliases.push((alias.name.0.clone(), alias.target));
    }
    // M16: same routing for enum bodies.
    for enum_def in enums {
        let mut node = &mut root;
        let mut prefix_ok = true;
        for seg in &enum_def.parent {
            let key = match seg {
                NameSegment::Namespace(id) => id.0.clone(),
                NameSegment::AnonymousNamespace => "__anon".to_string(),
                _ => {
                    prefix_ok = false;
                    break;
                }
            };
            node = node.children.entry(key).or_default();
        }
        if !prefix_ok {
            continue;
        }
        node.enums.push(enum_def.clone());
    }
    Ok(root)
}

fn render_namespace_tree(
    ctx: &CxxTypeCtx,
    tree: &NamespaceTree,
    out: &mut String,
    annotations: &AnnotationSet,
    config: &RustBindingsConfig,
    indent: &str,
) -> Result<(), BindingsError> {
    // M16: emit imported enum bodies first — classes and aliases
    // at this scope may reference them by name in their fields /
    // method signatures.
    for enum_def in &tree.enums {
        match render_cxx_enum(ctx, enum_def, indent) {
            Ok(block) => {
                out.push_str(&block);
                out.push('\n');
            }
            Err(BindingsError::UnsupportedType { kind, .. }) => {
                // Underlying integer width not supported (i128 /
                // u128 in some emitter passes), or other v0 gap.
                // Drop with a comment — not fatal.
                let _ = writeln!(
                    out,
                    "{indent}// enum `{}` skipped: {kind}",
                    enum_def.name.0,
                );
            }
            Err(other) => return Err(other),
        }
    }
    // M17: emit `pub type` aliases ahead of class blocks. Putting
    // them after enums so an alias of an enum lands legally; before
    // class blocks so the classes (and user code) see the names.
    // Render failures (target type unsupported by `render_rust_type`)
    // drop the alias rather than aborting: aliases are emit-only
    // ergonomics, not correctness.
    for (alias_name, target) in &tree.aliases {
        let where_ = format!("alias `{alias_name}`");
        match render_rust_type(ctx, *target, &where_) {
            Ok(rendered) => {
                let _ = writeln!(out, "{indent}pub type {alias_name} = {rendered};");
            }
            Err(_) => {
                // Skip silently — surfacing the alias name as a
                // doc comment lets users know it existed without
                // breaking compilation.
                let _ = writeln!(
                    out,
                    "{indent}// alias `{alias_name}` skipped: target type unsupported in v0",
                );
            }
        }
    }
    if !tree.aliases.is_empty() || !tree.enums.is_empty() {
        out.push('\n');
    }
    for &class_id in &tree.classes {
        let block =
            render_direct_extern_class(ctx, class_id, annotations, config, indent)?;
        out.push_str(&block);
        out.push('\n');
    }
    for (name, child) in &tree.children {
        let _ = writeln!(out, "{indent}pub mod {name} {{");
        let inner_indent = format!("{indent}    ");
        render_namespace_tree(ctx, child, out, annotations, config, &inner_indent)?;
        let _ = writeln!(out, "{indent}}}");
    }
    Ok(())
}

fn render_direct_extern_class(
    ctx: &CxxTypeCtx,
    class_id: ClassId,
    annotations: &AnnotationSet,
    config: &RustBindingsConfig,
    indent: &str,
) -> Result<String, BindingsError> {
    let class = ctx.class(class_id);
    let class_fqn = class_fqn_string(ctx, class_id);
    // Inline / sidecar annotations override the Rust class
    // identifier when `[[clang::annotate("rustcc::name=NewName")]]`
    // is present on the C++ class. Falls back to the source
    // identifier otherwise.
    let class_name = annotations
        .effective(&class_fqn)
        .into_iter()
        .find_map(|a| match a {
            Annotation::Name(n) => Some(n),
            _ => None,
        })
        .or_else(|| ident_of_class(class))
        .ok_or_else(|| BindingsError::UnsupportedType {
            where_: "class name".into(),
            kind: "anonymous or non-identifier-named class".into(),
        })?;

    // Poison nodes — minted by the importer for classes whose
    // lowering failed recoverably (forward-only declarations,
    // unsupported features in subordinate decls, etc.) — render
    // as opaque structs with a doc comment explaining the gap.
    // No methods, no extern block, no Drop impl. Users can name
    // the type and pass it through pointers; calling any method
    // produces a "no method named X" diagnostic at compile time.
    if let Some(reason) = ctx.poison_reason(class_id) {
        let mut block = String::new();
        for line in reason.lines() {
            let _ = writeln!(block, "{indent}/// {line}");
        }
        let _ = writeln!(
            block,
            "{indent}/// (Class poisoned by `cxx_importer`; method bodies omitted.)",
        );
        if config.doc_hidden {
            let _ = writeln!(block, "{indent}#[doc(hidden)]");
        }
        let _ = writeln!(block, "{indent}#[repr(C)]");
        let _ = writeln!(
            block,
            "{indent}pub struct {class_name} {{ _opaque: [::core::mem::MaybeUninit<u8>; 0] }}",
        );
        return Ok(block);
    }

    let layout = ctx.layout(class_id).map_err(|e| BindingsError::LayoutFailed {
        class: class_name.clone(),
        detail: format!("{e:?}"),
    })?;

    // Collect methods up front — we need them for the extern block,
    // the impl block, and the Drop check. We accept virtuals here
    // (their `vtable_index` was populated by the importer's
    // post-pass); the wrapper renderer routes them through a
    // vtable-lookup path. Pure virtuals stay rejected for v0
    // because they don't have an own-class implementation to call.
    let methods = ctx.class(class_id).methods.clone();
    for m in &methods {
        if m.virtuality == Virtuality::Virtual && m.vtable_index.is_none() {
            return Err(BindingsError::UnsupportedMethod {
                where_: format!("{class_name}::{:?}", m.name),
                why: "virtual method without populated vtable_index — the importer's \
                      vtable post-pass didn't recognize this slot. Likely a class \
                      structure (multi-inheritance, virtual bases) the v0 vtable \
                      walker doesn't yet handle."
                    .into(),
            });
        }
        if m.virtuality == Virtuality::PureVirtual {
            return Err(BindingsError::UnsupportedMethod {
                where_: format!("{class_name}::{:?}", m.name),
                why: "pure virtual method (no own-class implementation to call) — \
                      a future revision can either skip it or route to an \
                      `__cxa_pure_virtual` shim."
                    .into(),
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

    // Pre-compute the disambiguated Rust name for each method.
    // Ctors and dtors are special-cased to `new` / `drop`; operators
    // route through `rust_name_for_operator`; identifier-named
    // methods keep their source name, with collisions resolved by
    // appending a stringified parameter-type signature.
    let resolved_names = resolve_method_names(
        ctx,
        &methods,
        &class_name,
        &class_fqn,
        annotations,
    )?;

    let mut ctor_seen = 0usize;
    let mut method_blocks: Vec<MethodEmission> = Vec::with_capacity(methods.len());
    let mut has_user_dtor = false;
    for (method_idx, (method, resolved_name)) in
        methods.iter().zip(resolved_names.iter()).enumerate()
    {
        let emission = classify_for_direct_extern(
            ctx,
            class_id,
            &class_name,
            method,
            method_idx,
            resolved_name,
        )?;
        if matches!(emission.kind, EmissionKind::Dtor) {
            has_user_dtor = true;
        }
        if matches!(emission.kind, EmissionKind::Ctor) {
            ctor_seen += 1;
        }
        // Virtual methods don't get a `#[link_name]` extern decl —
        // they're dispatched via the vtable at the call site, not
        // by linker resolution. The wrapper does its own vptr load
        // and transmute.
        if !matches!(emission.kind, EmissionKind::Virtual { .. }) {
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
        }
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

    // 4.b: M19 — `CxxBase<Base>` upcast impls for every non-virtual
    // base. Virtual bases are deferred to M22 (their offset is
    // dynamic via the vtable; the emission shape is different).
    // Poisoned bases are skipped — there's no Rust type to refer
    // to. Multiple non-virtual bases each get their own impl.
    if let Ok(layout) = ctx.layout(class_id) {
        for base_spec in &class.bases {
            if base_spec.virtual_ {
                continue;
            }
            if ctx.is_poisoned(base_spec.class) {
                continue;
            }
            let base = ctx.class(base_spec.class);
            let base_name = match ident_of_class(base) {
                Some(n) => n,
                None => continue,
            };
            // Find the offset libclang/Itanium computed for this
            // base. base_offsets is keyed by ClassId so we can
            // index directly.
            let offset = layout
                .base_offsets
                .iter()
                .find_map(|(bid, off)| (*bid == base_spec.class).then_some(*off));
            let offset = match offset {
                Some(o) => o,
                None => continue,
            };
            let _ = writeln!(block);
            let _ = writeln!(
                block,
                "{indent}// M19: derived-to-base upcast (non-virtual, offset = {offset}).",
            );
            let _ = writeln!(
                block,
                "{indent}impl ::cxx::CxxBase<{base_name}> for {class_name} {{",
            );
            // For zero-offset bases (the common single-inheritance
            // case) elide the `add(0)` for readability. The
            // semantics are identical.
            if offset == 0 {
                let _ = writeln!(
                    block,
                    "{indent}    fn upcast(&self) -> &{base_name} {{",
                );
                let _ = writeln!(
                    block,
                    "{indent}        // SAFETY: primary base subobject sits at offset 0",
                );
                let _ = writeln!(
                    block,
                    "{indent}        //   per Itanium ABI; the cast is purely a type adjustment.",
                );
                let _ = writeln!(
                    block,
                    "{indent}        unsafe {{ &*(self as *const Self as *const {base_name}) }}",
                );
                let _ = writeln!(block, "{indent}    }}");
                let _ = writeln!(
                    block,
                    "{indent}    fn upcast_mut(&mut self) -> &mut {base_name} {{",
                );
                let _ = writeln!(
                    block,
                    "{indent}        // SAFETY: same reasoning as upcast().",
                );
                let _ = writeln!(
                    block,
                    "{indent}        unsafe {{ &mut *(self as *mut Self as *mut {base_name}) }}",
                );
                let _ = writeln!(block, "{indent}    }}");
            } else {
                let _ = writeln!(
                    block,
                    "{indent}    fn upcast(&self) -> &{base_name} {{",
                );
                let _ = writeln!(
                    block,
                    "{indent}        // SAFETY: base subobject offset pinned by Itanium layout.",
                );
                let _ = writeln!(
                    block,
                    "{indent}        unsafe {{",
                );
                let _ = writeln!(
                    block,
                    "{indent}            let p = (self as *const Self as *const u8).add({offset});",
                );
                let _ = writeln!(
                    block,
                    "{indent}            &*(p as *const {base_name})",
                );
                let _ = writeln!(block, "{indent}        }}");
                let _ = writeln!(block, "{indent}    }}");
                let _ = writeln!(
                    block,
                    "{indent}    fn upcast_mut(&mut self) -> &mut {base_name} {{",
                );
                let _ = writeln!(
                    block,
                    "{indent}        // SAFETY: same reasoning as upcast().",
                );
                let _ = writeln!(
                    block,
                    "{indent}        unsafe {{",
                );
                let _ = writeln!(
                    block,
                    "{indent}            let p = (self as *mut Self as *mut u8).add({offset});",
                );
                let _ = writeln!(
                    block,
                    "{indent}            &mut *(p as *mut {base_name})",
                );
                let _ = writeln!(block, "{indent}        }}");
                let _ = writeln!(block, "{indent}    }}");
            }
            let _ = writeln!(block, "{indent}}}");
        }
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

    // M14: heap-allocation thunks. For each ctor, emit:
    //   - An `unsafe extern "C"` decl for the matching
    //     `__cxx_<class>_new_heap_<i>` shim from `shims.rs`.
    //   - A `pub fn new_boxed[_<i>](args) -> ::cxx::CxxHeap<Self>`
    //     wrapper that calls the heap shim and wraps the returned
    //     pointer in `CxxHeap`.
    //
    // Plus, once per class:
    //   - An `unsafe extern "C"` decl for `__cxx_<class>_delete`.
    //   - An `unsafe impl ::cxx::CxxDeletable for <class>` that
    //     routes `Drop` for `CxxHeap<Self>` through the C++
    //     `delete` shim.
    //
    // Skipped when `config.emit_heap_alloc` is `false` (default)
    // because the emission references `::cxx`. Skipped also for
    // poisoned classes (no constructable Rust analog).
    let ctor_emissions: Vec<(usize, &MethodEmission)> = method_blocks
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e.kind, EmissionKind::Ctor))
        .collect();
    if config.emit_heap_alloc && !ctor_emissions.is_empty() {
        let _ = writeln!(block);
        let _ = writeln!(
            block,
            "{indent}// M14: heap-allocation thunks paired with `__cxx_<class>_new_heap_<i>`",
        );
        let _ = writeln!(
            block,
            "{indent}// and `__cxx_<class>_delete` shims emitted by `cxx_importer::shims`.",
        );
        let _ = writeln!(block, "{indent}unsafe extern \"C\" {{");
        for (ctor_idx, _emission) in ctor_emissions.iter().enumerate() {
            let (_, e) = _emission;
            // Reuse the existing extern_decl_params (which start
            // with `this: *mut <class>`) but we need a different
            // shape for the heap shim — drop the `this` slot, use
            // the user-arg list only.
            let user_args = strip_first_param(&e.extern_decl_params);
            let _ = writeln!(
                block,
                "{indent}    fn __cxx_{class_name}_new_heap_{ctor_idx}({user_args}) -> *mut {class_name};",
            );
        }
        let _ = writeln!(
            block,
            "{indent}    fn __cxx_{class_name}_delete(p: *mut {class_name});",
        );
        let _ = writeln!(block, "{indent}}}");

        // Heap-allocating wrappers and the CxxDeletable impl. Add
        // to the existing impl block by re-opening it briefly —
        // but the impl block was closed earlier, so emit a fresh
        // `impl Foo` block for the heap wrappers only.
        let _ = writeln!(block);
        let _ = writeln!(block, "{indent}impl {class_name} {{");
        for (ctor_idx, _emission) in ctor_emissions.iter().enumerate() {
            let (_, e) = _emission;
            let suffix = if ctor_emissions.len() > 1 {
                format!("_{ctor_idx}")
            } else {
                String::new()
            };
            let _ = writeln!(
                block,
                "{indent}    /// Heap-allocate via C++ `new {class_name}(...)`.",
            );
            let _ = writeln!(
                block,
                "{indent}    /// Returned [`::cxx::CxxHeap`] frees with C++ `delete` on drop,",
            );
            let _ = writeln!(
                block,
                "{indent}    /// keeping the new/delete pair the C++ ABI requires.",
            );
            let header = if e.wrapper_params.is_empty() {
                format!(
                    "{indent}    pub fn new_boxed{suffix}() -> ::cxx::CxxHeap<Self> {{",
                )
            } else {
                format!(
                    "{indent}    pub fn new_boxed{suffix}({params}) -> ::cxx::CxxHeap<Self> {{",
                    params = e.wrapper_params,
                )
            };
            let _ = writeln!(block, "{header}");
            if e.forward_args.is_empty() {
                let _ = writeln!(
                    block,
                    "{indent}        unsafe {{ ::cxx::CxxHeap::from_raw(__cxx_{class_name}_new_heap_{ctor_idx}()) }}",
                );
            } else {
                let _ = writeln!(
                    block,
                    "{indent}        unsafe {{ ::cxx::CxxHeap::from_raw(__cxx_{class_name}_new_heap_{ctor_idx}({fwd})) }}",
                    fwd = e.forward_args,
                );
            }
            let _ = writeln!(block, "{indent}    }}");
        }
        let _ = writeln!(block, "{indent}}}");

        let _ = writeln!(block);
        let _ = writeln!(
            block,
            "{indent}unsafe impl ::cxx::CxxDeletable for {class_name} {{",
        );
        let _ = writeln!(
            block,
            "{indent}    unsafe fn cxx_delete(p: *mut Self) {{",
        );
        let _ = writeln!(
            block,
            "{indent}        unsafe {{ __cxx_{class_name}_delete(p); }}",
        );
        let _ = writeln!(block, "{indent}    }}");
        let _ = writeln!(block, "{indent}}}");
    }

    Ok(block)
}

/// Strip the first parameter from a comma-joined extern-decl
/// parameter list (e.g. `"this: *mut Foo, arg0: i32"` →
/// `"arg0: i32"`). Used to reshape a ctor's normal extern params
/// for the heap-allocation shim, which doesn't take a `this`
/// slot — C++ `new` synthesizes the storage internally.
fn strip_first_param(params: &str) -> String {
    match params.find(',') {
        Some(comma) => params[comma + 1..].trim_start().to_string(),
        None => String::new(),
    }
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
    /// M18: trailing-default-arg count from `ctx.default_arg_count`.
    /// `0` when none; the renderer surfaces a `///` doc comment
    /// listing how many trailing parameters were optional in the
    /// C++ source so callers know which `Default::default()` /
    /// `core::ptr::null()` placeholders match the original
    /// signature.
    default_arg_count: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EmissionKind {
    Ctor,
    Dtor,
    /// `&self` / `&mut self` instance method, dispatched directly
    /// to the Itanium-mangled symbol.
    Instance,
    /// No-self static method.
    Static,
    /// `&self` / `&mut self` virtual method, dispatched through
    /// the C++ vtable. `vtable_index` is the rank of this method
    /// among the primary sub-table's function-pointer slots.
    Virtual { vtable_index: u32 },
}

/// Compute one disambiguated Rust identifier per method in `methods`,
/// in the same order. Handles three flavors:
///
/// - Ctor → `"new"` (every ctor in v0; multi-ctor support gets
///   suffixes via the disambiguator pass).
/// - Dtor → `"drop"`.
/// - Operator → `op_<word>` / `op_<word>_mut` per
///   [`rust_name_for_operator`].
/// - Plain identifier → the source name.
///
/// After base-name selection, collisions get
/// `<base>_<arg-type-signature>` suffixes via
/// [`disambiguate_overloads`]. The disambiguator string is the
/// param-type list joined by `_`, sanitized into a valid Rust
/// identifier suffix.
fn resolve_method_names(
    ctx: &CxxTypeCtx,
    methods: &[MethodDef],
    class_name: &str,
    class_fqn: &str,
    annotations: &AnnotationSet,
) -> Result<Vec<String>, BindingsError> {
    // First pass: base name + disambiguator string per method.
    // Per-method `Annotation::Name` overrides win over the
    // operator-table / source-name defaults, but the disambiguator
    // pass still runs on top to resolve any user-introduced
    // collisions.
    let mut entries: Vec<(String, String)> = Vec::with_capacity(methods.len());
    for method in methods {
        let default_base = base_rust_name_for_method(method, class_name)?;
        let method_fqn = format!(
            "{class_fqn}::{}",
            method_source_name_for_fqn(method).unwrap_or_else(|| default_base.clone()),
        );
        let base = annotations
            .effective(&method_fqn)
            .into_iter()
            .find_map(|a| match a {
                Annotation::Name(n) => Some(n),
                _ => None,
            })
            .unwrap_or(default_base);
        let disamb = stringify_param_signature(ctx, method, class_name)?;
        entries.push((base, disamb));
    }

    // Second pass: feed into the disambiguator.
    let entries_view: Vec<OverloadEntry<&str>> = entries
        .iter()
        .map(|(b, d)| OverloadEntry {
            base_name: b.as_str(),
            disambiguator: d.as_str(),
        })
        .collect();
    let resolved = disambiguate_overloads(entries_view);
    Ok(resolved.into_iter().map(|i| i.0).collect())
}

/// The source-level C++ identifier for a method, used as the
/// trailing component of the annotation-lookup FQN.
///
/// v0 only exposes annotation lookups for plain identifier-named
/// methods. Special members (ctors, dtors) and operator overloads
/// rarely benefit from `Annotation::Name` overrides — the rename
/// machinery for those is tracked separately.
fn method_source_name_for_fqn(method: &MethodDef) -> Option<String> {
    match (&method.special, &method.name) {
        (None, MethodName::Ident(id)) => Some(id.0.clone()),
        _ => None,
    }
}

fn base_rust_name_for_method(
    method: &MethodDef,
    class_name: &str,
) -> Result<String, BindingsError> {
    match &method.special {
        Some(SpecialMember::DefaultCtor | SpecialMember::OtherCtor) => Ok("new".into()),
        Some(SpecialMember::Dtor) => Ok("drop".into()),
        Some(other @ (SpecialMember::CopyCtor
        | SpecialMember::MoveCtor
        | SpecialMember::CopyAssign
        | SpecialMember::MoveAssign)) => Err(BindingsError::UnsupportedMethod {
            where_: format!("{class_name}::{:?}", method.name),
            why: format!(
                "special member `{other:?}` not yet wired (v0 covers Ctor + Dtor + \
                 plain instance/static methods + operators)"
            ),
        }),
        None => match &method.name {
            MethodName::Ident(id) => Ok(id.0.clone()),
            MethodName::Operator(op) => {
                Ok(rust_name_for_operator(*op, method.sig.cv.is_const))
            }
            MethodName::ConversionTo(_) => Err(BindingsError::UnsupportedMethod {
                where_: format!("{class_name}::<conversion>"),
                why: "C++ conversion functions (operator T()) deferred — needs \
                      separate target-type-aware lowering"
                    .into(),
            }),
        },
    }
}

/// Stringify a method's parameter type list as a stable token usable
/// as an overload-disambiguator suffix. Keeps just enough information
/// to differentiate signatures the C++ side considers distinct
/// overloads. Sanitization to a valid Rust identifier happens
/// downstream in [`name_mapping::sanitize_disambiguator`].
fn stringify_param_signature(
    ctx: &CxxTypeCtx,
    method: &MethodDef,
    class_name: &str,
) -> Result<String, BindingsError> {
    if method.sig.params.is_empty() {
        return Ok(String::new());
    }
    let mut parts = Vec::with_capacity(method.sig.params.len());
    for (i, &ty_id) in method.sig.params.iter().enumerate() {
        let s = render_rust_type(
            ctx,
            ty_id,
            &format!("{class_name} overload sig param {i}"),
        )?;
        parts.push(s);
    }
    Ok(parts.join("_"))
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
    method_idx: usize,
    resolved_rust_name: &str,
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

    // Special-case ctor / dtor first. The resolved Rust name is
    // already `"new"` / `"drop"` for these (assigned by
    // `resolve_method_names`), but we route through the same code
    // path so disambiguator suffixes (e.g. multiple ctors becoming
    // `new_int_int` / `new_double`) get propagated into the extern
    // ident too.
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
                rust_name: resolved_rust_name.to_string(),
                extern_ident: format!("__cxx_{class_name}_{resolved_rust_name}"),
                link_name: link,
                extern_decl_params: decl.join(", "),
                extern_return_clause: String::new(),
                wrapper_receiver: WrapperReceiver::Ctor,
                wrapper_params: user_arg_decls.join(", "),
                wrapper_return: "Self".into(),
                forward_args: user_forward.join(", "),
                default_arg_count: ctx.default_arg_count(class_id, method_idx),
            });
        }
        Some(SpecialMember::Dtor) => {
            let link = ctx.mangle(&Symbol::Dtor {
                class: class_id,
                variant: DtorVariant::D1,
            });
            return Ok(MethodEmission {
                kind: EmissionKind::Dtor,
                rust_name: resolved_rust_name.to_string(),
                extern_ident: format!("__cxx_{class_name}_dtor"),
                link_name: link,
                extern_decl_params: format!("this: *mut {class_name}"),
                extern_return_clause: String::new(),
                wrapper_receiver: WrapperReceiver::SelfMut,
                wrapper_params: String::new(),
                wrapper_return: "()".into(),
                forward_args: String::new(),
                default_arg_count: 0,
            });
        }
        Some(SpecialMember::CopyCtor | SpecialMember::MoveCtor)
        | Some(SpecialMember::CopyAssign | SpecialMember::MoveAssign) => {
            return Err(BindingsError::UnsupportedMethod {
                where_: format!("{class_name}::{:?}", method.name),
                why: "copy/move special members not yet wired (v0 covers \
                      DefaultCtor + OtherCtor + Dtor + plain methods + operators)"
                    .into(),
            });
        }
        None => {}
    }

    // Identifier-named or operator-named non-special method.
    // `resolved_rust_name` already encodes the operator → `op_<word>`
    // mapping and any disambiguator suffix; we just need the
    // *Itanium-mangling-friendly* original name token to feed to the
    // mangler. For operators we hand the raw `MethodName::Operator`
    // through; for plain identifiers we use the source string.
    let method_name = resolved_rust_name.to_string();
    let mangler_method_name: MethodName = match &method.name {
        MethodName::Ident(id) => MethodName::Ident(id.clone()),
        MethodName::Operator(op) => MethodName::Operator(*op),
        MethodName::ConversionTo(_) => {
            return Err(BindingsError::UnsupportedMethod {
                where_: format!("{class_name}::<conversion>"),
                why: "C++ conversion functions (operator T()) deferred".into(),
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

    // Distinguish instance / static / virtual. M11 wires
    // `ctx.is_method_static` for the Static path; virtuals carry
    // their `vtable_index` for the vptr-load-and-transmute path.
    // Everything else falls through to Instance.
    let kind = if method.virtuality == Virtuality::Virtual {
        match method.vtable_index {
            Some(vt) => EmissionKind::Virtual { vtable_index: vt },
            None => {
                return Err(BindingsError::UnsupportedMethod {
                    where_: format!("{class_name}::{method_name}"),
                    why: "virtual method missing vtable_index — populate_vtable_indices \
                          should have set it; this is an internal invariant break"
                        .into(),
                });
            }
        }
    } else if ctx.is_method_static(class_id, method_idx) {
        EmissionKind::Static
    } else {
        EmissionKind::Instance
    };
    let receiver = if method.sig.cv.is_const {
        WrapperReceiver::SelfConst
    } else {
        WrapperReceiver::SelfMut
    };
    // M11: static methods don't take a `this` slot. Emit just the
    // user-arg list; the wrapper will drop the receiver too.
    let mut extern_decl = if matches!(kind, EmissionKind::Static) {
        Vec::new()
    } else {
        let this_ty = match receiver {
            WrapperReceiver::SelfConst => format!("this: *const {class_name}"),
            _ => format!("this: *mut {class_name}"),
        };
        vec![this_ty]
    };
    extern_decl.extend(user_arg_decls.clone());

    let final_receiver = if matches!(kind, EmissionKind::Static) {
        WrapperReceiver::None
    } else {
        receiver
    };

    // Mangle using the *original* C++ method name (operator code or
    // identifier) so the symbol matches what Clang produced for the
    // C++ object. The Rust-side identifier (`resolved_rust_name`)
    // is what the wrapper exposes to callers; the link_name is
    // separate and follows the C++ side verbatim.
    let link = ctx.mangle(&Symbol::Method {
        class: class_id,
        name: mangler_method_name,
        sig: method.sig.clone(),
    });

    Ok(MethodEmission {
        kind,
        rust_name: method_name.clone(),
        extern_ident: format!("__cxx_{class_name}_{method_name}"),
        link_name: link,
        extern_decl_params: extern_decl.join(", "),
        extern_return_clause: extern_ret_clause,
        wrapper_receiver: final_receiver,
        wrapper_params: user_arg_decls.join(", "),
        wrapper_return: ret_rust,
        forward_args: user_forward.join(", "),
        default_arg_count: ctx.default_arg_count(class_id, method_idx),
    })
}

/// Convert a wrapper-style parameter list (`arg0: i32, arg1: f64`)
/// into a fn-pointer-style type list (`i32, f64`). Used to build
/// the `unsafe extern "C++" fn(...)` type for vtable-lookup
/// transmutes.
fn strip_arg_names(wrapper_params: &str) -> String {
    wrapper_params
        .split(',')
        .map(|p| p.trim().split(": ").nth(1).unwrap_or("").trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
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
    // M18: surface trailing default-arg counts as a doc comment so
    // callers know which parameters were optional in the C++
    // source. v0 doesn't synthesize per-arity convenience wrappers
    // — that's tracked as M18.b — but the doc comment lets users
    // pick sensible placeholders (`core::ptr::null()`, `0`, …)
    // without going back to the headers.
    if emission.default_arg_count > 0 {
        let _ = writeln!(
            out,
            "{indent}/// In C++, the trailing {n} parameter{s} of this method \
             {has} default value{s} — pass any value of the right type when calling \
             from Rust. (M18 v0: the importer surfaces the count as a hint; per-arity \
             convenience wrappers are tracked as M18.b.)",
            n = emission.default_arg_count,
            s = if emission.default_arg_count == 1 { "" } else { "s" },
            has = if emission.default_arg_count == 1 { "has a" } else { "have" },
        );
    }
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
        EmissionKind::Virtual { vtable_index } => {
            // Vtable-lookup wrapper. Reads the vptr at offset 0 of
            // the primary subobject, indexes into the vtable, and
            // calls through a transmuted function pointer with the
            // right `extern "C++"` ABI so the fork's
            // `compute_cxx_abi_info` overlay still routes
            // record-by-value returns through the correct
            // indirect-result register.
            //
            // Layout assumptions (Itanium):
            //   - vptr lives at byte 0 of the most-derived object
            //     (no virtual bases preceding the primary).
            //   - The vptr in the object points at the vtable's
            //     "address point" — past the offset-to-top + RTTI
            //     prelude. Function-pointer slots start there.
            //   - `vtable_index` is the function-pointer rank
            //     populated by the importer's vtable post-pass.
            //
            // Multi-inheritance / virtual-base complications would
            // require offset adjustments on the `this` pointer
            // before the call. Left for a follow-up release;
            // single-inheritance hierarchies (the common case)
            // work today.
            let receiver_kw = match emission.wrapper_receiver {
                WrapperReceiver::SelfConst => "&self",
                WrapperReceiver::SelfMut => "&mut self",
                _ => unreachable!("virtual method without self receiver"),
            };
            let self_ptr_ty = match emission.wrapper_receiver {
                WrapperReceiver::SelfConst => "*const Self",
                WrapperReceiver::SelfMut => "*mut Self",
                _ => unreachable!(),
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
            let _ = writeln!(out, "{indent}    unsafe {{");
            let _ = writeln!(
                out,
                "{indent}        let __this: {self_ptr_ty} = {self_cast};",
            );
            let _ = writeln!(
                out,
                "{indent}        let __vtable: *const usize = \
                 *(__this as *const *const usize);",
            );
            let _ = writeln!(
                out,
                "{indent}        let __slot: usize = *__vtable.add({vtable_index});",
            );
            // Function pointer type: extern "C++" so the fork's
            // ABI overlay applies. Param signature mirrors the
            // wrapper, with the explicit `this` slot.
            let fn_ptr_args = if emission.wrapper_params.is_empty() {
                self_ptr_ty.to_string()
            } else {
                format!("{self_ptr_ty}, {}", strip_arg_names(&emission.wrapper_params))
            };
            let fn_ptr_ret = if emission.wrapper_return == "()" {
                String::new()
            } else {
                format!(" -> {}", emission.wrapper_return)
            };
            let _ = writeln!(
                out,
                "{indent}        let __f: unsafe extern \"C++\" fn({fn_ptr_args}){fn_ptr_ret} = \
                 ::core::mem::transmute(__slot);",
            );
            if emission.forward_args.is_empty() {
                let _ = writeln!(out, "{indent}        __f(__this)");
            } else {
                let _ = writeln!(
                    out,
                    "{indent}        __f(__this, {fwd})",
                    fwd = emission.forward_args,
                );
            }
            let _ = writeln!(out, "{indent}    }}");
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
    macro_path: MacroPath,
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
    let _ = writeln!(block, "{indent}{} {{", macro_path.token());
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
        // M15: function pointer / bare function type. Both render
        // as Rust function-pointer types (`extern "C" fn(...) -> ret`).
        // Variadic C functions render with `...` which Rust supports
        // only behind `unsafe extern "C"` and require feature-gated
        // syntax for non-`extern "C"` ABIs — keep it `extern "C"`
        // since C++ callbacks always cross an `extern "C"` boundary.
        CxxType::Fn(sig) => {
            // Variadic function pointers in Rust use `...` and are
            // currently unstable-ish in non-extern contexts — but
            // for `extern "C" fn` the compiler accepts them.
            let mut parts = Vec::with_capacity(sig.params.len());
            for (i, p) in sig.params.iter().enumerate() {
                let r = render_rust_type(ctx, *p, &format!("{where_} fnptr arg {i}"))?;
                parts.push(r);
            }
            let ret = render_rust_type(ctx, sig.ret, &format!("{where_} fnptr ret"))?;
            let args = if sig.variadic {
                let mut v = parts;
                v.push("...".into());
                v.join(", ")
            } else {
                parts.join(", ")
            };
            // Wrap in `Option<...>` so the natural mapping for a
            // C++ `void (*)()` parameter (which can be `nullptr`)
            // works without extra ceremony — Option<extern "C"
            // fn(...)> uses the same null-pointer-optimization
            // representation as the bare fn pointer.
            if matches!(ctx.type_of(sig.ret), CxxType::Void) {
                format!("Option<unsafe extern \"C\" fn({args})>")
            } else {
                format!("Option<unsafe extern \"C\" fn({args}) -> {ret}>")
            }
        }
        other => {
            return Err(BindingsError::UnsupportedType {
                where_: where_.into(),
                kind: format!("{other:?}"),
            });
        }
    })
}

/// M16: render one captured C++ enum as Rust source. Picks
/// between the idiomatic `pub enum` shape and the fall-back
/// `pub struct + assoc consts` shape based on whether all
/// variants are unique and the enum is `enum class`-scoped.
///
/// Both shapes are layout-identical (single underlying integer)
/// so the choice is purely about Rust-side ergonomics:
///
/// - `pub enum` lets `match` exhaustiveness fire and supports
///   derives, but disallows duplicate discriminants.
/// - `pub struct` tolerates aliasing variants and is the safe
///   default for unscoped enums users may bit-twiddle on.
fn render_cxx_enum(
    ctx: &CxxTypeCtx,
    def: &crate::enums::CxxEnumDef,
    indent: &str,
) -> Result<String, BindingsError> {
    let underlying_kind = ctx.type_of(def.underlying);
    let (signed, width) = match underlying_kind {
        CxxType::Int { signed, width } => (*signed, *width),
        CxxType::Bool => (false, IntWidth::I8),
        other => {
            return Err(BindingsError::UnsupportedType {
                where_: format!("enum `{}` underlying", def.name.0),
                kind: format!("non-integer underlying: {other:?}"),
            });
        }
    };
    let int_repr = int_rust(signed, width);

    // Detect duplicate discriminants. Aliasing happens often
    // enough in real headers that we always check.
    let mut seen = std::collections::HashSet::new();
    let mut has_dups = false;
    for v in &def.variants {
        if !seen.insert(v.value) {
            has_dups = true;
            break;
        }
    }

    // Shape selection: idiomatic `pub enum` only when scoped
    // AND no aliasing. Anything else falls back to the
    // `pub struct + assoc consts` shape.
    let prefer_pub_enum = def.scoped && !has_dups;

    let mut out = String::new();
    let _ = writeln!(out, "{indent}#[allow(non_camel_case_types)]");
    if prefer_pub_enum {
        let _ = writeln!(out, "{indent}#[repr({int_repr})]");
        let _ = writeln!(out, "{indent}#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]");
        let _ = writeln!(out, "{indent}pub enum {} {{", def.name.0);
        for v in &def.variants {
            // Cast to keep negative discriminants legal with
            // unsigned reprs on the C++ side (libclang signed
            // accessor sign-extends).
            let lit = render_enum_discriminant(v.value, signed);
            let _ = writeln!(
                out,
                "{indent}    #[allow(non_camel_case_types)] {} = {lit},",
                v.name,
            );
        }
        let _ = writeln!(out, "{indent}}}");
    } else {
        // `#[repr(transparent)]` on the wrapper so the layout
        // and ABI match the underlying integer exactly. The
        // `pub` field on the inner integer lets users reach
        // for `MyEnum(0).0` / `MyEnum(x.0 | y.0)` for the
        // bitwise math idioms common in unscoped C++ enums.
        let _ = writeln!(out, "{indent}#[repr(transparent)]");
        let _ = writeln!(
            out,
            "{indent}#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]",
        );
        let _ = writeln!(
            out,
            "{indent}pub struct {}(pub {int_repr});",
            def.name.0,
        );
        let _ = writeln!(out, "{indent}impl {} {{", def.name.0);
        for v in &def.variants {
            let lit = render_enum_discriminant(v.value, signed);
            let _ = writeln!(
                out,
                "{indent}    #[allow(non_upper_case_globals)] pub const {}: Self = Self({lit});",
                v.name,
            );
        }
        let _ = writeln!(out, "{indent}}}");
    }
    Ok(out)
}

/// Render an enum discriminant literal. libclang gives us a
/// signed `i64`; if the underlying type is unsigned we cast at
/// the literal level so negative source values (rare but legal,
/// e.g. `enum E : unsigned { X = -1 }`) round-trip correctly.
fn render_enum_discriminant(value: i64, signed: bool) -> String {
    if signed || value >= 0 {
        format!("{value}")
    } else {
        // Two's-complement bit-pattern as the unsigned literal.
        let as_u64 = value as u64;
        format!("{as_u64}")
    }
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
    fn virtual_method_emits_vtable_lookup_wrapper_with_transmute() {
        // Build a polymorphic class by hand: one virtual method
        // with a known vtable_index. The emitter should skip the
        // extern decl for this method (no `#[link_name]`) and emit
        // a wrapper that loads the vptr, indexes, transmutes, and
        // calls.
        let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
        let i32_ = ctx.intern_type(CxxType::Int {
            signed: true,
            width: IntWidth::I32,
        });
        let id = ctx.define_rust_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Shape".into()))]),
            bases: vec![],
            fields: vec![],
            methods: vec![MethodDef {
                name: MethodName::Ident(Ident("area".into())),
                sig: FnSig {
                    params: vec![],
                    ret: i32_,
                    cv: CvQual { is_const: true, is_volatile: false },
                    ref_q: None,
                    variadic: false,
                    noexcept: false,
                },
                virtuality: Virtuality::Virtual,
                vtable_index: Some(0),
                special: None,
            }],
            kind: RecordKind::Struct,
            is_polymorphic: true,
            is_final: false,
            source_alignment: None,
        });

        let src = generate_rust_bindings(&ctx, &[id], &RustBindingsConfig::default())
            .expect("emit");

        // No `#[link_name]` for the virtual method — it's not in
        // the extern block at all.
        assert!(
            !src.contains("#[link_name = \"_ZNK5Shape4areaEv\"]"),
            "virtual method should not have a #[link_name] extern decl:\n{src}"
        );
        // Wrapper exists, takes `&self`, returns `i32`.
        assert!(
            src.contains("pub fn area(&self) -> i32 {"),
            "expected `pub fn area(&self) -> i32` wrapper:\n{src}"
        );
        // Vtable lookup pattern.
        assert!(
            src.contains("*(__this as *const *const usize)"),
            "expected vptr load:\n{src}"
        );
        assert!(
            src.contains("__vtable.add(0)"),
            "expected vtable_index = 0 lookup:\n{src}"
        );
        assert!(
            src.contains("::core::mem::transmute(__slot)"),
            "expected transmute call:\n{src}"
        );
        // The transmuted fn pointer carries `extern "C++"` so the
        // fork's ABI overlay routes by-value record returns via
        // sret on aarch64 (P09.50).
        assert!(
            src.contains("unsafe extern \"C++\" fn(*const Self) -> i32"),
            "expected extern \"C++\" fn pointer type:\n{src}"
        );
    }

    #[test]
    fn annotation_overrides_class_name_in_emission() {
        use crate::annotations::Annotation;
        let (ctx, id) = point_ctx();
        let mut ann = AnnotationSet::default();
        ann.inline.insert(
            "Point".into(),
            vec![Annotation::Name("RenamedPoint".into())],
        );
        let src = generate_rust_bindings_with_annotations(
            &ctx,
            &[id],
            &ann,
            &RustBindingsConfig::default(),
        )
        .expect("emit");
        assert!(
            src.contains("pub struct RenamedPoint"),
            "expected renamed struct identifier:\n{src}"
        );
        assert!(
            !src.contains("pub struct Point "),
            "original `Point` shouldn't appear as a struct head:\n{src}"
        );
    }

    #[test]
    fn annotation_overrides_method_name_in_emission() {
        use crate::annotations::Annotation;
        let (ctx, id) = point_ctx();
        let mut ann = AnnotationSet::default();
        ann.inline.insert(
            "Point::get_x".into(),
            vec![Annotation::Name("x".into())],
        );
        let src = generate_rust_bindings_with_annotations(
            &ctx,
            &[id],
            &ann,
            &RustBindingsConfig::default(),
        )
        .expect("emit");
        assert!(
            src.contains("pub fn x(&self) -> i32"),
            "expected renamed method `x`:\n{src}"
        );
        assert!(
            !src.contains("pub fn get_x("),
            "original `get_x` wrapper shouldn't appear:\n{src}"
        );
    }

    #[test]
    fn skip_annotation_omits_class_from_emission() {
        use crate::annotations::Annotation;
        let (ctx, id) = point_ctx();
        let mut ann = AnnotationSet::default();
        ann.inline.insert("Point".into(), vec![Annotation::Skip]);
        let src = generate_rust_bindings_with_annotations(
            &ctx,
            &[id],
            &ann,
            &RustBindingsConfig::default(),
        )
        .expect("emit");
        assert!(
            !src.contains("pub struct Point"),
            "Skip-annotated class shouldn't be emitted:\n{src}"
        );
    }

    #[test]
    fn static_method_emits_receiver_less_wrapper_and_extern() {
        // M11: a method marked static via `ctx.mark_method_static`
        // emits `pub fn run() -> i32` (no `&self`) and the extern
        // decl drops the `this` slot.
        let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
        let i32_ = ctx.intern_type(CxxType::Int {
            signed: true,
            width: IntWidth::I32,
        });
        let id = ctx.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Fl".into()))]),
            bases: vec![],
            fields: vec![],
            methods: vec![MethodDef {
                name: MethodName::Ident(Ident("run".into())),
                sig: FnSig {
                    params: vec![],
                    ret: i32_,
                    cv: CvQual { is_const: false, is_volatile: false },
                    ref_q: None,
                    variadic: false,
                    noexcept: false,
                },
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: None,
            }],
            kind: RecordKind::Class,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        ctx.mark_method_static(id, 0);

        let src = generate_rust_bindings(&ctx, &[id], &RustBindingsConfig::default())
            .expect("emit");

        // Wrapper has no receiver.
        assert!(
            src.contains("pub fn run() -> i32"),
            "expected receiver-less static wrapper:\n{src}"
        );
        // Extern decl skips the `this` slot.
        assert!(
            src.contains("fn __cxx_Fl_run() -> i32;"),
            "expected static extern with no `this`:\n{src}"
        );
    }

    #[test]
    fn macro_set_renders_pub_const_lines_at_top() {
        use crate::macros::{MacroConst, MacroSet, MacroValue};
        let (ctx, id) = point_ctx();
        let macros = MacroSet {
            entries: vec![
                MacroConst {
                    name: "FL_RED".into(),
                    value: MacroValue::SignedInteger(88),
                },
                MacroConst {
                    name: "FL_PI".into(),
                    value: MacroValue::Float(3.14159),
                },
                MacroConst {
                    name: "FL_NAME".into(),
                    value: MacroValue::String("widget".into()),
                },
                MacroConst {
                    name: "FL_FLAG".into(),
                    value: MacroValue::Bool(true),
                },
            ],
        };
        let src = generate_rust_bindings_with_macros(
            &ctx,
            &[id],
            &AnnotationSet::default(),
            &macros,
            &RustBindingsConfig::default(),
        )
        .expect("emit");
        assert!(
            src.contains("pub const FL_RED: i64 = 88;"),
            "expected signed-int macro:\n{src}"
        );
        assert!(
            src.contains("pub const FL_PI: f64 = 3.14159;"),
            "expected float macro:\n{src}"
        );
        assert!(
            src.contains("pub const FL_NAME: &str = \"widget\";"),
            "expected string macro:\n{src}"
        );
        assert!(
            src.contains("pub const FL_FLAG: bool = true;"),
            "expected bool macro:\n{src}"
        );
    }

    #[test]
    fn empty_macro_set_does_not_inject_const_block() {
        use crate::macros::MacroSet;
        let (ctx, id) = point_ctx();
        let src = generate_rust_bindings_with_macros(
            &ctx,
            &[id],
            &AnnotationSet::default(),
            &MacroSet::default(),
            &RustBindingsConfig::default(),
        )
        .expect("emit");
        assert!(
            !src.contains("// M12:"),
            "no macros means no M12 comment:\n{src}"
        );
        assert!(
            !src.contains("pub const "),
            "no macros means no pub const:\n{src}"
        );
    }

    #[test]
    fn direct_extern_cpp_emits_heap_alloc_shims_paired_with_cxx_heap() {
        // M14: ctors get an extra `__cxx_<class>_new_heap_<i>`
        // extern decl + a `pub fn new_boxed(...) -> ::cxx::CxxHeap<Self>`
        // wrapper. Once per class, a `__cxx_<class>_delete` extern
        // decl + `unsafe impl ::cxx::CxxDeletable for <class>`.
        let (ctx, id) = point_ctx();
        let src = generate_rust_bindings(
            &ctx,
            &[id],
            &RustBindingsConfig {
                emit_heap_alloc: true,
                ..RustBindingsConfig::default()
            },
        )
        .expect("emit");

        // Heap extern block has the new_heap thunk for the single ctor.
        assert!(
            src.contains("fn __cxx_Point_new_heap_0(arg0: i32, arg1: i32) -> *mut Point;"),
            "expected heap-ctor extern decl:\n{src}"
        );
        // Heap extern block has the delete thunk.
        assert!(
            src.contains("fn __cxx_Point_delete(p: *mut Point);"),
            "expected delete extern decl:\n{src}"
        );
        // Heap-alloc wrapper.
        assert!(
            src.contains("pub fn new_boxed(arg0: i32, arg1: i32) -> ::cxx::CxxHeap<Self>"),
            "expected heap wrapper:\n{src}"
        );
        // CxxDeletable impl.
        assert!(
            src.contains("unsafe impl ::cxx::CxxDeletable for Point"),
            "expected CxxDeletable impl:\n{src}"
        );
        assert!(
            src.contains("__cxx_Point_delete(p)"),
            "expected delete-shim call inside cxx_delete:\n{src}"
        );
    }

    #[test]
    fn poisoned_class_emits_opaque_struct_with_reason_doc_comment() {
        // M9: classes the importer marks as poison (recoverable
        // lowering failures) should render as an opaque struct
        // with the failure reason in a `///` doc comment. No
        // extern block, no impl, no Drop.
        let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
        let id = ctx.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("BrokenWidget".into()))]),
            bases: vec![],
            fields: vec![],
            methods: vec![],
            kind: RecordKind::Class,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        ctx.poison(
            id,
            "widget.hpp:14:7: virtual inheritance not supported",
        );

        let src = generate_rust_bindings(&ctx, &[id], &RustBindingsConfig::default())
            .expect("emit");

        assert!(
            src.contains("/// widget.hpp:14:7: virtual inheritance not supported"),
            "expected reason in doc comment:\n{src}"
        );
        assert!(
            src.contains("/// (Class poisoned by `cxx_importer`"),
            "expected poison-marker comment:\n{src}"
        );
        assert!(
            src.contains("pub struct BrokenWidget"),
            "expected opaque struct decl:\n{src}"
        );
        assert!(
            !src.contains("unsafe extern \"C++\""),
            "poisoned class should have no extern block:\n{src}"
        );
        assert!(
            !src.contains("impl BrokenWidget"),
            "poisoned class should have no impl block:\n{src}"
        );
        assert!(
            !src.contains("impl ::core::ops::Drop"),
            "poisoned class should have no Drop impl:\n{src}"
        );
    }

    #[test]
    fn cxx_class_macro_backend_emits_stable_macro_invocation() {
        let (ctx, id) = point_ctx();
        let cfg = RustBindingsConfig {
            backend: BindingsBackend::CxxClassMacro,
            ..RustBindingsConfig::default()
        };
        let src = generate_rust_bindings(&ctx, &[id], &cfg).expect("emit");

        // Stable-rustc-friendly macro path.
        assert!(
            src.contains("::rustcc_macros::cxx_class!"),
            "expected cxx_class! invocation:\n{src}"
        );
        // Same input grammar as native_cpp_class! — size/align/methods.
        assert!(
            src.contains("#[size = 8]"),
            "expected #[size = 8]:\n{src}"
        );
        assert!(
            src.contains("#[align = 4]"),
            "expected #[align = 4]:\n{src}"
        );
        assert!(
            src.contains("pub class Point"),
            "expected `pub class Point`:\n{src}"
        );
        assert!(
            src.contains("#[ctor] fn new(arg0: i32, arg1: i32) -> Self;"),
            "expected ctor line:\n{src}"
        );
        // Native macro path should NOT be in this output.
        assert!(
            !src.contains("::rustcc_macros::native_cpp_class!"),
            "native macro path leaked into stable backend:\n{src}"
        );
    }
}
