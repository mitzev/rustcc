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
//! # Scope (v0 scaffold)
//!
//! - One backend implemented: [`BindingsBackend::NativeCppClassMacro`].
//!   It writes one `::rustcc_macros::native_cpp_class! { … }` per
//!   imported class. The macro is fork-only — it depends on the
//!   compiler's `extern "C++"` ABI (P09.50) and unified Itanium
//!   mangler. The other backends ([`BindingsBackend::CxxClassMacro`],
//!   [`BindingsBackend::DirectExternCpp`]) return
//!   [`BindingsError::UnsupportedBackend`] until they're built out.
//! - Method shapes covered: ctors, dtors (rare, usually auto-Drop),
//!   const + non-const instance methods, static methods.
//! - Parameter / return types covered: scalars (`int*`, `float*`,
//!   `bool`, `void`) and raw pointer/reference wrappings of the same.
//!   Records-by-value, operator names, templates, virtuals, and
//!   inheritance return [`BindingsError::UnsupportedType`] /
//!   [`BindingsError::UnsupportedMethod`].
//! - No namespace scoping. The current importer flattens all classes
//!   to top-level (see `import.rs`'s self-doc). When namespace
//!   recovery lands, this emitter grows a `mod` tree.
//!
//! # Output shape
//!
//! ```ignore
//! pub mod imported {  // (only when `crate_module` is set)
//!     ::rustcc_macros::native_cpp_class! {
//!         #[size = 8]
//!         #[align = 4]
//!         pub class Point {
//!             #[ctor] fn new(x: i32, y: i32) -> Self;
//!             fn get_x(&self) -> i32;
//!             fn translated(&self, dx: i32, dy: i32) -> Point;
//!         }
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

use std::fmt::Write as _;

use rustc_abi_cxx::{
    ClassId, CxxType, CxxTypeCtx, FloatKind, IntWidth, MethodDef,
    MethodName, SpecialMember, TypeId, Virtuality,
};

/// Selects the surface the emitter writes against. See module-level
/// docs for the per-backend contract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BindingsBackend {
    /// Fork-only: emit `native_cpp_class! { … }` invocations. Default
    /// because rustcc users opt into the fork; it produces the most
    /// compact output and rides on P09's per-target ABI.
    #[default]
    NativeCppClassMacro,
    /// Stock-rustc-friendly: emit `cxx_class! { … }` invocations.
    /// Compiles on stable rustc at the cost of the explicit-`__sret`
    /// trampoline trick (correct on x86_64 SysV only). **TODO**.
    CxxClassMacro,
    /// Auditable raw form: emit `#[repr(cpp)]` structs +
    /// `unsafe extern "C++" { … }` blocks + `impl` wrappers, no
    /// macros. Fork-only. **TODO**.
    DirectExternCpp,
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
        backend @ (BindingsBackend::CxxClassMacro | BindingsBackend::DirectExternCpp) => {
            Err(BindingsError::UnsupportedBackend {
                backend,
                reason: "v0 scaffold ships only `NativeCppClassMacro`; the \
                         other backends are tracked for follow-up releases"
                    .into(),
            })
        }
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
    fn native_macro_emits_class_block_with_size_align_and_methods() {
        let (ctx, id) = point_ctx();
        let src = generate_rust_bindings(
            &ctx,
            &[id],
            &RustBindingsConfig::default(),
        )
        .expect("emit");

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
    fn unsupported_backend_returns_clear_error() {
        let (ctx, id) = point_ctx();
        for backend in [BindingsBackend::CxxClassMacro, BindingsBackend::DirectExternCpp] {
            let cfg = RustBindingsConfig {
                backend,
                ..RustBindingsConfig::default()
            };
            let err = generate_rust_bindings(&ctx, &[id], &cfg).unwrap_err();
            assert!(
                matches!(err, BindingsError::UnsupportedBackend { .. }),
                "expected UnsupportedBackend for {backend:?}, got {err:?}"
            );
        }
    }
}
