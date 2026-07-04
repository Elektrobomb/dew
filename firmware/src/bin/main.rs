#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

//! dew firmware — ST7701S 480x480 RGB display bring-up.
//!
//! Draws a plant-care telemetry dashboard on the Waveshare
//! ESP32-S3-Touch-LCD-2.1: a humidity water-drop gauge, a temperature
//! thermometer, and a raincloud watering-status indicator.
//!
//! The panel is an ST7701S driven over a 16-bit RGB565 parallel bus (esp-hal
//! LCD_CAM DPI peripheral), streamed continuously from a framebuffer in PSRAM
//! via DMA. The panel's configuration registers are programmed over a 3-wire
//! 9-bit SPI that we bit-bang on GPIO1 (SDA) / GPIO2 (SCL); chip-select and
//! reset live on a TCA9554 I2C GPIO expander (addr 0x20, SDA=GPIO15/SCL=GPIO7).

extern crate alloc;

use alloc::format;

use embassy_executor::Spawner;
use embedded_graphics::{
    mono_font::{
        iso_8859_1::{FONT_10X20, FONT_9X15},
        MonoFont, MonoTextStyle,
    },
    pixelcolor::Rgb565,
    prelude::*,
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    dma::{DmaTxBuf, ExternalBurstConfig},
    gpio::{Level, Output, OutputConfig},
    i2c::master::{Config as I2cConfig, I2c},
    lcd_cam::{
        lcd::{
            dpi::{Config as DpiConfig, Dpi, Format, FrameTiming},
            ClockMode, Phase, Polarity,
        },
        LcdCam,
    },
    time::Rate,
};
use log::info;

esp_bootloader_esp_idf::esp_app_desc!();

const H_RES: usize = 480;
const V_RES: usize = 480;
const FB_SIZE: usize = H_RES * V_RES * 2; // RGB565, 2 bytes/pixel = 460_800

// DMA over PSRAM needs 64-byte external burst alignment; keep chunks under 4092.
const DMA_ALIGNMENT: ExternalBurstConfig = ExternalBurstConfig::Size64;
const DMA_CHUNK_SIZE: usize = 4096 - DMA_ALIGNMENT as usize;

// TCA9554 expander bit assignments (Waveshare 2.1 board).
const EXIO_LCD_RST: u8 = 0; // bit0
const EXIO_TP_RST: u8 = 1; // bit1
const EXIO_LCD_CS: u8 = 2; // bit2
const EXIO_BUZZER: u8 = 7; // bit7 (EXIO_PIN8) — active high, keep LOW

macro_rules! dma_alloc_buffer {
    ($size:expr, $align:expr) => {{
        let layout = core::alloc::Layout::from_size_align($size, $align).unwrap();
        unsafe {
            let ptr = alloc::alloc::alloc(layout);
            if ptr.is_null() {
                alloc::alloc::handle_alloc_error(layout);
            }
            core::slice::from_raw_parts_mut(ptr, $size)
        }
    }};
}

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    // Bring PSRAM online as heap so we can allocate the ~450 KB framebuffer.
    // (This build MUST be release — PSRAM code does not work in debug.)
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);
    // A small internal-RAM heap for everything else (fast, DMA-reachable).
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);

    let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
    let sw_int =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    info!("dew: ST7701S bring-up starting");

    let delay = Delay::new();

    // --- Backlight on (GPIO6) ---
    let mut backlight = Output::new(peripherals.GPIO6, Level::Low, OutputConfig::default());
    backlight.set_high();

    // --- I2C to the TCA9554 expander (SDA=GPIO15, SCL=GPIO7) ---
    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .unwrap()
    .with_sda(peripherals.GPIO15)
    .with_scl(peripherals.GPIO7);

    // All expander pins as outputs. Start with everything high EXCEPT the
    // buzzer (bit7), which is active-high and must stay low.
    let mut exio: u8 = 0xFF & !(1 << EXIO_BUZZER);
    i2c.write(0x20, &[0x03, 0x00]).unwrap(); // config reg: 0 = output
    i2c.write(0x20, &[0x01, exio]).unwrap(); // output reg

    // --- Panel reset via expander bit0 ---
    exio &= !(1 << EXIO_LCD_RST);
    i2c.write(0x20, &[0x01, exio]).unwrap();
    delay.delay_millis(20);
    exio |= 1 << EXIO_LCD_RST;
    i2c.write(0x20, &[0x01, exio]).unwrap();
    delay.delay_millis(80);
    let _ = EXIO_TP_RST; // touch reset unused for this test

    // --- 3-wire SPI init: SDA=GPIO1, SCL=GPIO2, CS via expander bit2 ---
    let mut sda = Output::new(peripherals.GPIO1, Level::High, OutputConfig::default());
    let mut scl = Output::new(peripherals.GPIO2, Level::High, OutputConfig::default());

    // Keep VSYNC (GPIO39) high during panel setup so the panel doesn't latch
    // garbage before the DPI peripheral takes over.
    let mut vsync_pin = peripherals.GPIO39;
    let vsync_guard = Output::new(vsync_pin.reborrow(), Level::High, OutputConfig::default());

    // CS low for the whole init.
    exio &= !(1 << EXIO_LCD_CS);
    i2c.write(0x20, &[0x01, exio]).unwrap();

    for cmd in ST7701_INIT {
        match *cmd {
            Init::Cmd(c) => spi9(&mut sda, &mut scl, c, false),
            Init::Data(d) => spi9(&mut sda, &mut scl, d, true),
            Init::Delay(ms) => delay.delay_millis(ms as u32),
        }
    }

    // CS high — panel is configured.
    exio |= 1 << EXIO_LCD_CS;
    i2c.write(0x20, &[0x01, exio]).unwrap();
    drop(vsync_guard);
    info!("dew: ST7701S init sequence sent");

    // --- Allocate the PSRAM framebuffer ---
    // esp-hal's DPI streams straight from PSRAM with no bounce buffer, so we must
    // never write the framebuffer while its DMA is reading it: doing so starves
    // the display DMA of PSRAM bandwidth and shears the image. We therefore draw
    // only between frames (while the DMA is idle) and, to keep that gap tiny,
    // repaint only the widget whose value actually changed.
    let fb: &'static mut [u8] = dma_alloc_buffer!(FB_SIZE, DMA_ALIGNMENT as usize);
    info!("dew: framebuffer @ {:p}", fb.as_ptr());

    // --- Configure the DPI (RGB) peripheral ---
    let lcd_cam = LcdCam::new(peripherals.LCD_CAM);
    let dpi_config = DpiConfig::default()
        .with_clock_mode(ClockMode {
            polarity: Polarity::IdleLow,
            phase: Phase::ShiftLow,
        })
        .with_frequency(Rate::from_mhz(16))
        .with_format(Format {
            enable_2byte_mode: true,
            ..Default::default()
        })
        .with_timing(FrameTiming {
            horizontal_active_width: H_RES,
            // total = active + hsync_width + back_porch + front_porch
            horizontal_total_width: H_RES + 8 + 10 + 50,
            horizontal_blank_front_porch: 50,
            hsync_width: 8,
            hsync_position: 0,

            vertical_active_height: V_RES,
            vertical_total_height: V_RES + 3 + 8 + 8,
            vertical_blank_front_porch: 8,
            vsync_width: 3,
        })
        .with_vsync_idle_level(Level::High)
        .with_hsync_idle_level(Level::High)
        .with_de_idle_level(Level::Low)
        .with_disable_black_region(false);

    let mut dpi = Dpi::new(lcd_cam.lcd, peripherals.DMA_CH0, dpi_config)
        .unwrap()
        .with_vsync(vsync_pin)
        .with_hsync(peripherals.GPIO38)
        .with_de(peripherals.GPIO40)
        .with_pclk(peripherals.GPIO41)
        // Blue: data0..data4 = B0..B4
        .with_data0(peripherals.GPIO5)
        .with_data1(peripherals.GPIO45)
        .with_data2(peripherals.GPIO48)
        .with_data3(peripherals.GPIO47)
        .with_data4(peripherals.GPIO21)
        // Green: data5..data10 = G0..G5
        .with_data5(peripherals.GPIO14)
        .with_data6(peripherals.GPIO13)
        .with_data7(peripherals.GPIO12)
        .with_data8(peripherals.GPIO11)
        .with_data9(peripherals.GPIO10)
        .with_data10(peripherals.GPIO9)
        // Red: data11..data15 = R0..R4
        .with_data11(peripherals.GPIO46)
        .with_data12(peripherals.GPIO3)
        .with_data13(peripherals.GPIO8)
        .with_data14(peripherals.GPIO18)
        .with_data15(peripherals.GPIO17);

    // --- Build the DMA framebuffer ---
    let (_, tx_descriptors) = esp_hal::dma_descriptors_chunk_size!(0, FB_SIZE, DMA_CHUNK_SIZE);
    let mut dma_buf = DmaTxBuf::new_with_config(tx_descriptors, fb, DMA_ALIGNMENT).unwrap();

    // Initial full paint: static background/labels plus all three gauges.
    let (mut rh, mut temp, mut watering) = telemetry(0);
    {
        let mut canvas = FrameBuf {
            buf: dma_buf.as_mut_slice(),
        };
        draw_static(&mut canvas);
        draw_humidity(&mut canvas, rh);
        draw_temperature(&mut canvas, temp);
        draw_watering(&mut canvas, watering);
    }

    info!("dew: streaming framebuffer to panel");

    // Single-buffer loop. Each iteration streams one frame; the (tiny) redraw of
    // any changed widget happens *between* frames, while the DMA is idle, so it
    // never contends with scanout. Repainting only what changed keeps the gap
    // sub-millisecond, so the flicker stays below the eye's fusion threshold.
    let mut frame: u32 = 1;
    loop {
        let (new_rh, new_temp, new_watering) = telemetry(frame);
        {
            let mut canvas = FrameBuf {
                buf: dma_buf.as_mut_slice(),
            };
            if new_rh != rh {
                draw_humidity(&mut canvas, new_rh);
            }
            if new_temp != temp {
                draw_temperature(&mut canvas, new_temp);
            }
            if new_watering != watering {
                draw_watering(&mut canvas, new_watering);
            }
        }
        (rh, temp, watering) = (new_rh, new_temp, new_watering);

        let transfer = dpi.send(false, dma_buf).map_err(|e| e.0).unwrap();
        (_, dpi, dma_buf) = transfer.wait();
        frame = frame.wrapping_add(1);
    }
}

/// Placeholder telemetry: slow triangle sweeps so the gauges visibly fill and
/// empty. `frame` advances once per displayed frame (~58 Hz).
fn telemetry(frame: u32) -> (u8, i16, bool) {
    let rh = triangle(frame, 464, 100) as u8; // 0..100 %RH, ~8 s sweep
    let temp = 5 + triangle(frame, 340, 30) as i16; // 5..35 °C, ~6 s sweep
    let watering = rh < 25; // "water when it's dry"
    (rh, temp, watering)
}

/// Triangle wave: ramps 0 -> `max` -> 0 over `period` steps of `phase`.
fn triangle(phase: u32, period: u32, max: u32) -> u32 {
    let half = period / 2;
    let p = phase % period;
    if p < half {
        max * p / half
    } else {
        max * (period - p) / half
    }
}

/// Bit-bang one 9-bit ST7701S SPI word: D/C bit first (0 = command, 1 = data),
/// then 8 payload bits MSB-first. SPI mode 0, data sampled on the rising edge.
fn spi9(sda: &mut Output<'_>, scl: &mut Output<'_>, byte: u8, is_data: bool) {
    let dc = is_data; // first bit: 0 for command, 1 for data
    write_bit(sda, scl, dc);
    for i in (0..8).rev() {
        write_bit(sda, scl, (byte >> i) & 1 != 0);
    }
}

#[inline(always)]
fn write_bit(sda: &mut Output<'_>, scl: &mut Output<'_>, bit: bool) {
    scl.set_low();
    sda.set_level(if bit { Level::High } else { Level::Low });
    tiny_delay();
    scl.set_high();
    tiny_delay();
}

#[inline(always)]
fn tiny_delay() {
    for _ in 0..40 {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// embedded-graphics draw target over the raw RGB565 framebuffer
// ---------------------------------------------------------------------------

struct FrameBuf<'a> {
    buf: &'a mut [u8],
}

impl FrameBuf<'_> {
    #[inline]
    fn put(&mut self, x: usize, y: usize, color: Rgb565) {
        if x >= H_RES || y >= V_RES {
            return;
        }
        let idx = (y * H_RES + x) * 2;
        let raw = RawU16::from(color).into_inner();
        // Little-endian, matching the DPI 2-byte word order.
        self.buf[idx] = (raw & 0xFF) as u8;
        self.buf[idx + 1] = (raw >> 8) as u8;
    }

    /// Signed-coordinate pixel plot; silently drops out-of-range points.
    #[inline]
    fn pset(&mut self, x: i32, y: i32, color: Rgb565) {
        if x >= 0 && y >= 0 {
            self.put(x as usize, y as usize, color);
        }
    }

    /// Fill a horizontal span [x0, x1] on row `y`.
    fn hspan(&mut self, y: i32, x0: i32, x1: i32, color: Rgb565) {
        let x0 = x0.max(0);
        let x1 = x1.min(H_RES as i32 - 1);
        for x in x0..=x1 {
            self.pset(x, y, color);
        }
    }

    fn vline(&mut self, x: i32, y0: i32, y1: i32, color: Rgb565) {
        for y in y0.max(0)..=y1.min(V_RES as i32 - 1) {
            self.pset(x, y, color);
        }
    }

    fn fill_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgb565) {
        for y in y0..=y1 {
            self.hspan(y, x0, x1, color);
        }
    }

    fn fill_disc(&mut self, cx: i32, cy: i32, r: i32, color: Rgb565) {
        for dy in -r..=r {
            let hw = isqrt(r * r - dy * dy);
            self.hspan(cy + dy, cx - hw, cx + hw, color);
        }
    }

    /// One-pixel circle outline.
    fn ring(&mut self, cx: i32, cy: i32, r: i32, color: Rgb565) {
        for dy in -r..=r {
            let hw = isqrt(r * r - dy * dy);
            self.pset(cx - hw, cy + dy, color);
            self.pset(cx + hw, cy + dy, color);
        }
        for dx in -r..=r {
            let hw = isqrt(r * r - dx * dx);
            self.pset(cx + dx, cy - hw, color);
            self.pset(cx + dx, cy + hw, color);
        }
    }

    /// Upper-semicircle outline (a dome cap), gap-free via both scan axes.
    fn arc_top(&mut self, cx: i32, cy: i32, r: i32, color: Rgb565) {
        for dx in -r..=r {
            let dy = isqrt(r * r - dx * dx);
            self.pset(cx + dx, cy - dy, color);
        }
        for dy in 0..=r {
            let dx = isqrt(r * r - dy * dy);
            self.pset(cx - dx, cy - dy, color);
            self.pset(cx + dx, cy - dy, color);
        }
    }
}

/// Integer square root (Newton's method); returns 0 for non-positive input.
fn isqrt(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

use embedded_graphics::pixelcolor::raw::{RawU16, RawData};

impl Dimensions for FrameBuf<'_> {
    fn bounding_box(&self) -> embedded_graphics::primitives::Rectangle {
        embedded_graphics::primitives::Rectangle::new(
            Point::zero(),
            Size::new(H_RES as u32, V_RES as u32),
        )
    }
}

impl DrawTarget for FrameBuf<'_> {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels {
            if coord.x >= 0 && coord.y >= 0 {
                self.put(coord.x as usize, coord.y as usize, color);
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Colour palette (RGB565: R 0..31, G 0..63, B 0..31)
// ---------------------------------------------------------------------------

const BG: Rgb565 = Rgb565::new(1, 4, 8);
const TEXT: Rgb565 = Rgb565::new(31, 63, 31);
const TEXT_DIM: Rgb565 = Rgb565::new(16, 34, 20);

const WATER: Rgb565 = Rgb565::new(7, 34, 31);
const WATER_EMPTY: Rgb565 = Rgb565::new(3, 9, 13);
const DROP_LINE: Rgb565 = Rgb565::new(15, 44, 31);

const MERCURY: Rgb565 = Rgb565::new(31, 10, 8);
const THERMO_EMPTY: Rgb565 = Rgb565::new(8, 18, 18);
const THERMO_LINE: Rgb565 = Rgb565::new(20, 40, 28);

const CLOUD_ON: Rgb565 = Rgb565::new(15, 42, 31);
const CLOUD_OFF: Rgb565 = Rgb565::new(13, 26, 13);
const RAIN: Rgb565 = Rgb565::new(9, 36, 31);

// Widget layout. Each gauge has a fixed centre and a "dirty rectangle" that
// bounds everything it draws (shape + value text). Only these rectangles are
// repainted per frame, so the per-frame PSRAM write traffic stays small enough
// that the display DMA never underruns (which would shear the image).
const DROP_CX: i32 = 150;
const DROP_BOX: (i32, i32, i32, i32) = (88, 90, 212, 274);
const THERMO_CX: i32 = 330;
const THERMO_BOX: (i32, i32, i32, i32) = (296, 100, 364, 274);
const CLOUD_CX: i32 = 240;
const CLOUD_CY: i32 = 360;
const CLOUD_BOX: (i32, i32, i32, i32) = (166, 300, 314, 454);

/// Paint the parts of the dashboard that never change: background and the
/// static labels. Drawn once into each framebuffer at start-up.
fn draw_static(fb: &mut FrameBuf) {
    fb.fill_rect(0, 0, H_RES as i32 - 1, V_RES as i32 - 1, BG);
    text_center(fb, "dew", 240, 46, &FONT_10X20, TEXT_DIM);
    text_center(fb, "HUMIDITY", DROP_CX, 284, &FONT_9X15, TEXT_DIM);
    text_center(fb, "TEMP", THERMO_CX, 284, &FONT_9X15, TEXT_DIM);
}

// Each widget clears its own dirty rectangle first, then draws its shape and
// value text. They are redrawn independently so we only touch the framebuffer
// region that actually changed.

/// Humidity: water-drop gauge (left) that fills to `rh` percent.
fn draw_humidity(fb: &mut FrameBuf, rh: u8) {
    fill_box(fb, DROP_BOX, BG);
    draw_drop(fb, DROP_CX, 180, 50, 84, rh as i32);
    text_center(fb, &format!("{rh}%"), DROP_CX, 258, &FONT_10X20, TEXT);
}

/// Temperature: thermometer (right); mercury maps 0..40 °C onto the tube.
fn draw_temperature(fb: &mut FrameBuf, temp_c: i16) {
    fill_box(fb, THERMO_BOX, BG);
    let t_fill = (temp_c.clamp(0, 40) as i32 * 100) / 40;
    draw_thermo(fb, THERMO_CX, 220, 25, 11, 116, t_fill);
    text_center(fb, &format!("{temp_c}\u{00b0}C"), THERMO_CX, 258, &FONT_10X20, TEXT);
}

/// Watering status: raincloud (bottom), blue and raining when active.
fn draw_watering(fb: &mut FrameBuf, watering: bool) {
    fill_box(fb, CLOUD_BOX, BG);
    draw_cloud(fb, CLOUD_CX, CLOUD_CY, watering);
    let (status, colour) = if watering {
        ("WATERING", CLOUD_ON)
    } else {
        ("IDLE", CLOUD_OFF)
    };
    text_center(fb, status, CLOUD_CX, 438, &FONT_10X20, colour);
}

fn fill_box(fb: &mut FrameBuf, (x0, y0, x1, y1): (i32, i32, i32, i32), colour: Rgb565) {
    fb.fill_rect(x0, y0, x1, y1, colour);
}

/// A teardrop that fills from the bottom to `fill_pct` (0..100).
///
/// The shape is a circle of radius `r` centred at `(cx, cy)` for its rounded
/// bottom, tapering linearly to a point `taper_h` pixels above the centre.
fn draw_drop(fb: &mut FrameBuf, cx: i32, cy: i32, r: i32, taper_h: i32, fill_pct: i32) {
    let top = cy - taper_h;
    let bottom = cy + r;
    let total = taper_h + r;
    let fill = fill_pct.clamp(0, 100);
    let y_fill = bottom - (fill * total) / 100;

    for y in top..=bottom {
        let dy = y - cy;
        let half = if dy >= 0 {
            isqrt(r * r - dy * dy) // rounded bottom
        } else {
            r * (y - top) / taper_h // linear taper to the point
        };
        let colour = if y >= y_fill { WATER } else { WATER_EMPTY };
        fb.hspan(y, cx - half, cx + half, colour);
        fb.pset(cx - half, y, DROP_LINE);
        fb.pset(cx + half, y, DROP_LINE);
    }
}

/// A thermometer whose mercury column fills to `fill_pct` (0..100). Bulb of
/// radius `bulb_r` at `(cx, bulb_y)`; tube of half-width `tube_half` rising to
/// `tube_top`.
fn draw_thermo(
    fb: &mut FrameBuf,
    cx: i32,
    bulb_y: i32,
    bulb_r: i32,
    tube_half: i32,
    tube_top: i32,
    fill_pct: i32,
) {
    let fill = fill_pct.clamp(0, 100);
    let y_merc = bulb_y - (fill * (bulb_y - tube_top)) / 100;

    // Empty (grey) tube first, with a rounded cap.
    fb.fill_rect(cx - tube_half, tube_top, cx + tube_half, bulb_y, THERMO_EMPTY);
    fb.fill_disc(cx, tube_top, tube_half, THERMO_EMPTY);

    // Mercury column + always-full bulb.
    fb.fill_rect(cx - tube_half, y_merc, cx + tube_half, bulb_y, MERCURY);
    if y_merc <= tube_top {
        fb.fill_disc(cx, tube_top, tube_half, MERCURY);
    }
    fb.fill_disc(cx, bulb_y, bulb_r, MERCURY);

    // Outline. The tube walls stop where they meet the bulb's edge (not its
    // centre), and the tube's top is a dome (arc), not a full circle.
    let neck_y = bulb_y - isqrt(bulb_r * bulb_r - tube_half * tube_half);
    fb.vline(cx - tube_half, tube_top, neck_y, THERMO_LINE);
    fb.vline(cx + tube_half, tube_top, neck_y, THERMO_LINE);
    fb.ring(cx, bulb_y, bulb_r, THERMO_LINE);
    fb.arc_top(cx, tube_top, tube_half, THERMO_LINE);
}

/// A cloud from overlapping discs; blue and raining when `on`, grey when off.
fn draw_cloud(fb: &mut FrameBuf, cx: i32, cy: i32, on: bool) {
    let body = if on { CLOUD_ON } else { CLOUD_OFF };
    fb.fill_disc(cx - 38, cy, 28, body);
    fb.fill_disc(cx + 40, cy, 30, body);
    fb.fill_disc(cx - 6, cy - 22, 38, body);
    fb.fill_disc(cx + 22, cy - 10, 28, body);
    fb.fill_rect(cx - 56, cy, cx + 62, cy + 20, body);

    if on {
        for i in 0..5 {
            let rx = cx - 44 + i * 22;
            fb.fill_rect(rx, cy + 32, rx + 3, cy + 52, RAIN);
        }
    }
}

/// Draw horizontally- and vertically-centred text at `(x, y)`.
fn text_center(fb: &mut FrameBuf, s: &str, x: i32, y: i32, font: &MonoFont<'_>, colour: Rgb565) {
    let char_style = MonoTextStyle::new(font, colour);
    let text_style = TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Middle)
        .build();
    Text::with_text_style(s, Point::new(x, y), char_style, text_style)
        .draw(fb)
        .unwrap();
}

// ---------------------------------------------------------------------------
// ST7701S initialisation (Waveshare ESP32-S3-Touch-LCD-2.1, verbatim values)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Init {
    Cmd(u8),
    Data(u8),
    Delay(u16),
}

use Init::{Cmd, Data, Delay as D};

#[rustfmt::skip]
const ST7701_INIT: &[Init] = &[
    // Command2 BK0
    Cmd(0xFF), Data(0x77), Data(0x01), Data(0x00), Data(0x00), Data(0x10),
    Cmd(0xC0), Data(0x3B), Data(0x00),
    Cmd(0xC1), Data(0x0B), Data(0x02),
    Cmd(0xC2), Data(0x07), Data(0x02),
    Cmd(0xCC), Data(0x10),
    Cmd(0xCD), Data(0x08),
    // Positive gamma
    Cmd(0xB0), Data(0x00), Data(0x11), Data(0x16), Data(0x0E), Data(0x11), Data(0x06),
               Data(0x05), Data(0x09), Data(0x08), Data(0x21), Data(0x06), Data(0x13),
               Data(0x10), Data(0x29), Data(0x31), Data(0x18),
    // Negative gamma
    Cmd(0xB1), Data(0x00), Data(0x11), Data(0x16), Data(0x0E), Data(0x11), Data(0x07),
               Data(0x05), Data(0x09), Data(0x09), Data(0x21), Data(0x05), Data(0x13),
               Data(0x11), Data(0x2A), Data(0x31), Data(0x18),
    // Command2 BK1
    Cmd(0xFF), Data(0x77), Data(0x01), Data(0x00), Data(0x00), Data(0x11),
    Cmd(0xB0), Data(0x6D),
    Cmd(0xB1), Data(0x37),
    Cmd(0xB2), Data(0x81),
    Cmd(0xB3), Data(0x80),
    Cmd(0xB5), Data(0x43),
    Cmd(0xB7), Data(0x85),
    Cmd(0xB8), Data(0x20),
    Cmd(0xC1), Data(0x78),
    Cmd(0xC2), Data(0x78),
    Cmd(0xD0), Data(0x88),
    Cmd(0xE0), Data(0x00), Data(0x00), Data(0x02),
    Cmd(0xE1), Data(0x03), Data(0xA0), Data(0x00), Data(0x00), Data(0x04), Data(0xA0),
               Data(0x00), Data(0x00), Data(0x00), Data(0x20), Data(0x20),
    Cmd(0xE2), Data(0x00), Data(0x00), Data(0x00), Data(0x00), Data(0x00), Data(0x00),
               Data(0x00), Data(0x00), Data(0x00), Data(0x00), Data(0x00), Data(0x00),
               Data(0x00),
    Cmd(0xE3), Data(0x00), Data(0x00), Data(0x11), Data(0x00),
    Cmd(0xE4), Data(0x22), Data(0x00),
    Cmd(0xE5), Data(0x05), Data(0xEC), Data(0xA0), Data(0xA0), Data(0x07), Data(0xEE),
               Data(0xA0), Data(0xA0), Data(0x00), Data(0x00), Data(0x00), Data(0x00),
               Data(0x00), Data(0x00), Data(0x00), Data(0x00),
    Cmd(0xE6), Data(0x00), Data(0x00), Data(0x11), Data(0x00),
    Cmd(0xE7), Data(0x22), Data(0x00),
    Cmd(0xE8), Data(0x06), Data(0xED), Data(0xA0), Data(0xA0), Data(0x08), Data(0xEF),
               Data(0xA0), Data(0xA0), Data(0x00), Data(0x00), Data(0x00), Data(0x00),
               Data(0x00), Data(0x00), Data(0x00), Data(0x00),
    Cmd(0xEB), Data(0x00), Data(0x00), Data(0x40), Data(0x40), Data(0x00), Data(0x00),
               Data(0x00),
    Cmd(0xED), Data(0xFF), Data(0xFF), Data(0xFF), Data(0xBA), Data(0x0A), Data(0xBF),
               Data(0x45), Data(0xFF), Data(0xFF), Data(0x54), Data(0xFB), Data(0xA0),
               Data(0xAB), Data(0xFF), Data(0xFF), Data(0xFF),
    Cmd(0xEF), Data(0x10), Data(0x0D), Data(0x04), Data(0x08), Data(0x3F), Data(0x1F),
    // Command2 BK3
    Cmd(0xFF), Data(0x77), Data(0x01), Data(0x00), Data(0x00), Data(0x13),
    Cmd(0xEF), Data(0x08),
    // Back to user commands (BK0 off)
    Cmd(0xFF), Data(0x77), Data(0x01), Data(0x00), Data(0x00), Data(0x00),
    Cmd(0x36), Data(0x00),
    Cmd(0x3A), Data(0x66),
    Cmd(0x11),            // Sleep out
    D(480),
    Cmd(0x20),            // Display inversion off
    D(120),
    Cmd(0x29),            // Display on
    D(20),
];
