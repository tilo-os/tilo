#![cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the journal is the host's own file record")
)]
//! What a workspace's record became after its last snapshot: one entry per
//! publication, appended and synced before the publication is installed.
//!
//! An entry holds what its publication changed and nothing else: the
//! workspace's own fields (as JSON), the frames this device keeps until a link takes
//! them as stretches of the frames it kept before, and of its replica's durable encoding
//! the stretches before and after the documents and each document it changed,
//! each as a delta of what the entry before left. The documents are a map whose
//! copies share what neither changed, so telling what a publication changed skips
//! everything it did not: a publication of one keystroke into one document of
//! thousands appends that keystroke, however much the workspace already holds.
//!
//! Each entry is framed with its length and digest: an entry a crash cut
//! short fails its digest, and it and anything after it were never
//! acknowledged, so the journal is read up to it.
use crate::ordered::Ordered;
use crate::owner::stored::{DurablePeer, FrameToSend, Frames, StoredWorkspace, WORKSPACE_VERSION};
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
    /// The frames this device keeps until a link takes them, as stretches of the frames it kept
    /// before: a device offline keeps every write it makes, and a write carries its own frame.
    frames: Vec<FrameStretch>,
    /// The replica's stretches before its documents.
    head: Vec<Op>,
    /// Each document that changed, with its stretches now, or `None` when
    /// it is no longer held.
    documents: Vec<(Route, Option<DocumentOps>)>,
    /// The replica's stretches after its documents.
    tail: Vec<Op>,
}

/// An entry a version 10 build appended: [`Entry`], with ownership's answers and lists of writes
/// between its fields and its frames. Those go with the ownership the step to version 11 drops
/// ([`earlier`]), and are read only to reach what follows them.
#[derive(Deserialize)]
struct PreviousEntry {
    revision: u64,
    fields: Vec<Op>,
    #[serde(rename = "answered")]
    _answered: Vec<earlier::Answered>,
    #[serde(rename = "unanswered")]
    _unanswered: Vec<earlier::Unanswered>,
    writes: [Vec<earlier::Stretch>; 3],
    frames: Vec<FrameStretch>,
    head: Vec<Op>,
    documents: Vec<(Route, Option<DocumentOps>)>,
    tail: Vec<Op>,
}

impl From<PreviousEntry> for Entry {
    fn from(entry: PreviousEntry) -> Self {
        Self {
            revision: entry.revision,
            fields: entry.fields,
            frames: entry.frames,
            head: entry.head,
            documents: entry.documents,
            tail: entry.tail,
        }
    }
}

/// An entry a version 8 or 9 build appended: [`PreviousEntry`] before the frames a device keeps
/// went apart from the workspace's fields, so it carries none of its own; its fields hold them.
#[derive(Deserialize)]
struct EntryBeforeFrames {
    revision: u64,
    fields: Vec<Op>,
    #[serde(rename = "answered")]
    _answered: Vec<earlier::Answered>,
    #[serde(rename = "unanswered")]
    _unanswered: Vec<earlier::Unanswered>,
    writes: [Vec<earlier::Stretch>; 3],
    head: Vec<Op>,
    documents: Vec<(Route, Option<DocumentOps>)>,
    tail: Vec<Op>,
}

impl From<EntryBeforeFrames> for Entry {
    fn from(entry: EntryBeforeFrames) -> Self {
        Self {
            revision: entry.revision,
            fields: entry.fields,
            frames: Vec::new(),
            head: entry.head,
            documents: entry.documents,
            tail: entry.tail,
        }
    }
}

/// An entry a version 7 build appended: the whole of the workspace's fields, ownership's lists of
/// writes among them, and the answers its server gave apart from them.
#[derive(Deserialize)]
struct EntryOfFields {
    revision: u64,
    fields: Vec<Op>,
    #[serde(rename = "answered")]
    _answered: Vec<earlier::Answered>,
    head: Vec<Op>,
    documents: Vec<(Route, Option<DocumentOps>)>,
    tail: Vec<Op>,
}

impl From<EntryOfFields> for Entry {
    fn from(entry: EntryOfFields) -> Self {
        Self {
            revision: entry.revision,
            fields: entry.fields,
            frames: Vec::new(),
            head: entry.head,
            documents: entry.documents,
            tail: entry.tail,
        }
    }
}

/// One stretch of the frames a device keeps: frames it kept before, or frames of its own.
#[derive(Debug, Serialize, Deserialize)]
enum FrameStretch {
    Kept { from: u32, len: u32 },
    Frames(Vec<Vec<u8>>),
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
    frames: Frames,
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

/// The entry version whose journal this build reads: an entry is a delta of the snapshot it was
/// appended beside, and a journal of another version is refused.
pub(crate) const LAYOUT_VERSION: u32 = 11;

/// The layout of the build before this one, whose journal this build replays once: the next
/// publication writes a snapshot of this build's layout first.
pub(crate) const PREVIOUS_LAYOUT: u32 = 10;

/// The layout of a journal whose entries carry ownership's lists of writes apart from the
/// workspace's fields, the frames a device keeps among them ([`EntryBeforeFrames`]). Version 9
/// changed no stored byte of version 8's.
pub(crate) const WRITES_APART: u32 = 8;

/// The oldest layout of a journal this build replays: one a version 7 build appended, the released
/// one, each entry a delta of every field that build kept ([`EntryOfFields`]). A device on that
/// build keeps there what it wrote since its last snapshot.
pub(crate) const OLDEST_LAYOUT: u32 = 7;

/// What an entry carries of a record: its fields and the frames it keeps.
struct Split {
    fields: Arc<[u8]>,
    frames: Frames,
}

/// The workspace's own fields as JSON, as the snapshot holds them, without its replica and the
/// frames it carries apart: those as well.
fn split(stored: &StoredWorkspace) -> Result<Split, String> {
    split_as(stored, stored.version)
}

/// [`split`], the fields naming `version`.
fn split_as(stored: &StoredWorkspace, version: u32) -> Result<Split, String> {
    // The record's own fields alone: its replica and frames, most of what it holds, are carried
    // apart and not copied here.
    let fields = StoredWorkspace {
        version,
        workspace: stored.workspace,
        workspace_host: stored.workspace_host.clone(),
        device: stored.device.clone(),
        enrollment: stored.enrollment.clone(),
        vouched: stored.vouched.clone(),
        host_hint: stored.host_hint.clone(),
        peer: DurablePeer::default(),
        generation: stored.generation,
        continuity: stored.continuity,
        frames: Frames::default(),
    };
    let frames = stored.frames.clone();
    let fields = serde_json::to_vec(&fields).map_err(|error| error.to_string())?;
    Ok(Split {
        fields: fields.into(),
        frames,
    })
}

/// What the first entry of a snapshot's journal is a delta of: `stored`,
/// whose snapshot is `snapshot`. Its fields are read back from the snapshot,
/// as a replay reads them; its frames are the record's own, the bytes the
/// snapshot holds, so the next entry finds them as the record keeps them.
pub(super) fn first(stored: &StoredWorkspace, snapshot: &[u8]) -> Result<Written, String> {
    let read: StoredWorkspace =
        serde_json::from_slice(snapshot).map_err(|error| error.to_string())?;
    let Split { fields, .. } = split(&read)?;
    Ok(Written {
        fields,
        frames: stored.frames.clone(),
        peer: stored.peer.layout().clone(),
    })
}

/// What the next entry is a delta of, from the record itself: a store that
/// decodes the record before it keeps it needs no second read of its snapshot.
pub(super) fn first_of(stored: &StoredWorkspace) -> Result<Written, String> {
    let Split { fields, frames } = split(stored)?;
    Ok(Written {
        fields,
        frames,
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
    let Split { fields, frames } = split(stored)?;
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
        frames: frame_stretches(&written.frames, &frames),
        head: delta(&written.peer.head, &peer.head),
        documents,
        tail: delta(&written.peer.tail, &peer.tail),
    };
    Ok((
        frame(&entry)?,
        Written {
            fields,
            frames,
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
/// digest: no publication after it was acknowledged. `layout` is the version of the snapshot the
/// journal was appended beside, which a build of that version appended; `held` is what that
/// snapshot held that this build's record leaves out ([`held_before`]).
pub(super) fn replay(
    stored: StoredWorkspace,
    log: &[u8],
    layout: u32,
    held: Option<&serde_json::Value>,
) -> Result<(StoredWorkspace, Option<u64>), String> {
    let mut rest = log;
    match layout {
        LAYOUT_VERSION => {
            let Split { fields, .. } = split(&stored)?;
            replay_entries(
                stored,
                &fields,
                std::iter::from_fn(|| take::<Entry>(&mut rest).transpose()),
                false,
            )
        }
        PREVIOUS_LAYOUT => {
            let fields = previous_fields(&stored, layout, held)?;
            replay_entries(
                stored,
                &fields,
                std::iter::from_fn(|| {
                    take::<PreviousEntry>(&mut rest)
                        .map(|entry| entry.map(Entry::from))
                        .transpose()
                }),
                false,
            )
        }
        WRITES_APART..PREVIOUS_LAYOUT => {
            let fields = previous_fields(&stored, layout, held)?;
            replay_entries(
                stored,
                &fields,
                std::iter::from_fn(|| {
                    take::<EntryBeforeFrames>(&mut rest)
                        .map(|entry| entry.map(Entry::from))
                        .transpose()
                }),
                true,
            )
        }
        OLDEST_LAYOUT => {
            let fields = held
                .map(serde_json::to_vec)
                .transpose()
                .map_err(|error| error.to_string())?
                .ok_or("the snapshot's fields were not read")?;
            replay_entries(
                stored,
                &fields.into(),
                std::iter::from_fn(|| {
                    take::<EntryOfFields>(&mut rest)
                        .map(|entry| entry.map(Entry::from))
                        .transpose()
                }),
                true,
            )
        }
        _ => Err(format!(
            "the workspace's journal is a version {layout} one, and this build reads one of \
             version {OLDEST_LAYOUT} to {LAYOUT_VERSION}"
        )),
    }
}

/// Writes this device made that an earlier build's ownership held and had not sent, in the order
/// it would have sent them: its governance and its head ([`Self::writes`]), the runs it kept beside
/// the record ([`Self::runs`]), then its tail ([`Self::tail`]). Each is the message it would have
/// sent, as that build encoded it. The step to version 11 drops that ownership, and a device
/// offline keeps there every write it made: they go on as this build's own frames instead.
#[derive(Debug, Default)]
pub(super) struct Unsent {
    pub(super) writes: Vec<Vec<u8>>,
    /// Each run by its hash, with how many of its writes the host had taken already.
    pub(super) runs: Vec<([u8; 32], u32)>,
    pub(super) tail: Vec<Vec<u8>>,
}

/// A write ownership kept to send, as a snapshot's fields hold it.
#[derive(Deserialize)]
struct HeldWrite {
    #[serde(deserialize_with = "crate::owner::stored::base64_bytes::deserialize")]
    bytes: Vec<u8>,
}

/// A run of ownership's writes kept beside the record, as its fields name it.
#[derive(Deserialize)]
struct HeldRun {
    hash: [u8; 32],
    taken: u32,
}

/// Ownership as an earlier build's fields held it: only what names a write not sent yet.
#[derive(Deserialize, Default)]
struct HeldOwnership {
    #[serde(default)]
    governance: Vec<HeldWrite>,
    #[serde(default)]
    entries: Vec<HeldWrite>,
    #[serde(default)]
    tail: Vec<HeldWrite>,
    #[serde(default)]
    spilled: Vec<HeldRun>,
}

/// What the writes of an earlier build's journal entry did to ownership's lists, if its layout
/// carried them apart from its fields.
type ListStretches = Option<[Vec<earlier::Stretch>; 3]>;

/// The entries of an earlier build's journal: each one's change to the fields, and to ownership's
/// lists when its layout carried them apart.
type EntryChanges = Vec<(Vec<Op>, ListStretches)>;

/// An earlier build's fields, read for the ownership they held.
#[derive(Deserialize)]
struct HeldFields {
    #[serde(default)]
    publication: Option<HeldOwnership>,
}

/// The writes this device made that `stored`, a snapshot of version `layout` an earlier build
/// wrote, and the entries of its journal `log` leave unsent ([`Unsent`]); `held` is what the
/// snapshot held that this build's record leaves out ([`held_before`]). Nothing for a snapshot of
/// this build's version, which keeps its frames itself.
///
/// # Errors
/// The snapshot's ownership or the journal does not read.
pub(super) fn unsent(
    stored: &StoredWorkspace,
    layout: u32,
    held: Option<&serde_json::Value>,
    log: Option<&[u8]>,
) -> Result<Unsent, String> {
    let mut rest = log.unwrap_or_default();
    let (fields, lists, entries): (Arc<[u8]>, _, EntryChanges) = match layout {
        OLDEST_LAYOUT => {
            let fields = held
                .map(serde_json::to_vec)
                .transpose()
                .map_err(|error| error.to_string())?
                .ok_or("the snapshot's fields were not read")?;
            let entries = std::iter::from_fn(|| take::<EntryOfFields>(&mut rest).transpose())
                .map(|entry| entry.map(|entry| (entry.fields, None)))
                .collect::<Result<_, _>>()?;
            (fields.into(), None, entries)
        }
        WRITES_APART..=PREVIOUS_LAYOUT => {
            let ownership: HeldOwnership = held
                .map(|held| serde_json::from_value(held.clone()))
                .transpose()
                .map_err(|error| format!("the snapshot's ownership does not read: {error}"))?
                .unwrap_or_default();
            let bytes = |list: Vec<HeldWrite>| list.into_iter().map(|write| write.bytes).collect();
            let lists: [Vec<Vec<u8>>; 3] = [
                bytes(ownership.governance),
                bytes(ownership.entries),
                bytes(ownership.tail),
            ];
            let entries = if layout == PREVIOUS_LAYOUT {
                std::iter::from_fn(|| take::<PreviousEntry>(&mut rest).transpose())
                    .map(|entry| entry.map(|entry| (entry.fields, Some(entry.writes))))
                    .collect::<Result<_, _>>()?
            } else {
                std::iter::from_fn(|| take::<EntryBeforeFrames>(&mut rest).transpose())
                    .map(|entry| entry.map(|entry| (entry.fields, Some(entry.writes))))
                    .collect::<Result<_, _>>()?
            };
            (previous_fields(stored, layout, held)?, Some(lists), entries)
        }
        _ => return Ok(Unsent::default()),
    };
    let mut fields = Rope::of(&fields);
    let mut lists = lists;
    for (ops, stretches) in entries {
        fields = fields.apply(ops)?;
        if let (Some(lists), Some(stretches)) = (lists.as_mut(), stretches) {
            *lists = earlier::replay_stretches(lists, stretches)?;
        }
    }
    let held_fields: HeldFields = serde_json::from_slice(&fields.to_vec())
        .map_err(|error| format!("the record's ownership does not read: {error}"))?;
    // Fields that held no ownership left no write unsent.
    let ownership = held_fields.publication.unwrap_or_default();
    let bytes = |list: Vec<HeldWrite>| list.into_iter().map(|write| write.bytes).collect();
    // A build of version 8 to 10 carried the lists apart from the fields, which held them empty.
    let [governance, entries, tail]: [Vec<Vec<u8>>; 3] = lists.unwrap_or_else(|| {
        [
            bytes(ownership.governance),
            bytes(ownership.entries),
            bytes(ownership.tail),
        ]
    });
    Ok(Unsent {
        writes: governance.into_iter().chain(entries).collect(),
        runs: ownership
            .spilled
            .into_iter()
            .map(|run| (run.hash, run.taken))
            .collect(),
        tail,
    })
}

/// The writes of a run an earlier build's ownership kept beside its record, `bytes` as it kept
/// them, in their order: the run is named by its hash, which `hash` is.
///
/// # Errors
/// The bytes are not the run named, or do not read.
pub(super) fn run_writes(bytes: &[u8], hash: [u8; 32]) -> Result<Vec<Vec<u8>>, String> {
    if *blake3::hash(bytes).as_bytes() != hash {
        return Err("a run of writes kept beside the record is not the one it names".into());
    }
    let writes: Vec<earlier::Outgoing> =
        postcard::from_bytes(bytes).map_err(|error| error.to_string())?;
    Ok(writes.into_iter().map(|write| write.bytes).collect())
}

/// What a snapshot `bytes` an earlier build wrote held that this build's record leaves out, which
/// the journal beside it is a delta of; `None` for a snapshot of another version, or one that held
/// none.
///
/// Of version [`WRITES_APART`] to [`PREVIOUS_LAYOUT`] that is the outgoing ownership a build
/// before the one path kept. Of version [`OLDEST_LAYOUT`] it is every field, as that build split
/// them from its replica: the snapshot's own, in its order, its replica empty and its server's
/// answers left out, which that build's entries carried apart.
///
/// # Errors
/// The snapshot does not read.
pub(super) fn held_before(bytes: &[u8]) -> Result<Option<serde_json::Value>, String> {
    let mut held: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let version = held
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or("the snapshot names no version")?;
    if version == u64::from(OLDEST_LAYOUT) {
        held.insert(
            "peer".into(),
            serde_json::to_value(DurablePeer::default()).map_err(|error| error.to_string())?,
        );
        if let Some(serde_json::Value::Object(ownership)) = held.get_mut("publication") {
            ownership.shift_remove("answered");
        }
        return Ok(Some(serde_json::Value::Object(held)));
    }
    if !(u64::from(WRITES_APART)..=u64::from(PREVIOUS_LAYOUT)).contains(&version) {
        return Ok(None);
    }
    Ok(held.shift_remove("publication"))
}

/// The fields the first entry of a version `layout` build is a delta of: this build's, naming
/// `layout`, then the ownership its snapshot held, as that build split it: its lists of writes
/// emptied, and its answers and the writes its server had not taken left out, as an empty one of
/// each is. Before version 10 the frames a device keeps came last among them. The key order is
/// the snapshot's, which that build's types wrote.
fn previous_fields(
    stored: &StoredWorkspace,
    layout: u32,
    ownership: Option<&serde_json::Value>,
) -> Result<Arc<[u8]>, String> {
    let Split { fields, .. } = split_as(stored, layout)?;
    let mut bytes = fields.to_vec();
    if bytes.pop() != Some(b'}') {
        return Err("the record's fields are not a record".into());
    }
    if let Some(ownership) = ownership {
        let mut ownership = ownership.clone();
        let serde_json::Value::Object(held) = &mut ownership else {
            return Err("the snapshot's ownership is not a record".into());
        };
        for list in ["entries", "governance"] {
            held.insert(list.into(), serde_json::Value::Array(Vec::new()));
        }
        for gone in ["answered", "tail", "unanswered"] {
            held.shift_remove(gone);
        }
        bytes.extend_from_slice(b",\"publication\":");
        serde_json::to_writer(&mut bytes, &ownership).map_err(|error| error.to_string())?;
    }
    if layout < PREVIOUS_LAYOUT && !stored.frames.is_empty() {
        bytes.extend_from_slice(b",\"frames\":");
        serde_json::to_writer(&mut bytes, &stored.frames).map_err(|error| error.to_string())?;
    }
    bytes.push(b'}');
    Ok(bytes.into())
}

/// `stored`, a snapshot as read, as `entries` leave it, and the revision of the last one. With
/// `frames_in_fields` the entries carry the frames a device keeps among the fields, as a build
/// before version 10 appended them.
fn replay_entries(
    stored: StoredWorkspace,
    fields: &Arc<[u8]>,
    entries: impl Iterator<Item = Result<Entry, String>>,
    frames_in_fields: bool,
) -> Result<(StoredWorkspace, Option<u64>), String> {
    let rope = |parts: &[Arc<[u8]>]| Rope::of(&parts.concat().into());
    let mut frames = stored.frames.clone();
    let mut fields = Rope::of(fields);
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
        frames = replay_frames(&frames, entry.frames)?;
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
    // The fields name the version of the build that appended the journal, and an earlier one
    // its ownership, which this build's fields leave out: they read as this build's.
    replayed.version = WORKSPACE_VERSION;
    if !frames_in_fields {
        replayed.frames = frames;
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

/// `new`, the frames a device keeps, as stretches of `old`, the frames it kept before: a link
/// takes the oldest and a write adds its own, so the frames kept go on in their order and an
/// entry carries only those it added, however many a device offline holds. A run of `old` that
/// `new` shares is kept whole, so the frames a write left as they were cost nothing to compare.
fn frame_stretches(old: &Frames, new: &Frames) -> Vec<FrameStretch> {
    let mut stretches: Vec<FrameStretch> = Vec::new();
    // The first frame before that a frame now may still be, and the first run before that a run
    // now may still be, with the frames ahead of it.
    let mut next = 0usize;
    let (mut cursor, mut cursor_at) = (0usize, 0usize);
    for run in new.runs() {
        let mut at = cursor_at;
        let shared = old
            .runs()
            .iter()
            .enumerate()
            .skip(cursor)
            .find_map(|(index, before)| {
                let start = at;
                at = at.saturating_add(before.len());
                (start >= next && Arc::ptr_eq(before, run)).then_some((index, start))
            });
        if let Some((index, start)) = shared {
            next = start.saturating_add(run.len());
            (cursor, cursor_at) = (index.saturating_add(1), next);
            kept(&mut stretches, start, run.len());
            continue;
        }
        let mut rest = old.iter_from(next);
        for frame in run.iter() {
            let found = rest.clone().position(|before| {
                Arc::ptr_eq(&before.bytes, &frame.bytes) || before.bytes == frame.bytes
            });
            match found {
                Some(skipped) => {
                    rest.nth(skipped);
                    let at = next.saturating_add(skipped);
                    next = at.saturating_add(1);
                    kept(&mut stretches, at, 1);
                }
                None => match stretches.last_mut() {
                    Some(FrameStretch::Frames(frames)) => frames.push(frame.bytes.to_vec()),
                    _ => stretches.push(FrameStretch::Frames(vec![frame.bytes.to_vec()])),
                },
            }
        }
    }
    stretches
}

/// `len` frames kept from the one at `at` of those before, after `stretches`.
fn kept(stretches: &mut Vec<FrameStretch>, at: usize, len: usize) {
    let (Ok(at), Ok(len)) = (u32::try_from(at), u32::try_from(len)) else {
        return;
    };
    match stretches.last_mut() {
        Some(FrameStretch::Kept { from, len: kept }) if from.checked_add(*kept) == Some(at) => {
            *kept = kept.saturating_add(len);
        }
        _ => stretches.push(FrameStretch::Kept { from: at, len }),
    }
}

/// The frames `stretches` make of `old`, the frames kept before ([`frame_stretches()`]).
fn replay_frames(old: &Frames, stretches: Vec<FrameStretch>) -> Result<Frames, String> {
    let mut frames = Frames::default();
    for stretch in stretches {
        match stretch {
            FrameStretch::Kept { from, len } => {
                let from = usize::try_from(from).map_err(|e| e.to_string())?;
                let len = usize::try_from(len).map_err(|e| e.to_string())?;
                if from.saturating_add(len) > old.len() {
                    return Err("a journal entry keeps frames the device did not keep".into());
                }
                for frame in old.iter_from(from).take(len) {
                    frames.push(frame.clone());
                }
            }
            FrameStretch::Frames(added) => {
                for bytes in added {
                    frames.push(FrameToSend {
                        bytes: bytes.into(),
                    });
                }
            }
        }
    }
    Ok(frames)
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
fn take<E: serde::de::DeserializeOwned>(rest: &mut &[u8]) -> Result<Option<E>, String> {
    let Some(payload) = whole_payload(rest) else {
        return Ok(None);
    };
    let Ok((entry, [])) = postcard::take_from_bytes::<E>(payload) else {
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

/// Where a snapshot's chunks are cut, and the least and most each holds: a
/// snapshot written again after a change writes the chunks that change touched,
/// and a workspace of many megabytes is kept in some hundreds of them.
const SNAPSHOT_CUT_MASK: u64 = 0xffff_0000_0000_0000;
const SNAPSHOT_MIN_CHUNK: usize = 16 * 1024;
const SNAPSHOT_MAX_CHUNK: usize = 256 * 1024;

/// `bytes` cut into chunks by their content, as ranges.
fn chunks(bytes: &[u8]) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    cut(bytes, MIN_CHUNK, MAX_CHUNK, CUT_MASK)
}

/// `bytes` cut into a snapshot's chunks by their content, as ranges: each is kept once by its
/// digest, so a snapshot written again writes only the chunks a change touched.
pub(super) fn snapshot_chunks(bytes: &[u8]) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    cut(
        bytes,
        SNAPSHOT_MIN_CHUNK,
        SNAPSHOT_MAX_CHUNK,
        SNAPSHOT_CUT_MASK,
    )
}

/// `bytes` cut where the rolling hash has the bits of `mask` clear, each chunk
/// holding at least `min` bytes and at most `max`.
fn cut(
    bytes: &[u8],
    min: usize,
    max: usize,
    mask: u64,
) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= bytes.len() {
            return None;
        }
        let rest = after(bytes, start);
        let mut hash = 0u64;
        let mut len = rest.len().min(max);
        for (at, byte) in rest.iter().take(max).enumerate() {
            let gear = GEAR.get(usize::from(*byte)).copied().unwrap_or_default();
            hash = (hash << 1).wrapping_add(gear);
            if at >= min && hash & mask == 0 {
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
    // Every place each chunk is in `old`: a list of like entries (the frames a device keeps until
    // a link carries them) repeats chunks, and copying the one that goes on from the last copy
    // keeps an unchanged stretch one copy however many entries it holds.
    let mut known: std::collections::HashMap<&[u8], Vec<u64>> = std::collections::HashMap::new();
    for range in chunks(old) {
        if let Some(chunk) = old.get(range.clone()) {
            known.entry(chunk).or_default().push(range.start as u64);
        }
    }
    let mut ops = Ops(Vec::new());
    for range in chunks(new) {
        let chunk = new.get(range).unwrap_or_default();
        match known.get(chunk) {
            Some(offsets) => {
                let next = ops.next_copied();
                let offset = next
                    .filter(|next| offsets.binary_search(next).is_ok())
                    .or_else(|| offsets.first().copied())
                    .unwrap_or_default();
                ops.copy(offset, chunk.len());
            }
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
    /// Where in the old bytes a copy would go on from the last one: `None` after bytes of its own.
    fn next_copied(&self) -> Option<u64> {
        match self.0.last() {
            Some(Op::Copy { from, len }) => Some(from.saturating_add(*len)),
            _ => None,
        }
    }

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

/// Ownership's answers and lists of writes as builds of version 8 to 10 appended them to an entry:
/// postcard names no field, so an entry is read past them only by their exact layout. They go with
/// the ownership the step to version 11 drops, so nothing reads what they hold.
#[allow(
    dead_code,
    reason = "the layout earlier builds wrote, read only to reach what follows it"
)]
mod earlier {
    use serde::Deserialize;

    /// A document whose answers changed, with what it held then.
    pub(super) type Answered = ((String, String), Option<Answer>);

    /// The signature of a write its server had not answered, and whether it was held.
    pub(super) type Unanswered = (Vec<u8>, bool);

    /// The server's answer to a document's writes.
    #[derive(Deserialize)]
    pub(super) struct Answer {
        durable: u64,
        refused: u64,
        last_order: Option<u64>,
    }

    /// One stretch of a list of ownership's writes: writes it held before, or writes of its own.
    #[derive(Deserialize)]
    pub(super) enum Stretch {
        Kept { from: u32, len: u32 },
        Writes(Vec<Outgoing>),
    }

    /// A write ownership kept to send.
    #[derive(Deserialize)]
    pub(super) struct Outgoing {
        ordinal: u64,
        id: [u8; 32],
        pub(super) bytes: Vec<u8>,
        binding: Binding,
        timestamp_ns: u64,
        channel: [u8; 32],
        incremental: bool,
    }

    /// The lists `stretches` make of `old`, ownership's governance, head and tail before an entry:
    /// a write kept is named by its place over the three in turn.
    pub(super) fn replay_stretches(
        old: &[Vec<Vec<u8>>; 3],
        stretches: [Vec<Stretch>; 3],
    ) -> Result<[Vec<Vec<u8>>; 3], String> {
        let old: Vec<&Vec<u8>> = old.iter().flatten().collect();
        let mut lists: [Vec<Vec<u8>>; 3] = Default::default();
        for (list, stretches) in lists.iter_mut().zip(stretches) {
            for stretch in stretches {
                match stretch {
                    Stretch::Kept { from, len } => {
                        let from = usize::try_from(from).map_err(|e| e.to_string())?;
                        let len = usize::try_from(len).map_err(|e| e.to_string())?;
                        let kept = old
                            .get(from..from.saturating_add(len))
                            .ok_or("a journal entry keeps writes ownership did not hold")?;
                        list.extend(kept.iter().map(|write| (*write).clone()));
                    }
                    Stretch::Writes(writes) => {
                        list.extend(writes.into_iter().map(|write| write.bytes));
                    }
                }
            }
        }
        Ok(lists)
    }

    /// What a write was bound to.
    #[derive(Deserialize)]
    struct Binding {
        workspace: String,
        target: Target,
        request_digest: [u8; 32],
        basis: [u8; 32],
    }

    /// The record a write was for. Postcard names a variant by its place, never its name.
    #[derive(Deserialize)]
    enum Target {
        Document {
            app: String,
            record: String,
        },
        Effect {
            invocation: uuid::Uuid,
        },
        DesktopCommand {
            invocation: uuid::Uuid,
        },
        RefusedAgentRuntime,
        DocumentImport {
            invocation: uuid::Uuid,
        },
        RefusedWorkspaceTask,
        WorkspaceMembershipCommand {
            device: [u8; 32],
            invocation: uuid::Uuid,
        },
        NativeCheckpointRoot {
            root: [u8; 32],
        },
        RefusedShortcutChange {
            device: [u8; 32],
            invocation: uuid::Uuid,
        },
        JoinAdmission {
            device: [u8; 32],
        },
        RefusedDirectPair,
    }
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

        phase frames_kept_carry_only_those_an_entry_added;
        {
            let frame = |id: u8| FrameToSend {
                bytes: Arc::from(vec![id; 500]),
            };
            let carried = |entry: &[FrameStretch]| -> usize {
                entry.iter()
                    .map(|stretch| match stretch {
                        FrameStretch::Frames(frames) => frames.len(),
                        FrameStretch::Kept { .. } => 0,
                    })
                    .sum()
            };
            let old: Frames = (1..=4).map(frame).collect();
            // A write adds one: the entry carries it alone.
            let mut new = old.clone();
            new.push(frame(5));
            let entry = frame_stretches(&old, &new);
            assert_eq!(carried(&entry), 1);
            assert_eq!(replay_frames(&old, entry).unwrap(), new);
            // A link took the oldest two and a write added one, read back as other allocations.
            let read_back: Frames = (1..=4).map(frame).collect();
            let new: Frames = [frame(3), frame(4), frame(6)].into_iter().collect();
            let entry = frame_stretches(&read_back, &new);
            assert_eq!(carried(&entry), 1);
            assert_eq!(replay_frames(&old, entry).unwrap(), new);
            // A device offline keeps runs of frames: a write shares every run but its last, and
            // its entry carries its own frame, in one stretch kept.
            let offline: Frames = (0..=199).map(frame).collect();
            let mut written = offline.clone();
            written.push(frame(200));
            let entry = frame_stretches(&offline, &written);
            assert_eq!(carried(&entry), 1);
            assert!(matches!(
                entry.as_slice(),
                [FrameStretch::Kept { from: 0, len: 200 }, FrameStretch::Frames(_)]
            ));
            assert_eq!(replay_frames(&offline, entry).unwrap(), written);
            // Every frame taken leaves none, and an entry that keeps frames never kept is refused.
            let none = Frames::default();
            assert!(replay_frames(&old, frame_stretches(&old, &none)).unwrap().is_empty());
            assert!(replay_frames(&none, frame_stretches(&old, &old)).is_err());
        }

        phase a_torn_entry_ends_the_journal;
        {
            let entry = |revision| {
                frame(&Entry {
                    revision,
                    fields: Vec::new(),
                    frames: Vec::new(),
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
            let revisions: Vec<u64> = std::iter::from_fn(|| take::<Entry>(&mut rest).unwrap())
                .map(|entry| entry.revision)
                .collect();
            assert_eq!(revisions, [1, 2]);
            assert_eq!(rest.len(), log.len() - whole);
            let mut corrupt = entry(4);
            *corrupt.last_mut().unwrap() ^= 1;
            assert!(take::<Entry>(&mut &corrupt[..]).unwrap().is_none());
        }

        phase a_whole_entry_of_another_layout_is_refused_not_skipped;
        // An entry that is whole and reads as no entry of this build's was appended by a build of
        // another layout: reading on past it would drop what came after without a word.
        {
            let payload = postcard::to_allocvec(&(1_u64, "not an entry")).unwrap();
            let mut framed = u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
            framed.extend_from_slice(blake3::hash(&payload).as_bytes());
            framed.extend_from_slice(&payload);
            assert!(take::<Entry>(&mut &framed[..]).is_err());
        }

        phase a_journal_of_a_layout_this_build_does_not_replay_is_refused_by_its_version;
        // A journal older than the oldest layout this build replays, or one a later build
        // appended, is refused by the version it names, before any of it is read.
        {
            let golden: Vec<u8> = include_str!("../../golden/owner-workspace-document/v11.hex")
                .split_whitespace()
                .map(|byte| u8::from_str_radix(byte, 16).unwrap())
                .collect();
            let stored: StoredWorkspace = serde_json::from_slice(&golden).unwrap();
            for layout in [OLDEST_LAYOUT - 1, LAYOUT_VERSION + 1] {
                let error = replay(stored.clone(), b"", layout, None).unwrap_err();
                assert!(error.contains(&format!("version {layout}")), "{error}");
            }
        }
    }
}
