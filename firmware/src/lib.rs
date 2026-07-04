#![no_std]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

//! dew firmware support library.
//!
//! Everything reusable behind the display bring-up lives here; the boot and
//! render-loop orchestration stays in the `dew-firmware` binary
//! (`src/bin/main.rs`). Modules:
//!
//! - [`board`] — panel/framebuffer constants and the PSRAM framebuffer allocator.
//! - [`st7701`] — the ST7701S register init table and its bit-banged 9-bit SPI.
//! - [`framebuffer`] — the RGB565 [`FrameBuf`](framebuffer::FrameBuf) draw target
//!   and integer-only drawing primitives.
//! - [`palette`] — the dashboard colour palette.
//! - [`widgets`] — the humidity/temperature/watering gauges and static layout.
//! - [`telemetry`] — the (currently synthetic) sensor value source.

extern crate alloc;

pub mod board;
pub mod framebuffer;
pub mod palette;
pub mod st7701;
pub mod telemetry;
pub mod widgets;
