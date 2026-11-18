//! What a workspace's record became after its last snapshot: one entry per
//! publication, appended and synced before the publication is installed.
//!
//! An entry holds the bytes its publication changed and nothing else: the
//! record's own fields (as JSON) and its replica's durable encoding, each as
//! a delta of what the entry before left. A publication of one keystroke into
//! a record with a long history appends that keystroke, not the history, so
//! what a write costs does not grow with what the workspace already holds.
//!
//! Each entry is framed with its length and digest: an entry a crash cut
//! short fails its digest, and it and anything after it were never
//! acknowledged, so the journal is read up to it.
use crate::owner::stored::StoredWorkspace;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One run of the new bytes: a range of the old ones, or bytes of its own.
#[derive(Debug, Serialize, Deserialize)]
enum Op {
    Copy { from: u64, len: u64 },
    Bytes(#[serde(with = "serde_bytes")] Vec<u8>),
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    revision: u64,
    fields: Vec<Op>,
    peer: Vec<Op>,
}

/// What the next entry is a delta of: the record as the last entry left it.
pub(crate) struct Written {
    fields: Arc<[u8]>,
    peer: Vec<Arc<[u8]>>,
}

impl Written {
    /// Go on from `parts`, the replica restored from these bytes as it
    /// encodes itself: read back, the replica is one stretch of bytes no part
    /// of its own encoding shares, and a delta of that matches nothing by place.
    /// `false`, with nothing changed, when `parts` are other bytes.
    pub(super) fn resume(&mut self, parts: Vec<Arc<[u8]>>) -> bool {
        let parts = crate::owner::stored::DurablePeer::from(parts);
        if parts != crate::owner::stored::DurablePeer::from(self.peer.clone()) {
            return false;
        }
        self.peer = parts.parts().to_vec();
        true
    }
}

/// Length and digest ahead of each entry.
const FRAME_HEAD: usize = 4 + 32;

/// The record's own fields, without its replica: JSON, as the snapshot
/// holds them.
fn fields_of(stored: &StoredWorkspace) -> Result<Arc<[u8]>, String> {
    let fields = StoredWorkspace {
        peer: crate::owner::stored::DurablePeer::default(),
        ..stored.clone()
    };
    serde_json::to_vec(&fields)
        .map(Into::into)
        .map_err(|error| error.to_string())
}

/// What the first entry of a snapshot's journal is a delta of: `stored`,
/// whose snapshot is `snapshot`. Its fields are read back from the snapshot,
/// as a replay reads them.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn first(stored: &StoredWorkspace, snapshot: &[u8]) -> Result<Written, String> {
    let read: StoredWorkspace =
        serde_json::from_slice(snapshot).map_err(|error| error.to_string())?;
    Ok(Written {
        fields: fields_of(&read)?,
        peer: stored.peer.parts().to_vec(),
    })
}

/// What the next entry is a delta of, from the record itself: a store that
/// decodes the record before it keeps it needs no second read of its snapshot.
pub(super) fn first_of(stored: &StoredWorkspace) -> Result<Written, String> {
    Ok(Written {
        fields: fields_of(stored)?,
        peer: stored.peer.parts().to_vec(),
    })
}

/// The entry that takes `written` to `stored` at `revision`, and what the
/// entry after it is a delta of.
pub(super) fn next(
    written: &Written,
    stored: &StoredWorkspace,
    revision: u64,
) -> Result<(Vec<u8>, Written), String> {
    let fields = fields_of(stored)?;
    let peer = stored.peer.parts().to_vec();
    let entry = Entry {
        revision,
        fields: delta_chunked(&written.fields, &fields),
        peer: delta(&written.peer, &peer),
    };
    Ok((frame(&entry)?, Written { fields, peer }))
}

fn frame(entry: &Entry) -> Result<Vec<u8>, String> {
    let payload = postcard::to_allocvec(entry).map_err(|error| error.to_string())?;
    let len = u32::try_from(payload.len()).map_err(|_| "journal entry too large".to_owned())?;
    let mut framed = Vec::with_capacity(FRAME_HEAD.saturating_add(payload.len()));
    framed.extend_from_slice(&len.to_le_bytes());
    framed.extend_from_slice(blake3::hash(&payload).as_bytes());
    framed.extend_from_slice(&payload);
    Ok(framed)
}

/// `stored`, a snapshot as read, as the entries of its journal `log` leave it, and
/// the revision of the last entry, or `None` when the journal holds none.
/// Reading stops at the first entry that is cut short or does not match its
/// digest: no publication after it was acknowledged.
pub(super) fn replay(
    stored: StoredWorkspace,
    log: &[u8],
) -> Result<(StoredWorkspace, Option<u64>), String> {
    let mut fields = Rope::of(&fields_of(&stored)?);
    let mut peer = Rope::of(&stored.peer.to_vec().into());
    let mut revision = None;
    let mut rest = log;
    while let Some(entry) = take(&mut rest) {
        fields = fields.apply(entry.fields)?;
        peer = peer.apply(entry.peer)?;
        revision = Some(entry.revision);
    }
    if revision.is_none() {
        return Ok((stored, None));
    }
    let mut replayed: StoredWorkspace = serde_json::from_slice(&fields.to_vec())
        .map_err(|error| format!("the record's journal does not parse: {error}"))?;
    replayed.peer = peer.to_vec().into();
    Ok((replayed, revision))
}

/// Bytes as stretches of the buffers they came from: the snapshot's, and the
/// bytes of the entries after it. A replay takes each entry as stretches of
/// the bytes before it, so an entry costs what it changed, not a copy of
/// all the record holds; the bytes are put together once, at its end.
struct Rope {
    /// Each stretch: its buffer, and where in it the stretch starts and ends.
    stretches: Vec<(Arc<[u8]>, usize, usize)>,
    /// Where each stretch ends in the bytes.
    ends: Vec<u64>,
}

impl Rope {
    fn of(bytes: &Arc<[u8]>) -> Self {
        let mut rope = Self {
            stretches: Vec::new(),
            ends: Vec::new(),
        };
        rope.push(bytes, 0, bytes.len());
        rope
    }

    fn len(&self) -> u64 {
        self.ends.last().copied().unwrap_or(0)
    }

    /// Add `buffer[start..end]`, as a longer stretch where it goes on from the last one.
    fn push(&mut self, buffer: &Arc<[u8]>, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let len = end.saturating_sub(start) as u64;
        let total = self.len().saturating_add(len);
        if let Some((last, _, last_end)) = self.stretches.last_mut()
            && Arc::ptr_eq(last, buffer)
            && *last_end == start
        {
            *last_end = end;
            if let Some(at) = self.ends.last_mut() {
                *at = total;
            }
            return;
        }
        self.stretches.push((Arc::clone(buffer), start, end));
        self.ends.push(total);
    }

    /// These bytes as `ops` make the next ones of them.
    fn apply(&self, ops: Vec<Op>) -> Result<Self, String> {
        let mut out = Self {
            stretches: Vec::new(),
            ends: Vec::new(),
        };
        for op in ops {
            match op {
                Op::Copy { from, len } => {
                    let end = from
                        .checked_add(len)
                        .filter(|end| *end <= self.len())
                        .ok_or("a journal entry copies bytes the record does not hold")?;
                    let mut at = from;
                    let mut index = self.ends.partition_point(|stretch_end| *stretch_end <= at);
                    while at < end {
                        let (Some((buffer, start, stop)), Some(stretch_end)) =
                            (self.stretches.get(index), self.ends.get(index))
                        else {
                            return Err(
                                "a journal entry copies bytes the record does not hold".into()
                            );
                        };
                        let stretch_start =
                            stretch_end.saturating_sub(stop.saturating_sub(*start) as u64);
                        let skip = usize::try_from(at.saturating_sub(stretch_start))
                            .map_err(|e| e.to_string())?;
                        let take = usize::try_from(end.min(*stretch_end).saturating_sub(at))
                            .map_err(|e| e.to_string())?;
                        let from = start.saturating_add(skip);
                        out.push(buffer, from, from.saturating_add(take));
                        at = at.saturating_add(take as u64);
                        index = index.saturating_add(1);
                    }
                }
                Op::Bytes(bytes) => {
                    let len = bytes.len();
                    out.push(&Arc::from(bytes), 0, len);
                }
            }
        }
        Ok(out)
    }

    fn to_vec(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(usize::try_from(self.len()).unwrap_or(0));
        for (buffer, start, end) in &self.stretches {
            bytes.extend_from_slice(buffer.get(*start..*end).unwrap_or_default());
        }
        bytes
    }
}

/// Whether every byte of `log` is an entry: one a write cut short ends it
/// early, and an entry appended after that would never be read.
pub(super) fn read_whole(log: &[u8]) -> bool {
    let mut rest = log;
    while take(&mut rest).is_some() {}
    rest.is_empty()
}

/// The next whole entry of `rest`, advancing past it.
fn take(rest: &mut &[u8]) -> Option<Entry> {
    let len = usize::try_from(u32::from_le_bytes(rest.get(..4)?.try_into().ok()?)).ok()?;
    let digest = rest.get(4..FRAME_HEAD)?;
    let end = FRAME_HEAD.checked_add(len)?;
    let payload = rest.get(FRAME_HEAD..end)?;
    if blake3::hash(payload).as_bytes() != digest {
        return None;
    }
    let entry = postcard::from_bytes(payload).ok()?;
    *rest = rest.get(end..)?;
    Some(entry)
}

/// `new` as runs of `old`: a part that is an allocation of `old` is copied
/// from it; any other is matched against the part `old` held at its place,
/// so a part that grew or changed in its middle takes only what changed.
fn delta(old: &[Arc<[u8]>], new: &[Arc<[u8]>]) -> Vec<Op> {
    let mut offsets = Vec::with_capacity(old.len());
    let mut at = 0u64;
    for part in old {
        offsets.push(at);
        at = at.saturating_add(part.len() as u64);
    }
    // `old` keeps its parts alive, so no other allocation has their address.
    let by_address: std::collections::HashMap<(usize, usize), u64> = old
        .iter()
        .zip(&offsets)
        .filter(|(part, _)| !part.is_empty())
        .map(|(part, offset)| ((part.as_ptr() as usize, part.len()), *offset))
        .collect();
    let mut ops = Ops(Vec::new());
    for (index, part) in new.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if let Some(offset) = by_address.get(&(part.as_ptr() as usize, part.len())) {
            ops.copy(*offset, part.len());
            continue;
        }
        // The part at its place, counted from the start or, once a part
        // was added or taken out before it, from the end.
        let from_end = index
            .checked_add(old.len())
            .and_then(|at| at.checked_sub(new.len()));
        let alike = |at: usize| {
            let matched = matched(old.get(at).map_or(&[][..], |before| &**before), part);
            matched.prefix.saturating_add(matched.suffix)
        };
        let mut at = match from_end {
            Some(end) if end != index && alike(end) > alike(index) => end,
            _ => index,
        };
        // A part that joined parts kept apart before starts with them whole.
        let mut rest = &**part;
        while let Some((before, offset)) = old.get(at).zip(offsets.get(at)).filter(|(before, _)| {
            !before.is_empty() && before.len() < rest.len() && rest.starts_with(before)
        }) {
            ops.copy(*offset, before.len());
            rest = after(rest, before.len());
            at = at.saturating_add(1);
        }
        match old.get(at).zip(offsets.get(at)) {
            Some((before, offset)) => changed(&mut ops, before, *offset, rest),
            None => ops.bytes(rest),
        }
    }
    ops.0
}

/// Where content-defined chunks are cut: a rolling hash of the bytes before
/// a place has these bits clear, so a chunk ends where its own bytes say and
/// an edit moves the cuts near it only.
const CUT_MASK: u64 = 0xff00_0000_0000_0000;
/// The least and most a chunk holds.
const MIN_CHUNK: usize = 64;
const MAX_CHUNK: usize = 4096;

/// A table of 256 hashes for the rolling hash, made by splitmix64.
const GEAR: [u64; 256] = {
    let mut table = [0u64; 256];
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut index = 0;
    while index < 256 {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        #[allow(
            clippy::indexing_slicing,
            reason = "the index stays below the table's length"
        )]
        {
            table[index] = z ^ (z >> 31);
        }
        index += 1;
    }
    table
};

/// `bytes` cut into chunks by their content, as ranges.
fn chunks(bytes: &[u8]) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= bytes.len() {
            return None;
        }
        let rest = after(bytes, start);
        let mut hash = 0u64;
        let mut len = rest.len().min(MAX_CHUNK);
        for (at, byte) in rest.iter().take(MAX_CHUNK).enumerate() {
            let gear = GEAR.get(usize::from(*byte)).copied().unwrap_or_default();
            hash = (hash << 1).wrapping_add(gear);
            if at >= MIN_CHUNK && hash & CUT_MASK == 0 {
                len = at;
                break;
            }
        }
        let range = start..start.saturating_add(len);
        start = range.end;
        Some(range)
    })
}

/// `new` as stretches of `old`, one buffer each, matched a content-defined
/// chunk at a time: the workspace's own fields change in several places at
/// once (what this device sent, what its server answered), and each chunk in `old`
/// anywhere is copied, so a write carries the chunks it changed and not the
/// stretch between its first change and its last.
fn delta_chunked(old: &[u8], new: &[u8]) -> Vec<Op> {
    let known: std::collections::HashMap<&[u8], u64> = chunks(old)
        .filter_map(|range| Some((old.get(range.clone())?, range.start as u64)))
        .collect();
    let mut ops = Ops(Vec::new());
    for range in chunks(new) {
        let chunk = new.get(range).unwrap_or_default();
        match known.get(chunk) {
            Some(offset) => ops.copy(*offset, chunk.len()),
            None => ops.bytes(chunk),
        }
    }
    ops.0
}

/// The longest length prefix a part starts with: `postcard`'s count of what
/// follows, which changes with every entry a map or list gains.
const LENGTH_PREFIX: usize = 10;

/// `part` as runs of `before`, the part it took the place of, which starts
/// at `offset` of the old bytes: what both start and end with is copied.
/// Where their starts differ only in a count ahead of them, what follows
/// the counts is matched instead.
fn changed(ops: &mut Ops, before: &[u8], offset: u64, part: &[u8]) {
    let Matched {
        skipped,
        skip,
        prefix,
        suffix,
    } = matched(before, part);
    let rest = after(part, skip.saturating_add(prefix));
    ops.bytes(part.get(..skip).unwrap_or_default());
    ops.copy(offset.saturating_add(skipped as u64), prefix);
    ops.bytes(
        rest.get(..rest.len().saturating_sub(suffix))
            .unwrap_or_default(),
    );
    ops.copy(
        offset.saturating_add(before.len().saturating_sub(suffix) as u64),
        suffix,
    );
}

/// What `part` shares with `before`: past the first `skipped` bytes of
/// `before` and `skip` of `part` (counts ahead of what they hold), `prefix`
/// bytes, then `suffix` bytes at their ends.
struct Matched {
    skipped: usize,
    skip: usize,
    prefix: usize,
    suffix: usize,
}

fn matched(before: &[u8], part: &[u8]) -> Matched {
    let (mut skipped, mut skip, mut prefix) = (0, 0, common_prefix(before, part));
    if prefix < LENGTH_PREFIX {
        for from in 0..=LENGTH_PREFIX.min(before.len()) {
            for to in 0..=LENGTH_PREFIX.min(part.len()) {
                let matched = common_prefix(after(before, from), after(part, to));
                if matched > prefix {
                    (skipped, skip, prefix) = (from, to, matched);
                }
            }
        }
    }
    let suffix = common_suffix(
        after(before, skipped.saturating_add(prefix)),
        after(part, skip.saturating_add(prefix)),
    );
    Matched {
        skipped,
        skip,
        prefix,
        suffix,
    }
}

/// `bytes` but their last `cut`, empty when they are no longer.
fn before(bytes: &[u8], cut: usize) -> &[u8] {
    bytes
        .get(..bytes.len().saturating_sub(cut))
        .unwrap_or_default()
}

/// `bytes` from `from` on, empty past their end.
fn after(bytes: &[u8], from: usize) -> &[u8] {
    bytes.get(from..).unwrap_or_default()
}

struct Ops(Vec<Op>);

impl Ops {
    fn copy(&mut self, from: u64, len: usize) {
        if len == 0 {
            return;
        }
        let len = len as u64;
        if let Some(Op::Copy {
            from: last,
            len: run,
        }) = self.0.last_mut()
            && last.saturating_add(*run) == from
        {
            *run = run.saturating_add(len);
            return;
        }
        self.0.push(Op::Copy { from, len });
    }

    fn bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(Op::Bytes(run)) = self.0.last_mut() {
            run.extend_from_slice(bytes);
            return;
        }
        self.0.push(Op::Bytes(bytes.to_vec()));
    }
}

/// Compared a block at a time: a part is often hundreds of kilobytes.
const BLOCK: usize = 64;

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    let blocks = a
        .chunks_exact(BLOCK)
        .zip(b.chunks_exact(BLOCK))
        .take_while(|(a, b)| a == b)
        .count();
    let at = blocks.saturating_mul(BLOCK);
    let bytes = after(a, at)
        .iter()
        .zip(after(b, at))
        .take_while(|(a, b)| a == b)
        .count();
    at.saturating_add(bytes)
}

fn common_suffix(a: &[u8], b: &[u8]) -> usize {
    let blocks = a
        .rchunks_exact(BLOCK)
        .zip(b.rchunks_exact(BLOCK))
        .take_while(|(a, b)| a == b)
        .count();
    let at = blocks.saturating_mul(BLOCK);
    let bytes = before(a, at)
        .iter()
        .rev()
        .zip(before(b, at).iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    at.saturating_add(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(parts: &[&[u8]]) -> Vec<Arc<[u8]>> {
        parts.iter().map(|part| Arc::from(*part)).collect()
    }

    fn rebuilt(old: &[Arc<[u8]>], new: &[Arc<[u8]>]) -> (Vec<u8>, usize) {
        let ops = delta(old, new);
        let literal = ops
            .iter()
            .map(|op| match op {
                Op::Bytes(bytes) => bytes.len(),
                Op::Copy { .. } => 0,
            })
            .sum();
        // Applied over the old bytes kept as their parts, as a replay of
        // several entries keeps them.
        let mut rope = Rope::of(&Arc::from(&[][..]));
        for part in old {
            rope.push(part, 0, part.len());
        }
        (rope.apply(ops).unwrap().to_vec(), literal)
    }

    tilo_test_env::scenario! {
        #[allow(clippy::arithmetic_side_effects, reason = "test arithmetic in a phase's closure")]
        fn a_journal_delta_carries_what_changed_and_ends_at_a_torn_entry;

        phase a_delta_carries_only_what_changed;
        {
            let old = parts(&[b"head", &[7; 4096], b"middle-part", &[9; 4096], b"tail"]);
            let mut new = old.clone();
            new[0] = Arc::from(&b"head2"[..]);
            new[2] = Arc::from(&b"middle-part grew"[..]);
            new.push(Arc::from(&b"appended"[..]));
            let (bytes, literal) = rebuilt(&old, &new);
            assert_eq!(bytes, new.concat());
            assert!(literal <= 1 + 5 + 8, "{literal} literal bytes");
        }

        phase a_part_added_before_others_leaves_their_changes_small;
        {
            let old = parts(&[b"head", &[7; 4096], &[1; 300], b"tail"]);
            let mut grown = vec![2; 300];
            grown.push(3);
            let new = parts(&[b"head", &[7; 4096], b"added", &grown, b"tail"]);
            let (bytes, literal) = rebuilt(&old, &new);
            assert_eq!(bytes, new.concat());
            assert!(literal <= 5 + 1 + 300, "{literal} literal bytes");
            // The part after the added one is a fresh allocation that grew by one byte.
            let new = parts(&[b"head", &[7; 4096], b"added", &[1; 301], b"tail"]);
            let (bytes, literal) = rebuilt(&old, &new);
            assert_eq!(bytes, new.concat());
            assert!(literal <= 5 + 1 + 4, "{literal} literal bytes");
        }

        phase parts_joined_into_one_are_copied_from_where_they_were;
        {
            let old = parts(&[b"head", &[7; 4096], &[8; 2048], b"meta"]);
            let mut joined = [[7; 4096].as_slice(), &[8; 2048]].concat();
            joined.extend_from_slice(b"new");
            let new = parts(&[b"head", &joined, b"meta2"]);
            let (bytes, literal) = rebuilt(&old, &new);
            assert_eq!(bytes, new.concat());
            assert!(literal <= 3 + 1, "{literal} literal bytes");
        }

        phase a_list_that_grew_carries_its_count_and_its_new_entries;
        {
            let entries = |count: u8| {
                let mut bytes = vec![count];
                for entry in 0..count {
                    bytes.extend_from_slice(&[entry; 100]);
                }
                bytes.extend_from_slice(&[count; 8]);
                bytes
            };
            let old = vec![Arc::from(entries(20))];
            let new = vec![Arc::from(entries(21))];
            let (bytes, literal) = rebuilt(&old, &new);
            assert_eq!(bytes, new.concat());
            assert!(literal <= 1 + 100 + 8, "{literal} literal bytes");
        }

        phase a_delta_of_unrelated_parts_still_rebuilds_them;
        {
            let old = parts(&[b"abc", b"defgh"]);
            let new = parts(&[b"xbz", b"", b"defgh", b"q"]);
            assert_eq!(rebuilt(&old, &new).0, new.concat());
            assert_eq!(rebuilt(&[], &new).0, new.concat());
            assert_eq!(rebuilt(&new, &[]).0, Vec::<u8>::new());
        }

        phase a_torn_entry_ends_the_journal;
        {
            let entry = |revision| {
                frame(&Entry {
                    revision,
                    fields: Vec::new(),
                    peer: vec![Op::Bytes(revision.to_le_bytes().to_vec())],
                })
                .unwrap()
            };
            let mut log = [entry(1), entry(2)].concat();
            let whole = log.len();
            log.extend_from_slice(&entry(3)[..10]);
            let mut rest = &log[..];
            let revisions: Vec<u64> = std::iter::from_fn(|| take(&mut rest))
                .map(|entry| entry.revision)
                .collect();
            assert_eq!(revisions, [1, 2]);
            assert_eq!(rest.len(), log.len() - whole);
            let mut corrupt = entry(4);
            *corrupt.last_mut().unwrap() ^= 1;
            assert!(take(&mut &corrupt[..]).is_none());
        }
    }
}
