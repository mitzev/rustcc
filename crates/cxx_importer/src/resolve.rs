//! Resolve a qualified C++ name to a Clang cursor.
//!
//! See `docs/cxx_importer.md §4`.

use rustc_abi_cxx::NestedName;

use crate::driver::Driver;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct EntityKey(pub NestedName);

/// Opaque handle around a Clang cursor.
pub struct ResolvedCursor {
    _private: (),
}

impl Driver {
    pub fn resolve(&self, _key: &EntityKey) -> Option<ResolvedCursor> {
        todo!("walk the CXCursor tree to find the entity")
    }
}
