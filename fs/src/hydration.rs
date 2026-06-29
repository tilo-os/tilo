//! Content hydration: serve file bytes locally, fetching from peers on demand.
//!
//! A `read()` resolves an inode to a blake3 hash via the namespace CRDT, then
//! asks the [`Hydrator`]: if the blob is in the local [`ContentStore`] it is
//! sliced and returned; otherwise it is fetched from a known peer over
//! iroh-blobs (bao-verified against the hash), persisted, and returned.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use bytes::Bytes;
use iroh::endpoint::Endpoint;
use iroh_base::EndpointAddr;
use iroh_blobs::Hash;

use crate::content::Manifest;
use crate::store::ContentStore;

/// Fetches content for the FUSE read path, hydrating from peers as needed.
pub struct Hydrator {
    pub endpoint: Endpoint,
    pub store: Arc<ContentStore>,
    /// Peers we may hydrate missing blobs from (populated by the sync layer).
    peers: Arc<RwLock<Vec<EndpointAddr>>>,
    pub runtime_handle: tokio::runtime::Handle,
}

impl Hydrator {
    pub fn new(endpoint: Endpoint, store: Arc<ContentStore>) -> Self {
        Self {
            endpoint,
            store,
            peers: Arc::new(RwLock::new(Vec::new())),
            runtime_handle: tokio::runtime::Handle::current(),
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

    /// Read a byte range of a raw blob `hash`, hydrating from a peer if needed.
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

    /// Read a byte range of a file given its **manifest** hash: fetch the
    /// manifest, then only the chunks overlapping `[offset, offset+size)`.
    pub async fn read(&self, manifest_hash: Hash, offset: u64, size: u64) -> Result<Vec<u8>> {
        let manifest = self.load_manifest(manifest_hash).await?;
        let end = offset.saturating_add(size);
        let mut out = Vec::new();
        let mut pos: u64 = 0;
        for c in &manifest.chunks {
            let cstart = pos;
            let cend = pos + c.len as u64;
            pos = cend;
            if cend <= offset {
                continue; // entirely before the range
            }
            if cstart >= end {
                break; // entirely after the range
            }
            let chunk = self.read_blob(Hash::from(c.hash), 0, c.len as u64).await?;
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
    ///
    /// `get_blob` streams bao-verified content, so the returned bytes are
    /// guaranteed to hash to `hash`; a corrupt/wrong blob fails the stream.
    pub async fn fetch_from(&self, hash: Hash, peer: EndpointAddr) -> Result<Bytes> {
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
    use iroh::protocol::Router;

    #[tokio::test]
    async fn reads_local_content_without_peers() {
        let endpoint = Endpoint::bind().await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hash = store.write_content(b"hello local").await.unwrap().unwrap();

        let hydrator = Hydrator::new(endpoint, store);
        let got = hydrator.read(hash, 0, 11).await.unwrap();
        assert_eq!(&got, b"hello local");

        // ranged read (across the manifest)
        let mid = hydrator.read(hash, 6, 5).await.unwrap();
        assert_eq!(&mid, b"local");
    }

    #[tokio::test]
    async fn chunked_large_file_ranged_reads() {
        let endpoint = Endpoint::bind().await.unwrap();
        let store = Arc::new(ContentStore::memory());
        // A multi-chunk buffer (well over MAX_CHUNK).
        let data: Vec<u8> = (0..300_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let mh = store.write_content(&data).await.unwrap().unwrap();
        assert_eq!(store.local(mh).await.is_some(), true, "manifest stored");

        let hyd = Hydrator::new(endpoint, store);
        assert_eq!(hyd.content_len(mh).await.unwrap(), data.len() as u64);

        // whole file
        assert_eq!(hyd.read(mh, 0, data.len() as u64).await.unwrap(), data);

        // ranges that cross chunk boundaries
        for (off, len) in [(0usize, 100usize), (8000, 20000), (150_000, 1), (299_000, 2000)] {
            let end = (off + len).min(data.len());
            let got = hyd.read(mh, off as u64, (end - off) as u64).await.unwrap();
            assert_eq!(got, &data[off..end], "range {off}+{len} mismatch");
        }
    }

    #[tokio::test]
    async fn missing_blob_with_no_peers_errors() {
        let endpoint = Endpoint::bind().await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Hydrator::new(endpoint, store);
        let absent = Hash::from([9u8; 32]);
        assert!(hydrator.read(absent, 0, 16).await.is_err());
    }

    /// The crux of Phase 3: a blob ingested only on node A is read on node B by
    /// fetching it over a real iroh connection (bao-verified), then cached.
    #[tokio::test]
    async fn hydrates_real_bytes_from_a_peer() {
        // --- node A: serve a blob over iroh-blobs ---
        let server_ep = Endpoint::bind().await.unwrap();
        let server_store = ContentStore::memory();
        let payload = Bytes::from_static(b"collaborative filesystem payload");
        let hash = server_store.write_content(payload.as_ref()).await.unwrap().unwrap();
        let _router = Router::builder(server_ep.clone())
            .accept(iroh_blobs::ALPN, server_store.blobs_protocol(None))
            .spawn();
        let server_addr = server_ep.addr();

        // --- node B: empty store, must hydrate from A ---
        let client_ep = Endpoint::bind().await.unwrap();
        let client_store = Arc::new(ContentStore::memory());
        let hydrator = Hydrator::new(client_ep, client_store.clone());

        assert!(
            !client_store.has(hash).await,
            "precondition: B must not have the blob yet"
        );
        hydrator.add_peer(server_addr);

        let got = hydrator.read(hash, 0, payload.len() as u64).await.unwrap();
        assert_eq!(got.as_slice(), payload.as_ref(), "B must read A's real bytes");

        // after hydration the blob is cached locally on B
        assert!(client_store.has(hash).await, "fetched blob must be cached on B");
    }
}
