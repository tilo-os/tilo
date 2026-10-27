//! How the index writes what it holds: field values, the pages of a list and a list's
//! directory, each sealed with a checksum. A value that does not check out is never read as
//! one: the index it is in is corrupt, and is built again from the record.

use tilolang_runtime::store::{FieldValue, Scalar};

/// What reading a sealed value found wrong: the bytes are not what the index wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Corrupt;

/// `bytes` with their checksum after them.
pub fn seal(mut bytes: Vec<u8>) -> Vec<u8> {
    let sum = crc32fast::hash(&bytes);
    bytes.extend_from_slice(&sum.to_le_bytes());
    bytes
}

/// The bytes `sealed` holds, when its checksum checks out.
///
/// # Errors
/// The checksum does not match, or there is none.
pub fn open(sealed: &[u8]) -> Result<&[u8], Corrupt> {
    let split = sealed.len().checked_sub(4).ok_or(Corrupt)?;
    let (bytes, sum) = sealed.split_at(split);
    let sum: [u8; 4] = sum.try_into().map_err(|_| Corrupt)?;
    if crc32fast::hash(bytes) == u32::from_le_bytes(sum) {
        Ok(bytes)
    } else {
        Err(Corrupt)
    }
}

/// Writes the parts of a value.
#[derive(Default)]
pub struct Writer(Vec<u8>);

impl Writer {
    pub fn u8(&mut self, value: u8) {
        self.0.push(value);
    }

    pub fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    pub fn str(&mut self, value: &str) {
        self.u32(u32::try_from(value.len()).unwrap_or(u32::MAX));
        self.0.extend_from_slice(value.as_bytes());
    }

    pub fn sealed(self) -> Vec<u8> {
        seal(self.0)
    }
}

/// Reads the parts of a value [`Writer`] wrote.
pub struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Corrupt> {
        if n > self.0.len() {
            return Err(Corrupt);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    /// The next u8, little-endian.
    ///
    /// # Errors
    /// The value ends before it.
    pub fn u8(&mut self) -> Result<u8, Corrupt> {
        self.take(1)?.first().copied().ok_or(Corrupt)
    }

    /// The next u32, little-endian.
    ///
    /// # Errors
    /// The value ends before it.
    pub fn u32(&mut self) -> Result<u32, Corrupt> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| Corrupt)?,
        ))
    }

    /// The next u64, little-endian.
    ///
    /// # Errors
    /// The value ends before it.
    pub fn u64(&mut self) -> Result<u64, Corrupt> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| Corrupt)?,
        ))
    }

    /// The next text, after its length.
    ///
    /// # Errors
    /// The value ends before it, or it is not UTF-8.
    pub fn str(&mut self) -> Result<&'a str, Corrupt> {
        let len = usize::try_from(self.u32()?).map_err(|_| Corrupt)?;
        std::str::from_utf8(self.take(len)?).map_err(|_| Corrupt)
    }

    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

const ABSENT: u8 = 0;
const BOOL: u8 = 1;
const INT: u8 = 2;
const FLOAT: u8 = 3;
const TEXT: u8 = 4;
const JSON: u8 = 5;
const COUNTER: u8 = 6;
const TEXT_FIELD: u8 = 7;
const KEYS: u8 = 8;
const ENTRIES: u8 = 9;
const ITEMS: u8 = 10;

fn put_scalar(w: &mut Writer, scalar: &Scalar) {
    match scalar {
        Scalar::Bool(flag) => {
            w.u8(BOOL);
            w.u8(u8::from(*flag));
        }
        Scalar::Int(value) => {
            w.u8(INT);
            w.u64(u64::from_le_bytes(value.to_le_bytes()));
        }
        Scalar::Float(value) => {
            w.u8(FLOAT);
            w.u64(value.to_bits());
        }
        Scalar::Text(text) => {
            w.u8(TEXT);
            w.str(text);
        }
        Scalar::Json(value) => {
            w.u8(JSON);
            w.str(&value.to_string());
        }
    }
}

fn get_scalar(tag: u8, r: &mut Reader<'_>) -> Result<Scalar, Corrupt> {
    Ok(match tag {
        BOOL => Scalar::Bool(r.u8()? != 0),
        INT => Scalar::Int(i64::from_le_bytes(r.u64()?.to_le_bytes())),
        FLOAT => Scalar::Float(f64::from_bits(r.u64()?)),
        TEXT => Scalar::Text(r.str()?.to_owned()),
        JSON => Scalar::Json(serde_json::from_str(r.str()?).map_err(|_| Corrupt)?),
        _ => return Err(Corrupt),
    })
}

fn len_u32(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// A field value, sealed.
pub fn put_value(value: &FieldValue) -> Vec<u8> {
    let mut w = Writer::default();
    match value {
        FieldValue::Absent => w.u8(ABSENT),
        FieldValue::Scalar(scalar) => put_scalar(&mut w, scalar),
        FieldValue::Counter(value) => {
            w.u8(COUNTER);
            w.u64(u64::from_le_bytes(value.to_le_bytes()));
        }
        FieldValue::Text(text) => {
            w.u8(TEXT_FIELD);
            w.str(text);
        }
        FieldValue::Keys(keys) => {
            w.u8(KEYS);
            w.u32(len_u32(keys.len()));
            for key in keys {
                w.str(key);
            }
        }
        FieldValue::Entries(entries) => {
            w.u8(ENTRIES);
            w.u32(len_u32(entries.len()));
            for (key, scalar) in entries {
                w.str(key);
                put_scalar(&mut w, scalar);
            }
        }
        FieldValue::Items(items) => {
            w.u8(ITEMS);
            w.u32(len_u32(items.len()));
            for item in items {
                w.str(item);
            }
        }
    }
    w.sealed()
}

/// The field value `sealed` holds.
///
/// # Errors
/// The bytes are not a value the index wrote.
pub fn get_value(sealed: &[u8]) -> Result<FieldValue, Corrupt> {
    let mut r = Reader::new(open(sealed)?);
    let tag = r.u8()?;
    let value = match tag {
        ABSENT => FieldValue::Absent,
        COUNTER => FieldValue::Counter(i64::from_le_bytes(r.u64()?.to_le_bytes())),
        TEXT_FIELD => FieldValue::Text(r.str()?.to_owned()),
        KEYS => {
            let n = r.u32()?;
            let mut keys = Vec::new();
            for _ in 0..n {
                keys.push(r.str()?.to_owned());
            }
            FieldValue::Keys(keys)
        }
        ENTRIES => {
            let n = r.u32()?;
            let mut entries = Vec::new();
            for _ in 0..n {
                let key = r.str()?.to_owned();
                let tag = r.u8()?;
                entries.push((key, get_scalar(tag, &mut r)?));
            }
            FieldValue::Entries(entries)
        }
        ITEMS => {
            let n = r.u32()?;
            let mut items = Vec::new();
            for _ in 0..n {
                items.push(r.str()?.to_owned());
            }
            FieldValue::Items(items)
        }
        other => FieldValue::Scalar(get_scalar(other, &mut r)?),
    };
    if r.is_empty() {
        Ok(value)
    } else {
        Err(Corrupt)
    }
}

/// A short text, sealed: a type id, an order key.
pub fn put_text(text: &str) -> Vec<u8> {
    let mut w = Writer::default();
    w.str(text);
    w.sealed()
}

/// The short text `sealed` holds.
///
/// # Errors
/// The bytes are not a text the index wrote.
pub fn get_text(sealed: &[u8]) -> Result<String, Corrupt> {
    let mut r = Reader::new(open(sealed)?);
    let text = r.str()?.to_owned();
    if r.is_empty() { Ok(text) } else { Err(Corrupt) }
}
