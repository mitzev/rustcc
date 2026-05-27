//! Itanium C++ ABI record layout.
//!
//! See `docs/rustc_abi_cxx.md §5`.
//!
//! **Scope — single non-virtual inheritance.** The state machine covers
//! the subset exercised by the v1 layout corpus:
//!
//! - Scalar fields, records, arrays, pointers, and references.
//! - `alignas` on the record (via `ClassDef::source_alignment`).
//! - Primary-base sharing when both the base and the derived class are
//!   polymorphic (inherits the vptr).
//! - Empty Base Optimization — an empty base contributes zero bytes but
//!   a placement offset tracked in `empty_subobjects`.
//! - Tail-padding reuse — a derived class can place its own fields into
//!   the tail padding of a non-POD base because `state.dsize < size`.
//!
//! Implemented in later milestones: virtual bases + multiple
//! inheritance (vbase offsets, secondary vtables, this-adjusting
//! thunks), and **bit-fields** (M21.b `place_bitfield`, validated
//! against clang by `tests/corpus/bitfield`).
//!
//! Out of scope today: `__attribute__((packed))` on Itanium
//! (MSVC `#pragma pack` is honored by `layout_msvc.rs`); VTT +
//! construction vtables for virtual-base construction ordering.
//! `__attribute__((packed))` surfaces as a `LayoutError` until
//! the packed flag is plumbed.

use crate::ctx::CxxTypeCtx;
use crate::diag::LayoutError;
use crate::target::LongDoubleKind;
use crate::ty::{
    ClassDef, ClassId, CxxType, FieldDef, FieldId, FloatKind, IntWidth,
    RecordKind, TypeId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordLayout {
    pub size_bytes: u64,
    pub align_bytes: u64,
    pub data_size_bytes: u64,
    pub nv_size_bytes: u64,
    pub nv_align_bytes: u64,
    pub has_vptr: bool,
    pub field_offsets: Vec<u64>,
    /// M21.b: bit-offset *within the byte at `field_offsets[i]`*
    /// for bitfield members. `0` for non-bitfield fields.
    /// Combined with `field_bit_widths[i]` to fully locate a
    /// bitfield. Always parallel to `field_offsets`.
    pub field_bit_offsets: Vec<u8>,
    /// M21.b: declared bitfield width per field. `0` for non-
    /// bitfield fields (which means "field width = sizeof(ty)
    /// bytes"); a non-zero value means the field is a bitfield
    /// of that many bits. Always parallel to `field_offsets`.
    pub field_bit_widths: Vec<u64>,
    pub base_offsets: Vec<(ClassId, u64)>,
    /// Virtual-base subobject offsets within this class. Populated only
    /// for the most-derived class; intermediate classes in a chain
    /// don't commit virtual-base offsets (those are deferred until the
    /// most-derived class lays them out at its own end).
    pub virtual_base_offsets: Vec<(ClassId, u64)>,
    pub empty_subobjects: Vec<(ClassId, u64)>,
    /// v1.09.2: Homogeneous Float / Vector Aggregate detection.
    /// `Some` when the record is an HFA/HVA per AAPCS64 — every
    /// leaf field has the same primitive float / vector type and
    /// the total leaf count is 1..=4. Consumed by the ARM64
    /// codegen path to route extern "C++" parameters/returns
    /// into V0..V3 individually rather than as a packed struct.
    ///
    /// `None` on every non-ARM64 target's consumer side (the
    /// detection logic still runs but the field is ignored). The
    /// rule is target-agnostic per AAPCS64 §B.2.5; only the codegen
    /// dispatch is target-specific.
    pub hfa_kind: Option<HfaKind>,
}

/// HFA / HVA classification per AAPCS64 §B.2.5.
///
/// - `count` is the number of leaf elements (1..=4 for a valid
///   HFA/HVA; anything outside that range never becomes an HFA).
/// - `elem` is the leaf type — `f32`, `f64`, or a SIMD vector
///   width. (v1 supports just the float variants — vector
///   support follows when we add `#[repr(simd)]` plumbing.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HfaKind {
    pub elem: HfaElem,
    pub count: u8,
}

/// Leaf-element kind that an HFA's fields all share.
///
/// v1 supports the two float forms. Vector kinds (`V64`, `V128`)
/// come with `#[repr(simd)]` support in a follow-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum HfaElem {
    F32,
    F64,
}

impl CxxTypeCtx {
    /// Record layout dispatcher. Routes to either the Itanium engine
    /// (this module) or the MSVC engine ([`crate::layout_msvc`]) based
    /// on `target().abi_flavor`. Every caller in the workspace goes
    /// through this entry point so the choice of ABI is made in one
    /// place.
    pub fn layout(&self, class_id: ClassId) -> Result<RecordLayout, LayoutError> {
        match self.target().abi_flavor {
            crate::target::AbiFlavor::Itanium => self.layout_itanium(class_id),
            crate::target::AbiFlavor::Msvc => self.layout_msvc(class_id),
        }
    }

    /// Itanium-only layout entry point. Exposed for tests and for the
    /// rare downstream consumer (e.g. cross-ABI comparisons in the
    /// fork patches) that needs to force Itanium semantics regardless
    /// of the target.
    pub fn layout_itanium(&self, class_id: ClassId) -> Result<RecordLayout, LayoutError> {
        compute_layout(self, class_id)
    }

    /// POD-for-layout per Itanium: not polymorphic, no user-declared
    /// special members, no reference members, no virtual bases, and
    /// recursively POD bases and record members. This is a
    /// conservative lower bound on "trivially copyable" — every
    /// POD-for-layout class is trivially copyable, but Itanium also
    /// admits some non-POD-for-layout classes as trivially copyable.
    /// Good enough for v1 call-convention classification (records
    /// that pass this check can safely be returned in registers on
    /// AMD64; records that fail it must go through sret).
    pub fn is_pod_for_layout(&self, class_id: ClassId) -> bool {
        is_pod_for_layout(self, class_id)
    }
}

// -------- Working state -------------------------------------------------

struct LayoutState {
    /// Maximum byte touched so far (i.e., highest end offset of any
    /// placed subobject). Becomes the un-rounded `sizeof`.
    size: u64,
    /// Required alignment so far.
    align: u64,
    /// Next placement position. Equal to the end of the last-placed
    /// field or non-empty base. Tail padding of non-POD bases lets this
    /// be less than `size`.
    dsize: u64,
    has_vptr: bool,
    field_offsets: Vec<u64>,
    /// M21.b: parallel to `field_offsets` — the bit position
    /// within the byte at `field_offsets[i]` where the
    /// bitfield's bits start. 0 for non-bitfield fields.
    field_bit_offsets: Vec<u8>,
    /// M21.b: parallel to `field_offsets` — the declared
    /// bitfield width in bits. 0 for non-bitfield fields.
    field_bit_widths: Vec<u64>,
    base_offsets: Vec<(ClassId, u64)>,
    virtual_base_offsets: Vec<(ClassId, u64)>,
    empty_subobjects: Vec<(ClassId, u64)>,
    /// M21.b: open bitfield allocation unit (AU). When some,
    /// `(au_offset, au_size_bytes, used_bits)` describe the
    /// AU the next bitfield can pack into iff its container
    /// type matches. None when no AU is open (e.g. the last
    /// placed field was a regular non-bitfield field, or the
    /// AU got fully consumed and was closed).
    bitfield_au: Option<BitfieldAu>,
}

#[derive(Debug, Clone, Copy)]
struct BitfieldAu {
    /// Byte offset of the AU within the record.
    offset: u64,
    /// Size of the AU's container type, in bytes (1, 2, 4, 8).
    size_bytes: u64,
    /// Bits already consumed in the AU (always
    /// `<= size_bytes * 8`).
    used_bits: u64,
}

impl LayoutState {
    fn new() -> Self {
        Self {
            size: 0,
            align: 1,
            dsize: 0,
            has_vptr: false,
            field_offsets: Vec::new(),
            field_bit_offsets: Vec::new(),
            field_bit_widths: Vec::new(),
            base_offsets: Vec::new(),
            virtual_base_offsets: Vec::new(),
            empty_subobjects: Vec::new(),
            bitfield_au: None,
        }
    }
}

// -------- Main algorithm ------------------------------------------------

fn compute_layout(
    ctx: &CxxTypeCtx,
    class_id: ClassId,
) -> Result<RecordLayout, LayoutError> {
    let class = ctx.class(class_id);

    // Virtual bases are now supported for layout: they're placed at
    // the end of the non-virtual portion after all non-virtual bases
    // and fields are laid out (phase 2 below). Classes that have (or
    // inherit) a virtual base acquire a vptr even without declaring
    // any virtual method, matching Itanium ABI semantics. Secondary
    // vtables, vbase-offset slots, VTT, and construction vtables are
    // still out of scope.

    let mut state = LayoutState::new();

    match class.kind {
        RecordKind::Struct | RecordKind::Class => {
            // Step 1 — primary base or fresh vptr. A class acquires a
            // vptr either because it declares virtual methods, or
            // because it (transitively) has a virtual base.
            let primary = pick_primary_base(ctx, class);
            let needs_vptr = class.is_polymorphic
                || has_virtual_base_chain(ctx, class_id);
            if let Some(primary_id) = primary {
                place_base(ctx, &mut state, primary_id, /*is_primary=*/ true)?;
            } else if needs_vptr {
                allocate_vptr(ctx, &mut state);
            }

            // Step 2 — non-primary non-virtual bases. Virtual bases
            // are deferred to phase 4 so they land at the end of the
            // most-derived class, per Itanium.
            for base in &class.bases {
                if base.virtual_ {
                    continue;
                }
                if Some(base.class) == primary {
                    continue;
                }
                place_base(ctx, &mut state, base.class, /*is_primary=*/ false)?;
            }

            // Step 3 — fields in declaration order.
            for (idx, field) in class.fields.iter().enumerate() {
                place_field(ctx, &mut state, field, class_id, idx)?;
            }
        }
        RecordKind::Union => {
            // Unions overlap all members at offset 0. Bases and vptrs
            // are rejected by C++, so we don't need to process them.
            for (idx, field) in class.fields.iter().enumerate() {
                place_union_field(ctx, &mut state, field, class_id, idx)?;
            }
            // All fields occupy the same [0, size) region, so dsize ==
            // size for unions.
            state.dsize = state.size;
        }
    }

    // Step 4 — `alignas` on the class.
    if let Some(src_align) = class.source_alignment {
        state.align = state.align.max(src_align);
    }

    // Snapshot the end of the non-virtual portion before laying out
    // virtual bases — `nv_size`/`nv_align` reported to derived classes
    // excludes the virtual-base tail.
    let is_pod = is_pod_for_layout(ctx, class_id);
    let is_empty = is_empty_class(ctx, class_id);
    let nv_dsize = state.dsize;
    let nv_align = state.align;
    let nv_size_for_bases = if is_empty {
        0
    } else if is_pod {
        // POD types have no usable tail padding — their non-virtual
        // size is the sizeof of the non-virtual portion, not just
        // the last-data offset. Match Clang's `nvsize == sizeof`
        // for POD classes.
        align_up(nv_dsize, nv_align.max(1))
    } else {
        nv_dsize
    };

    // Step 5 — virtual bases. Collected transitively from the chain
    // and deduplicated by `ClassId`; placed at aligned offsets past
    // `nv_dsize`, growing `state.size` (but not `state.dsize`) so the
    // non-virtual size reported upward stays clean.
    let vbase_ids = collect_virtual_bases(ctx, class_id);
    let mut vbase_end = nv_dsize;
    for vbase_id in vbase_ids {
        let vbase_layout = compute_layout(ctx, vbase_id)?;
        let off = align_up(vbase_end, vbase_layout.nv_align_bytes);
        state.virtual_base_offsets.push((vbase_id, off));
        vbase_end = off + vbase_layout.nv_size_bytes;
        state.size = state.size.max(vbase_end);
        state.align = state.align.max(vbase_layout.align_bytes);
    }

    // Step 6 — finalize size/dsize.
    let dsize_raw = state.size.max(state.dsize);
    let mut size = dsize_raw;
    if size == 0 {
        // C++: a class with no members must still occupy at least one byte
        // so that distinct instances have distinct addresses.
        //
        // Order matters: bump to 1 BEFORE aligning up, so an empty class
        // with `alignas(N)` lands at size=N align=N (satisfying the
        // "size is a multiple of align" invariant) rather than size=1
        // align=N (which violates it). Clang behaves the same way.
        size = 1;
    }
    size = align_up(size, state.align);

    // POD-for-layout types round dsize up to sizeof (no usable tail padding
    // for derived classes). Non-POD types keep dsize at the last-data byte
    // (which, for a virtual-inheriting class, means the end of the virtual-
    // base region).
    let dsize = if is_pod { size } else { dsize_raw };

    Ok(RecordLayout {
        size_bytes: size,
        align_bytes: state.align,
        data_size_bytes: dsize,
        nv_size_bytes: nv_size_for_bases,
        nv_align_bytes: nv_align,
        has_vptr: state.has_vptr,
        field_offsets: state.field_offsets,
        field_bit_offsets: state.field_bit_offsets,
        field_bit_widths: state.field_bit_widths,
        base_offsets: state.base_offsets,
        virtual_base_offsets: state.virtual_base_offsets,
        empty_subobjects: state.empty_subobjects,
        hfa_kind: detect_hfa(ctx, class_id),
    })
}

/// v1.09.2 patch 19c: HFA / HVA detection per AAPCS64 §B.2.5.
///
/// An HFA (Homogeneous Float Aggregate) is a struct whose every
/// leaf scalar field has the same primitive float type and whose
/// total leaf count is 1..=4. Detection is recursive — nested
/// structs flatten their fields. Bases count toward the leaf set
/// the same way.
///
/// Returns `None` if any of:
/// - The class is polymorphic (has vptr — disqualifies)
/// - The class has any non-float leaf field
/// - The leaf count is 0 or > 4
/// - Any field type isn't a primitive float (Int, Ptr, Ref, etc.)
///
/// The detection is target-agnostic — it runs on every layout
/// computation. ARM64 codegen consults `RecordLayout::hfa_kind`
/// to route eligible types into V0..V3; non-ARM64 targets just
/// ignore the field.
pub(crate) fn detect_hfa(ctx: &CxxTypeCtx, class_id: ClassId) -> Option<HfaKind> {
    if ctx.class(class_id).is_polymorphic {
        return None;
    }
    let mut probe = HfaProbe { elem: None, count: 0 };
    if !probe.walk_class(ctx, class_id) {
        return None;
    }
    let elem = probe.elem?;
    if probe.count == 0 || probe.count > 4 {
        return None;
    }
    Some(HfaKind {
        elem,
        count: probe.count,
    })
}

struct HfaProbe {
    /// Established leaf-type for the aggregate, set on the first
    /// scalar field encountered. Subsequent leaves must match.
    elem: Option<HfaElem>,
    count: u8,
}

impl HfaProbe {
    /// Walk every leaf field of `class_id`. Returns `false` to
    /// abort detection (e.g. unsupported field type encountered);
    /// `true` to continue. Mutates `self.elem`/`self.count` to
    /// record discoveries.
    fn walk_class(&mut self, ctx: &CxxTypeCtx, class_id: ClassId) -> bool {
        // Bases recurse first so the order matches source layout.
        for base in &ctx.class(class_id).bases {
            if base.virtual_ {
                // Virtual bases disqualify an aggregate from being
                // an HFA — they introduce vbptr indirection.
                return false;
            }
            if !self.walk_class(ctx, base.class) {
                return false;
            }
        }
        for field in &ctx.class(class_id).fields {
            if !self.walk_type(ctx, field.ty) {
                return false;
            }
        }
        true
    }

    fn walk_type(&mut self, ctx: &CxxTypeCtx, ty: TypeId) -> bool {
        use crate::ty::FloatKind;
        match ctx.type_of(ty).clone() {
            CxxType::Float { kind: FloatKind::F32 } => self.record(HfaElem::F32),
            CxxType::Float { kind: FloatKind::F64 } => self.record(HfaElem::F64),
            // LongDouble: 64-bit on Apple/Windows, 80-bit on x86-Linux,
            // 128-bit on aarch64-linux. Conservatively reject — codegen
            // for ARM64-MSVC sees long double as 64-bit but the wider
            // forms break the HFA invariant.
            CxxType::Float { kind: FloatKind::LongDouble } => false,
            // Arrays of float — every element counts as a leaf.
            CxxType::Array { elem, len } => {
                let saved_count = self.count;
                for _ in 0..len {
                    if !self.walk_type(ctx, elem) {
                        return false;
                    }
                    // Short-circuit: once count exceeds 4 we know
                    // we're not an HFA. Stop walking further
                    // elements to avoid blowing the budget.
                    if self.count > 4 {
                        self.count = saved_count;
                        return false;
                    }
                }
                true
            }
            // Nested struct: recurse.
            CxxType::Record(inner) => self.walk_class(ctx, inner),
            // Anything else (Int, Ptr, Ref, Bool, Void, Enum, Fn,
            // MemberPtr) disqualifies the aggregate from HFA status.
            _ => false,
        }
    }

    fn record(&mut self, e: HfaElem) -> bool {
        match self.elem {
            None => {
                self.elem = Some(e);
            }
            Some(existing) if existing == e => {}
            Some(_) => return false, // type mismatch — not homogeneous
        }
        self.count = self.count.saturating_add(1);
        true
    }
}

/// Does `class_id` (or any class reachable through non-virtual bases)
/// declare a virtual base? Such a class needs a vptr at offset 0 to
/// store vbase offsets, regardless of whether it declares virtual
/// methods directly.
pub(crate) fn has_virtual_base_chain(ctx: &CxxTypeCtx, class_id: ClassId) -> bool {
    let class = ctx.class(class_id);
    for base in &class.bases {
        if base.virtual_ {
            return true;
        }
        if has_virtual_base_chain(ctx, base.class) {
            return true;
        }
    }
    false
}

/// Collect every virtual base reachable from `class_id` (via any base
/// chain), deduplicated by `ClassId`, in the order Itanium uses:
/// post-order over the inheritance graph. For the common single-
/// diamond case (`D : D1, D2`, both virtually inheriting `A`) this
/// yields `[A]`.
pub(crate) fn collect_virtual_bases(
    ctx: &CxxTypeCtx,
    class_id: ClassId,
) -> Vec<ClassId> {
    let mut out = Vec::new();
    fn walk(
        ctx: &CxxTypeCtx,
        class_id: ClassId,
        out: &mut Vec<ClassId>,
    ) {
        for base in &ctx.class(class_id).bases {
            walk(ctx, base.class, out);
            if base.virtual_ && !out.contains(&base.class) {
                out.push(base.class);
            }
        }
    }
    walk(ctx, class_id, &mut out);
    out
}

fn pick_primary_base(ctx: &CxxTypeCtx, class: &ClassDef) -> Option<ClassId> {
    if !class.is_polymorphic {
        return None;
    }
    // Single-inheritance subset: at most one base. Promote it to primary
    // iff it's itself polymorphic (so the vptr can be shared).
    for base in &class.bases {
        if ctx.class(base.class).is_polymorphic {
            return Some(base.class);
        }
    }
    None
}

fn allocate_vptr(ctx: &CxxTypeCtx, state: &mut LayoutState) {
    let ps = pointer_size(ctx);
    state.size = ps;
    state.dsize = ps;
    state.align = state.align.max(ps);
    state.has_vptr = true;
}

fn place_base(
    ctx: &CxxTypeCtx,
    state: &mut LayoutState,
    base_id: ClassId,
    is_primary: bool,
) -> Result<(), LayoutError> {
    let base_layout = compute_layout(ctx, base_id)?;

    // When placing a base, we treat it as occupying only its non-virtual
    // portion (`nv_size_bytes`). Its own virtual bases are NOT carried
    // into the derived class's non-virtual layout — they'll be laid out
    // fresh (and deduplicated) at the most-derived class's end.
    let offset: u64 = if is_primary {
        state.size = state.size.max(base_layout.nv_size_bytes);
        state.dsize = state.dsize.max(base_layout.nv_size_bytes);
        state.align = state.align.max(base_layout.align_bytes);
        state.has_vptr = state.has_vptr || base_layout.has_vptr;
        state
            .empty_subobjects
            .extend(base_layout.empty_subobjects.iter().cloned());
        0
    } else if base_layout.nv_size_bytes == 0 {
        // EBO: pick the lowest offset that doesn't collide with another
        // empty subobject of the same type.
        let off = find_empty_base_offset(state, base_id, &base_layout);
        state.empty_subobjects.push((base_id, off));
        state.align = state.align.max(base_layout.align_bytes);
        off
    } else {
        // Non-empty non-primary base at aligned dsize, counting only
        // its non-virtual portion.
        let off = align_up(state.dsize, base_layout.nv_align_bytes);
        state.size = state.size.max(off + base_layout.nv_size_bytes);
        state.dsize = off + base_layout.nv_size_bytes;
        state.align = state.align.max(base_layout.align_bytes);
        off
    };

    state.base_offsets.push((base_id, offset));
    Ok(())
}

fn find_empty_base_offset(
    state: &LayoutState,
    base_id: ClassId,
    base_layout: &RecordLayout,
) -> u64 {
    let step = base_layout.align_bytes.max(1);
    let mut candidate = 0;
    loop {
        let conflict = state
            .empty_subobjects
            .iter()
            .any(|(id, off)| *id == base_id && *off == candidate);
        if !conflict {
            return candidate;
        }
        candidate += step;
    }
}

fn place_field(
    ctx: &CxxTypeCtx,
    state: &mut LayoutState,
    field: &FieldDef,
    class_id: ClassId,
    idx: usize,
) -> Result<(), LayoutError> {
    // M21.b: bitfield fast path. The sidecar `bitfield_width`
    // returns Some(w) for bitfield members; everything else
    // flows through the original byte-aligned placement.
    if let Some(width) = ctx.bitfield_width(class_id, idx) {
        return place_bitfield(ctx, state, field, class_id, idx, width);
    }
    // Non-bitfield: close any open AU first so the next byte-
    // aligned field doesn't overlap with the AU's tail.
    state.bitfield_au = None;

    let (size, align) = type_size_align(ctx, field.ty).map_err(|_| {
        LayoutError::UnsizedField {
            class: class_id,
            field: FieldId(idx as u32),
        }
    })?;
    // v1.13.1: `__attribute__((packed))` forces field alignment to
    // 1 (no inter-field padding). A per-field `alignas(N)` still
    // wins — GCC honors explicit field alignment even inside a
    // packed struct. Without packing, the field takes the larger
    // of its natural alignment and any explicit `alignas`.
    let packed = ctx.is_packed(class_id);
    let align = if packed {
        field.explicit_align.unwrap_or(1).max(1)
    } else {
        field.explicit_align.unwrap_or(align).max(align).max(1)
    };

    let offset = align_up(state.dsize, align);
    let end = offset + size;

    state.size = state.size.max(end);
    state.dsize = end;
    state.align = state.align.max(align);
    state.field_offsets.push(offset);
    state.field_bit_offsets.push(0);
    state.field_bit_widths.push(0);
    Ok(())
}

/// M21.b: Itanium bit-packing for one bitfield field.
///
/// Simplified ruleset (covers the cases v0 callers see):
///
/// - **AU = container type.** Each bitfield's storage unit is
///   the size/alignment of the field's declared C++ type
///   (`unsigned int a:4` uses a 4-byte AU). We recover the
///   container size from `type_size_align(ctx, field.ty)`.
/// - **Pack into the open AU when possible.** If the previous
///   field was a bitfield AND its container size matches AND
///   the new bits fit in the AU's remaining space, append
///   directly.
/// - **Otherwise open a new AU.** Align to the container's
///   alignment, place the new AU there, reset `used_bits`.
/// - **Width 0** is a "force boundary" marker — close any open
///   AU and don't allocate any bits. The next bitfield starts
///   a fresh AU.
/// - **Oversize bitfields** (`width > AU_size_in_bits`) aren't
///   handled in v0 — they require a wider container. The
///   layout engine treats them as the AU-sized portion only,
///   which is wrong but visible (the user sees one bit-width
///   diagnostic in the doc-comment hint we add later).
fn place_bitfield(
    ctx: &CxxTypeCtx,
    state: &mut LayoutState,
    field: &FieldDef,
    class_id: ClassId,
    idx: usize,
    width: u64,
) -> Result<(), LayoutError> {
    let (size, align) = type_size_align(ctx, field.ty).map_err(|_| {
        LayoutError::UnsizedField {
            class: class_id,
            field: FieldId(idx as u32),
        }
    })?;
    let align = field.explicit_align.unwrap_or(align).max(align).max(1);
    let au_bits = size * 8;

    // Width 0: force AU boundary. Close any open AU and
    // record a zero-width slot at the next aligned offset
    // (the slot itself doesn't reserve bits).
    if width == 0 {
        // Advance dsize past the open AU so the next field
        // starts on a fresh AU.
        if let Some(au) = state.bitfield_au.take() {
            let au_end = au.offset + au.size_bytes;
            state.dsize = state.dsize.max(au_end);
        }
        let offset = align_up(state.dsize, align);
        state.size = state.size.max(offset);
        state.align = state.align.max(align);
        state.field_offsets.push(offset);
        state.field_bit_offsets.push(0);
        state.field_bit_widths.push(0);
        return Ok(());
    }

    // Try to pack into the open AU. Same container size +
    // enough room left.
    if let Some(au) = state.bitfield_au {
        if au.size_bytes == size && au.used_bits + width <= au_bits {
            let bit_offset_in_byte = (au.used_bits % 8) as u8;
            let byte_within_au = au.used_bits / 8;
            let field_byte_offset = au.offset + byte_within_au;
            state.field_offsets.push(field_byte_offset);
            state.field_bit_offsets.push(bit_offset_in_byte);
            state.field_bit_widths.push(width);
            // Advance the AU.
            let new_used = au.used_bits + width;
            let updated = BitfieldAu {
                offset: au.offset,
                size_bytes: au.size_bytes,
                used_bits: new_used,
            };
            state.bitfield_au = if new_used >= au_bits {
                None
            } else {
                Some(updated)
            };
            // Reserve the entire AU in dsize / size, even if
            // not fully used yet. Subsequent non-bitfield
            // fields skip past the AU; subsequent bitfields
            // of a different container restart cleanly.
            let au_end = au.offset + au.size_bytes;
            state.size = state.size.max(au_end);
            state.dsize = state.dsize.max(au_end);
            state.align = state.align.max(align);
            return Ok(());
        }
    }

    // Open a new AU at the next aligned offset.
    let au_offset = align_up(state.dsize, align);
    state.field_offsets.push(au_offset);
    state.field_bit_offsets.push(0);
    state.field_bit_widths.push(width);
    let used_bits = width.min(au_bits); // clamp; oversize is v0 fall-back
    state.bitfield_au = if used_bits >= au_bits {
        None
    } else {
        Some(BitfieldAu {
            offset: au_offset,
            size_bytes: size,
            used_bits,
        })
    };
    let au_end = au_offset + size;
    state.size = state.size.max(au_end);
    state.dsize = state.dsize.max(au_end);
    state.align = state.align.max(align);
    Ok(())
}

fn place_union_field(
    ctx: &CxxTypeCtx,
    state: &mut LayoutState,
    field: &FieldDef,
    class_id: ClassId,
    idx: usize,
) -> Result<(), LayoutError> {
    let (size, align) = type_size_align(ctx, field.ty).map_err(|_| {
        LayoutError::UnsizedField {
            class: class_id,
            field: FieldId(idx as u32),
        }
    })?;
    let align = field.explicit_align.unwrap_or(align).max(align).max(1);
    state.size = state.size.max(size);
    state.align = state.align.max(align);
    state.field_offsets.push(0);
    state.field_bit_offsets.push(0);
    state.field_bit_widths.push(ctx.bitfield_width(class_id, idx).unwrap_or(0));
    Ok(())
}

fn type_size_align(ctx: &CxxTypeCtx, ty: TypeId) -> Result<(u64, u64), ()> {
    match ctx.type_of(ty) {
        CxxType::Void | CxxType::MemberPtr { .. } => Err(()),
        // M15 collapses `Ptr<Fn>` → `Fn` in `import_type`'s Pointer
        // arm, because Itanium treats function pointer + function
        // type as a single ABI unit at parameter/return position.
        // When the same `Fn` then surfaces as a *field* type
        // (e.g. `Fl_Callback* callback_;` in `Fl_Widget`), it
        // represents a function-pointer slot and is therefore
        // pointer-sized. Without this branch, every class with a
        // function-pointer field hits `LayoutError::UnsizedField`.
        CxxType::Fn(_) => {
            let ps = pointer_size(ctx);
            Ok((ps, ps))
        }
        CxxType::Bool => Ok((1, 1)),
        CxxType::Int { width, .. } => {
            let s = match width {
                IntWidth::I8 => 1,
                IntWidth::I16 => 2,
                IntWidth::I32 => 4,
                IntWidth::I64 => 8,
                IntWidth::I128 => 16,
            };
            Ok((s, s))
        }
        CxxType::Float { kind } => {
            let s = match kind {
                FloatKind::F32 => 4,
                FloatKind::F64 => 8,
                FloatKind::LongDouble => match ctx.target().long_double {
                    // Itanium/x86_64-linux uses 80-bit x87 padded to 16 bytes;
                    // Darwin and aarch64 vary.
                    LongDoubleKind::F64 => 8,
                    LongDoubleKind::F80 => 16,
                    LongDoubleKind::F128 => 16,
                },
            };
            Ok((s, s))
        }
        CxxType::Ptr { .. } | CxxType::Ref { .. } => {
            let ps = pointer_size(ctx);
            Ok((ps, ps))
        }
        CxxType::Array { elem, len } => {
            let (es, ea) = type_size_align(ctx, *elem)?;
            Ok((es.saturating_mul(*len), ea))
        }
        CxxType::Record(id) => {
            let layout =
                compute_layout(ctx, *id).map_err(|_| ())?;
            Ok((layout.size_bytes, layout.align_bytes))
        }
        CxxType::Enum { underlying, .. } => type_size_align(ctx, *underlying),
    }
}

// -------- Classification ------------------------------------------------

fn is_pod_for_layout(ctx: &CxxTypeCtx, class_id: ClassId) -> bool {
    let class = ctx.class(class_id);
    if class.is_polymorphic {
        return false;
    }
    // Any user-declared special member (ctor, dtor, copy/move) makes the
    // class non-trivially-copyable and hence non-POD-for-layout.
    if class.methods.iter().any(|m| m.special.is_some()) {
        return false;
    }
    // A virtual base forces a non-trivial default ctor (the vbase vptr
    // must be set up), so the class is never POD-for-layout.
    if class.bases.iter().any(|b| b.virtual_) {
        return false;
    }
    for base in &class.bases {
        if !is_pod_for_layout(ctx, base.class) {
            return false;
        }
    }
    for field in &class.fields {
        match ctx.type_of(field.ty) {
            // A reference member makes the class's implicit default ctor
            // deleted, which disqualifies it from being trivial and
            // therefore from being POD-for-layout — Clang accordingly
            // treats such classes as having reusable tail padding.
            CxxType::Ref { .. } => return false,
            // A record field whose type is itself non-POD taints us too.
            CxxType::Record(inner) => {
                if !is_pod_for_layout(ctx, *inner) {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

fn is_empty_class(ctx: &CxxTypeCtx, class_id: ClassId) -> bool {
    let class = ctx.class(class_id);
    if class.is_polymorphic {
        return false;
    }
    if !class.fields.is_empty() {
        return false;
    }
    for base in &class.bases {
        if !is_empty_class(ctx, base.class) {
            return false;
        }
    }
    true
}

// -------- Arithmetic helpers -------------------------------------------

fn align_up(val: u64, align: u64) -> u64 {
    if align <= 1 {
        return val;
    }
    (val + align - 1) / align * align
}

fn pointer_size(ctx: &CxxTypeCtx) -> u64 {
    (ctx.target().pointer_width_bits as u64) / 8
}

#[cfg(test)]
mod tests {
    use super::{align_up, HfaElem, HfaKind};
    use crate::ctx::CxxTypeCtx;
    use crate::target::Target;
    use crate::ty::{
        ClassDef, CxxType, FieldDef, FloatKind, Ident, IntWidth, NameSegment,
        NestedName, RecordKind,
    };

    #[test]
    fn align_up_basics() {
        assert_eq!(align_up(0, 4), 0);
        assert_eq!(align_up(1, 4), 4);
        assert_eq!(align_up(4, 4), 4);
        assert_eq!(align_up(5, 4), 8);
        assert_eq!(align_up(5, 16), 16);
        assert_eq!(align_up(16, 16), 16);
        assert_eq!(align_up(17, 16), 32);
        // align 0 / 1: no-op.
        assert_eq!(align_up(5, 0), 5);
        assert_eq!(align_up(5, 1), 5);
    }

    fn empty_class(name: &str, alignas: Option<u64>) -> ClassDef {
        ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident(name.into()))]),
            bases: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: alignas,
        }
    }

    #[test]
    fn empty_class_has_size_one_align_one() {
        // Baseline Itanium rule: empty class has size 1, align 1
        // so distinct instances have distinct addresses.
        let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
        let id = ctx.define_class(empty_class("Empty", None));
        let layout = ctx.layout(id).unwrap();
        assert_eq!(layout.size_bytes, 1);
        assert_eq!(layout.align_bytes, 1);
    }

    #[test]
    fn empty_aligned_class_size_is_multiple_of_align() {
        // Regression test for the ordering bug where the empty-
        // class "bump to 1" happened after align_up, producing
        // size=1 align=N. Itanium requires size % align == 0.
        // Verified against Clang:
        //   struct alignas(4) E {};   → size 4 align 4
        //   struct alignas(16) E {};  → size 16 align 16
        for alignas in [2, 4, 8, 16, 32] {
            let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
            let id =
                ctx.define_class(empty_class("E", Some(alignas)));
            let layout = ctx.layout(id).unwrap();
            assert_eq!(
                layout.size_bytes, alignas,
                "alignas({alignas}) empty: expected size {alignas}, got {}",
                layout.size_bytes
            );
            assert_eq!(
                layout.align_bytes, alignas,
                "alignas({alignas}) empty: expected align {alignas}, got {}",
                layout.align_bytes
            );
            assert_eq!(
                layout.size_bytes % layout.align_bytes,
                0,
                "alignas({alignas}) empty: size not a multiple of align",
            );
        }
    }

    // -------- v1.09.2 patch 19c: HFA detection ----------------------

    /// Build a struct with the given field types and return its
    /// `hfa_kind`. Helper for the HFA tests below.
    fn hfa_of(field_types: &[CxxType]) -> Option<HfaKind> {
        let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
        let fields: Vec<FieldDef> = field_types
            .iter()
            .enumerate()
            .map(|(i, t)| FieldDef {
                name: Ident(format!("f{i}")),
                ty: ctx.intern_type(t.clone()),
                explicit_align: None,
            })
            .collect();
        let id = ctx.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("H".into()))]),
            bases: Vec::new(),
            fields,
            methods: Vec::new(),
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        ctx.layout(id).unwrap().hfa_kind
    }

    fn f32_ty() -> CxxType {
        CxxType::Float { kind: FloatKind::F32 }
    }
    fn f64_ty() -> CxxType {
        CxxType::Float { kind: FloatKind::F64 }
    }
    fn int_ty() -> CxxType {
        CxxType::Int {
            signed: true,
            width: IntWidth::I32,
        }
    }

    #[test]
    fn hfa_single_f32() {
        // `struct { float; }` — count 1, F32.
        assert_eq!(
            hfa_of(&[f32_ty()]),
            Some(HfaKind { elem: HfaElem::F32, count: 1 })
        );
    }

    #[test]
    fn hfa_three_f64() {
        // `struct { double; double; double; }` — count 3, F64.
        assert_eq!(
            hfa_of(&[f64_ty(), f64_ty(), f64_ty()]),
            Some(HfaKind { elem: HfaElem::F64, count: 3 })
        );
    }

    #[test]
    fn hfa_four_f32_is_max() {
        // `struct { float; float; float; float; }` — count 4, F32.
        assert_eq!(
            hfa_of(&[f32_ty(), f32_ty(), f32_ty(), f32_ty()]),
            Some(HfaKind { elem: HfaElem::F32, count: 4 })
        );
    }

    #[test]
    fn hfa_five_f32_disqualified() {
        // 5 floats exceed AAPCS64 §B.2.5 limit of 1..=4 — not HFA.
        assert_eq!(
            hfa_of(&[f32_ty(), f32_ty(), f32_ty(), f32_ty(), f32_ty()]),
            None
        );
    }

    #[test]
    fn hfa_mixed_types_disqualified() {
        // `struct { float; double; }` — not homogeneous.
        assert_eq!(hfa_of(&[f32_ty(), f64_ty()]), None);
    }

    #[test]
    fn hfa_int_field_disqualifies() {
        // Any non-float leaf disqualifies — even one int.
        assert_eq!(hfa_of(&[f32_ty(), int_ty()]), None);
    }

    #[test]
    fn hfa_empty_struct_is_not_hfa() {
        // Empty count → no HFA. The "1..=4" rule excludes 0.
        assert_eq!(hfa_of(&[]), None);
    }

    #[test]
    fn hfa_array_flattens_into_leaf_count() {
        // `struct { float[3]; }` — array of 3 floats becomes a 3-leaf
        // count, qualifying as an HFA.
        let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
        let f = ctx.intern_type(f32_ty());
        let arr = ctx.intern_type(CxxType::Array { elem: f, len: 3 });
        let id = ctx.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Pos".into()))]),
            bases: Vec::new(),
            fields: vec![FieldDef {
                name: Ident("xs".into()),
                ty: arr,
                explicit_align: None,
            }],
            methods: Vec::new(),
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        assert_eq!(
            ctx.layout(id).unwrap().hfa_kind,
            Some(HfaKind { elem: HfaElem::F32, count: 3 })
        );
    }

    #[test]
    fn hfa_array_of_5_floats_disqualifies() {
        // Array element count > 4 also exceeds the limit.
        let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
        let f = ctx.intern_type(f32_ty());
        let arr = ctx.intern_type(CxxType::Array { elem: f, len: 5 });
        let id = ctx.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Five".into()))]),
            bases: Vec::new(),
            fields: vec![FieldDef {
                name: Ident("xs".into()),
                ty: arr,
                explicit_align: None,
            }],
            methods: Vec::new(),
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        assert_eq!(ctx.layout(id).unwrap().hfa_kind, None);
    }

    #[test]
    fn hfa_polymorphic_disqualified() {
        // Any vptr disqualifies — the inherited base subobject
        // breaks the homogeneous-leaves invariant.
        let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
        let f = ctx.intern_type(f32_ty());
        let id = ctx.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Poly".into()))]),
            bases: Vec::new(),
            fields: vec![FieldDef {
                name: Ident("x".into()),
                ty: f,
                explicit_align: None,
            }],
            methods: Vec::new(),
            kind: RecordKind::Struct,
            is_polymorphic: true,
            is_final: false,
            source_alignment: None,
        });
        assert_eq!(ctx.layout(id).unwrap().hfa_kind, None);
    }

    #[test]
    fn hfa_nested_struct_flattens() {
        // `struct Outer { Inner; float; }` where `Inner { float; float; }`
        // → 3 floats total, qualifies as F32 HFA count 3.
        let mut ctx = CxxTypeCtx::new(Target::aarch64_apple_darwin());
        let f = ctx.intern_type(f32_ty());
        let inner = ctx.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Inner".into()))]),
            bases: Vec::new(),
            fields: vec![
                FieldDef { name: Ident("a".into()), ty: f, explicit_align: None },
                FieldDef { name: Ident("b".into()), ty: f, explicit_align: None },
            ],
            methods: Vec::new(),
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        let inner_ty = ctx.intern_type(CxxType::Record(inner));
        let outer = ctx.define_class(ClassDef {
            name: NestedName(vec![NameSegment::Class(Ident("Outer".into()))]),
            bases: Vec::new(),
            fields: vec![
                FieldDef { name: Ident("i".into()), ty: inner_ty, explicit_align: None },
                FieldDef { name: Ident("c".into()), ty: f, explicit_align: None },
            ],
            methods: Vec::new(),
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        });
        assert_eq!(
            ctx.layout(outer).unwrap().hfa_kind,
            Some(HfaKind { elem: HfaElem::F32, count: 3 })
        );
    }
}
