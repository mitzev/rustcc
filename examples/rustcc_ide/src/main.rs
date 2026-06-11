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
/// (ESP32-C2, rv32imc), 3 = ESP32-C3-class (rv32imac),
/// 4 = STM32F4-class (CM4F), 5 = Raspberry Pi Pico (RP2040, CM0+).
static TARGET: AtomicI32 = AtomicI32::new(1);
static PROJECT_DIR: Mutex<Option<String>> = Mutex::new(None);
static BUILD_RUNNING: AtomicBool = AtomicBool::new(false);
static FIND_WIN: AtomicPtr<Fl_Window> = AtomicPtr::new(core::ptr::null_mut());
static HELP_WIN: AtomicPtr<Fl_Window> = AtomicPtr::new(core::ptr::null_mut());
static NAV: AtomicPtr<FileNav> = AtomicPtr::new(core::ptr::null_mut());
/// Open files: (path, Fl_Text_Buffer* as usize). One buffer per file;
/// the single editor view switches between them (nav click / Open).
static OPEN_FILES: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());
static NAV_PATHS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Highlight keywords: fork keywords extracted at startup from the
/// VSCode extension's TextMate grammar (single source of truth) plus
/// the core Rust keyword set.
static KEYWORDS: Mutex<Vec<String>> = Mutex::new(Vec::new());
// Autocompletion popup state.
static COMPLETE_WIN: AtomicPtr<Fl_Window> = AtomicPtr::new(core::ptr::null_mut());
static COMPLETE_LIST: AtomicPtr<CompleteList> = AtomicPtr::new(core::ptr::null_mut());
static COMPLETE_ITEMS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static COMPLETE_START: AtomicI32 = AtomicI32::new(-1);
// Interactive debugger (host/lldb) state.
static DBG_CHILD: Mutex<Option<std::process::Child>> = Mutex::new(None);
static DBG_STDIN: Mutex<Option<std::process::ChildStdin>> = Mutex::new(None);
static DBG_PENDING: Mutex<String> = Mutex::new(String::new());
static BREAKPOINTS: Mutex<Vec<(String, i32)>> = Mutex::new(Vec::new());
static DBG_CURLINE: Mutex<Option<(String, i32)>> = Mutex::new(None);

/// Variables window: latest `frame variable` capture (refreshed on
/// every stop while the window exists) + watch-expression results.
static VARS_WIN: AtomicPtr<Fl_Window> = AtomicPtr::new(core::ptr::null_mut());
static VARS_DISP: AtomicPtr<Fl_Text_Display> = AtomicPtr::new(core::ptr::null_mut());
static VARS_BUF: AtomicPtr<Fl_Text_Buffer> = AtomicPtr::new(core::ptr::null_mut());
static VARS_LOCALS: Mutex<String> = Mutex::new(String::new());
static VARS_WATCH: Mutex<String> = Mutex::new(String::new());

/// One in-flight lldb output capture (locals or watch). lldb output
/// is a single async stream, so a capture brackets it: payload =
/// lines after the request until the `script print` sentinel line.
struct CapState {
    mode: u8, // 0 idle, 1 locals, 2 watch
    acc: String,
    label: String,
    cmds: Vec<String>, // sent commands, to suppress their pty echoes
}
static DBG_CAPTURE: Mutex<CapState> = Mutex::new(CapState {
    mode: 0,
    acc: String::new(),
    label: String::new(),
    cmds: Vec::new(),
});
const VARS_SENTINEL: &str = "--rustcc-vars-end--";
static DBG_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Tab bar: a slim Fl_Menu_Bar listing open files (click = switch).
static TABBAR: AtomicPtr<FileTabs> = AtomicPtr::new(core::ptr::null_mut());
/// Names currently shown as tab pages — rebuild only when the open
/// set changes; a same-set refresh just syncs the selected tab (so
/// clicks inside Fl_Tabs::handle never delete live child widgets).
static TAB_NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());

const TARGET_NAMES: [&str; 6] = [
    "Host (LLVM backend)",
    "RAK11161 — STM32WLE5 core (Cortex-M4, FreeRTOS, qemu mps2)",
    "RAK11161 — ESP8684 / ESP32-C2 (rv32imc, FreeRTOS, qemu virt)",
    "ESP32-C3-class (rv32imac, FreeRTOS, qemu virt)",
    "STM32F4-class (Cortex-M4F, FreeRTOS, qemu mps2)",
    "Raspberry Pi Pico (RP2040, Cortex-M0+, FreeRTOS, qemu mps2)",
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
/// 'E' attribute (`#[...]`), 'F' breakpoint line (red bg),
/// 'G' debugger's current line (amber bg).
static STYLE_TABLE: [StyleEntry; 7] = [
    StyleEntry { color: 0, font: 4, size: 14, attr: 0, bgcolor: 0xFFFFFF00 },
    StyleEntry { color: 0x00800000, font: 4, size: 14, attr: 0, bgcolor: 0xFFFFFF00 },
    StyleEntry { color: 0x8000A000, font: 4, size: 14, attr: 0, bgcolor: 0xFFFFFF00 },
    StyleEntry { color: 0xA0500000, font: 4, size: 14, attr: 0, bgcolor: 0xFFFFFF00 },
    StyleEntry { color: 0x0060C000, font: 4, size: 14, attr: 0, bgcolor: 0xFFFFFF00 },
    StyleEntry { color: 0, font: 4, size: 14, attr: Fl_Text_Display_ATTR_BGCOLOR, bgcolor: 0xFFD0D000 },
    StyleEntry { color: 0, font: 4, size: 14, attr: Fl_Text_Display_ATTR_BGCOLOR, bgcolor: 0xFFF0A000 },
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
const EV_PUSH: i32 = 1; // FL_PUSH
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
const ACT_TGT_BASE: usize = 30; // 30..=35 → TARGET 0..=5
const ACT_NEW_PROJECT: usize = 40;
const ACT_OPEN_PROJECT: usize = 41;
const ACT_BUILD: usize = 42;
const ACT_BUILD_RUN: usize = 43;
const ACT_CONSOLE_CLEAR: usize = 44;
const ACT_NEW_HOST: usize = 45;
const ACT_DEBUG: usize = 46;
const ACT_NEW_PICO: usize = 47;
const ACT_NEW_STM32: usize = 58;
const ACT_NEW_ESP32: usize = 59;
const ACT_HELP: usize = 60;
const ACT_ABOUT: usize = 61;
const ACT_UPLOAD: usize = 48;
const ACT_UPLOAD_CFG: usize = 49;
const ACT_DBG_START: usize = 50;
const ACT_DBG_STEP_OVER: usize = 51;
const ACT_DBG_STEP_IN: usize = 52;
const ACT_DBG_STEP_OUT: usize = 53;
const ACT_DBG_CONTINUE: usize = 54;
const ACT_DBG_VARS: usize = 55;
const ACT_DBG_STOP: usize = 56;
const ACT_DBG_BREAKPOINT: usize = 57;
const ACT_CLOSE_FILE: usize = 58;
const KEY_F: i32 = FL_F as i32; // F-keys: KEY_F + n

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
    #[link_name = "_ZN7Fl_Tabs6handleEi"]
    fn base_tabs_handle(this: *mut Fl_Tabs, ev: i32) -> i32;
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
            if key == ' ' as i32 && Fl::event_state() & MOD_CTRL != 0 {
                unsafe { show_completions() };
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

/// The Variables window's watch box: Enter evaluates the expression
/// in the live lldb session (bare identifiers — e.g. a GLOBAL — via
/// `target variable`, anything else via `expression --`).
pub class WatchInput : Fl_Input {
    pad: i32,

    pub constructor fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        WatchInput {
            __base: Fl_Input::new(x, y, w, h, c"watch:".as_ptr()),
            pad: 0,
        }
    }

    pub override fn handle(&self, ev: i32) -> i32 {
        let this = self as *const Self as *mut Fl_Input;
        if ev == EV_KEYDOWN && Fl::event_key() == KEY_ENTER {
            let v = unsafe {
                CStr::from_ptr((*this).as_fl_input_().value_ovl())
                    .to_string_lossy()
                    .into_owned()
            };
            let v = v.trim();
            if !v.is_empty() {
                watch_eval(v);
            }
            return 1;
        }
        if ev == EV_KEYDOWN && Fl::event_key() == KEY_ESCAPE {
            let w = VARS_WIN.load(Relaxed);
            if !w.is_null() {
                unsafe { (*(w as *mut Fl_Widget)).hide() };
            }
            return 1;
        }
        unsafe { base_input_handle(this, ev) }
    }
}

/// The tab strip: a real Fl_Tabs (imported chain Fl_Tabs -> Fl_Group
/// -> Fl_Widget) whose zero-height child pages carry the file names —
/// the single shared editor stays OUTSIDE the tabs, so selecting a
/// tab just swaps the editor's buffer.
pub class FileTabs : Fl_Tabs {
    pad: i32,

    pub constructor fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        FileTabs {
            __base: Fl_Tabs::new(x, y, w, h, ::core::ptr::null()),
            pad: 0,
        }
    }

    pub override fn handle(&self, ev: i32) -> i32 {
        let this = self as *const Self as *mut Fl_Tabs;
        let before = unsafe { (*this).value() };
        let r = unsafe { base_tabs_handle(this, ev) };
        if ev == EV_PUSH || ev == EV_RELEASE {
            let after = unsafe { (*this).value() };
            if !after.is_null() && after != before {
                let g = unsafe { (*this).as_fl_group() };
                for i in 0..g.children() {
                    if g.child(i) == after {
                        unsafe { switch_to_file(i as usize) };
                        break;
                    }
                }
            }
        }
        r
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
// Autocompletion list: fourth browser-chain subclass. Enter or a
// click applies the selected completion; Escape dismisses.
// ------------------------------------------------------------------
pub class CompleteList : Fl_Hold_Browser {
    pad: i32,

    pub constructor fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        CompleteList {
            __base: Fl_Hold_Browser::new(x, y, w, h, ::core::ptr::null()),
            pad: 0,
        }
    }

    pub override fn handle(&self, ev: i32) -> i32 {
        if ev == EV_KEYDOWN {
            let k = Fl::event_key();
            if k == KEY_ENTER {
                unsafe { apply_completion() };
                return 1;
            }
            if k == KEY_ESCAPE {
                hide_completions();
                return 1;
            }
        }
        let this = self as *const Self as *mut Fl_Browser_;
        let r = unsafe { base_browser_handle(this, ev) };
        if ev == EV_RELEASE {
            unsafe { apply_completion() };
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
        // Debugger overlays: whole-line marks for breakpoints ('F')
        // and the stopped line ('G') of the CURRENT file.
        {
            let cur_path = PATH.lock().unwrap().clone().unwrap_or_default();
            let base = cur_path.rsplit('/').next().unwrap_or("").to_string();
            let bp_lines: Vec<i32> = BREAKPOINTS
                .lock()
                .unwrap()
                .iter()
                .filter(|(f, _)| *f == cur_path)
                .map(|(_, l)| *l)
                .collect();
            let dbg_line = DBG_CURLINE
                .lock()
                .unwrap()
                .clone()
                .filter(|(f, _)| *f == base || *f == cur_path)
                .map(|(_, l)| l);
            if !bp_lines.is_empty() || dbg_line.is_some() {
                let mut lineno = 1i32;
                let mut start = 0usize;
                for (i, ch) in b.iter().enumerate() {
                    if *ch == b'\n' || i == b.len() - 1 {
                        let end = i + 1;
                        let mark = if dbg_line == Some(lineno) {
                            Some(b'G')
                        } else if bp_lines.contains(&lineno) {
                            Some(b'F')
                        } else {
                            None
                        };
                        if let Some(m) = mark {
                            // Keep the newline's style byte intact so the
                            // parallel styles string stays line-aligned.
                            let stop = if b[end - 1] == b'\n' { end - 1 } else { end };
                            for s in &mut styles[start..stop] {
                                *s = m;
                            }
                        }
                        lineno += 1;
                        start = end;
                    }
                }
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
            a if (ACT_TGT_BASE..ACT_TGT_BASE + 6).contains(&a) => {
                let t = (a - ACT_TGT_BASE) as i32;
                TARGET.store(t, Relaxed);
                console_append(&format!("target = {}\n", TARGET_NAMES[t as usize]));
            }
            ACT_NEW_HOST => new_project_flow(0),
            ACT_NEW_PROJECT => new_project_flow(1),
            ACT_NEW_STM32 => new_project_flow(2),
            ACT_NEW_ESP32 => new_project_flow(3),
            ACT_NEW_PICO => new_project_flow(4),
            ACT_HELP => show_help_popup(),
            ACT_ABOUT => console_append(&format!(
                "rustcc IDE {} — fork-Rust FLTK IDE (class keyword over an \
                 imported C++ widget chain).\nRepo: \
                 https://github.com/mitzev/rustcc — docs: \
                 examples/rustcc_ide/README.md (or press F1)\n",
                env!("CARGO_PKG_VERSION")
            )),
            ACT_DEBUG => debug_project(),
            ACT_UPLOAD => upload_firmware(),
            ACT_DBG_START => dbg_start(),
            ACT_DBG_BREAKPOINT => dbg_toggle_breakpoint(),
            ACT_DBG_STEP_OVER => dbg_send("thread step-over"),
            ACT_DBG_STEP_IN => dbg_send("thread step-in"),
            ACT_DBG_STEP_OUT => dbg_send("thread step-out"),
            ACT_DBG_CONTINUE => dbg_send("continue"),
            ACT_DBG_VARS => {
                show_vars_window();
                if DBG_ACTIVE.load(Relaxed) {
                    dbg_request_vars();
                }
            }
            ACT_DBG_STOP => dbg_stop(),
            ACT_CLOSE_FILE => close_current_file(),
            ACT_UPLOAD_CFG => edit_upload_config(),
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
        // Host builds need the FORK rustc for the `class` keyword —
        // default RUSTC exactly like the RTOS run scripts do.
        0 => format!(
            "RUSTC=\"${{RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}}\" \
             RUSTC_BOOTSTRAP=1 cargo +nightly {} --release 2>&1 && echo HOST-{}-OK",
            if run { "run" } else { "build" },
            if run { "RUN" } else { "BUILD" },
        ),
        1 | 4 => format!("{skip}./run_arm.sh"),
        2 => format!("{skip}./run_riscv_c2.sh"),
        5 => format!("{skip}./run_pico.sh"),
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
        tabs_refresh();
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

/// Help ▸ rustcc IDE Help (F1): a lazily-built window with a
/// read-only text view of the cheat sheet. Escape / close hides it.
const HELP_TEXT: &str = concat!(
    "rustcc IDE ",
    env!("CARGO_PKG_VERSION"),
    " — an embedded-RTOS IDE written in fork Rust\n",
    "=========================================================\n\
     The IDE itself is a fork-Rust program (the `class` keyword\n\
     subclassing FLTK across an imported C++ chain). It scaffolds,\n\
     builds, runs (qemu), debugs and flashes RTOS firmware for\n\
     RAK11161 (STM32WLE5 + ESP8684/ESP32-C2), STM32, ESP32 and\n\
     Raspberry Pi Pico — plus host-side fork programs.\n\
     \n\
     Projects\n\
       File > New Project > Host / RAK11161 / STM32 / ESP32 / Pico\n\
         RTOS flavors share one self-contained scaffold (every\n\
         core's run script ships) and differ only in the default\n\
         Target. Switch cores anytime in the Target menu.\n\
       Cmd+B        build (link-only for RTOS targets)\n\
       Cmd+R        build + run (qemu for RTOS, cargo run for Host)\n\
       Cmd+U        upload firmware (tools configured in upload.toml\n\
                    via Project > Edit Upload Config…)\n\
       Cmd+Shift+D  Project > Debug…: RTOS targets boot qemu HALTED\n\
                    (-s -S) and print the gdb attach command; Host\n\
                    opens lldb in a Terminal window.\n\
     \n\
     In-IDE debugger (Target = Host)\n\
       F5           start lldb session    Shift+F5  stop\n\
       F8 / Cmd+D   toggle breakpoint (red lines; replayed live)\n\
       F10          step over             F11       step into\n\
       Shift+F11    step out              F9        continue\n\
       F7           Variables window — frame locals auto-refresh\n\
                    on every stop; its watch box reads a global or\n\
                    static by name (target variable) or evaluates\n\
                    any expression.\n\
       Every stop follows in the editor — current line amber,\n\
       breakpoint lines red; stops in other open files switch tabs.\n\
     \n\
     Editing\n\
       Cmd+F        find (Enter = next, Escape = close)\n\
       Ctrl+Space   autocomplete (keywords + open-buffer symbols)\n\
       Cmd+W        close file        Cmd+Shift+W  wrap lines\n\
       Cmd+= / -    font size\n\
       Tabs above the editor switch open files; the left sidebar\n\
       lists and opens project files.\n\
     \n\
     More: examples/rustcc_ide/README.md\n\
     Repo: https://github.com/mitzev/rustcc\n"
);

fn show_help_popup() {
    unsafe {
        let mut w = HELP_WIN.load(Relaxed);
        if w.is_null() {
            w = cxx_operator_new(core::mem::size_of::<Fl_Window>()) as *mut Fl_Window;
            Fl_Window::new_at(w, 640, 560, c"rustcc IDE — Help".as_ptr());
            let hb = cxx_operator_new(core::mem::size_of::<Fl_Text_Buffer>())
                as *mut Fl_Text_Buffer;
            Fl_Text_Buffer::new_at(hb, 0, 1024);
            (*hb).text_const_i8_str(HELP_TEXT);
            let d = cxx_operator_new(core::mem::size_of::<Fl_Text_Display>())
                as *mut Fl_Text_Display;
            Fl_Text_Display::new_at(d, 10, 10, 620, 540, c"".as_ptr());
            (*d).buffer(hb);
            (*d).textsize_i32(12);
            (*w).as_fl_group_mut().end();
            (*w).as_fl_group_mut().add(d as *mut Fl_Widget);
            HELP_WIN.store(w, Relaxed);
        }
        (*(w as *mut Fl_Widget)).show();
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

/// Word-based autocompletion: candidates = grammar/Rust keyword set
/// plus every identifier (len > 2) in every OPEN buffer — so project
/// symbols (class names, fns, fields) complete as soon as their file
/// is open. Triggered with Ctrl+Space; Enter/click inserts, Escape
/// dismisses.
unsafe fn show_completions() {
    unsafe {
        let buf = BUF.load(Relaxed);
        let ed = ED.load(Relaxed);
        if buf.is_null() || ed.is_null() {
            return;
        }
        let disp = ed as *mut Fl_Text_Display;
        let pos = (*disp).insert_position_ovl();
        let raw = (*buf).text();
        if raw.is_null() {
            return;
        }
        let text = CStr::from_ptr(raw).to_string_lossy().into_owned();
        libc_free(raw as *mut ::core::ffi::c_void);
        let b = text.as_bytes();
        let mut start = pos as usize;
        while start > 0
            && start <= b.len()
            && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_')
        {
            start -= 1;
        }
        let prefix = &text[start..pos as usize];
        if prefix.is_empty() {
            console_append("completion: type a word prefix first\n");
            return;
        }

        // Harvest candidates.
        let mut cands: Vec<String> = Vec::new();
        for k in KEYWORDS.lock().unwrap().iter() {
            if k.starts_with(prefix) && k != prefix {
                cands.push(k.clone());
            }
        }
        for (_, bufp) in OPEN_FILES.lock().unwrap().iter() {
            let ob = *bufp as *mut Fl_Text_Buffer;
            let oraw = (*ob).text();
            if oraw.is_null() {
                continue;
            }
            let otext = CStr::from_ptr(oraw).to_string_lossy().into_owned();
            libc_free(oraw as *mut ::core::ffi::c_void);
            let mut w = String::new();
            for ch in otext.chars() {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    w.push(ch);
                } else {
                    if w.len() > 2 && w.starts_with(prefix) && w != prefix && !cands.contains(&w)
                    {
                        cands.push(w.clone());
                    }
                    w.clear();
                }
            }
        }
        cands.sort();
        cands.truncate(60);
        if cands.is_empty() {
            console_append(&format!("no completions for '{prefix}'\n"));
            return;
        }
        COMPLETE_START.store(start as i32, Relaxed);
        *COMPLETE_ITEMS.lock().unwrap() = cands.clone();

        // Popup near the cursor (screen coords = window + widget +
        // glyph position).
        let mut cx = 0i32;
        let mut cy = 0i32;
        (*disp).position_to_xy(pos, &mut cx as *mut i32, &mut cy as *mut i32);
        let win = WIN.load(Relaxed);
        let (wx, wy) = if win.is_null() {
            (100, 100)
        } else {
            let w = win as *mut Fl_Widget;
            ((*w).x(), (*w).y())
        };

        let mut cw = COMPLETE_WIN.load(Relaxed);
        if cw.is_null() {
            cw = cxx_operator_new(core::mem::size_of::<Fl_Window>()) as *mut Fl_Window;
            Fl_Window::new_at(cw, 260, 180, c"".as_ptr());
            let l = cxx_operator_new(core::mem::size_of::<CompleteList>()) as *mut CompleteList;
            l.write(CompleteList::new(0, 0, 260, 180));
            COMPLETE_LIST.store(l, Relaxed);
            (*cw).as_fl_group_mut().end();
            (*cw).as_fl_group_mut().add(l as *mut Fl_Widget);
            COMPLETE_WIN.store(cw, Relaxed);
        }
        let l = COMPLETE_LIST.load(Relaxed);
        let lb = &mut *(l as *mut Fl_Browser);
        lb.clear();
        for c in &cands {
            lb.add_str_with_defaults(c);
        }
        (*(l as *mut Fl_Browser)).value_i32(1);
        // cx/cy are editor-relative... position_to_xy returns window
        // coords; offset by the top-level window's screen position.
        (*(cw as *mut Fl_Widget)).resize(wx + cx, wy + cy + 18, 260, 180);
        (*(cw as *mut Fl_Widget)).show();
        (*(l as *mut Fl_Widget)).take_focus();
    }
}

fn hide_completions() {
    let cw = COMPLETE_WIN.load(Relaxed);
    if !cw.is_null() {
        unsafe { (*(cw as *mut Fl_Widget)).hide() };
    }
}

unsafe fn apply_completion() {
    unsafe {
        let l = COMPLETE_LIST.load(Relaxed);
        if l.is_null() {
            return;
        }
        let v = (*(l as *mut Fl_Browser)).value();
        let item = {
            let items = COMPLETE_ITEMS.lock().unwrap();
            if v <= 0 {
                None
            } else {
                items.get((v - 1) as usize).cloned()
            }
        };
        let Some(word) = item else { return };
        let buf = BUF.load(Relaxed);
        let ed = ED.load(Relaxed);
        let start = COMPLETE_START.load(Relaxed);
        if buf.is_null() || ed.is_null() || start < 0 {
            return;
        }
        let disp = ed as *mut Fl_Text_Display;
        let pos = (*disp).insert_position_ovl();
        (*buf).replace_with_defaults(start, pos, cstr(&word).as_ptr());
        (*disp).insert_position(start + word.len() as i32);
        hide_completions();
        (*(ed as *mut Fl_Widget)).take_focus();
        (*(ed as *mut Fl_Widget)).redraw();
    }
}

// --- firmware upload (configurable tools) ---------------------------
//
// Per-project `upload.toml` maps each target family to a shell
// command template; placeholders {elf} {dir} {port} are substituted
// at upload time. The file is plain text edited in the IDE itself
// (Project > Edit Upload Config…) — created with documented defaults
// on first use. STM32 uses STM32_Programmer_CLI, ESP32 esptool.py,
// Pico picotool.

const UPLOAD_TOML: &str = r#"# rustcc IDE — firmware upload configuration (per project).
#
# Each [section] provides `cmd`, a shell template run from the
# project root with these placeholders:
#   {elf}   the firmware ELF for the selected target
#   {dir}   the project root
#   {port}  the serial port from [serial] below
#
# IMPORTANT: the qemu-validated ELFs use the qemu machines' memory
# maps. Before flashing REAL hardware, point the linker script at
# your board (STM32: flash @ 0x08000000; ESP32: esp-idf image
# layout; Pico: pico-sdk crt0/boot2) — then these commands apply
# unchanged.

[stm32]
# STM32CubeProgrammer CLI (SWD probe, e.g. ST-LINK):
cmd = "STM32_Programmer_CLI -c port=SWD -w {elf} -v -rst"

[esp32]
# esptool.py: convert the ELF to an esp image, then flash:
cmd = "esptool.py --chip auto elf2image {elf} -o {dir}/fw.bin && esptool.py --chip auto --port {port} write_flash 0x0 {dir}/fw.bin"

[pico]
# picotool (BOOTSEL mode or with -f to force-reboot):
cmd = "picotool load {elf} -fx"

[serial]
port = "/dev/cu.usbmodem01"
"#;

fn upload_cfg_path() -> Option<String> {
    PROJECT_DIR.lock().unwrap().clone().map(|d| format!("{d}/upload.toml"))
}

fn ensure_upload_config() -> Option<String> {
    let p = upload_cfg_path()?;
    if !std::path::Path::new(&p).exists() {
        if std::fs::write(&p, UPLOAD_TOML).is_err() {
            console_append("could not create upload.toml\n");
            return None;
        }
        console_append(&format!("created default {p}\n"));
    }
    Some(p)
}

fn edit_upload_config() {
    match ensure_upload_config() {
        Some(p) => {
            open_in_editor(&p);
            console_append("upload config opened — edit & save; Cmd+U uses it\n");
        }
        None => console_append("no project open — File > New/Open Project first\n"),
    }
}

/// Tiny section/key parser for upload.toml: returns `key = "value"`
/// inside `[section]`.
fn upload_cfg_get(cfg: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in cfg.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_section = l == format!("[{section}]");
            continue;
        }
        if in_section && !l.starts_with('#') {
            if let Some(rest) = l.strip_prefix(key) {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('=') {
                    let v = rest.trim().trim_matches('"');
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// (config section, firmware tag) for an uploadable target.
fn upload_route(target: i32) -> Option<(&'static str, &'static str)> {
    match target {
        1 | 4 => Some(("stm32", "arm")),
        2 => Some(("esp32", "riscv-c2")),
        3 => Some(("esp32", "riscv")),
        5 => Some(("pico", "pico")),
        _ => None,
    }
}

fn upload_firmware() {
    let Some(dir) = PROJECT_DIR.lock().unwrap().clone() else {
        console_append("no project open — File > New/Open Project first\n");
        return;
    };
    let t = TARGET.load(Relaxed);
    let Some((section, tag)) = upload_route(t) else {
        console_append("host target has nothing to flash — pick an RTOS target\n");
        return;
    };
    let Some(cfgp) = ensure_upload_config() else { return };
    let cfg = std::fs::read_to_string(&cfgp).unwrap_or_default();
    let Some(cmd_tpl) = upload_cfg_get(&cfg, section, "cmd") else {
        console_append(&format!(
            "no [{section}] cmd in upload.toml — Project > Edit Upload Config…\n"
        ));
        return;
    };
    let port = upload_cfg_get(&cfg, "serial", "port").unwrap_or_default();
    let elf = format!("{dir}/target/{tag}/firmware.elf");
    if !std::path::Path::new(&elf).exists() {
        console_append(&format!(
            "{elf} not built yet — Cmd+B first (link-only is enough)\n"
        ));
        return;
    }
    let cmd = cmd_tpl
        .replace("{elf}", &elf)
        .replace("{dir}", &dir)
        .replace("{port}", &port);
    console_append(&format!(
        "==> upload [{}] via [{section}]\n    {cmd}\n",
        TARGET_NAMES[t as usize]
    ));
    let code = run_streamed(&dir, &cmd);
    console_append(if code == 0 { "UPLOAD OK\n" } else { "UPLOAD FAILED\n" });
}

// --- interactive debugger (host target, lldb in-IDE) ----------------
//
// lldb runs as a piped child; a reader thread appends its output to
// DBG_PENDING, and the main loop (Fl::wait_f64 pump) drains it into
// the console, parsing stop locations to move the editor to the
// stopped line. Breakpoints are kept per (file, line), marked in the
// style buffer, and replayed into every new session. Stepping /
// continue / variables are one-keystroke lldb commands (F10/F11/F9/
// F7). RTOS targets keep the Terminal+gdbserver flow (Project menu).

fn dbg_start() {
    if DBG_ACTIVE.load(Relaxed) {
        console_append("debug session already running (Debug > Stop first)\n");
        return;
    }
    let Some(dir) = PROJECT_DIR.lock().unwrap().clone() else {
        console_append("no project open — File > New/Open Project first\n");
        return;
    };
    if TARGET.load(Relaxed) != 0 {
        console_append(
            "in-IDE stepping is host-only for now — Target > Host, or use\n\
             Project > Debug in Terminal for the qemu+gdbserver flow\n",
        );
        return;
    }
    let cargo = std::fs::read_to_string(format!("{dir}/Cargo.toml")).unwrap_or_default();
    if cargo.contains("staticlib") {
        console_append("firmware staticlib — no host binary; create a Host project\n");
        return;
    }
    let name = cargo
        .lines()
        .find_map(|l| {
            let l = l.trim();
            l.strip_prefix("name = \"").and_then(|r| r.strip_suffix('\"'))
        })
        .unwrap_or("rustcc_app")
        .to_string();

    console_append("==> building (debug profile, full debug info)\n");
    let code = run_streamed(
        &dir,
        "RUSTC=\"${RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}\" RUSTC_BOOTSTRAP=1 cargo +nightly build 2>&1",
    );
    if code != 0 {
        console_append("build failed — not starting the debugger\n");
        return;
    }
    let bin = format!("{dir}/target/debug/{name}");

    // lldb must believe it has a terminal (async stop events, command
    // multiplexing vs the inferior) — bridge it through a pty with
    // `script -q /dev/null`, the portable macOS/BSD trick.
    let child = std::process::Command::new("script")
        .args(["-q", "/dev/null", "lldb", "--no-use-colors"])
        .arg(&bin)
        .current_dir(&dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            console_append(&format!("lldb spawn failed: {e}\n"));
            return;
        }
    };
    let stdin = child.stdin.take().expect("lldb stdin");
    let stdout = child.stdout.take().expect("lldb stdout");
    let stderr = child.stderr.take().expect("lldb stderr");
    for pipe in [Some(stdout), None].into_iter().flatten() {
        let mut r = std::io::BufReader::new(pipe);
        std::thread::spawn(move || {
            use std::io::BufRead;
            let mut line = String::new();
            loop {
                line.clear();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => DBG_PENDING.lock().unwrap().push_str(&line),
                }
            }
            DBG_PENDING.lock().unwrap().push_str("[debugger exited]\n");
            DBG_ACTIVE.store(false, Relaxed);
        });
    }
    {
        let mut r = std::io::BufReader::new(stderr);
        std::thread::spawn(move || {
            use std::io::BufRead;
            let mut line = String::new();
            loop {
                line.clear();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => DBG_PENDING.lock().unwrap().push_str(&line),
                }
            }
        });
    }
    *DBG_STDIN.lock().unwrap() = Some(stdin);
    *DBG_CHILD.lock().unwrap() = Some(child);
    DBG_ACTIVE.store(true, Relaxed);
    console_append(&format!(
        "==> lldb session on {bin}\n    F8 breakpoint  F10 over  F11 into  F9 continue  F7 variables\n"
    ));
    // Replay breakpoints, then run.
    let bps = BREAKPOINTS.lock().unwrap().clone();
    if bps.is_empty() {
        dbg_send("breakpoint set --name main");
    }
    for (f, l) in bps {
        let base = f.rsplit('/').next().unwrap_or(&f).to_string();
        dbg_send(&format!("breakpoint set --file {base} --line {l}"));
    }
    dbg_send("run");
}

fn dbg_send_impl(cmd: &str, echo: bool) {
    use std::io::Write;
    let mut g = DBG_STDIN.lock().unwrap();
    match g.as_mut() {
        Some(stdin) => {
            if writeln!(stdin, "{cmd}").and_then(|_| stdin.flush()).is_err() {
                console_append("debugger pipe closed\n");
            } else if echo {
                console_append(&format!("(lldb) {cmd}\n"));
            }
        }
        None => console_append("no debug session — Debug > Start Session (F5)\n"),
    }
}

fn dbg_send(cmd: &str) {
    dbg_send_impl(cmd, true);
}

/// Map a watch-box entry to an lldb command: a bare identifier path
/// (global / static / local name) reads best via `target variable`;
/// anything with operators goes through the expression evaluator.
fn watch_cmd(expr: &str) -> String {
    let ident = !expr.is_empty()
        && expr.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '.');
    if ident {
        format!("target variable {expr}")
    } else {
        format!("expression -- {expr}")
    }
}

/// Begin a sentinel-bracketed capture: `cmds` run back-to-back and
/// everything they print (minus pty echoes/prompts) lands in the
/// capture accumulator until the sentinel line arrives.
fn dbg_capture_begin(mode: u8, label: &str, cmd: &str) {
    if !DBG_ACTIVE.load(Relaxed) {
        console_append("no debug session — Debug > Start Session (F5)\n");
        return;
    }
    let sentinel_cmd = format!("script print(\"{VARS_SENTINEL}\")");
    {
        let mut cap = DBG_CAPTURE.lock().unwrap();
        if cap.mode != 0 {
            return; // one capture at a time; the next stop re-requests
        }
        cap.mode = mode;
        cap.acc.clear();
        cap.label = label.to_string();
        cap.cmds = vec![cmd.to_string(), sentinel_cmd.clone()];
    }
    dbg_send_impl(cmd, false);
    dbg_send_impl(&sentinel_cmd, false);
}

fn dbg_request_vars() {
    dbg_capture_begin(1, "", "frame variable");
}

fn watch_eval(expr: &str) {
    dbg_capture_begin(2, expr, &watch_cmd(expr));
}

/// Rebuild the Variables window text from the latest captures.
fn vars_render() {
    unsafe {
        let vb = VARS_BUF.load(Relaxed);
        if vb.is_null() {
            return;
        }
        let locals = VARS_LOCALS.lock().unwrap().clone();
        let watch = VARS_WATCH.lock().unwrap().clone();
        let mut t = String::from("== locals @ last stop (auto-refreshes) ==\n");
        if locals.trim().is_empty() {
            t.push_str("(none — hit a breakpoint or step; F5 starts a session)\n");
        } else {
            t.push_str(&locals);
        }
        t.push_str("\n== watch — type a global/expression below, Enter ==\n");
        if watch.is_empty() {
            t.push_str("(none yet — e.g. a static's name; uses `target variable`)\n");
        } else {
            t.push_str(&watch);
        }
        (*vb).text_const_i8_str(&t);
        let d = VARS_DISP.load(Relaxed);
        if !d.is_null() {
            (*(d as *mut Fl_Widget)).redraw();
        }
    }
}

/// A finished capture lands here (UI thread, from the pump).
fn vars_publish(mode: u8, label: String, acc: String) {
    if mode == 1 {
        *VARS_LOCALS.lock().unwrap() =
            if acc.trim().is_empty() { "(no locals in this frame)\n".into() } else { acc };
    } else {
        let mut w = VARS_WATCH.lock().unwrap();
        w.push_str(&format!(
            "{label} ->\n{}",
            if acc.trim().is_empty() { "  (no value — symbol not found?)\n".into() } else { acc }
        ));
        // Keep the tail; old watch results scroll away.
        if w.len() > 4000 {
            let cut = w.len() - 4000;
            let cut = w[cut..].find('\n').map(|i| cut + i + 1).unwrap_or(cut);
            *w = w[cut..].to_string();
        }
    }
    vars_render();
}

/// The Variables window: locals view + watch box. F7 opens it; while
/// it exists, every stop re-captures `frame variable` into it.
fn show_vars_window() {
    unsafe {
        let mut w = VARS_WIN.load(Relaxed);
        if w.is_null() {
            w = cxx_operator_new(core::mem::size_of::<Fl_Window>()) as *mut Fl_Window;
            Fl_Window::new_at(w, 460, 412, c"Variables".as_ptr());
            let vb = cxx_operator_new(core::mem::size_of::<Fl_Text_Buffer>())
                as *mut Fl_Text_Buffer;
            Fl_Text_Buffer::new_at(vb, 0, 1024);
            let d = cxx_operator_new(core::mem::size_of::<Fl_Text_Display>())
                as *mut Fl_Text_Display;
            Fl_Text_Display::new_at(d, 8, 8, 444, 364, c"".as_ptr());
            (*d).buffer(vb);
            (*d).textsize_i32(12);
            let wi = cxx_operator_new(core::mem::size_of::<WatchInput>()) as *mut WatchInput;
            wi.write(WatchInput::new(70, 378, 382, 26));
            (*w).as_fl_group_mut().end();
            (*w).as_fl_group_mut().add(d as *mut Fl_Widget);
            (*w).as_fl_group_mut().add(wi as *mut Fl_Widget);
            VARS_BUF.store(vb, Relaxed);
            VARS_DISP.store(d, Relaxed);
            VARS_WIN.store(w, Relaxed);
            vars_render();
        }
        (*(w as *mut Fl_Widget)).show();
    }
}

fn dbg_stop() {
    {
        let mut g = DBG_STDIN.lock().unwrap();
        if let Some(stdin) = g.as_mut() {
            use std::io::Write;
            let _ = writeln!(stdin, "process kill");
            let _ = writeln!(stdin, "quit");
            let _ = stdin.flush();
        }
        *g = None;
    }
    if let Some(mut c) = DBG_CHILD.lock().unwrap().take() {
        let _ = c.wait();
    }
    DBG_ACTIVE.store(false, Relaxed);
    *DBG_CURLINE.lock().unwrap() = None;
    {
        let mut cap = DBG_CAPTURE.lock().unwrap();
        cap.mode = 0;
        cap.acc.clear();
        cap.cmds.clear();
    }
    unsafe { restyle() };
    console_append("debug session stopped\n");
}

/// Toggle a breakpoint on the cursor's line of the current file.
fn dbg_toggle_breakpoint() {
    unsafe {
        let Some(path) = PATH.lock().unwrap().clone() else { return };
        let buf = BUF.load(Relaxed);
        let ed = ED.load(Relaxed);
        if buf.is_null() || ed.is_null() {
            return;
        }
        let pos = (*(ed as *mut Fl_Text_Display)).insert_position_ovl();
        let line = (*buf).count_lines(0, pos) + 1;
        let base = path.rsplit('/').next().unwrap_or(&path).to_string();
        // Compute under the lock, RELEASE, then talk to lldb/restyle —
        // restyle() takes BREAKPOINTS itself (std Mutex ≠ reentrant).
        let added = {
            let mut bps = BREAKPOINTS.lock().unwrap();
            if let Some(i) = bps.iter().position(|(f, l)| *f == path && *l == line) {
                bps.remove(i);
                false
            } else {
                bps.push((path.clone(), line));
                true
            }
        };
        if DBG_ACTIVE.load(Relaxed) {
            let verb = if added { "set" } else { "clear" };
            dbg_send(&format!("breakpoint {verb} --file {base} --line {line}"));
        }
        console_append(&format!(
            "breakpoint {}: {base}:{line}\n",
            if added { "set" } else { "removed" }
        ));
        restyle();
    }
}

/// Parse an lldb stop frame line: `... at <file>:<line>:<col>`.
fn parse_stop_location(s: &str) -> Option<(String, i32)> {
    let at = s.rfind(" at ")?;
    let rest = &s[at + 4..];
    let mut parts = rest.trim().split(':');
    let file = parts.next()?.to_string();
    let line: i32 = parts.next()?.trim().parse().ok()?;
    if file.is_empty() || line <= 0 {
        return None;
    }
    Some((file, line))
}

/// Drained from the main loop: stream lldb output to the console
/// (minus capture payloads), feed any active locals/watch capture,
/// and follow stop locations in the editor.
fn pump_debugger() {
    let pending = {
        let mut g = DBG_PENDING.lock().unwrap();
        if g.is_empty() {
            return;
        }
        std::mem::take(&mut *g)
    };
    // Route: with no capture in flight the raw stream goes straight
    // to the console. During a capture, payload lines accumulate for
    // the Variables window; pty echoes / prompts / the sentinel are
    // dropped; process events still pass through.
    let mut publish: Option<(u8, String, String)> = None;
    {
        let mut cap = DBG_CAPTURE.lock().unwrap();
        if cap.mode == 0 {
            console_append(&pending);
        } else {
            let mut to_console = String::new();
            for l in pending.lines() {
                let tt = l.trim_end_matches('\r').trim();
                if cap.mode != 0 {
                    // The prompt is written without a newline, so the
                    // sentinel often shares its line: "(lldb) --…end--".
                    // ends_with matches that but NOT the command echo,
                    // which ends in `")`. Must run before the echo skip.
                    if tt.ends_with(VARS_SENTINEL) && !tt.contains("script print(") {
                        publish = Some((
                            cap.mode,
                            std::mem::take(&mut cap.label),
                            std::mem::take(&mut cap.acc),
                        ));
                        cap.mode = 0;
                        cap.cmds.clear();
                        continue;
                    }
                    let echo = tt.starts_with("(lldb)")
                        || tt.contains("script print(")
                        || cap.cmds.iter().any(|c| tt == c || tt.ends_with(c.as_str()));
                    if echo {
                        continue;
                    }
                    let event = tt.starts_with("Process ")
                        || tt.starts_with("* thread")
                        || tt.starts_with("frame #")
                        || tt.starts_with("Target ")
                        || tt.starts_with("[debugger exited]");
                    if event {
                        to_console.push_str(l);
                        to_console.push('\n');
                    } else {
                        cap.acc.push_str(l.trim_end_matches('\r'));
                        cap.acc.push('\n');
                    }
                } else {
                    to_console.push_str(l);
                    to_console.push('\n');
                }
            }
            if !to_console.is_empty() {
                console_append(&to_console);
            }
        }
    }
    if let Some((mode, label, acc)) = publish {
        vars_publish(mode, label, acc);
    }
    // Follow the LAST stop location mentioned.
    let mut hit: Option<(String, i32)> = None;
    for l in pending.lines() {
        if l.contains("frame #0") || l.contains("stop reason") || l.contains(" at ") {
            if let Some(loc) = parse_stop_location(l) {
                hit = Some(loc);
            }
        }
    }
    if let Some((file, line)) = hit {
        *DBG_CURLINE.lock().unwrap() = Some((file.clone(), line));
        unsafe {
            // Land the editor on the stopped line: if the stop is in
            // a different OPEN file, switch tabs to it first.
            let cur = PATH.lock().unwrap().clone().unwrap_or_default();
            if cur.rsplit('/').next() != Some(file.as_str()) {
                let idx = OPEN_FILES
                    .lock()
                    .unwrap()
                    .iter()
                    .position(|(p, _)| p.rsplit('/').next() == Some(file.as_str()));
                if let Some(i) = idx {
                    switch_to_file(i);
                }
            }
            let cur = PATH.lock().unwrap().clone().unwrap_or_default();
            if cur.rsplit('/').next() == Some(file.as_str()) {
                let buf = BUF.load(Relaxed);
                let ed = ED.load(Relaxed);
                if !buf.is_null() && !ed.is_null() {
                    let pos = (*buf).skip_lines(0, line - 1);
                    let disp = ed as *mut Fl_Text_Display;
                    (*disp).insert_position(pos);
                    (*disp).show_insert_position();
                    restyle();
                    (*(ed as *mut Fl_Widget)).redraw();
                }
            }
        }
        // While the Variables window exists, every stop re-captures
        // the frame's locals into it.
        if DBG_ACTIVE.load(Relaxed) && !VARS_WIN.load(Relaxed).is_null() {
            dbg_request_vars();
        }
    }
}

/// Rebuild the tab strip from OPEN_FILES; the active file is marked.
fn tabs_refresh() {
    unsafe {
        let tabs = TABBAR.load(Relaxed);
        if tabs.is_null() {
            return;
        }
        let cur = PATH.lock().unwrap().clone();
        let files = OPEN_FILES.lock().unwrap().clone();
        let names: Vec<String> = files
            .iter()
            .map(|(p, _)| p.rsplit('/').next().unwrap_or(p).to_string())
            .collect();
        let active = files.iter().position(|(p, _)| Some(p) == cur.as_ref());
        let t = &mut *(tabs as *mut Fl_Tabs);
        if *TAB_NAMES.lock().unwrap() != names {
            // Open set changed: rebuild the pages. clear() C++-deletes
            // the children — they're plain imported Fl_Groups from
            // operator new, so that pairing is exact.
            t.as_fl_group_mut().clear();
            for name in &names {
                let pg =
                    cxx_operator_new(core::mem::size_of::<Fl_Group>()) as *mut Fl_Group;
                // Zero-height page right under the strip: the widget
                // is ALL tab bar; the shared editor lives outside.
                Fl_Group::new_at(pg, 220, 52, 960, 0, core::ptr::null());
                (*pg).end();
                (*(pg as *mut Fl_Widget)).copy_label_str(name);
                t.as_fl_group_mut().add(pg as *mut Fl_Widget);
            }
            *TAB_NAMES.lock().unwrap() = names;
        }
        if let Some(i) = active {
            let c = t.as_fl_group().child(i as i32);
            if !c.is_null() {
                t.value_mut_fl_widget(c);
            }
        }
        (*(tabs as *mut Fl_Widget)).redraw();
    }
}

fn close_current_file() {
    unsafe {
        let cur = PATH.lock().unwrap().clone();
        let Some(cur) = cur else { return };
        let next = {
            let mut files = OPEN_FILES.lock().unwrap();
            let Some(i) = files.iter().position(|(p, _)| *p == cur) else { return };
            files.remove(i); // buffer intentionally leaked (FLTK owns widgets-by-ptr idiom)
            if files.is_empty() { None } else { Some(i.min(files.len() - 1)) }
        };
        match next {
            Some(i) => switch_to_file(i),
            None => {
                // No files left: blank buffer.
                let buf = BUF.load(Relaxed);
                if !buf.is_null() {
                    (*buf).text_const_i8_str("");
                }
                *PATH.lock().unwrap() = None;
                DIRTY.store(false, Relaxed);
                refresh_title();
                tabs_refresh();
                nav_refresh();
            }
        }
        console_append(&format!("closed {cur}\n"));
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

/// File ▸ New Project flavors. Every RTOS flavor emits the same
/// self-contained scaffold (it carries all cores' run scripts and
/// linker maps); flavors differ only in the default Target selected.
const NEW_FLAVORS: [(&str, i32); 5] = [
    ("Host", 0),
    ("RAK11161", 1),             // dual-core: start on the STM32WLE5 side
    ("STM32", 4),
    ("ESP32", 3),                // C3-class default; Target menu flips to C2
    ("Raspberry Pi Pico", 5),
];

fn new_project_flow(flavor: usize) {
    let (name, tgt) = NEW_FLAVORS[flavor];
    let title = format!("New {name} project folder");
    if let Some(dir) = unsafe { choose_file(CHOOSER_DIR_NEW, &title) } {
        let r = if tgt == 0 { scaffold_host(&dir) } else { scaffold_project(&dir) };
        match r {
            Ok(()) => {
                TARGET.store(tgt, Relaxed);
                console_append(&format!(
                    "scaffolded {name} project at {dir}\ntarget = {}\n",
                    TARGET_NAMES[tgt as usize]
                ));
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
        0 => {
            // Local debugging, no qemu: build, then lldb the project's
            // own binary (name from Cargo.toml). Staticlib-only RTOS
            // projects have no host binary — say so instead.
            let cargo = std::fs::read_to_string(format!("{dir}/Cargo.toml")).unwrap_or_default();
            if cargo.contains("staticlib") {
                console_append(
                    "this project builds a firmware staticlib — no host binary to debug.\n\
                     Pick an RTOS target for qemu+gdb, or create a Host project.\n",
                );
                return;
            }
            let name = cargo
                .lines()
                .find_map(|l| {
                    let l = l.trim();
                    l.strip_prefix("name = \"")
                        .and_then(|r| r.strip_suffix('\"'))
                })
                .unwrap_or("rustcc_app")
                .to_string();
            (
                format!(
                    "cd '{dir}' && RUSTC=\"${{RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}}\" RUSTC_BOOTSTRAP=1 cargo +nightly build --release && lldb target/release/{name}"
                ),
                format!("lldb drives target/release/{name} directly in the Terminal window"),
            )
        }
        1 | 4 => (
            format!("cd '{dir}' && GDB=1 ./run_arm.sh"),
            format!("arm-none-eabi-gdb '{dir}/target/arm/firmware.elf' -ex 'target remote :1234' -ex 'break main' -ex continue"),
        ),
        5 => (
            format!("cd '{dir}' && GDB=1 ./run_pico.sh"),
            format!("arm-none-eabi-gdb '{dir}/target/pico/firmware.elf' -ex 'target remote :1234' -ex 'break main' -ex continue"),
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
    fs::write(root.join("build.rs"), HOST_BUILD_RS).map_err(werr)?;
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

const HOST_BUILD_RS: &str = r#"fn main() {
    // rustcc `class` types emit Itanium RTTI (`_ZTI…`) that
    // references the C++ ABI runtime (`__cxxabiv1::__class_type_info`
    // vtable). Optimized builds may strip it, but debug builds keep
    // it — link the platform C++ standard library.
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-lib=c++");
    #[cfg(not(target_os = "macos"))]
    println!("cargo:rustc-link-lib=stdc++");
}
"#;

const HOST_MAIN_RS: &str = r#"// Hello, world — rustcc fork edition.
//
// An ordinary Rust program whose greeter is a C++-ABI `class` (the
// fork's headline feature; vtable-real, zero crate-root boilerplate
// since v1.14). Build & run: Cmd+R in the rustcc IDE, or:
//   RUSTC=<fork-stage1>/bin/rustc cargo +nightly run --release

pub class Greeter {
    excitement: i32,

    pub constructor fn new(excitement: i32) -> Self {
        Greeter { excitement }
    }

    // Class methods are real C++ member functions (Itanium-mangled,
    // virtual = vtable slot), so their signatures use C++-compatible
    // types like i32…
    pub virtual fn excitement_level(&self) -> i32 {
        self.excitement
    }
}

// …while free functions live in ordinary Rust land — any types.
fn greeting(g: &Greeter) -> String {
    let bangs = "!".repeat(g.excitement_level().max(0) as usize);
    format!("Hello, world{bangs}")
}

// A global the debugger can read: type EXCITEMENT_BASE into the
// Variables window's watch box (F7) while stopped.
static EXCITEMENT_BASE: i32 = 2;

fn main() {
    println!("{}", greeting(&Greeter::new(EXCITEMENT_BASE + 1))); // → Hello, world!!!
    println!("{}", greeting(&Greeter::new(1))); // → Hello, world!
}
"#;

const HOST_TASKS_JSON: &str = r#"{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "rustcc: build (host)",
      "type": "shell",
      "command": "RUSTC=\"${RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}\" RUSTC_BOOTSTRAP=1 cargo +nightly build --release",
      "group": "build",
      "problemMatcher": ["$rustc"]
    },
    {
      "label": "rustcc: run (host)",
      "type": "shell",
      "command": "RUSTC=\"${RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}\" RUSTC_BOOTSTRAP=1 cargo +nightly run --release",
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
        ("run_pico.sh", fix(embed!("freertos_cpp/run_pico.sh"))),
        ("upload.toml", UPLOAD_TOML.to_string()),
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
        for s in ["run_arm.sh", "run_riscv.sh", "run_riscv_c2.sh", "run_pico.sh"] {
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

/// The whole menu as data: (path, shortcut, action). FLTK treats
/// EVERY '/' in the path as a submenu separator — a slash inside a
/// label like "(qemu/lldb)" silently splits into a bogus submenu, so
/// the self-test rejects any '/' between parentheses.
const MENU_SPEC: &[(&str, i32, usize)] = &[
    ("&File/&New File", MOD_META | 'n' as i32, ACT_NEW),
    ("&File/&Open File…", MOD_META | 'o' as i32, ACT_OPEN),
    ("&File/&Save", MOD_META | 's' as i32, ACT_SAVE),
    ("&File/Save &As…", MOD_META | MOD_SHIFT | 's' as i32, ACT_SAVE_AS),
    ("&File/New Project/&Host Project…", 0, ACT_NEW_HOST),
    ("&File/New Project/&RAK11161 Project…", MOD_META | MOD_SHIFT | 'n' as i32, ACT_NEW_PROJECT),
    ("&File/New Project/&STM32 Project…", 0, ACT_NEW_STM32),
    ("&File/New Project/&ESP32 Project…", 0, ACT_NEW_ESP32),
    ("&File/New Project/Raspberry Pi &Pico Project…", 0, ACT_NEW_PICO),
    ("&File/Open &Project…", MOD_META | MOD_SHIFT | 'o' as i32, ACT_OPEN_PROJECT),
    ("&File/&Close File", MOD_META | 'w' as i32, ACT_CLOSE_FILE),
    ("&File/&Quit", MOD_META | 'q' as i32, ACT_QUIT),
    ("&Edit/&Undo", MOD_META | 'z' as i32, ACT_UNDO),
    ("&Edit/&Redo", MOD_META | MOD_SHIFT | 'z' as i32, ACT_REDO),
    ("&Edit/Cu&t", MOD_META | 'x' as i32, ACT_CUT),
    ("&Edit/&Copy", MOD_META | 'c' as i32, ACT_COPY),
    ("&Edit/&Paste", MOD_META | 'v' as i32, ACT_PASTE),
    ("&Edit/Select &All", MOD_META | 'a' as i32, ACT_SELECT_ALL),
    ("&Edit/&Find…", MOD_META | 'f' as i32, ACT_FIND),
    ("F&ormat/&Wrap Lines", MOD_META | MOD_SHIFT | 'w' as i32, ACT_WRAP),
    ("F&ormat/Bigger", MOD_META | '=' as i32, ACT_FONT_UP),
    ("F&ormat/Smaller", MOD_META | '-' as i32, ACT_FONT_DOWN),
    // --- IDE menus ---
    ("&Project/&Build", MOD_META | 'b' as i32, ACT_BUILD),
    ("&Project/Build && &Run", MOD_META | 'r' as i32, ACT_BUILD_RUN),
    ("&Project/&Debug…", MOD_META | MOD_SHIFT | 'd' as i32, ACT_DEBUG),
    ("&Project/&Upload Firmware", MOD_META | 'u' as i32, ACT_UPLOAD),
    ("&Project/Edit Upload Co&nfig…", 0, ACT_UPLOAD_CFG),
    ("&Project/&Clear Console", 0, ACT_CONSOLE_CLEAR),
    ("&Debug/&Start Session", KEY_F + 5, ACT_DBG_START),
    ("&Debug/Toggle &Breakpoint @ cursor", KEY_F + 8, ACT_DBG_BREAKPOINT),
    ("&Debug/Step &Over", KEY_F + 10, ACT_DBG_STEP_OVER),
    ("&Debug/Step &Into", KEY_F + 11, ACT_DBG_STEP_IN),
    ("&Debug/Step Ou&t", MOD_SHIFT | (KEY_F + 11), ACT_DBG_STEP_OUT),
    ("&Debug/&Continue", KEY_F + 9, ACT_DBG_CONTINUE),
    ("&Debug/Show &Variables", KEY_F + 7, ACT_DBG_VARS),
    ("&Debug/Sto&p Session", MOD_SHIFT | (KEY_F + 5), ACT_DBG_STOP),
    ("&Target/&Host (LLVM)", 0, ACT_TGT_BASE),
    ("&Target/RAK11161: &STM32WLE5 (Cortex-M4)", 0, ACT_TGT_BASE + 1),
    ("&Target/RAK11161: &ESP8684 (ESP32-C2, rv32imc)", 0, ACT_TGT_BASE + 2),
    ("&Target/ESP32-&C3-class (rv32imac)", 0, ACT_TGT_BASE + 3),
    ("&Target/STM32&F4-class (Cortex-M4F)", 0, ACT_TGT_BASE + 4),
    ("&Target/Raspberry Pi &Pico (RP2040)", 0, ACT_TGT_BASE + 5),
    ("&Help/rustcc IDE &Help…", KEY_F + 1, ACT_HELP),
    ("&Help/&About rustcc IDE", 0, ACT_ABOUT),
];

unsafe fn add_menu_items(bar: *mut Fl_Menu_Bar) {
    unsafe {
        let m = (*bar).as_fl_menu__mut();
        for &(label, shortcut, act) in MENU_SPEC {
            m.add(cstr(label).as_ptr(), shortcut, Some(menu_cb), act as *mut (), 0);
        }
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
        ed.write(RustEditor::new(220, 52, 960, 406));
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

        // Tab strip for open files: a real Fl_Tabs via the fork
        // subclass. Its Fl_Group ctor leaves itself as the "current"
        // group (begin()), and the by-value construct + move would
        // leave that pointing at the dead temporary — clear it.
        let tabs = cxx_operator_new(core::mem::size_of::<FileTabs>()) as *mut FileTabs;
        tabs.write(FileTabs::new(220, 28, 960, 24));
        Fl_Group::current_mut_fl_group(core::ptr::null_mut());
        TABBAR.store(tabs, Relaxed);

        (*(ed as *mut Fl_Text_Display)).linenumber_width(36);

        let g = (*win).as_fl_group_mut();
        g.add(bar as *mut Fl_Widget);
        g.add(tabs as *mut Fl_Widget);
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
        // Menu invariants: FLTK splits labels at EVERY '/', so a
        // slash inside a parenthesized label (e.g. "(qemu/lldb)")
        // silently becomes a bogus submenu — reject it. Also pin the
        // Help menu + a slash-free Project ▸ Debug… leaf.
        check("menu labels: no '/' inside parentheses", {
            MENU_SPEC.iter().all(|&(label, _, _)| {
                let mut depth = 0i32;
                label.chars().all(|c| match c {
                    '(' => {
                        depth += 1;
                        true
                    }
                    ')' => {
                        depth -= 1;
                        true
                    }
                    '/' => depth == 0,
                    _ => true,
                })
            })
        });
        check(
            "menu: Help present, Project Debug is short",
            MENU_SPEC.iter().any(|&(l, _, a)| l.starts_with("&Help/") && a == ACT_HELP)
                && MENU_SPEC.iter().any(|&(l, _, a)| a == ACT_ABOUT && l.starts_with("&Help/"))
                && MENU_SPEC.iter().any(|&(l, _, a)| a == ACT_DEBUG && l == "&Project/&Debug…"),
        );
        check(
            "help text covers projects/debugger/editing bindings",
            ["Cmd+B", "Cmd+R", "Cmd+U", "F5", "F8", "F10", "Ctrl+Space", "Cmd+W"]
                .iter()
                .all(|n| HELP_TEXT.contains(n)),
        );
        // Watch-box command mapping: bare identifier paths (globals)
        // read via `target variable`; expressions via `expression`.
        check(
            "watch cmd mapping",
            watch_cmd("EXCITEMENT_BASE") == "target variable EXCITEMENT_BASE"
                && watch_cmd("foo::BAR") == "target variable foo::BAR"
                && watch_cmd("g.excitement") == "target variable g.excitement"
                && watch_cmd("1 + 2") == "expression -- 1 + 2",
        );
        // New Project must offer every board family the Target menu
        // knows, each mapped to a valid default target.
        check("new-project flavors cover STM32/ESP32/Pico/RAK/Host", {
            let names: Vec<&str> = NEW_FLAVORS.iter().map(|f| f.0).collect();
            ["Host", "RAK11161", "STM32", "ESP32"].iter().all(|n| names.contains(n))
                && names.iter().any(|n| n.contains("Pico"))
                && NEW_FLAVORS
                    .iter()
                    .all(|&(_, t)| (t as usize) < TARGET_NAMES.len())
                && NEW_FLAVORS.iter().filter(|&&(_, t)| t == 0).count() == 1
        });
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
        check("host template = hello-world class", hm.contains("pub class Greeter") && hm.contains("Hello, world"));
        check(
            "host build cmd carries fork RUSTC",
            target_cmdline(0, false).contains("RUSTC="),
        );
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

        // 15. IDE v3: new targets routed to the right scripts.
        check(
            "STM32F4 target -> run_arm.sh",
            target_cmdline(4, true).contains("run_arm.sh"),
        );
        check(
            "Pico target -> run_pico.sh",
            target_cmdline(5, true).contains("run_pico.sh"),
        );
        check(
            "scaffold ships run_pico.sh",
            {
                let p3 = std::env::temp_dir().join(format!("rustcc_ide_pico_{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&p3);
                let ok = scaffold_project(&p3.to_string_lossy()).is_ok()
                    && p3.join("run_pico.sh").exists();
                let _ = std::fs::remove_dir_all(&p3);
                ok
            },
        );

        // 16. IDE v3: completion — open a buffer with known idents,
        //     place the cursor after a prefix, complete, apply.
        let cf = std::env::temp_dir().join("rustcc_ide_complete.rs");
        std::fs::write(&cf, "fn grandiose_identifier() {}\nfn main() { gran }\n").unwrap();
        open_in_editor(&cf.to_string_lossy());
        let ed2 = ED.load(Relaxed);
        let text_now = {
            let b = BUF.load(Relaxed);
            CStr::from_ptr((*b).text()).to_string_lossy().into_owned()
        };
        let cursor = text_now.find("gran }").unwrap() as i32 + 4;
        (*(ed2 as *mut Fl_Text_Display)).insert_position(cursor);
        show_completions();
        check(
            "completion candidates found",
            COMPLETE_ITEMS
                .lock()
                .unwrap()
                .iter()
                .any(|c| c == "grandiose_identifier"),
        );
        apply_completion();
        let after = {
            let b = BUF.load(Relaxed);
            CStr::from_ptr((*b).text()).to_string_lossy().into_owned()
        };
        check(
            "completion applied",
            after.contains("{ grandiose_identifier }"),
        );
        let _ = std::fs::remove_file(&cf);

        // 17. IDE v4: upload config + routing + substitution.
        let up = std::env::temp_dir().join(format!("rustcc_ide_up_{}", std::process::id()));
        let up_s = up.to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&up);
        check("scaffold (upload) ok", scaffold_project(&up_s).is_ok());
        check("scaffold ships upload.toml", up.join("upload.toml").exists());
        let cfg = std::fs::read_to_string(up.join("upload.toml")).unwrap_or_default();
        check(
            "upload tools configured",
            upload_cfg_get(&cfg, "stm32", "cmd")
                .is_some_and(|c| c.contains("STM32_Programmer_CLI"))
                && upload_cfg_get(&cfg, "esp32", "cmd").is_some_and(|c| c.contains("esptool.py"))
                && upload_cfg_get(&cfg, "pico", "cmd").is_some_and(|c| c.contains("picotool")),
        );
        check(
            "upload routes per target",
            upload_route(1) == Some(("stm32", "arm"))
                && upload_route(2) == Some(("esp32", "riscv-c2"))
                && upload_route(5) == Some(("pico", "pico"))
                && upload_route(0).is_none(),
        );
        check(
            "placeholder substitution",
            upload_cfg_get(&cfg, "pico", "cmd")
                .map(|c| c.replace("{elf}", "/x/fw.elf"))
                .is_some_and(|c| c.contains("load /x/fw.elf")),
        );
        // Host target upload → friendly message, no spawn.
        *PROJECT_DIR.lock().unwrap() = Some(up_s.clone());
        TARGET.store(0, Relaxed);
        upload_firmware();
        // Missing-ELF guard for an RTOS target.
        TARGET.store(5, Relaxed);
        upload_firmware();
        check("upload guards ran", true);
        let _ = std::fs::remove_dir_all(&up);

        // 18. IDE v5: debugger plumbing (no live lldb needed).
        check(
            "parse lldb stop location",
            parse_stop_location(
                "    frame #0: 0x0001 app`main at main.rs:23:5"
            ) == Some(("main.rs".to_string(), 23)),
        );
        let bf = std::env::temp_dir().join("rustcc_ide_bp.rs");
        std::fs::write(&bf, "fn main() {\n    let x = 1;\n    let y = 2;\n}\n").unwrap();
        open_in_editor(&bf.to_string_lossy());
        let ed3 = ED.load(Relaxed);
        let b3 = BUF.load(Relaxed);
        (*(ed3 as *mut Fl_Text_Display)).insert_position((*b3).skip_lines(0, 1));
        dbg_toggle_breakpoint();
        check(
            "breakpoint recorded",
            BREAKPOINTS.lock().unwrap().iter().any(|(_, l)| *l == 2),
        );
        // The style buffer is a PARALLEL byte array (no newlines) —
        // assert the exact byte range of line 2 is marked.
        let sb3 = STYLE_BUF.load(Relaxed);
        let st = CStr::from_ptr((*sb3).text()).to_string_lossy().into_owned();
        let l2_start = (*b3).skip_lines(0, 1) as usize;
        let l2_end = (*b3).skip_lines(0, 2) as usize - 1; // exclude newline
        check(
            "breakpoint line marked F",
            st.get(l2_start..l2_end).is_some_and(|s| s.chars().all(|c| c == 'F')),
        );
        dbg_toggle_breakpoint();
        check("breakpoint cleared", BREAKPOINTS.lock().unwrap().is_empty());

        // 19. IDE v5/v6: a real Fl_Tabs strip mirrors OPEN_FILES —
        //     one zero-height page per file, selection == active file.
        if TABBAR.load(Relaxed).is_null() {
            let tw = cxx_operator_new(core::mem::size_of::<FileTabs>()) as *mut FileTabs;
            tw.write(FileTabs::new(220, 28, 960, 24));
            Fl_Group::current_mut_fl_group(core::ptr::null_mut());
            TABBAR.store(tw, Relaxed);
        }
        tabs_refresh();
        let tab_count_before = OPEN_FILES.lock().unwrap().len();
        check("tabs track open files", tab_count_before >= 2);
        {
            let tw = TABBAR.load(Relaxed) as *mut Fl_Tabs;
            check(
                "Fl_Tabs pages mirror open files",
                (*tw).as_fl_group().children() as usize == tab_count_before,
            );
            let cur = PATH.lock().unwrap().clone();
            let files = OPEN_FILES.lock().unwrap().clone();
            let act = files.iter().position(|(p, _)| Some(p) == cur.as_ref());
            check(
                "Fl_Tabs selection is the active file",
                act.is_some_and(|i| (*tw).value() == (*tw).as_fl_group().child(i as i32)),
            );
        }
        close_current_file();
        check(
            "close removed a tab",
            OPEN_FILES.lock().unwrap().len() == tab_count_before - 1,
        );
        check(
            "Fl_Tabs page count follows close",
            (*(TABBAR.load(Relaxed) as *mut Fl_Tabs)).as_fl_group().children() as usize
                == tab_count_before - 1,
        );
        let _ = std::fs::remove_file(&bf);

        // 20. FULL gate: real lldb session — breakpoint hit, variables
        //     visible, step, continue to exit.
        if std::env::var("RUSTCC_IDE_SELFTEST_FULL").as_deref() == Ok("1") {
            let hp = std::env::temp_dir().join(format!("rustcc_ide_lldb_{}", std::process::id()));
            let hp_s = hp.to_string_lossy().into_owned();
            let _ = std::fs::remove_dir_all(&hp);
            scaffold_host(&hp_s).unwrap();
            *PROJECT_DIR.lock().unwrap() = Some(hp_s.clone());
            TARGET.store(0, Relaxed);
            open_in_editor(&format!("{hp_s}/src/main.rs"));
            // Breakpoint on the println! line inside main().
            let b5 = BUF.load(Relaxed);
            let raw5 = CStr::from_ptr((*b5).text()).to_string_lossy().into_owned();
            let line_no = raw5
                .lines()
                .position(|l| l.contains("println!"))
                .map(|i| i as i32 + 1)
                .unwrap_or(1);
            BREAKPOINTS.lock().unwrap().push((format!("{hp_s}/src/main.rs"), line_no));
            dbg_start();
            // Async lldb: only ever look at output NEWER than the
            // last command, and wait for each STOP before sending
            // the next command.
            let console_len = || {
                let cb = CONSOLE_BUF.load(Relaxed);
                CStr::from_ptr((*cb).text()).to_bytes().len()
            };
            let wait_from = |from: usize, needle: &str, secs: u32| -> bool {
                for _ in 0..secs * 10 {
                    pump_debugger();
                    {
                        let cb = CONSOLE_BUF.load(Relaxed);
                        let txt =
                            CStr::from_ptr((*cb).text()).to_string_lossy().into_owned();
                        if txt.get(from.min(txt.len())..).is_some_and(|s| s.contains(needle)) {
                            return true;
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                false
            };
            check("lldb: breakpoint hit", wait_from(0, "stop reason = breakpoint", 90));
            // The stop must paint: editor follows the location and the
            // style overlay marks the stopped line 'G' (amber).
            check("lldb: stopped line tinted amber", {
                let b5b = BUF.load(Relaxed);
                let sb = STYLE_BUF.load(Relaxed);
                let st = CStr::from_ptr((*sb).text()).to_string_lossy().into_owned();
                let ls = (*b5b).skip_lines(0, line_no - 1) as usize;
                let le = (*b5b).skip_lines(0, line_no) as usize - 1;
                ls < le && st.get(ls..le).is_some_and(|s| s.chars().all(|c| c == 'G'))
            });
            // Argument evaluation runs the fork-emitted C++ ctor
            // first: step-in lands in Greeter::Greeter(excitement=…).
            let m = console_len();
            dbg_send("thread step-in");
            check("lldb: stepped", wait_from(m, "stop reason = step in", 20));
            let m = console_len();
            dbg_send("frame variable");
            check("lldb: variables visible", wait_from(m, "excitement", 15));
            // Variables window: capture the ctor frame's locals, then
            // watch the template's GLOBAL via `target variable`.
            show_vars_window();
            dbg_request_vars();
            let wait_text = |get: &dyn Fn() -> String, needle: &str, secs: u32| -> bool {
                for _ in 0..secs * 10 {
                    pump_debugger();
                    if get().contains(needle) {
                        return true;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                false
            };
            check(
                "vars window captured frame locals",
                wait_text(&|| VARS_LOCALS.lock().unwrap().clone(), "excitement", 15),
            );
            watch_eval("EXCITEMENT_BASE");
            check(
                "watch read a global (target variable)",
                wait_text(&|| VARS_WATCH.lock().unwrap().clone(), "EXCITEMENT_BASE = 2", 15),
            );
            let m = console_len();
            dbg_send("breakpoint disable");
            check("lldb: bp disabled", wait_from(m, "disabled", 10));
            let m = console_len();
            dbg_send("continue");
            check("lldb: ran to exit", wait_from(m, "exited", 25));
            dbg_stop();
            let _ = std::fs::remove_dir_all(&hp);
        }

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
            // Custom event loop: FLTK events + the debugger pump
            // (lldb output arrives from a reader thread and must be
            // drained on the UI thread).
            loop {
                Fl::wait_f64(0.05);
                pump_debugger();
                if Fl::first_window().is_null() {
                    break;
                }
            }
            0
        }
    };
    std::process::exit(rc);
}
