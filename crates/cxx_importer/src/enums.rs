//! Imported-enum body side-table (M16).
//!
//! `rustc_abi_cxx::CxxType::Enum` records enough about a C++ enum
//! type to mangle and lay it out (name + underlying integer type
//! + scoped bit), but it doesn't carry the variant list. That's
//! intentional — layout / mangling don't need variants — but the
//! Rust binding emitter does, so we keep them here in a parallel
//! side-table sourced from the importer.
//!
//! ## Two emission shapes
//!
//! Real-world C++ code occasionally uses **aliasing variants**
//! (`Red = 1, Crimson = 1`) and **unscoped enums** that flow into
//! integer arithmetic (`flags |= FLAG_X`). Rust's `enum` requires
//! each variant to have a unique discriminant. To stay safe in
//! both shapes, the emitter picks per-enum:
//!
//! - **Scoped, all unique** → `#[repr(<int>)] pub enum Foo { Var = N, ... }`.
//!   The idiomatic Rust shape; supports `match`, derives, etc.
//!
//! - **Unscoped** *or* **aliasing variants** → `#[repr(transparent)]
//!   pub struct Foo(pub <int>); impl Foo { pub const VAR: Self =
//!   Self(N); ... }`. Layout-identical, but tolerates duplicate
//!   discriminants and lets users do bitwise math the way the
//!   C++ side intends.
//!
//! Class-scope enums (`struct C { enum E { ... }; }`) follow the
//! same path as type aliases: deferred until the emitter grows
//! associated-item support.

use rustc_abi_cxx::{Ident, NameSegment, TypeId};

/// One `enum` (scoped or unscoped) captured during import.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "cache", derive(serde::Serialize, serde::Deserialize))]
pub struct CxxEnumDef {
    /// Enclosing namespace path (empty for TU-scope enums).
    pub parent: Vec<NameSegment>,
    /// The enum identifier (`Color` in `enum class Color { ... }`).
    pub name: Ident,
    /// `TypeId` for the underlying integer (defaults to `int` for
    /// unscoped enums where the source omits the underlying type;
    /// matches Itanium-mangled layout).
    pub underlying: TypeId,
    /// `true` for `enum class` / `enum struct`, `false` for plain
    /// unscoped enums. Drives shape selection in the emitter.
    pub scoped: bool,
    /// Declaration-order list of variants. Discriminants are taken
    /// directly from libclang's `get_enum_constant_value`, which
    /// already evaluated any constant-folding (e.g. `Red = 1 << 2`).
    pub variants: Vec<CxxEnumVariant>,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "cache", derive(serde::Serialize, serde::Deserialize))]
pub struct CxxEnumVariant {
    pub name: String,
    /// Stored as i64 with a separate signed-ness flag so the
    /// emitter can pick the right repr for unsigned underlying
    /// types without losing high-bit values.
    pub value: i64,
}

/// Collection of imported enums in source-declaration order.
#[derive(Default, Clone, Debug)]
#[cfg_attr(feature = "cache", derive(serde::Serialize, serde::Deserialize))]
pub struct EnumSet {
    pub entries: Vec<CxxEnumDef>,
}

impl EnumSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, def: CxxEnumDef) {
        self.entries.push(def);
    }

    pub fn iter(&self) -> impl Iterator<Item = &CxxEnumDef> {
        self.entries.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
