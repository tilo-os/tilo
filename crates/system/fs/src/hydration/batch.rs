//! Batched, pipelined content fetches over one pooled connection per peer.
//!
//! A want list (manifests, chunks, whole small files) becomes as few
//! iroh-blobs `GetMany` requests as possible: up to [`BATCH_BLOBS`] blobs or
//! [`BATCH_BYTES`] per request, [`WINDOW_PER_PEER`] requests in flight on
//! each peer's connection, and batches spread over every healthy peer, so a
//! faster peer simply serves more of them. Throughput is then bounded by the
//! link rather than by round trips per blob.
//!
//! Each request also names the manifests its chunks belong to, with empty
//! ranges: the serving peer authorises a chunk as part of a readable
//! manifest in the same request instead of searching for one. Everything
//! received is BAO-verified by the protocol and hash-checked again before
//! it is stored, one durability barrier per batch.

use std::collections::HashSet;

use iroh::endpoint::Connection;
use iroh_blobs::get::fsm::{self, EndBlobNext, RequestCounters};
use iroh_blobs::protocol::{ChunkRanges, ChunkRangesSeq, GetManyRequest};

use super::{Bytes, Duration, EndpointAddr, Hash, Hydrator, Instant, Result, anyhow};
use tilo_lock::RwLockRecover as _;

/// Blobs requested in one `GetMany`.
pub const BATCH_BLOBS: usize = 512;
/// Bytes requested in one `GetMany`, by the callers' size hints.
pub const BATCH_BYTES: u64 = 8 * 1024 * 1024;
/// `GetMany` requests in flight per peer: with [`BATCH_BYTES`] up to 64 MiB
/// outstanding, enough for a gigabit link at 500 ms round trip.
pub const WINDOW_PER_PEER: usize = 8;
/// Largest single blob accepted in a batch response.
const MAX_BATCH_BLOB: u64 = 64 * 1024 * 1024;
/// A batch that makes no progress for this long is abandoned for the next
/// peer.
const BATCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Connecting to a peer for the pool.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

/// One blob to fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Want {
    pub hash: Hash,
    /// Expected size in bytes, for batching; 0 when unknown.
    pub size_hint: u64,
    /// The manifest this blob is a chunk of, when it is one. The serving
    /// peer authorises the chunk through it.
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

    /// A chunk of the manifest `root`.
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
    /// Blobs fetched and stored.
    pub fetched: usize,
    pub bytes: u64,
    /// Wanted blobs no peer served.
    pub missing: Vec<Hash>,
}

fn batches(wants: Vec<Want>) -> Vec<Vec<Want>> {
    let mut out = Vec::new();
    let mut current: Vec<Want> = Vec::new();
    let mut bytes = 0_u64;
    for want in wants {
        let size = want.size_hint.max(1);
        if !current.is_empty()
            && (current.len() >= BATCH_BLOBS || bytes.saturating_add(size) > BATCH_BYTES)
        {
            out.push(std::mem::take(&mut current));
            bytes = 0;
        }
        bytes = bytes.saturating_add(size);
        current.push(want);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

impl Hydrator {
    /// The pooled blob-protocol connection to `peer`, opened on first use
    /// and reused for every later request until it closes.
    ///
    /// # Errors
    ///
    /// Returns an error when no connection can be opened in time.
    pub async fn blob_connection(&self, peer: &EndpointAddr) -> Result<Connection> {
        self.blob_connection_lane(peer, 0).await
    }

    /// The pooled connection to `peer` in `lane`. One QUIC connection is
    /// processed by one task on each side, which bounds its throughput;
    /// large transfers therefore spread their requests over a few lanes.
    ///
    /// # Errors
    ///
    /// Returns an error when no connection can be opened in time.
    pub(super) async fn blob_connection_lane(
        &self,
        peer: &EndpointAddr,
        lane: usize,
    ) -> Result<Connection> {
        let key = (peer.id, lane);
        {
            let pool = self.blob_connections.lock().await;
            if let Some(connection) = pool.get(&key)
                && connection.close_reason().is_none()
            {
                return Ok(connection.clone());
            }
        }
        let connection = tokio::time::timeout(
            CONNECT_TIMEOUT,
            self.endpoint.connect(peer.clone(), iroh_blobs::ALPN),
        )
        .await
        .map_err(|_| anyhow!("connecting to {} timed out", peer.id))??;
        let mut pool = self.blob_connections.lock().await;
        // Another caller may have connected meanwhile; keep one.
        if let Some(existing) = pool.get(&key)
            && existing.close_reason().is_none()
        {
            connection.close(0u32.into(), b"duplicate");
            return Ok(existing.clone());
        }
        pool.insert(key, connection.clone());
        drop(pool);
        Ok(connection)
    }

    /// Drops the pooled connection to `peer` after a failure, so the next
    /// request connects afresh.
    pub(super) async fn forget_blob_connection(
        &self,
        peer: iroh_base::PublicKey,
        failed: &Connection,
    ) {
        let mut pool = self.blob_connections.lock().await;
        pool.retain(|(id, _), c| *id != peer || c.stable_id() != failed.stable_id());
    }

    /// Fetches every blob in `wants` that is not local yet, in batched,
    /// pipelined `GetMany` requests spread across the known peers.
    /// Best effort: what no peer serves is reported, not an error.
    pub async fn fetch_many(&self, wants: Vec<Want>) -> FetchReport {
        let mut seen = HashSet::with_capacity(wants.len());
        let mut todo = Vec::with_capacity(wants.len());
        for want in wants {
            if seen.insert(want.hash) && !self.store.has(want.hash).await {
                todo.push(want);
            }
        }
        let mut report = FetchReport::default();
        if todo.is_empty() {
            return report;
        }
        let peers = self.known_peers();
        if peers.is_empty() {
            report.missing = todo.iter().map(|w| w.hash).collect();
            return report;
        }
        let window = WINDOW_PER_PEER.saturating_mul(peers.len());
        let jobs = batches(todo).into_iter().enumerate().map(|(index, batch)| {
            let peers = &peers;
            async move { self.fetch_batch_any(peers, index, batch).await }
        });
        let results: Vec<(usize, u64, Vec<Hash>)> = futures_util::StreamExt::collect(
            futures_util::StreamExt::buffer_unordered(futures_util::stream::iter(jobs), window),
        )
        .await;
        for (fetched, bytes, missing) in results {
            report.fetched = report.fetched.saturating_add(fetched);
            report.bytes = report.bytes.saturating_add(bytes);
            report.missing.extend(missing);
        }
        report
    }

    /// One batch, from the peer `index` selects and then from the others
    /// for whatever is still missing. Returns the blobs and bytes fetched and
    /// the hashes nobody served.
    async fn fetch_batch_any(
        &self,
        peers: &[EndpointAddr],
        index: usize,
        mut batch: Vec<Want>,
    ) -> (usize, u64, Vec<Hash>) {
        let mut fetched = 0_usize;
        let mut bytes = 0_u64;
        // Start at the peer `index` selects and go round the others.
        let (earlier, later) = peers.split_at(index.checked_rem(peers.len()).unwrap_or(0));
        for peer in later.iter().chain(earlier) {
            let started = Instant::now();
            let Ok(connection) = self.blob_connection(peer).await else {
                self.record_peer_result(peer.id, started.elapsed(), false);
                continue;
            };
            let (got, got_bytes, error) = self.fetch_batch_from(&connection, peer, &batch).await;
            fetched = fetched.saturating_add(got.len());
            bytes = bytes.saturating_add(got_bytes);
            batch.retain(|want| !got.contains(&want.hash));
            let failed = error.is_some();
            self.record_peer_result(peer.id, started.elapsed(), !failed);
            if failed && connection.close_reason().is_some() {
                self.forget_blob_connection(peer.id, &connection).await;
            }
            if batch.is_empty() {
                break;
            }
        }
        (fetched, bytes, batch.into_iter().map(|w| w.hash).collect())
    }

    /// One `GetMany` on `connection`. Whatever arrived before an error is
    /// verified and stored; the error is returned alongside.
    async fn fetch_batch_from(
        &self,
        connection: &Connection,
        peer: &EndpointAddr,
        batch: &[Want],
    ) -> (HashSet<Hash>, u64, Option<anyhow::Error>) {
        let wanted: HashSet<Hash> = batch.iter().map(|w| w.hash).collect();
        let mut roots: Vec<Hash> = batch
            .iter()
            .filter_map(|w| w.root)
            .filter(|root| !wanted.contains(root))
            .collect();
        roots.sort();
        roots.dedup();
        let mut hashes = roots.clone();
        hashes.extend(batch.iter().map(|w| w.hash));
        let ranges = std::iter::repeat_n(ChunkRanges::empty(), roots.len())
            .chain(std::iter::repeat_n(ChunkRanges::all(), batch.len()));
        let request = GetManyRequest::new(hashes.clone(), ChunkRangesSeq::from_ranges(ranges));
        let _guards: Vec<_> = batch.iter().map(|w| self.mark_fetching(w.hash)).collect();
        let mut received: Vec<(Hash, Bytes)> = Vec::with_capacity(batch.len());
        let transfer = async {
            let mut next =
                fsm::start_get_many(connection.clone(), request, RequestCounters::default())
                    .await
                    .map_err(|e| anyhow!("{e}"))?;
            loop {
                match next {
                    Ok(child) => {
                        let offset = usize::try_from(child.offset()).unwrap_or(usize::MAX);
                        let hash = *hashes
                            .get(offset)
                            .ok_or_else(|| anyhow!("response child {offset} out of range"))?;
                        let (content, size) =
                            child.next(hash).next().await.map_err(|e| anyhow!("{e}"))?;
                        anyhow::ensure!(
                            size <= MAX_BATCH_BLOB,
                            "peer announced a {size} byte blob"
                        );
                        let (end, bytes) = content
                            .concatenate_into_vec()
                            .await
                            .map_err(|e| anyhow!("{e}"))?;
                        received.push((hash, Bytes::from(bytes)));
                        next = match end.next() {
                            EndBlobNext::MoreChildren(child) => Ok(child),
                            EndBlobNext::Closing(closing) => Err(closing),
                        };
                    }
                    Err(closing) => {
                        closing.next().await.map_err(|e| anyhow!("{e}"))?;
                        return Ok::<_, anyhow::Error>(());
                    }
                }
            }
        };
        let error = match tokio::time::timeout(BATCH_TIMEOUT, transfer).await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(_) => Some(anyhow!("batch from {} timed out", peer.id)),
        };
        let bytes = received.iter().map(|(_, b)| b.len() as u64).sum();
        self.count_blob_bytes(bytes);
        let got: HashSet<Hash> = received.iter().map(|(h, _)| *h).collect();
        if let Err(store_error) = self.store.store_fetched_batch(received).await {
            return (HashSet::new(), 0, Some(store_error));
        }
        {
            let mut sources = self.remote_sources.write_recover();
            for hash in &got {
                sources.insert(*hash, peer.clone());
            }
        }
        (got, bytes, error)
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
                    u64::from(chunk.len),
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

    /// Stores content an authenticated peer sent along with the records
    /// naming it. Every blob is hash-checked before anything is stored, and
    /// counts as fetched from `peer`, as a verified download does.
    ///
    /// # Errors
    ///
    /// Returns an error on a hash mismatch or when the store fails.
    pub async fn accept_inline(
        &self,
        peer: &EndpointAddr,
        blobs: Vec<(Hash, Bytes)>,
    ) -> Result<()> {
        let hashes: Vec<Hash> = blobs.iter().map(|(hash, _)| *hash).collect();
        self.store.store_fetched_batch(blobs).await?;
        let mut sources = self.remote_sources.write_recover();
        for hash in hashes {
            sources.insert(hash, peer.clone());
        }
        drop(sources);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn want(n: u8, size: u64) -> Want {
        Want::root(Hash::from([n; 32]), size)
    }

    #[test]
    fn batches_respect_count_and_byte_bounds() {
        let small: Vec<Want> = (0..=255u8).chain(0..=255u8).map(|n| want(n, 10)).collect();
        let split = batches(small.clone());
        assert_eq!(split.len(), 1, "512 small blobs fit one request");
        let mut more = small;
        more.push(want(1, 10));
        assert_eq!(batches(more).len(), 2);

        let big: Vec<Want> = (0..5u8).map(|n| want(n, 3 * 1024 * 1024)).collect();
        let split = batches(big);
        assert_eq!(
            split.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![2, 2, 1],
            "8 MiB per request"
        );
        let huge = vec![want(1, 100 * 1024 * 1024), want(2, 1)];
        assert_eq!(batches(huge).len(), 2, "an oversized blob travels alone");
    }
}
