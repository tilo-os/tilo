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
