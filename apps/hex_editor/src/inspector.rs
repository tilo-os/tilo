//! Data inspector — interpret the bytes at the cursor as the common
//! scalar types, in either endianness, plus a couple of binary-format
//! niceties (binary / octal of the first byte, and a LEB128 varint).
//!
//! The inspector is pure: it takes a byte slice and an offset and yields
//! a list of `(label, value, byte_width)` rows. `byte_width` lets the
//! view highlight exactly the bytes a given interpretation consumes.

use crate::model::Endian;

/// One row of the inspector table.
#[derive(Clone, Debug, PartialEq)]
pub struct Interpretation {
    /// Display name, e.g. "u16", "f32", "uleb128".
    pub label: &'static str,
    /// Rendered value, or "—" when there aren't enough bytes.
    pub value: String,
    /// How many bytes this interpretation reads starting at the cursor.
    /// 0 when not enough bytes were available.
    pub width: usize,
}

/// Read up to `n` bytes starting at `offset` into a fixed array,
/// returning None if the slice is too short.
fn read_n(bytes: &[u8], offset: usize, n: usize) -> Option<Vec<u8>> {
    if offset + n <= bytes.len() {
        Some(bytes[offset..offset + n].to_vec())
    } else {
        None
    }
}

/// Order bytes per endianness so we can always treat them little-endian
/// after this normalisation.
fn ordered(mut raw: Vec<u8>, endian: Endian) -> Vec<u8> {
    if endian == Endian::Big {
        raw.reverse();
    }
    raw
}

macro_rules! int_row {
    ($label:literal, $bytes:expr, $offset:expr, $endian:expr, $n:expr, $ty:ty, $from:path) => {{
        match read_n($bytes, $offset, $n) {
            Some(raw) => {
                let ord = ordered(raw, $endian);
                let mut arr = [0u8; $n];
                arr.copy_from_slice(&ord);
                let v = <$ty>::from_le_bytes(arr);
                Interpretation {
                    label: $label,
                    value: v.to_string(),
                    width: $n,
                }
            }
            None => Interpretation {
                label: $label,
                value: "—".to_string(),
                width: 0,
            },
        }
    }};
}

/// Decode an unsigned LEB128 varint starting at `offset`. Returns the
/// decoded value and the number of bytes consumed, or None if the slice
/// ends mid-varint (continuation bit set on the last available byte) or
/// the value overflows u64.
pub fn decode_uleb128(bytes: &[u8], offset: usize) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    let mut consumed = 0usize;
    for &b in &bytes[offset..] {
        consumed += 1;
        let low = u64::from(b & 0x7F);
        // Guard against shifting past the width of u64.
        if shift >= 64 {
            return None;
        }
        result |= low << shift;
        if b & 0x80 == 0 {
            return Some((result, consumed));
        }
        shift += 7;
    }
    None
}

/// Decode a signed LEB128 varint. Sign-extends the final group.
pub fn decode_sleb128(bytes: &[u8], offset: usize) -> Option<(i64, usize)> {
    let mut result: i64 = 0;
    let mut shift: u32 = 0;
    let mut consumed = 0usize;
    for &b in &bytes[offset..] {
        consumed += 1;
        if shift >= 64 {
            return None;
        }
        let low = i64::from(b & 0x7F);
        result |= low << shift;
        shift += 7;
        if b & 0x80 == 0 {
            // Sign-extend if the sign bit of this group is set.
            if shift < 64 && (b & 0x40) != 0 {
                result |= -1i64 << shift;
            }
            return Some((result, consumed));
        }
    }
    None
}

/// Produce the full inspector table for the cursor at `offset`.
pub fn inspect(bytes: &[u8], offset: usize, endian: Endian) -> Vec<Interpretation> {
    let mut rows = Vec::new();

    // Single-byte views.
    match bytes.get(offset).copied() {
        Some(b) => {
            rows.push(Interpretation {
                label: "i8",
                value: (b as i8).to_string(),
                width: 1,
            });
            rows.push(Interpretation {
                label: "u8",
                value: b.to_string(),
                width: 1,
            });
            rows.push(Interpretation {
                label: "bin",
                value: format!("{b:08b}"),
                width: 1,
            });
            rows.push(Interpretation {
                label: "oct",
                value: format!("0o{b:o}"),
                width: 1,
            });
            rows.push(Interpretation {
                label: "char",
                value: printable_char(b),
                width: 1,
            });
        }
        None => {
            for label in ["i8", "u8", "bin", "oct", "char"] {
                rows.push(Interpretation {
                    label,
                    value: "—".to_string(),
                    width: 0,
                });
            }
        }
    }

    // Multi-byte integers (endianness-aware).
    rows.push(int_row!(
        "i16",
        bytes,
        offset,
        endian,
        2,
        i16,
        i16::from_le_bytes
    ));
    rows.push(int_row!(
        "u16",
        bytes,
        offset,
        endian,
        2,
        u16,
        u16::from_le_bytes
    ));
    rows.push(int_row!(
        "i32",
        bytes,
        offset,
        endian,
        4,
        i32,
        i32::from_le_bytes
    ));
    rows.push(int_row!(
        "u32",
        bytes,
        offset,
        endian,
        4,
        u32,
        u32::from_le_bytes
    ));
    rows.push(int_row!(
        "i64",
        bytes,
        offset,
        endian,
        8,
        i64,
        i64::from_le_bytes
    ));
    rows.push(int_row!(
        "u64",
        bytes,
        offset,
        endian,
        8,
        u64,
        u64::from_le_bytes
    ));

    // Floats.
    rows.push(match read_n(bytes, offset, 4) {
        Some(raw) => {
            let ord = ordered(raw, endian);
            let mut arr = [0u8; 4];
            arr.copy_from_slice(&ord);
            Interpretation {
                label: "f32",
                value: format_float(f64::from(f32::from_le_bytes(arr))),
                width: 4,
            }
        }
        None => na("f32"),
    });
    rows.push(match read_n(bytes, offset, 8) {
        Some(raw) => {
            let ord = ordered(raw, endian);
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&ord);
            Interpretation {
                label: "f64",
                value: format_float(f64::from_le_bytes(arr)),
                width: 8,
            }
        }
        None => na("f64"),
    });

    // LEB128 varints (endianness does not apply — byte-stream encoding).
    rows.push(match decode_uleb128(bytes, offset) {
        Some((v, n)) => Interpretation {
            label: "uleb128",
            value: v.to_string(),
            width: n,
        },
        None => na("uleb128"),
    });
    rows.push(match decode_sleb128(bytes, offset) {
        Some((v, n)) => Interpretation {
            label: "sleb128",
            value: v.to_string(),
            width: n,
        },
        None => na("sleb128"),
    });

    rows
}

fn na(label: &'static str) -> Interpretation {
    Interpretation {
        label,
        value: "—".to_string(),
        width: 0,
    }
}

/// Format a float compactly: trim noise but keep enough precision to be
/// useful when eyeballing a buffer.
fn format_float(v: f64) -> String {
    if v == 0.0 {
        "0".to_string()
    } else if v.is_nan() {
        "NaN".to_string()
    } else if v.is_infinite() {
        if v > 0.0 {
            "+inf".to_string()
        } else {
            "-inf".to_string()
        }
    } else if v.abs() >= 1e-4 && v.abs() < 1e9 {
        // Plain decimal with up to 6 significant fractional digits.
        let s = format!("{v:.6}");
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_string()
    } else {
        format!("{v:e}")
    }
}

/// Render a byte as a quoted printable char, or its escape for control
/// bytes so the inspector never emits a glyph that breaks the layout.
fn printable_char(b: u8) -> String {
    match b {
        0x20..=0x7E => format!("'{}'", b as char),
        b'\n' => "'\\n'".to_string(),
        b'\r' => "'\\r'".to_string(),
        b'\t' => "'\\t'".to_string(),
        0 => "'\\0'".to_string(),
        _ => format!("\\x{b:02x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u16_respects_endianness() {
        let bytes = [0x34, 0x12];
        let le = inspect(&bytes, 0, Endian::Little);
        let be = inspect(&bytes, 0, Endian::Big);
        let le_u16 = le.iter().find(|r| r.label == "u16").unwrap();
        let be_u16 = be.iter().find(|r| r.label == "u16").unwrap();
        assert_eq!(le_u16.value, "4660"); // 0x1234
        assert_eq!(be_u16.value, "13330"); // 0x3412
    }

    #[test]
    fn i8_sign_extends() {
        let bytes = [0xFF];
        let rows = inspect(&bytes, 0, Endian::Little);
        let i8 = rows.iter().find(|r| r.label == "i8").unwrap();
        let u8 = rows.iter().find(|r| r.label == "u8").unwrap();
        assert_eq!(i8.value, "-1");
        assert_eq!(u8.value, "255");
    }

    #[test]
    fn binary_and_octal_of_byte() {
        let bytes = [0b1010_0101];
        let rows = inspect(&bytes, 0, Endian::Little);
        assert_eq!(
            rows.iter().find(|r| r.label == "bin").unwrap().value,
            "10100101"
        );
        assert_eq!(
            rows.iter().find(|r| r.label == "oct").unwrap().value,
            "0o245"
        );
    }

    #[test]
    fn f32_roundtrips_one() {
        let bytes = 1.0f32.to_le_bytes();
        let rows = inspect(&bytes, 0, Endian::Little);
        assert_eq!(rows.iter().find(|r| r.label == "f32").unwrap().value, "1");
    }

    #[test]
    fn short_buffer_yields_dashes_for_wide_types() {
        let bytes = [0x01]; // only one byte
        let rows = inspect(&bytes, 0, Endian::Little);
        let u32 = rows.iter().find(|r| r.label == "u32").unwrap();
        assert_eq!(u32.value, "—");
        assert_eq!(u32.width, 0);
        // u8 is still fine.
        assert_eq!(rows.iter().find(|r| r.label == "u8").unwrap().value, "1");
    }

    #[test]
    fn uleb128_multi_byte() {
        // 624485 = 0xE5 0x8E 0x26 in ULEB128.
        let bytes = [0xE5, 0x8E, 0x26];
        let (v, n) = decode_uleb128(&bytes, 0).unwrap();
        assert_eq!(v, 624485);
        assert_eq!(n, 3);
    }

    #[test]
    fn sleb128_negative() {
        // -123456 encodes as 0xC0 0xBB 0x78.
        let bytes = [0xC0, 0xBB, 0x78];
        let (v, n) = decode_sleb128(&bytes, 0).unwrap();
        assert_eq!(v, -123456);
        assert_eq!(n, 3);
    }

    #[test]
    fn uleb128_truncated_is_none() {
        // Continuation bit set with no following byte.
        let bytes = [0x80];
        assert!(decode_uleb128(&bytes, 0).is_none());
    }

    #[test]
    fn inspector_width_marks_consumed_bytes() {
        let bytes = [0, 0, 0, 0, 0, 0, 0, 0];
        let rows = inspect(&bytes, 0, Endian::Little);
        assert_eq!(rows.iter().find(|r| r.label == "u64").unwrap().width, 8);
        assert_eq!(rows.iter().find(|r| r.label == "u16").unwrap().width, 2);
    }
}
