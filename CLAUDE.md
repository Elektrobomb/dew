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

The crate is a thin binary over a support library:

- `src/bin/main.rs` — boot sequence and the render loop (this is the code the
  "Boot flow" below walks through).
- `src/lib.rs` (`dew_firmware`) — everything reusable, split into modules:
  - `board` — panel/framebuffer constants (`H_RES`/`V_RES`/`FB_SIZE`, DMA
    alignment, `EXIO_*` expander bits) and `alloc_framebuffer()`.
  - `st7701` — the `ST7701_INIT` register table and `send_init()` (the
    bit-banged 9-bit SPI).
  - `framebuffer` — the RGB565 `FrameBuf` `DrawTarget` and the integer-only
    drawing primitives (`fill_rect`/`fill_disc`/`ring`/`arc_top`/`isqrt`/
    `text_center`).
  - `palette` — the RGB565 colour constants.
  - `widgets` — the humidity/temperature/watering gauges and `draw_static`.
  - `telemetry` — `telemetry()`, the (currently synthetic) sensor value source.

The display is an **ST7701S over a 16-bit RGB565 parallel bus** (not SPI). Boot
flow:

1. Bring up PSRAM as heap (`esp_alloc::psram_allocator!`) + a small internal heap.
2. Backlight on (GPIO6).
3. Configure the **TCA9554 I2C expander** (addr `0x20` on SDA=GPIO15/SCL=GPIO7):
   toggle LCD reset (bit0) and hold chip-select (bit2) low for init.
4. Send the ST7701S register init sequence over a **bit-banged 3-wire 9-bit SPI**
   (SDA=GPIO1, SCL=GPIO2).
5. Allocate a 480×480×2 = 460,800-byte RGB565 framebuffer in PSRAM.
6. Configure the **LCD_CAM DPI** peripheral (pins + timing) and stream the
   framebuffer to the panel by looping `dpi.send(false, buf)` + `wait()` — each
   send emits one full frame (~58 fps). The dashboard is drawn between frames;
   see "Rendering" below.

### Rendering & the PSRAM/DMA constraint (important, hard-won)

The DPI peripheral streams the framebuffer **directly from PSRAM via DMA, with no
bounce buffer**. Golden rule: **never write the framebuffer while its DMA is
reading it.** Two failure modes we hit, and why:

- **Double buffering (draw *while* streaming) → diagonal shear.** Rendering into
  a second PSRAM buffer while the first is streamed makes the CPU and the display
  DMA contend for PSRAM bandwidth; the display FIFO underruns and every scanline
  shifts a few bytes → diagonal stripes.
- **Single buffer, full-screen redraw *between* frames → whole-screen flicker.**
  Stopping the stream to repaint the whole ~460 KB (~5 ms) leaves the panel
  unfed. RGB panels have no on-glass memory, so it briefly blanks.

**Working approach (current): single framebuffer + dirty-rectangle updates drawn
between frames.** `draw_static` paints the background/labels once; each frame only
the widget whose value changed is repainted (sub-millisecond), in the gap between
`wait()` and the next `send()`. No contention (no shear); the gap is tiny and
recurs at ~58 Hz, above the flicker-fusion threshold (no visible flicker).

Corollary for the real UI: keep it a **mostly-static layout with small per-value
dirty-rect updates**. `telemetry()` (in the `telemetry` module) is the seam where
real sensor reads replace the animated placeholder sweeps.

Drawing helpers: gauges (drop / thermometer / cloud) are hand-drawn into the
RGB565 buffer with integer-only helpers (`fill_rect` / `fill_disc` / `ring` /
`arc_top`, built on `isqrt`) because a fill-to-a-level shape needs per-row
clipping the stock embedded-graphics primitives don't provide. Text uses
embedded-graphics `MonoTextStyle` with **iso_8859_1** fonts (needed for the `°`
glyph; the plain `ascii` fonts lack it). Colours are `Rgb565::new(r,g,b)` with
`r`/`b` in 0..31 and `g` in 0..63.

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

- Keep hardware constants (pins, expander bits, init bytes) named and grouped in
  their module (`board`, `st7701`); the ST7701S init table mirrors Waveshare's
  demo values. Pin assignments passed to esp-hal builders stay inline in
  `main.rs` (they're one-shot boot wiring, not reusable constants).
- Follow the dirty-rect rendering pattern above for any new on-screen element:
  add a widget draw fn in the `widgets` module that clears its own bounding box
  and repaints, and only call it when its value changes. Don't reintroduce
  full-screen redraws or double buffering (see the PSRAM/DMA constraint).
- The main loop currently spins on `send()`/`wait()` (blocking) and computes
  placeholder `telemetry()`. When adding sensor/control logic, it can move to
  concurrent Embassy tasks that publish values, with the render loop consuming
  them — the executor is otherwise idle.
