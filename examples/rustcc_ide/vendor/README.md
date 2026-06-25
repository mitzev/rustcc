# Vendored host flash tools

The IDE flashes firmware over the **serial port** with two open-source
Rust tools, kept here so the example is self-contained. Each is a plain
(non-fork) crate that builds with stock cargo and installs a CLI the IDE
invokes via `upload.toml`.

| Tool | Source | Vendored as | License | Used for |
|---|---|---|---|---|
| **stm32-uart-boot** | https://github.com/cbiffle/stm32-uart-boot | flat copy (≈100 KB) | MPL-2.0 | STM32 UART system bootloader (AN3155) — `[stm32]` |
| **espflash** | https://github.com/esp-rs/espflash | **git submodule**, pinned to `v4.4.0` | MIT OR Apache-2.0 | ESP32 family serial bootloader — `[esp32]` |

- **stm32-uart-boot** is a flat vendored copy (`.git` removed) — it isn't
  published to crates.io, and it's tiny.
- **espflash** is a **git submodule** (pinned to `v4.4.0`) rather than a
  flat copy — its source + embedded per-chip ROM images run to several MB,
  too heavy to keep in-tree. Initialize it before building:

  ```sh
  git submodule update --init examples/rustcc_ide/vendor/espflash
  ```

`target/` build dirs are git-ignored.

## Install (puts the CLIs on `~/.cargo/bin`, which the IDE's build
## shell already has on PATH)

```sh
git submodule update --init examples/rustcc_ide/vendor/espflash   # once
cargo install --path vendor/stm32-uart-boot --locked
cargo install --path vendor/espflash/espflash --locked
```

## How the IDE uses them

The scaffolds' `upload.toml` calls them by name (Device-mode Build &
Run, or the Upload action), substituting `{port}`/`{elf}`:

```toml
[stm32]
cmd = "stm32-uart-boot {port} load {elf}"     # chip must be in bootloader mode (BOOT0+reset)
[esp32]
cmd = "espflash flash --port {port} --baud 460800 {elf}"
```

The previous proprietary commands (`STM32_Programmer_CLI`, `esptool.py`)
remain as commented fallbacks in each `upload.toml`.
