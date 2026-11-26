//! Hardening for parsing **untrusted** office packages (`.odt`/`.docx`/`.xlsx`/`.ods`):
//! an office file is an attacker-controlled ZIP of attacker-controlled XML.
//!
//! The zip-bomb and XXE defence lives in the `bounded-zip` crate, shared with every
//! other package reader in the tree; this module adapts it to [`CodecError`] so all
//! qdoc containers read through one path and the guarantees are uniform.

use crate::format::CodecError;

pub use bounded_zip::{ZipLimits, contains_dtd};

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
    bounded_zip::read_zip_parts(bytes, limits).map_err(|e| CodecError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    #[test]
    fn limit_breaches_and_dtds_surface_as_parse_errors() {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zw.start_file("content.xml", opts).unwrap();
        zw.write_all(br#"<!DOCTYPE lolz [<!ENTITY a "boom">]><doc>&a;</doc>"#)
            .unwrap();
        let z = zw.finish().unwrap().into_inner();
        let err = read_zip_parts(&z, &ZipLimits::default()).unwrap_err();
        assert!(
            matches!(&err, CodecError::Parse(msg) if msg.contains("DTD")),
            "expected DTD rejection, got {err}"
        );
        let limits = ZipLimits {
            max_part_bytes: 8,
            ..ZipLimits::default()
        };
        let err = read_zip_parts(&z, &limits).unwrap_err();
        assert!(
            matches!(&err, CodecError::Parse(msg) if msg.contains("per-part limit")),
            "expected size rejection, got {err}"
        );
    }
}
