//! The pixel pipeline: decode, crop, rotate, encode.
//!
//! Everything here is a pure function over RGBA8 buffers. No widget, no
//! document, no clock — which is what lets the edit operations be asserted on
//! their output bytes rather than on a flag that claims they ran.
//!
//! # The pipeline, and why the order is fixed
//!
//! A rendered frame is always `decode + EXIF-normalise → crop → rotate`:
//!
//! ```text
//!   upright RGBA  ──crop(Rect01 in UPRIGHT coords)──▶  cropped  ──rotate(N×90° CW)──▶  shown
//! ```
//!
//! EXIF orientation is a container concern, so decode first turns encoded
//! pixels into their canonical upright form. Crop then lives in those
//! *upright source* normalized coordinates, never in the user-rotated frame.
//! That is the whole reason rotation stays a free operation: a
//! stored crop keeps meaning exactly the same region of the photograph no
//! matter how many times the image is subsequently turned. Store the crop in
//! rotated space instead and every rotation has to rewrite it, which is a
//! migration on a CRDT field that two peers can be editing at once.
//!
//! The cost is that a crop *taken* from the rotated view has to be mapped back
//! through the rotation before it is stored — [`unrotate_rect`] is that map,
//! and it is exercised at every quarter turn by the tests below.

use std::io::{Cursor, Read};

use crate::model::Rect01;

/// Largest source image Photos will expand in memory.
///
/// RGBA8 needs four bytes per pixel, so this caps one decoded source at about
/// 128 MiB. Rendering may briefly hold a second buffer while applying an
/// edit, which is still bounded rather than letting a compressed image
/// exhaust the session.
pub const MAX_DECODED_PIXELS: u64 = 32 * 1024 * 1024;

/// Reject implausibly large compressed inputs before handing them to a codec.
pub const MAX_ENCODED_BYTES: usize = 64 * 1024 * 1024;

pub fn read_bounded(reader: impl Read, max_bytes: usize) -> Option<Vec<u8>> {
    let limit = u64::try_from(max_bytes).ok()?.saturating_add(1);
    let mut bytes = Vec::new();
    reader.take(limit).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= max_bytes).then_some(bytes)
}

#[derive(Clone, PartialEq, Eq)]
pub struct Rgba {
    pub w: u32,
    pub h: u32,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Rgba {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rgba({}x{}, {} bytes)", self.w, self.h, self.bytes.len())
    }
}

impl Rgba {
    pub fn new(w: u32, h: u32, bytes: Vec<u8>) -> Option<Self> {
        if w == 0 || h == 0 || bytes.len() != (w as usize) * (h as usize) * 4 {
            return None;
        }
        Some(Self { w, h, bytes })
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.w || y >= self.h {
            return None;
        }
        let i = ((y as usize) * (self.w as usize) + (x as usize)) * 4;
        self.bytes.get(i..i + 4)?.try_into().ok()
    }
}

pub async fn decode_async(bytes: &[u8]) -> Option<Rgba> {
    size(bytes)?;
    let orientation = encoded_exif_orientation(bytes);
    let decoded = ui::image_codec::decode(bytes, None).await.ok()?;
    let decoded = Rgba::new(decoded.width, decoded.height, decoded.rgba)?;
    Some(normalize_exif_orientation(decoded, orientation))
}

pub fn decode(bytes: &[u8]) -> Option<Rgba> {
    size(bytes)?;
    let orientation = encoded_exif_orientation(bytes);
    let decoded = ui::image_codec::decode_now(bytes, None).ok()?;
    let decoded = Rgba::new(decoded.width, decoded.height, decoded.rgba)?;
    Some(normalize_exif_orientation(decoded, orientation))
}

pub fn size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() > MAX_ENCODED_BYTES {
        return None;
    }
    let (w, h) = ui::image_codec::dimensions(bytes)?;
    within_decode_limits(w, h).then_some((w, h))
}

fn encoded_exif_orientation(bytes: &[u8]) -> Option<u16> {
    let mut cursor = Cursor::new(bytes);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;
    let value = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
        .value
        .get_uint(0)?;
    u16::try_from(value)
        .ok()
        .filter(|orientation| (1..=8).contains(orientation))
}

#[expect(
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "`Rgba` guarantees `w * h * 4` bytes within the decode limits; orientation only swaps axes, so every offset is in bounds and the rebuilt buffer is valid"
)]
fn normalize_exif_orientation(src: Rgba, orientation: Option<u16>) -> Rgba {
    let orientation = orientation.unwrap_or(1);
    if orientation == 1 || !(1..=8).contains(&orientation) {
        return src;
    }

    let swaps_axes = orientation >= 5;
    let (w, h) = if swaps_axes {
        (src.h, src.w)
    } else {
        (src.w, src.h)
    };
    let mut bytes = vec![0_u8; src.bytes.len()];
    for y in 0..h {
        for x in 0..w {
            let (source_x, source_y) = match orientation {
                2 => (src.w - 1 - x, y),
                3 => (src.w - 1 - x, src.h - 1 - y),
                4 => (x, src.h - 1 - y),
                5 => (y, x),
                6 => (y, src.h - 1 - x),
                7 => (src.w - 1 - y, src.h - 1 - x),
                8 => (src.w - 1 - y, x),
                _ => return src,
            };
            let source = (u64::from(source_y) * u64::from(src.w) + u64::from(source_x)) * 4;
            let destination = (u64::from(y) * u64::from(w) + u64::from(x)) * 4;
            let source = usize::try_from(source).expect("validated image dimensions fit usize");
            let destination =
                usize::try_from(destination).expect("validated image dimensions fit usize");
            bytes[destination..destination + 4].copy_from_slice(&src.bytes[source..source + 4]);
        }
    }
    Rgba::new(w, h, bytes).expect("EXIF orientation preserves a valid RGBA buffer")
}

fn within_decode_limits(w: u32, h: u32) -> bool {
    w != 0 && h != 0 && u64::from(w).saturating_mul(u64::from(h)) <= MAX_DECODED_PIXELS
}

pub fn encode_png(img: &Rgba) -> Option<Vec<u8>> {
    ui::png::encode_rgba8(img.w, img.h, &img.bytes).ok()
}

pub fn crop(src: &Rgba, rect: Rect01) -> Rgba {
    let rect = rect.clamped();
    let x0 = ((rect.x * f64::from(src.w)).floor() as u32).min(src.w - 1);
    let y0 = ((rect.y * f64::from(src.h)).floor() as u32).min(src.h - 1);
    let w = ((rect.w * f64::from(src.w)).round() as u32).clamp(1, src.w - x0);
    let h = ((rect.h * f64::from(src.h)).round() as u32).clamp(1, src.h - y0);

    let mut bytes = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for y in 0..h {
        let row = ((y0 + y) as usize) * (src.w as usize) * 4;
        let start = row + (x0 as usize) * 4;
        let end = start + (w as usize) * 4;
        if let Some(slice) = src.bytes.get(start..end) {
            bytes.extend_from_slice(slice);
        }
    }
    Rgba { w, h, bytes }
}

pub fn rotate(src: &Rgba, quarters: u8) -> Rgba {
    match quarters % 4 {
        0 => src.clone(),
        1 => rotate_cw(src),
        2 => rotate_cw(&rotate_cw(src)),
        _ => rotate_cw(&rotate_cw(&rotate_cw(src))),
    }
}

fn rotate_cw(src: &Rgba) -> Rgba {
    let (sw, sh) = (src.w as usize, src.h as usize);
    let (dw, dh) = (sh, sw);
    let mut bytes = vec![0u8; dw * dh * 4];
    for dy in 0..dh {
        for dx in 0..dw {
            let sx = dy;
            let sy = sh - 1 - dx;
            let si = (sy * sw + sx) * 4;
            let di = (dy * dw + dx) * 4;
            if let (Some(dst), Some(src_px)) =
                (bytes.get_mut(di..di + 4), src.bytes.get(si..si + 4))
            {
                dst.copy_from_slice(src_px);
            }
        }
    }
    Rgba {
        w: dw as u32,
        h: dh as u32,
        bytes,
    }
}

pub fn render(original: &Rgba, crop_rect: Rect01, quarters: u8) -> Rgba {
    let cropped = if crop_rect.is_full() {
        original.clone()
    } else {
        crop(original, crop_rect)
    };
    rotate(&cropped, quarters)
}

pub fn unrotate_rect(rect: Rect01, quarters: u8) -> Rect01 {
    let mut r = rect.clamped();
    for _ in 0..(quarters % 4) {
        r = Rect01 {
            x: r.y,
            y: 1.0 - (r.x + r.w),
            w: r.h,
            h: r.w,
        };
    }
    r.clamped()
}

pub fn to_peniko(img: &Rgba) -> vello::peniko::ImageData {
    ui::raster::rgba8(img.bytes.clone(), img.w, img.h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_is_bounded_round_trips_png_and_refuses_non_images() {
        assert_eq!(
            read_bounded(Cursor::new([1, 2, 3, 4]), 4),
            Some(vec![1, 2, 3, 4])
        );
        assert_eq!(read_bounded(Cursor::new([1, 2, 3, 4, 5]), 4), None);

        for (w, h, ok) in [
            (8_000, 4_000, true),
            (8_193, 4_096, false),
            (u32::MAX, u32::MAX, false),
            (0, 1, false),
        ] {
            assert_eq!(within_decode_limits(w, h), ok, "decode limits for {w}x{h}");
        }

        let src = ramp(6, 4);
        let png = encode_png(&src).expect("encode");
        assert_eq!(&png[1..4], b"PNG", "not a PNG");
        let back = decode(&png).expect("decode");
        assert_eq!((back.w, back.h), (6, 4));
        assert_eq!(back.bytes, src.bytes, "the round trip changed pixels");

        assert!(decode(b"this is not a picture").is_none());
        assert!(decode(&[]).is_none());
    }

    fn ramp(w: u32, h: u32) -> Rgba {
        let mut bytes = Vec::new();
        for y in 0..h {
            for x in 0..w {
                bytes.extend_from_slice(&[x as u8, y as u8, 0, 255]);
            }
        }
        Rgba::new(w, h, bytes).unwrap()
    }

    fn coordinates(image: &Rgba) -> Vec<(u8, u8)> {
        (0..image.h)
            .flat_map(|y| {
                (0..image.w).map(move |x| {
                    let pixel = image.pixel(x, y).expect("coordinate is in bounds");
                    (pixel[0], pixel[1])
                })
            })
            .collect()
    }

    #[test]
    fn exif_orientation_uprights_pixels_and_is_read_from_real_jpeg_bytes() {
        let source = ramp(3, 2);
        let cases = [
            (
                1,
                (3, 2),
                vec![(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)],
            ),
            (
                2,
                (3, 2),
                vec![(2, 0), (1, 0), (0, 0), (2, 1), (1, 1), (0, 1)],
            ),
            (
                3,
                (3, 2),
                vec![(2, 1), (1, 1), (0, 1), (2, 0), (1, 0), (0, 0)],
            ),
            (
                4,
                (3, 2),
                vec![(0, 1), (1, 1), (2, 1), (0, 0), (1, 0), (2, 0)],
            ),
            (
                5,
                (2, 3),
                vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0), (2, 1)],
            ),
            (
                6,
                (2, 3),
                vec![(0, 1), (0, 0), (1, 1), (1, 0), (2, 1), (2, 0)],
            ),
            (
                7,
                (2, 3),
                vec![(2, 1), (2, 0), (1, 1), (1, 0), (0, 1), (0, 0)],
            ),
            (
                8,
                (2, 3),
                vec![(2, 0), (2, 1), (1, 0), (1, 1), (0, 0), (0, 1)],
            ),
        ];

        for (orientation, dimensions, expected) in cases {
            let upright = normalize_exif_orientation(source.clone(), Some(orientation));
            assert_eq!(
                (upright.w, upright.h),
                dimensions,
                "orientation {orientation}"
            );
            assert_eq!(coordinates(&upright), expected, "orientation {orientation}");
        }
        assert_eq!(normalize_exif_orientation(source.clone(), None), source);
        assert_eq!(normalize_exif_orientation(source.clone(), Some(0)), source);
        assert_eq!(normalize_exif_orientation(source.clone(), Some(9)), source);

        for orientation in 1..=8 {
            let jpeg = jpeg_with_orientation(orientation);
            assert_eq!(encoded_exif_orientation(&jpeg), Some(orientation));
            let upright = decode(&jpeg).expect("decode tagged JPEG");
            let expected_dimensions = if orientation >= 5 { (2, 3) } else { (3, 2) };
            assert_eq!(
                (upright.w, upright.h),
                expected_dimensions,
                "orientation {orientation}"
            );
        }
        for orientation in [0, 9, u16::MAX] {
            let jpeg = jpeg_with_orientation(orientation);
            assert_eq!(
                encoded_exif_orientation(&jpeg),
                None,
                "orientation {orientation}"
            );
            let upright = decode(&jpeg).expect("decode JPEG despite invalid metadata");
            assert_eq!((upright.w, upright.h), (3, 2), "orientation {orientation}");
        }
    }

    fn jpeg_with_orientation(orientation: u16) -> Vec<u8> {
        use image::ImageEncoder as _;

        let rgb = vec![
            255, 0, 0, 0, 255, 0, 0, 0, 255, // first row
            255, 255, 0, 0, 255, 255, 255, 0, 255, // second row
        ];
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
            .write_image(&rgb, 3, 2, image::ExtendedColorType::Rgb8)
            .expect("encode test JPEG");
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);

        let mut payload = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0".to_vec();
        payload.extend_from_slice(&[
            0x12,
            0x01, // Orientation tag
            0x03,
            0x00, // SHORT
            0x01,
            0x00,
            0x00,
            0x00, // one value
            orientation as u8,
            (orientation >> 8) as u8,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00, // no next IFD
        ]);
        let segment_length = u16::try_from(payload.len() + 2).expect("small APP1 segment");
        let mut tagged = Vec::with_capacity(jpeg.len() + payload.len() + 4);
        tagged.extend_from_slice(&jpeg[..2]);
        tagged.extend_from_slice(&[0xff, 0xe1]);
        tagged.extend_from_slice(&segment_length.to_be_bytes());
        tagged.extend_from_slice(&payload);
        tagged.extend_from_slice(&jpeg[2..]);
        tagged
    }

    fn rect(left: f64, top: f64, width: f64, height: f64) -> Rect01 {
        Rect01 {
            x: left,
            y: top,
            w: width,
            h: height,
        }
    }

    #[test]
    fn crops_take_exactly_their_bounded_region() {
        let src = ramp(4, 4);
        let out = crop(&src, rect(0.5, 0.5, 0.5, 0.5));
        assert_eq!((out.w, out.h), (2, 2));
        assert_eq!(out.pixel(0, 0).unwrap()[0..2], [2, 2]);
        assert_eq!(out.pixel(1, 1).unwrap()[0..2], [3, 3]);
        assert_eq!(out.bytes.len(), 2 * 2 * 4);

        let out = crop(&src, rect(0.5, 0.5, 0.0, 0.0));
        assert_eq!((out.w, out.h), (1, 1), "a zero-area crop must not vanish");
        assert_eq!(out.bytes.len(), 4);
    }

    #[test]
    fn rotation_moves_pixels_and_unrotate_inverts_the_view() {
        let cases = [
            (
                "quarter turn",
                rotate(&ramp(4, 2), 1),
                (2, 4),
                vec![((0, 0), [0, 1]), ((1, 0), [0, 0])],
            ),
            (
                "square quarter turn",
                rotate(&ramp(3, 3), 1),
                (3, 3),
                vec![((0, 0), [0, 2])],
            ),
            (
                "render crops then rotates",
                render(&ramp(4, 2), rect(0.0, 0.0, 0.5, 1.0), 1),
                (2, 2),
                vec![((0, 0), [0, 1])],
            ),
        ];
        for (case, out, dims, pixels) in cases {
            assert_eq!((out.w, out.h), dims, "case {case}");
            for ((x, y), want) in pixels {
                assert_eq!(
                    out.pixel(x, y).unwrap()[0..2],
                    want,
                    "case {case} pixel ({x}, {y})"
                );
            }
        }
        assert_ne!(
            rotate(&ramp(3, 3), 1).bytes,
            ramp(3, 3).bytes,
            "a square rotation must still move pixels"
        );
        let src = ramp(5, 3);
        assert_eq!(rotate(&src, 4), src, "four quarter turns are the identity");
        assert_eq!(rotate(&src, 0), src);

        let back = unrotate_rect(rect(0.0, 0.0, 0.5, 0.5), 1);
        assert!((back.x - 0.0).abs() < 1e-9, "got {back:?}");
        assert!((back.y - 0.5).abs() < 1e-9, "got {back:?}");
        assert!((back.w - 0.5).abs() < 1e-9, "got {back:?}");
        assert!((back.h - 0.5).abs() < 1e-9, "got {back:?}");
        let r = rect(0.25, 0.125, 0.5, 0.25);
        for q in 0..4u8 {
            let back = unrotate_rect(unrotate_rect(r, q), 4 - q);
            assert!((back.x - r.x).abs() < 1e-9, "q={q} {back:?}");
            assert!((back.y - r.y).abs() < 1e-9, "q={q} {back:?}");
            assert!((back.w - r.w).abs() < 1e-9, "q={q} {back:?}");
            assert!((back.h - r.h).abs() < 1e-9, "q={q} {back:?}");
        }
    }
}
