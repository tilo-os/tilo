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

    /// Read a byte range of `hash`, hydrating from a peer if not already local.
    pub async fn read(&self, hash: Hash, offset: u64, size: u64) -> Result<Vec<u8>> {
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
        let hash = store.ingest(Bytes::from_static(b"hello local")).await.unwrap();

        let hydrator = Hydrator::new(endpoint, store);
        let got = hydrator.read(hash, 0, 11).await.unwrap();
        assert_eq!(&got, b"hello local");

        // ranged read
        let mid = hydrator.read(hash, 6, 5).await.unwrap();
        assert_eq!(&mid, b"local");
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
        let hash = server_store.ingest(payload.clone()).await.unwrap();
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
