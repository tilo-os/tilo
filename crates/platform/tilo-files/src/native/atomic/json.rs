//! Small JSON state files: forgiving load, crash-safe save.
//!
//! Portal decisions, per-app preferences and similar stores are one `serde`
//! value in one file. Reading must never take the owner down — a missing,
//! unreadable or malformed file is the default state — and saving goes through
//! the crate's temp-file, fsync, rename, directory-fsync path.

use std::io;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Read `path` as JSON, or `T::default()` when it is missing, unreadable or
/// does not decode as `T`.
#[must_use]
pub fn load_or_default<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Encode `value` as compact JSON and save it with [`crate::native::atomic::write_private`]:
/// owner-only parent directory, `0600` file, durable replacement.
///
/// # Errors
///
/// Returns an `InvalidData` error if `value` cannot be encoded, or any error
/// [`crate::native::atomic::write_private`] documents.
pub fn write_private<T: Serialize + ?Sized>(path: &Path, value: &T) -> io::Result<()> {
    crate::native::atomic::write_private(path, &serde_json::to_vec(value)?)
}
