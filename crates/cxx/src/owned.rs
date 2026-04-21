//! Owned C++ value. Non-`Copy`. Runs the C++ destructor on `Drop`.
//!
//! See `docs/ownership_and_safety.md §3.1`.
//!
//! **v1 runtime.** The backing storage is a heap-allocated, pinned `T`.
//! `Drop` on a `CxxOwned<T>` runs Rust's `Drop` glue for `T`, which is
//! the correct behaviour when `T` is a Rust type or a `#[repr(cpp)]`
//! type whose destructor rustcc will later have lowered into Rust's
//! drop glue via the mechanism in `docs/codegen.md §3.2`. Until the
//! codegen layer lands, this is therefore a faithful runtime for Rust
//! types and a forward-compatible shape for C++ ones.
//!
//! `CxxOwned<T>` is `!Unpin` regardless of `T`: even trivially
//! relocatable C++ types benefit from pinning because rustcc codegen
//! will store their address where it matters (e.g., in base-subobject
//! vptrs). Pinning is the conservative default; callers that know their
//! type is address-insensitive can move out via `CxxOwned::into_inner`.

use core::marker::{PhantomData, PhantomPinned};
use core::pin::Pin;

pub struct CxxOwned<T> {
    storage: Pin<Box<T>>,
    _pin: PhantomPinned,
    _phantom: PhantomData<T>,
}

impl<T> CxxOwned<T> {
    /// Construct by moving `value` onto the heap.
    pub fn new(value: T) -> Self {
        Self {
            storage: Box::pin(value),
            _pin: PhantomPinned,
            _phantom: PhantomData,
        }
    }

    /// Take ownership of `ptr` (which must have been produced by
    /// `Box::into_raw(Box::new(...))` or an equivalent heap allocation).
    /// `CxxOwned` will free it on drop.
    ///
    /// # Safety
    /// * `ptr` must be non-null and point to a fully-initialized `T`.
    /// * The memory pointed to must be a valid heap allocation compatible
    ///   with `Box::from_raw` (same global allocator, same `T` layout).
    /// * Ownership must be unique — no other `CxxOwned`, `Box`, or
    ///   `CxxShared` may reference `ptr`.
    pub unsafe fn from_raw(ptr: *mut T) -> Self {
        let boxed = unsafe { Box::from_raw(ptr) };
        Self {
            storage: Box::into_pin(boxed),
            _pin: PhantomPinned,
            _phantom: PhantomData,
        }
    }

    /// Release ownership, returning the raw pointer. The caller becomes
    /// responsible for dropping the `T` (typically by calling
    /// `CxxOwned::from_raw` again or by hand-calling the C++ dtor).
    pub fn into_raw(self) -> *mut T {
        // We are about to relinquish pinning by handing out a raw
        // pointer. This is safe in the sense that the allocation is
        // unchanged — the caller now bears responsibility for honoring
        // any address-stability contract.
        let boxed = unsafe { Pin::into_inner_unchecked(self.storage) };
        Box::into_raw(boxed)
    }

    /// Shared access to the pinned `T`.
    pub fn as_ref_(&self) -> &T {
        &self.storage
    }

    /// Pinned mutable access to `T`.
    pub fn as_pin_mut(&mut self) -> Pin<&mut T> {
        self.storage.as_mut()
    }
}

impl<T> core::ops::Deref for CxxOwned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.as_ref_()
    }
}
