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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub tool: PackageId,
    pub key: CacheKey,
    #[serde(default)]
    pub requested: Vec<String>,
}

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

#[derive(Clone, Copy, Debug, Default)]
pub struct NoCatalog;

impl ToolLookup for NoCatalog {
    fn lookup(&self, _: &KeyDigest) -> Option<PackageId> {
        None
    }
}

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

    pub fn put(&self, entry: &Entry) -> std::io::Result<()> {
        tilo_files::native::atomic::write_creating_parent(
            &self.path(&entry.key.digest()),
            &serde_json::to_vec_pretty(entry)?,
        )
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

    pub fn merge(&mut self, other: Self) {
        self.tools.extend(other.tools);
    }

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
