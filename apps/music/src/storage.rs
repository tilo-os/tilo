//! On-disk persistence for the music library document.
//!
//! The collaborative library is an Automerge document ([`crate::doc`]). For the
//! qOS self-hosting model — and so a headless home server's replica survives
//! restarts — the raw `doc.save()` bytes are persisted to disk (NOT a JSON
//! projection), exactly like `chat/src/storage.rs`: persisting the Automerge
//! save means a future peer can merge against the file directly.
//!
//! The default GUI app keeps its document in memory, starting from an **empty**
//! library each run (real tracks arrive from playback, search, or a synced
//! relay — never fabricated); this module is the seam the headless relay
//! ([`crate::relay`]) uses to load and atomically persist its authoritative
//! copy. Playback and credentials remain device-local effects and are
//! intentionally absent from this file. Music currently has no byte-backed
//! download cache, so it persists only paused resume state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use crate::model::{CacheState, Playback};

/// Device-private durable state. It deliberately excludes `playing`: a launch
/// may offer a paused resume point, but must never start audio without a local
/// user gesture.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceProfile {
    /// Legacy field accepted on read so older profiles migrate safely. It is
    /// never written or used to advertise offline playback without media bytes.
    #[serde(default)]
    pub cache: BTreeMap<String, CacheState>,
    #[serde(default)]
    pub playback: Playback,
}

impl DeviceProfile {
    #[must_use]
    pub fn paused(mut self) -> Self {
        self.playback.playing = false;
        self
    }
}

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

/// Resolve the local-only profile path. `QOS_MUSIC_PROFILE` is useful for
/// sandboxed tests; unlike `doc_path`, it is never a replication input.
pub fn device_profile_path() -> PathBuf {
    if let Ok(p) = std::env::var("QOS_MUSIC_PROFILE") {
        return PathBuf::from(p);
    }
    dirs_data_dir()
        .join("qos-music")
        .join("device-profile.json")
}

/// Read a local profile. A missing or malformed profile is treated as a safe
/// empty/paused state; callers can continue using the library without trusting
/// corrupt device data.
pub fn load_device_profile_from(path: &Path) -> DeviceProfile {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<DeviceProfile>(&bytes).ok())
        .unwrap_or_default()
        .paused()
}

pub fn load_device_profile() -> DeviceProfile {
    load_device_profile_from(&device_profile_path())
}

/// Atomically store paused device-local resume state. Cache metadata is
/// deliberately dropped until it is backed by durable media bytes.
///
/// # Errors
///
/// Returns an I/O error when the profile directory cannot be created or the
/// atomic write cannot be completed.
pub fn save_device_profile_to(path: &Path, profile: &DeviceProfile) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut persisted = profile.clone().paused();
    persisted.cache.clear();
    let bytes = serde_json::to_vec(&persisted).map_err(std::io::Error::other)?;
    atomic_file::write(path, &bytes)
}

///
/// # Errors
///
/// Propagates local profile-directory or atomic-write failures.
pub fn save_device_profile(profile: &DeviceProfile) -> std::io::Result<()> {
    save_device_profile_to(&device_profile_path(), profile)
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
///
/// # Errors
///
/// Returns an error if the parent directory can't be created, or if the
/// atomic temp-file write/rename fails (e.g. permissions, no space left).
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

    #[test]
    fn device_profile_round_trips_locally_and_forces_paused_resume() {
        let dir = std::env::temp_dir().join(format!("qos-music-profile-{}", std::process::id()));
        let path = dir.join("device-profile.json");
        let mut profile = DeviceProfile::default();
        profile.cache.insert("tk_g1".into(), CacheState::Cached);
        profile.playback.current = Some("tk_g1".into());
        profile.playback.queue = vec!["tk_g1".into()];
        profile.playback.playing = true;
        profile.playback.position_ms = 12_345;
        profile.playback.volume = 37;
        save_device_profile_to(&path, &profile).expect("save local profile");

        let loaded = load_device_profile_from(&path);
        assert!(
            loaded.cache.is_empty(),
            "metadata never pretends bytes are cached"
        );
        assert_eq!(loaded.playback.current, profile.playback.current);
        assert_eq!(loaded.playback.position_ms, 12_345);
        assert_eq!(loaded.playback.volume, 37);
        assert!(!loaded.playback.playing, "restart never autoplays");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
