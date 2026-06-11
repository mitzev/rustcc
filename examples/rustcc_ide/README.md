# rustcc IDE — an embedded-RTOS IDE built *with* the fork

The capstone sample: an IDE for RAK11161-class dual-core firmware,
written in fork Rust, that composes the project's own validated
pieces —

| Piece | Reused from |
|---|---|
| Editor core (menus, dialogs, undo/find/wrap, syntax highlighting, `class RustEditor : Fl_Text_Editor`) | `examples/fltk_text_editor` |
| Firmware sources + FreeRTOS glue the scaffold emits | `examples/bare_metal_arm` + `examples/freertos_cpp` (embedded via `include_str!` — the scaffold can never drift from the validated probes) |
| `.vscode/tasks.json` conventions in scaffolded projects | `tools/vscode-rustcc` plugin |

## The target: RAKwireless RAK11161 (WisDuo breakout)

Two cores, both already qemu-validated by this repo's probes:

- **STM32WLE5** — Arm Cortex-M4 (LoRa side) → FreeRTOS `ARM_CM4F`
  port on qemu `mps2-an386`
- **ESP8684 = ESP32-C2** — RISC-V rv32**imc**, no atomic extension
  (WiFi/BLE side) → FreeRTOS RISC-V port on qemu `virt` with the A
  extension disabled

The Target menu also offers Host (LLVM) and an ESP32-C3-class
(rv32imac) flavor. qemu models the cores, not RAK's radios — LoRa/
WiFi peripheral work needs hardware.

## Run it

```sh
cargo run --release --bin gen_bindings        # stock toolchain + libclang
cargo +rustcc run --release --bin ide         # the GUI
```

## v2 additions

- **File navigator** (left sidebar, a Rust `class FileNav :
  Fl_Hold_Browser` over the 3-level imported chain) — click a file to
  open it; the active file is bolded.
- **Multiple open files** — one `Fl_Text_Buffer` per file, the editor
  view switches instantly; re-opening switches instead of reloading.
- **Find is a popup** (⌘F shows + focuses, Enter finds wrap-around,
  Escape hides).
- **File ▸ New Project ▸ {Host, RAK11161}** — the Host scaffold is
  the vscode-rustcc plugin's class-surface template (Counter class);
  Open Project also moved to File.
- **Grammar-driven highlighting** — the tokenizer's keyword set is
  built at startup from the **VSCode extension's TextMate grammar**
  (embedded `include_str!`, single source of truth) merged with the
  core Rust keywords: comments, strings, `#[...]` attributes, and
  keywords each get their own style.
- **Project ▸ Debug (qemu + gdbserver)** (⌘⇧D) — launches the
  firmware HALTED under `qemu -s -S` in a separate Terminal window
  and prints the exact `arm-none-eabi-gdb` / `riscv64-elf-gdb` attach
  command in the console (host target: lldb on the binary). The
  `GDB=1` gate lives in the freertos_cpp run scripts, so scaffolded
  projects and the standalone probes share it.

## v3/v4 additions

- **Targets**: + STM32F4-class (Cortex-M4F) and **Raspberry Pi Pico**
  (RP2040, Cortex-M0+ → `thumbv6m` + FreeRTOS `ARM_CM0` port, qemu
  `mps2-an385` ISA-superset stand-in); File ▸ New Project gains a
  Pico flavor.
- **Local host debug** — Target=Host ⌘⇧D builds and opens `lldb` on
  the project's own binary (no qemu anywhere).
- **Autocompletion** — Ctrl+Space pops candidates (grammar/Rust
  keywords + every identifier from all open buffers) next to the
  cursor; Enter/click inserts, Escape dismisses.
- **Firmware upload (⌘U)** with **configurable tools**: per-project
  `upload.toml` (Project ▸ Edit Upload Config… opens/creates it)
  maps target families to shell templates with `{elf}`/`{dir}`/
  `{port}` placeholders — defaults: `STM32_Programmer_CLI` (STM32),
  `esptool.py` elf2image + write_flash (ESP32), `picotool load`
  (Pico). Output streams to the console. Caveat in the file itself:
  the qemu-validated ELFs use the qemu machines' memory maps — point
  the linker scripts at your board before flashing real hardware.

## v5 additions

- **Interactive debugger** (Target = Host) — the **Debug** menu drives
  a live `lldb` session *inside* the IDE: **Start Session (F5)**
  builds a debug-profile binary and attaches lldb through a pty
  (`script -q` — piped stdin would be stolen by the inferior after
  `run`); **Toggle Breakpoint (F8** or **⌘D** in the editor**)** marks
  lines red and replays them into any live session; **Step
  Over/Into/Out (F10/F11/⇧F11)**, **Continue (F9)**, **Show Variables
  (F7)**, **Stop (⇧F5)**. The console doubles as the lldb transcript,
  and on every stop the IDE parses `… at file:line`, jumps the editor
  there, and tints the current line amber (breakpoint lines red) via
  a style-buffer overlay.
- **Tabs** — a real **`Fl_Tabs`** strip above the editor (a fork
  `class FileTabs : Fl_Tabs` over the imported chain, one zero-height
  child page per open buffer; the shared editor stays outside the
  tabs, so selecting a tab just swaps buffers) plus **File ▸ Close
  File (⌘W)**; Wrap Lines moved to ⌘⇧W. Line numbers are on in the
  gutter. Pages rebuild only when the open set changes — a plain
  click only syncs selection, so `Fl_Tabs::handle` never deletes the
  widgets it is processing. **Every tab has an × close button**
  (FLTK 1.4's `FL_WHEN_CLOSED`): the × fires the page's callback
  with `FL_REASON_CLOSED`, which only *records* the index — the
  main loop performs the close (`tabs_pump`), since the click is
  still inside `Fl_Tabs::handle`. Closing a background tab keeps
  the current view. Wiring the callback surfaced an importer gap,
  now fixed (M15.c): inline methods with function-pointer params/
  returns previously got a Rust decl but no C++ shim — undefined
  symbol the moment they were used.
- **File ▸ New Project covers every board family**: Host, RAK11161,
  **STM32**, **ESP32**, Raspberry Pi Pico. The RTOS flavors share one
  self-contained scaffold (all cores' run scripts ship in every
  project) and differ only in the default Target they select — so a
  "STM32 project" can still be rebuilt for the ESP32-C2 core from the
  Target menu without rescaffolding.
- **Variables window (F7)** — frame locals re-capture automatically
  on **every stop** (a sentinel-bracketed `frame variable` round-trip
  through the lldb pty; the prompt is newline-less, so the sentinel
  match is `ends_with`), and a **watch box**: type a global/static's
  name (read via `target variable` — works from any frame) or any
  expression (via `expression --`), Enter appends the result. The
  Host template ships a `static EXCITEMENT_BASE: i32` to try it on.
  Capture payloads stay out of the console; process events (stops,
  exits) still stream there. Stops in *other* open files now switch
  tabs before the amber current-line tint lands.
- **File ▸ New opens an "unsaved" tab** — each ⌘N is its own buffer
  + tab (unsaved, unsaved-2, …); Save / Save As renames the tab in
  place to the real file. File ▸ Open also routes through the
  multi-buffer path now (it used to load into the current view).
- **Draggable splitters** — nav | editor and editor | console borders
  drag (`Fl_Tile` owns everything under the menu bar; the tab strip +
  editor share a group whose `resizable` is the editor, so the strip
  keeps its height). The window itself resizes proportionally.
- **Paths display project-relative** everywhere (nav, console
  messages), with a canonicalized fallback for `/tmp` → `/private/tmp`
  style symlinks; full paths only for files outside the project.
- **File ▸ Remove from Project** — closes the tab and hides the file
  from the navigator (persisted as `exclude =` lines in
  `.rustcc_ide.toml`); the file stays on disk. **File ▸ Delete
  File…** — `fl_choice` confirm, then removes it from disk and closes
  the tab. (Wiring the confirm dialog exposed another importer bug,
  now fixed: variadic functions mangled without the trailing `z` —
  `fl_choice` is printf-style — producing unlinkable symbols.)
- **Per-project Target persistence** — the selected Target is written
  to `<project>/.rustcc_ide.toml` (stable slugs, not indices) on every
  Target-menu change; Open Project restores it. Fresh projects are
  seeded with their flavor's default.
- **Help menu** — *rustcc IDE Help… (F1)* opens a cheat-sheet window
  (projects/targets, debugger keys, editing keys); *About* prints
  version + links to the console. The whole menu is now a `MENU_SPEC`
  const table, and the self-test rejects any label with a `/` inside
  parentheses — FLTK treats every slash as a submenu separator, which
  is how *Project ▸ Debug in Terminal (qemu/lldb)…* used to render as
  a broken nested submenu (now just *Project ▸ Debug…*, ⌘⇧D).
- **Host scaffold is a real Hello World** — a `Greeter` fork class
  (virtual `excitement_level()`) + a free `greeting() -> String`
  function, a `build.rs` that links the platform C++ runtime (debug
  profile keeps `_ZTI*` references), and tasks/build lines that pin
  `RUSTC` to the fork stage1 so `cargo +nightly` works out of the box.
  Class methods stay C++-compatible (`i32`), Rust-typed logic lives in
  free functions — the fork's own diagnostic taught the template that.

The FULL self-test now also drives a complete scripted debug session
through the IDE engine: set breakpoint → run → hit → **step-in lands
inside `Greeter::Greeter(excitement=3)` — the fork-emitted C++
constructor, with its parameter readable in the frame** → `frame
variable` shows `excitement` → disable → continue → clean exit. Waits
are stop-synced (each command waits for its own `stop reason =` /
effect in *new* console output before the next is sent — lldb is
async, and firing commands mid-step makes them land on a running
process).

Workflow: **File ▸ New Project ▸ RAK11161 Project…** (pick a folder) →
edit `src/lib.rs` (a fork `class` crate: `Widget`, `Gauge : Widget`,
imported `Sensor`, `Reader : Sensor`) → pick a core in **Target** →
**⌘B** builds (link-only via `SKIP_QEMU=1`), **⌘R** builds *and
executes the firmware under qemu*, with toolchain + scheduler + probe
output streaming live into the console pane (`Fl::check()` pump — the
UI stays responsive mid-build). Expected run output ends with:

```
FREERTOS CXX PROBE (…): PASS (105/4000/503/42 across tasks)
```

Scaffolded projects are self-contained (sources, FreeRTOS config,
linker scripts, per-core run scripts, VSCode tasks) and work without
the IDE: `./run_arm.sh`, `./run_riscv_c2.sh`.

## Self-test

```sh
cargo +rustcc run --release --bin ide -- --self-test
RUSTCC_IDE_SELFTEST_FULL=1 ./target/release/ide --self-test   # + real CM4 build + qemu
```

The headless suite runs the editor probes plus scaffold verification
(file set, path rewrites, `SKIP_QEMU` gate, VSCode tasks, `bash -n`
on the scripts). The `FULL` gate scaffolds into a temp dir and drives
a complete CM4 firmware build + qemu execution through the IDE's own
streamed-console engine, asserting the `PASS (105/4000/503/42)` line
arrives — the whole IDE story, end to end, in one assertion.

## Why this is a good fork test

One binary exercises: the `class` keyword + subclassing FLTK across a
4-level imported chain, ctor-in-place, inline-method shims, nested
records (style tables), `#define`/enum constants, native dialogs,
C→Rust function-pointer callbacks, `std::process` orchestration of
the fork toolchain itself, and — through what it builds — the
bare-metal + FreeRTOS story on two embedded architectures.
