//! A stretch of a resource's history as a document of its own: what a report
//! carries so its reader can step through the states the reporter saw
//! (`docs/architecture/session-replay.md`).
//!
//! A resource's document already records every change with its author, time
//! and action. [`record`] takes the changes of a window of time and writes
//! them into a new document: the first change writes the resource's state at
//! the start of the window, and each later one is an original change's
//! effect, read as Automerge's own diff between the two frontiers and
//! written with the original's actor, timestamp and action entry. Nothing
//! from before the window is in it; an Automerge change graph cannot be
//! served in part, so handing out the resource itself would hand out all of
//! its history.
//!
//! The recording is a document of the same shape as the resource, so the
//! app's views draw it and History steps through it: its [`history`]
//! entries are the steps, and [`Recording::heads_at`] is the state at one.
//!
//! [`history`]: crate::history

use std::collections::{BTreeMap, HashMap, HashSet};

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::{CommitOptions, Transactable};
use automerge::{
    ActorId, Automerge, ChangeHash, ObjId, ObjType, Patch, PatchAction, Prop, ROOT, ReadDoc,
    ScalarValue, Value,
};
use tilo_actions::{ActionContext, ChangeKind, EntryReader};
use tilo_sort::CompactSort as _;

use crate::history::{Version, committed_versions};
use crate::model::Doc;

/// Root keys a recording leaves out: the recorded record's own identity and
/// the records it indexes. A recording is not that record.
const LEFT_OUT: [&str; 2] = ["tilo:record", "tilo:records"];

/// How large a recording may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Changes after the start state. A longer window starts later, so the
    /// last changes before the end are always kept.
    pub steps: usize,
    /// Saved bytes of the whole recording.
    pub bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            steps: 2_000,
            bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RecordingError {
    #[error("the end of the recording is not in this document's history")]
    UnknownEnd,
    #[error("the recording would take {0} bytes, more than it may")]
    TooLarge(usize),
    #[error("the recording holds more steps than it may")]
    TooManySteps,
    #[error("this is not a recording: {0}")]
    Invalid(String),
    #[error("could not write the recording: {0}")]
    Write(String),
    #[error("the recording does not reproduce the recorded state")]
    Diverged,
}

impl From<automerge::AutomergeError> for RecordingError {
    fn from(error: automerge::AutomergeError) -> Self {
        Self::Write(error.to_string())
    }
}

/// One step of a recording, by the change it reproduces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The recorded document's change.
    pub original: ChangeHash,
    /// The recording's change that reproduces it.
    pub recorded: ChangeHash,
}

/// A recording: its document and what it covers.
pub struct Recording {
    doc: Doc,
    /// Milliseconds since the Unix epoch at which the start state stands.
    pub from_ms: i64,
    /// Milliseconds since the Unix epoch of the last step, or `from_ms`.
    pub to_ms: i64,
    /// The steps after the start, oldest first. Empty for a loaded
    /// recording, whose original changes are not known.
    pub steps: Vec<Step>,
}

/// The changes of `source` that `upto` holds, from the first at or after
/// `from_ms` (milliseconds since the Unix epoch), recorded on the state
/// before them.
///
/// # Errors
///
/// [`RecordingError::UnknownEnd`] when `source` lacks a change of `upto`;
/// [`RecordingError::TooLarge`] past `limits.bytes`;
/// [`RecordingError::Diverged`] when the written recording does not end in
/// `source`'s state at `upto`.
pub fn record(
    source: &Doc,
    from_ms: i64,
    upto: &[ChangeHash],
    limits: &Limits,
) -> Result<Recording, RecordingError> {
    let document = source.committed_document();
    let ordered = ancestry(document, upto)?;
    let Window { start, from_ms, .. } = window(&ordered, from_ms, limits);
    let mut writer = Writer {
        source: document,
        target: Doc::new(),
        objects: HashMap::from([(ROOT, ROOT)]),
        authored: HashSet::new(),
    };
    let mut entries = EntryReader::default();
    for change in ordered.iter().take(start) {
        entries.read(&change.actor, change.message.as_deref());
    }
    let start_heads = frontier(ordered.get(..start).unwrap_or(&[]));
    writer.write(&[], &start_heads)?;
    let begun = ActionContext::default().with_change(ChangeKind::External);
    let start_time = from_ms.div_euclid(1000);
    writer.commit(start_time, begun.change_message().as_deref());
    let mut steps = Vec::new();
    let mut before = start_heads;
    for (at, change) in ordered.iter().enumerate().skip(start) {
        let after = frontier(ordered.get(..=at).unwrap_or(&[]));
        writer.target.set_actor(change.actor.clone());
        writer.write(&before, &after)?;
        let entry = entries.read(&change.actor, change.message.as_deref());
        let message = match change.message.as_deref() {
            Some(text) if ActionContext::continued_gesture(text).is_some() => {
                entry.and_then(|entry| entry.change_message())
            }
            text => text.map(str::to_owned),
        };
        steps.push(Step {
            original: change.hash,
            recorded: writer.commit(change.time_s, message.as_deref()),
        });
        before = after;
    }
    let mut doc = writer.target;
    let heads = doc.get_heads();
    if !same_state(document, &before, doc.committed_document(), &heads)? {
        return Err(RecordingError::Diverged);
    }
    let size = doc
        .save()
        .len()
        .saturating_add(tilo_content::RECORDING_MAGIC.len());
    if size > limits.bytes {
        return Err(RecordingError::TooLarge(size));
    }
    let to_ms = window(&ordered, from_ms, limits).to_ms;
    Ok(Recording {
        doc,
        from_ms,
        to_ms,
        steps,
    })
}

/// What [`record`] would record of `source` with the same arguments, read
/// from the changes' metadata alone: nothing is written.
///
/// # Errors
///
/// [`RecordingError::UnknownEnd`] when `source` lacks a change of `upto`.
pub fn plan(
    source: &Doc,
    from_ms: i64,
    upto: &[ChangeHash],
    limits: &Limits,
) -> Result<Planned, RecordingError> {
    let ordered = ancestry(source.committed_document(), upto)?;
    let window = window(&ordered, from_ms, limits);
    Ok(Planned {
        from_ms: window.from_ms,
        to_ms: window.to_ms,
        steps: ordered.len().saturating_sub(window.start),
    })
}

/// What a recording would hold, before it is made ([`plan`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Planned {
    pub from_ms: i64,
    pub to_ms: i64,
    pub steps: usize,
}

/// Where a recording of `ordered` from `from_ms` starts within `limits`.
struct Window {
    /// The first change recorded as a step.
    start: usize,
    from_ms: i64,
    to_ms: i64,
}

fn window(ordered: &[Change], from_ms: i64, limits: &Limits) -> Window {
    let times: Vec<i64> = ordered
        .iter()
        .map(|change| change.time_s.saturating_mul(1000))
        .collect();
    let mut start = times.iter().take_while(|time| **time < from_ms).count();
    let mut from_ms = from_ms;
    if ordered.len().saturating_sub(start) > limits.steps {
        start = ordered.len().saturating_sub(limits.steps);
        from_ms = times.get(start).copied().unwrap_or(from_ms);
    }
    let to_ms = times
        .last()
        .copied()
        .filter(|_| ordered.len() > start)
        .unwrap_or(from_ms);
    Window {
        start,
        from_ms,
        to_ms: to_ms.max(from_ms),
    }
}

impl Recording {
    /// Read a recording from its saved `bytes`, within `limits`.
    ///
    /// # Errors
    ///
    /// Refuses bytes past `limits.bytes`, a document that does not start
    /// with a recording's start state, or one with more steps than
    /// `limits.steps`.
    pub fn load(bytes: &[u8], limits: &Limits) -> Result<Self, RecordingError> {
        if bytes.len() > limits.bytes {
            return Err(RecordingError::TooLarge(bytes.len()));
        }
        let saved = bytes
            .strip_prefix(tilo_content::RECORDING_MAGIC)
            .ok_or_else(|| RecordingError::Invalid("it has no recording header".into()))?;
        let doc = Doc::load(saved).map_err(|error| RecordingError::Invalid(error.to_string()))?;
        let versions = committed_versions(&doc);
        let (first, rest) = versions
            .split_first()
            .ok_or_else(|| RecordingError::Invalid("it has no start".into()))?;
        let begins = first
            .context
            .as_ref()
            .is_some_and(|context| context.change == ChangeKind::External);
        let has_deps = doc
            .committed_document()
            .get_change_meta_by_hash(&first.hash)
            .is_none_or(|meta| !meta.deps.is_empty());
        if !begins || has_deps {
            return Err(RecordingError::Invalid(
                "it does not start with a state".into(),
            ));
        }
        if rest.len() > limits.steps {
            return Err(RecordingError::TooManySteps);
        }
        Ok(Self {
            from_ms: first.time_ms,
            to_ms: rest.last().map_or(first.time_ms, |last| last.time_ms),
            doc,
            steps: Vec::new(),
        })
    }

    /// The recording's document: a document of the recorded resource's
    /// shape whose history is the recording.
    #[must_use]
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// The saved bytes, behind the recording header
    /// ([`tilo_content::RECORDING_MAGIC`]) that says what they are: what a
    /// report carries and what the recorded app opens.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let mut bytes = tilo_content::RECORDING_MAGIC.to_vec();
        bytes.extend(self.doc.clone().save());
        bytes
    }

    /// The start state and every step after it, oldest first, as History
    /// lists them.
    #[must_use]
    pub fn versions(&self) -> Vec<Version> {
        committed_versions(&self.doc)
    }

    /// How many steps follow the start state.
    #[must_use]
    pub fn step_count(&self) -> usize {
        self.versions().len().saturating_sub(1)
    }

    /// The heads of the state at `step`: 0 is the start state, `n` the
    /// state after the `n`th step. `None` past the last step.
    #[must_use]
    pub fn heads_at(&self, step: usize) -> Option<Vec<ChangeHash>> {
        let versions = self.versions();
        // Each step depends on the one before it: the frontier is the step.
        versions.get(step).map(|version| vec![version.hash])
    }
}

/// One change of the recorded window, as its metadata reads.
struct Change {
    hash: ChangeHash,
    deps: Vec<ChangeHash>,
    actor: ActorId,
    time_s: i64,
    message: Option<String>,
}

/// The changes `upto` holds, in causal order.
fn ancestry(document: &Automerge, upto: &[ChangeHash]) -> Result<Vec<Change>, RecordingError> {
    let mut held = HashSet::new();
    let mut pending = upto.to_vec();
    while let Some(hash) = pending.pop() {
        if !held.insert(hash) {
            continue;
        }
        let meta = document
            .get_change_meta_by_hash(&hash)
            .ok_or(RecordingError::UnknownEnd)?;
        pending.extend(meta.deps.iter().copied());
    }
    Ok(document
        .get_changes_meta(&[])
        .into_iter()
        .filter(|meta| held.contains(&meta.hash))
        .map(|meta| Change {
            hash: meta.hash,
            deps: meta.deps.clone(),
            actor: meta.actor.clone().into_owned(),
            time_s: meta.timestamp,
            message: meta.message.as_deref().map(str::to_owned),
        })
        .collect())
}

/// The heads of a causally closed run of changes: those no other change of
/// the run depends on.
fn frontier(changes: &[Change]) -> Vec<ChangeHash> {
    let depended: HashSet<ChangeHash> = changes
        .iter()
        .flat_map(|change| change.deps.iter().copied())
        .collect();
    let mut heads: Vec<ChangeHash> = changes
        .iter()
        .map(|change| change.hash)
        .filter(|hash| !depended.contains(hash))
        .collect();
    heads.compact_sort();
    heads
}

/// Writes the effect of the source's changes into the recording.
struct Writer<'a> {
    source: &'a Automerge,
    target: Doc,
    /// Each source object the recording holds, by its id in the source.
    objects: HashMap<ObjId, ObjId>,
    /// Source objects written whole in the current step.
    authored: HashSet<ObjId>,
}

impl Writer<'_> {
    /// Write what changes from the source's state at `before` to its state
    /// at `after`.
    fn write(&mut self, before: &[ChangeHash], after: &[ChangeHash]) -> Result<(), RecordingError> {
        self.authored.clear();
        for patch in self.source.diff(before, after) {
            self.apply(patch, after)?;
        }
        Ok(())
    }

    /// Commit what was written as one change at `time_s` with `message`,
    /// an empty change when nothing was.
    fn commit(&mut self, time_s: i64, message: Option<&str>) -> ChangeHash {
        let options = || {
            let options = CommitOptions::default().with_time(time_s);
            match &message {
                Some(message) => options.with_message((*message).to_owned()),
                None => options,
            }
        };
        if self.target.pending_ops() > 0
            && let Some(hash) = self.target.commit_with(options())
        {
            return hash;
        }
        self.target.rollback();
        self.target.empty_change(options())
    }

    fn left_out(patch: &Patch) -> bool {
        let at_root =
            |prop: &Prop| matches!(prop, Prop::Map(key) if LEFT_OUT.contains(&key.as_str()));
        if let Some((obj, prop)) = patch.path.first() {
            return *obj == ROOT && at_root(prop);
        }
        patch.obj == ROOT
            && match &patch.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                    LEFT_OUT.contains(&key.as_str())
                }
                PatchAction::Increment { prop, .. } | PatchAction::Conflict { prop } => {
                    at_root(prop)
                }
                _ => false,
            }
    }

    fn apply(&mut self, patch: Patch, after: &[ChangeHash]) -> Result<(), RecordingError> {
        if Self::left_out(&patch)
            || self.authored.contains(&patch.obj)
            || patch
                .path
                .iter()
                .any(|(obj, _)| self.authored.contains(obj))
        {
            return Ok(());
        }
        let obj = self
            .objects
            .get(&patch.obj)
            .cloned()
            .ok_or_else(|| RecordingError::Write("a change names an unknown object".into()))?;
        match patch.action {
            PatchAction::PutMap { key, value, .. } => {
                self.put(&obj, Prop::Map(key), value, false, after)?;
            }
            PatchAction::PutSeq { index, value, .. } => {
                self.put(&obj, Prop::Seq(index), value, false, after)?;
            }
            PatchAction::Insert { index, values } => {
                for (offset, (value, id, _)) in values.iter().enumerate() {
                    let at = index.checked_add(offset).ok_or_else(overflow)?;
                    let value = (value.clone(), id.clone());
                    self.put(&obj, Prop::Seq(at), value, true, after)?;
                }
            }
            PatchAction::SpliceText {
                index,
                value,
                marks,
            } => {
                let before = self.target.length(&obj);
                self.target
                    .splice_text(&obj, index, 0, &value.make_string())?;
                let inserted = self.target.length(&obj).saturating_sub(before);
                let wanted = marks
                    .map(|set| {
                        set.iter()
                            .map(|(name, value)| (name.to_owned(), value.clone()))
                            .collect::<BTreeMap<_, _>>()
                    })
                    .unwrap_or_default();
                self.settle_marks(&obj, index, inserted, &wanted)?;
            }
            PatchAction::Increment { prop, value } => self.target.increment(&obj, prop, value)?,
            PatchAction::Conflict { .. } => {}
            PatchAction::DeleteMap { key } => self.target.delete(&obj, key.as_str())?,
            PatchAction::DeleteSeq { index, length } => {
                if self.target.object_type(&obj)? == ObjType::Text {
                    let length = isize::try_from(length).map_err(|_| overflow())?;
                    self.target.splice_text(&obj, index, length, "")?;
                } else {
                    for _ in 0..length {
                        self.target.delete(&obj, index)?;
                    }
                }
            }
            PatchAction::Mark { marks } => {
                for mark in marks {
                    self.target.mark(&obj, mark, ExpandMark::None)?;
                }
            }
        }
        Ok(())
    }

    /// Give the `len` characters just inserted at `index` exactly the marks
    /// `wanted`, whatever they took from their neighbours.
    fn settle_marks(
        &mut self,
        obj: &ObjId,
        index: usize,
        len: usize,
        wanted: &BTreeMap<String, ScalarValue>,
    ) -> Result<(), RecordingError> {
        if len == 0 {
            return Ok(());
        }
        let end = index.checked_add(len).ok_or_else(overflow)?;
        let have = self
            .target
            .get_marks(obj, index, None)?
            .iter()
            .map(|(name, value)| (name.to_owned(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        for (name, value) in wanted {
            if have.get(name) != Some(value) {
                let mark = Mark::new(name.clone(), value.clone(), index, end);
                self.target.mark(obj, mark, ExpandMark::None)?;
            }
        }
        for name in have.keys().filter(|name| !wanted.contains_key(*name)) {
            self.target
                .unmark(obj, name, index, end, ExpandMark::None)?;
        }
        Ok(())
    }

    /// Write one value at `prop` of `parent`: a scalar as itself, an object
    /// as a new object holding the source's whole copy of it at `after`.
    fn put(
        &mut self,
        parent: &ObjId,
        prop: Prop,
        (value, source): (Value<'_>, ObjId),
        insert: bool,
        after: &[ChangeHash],
    ) -> Result<(), RecordingError> {
        match (value, insert, prop) {
            (Value::Scalar(scalar), true, Prop::Seq(index)) => {
                self.target.insert(parent, index, scalar.into_owned())?;
            }
            (Value::Scalar(scalar), _, prop) => {
                self.target.put(parent, prop, scalar.into_owned())?;
            }
            (Value::Object(kind), true, Prop::Seq(index)) => {
                let made = self.target.insert_object(parent, index, kind)?;
                self.author(source, made, kind, after)?;
            }
            (Value::Object(kind), _, prop) => {
                let made = self.target.put_object(parent, prop, kind)?;
                self.author(source, made, kind, after)?;
            }
        }
        Ok(())
    }

    /// Fill the new object `made` with the whole content `source` has at
    /// `after`.
    fn author(
        &mut self,
        source: ObjId,
        made: ObjId,
        kind: ObjType,
        after: &[ChangeHash],
    ) -> Result<(), RecordingError> {
        let mut pending = vec![(source, made, kind)];
        while let Some((from, into, kind)) = pending.pop() {
            self.authored.insert(from.clone());
            self.objects.insert(from.clone(), into.clone());
            match kind {
                ObjType::Map | ObjType::Table => {
                    let keys = self.source.keys_at(&from, after).collect::<Vec<_>>();
                    for key in keys {
                        match entry(self.source, &from, key.as_str(), after)? {
                            Some(Entry::Scalar(scalar)) => {
                                self.target.put(&into, key.as_str(), scalar)?;
                            }
                            Some(Entry::Object(kind, child)) => {
                                let made = self.target.put_object(&into, key.as_str(), kind)?;
                                pending.push((child, made, kind));
                            }
                            None => {}
                        }
                    }
                }
                ObjType::List => {
                    for index in 0..self.source.length_at(&from, after) {
                        match entry(self.source, &from, index, after)? {
                            Some(Entry::Scalar(scalar)) => {
                                self.target.insert(&into, index, scalar)?;
                            }
                            Some(Entry::Object(kind, child)) => {
                                let made = self.target.insert_object(&into, index, kind)?;
                                pending.push((child, made, kind));
                            }
                            None => {}
                        }
                    }
                }
                ObjType::Text => {
                    let text = self.source.text_at(&from, after)?;
                    self.target.splice_text(&into, 0, 0, &text)?;
                    for mark in self.source.marks_at(&from, after)? {
                        self.target.mark(&into, mark, ExpandMark::None)?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn overflow() -> RecordingError {
    RecordingError::Write("an index overflowed".into())
}

/// One value of an object, owned.
#[derive(PartialEq)]
enum Entry {
    Scalar(ScalarValue),
    Object(ObjType, ObjId),
}

fn entry(
    doc: &Automerge,
    obj: &ObjId,
    prop: impl Into<Prop>,
    at: &[ChangeHash],
) -> Result<Option<Entry>, RecordingError> {
    Ok(doc.get_at(obj, prop, at)?.map(|(value, id)| match value {
        Value::Object(kind) => Entry::Object(kind, id),
        Value::Scalar(scalar) => Entry::Scalar(scalar.into_owned()),
    }))
}

/// Whether `left` at `left_at` and `right` at `right_at` hold the same
/// keys, values, sequences, text and marks, whatever their objects' ids,
/// leaving out the root keys a recording leaves out.
///
/// # Errors
///
/// A read Automerge refuses.
pub fn same_state(
    left: &Automerge,
    left_at: &[ChangeHash],
    right: &Automerge,
    right_at: &[ChangeHash],
) -> Result<bool, RecordingError> {
    let mut pending = vec![(ROOT, ROOT, ObjType::Map)];
    while let Some((one, other, kind)) = pending.pop() {
        match kind {
            ObjType::Map | ObjType::Table => {
                let keys = |doc: &Automerge, obj: &ObjId, at: &[ChangeHash]| {
                    let mut keys = doc
                        .keys_at(obj, at)
                        .filter(|key| !(*obj == ROOT && LEFT_OUT.contains(&key.as_str())))
                        .collect::<Vec<_>>();
                    keys.compact_sort();
                    keys
                };
                let mine = keys(left, &one, left_at);
                if mine != keys(right, &other, right_at) {
                    return Ok(false);
                }
                for key in mine {
                    let a = entry(left, &one, key.as_str(), left_at)?;
                    let b = entry(right, &other, key.as_str(), right_at)?;
                    if !same_entry(a, b, &mut pending) {
                        return Ok(false);
                    }
                }
            }
            ObjType::List => {
                let len = left.length_at(&one, left_at);
                if len != right.length_at(&other, right_at) {
                    return Ok(false);
                }
                for index in 0..len {
                    let a = entry(left, &one, index, left_at)?;
                    let b = entry(right, &other, index, right_at)?;
                    if !same_entry(a, b, &mut pending) {
                        return Ok(false);
                    }
                }
            }
            ObjType::Text => {
                if left.text_at(&one, left_at)? != right.text_at(&other, right_at)?
                    || marks(left, &one, left_at)? != marks(right, &other, right_at)?
                {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

/// The marks of a text, as spans that say what they mark.
fn marks(
    doc: &Automerge,
    obj: &ObjId,
    at: &[ChangeHash],
) -> Result<Vec<(usize, usize, String, ScalarValue)>, RecordingError> {
    Ok(doc
        .marks_at(obj, at)?
        .into_iter()
        .map(|mark| {
            (
                mark.start,
                mark.end,
                mark.name().to_owned(),
                mark.value().clone(),
            )
        })
        .collect())
}

fn same_entry(
    left: Option<Entry>,
    right: Option<Entry>,
    pending: &mut Vec<(ObjId, ObjId, ObjType)>,
) -> bool {
    match (left, right) {
        (Some(Entry::Object(kind, one)), Some(Entry::Object(other_kind, other)))
            if kind == other_kind =>
        {
            pending.push((one, other, kind));
            true
        }
        (left, right) => left == right,
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "a test generator and the positions it picks"
)]
#[path = "recording_tests.rs"]
mod tests;
