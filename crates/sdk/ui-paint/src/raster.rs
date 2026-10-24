//! Images built from raw RGBA8 pixels.

use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

/// `width`×`height` RGBA8 pixels with straight (unpremultiplied) alpha.
pub fn rgba8(pixels: impl Into<Blob<u8>>, width: u32, height: u32) -> ImageData {
    image(pixels.into(), width, height, ImageAlphaType::Alpha)
}

/// `width`×`height` RGBA8 pixels whose colour is already multiplied by alpha.
pub fn rgba8_premultiplied(pixels: impl Into<Blob<u8>>, width: u32, height: u32) -> ImageData {
    image(
        pixels.into(),
        width,
        height,
        ImageAlphaType::AlphaPremultiplied,
    )
}

fn image(data: Blob<u8>, width: u32, height: u32, alpha_type: ImageAlphaType) -> ImageData {
    ImageData {
        data,
        format: ImageFormat::Rgba8,
        alpha_type,
        width,
        height,
    }
}
