//! Free-function side-table populated by the importer (M11.b).
//!
//! C++ has three "non-method-on-class" callable shapes the
//! importer needs to surface to Rust:
//!
//! 1. **Static methods on classes** — already handled (M11.a) via
//!    `CxxTypeCtx::mark_method_static` and the `EmissionKind::Static`
//!    binding-emitter path.
//! 2. **Free functions at TU/namespace scope** — *this module*.
//!    Examples: `void fl_message(const char*)`,
//!    `Fl_Color fl_color(int)`. The bindings emitter renders
//!    these as top-level `pub fn`s with a `#[link_name = "..."]`
//!    extern decl carrying the Itanium-mangled symbol.
//! 3. **Static data members + namespace-scope variables** — M11.c,
//!    a parallel module to this one.
//!
//! Free functions never appear in `CxxTypeCtx::classes` because
//! they aren't classes; storing them on a sidecar mirrors how
//! aliases (M17) and enums (M16) are handled and avoids changes
//! to the IR proper.
//!
//! Class-scope `friend` functions and operator overloads at
//! namespace scope (`operator+(const A&, const A&)`) are
//! deliberately deferred — they're rare in widget-style libraries
//! like FLTK and would need extra naming machinery on the Rust
//! side. The walker's filter currently picks up plain
//! identifier-named non-method functions only.

use rustc_abi_cxx::{FnSig, Ident, NameSegment};

/// One captured C++ free-function declaration at TU or namespace
/// scope. The Itanium symbol is reconstructed at emission time
/// from `(parent + name + sig)` via `Symbol::Function`.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "cache", derive(serde::Serialize, serde::Deserialize))]
pub struct FreeFnDef {
    /// Enclosing namespace path (empty for TU-scope functions).
    /// Only `Namespace` / `AnonymousNamespace` segments end up
    /// here in v0 — class-scope friend functions are skipped.
    pub parent: Vec<NameSegment>,
    /// The function identifier (`fl_message` in
    /// `void fl_message(const char* s);`).
    pub name: Ident,
    /// Full signature — params, return, variadic, noexcept.
    pub sig: FnSig,
    /// Header-inline (in-class body or `inline` keyword): no
    /// out-of-line symbol exists, so the bindings route the call
    /// through a `__rustcc_shim_<mangled>` trampoline whose C++ TU
    /// instantiates the inline definition. (FLTK's whole fl_draw
    /// surface — fl_rectf, fl_polygon, fl_arc … — is shaped
    /// exactly like this.)
    #[cfg_attr(feature = "cache", serde(default))]
    pub is_inline: bool,
}

impl FreeFnDef {
    /// Slice view of `parent`, named for parity with how the
    /// emitter constructs `NestedName` for `Symbol::Function`
    /// mangling. Avoids cloning when we just need a borrow.
    pub fn def_scope(&self) -> &[NameSegment] {
        &self.parent
    }
}

/// Collection of free functions in source-declaration order.
#[derive(Default, Clone, Debug)]
#[cfg_attr(feature = "cache", derive(serde::Serialize, serde::Deserialize))]
pub struct FreeFnSet {
    pub entries: Vec<FreeFnDef>,
}

impl FreeFnSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, def: FreeFnDef) {
        self.entries.push(def);
    }

    pub fn iter(&self) -> impl Iterator<Item = &FreeFnDef> {
        self.entries.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
