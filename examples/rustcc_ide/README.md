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

Workflow: **Project ▸ New RAK11161 Project…** (pick a folder) →
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
