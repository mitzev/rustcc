//! Arena and interning for the type IR.
//!
//! See `docs/rustc_abi_cxx.md §4`.

use std::collections::{HashMap, HashSet};

use crate::target::Target;
use crate::ty::{
    ClassDef, ClassId, CxxType, RustEnumDef, RustEnumId, TypeId, TypeOrigin,
};

#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CxxTypeCtx {
    target: Target,
    classes: Vec<ClassDef>,
    /// Parallel to `classes`: origin of each class. Kept as a sidecar
    /// rather than a field on `ClassDef` so the large number of
    /// existing `ClassDef` literals (tests + importer) continue to work
    /// unchanged; origins flow through the `define_*` API on this
    /// context.
    class_origin: Vec<TypeOrigin>,
    /// Side-table marking poisoned classes — entries minted from a
    /// failed-but-recoverable lowering. The map's value is a human-
    /// readable explanation (e.g. "virtual inheritance not
    /// supported"). Same rationale as `class_origin`: keeping it off
    /// `ClassDef` avoids touching the 70+ existing struct-literal
    /// sites in tests and the importer.
    poison_reason: HashMap<ClassId, String>,
    /// Side-table marking class methods as static (no receiver).
    /// `(ClassId, method_idx)` keys point into `class.methods`. Off
    /// `MethodDef` for the same reason as `poison_reason`: the
    /// existing struct-literal sites stay untouched while M11
    /// can still distinguish `static Fl::run()` from instance
    /// methods.
    static_methods: HashSet<(ClassId, usize)>,
    /// M18: per-method count of trailing parameters that have C++
    /// default values. The bindings emitter renders this as an
    /// informational doc comment so users know which arguments
    /// the C++ side considered optional. Stored as a side-table
    /// for the same back-compat reasoning as `static_methods`.
    default_arg_counts: HashMap<(ClassId, usize), usize>,
    /// Rust-origin enums exposed to C++ as scoped enums. No parallel
    /// for C++-origin enums yet — imported enums flow as anonymous
    /// `CxxType::Enum` instances with the variants living in the
    /// user's headers.
    rust_enums: Vec<RustEnumDef>,
    types: Vec<CxxType>,
}

impl CxxTypeCtx {
    pub fn new(target: Target) -> Self {
        Self {
            target,
            classes: Vec::new(),
            class_origin: Vec::new(),
            poison_reason: HashMap::new(),
            static_methods: HashSet::new(),
            default_arg_counts: HashMap::new(),
            rust_enums: Vec::new(),
            types: Vec::new(),
        }
    }

    /// Mark `id` as a poison node and record `reason` for later
    /// diagnostic rendering. Idempotent — overwriting a previously-
    /// recorded reason is intentional (the most recent failure
    /// wins; the importer typically only marks each class once).
    pub fn poison(&mut self, id: ClassId, reason: impl Into<String>) {
        self.poison_reason.insert(id, reason.into());
    }

    /// Return the poison reason recorded on `id`, or `None` if the
    /// class is healthy. Used by [`Self::is_poisoned`] and by
    /// downstream emitters that include the reason in generated
    /// doc comments.
    pub fn poison_reason(&self, id: ClassId) -> Option<&str> {
        self.poison_reason.get(&id).map(String::as_str)
    }

    /// True when the importer registered `id` via [`Self::poison`].
    /// Poisoned classes have empty `methods` / `fields` and should
    /// be rendered opaquely by emitters.
    pub fn is_poisoned(&self, id: ClassId) -> bool {
        self.poison_reason.contains_key(&id)
    }

    /// Clear the poison marker on `id`. Used by the importer's M13
    /// upgrade path: when a class previously poisoned for being
    /// forward-only is later seen with a full definition (in the
    /// same TU or another included header), the placeholder gets
    /// replaced in place via `class_mut`, and this call promotes
    /// it back to a healthy entry.
    pub fn unpoison(&mut self, id: ClassId) {
        self.poison_reason.remove(&id);
    }

    /// Mark a class method as static. M11 — without this side
    /// channel, `MethodDef` has no way to express
    /// `static int Fl::run()`-style methods. The bindings emitter
    /// reads this flag and routes static methods through the
    /// receiver-less wrapper path. Same off-`MethodDef`-for-back-
    /// compat reasoning as `poison_reason`.
    ///
    /// `method_idx` is the position in `class.methods`.
    pub fn mark_method_static(&mut self, class: ClassId, method_idx: usize) {
        self.static_methods.insert((class, method_idx));
    }

    /// True when the importer (or a hand-built test) flagged
    /// `class.methods[method_idx]` as a static method via
    /// [`Self::mark_method_static`].
    pub fn is_method_static(&self, class: ClassId, method_idx: usize) -> bool {
        self.static_methods.contains(&(class, method_idx))
    }

    /// M18: record that `class.methods[method_idx]` has `count`
    /// trailing parameters with C++ default values. The bindings
    /// emitter renders this as a doc comment so users know which
    /// arguments are nominally optional in the source language.
    pub fn record_default_arg_count(
        &mut self,
        class: ClassId,
        method_idx: usize,
        count: usize,
    ) {
        if count > 0 {
            self.default_arg_counts.insert((class, method_idx), count);
        }
    }

    /// Number of trailing parameters with C++ default values for
    /// `class.methods[method_idx]`. Returns 0 when none recorded.
    pub fn default_arg_count(&self, class: ClassId, method_idx: usize) -> usize {
        self.default_arg_counts
            .get(&(class, method_idx))
            .copied()
            .unwrap_or(0)
    }

    /// Register a Rust-origin enum for C++ exposure. Returns the
    /// stable id used by emitters when rendering the `enum class`
    /// declaration.
    pub fn define_rust_enum(&mut self, def: RustEnumDef) -> RustEnumId {
        let id = RustEnumId(self.rust_enums.len() as u32);
        self.rust_enums.push(def);
        id
    }

    pub fn rust_enum(&self, id: RustEnumId) -> &RustEnumDef {
        &self.rust_enums[id.0 as usize]
    }

    /// Iterate every registered Rust-origin enum in definition order.
    pub fn rust_enum_ids(&self) -> impl Iterator<Item = RustEnumId> + '_ {
        (0..self.rust_enums.len() as u32).map(RustEnumId)
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// Define a class with the default origin (`TypeOrigin::Cxx`).
    ///
    /// Back-compatible shim: preserves the pre-`TypeOrigin` API so the
    /// importer and existing tests keep working without churn. New code
    /// that wants a specific origin should use `define_class_with_origin`
    /// or `define_rust_class`.
    pub fn define_class(&mut self, def: ClassDef) -> ClassId {
        self.define_class_with_origin(def, TypeOrigin::Cxx)
    }

    /// Define a class and record its origin. `TypeOrigin::RustReprCpp`
    /// marks a class declared in Rust; `TypeOrigin::Cxx` marks one
    /// imported from a C++ header.
    pub fn define_class_with_origin(
        &mut self,
        def: ClassDef,
        origin: TypeOrigin,
    ) -> ClassId {
        let id = ClassId(self.classes.len() as u32);
        self.classes.push(def);
        self.class_origin.push(origin);
        id
    }

    /// Convenience: define a class with `TypeOrigin::RustReprCpp`.
    pub fn define_rust_class(&mut self, def: ClassDef) -> ClassId {
        self.define_class_with_origin(def, TypeOrigin::RustReprCpp)
    }

    pub fn class(&self, id: ClassId) -> &ClassDef {
        &self.classes[id.0 as usize]
    }

    /// Mutable access to a previously-defined class. Needed by importers
    /// that use a two-phase "register placeholder, fill in body"
    /// strategy so self-referential types (e.g., `struct Bar { Bar* p; }`
    /// or member functions that take `const Bar&`) don't recurse into
    /// themselves.
    pub fn class_mut(&mut self, id: ClassId) -> &mut ClassDef {
        &mut self.classes[id.0 as usize]
    }

    /// Origin of the given class.
    pub fn class_origin(&self, id: ClassId) -> TypeOrigin {
        self.class_origin[id.0 as usize]
    }

    /// Iterate every class id in definition order.
    pub fn class_ids(&self) -> impl Iterator<Item = ClassId> + '_ {
        (0..self.classes.len() as u32).map(ClassId)
    }

    /// Iterate class ids whose origin is `TypeOrigin::RustReprCpp`.
    /// Used by `emit_hpp` to decide which classes need C++ declarations
    /// generated and by the rustc fork to drive layout/mangling for
    /// Rust-declared `#[repr(cpp)]` types.
    pub fn rust_classes(&self) -> impl Iterator<Item = ClassId> + '_ {
        self.class_origin
            .iter()
            .enumerate()
            .filter_map(|(i, o)| {
                matches!(o, TypeOrigin::RustReprCpp)
                    .then_some(ClassId(i as u32))
            })
    }

    /// Iterate class ids whose origin is `TypeOrigin::Cxx` (imported).
    pub fn cxx_classes(&self) -> impl Iterator<Item = ClassId> + '_ {
        self.class_origin
            .iter()
            .enumerate()
            .filter_map(|(i, o)| {
                matches!(o, TypeOrigin::Cxx).then_some(ClassId(i as u32))
            })
    }

    pub fn intern_type(&mut self, ty: CxxType) -> TypeId {
        // Skeleton uses linear scan; a real implementation swaps in a hasher.
        if let Some(idx) = self.types.iter().position(|t| t == &ty) {
            return TypeId(idx as u32);
        }
        let id = TypeId(self.types.len() as u32);
        self.types.push(ty);
        id
    }

    pub fn type_of(&self, id: TypeId) -> &CxxType {
        &self.types[id.0 as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::{Ident, NameSegment, NestedName, RecordKind};

    fn class(name: &str) -> ClassDef {
        ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident(name.into()))]),
            bases: vec![],
            fields: vec![],
            methods: vec![],
            kind: RecordKind::Class,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        }
    }

    #[test]
    fn define_class_defaults_to_cxx_origin() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let id = ctx.define_class(class("Foo"));
        assert_eq!(ctx.class_origin(id), TypeOrigin::Cxx);
    }

    #[test]
    fn define_rust_class_marks_origin() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let id = ctx.define_rust_class(class("Foo"));
        assert_eq!(ctx.class_origin(id), TypeOrigin::RustReprCpp);
    }

    #[test]
    fn origin_iterators_partition_classes() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let a = ctx.define_class(class("A"));
        let b = ctx.define_rust_class(class("B"));
        let c = ctx.define_class(class("C"));
        let d = ctx.define_rust_class(class("D"));

        let rust: Vec<ClassId> = ctx.rust_classes().collect();
        let cxx: Vec<ClassId> = ctx.cxx_classes().collect();
        assert_eq!(rust, vec![b, d]);
        assert_eq!(cxx, vec![a, c]);
        // Union covers everything exactly once, in definition order.
        let all: Vec<ClassId> = ctx.class_ids().collect();
        assert_eq!(all, vec![a, b, c, d]);
    }
}
