//! The chunker of the legacy format: a byte-at-a-time gear hash with about
//! 8 KiB chunks. Kept so tests can build content the way older daemons
//! stored it and prove it stays readable; nothing new is written this way.

use crate::manifest::{ChunkRef, Manifest};

const MIN_CHUNK: usize = 2 * 1024;
const MAX_CHUNK: usize = 64 * 1024;
/// ~8 KiB average chunk size (13 one-bits in the mask).
const MASK: u64 = (1 << 13) - 1;

/// A fixed gear table derived from a fixed seed.
fn gear_table() -> [u64; 256] {
    let mut t = [0u64; 256];
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    for slot in &mut t {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *slot = x;
    }
    t
}

/// Splits `data` the way the legacy format did. Empty input has no chunks.
pub fn split(data: &[u8]) -> Vec<&[u8]> {
    let table = gear_table();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut hash: u64 = 0;
    for (i, &byte) in data.iter().enumerate() {
        let gear = table.get(usize::from(byte)).copied().unwrap_or(0);
        hash = (hash << 1).wrapping_add(gear);
        let len = i.saturating_sub(start).saturating_add(1);
        if (len >= MIN_CHUNK && (hash & MASK) == 0) || len >= MAX_CHUNK {
            let Some(chunk) = data.get(start..=i) else {
                break;
            };
            chunks.push(chunk);
            start = i.saturating_add(1);
            hash = 0;
        }
    }
    if start < data.len()
        && let Some(rest) = data.get(start..)
    {
        chunks.push(rest);
    }
    chunks
}

/// The legacy manifest of `data` and its chunks' bytes.
pub fn manifest(data: &[u8]) -> (Manifest, Vec<&[u8]>) {
    let chunks = split(data);
    let refs = chunks
        .iter()
        .map(|c| ChunkRef {
            hash: *blake3::hash(c).as_bytes(),
            len: u32::try_from(c.len()).unwrap_or(u32::MAX),
        })
        .collect();
    (Manifest::legacy(refs), chunks)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixtures")]
mod tests {
    use super::*;

    fn pseudo(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x.to_le_bytes()[0]
            })
            .collect()
    }

    #[test]
    fn split_is_deterministic_and_reassembles() {
        let data = pseudo(200_000, 42);
        let a = split(&data);
        assert_eq!(a.len(), split(&data).len());
        let reassembled: Vec<u8> = a.iter().flat_map(|c| c.iter().copied()).collect();
        assert_eq!(reassembled, data);
        assert!(a.len() > 1);
        let (m, _) = manifest(&data);
        assert!(m.verify(&data));
    }
}
