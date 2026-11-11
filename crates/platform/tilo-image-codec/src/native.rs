//! The codec's native backend: the `image` crate.

use image::{ExtendedColorType, ImageEncoder};

use crate::codec::{EncodeError, Encoding};
use crate::{DecodeError, DecodedImage};

#[allow(
    clippy::unused_async,
    reason = "the browser's backend awaits; callers see one signature"
)]
pub(crate) async fn decode(
    bytes: &[u8],
    max_dimension: Option<u32>,
) -> Result<DecodedImage, DecodeError> {
    let rgba = image::load_from_memory(bytes)
        .map_err(|_| DecodeError::InvalidImage)?
        .to_rgba8();
    let (width, height) = rgba.dimensions();
    let decoded = DecodedImage {
        rgba: rgba.into_raw(),
        width,
        height,
    };
    Ok(match max_dimension {
        Some(max) => decoded.fitted(max),
        None => decoded,
    })
}

#[allow(
    clippy::unused_async,
    reason = "the browser's backend awaits; callers see one signature"
)]
pub(crate) async fn encode(
    image: &DecodedImage,
    encoding: Encoding,
) -> Result<Vec<u8>, EncodeError> {
    let failed = |error: image::ImageError| EncodeError::Failed(error.to_string());
    let mut bytes = Vec::new();
    match encoding {
        Encoding::Png => image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                &image.rgba,
                image.width,
                image.height,
                ExtendedColorType::Rgba8,
            )
            .map_err(failed)?,
        Encoding::Jpeg { quality } => {
            let rgb: Vec<u8> = image
                .rgba
                .chunks_exact(4)
                .flat_map(|pixel| pixel.iter().take(3).copied())
                .collect();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality.clamp(1, 100))
                .write_image(&rgb, image.width, image.height, ExtendedColorType::Rgb8)
                .map_err(failed)?;
        }
    }
    Ok(bytes)
}
