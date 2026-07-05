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
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};
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
const TARGET_NAMES: [&str; 8] = [
    "Host (LLVM backend)",
    "RAK11161 — STM32WLE5 core (Cortex-M4, FreeRTOS, qemu mps2)",
    "RAK11161 — ESP8684 / ESP32-C2 (rv32imc, FreeRTOS, qemu virt)",
    "ESP32-C3-class (rv32imac, FreeRTOS, qemu virt)",
    "STM32F4-class (Cortex-M4F, FreeRTOS, qemu mps2)",
    "Raspberry Pi Pico (RP2040, Cortex-M0+, FreeRTOS, qemu mps2)",
    "Zephyr — STM32WLE5 / Cortex-M3 (qemu_cortex_m3)",
    "Zephyr — ESP8684 / ESP32-C2 (rv32imc, qemu_riscv32)",
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
/// Two serial channels — dual-target boards like the RAK11161 have a
/// console per core (STM32WLE5 + ESP32-C2). Channel 0 is the primary
/// (feeds Upload + persists to upload.toml `[serial] port`); channel 1
/// is a second monitor (`port_b`).
const NSERIAL: usize = 2;
static SERIAL_PORT: [Mutex<String>; NSERIAL] =
    [Mutex::new(String::new()), Mutex::new(String::new())];
/// Target + baud loaded from the open project's .rustcc_ide.json, for
/// the SwiftUI side to read back (target auto-select, baud restore).
/// -1 / 0 = "not set in config".
static CFG_TARGET: AtomicI64 = AtomicI64::new(-1);
static CFG_BAUD: AtomicI64 = AtomicI64::new(0);
/// Open monitor handle (a cloned fd is read by the reader thread; this
/// one is for sending).
static SERIAL_TX: [Mutex<Option<std::fs::File>>; NSERIAL] = [Mutex::new(None), Mutex::new(None)];
static SERIAL_OPEN: [AtomicBool; NSERIAL] = [AtomicBool::new(false), AtomicBool::new(false)];
/// Per-channel connection generation. The reader thread loops only
/// while the generation it was spawned under is still current — bumped
/// on every open AND close, so a stale reader can't be resurrected by
/// a quick close→open flipping SERIAL_OPEN back to true, and a
/// second open can't leave two readers on one channel. Open/close
/// serialize on SERIAL_TX's lock (rc_run_on_device's worker calls
/// rc_serial_open concurrently with the UI).
static SERIAL_GEN: [AtomicU64; NSERIAL] = [AtomicU64::new(0), AtomicU64::new(0)];
/// Raw received bytes per channel (NOT a String: a 512-byte read can
/// split a multi-byte UTF-8 sequence; take_utf8 keeps the tail).
static SERIAL_RX: [Mutex<Vec<u8>>; NSERIAL] = [Mutex::new(Vec::new()), Mutex::new(Vec::new())];

// ---------------------------------------------------------------------
// Pure-logic core (no ABI concerns — unit-testable as plain Rust)
// ---------------------------------------------------------------------

mod engine {
    use super::*;
    use std::path::Path;

    pub fn console_append(s: &str) {
        CONSOLE.lock().unwrap_or_else(|e| e.into_inner()).push_str(s);
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
            6 => format!("{skip}./run_zephyr.sh"),     // Zephyr CM3
            7 => format!("{skip}./run_zephyr_c2.sh"),  // Zephyr ESP32-C2
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
    pub fn persist_serial_port(ch: usize, port: &str) {
        let Some(dir) = PROJECT_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
        let path = format!("{dir}/upload.toml");
        let Ok(cfg) = std::fs::read_to_string(&path) else { return };
        let key = if ch == 0 { "port" } else { "port_b" };
        let mut in_serial = false;
        let mut wrote = false;
        let mut out = String::new();
        for line in cfg.lines() {
            let l = line.trim();
            if l.starts_with('[') {
                // leaving [serial] without the key written → append it
                if in_serial && !wrote {
                    out.push_str(&format!("{key} = \"{port}\"\n"));
                    wrote = true;
                }
                in_serial = l == "[serial]";
            }
            if in_serial && !wrote && l.split('=').next().map(str::trim) == Some(key) {
                out.push_str(&format!("{key} = \"{port}\"\n"));
                wrote = true;
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        if in_serial && !wrote {
            out.push_str(&format!("{key} = \"{port}\"\n")); // [serial] was last
        }
        let _ = std::fs::write(&path, out);
    }

    /// Load the project's saved serial port (upload.toml `[serial]`)
    /// into the live selection, so the picker reflects it on open.
    pub fn load_serial_from_project() {
        let Some(dir) = PROJECT_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
        if let Ok(cfg) = std::fs::read_to_string(format!("{dir}/upload.toml")) {
            for (ch, key) in [(0usize, "port"), (1usize, "port_b")] {
                if let Some(p) = upload_cfg_get(&cfg, "serial", key) {
                    if !p.is_empty() {
                        *SERIAL_PORT[ch].lock().unwrap_or_else(|e| e.into_inner()) = p;
                    }
                }
            }
        }
    }

    /// Map a target to its upload `(upload.toml section, ELF path
    /// relative to the project)`; None for host. The FreeRTOS targets
    /// link to `target/<tag>/firmware.elf`; the Zephyr targets (6/7)
    /// produce `build/<board>/zephyr/zephyr.elf` instead — both are
    /// real hardware, flashed with the same per-section tool.
    pub fn upload_route(target: i64) -> Option<(&'static str, &'static str)> {
        match target {
            1 | 4 => Some(("stm32", "target/arm/firmware.elf")),
            2 => Some(("esp32", "target/riscv-c2/firmware.elf")),
            3 => Some(("esp32", "target/riscv/firmware.elf")),
            5 => Some(("pico", "target/pico/firmware.elf")),
            6 => Some(("stm32", "build/qemu_cortex_m3/zephyr/zephyr.elf")),
            7 => Some(("esp32", "build/qemu_riscv32/zephyr/zephyr.elf")),
            _ => None,
        }
    }

    // --- per-project JSON config (.rustcc_ide.json) ------------------
    // Remembers the selected target + serial port/baud so a project
    // restores them on open (target auto-selected; serial preserved
    // across IDE restarts). Hand-rolled flat JSON — no serde dep.

    pub fn config_path(dir: &str) -> std::path::PathBuf {
        Path::new(dir).join(".rustcc_ide.json")
    }

    fn json_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
        let i = text.find(&format!("\"{key}\""))?;
        let rest = &text[i..];
        let colon = rest.find(':')?;
        Some(rest[colon + 1..].trim_start())
    }
    fn json_int(text: &str, key: &str) -> Option<i64> {
        let v = json_field(text, key)?;
        let n: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
        n.parse().ok()
    }
    fn json_string(text: &str, key: &str) -> Option<String> {
        let v = json_field(text, key)?.strip_prefix('"')?;
        // Unescape what config_save escapes (`\"`, `\\`) — a port/path
        // containing a quote or backslash must round-trip, not truncate
        // at the escape.
        let mut out = String::new();
        let mut it = v.chars();
        while let Some(c) = it.next() {
            match c {
                '\\' => out.push(it.next()?),
                '"' => return Some(out),
                _ => out.push(c),
            }
        }
        None
    }

    /// Decode as much of `buf` as is valid UTF-8, LEAVING a trailing
    /// incomplete multi-byte sequence in place for the next chunk to
    /// finish (a 512-byte serial read can split a character). Truly
    /// invalid bytes are replaced (lossy) rather than kept forever.
    pub fn take_utf8(buf: &mut Vec<u8>) -> String {
        match std::str::from_utf8(buf) {
            Ok(s) => {
                let s = s.to_string();
                buf.clear();
                s
            }
            Err(e) if e.error_len().is_none() => {
                let valid = e.valid_up_to();
                let s = std::str::from_utf8(&buf[..valid]).unwrap().to_string();
                buf.drain(..valid);
                s
            }
            Err(_) => {
                let s = String::from_utf8_lossy(buf).into_owned();
                buf.clear();
                s
            }
        }
    }

    /// (target, serial_port, baud) from the project's .rustcc_ide.json;
    /// each field is None/empty if absent.
    pub fn config_load(dir: &str) -> (Option<i64>, String, Option<i64>) {
        let text = std::fs::read_to_string(config_path(dir)).unwrap_or_default();
        (
            json_int(&text, "target"),
            json_string(&text, "serial_port").unwrap_or_default(),
            json_int(&text, "baud"),
        )
    }

    pub fn config_save(dir: &str, target: i64, port: &str, baud: i64) {
        let esc = port.replace('\\', "\\\\").replace('"', "\\\"");
        let json = format!(
            "{{\n  \"target\": {target},\n  \"serial_port\": \"{esc}\",\n  \"baud\": {baud}\n}}\n"
        );
        let _ = std::fs::write(config_path(dir), json);
    }

    /// Guess the target a project supports from its files — so a project
    /// with no saved config (e.g. one scaffolded before this existed)
    /// still auto-selects a sensible target on open. Zephyr → CM3 (6),
    /// FreeRTOS → CM4 (1), host crate → Host (0).
    pub fn infer_target(dir: &str) -> Option<i64> {
        let has = |f: &str| Path::new(dir).join(f).exists();
        if has("run_zephyr.sh") || has("prj.conf") {
            Some(6)
        } else if has("run_arm.sh") {
            Some(1)
        } else if has("build.rs") {
            Some(0)
        } else {
            None
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

    /// Scaffold a **Zephyr RTOS** C++ interop project for the RAK11161
    /// — both cores. CMake/`west` app (qemu_cortex_m3 + qemu_riscv32
    /// rv32imc) over the same Rust `class` crate + C++ side as the
    /// FreeRTOS scaffold, embedded at compile time from examples/
    /// zephyr_cpp + examples/bare_metal_arm. Self-contained: the
    /// `../bare_metal_arm` references are rewritten to a local `cpp/`.
    pub fn scaffold_zephyr(dir: &str) -> Result<(), String> {
        let root = Path::new(dir);
        let werr = |e: std::io::Error| e.to_string();
        for sub in ["src", "cpp", "boards"] {
            std::fs::create_dir_all(root.join(sub)).map_err(werr)?;
        }
        // CMake: the imported C++ side moves from ../bare_metal_arm to
        // a local cpp/. Run scripts: build the Rust crate in-place and
        // link librak_zephyr_fw.a.
        let cmake = embed!("zephyr_cpp/CMakeLists.txt").replace("/../bare_metal_arm", "/cpp");
        let fix_run = |s: &str| -> String {
            s.replace("../bare_metal_arm", ".")
                .replace("libbare_metal_arm.a", "librak_zephyr_fw.a")
        };
        let files: &[(&str, String)] = &[
            ("Cargo.toml", ZEPHYR_CARGO_TOML.to_string()),
            ("src/lib.rs", embed!("bare_metal_arm/src/lib.rs").to_string()),
            ("src/main.c", embed!("zephyr_cpp/src/main.c").to_string()),
            ("cpp/caller.cpp", embed!("bare_metal_arm/caller.cpp").to_string()),
            ("cpp/sensor.cpp", embed!("bare_metal_arm/sensor.cpp").to_string()),
            ("cpp/sensor.hpp", embed!("bare_metal_arm/sensor.hpp").to_string()),
            ("cpp/rtti_stub.c", embed!("bare_metal_arm/rtti_stub.c").to_string()),
            ("CMakeLists.txt", cmake),
            ("prj.conf", embed!("zephyr_cpp/prj.conf").to_string()),
            (
                "boards/qemu_riscv32.overlay",
                embed!("zephyr_cpp/boards/qemu_riscv32.overlay").to_string(),
            ),
            ("run_zephyr.sh", fix_run(embed!("zephyr_cpp/run_zephyr.sh"))),
            ("run_zephyr_c2.sh", embed!("zephyr_cpp/run_zephyr_c2.sh").to_string()),
            ("upload.toml", UPLOAD_TOML.to_string()), // so Device run can flash
            ("README.md", ZEPHYR_SCAFFOLD_README.to_string()),
        ];
        for (rel, content) in files {
            std::fs::write(root.join(rel), content).map_err(werr)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for s in ["run_zephyr.sh", "run_zephyr_c2.sh"] {
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
            "conf", "overlay", "cmake", // Zephyr: prj.conf, .overlay
        ];
        // Extensionless project files worth showing in the tree.
        const NAMES: &[&str] = &["CMakeLists.txt"];
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
                } else {
                    let ext_ok = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| EXTS.contains(&e));
                    if ext_ok || NAMES.contains(&name.as_str()) {
                        if let Ok(rel) = p.strip_prefix(base) {
                            out.push(rel.to_string_lossy().into_owned());
                        }
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
    /// Prepended to every build/run shell so a GUI-launched app (opened
    /// from Finder/`open`, inheriting a minimal /usr/bin:/bin PATH) still
    /// finds the dev toolchain: rustup's cargo (~/.cargo/bin) and
    /// Homebrew's cmake/ninja/qemu/dtc. Without it the build dies with
    /// "cargo: command not found". Missing dirs are harmless; the run
    /// scripts resolve west/RUSTC by absolute path themselves.
    const DEV_PATH_PREFIX: &str =
        "export PATH=\"$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH\";";

    /// (no `Fl::check()` pump — SwiftUI polls `rc_console_drain`).
    /// Run `cmdline` in `dir`, streaming merged output into the console
    /// drain. **Blocking** — returns the exit code. Does NOT touch
    /// RUNNING (the caller owns that flag), so it can be chained for a
    /// build → flash → … sequence inside one background thread.
    pub fn run_streamed_blocking(dir: &str, cmdline: &str) -> i32 {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let child = Command::new("bash")
            .arg("-c")
            .arg(format!("{DEV_PATH_PREFIX} cd '{dir}' && {cmdline} 2>&1"))
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
                code
            }
            Err(e) => {
                console_append(&format!("spawn failed: {e}\n"));
                -1
            }
        }
    }

    /// Clears RUNNING when dropped — INCLUDING on a worker-thread
    /// panic/unwind. Without this, a panic on the worker (e.g. a
    /// poisoned-lock unwrap) left RUNNING stuck true forever, so every
    /// later Build/Run/Upload returned "already running" until the app
    /// was restarted.
    pub struct RunGuard;
    impl Drop for RunGuard {
        fn drop(&mut self) {
            RUNNING.store(false, Relaxed);
        }
    }

    pub fn spawn_streamed(dir: &str, cmdline: &str) {
        let dir = dir.to_string();
        let cmdline = cmdline.to_string();
        RUNNING.store(true, Relaxed);
        std::thread::spawn(move || {
            let _guard = RunGuard; // clears RUNNING on exit OR panic
            run_streamed_blocking(&dir, &cmdline);
        });
    }

    // --- recent workspaces (shared file with the FLTK IDE) -----------
    // ~/.rustcc_ide_recents, one project dir per line, most-recent
    // first. Open in either IDE → shows up in both. Surfaced as the
    // SwiftUI "Open Recent" menu / the FLTK "File ▸ Open Recent".

    pub fn recents_path() -> Option<std::path::PathBuf> {
        std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".rustcc_ide_recents"))
    }

    pub fn load_recents() -> Vec<String> {
        let Some(p) = recents_path() else {
            return Vec::new();
        };
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Prepend `dir` (most-recent first), dedup, cap at 10, persist.
    pub fn push_recent(dir: &str) {
        let mut list = load_recents();
        list.retain(|d| d != dir);
        list.insert(0, dir.to_string());
        list.truncate(10);
        if let Some(p) = recents_path() {
            let _ = std::fs::write(p, list.join("\n"));
        }
    }

    pub fn clear_recents() {
        if let Some(p) = recents_path() {
            let _ = std::fs::remove_file(p);
        }
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
            .arg(format!("{DEV_PATH_PREFIX} cd '{dir}' && {cmdline} 2>&1"))
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
        let mut g = DBG_STDIN.lock().unwrap_or_else(|e| e.into_inner());
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
        if let Some(stdin) = DBG_STDIN.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
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
            // Graceful, not expect(): a panic here would unwind the
            // worker (and poison any held lock) over a recoverable
            // pipe-setup failure.
            let (Some(stdin), Some(stdout), Some(stderr)) =
                (child.stdin.take(), child.stdout.take(), child.stderr.take())
            else {
                console_append("lldb pipes unavailable — debug session not started\n");
                let _ = child.kill();
                return;
            };
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
                                let mut cap = DBG_VARCAP.lock().unwrap_or_else(|e| e.into_inner());
                                if cap.active {
                                    let t = line.trim();
                                    // `ends_with`, NOT `contains`: the
                                    // command echo `(lldb) script
                                    // print("<<…>>")` contains the
                                    // sentinel but ends in `")`; the
                                    // real sentinel line ends in it.
                                    if t.ends_with(VARS_SENTINEL) {
                                        cap.active = false;
                                        *DBG_VARS.lock().unwrap_or_else(|e| e.into_inner()) = std::mem::take(&mut cap.acc);
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
                                        *DBG_CURLINE.lock().unwrap_or_else(|e| e.into_inner()) = Some(loc);
                                    }
                                }
                                if line.contains("Process") && line.contains("exited") {
                                    *DBG_CURLINE.lock().unwrap_or_else(|e| e.into_inner()) = None;
                                }
                            }
                        }
                    }
                }
                console_append("[debugger exited]\n");
                DBG_ACTIVE.store(false, Relaxed);
                *DBG_CURLINE.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
            *DBG_STDIN.lock().unwrap_or_else(|e| e.into_inner()) = Some(stdin);
            *DBG_CHILD.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
            DBG_ACTIVE.store(true, Relaxed);
            console_append(&format!("==> lldb session on {bin}\n"));
            // Replay breakpoints (basename + line), then run.
            let bps = BREAKPOINTS.lock().unwrap_or_else(|e| e.into_inner()).clone();
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
# Native Rust UART-bootloader flasher (stm32-uart-boot, MPL-2.0). The
# chip must be in BOOTLOADER mode first (BOOT0 high + reset).
cmd = "stm32-uart-boot {port} load {elf}"
# Fallback — STM32CubeProgrammer CLI over an SWD probe (ST-LINK):
# cmd = "STM32_Programmer_CLI -c port=SWD -w {elf} -v -rst"

[esp32]
# Native Rust serial flasher (espflash, Apache/MIT). Takes the ELF
# directly. For the ESP32-C2 core add `--chip esp32c2` (+ `--no-stub`
# if it balks).
cmd = "espflash flash --port {port} --baud 460800 {elf}"
# Fallback — esptool.py (Python):
# cmd = "esptool.py --chip auto elf2image {elf} -o {dir}/fw.bin && esptool.py --chip auto --port {port} write_flash 0x0 {dir}/fw.bin"

[pico]
cmd = "picotool load {elf} -fx"

[serial]
port = "/dev/cu.usbmodem01"
port_b = "/dev/cu.usbserial01"
"#;

const ZEPHYR_CARGO_TOML: &str = r#"# Rust `class` staticlib for the Zephyr RAK11161 project — built per
# core by run_zephyr*.sh and linked into the Zephyr app via CMake.
[package]
name = "rak_zephyr_fw"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["staticlib"]

[profile.dev]
panic = "abort"

[profile.release]
panic = "abort"

[workspace]
"#;

const ZEPHYR_SCAFFOLD_README: &str = r#"# RAK11161 dual-core firmware on Zephyr RTOS (rustcc)

Scaffolded by the **rustcc IDE**. The same Rust `class` crate
(`src/lib.rs`: Widget/Gauge + imported Sensor/Reader) + C++ side
(`cpp/`) as the FreeRTOS scaffold, but built by Zephyr's CMake/`west`
and run on qemu for both RAK11161 cores:

```sh
RUSTC=<fork-stage1>/bin/rustc ./run_zephyr.sh      # STM32WLE5 (Cortex-M3, qemu_cortex_m3)
RUSTC=<fork-stage1>/bin/rustc ./run_zephyr_c2.sh   # ESP8684/ESP32-C2 (rv32imc, qemu_riscv32)
SKIP_QEMU=1 ./run_zephyr.sh                         # build only
```

Prereqs: a Zephyr west workspace + SDK (ARM + RISC-V toolchains). See
`examples/zephyr_cpp` in the rustcc repo for the one-time setup.
Expected: `ZEPHYR CXX PROBE (...): PASS (105/4000/503/42 ...)`.
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
        2 => (engine::scaffold_zephyr(&dir), "RAK11161 dual-core Zephyr"),
        _ => (Err(format!("unknown scaffold kind {kind}")), ""),
    };
    match r {
        Ok(()) => {
            *PROJECT_DIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.clone());
            engine::load_serial_from_project();
            load_project_config(&dir);
            engine::push_recent(&dir);
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
    *PROJECT_DIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.clone());
    engine::load_serial_from_project();
    load_project_config(&dir);
    engine::push_recent(&dir);
    engine::console_append(&format!("project = {dir} ({n} files)\n"));
    n
}

/// Load .rustcc_ide.json: apply the saved serial port to channel 0
/// (the flash/device-run channel), and stash target + baud for the
/// SwiftUI side to read via rc_config_target / rc_config_baud.
fn load_project_config(dir: &str) {
    let (target, port, baud) = engine::config_load(dir);
    if !port.is_empty() {
        *SERIAL_PORT[0].lock().unwrap_or_else(|e| e.into_inner()) = port;
    }
    // Saved target wins; otherwise infer from the project's files.
    let t = target.or_else(|| engine::infer_target(dir));
    CFG_TARGET.store(t.unwrap_or(-1), Relaxed);
    CFG_BAUD.store(baud.unwrap_or(0), Relaxed);
}

/// The target saved in the open project's config, or -1 if none.
/// SwiftUI calls this right after open to auto-select the target.
#[export_name = "rc_config_target"]
pub extern "Swift" fn rc_config_target() -> i64 {
    CFG_TARGET.load(Relaxed)
}

/// The serial baud saved in the open project's config, or 0 if none.
#[export_name = "rc_config_baud"]
pub extern "Swift" fn rc_config_baud() -> i64 {
    CFG_BAUD.load(Relaxed)
}

/// Persist target + serial port/baud to the open project's
/// .rustcc_ide.json (and apply the port to channel 0). No-op if no
/// project is open.
#[export_name = "rc_config_save"]
pub extern "Swift" fn rc_config_save(target: i64, port: *const c_char, baud: i64) {
    let port = unsafe { in_str(port) };
    if let Some(dir) = PROJECT_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        engine::config_save(&dir, target, &port, baud);
        if !port.is_empty() {
            *SERIAL_PORT[0].lock().unwrap_or_else(|e| e.into_inner()) = port;
        }
        CFG_TARGET.store(target, Relaxed);
        CFG_BAUD.store(baud, Relaxed);
    }
}

/// Newline-joined recent-workspace dirs, most-recent first (caller
/// frees). Empty string if none.
#[export_name = "rc_recents_list"]
pub extern "Swift" fn rc_recents_list() -> *mut c_char {
    out_cstring(engine::load_recents().join("\n"))
}

/// Forget all recent workspaces.
#[export_name = "rc_recents_clear"]
pub extern "Swift" fn rc_recents_clear() {
    engine::clear_recents();
    engine::console_append("recent workspaces cleared\n");
}

/// Newline-joined, project-relative file list of the open project
/// (caller frees). Empty string if no project is open.
#[export_name = "rc_list_files"]
pub extern "Swift" fn rc_list_files() -> *mut c_char {
    let dir = PROJECT_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone();
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
    let dir = PROJECT_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone();
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
    let Some(d) = PROJECT_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
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
    let Some(dir) = PROJECT_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
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
    let mut g = CONSOLE.lock().unwrap_or_else(|e| e.into_inner());
    let taken = std::mem::take(&mut *g);
    out_cstring(taken)
}

/// Flash the built firmware for `target` using the per-project
/// upload.toml (configurable shell template with {elf}/{dir}/{port}),
/// streaming the tool's output. Returns 0 if started, -1 on any
/// precondition failure (no project / host target / no config / ELF
/// not built). Mirrors the FLTK IDE's Upload (⌘U).
/// Build the flash command for `target` from the project's upload.toml,
/// with the live serial selection (channel 0) winning over the stored
/// `[serial] port`. Err = a human-readable reason. Shared by Upload and
/// the Device run path so they flash identically.
fn upload_cmd_for(dir: &str, target: i64) -> Result<String, String> {
    let (section, elf_rel) =
        engine::upload_route(target).ok_or("host target has nothing to flash — pick an RTOS target")?;
    // Seed a default upload.toml if the project lacks one (e.g. an older
    // Zephyr scaffold) so Device run has a flash command to fill in.
    let cfg_path = format!("{dir}/upload.toml");
    if !std::path::Path::new(&cfg_path).exists() {
        let _ = std::fs::write(&cfg_path, UPLOAD_TOML);
        engine::console_append(
            "created a default upload.toml — set its flash command for your board/probe\n",
        );
    }
    let cfg = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    if cfg.is_empty() {
        return Err("no upload.toml in project".into());
    }
    let tpl = engine::upload_cfg_get(&cfg, section, "cmd")
        .ok_or_else(|| format!("no [{section}] cmd in upload.toml"))?;
    let selected = SERIAL_PORT[0].lock().unwrap_or_else(|e| e.into_inner()).clone();
    let port = if selected.is_empty() {
        engine::upload_cfg_get(&cfg, "serial", "port").unwrap_or_default()
    } else {
        selected
    };
    let elf = format!("{dir}/{elf_rel}");
    if !std::path::Path::new(&elf).exists() {
        return Err(format!("{elf} not built yet — Build first (link-only is enough)"));
    }
    Ok(tpl.replace("{elf}", &elf).replace("{dir}", dir).replace("{port}", &port))
}

#[export_name = "rc_upload"]
pub extern "Swift" fn rc_upload(target: i64) -> i64 {
    if RUNNING.load(Relaxed) {
        engine::console_append("a build/run is in flight — wait for it to finish\n");
        return -1;
    }
    let Some(dir) = PROJECT_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        engine::console_append("no project open\n");
        return -1;
    };
    match upload_cmd_for(&dir, target) {
        Ok(cmd) => {
            engine::console_append(&format!("==> upload\n    {cmd}\n"));
            engine::spawn_streamed(&dir, &cmd);
            0
        }
        Err(e) => {
            engine::console_append(&format!("{e}\n"));
            -1
        }
    }
}

/// Run on real hardware: build link-only → flash the selected serial
/// port → attach the channel-0 serial monitor, all on one background
/// thread (the console streams each step). `baud` is the monitor's
/// rate. Returns 0 if started, -1 if busy / no project / host target.
/// The QEMU/Device choice is the SwiftUI side's global toggle; this is
/// what Build & Run calls in Device mode.
#[export_name = "rc_run_on_device"]
pub extern "Swift" fn rc_run_on_device(target: i64, baud: i64) -> i64 {
    if RUNNING.load(Relaxed) {
        engine::console_append("a build/run is in flight — wait for it to finish\n");
        return -1;
    }
    let Some(dir) = PROJECT_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        engine::console_append("no project open\n");
        return -1;
    };
    if engine::upload_route(target).is_none() {
        engine::console_append("host target has nothing to flash — pick an RTOS target\n");
        return -1;
    }
    let name = TARGET_NAMES.get(target as usize).copied().unwrap_or("?");
    engine::console_append(&format!("==> build + run on DEVICE [{name}]\n"));
    RUNNING.store(true, Relaxed);
    std::thread::spawn(move || {
        // Clears RUNNING on every exit path — early return, the happy
        // end, or a panic. RUNNING therefore stays true until the
        // serial monitor is attached (closing the window where a
        // build could start while the port was being opened).
        let _guard = engine::RunGuard;
        // 1. build (link-only — qemu is skipped by SKIP_QEMU=1).
        let code = engine::run_streamed_blocking(&dir, &engine::target_cmdline(target, false));
        if code != 0 {
            engine::console_append("FAILED (build)\n");
            return;
        }
        // 2. flash the selected serial port.
        match upload_cmd_for(&dir, target) {
            Ok(cmd) => {
                engine::console_append(&format!("==> flash\n    {cmd}\n"));
                if engine::run_streamed_blocking(&dir, &cmd) != 0 {
                    engine::console_append("UPLOAD FAILED\n");
                    return;
                }
            }
            Err(e) => {
                engine::console_append(&format!("{e}\n"));
                return;
            }
        }
        // 3. watch it (channel 0) unless already open.
        if !SERIAL_OPEN[0].load(Relaxed) {
            rc_serial_open(0, baud);
        }
        engine::console_append("DEVICE RUN: flashed + serial monitor attached\n");
    });
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
    let Some(dir) = PROJECT_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
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
        let mut g = DBG_STDIN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(stdin) = g.as_mut() {
            use std::io::Write;
            let _ = writeln!(stdin, "process kill");
            let _ = writeln!(stdin, "quit");
            let _ = stdin.flush();
        }
        *g = None;
    }
    if let Some(mut c) = DBG_CHILD.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    DBG_ACTIVE.store(false, Relaxed);
    *DBG_CURLINE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    DBG_VARCAP.lock().unwrap_or_else(|e| e.into_inner()).active = false;
    DBG_VARS.lock().unwrap_or_else(|e| e.into_inner()).clear();
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
        let mut bps = BREAKPOINTS.lock().unwrap_or_else(|e| e.into_inner());
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
    let s = match &*DBG_CURLINE.lock().unwrap_or_else(|e| e.into_inner()) {
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
        let mut cap = DBG_VARCAP.lock().unwrap_or_else(|e| e.into_inner());
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
    out_cstring(DBG_VARS.lock().unwrap_or_else(|e| e.into_inner()).clone())
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

/// Clamp a Swift-supplied channel index to a valid serial channel.
fn serial_ch(ch: i64) -> usize {
    (ch as usize).min(NSERIAL - 1)
}

/// The selected serial port on channel `ch` (caller frees).
#[export_name = "rc_serial_port"]
pub extern "Swift" fn rc_serial_port(ch: i64) -> *mut c_char {
    out_cstring(SERIAL_PORT[serial_ch(ch)].lock().unwrap_or_else(|e| e.into_inner()).clone())
}

/// Select the serial port for channel `ch`; persists into upload.toml
/// (`port` for ch0 — also feeds Upload — `port_b` for ch1).
#[export_name = "rc_set_serial_port"]
pub extern "Swift" fn rc_set_serial_port(ch: i64, port: *const c_char) {
    let ch = serial_ch(ch);
    let port = unsafe { in_str(port) };
    *SERIAL_PORT[ch].lock().unwrap_or_else(|e| e.into_inner()) = port.clone();
    engine::persist_serial_port(ch, &port);
    engine::console_append(&format!(
        "serial[{ch}] port = {}\n",
        if port.is_empty() { "(none)" } else { &port }
    ));
}

/// Open channel `ch`'s selected port at `baud`, streaming RX into that
/// channel's drain. Returns 0 on success, -1 on error.
#[export_name = "rc_serial_open"]
pub extern "Swift" fn rc_serial_open(ch: i64, baud: i64) -> i64 {
    use std::io::Read;
    let ch = serial_ch(ch);
    // Hold the channel's TX lock across the whole check→configure→
    // install sequence: open must be atomic against a concurrent
    // open/close on the same channel (the device-run WORKER calls this
    // while the UI can too — an interleave used to double-open the
    // port and leave two reader threads, or strand SERIAL_OPEN=true
    // with no reader).
    let mut tx = SERIAL_TX[ch].lock().unwrap_or_else(|e| e.into_inner());
    if SERIAL_OPEN[ch].load(Relaxed) {
        return 0;
    }
    let port = SERIAL_PORT[ch].lock().unwrap_or_else(|e| e.into_inner()).clone();
    let file = match engine::serial_open(&port, baud) {
        Ok(f) => f,
        Err(e) => {
            engine::console_append(&format!("serial[{ch}] open FAILED: {e}\n"));
            return -1;
        }
    };
    let reader = match file.try_clone() {
        Ok(r) => r,
        Err(e) => {
            engine::console_append(&format!("serial[{ch}] clone FAILED: {e}\n"));
            return -1;
        }
    };
    *tx = Some(file);
    let my_gen = SERIAL_GEN[ch].fetch_add(1, Relaxed) + 1;
    SERIAL_OPEN[ch].store(true, Relaxed);
    drop(tx);
    engine::console_append(&format!("==> serial[{ch}] open: {port} @ {baud}\n"));
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 512];
        // Generation check, not just the OPEN flag: a close→reopen
        // within this thread's ~1s read timeout would flip OPEN back
        // to true and resurrect a stale reader (two readers on one
        // channel). A stale generation can't match.
        while SERIAL_GEN[ch].load(Relaxed) == my_gen && SERIAL_OPEN[ch].load(Relaxed) {
            match reader.read(&mut buf) {
                Ok(0) => {} // `min 0 time 10` read timeout — no data
                Ok(n) => SERIAL_RX[ch]
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend_from_slice(&buf[..n]),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
        // The reader owns a dup'd fd (try_clone); it closes on thread
        // exit — dropping SERIAL_TX does not close it.
    });
    0
}

/// Close serial channel `ch`.
#[export_name = "rc_serial_close"]
pub extern "Swift" fn rc_serial_close(ch: i64) {
    let ch = serial_ch(ch);
    // Same TX-lock serialization as open (see rc_serial_open).
    let mut tx = SERIAL_TX[ch].lock().unwrap_or_else(|e| e.into_inner());
    SERIAL_GEN[ch].fetch_add(1, Relaxed); // invalidate the reader's generation
    SERIAL_OPEN[ch].store(false, Relaxed);
    *tx = None;
    drop(tx);
    engine::console_append(&format!("serial[{ch}] closed\n"));
}

/// 1 while channel `ch` is open, else 0.
#[export_name = "rc_serial_is_open"]
pub extern "Swift" fn rc_serial_is_open(ch: i64) -> i64 {
    SERIAL_OPEN[serial_ch(ch)].load(Relaxed) as i64
}

/// Return and CLEAR channel `ch`'s pending received bytes (caller
/// frees). A trailing incomplete UTF-8 sequence stays buffered for the
/// next call (a 512-byte read can split a character).
#[export_name = "rc_serial_recv"]
pub extern "Swift" fn rc_serial_recv(ch: i64) -> *mut c_char {
    let mut g = SERIAL_RX[serial_ch(ch)].lock().unwrap_or_else(|e| e.into_inner());
    out_cstring(engine::take_utf8(&mut g))
}

/// Send `text` (a CR/LF is appended) to channel `ch`'s board.
#[export_name = "rc_serial_send"]
pub extern "Swift" fn rc_serial_send(ch: i64, text: *const c_char) {
    use std::io::Write;
    let text = unsafe { in_str(text) };
    if let Some(f) = SERIAL_TX[serial_ch(ch)].lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
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
        assert_eq!(rc_target_count(), 8);
        let host = unsafe { take(rc_target_name(0)) };
        assert!(host.starts_with("Host"), "got {host:?}");
        let c2 = unsafe { take(rc_target_name(2)) };
        assert!(c2.contains("ESP32-C2"), "got {c2:?}");
        // Zephyr targets (6 = CM3, 7 = ESP32-C2) → run_zephyr scripts.
        assert!(unsafe { take(rc_target_name(6)) }.contains("Zephyr"));
        assert!(engine::target_cmdline(6, false).contains("./run_zephyr.sh"));
        assert!(engine::target_cmdline(7, true).contains("./run_zephyr_c2.sh"));
    }

    #[test]
    fn scaffold_zephyr_file_set() {
        let dir = std::env::temp_dir().join(format!("swiftui_ide_zeph{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(rc_scaffold(2, cstr(dir.to_str().unwrap()).as_ptr()), 0);
        let files = unsafe { take(rc_list_files()) };
        for want in [
            "Cargo.toml", "CMakeLists.txt", "prj.conf", "src/lib.rs", "src/main.c",
            "cpp/caller.cpp", "cpp/sensor.cpp", "run_zephyr.sh", "run_zephyr_c2.sh",
        ] {
            assert!(files.lines().any(|l| l == want), "missing {want} in:\n{files}");
        }
        // CMake points at the local cpp/, not ../bare_metal_arm.
        let cmake = unsafe { take(rc_read_file(cstr("CMakeLists.txt").as_ptr())) };
        assert!(
            cmake.contains("/cpp") && !cmake.contains("../bare_metal_arm"),
            "CMake paths not localized"
        );
        // run script builds the local crate + links librak_zephyr_fw.a.
        let run = unsafe { take(rc_read_file(cstr("run_zephyr.sh").as_ptr())) };
        assert!(run.contains("librak_zephyr_fw.a") && !run.contains("../bare_metal_arm"));
        // The Rust side is the validated fork class crate.
        let lib = unsafe { take(rc_read_file(cstr("src/lib.rs").as_ptr())) };
        assert!(lib.contains("class") && lib.contains("Sensor"));
        let _ = std::fs::remove_dir_all(&dir);
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
        BREAKPOINTS.lock().unwrap_or_else(|e| e.into_inner()).clear();
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
        BREAKPOINTS.lock().unwrap_or_else(|e| e.into_inner()).clear();

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
        BREAKPOINTS.lock().unwrap_or_else(|e| e.into_inner()).clear();
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
            DBG_CURLINE.lock().unwrap_or_else(|e| e.into_inner()).is_some(),
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
        // target → (section, elf relpath); host has nothing to flash.
        assert_eq!(engine::upload_route(1), Some(("stm32", "target/arm/firmware.elf")));
        assert_eq!(engine::upload_route(2), Some(("esp32", "target/riscv-c2/firmware.elf")));
        assert_eq!(engine::upload_route(5), Some(("pico", "target/pico/firmware.elf")));
        // Zephyr targets ARE flashable (the device-run regression: they
        // used to return None, so Device mode fell back to a qemu run).
        assert_eq!(
            engine::upload_route(6),
            Some(("stm32", "build/qemu_cortex_m3/zephyr/zephyr.elf"))
        );
        assert_eq!(
            engine::upload_route(7),
            Some(("esp32", "build/qemu_riscv32/zephyr/zephyr.elf"))
        );
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
        // Two independent channels: select / read-back per channel.
        rc_set_serial_port(0, cstr("/dev/cu.unit-test-a").as_ptr());
        rc_set_serial_port(1, cstr("/dev/cu.unit-test-b").as_ptr());
        assert_eq!(unsafe { take(rc_serial_port(0)) }, "/dev/cu.unit-test-a");
        assert_eq!(unsafe { take(rc_serial_port(1)) }, "/dev/cu.unit-test-b");
        // A bogus port fails cleanly (stty errors) — no hang, returns -1.
        rc_set_serial_port(0, cstr("/dev/cu.nonexistent-xyz-123").as_ptr());
        assert_eq!(rc_serial_open(0, 115_200), -1);
        assert_eq!(rc_serial_is_open(0), 0);
        assert_eq!(rc_serial_is_open(1), 0);
        assert!(unsafe { take(rc_serial_recv(0)) }.is_empty());
        rc_set_serial_port(0, cstr("").as_ptr()); // reset shared state
        rc_set_serial_port(1, cstr("").as_ptr());
    }

    #[test]
    fn recents_roundtrip() {
        // Non-destructive: save the user's real list, exercise, restore.
        let saved = engine::load_recents();
        engine::clear_recents();
        engine::push_recent("/tmp/swiftui_recent_a");
        engine::push_recent("/tmp/swiftui_recent_b");
        engine::push_recent("/tmp/swiftui_recent_a"); // re-open → front, no dup
        let r = engine::load_recents();
        assert_eq!(r.first().map(String::as_str), Some("/tmp/swiftui_recent_a"));
        assert_eq!(r.iter().filter(|d| *d == "/tmp/swiftui_recent_a").count(), 1);
        assert_eq!(r.len(), 2);
        // rc_recents_list mirrors it (newline-joined, most-recent first).
        assert_eq!(
            unsafe { take(rc_recents_list()) }.lines().next(),
            Some("/tmp/swiftui_recent_a")
        );
        engine::clear_recents();
        assert!(engine::load_recents().is_empty());
        if let Some(p) = engine::recents_path() {
            let _ = std::fs::write(p, saved.join("\n"));
        }
    }

    #[test]
    fn device_run_gating_and_blocking_runner() {
        // The blocking runner streams and returns the real exit code.
        let _ = unsafe { take(rc_console_drain()) };
        assert_eq!(engine::run_streamed_blocking(".", "true"), 0);
        assert_ne!(engine::run_streamed_blocking(".", "exit 7"), 0);
        // Host target can never be device-run (nothing to flash) — -1
        // regardless of which project happens to be open.
        assert_eq!(rc_run_on_device(0, 115_200), -1);
        // upload_cmd_for: host errors; RTOS errors until the ELF exists,
        // then yields a command carrying the resolved port.
        let dir = std::env::temp_dir().join(format!("swiftui_ide_devcmd{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("upload.toml"),
            "[stm32]\ncmd = \"prog -w {elf} {port}\"\n[serial]\nport = \"/dev/x\"\n",
        )
        .unwrap();
        let ds = dir.to_str().unwrap();
        assert!(upload_cmd_for(ds, 0).is_err(), "host has no route");
        assert!(upload_cmd_for(ds, 1).is_err(), "ELF not built yet");
        std::fs::create_dir_all(dir.join("target/arm")).unwrap();
        std::fs::write(dir.join("target/arm/firmware.elf"), b"").unwrap();
        let cmd = upload_cmd_for(ds, 1).expect("cmd builds once ELF exists");
        assert!(cmd.contains("firmware.elf") && cmd.contains("/dev/"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_config_roundtrip() {
        let dir = std::env::temp_dir().join(format!("swiftui_ide_cfg{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ds = dir.to_str().unwrap();
        // No config yet → all absent.
        let (t, p, b) = engine::config_load(ds);
        assert!(t.is_none() && p.is_empty() && b.is_none());
        // Save → load round-trips target, port, baud.
        engine::config_save(ds, 6, "/dev/cu.usbserial-XYZ", 115_200);
        let (t, p, b) = engine::config_load(ds);
        assert_eq!(t, Some(6));
        assert_eq!(p, "/dev/cu.usbserial-XYZ");
        assert_eq!(b, Some(115_200));
        // It really is the JSON file we claim.
        let text = std::fs::read_to_string(engine::config_path(ds)).unwrap();
        assert!(text.contains("\"target\": 6") && text.contains("\"serial_port\""));
        // Escaping round-trip: a port containing " and \ must survive
        // save → load (the reader unescapes what save escapes;
        // regression: it used to truncate at the escaped quote).
        engine::config_save(ds, 6, r#"/dev/cu.we"ird\port"#, 9_600);
        let (_, p, _) = engine::config_load(ds);
        assert_eq!(p, r#"/dev/cu.we"ird\port"#);
        std::fs::remove_file(engine::config_path(ds)).ok();
        // Target inference from project files (the no-config fallback so
        // pre-existing projects still auto-select on open).
        assert_eq!(engine::infer_target(ds), None); // bare dir, no markers
        std::fs::write(dir.join("run_zephyr.sh"), "").unwrap();
        assert_eq!(engine::infer_target(ds), Some(6)); // Zephyr → CM3
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn serial_utf8_chunk_reassembly() {
        // A multi-byte char split across two reads must reassemble
        // instead of becoming replacement chars at the read boundary.
        let mut rx: Vec<u8> = Vec::new();
        rx.extend_from_slice("ok ".as_bytes());
        rx.extend_from_slice(&"é".as_bytes()[..1]); // half a 2-byte char
        assert_eq!(engine::take_utf8(&mut rx), "ok ");
        rx.extend_from_slice(&"é".as_bytes()[1..]);
        assert_eq!(engine::take_utf8(&mut rx), "é");
        assert!(rx.is_empty());
        // A genuinely invalid byte is replaced, not buffered forever.
        rx.extend_from_slice(b"a\xffb");
        assert_eq!(engine::take_utf8(&mut rx), "a\u{fffd}b");
    }

    /// Scaffold a Zephyr project and run its Cortex-M3 core on qemu via
    /// the scaffolded run_zephyr.sh — proves the path rewrites produce a
    /// working CMake/west build. Gated (needs the Zephyr SDK + west).
    #[test]
    fn full_zephyr_scaffold() {
        if std::env::var("RUSTCC_SWIFTUI_IDE_FULL").as_deref() != Ok("1") {
            return;
        }
        let dir = std::env::temp_dir().join(format!("swiftui_ide_zrun{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(rc_scaffold(2, cstr(dir.to_str().unwrap()).as_ptr()), 0);
        let out = std::process::Command::new("bash")
            .arg("run_zephyr.sh")
            .current_dir(&dir)
            .output()
            .expect("run_zephyr.sh");
        let log = String::from_utf8_lossy(&out.stdout);
        assert!(
            log.contains("ZEPHYR CXX PROBE") && log.contains("PASS (105/4000/503/42"),
            "scaffolded Zephyr CM3 did not reach PASS:\n{log}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
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
