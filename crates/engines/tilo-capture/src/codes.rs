//! The codes a frame holds: QR and barcode detection over a picture's pixels, so an app that
//! scans codes asks the device instead of linking a detector of its own.
//!
//! The detector reads a grey buffer (`width * height` bytes, row-major, 0 black) and answers
//! every code it can decode with the text it says and the four corners of the code in that
//! buffer, clockwise from the top left. A frame is greyed by [`grey`] first: one pass over the
//! RGBA picture, no decode and no allocation beyond the buffer.

use crate::Frame;

/// One code the picture holds: what it says, and where it stands in the picture the detector
/// read, clockwise from the top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Code {
    pub text: String,
    pub corners: [(f32, f32); 4],
}

/// The grey buffer of `frame`: its luminance, as the detector reads it.
#[must_use]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "three u8 weights summed into a u32 cannot pass u32::MAX"
)]
pub fn grey(frame: &Frame) -> Vec<u8> {
    frame
        .rgba
        .chunks_exact(4)
        .map(|pixel| match pixel {
            [r, g, b, _] => {
                let (r, g, b) = (u32::from(*r), u32::from(*g), u32::from(*b));
                u8::try_from((r * 299 + g * 587 + b * 114) / 1000).unwrap_or(255)
            }
            _ => 255,
        })
        .collect()
}

/// Every code `luma` holds, decoded for real.
///
/// A buffer that is not `width * height` bytes, or a picture with no side, holds no code.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "a corner is a pixel position in a picture at most 16 megapixels, which f32               represents exactly"
)]
pub fn decode(luma: &[u8], width: usize, height: usize) -> Vec<Code> {
    if width == 0 || height == 0 || luma.len() < width.saturating_mul(height) {
        return Vec::new();
    }
    #[cfg(codes)]
    {
        let mut picture = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
            luma.get(y.saturating_mul(width).saturating_add(x))
                .copied()
                .unwrap_or(0)
        });
        let mut found = Vec::new();
        for grid in picture.detect_grids() {
            let Ok((_meta, text)) = grid.decode() else {
                continue;
            };
            let c = grid.bounds;
            found.push(Code {
                text,
                corners: [
                    (c[0].x as f32, c[0].y as f32),
                    (c[1].x as f32, c[1].y as f32),
                    (c[2].x as f32, c[2].y as f32),
                    (c[3].x as f32, c[3].y as f32),
                ],
            });
        }
        found
    }
    #[cfg(not(codes))]
    {
        let _ = (luma, width, height);
        Vec::new()
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "the test draws a code of at most a few hundred pixels"
)]
mod tests {
    use super::*;

    /// The smallest picture a code needs: its modules, each `scale` pixels, with a quiet zone.
    fn decode_rendered(payload: &str) -> Vec<Code> {
        let code = qrcode::QrCode::new(payload.as_bytes()).expect("encode");
        let size = code.width();
        let scale = 6;
        let quiet = 4;
        let dim = (size + quiet * 2) * scale;
        let mut luma = vec![255_u8; dim * dim];
        let colors = code.to_colors();
        for y in 0..size {
            for x in 0..size {
                if colors[y * size + x] != qrcode::Color::Dark {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = (x + quiet) * scale + dx;
                        let py = (y + quiet) * scale + dy;
                        luma[py * dim + px] = 0;
                    }
                }
            }
        }
        decode(&luma, dim, dim)
    }

    #[test]
    fn a_rendered_code_is_found_with_its_text_and_corners() {
        let found = decode_rendered("https://example.test/welcome");
        assert_eq!(found.len(), 1, "exactly one code");
        assert_eq!(found[0].text, "https://example.test/welcome");
        for (x, y) in found[0].corners {
            assert!(x >= 0.0 && y >= 0.0, "a corner stands in the picture");
        }
    }

    #[test]
    fn a_blank_or_malformed_buffer_holds_no_code() {
        assert!(decode(&vec![255_u8; 64 * 64], 64, 64).is_empty());
        assert!(decode(&[], 0, 0).is_empty());
        assert!(decode(&[0_u8; 10], 64, 64).is_empty());
    }

    #[test]
    fn a_frames_grey_is_its_luminance() {
        let frame = Frame {
            width: 2,
            height: 1,
            rgba: vec![255, 255, 255, 255, 0, 0, 0, 255],
        };
        assert_eq!(grey(&frame), vec![255, 0]);
    }
}
