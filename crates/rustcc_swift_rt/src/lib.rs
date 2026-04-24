//! Runtime helpers for rustcc's Swift Phase 2b interop.
//!
//! Non-trivial Swift value types (structs with `deinit`, classes,
//! types with non-POD fields) can't be copied or dropped via a
//! plain Rust `Drop` / `Clone` — they need to dispatch through
//! Swift's runtime **value-witness table** (VWT). This crate
//! provides the minimum helpers a `#[repr(swift)]` type's
//! `Drop` / `Clone` impls need to call.
//!
//! # Swift runtime primitives
//!
//! For every Swift value type `T`, `swiftc` emits a **type
//! metadata accessor** with the symbol `$s<module><Type>VMa`.
//! Calling it returns a `MetadataResponse` whose `_0` field is
//! a pointer to the type metadata. At offset `-1` (that is, one
//! pointer before the metadata), Swift stores a pointer to the
//! VWT.
//!
//! The VWT is a struct of function pointers:
//!
//! ```ignore
//! struct ValueWitnessTable {
//!     destroy:                    fn(*mut u8, *mut Metadata),
//!     initialize_with_copy:       fn(*mut u8, *const u8, *mut Metadata) -> *mut u8,
//!     initialize_with_take:       fn(*mut u8, *mut u8,   *mut Metadata) -> *mut u8,
//!     assign_with_copy:           fn(*mut u8, *const u8, *mut Metadata) -> *mut u8,
//!     assign_with_take:           fn(*mut u8, *mut u8,   *mut Metadata) -> *mut u8,
//!     get_enum_tag_single:        fn(*const u8, u32, *mut Metadata) -> u32,
//!     store_enum_tag_single:      fn(*mut u8, u32, u32, *mut Metadata),
//!     size:                       usize,
//!     stride:                     usize,
//!     flags:                      u32,
//!     extra_inhabitant_count:     u32,
//! }
//! ```
//!
//! `drop_swift_value` and `clone_swift_value` below load the
//! VWT and dispatch through the relevant field.
//!
//! # Usage pattern (Phase 2b.1 manual form — outlined destroy)
//!
//! Swift emits an **outlined destroy** symbol for every
//! non-trivial value type, named
//! `$s<modlen><module><typelen><type>VWOh`. It knows the type's
//! layout statically and releases any class/tuple fields without
//! going through the metadata / VWT dance. Calling it from Rust
//! is the simplest way to free a Swift non-trivial value:
//!
//! ```ignore
//! #[repr(swift)]
//! #[rustc_swift_type = "MyLib.NT"]
//! pub struct NT {
//!     pub x: u64,
//!     pub inner: *mut core::ffi::c_void,   // Swift class ref
//! }
//!
//! unsafe extern "Swift" {
//!     #[link_name = "$s5MyLib2NTVWOh"]
//!     fn nt_outlined_destroy(obj: *mut NT) -> *mut NT;
//! }
//!
//! impl Drop for NT {
//!     fn drop(&mut self) {
//!         unsafe { nt_outlined_destroy(self); }
//!     }
//! }
//! ```
//!
//! Phase 2b.2 will eliminate this boilerplate via compiler-side
//! auto-synthesis: the `#[repr(swift)]` attribute alone will
//! imply the Drop + Clone impls, and the compiler will
//! auto-declare the outlined-destroy / metadata-accessor
//! externs from the `#[rustc_swift_type]` binding.
//!
//! # VWT-dispatch path (generic — works for any non-trivial type)
//!
//! For types where the outlined helpers aren't available (e.g.
//! across Swift versions, or for dynamic metadata), the generic
//! path is:
//!
//! 1. Call `$s...VMa` to get `MetadataResponse`
//! 2. Load the VWT pointer at `metadata - sizeof(*VWT)`
//! 3. Call through `vwt->destroy` / `vwt->initializeWithCopy`
//!
//! Helpers [`drop_swift_value`] and [`clone_swift_value`]
//! below implement this path. Previously these crashed on
//! types with class-reference fields because the
//! `ValueWitnessTable` struct had `destroy` at the wrong slot
//! (offset 0 instead of offset 8, where Swift places
//! `initializeBufferWithCopyOfBuffer` first). With the slot
//! order corrected, both helpers dispatch into the right
//! witnesses.

#![no_std]

/// Response shape of a Swift type metadata accessor
/// (`$s<module><Type>VMa`). `_0` is the metadata pointer; `_1`
/// is the completion state (0 = complete). The layout matches
/// Swift's `MetadataResponse` struct.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct MetadataResponse {
    pub metadata: *mut Metadata,
    pub state: usize,
}

/// Opaque Swift type metadata. The only field users touch is
/// via the VWT stored one pointer before the metadata address.
#[repr(C)]
pub struct Metadata {
    _private: [u8; 0],
}

/// Swift's value-witness table layout. Field order matches
/// Swift's `ValueWitness.def`: `initializeBufferWithCopyOfBuffer`
/// is slot 0, `destroy` is slot 1, and so on. An earlier version
/// of this struct had `destroy` at slot 0 — which meant every
/// call to `(*vwt).destroy(value, metadata)` was actually
/// dispatching into `initializeBufferWithCopyOfBuffer(dst, src,
/// metadata)` with `dst=value` and `src=metadata`. For trivial
/// types the init-buffer witness is a plain memcpy and the
/// mistake went silent; for types with class fields it dereferences
/// `src` as a heap object and tries to retain it, crashing deep in
/// `swift::RefCounts::incrementSlow`. Per-slot layout now matches
/// the IR emitted by `swiftc` (see e.g. `$s...VWV` constants).
///
/// VWT function pointers use Swift's calling convention (swiftcc).
/// Going through `extern "C"` works for simple signatures on
/// SysV AMD64 but leaks on some callees that rely on swiftcc's
/// context register (r14) and error register (r12) being set up.
/// Require `extern "Swift"`, which is fork-specific.
#[repr(C)]
pub struct ValueWitnessTable {
    pub initialize_buffer_with_copy_of_buffer:
        unsafe extern "Swift" fn(dst: *mut u8, src: *mut u8, metadata: *mut Metadata) -> *mut u8,
    pub destroy:
        unsafe extern "Swift" fn(ptr: *mut u8, metadata: *mut Metadata),
    pub initialize_with_copy:
        unsafe extern "Swift" fn(dst: *mut u8, src: *const u8, metadata: *mut Metadata) -> *mut u8,
    pub assign_with_copy:
        unsafe extern "Swift" fn(dst: *mut u8, src: *const u8, metadata: *mut Metadata) -> *mut u8,
    pub initialize_with_take:
        unsafe extern "Swift" fn(dst: *mut u8, src: *mut u8, metadata: *mut Metadata) -> *mut u8,
    pub assign_with_take:
        unsafe extern "Swift" fn(dst: *mut u8, src: *mut u8, metadata: *mut Metadata) -> *mut u8,
}

/// Load the VWT for a Swift type given its metadata. The VWT
/// pointer lives one machine word *before* the metadata pointer,
/// per the Swift runtime ABI.
///
/// # Safety
///
/// `metadata` must be a valid Swift type metadata pointer
/// obtained from a call to the type's `$s...Ma` accessor.
#[inline]
pub unsafe fn vwt(metadata: *mut Metadata) -> *const ValueWitnessTable {
    // metadata is a pointer; cast to `*mut *const VWT` and
    // step back one element. The VWT pointer sits at
    // metadata[-1].
    let slot = (metadata as *mut *const ValueWitnessTable).wrapping_sub(1);
    unsafe { *slot }
}

/// Invoke the Swift destroy witness on `value`. Equivalent to
/// running `deinit` on the Swift side.
///
/// # Safety
///
/// `value` must point to a valid, initialized Swift value of
/// the type whose metadata accessor is `metadata_accessor`.
/// After this call the storage at `value` is uninitialized —
/// do not access it further without reinitializing.
#[inline]
pub unsafe fn drop_swift_value<T>(
    value: *mut T,
    metadata_accessor: unsafe extern "C" fn(usize) -> MetadataResponse,
) {
    unsafe {
        let md = metadata_accessor(0);
        let vwt = vwt(md.metadata);
        ((*vwt).destroy)(value as *mut u8, md.metadata);
    }
}

/// Copy-initialize `dst` from `src` using the Swift copy witness.
/// Equivalent to Swift's implicit copy when passing a value by
/// copy.
///
/// # Safety
///
/// `src` must point to a valid Swift value. `dst` must point to
/// uninitialized storage of the same type with correct alignment
/// and size (`vwt.size` bytes).
#[inline]
pub unsafe fn clone_swift_value<T>(
    dst: *mut T,
    src: *const T,
    metadata_accessor: unsafe extern "C" fn(usize) -> MetadataResponse,
) {
    unsafe {
        let md = metadata_accessor(0);
        let vwt = vwt(md.metadata);
        ((*vwt).initialize_with_copy)(dst as *mut u8, src as *const u8, md.metadata);
    }
}

// ==========================================================
// P09.21 — Swift class (reference type) ARC helpers.
//
// Swift classes are heap-allocated with retain/release reference
// counting. A Rust-side `#[rustc_swift_type = "MyLib.X:class"]`
// struct holds the class ref as a raw pointer; its Drop impl
// calls `swift_release`, and any Clone calls `swift_retain`
// before copying the pointer.
//
// On Apple platforms `libswiftCore.dylib` provides these
// symbols. Other Swift-runtime-bearing targets export them
// under the same name. We declare them extern "C" because
// swiftc exposes them with C-linkage signatures — the Swift
// stdlib's own retain/release interface is already C-ABI to
// simplify cross-language calls.
// ==========================================================

unsafe extern "C" {
    /// Increment the reference count of a Swift class instance.
    /// Returns the same pointer (convenience for chained calls).
    pub fn swift_retain(obj: *mut core::ffi::c_void) -> *mut core::ffi::c_void;

    /// Decrement the reference count of a Swift class instance.
    /// When the count hits zero, Swift runs the deinit and frees
    /// the instance.
    pub fn swift_release(obj: *mut core::ffi::c_void);
}

/// Convenience: clone a Swift class handle by incrementing its
/// reference count. The returned pointer is the same bit-pattern
/// but with one added reference the caller now owns.
///
/// # Safety
///
/// `obj` must be a valid Swift class instance pointer.
#[inline]
pub unsafe fn retain_swift_class(obj: *mut core::ffi::c_void) -> *mut core::ffi::c_void {
    unsafe { swift_retain(obj) }
}

/// Convenience: drop a Swift class reference by decrementing
/// its count. May trigger deinit + free if this was the last
/// reference.
///
/// # Safety
///
/// `obj` must be a valid Swift class instance pointer.
#[inline]
pub unsafe fn release_swift_class(obj: *mut core::ffi::c_void) {
    unsafe { swift_release(obj) };
}

// ============================================================
// P09.48 / 1.02 throws — Swift `throws` error handle.
// ============================================================
//
// `SwiftError` is an opaque owned handle to a Swift `Error`
// instance. It behaves like a `Box<dyn Error>` in Rust: the
// inner pointer is `swift_retain`'d on construction and
// `swift_release`'d on drop. Dropping a null-pointer
// `SwiftError` is a no-op (the Swift convention for "no error").
//
// A throwing Swift function, when declared in `extern "Swift"`,
// takes a trailing `err: *mut *mut SwiftError` parameter. The
// caller seeds `*err = null_mut()` before the call; the callee
// writes a retained error pointer there on throw and leaves it
// null on success. The `#[rustc_swift_throws]` attribute on
// the foreign-fn decl tells rustcc to attach LLVM's
// `swifterror` to that parameter, which pins it to the Swift-
// ABI error register (r12 on x86_64, x21 on aarch64 Darwin).
//
// A future helper macro will generate the Result-wrapping
// thunk automatically; for now users write it themselves —
// see the example in the doc-comment below.

/// Owned handle to a Swift `Error` instance. Holds one retain
/// on the underlying reference.
///
/// # Example
///
/// ```ignore
/// use rustcc_swift_rt::SwiftError;
///
/// extern "Swift" {
///     #[rustc_swift_throws]
///     #[link_name = "$s5MyLib8do_thingSiSiAA5InputVtKF"]
///     fn do_thing_raw(
///         input: Input,
///         err: *mut *mut core::ffi::c_void,
///     ) -> i64;
/// }
///
/// fn do_thing(input: Input) -> Result<i64, SwiftError> {
///     let mut err: *mut core::ffi::c_void = core::ptr::null_mut();
///     let ret = unsafe { do_thing_raw(input, &mut err) };
///     if err.is_null() {
///         Ok(ret)
///     } else {
///         // SAFETY: the Swift ABI guarantees `err` is either
///         // null or a retained Error; we take ownership here.
///         Err(unsafe { SwiftError::from_retained(err) })
///     }
/// }
/// ```
#[repr(transparent)]
pub struct SwiftError {
    ptr: *mut core::ffi::c_void,
}

impl SwiftError {
    /// Take ownership of a retained Swift Error pointer produced
    /// by a throwing extern "Swift" call. The caller must not
    /// release the pointer separately — `SwiftError`'s `Drop`
    /// will do it.
    ///
    /// # Safety
    ///
    /// `ptr` must be either null or a valid, already-retained
    /// Swift `Error` class instance pointer.
    #[inline]
    pub unsafe fn from_retained(ptr: *mut core::ffi::c_void) -> Self {
        SwiftError { ptr }
    }

    /// Extract the underlying raw pointer without releasing it.
    /// Useful when handing the error back to Swift (e.g. forwarding
    /// from a Rust throwing shim). After this call the caller is
    /// responsible for the retain.
    #[inline]
    pub fn into_raw(self) -> *mut core::ffi::c_void {
        let p = self.ptr;
        core::mem::forget(self);
        p
    }

    /// Borrow the raw pointer without transferring ownership.
    #[inline]
    pub fn as_ptr(&self) -> *mut core::ffi::c_void {
        self.ptr
    }

    /// True if this handle is the Swift "no error" null pointer.
    /// The throwing-call wrapper usually short-circuits before
    /// constructing a `SwiftError` with null; this method is
    /// provided for defensive code paths.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }
}

impl Drop for SwiftError {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { swift_release(self.ptr) };
        }
    }
}

// `SwiftError` is a pointer to a refcounted object. Swift's
// ARC is thread-safe (atomic retain/release), so the handle
// itself is safe to `Send` and `Sync` as long as the inner
// error type is. We default to `Send` only — Swift errors can
// carry interior mutability, so `Sync` is opt-in.
unsafe impl Send for SwiftError {}
