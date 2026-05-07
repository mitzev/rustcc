//! `CxxCallback<F>` — closure-as-C++-callback adapter (M15.b).
//!
//! Many C++ libraries use the classic two-argument callback shape:
//!
//! ```cpp
//! widget->callback(my_func, user_data);
//! //               ^^^^^^^  ^^^^^^^^^
//! //               function-pointer    void* opaque
//! ```
//!
//! and store both halves; on each invocation they call
//! `my_func(args..., user_data)`. Rust's idiomatic equivalent is a
//! closure capturing the would-be user-data, but a closure can't
//! be passed to C++ directly — its calling convention isn't
//! `extern "C"` and its environment lives on the Rust stack.
//!
//! `CxxCallback<F>` wraps a closure into a heap-stable, ABI-stable
//! pair of `(extern "C" fn, *mut c_void)` that a C++ API can store
//! and call. The wrapper keeps the closure alive until the
//! callback is unregistered (via `Drop` on `CxxCallback`).
//!
//! ## Status
//!
//! v0 is a single-arity scaffold: handles `Fn(*mut c_void) ->
//! ()`. Real-world usage needs per-arity glue (e.g. `Fn(int) ->
//! ()`, `Fn(int, *mut c_void) -> int`) which is best generated
//! per-callback-type by the importer (M15.c). The shape exposed
//! here matches what FLTK's `Fl_Callback` looks like
//! (`void(Fl_Widget*, void*)`) so the most common case is
//! covered immediately; richer shapes layer on top in a
//! subsequent release without breaking the current API.

use core::ffi::c_void;
use core::marker::PhantomData;

/// Heap-stable adapter wrapping a Rust closure into the
/// `(fn-pointer, user-data)` shape that C++ callbacks expect.
///
/// The closure is boxed and the box's raw pointer is handed to
/// C++ as `*mut c_void`; a generated `extern "C" fn` thunk
/// re-borrows it on each callback invocation. Dropping the
/// `CxxCallback` reclaims the box — so the user must keep the
/// `CxxCallback` alive for as long as the C++ side might fire it.
///
/// `F` is the closure type; the wrapper is `Send + Sync` only when
/// `F` is, and the resulting fn-pointer can be passed across
/// thread boundaries iff the closure was already known to be
/// thread-safe. The standard library's `Fn` trait bound captures
/// "callable repeatedly with shared access" — exactly what C++
/// callbacks require.
pub struct CxxCallback<F> {
    boxed: *mut c_void,
    _phantom: PhantomData<F>,
}

impl<F: Fn() + 'static> CxxCallback<F> {
    /// Heap-allocate `f` and return a `CxxCallback` whose
    /// `(fn_ptr, user_data)` pair is ready to hand to C++.
    pub fn new(f: F) -> Self {
        let boxed: Box<F> = Box::new(f);
        let raw = Box::into_raw(boxed) as *mut c_void;
        Self {
            boxed: raw,
            _phantom: PhantomData,
        }
    }

    /// Returns the `extern "C" fn(*mut c_void)` thunk to pass as
    /// the function-pointer half of the callback pair. Calls
    /// through to the boxed closure on every invocation.
    ///
    /// Taking `&self` (rather than being a static method) makes
    /// the type parameter `F` inferable at call sites where `F`
    /// is the unnameable closure type — `cb.fn_ptr()` is enough.
    pub fn fn_ptr(&self) -> unsafe extern "C" fn(*mut c_void) {
        thunk::<F>
    }

    /// Returns the opaque user-data pointer to pair with
    /// [`Self::fn_ptr`]. The C++ side stores this verbatim and
    /// passes it back to the thunk on each invocation.
    pub fn user_data(&self) -> *mut c_void {
        self.boxed
    }
}

unsafe extern "C" fn thunk<F: Fn() + 'static>(user: *mut c_void) {
    // SAFETY: `user` was created by `Box::into_raw(Box::new(f))`
    // in `CxxCallback::new` and is alive as long as the
    // owning `CxxCallback` hasn't been dropped. We re-borrow
    // (not re-take) the closure for the duration of this
    // call so subsequent invocations stay legal.
    let f: &F = unsafe { &*(user as *const F) };
    f();
}

impl<F> Drop for CxxCallback<F> {
    fn drop(&mut self) {
        if !self.boxed.is_null() {
            // SAFETY: `self.boxed` was minted by
            // `Box::into_raw(Box::new(...))` in `new`. We're
            // the unique owner. Reclaim it back into a `Box<F>`
            // and let it drop normally.
            unsafe {
                let _ = Box::from_raw(self.boxed as *mut F);
            }
            self.boxed = core::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn invoking_the_thunk_calls_the_closure() {
        let counter = Rc::new(Cell::new(0u32));
        let counter_for_closure = counter.clone();
        let cb: CxxCallback<_> =
            CxxCallback::new(move || counter_for_closure.set(counter_for_closure.get() + 1));

        let f = cb.fn_ptr();
        let data = cb.user_data();
        // SAFETY: `f`'s boxed closure is alive as long as `cb` is.
        unsafe { f(data) };
        unsafe { f(data) };
        unsafe { f(data) };
        assert_eq!(counter.get(), 3);
    }

    #[test]
    fn dropping_the_callback_reclaims_the_box() {
        struct Probe(Rc<Cell<bool>>);
        impl Drop for Probe {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        let dropped = Rc::new(Cell::new(false));
        let probe = Probe(dropped.clone());
        let cb = CxxCallback::new(move || {
            // Move the probe into the closure so its drop
            // fires when the closure is dropped.
            let _keep_alive = &probe;
        });
        assert!(!dropped.get(), "drop hasn't fired while cb is alive");
        drop(cb);
        assert!(dropped.get(), "drop fired after cb went away");
    }
}
