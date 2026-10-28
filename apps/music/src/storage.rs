//! Device-local persistence for Music.
//!
//! Collaborative library state is owned and persisted by the platform app
//! document. This module owns only device-private playback preferences and a
//! content-addressed media cache. Neither local file paths nor media bytes
//! enter the shared document or collaboration channel.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tilo_clock::unix_now_ms;

use crate::model::Playback;

/// Device-private durable state. It deliberately excludes `playing`: a launch
/// may offer a paused resume point, but must never start audio without a local
/// user gesture.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceProfile {
    /// Catalogue choice belongs to this device, never its collaborators.
    #[serde(default)]
    pub spotify_catalogue: bool,
    /// Whether the person connected an online catalogue (Spotify or `YouTube`
    /// Music) on this device. Until they do, Music contacts no provider.
    #[serde(default)]
    pub catalogue_connected: bool,
    /// The last explicitly opened file belongs only to this device.
    #[serde(default)]
    pub local_file: Option<crate::local_file::LocalFile>,
    #[serde(default)]
    pub playback: Playback,
    /// Queued catalogue metadata remains private until explicitly saved to the
    /// library; restoring playback must not depend on a previous search page.
    #[serde(default)]
    pub queue_catalogue: Vec<crate::model::CatalogueTrack>,
}

/// Device-local index for durable media objects. It is deliberately separate
/// from the resume profile and Automerge: track ids are app data, while paths,
/// byte hashes, and eviction timing are private to one device.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaCache {
    #[serde(default)]
    pub entries: BTreeMap<String, MediaCacheEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaCacheEntry {
    /// SHA-256 of the complete media bytes; the object filename is this value.
    pub content_id: String,
    pub bytes: u64,
    /// Last local use, for deterministic least-recently-used eviction.
    pub last_access_ms: u64,
}

const DEFAULT_MEDIA_CACHE_QUOTA: u64 = 1_073_741_824;

/// Local media cache root. The explicit override makes tests and sandboxes
/// hermetic; no cache path is ever exposed through a shared resource.
pub fn media_cache_dir() -> PathBuf {
    if let Ok(path) = std::env::var("TILO_MUSIC_CACHE")
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    dirs_data_dir().join("tilo-music").join("media-cache")
}

fn media_manifest_path(root: &Path) -> PathBuf {
    root.join("manifest.json")
}

fn media_object_path(root: &Path, content_id: &str) -> PathBuf {
    root.join("objects").join(format!("{content_id}.media"))
}

fn valid_content_id(content_id: &str) -> bool {
    content_id.len() == 64 && content_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A regular object file of exactly `bytes` (non-zero) bytes.
fn object_has_size(path: &Path, bytes: u64) -> bool {
    bytes > 0
        && crate::files::metadata(path)
            .is_ok_and(|meta| meta.kind == tilo_files::Kind::File && meta.len == bytes)
}

/// Verify a content-addressed object against its hash.
fn object_matches_entry(path: &Path, entry: &MediaCacheEntry) -> bool {
    if !valid_content_id(&entry.content_id) || !object_has_size(path, entry.bytes) {
        return false;
    }
    let Ok(bytes) = crate::files::read(path) else {
        return false;
    };
    hex::encode(Sha256::digest(&bytes)) == entry.content_id
}

/// Load only entries whose content-addressed byte object remains present and
/// non-empty. A damaged manifest or missing object can never manufacture a
/// `Cached` badge.
pub fn load_media_cache_from(root: &Path) -> MediaCache {
    let Ok(bytes) = crate::files::read(&media_manifest_path(root)) else {
        return MediaCache::default();
    };
    let Ok(mut cache) = serde_json::from_slice::<MediaCache>(&bytes) else {
        return MediaCache::default();
    };
    // Only validate structure/size here. Full hashing is deliberately lazy at
    // playback (`object_path`/`touch`) so a large cache never blocks startup.
    cache.entries.retain(|_, entry| {
        valid_content_id(&entry.content_id)
            && object_has_size(&media_object_path(root, &entry.content_id), entry.bytes)
    });
    cache
}

pub fn load_media_cache() -> MediaCache {
    load_media_cache_from(&media_cache_dir())
}

impl MediaCache {
    #[must_use]
    pub fn object_path(&self, root: &Path, track_id: &str) -> Option<PathBuf> {
        let entry = self.entries.get(track_id)?;
        if !valid_content_id(&entry.content_id) {
            return None;
        }
        let path = media_object_path(root, &entry.content_id);
        object_matches_entry(&path, entry).then_some(path)
    }

    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.entries.values().map(|entry| entry.bytes).sum()
    }

    /// Atomically install complete media bytes, then atomically publish their
    /// manifest entry. A partial transfer is never accepted by this API.
    #[cfg(test)]
    pub fn install_bytes(
        &mut self,
        root: &Path,
        track_id: &str,
        bytes: &[u8],
        quota: u64,
    ) -> std::io::Result<()> {
        if bytes.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "empty media is not cacheable",
            ));
        }
        let content_id = hex::encode(Sha256::digest(bytes));
        let object = media_object_path(root, &content_id);
        if crate::files::metadata(&object).is_err() {
            crate::files::write(&object, bytes)?;
        }
        self.entries.insert(
            track_id.to_string(),
            MediaCacheEntry {
                content_id,
                bytes: bytes.len() as u64,
                last_access_ms: unix_now_ms(),
            },
        );
        self.evict_to_quota(root, quota);
        self.save(root)
    }

    /// Atomically adopt a complete temporary media file into the content store.
    /// The temporary file must be on the same filesystem as `root`; callers
    /// write it under `root/partials` so the final rename is atomic.
    pub fn install_completed_file(
        &mut self,
        root: &Path,
        track_id: &str,
        partial: &Path,
        quota: u64,
    ) -> std::io::Result<()> {
        let data = crate::files::read(partial)?;
        let bytes = data.len() as u64;
        if bytes == 0 {
            crate::files::remove(partial);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "empty media is not cacheable",
            ));
        }
        let content_id = hex::encode(Sha256::digest(&data));
        drop(data);
        let object = media_object_path(root, &content_id);
        if crate::files::metadata(&object).is_ok() {
            crate::files::remove(partial);
        } else {
            crate::files::create_dir_all(&root.join("objects"))?;
            crate::files::rename_noreplace(partial, &object)?;
        }
        self.entries.insert(
            track_id.to_string(),
            MediaCacheEntry {
                content_id,
                bytes,
                last_access_ms: unix_now_ms(),
            },
        );
        self.evict_to_quota(root, quota);
        self.save(root)
    }

    /// Remove a track's manifest entry and its unreferenced byte object.
    ///
    /// # Errors
    ///
    /// Returns an error when the replacement manifest cannot be written.
    pub fn remove(&mut self, root: &Path, track_id: &str) -> std::io::Result<()> {
        let removed = self.entries.remove(track_id);
        // Publish the removal before deleting bytes: a failed manifest write
        // must leave the offline copy available both here and after restart.
        if let Err(error) = self.save(root) {
            if let Some(entry) = removed {
                self.entries.insert(track_id.to_owned(), entry);
            }
            return Err(error);
        }
        if let Some(entry) = removed
            && !self
                .entries
                .values()
                .any(|other| other.content_id == entry.content_id)
        {
            crate::files::remove(&media_object_path(root, &entry.content_id));
        }
        Ok(())
    }

    /// Validate and mark one cached object as recently used.
    ///
    /// # Errors
    ///
    /// Returns an error when a stale entry cannot be removed from the manifest
    /// or when the updated access time cannot be persisted.
    pub fn touch(&mut self, root: &Path, track_id: &str) -> std::io::Result<Option<PathBuf>> {
        let Some(entry) = self.entries.get_mut(track_id) else {
            return Ok(None);
        };
        let path = media_object_path(root, &entry.content_id);
        if !object_matches_entry(&path, entry) {
            self.entries.remove(track_id);
            self.save(root)?;
            return Ok(None);
        }
        entry.last_access_ms = unix_now_ms();
        self.save(root)?;
        Ok(Some(path))
    }

    fn evict_to_quota(&mut self, root: &Path, quota: u64) {
        while self.total_bytes() > quota {
            let Some(track_id) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_access_ms)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            let Some(entry) = self.entries.remove(&track_id) else {
                break;
            };
            if !self
                .entries
                .values()
                .any(|other| other.content_id == entry.content_id)
            {
                crate::files::remove(&media_object_path(root, &entry.content_id));
            }
        }
    }

    fn save(&self, root: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        crate::files::write(&media_manifest_path(root), &bytes)
    }
}

/// Finish a background partial transfer without exposing its path outside the
/// device-local storage layer.
pub fn install_completed_media_file(
    root: &Path,
    track_id: &str,
    partial: &Path,
    quota: u64,
) -> std::io::Result<()> {
    let mut cache = load_media_cache_from(root);
    cache.install_completed_file(root, track_id, partial, quota)
}

pub fn media_cache_quota() -> u64 {
    std::env::var("TILO_MUSIC_CACHE_QUOTA_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|quota| *quota > 0)
        .unwrap_or(DEFAULT_MEDIA_CACHE_QUOTA)
}

impl DeviceProfile {
    #[must_use]
    pub fn paused(mut self) -> Self {
        self.playback.playing = false;
        self
    }
}

/// Resolve the local-only profile path. `TILO_MUSIC_PROFILE` is useful for
/// sandboxed tests; it is never a replication input.
/// Whether this device keeps the playback profile in a file. A browser page
/// has no file paths: the player keeps its state in memory there, and
/// opening or playing a file never fails for want of saving it.
pub fn device_profile_has_a_place() -> bool {
    tilo_paths::availability(tilo_paths::Feature::NativePaths)
        == tilo_paths::Availability::Available
}

pub fn device_profile_path() -> PathBuf {
    if let Ok(p) = std::env::var("TILO_MUSIC_PROFILE") {
        return PathBuf::from(p);
    }
    dirs_data_dir()
        .join("tilo-music")
        .join("device-profile.json")
}

/// Read a local profile. A missing or malformed profile is treated as a safe
/// empty/paused state; callers can continue using the library without trusting
/// corrupt device data.
pub fn load_device_profile_from(path: &Path) -> DeviceProfile {
    crate::files::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<DeviceProfile>(&bytes).ok())
        .unwrap_or_default()
        .paused()
}

pub fn load_device_profile() -> DeviceProfile {
    load_device_profile_from(&device_profile_path())
}

/// Atomically store paused device-local resume state. Cache availability is
/// not in it: the media manifest is the sole authority for bytes.
///
/// # Errors
///
/// Returns an I/O error when the profile directory cannot be created or the
/// atomic write cannot be completed.
pub fn save_device_profile_to(path: &Path, profile: &DeviceProfile) -> std::io::Result<()> {
    let persisted = profile.clone().paused();
    let bytes = serde_json::to_vec(&persisted).map_err(std::io::Error::other)?;
    crate::files::write(path, &bytes)
}

///
/// # Errors
///
/// Propagates local profile-directory or atomic-write failures.
pub fn save_device_profile(profile: &DeviceProfile) -> std::io::Result<()> {
    save_device_profile_to(&device_profile_path(), profile)
}

/// Native device-local data directory, with an absolute XDG override for
/// sandboxed launches. Windows resolves to `LocalAppData` through `tilo-paths`.
fn dirs_data_dir() -> PathBuf {
    tilo_paths::env_dir("XDG_DATA_HOME")
        .or_else(tilo_paths::data_local_dir)
        .or_else(tilo_paths::temp_dir)
        // No file system (the page): an unreachable relative path fails the
        // profile's reads and writes, and the player keeps its state in
        // memory.
        .unwrap_or_else(|| PathBuf::from("tilo-music"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_profile_round_trips_locally_and_forces_paused_resume() {
        let dir = std::env::temp_dir().join(format!("tilo-music-profile-{}", std::process::id()));
        let path = dir.join("device-profile.json");
        let mut profile = DeviceProfile {
            spotify_catalogue: true,
            ..DeviceProfile::default()
        };
        profile.playback.current = Some("tk_g1".into());
        profile.playback.queue = vec!["tk_g1".into()];
        profile.playback.playing = true;
        profile.playback.position_ms = 12_345;
        profile.playback.volume = 37;
        save_device_profile_to(&path, &profile).expect("save local profile");

        let loaded = load_device_profile_from(&path);
        assert!(loaded.spotify_catalogue);
        assert_eq!(loaded.playback.current, profile.playback.current);
        assert_eq!(loaded.playback.position_ms, 12_345);
        assert_eq!(loaded.playback.volume, 37);
        assert!(!loaded.playback.playing, "restart never autoplays");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_offline_removal_preserves_the_cached_audio() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut cache = MediaCache::default();
        cache
            .install_bytes(root, "track", b"saved audio", 1024)
            .unwrap();
        let before = cache.clone();
        let manifest = media_manifest_path(root);
        let saved_manifest = crate::files::read(&manifest).unwrap();
        std::fs::remove_file(&manifest).unwrap();
        std::fs::create_dir(&manifest).unwrap();

        assert!(cache.remove(root, "track").is_err());
        assert_eq!(cache, before);
        let object = cache.object_path(root, "track").unwrap();
        assert_eq!(crate::files::read(&object).unwrap(), b"saved audio");

        std::fs::remove_dir(&manifest).unwrap();
        crate::files::write(&manifest, &saved_manifest).unwrap();
        assert_eq!(load_media_cache_from(root), before);
        cache.remove(root, "track").unwrap();
        assert!(!object.exists());
        assert!(load_media_cache_from(root).entries.is_empty());
    }

    #[test]
    fn media_cache_is_atomic_bounded_and_rejects_corruption() {
        {
            let root =
                std::env::temp_dir().join(format!("tilo-music-media-{}", std::process::id()));
            let mut cache = MediaCache::default();
            cache
                .install_bytes(&root, "track-a", b"real media bytes", 1024)
                .expect("install");
            let path = cache.object_path(&root, "track-a").expect("byte object");
            assert_eq!(
                path.extension().and_then(|value| value.to_str()),
                Some("media")
            );
            assert_eq!(
                std::fs::read(&path).expect("read object"),
                b"real media bytes"
            );
            let reloaded = load_media_cache_from(&root);
            assert_eq!(reloaded.entries, cache.entries);
            let _ = std::fs::remove_dir_all(root);
        }

        {
            let root =
                std::env::temp_dir().join(format!("tilo-music-media-evict-{}", std::process::id()));
            let mut cache = MediaCache::default();
            cache
                .install_bytes(&root, "old", b"1234", 6)
                .expect("first");
            cache
                .entries
                .get_mut("old")
                .expect("old entry")
                .last_access_ms = 0;
            cache
                .install_bytes(&root, "new", b"5678", 6)
                .expect("second evicts first");
            assert!(cache.object_path(&root, "old").is_none());
            assert!(cache.object_path(&root, "new").is_some());
            let _ = std::fs::remove_dir_all(root);
        }

        {
            let root = std::env::temp_dir()
                .join(format!("tilo-music-media-same-size-{}", std::process::id()));
            let mut cache = MediaCache::default();
            cache
                .install_bytes(&root, "track-a", b"real media bytes", 1024)
                .expect("install");
            let path = cache.object_path(&root, "track-a").expect("object");
            std::fs::write(&path, b"forgededia bytes").expect("same-sized corruption");
            assert!(cache.object_path(&root, "track-a").is_none());
            assert!(cache.touch(&root, "track-a").expect("touch").is_none());
            let _ = std::fs::remove_dir_all(root);
        }

        {
            let root = std::env::temp_dir()
                .join(format!("tilo-music-media-malicious-{}", std::process::id()));
            std::fs::create_dir_all(&root).expect("root");
            let manifest = serde_json::json!({
                "entries": {
                    "track-a": { "content_id": "../../outside", "bytes": 9, "last_access_ms": 0 }
                }
            });
            std::fs::write(media_manifest_path(&root), manifest.to_string()).expect("manifest");
            assert!(load_media_cache_from(&root).entries.is_empty());
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
