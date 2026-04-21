//! Parse rustcc-specific attributes on Rust items and lower the
//! results into `rustc_abi_cxx` IR.
//!
//! Recognizes:
//!
//! - `#[repr(cpp)]` on structs — the type is exposed to C++ via the
//!   generated header. May combine with `#[repr(align(N))]` to force a
//!   minimum alignment.
//! - `#[cpp_name = "Foo"]` on structs — overrides the C++-visible
//!   name. Useful when the Rust identifier would collide with an
//!   existing C++ type or breaks C++ naming conventions.
//! - `#[cpp_name = "method"]` on `impl` methods — same override, for
//!   the method name spelled in the generated header and the mangled
//!   symbol.
//!
//! Scope for v1:
//!
//! - Struct fields: scalar builtins only (`bool`, `i8..i128`,
//!   `u8..u128`, `f32`, `f64`). Pointers/references/records come later
//!   once cross-module `ClassId` resolution is wired.
//! - `impl` methods: identifier-named, `&self` / `&mut self` / by-value
//!   / no-receiver. Parameter and return types are the same scalar set
//!   as fields.
//!
//! Out of scope for v1: enums, unions, traits, generics, lifetimes on
//! methods, `unsafe` methods, attributes on fields.

use std::collections::HashMap;

use rustc_abi_cxx::{
    ClassDef, ClassId, CxxType, CxxTypeCtx, FieldDef, FloatKind, FnSig, Ident,
    IntWidth, MethodDef, MethodName, NameSegment, NestedName, RecordKind,
    RustEnumDef, RustEnumId, RustEnumVariant, SpecialMember, TypeId, Virtuality,
};
use syn::spanned::Spanned;
use syn::{
    Expr, ExprLit, Fields, File, ImplItem, ImplItemFn, Item, ItemEnum,
    ItemImpl, ItemStruct, Lit, Meta, Type, TypePath,
};

/// Format a syn span as `line:column` for inclusion in error
/// messages. Returns an empty string when span info isn't available
/// (e.g., spans from macro-synthesized tokens sometimes report
/// `LineColumn(0, 0)` — not useful to the user).
fn span_note(span: proc_macro2::Span) -> String {
    let start = span.start();
    if start.line == 0 {
        String::new()
    } else {
        format!("line {}, col {}", start.line, start.column + 1)
    }
}

/// Combine a where-label with a span note if one's available.
/// `label` describes *what* broke (e.g. `field `Foo::x``); the span
/// tells the user *where* to look. Result shape: `"<label> (at line
/// N, col M)"` or just `"<label>"` when no span info is available.
fn format_span(span: proc_macro2::Span, label: &str) -> String {
    let note = span_note(span);
    if note.is_empty() {
        label.to_string()
    } else {
        format!("{label} (at {note})")
    }
}

/// Parse failure. The `span` field carries the approximate source
/// location so callers (the rustc fork, a build-driver scanner) can
/// emit useful diagnostics; it's a `proc_macro2::Span` which works in
/// both proc-macro and non-proc-macro contexts.
#[derive(Debug, Clone)]
pub enum AttrError {
    /// A syn parse error while reading the Rust source.
    Syn(String),
    /// `#[repr(cpp)]` appeared with an unsupported modifier (e.g.,
    /// `#[repr(cpp, C)]`, which mixes two layout protocols).
    BadRepr {
        msg: String,
    },
    /// `#[cpp_name]` was present but not a `"literal string"` value.
    BadCppName {
        msg: String,
    },
    /// A field/parameter/return type uses a Rust form not yet
    /// supported by the v1 lowering (references, generics, trait
    /// objects, etc.). `hint` identifies which item triggered it.
    UnsupportedType {
        hint: String,
        rust_source: String,
    },
    /// An `impl` block was anchored at a path the parser couldn't
    /// resolve to a locally-declared `#[repr(cpp)]` type. Currently
    /// identifier-only `Self` bases are supported; `some::path::Foo`
    /// and generics are not.
    ImplTargetUnresolved {
        msg: String,
    },
    /// A method shape the v1 lowering doesn't handle (async, unsafe,
    /// generic, default body absent on non-trait impls, etc.).
    UnsupportedMethodShape {
        name: String,
        msg: String,
    },
}

impl core::fmt::Display for AttrError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for AttrError {}

/// A parsed `#[repr(cpp)]` struct, pre-IR. Kept as its own type so
/// lowering into `CxxTypeCtx` can happen in a later pass when the
/// context exists (the rustc fork builds one per crate compilation).
#[derive(Debug, Clone)]
pub struct ReprCppStruct {
    /// Rust identifier (e.g., `Point`).
    pub rust_name: String,
    /// C++-visible identifier; equal to `rust_name` unless
    /// `#[cpp_name = "..."]` overrides it.
    pub cpp_name: String,
    /// Minimum alignment from `#[repr(align(N))]`, if any.
    pub align: Option<u64>,
    pub fields: Vec<ReprCppField>,
}

#[derive(Debug, Clone)]
pub struct ReprCppField {
    pub name: String,
    pub ty: RustTy,
}

/// A parsed `impl Foo { ... }` block whose target is a Rust-side type.
#[derive(Debug, Clone)]
pub struct ReprCppImpl {
    /// Rust identifier of the target type.
    pub target_rust_name: String,
    pub methods: Vec<ReprCppMethod>,
}

/// A parsed `impl Drop for Foo { ... }` — the Rust equivalent of a
/// user-defined C++ destructor. The rustc fork's codegen emits the
/// D1/D2 dtor bodies that invoke Rust's drop glue; pre-fork we only
/// need the presence signal to mark the class as non-trivially-
/// destructible (affects sret classification and call-lowering).
#[derive(Debug, Clone)]
pub struct ReprCppDrop {
    pub target_rust_name: String,
}

#[derive(Debug, Clone)]
pub struct ReprCppMethod {
    pub rust_name: String,
    pub cpp_name: String,
    pub receiver: Receiver,
    pub params: Vec<RustTy>,
    pub ret: RustTy,
    pub is_const: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receiver {
    /// No `self` parameter — a static method / C++ `static` member.
    None,
    /// `&self`.
    RefSelf,
    /// `&mut self`.
    RefMutSelf,
    /// `self` (by value).
    ByValueSelf,
}

/// A Rust primitive type. The v1 set for fields; methods also admit
/// `RustTy::Record(name)` via `RustTy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustScalar {
    Unit,
    Bool,
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
    F32,
    F64,
}

/// The Rust type surface v1 understands in method signatures.
///
/// References and raw pointers are accepted in parameter/return
/// positions. Reference types map to C++ lvalue references
/// (`&T` → `T const&`, `&mut T` → `T&`); raw pointers map to C++
/// pointers with matching const-ness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustTy {
    Scalar(RustScalar),
    /// A locally-declared `#[repr(cpp)]` type, referenced by its Rust
    /// name. Resolved to a `ClassId` during `lower_into_ctx`.
    Record(String),
    /// `&T` (mutable=false) or `&mut T` (mutable=true).
    Ref {
        mutable: bool,
        inner: Box<RustTy>,
    },
    /// `*const T` (mutable=false) or `*mut T` (mutable=true).
    Ptr {
        mutable: bool,
        inner: Box<RustTy>,
    },
}

impl RustScalar {
    fn to_cxx(self) -> CxxType {
        match self {
            Self::Unit => CxxType::Void,
            Self::Bool => CxxType::Bool,
            Self::I8 => CxxType::Int { signed: true, width: IntWidth::I8 },
            Self::I16 => CxxType::Int { signed: true, width: IntWidth::I16 },
            Self::I32 => CxxType::Int { signed: true, width: IntWidth::I32 },
            Self::I64 => CxxType::Int { signed: true, width: IntWidth::I64 },
            Self::I128 => CxxType::Int { signed: true, width: IntWidth::I128 },
            Self::U8 => CxxType::Int { signed: false, width: IntWidth::I8 },
            Self::U16 => CxxType::Int { signed: false, width: IntWidth::I16 },
            Self::U32 => CxxType::Int { signed: false, width: IntWidth::I32 },
            Self::U64 => CxxType::Int { signed: false, width: IntWidth::I64 },
            Self::U128 => {
                CxxType::Int { signed: false, width: IntWidth::I128 }
            }
            Self::F32 => CxxType::Float { kind: FloatKind::F32 },
            Self::F64 => CxxType::Float { kind: FloatKind::F64 },
        }
    }

    fn lower(self, ctx: &mut CxxTypeCtx) -> TypeId {
        ctx.intern_type(self.to_cxx())
    }
}

impl RustTy {
    fn lower(
        &self,
        ctx: &mut CxxTypeCtx,
        class_lookup: &HashMap<String, ClassId>,
    ) -> Result<TypeId, AttrError> {
        match self {
            RustTy::Scalar(s) => Ok(s.lower(ctx)),
            RustTy::Record(name) => {
                let id = class_lookup.get(name).copied().ok_or_else(|| {
                    AttrError::UnsupportedType {
                        hint: "method signature".into(),
                        rust_source: format!(
                            "unresolved record `{name}` — only locally-declared #[repr(cpp)] types can be named by v1"
                        ),
                    }
                })?;
                Ok(ctx.intern_type(CxxType::Record(id)))
            }
            RustTy::Ref { mutable, inner } => {
                let pointee = inner.lower(ctx, class_lookup)?;
                // Rust `&T` is immutable → C++ `T const&`.
                // Rust `&mut T` is unique mutable → C++ `T&`.
                let cv = rustc_abi_cxx::CvQual {
                    is_const: !*mutable,
                    is_volatile: false,
                };
                Ok(ctx.intern_type(CxxType::Ref {
                    pointee,
                    kind: rustc_abi_cxx::RefKind::Lvalue,
                    cv,
                }))
            }
            RustTy::Ptr { mutable, inner } => {
                let pointee = inner.lower(ctx, class_lookup)?;
                let cv = rustc_abi_cxx::CvQual {
                    is_const: !*mutable,
                    is_volatile: false,
                };
                Ok(ctx.intern_type(CxxType::Ptr { pointee, cv }))
            }
        }
    }
}

/// Parsed output of a single crate / file scan.
#[derive(Debug, Default)]
pub struct ParsedModule {
    pub structs: Vec<ReprCppStruct>,
    pub impls: Vec<ReprCppImpl>,
    pub drops: Vec<ReprCppDrop>,
    pub enums: Vec<ReprCppEnum>,
}

impl ParsedModule {
    /// Merge another module's items into this one. Used by multi-file
    /// scans to accumulate results.
    pub fn extend(&mut self, other: ParsedModule) {
        self.structs.extend(other.structs);
        self.impls.extend(other.impls);
        self.drops.extend(other.drops);
        self.enums.extend(other.enums);
    }
}

/// A parsed `#[repr(cpp)] enum` (or `#[cpp_class] enum`). Only simple
/// C-style enums — no variants with fields, no generics — are
/// supported in v1. Variants may carry explicit discriminants
/// (`Red = 1`); otherwise the C++ side auto-increments from 0 using
/// scoped-enum semantics.
#[derive(Debug, Clone)]
pub struct ReprCppEnum {
    pub rust_name: String,
    pub cpp_name: String,
    /// Explicit underlying integer width from `#[repr(iN)]` /
    /// `#[repr(uN)]`. `None` → use `i32`, matching C++'s default
    /// scoped-enum underlying type.
    pub underlying: Option<RustScalar>,
    pub variants: Vec<ReprCppEnumVariantSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReprCppEnumVariantSpec {
    pub name: String,
    pub discriminant: Option<i64>,
}

// -------- Public entry points ------------------------------------------

/// Parse a whole file of Rust source. Non-`#[repr(cpp)]` items are
/// skipped silently; malformed items produce errors.
pub fn parse_source(source: &str) -> Result<ParsedModule, AttrError> {
    let file: File = syn::parse_str(source)
        .map_err(|e| AttrError::Syn(format!("{e}")))?;
    parse_file(&file)
}

/// Walk a crate's source tree (`src/**/*.rs` under `crate_root`) and
/// return the merged `ParsedModule`, plus the ordered list of `.rs`
/// files that were read (for fingerprinting).
///
/// This is the pre-fork stand-in for rustc's own HIR traversal. When
/// the fork lands, it'll replace this helper with a query over
/// `TyCtxt::hir()` that hits every module in the crate graph —
/// including `cfg`-gated ones — without walking the disk. Until then
/// this disk walk is adequate for smoke-tests and demos, but it
/// deliberately does NOT evaluate `mod x;` declarations (reporting
/// every `.rs` under `src/` regardless) so users don't need a correct
/// `mod` tree to try out `#[repr(cpp)]`.
pub fn scan_crate_sources(
    crate_root: &std::path::Path,
) -> Result<(ParsedModule, Vec<std::path::PathBuf>), AttrError> {
    let src_dir = crate_root.join("src");
    let mut files = Vec::new();
    if src_dir.is_dir() {
        walk_rs_files(&src_dir, &mut files).map_err(|e| {
            AttrError::Syn(format!(
                "scanning {}: {e}",
                src_dir.display()
            ))
        })?;
    }
    files.sort();

    let mut merged = ParsedModule::default();
    for path in &files {
        let body = std::fs::read_to_string(path).map_err(|e| {
            AttrError::Syn(format!("reading {}: {e}", path.display()))
        })?;
        let parsed = parse_source(&body).map_err(|e| match e {
            AttrError::Syn(s) => {
                AttrError::Syn(format!("{}: {s}", path.display()))
            }
            other => other,
        })?;
        merged.extend(parsed);
    }
    Ok((merged, files))
}

fn walk_rs_files(
    dir: &std::path::Path,
    out: &mut Vec<std::path::PathBuf>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let path = entry.path();
        if ft.is_dir() {
            walk_rs_files(&path, out)?;
        } else if ft.is_file()
            && path.extension().and_then(|s| s.to_str()) == Some("rs")
        {
            out.push(path);
        }
    }
    Ok(())
}

pub fn parse_file(file: &File) -> Result<ParsedModule, AttrError> {
    let mut out = ParsedModule::default();
    for item in &file.items {
        match item {
            Item::Struct(s) => {
                if let Some(parsed) = extract_repr_cpp_struct(s)? {
                    out.structs.push(parsed);
                }
            }
            Item::Impl(i) => {
                if let Some(d) = extract_drop_impl(i)? {
                    out.drops.push(d);
                } else if let Some(parsed) = extract_repr_cpp_impl(i)? {
                    out.impls.push(parsed);
                }
            }
            Item::Enum(e) => {
                if let Some(parsed) = extract_repr_cpp_enum(e)? {
                    out.enums.push(parsed);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Extract a `#[repr(cpp)]` / `#[cpp_class]` enum. Returns `Ok(None)`
/// when neither marker is present; `Err` when a marker is present
/// but the enum shape isn't supported (variants with fields, etc.).
pub fn extract_repr_cpp_enum(
    item: &ItemEnum,
) -> Result<Option<ReprCppEnum>, AttrError> {
    let (is_cpp, _align) = parse_repr_attrs(&item.attrs)?;
    let has_cpp_class = has_cpp_class_attr(&item.attrs);
    if !is_cpp && !has_cpp_class {
        return Ok(None);
    }
    let rust_name = item.ident.to_string();
    let cpp_name = parse_cpp_name(&item.attrs)?.unwrap_or_else(|| rust_name.clone());
    let underlying = parse_repr_underlying(&item.attrs)?;

    let mut variants = Vec::with_capacity(item.variants.len());
    for v in &item.variants {
        if !matches!(v.fields, Fields::Unit) {
            return Err(AttrError::UnsupportedType {
                hint: format_span(
                    v.span(),
                    &format!("variant `{}::{}`", rust_name, v.ident),
                ),
                rust_source:
                    "enum variants with fields are not supported in v1 — \
                     use a plain C-style enum"
                        .into(),
            });
        }
        let discriminant = match &v.discriminant {
            None => None,
            Some((_, expr)) => match expr {
                Expr::Lit(ExprLit {
                    lit: Lit::Int(i), ..
                }) => Some(i.base10_parse::<i64>().map_err(|e| {
                    AttrError::UnsupportedType {
                        hint: format_span(
                            v.span(),
                            &format!("variant `{}::{}`", rust_name, v.ident),
                        ),
                        rust_source: format!("bad discriminant: {e}"),
                    }
                })?),
                _ => {
                    return Err(AttrError::UnsupportedType {
                        hint: format_span(
                            v.span(),
                            &format!("variant `{}::{}`", rust_name, v.ident),
                        ),
                        rust_source:
                            "only integer-literal discriminants are \
                             supported in v1 (no consts, no expressions)"
                                .into(),
                    });
                }
            },
        };
        variants.push(ReprCppEnumVariantSpec {
            name: v.ident.to_string(),
            discriminant,
        });
    }

    Ok(Some(ReprCppEnum {
        rust_name,
        cpp_name,
        underlying,
        variants,
    }))
}

/// Extract `#[repr(iN)]` / `#[repr(uN)]` — the underlying integer
/// width for an enum. None when no such repr is present (the
/// emitter will default to `i32`).
fn parse_repr_underlying(
    attrs: &[syn::Attribute],
) -> Result<Option<RustScalar>, AttrError> {
    for attr in attrs {
        if !attr.path().is_ident("repr") {
            continue;
        }
        let metas = attr
            .parse_args_with(
                syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
            )
            .map_err(|e| AttrError::BadRepr { msg: format!("{e}") })?;
        for meta in metas {
            if let Meta::Path(p) = meta {
                if let Some(ident) = p.get_ident() {
                    let name = ident.to_string();
                    let scalar = match name.as_str() {
                        "i8" => RustScalar::I8,
                        "i16" => RustScalar::I16,
                        "i32" => RustScalar::I32,
                        "i64" => RustScalar::I64,
                        "u8" => RustScalar::U8,
                        "u16" => RustScalar::U16,
                        "u32" => RustScalar::U32,
                        "u64" => RustScalar::U64,
                        _ => continue,
                    };
                    return Ok(Some(scalar));
                }
            }
        }
    }
    Ok(None)
}

/// Extract a struct marked for C++ exposure. Accepts two entry
/// markers:
///
/// - `#[repr(cpp)]` — the fork-native syntax.
/// - `#[cpp_class]` — the proc-macro entry point in `rustcc_macros`,
///   which expands to `#[repr(C)]` at compile time so stable rustc
///   accepts the struct. We still see the original attribute in the
///   scanner because `rustcc_attr` reads source text.
///
/// Returns `Ok(None)` when neither marker is present; `Err` when a
/// marker is present but the struct shape is malformed.
pub fn extract_repr_cpp_struct(
    item: &ItemStruct,
) -> Result<Option<ReprCppStruct>, AttrError> {
    let (is_cpp, align) = parse_repr_attrs(&item.attrs)?;
    let has_cpp_class = has_cpp_class_attr(&item.attrs);
    if !is_cpp && !has_cpp_class {
        return Ok(None);
    }
    let cpp_name_override = parse_cpp_name(&item.attrs)?;
    let rust_name = item.ident.to_string();
    let cpp_name = cpp_name_override.unwrap_or_else(|| rust_name.clone());

    let fields = match &item.fields {
        Fields::Named(named) => {
            let mut out = Vec::with_capacity(named.named.len());
            for f in &named.named {
                let name = f
                    .ident
                    .as_ref()
                    .expect("Fields::Named always has idents")
                    .to_string();
                // Fields accept scalars OR named record types. The
                // latter are resolved to `ClassId`s in the lowering
                // pass once every struct has been registered, so
                // cross-struct references work regardless of source
                // order. A field referencing its own type is flagged
                // at lowering time (C++ doesn't allow infinite-size
                // structs and v1 doesn't lift pointers/unique_ptr).
                let ty =
                    lower_rust_ty(&f.ty, &rust_name).ok_or_else(|| {
                        let where_ = format_span(
                            f.span(),
                            &format!("field `{rust_name}::{name}`"),
                        );
                        AttrError::UnsupportedType {
                            hint: where_,
                            rust_source: quote_type(&f.ty),
                        }
                    })?;
                out.push(ReprCppField { name, ty });
            }
            out
        }
        Fields::Unit => Vec::new(),
        Fields::Unnamed(_) => {
            return Err(AttrError::UnsupportedType {
                hint: format_span(
                    item.span(),
                    &format!("struct `{rust_name}`"),
                ),
                rust_source: "tuple struct".to_string(),
            });
        }
    };

    Ok(Some(ReprCppStruct { rust_name, cpp_name, align, fields }))
}

/// Recognize `impl Drop for T { fn drop(&mut self) { ... } }`. Returns
/// `Ok(Some(..))` when the impl is a `Drop` trait impl on a
/// (presumed) Rust-side type; `Ok(None)` for any other trait impl or
/// inherent impl so callers can fall through to
/// `extract_repr_cpp_impl`. The target identifier is left unresolved
/// (same as methods) — `lower_into_ctx` looks it up in the class map
/// and errors if it isn't a `#[repr(cpp)]` struct.
pub fn extract_drop_impl(
    item: &ItemImpl,
) -> Result<Option<ReprCppDrop>, AttrError> {
    let Some((_, trait_path, _)) = &item.trait_ else {
        return Ok(None);
    };
    // Match either bare `Drop` or fully-qualified `std::ops::Drop` /
    // `core::ops::Drop`. The last segment carries the trait ident.
    let last = trait_path
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    if last != "Drop" {
        return Ok(None);
    }
    let target_rust_name = match &*item.self_ty {
        Type::Path(TypePath { qself: None, path })
            if path.segments.len() == 1
                && path.segments[0].arguments.is_none() =>
        {
            path.segments[0].ident.to_string()
        }
        other => {
            return Err(AttrError::ImplTargetUnresolved {
                msg: format!(
                    "impl Drop target `{}` not supported (v1 accepts a bare identifier)",
                    quote_type(other)
                ),
            });
        }
    };
    Ok(Some(ReprCppDrop { target_rust_name }))
}

pub fn extract_repr_cpp_impl(
    item: &ItemImpl,
) -> Result<Option<ReprCppImpl>, AttrError> {
    // Trait impls can't expose to C++ — skip silently.
    if item.trait_.is_some() {
        return Ok(None);
    }
    // Resolve target type to a single Rust identifier. v1 rejects
    // generics and path-qualified bases; we need a bare name because
    // we can't do cross-module resolution here.
    let target_rust_name = match &*item.self_ty {
        Type::Path(TypePath { qself: None, path })
            if path.segments.len() == 1
                && path.segments[0].arguments.is_none() =>
        {
            path.segments[0].ident.to_string()
        }
        other => {
            let note = span_note(other.span());
            let where_ = if note.is_empty() {
                String::new()
            } else {
                format!(" (at {note})")
            };
            return Err(AttrError::ImplTargetUnresolved {
                msg: format!(
                    "impl target `{}`{where_} not supported (v1 accepts a bare identifier)",
                    quote_type(other)
                ),
            });
        }
    };

    let mut methods = Vec::new();
    for it in &item.items {
        let ImplItem::Fn(f) = it else { continue; };
        // Skip methods not marked for C++ exposure: for v1 we expose
        // every `pub` method; private methods are silently skipped.
        if !matches!(f.vis, syn::Visibility::Public(_)) {
            continue;
        }
        methods.push(lower_method(f, &target_rust_name)?);
    }

    Ok(Some(ReprCppImpl { target_rust_name, methods }))
}

// -------- Attribute helpers --------------------------------------------

/// Parse `#[repr(...)]` attributes on an item. Returns `(has_cpp, align)`
/// — `has_cpp` true iff `cpp` appears in any `repr(...)`, `align` the
/// explicit `align(N)` if any. Rejects `#[repr(cpp, C)]` and other
/// conflicting combinations.
fn parse_repr_attrs(
    attrs: &[syn::Attribute],
) -> Result<(bool, Option<u64>), AttrError> {
    let mut has_cpp = false;
    let mut align: Option<u64> = None;
    let mut seen_layout_proto: Option<&'static str> = None;
    for attr in attrs {
        if !attr.path().is_ident("repr") {
            continue;
        }
        let metas = attr
            .parse_args_with(
                syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
            )
            .map_err(|e| AttrError::BadRepr { msg: format!("{e}") })?;
        for meta in metas {
            match meta {
                Meta::Path(p) if p.is_ident("cpp") => {
                    has_cpp = true;
                    if let Some(other) = seen_layout_proto {
                        return Err(AttrError::BadRepr {
                            msg: format!(
                                "#[repr(cpp)] conflicts with #[repr({other})]"
                            ),
                        });
                    }
                    seen_layout_proto = Some("cpp");
                }
                Meta::Path(p) if p.is_ident("C") => {
                    if let Some("cpp") = seen_layout_proto {
                        return Err(AttrError::BadRepr {
                            msg: "#[repr(C)] conflicts with #[repr(cpp)]"
                                .into(),
                        });
                    }
                    seen_layout_proto = Some("C");
                }
                Meta::Path(p) if p.is_ident("transparent") => {
                    if let Some("cpp") = seen_layout_proto {
                        return Err(AttrError::BadRepr {
                            msg:
                                "#[repr(transparent)] conflicts with #[repr(cpp)]"
                                    .into(),
                        });
                    }
                }
                Meta::List(list) if list.path.is_ident("align") => {
                    let n = syn::parse2::<syn::LitInt>(list.tokens.clone())
                        .and_then(|lit| lit.base10_parse::<u64>())
                        .map_err(|e| AttrError::BadRepr {
                            msg: format!("bad align argument: {e}"),
                        })?;
                    if !n.is_power_of_two() {
                        return Err(AttrError::BadRepr {
                            msg: format!(
                                "#[repr(align({n}))] must be a power of two"
                            ),
                        });
                    }
                    align = Some(n);
                }
                // Ignore repr modifiers we don't recognize (e.g.,
                // `packed`, `u8`) — they're valid Rust but irrelevant
                // unless combined with cpp, in which case rejecting is
                // the right thing to do. The enum/integer reprs cannot
                // appear on a struct so we'd surface any misuse via a
                // later syn error.
                _ => {}
            }
        }
    }
    Ok((has_cpp, align))
}

/// True when the attribute list includes `#[cpp_class]` (the
/// proc-macro entry point) in any form. Accepts both the bare
/// `#[cpp_class]` and `rustcc_macros::cpp_class` path forms.
fn has_cpp_class_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        // Attribute-style invocations: `#[cpp_class]` or
        // `#[rustcc_macros::cpp_class]`. The `path()` on both forms
        // ends with the `cpp_class` ident.
        let segs = &a.path().segments;
        segs.last()
            .map(|s| s.ident == "cpp_class")
            .unwrap_or(false)
    })
}

fn parse_cpp_name(
    attrs: &[syn::Attribute],
) -> Result<Option<String>, AttrError> {
    for attr in attrs {
        if !attr.path().is_ident("cpp_name") {
            continue;
        }
        let Meta::NameValue(nv) = &attr.meta else {
            return Err(AttrError::BadCppName {
                msg: "#[cpp_name] must be `#[cpp_name = \"...\"]`".into(),
            });
        };
        match &nv.value {
            Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) => {
                return Ok(Some(s.value()));
            }
            _ => {
                return Err(AttrError::BadCppName {
                    msg: "#[cpp_name] value must be a string literal".into(),
                });
            }
        }
    }
    Ok(None)
}

// -------- Method lowering ----------------------------------------------

fn lower_method(
    f: &ImplItemFn,
    target_rust_name: &str,
) -> Result<ReprCppMethod, AttrError> {
    let rust_name = f.sig.ident.to_string();
    let cpp_name = parse_cpp_name(&f.attrs)?.unwrap_or_else(|| rust_name.clone());

    if f.sig.asyncness.is_some() {
        return Err(AttrError::UnsupportedMethodShape {
            name: rust_name,
            msg: "async methods are not supported in v1".into(),
        });
    }
    if f.sig.unsafety.is_some() {
        return Err(AttrError::UnsupportedMethodShape {
            name: rust_name,
            msg: "unsafe methods are not supported in v1".into(),
        });
    }
    if !f.sig.generics.params.is_empty() {
        return Err(AttrError::UnsupportedMethodShape {
            name: rust_name,
            msg: "generic methods are not supported in v1".into(),
        });
    }

    let mut inputs = f.sig.inputs.iter();
    let (receiver, is_const) = match inputs.next() {
        Some(syn::FnArg::Receiver(r)) => {
            if r.colon_token.is_some() {
                return Err(AttrError::UnsupportedMethodShape {
                    name: rust_name,
                    msg: "explicit receiver types not supported (use &self / &mut self / self)".into(),
                });
            }
            if r.reference.is_some() {
                if r.mutability.is_some() {
                    (Receiver::RefMutSelf, false)
                } else {
                    (Receiver::RefSelf, true)
                }
            } else {
                (Receiver::ByValueSelf, false)
            }
        }
        _ => {
            // Back up — this is a static method; still consume the
            // argument by re-building the iterator lazily.
            (Receiver::None, false)
        }
    };
    // If there was no receiver, the iterator already has all args; if
    // there was, `inputs` is now positioned past it. Good.
    let rest_inputs: Vec<&syn::FnArg> = if matches!(receiver, Receiver::None) {
        f.sig.inputs.iter().collect()
    } else {
        inputs.collect()
    };

    let mut params = Vec::new();
    for arg in rest_inputs {
        match arg {
            syn::FnArg::Typed(pt) => {
                let ty = lower_rust_ty(&pt.ty, target_rust_name)
                    .ok_or_else(|| AttrError::UnsupportedType {
                        hint: format_span(
                            pt.span(),
                            &format!("parameter of `{rust_name}`"),
                        ),
                        rust_source: quote_type(&pt.ty),
                    })?;
                params.push(ty);
            }
            syn::FnArg::Receiver(_) => {
                return Err(AttrError::UnsupportedMethodShape {
                    name: rust_name,
                    msg: "duplicate receiver in parameter list".into(),
                });
            }
        }
    }

    let ret = match &f.sig.output {
        syn::ReturnType::Default => RustTy::Scalar(RustScalar::Unit),
        syn::ReturnType::Type(_, ty) => {
            lower_rust_ty(ty, target_rust_name).ok_or_else(|| {
                AttrError::UnsupportedType {
                    hint: format_span(
                        ty.span(),
                        &format!("return of `{rust_name}`"),
                    ),
                    rust_source: quote_type(ty),
                }
            })?
        }
    };

    Ok(ReprCppMethod {
        rust_name,
        cpp_name,
        receiver,
        params,
        ret,
        is_const,
    })
}

// -------- Rust type -> RustScalar / RustTy ----------------------------

fn lower_rust_scalar(ty: &Type) -> Option<RustScalar> {
    // Unit type `()` spells differently — syn::Type::Tuple with empty.
    if let Type::Tuple(t) = ty {
        if t.elems.is_empty() {
            return Some(RustScalar::Unit);
        }
    }
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return None;
    };
    if path.segments.len() != 1 {
        return None;
    }
    let seg = &path.segments[0];
    if !seg.arguments.is_none() {
        return None;
    }
    Some(match seg.ident.to_string().as_str() {
        "bool" => RustScalar::Bool,
        "i8" => RustScalar::I8,
        "i16" => RustScalar::I16,
        "i32" => RustScalar::I32,
        "i64" => RustScalar::I64,
        "i128" => RustScalar::I128,
        "u8" => RustScalar::U8,
        "u16" => RustScalar::U16,
        "u32" => RustScalar::U32,
        "u64" => RustScalar::U64,
        "u128" => RustScalar::U128,
        "f32" => RustScalar::F32,
        "f64" => RustScalar::F64,
        _ => return None,
    })
}

/// Like `lower_rust_scalar`, but additionally accepts named types that
/// resolve to `#[repr(cpp)]` records, and reference / pointer types.
/// `Self` resolves to the target of the enclosing `impl` block
/// (`target_rust_name`); other bare identifiers are captured as
/// `RustTy::Record(name)` and resolved later when the full
/// class_lookup map is available.
fn lower_rust_ty(ty: &Type, target_rust_name: &str) -> Option<RustTy> {
    if let Some(s) = lower_rust_scalar(ty) {
        return Some(RustTy::Scalar(s));
    }
    match ty {
        Type::Reference(r) => {
            // Lifetimes are discarded — all C++ references are
            // target-lifetime at this layer (the fork handles
            // outlives relationships separately).
            let inner = lower_rust_ty(&r.elem, target_rust_name)?;
            Some(RustTy::Ref {
                mutable: r.mutability.is_some(),
                inner: Box::new(inner),
            })
        }
        Type::Ptr(p) => {
            let inner = lower_rust_ty(&p.elem, target_rust_name)?;
            Some(RustTy::Ptr {
                mutable: p.mutability.is_some(),
                inner: Box::new(inner),
            })
        }
        Type::Path(TypePath { qself: None, path })
            if path.segments.len() == 1
                && path.segments[0].arguments.is_none() =>
        {
            let name = path.segments[0].ident.to_string();
            if name == "Self" {
                return Some(RustTy::Record(target_rust_name.to_string()));
            }
            // Heuristic: bare uppercase-initial ident is a record
            // name. syn already ensures this is a valid ident; the
            // lookup resolves it to a ClassId at lowering time and
            // errors if it isn't a declared #[repr(cpp)] type.
            if name.starts_with(|c: char| c.is_ascii_uppercase()) {
                return Some(RustTy::Record(name));
            }
            None
        }
        _ => None,
    }
}

fn quote_type(ty: &Type) -> String {
    use quote_hack::ToSource as _;
    ty.to_source()
}

// Tiny stand-in for the `quote` crate's Display — we avoid adding the
// full `quote` dep by using `syn`'s token-stream printing. Good enough
// for error messages.
mod quote_hack {
    pub trait ToSource {
        fn to_source(&self) -> String;
    }
    impl<T: quote::ToTokens> ToSource for T {
        fn to_source(&self) -> String {
            let mut ts = proc_macro2::TokenStream::new();
            quote::ToTokens::to_tokens(self, &mut ts);
            ts.to_string()
        }
    }
}

// -------- Lowering into CxxTypeCtx -------------------------------------

/// Lower a parsed module into `CxxTypeCtx`, registering structs as
/// `TypeOrigin::RustReprCpp` classes and attaching `impl` block methods
/// to the right classes. Returns a map from Rust identifier to the
/// assigned `ClassId` so callers can resolve later references.
/// Lowering result: ids for structs and enums as they were registered
/// in the context. Most callers care about classes; tests that
/// exercise enum emission also read `enum_ids`.
#[derive(Debug, Default)]
pub struct LowerResult {
    pub class_ids: HashMap<String, ClassId>,
    pub enum_ids: HashMap<String, RustEnumId>,
}

/// Richer lowering result that also surfaces enum ids. Most callers
/// use `lower_into_ctx` which returns just the class-id map; the
/// `LowerResult` form is useful for emitters that want to cross-
/// reference specific enums they've just registered.
pub fn lower_module_into_ctx(
    module: &ParsedModule,
    ctx: &mut CxxTypeCtx,
) -> Result<LowerResult, AttrError> {
    // `lower_into_ctx` already registers enums via the shared
    // helper below. We re-derive the map here rather than having
    // `lower_into_ctx` return it so that the existing public
    // signature stays backwards-compatible.
    let class_ids = lower_into_ctx(module, ctx)?;
    // Enums were pushed in declaration order starting from whatever
    // the pre-lower count was. We don't know that count here without
    // tracking it ourselves, so look them up by name — O(n*m) in
    // practice the expected counts are tiny.
    let mut enum_ids = HashMap::new();
    for e in &module.enums {
        let id = ctx
            .rust_enum_ids()
            .find(|id| ctx.rust_enum(*id).rust_name == e.rust_name)
            .expect("enum was just registered");
        enum_ids.insert(e.rust_name.clone(), id);
    }
    Ok(LowerResult { class_ids, enum_ids })
}

fn to_rust_enum_def(
    ctx: &mut CxxTypeCtx,
    e: &ReprCppEnum,
) -> RustEnumDef {
    // Underlying default: i32.
    let underlying_scalar =
        e.underlying.unwrap_or(RustScalar::I32);
    let underlying = underlying_scalar.lower(ctx);
    RustEnumDef {
        rust_name: e.rust_name.clone(),
        cpp_name: e.cpp_name.clone(),
        underlying,
        variants: e
            .variants
            .iter()
            .map(|v| RustEnumVariant {
                name: v.name.clone(),
                discriminant: v.discriminant,
            })
            .collect(),
    }
}

pub fn lower_into_ctx(
    module: &ParsedModule,
    ctx: &mut CxxTypeCtx,
) -> Result<HashMap<String, ClassId>, AttrError> {
    let mut ids = HashMap::new();
    // Pass 1 — register every struct with empty field lists. This lets
    // later fields name *any* registered struct, regardless of source
    // order. Same shape as rustc's two-pass `resolve` + `typeck` for
    // HIR items.
    for s in &module.structs {
        let class_id = register_struct_shell(ctx, s);
        ids.insert(s.rust_name.clone(), class_id);
    }
    // Pass 2 — fill in field types (scalars + Records) now that every
    // target name exists in `ids`.
    for s in &module.structs {
        let class_id = ids[&s.rust_name];
        fill_struct_fields(ctx, class_id, s, &ids)?;
    }
    // Pass 3 — attach methods.
    for i in &module.impls {
        let class_id = *ids.get(&i.target_rust_name).ok_or_else(|| {
            AttrError::ImplTargetUnresolved {
                msg: format!(
                    "impl target `{}` is not a #[repr(cpp)] struct in this module",
                    i.target_rust_name
                ),
            }
        })?;
        attach_methods(ctx, class_id, &i.methods, &ids)?;
    }
    // Pass 4 — inject dtors for user-written `impl Drop`.
    for d in &module.drops {
        let class_id = *ids.get(&d.target_rust_name).ok_or_else(|| {
            AttrError::ImplTargetUnresolved {
                msg: format!(
                    "impl Drop target `{}` is not a #[repr(cpp)] struct in this module",
                    d.target_rust_name
                ),
            }
        })?;
        inject_dtor(ctx, class_id);
    }
    // Pass 5 — register Rust-origin enums. Enums don't cross-
    // reference classes in v1 (no enum-of-record, no records carrying
    // enum fields yet), so ordering is trivial.
    for e in &module.enums {
        let def = to_rust_enum_def(ctx, e);
        ctx.define_rust_enum(def);
    }
    Ok(ids)
}

/// Add a `SpecialMember::Dtor` method to the class if one isn't
/// already present. Idempotent — a user who writes both an explicit
/// `fn drop` in an inherent impl AND an `impl Drop` block still gets
/// a single dtor on the class (the Drop-derived one wins because it's
/// what the rustc fork codegen will actually emit; the inherent
/// `drop` method is ignored by C++ since the only destructor
/// discoverable through the Itanium ABI is the class's D1/D2).
fn inject_dtor(ctx: &mut CxxTypeCtx, class_id: ClassId) {
    // Snapshot the pieces we need from the read phase before we
    // acquire a mutable borrow via `intern_type`.
    let (already_has_dtor, cname) = {
        let class = ctx.class(class_id);
        (
            class
                .methods
                .iter()
                .any(|m| m.special == Some(SpecialMember::Dtor)),
            class_name(class),
        )
    };
    if already_has_dtor {
        return;
    }
    let void = ctx.intern_type(CxxType::Void);
    let dtor = MethodDef {
        name: MethodName::Ident(Ident(format!("~{cname}"))),
        sig: FnSig {
            params: vec![],
            ret: void,
            cv: rustc_abi_cxx::CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: true,
        },
        virtuality: Virtuality::NonVirtual,
        vtable_index: None,
        special: Some(SpecialMember::Dtor),
    };
    ctx.class_mut(class_id).methods.push(dtor);
}

fn class_name(class: &ClassDef) -> String {
    class
        .name
        .0
        .last()
        .and_then(|seg| match seg {
            NameSegment::Class(i) | NameSegment::Enum(i) => Some(i.0.clone()),
            NameSegment::TemplateSpec { name, .. } => Some(name.0.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "_".into())
}

fn register_struct_shell(
    ctx: &mut CxxTypeCtx,
    decl: &ReprCppStruct,
) -> ClassId {
    let name = NestedName(vec![NameSegment::Class(Ident(decl.cpp_name.clone()))]);
    let class = ClassDef {
        name,
        bases: vec![],
        fields: vec![],
        methods: vec![],
        kind: RecordKind::Struct,
        is_polymorphic: false,
        is_final: false,
        source_alignment: decl.align,
    };
    ctx.define_rust_class(class)
}

fn fill_struct_fields(
    ctx: &mut CxxTypeCtx,
    class_id: ClassId,
    decl: &ReprCppStruct,
    class_lookup: &HashMap<String, ClassId>,
) -> Result<(), AttrError> {
    let fields: Vec<FieldDef> = decl
        .fields
        .iter()
        .map(|f| -> Result<FieldDef, AttrError> {
            // C++ forbids reference members without in-class
            // initialization (and we never emit one). Reject them
            // with a pointer suggestion so the user knows what to
            // change to.
            if let RustTy::Ref { .. } = &f.ty {
                return Err(AttrError::UnsupportedType {
                    hint: format!(
                        "field `{}::{}`",
                        decl.rust_name, f.name
                    ),
                    rust_source:
                        "reference fields are not supported in v1 — \
                         use a raw pointer (`*const T` / `*mut T`) \
                         instead"
                            .into(),
                });
            }
            if let RustTy::Record(name) = &f.ty {
                let tgt = class_lookup.get(name).copied().ok_or_else(|| {
                    AttrError::UnsupportedType {
                        hint: format!(
                            "field `{}::{}`",
                            decl.rust_name, f.name
                        ),
                        rust_source: format!(
                            "unresolved record `{name}` — only locally-declared #[repr(cpp)] types can be named by v1"
                        ),
                    }
                })?;
                if tgt == class_id {
                    return Err(AttrError::UnsupportedType {
                        hint: format!(
                            "field `{}::{}`",
                            decl.rust_name, f.name
                        ),
                        rust_source: format!(
                            "self-referential field `{name}` would make the struct infinite-size; use a pointer or box (not yet supported by v1)"
                        ),
                    });
                }
            }
            let ty = f.ty.lower(ctx, class_lookup)?;
            Ok(FieldDef {
                name: Ident(f.name.clone()),
                ty,
                explicit_align: None,
            })
        })
        .collect::<Result<_, _>>()?;
    ctx.class_mut(class_id).fields = fields;
    Ok(())
}

fn attach_methods(
    ctx: &mut CxxTypeCtx,
    class_id: ClassId,
    methods: &[ReprCppMethod],
    class_lookup: &HashMap<String, ClassId>,
) -> Result<(), AttrError> {
    let defs: Vec<MethodDef> = methods
        .iter()
        .map(|m| lower_method_def(ctx, m, class_lookup))
        .collect::<Result<_, _>>()?;
    ctx.class_mut(class_id).methods.extend(defs);
    Ok(())
}

fn lower_method_def(
    ctx: &mut CxxTypeCtx,
    m: &ReprCppMethod,
    class_lookup: &HashMap<String, ClassId>,
) -> Result<MethodDef, AttrError> {
    let ret = m.ret.lower(ctx, class_lookup)?;
    let params = m
        .params
        .iter()
        .map(|p| p.lower(ctx, class_lookup))
        .collect::<Result<_, _>>()?;
    let sig = FnSig {
        params,
        ret,
        cv: rustc_abi_cxx::CvQual { is_const: m.is_const, is_volatile: false },
        ref_q: None,
        variadic: false,
        noexcept: true,
    };
    let special = if m.rust_name == "new" && matches!(m.receiver, Receiver::None)
    {
        // A no-receiver `new` function is a common Rust idiom that we
        // expose to C++ as a constructor (`OtherCtor`). The generated
        // header emits `Foo(args);`. The ctor body is emitted by the
        // rustc fork as a call to the mangled `new` fn.
        Some(SpecialMember::OtherCtor)
    } else {
        None
    };
    Ok(MethodDef {
        name: MethodName::Ident(Ident(m.cpp_name.clone())),
        sig,
        virtuality: Virtuality::NonVirtual,
        vtable_index: None,
        special,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> ParsedModule {
        parse_source(src).expect("parse ok")
    }

    #[test]
    fn plain_repr_cpp_struct_with_scalar_fields() {
        let m = parse(
            r#"
            #[repr(cpp)]
            pub struct Point {
                pub x: i32,
                pub y: i32,
            }
            "#,
        );
        assert_eq!(m.structs.len(), 1);
        assert_eq!(m.impls.len(), 0);
        let s = &m.structs[0];
        assert_eq!(s.rust_name, "Point");
        assert_eq!(s.cpp_name, "Point");
        assert_eq!(s.align, None);
        assert_eq!(s.fields.len(), 2);
        assert_eq!(s.fields[0].name, "x");
        assert_eq!(s.fields[0].ty, RustTy::Scalar(RustScalar::I32));
    }

    #[test]
    fn cpp_class_attribute_recognized_as_entry_marker() {
        let m = parse(
            r#"
            #[cpp_class]
            pub struct Point { pub x: i32, pub y: i32 }
            "#,
        );
        assert_eq!(m.structs.len(), 1);
        assert_eq!(m.structs[0].rust_name, "Point");
    }

    #[test]
    fn cpp_class_with_repr_c_still_recognized() {
        // What the proc-macro expansion looks like to the scanner.
        let m = parse(
            r#"
            #[cpp_class]
            #[repr(C)]
            pub struct Point { pub x: i32, pub y: i32 }
            "#,
        );
        assert_eq!(m.structs.len(), 1);
    }

    #[test]
    fn cpp_class_with_cpp_name_override_takes_the_override() {
        let m = parse(
            r#"
            #[cpp_class]
            #[cpp_name = "CppPoint"]
            pub struct Point { pub x: i32 }
            "#,
        );
        let s = &m.structs[0];
        assert_eq!(s.rust_name, "Point");
        assert_eq!(s.cpp_name, "CppPoint");
    }

    #[test]
    fn cpp_name_override() {
        let m = parse(
            r#"
            #[repr(cpp)]
            #[cpp_name = "CppPoint"]
            pub struct Point { pub x: f32 }
            "#,
        );
        let s = &m.structs[0];
        assert_eq!(s.rust_name, "Point");
        assert_eq!(s.cpp_name, "CppPoint");
    }

    #[test]
    fn align_from_repr_align() {
        let m = parse(
            r#"
            #[repr(cpp, align(16))]
            pub struct Vec4 { pub a: f32, pub b: f32, pub c: f32, pub d: f32 }
            "#,
        );
        assert_eq!(m.structs[0].align, Some(16));
    }

    #[test]
    fn non_cpp_structs_are_skipped() {
        let m = parse(
            r#"
            pub struct Normal { pub x: i32 }
            #[repr(C)] pub struct PlainC { pub y: u8 }
            "#,
        );
        assert_eq!(m.structs.len(), 0);
    }

    #[test]
    fn repr_cpp_c_is_rejected() {
        let err = parse_source(
            r#"
            #[repr(cpp, C)]
            pub struct Bad { pub x: i32 }
            "#,
        )
        .unwrap_err();
        matches!(err, AttrError::BadRepr { .. });
    }

    #[test]
    fn impl_block_captures_methods() {
        let m = parse(
            r#"
            #[repr(cpp)]
            pub struct Point { pub x: i32, pub y: i32 }
            impl Point {
                pub fn magnitude_sq(&self) -> i32 { self.x * self.x + self.y * self.y }
                pub fn set_x(&mut self, v: i32) {}
                pub fn new(x: i32, y: i32) -> Point { Point { x, y } }
                fn private_thing(&self) -> i32 { 0 }
            }
            "#,
        );
        assert_eq!(m.impls.len(), 1);
        let i = &m.impls[0];
        assert_eq!(i.target_rust_name, "Point");
        // Private method is filtered out.
        let names: Vec<&str> =
            i.methods.iter().map(|mm| mm.rust_name.as_str()).collect();
        assert_eq!(names, vec!["magnitude_sq", "set_x", "new"]);

        assert_eq!(i.methods[0].receiver, Receiver::RefSelf);
        assert!(i.methods[0].is_const);
        assert_eq!(i.methods[1].receiver, Receiver::RefMutSelf);
        assert!(!i.methods[1].is_const);
        assert_eq!(i.methods[2].receiver, Receiver::None);
    }

    #[test]
    fn lower_registers_struct_as_rust_origin() {
        use rustc_abi_cxx::{Target, TypeOrigin};
        let m = parse(
            r#"
            #[repr(cpp)]
            pub struct Point { pub x: i32, pub y: i32 }
            "#,
        );
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let ids = lower_into_ctx(&m, &mut ctx).unwrap();
        let id = ids["Point"];
        assert_eq!(ctx.class_origin(id), TypeOrigin::RustReprCpp);
        // Layout sanity: two i32 = 8 bytes, align 4.
        let layout = ctx.layout(id).unwrap();
        assert_eq!(layout.size_bytes, 8);
        assert_eq!(layout.align_bytes, 4);
        assert_eq!(layout.field_offsets, vec![0, 4]);
    }

    #[test]
    fn lower_attaches_impl_methods() {
        use rustc_abi_cxx::Target;
        let m = parse(
            r#"
            #[repr(cpp)]
            pub struct Point { pub x: i32, pub y: i32 }
            impl Point {
                pub fn mag2(&self) -> i32 { 0 }
                pub fn new(x: i32, y: i32) -> Point { Point { x, y } }
            }
            "#,
        );
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let ids = lower_into_ctx(&m, &mut ctx).unwrap();
        let class = ctx.class(ids["Point"]);
        assert_eq!(class.methods.len(), 2);
        // `new` is sugared to OtherCtor so the emitter produces a ctor.
        let new_m = class
            .methods
            .iter()
            .find(|mm| mm.name.ident_name() == Some("new"))
            .expect("new exists");
        assert_eq!(
            new_m.special,
            Some(rustc_abi_cxx::SpecialMember::OtherCtor)
        );
        let mag2 = class
            .methods
            .iter()
            .find(|mm| mm.name.ident_name() == Some("mag2"))
            .unwrap();
        assert!(mag2.sig.cv.is_const);
        assert!(mag2.special.is_none());
    }

    #[test]
    fn diagnostic_hint_includes_line_and_column() {
        // The offending field is on line 4 (1-indexed) — verify the
        // error's hint string includes that line number.
        let src = "\
// line 1
#[repr(cpp)]
pub struct Bad {
    pub r: Vec<i32>,
}
";
        // `Vec<i32>` isn't in the accepted surface (generic types
        // aren't supported in v1), so the field fails lowering. The
        // hint should carry the span of that field.
        let err = parse_source(src).unwrap_err();
        match err {
            AttrError::UnsupportedType { hint, .. } => {
                assert!(
                    hint.contains("line 4"),
                    "expected line-4 annotation, got: {hint}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn impl_target_error_includes_span() {
        let src = "\
#[repr(cpp)]
pub struct Foo { pub x: i32 }

impl ::external::Foo {
    pub fn f(&self) {}
}
";
        let err = parse_source(src).unwrap_err();
        match err {
            AttrError::ImplTargetUnresolved { msg } => {
                assert!(
                    msg.contains("line 4"),
                    "expected line-4 annotation, got: {msg}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unsupported_field_type_surfaces_error() {
        // `Vec<i32>` has an unsupported syn::PathArguments (generic
        // args), so lower_rust_ty returns None and the field fails
        // lowering.
        let err = parse_source(
            r#"
            #[repr(cpp)]
            pub struct HasVec { pub v: Vec<i32> }
            "#,
        );
        match err {
            Err(AttrError::UnsupportedType { .. }) | Err(AttrError::Syn(_)) => {
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn fields_may_reference_other_repr_cpp_types() {
        use rustc_abi_cxx::{CxxType, Target};
        let m = parse(
            r#"
            #[repr(cpp)]
            pub struct Point { pub x: i32, pub y: i32 }

            #[repr(cpp)]
            pub struct Segment {
                pub start: Point,
                pub end: Point,
                pub thickness: i32,
            }
            "#,
        );
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let ids = lower_into_ctx(&m, &mut ctx).unwrap();

        let point_id = ids["Point"];
        let seg_id = ids["Segment"];
        let seg_class = ctx.class(seg_id);
        assert_eq!(seg_class.fields.len(), 3);
        // start/end carry the record type; thickness is a scalar.
        match ctx.type_of(seg_class.fields[0].ty) {
            CxxType::Record(id) => assert_eq!(*id, point_id),
            other => panic!("expected Record, got {other:?}"),
        }
        // Layout: two 8-byte Points (size 8, align 4) + 1 i32 = 20;
        // alignment of the whole struct is 4 (no stricter member).
        let layout = ctx.layout(seg_id).unwrap();
        assert_eq!(layout.size_bytes, 20);
        assert_eq!(layout.align_bytes, 4);
        assert_eq!(layout.field_offsets, vec![0, 8, 16]);
    }

    #[test]
    fn fields_can_reference_a_struct_declared_later() {
        use rustc_abi_cxx::Target;
        // Forward reference — Segment mentions Point before Point is
        // declared. Two-pass lowering handles this.
        let m = parse(
            r#"
            #[repr(cpp)]
            pub struct Segment {
                pub start: Point,
                pub end: Point,
            }

            #[repr(cpp)]
            pub struct Point { pub x: i32, pub y: i32 }
            "#,
        );
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let ids = lower_into_ctx(&m, &mut ctx).unwrap();
        let layout = ctx.layout(ids["Segment"]).unwrap();
        assert_eq!(layout.size_bytes, 16);
    }

    #[test]
    fn self_referential_field_is_rejected() {
        use rustc_abi_cxx::Target;
        let m = parse_source(
            r#"
            #[repr(cpp)]
            pub struct Node { pub child: Node }
            "#,
        )
        .unwrap();
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let err = lower_into_ctx(&m, &mut ctx).unwrap_err();
        match err {
            AttrError::UnsupportedType { rust_source, .. } => {
                assert!(
                    rust_source.contains("self-referential"),
                    "msg: {rust_source}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn field_referencing_unknown_struct_errors() {
        use rustc_abi_cxx::Target;
        let m = parse_source(
            r#"
            #[repr(cpp)]
            pub struct Seg { pub start: Unknown }
            "#,
        )
        .unwrap();
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let err = lower_into_ctx(&m, &mut ctx).unwrap_err();
        match err {
            AttrError::UnsupportedType { rust_source, .. } => {
                assert!(
                    rust_source.contains("unresolved record `Unknown`"),
                    "msg: {rust_source}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn methods_accept_reference_and_pointer_types() {
        use rustc_abi_cxx::{CxxType, RefKind, Target};
        let m = parse(
            r#"
            #[repr(cpp)]
            pub struct Buf { pub n: i32 }
            impl Buf {
                pub fn peek(&self, idx: *const i8) -> *const i8 { idx }
                pub fn fill(&mut self, data: *mut u8, len: i32) {}
                pub fn swap(&self, other: &Buf) {}
            }
            "#,
        );
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let ids = lower_into_ctx(&m, &mut ctx).unwrap();
        let class = ctx.class(ids["Buf"]);

        // peek: (const char*) -> const char*.
        let peek = class
            .methods
            .iter()
            .find(|mm| mm.name.ident_name() == Some("peek"))
            .unwrap();
        match ctx.type_of(peek.sig.params[0]) {
            CxxType::Ptr { cv, .. } => {
                assert!(cv.is_const, "*const i8 should be const T*");
            }
            other => panic!("expected Ptr, got {other:?}"),
        }
        match ctx.type_of(peek.sig.ret) {
            CxxType::Ptr { cv, .. } => assert!(cv.is_const),
            other => panic!("expected Ptr return, got {other:?}"),
        }

        // fill: (uint8_t*, int32_t) -> void. The *mut u8 must be
        // non-const.
        let fill = class
            .methods
            .iter()
            .find(|mm| mm.name.ident_name() == Some("fill"))
            .unwrap();
        match ctx.type_of(fill.sig.params[0]) {
            CxxType::Ptr { cv, .. } => assert!(!cv.is_const),
            other => panic!("expected Ptr, got {other:?}"),
        }

        // swap: (Buf const&) — `&Buf` maps to a const lvalue ref.
        let swap = class
            .methods
            .iter()
            .find(|mm| mm.name.ident_name() == Some("swap"))
            .unwrap();
        match ctx.type_of(swap.sig.params[0]) {
            CxxType::Ref { kind, cv, .. } => {
                assert_eq!(*kind, RefKind::Lvalue);
                assert!(cv.is_const, "&Buf should be const T&");
            }
            other => panic!("expected Ref, got {other:?}"),
        }
    }

    #[test]
    fn mut_reference_maps_to_non_const_cpp_reference() {
        use rustc_abi_cxx::{CxxType, Target};
        let m = parse(
            r#"
            #[repr(cpp)]
            pub struct Buf { pub n: i32 }
            impl Buf {
                pub fn absorb(&mut self, src: &mut Buf) {}
            }
            "#,
        );
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let ids = lower_into_ctx(&m, &mut ctx).unwrap();
        let absorb = ctx
            .class(ids["Buf"])
            .methods
            .iter()
            .find(|mm| mm.name.ident_name() == Some("absorb"))
            .unwrap();
        match ctx.type_of(absorb.sig.params[0]) {
            CxxType::Ref { cv, .. } => assert!(!cv.is_const),
            other => panic!("expected Ref, got {other:?}"),
        }
    }

    #[test]
    fn reference_fields_are_rejected_with_pointer_suggestion() {
        use rustc_abi_cxx::Target;
        let m = parse_source(
            r#"
            #[repr(cpp)]
            pub struct Bad { pub r: &'static i32 }
            "#,
        )
        .unwrap();
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let err = lower_into_ctx(&m, &mut ctx).unwrap_err();
        match err {
            AttrError::UnsupportedType { rust_source, .. } => {
                assert!(
                    rust_source.contains("raw pointer"),
                    "msg: {rust_source}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn impl_drop_injects_a_dtor_method() {
        use rustc_abi_cxx::{SpecialMember, Target};
        let m = parse(
            r#"
            #[repr(cpp)]
            pub struct Resource { pub handle: i64 }

            impl Drop for Resource {
                fn drop(&mut self) { /* close handle */ }
            }
            "#,
        );
        assert_eq!(m.drops.len(), 1);
        assert_eq!(m.drops[0].target_rust_name, "Resource");

        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let ids = lower_into_ctx(&m, &mut ctx).unwrap();
        let class = ctx.class(ids["Resource"]);
        let dtor = class
            .methods
            .iter()
            .find(|mm| mm.special == Some(SpecialMember::Dtor))
            .expect("dtor injected");
        // Methods named with the ~ prefix so downstream emitters know
        // it's the destructor.
        assert!(
            dtor.name.ident_name().unwrap_or("").starts_with('~'),
            "expected ~ prefix, got: {:?}",
            dtor.name
        );
    }

    #[test]
    fn fully_qualified_drop_impl_is_recognized() {
        let m = parse(
            r#"
            #[repr(cpp)]
            pub struct R { pub x: i32 }
            impl ::core::ops::Drop for R {
                fn drop(&mut self) {}
            }
            "#,
        );
        assert_eq!(m.drops.len(), 1);
    }

    #[test]
    fn non_drop_trait_impls_are_still_ignored() {
        // Make sure we didn't accidentally match any trait impl.
        let m = parse(
            r#"
            #[repr(cpp)]
            pub struct P { pub x: i32 }
            impl Default for P {
                fn default() -> Self { P { x: 0 } }
            }
            "#,
        );
        assert!(m.drops.is_empty());
        assert!(m.impls.is_empty());
    }

    #[test]
    fn impl_drop_on_non_repr_cpp_type_errors() {
        use rustc_abi_cxx::Target;
        // Drop impl targeting a struct that was never declared with
        // #[repr(cpp)] in this module — can't resolve to a class.
        let m = parse(
            r#"
            impl Drop for Ghost {
                fn drop(&mut self) {}
            }
            "#,
        );
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let err = lower_into_ctx(&m, &mut ctx).unwrap_err();
        assert!(matches!(err, AttrError::ImplTargetUnresolved { .. }));
    }

    #[test]
    fn pointer_fields_are_allowed() {
        use rustc_abi_cxx::{CxxType, Target};
        let m = parse(
            r#"
            #[repr(cpp)]
            pub struct Node {
                pub value: i32,
                pub next: *mut Node,
            }
            "#,
        );
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let ids = lower_into_ctx(&m, &mut ctx).unwrap();
        let node = ctx.class(ids["Node"]);
        assert_eq!(node.fields.len(), 2);
        match ctx.type_of(node.fields[1].ty) {
            CxxType::Ptr { cv, .. } => {
                assert!(!cv.is_const, "*mut → non-const");
            }
            other => panic!("expected Ptr, got {other:?}"),
        }
        // Self-referential pointer is ok — gets correct size.
        let layout = ctx.layout(ids["Node"]).unwrap();
        // On x86_64: i32 (4) + padding (4) + ptr (8) = 16.
        assert_eq!(layout.size_bytes, 16);
        assert_eq!(layout.align_bytes, 8);
    }

    #[test]
    fn repr_cpp_enum_is_captured() {
        let m = parse(
            r#"
            #[repr(cpp)]
            pub enum Color { Red, Green, Blue }
            "#,
        );
        assert_eq!(m.enums.len(), 1);
        let e = &m.enums[0];
        assert_eq!(e.rust_name, "Color");
        assert_eq!(e.underlying, None);
        assert_eq!(e.variants.len(), 3);
        assert_eq!(e.variants[0].name, "Red");
        assert_eq!(e.variants[0].discriminant, None);
    }

    #[test]
    fn cpp_class_enum_is_also_captured() {
        let m = parse(
            r#"
            #[cpp_class]
            pub enum Status { Ok, Err }
            "#,
        );
        assert_eq!(m.enums.len(), 1);
        assert_eq!(m.enums[0].rust_name, "Status");
    }

    #[test]
    fn enum_with_explicit_discriminants_preserves_them() {
        let m = parse(
            r#"
            #[repr(cpp)]
            pub enum Mode { Off = 0, On = 1, Auto = 42 }
            "#,
        );
        let e = &m.enums[0];
        assert_eq!(e.variants[0].discriminant, Some(0));
        assert_eq!(e.variants[1].discriminant, Some(1));
        assert_eq!(e.variants[2].discriminant, Some(42));
    }

    #[test]
    fn repr_i16_pins_underlying_width() {
        let m = parse(
            r#"
            #[repr(cpp, i16)]
            pub enum Small { A, B }
            "#,
        );
        assert_eq!(m.enums[0].underlying, Some(RustScalar::I16));
    }

    #[test]
    fn variants_with_fields_are_rejected() {
        // Rust allows `enum E { V(u32) }` but we can't represent
        // Rust-tagged-union semantics as a C++ scoped enum.
        let err = parse_source(
            r#"
            #[repr(cpp)]
            pub enum E { Plain, Tagged(u32) }
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, AttrError::UnsupportedType { .. }));
    }

    #[test]
    fn non_literal_discriminant_is_rejected() {
        let err = parse_source(
            r#"
            #[repr(cpp)]
            pub enum E { V = 1 + 1 }
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, AttrError::UnsupportedType { .. }));
    }

    #[test]
    fn lower_registers_enums_in_ctx() {
        use rustc_abi_cxx::Target;
        let m = parse(
            r#"
            #[repr(cpp)]
            pub enum Color { Red, Green, Blue }
            "#,
        );
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let _ = lower_into_ctx(&m, &mut ctx).unwrap();
        let ids: Vec<_> = ctx.rust_enum_ids().collect();
        assert_eq!(ids.len(), 1);
        let e = ctx.rust_enum(ids[0]);
        assert_eq!(e.rust_name, "Color");
        assert_eq!(e.variants.len(), 3);
    }

    #[test]
    fn impl_target_must_be_bare_identifier() {
        let err = parse_source(
            r#"
            #[repr(cpp)]
            pub struct Foo { pub x: i32 }
            impl ::some::path::Foo {
                pub fn thing(&self) -> i32 { 0 }
            }
            "#,
        )
        .unwrap_err();
        match err {
            AttrError::ImplTargetUnresolved { .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
