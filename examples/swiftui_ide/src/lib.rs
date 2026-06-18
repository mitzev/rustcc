//! Rust engine for the SwiftUI IDE demo, exposed to Swift through the
//! rustcc fork's `swiftcc` ABI (`extern "Swift"`).
//!
//! ## Architecture — the INVERSE of `examples/rustcc_ide`
//!
//! ```text
//!   ┌──────────────────────────┐  swiftcc (extern "Swift")  ┌──────────────┐
//!   │  SwiftUI app (Swift)     │  ─────────────────────────▶ │  Rust engine │
//!   │  @main · View · @State   │   @_silgen_name("rc_*")      │  (this lib)  │
//!   └──────────────────────────┘  ◀───────────────────────── └──────────────┘
//!        the whole UI              C-strings / file lists       scaffold·build
//!                                   / streamed console           ·run·project
//! ```
//!
//! The FLTK IDE (`examples/rustcc_ide`) is written ENTIRELY in fork
//! Rust — every widget is a `class` subclassing imported FLTK C++. A
//! SwiftUI IDE cannot work that way: SwiftUI's surface (`@main`, the
//! `View` protocol with `some View`, `@ViewBuilder` result builders,
//! `@State`) is Swift-compiler machinery that crosses no ABI, so the
//! view tree MUST stay in Swift. What carries over is the IDE
//! *engine* — scaffolding, the per-target build/run command, process
//! streaming, the project/file model — which was never FLTK-specific.
//! That engine lives here and is reached over `extern "Swift"`.
//!
//! ## The bridge (beyond `swiftui_counter`)
//!
//! `swiftui_counter` passes only `i64` scalars. An IDE needs strings
//! and lists, so every entry point here is `extern "Swift"` taking
//! C-strings in (`*const c_char`, NUL-terminated — Swift's
//! `String.withCString`) and returning Rust-owned C-strings out
//! (`*mut c_char`, released via `rc_string_free`). File lists are
//! newline-joined; the streamed build console is a drain buffer the
//! SwiftUI side polls on a timer (there is no FLTK event loop to
//! pump). Pointers ride swiftcc in integer class, so the ABI is the
//! same mechanism the `swift_extern_call` probe validates — just
//! exercised with real data instead of three integers.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Mutex;

// ---------------------------------------------------------------------
// Engine state
// ---------------------------------------------------------------------

/// The 6 build targets the IDE understands (copied from the FLTK IDE's
/// `TARGET_NAMES` so the two front-ends agree on the target matrix).
const TARGET_NAMES: [&str; 6] = [
    "Host (LLVM backend)",
    "RAK11161 — STM32WLE5 core (Cortex-M4, FreeRTOS, qemu mps2)",
    "RAK11161 — ESP8684 / ESP32-C2 (rv32imc, FreeRTOS, qemu virt)",
    "ESP32-C3-class (rv32imac, FreeRTOS, qemu virt)",
    "STM32F4-class (Cortex-M4F, FreeRTOS, qemu mps2)",
    "Raspberry Pi Pico (RP2040, Cortex-M0+, FreeRTOS, qemu mps2)",
];

static PROJECT_DIR: Mutex<Option<String>> = Mutex::new(None);
static CONSOLE: Mutex<String> = Mutex::new(String::new());
static RUNNING: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------
// Pure-logic core (no ABI concerns — unit-testable as plain Rust)
// ---------------------------------------------------------------------

mod engine {
    use super::*;
    use std::path::Path;

    pub fn console_append(s: &str) {
        CONSOLE.lock().unwrap().push_str(s);
    }

    /// Per-target build/run command, executed in the project root —
    /// byte-for-byte the FLTK IDE's `target_cmdline` so a project
    /// scaffolded by either IDE builds identically. `run=false` stops
    /// after the link (`SKIP_QEMU=1` for the RTOS run scripts).
    pub fn target_cmdline(target: i64, run: bool) -> String {
        let skip = if run { "" } else { "SKIP_QEMU=1 " };
        match target {
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

    /// Scaffold a host (LLVM-target) fork-Rust project: a `class
    /// Greeter` Hello World + a build.rs linking the C++ runtime +
    /// VSCode tasks pinning the fork `RUSTC`. Mirrors the FLTK IDE's
    /// `scaffold_host`.
    pub fn scaffold_host(dir: &str) -> Result<(), String> {
        let root = Path::new(dir);
        let werr = |e: std::io::Error| e.to_string();
        std::fs::create_dir_all(root.join("src")).map_err(werr)?;
        std::fs::create_dir_all(root.join(".vscode")).map_err(werr)?;
        std::fs::write(root.join("Cargo.toml"), HOST_CARGO_TOML).map_err(werr)?;
        std::fs::write(root.join("build.rs"), HOST_BUILD_RS).map_err(werr)?;
        std::fs::write(root.join("src/main.rs"), HOST_MAIN_RS).map_err(werr)?;
        std::fs::write(root.join(".vscode/tasks.json"), HOST_TASKS_JSON).map_err(werr)?;
        std::fs::write(root.join("README.md"), HOST_README).map_err(werr)?;
        Ok(())
    }

    /// Source-ish files in `dir`, two levels deep, project-relative,
    /// sorted. (Same filter spirit as the FLTK IDE's `collect_files`.)
    pub fn list_files(dir: &str) -> Vec<String> {
        const EXTS: &[&str] = &[
            "rs", "toml", "c", "h", "cpp", "hpp", "ld", "md", "json", "sh", "swift",
        ];
        fn walk(base: &Path, cur: &Path, depth: u32, out: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(cur) else { return };
            for ent in rd.flatten() {
                let p = ent.path();
                let name = ent.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || name == "target" {
                    continue;
                }
                if p.is_dir() {
                    if depth < 2 {
                        walk(base, &p, depth + 1, out);
                    }
                } else if p
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| EXTS.contains(&e))
                {
                    if let Ok(rel) = p.strip_prefix(base) {
                        out.push(rel.to_string_lossy().into_owned());
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(Path::new(dir), Path::new(dir), 0, &mut out);
        out.sort();
        out
    }

    /// Spawn `cmdline` in `dir` on a background thread, streaming
    /// merged stdout+stderr line-by-line into the CONSOLE drain
    /// buffer. Adapts the FLTK IDE's `run_streamed` to a pull model
    /// (no `Fl::check()` pump — SwiftUI polls `rc_console_drain`).
    pub fn spawn_streamed(dir: &str, cmdline: &str) {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let dir = dir.to_string();
        let cmdline = cmdline.to_string();
        RUNNING.store(true, Relaxed);
        std::thread::spawn(move || {
            let child = Command::new("bash")
                .arg("-c")
                .arg(format!("cd '{dir}' && {cmdline} 2>&1"))
                .stdout(Stdio::piped())
                .stdin(Stdio::null())
                .spawn();
            match child {
                Ok(mut child) => {
                    if let Some(out) = child.stdout.take() {
                        let mut reader = BufReader::new(out);
                        let mut line = String::new();
                        loop {
                            line.clear();
                            match reader.read_line(&mut line) {
                                Ok(0) | Err(_) => break,
                                Ok(_) => console_append(&line),
                            }
                        }
                    }
                    let code = child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
                    console_append(&format!("[exit {code}]\n"));
                }
                Err(e) => console_append(&format!("spawn failed: {e}\n")),
            }
            RUNNING.store(false, Relaxed);
        });
    }
}

const HOST_CARGO_TOML: &str = r#"[package]
name = "rustcc_app"
version = "0.1.0"
edition = "2021"

[workspace]
"#;

const HOST_BUILD_RS: &str = r#"fn main() {
    // rustcc `class` types emit Itanium RTTI (`_ZTI…`) referencing the
    // C++ ABI runtime; debug builds keep it, so link the platform C++
    // standard library.
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-lib=c++");
    #[cfg(not(target_os = "macos"))]
    println!("cargo:rustc-link-lib=stdc++");
}
"#;

const HOST_MAIN_RS: &str = r#"// Hello, world — rustcc fork edition (scaffolded by the SwiftUI IDE).
//
// An ordinary Rust program whose greeter is a C++-ABI `class` (the
// fork's headline feature). Build & run: the IDE's ▶ Run button, or:
//   RUSTC=<fork-stage1>/bin/rustc RUSTC_BOOTSTRAP=1 cargo +nightly run --release

pub class Greeter {
    excitement: i32,

    pub constructor fn new(excitement: i32) -> Self {
        Greeter { excitement }
    }

    // Class methods are real C++ member functions (Itanium-mangled,
    // virtual = vtable slot), so they use C++-compatible types (i32)…
    pub virtual fn excitement_level(&self) -> i32 {
        self.excitement
    }
}

// …while free functions live in ordinary Rust land — any types.
fn greeting(g: &Greeter) -> String {
    let bangs = "!".repeat(g.excitement_level().max(0) as usize);
    format!("Hello from SwiftUI + rustcc{bangs}")
}

fn main() {
    println!("{}", greeting(&Greeter::new(3))); // → …rustcc!!!
    println!("{}", greeting(&Greeter::new(1))); // → …rustcc!
}
"#;

const HOST_TASKS_JSON: &str = r#"{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "rustcc: build (host)",
      "type": "shell",
      "command": "RUSTC=\"${RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}\" RUSTC_BOOTSTRAP=1 cargo +nightly build --release",
      "group": "build"
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

Scaffolded by the **SwiftUI rustcc IDE**. Build with the fork:

```sh
RUSTC=<fork-stage1>/bin/rustc RUSTC_BOOTSTRAP=1 cargo +nightly run --release
```
"#;

// ---------------------------------------------------------------------
// C-string marshaling helpers
// ---------------------------------------------------------------------

/// Hand a Rust string to the caller as an owned NUL-terminated
/// C-string. The caller MUST return it via `rc_string_free`.
fn out_cstring(s: String) -> *mut c_char {
    CString::new(s.replace('\0', "")).unwrap_or_default().into_raw()
}

/// Borrow a caller-provided C-string as `&str` (lossy on bad UTF-8).
///
/// # Safety
/// `p` must be a valid NUL-terminated C-string for the call's
/// duration, or null.
unsafe fn in_str(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------
// The `swiftcc` API the SwiftUI app calls
// ---------------------------------------------------------------------
//
// Every entry point is `extern "Swift"` (swiftcc) with a stable
// `#[export_name]`; the Swift side binds via `@_silgen_name("rc_…")`.
// They are thin wrappers over `engine::*` so the logic stays plain,
// unit-testable Rust (see the tests below).

/// Number of build targets.
#[export_name = "rc_target_count"]
pub extern "Swift" fn rc_target_count() -> i64 {
    TARGET_NAMES.len() as i64
}

/// Display name of target `i` (caller frees).
#[export_name = "rc_target_name"]
pub extern "Swift" fn rc_target_name(i: i64) -> *mut c_char {
    let name = TARGET_NAMES.get(i as usize).copied().unwrap_or("");
    out_cstring(name.to_string())
}

/// Scaffold a new project. `kind`: 0 = Host. Returns 0 on success,
/// -1 on error (message goes to the console).
#[export_name = "rc_scaffold"]
pub extern "Swift" fn rc_scaffold(kind: i64, dir: *const c_char) -> i64 {
    let dir = unsafe { in_str(dir) };
    if dir.is_empty() {
        return -1;
    }
    let r = match kind {
        0 => engine::scaffold_host(&dir),
        _ => Err(format!("scaffold kind {kind} not yet ported to the SwiftUI IDE")),
    };
    match r {
        Ok(()) => {
            *PROJECT_DIR.lock().unwrap() = Some(dir.clone());
            engine::console_append(&format!("scaffolded host project at {dir}\n"));
            0
        }
        Err(e) => {
            engine::console_append(&format!("scaffold FAILED: {e}\n"));
            -1
        }
    }
}

/// Open an existing project directory. Returns the file count.
#[export_name = "rc_open"]
pub extern "Swift" fn rc_open(dir: *const c_char) -> i64 {
    let dir = unsafe { in_str(dir) };
    if dir.is_empty() || !std::path::Path::new(&dir).is_dir() {
        engine::console_append("open: not a directory\n");
        return -1;
    }
    let n = engine::list_files(&dir).len() as i64;
    *PROJECT_DIR.lock().unwrap() = Some(dir.clone());
    engine::console_append(&format!("project = {dir} ({n} files)\n"));
    n
}

/// Newline-joined, project-relative file list of the open project
/// (caller frees). Empty string if no project is open.
#[export_name = "rc_list_files"]
pub extern "Swift" fn rc_list_files() -> *mut c_char {
    let dir = PROJECT_DIR.lock().unwrap().clone();
    let body = match dir {
        Some(d) => engine::list_files(&d).join("\n"),
        None => String::new(),
    };
    out_cstring(body)
}

/// Contents of project-relative file `rel` (caller frees).
#[export_name = "rc_read_file"]
pub extern "Swift" fn rc_read_file(rel: *const c_char) -> *mut c_char {
    let rel = unsafe { in_str(rel) };
    let dir = PROJECT_DIR.lock().unwrap().clone();
    let body = match dir {
        Some(d) => std::fs::read_to_string(std::path::Path::new(&d).join(&rel))
            .unwrap_or_else(|e| format!("<cannot read {rel}: {e}>")),
        None => String::new(),
    };
    out_cstring(body)
}

/// Write `body` to project-relative file `rel`. Returns 0 on success.
#[export_name = "rc_save_file"]
pub extern "Swift" fn rc_save_file(rel: *const c_char, body: *const c_char) -> i64 {
    let rel = unsafe { in_str(rel) };
    let body = unsafe { in_str(body) };
    let Some(d) = PROJECT_DIR.lock().unwrap().clone() else {
        return -1;
    };
    match std::fs::write(std::path::Path::new(&d).join(&rel), body) {
        Ok(()) => {
            engine::console_append(&format!("saved {rel}\n"));
            0
        }
        Err(e) => {
            engine::console_append(&format!("save {rel} FAILED: {e}\n"));
            -1
        }
    }
}

/// Build (`run=0`) or build-and-run (`run=1`) the open project for
/// `target`, streaming output into the console drain. Returns 0 if
/// started, -1 if busy or no project is open.
#[export_name = "rc_build"]
pub extern "Swift" fn rc_build(target: i64, run: i64) -> i64 {
    if RUNNING.load(Relaxed) {
        engine::console_append("build already running\n");
        return -1;
    }
    let Some(dir) = PROJECT_DIR.lock().unwrap().clone() else {
        engine::console_append("no project open\n");
        return -1;
    };
    let verb = if run != 0 { "run" } else { "build" };
    let name = TARGET_NAMES.get(target as usize).copied().unwrap_or("?");
    engine::console_append(&format!("==> {verb} [{name}]\n"));
    engine::spawn_streamed(&dir, &engine::target_cmdline(target, run != 0));
    0
}

/// 1 while a build/run is in flight, else 0.
#[export_name = "rc_is_running"]
pub extern "Swift" fn rc_is_running() -> i64 {
    RUNNING.load(Relaxed) as i64
}

/// Return and CLEAR the pending console text (caller frees). Polled by
/// the SwiftUI front-end on a timer.
#[export_name = "rc_console_drain"]
pub extern "Swift" fn rc_console_drain() -> *mut c_char {
    let mut g = CONSOLE.lock().unwrap();
    let taken = std::mem::take(&mut *g);
    out_cstring(taken)
}

/// Free a C-string previously returned by an `rc_*` function.
///
/// # Safety
/// `p` must be a pointer returned by one of this library's `rc_*`
/// functions and not yet freed.
#[export_name = "rc_string_free"]
pub extern "Swift" fn rc_string_free(p: *mut c_char) {
    if !p.is_null() {
        drop(unsafe { CString::from_raw(p) });
    }
}

// ---------------------------------------------------------------------
// Headless self-test: exercises the engine through the exact
// `extern "Swift"` entry points the SwiftUI app links against — no
// Swift toolchain, no GUI. Validates the Rust half end-to-end.
// ---------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a returned C-string into an owned Rust String,
    /// freeing it the way Swift would.
    unsafe fn take(p: *mut c_char) -> String {
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        rc_string_free(p);
        s
    }

    fn cstr(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    #[test]
    fn target_table_matches_fltk_ide() {
        assert_eq!(rc_target_count(), 6);
        let host = unsafe { take(rc_target_name(0)) };
        assert!(host.starts_with("Host"), "got {host:?}");
        let c2 = unsafe { take(rc_target_name(2)) };
        assert!(c2.contains("ESP32-C2"), "got {c2:?}");
    }

    #[test]
    fn scaffold_open_read_save_flow() {
        let dir = std::env::temp_dir().join(format!("swiftui_ide_t{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dirc = cstr(dir.to_str().unwrap());

        // Scaffold a host project and verify the file set + content.
        assert_eq!(rc_scaffold(0, dirc.as_ptr()), 0);
        let files = unsafe { take(rc_list_files()) };
        for want in ["Cargo.toml", "build.rs", "src/main.rs"] {
            assert!(files.lines().any(|l| l == want), "missing {want} in:\n{files}");
        }
        let main_rs = unsafe { take(rc_read_file(cstr("src/main.rs").as_ptr())) };
        assert!(main_rs.contains("class Greeter"), "scaffold lost the class");

        // Edit + save + read-back round-trips through the bridge.
        let edited = main_rs.replace("excitement: i32", "excitement: i32, // edited");
        assert_eq!(
            rc_save_file(cstr("src/main.rs").as_ptr(), cstr(&edited).as_ptr()),
            0
        );
        let reread = unsafe { take(rc_read_file(cstr("src/main.rs").as_ptr())) };
        assert!(reread.contains("// edited"), "save did not persist");

        // Re-open counts the files (dotfiles like .vscode are skipped,
        // so: Cargo.toml, build.rs, src/main.rs, README.md = 4).
        assert!(rc_open(dirc.as_ptr()) >= 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn streamed_console_drains() {
        let dir = std::env::temp_dir();
        let _ = unsafe { take(rc_console_drain()) }; // clear
        engine::spawn_streamed(dir.to_str().unwrap(), "echo swiftui-ide-stream-probe");
        // Poll the drain the way the SwiftUI timer does.
        let mut acc = String::new();
        for _ in 0..200 {
            acc.push_str(&unsafe { take(rc_console_drain()) });
            if acc.contains("swiftui-ide-stream-probe") && acc.contains("[exit 0]") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(acc.contains("swiftui-ide-stream-probe"), "no streamed output: {acc:?}");
        assert!(acc.contains("[exit 0]"), "no exit marker: {acc:?}");
        assert_eq!(rc_is_running(), 0);
    }

    #[test]
    fn target_cmdline_host_uses_fork_rustc() {
        let c = engine::target_cmdline(0, false);
        assert!(c.contains("cargo +nightly build"));
        assert!(c.contains("RUSTC"));
        assert!(engine::target_cmdline(1, false).contains("SKIP_QEMU=1 ./run_arm.sh"));
        assert!(engine::target_cmdline(2, true).contains("./run_riscv_c2.sh"));
    }
}
