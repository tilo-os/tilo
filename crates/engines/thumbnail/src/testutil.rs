//! Shared test fixtures (compiled only under `#[cfg(test)]`).

use std::io::Cursor;

pub(crate) fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut buf = image::RgbaImage::new(w, h);
    for px in buf.pixels_mut() {
        *px = image::Rgba([rgb[0], rgb[1], rgb[2], 255]);
    }
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(buf)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}
