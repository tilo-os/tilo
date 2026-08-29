//! The pixel pipeline: decode, crop, rotate, encode.
//!
//! Everything here is a pure function over RGBA8 buffers. No widget, no
//! document, no clock — which is what lets the edit operations be asserted on
//! their output bytes rather than on a flag that claims they ran.
//!
//! # The pipeline, and why the order is fixed
//!
//! A rendered frame is always `decode → crop → rotate`:
//!
//! ```text
//!   original RGBA  ──crop(Rect01 in ORIGINAL coords)──▶  cropped  ──rotate(N×90° CW)──▶  shown
//! ```
//!
//! Crop lives in the *original* image's normalized coordinates, never in the
//! rotated frame. That is the whole reason rotation stays a free operation: a
//! stored crop keeps meaning exactly the same region of the photograph no
//! matter how many times the image is subsequently turned. Store the crop in
//! rotated space instead and every rotation has to rewrite it, which is a
//! migration on a CRDT field that two peers can be editing at once.
//!
//! The cost is that a crop *taken* from the rotated view has to be mapped back
//! through the rotation before it is stored — [`unrotate_rect`] is that map,
//! and it is exercised at every quarter turn by the tests below.

use crate::model::Rect01;

/// A decoded image: RGBA8, tightly packed, `w * h * 4` bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Rgba {
    pub w: u32,
    pub h: u32,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Rgba {
    /// Deliberately does not print the buffer: a failing assertion on a
    /// megapixel image would otherwise emit megabytes into the test log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rgba({}x{}, {} bytes)", self.w, self.h, self.bytes.len())
    }
}

impl Rgba {
    /// Wrap a buffer, checking the length matches the dimensions.
    pub fn new(w: u32, h: u32, bytes: Vec<u8>) -> Option<Self> {
        if w == 0 || h == 0 || bytes.len() != (w as usize) * (h as usize) * 4 {
            return None;
        }
        Some(Self { w, h, bytes })
    }

    /// The pixel at `(x, y)` as `[r, g, b, a]`. `None` when out of bounds —
    /// used by tests to sample a region rather than compare whole buffers.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.w || y >= self.h {
            return None;
        }
        let i = ((y as usize) * (self.w as usize) + (x as usize)) * 4;
        Some([
            self.bytes[i],
            self.bytes[i + 1],
            self.bytes[i + 2],
            self.bytes[i + 3],
        ])
    }
}

/// Decode any format the build enabled (PNG, JPEG, WebP) into RGBA8.
///
/// Returns `None` for bytes that are not a decodable image, rather than
/// panicking: this runs on whatever the file chooser handed back, and a user
/// picking a text file must produce an honest error, not a crash.
pub fn decode(bytes: &[u8]) -> Option<Rgba> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Rgba::new(w, h, rgba.into_raw())
}

/// Encode RGBA8 as a PNG. This is what reaches the clipboard consumer that
/// wants a file rather than a raw buffer, and what `Save` writes.
pub fn encode_png(img: &Rgba) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.w, img.h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(&img.bytes).ok()?;
    }
    Some(out)
}

/// Crop to `rect`, expressed in normalized `0..1` coordinates of `src`.
///
/// The rect is clamped into the unit square and the resulting pixel rect is
/// forced to at least one pixel in each axis, so a degenerate drag can never
/// produce a zero-sized image (which would then fail to encode and read to the
/// user as "the crop deleted my photo").
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
        bytes.extend_from_slice(&src.bytes[start..end]);
    }
    Rgba { w, h, bytes }
}

/// Rotate by `quarters` × 90° clockwise. `quarters` is taken modulo 4, so a
/// caller may pass an ever-increasing counter.
pub fn rotate(src: &Rgba, quarters: u8) -> Rgba {
    match quarters % 4 {
        0 => src.clone(),
        1 => rotate_cw(src),
        2 => rotate_cw(&rotate_cw(src)),
        _ => rotate_cw(&rotate_cw(&rotate_cw(src))),
    }
}

/// One 90° clockwise turn. Destination `(x, y)` reads source
/// `(y, src.h - 1 - x)`.
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
            bytes[di..di + 4].copy_from_slice(&src.bytes[si..si + 4]);
        }
    }
    Rgba {
        w: dw as u32,
        h: dh as u32,
        bytes,
    }
}

/// Run the whole pipeline: crop in original coordinates, then rotate.
pub fn render(original: &Rgba, crop_rect: Rect01, quarters: u8) -> Rgba {
    let cropped = if crop_rect.is_full() {
        original.clone()
    } else {
        crop(original, crop_rect)
    };
    rotate(&cropped, quarters)
}

/// Map a rect taken from the *rotated* view back into the un-rotated frame.
///
/// The inverse of one clockwise turn sends a unit-square point `(x, y)` to
/// `(y, 1 - x)`; applied to a rect's corners and re-normalised, that is the
/// transform below, iterated `quarters` times.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4x2 image whose every pixel is distinguishable: red channel encodes
    /// the column, green the row. Any transform that loses or mirrors a pixel
    /// shows up as a wrong `[r, g]` pair rather than as a size that happens to
    /// match.
    fn ramp(w: u32, h: u32) -> Rgba {
        let mut bytes = Vec::new();
        for y in 0..h {
            for x in 0..w {
                bytes.extend_from_slice(&[x as u8, y as u8, 0, 255]);
            }
        }
        Rgba::new(w, h, bytes).unwrap()
    }

    #[test]
    fn crop_takes_the_requested_region_and_nothing_else() {
        let src = ramp(4, 4);
        // The bottom-right quadrant: columns 2..4, rows 2..4.
        let out = crop(
            &src,
            Rect01 {
                x: 0.5,
                y: 0.5,
                w: 0.5,
                h: 0.5,
            },
        );
        assert_eq!((out.w, out.h), (2, 2));
        // Pixel (0,0) of the crop must be pixel (2,2) of the source.
        assert_eq!(out.pixel(0, 0).unwrap()[0..2], [2, 2]);
        assert_eq!(out.pixel(1, 1).unwrap()[0..2], [3, 3]);
        // And the byte count must match the new size exactly — a crop that
        // kept the original buffer would still pass a dimension check.
        assert_eq!(out.bytes.len(), 2 * 2 * 4);
    }

    #[test]
    fn crop_of_a_degenerate_rect_still_yields_a_real_pixel() {
        let src = ramp(4, 4);
        let out = crop(
            &src,
            Rect01 {
                x: 0.5,
                y: 0.5,
                w: 0.0,
                h: 0.0,
            },
        );
        assert_eq!((out.w, out.h), (1, 1), "a zero-area crop must not vanish");
        assert_eq!(out.bytes.len(), 4);
    }

    #[test]
    fn rotate_quarter_turn_swaps_the_axes_and_moves_the_corner() {
        let src = ramp(4, 2);
        let out = rotate(&src, 1);
        assert_eq!((out.w, out.h), (2, 4), "90 degrees swaps width and height");
        // The source's bottom-left pixel (0, 1) becomes the destination's
        // top-left after one clockwise turn.
        assert_eq!(out.pixel(0, 0).unwrap()[0..2], [0, 1]);
        // The source's top-left (0, 0) lands top-right.
        assert_eq!(out.pixel(1, 0).unwrap()[0..2], [0, 0]);
    }

    #[test]
    fn four_quarter_turns_are_the_identity() {
        let src = ramp(5, 3);
        assert_eq!(rotate(&src, 4), src);
        assert_eq!(rotate(&src, 0), src);
    }

    #[test]
    fn rotate_is_not_a_no_op_on_a_square_image() {
        // The dimension check cannot catch a broken square rotation, so assert
        // the content moved.
        let src = ramp(3, 3);
        let out = rotate(&src, 1);
        assert_eq!((out.w, out.h), (3, 3));
        assert_ne!(
            out.bytes, src.bytes,
            "a square rotation must still move pixels"
        );
        assert_eq!(out.pixel(0, 0).unwrap()[0..2], [0, 2]);
    }

    #[test]
    fn unrotate_is_the_inverse_of_rotating_the_view() {
        // Take the top-left quarter of a view that has been turned once, map
        // it back, and it must name the region that *is* top-left once the
        // image is turned — the bottom-left quarter of the original.
        let r = Rect01 {
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        };
        let back = unrotate_rect(r, 1);
        assert!((back.x - 0.0).abs() < 1e-9, "got {back:?}");
        assert!((back.y - 0.5).abs() < 1e-9, "got {back:?}");
        assert!((back.w - 0.5).abs() < 1e-9, "got {back:?}");
        assert!((back.h - 0.5).abs() < 1e-9, "got {back:?}");
    }

    #[test]
    fn unrotate_round_trips_through_every_quarter() {
        let r = Rect01 {
            x: 0.25,
            y: 0.125,
            w: 0.5,
            h: 0.25,
        };
        // Undoing q turns and then undoing (4 - q) more is a full circle.
        for q in 0..4u8 {
            let there = unrotate_rect(r, q);
            let back = unrotate_rect(there, 4 - q);
            assert!((back.x - r.x).abs() < 1e-9, "q={q} {back:?}");
            assert!((back.y - r.y).abs() < 1e-9, "q={q} {back:?}");
            assert!((back.w - r.w).abs() < 1e-9, "q={q} {back:?}");
            assert!((back.h - r.h).abs() < 1e-9, "q={q} {back:?}");
        }
    }

    #[test]
    fn render_composes_crop_then_rotate() {
        let src = ramp(4, 2);
        let out = render(
            &src,
            Rect01 {
                x: 0.0,
                y: 0.0,
                w: 0.5,
                h: 1.0,
            },
            1,
        );
        // Crop to the left half (2x2), then a quarter turn keeps it 2x2 but
        // moves the corner.
        assert_eq!((out.w, out.h), (2, 2));
        assert_eq!(out.pixel(0, 0).unwrap()[0..2], [0, 1]);
    }

    #[test]
    fn png_round_trips_through_decode() {
        let src = ramp(6, 4);
        let png = encode_png(&src).expect("encode");
        assert_eq!(&png[1..4], b"PNG", "not a PNG");
        let back = decode(&png).expect("decode");
        assert_eq!((back.w, back.h), (6, 4));
        assert_eq!(back.bytes, src.bytes, "the round trip changed pixels");
    }

    #[test]
    fn decode_refuses_bytes_that_are_not_an_image() {
        assert!(decode(b"this is not a picture").is_none());
        assert!(decode(&[]).is_none());
    }
}
