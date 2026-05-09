# rustcc-tools — VS Code extension

Syntax + commands + snippets for [rustcc](https://github.com/rustcc/rustcc), the C++/Swift-interop fork of rustc.

## What it does

### Highlighting

Injects an overlay into the existing Rust grammar that picks out:

- The `class` keyword and its bound type identifier
- `extern "C++"` and `extern "swiftcall"` / `extern "Swift"` ABI strings
- `#[cpp_virtual]`, `#[constructor]`, `#[swift_throws]`, `#[swift_value]`
- `#[repr(cpp)]`, `#[repr(swift)]`

Stock rust-analyzer treats `class` as an ordinary identifier and complains; this overlay at least makes the keyword visually distinct so the source is readable. Full HIR-level resolution still needs the rust-analyzer fork (`fork/ra-patches/01-ra-class-keyword.patch`).

### Snippets

Type any of these prefixes for autocomplete:

| Prefix | Expands to |
|---|---|
| `cppclass` / `rustcc-class` | `pub class ... { ... }` with constructor + one virtual method |
| `cppclass-inherit` | Derived class with `__base` field and an override |
| `cxxclass` / `cxx-class` | `cxx_class! { ... }` proc-macro form (stable-rustc compatible) |
| `swiftvalue` / `swift-value` | `#[repr(swift)]` `swift_value! { ... }` |
| `rustcc-build` / `m26-build` | Skeleton `build.rs` invoking `cxx_importer::build::Build` |
| `rustcc-include-bindings` | One-line `include!(concat!(env!("OUT_DIR"), "/bindings.rs"));` |

### Commands (Cmd/Ctrl+Shift+P)

| Command | What it does |
|---|---|
| `rustcc: Install Toolchain (latest)` | Shells to `rustcc install` (latest or pinned version) |
| `rustcc: Run Doctor` | Shells to `rustcc doctor` in a terminal |
| `rustcc: Generate Bindings for Header` | If the project has a `gen_bindings` binary, runs it; otherwise prompts |
| `rustcc: Show Bindings Skips` | Reads any `bindings.skips.json` in the workspace, summarizes by class in an output channel |
| `rustcc: Install RA Fork (latest)` | Downloads `rust-analyzer-rustcc-<triple>.tar.xz` from the GitHub release, extracts to the extension's storage dir, sets `rust-analyzer.server.path` (workspace-scoped). One-click bootstrap of the patched RA binary that understands the `class` keyword. |

### Status bar

Shows the active toolchain pin from `rust-toolchain.toml` in the bottom-right:

- 🚀 `rustcc` — pinned to the fork
- ○ `rustcc: stable` — pinned to a different channel
- ○ `rustcc` — no `rust-toolchain.toml`

Click for the doctor output.

### Problems pane

If `cxx_importer::build::Build` produced a `bindings.skips.json` (PR #22), every skipped method surfaces as an info-level diagnostic on `bindings.rs` so you don't have to grep.

## Install (sideload)

```bash
cd tools/vscode-rustcc
npm install
npm run compile
npm run package      # writes rustcc-tools.vsix
code --install-extension rustcc-tools.vsix
```

The extension talks to the [rustcc CLI](../../crates/rustcc-cli/) for install / doctor. If `rustcc` isn't on PATH, set `rustcc.cliPath` in VS Code settings to point at the binary.

## Roadmap

- v0.1 (this): grammar overlay, snippets, 4 commands, status bar, skip-diagnostics
- v0.2: bundle the rust-analyzer fork binary; source-position-aware skip diagnostics
- v0.3: hover provider with Itanium-mangled symbols + vtable indices

The v0.1 scope was deliberately chosen to work entirely without the fork — every command shells out to the CLI, so users without a rustcc toolchain installed still get syntax + snippets.
