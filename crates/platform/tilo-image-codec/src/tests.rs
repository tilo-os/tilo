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

/// A native codec future, which is ready at once.
fn now<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(output) => output,
        std::task::Poll::Pending => panic!("a native codec future is ready at once"),
    }
}

#[test]
fn the_header_names_format_and_size_without_decoding() {
    let png = fixture(image::ImageFormat::Png);
    assert_eq!(sniff(&png), Some(Format::Png));
    assert_eq!(dimensions(&png), Some((11, 7)));
    assert_eq!(
        sniff(&fixture(image::ImageFormat::Jpeg)),
        Some(Format::Jpeg)
    );
    assert_eq!(
        sniff(&fixture(image::ImageFormat::WebP)),
        Some(Format::WebP)
    );
    assert_eq!(sniff(b"not an image"), None);
    assert_eq!(dimensions(b"not an image"), None);
}

#[test]
fn pixels_encoded_and_decoded_come_back_as_they_were() {
    let mut image = DecodedImage::filled(5, 3, [10, 20, 30, 255]);
    image.rgba[0..4].copy_from_slice(&[200, 0, 0, 128]);
    let png = now(encode(&image, Encoding::Png)).expect("a PNG");
    assert_eq!(sniff(&png), Some(Format::Png));
    assert_eq!(now(decode(&png, None)), Ok(image.clone()));
    let jpeg = now(encode(&image, Encoding::Jpeg { quality: 90 })).expect("a JPEG");
    assert_eq!(sniff(&jpeg), Some(Format::Jpeg));
    let back = now(decode(&jpeg, None)).expect("the JPEG decodes");
    assert_eq!((back.width, back.height), (5, 3));
    assert!(!back.has_alpha(), "a JPEG keeps no alpha");
    assert_eq!(
        now(encode(
            &DecodedImage {
                rgba: vec![0; 3],
                width: 1,
                height: 1
            },
            Encoding::Png
        )),
        Err(EncodeError::Malformed)
    );
    assert_eq!(
        now(decode(b"not an image", None)),
        Err(DecodeError::InvalidImage)
    );
}

#[test]
fn a_bounded_decode_keeps_proportions_inside_the_bound() {
    let png = fixture(image::ImageFormat::Png);
    let decoded = now(decode(&png, Some(6))).expect("decodes");
    assert_eq!((decoded.width, decoded.height), (6, 4));
}

#[test]
fn pixels_crop_turn_scale_and_grey() {
    let mut image = DecodedImage::filled(3, 2, [0, 0, 0, 255]);
    image.rgba[0..4].copy_from_slice(&[255, 255, 255, 255]);
    let turned = image.turned(1);
    assert_eq!((turned.width, turned.height), (2, 3));
    assert_eq!(
        turned.pixel(1, 0),
        Some([255, 255, 255, 255]),
        "the top-left corner turns to the top right"
    );
    assert_eq!(image.turned(2).pixel(2, 1), Some([255, 255, 255, 255]));
    assert_eq!(image.turned(3).pixel(0, 2), Some([255, 255, 255, 255]));
    assert_eq!(image.turned(4), image);
    let crop = image.cropped(0, 0, 1, 1).expect("inside");
    assert_eq!(crop, DecodedImage::filled(1, 1, [255, 255, 255, 255]));
    assert_eq!(
        image.cropped(1, 1, 9, 9).map(|c| (c.width, c.height)),
        Some((2, 1))
    );
    assert_eq!(image.cropped(3, 0, 1, 1), None);
    let grey = DecodedImage::filled(40, 20, [90, 90, 90, 255]).resized(7, 3);
    assert_eq!((grey.width, grey.height), (7, 3));
    assert!(
        grey.rgba.chunks_exact(4).all(|p| p == [90, 90, 90, 255]),
        "a flat colour stays flat"
    );
    assert_eq!(DecodedImage::filled(40, 20, [0; 4]).fitted(10).width, 10);
    assert_eq!(
        DecodedImage::filled(1, 1, [255, 255, 255, 255]).luma(),
        vec![255]
    );
    assert_eq!(DecodedImage::from_rgba(2, 2, vec![0; 15]), None);
}
