//! Heap-owned C++ value.
//!
//! See `docs/ownership_and_safety.md §3.2`. Alias of `CxxOwned<T>` today;
//! a dedicated type may diverge later if allocator distinctions matter.

pub type CxxBox<T> = crate::owned::CxxOwned<T>;
