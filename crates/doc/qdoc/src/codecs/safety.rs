//! Hardening for parsing **untrusted** office packages (`.odt`/`.docx`): an office
//! file is an attacker-controlled ZIP of attacker-controlled XML.
//!
//! * **ZIP bombs** — a few KB that decompress to gigabytes; every part is read into
//!   memory, so an unbounded read is an OOM primitive. [`read_zip_parts`] caps entry
//!   count, per-part size, and the running total (never truncating, only rejecting),
//!   checking each entry's *declared* size first, then enforcing the cap with a
//!   hard-limited reader so a lying header still can't exceed it.
//!
//! * **XXE / "billion laughs"** — `quick-xml` never resolves external entities or
//!   expands `<!ENTITY>`, so a `<!DOCTYPE>` isn't directly exploitable here, but
//!   valid ODF/OOXML parts never carry one either — [`read_zip_parts`] rejects any
//!   `.xml`/`.rels` part containing `<!DOCTYPE`/`<!ENTITY` outright, in depth.
//!
//! Both containers read through this module so the guarantees are uniform.

use crate::format::CodecError;
use std::io::{Cursor, Read};
use zip::ZipArchive;

/// Bounds applied when reading an untrusted office ZIP. Defaults are generous for
/// real documents (comfortably fits a book-length `.docx` with embedded images)
/// while turning a decompression bomb into a fast, bounded rejection.
#[derive(Debug, Clone, Copy)]
pub struct ZipLimits {
    /// Maximum number of file entries — real packages have a few dozen; thousands is already pathological.
    pub max_parts: usize,
    pub max_part_bytes: u64,
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

/// Reads every file entry of an office ZIP into memory, preserving order, under
/// [`ZipLimits`]. Directory entries are skipped; `.xml`/`.rels` parts are scanned
/// for a DTD/entity declaration and rejected if found (XXE defence).
///
/// # Errors
/// Any breach of a limit is a [`CodecError::Parse`] — the input is never silently
/// truncated. A malformed archive, an unreadable entry and a DTD-bearing XML part
/// are reported the same way.
pub fn read_zip_parts(
    bytes: &[u8],
    limits: &ZipLimits,
) -> Result<Vec<(String, Vec<u8>)>, CodecError> {
    let mut ar = ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| CodecError::Parse(format!("zip open: {e}")))?;
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
        let mut f = ar
            .by_index(i)
            .map_err(|e| CodecError::Parse(format!("zip entry {i}: {e}")))?;
        let name = f.name().to_string();
        if name.ends_with('/') {
            continue;
        }
        // Fast path: reject on the entry's *declared* uncompressed size before inflating
        // anything — an honest bomb is rejected here without allocating the payload.
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
            .take(cap.saturating_add(1))
            .read_to_end(&mut buf)
            .map_err(|e| CodecError::Parse(format!("zip read {name}: {e}")))?
            as u64;
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

/// Whether `name` is a package part we hold to the no-DTD rule (parseable XML).
/// Binary parts (images, the ODF `mimetype` literal) are exempt — a `<!DOCTYPE`
/// byte sequence inside a JPEG isn't a threat, and such parts are never parsed as XML.
fn is_xml_part(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("xml") || ext.eq_ignore_ascii_case("rels"))
}

/// Whether `bytes` contains an XML DTD/entity declaration (`<!DOCTYPE`/`<!ENTITY`),
/// scanned case-insensitively. Only the head is examined — a DTD is legal solely
/// in the prolog, so a match past a generous prefix can't be an active DTD.
pub fn contains_dtd(bytes: &[u8]) -> bool {
    // A DTD must appear before the root element; 64 KiB of prolog is far more
    // than any real document uses, and caps the scan cost.
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

    #[test]
    fn reads_a_normal_package() {
        let z = zip_with(
            &[("a.xml", b"<x/>"), ("b.bin", b"\x00\x01\x02")],
            CompressionMethod::Deflated,
        );
        let parts = read_zip_parts(&z, &ZipLimits::default()).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, "a.xml");
        assert_eq!(parts[1].1, b"\x00\x01\x02");
    }

    #[test]
    fn rejects_too_many_parts() {
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
    }

    #[test]
    fn rejects_oversized_part_by_declared_size() {
        // A genuine bomb: ~4 MiB of zeros deflates to a few KB but declares its true size
        // in the entry header — the pre-check must reject it without inflating the payload.
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
    }

    #[test]
    fn rejects_oversized_total() {
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
    }

    #[test]
    fn rejects_dtd_in_xml_part() {
        let evil = br#"<?xml version="1.0"?><!DOCTYPE lolz [<!ENTITY a "boom">]><doc>&a;</doc>"#;
        let z = zip_with(&[("content.xml", evil)], CompressionMethod::Deflated);
        let err = read_zip_parts(&z, &ZipLimits::default()).unwrap_err();
        assert!(
            format!("{err}").contains("DTD"),
            "expected DTD rejection, got {err}"
        );
    }

    #[test]
    fn allows_doctype_bytes_in_binary_part() {
        // The same bytes inside a non-XML part are harmless (never parsed as XML).
        let z = zip_with(
            &[("image.png", b"<!DOCTYPE not-xml>")],
            CompressionMethod::Deflated,
        );
        let parts = read_zip_parts(&z, &ZipLimits::default()).unwrap();
        assert_eq!(parts.len(), 1);
    }
}
