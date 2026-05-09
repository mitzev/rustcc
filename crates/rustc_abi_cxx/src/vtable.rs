//! Primary vtable construction and RTTI symbol synthesis.
//!
//! See `docs/rustc_abi_cxx.md §7`.
//!
//! **Scope — single-inheritance primary vtable.** The on-disk layout per
//! Itanium is:
//!
//! - slot 0 — offset-to-top (always 0 in our v1 subset, no virtual base).
//! - slot 1 — RTTI pointer (`_ZTI<class>`).
//! - slot 2+ — function pointers in slot order.
//!
//! The vptr in an object points at slot 2, so `address_point_offset`
//! equals two pointer widths.
//!
//! Slot-order rules for single non-virtual inheritance:
//!
//! - The root of the polymorphic chain seeds the slots: `D1` then `D0`
//!   if it declares a virtual dtor, then every other virtual method in
//!   declaration order.
//! - Each class deeper in the chain either overrides an existing slot
//!   (signature match — same name, params, return, cv) or appends a new
//!   slot for a newly-declared virtual method.
//! - The dtor pair is always resolved to the **most-derived** class,
//!   because any class with a virtual-dtor base gets an implicit virtual
//!   dtor of its own. The algorithm seeds `overrider_class = most_derived`
//!   for the dtor slots up-front.
//! - Pure-virtual method slots emit `__cxa_pure_virtual` as the target.

use crate::ctx::CxxTypeCtx;
use crate::mangle::{DtorVariant, Symbol};
use crate::ty::{
    ClassId, MethodDef, MethodId, SpecialMember, Virtuality,
};

#[derive(Clone, Debug)]
pub struct VTable {
    pub class: ClassId,
    pub symbol: String,
    /// One or more sub-tables concatenated into a single `_ZTV<class>`
    /// symbol. `sub_tables[0]` is always the primary — the one the
    /// class's own `this` pointer consults. For polymorphic multi-
    /// inheritance, each non-primary polymorphic base contributes a
    /// further sub-table whose `offset_to_top` is the negative of the
    /// base's subobject offset within the most-derived class.
    pub sub_tables: Vec<VTableSubTable>,
}

#[derive(Clone, Debug)]
pub struct VTableSubTable {
    /// The subobject class this sub-table serves dispatch for.
    /// Equals the most-derived class for the primary.
    pub for_subobject: ClassId,
    /// Byte offset of `for_subobject` within the most-derived class.
    /// Always 0 for the primary sub-table.
    pub subobject_offset: u64,
    pub entries: Vec<VTableEntry>,
    /// Byte offset from the `_ZTV<class>` symbol start to this
    /// sub-table's address point (the slot a vptr points at).
    pub address_point_offset: u64,
}

#[derive(Clone, Debug)]
pub enum VTableEntry {
    /// Virtual-base-offset slot. Present in primary sub-tables of
    /// classes with virtual bases, preceding `OffsetToTop`. The value
    /// is the byte offset from the most-derived origin to the virtual
    /// base subobject. One `VbaseOffset` appears per virtual base in
    /// the class's chain, in reverse of appearance order.
    VbaseOffset(i64),
    OffsetToTop(i64),
    Rtti(String),
    FunctionPointer {
        mangled_target: String,
        method: MethodId,
    },
}

impl CxxTypeCtx {
    pub fn vtable(&self, class_id: ClassId) -> Option<VTable> {
        if !self.class(class_id).is_polymorphic {
            return None;
        }

        let ptr_bytes = (self.target().pointer_width_bits as u64) / 8;
        let mut sub_tables = Vec::new();
        let mut running_offset: u64 = 0;

        // Primary sub-table — serves the most-derived class's own vptr.
        // Starts with any vbase-offset slots (one per virtual base),
        // then `offset_to_top`, `RTTI`, then function pointers. The
        // address point is at the function-pointer region, i.e.
        // `(num_vbases + 2) * ptr_bytes` from the sub-table's start.
        let vbase_count = self
            .layout(class_id)
            .map(|l| l.virtual_base_offsets.len() as u64)
            .unwrap_or(0);
        let primary_entries = build_primary_entries(self, class_id);
        let primary_len = primary_entries.len() as u64;
        sub_tables.push(VTableSubTable {
            for_subobject: class_id,
            subobject_offset: 0,
            entries: primary_entries,
            address_point_offset: running_offset
                + (vbase_count + 2) * ptr_bytes,
        });
        running_offset += primary_len * ptr_bytes;

        // Secondary sub-tables — one per non-primary polymorphic base.
        // Requires the importer/layout to have populated `base_offsets`
        // for this class. We look up each non-virtual base and check if
        // it's polymorphic.
        let class = self.class(class_id);
        let primary_base = pick_primary_base(self, class);
        if let Ok(layout) = self.layout(class_id) {
            for &(base_id, base_offset) in &layout.base_offsets {
                if Some(base_id) == primary_base {
                    continue;
                }
                if !self.class(base_id).is_polymorphic {
                    continue;
                }
                let secondary = build_secondary_entries(
                    self,
                    class_id,
                    base_id,
                    base_offset,
                );
                let secondary_len = secondary.len() as u64;
                sub_tables.push(VTableSubTable {
                    for_subobject: base_id,
                    subobject_offset: base_offset,
                    entries: secondary,
                    address_point_offset: running_offset + 2 * ptr_bytes,
                });
                running_offset += secondary_len * ptr_bytes;
            }
        }

        let symbol = self.mangle(&Symbol::VTable(class_id));
        Some(VTable {
            class: class_id,
            symbol,
            sub_tables,
        })
    }

    pub fn rtti_symbol(&self, class_id: ClassId) -> String {
        self.mangle(&Symbol::TypeInfo(class_id))
    }
}

fn pick_primary_base(
    ctx: &CxxTypeCtx,
    class: &crate::ty::ClassDef,
) -> Option<ClassId> {
    if !class.is_polymorphic {
        return None;
    }
    for base in &class.bases {
        if ctx.class(base.class).is_polymorphic {
            return Some(base.class);
        }
    }
    None
}

fn build_primary_entries(
    ctx: &CxxTypeCtx,
    class_id: ClassId,
) -> Vec<VTableEntry> {
    let mut entries = Vec::new();
    // Virtual-base offset slots. Placed in reverse of appearance order
    // per Itanium §2.5.2 — the most-deeply-inherited vbase goes first.
    if let Ok(layout) = ctx.layout(class_id) {
        for (_vbase_id, vbase_off) in
            layout.virtual_base_offsets.iter().rev()
        {
            entries.push(VTableEntry::VbaseOffset(*vbase_off as i64));
        }
    }
    entries.push(VTableEntry::OffsetToTop(0));
    entries.push(VTableEntry::Rtti(ctx.rtti_symbol(class_id)));
    let slots = build_virtual_slots(ctx, class_id);
    for slot in &slots {
        entries.push(slot_to_entry(ctx, slot));
    }
    entries
}

/// Secondary sub-table: used by the `base_id` subobject's vptr for
/// dispatching methods declared on that base (or inherited into it).
/// Methods that the most-derived class overrides are reached via
/// `this`-adjusting thunks; methods the most-derived hasn't touched
/// point directly at the base's own implementation.
fn build_secondary_entries(
    ctx: &CxxTypeCtx,
    most_derived: ClassId,
    base_id: ClassId,
    base_offset: u64,
) -> Vec<VTableEntry> {
    let mut entries = Vec::new();
    // offset_to_top is negative: from the base subobject back to the
    // most-derived origin.
    entries.push(VTableEntry::OffsetToTop(-(base_offset as i64)));
    entries.push(VTableEntry::Rtti(ctx.rtti_symbol(most_derived)));

    // The base's virtual slots, resolved with overriders walked from
    // the most-derived class's perspective.
    let base_slots = build_virtual_slots(ctx, base_id);
    for slot in &base_slots {
        let slot = most_derived_override(ctx, most_derived, base_id, slot);
        let entry = slot_to_secondary_entry(ctx, &slot, base_offset);
        entries.push(entry);
    }
    entries
}

fn most_derived_override(
    ctx: &CxxTypeCtx,
    most_derived: ClassId,
    base_id: ClassId,
    slot: &VSlot,
) -> VSlot {
    // Dtor slots always resolve to the most-derived class.
    if matches!(slot.kind, VSlotKind::DtorD1 | VSlotKind::DtorD0) {
        return VSlot {
            kind: slot.kind,
            originator_class: slot.originator_class,
            originator_method_idx: slot.originator_method_idx,
            overrider_class: most_derived,
            overrider_method_idx: None,
        };
    }
    // For regular methods: scan the most-derived class's methods for a
    // signature match against the slot's originator method. Use the
    // most-derived method if found, otherwise keep the base's.
    let orig_idx = match slot.originator_method_idx {
        Some(i) => i,
        None => return slot.clone(),
    };
    let orig_method =
        &ctx.class(slot.originator_class).methods[orig_idx];
    for (idx, m) in ctx.class(most_derived).methods.iter().enumerate() {
        if m.virtuality == Virtuality::NonVirtual {
            continue;
        }
        if signatures_match(m, orig_method) {
            return VSlot {
                kind: slot.kind,
                originator_class: slot.originator_class,
                originator_method_idx: slot.originator_method_idx,
                overrider_class: most_derived,
                overrider_method_idx: Some(idx),
            };
        }
    }
    let _ = base_id;
    slot.clone()
}

fn slot_to_secondary_entry(
    ctx: &CxxTypeCtx,
    slot: &VSlot,
    base_offset: u64,
) -> VTableEntry {
    // If the overrider is the most-derived class (or any non-base
    // class), we need a this-adjusting thunk because callers on this
    // sub-table pass a `this` pointing at the BASE subobject, not at
    // the most-derived origin.
    let direct = slot_to_entry(ctx, slot);
    match direct {
        VTableEntry::FunctionPointer {
            mangled_target,
            method,
        } => {
            // Pure-virtual slots keep their literal target symbol.
            if mangled_target == "__cxa_pure_virtual" {
                return VTableEntry::FunctionPointer {
                    mangled_target,
                    method,
                };
            }
            let thunk = make_nv_thunk(&mangled_target, -(base_offset as i64));
            VTableEntry::FunctionPointer {
                mangled_target: thunk,
                method,
            }
        }
        other => other,
    }
}

/// Build an Itanium non-virtual this-adjusting thunk symbol for
/// `target` with the given adjustment to `this` (typically negative).
/// `_ZThn<|adj|>_<body>` for negative, `_ZTh<adj>_<body>` for positive.
fn make_nv_thunk(target: &str, this_adjust: i64) -> String {
    let body = target.strip_prefix("_Z").unwrap_or(target);
    if this_adjust < 0 {
        format!("_ZThn{}_{body}", this_adjust.unsigned_abs())
    } else {
        format!("_ZTh{}_{body}", this_adjust)
    }
}

// -------- Slot construction -------------------------------------------

#[derive(Clone, Debug)]
struct VSlot {
    kind: VSlotKind,
    /// The class that originally introduced this slot. Used to look up
    /// the slot's reference signature for override matching.
    originator_class: ClassId,
    originator_method_idx: Option<usize>,
    /// The current final overrider — the class whose implementation this
    /// slot will point at.
    overrider_class: ClassId,
    overrider_method_idx: Option<usize>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum VSlotKind {
    DtorD1,
    DtorD0,
    Method,
}

fn build_virtual_slots(
    ctx: &CxxTypeCtx,
    most_derived: ClassId,
) -> Vec<VSlot> {
    let chain = collect_chain(ctx, most_derived);
    let mut slots: Vec<VSlot> = Vec::new();

    for (level, &cls_id) in chain.iter().enumerate() {
        let cls = ctx.class(cls_id);

        if level == 0 && has_virtual_dtor(cls) {
            slots.push(VSlot {
                kind: VSlotKind::DtorD1,
                originator_class: cls_id,
                originator_method_idx: None,
                overrider_class: most_derived,
                overrider_method_idx: None,
            });
            slots.push(VSlot {
                kind: VSlotKind::DtorD0,
                originator_class: cls_id,
                originator_method_idx: None,
                overrider_class: most_derived,
                overrider_method_idx: None,
            });
        }

        for (idx, m) in cls.methods.iter().enumerate() {
            if m.virtuality == Virtuality::NonVirtual {
                continue;
            }
            // Dtors are represented as the D1/D0 slot pair; an explicitly
            // declared dtor at an intermediate level doesn't add slots
            // (the dtor slot is already targeted at `most_derived`).
            if m.special == Some(SpecialMember::Dtor) {
                continue;
            }

            match find_override_target(ctx, &slots, m) {
                Some(slot_idx) => {
                    slots[slot_idx].overrider_class = cls_id;
                    slots[slot_idx].overrider_method_idx = Some(idx);
                }
                None => slots.push(VSlot {
                    kind: VSlotKind::Method,
                    originator_class: cls_id,
                    originator_method_idx: Some(idx),
                    overrider_class: cls_id,
                    overrider_method_idx: Some(idx),
                }),
            }
        }
    }

    slots
}

fn has_virtual_dtor(cls: &crate::ty::ClassDef) -> bool {
    cls.methods.iter().any(|m| {
        m.virtuality != Virtuality::NonVirtual
            && m.special == Some(SpecialMember::Dtor)
    })
}

fn find_override_target(
    ctx: &CxxTypeCtx,
    slots: &[VSlot],
    new_method: &MethodDef,
) -> Option<usize> {
    for (slot_idx, slot) in slots.iter().enumerate() {
        if slot.kind != VSlotKind::Method {
            continue;
        }
        let orig_idx = slot.originator_method_idx?;
        let orig = &ctx.class(slot.originator_class).methods[orig_idx];
        if signatures_match(new_method, orig) {
            return Some(slot_idx);
        }
    }
    None
}

fn signatures_match(a: &MethodDef, b: &MethodDef) -> bool {
    a.name == b.name
        && a.sig.cv == b.sig.cv
        && a.sig.ret == b.sig.ret
        && a.sig.params == b.sig.params
}

/// Walk the non-virtual base chain, returning classes with the root
/// first and `class_id` last. Single-inheritance only.
fn collect_chain(ctx: &CxxTypeCtx, class_id: ClassId) -> Vec<ClassId> {
    let mut chain = Vec::new();
    let mut current = Some(class_id);
    while let Some(cls_id) = current {
        chain.push(cls_id);
        let cls = ctx.class(cls_id);
        current = cls.bases.iter().find(|b| !b.virtual_).map(|b| b.class);
    }
    chain.reverse();
    chain
}

// -------- Slot → entry lowering ---------------------------------------

fn slot_to_entry(ctx: &CxxTypeCtx, slot: &VSlot) -> VTableEntry {
    match slot.kind {
        VSlotKind::DtorD1 => function_slot(
            ctx.mangle(&Symbol::Dtor {
                class: slot.overrider_class,
                variant: DtorVariant::D1,
            }),
            // M22 partial: track the actual dtor MethodId on the
            // overrider class instead of hard-coding `MethodId(0)`.
            // The cxx_importer's `populate_vtable_indices` walker
            // (which is keyed by MethodId) used to mis-attribute
            // dtor slot ranks to the class's first method
            // (index 0); now it correctly assigns the rank to
            // the actual dtor MethodId. The originator_method_idx
            // here is the dtor's index in the originating
            // class's method list — for the override path this is
            // the BASE class's dtor index. We want the
            // OVERRIDER's dtor index because the slot is keyed
            // by the most-derived class's `class.methods`. Look
            // it up against the overrider class.
            find_dtor_method_id(ctx, slot.overrider_class),
        ),
        VSlotKind::DtorD0 => function_slot(
            ctx.mangle(&Symbol::Dtor {
                class: slot.overrider_class,
                variant: DtorVariant::D0,
            }),
            find_dtor_method_id(ctx, slot.overrider_class),
        ),
        VSlotKind::Method => {
            let method_idx = slot
                .overrider_method_idx
                .expect("method slot has an overrider method");
            let method =
                &ctx.class(slot.overrider_class).methods[method_idx];
            let target = if method.virtuality == Virtuality::PureVirtual {
                String::from("__cxa_pure_virtual")
            } else {
                ctx.mangle(&Symbol::Method {
                    class: slot.overrider_class,
                    name: method.name.clone(),
                    sig: method.sig.clone(),
                })
            };
            function_slot(target, MethodId(method_idx as u32))
        }
    }
}

fn function_slot(mangled_target: String, method: MethodId) -> VTableEntry {
    VTableEntry::FunctionPointer {
        mangled_target,
        method,
    }
}

/// M22 partial: find the index of `class_id`'s destructor in
/// its `methods` list, returning a `MethodId`. Falls back to
/// `MethodId(0)` if no dtor is declared on the class itself —
/// this matches the pre-fix behavior for classes whose dtor
/// is implicit (synthesized by the compiler) so the importer's
/// downstream walkers see the same shape they used to.
///
/// Used by `slot_to_entry` for `VSlotKind::DtorD1` /
/// `VSlotKind::DtorD0` slots. Without this, every dtor slot
/// reported `MethodId(0)`, which caused the
/// `cxx_importer::populate_vtable_indices` walker (keyed by
/// MethodId) to overwrite `methods[0].vtable_index` with the
/// dtor slot's rank — leaving the actual dtor method at the
/// end of the class's method list with `vtable_index = None`.
fn find_dtor_method_id(ctx: &CxxTypeCtx, class_id: ClassId) -> MethodId {
    let class = ctx.class(class_id);
    for (i, m) in class.methods.iter().enumerate() {
        if matches!(m.special, Some(SpecialMember::Dtor)) {
            return MethodId(i as u32);
        }
    }
    MethodId(0)
}
