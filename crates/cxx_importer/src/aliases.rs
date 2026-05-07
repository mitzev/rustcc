//! Type-alias side-table populated by the importer (M17).
//!
//! C++ `typedef T U;` and `using U = T;` are pure ergonomics:
//! they don't change layout, mangling, or vtable. libclang
//! already strips typedef sugar back to canonical types via
//! [`Type::get_canonical_type`] inside [`crate::import::Importer::import_type`],
//! so anywhere an alias is *used* (a field, a parameter, a
//! return type) the importer sees the underlying type and the
//! existing layout/codegen path takes over unchanged.
//!
//! What we still need is to surface alias *names*, so a
//! generated Rust binding can offer `pub type FooAlias =
//! Bar;` matching the C++ source. That's what this module
//! captures: a simple list of `(parent path, leaf ident,
//! target TypeId)` triples that the bindings emitter walks
//! after the namespace tree of classes.
//!
//! Aliases nested inside class bodies (`struct Foo { using
//! It = int; };`) are deliberately deferred — they map to
//! associated `type` items on a Rust impl block which
//! requires more emitter plumbing. The first cut handles the
//! TU-/namespace-scope case which covers the common
//! "header-level convenience names" idiom.

use rustc_abi_cxx::{Ident, NameSegment, TypeId};

/// One captured C++ alias declaration.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "cache", derive(serde::Serialize, serde::Deserialize))]
pub struct TypeAlias {
    /// Enclosing namespace path (empty for TU-scope aliases).
    /// Only `Namespace` / `AnonymousNamespace` segments end up
    /// here in v0 — class-scope aliases are skipped.
    pub parent: Vec<NameSegment>,
    /// The alias identifier itself (`FooAlias` in `using
    /// FooAlias = Foo;`).
    pub name: Ident,
    /// `TypeId` for the underlying type, interned in the same
    /// `CxxTypeCtx` the alias was harvested with.
    pub target: TypeId,
}

/// Collection of aliases captured during a single import. Stored
/// in source-declaration order so emission is deterministic.
#[derive(Default, Clone, Debug)]
#[cfg_attr(feature = "cache", derive(serde::Serialize, serde::Deserialize))]
pub struct AliasSet {
    pub entries: Vec<TypeAlias>,
}

impl AliasSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, alias: TypeAlias) {
        self.entries.push(alias);
    }

    pub fn iter(&self) -> impl Iterator<Item = &TypeAlias> {
        self.entries.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
