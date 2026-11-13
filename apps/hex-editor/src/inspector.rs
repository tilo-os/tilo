//! The data inspector: the bytes at the cursor read as the common scalar types in
//! either endianness, plus binary and octal of the first byte and a LEB128 varint.
//! Pure: a byte slice and an offset become `(label, value, byte_width)` rows.

use crate::model::Endian;

#[derive(Clone, Debug, PartialEq)]
pub struct Interpretation {
    pub label: &'static str,
    pub value: String,
    /// How many bytes this interpretation reads at the cursor; 0 when too few were
    /// available.
    pub width: usize,
}

fn read_n(bytes: &[u8], offset: usize, n: usize) -> Option<Vec<u8>> {
    bytes.get(offset..offset + n).map(<[u8]>::to_vec)
}

/// Bytes in little-endian order, whatever the endianness.
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

/// An unsigned LEB128 varint at `offset`: the value and the bytes it took, or `None`
/// when the slice ends mid-varint or the value overflows.
pub fn decode_uleb128(bytes: &[u8], offset: usize) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    let mut consumed = 0usize;
    for &b in bytes.get(offset..)? {
        consumed += 1;
        let low = u64::from(b & 0x7F);
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

/// A signed LEB128 varint, the final group sign-extended.
pub fn decode_sleb128(bytes: &[u8], offset: usize) -> Option<(i64, usize)> {
    let mut result: i64 = 0;
    let mut shift: u32 = 0;
    let mut consumed = 0usize;
    for &b in bytes.get(offset..)? {
        consumed += 1;
        if shift >= 64 {
            return None;
        }
        let low = i64::from(b & 0x7F);
        result |= low << shift;
        shift += 7;
        if b & 0x80 == 0 {
            if shift < 64 && (b & 0x40) != 0 {
                result |= -1i64 << shift;
            }
            return Some((result, consumed));
        }
    }
    None
}

pub fn inspect(bytes: &[u8], offset: usize, endian: Endian) -> Vec<Interpretation> {
    let mut rows = single_byte_rows(bytes, offset);
    rows.extend(integer_rows(bytes, offset, endian));
    rows.extend(float_rows(bytes, offset, endian));
    rows.extend(leb128_rows(bytes, offset));
    rows
}

fn single_byte_rows(bytes: &[u8], offset: usize) -> Vec<Interpretation> {
    let mut rows = Vec::new();
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
    rows
}

fn integer_rows(bytes: &[u8], offset: usize, endian: Endian) -> Vec<Interpretation> {
    vec![
        int_row!("i16", bytes, offset, endian, 2, i16, i16::from_le_bytes),
        int_row!("u16", bytes, offset, endian, 2, u16, u16::from_le_bytes),
        int_row!("i32", bytes, offset, endian, 4, i32, i32::from_le_bytes),
        int_row!("u32", bytes, offset, endian, 4, u32, u32::from_le_bytes),
        int_row!("i64", bytes, offset, endian, 8, i64, i64::from_le_bytes),
        int_row!("u64", bytes, offset, endian, 8, u64, u64::from_le_bytes),
    ]
}

fn float_rows(bytes: &[u8], offset: usize, endian: Endian) -> Vec<Interpretation> {
    let f32_row = match read_n(bytes, offset, 4) {
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
    };
    let f64_row = match read_n(bytes, offset, 8) {
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
    };
    vec![f32_row, f64_row]
}

/// The varint views, where endianness does not apply.
fn leb128_rows(bytes: &[u8], offset: usize) -> Vec<Interpretation> {
    let uleb = match decode_uleb128(bytes, offset) {
        Some((v, n)) => Interpretation {
            label: "uleb128",
            value: v.to_string(),
            width: n,
        },
        None => na("uleb128"),
    };
    let sleb = match decode_sleb128(bytes, offset) {
        Some((v, n)) => Interpretation {
            label: "sleb128",
            value: v.to_string(),
            width: n,
        },
        None => na("sleb128"),
    };
    vec![uleb, sleb]
}

fn na(label: &'static str) -> Interpretation {
    Interpretation {
        label,
        value: "—".to_string(),
        width: 0,
    }
}

/// A float, compact: enough precision to read a buffer, without noise.
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
        let s = format!("{v:.6}");
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_string()
    } else {
        format!("{v:e}")
    }
}

/// A byte as a quoted printable char, or its escape for control bytes.
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

    type InspectCase<'a> = (
        &'a str,
        &'a [u8],
        Endian,
        &'a str,
        Option<&'a str>,
        Option<usize>,
    );

    #[test]
    fn inspect_rows_decode_values_and_consumed_width() {
        let one_le = 1.0f32.to_le_bytes();
        let cases: &[InspectCase<'_>] = &[
            (
                "u16 little",
                &[0x34, 0x12],
                Endian::Little,
                "u16",
                Some("4660"),
                None,
            ), // 0x1234
            (
                "u16 big",
                &[0x34, 0x12],
                Endian::Big,
                "u16",
                Some("13330"),
                None,
            ), // 0x3412
            (
                "i8 of 0xFF",
                &[0xFF],
                Endian::Little,
                "i8",
                Some("-1"),
                None,
            ),
            (
                "u8 of 0xFF",
                &[0xFF],
                Endian::Little,
                "u8",
                Some("255"),
                None,
            ),
            (
                "bin",
                &[0b1010_0101],
                Endian::Little,
                "bin",
                Some("10100101"),
                None,
            ),
            (
                "oct",
                &[0b1010_0101],
                Endian::Little,
                "oct",
                Some("0o245"),
                None,
            ),
            ("f32 one", &one_le, Endian::Little, "f32", Some("1"), None),
            (
                "short u32",
                &[0x01],
                Endian::Little,
                "u32",
                Some("—"),
                Some(0),
            ),
            ("short u8", &[0x01], Endian::Little, "u8", Some("1"), None),
            ("u64 width", &[0; 8], Endian::Little, "u64", None, Some(8)),
            ("u16 width", &[0; 8], Endian::Little, "u16", None, Some(2)),
        ];
        for (name, bytes, endian, label, value, width) in cases {
            let rows = inspect(bytes, 0, *endian);
            let row = rows
                .iter()
                .find(|r| r.label == *label)
                .unwrap_or_else(|| panic!("case {name}: no {label} row"));
            if let Some(value) = value {
                assert_eq!(row.value, *value, "case {name}");
            }
            if let Some(width) = width {
                assert_eq!(row.width, *width, "case {name}");
            }
        }
    }

    #[test]
    fn leb128_decodes_multi_byte_and_negative_and_rejects_truncation() {
        let (v, n) = decode_uleb128(&[0xE5, 0x8E, 0x26], 0).unwrap();
        assert_eq!(v, 624_485);
        assert_eq!(n, 3);

        let (v, n) = decode_sleb128(&[0xC0, 0xBB, 0x78], 0).unwrap();
        assert_eq!(v, -123_456);
        assert_eq!(n, 3);

        assert!(decode_uleb128(&[0x80], 0).is_none());
    }
}
