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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_codec_encodes_frames_and_images_and_rejects_bad_buffers() {
        {
            let rgb = [200u8, 30, 30].repeat(16 * 8);
            let jpeg = encode_rgb8(16, 8, &rgb, 90).expect("valid RGB frame");
            assert_eq!(&jpeg[..2], &[0xff, 0xd8], "a JPEG starts with SOI");
            let decoded = image::load_from_memory(&jpeg).expect("complete JPEG");
            assert_eq!((decoded.width(), decoded.height()), (16, 8));
            let centre = decoded.to_rgb8().get_pixel(8, 4).0;
            assert!(centre[0] > 150 && centre[1] < 80, "still red: {centre:?}");
        }

        {
            let rgba = [10u8, 220, 10, 0].repeat(8 * 8);
            let jpeg = encode_rgba8_opaque(8, 8, &rgba, 90).expect("valid RGBA frame");
            let centre = image::load_from_memory(&jpeg)
                .expect("complete JPEG")
                .to_rgb8()
                .get_pixel(4, 4)
                .0;
            assert!(centre[1] > 150 && centre[0] < 80, "still green: {centre:?}");
        }

        {
            let rgba = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                4,
                4,
                image::Rgba([1, 2, 3, 4]),
            ));
            let grey =
                DynamicImage::ImageLuma8(image::GrayImage::from_pixel(4, 4, image::Luma([90])));
            assert!(encode_image(&rgba, 85).is_ok());
            let grey = encode_image(&grey, 85).expect("grey image");
            assert_eq!(
                image::load_from_memory(&grey)
                    .expect("complete JPEG")
                    .color(),
                image::ColorType::L8
            );
        }

        {
            assert!(encode_rgb8(2, 2, &[0; 11], 90).is_err());
            assert!(encode_rgba8_opaque(2, 2, &[0; 12], 90).is_err());
        }
    }
}
