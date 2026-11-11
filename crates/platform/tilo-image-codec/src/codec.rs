//! Decode and encode, the one way every program does: the browser's own
//! codecs on the web, the `image` crate's natively. Both are asynchronous,
//! because a browser decodes and encodes off the calling task.

use crate::{DecodeError, DecodedImage};

/// How [`encode`] writes pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Lossless, alpha kept.
    Png,
    /// Lossy at `quality` (1-100), alpha dropped.
    Jpeg { quality: u8 },
}

impl Encoding {
    /// The media type of what it writes.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg { .. } => "image/jpeg",
        }
    }
}

/// Why [`encode`] wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The pixels are not `width * height * 4` bytes, or have no area.
    Malformed,
    /// The encoder refused them.
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

/// `bytes` decoded to straight-alpha RGBA, scaled down (keeping its
/// proportions) so neither side exceeds `max_dimension` when one is given.
///
/// # Errors
///
/// [`DecodeError::InvalidImage`] for bytes that are no image the codec
/// reads; [`DecodeError::InvalidDimensions`] for one with no area.
pub async fn decode(bytes: &[u8], max_dimension: Option<u32>) -> Result<DecodedImage, DecodeError> {
    crate::format::sniff(bytes).ok_or(DecodeError::InvalidImage)?;
    let decoded = crate::backend::decode(bytes, max_dimension).await?;
    if decoded.width == 0 || decoded.height == 0 {
        return Err(DecodeError::InvalidDimensions);
    }
    Ok(decoded)
}

/// `image` written as `encoding`.
///
/// # Errors
///
/// [`EncodeError::Malformed`] for pixels that do not match their size;
/// [`EncodeError::Failed`] when the encoder refuses them.
pub async fn encode(image: &DecodedImage, encoding: Encoding) -> Result<Vec<u8>, EncodeError> {
    let expected = (image.width as usize)
        .checked_mul(image.height as usize)
        .and_then(|pixels| pixels.checked_mul(4));
    if image.width == 0 || image.height == 0 || expected != Some(image.rgba.len()) {
        return Err(EncodeError::Malformed);
    }
    crate::backend::encode(image, encoding).await
}
