//! The one image codec a program uses: [`sniff`] and [`dimensions`] read a
//! header, [`decode`] and [`encode`] turn bytes into pixels and back, and
//! [`DecodedImage`]'s methods crop, turn, scale and grey them. Apps never decode
//! for themselves.
//!
//! [`DecodeImage::decode_native`] decodes the source before resizing.
//! [`AdmissionLimits`] is a preflight check on a returned image, never an
//! implicit restriction on the native path.
mod codec;
mod format;
mod pixels;
#[cfg(test)]
mod tests;

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
#[path = "native.rs"]
mod backend;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[path = "web.rs"]
mod backend;

pub use codec::{EncodeError, Encoding, decode, decode_now, encode, encode_now};
pub use format::{Format, dimensions, sniff};

pub struct DecodeImage<'a> {
    pub encoded: &'a [u8],
    pub max_dimension: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    InvalidImage,
    InvalidDimensions,
    EncodedLimit,
    DecodedLimit,
    DimensionLimit,
    LengthMismatch,
}
impl DecodeImage<'_> {
    /// Decode with the native output semantics: dimensions clamped to 1..=4096.
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
    /// Validate returned pixels before a host publishes them: dimensions, byte
    /// ceilings and length. The host still checks owner liveness and request
    /// identity.
    ///
    /// # Errors
    ///
    /// Refuses excessive dimensions, byte ceilings or a mismatched pixel length.
    pub fn validate(self, decoded: &DecodedImage) -> Result<(), DecodeError> {
        let expected = self.rgba_len(decoded.width, decoded.height)?;
        if decoded.rgba.len() != expected {
            return Err(DecodeError::LengthMismatch);
        }
        Ok(())
    }
}
