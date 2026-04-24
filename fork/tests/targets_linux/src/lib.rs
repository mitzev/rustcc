// P09.44: polymorphic class probe for cross-target Linux / desktop
// Itanium output. Used by `fork/tests/run_targets.sh` which builds
// this crate against x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu,
// armv7-unknown-linux-gnueabihf, and riscv64gc-unknown-linux-gnu.

#![no_std]
#![feature(rustc_attrs)]

pub class Widget {
    x: i32,

    pub fn new(v: i32) -> Self { Self { x: v } }

    #[cpp_virtual]
    pub fn foo(&self) -> i32 { self.x }
}

// Keep the ctor and foo alive through unoptimized builds even at
// -C opt-level=2, so IR inspection finds both symbols.
#[no_mangle]
pub extern "C" fn make_widget(v: i32) -> Widget { Widget::new(v) }

#[no_mangle]
pub extern "C" fn call_foo(w: &Widget) -> i32 { w.foo() }

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }
