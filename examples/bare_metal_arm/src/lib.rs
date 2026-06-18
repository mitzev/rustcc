#![no_std]
// v1.14: the fork's class/ctor/virtual attributes are ungated — no
// feature gates or allow attributes needed.

//! Bare-metal ARM Cortex-M (STM32) example.
//!
//! Builds for `thumbv7em-none-eabihf` (Cortex-M4F) and other
//! `thumbv7m` / `thumbv7em` / `thumbv8m.*` targets. Demonstrates:
//!
//! 1. `#[repr(cpp)]` layout on 32-bit ARM.
//! 2. `virtual fn` emits correct vtable / typeinfo with 4-byte
//!    pointer slots.
//! 3. Parser-level `class` keyword + method-modifier keywords
//!    (`constructor` / `virtual` / `override`, v1.13.5) survive under
//!    `#![no_std]`.
//! 4. Generated Rust ctor mangling matches Clang's on ARM32.
//! 5. **Subclassing** (single inheritance, Rust base): `Gauge : Widget`
//!    shares the base vptr at offset 0; C++ dispatching through a
//!    `Widget*` lands in the Rust `override` — heap-free, static
//!    storage only.

pub class Widget {
    v: i32,

    pub constructor fn new(v: i32) -> Self { Widget { v } }

    pub virtual fn foo(&self) -> i32 { self.v + 100 }
}

// Derived Rust class over the Rust base — no heap anywhere. The
// parser synthesizes the `__base` field; the v1.14 ctor-in-place
// pass constructs the base directly into the base subobject.
pub class Gauge : Widget {
    scale: i32,

    pub constructor fn new(v: i32, scale: i32) -> Self {
        Self { __base: Widget::new(v), scale }
    }

    pub override fn foo(&self) -> i32 { self.scale * 1000 }

    pub virtual fn bar(&self) -> i32 { self.scale + 7 }
}

// ------------------------------------------------------------------
// Subclassing an IMPORTED C++ base, heap-free.
//
// `Sensor` is defined in sensor.hpp / sensor.cpp and compiled with
// arm-none-eabi-g++ — Rust never sees the C++ definition. The binding
// below is exactly what `cxx_importer` emits for it (hand-inlined so
// the probe stays self-contained / host-toolless): an opaque
// #[repr(C)] shell carrying the base's vtable slots in
// #[rustc_cxx_imported_vtable], plus the by-value `new` whose
// MaybeUninit shape the v1.14 ctor-in-place pass folds into the
// return slot. Non-virtual dtor → no `vdtor=1`, no heap, no Drop.
// ------------------------------------------------------------------

#[repr(C)]
#[repr(align(4))]
#[rustc_cxx_imported_vtable = "ztv=_ZTV6Sensor;zti=_ZTI6Sensor;slot=read,_ZNK6Sensor4readEv,v;slot=unit,_ZNK6Sensor4unitEv,v"]
pub struct Sensor {
    _opaque: [core::mem::MaybeUninit<u8>; 8], // vptr + int32_t id
    _not_send_sync: core::marker::PhantomData<*mut u8>,
}

unsafe extern "C++" {
    #[link_name = "_ZN6SensorC2Ei"]
    fn __cxx_Sensor_new(this: *mut Sensor, id: i32);
}

impl Sensor {
    pub fn new(id: i32) -> Self {
        unsafe {
            let mut __slot = core::mem::MaybeUninit::<Self>::uninit();
            __cxx_Sensor_new(__slot.as_mut_ptr(), id);
            __slot.assume_init()
        }
    }
}

// Rust subclass of the imported base. `read` overrides the C++ slot;
// `unit` is NOT overridden, so the derived vtable's slot 1 points
// straight at the GCC-compiled `_ZNK6Sensor4unitEv`.
pub class Reader : Sensor {
    offset: i32,

    pub constructor fn new(id: i32, offset: i32) -> Self {
        Self { __base: Sensor::new(id), offset }
    }

    pub override fn read(&self) -> i32 { self.offset + 500 }
}

// Static storage — no heap on bare-metal.
static mut SLOT: Option<Widget> = None;
static mut GAUGE_SLOT: Option<Gauge> = None;
static mut READER_SLOT: Option<Reader> = None;

#[unsafe(no_mangle)]
pub extern "C" fn init_widget(v: i32) -> *mut Widget {
    unsafe {
        // Go through a raw pointer (`&raw mut`) rather than `&mut SLOT`:
        // a reference to a `static mut` trips the `static_mut_refs`
        // lint (a hard error in edition 2024). The deref-of-raw-pointer
        // reference is fine, and the place semantics are identical.
        let slot = &raw mut SLOT;
        *slot = Some(Widget::new(v));
        match (*slot).as_mut() {
            Some(w) => w as *mut Widget,
            None => core::ptr::null_mut(),
        }
    }
}

/// Returns the derived object UPCAST to the base — the C++ side only
/// sees a `Widget*`, so `w->foo()` must dispatch through the vtable
/// (no devirtualization possible) and must land in `Gauge::foo`.
#[unsafe(no_mangle)]
pub extern "C" fn init_gauge(v: i32, scale: i32) -> *mut Widget {
    unsafe {
        let slot = &raw mut GAUGE_SLOT;
        *slot = Some(Gauge::new(v, scale));
        match (*slot).as_mut() {
            Some(g) => g as *mut Gauge as *mut Widget,
            None => core::ptr::null_mut(),
        }
    }
}

/// Same shape over the IMPORTED base: builds a `Reader : Sensor` in
/// static storage (the C++ ctor runs in place — v1.14) and returns it
/// as a `Sensor*` for C++-side virtual dispatch.
#[unsafe(no_mangle)]
pub extern "C" fn init_reader(id: i32, offset: i32) -> *mut Sensor {
    unsafe {
        let slot = &raw mut READER_SLOT;
        *slot = Some(Reader::new(id, offset));
        match (*slot).as_mut() {
            Some(r) => r as *mut Reader as *mut Sensor,
            None => core::ptr::null_mut(),
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { loop {} }
