//! A bundled sample buffer so the editor opens onto real, recognisable
//! structured data instead of an empty canvas.
//!
//! The sample is a complete, valid PNG: the 8-byte signature, an `IHDR`
//! chunk describing a 16 × 16 truecolour image, a `tEXt` chunk, one `IDAT`
//! chunk holding the pixels (a zlib stream of stored blocks, with its real
//! Adler-32) and `IEND`, each chunk carrying its real CRC-32. The ASCII
//! column reveals the chunk tags, and parking the cursor on the width field
//! makes the data inspector read the big-endian `u32` dimensions that the
//! PNG format uses.

/// Image width and height of the sample, in pixels.
const SIDE: u8 = 16;

/// CRC-32 (ISO 3309) as the PNG format checksums each chunk.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Adler-32 as the zlib stream ends with.
fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1_u32, 0_u32);
    for &byte in bytes {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// Append one chunk: length, tag, data, and the CRC over tag and data.
fn chunk(out: &mut Vec<u8>, tag: [u8; 4], data: &[u8]) {
    let length = u32::try_from(data.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    let mut checked = tag.to_vec();
    checked.extend_from_slice(data);
    out.extend_from_slice(&checked);
    out.extend_from_slice(&crc32(&checked).to_be_bytes());
}

/// The pixels as PNG scanlines (a filter byte, then RGB per pixel): red
/// grows to the right, green downwards, blue is fixed.
fn scanlines() -> Vec<u8> {
    let mut raw = Vec::with_capacity(usize::from(SIDE) * (1 + 3 * usize::from(SIDE)));
    for y in 0..SIDE {
        raw.push(0);
        for x in 0..SIDE {
            raw.extend_from_slice(&[x * 16, y * 16, 0x80]);
        }
    }
    raw
}

/// `raw` as a zlib stream made of one stored (uncompressed) deflate block.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let length = u16::try_from(raw.len()).unwrap_or(u16::MAX);
    let mut out = vec![0x78, 0x01, 0x01];
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(&(!length).to_le_bytes());
    out.extend_from_slice(raw);
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

/// Build the bundled sample byte buffer.
pub fn png_image() -> Vec<u8> {
    let mut b = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    let mut header = Vec::new();
    header.extend_from_slice(&u32::from(SIDE).to_be_bytes()); // width
    header.extend_from_slice(&u32::from(SIDE).to_be_bytes()); // height
    header.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB, no interlace
    chunk(&mut b, *b"IHDR", &header);

    chunk(&mut b, *b"tEXt", b"Title\0Colour gradient");
    chunk(&mut b, *b"IDAT", &zlib_stored(&scanlines()));
    chunk(&mut b, *b"IEND", &[]);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_image_is_a_complete_png_with_real_checksums() {
        let b = png_image();
        assert_eq!(&b[..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        // IHDR tag follows the 8-byte signature + 4-byte length.
        assert_eq!(&b[12..16], b"IHDR");
        // Width/height fields are big-endian 16.
        assert_eq!(&b[16..20], &16u32.to_be_bytes());
        assert_eq!(&b[20..24], &16u32.to_be_bytes());
        let len = b.len();
        assert_eq!(&b[len - 8..len - 4], b"IEND");
        assert_eq!(&b[len - 4..], &[0xAE, 0x42, 0x60, 0x82]);
        assert_eq!(&b[29..33], &crc32(&b[12..29]).to_be_bytes());
    }

    #[test]
    fn checksums_match_their_reference_values() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }
}
