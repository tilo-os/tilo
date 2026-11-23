//! One strict parser for `/proc/self/mountinfo`, shared by the capture and projection machinery.
//!
//! A row is only usable once the whole table is: these callers compare a table against a held
//! descriptor, so a malformed or truncated row is refused rather than skipped. Paths are decoded
//! from the kernel's octal escapes for every row.
use std::{
    ffi::OsStr,
    io,
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

/// One mount table row, with its paths decoded and its numeric fields parsed.
pub(crate) struct Row {
    pub id: u64,
    pub parent: u64,
    pub major: u32,
    pub minor: u32,
    pub root: PathBuf,
    pub point: PathBuf,
    /// The per-mount options field (`rw,nodev,nosuid,noexec,…`), still comma-joined.
    pub options: Vec<u8>,
    /// The optional fields, each still in its `tag:value` form.
    pub optional: Vec<Vec<u8>>,
    pub fs_type: Vec<u8>,
}

/// Parse a whole mount table, refusing anything over `bound` or malformed. No row is skipped.
pub(crate) fn parse(bytes: &[u8], bound: usize) -> io::Result<Vec<Row>> {
    if bytes.len() > bound {
        return Err(io::ErrorKind::InvalidData.into());
    }
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|row| !row.is_empty())
        .map(parse_row)
        .collect()
}

fn parse_row(row: &[u8]) -> io::Result<Row> {
    let fields = row.split(|byte| *byte == b' ').collect::<Vec<_>>();
    let split = fields
        .iter()
        .position(|field| *field == b"-")
        .ok_or(io::ErrorKind::InvalidData)?;
    if split < 6 || split.checked_add(4) != Some(fields.len()) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let tail = fields.get(split..).ok_or(io::ErrorKind::InvalidData)?;
    let device = fields
        .get(2)
        .ok_or(io::ErrorKind::InvalidData)?
        .split(|byte| *byte == b':')
        .collect::<Vec<_>>();
    if device.len() != 2 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(Row {
        id: number(fields.first().ok_or(io::ErrorKind::InvalidData)?)?,
        parent: number(fields.get(1).ok_or(io::ErrorKind::InvalidData)?)?,
        major: u32::try_from(number(device.first().ok_or(io::ErrorKind::InvalidData)?)?)
            .map_err(io::Error::other)?,
        minor: u32::try_from(number(device.get(1).ok_or(io::ErrorKind::InvalidData)?)?)
            .map_err(io::Error::other)?,
        root: decode(fields.get(3).ok_or(io::ErrorKind::InvalidData)?)?,
        point: decode(fields.get(4).ok_or(io::ErrorKind::InvalidData)?)?,
        options: fields.get(5).ok_or(io::ErrorKind::InvalidData)?.to_vec(),
        optional: fields
            .get(6..split)
            .ok_or(io::ErrorKind::InvalidData)?
            .iter()
            .map(|field| field.to_vec())
            .collect(),
        fs_type: tail.get(1).ok_or(io::ErrorKind::InvalidData)?.to_vec(),
    })
}

/// Whether `path` is an absolute, literal coordinate: no NUL, no parent or current-dir components,
/// no deleted suffix and a sane bound.
pub(crate) fn literal(path: &Path) -> bool {
    let bytes = path.as_os_str().as_bytes();
    path.is_absolute()
        && bytes.len() <= 65536
        && !bytes.contains(&0)
        && !bytes.ends_with(b" (deleted)")
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn number(bytes: &[u8]) -> io::Result<u64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    std::str::from_utf8(bytes)
        .map_err(io::Error::other)?
        .parse()
        .map_err(io::Error::other)
}

fn decode(bytes: &[u8]) -> io::Result<PathBuf> {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;
    while let Some((&byte, rest)) = remaining.split_first() {
        if byte != b'\\' {
            decoded.push(byte);
            remaining = rest;
            continue;
        }
        let (encoded, rest) = rest.split_at_checked(3).ok_or(io::ErrorKind::InvalidData)?;
        decoded.push(match encoded {
            b"040" => b' ',
            b"011" => b'\t',
            b"012" => b'\n',
            b"134" => b'\\',
            _ => return Err(io::ErrorKind::InvalidData.into()),
        });
        remaining = rest;
    }
    Ok(PathBuf::from(OsStr::from_bytes(&decoded)))
}
