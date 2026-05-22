//! MSVC C++ ABI name mangling.
//!
//! This is the Microsoft Visual C++ name-mangling scheme — distinct
//! from Itanium in nearly every syntactic detail. The scheme is not
//! formally documented by Microsoft; this implementation follows the
//! observed behavior of MSVC 2019/2022 cross-referenced with LLVM's
//! `clang/lib/AST/MicrosoftMangle.cpp`, which has been the
//! authoritative reverse-engineered reference for two decades.
//!
//! ## Surface format
//!
//! All mangled names start with `?`. A function symbol has the shape:
//!
//! ```text
//! ?<name>@<scope-reversed>@@<qualifier><type-info>
//! ```
//!
//! The name (and each scope segment) is followed by `@`; the whole
//! qualified-name section is terminated by an extra `@`, producing the
//! `@@` separator that's the visual fingerprint of MSVC mangling.
//!
//! Scopes are emitted **inside-out**: `N::M::Foo::bar` mangles as
//! `?bar@Foo@M@N@@…`. That feeds the back-reference table — see below.
//!
//! ## Type encoding (highlights)
//!
//! Builtin codes (subset, full table in `builtin_code`):
//! - `X` void, `_N` bool, `D` char, `E` unsigned char,
//! - `F` short, `G` unsigned short, `H` int, `I` unsigned int,
//! - `J` long, `K` unsigned long, `_J` int64_t, `_K` uint64_t,
//! - `M` float, `N` double, `O` long double,
//! - `_W` wchar_t.
//!
//! Pointers / references prefix the pointee with a CV-modifier letter:
//! - `PEA` near pointer to non-cv (64-bit; the `E` is the "extended"
//!   pointer specifier for 64-bit MSVC),
//! - `PEB` near pointer to const,
//! - `PEC` near pointer to volatile,
//! - `PED` near pointer to const volatile,
//! - `AEA` lvalue reference; `$$Q` rvalue reference.
//!
//! Function-type info after `@@` looks like
//! `<access><cc><return><params>@<exc>`:
//! - access: `Q` public-static / non-member, `A`..`P` various
//!   class-member access × cv-qual combinations.
//! - cc (calling convention): `A` __cdecl, `B` __cdecl with `__declspec(dllexport)`,
//!   `C` __pascal, `E` __thiscall, `G` __stdcall, `I` __fastcall.
//! - For functions returning a non-trivially-destructible record by
//!   value, MSVC inserts a `?$` qualifier into the return-type encoding;
//!   we handle that via the same flag plumbing the Itanium side uses
//!   for sret.
//!
//! ## Back-reference compression
//!
//! MSVC mangling uses two parallel back-reference tables:
//! - **Name table** (digits 0–9): records each *name segment* (identifier
//!   that would otherwise be re-emitted). Lookup `0` re-emits slot 0; `9`
//!   re-emits slot 9; after 10 entries the table is closed (no further
//!   names are added, even though more names are emitted in full).
//! - **Type table** (digits 0–9 too, separate slot space): records each
//!   *full type encoding* (`PEAH`, `AEAVFoo@@`, etc.). Same 10-slot cap.
//!
//! Both tables reset between top-level symbols. The mangler tracks them
//! via the `BackRefs` struct below.
//!
//! ## Special names
//!
//! - Constructors: `?0` (in the name position; e.g. `??0Foo@@QEAA@XZ`).
//! - Destructors: `?1`.
//! - Operators: `??_2` operator new, `??_3` operator delete, `??H`
//!   operator+, `??G` operator-, etc.
//! - vftable: `??_7<class>@@6B@`.
//! - vbtable: `??_8<class>@@7B@`.
//! - RTTI complete-object-locator: `??_R4<class>@@6B<class>@@@`.
//! - RTTI type descriptor: `??_R0?AVfoo@@@8` (used for typeid).
//!
//! See [`fork/MSVC-PLAN.md`](../../../fork/MSVC-PLAN.md) §B.1 for the
//! full breakdown and references used.

use std::fmt::Write as _;

use crate::ctx::CxxTypeCtx;
use crate::mangle::{CtorVariant, DtorVariant, Symbol};
use crate::target::AbiFlavor;
use crate::ty::{
    ClassId, CvQual, CxxType, FloatKind, FnSig, IntWidth, MethodName,
    NameSegment, NestedName, OperatorKind, RefKind, TemplateArg, TypeId,
};

impl CxxTypeCtx {
    /// Mangle `sym` according to the MSVC C++ ABI.
    ///
    /// Returns a string starting with `?` (free function) or `??`
    /// (constructor / destructor / operator / special-name). Caller is
    /// responsible for routing here vs. `mangle()` (Itanium) based on
    /// `self.target().abi_flavor`.
    pub fn mangle_msvc(&self, sym: &Symbol) -> String {
        let mut m = MsvcMangler::new(self);
        m.mangle_symbol(sym);
        m.out
    }
}

// -------- Mangler state ------------------------------------------------

/// MSVC's two parallel back-reference tables. Both cap at 10 entries
/// (digits 0–9) and reset per top-level symbol.
///
/// The *name* table is keyed by identifier text (each scope segment
/// of a qualified name is a separate entry). The *type* table is
/// keyed by `TypeId` — the semantic type identity — so that the
/// same type (even when its rendered form changes because inner
/// names compress via the name table) matches as a single entry.
/// This matches MSVC's "same param type repeats compress to a
/// digit" behavior: e.g., `void f(V, V)` mangles as
/// `?f@@YAXUV@@0@Z` with the second `V` compressed to `0`
/// regardless of whether the inner name "V" is already
/// back-referenced.
#[derive(Default)]
struct BackRefs {
    /// Identifier name slots. Entries are added in emission order; once
    /// 10 names have been recorded, no further names enter the table
    /// (subsequent names still emit in full, they just don't compress).
    names: Vec<String>,
    /// Type slots, keyed by `TypeId`. Same 10-entry cap. Builtin
    /// types (single-character codes like `H`, `M`) are never
    /// recorded here — the table is for compound types only.
    types: Vec<TypeId>,
}

impl BackRefs {
    /// Look up an identifier in the name table. Returns the slot digit
    /// (`'0'`..`'9'`) if `name` is already there, otherwise `None`.
    /// Side-effect-free; the caller decides whether to record on miss.
    fn find_name(&self, name: &str) -> Option<char> {
        self.names
            .iter()
            .position(|n| n == name)
            .map(|i| (b'0' + i as u8) as char)
    }

    /// Add `name` to the table if there's room. Capacity 10, FIFO with
    /// permanent insertion (no eviction — once the table fills, no
    /// new names enter, but existing entries stay valid for lookup).
    fn record_name(&mut self, name: &str) {
        if self.names.len() < 10 && !self.names.iter().any(|n| n == name) {
            self.names.push(name.to_string());
        }
    }

    fn find_type(&self, ty: TypeId) -> Option<char> {
        self.types
            .iter()
            .position(|t| *t == ty)
            .map(|i| (b'0' + i as u8) as char)
    }

    fn record_type(&mut self, ty: TypeId) {
        if self.types.len() < 10 && !self.types.iter().any(|t| *t == ty) {
            self.types.push(ty);
        }
    }
}

struct MsvcMangler<'a> {
    ctx: &'a CxxTypeCtx,
    out: String,
    back_refs: BackRefs,
}

/// Bundle of arguments to `emit_function_info`. Replaces a function
/// signature with bool/cv plumbing that gradually expanded across
/// callers — caller now spells out exactly which slot variants apply.
struct FnInfo<'a> {
    sig: &'a FnSig,
    /// `true` for class members (Method/Ctor/Dtor); `false` for free
    /// functions. Decides whether `E<this-cv>` is interposed before
    /// the calling convention.
    is_member: bool,
    /// Implicit-`this` qualifier. Ignored for `is_member == false`.
    cv: CvQual,
    /// `true` for ctors and dtors — emit `@` in the return-type slot
    /// instead of encoding any type. (C++ source has no return type
    /// for these.)
    no_return_type: bool,
    /// `true` for `virtual`-declared member functions. Selects `U`
    /// instead of `Q` for the access letter. Ignored when
    /// `is_member == false`.
    is_virtual: bool,
}

impl<'a> MsvcMangler<'a> {
    fn new(ctx: &'a CxxTypeCtx) -> Self {
        Self {
            ctx,
            out: String::new(),
            back_refs: BackRefs::default(),
        }
    }

    fn mangle_symbol(&mut self, sym: &Symbol) {
        match sym {
            Symbol::Function { scope, name, sig } => {
                // `?<name>@<scope-reversed>@@<info>`. The function
                // name IS recorded in the back-ref table — MSVC's
                // mangler records every identifier in appearance
                // order, including the symbol's own leaf name. (The
                // first emission still spells out the name; later
                // appearances of the same identifier compress.)
                self.out.push('?');
                self.emit_unqualified_name(&name.0);
                self.emit_qualified_name_tail(&scope.0);
                self.emit_function_info(FnInfo {
                    sig,
                    is_member: false,
                    cv: sig.cv,
                    no_return_type: false,
                    is_virtual: false,
                });
            }
            Symbol::Method { class, name, sig } => {
                self.out.push('?');
                self.emit_method_name(name);
                let class_path = &self.ctx.class(*class).name.0;
                self.emit_qualified_name_tail(class_path);
                let is_virtual = self.lookup_method_is_virtual(*class, name, sig);
                self.emit_function_info(FnInfo {
                    sig,
                    is_member: true,
                    cv: sig.cv,
                    no_return_type: false,
                    is_virtual,
                });
            }
            Symbol::Ctor { class, variant, sig } => {
                // Ctor name slot is `?0` (always — MSVC has a single
                // ctor "variant" mangled symbol; the unified-vs-base
                // vs allocating distinction the Itanium ABI carries
                // does not exist in MSVC's ABI. We still accept the
                // variant parameter for shape parity with the
                // Itanium mangler, but emit the same string for all
                // three variants. The C1/C2/C3 distinction is
                // collapsed by `Symbol::Ctor` at the symbol-emit
                // layer; downstream codegen invokes the single
                // mangled name.)
                let _ = variant; // intentionally unused for MSVC
                self.out.push_str("??0");
                let class_path = &self.ctx.class(*class).name.0;
                self.emit_qualified_name_tail(class_path);
                let cv = CvQual::default();
                // Constructors are never virtual.
                self.emit_function_info(FnInfo {
                    sig,
                    is_member: true,
                    cv,
                    no_return_type: true,
                    is_virtual: false,
                });
            }
            Symbol::Dtor { class, variant } => {
                // Dtor: `??1Class@@<info>` (vector deleting dtor is
                // a separate symbol `??_E`, scalar deleting is
                // `??_G`. The D2/D1/D0 distinction maps roughly to
                // base / complete / deleting; for the layer this
                // crate models, we emit the base (`??1`) and let
                // the caller request the deleting variants
                // separately via vtable entries.)
                let _ = variant;
                self.out.push_str("??1");
                let class_path = &self.ctx.class(*class).name.0;
                self.emit_qualified_name_tail(class_path);
                // Dtor: no return type slot, no params. We use a
                // sentinel TypeId — `no_return_type: true` means
                // `emit_function_info` never reads `sig.ret`, so the
                // `TypeId(u32::MAX)` value is never resolved.
                let dummy_sig = FnSig {
                    params: vec![],
                    ret: TypeId(u32::MAX),
                    cv: CvQual::default(),
                    ref_q: None,
                    variadic: false,
                    noexcept: false,
                };
                // Destructor virtuality: any class with a virtual
                // base or any virtual method has a virtual dtor;
                // explicit `virtual ~T()` also marks it. Look it up.
                let is_virtual = self.dtor_is_virtual(*class);
                self.emit_function_info(FnInfo {
                    sig: &dummy_sig,
                    is_member: true,
                    cv: CvQual::default(),
                    no_return_type: true,
                    is_virtual,
                });
            }
            Symbol::VTable(class) => {
                // vftable: `??_7Class@@6B@`. The `6B` indicates
                // const-data of class storage; the trailing `@` is
                // the empty "for which base" name (primary table).
                self.out.push_str("??_7");
                let class_path = &self.ctx.class(*class).name.0;
                self.emit_qualified_name_tail(class_path);
                self.out.push_str("6B@");
            }
            Symbol::TypeInfo(class) => {
                // RTTI complete-object-locator: `??_R4Class@@6BClass@@@`.
                // The trailing `Class@@@` repeats the class name — this
                // is the "for-class" qualifier (vftable layout location).
                self.out.push_str("??_R4");
                let class_path = &self.ctx.class(*class).name.0;
                self.emit_qualified_name_tail(class_path);
                self.out.push_str("6B");
                self.emit_qualified_name_tail(class_path);
                self.out.push('@');
            }
            Symbol::TypeInfoName(class) => {
                // RTTI type descriptor: `??_R0?AVClass@@@8`.
                // The `?AV` prefix is "?A" (storage modifier:
                // local-symbol no-name) + `V` (class type).
                self.out.push_str("??_R0?AV");
                let class_path = &self.ctx.class(*class).name.0;
                self.emit_qualified_name_tail(class_path);
                self.out.push_str("@8");
            }
            Symbol::Variable { scope, name, ty } => {
                // Free / static-member variable:
                // `?<name>@<scope>@@3<type><cv>`. Trailing digit `3`
                // indicates "static data member with external linkage";
                // local statics use `4`, threadlocals use `0..`.
                self.out.push('?');
                self.emit_unqualified_name(&name.0);
                self.emit_qualified_name_tail(&scope.0);
                self.out.push('3');
                let mut subout = String::new();
                std::mem::swap(&mut self.out, &mut subout);
                self.emit_type(*ty);
                std::mem::swap(&mut self.out, &mut subout);
                self.out.push_str(&subout);
                // Variables don't carry their own cv on the symbol
                // line; the type encoding above already covers any
                // pointer-level cv. Trailing `A` = no top-level cv.
                self.out.push('A');
            }
            Symbol::GuardVariable { for_var } => {
                // MSVC TLS guard / dynamic initializer guard:
                // `?$TSS<n>@?<func>@@4HA` in the general case; for the
                // narrower "static-var initialization guard" Itanium
                // models with `_ZGV`, MSVC uses
                // `?$S1@?1??func@@4HA` patterns. For the symbol layer
                // this crate models (a single fully-qualified guard
                // for a top-level variable), we emit the simpler
                // `?$TSS0@?<var>@@4HA` form. This is approximate; the
                // fork's codegen patch tightens it up against
                // observed clang output.
                self.out.push_str("?$TSS0@?");
                if let Some(last) = for_var.0.last() {
                    let last = last.clone();
                    if let Some(name) = unqualified_name(&last) {
                        self.emit_unqualified_name(name);
                    }
                }
                // Emit any remaining scope (everything except the
                // last segment).
                if for_var.0.len() > 1 {
                    self.emit_qualified_name_tail(&for_var.0[..for_var.0.len() - 1]);
                } else {
                    self.out.push_str("@@");
                }
                self.out.push_str("4HA");
            }
        }
    }

    // -------- Name emission ------------------------------------------

    /// Emit an unqualified identifier with name-table back-reference
    /// compression. `name` is the source-text identifier (no `@`).
    fn emit_unqualified_name(&mut self, name: &str) {
        if let Some(d) = self.back_refs.find_name(name) {
            self.out.push(d);
            return;
        }
        self.out.push_str(name);
        self.out.push('@');
        self.back_refs.record_name(name);
    }

    /// Emit the qualified-name tail: scope segments **in reverse order**
    /// (innermost first), each followed by `@`, terminated by an extra
    /// `@` to produce the canonical `@@` separator.
    ///
    /// Caller has already emitted the unqualified-name part (and any
    /// special prefix like `?0`). This method handles everything from
    /// the first scope segment through to the `@@` terminator.
    fn emit_qualified_name_tail(&mut self, scope: &[NameSegment]) {
        for seg in scope.iter().rev() {
            self.emit_name_segment(seg);
        }
        self.out.push('@'); // closes the qualified-name list
    }

    fn emit_name_segment(&mut self, seg: &NameSegment) {
        match seg {
            NameSegment::Namespace(ident)
            | NameSegment::Class(ident)
            | NameSegment::Enum(ident) => {
                self.emit_unqualified_name(&ident.0);
            }
            NameSegment::AnonymousNamespace => {
                // MSVC mangles anonymous namespaces as `?A0x<hash>` —
                // we use a deterministic placeholder for now. The
                // hash is meant to vary between translation units;
                // collisions across TUs are tolerated by the linker
                // because each anonymous namespace's contents are
                // internal-linkage.
                self.out.push_str("?A0x00000000@");
            }
            NameSegment::TemplateSpec { name, args } => {
                // Template specialization: `?$<name>@<targs>@`. The
                // trailing `@` after the args closes the template-
                // args block; the outer `emit_qualified_name_tail`
                // appends one MORE `@` between segments / for the
                // qual-name terminator. So a top-level `Box<int>`
                // type ends up rendered as `U?$Box@H@@`.
                //
                // The whole `?$name@<targs>@` block participates in
                // the name back-reference table as a single unit.
                let mut tmp = String::new();
                std::mem::swap(&mut self.out, &mut tmp);
                self.out.push_str("?$");
                self.out.push_str(&name.0);
                self.out.push('@');
                for arg in args {
                    match arg {
                        TemplateArg::Type(ty) => self.emit_type_nested(*ty),
                    }
                }
                self.out.push('@'); // close template-args block
                let chunk = std::mem::take(&mut self.out);
                std::mem::swap(&mut self.out, &mut tmp);
                if let Some(d) = self.back_refs.find_name(&chunk) {
                    self.out.push(d);
                } else {
                    self.out.push_str(&chunk);
                    self.back_refs.record_name(&chunk);
                }
            }
        }
    }

    fn emit_method_name(&mut self, name: &MethodName) {
        match name {
            MethodName::Ident(ident) => self.emit_unqualified_name(&ident.0),
            MethodName::Operator(op) => self.out.push_str(operator_code(*op)),
            MethodName::ConversionTo(ty) => {
                // Conversion operator: `??B` (cast to T). The target
                // type follows in the return-position slot.
                self.out.push_str("?B");
                let _ = ty;
            }
        }
    }

    // -------- Function-info block ------------------------------------

    /// Emit `<access>[E<this-cv>]<cc><return-or-@><params>[<exc>]Z`
    /// after the qualified name.
    ///
    /// Layout breakdown:
    /// - **Access letter.** `Y` for free functions, `Q` for public
    ///   member functions (we don't model protected/private since the
    ///   linker can't see access anyway).
    /// - **Extended this-pointer marker.** On 64-bit MSVC targets,
    ///   every member function gets an `E` here to mark its `this`
    ///   pointer as 64-bit. Free functions don't.
    /// - **This-cv letter.** Member functions emit a CV qualifier
    ///   for the implicit `this` (`A`/`B`/`C`/`D`). Free functions
    ///   skip this slot entirely.
    /// - **Calling convention.** `A` for `__cdecl` (the only one we
    ///   model right now; `__stdcall`, `__fastcall`, etc. live behind
    ///   target-CC plumbing not yet wired up).
    /// - **Return type.** Either the encoded type, or `@` for
    ///   ctors/dtors that have no return-type slot.
    /// - **Params.** `X` for `(void)`, otherwise the type-sequence
    ///   followed by `@` terminator.
    /// - **Exception spec.** `_E` prefix for `noexcept`, then `Z`
    ///   universally.
    fn emit_function_info(&mut self, info: FnInfo<'_>) {
        if info.is_member {
            // `Q` for public non-virtual, `U` for public virtual.
            // (Protected and private have their own letter ranges
            // that this layer doesn't yet model; access doesn't
            // affect linkage so all public is a safe default for
            // codegen-relevant mangling.)
            self.out.push(if info.is_virtual { 'U' } else { 'Q' });
            // `E` marks a 64-bit member function's `this` pointer.
            // On 32-bit MSVC this slot is empty; on 64-bit it's
            // always `E`.
            if self.is_64_bit() {
                self.out.push('E');
            }
            // This-cv as a standalone letter.
            self.out.push(ptr_cv_letter(info.cv));
        } else {
            // Free function: single `Y` letter encompassing the
            // no-this, public, no-cv combination.
            self.out.push('Y');
        }
        // Calling convention — always `A` (__cdecl) for now.
        self.out.push('A');
        // Return type. Ctors and dtors collapse this slot to a
        // single `@` because they have no return-type in the C++
        // source. Class-by-value returns get a `?A` storage-class
        // prefix (MSVC's "no-cv UDT return value" marker) — clang
        // emits it for every record-typed return, trivially
        // destructible or not.
        if info.no_return_type {
            self.out.push('@');
        } else {
            if matches!(
                self.ctx.type_of(info.sig.ret),
                CxxType::Record(_) | CxxType::Enum { .. }
            ) {
                self.out.push_str("?A");
            }
            self.emit_type(info.sig.ret);
        }
        // Params.
        if info.sig.params.is_empty() {
            self.out.push('X');
        } else {
            for p in &info.sig.params {
                self.emit_type(*p);
            }
            self.out.push('@');
        }
        if info.sig.noexcept {
            self.out.push_str("_E");
        }
        self.out.push('Z');
    }

    fn is_64_bit(&self) -> bool {
        self.ctx.target().pointer_width_bits == 64
            && matches!(self.ctx.target().abi_flavor, AbiFlavor::Msvc)
    }

    /// Look up the virtuality of `Class::name(sig)` in the class's
    /// method list. Returns `true` for `Virtual` and `PureVirtual`.
    /// Returns `false` when no matching method exists — that path
    /// is taken by test fixtures that build a `Symbol::Method`
    /// without populating the class's `.methods`, in which case
    /// the non-virtual default `Q` is the right output.
    fn lookup_method_is_virtual(
        &self,
        class: ClassId,
        name: &MethodName,
        sig: &FnSig,
    ) -> bool {
        for m in &self.ctx.class(class).methods {
            if &m.name == name
                && m.sig.params == sig.params
                && m.sig.cv == sig.cv
            {
                return matches!(
                    m.virtuality,
                    crate::ty::Virtuality::Virtual
                        | crate::ty::Virtuality::PureVirtual
                );
            }
        }
        false
    }

    /// A class's destructor is virtual if any explicit dtor entry in
    /// the class's method list is marked virtual, OR if any base of
    /// the class has a virtual dtor (the implicit-virtual-dtor
    /// rule). Approximated as: any virtual method on the class or
    /// any base chain.
    fn dtor_is_virtual(&self, class: ClassId) -> bool {
        // Direct check.
        for m in &self.ctx.class(class).methods {
            if matches!(m.special, Some(crate::ty::SpecialMember::Dtor)) {
                return matches!(
                    m.virtuality,
                    crate::ty::Virtuality::Virtual
                        | crate::ty::Virtuality::PureVirtual
                );
            }
        }
        // Inherited virtual dtor from a polymorphic base.
        self.ctx
            .class(class)
            .bases
            .iter()
            .any(|b| self.dtor_is_virtual(b.class))
    }

    // -------- Type emission ------------------------------------------

    /// Emit a type encoding at the **top level** of a parameter (or
    /// return) position.
    ///
    /// MSVC's type back-reference table records compound types
    /// keyed by *semantic type identity*. The table is consulted
    /// only at top-level type positions — typically each parameter
    /// of a function signature, plus the return type. Nested types
    /// inside modifiers (the pointee of a `Ptr`, the pointee of a
    /// `Ref`, the element of an `Array`) bypass the type table and
    /// rely on the name back-reference table for compression. This
    /// matches clang's observed behavior: `void f(V, V)` emits
    /// `?f@@YAXUV@@0@Z` (second `V` compresses via type table),
    /// `void g(V, V&)` emits `?g@@YAXUV@@AEAU1@@Z` (different
    /// top-level types — no type table hit; the `V` inside `V&`
    /// uses only the name back-ref).
    fn emit_type(&mut self, ty: TypeId) {
        let cxx = self.ctx.type_of(ty).clone();
        // Builtin types: never participate in the type table. Their
        // rendered form is already a single character.
        if matches!(
            cxx,
            CxxType::Void | CxxType::Bool | CxxType::Int { .. } | CxxType::Float { .. }
        ) {
            self.emit_type_body(&cxx);
            return;
        }
        // Compound type: check the type table by TypeId.
        if let Some(d) = self.back_refs.find_type(ty) {
            self.out.push(d);
            return;
        }
        // First emission — render body normally. Nested types inside
        // it route through `emit_type_nested` so they don't consult
        // the type table. Record this TypeId in the table after.
        self.emit_type_body(&cxx);
        self.back_refs.record_type(ty);
    }

    /// Emit a type encoding at a **nested** position — inside a
    /// modifier wrapper (`Ptr` pointee, `Ref` pointee, `Array`
    /// element). Skips the type back-reference table; only the
    /// name table is consulted (transparently, via
    /// `emit_qualified_name_tail`).
    fn emit_type_nested(&mut self, ty: TypeId) {
        let cxx = self.ctx.type_of(ty).clone();
        self.emit_type_body(&cxx);
    }

    fn emit_type_body(&mut self, ty: &CxxType) {
        match ty {
            CxxType::Void => self.out.push('X'),
            CxxType::Bool => self.out.push_str("_N"),
            CxxType::Int { signed, width } => {
                self.out.push_str(int_code(*signed, *width));
            }
            CxxType::Float { kind } => self.out.push(float_code(*kind)),
            CxxType::Ptr { pointee, cv } => {
                // 64-bit: `PE` + CV-letter (`A`/`B`/`C`/`D`).
                self.out.push_str("PE");
                self.out.push(ptr_cv_letter(*cv));
                // Nested position: don't consult the type table.
                self.emit_type_nested(*pointee);
            }
            CxxType::Ref { pointee, kind, cv } => {
                let prefix = match kind {
                    RefKind::Lvalue => "AE",
                    RefKind::Rvalue => "$$Q",
                };
                self.out.push_str(prefix);
                self.out.push(ptr_cv_letter(*cv));
                self.emit_type_nested(*pointee);
            }
            CxxType::Array { elem, len } => {
                // `_O` prefix + element + length. MSVC's array
                // encoding is `_O<bound><element>` with bound as
                // decimal; for now we emit the bound as a literal
                // digit run.
                let _ = write!(self.out, "_O{len}");
                self.emit_type_nested(*elem);
            }
            CxxType::Record(class_id) => {
                // Class type: `V<qualified-name>@@`.
                // Struct vs class is distinguished in MSVC mangling by
                // `U` vs `V` respectively (and `T` for union).
                let letter = record_kind_letter(self.ctx, *class_id);
                self.out.push(letter);
                let path = self.ctx.class(*class_id).name.0.clone();
                self.emit_qualified_name_tail(&path);
            }
            CxxType::Enum { name, underlying, .. } => {
                // Enum: `W4<qualified-name>@@`.
                // The `4` indicates the underlying type is the default
                // (`int`); when explicit, codes 0..7 select from
                // a fixed lookup table (char/short/int/long/ulong/uchar/ushort/uint).
                let code = enum_underlying_code(self.ctx.type_of(*underlying));
                self.out.push('W');
                self.out.push(code);
                let path = name.0.clone();
                self.emit_qualified_name_tail(&path);
            }
            CxxType::Fn(_) => {
                // Function type encoded inline: rare outside member
                // pointers. Emit a placeholder for now.
                self.out.push_str("P6A?@Z");
            }
            CxxType::MemberPtr { .. } => {
                // Pointer-to-member: `P8<class>@@A<sig>` in full; we
                // emit a placeholder.
                self.out.push_str("P8?@@A?@Z");
            }
        }
    }

}

fn unqualified_name(seg: &NameSegment) -> Option<&str> {
    match seg {
        NameSegment::Namespace(i) | NameSegment::Class(i) | NameSegment::Enum(i) => Some(&i.0),
        NameSegment::TemplateSpec { name, .. } => Some(&name.0),
        NameSegment::AnonymousNamespace => None,
    }
}

/// Backwards-compatibility helper: the dispatcher passes a
/// `NestedName` and we need to fan it into the per-segment emit. Kept
/// outside `MsvcMangler` so the dispatcher doesn't need to hold the
/// mangler around.
#[allow(dead_code)]
pub(crate) fn render_nested(ctx: &CxxTypeCtx, name: &NestedName) -> String {
    let mut m = MsvcMangler::new(ctx);
    m.emit_qualified_name_tail(&name.0);
    m.out
}

// -------- Code tables --------------------------------------------------

fn int_code(signed: bool, width: IntWidth) -> &'static str {
    match (signed, width) {
        (true, IntWidth::I8) => "D",   // char
        (false, IntWidth::I8) => "E",  // unsigned char
        (true, IntWidth::I16) => "F",  // short
        (false, IntWidth::I16) => "G", // unsigned short
        (true, IntWidth::I32) => "H",  // int
        (false, IntWidth::I32) => "I", // unsigned int
        (true, IntWidth::I64) => "_J", // int64_t / long long
        (false, IntWidth::I64) => "_K", // uint64_t / unsigned long long
        (true, IntWidth::I128) => "_L", // __int128 — MSVC extension code
        (false, IntWidth::I128) => "_M",
    }
}

fn float_code(kind: FloatKind) -> char {
    match kind {
        FloatKind::F32 => 'M',         // float
        FloatKind::F64 => 'N',         // double
        FloatKind::LongDouble => 'O',  // long double (same as double on MSVC)
    }
}

/// Pointer-CV letter. MSVC encodes pointer / reference qualifiers as:
/// `A` = neither, `B` = const, `C` = volatile, `D` = const volatile.
fn ptr_cv_letter(cv: CvQual) -> char {
    match (cv.is_const, cv.is_volatile) {
        (false, false) => 'A',
        (true, false) => 'B',
        (false, true) => 'C',
        (true, true) => 'D',
    }
}

fn record_kind_letter(ctx: &CxxTypeCtx, class: ClassId) -> char {
    use crate::ty::RecordKind;
    match ctx.class(class).kind {
        RecordKind::Class => 'V',
        RecordKind::Struct => 'U',
        RecordKind::Union => 'T',
    }
}

fn enum_underlying_code(ty: &CxxType) -> char {
    // MSVC encodes the underlying type as a single digit:
    //   0 char, 1 unsigned char, 2 short, 3 unsigned short,
    //   4 int, 5 unsigned int, 6 long, 7 unsigned long.
    // 64-bit underlying types use `_J`/`_K` instead and the enum
    // prefix becomes `W_J@...` — handled by enum emission elsewhere.
    match ty {
        CxxType::Int { signed: true, width: IntWidth::I8 } => '0',
        CxxType::Int { signed: false, width: IntWidth::I8 } => '1',
        CxxType::Int { signed: true, width: IntWidth::I16 } => '2',
        CxxType::Int { signed: false, width: IntWidth::I16 } => '3',
        CxxType::Int { signed: true, width: IntWidth::I32 } => '4',
        CxxType::Int { signed: false, width: IntWidth::I32 } => '5',
        CxxType::Int { signed: true, width: IntWidth::I64 } => '6',
        CxxType::Int { signed: false, width: IntWidth::I64 } => '7',
        _ => '4', // default int
    }
}

fn operator_code(op: OperatorKind) -> &'static str {
    // MSVC operator codes — all live in the `??` namespace and the
    // unqualified part is the digit/letter combination listed here.
    // Lookup table compiled from MicrosoftMangle.cpp's
    // `mangleOperatorName` switch.
    match op {
        OperatorKind::Plus => "?H",
        OperatorKind::Minus => "?G",
        OperatorKind::Mul => "?D",
        OperatorKind::Div => "?K",
        OperatorKind::Mod => "?L",
        OperatorKind::Assign => "?4",
        OperatorKind::PlusAssign => "?Y",
        OperatorKind::Eq => "?8",
        OperatorKind::Ne => "?9",
        OperatorKind::Lt => "?M",
        OperatorKind::Le => "?N",
        OperatorKind::Gt => "?O",
        OperatorKind::Ge => "?P",
        OperatorKind::Call => "?R",
        OperatorKind::Index => "?A",
        OperatorKind::Deref => "?C",
        OperatorKind::PreIncr => "?E",
        OperatorKind::PreDecr => "?F",
    }
}

// Variants stay accessible from this module for parity with the
// Itanium mangler's API shape. They're a no-op for MSVC because the
// MSVC ABI doesn't carry the Itanium C1/C2/C3 / D0/D1/D2 distinction
// at the symbol level.
#[allow(dead_code)]
fn _accept_ctor_variant(_: CtorVariant) {}
#[allow(dead_code)]
fn _accept_dtor_variant(_: DtorVariant) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::Target;
    use crate::ty::{
        ClassDef, FnSig, Ident, NameSegment, NestedName, RecordKind,
    };

    fn ctx() -> CxxTypeCtx {
        CxxTypeCtx::new(Target::x86_64_pc_windows_msvc())
    }

    fn intern_int(ctx: &mut CxxTypeCtx, signed: bool, width: IntWidth) -> TypeId {
        ctx.intern_type(CxxType::Int { signed, width })
    }

    fn intern_void(ctx: &mut CxxTypeCtx) -> TypeId {
        ctx.intern_type(CxxType::Void)
    }

    fn class(name: &str, kind: RecordKind) -> ClassDef {
        ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident(name.into()))]),
            bases: vec![],
            fields: vec![],
            methods: vec![],
            kind,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        }
    }

    #[test]
    fn free_function_int_int() {
        // `int foo(int)` -> `?foo@@YAHH@Z`
        let mut c = ctx();
        let i = intern_int(&mut c, true, IntWidth::I32);
        let sig = FnSig {
            params: vec![i],
            ret: i,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        };
        let mangled = c.mangle_msvc(&Symbol::Function {
            scope: NestedName(vec![]),
            name: Ident("foo".into()),
            sig,
        });
        assert_eq!(mangled, "?foo@@YAHH@Z");
    }

    #[test]
    fn free_function_void_no_args() {
        // `void foo(void)` -> `?foo@@YAXXZ`
        let mut c = ctx();
        let v = intern_void(&mut c);
        let sig = FnSig {
            params: vec![],
            ret: v,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        };
        let m = c.mangle_msvc(&Symbol::Function {
            scope: NestedName(vec![]),
            name: Ident("foo".into()),
            sig,
        });
        assert_eq!(m, "?foo@@YAXXZ");
    }

    #[test]
    fn member_const_method() {
        // `class C { int f(int) const; }` -> `?f@C@@QEBAHH@Z`
        // Q = public access, E = 64-bit this, B = const-this,
        // A = __cdecl, H = int return, H = int param, @ = end
        // params, Z = no exception spec.
        let mut c = ctx();
        let cid = c.define_class(class("C", RecordKind::Class));
        let i = intern_int(&mut c, true, IntWidth::I32);
        let sig = FnSig {
            params: vec![i],
            ret: i,
            cv: CvQual { is_const: true, is_volatile: false },
            ref_q: None,
            variadic: false,
            noexcept: false,
        };
        let m = c.mangle_msvc(&Symbol::Method {
            class: cid,
            name: MethodName::Ident(Ident("f".into())),
            sig,
        });
        assert_eq!(m, "?f@C@@QEBAHH@Z");
    }

    #[test]
    fn ctor_emits_double_question_zero() {
        // `class C { C(); }` -> `??0C@@QEAA@XZ`
        let mut c = ctx();
        let cid = c.define_class(class("C", RecordKind::Class));
        let v = intern_void(&mut c);
        let sig = FnSig {
            params: vec![],
            ret: v,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        };
        let m = c.mangle_msvc(&Symbol::Ctor {
            class: cid,
            variant: CtorVariant::C1,
            sig,
        });
        assert_eq!(m, "??0C@@QEAA@XZ");
    }

    #[test]
    fn dtor_emits_double_question_one() {
        let mut c = ctx();
        let cid = c.define_class(class("C", RecordKind::Class));
        let m = c.mangle_msvc(&Symbol::Dtor {
            class: cid,
            variant: DtorVariant::D1,
        });
        // `??1C@@QEAA@XZ` (member dtor, no params, no ret)
        assert_eq!(m, "??1C@@QEAA@XZ");
    }

    #[test]
    fn vtable_emits_question_underscore_seven() {
        let mut c = ctx();
        let cid = c.define_class(class("C", RecordKind::Class));
        let m = c.mangle_msvc(&Symbol::VTable(cid));
        assert_eq!(m, "??_7C@@6B@");
    }

    #[test]
    fn nested_namespace_reverses_segments() {
        // `void N::M::foo()` -> `?foo@M@N@@YAXXZ`
        let mut c = ctx();
        let v = intern_void(&mut c);
        let sig = FnSig {
            params: vec![],
            ret: v,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        };
        let scope = NestedName(vec![
            NameSegment::Namespace(Ident("N".into())),
            NameSegment::Namespace(Ident("M".into())),
        ]);
        let m = c.mangle_msvc(&Symbol::Function {
            scope,
            name: Ident("foo".into()),
            sig,
        });
        assert_eq!(m, "?foo@M@N@@YAXXZ");
    }

    #[test]
    fn name_back_reference_compresses_repeated_scope() {
        // `int N::f(N::S)` where S is in namespace N. MSVC records
        // every identifier (including the function's own leaf
        // name) in appearance order. After emitting `?f@N@@`, the
        // name table is `[f, N]`. The parameter type then emits
        // `VS@` (new name → slot 2) and looks up `N` (slot 1) →
        // back-ref `1`.
        let mut c = ctx();
        let i = intern_int(&mut c, true, IntWidth::I32);
        let sid = c.define_class(ClassDef {
            name: NestedName(vec![
                NameSegment::Namespace(Ident("N".into())),
                NameSegment::Class(Ident("S".into())),
            ]),
            bases: vec![],
            fields: vec![],
            methods: vec![],
            kind: RecordKind::Class,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        let sty = c.intern_type(CxxType::Record(sid));
        let sig = FnSig {
            params: vec![sty],
            ret: i,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        };
        let m = c.mangle_msvc(&Symbol::Function {
            scope: NestedName(vec![NameSegment::Namespace(Ident("N".into()))]),
            name: Ident("f".into()),
            sig,
        });
        // Expected: `?f@N@@YAHVS@1@@Z`
        // Breakdown:
        //   ?f       — function name (slot 0)
        //   @N       — scope (slot 1)
        //   @@       — terminator
        //   YA       — free fn + __cdecl
        //   H        — return int
        //   V        — class type ahead
        //   S@       — class name "S" (slot 2)
        //   1        — back-ref to N at slot 1
        //   @        — end of class qualified-name
        //   @        — end of parameter list
        //   Z        — no exception spec
        assert_eq!(m, "?f@N@@YAHVS@1@@Z");
    }

    #[test]
    fn ptr_to_const_int() {
        // `void f(const int *)` -> `?f@@YAXPEBH@Z`
        let mut c = ctx();
        let v = intern_void(&mut c);
        let i = intern_int(&mut c, true, IntWidth::I32);
        let p = c.intern_type(CxxType::Ptr {
            pointee: i,
            cv: CvQual { is_const: true, is_volatile: false },
        });
        let sig = FnSig {
            params: vec![p],
            ret: v,
            cv: CvQual::default(),
            ref_q: None,
            variadic: false,
            noexcept: false,
        };
        let m = c.mangle_msvc(&Symbol::Function {
            scope: NestedName(vec![]),
            name: Ident("f".into()),
            sig,
        });
        assert_eq!(m, "?f@@YAXPEBH@Z");
    }
}
