//! v1.09.2 MSVC virtual destructor runtime smoke.
//!
//! Validates v1.09.1 patch 18 (scalar deleting destructor) at
//! runtime: a polymorphic class's dtor runs when the value goes
//! out of scope, observable via a side-effect on a static.
//!
//! Expected exit code: 100 (sentinel set by the dtor).

#![no_std]
#![no_main]
#![feature(rustc_attrs)]
#![allow(internal_features)]

pub class Resource {
    handle: u32,

    #[constructor]
    pub fn new(handle: u32) -> Self { Resource { handle } }

    #[cpp_virtual]
    pub fn get(&self) -> u32 { self.handle }
}

impl Drop for Resource {
    fn drop(&mut self) {
        unsafe { DTOR_FLAG = 100; }
    }
}

static mut DTOR_FLAG: u32 = 0;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn ExitProcess(uExitCode: u32) -> !;
}

#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> ! {
    {
        let r = Resource::new(42);
        let _ = r.get();
    } // r dropped here — dtor runs, sets DTOR_FLAG
    unsafe { ExitProcess(DTOR_FLAG) }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }
