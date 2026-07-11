//! Hardening for parsing **untrusted** office packages (`.odt`/`.docx`).
//!
//! An office file is an attacker-controlled ZIP of attacker-controlled XML. Two
//! classic pitfalls have to be bounded before any of it touches the model:
//!
//! * **ZIP bombs** — a few KB that decompress to gigabytes. Every part is read
//!   into memory, so an unbounded read is an OOM primitive. [`read_zip_parts`]
//!   caps the entry count, each part's decompressed size, and the running total,
//!   rejecting (never truncating) anything past the limit. It pre-checks each
//!   entry's *declared* uncompressed size (cheap, rejects an honest bomb without
//!   inflating a byte) **and** enforces the cap with a hard-limited reader (so a
//!   header that lies about its size is still bounded to the cap).
//!
//! * **XXE / entity-expansion ("billion laughs")** — a `<!DOCTYPE>` with an
//!   internal/external DTD subset. `quick-xml` (the parser the codecs use) never
//!   resolves external entities or expands custom `<!ENTITY>` definitions, so it
//!   is not directly exploitable; but valid ODF/OOXML package parts never carry a
//!   DTD, so [`read_zip_parts`] rejects any `.xml`/`.rels` part containing a
//!   `<!DOCTYPE`/`<!ENTITY` declaration outright — defence in depth, and it also
//!   shields any future consumer that parses these bytes with a less strict
//!   reader.
//!
//! Both containers ([`crate::codecs::odt`], [`crate::codecs::docx`]) read through
//! this module so the guarantees are uniform.

use crate::format::CodecError;
use std::io::{Cursor, Read};
use zip::ZipArchive;

/// Bounds applied when reading an untrusted office ZIP. The defaults are generous
/// for real documents (a book-length `.docx` with embedded images is comfortably
/// under them) while still turning a decompression bomb into a fast, bounded
/// rejection.
#[derive(Debug, Clone, Copy)]
pub struct ZipLimits {
    /// Maximum number of file entries. Real office packages have a few dozen
    /// parts; thousands is already pathological.
    pub max_parts: usize,
    /// Maximum decompressed size of any single part.
    pub max_part_bytes: u64,
    /// Maximum decompressed size summed across all parts.
    pub max_total_bytes: u64,
}

impl Default for ZipLimits {
    fn default() -> Self {
        Self {
            max_parts: 8_192,
            max_part_bytes: 256 * 1024 * 1024, // 256 MiB per part
            max_total_bytes: 1024 * 1024 * 1024, // 1 GiB total
        }
    }
}

/// Read every file entry of an office ZIP into memory, preserving entry order,
/// under [`ZipLimits`]. Directory entries are skipped. `.xml`/`.rels` parts are
/// scanned for a DTD/entity declaration and rejected if present (XXE defence).
///
/// Returns `(name, bytes)` in original zip order — the byte-faithful part list
/// the containers rebuild from. Any breach of a limit (too many parts, a part
/// or the total over the size cap, or a DTD-bearing XML part) is a
/// [`CodecError::Parse`]; the input is never silently truncated.
pub fn read_zip_parts(bytes: &[u8], limits: &ZipLimits) -> Result<Vec<(String, Vec<u8>)>, CodecError> {
    let mut ar = ZipArchive::new(Cursor::new(bytes)).map_err(|e| CodecError::Parse(format!("zip open: {e}")))?;
    if ar.len() > limits.max_parts {
        return Err(CodecError::Parse(format!(
            "zip has {} entries, over the {} limit (possible zip bomb)",
            ar.len(),
            limits.max_parts
        )));
    }
    let mut parts = Vec::with_capacity(ar.len());
    let mut total: u64 = 0;
    for i in 0..ar.len() {
        let mut f = ar.by_index(i).map_err(|e| CodecError::Parse(format!("zip entry {i}: {e}")))?;
        let name = f.name().to_string();
        if name.ends_with('/') {
            continue; // directory entry
        }
        // Fast path: reject on the entry's *declared* uncompressed size before
        // inflating anything. An honest bomb (the size written by any real zipper)
        // is rejected here without allocating the payload.
        let declared = f.size();
        if declared > limits.max_part_bytes {
            return Err(CodecError::Parse(format!(
                "zip part {name} declares {declared} bytes, over the {} per-part limit (possible zip bomb)",
                limits.max_part_bytes
            )));
        }
        if total.saturating_add(declared) > limits.max_total_bytes {
            return Err(CodecError::Parse(format!(
                "zip parts exceed the {}-byte total limit (possible zip bomb)",
                limits.max_total_bytes
            )));
        }
        // Enforce the cap even if the header lied: a hard-limited reader inflates
        // at most `max_part_bytes + 1`, and hitting that means over-limit.
        let cap = limits.max_part_bytes;
        let mut buf = Vec::new();
        let read = (&mut f)
            .take(cap + 1)
            .read_to_end(&mut buf)
            .map_err(|e| CodecError::Parse(format!("zip read {name}: {e}")))? as u64;
        if read > cap {
            return Err(CodecError::Parse(format!(
                "zip part {name} inflates past the {cap}-byte per-part limit (possible zip bomb)"
            )));
        }
        total = total.saturating_add(read);
        if total > limits.max_total_bytes {
            return Err(CodecError::Parse(format!(
                "zip parts exceed the {}-byte total limit (possible zip bomb)",
                limits.max_total_bytes
            )));
        }
        if is_xml_part(&name) && contains_dtd(&buf) {
            return Err(CodecError::Parse(format!(
                "zip part {name} contains a DTD/entity declaration — rejected (XXE defence)"
            )));
        }
        parts.push((name, buf));
    }
    Ok(parts)
}

/// Whether `name` is a package part we treat as parseable XML (and therefore
/// hold to the no-DTD rule). Binary parts (images, the ODF `mimetype` literal)
/// are exempt — a `<!DOCTYPE` byte sequence inside a JPEG is not a threat, and
/// those parts are carried verbatim, never parsed as XML.
fn is_xml_part(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".xml") || lower.ends_with(".rels")
}

/// Whether `bytes` contains an XML DTD or entity declaration (`<!DOCTYPE` /
/// `<!ENTITY`), scanning case-insensitively. Only the head of the part is
/// examined — a DTD is legal solely in the prolog, so a match past a generous
/// prefix cannot be an active DTD, and bounding the scan keeps it O(prefix).
pub fn contains_dtd(bytes: &[u8]) -> bool {
    // A DTD must appear before the root element; 64 KiB of prolog is far more
    // than any real document uses, and caps the scan cost.
    let head = &bytes[..bytes.len().min(64 * 1024)];
    contains_ci(head, b"<!doctype") || contains_ci(head, b"<!entity")
}

/// Case-insensitive substring search (ASCII), used for the small DTD needles.
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

    #[test]
    fn reads_a_normal_package() {
        let z = zip_with(&[("a.xml", b"<x/>"), ("b.bin", b"\x00\x01\x02")], CompressionMethod::Deflated);
        let parts = read_zip_parts(&z, &ZipLimits::default()).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, "a.xml");
        assert_eq!(parts[1].1, b"\x00\x01\x02");
    }

    #[test]
    fn rejects_too_many_parts() {
        let entries: Vec<(String, Vec<u8>)> = (0..20).map(|i| (format!("p{i}.dat"), vec![i as u8])).collect();
        let refs: Vec<(&str, &[u8])> = entries.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
        let z = zip_with(&refs, CompressionMethod::Stored);
        let limits = ZipLimits { max_parts: 8, ..ZipLimits::default() };
        let err = read_zip_parts(&z, &limits).unwrap_err();
        assert!(format!("{err}").contains("entries"), "expected entry-count rejection, got {err}");
    }

    #[test]
    fn rejects_oversized_part_by_declared_size() {
        // A genuine decompression bomb: ~4 MiB of zeros deflates to a few KB but
        // declares its true (large) size in the entry header. With a small cap the
        // pre-check rejects it without inflating the payload.
        let bomb = vec![0u8; 4 * 1024 * 1024];
        let z = zip_with(&[("content.xml", &bomb)], CompressionMethod::Deflated);
        assert!(z.len() < 64 * 1024, "the bomb must be tiny on disk: {} bytes", z.len());
        let limits = ZipLimits { max_part_bytes: 1024 * 1024, ..ZipLimits::default() };
        let err = read_zip_parts(&z, &limits).unwrap_err();
        assert!(format!("{err}").contains("per-part limit"), "expected size rejection, got {err}");
    }

    #[test]
    fn rejects_oversized_total() {
        let part = vec![7u8; 512 * 1024];
        let z = zip_with(
            &[("a.dat", &part), ("b.dat", &part), ("c.dat", &part)],
            CompressionMethod::Deflated,
        );
        let limits = ZipLimits { max_total_bytes: 1024 * 1024, ..ZipLimits::default() };
        let err = read_zip_parts(&z, &limits).unwrap_err();
        assert!(format!("{err}").contains("total limit"), "expected total rejection, got {err}");
    }

    #[test]
    fn rejects_dtd_in_xml_part() {
        let evil = br#"<?xml version="1.0"?><!DOCTYPE lolz [<!ENTITY a "boom">]><doc>&a;</doc>"#;
        let z = zip_with(&[("content.xml", evil)], CompressionMethod::Deflated);
        let err = read_zip_parts(&z, &ZipLimits::default()).unwrap_err();
        assert!(format!("{err}").contains("DTD"), "expected DTD rejection, got {err}");
    }

    #[test]
    fn allows_doctype_bytes_in_binary_part() {
        // The same bytes inside a non-XML part are harmless (never parsed as XML).
        let z = zip_with(&[("image.png", b"<!DOCTYPE not-xml>")], CompressionMethod::Deflated);
        let parts = read_zip_parts(&z, &ZipLimits::default()).unwrap();
        assert_eq!(parts.len(), 1);
    }
}
