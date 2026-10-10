//! The change hashes of a saved document, kept beside it so loading it again
//! need not rebuild and hash every change to learn them (TIL-762).
//!
//! A compacted Automerge document stores each change's metadata but not its
//! hash, so a plain load re-encodes every change from the op set and hashes
//! it: most of the cost of opening a large record. The writer has those
//! hashes in memory when it saves, so it keeps them in a hint beside the
//! record, and the next load passes them to
//! [`LoadOptions::known_change_hashes`](automerge::LoadOptions::known_change_hashes).
//!
//! A hint is trusted like the record bytes it names, and only for them:
//! - it names the exact document chunk it was made for by BLAKE3 digest, so
//!   it is used for those bytes or not at all (changes appended after the
//!   chunk keep it valid; a replacement makes it stale and ignored);
//! - only [`DocWriter`](crate::DocWriter) makes one, from the document it
//!   holds, whose changes were verified when they entered it;
//! - the store keeps it on this device beside the record and never syncs or
//!   receives it: documents and changes from peers are always rebuilt and
//!   verified in full.
//!
//! Automerge still checks a hint is one distinct hash per change whose
//! dependency-free tips are the document's heads, and ignores it otherwise.

use automerge::ChangeHash;

/// Format tag; a hint in any other format is ignored.
const MAGIC: &[u8; 8] = b"TILOCH01";

/// A Blake3 digest's length.
const DIGEST: usize = 32;

/// An Automerge change hash's length.
const HASH: usize = 32;

/// Below this a document loads in about a millisecond, and the hint (32
/// bytes per change) would not pay for the file beside it.
pub(crate) const MIN_HINTED_BYTES: usize = 64 * 1024;

/// The hint for `saved` (bytes [`automerge::AutoCommit::save`] returned)
/// listing `hashes` (its `change_hashes()` at that save), or `None` for a
/// document too small to need one.
pub(crate) fn encode(saved: &[u8], hashes: &[ChangeHash]) -> Option<Vec<u8>> {
    if saved.len() < MIN_HINTED_BYTES {
        return None;
    }
    let chunk = leading_document_chunk(saved)?;
    let mut hint = Vec::with_capacity(
        MAGIC
            .len()
            .saturating_add(DIGEST)
            .saturating_add(HASH.saturating_mul(hashes.len())),
    );
    hint.extend_from_slice(MAGIC);
    hint.extend_from_slice(blake3::hash(chunk).as_bytes());
    for hash in hashes {
        hint.extend_from_slice(&hash.0);
    }
    Some(hint)
}

/// The change hashes `hint` lists, when it was made for the document chunk
/// `stored` starts with; `None` for any other bytes or a malformed hint.
pub(crate) fn hashes_for(hint: &[u8], stored: &[u8]) -> Option<Vec<ChangeHash>> {
    let rest = hint.strip_prefix(MAGIC.as_slice())?;
    let (digest, hashes) = rest.split_at_checked(DIGEST)?;
    if !hashes.len().is_multiple_of(HASH) {
        return None;
    }
    let chunk = leading_document_chunk(stored)?;
    if blake3::hash(chunk).as_bytes().as_slice() != digest {
        return None;
    }
    hashes
        .chunks_exact(HASH)
        .map(|hash| ChangeHash::try_from(hash).ok())
        .collect()
}

/// The chunk `bytes` starts with, header included, when it is a document
/// chunk: Automerge's magic bytes, a 4-byte checksum, chunk type 0 and the
/// body's length as unsigned LEB128.
fn leading_document_chunk(bytes: &[u8]) -> Option<&[u8]> {
    const CHUNK_MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];
    const DOCUMENT: u8 = 0;
    let rest = bytes.strip_prefix(CHUNK_MAGIC.as_slice())?.get(4..)?;
    let (&kind, rest) = rest.split_first()?;
    if kind != DOCUMENT {
        return None;
    }
    let mut body = 0u64;
    let mut used = 0usize;
    for (index, byte) in rest.iter().take(10).enumerate() {
        let shift = u32::try_from(index).ok()?.checked_mul(7)?;
        body |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            used = index.checked_add(1)?;
            break;
        }
    }
    if used == 0 {
        return None;
    }
    let header = CHUNK_MAGIC
        .len()
        .checked_add(4)?
        .checked_add(1)?
        .checked_add(used)?;
    bytes.get(..header.checked_add(usize::try_from(body).ok()?)?)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::arithmetic_side_effects,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ObjType, ROOT};

    fn large() -> AutoCommit {
        let mut doc = AutoCommit::new();
        let items = doc.put_object(ROOT, "items", ObjType::Map).unwrap();
        for i in 0..4_000_u32 {
            doc.put(
                &items,
                format!("item-{i:05}"),
                blake3::hash(&i.to_le_bytes()).to_hex().to_string(),
            )
            .unwrap();
            if i % 100 == 0 {
                doc.commit();
            }
        }
        doc.commit();
        doc
    }

    #[test]
    fn a_hint_names_exactly_the_document_chunk_it_was_made_for() {
        let mut doc = large();
        let saved = doc.save();
        let hashes = doc.committed_document().change_hashes().to_vec();
        let hint = encode(&saved, &hashes).expect("a large document is hinted");
        assert_eq!(hashes_for(&hint, &saved), Some(hashes.clone()));

        // Phase `chunks_appended_after_it_keep_it_valid`.
        let mut grown = saved.clone();
        let sent = doc.get_heads();
        doc.put(ROOT, "later", 1).unwrap();
        doc.commit();
        grown.extend(doc.save_after(&sent));
        assert_eq!(hashes_for(&hint, &grown), Some(hashes));

        // Phase `any_other_bytes_make_it_stale`.
        let replaced = doc.save();
        assert_eq!(hashes_for(&hint, &replaced), None);
        let mut flipped = saved.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        assert_eq!(hashes_for(&hint, &flipped), None);
        assert_eq!(hashes_for(&hint, &saved[..saved.len() / 2]), None);
        assert_eq!(hashes_for(&hint, &grown[saved.len()..]), None);

        // Phase `a_malformed_hint_is_ignored`.
        assert_eq!(hashes_for(&hint[..hint.len() - 1], &saved), None);
        assert_eq!(hashes_for(&hint[1..], &saved), None);
        assert_eq!(hashes_for(b"", &saved), None);
    }

    #[test]
    fn a_small_document_gets_no_hint() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "small", 1).unwrap();
        let saved = doc.save();
        let hashes = doc.committed_document().change_hashes().to_vec();
        assert!(saved.len() < MIN_HINTED_BYTES);
        assert_eq!(encode(&saved, &hashes), None);
    }
}
