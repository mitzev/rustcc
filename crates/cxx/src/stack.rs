//! `cxx_stack!` — pinned, stack-allocated C++ value.
//!
//! See `docs/ownership_and_safety.md §3.3`.

/// Construct a value on the stack and bind it as a pinned mutable
/// reference. The macro guarantees that the storage lives for the
/// remainder of the enclosing scope and that `$name` never escapes
/// beyond that scope (since `$name` borrows from a local).
///
/// # Example
/// ```
/// cxx::cxx_stack!(w: u32 = 42u32);
/// assert_eq!(*w.as_ref(), 42);
/// ```
///
/// # Pinning contract
///
/// The macro is address-stable: `w` points at a fixed stack slot until
/// the enclosing scope ends. Moving `w` out of the slot would violate
/// the pin invariant, so the macro returns `Pin<&mut T>` which forbids
/// moving through it in safe code. For `T: Unpin` types (most Rust
/// types) the pin is effectively a no-op; the guarantee matters for
/// `#[repr(cpp)]` or other address-sensitive types.
#[macro_export]
macro_rules! cxx_stack {
    ($name:ident : $ty:ty = $init:expr) => {
        let mut __cxx_stack_slot: $ty = $init;
        // SAFETY: `__cxx_stack_slot` is a local whose address is stable
        // for the remainder of this scope, and we never move it after
        // this line (we only borrow pinned-mutably through `$name`).
        // Pinning it is therefore sound, and the `Pin<&mut T>` that
        // escapes prevents safe-Rust moves for the rest of the scope.
        #[allow(unused_mut)]
        let mut $name: ::core::pin::Pin<&mut $ty> = unsafe {
            ::core::pin::Pin::new_unchecked(&mut __cxx_stack_slot)
        };
    };
}
