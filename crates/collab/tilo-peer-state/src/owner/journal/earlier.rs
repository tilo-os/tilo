//! A journal written beside a workspace's entry of an earlier version, folded into that entry
//! once, on its way up to this build's version.
//!
//! Up to version 7 a journal entry carried ownership's lists of writes inside the workspace's
//! fields: its governance, its head and its tail rode in the JSON each entry is a delta of.
//! Version 8 journals them apart ([`super::Entry::writes`]), so an entry written before is a delta
//! of other bytes. A device that updates from such a build folds the journal that build left into
//! its snapshot under that build's rules, brings the snapshot up through the entry's format, and
//! writes a snapshot of its own before it appends again.
//!
//! Version 7 entries come in both layouts: the builds just before version 8 appended this build's
//! layout beside a version 7 snapshot. Each entry's bytes read whole as only one of the two, and a
//! journal is folded under the one all its entries read as.

use super::{DocumentOps, FRAME_HEAD, Op, Rope, Stretch};
use crate::owner::answers::AnswerSignature;
use crate::owner::publication::Answer;
use crate::owner::stored::{DurablePeer, StoredWorkspace};
use crate::state::{DocumentParts, DurableParts, Route};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A version 7 entry of the layout version 8 keeps, but for its answers, which left out an order
/// they had none of.
#[derive(Deserialize)]
struct EntryApart {
    revision: u64,
    fields: Vec<Op>,
    answered: Vec<((String, String), Option<Answer>)>,
    unanswered: Vec<(AnswerSignature, bool)>,
    writes: [Vec<Stretch>; 3],
    head: Vec<Op>,
    documents: Vec<(Route, Option<DocumentOps>)>,
    tail: Vec<Op>,
}

impl From<EntryApart> for super::Entry {
    fn from(entry: EntryApart) -> Self {
        Self {
            revision: entry.revision,
            fields: entry.fields,
            answered: entry
                .answered
                .into_iter()
                .map(|(key, answer)| (key, answer.map(Into::into)))
                .collect(),
            unanswered: entry.unanswered,
            writes: entry.writes,
            head: entry.head,
            documents: entry.documents,
            tail: entry.tail,
        }
    }
}

/// An entry as builds before version 8 appended it, ownership's writes inside its fields.
#[derive(Deserialize)]
struct Entry {
    revision: u64,
    /// The workspace's fields, ownership's writes among them, without its answers and its replica.
    fields: Vec<Op>,
    answered: Vec<((String, String), Option<Answer>)>,
    head: Vec<Op>,
    documents: Vec<(Route, Option<DocumentOps>)>,
    tail: Vec<Op>,
}

/// The first version whose journal is this build's layout alone.
pub(crate) const JOURNAL_LAYOUT: u32 = 8;

/// `snapshot`, a workspace's entry of `version`, earlier than [`JOURNAL_LAYOUT`], with the entries
/// of its journal `log` applied, as that version's bytes: what the build that wrote both read.
/// Entries end at one a crash cut short, as they always did.
///
/// The revision of the last entry comes with it, or `None` when the journal holds none.
///
/// # Errors
/// The snapshot does not read, an entry reads as neither layout, or an entry names bytes the
/// snapshot does not hold.
pub(crate) fn fold(
    version: u32,
    snapshot: Vec<u8>,
    log: &[u8],
) -> Result<(Vec<u8>, Option<u64>), String> {
    let payloads = payloads(log);
    if payloads.is_empty() {
        return Ok((snapshot, None));
    }
    let apart = if version.checked_add(1) == Some(JOURNAL_LAYOUT) {
        payloads
            .iter()
            .map(|payload| whole::<EntryApart>(payload))
            .collect::<Option<Vec<_>>>()
    } else {
        None
    };
    if let Some(entries) = apart {
        let stored: StoredWorkspace =
            serde_json::from_slice(&snapshot).map_err(|error| error.to_string())?;
        let (stored, revision) =
            super::replay_entries(stored, entries.into_iter().map(|entry| Ok(entry.into())))?;
        let bytes = serde_json::to_vec(&stored).map_err(|error| error.to_string())?;
        return Ok((bytes, revision));
    }
    let entries = payloads
        .iter()
        .map(|payload| whole::<Entry>(payload))
        .collect::<Option<Vec<_>>>()
        .ok_or("a journal entry of an earlier version reads as neither of its layouts")?;
    replay(&snapshot, entries)
}

/// Each entry's bytes in `log`, up to one a crash cut short or that fails its digest.
fn payloads(log: &[u8]) -> Vec<&[u8]> {
    let mut found = Vec::new();
    let mut rest = log;
    while let Some(head) = rest.get(..FRAME_HEAD) {
        let Some(len) = head
            .get(..4)
            .and_then(|len| len.try_into().ok())
            .map(u32::from_le_bytes)
            .and_then(|len| usize::try_from(len).ok())
        else {
            break;
        };
        let Some(payload) = FRAME_HEAD
            .checked_add(len)
            .and_then(|end| rest.get(FRAME_HEAD..end))
        else {
            break;
        };
        if blake3::hash(payload).as_bytes() != head.get(4..FRAME_HEAD).unwrap_or_default() {
            break;
        }
        found.push(payload);
        rest = rest
            .get(FRAME_HEAD.saturating_add(len)..)
            .unwrap_or_default();
    }
    found
}

/// `payload` read as a `T` that is all of its bytes.
fn whole<'a, T: Deserialize<'a>>(payload: &'a [u8]) -> Option<T> {
    match postcard::take_from_bytes::<T>(payload) {
        Ok((read, [])) => Some(read),
        _ => None,
    }
}

/// The workspace's fields as an earlier build's entries are deltas of: the snapshot's own JSON
/// without its replica and its answers, every other field in its place.
fn fields_before(snapshot: &[u8]) -> Result<Vec<u8>, String> {
    let mut fields: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(snapshot).map_err(|error| error.to_string())?;
    fields.insert(
        "peer".into(),
        serde_json::to_value(DurablePeer::default()).map_err(|error| error.to_string())?,
    );
    if let Some(publication) = fields
        .get_mut("publication")
        .and_then(serde_json::Value::as_object_mut)
    {
        publication.shift_remove("answered");
    }
    serde_json::to_vec(&fields).map_err(|error| error.to_string())
}

/// `snapshot` as an earlier build replayed `entries` over it.
fn replay(snapshot: &[u8], entries: Vec<Entry>) -> Result<(Vec<u8>, Option<u64>), String> {
    let read: StoredWorkspace =
        serde_json::from_slice(snapshot).map_err(|error| error.to_string())?;
    let mut answered = read
        .publication
        .clone()
        .map(|mut publication| publication.take_journaled().answered)
        .unwrap_or_default();
    let rope = |parts: &[Arc<[u8]>]| Rope::of(&parts.concat().into());
    let layout = read.peer.layout();
    let mut fields = Rope::of(&fields_before(snapshot)?.into());
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
        revision = Some(entry.revision);
        fields = fields.apply(entry.fields)?;
        for (key, answer) in entry.answered {
            match answer {
                Some(answer) => answered.insert(key, answer),
                None => answered.remove(&key),
            };
        }
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
    }
    let mut replayed: StoredWorkspace = serde_json::from_slice(&fields.to_vec())
        .map_err(|error| format!("the workspace's journal does not parse: {error}"))?;
    if let Some(publication) = replayed.publication.as_mut() {
        let mut journaled = publication.take_journaled();
        journaled.answered = answered;
        publication.set_journaled(journaled);
    }
    let whole = |rope: &Rope| -> Arc<[Arc<[u8]>]> { Arc::from([Arc::from(rope.to_vec())]) };
    replayed.peer = DurablePeer::from(DurableParts {
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
    });
    let bytes = serde_json::to_vec(&replayed).map_err(|error| error.to_string())?;
    Ok((bytes, revision))
}
