//! End-to-end exercise for the `cxx_class!` function-like macro.
//!
//! Compiling this test proves that the macro input parses and
//! the expansion is syntactically valid Rust. We don't link
//! against a real C++ object here — that's covered by the
//! fork-level probes in `/tmp/p09-*`.

use rustcc_macros::cxx_class;

// Phase 1 scope: primitives + raw pointers + refs + user classes.
cxx_class! {
    #[size = 16]
    #[align = 8]
    pub class Widget {
        #[ctor]
        fn new(handle: u64) -> Self;
        fn area(&self) -> u64;
        fn bump(&mut self, by: u64);
        fn compare(&self, other: *const Widget) -> bool;
    }
}

#[test]
fn widget_layout_size_matches() {
    // 16 bytes was declared at the macro site; Rust's mem::size_of
    // should agree (we wrote `__opaque: [u8; 16]`).
    assert_eq!(core::mem::size_of::<Widget>(), 16);
    assert_eq!(core::mem::align_of::<Widget>(), 8);
}

// Second class exercising the no-args-ctor and static-method
// paths. Gets wrapped in a separate `mod` so the two classes'
// auto-dtors don't collide.
mod second {
    use rustcc_macros::cxx_class;

    cxx_class! {
        #[size = 8]
        pub class Counter {
            #[ctor]
            fn zero() -> Self;
            fn increment(&mut self);
            fn read(&self) -> u32;
        }
    }

    #[test]
    fn counter_layout() {
        assert_eq!(core::mem::size_of::<Counter>(), 8);
    }
}
