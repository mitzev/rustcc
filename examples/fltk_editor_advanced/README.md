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

## Construct-in-place (v1.14)

C++ constructors may *escape `this`*: `Fl_Text_Display`'s ctor creates
child scrollbars whose `parent_` back-pointers capture the address of
the object under construction. Plain Rust builds a value in a temporary
and bitwise-moves it home, which would leave those back-pointers
dangling. Since v1.14 the fork's **ctor-in-place MIR pass** removes the
temporaries along the whole chain — `ed.write(RustEditor::new(..))`
constructs directly into `*ed`, the `Self { __base: Base::new(..) }`
aggregate constructs the base directly into the base subobject, and the
imported binding's by-value `new` constructs into its return slot — so
ctor-time self-references are born at the final address. The self-test
asserts this (`children_parent_ok (no fix-up)`); no re-parent fix-up or
`new_at` workaround is needed for the Rust-`class` path anymore.

Widgets are still heap-placed via `operator new` (C++ `delete`s them
through the base pointer), and the window's implicit group capture is
cleared before constructing Rust widgets so they don't self-register
mid-construction.

One more build note: FLTK is linked as the **static archive by
absolute path** — the Homebrew dylib hides inline symbols (e.g.
`~Fl_Text_Editor()`), and `-lfltk` would pick the dylib.

(History of removed workarounds: v1.13.10 dropped the drop-glue force
function — the collector now emits vtable-referenced `drop_in_place`
at every opt level — and the `codegen-units = 1` pin. v1.14 dropped the
construct-then-move re-parent fix-up: the ctor-in-place MIR pass now
constructs at the final address. `new_at` placement constructors remain
available in the bindings for C++-side placement scenarios.)
