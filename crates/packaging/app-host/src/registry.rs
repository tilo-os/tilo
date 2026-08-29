//! On-disk registry: one entry per known package, persisted as TOML.
//!
//! The registry tracks:
//!
//! - the `Pinning` state (`Pinned` — never evicted, `Tried` — LRU),
//! - `installed_at` and `last_used_unix` (for LRU ordering),
//! - the payload directory the launcher will use as `--payload-root`,
//! - the manifest TOML text so the service can echo it back without
//!   re-reading a file.
//!
//! Persistence is atomic: `save` writes a sibling `.tmp` and renames it
//! over the target so a crash never leaves a half-written registry.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use pkg::id::PackageId;

/// Whether the caller has committed to keeping this package around.
///
/// `Pinned` entries are exempt from GC; `Tried` entries are candidates
/// for LRU eviction under size pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Pinning {
    Tried,
    Pinned,
}

impl Pinning {
    pub fn as_str(&self) -> &'static str {
        match self {
            Pinning::Tried => "tried",
            Pinning::Pinned => "pinned",
        }
    }
}

/// One row of the on-disk registry.
///
/// `id` is the content-addressed BLAKE3 hash of the signed-root — the
/// same 32 bytes the Cap'n Proto interface echoes to callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub id: PackageId,
    pub pinning: Pinning,
    pub payload_root: PathBuf,
    pub manifest_text: String,
    pub installed_at_unix: u64,
    pub last_used_unix: u64,
}

impl RegistryEntry {
    /// Convenience constructor. Sets `installed_at_unix` and
    /// `last_used_unix` to `now`.
    pub fn new_now(
        id: PackageId,
        pinning: Pinning,
        payload_root: PathBuf,
        manifest_text: String,
    ) -> Self {
        let n = unix_now();
        Self {
            id,
            pinning,
            payload_root,
            manifest_text,
            installed_at_unix: n,
            last_used_unix: n,
        }
    }
}

/// TOML-persisted registry. In-memory shape: a `BTreeMap` keyed by the
/// 32-byte package id so iteration order is stable across saves.
///
/// Saves are atomic (tmp+rename).
#[derive(Debug, Default, Clone)]
pub struct Registry {
    entries: BTreeMap<[u8; 32], RegistryEntry>,
}

impl Registry {
    /// Fresh empty registry (nothing loaded from disk).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load from `path`. Missing file → empty registry (fresh install).
    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        match fs::read(path) {
            Ok(bytes) => {
                let s = std::str::from_utf8(&bytes)
                    .map_err(|e| RegistryError::BadUtf8(format!("{}: {e}", path.display())))?;
                let file: RegistryFile = toml::from_str(s)
                    .map_err(|e| RegistryError::Toml(format!("{}: {e}", path.display())))?;
                let mut entries = BTreeMap::new();
                for e in file.entry {
                    entries.insert(*e.id.as_bytes(), e);
                }
                Ok(Self { entries })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(e) => Err(RegistryError::Io(e)),
        }
    }

    /// Persist to `path`. Atomic: writes `<path>.tmp` and renames.
    /// Creates parent directories as needed.
    pub fn save(&self, path: &Path) -> Result<(), RegistryError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut file = RegistryFile {
            entry: Vec::with_capacity(self.entries.len()),
        };
        for e in self.entries.values() {
            file.entry.push(e.clone());
        }
        // Deterministic order by (installed_at_unix, id) so text
        // diffs are stable across saves.
        file.entry.sort_by(|a, b| {
            a.installed_at_unix
                .cmp(&b.installed_at_unix)
                .then_with(|| a.id.as_bytes().cmp(b.id.as_bytes()))
        });
        let text = toml::to_string_pretty(&file).map_err(|e| RegistryError::Toml(e.to_string()))?;
        atomic_file::write(path, text.as_bytes())?;
        Ok(())
    }

    /// All entries in insertion (id) order.
    pub fn entries(&self) -> impl Iterator<Item = &RegistryEntry> {
        self.entries.values()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up a single entry by id.
    pub fn get(&self, id: &PackageId) -> Option<&RegistryEntry> {
        self.entries.get(id.as_bytes())
    }

    /// Insert / overwrite.
    pub fn insert(&mut self, entry: RegistryEntry) {
        self.entries.insert(*entry.id.as_bytes(), entry);
    }

    /// Drop an entry. `true` if it existed.
    pub fn remove(&mut self, id: &PackageId) -> bool {
        self.entries.remove(id.as_bytes()).is_some()
    }

    /// Flip pinning; `true` if the id existed.
    pub fn set_pinning(&mut self, id: &PackageId, pinning: Pinning) -> bool {
        if let Some(e) = self.entries.get_mut(id.as_bytes()) {
            e.pinning = pinning;
            true
        } else {
            false
        }
    }

    /// LRU touch: bump `last_used_unix` to now. `true` if the id
    /// existed.
    pub fn touch(&mut self, id: &PackageId) -> bool {
        if let Some(e) = self.entries.get_mut(id.as_bytes()) {
            e.last_used_unix = unix_now();
            true
        } else {
            false
        }
    }
}

/// TOML wire schema. `[[entry]]` array-of-tables layout for a
/// human-readable file.
#[derive(Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    entry: Vec<RegistryEntry>,
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("TOML: {0}")]
    Toml(String),
    #[error("registry file is not valid UTF-8: {0}")]
    BadUtf8(String),
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_id(byte: u8) -> PackageId {
        PackageId::from_bytes([byte; 32])
    }

    #[test]
    fn empty_registry_round_trips_through_load_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.toml");
        let r = Registry::empty();
        r.save(&path).unwrap();
        let r2 = Registry::load(&path).unwrap();
        assert_eq!(r2.len(), 0);
    }

    #[test]
    fn insert_pin_unpin_touch_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.toml");
        let mut r = Registry::empty();
        let id = fake_id(1);
        r.insert(RegistryEntry::new_now(
            id,
            Pinning::Tried,
            dir.path().join("payloads").join(id.to_hex()),
            "manifest".into(),
        ));
        assert!(r.set_pinning(&id, Pinning::Pinned));
        r.save(&path).unwrap();

        let r2 = Registry::load(&path).unwrap();
        assert_eq!(r2.get(&id).unwrap().pinning, Pinning::Pinned);
    }

    #[test]
    fn atomic_save_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.toml");
        let mut r = Registry::empty();
        r.insert(RegistryEntry::new_now(
            fake_id(2),
            Pinning::Tried,
            dir.path().into(),
            String::new(),
        ));
        r.save(&path).unwrap();
        let tmp = path.with_extension("toml.tmp");
        assert!(!tmp.exists(), "tmp should be renamed away");
        assert!(path.exists());
    }

    #[test]
    fn missing_file_yields_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.toml");
        let r = Registry::load(&path).unwrap();
        assert!(r.is_empty());
    }
}
