//! Tool lookups by cache key: the builder's local index and the published
//! catalog.
//!
//! Both map a [`KeyDigest`] to the tool image built for it. The local index
//! is one small JSON file per key under the builder's state directory, so
//! concurrent builders never rewrite each other's entries. The catalog is a
//! single JSON document CI publishes for every env-runtime it builds tools
//! for ([`Catalog`]); a device reads it the same way whether it was fetched
//! or shipped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_app_package::PackageId;

use crate::key::{CacheKey, KeyDigest};

/// One key's tool image.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub tool: PackageId,
    pub key: CacheKey,
    /// What was asked for when it was first built (`["pandoc"]`).
    #[serde(default)]
    pub requested: Vec<String>,
}

/// Looks a key up somewhere: the local index, a catalog, a chain of both.
pub trait ToolLookup {
    fn lookup(&self, digest: &KeyDigest) -> Option<PackageId>;
}

impl<T: ToolLookup + ?Sized> ToolLookup for &T {
    fn lookup(&self, digest: &KeyDigest) -> Option<PackageId> {
        (**self).lookup(digest)
    }
}

impl<T: ToolLookup> ToolLookup for Option<T> {
    fn lookup(&self, digest: &KeyDigest) -> Option<PackageId> {
        self.as_ref().and_then(|inner| inner.lookup(digest))
    }
}

/// No catalog.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCatalog;

impl ToolLookup for NoCatalog {
    fn lookup(&self, _: &KeyDigest) -> Option<PackageId> {
        None
    }
}

/// `<root>/<digest-hex>.json`, one [`Entry`] each.
#[derive(Clone, Debug)]
pub struct LocalIndex {
    root: PathBuf,
}

impl LocalIndex {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, digest: &KeyDigest) -> PathBuf {
        self.root.join(format!("{}.json", digest.to_hex()))
    }

    pub fn get(&self, digest: &KeyDigest) -> Option<Entry> {
        let bytes = std::fs::read(self.path(digest)).ok()?;
        let entry: Entry = serde_json::from_slice(&bytes).ok()?;
        // A file whose content doesn't hash to its name is not this key's.
        (entry.key.digest() == *digest).then_some(entry)
    }

    /// Record `entry` under its key, atomically.
    ///
    /// # Errors
    ///
    /// Creating the directory or writing/renaming the file.
    pub fn put(&self, entry: &Entry) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.path(&entry.key.digest());
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec_pretty(entry)?)?;
        std::fs::rename(&tmp, &path)
    }

    /// Forget a key (its tool image was collected).
    ///
    /// # Errors
    ///
    /// Removing the file, other than it already being gone.
    pub fn forget(&self, digest: &KeyDigest) -> std::io::Result<()> {
        match std::fs::remove_file(self.path(digest)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

impl ToolLookup for LocalIndex {
    fn lookup(&self, digest: &KeyDigest) -> Option<PackageId> {
        self.get(digest).map(|e| e.tool)
    }
}

/// The published tool catalog: every tool image CI built, by key.
///
/// ```json
/// { "tools": { "<digest-hex>": { "tool": "…", "key": { … }, "requested": ["git"] } } }
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub tools: BTreeMap<String, Entry>,
}

impl Catalog {
    /// Read a catalog document. Entries whose key doesn't hash to their name
    /// are dropped.
    ///
    /// # Errors
    ///
    /// Reading or parsing the file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        let mut catalog: Self = serde_json::from_slice(&bytes)?;
        catalog
            .tools
            .retain(|hex, entry| entry.key.digest().to_hex() == *hex);
        Ok(catalog)
    }

    pub fn insert(&mut self, entry: Entry) {
        self.tools.insert(entry.key.digest().to_hex(), entry);
    }

    /// Merge `other` in; its entries win.
    pub fn merge(&mut self, other: Self) {
        self.tools.extend(other.tools);
    }

    /// Write the document, sorted by digest.
    ///
    /// # Errors
    ///
    /// Serialising or writing.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

impl ToolLookup for Catalog {
    fn lookup(&self, digest: &KeyDigest) -> Option<PackageId> {
        self.tools.get(&digest.to_hex()).map(|e| e.tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(runtime: u8, nevra: &str, tool: u8) -> Entry {
        Entry {
            tool: PackageId::from_bytes([tool; 32]),
            key: CacheKey::new(PackageId::from_bytes([runtime; 32]), [nevra]),
            requested: vec!["x".into()],
        }
    }

    #[test]
    fn local_index_round_trips_and_rejects_misfiled_entries() {
        let dir = tempfile::tempdir().unwrap();
        let index = LocalIndex::at(dir.path());
        let e = entry(1, "jq-1-1.x86_64", 9);
        let digest = e.key.digest();
        assert_eq!(index.lookup(&digest), None);
        index.put(&e).unwrap();
        assert_eq!(index.lookup(&digest), Some(e.tool));

        // Copy the entry to another key's name: not trusted.
        let other = entry(2, "jq-1-1.x86_64", 9).key.digest();
        std::fs::copy(
            dir.path().join(format!("{digest}.json")),
            dir.path().join(format!("{other}.json")),
        )
        .unwrap();
        assert_eq!(index.lookup(&other), None);

        index.forget(&digest).unwrap();
        index.forget(&digest).unwrap();
        assert_eq!(index.lookup(&digest), None);
    }

    #[test]
    fn catalog_round_trips_and_drops_misnamed_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut catalog = Catalog::default();
        let good = entry(1, "git-2-1.x86_64", 3);
        catalog.insert(good.clone());
        catalog
            .tools
            .insert("00".repeat(32), entry(1, "evil-1-1.x86_64", 4));
        let path = dir.path().join("catalog.json");
        catalog.save(&path).unwrap();
        let loaded = Catalog::load(&path).unwrap();
        assert_eq!(loaded.tools.len(), 1);
        assert_eq!(loaded.lookup(&good.key.digest()), Some(good.tool));
    }
}
