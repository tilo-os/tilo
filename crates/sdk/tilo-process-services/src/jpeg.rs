//! JPEG encoding of 8-bit frames and decoded images.
//!
//! Apps keep photographs, camera frames and re-encoded pastes as JPEG. They
//! encode through these functions rather than the `image` crate's generic
//! encoders, so the encoder is compiled once, in the shared runtime, and not
//! again into each app's binary.

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ExtendedColorType, ImageEncoder, ImageResult};

use crate::png::check_len;

/// Encode `rgb` (`width * height * 3` bytes, row-major) as a JPEG of
/// `quality` (1-100).
pub fn encode_rgb8(width: u32, height: u32, rgb: &[u8], quality: u8) -> ImageResult<Vec<u8>> {
    check_len(width, height, 3, rgb)?;
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, quality).write_image(
        rgb,
        width,
        height,
        ExtendedColorType::Rgb8,
    )?;
    Ok(bytes)
}

/// Encode `rgba` (`width * height * 4` bytes) as a JPEG of `quality`,
/// dropping its alpha channel: JPEG keeps none. Each pixel keeps its colour
/// values as they are, uncomposited.
pub fn encode_rgba8_opaque(
    width: u32,
    height: u32,
    rgba: &[u8],
    quality: u8,
) -> ImageResult<Vec<u8>> {
    check_len(width, height, 4, rgba)?;
    let rgb: Vec<u8> = rgba
        .chunks_exact(4)
        .flat_map(|pixel| pixel.iter().take(3).copied())
        .collect();
    encode_rgb8(width, height, &rgb, quality)
}

/// Encode `image` as a JPEG of `quality`: grey images stay grey, every other
/// colour type becomes 8-bit RGB (alpha dropped, deeper channels narrowed),
/// as the encoder requires.
pub fn encode_image(image: &DynamicImage, quality: u8) -> ImageResult<Vec<u8>> {
    let mut bytes = Vec::new();
    image.write_with_encoder(JpegEncoder::new_with_quality(&mut bytes, quality))?;
    Ok(bytes)
}
