//! Minimal standard-alphabet base64 encoder for image `data:` URIs.
//!
//! Hand-rolled (rather than pulling the `base64` crate) so adding `qdoc-html`
//! doesn't touch the workspace lockfile. Standard RFC 4648 alphabet with `=`
//! padding — exactly what a browser expects in a `data:image/png;base64,…` URI.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode `input` to a standard, padded base64 string.
pub fn encode(input: &[u8]) -> String {
    // Every 3 input bytes become 4 output chars; bounded by the input
    // blob's length, nowhere near `usize::MAX`.
    let mut out = String::with_capacity(input.len().div_ceil(3).saturating_mul(4));
    for chunk in input.chunks(3) {
        // `chunks(3)` never yields an empty chunk, so `first()` is always
        // `Some`; `.get()`/`.first()` just spell that out instead of `[]`.
        let b0 = u32::from(chunk.first().copied().unwrap_or(0));
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(sextet_char(n >> 18));
        out.push(sextet_char(n >> 12));
        if chunk.len() > 1 {
            out.push(sextet_char(n >> 6));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(sextet_char(n));
        } else {
            out.push('=');
        }
    }
    out
}

/// One base64 output character for the low 6 bits of `packed`.
/// `& 0x3f` always yields `0..64`, exactly `ALPHABET`'s length, and every
/// entry in `ALPHABET` is ASCII, so `as char` cannot corrupt anything —
/// but `.get()` spells the bound out instead of indexing, and this is the
/// one place that needs to.
fn sextet_char(packed: u32) -> char {
    let sextet = (packed & 0x3f) as usize;
    ALPHABET.get(sextet).copied().unwrap_or(b'?') as char
}

#[cfg(test)]
mod tests {
    use super::encode;

    #[test]
    fn known_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn binary_bytes_round_alphabet() {
        // Full-byte coverage: 0xff,0x00,0x80 → exercises all bit positions.
        assert_eq!(encode(&[0xff, 0x00, 0x80]), "/wCA");
        assert_eq!(encode(&[0x00]), "AA==");
    }
}
