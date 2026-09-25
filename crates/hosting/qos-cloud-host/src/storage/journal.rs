//! Append-only commit journal behind the whole-file Cloud Store checkpoint.
//!
//! Ported from `qos-session-peer/src/store/journal.rs` (same record format and
//! delta algorithm; the crates are separate, so the module is copied rather
//! than shared). Differences: the file name; the replay entry points take the
//! checkpoint digest the caller already computed; and `replay_fast`, because
//! every Cloud Store transaction (not only an owner open) reconstructs the
//! journal: it applies records without hashing each intermediate state and
//! checks only the last applied record's digest, falling back to the
//! per-record checked `replay_bound` on any mismatch.
//!
//! `state.json` stays the atomically replaced checkpoint. Between checkpoints
//! each commit appends one self-validating record to `state.journal` and
//! issues one file barrier, instead of rewriting and barriering the complete
//! aggregate (file plus directory) on every commit.
//!
//! Layout: a header binds the journal to one exact checkpoint
//! (`MAGIC | checkpoint length u64 | blake3(checkpoint)`), followed by records
//! `body length u32 | body`. A body is a copy/literal edit script against the
//! previous state plus the exact length and blake3 digest of the resulting
//! state. Replay applies records in order from the checkpoint and stops at the
//! first record that is truncated, malformed, out of bounds or whose result
//! digest differs: only a torn tail from an unacknowledged append can end that
//! way, because every acknowledged record was fully written and barriered
//! before its commit returned. A header naming any other checkpoint makes the
//! whole journal stale (a later checkpoint superseded it).
use anyhow::{Result, bail, ensure};

pub(super) const NAME: &str = "state.journal";
const MAGIC: &[u8; 8] = b"QOSJRN01";
pub(super) const HEADER_LEN: usize = 8 + 8 + 32;
const BLOCK: usize = 64;
const COPY: u8 = 0;
const LITERAL: u8 = 1;

#[cfg(test)]
pub(super) fn header(checkpoint: &[u8]) -> [u8; HEADER_LEN] {
    header_for(checkpoint.len() as u64, blake3::hash(checkpoint).as_bytes())
}
pub(super) fn header_for(checkpoint_len: u64, checkpoint_digest: &[u8; 32]) -> [u8; HEADER_LEN] {
    let mut header = [0; HEADER_LEN];
    header[..8].copy_from_slice(MAGIC);
    header[8..16].copy_from_slice(&checkpoint_len.to_le_bytes());
    header[16..].copy_from_slice(checkpoint_digest);
    header
}

/// Result of replaying a journal against its checkpoint.
pub(super) struct Replay {
    pub state: Vec<u8>,
    /// Byte length of the valid prefix (header plus complete records); zero
    /// when the journal is absent, empty, stale or has no valid header.
    pub valid_len: u64,
    pub records: usize,
}

#[cfg(test)]
pub(super) fn replay(checkpoint: Vec<u8>, journal: &[u8], limit: usize) -> Replay {
    let digest = *blake3::hash(&checkpoint).as_bytes();
    replay_bound(checkpoint, &digest, journal, limit)
}
/// `replay` for a checkpoint whose blake3 digest the caller already holds.
pub(super) fn replay_bound(
    checkpoint: Vec<u8>,
    checkpoint_digest: &[u8; 32],
    journal: &[u8],
    limit: usize,
) -> Replay {
    if journal.len() < HEADER_LEN
        || journal[..HEADER_LEN] != header_for(checkpoint.len() as u64, checkpoint_digest)
    {
        return Replay {
            state: checkpoint,
            valid_len: 0,
            records: 0,
        };
    }
    let mut state = checkpoint;
    let mut offset = HEADER_LEN;
    let mut records = 0;
    while let Some(len) = journal
        .get(offset..offset + 4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()) as usize)
    {
        let Some(body) = journal.get(offset + 4..(offset + 4).saturating_add(len)) else {
            break;
        };
        match apply(&state, body, limit) {
            Ok(next) => state = next,
            Err(_) => break,
        }
        offset += 4 + len;
        records += 1;
    }
    Replay {
        state,
        valid_len: offset as u64,
        records,
    }
}

/// `replay_bound`'s result, hashing only the final state in the common case.
///
/// Records are applied structurally (bounds and lengths checked) until the
/// first that does not frame or apply; the resulting state is accepted when
/// its blake3 equals the digest the last applied record carries. By
/// collision resistance that state is then exactly the state its writer
/// recorded, which is what `replay_bound` reconstructs for every journal this
/// crate writes (single locked appender; only an unacknowledged torn tail can
/// follow). Any mismatch falls back to `replay_bound`, which stops at the
/// first invalid record.
pub(super) fn replay_fast(
    checkpoint: Vec<u8>,
    checkpoint_digest: &[u8; 32],
    journal: &[u8],
    limit: usize,
) -> Replay {
    if journal.len() < HEADER_LEN
        || journal[..HEADER_LEN] != header_for(checkpoint.len() as u64, checkpoint_digest)
    {
        return Replay {
            state: checkpoint,
            valid_len: 0,
            records: 0,
        };
    }
    let mut current: Option<Vec<u8>> = None;
    let mut spare = Vec::new();
    let mut claimed = None;
    let mut offset = HEADER_LEN;
    let mut records = 0;
    while let Some(len) = journal
        .get(offset..offset + 4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()) as usize)
    {
        let Some(body) = journal.get(offset + 4..(offset + 4).saturating_add(len)) else {
            break;
        };
        let base = current.as_deref().unwrap_or(&checkpoint);
        match apply_into(base, body, limit, &mut spare) {
            Ok(digest) => {
                claimed = Some(digest);
                let previous = current.replace(std::mem::take(&mut spare));
                spare = previous.unwrap_or_default();
            }
            Err(_) => break,
        }
        offset += 4 + len;
        records += 1;
    }
    match (current, claimed) {
        (None, _) => Replay {
            state: checkpoint,
            valid_len: HEADER_LEN as u64,
            records: 0,
        },
        (Some(state), Some(digest)) if *blake3::hash(&state).as_bytes() == digest => Replay {
            state,
            valid_len: offset as u64,
            records,
        },
        _ => replay_bound(checkpoint, checkpoint_digest, journal, limit),
    }
}

/// One framed record turning `old` into `new`, or `None` when the edit script
/// would not be meaningfully smaller than rewriting `new` (callers then
/// checkpoint instead).
///
/// `new_digest` is `blake3(new)`, which the caller already needs.
pub(super) fn record(old: &[u8], new: &[u8], new_digest: &[u8; 32]) -> Option<Vec<u8>> {
    let mut body = Vec::with_capacity(64);
    let mut ops = 0u32;
    body.extend_from_slice(&[0; 4]);
    let push_copy = |body: &mut Vec<u8>, ops: &mut u32, from: usize, len: usize| {
        body.push(COPY);
        body.extend_from_slice(&(from as u32).to_le_bytes());
        body.extend_from_slice(&(len as u32).to_le_bytes());
        *ops += 1;
    };
    let push_literal = |body: &mut Vec<u8>, ops: &mut u32, bytes: &[u8]| {
        if bytes.is_empty() {
            return;
        }
        body.push(LITERAL);
        body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(bytes);
        *ops += 1;
    };
    if old.len() > u32::MAX as usize || new.len() > u32::MAX as usize {
        return None;
    }
    let budget = new.len() / 2;
    let prefix = common_prefix(old, new);
    let suffix = common_prefix_rev(&old[prefix..], &new[prefix..]);
    if prefix > 0 {
        push_copy(&mut body, &mut ops, 0, prefix);
    }
    let middle = &new[prefix..new.len() - suffix];
    // Content between the common prefix and suffix comes (shifted) from the
    // same region of the previous state; only that region is indexed.
    let index = BlockIndex::new(old, prefix, old.len() - suffix);
    let mut literal_start = 0;
    let mut at = 0;
    let mut hash = RollingHash::default();
    let mut window = 0;
    while at + BLOCK <= middle.len() {
        if window < BLOCK {
            // (Re)prime the rolling window after a copy or at the start.
            hash = RollingHash::of(&middle[at..at + BLOCK]);
            window = BLOCK;
        }
        let found = index
            .lookup(hash.value(), old, &middle[at..at + BLOCK])
            .map(|from| {
                (
                    from,
                    BLOCK + match_len(&middle[at + BLOCK..], &old[from + BLOCK..]),
                )
            });
        if let Some((mut from, mut len)) = found {
            // Extend backwards into the pending literal.
            let mut start = at;
            while start > literal_start && from > 0 && middle[start - 1] == old[from - 1] {
                start -= 1;
                from -= 1;
                len += 1;
            }
            push_literal(&mut body, &mut ops, &middle[literal_start..start]);
            push_copy(&mut body, &mut ops, from, len);
            at = start + len;
            literal_start = at;
            window = 0;
            if body.len() > budget {
                return None;
            }
            continue;
        }
        if at + BLOCK < middle.len() {
            hash.roll(middle[at], middle[at + BLOCK]);
        }
        at += 1;
        if at - literal_start > budget {
            return None;
        }
    }
    push_literal(&mut body, &mut ops, &middle[literal_start..]);
    if suffix > 0 {
        push_copy(&mut body, &mut ops, old.len() - suffix, suffix);
    }
    body[..4].copy_from_slice(&ops.to_le_bytes());
    body.extend_from_slice(&(new.len() as u32).to_le_bytes());
    body.extend_from_slice(new_digest);
    if body.len() > budget {
        return None;
    }
    let mut framed = Vec::with_capacity(4 + body.len());
    framed.extend_from_slice(&(body.len() as u32).to_le_bytes());
    framed.extend_from_slice(&body);
    Some(framed)
}

fn apply(old: &[u8], body: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let digest = apply_into(old, body, limit, &mut out)?;
    ensure!(
        blake3::hash(&out).as_bytes() == &digest,
        "journal state digest mismatch"
    );
    Ok(out)
}
/// Apply one record's edit script into `out` (cleared first), checking its
/// structure and resulting length; returns the digest the record claims.
fn apply_into(old: &[u8], body: &[u8], limit: usize, out: &mut Vec<u8>) -> Result<[u8; 32]> {
    let mut reader = Reader(body);
    let ops = reader.u32()?;
    out.clear();
    for _ in 0..ops {
        match reader.u8()? {
            COPY => {
                let from = reader.u32()? as usize;
                let len = reader.u32()? as usize;
                let Some(bytes) = old.get(from..from.saturating_add(len)) else {
                    bail!("journal copy out of bounds");
                };
                ensure!(out.len() + len <= limit, "journal state exceeds limit");
                out.extend_from_slice(bytes);
            }
            LITERAL => {
                let len = reader.u32()? as usize;
                ensure!(out.len() + len <= limit, "journal state exceeds limit");
                out.extend_from_slice(reader.take(len)?);
            }
            _ => bail!("unknown journal operation"),
        }
    }
    let len = reader.u32()? as usize;
    let digest = reader.take(32)?;
    ensure!(reader.0.is_empty(), "trailing journal record bytes");
    ensure!(out.len() == len, "journal state length mismatch");
    Ok(digest.try_into().expect("32-byte digest"))
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        ensure!(self.0.len() >= len, "truncated journal record");
        let (head, tail) = self.0.split_at(len);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

/// Length of the common prefix of `a` and `b`, compared a word at a time.
fn match_len(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 8 <= n {
        let x = u64::from_le_bytes(a[i..i + 8].try_into().unwrap())
            ^ u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        if x != 0 {
            return i + (x.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}
fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    match_len(a, b)
}
fn common_prefix_rev(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let (a, b) = (&a[a.len() - n..], &b[b.len() - n..]);
    let mut i = 0;
    while i + 8 <= n {
        let x = u64::from_le_bytes(a[n - i - 8..n - i].try_into().unwrap())
            ^ u64::from_le_bytes(b[n - i - 8..n - i].try_into().unwrap());
        if x != 0 {
            return i + (x.leading_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < n && a[n - 1 - i] == b[n - 1 - i] {
        i += 1;
    }
    i
}

/// Polynomial rolling hash over exactly `BLOCK` bytes (wrapping arithmetic).
#[derive(Default, Clone, Copy)]
struct RollingHash(u64);
const BASE: u64 = 0x100_0000_01b3;
const BASE_POW: u64 = {
    // BASE^(BLOCK-1), used to remove the outgoing byte.
    let mut value = 1u64;
    let mut i = 1;
    while i < BLOCK {
        value = value.wrapping_mul(BASE);
        i += 1;
    }
    value
};
const BASE2: u64 = BASE.wrapping_mul(BASE);
const BASE3: u64 = BASE2.wrapping_mul(BASE);
const BASE4: u64 = BASE2.wrapping_mul(BASE2);
impl RollingHash {
    /// Same value as folding `hash * BASE + byte + 1` over the block, split
    /// into four independent lanes (stride four) so the multiplies pipeline.
    fn of(bytes: &[u8]) -> Self {
        debug_assert_eq!(bytes.len(), BLOCK);
        let mut lanes = [0u64; 4];
        for chunk in bytes.chunks_exact(4) {
            for (lane, &byte) in lanes.iter_mut().zip(chunk) {
                *lane = lane.wrapping_mul(BASE4).wrapping_add(u64::from(byte) + 1);
            }
        }
        Self(
            lanes[0]
                .wrapping_mul(BASE3)
                .wrapping_add(lanes[1].wrapping_mul(BASE2))
                .wrapping_add(lanes[2].wrapping_mul(BASE))
                .wrapping_add(lanes[3]),
        )
    }
    fn roll(&mut self, outgoing: u8, incoming: u8) {
        self.0 = self
            .0
            .wrapping_sub((u64::from(outgoing) + 1).wrapping_mul(BASE_POW))
            .wrapping_mul(BASE)
            .wrapping_add(u64::from(incoming) + 1);
    }
    fn value(self) -> u64 {
        self.0
    }
}

/// Direct-mapped table from block hash to the offset (plus one) of one
/// `BLOCK` of the previous state at a `BLOCK` stride inside `start..end`.
/// Collisions keep the first block; a miss only makes a record larger.
struct BlockIndex {
    slots: Vec<u32>,
    shift: u32,
}
impl BlockIndex {
    fn new(old: &[u8], start: usize, end: usize) -> Self {
        let blocks = end.saturating_sub(start) / BLOCK;
        let bits = (blocks.max(1) * 2)
            .next_power_of_two()
            .trailing_zeros()
            .max(4);
        let mut index = Self {
            slots: vec![0; 1 << bits],
            shift: 64 - bits,
        };
        for block in 0..blocks {
            let from = start + block * BLOCK;
            let slot = index.slot(RollingHash::of(&old[from..from + BLOCK]).value());
            if index.slots[slot] == 0 {
                index.slots[slot] = from as u32 + 1;
            }
        }
        index
    }
    fn slot(&self, hash: u64) -> usize {
        // Fibonacci hashing spreads the polynomial hash's low-entropy bits.
        (hash.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> self.shift) as usize
    }
    fn lookup(&self, hash: u64, old: &[u8], block: &[u8]) -> Option<usize> {
        let from = self.slots[self.slot(hash)].checked_sub(1)? as usize;
        (old[from..from + BLOCK] == *block).then_some(from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pseudo(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    fn journal_of(checkpoint: &[u8], states: &[Vec<u8>]) -> Vec<u8> {
        let mut journal = header(checkpoint).to_vec();
        let mut previous = checkpoint.to_vec();
        for state in states {
            journal.extend(
                record(&previous, state, blake3::hash(state).as_bytes()).expect("small edit"),
            );
            previous = state.clone();
        }
        journal
    }

    #[test]
    fn scattered_edits_insertions_and_deletions_round_trip_compactly() {
        let base = pseudo(300_000, 7);
        let mut states = vec![];
        let mut current = base.clone();
        for step in 0..20u8 {
            let mut next = current.clone();
            next[10] = step; // header-like counter
            let at = 1000 + usize::from(step) * 7919;
            next.splice(at..at, pseudo(37, u64::from(step) + 100)); // insertion
            next.drain(200_000..200_011); // deletion
            let tail = next.len() - 64;
            next[tail..].copy_from_slice(&pseudo(64, u64::from(step) + 900)); // signature
            states.push(next.clone());
            current = next;
        }
        let journal = journal_of(&base, &states);
        assert!(journal.len() < 20 * 1024, "journal {} bytes", journal.len());
        let replayed = replay(base, &journal, usize::MAX);
        assert_eq!(replayed.records, 20);
        assert_eq!(replayed.valid_len, journal.len() as u64);
        assert_eq!(replayed.state, current);
    }

    #[test]
    fn every_torn_tail_recovers_exactly_the_last_complete_record() {
        let base = pseudo(20_000, 3);
        let mut states = vec![];
        let mut current = base.clone();
        for step in 0..5u8 {
            current[500 + usize::from(step)] ^= 0xff;
            current.extend_from_slice(&pseudo(100, u64::from(step)));
            states.push(current.clone());
        }
        let journal = journal_of(&base, &states);
        // Boundary of each complete record.
        let mut ends = vec![HEADER_LEN];
        let mut offset = HEADER_LEN;
        while offset < journal.len() {
            let len = u32::from_le_bytes(journal[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4 + len;
            ends.push(offset);
        }
        for cut in 0..=journal.len() {
            let replayed = replay(base.clone(), &journal[..cut], usize::MAX);
            let complete = ends.iter().filter(|end| **end <= cut).count();
            if cut < HEADER_LEN {
                assert_eq!(replayed.valid_len, 0);
                assert_eq!(replayed.state, base);
                continue;
            }
            let records = complete - 1;
            assert_eq!(replayed.records, records, "cut {cut}");
            assert_eq!(replayed.valid_len, ends[records] as u64);
            let expected = if records == 0 {
                &base
            } else {
                &states[records - 1]
            };
            assert_eq!(&replayed.state, expected, "cut {cut}");
        }
    }

    #[test]
    fn corrupted_or_stale_journals_never_produce_an_unrecorded_state() {
        let base = pseudo(10_000, 11);
        let mut next = base.clone();
        next[42] ^= 1;
        let mut journal = journal_of(&base, &[next.clone()]);
        // Any single corrupted record byte stops replay at the checkpoint.
        for index in HEADER_LEN..journal.len() {
            journal[index] ^= 0x40;
            let replayed = replay(base.clone(), &journal, usize::MAX);
            assert!(
                replayed.state == base || replayed.state == next,
                "byte {index}"
            );
            if replayed.state == base {
                assert_eq!(replayed.valid_len, HEADER_LEN as u64);
            }
            journal[index] ^= 0x40;
        }
        // A journal bound to another checkpoint is ignored entirely.
        let other = pseudo(10_000, 12);
        let replayed = replay(other.clone(), &journal, usize::MAX);
        assert_eq!((replayed.state, replayed.valid_len), (other, 0));
        // The limit bounds reconstruction.
        assert_eq!(replay(base.clone(), &journal, 100).records, 0);
    }

    #[test]
    fn fast_replay_equals_checked_replay_for_every_cut_and_corruption() {
        let base = pseudo(20_000, 5);
        let mut states = vec![];
        let mut current = base.clone();
        for step in 0..6u8 {
            current[700 + usize::from(step)] ^= 0x5a;
            current.extend_from_slice(&pseudo(80, u64::from(step) + 40));
            states.push(current.clone());
        }
        let journal = journal_of(&base, &states);
        let digest = *blake3::hash(&base).as_bytes();
        let same = |journal: &[u8]| {
            let fast = replay_fast(base.clone(), &digest, journal, usize::MAX);
            let checked = replay_bound(base.clone(), &digest, journal, usize::MAX);
            assert_eq!(fast.state, checked.state);
            assert_eq!(fast.valid_len, checked.valid_len);
            assert_eq!(fast.records, checked.records);
        };
        for cut in 0..=journal.len() {
            same(&journal[..cut]);
        }
        let mut corrupted = journal.clone();
        for index in HEADER_LEN..journal.len() {
            corrupted[index] ^= 0x40;
            let fast = replay_fast(base.clone(), &digest, &corrupted, usize::MAX);
            // Never a state no writer recorded.
            assert!(
                fast.state == base || states.contains(&fast.state),
                "byte {index}"
            );
            corrupted[index] ^= 0x40;
        }
        same(&journal);
    }

    #[test]
    fn laned_block_hash_equals_the_rolling_hash() {
        let bytes = pseudo(4 * BLOCK, 21);
        let mut rolling = RollingHash::of(&bytes[..BLOCK]);
        for at in 0..3 * BLOCK {
            let mut serial = 0u64;
            for &byte in &bytes[at..at + BLOCK] {
                serial = serial.wrapping_mul(BASE).wrapping_add(u64::from(byte) + 1);
            }
            assert_eq!(RollingHash::of(&bytes[at..at + BLOCK]).value(), serial);
            assert_eq!(rolling.value(), serial, "offset {at}");
            rolling.roll(bytes[at], bytes[at + BLOCK]);
        }
        for (a, b, len) in [
            (&b"abcdefghij"[..], &b"abcdefghXj"[..], 8),
            (b"abc", b"abc", 3),
            (b"", b"x", 0),
        ] {
            assert_eq!(match_len(a, b), len);
        }
        assert_eq!(common_prefix_rev(b"xxabcdefghijk", b"yabcdefghijk"), 11);
        assert_eq!(common_prefix_rev(b"zabcdefghijk", b"yabcdefghijk"), 11);
        assert_eq!(common_prefix_rev(b"abc", b"abd"), 0);
    }

    #[test]
    fn unrelated_states_are_not_journaled() {
        let digest = |bytes: &[u8]| *blake3::hash(bytes).as_bytes();
        let unrelated = pseudo(10_000, 2);
        assert!(record(&pseudo(10_000, 1), &unrelated, &digest(&unrelated)).is_none());
        assert!(record(b"old", b"new", &digest(b"new")).is_none());
    }
}
