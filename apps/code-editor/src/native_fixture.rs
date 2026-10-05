//! Deterministic source fixtures for the production `code-editor FILE` route.
//! Hashes describe UTF-8 source bytes, not Automerge serialization or pixels.

use std::io::Write;
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Serialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub hash_algorithm: &'static str,
    pub lines: usize,
    pub editor_id: &'static str,
    pub body_hash: String,
    pub body_bytes: usize,
    pub prefixed_x_hash: String,
    pub prefixed_x_bytes: usize,
}

pub(crate) fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Write a new 2,000-line Rust file; refuse existing files and symlinks.
///
/// # Errors
/// Returns file creation, writing, or synchronization errors.
pub fn seed(path: &Path) -> std::io::Result<Manifest> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let mut source = String::new();
    for index in 0..500 {
        use std::fmt::Write as _;
        write!(
            source,
            "pub fn transform_{index:04}(value: u64) -> u64 {{\n    let shifted = value.wrapping_add({index});\n    shifted.rotate_left(3)\n}}\n"
        )
        .map_err(std::io::Error::other)?;
    }
    let prefixed = format!("x{source}");
    file.write_all(source.as_bytes())?;
    file.sync_all()?;
    Ok(Manifest {
        schema_version: 1,
        hash_algorithm: "sha256",
        lines: source.lines().count(),
        editor_id: "editor_view",
        body_hash: hash(source.as_bytes()),
        body_bytes: source.len(),
        prefixed_x_hash: hash(prefixed.as_bytes()),
        prefixed_x_bytes: prefixed.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_fixture_is_deterministic_and_does_not_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.rs");
        let second = directory.path().join("second.rs");
        let a = seed(&first).unwrap();
        let b = seed(&second).unwrap();
        let bytes = std::fs::read(&first).unwrap();
        assert_eq!(bytes, std::fs::read(&second).unwrap());
        assert_eq!(a.lines, 2000);
        assert_eq!(a.body_bytes, bytes.len());
        assert_eq!(a.body_hash, b.body_hash);
        assert_eq!(a.body_hash, hash(&bytes));
        assert_eq!(
            a.prefixed_x_hash,
            hash(&[b"x".as_slice(), bytes.as_slice()].concat())
        );
        assert!(seed(&first).is_err());
        assert_eq!(std::fs::read(first).unwrap(), bytes);
    }
}
