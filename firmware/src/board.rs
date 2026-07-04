//! Board and framebuffer constants for the Waveshare ESP32-S3-Touch-LCD-2.1.

use esp_hal::dma::ExternalBurstConfig;

/// Panel resolution (square 480×480 round IPS).
pub const H_RES: usize = 480;
pub const V_RES: usize = 480;
/// RGB565 framebuffer size in bytes (2 bytes/pixel = 460_800).
pub const FB_SIZE: usize = H_RES * V_RES * 2;

// DMA over PSRAM needs 64-byte external burst alignment; keep chunks under 4092.
pub const DMA_ALIGNMENT: ExternalBurstConfig = ExternalBurstConfig::Size64;
pub const DMA_CHUNK_SIZE: usize = 4096 - DMA_ALIGNMENT as usize;

// TCA9554 expander bit assignments (Waveshare 2.1 board).
pub const EXIO_LCD_RST: u8 = 0; // bit0
pub const EXIO_TP_RST: u8 = 1; // bit1
pub const EXIO_LCD_CS: u8 = 2; // bit2
pub const EXIO_BUZZER: u8 = 7; // bit7 (EXIO_PIN8) — active high, keep LOW

/// Allocate the RGB565 framebuffer from the current (PSRAM) heap, aligned for
/// the DPI peripheral's external-burst DMA.
///
/// The framebuffer lives for the whole program, so the allocation is
/// intentionally leaked and handed back as a `'static` slice.
pub fn alloc_framebuffer() -> &'static mut [u8] {
    let layout = core::alloc::Layout::from_size_align(FB_SIZE, DMA_ALIGNMENT as usize).unwrap();
    unsafe {
        let ptr = alloc::alloc::alloc(layout);
        if ptr.is_null() {
            alloc::alloc::handle_alloc_error(layout);
        }
        core::slice::from_raw_parts_mut(ptr, FB_SIZE)
    }
}
