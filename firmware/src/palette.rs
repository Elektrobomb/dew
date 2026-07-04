//! Dashboard colour palette (RGB565: R 0..31, G 0..63, B 0..31).

use embedded_graphics::pixelcolor::Rgb565;

pub const BG: Rgb565 = Rgb565::new(1, 4, 8);
pub const TEXT: Rgb565 = Rgb565::new(31, 63, 31);
pub const TEXT_DIM: Rgb565 = Rgb565::new(16, 34, 20);

pub const WATER: Rgb565 = Rgb565::new(7, 34, 31);
pub const WATER_EMPTY: Rgb565 = Rgb565::new(3, 9, 13);
pub const DROP_LINE: Rgb565 = Rgb565::new(15, 44, 31);

pub const MERCURY: Rgb565 = Rgb565::new(31, 10, 8);
pub const THERMO_EMPTY: Rgb565 = Rgb565::new(8, 18, 18);
pub const THERMO_LINE: Rgb565 = Rgb565::new(20, 40, 28);

pub const CLOUD_ON: Rgb565 = Rgb565::new(15, 42, 31);
pub const CLOUD_OFF: Rgb565 = Rgb565::new(13, 26, 13);
pub const RAIN: Rgb565 = Rgb565::new(9, 36, 31);
