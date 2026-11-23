//! Byte ranges of file-backed content, fetched straight into the content store's
//! area. A range asks the content source for exactly the chunks it overlaps; each
//! verified chunk is written at its offset in the entry's file and recorded
//! resident once durable, so a 10 GiB file costs no more memory than a 10 MiB one.
//!
//! A chunk this device already holds is copied locally instead of fetched, and
//! residency is persisted, so an interrupted transfer asks only for what is
//! missing. Concurrent readers of overlapping ranges wait for the fetch under way.
//! A blocked read and read-ahead are interactive and always sent at once; pinned,
//! predictive and backfill work waits while they are in flight.

use std::collections::HashMap;
use std::ops::Range;
use std::os::unix::fs::FileExt as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

use super::{Hash, Hydrator, Result, anyhow};
use crate::content::{self, Manifest, RangeSet};
use crate::store::Retention;
use tilo_lock::MutexRecover as _;

const REQUEST_BYTES: u64 = 16 * 1024 * 1024;
/// A range is asked for in whole 16 KiB groups, so a request never asks for less.
const GROUP: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    Backfill,
    Predictive,
    Pinned,
    ReadAhead,
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

#[derive(Default)]
pub(crate) struct Scheduler {
    interactive: AtomicUsize,
    idle: Notify,
}

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

    #[cfg(test)]
    fn interactive_in_flight(&self) -> usize {
        self.interactive.load(Ordering::Acquire)
    }
}

type Released = std::pin::Pin<Box<tokio::sync::futures::OwnedNotified>>;

#[derive(Default)]
pub(crate) struct Flights {
    ranges: std::sync::Mutex<HashMap<Hash, (RangeSet, Arc<Notify>)>>,
}

impl Flights {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Hash, (RangeSet, Arc<Notify>)>> {
        self.ranges.lock_recover()
    }

    /// Claims the parts of `wanted` nobody is fetching. Returns them, and, when
    /// others hold the rest, a wait for them to give ranges back. The wait is
    /// registered before the claim lets go of the lock, so a fetch that fails and
    /// releases before the caller waits still wakes it.
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

/// Ranges claimed from [`Flights`], given back when dropped: a fetch abandoned part
/// way never leaves them claimed for readers to wait on forever.
struct Claim<'a> {
    flights: &'a Flights,
    content: Hash,
    ranges: Vec<Range<u64>>,
}

impl Claim<'_> {
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

#[derive(Clone, Debug)]
struct ChunkWant {
    index: usize,
    /// Byte range within the chunk, aligned to BAO groups.
    within: Range<u64>,
}

/// The parts of the chunks of `manifest` that cover `ranges`, one entry per chunk,
/// aligned to whole BAO groups.
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

impl Hydrator {
    pub fn fetched_range_bytes(&self) -> u64 {
        self.ranged_bytes.load(Ordering::Relaxed)
    }

    /// Makes bytes `range` of file-backed `content` resident in the area, fetching
    /// what no local entry holds. Returns once every byte of the range is resident.
    ///
    /// # Errors
    ///
    /// Part of the range could not be fetched from the workspace, or the area cannot be
    /// written.
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
        let tracing = super::tracing_reads();
        let began = std::time::Instant::now();
        let entry = files.entry(&key);
        if entry.as_ref().is_none_or(|e| e.size != size) {
            let chunks = content::chunk_locations(manifest);
            let retention = priority.retention();
            let files = Arc::clone(&files);
            tokio::task::spawn_blocking(move || files.ensure(&key, size, retention, &chunks))
                .await??;
            if tracing {
                eprintln!(
                    "range of {content}: its area entry in {:?}",
                    began.elapsed()
                );
            }
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
        if tracing {
            eprintln!(
                "range of {content}: its pieces and budget in {:?}",
                began.elapsed()
            );
        }
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
        // What this call fetched itself: still missing afterwards, nobody served it. A
        // range another reader's failed fetch released is fetched here instead.
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
                    "{unserved} bytes of {content} were not handed over by the workspace"
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
            let result = self.fetch_claimed(content, manifest, &claim.ranges).await;
            let claimed = claim.release();
            drop(admission);
            result?;
            fetched.extend(claimed);
        }
    }

    async fn fetch_claimed(
        &self,
        content: Hash,
        manifest: &Manifest,
        claimed: &[Range<u64>],
    ) -> Result<()> {
        let wants = chunk_wants(manifest, claimed);
        let wants = self.fill_from_local(content, manifest, wants).await?;
        if wants.is_empty() {
            return Ok(());
        }
        let chunks = wants
            .iter()
            .filter_map(|want| manifest.chunks().get(want.index))
            .map(|chunk| Hash::from(chunk.hash))
            .collect::<std::collections::BTreeSet<_>>();
        let asked: u64 = wants
            .iter()
            .filter_map(|want| manifest.chunks().get(want.index))
            .map(|chunk| u64::from(chunk.len))
            .sum();
        let _guard = self.mark_fetching(content);
        self.fetch_from_source(chunks.into_iter().collect()).await;
        let rest = self.fill_from_local(content, manifest, wants).await?;
        if rest.is_empty() {
            self.ranged_bytes.fetch_add(asked, Ordering::Relaxed);
            return Ok(());
        }
        Err(anyhow!(
            "the workspace did not hand over {} chunks of {content}",
            rest.len()
        ))
    }

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
            if let Some(bytes) = self.store.local(Hash::from(chunk.hash)).await
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
}

#[cfg(test)]
#[allow(
    clippy::single_range_in_vec_init,
    clippy::cast_possible_truncation,
    reason = "test fixtures: one range per request, small known sizes"
)]
mod tests {
    use std::time::Duration;

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

    tilo_test_env::scenario! {
        fn ranged_hydration_wants_whole_groups_and_hands_each_range_out_once;

        phase wants_cover_the_range_in_whole_groups_per_chunk;
        {
            let m = manifest(&[100_000, 50_000, 70_000]);
            let wants = chunk_wants(&m, &[99_000..101_000]);
            assert_eq!(wants.len(), 2);
            assert_eq!(
                (wants[0].index, wants[0].within.clone()),
                (0, 98_304..100_000)
            );
            assert_eq!((wants[1].index, wants[1].within.clone()), (1, 0..16_384));
            let wants = chunk_wants(&m, &[0..10, 60_000..60_010]);
            assert_eq!(wants.len(), 1);
            assert_eq!(wants[0].within, 0..65_536);
        }

        phase flights_hand_out_each_range_once;
        {
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
    }

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

    async fn pair(data: &[u8]) -> (Hydrator, Hash, Arc<crate::store::ContentStore>, ()) {
        let server_store = Arc::new(crate::store::ContentStore::memory());
        let id = server_store.write_content(data).await.unwrap().unwrap();
        let hydrator = Hydrator::new(Arc::new(crate::store::ContentStore::memory())).with_source(
            Arc::new(crate::hydration::StoreSource(Arc::clone(&server_store))),
        );
        (hydrator, id, server_store, ())
    }

    /// A workspace that is asked and never answers.
    struct Silent;

    impl crate::hydration::ContentSource for Silent {
        fn fetch(
            &self,
            _hash: Hash,
        ) -> futures_util::future::BoxFuture<'static, Option<bytes::Bytes>> {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test]
    async fn a_photo_sized_file_arrives_from_the_workspace_chunk_by_chunk() {
        // A quarter of a megabyte is a photo: too big for the single chunk a
        // test file is, small enough to stay loose blobs rather than the
        // area. Its manifest and several chunks travel, and the first chunk
        // alone must arrive for the open that reads its head.
        let data = pseudo(256 * 1024, 21);
        let (hydrator, id, server_store, _router) = pair(&data).await;
        let manifest = server_store.manifest(id).await.unwrap();
        assert!(!content::is_file_backed(&manifest));
        assert!(manifest.chunks().len() > 1, "several chunks");
        let head = hydrator.read(id, 0, 4096).await.unwrap();
        assert_eq!(head, &data[..4096]);
        let all = hydrator.read(id, 0, data.len() as u64).await.unwrap();
        assert_eq!(all, data);
    }

    #[tokio::test]
    async fn ranges_of_a_large_file_arrive_verified_and_resume() {
        let data = pseudo(6 * 1024 * 1024 + 123, 7);
        let (hydrator, id, server_store, _router) = pair(&data).await;
        assert!(content::is_file_backed(
            &server_store.manifest(id).await.unwrap()
        ));
        let got = hydrator.read(id, 3_000_000, 1_000_000).await.unwrap();
        assert_eq!(got, &data[3_000_000..4_000_000]);
        let files = hydrator.store.files();
        let entry = files.entry(id.as_bytes()).unwrap();
        assert!(!entry.complete(), "only what was read is resident");
        assert!(entry.resident >= 1_000_000);
        assert!(entry.resident < 2_000_000, "{}", entry.resident);
        let all = hydrator.read(id, 0, data.len() as u64).await.unwrap();
        assert_eq!(all, data);
        assert!(files.entry(id.as_bytes()).unwrap().complete());
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

    #[tokio::test]
    async fn an_abandoned_fetch_gives_its_ranges_back() {
        let data = pseudo(3 * 1024 * 1024, 17);
        let source = crate::store::ContentStore::memory();
        let id = source.write_content(&data).await.unwrap().unwrap();
        let hydrator = Hydrator::new(Arc::new(crate::store::ContentStore::memory()))
            .with_source(Arc::new(Silent));
        let manifest = source.local(id).await.unwrap();
        assert_eq!(hydrator.store.ingest(manifest).await.unwrap(), id);

        let abandoned = tokio::time::timeout(
            Duration::from_millis(500),
            hydrator.read_with(id, 0, 64 * 1024, Priority::Predictive),
        )
        .await;
        assert!(abandoned.is_err(), "a quiet workspace hands over nothing");
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
