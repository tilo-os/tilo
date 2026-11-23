#![cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the journal is the host's own file record")
)]
//! What a workspace's record became after its last snapshot: one entry per
//! publication, appended and synced before the publication is installed.
//!
//! An entry holds what its publication changed and nothing else: the
//! workspace's own fields (as JSON), the server's answers that changed, the
//! writes its server took or was given to take, ownership's lists of writes as
//! stretches of the writes they held before, and of its replica's durable encoding
//! the stretches before and after the documents and each document it changed,
//! each as a delta of what the entry before left. The answers, the writes not
//! taken and the documents are maps whose copies share what neither changed,
//! so telling what a publication changed skips everything it did not: a
//! publication of one keystroke into one document of thousands appends that
//! keystroke, and an answer to a write the write it answered, however much the
//! workspace already holds.
//!
//! Each entry is framed with its length and digest: an entry a crash cut
//! short fails its digest, and it and anything after it were never
//! acknowledged, so the journal is read up to it.
use crate::ordered::Ordered;
use crate::owner::answers::AnswerSignature;
use crate::owner::publication::{Answer, Journaled, StoredOutgoing};
use crate::owner::stored::{DurablePeer, StoredWorkspace};
use crate::state::{DocumentParts, DurableParts, Route};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    /// The workspace's fields, without its answers and its replica.
    fields: Vec<Op>,
    /// Each document whose answers changed, with what it holds now.
    answered: Vec<((String, String), Option<JournalAnswer>)>,
    /// Each write whose server's answer changed: `true` once it waits for one, `false` once taken.
    unanswered: Vec<(AnswerSignature, bool)>,
    /// Ownership's governance, head and tail, each as stretches of the writes the three held before.
    writes: [Vec<Stretch>; 3],
    /// The replica's stretches before its documents.
    head: Vec<Op>,
    /// Each document that changed, with its stretches now, or `None` when
    /// it is no longer held.
    documents: Vec<(Route, Option<DocumentOps>)>,
    /// The replica's stretches after its documents.
    tail: Vec<Op>,
}

/// A document's answers as an entry keeps them, every field written: postcard reads a field
/// whether or not it was written, and [`Answer`] leaves out an order it has none of, which only its
/// JSON can do without.
#[derive(Debug, Serialize, Deserialize)]
struct JournalAnswer {
    durable: u64,
    refused: u64,
    last_order: Option<u64>,
}

impl From<Answer> for JournalAnswer {
    fn from(answer: Answer) -> Self {
        let (durable, refused, last_order) = answer.parts();
        Self {
            durable,
            refused,
            last_order,
        }
    }
}

impl From<JournalAnswer> for Answer {
    fn from(answer: JournalAnswer) -> Self {
        Self::from_parts(answer.durable, answer.refused, answer.last_order)
    }
}

/// One stretch of a list of ownership's writes: writes the lists held before, counted over the
/// three of them in turn, or writes of its own.
#[derive(Debug, Serialize, Deserialize)]
enum Stretch {
    Kept { from: u32, len: u32 },
    Writes(Vec<StoredOutgoing>),
}

/// A document's stretches as a delta of what it held before.
#[derive(Debug, Serialize, Deserialize)]
struct DocumentOps {
    document: Vec<Op>,
    sealed: Option<Vec<Op>>,
}

/// What the next entry is a delta of: the record as the last entry left it.
pub(crate) struct Written {
    fields: Arc<[u8]>,
    journaled: Journaled,
    peer: DurableParts,
}

impl Written {
    /// Go on from `parts`, the replica restored from these bytes as it
    /// encodes itself: read back, each stretch of the replica is one part no
    /// part of its own encoding shares, and a delta of that matches nothing
    /// by place. `false`, with nothing changed, when `parts` are other bytes.
    pub(super) fn resume(&mut self, parts: DurableParts) -> bool {
        if parts != self.peer {
            return false;
        }
        self.peer = parts;
        true
    }
}

/// Length and digest ahead of each entry.
const FRAME_HEAD: usize = 4 + 32;

/// The workspace's own fields as JSON, as the snapshot holds them, without its replica and what
/// [`Journaled`] carries apart: those as well.
fn split(stored: &StoredWorkspace) -> Result<(Arc<[u8]>, Journaled), String> {
    let mut fields = StoredWorkspace {
        peer: DurablePeer::default(),
        ..stored.clone()
    };
    let journaled = fields
        .publication
        .as_mut()
        .map(crate::owner::publication::StoredPublication::take_journaled)
        .unwrap_or_default();
    let fields = serde_json::to_vec(&fields).map_err(|error| error.to_string())?;
    Ok((fields.into(), journaled))
}

/// What the first entry of a snapshot's journal is a delta of: `stored`,
/// whose snapshot is `snapshot`. Its fields are read back from the snapshot,
/// as a replay reads them.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn first(stored: &StoredWorkspace, snapshot: &[u8]) -> Result<Written, String> {
    let read: StoredWorkspace =
        serde_json::from_slice(snapshot).map_err(|error| error.to_string())?;
    let (fields, journaled) = split(&read)?;
    Ok(Written {
        fields,
        journaled,
        peer: stored.peer.layout().clone(),
    })
}

/// What the next entry is a delta of, from the record itself: a store that
/// decodes the record before it keeps it needs no second read of its snapshot.
pub(super) fn first_of(stored: &StoredWorkspace) -> Result<Written, String> {
    let (fields, journaled) = split(stored)?;
    Ok(Written {
        fields,
        journaled,
        peer: stored.peer.layout().clone(),
    })
}

/// The entry that takes `written` to `stored` at `revision`, and what the
/// entry after it is a delta of.
pub(super) fn next(
    written: &Written,
    stored: &StoredWorkspace,
    revision: u64,
) -> Result<(Vec<u8>, Written), String> {
    let (fields, journaled) = split(stored)?;
    let peer = stored.peer.layout().clone();
    let empty: Arc<[Arc<[u8]>]> = Arc::from([]);
    let documents = Ordered::changes(
        &written.peer.documents,
        &peer.documents,
        DocumentParts::same,
    )
    .into_iter()
    .map(|change| {
        let ops = change.after.map(|now| {
            let before = change.before;
            DocumentOps {
                document: delta(
                    before.map_or(&empty, |before| &before.document),
                    &now.document,
                ),
                sealed: now.sealed.as_ref().map(|sealed| {
                    delta(
                        before
                            .and_then(|before| before.sealed.as_ref())
                            .unwrap_or(&empty),
                        sealed,
                    )
                }),
            }
        });
        (change.key.clone(), ops)
    })
    .collect();
    let entry = Entry {
        revision,
        fields: delta_chunked(&written.fields, &fields),
        answered: Ordered::changes(&written.journaled.answered, &journaled.answered, |a, b| {
            a == b
        })
        .into_iter()
        .map(|change| {
            (
                change.key.clone(),
                change.after.copied().map(JournalAnswer::from),
            )
        })
        .collect(),
        unanswered: Ordered::changes(
            &written.journaled.unanswered,
            &journaled.unanswered,
            |(), ()| true,
        )
        .into_iter()
        .map(|change| (change.key.clone(), change.after.is_some()))
        .collect(),
        writes: stretches(&written.journaled.writes, &journaled.writes),
        head: delta(&written.peer.head, &peer.head),
        documents,
        tail: delta(&written.peer.tail, &peer.tail),
    };
    Ok((
        frame(&entry)?,
        Written {
            fields,
            journaled,
            peer,
        },
    ))
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
    let mut rest = log;
    replay_entries(stored, std::iter::from_fn(|| take(&mut rest).transpose()))
}

/// `stored`, a snapshot as read, as `entries` leave it, and the revision of the last one.
fn replay_entries(
    stored: StoredWorkspace,
    entries: impl Iterator<Item = Result<Entry, String>>,
) -> Result<(StoredWorkspace, Option<u64>), String> {
    let rope = |parts: &[Arc<[u8]>]| Rope::of(&parts.concat().into());
    let (fields, mut journaled) = split(&stored)?;
    let mut fields = Rope::of(&fields);
    let layout = stored.peer.layout();
    let mut head = rope(&layout.head);
    let mut tail = rope(&layout.tail);
    let mut documents: BTreeMap<Route, (Rope, Option<Rope>)> = layout
        .documents
        .iter()
        .map(|(route, parts)| {
            (
                route.clone(),
                (rope(&parts.document), parts.sealed.as_deref().map(rope)),
            )
        })
        .collect();
    let mut revision = None;
    for entry in entries {
        let entry = entry?;
        fields = fields.apply(entry.fields)?;
        for (key, answer) in entry.answered {
            match answer {
                Some(answer) => journaled.answered.insert(key, answer.into()),
                None => journaled.answered.remove(&key),
            };
        }
        for (signature, waits) in entry.unanswered {
            if waits {
                journaled.unanswered.insert(signature, ());
            } else {
                journaled.unanswered.remove(&signature);
            }
        }
        journaled.writes = replay_stretches(&journaled.writes, entry.writes)?;
        head = head.apply(entry.head)?;
        tail = tail.apply(entry.tail)?;
        for (route, ops) in entry.documents {
            let Some(ops) = ops else {
                documents.remove(&route);
                continue;
            };
            let empty = Rope::of(&Arc::from([]));
            let (document, sealed) = documents
                .get(&route)
                .map_or((&empty, None), |(document, sealed)| {
                    (document, sealed.as_ref())
                });
            let next = (
                document.apply(ops.document)?,
                ops.sealed
                    .map(|ops| sealed.unwrap_or(&empty).apply(ops))
                    .transpose()?,
            );
            documents.insert(route, next);
        }
        revision = Some(entry.revision);
    }
    if revision.is_none() {
        return Ok((stored, None));
    }
    let mut replayed: StoredWorkspace = serde_json::from_slice(&fields.to_vec())
        .map_err(|error| format!("the record's journal does not parse: {error}"))?;
    if let Some(publication) = replayed.publication.as_mut() {
        publication.set_journaled(journaled);
    }
    let whole = |rope: &Rope| -> Arc<[Arc<[u8]>]> { Arc::from([Arc::from(rope.to_vec())]) };
    replayed.peer = DurableParts {
        head: whole(&head),
        documents: documents
            .iter()
            .map(|(route, (document, sealed))| {
                (
                    route.clone(),
                    DocumentParts {
                        document: whole(document),
                        sealed: sealed.as_ref().map(whole),
                    },
                )
            })
            .collect(),
        tail: whole(&tail),
    }
    .into();
    Ok((replayed, revision))
}

/// `new`, ownership's lists of writes, as stretches of `old`, the lists before: a write the lists held
/// as it is now is named by its place among them, so a publication that answered or added a few
/// writes carries those and not every write ownership still holds.
fn stretches(old: &[Vec<StoredOutgoing>; 3], new: &[Vec<StoredOutgoing>; 3]) -> [Vec<Stretch>; 3] {
    let held: std::collections::HashMap<[u8; 32], usize> = old
        .iter()
        .flatten()
        .enumerate()
        .map(|(at, write)| (write.id, at))
        .collect();
    let old: Vec<&StoredOutgoing> = old.iter().flatten().collect();
    new.each_ref().map(|list| {
        let mut stretches: Vec<Stretch> = Vec::new();
        for write in list {
            let kept = held
                .get(&write.id)
                .copied()
                .filter(|at| old.get(*at).is_some_and(|before| *before == write))
                .and_then(|at| u32::try_from(at).ok());
            match (stretches.last_mut(), kept) {
                (Some(Stretch::Kept { from, len }), Some(at))
                    if from.checked_add(*len) == Some(at) =>
                {
                    *len = len.saturating_add(1);
                }
                (_, Some(at)) => stretches.push(Stretch::Kept { from: at, len: 1 }),
                (Some(Stretch::Writes(writes)), None) => writes.push(write.clone()),
                (_, None) => stretches.push(Stretch::Writes(vec![write.clone()])),
            }
        }
        stretches
    })
}

/// The lists `stretches` make of `old`, the lists before ([`stretches()`]).
fn replay_stretches(
    old: &[Vec<StoredOutgoing>; 3],
    stretches: [Vec<Stretch>; 3],
) -> Result<[Vec<StoredOutgoing>; 3], String> {
    let old: Vec<&StoredOutgoing> = old.iter().flatten().collect();
    let mut lists: [Vec<StoredOutgoing>; 3] = Default::default();
    for (list, stretches) in lists.iter_mut().zip(stretches) {
        for stretch in stretches {
            match stretch {
                Stretch::Kept { from, len } => {
                    let from = usize::try_from(from).map_err(|e| e.to_string())?;
                    let end = from.saturating_add(usize::try_from(len).map_err(|e| e.to_string())?);
                    let kept = old
                        .get(from..end)
                        .ok_or("a journal entry keeps writes ownership did not hold")?;
                    list.extend(kept.iter().map(|write| (*write).clone()));
                }
                Stretch::Writes(writes) => list.extend(writes),
            }
        }
    }
    Ok(lists)
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
    while let Some(payload) = whole_payload(&rest) {
        rest = rest
            .get(FRAME_HEAD.saturating_add(payload.len())..)
            .unwrap_or_default();
    }
    rest.is_empty()
}

/// The next whole entry of `rest`, advancing past it, or `None` at one a crash cut short.
///
/// # Errors
/// An entry that is whole and does not read as this build's layout: a build that appended another
/// layout wrote it, and reading on past it would drop every publication after it without a word.
fn take(rest: &mut &[u8]) -> Result<Option<Entry>, String> {
    let Some(payload) = whole_payload(rest) else {
        return Ok(None);
    };
    let Ok((entry, [])) = postcard::take_from_bytes::<Entry>(payload) else {
        return Err(
            "the workspace's journal holds an entry of a layout this build does not append".into(),
        );
    };
    *rest = rest
        .get(FRAME_HEAD.saturating_add(payload.len())..)
        .unwrap_or_default();
    Ok(Some(entry))
}

/// The payload of the next entry of `rest` when it is whole and matches its digest.
fn whole_payload<'a>(rest: &&'a [u8]) -> Option<&'a [u8]> {
    let len = usize::try_from(u32::from_le_bytes(rest.get(..4)?.try_into().ok()?)).ok()?;
    let digest = rest.get(4..FRAME_HEAD)?;
    let end = FRAME_HEAD.checked_add(len)?;
    let payload = rest.get(FRAME_HEAD..end)?;
    (blake3::hash(payload).as_bytes() == digest).then_some(payload)
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

        phase ownership_carries_the_writes_that_changed_and_names_the_rest;
        {
            let write = |id: u8| StoredOutgoing::sample(id, vec![id; 1000]);
            let lists = |governance: &[u8], head: &[u8], tail: &[u8]| {
                [governance, head, tail].map(|ids| ids.iter().map(|id| write(*id)).collect::<Vec<_>>())
            };
            let carried = |entry: &[Vec<Stretch>; 3]| -> usize {
                entry.iter()
                    .flatten()
                    .map(|stretch| match stretch {
                        Stretch::Writes(writes) => writes.len(),
                        Stretch::Kept { .. } => 0,
                    })
                    .sum()
            };
            let old = lists(&[1], &[2, 3, 4, 5], &[6, 7]);
            // The server answered the head's first two, the tail moved up to the head, and a write
            // the head did not hold before (one read back from a kept stretch) followed it.
            let new = lists(&[1], &[4, 5, 6, 7, 20], &[21]);
            let entry = stretches(&old, &new);
            assert_eq!(carried(&entry), 2, "{entry:?}");
            assert_eq!(replay_stretches(&old, entry).unwrap(), new);
            // A renewal carries the change of the write it renews behind every other one, and a
            // compaction drafted in its place keeps its name with other bytes.
            let mut new = lists(&[1], &[2, 4, 5], &[6, 7, 30]);
            new[1][0].bytes = vec![0; 10];
            let entry = stretches(&old, &new);
            assert_eq!(carried(&entry), 2, "{entry:?}");
            assert_eq!(replay_stretches(&old, entry).unwrap(), new);
            // Nothing changed carries nothing, and every list emptied rebuilds as empty.
            assert_eq!(carried(&stretches(&old, &old)), 0);
            assert_eq!(replay_stretches(&old, stretches(&old, &old)).unwrap(), old);
            let empty: [Vec<StoredOutgoing>; 3] = Default::default();
            assert_eq!(replay_stretches(&old, stretches(&old, &empty)).unwrap(), empty);
            // An entry that names writes the lists never held is refused.
            assert!(replay_stretches(&empty, stretches(&old, &old)).is_err());
        }

        phase a_torn_entry_ends_the_journal;
        {
            let entry = |revision| {
                frame(&Entry {
                    revision,
                    fields: Vec::new(),
                    answered: Vec::new(),
                    unanswered: Vec::new(),
                    writes: Default::default(),
                    head: vec![Op::Bytes(revision.to_le_bytes().to_vec())],
                    documents: Vec::new(),
                    tail: Vec::new(),
                })
                .unwrap()
            };
            let mut log = [entry(1), entry(2)].concat();
            let whole = log.len();
            log.extend_from_slice(&entry(3)[..10]);
            let mut rest = &log[..];
            let revisions: Vec<u64> = std::iter::from_fn(|| take(&mut rest).unwrap())
                .map(|entry| entry.revision)
                .collect();
            assert_eq!(revisions, [1, 2]);
            assert_eq!(rest.len(), log.len() - whole);
            let mut corrupt = entry(4);
            *corrupt.last_mut().unwrap() ^= 1;
            assert!(take(&mut &corrupt[..]).unwrap().is_none());
        }

        phase an_answer_without_an_order_reads_back_whole;
        // A document whose writes were only refused has no order to keep: the entry still writes
        // every field, so it and every entry after it read back.
        {
            let entry = |revision: u64| Entry {
                revision,
                fields: Vec::new(),
                answered: vec![
                    (("so.tilo.chat".into(), "a".into()), Some(Answer::from_parts(0, 1, None).into())),
                    (("so.tilo.chat".into(), "b".into()), Some(Answer::from_parts(2, 0, Some(9)).into())),
                ],
                unanswered: Vec::new(),
                writes: Default::default(),
                head: Vec::new(),
                documents: Vec::new(),
                tail: Vec::new(),
            };
            let log = [frame(&entry(1)).unwrap(), frame(&entry(2)).unwrap()].concat();
            let mut rest = &log[..];
            let read: Vec<Entry> = std::iter::from_fn(|| take(&mut rest).unwrap()).collect();
            assert_eq!(read.len(), 2);
            let answers: Vec<Option<Answer>> = read[1]
                .answered
                .iter()
                .map(|(_, answer)| answer.as_ref().map(|answer| Answer::from_parts(answer.durable, answer.refused, answer.last_order)))
                .collect();
            assert_eq!(
                answers,
                [Some(Answer::from_parts(0, 1, None)), Some(Answer::from_parts(2, 0, Some(9)))]
            );
        }

        phase a_whole_entry_of_another_layout_is_refused_not_skipped;
        // An entry that is whole and reads as no entry of this build's was appended by a build of
        // another layout: reading on past it would drop what came after without a word.
        {
            let payload = postcard::to_allocvec(&(1_u64, "not an entry")).unwrap();
            let mut framed = u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
            framed.extend_from_slice(blake3::hash(&payload).as_bytes());
            framed.extend_from_slice(&payload);
            assert!(take(&mut &framed[..]).is_err());
        }
    }
}
