//! Reference-counted C++ handle.
//!
//! See `docs/ownership_and_safety.md §3.4`.
//!
//! **v1 runtime.** Backed by `std::sync::Arc<T>`. For C++ types that
//! carry intrinsic refcounting (e.g. classes annotated
//! `[[rustcc::shared_reference(retain=..., release=...)]]`), rustcc
//! codegen will later replace `Clone::clone` and `Drop::drop` with
//! calls to the user-supplied retain/release functions. Until that
//! lands, `Arc` gives us a portable, correct implementation with the
//! same public API shape.

use std::sync::Arc;

pub struct CxxShared<T> {
    inner: Arc<T>,
}

impl<T> CxxShared<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(value),
        }
    }

    pub fn as_ref_(&self) -> &T {
        &self.inner
    }

    /// Current strong-reference count. Useful for tests.
    pub fn strong_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }
}

impl<T> Clone for CxxShared<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> core::ops::Deref for CxxShared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}
