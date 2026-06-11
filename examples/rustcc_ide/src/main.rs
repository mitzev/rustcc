//! rustcc IDE — an embedded-RTOS IDE written WITH the rustcc fork,
//! composing the project's own samples into one app:
//!
//!   - The **editor core** is `examples/fltk_text_editor` (menus,
//!     native dialogs, undo/find/wrap, syntax highlighting, the Rust
//!     `class RustEditor : Fl_Text_Editor` subclass).
//!   - A **build console** pane streams toolchain + qemu output live
//!     (`Fl::check()` pump keeps the UI responsive mid-build).
//!   - A **Target menu** models the RAKwireless **RAK11161** WisDuo
//!     breakout: its STM32WLE5 core (Arm Cortex-M4, LoRa side) and
//!     its ESP8684 co-processor (= ESP32-C2, RISC-V rv32imc, no
//!     atomic extension) — plus host and ESP32-C3-class flavors. The
//!     RTOS flavors build FreeRTOS firmware and EXECUTE it under
//!     qemu, reusing `examples/freertos_cpp`'s proven glue.
//!   - **Project ▸ New RAK11161 Project…** scaffolds a complete
//!     dual-core firmware project (Rust `class` sources + FreeRTOS
//!     glue + per-core run scripts + `.vscode/tasks.json` matching
//!     the `tools/vscode-rustcc` plugin conventions) — sources are
//!     embedded at compile time from the sibling examples, so the
//!     scaffold can never drift from the validated probes.
//!
//! ```sh
//! cargo run --release --bin gen_bindings       # stock toolchain + libclang
//! cargo +rustcc run --release --bin ide                  # the GUI
//! cargo +rustcc run --release --bin ide -- --self-test   # headless probes
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
static STYLE_BUF: AtomicPtr<Fl_Text_Buffer> = AtomicPtr::new(core::ptr::null_mut());

// --- IDE state -----------------------------------------------------
static CONSOLE: AtomicPtr<Fl_Text_Display> = AtomicPtr::new(core::ptr::null_mut());
static CONSOLE_BUF: AtomicPtr<Fl_Text_Buffer> = AtomicPtr::new(core::ptr::null_mut());
/// 0 = host, 1 = RAK11161/STM32WLE5 (CM4), 2 = RAK11161/ESP8684
/// (ESP32-C2, rv32imc), 3 = ESP32-C3-class (rv32imac).
static TARGET: AtomicI32 = AtomicI32::new(1);
static PROJECT_DIR: Mutex<Option<String>> = Mutex::new(None);
static BUILD_RUNNING: AtomicBool = AtomicBool::new(false);
static FIND_WIN: AtomicPtr<Fl_Window> = AtomicPtr::new(core::ptr::null_mut());
static NAV: AtomicPtr<FileNav> = AtomicPtr::new(core::ptr::null_mut());
/// Open files: (path, Fl_Text_Buffer* as usize). One buffer per file;
/// the single editor view switches between them (nav click / Open).
static OPEN_FILES: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());
static NAV_PATHS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Highlight keywords: fork keywords extracted at startup from the
/// VSCode extension's TextMate grammar (single source of truth) plus
/// the core Rust keyword set.
static KEYWORDS: Mutex<Vec<String>> = Mutex::new(Vec::new());

const TARGET_NAMES: [&str; 4] = [
    "Host (LLVM backend)",
    "RAK11161 — STM32WLE5 core (Cortex-M4, FreeRTOS, qemu mps2)",
    "RAK11161 — ESP8684 / ESP32-C2 (rv32imc, FreeRTOS, qemu virt)",
    "ESP32-C3-class (rv32imac, FreeRTOS, qemu virt)",
];

/// repr(C) twin of `Fl_Text_Display_Style_Table_Entry` (the nested
/// record emits opaque; field emission for PODs is a tracked
/// enhancement). Layout asserted against the generated type's size
/// in `build_ui`.
#[repr(C)]
#[derive(Copy, Clone)]
struct StyleEntry {
    color: u32,   // Fl_Color
    font: i32,    // Fl_Font
    size: i32,    // Fl_Fontsize
    attr: u32,
    bgcolor: u32, // Fl_Color
}
/// 'A' plain, 'B' comment, 'C' keyword (grammar-driven), 'D' string,
/// 'E' attribute (`#[...]`).
static STYLE_TABLE: [StyleEntry; 5] = [
    StyleEntry { color: 0, font: 4, size: 14, attr: 0, bgcolor: 0xFFFFFF00 },
    StyleEntry { color: 0x00800000, font: 4, size: 14, attr: 0, bgcolor: 0xFFFFFF00 },
    StyleEntry { color: 0x8000A000, font: 4, size: 14, attr: 0, bgcolor: 0xFFFFFF00 },
    StyleEntry { color: 0xA0500000, font: 4, size: 14, attr: 0, bgcolor: 0xFFFFFF00 },
    StyleEntry { color: 0x0060C000, font: 4, size: 14, attr: 0, bgcolor: 0xFFFFFF00 },
];

// FLTK constants — from the generated bindings (the M12 macro pass
// captures the FL_* `#define`s; v1.14). Narrowed to i32 once here.
const EV_KEYDOWN: i32 = 8; // FL_KEYDOWN (Fl_Event enum)
const MOD_CTRL: i32 = FL_CTRL as i32;
const MOD_META: i32 = FL_META as i32; // FL_COMMAND on macOS
const MOD_SHIFT: i32 = FL_SHIFT as i32;
const KEY_ENTER: i32 = FL_Enter as i32;
const KEY_ESCAPE: i32 = FL_Escape as i32;
const EV_RELEASE: i32 = 2; // FL_RELEASE (Fl_Event enum)
// Class-scope enums — generated bindings (v1.14): named nested enums
// emit as transparent structs with assoc consts; anonymous ones as
// prefixed plain consts.
const CHOOSER_OPEN: i32 = Fl_Native_File_Chooser_Type::BROWSE_FILE.0 as i32;
const CHOOSER_SAVE: i32 = Fl_Native_File_Chooser_Type::BROWSE_SAVE_FILE.0 as i32;
// New Project: SAVE_DIRECTORY (lets you name a new folder; native
// dialog shows "Save"). Open Project: plain DIRECTORY ("Open").
const CHOOSER_DIR_NEW: i32 = Fl_Native_File_Chooser_Type::BROWSE_SAVE_DIRECTORY.0 as i32;
const CHOOSER_DIR_OPEN: i32 = Fl_Native_File_Chooser_Type::BROWSE_DIRECTORY.0 as i32;
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
// IDE actions.
const ACT_TGT_BASE: usize = 30; // 30..=33 → TARGET 0..=3
const ACT_NEW_PROJECT: usize = 40;
const ACT_OPEN_PROJECT: usize = 41;
const ACT_BUILD: usize = 42;
const ACT_BUILD_RUN: usize = 43;
const ACT_CONSOLE_CLEAR: usize = 44;
const ACT_NEW_HOST: usize = 45;
const ACT_DEBUG: usize = 46;

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
    #[link_name = "_ZN11Fl_Browser_6handleEi"]
    fn base_browser_handle(this: *mut Fl_Browser_, ev: i32) -> i32;
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
        if ev == EV_KEYDOWN && Fl::event_key() == KEY_ESCAPE {
            let w = FIND_WIN.load(Relaxed);
            if !w.is_null() {
                unsafe { (*(w as *mut Fl_Widget)).hide() };
            }
            return 1;
        }
        unsafe { base_input_handle(this, ev) }
    }
}

// ------------------------------------------------------------------
// File navigator: Rust subclass of the 3-level imported chain
// Fl_Hold_Browser -> Fl_Browser -> Fl_Browser_. Click (release)
// opens the selected file in the editor.
// ------------------------------------------------------------------
pub class FileNav : Fl_Hold_Browser {
    pad: i32,

    pub constructor fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        FileNav {
            __base: Fl_Hold_Browser::new(x, y, w, h, ::core::ptr::null()),
            pad: 0,
        }
    }

    pub override fn handle(&self, ev: i32) -> i32 {
        let this = self as *const Self as *mut Fl_Browser_;
        let r = unsafe { base_browser_handle(this, ev) };
        if ev == EV_RELEASE {
            unsafe { nav_open_selected(self as *const Self as *mut FileNav) };
        }
        r
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
        unsafe {
            restyle();
            refresh_title();
        }
    }
}

/// Rebuild the parallel style buffer with a small tokenizer:
/// line comments 'B', strings 'D', `#[...]` attributes 'E', and
/// keyword identifiers 'C' — the keyword set merges the core Rust
/// keywords with the fork-specific ones extracted at startup from
/// the VSCode extension's TextMate grammar (see load_keywords).
unsafe fn restyle() {
    unsafe {
        let buf = BUF.load(Relaxed);
        let sbuf = STYLE_BUF.load(Relaxed);
        if buf.is_null() || sbuf.is_null() {
            return;
        }
        let raw = (*buf).text();
        if raw.is_null() {
            return;
        }
        let text = CStr::from_ptr(raw).to_string_lossy().into_owned();
        libc_free(raw as *mut ::core::ffi::c_void);
        let kw = KEYWORDS.lock().unwrap();
        let b = text.as_bytes();
        let mut styles = vec![b'A'; b.len()];
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            // line comment
            if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
                let mut j = i;
                while j < b.len() && b[j] != b'\n' {
                    styles[j] = b'B';
                    j += 1;
                }
                i = j;
            // string literal (no escapes-across-lines ambition)
            } else if c == b'"' {
                styles[i] = b'D';
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' && b[j] != b'\n' {
                    if b[j] == b'\\' && j + 1 < b.len() {
                        styles[j] = b'D';
                        j += 1;
                    }
                    styles[j] = b'D';
                    j += 1;
                }
                if j < b.len() && b[j] == b'"' {
                    styles[j] = b'D';
                    j += 1;
                }
                i = j;
            // attribute: #[...] possibly #![...]
            } else if c == b'#'
                && i + 1 < b.len()
                && (b[i + 1] == b'[' || (b[i + 1] == b'!' && i + 2 < b.len() && b[i + 2] == b'['))
            {
                let mut j = i;
                let mut depth = 0i32;
                while j < b.len() {
                    styles[j] = b'E';
                    if b[j] == b'[' {
                        depth += 1;
                    }
                    if b[j] == b']' {
                        depth -= 1;
                        if depth == 0 {
                            j += 1;
                            break;
                        }
                    }
                    j += 1;
                }
                i = j;
            // identifier / keyword
            } else if c.is_ascii_alphabetic() || c == b'_' {
                let mut j = i;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                let word = &text[i..j];
                if kw.iter().any(|k| k == word) {
                    for s in &mut styles[i..j] {
                        *s = b'C';
                    }
                }
                i = j;
            } else {
                i += 1;
            }
        }
        let styles = String::from_utf8(styles).unwrap_or_default();
        (*sbuf).text_const_i8_str(&styles);
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
            ACT_FIND => show_find_popup(),
            ACT_WRAP => {
                let on = !WRAP_ON.load(Relaxed);
                WRAP_ON.store(on, Relaxed);
                let disp = ed as *mut Fl_Text_Display;
                (*disp).wrap_mode(if on { WRAP_AT_BOUNDS } else { WRAP_NONE }, 0);
                (*(ed as *mut Fl_Widget)).redraw();
            }
            ACT_FONT_UP => bump_textsize(ed as *mut Fl_Text_Editor, 2),
            ACT_FONT_DOWN => bump_textsize(ed as *mut Fl_Text_Editor, -2),
            // --- IDE actions ---
            a if (ACT_TGT_BASE..ACT_TGT_BASE + 4).contains(&a) => {
                let t = (a - ACT_TGT_BASE) as i32;
                TARGET.store(t, Relaxed);
                console_append(&format!("target = {}\n", TARGET_NAMES[t as usize]));
            }
            ACT_NEW_PROJECT => new_project_flow(false),
            ACT_NEW_HOST => new_project_flow(true),
            ACT_DEBUG => debug_project(),
            ACT_OPEN_PROJECT => {
                if let Some(dir) = choose_file(CHOOSER_DIR_OPEN, "Open project folder") {
                    set_project(&dir);
                }
            }
            ACT_BUILD => build_project(false),
            ACT_BUILD_RUN => build_project(true),
            ACT_CONSOLE_CLEAR => {
                let cb = CONSOLE_BUF.load(Relaxed);
                if !cb.is_null() {
                    (*cb).text_const_i8_str("");
                }
            }
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
// IDE engine: console, streamed process runner, project scaffold.
// ------------------------------------------------------------------

/// Build the highlight keyword set: core Rust keywords + every
/// fork-specific identifier found in the vscode-rustcc TextMate
/// grammar (embedded at compile time — single source of truth with
/// the editor extension).
fn load_keywords() {
    const GRAMMAR: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tools/vscode-rustcc/syntaxes/rustcc-injection.tmLanguage.json"
    ));
    let mut kw: Vec<String> = [
        "fn", "let", "pub", "use", "impl", "struct", "enum", "match", "if", "else", "for",
        "while", "loop", "return", "unsafe", "mod", "static", "const", "trait", "where",
        "as", "in", "mut", "ref", "move", "dyn", "self", "Self", "super", "crate", "true",
        "false",
        // fork method-modifier keywords (parser-level, not in the
        // injection grammar which targets attributes):
        "class", "constructor", "virtual", "override",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // Harvest fork identifiers from the grammar's match patterns:
    // every word that appears inside the attribute/keyword captures.
    let mut word = String::new();
    for ch in GRAMMAR.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            word.push(ch);
        } else {
            if word.len() > 2
                && (word.starts_with("cpp")
                    || word.starts_with("swift")
                    || word.starts_with("rustc_")
                    || word == "repr"
                    || word == "extern"
                    || word == "class"
                    || word == "constructor")
                && !kw.contains(&word)
            {
                kw.push(word.clone());
            }
            word.clear();
        }
    }
    *KEYWORDS.lock().unwrap() = kw;
}

fn console_append(s: &str) {
    unsafe {
        let cb = CONSOLE_BUF.load(Relaxed);
        if cb.is_null() {
            // Headless (self-test before UI) — mirror to stdout.
            print!("{s}");
            return;
        }
        (*cb).append_with_defaults(cstr(s).as_ptr());
        let disp = CONSOLE.load(Relaxed);
        if !disp.is_null() {
            (*disp).insert_position((*cb).length());
            (*disp).show_insert_position();
            (*(disp as *mut Fl_Widget)).redraw();
        }
    }
}

/// Run `script` in `dir` with merged stderr, streaming each output
/// line into the console while pumping the FLTK event loop — the UI
/// stays live for the whole build/qemu run. Returns the exit code.
fn run_streamed(dir: &str, cmdline: &str) -> i32 {
    use std::io::{BufRead, BufReader};
    let child = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!("cd '{dir}' && {cmdline} 2>&1"))
        .stdout(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            console_append(&format!("spawn failed: {e}\n"));
            return -1;
        }
    };
    let stdout = child.stdout.take().expect("piped stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                console_append(&line);
                if !TEST_MODE.load(Relaxed) {
                    Fl::check();
                }
            }
            Err(_) => break,
        }
    }
    let code = child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
    console_append(&format!("[exit {code}]\n"));
    code
}

/// The per-target build/run command, executed in the project root.
/// RTOS flavors use the project's own run scripts (scaffolded from
/// the validated examples/freertos_cpp glue); `run=false` stops after
/// the link via SKIP_QEMU=1.
fn target_cmdline(target: i32, run: bool) -> String {
    let skip = if run { "" } else { "SKIP_QEMU=1 " };
    match target {
        0 => format!(
            "RUSTC_BOOTSTRAP=1 cargo +nightly {} --release 2>&1 && echo HOST-{}-OK",
            if run { "run" } else { "build" },
            if run { "RUN" } else { "BUILD" },
        ),
        1 => format!("{skip}./run_arm.sh"),
        2 => format!("{skip}./run_riscv_c2.sh"),
        _ => format!("{skip}./run_riscv.sh"),
    }
}

fn build_project(run: bool) {
    if BUILD_RUNNING.swap(true, Relaxed) {
        console_append("a build is already running\n");
        return;
    }
    let dir = PROJECT_DIR.lock().unwrap().clone();
    match dir {
        None => console_append("no project open — Project ▸ New/Open first\n"),
        Some(dir) => {
            let t = TARGET.load(Relaxed);
            console_append(&format!(
                "==> {} [{}]\n",
                if run { "build + run" } else { "build" },
                TARGET_NAMES[t as usize]
            ));
            let code = run_streamed(&dir, &target_cmdline(t, run));
            console_append(if code == 0 { "SUCCESS\n" } else { "FAILED\n" });
        }
    }
    BUILD_RUNNING.store(false, Relaxed);
}

/// Multi-buffer open: one Fl_Text_Buffer per file, the single editor
/// view switches between them. Re-opening an open file just switches.
fn open_in_editor(path: &str) {
    unsafe {
        let existing = {
            let files = OPEN_FILES.lock().unwrap();
            files.iter().position(|(p, _)| p == path)
        };
        let idx = match existing {
            Some(i) => i,
            None => {
                let nbuf = cxx_operator_new(core::mem::size_of::<Fl_Text_Buffer>())
                    as *mut Fl_Text_Buffer;
                Fl_Text_Buffer::new_at(nbuf, 0, 1024);
                (*nbuf).loadfile_with_defaults(cstr(path).as_ptr());
                (*nbuf).add_modify_callback(Some(modify_cb), core::ptr::null_mut());
                let mut files = OPEN_FILES.lock().unwrap();
                files.push((path.to_string(), nbuf as usize));
                files.len() - 1
            }
        };
        switch_to_file(idx);
    }
}

unsafe fn switch_to_file(idx: usize) {
    unsafe {
        let (path, bufp) = {
            let files = OPEN_FILES.lock().unwrap();
            match files.get(idx) {
                Some((p, b)) => (p.clone(), *b as *mut Fl_Text_Buffer),
                None => return,
            }
        };
        BUF.store(bufp, Relaxed);
        let ed = ED.load(Relaxed);
        if !ed.is_null() {
            (*(ed as *mut Fl_Text_Display)).buffer(bufp);
        }
        *PATH.lock().unwrap() = Some(path);
        DIRTY.store(false, Relaxed);
        restyle();
        refresh_title();
        nav_refresh(); // re-mark the selected entry
    }
}

/// Repopulate the navigator from the project dir (2 levels deep,
/// source-ish files only) + every open file.
fn nav_refresh() {
    unsafe {
        let nav = NAV.load(Relaxed);
        if nav.is_null() {
            return;
        }
        let b = &mut *(nav as *mut Fl_Browser);
        b.clear();
        let mut paths: Vec<String> = Vec::new();
        if let Some(dir) = PROJECT_DIR.lock().unwrap().clone() {
            collect_files(&dir, 0, &mut paths);
        }
        for (p, _) in OPEN_FILES.lock().unwrap().iter() {
            if !paths.contains(p) {
                paths.push(p.clone());
            }
        }
        let cur = PATH.lock().unwrap().clone();
        let root = PROJECT_DIR.lock().unwrap().clone().unwrap_or_default();
        for p in &paths {
            let shown = p.strip_prefix(&format!("{root}/")).unwrap_or(p);
            let marker = if Some(p) == cur.as_ref() { "@b" } else { "" };
            b.add_str_with_defaults(&format!("{marker}{shown}"));
        }
        *NAV_PATHS.lock().unwrap() = paths;
        (*(nav as *mut Fl_Widget)).redraw();
    }
}

fn collect_files(dir: &str, depth: u32, out: &mut Vec<String>) {
    if depth > 2 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if p.is_dir() {
            collect_files(&p.to_string_lossy(), depth + 1, out);
        } else if matches!(
            p.extension().and_then(|x| x.to_str()),
            Some("rs" | "c" | "h" | "cpp" | "hpp" | "ld" | "sh" | "toml" | "json" | "md")
        ) {
            out.push(p.to_string_lossy().into_owned());
        }
    }
}

unsafe fn nav_open_selected(nav: *mut FileNav) {
    unsafe {
        let v = (*(nav as *mut Fl_Browser)).value();
        if v <= 0 {
            return;
        }
        let path = {
            let paths = NAV_PATHS.lock().unwrap();
            paths.get((v - 1) as usize).cloned()
        };
        if let Some(p) = path {
            open_in_editor(&p);
        }
    }
}

/// The Find popup: a small always-on-top-ish window holding the
/// FindBar. Created lazily; ⌘F shows + focuses, Escape hides.
fn show_find_popup() {
    unsafe {
        let mut w = FIND_WIN.load(Relaxed);
        if w.is_null() {
            w = cxx_operator_new(core::mem::size_of::<Fl_Window>()) as *mut Fl_Window;
            Fl_Window::new_at(w, 380, 44, c"Find".as_ptr());
            let f = cxx_operator_new(core::mem::size_of::<FindBar>()) as *mut FindBar;
            f.write(FindBar::new(60, 8, 300, 28));
            FIND.store(f, Relaxed);
            (*w).as_fl_group_mut().end();
            (*w).as_fl_group_mut().add(f as *mut Fl_Widget);
            FIND_WIN.store(w, Relaxed);
        }
        (*(w as *mut Fl_Widget)).show();
        let f = FIND.load(Relaxed);
        if !f.is_null() {
            (*(f as *mut Fl_Widget)).take_focus();
        }
    }
}

fn set_project(dir: &str) {
    *PROJECT_DIR.lock().unwrap() = Some(dir.to_string());
    console_append(&format!("project = {dir}\n"));
    let lib = format!("{dir}/src/lib.rs");
    let main = format!("{dir}/src/main.rs");
    if std::path::Path::new(&lib).exists() {
        open_in_editor(&lib);
    } else if std::path::Path::new(&main).exists() {
        open_in_editor(&main);
    }
    nav_refresh();
}

fn new_project_flow(host: bool) {
    let title = if host { "New Host project folder" } else { "New RAK11161 project folder" };
    if let Some(dir) = unsafe { choose_file(CHOOSER_DIR_NEW, title) } {
        let r = if host { scaffold_host(&dir) } else { scaffold_project(&dir) };
        match r {
            Ok(()) => {
                console_append(&format!(
                    "scaffolded {} project at {dir}\n",
                    if host { "Host" } else { "RAK11161" }
                ));
                if host {
                    TARGET.store(0, Relaxed);
                    console_append("target = Host (LLVM backend)\n");
                }
                set_project(&dir);
            }
            Err(e) => console_append(&format!("scaffold FAILED: {e}\n")),
        }
    }
}

/// Debug: launch the firmware under qemu's gdbserver (halted) in a
/// separate Terminal window — it blocks until a debugger attaches —
/// and print the exact attach command in the console. Host target:
/// lldb on the release binary.
fn debug_project() {
    let dir = PROJECT_DIR.lock().unwrap().clone();
    let Some(dir) = dir else {
        console_append("no project open — File > New/Open Project first\n");
        return;
    };
    let t = TARGET.load(Relaxed);
    let (launch, attach): (String, String) = match t {
        0 => (
            format!("cd '{dir}' && RUSTC_BOOTSTRAP=1 cargo +nightly build --release && lldb target/release/rustcc_app"),
            "lldb drives the host binary directly in the Terminal window".to_string(),
        ),
        1 => (
            format!("cd '{dir}' && GDB=1 ./run_arm.sh"),
            format!("arm-none-eabi-gdb '{dir}/target/arm/firmware.elf' -ex 'target remote :1234' -ex 'break main' -ex continue"),
        ),
        2 => (
            format!("cd '{dir}' && GDB=1 ./run_riscv_c2.sh"),
            format!("riscv64-elf-gdb '{dir}/target/riscv-c2/firmware.elf' -ex 'target remote :1234' -ex 'break main' -ex continue"),
        ),
        _ => (
            format!("cd '{dir}' && GDB=1 ./run_riscv.sh"),
            format!("riscv64-elf-gdb '{dir}/target/riscv/firmware.elf' -ex 'target remote :1234' -ex 'break main' -ex continue"),
        ),
    };
    let script = format!(
        "tell application \"Terminal\" to do script \"{}\"",
        launch.replace('"', "\\\"")
    );
    let ok = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        console_append(&format!(
            "debug session launched in Terminal ({}).\n",
            TARGET_NAMES[t as usize]
        ));
        if t != 0 {
            console_append(&format!(
                "qemu is HALTED with gdbserver on :1234 — attach from another shell:\n  {attach}\n"
            ));
        }
    } else {
        console_append(&format!(
            "could not open Terminal; run manually:\n  {launch}\n  then attach:\n  {attach}\n"
        ));
    }
}

/// Host project scaffold — mirrors the vscode-rustcc plugin's
/// "New Project (class surface)" template (Counter class + main).
fn scaffold_host(dir: &str) -> Result<(), String> {
    use std::fs;
    let root = std::path::Path::new(dir);
    let werr = |e: std::io::Error| e.to_string();
    fs::create_dir_all(root.join("src")).map_err(werr)?;
    fs::create_dir_all(root.join(".vscode")).map_err(werr)?;
    fs::write(root.join("Cargo.toml"), HOST_CARGO_TOML).map_err(werr)?;
    fs::write(root.join("src/main.rs"), HOST_MAIN_RS).map_err(werr)?;
    fs::write(root.join(".vscode/tasks.json"), HOST_TASKS_JSON).map_err(werr)?;
    fs::write(root.join("README.md"), HOST_README).map_err(werr)?;
    Ok(())
}

const HOST_CARGO_TOML: &str = r#"[package]
name = "rustcc_app"
version = "0.1.0"
edition = "2021"

[workspace]
"#;

const HOST_MAIN_RS: &str = r#"// Scaffolded by the rustcc IDE (class-keyword surface) — the same
// template the vscode-rustcc plugin's "New Project" command emits.
// The `class` keyword is fork-only; the IDE builds with RUSTC
// pointed at the fork stage1. Since v1.14 no feature gates or allow
// attributes are needed.

pub class Counter {
    n: i64,

    pub constructor fn new() -> Self {
        Counter { n: 0 }
    }

    pub fn bump(&mut self, by: i64) {
        self.n += by;
    }

    pub fn value(&self) -> i64 {
        self.n
    }
}

fn main() {
    let mut c = Counter::new();
    c.bump(41);
    c.bump(1);
    println!("counter = {}", c.value());
}
"#;

const HOST_TASKS_JSON: &str = r#"{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "rustcc: build (host)",
      "type": "shell",
      "command": "RUSTC_BOOTSTRAP=1 cargo +nightly build --release",
      "group": "build",
      "problemMatcher": ["$rustc"]
    },
    {
      "label": "rustcc: run (host)",
      "type": "shell",
      "command": "RUSTC_BOOTSTRAP=1 cargo +nightly run --release",
      "group": "test"
    }
  ]
}
"#;

const HOST_README: &str = r#"# rustcc host project

Scaffolded by the rustcc IDE (mirrors the vscode-rustcc plugin's
class-surface template). Build with the fork:

```sh
RUSTC=<fork-stage1>/bin/rustc cargo +nightly run --release
```
"#;

// --- scaffold: a complete dual-core RAK11161 firmware project -------
//
// Every source is embedded at COMPILE TIME from the sibling examples
// (the validated FreeRTOS probes + bare-metal class crate), so the
// scaffold cannot drift from what CI/qemu actually proves. Path
// references are rewritten from the example tree's layout to the
// scaffolded project's flat layout.

macro_rules! embed {
    ($p:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../", $p))
    };
}

fn scaffold_project(dir: &str) -> Result<(), String> {
    use std::fs;
    let root = std::path::Path::new(dir);
    let werr = |e: std::io::Error| e.to_string();
    for sub in ["src", "cpp", "libc_stub", ".vscode"] {
        fs::create_dir_all(root.join(sub)).map_err(werr)?;
    }

    // Rewrites: examples-tree relative paths → project-local.
    let fix = |s: &str| -> String {
        s.replace("../bare_metal_arm/caller.cpp", "cpp/caller.cpp")
            .replace("../bare_metal_arm/sensor.cpp", "cpp/sensor.cpp")
            .replace("../bare_metal_arm/rtti_stub.c", "cpp/rtti_stub.c")
            .replace("libfreertos_cpp.a", "librak11161_fw.a")
            .replace(
                "echo \"==> qemu",
                "[[ \"${SKIP_QEMU:-0}\" == 1 ]] && { echo \"(SKIP_QEMU=1 — link-only build done)\"; exit 0; }\necho \"==> qemu",
            )
    };

    let files: &[(&str, String)] = &[
        ("src/lib.rs", embed!("bare_metal_arm/src/lib.rs").to_string()),
        ("cpp/caller.cpp", embed!("bare_metal_arm/caller.cpp").to_string()),
        ("cpp/sensor.cpp", embed!("bare_metal_arm/sensor.cpp").to_string()),
        ("cpp/sensor.hpp", embed!("bare_metal_arm/sensor.hpp").to_string()),
        ("cpp/rtti_stub.c", embed!("bare_metal_arm/rtti_stub.c").to_string()),
        ("FreeRTOSConfig.h", embed!("freertos_cpp/FreeRTOSConfig.h").to_string()),
        ("main_arm.c", embed!("freertos_cpp/main_arm.c").to_string()),
        ("main_riscv.c", embed!("freertos_cpp/main_riscv.c").to_string()),
        ("link_arm.ld", embed!("freertos_cpp/link_arm.ld").to_string()),
        ("link_riscv.ld", embed!("freertos_cpp/link_riscv.ld").to_string()),
        ("libc_stub/string.h", embed!("freertos_cpp/libc_stub/string.h").to_string()),
        ("libc_stub/stdlib.h", embed!("freertos_cpp/libc_stub/stdlib.h").to_string()),
        ("libc_stub/tinylibc.c", embed!("freertos_cpp/libc_stub/tinylibc.c").to_string()),
        ("run_arm.sh", fix(embed!("freertos_cpp/run_arm.sh"))),
        ("run_riscv.sh", fix(embed!("freertos_cpp/run_riscv.sh"))),
        ("run_riscv_c2.sh", fix(embed!("freertos_cpp/run_riscv_c2.sh"))),
        ("Cargo.toml", SCAFFOLD_CARGO_TOML.to_string()),
        (".vscode/tasks.json", SCAFFOLD_TASKS_JSON.to_string()),
        ("README.md", SCAFFOLD_README.to_string()),
    ];
    for (rel, content) in files {
        fs::write(root.join(rel), content).map_err(werr)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for s in ["run_arm.sh", "run_riscv.sh", "run_riscv_c2.sh"] {
            fs::set_permissions(root.join(s), fs::Permissions::from_mode(0o755))
                .map_err(werr)?;
        }
    }
    Ok(())
}

const SCAFFOLD_CARGO_TOML: &str = r#"# RAK11161 dual-core firmware — scaffolded by the rustcc IDE.
# The Rust side is a fork `class` crate (Widget/Gauge + imported
# Sensor/Reader); per-core builds are driven by the run scripts.
[workspace]

[package]
name = "rak11161_fw"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["staticlib"]

[profile.release]
panic = "abort"

[profile.dev]
panic = "abort"
"#;

const SCAFFOLD_TASKS_JSON: &str = r#"{
  // VSCode tasks matching the tools/vscode-rustcc plugin conventions:
  // each RAK11161 core gets a build task and a qemu run task.
  "version": "2.0.0",
  "tasks": [
    {
      "label": "rustcc: build (RAK11161 STM32WLE5 / CM4)",
      "type": "shell",
      "command": "SKIP_QEMU=1 ./run_arm.sh",
      "group": "build",
      "problemMatcher": ["$rustc"]
    },
    {
      "label": "rustcc: run on qemu (RAK11161 STM32WLE5 / CM4)",
      "type": "shell",
      "command": "./run_arm.sh",
      "group": "test"
    },
    {
      "label": "rustcc: build (RAK11161 ESP8684 / ESP32-C2)",
      "type": "shell",
      "command": "SKIP_QEMU=1 ./run_riscv_c2.sh",
      "group": "build",
      "problemMatcher": ["$rustc"]
    },
    {
      "label": "rustcc: run on qemu (RAK11161 ESP8684 / ESP32-C2)",
      "type": "shell",
      "command": "./run_riscv_c2.sh",
      "group": "test"
    }
  ]
}
"#;

const SCAFFOLD_README: &str = r#"# RAK11161 dual-core firmware (rustcc)

Scaffolded by the **rustcc IDE** for the RAKwireless RAK11161 WisDuo
breakout: STM32WLE5 (Arm Cortex-M4, LoRa side) + ESP8684 = ESP32-C2
(RISC-V rv32imc, WiFi/BLE side). One Rust `class` crate
(`src/lib.rs`) is built per-core and runs under FreeRTOS, executed on
qemu stand-ins for each core:

```sh
./run_arm.sh        # STM32WLE5 core  (ARM_CM4F port, qemu mps2-an386)
./run_riscv_c2.sh   # ESP8684 core    (RISC-V port, rv32imc, A ext OFF)
SKIP_QEMU=1 ./run_arm.sh   # build + link only
```

Expected: `FREERTOS CXX PROBE (…): PASS (105/4000/503/42 across tasks)`.

`.vscode/tasks.json` carries the same four commands as VSCode build /
test tasks (rustcc plugin conventions). The qemu machines model the
CORES (Cortex-M4 / rv32imc), not RAK's radios — LoRa/WiFi peripheral
work needs hardware or vendor simulators.
"#;

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
        add("&File/&New File", MOD_META | 'n' as i32, ACT_NEW);
        add("&File/&Open File…", MOD_META | 'o' as i32, ACT_OPEN);
        add("&File/&Save", MOD_META | 's' as i32, ACT_SAVE);
        add("&File/Save &As…", MOD_META | MOD_SHIFT | 's' as i32, ACT_SAVE_AS);
        add("&File/New Project/&Host Project…", 0, ACT_NEW_HOST);
        add("&File/New Project/&RAK11161 Project…", MOD_META | MOD_SHIFT | 'n' as i32, ACT_NEW_PROJECT);
        add("&File/Open &Project…", MOD_META | MOD_SHIFT | 'o' as i32, ACT_OPEN_PROJECT);
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
        // --- IDE menus ---
        add("&Project/&Build", MOD_META | 'b' as i32, ACT_BUILD);
        add("&Project/Build && &Run (qemu)", MOD_META | 'r' as i32, ACT_BUILD_RUN);
        add("&Project/&Debug (qemu + gdbserver)…", MOD_META | MOD_SHIFT | 'd' as i32, ACT_DEBUG);
        add("&Project/&Clear Console", 0, ACT_CONSOLE_CLEAR);
        add("&Target/&Host (LLVM)", 0, ACT_TGT_BASE + 0);
        add("&Target/RAK11161: &STM32WLE5 (Cortex-M4)", 0, ACT_TGT_BASE + 1);
        add("&Target/RAK11161: &ESP8684 (ESP32-C2, rv32imc)", 0, ACT_TGT_BASE + 2);
        add("&Target/ESP32-&C3-class (rv32imac)", 0, ACT_TGT_BASE + 3);
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
        Fl_Window::new_at(win, 1180, 760, c"rustcc IDE".as_ptr());
        (*win).as_fl_group_mut().end();
        // No implicit group capture while heap-placing the Rust widgets.
        Fl_Group::current_mut_fl_group(core::ptr::null_mut());

        let buf = cxx_operator_new(core::mem::size_of::<Fl_Text_Buffer>()) as *mut Fl_Text_Buffer;
        Fl_Text_Buffer::new_at(buf, 0, 1024);
        BUF.store(buf, Relaxed);
        (*buf).add_modify_callback(Some(modify_cb), core::ptr::null_mut());

        let bar = cxx_operator_new(core::mem::size_of::<Fl_Menu_Bar>()) as *mut Fl_Menu_Bar;
        Fl_Menu_Bar::new_at(bar, 0, 0, 1180, 28, core::ptr::null());
        add_menu_items(bar);

        let ed = cxx_operator_new(core::mem::size_of::<RustEditor>()) as *mut RustEditor;
        // The ctor-in-place MIR pass (v1.14) constructs straight into
        // *ed, so the ctor-created children (scrollbars) capture the
        // final address — no re-parent fix-up needed.
        ed.write(RustEditor::new(220, 28, 960, 430));
        ED.store(ed, Relaxed);
        debug_assert!({
            let g = ed as *mut Fl_Group;
            (0..(*g).children()).all(|i| (*(*g).child(i)).parent() == g)
        });
        let disp = ed as *mut Fl_Text_Display;
        (*disp).buffer(buf);
        (*disp).linenumber_width(36);

        // Syntax highlighting (v1.14 nested-record binding): the
        // style table rides the repr(C) twin; size-checked here.
        assert_eq!(
            core::mem::size_of::<StyleEntry>(),
            core::mem::size_of::<Fl_Text_Display_Style_Table_Entry>(),
        );
        let sbuf = cxx_operator_new(core::mem::size_of::<Fl_Text_Buffer>()) as *mut Fl_Text_Buffer;
        Fl_Text_Buffer::new_at(sbuf, 0, 1024);
        STYLE_BUF.store(sbuf, Relaxed);
        (*disp).highlight_data(
            sbuf,
            STYLE_TABLE.as_ptr() as *const Fl_Text_Display_Style_Table_Entry,
            STYLE_TABLE.len() as i32,
            b'A' as i8,
            None,
            core::ptr::null_mut(),
        );

        // File navigator (left sidebar): Rust subclass of the
        // 3-level imported Fl_Hold_Browser chain.
        let nav = cxx_operator_new(core::mem::size_of::<FileNav>()) as *mut FileNav;
        nav.write(FileNav::new(0, 28, 220, 732));
        NAV.store(nav, Relaxed);

        // Build console: read-only Fl_Text_Display + its own buffer,
        // streamed into by run_streamed() during builds/qemu runs.
        let cbuf =
            cxx_operator_new(core::mem::size_of::<Fl_Text_Buffer>()) as *mut Fl_Text_Buffer;
        Fl_Text_Buffer::new_at(cbuf, 0, 1024);
        CONSOLE_BUF.store(cbuf, Relaxed);
        let con =
            cxx_operator_new(core::mem::size_of::<Fl_Text_Display>()) as *mut Fl_Text_Display;
        Fl_Text_Display::new_at(con, 220, 462, 960, 298, c"".as_ptr());
        (*con).buffer(cbuf);
        (*con).textsize_i32(12);
        CONSOLE.store(con, Relaxed);
        (*cbuf).text_const_i8_str(
            "rustcc IDE console — Project > New RAK11161 Project... to start;\n\
             Target menu picks the core (default: RAK11161 STM32WLE5 / CM4).\n",
        );

        let g = (*win).as_fl_group_mut();
        g.add(bar as *mut Fl_Widget);
        g.add(nav as *mut Fl_Widget);
        g.add(ed as *mut Fl_Widget);
        g.add(con as *mut Fl_Widget);

        load_keywords();
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

        // 7. IDE: target selection round-trips.
        run_action(ACT_TGT_BASE + 2);
        check("target select (ESP8684/C2)", TARGET.load(Relaxed) == 2);

        // 8. IDE: RAK11161 scaffold — files land, path rewrites
        //    applied, scripts executable, VSCode tasks present.
        let proj = std::env::temp_dir().join(format!(
            "rustcc_ide_scaffold_{}",
            std::process::id()
        ));
        let proj_s = proj.to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&proj);
        check("scaffold ok", scaffold_project(&proj_s).is_ok());
        for f in [
            "src/lib.rs",
            "cpp/caller.cpp",
            "cpp/sensor.cpp",
            "cpp/rtti_stub.c",
            "FreeRTOSConfig.h",
            "main_arm.c",
            "main_riscv.c",
            "link_arm.ld",
            "link_riscv.ld",
            "libc_stub/tinylibc.c",
            "run_arm.sh",
            "run_riscv_c2.sh",
            "Cargo.toml",
            ".vscode/tasks.json",
            "README.md",
        ] {
            check(&format!("scaffold file {f}"), proj.join(f).exists());
        }
        let arm_sh = std::fs::read_to_string(proj.join("run_arm.sh")).unwrap_or_default();
        check("scaffold path rewrite", arm_sh.contains("cpp/caller.cpp"));
        check("scaffold SKIP_QEMU gate", arm_sh.contains("SKIP_QEMU"));
        check(
            "scaffold lib rename",
            arm_sh.contains("librak11161_fw.a") && !arm_sh.contains("libfreertos_cpp.a"),
        );
        let tasks =
            std::fs::read_to_string(proj.join(".vscode/tasks.json")).unwrap_or_default();
        check(
            "vscode tasks cover both cores",
            tasks.contains("run_arm.sh") && tasks.contains("run_riscv_c2.sh"),
        );
        let scripts_ok = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "bash -n '{p}/run_arm.sh' && bash -n '{p}/run_riscv.sh' && bash -n '{p}/run_riscv_c2.sh'",
                p = proj_s
            ))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        check("scaffold scripts parse (bash -n)", scripts_ok);

        // 9. IDE: full firmware build+run for the open project —
        //    gated (needs cross toolchains + qemu + minutes).
        if std::env::var("RUSTCC_IDE_SELFTEST_FULL").as_deref() == Ok("1") {
            *PROJECT_DIR.lock().unwrap() = Some(proj_s.clone());
            TARGET.store(1, Relaxed);
            build_project(true);
            let con = CStr::from_ptr((*CONSOLE_BUF.load(Relaxed)).text())
                .to_string_lossy()
                .into_owned();
            check("full CM4 qemu run PASS", con.contains("PASS (105/4000/503/42"));
        }

        // 10. IDE v2: grammar keywords loaded (incl. fork + plugin set).
        let kw = KEYWORDS.lock().unwrap().clone();
        check("keywords loaded", kw.len() > 30);
        check(
            "fork keywords present",
            ["class", "constructor", "override", "cpp_virtual", "swift_value"]
                .iter()
                .all(|k| kw.iter().any(|w| w == k)),
        );

        // 11. IDE v2: multi-buffer open + switch.
        let f1 = std::env::temp_dir().join("rustcc_ide_a.rs");
        let f2 = std::env::temp_dir().join("rustcc_ide_b.rs");
        std::fs::write(&f1, "// file a\n").unwrap();
        std::fs::write(&f2, "// file b\n").unwrap();
        open_in_editor(&f1.to_string_lossy());
        let buf_a = BUF.load(Relaxed);
        open_in_editor(&f2.to_string_lossy());
        let buf_b = BUF.load(Relaxed);
        check("multi-buffer: distinct buffers", buf_a != buf_b && !buf_a.is_null());
        check("open files tracked", OPEN_FILES.lock().unwrap().len() >= 2);
        open_in_editor(&f1.to_string_lossy());
        check("switch back reuses buffer", BUF.load(Relaxed) == buf_a);
        let _ = std::fs::remove_file(&f1);
        let _ = std::fs::remove_file(&f2);

        // 12. IDE v2: host scaffold.
        let hostp = std::env::temp_dir().join(format!("rustcc_ide_host_{}", std::process::id()));
        let hostp_s = hostp.to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&hostp);
        check("host scaffold ok", scaffold_host(&hostp_s).is_ok());
        for f in ["Cargo.toml", "src/main.rs", ".vscode/tasks.json", "README.md"] {
            check(&format!("host file {f}"), hostp.join(f).exists());
        }
        let hm = std::fs::read_to_string(hostp.join("src/main.rs")).unwrap_or_default();
        check("host template = plugin class surface", hm.contains("pub class Counter"));
        let _ = std::fs::remove_dir_all(&hostp);

        // 13. IDE v2: GDB gate present in the scaffolded run scripts.
        let proj2 = std::env::temp_dir().join(format!("rustcc_ide_gdb_{}", std::process::id()));
        let proj2_s = proj2.to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&proj2);
        check("scaffold (gdb) ok", scaffold_project(&proj2_s).is_ok());
        let sh = std::fs::read_to_string(proj2.join("run_arm.sh")).unwrap_or_default();
        check("gdbserver gate in scaffold", sh.contains("GDB") && sh.contains("-s -S"));
        let _ = std::fs::remove_dir_all(&proj2);

        // 14. IDE v2: find popup constructs.
        show_find_popup();
        check("find popup exists", !FIND_WIN.load(Relaxed).is_null());

        let _ = std::fs::remove_dir_all(&proj);
    }
    if failures == 0 {
        println!("\nRUSTCC IDE SELF-TEST: ALL OK");
        0
    } else {
        println!("\nRUSTCC IDE SELF-TEST: {failures} FAILURE(S)");
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
