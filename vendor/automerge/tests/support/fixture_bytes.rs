//! Decode byte-exact historical fixtures stored as LF hexadecimal text.
pub fn decode(text: &str) -> Result<Vec<u8>, std::num::ParseIntError> {
    text.split_ascii_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16))
        .collect()
}
