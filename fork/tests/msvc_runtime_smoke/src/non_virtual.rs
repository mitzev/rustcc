//! Class-without-virtual baseline: same `class` keyword + ctor, no
//! virtual method. Confirms the class machinery itself works under
//! MSVC. Explicitly calls ExitProcess from kernel32 so Wine sees
//! the right exit code regardless of CRT init state.

#![no_std]
#![no_main]
#![feature(rustc_attrs)]
#![allow(internal_features)]

pub class Widget {
    count: i32,

    #[constructor]
    pub fn new(initial: i32) -> Self { Widget { count: initial } }

    pub fn bump(&mut self) -> i32 {
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
    let result = w.bump();
    unsafe { ExitProcess(result as u32) }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }
