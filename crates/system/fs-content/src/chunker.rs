//! Chunking and hashing: in memory, over a mapped file with the ranges an
//! edit can have changed, or streamed from a reader in bounded memory.
//!
//! Cut points are found sequentially (a `FastCDC` cut depends only on the
//! bytes from the chunk's start); the chunks are then hashed on several
//! threads, since BLAKE3 over large chunks is what costs.

use std::io::{self, Read};
use std::ops::Range;

use fastcdc::v2020::{self, MASKS, Normalization};

use crate::manifest::{ChunkRef, Manifest};
use crate::tier::ChunkTier;

/// Bytes read per step when chunking a stream; memory stays at about this
/// plus one maximal chunk.
const STREAM_BLOCK: usize = 16 * 1024 * 1024;
/// Below this many bytes to hash, one thread does it.
const PARALLEL_MIN: usize = 4 * 1024 * 1024;
/// Below this many bytes, cut points are found on one thread.
const PARALLEL_CUT_MIN: usize = 32 * 1024 * 1024;
const MAX_HASH_THREADS: usize = 16;

/// Work done to produce a manifest.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ChunkStats {
    pub chunks_hashed: u64,
    pub chunks_reused: u64,
    pub bytes_hashed: u64,
}

impl ChunkStats {
    pub const fn add(&mut self, other: Self) {
        self.chunks_hashed = self.chunks_hashed.saturating_add(other.chunks_hashed);
        self.chunks_reused = self.chunks_reused.saturating_add(other.chunks_reused);
        self.bytes_hashed = self.bytes_hashed.saturating_add(other.bytes_hashed);
    }
}

/// Normalised (level 1) `FastCDC` cut points for one tier.
#[derive(Clone, Copy, Debug)]
pub struct Cutter {
    min: usize,
    avg: usize,
    max: usize,
    mask_s: u64,
    mask_l: u64,
}

impl Cutter {
    /// The cutter of a chunked tier; `None` for tiers that are one chunk.
    pub fn for_tier(tier: ChunkTier) -> Option<Self> {
        let (min, avg, max) = tier.cdc_params()?;
        let bits = v2020::logarithm2(avg);
        let normalization = Normalization::Level1.bits();
        let mask_s = *MASKS.get(usize::try_from(bits.checked_add(normalization)?).ok()?)?;
        let mask_l = *MASKS.get(usize::try_from(bits.checked_sub(normalization)?).ok()?)?;
        Some(Self {
            min: usize::try_from(min).ok()?,
            avg: usize::try_from(avg).ok()?,
            max: usize::try_from(max).ok()?,
            mask_s,
            mask_l,
        })
    }

    /// The largest chunk this cutter produces.
    pub const fn max(&self) -> usize {
        self.max
    }

    /// Length of the chunk starting at `data[0]`. `data` must hold at least
    /// [`Self::max`] bytes unless it ends at the end of the body.
    pub fn cut(&self, data: &[u8]) -> usize {
        let window = data.get(..self.max.min(data.len())).unwrap_or(data);
        let (_, len) = v2020::cut(
            window,
            self.min,
            self.avg,
            self.max,
            self.mask_s,
            self.mask_l,
            self.mask_s << 1,
            self.mask_l << 1,
        );
        len.max(1).min(window.len())
    }
}

/// Cut points of `data` from `start` until the first cut at or past
/// `until`, as chunk end offsets.
fn cuts_from(data: &[u8], cutter: &Cutter, start: usize, until: usize) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut pos = start;
    while pos < data.len() {
        let len = cutter.cut(data.get(pos..).unwrap_or_default());
        pos = pos.saturating_add(len);
        ends.push(pos);
        if pos >= until {
            break;
        }
    }
    ends
}

/// Every chunk end offset of `data`, found on several threads for a large
/// body.
///
/// Each thread chunks its own segment as if a chunk started at the
/// segment's first byte. A cut depends only on the bytes from its chunk's
/// start, so once the true sequence (continued from the previous segment)
/// reaches a cut the segment's thread also found, the two sequences agree
/// from there on; the segment's remaining cuts are taken as they are. That
/// usually happens within a few chunks.
fn chunk_ends(data: &[u8], cutter: &Cutter) -> Vec<usize> {
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(MAX_HASH_THREADS);
    if data.len() < PARALLEL_CUT_MIN || threads <= 1 {
        return cuts_from(data, cutter, 0, data.len());
    }
    let segment = data.len().div_ceil(threads);
    let starts: Vec<usize> = (0..threads)
        .map(|k| k.saturating_mul(segment))
        .filter(|s| *s < data.len())
        .collect();
    let segments: Vec<Vec<usize>> = std::thread::scope(|scope| {
        #[allow(
            clippy::needless_collect,
            reason = "every worker must be spawned before the first is joined"
        )]
        let workers: Vec<_> = starts
            .iter()
            .map(|&start| {
                let until = start.saturating_add(segment);
                scope.spawn(move || cuts_from(data, cutter, start, until))
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().unwrap_or_default())
            .collect()
    });
    let mut ends: Vec<usize> = Vec::new();
    for (k, found) in segments.into_iter().enumerate() {
        if k == 0 {
            ends = found;
            continue;
        }
        // Continue the true sequence until it meets a cut of this segment.
        let mut pos = ends.last().copied().unwrap_or(0);
        let mut joined = None;
        while pos < data.len() {
            if let Ok(at) = found.binary_search(&pos) {
                joined = Some(at);
                break;
            }
            if found.last().is_some_and(|last| pos > *last) {
                break;
            }
            let len = cutter.cut(data.get(pos..).unwrap_or_default());
            pos = pos.saturating_add(len);
            ends.push(pos);
        }
        if let Some(at) = joined {
            ends.extend(found.get(at.saturating_add(1)..).unwrap_or_default());
        }
    }
    // The last segment's worker stopped at its first cut past the end of
    // its segment, which is the end of the body; make sure the sequence
    // reaches it even when the segments never joined.
    let mut pos = ends.last().copied().unwrap_or(0);
    while pos < data.len() {
        let len = cutter.cut(data.get(pos..).unwrap_or_default());
        pos = pos.saturating_add(len);
        ends.push(pos);
    }
    ends
}

fn span(data: &[u8], start: usize, len: usize) -> &[u8] {
    data.get(start..start.saturating_add(len))
        .unwrap_or_default()
}

/// BLAKE3 of every `(start, len)` span of `data`, on several threads when
/// there is enough to hash.
fn hash_spans(data: &[u8], spans: &[(usize, usize)]) -> Vec<[u8; 32]> {
    let total: usize = spans.iter().map(|(_, len)| *len).sum();
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(MAX_HASH_THREADS)
        .min(spans.len());
    if total < PARALLEL_MIN || threads <= 1 {
        return spans
            .iter()
            .map(|(start, len)| *blake3::hash(span(data, *start, *len)).as_bytes())
            .collect();
    }
    // Contiguous groups of about equal bytes, one per thread.
    let target = total.div_ceil(threads);
    let mut groups: Vec<&[(usize, usize)]> = Vec::with_capacity(threads);
    let mut begin = 0;
    let mut bytes = 0_usize;
    for (i, (_, len)) in spans.iter().enumerate() {
        bytes = bytes.saturating_add(*len);
        if bytes >= target {
            groups.push(spans.get(begin..=i).unwrap_or_default());
            begin = i.saturating_add(1);
            bytes = 0;
        }
    }
    if begin < spans.len() {
        groups.push(spans.get(begin..).unwrap_or_default());
    }
    #[allow(
        clippy::needless_collect,
        reason = "every worker must be spawned before the first is joined"
    )]
    std::thread::scope(|scope| {
        let workers: Vec<_> = groups
            .into_iter()
            .map(|group| {
                scope.spawn(move || {
                    group
                        .iter()
                        .map(|(start, len)| *blake3::hash(span(data, *start, *len)).as_bytes())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_default())
            .collect()
    })
}

fn to_len(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// Chunks and hashes `data` from scratch.
pub fn build(data: &[u8], previous_tier: Option<ChunkTier>) -> (Manifest, ChunkStats) {
    rebuild(None, data, &[], previous_tier)
}

/// Chunks `data` with the parameters of `tier`, regardless of its size.
/// For measuring tiers against each other; records use [`build`].
pub fn build_in_tier(data: &[u8], tier: ChunkTier) -> (Manifest, ChunkStats) {
    rebuild_in(tier, None, data, &[])
}

/// The manifest of `data`, re-hashing only what `changed` can have
/// affected.
///
/// `changed` lists the byte ranges of `data` that may differ from the bytes
/// `previous` describes (for example from an extent comparison of two
/// snapshots). Every chunk of `previous` that starts at a cut point of the
/// new chunking and lies wholly in unchanged bytes is reused without
/// reading it. Because a cut depends only on the bytes from the chunk's
/// start, the result equals chunking `data` from scratch.
pub fn rebuild(
    previous: Option<&Manifest>,
    data: &[u8],
    changed: &[Range<u64>],
    previous_tier: Option<ChunkTier>,
) -> (Manifest, ChunkStats) {
    let tier = ChunkTier::sticky(
        previous.map(Manifest::tier).or(previous_tier),
        data.len() as u64,
    );
    rebuild_in(tier, previous, data, changed)
}

enum Piece {
    Reused(ChunkRef),
    /// A chunk of this length, hashed below.
    New(usize),
}

fn rebuild_in(
    tier: ChunkTier,
    previous: Option<&Manifest>,
    data: &[u8],
    changed: &[Range<u64>],
) -> (Manifest, ChunkStats) {
    let mut stats = ChunkStats::default();
    let Some(cutter) = Cutter::for_tier(tier) else {
        let chunks = if data.is_empty() {
            Vec::new()
        } else {
            stats.chunks_hashed = 1;
            stats.bytes_hashed = data.len() as u64;
            vec![ChunkRef {
                hash: *blake3::hash(data).as_bytes(),
                len: to_len(data.len()),
            }]
        };
        return (Manifest::new(tier, chunks), stats);
    };
    let reusable = previous.filter(|p| p.tier() == tier);
    // The overlap lookup binary-searches range ends, so sorting starts alone
    // is insufficient when dirty ranges overlap or nest.
    let mut dirty = crate::ranges::RangeSet::new();
    for range in changed {
        dirty.insert(range.clone());
    }
    let changed: Vec<Range<u64>> = dirty.iter().cloned().collect();
    let mut pieces = Vec::new();
    let mut spans = Vec::new();
    let mut pos = 0_usize;
    if reusable.is_none() {
        for end in chunk_ends(data, &cutter) {
            let len = end.saturating_sub(pos);
            pieces.push(Piece::New(len));
            spans.push((pos, len));
            stats.chunks_hashed = stats.chunks_hashed.saturating_add(1);
            stats.bytes_hashed = stats.bytes_hashed.saturating_add(len as u64);
            pos = end;
        }
    }
    while pos < data.len() {
        if let Some((old, len)) =
            reusable.and_then(|p| reusable_chunk(p, pos as u64, data.len() as u64, &changed))
        {
            stats.chunks_reused = stats.chunks_reused.saturating_add(1);
            pieces.push(Piece::Reused(old));
            pos = pos.saturating_add(len);
            continue;
        }
        let rest = data.get(pos..).unwrap_or_default();
        let len = cutter.cut(rest);
        pieces.push(Piece::New(len));
        spans.push((pos, len));
        stats.chunks_hashed = stats.chunks_hashed.saturating_add(1);
        stats.bytes_hashed = stats.bytes_hashed.saturating_add(len as u64);
        pos = pos.saturating_add(len);
    }
    let mut hashes = hash_spans(data, &spans).into_iter();
    let chunks = pieces
        .into_iter()
        .map(|piece| match piece {
            Piece::Reused(chunk) => chunk,
            Piece::New(len) => ChunkRef {
                hash: hashes.next().unwrap_or_default(),
                len: to_len(len),
            },
        })
        .collect();
    (Manifest::new(tier, chunks), stats)
}

/// The chunk of `previous` starting at `pos`, with its length, if it can be
/// reused verbatim for a body of `new_len` bytes whose `changed` ranges are
/// known.
fn reusable_chunk(
    previous: &Manifest,
    pos: u64,
    new_len: u64,
    changed: &[Range<u64>],
) -> Option<(ChunkRef, usize)> {
    let index = previous.chunk_at(pos)?;
    let range = previous.chunk_range(index)?;
    if range.start != pos || range.end > new_len {
        return None;
    }
    // A chunk that ended at the old end of file was cut by the end, not by
    // the content; with a different length the cut can move.
    if range.end == previous.total_len() && new_len != previous.total_len() {
        return None;
    }
    let first = changed.partition_point(|r| r.end <= range.start);
    let touched = changed
        .get(first..)
        .unwrap_or_default()
        .iter()
        // FastCDC tests the byte at the cut offset before returning that
        // offset, so the first byte of the next chunk can move this cut.
        .take_while(|r| r.start <= range.end)
        .any(|r| !r.is_empty() && r.end > range.start);
    let chunk = *previous.chunks().get(index)?;
    (!touched).then(|| {
        (
            chunk,
            usize::try_from(range.end.saturating_sub(range.start)).unwrap_or(0),
        )
    })
}

/// Chunks `len` bytes read from `reader` in bounded memory. The result
/// equals [`build`] over the same bytes.
///
/// # Errors
///
/// Returns the reader's error, or `UnexpectedEof`/`InvalidData` when the
/// reader yields fewer or more than `len` bytes (the source changed while it
/// was read).
pub fn build_reader(
    mut reader: impl Read,
    len: u64,
    previous_tier: Option<ChunkTier>,
) -> io::Result<(Manifest, ChunkStats)> {
    let tier = ChunkTier::sticky(previous_tier, len);
    let mut stats = ChunkStats::default();
    let Some(cutter) = Cutter::for_tier(tier) else {
        // One chunk of at most 64 KiB.
        let mut data = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        reader
            .by_ref()
            .take(len.saturating_add(1))
            .read_to_end(&mut data)?;
        check_len(data.len() as u64, len)?;
        return Ok(build_in_tier(&data, tier));
    };
    let capacity = STREAM_BLOCK.saturating_add(cutter.max());
    let mut buf = vec![0_u8; capacity];
    let mut filled = 0_usize;
    let mut total = 0_u64;
    let mut eof = false;
    let mut chunks = Vec::new();
    loop {
        while !eof && filled < capacity {
            let n = match reader.read(buf.get_mut(filled..).unwrap_or_default()) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if n == 0 {
                eof = true;
            }
            filled = filled.saturating_add(n);
            total = total.saturating_add(n as u64);
            if total > len {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "source grew while read",
                ));
            }
        }
        let mut spans = Vec::new();
        let mut start = 0_usize;
        while start < filled {
            let remaining = filled.saturating_sub(start);
            if remaining < cutter.max() && !eof {
                break;
            }
            let n = cutter.cut(buf.get(start..filled).unwrap_or_default());
            spans.push((start, n));
            start = start.saturating_add(n);
        }
        for ((_, n), hash) in spans.iter().zip(hash_spans(&buf, &spans)) {
            chunks.push(ChunkRef {
                hash,
                len: to_len(*n),
            });
            stats.chunks_hashed = stats.chunks_hashed.saturating_add(1);
            stats.bytes_hashed = stats.bytes_hashed.saturating_add(*n as u64);
        }
        if eof && start >= filled {
            break;
        }
        buf.copy_within(start..filled, 0);
        filled = filled.saturating_sub(start);
    }
    check_len(total, len)?;
    Ok((Manifest::new(tier, chunks), stats))
}

fn check_len(got: u64, want: u64) -> io::Result<()> {
    match got.cmp(&want) {
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Less => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "source shrank while read",
        )),
        std::cmp::Ordering::Greater => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source grew while read",
        )),
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::range_plus_one,
    reason = "test fixtures use small, known sizes"
)]
mod tests {
    use super::*;

    const KIB: u64 = 1024;

    fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x.to_le_bytes()[0]
            })
            .collect()
    }

    #[test]
    fn rebuild_rehashes_nested_dirty_ranges() {
        let mut data = pseudo_random(512 * 1024, 7);
        let (previous, _) = build(&data, None);
        let offset = previous.chunk_range(2).unwrap().start as usize;
        data[offset] ^= 1;
        let changed = vec![0..data.len() as u64, 1..2];
        let (rebuilt, _) = rebuild(Some(&previous), &data, &changed, None);
        let (fresh, _) = build(&data, None);
        assert_eq!(rebuilt, fresh);
        assert!(rebuilt.verify(&data));
    }

    #[test]
    fn edits_at_chunk_boundaries_recompute_the_preceding_cut() {
        let mut data = pseudo_random(512 * 1024, 7);
        let (previous, _) = build(&data, None);
        let offset = previous.chunk_range(2).unwrap().start as usize;
        data[offset] ^= 1;
        let changed = offset as u64..offset as u64 + 1;
        let (rebuilt, _) = rebuild(Some(&previous), &data, std::slice::from_ref(&changed), None);
        let (fresh, _) = build(&data, None);
        assert!(rebuilt.verify(&data));
        assert_eq!(rebuilt, fresh);
    }

    #[test]
    fn streamed_chunking_retries_interrupted_reads() {
        struct InterruptedReader<'a> {
            remaining: &'a [u8],
            interrupt: bool,
        }
        impl Read for InterruptedReader<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.interrupt = !self.interrupt;
                if self.interrupt {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let limit = buf.len().min(4096);
                self.remaining.read(&mut buf[..limit])
            }
        }

        let data = pseudo_random(512 * 1024, 9);
        let reader = InterruptedReader {
            remaining: &data,
            interrupt: false,
        };
        let (streamed, _) = build_reader(reader, data.len() as u64, None).unwrap();
        let (fresh, _) = build(&data, None);
        assert_eq!(streamed, fresh);
        assert!(streamed.verify(&data));
    }

    #[test]
    fn cutter_matches_the_fastcdc_iterator() {
        let data = pseudo_random(3 * 1024 * 1024, 11);
        for tier in [ChunkTier::Small, ChunkTier::Medium] {
            let (min, avg, max) = tier.cdc_params().unwrap();
            let expected: Vec<usize> =
                v2020::FastCDC::with_level(&data, min, avg, max, Normalization::Level1)
                    .map(|c| c.length)
                    .collect();
            let cutter = Cutter::for_tier(tier).unwrap();
            let mut got = Vec::new();
            let mut pos = 0;
            while pos < data.len() {
                let n = cutter.cut(&data[pos..]);
                got.push(n);
                pos += n;
            }
            assert_eq!(got, expected, "{tier:?}");
        }
    }

    #[test]
    fn chunks_cover_the_body_within_tier_bounds() {
        let data = pseudo_random(3 * 1024 * 1024 + 17, 3);
        let (m, stats) = build(&data, None);
        assert_eq!(m.tier(), ChunkTier::Small);
        for (i, c) in m.chunks().iter().enumerate() {
            let r = m.chunk_range(i).unwrap();
            assert!(u64::from(c.len) <= 256 * KIB);
            if i + 1 < m.chunks().len() {
                assert!(u64::from(c.len) >= 16 * KIB);
            }
            assert_eq!(
                &c.hash,
                blake3::hash(&data[r.start as usize..r.end as usize]).as_bytes()
            );
        }
        assert_eq!(m.total_len(), data.len() as u64);
        assert_eq!(stats.bytes_hashed, data.len() as u64);
        assert!(m.verify(&data));
    }

    #[test]
    fn small_bodies_are_one_chunk() {
        let data = pseudo_random(60_000, 2);
        let (m, _) = build(&data, None);
        assert_eq!(m.tier(), ChunkTier::Single);
        assert_eq!(m.chunks().len(), 1);
        assert_eq!(&m.chunks()[0].hash, blake3::hash(&data).as_bytes());
        let (empty, _) = build(&[], None);
        assert!(empty.chunks().is_empty());
    }

    fn edit(data: &mut Vec<u8>, at: usize, bytes: &[u8]) -> Range<u64> {
        let end = at + bytes.len();
        if end > data.len() {
            data.resize(end, 0);
        }
        data[at..end].copy_from_slice(bytes);
        at as u64..end as u64
    }

    #[test]
    fn in_place_edit_rehashes_about_one_chunk_and_matches_a_full_rebuild() {
        let mut data = pseudo_random(8 * 1024 * 1024, 4);
        let (old, _) = build(&data, None);
        let r = edit(&mut data, 5 * 1024 * 1024 + 123, &[0xAA; 4096]);
        let (inc, stats) = rebuild(Some(&old), &data, &[r], None);
        let (full, _) = build(&data, None);
        assert_eq!(inc, full);
        assert!(stats.chunks_hashed <= 3, "rehashed {}", stats.chunks_hashed);
        assert_eq!(
            stats.chunks_hashed + stats.chunks_reused,
            full.chunks().len() as u64
        );
    }

    #[test]
    fn incremental_rebuild_equals_full_rebuild_for_random_edits() {
        let mut seed = 99u64;
        for round in 0..24u64 {
            let base_len = 200_000 + (round as usize * 37_111) % 2_000_000;
            let mut data = pseudo_random(base_len, round + 10);
            let (old, _) = build(&data, None);
            let mut ranges = Vec::new();
            for _ in 0..(1 + round % 4) {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let at = (seed >> 20) as usize % (data.len() + 1000);
                let n = 1 + (seed >> 40) as usize % 70_000;
                ranges.push(edit(&mut data, at, &pseudo_random(n, seed)));
            }
            if round % 3 == 0 {
                let new_len = data.len() * 3 / 4;
                data.truncate(new_len);
                ranges.retain(|r| r.start < new_len as u64);
            }
            let (inc, _) = rebuild(Some(&old), &data, &ranges, None);
            let (full, _) = build(&data, Some(old.tier()));
            assert_eq!(inc, full, "round {round}");
        }
    }

    #[test]
    fn streaming_equals_in_memory_chunking() {
        for (len, tier) in [
            (0, None),
            (3000, None),
            (60_000, None),
            (5 * 1024 * 1024 + 3, None),
            (40 * 1024 * 1024 + 99, Some(ChunkTier::Medium)),
        ] {
            let data = pseudo_random(len, len as u64 + 1);
            let (expected, _) = build(&data, tier);
            let (got, _) = build_reader(&data[..], len as u64, tier).unwrap();
            assert_eq!(got, expected, "{len}");
        }
        let data = pseudo_random(1000, 1);
        assert!(build_reader(&data[..], 1001, None).is_err());
        assert!(build_reader(&data[..], 999, None).is_err());
    }

    #[test]
    fn parallel_cut_points_equal_sequential_ones() {
        let cutter = Cutter::for_tier(ChunkTier::Medium).unwrap();
        for (len, seed) in [(40 * 1024 * 1024 + 7, 21), (96 * 1024 * 1024, 22)] {
            let data = pseudo_random(len, seed);
            assert_eq!(
                chunk_ends(&data, &cutter),
                cuts_from(&data, &cutter, 0, data.len()),
                "{len}"
            );
        }
        // Pathological data: every cut is at the maximum, so segments that
        // start off the maximum's grid never join; the result still matches.
        let zeros = vec![0_u8; 40 * 1024 * 1024 + 12_345];
        assert_eq!(
            chunk_ends(&zeros, &cutter),
            cuts_from(&zeros, &cutter, 0, zeros.len())
        );
    }

    #[test]
    fn a_large_body_hashes_in_parallel_to_the_same_result() {
        let data = pseudo_random(24 * 1024 * 1024, 5);
        let (m, _) = build(&data, None);
        let sequential: Vec<[u8; 32]> = m
            .chunks()
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let r = m.chunk_range(i).unwrap();
                *blake3::hash(&data[r.start as usize..r.end as usize]).as_bytes()
            })
            .collect();
        let got: Vec<[u8; 32]> = m.chunks().iter().map(|c| c.hash).collect();
        assert_eq!(got, sequential);
    }
}
