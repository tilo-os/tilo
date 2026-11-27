//! What an image itself says: its upright size, when it was taken, which way its pixels are
//! turned, and where it was taken. EXIF is the container's concern, read before any pixel is
//! decoded; an image without it still has a size, and every field EXIF would fill stays empty
//! rather than invented. The size reported is the upright one a viewer shows.

use std::io::Cursor;

/// An image's own metadata, as its bytes state it.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageInfo {
    /// When the camera says it took the picture, as EXIF stores it (`2026:07:12 18:16:00`).
    pub captured_at: Option<String>,
    /// The EXIF orientation, 1 to 8, only when the container states a valid one.
    pub orientation: Option<u16>,
    /// The upright width: the stored width, swapped with the height when the orientation turns
    /// the picture on its side.
    pub width: u32,
    /// The upright height.
    pub height: u32,
    /// Where the camera says it took the picture, in WGS-84 degrees.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

/// The metadata `bytes` states, or `None` for bytes that are no image this crate reads.
#[must_use]
pub fn read(bytes: &[u8]) -> Option<ImageInfo> {
    let size = imagesize::blob_size(bytes).ok()?;
    let width = u32::try_from(size.width).ok()?;
    let height = u32::try_from(size.height).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let exif = exif_of(bytes);
    let orientation = exif.as_ref().and_then(orientation_of);
    let (width, height) = if orientation.is_some_and(|turns| turns >= 5) {
        (height, width)
    } else {
        (width, height)
    };
    let (latitude, longitude) = exif.as_ref().map_or((None, None), location_of);
    Some(ImageInfo {
        captured_at: exif.as_ref().and_then(capture_of),
        orientation,
        width,
        height,
        latitude,
        longitude,
    })
}

/// The orientation `bytes` states, 1 to 8, when their container states a valid one: what turns
/// decoded pixels upright.
#[must_use]
pub fn orientation(bytes: &[u8]) -> Option<u16> {
    exif_of(bytes).as_ref().and_then(orientation_of)
}

fn exif_of(bytes: &[u8]) -> Option<exif::Exif> {
    exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()
}

fn orientation_of(exif: &exif::Exif) -> Option<u16> {
    let value = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
        .value
        .get_uint(0)?;
    u16::try_from(value)
        .ok()
        .filter(|orientation| (1..=8).contains(orientation))
}

/// The first capture time the container states: the original, then the digitised one, then the
/// file's own.
fn capture_of(exif: &exif::Exif) -> Option<String> {
    [
        exif::Tag::DateTimeOriginal,
        exif::Tag::DateTimeDigitized,
        exif::Tag::DateTime,
    ]
    .iter()
    .find_map(|tag| ascii_field(exif.get_field(*tag, exif::In::PRIMARY)?))
}

fn location_of(exif: &exif::Exif) -> (Option<f64>, Option<f64>) {
    let latitude = gps_coordinate(exif, exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef);
    let longitude = gps_coordinate(exif, exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef);
    (latitude, longitude)
}

/// A GPS coordinate as degrees, from the degrees/minutes/seconds EXIF stores and the hemisphere
/// its reference names. A coordinate with no hemisphere, or with a zero denominator, is no
/// coordinate, and a non-finite one is no place.
fn gps_coordinate(exif: &exif::Exif, value_tag: exif::Tag, ref_tag: exif::Tag) -> Option<f64> {
    let field = exif.get_field(value_tag, exif::In::PRIMARY)?;
    let exif::Value::Rational(values) = &field.value else {
        return None;
    };
    let [degrees, minutes, seconds, ..] = values.as_slice() else {
        return None;
    };
    if degrees.denom == 0 || minutes.denom == 0 || seconds.denom == 0 {
        return None;
    }
    let magnitude = f64::from(degrees.num) / f64::from(degrees.denom)
        + f64::from(minutes.num) / f64::from(minutes.denom) / 60.0
        + f64::from(seconds.num) / f64::from(seconds.denom) / 3600.0;
    let reference = ascii_field(exif.get_field(ref_tag, exif::In::PRIMARY)?)?;
    let signed = match reference.as_str() {
        "N" | "E" => magnitude,
        "S" | "W" => -magnitude,
        _ => return None,
    };
    signed.is_finite().then_some(signed)
}

fn ascii_field(field: &exif::Field) -> Option<String> {
    let exif::Value::Ascii(values) = &field.value else {
        return None;
    };
    let value = values.first()?;
    let text = std::str::from_utf8(value)
        .ok()?
        .trim_matches(char::from(0))
        .trim();
    (!text.is_empty()).then_some(text.to_owned())
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "a test builds the coordinates it asserts"
)]
mod tests {
    use super::*;

    /// The header of a `width` by `height` PNG: all a size is read from.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(&13_u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        bytes
    }

    const CAPTURED_AT: &str = "2026:07:12 18:16:00";

    /// A 3x2 JPEG whose EXIF states `orientation` and the file's own date (tag 0x0132).
    fn jpeg(orientation: u16, captured_at: Option<&str>) -> Vec<u8> {
        let datetime = captured_at.map(|text| {
            let mut value = text.as_bytes().to_vec();
            value.push(0);
            value
        });
        let entries = 1 + usize::from(datetime.is_some());
        let ifd0_len = 2 + entries * 12 + 4;
        let datetime_at = 8 + ifd0_len;

        let mut tiff = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
        tiff.extend_from_slice(&u16::try_from(entries).unwrap().to_le_bytes());
        // IFD0's entries, in tag order.
        tiff.extend_from_slice(&entry(0x0112, 3, 1, inline_u16(orientation)));
        if let Some(value) = &datetime {
            tiff.extend_from_slice(&entry(
                0x0132,
                2,
                u32::try_from(value.len()).unwrap(),
                u32::try_from(datetime_at).unwrap().to_le_bytes(),
            ));
        }
        tiff.extend_from_slice(&0_u32.to_le_bytes());
        if let Some(value) = &datetime {
            tiff.extend_from_slice(value);
        }

        // The segment's length counts its own two bytes and the `Exif\0\0` header.
        let segment = u16::try_from(tiff.len() + 8).unwrap();
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        jpeg.extend_from_slice(&segment.to_be_bytes());
        jpeg.extend_from_slice(b"Exif\0\0");
        jpeg.extend_from_slice(&tiff);
        // A 3x2 frame header, which the size is read from.
        jpeg.extend_from_slice(&[
            0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x02, 0x00, 0x03, 0x03, 0x01, 0x11, 0x00, 0x02,
            0x11, 0x01, 0x03, 0x11, 0x01,
        ]);
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        jpeg
    }

    fn entry(tag: u16, kind: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(12);
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend_from_slice(&value);
        bytes
    }

    fn inline_u16(value: u16) -> [u8; 4] {
        let [low, high] = value.to_le_bytes();
        [low, high, 0, 0]
    }

    #[test]
    fn a_plain_image_states_its_size_and_nothing_it_does_not_know() {
        let info = read(&png(2, 1)).expect("a PNG header states its size");
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(info.captured_at, None);
        assert_eq!(info.orientation, None);
        assert_eq!((info.latitude, info.longitude), (None, None));
        assert_eq!(orientation(&png(2, 1)), None);
        assert_eq!(read(b"not a picture"), None);
    }

    #[test]
    fn an_orientation_that_turns_the_picture_reports_the_upright_size() {
        for orientation in 1..=8 {
            let bytes = jpeg(orientation, None);
            assert_eq!(super::orientation(&bytes), Some(orientation));
            let info = read(&bytes).expect("a JPEG states its size");
            let expected = if orientation >= 5 { (2, 3) } else { (3, 2) };
            assert_eq!(
                (info.width, info.height),
                expected,
                "orientation {orientation}"
            );
        }
        for invalid in [0, 9, u16::MAX] {
            let bytes = jpeg(invalid, None);
            assert_eq!(super::orientation(&bytes), None, "orientation {invalid}");
            let info = read(&bytes).expect("a JPEG states its size, metadata or not");
            assert_eq!((info.width, info.height), (3, 2), "orientation {invalid}");
        }
    }

    #[test]
    fn the_capture_time_comes_from_the_container() {
        let bytes = jpeg(1, Some(CAPTURED_AT));
        let info = read(&bytes).expect("a JPEG states its metadata");
        assert_eq!(info.captured_at.as_deref(), Some(CAPTURED_AT));
        assert_eq!((info.latitude, info.longitude), (None, None));
    }
}
