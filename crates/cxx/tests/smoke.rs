// Runtime smoke tests — verify that the `cxx` crate's core types have
// working Rust-backed implementations (without the rustcc codegen layer).

use std::cell::Cell;
use std::rc::Rc;

use cxx::{CxxBase, CxxBox, CxxMove, CxxOwned, CxxShared, CxxString};

fn _compiles<T>()
where
    T: Sized,
{
}

#[test]
fn surface_is_visible() {
    _compiles::<CxxOwned<u8>>();
    _compiles::<CxxBox<u8>>();
    _compiles::<CxxShared<u8>>();
    _compiles::<CxxMove<u8>>();
    _compiles::<CxxString>();
    let _: fn() -> Box<dyn CxxBase<u8>> = || unimplemented!();
}

#[test]
fn cxx_owned_reads_through_as_ref() {
    let owned = CxxOwned::new(42i32);
    assert_eq!(*owned.as_ref_(), 42);
    assert_eq!(*owned, 42);
}

#[test]
fn cxx_owned_from_into_raw_roundtrip() {
    let raw = Box::into_raw(Box::new(0x1337u32));
    let owned = unsafe { CxxOwned::from_raw(raw) };
    assert_eq!(*owned.as_ref_(), 0x1337);
    let back = owned.into_raw();
    assert_eq!(back, raw);
    // Release the leak so miri / leak sanitizers stay clean.
    let _ = unsafe { Box::from_raw(back) };
}

#[test]
fn cxx_owned_drops_inner_value_when_scope_ends() {
    struct Tracked(Rc<Cell<bool>>);
    impl Drop for Tracked {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }
    let flag = Rc::new(Cell::new(false));
    {
        let _owned = CxxOwned::new(Tracked(flag.clone()));
        assert!(!flag.get(), "not yet dropped");
    }
    assert!(flag.get(), "drop should have fired when `_owned` left scope");
}

#[test]
fn cxx_stack_binds_pinned_mutable_reference() {
    cxx::cxx_stack!(w: i32 = 42i32);
    assert_eq!(*w.as_ref(), 42);
}

#[test]
fn cxx_stack_runs_drop_at_scope_end() {
    struct Tracked(Rc<Cell<bool>>);
    impl Drop for Tracked {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }
    let flag = Rc::new(Cell::new(false));
    {
        cxx::cxx_stack!(_t: Tracked = Tracked(flag.clone()));
        assert!(!flag.get());
    }
    assert!(flag.get(), "Tracked should have been dropped leaving scope");
}

#[test]
fn cxx_shared_refcounts() {
    let a = CxxShared::new(7u32);
    assert_eq!(a.strong_count(), 1);
    let b = a.clone();
    assert_eq!(a.strong_count(), 2);
    assert_eq!(*a.as_ref_(), 7);
    assert_eq!(*b.as_ref_(), 7);
    drop(b);
    assert_eq!(a.strong_count(), 1);
}
