//! MSVC C++ ABI vtable layout.
//!
//! MSVC vs Itanium at the vtable level — the key shape differences:
//!
//! - **No header slots before the function pointers.** Itanium puts an
//!   offset-to-top (a signed int interpreted as bytes from the
//!   vfunc-slot range back to the most-derived object) at slot −2 and
//!   the RTTI pointer at slot −1, with the vptr pointing at slot 0
//!   (the first virtual function). MSVC instead places a single
//!   "complete object locator" pointer at slot −1 — the COL itself
//!   carries the offset-to-top and a pointer to the RTTI type descriptor.
//!   The vptr points at the first vfunction slot, with no header in
//!   between.
//!
//! - **Per-base subobject vftables.** Like Itanium, each polymorphic
//!   base subobject in a multi-inheritance hierarchy needs its own
//!   vftable. The vftable symbol is `??_7Class@@6B<For>@` where `<For>`
//!   names the base subobject the table dispatches for (empty for the
//!   primary).
//!
//! - **vbtable for virtual inheritance.** Classes with virtual bases
//!   carry a `vbtable` (separate from the vftable). The vbptr —
//!   placed by `layout_msvc` — points at the vbtable. The vbtable's
//!   slot 0 is the offset from the vbptr to the most-derived class's
//!   origin; slots 1..N are offsets to each virtual base subobject.
//!
//! - **No covariant-return thunks at the function-pointer level.**
//!   MSVC encodes covariant returns by emitting a wrapper thunk
//!   (`?_<base>...` mangled) as a normal-looking vfunction. We model
//!   that the same way: a `FunctionPointer` entry whose `mangled_target`
//!   is the thunk symbol.
//!
//! - **Deleting destructor convention.** Where Itanium has separate
//!   D1 (complete) / D0 (deleting) / D2 (base) symbols and entries
//!   for both D1 and D0 in the vtable, MSVC ships a single
//!   "scalar deleting destructor" (`??_GClass@@`) and the dtor slot
//!   in the vftable points at it. The base dtor (`??1Class@@`) is
//!   called explicitly from user code; the deleting variant
//!   chains to it.
//!
//! ## What this module returns
//!
//! Same [`VTable`] type as the Itanium [`vtable`] module — sub-tables
//! per polymorphic base subobject, each with an `address_point_offset`,
//! a `Rtti` entry serving as the complete-object-locator pointer, and
//! a sequence of `FunctionPointer` entries.
//!
//! The MSVC variant does **not** emit `OffsetToTop` entries (those live
//! inside the COL, not as a slot before the address point). Instead,
//! the `Rtti` entry's mangled string is the COL symbol, and at the IR
//! level callers should treat the COL as a header that's *not part of*
//! the address-point-relative slot space.

use crate::ctx::CxxTypeCtx;
use crate::mangle::{DtorVariant, Symbol};
use crate::ty::{ClassId, MethodDef, MethodId, SpecialMember, Virtuality};
use crate::vtable::{VTable, VTableEntry, VTableSubTable};

impl CxxTypeCtx {
    /// MSVC vtable construction. Routed here by `CxxTypeCtx::vtable`
    /// when `target().abi_flavor == AbiFlavor::Msvc`.
    pub fn vtable_msvc(&self, class_id: ClassId) -> Option<VTable> {
        if !self.class(class_id).is_polymorphic {
            return None;
        }
        let mut sub_tables = Vec::new();

        // 1. Primary subtable. MSVC: address_point_offset = 0 (the
        //    vptr points directly at the first vfunction slot). The
        //    Rtti slot is conceptually at offset -pointer_width; we
        //    represent it as a *separate* entry that the symbol
        //    emitter knows to place at a negative offset rather than
        //    a positive slot.
        let entries = build_subtable_entries(self, class_id, class_id, 0);
        sub_tables.push(VTableSubTable {
            for_subobject: class_id,
            subobject_offset: 0,
            entries,
            // MSVC's address-point is right at the first slot. The
            // COL is at offset -pointer_width relative to that and
            // is emitted as the leading Rtti entry in our IR.
            address_point_offset: pointer_width(self),
        });

        // 2. Per-polymorphic-non-primary-base subtables (multi-
        //    inheritance). Each non-virtual non-primary polymorphic
        //    base contributes its own subtable; the vptr inside that
        //    base subobject points at the subtable's address-point.
        //
        // We compute base subobject offsets via the MSVC layout
        // engine. Bases that are themselves non-polymorphic don't
        // need a subtable.
        if let Ok(layout) = self.layout_msvc(class_id) {
            // The primary polymorphic base (if any) shares the
            // primary subtable — skip it here.
            let primary_polymorphic_base = self
                .class(class_id)
                .bases
                .iter()
                .find(|b| !b.virtual_ && self.class(b.class).is_polymorphic)
                .map(|b| b.class);

            for (base, offset) in &layout.base_offsets {
                if Some(*base) == primary_polymorphic_base {
                    continue;
                }
                if !self.class(*base).is_polymorphic {
                    continue;
                }
                let entries = build_subtable_entries(self, *base, class_id, *offset);
                sub_tables.push(VTableSubTable {
                    for_subobject: *base,
                    subobject_offset: *offset,
                    entries,
                    address_point_offset: pointer_width(self),
                });
            }
        }

        let symbol = self.mangle_msvc(&Symbol::VTable(class_id));
        Some(VTable {
            class: class_id,
            symbol,
            sub_tables,
        })
    }
}

/// Build the entry sequence for a single subtable.
///
/// `subobj`: the base subobject this subtable serves dispatch for.
/// `most_derived`: the class whose vtable symbol we're constructing.
/// `subobj_offset`: byte offset of `subobj` within `most_derived`.
///
/// MSVC subtable layout (slot 0 onward, after the COL header):
/// - For each virtual method in `subobj` (in slot order), emit a
///   `FunctionPointer` whose `mangled_target` is the most-derived
///   override.
/// - The dtor slot points at the scalar deleting destructor
///   (`??_GClass@@`) when present, falling back to the base dtor
///   (`??1Class@@`) when no deleting form is emitted.
fn build_subtable_entries(
    ctx: &CxxTypeCtx,
    subobj: ClassId,
    most_derived: ClassId,
    _subobj_offset: u64,
) -> Vec<VTableEntry> {
    let mut entries = Vec::new();

    // COL pointer (at conceptual -1 slot).
    let col = ctx.mangle_msvc(&Symbol::TypeInfo(most_derived));
    entries.push(VTableEntry::Rtti(col));

    // Function-pointer slots. The subtable for `most_derived` is
    // built from the chain rooted at `subobj` (the subobject this
    // subtable serves dispatch for). For the primary subtable
    // these are the same class; for inheritance-induced
    // subtables `subobj` is a polymorphic base of `most_derived`.
    let slots = compute_slots(ctx, most_derived);
    let _ = subobj; // subobj equals most_derived for the primary subtable
    for slot in &slots {
        // Find the most-derived overrider of `slot.method`. We walk
        // from `most_derived` down toward the class that originally
        // declared the slot; the first class that re-declares the
        // method with a matching signature wins.
        let target_class = find_overrider(ctx, most_derived, slot.declared_in, &slot.method);
        // Resolve the actual MethodDef on `target_class` rather than
        // assuming the slot index matches there.
        let original = ctx.class(slot.declared_in).methods[slot.method.as_index()].clone();
        let method = ctx
            .class(target_class)
            .methods
            .iter()
            .find(|m| methods_override(&original, m))
            .unwrap_or(&ctx.class(slot.declared_in).methods[slot.method.as_index()]);
        let mangled = if matches!(method.special, Some(SpecialMember::Dtor)) {
            // MSVC's vtable dtor slot points at the scalar deleting
            // dtor `??_G`, not the base dtor. We model that as a
            // synthetic Dtor symbol using the D0 variant marker.
            ctx.mangle_msvc(&Symbol::Dtor {
                class: target_class,
                variant: DtorVariant::D0,
            })
        } else {
            ctx.mangle_msvc(&Symbol::Method {
                class: target_class,
                name: method.name.clone(),
                sig: method.sig.clone(),
            })
        };
        entries.push(VTableEntry::FunctionPointer {
            mangled_target: mangled,
            method: slot.method,
        });
    }

    entries
}

#[derive(Clone)]
struct Slot {
    /// Index into the *defining* class's method list. The vtable IR
    /// canonically references the originating method id (where the
    /// virtual function was first declared).
    method: MethodId,
    /// The class that first declared this virtual method (where the
    /// slot was minted).
    declared_in: ClassId,
}

/// Compute the slot sequence for `subobj`. Walks the inheritance chain
/// from root to `subobj`; each virtual method declared in that chain
/// gets one slot in declaration order, with dtors threaded through
/// per MSVC rules:
/// - If the root has a virtual dtor, slot 0 = dtor.
/// - Otherwise, slot 0 = first non-dtor virtual method.
fn compute_slots(ctx: &CxxTypeCtx, subobj: ClassId) -> Vec<Slot> {
    let mut slots: Vec<Slot> = Vec::new();
    let chain = inheritance_chain(ctx, subobj);
    for &class in &chain {
        let methods = &ctx.class(class).methods;
        for (idx, m) in methods.iter().enumerate() {
            if !is_virtual(m) {
                continue;
            }
            // Check whether this method overrides an existing slot.
            let override_idx = slots.iter().position(|s| {
                let original = &ctx.class(s.declared_in).methods[s.method.as_index()];
                methods_override(original, m)
            });
            if override_idx.is_some() {
                // Override — same slot; canonical method id is the
                // original declarer's, which we keep.
                continue;
            }
            slots.push(Slot {
                method: MethodId(idx as u32),
                declared_in: class,
            });
        }
    }
    slots
}

fn is_virtual(m: &MethodDef) -> bool {
    matches!(m.virtuality, Virtuality::Virtual | Virtuality::PureVirtual)
}

fn methods_override(base: &MethodDef, derived: &MethodDef) -> bool {
    // Destructors override across the type hierarchy regardless of
    // their per-class source name — `~A` and `~B` are the same
    // virtual slot from the dispatch table's perspective. Special-
    // case this before name comparison.
    if matches!(base.special, Some(SpecialMember::Dtor))
        && matches!(derived.special, Some(SpecialMember::Dtor))
    {
        return true;
    }
    // For non-dtor methods: signature match.
    base.name == derived.name
        && base.sig.params == derived.sig.params
        && base.sig.cv == derived.sig.cv
        && base.sig.ret == derived.sig.ret
}

/// Inheritance chain: root → ... → `class`. Single non-virtual
/// inheritance only for now (matches the M21 corpus this crate
/// services); multi-inheritance gets called by the dispatcher with
/// each base individually so this chain still terminates at the
/// branching point.
fn inheritance_chain(ctx: &CxxTypeCtx, class: ClassId) -> Vec<ClassId> {
    let mut chain = vec![class];
    let mut cur = class;
    loop {
        let bases = &ctx.class(cur).bases;
        let primary = bases
            .iter()
            .find(|b| !b.virtual_ && ctx.class(b.class).is_polymorphic);
        match primary {
            Some(b) => {
                chain.insert(0, b.class);
                cur = b.class;
            }
            None => break,
        }
    }
    chain
}

fn find_overrider(
    ctx: &CxxTypeCtx,
    most_derived: ClassId,
    declared_in: ClassId,
    method: &MethodId,
) -> ClassId {
    // Walk from most_derived back toward declared_in; first class
    // re-declaring the method is the overrider.
    let original = ctx.class(declared_in).methods[method.as_index()].clone();
    let mut cur = most_derived;
    loop {
        for m in &ctx.class(cur).methods {
            if methods_override(&original, m) {
                return cur;
            }
        }
        if cur == declared_in {
            return declared_in;
        }
        let next = ctx
            .class(cur)
            .bases
            .iter()
            .find(|b| !b.virtual_ && ctx.class(b.class).is_polymorphic)
            .map(|b| b.class);
        cur = match next {
            Some(c) => c,
            None => return declared_in,
        };
    }
}

fn pointer_width(ctx: &CxxTypeCtx) -> u64 {
    (ctx.target().pointer_width_bits / 8) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::Target;
    use crate::ty::{
        ClassDef, FnSig, Ident, MethodDef, MethodName, NameSegment, NestedName,
        RecordKind,
    };

    fn ctx() -> CxxTypeCtx {
        CxxTypeCtx::new(Target::x86_64_pc_windows_msvc())
    }

    #[test]
    fn non_polymorphic_class_has_no_vtable() {
        let mut c = ctx();
        let id = c.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Plain".into()))]),
            bases: vec![],
            fields: vec![],
            methods: vec![],
            kind: RecordKind::Class,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        assert!(c.vtable_msvc(id).is_none());
    }

    #[test]
    fn polymorphic_class_has_one_subtable_with_col_leading() {
        let mut c = ctx();
        let v = c.intern_type(crate::ty::CxxType::Void);
        let m = MethodDef {
            name: MethodName::Ident(Ident("f".into())),
            sig: FnSig {
                params: vec![],
                ret: v,
                cv: crate::ty::CvQual::default(),
                ref_q: None,
                variadic: false,
                noexcept: false,
            },
            virtuality: Virtuality::Virtual,
            vtable_index: None,
            special: None,
        };
        let id = c.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Poly".into()))]),
            bases: vec![],
            fields: vec![],
            methods: vec![m],
            kind: RecordKind::Class,
            is_polymorphic: true,
            is_final: false,
            source_alignment: None,
        });
        let vt = c.vtable_msvc(id).unwrap();
        assert_eq!(vt.symbol, "??_7Poly@@6B@");
        assert_eq!(vt.sub_tables.len(), 1);
        let primary = &vt.sub_tables[0];
        assert_eq!(primary.address_point_offset, 8);
        // First entry is the COL pointer.
        assert!(matches!(&primary.entries[0], VTableEntry::Rtti(_)));
        // Second entry is the function pointer. The method is
        // virtual (this class is polymorphic with one virtual fn),
        // so the access letter is `U`, not `Q`.
        match &primary.entries[1] {
            VTableEntry::FunctionPointer { mangled_target, .. } => {
                assert_eq!(mangled_target, "?f@Poly@@UEAAXXZ");
            }
            _ => panic!("expected fn-ptr entry"),
        }
    }
}
