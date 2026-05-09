//! Class-scope static data members (M11.c).
//!
//! C++ classes can carry static data — `class Fl { static const
//! char* scheme_; }`. These are linked symbols just like
//! functions but exposed at module scope rather than dispatched
//! through `this`. The Itanium symbol shape is
//! `_ZN<class-scope><name>E` (e.g. `_ZN2Fl7scheme_E`),
//! constructed by the existing `Symbol::Variable` mangler.
//!
//! ## Scope of v0
//!
//! - Captures static data members declared inside a class /
//!   struct / union body.
//! - Lowers the field type via the standard `import_type` path
//!   so primitive scalars and pointer-typed members work
//!   immediately. Templated types and member-pointer types
//!   silently skip (same policy as M17 aliases / M11.b free
//!   fns).
//! - Class-scope only. Namespace-scope `extern` variables are
//!   tracked for a follow-up (rare in widget-style libraries
//!   like FLTK; needed for some standard libraries).
//!
//! ## Emission shape
//!
//! Each captured entry generates two pieces inside the owning
//! class's emission block:
//!
//! 1. An `unsafe extern "C++" { static [mut] __cxx_<class>_<name>: T; }`
//!    decl with a `#[link_name]` carrying the Itanium-mangled
//!    symbol.
//! 2. An accessor inside `impl <Class> { pub fn <name>_ptr()
//!    -> *[const|mut] T { unsafe { core::ptr::addr_of[_mut]!(...) } } }`.
//!
//! Returning a raw pointer (rather than `&'static T` / `&mut T`)
//! is intentional: extern statics on the C++ side may be written
//! from any thread without Rust's aliasing rules in scope, so a
//! safe-Rust reference can't be soundly produced. Users coerce
//! to `&` only inside their own `unsafe` block where they own
//! the synchronization argument.

use rustc_abi_cxx::{CvQual, Ident, NameSegment, TypeId};

/// One captured static data member at class scope.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "cache", derive(serde::Serialize, serde::Deserialize))]
pub struct StaticDataDef {
    /// Enclosing path. The trailing segment is the owning
    /// class — emission groups members by that segment so each
    /// class's `impl` block carries its own accessors.
    pub parent: Vec<NameSegment>,
    /// The data-member identifier (`scheme_` in
    /// `static const char* scheme_;`).
    pub name: Ident,
    /// Member type (already canonicalized via `import_type`).
    pub ty: TypeId,
    /// Top-level `cv` of the member type. `is_const = true`
    /// drives the renderer to emit `static <NAME>:` without
    /// `mut`, matching the C++ semantics.
    pub cv: CvQual,
}

/// Collection of static data members in source-declaration order.
#[derive(Default, Clone, Debug)]
#[cfg_attr(feature = "cache", derive(serde::Serialize, serde::Deserialize))]
pub struct StaticDataSet {
    pub entries: Vec<StaticDataDef>,
}

impl StaticDataSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, def: StaticDataDef) {
        self.entries.push(def);
    }

    pub fn iter(&self) -> impl Iterator<Item = &StaticDataDef> {
        self.entries.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
