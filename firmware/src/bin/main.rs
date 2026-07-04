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
//!
//! This binary owns the boot sequence and the render loop; the display driver,
//! framebuffer graphics, and dashboard widgets live in the `dew_firmware`
//! library modules.

extern crate alloc;

use dew_firmware::{
    board::{
        self, DMA_ALIGNMENT, DMA_CHUNK_SIZE, EXIO_BUZZER, EXIO_LCD_CS, EXIO_LCD_RST, EXIO_TP_RST,
        FB_SIZE, H_RES, V_RES,
    },
    framebuffer::FrameBuf,
    st7701,
    telemetry::telemetry,
    widgets::{draw_humidity, draw_static, draw_temperature, draw_watering},
};
use embassy_executor::Spawner;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    dma::DmaTxBuf,
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

    st7701::send_init(&mut sda, &mut scl, &delay);

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
    let fb = board::alloc_framebuffer();
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
        let mut canvas = FrameBuf::new(dma_buf.as_mut_slice());
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
            let mut canvas = FrameBuf::new(dma_buf.as_mut_slice());
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
