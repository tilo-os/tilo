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
use crate::owner::record::StoredWorkspace;
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
pub(super) struct Written {
    fields: Arc<[u8]>,
    peer: Vec<Arc<[u8]>>,
}

/// Length and digest ahead of each entry.
const FRAME_HEAD: usize = 4 + 32;

/// The record's own fields, without its replica: JSON, as the snapshot
/// holds them.
fn fields_of(stored: &StoredWorkspace) -> Result<Arc<[u8]>, String> {
    let fields = StoredWorkspace {
        peer: crate::owner::record::DurablePeer::default(),
        ..stored.clone()
    };
    serde_json::to_vec(&fields)
        .map(Into::into)
        .map_err(|error| error.to_string())
}

/// What the first entry of a snapshot's journal is a delta of: `stored`,
/// whose snapshot is `snapshot`. Its fields are read back from the snapshot,
/// as a replay reads them.
pub(super) fn first(stored: &StoredWorkspace, snapshot: &[u8]) -> Result<Written, String> {
    let read: StoredWorkspace =
        serde_json::from_slice(snapshot).map_err(|error| error.to_string())?;
    Ok(Written {
        fields: fields_of(&read)?,
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
        fields: delta(
            std::slice::from_ref(&written.fields),
            std::slice::from_ref(&fields),
        ),
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
    cap: usize,
) -> Result<(StoredWorkspace, Option<u64>), String> {
    let mut fields = fields_of(&stored)?.to_vec();
    let mut peer = stored.peer.to_vec();
    let mut revision = None;
    let mut rest = log;
    while let Some(entry) = take(&mut rest) {
        fields = apply(&fields, &entry.fields, cap)?;
        peer = apply(&peer, &entry.peer, cap)?;
        revision = Some(entry.revision);
    }
    if revision.is_none() {
        return Ok((stored, None));
    }
    let mut replayed: StoredWorkspace = serde_json::from_slice(&fields)
        .map_err(|error| format!("the record's journal does not parse: {error}"))?;
    replayed.peer = peer.into();
    Ok((replayed, revision))
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

fn apply(old: &[u8], ops: &[Op], cap: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for op in ops {
        match op {
            Op::Copy { from, len } => {
                let range = usize::try_from(*from)
                    .ok()
                    .zip(usize::try_from(*len).ok())
                    .and_then(|(from, len)| old.get(from..from.checked_add(len)?))
                    .ok_or("a journal entry copies bytes the record does not hold")?;
                out.extend_from_slice(range);
            }
            Op::Bytes(bytes) => out.extend_from_slice(bytes),
        }
        if out.len() > cap {
            return Err("joined-workspace record exceeds its cap".into());
        }
    }
    Ok(out)
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
        (apply(&old.concat(), &ops, usize::MAX).unwrap(), literal)
    }

    #[test]
    fn a_delta_carries_only_what_changed() {
        let old = parts(&[b"head", &[7; 4096], b"middle-part", &[9; 4096], b"tail"]);
        let mut new = old.clone();
        new[0] = Arc::from(&b"head2"[..]);
        new[2] = Arc::from(&b"middle-part grew"[..]);
        new.push(Arc::from(&b"appended"[..]));
        let (bytes, literal) = rebuilt(&old, &new);
        assert_eq!(bytes, new.concat());
        assert!(literal <= 1 + 5 + 8, "{literal} literal bytes");
    }

    #[test]
    fn a_part_added_before_others_leaves_their_changes_small() {
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

    #[test]
    fn parts_joined_into_one_are_copied_from_where_they_were() {
        let old = parts(&[b"head", &[7; 4096], &[8; 2048], b"meta"]);
        let mut joined = [[7; 4096].as_slice(), &[8; 2048]].concat();
        joined.extend_from_slice(b"new");
        let new = parts(&[b"head", &joined, b"meta2"]);
        let (bytes, literal) = rebuilt(&old, &new);
        assert_eq!(bytes, new.concat());
        assert!(literal <= 3 + 1, "{literal} literal bytes");
    }

    #[test]
    fn a_list_that_grew_carries_its_count_and_its_new_entries() {
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

    #[test]
    fn a_delta_of_unrelated_parts_still_rebuilds_them() {
        let old = parts(&[b"abc", b"defgh"]);
        let new = parts(&[b"xbz", b"", b"defgh", b"q"]);
        assert_eq!(rebuilt(&old, &new).0, new.concat());
        assert_eq!(rebuilt(&[], &new).0, new.concat());
        assert_eq!(rebuilt(&new, &[]).0, Vec::<u8>::new());
    }

    #[test]
    fn a_torn_entry_ends_the_journal() {
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
