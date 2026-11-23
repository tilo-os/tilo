//! The one spelling of a segment.
//!
//! The RFC 3986 pchar set, minus `/` and minus `&` and `=` (which separate
//! parameters), stays raw. Every other byte is `%XX` with uppercase hex.
//! Decoding refuses anything the encoder would not have written, so two
//! different strings never name the same segment.

use super::LinkError;

fn is_raw(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~!$'()*+,;:@".contains(&byte)
}

fn hex_digit(nibble: u8) -> char {
    char::from(
        *b"0123456789ABCDEF"
            .get(usize::from(nibble & 0x0f))
            .unwrap_or(&b'0'),
    )
}

fn unhex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => byte.checked_sub(b'0'),
        b'A'..=b'F' => byte.checked_sub(b'A').and_then(|n| n.checked_add(10)),
        _ => None,
    }
}

/// Append `text` to `out` as one canonical segment.
pub(crate) fn push_segment(out: &mut String, text: &str) {
    for byte in text.bytes() {
        if is_raw(byte) {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(hex_digit(byte.wrapping_shr(4)));
            out.push(hex_digit(byte));
        }
    }
}

pub(crate) fn segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    push_segment(&mut out, text);
    out
}

/// Decode a segment written by [`push_segment`], refusing any other spelling.
pub(crate) fn decode_segment(text: &str) -> Result<String, LinkError> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut input = text.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let (Some(high), Some(low)) =
                (input.next().and_then(unhex), input.next().and_then(unhex))
            else {
                return Err(LinkError::Encoding);
            };
            let decoded = high.wrapping_mul(16) | low;
            if is_raw(decoded) {
                return Err(LinkError::NonCanonical(
                    "letters, digits and -._~!$'()*+,;:@ are never percent-encoded",
                ));
            }
            bytes.push(decoded);
        } else if is_raw(byte) {
            bytes.push(byte);
        } else {
            return Err(LinkError::Encoding);
        }
    }
    String::from_utf8(bytes).map_err(|_| LinkError::Encoding)
}
