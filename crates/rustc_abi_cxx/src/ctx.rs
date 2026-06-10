//! Arena and interning for the type IR.
//!
//! See `docs/rustc_abi_cxx.md §4`.

use std::collections::{HashMap, HashSet};

use crate::target::Target;
use crate::ty::{
    ClassDef, ClassId, CxxType, RustEnumDef, RustEnumId, TypeId, TypeOrigin,
};

/// M18.c: a C++ default-argument value the importer constant-
/// evaluated via libclang (`clang_Cursor_Evaluate`). Only scalar
/// results are representable — that's all the convenience-wrapper
/// emitter can render as a Rust literal anyway. Booleans ride
/// `Int` (libclang evaluates `true` to integer 1); the emitter
/// re-types the value against the parameter's `CxxType`.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DefaultArgValue {
    /// Signed-integer evaluation result (also bools and unscoped
    /// enum values).
    Int(i64),
    /// Unsigned-integer evaluation result (libclang ≥ 4.0 reports
    /// unsigned-typed constants separately).
    UInt(u64),
    /// Floating-point evaluation result.
    Float(f64),
}

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
    /// M18.c: per-method *evaluated* values for the trailing
    /// default parameters counted in `default_arg_counts`. The
    /// `Vec` is aligned with the trailing-default window in
    /// source order (length == the recorded count); `None` slots
    /// are defaults libclang couldn't constant-evaluate (string
    /// literals, enum-class members, ctor calls, …), for which
    /// the emitter falls back to M18.b zero-synthesis. Same
    /// side-table reasoning as `default_arg_counts`.
    default_arg_values: HashMap<(ClassId, usize), Vec<Option<DefaultArgValue>>>,
    /// M21.b: per-field bit width for bitfield members.
    /// `(ClassId, field_idx)` points into `class.fields`. Non-
    /// bitfield fields don't appear in this map. Stored as a
    /// sidecar rather than on `FieldDef` for the same back-
    /// compat reasoning as the other side-tables — every test
    /// fixture that hand-builds a `FieldDef` literal stays
    /// untouched. The `rustc_abi_cxx::layout` engine reads
    /// these widths to apply Itanium bit-packing rules; the
    /// importer populates them via
    /// `Entity::get_bit_field_width()` on `FieldDecl` cursors.
    bitfield_widths: HashMap<(ClassId, usize), u64>,
    /// Per-class `#pragma pack(N)` override. When set, the layout
    /// engine clamps every alignment requirement (field, base, vptr,
    /// tail padding) to the smaller of (natural alignment, N).
    /// Itanium's layout engine ignores this (matches gcc/clang
    /// behavior on Unix targets — `#pragma pack` is a Microsoft
    /// extension); the MSVC layout engine honors it.
    pragma_pack: HashMap<ClassId, u64>,
    /// v1.13.1: per-class `__attribute__((packed))` flag. When set,
    /// the **Itanium** layout engine forces every field's alignment
    /// to 1 (removing inter-field padding) and does not bump the
    /// record's alignment from its fields — the record's alignment
    /// becomes 1 unless a larger `alignas` is also present. This is
    /// the GCC/Clang `packed` attribute, distinct from MSVC's
    /// `#pragma pack(N)` (which clamps to N rather than forcing 1).
    /// A per-field `alignas` still wins over packing for that field.
    packed: HashSet<ClassId>,
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
            default_arg_values: HashMap::new(),
            bitfield_widths: HashMap::new(),
            pragma_pack: HashMap::new(),
            packed: HashSet::new(),
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

    /// M18.c: record the libclang-evaluated values for the trailing
    /// default parameters of `class.methods[method_idx]`. `values`
    /// is aligned with the trailing-default window in source order
    /// (same window `record_default_arg_count` counts); `None`
    /// slots are defaults that didn't constant-evaluate. All-`None`
    /// vectors are dropped — they carry no more information than
    /// the count alone.
    pub fn record_default_arg_values(
        &mut self,
        class: ClassId,
        method_idx: usize,
        values: Vec<Option<DefaultArgValue>>,
    ) {
        if values.iter().any(Option::is_some) {
            self.default_arg_values.insert((class, method_idx), values);
        }
    }

    /// M18.c: the evaluated value of the `trailing_idx`-th
    /// parameter *within the trailing-default window* of
    /// `class.methods[method_idx]` (0 = first defaulted param).
    /// `None` when the importer recorded no value — the emitter
    /// then falls back to M18.b zero-synthesis.
    pub fn default_arg_value(
        &self,
        class: ClassId,
        method_idx: usize,
        trailing_idx: usize,
    ) -> Option<DefaultArgValue> {
        self.default_arg_values
            .get(&(class, method_idx))
            .and_then(|v| v.get(trailing_idx).copied().flatten())
    }

    /// M21.b: record that `class.fields[field_idx]` is a bitfield
    /// declared with `width` bits in the C++ source. Width 0 is
    /// permitted — it's a special "force alignment to next AU"
    /// marker per Itanium.
    pub fn record_bitfield_width(
        &mut self,
        class: ClassId,
        field_idx: usize,
        width: u64,
    ) {
        self.bitfield_widths.insert((class, field_idx), width);
    }

    /// Bitfield width for `class.fields[field_idx]`. `Some(w)`
    /// means the field is a bitfield (including `w == 0`); `None`
    /// means it's a regular field.
    pub fn bitfield_width(&self, class: ClassId, field_idx: usize) -> Option<u64> {
        self.bitfield_widths.get(&(class, field_idx)).copied()
    }

    /// True iff `class.fields[field_idx]` is a bitfield. Slim
    /// convenience wrapper over `bitfield_width`.
    pub fn is_bitfield(&self, class: ClassId, field_idx: usize) -> bool {
        self.bitfield_widths.contains_key(&(class, field_idx))
    }

    /// True iff *any* field on `class` is a bitfield. Used by the
    /// layout engine to decide whether to switch the per-field
    /// placement loop into the bit-packed walker.
    pub fn class_has_bitfields(&self, class: ClassId) -> bool {
        self.bitfield_widths
            .keys()
            .any(|(c, _)| *c == class)
    }

    /// Set a `#pragma pack(N)` override for `class`. Honored by the
    /// MSVC layout engine; ignored by Itanium. Common values:
    /// 1 (byte-packed), 2, 4, 8 (MSVC's default).
    pub fn set_pragma_pack(&mut self, class: ClassId, pack: u64) {
        self.pragma_pack.insert(class, pack);
    }

    /// Read the `#pragma pack(N)` override for `class`, if any.
    /// `None` means no override; the layout engine uses its target's
    /// default packing (8 on MSVC for 64-bit targets).
    pub fn pragma_pack(&self, class: ClassId) -> Option<u64> {
        self.pragma_pack.get(&class).copied()
    }

    /// v1.13.1: mark `class` as `__attribute__((packed))`. Honored
    /// by the Itanium layout engine (forces field alignment to 1,
    /// removes inter-field padding, and caps the record's
    /// field-derived alignment at 1). The importer sets this from
    /// the `packed` attribute on a `StructDecl`/`ClassDecl` cursor.
    pub fn set_packed(&mut self, class: ClassId) {
        self.packed.insert(class);
    }

    /// True iff `class` carries `__attribute__((packed))`.
    pub fn is_packed(&self, class: ClassId) -> bool {
        self.packed.contains(&class)
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
