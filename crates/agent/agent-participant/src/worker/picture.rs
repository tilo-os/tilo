//! Pictures made small enough to show a planning model, in a confined
//! worker.
//!
//! A picture someone put in the room is untrusted input, and decoding one
//! runs a large parser over it, so the host never does: a worker started
//! in [`Mode::Picture`] decodes it under decode limits and a pixel cap,
//! turns it as its camera meant, shrinks it to fit and writes it out again
//! as a PNG (when it has transparency) or a JPEG. What it writes holds only
//! pixels: whatever the source said about itself (where and when a photo
//! was taken, the camera) is left behind.

use super::process::Process;
use super::proto::{Mode, Reply, Request};

/// The most pixels the worker decodes: a 40-megapixel photo, four bytes
/// each, fits well inside its allocation limit.
const MOST_DECODED_PIXELS: u64 = 40_000_000;

/// The most the decoder may allocate at once.
const MOST_DECODE_ALLOCATION: u64 = 256 * 1024 * 1024;

/// The longest side the decoder accepts.
const MOST_DECODED_SIDE: u32 = 16_384;

/// The JPEG qualities tried, best first, until the picture fits.
const QUALITIES: [u8; 3] = [85, 70, 55];

/// `bytes`, a picture of any format the worker reads, at most `max_side`
/// pixels a side and `max_bytes` long, decoded and written out again in a
/// confined worker.
///
/// # Errors
///
/// The worker did not start, or could not read or shrink the picture.
pub(crate) fn shrink_picture(
    bytes: &[u8],
    max_side: u32,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let mut process = Process::start(Mode::Picture)?;
    match process.call(&Request::Shrink {
        bytes: bytes.to_vec(),
        max_side,
        max_bytes: u64::try_from(max_bytes).unwrap_or(u64::MAX),
    })? {
        Reply::Picture(shrunk) => Ok(shrunk),
        Reply::Failed(reason) => Err(reason),
        _ => Err("the picture worker answered out of turn".into()),
    }
}

/// In the worker: `bytes` decoded, turned upright, shrunk to `max_side` and
/// encoded within `max_bytes`.
pub(super) fn shrink(bytes: &[u8], max_side: u32, max_bytes: u64) -> Reply {
    match shrunk(bytes, max_side, max_bytes) {
        Ok(picture) => Reply::Picture(picture),
        Err(reason) => Reply::Failed(reason),
    }
}

fn shrunk(bytes: &[u8], max_side: u32, max_bytes: u64) -> Result<Vec<u8>, String> {
    use image::ImageDecoder as _;
    let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("the picture could not be read: {error}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MOST_DECODED_SIDE);
    limits.max_image_height = Some(MOST_DECODED_SIDE);
    limits.max_alloc = Some(MOST_DECODE_ALLOCATION);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| format!("the picture could not be read: {error}"))?;
    let (width, height) = decoder.dimensions();
    if u64::from(width).saturating_mul(u64::from(height)) > MOST_DECODED_PIXELS {
        return Err(format!(
            "the picture is {width}×{height} pixels, more than is read to show a model"
        ));
    }
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut picture = image::DynamicImage::from_decoder(decoder)
        .map_err(|error| format!("the picture could not be read: {error}"))?;
    picture.apply_orientation(orientation);
    let mut side = max_side.max(1);
    // Smaller and smaller until it fits: a few steps at most.
    for _ in 0..6 {
        let fitted = if picture.width() > side || picture.height() > side {
            picture.resize(side, side, image::imageops::FilterType::Triangle)
        } else {
            picture.clone()
        };
        if let Some(encoded) = encoded_within(&fitted, max_bytes)? {
            return Ok(encoded);
        }
        side = side.saturating_mul(3) / 4;
    }
    Err("the picture could not be made small enough to show".into())
}

/// `picture` as a PNG when it has transparency and fits, else as a JPEG at
/// the best quality that fits `max_bytes`; `None` when none does.
fn encoded_within(
    picture: &image::DynamicImage,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, String> {
    let encode = |format: image::ImageFormat, picture: &image::DynamicImage| {
        let mut bytes = Vec::new();
        picture
            .write_to(&mut std::io::Cursor::new(&mut bytes), format)
            .map(|()| bytes)
            .map_err(|error| format!("the picture could not be written: {error}"))
    };
    if picture.color().has_alpha() {
        let png = encode(image::ImageFormat::Png, picture)?;
        if png.len() <= max_bytes {
            return Ok(Some(png));
        }
    }
    let rgb = image::DynamicImage::ImageRgb8(picture.to_rgb8());
    for quality in QUALITIES {
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
            .encode_image(&rgb)
            .map_err(|error| format!("the picture could not be written: {error}"))?;
        if bytes.len() <= max_bytes {
            return Ok(Some(bytes));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vision::{Admission, admit, picture_size, tests::png};

    #[test]
    fn a_large_picture_is_shrunk_in_a_confined_worker_and_garbage_is_refused() {
        let shrunk = shrink_picture(&png(3000, 1500), 1024, 1024 * 1024).unwrap();
        let (format, width, height) = picture_size(&shrunk).unwrap();
        assert_eq!(format, "JPEG", "no transparency: a JPEG");
        assert_eq!((width, height), (1024, 512));
        assert!(matches!(admit(&shrunk), Admission::PassThrough { .. }));

        let refused = shrink_picture(b"GIF89a not really", 1024, 1024 * 1024).unwrap_err();
        assert!(refused.contains("could not be read"), "{refused}");
    }

    #[test]
    fn a_photo_is_written_out_again_without_what_it_said_about_itself() {
        // A JPEG carrying an EXIF segment (a camera and where it was).
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .encode_image(&image::DynamicImage::ImageRgb8(image::RgbImage::new(
                40, 30,
            )))
            .unwrap();
        let exif = b"Exif\0\0GPS 52.52N 13.40E secret-camera";
        let mut tagged = jpeg[..2].to_vec();
        tagged.extend([0xFF, 0xE1]);
        tagged.extend(u16::try_from(exif.len() + 2).unwrap().to_be_bytes());
        tagged.extend(exif);
        tagged.extend(&jpeg[2..]);
        let shrunk = shrunk(&tagged, 1024, 1024 * 1024).unwrap();
        assert_eq!(picture_size(&shrunk), Some(("JPEG", 40, 30)));
        assert!(
            !shrunk.windows(13).any(|window| window == b"secret-camera"),
            "what the photo said about itself is left behind"
        );
    }

    #[test]
    fn a_picture_with_transparency_stays_a_png() {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::new(2000, 100))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        let shrunk = shrunk(&bytes, 1024, 1024 * 1024).unwrap();
        assert_eq!(picture_size(&shrunk), Some(("PNG", 1024, 51)));
    }
}
