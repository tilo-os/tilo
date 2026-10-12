//! File versions in and out of the content store.
//!
//! A version enters as bytes (a FUSE write, a merge) or as a file (a Tier 1
//! capture snapshot, which is immutable). Either way it is chunked in the
//! current format and stored under its content id: small content as loose
//! blobs, larger content as one file in the area, cloned from the snapshot
//! where the filesystem shares extents. The author never holds a large file
//! in memory: a capture hands over the manifest it computed over the mapped
//! snapshot, and a file without one is chunked as a stream.

use std::fs::File;
use std::io::BufReader;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use bytes::Bytes;
use iroh_blobs::Hash;
use iroh_blobs::api::blobs::BlobStatus;

use super::{ContentStore, Retention};
use crate::content::{self, ChunkTier, Manifest};

impl ContentStore {
    /// Chunks `data` in the current format, stores it and returns its
    /// content id (`None` when empty). Unchanged chunks of earlier versions
    /// are stored once.
    ///
    /// # Errors
    ///
    /// Returns an error if the content cannot be stored.
    pub async fn write_content(&self, data: &[u8]) -> Result<Option<Hash>> {
        self.write_content_after(data, None).await
    }

    /// [`Self::write_content`] for the next version of a file whose current
    /// content is `previous`: the new version keeps its chunk tier, so it
    /// shares chunks with the previous one.
    ///
    /// # Errors
    ///
    /// Returns an error if the content cannot be stored.
    pub async fn write_content_after(
        &self,
        data: &[u8],
        previous: Option<Hash>,
    ) -> Result<Option<Hash>> {
        if data.is_empty() {
            return Ok(None);
        }
        let tier = self.tier_of(previous).await;
        let (manifest, _) = content::build(data, tier);
        if content::is_file_backed(&manifest) {
            let files = Arc::clone(&self.files);
            let key = manifest.id();
            if !files.entry(&key).is_some_and(|e| e.complete()) {
                files.adopt_bytes(
                    &key,
                    data,
                    Retention::LocalOnly,
                    &content::chunk_locations(&manifest),
                )?;
            }
        } else {
            let chunks = manifest
                .chunks()
                .iter()
                .enumerate()
                .filter_map(|(index, _)| {
                    let range = manifest.chunk_range(index)?;
                    let bytes = data.get(
                        usize::try_from(range.start).ok()?..usize::try_from(range.end).ok()?,
                    )?;
                    Some(Bytes::copy_from_slice(bytes))
                })
                .collect();
            self.persist_owned_batch(chunks).await?;
        }
        self.persist_manifest(&manifest).await.map(Some)
    }

    /// Stores the file at `path` as a version and returns its content id
    /// and length (`None` for an empty file). `path` must not change while
    /// this runs (a capture snapshot). `known` is the manifest a capture
    /// already computed for exactly these bytes; without one the file is
    /// chunked as a stream. Large content is cloned into the area, sharing
    /// extents with `path` where the filesystem can.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or the content stored.
    pub async fn publish_file(
        &self,
        path: &Path,
        known: Option<Arc<Manifest>>,
        previous: Option<Hash>,
    ) -> Result<Option<(Hash, u64)>> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Ok(None);
        }
        let tier = self.tier_of(previous).await;
        let manifest = match known {
            Some(m) if m.total_len() == len => Arc::unwrap_or_clone(m),
            _ => {
                let reader = File::open(path)?;
                tokio::task::spawn_blocking(move || {
                    content::build_reader(BufReader::with_capacity(1 << 20, reader), len, tier)
                })
                .await??
                .0
            }
        };
        if content::is_file_backed(&manifest) {
            let files = Arc::clone(&self.files);
            let key = manifest.id();
            let chunks = content::chunk_locations(&manifest);
            tokio::task::spawn_blocking(move || {
                if files.entry(&key).is_some_and(|e| e.complete()) {
                    return files.set_retention(&key, Retention::LocalOnly);
                }
                files.adopt_clone(&key, &file, len, Retention::LocalOnly, &chunks)
            })
            .await??;
            return Ok(Some((self.persist_manifest(&manifest).await?, len)));
        }
        let data = std::fs::read(path)?;
        anyhow::ensure!(
            data.len() as u64 == len,
            "{} changed while it was published",
            path.display()
        );
        Ok(self
            .write_content_after(&data, previous)
            .await?
            .map(|hash| (hash, len)))
    }

    async fn persist_manifest(&self, manifest: &Manifest) -> Result<Hash> {
        let hash = self.ingest(Bytes::from(manifest.encode())).await?;
        anyhow::ensure!(
            hash.as_bytes() == &manifest.id(),
            "stored manifest has another hash"
        );
        Ok(hash)
    }

    /// The decoded manifest of `content`, if it is stored locally.
    pub async fn manifest(&self, content: Hash) -> Option<Manifest> {
        let bytes = self.local(content).await?;
        Manifest::decode(&bytes).ok()
    }

    /// The chunk tier of `previous`'s manifest, for tier stickiness.
    async fn tier_of(&self, previous: Option<Hash>) -> Option<ChunkTier> {
        Some(self.manifest(previous?).await?.tier())
    }

    /// A file-backed chunk's bytes, checked against its hash.
    pub(super) async fn local_chunk(&self, hash: Hash) -> Option<Bytes> {
        let files = Arc::clone(&self.files);
        tokio::task::spawn_blocking(move || {
            let location = files.find_chunk(hash.as_bytes())?;
            let bytes = files
                .read_at(&location.key, location.offset, u64::from(location.len))
                .ok()?;
            (blake3::hash(&bytes).as_bytes() == hash.as_bytes()).then(|| Bytes::from(bytes))
        })
        .await
        .ok()
        .flatten()
    }

    /// Bytes `range` of `content` (described by `manifest`) if every one is
    /// local; never fetches.
    pub async fn read_local(
        &self,
        content: Hash,
        manifest: &Manifest,
        range: Range<u64>,
    ) -> Option<Vec<u8>> {
        let end = range.end.min(manifest.total_len());
        let start = range.start.min(end);
        if content::is_file_backed(manifest) {
            let files = Arc::clone(&self.files);
            let key = *content.as_bytes();
            return tokio::task::spawn_blocking(move || {
                files.read_at(&key, start, end.saturating_sub(start)).ok()
            })
            .await
            .ok()
            .flatten();
        }
        let mut out = Vec::with_capacity(usize::try_from(end.saturating_sub(start)).unwrap_or(0));
        for (_, chunk_start, chunk) in manifest.overlapping(start..end) {
            let bytes = self.local(Hash::from(chunk.hash)).await?;
            copy_part(&mut out, &bytes, chunk_start, start, end);
        }
        Some(out)
    }

    /// Whether every byte of `content` is local.
    pub async fn is_complete(&self, content: Hash, manifest: &Manifest) -> bool {
        if content::is_file_backed(manifest) {
            return self
                .files
                .entry(content.as_bytes())
                .is_some_and(|e| e.complete() && e.size == manifest.total_len());
        }
        for chunk in manifest.chunks() {
            if !self.has(Hash::from(chunk.hash)).await {
                return false;
            }
        }
        true
    }

    /// The status of blob `hash` in either backing: a chunk resident in the
    /// area counts as complete.
    ///
    /// # Errors
    ///
    /// Returns an error if the loose store cannot answer.
    pub async fn blob_status(&self, hash: Hash) -> Result<BlobStatus> {
        if let Some(location) = self.files.find_chunk(hash.as_bytes()) {
            return Ok(BlobStatus::Complete {
                size: u64::from(location.len),
            });
        }
        Ok(self.store().blobs().status(hash).await?)
    }

    /// Keeps `content` offline (or lets it be evicted again). Only
    /// file-backed content has a retention; loose blobs are kept by their
    /// tags.
    ///
    /// # Errors
    ///
    /// Returns an error when the change cannot be recorded.
    pub fn set_pinned(&self, content: Hash, pinned: bool) -> Result<()> {
        let key = content.as_bytes();
        let Some(entry) = self.files.entry(key) else {
            return Ok(());
        };
        let retention = match (pinned, entry.retention) {
            (true, _) => Retention::Pinned,
            (false, Retention::Pinned) => Retention::Cached,
            (false, other) => other,
        };
        self.files.set_retention(key, retention)?;
        Ok(())
    }
}

/// Appends the part of a chunk starting at file offset `chunk_start` that
/// falls in `[start, end)`.
pub(crate) fn copy_part(out: &mut Vec<u8>, chunk: &[u8], chunk_start: u64, start: u64, end: u64) {
    let len = chunk.len() as u64;
    let from = usize::try_from(start.saturating_sub(chunk_start).min(len)).unwrap_or(chunk.len());
    let to = usize::try_from(end.saturating_sub(chunk_start).min(len)).unwrap_or(chunk.len());
    if from < to
        && let Some(part) = chunk.get(from..to)
    {
        out.extend_from_slice(part);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_bytes::pseudo;

    #[tokio::test]
    async fn small_content_is_loose_and_large_content_file_backed() {
        let store = ContentStore::memory();
        let small = pseudo(200_000, 1);
        let id = store.write_content(&small).await.unwrap().unwrap();
        let m = store.manifest(id).await.unwrap();
        assert!(!content::is_file_backed(&m));
        assert!(store.files().entry(id.as_bytes()).is_none());
        assert_eq!(store.read_local(id, &m, 0..200_000).await.unwrap(), small);

        let large = pseudo(3 * 1024 * 1024, 2);
        let id = store.write_content(&large).await.unwrap().unwrap();
        let m = store.manifest(id).await.unwrap();
        assert!(content::is_file_backed(&m));
        assert!(store.is_complete(id, &m).await);
        let entry = store.files().entry(id.as_bytes()).unwrap();
        assert_eq!(entry.retention, Retention::LocalOnly);
        assert_eq!(
            store.read_local(id, &m, 1000..2_500_000).await.unwrap(),
            &large[1000..2_500_000]
        );
        // Chunks are found where they live, checked against their hash.
        let first = m.chunks()[0];
        let chunk = store.local(Hash::from(first.hash)).await.unwrap();
        assert_eq!(chunk.len(), first.len as usize);
        assert!(store.has(Hash::from(first.hash)).await);
    }

    #[tokio::test]
    async fn a_published_file_keeps_its_tier_and_shares_chunks_with_the_last_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::memory();
        let path = dir.path().join("f");
        let mut bytes = pseudo(5 * 1024 * 1024, 3);
        std::fs::write(&path, &bytes).unwrap();
        let (first, len) = store
            .publish_file(&path, None, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(len, bytes.len() as u64);
        bytes[2_000_000] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();
        let (second, _) = store
            .publish_file(&path, None, Some(first))
            .await
            .unwrap()
            .unwrap();
        let (a, b) = (
            store.manifest(first).await.unwrap(),
            store.manifest(second).await.unwrap(),
        );
        assert_eq!(a.tier(), b.tier());
        let shared = b.chunks().iter().filter(|c| a.chunks().contains(c)).count();
        assert!(
            shared + 3 >= b.chunks().len(),
            "{shared} of {}",
            b.chunks().len()
        );
        assert_eq!(
            store.read_local(second, &b, 0..len).await.unwrap(),
            bytes,
            "the area holds the published bytes"
        );
    }
}
