//! Content format: size-scaled `FastCDC` chunking, BLAKE3 chunk hashes and
//! `HashSeq`-shaped manifests, with incremental re-chunking of changed
//! ranges.
//!
//! Chunk targets scale with body size (`docs/architecture/storage.md`, §7.1):
//!
//! | Body size        | Representation | min / avg / max        |
//! |------------------|----------------|------------------------|
//! | ≤ 4 KiB          | inline         | –                      |
//! | ≤ 64 KiB         | one blob       | –                      |
//! | 64 KiB – 64 MiB  | manifest       | 16 / 64 / 256 KiB      |
//! | 64 MiB – 4 GiB   | manifest       | 64 / 256 KiB / 1 MiB   |
//! | > 4 GiB          | manifest       | 256 KiB / 1 / 4 MiB    |
//!
//! A record keeps its tier until its size moves past a boundary by 4×, so a
//! file that hovers at a boundary keeps its dedup.

use std::ops::Range;

use fastcdc::v2020::{FastCDC, Normalization};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

pub const INLINE_MAX: u64 = 4 * KIB;
pub const SINGLE_MAX: u64 = 64 * KIB;
const SMALL_MAX: u64 = 64 * MIB;
const MEDIUM_MAX: u64 = 4 * GIB;
/// How far past a tier boundary a record must move before it changes tier.
const TIER_HYSTERESIS: u64 = 4;

/// Manifest format version (`meta.format`).
pub const MANIFEST_FORMAT: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum ChunkTier {
    /// Bytes travel inside the record operation.
    Inline,
    /// One blob; the content id is the BLAKE3 hash of the bytes.
    Single,
    Small,
    Medium,
    Large,
}

impl ChunkTier {
    /// The tier a body of `len` bytes gets when it has no history.
    pub fn for_size(len: u64) -> Self {
        match len {
            l if l <= INLINE_MAX => Self::Inline,
            l if l <= SINGLE_MAX => Self::Single,
            l if l <= SMALL_MAX => Self::Small,
            l if l <= MEDIUM_MAX => Self::Medium,
            _ => Self::Large,
        }
    }

    /// Size range in which `self` is kept once a record has it.
    fn sticky_range(self) -> (u64, u64) {
        let (lo, hi) = match self {
            Self::Inline => (0, INLINE_MAX),
            Self::Single => (INLINE_MAX, SINGLE_MAX),
            Self::Small => (SINGLE_MAX, SMALL_MAX),
            Self::Medium => (SMALL_MAX, MEDIUM_MAX),
            Self::Large => (MEDIUM_MAX, u64::MAX),
        };
        (lo / TIER_HYSTERESIS, hi.saturating_mul(TIER_HYSTERESIS))
    }

    /// The tier for a record that had `previous` and now has `len` bytes.
    ///
    /// Inline and single-blob bodies are not chunked, so they always follow
    /// the size; the hysteresis only applies between chunked tiers, where a
    /// change of chunk targets would lose all dedup with the previous version.
    pub fn sticky(previous: Option<Self>, len: u64) -> Self {
        let fresh = Self::for_size(len);
        match previous {
            Some(prev) if prev >= Self::Small && fresh >= Self::Small => {
                let (lo, hi) = prev.sticky_range();
                if len > lo && len <= hi { prev } else { fresh }
            }
            _ => fresh,
        }
    }

    /// `FastCDC` `(min, avg, max)` chunk sizes, or `None` for unchunked tiers.
    pub fn cdc_params(self) -> Option<(u32, u32, u32)> {
        match self {
            Self::Inline | Self::Single => None,
            Self::Small => Some((16 * 1024, 64 * 1024, 256 * 1024)),
            Self::Medium => Some((64 * 1024, 256 * 1024, 1024 * 1024)),
            Self::Large => Some((256 * 1024, 1024 * 1024, 4 * 1024 * 1024)),
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Inline => 0,
            Self::Single => 1,
            Self::Small => 2,
            Self::Medium => 3,
            Self::Large => 4,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct ChunkRef {
    pub offset: u64,
    pub len: u64,
    pub hash: blake3::Hash,
}

impl ChunkRef {
    pub fn end(&self) -> u64 {
        self.offset.saturating_add(self.len)
    }
}

/// The content description of one file version.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Manifest {
    pub tier: ChunkTier,
    pub total_len: u64,
    pub chunks: Vec<ChunkRef>,
    /// For ≤ 64 KiB bodies the BLAKE3 hash of the bytes; above that the
    /// hash of the `HashSeq[meta, c0, …, cN]` bytes.
    pub content_id: blake3::Hash,
    /// The bytes themselves for inline bodies.
    pub inline: Option<Vec<u8>>,
}

/// Work done to produce a manifest.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ChunkStats {
    pub chunks_hashed: u64,
    pub chunks_reused: u64,
    pub bytes_hashed: u64,
}

impl ChunkStats {
    pub fn add(&mut self, other: Self) {
        self.chunks_hashed = self.chunks_hashed.saturating_add(other.chunks_hashed);
        self.chunks_reused = self.chunks_reused.saturating_add(other.chunks_reused);
        self.bytes_hashed = self.bytes_hashed.saturating_add(other.bytes_hashed);
    }
}

fn push_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = u8::try_from(v & 0x7f).unwrap_or(0);
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

impl Manifest {
    /// The manifest metadata blob: format, total length, tier and the chunk
    /// lengths as LEB128 varints.
    pub fn meta_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.chunks.len().saturating_mul(3).saturating_add(16));
        out.push(MANIFEST_FORMAT);
        out.extend_from_slice(&self.total_len.to_le_bytes());
        out.push(self.tier.code());
        for c in &self.chunks {
            push_varint(&mut out, c.len);
        }
        out
    }

    /// The `HashSeq[meta, c0, …, cN]` bytes whose hash is the content id of
    /// a chunked body.
    pub fn hash_seq(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.chunks.len().saturating_add(1).saturating_mul(32));
        out.extend_from_slice(blake3::hash(&self.meta_bytes()).as_bytes());
        for c in &self.chunks {
            out.extend_from_slice(c.hash.as_bytes());
        }
        out
    }

    fn finish(tier: ChunkTier, data: &[u8], chunks: Vec<ChunkRef>) -> Self {
        let total_len = data.len() as u64;
        let mut m = Self {
            tier,
            total_len,
            chunks,
            content_id: blake3::Hash::from_bytes([0; 32]),
            inline: None,
        };
        m.content_id = match tier {
            ChunkTier::Inline | ChunkTier::Single => m
                .chunks
                .first()
                .map_or_else(|| blake3::hash(&[]), |c| c.hash),
            _ => blake3::hash(&m.hash_seq()),
        };
        if tier == ChunkTier::Inline {
            m.inline = Some(data.to_vec());
        }
        m
    }

    /// Chunks and hashes `data` from scratch.
    pub fn build(data: &[u8], previous_tier: Option<ChunkTier>) -> (Self, ChunkStats) {
        Self::rebuild(None, data, &[], previous_tier)
    }

    /// Produces the manifest of `data`, re-hashing only what `changed` can
    /// have affected.
    ///
    /// `changed` lists the byte ranges of `data` that may differ from the
    /// bytes `previous` describes (for example from an extent comparison of
    /// two snapshots). Every chunk of `previous` that starts at a boundary
    /// of the new chunking and lies wholly in unchanged bytes is reused
    /// without reading it. Because a `FastCDC` cut depends only on the bytes
    /// from the chunk start, the result is identical to chunking `data` from
    /// scratch.
    pub fn rebuild(
        previous: Option<&Self>,
        data: &[u8],
        changed: &[Range<u64>],
        previous_tier: Option<ChunkTier>,
    ) -> (Self, ChunkStats) {
        let len = data.len() as u64;
        let tier = ChunkTier::sticky(previous.map(|p| p.tier).or(previous_tier), len);
        Self::rebuild_in(tier, previous, data, changed)
    }

    /// Chunks `data` with the parameters of `tier`, regardless of its size.
    /// For measuring tiers against each other; records use [`Self::build`].
    pub fn build_in_tier(data: &[u8], tier: ChunkTier) -> (Self, ChunkStats) {
        Self::rebuild_in(tier, None, data, &[])
    }

    fn rebuild_in(
        tier: ChunkTier,
        previous: Option<&Self>,
        data: &[u8],
        changed: &[Range<u64>],
    ) -> (Self, ChunkStats) {
        let len = data.len() as u64;
        let mut stats = ChunkStats::default();
        let Some((min, avg, max)) = tier.cdc_params() else {
            stats.chunks_hashed = 1;
            stats.bytes_hashed = len;
            let chunk = ChunkRef {
                offset: 0,
                len,
                hash: blake3::hash(data),
            };
            return (Self::finish(tier, data, vec![chunk]), stats);
        };
        let reusable = previous.filter(|p| p.tier == tier);
        let mut changed: Vec<Range<u64>> = changed.to_vec();
        changed.sort_by_key(|r| r.start);
        let cdc = FastCDC::with_level(data, min, avg, max, Normalization::Level1);
        let mut chunks = Vec::with_capacity(
            usize::try_from(len.checked_div(u64::from(avg)).unwrap_or(0))
                .unwrap_or(0)
                .saturating_add(1),
        );
        let mut pos: u64 = 0;
        while pos < len {
            if let Some(old) = reusable.and_then(|p| reusable_chunk(p, pos, len, &changed)) {
                stats.chunks_reused = stats.chunks_reused.saturating_add(1);
                pos = old.end();
                chunks.push(old);
                continue;
            }
            let start = usize::try_from(pos).unwrap_or(usize::MAX);
            let remaining = data.len().saturating_sub(start);
            let (_, cut) = cdc.cut(start, remaining);
            let bytes = data.get(start..cut).unwrap_or_default();
            if bytes.is_empty() {
                break;
            }
            let chunk_len = bytes.len() as u64;
            chunks.push(ChunkRef {
                offset: pos,
                len: chunk_len,
                hash: blake3::hash(bytes),
            });
            stats.chunks_hashed = stats.chunks_hashed.saturating_add(1);
            stats.bytes_hashed = stats.bytes_hashed.saturating_add(chunk_len);
            pos = pos.saturating_add(chunk_len);
        }
        (Self::finish(tier, data, chunks), stats)
    }

    /// Index of the chunk containing byte `offset`.
    pub fn chunk_at(&self, offset: u64) -> Option<&ChunkRef> {
        let idx = self.chunks.partition_point(|c| c.end() <= offset);
        self.chunks.get(idx).filter(|c| c.offset <= offset)
    }
}

/// The chunk of `previous` starting at `pos`, if it can be reused verbatim
/// for a body of `new_len` bytes whose `changed` ranges are known.
fn reusable_chunk(
    previous: &Manifest,
    pos: u64,
    new_len: u64,
    changed: &[Range<u64>],
) -> Option<ChunkRef> {
    let idx = previous.chunks.partition_point(|c| c.offset < pos);
    let old = *previous.chunks.get(idx)?;
    if old.offset != pos || old.end() > new_len {
        return None;
    }
    // A chunk that ended at the old end of file was cut by EOF, not by the
    // content; with a different length the cut can move.
    if old.end() == previous.total_len && new_len != previous.total_len {
        return None;
    }
    let first = changed.partition_point(|r| r.end <= old.offset);
    let touched = changed
        .get(first..)
        .unwrap_or_default()
        .iter()
        .take_while(|r| r.start < old.end())
        .any(|r| !r.is_empty() && r.end > old.offset);
    (!touched).then_some(old)
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::range_plus_one,
    reason = "test fixtures use small, known sizes"
)]
mod tests {
    use super::*;

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
    fn tiers_follow_the_size_table() {
        assert_eq!(ChunkTier::for_size(0), ChunkTier::Inline);
        assert_eq!(ChunkTier::for_size(4096), ChunkTier::Inline);
        assert_eq!(ChunkTier::for_size(4097), ChunkTier::Single);
        assert_eq!(ChunkTier::for_size(64 * KIB), ChunkTier::Single);
        assert_eq!(ChunkTier::for_size(64 * KIB + 1), ChunkTier::Small);
        assert_eq!(ChunkTier::for_size(64 * MIB + 1), ChunkTier::Medium);
        assert_eq!(ChunkTier::for_size(5 * GIB), ChunkTier::Large);
    }

    #[test]
    fn chunked_tiers_stick_until_the_size_moves_four_times_past_the_boundary() {
        let s = Some(ChunkTier::Small);
        assert_eq!(ChunkTier::sticky(s, 65 * MIB), ChunkTier::Small);
        assert_eq!(ChunkTier::sticky(s, 256 * MIB), ChunkTier::Small);
        assert_eq!(ChunkTier::sticky(s, 257 * MIB), ChunkTier::Medium);
        let m = Some(ChunkTier::Medium);
        assert_eq!(ChunkTier::sticky(m, 63 * MIB), ChunkTier::Medium);
        assert_eq!(ChunkTier::sticky(m, 15 * MIB), ChunkTier::Small);
        // Unchunked tiers always follow the size.
        assert_eq!(ChunkTier::sticky(s, 1000), ChunkTier::Inline);
    }

    #[test]
    fn small_bodies_are_identified_by_the_hash_of_their_bytes() {
        let data = pseudo_random(3000, 1);
        let (m, _) = Manifest::build(&data, None);
        assert_eq!(m.tier, ChunkTier::Inline);
        assert_eq!(m.content_id, blake3::hash(&data));
        assert_eq!(m.inline.as_deref(), Some(&data[..]));
        let data = pseudo_random(60_000, 2);
        let (m, _) = Manifest::build(&data, None);
        assert_eq!(m.tier, ChunkTier::Single);
        assert_eq!(m.content_id, blake3::hash(&data));
        assert!(m.inline.is_none());
    }

    #[test]
    fn chunks_cover_the_body_within_tier_bounds() {
        let data = pseudo_random(3 * 1024 * 1024 + 17, 3);
        let (m, stats) = Manifest::build(&data, None);
        assert_eq!(m.tier, ChunkTier::Small);
        let mut pos = 0;
        for (i, c) in m.chunks.iter().enumerate() {
            assert_eq!(c.offset, pos);
            assert!(c.len <= 256 * KIB);
            if i + 1 < m.chunks.len() {
                assert!(c.len >= 16 * KIB);
            }
            let (s, e) = (c.offset as usize, c.end() as usize);
            assert_eq!(c.hash, blake3::hash(&data[s..e]));
            pos = c.end();
        }
        assert_eq!(pos, data.len() as u64);
        assert_eq!(stats.bytes_hashed, data.len() as u64);
        let avg = data.len() as u64 / m.chunks.len() as u64;
        assert!((32 * KIB..=128 * KIB).contains(&avg), "average chunk {avg}");
        assert_eq!(m.content_id, blake3::hash(&m.hash_seq()));
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
        let (old, _) = Manifest::build(&data, None);
        let r = edit(&mut data, 5 * 1024 * 1024 + 123, &[0xAA; 4096]);
        let (inc, stats) = Manifest::rebuild(Some(&old), &data, &[r], None);
        let (full, _) = Manifest::build(&data, None);
        assert_eq!(inc, full);
        assert!(
            stats.chunks_hashed <= 3,
            "rehashed {} chunks",
            stats.chunks_hashed
        );
        assert!(stats.bytes_hashed <= 3 * 256 * KIB);
        assert_eq!(
            stats.chunks_hashed + stats.chunks_reused,
            full.chunks.len() as u64
        );
    }

    #[test]
    fn incremental_rebuild_equals_full_rebuild_for_random_edits() {
        let mut seed = 99u64;
        for round in 0..24u64 {
            let base_len = 200_000 + (round as usize * 37_111) % 2_000_000;
            let mut data = pseudo_random(base_len, round + 10);
            let (old, _) = Manifest::build(&data, None);
            let mut ranges = Vec::new();
            for _ in 0..(1 + round % 4) {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let at = (seed >> 20) as usize % (data.len() + 1000);
                let n = 1 + (seed >> 40) as usize % 70_000;
                ranges.push(edit(&mut data, at, &pseudo_random(n, seed)));
            }
            if round % 3 == 0 {
                // Truncate: everything past the new end is gone.
                let new_len = data.len() * 3 / 4;
                data.truncate(new_len);
                ranges.retain(|r| r.start < new_len as u64);
            }
            let (inc, _) = Manifest::rebuild(Some(&old), &data, &ranges, None);
            let (full, _) = Manifest::build(&data, Some(old.tier));
            assert_eq!(inc, full, "round {round}");
        }
    }

    #[test]
    fn appending_rehashes_only_the_tail() {
        let mut data = pseudo_random(4 * 1024 * 1024, 5);
        let (old, _) = Manifest::build(&data, None);
        let at = data.len();
        let r = edit(&mut data, at, &pseudo_random(100_000, 6));
        let (inc, stats) = Manifest::rebuild(Some(&old), &data, &[r], None);
        assert_eq!(inc, Manifest::build(&data, None).0);
        assert!(stats.chunks_reused + 2 >= old.chunks.len() as u64);
    }

    #[test]
    fn chunk_at_maps_offsets_to_chunks() {
        let data = pseudo_random(2 * 1024 * 1024, 7);
        let (m, _) = Manifest::build(&data, None);
        for c in &m.chunks {
            assert_eq!(m.chunk_at(c.offset), Some(c));
            assert_eq!(m.chunk_at(c.end() - 1), Some(c));
        }
        assert!(m.chunk_at(data.len() as u64).is_none());
    }
}
