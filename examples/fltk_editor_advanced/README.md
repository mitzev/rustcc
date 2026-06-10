# fltk_editor_advanced — Rust **subclasses** FLTK widgets

Where [`../fltk_text_editor`](../fltk_text_editor) *composes* imported
FLTK widgets, this demo *subclasses* them with the fork's `class`
keyword — the canonical FLTK custom-widget pattern, written in Rust:

```
Fl_Widget                       (virtual ~, pure draw, handle, resize, …)
  └─ Fl_Group                   (protected on_insert/on_move/on_remove hooks)
       └─ Fl_Text_Display       (protected draw override, resize)
            └─ Fl_Text_Editor   (handle override)
                 └─ RustEditor  ← Rust `class`, overrides handle + draw + resize

Fl_Widget └─ Fl_Box └─ StatusBox  ← second Rust `class`, overrides draw
```

## What it proves

- **Deep-chain dispatch**: C++ calling `handle`/`draw`/`resize` through
  an `Fl_Widget*` (the chain root) lands in the Rust `override fn`s.
- **Protected virtuals**: `draw()` is pure on `Fl_Widget` and its C++
  final overrider (`Fl_Text_Display::draw`) is *protected* — the
  importer keeps non-public virtuals as vtable slots, and the Rust
  override chains to the protected base impl via a direct
  `#[link_name]` super-call (linker symbols don't do access control).
- **Virtual destructor**: the window's `~Fl_Group` deletes the Rust
  widgets through `Fl_Widget*`; Rust `Drop` + the FLTK destructor
  chain + `operator delete` each run exactly once.
- **Two independent Rust subclasses** in one binary, plus a new Rust
  `virtual fn` appended after the inherited C++ slots.
- **Editor features implemented in the override**: Ctrl/Cmd+D
  duplicates the current line (Fl_Text_Buffer line ops), Ctrl/Cmd+= / -
  changes the font size, and a live `StatusBox` shows buffer length,
  cursor, and override-dispatch counters.

## Build + run

```sh
cd examples/fltk_editor_advanced

# 1. Bindings + shim archive (stock toolchain; needs libclang + FLTK):
cargo run --release --bin gen_bindings

# 2. The editor (rustcc fork toolchain):
cargo +rustcc run --release --bin editor -- --self-test   # headless probes
cargo +rustcc run --release --bin editor                  # the GUI
```

Expected self-test tail: `ADVANCED FLTK SUBCLASS SELF-TEST: ALL OK`.

## The construct-then-move caveat (read this)

A Rust constructor builds the object in a temporary and **bitwise-moves**
it to its final address; Rust does *not* guarantee eliding that move.
`Fl_Text_Display`'s C++ constructor creates child scrollbars whose
`parent_` back-pointers capture the temporary's address — after the
move they dangle. This demo makes the hazard **visible** (the self-test
prints whether the pointers survived) and repairs it by re-parenting
the children at the final address (`Fl_Widget::parent(Fl_Group*)`,
FLTK's documented "for hacks only" setter) before the widget is used.

Generalizing beyond FLTK: any C++ base whose constructor *escapes
`this`* needs either such a fix-up or a construct-at-final-address
primitive (`new_at`) — tracked as a fork improvement. The same is why
widgets are heap-placed via `operator new` (C++ deletes them) and the
window's implicit group capture is cleared before constructing Rust
widgets.

Two more workarounds this example documents:

- `__rde_force_drop_glue`: at `-C opt-level=3` the collector's root for
  the classes' `drop_in_place` (referenced by the vtable's deleting-dtor
  thunk) fails to materialize; an exported function with a real use
  forces it.
- FLTK is linked as the **static archive by absolute path**: the
  Homebrew dylib hides inline symbols (e.g. `~Fl_Text_Editor()`), and
  `-lfltk` would pick the dylib.
