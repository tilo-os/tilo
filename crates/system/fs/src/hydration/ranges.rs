//! Byte ranges of file-backed content, fetched straight into the content
//! store's area.
//!
//! A range of a large file becomes a few `GET` requests for its content id
//! with a `RangeSpecSeq` naming exactly the chunks it overlaps, and within
//! each chunk exactly the 1 KiB chunk numbers needed. The provider streams
//! them BAO-verified in 16 KiB groups; every verified group is written at
//! its offset in the entry's file, and the ranges written are recorded
//! resident once they are durable. Nothing is assembled in memory, so a
//! 10 GiB file costs no more memory than a 10 MiB one.
//!
//! - **Dedup.** A chunk this device already holds, in another entry or as a
//!   loose blob, is copied locally (the kernel clones extents where it can)
//!   instead of fetched.
//! - **Resume.** Residency is persisted, so an interrupted transfer, or a
//!   restarted daemon, asks only for what is still missing.
//! - **One fetch per range.** Concurrent readers of overlapping ranges wait
//!   for the fetch already under way instead of repeating it.
//! - **Priorities.** A blocked read and the read-ahead of an active reader
//!   are interactive: they are always sent at once. Pinned, predictive and
//!   backfill work waits while interactive requests are in flight, so a
//!   large background transfer delays a foreground read by at most the
//!   requests already on the wire.
//! - **Peers.** Requests are spread over every healthy peer, several in
//!   flight per peer; what one peer fails to serve is asked of the next.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::os::unix::fs::FileExt as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bao_tree::ChunkNum;
use bao_tree::io::BaoContentItem;
use iroh::endpoint::Connection;
use iroh_blobs::get::fsm::{self, BlobContentNext, ConnectedNext, EndBlobNext, RequestCounters};
use iroh_blobs::protocol::{ChunkRanges, GetRequest};
use tokio::sync::Notify;

use super::{Duration, EndpointAddr, Hash, Hydrator, Result, anyhow};
use crate::content::{self, Manifest, RangeSet};
use crate::store::Retention;
use tilo_lock::MutexRecover as _;

/// Requested bytes per `GET` for background work and read-ahead.
const REQUEST_BYTES: u64 = 16 * 1024 * 1024;
/// Requested bytes per `GET` for a blocked read.
const FOREGROUND_REQUEST_BYTES: u64 = 2 * 1024 * 1024;
/// `GET` requests in flight per peer.
const REQUESTS_PER_PEER: usize = 8;
/// Connections per peer the requests of one transfer are spread over.
const LANES_PER_PEER: usize = 4;
/// A request that makes no progress for this long is abandoned for the
/// next peer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Granularity of what is fetched: BAO verification works in 16 KiB
/// groups, so a request never asks for less.
const GROUP: u64 = 16 * 1024;

/// Why content is being fetched, from most to least urgent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Replicating content nobody is waiting for.
    Backfill,
    /// Content a reader is predicted to want.
    Predictive,
    /// Content kept offline, or needed for a lease.
    Pinned,
    /// Ahead of an active sequential reader.
    ReadAhead,
    /// A reader is blocked on it.
    Foreground,
}

impl Priority {
    const fn interactive(self) -> bool {
        matches!(self, Self::Foreground | Self::ReadAhead)
    }

    const fn retention(self) -> Retention {
        match self {
            Self::Pinned => Retention::Pinned,
            _ => Retention::Cached,
        }
    }
}

/// Orders requests by priority: background requests wait while any
/// interactive request is in flight.
#[derive(Default)]
pub(crate) struct Scheduler {
    interactive: AtomicUsize,
    idle: Notify,
}

/// Held while a request is on the wire.
struct Admission<'a> {
    scheduler: &'a Scheduler,
    interactive: bool,
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        if self.interactive && self.scheduler.interactive.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.scheduler.idle.notify_waiters();
        }
    }
}

impl Scheduler {
    async fn admit(&self, priority: Priority) -> Admission<'_> {
        if priority.interactive() {
            self.interactive.fetch_add(1, Ordering::AcqRel);
            return Admission {
                scheduler: self,
                interactive: true,
            };
        }
        loop {
            let idle = self.idle.notified();
            tokio::pin!(idle);
            // Registered before the check, so a release in between is seen.
            idle.as_mut().enable();
            if self.interactive.load(Ordering::Acquire) == 0 {
                break;
            }
            idle.await;
        }
        Admission {
            scheduler: self,
            interactive: false,
        }
    }

    /// Interactive requests on the wire right now.
    #[cfg(test)]
    fn interactive_in_flight(&self) -> usize {
        self.interactive.load(Ordering::Acquire)
    }
}

/// A wait for another reader's fetch to give its ranges back
/// ([`Flights::claim`]).
type Released = std::pin::Pin<Box<tokio::sync::futures::OwnedNotified>>;

/// Ranges of each content id being fetched right now.
#[derive(Default)]
pub(crate) struct Flights {
    ranges: std::sync::Mutex<HashMap<Hash, (RangeSet, Arc<Notify>)>>,
}

impl Flights {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Hash, (RangeSet, Arc<Notify>)>> {
        self.ranges.lock_recover()
    }

    /// Claims the parts of `wanted` nobody is fetching. Returns them, and,
    /// when others hold the rest, a wait for them to give ranges back. The
    /// wait is registered before the claim lets go of the lock, so a fetch
    /// that fails and gives its ranges back before the caller waits still
    /// wakes it.
    fn claim(&self, content: Hash, wanted: &[Range<u64>]) -> (Vec<Range<u64>>, Option<Released>) {
        let mut claimed = Vec::new();
        let mut blocked = false;
        let mut map = self.lock();
        let (busy, notify) = map
            .entry(content)
            .or_insert_with(|| (RangeSet::new(), Arc::new(Notify::new())));
        for range in wanted {
            let free = busy.missing(range);
            blocked |= free
                .iter()
                .map(|r| r.end.saturating_sub(r.start))
                .sum::<u64>()
                < range.end.saturating_sub(range.start);
            claimed.extend(free);
        }
        for range in &claimed {
            busy.insert(range.clone());
        }
        let wait = blocked.then(|| {
            let mut released = Box::pin(Arc::clone(notify).notified_owned());
            released.as_mut().enable();
            released
        });
        drop(map);
        (claimed, wait)
    }

    fn release(&self, content: Hash, claimed: &[Range<u64>]) {
        let mut map = self.lock();
        if let Some((busy, notify)) = map.get_mut(&content) {
            for range in claimed {
                busy.remove(range.clone());
            }
            notify.notify_waiters();
            if busy.is_empty() {
                map.remove(&content);
            }
        }
    }
}

/// Ranges claimed from [`Flights`], given back when dropped: a fetch
/// abandoned part way (a predictive prefetch cancelled when the policy
/// changes) never leaves them claimed for readers to wait on forever.
struct Claim<'a> {
    flights: &'a Flights,
    content: Hash,
    ranges: Vec<Range<u64>>,
}

impl Claim<'_> {
    /// Gives the ranges back and returns them.
    fn release(mut self) -> Vec<Range<u64>> {
        let ranges = std::mem::take(&mut self.ranges);
        self.flights.release(self.content, &ranges);
        ranges
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if !self.ranges.is_empty() {
            self.flights.release(self.content, &self.ranges);
        }
    }
}

/// One chunk's share of a request: which bytes of it are wanted.
#[derive(Clone, Debug)]
struct ChunkWant {
    index: usize,
    /// Byte range within the chunk, aligned to BAO groups.
    within: Range<u64>,
}

impl ChunkWant {
    fn ranges(&self) -> ChunkRanges {
        ChunkRanges::from(
            ChunkNum::full_chunks(self.within.start)..ChunkNum::chunks(self.within.end),
        )
    }

    fn bytes(&self) -> u64 {
        self.within.end.saturating_sub(self.within.start)
    }
}

/// The parts of the chunks of `manifest` that cover `ranges`, one entry per
/// chunk, aligned to whole BAO groups.
fn chunk_wants(manifest: &Manifest, ranges: &[Range<u64>]) -> Vec<ChunkWant> {
    let mut wants: Vec<ChunkWant> = Vec::new();
    for range in ranges {
        for (index, start, chunk) in manifest.overlapping(range.clone()) {
            let len = u64::from(chunk.len);
            let from = range.start.saturating_sub(start).min(len);
            let to = range.end.saturating_sub(start).min(len);
            let from = from.checked_div(GROUP).unwrap_or(0).saturating_mul(GROUP);
            let to = to.div_ceil(GROUP).saturating_mul(GROUP).min(len);
            match wants.last_mut() {
                Some(last) if last.index == index => {
                    last.within.start = last.within.start.min(from);
                    last.within.end = last.within.end.max(to);
                }
                _ => wants.push(ChunkWant {
                    index,
                    within: from..to,
                }),
            }
        }
    }
    wants
}

/// Splits wants into requests of about `max_bytes`, in order.
fn requests(wants: Vec<ChunkWant>, max_bytes: u64) -> Vec<Vec<ChunkWant>> {
    let mut out = Vec::new();
    let mut current: Vec<ChunkWant> = Vec::new();
    let mut bytes = 0_u64;
    for want in wants {
        if !current.is_empty() && bytes.saturating_add(want.bytes()) > max_bytes {
            out.push(std::mem::take(&mut current));
            bytes = 0;
        }
        bytes = bytes.saturating_add(want.bytes());
        current.push(want);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

impl Hydrator {
    /// Bytes of file-backed content received from peers so far.
    pub fn fetched_range_bytes(&self) -> u64 {
        self.ranged_bytes.load(Ordering::Relaxed)
    }

    /// Makes bytes `range` of file-backed `content` resident in the area,
    /// fetching what no local entry holds. Returns once every byte of the
    /// range is resident.
    ///
    /// # Errors
    ///
    /// Returns an error when part of the range could not be fetched from
    /// any peer, or the area cannot be written.
    pub async fn ensure_range(
        &self,
        content: Hash,
        manifest: &Manifest,
        range: Range<u64>,
        priority: Priority,
    ) -> Result<()> {
        let size = manifest.total_len();
        let range = range.start.min(size)..range.end.min(size);
        if range.is_empty() {
            return Ok(());
        }
        let key = *content.as_bytes();
        let files = Arc::clone(self.store.files());
        let entry = files.entry(&key);
        if entry.as_ref().is_none_or(|e| e.size != size) {
            let chunks = content::chunk_locations(manifest);
            let retention = priority.retention();
            let files = Arc::clone(&files);
            tokio::task::spawn_blocking(move || files.ensure(&key, size, retention, &chunks))
                .await??;
        } else if priority == Priority::Pinned {
            files.keep_offline(&key, true)?;
        }
        // Interactive work claims all of it at once; background work claims
        // request by request, so it never holds ranges while it waits.
        let pieces: Vec<Range<u64>> = if priority.interactive() {
            vec![range.clone()]
        } else {
            let mut pieces = Vec::new();
            let mut at = range.start;
            while at < range.end {
                let end = at.saturating_add(REQUEST_BYTES).min(range.end);
                pieces.push(at..end);
                at = end;
            }
            pieces
        };
        for piece in pieces {
            self.ensure_piece(content, manifest, piece, priority)
                .await?;
        }
        files.touch(&key);
        // What was fetched counts against the cache budget; the entry just
        // filled stays.
        tokio::task::spawn_blocking(move || files.evict_to_budget(&[key])).await?;
        Ok(())
    }

    async fn ensure_piece(
        &self,
        content: Hash,
        manifest: &Manifest,
        piece: Range<u64>,
        priority: Priority,
    ) -> Result<()> {
        let key = *content.as_bytes();
        let files = Arc::clone(self.store.files());
        // What this call fetched itself: still missing afterwards, nobody
        // served it. A range another reader's failed fetch released is
        // fetched here instead.
        let mut fetched: Vec<Range<u64>> = Vec::new();
        loop {
            let missing = files.missing(&key, &piece);
            if missing.is_empty() {
                return Ok(());
            }
            let unserved: u64 = missing
                .iter()
                .flat_map(|m| fetched.iter().map(move |f| (m, f)))
                .map(|(m, f)| m.end.min(f.end).saturating_sub(m.start.max(f.start)))
                .sum();
            if unserved > 0 {
                return Err(anyhow!(
                    "{unserved} bytes of {content} were not served by any peer"
                ));
            }
            // Background work is admitted before it claims anything, so it
            // never holds ranges a reader waits for while it waits itself.
            let admission = self.scheduler.admit(priority).await;
            let (claimed, wait) = self.flights.claim(content, &missing);
            if claimed.is_empty() {
                drop(admission);
                if let Some(released) = wait
                    && !files.missing(&key, &piece).is_empty()
                {
                    released.await;
                }
                continue;
            }
            let claim = Claim {
                flights: &self.flights,
                content,
                ranges: claimed,
            };
            let result = self
                .fetch_claimed(content, manifest, &claim.ranges, priority)
                .await;
            let claimed = claim.release();
            drop(admission);
            result?;
            fetched.extend(claimed);
        }
    }

    /// Fills `claimed` of `content`: first from chunks held locally, then
    /// from peers.
    async fn fetch_claimed(
        &self,
        content: Hash,
        manifest: &Manifest,
        claimed: &[Range<u64>],
        priority: Priority,
    ) -> Result<()> {
        let wants = chunk_wants(manifest, claimed);
        let wants = self.fill_from_local(content, manifest, wants).await?;
        if wants.is_empty() {
            return Ok(());
        }
        let peers = self.known_peers();
        if peers.is_empty() {
            return Err(anyhow!("no peers to fetch {content} from"));
        }
        let max_bytes = if priority == Priority::Foreground {
            // A blocked reader waits for all of it: smaller requests,
            // answered in parallel over the lanes.
            FOREGROUND_REQUEST_BYTES
        } else {
            REQUEST_BYTES
        };
        let batches = requests(wants, max_bytes);
        let window = REQUESTS_PER_PEER.saturating_mul(peers.len()).max(1);
        let jobs = batches.into_iter().enumerate().map(|(index, batch)| {
            let peers = &peers;
            async move {
                self.fetch_request_any(content, manifest, peers, index, batch)
                    .await
            }
        });
        let results: Vec<Result<()>> = futures_util::StreamExt::collect(
            futures_util::StreamExt::buffer_unordered(futures_util::stream::iter(jobs), window),
        )
        .await;
        results.into_iter().find(Result::is_err).unwrap_or(Ok(()))
    }

    /// Copies wanted chunks that are resident elsewhere on this device
    /// (another area entry, or a loose blob) and returns the rest.
    async fn fill_from_local(
        &self,
        content: Hash,
        manifest: &Manifest,
        wants: Vec<ChunkWant>,
    ) -> Result<Vec<ChunkWant>> {
        let key = *content.as_bytes();
        let files = Arc::clone(self.store.files());
        let mut rest = Vec::with_capacity(wants.len());
        let mut copied = Vec::new();
        let mut dest_generation: Option<u64> = files.generation(&key);
        for want in wants {
            let (Some(chunk), Some(range)) = (
                manifest.chunks().get(want.index).copied(),
                manifest.chunk_range(want.index),
            ) else {
                continue;
            };
            if let Some(location) = files
                .find_chunk(&chunk.hash)
                .filter(|l| l.key != key && l.len == chunk.len)
            {
                let files = Arc::clone(&files);
                let start = range.start;
                let copy = tokio::task::spawn_blocking(move || {
                    let (source, source_generation) = files.file_in_generation(&location.key)?;
                    let (dest, dest_generation) = files.file_in_generation(&key)?;
                    crate::store::area::copy_range(
                        &source,
                        location.offset,
                        &dest,
                        start,
                        u64::from(location.len),
                    )?;
                    // The source must still be the entry that held the
                    // chunk: one evicted and created anew meanwhile is
                    // sparse where the chunk was.
                    let intact = files.generation(&location.key) == Some(source_generation)
                        && files.missing(&location.key, &location.range()).is_empty();
                    if !intact {
                        return Err(std::io::Error::other("the source entry changed"));
                    }
                    Ok::<_, std::io::Error>(dest_generation)
                })
                .await?;
                if let Ok(generation) = copy
                    && *dest_generation.get_or_insert(generation) == generation
                {
                    copied.push(range);
                    continue;
                }
            }
            if let Ok(bytes) = self
                .store
                .store()
                .blobs()
                .get_bytes(Hash::from(chunk.hash))
                .await
                && bytes.len() as u64 == u64::from(chunk.len)
                && let Ok((dest, generation)) = files.file_in_generation(&key)
                && *dest_generation.get_or_insert(generation) == generation
            {
                dest.write_all_at(&bytes, range.start)?;
                copied.push(range);
                continue;
            }
            rest.push(want);
        }
        if !copied.is_empty() {
            let files = Arc::clone(&files);
            tokio::task::spawn_blocking(move || {
                files.mark_resident_in(&key, dest_generation, &copied, false)
            })
            .await??;
        }
        Ok(rest)
    }

    /// One request, from the peer `index` selects and then from the others
    /// for whatever is still missing.
    async fn fetch_request_any(
        &self,
        content: Hash,
        manifest: &Manifest,
        peers: &[EndpointAddr],
        index: usize,
        batch: Vec<ChunkWant>,
    ) -> Result<()> {
        let (earlier, later) = peers.split_at(index.checked_rem(peers.len()).unwrap_or(0));
        let mut remaining = batch;
        let mut last_error = anyhow!("no peer served {content}");
        for peer in later.iter().chain(earlier) {
            let started = std::time::Instant::now();
            let lane = index.checked_rem(LANES_PER_PEER).unwrap_or(0);
            let connection = match self.blob_connection_lane(peer, lane).await {
                Ok(connection) => connection,
                Err(error) => {
                    self.record_peer_result(peer.id, started.elapsed(), false);
                    last_error = error;
                    continue;
                }
            };
            let (done, error) = self
                .fetch_request(&connection, content, manifest, &remaining)
                .await;
            remaining.retain(|w| !done.contains(&w.index));
            let failed = error.is_some();
            self.record_peer_result(peer.id, started.elapsed(), !failed);
            if let Some(error) = error {
                if connection.close_reason().is_some() {
                    self.forget_blob_connection(peer.id, &connection).await;
                }
                last_error = error;
            }
            if remaining.is_empty() {
                return Ok(());
            }
        }
        Err(last_error)
    }

    /// One `GET` on `connection`. Every verified group is written to the
    /// area as it arrives; what arrived is recorded resident even when the
    /// request fails part way. Returns the chunk indexes completed and the
    /// error, if any.
    #[allow(
        clippy::too_many_lines,
        reason = "one response, read child by child as the protocol state machine hands it over"
    )]
    async fn fetch_request(
        &self,
        connection: &Connection,
        content: Hash,
        manifest: &Manifest,
        wants: &[ChunkWant],
    ) -> (HashSet<usize>, Option<anyhow::Error>) {
        let key = *content.as_bytes();
        let files = Arc::clone(self.store.files());
        let (file, generation) = match files.file_in_generation(&key) {
            Ok(file) => file,
            Err(error) => return (HashSet::new(), Some(error.into())),
        };
        let by_offset: HashMap<u64, &ChunkWant> = wants
            .iter()
            .map(|w| (Manifest::child_offset(w.index), w))
            .collect();
        let mut builder = GetRequest::builder();
        for want in wants {
            builder = builder.offset(Manifest::child_offset(want.index), want.ranges());
        }
        let request = builder.build(content);
        let _guard = self.mark_fetching(content);
        let mut written: Vec<Range<u64>> = Vec::new();
        let mut done = HashSet::new();
        let transfer = async {
            let connected = fsm::start(connection.clone(), request, RequestCounters::default())
                .next()
                .await
                .map_err(|e| anyhow!("{e}"))?;
            let mut next = connected.next().await.map_err(|e| anyhow!("{e}"))?;
            loop {
                let child = match next {
                    ConnectedNext::StartChild(child) => child,
                    ConnectedNext::StartRoot(root) => {
                        // Never asked for; skip it.
                        let end = root.next().drain().await.map_err(|e| anyhow!("{e}"))?;
                        match end.next() {
                            EndBlobNext::MoreChildren(child) => child,
                            EndBlobNext::Closing(closing) => {
                                closing.next().await.map_err(|e| anyhow!("{e}"))?;
                                return Ok::<_, anyhow::Error>(());
                            }
                        }
                    }
                    ConnectedNext::Closing(closing) => {
                        closing.next().await.map_err(|e| anyhow!("{e}"))?;
                        return Ok(());
                    }
                };
                let offset = child.offset();
                let want = *by_offset
                    .get(&offset)
                    .ok_or_else(|| anyhow!("response child {offset} was not requested"))?;
                let chunk = *manifest
                    .chunks()
                    .get(want.index)
                    .ok_or_else(|| anyhow!("chunk {} out of range", want.index))?;
                let start = manifest.chunk_range(want.index).map_or(0, |r| r.start);
                let (mut blob, size) = child
                    .next(Hash::from(chunk.hash))
                    .next()
                    .await
                    .map_err(|e| anyhow!("{e}"))?;
                anyhow::ensure!(
                    size == u64::from(chunk.len),
                    "peer announced a {size} byte chunk for a {} byte one",
                    chunk.len
                );
                let end = loop {
                    match blob.next().await {
                        BlobContentNext::More((more, item)) => {
                            blob = more;
                            if let BaoContentItem::Leaf(leaf) = item.map_err(|e| anyhow!("{e}"))? {
                                let at = start.saturating_add(leaf.offset);
                                file.write_all_at(&leaf.data, at)?;
                                self.ranged_bytes
                                    .fetch_add(leaf.data.len() as u64, Ordering::Relaxed);
                                written.push(at..at.saturating_add(leaf.data.len() as u64));
                            }
                        }
                        BlobContentNext::Done(end) => break end,
                    }
                };
                done.insert(want.index);
                next = match end.next() {
                    EndBlobNext::MoreChildren(child) => ConnectedNext::StartChild(child),
                    EndBlobNext::Closing(closing) => ConnectedNext::Closing(closing),
                };
            }
        };
        let error = match tokio::time::timeout(REQUEST_TIMEOUT, transfer).await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(_) => Some(anyhow!("request for {content} timed out")),
        };
        if !written.is_empty() {
            let files = Arc::clone(&files);
            let mark = tokio::task::spawn_blocking(move || {
                files.mark_resident_in(&key, Some(generation), &written, false)
            })
            .await;
            if let Err(error) = mark
                .map_err(anyhow::Error::from)
                .and_then(|r| r.map_err(Into::into))
            {
                return (HashSet::new(), Some(error));
            }
        }
        (done, error)
    }
}

#[cfg(test)]
#[allow(
    clippy::single_range_in_vec_init,
    clippy::cast_possible_truncation,
    reason = "test fixtures: one range per request, small known sizes"
)]
mod tests {
    use super::*;

    use crate::content::{ChunkRef, ChunkTier};

    fn manifest(lens: &[u32]) -> Manifest {
        Manifest::new(
            ChunkTier::Small,
            lens.iter()
                .enumerate()
                .map(|(i, len)| ChunkRef {
                    hash: [u8::try_from(i).unwrap(); 32],
                    len: *len,
                })
                .collect(),
        )
    }

    #[test]
    fn wants_cover_the_range_in_whole_groups_per_chunk() {
        let m = manifest(&[100_000, 50_000, 70_000]);
        let wants = chunk_wants(&m, &[99_000..101_000]);
        assert_eq!(wants.len(), 2);
        assert_eq!(
            (wants[0].index, wants[0].within.clone()),
            (0, 98_304..100_000)
        );
        assert_eq!((wants[1].index, wants[1].within.clone()), (1, 0..16_384));
        // Two ranges in one chunk merge into one want.
        let wants = chunk_wants(&m, &[0..10, 60_000..60_010]);
        assert_eq!(wants.len(), 1);
        assert_eq!(wants[0].within, 0..65_536);
        let split = requests(chunk_wants(&m, &[0..220_000]), 120_000);
        assert_eq!(split.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn flights_hand_out_each_range_once() {
        let flights = Flights::default();
        let content = Hash::from([1; 32]);
        let (a, wait) = flights.claim(content, &[0..100]);
        assert_eq!(a, vec![0..100]);
        assert!(wait.is_none());
        let (b, wait) = flights.claim(content, &[50..150]);
        assert_eq!(b, vec![100..150]);
        assert!(wait.is_some(), "50..100 is someone else's");
        flights.release(content, &a);
        let (c, _) = flights.claim(content, &[0..150]);
        assert_eq!(c, vec![0..100]);
    }

    /// A reader waiting on another's fetch is woken when that fetch gives
    /// its ranges back, even when it fails at once (no peer answers) and
    /// gives them back before the waiter got round to waiting: nothing
    /// became resident, so the waiter must wake to fetch them itself.
    #[tokio::test]
    async fn a_fetch_released_before_its_waiter_waits_still_wakes_it() {
        let flights = Flights::default();
        let content = Hash::from([1; 32]);
        let (mine, _) = flights.claim(content, &[0..100]);
        let (theirs, wait) = flights.claim(content, &[0..100]);
        assert!(theirs.is_empty());
        let released = wait.expect("0..100 is someone else's");
        flights.release(content, &mine);
        tilo_test_env::within("the waiter wakes to fetch what was given back", released).await;
    }

    use crate::test_bytes::pseudo;

    /// A peer serving `data` as one file version, and an empty client.
    async fn pair(
        data: &[u8],
    ) -> (
        Hydrator,
        Hash,
        Arc<crate::store::ContentStore>,
        iroh::protocol::Router,
    ) {
        use iroh::endpoint::{Endpoint, presets};
        let server_store = Arc::new(crate::store::ContentStore::memory());
        let id = server_store.write_content(data).await.unwrap().unwrap();
        let server = Endpoint::bind(presets::N0).await.unwrap();
        let router = iroh::protocol::Router::builder(server.clone())
            .accept(iroh_blobs::ALPN, server_store.blobs_protocol(None))
            .spawn();
        let client = Endpoint::bind(presets::N0).await.unwrap();
        let hydrator = Hydrator::new(client, Arc::new(crate::store::ContentStore::memory()));
        hydrator.add_peer(server.addr());
        (hydrator, id, server_store, router)
    }

    #[tokio::test]
    async fn ranges_of_a_large_file_arrive_verified_and_resume() {
        let data = pseudo(6 * 1024 * 1024 + 123, 7);
        let (hydrator, id, server_store, _router) = pair(&data).await;
        assert!(content::is_file_backed(
            &server_store.manifest(id).await.unwrap()
        ));
        // A range in the middle, across chunk boundaries.
        let got = hydrator.read(id, 3_000_000, 1_000_000).await.unwrap();
        assert_eq!(got, &data[3_000_000..4_000_000]);
        let files = hydrator.store.files();
        let entry = files.entry(id.as_bytes()).unwrap();
        assert!(!entry.complete(), "only what was read is resident");
        assert!(entry.resident >= 1_000_000);
        assert!(entry.resident < 2_000_000, "{}", entry.resident);
        // The rest follows, asking only for what is missing.
        let all = hydrator.read(id, 0, data.len() as u64).await.unwrap();
        assert_eq!(all, data);
        assert!(files.entry(id.as_bytes()).unwrap().complete());
        // Chunks are now served from here too.
        let m = hydrator.manifest_index(id).await.unwrap();
        assert!(hydrator.store.has(Hash::from(m.chunks()[1].hash)).await);
    }

    #[tokio::test]
    async fn a_new_version_fetches_only_the_chunks_that_changed() {
        let mut data = pseudo(8 * 1024 * 1024, 9);
        let (hydrator, first, server_store, _router) = pair(&data).await;
        hydrator.read(first, 0, data.len() as u64).await.unwrap();
        data[4_000_000..4_001_000].fill(0xAB);
        let second = server_store
            .write_content_after(&data, Some(first))
            .await
            .unwrap()
            .unwrap();
        let m = hydrator.manifest_index(second).await.unwrap();
        let old = hydrator.manifest_index(first).await.unwrap();
        let changed: u64 = m
            .chunks()
            .iter()
            .filter(|c| !old.chunks().contains(c))
            .map(|c| u64::from(c.len))
            .sum();
        let before = hydrator.fetched_range_bytes();
        let got = hydrator.read(second, 0, data.len() as u64).await.unwrap();
        assert_eq!(got, data);
        let fetched = hydrator.fetched_range_bytes() - before;
        assert!(
            changed > 0 && changed < 1024 * 1024,
            "{changed} bytes changed"
        );
        assert_eq!(fetched, changed, "only the changed chunks travel");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_overlapping_reads_all_see_the_right_bytes() {
        let data = Arc::new(pseudo(12 * 1024 * 1024 + 4321, 13));
        let (hydrator, id, _server_store, _router) = pair(&data).await;
        let hydrator = Arc::new(hydrator);
        let len = data.len() as u64;
        let mut readers = Vec::new();
        for task in 0..16_u64 {
            let (hydrator, data) = (Arc::clone(&hydrator), Arc::clone(&data));
            readers.push(tokio::spawn(async move {
                let mut x = task.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                for round in 0..12 {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    let start = x % len;
                    let size = (x >> 20) % (3 * 1024 * 1024) + 1;
                    let priority = match (task + round) % 3 {
                        0 => Priority::Foreground,
                        1 => Priority::ReadAhead,
                        _ => Priority::Predictive,
                    };
                    let got = hydrator.read_with(id, start, size, priority).await.unwrap();
                    let end = (start + size).min(len) as usize;
                    assert!(
                        got == data[start as usize..end],
                        "task {task} round {round}"
                    );
                }
            }));
        }
        for reader in readers {
            reader.await.unwrap();
        }
        let all = hydrator.read(id, 0, len).await.unwrap();
        assert!(all.as_slice() == data.as_slice(), "the whole file");
    }

    #[tokio::test]
    async fn a_corrupt_stored_chunk_is_never_served() {
        let data = pseudo(3 * 1024 * 1024, 11);
        let (hydrator, id, server_store, _router) = pair(&data).await;
        let m = server_store.manifest(id).await.unwrap();
        // Damage the server's copy behind the store's back.
        let path = server_store.files().path_of(id.as_bytes());
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.write_all_at(&[0; 64], 100).unwrap();
        let first = m.chunk_range(0).unwrap();
        assert!(
            hydrator
                .read(id, first.start, first.end - first.start)
                .await
                .is_err(),
            "the damaged chunk fails the request"
        );
        let second = m.chunk_range(1).unwrap();
        assert_eq!(
            hydrator
                .read(id, second.start, second.end - second.start)
                .await
                .unwrap(),
            &data[second.start as usize..second.end as usize],
            "intact chunks are still served"
        );
    }

    /// A fetch abandoned while a quiet peer holds its ranges (a predictive
    /// prefetch cancelled when the policy changes) gives them back, so the
    /// next reader asks for them itself instead of waiting on a fetch that
    /// no longer exists.
    #[tokio::test]
    async fn an_abandoned_fetch_gives_its_ranges_back() {
        use iroh::endpoint::{Endpoint, presets};
        let data = pseudo(3 * 1024 * 1024, 17);
        let source = crate::store::ContentStore::memory();
        let id = source.write_content(&data).await.unwrap().unwrap();
        let quiet = crate::hydration::test_peers::silent().await;
        let hydrator = Hydrator::new(
            Endpoint::bind(presets::N0).await.unwrap(),
            Arc::new(crate::store::ContentStore::memory()),
        );
        // The manifest is here; the bytes only the quiet peer holds.
        let manifest = source.local(id).await.unwrap();
        assert_eq!(hydrator.store.ingest(manifest).await.unwrap(), id);
        hydrator.add_peer(quiet.endpoint().addr());

        let abandoned = tokio::time::timeout(
            Duration::from_millis(500),
            hydrator.read_with(id, 0, 64 * 1024, Priority::Predictive),
        )
        .await;
        assert!(abandoned.is_err(), "a quiet peer serves nothing");
        assert_eq!(
            hydrator
                .flights
                .lock()
                .get(&id)
                .map(|(busy, _)| busy.clone()),
            None,
            "the abandoned fetch still claims ranges"
        );
    }

    #[tokio::test]
    async fn background_work_waits_for_interactive_requests() {
        let scheduler = Arc::new(Scheduler::default());
        let reader = scheduler.admit(Priority::Foreground).await;
        assert_eq!(scheduler.interactive_in_flight(), 1);
        let mut waiting = Box::pin(scheduler.admit(Priority::Backfill));
        assert!(
            waiting
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                .is_pending(),
            "backfill waits for the reader"
        );
        drop(reader);
        drop(tilo_test_env::within("the backfill after the reader drops", waiting).await);
    }
}
