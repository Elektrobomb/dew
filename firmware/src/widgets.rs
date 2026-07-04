//! Plant-care dashboard widgets: a humidity water-drop, a temperature
//! thermometer, and a watering raincloud, over a static labelled background.
//!
//! Each widget clears its own dirty rectangle and repaints, so only the region
//! that actually changed is written to the framebuffer per frame. This keeps the
//! per-frame PSRAM write traffic small enough that the display DMA never
//! underruns (which would shear the image). Don't reintroduce full-screen
//! redraws — see the PSRAM/DMA constraint in CLAUDE.md.

use alloc::format;

use embedded_graphics::mono_font::iso_8859_1::{FONT_10X20, FONT_9X15};
use embedded_graphics::pixelcolor::Rgb565;

use crate::board::{H_RES, V_RES};
use crate::framebuffer::{isqrt, text_center, FrameBuf};
use crate::palette::*;

// Widget layout. Each gauge has a fixed centre and a "dirty rectangle" that
// bounds everything it draws (shape + value text). Only these rectangles are
// repainted per frame.
const DROP_CX: i32 = 150;
const DROP_BOX: (i32, i32, i32, i32) = (88, 90, 212, 274);
const THERMO_CX: i32 = 330;
const THERMO_BOX: (i32, i32, i32, i32) = (296, 100, 364, 274);
const CLOUD_CX: i32 = 240;
const CLOUD_CY: i32 = 360;
const CLOUD_BOX: (i32, i32, i32, i32) = (166, 300, 314, 454);

/// Paint the parts of the dashboard that never change: background and the
/// static labels. Drawn once into the framebuffer at start-up.
pub fn draw_static(fb: &mut FrameBuf) {
    fb.fill_rect(0, 0, H_RES as i32 - 1, V_RES as i32 - 1, BG);
    text_center(fb, "dew", 240, 46, &FONT_10X20, TEXT_DIM);
    text_center(fb, "HUMIDITY", DROP_CX, 284, &FONT_9X15, TEXT_DIM);
    text_center(fb, "TEMP", THERMO_CX, 284, &FONT_9X15, TEXT_DIM);
}

// Each widget clears its own dirty rectangle first, then draws its shape and
// value text. They are redrawn independently so we only touch the framebuffer
// region that actually changed.

/// Humidity: water-drop gauge (left) that fills to `rh` percent.
pub fn draw_humidity(fb: &mut FrameBuf, rh: u8) {
    fill_box(fb, DROP_BOX, BG);
    draw_drop(fb, DROP_CX, 180, 50, 84, rh as i32);
    text_center(fb, &format!("{rh}%"), DROP_CX, 258, &FONT_10X20, TEXT);
}

/// Temperature: thermometer (right); mercury maps 0..40 °C onto the tube.
pub fn draw_temperature(fb: &mut FrameBuf, temp_c: i16) {
    fill_box(fb, THERMO_BOX, BG);
    let t_fill = (temp_c.clamp(0, 40) as i32 * 100) / 40;
    draw_thermo(fb, THERMO_CX, 220, 25, 11, 116, t_fill);
    text_center(fb, &format!("{temp_c}\u{00b0}C"), THERMO_CX, 258, &FONT_10X20, TEXT);
}

/// Watering status: raincloud (bottom), blue and raining when active.
pub fn draw_watering(fb: &mut FrameBuf, watering: bool) {
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
