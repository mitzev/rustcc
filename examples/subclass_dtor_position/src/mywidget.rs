
//! v1.13.10 regression example: subclass an imported C++ base whose
//! virtual DESTRUCTOR is not the first declared virtual.
//!
//! `NotFirst` declares `early()`, then `~NotFirst()`, then `late()` —
//! Itanium lays its vtable out as `[early, D1, D0, late]` (the dtor
//! pair at its DECLARATION position). Before v1.13.10 the fork pinned
//! the pair to the front, so C++ calling `early()` on a Rust subclass
//! dispatched into the DESTRUCTOR. The importer now emits a positional
//! `slot=~dtor,~` record and the fork places the pair exactly where
//! clang does.

include!("../target/gen-out/bindings.rs");

unsafe extern "C++" {
    #[link_name = "_Znwm"] // C++ operator new(size_t)
    fn cxx_operator_new(size: usize) -> *mut u8;
}

unsafe extern "C" {
    fn note_drop(); // test counter (defined in caller.cpp)
}

pub class D : NotFirst {
    extra: i32,

    pub constructor fn new(x: i32, extra: i32) -> Self {
        D { __base: NotFirst::new(x), extra }
    }

    pub override fn early(&self) -> i32 { self.extra + 100 }
    pub override fn late(&self) -> i32 { self.extra + 200 }
}

impl Drop for D {
    fn drop(&mut self) {
        unsafe { note_drop() }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn make_d(x: i32, extra: i32) -> *mut D {
    unsafe {
        let p = cxx_operator_new(core::mem::size_of::<D>()) as *mut D;
        p.write(D::new(x, extra));
        p
    }
}
