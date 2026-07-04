# CLAUDE.md

Guidance for working in this repository.

## Project

`dew` is an embedded project for a plant monitoring and automated watering
system. The current code is firmware for a **Waveshare ESP32-S3-Touch-LCD-2.1**
(ESP32-S3R8: Xtensa LX7, 16 MB flash, 8 MB octal PSRAM, 480×480 round IPS touch
display). Long-term this grows into sensing (humidity/soil), a touch UI, and
pump/valve control.

The firmware is a `no_std` **Embassy** application under `firmware/`.

## Toolchain

The ESP32-S3 core is **Xtensa**, which needs the `esp` Rust toolchain fork
(installed via `espup`), not stock Rust. `rust-toolchain.toml` pins
`channel = "esp"`.

Build requires the esp toolchain env vars. In Git Bash:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
export LIBCLANG_PATH="C:\\Users\\james\\.rustup\\toolchains\\esp\\xtensa-esp32-elf-clang\\esp-clang\\bin\\libclang.dll"
export PATH="/c/Users/james/.rustup/toolchains/esp/xtensa-esp32-elf-clang/esp-clang/bin:/c/Users/james/.rustup/toolchains/esp/xtensa-esp-elf/bin:$PATH"
```

`C:\Users\james\export-esp.ps1` holds the same for PowerShell.

## Build & flash

Run from `firmware/`.

```sh
# Build (PSRAM code ONLY works in release — always release for display work)
cargo build --release

# Flash + monitor (single board, always COM3). Note: espflash has no --release
# flag; point it at the release-profile ELF instead.
espflash flash --monitor --chip esp32s3 --port COM3 \
  target/xtensa-esp32s3-none-elf/release/dew-firmware
```

`cargo run --release` also works (the runner in `.cargo/config.toml` calls
espflash). `espflash` is on the Windows `cargo` PATH (`~/.cargo/bin`), not
necessarily the Git Bash PATH — export it as above.

### Gotchas

- **PSRAM requires `--release`.** A debug build silently fails to use PSRAM, so
  the framebuffer allocation breaks.
- **espflash holds COM3 exclusively.** Kill a lingering monitor before
  re-flashing: `taskkill //F //IM espflash.exe`.
- **The serial monitor attaches after boot** and misses early logs. Reset the
  chip (Ctrl+R in the monitor, or `espflash reset --port COM3`) to see them.
- **The buzzer is on TCA9554 expander bit 7 (active high).** Any code that
  writes the expander output register must keep bit 7 low or the board screams.

## Firmware architecture

Single binary: `firmware/src/bin/main.rs`.

The display is an **ST7701S over a 16-bit RGB565 parallel bus** (not SPI). Boot
flow:

1. Bring up PSRAM as heap (`esp_alloc::psram_allocator!`) + a small internal heap.
2. Backlight on (GPIO6).
3. Configure the **TCA9554 I2C expander** (addr `0x20` on SDA=GPIO15/SCL=GPIO7):
   toggle LCD reset (bit0) and hold chip-select (bit2) low for init.
4. Send the ST7701S register init sequence over a **bit-banged 3-wire 9-bit SPI**
   (SDA=GPIO1, SCL=GPIO2).
5. Allocate a 480×480×2 = 460,800-byte RGB565 framebuffer in PSRAM, paint it with
   `embedded-graphics` via the `FrameBuf` `DrawTarget`.
6. Configure the **LCD_CAM DPI** peripheral (pins + timing) and stream the
   framebuffer to the panel by looping `dpi.send(false, buf)` + `wait()` — each
   send emits one full frame (~58 fps); re-sent continuously for a static image.

### Hardware pin map (Waveshare ESP32-S3-Touch-LCD-2.1)

| Function | Pins |
| --- | --- |
| RGB data `data0..15` = B0..4, G0..5, R0..4 | B: 5,45,48,47,21 · G: 14,13,12,11,10,9 · R: 46,3,8,18,17 |
| RGB timing | PCLK 41, DE 40, HSYNC 38, VSYNC 39 |
| Panel timing | 16 MHz; H active480/sync8/bp10/fp50; V active480/sync3/bp8/fp8 |
| Backlight | GPIO6 (active high) |
| Panel init SPI | SDA GPIO1, SCL GPIO2 (mode 0, D/C bit then 8 bits MSB) |
| I2C bus (expander + touch) | SDA GPIO15, SCL GPIO7, 400 kHz |
| TCA9554 @ 0x20 | bit0 LCD_RST, bit1 TP_RST, bit2 LCD_CS, **bit7 BUZZER (keep low)** |
| Touch (CST816 @ 0x15) | INT GPIO16, RST = expander bit1 — *not yet driven* |

## Conventions

- Keep hardware constants (pins, expander bits, init bytes) named and grouped as
  they are in `main.rs`; the ST7701S init table mirrors Waveshare's demo values.
- The render loop currently blocks re-sending a static frame. When adding UI or
  concurrent Embassy tasks, move to a redraw-on-change model so sensor/control
  tasks can run.
