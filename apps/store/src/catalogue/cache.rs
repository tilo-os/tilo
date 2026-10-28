//! The catalogue as the last refresh left it, kept on disk so a new window
//! paints real apps in its first frame and the network refresh only updates
//! them.
//!
//! Install state is kept too and is a few minutes old at most when read
//! back; every package operation is checked again against the host's answer
//! before it runs, so a stale row can show a stale button but never start a
//! wrong operation.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::StoreItem;

/// The file under the Store's cache. Its name carries the format: a new
/// format is a new file, and an old one is simply not read.
const FILE: &str = "catalogue-v1.json";
/// A kept catalogue larger than this is not read: the first frame must not
/// wait on it.
const MAX_FILE_BYTES: u64 = 24 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Stored {
    locale: String,
    saved_unix: u64,
    items: Vec<StoreItem>,
}

/// A catalogue read back from disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cached {
    /// The language its localized fields are in.
    pub locale: String,
    /// When it was kept, Unix seconds.
    pub saved_unix: u64,
    pub items: Vec<StoreItem>,
}

fn path() -> Option<PathBuf> {
    crate::config::cache_dir().map(|dir| dir.join(FILE))
}

/// The kept catalogue, if there is a readable one.
#[must_use]
pub fn load() -> Option<Cached> {
    let path = path()?;
    if std::fs::metadata(&path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let stored: Stored = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    Some(Cached {
        locale: stored.locale,
        saved_unix: stored.saved_unix,
        items: stored.items,
    })
}

/// Keep `items` for the next window. Rows a capture or a handed file added
/// (an opened `.flatpakref`, a reviewed local package) are the caller's to
/// leave out.
pub fn save(items: &[StoreItem], locale: &str) -> Result<(), String> {
    let path = path().ok_or_else(|| "no cache directory".to_owned())?;
    let stored = Stored {
        locale: locale.to_owned(),
        saved_unix: super::now_unix(),
        items: items.to_vec(),
    };
    let bytes = serde_json::to_vec(&stored).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent(&path, &bytes)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_catalogue_reads_back_as_it_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_STORE_CACHE_DIR", dir.path());
        assert!(load().is_none(), "no catalogue kept yet");
        let items = crate::fixture::fixture_catalog();
        save(&items, "de-DE").unwrap();
        let cached = load().unwrap();
        assert_eq!(cached.items, items);
        assert_eq!(cached.locale, "de-DE");
        std::fs::write(dir.path().join(FILE), b"{ not json").unwrap();
        assert!(load().is_none(), "an unreadable file is ignored, not fatal");
    }
}
