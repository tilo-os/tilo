//! Check that every chunk of incoming Automerge storage parses, before any of
//! it is applied.
//!
//! [`AutoCommit::load_incremental`] keeps the chunks before the first one that
//! does not parse and drops the rest with a log line, so a truncated or
//! corrupt payload would apply in part. Loading the payload on its own first
//! (the strict [`automerge::Automerge::load_with_options`]) refuses that, but
//! builds a whole document only to throw it away, and for changes that build
//! on history the payload does not carry, which is what an incremental
//! payload is, it fails with `MissingDeps` after parsing everything, and
//! Automerge logs that failure as an error for every payload.
//!
//! [`check`] parses each chunk on its own instead: a change decodes alone and
//! its stored checksum must match its hash, which is what a load checks.

use automerge::{Automerge, AutomergeError, Change, LoadOptions, OnPartialLoad, VerificationMode};

const MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];
const CHANGE: u8 = 1;
const COMPRESSED_CHANGE: u8 = 2;

fn malformed(what: &str) -> AutomergeError {
    AutomergeError::EncodingError(format!("malformed automerge storage: {what}"))
}

/// Refuse `data` unless every chunk of it parses. Nothing is applied.
///
/// # Errors
///
/// The first chunk that is not framed as Automerge storage, does not decode,
/// or is a change whose stored checksum is not its hash.
pub fn check(data: &[u8]) -> Result<(), AutomergeError> {
    let mut rest = data;
    while !rest.is_empty() {
        let (chunk, after) = split_chunk(rest)?;
        rest = after;
        match chunk.get(8) {
            Some(&(CHANGE | COMPRESSED_CHANGE)) => {
                let change = Change::try_from(chunk)?;
                if chunk.get(4..8) != change.hash().0.get(..4) {
                    return Err(malformed("bad change checksum"));
                }
            }
            // A document or a bundle has no cheaper parse than its load. A
            // bundle may build on history it does not carry: `MissingDeps`
            // comes only after every part of it parsed.
            _ => match Automerge::load_with_options(
                chunk,
                LoadOptions::new()
                    .on_partial_load(OnPartialLoad::Error)
                    .verification_mode(VerificationMode::Check),
            ) {
                Ok(_) | Err(AutomergeError::MissingDeps) => {}
                Err(error) => return Err(error),
            },
        }
    }
    Ok(())
}

/// The first chunk of `data`, header included, and the bytes after it.
fn split_chunk(data: &[u8]) -> Result<(&[u8], &[u8]), AutomergeError> {
    let (magic, after) = data
        .split_first_chunk::<4>()
        .ok_or_else(|| malformed("truncated chunk header"))?;
    if *magic != MAGIC {
        return Err(malformed("not an automerge chunk"));
    }
    let after = after
        .get(5..)
        .ok_or_else(|| malformed("truncated chunk header"))?;
    let (length, body) = uleb128(after)?;
    let length = usize::try_from(length).map_err(|_| malformed("chunk longer than input"))?;
    let header = data.len().saturating_sub(body.len());
    let end = header
        .checked_add(length)
        .filter(|end| *end <= data.len())
        .ok_or_else(|| malformed("chunk longer than input"))?;
    Ok(data.split_at(end))
}

/// One unsigned LEB128 value of at most 64 bits, and the bytes after it.
fn uleb128(bytes: &[u8]) -> Result<(u64, &[u8]), AutomergeError> {
    let mut value = 0u64;
    for (index, byte) in bytes.iter().enumerate().take(10) {
        let shift = u32::try_from(index.saturating_mul(7)).unwrap_or(u32::MAX);
        let part = u64::from(byte & 0x7f);
        if shift == 63 && part > 1 {
            return Err(malformed("chunk length overflows"));
        }
        value |= part << shift;
        if byte & 0x80 == 0 {
            return Ok((
                value,
                bytes.get(index.saturating_add(1)..).unwrap_or_default(),
            ));
        }
    }
    Err(malformed("truncated chunk length"))
}

#[cfg(test)]
mod tests {
    use super::check;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT};

    /// A document with one change, and the change after it as an incremental
    /// payload: it depends on the first, which the payload does not carry.
    fn delta() -> (AutoCommit, Vec<u8>) {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "k", "v").unwrap();
        doc.commit();
        let before = doc.get_heads();
        doc.put(ROOT, "k", "w").unwrap();
        doc.put(ROOT, "j", "x").unwrap();
        doc.commit();
        let delta = doc.save_after(&before);
        (doc, delta)
    }

    #[test]
    fn changes_that_build_on_history_elsewhere_parse() {
        let (mut doc, delta) = delta();
        check(&delta).unwrap();
        check(&doc.save()).unwrap();
        check(&[doc.save(), delta].concat()).unwrap();
        check(&[]).unwrap();
    }

    #[test]
    fn every_truncation_and_a_flipped_byte_are_refused() {
        let (_, delta) = delta();
        for end in 1..delta.len() {
            assert!(check(&delta[..end]).is_err(), "truncated at {end}");
        }
        for at in 0..delta.len() {
            let mut corrupt = delta.clone();
            corrupt[at] ^= 0x01;
            assert!(check(&corrupt).is_err(), "byte {at} flipped");
        }
    }
}
