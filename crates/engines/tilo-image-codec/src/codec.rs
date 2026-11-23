//! Decode and encode: the browser's own codecs on the web, the `image` crate's
//! natively. Both are asynchronous, because a browser decodes off the calling
//! task.

use crate::{DecodeError, DecodedImage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Png,
    /// Lossy at `quality` (1-100), alpha dropped.
    Jpeg {
        quality: u8,
    },
    WebP,
}

impl Encoding {
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg { .. } => "image/jpeg",
            Self::WebP => "image/webp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The pixels are not `width * height * 4` bytes, or have no area.
    Malformed,
    Failed(String),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => f.write_str("the pixels do not match their size"),
            Self::Failed(reason) => write!(f, "the image was not encoded: {reason}"),
        }
    }
}

impl std::error::Error for EncodeError {}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidImage => "not an image this program reads",
            Self::InvalidDimensions => "the image has no area",
            Self::EncodedLimit => "the image is too large to read",
            Self::DecodedLimit => "the image has too many pixels",
            Self::DimensionLimit => "the image is too wide or tall",
            Self::LengthMismatch => "the decoded pixels do not match their size",
        })
    }
}

impl std::error::Error for DecodeError {}

/// `bytes` decoded to straight-alpha RGBA, scaled down keeping its proportions
/// so neither side exceeds `max_dimension`.
///
/// # Errors
///
/// [`DecodeError::InvalidImage`] for bytes that are no image the codec reads,
/// [`DecodeError::InvalidDimensions`] for one with no area.
pub async fn decode(bytes: &[u8], max_dimension: Option<u32>) -> Result<DecodedImage, DecodeError> {
    crate::format::sniff(bytes).ok_or(DecodeError::InvalidImage)?;
    let decoded = crate::backend::decode(bytes, max_dimension).await?;
    if decoded.width == 0 || decoded.height == 0 {
        return Err(DecodeError::InvalidDimensions);
    }
    Ok(decoded)
}

/// `bytes` decoded at once on the calling thread with the codec's own Rust
/// decoders; [`decode`] is the one to use.
///
/// # Errors
///
/// As [`decode`].
pub fn decode_now(bytes: &[u8], max_dimension: Option<u32>) -> Result<DecodedImage, DecodeError> {
    crate::format::sniff(bytes).ok_or(DecodeError::InvalidImage)?;
    let rgba = image::load_from_memory(bytes)
        .map_err(|_| DecodeError::InvalidImage)?
        .to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return Err(DecodeError::InvalidDimensions);
    }
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

pub async fn encode(image: &DecodedImage, encoding: Encoding) -> Result<Vec<u8>, EncodeError> {
    let expected = (image.width as usize)
        .checked_mul(image.height as usize)
        .and_then(|pixels| pixels.checked_mul(4));
    if image.width == 0 || image.height == 0 || expected != Some(image.rgba.len()) {
        return Err(EncodeError::Malformed);
    }
    crate::backend::encode(image, encoding).await
}

pub fn encode_now(image: &DecodedImage, encoding: Encoding) -> Result<Vec<u8>, EncodeError> {
    use image::{ExtendedColorType, ImageEncoder};
    let expected = (image.width as usize)
        .checked_mul(image.height as usize)
        .and_then(|pixels| pixels.checked_mul(4));
    if image.width == 0 || image.height == 0 || expected != Some(image.rgba.len()) {
        return Err(EncodeError::Malformed);
    }
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
        Encoding::WebP => image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
            .encode(
                &image.rgba,
                image.width,
                image.height,
                ExtendedColorType::Rgba8,
            )
            .map_err(failed)?,
    }
    Ok(bytes)
}
