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

/// Embed a sibling-example source at COMPILE TIME, so a scaffolded
/// RTOS project can never drift from the qemu-validated probes (same
/// trick the FLTK IDE uses; `swiftui_ide` sits next to them under
/// `examples/`, so the `/../` paths resolve identically).
macro_rules! embed {
    ($p:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../", $p))
    };
}

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

// --- debugger (host-only lldb session over a pty) --------------------
static DBG_ACTIVE: AtomicBool = AtomicBool::new(false);
static DBG_CHILD: Mutex<Option<std::process::Child>> = Mutex::new(None);
static DBG_STDIN: Mutex<Option<std::process::ChildStdin>> = Mutex::new(None);
/// Current stop as `(basename, line)` — what lldb reports in its
/// `… at file:line:col` frame line. The SwiftUI editor highlights it.
static DBG_CURLINE: Mutex<Option<(String, i32)>> = Mutex::new(None);
/// Breakpoints as `(project-relative path, line)`.
static BREAKPOINTS: Mutex<Vec<(String, i32)>> = Mutex::new(Vec::new());

/// `frame variable` capture: while `active`, the lldb reader thread
/// routes payload lines into `acc` (instead of the console) until the
/// sentinel arrives; the result is published to `DBG_VARS`.
struct VarCap {
    active: bool,
    acc: String,
}
static DBG_VARCAP: Mutex<VarCap> = Mutex::new(VarCap { active: false, acc: String::new() });
static DBG_VARS: Mutex<String> = Mutex::new(String::new());
const VARS_SENTINEL: &str = "<<RUSTCC_VARS_END>>";

// --- serial monitor (talk to the dev board over USB-serial) ----------
/// The selected serial device (e.g. `/dev/cu.usbserial-xxxx`). Feeds
/// the upload `{port}` and the monitor.
static SERIAL_PORT: Mutex<String> = Mutex::new(String::new());
/// Open monitor handle (a cloned fd is read by the reader thread; this
/// one is for sending).
static SERIAL_TX: Mutex<Option<std::fs::File>> = Mutex::new(None);
static SERIAL_OPEN: AtomicBool = AtomicBool::new(false);
/// Bytes received from the board, drained by the UI (like the console).
static SERIAL_RX: Mutex<String> = Mutex::new(String::new());

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

    /// Minimal TOML reader: value of `key` in `[section]` of an
    /// upload.toml (the scaffold writes one). Ports the FLTK IDE's
    /// `upload_cfg_get`.
    pub fn upload_cfg_get(cfg: &str, section: &str, key: &str) -> Option<String> {
        let mut in_section = false;
        for line in cfg.lines() {
            let l = line.trim();
            if l.starts_with('[') {
                in_section = l == format!("[{section}]");
                continue;
            }
            if in_section && !l.starts_with('#') {
                if let Some(rest) = l.strip_prefix(key) {
                    if let Some(rest) = rest.trim_start().strip_prefix('=') {
                        return Some(rest.trim().trim_matches('"').to_string());
                    }
                }
            }
        }
        None
    }

    /// Enumerate likely serial devices for talking to a board: macOS
    /// callout devices (`/dev/cu.*`) and Linux USB CDC/ACM/serial
    /// (`ttyUSB*`/`ttyACM*`). `cu.*` (not `tty.*`) is the right macOS
    /// node — it doesn't block on carrier-detect.
    pub fn serial_ports() -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir("/dev") {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().into_owned();
                let pick = name.starts_with("cu.")
                    || name.starts_with("ttyUSB")
                    || name.starts_with("ttyACM");
                if pick {
                    out.push(format!("/dev/{name}"));
                }
            }
        }
        out.sort();
        out
    }

    /// Configure `port` for raw N81 at `baud` with a ~1s read timeout
    /// (`stty`), then open it read+write. Using stty avoids a
    /// platform-specific termios FFI; `cu.*` opens without blocking.
    pub fn serial_open(port: &str, baud: i64) -> Result<std::fs::File, String> {
        if port.is_empty() {
            return Err("no serial port selected".into());
        }
        // macOS: `stty -f <port>`; Linux: `stty -F <port>`.
        let flag = if cfg!(target_os = "macos") { "-f" } else { "-F" };
        let stty = std::process::Command::new("stty")
            .arg(flag)
            .arg(port)
            .args([&baud.to_string(), "raw", "-echo", "min", "0", "time", "10"])
            .status();
        match stty {
            Ok(s) if s.success() => {}
            Ok(s) => return Err(format!("stty failed ({s}) — is {port} a serial device?")),
            Err(e) => return Err(format!("stty not runnable: {e}")),
        }
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(port)
            .map_err(|e| format!("open {port}: {e}"))
    }

    /// Rewrite the `[serial] port = "…"` line of the open project's
    /// upload.toml so the choice persists (best-effort; no-op if there
    /// is no project / no upload.toml).
    pub fn persist_serial_port(port: &str) {
        let Some(dir) = PROJECT_DIR.lock().unwrap().clone() else { return };
        let path = format!("{dir}/upload.toml");
        let Ok(cfg) = std::fs::read_to_string(&path) else { return };
        let mut in_serial = false;
        let mut wrote = false;
        let mut out = String::new();
        for line in cfg.lines() {
            let l = line.trim();
            if l.starts_with('[') {
                in_serial = l == "[serial]";
            }
            if in_serial && l.starts_with("port") && l.contains('=') && !wrote {
                out.push_str(&format!("port = \"{port}\"\n"));
                wrote = true;
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        let _ = std::fs::write(&path, out);
    }

    /// Load the project's saved serial port (upload.toml `[serial]`)
    /// into the live selection, so the picker reflects it on open.
    pub fn load_serial_from_project() {
        let Some(dir) = PROJECT_DIR.lock().unwrap().clone() else { return };
        if let Ok(cfg) = std::fs::read_to_string(format!("{dir}/upload.toml")) {
            if let Some(p) = upload_cfg_get(&cfg, "serial", "port") {
                if !p.is_empty() {
                    *SERIAL_PORT.lock().unwrap() = p;
                }
            }
        }
    }

    /// Map a target to its upload `(section, elf-tag)`; None for host.
    pub fn upload_route(target: i64) -> Option<(&'static str, &'static str)> {
        match target {
            1 | 4 => Some(("stm32", "arm")),
            2 => Some(("esp32", "riscv-c2")),
            3 => Some(("esp32", "riscv")),
            5 => Some(("pico", "pico")),
            _ => None,
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

    /// Scaffold a complete RAK11161 dual-core FreeRTOS firmware
    /// project — the Rust `class` crate + C++ side + FreeRTOS glue +
    /// per-core qemu run scripts — embedded at compile time from the
    /// validated `bare_metal_arm` / `freertos_cpp` examples (so it
    /// can't drift). Byte-identical to the FLTK IDE's `scaffold_project`.
    pub fn scaffold_rtos(dir: &str) -> Result<(), String> {
        let root = Path::new(dir);
        let werr = |e: std::io::Error| e.to_string();
        for sub in ["src", "cpp", "libc_stub", ".vscode"] {
            std::fs::create_dir_all(root.join(sub)).map_err(werr)?;
        }
        // examples-tree relative paths → project-local, and inject the
        // SKIP_QEMU link-only gate into the run scripts.
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
            std::fs::write(root.join(rel), content).map_err(werr)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for s in ["run_arm.sh", "run_riscv.sh", "run_riscv_c2.sh", "run_pico.sh"] {
                std::fs::set_permissions(root.join(s), std::fs::Permissions::from_mode(0o755))
                    .map_err(werr)?;
            }
        }
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

    /// Parse an lldb stop frame line: `… at <file>:<line>:<col>`.
    pub fn parse_stop_location(s: &str) -> Option<(String, i32)> {
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

    /// Run `cmdline` in `dir` BLOCKING, streaming merged output into
    /// the console drain. Returns the exit code. (Used for the debug-
    /// profile build that precedes the lldb spawn.)
    pub fn run_blocking(dir: &str, cmdline: &str) -> i32 {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let child = Command::new("bash")
            .arg("-c")
            .arg(format!("cd '{dir}' && {cmdline} 2>&1"))
            .stdout(Stdio::piped())
            .stdin(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                console_append(&format!("spawn failed: {e}\n"));
                return -1;
            }
        };
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
        child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1)
    }

    /// Send a raw command to the live lldb session (echoes it to the
    /// console transcript). No-op with a clear note if no session.
    pub fn dbg_send(cmd: &str) {
        use std::io::Write;
        let mut g = DBG_STDIN.lock().unwrap();
        match g.as_mut() {
            Some(stdin) => {
                if writeln!(stdin, "{cmd}").and_then(|_| stdin.flush()).is_err() {
                    console_append("debugger pipe closed\n");
                } else {
                    console_append(&format!("(lldb) {cmd}\n"));
                }
            }
            None => console_append("no debug session — Debug ▸ Start first\n"),
        }
    }

    /// Send a command WITHOUT echoing it to the console — used for the
    /// `frame variable` capture so the transcript stays clean.
    pub fn dbg_send_quiet(cmd: &str) {
        use std::io::Write;
        if let Some(stdin) = DBG_STDIN.lock().unwrap().as_mut() {
            let _ = writeln!(stdin, "{cmd}");
            let _ = stdin.flush();
        }
    }

    /// The crate name from the open project's Cargo.toml (host bin).
    pub fn project_bin_name(dir: &str) -> String {
        std::fs::read_to_string(format!("{dir}/Cargo.toml"))
            .unwrap_or_default()
            .lines()
            .find_map(|l| {
                let l = l.trim();
                l.strip_prefix("name = \"").and_then(|r| r.strip_suffix('\"')).map(str::to_string)
            })
            .unwrap_or_else(|| "rustcc_app".to_string())
    }

    /// Build the host project (debug profile) then attach lldb over a
    /// pty, replay breakpoints, and `run`. Runs entirely on a
    /// background thread so the UI never blocks; the reader thread
    /// streams the transcript into the console and tracks stop
    /// locations in `DBG_CURLINE`.
    pub fn dbg_start_session(dir: String) {
        std::thread::spawn(move || {
            console_append("==> building (debug profile, full debug info)\n");
            let code = run_blocking(
                &dir,
                "RUSTC=\"${RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}\" \
                 RUSTC_BOOTSTRAP=1 cargo +nightly build 2>&1",
            );
            if code != 0 {
                console_append("build failed — not starting the debugger\n");
                return;
            }
            let bin = format!("{dir}/target/debug/{}", project_bin_name(&dir));

            // lldb needs to believe it owns a terminal (async stop
            // events, command multiplexing vs the inferior) — bridge
            // it through a pty with `script -q /dev/null`.
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
            // stdout reader: transcript → console, frames → DBG_CURLINE.
            std::thread::spawn(move || {
                use std::io::BufRead;
                let mut r = std::io::BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match r.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            // During a `frame variable` capture, route
                            // payload lines into the vars buffer (drop
                            // command echoes + the sentinel); otherwise
                            // stream to the console and track stops.
                            let captured = {
                                let mut cap = DBG_VARCAP.lock().unwrap();
                                if cap.active {
                                    let t = line.trim();
                                    // `ends_with`, NOT `contains`: the
                                    // command echo `(lldb) script
                                    // print("<<…>>")` contains the
                                    // sentinel but ends in `")`; the
                                    // real sentinel line ends in it.
                                    if t.ends_with(VARS_SENTINEL) {
                                        cap.active = false;
                                        *DBG_VARS.lock().unwrap() = std::mem::take(&mut cap.acc);
                                    } else if t.starts_with("(lldb)") || t.contains("script print(") {
                                        // command echo — drop
                                    } else {
                                        cap.acc.push_str(&line);
                                    }
                                    true
                                } else {
                                    false
                                }
                            };
                            if !captured {
                                console_append(&line);
                                if line.contains(" at ") {
                                    if let Some(loc) = parse_stop_location(&line) {
                                        *DBG_CURLINE.lock().unwrap() = Some(loc);
                                    }
                                }
                                if line.contains("Process") && line.contains("exited") {
                                    *DBG_CURLINE.lock().unwrap() = None;
                                }
                            }
                        }
                    }
                }
                console_append("[debugger exited]\n");
                DBG_ACTIVE.store(false, Relaxed);
                *DBG_CURLINE.lock().unwrap() = None;
            });
            std::thread::spawn(move || {
                use std::io::BufRead;
                let mut r = std::io::BufReader::new(stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    match r.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => console_append(&line),
                    }
                }
            });
            *DBG_STDIN.lock().unwrap() = Some(stdin);
            *DBG_CHILD.lock().unwrap() = Some(child);
            DBG_ACTIVE.store(true, Relaxed);
            console_append(&format!("==> lldb session on {bin}\n"));
            // Replay breakpoints (basename + line), then run.
            let bps = BREAKPOINTS.lock().unwrap().clone();
            if bps.is_empty() {
                dbg_send("breakpoint set --name main");
            }
            for (f, l) in bps {
                let base = f.rsplit('/').next().unwrap_or(&f);
                dbg_send(&format!("breakpoint set --file {base} --line {l}"));
            }
            dbg_send("run");
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

const SCAFFOLD_CARGO_TOML: &str = r#"# RAK11161 dual-core firmware — scaffolded by the rustcc SwiftUI IDE.
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
  "version": "2.0.0",
  "tasks": [
    { "label": "rustcc: build (RAK11161 STM32WLE5 / CM4)", "type": "shell",
      "command": "SKIP_QEMU=1 ./run_arm.sh", "group": "build" },
    { "label": "rustcc: run on qemu (RAK11161 STM32WLE5 / CM4)", "type": "shell",
      "command": "./run_arm.sh", "group": "test" },
    { "label": "rustcc: build (RAK11161 ESP8684 / ESP32-C2)", "type": "shell",
      "command": "SKIP_QEMU=1 ./run_riscv_c2.sh", "group": "build" },
    { "label": "rustcc: run on qemu (RAK11161 ESP8684 / ESP32-C2)", "type": "shell",
      "command": "./run_riscv_c2.sh", "group": "test" }
  ]
}
"#;

const SCAFFOLD_README: &str = r#"# RAK11161 dual-core firmware (rustcc)

Scaffolded by the **rustcc SwiftUI IDE** for the RAKwireless RAK11161
WisDuo breakout: STM32WLE5 (Arm Cortex-M4, LoRa side) + ESP8684 =
ESP32-C2 (RISC-V rv32imc, WiFi/BLE side). One Rust `class` crate
(`src/lib.rs`) is built per-core and runs under FreeRTOS on qemu:

```sh
./run_arm.sh        # STM32WLE5 core  (ARM_CM4F port, qemu mps2-an386)
./run_riscv_c2.sh   # ESP8684 core    (RISC-V port, rv32imc, A ext OFF)
SKIP_QEMU=1 ./run_arm.sh   # build + link only
```

Expected: `FREERTOS CXX PROBE (…): PASS (105/4000/503/42 across tasks)`.
The qemu machines model the CORES, not RAK's radios.
"#;

const UPLOAD_TOML: &str = r#"# rustcc IDE — firmware upload configuration (per project).
# Each [section]'s `cmd` is a shell template run from the project root
# with {elf}/{dir}/{port} placeholders. The qemu-validated ELFs use the
# qemu memory maps — point the linker scripts at your board before
# flashing real hardware.

[stm32]
cmd = "STM32_Programmer_CLI -c port=SWD -w {elf} -v -rst"

[esp32]
cmd = "esptool.py --chip auto elf2image {elf} -o {dir}/fw.bin && esptool.py --chip auto --port {port} write_flash 0x0 {dir}/fw.bin"

[pico]
cmd = "picotool load {elf} -fx"

[serial]
port = "/dev/cu.usbmodem01"
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
    let (r, label) = match kind {
        0 => (engine::scaffold_host(&dir), "host"),
        1 => (engine::scaffold_rtos(&dir), "RAK11161 dual-core FreeRTOS"),
        _ => (Err(format!("unknown scaffold kind {kind}")), ""),
    };
    match r {
        Ok(()) => {
            *PROJECT_DIR.lock().unwrap() = Some(dir.clone());
            engine::load_serial_from_project();
            engine::console_append(&format!("scaffolded {label} project at {dir}\n"));
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
    engine::load_serial_from_project();
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

/// Flash the built firmware for `target` using the per-project
/// upload.toml (configurable shell template with {elf}/{dir}/{port}),
/// streaming the tool's output. Returns 0 if started, -1 on any
/// precondition failure (no project / host target / no config / ELF
/// not built). Mirrors the FLTK IDE's Upload (⌘U).
#[export_name = "rc_upload"]
pub extern "Swift" fn rc_upload(target: i64) -> i64 {
    if RUNNING.load(Relaxed) {
        engine::console_append("a build/run is in flight — wait for it to finish\n");
        return -1;
    }
    let Some(dir) = PROJECT_DIR.lock().unwrap().clone() else {
        engine::console_append("no project open\n");
        return -1;
    };
    let Some((section, tag)) = engine::upload_route(target) else {
        engine::console_append("host target has nothing to flash — pick an RTOS target\n");
        return -1;
    };
    let cfg = std::fs::read_to_string(format!("{dir}/upload.toml")).unwrap_or_default();
    if cfg.is_empty() {
        engine::console_append("no upload.toml in project (scaffold an RTOS project)\n");
        return -1;
    }
    let Some(tpl) = engine::upload_cfg_get(&cfg, section, "cmd") else {
        engine::console_append(&format!("no [{section}] cmd in upload.toml\n"));
        return -1;
    };
    // The live selection wins over upload.toml's stored value.
    let selected = SERIAL_PORT.lock().unwrap().clone();
    let port = if selected.is_empty() {
        engine::upload_cfg_get(&cfg, "serial", "port").unwrap_or_default()
    } else {
        selected
    };
    let elf = format!("{dir}/target/{tag}/firmware.elf");
    if !std::path::Path::new(&elf).exists() {
        engine::console_append(&format!("{elf} not built yet — Build first (link-only is enough)\n"));
        return -1;
    }
    let cmd = tpl.replace("{elf}", &elf).replace("{dir}", &dir).replace("{port}", &port);
    engine::console_append(&format!("==> upload via [{section}]\n    {cmd}\n"));
    engine::spawn_streamed(&dir, &cmd);
    0
}

// --- debugger API (host target only) ---------------------------------

/// Start an in-IDE lldb session on the open Host project. Builds the
/// debug profile, attaches lldb, replays breakpoints, runs — all on a
/// background thread. Returns 0 if starting, -1 if busy / no project /
/// non-host target.
#[export_name = "rc_dbg_start"]
pub extern "Swift" fn rc_dbg_start(target: i64) -> i64 {
    if DBG_ACTIVE.load(Relaxed) {
        engine::console_append("debug session already running — Stop first\n");
        return -1;
    }
    if target != 0 {
        engine::console_append(
            "in-IDE stepping is host-only; pick the Host target (RTOS uses qemu+gdb)\n",
        );
        return -1;
    }
    let Some(dir) = PROJECT_DIR.lock().unwrap().clone() else {
        engine::console_append("no project open\n");
        return -1;
    };
    if std::fs::read_to_string(format!("{dir}/Cargo.toml"))
        .unwrap_or_default()
        .contains("staticlib")
    {
        engine::console_append("firmware staticlib — no host binary to debug\n");
        return -1;
    }
    engine::dbg_start_session(dir);
    0
}

/// Send a raw command to the live lldb session (step/continue/etc.).
#[export_name = "rc_dbg_send"]
pub extern "Swift" fn rc_dbg_send(cmd: *const c_char) {
    let cmd = unsafe { in_str(cmd) };
    engine::dbg_send(&cmd);
}

/// Kill the lldb session and clear debug state.
#[export_name = "rc_dbg_stop"]
pub extern "Swift" fn rc_dbg_stop() {
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
        let _ = c.kill();
        let _ = c.wait();
    }
    DBG_ACTIVE.store(false, Relaxed);
    *DBG_CURLINE.lock().unwrap() = None;
    DBG_VARCAP.lock().unwrap().active = false;
    DBG_VARS.lock().unwrap().clear();
    engine::console_append("debug session stopped\n");
}

/// Toggle a breakpoint at project-relative `rel`:`line`. Returns 1 if
/// added, 0 if removed. Replays into a live session immediately.
#[export_name = "rc_dbg_toggle_breakpoint"]
pub extern "Swift" fn rc_dbg_toggle_breakpoint(rel: *const c_char, line: i64) -> i64 {
    let rel = unsafe { in_str(rel) };
    if rel.is_empty() || line <= 0 {
        return 0;
    }
    let line = line as i32;
    let added = {
        let mut bps = BREAKPOINTS.lock().unwrap();
        if let Some(i) = bps.iter().position(|(f, l)| *f == rel && *l == line) {
            bps.remove(i);
            false
        } else {
            bps.push((rel.clone(), line));
            true
        }
    };
    let base = rel.rsplit('/').next().unwrap_or(&rel).to_string();
    if DBG_ACTIVE.load(Relaxed) {
        let verb = if added { "set" } else { "clear" };
        engine::dbg_send(&format!("breakpoint {verb} --file {base} --line {line}"));
    }
    engine::console_append(&format!(
        "breakpoint {}: {base}:{line}\n",
        if added { "set" } else { "removed" }
    ));
    added as i64
}

/// 1 while an lldb session is live, else 0.
#[export_name = "rc_dbg_active"]
pub extern "Swift" fn rc_dbg_active() -> i64 {
    DBG_ACTIVE.load(Relaxed) as i64
}

/// Current stop as `basename:line` (e.g. `main.rs:30`), or empty if
/// not stopped. Caller frees.
#[export_name = "rc_dbg_curline"]
pub extern "Swift" fn rc_dbg_curline() -> *mut c_char {
    let s = match &*DBG_CURLINE.lock().unwrap() {
        Some((f, l)) => format!("{f}:{l}"),
        None => String::new(),
    };
    out_cstring(s)
}

/// Request a fresh `frame variable` capture from the live session
/// (sentinel-bracketed; the reader thread fills DBG_VARS). No-op if no
/// session or a capture is already in flight.
#[export_name = "rc_dbg_request_vars"]
pub extern "Swift" fn rc_dbg_request_vars() {
    if !DBG_ACTIVE.load(Relaxed) {
        return;
    }
    {
        let mut cap = DBG_VARCAP.lock().unwrap();
        if cap.active {
            return;
        }
        cap.active = true;
        cap.acc.clear();
    }
    engine::dbg_send_quiet("frame variable");
    engine::dbg_send_quiet(&format!("script print(\"{VARS_SENTINEL}\")"));
}

/// The latest captured `frame variable` output (caller frees).
#[export_name = "rc_dbg_vars"]
pub extern "Swift" fn rc_dbg_vars() -> *mut c_char {
    out_cstring(DBG_VARS.lock().unwrap().clone())
}

/// All breakpoints as newline-joined `rel:line` rows. Caller frees.
#[export_name = "rc_dbg_breakpoints"]
pub extern "Swift" fn rc_dbg_breakpoints() -> *mut c_char {
    let body = BREAKPOINTS
        .lock()
        .unwrap()
        .iter()
        .map(|(f, l)| format!("{f}:{l}"))
        .collect::<Vec<_>>()
        .join("\n");
    out_cstring(body)
}

// --- serial port selection + monitor ---------------------------------

/// Newline-joined list of available serial devices (caller frees).
#[export_name = "rc_serial_ports"]
pub extern "Swift" fn rc_serial_ports() -> *mut c_char {
    out_cstring(engine::serial_ports().join("\n"))
}

/// The currently selected serial port (caller frees).
#[export_name = "rc_serial_port"]
pub extern "Swift" fn rc_serial_port() -> *mut c_char {
    out_cstring(SERIAL_PORT.lock().unwrap().clone())
}

/// Select the serial port used by upload + the monitor; persists into
/// the open project's upload.toml.
#[export_name = "rc_set_serial_port"]
pub extern "Swift" fn rc_set_serial_port(port: *const c_char) {
    let port = unsafe { in_str(port) };
    *SERIAL_PORT.lock().unwrap() = port.clone();
    engine::persist_serial_port(&port);
    engine::console_append(&format!("serial port = {}\n", if port.is_empty() { "(none)" } else { &port }));
}

/// Open the selected port at `baud` and start streaming RX into the
/// serial drain. Returns 0 on success, -1 on error.
#[export_name = "rc_serial_open"]
pub extern "Swift" fn rc_serial_open(baud: i64) -> i64 {
    use std::io::Read;
    if SERIAL_OPEN.load(Relaxed) {
        return 0;
    }
    let port = SERIAL_PORT.lock().unwrap().clone();
    let file = match engine::serial_open(&port, baud) {
        Ok(f) => f,
        Err(e) => {
            engine::console_append(&format!("serial open FAILED: {e}\n"));
            return -1;
        }
    };
    let reader = match file.try_clone() {
        Ok(r) => r,
        Err(e) => {
            engine::console_append(&format!("serial clone FAILED: {e}\n"));
            return -1;
        }
    };
    *SERIAL_TX.lock().unwrap() = Some(file);
    SERIAL_OPEN.store(true, Relaxed);
    engine::console_append(&format!("==> serial open: {port} @ {baud} (raw, read-timeout)\n"));
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 512];
        loop {
            if !SERIAL_OPEN.load(Relaxed) {
                break;
            }
            match reader.read(&mut buf) {
                Ok(0) => {} // `min 0 time 10` read timeout — no data
                Ok(n) => SERIAL_RX
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
    });
    0
}

/// Close the serial monitor.
#[export_name = "rc_serial_close"]
pub extern "Swift" fn rc_serial_close() {
    SERIAL_OPEN.store(false, Relaxed);
    *SERIAL_TX.lock().unwrap() = None;
    engine::console_append("serial closed\n");
}

/// 1 while the monitor is open, else 0.
#[export_name = "rc_serial_is_open"]
pub extern "Swift" fn rc_serial_is_open() -> i64 {
    SERIAL_OPEN.load(Relaxed) as i64
}

/// Return and CLEAR pending bytes received from the board (caller frees).
#[export_name = "rc_serial_recv"]
pub extern "Swift" fn rc_serial_recv() -> *mut c_char {
    let mut g = SERIAL_RX.lock().unwrap();
    out_cstring(std::mem::take(&mut *g))
}

/// Send `text` (a CR/LF is appended) to the board over the open port.
#[export_name = "rc_serial_send"]
pub extern "Swift" fn rc_serial_send(text: *const c_char) {
    use std::io::Write;
    let text = unsafe { in_str(text) };
    if let Some(f) = SERIAL_TX.lock().unwrap().as_mut() {
        let _ = f.write_all(text.as_bytes());
        let _ = f.write_all(b"\r\n");
        let _ = f.flush();
    } else {
        engine::console_append("serial not open\n");
    }
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
    fn scaffold_rtos_file_set_and_skip_gate() {
        let dir = std::env::temp_dir().join(format!("swiftui_ide_rtos{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(rc_scaffold(1, cstr(dir.to_str().unwrap()).as_ptr()), 0);
        let files = unsafe { take(rc_list_files()) };
        // The dual-core firmware set the run scripts need.
        for want in [
            "src/lib.rs", "cpp/caller.cpp", "cpp/sensor.cpp", "FreeRTOSConfig.h",
            "main_arm.c", "link_arm.ld", "run_arm.sh", "run_riscv_c2.sh", "Cargo.toml",
        ] {
            assert!(files.lines().any(|l| l == want), "missing {want} in:\n{files}");
        }
        // The Rust side is the validated fork `class` crate.
        let lib = unsafe { take(rc_read_file(cstr("src/lib.rs").as_ptr())) };
        assert!(lib.contains("class") && lib.contains("Sensor"), "scaffold lost the class crate");
        // The SKIP_QEMU link-only gate was injected into run_arm.sh.
        let run = unsafe { take(rc_read_file(cstr("run_arm.sh").as_ptr())) };
        assert!(run.contains("SKIP_QEMU"), "run_arm.sh missing the SKIP_QEMU gate");
        // run scripts are executable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(dir.join("run_arm.sh")).unwrap().permissions().mode();
            assert!(m & 0o111 != 0, "run_arm.sh not executable");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn breakpoint_toggle_and_stop_parser() {
        BREAKPOINTS.lock().unwrap().clear();
        // add → remove round-trip, reported via the return code.
        assert_eq!(rc_dbg_toggle_breakpoint(cstr("src/main.rs").as_ptr(), 30), 1);
        assert_eq!(rc_dbg_toggle_breakpoint(cstr("src/main.rs").as_ptr(), 42), 1);
        let bps = unsafe { take(rc_dbg_breakpoints()) };
        assert!(bps.lines().any(|l| l == "src/main.rs:30"));
        assert!(bps.lines().any(|l| l == "src/main.rs:42"));
        assert_eq!(rc_dbg_toggle_breakpoint(cstr("src/main.rs").as_ptr(), 30), 0);
        let bps = unsafe { take(rc_dbg_breakpoints()) };
        assert!(!bps.lines().any(|l| l == "src/main.rs:30"));
        assert!(bps.lines().any(|l| l == "src/main.rs:42"));
        BREAKPOINTS.lock().unwrap().clear();

        // stop-frame parsing (basename:line) from real lldb output.
        let loc = engine::parse_stop_location(
            "    frame #0: 0x0001 rustcc_app`main at main.rs:30:5",
        );
        assert_eq!(loc, Some(("main.rs".to_string(), 30)));
        assert_eq!(engine::parse_stop_location("Process 1 resuming"), None);

        // start refuses non-host + no-session sends are safe.
        assert_eq!(rc_dbg_active(), 0);
        assert_eq!(rc_dbg_start(1), -1); // non-host target rejected
    }

    /// Full lldb session against a scaffolded host binary. Slow (builds
    /// the debug profile with the fork), so gated. Mirrors the FLTK
    /// IDE's FULL gate, pull-based on the console drain.
    #[test]
    fn full_lldb_session() {
        if std::env::var("RUSTCC_SWIFTUI_IDE_FULL").as_deref() != Ok("1") {
            return;
        }
        let dir = std::env::temp_dir().join(format!("swiftui_ide_dbg{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dirc = cstr(dir.to_str().unwrap());
        assert_eq!(rc_scaffold(0, dirc.as_ptr()), 0);

        // Breakpoint on the first println! line of the scaffold.
        let main_rs = unsafe { take(rc_read_file(cstr("src/main.rs").as_ptr())) };
        let line = main_rs
            .lines()
            .position(|l| l.contains("println!"))
            .map(|i| i as i64 + 1)
            .unwrap_or(1);
        BREAKPOINTS.lock().unwrap().clear();
        assert_eq!(rc_dbg_toggle_breakpoint(cstr("src/main.rs").as_ptr(), line), 1);

        let mut acc = String::new();
        let wait = |acc: &mut String, needle: &str, secs: u32| -> bool {
            for _ in 0..secs * 10 {
                acc.push_str(&unsafe { take(rc_console_drain()) });
                if acc.contains(needle) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            false
        };

        assert_eq!(rc_dbg_start(0), 0);
        assert!(wait(&mut acc, "stop reason = breakpoint", 120), "no bp hit:\n{acc}");
        assert!(
            DBG_CURLINE.lock().unwrap().is_some(),
            "curline not tracked after stop"
        );
        // main's println! line has no locals — step into the call so
        // there's a variable to capture (the FLTK IDE's lesson too).
        rc_dbg_send(cstr("thread step-in").as_ptr());
        assert!(wait(&mut acc, "stop reason = step", 20), "step-in did not stop:\n{acc}");
        // Variables pane: the sentinel capture fills DBG_VARS without
        // polluting the console transcript.
        rc_dbg_request_vars();
        let mut vars = String::new();
        for _ in 0..150 {
            vars = unsafe { take(rc_dbg_vars()) };
            if !vars.trim().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(!vars.trim().is_empty(), "frame-variable capture was empty");
        assert!(
            !acc.contains(VARS_SENTINEL),
            "capture sentinel leaked into the console"
        );
        rc_dbg_send(cstr("breakpoint disable").as_ptr());
        rc_dbg_send(cstr("continue").as_ptr());
        assert!(wait(&mut acc, "exited", 30), "did not run to exit:\n{acc}");
        rc_dbg_stop();
        assert_eq!(rc_dbg_active(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Scaffold an RTOS project and run its STM32WLE5 (CM4) core under
    /// qemu through the engine, asserting the FreeRTOS probe PASS line.
    /// Gated (needs arm-none-eabi-gcc + qemu + the FreeRTOS kernel).
    #[test]
    fn full_rtos_arm() {
        if std::env::var("RUSTCC_SWIFTUI_IDE_FULL").as_deref() != Ok("1") {
            return;
        }
        let dir = std::env::temp_dir().join(format!("swiftui_ide_rtosrun{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(rc_scaffold(1, cstr(dir.to_str().unwrap()).as_ptr()), 0);

        // Drive the CM4 core (target 1 → run_arm.sh) via the build API.
        let _ = unsafe { take(rc_console_drain()) };
        assert_eq!(rc_build(1, 1), 0); // run on qemu
        let mut acc = String::new();
        let mut ok = false;
        for _ in 0..1800 {
            acc.push_str(&unsafe { take(rc_console_drain()) });
            if acc.contains("FREERTOS CXX PROBE") && acc.contains("PASS (105/4000/503/42") {
                ok = true;
                break;
            }
            if acc.contains("[exit") && !acc.contains("PASS (105/4000/503/42") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(ok, "RTOS CM4 qemu run did not reach the PASS line:\n{acc}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upload_routing_and_config() {
        // target → (section, elf-tag); host has nothing to flash.
        assert_eq!(engine::upload_route(1), Some(("stm32", "arm")));
        assert_eq!(engine::upload_route(2), Some(("esp32", "riscv-c2")));
        assert_eq!(engine::upload_route(5), Some(("pico", "pico")));
        assert_eq!(engine::upload_route(0), None);
        // upload.toml parsing.
        let cfg = "[stm32]\ncmd = \"prog -w {elf}\"\n[serial]\nport = \"/dev/x\"\n";
        assert_eq!(engine::upload_cfg_get(cfg, "stm32", "cmd").as_deref(), Some("prog -w {elf}"));
        assert_eq!(engine::upload_cfg_get(cfg, "serial", "port").as_deref(), Some("/dev/x"));
        assert_eq!(engine::upload_cfg_get(cfg, "esp32", "cmd"), None);
        // rc_upload rejects host + missing-ELF up front.
        let dir = std::env::temp_dir().join(format!("swiftui_ide_up{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(rc_scaffold(1, cstr(dir.to_str().unwrap()).as_ptr()), 0);
        assert_eq!(rc_upload(0), -1); // host
        assert_eq!(rc_upload(1), -1); // RTOS but firmware.elf not built
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn serial_enumerate_select_open() {
        // Enumeration must not panic (it reads /dev).
        let _ = engine::serial_ports();
        let _ = unsafe { take(rc_serial_ports()) };
        // Select / read-back roundtrip.
        rc_set_serial_port(cstr("/dev/cu.unit-test-sel").as_ptr());
        assert_eq!(unsafe { take(rc_serial_port()) }, "/dev/cu.unit-test-sel");
        // A bogus port fails cleanly (stty errors) — no hang, returns -1.
        rc_set_serial_port(cstr("/dev/cu.nonexistent-xyz-123").as_ptr());
        assert_eq!(rc_serial_open(115_200), -1);
        assert_eq!(rc_serial_is_open(), 0);
        assert!(unsafe { take(rc_serial_recv()) }.is_empty());
        rc_set_serial_port(cstr("").as_ptr()); // reset shared state
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
