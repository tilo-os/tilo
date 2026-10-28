//! Lossless PNG encoding and decoding of tightly packed 8-bit RGBA frames.
//!
//! Screenshots, clipboard images, image widgets and test fixtures all move
//! plain RGBA through PNG. These helpers keep that one path on the `image`
//! codec the toolkit already carries for its other formats.

use std::path::Path;

use image::codecs::png::{PngDecoder, PngEncoder};
use image::{
    DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageFormat, ImageResult,
};

/// Encode `rgba` (`width * height * 4` bytes, row-major) as a PNG.
pub fn encode_rgba8(width: u32, height: u32, rgba: &[u8]) -> ImageResult<Vec<u8>> {
    check_rgba8_len(width, height, rgba)?;
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes).write_image(rgba, width, height, ExtendedColorType::Rgba8)?;
    Ok(bytes)
}

/// Encode `image` as a PNG in its own colour type (grey, RGB, with or
/// without alpha, 8 or 16 bits).
///
/// Apps encode through this rather than `DynamicImage::write_to`: that is
/// generic over the writer and dispatches over every format the `image`
/// crate was built with, so each app calling it compiled every encoder into
/// its own binary. This one is compiled once, in the shared runtime.
pub fn encode_image(image: &DynamicImage) -> ImageResult<Vec<u8>> {
    let mut bytes = Vec::new();
    image.write_with_encoder(PngEncoder::new(&mut bytes))?;
    Ok(bytes)
}

/// Encode `rgba` as a PNG file at `path`, replacing any existing file.
///
/// # Errors
///
/// Fails when the file cannot be written or the buffer does not match the
/// dimensions.
pub fn write_rgba8(
    path: impl AsRef<Path>,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> ImageResult<()> {
    check_rgba8_len(width, height, rgba)?;
    image::save_buffer_with_format(
        path,
        rgba,
        width,
        height,
        ExtendedColorType::Rgba8,
        ImageFormat::Png,
    )
}

/// The encoder treats a mismatched buffer as a programming error and
/// panics; callers pass buffers from renderers and the clipboard, so report
/// it as an error instead.
fn check_rgba8_len(width: u32, height: u32, rgba: &[u8]) -> ImageResult<()> {
    check_len(width, height, 4, rgba)
}

/// That `pixels` holds exactly `width * height` pixels of `channels` bytes.
pub(crate) fn check_len(width: u32, height: u32, channels: u64, pixels: &[u8]) -> ImageResult<()> {
    let expected = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(channels);
    if u64::try_from(pixels.len()).ok() == Some(expected) {
        Ok(())
    } else {
        Err(image::ImageError::Parameter(
            image::error::ParameterError::from_kind(
                image::error::ParameterErrorKind::DimensionMismatch,
            ),
        ))
    }
}

/// A decoded PNG as 8-bit RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba8 {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major.
    pub rgba: Vec<u8>,
}

/// Decode a PNG of any colour type and bit depth into 8-bit RGBA.
pub fn decode_rgba8(bytes: &[u8]) -> ImageResult<Rgba8> {
    decode_rgba8_within(bytes, u64::MAX)
}

/// Like [`decode_rgba8`], but rejects images with more than `max_pixels`
/// pixels before decoding any image data.
pub fn decode_rgba8_within(bytes: &[u8], max_pixels: u64) -> ImageResult<Rgba8> {
    let decoder = PngDecoder::new(std::io::Cursor::new(bytes))?;
    let (width, height) = decoder.dimensions();
    if u64::from(width).saturating_mul(u64::from(height)) > max_pixels {
        return Err(image::ImageError::Limits(
            image::error::LimitError::from_kind(image::error::LimitErrorKind::DimensionError),
        ));
    }
    let rgba = DynamicImage::from_decoder(decoder)?.into_rgba8();
    Ok(Rgba8 {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

/// Read and decode the PNG file at `path` into 8-bit RGBA.
///
/// # Errors
///
/// Fails when the file cannot be read or is not a complete PNG.
pub fn read_rgba8(path: impl AsRef<Path>) -> ImageResult<Rgba8> {
    decode_rgba8(&std::fs::read(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_codec_round_trips_rgba_adds_alpha_and_rejects_bad_buffers() {
        {
            let rgba = [0xff, 0x00, 0x80, 0xff, 0x10, 0x20, 0x30, 0x40];
            let png = encode_rgba8(2, 1, &rgba).expect("valid RGBA image");
            assert_eq!(
                decode_rgba8(&png).expect("complete PNG"),
                Rgba8 {
                    width: 2,
                    height: 1,
                    rgba: rgba.to_vec(),
                }
            );
        }

        {
            assert!(encode_rgba8(2, 1, &[0; 4]).is_err());
            let png = encode_rgba8(2, 2, &[0; 16]).expect("valid RGBA image");
            assert!(decode_rgba8_within(&png, 3).is_err());
            assert!(decode_rgba8_within(&png, 4).is_ok());
            assert!(decode_rgba8(b"not a png").is_err());
        }

        {
            let grey = DynamicImage::ImageLuma8(
                image::GrayImage::from_raw(2, 1, vec![7, 200]).expect("2x1 grey pixels"),
            );
            let png = encode_image(&grey).expect("valid grey image");
            let decoded = image::load_from_memory(&png).expect("complete PNG");
            assert_eq!(decoded.color(), image::ColorType::L8);
            assert_eq!(decoded.to_luma8().into_raw(), [7, 200]);
            assert_eq!(
                decode_rgba8(&png).expect("complete PNG").rgba,
                [7, 7, 7, 255, 200, 200, 200, 255]
            );
        }

        {
            let mut png = Vec::new();
            PngEncoder::new(&mut png)
                .write_image(&[12, 34, 56], 1, 1, ExtendedColorType::Rgb8)
                .expect("valid RGB image");
            assert_eq!(
                decode_rgba8(&png).expect("complete PNG").rgba,
                [12, 34, 56, 255]
            );
        }
    }
}
