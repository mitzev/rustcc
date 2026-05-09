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
//! Out of scope for v1: virtual bases, multiple inheritance, bit-fields,
//! `__attribute__((packed))`. These surface as `LayoutError` today and
//! are grown in later milestones.

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
}

impl CxxTypeCtx {
    pub fn layout(&self, class_id: ClassId) -> Result<RecordLayout, LayoutError> {
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
    })
}

/// Does `class_id` (or any class reachable through non-virtual bases)
/// declare a virtual base? Such a class needs a vptr at offset 0 to
/// store vbase offsets, regardless of whether it declares virtual
/// methods directly.
fn has_virtual_base_chain(ctx: &CxxTypeCtx, class_id: ClassId) -> bool {
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
fn collect_virtual_bases(
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
    let align = field.explicit_align.unwrap_or(align).max(align).max(1);

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
        CxxType::Void | CxxType::Fn(_) | CxxType::MemberPtr { .. } => Err(()),
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
    use super::align_up;
    use crate::ctx::CxxTypeCtx;
    use crate::target::Target;
    use crate::ty::{
        ClassDef, Ident, NameSegment, NestedName, RecordKind,
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
}
