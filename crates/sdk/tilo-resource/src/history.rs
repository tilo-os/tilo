//! Version history of any resource, derived from its document's change graph:
//! who changed it, when, through which action — and what it looked like then.
//!
//! Nothing here is specific to a resource type. The attribution comes from
//! the executor (the change's actor is the author, its message carries the
//! action's [`ActionContext`]); materialising a past version is
//! [`Resource::read`] on the document forked at that version's heads.
//!
//! Versions are addressed two ways: by heads (a set of change hashes, what a
//! collaborator can name) and by position in [`versions`]' causal order (what
//! a scrubber counts). [`prefix_heads`] turns a position into heads, and
//! [`replay_prefix`] rebuilds the document a position names.

use std::collections::{BinaryHeap, HashMap};
use tilo_sort::CompactSort as _;

use automerge::ChangeHash;
use automerge::transaction::{CommitOptions, Transactable};
use tilo_actions::ActionContext;
use tilo_automerge::ApplyIncoming;

use crate::model::{Doc, ModelError};
use crate::resource::Resource;

/// One change on a resource's timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub hash: ChangeHash,
    /// Hex of the authoring actor.
    pub actor: String,
    /// Milliseconds since the Unix epoch, or 0 when the change carries no time.
    pub time_ms: i64,
    /// What the change did, when it was made through an action.
    pub context: Option<ActionContext>,
    /// The raw change message.
    pub message: Option<String>,
}

/// One change of `doc` as a version on the timeline; a continuation of a
/// gesture reads as its gesture's entry ([`tilo_actions::entry_of`]).
pub fn version_of(doc: &automerge::Automerge, meta: &automerge::ChangeMetadata<'_>) -> Version {
    let message = meta.message.as_deref().map(str::to_owned);
    let context = match message.as_deref() {
        Some(text) if text.starts_with(tilo_actions::CONTINUATION_PREFIX) => {
            tilo_actions::entry_of(doc, meta.hash)
        }
        text => text.and_then(ActionContext::from_change_message),
    };
    Version {
        hash: meta.hash,
        actor: meta.actor.to_hex_string(),
        time_ms: meta.timestamp.saturating_mul(1000),
        context,
        message,
    }
}

/// Every change in causal order, oldest first. A change's position in this
/// list is the address [`prefix_heads`] and [`replay_prefix`] take.
///
/// Reads every change's metadata (never its operations). For a long history
/// shown a screen at a time, [`page`] reads only the page.
pub fn versions(doc: &mut Doc) -> Vec<Version> {
    // Closes the open transaction, so its change is listed too.
    doc.get_heads();
    committed_versions(doc)
}

/// [`versions`] of the committed document, read through a shared
/// reference: a transaction still open is not listed.
pub fn committed_versions(doc: &Doc) -> Vec<Version> {
    let mut entries = tilo_actions::EntryReader::default();
    doc.committed_document()
        .get_changes_meta(&[])
        .iter()
        .map(|meta| {
            let message = meta.message.as_deref().map(str::to_owned);
            Version {
                hash: meta.hash,
                actor: meta.actor.to_hex_string(),
                time_ms: meta.timestamp.saturating_mul(1000),
                context: entries.read(&meta.actor, message.as_deref()),
                message,
            }
        })
        .collect()
}

/// Commit the open transaction with a wall-clock timestamp attached.
/// `AutoCommit::commit()` leaves the change's advisory time at `0`, which is
/// why routing commits through here is what makes [`Version::time_ms`] — and
/// any date-range filter over the history — mean something. Returns `None`
/// when there was nothing to commit, exactly like `AutoCommit::commit`.
///
/// An empty commit abandons the empty transaction and records nothing.
pub fn commit_stamped(doc: &mut Doc) -> Option<ChangeHash> {
    commit_stamped_at_with_message(doc, tilo_clock::unix_now_i64(), None)
}

/// As [`commit_stamped`], with a caller-supplied Unix time in **seconds**
/// (Automerge's own unit), for hosts that own a clock: a store replaying a
/// durable change chain, a test spreading versions across a day.
pub fn commit_stamped_at(doc: &mut Doc, unix_secs: i64) -> Option<ChangeHash> {
    commit_stamped_at_with_message(doc, unix_secs, None)
}

/// Commit a native document edit with a timestamp and an optional semantic
/// message: the commit-boundary counterpart to action dispatch, so history
/// learns which app and resource an edit belongs to.
pub fn commit_stamped_with_message(doc: &mut Doc, message: Option<String>) -> Option<ChangeHash> {
    commit_stamped_at_with_message(doc, tilo_clock::unix_now_i64(), message)
}

/// [`commit_stamped_with_message`] at a caller-supplied Unix time in seconds.
pub fn commit_stamped_at_with_message(
    doc: &mut Doc,
    unix_secs: i64,
    message: Option<String>,
) -> Option<ChangeHash> {
    // commit_with opens a transaction even when there are no edits, which
    // can add then prune a fresh actor, poisoning the next differently
    // authored transaction. Rolling the empty transaction back discards it
    // with its own patch log branch, so the document's diff cursor — and a
    // `Tracker` following it — stays valid.
    if doc.pending_ops() == 0 {
        doc.rollback();
        return None;
    }
    let mut options = CommitOptions::default().with_time(unix_secs);
    if let Some(message) = message {
        options = options.with_message(message);
    }
    doc.commit_with(options)
}

/// Heads of the document as of the first `upto + 1` changes of [`versions`].
/// Any prefix of that causal order is a valid document state, but with
/// several collaborators it can have several concurrent frontiers, so "the
/// heads" is a set: a change is a head of the prefix when no other change in
/// the prefix depends on it.
pub fn prefix_heads(doc: &mut Doc, upto: usize) -> Vec<ChangeHash> {
    let metas = doc.get_changes_meta(&[]);
    let take = upto.saturating_add(1).min(metas.len());
    let prefix = metas.get(..take).unwrap_or(&[]);
    let depended_on: std::collections::HashSet<ChangeHash> = prefix
        .iter()
        .flat_map(|meta| meta.deps.iter().copied())
        .collect();
    prefix
        .iter()
        .map(|meta| meta.hash)
        .filter(|hash| !depended_on.contains(hash))
        .collect()
}

/// The document as of position `version` of [`versions`], rebuilt by
/// replaying that prefix of the change log into a fresh replica.
pub fn replay_prefix(doc: &mut Doc, version: usize) -> Result<Doc, ModelError> {
    let changes = doc.get_changes(&[]);
    let take = version.saturating_add(1).min(changes.len());
    let mut rebuilt = Doc::new();
    rebuilt.apply_incoming(changes.into_iter().take(take))?;
    Ok(rebuilt)
}

/// `read` of the document at each of `versions` (ascending positions of
/// [`versions`]), from one replay of the change log: equivalent to
/// [`replay_prefix`] per version without replaying the prefix again each
/// time. A version Automerge rejects, and every later one, is `None`.
pub fn replay_series<T>(
    doc: &mut Doc,
    versions: &[usize],
    mut read: impl FnMut(&Doc) -> Option<T>,
) -> Vec<Option<T>> {
    let mut changes = doc.get_changes(&[]).into_iter();
    let total = changes.len();
    let mut rebuilt = Doc::new();
    let mut applied = 0usize;
    let mut failed = false;
    versions
        .iter()
        .map(|&version| {
            let take = version.saturating_add(1).min(total);
            if !failed && take > applied {
                let batch: Vec<_> = changes
                    .by_ref()
                    .take(take.saturating_sub(applied))
                    .collect();
                failed = rebuilt.apply_incoming(batch).is_err();
                applied = take;
            }
            if failed {
                return None;
            }
            read(&rebuilt)
        })
        .collect()
}

/// Where a newest-first walk of the history stopped: hand it back to [`page`]
/// for the next older page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// The newest changes not listed yet, by operation counter: every change
    /// they depend on is older than all of them.
    frontier: Vec<(Rank, ChangeHash)>,
}

/// One page of history, newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub versions: Vec<Version>,
    /// The rest of the history, or `None` when this page reached the start.
    pub older: Option<Cursor>,
}

/// Up to `limit` versions, newest first, starting at the document's heads
/// or where `from` stopped.
///
/// A page reads only the changes it lists and their direct dependencies, so
/// a participant who joins a document with years of history sees its latest
/// changes at once, and older ones load as they scroll back. Every change is
/// listed once, after every change that depends on it: a change's operation
/// counters are greater than those of anything it depends on, so taking the
/// frontier's greatest counter first is a reverse causal order.
pub fn page(doc: &mut Doc, from: Option<&Cursor>, limit: usize) -> Page {
    let mut walk = Walk::default();
    let mut queue: BinaryHeap<(Rank, ChangeHash)> = if let Some(cursor) = from {
        cursor.frontier.iter().copied().collect()
    } else {
        let heads = doc.get_heads();
        heads
            .into_iter()
            .filter_map(|hash| walk.rank(doc, hash).map(|rank| (rank, hash)))
            .collect()
    };
    let mut versions = Vec::new();
    let mut listed = std::collections::HashSet::new();
    while versions.len() < limit {
        let Some((_, hash)) = queue.pop() else { break };
        // A change reached along two paths is queued once per path.
        if !listed.insert(hash) || walk.rank(doc, hash).is_none() {
            continue;
        }
        let Some((_, deps, version)) = walk.metas.get(&hash).cloned() else {
            continue;
        };
        for dep in deps {
            if let Some(rank) = walk.rank(doc, dep) {
                queue.push((rank, dep));
            }
        }
        versions.push(version);
    }
    let mut frontier: Vec<(Rank, ChangeHash)> = queue.into_vec();
    frontier.compact_sort();
    frontier.dedup();
    frontier.retain(|(_, hash)| !listed.contains(hash));
    Page {
        versions,
        older: (!frontier.is_empty()).then_some(Cursor { frontier }),
    }
}

/// A change's place in the newest-first walk: its last operation counter,
/// then its first. An empty change ends at its dependency's last counter but
/// starts after it, so the start breaks that tie.
type Rank = (u64, u64);

#[cfg(test)]
thread_local! {
    /// Change metadata [`Walk`] has read from the document on this thread.
    static META_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The change metadata one [`page`] call has read.
#[derive(Default)]
struct Walk {
    metas: HashMap<ChangeHash, (Rank, Vec<ChangeHash>, Version)>,
}

impl Walk {
    fn rank(&mut self, doc: &Doc, hash: ChangeHash) -> Option<Rank> {
        if let Some((rank, _, _)) = self.metas.get(&hash) {
            return Some(*rank);
        }
        let document = doc.committed_document();
        let meta = document.get_change_meta_by_hash(&hash)?;
        #[cfg(test)]
        META_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
        let rank = (meta.max_op, meta.start_op);
        let entry = (rank, meta.deps.clone(), version_of(document, &meta));
        self.metas.insert(hash, entry);
        Some(rank)
    }
}

/// What one change wrote to one property of an object: the value the
/// property held before the change (at the change's dependencies) and after
/// it (at the change itself), each as JSON, `null` where it held nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    /// The property of the object: a field's key.
    pub prop: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
}

/// The keys of the map `obj` the change `hash` of `doc` reached, in the
/// order it first reached each: a key it set or removed, or one holding an
/// object it changed inside (the text of a field typed into, a list or map
/// of it), read from the change's own operations. Empty for a change `doc`
/// does not hold. A reader that decodes the values itself reads each key at
/// the change's dependencies and at the change ([`written`] reads them as
/// JSON).
pub fn wrote(
    doc: &automerge::Automerge,
    hash: &ChangeHash,
    obj: &automerge::ObjId,
) -> Result<Vec<String>, ModelError> {
    use automerge::ReadDoc as _;
    let Some(change) = doc.get_change_by_hash(hash) else {
        return Ok(Vec::new());
    };
    let after_heads = [*hash];
    let mut keys: Vec<String> = Vec::new();
    // The keys the change's named objects sit under, each read once: a change of a long note
    // names one object per character typed, and Automerge's way up from an object searches
    // the document's every operation for that object's op, so per op it is the whole document.
    let mut reached: Vec<(automerge::ObjId, Option<String>)> = Vec::new();
    for op in change.decode().operations {
        let target = op_obj(doc, &op.obj)?;
        let key = if target == *obj {
            match &op.key {
                automerge::legacy::Key::Map(key) => Some(key.to_string()),
                automerge::legacy::Key::Seq(_) => None,
            }
        } else if let Some((_, key)) = reached.iter().find(|(at, _)| *at == target) {
            key.clone()
        } else {
            // An operation inside an object under `obj`: the key of `obj` on its way up.
            let key = doc.parents_at(&target, &after_heads)?.find_map(|parent| {
                match (parent.obj == *obj, parent.prop) {
                    (true, automerge::Prop::Map(key)) => Some(key),
                    _ => None,
                }
            });
            reached.push((target.clone(), key.clone()));
            key
        };
        if let Some(key) = key
            && !keys.contains(&key)
        {
            keys.push(key);
        }
    }
    Ok(keys)
}

/// What the change `hash` of `doc` wrote under the map `obj`, one entry per
/// key of `obj` it reached ([`wrote`]), with the values before and after
/// read as JSON from the document as of the change's dependencies and as of
/// the change, in place. A key whose value reads the same on both sides (a
/// write of what it held) is left out. Empty for a change `doc` does not
/// hold.
pub fn written(
    doc: &automerge::Automerge,
    hash: &ChangeHash,
    obj: &automerge::ObjId,
) -> Result<Vec<Written>, ModelError> {
    use automerge::ReadDoc as _;
    let Some(change) = doc.get_change_by_hash(hash) else {
        return Ok(Vec::new());
    };
    let before_heads = change.deps().to_vec();
    let after_heads = [*hash];
    let keys = wrote(doc, hash, obj)?;
    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        let before = held_at(doc, obj, &key, &before_heads)?;
        let after = held_at(doc, obj, &key, &after_heads)?;
        if before != after {
            out.push(Written {
                prop: key,
                before,
                after,
            });
        }
    }
    Ok(out)
}

/// What the key `key` of the map `obj` held as of `heads`, as JSON, `null`
/// where it held nothing (or `obj` did not exist yet), read in place: the
/// cost of the value, not of the document.
pub fn held_at(
    doc: &automerge::Automerge,
    obj: &automerge::ObjId,
    key: &str,
    heads: &[ChangeHash],
) -> Result<serde_json::Value, ModelError> {
    use automerge::ReadDoc as _;
    match doc.get_at(obj, key, heads)? {
        Some((value, inner)) => crate::value::read_value_at(doc, &value, &inner, heads),
        None => Ok(serde_json::Value::Null),
    }
}

/// One text a change typed into: where it sits under the object asked about, and what
/// the change did to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    /// The map keys from the object [`spliced_texts`] was given down to this text, in
    /// reading order: a rich text field's `blocks`, its paragraph's id, `text`.
    pub path: Vec<String>,
    /// What the text read before the change and what it reads after it, whole: a reader
    /// that shows a few words around the change cuts them itself.
    pub before: String,
    pub after: String,
    /// What the change inserted: the character range of `after` it wrote, and its text.
    pub inserted: Vec<(usize, usize, String)>,
    /// What the change took away: the character range of `before` it deleted, and its text.
    pub deleted: Vec<(usize, usize, String)>,
}

impl TextEdit {
    /// Whether the change left the text as it was: a mark, not a splice.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inserted.is_empty() && self.deleted.is_empty()
    }

    /// The name of the paragraph this text is in, when the text sits under one by a map
    /// key: for rich text, the id of the paragraph whose `text` this is.
    #[must_use]
    pub fn block(&self) -> &str {
        self.path.iter().rev().nth(1).map_or("", String::as_str)
    }
}

/// The texts the change `hash` of `doc` spliced into, below the object `root`, each with
/// what the change inserted into it and what it took away.
///
/// Which texts, from the change's own operations: a text object an operation of the change
/// names is one it spliced, and each is read in place, whole, at the two sides of the change
/// ([`text_at`]), and the two read as what changed. What a change inserted and what it took
/// away is what the person sees: a word replaced whole reads as that word, never as the
/// letters it happens to share with the one it replaced.
///
/// The cost is the texts the change spliced: rich text keeps each paragraph in a text of
/// its own, so a word typed into one paragraph of a long note reads that paragraph, twice,
/// and nothing else, and neither the note's length nor the document's is read. (Reading a
/// rich text whole at past heads, or finding a character's place in a text this way, costs
/// the document's operations: measured, see the test below.)
///
/// A change that only marked a run reads as no splice ([`TextEdit::is_empty`]).
pub fn spliced_texts(
    doc: &automerge::Automerge,
    hash: &ChangeHash,
    root: &automerge::ObjId,
) -> Result<Vec<TextEdit>, ModelError> {
    use automerge::ReadDoc as _;
    let Some(change) = doc.get_change_by_hash(hash) else {
        return Ok(Vec::new());
    };
    let before_heads = change.deps().to_vec();
    let after_heads = [*hash];
    // Which objects the change spliced, from its own operations: each op names its object.
    // No op is looked up in the document: that search is the document's operations, and a
    // change of a long note has as many ops as it typed characters.
    let mut spliced: Vec<automerge::ObjId> = Vec::new();
    for op in &change.decode().operations {
        // A mark names the text it covers too, but marks a run of it rather than splicing
        // it; only a put or a delete changes what the text says.
        if matches!(
            op.action,
            automerge::legacy::OpType::MarkBegin(_) | automerge::legacy::OpType::MarkEnd(_)
        ) {
            continue;
        }
        let target = op_obj(doc, &op.obj)?;
        if !spliced.contains(&target) {
            spliced.push(target);
        }
    }
    // Where each of them sits under `root`, from one walk of the rich text as it reads now.
    let paths = paths_of(doc, root);
    let mut out: Vec<TextEdit> = Vec::with_capacity(spliced.len());
    for target in spliced {
        let Some((_, path)) = paths
            .iter()
            .find(|(at, path)| *at == target && !path.is_empty())
        else {
            continue;
        };
        let path = path.clone();
        let before = text_at(doc, &target, &before_heads)?;
        let after = text_at(doc, &target, &after_heads)?;
        let (inserted, deleted) = spliced_runs(&before, &after);
        out.push(TextEdit {
            path,
            before,
            after,
            inserted,
            deleted,
        });
    }
    Ok(out)
}

/// The region of a text a change rewrote, as `(start, end)`: what the two sides do not share
/// at their start and their end.
fn region(before: &str, after: &str) -> (usize, usize, usize) {
    let kept_start = common_chars(before, after, true);
    let kept_end = common_chars(before, after, false);
    (
        kept_start,
        before.chars().count().saturating_sub(kept_end),
        after.chars().count().saturating_sub(kept_end),
    )
}

/// What `text` says, as a reader reads it: the words outside the block markers a rich text
/// lays its structure out with (each block's kind and attributes between two markers), its
/// placeholders dropped.
fn said(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut open = 0usize;
    for ch in text.chars() {
        match ch {
            '\u{fffc}' => open ^= 1,
            '\u{fffe}' | '\u{fffd}' => {}
            other if open == 0 => out.push(other),
            _ => {}
        }
    }
    out
}

/// What one text the change read at both sides made of it: the ranges of `after` the change
/// inserted and the ranges of `before` it removed, each with its text, in reading order.
///
/// The runs of one text a change inserted and the runs it removed: each `(start, end, text)`.
type Spliced = (Vec<(usize, usize, String)>, Vec<(usize, usize, String)>);

/// The two sides differ in one region: what both share at their start and at their end is
/// what the change kept. A length of zero either way is a text the change made or emptied,
/// and the whole of it is the change's.
fn spliced_runs(before: &str, after: &str) -> Spliced {
    let (kept_start, deleted_end, inserted_end) = region(before, after);
    // A change to the text's structure alone (an empty paragraph made, a body written whole
    // over an empty one) writes nothing a reader reads.
    let speaks = said(before).trim() != said(after).trim();
    let spoken = |text: &str, start: usize, end: usize| {
        speaks && !chars_between(text, start, end).trim().is_empty()
    };
    let inserted =
        (kept_start < inserted_end && spoken(after, kept_start, inserted_end)).then(|| {
            (
                kept_start,
                inserted_end,
                chars_between(after, kept_start, inserted_end),
            )
        });
    let deleted =
        (kept_start < deleted_end && spoken(before, kept_start, deleted_end)).then(|| {
            (
                kept_start,
                deleted_end,
                chars_between(before, kept_start, deleted_end),
            )
        });
    (
        inserted.into_iter().collect(),
        deleted.into_iter().collect(),
    )
}

/// How many characters `a` and `b` share at their start (`from_start`) or their end.
fn common_chars(a: &str, b: &str, from_start: bool) -> usize {
    let (a, b) = if from_start {
        (a.chars().collect::<Vec<_>>(), b.chars().collect::<Vec<_>>())
    } else {
        (
            a.chars().rev().collect::<Vec<_>>(),
            b.chars().rev().collect::<Vec<_>>(),
        )
    };
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

/// The characters of `text` in `[start, end)`.
fn chars_between(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

/// The object the key `key` of the map `obj` holds as of `heads`, or `None` where it
/// holds none: one seek into the map, never a walk of it.
pub fn object_at(
    doc: &automerge::Automerge,
    obj: &automerge::ObjId,
    key: &str,
    heads: &[ChangeHash],
) -> Result<Option<automerge::ObjId>, ModelError> {
    automerge::ReadDoc::get_at(doc, obj, key, heads)
        .map(|held| match held {
            Some((automerge::Value::Object(_), child)) => Some(child),
            _ => None,
        })
        .map_err(ModelError::from)
}

/// The text the text object `obj` held as of `heads`, read in place: the cost of this
/// text, never of the document.
///
/// # Errors
/// `obj` is not a text object of the document.
pub fn text_at(
    doc: &automerge::Automerge,
    obj: &automerge::ObjId,
    heads: &[ChangeHash],
) -> Result<String, ModelError> {
    automerge::ReadDoc::text_at(doc, obj, heads).map_err(ModelError::from)
}

/// How deep the keys from a rich text's field down to a paragraph's text run at most: the
/// text of a block of the body (`b1.text`), of a note, or of a comment.
const DEEPEST_TEXT: usize = 3;

/// The map keys from `root` down to the object `obj`, outermost first, or `None` when `obj`
/// does not sit `DEEPEST_TEXT` keys below `root`.
///
/// Read from the top, once for the rich text as it reads now: each map's children are its
/// own, so the walk costs the maps the text holds and nothing of the document. (Automerge's
/// way up from an object searches the document's every operation for that object's op, and
/// its way down at a past version seeks each key a whole document reads: either is the
/// note's length, and a reader of one entry cannot pay that. Only the texts a change spliced
/// are read at the past version, and only their own characters.)
fn paths_of(
    doc: &automerge::Automerge,
    root: &automerge::ObjId,
) -> Vec<(automerge::ObjId, Vec<String>)> {
    let mut out = Vec::new();
    walk(doc, root, &mut Vec::new(), DEEPEST_TEXT, &mut out);
    out
}

fn walk(
    doc: &automerge::Automerge,
    at: &automerge::ObjId,
    path: &mut Vec<String>,
    depth: usize,
    out: &mut Vec<(automerge::ObjId, Vec<String>)>,
) {
    use automerge::ReadDoc as _;
    out.push((at.clone(), path.clone()));
    if depth == 0 {
        return;
    }
    for key in doc.keys(at) {
        if let Ok(Some((automerge::Value::Object(_), child))) = doc.get(at, key.as_str()) {
            path.push(key.clone());
            walk(doc, &child, path, depth.saturating_sub(1), out);
            path.pop();
        }
    }
}

/// The object a change's operation names, as the document addresses it.
fn op_obj(
    doc: &automerge::Automerge,
    obj: &automerge::legacy::ObjectId,
) -> Result<automerge::ObjId, ModelError> {
    Ok(match obj {
        automerge::legacy::ObjectId::Root => automerge::ROOT,
        automerge::legacy::ObjectId::Id(id) => doc.import_obj(&id.to_string())?,
    })
}

pub fn read_at<R: Resource>(doc: &mut Doc, heads: &[ChangeHash]) -> Result<R::State, ModelError> {
    let past = doc.fork_at(heads)?;
    R::read(&past)
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ActorId, ROOT, ReadDoc};

    #[test]
    fn the_first_page_of_a_long_history_reads_only_that_page() {
        let mut doc = Doc::new();
        for n in 0..2_000_i64 {
            doc.put(ROOT, "tag", format!("t{}", n % 7)).unwrap();
            commit_stamped_at(&mut doc, n).unwrap();
        }
        META_READS.with(|reads| reads.set(0));
        let page = page(&mut doc, None, 50);
        let read = META_READS.with(std::cell::Cell::get);
        assert_eq!(page.versions.len(), 50);
        assert!(page.older.is_some());
        // Each listed change and the dependency that queues after it.
        assert!(read <= 51, "a page of 50 read {read} changes of 2,000");
    }

    #[test]
    fn a_change_says_what_it_wrote_with_the_values_before_and_after() {
        use automerge::transaction::Transactable as _;
        use automerge::{ObjType, ScalarValue};
        let mut doc = Doc::new();
        let note = doc.put_object(ROOT, "note", ObjType::Map).unwrap();
        let title = doc.put_object(&note, "title", ObjType::Text).unwrap();
        doc.splice_text(&title, 0, 0, "Groceries").unwrap();
        doc.put(&note, "due", "2026-10-30").unwrap();
        doc.put(&note, "pinned", false).unwrap();
        let first = commit_stamped_at(&mut doc, 1).unwrap();
        // One step: the title typed into, the due day moved, `pinned` written as it was.
        doc.splice_text(&title, 0, 9, "Shopping").unwrap();
        doc.put(&note, "due", "2026-11-02").unwrap();
        doc.put(&note, "pinned", ScalarValue::Boolean(false))
            .unwrap();
        let step = commit_stamped_at(&mut doc, 2).unwrap();
        // A later step the earlier one's values do not see.
        doc.put(&note, "due", "2026-12-24").unwrap();
        commit_stamped_at(&mut doc, 3).unwrap();
        let wrote = written(doc.document(), &step, &note).unwrap();
        assert_eq!(
            wrote,
            vec![
                Written {
                    prop: "title".into(),
                    before: "Groceries".into(),
                    after: "Shopping".into(),
                },
                Written {
                    prop: "due".into(),
                    before: "2026-10-30".into(),
                    after: "2026-11-02".into(),
                },
            ]
        );
        // The first step made everything it wrote: nothing before.
        let made = written(doc.document(), &first, &ROOT).unwrap();
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].prop, "note");
        assert!(made[0].before.is_null());
        assert_eq!(made[0].after["title"], "Groceries");
    }

    #[test]
    fn a_typed_paragraph_says_which_keys_of_characters_it_typed_and_erased() {
        use automerge::transaction::Transactable as _;
        use automerge::{ObjType, ScalarValue};
        let mut doc = Doc::new();
        let note = doc.put_object(ROOT, "note", ObjType::Map).unwrap();
        let body = doc.put_object(&note, "body", ObjType::Map).unwrap();
        let blocks = doc.put_object(&body, "blocks", ObjType::Map).unwrap();
        let first = doc.put_object(&blocks, "b1", ObjType::Map).unwrap();
        let text = doc.put_object(&first, "text", ObjType::Text).unwrap();
        let other = doc.put_object(&blocks, "b2", ObjType::Map).unwrap();
        let other_text = doc.put_object(&other, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "Groceries and milk").unwrap();
        doc.splice_text(&other_text, 0, 0, "Untouched").unwrap();
        commit_stamped_at(&mut doc, 1).unwrap();
        // One step: a word replaced in the first paragraph, a title written beside it.
        doc.splice_text(&text, 14, 4, "bread").unwrap();
        doc.put(&note, "title", ScalarValue::Str("Shopping".into()))
            .unwrap();
        let step = commit_stamped_at(&mut doc, 2).unwrap();
        let edits = spliced_texts(doc.document(), &step, &body).unwrap();
        assert_eq!(edits.len(), 1, "only the paragraph typed into: {edits:?}");
        let edit = &edits[0];
        assert_eq!(edit.path, ["blocks", "b1", "text"]);
        let inserted: String = edit
            .inserted
            .iter()
            .map(|(_, _, text)| text.as_str())
            .collect();
        let deleted: String = edit
            .deleted
            .iter()
            .map(|(_, _, text)| text.as_str())
            .collect();
        assert_eq!(inserted, "bread");
        assert_eq!(deleted, "milk");
        assert_eq!(edit.before, "Groceries and milk");
        assert_eq!(edit.after, "Groceries and bread");
        assert!(!edit.is_empty());
    }

    #[test]
    fn a_body_retyped_whole_reads_as_what_was_there_and_what_was_written() {
        use automerge::ObjType;
        use automerge::transaction::Transactable as _;
        let mut doc = Doc::new();
        let body = doc.put_object(ROOT, "body", ObjType::Map).unwrap();
        let text = doc.put_object(&body, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "Groceries and milk").unwrap();
        commit_stamped_at(&mut doc, 1).unwrap();
        // A control hands over its whole text: the whole of what was there goes, and what
        // was typed in its place comes.
        doc.splice_text(&text, 0, 15, "Oat milk and bread").unwrap();
        let step = commit_stamped_at(&mut doc, 2).unwrap();
        let edits = spliced_texts(doc.document(), &step, &body).unwrap();
        let edit = &edits[0];
        let inserted: String = edit
            .inserted
            .iter()
            .map(|(_, _, text)| text.as_str())
            .collect();
        let deleted: String = edit
            .deleted
            .iter()
            .map(|(_, _, text)| text.as_str())
            .collect();
        assert_eq!(inserted, "Oat milk and bread");
        // "il" is shared by "milk" and "bread" at the end, so those two stand outside the
        // region the change rewrote: the reader sees the part that differs, as anybody
        // reading the two texts sees it.
        assert_eq!(deleted, "Groceries and m");
        assert_eq!(edit.before, "Groceries and milk");
        // The trailing "il" the record kept is still there, after the typed text.
        assert_eq!(edit.after, "Oat milk and breadilk");
    }

    #[test]
    fn a_paragraph_the_change_only_marked_reads_as_no_splice() {
        use automerge::ObjType;
        use automerge::marks::Mark;
        use automerge::transaction::Transactable as _;
        let mut doc = Doc::new();
        let body = doc.put_object(ROOT, "body", ObjType::Map).unwrap();
        let text = doc.put_object(&body, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "Groceries").unwrap();
        commit_stamped_at(&mut doc, 1).unwrap();
        doc.mark(
            &text,
            Mark::new("strong".to_owned(), true, 0, 9),
            automerge::marks::ExpandMark::Both,
        )
        .unwrap();
        let step = commit_stamped_at(&mut doc, 2).unwrap();
        let edits = spliced_texts(doc.document(), &step, &body).unwrap();
        assert!(edits.iter().all(TextEdit::is_empty), "{edits:?}");
    }

    fn check_empty_commit_actor_transition(open_empty: bool) {
        let mut doc = Doc::new();
        doc.set_actor(ActorId::from(vec![0x80]));
        doc.put(ROOT, "body", "seed").unwrap();
        commit_stamped_at(&mut doc, 1).unwrap();
        let heads = doc.get_heads();
        doc.set_actor(ActorId::from(vec![0x10]));
        if open_empty {
            doc.delete(ROOT, "absent").unwrap();
            assert_eq!(doc.pending_ops(), 0);
        }
        assert_eq!(commit_stamped_at(&mut doc, 2), None);
        assert_eq!(doc.get_heads(), heads);
        doc.set_actor(ActorId::from(vec![0x20]));
        doc.put(ROOT, "body", "edited").unwrap();
        let change = commit_stamped_at(&mut doc, 3).unwrap();
        assert_eq!(doc.get_change_by_hash(&change).unwrap().timestamp(), 3);
        let after = doc.get_heads();
        let patches = doc.diff(&heads, &after);
        assert!(matches!(patches.as_slice(), [automerge::Patch {
            action: automerge::PatchAction::PutMap { key, value, .. }, ..
        }] if key == "body" && value.0.as_str() == Some("edited")));
    }

    #[test]
    fn empty_stamped_commit_does_not_poison_next_actor() {
        check_empty_commit_actor_transition(false);
    }

    #[test]
    fn empty_open_transaction_does_not_poison_next_actor() {
        check_empty_commit_actor_transition(true);
    }

    #[test]
    fn an_empty_stamped_commit_keeps_changes_tracked_incrementally() {
        let mut doc = Doc::new();
        doc.put(ROOT, "n", 0).unwrap();
        commit_stamped_at(&mut doc, 1).unwrap();
        let mut tracker = crate::Tracker::new();
        tracker.start(&mut doc);
        for (n, actor) in [(1_i64, 0x10_u8), (2, 0x20)] {
            doc.set_actor(ActorId::from(vec![actor]));
            assert_eq!(commit_stamped_at(&mut doc, 2), None);
            doc.put(ROOT, "n", n).unwrap();
            commit_stamped_at(&mut doc, 3).unwrap();
            let delta = tracker.delta(&mut doc);
            assert!(!delta.is_everything(), "round {n}");
            assert_eq!(delta.key(&ROOT, "n"), crate::Touch::Written);
        }
    }

    /// Two collaborators' interleaved, combined edits.
    fn combined() -> Doc {
        let mut left = Doc::new();
        left.put(ROOT, "n", 0).unwrap();
        commit_stamped_at(&mut left, 1).unwrap();
        let mut right = left.fork();
        for round in 1..8_i64 {
            left.put(ROOT, "left", round).unwrap();
            commit_stamped_at(&mut left, round).unwrap();
            right.put(ROOT, "right", round).unwrap();
            commit_stamped_at(&mut right, round).unwrap();
            if round % 3 == 0 {
                left.merge_incoming(&mut right).unwrap();
                right.merge_incoming(&mut left).unwrap();
            }
        }
        left.merge_incoming(&mut right).unwrap();
        left
    }

    #[test]
    fn positions_mean_the_same_to_every_addressing_function() {
        let mut doc = combined();
        let listed: Vec<ChangeHash> = versions(&mut doc).iter().map(|v| v.hash).collect();
        let replayed: Vec<ChangeHash> = doc
            .get_changes(&[])
            .iter()
            .map(automerge::Change::hash)
            .collect();
        assert_eq!(listed, replayed, "metadata and changes list in one order");
        for position in 0..listed.len() {
            let mut rebuilt = replay_prefix(&mut doc, position).unwrap();
            let mut heads = rebuilt.get_heads();
            heads.compact_sort();
            let mut expected = prefix_heads(&mut doc, position);
            expected.compact_sort();
            assert_eq!(heads, expected, "position {position}");
        }
        let last = listed.len() - 1;
        let series = replay_series(&mut doc, &[0, 3, last], |doc| {
            doc.get(ROOT, "left")
                .ok()
                .flatten()
                .map(|(value, _)| value.to_string())
        });
        assert_eq!(series.len(), 3);
        assert_eq!(series[0], None);
        assert!(series[2].is_some());
    }
}
