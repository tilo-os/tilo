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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn json_values_round_trip_and_missing_or_malformed_files_load_as_default() {
        {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("store.json");
            assert!(load_or_default::<BTreeSet<String>>(&p).is_empty());
            std::fs::write(&p, b"{not json").unwrap();
            assert!(load_or_default::<BTreeSet<String>>(&p).is_empty());
            std::fs::write(&p, b"42").unwrap();
            assert!(load_or_default::<BTreeSet<String>>(&p).is_empty());
        }
        {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("nested/store.json");
            let value: BTreeSet<String> = ["a".to_string(), "b".to_string()].into();
            write_private(&p, &value).unwrap();
            assert_eq!(load_or_default::<BTreeSet<String>>(&p), value);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600);
            }
        }
    }
}
