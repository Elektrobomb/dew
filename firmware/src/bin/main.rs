#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

//! dew firmware — ST7701S 480x480 RGB display bring-up.
//!
//! Draws a large smiley face on the Waveshare ESP32-S3-Touch-LCD-2.1.
//!
//! The panel is an ST7701S driven over a 16-bit RGB565 parallel bus (esp-hal
//! LCD_CAM DPI peripheral), streamed continuously from a framebuffer in PSRAM
//! via DMA. The panel's configuration registers are programmed over a 3-wire
//! 9-bit SPI that we bit-bang on GPIO1 (SDA) / GPIO2 (SCL); chip-select and
//! reset live on a TCA9554 I2C GPIO expander (addr 0x20, SDA=GPIO15/SCL=GPIO7).

extern crate alloc;

use embassy_executor::Spawner;
use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Arc, Circle, PrimitiveStyle, PrimitiveStyleBuilder},
    text::{Alignment, Text},
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

    // --- Allocate framebuffer in PSRAM and paint the smiley ---
    let fb: &'static mut [u8] = dma_alloc_buffer!(FB_SIZE, DMA_ALIGNMENT as usize);
    info!("dew: framebuffer @ {:p} ({} bytes)", fb.as_ptr(), fb.len());

    {
        let mut canvas = FrameBuf { buf: &mut fb[..] };
        draw_smiley(&mut canvas);
    }

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

    // --- Build the DMA framebuffer and stream it forever ---
    let (_, tx_descriptors) = esp_hal::dma_descriptors_chunk_size!(0, FB_SIZE, DMA_CHUNK_SIZE);
    let mut dma_buf = DmaTxBuf::new_with_config(tx_descriptors, fb, DMA_ALIGNMENT).unwrap();

    info!("dew: streaming framebuffer to panel");
    loop {
        let transfer = dpi.send(false, dma_buf).map_err(|e| e.0).unwrap();
        (_, dpi, dma_buf) = transfer.wait();
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

fn draw_smiley<D>(target: &mut D)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    let bg = Rgb565::new(6, 12, 25); // deep blue
    let face = Rgb565::new(31, 50, 0); // bright yellow
    let feature = Rgb565::new(3, 6, 10); // near-black navy

    let center = Point::new(H_RES as i32 / 2, V_RES as i32 / 2);

    // Background
    target.clear(bg).unwrap();

    // Face — big yellow disc
    let face_d = 400u32;
    Circle::with_center(center, face_d)
        .into_styled(PrimitiveStyle::with_fill(face))
        .draw(target)
        .unwrap();

    // Eyes
    let eye_d = 46u32;
    let eye_dx = 78;
    let eye_y = center.y - 40;
    let eye_style = PrimitiveStyle::with_fill(feature);
    Circle::with_center(Point::new(center.x - eye_dx, eye_y), eye_d)
        .into_styled(eye_style)
        .draw(target)
        .unwrap();
    Circle::with_center(Point::new(center.x + eye_dx, eye_y), eye_d)
        .into_styled(eye_style)
        .draw(target)
        .unwrap();

    // Smile — a thick open arc (no radial edges, so it's a curve not a pie slice).
    let mouth_style = PrimitiveStyleBuilder::new()
        .stroke_color(feature)
        .stroke_width(26)
        .build();
    // Arc angles: 0deg = 3 o'clock, sweeping clockwise in embedded-graphics.
    Arc::with_center(
        Point::new(center.x, center.y - 10),
        250,
        Angle::from_degrees(30.0),
        Angle::from_degrees(120.0),
    )
    .into_styled(mouth_style)
    .draw(target)
    .unwrap();

    // Little label so we know it's ours.
    let text_style = MonoTextStyle::new(&FONT_10X20, feature);
    Text::with_alignment(
        "dew :)",
        Point::new(center.x, center.y + 150),
        text_style,
        Alignment::Center,
    )
    .draw(target)
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
