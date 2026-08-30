//! Content hydration: serve file bytes locally, fetching from peers on demand.
//!
//! A `read()` resolves an inode to a blake3 hash via the namespace CRDT, then
//! asks the [`Hydrator`]: if the blob is in the local [`ContentStore`] it is
//! sliced and returned; otherwise it is fetched from a known peer over
//! iroh-blobs (bao-verified against the hash), persisted, and returned.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Result, anyhow};
use bytes::Bytes;
use iroh::endpoint::Endpoint;
use iroh_base::EndpointAddr;
use iroh_blobs::Hash;

use crate::content::Manifest;
use crate::store::ContentStore;
use crate::syncstate::SyncState;

pub struct Hydrator {
    pub endpoint: Endpoint,
    pub store: Arc<ContentStore>,
    /// Peers we may hydrate missing blobs from (populated by the sync layer).
    peers: Arc<RwLock<Vec<EndpointAddr>>>,
    /// Blob hashes with a hydration fetch in flight — inserted for the lifetime
    /// of [`fetch_from`](Hydrator::fetch_from), removed via [`FetchGuard`], so [`sync_state`](Hydrator::sync_state) doesn't race the fetch.
    in_flight: Arc<Mutex<HashSet<Hash>>>,
    pub runtime_handle: tokio::runtime::Handle,
}

/// Clears a hash from the in-flight set on drop; hold it for the duration of a hydration so `sync_state` reports `downloading`.
pub struct FetchGuard {
    set: Arc<Mutex<HashSet<Hash>>>,
    hash: Hash,
}

impl Drop for FetchGuard {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.hash);
    }
}

impl Hydrator {
    pub fn new(endpoint: Endpoint, store: Arc<ContentStore>) -> Self {
        Self {
            endpoint,
            store,
            peers: Arc::new(RwLock::new(Vec::new())),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            runtime_handle: tokio::runtime::Handle::current(),
        }
    }

    /// Whether a fetch of blob `hash` is in flight right now.
    pub fn is_fetching(&self, hash: Hash) -> bool {
        self.in_flight.lock().unwrap().contains(&hash)
    }

    /// Mark `hash` as being fetched; the returned guard clears it on drop.
    /// Idempotent — a concurrent fetch of the same blob shares the mark.
    pub fn mark_fetching(&self, hash: Hash) -> FetchGuard {
        self.in_flight.lock().unwrap().insert(hash);
        FetchGuard {
            set: self.in_flight.clone(),
            hash,
        }
    }

    /// The hydration state of the file whose content is `manifest_hash`:
    /// `downloading` if a fetch is in flight, `projected` if the manifest or a chunk is still missing, `local` only when all are present.
    pub async fn sync_state(&self, manifest_hash: Hash) -> SyncState {
        // A fetch of the manifest blob itself is under way.
        if self.is_fetching(manifest_hash) {
            return SyncState::Downloading;
        }
        // Without the manifest we don't have the chunk list — purely projected, as a freshly-joined peer sees every file until it reads one.
        let Some(mbytes) = self.store.local(manifest_hash).await else {
            return SyncState::Projected;
        };
        let Ok(manifest) = Manifest::decode(&mbytes) else {
            // A manifest we can't parse can't be served locally → projected.
            return SyncState::Projected;
        };
        let mut any_missing = false;
        for c in &manifest.chunks {
            let h = Hash::from(c.hash);
            if self.is_fetching(h) {
                return SyncState::Downloading;
            }
            if !self.store.has(h).await {
                any_missing = true;
            }
        }
        if any_missing {
            SyncState::Projected
        } else {
            SyncState::Local
        }
    }

    /// Register a peer we can hydrate blobs from. Idempotent per endpoint id.
    pub fn add_peer(&self, addr: EndpointAddr) {
        let mut peers = self.peers.write().unwrap();
        if !peers.iter().any(|a| a.id == addr.id) {
            peers.push(addr);
        }
    }

    pub fn known_peers(&self) -> Vec<EndpointAddr> {
        self.peers.read().unwrap().clone()
    }

    pub async fn read_blob(&self, hash: Hash, offset: u64, size: u64) -> Result<Vec<u8>> {
        if let Some(bytes) = self.store.local(hash).await {
            return Ok(slice(&bytes, offset, size));
        }
        let peers = self.known_peers();
        if peers.is_empty() {
            return Err(anyhow!("no peers available to hydrate {hash}"));
        }
        let mut last_err = anyhow!("hydration failed for {hash}");
        for peer in peers {
            match self.fetch_from(hash, peer).await {
                Ok(bytes) => return Ok(slice(&bytes, offset, size)),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    /// Read a byte range of a file given its manifest hash: fetch the manifest, then only the chunks overlapping `[offset, offset+size)`.
    pub async fn read(&self, manifest_hash: Hash, offset: u64, size: u64) -> Result<Vec<u8>> {
        let manifest = self.load_manifest(manifest_hash).await?;
        let end = offset.saturating_add(size);
        let mut out = Vec::new();
        let mut pos: u64 = 0;
        for c in &manifest.chunks {
            let cstart = pos;
            let cend = pos + u64::from(c.len);
            pos = cend;
            if cend <= offset {
                continue; // entirely before the range
            }
            if cstart >= end {
                break; // entirely after the range
            }
            let chunk = self
                .read_blob(Hash::from(c.hash), 0, u64::from(c.len))
                .await?;
            let from = offset.saturating_sub(cstart).min(chunk.len() as u64) as usize;
            let to = (end - cstart).min(chunk.len() as u64) as usize;
            if from < to {
                out.extend_from_slice(&chunk[from..to]);
            }
        }
        Ok(out)
    }

    /// Total byte length of a file from its manifest (fetches only the manifest).
    pub async fn content_len(&self, manifest_hash: Hash) -> Result<u64> {
        Ok(self.load_manifest(manifest_hash).await?.total_len())
    }

    async fn load_manifest(&self, manifest_hash: Hash) -> Result<Manifest> {
        let bytes = self.read_blob(manifest_hash, 0, u64::MAX).await?;
        Manifest::decode(&bytes)
    }

    /// Fetch a whole blob from a specific peer, verify, persist, and return it.
    /// `get_blob` streams bao-verified content, so a corrupt/wrong blob fails the stream rather than returning bad bytes.
    pub async fn fetch_from(&self, hash: Hash, peer: EndpointAddr) -> Result<Bytes> {
        // Publish that this blob is downloading so `sync_state` reports it; the guard clears on completion or failure.
        let _guard = self.mark_fetching(hash);
        // Bounded so an unreachable/stalled peer can't hang a read forever.
        tokio::time::timeout(Duration::from_secs(20), async {
            let conn = self.endpoint.connect(peer, iroh_blobs::ALPN).await?;
            let (bytes, _stats) = iroh_blobs::get::request::get_blob(conn, hash)
                .bytes_and_stats()
                .await?;
            self.store.store_fetched(hash, bytes.clone()).await?;
            Ok::<_, anyhow::Error>(bytes)
        })
        .await
        .map_err(|_| anyhow!("hydration from peer timed out"))?
    }
}

fn slice(bytes: &[u8], offset: u64, size: u64) -> Vec<u8> {
    let start = (offset as usize).min(bytes.len());
    let end = start.saturating_add(size as usize).min(bytes.len());
    bytes[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::endpoint::presets;
    use iroh::protocol::Router;

    #[tokio::test]
    async fn reads_local_content_without_peers() {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hash = store.write_content(b"hello local").await.unwrap().unwrap();

        let hydrator = Hydrator::new(endpoint, store);
        let got = hydrator.read(hash, 0, 11).await.unwrap();
        assert_eq!(&got, b"hello local");

        let mid = hydrator.read(hash, 6, 5).await.unwrap();
        assert_eq!(&mid, b"local");
    }

    #[tokio::test]
    async fn chunked_large_file_ranged_reads() {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        // A multi-chunk buffer (well over MAX_CHUNK).
        let data: Vec<u8> = (0..300_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let mh = store.write_content(&data).await.unwrap().unwrap();
        assert!(store.local(mh).await.is_some(), "manifest stored");

        let hyd = Hydrator::new(endpoint, store);
        assert_eq!(hyd.content_len(mh).await.unwrap(), data.len() as u64);

        assert_eq!(hyd.read(mh, 0, data.len() as u64).await.unwrap(), data);

        // ranges that cross chunk boundaries
        for (off, len) in [
            (0usize, 100usize),
            (8000, 20000),
            (150_000, 1),
            (299_000, 2000),
        ] {
            let end = (off + len).min(data.len());
            let got = hyd.read(mh, off as u64, (end - off) as u64).await.unwrap();
            assert_eq!(got, &data[off..end], "range {off}+{len} mismatch");
        }
    }

    #[tokio::test]
    async fn missing_blob_with_no_peers_errors() {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Hydrator::new(endpoint, store);
        let absent = Hash::from([9u8; 32]);
        assert!(hydrator.read(absent, 0, 16).await.is_err());
    }

    #[tokio::test]
    async fn sync_state_is_local_when_manifest_and_chunks_are_present() {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        // A multi-chunk file so we exercise the per-chunk presence check.
        let data: Vec<u8> = (0..250_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8)
            .collect();
        let mh = store.write_content(&data).await.unwrap().unwrap();
        let hyd = Hydrator::new(endpoint, store);
        assert_eq!(
            hyd.sync_state(mh).await,
            SyncState::Local,
            "fully-ingested file is local"
        );
    }

    #[tokio::test]
    async fn sync_state_is_projected_when_content_is_not_local() {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hyd = Hydrator::new(endpoint, store);
        // A manifest hash the store never ingested: exactly what a freshly-joined peer holds — projected metadata, no content.
        let absent = Hash::from([7u8; 32]);
        assert_eq!(
            hyd.sync_state(absent).await,
            SyncState::Projected,
            "unknown manifest is projected"
        );
    }

    #[tokio::test]
    async fn sync_state_is_projected_when_a_chunk_is_missing() {
        // Manifest present locally but one referenced chunk was never stored: partially projected (the state after a torn/partial fetch).
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let present = store
            .ingest(Bytes::from_static(b"chunk that exists"))
            .await
            .unwrap();
        let missing = Hash::from([9u8; 32]);
        let manifest = crate::content::Manifest {
            chunks: vec![
                crate::content::ChunkRef {
                    hash: *present.as_bytes(),
                    len: 17,
                },
                crate::content::ChunkRef {
                    hash: *missing.as_bytes(),
                    len: 42,
                },
            ],
        };
        let mh = store.ingest(Bytes::from(manifest.encode())).await.unwrap();
        let hyd = Hydrator::new(endpoint, store);
        assert_eq!(
            hyd.sync_state(mh).await,
            SyncState::Projected,
            "a locally-known manifest with a missing chunk is still projected",
        );
    }

    #[tokio::test]
    async fn sync_state_reports_downloading_while_a_fetch_is_in_flight() {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        // Ingest a real file (so absent state would otherwise be Local)...
        let data: Vec<u8> = (0..200_000u32)
            .map(|i| (i.wrapping_mul(40_503) >> 9) as u8)
            .collect();
        let mh = store.write_content(&data).await.unwrap().unwrap();
        let hyd = Hydrator::new(endpoint, store);
        assert_eq!(hyd.sync_state(mh).await, SyncState::Local);

        // Hold the exact in-flight marker `fetch_from` publishes for the manifest blob: `sync_state` must report downloading.
        let guard = hyd.mark_fetching(mh);
        assert!(hyd.is_fetching(mh));
        assert_eq!(
            hyd.sync_state(mh).await,
            SyncState::Downloading,
            "an in-flight fetch of the manifest reads as downloading",
        );
        drop(guard);
        // Once the fetch ends the mark clears and the file is local again.
        assert!(!hyd.is_fetching(mh));
        assert_eq!(
            hyd.sync_state(mh).await,
            SyncState::Local,
            "cleared after the fetch"
        );
    }

    // A blob ingested only on node A is read on node B by fetching it over a real iroh connection (bao-verified), then cached.
    #[tokio::test]
    async fn hydrates_real_bytes_from_a_peer() {
        // --- node A: serve a blob over iroh-blobs ---
        let server_ep = Endpoint::bind(presets::N0).await.unwrap();
        let server_store = ContentStore::memory();
        let payload = Bytes::from_static(b"collaborative filesystem payload");
        let hash = server_store
            .write_content(payload.as_ref())
            .await
            .unwrap()
            .unwrap();
        let _router = Router::builder(server_ep.clone())
            .accept(iroh_blobs::ALPN, server_store.blobs_protocol(None))
            .spawn();
        let server_addr = server_ep.addr();

        // --- node B: empty store, must hydrate from A ---
        let client_ep = Endpoint::bind(presets::N0).await.unwrap();
        let client_store = Arc::new(ContentStore::memory());
        let hydrator = Hydrator::new(client_ep, client_store.clone());

        assert!(
            !client_store.has(hash).await,
            "precondition: B must not have the blob yet"
        );
        hydrator.add_peer(server_addr);

        let got = hydrator.read(hash, 0, payload.len() as u64).await.unwrap();
        assert_eq!(
            got.as_slice(),
            payload.as_ref(),
            "B must read A's real bytes"
        );

        // after hydration the blob is cached locally on B
        assert!(
            client_store.has(hash).await,
            "fetched blob must be cached on B"
        );
    }
}
