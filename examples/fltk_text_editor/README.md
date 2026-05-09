# fltk_text_editor

A minimal FLTK text editor in Rust. End-to-end smoke test for rustcc's cxx_importer + Itanium ABI fork.

## What this exercises

- **Multi-inheritance import** — Fl_Window → Fl_Group → Fl_Widget. M22 cross-base accessors (`window.as_fl_group_mut()`).
- **Itanium ctor / dtor mangling** — `Fl_Text_Buffer::new` calls `_ZN14Fl_Text_BufferC1Eii`; `Drop` runs the matching D1 dtor.
- **Virtual dispatch** — clicking into the editor triggers `Fl_Text_Editor::handle()` through the vtable that `populate_vtable_indices` walked when it imported the class.
- **cstr ergonomics (M20.b/c)** — `c"…"` literals via `new_cstr`, `&str` via `text_const_i8_str`.
- **Abstract-class detection** — Fl_Menu_, Fl_Input_, Fl_Device_Plugin (transitively pulled in by Fl_Menu_Bar.H) all have pure virtuals; the importer skips ctor-shim emission for them so the C++ shims compile.
- **The fork's `extern "C++"` ABI** — every shim call goes through `__rustcc_shim_<mangled>` trampolines that the rustcc fork accepts as legitimate C++ entry points.

## Prereqs

- macOS / Linux with libclang available to `clang-sys` (`brew install llvm`, `apt install libclang-dev`).
- FLTK 1.4.x installed system-wide. macOS: `brew install fltk`. The build helper expects FLTK at `/opt/homebrew/include` and `/opt/homebrew/Cellar/fltk/1.4.5/lib`; edit `gen_bindings.rs` if your install lives elsewhere.
- The rustcc fork toolchain to actually run the editor — `extern "C++"` is a fork extension. Install via the [v1.05.0 release](https://github.com/rustcc/rustcc/releases/tag/v1.05.0) or `./fork/build.sh` from this repo's root.

## Build + run

```bash
cd examples/fltk_text_editor

# 1. Generate bindings + compile shim archive. Stock rustc/cargo;
#    only needs libclang + FLTK installed.
cargo run --release --bin gen_bindings

# 2. Build + run the editor. Needs the rustcc fork rustc.
cargo +rustcc run --release --bin editor
```

You should see an 800x600 window with a banner comment in a code-style monospace font. Type into it; the cursor blinks; selection works; `Ctrl+C / Ctrl+V` work because Fl_Text_Editor's default key bindings handle them. Close the window to exit.

## File layout

```
fltk_text_editor/
  Cargo.toml          # standalone manifest (outside the workspace —
                      # cxx_importer's libclang feature would bloat
                      # the workspace's crate graph)
  cpp/umbrella.hpp    # FLTK headers the importer parses
  gen_bindings.rs     # M26 build pipeline (libclang + FLTK only)
  src/main.rs         # the editor itself (rustcc fork only)
  target/m26-out/     # generated outputs:
    bindings.rs       #   ~950 KB Rust source the editor `include!`s
    cxx_shims.cpp     #   ~3 MB C++ trampolines compiled by cc-rs
    libfltk_text_editor.a  # ~470 KB static archive of the trampolines
```

## What's pulled into the umbrella

Each header transitively drags in the FLTK class hierarchy needed for it:

| Header | Adds |
|---|---|
| `Fl_Window.H` | Fl_Window, Fl_Group, Fl_Widget |
| `Fl_Text_Editor.H` | Fl_Text_Editor, Fl_Text_Display, Fl_Group |
| `Fl_Text_Buffer.H` | Fl_Text_Buffer, Fl_Text_Selection |
| `Fl_Menu_Bar.H` | Fl_Menu_Bar, Fl_Menu_, Fl_Widget |
| `Fl_Menu_Item.H` | Fl_Menu_Item |
| `Fl_Native_File_Chooser.H` | Fl_Native_File_Chooser |

Final binding count: ~50 classes, ~2500 methods, several abstract intermediates that get importer-detected and ctor-skipped (Fl_Menu_, Fl_Input_, Fl_Device_Plugin).

## Limitations / TODO

- **No menu bar yet.** Wiring `Fl_Menu_Bar` requires user-callback ergonomics (M15.b extension): a `Fl_Callback*` field needs a Rust closure trampoline. The importer emits `Fl_Callback_Wrapper<F>` but the editor flow needs threading it through `Fl_Menu_Bar::add(label, shortcut, cb, user_data)`.
- **No file open / save dialog.** Fl_Native_File_Chooser is in the umbrella; the call sequence (`fnfc.show()` → `fnfc.filename()` → buffer.loadfile) is straightforward to add once menus are wired.
- **No syntax highlighting.** `Fl_Text_Display` supports it via `Style_Table_Entry[]`; out of scope for the initial smoke test.
