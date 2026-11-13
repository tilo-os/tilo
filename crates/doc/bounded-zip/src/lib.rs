//! The ZIP archives Tilo reads and writes, under bounds: untrusted packages
//! (`.odt`/`.docx`/`.xlsx`, `.odp`/`.pptx`, `.epub`, browser extensions), where
//! the archive and every part in it are attacker-controlled, and the archives an
//! app writes of what it holds ([`write_zip`]), read back under the same bounds.
//!
//! Every read caps the entry count, the per-part size and the running total,
//! checking each entry's declared size first and then enforcing the cap through
//! a hard-limited reader, so a lying header cannot exceed it. A breach is
//! rejected, never truncated. [`read_zip_parts`] and [`BoundedArchive::read_xml`]
//! reject parts that declare a DTD or entity.
//!
//! Every package reader goes through this crate, so the guarantees are uniform;
//! a caller whose format warrants tighter bounds passes its own [`ZipLimits`] or
//! a smaller per-read cap, which can only narrow them.

#[cfg(feature = "epub")]
pub mod epub;

use std::borrow::Cow;
use std::io::{Cursor, Read, Seek, Write};
use std::path::Path;

use zip::ZipArchive;
use zip::result::ZipError as RawZipError;

/// Bounds applied when reading an untrusted ZIP. The defaults allow large
/// documents while keeping decompression bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZipLimits {
    /// Maximum number of entries (files and directories) in the archive.
    pub max_parts: usize,
    pub max_part_bytes: u64,
    /// Maximum uncompressed size of everything read from the archive.
    pub max_total_bytes: u64,
}

impl Default for ZipLimits {
    fn default() -> Self {
        Self {
            max_parts: 8_192,
            max_part_bytes: 256 * 1024 * 1024,
            max_total_bytes: 1024 * 1024 * 1024,
        }
    }
}

/// Why reading an untrusted archive failed: a limit breach is never silently
/// truncated, only one of these variants.
#[derive(Debug, thiserror::Error)]
pub enum ZipError {
    #[error("zip open: {0}")]
    Open(#[source] RawZipError),
    #[error("zip has {count} entries, over the {limit} limit (possible zip bomb)")]
    TooManyEntries { count: usize, limit: usize },
    #[error("zip entry {index}: {source}")]
    Entry {
        index: usize,
        #[source]
        source: RawZipError,
    },
    #[error("zip part {name} not found")]
    Missing { name: String },
    #[error("zip part {name}: {source}")]
    Member {
        name: String,
        #[source]
        source: RawZipError,
    },
    #[error(
        "zip part {name} declares {declared} bytes, over the {limit} per-part limit (possible zip bomb)"
    )]
    PartTooLarge {
        name: String,
        declared: u64,
        limit: u64,
    },
    #[error("zip part {name} inflates past the {limit}-byte per-part limit (possible zip bomb)")]
    PartInflatesTooLarge { name: String, limit: u64 },
    #[error("zip parts exceed the {limit}-byte total limit (possible zip bomb)")]
    TotalTooLarge { limit: u64 },
    #[error("zip read {name}: {source}")]
    Read {
        name: String,
        #[source]
        source: std::io::Error,
    },
    #[error("zip part {name} contains a DTD/entity declaration — rejected (XXE defence)")]
    Dtd { name: String },
    #[error("zip part {name} is not UTF-8")]
    NotUtf8 { name: String },
    #[error("zip entry {name} names a path outside the extraction directory")]
    UnsafeName { name: String },
    #[error("zip entry {name} is in the archive twice")]
    DuplicateName { name: String },
    #[error("zip write {name}: {source}")]
    Write {
        name: String,
        #[source]
        source: std::io::Error,
    },
}

impl ZipError {
    #[must_use]
    pub const fn is_limit_breach(&self) -> bool {
        matches!(
            self,
            Self::TooManyEntries { .. }
                | Self::PartTooLarge { .. }
                | Self::PartInflatesTooLarge { .. }
                | Self::TotalTooLarge { .. }
        )
    }
}

impl From<ZipError> for std::io::Error {
    fn from(error: ZipError) -> Self {
        Self::new(std::io::ErrorKind::InvalidData, error)
    }
}

/// A ZIP archive whose every read is bounded by [`ZipLimits`]: opening checks the
/// entry count, and each read checks the part's declared size, inflates through
/// a hard-limited reader and adds what it read to the total.
pub struct BoundedArchive<R> {
    archive: ZipArchive<R>,
    limits: ZipLimits,
    total: u64,
}

impl<'a> BoundedArchive<Cursor<&'a [u8]>> {
    /// Open an in-memory archive without copying it.
    ///
    /// # Errors
    ///
    /// [`ZipError::Open`] for unreadable bytes, [`ZipError::TooManyEntries`] when
    /// the archive has more entries than `limits.max_parts`.
    pub fn from_bytes(bytes: &'a [u8], limits: ZipLimits) -> Result<Self, ZipError> {
        Self::new(Cursor::new(bytes), limits)
    }
}

impl<R: Read + Seek> BoundedArchive<R> {
    /// Open an archive from a seekable reader.
    ///
    /// # Errors
    ///
    /// [`ZipError::Open`] for an unreadable archive, [`ZipError::TooManyEntries`]
    /// when it has more entries than `limits.max_parts`.
    pub fn new(reader: R, limits: ZipLimits) -> Result<Self, ZipError> {
        let archive = ZipArchive::new(reader).map_err(ZipError::Open)?;
        if archive.len() > limits.max_parts {
            return Err(ZipError::TooManyEntries {
                count: archive.len(),
                limit: limits.max_parts,
            });
        }
        Ok(Self {
            archive,
            limits,
            total: 0,
        })
    }

    #[must_use]
    pub const fn limits(&self) -> &ZipLimits {
        &self.limits
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.archive.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.archive.is_empty()
    }

    /// Names of every entry; an entry whose name does not decode is left out, and
    /// reading it fails with [`ZipError::Entry`].
    pub fn file_names(&self) -> impl Iterator<Item = Cow<'_, str>> {
        self.archive.file_names().filter_map(Result::ok)
    }

    /// Reject the archive up front if any entry declares more than
    /// [`ZipLimits::max_part_bytes`] or the declared sizes sum past
    /// [`ZipLimits::max_total_bytes`]: for a caller that refuses a hostile package
    /// before anything is inflated, or later hands it to an extractor.
    ///
    /// # Errors
    ///
    /// [`ZipError::Entry`], [`ZipError::PartTooLarge`] or
    /// [`ZipError::TotalTooLarge`].
    pub fn check_declared_sizes(&mut self) -> Result<(), ZipError> {
        let mut total: u64 = 0;
        for index in 0..self.archive.len() {
            let file = self
                .archive
                .by_index(index)
                .map_err(|source| ZipError::Entry { index, source })?;
            let declared = file.size();
            if declared > self.limits.max_part_bytes {
                return Err(ZipError::PartTooLarge {
                    name: file
                        .name()
                        .map_err(|source| ZipError::Entry { index, source })?
                        .into_owned(),
                    declared,
                    limit: self.limits.max_part_bytes,
                });
            }
            total = total.saturating_add(declared);
            if total > self.limits.max_total_bytes {
                return Err(ZipError::TotalTooLarge {
                    limit: self.limits.max_total_bytes,
                });
            }
        }
        Ok(())
    }

    /// Read the part `name`, allowing at most `cap` bytes, further narrowed to
    /// [`ZipLimits::max_part_bytes`].
    ///
    /// # Errors
    ///
    /// [`ZipError::Missing`] when there is no such part, a limit breach or a read
    /// failure otherwise.
    pub fn read(&mut self, name: &str, cap: u64) -> Result<Vec<u8>, ZipError> {
        let cap = cap.min(self.limits.max_part_bytes);
        let max_total = self.limits.max_total_bytes;
        let file = match self.archive.by_name(name) {
            Ok(file) => file,
            Err(RawZipError::FileNotFound) => {
                return Err(ZipError::Missing {
                    name: name.to_string(),
                });
            }
            Err(source) => {
                return Err(ZipError::Member {
                    name: name.to_string(),
                    source,
                });
            }
        };
        let declared = file.size();
        read_capped(file, name, declared, cap, max_total, &mut self.total)
    }

    /// The first `len` bytes of the part `name`, inflating no more than that: a
    /// header check on a part too large to read whole.
    ///
    /// # Errors
    ///
    /// [`ZipError::Missing`] when there is no such part, a read failure otherwise.
    pub fn read_prefix(&mut self, name: &str, len: usize) -> Result<Vec<u8>, ZipError> {
        let file = match self.archive.by_name(name) {
            Ok(file) => file,
            Err(RawZipError::FileNotFound) => {
                return Err(ZipError::Missing {
                    name: name.to_string(),
                });
            }
            Err(source) => {
                return Err(ZipError::Member {
                    name: name.to_string(),
                    source,
                });
            }
        };
        let mut prefix = Vec::with_capacity(len);
        file.take(len as u64)
            .read_to_end(&mut prefix)
            .map_err(|source| ZipError::Read {
                name: name.to_string(),
                source,
            })?;
        Ok(prefix)
    }

    /// [`read`](Self::read), decoded as UTF-8. A doctype is allowed; formats whose
    /// XML never carries one read through [`read_xml`](Self::read_xml).
    ///
    /// # Errors
    ///
    /// As [`read`](Self::read), plus [`ZipError::NotUtf8`].
    pub fn read_text(&mut self, name: &str, cap: u64) -> Result<String, ZipError> {
        let bytes = self.read(name, cap)?;
        String::from_utf8(bytes).map_err(|_| ZipError::NotUtf8 {
            name: name.to_string(),
        })
    }

    /// [`read_text`](Self::read_text) for an XML part that must not declare a DTD or
    /// entity (XXE defence).
    ///
    /// # Errors
    ///
    /// As [`read_text`](Self::read_text), plus [`ZipError::Dtd`].
    pub fn read_xml(&mut self, name: &str, cap: u64) -> Result<String, ZipError> {
        let bytes = self.read(name, cap)?;
        if contains_dtd(&bytes) {
            return Err(ZipError::Dtd {
                name: name.to_string(),
            });
        }
        String::from_utf8(bytes).map_err(|_| ZipError::NotUtf8 {
            name: name.to_string(),
        })
    }

    /// Every file entry in memory, in archive order; directory entries are skipped
    /// and `.xml`/`.rels` parts are rejected if they declare a DTD or entity.
    pub fn read_all(&mut self) -> Result<Vec<(String, Vec<u8>)>, ZipError> {
        let cap = self.limits.max_part_bytes;
        let max_total = self.limits.max_total_bytes;
        let mut parts = Vec::with_capacity(self.archive.len());
        for index in 0..self.archive.len() {
            let file = self
                .archive
                .by_index(index)
                .map_err(|source| ZipError::Entry { index, source })?;
            let name = file
                .name()
                .map_err(|source| ZipError::Entry { index, source })?
                .into_owned();
            if name.ends_with('/') {
                continue;
            }
            let declared = file.size();
            let buf = read_capped(file, &name, declared, cap, max_total, &mut self.total)?;
            if is_xml_part(&name) && contains_dtd(&buf) {
                return Err(ZipError::Dtd { name });
            }
            parts.push((name, buf));
        }
        Ok(parts)
    }

    /// Extract every entry under `dir`, each part inflated under the per-part cap and
    /// all of them under the total, the same bounds [`read_all`](Self::read_all)
    /// enforces. Returns the number of files written.
    ///
    /// Names are confined to `dir`: a `..` or an absolute entry is an error, and a
    /// symbolic link is written as the bytes it contains, never created. A declared
    /// size is not trusted, so [`check_declared_sizes`](Self::check_declared_sizes)
    /// is an optional early refusal, not a precondition.
    pub fn extract_to(&mut self, dir: &Path) -> Result<usize, ZipError> {
        let cap = self.limits.max_part_bytes;
        let max_total = self.limits.max_total_bytes;
        let mut written: usize = 0;
        for index in 0..self.archive.len() {
            let file = self
                .archive
                .by_index(index)
                .map_err(|source| ZipError::Entry { index, source })?;
            let name = file
                .name()
                .map_err(|source| ZipError::Entry { index, source })?
                .into_owned();
            let Some(relative) = file.enclosed_name() else {
                return Err(ZipError::UnsafeName { name });
            };
            let target = dir.join(relative);
            let io_error = |source| ZipError::Read {
                name: name.clone(),
                source,
            };
            if file.is_dir() {
                std::fs::create_dir_all(&target).map_err(io_error)?;
                continue;
            }
            let declared = file.size();
            let bytes = read_capped(file, &name, declared, cap, max_total, &mut self.total)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io_error)?;
            }
            std::fs::write(&target, bytes).map_err(io_error)?;
            written = written.saturating_add(1);
        }
        Ok(written)
    }
}

/// Inflate one entry under `cap`, rejecting on the declared size before
/// allocating and on the actual size through a hard-limited reader, then add it
/// to the running `total`.
fn read_capped(
    mut file: impl Read,
    name: &str,
    declared: u64,
    cap: u64,
    max_total: u64,
    total: &mut u64,
) -> Result<Vec<u8>, ZipError> {
    if declared > cap {
        return Err(ZipError::PartTooLarge {
            name: name.to_string(),
            declared,
            limit: cap,
        });
    }
    if total.saturating_add(declared) > max_total {
        return Err(ZipError::TotalTooLarge { limit: max_total });
    }
    // Pre-size from the (bounded) declared size, but never trust it for more
    // than a small first allocation.
    let mut buf = Vec::with_capacity(usize::try_from(declared.min(64 * 1024)).unwrap_or(0));
    // Enforce the cap even if the header lied: inflate at most `cap + 1`, and
    // hitting that means over-limit. The zip reader also refuses to inflate past
    // the declared size, with an InvalidData error its message alone tells apart
    // from a checksum failure; that is the same lie, caught earlier.
    let read = match (&mut file)
        .take(cap.saturating_add(1))
        .read_to_end(&mut buf)
    {
        Ok(read) => read as u64,
        Err(source)
            if source.kind() == std::io::ErrorKind::InvalidData
                && source.to_string().contains("larger than its declared") =>
        {
            return Err(ZipError::PartInflatesTooLarge {
                name: name.to_string(),
                limit: cap,
            });
        }
        Err(source) => {
            return Err(ZipError::Read {
                name: name.to_string(),
                source,
            });
        }
    };
    if read > cap {
        return Err(ZipError::PartInflatesTooLarge {
            name: name.to_string(),
            limit: cap,
        });
    }
    *total = total.saturating_add(read);
    if *total > max_total {
        return Err(ZipError::TotalTooLarge { limit: max_total });
    }
    Ok(buf)
}

/// Every file entry of an untrusted ZIP in memory, in archive order, under
/// `limits`; directory entries are skipped and `.xml`/`.rels` parts are rejected
/// if they declare a DTD or entity (XXE defence).
///
/// # Errors
///
/// Any breach of a limit, a malformed archive, an unreadable entry or a
/// DTD-bearing XML part; the input is never silently truncated.
pub fn read_zip_parts(
    bytes: &[u8],
    limits: &ZipLimits,
) -> Result<Vec<(String, Vec<u8>)>, ZipError> {
    BoundedArchive::from_bytes(bytes, *limits)?.read_all()
}

/// Write `entries` — each name inside the archive and its bytes — into one
/// archive, in that order, under `limits`: what it is written within is what it is
/// read back under. A name that could place a file outside the folder the archive
/// is extracted into ([`is_safe_name`]) is refused rather than written.
///
/// Parts are stored, not deflated: an app writes the documents and pictures it
/// already holds, and a reader finds them as they went in.
///
/// # Errors
///
/// [`ZipError::TooManyEntries`], [`ZipError::PartTooLarge`] or
/// [`ZipError::TotalTooLarge`] for an archive over `limits`;
/// [`ZipError::UnsafeName`] or [`ZipError::DuplicateName`] for a name no archive
/// may hold; [`ZipError::Write`] when the bytes could not be written.
pub fn write_zip(entries: &[(String, Vec<u8>)], limits: &ZipLimits) -> Result<Vec<u8>, ZipError> {
    if entries.len() > limits.max_parts {
        return Err(ZipError::TooManyEntries {
            count: entries.len(),
            limit: limits.max_parts,
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut total: u64 = 0;
    for (name, bytes) in entries {
        if !is_safe_name(name) {
            return Err(ZipError::UnsafeName { name: name.clone() });
        }
        if !seen.insert(name.as_str()) {
            return Err(ZipError::DuplicateName { name: name.clone() });
        }
        let declared = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if declared > limits.max_part_bytes {
            return Err(ZipError::PartTooLarge {
                name: name.clone(),
                declared,
                limit: limits.max_part_bytes,
            });
        }
        total = total.saturating_add(declared);
        if total > limits.max_total_bytes {
            return Err(ZipError::TotalTooLarge {
                limit: limits.max_total_bytes,
            });
        }
    }
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let written = |name: &str, source: zip::result::ZipError| ZipError::Write {
        name: name.to_owned(),
        source: source.into(),
    };
    for (name, bytes) in entries {
        writer
            .start_file(name.as_str(), options)
            .map_err(|source| written(name, source))?;
        writer.write_all(bytes).map_err(|source| ZipError::Write {
            name: name.clone(),
            source,
        })?;
    }
    let archive = writer.finish().map_err(|source| written("", source))?;
    Ok(archive.into_inner())
}

/// Whether `name` is a name a file inside an archive may have: parts that are each
/// a file or folder name, none empty, `.` or `..`, so nothing it names stands
/// outside the extraction folder. A backslash is refused too, since some
/// extractors read it as the separator.
#[must_use]
pub fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', '\0'])
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Whether `name` is a package part held to the no-DTD rule (parseable XML).
/// Binary parts are exempt: a `<!DOCTYPE` sequence inside a JPEG is no threat,
/// and such parts are never parsed as XML.
fn is_xml_part(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("xml") || ext.eq_ignore_ascii_case("rels"))
}

/// Whether `bytes` declares a DTD or entity (`<!DOCTYPE`/`<!ENTITY`), scanned
/// case-insensitively. A DTD is legal only in the prolog, so scanning a generous
/// prefix bounds the cost without missing an active one.
#[must_use]
pub fn contains_dtd(bytes: &[u8]) -> bool {
    // A DTD must appear before the root element; scanning 64 KiB covers the
    // prolog while bounding the scan cost.
    let head = bytes.get(..bytes.len().min(64 * 1024)).unwrap_or(bytes);
    contains_ci(head, b"<!doctype") || contains_ci(head, b"<!entity")
}

fn contains_ci(haystack: &[u8], needle_lower: &[u8]) -> bool {
    if needle_lower.is_empty() || haystack.len() < needle_lower.len() {
        return false;
    }
    haystack.windows(needle_lower.len()).any(|w| {
        w.iter()
            .zip(needle_lower)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    fn zip_with(entries: &[(&str, &[u8])], method: CompressionMethod) -> Vec<u8> {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(method);
        for (name, data) in entries {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    /// Rewrites every declared uncompressed size (local headers and central
    /// directory) to `size`, forging a header that lies about the payload.
    fn forge_declared_size(zip: &mut [u8], size: u32) {
        let fields: Vec<usize> = zip
            .windows(4)
            .enumerate()
            .filter_map(|(at, magic)| match magic {
                [0x50, 0x4b, 0x03, 0x04] => at.checked_add(22),
                [0x50, 0x4b, 0x01, 0x02] => at.checked_add(24),
                _ => None,
            })
            .collect();
        for start in fields {
            let end = start.checked_add(4).unwrap();
            zip.get_mut(start..end)
                .unwrap()
                .copy_from_slice(&size.to_le_bytes());
        }
    }

    #[test]
    fn normal_packages_read_and_missing_or_non_utf8_parts_are_distinct_errors() {
        let z = zip_with(
            &[("a.xml", b"<x/>"), ("b.bin", b"\x00\x01\x02")],
            CompressionMethod::Deflated,
        );
        let parts = read_zip_parts(&z, &ZipLimits::default()).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, "a.xml");
        assert_eq!(parts[1].1, b"\x00\x01\x02");

        let z = zip_with(&[("a.bin", b"\xff\xfe")], CompressionMethod::Stored);
        let mut archive = BoundedArchive::from_bytes(&z, ZipLimits::default()).unwrap();
        assert!(matches!(
            archive.read("nope", u64::MAX),
            Err(ZipError::Missing { .. })
        ));
        assert!(matches!(
            archive.read_text("a.bin", u64::MAX),
            Err(ZipError::NotUtf8 { .. })
        ));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one fixture per limit, each with its own before/during-inflation assertions"
    )]
    fn size_limits_stop_bombs_before_and_during_inflation() {
        let entries: Vec<(String, Vec<u8>)> =
            (0..20u8).map(|i| (format!("p{i}.dat"), vec![i])).collect();
        let refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        let z = zip_with(&refs, CompressionMethod::Stored);
        let limits = ZipLimits {
            max_parts: 8,
            ..ZipLimits::default()
        };
        let err = read_zip_parts(&z, &limits).unwrap_err();
        assert!(
            format!("{err}").contains("entries"),
            "expected entry-count rejection, got {err}"
        );
        assert!(err.is_limit_breach());
        assert!(matches!(
            BoundedArchive::from_bytes(&z, limits),
            Err(ZipError::TooManyEntries {
                count: 20,
                limit: 8
            })
        ));

        // A genuine bomb: ~4 MiB of zeros deflates to a few KB but declares its
        // true size in the entry header — the pre-check must reject it without
        // inflating the payload.
        let bomb = vec![0u8; 4 * 1024 * 1024];
        let z = zip_with(&[("content.xml", &bomb)], CompressionMethod::Deflated);
        assert!(
            z.len() < 64 * 1024,
            "the bomb must be tiny on disk: {} bytes",
            z.len()
        );
        let limits = ZipLimits {
            max_part_bytes: 1024 * 1024,
            ..ZipLimits::default()
        };
        let err = read_zip_parts(&z, &limits).unwrap_err();
        assert!(
            format!("{err}").contains("per-part limit"),
            "expected size rejection, got {err}"
        );
        let mut archive = BoundedArchive::from_bytes(&z, limits).unwrap();
        assert!(matches!(
            archive.check_declared_sizes(),
            Err(ZipError::PartTooLarge { .. })
        ));
        assert!(matches!(
            archive.read("content.xml", u64::MAX),
            Err(ZipError::PartTooLarge { .. })
        ));

        // Three half-MiB parts against a 1 MiB total: refused up front, and the
        // running total spans named reads.
        let part = vec![7u8; 512 * 1024];
        let z = zip_with(
            &[("a.dat", &part), ("b.dat", &part), ("c.dat", &part)],
            CompressionMethod::Deflated,
        );
        let limits = ZipLimits {
            max_total_bytes: 1024 * 1024,
            ..ZipLimits::default()
        };
        let err = read_zip_parts(&z, &limits).unwrap_err();
        assert!(
            format!("{err}").contains("total limit"),
            "expected total rejection, got {err}"
        );
        let mut archive = BoundedArchive::from_bytes(&z, limits).unwrap();
        assert!(matches!(
            archive.check_declared_sizes(),
            Err(ZipError::TotalTooLarge { .. })
        ));
        let mut archive = BoundedArchive::from_bytes(&z, limits).unwrap();
        archive.read("a.dat", u64::MAX).unwrap();
        archive.read("b.dat", u64::MAX).unwrap();
        assert!(matches!(
            archive.read("c.dat", u64::MAX),
            Err(ZipError::TotalTooLarge { .. })
        ));

        // A per-read cap narrows the part limit.
        let z = zip_with(&[("a.txt", b"hello world")], CompressionMethod::Deflated);
        let mut archive = BoundedArchive::from_bytes(&z, ZipLimits::default()).unwrap();
        let err = archive.read("a.txt", 4).unwrap_err();
        assert!(
            matches!(
                err,
                ZipError::PartTooLarge {
                    declared: 11,
                    limit: 4,
                    ..
                }
            ),
            "got {err}"
        );
        assert_eq!(archive.read_text("a.txt", 11).unwrap(), "hello world");

        // Declares 4 bytes, actually stores 4 KiB: the declared-size check
        // passes, so only the hard-limited reader can catch it.
        let payload = vec![b'x'; 4096];
        let mut z = zip_with(&[("a.dat", &payload)], CompressionMethod::Stored);
        forge_declared_size(&mut z, 4);
        let limits = ZipLimits {
            max_part_bytes: 1024,
            ..ZipLimits::default()
        };
        let mut archive = BoundedArchive::from_bytes(&z, limits).unwrap();
        archive.check_declared_sizes().unwrap();
        let err = archive.read("a.dat", u64::MAX).unwrap_err();
        assert!(
            matches!(err, ZipError::PartInflatesTooLarge { limit: 1024, .. }),
            "got {err}"
        );
        let err = read_zip_parts(&z, &limits).unwrap_err();
        assert!(
            matches!(err, ZipError::PartInflatesTooLarge { .. }),
            "got {err}"
        );
    }

    #[test]
    fn dtds_are_refused_only_in_parsed_xml_prologs() {
        let evil = br#"<?xml version="1.0"?><!DOCTYPE lolz [<!ENTITY a "boom">]><doc>&a;</doc>"#;
        let z = zip_with(&[("content.xml", evil)], CompressionMethod::Deflated);
        let err = read_zip_parts(&z, &ZipLimits::default()).unwrap_err();
        assert!(
            format!("{err}").contains("DTD"),
            "expected DTD rejection, got {err}"
        );
        let mut archive = BoundedArchive::from_bytes(&z, ZipLimits::default()).unwrap();
        assert!(matches!(
            archive.read_xml("content.xml", u64::MAX),
            Err(ZipError::Dtd { .. })
        ));

        // EPUB 2 XHTML legitimately starts with `<!DOCTYPE html ...>`; read as
        // text it is admitted.
        let xhtml = b"<!DOCTYPE html><html/>";
        let z = zip_with(&[("ch1.xhtml", xhtml)], CompressionMethod::Deflated);
        let mut archive = BoundedArchive::from_bytes(&z, ZipLimits::default()).unwrap();
        assert_eq!(
            archive.read_text("ch1.xhtml", u64::MAX).unwrap().as_bytes(),
            xhtml
        );

        // The same bytes inside a non-XML part are harmless (never parsed as XML).
        let z = zip_with(
            &[("image.png", b"<!DOCTYPE not-xml>")],
            CompressionMethod::Deflated,
        );
        assert_eq!(read_zip_parts(&z, &ZipLimits::default()).unwrap().len(), 1);

        assert!(contains_dtd(b"<!doctype x>"));
        assert!(contains_dtd(b"<!EnTiTy a 'b'>"));
        assert!(!contains_dtd(b"<doc/>"));
        let mut late = vec![b' '; 64 * 1024];
        late.extend_from_slice(b"<!DOCTYPE x>");
        assert!(!contains_dtd(&late));
    }

    /// A fresh directory under the system temp dir, so the crate needs no
    /// test-only dependency for a few files.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bounded-zip-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extraction_is_bounded_and_confined() {
        let limits = ZipLimits {
            max_parts: 8,
            max_part_bytes: 64,
            max_total_bytes: 1024,
        };
        {
            let dir = scratch("lands");
            let bytes = zip_with(
                &[("manifest.json", b"{}"), ("sub/page.js", b"x")],
                CompressionMethod::Stored,
            );
            let mut archive = BoundedArchive::from_bytes(&bytes, limits).unwrap();
            assert_eq!(archive.extract_to(&dir).unwrap(), 2);
            assert_eq!(std::fs::read(dir.join("sub/page.js")).unwrap(), b"x");
        }
        {
            let dir = scratch("lies");
            let big = vec![b'a'; 4096];
            let mut bytes = zip_with(&[("blob.bin", &big)], CompressionMethod::Deflated);
            forge_declared_size(&mut bytes, 8);
            let mut archive = BoundedArchive::from_bytes(&bytes, limits).unwrap();
            assert!(
                archive.check_declared_sizes().is_ok(),
                "the forged header passes the declared check on purpose"
            );
            assert!(matches!(
                archive.extract_to(&dir),
                Err(ZipError::PartInflatesTooLarge { .. })
            ));
            assert!(
                !dir.join("blob.bin").exists(),
                "an over-limit part is never written"
            );
        }
        {
            let dir = scratch("escape");
            let bytes = zip_with(&[("../escape.txt", b"x")], CompressionMethod::Stored);
            let mut archive = BoundedArchive::from_bytes(&bytes, limits).unwrap();
            assert!(matches!(
                archive.extract_to(&dir),
                Err(ZipError::UnsafeName { .. })
            ));
            assert!(!dir.join("../escape.txt").exists());
        }
    }

    #[test]
    fn what_is_written_is_what_reads_back() {
        let entries = vec![
            ("Meeting.md".to_owned(), b"# Meeting\n".to_vec()),
            (
                "notes/images/fox.png".to_owned(),
                vec![0x89, 0x50, 0x4e, 0x47],
            ),
            ("notes/Monday.md".to_owned(), b"# Monday\n".to_vec()),
        ];
        let bytes = write_zip(&entries, &ZipLimits::default()).unwrap();
        let read = read_zip_parts(&bytes, &ZipLimits::default()).unwrap();
        assert_eq!(read, entries, "the files read back as they went in");
        assert_eq!(
            BoundedArchive::from_bytes(&bytes, ZipLimits::default())
                .unwrap()
                .file_names()
                .map(Cow::into_owned)
                .collect::<Vec<_>>(),
            ["Meeting.md", "notes/images/fox.png", "notes/Monday.md"]
        );
    }

    #[test]
    fn a_name_that_leads_outside_or_repeats_is_never_written() {
        for name in [
            "",
            "/etc/passwd",
            "../out.md",
            "a/../../b.md",
            "a//b.md",
            "a/./b.md",
            "a\\b.md",
        ] {
            assert!(
                !is_safe_name(name),
                "`{name}` is not a name inside an archive"
            );
            assert!(
                matches!(
                    write_zip(&[(name.to_owned(), b"x".to_vec())], &ZipLimits::default()),
                    Err(ZipError::UnsafeName { .. })
                ),
                "`{name}` is refused"
            );
        }
        for name in ["Meeting.md", "a/b.md", "images/fox.png", "..hidden.md"] {
            assert!(is_safe_name(name), "`{name}` is a name inside an archive");
        }
        assert!(matches!(
            write_zip(
                &[
                    ("a.md".to_owned(), b"1".to_vec()),
                    ("a.md".to_owned(), b"2".to_vec()),
                ],
                &ZipLimits::default()
            ),
            Err(ZipError::DuplicateName { .. })
        ));
    }

    #[test]
    fn what_is_written_stays_within_the_limits_it_reads_back_under() {
        let limits = ZipLimits {
            max_parts: 2,
            max_part_bytes: 4,
            max_total_bytes: 6,
        };
        let entries = |n: usize, len: usize| -> Vec<(String, Vec<u8>)> {
            (0..n)
                .map(|i| (format!("p{i}.bin"), vec![b'x'; len]))
                .collect()
        };
        assert!(matches!(
            write_zip(&entries(3, 1), &limits),
            Err(ZipError::TooManyEntries { count: 3, limit: 2 })
        ));
        let err = write_zip(&entries(1, 5), &limits).unwrap_err();
        assert!(matches!(err, ZipError::PartTooLarge { declared: 5, .. }));
        assert!(err.is_limit_breach());
        let err = write_zip(&entries(2, 4), &limits).unwrap_err();
        assert!(matches!(err, ZipError::TotalTooLarge { limit: 6 }));
        // Two of three bytes fit, and read back under the same limits.
        let bytes = write_zip(&entries(2, 3), &limits).unwrap();
        assert_eq!(
            read_zip_parts(&bytes, &limits).unwrap().len(),
            2,
            "what is written reads back"
        );
    }
}
