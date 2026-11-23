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
use std::io;
use std::io::BufReader;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use bytes::Bytes;
use iroh_blobs::Hash;

use super::{ContentStore, Retention};
use crate::content::{self, ChunkTier, Manifest};

/// The largest file a mount's staged bytes become an entry for by a link: above this the entry is
/// cloned or copied from the stage as the capture paths do, and one photo-sized file is exactly
/// what a copy of a photo library closes, so the link is what a copy pays.
const ADOPT_MAX: u64 = 1024 * 1024;

/// What became of a staged file handed to the store.
enum Staged {
    /// Its bytes are the entry now: the content id, and the encoded manifest to cache.
    Linked { hash: Hash, encoded: Bytes },
    /// The store already holds these bytes; the manifest names them.
    AlreadyStored(Manifest),
    /// The stage and the store are on different filesystems, so it is written in as content.
    Elsewhere,
}

/// Links the staged bytes at `path` in as the entry for their chunks: the one place a stage
/// becomes an entry, shared by the mount's own thread ([`ContentStore::adopt_staged_inline`]) and
/// the async paths.
fn link_staged(
    files: &crate::store::FileArea,
    path: &Path,
    len: u64,
    data: &[u8],
    tier: Option<ChunkTier>,
) -> Result<Staged> {
    let (manifest, _) = content::build(data, tier);
    let key = manifest.id();
    if files.entry(&key).is_some_and(|e| e.complete()) {
        return Ok(Staged::AlreadyStored(manifest));
    }
    let encoded = manifest.encode();
    let chunks = content::chunk_locations(&manifest);
    match files.adopt_version_file(&key, path, len, Retention::LocalOnly, &chunks, &encoded) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            return Ok(Staged::Elsewhere);
        }
        Err(error) => return Err(error.into()),
    }
    Ok(Staged::Linked {
        hash: Hash::from_bytes(key),
        encoded: Bytes::from(encoded),
    })
}

/// The largest file that is published by reading it into memory rather than chunking it as a
/// stream: a version this size costs less held once than read from the disk twice.
const IN_MEMORY_PUBLISH_MAX: u64 = 256 * 1024;

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
            // The chunks and the manifest that names them go in as one batch: a batch is one
            // database commit and one sync, and a checkout writes one version per file.
            let mut batch = manifest
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
                .collect::<Vec<_>>();
            let encoded = manifest.encode();
            batch.push(Bytes::from(encoded));
            let stored = self.persist_owned_batch(batch).await?;
            let hash = stored
                .last()
                .copied()
                .ok_or_else(|| anyhow::anyhow!("the stored manifest went missing"))?;
            anyhow::ensure!(
                hash.as_bytes() == &manifest.id(),
                "stored manifest has another hash"
            );
            return Ok(Some(hash));
        }
        self.persist_manifest(&manifest).await.map(Some)
    }

    /// A staged file becomes the entry itself on the calling thread: its bytes are read for their
    /// chunks and linked in as the one write the mount already made, so a closed photo costs no
    /// thread hand-off beside it. A photo the library already holds is named here too. `None` when
    /// the stage is on another filesystem or the bytes need the async paths to be named;
    /// [`Self::publish_file`] then does that.
    ///
    /// # Errors
    ///
    /// The file could not be read or linked into the store.
    #[cfg(feature = "filesystem")]
    pub(crate) fn adopt_staged_inline(
        &self,
        path: &Path,
    ) -> Result<Option<crate::store::Published>> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let len = file.metadata()?.len();
        if len == 0 || len > ADOPT_MAX {
            return Ok(None);
        }
        let data = std::fs::read(path)?;
        anyhow::ensure!(
            data.len() as u64 == len,
            "{} changed while it was published",
            path.display()
        );
        // A version with no previous one keeps the default chunk tier, so nothing here waits on
        // a manifest.
        match link_staged(&self.files, path, len, &data, None)? {
            Staged::Linked { hash, encoded } => {
                self.cache_insert(hash, encoded);
                Ok(Some((Some((hash, len)), true)))
            }
            Staged::AlreadyStored(manifest) => {
                // A library holds the same photo more than once: when the manifest naming the
                // entry's chunks is stored too, the close is done, with nothing to name twice.
                let hash = Hash::from_bytes(manifest.id());
                if self
                    .files
                    .entry(hash.as_bytes())
                    .is_some_and(|entry| entry.complete())
                {
                    self.cache_insert(hash, Bytes::from(manifest.encode()));
                    Ok(Some((Some((hash, len)), true)))
                } else {
                    Ok(None)
                }
            }
            Staged::Elsewhere => Ok(None),
        }
    }

    /// Stores the file at `path` as a version and returns its content id
    /// and length (`None` for an empty file), and whether the file itself became
    /// the stored entry. `path` must not change while
    /// this runs (a capture snapshot). `known` is the manifest a capture
    /// already computed for exactly these bytes; without one the file is
    /// chunked as a stream. Large content is cloned into the area, sharing
    /// extents with `path` where the filesystem can.
    ///
    /// `adopt` lets a file a mount staged become the stored entry itself, instead
    /// of a second write, for sizes up to [`ADOPT_MAX`]. The caller must not
    /// write it again: its bytes are the entry's.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or the content stored.
    pub async fn publish_file(
        &self,
        path: &Path,
        known: Option<Arc<Manifest>>,
        previous: Option<Hash>,
        adopt: bool,
    ) -> Result<(Option<(Hash, u64)>, bool)> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Ok((None, false));
        }
        if adopt && known.is_none() && len <= ADOPT_MAX {
            let data = std::fs::read(path)?;
            anyhow::ensure!(
                data.len() as u64 == len,
                "{} changed while it was published",
                path.display()
            );
            // The bytes are read once for their chunks and linked into the store as they stand:
            // the write the mount already made is the write the entry holds.
            let tier = self.tier_of(previous).await;
            let files = Arc::clone(&self.files);
            let staged = path.to_path_buf();
            let (outcome, data) = tokio::task::spawn_blocking(move || {
                let outcome = link_staged(&files, &staged, len, &data, tier);
                (outcome, data)
            })
            .await?;
            match outcome? {
                Staged::Linked { hash, encoded } => {
                    self.cache_insert(hash, encoded);
                    return Ok((Some((hash, len)), true));
                }
                Staged::AlreadyStored(manifest) => {
                    // The bytes are already stored: only the naming is left, and a manifest some
                    // other path stored beside its entry may be missing after a crash in its window.
                    let hash = self.persist_manifest(&manifest).await?;
                    self.cache_insert(hash, Bytes::from(manifest.encode()));
                    return Ok((Some((hash, len)), true));
                }
                Staged::Elsewhere => {}
            }
            let hash = self.write_content_after(&data, previous).await?;
            return Ok((hash.map(|hash| (hash, len)), false));
        }
        // A file this size's bytes cost less to hold in memory once than to chunk from the disk
        // and read again: a mount staging writes on disk hands every closed file through here.
        if known.is_none() && len <= IN_MEMORY_PUBLISH_MAX {
            let data = std::fs::read(path)?;
            anyhow::ensure!(
                data.len() as u64 == len,
                "{} changed while it was published",
                path.display()
            );
            let hash = self.write_content_after(&data, previous).await?;
            return Ok((hash.map(|hash| (hash, len)), false));
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
            let hash = self.persist_manifest(&manifest).await?;
            return Ok((Some((hash, len)), false));
        }
        let data = std::fs::read(path)?;
        anyhow::ensure!(
            data.len() as u64 == len,
            "{} changed while it was published",
            path.display()
        );
        let hash = self.write_content_after(&data, previous).await?;
        Ok((hash.map(|hash| (hash, len)), false))
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
        if manifest.sealed().is_some() {
            return None;
        }
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

    /// Keeps `content` offline (or lets it be evicted again).
    ///
    /// # Errors
    ///
    /// Returns an error when the change cannot be recorded.
    pub fn set_pinned(&self, content: Hash, pinned: bool) -> Result<()> {
        self.files.keep_offline(content.as_bytes(), pinned)?;
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
        let first = m.chunks()[0];
        let chunk = store.local(Hash::from(first.hash)).await.unwrap();
        assert_eq!(chunk.len(), first.len as usize);
        assert!(store.has(Hash::from(first.hash)).await);
    }

    /// A small file a mount staged is stored as one entry that is the staged file itself: its
    /// chunks are ranges of it, and dropping the stage name leaves the entry whole.
    #[tokio::test]
    async fn a_staged_small_file_becomes_the_entry_itself() {
        use std::os::unix::fs::MetadataExt as _;

        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::memory();
        let path = dir.path().join("photo");
        let bytes = pseudo(256 * 1024, 5);
        std::fs::write(&path, &bytes).unwrap();
        let staged = std::fs::metadata(&path).unwrap();
        let (stored, adopted) = store.publish_file(&path, None, None, true).await.unwrap();
        assert!(adopted);
        let (hash, len) = stored.unwrap();
        assert_eq!(len, bytes.len() as u64);
        assert_eq!(
            std::fs::metadata(store.files().path_of(hash.as_bytes()))
                .unwrap()
                .ino(),
            staged.ino(),
            "the entry is the staged file, moved in by a link rather than written again"
        );
        let manifest = store.manifest(hash).await.unwrap();
        assert!(!content::is_file_backed(&manifest));
        assert_eq!(
            store.read_local(hash, &manifest, 0..len).await.unwrap(),
            bytes
        );
        for chunk in manifest.chunks() {
            assert!(
                store.has(Hash::from(chunk.hash)).await,
                "every chunk is served from the entry's ranges"
            );
        }
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            store.read_local(hash, &manifest, 0..len).await.unwrap(),
            bytes,
            "the stage name's removal leaves the entry whole"
        );
    }

    /// Pinning and unpinning a file only this device holds leaves it
    /// unevictable: a pin may only hold back what the budget could take.
    #[tokio::test]
    async fn a_pin_never_makes_content_only_this_device_holds_evictable() {
        let store = ContentStore::memory();
        let authored = store
            .write_content(&pseudo(3 * 1024 * 1024, 4))
            .await
            .unwrap()
            .unwrap();
        let retention = || store.files().entry(authored.as_bytes()).unwrap().retention;
        store.set_pinned(authored, true).unwrap();
        assert_eq!(retention(), Retention::LocalOnly);
        store.set_pinned(authored, false).unwrap();
        assert_eq!(retention(), Retention::LocalOnly);
        store.files().set_budget(0);
        store.files().evict_to_budget(&[]);
        assert!(store.files().entry(authored.as_bytes()).is_some());
    }

    #[tokio::test]
    async fn a_published_file_keeps_its_tier_and_shares_chunks_with_the_last_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::memory();
        let path = dir.path().join("f");
        let mut bytes = pseudo(5 * 1024 * 1024, 3);
        std::fs::write(&path, &bytes).unwrap();
        let (stored, adopted) = store.publish_file(&path, None, None, false).await.unwrap();
        assert!(!adopted);
        let (first, len) = stored.unwrap();
        assert_eq!(len, bytes.len() as u64);
        bytes[2_000_000] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();
        let (second, _) = store
            .publish_file(&path, None, Some(first), false)
            .await
            .unwrap()
            .0
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
