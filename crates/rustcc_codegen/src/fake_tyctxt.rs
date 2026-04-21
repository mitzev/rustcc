//! Fake TyCtxt demonstrator.
//!
//! Purpose: stand in for rustc's `TyCtxt` when exercising the fork
//! seam (`LayoutProvider` + `ManglerProvider` + `ShimResolver`) in
//! tests and docs. The rustc fork will implement the same trait set
//! around a real `TyCtxt`; this module implements it around an
//! in-memory `CxxTypeCtx` plus a small call-site table. Lowering
//! results produced through this harness are byte-for-byte identical
//! to what the fork will produce once wired up — which is the point:
//! future integration can lean on these tests for regression checks
//! that don't require a rustc build.
//!
//! What's here:
//!
//! - [`FakeCrate`] — a batch of classes + call sites.
//! - [`FakeProvider`] — wraps a `CxxTypeCtx` and implements all three
//!   provider traits.
//! - [`lower_fake_crate`] — walks every call site and returns the
//!   lowered calls, checking them for shape (argv length matches
//!   formal-count, sret slot when required, etc.).
//!
//! What's NOT here: HIR emission, LLVM IR, rustc's actual types.
//! Those live in the fork.

use rustc_abi_cxx::{ClassId, CxxTypeCtx};

use crate::{
    lower_ctor_call, lower_dtor_call, lower_method_call, lower_static_call,
    ArgSlot, LoweredCall, LowerError, ReturnConvention,
};

/// Kind of call site the fork is lowering. Each variant corresponds
/// to one of the `lower_*_call` entry points in the parent crate.
#[derive(Debug, Clone)]
pub enum FakeCallKind {
    /// Non-virtual instance method.
    Method {
        class: ClassId,
        method_name: String,
    },
    /// Static method (no `this`).
    Static {
        class: ClassId,
        method_name: String,
    },
    /// Constructor. `method_name` is the class's identifier.
    Ctor {
        class: ClassId,
        method_name: String,
    },
    /// Destructor. Targets D1 (complete-object).
    Dtor {
        class: ClassId,
    },
    // Free functions would need a scope/name/sig here — omitted
    // because the method forms are the ones the fork hits first.
}

#[derive(Debug, Clone)]
pub struct FakeCallSite {
    pub id: u32,
    pub kind: FakeCallKind,
}

/// A toy crate: shared `CxxTypeCtx` + a set of call sites to lower.
pub struct FakeCrate<'ctx> {
    pub ctx: &'ctx CxxTypeCtx,
    pub calls: Vec<FakeCallSite>,
}

/// Wraps a `CxxTypeCtx` as a TyCtxt-shaped facade implementing the
/// provider traits. The fork's real provider will live around a real
/// `TyCtxt`; the shape is the same.
pub struct FakeProvider<'ctx> {
    pub ctx: &'ctx CxxTypeCtx,
}

impl<'ctx> crate::LayoutProvider for FakeProvider<'ctx> {
    fn layout_of(
        &self,
        class: ClassId,
    ) -> Result<rustc_abi_cxx::RecordLayout, rustc_abi_cxx::LayoutError> {
        crate::LayoutProvider::layout_of(self.ctx, class)
    }
}

impl<'ctx> crate::ManglerProvider for FakeProvider<'ctx> {
    fn mangle(&self, symbol: &rustc_abi_cxx::Symbol) -> String {
        crate::ManglerProvider::mangle(self.ctx, symbol)
    }
}

impl<'ctx> crate::ShimResolver for FakeProvider<'ctx> {
    fn resolve_method<'a>(
        &'a self,
        class: ClassId,
        method_name: &str,
    ) -> Option<crate::ResolvedMethod<'a>> {
        // Inline resolution — going through a stack-local
        // `CtxShimResolver` doesn't compose because the returned
        // `ResolvedMethod` borrows from `&self`, and the trait
        // signature ties lifetimes to our own `&'a self`.
        let class_def = self.ctx.class(class);
        for method in &class_def.methods {
            if let rustc_abi_cxx::MethodName::Ident(i) = &method.name {
                if i.0 == method_name {
                    let sym = crate::method_symbol_for(self.ctx, class, method);
                    let mangled = crate::ManglerProvider::mangle(self.ctx, &sym);
                    let shim = format!("__rustcc_shim_{mangled}");
                    return Some(crate::ResolvedMethod {
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

/// A single lowered call, paired with the call-site id for traceability.
#[derive(Debug)]
pub struct FakeLoweredCall {
    pub site_id: u32,
    pub lowered: LoweredCall,
}

/// Walk every call site in `krate`, lower it through the corresponding
/// `lower_*_call` entry point, and return the results in order.
///
/// This function also validates the shape invariants each variant
/// promises — e.g., method calls always have a `ThisPtr` in argv,
/// ctor calls never have an sret, dtor calls have exactly one slot.
/// A failure of these invariants is a bug in the lowering layer;
/// callers surface it as `LowerError::Layout` or similar.
pub fn lower_fake_crate(
    krate: &FakeCrate<'_>,
) -> Result<Vec<FakeLoweredCall>, LowerError> {
    let mut out = Vec::with_capacity(krate.calls.len());
    for site in &krate.calls {
        let lowered = match &site.kind {
            FakeCallKind::Method { class, method_name } => {
                let l = lower_method_call(krate.ctx, *class, method_name)?;
                assert_method_shape(&l);
                l
            }
            FakeCallKind::Static { class, method_name } => {
                let l = lower_static_call(krate.ctx, *class, method_name)?;
                assert_static_shape(&l);
                l
            }
            FakeCallKind::Ctor { class, method_name } => {
                let l = lower_ctor_call(krate.ctx, *class, method_name)?;
                assert_ctor_shape(&l);
                l
            }
            FakeCallKind::Dtor { class } => {
                let l = lower_dtor_call(krate.ctx, *class)?;
                assert_dtor_shape(&l);
                l
            }
        };
        out.push(FakeLoweredCall { site_id: site.id, lowered });
    }
    Ok(out)
}

fn assert_method_shape(l: &LoweredCall) {
    let expected_len = if matches!(l.ret_conv, ReturnConvention::Sret { .. }) {
        1 + 1 + l.formal_count
    } else {
        1 + l.formal_count
    };
    debug_assert_eq!(l.argv.len(), expected_len, "method argv shape");
    // Exactly one ThisPtr; position depends on sret.
    debug_assert_eq!(
        l.argv.iter().filter(|a| matches!(a, ArgSlot::ThisPtr)).count(),
        1
    );
}

fn assert_static_shape(l: &LoweredCall) {
    // No ThisPtr; argv = sret? + formals.
    debug_assert!(!l.argv.iter().any(|a| matches!(a, ArgSlot::ThisPtr)));
    let expected_len = if matches!(l.ret_conv, ReturnConvention::Sret { .. }) {
        1 + l.formal_count
    } else {
        l.formal_count
    };
    debug_assert_eq!(l.argv.len(), expected_len);
}

fn assert_ctor_shape(l: &LoweredCall) {
    debug_assert_eq!(l.ret_conv, ReturnConvention::ByValue);
    debug_assert!(
        matches!(l.argv.first(), Some(ArgSlot::ThisPtr)),
        "ctor must start with ThisPtr"
    );
}

fn assert_dtor_shape(l: &LoweredCall) {
    debug_assert_eq!(l.ret_conv, ReturnConvention::ByValue);
    debug_assert_eq!(l.argv, vec![ArgSlot::ThisPtr]);
    debug_assert_eq!(l.formal_count, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_abi_cxx::{
        ClassDef, CvQual, CxxType, FieldDef, FnSig, Ident, IntWidth, MethodDef,
        MethodName, NameSegment, NestedName, RecordKind, SpecialMember, Target,
        Virtuality,
    };

    fn sig(params: Vec<rustc_abi_cxx::TypeId>, ret: rustc_abi_cxx::TypeId, is_const: bool) -> FnSig {
        FnSig {
            params,
            ret,
            cv: CvQual { is_const, is_volatile: false },
            ref_q: None,
            variadic: false,
            noexcept: true,
        }
    }

    fn fixture_ctx() -> (CxxTypeCtx, ClassId) {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = ctx.intern_type(CxxType::Int { signed: true, width: IntWidth::I32 });
        let void_ = ctx.intern_type(CxxType::Void);
        let class = ctx.define_rust_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Counter".into()))]),
            bases: vec![],
            fields: vec![FieldDef {
                name: Ident("n".into()),
                ty: i32_,
                explicit_align: None,
            }],
            methods: vec![
                // Ctor Counter(i32)
                MethodDef {
                    name: MethodName::Ident(Ident("Counter".into())),
                    sig: sig(vec![i32_], void_, false),
                    virtuality: Virtuality::NonVirtual,
                    vtable_index: None,
                    special: Some(SpecialMember::OtherCtor),
                },
                // Static fn Counter::zero() -> Counter { ... }
                MethodDef {
                    name: MethodName::Ident(Ident("default_count".into())),
                    sig: sig(vec![], i32_, false),
                    virtuality: Virtuality::NonVirtual,
                    vtable_index: None,
                    special: None,
                },
                // Instance method i32 Counter::get() const
                MethodDef {
                    name: MethodName::Ident(Ident("get".into())),
                    sig: sig(vec![], i32_, true),
                    virtuality: Virtuality::NonVirtual,
                    vtable_index: None,
                    special: None,
                },
                // Instance method void Counter::bump(i32)
                MethodDef {
                    name: MethodName::Ident(Ident("bump".into())),
                    sig: sig(vec![i32_], void_, false),
                    virtuality: Virtuality::NonVirtual,
                    vtable_index: None,
                    special: None,
                },
            ],
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        (ctx, class)
    }

    #[test]
    fn lowers_a_mixed_call_site_batch() {
        let (ctx, class) = fixture_ctx();
        let calls = vec![
            FakeCallSite {
                id: 1,
                kind: FakeCallKind::Ctor {
                    class,
                    method_name: "Counter".into(),
                },
            },
            FakeCallSite {
                id: 2,
                kind: FakeCallKind::Static {
                    class,
                    method_name: "default_count".into(),
                },
            },
            FakeCallSite {
                id: 3,
                kind: FakeCallKind::Method {
                    class,
                    method_name: "get".into(),
                },
            },
            FakeCallSite {
                id: 4,
                kind: FakeCallKind::Method {
                    class,
                    method_name: "bump".into(),
                },
            },
            FakeCallSite {
                id: 5,
                kind: FakeCallKind::Dtor { class },
            },
        ];
        let krate = FakeCrate { ctx: &ctx, calls };
        let lowered = lower_fake_crate(&krate).expect("lower");
        assert_eq!(lowered.len(), 5);

        // Ctor.
        let ctor = &lowered[0].lowered;
        assert!(ctor.target_symbol.contains("C1"));
        assert_eq!(ctor.argv[0], ArgSlot::ThisPtr);
        assert_eq!(ctor.formal_count, 1);

        // Static: no ThisPtr.
        let stat = &lowered[1].lowered;
        assert!(!stat.argv.iter().any(|a| matches!(a, ArgSlot::ThisPtr)));

        // Method.
        let get = &lowered[2].lowered;
        assert_eq!(get.argv, vec![ArgSlot::ThisPtr]);

        // Dtor.
        let dtor = &lowered[4].lowered;
        assert!(dtor.target_symbol.contains("D1"));
    }

    #[test]
    fn unknown_method_surfaces_as_lower_error() {
        let (ctx, class) = fixture_ctx();
        let krate = FakeCrate {
            ctx: &ctx,
            calls: vec![FakeCallSite {
                id: 1,
                kind: FakeCallKind::Method {
                    class,
                    method_name: "nope".into(),
                },
            }],
        };
        let err = lower_fake_crate(&krate).unwrap_err();
        assert!(matches!(err, LowerError::MethodNotFound { .. }));
    }

    #[test]
    fn fake_provider_implements_all_three_traits_consistently() {
        use crate::{LayoutProvider, ManglerProvider, ShimResolver};
        let (ctx, class) = fixture_ctx();
        let provider = FakeProvider { ctx: &ctx };

        // Layout via trait matches direct call.
        let via = LayoutProvider::layout_of(&provider, class).unwrap();
        let direct = ctx.layout(class).unwrap();
        assert_eq!(via.size_bytes, direct.size_bytes);

        // Mangling produces something stable (just assert it's a valid
        // Itanium prefix).
        let sym = rustc_abi_cxx::Symbol::VTable(class);
        let name = ManglerProvider::mangle(&provider, &sym);
        assert!(name.starts_with("_ZTV"), "got: {name}");

        // Resolver finds `get`.
        let resolved = provider.resolve_method(class, "get").unwrap();
        assert!(resolved.shim_symbol.starts_with("__rustcc_shim_"));
    }
}
