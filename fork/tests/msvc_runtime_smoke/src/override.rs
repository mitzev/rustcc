//! v1.09.2 MSVC virtual-method override runtime smoke.
//!
//! Validates that single-inheritance virtual dispatch picks the
//! most-derived override at the vtable slot, not the base.
//!
//! Setup:
//!   class Animal { virtual fn legs() -> u32 { 4 } }
//!   class Dog : Animal { virtual fn legs() -> u32 { 4 } virtual fn bark() -> u32 { 10 } }
//!
//! Calling legs() through a Dog should go via the Dog vtable
//! (returns 4), and bark() should resolve via the Dog vtable
//! (returns 10). Sum: 14.
//!
//! Expected exit code: 14.

#![no_std]
#![no_main]
#![feature(rustc_attrs)]
#![allow(internal_features)]

pub class Animal {
    tag: u32,

    #[constructor]
    pub fn new(tag: u32) -> Self { Animal { tag } }

    #[cpp_virtual]
    pub fn legs(&self) -> u32 { 4 }
}

pub class Dog : Animal {
    bark_count: u32,

    #[constructor]
    pub fn new(tag: u32, bark_count: u32) -> Self {
        Dog { __base: Animal::new(tag), bark_count }
    }

    // Override — same name as Animal::legs, takes over base slot.
    #[cpp_virtual]
    pub fn legs(&self) -> u32 { 4 }

    // New virtual — appended as a new slot.
    #[cpp_virtual]
    pub fn bark(&self) -> u32 { self.bark_count }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn ExitProcess(uExitCode: u32) -> !;
}

#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> ! {
    let d = Dog::new(1, 10);
    let total = d.legs() + d.bark();
    unsafe { ExitProcess(total) }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }
