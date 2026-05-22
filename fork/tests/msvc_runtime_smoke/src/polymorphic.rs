//! v1.09.2 MSVC polymorphic-class runtime smoke.
//!
//! Builds a `#[repr(cpp)]` polymorphic class with a virtual method,
//! exercises virtual dispatch under the MSVC C++ ABI. Validates:
//!
//! - vftable emission (`??_7Widget@@6B@` global with COL at slot 0)
//! - vptr-init on construction (offset +ptr_bytes past vtable start)
//! - virtual dispatch through the vptr
//!
//! Expected runtime exit code: 17. Any other code means a v1.09.1
//! patch regressed.

#![no_std]
#![no_main]
#![feature(rustc_attrs)]
#![allow(internal_features)]

pub class Widget {
    count: i32,

    #[constructor]
    pub fn new(initial: i32) -> Self { Widget { count: initial } }

    #[cpp_virtual]
    pub fn next(&mut self) -> i32 {
        self.count += 1;
        self.count
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn ExitProcess(uExitCode: u32) -> !;
}

#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> ! {
    let mut w = Widget::new(16);
    let result = w.next();
    unsafe { ExitProcess(result as u32) }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }
