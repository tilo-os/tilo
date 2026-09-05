//! Content hydration: serve file bytes locally, fetching from peers on demand.
//!
//! A `read()` resolves an inode to a blake3 hash via the namespace CRDT, then
//! asks the [`Hydrator`]: if the blob is in the local [`ContentStore`] it is
//! sliced and returned; otherwise it is fetched from a known peer over
//! iroh-blobs (bao-verified against the hash), persisted, and returned.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use bytes::Bytes;
use futures_lite::StreamExt;
use futures_util::stream::FuturesUnordered;
use iroh::endpoint::Endpoint;
use iroh_base::EndpointAddr;
use iroh_blobs::Hash;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};

use crate::content::Manifest;
use crate::store::ContentStore;
use crate::syncstate::SyncState;

type FetchResult = std::result::Result<Bytes, String>;
type FetchCell = Arc<OnceCell<FetchResult>>;
type FetchMap = HashMap<Hash, FetchCell>;

const PEER_STALE_AFTER: Duration = Duration::from_secs(5 * 60);
const PEER_FETCH_TIMEOUT: Duration = Duration::from_secs(4);
const FETCH_DEADLINE: Duration = Duration::from_secs(6);
const HEDGE_DELAY: Duration = Duration::from_millis(150);

#[derive(Clone)]
struct PeerHealth {
    addr: EndpointAddr,
    last_seen: Instant,
    consecutive_failures: u8,
    retry_after: Instant,
    latency: Option<Duration>,
}

pub struct Hydrator {
    pub endpoint: Endpoint,
    pub store: Arc<ContentStore>,
    /// Peers we may hydrate missing blobs from (populated by the sync layer).
    peers: Arc<RwLock<Vec<PeerHealth>>>,
    /// Number of hydration fetches currently in flight for each blob. This is
    /// refcounted rather than a set: two overlapping guards for the same hash
    /// must keep the file `downloading` until the final fetch ends.
    in_flight: Arc<Mutex<HashMap<Hash, usize>>>,
    /// Per-blob single-flight cells. Concurrent foreground reads and predictive
    /// prefetches await the same peer request instead of downloading identical
    /// bytes independently.
    fetches: Arc<AsyncMutex<FetchMap>>,
    /// Verified peer source for fetched blobs, used as remote-durability evidence.
    remote_sources: Arc<RwLock<HashMap<Hash, EndpointAddr>>>,
    pub runtime_handle: tokio::runtime::Handle,
}

/// Decrements a hash's in-flight count on drop; hold it for the duration of a
/// hydration so `sync_state` reports `downloading` until every overlapping
/// fetch has ended.
pub struct FetchGuard {
    counts: Arc<Mutex<HashMap<Hash, usize>>>,
    hash: Hash,
}

impl Drop for FetchGuard {
    fn drop(&mut self) {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = counts.get_mut(&self.hash) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.hash);
            }
        }
    }
}

impl Hydrator {
    pub fn new(endpoint: Endpoint, store: Arc<ContentStore>) -> Self {
        Self {
            endpoint,
            store,
            peers: Arc::new(RwLock::new(Vec::new())),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            fetches: Arc::new(AsyncMutex::new(HashMap::new())),
            remote_sources: Arc::new(RwLock::new(HashMap::new())),
            runtime_handle: tokio::runtime::Handle::current(),
        }
    }

    /// Whether a fetch of blob `hash` is in flight right now.
    pub fn is_fetching(&self, hash: Hash) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&hash)
            .is_some_and(|count| *count > 0)
    }

    /// Mark `hash` as being fetched; the returned guard releases one reference
    /// on drop. Overlapping fetches therefore cannot clear each other's state.
    pub fn mark_fetching(&self, hash: Hash) -> FetchGuard {
        let mut counts = self
            .in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = counts.entry(hash).or_insert(0);
        *count = count.saturating_add(1);
        drop(counts);
        FetchGuard {
            counts: self.in_flight.clone(),
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
        let mut peers = self
            .peers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if let Some(peer) = peers.iter_mut().find(|peer| peer.addr.id == addr.id) {
            // A stable node id can acquire new direct/relay hints after a
            // network move. Keep the health history, but always use the fresh
            // address rather than requiring a daemon restart.
            peer.addr = addr;
            peer.last_seen = now;
        } else {
            peers.push(PeerHealth {
                addr,
                last_seen: now,
                consecutive_failures: 0,
                retry_after: now,
                latency: None,
            });
        }
    }

    /// Forget a peer removed from authenticated discovery or local policy.
    pub fn remove_peer(&self, id: iroh_base::PublicKey) -> bool {
        let mut peers = self
            .peers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = peers.len();
        peers.retain(|peer| peer.addr.id != id);
        before != peers.len()
    }

    pub fn known_peers(&self) -> Vec<EndpointAddr> {
        let now = Instant::now();
        let mut peers = self
            .peers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        peers.retain(|peer| now.duration_since(peer.last_seen) <= PEER_STALE_AFTER);
        let any_ready = peers.iter().any(|peer| peer.retry_after <= now);
        let mut ranked: Vec<_> = peers
            .iter()
            .filter(|peer| !any_ready || peer.retry_after <= now)
            .cloned()
            .collect();
        ranked.sort_by_key(|peer| {
            (
                peer.consecutive_failures,
                peer.latency.unwrap_or(Duration::from_secs(60)),
                peer.addr.id,
            )
        });
        drop(peers);
        ranked.into_iter().map(|peer| peer.addr).collect()
    }

    fn record_peer_result(&self, id: iroh_base::PublicKey, elapsed: Duration, success: bool) {
        let now = Instant::now();
        let mut peers = self
            .peers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(peer) = peers.iter_mut().find(|peer| peer.addr.id == id) else {
            drop(peers);
            return;
        };
        if success {
            peer.last_seen = now;
            peer.consecutive_failures = 0;
            peer.retry_after = now;
            peer.latency = Some(peer.latency.map_or(elapsed, |old| {
                old.saturating_mul(3)
                    .saturating_add(elapsed)
                    .checked_div(4)
                    .unwrap_or(Duration::ZERO)
            }));
        } else {
            peer.consecutive_failures = peer.consecutive_failures.saturating_add(1);
            let exponent = peer.consecutive_failures.min(6);
            let delay =
                250_u64.saturating_mul(1_u64.checked_shl(u32::from(exponent)).unwrap_or(u64::MAX));
            peer.retry_after = now.checked_add(Duration::from_millis(delay)).unwrap_or(now);
        }
        drop(peers);
    }

    /// Local residency and the exact physical blobs that comprise one file.
    /// `remotely_durable` is deliberately strict: every present component must
    /// have been bao-verified from a peer in this process.
    pub async fn residency(&self, manifest_hash: Hash) -> ContentResidency {
        let mut result = ContentResidency {
            remote_only: true,
            in_flight: self.is_fetching(manifest_hash),
            ..ContentResidency::default()
        };
        let Some(bytes) = self.store.local(manifest_hash).await else {
            return result;
        };
        let Ok(manifest) = Manifest::decode(&bytes) else {
            return result;
        };
        result.remote_only = false;
        result.hydrated_bytes = bytes.len() as u64;
        result.blobs.push(manifest_hash);
        let sources = self
            .remote_sources
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        result.remotely_durable = sources.contains_key(&manifest_hash);
        result.remote_source = sources.get(&manifest_hash).cloned();
        for chunk in manifest.chunks {
            let hash = Hash::from(chunk.hash);
            result.in_flight |= self.is_fetching(hash);
            if self.store.has(hash).await {
                result.hydrated_bytes = result.hydrated_bytes.saturating_add(u64::from(chunk.len));
                result.blobs.push(hash);
                result.remotely_durable &= sources.contains_key(&hash);
                if result.remote_source.as_ref().map(|p| p.id) != sources.get(&hash).map(|p| p.id) {
                    result.remote_source = None;
                }
            } else {
                result.remotely_durable = false;
            }
        }
        result
    }

    /// Revalidate that a peer still serves this exact, content-addressed blob.
    /// The bytes are discarded; this is the last online check before local GC.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails, the peer doesn't serve the blob, or the check times out.
    pub async fn verify_remote_copy(&self, hash: Hash, peer: EndpointAddr) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(20), async {
            let conn = self
                .endpoint
                .connect(peer.clone(), iroh_blobs::ALPN)
                .await?;
            let mut result = iroh_blobs::get::request::get_blob(conn, hash);
            while let Some(item) = result.next().await {
                match item {
                    iroh_blobs::get::request::GetBlobItem::Done(_) => {
                        return Ok::<_, anyhow::Error>(());
                    }
                    iroh_blobs::get::request::GetBlobItem::Error(error) => {
                        return Err(anyhow!(error));
                    }
                    iroh_blobs::get::request::GetBlobItem::Item(_) => {}
                }
            }
            Err(anyhow!("remote durability stream ended early"))
        })
        .await
        .map_err(|_| anyhow!("remote durability check timed out"))?
    }

    /// # Errors
    ///
    /// Returns an error if `hash` is absent locally and no known peer can serve it.
    pub async fn read_blob(&self, hash: Hash, offset: u64, size: u64) -> Result<Vec<u8>> {
        if let Some(bytes) = self.store.local(hash).await {
            return Ok(slice(&bytes, offset, size));
        }

        let cell = {
            let mut fetches = self.fetches.lock().await;
            fetches
                .entry(hash)
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };
        let fetched = cell
            .get_or_init(|| async {
                let peers = self.known_peers();
                if peers.is_empty() {
                    return Err(format!("no peers available to hydrate {hash}"));
                }
                let mut attempts = FuturesUnordered::new();
                for (index, peer) in peers.into_iter().enumerate() {
                    attempts.push(async move {
                        if index > 0 {
                            let delay = u32::try_from(index)
                                .map_or(Duration::MAX, |index| HEDGE_DELAY.saturating_mul(index));
                            tokio::time::sleep(delay).await;
                        }
                        self.fetch_from(hash, peer).await
                    });
                }
                let result = tokio::time::timeout(FETCH_DEADLINE, async {
                    let mut last_err = format!("hydration failed for {hash}");
                    while let Some(result) = attempts.next().await {
                        match result {
                            Ok(bytes) => return Ok(bytes),
                            Err(error) => last_err = error.to_string(),
                        }
                    }
                    Err(last_err)
                })
                .await;
                match result {
                    Ok(result) => result,
                    Err(_) => Err(format!("hydration deadline exceeded for {hash}")),
                }
            })
            .await
            .clone();

        // Remove only our completed cell. A later caller may already have
        // installed a new retry cell after a failed request.
        {
            let mut pending = self.fetches.lock().await;
            if pending
                .get(&hash)
                .is_some_and(|current| Arc::ptr_eq(current, &cell))
            {
                pending.remove(&hash);
            }
        }

        fetched
            .map(|bytes| slice(&bytes, offset, size))
            .map_err(|message| anyhow!(message))
    }

    /// Read a byte range of a file given its manifest hash: fetch the manifest, then only the chunks overlapping `[offset, offset+size)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest or any overlapping chunk can't be loaded (locally or from a peer).
    pub async fn read(&self, manifest_hash: Hash, offset: u64, size: u64) -> Result<Vec<u8>> {
        let manifest = self.load_manifest(manifest_hash).await?;
        let end = offset.saturating_add(size);
        let mut out = Vec::new();
        let mut pos: u64 = 0;
        for c in &manifest.chunks {
            let cstart = pos;
            let cend = pos.saturating_add(u64::from(c.len));
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
            // `chunk.len()` widened back to `u64` is always exact; the range end
            // is then clamped to it, so these `usize` conversions never truncate.
            let chunk_len = chunk.len() as u64;
            let from = usize::try_from(offset.saturating_sub(cstart).min(chunk_len))
                .unwrap_or(chunk.len());
            let to =
                usize::try_from(end.saturating_sub(cstart).min(chunk_len)).unwrap_or(chunk.len());
            if from < to
                && let Some(range) = chunk.get(from..to)
            {
                out.extend_from_slice(range);
            }
        }
        Ok(out)
    }

    /// Total byte length of a file from its manifest (fetches only the manifest).
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest can't be loaded.
    pub async fn content_len(&self, manifest_hash: Hash) -> Result<u64> {
        Ok(self.load_manifest(manifest_hash).await?.total_len())
    }

    /// Hydrate every blob referenced by `manifest_hash` without assembling the
    /// file into one large buffer. Predictive prefetch and explicit offline
    /// pinning use this path: the manifest is fetched first, its declared size
    /// is checked before any content chunks are requested, then missing chunks
    /// are fetched one at a time into the content store.
    ///
    /// Returns `Ok(false)` when the file exceeds `max_bytes`; that is a policy
    /// refusal, not a transport failure. A later foreground read remains free
    /// to hydrate the range the caller actually requested.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest or any required chunk cannot be
    /// fetched, verified, decoded, or persisted.
    pub async fn prefetch(&self, manifest_hash: Hash, max_bytes: u64) -> Result<bool> {
        self.prefetch_with_progress(manifest_hash, max_bytes, |_, _| {})
            .await
    }

    /// Hydrate a complete manifest while reporting verified file-byte
    /// progress. The callback is deliberately synchronous and cheap: callers
    /// can atomically project progress without holding a namespace lock across
    /// network I/O.
    ///
    /// # Errors
    ///
    /// Returns an error when a required manifest or chunk cannot be hydrated.
    pub async fn prefetch_with_progress(
        &self,
        manifest_hash: Hash,
        max_bytes: u64,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<bool> {
        let manifest = self.load_manifest(manifest_hash).await?;
        let total = manifest.total_len();
        if total > max_bytes {
            return Ok(false);
        }
        let mut completed = 0_u64;
        progress(completed, total);
        for chunk in manifest.chunks {
            let hash = Hash::from(chunk.hash);
            if !self.store.has(hash).await {
                let _ = self.read_blob(hash, 0, u64::from(chunk.len)).await?;
            }
            completed = completed.saturating_add(u64::from(chunk.len)).min(total);
            progress(completed, total);
        }
        Ok(true)
    }

    async fn load_manifest(&self, manifest_hash: Hash) -> Result<Manifest> {
        let bytes = self.read_blob(manifest_hash, 0, u64::MAX).await?;
        Manifest::decode(&bytes)
    }

    /// Fetch a whole blob from a specific peer, verify, persist, and return it.
    /// `get_blob` streams bao-verified content, so a corrupt/wrong blob fails the stream rather than returning bad bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection, transfer, or local persistence fails, or the fetch times out.
    pub async fn fetch_from(&self, hash: Hash, peer: EndpointAddr) -> Result<Bytes> {
        // Publish that this blob is downloading so `sync_state` reports it; the guard clears on completion or failure.
        let _guard = self.mark_fetching(hash);
        // Bounded so an unreachable/stalled peer can't hang a read forever.
        let started = Instant::now();
        let peer_id = peer.id;
        let result = match tokio::time::timeout(PEER_FETCH_TIMEOUT, async {
            let conn = self
                .endpoint
                .connect(peer.clone(), iroh_blobs::ALPN)
                .await?;
            let (bytes, _stats) = iroh_blobs::get::request::get_blob(conn, hash)
                .bytes_and_stats()
                .await?;
            self.store.store_fetched(hash, bytes.clone()).await?;
            self.remote_sources
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(hash, peer);
            Ok::<_, anyhow::Error>(bytes)
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow!("hydration from peer timed out")),
        };
        self.record_peer_result(peer_id, started.elapsed(), result.is_ok());
        result
    }
}

#[derive(Debug, Clone, Default)]
pub struct ContentResidency {
    pub hydrated_bytes: u64,
    pub remote_only: bool,
    pub in_flight: bool,
    pub remotely_durable: bool,
    pub blobs: Vec<Hash>,
    pub remote_source: Option<EndpointAddr>,
}

fn slice(bytes: &[u8], offset: u64, size: u64) -> Vec<u8> {
    // `offset`/`size` cross the FUSE boundary as untrusted request values, so
    // clamp rather than trust them to fit `usize` before bounding against `bytes.len()`.
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let end = start
        .saturating_add(usize::try_from(size).unwrap_or(usize::MAX))
        .min(bytes.len());
    bytes
        .get(start..end)
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
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
            .map(|i| u8::try_from((i.wrapping_mul(2_654_435_761) >> 13) & 0xff).unwrap())
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
            .map(|i| u8::try_from((i.wrapping_mul(2_654_435_761) >> 11) & 0xff).unwrap())
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
        let mh = store
            .ingest(Bytes::from(manifest.encode().unwrap()))
            .await
            .unwrap();
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
            .map(|i| u8::try_from((i.wrapping_mul(40_503) >> 9) & 0xff).unwrap())
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

    #[tokio::test]
    async fn overlapping_fetch_guards_keep_downloading_until_the_last_drop() {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let mh = store
            .write_content(b"already local")
            .await
            .unwrap()
            .unwrap();
        let hyd = Hydrator::new(endpoint, store);

        let first = hyd.mark_fetching(mh);
        let second = hyd.mark_fetching(mh);
        assert!(hyd.is_fetching(mh));
        assert_eq!(hyd.sync_state(mh).await, SyncState::Downloading);

        drop(first);
        assert!(
            hyd.is_fetching(mh),
            "dropping one of two guards must not clear the shared state"
        );
        assert_eq!(
            hyd.sync_state(mh).await,
            SyncState::Downloading,
            "the remaining guard keeps the file downloading"
        );

        drop(second);
        assert!(!hyd.is_fetching(mh));
        assert_eq!(hyd.sync_state(mh).await, SyncState::Local);
    }

    #[tokio::test]
    async fn overlapping_guards_for_different_hashes_are_independent() {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let a = Hash::from([0xA1; 32]);
        let b = Hash::from([0xB2; 32]);
        let hyd = Hydrator::new(endpoint, store);

        let a1 = hyd.mark_fetching(a);
        let a2 = hyd.mark_fetching(a);
        let b1 = hyd.mark_fetching(b);
        drop(a1);
        drop(b1);

        assert!(hyd.is_fetching(a), "a still has its second guard");
        assert!(!hyd.is_fetching(b), "b's only guard was released");
        drop(a2);
        assert!(!hyd.is_fetching(a));
    }

    #[tokio::test]
    async fn peer_registration_refreshes_address_and_reclaims_stale_entries() {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Hydrator::new(endpoint, store);
        let key = iroh::SecretKey::from_bytes(&[0x41; 32]).public();
        let old = EndpointAddr::new(key).with_ip_addr("127.0.0.1:4101".parse().unwrap());
        let fresh = EndpointAddr::new(key).with_ip_addr("127.0.0.1:4102".parse().unwrap());

        hydrator.add_peer(old);
        hydrator.add_peer(fresh.clone());
        assert_eq!(hydrator.known_peers(), vec![fresh]);

        hydrator
            .peers
            .write()
            .unwrap()
            .first_mut()
            .unwrap()
            .last_seen = Instant::now()
            .checked_sub(PEER_STALE_AFTER.saturating_add(Duration::from_secs(1)))
            .expect("test monotonic clock has sufficient range");
        assert!(hydrator.known_peers().is_empty());
    }

    #[tokio::test]
    async fn failed_peer_is_backed_off_behind_a_healthy_peer() {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Hydrator::new(endpoint, store);
        let failed = iroh::SecretKey::from_bytes(&[0x51; 32]).public();
        let healthy = iroh::SecretKey::from_bytes(&[0x52; 32]).public();
        hydrator.add_peer(EndpointAddr::new(failed));
        hydrator.add_peer(EndpointAddr::new(healthy));

        hydrator.record_peer_result(failed, Duration::from_secs(2), false);
        assert_eq!(hydrator.known_peers()[0].id, healthy);
        assert!(hydrator.remove_peer(healthy));
        // If every peer is cooling down, return the best remaining candidate
        // so a foreground retry is never permanently starved.
        assert_eq!(hydrator.known_peers()[0].id, failed);
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

    #[tokio::test]
    async fn stale_first_peer_does_not_block_a_healthy_replica() {
        let server_ep = Endpoint::bind(presets::N0).await.unwrap();
        let server_store = ContentStore::memory();
        let payload = Bytes::from_static(b"healthy replica wins the hedge");
        let hash = server_store.ingest(payload.clone()).await.unwrap();
        let _router = Router::builder(server_ep.clone())
            .accept(iroh_blobs::ALPN, server_store.blobs_protocol(None))
            .spawn();

        let client_ep = Endpoint::bind(presets::N0).await.unwrap();
        let client_store = Arc::new(ContentStore::memory());
        let hydrator = Hydrator::new(client_ep, client_store);
        let ghost = iroh::SecretKey::from_bytes(&[0x61; 32]).public();
        hydrator.add_peer(EndpointAddr::new(ghost));
        let server_addr = server_ep.addr();
        let server_id = server_addr.id;
        hydrator.add_peer(server_addr);
        hydrator.record_peer_result(ghost, Duration::from_millis(1), true);
        hydrator.record_peer_result(server_id, Duration::from_millis(2), true);
        assert_eq!(hydrator.known_peers()[0].id, ghost);

        let started = Instant::now();
        let got = hydrator.read_blob(hash, 0, u64::MAX).await.unwrap();
        assert_eq!(got.as_slice(), payload.as_ref());
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "a dead first peer must not consume its full timeout before the healthy peer is tried"
        );
    }

    #[tokio::test]
    async fn prefetch_hydrates_every_chunk_without_returning_the_file() {
        let server_ep = Endpoint::bind(presets::N0).await.unwrap();
        let server_store = ContentStore::memory();
        let payload: Vec<u8> = (0..400_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 11).to_le_bytes()[0])
            .collect();
        let manifest_hash = server_store.write_content(&payload).await.unwrap().unwrap();
        let _router = Router::builder(server_ep.clone())
            .accept(iroh_blobs::ALPN, server_store.blobs_protocol(None))
            .spawn();

        let client_ep = Endpoint::bind(presets::N0).await.unwrap();
        let client_store = Arc::new(ContentStore::memory());
        let hydrator = Hydrator::new(client_ep, client_store);
        hydrator.add_peer(server_ep.addr());

        assert_eq!(
            hydrator.sync_state(manifest_hash).await,
            SyncState::Projected
        );
        assert!(
            hydrator
                .prefetch(manifest_hash, payload.len() as u64)
                .await
                .unwrap(),
            "the bounded file is accepted for prefetch"
        );
        assert_eq!(
            hydrator.sync_state(manifest_hash).await,
            SyncState::Local,
            "manifest and every content chunk are local afterwards"
        );
        assert_eq!(
            hydrator
                .read(manifest_hash, 0, payload.len() as u64)
                .await
                .unwrap(),
            payload
        );
    }

    #[tokio::test]
    async fn prefetch_refuses_a_file_over_the_policy_limit() {
        let endpoint = Endpoint::bind(presets::N0).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let payload = vec![7u8; 1024];
        let manifest_hash = store.write_content(&payload).await.unwrap().unwrap();
        let hydrator = Hydrator::new(endpoint, store);
        assert!(!hydrator.prefetch(manifest_hash, 512).await.unwrap());
    }
}
