use serde::Deserialize;
use serde_json::value::RawValue;

use crate::{CodecError, MAX_FRAME_BODY_BYTES};

const UNIX_PREFIX: &str = r#"{"encoding":"unix_bytes","hex":""#;
const WINDOWS_PREFIX: &str = r#"{"encoding":"windows_utf16","hex":""#;
const SUFFIX: &str = r#""}"#;

/// Lossless native string data. This inert value never grants path access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeString {
    /// Unix bytes, including arbitrary non-UTF-8 and NUL bytes.
    UnixBytes(Vec<u8>),
    /// Windows UTF-16 code units, including unpaired surrogates and NUL.
    WindowsUtf16(Vec<u16>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireNativeString {
    encoding: String,
    hex: String,
}

impl NativeString {
    /// Decodes the strict standalone tagged JSON representation.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::FrameTooLarge`] before parsing oversized input,
    /// or [`CodecError::InvalidNativeString`] for any noncanonical encoding.
    pub fn decode_json(input: &[u8]) -> Result<Self, CodecError> {
        // Bound before UTF-8, syntax, depth, or hex conversion work.
        if input.len() > MAX_FRAME_BODY_BYTES {
            return Err(CodecError::FrameTooLarge);
        }
        let text = std::str::from_utf8(input).map_err(|error| CodecError::InvalidNativeString {
            offset: error.valid_up_to(),
        })?;
        let syntax: Box<RawValue> =
            serde_json::from_str(text).map_err(|error| CodecError::InvalidNativeString {
                offset: native_error_offset(text, &error),
            })?;
        if !has_object_root(syntax.get()) {
            return Err(CodecError::InvalidNativeString { offset: 0 });
        }
        let wire: WireNativeString =
            serde_json::from_str(text).map_err(|error| CodecError::InvalidNativeString {
                offset: native_error_offset(text, &error),
            })?;
        match wire.encoding.as_str() {
            "unix_bytes" => decode_unix(&wire.hex),
            "windows_utf16" => decode_windows(&wire.hex),
            _ => Err(CodecError::InvalidNativeString { offset: 0 }),
        }
    }

    /// Encodes the strict standalone tagged JSON representation.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::FrameTooLarge`] when checked hex expansion and
    /// tagged JSON metadata cannot fit within the fixed body budget.
    pub fn encode_json(&self) -> Result<Vec<u8>, CodecError> {
        match self {
            Self::UnixBytes(bytes) => {
                let hex_len = bytes
                    .len()
                    .checked_mul(2)
                    .ok_or(CodecError::FrameTooLarge)?;
                ensure_encoded_size(UNIX_PREFIX.len(), hex_len)?;
                let encoded = hex::encode(bytes);
                assemble(UNIX_PREFIX, &encoded)
            }
            Self::WindowsUtf16(units) => {
                let hex_len = units
                    .len()
                    .checked_mul(4)
                    .ok_or(CodecError::FrameTooLarge)?;
                ensure_encoded_size(WINDOWS_PREFIX.len(), hex_len)?;
                let mut encoded = String::with_capacity(hex_len);
                for unit in units {
                    push_u16_hex(&mut encoded, *unit)?;
                }
                assemble(WINDOWS_PREFIX, &encoded)
            }
        }
    }
}

fn has_object_root(raw: &str) -> bool {
    raw.as_bytes()
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'{')
}

fn ensure_encoded_size(prefix_len: usize, hex_len: usize) -> Result<(), CodecError> {
    let encoded_len = prefix_len
        .checked_add(hex_len)
        .and_then(|length| length.checked_add(SUFFIX.len()))
        .ok_or(CodecError::FrameTooLarge)?;
    if encoded_len > MAX_FRAME_BODY_BYTES {
        Err(CodecError::FrameTooLarge)
    } else {
        Ok(())
    }
}

fn assemble(prefix: &str, encoded: &str) -> Result<Vec<u8>, CodecError> {
    let capacity = prefix
        .len()
        .checked_add(encoded.len())
        .and_then(|length| length.checked_add(SUFFIX.len()))
        .ok_or(CodecError::FrameTooLarge)?;
    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(prefix.as_bytes());
    output.extend_from_slice(encoded.as_bytes());
    output.extend_from_slice(SUFFIX.as_bytes());
    Ok(output)
}

fn decode_unix(encoded: &str) -> Result<NativeString, CodecError> {
    validate_lower_hex(encoded.as_bytes(), 2)?;
    let bytes = hex::decode(encoded).map_err(|_| CodecError::InvalidNativeString { offset: 0 })?;
    Ok(NativeString::UnixBytes(bytes))
}

fn decode_windows(encoded: &str) -> Result<NativeString, CodecError> {
    validate_lower_hex(encoded.as_bytes(), 4)?;
    let capacity = encoded.len().checked_div(4).unwrap_or(0);
    let mut units = Vec::with_capacity(capacity);
    for (chunk_index, chunk) in encoded.as_bytes().chunks_exact(4).enumerate() {
        let mut value = 0_u16;
        for (digit_index, byte) in chunk.iter().copied().enumerate() {
            let digit = hex_digit(byte).ok_or_else(|| CodecError::InvalidNativeString {
                offset: chunk_index
                    .checked_mul(4)
                    .and_then(|offset| offset.checked_add(digit_index))
                    .unwrap_or(0),
            })?;
            value = value
                .checked_mul(16)
                .and_then(|partial| partial.checked_add(u16::from(digit)))
                .ok_or(CodecError::InvalidNativeString { offset: 0 })?;
        }
        units.push(value);
    }
    Ok(NativeString::WindowsUtf16(units))
}

fn validate_lower_hex(bytes: &[u8], unit_width: usize) -> Result<(), CodecError> {
    if bytes.len().checked_rem(unit_width) != Some(0) {
        return Err(CodecError::InvalidNativeString {
            offset: bytes.len(),
        });
    }
    if let Some((offset, _)) = bytes
        .iter()
        .enumerate()
        .find(|(_, byte)| hex_digit(**byte).is_none())
    {
        return Err(CodecError::InvalidNativeString { offset });
    }
    Ok(())
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.saturating_sub(b'0')),
        b'a'..=b'f' => Some(byte.saturating_sub(b'a').saturating_add(10)),
        _ => None,
    }
}

fn push_u16_hex(output: &mut String, value: u16) -> Result<(), CodecError> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for shift in [12_u32, 8, 4, 0] {
        let nibble = usize::from((value >> shift) & 0x000f);
        let byte = DIGITS
            .get(nibble)
            .copied()
            .ok_or(CodecError::InvalidNativeString { offset: 0 })?;
        output.push(char::from(byte));
    }
    Ok(())
}

fn native_error_offset(text: &str, error: &serde_json::Error) -> usize {
    if error.is_eof() {
        text.len()
    } else {
        error.column().saturating_sub(1)
    }
}
