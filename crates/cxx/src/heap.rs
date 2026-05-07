//! C++-allocated heap value.
//!
//! See `docs/ownership_and_safety.md §3.x` (heap ownership for
//! imported C++ classes).
//!
//! Distinct from [`crate::CxxOwned`] / [`crate::CxxBox`] in one
//! important way: the storage was allocated by C++'s `new` (via a
//! shim emitted by `cxx_importer`), and freeing it requires C++'s
//! `delete`, not Rust's `Box::from_raw` / global allocator. Mixing
//! the two is undefined behavior, so [`CxxHeap`] keeps the C++
//! allocator's invariant by routing `Drop` through a class-specific
//! delete shim provided via the [`CxxDeletable`] trait.
//!
//! Per-class implementations of [`CxxDeletable`] are emitted by the
//! `cxx_importer::rust_bindings` `DirectExternCpp` backend, alongside
//! `pub fn new_boxed(...)` ctor wrappers that return `CxxHeap<Self>`.

use core::marker::PhantomData;
use core::ptr::NonNull;

/// Class-specific contract for deleting a heap-allocated C++ value.
///
/// Implemented per-class by the `cxx_importer` bindings emitter
/// pointing at a `__cxx_<class>_delete` shim that wraps C++ `delete`.
///
/// # Safety
/// Implementations must call C++ `delete` on the pointer (which
/// runs the destructor and frees the memory) — not just the
/// destructor, and not Rust's global allocator. Memory must have
/// been allocated by the matching `__cxx_<class>_new_heap_<i>`
/// shim or another caller-validated C++ `new`.
pub unsafe trait CxxDeletable {
    /// Free `p` via C++ `delete`. The pointer must be non-null and
    /// uniquely owned at the call site.
    ///
    /// # Safety
    /// See trait-level docs.
    unsafe fn cxx_delete(p: *mut Self);
}

/// Heap-owned, exclusively-owned C++ value. Drop runs the
/// class-specific `cxx_delete` shim (which ultimately resolves to
/// C++ `delete`).
///
/// Created by `cxx_importer`-emitted `pub fn new_boxed(...)`
/// wrappers; users typically don't construct it directly.
pub struct CxxHeap<T: ?Sized + CxxDeletable> {
    ptr: NonNull<T>,
    _phantom: PhantomData<T>,
}

impl<T: ?Sized + CxxDeletable> CxxHeap<T> {
    /// Take ownership of `ptr`, which must have been produced by a
    /// matching `__cxx_<class>_new_heap_<i>` shim (or another
    /// caller-validated C++ `new T(...)` invocation).
    ///
    /// # Safety
    /// * `ptr` must be non-null.
    /// * `ptr` must point to a fully-initialized `T` allocated via
    ///   C++ `new`.
    /// * Ownership must be unique — no other `CxxHeap`, `Box`, or
    ///   `CxxOwned` may reference `ptr`.
    pub unsafe fn from_raw(ptr: *mut T) -> Self {
        Self {
            ptr: unsafe { NonNull::new_unchecked(ptr) },
            _phantom: PhantomData,
        }
    }

    /// Borrow the underlying pointer. Does not transfer ownership.
    pub fn as_raw(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    /// Release ownership, returning the raw pointer. The caller
    /// becomes responsible for calling C++ `delete` (typically by
    /// re-wrapping in `CxxHeap::from_raw` or invoking the matching
    /// `__cxx_<class>_delete` shim directly).
    pub fn into_raw(self) -> *mut T {
        let p = self.ptr.as_ptr();
        core::mem::forget(self);
        p
    }
}

impl<T: ?Sized + CxxDeletable> Drop for CxxHeap<T> {
    fn drop(&mut self) {
        // Calls C++ `delete` via the class's `__cxx_<class>_delete`
        // shim. The shim runs the C++ dtor and frees the memory
        // with the C++ allocator that allocated it — keeping the
        // new/delete pairing the C++ ABI requires.
        unsafe { T::cxx_delete(self.ptr.as_ptr()); }
    }
}

impl<T: ?Sized + CxxDeletable> core::ops::Deref for CxxHeap<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `from_raw` requires a fully-initialized,
        // exclusively-owned `T` at `self.ptr`. The pointer is
        // non-null by construction.
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: ?Sized + CxxDeletable> core::ops::DerefMut for CxxHeap<T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: see `Deref::deref`. `&mut` is fine because
        // ownership is exclusive.
        unsafe { self.ptr.as_mut() }
    }
}

impl<T: CxxDeletable + core::fmt::Debug> core::fmt::Debug for CxxHeap<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("CxxHeap").field(&**self).finish()
    }
}

// `CxxHeap<T>` is `Send`/`Sync` exactly when `T` is — same rules
// as `Box<T>`. Users who want a different policy can opt out with
// a newtype wrapper.
unsafe impl<T: ?Sized + CxxDeletable + Send> Send for CxxHeap<T> {}
unsafe impl<T: ?Sized + CxxDeletable + Sync> Sync for CxxHeap<T> {}
