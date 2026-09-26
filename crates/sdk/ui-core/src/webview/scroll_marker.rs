//! Benchmark-only visual acknowledgement from the controlled scroll fixture.
//! The marker is 32 four-pixel cells at the page's physical top-left: magic,
//! signed wheel delta (big endian), then an XOR check byte. No page text is read.
pub(super) fn decode(rgba: &[u8], width: u32, height: u32) -> Option<i16> {
    if width < 128 || height < 4 {
        return None;
    }
    let stride = usize::try_from(width).ok()?.checked_mul(4)?;
    let size = stride.checked_mul(usize::try_from(height).ok()?)?;
    if rgba.len() != size {
        return None;
    }
    let row = rgba.chunks_exact(stride).nth(2)?.get(..512)?;
    let mut bytes = [0u8; 4];
    for (byte, cells) in bytes.iter_mut().zip(row.chunks_exact(128)) {
        for cell in cells.chunks_exact(16) {
            let [red, green, blue, alpha] = cell.get(8..12)? else {
                return None;
            };
            if *alpha != 255 {
                return None;
            }
            let channels = [*red, *green, *blue];
            let value = if channels.iter().all(|v| *v >= 250) {
                1
            } else if channels.iter().all(|v| *v <= 5) {
                0
            } else {
                return None;
            };
            *byte = (*byte << 1) | value;
        }
    }
    let [magic, high, low, check] = bytes;
    if magic != 0xb3 || check != magic ^ high ^ low {
        return None;
    }
    let delta = i16::from_be_bytes([high, low]);
    (delta != 0).then_some(delta)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(bytes: [u8; 4]) -> Vec<u8> {
        let mut pixels = vec![0; 128 * 4 * 4];
        for bit in 0..32 {
            let color = if bytes[bit / 8] & (1 << (7 - bit % 8)) != 0 {
                255
            } else {
                0
            };
            for y in 0..4 {
                for x in bit * 4..bit * 4 + 4 {
                    let offset = (y * 128 + x) * 4;
                    pixels[offset..offset + 4].copy_from_slice(&[color, color, color, 255]);
                }
            }
        }
        pixels
    }
    #[test]
    fn scroll_markers_decode_signed_acknowledgements_and_reject_corruption() {
        // phase: decodes_signed_scroll_acknowledgement_from_pixels
        {
            assert_eq!(decode(&frame([0xb3, 0, 65, 0xf2]), 128, 4), Some(65));
            assert_eq!(decode(&frame([0xb3, 255, 191, 0xf3]), 128, 4), Some(-65));
        }
        // phase: rejects_baseline_corruption_transparency_and_truncated_frames
        {
            for bytes in [[0xb3, 0, 0, 0xb3], [0xb2, 0, 65, 0xf3], [0xb3, 0, 65, 0]] {
                assert_eq!(decode(&frame(bytes), 128, 4), None);
            }
            let mut pixels = frame([0xb3, 0, 65, 0xf2]);
            pixels[(2 * 128 + 2) * 4 + 3] = 0;
            assert_eq!(decode(&pixels, 128, 4), None);
            assert_eq!(decode(&pixels[..5], 128, 4), None);
            assert_eq!(decode(&pixels, 127, 4), None);
            assert_eq!(decode(&pixels, u32::MAX, u32::MAX), None);
        }
    }
}
