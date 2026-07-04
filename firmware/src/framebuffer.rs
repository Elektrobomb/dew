//! RGB565 framebuffer: an `embedded-graphics` [`DrawTarget`] plus the
//! integer-only fill/outline primitives the dashboard gauges are drawn with.
//!
//! The gauges need per-row clipping (a fill-to-a-level shape) that the stock
//! embedded-graphics primitives don't provide, so the fills/outlines here are
//! hand-rolled on top of [`isqrt`].

use embedded_graphics::{
    mono_font::{MonoFont, MonoTextStyle},
    pixelcolor::{
        raw::{RawData, RawU16},
        Rgb565,
    },
    prelude::*,
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};

use crate::board::{H_RES, V_RES};

/// A mutable view over the raw little-endian RGB565 framebuffer bytes.
pub struct FrameBuf<'a> {
    buf: &'a mut [u8],
}

impl<'a> FrameBuf<'a> {
    /// Wrap the raw framebuffer byte slice (`H_RES * V_RES * 2` bytes).
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf }
    }

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
    pub fn pset(&mut self, x: i32, y: i32, color: Rgb565) {
        if x >= 0 && y >= 0 {
            self.put(x as usize, y as usize, color);
        }
    }

    /// Fill a horizontal span [x0, x1] on row `y`.
    pub fn hspan(&mut self, y: i32, x0: i32, x1: i32, color: Rgb565) {
        let x0 = x0.max(0);
        let x1 = x1.min(H_RES as i32 - 1);
        for x in x0..=x1 {
            self.pset(x, y, color);
        }
    }

    pub fn vline(&mut self, x: i32, y0: i32, y1: i32, color: Rgb565) {
        for y in y0.max(0)..=y1.min(V_RES as i32 - 1) {
            self.pset(x, y, color);
        }
    }

    pub fn fill_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgb565) {
        for y in y0..=y1 {
            self.hspan(y, x0, x1, color);
        }
    }

    pub fn fill_disc(&mut self, cx: i32, cy: i32, r: i32, color: Rgb565) {
        for dy in -r..=r {
            let hw = isqrt(r * r - dy * dy);
            self.hspan(cy + dy, cx - hw, cx + hw, color);
        }
    }

    /// One-pixel circle outline.
    pub fn ring(&mut self, cx: i32, cy: i32, r: i32, color: Rgb565) {
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
    pub fn arc_top(&mut self, cx: i32, cy: i32, r: i32, color: Rgb565) {
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
pub fn isqrt(n: i32) -> i32 {
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

/// Draw horizontally- and vertically-centred text at `(x, y)`.
pub fn text_center(fb: &mut FrameBuf, s: &str, x: i32, y: i32, font: &MonoFont<'_>, colour: Rgb565) {
    let char_style = MonoTextStyle::new(font, colour);
    let text_style = TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Middle)
        .build();
    Text::with_text_style(s, Point::new(x, y), char_style, text_style)
        .draw(fb)
        .unwrap();
}

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
