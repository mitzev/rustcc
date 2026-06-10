# fltk_text_editor — a TextEdit-style editor in Rust-subclassed FLTK

A Mac-TextEdit-equivalent plain-text editor and the fork's broadest
application testbed: menu bar with shortcuts (File/Edit/Format), native
open/save dialogs, undo/redo, find bar, word wrap, font sizing, and an
"— Edited" dirty title — built on THREE Rust `class` subclasses
(`RustEditor : Fl_Text_Editor`, `FindBar : Fl_Input`) plus C++→Rust
function-pointer callbacks (menu dispatch, buffer modify callback).

Building it surfaced and fixed four fork/importer bugs (v1.13.10+):
the Itanium `PF…E` function-type mangling (63 unlinkable callback
symbols), the `Ptr{Fn}` model collapse that dropped the `P` from
fn-pointer-typedef params, `Option<fn>` default-arg synthesis, and —
found by `Fl_Window::show()` silently no-oping — the construct-then-
move hazard on window drivers, solved with the `new_at` placement
constructors. Known limitation it documents: header-INLINE C++ methods
(e.g. `Fl_Widget::callback`) have no out-of-line symbol for
DirectExternCpp to bind — the `FindBar` subclass routes around it.

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

> **Status (v1.13.x):** step 1 (binding generation) works against
> FLTK 1.4.5 with the current `cxx_importer`. Step 2 (the `editor`
> bin) is being reconciled with the much-advanced importer — its
> output for FLTK's *full* surface still references a few nested types
> it doesn't yet emit at the right scope (`Key_Binding`, `matrix`,
> …). Recent importer-robustness fixes landed along the way
> (standalone `OUT_DIR`/`TARGET` defaults in `Build::compile`,
> `pub(crate)` statics for crate-root `include!`, enum/alias name
> dedup). Tracking the remaining work to a green editor build as a
> follow-up.

## Subclassing an FLTK widget from Rust (v1.13.7)

Before v1.13.7 this example could only *compose* FLTK widgets (hold a
C++ object and call its methods). As of v1.13.7 the fork supports the
other direction — a Rust `class` that **subclasses** an imported C++
polymorphic widget and overrides its virtuals, with C++ dispatching
into the Rust override and `delete` running the Rust `Drop`:

```rust
// Fl_Widget is imported by gen_bindings; cxx_importer emits the
// #[rustc_cxx_imported_vtable] attribute (carrying its virtual slots +
// virtual destructor) that lets rustcc extend the C++ vtable.
pub class MyButton : Fl_Widget {
    clicks: u32,

    pub constructor fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        MyButton { __base: Fl_Widget::new(x, y, w, h), clicks: 0 }
    }

    // FLTK calls handle()/draw() through an Fl_Widget* — they land here.
    pub override fn handle(&mut self, event: i32) -> i32 { self.clicks += 1; 1 }
    pub override fn draw(&self) { /* custom drawing */ }
}
```

When FLTK owns the widget (added to a group) and `delete`s it, the Rust
`Drop` runs and the storage is reclaimed — see `docs/repr_cpp.md §5`.

**Runnable, tested proof of the mechanism:** `examples/subclass_cpp_base/`
exercises exactly this (override of a concrete *and* a pure virtual,
plus a virtual destructor) end-to-end against a clang-compiled base, on
`aarch64-apple-darwin` and `x86_64-apple-darwin`.

**Scope note:** v1.13.7 supports **single-level** inheritance from a
*root* polymorphic base (`class D : Base`, `Base` itself has no
polymorphic base). FLTK's deeper chains (`Fl_Text_Editor → Fl_Text_Display
→ Fl_Group → Fl_Widget`) and multiple inheritance are a follow-up; a Rust
subclass should target a root FLTK base (e.g. `Fl_Widget`) for now. The
full GUI editor bin (step 2) is still being reconciled with the importer
(see the status note above), so this section documents the pattern; the
self-contained proof lives in `subclass_cpp_base`.

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
