//! MSVC C++ ABI record layout.
//!
//! Where MSVC diverges from Itanium (this module's reason to exist):
//!
//! - **No tail-padding reuse across bases.** Itanium lets a derived
//!   class place its own fields into the tail padding of a non-POD
//!   base subobject; MSVC always rounds the base subobject to its
//!   full `sizeof`. This matters for inheritance hierarchies that
//!   want the smallest possible layout.
//!
//! - **Empty Base Optimization is more restrictive.** MSVC only
//!   collapses an empty base to zero bytes when (a) it's the first
//!   base, (b) it has no virtual functions, and (c) the derived
//!   class doesn't already have a vptr or vbptr that would share
//!   address-0. Otherwise the empty base occupies 1 byte
//!   (rounded up to alignment).
//!
//! - **vbptr placement.** Classes with virtual bases get a `vbptr`
//!   (a pointer into the vbtable) placed *after* any non-virtual-base
//!   subobjects and field padding but *before* the virtual base
//!   subobjects themselves. The vbptr is 1 pointer-width.
//!
//! - **Bitfield allocation rules.** MSVC packs bitfields into
//!   storage units sized by the declared type (`int x : 3;` lives in
//!   an `int`-sized unit), with cross-unit straddling forbidden. The
//!   exact rules differ from Itanium when bitfield types mix —
//!   `short x : 3; int y : 4;` lays out differently under the two
//!   ABIs.
//!
//! - **`#pragma pack(N)`.** MSVC honors `#pragma pack` for both
//!   field offsets and the implicit padding-after-final-field; the
//!   default value is 8. Itanium-clang only applies it to field
//!   offsets and ignores the pad-to-align step. This module models
//!   `pragma_pack` as `Option<u64>` on the layout request because
//!   most classes don't override it.
//!
//! This module returns the same [`RecordLayout`] type that the
//! Itanium [`layout`] module produces. The dispatcher in
//! `CxxTypeCtx::layout` (added below) picks the right backend based on
//! `target().abi_flavor`.

use crate::ctx::CxxTypeCtx;
use crate::diag::LayoutError;
use crate::layout::RecordLayout;
use crate::ty::{ClassId, CxxType, RecordKind, TypeId};

impl CxxTypeCtx {
    /// MSVC-flavored layout entry point. Keep parity with
    /// `CxxTypeCtx::layout` (Itanium) — same return shape, same error
    /// surface. The dispatcher in `layout.rs` routes here when
    /// `target().abi_flavor == AbiFlavor::Msvc`.
    pub fn layout_msvc(&self, class_id: ClassId) -> Result<RecordLayout, LayoutError> {
        compute_layout(self, class_id)
    }
}

// -------- Working state ------------------------------------------------

struct State {
    /// Maximum byte touched so far.
    size: u64,
    /// Required alignment so far.
    align: u64,
    /// Next placement position.
    cursor: u64,
    has_vptr: bool,
    /// Whether a vbptr has been allocated. MSVC places at most one
    /// vbptr per class; it goes after the last non-virtual-base
    /// subobject (or after the vptr if there's one) and before any
    /// virtual base subobjects.
    has_vbptr: bool,
    field_offsets: Vec<u64>,
    field_bit_offsets: Vec<u8>,
    field_bit_widths: Vec<u64>,
    base_offsets: Vec<(ClassId, u64)>,
    virtual_base_offsets: Vec<(ClassId, u64)>,
    empty_subobjects: Vec<(ClassId, u64)>,
    /// `#pragma pack(N)` in effect for this class. `None` = no pragma
    /// (use natural alignment).
    pragma_pack: Option<u64>,
}

impl State {
    fn new(pragma_pack: Option<u64>) -> Self {
        Self {
            size: 0,
            align: 1,
            cursor: 0,
            has_vptr: false,
            has_vbptr: false,
            field_offsets: Vec::new(),
            field_bit_offsets: Vec::new(),
            field_bit_widths: Vec::new(),
            base_offsets: Vec::new(),
            virtual_base_offsets: Vec::new(),
            empty_subobjects: Vec::new(),
            pragma_pack,
        }
    }

    /// Apply `#pragma pack(N)` to an alignment requirement. The
    /// effective alignment is `min(natural, pragma_pack)`.
    fn cap(&self, natural: u64) -> u64 {
        match self.pragma_pack {
            Some(n) if natural > n => n,
            _ => natural,
        }
    }

    fn bump_align(&mut self, a: u64) {
        let capped = self.cap(a);
        if capped > self.align {
            self.align = capped;
        }
    }

    fn round_up_cursor(&mut self, a: u64) {
        let aa = self.cap(a);
        if aa > 1 {
            let rem = self.cursor % aa;
            if rem != 0 {
                self.cursor += aa - rem;
            }
        }
        if self.cursor > self.size {
            self.size = self.cursor;
        }
    }
}

// -------- Layout driver ------------------------------------------------

fn compute_layout(ctx: &CxxTypeCtx, class_id: ClassId) -> Result<RecordLayout, LayoutError> {
    let class = ctx.class(class_id);

    // Pragma pack is not yet plumbed through the side-tables on
    // `CxxTypeCtx` — when it lands, replace `None` with a context
    // lookup. For now MSVC default (8) is implicit.
    let mut st = State::new(None);

    // 1. Polymorphic class: vptr at offset 0 (no tail-padding sharing
    //    with bases under MSVC; the vptr always lives at the class's
    //    own offset 0 unless the primary base already provides one,
    //    in which case we reuse it).
    if class.is_polymorphic {
        let has_primary_polymorphic_base = class
            .bases
            .iter()
            .filter(|b| !b.virtual_)
            .any(|b| ctx.class(b.class).is_polymorphic);
        if !has_primary_polymorphic_base {
            st.has_vptr = true;
            st.bump_align(8);
            st.cursor = 8;
            st.size = 8;
        }
    }

    // 2. Non-virtual base subobjects, in declaration order. MSVC
    //    always rounds each base to its full `sizeof` — no tail-pad
    //    reuse.
    let mut has_virtual_bases = false;
    for base in &class.bases {
        if base.virtual_ {
            has_virtual_bases = true;
            continue;
        }
        let base_layout = ctx.layout_msvc(base.class)?;
        // Empty Base Optimization. MSVC: only the first base, and
        // only when neither has virtual functions or virtual bases,
        // and the derived class has not yet placed a vptr/vbptr.
        let base_size = base_layout.size_bytes;
        let base_is_empty = base_size == 0 || (base_size == 1 && base_layout.field_offsets.is_empty() && base_layout.base_offsets.is_empty());
        if base_is_empty
            && st.base_offsets.is_empty()
            && !st.has_vptr
            && !st.has_vbptr
            && !ctx.class(base.class).is_polymorphic
        {
            // EBO: zero bytes, but tracked.
            st.empty_subobjects.push((base.class, st.cursor));
            st.base_offsets.push((base.class, st.cursor));
            continue;
        }
        let base_align = base_layout.align_bytes.max(1);
        st.round_up_cursor(base_align);
        st.base_offsets.push((base.class, st.cursor));
        // MSVC: bump cursor by full `size`, not `data_size` — no
        // tail-padding reuse across bases.
        st.cursor += base_size;
        if st.cursor > st.size {
            st.size = st.cursor;
        }
        st.bump_align(base_align);
        // Inherit the vptr flag from any base that has one. The
        // primary polymorphic base case is already covered (vptr
        // skipped above for the derived class itself), but the
        // logical `has_vptr` for the derived class is still true
        // because the vptr is accessible through the inherited
        // base subobject.
        if base_layout.has_vptr {
            st.has_vptr = true;
        }
    }

    // 3. vbptr — inserted after non-virtual bases / fields-so-far,
    //    before virtual bases. Only present when this class itself
    //    introduces virtual inheritance and doesn't inherit a vbptr
    //    from a non-virtual base.
    let inherits_vbptr = class.bases.iter().any(|b| {
        !b.virtual_ && ctx
            .class(b.class)
            .bases
            .iter()
            .any(|gb| gb.virtual_)
    });
    if has_virtual_bases && !inherits_vbptr {
        st.bump_align(8);
        st.round_up_cursor(8);
        st.has_vbptr = true;
        st.cursor += 8;
        if st.cursor > st.size {
            st.size = st.cursor;
        }
    }

    // 4. Field placement.
    for (idx, field) in class.fields.iter().enumerate() {
        // Bitfield handling — simplified MSVC packing. Full rule:
        // bitfields live in a unit sized by the declared type;
        // mixing types starts a new unit. We model the simplified
        // "always start a new unit for each bitfield" form for v1;
        // the dispatcher will route mixed-type bitfields to the
        // Itanium engine until M21.c lands.
        if let Some(bit_width) = ctx.bitfield_width(class_id, idx) {
            let (ty_size, ty_align) = type_size_align(ctx, field.ty)?;
            st.bump_align(ty_align);
            st.round_up_cursor(ty_align);
            let byte_offset = st.cursor;
            st.field_offsets.push(byte_offset);
            st.field_bit_offsets.push(0);
            st.field_bit_widths.push(bit_width);
            // Bitfield consumes the storage unit.
            st.cursor += ty_size;
            if st.cursor > st.size {
                st.size = st.cursor;
            }
            continue;
        }

        let (sz, mut align) = type_size_align(ctx, field.ty)?;
        if let Some(explicit) = field.explicit_align {
            if explicit > align {
                align = explicit;
            }
        }
        st.bump_align(align);
        st.round_up_cursor(align);
        st.field_offsets.push(st.cursor);
        st.field_bit_offsets.push(0);
        st.field_bit_widths.push(0);
        st.cursor += sz;
        if st.cursor > st.size {
            st.size = st.cursor;
        }
    }

    // 5. Source-level `alignas` (applied before the nv-size snapshot
    //    so it folds into the non-virtual tail padding too).
    if let Some(src_align) = class.source_alignment {
        if src_align > st.align {
            st.align = src_align;
        }
    }

    // 6. Non-virtual size + alignment snapshot. MSVC reports nv_size
    //    as the *tail-padded* size — i.e. what `sizeof` would yield
    //    for the class if it had no virtual bases. Round up here.
    let nv_align = st.align.max(1);
    let nv_size = round_up(st.size, nv_align);

    // 7. Virtual base subobjects. These are appended to the end of
    //    the most-derived class only; intermediate classes lay them
    //    out for their own state but the final commit happens here.
    //    We start virtual bases from the nv-tail-padded offset, not
    //    the raw `st.size`, so the vbase subobjects sit at well-
    //    aligned offsets relative to the nv-size boundary.
    st.cursor = nv_size;
    if st.cursor > st.size {
        st.size = st.cursor;
    }
    for base in &class.bases {
        if !base.virtual_ {
            continue;
        }
        let base_layout = ctx.layout_msvc(base.class)?;
        let base_align = base_layout.align_bytes.max(1);
        st.bump_align(base_align);
        st.round_up_cursor(base_align);
        st.virtual_base_offsets.push((base.class, st.cursor));
        st.cursor += base_layout.size_bytes;
        if st.cursor > st.size {
            st.size = st.cursor;
        }
    }

    // 8. Final tail-padding round-up.
    let final_size = round_up(st.size, st.align.max(1));

    // Empty classes still get 1 byte (matching the Itanium rule).
    let empty = class.fields.is_empty()
        && class.bases.is_empty()
        && !st.has_vptr
        && !st.has_vbptr;
    let final_size = if empty && matches!(class.kind, RecordKind::Class | RecordKind::Struct) {
        1.max(final_size)
    } else {
        final_size
    };
    let final_align = st.align.max(1);

    Ok(RecordLayout {
        size_bytes: final_size,
        align_bytes: final_align,
        // MSVC has no data_size != size distinction at the class
        // level (no tail-pad reuse across bases) — we report
        // `data_size == size`.
        data_size_bytes: final_size,
        nv_size_bytes: nv_size,
        nv_align_bytes: nv_align,
        has_vptr: st.has_vptr,
        field_offsets: st.field_offsets,
        field_bit_offsets: st.field_bit_offsets,
        field_bit_widths: st.field_bit_widths,
        base_offsets: st.base_offsets,
        virtual_base_offsets: st.virtual_base_offsets,
        empty_subobjects: st.empty_subobjects,
    })
}

fn round_up(value: u64, align: u64) -> u64 {
    if align <= 1 {
        return value;
    }
    let rem = value % align;
    if rem == 0 { value } else { value + (align - rem) }
}

fn type_size_align(ctx: &CxxTypeCtx, ty: TypeId) -> Result<(u64, u64), LayoutError> {
    use crate::ty::{FloatKind, IntWidth};
    let t = ctx.type_of(ty).clone();
    let pw = ctx.target().pointer_width_bits as u64 / 8;
    Ok(match t {
        CxxType::Void => (0, 1),
        CxxType::Bool => (1, 1),
        CxxType::Int { width, .. } => match width {
            IntWidth::I8 => (1, 1),
            IntWidth::I16 => (2, 2),
            IntWidth::I32 => (4, 4),
            IntWidth::I64 => (8, 8),
            IntWidth::I128 => (16, 16),
        },
        CxxType::Float { kind } => match kind {
            FloatKind::F32 => (4, 4),
            FloatKind::F64 => (8, 8),
            // MSVC: long double == double (8 bytes).
            FloatKind::LongDouble => (8, 8),
        },
        CxxType::Ptr { .. } => (pw, pw),
        CxxType::Ref { .. } => (pw, pw),
        CxxType::Array { elem, len } => {
            let (sz, a) = type_size_align(ctx, elem)?;
            (sz * len, a)
        }
        CxxType::Record(class_id) => {
            let layout = ctx.layout_msvc(class_id)?;
            (layout.size_bytes, layout.align_bytes)
        }
        CxxType::Enum { underlying, .. } => type_size_align(ctx, underlying)?,
        CxxType::Fn(_) => (pw, pw),
        CxxType::MemberPtr { .. } => (pw * 2, pw), // simplified
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::Target;
    use crate::ty::{
        BaseSpec, ClassDef, FieldDef, Ident, NameSegment, NestedName,
    };

    fn ctx() -> CxxTypeCtx {
        CxxTypeCtx::new(Target::x86_64_pc_windows_msvc())
    }

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
    fn empty_class_is_one_byte() {
        let mut c = ctx();
        let id = c.define_class(class("E"));
        let l = c.layout_msvc(id).unwrap();
        assert_eq!(l.size_bytes, 1);
        assert_eq!(l.align_bytes, 1);
    }

    #[test]
    fn polymorphic_empty_class_is_pointer_size() {
        let mut c = ctx();
        let mut def = class("Poly");
        def.is_polymorphic = true;
        let id = c.define_class(def);
        let l = c.layout_msvc(id).unwrap();
        assert_eq!(l.size_bytes, 8);
        assert_eq!(l.align_bytes, 8);
        assert!(l.has_vptr);
    }

    #[test]
    fn two_int_fields() {
        let mut c = ctx();
        let i = c.intern_type(CxxType::Int {
            signed: true,
            width: crate::ty::IntWidth::I32,
        });
        let mut def = class("Pair");
        def.fields = vec![
            FieldDef { name: Ident("a".into()), ty: i, explicit_align: None },
            FieldDef { name: Ident("b".into()), ty: i, explicit_align: None },
        ];
        let id = c.define_class(def);
        let l = c.layout_msvc(id).unwrap();
        assert_eq!(l.size_bytes, 8);
        assert_eq!(l.align_bytes, 4);
        assert_eq!(l.field_offsets, vec![0, 4]);
    }

    #[test]
    fn msvc_no_tail_padding_reuse_across_bases() {
        // Itanium would let `Derived` place its `char` field in
        // `Base`'s tail padding (size 4, dsize 3 -> child placed at
        // offset 3, total 4 bytes). MSVC does NOT — child gets
        // offset 4, total 8 bytes (post tail-pad).
        //
        //   struct Base { int x; char y; };  // sizeof 8 under MSVC,
        //                                    // dsize 5 under Itanium
        //   struct Derived : Base { char z; }; // sizeof 12 vs Itanium's 8
        let mut c = ctx();
        let i = c.intern_type(CxxType::Int {
            signed: true,
            width: crate::ty::IntWidth::I32,
        });
        let ch = c.intern_type(CxxType::Int {
            signed: true,
            width: crate::ty::IntWidth::I8,
        });
        let base = c.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Base".into()))]),
            bases: vec![],
            fields: vec![
                FieldDef { name: Ident("x".into()), ty: i, explicit_align: None },
                FieldDef { name: Ident("y".into()), ty: ch, explicit_align: None },
            ],
            methods: vec![],
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        let derived = c.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Derived".into()))]),
            bases: vec![BaseSpec {
                class: base,
                virtual_: false,
                access: crate::ty::Access::Public,
            }],
            fields: vec![FieldDef {
                name: Ident("z".into()),
                ty: ch,
                explicit_align: None,
            }],
            methods: vec![],
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        // Base under MSVC: x=0..4, y=4..5, padded to 8.
        let l_base = c.layout_msvc(base).unwrap();
        assert_eq!(l_base.size_bytes, 8);
        // Derived: base [0..8], z at offset 8, padded up to 12.
        let l = c.layout_msvc(derived).unwrap();
        assert_eq!(l.base_offsets, vec![(base, 0)]);
        assert_eq!(l.field_offsets, vec![8]);
        // Derived size is 9 (rounded to 12 by base alignment of 4? char align = 1)
        // Actually align is 4 (from int in base), so 9 -> 12.
        assert_eq!(l.size_bytes, 12);
    }
}
