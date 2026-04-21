//! C++ → Rust identifier transformations.
//!
//! See `docs/cxx_importer.md §6, §7`.

use rustc_abi_cxx::Ident;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SelfReceiver {
    Shared,
    Mut,
    Owned,
    Static,
}

pub fn map_namespace(name: &Ident) -> Ident {
    name.clone()
}

pub fn map_class(name: &Ident) -> Ident {
    name.clone()
}

pub fn map_method(name: &Ident, is_const: bool) -> (Ident, SelfReceiver) {
    let receiver = if is_const {
        SelfReceiver::Shared
    } else {
        SelfReceiver::Mut
    };
    (name.clone(), receiver)
}

pub fn disambiguate_overloads(_names: &[Ident]) -> Vec<Ident> {
    todo!("overload disambiguation algorithm — see docs §7")
}
