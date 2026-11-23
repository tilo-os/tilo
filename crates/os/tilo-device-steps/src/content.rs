//! The bytes a step names by hash: the device-control content store, which the daemon fills
//! from a caller's content puts before it starts the step.

use std::path::{Path, PathBuf};

use crate::error::DaemonError;

/// Where the daemon keeps what callers put, one file per blake3 hex.
pub const CONTENT_DIR: &str = "/var/lib/tilo/device-control/content";

/// Is `hash` a lowercase blake3 hex address, so it names a file and never a path?
pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The file holding `hash` in the store at `dir`.
///
/// # Errors
/// A hash that is not one, or content the store does not hold.
pub fn blob(dir: &Path, hash: &str) -> Result<PathBuf, DaemonError> {
    if !valid_hash(hash) {
        return Err(DaemonError::InvalidParams(format!(
            "'{hash}' is not a lowercase blake3 address"
        )));
    }
    let path = dir.join(hash);
    if path.is_file() {
        Ok(path)
    } else {
        Err(DaemonError::ContentNotFound(hash.to_owned()))
    }
}
