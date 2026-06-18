# swiftui_ide — a SwiftUI rustcc IDE (the inverse of the FLTK one)

The [`rustcc_ide`](../rustcc_ide) sample is an embedded-RTOS IDE
written **entirely in fork Rust** — every widget is a `class`
subclassing imported FLTK C++. This is its mirror image: the same IDE
shape with a **SwiftUI front-end** over a **fork-Rust engine**, bridged
by the fork's Swift `swiftcc` ABI (`extern "Swift"`).

```text
  ┌──────────────────────────┐  swiftcc (extern "Swift")  ┌──────────────┐
  │  SwiftUI app (Swift)     │  ─────────────────────────▶ │  Rust engine │
  │  @main · View · @State   │   @_silgen_name("rc_*")      │  (src/lib.rs)│
  └──────────────────────────┘  ◀───────────────────────── └──────────────┘
       the entire UI            C-strings · file lists       scaffold·build
                                 · streamed console           ·run·project
```

## Why it's inverted (and not "Rust drives SwiftUI")

SwiftUI **cannot be driven from Rust** the way FLTK can. Its surface —
`@main`, the `View` protocol with `some View` opaque returns,
`@ViewBuilder` result builders, `@State`/`@Observable` — is built from
Swift-compiler constructs that cross no ABI. There is no function to
call to "make a view". So the view tree stays in Swift; what crosses
the boundary is the IDE **engine**: scaffolding, the per-target
build/run command, process streaming, the project/file model. None of
that was ever FLTK-specific — it ports straight across from
`rustcc_ide`'s `src/main.rs`. (Same lesson the
[`swiftui_counter`](../swiftui_counter) README states; this is that
example grown into a real tool.)

## What the fork contributes

The fork lets Rust **define** `extern "Swift"` functions — they use
Swift's `swiftcc` calling convention, so Swift calls them as ordinary
Swift functions (bound by symbol via `@_silgen_name`). Where
`swiftui_counter` passes only `i64` scalars, an IDE needs real data, so
this bridge moves **C-strings** (file paths, source text, console
output) and **lists** (newline-joined file enumerations) across
`swiftcc`, plus a **drain buffer** the SwiftUI side polls on a timer
(there is no FLTK event loop to pump). Each engine entry point is:

```rust
#[export_name = "rc_list_files"]
pub extern "Swift" fn rc_list_files() -> *mut c_char { … }   // caller frees via rc_string_free
```

bound on the Swift side as:

```swift
@_silgen_name("rc_list_files") func rc_list_files() -> UnsafeMutablePointer<CChar>?
```

## What it does

- **File ▸ New Host** — scaffolds a fork-Rust Hello-World project (a
  `class Greeter` with a virtual method + a free `greeting() -> String`)
  and opens it.
- **Open** — pick any folder; the sidebar lists its source files.
- **Editor** — a `TextEditor` bound to the selected file; **Save**
  writes it back through the engine.
- **Target picker** — the same six targets as the FLTK IDE (Host,
  RAK11161 ×2, ESP32-C3, STM32F4, Pico).
- **Build / Run** — runs the engine's per-target command (identical to
  the FLTK IDE's `target_cmdline`, so a project scaffolded by either
  tool builds the same way), streaming toolchain output live into the
  console pane.
- **Debug** (Host target) — an in-IDE `lldb` session: **Start Debug**
  builds the debug profile and attaches lldb over a pty (the engine
  spawns it on a background thread; the transcript streams into the
  console). **Step Over/Into/Out**, **Continue**, **Variables**, and
  **Stop**; a **⏸ file:line** banner shows the current stop. Set
  breakpoints with the **BP line** stepper + **Toggle BP** (SwiftUI's
  `TextEditor` exposes no gutter or cursor line, so breakpoints are
  placed by line number — set ones show as red ● chips you click to
  remove); they replay into a live session.

## Build & run (macOS)

```sh
./build.sh test     # engine self-test only — no Swift toolchain needed
./build.sh          # build the engine + the SwiftUI app bundle
./build.sh run      # build, then launch RustccIDE.app
```

`build.sh` builds `libswiftui_ide.a` with the fork rustc (`RUSTC=` env
or the default migration-tree path), links the SwiftUI sources against
it with `swiftc`, and wraps the result in a minimal `.app` bundle.

## Self-test

```sh
RUSTC=<fork-stage1>/bin/rustc RUSTC_BOOTSTRAP=1 cargo +nightly test
```

The engine is validated **headlessly** through the exact `extern
"Swift"` entry points the app links against (no Swift toolchain, no
GUI): the target table, a scaffold → list → read → edit → save → reopen
round-trip, the streamed-console drain (a real subprocess, polled the
way the SwiftUI timer does), the per-target command shapes, and the
breakpoint bookkeeping + lldb stop-frame parser. The GUI itself is
verified by the build linking cleanly against the staticlib (all
`rc_*` symbols resolved) — the same bar as `swiftui_counter`.

A **gated** end-to-end test drives a real lldb session against a
scaffolded host binary (set breakpoint → run → hit → `frame variable`
→ continue → exit), mirroring the FLTK IDE's FULL gate:

```sh
RUSTCC_SWIFTUI_IDE_FULL=1 RUSTC=<fork-stage1>/bin/rustc \
    RUSTC_BOOTSTRAP=1 cargo +nightly test full_lldb_session
```

## Scope

This is the **MVP**: scaffold/open/edit/save + build/run a **Host**
project end to end. The FLTK IDE's later features — RTOS project
scaffolds (the `include_str!` firmware embeds), the in-IDE lldb
debugger, tabs, find/replace, firmware upload — are deliberately left
as follow-ups; each maps onto an additional `rc_*` engine call plus
SwiftUI views, exactly as they were added to `rustcc_ide` iteratively.
The Target menu lists all six cores; building an RTOS target requires a
project scaffolded with the run scripts (today: scaffold those with the
FLTK IDE, open the folder here).

## Why this is a good fork test

It exercises the Swift-calling-Rust direction (`swiftcc` /
`extern "Swift"`) with **non-trivial data** — strings, lists, and a
streamed subprocess — rather than the counter's three integers, and it
proves the orchestration engine is genuinely UI-agnostic: the same Rust
that drove a C++ FLTK UI now drives a Swift SwiftUI one, unchanged.
