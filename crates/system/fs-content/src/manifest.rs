//! Manifests: which chunks make up one file version, and their encoding.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::tier::ChunkTier;

/// First bytes of a manifest's header entry.
pub const HEADER_MAGIC: [u8; 6] = *b"tilocf";
/// The manifest format this crate writes.
pub const FORMAT: u8 = 2;
/// Largest chunk any tier produces.
pub const MAX_CHUNK_LEN: u32 = 4 * 1024 * 1024;
const ENTRY: usize = 32;
const LENGTHS_PER_ENTRY: usize = ENTRY / 4;

/// One chunk: its BLAKE3 hash and length.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ChunkRef {
    pub hash: [u8; 32],
    pub len: u32,
}

/// The chunks of one file version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    tier: ChunkTier,
    chunks: Vec<ChunkRef>,
    /// End offset of every chunk.
    ends: Vec<u64>,
    id: [u8; 32],
}

/// Why bytes are not a manifest.
#[derive(Debug)]
pub enum DecodeError {
    Malformed(&'static str),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "malformed manifest: {why}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Whether `bytes` carry a manifest header. Cheap: looks at the first entry
/// only.
pub fn is_manifest(bytes: &[u8]) -> bool {
    bytes.len().is_multiple_of(ENTRY)
        && bytes.get(..HEADER_MAGIC.len()) == Some(&HEADER_MAGIC[..])
        && bytes.get(HEADER_MAGIC.len()) == Some(&FORMAT)
}

fn ends_of(chunks: &[ChunkRef]) -> Vec<u64> {
    let mut end = 0_u64;
    chunks
        .iter()
        .map(|c| {
            end = end.saturating_add(u64::from(c.len));
            end
        })
        .collect()
}

fn read_u64(bytes: &[u8], at: usize) -> Option<u64> {
    let raw = bytes.get(at..at.checked_add(8)?)?;
    Some(u64::from_le_bytes(raw.try_into().ok()?))
}

impl Manifest {
    /// A manifest of `chunks`, chunked with `tier`.
    pub fn new(tier: ChunkTier, chunks: Vec<ChunkRef>) -> Self {
        let ends = ends_of(&chunks);
        let mut m = Self {
            tier,
            chunks,
            ends,
            id: [0; 32],
        };
        m.id = *blake3::hash(&m.encode()).as_bytes();
        m
    }

    /// The size tier.
    pub const fn tier(&self) -> ChunkTier {
        self.tier
    }

    pub fn chunks(&self) -> &[ChunkRef] {
        &self.chunks
    }

    pub fn total_len(&self) -> u64 {
        self.ends.last().copied().unwrap_or(0)
    }

    /// The content id: BLAKE3 of [`Self::encode`].
    pub const fn id(&self) -> [u8; 32] {
        self.id
    }

    /// Byte range of chunk `index` in the file.
    pub fn chunk_range(&self, index: usize) -> Option<Range<u64>> {
        let end = *self.ends.get(index)?;
        let start = index
            .checked_sub(1)
            .and_then(|previous| self.ends.get(previous))
            .copied()
            .unwrap_or(0);
        Some(start..end)
    }

    /// Index of the chunk holding byte `offset`.
    pub fn chunk_at(&self, offset: u64) -> Option<usize> {
        let index = self.ends.partition_point(|end| *end <= offset);
        (index < self.chunks.len()).then_some(index)
    }

    /// Every chunk overlapping `range`: its index, start offset and entry.
    pub fn overlapping(
        &self,
        range: Range<u64>,
    ) -> impl Iterator<Item = (usize, u64, &ChunkRef)> + '_ {
        let first = self.ends.partition_point(|end| *end <= range.start);
        self.chunks
            .iter()
            .enumerate()
            .skip(first)
            .map(move |(index, chunk)| {
                let start = index
                    .checked_sub(1)
                    .and_then(|previous| self.ends.get(previous))
                    .copied()
                    .unwrap_or(0);
                (index, start, chunk)
            })
            .take_while(move |(_, start, _)| *start < range.end)
    }

    /// The position of chunk `index` in a `RangeSpecSeq` for a `GET` of the
    /// content id: 0 is the manifest itself, 1 the header entry.
    pub fn child_offset(index: usize) -> u64 {
        (index as u64).saturating_add(2)
    }

    /// Chunk index for a `RangeSpecSeq` position, if it names a chunk.
    pub fn chunk_of_child(&self, offset: u64) -> Option<usize> {
        let index = usize::try_from(offset.checked_sub(2)?).ok()?;
        (index < self.chunks.len()).then_some(index)
    }

    /// The manifest's bytes.
    pub fn encode(&self) -> Vec<u8> {
        let n = self.chunks.len();
        let length_entries = n.div_ceil(LENGTHS_PER_ENTRY);
        let entries = n.saturating_add(length_entries).saturating_add(1);
        let mut out = Vec::with_capacity(entries.saturating_mul(ENTRY));
        out.extend_from_slice(&HEADER_MAGIC);
        out.push(FORMAT);
        out.push(self.tier.code());
        out.extend_from_slice(&self.total_len().to_le_bytes());
        out.extend_from_slice(&(n as u64).to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        for chunk in &self.chunks {
            out.extend_from_slice(&chunk.hash);
        }
        for chunk in &self.chunks {
            out.extend_from_slice(&chunk.len.to_le_bytes());
        }
        out.resize(entries.saturating_mul(ENTRY), 0);
        out
    }

    /// Decodes a manifest; the content id is BLAKE3 of `bytes`.
    ///
    /// # Errors
    ///
    /// Returns an error when `bytes` are not a well-formed manifest.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        use DecodeError::Malformed;
        if !is_manifest(bytes) {
            return Err(Malformed("no manifest header"));
        }
        let header = bytes.get(..ENTRY).ok_or(Malformed("no header"))?;
        let tier = header
            .get(7)
            .copied()
            .and_then(ChunkTier::from_code)
            .ok_or(Malformed("unknown tier"))?;
        let total_len = read_u64(header, 8).ok_or(Malformed("short header"))?;
        let count = read_u64(header, 16).ok_or(Malformed("short header"))?;
        if read_u64(header, 24) != Some(0) {
            return Err(Malformed("reserved header bytes are set"));
        }
        let count = usize::try_from(count).map_err(|_| Malformed("chunk count"))?;
        let entries = bytes.len() / ENTRY;
        let expected = count
            .checked_add(count.div_ceil(LENGTHS_PER_ENTRY))
            .and_then(|n| n.checked_add(1))
            .ok_or(Malformed("chunk count"))?;
        if entries != expected {
            return Err(Malformed("entry count does not match the chunk count"));
        }
        let hashes_end = count
            .checked_add(1)
            .and_then(|n| n.checked_mul(ENTRY))
            .ok_or(Malformed("chunk count"))?;
        let hashes = bytes.get(ENTRY..hashes_end).ok_or(Malformed("short"))?;
        let lengths = bytes.get(hashes_end..).ok_or(Malformed("short"))?;
        let mut chunks = Vec::with_capacity(count);
        let mut sum = 0_u64;
        for (hash, len) in hashes.chunks_exact(ENTRY).zip(lengths.chunks_exact(4)) {
            let hash: [u8; 32] = hash.try_into().map_err(|_| Malformed("hash"))?;
            let len = u32::from_le_bytes(len.try_into().map_err(|_| Malformed("length"))?);
            if len == 0 || len > MAX_CHUNK_LEN {
                return Err(Malformed("chunk length out of range"));
            }
            sum = sum.saturating_add(u64::from(len));
            chunks.push(ChunkRef { hash, len });
        }
        if chunks.len() != count {
            return Err(Malformed("short chunk list"));
        }
        let padding = lengths.get(count.saturating_mul(4)..).unwrap_or_default();
        if padding.iter().any(|b| *b != 0) {
            return Err(Malformed("length padding is set"));
        }
        if sum != total_len {
            return Err(Malformed("chunk lengths do not add up to the total"));
        }
        let ends = ends_of(&chunks);
        Ok(Self {
            tier,
            chunks,
            ends,
            id: *blake3::hash(bytes).as_bytes(),
        })
    }

    /// Checks that `data` is exactly the content this manifest describes.
    pub fn verify(&self, data: &[u8]) -> bool {
        if data.len() as u64 != self.total_len() {
            return false;
        }
        self.chunks.iter().enumerate().all(|(index, chunk)| {
            self.chunk_range(index)
                .and_then(|r| {
                    data.get(usize::try_from(r.start).ok()?..usize::try_from(r.end).ok()?)
                })
                .is_some_and(|bytes| blake3::hash(bytes).as_bytes() == &chunk.hash)
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "test fixtures use small, known sizes"
)]
mod tests {
    use super::*;

    fn chunk(n: u8, len: u32) -> ChunkRef {
        ChunkRef { hash: [n; 32], len }
    }

    #[test]
    fn round_trips_and_is_recognised() {
        for count in [1_usize, 7, 8, 9, 100] {
            let chunks: Vec<ChunkRef> = (0..count)
                .map(|i| chunk(u8::try_from(i % 250).unwrap() + 1, 1000 + i as u32))
                .collect();
            let m = Manifest::new(ChunkTier::Small, chunks);
            let bytes = m.encode();
            assert!(is_manifest(&bytes));
            assert_eq!(bytes.len() % 32, 0);
            let back = Manifest::decode(&bytes).unwrap();
            assert_eq!(back, m);
            assert_eq!(back.id(), *blake3::hash(&bytes).as_bytes());
            assert_eq!(back.tier(), ChunkTier::Small);
        }
    }

    #[test]
    fn tampered_manifests_are_refused() {
        let m = Manifest::new(ChunkTier::Small, vec![chunk(1, 10), chunk(2, 20)]);
        let good = m.encode();
        let mut wrong_total = good.clone();
        wrong_total[8] ^= 1;
        assert!(Manifest::decode(&wrong_total).is_err());
        let mut padded = good.clone();
        let last = padded.len() - 1;
        padded[last] = 1;
        assert!(Manifest::decode(&padded).is_err());
        let mut zero_len = good.clone();
        zero_len[3 * 32..3 * 32 + 4].copy_from_slice(&0_u32.to_le_bytes());
        assert!(Manifest::decode(&zero_len).is_err());
        assert!(Manifest::decode(&good[..64]).is_err());
    }

    #[test]
    fn offsets_map_to_chunks() {
        let m = Manifest::new(
            ChunkTier::Small,
            vec![chunk(1, 10), chunk(2, 20), chunk(3, 5)],
        );
        assert_eq!(m.chunk_at(0), Some(0));
        assert_eq!(m.chunk_at(9), Some(0));
        assert_eq!(m.chunk_at(10), Some(1));
        assert_eq!(m.chunk_at(34), Some(2));
        assert_eq!(m.chunk_at(35), None);
        assert_eq!(m.chunk_range(1), Some(10..30));
        let hit: Vec<(usize, u64)> = m.overlapping(12..31).map(|(i, s, _)| (i, s)).collect();
        assert_eq!(hit, vec![(1, 10), (2, 30)]);
        assert_eq!(Manifest::child_offset(0), 2);
        assert_eq!(m.chunk_of_child(4), Some(2));
        assert_eq!(m.chunk_of_child(1), None);
        assert_eq!(m.chunk_of_child(5), None);
    }
}
