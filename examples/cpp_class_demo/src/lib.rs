// Demo of `#[cpp_class]` — the stable-rustc-compatible interop entry
// point. Expands to `#[repr(C)]` at compile time, so this file
// compiles cleanly under stable rustc while still being recognized
// by the rustcc driver's source-text scanner.

use rustcc_macros::cpp_class;

#[cpp_class]
pub struct Counter {
    pub n: i64,
}

impl Counter {
    pub fn new() -> Self {
        Counter { n: 0 }
    }

    pub fn get(&self) -> i64 {
        self.n
    }

    pub fn bump(&mut self, by: i64) {
        self.n += by;
    }
}

#[cpp_class]
pub enum Signal {
    Green = 0,
    Yellow = 1,
    Red = 2,
}

// Rust-side usage to prove the layout survives stable compilation.
pub fn default_counter() -> Counter {
    let mut c = Counter::new();
    c.bump(1);
    c
}

pub fn signal_label(s: Signal) -> &'static str {
    match s {
        Signal::Green => "go",
        Signal::Yellow => "slow",
        Signal::Red => "stop",
    }
}

// Pull in the rustcc-generated forwarder thunks. The `rustcc`
// driver emits them to `<cache>/cpp_class_demo-cxx-forwarders.rs`
// when `emit-forwarders = true` in the manifest, and sets the
// `RUSTCC_FORWARDERS_PATH` env + `--cfg rustcc_forwarders` cfg flag
// during compilation. Stock `cargo build` (without the rustcc
// driver) sees the cfg as disabled and skips this include — which
// means the cdylib/staticlib won't have the Itanium-mangled
// symbols, but the Rust code still compiles.
#[cfg(rustcc_forwarders)]
include!(env!("RUSTCC_FORWARDERS_PATH"));
