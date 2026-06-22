# Vendored host flash tools

The IDE flashes firmware over the **serial port** with two open-source
Rust tools, vendored here so the example is self-contained. Each is a
plain (non-fork) crate that builds with stock cargo and installs a CLI
the IDE invokes via `upload.toml`.

| Tool | Source | License | Used for |
|---|---|---|---|
| **stm32-uart-boot** | https://github.com/cbiffle/stm32-uart-boot | MPL-2.0 | STM32 UART system bootloader (AN3155) — `[stm32]` |
| **espflash** | https://github.com/esp-rs/espflash (v4.4.0) | MIT OR Apache-2.0 | ESP32 family serial bootloader — `[esp32]` |

These are **vendored clones with `.git` removed** (re-vendor to update).
`espflash/espflash/tests/data/` (≈45 MB of test-only ELF fixtures) was
deleted — not needed to build the CLI; `resources/roms/*.elf` (embedded
at build time) are kept. `target/` is git-ignored.

## Install (puts the CLIs on `~/.cargo/bin`, which the IDE's build
## shell already has on PATH)

```sh
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
