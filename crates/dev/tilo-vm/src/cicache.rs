//! Layers extracted on this host from CI images (`swap --from sha:`/`ci:`
//! on a device), kept so a repeat swap of the same build skips the pull.
//!
//! The image itself is deleted after each extraction; what stays is the
//! layer archive and what the image said about itself ([`Pulled`]), keyed
//! by the image's digest for the device's architecture and the component.
//! The digest is looked up in the registry first, so a moved tag is a miss.
//! Only the most recently used [`KEEP`] layers stay.
//!
//! ```text
//! <cache>/tilo-vm/ci-layers/<digest hex>-<component>/layer.tar
//! <cache>/tilo-vm/ci-layers/<digest hex>-<component>/pulled.json
//! ```

use std::path::{Path, PathBuf};

use crate::ci::Pulled;

/// Layers kept, most recently used first: a shell layer is about 110 MB.
pub const KEEP: usize = 6;

const ARCHIVE: &str = "layer.tar";
const PULLED: &str = "pulled.json";
/// The content hash tilo-vmd gave the layer when it was imported.
const HASH: &str = "layer.hash";

pub struct CiCache {
    dir: PathBuf,
    keep: usize,
}

impl CiCache {
    pub fn new(dir: PathBuf, keep: usize) -> Self {
        Self { dir, keep }
    }

    /// `<cache>/tilo-vm/ci-layers`, keeping [`KEEP`] layers.
    pub fn at_home() -> Self {
        Self::new(
            xdg_paths::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("tilo-vm")
                .join("ci-layers"),
            KEEP,
        )
    }

    fn entry(&self, digest: &str, component: &str) -> Option<PathBuf> {
        let hex = digest.strip_prefix("sha256:")?;
        let safe = |text: &str| {
            !text.is_empty()
                && text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+-_".contains(&b))
        };
        (safe(hex) && safe(component)).then(|| self.dir.join(format!("{hex}-{component}")))
    }

    /// The cached layer archive of `component` from the image `digest`, and
    /// what that image said about itself; marks it used.
    pub fn get(&self, digest: &str, component: &str) -> Option<(PathBuf, Pulled)> {
        let entry = self.entry(digest, component)?;
        let pulled: Pulled =
            serde_json::from_slice(&std::fs::read(entry.join(PULLED)).ok()?).ok()?;
        let archive = entry.join(ARCHIVE);
        if !archive.is_file() || pulled.digest.as_deref() != Some(digest) {
            return None;
        }
        touch(&entry);
        Some((archive, pulled))
    }

    /// The content hash a device gave this layer on import, once known: a
    /// device that lists it needs no upload.
    pub fn layer_hash(&self, digest: &str, component: &str) -> Option<String> {
        let entry = self.entry(digest, component)?;
        let hash = std::fs::read_to_string(entry.join(HASH)).ok()?;
        let hash = hash.trim();
        (!hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hash.to_owned())
    }

    /// Note the content hash a device gave the kept layer on import.
    ///
    /// # Errors
    /// I/O failures, or no such kept layer.
    pub fn record_hash(&self, digest: &str, component: &str, hash: &str) -> std::io::Result<()> {
        let entry = self
            .entry(digest, component)
            .filter(|entry| entry.join(ARCHIVE).is_file())
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        std::fs::write(entry.join(HASH), hash)
    }

    /// Keep `archive` as `component` from the image `pulled` names, then
    /// drop the least recently used layers beyond the bound.
    ///
    /// # Errors
    /// I/O failures; the cache is only an optimisation, so callers may
    /// ignore them.
    pub fn put(&self, component: &str, archive: &Path, pulled: &Pulled) -> std::io::Result<()> {
        let Some(entry) = pulled
            .digest
            .as_deref()
            .and_then(|digest| self.entry(digest, component))
        else {
            return Ok(());
        };
        std::fs::create_dir_all(&self.dir)?;
        let staging = tempfile::Builder::new()
            .prefix(".put-")
            .tempdir_in(&self.dir)?;
        std::fs::copy(archive, staging.path().join(ARCHIVE))?;
        std::fs::write(
            staging.path().join(PULLED),
            serde_json::to_vec_pretty(pulled).map_err(std::io::Error::other)?,
        )?;
        let _ = std::fs::remove_dir_all(&entry);
        std::fs::rename(staging.keep(), &entry)?;
        touch(&entry);
        self.evict();
        Ok(())
    }

    /// Remove all but the `keep` most recently used layers.
    fn evict(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut layers: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        layers.sort_by_key(|layer| std::cmp::Reverse(layer.0));
        for (_, path) in layers.into_iter().skip(self.keep) {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// Mark `entry` used now: its modification time orders eviction.
fn touch(entry: &Path) {
    if let Ok(dir) = std::fs::File::open(entry) {
        let _ = dir.set_modified(std::time::SystemTime::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pulled(digest: &str) -> Pulled {
        Pulled {
            digest: Some(digest.into()),
            revision: Some("fff3babf03eb".into()),
        }
    }

    /// A repeat swap finds the layer by digest and component; a moved tag
    /// (another digest) misses; the cache keeps only the newest layers.
    #[test]
    fn ci_layers_are_found_by_digest_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CiCache::new(dir.path().join("ci-layers"), 2);
        let archive = dir.path().join("layer.tar");
        std::fs::write(&archive, b"layer bytes").unwrap();

        // Phase `a_put_layer_is_found_again`.
        {
            assert!(cache.get("sha256:aa", "shell").is_none());
            cache.put("shell", &archive, &pulled("sha256:aa")).unwrap();
            let (found, info) = cache.get("sha256:aa", "shell").unwrap();
            assert_eq!(std::fs::read(found).unwrap(), b"layer bytes");
            assert_eq!(info, pulled("sha256:aa"));
            assert!(cache.get("sha256:aa", "launcher").is_none());
            assert!(
                cache.get("sha256:bb", "shell").is_none(),
                "a moved tag misses"
            );
            assert!(cache.get("../x", "shell").is_none());
        }
        // Phase `the_imported_hash_is_remembered_with_the_layer`.
        {
            assert_eq!(cache.layer_hash("sha256:aa", "shell"), None);
            cache
                .record_hash("sha256:aa", "shell", "999eb36b8b34")
                .unwrap();
            assert_eq!(
                cache.layer_hash("sha256:aa", "shell").as_deref(),
                Some("999eb36b8b34")
            );
            assert!(cache.record_hash("sha256:zz", "shell", "ab").is_err());
            // A new put of the same build starts without one.
            cache.put("shell", &archive, &pulled("sha256:aa")).unwrap();
            assert_eq!(cache.layer_hash("sha256:aa", "shell"), None);
        }
        // Phase `only_the_most_recently_used_stay`.
        {
            let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
            cache.put("shell", &archive, &pulled("sha256:bb")).unwrap();
            for name in ["aa-shell", "bb-shell"] {
                std::fs::File::open(cache.dir.join(name))
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
            }
            // Using aa makes bb the least recently used.
            assert!(cache.get("sha256:aa", "shell").is_some());
            cache.put("shell", &archive, &pulled("sha256:cc")).unwrap();
            assert!(cache.get("sha256:bb", "shell").is_none(), "evicted");
            assert!(cache.get("sha256:aa", "shell").is_some());
            assert!(cache.get("sha256:cc", "shell").is_some());
            let left = std::fs::read_dir(&cache.dir).unwrap().count();
            assert_eq!(left, 2);
        }
        // Phase `a_pull_without_a_digest_is_not_kept`.
        {
            cache.put("shell", &archive, &Pulled::default()).unwrap();
            assert_eq!(std::fs::read_dir(&cache.dir).unwrap().count(), 2);
        }
    }
}
