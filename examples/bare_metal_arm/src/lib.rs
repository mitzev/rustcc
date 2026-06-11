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

// Static storage — no heap on bare-metal.
static mut SLOT: Option<Widget> = None;
static mut GAUGE_SLOT: Option<Gauge> = None;

#[unsafe(no_mangle)]
pub extern "C" fn init_widget(v: i32) -> *mut Widget {
    unsafe {
        SLOT = Some(Widget::new(v));
        match SLOT.as_mut() {
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
        GAUGE_SLOT = Some(Gauge::new(v, scale));
        match GAUGE_SLOT.as_mut() {
            Some(g) => g as *mut Gauge as *mut Widget,
            None => core::ptr::null_mut(),
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { loop {} }
