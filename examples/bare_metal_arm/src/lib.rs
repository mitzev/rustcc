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
//!    (`constructor` / `virtual`, v1.13.5) survive under `#![no_std]`.
//! 4. Generated Rust ctor mangling matches Clang's on ARM32.

pub class Widget {
    v: i32,

    pub constructor fn new(v: i32) -> Self { Widget { v } }

    pub virtual fn foo(&self) -> i32 { self.v + 100 }
}

// Static storage — no heap on bare-metal.
static mut SLOT: Option<Widget> = None;

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

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { loop {} }
