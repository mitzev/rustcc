//! Rust-side forwarding thunks: `extern "C" fn` wrappers with
//! `#[export_name = "<Itanium mangled>"]` that delegate to the user's
//! real Rust method bodies.
//!
//! # What this replaces
//!
//! The canonical plan in `docs/codegen.md` is for the rustc fork to
//! emit Itanium-mangled object code directly for every
//! `#[repr(cpp)]` method, so C++ callers can dispatch via ordinary
//! linker resolution. Pre-fork, we achieve the same end via stable
//! rustc's `#[export_name]` attribute: a `pub unsafe extern "C" fn`
//! with an explicit symbol name lands in the crate's object file
//! under exactly that name. If the name matches what Itanium's
//! mangler produces for `Foo::bar(...)`, C++ links against it
//! transparently.
//!
//! # Coverage
//!
//! v1 emits forwarders for:
//!
//! - Instance methods with `&self` (mapped to `*const T` this-slot)
//!   or `&mut self` (mapped to `*mut T`).
//! - Constructors (`SpecialMember::DefaultCtor` /
//!   `SpecialMember::OtherCtor`): take a `this: *mut T` output slot,
//!   call the Rust fn (typically `fn new(args) -> Self`), then
//!   `std::ptr::write(this, result)` the returned value into the
//!   slot. Itanium C1 complete-object ctor mangling.
//! - Destructors (`SpecialMember::Dtor`): take a `this: *mut T`,
//!   call `std::ptr::drop_in_place(this)`. Itanium D1 complete-
//!   object dtor mangling. Every Rust-origin class gets a dtor
//!   forwarder even without an explicit `impl Drop`, so the C++
//!   side always has a destructor symbol to link against.
//! - Parameter and return types: scalar builtins + raw pointers.
//!   References and records are deferred until we have a careful
//!   account of the Itanium ABI mismatch for by-value struct
//!   returns (sret on the C++ side may or may not match rustc's
//!   lowering for `extern "C"`).
//!
//! # Output shape
//!
//! The emitted source expects the user's types to be visible in
//! scope at the point of `include!`. A convenient pattern for the
//! user's crate:
//!
//! ```ignore
//! #[cfg(rustcc_forwarders)]
//! include!(env!("RUSTCC_FORWARDERS_PATH"));
//! ```
//!
//! The rustcc driver sets `RUSTCC_FORWARDERS_PATH` + `--cfg
//! rustcc_forwarders` when `emit-forwarders = true` in the manifest,
//! so the include! is inert otherwise.

use std::fmt::Write as _;

use rustc_abi_cxx::{
    ClassId, CtorVariant, CxxType, CxxTypeCtx, DtorVariant, FloatKind, IntWidth,
    MethodDef, SpecialMember, Symbol, TypeId, TypeOrigin, Virtuality,
};

#[derive(Debug)]
pub enum ForwarderError {
    /// A parameter or return type that v1 forwarders don't handle.
    /// Limited set by design — expansion here requires ABI audit
    /// per type kind.
    UnsupportedType {
        where_: String,
        kind: String,
    },
}

impl core::fmt::Display for ForwarderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ForwarderError {}

/// Selects the calling-convention shape for record-by-value return
/// forwarders. Stock rustc and the rustcc fork disagree on what's
/// possible here:
///
/// - `ExternCExplicitSret` (default): emit `extern "C" fn(__sret:
///   *mut T, ...)` with `()` return. Works on any stock rustc, but
///   only correct on x86_64 SysV — there the indirect-result
///   pointer goes in `rdi`, which happens to coincide with the
///   first pointer arg slot under SysV C, so the explicit
///   `__sret` arg lands in the right register. AAPCS64 puts the
///   indirect-result pointer in the dedicated `x8` register, so
///   this shape misroutes every other register and crashes at
///   runtime.
/// - `ExternCpp` (fork-only): emit `extern "C++" fn(...) -> T`
///   and let the fork's per-target `compute_cxx_abi_info` overlay
///   force-indirect the ADT return — sret in `rdi` on x86_64,
///   `x8` on AAPCS64. Requires the rustcc fork's `extern "C++"`
///   ABI; stock rustc rejects the ABI string outright.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecordReturnAbi {
    /// Stock-rustc-compatible shape (x86_64 SysV only).
    #[default]
    ExternCExplicitSret,
    /// Fork-only shape that delegates sret to `extern "C++"`.
    ExternCpp,
}

/// Tunables for [`generate_rust_forwarders_with`]. Defaults select
/// the maximally compatible (stock rustc, x86_64) shape.
#[derive(Clone, Copy, Debug, Default)]
pub struct ForwarderConfig {
    pub record_return_abi: RecordReturnAbi,
}

/// Emit the full forwarders source for every Rust-origin class in
/// `ctx`. Callers write the result to disk and arrange for it to be
/// `include!`'d from the user's crate.
///
/// `rust_name_of` maps a `ClassId` back to the Rust-side identifier
/// used to construct the type in source. For the common case where
/// the Rust and C++ names match, use `default_rust_name` — it reads
/// the cpp name directly out of the IR. Users who override
/// `#[cpp_name = "..."]` should pass a closure that looks up the
/// original Rust identifier.
///
/// Uses [`ForwarderConfig::default`] — the stock-compatible record-
/// return shape. To opt into the fork's `extern "C++"` shape (required
/// for aarch64 correctness), call [`generate_rust_forwarders_with`].
pub fn generate_rust_forwarders<F>(
    ctx: &CxxTypeCtx,
    rust_name_of: F,
) -> Result<String, ForwarderError>
where
    F: Fn(ClassId) -> String,
{
    generate_rust_forwarders_with(ctx, rust_name_of, ForwarderConfig::default())
}

/// Same as [`generate_rust_forwarders`], with caller-supplied
/// [`ForwarderConfig`] for the record-return ABI shape.
pub fn generate_rust_forwarders_with<F>(
    ctx: &CxxTypeCtx,
    rust_name_of: F,
    config: ForwarderConfig,
) -> Result<String, ForwarderError>
where
    F: Fn(ClassId) -> String,
{
    let mut out = String::new();
    out.push_str(
        "// Generated by rustcc cxx_importer::rust_forwarders. Do not hand-edit.\n\
         // See the module docs for the role of these thunks.\n\n",
    );

    // Panic guard. Every forwarder body routes through this to turn
    // an unwinding Rust panic into an `abort()` — crossing `extern
    // \"C\"` with an in-flight panic is UB, and C++ on the other
    // side has no way to catch a Rust unwind safely. `AssertUnwindSafe`
    // is correct here because the closure only captures raw pointers
    // and scalar args (both unconditionally UnwindSafe); the user
    // method being called can mutate arbitrary state, but any
    // invariant violation from a panic is resolved by aborting
    // before control returns to the C++ caller.
    out.push_str(
        "#[doc(hidden)]\n\
         #[inline]\n\
         fn __rustcc_guard<R>(f: impl FnOnce() -> R) -> R {\n\
         \x20\x20\x20\x20match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(f)) {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20Ok(v) => v,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20Err(_) => ::std::process::abort(),\n\
         \x20\x20\x20\x20}\n\
         }\n\n",
    );

    for class_id in ctx.rust_classes() {
        debug_assert_eq!(ctx.class_origin(class_id), TypeOrigin::RustReprCpp);
        let class = ctx.class(class_id);
        let rust_name = rust_name_of(class_id);

        // Dtor — always emitted. `drop_in_place` handles both
        // user-written and implicit `Drop`.
        let dtor_sym = ctx.mangle(&Symbol::Dtor {
            class: class_id,
            variant: DtorVariant::D1,
        });
        let _ = writeln!(out, "// Destructor forwarder for {rust_name}");
        let _ = writeln!(
            out,
            "#[unsafe(export_name = \"{dtor_sym}\")]"
        );
        let _ = writeln!(
            out,
            "pub unsafe extern \"C\" fn __rustcc_fwd_{rust_name}_dtor(this: *mut {rust_name}) {{"
        );
        let _ = writeln!(
            out,
            "    __rustcc_guard(|| unsafe {{ ::core::ptr::drop_in_place(this); }})"
        );
        let _ = writeln!(out, "}}\n");

        // Methods / ctors.
        let mut method_counter: usize = 0;
        for method in &class.methods {
            if method.virtuality != Virtuality::NonVirtual {
                continue;
            }
            if matches!(
                method.special,
                Some(SpecialMember::Dtor)
                    | Some(SpecialMember::CopyCtor)
                    | Some(SpecialMember::MoveCtor)
                    | Some(SpecialMember::CopyAssign)
                    | Some(SpecialMember::MoveAssign)
            ) {
                // Dtor already handled above; copy/move/assign
                // special members are not supported in v1.
                continue;
            }

            let name = match method.name.ident_name() {
                Some(n) => n,
                None => continue,
            };
            let idx = method_counter;
            method_counter += 1;

            let is_ctor = matches!(
                method.special,
                Some(SpecialMember::DefaultCtor)
                    | Some(SpecialMember::OtherCtor)
            );

            let sym = if is_ctor {
                ctx.mangle(&Symbol::Ctor {
                    class: class_id,
                    variant: CtorVariant::C1,
                    sig: method.sig.clone(),
                })
            } else {
                ctx.mangle(&Symbol::Method {
                    class: class_id,
                    name: method.name.clone(),
                    sig: method.sig.clone(),
                })
            };

            let body = render_forwarder(
                ctx,
                &rust_name,
                name,
                idx,
                method,
                is_ctor,
                &sym,
                &rust_name_of,
                config,
            )?;
            out.push_str(&body);
        }
    }

    Ok(out)
}

/// Convenience wrapper: assume rust_name == cpp_name. Useful when
/// the user hasn't used `#[cpp_name]` overrides — the common case.
pub fn default_rust_name(ctx: &CxxTypeCtx) -> impl Fn(ClassId) -> String + '_ {
    move |class_id| {
        let class = ctx.class(class_id);
        class
            .name
            .0
            .last()
            .and_then(|seg| match seg {
                rustc_abi_cxx::NameSegment::Class(i)
                | rustc_abi_cxx::NameSegment::Enum(i) => Some(i.0.clone()),
                rustc_abi_cxx::NameSegment::TemplateSpec { name, .. } => {
                    Some(name.0.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| "_".into())
    }
}

fn render_forwarder<F>(
    ctx: &CxxTypeCtx,
    rust_name: &str,
    method_name: &str,
    idx: usize,
    method: &MethodDef,
    is_ctor: bool,
    export_sym: &str,
    rust_name_of: &F,
    config: ForwarderConfig,
) -> Result<String, ForwarderError>
where
    F: Fn(ClassId) -> String,
{
    let mut out = String::new();
    let (params, call_exprs) =
        render_param_list(ctx, &method.sig.params, rust_name_of)?;
    let args_forward = call_exprs.join(", ");
    let fwd_ident = format!("__rustcc_fwd_{rust_name}_{method_name}_{idx}");

    if is_ctor {
        // `fn new(args) -> Self` convention: we call it and write
        // the result into the caller's `this` slot. Other ctor
        // shapes (ones that take `&mut self` already pointing at
        // storage) would need different body generation; v1
        // supports only the `Self`-returning form.
        let _ = writeln!(
            out,
            "// Constructor forwarder for {rust_name}::{method_name}"
        );
        let _ = writeln!(out, "#[unsafe(export_name = \"{export_sym}\")]");
        let _ = writeln!(
            out,
            "pub unsafe extern \"C\" fn {fwd_ident}(this: *mut {rust_name}{maybe_comma}{params}) {{",
            maybe_comma = if method.sig.params.is_empty() { "" } else { ", " },
        );
        let _ = writeln!(
            out,
            "    __rustcc_guard(|| unsafe {{ ::core::ptr::write(this, {rust_name}::{method_name}({args_forward})); }})"
        );
        let _ = writeln!(out, "}}\n");
        return Ok(out);
    }

    // Instance method. Receiver is inferred from method.sig.cv:
    // const → &self (via *const T), non-const → &mut self (via
    // *mut T).
    let is_const = method.sig.cv.is_const;
    let self_ptr_ty = if is_const { "*const" } else { "*mut" };
    let self_deref = if is_const {
        format!("(&*this).{method_name}")
    } else {
        format!("(&mut *this).{method_name}")
    };

    // Record-by-value return splits two ways depending on
    // `config.record_return_abi` — see `RecordReturnAbi` for
    // the per-target / per-rustc tradeoffs.
    let ret_is_record = matches!(
        ctx.type_of(method.sig.ret),
        CxxType::Record(_)
    );
    if ret_is_record {
        let record_ty =
            render_type_rust(ctx, method.sig.ret, "return", rust_name_of)?;
        match config.record_return_abi {
            RecordReturnAbi::ExternCExplicitSret => {
                let _ = writeln!(
                    out,
                    "// Forwarder for {rust_name}::{method_name} (sret return)"
                );
                let _ = writeln!(
                    out,
                    "#[unsafe(export_name = \"{export_sym}\")]"
                );
                let _ = writeln!(
                    out,
                    "pub unsafe extern \"C\" fn {fwd_ident}(__sret: *mut {record_ty}, this: {self_ptr_ty} {rust_name}{maybe_comma}{params}) {{",
                    maybe_comma = if method.sig.params.is_empty() { "" } else { ", " },
                );
                let _ = writeln!(
                    out,
                    "    __rustcc_guard(|| unsafe {{ ::core::ptr::write(__sret, {self_deref}({args_forward})); }})"
                );
            }
            RecordReturnAbi::ExternCpp => {
                let _ = writeln!(
                    out,
                    "// Forwarder for {rust_name}::{method_name} (record return)"
                );
                let _ = writeln!(
                    out,
                    "#[unsafe(export_name = \"{export_sym}\")]"
                );
                let _ = writeln!(
                    out,
                    "pub unsafe extern \"C++\" fn {fwd_ident}(this: {self_ptr_ty} {rust_name}{maybe_comma}{params}) -> {record_ty} {{",
                    maybe_comma = if method.sig.params.is_empty() { "" } else { ", " },
                );
                let _ = writeln!(
                    out,
                    "    __rustcc_guard(|| unsafe {{ {self_deref}({args_forward}) }})"
                );
            }
        }
        let _ = writeln!(out, "}}\n");
        return Ok(out);
    }

    let ret_ty = render_type_rust(ctx, method.sig.ret, "return", rust_name_of)?;
    let ret_sep = if ret_ty == "()" { "" } else { " -> " };
    let ret_rendered = if ret_ty == "()" {
        String::new()
    } else {
        ret_ty.clone()
    };
    let _ = writeln!(
        out,
        "// Forwarder for {rust_name}::{method_name}"
    );
    let _ = writeln!(out, "#[unsafe(export_name = \"{export_sym}\")]");
    let _ = writeln!(
        out,
        "pub unsafe extern \"C\" fn {fwd_ident}(this: {self_ptr_ty} {rust_name}{maybe_comma}{params}){ret_sep}{ret_rendered} {{",
        maybe_comma = if method.sig.params.is_empty() { "" } else { ", " },
    );
    let _ = writeln!(
        out,
        "    __rustcc_guard(|| unsafe {{ {self_deref}({args_forward}) }})"
    );
    let _ = writeln!(out, "}}\n");
    Ok(out)
}

/// Render a method's param list two ways: (a) the extern-C decl
/// list for the forwarder signature, (b) the call-site expressions
/// used to forward each arg into the user's Rust method. Scalars
/// pass through verbatim; record-by-value params go through the
/// caller-destroys Itanium convention (caller allocates temp,
/// passes pointer, runs ~T after return), which on the Rust side
/// means the forwarder takes `*const RecordName` and `ptr::read`s
/// to materialize an owned value. We reject record params on types
/// with user-declared `Drop` — after `ptr::read` transfers
/// ownership to Rust (and Rust's drop fires at end of method),
/// C++'s caller-destroys ~T() on the original slot would be a
/// second destructor call on moved-out bytes. For types without
/// `Drop`, `drop_in_place` is a no-op so the "double destroy" is
/// harmless.
fn render_param_list<F>(
    ctx: &CxxTypeCtx,
    params: &[TypeId],
    rust_name_of: &F,
) -> Result<(String, Vec<String>), ForwarderError>
where
    F: Fn(ClassId) -> String,
{
    let mut decls = Vec::with_capacity(params.len());
    let mut call_exprs = Vec::with_capacity(params.len());
    for (i, p) in params.iter().enumerate() {
        match ctx.type_of(*p) {
            CxxType::Record(class_id) => {
                if class_has_user_dtor(ctx, *class_id) {
                    return Err(ForwarderError::UnsupportedType {
                        where_: format!("parameter {i}"),
                        kind: format!(
                            "record-by-value param of type `{}` that \
                             has a user `impl Drop` — v1 forwarders \
                             can't safely bridge the caller-destroys \
                             Itanium convention for Drop types; pass \
                             as `*const T` / `*mut T` instead",
                            rust_name_of(*class_id)
                        ),
                    });
                }
                let ty = rust_name_of(*class_id);
                decls.push(format!("arg{i}: *const {ty}"));
                call_exprs.push(format!("::core::ptr::read(arg{i})"));
            }
            _ => {
                let ty =
                    render_type_rust(ctx, *p, "parameter", rust_name_of)?;
                decls.push(format!("arg{i}: {ty}"));
                call_exprs.push(format!("arg{i}"));
            }
        }
    }
    Ok((decls.join(", "), call_exprs))
}

fn class_has_user_dtor(ctx: &CxxTypeCtx, class_id: ClassId) -> bool {
    ctx.class(class_id)
        .methods
        .iter()
        .any(|m| matches!(m.special, Some(SpecialMember::Dtor)))
}

/// Map a `CxxType` back to the Rust type syntax that matches the ABI.
/// Critical: the Rust-side `extern "C"` parameter type must share the
/// C ABI of the C++ signature Itanium generates — any divergence
/// silently corrupts the call.
fn render_type_rust<F>(
    ctx: &CxxTypeCtx,
    ty: TypeId,
    where_: &str,
    rust_name_of: &F,
) -> Result<String, ForwarderError>
where
    F: Fn(ClassId) -> String,
{
    match ctx.type_of(ty) {
        CxxType::Void => Ok("()".into()),
        CxxType::Bool => Ok("bool".into()),
        CxxType::Int { signed, width } => Ok(int_rust(*signed, *width).into()),
        CxxType::Float { kind } => Ok(match kind {
            FloatKind::F32 => "f32".into(),
            FloatKind::F64 => "f64".into(),
            FloatKind::LongDouble => {
                return Err(ForwarderError::UnsupportedType {
                    where_: where_.into(),
                    kind: "long double (no direct Rust equivalent)".into(),
                });
            }
        }),
        CxxType::Ptr { pointee, cv } => {
            let inner = render_type_rust(ctx, *pointee, where_, rust_name_of)?;
            Ok(if cv.is_const {
                format!("*const {inner}")
            } else {
                format!("*mut {inner}")
            })
        }
        // References are skipped in v1 — mapping Rust `&T` to the
        // C++ reference ABI requires care (Rust's raw-ptr ABI is
        // equivalent, but `&T` imposes aliasing invariants the
        // extern "C" boundary can't enforce). Use raw pointers for
        // interop; a future revision may lift this.
        CxxType::Ref { .. } => Err(ForwarderError::UnsupportedType {
            where_: where_.into(),
            kind: "C++ reference (use *const/*mut T instead in v1 forwarders)".into(),
        }),
        // Record-by-value on the boundary: Rust's `extern "C" fn`
        // lowering already agrees with the SysV AMD64 classifier
        // for POD-for-layout types — small PODs pass/return in
        // registers (packed into rax/rdx or xmm0/xmm1), larger
        // ones via sret. This matches Clang's Itanium ABI output
        // for the same shapes, so passing records by value through
        // the forwarders Just Works for everything that's
        // trivially copyable. Non-trivial classes (user-declared
        // copy/move/dtor) go sret on both sides so also agree.
        // The one open edge where the ABIs diverge is sub-64-bit
        // aggregates containing f32 (xmm0 vs rax classification)
        // on some older targets — we lean on e2e tests rather
        // than over-conservatism to catch it.
        CxxType::Record(class_id) => Ok(rust_name_of(*class_id)),
        CxxType::Enum { .. }
        | CxxType::Array { .. }
        | CxxType::Fn(_)
        | CxxType::MemberPtr { .. } => Err(ForwarderError::UnsupportedType {
            where_: where_.into(),
            kind: format!("{:?}", ctx.type_of(ty)),
        }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_abi_cxx::{
        ClassDef, CvQual, CxxType, FieldDef, FnSig, Ident, IntWidth, MethodDef,
        MethodName, NameSegment, NestedName, RecordKind, Target, Virtuality,
    };

    fn ctx_with_point() -> (CxxTypeCtx, ClassId) {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
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
            methods: vec![
                MethodDef { access: Default::default(),
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
                    special: Some(rustc_abi_cxx::SpecialMember::OtherCtor),
                },
                MethodDef { access: Default::default(),
                    name: MethodName::Ident(Ident("magnitude_sq".into())),
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
                },
                MethodDef { access: Default::default(),
                    name: MethodName::Ident(Ident("translate".into())),
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
                    special: None,
                },
            ],
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        (ctx, id)
    }

    #[test]
    fn emits_dtor_forwarder_for_every_rust_class() {
        let (ctx, _) = ctx_with_point();
        let src =
            generate_rust_forwarders(&ctx, default_rust_name(&ctx)).unwrap();
        assert!(
            src.contains("__rustcc_fwd_Point_dtor"),
            "missing dtor fwd:\n{src}"
        );
        // D1 destructor symbol.
        assert!(
            src.contains("_ZN5PointD1Ev"),
            "missing Itanium D1 symbol in dtor export_name:\n{src}"
        );
        // drop_in_place body.
        assert!(
            src.contains("::core::ptr::drop_in_place(this)"),
            "missing drop_in_place in dtor body:\n{src}"
        );
    }

    #[test]
    fn emits_ctor_forwarder_with_write() {
        let (ctx, _) = ctx_with_point();
        let src =
            generate_rust_forwarders(&ctx, default_rust_name(&ctx)).unwrap();
        // Ctor uses C1 mangling.
        assert!(
            src.contains("_ZN5PointC1Eii"),
            "missing Itanium C1 ctor symbol:\n{src}"
        );
        assert!(
            src.contains("::core::ptr::write(this, Point::new(arg0, arg1))"),
            "missing write(this, Point::new(...)):\n{src}"
        );
    }

    #[test]
    fn const_method_uses_const_ptr_and_ref() {
        let (ctx, _) = ctx_with_point();
        let src =
            generate_rust_forwarders(&ctx, default_rust_name(&ctx)).unwrap();
        // magnitude_sq is const → *const Point receiver, (&*this).
        assert!(
            src.contains("this: *const Point"),
            "const method should take *const:\n{src}"
        );
        assert!(
            src.contains("(&*this).magnitude_sq()"),
            "const method body should use (&*this):\n{src}"
        );
    }

    #[test]
    fn mut_method_uses_mut_ptr_and_mut_ref() {
        let (ctx, _) = ctx_with_point();
        let src =
            generate_rust_forwarders(&ctx, default_rust_name(&ctx)).unwrap();
        assert!(
            src.contains("this: *mut Point, arg0: i32, arg1: i32"),
            "translate should take *mut Point with args:\n{src}"
        );
        assert!(
            src.contains("(&mut *this).translate(arg0, arg1)"),
            "mut method body should use (&mut *this):\n{src}"
        );
    }

    #[test]
    fn every_body_is_wrapped_by_the_panic_guard() {
        let (ctx, _) = ctx_with_point();
        let src =
            generate_rust_forwarders(&ctx, default_rust_name(&ctx)).unwrap();
        // Helper definition at the top of the file.
        assert!(
            src.contains("fn __rustcc_guard"),
            "panic guard definition missing:\n{src}"
        );
        assert!(
            src.contains("::std::panic::catch_unwind"),
            "catch_unwind missing from guard:\n{src}"
        );
        assert!(
            src.contains("::std::process::abort()"),
            "abort() missing from guard:\n{src}"
        );
        // Every emitted body invokes the guard. For the Point
        // fixture that's dtor + ctor + 2 methods = 4 invocations.
        assert_eq!(
            src.matches("__rustcc_guard(").count(),
            4,
            "wrong guard call count:\n{src}"
        );
    }

    #[test]
    fn record_return_type_renders_as_rust_struct_name() {
        // Point::make(i32, i32) -> Point — record by value as
        // return. Previously rejected with UnsupportedType; now
        // should render as `-> Point`.
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = ctx.intern_type(CxxType::Int {
            signed: true,
            width: IntWidth::I32,
        });
        let point_id = ctx.define_rust_class(ClassDef {
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
        let point_ty = ctx.intern_type(CxxType::Record(point_id));
        // Attach a static-style method to Point that returns a Point.
        // `make` is not a ctor (no `new`/`OtherCtor` special), so it
        // exercises the record-return path rather than the ctor
        // `ptr::write` path.
        ctx.class_mut(point_id).methods.push(MethodDef { access: Default::default(),
            name: MethodName::Ident(Ident("make".into())),
            sig: FnSig {
                params: vec![i32_, i32_],
                ret: point_ty,
                cv: CvQual::default(),
                ref_q: None,
                variadic: false,
                noexcept: true,
            },
            virtuality: Virtuality::NonVirtual,
            vtable_index: None,
            special: None,
        });

        // Default config (stock-rustc compatible): explicit
        // `__sret` first arg under `extern "C"`. Matches Itanium
        // sret on x86_64 SysV by ABI coincidence (sret in rdi ==
        // first ptr arg in rdi); broken on AAPCS64 (sret in x8).
        let src_default =
            generate_rust_forwarders(&ctx, default_rust_name(&ctx)).unwrap();
        assert!(
            src_default.contains("__sret: *mut Point"),
            "default record return didn't emit sret arg: {src_default}"
        );
        assert!(
            src_default.contains("::core::ptr::write(__sret,"),
            "default record return didn't write into sret slot: {src_default}"
        );

        // Fork-only config: `extern "C++"` + return-by-value.
        // Relies on the fork's `compute_cxx_abi_info` overlay to
        // force-indirect ADT returns per-target.
        let src_cpp = generate_rust_forwarders_with(
            &ctx,
            default_rust_name(&ctx),
            ForwarderConfig {
                record_return_abi: RecordReturnAbi::ExternCpp,
            },
        )
        .unwrap();
        assert!(
            src_cpp.contains("extern \"C++\""),
            "ExternCpp record return didn't switch ABI: {src_cpp}"
        );
        assert!(
            src_cpp.contains("-> Point"),
            "ExternCpp record return didn't return Point by value: {src_cpp}"
        );
    }

    #[test]
    fn record_by_value_parameter_renders_as_rust_struct_name() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = ctx.intern_type(CxxType::Int {
            signed: true,
            width: IntWidth::I32,
        });
        let void_ = ctx.intern_type(CxxType::Void);
        let point_id = ctx.define_rust_class(ClassDef {
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
        let point_ty = ctx.intern_type(CxxType::Record(point_id));
        ctx.class_mut(point_id).methods.push(MethodDef { access: Default::default(),
            name: MethodName::Ident(Ident("absorb".into())),
            sig: FnSig {
                params: vec![point_ty],
                ret: void_,
                cv: CvQual::default(),
                ref_q: None,
                variadic: false,
                noexcept: true,
            },
            virtuality: Virtuality::NonVirtual,
            vtable_index: None,
            special: None,
        });

        let src =
            generate_rust_forwarders(&ctx, default_rust_name(&ctx)).unwrap();
        // Record-by-value params go through the caller-destroys
        // Itanium convention: forwarder takes `*const T`, body
        // `ptr::read`s to materialize an owned value.
        assert!(
            src.contains("arg0: *const Point"),
            "record-by-value param didn't render as *const T:\n{src}"
        );
        assert!(
            src.contains("::core::ptr::read(arg0)"),
            "record-by-value param didn't use ptr::read:\n{src}"
        );
    }

    #[test]
    fn record_param_on_drop_type_is_rejected() {
        // Types with a user-declared `impl Drop` can't safely use
        // the ptr::read + caller-destroys convention — the callee-
        // and caller- destructors would both fire. Generator must
        // refuse.
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let i32_ = ctx.intern_type(CxxType::Int {
            signed: true,
            width: IntWidth::I32,
        });
        let void_ = ctx.intern_type(CxxType::Void);
        // Drop-typed Widget has a SpecialMember::Dtor method.
        let widget_id = ctx.define_rust_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Widget".into()))]),
            bases: vec![],
            fields: vec![FieldDef {
                name: Ident("h".into()),
                ty: i32_,
                explicit_align: None,
            }],
            methods: vec![MethodDef { access: Default::default(),
                name: MethodName::Ident(Ident("~Widget".into())),
                sig: FnSig {
                    params: vec![],
                    ret: void_,
                    cv: CvQual::default(),
                    ref_q: None,
                    variadic: false,
                    noexcept: true,
                },
                virtuality: Virtuality::NonVirtual,
                vtable_index: None,
                special: Some(SpecialMember::Dtor),
            }],
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        let widget_ty = ctx.intern_type(CxxType::Record(widget_id));
        // `consume(Widget)` would force the ptr::read double-destroy
        // path.
        ctx.class_mut(widget_id).methods.push(MethodDef { access: Default::default(),
            name: MethodName::Ident(Ident("consume".into())),
            sig: FnSig {
                params: vec![widget_ty],
                ret: void_,
                cv: CvQual::default(),
                ref_q: None,
                variadic: false,
                noexcept: true,
            },
            virtuality: Virtuality::NonVirtual,
            vtable_index: None,
            special: None,
        });

        let err = generate_rust_forwarders(&ctx, default_rust_name(&ctx))
            .expect_err("generator must reject");
        match err {
            ForwarderError::UnsupportedType { kind, .. } => {
                assert!(
                    kind.contains("user `impl Drop`"),
                    "unexpected reason: {kind}"
                );
            }
        }
    }

    #[test]
    fn cxx_origin_classes_do_not_appear_in_forwarders() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        // A C++-origin class should not produce forwarders.
        let _ = ctx.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Imported".into()))]),
            bases: vec![],
            fields: vec![],
            methods: vec![],
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        let src =
            generate_rust_forwarders(&ctx, default_rust_name(&ctx)).unwrap();
        assert!(
            !src.contains("Imported"),
            "C++-origin class leaked into forwarders:\n{src}"
        );
    }
}
