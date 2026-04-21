//! `rustc_abi_cxx` — Itanium C++ ABI: record layout, mangling, vtables.
//!
//! Foundation crate for rustcc's C++ interop. All layout and symbol-naming
//! decisions route through this crate so they stay consistent between the
//! header importer and the `#[repr(cpp)]` codegen direction.
//!
//! See `docs/rustc_abi_cxx.md`.

#![deny(unsafe_op_in_unsafe_fn)]
#![allow(dead_code)]

mod ctx;
mod diag;
mod layout;
mod mangle;
mod target;
mod ty;
mod vtable;

pub use ctx::CxxTypeCtx;
pub use diag::LayoutError;
pub use layout::RecordLayout;
pub use mangle::{CtorVariant, DtorVariant, Symbol};
pub use target::{LongDoubleKind, Target};
pub use ty::{
    Access, BaseSpec, ClassDef, ClassId, CvQual, CxxType, FieldDef, FieldId,
    FloatKind, FnSig, Ident, IntWidth, MethodDef, MethodId, MethodName,
    NameSegment, NestedName, OperatorKind, RecordKind, RefKind, RustEnumDef,
    RustEnumId, RustEnumVariant, SpecialMember, TemplateArg, TypeId,
    TypeOrigin, Virtuality,
};
pub use vtable::{VTable, VTableEntry, VTableSubTable};
