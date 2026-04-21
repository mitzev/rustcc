//! Derived-to-base explicit upcast trait.
//!
//! See `docs/cxx_importer.md §9`.

pub trait CxxBase<Parent: ?Sized> {
    fn upcast(&self) -> &Parent;
    fn upcast_mut(&mut self) -> &mut Parent;
}
