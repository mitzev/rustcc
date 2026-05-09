//! Minimal FLTK text editor — end-to-end smoke test for rustcc's
//! cxx_importer + Itanium ABI fork.
//!
//! What this exercises:
//!   - Multi-inheritance import (Fl_Window → Fl_Group → Fl_Widget)
//!     including M22 cross-base accessors (`window.as_fl_group_mut()`).
//!   - Itanium ctor / dtor mangling: `Fl_Text_Buffer::new` calls
//!     `_ZN14Fl_Text_BufferC1Eii`; matching D1 dtor on Drop.
//!   - Virtual dispatch through Fl_Widget's vtable: clicking
//!     into the editor triggers `Fl_Text_Editor::handle()`.
//!   - cstr ergonomics (M20.b): we pass `c"..."` via `new_cstr`.
//!   - The fork's `extern "C++"` ABI: every shim call goes through
//!     a `__rustcc_shim_<mangled>` trampoline emitted by
//!     cxx_importer::shims and linked from the static archive.
//!
//! Build + run:
//!
//! ```sh
//! cd examples/fltk_text_editor
//! # 1. Generate bindings + compile shim archive (libclang only).
//! cargo run --release --bin gen_bindings
//! # 2. Build + run the editor (fork rustc only).
//! cargo +rustcc run --release --bin editor
//! ```

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_unsafe)]
#![allow(unused_variables)]
#![allow(unused_parens)]
#![allow(clippy::all)]

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/target/m26-out/bindings.rs"));

fn main() {
    // SAFETY: every binding call below is `unsafe` at the FFI
    // surface. Encapsulating all the unsafety in `main` keeps
    // the demo's intent legible without scattering `unsafe`
    // blocks everywhere.
    unsafe { run_editor() }
}

unsafe fn run_editor() {
    // 1. Top-level window. `new_cstr` is the M20.b ergonomic
    //    wrapper around `Fl_Window(W, H, const char* title)` that
    //    accepts a `&CStr`. The non-cstr `new(W, H, *const c_char)`
    //    works equally well; we use `_cstr` to flex M20.b.
    let title = c"rustcc text editor";
    let mut window = Fl_Window::new_cstr(800, 600, title);

    // 2. Backing storage for the editor. `Fl_Text_Buffer::new`
    //    has two int defaults (requestedSize, preferredGapSize);
    //    `new_with_defaults` skips both via M18.b.
    let mut buffer = Fl_Text_Buffer::new_with_defaults();

    // Pre-load a banner. The C++ method is overloaded; pick the
    // `_str` form (M20.c) which takes `&str` and copies into a
    // CString internally.
    buffer.text_const_i8_str(
        "// rustcc text editor\n\
         //\n\
         // This window is alive because cxx_importer parsed FLTK\n\
         // headers, generated bindings + C++ shims, the rustcc\n\
         // fork compiled `extern \"C++\"` Rust, and the linker\n\
         // pulled it together with libfltk.\n\
         //\n\
         // Type something. The editor's virtual `handle()`\n\
         // dispatch fires through the vtable that\n\
         // `cxx_importer::populate_vtable_indices` walked when\n\
         // it imported Fl_Text_Editor.\n",
    );

    // 3. The editor widget itself. Sized to fill the window.
    //    Fifth ctor arg is the optional label; `null()` for
    //    no label.
    let mut editor =
        Fl_Text_Editor::new(0, 0, 800, 600, ::core::ptr::null());

    // 4. Wire the buffer into the editor. `buffer` is a method
    //    on Fl_Text_Display (the editor's base); we reach it
    //    through the M22 cross-base accessor.
    editor
        .as_fl_text_display_mut()
        .buffer(&mut buffer as *mut Fl_Text_Buffer);

    // 5. End the window's group context (Fl_Group::end). Reached
    //    via M22 `as_fl_group_mut()` cross-base accessor.
    window.as_fl_group_mut().end();

    // 6. Show the window and pump the FLTK event loop.
    window.show();

    // Fl::run() returns when the last window closes.
    Fl::run();
}
