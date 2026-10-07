use super::*;
use std::io::Cursor;

fn fixture(format: image::ImageFormat) -> Vec<u8> {
    let rgba = image::RgbaImage::from_fn(11, 7, |x, y| {
        image::Rgba([
            u8::try_from(x.checked_mul(19).unwrap()).unwrap(),
            u8::try_from(y.checked_mul(31).unwrap()).unwrap(),
            67,
            u8::try_from(x.checked_add(y).unwrap().checked_mul(13).unwrap()).unwrap(),
        ])
    });
    let img = image::DynamicImage::ImageRgba8(rgba);
    let img = if format == image::ImageFormat::Jpeg {
        image::DynamicImage::ImageRgb8(img.to_rgb8())
    } else {
        img
    };
    let mut encoded = Cursor::new(Vec::new());
    img.write_to(&mut encoded, format).unwrap();
    encoded.into_inner()
}

#[test]
fn native_png_jpeg_webp_match_existing_decode_resize_and_alpha() {
    for format in [
        image::ImageFormat::Png,
        image::ImageFormat::Jpeg,
        image::ImageFormat::WebP,
    ] {
        let encoded = fixture(format);
        for limit in [0, 1, 5, 11, 2048, u32::MAX] {
            let original = image::load_from_memory(&encoded).unwrap();
            let cap = limit.clamp(1, 4096);
            let original = if original.width().max(original.height()) > cap {
                original.resize(cap, cap, image::imageops::FilterType::Triangle)
            } else {
                original
            };
            let expected = original.to_rgba8();
            let actual = DecodeImage {
                encoded: &encoded,
                max_dimension: limit,
            }
            .decode_native()
            .unwrap();
            assert_eq!(
                (actual.width, actual.height),
                expected.dimensions(),
                "{format:?}/{limit}"
            );
            assert_eq!(actual.rgba, expected.into_raw(), "{format:?}/{limit}");
        }
    }
}

#[test]
fn malformed_native_input_fails() {
    for encoded in [
        &[][..],
        &b"not an image"[..],
        &fixture(image::ImageFormat::Png)[..20],
    ] {
        assert!(
            DecodeImage {
                encoded,
                max_dimension: 2048
            }
            .decode_native()
            .is_err()
        );
    }
}

#[test]
fn admission_checks_encoded_source_dimensions_and_bytes_before_decode() {
    let encoded = fixture(image::ImageFormat::Png);
    assert_eq!(AdmissionLimits::default().admit(&encoded), Ok((11, 7)));
    assert_eq!(
        AdmissionLimits {
            max_encoded_bytes: encoded.len() - 1,
            ..AdmissionLimits::default()
        }
        .admit(&encoded),
        Err(DecodeError::EncodedLimit)
    );
    assert_eq!(
        AdmissionLimits {
            max_dimension: 10,
            ..AdmissionLimits::default()
        }
        .admit(&encoded),
        Err(DecodeError::DimensionLimit)
    );
    assert_eq!(
        AdmissionLimits {
            max_rgba_bytes: 307,
            ..AdmissionLimits::default()
        }
        .admit(&encoded),
        Err(DecodeError::DecodedLimit)
    );
    assert!(AdmissionLimits::default().admit(b"bad header").is_err());
}

#[test]
fn rgba_validation_checks_zero_overflow_dimension_byte_ceiling_and_exact_length() {
    let limits = AdmissionLimits::default();
    assert_eq!(limits.rgba_len(0, 1), Err(DecodeError::InvalidDimensions));
    assert_eq!(limits.rgba_len(4097, 1), Err(DecodeError::DimensionLimit));
    assert_eq!(
        AdmissionLimits {
            max_rgba_bytes: 3,
            ..limits
        }
        .rgba_len(1, 1),
        Err(DecodeError::DecodedLimit)
    );
    assert!(
        AdmissionLimits {
            max_dimension: u32::MAX,
            max_rgba_bytes: u64::MAX,
            ..limits
        }
        .rgba_len(u32::MAX, u32::MAX)
        .is_err()
    );
    assert_eq!(limits.rgba_len(4096, 4096), Ok(64 * 1024 * 1024));
    assert_eq!(
        limits.validate(&DecodedImage {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3]
        }),
        Err(DecodeError::LengthMismatch)
    );
    assert_eq!(
        limits.validate(&DecodedImage {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 4]
        }),
        Ok(())
    );
}
