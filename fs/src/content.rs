//! Content-defined chunking + manifests.
//!
//! A file's bytes are split into content-defined chunks (a gear-hash rolling
//! boundary, so an edit only shifts the boundaries *near* the edit). Each chunk
//! is a content-addressed blob; the file is described by a [`Manifest`] (an
//! ordered list of chunk hashes + lengths) which is itself stored as a blob. A
//! file node's content hash points at its manifest.
//!
//! Win: editing part of a large file re-stores and re-transfers only the chunks
//! that actually changed; peers hydrate only the chunks they're missing.

use serde::{Deserialize, Serialize};

const MIN_CHUNK: usize = 2 * 1024;
const MAX_CHUNK: usize = 64 * 1024;
/// ~8 KiB average chunk size (13 one-bits in the mask).
const MASK: u64 = (1 << 13) - 1;

/// A reference to one content-addressed chunk.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkRef {
    pub hash: [u8; 32],
    pub len: u32,
}

/// The chunk list describing a file's content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub chunks: Vec<ChunkRef>,
}

impl Manifest {
    pub fn total_len(&self) -> u64 {
        self.chunks.iter().map(|c| c.len as u64).sum()
    }
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("serialize manifest")
    }
    pub fn decode(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(postcard::from_bytes(bytes)?)
    }
}

/// A fixed gear table derived from a fixed seed (deterministic across peers —
/// chunk boundaries must be identical everywhere for dedup to work).
fn gear_table() -> [u64; 256] {
    let mut t = [0u64; 256];
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    for slot in t.iter_mut() {
        // xorshift64
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *slot = x;
    }
    t
}

/// Split `data` into content-defined chunks. Deterministic; empty input → no
/// chunks.
pub fn split(data: &[u8]) -> Vec<&[u8]> {
    let table = gear_table();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut hash: u64 = 0;
    for i in 0..data.len() {
        hash = (hash << 1).wrapping_add(table[data[i] as usize]);
        let len = i - start + 1;
        if (len >= MIN_CHUNK && (hash & MASK) == 0) || len >= MAX_CHUNK {
            chunks.push(&data[start..=i]);
            start = i + 1;
            hash = 0;
        }
    }
    if start < data.len() {
        chunks.push(&data[start..]);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pseudo(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x & 0xff) as u8
            })
            .collect()
    }

    #[test]
    fn split_is_deterministic_and_reassembles() {
        let data = pseudo(200_000, 42);
        let a = split(&data);
        let b = split(&data);
        assert_eq!(a.len(), b.len(), "chunking is deterministic");
        let reassembled: Vec<u8> = a.iter().flat_map(|c| c.iter().copied()).collect();
        assert_eq!(reassembled, data, "chunks reassemble to the original");
        assert!(a.len() > 1, "a 200KB file should split into multiple chunks");
    }

    #[test]
    fn chunks_respect_size_bounds() {
        let data = pseudo(300_000, 7);
        let chunks = split(&data);
        for (i, c) in chunks.iter().enumerate() {
            assert!(c.len() <= MAX_CHUNK, "chunk over max");
            // every chunk except the last must be >= MIN (the last is a remainder)
            if i + 1 < chunks.len() {
                assert!(c.len() >= MIN_CHUNK, "interior chunk under min");
            }
        }
    }

    #[test]
    fn edit_only_changes_local_chunks() {
        // Content-defined chunking: editing one byte mid-file must leave most
        // chunk hashes identical (that's the dedup property).
        let data = pseudo(200_000, 99);
        let before: Vec<Vec<u8>> = split(&data).iter().map(|c| c.to_vec()).collect();

        let mut edited = data.clone();
        edited[100_000] ^= 0xff; // flip a byte in the middle
        let after: Vec<Vec<u8>> = split(&edited).iter().map(|c| c.to_vec()).collect();

        use std::collections::HashSet;
        let before_set: HashSet<&Vec<u8>> = before.iter().collect();
        let shared = after.iter().filter(|c| before_set.contains(c)).count();
        // The vast majority of chunks must be unchanged.
        assert!(
            shared as f64 > before.len() as f64 * 0.8,
            "expected most chunks unchanged, only {shared}/{} shared",
            before.len()
        );
    }

    #[test]
    fn manifest_roundtrips() {
        let m = Manifest {
            chunks: vec![
                ChunkRef { hash: [1; 32], len: 10 },
                ChunkRef { hash: [2; 32], len: 20 },
            ],
        };
        assert_eq!(m.total_len(), 30);
        let back = Manifest::decode(&m.encode()).unwrap();
        assert_eq!(back, m);
    }
}
