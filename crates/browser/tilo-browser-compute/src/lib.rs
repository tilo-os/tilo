//! Image-only worker kernel; no host transport, UI, session or GPU dependency.
use tilo_image_codec::{DecodeError, DecodedImage};

/// Admit source metadata before decoding, then validate the requested result.
///
/// # Errors
/// Returns a codec error for malformed or excessive input/output.
pub fn decode(encoded: &[u8], max_dimension: u32) -> Result<DecodedImage, DecodeError> {
    let limits = tilo_image_codec::AdmissionLimits::default();
    limits.admit(encoded)?;
    if !(1..=4096).contains(&max_dimension) {
        return Err(DecodeError::InvalidDimensions);
    }
    let result = tilo_image_codec::DecodeImage {
        encoded,
        max_dimension,
    }
    .decode_native()?;
    tilo_image_codec::AdmissionLimits {
        max_dimension,
        ..limits
    }
    .validate(&result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[test]
    fn admitted_real_formats_match_production_pixels() {
        for format in [
            image::ImageFormat::Png,
            image::ImageFormat::Jpeg,
            image::ImageFormat::WebP,
        ] {
            let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                11,
                7,
                image::Rgb([12, 34, 56]),
            ));
            let mut bytes = Cursor::new(Vec::new());
            image.write_to(&mut bytes, format).unwrap();
            let expected = tilo_image_codec::DecodeImage {
                encoded: bytes.get_ref(),
                max_dimension: 5,
            }
            .decode_native()
            .unwrap();
            assert_eq!(decode(bytes.get_ref(), 5).unwrap(), expected);
        }
    }
    #[test]
    fn malformed_and_excessive_source_refused_before_resize() {
        assert!(decode(b"bad", 1).is_err());
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::new(4097, 1));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        assert_eq!(decode(bytes.get_ref(), 1), Err(DecodeError::DimensionLimit));
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use wasm_bindgen::prelude::*;
    #[wasm_bindgen]
    pub struct Pixels {
        pub width: u32,
        pub height: u32,
        rgba: Vec<u8>,
    }
    #[wasm_bindgen]
    impl Pixels {
        pub fn take_rgba(&mut self) -> Vec<u8> {
            std::mem::take(&mut self.rgba)
        }
    }
    #[wasm_bindgen]
    pub fn decode_image(encoded: &[u8], max_dimension: u32) -> Result<Pixels, JsValue> {
        let result = super::decode(encoded, max_dimension)
            .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
        Ok(Pixels {
            width: result.width,
            height: result.height,
            rgba: result.rgba,
        })
    }
}
