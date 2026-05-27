//! Itanium C++ ABI VTT (virtual-table-table) + construction
//! vtables. See `docs/rustc_abi_cxx.md §6` and the Itanium ABI
//! §2.6.2 ("VTT Order").
//!
//! ## What this is for
//!
//! The VTT (`_ZTT<class>`) and construction vtables
//! (`_ZTC<class><offset>_<base>`) are consumed *by constructors
//! and destructors* of classes with virtual bases. While a
//! base subobject is being constructed, its vptr must point at a
//! *construction vtable* reflecting the partially-built object's
//! layout, not the most-derived class's final vtable. The VTT is
//! the ordered table of those address points; the constructor
//! receives a hidden VTT pointer and walks it.
//!
//! ## Why rustcc computes it (and where it doesn't need to)
//!
//! For the **import** direction — the common case — rustcc binds
//! to the C++-compiled constructor symbol (`_ZN..C1..`), and the
//! C++ compiler (clang/gcc) emits + uses the VTT in its own TU.
//! rustcc never emits a VTT there. This module exists to:
//!
//! 1. serve as a **correctness oracle** (match clang's `_ZTT` /
//!    `_ZTC` exactly, validated by the `vtt_corpus` golden), and
//! 2. provide the layout groundwork for the **export** direction
//!    (a Rust-defined `#[repr(cpp)]` class with virtual bases),
//!    where rustcc *would* emit the VTT.
//!
//! ## Scope
//!
//! Implemented + clang-validated for the canonical single-shared-
//! virtual-base diamond (`D : B, C` with `B : virtual A`,
//! `C : virtual A`) and its degenerate cases. Deeper nesting
//! (virtual bases of virtual bases, a virtual base that is itself
//! a primary base, multiple distinct virtual bases) follows the
//! same `VTTBuilder` recursion but isn't golden-validated yet —
//! tracked as a follow-on.

use crate::ctx::CxxTypeCtx;
use crate::layout::{collect_virtual_bases, has_virtual_base_chain};
use crate::ty::ClassId;

/// A class's VTT — the ordered list of vtable address points a
/// virtual-base constructor/destructor walks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vtt {
    pub class: ClassId,
    /// `_ZTT<class>`.
    pub symbol: String,
    pub entries: Vec<VttEntry>,
}

/// One VTT slot: a reference to an address point inside some
/// vtable (the most-derived class's own vtable, or a construction
/// vtable for an intermediate base subobject).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VttEntry {
    pub vtable: VttVtableRef,
    /// Index of the sub-table within `vtable` whose address point
    /// this slot holds. 0 = that vtable's primary sub-table.
    pub sub_table: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VttVtableRef {
    /// The most-derived class's own vtable, `_ZTV<class>`.
    MostDerived,
    /// A construction vtable `_ZTC<class><offset>_<base>` for the
    /// `base` subobject at byte `offset` within the most-derived
    /// class.
    Construction { base: ClassId, offset: u64 },
}

/// A construction vtable descriptor: the `_ZTC..` symbol plus the
/// base subobject it serves. The entry *contents* mirror the
/// base's secondary vtable (offset-to-top, RTTI, vbase offsets,
/// function pointers); consumers that only need identity +
/// placement read `base`/`offset`/`symbol`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstructionVtable {
    /// `_ZTC<most_derived><offset>_<base>`.
    pub symbol: String,
    pub base: ClassId,
    pub offset: u64,
}

impl CxxTypeCtx {
    /// Compute the VTT for `class_id`, or `None` when the class
    /// has no virtual bases (no VTT is emitted in that case — the
    /// plain vtable suffices).
    ///
    /// **Itanium-only.** The VTT is an Itanium C++ ABI construct.
    /// MSVC has no VTT and no construction vtables — it drives
    /// virtual-base construction through the **vbtable** (a
    /// separate table reached via the vbptr, built by
    /// `vtable_msvc` / `layout_msvc`) plus constructor
    /// displacement (vtordisp). On an MSVC-flavored context this
    /// returns `None`; consult the vbtable instead.
    pub fn vtt(&self, class_id: ClassId) -> Option<Vtt> {
        if !matches!(self.target().abi_flavor, crate::target::AbiFlavor::Itanium) {
            return None;
        }
        if !self.class(class_id).is_polymorphic {
            return None;
        }
        if !has_virtual_base_chain(self, class_id) {
            return None;
        }

        let md_mangled = self.vtable_class_mangled(class_id);
        let symbol = format!("_ZTT{md_mangled}");

        let mut entries = Vec::new();

        // 1. Primary VTT entry — the most-derived class's primary
        //    vtable address point.
        entries.push(VttEntry {
            vtable: VttVtableRef::MostDerived,
            sub_table: 0,
        });

        // 2. Secondary VTTs — for each direct base that contributes
        //    a construction vtable (i.e. has its own virtual base
        //    chain), emit the base's construction-vtable address
        //    points: its primary sub-table, then one per virtual
        //    base it carries. Direct bases are visited primary-base
        //    first, then the rest in declaration order — matching
        //    clang's `VTTBuilder::LayoutSecondaryVTTs`.
        let class = self.class(class_id);
        let layout = self.layout(class_id);
        let base_offsets: Vec<(ClassId, u64)> = layout
            .as_ref()
            .map(|l| l.base_offsets.clone())
            .unwrap_or_default();
        let offset_of = |bid: ClassId| -> u64 {
            base_offsets
                .iter()
                .find(|(c, _)| *c == bid)
                .map(|(_, o)| *o)
                .unwrap_or(0)
        };

        for base in &class.bases {
            if base.virtual_ {
                continue; // virtual bases handled in step 3
            }
            if !has_virtual_base_chain(self, base.class) {
                continue; // no construction vtable needed
            }
            let off = offset_of(base.class);
            // sub-table 0: the base's own primary address point.
            entries.push(VttEntry {
                vtable: VttVtableRef::Construction { base: base.class, offset: off },
                sub_table: 0,
            });
            // one entry per virtual base the base subobject carries,
            // pointing at the corresponding sub-table of the *same*
            // construction vtable (index 1.. in declaration order).
            let sub_vbases = collect_virtual_bases(self, base.class);
            for i in 0..sub_vbases.len() {
                entries.push(VttEntry {
                    vtable: VttVtableRef::Construction { base: base.class, offset: off },
                    sub_table: 1 + i,
                });
            }
        }

        // 3. Secondary virtual pointers — address points inside the
        //    most-derived class's *own* vtable for its secondary and
        //    virtual-base sub-tables. clang emits the virtual-base
        //    sub-tables first, then the non-primary non-virtual
        //    secondary bases. Sub-table indices follow the vtable
        //    builder's ordering: [primary, secondary.., vbase..].
        if let Some(vt) = self.vtable(class_id) {
            // Map each sub-table to its index, then emit vbase
            // sub-tables, then non-primary secondary sub-tables.
            let vbases = collect_virtual_bases(self, class_id);
            for vb in &vbases {
                if let Some(idx) = vt
                    .sub_tables
                    .iter()
                    .position(|st| st.for_subobject == *vb)
                {
                    entries.push(VttEntry { vtable: VttVtableRef::MostDerived, sub_table: idx });
                }
            }
            // Non-primary, non-virtual secondary bases (skip the
            // primary at index 0 and any vbase already emitted).
            for (idx, st) in vt.sub_tables.iter().enumerate() {
                if idx == 0 {
                    continue;
                }
                if vbases.contains(&st.for_subobject) {
                    continue;
                }
                entries.push(VttEntry { vtable: VttVtableRef::MostDerived, sub_table: idx });
            }
        }

        Some(Vtt { class: class_id, symbol, entries })
    }

    /// Construction vtables for `class_id` — one per direct
    /// non-virtual base that carries its own virtual base(s). The
    /// symbol form is `_ZTC<class><offset>_<base>`.
    ///
    /// **Itanium-only** (see [`Self::vtt`]); returns empty on MSVC.
    pub fn construction_vtables(&self, class_id: ClassId) -> Vec<ConstructionVtable> {
        if !matches!(self.target().abi_flavor, crate::target::AbiFlavor::Itanium) {
            return Vec::new();
        }
        if !has_virtual_base_chain(self, class_id) {
            return Vec::new();
        }
        let md = self.vtable_class_mangled(class_id);
        let class = self.class(class_id);
        let layout = self.layout(class_id);
        let base_offsets: Vec<(ClassId, u64)> = layout
            .as_ref()
            .map(|l| l.base_offsets.clone())
            .unwrap_or_default();
        let mut out = Vec::new();
        for base in &class.bases {
            if base.virtual_ {
                continue;
            }
            if !has_virtual_base_chain(self, base.class) {
                continue;
            }
            let off = base_offsets
                .iter()
                .find(|(c, _)| *c == base.class)
                .map(|(_, o)| *o)
                .unwrap_or(0);
            let base_mangled = self.vtable_class_mangled(base.class);
            out.push(ConstructionVtable {
                symbol: format!("_ZTC{md}{off}_{base_mangled}"),
                base: base.class,
                offset: off,
            });
        }
        out
    }

    /// The Itanium-mangled `<name>` portion of a class's vtable
    /// symbol — i.e. `_ZTV<name>` with the `_ZTV` prefix stripped.
    /// Used to assemble `_ZTT` / `_ZTC` symbols without adding new
    /// `Symbol` variants.
    fn vtable_class_mangled(&self, class_id: ClassId) -> String {
        let v = self.mangle(&crate::mangle::Symbol::VTable(class_id));
        v.strip_prefix("_ZTV").unwrap_or(&v).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::Target;
    use crate::ty::{
        Access, BaseSpec, ClassDef, CvQual, CxxType, FieldDef, FnSig, Ident,
        MethodDef, MethodName, NameSegment, NestedName, RecordKind, TypeId,
        Virtuality,
    };

    fn nested(parts: &[&str]) -> NestedName {
        NestedName(parts.iter().map(|p| NameSegment::Class(Ident((*p).into()))).collect())
    }
    fn int_ty(ctx: &mut CxxTypeCtx) -> TypeId {
        ctx.intern_type(CxxType::Int { signed: true, width: crate::ty::IntWidth::I32 })
    }
    fn void_ty(ctx: &mut CxxTypeCtx) -> TypeId {
        ctx.intern_type(CxxType::Void)
    }
    fn virt_method(name: &str, ret: TypeId) -> MethodDef {
        MethodDef {
            name: MethodName::Ident(Ident(name.into())),
            sig: FnSig {
                params: vec![],
                ret,
                cv: CvQual::default(),
                ref_q: None,
                variadic: false,
                noexcept: false,
            },
            virtuality: Virtuality::Virtual,
            vtable_index: None,
            special: None,
        }
    }
    fn field(name: &str, ty: TypeId) -> FieldDef {
        FieldDef { name: Ident(name.into()), ty, explicit_align: None }
    }

    /// Build the canonical diamond:
    ///   struct A { virtual void fa(); int a; };
    ///   struct B : virtual A { virtual void fb(); int b; };
    ///   struct C : virtual A { virtual void fc(); int c; };
    ///   struct D : B, C { virtual void fd(); int d; };
    /// Returns the four ClassIds (a, b, c, d).
    fn build_diamond(ctx: &mut CxxTypeCtx) -> (ClassId, ClassId, ClassId, ClassId) {
        let void = void_ty(ctx);
        let int = int_ty(ctx);
        let a = ctx.define_class(ClassDef {
            name: nested(&["A"]),
            bases: vec![],
            fields: vec![field("a", int)],
            methods: vec![virt_method("fa", void)],
            kind: RecordKind::Struct,
            is_polymorphic: true,
            is_final: false,
            source_alignment: None,
        });
        let vbase_a = BaseSpec { class: a, virtual_: true, access: Access::Public };
        let b = ctx.define_class(ClassDef {
            name: nested(&["B"]),
            bases: vec![vbase_a.clone()],
            fields: vec![field("b", int)],
            methods: vec![virt_method("fb", void)],
            kind: RecordKind::Struct,
            is_polymorphic: true,
            is_final: false,
            source_alignment: None,
        });
        let c = ctx.define_class(ClassDef {
            name: nested(&["C"]),
            bases: vec![vbase_a],
            fields: vec![field("c", int)],
            methods: vec![virt_method("fc", void)],
            kind: RecordKind::Struct,
            is_polymorphic: true,
            is_final: false,
            source_alignment: None,
        });
        let d = ctx.define_class(ClassDef {
            name: nested(&["D"]),
            bases: vec![
                BaseSpec { class: b, virtual_: false, access: Access::Public },
                BaseSpec { class: c, virtual_: false, access: Access::Public },
            ],
            fields: vec![field("d", int)],
            methods: vec![virt_method("fd", void)],
            kind: RecordKind::Struct,
            is_polymorphic: true,
            is_final: false,
            source_alignment: None,
        });
        (a, b, c, d)
    }

    #[test]
    fn no_vtt_without_virtual_bases() {
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let void = void_ty(&mut ctx);
        let plain = ctx.define_class(ClassDef {
            name: nested(&["Plain"]),
            bases: vec![],
            fields: vec![],
            methods: vec![virt_method("f", void)],
            kind: RecordKind::Struct,
            is_polymorphic: true,
            is_final: false,
            source_alignment: None,
        });
        assert!(ctx.vtt(plain).is_none(), "no virtual bases ⇒ no VTT");
        assert!(ctx.construction_vtables(plain).is_empty());
    }

    #[test]
    fn diamond_construction_vtable_symbols_match_clang() {
        // clang emits `_ZTC1D0_1B` (B at offset 0) and
        // `_ZTC1D16_1C` (C at offset 16) for the diamond.
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let (_a, b, c, d) = build_diamond(&mut ctx);
        let cvts = ctx.construction_vtables(d);
        assert_eq!(cvts.len(), 2, "two ctor vtables (B, C): {cvts:?}");
        assert_eq!(cvts[0].base, b);
        assert_eq!(cvts[0].symbol, "_ZTC1D0_1B");
        assert_eq!(cvts[1].base, c);
        assert_eq!(cvts[1].offset, 16);
        assert_eq!(cvts[1].symbol, "_ZTC1D16_1C");
    }

    #[test]
    fn msvc_has_no_vtt_or_construction_vtables() {
        // VTT + construction vtables are Itanium-only. On MSVC the
        // diamond uses a vbtable (vtable_msvc/layout_msvc), so vtt()
        // must return None and construction_vtables() empty — never
        // garbage `_ZTT??_7..` symbols built from MSVC manglings.
        let mut ctx = CxxTypeCtx::new(Target::x86_64_pc_windows_msvc());
        let (_a, _b, _c, d) = build_diamond(&mut ctx);
        assert!(ctx.vtt(d).is_none(), "MSVC has no VTT");
        assert!(
            ctx.construction_vtables(d).is_empty(),
            "MSVC has no construction vtables"
        );
    }

    #[test]
    fn diamond_vtt_structure_matches_clang() {
        // clang's `_ZTT1D` is a 7-entry table:
        //   0: D's own vtable, primary sub-table
        //   1: B-in-D ctor vtable, primary
        //   2: B-in-D ctor vtable, A-vbase sub-table
        //   3: C-in-D ctor vtable, primary
        //   4: C-in-D ctor vtable, A-vbase sub-table
        //   5: D's own vtable, A-vbase sub-table
        //   6: D's own vtable, C-secondary sub-table
        let mut ctx = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
        let (_a, b, c, d) = build_diamond(&mut ctx);
        let vtt = ctx.vtt(d).expect("D has virtual bases ⇒ VTT");
        assert_eq!(vtt.symbol, "_ZTT1D");
        assert_eq!(vtt.entries.len(), 7, "7-entry VTT: {:#?}", vtt.entries);

        use VttVtableRef::*;
        assert_eq!(vtt.entries[0], VttEntry { vtable: MostDerived, sub_table: 0 });
        assert_eq!(vtt.entries[1], VttEntry { vtable: Construction { base: b, offset: 0 }, sub_table: 0 });
        assert_eq!(vtt.entries[2], VttEntry { vtable: Construction { base: b, offset: 0 }, sub_table: 1 });
        assert_eq!(vtt.entries[3], VttEntry { vtable: Construction { base: c, offset: 16 }, sub_table: 0 });
        assert_eq!(vtt.entries[4], VttEntry { vtable: Construction { base: c, offset: 16 }, sub_table: 1 });
        // Entries 5 + 6 are D's own secondary virtual pointers:
        // the A-vbase sub-table, then the C-secondary sub-table.
        // Both reference MostDerived; assert that and that they're
        // distinct sub-tables (vbase ≠ secondary).
        assert!(matches!(vtt.entries[5], VttEntry { vtable: MostDerived, .. }));
        assert!(matches!(vtt.entries[6], VttEntry { vtable: MostDerived, .. }));
        assert_ne!(vtt.entries[5].sub_table, vtt.entries[6].sub_table);
    }
}
