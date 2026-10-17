//! Undoing somebody else's work: a contiguous run of changes, addressed by
//! what it is rather than by where it sits on a local stack.
//!
//! [`UndoLog`](crate::state::UndoLog) only holds actions *this* process
//! handled — a peer's change re-baselines without pushing — so "undo what
//! the agent just did" finds nothing there. A run is addressed instead by
//! content-addressed [`HistoryEntryId`], so a peer that only ever received
//! those changes can undo them. The inverse is written *forward* through
//! [`write_patches`](crate::state::write_patches), never cut out of the
//! graph, so the original keeps its author and the undo is itself undoable.
//!
//! Not wholesale: inverting a splice deletes everything in the span,
//! including what somebody typed there afterwards. So a [`Slot`] written
//! since the run is left alone and reported in [`RevertOutcome::kept`], and
//! a caller must consult [`RevertOutcome::is_complete`] before saying
//! "undone".

use std::collections::{HashMap, HashSet};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, Patch, PatchAction, Prop, ReadDoc};

use crate::history_source::authors::AuthorDirectory;
use crate::history_source::entry_id_from_hash;
use crate::state::write_selected_patches;
use crate::widget::history::{HistoryAuthor, HistoryEntryId};

/// A contiguous run of changes, addressed by its first and last change.
///
/// Both ends are inclusive and content-addressed, so the same `ChangeRun` names
/// the same work on every peer that has those changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeRun {
    pub start: HistoryEntryId,
    pub end: HistoryEntryId,
}

impl ChangeRun {
    pub fn new(start: HistoryEntryId, end: HistoryEntryId) -> Self {
        Self { start, end }
    }

    pub fn single(only: HistoryEntryId) -> Self {
        Self {
            start: only,
            end: only,
        }
    }
}

/// A place in the document a change wrote to, at the granularity where two
/// writers collide.
///
/// Deliberately *not* the whole object for sequences: two people appending
/// different paragraphs to the same list have not collided, and treating the
/// shared parent as a conflict would make almost every run un-undoable in a
/// document anybody is still working in.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    /// One text object — the case where an undo would eat somebody's words.
    Text(ObjId),
    /// One key of one map. Two writers on the same key is a plain overwrite.
    Field(ObjId, String),
    /// A whole object the run brought into existence that somebody has since
    /// written inside. Undoing the run would remove the container and take
    /// their work with it, so the container stays.
    Object(ObjId),
}

/// A slot the revert declined to touch, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct KeptSlot {
    pub slot: Slot,
    /// Whoever wrote to it after the run, resolved through the caller's
    /// directory so the reason can name a person rather than an actor hex.
    pub by: HistoryAuthor,
    /// Actor hex, for a caller grouping by identity rather than display name.
    pub actor_hex: String,
}

/// What a revert did, and what it deliberately did not do.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RevertOutcome {
    /// Changes the run covered.
    pub covered: usize,
    /// Slots whose effect was inverted.
    pub reverted: usize,
    /// Slots left alone because somebody edited them after the run. Empty is
    /// the ordinary case; non-empty means this revert was partial **and the
    /// caller must say so**.
    pub kept: Vec<KeptSlot>,
    /// True when the document changed. A run whose effect had already
    /// been undone reverts nothing and is not an error.
    pub wrote: bool,
}

impl RevertOutcome {
    /// True when every slot the run wrote was inverted — i.e. the caller may
    /// report that the work was undone completely.
    pub fn is_complete(&self) -> bool {
        self.kept.is_empty()
    }

    /// Distinct people whose later edits held part of the run back, in
    /// first-seen order.
    pub fn blocked_by(&self) -> Vec<HistoryAuthor> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for k in &self.kept {
            if seen.insert(k.actor_hex.clone()) {
                out.push(k.by.clone());
            }
        }
        out
    }
}

/// Why a run could not be addressed at all — distinct from "addressed it and
/// chose to keep some of it", which is [`RevertOutcome::kept`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevertError {
    /// Neither end of the run names a change in this document. The usual cause
    /// is asking the wrong document — a run recorded against the photo
    /// editor's, handed to the word processor's.
    UnknownRun,
    /// The run's ends are in the document but the end precedes the start.
    Inverted,
}

/// Both returned head sets are prefix frontiers of the document's own causal
/// order, so they are valid arguments to `AutoCommit::diff` and mean the
/// same thing on every peer holding the same changes.
///
/// # Errors
///
/// [`RevertError::UnknownRun`] when neither end of `run` names a change in
/// `doc` — the usual cause is a run recorded against a different document.
/// [`RevertError::Inverted`] when the run's end precedes its start.
pub fn run_bounds(
    doc: &mut AutoCommit,
    run: &ChangeRun,
) -> Result<(Vec<ChangeHash>, Vec<ChangeHash>, usize), RevertError> {
    let versions = tilo_resource::history::versions(doc);
    let mut first = None;
    let mut last = None;
    for (index, v) in versions.iter().enumerate() {
        let id = entry_id_from_hash(&v.hash.to_string());
        if id == run.start && first.is_none() {
            first = Some(index);
        }
        if id == run.end {
            last = Some(index);
        }
    }
    let (Some(i0), Some(i1)) = (first, last) else {
        return Err(RevertError::UnknownRun);
    };
    if i1 < i0 {
        return Err(RevertError::Inverted);
    }
    // A run starting at the document's very first change is bracketed by the
    // empty document — the starting state, and a valid head set for `diff`.
    let before = if i0 == 0 {
        Vec::new()
    } else {
        // `i0 != 0` in this branch, so this matches plain subtraction.
        tilo_resource::history::prefix_heads(doc, i0.saturating_sub(1))
    };
    let after = tilo_resource::history::prefix_heads(doc, i1);
    // `i1 >= i0` was just checked above, so the subtraction never
    // saturates and the result matches `i1 - i0 + 1` exactly.
    Ok((before, after, i1.saturating_sub(i0).saturating_add(1)))
}

/// Structural sequence work returns `None` on purpose: `write_patches`
/// re-anchors those positions through cursors, so a concurrent insert
/// elsewhere in the same list is already handled and is not a reason to refuse.
fn slot_of(doc: &AutoCommit, p: &Patch) -> Option<Slot> {
    let is_text = matches!(doc.object_type(&p.obj), Ok(ObjType::Text));
    match &p.action {
        PatchAction::SpliceText { .. } | PatchAction::Mark { .. } => {
            Some(Slot::Text(p.obj.clone()))
        }
        PatchAction::DeleteSeq { .. } | PatchAction::Insert { .. } | PatchAction::PutSeq { .. } => {
            is_text.then(|| Slot::Text(p.obj.clone()))
        }
        PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
            Some(Slot::Field(p.obj.clone(), key.clone()))
        }
        PatchAction::Increment { prop, .. } => match prop {
            Prop::Map(k) => Some(Slot::Field(p.obj.clone(), k.clone())),
            Prop::Seq(_) => is_text.then(|| Slot::Text(p.obj.clone())),
        },
        // A conflict marker carries no content.
        PatchAction::Conflict { .. } => None,
    }
}

/// Index of the first change after the run bracketed by `after`.
///
/// Found by walking prefix frontiers rather than comparing hashes, because
/// the run's last change need not be the only head of that prefix — with
/// two collaborators a prefix can have several concurrent frontiers.
fn first_index_after(doc: &mut AutoCommit, after: &[ChangeHash], total: usize) -> usize {
    let after_set: HashSet<ChangeHash> = after.iter().copied().collect();
    for i in 0..total {
        let hs: HashSet<ChangeHash> = tilo_resource::history::prefix_heads(doc, i)
            .into_iter()
            .collect();
        if hs == after_set {
            // `i < total` inside this loop, so this never saturates.
            return i.saturating_add(1);
        }
    }
    total
}

/// Objects the run brought into existence.
///
/// Undoing an insertion removes the *container*, not just its contents. If
/// somebody has since written inside a paragraph the run created, the dangerous
/// patch is the structural delete of that paragraph — and it mentions only an
/// index, so knowing what it would destroy means knowing which objects the run
/// made.
fn objects_created(forward: &[Patch]) -> HashSet<ObjId> {
    let mut out = HashSet::new();
    for p in forward {
        match &p.action {
            PatchAction::PutMap { value, .. } | PatchAction::PutSeq { value, .. } => {
                if matches!(value.0, automerge::Value::Object(_)) {
                    out.insert(value.1.clone());
                }
            }
            PatchAction::Insert { values, .. } => {
                for (v, id, _) in values.iter() {
                    if matches!(v, automerge::Value::Object(_)) {
                        out.insert(id.clone());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Slots written by changes that landed after `after`, mapped to the actor
/// that wrote them.
///
/// Walked change by change rather than as one aggregate diff, because the
/// aggregate cannot say *who* — and "kept, because Alice edited it" is the
/// whole value of the report. Changes by `by` itself are skipped: a later
/// change by the author being undone is their own business, not somebody
/// building on it. A later change landing anywhere inside an object from
/// `created` (visible via `Patch::path`) protects that whole object, since
/// the inverse would delete it outright.
fn later_writers(
    doc: &mut AutoCommit,
    after: &[ChangeHash],
    by: Option<&str>,
    invoker: Option<&str>,
    created: &HashSet<ObjId>,
) -> HashMap<Slot, String> {
    let versions = tilo_resource::history::versions(doc);
    let start_at = first_index_after(doc, after, versions.len());
    let mut out: HashMap<Slot, String> = HashMap::new();
    for i in start_at..versions.len() {
        // `i` is always `< versions.len()` by the loop bound; retain the
        // checked access for a panic-free path.
        let Some(v) = versions.get(i).cloned() else {
            continue;
        };
        // The same writer, or the same authenticated invoker writing from
        // another of their writers (a mount, a device, their undo), builds
        // on nobody else's work.
        if by.is_some_and(|b| b.eq_ignore_ascii_case(&v.actor))
            || invoker.is_some_and(|invoker| {
                v.context
                    .as_ref()
                    .and_then(|context| context.invoker.as_ref())
                    .is_some_and(|other| other.actor == invoker)
            })
        {
            continue;
        }
        let lo = if i == 0 {
            Vec::new()
        } else {
            // `i != 0` in this branch, so this matches plain subtraction.
            tilo_resource::history::prefix_heads(doc, i.saturating_sub(1))
        };
        let hi = tilo_resource::history::prefix_heads(doc, i);
        let patches = doc.diff(&lo, &hi);
        // A change that only wrote a group or run marker did not touch content.
        if crate::history_source::groups::is_group_marker(&patches)
            || crate::history_source::runs::is_run_marker(&patches)
        {
            continue;
        }
        for p in &patches {
            if let Some(s) = slot_of(doc, p) {
                out.entry(s).or_insert_with(|| v.actor.clone());
            }
            // Every created ancestor is protected: the inverse deletes the
            // *outermost* container, so protecting only the innermost object
            // would let the whole paragraph go anyway.
            for o in std::iter::once(&p.obj).chain(p.path.iter().map(|(o, _)| o)) {
                if created.contains(o) {
                    out.entry(Slot::Object(o.clone()))
                        .or_insert_with(|| v.actor.clone());
                }
            }
        }
    }
    out
}

/// The object living at `prop` of `obj` as of `after`, when it is one the run
/// created and somebody has since written inside.
fn blocked_container(
    doc: &AutoCommit,
    after: &[ChangeHash],
    obj: &ObjId,
    prop: impl Into<Prop>,
    later: &HashMap<Slot, String>,
) -> Option<(Slot, String)> {
    let (value, id) = doc.get_at(obj, prop, after).ok().flatten()?;
    if !matches!(value, automerge::Value::Object(_)) {
        return None;
    }
    let slot = Slot::Object(id);
    let actor = later.get(&slot)?.clone();
    Some((slot, actor))
}

/// Two kinds of hold-back: a patch writing to a [`Slot`] somebody has
/// edited since would overwrite their edit; a patch *removing a container*
/// the run created that somebody has since written inside would delete
/// their work outright. A multi-element removal is split so the elements
/// nobody touched still go.
#[allow(
    clippy::too_many_lines,
    reason = "classification keeps the patch shadow, conflict attribution, and write decision in one auditable pass"
)]
fn classify(
    doc: &AutoCommit,
    inverse: Vec<Patch>,
    later: &HashMap<Slot, String>,
    after: &[ChangeHash],
    covered: usize,
    authors: &AuthorDirectory,
) -> (RevertOutcome, Vec<(Patch, bool)>) {
    let mut writable = Vec::new();
    let mut list_indices: HashMap<ObjId, Vec<Option<usize>>> = HashMap::new();
    let mut kept: Vec<KeptSlot> = Vec::new();
    let mut kept_seen: HashSet<Slot> = HashSet::new();
    let mut reverted_slots: HashSet<Slot> = HashSet::new();
    let mut structural = 0usize;

    for p in inverse {
        // Removals first: they are the patches that can destroy a container.
        match &p.action {
            PatchAction::Insert { index, values }
                if matches!(doc.object_type(&p.obj), Ok(ObjType::List)) =>
            {
                let order = list_indices
                    .entry(p.obj.clone())
                    .or_insert_with(|| (0..doc.length_at(&p.obj, after)).map(Some).collect());
                let index = (*index).min(order.len());
                order.splice(index..index, std::iter::repeat_n(None, values.len()));
            }
            PatchAction::DeleteSeq { index, length }
                if matches!(doc.object_type(&p.obj), Ok(ObjType::List)) =>
            {
                let order = list_indices
                    .entry(p.obj.clone())
                    .or_insert_with(|| (0..doc.length_at(&p.obj, after)).map(Some).collect());
                let end = index.saturating_add(*length).min(order.len());
                let anchors = if *index < end {
                    order.drain(*index..end).collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                for anchor in anchors {
                    let blocked =
                        anchor.and_then(|at| blocked_container(doc, after, &p.obj, at, later));
                    let apply = if let Some((slot, actor)) = blocked {
                        if kept_seen.insert(slot.clone()) {
                            kept.push(KeptSlot {
                                by: authors.resolve(&actor),
                                slot,
                                actor_hex: actor,
                            });
                        }
                        false
                    } else {
                        structural = structural.saturating_add(1);
                        true
                    };
                    // Every unit removal occupies this same evolving index.
                    // Skipped removals still advance the replay's logical shadow.
                    writable.push((
                        Patch {
                            obj: p.obj.clone(),
                            path: p.path.clone(),
                            action: PatchAction::DeleteSeq {
                                index: *index,
                                length: 1,
                            },
                        },
                        apply,
                    ));
                }
                continue;
            }
            PatchAction::DeleteMap { key } => {
                if let Some((slot, actor)) =
                    blocked_container(doc, after, &p.obj, key.as_str(), later)
                {
                    if kept_seen.insert(slot.clone()) {
                        kept.push(KeptSlot {
                            by: authors.resolve(&actor),
                            slot,
                            actor_hex: actor,
                        });
                    }
                    writable.push((p, false));
                    continue;
                }
            }
            _ => {}
        }

        if let Some(s) = slot_of(doc, &p) {
            if let Some(actor) = later.get(&s) {
                if kept_seen.insert(s.clone()) {
                    kept.push(KeptSlot {
                        by: authors.resolve(actor),
                        slot: s,
                        actor_hex: actor.clone(),
                    });
                }
                writable.push((p, false));
            } else {
                reverted_slots.insert(s);
                writable.push((p, true));
            }
        } else {
            structural = structural.saturating_add(1);
            writable.push((p, true));
        }
    }

    let outcome = RevertOutcome {
        covered,
        reverted: reverted_slots.len().saturating_add(structural),
        kept,
        wrote: false,
    };
    (outcome, writable)
}

/// Work out what undoing `run` would do, without touching the document.
///
/// The dry run behind an affordance that must be explicit *before* the user
/// commits to it — a menu item saying "undo, except the paragraph you edited"
/// needs this, and so does a caller deciding whether to refuse.
///
/// # Errors
///
/// Propagates [`RevertError`] from [`run_bounds`] when `run` cannot be
/// addressed in `doc`.
pub fn plan_revert(
    doc: &mut AutoCommit,
    run: &ChangeRun,
    by: Option<&str>,
    authors: &AuthorDirectory,
) -> Result<RevertOutcome, RevertError> {
    let (before, after, covered) = run_bounds(doc, run)?;
    let forward = doc.diff(&before, &after);
    let created = objects_created(&forward);
    let inverse = doc.diff(&after, &before);
    let later = later_writers(doc, &after, by, None, &created);
    Ok(classify(doc, inverse, &later, &after, covered, authors).0)
}

/// Undo a run of changes by writing its inverse forward into `doc`.
///
/// `by` is the actor hex whose run this is; later changes under that same
/// actor are not treated as somebody building on it.
///
/// The transaction is left **open**: the caller commits, so the undo lands
/// stamped and attributed under whoever asked for it, exactly like a typed
/// edit. A caller that forgets to commit has written nothing.
///
/// # Errors
///
/// Propagates [`RevertError`] from [`run_bounds`] when `run` cannot be
/// addressed in `doc`.
pub fn revert_run(
    doc: &mut AutoCommit,
    run: &ChangeRun,
    by: Option<&str>,
    authors: &AuthorDirectory,
) -> Result<RevertOutcome, RevertError> {
    let (before, after, covered) = run_bounds(doc, run)?;
    let forward = doc.diff(&before, &after);
    let created = objects_created(&forward);
    let inverse = doc.diff(&after, &before);
    let later = later_writers(doc, &after, by, None, &created);
    let (mut outcome, patches) = classify(doc, inverse, &later, &after, covered, authors);

    if !patches.iter().any(|(_, apply)| *apply) {
        return Ok(outcome);
    }
    let pending_before = doc.pending_ops();
    // `after` is the state the reverted elements still exist in; `before` is the
    // state a re-inserted element's boundary is expressed in.
    write_selected_patches(doc, patches, &before, &after);
    outcome.wrote = doc.pending_ops() > pending_before;
    Ok(outcome)
}

/// Undo several changes, each on its own, into one open transaction:
/// `changes` pairs each change with the actor hex whose work it is, newest
/// first. Every revert is planned before any is written, because reading
/// the history closes the open transaction, which would commit the first
/// revert as a change of its own.
///
/// As with [`revert_run`] the caller commits, and must consult each
/// outcome's [`RevertOutcome::kept`] before saying "undone".
///
/// # Errors
///
/// [`RevertError::UnknownRun`] when a change is not in `doc`.
pub fn revert_changes(
    doc: &mut AutoCommit,
    changes: &[(ChangeHash, String)],
    authors: &AuthorDirectory,
) -> Result<Vec<RevertOutcome>, RevertError> {
    let mut plans = Vec::with_capacity(changes.len());
    for (hash, actor) in changes {
        let run = ChangeRun::single(entry_id_from_hash(&hash.to_string()));
        let (before, after, covered) = run_bounds(doc, &run)?;
        let forward = doc.diff(&before, &after);
        let created = objects_created(&forward);
        let inverse = doc.diff(&after, &before);
        let invoker = tilo_actions::entry_of(doc.committed_document(), *hash)
            .and_then(|context| context.invoker)
            .map(|invoker| invoker.actor);
        let later = later_writers(doc, &after, Some(actor), invoker.as_deref(), &created);
        let (outcome, patches) = classify(doc, inverse, &later, &after, covered, authors);
        plans.push((outcome, patches, before, after));
    }
    Ok(plans
        .into_iter()
        .map(|(mut outcome, patches, before, after)| {
            if patches.iter().any(|(_, apply)| *apply) {
                let pending_before = doc.pending_ops();
                write_selected_patches(doc, patches, &before, &after);
                outcome.wrote = doc.pending_ops() > pending_before;
            }
            outcome
        })
        .collect())
}

/// The contiguous run of changes authored by `actor_hex`, ending at the
/// document's newest change by that actor.
///
/// How "undo what the agent just did" finds its target when nobody recorded
/// a run: the agent commits under one stable actor, so its work is a
/// maximal run of consecutive changes under that actor, stopping at the
/// first change by anybody else.
pub fn latest_run_by(doc: &mut AutoCommit, actor_hex: &str) -> Option<ChangeRun> {
    let versions = tilo_resource::history::versions(doc);
    // Schema genesis is written by a fixed actor nobody is, so inverting it
    // would take the document's containers with it.
    let mine = |v: &tilo_resource::history::Version| {
        !tilo_doc::model::is_schema_genesis(v) && v.actor.eq_ignore_ascii_case(actor_hex)
    };
    let last = versions.iter().rposition(mine)?;
    let mut first = last;
    while first > 0 {
        // `first > 0` here, so this matches plain subtraction; `first` only
        // ever holds an in-bounds index, so `get` is always `Some` — the
        // `else` branch exists only to avoid an unchecked index.
        let prev = first.saturating_sub(1);
        if !versions.get(prev).is_some_and(mine) {
            break;
        }
        first = prev;
    }
    let start_hash = &versions.get(first)?.hash.to_string();
    let end_hash = &versions.get(last)?.hash.to_string();
    Some(ChangeRun::new(
        entry_id_from_hash(start_hash),
        entry_id_from_hash(end_hash),
    ))
}
#[cfg(test)]
mod replay_regressions {
    use super::*;
    use automerge::{ActorId, ROOT};

    fn insert(doc: &mut AutoCommit, list: &ObjId, index: usize, label: &str) -> ObjId {
        let block = doc.insert_object(list, index, ObjType::Map).unwrap();
        doc.put(&block, "label", label).unwrap();
        block
    }

    #[test]
    fn filtered_reverts_preserve_later_peer_content_and_formatting() {
        // phase: filtered_removal_keeps_middle_peer_child_and_targets_later_original_element
        {
            let mut doc = AutoCommit::new().with_actor(ActorId::from(b"alice".as_slice()));
            let list = doc.put_object(ROOT, "blocks", ObjType::List).unwrap();
            insert(&mut doc, &list, 0, "keep-tail");
            doc.commit();
            insert(&mut doc, &list, 0, "remove-a");
            let protected = insert(&mut doc, &list, 1, "protect-b");
            insert(&mut doc, &list, 2, "remove-c");
            insert(&mut doc, &list, 4, "remove-e");
            let change = doc.commit().unwrap();
            doc.set_actor(ActorId::from(b"bob".as_slice()));
            doc.put(&protected, "label", "peer edited b").unwrap();
            doc.commit();
            let run = ChangeRun::single(entry_id_from_hash(&change.to_string()));
            let result = revert_run(&mut doc, &run, None, &AuthorDirectory::default()).unwrap();
            assert!(result.wrote);
            assert_eq!(result.kept.len(), 1);
            let labels = (0..doc.length(&list))
                .map(|index| {
                    let block = doc.get(&list, index).unwrap().unwrap().1;
                    doc.get(block, "label")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .collect::<Vec<_>>();
            assert_eq!(labels, ["peer edited b", "keep-tail"]);
        }
        // phase: filtered_mark_revert_preserves_later_peer_formatting
        {
            use automerge::marks::{ExpandMark, Mark};
            let mut doc = AutoCommit::new().with_actor(ActorId::from(b"alice".as_slice()));
            let text = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
            doc.splice_text(&text, 0, 0, "hello").unwrap();
            doc.commit();
            doc.mark(
                &text,
                Mark::new("bold".into(), true, 0, 5),
                ExpandMark::After,
            )
            .unwrap();
            let change = doc.commit().unwrap();
            doc.set_actor(ActorId::from(b"bob".as_slice()));
            doc.mark(
                &text,
                Mark::new("italic".into(), true, 0, 5),
                ExpandMark::After,
            )
            .unwrap();
            doc.commit();
            let before = doc.marks(&text).unwrap();
            let result = revert_run(
                &mut doc,
                &ChangeRun::single(entry_id_from_hash(&change.to_string())),
                None,
                &AuthorDirectory::default(),
            )
            .unwrap();
            assert!(!result.wrote);
            assert_eq!(result.kept.len(), 1);
            assert_eq!(doc.marks(&text).unwrap(), before);
            assert_eq!(doc.text(&text).unwrap(), "hello");
        }
    }
}
