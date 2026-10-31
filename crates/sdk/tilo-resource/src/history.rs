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
///
/// # Errors
/// An operation names an object the document does not hold.
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
    for op in change.decode().operations {
        let target = op_obj(doc, &op.obj)?;
        let key = if target == *obj {
            match &op.key {
                automerge::legacy::Key::Map(key) => Some(key.to_string()),
                automerge::legacy::Key::Seq(_) => None,
            }
        } else {
            // An operation inside an object under `obj`: the key of `obj` on its way up.
            doc.parents_at(&target, &after_heads)?.find_map(|parent| {
                match (parent.obj == *obj, parent.prop) {
                    (true, automerge::Prop::Map(key)) => Some(key),
                    _ => None,
                }
            })
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
///
/// # Errors
/// A value the document holds cannot be read as JSON.
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
///
/// # Errors
/// The value cannot be read as JSON.
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
