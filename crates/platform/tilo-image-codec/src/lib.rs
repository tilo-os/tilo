//! Portable JPEG/PNG/WebP decode with native-compatible triangle resizing.
//!
//! [`DecodeImage::decode_native`] preserves the existing native image path:
//! it decodes the source before resizing. [`AdmissionLimits`] is a separate
//! experimental preflight/result check, never an implicit native restriction.
//! Admission does not enforce allocator, codec-intermediate, RSS or private
//! linear-memory limits. A future worker must enforce its own 256 MiB linear
//! memory maximum and refuse formats without enforceable decoder limits.
#[cfg(test)]
mod tests;

/// Encoded input and the existing store's output resize preference.
pub struct DecodeImage<'a> {
    pub encoded: &'a [u8],
    pub max_dimension: u32,
}
/// Straight-alpha, eight-bit RGBA pixels, ready for host validation.
#[derive(Debug, PartialEq, Eq)]
pub struct DecodedImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}
/// Decode or admission refusal.
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    InvalidImage,
    InvalidDimensions,
    EncodedLimit,
    DecodedLimit,
    DimensionLimit,
    LengthMismatch,
}
impl DecodeImage<'_> {
    /// Decode with the original native output semantics (dimension clamped to 1..=4096).
    ///
    /// # Errors
    /// Returns an error for undecodable input or zero-sized output. Source
    /// allocations are governed by the image library, not worker admission.
    pub fn decode_native(self) -> Result<DecodedImage, DecodeError> {
        let max_dimension = self.max_dimension.clamp(1, 4096);
        let img = image::load_from_memory(self.encoded).map_err(|_| DecodeError::InvalidImage)?;
        let img = if img.width().max(img.height()) > max_dimension {
            img.resize(
                max_dimension,
                max_dimension,
                image::imageops::FilterType::Triangle,
            )
        } else {
            img
        };
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        if width == 0 || height == 0 {
            return Err(DecodeError::InvalidDimensions);
        }
        Ok(DecodedImage {
            rgba: rgba.into_raw(),
            width,
            height,
        })
    }
}

/// Configurable experimental admission limits; not an allocator sandbox.
#[derive(Clone, Copy, Debug)]
pub struct AdmissionLimits {
    pub max_encoded_bytes: usize,
    pub max_rgba_bytes: u64,
    pub max_dimension: u32,
}
impl Default for AdmissionLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: 16 * 1024 * 1024,
            max_rgba_bytes: 64 * 1024 * 1024,
            max_dimension: 4096,
        }
    }
}
impl AdmissionLimits {
    /// Check dimensions and RGBA byte length without allocating pixels.
    ///
    /// # Errors
    /// Refuses zero, excessive or overflowing dimensions and excessive bytes.
    pub fn rgba_len(self, width: u32, height: u32) -> Result<usize, DecodeError> {
        if width == 0 || height == 0 {
            return Err(DecodeError::InvalidDimensions);
        }
        if width.max(height) > self.max_dimension {
            return Err(DecodeError::DimensionLimit);
        }
        let len = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(DecodeError::DecodedLimit)?;
        if len > self.max_rgba_bytes {
            return Err(DecodeError::DecodedLimit);
        }
        usize::try_from(len).map_err(|_| DecodeError::DecodedLimit)
    }
    /// Check encoded length and source dimensions before pixel decoding.
    ///
    /// This does not authorize fetches or worker execution. The image library's
    /// metadata allocator limit is best effort; worker execution needs a separate
    /// enforced memory boundary.
    ///
    /// # Errors
    /// Refuses malformed headers and encoded, source-dimension or RGBA excess.
    pub fn admit(self, encoded: &[u8]) -> Result<(u32, u32), DecodeError> {
        if encoded.len() > self.max_encoded_bytes {
            return Err(DecodeError::EncodedLimit);
        }
        let mut reader = image::ImageReader::new(std::io::Cursor::new(encoded))
            .with_guessed_format()
            .map_err(|_| DecodeError::InvalidImage)?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(self.max_rgba_bytes);
        reader.limits(limits);
        let dimensions = reader
            .into_dimensions()
            .map_err(|_| DecodeError::InvalidImage)?;
        self.rgba_len(dimensions.0, dimensions.1)?;
        Ok(dimensions)
    }
    /// Validate returned pixels before a host considers publishing them.
    ///
    /// # Errors
    /// Refuses excessive/invalid dimensions, byte ceilings or mismatched length.
    /// Owner liveness and request identity must be checked by the host as well.
    pub fn validate(self, decoded: &DecodedImage) -> Result<(), DecodeError> {
        let expected = self.rgba_len(decoded.width, decoded.height)?;
        if decoded.rgba.len() != expected {
            return Err(DecodeError::LengthMismatch);
        }
        Ok(())
    }
}
