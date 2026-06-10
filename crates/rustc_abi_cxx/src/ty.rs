//! C++-facing type IR. Inputs to every query in this crate.
//!
//! See `docs/rustc_abi_cxx.md §4`.

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClassId(pub(crate) u32);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FieldId(pub(crate) u32);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MethodId(pub(crate) u32);

impl MethodId {
    /// Index back into the owning `class.methods` slice. Used
    /// by `cxx_importer::populate_vtable_indices` (M23) which
    /// reads `VTableEntry::FunctionPointer { method, .. }` and
    /// stamps the slot's rank into the method's
    /// `vtable_index` — no crate-private field access needed.
    pub fn as_index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TypeId(pub(crate) u32);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CxxType {
    Void,
    Bool,
    Int { signed: bool, width: IntWidth },
    Float { kind: FloatKind },
    Ptr { pointee: TypeId, cv: CvQual },
    Ref { pointee: TypeId, kind: RefKind, cv: CvQual },
    Array { elem: TypeId, len: u64 },
    Record(ClassId),
    Enum { name: NestedName, underlying: TypeId, scoped: bool },
    Fn(FnSig),
    MemberPtr { class: ClassId, pointee: TypeId },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum IntWidth {
    I8,
    I16,
    I32,
    I64,
    I128,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FloatKind {
    F32,
    F64,
    LongDouble,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CvQual {
    pub is_const: bool,
    pub is_volatile: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RefKind {
    Lvalue,
    Rvalue,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FnSig {
    pub params: Vec<TypeId>,
    pub ret: TypeId,
    pub cv: CvQual,
    pub ref_q: Option<RefKind>,
    pub variadic: bool,
    pub noexcept: bool,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClassDef {
    pub name: NestedName,
    pub bases: Vec<BaseSpec>,
    pub fields: Vec<FieldDef>,
    pub methods: Vec<MethodDef>,
    pub kind: RecordKind,
    pub is_polymorphic: bool,
    pub is_final: bool,
    pub source_alignment: Option<u64>,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BaseSpec {
    pub class: ClassId,
    pub virtual_: bool,
    pub access: Access,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FieldDef {
    pub name: Ident,
    pub ty: TypeId,
    pub explicit_align: Option<u64>,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MethodDef {
    pub name: MethodName,
    pub sig: FnSig,
    pub virtuality: Virtuality,
    pub vtable_index: Option<u32>,
    pub special: Option<SpecialMember>,
    /// C++ member access. Access does NOT affect vtable layout —
    /// protected/private virtuals occupy slots and drive final-overrider
    /// resolution exactly like public ones (e.g. FLTK's protected
    /// `Fl_Text_Display::draw()` overriding the pure `Fl_Widget::draw()`).
    /// Emitters use this to suppress callable wrappers/shims, which a
    /// free C trampoline could not legally name.
    #[cfg_attr(feature = "serde", serde(default))]
    pub access: Access,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MethodName {
    Ident(Ident),
    Operator(OperatorKind),
    ConversionTo(TypeId),
}

impl MethodName {
    /// Return the identifier name if this is a plain identifier method;
    /// `None` for operators and conversion functions. Useful for tests
    /// and diagnostics that want to search methods by their source name.
    pub fn ident_name(&self) -> Option<&str> {
        match self {
            MethodName::Ident(i) => Some(&i.0),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RecordKind {
    Class,
    Struct,
    Union,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Virtuality {
    NonVirtual,
    Virtual,
    PureVirtual,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Access {
    #[default]
    Public,
    Protected,
    Private,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SpecialMember {
    DefaultCtor,
    CopyCtor,
    MoveCtor,
    /// A user-declared ctor that isn't default/copy/move (e.g.,
    /// `Foo(int)`). Still counts as "user-declared" for POD purposes
    /// and removes the implicit default ctor, but isn't a C++-defined
    /// special member per se.
    OtherCtor,
    CopyAssign,
    MoveAssign,
    Dtor,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Ident(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NestedName(pub Vec<NameSegment>);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum NameSegment {
    Namespace(Ident),
    Class(Ident),
    /// An enum (scoped or unscoped). Kept distinct from `Class` because
    /// C++ semantically treats enums and classes as different kinds even
    /// though their Itanium mangling is identical (both use the
    /// `<length><name>` source-name form).
    Enum(Ident),
    /// A class template specialization — the underlying template's name
    /// plus concrete template arguments. Itanium mangles this as
    /// `<length><name> I <args...> E`.
    TemplateSpec { name: Ident, args: Vec<TemplateArg> },
    AnonymousNamespace,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TemplateArg {
    /// A type template argument. `Box<int>` has `[Type(int_id)]`.
    Type(TypeId),
    /// A non-type (value) template argument: an integral, boolean,
    /// character, or enumeration constant. `value` is the constant
    /// sign-extended into `i128`; `ty` is its declared C++ type, which
    /// selects the Itanium type letter inside the `L…E` literal wrapper
    /// (`Li4E` for `int 4`, `Lm4E` for `unsigned long 4`, `Lb1E` for
    /// `bool true`, `Lc65E` for `char 'A'`). MSVC ignores `ty` and
    /// encodes every integral argument as `$0<number>`.
    Integral { value: i128, ty: TypeId },
    /// A template-template argument: the name of a class template passed
    /// where a template-template parameter is expected, e.g. the second
    /// argument of `Stack<int, std::vector>`. Itanium mangles it as the
    /// bare name prefix (`3Box`, `St6vector`); MSVC encodes it as a
    /// struct-tag reference (`UBox@@`).
    Template(NestedName),
}

/// Where a `ClassDef` came from. Drives emitter routing (importer-side
/// types are declared by the user's C++ headers and need shims to be
/// callable from Rust; Rust-side `#[repr(cpp)]` types are declared in
/// Rust and need a `.hpp` to be callable from C++) without splitting
/// the underlying IR.
///
/// Layout, mangling, and vtable algorithms are origin-agnostic — they
/// see `ClassDef` and produce results that are correct for either
/// direction. Only the emitters and driver pipeline dispatch on origin.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TypeOrigin {
    /// Imported from a C++ header via `cxx_importer`. Method bodies are
    /// defined in the user's C++ translation units; Rust calls them
    /// through emitted extern-C shims.
    Cxx,
    /// Declared in Rust with `#[repr(cpp)]`. Method bodies are defined
    /// in Rust (the rustc fork emits Itanium-mangled object code);
    /// C++ consumers see the type through the generated `.hpp` and
    /// call through the same shim naming scheme.
    RustReprCpp,
}

impl Default for TypeOrigin {
    fn default() -> Self {
        Self::Cxx
    }
}

/// Rust-origin enum exposed to C++ as a scoped enum (`enum class`).
///
/// Kept as a sidecar (not a `ClassDef`) because C++ and Itanium treat
/// enums as distinct from classes — no inheritance, no methods, no
/// vtable, just a sized integral with a namespaced set of discrete
/// values. Layout flows through the underlying type; mangling uses
/// the name alone. Variants are strings rather than fully-interned
/// identifiers since the emit-only path doesn't require dedup.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RustEnumDef {
    /// Rust-side identifier.
    pub rust_name: String,
    /// C++-visible identifier (may be the same as `rust_name`).
    pub cpp_name: String,
    /// Underlying integer type. For a stable ABI we default to
    /// `int32_t`-equivalent when the source doesn't pin one down.
    pub underlying: TypeId,
    pub variants: Vec<RustEnumVariant>,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RustEnumVariant {
    pub name: String,
    /// Explicit discriminant (`Red = 1`). `None` means "let C++
    /// decide via auto-increment", which matches the Rust default
    /// when no `= expr` is provided.
    pub discriminant: Option<i64>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RustEnumId(pub(crate) u32);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OperatorKind {
    Plus,
    Minus,
    Mul,
    Div,
    Mod,
    Assign,
    PlusAssign,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Call,
    Index,
    Deref,
    PreIncr,
    PreDecr,
}
