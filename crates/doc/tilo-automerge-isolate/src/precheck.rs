//! Cheap structural checks run in the calling process before any Automerge
//! code sees untrusted bytes.
//!
//! Automerge storage is a sequence of chunks, each framed as
//! `magic(4) checksum(4) type(1) uleb128-length body`. Walking the frames
//! reads only those headers, so it costs nothing proportional to what the
//! body would decode to. It refuses input that is too large, holds too many
//! chunks, uses an unknown chunk type, declares a body longer than the
//! input, or carries bytes after its last chunk. What a body decodes to is
//! bounded by the worker, not here.

use crate::{Limits, Rejected};

const MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];
const KNOWN_CHUNK_TYPES: std::ops::RangeInclusive<u8> = 0..=3;
#[cfg(target_os = "linux")]
pub(crate) const DOCUMENT: u8 = 0;
pub(crate) const CHANGE: u8 = 1;
pub(crate) const COMPRESSED_CHANGE: u8 = 2;

/// One framed chunk: its type and its bytes, header included.
#[derive(Clone, Copy)]
pub(crate) struct Chunk<'a> {
    pub(crate) kind: u8,
    pub(crate) bytes: &'a [u8],
}

pub(crate) fn chunks(mut bytes: &[u8]) -> impl Iterator<Item = Chunk<'_>> {
    std::iter::from_fn(move || {
        let (_, after) = bytes.split_first_chunk::<8>()?;
        let (kind, after) = after.split_first()?;
        let (length, after) = uleb128(after).ok()?;
        let body = usize::try_from(length).ok()?;
        let header = bytes.len().checked_sub(after.len())?;
        let (chunk, rest) = bytes.split_at_checked(header.checked_add(body)?)?;
        bytes = rest;
        Some(Chunk {
            kind: *kind,
            bytes: chunk,
        })
    })
}

/// Check the framing of `bytes` against `limits`.
///
/// # Errors
///
/// [`Rejected::TooLarge`] past the byte or chunk ceiling,
/// [`Rejected::Malformed`] for framing Automerge would not load.
pub fn check(bytes: &[u8], limits: &Limits) -> Result<(), Rejected> {
    if bytes.len() > limits.max_input_bytes {
        return Err(Rejected::TooLarge);
    }
    let mut rest = bytes;
    let mut chunks = 0usize;
    while !rest.is_empty() {
        chunks = chunks.saturating_add(1);
        if chunks > limits.max_chunks {
            return Err(Rejected::TooLarge);
        }
        let Some((magic, after)) = rest.split_first_chunk::<4>() else {
            return Err(Rejected::Malformed("truncated chunk header"));
        };
        if *magic != MAGIC {
            return Err(Rejected::Malformed("not an automerge chunk"));
        }
        let Some((_checksum, after)) = after.split_first_chunk::<4>() else {
            return Err(Rejected::Malformed("truncated chunk header"));
        };
        let Some((kind, after)) = after.split_first() else {
            return Err(Rejected::Malformed("truncated chunk header"));
        };
        if !KNOWN_CHUNK_TYPES.contains(kind) {
            return Err(Rejected::Malformed("unknown chunk type"));
        }
        let (length, after) = uleb128(after)?;
        let length =
            usize::try_from(length).map_err(|_| Rejected::Malformed("chunk longer than input"))?;
        rest = after
            .get(length..)
            .ok_or(Rejected::Malformed("chunk longer than input"))?;
    }
    Ok(())
}

/// One unsigned LEB128 value of at most 64 bits, and the bytes after it.
fn uleb128(bytes: &[u8]) -> Result<(u64, &[u8]), Rejected> {
    let mut value = 0u64;
    for (index, byte) in bytes.iter().enumerate().take(10) {
        let shift = u32::try_from(index)
            .ok()
            .and_then(|i| i.checked_mul(7))
            .ok_or(Rejected::Malformed("chunk length overflows"))?;
        let part = u64::from(byte & 0x7f);
        if shift == 63 && part > 1 {
            return Err(Rejected::Malformed("chunk length overflows"));
        }
        value |= part << shift;
        if byte & 0x80 == 0 {
            let rest = bytes
                .get(index.saturating_add(1)..)
                .ok_or(Rejected::Malformed("truncated chunk length"))?;
            return Ok((value, rest));
        }
    }
    Err(Rejected::Malformed("truncated chunk length"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT};

    fn saved_with_end() -> (Vec<u8>, usize) {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "k", "v").unwrap();
        doc.commit();
        let mut bytes = doc.save();
        let end = bytes.len();
        doc.put(ROOT, "k", "w").unwrap();
        doc.commit();
        bytes.extend(doc.save_incremental());
        assert!(bytes.len() > end);
        (bytes, end)
    }

    fn saved() -> Vec<u8> {
        saved_with_end().0
    }

    #[test]
    fn real_storage_passes_and_every_truncation_is_refused_or_still_framed() {
        let limits = Limits::default();
        let (bytes, first_chunk_end) = saved_with_end();
        check(&bytes, &limits).unwrap();
        check(&[], &limits).unwrap();
        for cut in 1..bytes.len() {
            let outcome = check(bytes.get(..cut).unwrap(), &limits);
            if cut == first_chunk_end {
                assert!(outcome.is_ok(), "a whole chunk is well framed");
            } else {
                assert!(outcome.is_err(), "cut at {cut} passed");
            }
        }
    }

    /// Framing faults, and the framing nearest to each that is sound: what
    /// the input is, its bytes, and what the check says of it.
    fn framing_cases() -> Vec<(&'static str, Vec<u8>, Result<(), Rejected>)> {
        // A change chunk's header, to which a case adds its length and body.
        let header = |length: &[u8]| {
            let mut bytes = MAGIC.to_vec();
            bytes.extend([0; 4]);
            bytes.push(1);
            bytes.extend(length);
            bytes
        };
        // Real storage whose first chunk says it is of another type.
        let of_type = |kind: u8| {
            let mut bytes = saved();
            *bytes.get_mut(8).unwrap() = kind;
            bytes
        };
        let mut trailing = saved();
        trailing.push(0);
        let mut overlong = vec![0xff; 9];
        overlong.push(0x02);
        let mut all_bits = vec![0xff; 9];
        all_bits.push(0x01);
        vec![
            (
                "a trailing byte",
                trailing,
                Err(Rejected::Malformed("truncated chunk header")),
            ),
            (
                "an unknown chunk type",
                of_type(9),
                Err(Rejected::Malformed("unknown chunk type")),
            ),
            (
                "the lowest unknown chunk type",
                of_type(4),
                Err(Rejected::Malformed("unknown chunk type")),
            ),
            ("the highest known chunk type", of_type(3), Ok(())),
            (
                "bytes that are no chunk",
                vec![0; 16],
                Err(Rejected::Malformed("not an automerge chunk")),
            ),
            (
                "a length that does not fit in 64 bits",
                header(&overlong),
                Err(Rejected::Malformed("chunk length overflows")),
            ),
            (
                "a length of all 64 bits",
                header(&all_bits),
                Err(Rejected::Malformed("chunk longer than input")),
            ),
            (
                "a length that never ends",
                header(&[0x80; 10]),
                Err(Rejected::Malformed("truncated chunk length")),
            ),
            (
                "a body one byte short",
                header(&[1]),
                Err(Rejected::Malformed("chunk longer than input")),
            ),
            ("an empty body", header(&[0]), Ok(())),
        ]
    }

    #[test]
    fn refuses_malformed_framing_and_input_past_a_ceiling_but_not_at_it() {
        let limits = Limits::default();
        for (case, bytes, expected) in framing_cases() {
            assert_eq!(check(&bytes, &limits), expected, "{case}");
        }

        // Two chunks: the ceilings refuse one byte or one chunk past them and
        // admit input that is exactly at them.
        let bytes = saved();
        for (case, max_input_bytes, max_chunks, expected) in [
            ("a 4 byte ceiling", 4, usize::MAX, Err(Rejected::TooLarge)),
            (
                "one byte past the byte ceiling",
                bytes.len().saturating_sub(1),
                usize::MAX,
                Err(Rejected::TooLarge),
            ),
            ("exactly the byte ceiling", bytes.len(), usize::MAX, Ok(())),
            (
                "one chunk past the chunk ceiling",
                usize::MAX,
                1,
                Err(Rejected::TooLarge),
            ),
            ("exactly the chunk ceiling", usize::MAX, 2, Ok(())),
            (
                "a chunk ceiling of none",
                usize::MAX,
                0,
                Err(Rejected::TooLarge),
            ),
        ] {
            let limits = Limits {
                max_input_bytes,
                max_chunks,
                ..Limits::default()
            };
            assert_eq!(check(&bytes, &limits), expected, "{case}");
        }

        // Ceilings of nothing admit the empty input, which holds no byte and
        // no chunk, and refuse one byte.
        let nothing = Limits {
            max_input_bytes: 0,
            max_chunks: 0,
            ..Limits::default()
        };
        assert_eq!(check(&[], &nothing), Ok(()), "nothing, under no ceiling");
        assert_eq!(
            check(&[0], &nothing),
            Err(Rejected::TooLarge),
            "one byte, under no ceiling"
        );
    }
}
