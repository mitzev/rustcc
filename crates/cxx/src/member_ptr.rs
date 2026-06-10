//! Pointer-to-member-function representation (Itanium C++ ABI §2.3).
//!
//! A member function pointer is a two-word pair. The Itanium *generic*
//! variant encodes the virtual/non-virtual discriminator in `ptr`
//! (function address, or 1 + vtable byte offset); the **ARM variant**
//! (used by AArch64, incl. Apple Silicon) keeps `ptr` un-tagged and
//! moves the discriminator into the LOW BIT of `adj`
//! (`adj = (this_adjustment << 1) | is_virtual`).
//!
//! Two encodings agree on the cases this crate constructs:
//! - **null** is `{0, 0}` in both variants;
//! - a **non-virtual member with zero this-adjustment** is
//!   `{fn_address, 0}` in both.
//!
//! Anything else (virtual targets, nonzero adjustments) is therefore
//! only produced on the C++ side and travels through Rust opaquely —
//! which is exactly the wx-style use (`Connect(&Class::OnEvent)`):
//! receive, store, pass back. Decoding/invoking from Rust is future
//! work (per-variant) — see `fork/MI-DESIGN-v1.14.md` phase 1.

use core::marker::PhantomData;

/// `int (T::*)(...)` — a C++ pointer-to-member-function of class `T`.
///
/// The parameter list is intentionally NOT captured in the type (v1):
/// this is a transport/repr type, not a callable. `PhantomData<*mut T>`
/// keeps it `!Send + !Sync` like the imported class types themselves.
#[repr(C)]
#[cfg_attr(feature = "rustcc-fork", rustc_diagnostic_item = "CxxMemberFnPtr")]
pub struct CxxMemberFnPtr<T> {
    ptr_or_voff: usize,
    adj: isize,
    _class: PhantomData<*mut T>,
}

impl<T> CxxMemberFnPtr<T> {
    /// The null member pointer (`nullptr`) — `{0, 0}` in both the
    /// generic and ARM Itanium variants.
    pub const fn null() -> Self {
        Self { ptr_or_voff: 0, adj: 0, _class: PhantomData }
    }

    /// True iff this is the null member pointer.
    ///
    /// (Itanium-generic null tests `ptr == 0`; the ARM variant tests
    /// `ptr == 0 && (adj & 1) == 0`. The combined test is correct for
    /// both.)
    pub fn is_null(&self) -> bool {
        self.ptr_or_voff == 0 && (self.adj & 1) == 0
    }

    /// Build a member pointer to a NON-VIRTUAL member function with
    /// zero this-adjustment — `{addr, 0}`, identical in the generic
    /// and ARM encodings.
    ///
    /// # Safety
    /// `addr` must be the address of a function whose ABI matches a
    /// C++ member function of `T` (takes `this: *mut T` first, in the
    /// C++ calling convention) and whose signature matches what the
    /// receiving C++ code will invoke it with.
    pub unsafe fn from_nonvirtual_fn(addr: usize) -> Self {
        Self { ptr_or_voff: addr, adj: 0, _class: PhantomData }
    }

    /// The raw `{ptr_or_voff, adj}` pair (encoding is target-variant
    /// specific — see the module docs).
    pub fn raw_parts(&self) -> (usize, isize) {
        (self.ptr_or_voff, self.adj)
    }
}

// Manual impls: derives would bound `T: Clone`/`T: Copy`, but the pair
// is always copyable regardless of the class type parameter.
impl<T> Clone for CxxMemberFnPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for CxxMemberFnPtr<T> {}

impl<T> core::fmt::Debug for CxxMemberFnPtr<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CxxMemberFnPtr")
            .field("ptr_or_voff", &(self.ptr_or_voff as *const ()))
            .field("adj", &self.adj)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_words_and_null_roundtrip() {
        struct Dummy;
        assert_eq!(
            core::mem::size_of::<CxxMemberFnPtr<Dummy>>(),
            2 * core::mem::size_of::<usize>(),
        );
        assert_eq!(
            core::mem::align_of::<CxxMemberFnPtr<Dummy>>(),
            core::mem::align_of::<usize>(),
        );
        let n = CxxMemberFnPtr::<Dummy>::null();
        assert!(n.is_null());
        let f = unsafe { CxxMemberFnPtr::<Dummy>::from_nonvirtual_fn(0x1000) };
        assert!(!f.is_null());
        assert_eq!(f.raw_parts(), (0x1000, 0));
    }
}
