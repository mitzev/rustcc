// Demo + regression test: `#[swift_value]` on a Swift **value type**
// (P09.17 + P09.18 + P09.46), the value-witness-table path.
//
// The existing swift_value demos (`swift_nonpod`, `swift_value_attr`)
// cover the *class* case, where Drop/Clone are `swift_release` /
// `swift_retain` on a pointer. This demo covers the other half: a
// `#[repr(swift)]` **value** type, whose Drop and Clone are routed
// through the Swift runtime's value-witness table (VWT).
//
// The built-in `#[swift_value]` attribute (with `#[swift_type]`)
// synthesizes, for a value type:
//   * an `extern "C"` metadata-accessor declaration named with the
//     Swift mangling `$s<mod><Type>VMa` (here `$s4Demo7CounterVMa`);
//   * `impl Drop`  → `rustcc_swift_rt::drop_swift_value(self, acc)`,
//     which loads the VWT (`metadata[-1]`) and calls its `destroy`
//     witness;
//   * `impl Clone` → `rustcc_swift_rt::clone_swift_value(dst, src,
//     acc)`, which calls the VWT's `initialize_with_copy` witness.
//
// In a real build those witnesses live in the `swiftc`-emitted VWT.
// To keep the demo self-contained (no Swift toolchain), we *provide*
// the metadata accessor and a hand-built VWT whose witnesses record
// that they were invoked. That lets us assert the compiler-synthesized
// Drop/Clone actually dispatch through the Swift ABI surface.

#![feature(rustc_attrs)]

use core::sync::atomic::{AtomicUsize, Ordering};

use rustcc_swift_rt::{Metadata, MetadataResponse, ValueWitnessTable};

// A Swift value type. `#[swift_value]` synthesizes the metadata extern,
// `impl Drop`, and `impl Clone` (all routed through the VWT below).
#[swift_value]
#[swift_type = "Demo.Counter"]
pub struct Counter {
    value: i64,
}

// ---- Stand-in for the swiftc-emitted runtime symbols. ----------------

static DESTROY_CALLS: AtomicUsize = AtomicUsize::new(0);
static COPY_CALLS: AtomicUsize = AtomicUsize::new(0);

// `destroy` witness: a `Counter` is trivially destructible, so this is
// a no-op beyond recording the call.
unsafe extern "Swift" fn vw_destroy(_ptr: *mut u8, _md: *mut Metadata) {
    DESTROY_CALLS.fetch_add(1, Ordering::SeqCst);
}

// `initialize_with_copy` witness: copy the bytes of a `Counter` into
// the (uninitialized) destination and return it.
unsafe extern "Swift" fn vw_init_with_copy(
    dst: *mut u8,
    src: *const u8,
    _md: *mut Metadata,
) -> *mut u8 {
    COPY_CALLS.fetch_add(1, Ordering::SeqCst);
    unsafe { core::ptr::copy_nonoverlapping(src, dst, core::mem::size_of::<Counter>()) };
    dst
}

// The remaining witnesses are never reached by the synthesized
// Drop/Clone, but the VWT must be fully populated. Give each the right
// signature and a trivial copy/no-op body.
unsafe extern "Swift" fn vw_init_buf(
    dst: *mut u8,
    src: *mut u8,
    _md: *mut Metadata,
) -> *mut u8 {
    unsafe { core::ptr::copy_nonoverlapping(src, dst, core::mem::size_of::<Counter>()) };
    dst
}
unsafe extern "Swift" fn vw_copy(
    dst: *mut u8,
    src: *const u8,
    _md: *mut Metadata,
) -> *mut u8 {
    unsafe { core::ptr::copy_nonoverlapping(src, dst, core::mem::size_of::<Counter>()) };
    dst
}
unsafe extern "Swift" fn vw_take(
    dst: *mut u8,
    src: *mut u8,
    _md: *mut Metadata,
) -> *mut u8 {
    unsafe { core::ptr::copy_nonoverlapping(src, dst, core::mem::size_of::<Counter>()) };
    dst
}

// Function pointers are `Sync`, so the VWT itself can be a `static`.
static VWT: ValueWitnessTable = ValueWitnessTable {
    initialize_buffer_with_copy_of_buffer: vw_init_buf,
    destroy: vw_destroy,
    initialize_with_copy: vw_init_with_copy,
    assign_with_copy: vw_copy,
    initialize_with_take: vw_take,
    assign_with_take: vw_take,
};

// The metadata accessor the compiler emits a `#[link_name]` extern for.
// Per the Swift runtime ABI the VWT pointer sits one machine word
// *before* the metadata pointer, so we hand back a two-word block
// `[ &VWT, _ ]` and point `metadata` at the second word — making
// `metadata[-1]` resolve to `&VWT` (exactly what `rustcc_swift_rt::vwt`
// reads). The block is intentionally leaked; a metadata symbol lives
// for the whole process in a real Swift program too.
#[export_name = "$s4Demo7CounterVMa"]
extern "C" fn counter_metadata(_flags: usize) -> MetadataResponse {
    let block: &'static mut [*const ValueWitnessTable; 2] =
        Box::leak(Box::new([&VWT as *const ValueWitnessTable, core::ptr::null()]));
    let metadata = unsafe { block.as_mut_ptr().add(1) } as *mut Metadata;
    MetadataResponse { metadata, state: 0 }
}

fn main() {
    let a = Counter { value: 41 };

    // Clone routes through the VWT's `initialize_with_copy` witness.
    let b = a.clone();
    assert_eq!(b.value, 41, "value copied through VWT");
    assert_eq!(COPY_CALLS.load(Ordering::SeqCst), 1, "one copy witness call");

    // Both values drop → two `destroy` witness calls.
    drop(b);
    drop(a);
    assert_eq!(
        DESTROY_CALLS.load(Ordering::SeqCst),
        2,
        "two destroy witness calls",
    );

    println!("ok: swift_value value-type Drop/Clone routed through VWT");
}
