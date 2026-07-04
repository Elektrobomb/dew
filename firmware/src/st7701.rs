//! ST7701S panel initialisation.
//!
//! The panel's configuration registers are programmed over a 3-wire SPI that we
//! bit-bang on GPIO1 (SDA) / GPIO2 (SCL). Each word is 9 bits — a D/C flag then
//! 8 payload bits, MSB-first, SPI mode 0 (data sampled on the rising edge).
//! Chip-select and reset live on the TCA9554 I2C expander and are driven by the
//! caller; [`send_init`] only owns the SPI clocking.

use esp_hal::{
    delay::Delay,
    gpio::{Level, Output},
};

/// One entry of the ST7701S init sequence.
#[derive(Clone, Copy)]
pub enum Init {
    Cmd(u8),
    Data(u8),
    Delay(u16),
}

use Init::{Cmd, Data, Delay as D};

/// Play the whole [`ST7701_INIT`] table out over the bit-banged SPI. The caller
/// must hold chip-select (TCA9554 bit2) low across the call.
pub fn send_init(sda: &mut Output<'_>, scl: &mut Output<'_>, delay: &Delay) {
    for cmd in ST7701_INIT {
        match *cmd {
            Init::Cmd(c) => spi9(sda, scl, c, false),
            Init::Data(d) => spi9(sda, scl, d, true),
            Init::Delay(ms) => delay.delay_millis(ms as u32),
        }
    }
}

/// Bit-bang one 9-bit ST7701S SPI word: D/C bit first (0 = command, 1 = data),
/// then 8 payload bits MSB-first.
fn spi9(sda: &mut Output<'_>, scl: &mut Output<'_>, byte: u8, is_data: bool) {
    write_bit(sda, scl, is_data); // first bit: 0 for command, 1 for data
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
// ST7701S initialisation table (Waveshare ESP32-S3-Touch-LCD-2.1, verbatim)
// ---------------------------------------------------------------------------

#[rustfmt::skip]
pub const ST7701_INIT: &[Init] = &[
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
