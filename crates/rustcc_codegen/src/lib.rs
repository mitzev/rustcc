//! `rustcc_codegen` — adapter between a rustc fork and `rustc_abi_cxx`.
//!
//! # Scope
//!
//! This crate is the seam where the rustc fork meets the rustcc IR.
//! The fork delegates layout, mangling, and cross-language call
//! lowering through the traits defined here; implementations live in
//! the fork (wrapping `TyCtxt`) or in this crate's tests (in-memory
//! fakes). The v1 scope matches `docs/codegen.md §3–§4`:
//!
//! - Layout queries for `#[repr(cpp)]` Rust types and imported C++
//!   classes (both stored as `ClassDef` in `CxxTypeCtx`; origins
//!   distinguish them).
//! - Itanium mangling via `rustc_abi_cxx::mangle`.
//! - Lowering a direct method call on a C++ class (from Rust) into an
//!   extern-C shim invocation: prepends `this`, selects between
//!   in-reg/sret return conventions, and hands back a [`LoweredCall`]
//!   describing the lowered argv. The rustc fork takes that struct
//!   and emits LLVM IR against it.
//!
//! # Non-scope
//!
//! - Actual LLVM IR emission — that lives in the fork.
//! - Virtual dispatch (v1.5 per `docs/codegen.md §5`).
//! - Constructor/destructor lifecycle (tracked against
//!   `ownership_and_safety.md`).
//! - Rust → C++ direction calls (C++ calling a Rust `#[repr(cpp)]`
//!   method); the hpp side is wired, the codegen side awaits the fork.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod fake_tyctxt;

use std::collections::HashMap;

use rustc_abi_cxx::{
    ClassId, CtorVariant, CxxType, CxxTypeCtx, DtorVariant, FnSig, Ident,
    LayoutError, MethodDef, MethodName, NestedName, RecordLayout,
    SpecialMember, Symbol, TypeId,
};

// -------- Provider traits -----------------------------------------------

/// Query the layout of a class by id. Both imported C++ classes and
/// Rust-declared `#[repr(cpp)]` classes answer through the same
/// interface — the implementation just forwards to
/// `CxxTypeCtx::layout`. Splitting it into a trait lets the fork
/// substitute a cached implementation without changing callers.
pub trait LayoutProvider {
    fn layout_of(&self, class: ClassId) -> Result<RecordLayout, LayoutError>;
}

impl LayoutProvider for CxxTypeCtx {
    fn layout_of(&self, class: ClassId) -> Result<RecordLayout, LayoutError> {
        self.layout(class)
    }
}

/// Produce a mangled symbol for a rustcc `Symbol`. Every interop
/// symbol — class methods, ctors, dtors, vtables, RTTI — flows
/// through this trait so the rustc fork's linker drives the same
/// names `rustc_abi_cxx` computes.
pub trait ManglerProvider {
    fn mangle(&self, symbol: &Symbol) -> String;
}

impl ManglerProvider for CxxTypeCtx {
    fn mangle(&self, symbol: &Symbol) -> String {
        CxxTypeCtx::mangle(self, symbol)
    }
}

/// Resolve a method name on a class to its `MethodDef`, a disambig
/// index, and its mangled ABI symbol. Wraps the common lookup that
/// every call site needs and keeps overload disambiguation in one
/// place.
pub trait ShimResolver {
    /// Look up a non-virtual method on `class` by its identifier name.
    /// Returns the first match today; overload resolution per
    /// `docs/cxx_importer.md §7` happens in a future revision.
    fn resolve_method<'ctx>(
        &'ctx self,
        class: ClassId,
        method_name: &str,
    ) -> Option<ResolvedMethod<'ctx>>;
}

pub struct ResolvedMethod<'ctx> {
    pub method: &'ctx MethodDef,
    /// Mangled ABI symbol of the method (not the shim).
    pub symbol: String,
    /// Shim symbol: `__rustcc_shim_<mangled>` per
    /// `docs/exception_boundary.md §2`.
    pub shim_symbol: String,
}

/// Default implementation for a `CxxTypeCtx`: linear scan through
/// `class.methods`. The rustc fork will use the same logic but drive
/// it off HIR definitions once `extern "C++"` paths are resolved.
pub struct CtxShimResolver<'ctx> {
    pub ctx: &'ctx CxxTypeCtx,
}

impl<'ctx> ShimResolver for CtxShimResolver<'ctx> {
    fn resolve_method<'a>(
        &'a self,
        class: ClassId,
        method_name: &str,
    ) -> Option<ResolvedMethod<'a>> {
        let class_def = self.ctx.class(class);
        for method in &class_def.methods {
            if let MethodName::Ident(i) = &method.name {
                if i.0 == method_name {
                    let sym = method_symbol_for(self.ctx, class, method);
                    let mangled = self.ctx.mangle(&sym);
                    let shim = format!("__rustcc_shim_{mangled}");
                    return Some(ResolvedMethod {
                        method,
                        symbol: mangled,
                        shim_symbol: shim,
                    });
                }
            }
        }
        None
    }
}

// -------- Call lowering -------------------------------------------------

/// Abstract "where does the return value live" decision. The v1
/// heuristic matches the Itanium rules we care about: (a) trivially-
/// copyable types ≤ 16 bytes return in registers, (b) anything larger
/// or non-trivial flows through sret (caller-provided output slot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnConvention {
    /// Return is in-register (void counts as this; caller ignores).
    ByValue,
    /// Return is via an sret output pointer prepended to argv.
    Sret { slot_align: u64, slot_size: u64 },
}

/// A method call ready for the rustc fork to lower to LLVM IR. This
/// is the output of `lower_method_call` — the fork takes this struct,
/// materializes the caller-side values, and writes a `call` to the
/// shim symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredCall {
    /// Shim symbol to call: `__rustcc_shim_<mangled>`.
    pub shim_symbol: String,
    /// Mangled target (for debugging / relocation tables / linker
    /// diagnostics).
    pub target_symbol: String,
    /// Return convention decided during lowering.
    pub ret_conv: ReturnConvention,
    /// Argument slots in ABI order. The first one is the `this`
    /// pointer for non-static method calls; if `ret_conv` is `Sret`,
    /// the sret slot precedes `this` (Itanium lowering sends the sret
    /// first, then `this`, then formals).
    pub argv: Vec<ArgSlot>,
    /// Number of Rust-visible formal arguments (for the fork to
    /// cross-check against the method's sig during IR emission).
    pub formal_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgSlot {
    /// Caller-provided sret slot holding the method's return value.
    SretSlot,
    /// `this` pointer — a pointer to the receiver object.
    ThisPtr,
    /// One formal argument, by positional index in the method's
    /// Rust-visible signature.
    Formal { index: usize, ty: TypeId },
}

#[derive(Debug, Clone)]
pub enum LowerError {
    MethodNotFound {
        class: ClassId,
        method: String,
    },
    Layout(LayoutError),
    UnsupportedVirtual {
        class: ClassId,
        method: String,
    },
    /// A ctor-specific lowering was requested on a method whose
    /// `special` field says it isn't a ctor. Callers shouldn't rely
    /// on `lower_ctor_call` to classify methods; they should already
    /// know which ones are ctors.
    NotACtor {
        class: ClassId,
        method: String,
    },
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MethodNotFound { class, method } => write!(
                f,
                "method `{method}` not found on class id {class:?}",
            ),
            Self::Layout(e) => write!(f, "layout: {e:?}"),
            Self::UnsupportedVirtual { method, .. } => write!(
                f,
                "virtual method `{method}` is not supported in v1 codegen",
            ),
            Self::NotACtor { method, .. } => write!(
                f,
                "method `{method}` is not a ctor — use lower_method_call instead",
            ),
        }
    }
}

impl std::error::Error for LowerError {}

impl From<LayoutError> for LowerError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// Lower a non-virtual instance-method call. Returns the data the
/// fork needs to emit LLVM IR.
///
/// `argv` shape: `[sret?, this, formals...]`. Constructor / destructor
/// / static / free-function calls use dedicated entry points
/// ([`lower_ctor_call`], [`lower_dtor_call`], [`lower_static_call`],
/// [`lower_free_fn_call`]) so the argv shape matches their Itanium
/// conventions exactly.
pub fn lower_method_call(
    ctx: &CxxTypeCtx,
    class: ClassId,
    method_name: &str,
) -> Result<LoweredCall, LowerError> {
    let resolved = resolve_or_err(ctx, class, method_name)?;
    reject_virtual(resolved.method, class, method_name)?;
    let ret_conv = classify_return(ctx, resolved.method.sig.ret)?;
    let mut argv = Vec::new();
    if matches!(ret_conv, ReturnConvention::Sret { .. }) {
        argv.push(ArgSlot::SretSlot);
    }
    argv.push(ArgSlot::ThisPtr);
    append_formals(&mut argv, &resolved.method.sig.params);

    Ok(LoweredCall {
        shim_symbol: resolved.shim_symbol,
        target_symbol: resolved.symbol,
        ret_conv,
        argv,
        formal_count: resolved.method.sig.params.len(),
    })
}

/// Lower a static-member call — a method declared on a class but
/// without a `this` parameter. Mangling is `Symbol::Method` (it lives
/// in the class's nested-name scope) but argv has no `this`.
///
/// Because the IR today doesn't carry a static flag on `MethodDef`,
/// callers are responsible for ensuring `method_name` really is a
/// static method. A misuse won't corrupt memory — the emitted shim
/// will just have a wrong argv shape and fail at the shim-side
/// compilation step, which is loud and recoverable.
pub fn lower_static_call(
    ctx: &CxxTypeCtx,
    class: ClassId,
    method_name: &str,
) -> Result<LoweredCall, LowerError> {
    let resolved = resolve_or_err(ctx, class, method_name)?;
    reject_virtual(resolved.method, class, method_name)?;
    let ret_conv = classify_return(ctx, resolved.method.sig.ret)?;
    let mut argv = Vec::new();
    if matches!(ret_conv, ReturnConvention::Sret { .. }) {
        argv.push(ArgSlot::SretSlot);
    }
    append_formals(&mut argv, &resolved.method.sig.params);
    Ok(LoweredCall {
        shim_symbol: resolved.shim_symbol,
        target_symbol: resolved.symbol,
        ret_conv,
        argv,
        formal_count: resolved.method.sig.params.len(),
    })
}

/// Lower a ctor call. The resolved method's special member must be a
/// ctor; argv starts with `this` (complete-object address the caller
/// provides — typically a stack slot or `CxxOwned<T>`'s storage).
/// Return is void; sret never applies. Mangling uses Itanium C1
/// (complete-object ctor).
pub fn lower_ctor_call(
    ctx: &CxxTypeCtx,
    class: ClassId,
    method_name: &str,
) -> Result<LoweredCall, LowerError> {
    let resolved = resolve_or_err(ctx, class, method_name)?;
    let is_ctor = matches!(
        resolved.method.special,
        Some(SpecialMember::DefaultCtor)
            | Some(SpecialMember::OtherCtor)
            | Some(SpecialMember::CopyCtor)
            | Some(SpecialMember::MoveCtor)
    );
    if !is_ctor {
        return Err(LowerError::NotACtor {
            class,
            method: method_name.to_string(),
        });
    }
    let mut argv = vec![ArgSlot::ThisPtr];
    append_formals(&mut argv, &resolved.method.sig.params);
    Ok(LoweredCall {
        shim_symbol: resolved.shim_symbol,
        target_symbol: resolved.symbol,
        ret_conv: ReturnConvention::ByValue,
        argv,
        formal_count: resolved.method.sig.params.len(),
    })
}

/// Lower a dtor call. Targets the D1 (complete-object) dtor. argv =
/// `[this]`; return is void. Independent of whether the class has an
/// explicit `SpecialMember::Dtor` in its method list — C++ implicitly
/// emits a trivial dtor for any class, and we want callers to always
/// have a destructor symbol available.
pub fn lower_dtor_call(
    ctx: &CxxTypeCtx,
    class: ClassId,
) -> Result<LoweredCall, LowerError> {
    let sym = Symbol::Dtor {
        class,
        variant: DtorVariant::D1,
    };
    let mangled = ctx.mangle(&sym);
    let shim = format!("__rustcc_shim_{mangled}");
    Ok(LoweredCall {
        shim_symbol: shim,
        target_symbol: mangled,
        ret_conv: ReturnConvention::ByValue,
        argv: vec![ArgSlot::ThisPtr],
        formal_count: 0,
    })
}

/// Lower a free-function call — no enclosing class, no `this`. The
/// caller supplies the symbol's scope (usually a namespace chain),
/// name, and signature; the mangler produces the Itanium symbol.
pub fn lower_free_fn_call(
    ctx: &CxxTypeCtx,
    scope: NestedName,
    name: Ident,
    sig: FnSig,
) -> Result<LoweredCall, LowerError> {
    let ret = sig.ret;
    let params = sig.params.clone();
    let sym = Symbol::Function { scope, name, sig };
    let mangled = ctx.mangle(&sym);
    let shim = format!("__rustcc_shim_{mangled}");

    let ret_conv = classify_return(ctx, ret)?;
    let mut argv = Vec::new();
    if matches!(ret_conv, ReturnConvention::Sret { .. }) {
        argv.push(ArgSlot::SretSlot);
    }
    append_formals(&mut argv, &params);
    Ok(LoweredCall {
        shim_symbol: shim,
        target_symbol: mangled,
        ret_conv,
        argv,
        formal_count: params.len(),
    })
}

// -------- Shared helpers -----------------------------------------------

fn resolve_or_err<'ctx>(
    ctx: &'ctx CxxTypeCtx,
    class: ClassId,
    method_name: &str,
) -> Result<ResolvedMethod<'ctx>, LowerError> {
    // Inline the resolution here rather than going through a local
    // CtxShimResolver — the trait method's returned ResolvedMethod
    // borrows from `&self`, and a stack-local resolver would be
    // dropped before we return.
    let class_def = ctx.class(class);
    for method in &class_def.methods {
        if let MethodName::Ident(i) = &method.name {
            if i.0 == method_name {
                let sym = method_symbol_for(ctx, class, method);
                let mangled = ctx.mangle(&sym);
                let shim = format!("__rustcc_shim_{mangled}");
                return Ok(ResolvedMethod {
                    method,
                    symbol: mangled,
                    shim_symbol: shim,
                });
            }
        }
    }
    Err(LowerError::MethodNotFound {
        class,
        method: method_name.to_string(),
    })
}

fn method_symbol_for(
    _ctx: &CxxTypeCtx,
    class: ClassId,
    method: &MethodDef,
) -> Symbol {
    match &method.special {
        Some(SpecialMember::DefaultCtor)
        | Some(SpecialMember::OtherCtor)
        | Some(SpecialMember::CopyCtor)
        | Some(SpecialMember::MoveCtor) => Symbol::Ctor {
            class,
            variant: CtorVariant::C1,
            sig: method.sig.clone(),
        },
        Some(SpecialMember::Dtor) => Symbol::Dtor {
            class,
            variant: DtorVariant::D1,
        },
        _ => Symbol::Method {
            class,
            name: method.name.clone(),
            sig: method.sig.clone(),
        },
    }
}

fn reject_virtual(
    method: &MethodDef,
    class: ClassId,
    method_name: &str,
) -> Result<(), LowerError> {
    if method.virtuality != rustc_abi_cxx::Virtuality::NonVirtual {
        return Err(LowerError::UnsupportedVirtual {
            class,
            method: method_name.to_string(),
        });
    }
    Ok(())
}

fn append_formals(argv: &mut Vec<ArgSlot>, params: &[TypeId]) {
    for (idx, ty) in params.iter().enumerate() {
        argv.push(ArgSlot::Formal { index: idx, ty: *ty });
    }
}

/// Itanium AMD64 SysV return-convention classification, v1 subset.
///
/// Correctness rules (matching the Itanium ABI spec §3.2.3 and Clang's
/// behavior for our supported targets):
///
/// - `void`, scalar, ptr, ref, enum → in-register. Always.
/// - Record types: split on **size** and **trivial-copyability**.
///   - Size > 16 bytes → always sret (no amount of trivial-ness lets a
///     big aggregate pass in registers on AMD64).
///   - Non-trivially-copyable → always sret (the caller can't memcpy
///     the return value into its slot; the callee must own placement).
///   - Otherwise (≤ 16 bytes AND trivially copyable) → in-register.
///     The fork splits the record across `{rax,rdx}` / `{xmm0,xmm1}`
///     per the AMD64 classifier when it lowers; this crate reports
///     only the outer "by-value vs sret" split.
/// - Array / fn-type / member-pointer returns: not representable in
///   Rust anyway; we conservatively report sret with size=0 so the
///   fork can refuse to emit without crashing.
fn classify_return(
    ctx: &CxxTypeCtx,
    ret: TypeId,
) -> Result<ReturnConvention, LayoutError> {
    match ctx.type_of(ret) {
        CxxType::Void
        | CxxType::Bool
        | CxxType::Int { .. }
        | CxxType::Float { .. }
        | CxxType::Ptr { .. }
        | CxxType::Ref { .. }
        | CxxType::Enum { .. } => Ok(ReturnConvention::ByValue),
        CxxType::Record(id) => {
            let layout = ctx.layout(*id)?;
            if layout.size_bytes <= 16 && ctx.is_pod_for_layout(*id) {
                Ok(ReturnConvention::ByValue)
            } else {
                Ok(ReturnConvention::Sret {
                    slot_align: layout.align_bytes,
                    slot_size: layout.size_bytes,
                })
            }
        }
        // Arrays, fn types, member-ptrs: not returnable / unsupported.
        // We fold them into sret conservatively so the fork gets a
        // structured answer (it can still error out later if this
        // can't be materialized) rather than silently producing wrong
        // code. Actual support comes with docs/codegen.md §4.
        _ => Ok(ReturnConvention::Sret {
            slot_align: 8,
            slot_size: 0,
        }),
    }
}

// -------- In-memory fakes for tests ------------------------------------

/// Test-only shim resolver backed by an in-memory map. Handy for
/// decoupling lowering tests from the full CxxTypeCtx machinery.
pub struct FakeShimResolver {
    pub methods: HashMap<(ClassId, String), (MethodDef, String, String)>,
}

impl ShimResolver for FakeShimResolver {
    fn resolve_method<'a>(
        &'a self,
        class: ClassId,
        method_name: &str,
    ) -> Option<ResolvedMethod<'a>> {
        let key = (class, method_name.to_string());
        self.methods.get(&key).map(|(m, sym, shim)| ResolvedMethod {
            method: m,
            symbol: sym.clone(),
            shim_symbol: shim.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_abi_cxx::{
        ClassDef, CxxType, FieldDef, FnSig, Ident, IntWidth, MethodDef,
        MethodName, NameSegment, NestedName, RecordKind, SpecialMember, Target,
        Virtuality,
    };

    fn int32_ty(ctx: &mut CxxTypeCtx) -> TypeId {
        ctx.intern_type(CxxType::Int {
            signed: true,
            width: IntWidth::I32,
        })
    }

    fn void_ty(ctx: &mut CxxTypeCtx) -> TypeId {
        ctx.intern_type(CxxType::Void)
    }

    fn class_with(
        ctx: &mut CxxTypeCtx,
        name: &str,
        fields: Vec<FieldDef>,
        methods: Vec<MethodDef>,
    ) -> ClassId {
        ctx.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident(name.into()))]),
            bases: vec![],
            fields,
            methods,
            kind: RecordKind::Class,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        })
    }

    fn sig(params: Vec<TypeId>, ret: TypeId, is_const: bool) -> FnSig {
        FnSig {
            params,
            ret,
            cv: rustc_abi_cxx::CvQual {
                is_const,
                is_volatile: false,
            },
            ref_q: None,
            variadic: false,
            noexcept: true,
        }
    }

    #[test]
    fn lower_scalar_return_produces_this_first_argv() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = int32_ty(&mut ctx);
        let class = class_with(
            &mut ctx,
            "Calc",
            vec![],
            vec![MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("add".into())),
                sig: sig(vec![i32_, i32_], i32_, false),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: None,
            }],
        );

        let lowered = lower_method_call(&ctx, class, "add").unwrap();
        assert_eq!(lowered.ret_conv, ReturnConvention::ByValue);
        assert_eq!(lowered.formal_count, 2);
        assert_eq!(
            lowered.argv,
            vec![
                ArgSlot::ThisPtr,
                ArgSlot::Formal { index: 0, ty: i32_ },
                ArgSlot::Formal { index: 1, ty: i32_ },
            ]
        );
        assert!(lowered.shim_symbol.starts_with("__rustcc_shim_"));
        assert!(
            lowered.shim_symbol.contains("add"),
            "symbol contains method name: {}",
            lowered.shim_symbol
        );
    }

    #[test]
    fn small_pod_record_return_is_by_value() {
        // 3×i32 = 12 bytes, POD-for-layout → registers on AMD64.
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = int32_ty(&mut ctx);
        let record = class_with(
            &mut ctx,
            "Triple",
            vec![
                FieldDef {
                    name: Ident("a".into()),
                    ty: i32_,
                    explicit_align: None,
                },
                FieldDef {
                    name: Ident("b".into()),
                    ty: i32_,
                    explicit_align: None,
                },
                FieldDef {
                    name: Ident("c".into()),
                    ty: i32_,
                    explicit_align: None,
                },
            ],
            vec![],
        );
        let record_ty = ctx.intern_type(CxxType::Record(record));
        let factory = class_with(
            &mut ctx,
            "Factory",
            vec![],
            vec![MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("build".into())),
                sig: sig(vec![], record_ty, false),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: None,
            }],
        );
        let lowered = lower_method_call(&ctx, factory, "build").unwrap();
        assert_eq!(lowered.ret_conv, ReturnConvention::ByValue);
        assert_eq!(lowered.argv, vec![ArgSlot::ThisPtr]);
    }

    #[test]
    fn large_record_return_uses_sret_regardless_of_pod() {
        // 5×i32 = 20 bytes > 16. POD but still sret.
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = int32_ty(&mut ctx);
        let fields: Vec<FieldDef> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|n| FieldDef {
                name: Ident((*n).into()),
                ty: i32_,
                explicit_align: None,
            })
            .collect();
        let record = class_with(&mut ctx, "Wide", fields, vec![]);
        let record_ty = ctx.intern_type(CxxType::Record(record));
        let factory = class_with(
            &mut ctx,
            "Factory",
            vec![],
            vec![MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("build".into())),
                sig: sig(vec![], record_ty, false),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: None,
            }],
        );
        let lowered = lower_method_call(&ctx, factory, "build").unwrap();
        match lowered.ret_conv {
            ReturnConvention::Sret { slot_size, slot_align } => {
                assert_eq!(slot_size, 20);
                assert_eq!(slot_align, 4);
            }
            other => panic!("expected Sret, got {other:?}"),
        }
        assert_eq!(
            lowered.argv,
            vec![ArgSlot::SretSlot, ArgSlot::ThisPtr]
        );
    }

    #[test]
    fn non_pod_small_record_still_uses_sret() {
        // Small record (8 bytes) but with a user-declared ctor →
        // non-POD → must sret because the caller can't memcpy.
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = int32_ty(&mut ctx);
        let void_ = void_ty(&mut ctx);
        let record = class_with(
            &mut ctx,
            "Handle",
            vec![FieldDef {
                name: Ident("inner".into()),
                ty: i32_,
                explicit_align: None,
            }],
            vec![MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("Handle".into())),
                sig: sig(vec![i32_], void_, false),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: Some(SpecialMember::OtherCtor),
            }],
        );
        let record_ty = ctx.intern_type(CxxType::Record(record));
        let factory = class_with(
            &mut ctx,
            "Factory",
            vec![],
            vec![MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("build".into())),
                sig: sig(vec![], record_ty, false),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: None,
            }],
        );
        let lowered = lower_method_call(&ctx, factory, "build").unwrap();
        match lowered.ret_conv {
            ReturnConvention::Sret { slot_size, .. } => {
                assert_eq!(slot_size, 4);
            }
            other => panic!("expected Sret, got {other:?}"),
        }
    }

    #[test]
    fn virtual_methods_are_rejected_in_v1() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let void = void_ty(&mut ctx);
        let class = class_with(
            &mut ctx,
            "Animal",
            vec![],
            vec![MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("speak".into())),
                sig: sig(vec![], void, false),
                virtuality: Virtuality::Virtual,
                vtable_index: Some(0),
                special: None,
            }],
        );
        let err = lower_method_call(&ctx, class, "speak").unwrap_err();
        assert!(
            matches!(err, LowerError::UnsupportedVirtual { .. }),
            "unexpected: {err:?}"
        );
    }

    #[test]
    fn missing_method_surfaces_error() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let class = class_with(&mut ctx, "Empty", vec![], vec![]);
        let err = lower_method_call(&ctx, class, "nope").unwrap_err();
        assert!(matches!(err, LowerError::MethodNotFound { .. }));
    }

    #[test]
    fn ctor_gets_ctor_mangling() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = int32_ty(&mut ctx);
        let void = void_ty(&mut ctx);
        let widget = class_with(
            &mut ctx,
            "Widget",
            vec![],
            vec![MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("Widget".into())),
                sig: sig(vec![i32_], void, false),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: Some(SpecialMember::OtherCtor),
            }],
        );
        let resolver = CtxShimResolver { ctx: &ctx };
        let r = resolver.resolve_method(widget, "Widget").unwrap();
        // The Itanium ctor mangling includes `C1` for complete-object.
        assert!(
            r.symbol.contains("C1"),
            "expected C1 ctor mangle in {}",
            r.symbol,
        );
    }

    #[test]
    fn ctx_layout_provider_matches_direct_call() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = int32_ty(&mut ctx);
        let class = class_with(
            &mut ctx,
            "Pt",
            vec![
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
            vec![],
        );
        let via_trait = LayoutProvider::layout_of(&ctx, class).unwrap();
        let direct = ctx.layout(class).unwrap();
        assert_eq!(via_trait.size_bytes, direct.size_bytes);
    }

    #[test]
    fn lower_static_call_omits_this_pointer() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = int32_ty(&mut ctx);
        let class = class_with(
            &mut ctx,
            "Math",
            vec![],
            vec![MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("pi_times".into())),
                sig: sig(vec![i32_], i32_, false),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: None,
            }],
        );
        let lowered = lower_static_call(&ctx, class, "pi_times").unwrap();
        assert_eq!(lowered.ret_conv, ReturnConvention::ByValue);
        // No ThisPtr in argv.
        assert_eq!(lowered.argv, vec![ArgSlot::Formal { index: 0, ty: i32_ }]);
        assert_eq!(lowered.formal_count, 1);
    }

    #[test]
    fn lower_ctor_call_puts_this_first_and_returns_void() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = int32_ty(&mut ctx);
        let void = void_ty(&mut ctx);
        let class = class_with(
            &mut ctx,
            "Widget",
            vec![],
            vec![MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("Widget".into())),
                sig: sig(vec![i32_, i32_], void, false),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: Some(SpecialMember::OtherCtor),
            }],
        );
        let lowered = lower_ctor_call(&ctx, class, "Widget").unwrap();
        assert_eq!(lowered.ret_conv, ReturnConvention::ByValue);
        assert_eq!(
            lowered.argv,
            vec![
                ArgSlot::ThisPtr,
                ArgSlot::Formal { index: 0, ty: i32_ },
                ArgSlot::Formal { index: 1, ty: i32_ },
            ]
        );
        // Mangling used C1 (complete-object ctor).
        assert!(
            lowered.target_symbol.contains("C1"),
            "expected C1 in {}",
            lowered.target_symbol,
        );
    }

    #[test]
    fn lower_ctor_call_rejects_non_ctor_methods() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = int32_ty(&mut ctx);
        let class = class_with(
            &mut ctx,
            "Widget",
            vec![],
            vec![MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("compute".into())),
                sig: sig(vec![], i32_, false),
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: None,
            }],
        );
        let err = lower_ctor_call(&ctx, class, "compute").unwrap_err();
        assert!(matches!(err, LowerError::NotACtor { .. }));
    }

    #[test]
    fn lower_dtor_call_produces_d1_symbol() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let class = class_with(&mut ctx, "Widget", vec![], vec![]);
        let lowered = lower_dtor_call(&ctx, class).unwrap();
        assert_eq!(lowered.argv, vec![ArgSlot::ThisPtr]);
        assert_eq!(lowered.formal_count, 0);
        assert!(
            lowered.target_symbol.contains("D1"),
            "expected D1 in {}",
            lowered.target_symbol,
        );
    }

    #[test]
    fn lower_free_fn_call_has_no_this_and_mangles_in_scope() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = int32_ty(&mut ctx);
        let scope = NestedName(vec![
            rustc_abi_cxx::NameSegment::Namespace(Ident("acme".into())),
        ]);
        let name = Ident("add".into());
        let sig = sig(vec![i32_, i32_], i32_, false);
        let lowered =
            lower_free_fn_call(&ctx, scope, name, sig).unwrap();
        assert_eq!(lowered.ret_conv, ReturnConvention::ByValue);
        assert_eq!(
            lowered.argv,
            vec![
                ArgSlot::Formal { index: 0, ty: i32_ },
                ArgSlot::Formal { index: 1, ty: i32_ },
            ]
        );
        // Itanium mangles `acme::add` as `_ZN4acme3addE...`.
        assert!(
            lowered.target_symbol.starts_with("_ZN4acme3add"),
            "unexpected mangling: {}",
            lowered.target_symbol,
        );
    }

    #[test]
    fn fake_shim_resolver_roundtrips() {
        // Need a real ClassId minted by a ctx (the constructor is
        // crate-private) — any ctx-owned id will do for wiring this.
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let placeholder = class_with(&mut ctx, "_Fake", vec![], vec![]);
        let i32_ = int32_ty(&mut ctx);

        let md = MethodDef { access: Default::default(),
            name: MethodName::Ident(Ident("foo".into())),
            sig: FnSig {
                params: vec![],
                ret: i32_,
                cv: Default::default(),
                ref_q: None,
                variadic: false,
                noexcept: true,
            },
            virtuality: Virtuality::NonVirtual,
            vtable_index: None,
            special: None,
        };
        let mut map = HashMap::new();
        map.insert(
            (placeholder, "foo".into()),
            (md, "_Z3foo".into(), "__rustcc_shim__Z3foo".into()),
        );
        let r = FakeShimResolver { methods: map };
        let resolved = r.resolve_method(placeholder, "foo").unwrap();
        assert_eq!(resolved.symbol, "_Z3foo");
        assert_eq!(resolved.shim_symbol, "__rustcc_shim__Z3foo");
    }
}
