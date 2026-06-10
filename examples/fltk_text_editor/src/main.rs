//! FLTK text editor — a Mac-TextEdit-style plain-text editor, built to
//! stress the rustcc fork with real application surface:
//!
//!   - **Menu bar** (`Fl_Menu_::add` — C function-pointer callback
//!     params; exercised the v1.13.10 Itanium `PF…E` mangler fix and
//!     the `Ptr{Fn}` double-wrap importer fix).
//!   - **Native open/save dialogs** (`Fl_Native_File_Chooser`).
//!   - **Undo/redo, find, word wrap, font size** (buffer + display API).
//!   - **Dirty-title tracking** via `Fl_Text_Buffer::add_modify_callback`
//!     (C++ → Rust function-pointer registration).
//!   - A **Rust `class RustEditor : Fl_Text_Editor`** subclass whose
//!     `handle` override adds Cmd+D duplicate-line on top of the stock
//!     bindings via a non-virtual super-call.
//!
//! ```sh
//! cargo run --release --bin gen_bindings          # stock toolchain + libclang
//! cargo +rustcc run --release --bin editor                  # the GUI
//! cargo +rustcc run --release --bin editor -- --self-test   # headless probes
//! ```


include!(concat!(env!("CARGO_MANIFEST_DIR"), "/target/m26-out/bindings.rs"));

use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering::Relaxed};
use std::sync::Mutex;

// ------------------------------------------------------------------
// Globals: the single-document app state (TextEdit model: one window).
// ------------------------------------------------------------------
static WIN: AtomicPtr<Fl_Window> = AtomicPtr::new(core::ptr::null_mut());
static ED: AtomicPtr<RustEditor> = AtomicPtr::new(core::ptr::null_mut());
static BUF: AtomicPtr<Fl_Text_Buffer> = AtomicPtr::new(core::ptr::null_mut());
static FIND: AtomicPtr<FindBar> = AtomicPtr::new(core::ptr::null_mut());
static DIRTY: AtomicBool = AtomicBool::new(false);
static MODIFY_EVENTS: AtomicI32 = AtomicI32::new(0);
static WRAP_ON: AtomicBool = AtomicBool::new(false);
static TEST_MODE: AtomicBool = AtomicBool::new(false);
static HANDLE_CALLS: AtomicI32 = AtomicI32::new(0);
static PATH: Mutex<Option<String>> = Mutex::new(None);

// FLTK constants — from the generated bindings (the M12 macro pass
// captures the FL_* `#define`s; v1.14). Narrowed to i32 once here.
const EV_KEYDOWN: i32 = 8; // FL_KEYDOWN (Fl_Event enum)
const MOD_CTRL: i32 = FL_CTRL as i32;
const MOD_META: i32 = FL_META as i32; // FL_COMMAND on macOS
const MOD_SHIFT: i32 = FL_SHIFT as i32;
const KEY_ENTER: i32 = FL_Enter as i32;
// Class-scope enums — generated bindings (v1.14): named nested enums
// emit as transparent structs with assoc consts; anonymous ones as
// prefixed plain consts.
const CHOOSER_OPEN: i32 = Fl_Native_File_Chooser_Type::BROWSE_FILE.0 as i32;
const CHOOSER_SAVE: i32 = Fl_Native_File_Chooser_Type::BROWSE_SAVE_FILE.0 as i32;
const WRAP_NONE: i32 = Fl_Text_Display_WRAP_NONE as i32;
const WRAP_AT_BOUNDS: i32 = Fl_Text_Display_WRAP_AT_BOUNDS as i32;

// Menu action ids (passed through the menu callback's user_data).
const ACT_NEW: usize = 1;
const ACT_OPEN: usize = 2;
const ACT_SAVE: usize = 3;
const ACT_SAVE_AS: usize = 4;
const ACT_QUIT: usize = 5;
const ACT_UNDO: usize = 10;
const ACT_REDO: usize = 11;
const ACT_CUT: usize = 12;
const ACT_COPY: usize = 13;
const ACT_PASTE: usize = 14;
const ACT_SELECT_ALL: usize = 15;
const ACT_FIND: usize = 16;
const ACT_WRAP: usize = 20;
const ACT_FONT_UP: usize = 21;
const ACT_FONT_DOWN: usize = 22;

// Super-calls (non-virtual, by mangled symbol).
unsafe extern "C++" {
    #[link_name = "_ZN14Fl_Text_Editor6handleEi"]
    fn base_editor_handle(this: *mut Fl_Text_Editor, ev: i32) -> i32;
    #[link_name = "_ZN15Fl_Text_Display4drawEv"]
    fn base_display_draw(this: *mut Fl_Text_Display);
    #[link_name = "_ZN15Fl_Text_Display6resizeEiiii"]
    fn base_display_resize(this: *mut Fl_Text_Display, x: i32, y: i32, w: i32, h: i32);
    #[link_name = "_ZN8Fl_Input6handleEi"]
    fn base_input_handle(this: *mut Fl_Input, ev: i32) -> i32;
    #[link_name = "_Znwm"]
    fn cxx_operator_new(size: usize) -> *mut u8;
}

// ------------------------------------------------------------------
// The Rust subclass: stock Fl_Text_Editor behavior + Cmd+D.
// ------------------------------------------------------------------
pub class RustEditor : Fl_Text_Editor {
    extra: i32,

    pub constructor fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        RustEditor {
            __base: Fl_Text_Editor::new(x, y, w, h, ::core::ptr::null()),
            extra: 0,
        }
    }

    pub override fn handle(&self, ev: i32) -> i32 {
        HANDLE_CALLS.fetch_add(1, Relaxed);
        let this = self as *const Self as *mut Fl_Text_Editor;
        if ev == EV_KEYDOWN {
            let key = Fl::event_key();
            let cmd = Fl::event_state() & (MOD_CTRL | MOD_META) != 0;
            if cmd && key as u8 == b'd' {
                unsafe { duplicate_current_line(this) };
                return 1;
            }
        }
        unsafe { base_editor_handle(this, ev) }
    }

    pub override fn draw(&self) {
        if TEST_MODE.load(Relaxed) {
            return;
        }
        unsafe { base_display_draw(self as *const Self as *mut Fl_Text_Display) };
    }

    pub override fn resize(&self, x: i32, y: i32, w: i32, h: i32) {
        unsafe { base_display_resize(self as *const Self as *mut Fl_Text_Display, x, y, w, h) };
    }
}

/// Third Rust subclass: the find bar. Catches Enter in its `handle`
/// override (FLTK's `Fl_Widget::callback` registration is
/// header-INLINE — no out-of-line symbol exists to bind, a known
/// DirectExternCpp limitation — and a subclass is the better fork
/// demo anyway: it extends a SHALLOW chain, Fl_Input_ : Fl_Widget).
pub class FindBar : Fl_Input {
    pad: i32,

    pub constructor fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        FindBar {
            __base: Fl_Input::new(x, y, w, h, c"Find:".as_ptr()),
            pad: 0,
        }
    }

    pub override fn handle(&self, ev: i32) -> i32 {
        let this = self as *const Self as *mut Fl_Input;
        if ev == EV_KEYDOWN && Fl::event_key() == KEY_ENTER {
            unsafe { find_next() };
            return 1;
        }
        unsafe { base_input_handle(this, ev) }
    }
}

// ------------------------------------------------------------------
// C++ → Rust callbacks
// ------------------------------------------------------------------

/// `Fl_Text_Buffer` modify callback: any insert/delete marks the
/// document dirty and refreshes the title (TextEdit's "Edited").
unsafe extern "C" fn modify_cb(
    _pos: i32,
    inserted: i32,
    deleted: i32,
    _restyled: i32,
    _deltext: *const i8,
    _user: *mut (),
) {
    MODIFY_EVENTS.fetch_add(1, Relaxed);
    if inserted > 0 || deleted > 0 {
        DIRTY.store(true, Relaxed);
        unsafe { refresh_title() };
    }
}

/// Single dispatcher for every menu item; the action id rides in
/// `user_data`.
unsafe extern "C" fn menu_cb(_w: *mut Fl_Widget, user: *mut ()) {
    unsafe { run_action(user as usize) };
}

unsafe fn run_action(act: usize) {
    unsafe {
        let ed = ED.load(Relaxed);
        let buf = BUF.load(Relaxed);
        match act {
            ACT_NEW => {
                (*buf).text_const_i8_str("");
                *PATH.lock().unwrap() = None;
                DIRTY.store(false, Relaxed);
                refresh_title();
            }
            ACT_OPEN => {
                if let Some(p) = choose_file(CHOOSER_OPEN, "Open") {
                    (*buf).loadfile_with_defaults(cstr(&p).as_ptr());
                    *PATH.lock().unwrap() = Some(p);
                    DIRTY.store(false, Relaxed);
                    refresh_title();
                }
            }
            ACT_SAVE => {
                let existing = PATH.lock().unwrap().clone();
                match existing {
                    Some(p) => save_to(&p),
                    None => {
                        if let Some(p) = choose_file(CHOOSER_SAVE, "Save") {
                            save_to(&p);
                            *PATH.lock().unwrap() = Some(p);
                        }
                    }
                }
                refresh_title();
            }
            ACT_SAVE_AS => {
                if let Some(p) = choose_file(CHOOSER_SAVE, "Save As") {
                    save_to(&p);
                    *PATH.lock().unwrap() = Some(p);
                    refresh_title();
                }
            }
            ACT_QUIT => std::process::exit(0),
            ACT_UNDO => {
                (*buf).undo_with_defaults();
            }
            ACT_REDO => {
                (*buf).redo_with_defaults();
            }
            ACT_CUT => {
                Fl_Text_Editor::kf_cut(0, ed as *mut Fl_Text_Editor);
            }
            ACT_COPY => {
                Fl_Text_Editor::kf_copy(0, ed as *mut Fl_Text_Editor);
            }
            ACT_PASTE => {
                Fl_Text_Editor::kf_paste(0, ed as *mut Fl_Text_Editor);
            }
            ACT_SELECT_ALL => {
                (*buf).select(0, (*buf).length());
            }
            ACT_FIND => {
                let f = FIND.load(Relaxed);
                if !f.is_null() {
                    (*(f as *mut Fl_Widget)).take_focus();
                }
            }
            ACT_WRAP => {
                let on = !WRAP_ON.load(Relaxed);
                WRAP_ON.store(on, Relaxed);
                let disp = ed as *mut Fl_Text_Display;
                (*disp).wrap_mode(if on { WRAP_AT_BOUNDS } else { WRAP_NONE }, 0);
                (*(ed as *mut Fl_Widget)).redraw();
            }
            ACT_FONT_UP => bump_textsize(ed as *mut Fl_Text_Editor, 2),
            ACT_FONT_DOWN => bump_textsize(ed as *mut Fl_Text_Editor, -2),
            _ => {}
        }
    }
}

// ------------------------------------------------------------------
// Editor features
// ------------------------------------------------------------------

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

unsafe fn save_to(path: &str) {
    unsafe {
        let buf = BUF.load(Relaxed);
        let len = (*buf).length();
        (*buf).outputfile_with_defaults(cstr(path).as_ptr(), 0, len);
        DIRTY.store(false, Relaxed);
    }
}

unsafe fn choose_file(ty: i32, title: &str) -> Option<String> {
    unsafe {
        let mut ch = Fl_Native_File_Chooser::new(ty);
        ch.title_str(title);
        if ch.show() != 0 {
            return None; // cancelled / error
        }
        let p = ch.filename();
        if p.is_null() {
            return None;
        }
        Some(CStr::from_ptr(p).to_string_lossy().into_owned())
    }
}

unsafe fn refresh_title() {
    unsafe {
        let win = WIN.load(Relaxed);
        if win.is_null() {
            return;
        }
        let name = PATH
            .lock()
            .unwrap()
            .clone()
            .map(|p| p.rsplit('/').next().unwrap_or("?").to_string())
            .unwrap_or_else(|| "Untitled".to_string());
        let title = if DIRTY.load(Relaxed) {
            format!("{name} — Edited")
        } else {
            name
        };
        (*(win as *mut Fl_Widget)).copy_label_str(&title);
    }
}

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
        let raw = (*buf).text_range(ls, le);
        if raw.is_null() {
            return;
        }
        let line = CStr::from_ptr(raw).to_string_lossy().into_owned();
        libc_free(raw as *mut ::core::ffi::c_void);
        let dup = format!("\n{line}");
        (*buf).insert_str(le, &dup, dup.len() as i32);
    }
}

unsafe fn bump_textsize(ed: *mut Fl_Text_Editor, delta: i32) {
    unsafe {
        let disp = ed as *mut Fl_Text_Display;
        let size = ((*disp).textsize() + delta).clamp(6, 48);
        (*disp).textsize_i32(size);
        (*(ed as *mut Fl_Widget)).redraw();
    }
}

unsafe fn find_next() {
    unsafe {
        let buf = BUF.load(Relaxed);
        let ed = ED.load(Relaxed);
        let f = FIND.load(Relaxed);
        if buf.is_null() || ed.is_null() || f.is_null() {
            return;
        }
        let needle = (*(f as *mut Fl_Input)).as_fl_input_().value_ovl();
        if needle.is_null() {
            return;
        }
        let needle = CStr::from_ptr(needle).to_string_lossy().into_owned();
        if needle.is_empty() {
            return;
        }
        let disp = ed as *mut Fl_Text_Display;
        let start = (*disp).insert_position_ovl();
        let mut found: i32 = 0;
        let c = cstr(&needle);
        // wrap-around search
        let hit = (*buf).search_forward(start, c.as_ptr(), &mut found as *mut i32, 0) != 0
            || (*buf).search_forward(0, c.as_ptr(), &mut found as *mut i32, 0) != 0;
        if hit {
            (*buf).select(found, found + needle.len() as i32);
            (*disp).insert_position(found + needle.len() as i32);
            (*disp).show_insert_position();
            (*(ed as *mut Fl_Widget)).redraw();
        }
    }
}

unsafe extern "C" {
    #[link_name = "free"]
    fn libc_free(p: *mut ::core::ffi::c_void);
}

// ------------------------------------------------------------------
// UI assembly
// ------------------------------------------------------------------

unsafe fn add_menu_items(bar: *mut Fl_Menu_Bar) {
    unsafe {
        let m = (*bar).as_fl_menu__mut();
        let mut add = |label: &str, shortcut: i32, act: usize| {
            m.add(
                cstr(label).as_ptr(),
                shortcut,
                Some(menu_cb),
                act as *mut (),
                0,
            );
        };
        add("&File/&New", MOD_META | 'n' as i32, ACT_NEW);
        add("&File/&Open…", MOD_META | 'o' as i32, ACT_OPEN);
        add("&File/&Save", MOD_META | 's' as i32, ACT_SAVE);
        add("&File/Save &As…", MOD_META | MOD_SHIFT | 's' as i32, ACT_SAVE_AS);
        add("&File/&Quit", MOD_META | 'q' as i32, ACT_QUIT);
        add("&Edit/&Undo", MOD_META | 'z' as i32, ACT_UNDO);
        add("&Edit/&Redo", MOD_META | MOD_SHIFT | 'z' as i32, ACT_REDO);
        add("&Edit/Cu&t", MOD_META | 'x' as i32, ACT_CUT);
        add("&Edit/&Copy", MOD_META | 'c' as i32, ACT_COPY);
        add("&Edit/&Paste", MOD_META | 'v' as i32, ACT_PASTE);
        add("&Edit/Select &All", MOD_META | 'a' as i32, ACT_SELECT_ALL);
        add("&Edit/&Find…", MOD_META | 'f' as i32, ACT_FIND);
        add("F&ormat/&Wrap Lines", MOD_META | 'w' as i32, ACT_WRAP);
        add("F&ormat/Bigger", MOD_META | '=' as i32, ACT_FONT_UP);
        add("F&ormat/Smaller", MOD_META | '-' as i32, ACT_FONT_DOWN);
    }
}

unsafe fn build_ui() -> *mut Fl_Window {
    unsafe {
        // new_at: Fl_Window's ctor creates a platform window-driver
        // that CAPTURES `this` — a by-value construct + move leaves
        // the driver pointing at the dead temporary and `show()`
        // silently no-ops (shown() stays 0). Construct at the final
        // heap address instead (v1.13.10 placement ctors).
        let win = cxx_operator_new(core::mem::size_of::<Fl_Window>()) as *mut Fl_Window;
        Fl_Window::new_at(win, 900, 700, c"Untitled".as_ptr());
        (*win).as_fl_group_mut().end();
        // No implicit group capture while heap-placing the Rust widgets.
        Fl_Group::current_mut_fl_group(core::ptr::null_mut());

        let buf = cxx_operator_new(core::mem::size_of::<Fl_Text_Buffer>()) as *mut Fl_Text_Buffer;
        Fl_Text_Buffer::new_at(buf, 0, 1024);
        BUF.store(buf, Relaxed);
        (*buf).add_modify_callback(Some(modify_cb), core::ptr::null_mut());

        let bar = cxx_operator_new(core::mem::size_of::<Fl_Menu_Bar>()) as *mut Fl_Menu_Bar;
        Fl_Menu_Bar::new_at(bar, 0, 0, 900, 28, core::ptr::null());
        add_menu_items(bar);

        let ed = cxx_operator_new(core::mem::size_of::<RustEditor>()) as *mut RustEditor;
        ed.write(RustEditor::new(0, 28, 900, 640));
        ED.store(ed, Relaxed);
        // Re-parent the ctor-created children (scrollbars) at the
        // final address — the Rust-class ctor protocol constructs the
        // __base in a temporary and moves it (see the advanced
        // example's README); pure-Rust fix-up via FLTK's public
        // parent() setter.
        let g = ed as *mut Fl_Group;
        for i in 0..(*g).children() {
            (*(*g).child(i)).parent_mut_fl_group(g);
        }
        let disp = ed as *mut Fl_Text_Display;
        (*disp).buffer(buf);
        (*disp).linenumber_width(36);

        let find = cxx_operator_new(core::mem::size_of::<FindBar>()) as *mut FindBar;
        find.write(FindBar::new(60, 672, 720, 26));
        FIND.store(find, Relaxed);

        let g = (*win).as_fl_group_mut();
        g.add(bar as *mut Fl_Widget);
        g.add(ed as *mut Fl_Widget);
        g.add(find as *mut Fl_Widget);

        WIN.store(win, Relaxed);
        refresh_title();
        win
    }
}

// ------------------------------------------------------------------
// Headless self-test
// ------------------------------------------------------------------
unsafe fn self_test() -> i32 {
    TEST_MODE.store(true, Relaxed);
    let mut failures = 0;
    let mut check = |name: &str, ok: bool| {
        println!("{} {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };
    unsafe {
        let _win = build_ui();
        let buf = BUF.load(Relaxed);
        let ed = ED.load(Relaxed);

        // 1. Typing marks dirty via the modify callback.
        (*buf).text_const_i8_str("hello rustcc\nsecond line\n");
        check("modify callback fired", MODIFY_EVENTS.load(Relaxed) > 0);
        check("dirty after insert", DIRTY.load(Relaxed));

        // 2. Save → load round-trip.
        let tmp = std::env::temp_dir().join("rustcc_textedit_test.txt");
        let tmp_s = tmp.to_string_lossy().into_owned();
        save_to(&tmp_s);
        check("saved file exists", tmp.exists());
        check("dirty cleared by save", !DIRTY.load(Relaxed));
        (*buf).text_const_i8_str("");
        (*buf).loadfile_with_defaults(cstr(&tmp_s).as_ptr());
        let txt = CStr::from_ptr((*buf).text()).to_string_lossy().into_owned();
        check("round-trip content", txt == "hello rustcc\nsecond line\n");

        // 3. Undo (the load counts as an insert).
        (*buf).insert_str((*buf).length(), "UNDO_ME", 7);
        let with = (*buf).length();
        (*buf).undo_with_defaults();
        check("undo removed insert", (*buf).length() < with);

        // 4. Find.
        (*buf).text_const_i8_str("alpha beta gamma beta");
        let c = cstr("beta");
        let mut found: i32 = -1;
        let hit = (*buf).search_forward(0, c.as_ptr(), &mut found as *mut i32, 0);
        check("search_forward hit", hit != 0 && found == 6);

        // 5. Cmd+D duplicate-line plumbing (call the action directly).
        (*buf).text_const_i8_str("dup me");
        (*(ed as *mut Fl_Text_Display)).insert_position(2);
        duplicate_current_line(ed as *mut Fl_Text_Editor);
        let txt = CStr::from_ptr((*buf).text()).to_string_lossy().into_owned();
        check("Cmd+D duplicates line", txt == "dup me\ndup me");

        // 6. Wrap toggle + font size actions run without crashing.
        run_action(ACT_WRAP);
        run_action(ACT_FONT_UP);
        run_action(ACT_FONT_DOWN);
        check("wrap toggled", WRAP_ON.load(Relaxed));

        let _ = std::fs::remove_file(&tmp);
    }
    if failures == 0 {
        println!("\nTEXTEDIT SELF-TEST: ALL OK");
        0
    } else {
        println!("\nTEXTEDIT SELF-TEST: {failures} FAILURE(S)");
        1
    }
}

fn main() {
    let wants_self_test = std::env::args().any(|a| a == "--self-test");
    let rc = unsafe {
        if wants_self_test {
            self_test()
        } else {
            let win = build_ui();
            (*win).show();
            Fl::run()
        }
    };
    std::process::exit(rc);
}
