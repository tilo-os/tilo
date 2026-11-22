//! Batched content fetches from the workspace's content source.
//!
//! A want list (manifests, chunks, whole small files) is asked of the source with up to
//! [`WINDOW_PER_PEER`] asks in flight, so a thousand small files cost a handful of round trips'
//! worth of waiting rather than one each. Everything received is hash-checked before it is kept.

use std::collections::HashSet;

use super::{Hash, Hydrator};

/// Bytes a prefetch asks for before it reports progress, by the callers' size hints.
pub const BATCH_BYTES: u64 = 8 * 1024 * 1024;
/// Windows of [`BATCH_BYTES`] a prefetch keeps asked at once.
pub const WINDOW_PER_PEER: usize = 8;

/// One blob to fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Want {
    pub hash: Hash,
    pub size_hint: u64,
    /// The manifest this blob is a chunk of, when it is one.
    pub root: Option<Hash>,
}

impl Want {
    /// A blob that is itself readable: a manifest, a thumbnail, a small
    /// file's only blob.
    #[must_use]
    pub const fn root(hash: Hash, size_hint: u64) -> Self {
        Self {
            hash,
            size_hint,
            root: None,
        }
    }

    #[must_use]
    pub const fn chunk(hash: Hash, size_hint: u64, root: Hash) -> Self {
        Self {
            hash,
            size_hint,
            root: Some(root),
        }
    }
}

/// What a [`Hydrator::fetch_many`] call achieved.
#[derive(Debug, Default)]
pub struct FetchReport {
    pub fetched: usize,
    pub bytes: u64,
    pub missing: Vec<Hash>,
}

impl Hydrator {
    /// Fetches every blob in `wants` that is not local yet from the content source. Best
    /// effort: what the source does not hand over is reported, not an error.
    pub async fn fetch_many(&self, wants: Vec<Want>) -> FetchReport {
        let mut seen = HashSet::with_capacity(wants.len());
        let mut todo = Vec::with_capacity(wants.len());
        let mut bytes = 0_u64;
        for want in wants {
            if seen.insert(want.hash) && !self.store.has(want.hash).await {
                bytes = bytes.saturating_add(want.size_hint);
                todo.push(want.hash);
            }
        }
        let asked = todo.len();
        let missing = self.fetch_from_source(todo).await;
        FetchReport {
            fetched: asked.saturating_sub(missing.len()),
            bytes,
            missing,
        }
    }

    /// Makes the complete content of every file in `files` local: missing
    /// manifests first, in one batched request set, then every missing
    /// chunk of each file up to `max_file_bytes`. Files whose manifest no
    /// peer serves are skipped. Returns what was fetched.
    ///
    /// This is how many small files arrive at link speed: a thousand files
    /// cost a handful of requests instead of two round trips each.
    pub async fn prefetch_files(&self, files: &[(Hash, u64)], max_file_bytes: u64) -> FetchReport {
        let manifests: Vec<Want> = files
            .iter()
            .filter(|(_, size)| *size <= max_file_bytes)
            .map(|(hash, size)| Want::root(*hash, (size / 256).max(64)))
            .collect();
        let mut report = self.fetch_many(manifests).await;
        let mut chunks = Vec::new();
        for (hash, size) in files {
            if *size > max_file_bytes {
                continue;
            }
            let Some(index) = self.local_manifest_index(*hash).await else {
                continue;
            };
            for chunk in index.chunks() {
                chunks.push(Want::chunk(
                    Hash::from(chunk.hash),
                    index.stored_len(chunk),
                    *hash,
                ));
            }
        }
        let content = self.fetch_many(chunks).await;
        report.fetched = report.fetched.saturating_add(content.fetched);
        report.bytes = report.bytes.saturating_add(content.bytes);
        report.missing.extend(content.missing);
        report
    }
}
