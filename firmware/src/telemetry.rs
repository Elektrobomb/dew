//! Telemetry source for the dashboard.
//!
//! This is the seam where real sensor reads (soil moisture, humidity,
//! temperature) will replace the synthetic sweeps below. Keep the return shape
//! `(humidity %RH, temperature °C, watering active)` so the render loop and
//! widgets stay unchanged when the placeholder is swapped out.

/// Placeholder telemetry: slow triangle sweeps so the gauges visibly fill and
/// empty. `frame` advances once per displayed frame (~58 Hz).
pub fn telemetry(frame: u32) -> (u8, i16, bool) {
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
