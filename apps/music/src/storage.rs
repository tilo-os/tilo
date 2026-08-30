//! On-disk persistence for the music library document.
//!
//! The library + play-state is an Automerge document ([`crate::doc`]). For the
//! qOS self-hosting model — and so a headless home server's replica survives
//! restarts — the raw `doc.save()` bytes are persisted to disk (NOT a JSON
//! projection), exactly like `chat/src/storage.rs`: persisting the Automerge
//! save means a future peer can merge against the file directly.
//!
//! The default GUI app keeps its document in memory, starting from an **empty**
//! library each run (real tracks arrive from playback, search, or a synced
//! relay — never fabricated); this module is the seam the headless relay
//! ([`crate::relay`]) uses to load and atomically persist its authoritative
//! copy.

use std::path::{Path, PathBuf};

use automerge::AutoCommit;

/// Resolve the on-disk path for the library document. Honours an explicit
/// override (`QOS_MUSIC_DATA`) so a relay can be pointed at a chosen location;
/// otherwise `$XDG_DATA_HOME/qos-music/library.automerge`.
pub fn doc_path() -> PathBuf {
    if let Ok(p) = std::env::var("QOS_MUSIC_DATA") {
        return PathBuf::from(p);
    }
    let base = dirs_data_dir();
    base.join("qos-music").join("library.automerge")
}

/// Best-effort XDG data dir without pulling the `dirs` crate: honour
/// `XDG_DATA_HOME`, else `$HOME/.local/share`, else the current dir.
fn dirs_data_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_DATA_HOME")
        && !x.is_empty()
    {
        return PathBuf::from(x);
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join(".local").join("share");
    }
    PathBuf::from(".")
}

/// Load a document from `path`, or `None` if it doesn't exist / can't parse.
pub fn load_from(path: &Path) -> Option<AutoCommit> {
    let bytes = std::fs::read(path).ok()?;
    AutoCommit::load(&bytes).ok()
}

/// Atomically persist `doc` to `path` (temp file + rename) so a crash mid-write
/// can't corrupt the saved library.
pub fn save_to(path: &Path, doc: &mut AutoCommit) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = doc.save();
    atomic_file::write(path, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{doc, seed};

    #[test]
    fn save_then_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("qos-music-test-{}", std::process::id()));
        let path = dir.join("library.automerge");
        let mut d = AutoCommit::new();
        let lib = seed::sample_library();
        doc::seed(&mut d, &lib, &seed::sample_playback(&lib));
        save_to(&path, &mut d).expect("save");

        let loaded = load_from(&path).expect("load");
        assert_eq!(doc::load_library(&loaded).total_tracks(), lib.tracks.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_is_none() {
        let path = std::env::temp_dir().join("qos-music-nonexistent-xyz.automerge");
        assert!(load_from(&path).is_none());
    }
}
