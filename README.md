# dew 💧🪴

Making water out of thin air so your plants never have to ask. An automated,
self-hydrating monitoring system that turns humidity into hydration. Just add
power. ⚡

## High-level concept

A system to monitor small plant soil moisture levels and condense water out of the air on-demand when needed.  

## Hardware

- **Board:** [Waveshare ESP32-S3-Touch-LCD-2.1](https://www.waveshare.com/wiki/ESP32-S3-Touch-LCD-2.1)
  — ESP32-S3R8 (Xtensa LX7 dual-core, 16 MB flash, 8 MB octal PSRAM).
- **Display:** 2.1" 480×480 round IPS, ST7701S controller over a 16-bit RGB
  parallel bus.
- **Touch:** CST816 capacitive controller (I2C).
- A TCA9554 I2C GPIO expander handles LCD reset / chip-select, touch reset, and
  the on-board buzzer.

## Firmware

The firmware is a `no_std` [Embassy](https://embassy.dev/) application written in
Rust, living in [`firmware/`](firmware/). It uses `esp-hal` for the ESP32-S3,
`esp-rtos` for the async executor, and `embedded-graphics` for drawing.

Because the ESP32-S3 is an Xtensa target, it builds with the `esp` Rust toolchain
fork (installed via [`espup`](https://github.com/esp-rs/espup)) and is flashed
with [`espflash`](https://github.com/esp-rs/espflash).

### Quick start

```sh
cd firmware

# PSRAM (the framebuffer) requires a release build.
cargo build --release

# Flash the connected board and open a serial monitor.
espflash flash --monitor --chip esp32s3 --port COM3 \
  target/xtensa-esp32s3-none-elf/release/dew-firmware
```

See [`CLAUDE.md`](CLAUDE.md) for the full toolchain setup, the board pin map, and
architecture notes.

## License

See [LICENSE](LICENSE).
