//! A small bundled sample buffer so the editor opens onto real,
//! recognisable structured data instead of an empty canvas.
//!
//! The sample is the head of a valid PNG file: the 8-byte signature,
//! the `IHDR` chunk describing a 2×2 truecolour-with-alpha image, the
//! start of an `IDAT` chunk, and an `IEND`. It is deliberately compact
//! (a few rows of hex) yet shows off the editor: the ASCII column reveals
//! the `PNG`, `IHDR`, `IDAT`, `IEND` chunk tags, and parking the cursor on
//! the width/height fields makes the data inspector light up with the
//! big-endian u32 dimensions that the PNG format uses.

/// Build the bundled sample byte buffer.
pub fn png_header() -> Vec<u8> {
    let mut b = Vec::new();

    // --- 8-byte PNG signature ---
    b.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

    // --- IHDR chunk ---
    // length = 13
    b.extend_from_slice(&13u32.to_be_bytes());
    b.extend_from_slice(b"IHDR");
    b.extend_from_slice(&2u32.to_be_bytes()); // width  = 2
    b.extend_from_slice(&2u32.to_be_bytes()); // height = 2
    b.push(8); // bit depth
    b.push(6); // colour type: truecolour + alpha
    b.push(0); // compression
    b.push(0); // filter
    b.push(0); // interlace
    b.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]); // (illustrative CRC)

    // --- IDAT chunk (truncated, illustrative) ---
    b.extend_from_slice(&16u32.to_be_bytes());
    b.extend_from_slice(b"IDAT");
    b.extend_from_slice(&[
        0x78, 0x9C, 0x62, 0x60, 0x60, 0x60, 0xF8, 0xCF, 0xC0, 0xC0, 0xC0, 0x00, 0xC4, 0x80, 0x00,
        0x00,
    ]);
    b.extend_from_slice(&[0xAB, 0xCD, 0xEF, 0x01]); // (illustrative CRC)

    // --- IEND chunk ---
    b.extend_from_slice(&0u32.to_be_bytes());
    b.extend_from_slice(b"IEND");
    b.extend_from_slice(&[0xAE, 0x42, 0x60, 0x82]); // real IEND CRC

    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_header_has_signature_chunk_tags_and_iend() {
        let b = png_header();
        assert_eq!(&b[..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        // IHDR tag follows the 8-byte signature + 4-byte length.
        assert_eq!(&b[12..16], b"IHDR");
        // Width/height fields are big-endian 2.
        assert_eq!(&b[16..20], &2u32.to_be_bytes());
        assert_eq!(&b[20..24], &2u32.to_be_bytes());
        let len = b.len();
        assert_eq!(&b[len - 8..len - 4], b"IEND");
    }
}
