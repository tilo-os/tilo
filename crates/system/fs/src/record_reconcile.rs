//! Range-based set reconciliation of record stream heads.
//!
//! Each side holds a set of items, one per `(record, actor)` stream: the
//! stream's contiguous sequence and prefix digest. Two peers learn which
//! streams differ without listing them all: the client sends the
//! fingerprint (XOR of item hashes, plus a count) of a key range; the
//! server compares it with its own, and answers "equal", or its items for
//! the range when there are few, or its fingerprints of sub-ranges for the
//! client to compare and descend into. A workspace of `n` streams with `d`
//! differences settles in O(log n) rounds and O(d · log n) items on the
//! wire, and an unchanged workspace in one round trip of one range.
//!
//! Only which streams to pull is decided here. Every envelope pulled
//! afterwards is still verified, authorised and prefix-checked by the
//! repository, so a wrong fingerprint can at worst delay a pull, never
//! admit anything.

use serde::{Deserialize, Serialize};

use crate::record_mesh::RecordMeshStreamV1;
use crate::record_repository::{RecordStreamCursor, RecordStreamKey};

/// A range with at most this many server items is answered with the items.
pub const ITEMS_THRESHOLD: usize = 64;
/// A range the client holds nothing of is answered with up to this many
/// items directly: there is nothing to compare.
pub const EMPTY_RANGE_ITEMS: usize = 4096;
/// Sub-ranges a differing range is split into.
pub const SPLIT_PARTS: usize = 16;

/// One key range `[lower, upper)` and the sender's summary of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcileRangeV1 {
    /// Inclusive lower bound; `None` is the start of the key space.
    pub lower: Option<RecordMeshStreamV1>,
    /// Exclusive upper bound; `None` is the end of the key space.
    pub upper: Option<RecordMeshStreamV1>,
    pub fingerprint: [u8; 32],
    pub count: u64,
}

/// The server's answer for one range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ReconcileAnswerV1 {
    /// Both sides hold the same items.
    Equal,
    Items(Vec<RecordStreamCursor>),
    /// The server's summaries of sub-ranges covering the range.
    Split(Vec<ReconcileRangeV1>),
}

pub fn stream_key(stream: &RecordMeshStreamV1) -> RecordStreamKey {
    RecordStreamKey {
        record_id: stream.record_id,
        person: *stream.actor.person.as_bytes(),
        device: *stream.actor.device.as_bytes(),
    }
}

fn item_hash(cursor: &RecordStreamCursor) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo.fs.record-stream-reconcile.v1\0");
    hasher.update(cursor.record_id.0.as_bytes());
    hasher.update(cursor.actor.person.as_bytes());
    hasher.update(cursor.actor.device.as_bytes());
    hasher.update(&cursor.contiguous_sequence.to_be_bytes());
    hasher.update(&cursor.prefix_digest);
    *hasher.finalize().as_bytes()
}

/// The summary of `items`.
#[must_use]
pub fn fingerprint(items: &[RecordStreamCursor]) -> ([u8; 32], u64) {
    let mut acc = [0_u8; 32];
    for item in items {
        for (a, b) in acc.iter_mut().zip(item_hash(item)) {
            *a ^= b;
        }
    }
    (acc, items.len() as u64)
}

/// A range over `items` (already restricted to it) with their summary.
#[must_use]
pub fn summarize(
    lower: Option<RecordMeshStreamV1>,
    upper: Option<RecordMeshStreamV1>,
    items: &[RecordStreamCursor],
) -> ReconcileRangeV1 {
    let (fingerprint, count) = fingerprint(items);
    ReconcileRangeV1 {
        lower,
        upper,
        fingerprint,
        count,
    }
}

/// The server side of one range: `items` are the server's items within it.
#[must_use]
pub fn answer(range: &ReconcileRangeV1, items: &[RecordStreamCursor]) -> ReconcileAnswerV1 {
    let (fingerprint, count) = fingerprint(items);
    if fingerprint == range.fingerprint && count == range.count {
        return ReconcileAnswerV1::Equal;
    }
    if items.len() <= ITEMS_THRESHOLD || (range.count == 0 && items.len() <= EMPTY_RANGE_ITEMS) {
        return ReconcileAnswerV1::Items(items.to_vec());
    }
    let per_part = items.len().div_ceil(SPLIT_PARTS).max(1);
    let mut parts = Vec::with_capacity(SPLIT_PARTS);
    let mut lower = range.lower;
    for (index, chunk) in items.chunks(per_part).enumerate() {
        let upper = items
            .get(index.saturating_add(1).saturating_mul(per_part))
            .map(RecordMeshStreamV1::from)
            .or(range.upper);
        parts.push(summarize(lower, upper, chunk));
        lower = upper;
    }
    ReconcileAnswerV1::Split(parts)
}

/// Whether `cursor` lies in `[lower, upper)`.
#[must_use]
pub fn contains(range: &ReconcileRangeV1, cursor: &RecordStreamCursor) -> bool {
    let key = cursor.key();
    range.lower.is_none_or(|lower| key >= stream_key(&lower))
        && range.upper.is_none_or(|upper| key < stream_key(&upper))
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::Ed25519PublicKey;
    use fs_protocol::{FileActor, FileRecordId};

    fn cursor(n: u32, sequence: u64) -> RecordStreamCursor {
        RecordStreamCursor {
            record_id: FileRecordId(uuid::Uuid::from_u128(u128::from(n))),
            actor: FileActor {
                person: Ed25519PublicKey::from_bytes([1; 32]),
                device: Ed25519PublicKey::from_bytes([2; 32]),
            },
            contiguous_sequence: sequence,
            prefix_digest: [u8::try_from(sequence % 251).unwrap_or(0); 32],
        }
    }

    fn within(range: &ReconcileRangeV1, items: &[RecordStreamCursor]) -> Vec<RecordStreamCursor> {
        items
            .iter()
            .filter(|c| contains(range, c))
            .copied()
            .collect()
    }

    /// Runs the protocol to completion; returns the server items the client
    /// learns about and the number of rounds.
    fn reconcile(
        client: &[RecordStreamCursor],
        server: &[RecordStreamCursor],
    ) -> (Vec<RecordStreamCursor>, usize) {
        let mut ranges = vec![summarize(None, None, client)];
        let mut learned = Vec::new();
        let mut rounds = 0_usize;
        while !ranges.is_empty() {
            rounds = rounds.saturating_add(1);
            let mut next = Vec::new();
            for range in &ranges {
                match answer(range, &within(range, server)) {
                    ReconcileAnswerV1::Equal => {}
                    ReconcileAnswerV1::Items(items) => learned.extend(items),
                    ReconcileAnswerV1::Split(parts) => {
                        for part in parts {
                            let mine = within(&part, client);
                            let summary = summarize(part.lower, part.upper, &mine);
                            if (summary.fingerprint, summary.count)
                                != (part.fingerprint, part.count)
                            {
                                next.push(summary);
                            }
                        }
                    }
                }
            }
            ranges = next;
        }
        (learned, rounds)
    }

    #[test]
    fn identical_sets_settle_in_one_round() {
        let items: Vec<_> = (0..10_000).map(|n| cursor(n, 3)).collect();
        let (learned, rounds) = reconcile(&items, &items);
        assert!(learned.is_empty());
        assert_eq!(rounds, 1);
    }

    #[test]
    fn few_differences_in_a_large_set_cost_a_few_rounds_and_items() {
        let server: Vec<_> = (0..10_000).map(|n| cursor(n, 3)).collect();
        let mut client = server.clone();
        client[17].contiguous_sequence = 2;
        client[9_000].contiguous_sequence = 1;
        client.remove(5_000);
        let (learned, rounds) = reconcile(&client, &server);
        for n in [17_u32, 9_000, 5_000] {
            assert!(
                learned.iter().any(|c| *c == cursor(n, 3)),
                "stream {n} is learned"
            );
        }
        assert!(rounds <= 4, "{rounds} rounds");
        assert!(
            learned.len() <= 3 * ITEMS_THRESHOLD,
            "{} items",
            learned.len()
        );
    }

    #[test]
    fn an_empty_client_learns_everything() {
        let server: Vec<_> = (0..20_000).map(|n| cursor(n, 1)).collect();
        let (learned, rounds) = reconcile(&[], &server);
        assert_eq!(learned.len(), server.len());
        assert!(rounds <= 3, "{rounds} rounds");
    }

    #[test]
    fn items_only_the_client_holds_are_not_reported() {
        let server: Vec<_> = (0..100).map(|n| cursor(n, 1)).collect();
        let mut client = server.clone();
        client.push(cursor(1_000, 1));
        let (learned, _) = reconcile(&client, &server);
        assert!(learned.iter().all(|c| server.contains(c)));
    }
}
