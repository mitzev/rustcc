// Minimal MSVC cross-compile probe — no_std + no_main, exports
// one C-ABI function so we can `nm` the result.
#![no_std]
#![no_main]

#[unsafe(no_mangle)]
pub extern "C" fn add(a: i32, b: i32) -> i32 {
    a + b
}

// Windows /SUBSYSTEM:CONSOLE entry point — link looks for either
// `main`/`WinMain`/`mainCRTStartup`. Defining a bare entry lets
// the linker resolve the EXE without dragging in the CRT.
#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> i32 {
    add(1, 2)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
