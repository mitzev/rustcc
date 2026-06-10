//! Itanium C++ ABI name mangling.
//!
//! See `docs/rustc_abi_cxx.md §6`.
//!
//! **Substitution support:** the mangler maintains the Itanium seq-id
//! substitution table per §5.1.6. Every compound entity is registered as
//! it's emitted: nested-name prefixes, record types, CV-qualified wrappers,
//! pointer/reference/array constructors. Bare builtin types are never
//! registered (per spec). Repeated occurrences are emitted as
//! `S_`/`S0_`/`S1_`/... using base-36 seq-ids.
//!
//! Standard library abbreviations (`St`, `Ss`, `Sa`, etc.) are **not**
//! implemented — anything in `std::` spells its scope out.
//!
//! Known limitations (carried from the first pass):
//! - Enum parameters fall back to the underlying integer type (the IR
//!   doesn't yet carry the enum's name).
//! - Anonymous-namespace mangling uses a placeholder.
//! - Function-type and member-pointer parameters emit stubs.
//! - Template instantiations are not supported.

use std::fmt::Write as _;

use crate::ctx::CxxTypeCtx;
use crate::ty::{
    ClassId, CvQual, CxxType, FloatKind, FnSig, Ident, IntWidth, MethodName,
    NameSegment, NestedName, OperatorKind, RefKind, TemplateArg, TypeId,
};

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Symbol {
    Function {
        scope: NestedName,
        name: Ident,
        sig: FnSig,
    },
    Method {
        class: ClassId,
        name: MethodName,
        sig: FnSig,
    },
    Ctor {
        class: ClassId,
        variant: CtorVariant,
        sig: FnSig,
    },
    Dtor {
        class: ClassId,
        variant: DtorVariant,
    },
    VTable(ClassId),
    TypeInfo(ClassId),
    TypeInfoName(ClassId),
    Variable {
        scope: NestedName,
        name: Ident,
        ty: TypeId,
    },
    GuardVariable {
        for_var: NestedName,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CtorVariant {
    C1,
    C2,
    C3,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DtorVariant {
    D0,
    D1,
    D2,
}

impl CxxTypeCtx {
    /// Mangle dispatcher. Routes to either Itanium
    /// ([`Self::mangle_itanium`]) or MSVC ([`Self::mangle_msvc`])
    /// based on `target().abi_flavor`. Every caller in the workspace
    /// goes through this entry point so the ABI choice is made
    /// centrally.
    pub fn mangle(&self, sym: &Symbol) -> String {
        match self.target().abi_flavor {
            crate::target::AbiFlavor::Itanium => self.mangle_itanium(sym),
            crate::target::AbiFlavor::Msvc => self.mangle_msvc(sym),
        }
    }

    /// Itanium-only mangling entry point. Useful for tests and any
    /// downstream consumer that needs to force Itanium semantics
    /// regardless of `target().abi_flavor`.
    pub fn mangle_itanium(&self, sym: &Symbol) -> String {
        let mut m = Mangler::new(self);
        m.mangle_symbol(sym);
        m.out
    }

    /// The Itanium *bare parameter encoding* of a function signature —
    /// the `<params>` tail of `_ZN…E<params>` (`"v"` for an empty
    /// list, e.g. `"iPKc"` for `(int, const char*)`). v1.13.10 uses
    /// this as the optional third `slot=` field of
    /// `#[rustc_cxx_imported_vtable]` so the fork can disambiguate
    /// overloaded virtuals and signature-check `override fn`s.
    /// Substitution state is local to this call, matching a
    /// standalone declaration's encoding.
    pub fn mangle_itanium_params(&self, sig: &crate::ty::FnSig) -> String {
        let mut m = Mangler::new(self);
        m.emit_params(&sig.params);
        m.out
    }
}

// -------- Substitution-aware mangler -----------------------------------

struct Mangler<'a> {
    ctx: &'a CxxTypeCtx,
    out: String,
    /// Substitution table in seq-id order. Index 0 → `S_`, 1 → `S0_`,
    /// 2 → `S1_`, ...
    subs: Vec<SubKey>,
}

/// Structural key identifying a type or nested-name entity for purposes
/// of substitution-table lookup. `Builtin` variants are never looked up
/// by themselves (per spec) but participate in compound keys so that
/// e.g. `int*` equals `int*` when used twice in a signature.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SubKey {
    Builtin(TypeId),
    /// A nested-name or class-type path.
    Path(Vec<NameSegment>),
    CvQualified {
        inner: Box<SubKey>,
        cv: CvQual,
    },
    Pointer(Box<SubKey>),
    Reference {
        inner: Box<SubKey>,
        kind: RefKind,
    },
    Array {
        inner: Box<SubKey>,
        len: u64,
    },
    /// Enum, function, member-pointer — not yet fully modeled.
    Opaque(TypeId),
}

impl<'a> Mangler<'a> {
    fn new(ctx: &'a CxxTypeCtx) -> Self {
        Self {
            ctx,
            out: String::new(),
            subs: Vec::new(),
        }
    }

    fn find(&self, key: &SubKey) -> Option<usize> {
        self.subs.iter().position(|k| k == key)
    }

    fn register(&mut self, key: SubKey) {
        if !self.subs.contains(&key) {
            self.subs.push(key);
        }
    }

    fn emit_seq(&mut self, index: usize) {
        self.out.push('S');
        if index > 0 {
            let _ = write!(self.out, "{}", to_base36((index - 1) as u64));
        }
        self.out.push('_');
    }

    fn mangle_symbol(&mut self, sym: &Symbol) {
        match sym {
            Symbol::Function { scope, name, sig } => {
                self.out.push_str("_Z");
                if scope.0.is_empty() {
                    emit_source_name(name, &mut self.out);
                } else {
                    // Itanium free-function mangling in a namespace:
                    // `_ZN<scope><name>E<params>`. Scope prefixes are
                    // substitutable.
                    self.out.push('N');
                    self.emit_nested_prefix(&scope.0);
                    emit_source_name(name, &mut self.out);
                    self.out.push('E');
                }
                self.emit_params(&sig.params);
            }
            Symbol::Method { class, name, sig } => {
                self.out.push_str("_Z");
                self.out.push('N');
                if sig.cv.is_const {
                    self.out.push('K');
                }
                if sig.cv.is_volatile {
                    self.out.push('V');
                }
                let path = self.ctx.class(*class).name.0.clone();
                self.emit_nested_prefix(&path);
                self.emit_method_name(name);
                self.out.push('E');
                self.emit_params(&sig.params);
            }
            Symbol::Ctor { class, variant, sig } => {
                self.out.push_str("_Z");
                self.out.push('N');
                let path = self.ctx.class(*class).name.0.clone();
                self.emit_nested_prefix(&path);
                self.out.push_str(match variant {
                    CtorVariant::C1 => "C1",
                    CtorVariant::C2 => "C2",
                    CtorVariant::C3 => "C3",
                });
                self.out.push('E');
                self.emit_params(&sig.params);
            }
            Symbol::Dtor { class, variant } => {
                self.out.push_str("_Z");
                self.out.push('N');
                let path = self.ctx.class(*class).name.0.clone();
                self.emit_nested_prefix(&path);
                self.out.push_str(match variant {
                    DtorVariant::D0 => "D0",
                    DtorVariant::D1 => "D1",
                    DtorVariant::D2 => "D2",
                });
                self.out.push('E');
                self.out.push('v');
            }
            Symbol::VTable(class) => {
                self.out.push_str("_ZTV");
                self.emit_class_as_type(*class);
            }
            Symbol::TypeInfo(class) => {
                self.out.push_str("_ZTI");
                self.emit_class_as_type(*class);
            }
            Symbol::TypeInfoName(class) => {
                self.out.push_str("_ZTS");
                self.emit_class_as_type(*class);
            }
            Symbol::Variable { scope, name, .. } => {
                self.out.push_str("_Z");
                if scope.0.is_empty() {
                    emit_source_name(name, &mut self.out);
                } else {
                    self.out.push('N');
                    self.emit_nested_prefix(&scope.0);
                    emit_source_name(name, &mut self.out);
                    self.out.push('E');
                }
            }
            Symbol::GuardVariable { for_var } => {
                self.out.push_str("_ZGV");
                if for_var.0.len() <= 1 {
                    if let Some(seg) = for_var.0.last() {
                        let seg = seg.clone();
                        self.emit_segment(&seg);
                    }
                } else {
                    self.out.push('N');
                    self.emit_nested_prefix(&for_var.0);
                    self.out.push('E');
                }
            }
        }
    }

    fn emit_class_as_type(&mut self, class: ClassId) {
        let path = self.ctx.class(class).name.0.clone();
        if path.len() > 1 {
            self.out.push('N');
        }
        self.emit_nested_prefix(&path);
        if path.len() > 1 {
            self.out.push('E');
        }
    }

    /// Emit a sequence of `NameSegment`s, with substitution lookup for the
    /// longest already-registered leading prefix. Each cumulative prefix
    /// is registered after being emitted.
    fn emit_nested_prefix(&mut self, path: &[NameSegment]) {
        let mut start = 0;
        for i in (0..path.len()).rev() {
            let prefix = &path[..=i];
            let key = SubKey::Path(prefix.to_vec());
            if let Some(idx) = self.find(&key) {
                self.emit_seq(idx);
                start = i + 1;
                break;
            }
        }
        for i in start..path.len() {
            // Clone the segment so the mutable-self template-arg emit
            // path doesn't collide with the immutable borrow of `path`.
            let seg = path[i].clone();
            self.emit_segment(&seg);
            let cum = path[..=i].to_vec();
            self.register(SubKey::Path(cum));
        }
    }

    /// Emit a single name segment. Template specializations receive
    /// `I...E` wrapping with each type argument emitted through
    /// `emit_type`, so they participate in the substitution table.
    fn emit_segment(&mut self, seg: &NameSegment) {
        match seg {
            NameSegment::Namespace(ident)
            | NameSegment::Class(ident)
            | NameSegment::Enum(ident) => {
                emit_source_name(ident, &mut self.out);
            }
            NameSegment::AnonymousNamespace => {
                self.out.push_str("12_GLOBAL__N_1");
            }
            NameSegment::TemplateSpec { name, args } => {
                emit_source_name(name, &mut self.out);
                self.out.push('I');
                for arg in args {
                    match arg {
                        TemplateArg::Type(ty) => {
                            self.emit_type(*ty);
                        }
                        TemplateArg::Integral { value, ty } => {
                            // `<expr-primary> ::= L <type> <number> E`.
                            // The type letter comes from emitting `ty`
                            // (`i`, `m`, `b`, `c`, an enum name, …);
                            // negative numbers use an `n` prefix on the
                            // magnitude (`Lin1E` for `int -1`).
                            self.out.push('L');
                            self.emit_type(*ty);
                            if *value < 0 {
                                self.out.push('n');
                                let _ = write!(self.out, "{}", value.unsigned_abs());
                            } else {
                                let _ = write!(self.out, "{value}");
                            }
                            self.out.push('E');
                        }
                        TemplateArg::Template(nested) => {
                            // A template-template argument mangles as the
                            // bare name prefix (no `I…E`), participating
                            // in the substitution table: `3Box`,
                            // `St6vector`.
                            self.emit_nested_prefix(&nested.0);
                        }
                    }
                }
                self.out.push('E');
            }
        }
    }

    fn emit_method_name(&mut self, name: &MethodName) {
        match name {
            MethodName::Ident(ident) => emit_source_name(ident, &mut self.out),
            MethodName::Operator(op) => self.out.push_str(operator_code(*op)),
            MethodName::ConversionTo(ty) => {
                self.out.push_str("cv");
                self.emit_type(*ty);
            }
        }
    }

    fn emit_params(&mut self, params: &[TypeId]) {
        if params.is_empty() {
            self.out.push('v');
        } else {
            for p in params {
                self.emit_type(*p);
            }
        }
    }

    fn emit_type(&mut self, ty: TypeId) -> SubKey {
        let key = self.type_to_key(ty);
        if !matches!(key, SubKey::Builtin(_)) {
            if let Some(idx) = self.find(&key) {
                self.emit_seq(idx);
                return key;
            }
        }
        self.emit_type_body(ty);
        // Compound types register themselves below. `Path` keys were
        // registered by `emit_nested_prefix` during body emission, so
        // we skip them here (avoid duplicate).
        match &key {
            SubKey::Builtin(_) => {}
            SubKey::Path(_) => {}
            _ => self.register(key.clone()),
        }
        key
    }

    fn emit_type_body(&mut self, ty: TypeId) {
        match self.ctx.type_of(ty).clone() {
            CxxType::Void => self.out.push('v'),
            CxxType::Bool => self.out.push('b'),
            CxxType::Int { signed, width } => {
                self.out.push(int_code(signed, width));
            }
            CxxType::Float { kind } => self.out.push(float_code(kind)),
            CxxType::Ptr { pointee, cv } => {
                self.out.push('P');
                self.emit_with_possible_cv(pointee, cv);
            }
            CxxType::Ref { pointee, kind, cv } => {
                self.out.push(match kind {
                    RefKind::Lvalue => 'R',
                    RefKind::Rvalue => 'O',
                });
                self.emit_with_possible_cv(pointee, cv);
            }
            CxxType::Array { elem, len } => {
                let _ = write!(self.out, "A{len}_");
                self.emit_type(elem);
            }
            CxxType::Record(class_id) => {
                let path = self.ctx.class(class_id).name.0.clone();
                if path.len() > 1 {
                    self.out.push('N');
                }
                self.emit_nested_prefix(&path);
                if path.len() > 1 {
                    self.out.push('E');
                }
            }
            CxxType::Enum { name, .. } => {
                // Enums mangle as source-names (just like classes) and
                // their paths are substitutable via the same SubKey::Path
                // mechanism, so they participate in the substitution
                // table end-to-end.
                let path = name.0.clone();
                if path.len() > 1 {
                    self.out.push('N');
                }
                self.emit_nested_prefix(&path);
                if path.len() > 1 {
                    self.out.push('E');
                }
            }
            // Function type (Itanium §5.1.5.1): `F <ret> <params> E`.
            // Appears in practice behind a pointer (`PFvP9Fl_WidgetPvE`
            // = `void (*)(Fl_Widget*, void*)` — FLTK's Fl_Callback) —
            // the `P` comes from the enclosing Ptr arm. Was a literal
            // `F?E`, which produced 63 unlinkable symbols in the FLTK
            // bindings and dropped the whole menu/callback surface.
            CxxType::Fn(sig) => {
                self.out.push('F');
                self.emit_type(sig.ret);
                if sig.params.is_empty() {
                    self.out.push('v');
                } else {
                    for &p in &sig.params {
                        self.emit_type(p);
                    }
                }
                if sig.variadic {
                    self.out.push('z');
                }
                self.out.push('E');
            }
            // Pointer-to-member (Itanium §5.1.5): `M <class> <member type>`.
            // For member FUNCTIONS the ref-qualifier-free cv of the
            // member function sits between the class and the `F…E`
            // function type (`int (X::*)(int) const` = `M1XKFiiE`).
            // Pinned against clang in tests/mangle_corpus.rs. Was a
            // literal `M??` placeholder.
            CxxType::MemberPtr { class, pointee } => {
                self.out.push('M');
                let path = self.ctx.class(class).name.0.clone();
                if path.len() > 1 {
                    self.out.push('N');
                }
                self.emit_nested_prefix(&path);
                if path.len() > 1 {
                    self.out.push('E');
                }
                if let CxxType::Fn(sig) = self.ctx.type_of(pointee) {
                    if sig.cv.is_volatile {
                        self.out.push('V');
                    }
                    if sig.cv.is_const {
                        self.out.push('K');
                    }
                }
                self.emit_type(pointee);
            }
        }
    }

    /// Emit a `<pointee>` that may be CV-qualified. When CV is non-empty,
    /// the textual form is `<CV-letters> <inner>` and the `<CV-qualified>`
    /// wrapper is itself a substitutable entity.
    ///
    /// Array normalization (Itanium §5.1.5.2): cv-qualifiers cannot
    /// apply to an array type directly — they attach to the element.
    /// `const T[N]` mangles as `A<N>_K<T>`, not `KA<N>_<T>`. If we
    /// see an array here with non-empty cv, push the cv down.
    fn emit_with_possible_cv(&mut self, inner: TypeId, cv: CvQual) {
        if !cv.is_const && !cv.is_volatile {
            self.emit_type(inner);
            return;
        }
        if let CxxType::Array { elem, len } = *self.ctx.type_of(inner) {
            let _ = write!(self.out, "A{len}_");
            self.emit_with_possible_cv(elem, cv);
            return;
        }
        let inner_key = self.type_to_key(inner);
        let qual_key = SubKey::CvQualified {
            inner: Box::new(inner_key),
            cv,
        };
        if let Some(idx) = self.find(&qual_key) {
            self.emit_seq(idx);
            return;
        }
        emit_cv_letters(cv, &mut self.out);
        self.emit_type(inner);
        self.register(qual_key);
    }

    fn type_to_key(&self, ty: TypeId) -> SubKey {
        match self.ctx.type_of(ty).clone() {
            CxxType::Void
            | CxxType::Bool
            | CxxType::Int { .. }
            | CxxType::Float { .. } => SubKey::Builtin(ty),
            CxxType::Ptr { pointee, cv } => {
                let inner = apply_cv_key(self.type_to_key(pointee), cv);
                SubKey::Pointer(Box::new(inner))
            }
            CxxType::Ref { pointee, kind, cv } => {
                let inner = apply_cv_key(self.type_to_key(pointee), cv);
                SubKey::Reference {
                    inner: Box::new(inner),
                    kind,
                }
            }
            CxxType::Array { elem, len } => SubKey::Array {
                inner: Box::new(self.type_to_key(elem)),
                len,
            },
            CxxType::Record(class_id) => {
                let path = self.ctx.class(class_id).name.0.clone();
                SubKey::Path(path)
            }
            CxxType::Enum { name, .. } => SubKey::Path(name.0.clone()),
            CxxType::Fn(_) | CxxType::MemberPtr { .. } => SubKey::Opaque(ty),
        }
    }
}

// -------- Helpers ------------------------------------------------------

fn apply_cv_key(inner: SubKey, cv: CvQual) -> SubKey {
    if cv.is_const || cv.is_volatile {
        SubKey::CvQualified {
            inner: Box::new(inner),
            cv,
        }
    } else {
        inner
    }
}

fn emit_source_name(ident: &Ident, out: &mut String) {
    let s = &ident.0;
    let _ = write!(out, "{}{}", s.len(), s);
}

fn emit_cv_letters(cv: CvQual, out: &mut String) {
    // Itanium order: `r` (restrict, unmodeled), `V` (volatile), `K` (const).
    if cv.is_volatile {
        out.push('V');
    }
    if cv.is_const {
        out.push('K');
    }
}

fn operator_code(op: OperatorKind) -> &'static str {
    match op {
        OperatorKind::Plus => "pl",
        OperatorKind::Minus => "mi",
        OperatorKind::Mul => "ml",
        OperatorKind::Div => "dv",
        OperatorKind::Mod => "rm",
        OperatorKind::Assign => "aS",
        OperatorKind::PlusAssign => "pL",
        OperatorKind::Eq => "eq",
        OperatorKind::Ne => "ne",
        OperatorKind::Lt => "lt",
        OperatorKind::Le => "le",
        OperatorKind::Gt => "gt",
        OperatorKind::Ge => "ge",
        OperatorKind::Call => "cl",
        OperatorKind::Index => "ix",
        OperatorKind::Deref => "de",
        OperatorKind::PreIncr => "pp",
        OperatorKind::PreDecr => "mm",
    }
}

fn int_code(signed: bool, width: IntWidth) -> char {
    match (signed, width) {
        (true, IntWidth::I8) => 'c',
        (false, IntWidth::I8) => 'h',
        (true, IntWidth::I16) => 's',
        (false, IntWidth::I16) => 't',
        (true, IntWidth::I32) => 'i',
        (false, IntWidth::I32) => 'j',
        // i64 / u64 map to `long long` / `unsigned long long`
        // (Itanium codes `x` / `y`) rather than `long` / `unsigned
        // long` (`l` / `m`). This keeps the generated .hpp and the
        // mangling in agreement across targets: on LP64 (Linux)
        // `long` and `long long` are both 64-bit but distinct types,
        // while on macOS `int64_t` is typedef'd to `long long`. The
        // hpp emitter outputs `long long` explicitly for the same
        // reason — no accidental dependency on `std::int64_t`'s
        // platform-specific underlying type.
        (true, IntWidth::I64) => 'x',
        (false, IntWidth::I64) => 'y',
        (true, IntWidth::I128) => 'n',
        (false, IntWidth::I128) => 'o',
    }
}

fn float_code(kind: FloatKind) -> char {
    match kind {
        FloatKind::F32 => 'f',
        FloatKind::F64 => 'd',
        FloatKind::LongDouble => 'e',
    }
}

/// Base-36 encoding used for substitution sequence ids: 0..=9 map to
/// `'0'..='9'`, 10..=35 map to `'A'..='Z'`. `n` is the raw sub-index
/// minus one (index 0 → `S_`, so index 1 → `S0_` uses base36(0)).
fn to_base36(n: u64) -> String {
    if n == 0 {
        return String::from("0");
    }
    let mut digits = Vec::new();
    let mut n = n;
    while n > 0 {
        let d = (n % 36) as u8;
        let ch = if d < 10 { b'0' + d } else { b'A' + d - 10 };
        digits.push(ch);
        n /= 36;
    }
    digits.reverse();
    String::from_utf8(digits).expect("ascii only")
}

#[cfg(test)]
mod tests {
    use super::to_base36;

    #[test]
    fn base36_encoding() {
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(9), "9");
        assert_eq!(to_base36(10), "A");
        assert_eq!(to_base36(35), "Z");
        assert_eq!(to_base36(36), "10");
        assert_eq!(to_base36(37), "11");
    }
}
