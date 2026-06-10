//! ADVANCED FLTK editor — **Rust subclasses FLTK widgets**.
//!
//! Where `examples/fltk_text_editor` *composes* imported FLTK widgets,
//! this demo *subclasses* them with the fork's `class` keyword and
//! proves the cross-boundary machinery end to end:
//!
//!   - `class RustEditor : Fl_Text_Editor` — subclasses the DEEPEST
//!     level of the imported 4-level chain (Fl_Text_Editor →
//!     Fl_Text_Display → Fl_Group → Fl_Widget) and overrides virtuals
//!     introduced at different ancestors: `handle` (Fl_Widget, final
//!     overrider Fl_Text_Editor), `draw` (pure on Fl_Widget, final
//!     overrider the *protected* Fl_Text_Display::draw), `resize`
//!     (Fl_Widget, final overrider Fl_Text_Display).
//!   - `class StatusBox : Fl_Box` — a second, independent Rust
//!     subclass in the same binary (its own vtable + RTTI chain).
//!   - **super-calls** — each override delegates to the C++ base impl
//!     via a direct `#[link_name]` symbol call (non-virtual, so no
//!     dispatch loop; also bypasses C++ access control, which is what
//!     lets Rust extend the *protected* `Fl_Text_Display::draw`).
//!   - **virtual destructor** — the C++ side `delete`s the Rust
//!     objects through `Fl_Widget*`; Rust `Drop` + the full FLTK
//!     destructor chain run exactly once.
//!   - **editor features** in the `handle` override: Ctrl/Cmd+D
//!     duplicates the current line, Ctrl/Cmd+'='/'-' changes the font
//!     size, and a live status bar tracks events + buffer length.
//!
//! Two modes:
//!
//! ```sh
//! cargo +rustcc run --release --bin editor -- --self-test   # headless probes
//! cargo +rustcc run --release --bin editor                  # the GUI
//! ```


include!(concat!(env!("CARGO_MANIFEST_DIR"), "/target/gen-out/bindings.rs"));

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering::Relaxed};

// ------------------------------------------------------------------
// Instrumentation: every Rust override bumps a counter so the
// self-test can prove C++ dispatch landed in Rust.
// ------------------------------------------------------------------
static HANDLE_CALLS: AtomicI32 = AtomicI32::new(0);
static DRAW_CALLS: AtomicI32 = AtomicI32::new(0);
static RESIZE_CALLS: AtomicI32 = AtomicI32::new(0);
static SB_DRAW_CALLS: AtomicI32 = AtomicI32::new(0);
static EDITOR_DROPS: AtomicI32 = AtomicI32::new(0);
static STATUS_DROPS: AtomicI32 = AtomicI32::new(0);
/// Headless mode: overrides skip the base draw call (drawing needs a
/// live graphics context) but still count — dispatch is what's tested.
static TEST_MODE: AtomicBool = AtomicBool::new(false);
/// The GUI status bar, updated from inside `RustEditor::handle`.
static STATUS_PTR: AtomicPtr<StatusBox> = AtomicPtr::new(core::ptr::null_mut());

// ------------------------------------------------------------------
// Super-calls: direct (non-virtual) calls to the C++ base impls via
// their mangled symbols. `#[link_name]` bypasses both the vtable (no
// dispatch loop back into the override) and C++ access control
// (Fl_Text_Display::draw is protected — overriding + chaining to a
// protected virtual is exactly the FLTK custom-widget idiom).
// ------------------------------------------------------------------
unsafe extern "C++" {
    #[link_name = "_ZN14Fl_Text_Editor6handleEi"]
    fn base_editor_handle(this: *mut Fl_Text_Editor, ev: i32) -> i32;
    #[link_name = "_ZN15Fl_Text_Display4drawEv"]
    fn base_display_draw(this: *mut Fl_Text_Display);
    #[link_name = "_ZN15Fl_Text_Display6resizeEiiii"]
    fn base_display_resize(this: *mut Fl_Text_Display, x: i32, y: i32, w: i32, h: i32);
    #[link_name = "_ZN6Fl_Box4drawEv"]
    fn base_box_draw(this: *mut Fl_Box);
    #[link_name = "_Znwm"] // C++ operator new(size_t) — C++ owns + deletes
    fn cxx_operator_new(size: usize) -> *mut u8;
}

// C++ helpers (cpp/helpers.cpp): dispatch probes through BASE
// pointers + construction self-checks.
unsafe extern "C" {
    fn rde_dispatch_handle(w: *mut Fl_Widget, ev: i32) -> i32;
    fn rde_dispatch_resize(w: *mut Fl_Widget, x: i32, y: i32, wd: i32, h: i32);
    fn rde_dispatch_draw(w: *mut Fl_Widget);
    fn rde_delete_widget(w: *mut Fl_Widget);
    fn rde_clear_current_group();
    fn rde_children_parent_ok(g: *mut Fl_Group) -> i32;
    fn rde_child_count(g: *mut Fl_Group) -> i32;
    fn rde_fix_children_parent(g: *mut Fl_Group);
    fn rde_color(c: u32);
    fn rde_rectf(x: i32, y: i32, w: i32, h: i32);
    fn free(p: *mut ::core::ffi::c_void); // for Fl_Text_Buffer::text_range results
}

// FLTK event constants. FL_KEYDOWN is in the bindings (enum const);
// the modifier masks are `#define`s the macro importer doesn't reach,
// so they're mirrored here (FL/Enumerations.H).
const EV_KEYDOWN: i32 = 8; // FL_KEYDOWN / FL_KEYBOARD
const MOD_CTRL: i32 = 0x0004_0000; // FL_CTRL
const MOD_META: i32 = 0x0040_0000; // FL_META (macOS Cmd)
/// Self-test sentinel event: the override answers it directly, so the
/// probe needs no synthesized FLTK event state.
const EV_PROBE: i32 = 7001;
const PROBE_ANSWER: i32 = 4242;

// ------------------------------------------------------------------
// The Rust subclasses
// ------------------------------------------------------------------

pub class RustEditor : Fl_Text_Editor {
    keystrokes: i32,

    pub constructor fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        RustEditor {
            __base: Fl_Text_Editor::new(x, y, w, h, ::core::ptr::null()),
            keystrokes: 0,
        }
    }

    // Override of `Fl_Widget::handle` (final overrider in C++ was
    // `Fl_Text_Editor::handle`). Adds editor features on top of the
    // stock key bindings, then super-calls the C++ impl.
    pub override fn handle(&self, ev: i32) -> i32 {
        HANDLE_CALLS.fetch_add(1, Relaxed);
        if ev == EV_PROBE {
            return PROBE_ANSWER;
        }
        let this = self as *const Self as *mut Fl_Text_Editor;
        if ev == EV_KEYDOWN {
            let key = Fl::event_key();
            let mods = Fl::event_state() & (MOD_CTRL | MOD_META);
            if mods != 0 {
                match key as u8 {
                    b'd' => unsafe {
                        duplicate_current_line(this);
                        update_status(this);
                        return 1;
                    },
                    b'=' | b'+' => unsafe {
                        bump_textsize(this, 2);
                        return 1;
                    },
                    b'-' => unsafe {
                        bump_textsize(this, -2);
                        return 1;
                    },
                    _ => {}
                }
            }
        }
        // Super-call: the stock Fl_Text_Editor behavior (cursor,
        // selection, default key bindings, mouse).
        let r = unsafe { base_editor_handle(this, ev) };
        unsafe { update_status(this) };
        r
    }

    // Override of the chain's `draw` — pure on `Fl_Widget`, final
    // overrider in C++ the *protected* `Fl_Text_Display::draw`.
    pub override fn draw(&self) {
        DRAW_CALLS.fetch_add(1, Relaxed);
        if TEST_MODE.load(Relaxed) {
            return; // headless: no graphics context to draw into
        }
        let this = self as *const Self as *mut Fl_Text_Display;
        unsafe { base_display_draw(this) };
    }

    // Override of `Fl_Widget::resize` (final overrider in C++ was
    // `Fl_Text_Display::resize`).
    pub override fn resize(&self, x: i32, y: i32, w: i32, h: i32) {
        RESIZE_CALLS.fetch_add(1, Relaxed);
        let this = self as *const Self as *mut Fl_Text_Display;
        unsafe { base_display_resize(this, x, y, w, h) };
    }

    // A NEW virtual introduced by the Rust class (appends a slot
    // after the inherited C++ ones).
    pub virtual fn rust_only(&self) -> i32 {
        self.keystrokes + 1000
    }
}

impl Drop for RustEditor {
    fn drop(&mut self) {
        EDITOR_DROPS.fetch_add(1, Relaxed);
    }
}

/// Second, independent Rust subclass: a status bar on the shallow
/// `Fl_Box : Fl_Widget` chain. Overrides `draw` (counted), chaining to
/// `Fl_Box::draw` for the actual box + label rendering.
pub class StatusBox : Fl_Box {
    revision: i32,

    pub constructor fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        StatusBox {
            __base: Fl_Box::new(x, y, w, h, ::core::ptr::null()),
            revision: 0,
        }
    }

    pub override fn draw(&self) {
        SB_DRAW_CALLS.fetch_add(1, Relaxed);
        if TEST_MODE.load(Relaxed) {
            return;
        }
        let this = self as *const Self as *mut Fl_Box;
        unsafe {
            base_box_draw(this);
            // v1.14 paint probe: the free-function drawing API from
            // inside a Rust override — fl_color + fl_rectf + fl_draw.
            let w = this as *mut Fl_Widget;
            let (x, y) = ((*w).x(), (*w).y());
            // fl_color/fl_rectf are header-inline free fns (no symbol;
            // free-fn inline-shim routing is tracked) -> C++ helpers.
            rde_color(0x00C00000); // 0xRRGGBB00 green
            rde_rectf(x + 4, y + 6, 10, 10);
            rde_color(0x00000000);
            fl_draw(c"rust-draw".as_ptr(), x + 20, y + 16);
        }
    }
}

impl Drop for StatusBox {
    fn drop(&mut self) {
        STATUS_DROPS.fetch_add(1, Relaxed);
    }
}

// ------------------------------------------------------------------
// Editor features used from the `handle` override
// ------------------------------------------------------------------

/// Ctrl/Cmd+D: duplicate the current line below itself.
unsafe fn duplicate_current_line(ed: *mut Fl_Text_Editor) {
    unsafe {
        let disp = ed as *mut Fl_Text_Display;
        let pos = (*disp).insert_position_ovl();
        let buf = (*disp).buffer_ovl();
        if buf.is_null() {
            return;
        }
        let ls = (*buf).line_start(pos);
        let le = (*buf).line_end(pos);
        let raw = (*buf).text_range(ls, le); // malloc'd C string
        if raw.is_null() {
            return;
        }
        let line = ::core::ffi::CStr::from_ptr(raw).to_string_lossy().into_owned();
        free(raw as *mut ::core::ffi::c_void);
        let dup = format!("\n{line}");
        (*buf).insert_str(le, &dup, dup.len() as i32);
    }
}

/// Ctrl/Cmd+'='/'-': grow/shrink the editor font.
unsafe fn bump_textsize(ed: *mut Fl_Text_Editor, delta: i32) {
    unsafe {
        let disp = ed as *mut Fl_Text_Display;
        let size = ((*disp).textsize() + delta).clamp(6, 48);
        (*disp).textsize_i32(size);
        (*(ed as *mut Fl_Widget)).redraw();
    }
}

/// Refresh the status bar from inside the `handle` override.
unsafe fn update_status(ed: *mut Fl_Text_Editor) {
    let status = STATUS_PTR.load(Relaxed);
    if status.is_null() {
        return;
    }
    unsafe {
        let disp = ed as *mut Fl_Text_Display;
        let buf = (*disp).buffer_ovl();
        let len = if buf.is_null() { 0 } else { (*buf).length() };
        let pos = (*disp).insert_position_ovl();
        let events = HANDLE_CALLS.load(Relaxed);
        let label = format!(
            "len {len} | cursor {pos} | events {events} | Rust overrides: \
             draw {} resize {}",
            DRAW_CALLS.load(Relaxed),
            RESIZE_CALLS.load(Relaxed),
        );
        let w = status as *mut Fl_Widget;
        (*w).copy_label_str(&label);
        (*w).redraw();
    }
}

// ------------------------------------------------------------------
// Headless self-test: construct on the heap, dispatch every override
// through C++ BASE-class pointers, delete through Fl_Widget*, verify
// counters. No window, CI-able.
// ------------------------------------------------------------------
unsafe fn self_test() -> i32 {
    TEST_MODE.store(true, Relaxed);
    let mut failures = 0;
    let mut check = |name: &str, got: i32, want: i32| {
        if got == want {
            println!("ok   {name} = {got}");
        } else {
            println!("FAIL {name}: got {got}, want {want}");
            failures += 1;
        }
    };

    unsafe {
        // No current group: widgets must not self-register into a
        // group while being constructed in a Rust temporary.
        rde_clear_current_group();

        // C++ owns the object: allocate via operator new, construct,
        // and let C++ `delete` it through a base pointer.
        let ed = cxx_operator_new(::core::mem::size_of::<RustEditor>()) as *mut RustEditor;
        ed.write(RustEditor::new(10, 10, 400, 300));

        // Construct-then-move probe: Fl_Text_Display's ctor created
        // child scrollbars whose parent_ pointed at the construction
        // temporary. Rust does NOT guarantee eliding the move from
        // `RustEditor::new`'s return into `ed.write(..)` — and in
        // practice it does not elide here, so the back-pointers
        // dangle until repaired. This probe makes the hazard VISIBLE,
        // then verifies the documented fix-up (re-parent the children
        // at the final address via FLTK's public parent() setter).
        let before = rde_children_parent_ok(ed as *mut Fl_Group);
        println!(
            "info construct-then-move: children parent ptrs {} the move \
             (child_count = {})",
            if before == 1 { "SURVIVED" } else { "DANGLED after" },
            rde_child_count(ed as *mut Fl_Group),
        );
        rde_fix_children_parent(ed as *mut Fl_Group);
        check(
            "children_parent_ok (after fix-up)",
            rde_children_parent_ok(ed as *mut Fl_Group),
            1,
        );

        // C++ virtual dispatch through the ROOT base pointer lands in
        // the Rust overrides.
        check(
            "dispatch handle via Fl_Widget*",
            rde_dispatch_handle(ed as *mut Fl_Widget, EV_PROBE),
            PROBE_ANSWER,
        );
        rde_dispatch_resize(ed as *mut Fl_Widget, 5, 5, 500, 400);
        check("resize override ran", RESIZE_CALLS.load(Relaxed), 1);
        rde_dispatch_draw(ed as *mut Fl_Widget);
        check("draw override ran", DRAW_CALLS.load(Relaxed), 1);

        // The Rust-introduced virtual works alongside inherited slots.
        check("rust_only()", (*ed).rust_only(), 1000);

        // Second subclass, same binary.
        let sb = cxx_operator_new(::core::mem::size_of::<StatusBox>()) as *mut StatusBox;
        sb.write(StatusBox::new(0, 0, 100, 20));
        rde_dispatch_draw(sb as *mut Fl_Widget);
        check("StatusBox draw override ran", SB_DRAW_CALLS.load(Relaxed), 1);

        // Virtual destructor through the root base pointer: Rust Drop
        // + the full FLTK dtor chain + operator delete, exactly once.
        rde_delete_widget(ed as *mut Fl_Widget);
        check("RustEditor dropped", EDITOR_DROPS.load(Relaxed), 1);
        rde_delete_widget(sb as *mut Fl_Widget);
        check("StatusBox dropped", STATUS_DROPS.load(Relaxed), 1);
    }

    if failures == 0 {
        println!("\nADVANCED FLTK SUBCLASS SELF-TEST: ALL OK");
        0
    } else {
        println!("\nADVANCED FLTK SUBCLASS SELF-TEST: {failures} FAILURE(S)");
        1
    }
}

// ------------------------------------------------------------------
// GUI mode
// ------------------------------------------------------------------
unsafe fn run_gui() -> i32 {
    unsafe {
        let mut window = Fl_Window::new_cstr(900, 640, c"rustcc — Rust subclasses FLTK");
        // Detach the implicit current-group capture BEFORE building
        // the Rust widgets, then parent them explicitly at their
        // final heap addresses (see README: construct-then-move).
        window.as_fl_group_mut().end();
        rde_clear_current_group();

        // The buffer outlives the widgets; Box gives it a stable address.
        let buffer = Box::into_raw(Box::new(Fl_Text_Buffer::new_with_defaults()));
        (*buffer).text_const_i8_str(
            "// rustcc ADVANCED editor — this widget is a Rust `class`\n\
             // subclassing FLTK's Fl_Text_Editor (a 4-level imported\n\
             // C++ chain). Every keystroke dispatches C++ -> Rust\n\
             // through the vtable slot the fork emitted.\n\
             //\n\
             //   Ctrl/Cmd+D       duplicate current line\n\
             //   Ctrl/Cmd+= / -   grow / shrink font\n\
             //\n\
             // The status bar below is a SECOND Rust subclass\n\
             // (StatusBox : Fl_Box) whose draw() override chains to\n\
             // the C++ base impl after counting the call.\n",
        );

        let ed = cxx_operator_new(::core::mem::size_of::<RustEditor>()) as *mut RustEditor;
        ed.write(RustEditor::new(10, 10, 880, 580));
        // Repair the ctor-time self-references that the (non-elided)
        // Rust move invalidated, then verify — see README.
        rde_fix_children_parent(ed as *mut Fl_Group);
        assert_eq!(
            rde_children_parent_ok(ed as *mut Fl_Group),
            1,
            "construct-then-move fix-up failed — see README"
        );
        (*(ed as *mut Fl_Text_Display)).buffer(buffer);
        (*(ed as *mut Fl_Text_Display)).linenumber_width(36);

        let sb = cxx_operator_new(::core::mem::size_of::<StatusBox>()) as *mut StatusBox;
        sb.write(StatusBox::new(10, 600, 880, 30));
        (*(sb as *mut Fl_Widget)).copy_label_str("ready — type away");
        STATUS_PTR.store(sb, Relaxed);

        // Parent the Rust widgets at their final addresses. The
        // window now owns them: closing it runs ~Fl_Group, which
        // deletes the children through Fl_Widget* — the virtual-dtor
        // path back into Rust Drop.
        window.as_fl_group_mut().add(ed as *mut Fl_Widget);
        window.as_fl_group_mut().add(sb as *mut Fl_Widget);

        window.show();
        let rc = Fl::run();

        // `window` (a Rust value) drops here: ~Fl_Window -> ~Fl_Group
        // deletes the two Rust children through their vtable dtors.
        drop(window);
        println!(
            "exit: rc={rc} | handle={} draw={} resize={} sb_draw={} | drops: editor={} status={}",
            HANDLE_CALLS.load(Relaxed),
            DRAW_CALLS.load(Relaxed),
            RESIZE_CALLS.load(Relaxed),
            SB_DRAW_CALLS.load(Relaxed),
            EDITOR_DROPS.load(Relaxed),
            STATUS_DROPS.load(Relaxed),
        );
        if EDITOR_DROPS.load(Relaxed) == 1 && STATUS_DROPS.load(Relaxed) == 1 {
            println!("virtual-dtor chain through Fl_Group teardown: OK");
            0
        } else {
            println!("FAIL: Rust Drops did not run via FLTK teardown");
            1
        }
    }
}

fn main() {
    let wants_self_test = std::env::args().any(|a| a == "--self-test");
    let rc = unsafe {
        if wants_self_test {
            self_test()
        } else {
            run_gui()
        }
    };
    std::process::exit(rc);
}
