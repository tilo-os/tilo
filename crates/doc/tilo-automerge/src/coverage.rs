//! What a saved Automerge document provably holds.
//!
//! A durable host trims a retained history only by what a newer checkpoint
//! names as covered, and the publisher of that checkpoint names only the
//! retained messages whose changes are in the bytes it saved. These helpers
//! answer that question from the bytes themselves — never from delivery
//! order — so a message the saver never received, deferred, or refused is
//! never claimed.

use automerge::{AutoCommit, Change, ChangeHash, ReadDoc};

/// Every Automerge chunk starts with these bytes.
const MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];
const DOCUMENT_CHUNK: u8 = 0;
const CHANGE_CHUNK: u8 = 1;
const COMPRESSED_CHANGE_CHUNK: u8 = 2;

/// The change hashes a message of Automerge bytes (a saved document, one
/// change, or incremental changes concatenated) must find in a document to
/// be held by it: the heads of each document chunk — history is closed under
/// dependencies — and the hash of each change chunk. `None` when the bytes
/// are not entirely chunks this can read (a bundle, or not Automerge at
/// all); such a message is never claimed as held.
#[must_use]
pub fn required_changes(bytes: &[u8]) -> Option<Vec<ChangeHash>> {
    let mut rest = bytes;
    let mut required = Vec::new();
    while !rest.is_empty() {
        let (chunk, remaining) = split_chunk(rest)?;
        match chunk.get(8)? {
            &DOCUMENT_CHUNK => required.extend(AutoCommit::load(chunk).ok()?.get_heads()),
            &CHANGE_CHUNK | &COMPRESSED_CHANGE_CHUNK => {
                required.push(Change::try_from(chunk).ok()?.hash());
            }
            _ => return None,
        }
        rest = remaining;
    }
    Some(required)
}

/// The changes a message of incremental Automerge bytes carries, read
/// without a document. `None` unless the bytes are entirely change chunks —
/// a saved document or anything unreadable is not incremental changes.
#[must_use]
pub fn changes_in(bytes: &[u8]) -> Option<Vec<Change>> {
    let mut rest = bytes;
    let mut changes = Vec::new();
    while !rest.is_empty() {
        let (chunk, remaining) = split_chunk(rest)?;
        match chunk.get(8)? {
            &CHANGE_CHUNK | &COMPRESSED_CHANGE_CHUNK => changes.push(Change::try_from(chunk).ok()?),
            _ => return None,
        }
        rest = remaining;
    }
    Some(changes)
}

/// Whether `doc` holds every change in `required`.
#[must_use]
pub fn holds_all(doc: &AutoCommit, required: &[ChangeHash]) -> bool {
    required
        .iter()
        .all(|hash| ReadDoc::get_change_by_hash(doc, hash).is_some())
}

/// Split the first chunk off `bytes`: magic, checksum, type, LEB128 length,
/// then that many bytes of data.
fn split_chunk(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    if bytes.get(..4)? != MAGIC {
        return None;
    }
    let mut at = 9usize;
    let mut len = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *bytes.get(at)?;
        at = at.checked_add(1)?;
        len |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            break;
        }
        shift = shift.checked_add(7)?;
        if shift > 63 {
            return None;
        }
    }
    let end = at.checked_add(usize::try_from(len).ok()?)?;
    Some((bytes.get(..end)?, bytes.get(end..)?))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;
    use automerge::transaction::Transactable;

    fn edit(doc: &mut AutoCommit, key: &str) -> Vec<u8> {
        doc.put(automerge::ROOT, key, 1).unwrap();
        doc.save_incremental()
    }

    #[test]
    fn a_saved_document_holds_exactly_the_changes_in_it() {
        let mut doc = AutoCommit::new();
        let first = edit(&mut doc, "a");
        let second = edit(&mut doc, "b");
        let both: Vec<u8> = [first.clone(), second.clone()].concat();
        let saved_early = doc.clone().save();
        let third = edit(&mut doc, "c");

        let early = AutoCommit::load(&saved_early).unwrap();
        for held in [&first, &second, &both, &saved_early] {
            assert!(holds_all(&early, &required_changes(held).unwrap()));
        }
        assert!(!holds_all(&early, &required_changes(&third).unwrap()));
        let full = AutoCommit::load(&doc.save()).unwrap();
        assert!(holds_all(&full, &required_changes(&third).unwrap()));
    }

    #[test]
    fn bytes_that_are_not_automerge_are_never_held() {
        assert!(required_changes(b"not automerge").is_none());
        assert_eq!(required_changes(&[]), Some(Vec::new()));
    }
}
