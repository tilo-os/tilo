//! Turning a live CRDT document into the rows the history widgets render.
//!
//! [`ActivityTimeline`](crate::widget::ActivityTimeline) consumes
//! `Vec<HistoryEntry>` and is source-agnostic; this module is the producer,
//! walking the change graph via [`tilo_resource::history::versions`] to emit one
//! [`HistoryEntry`] per change with author, timestamp, op class, and a
//! summary of what changed. It lives in `ui` (not a leaf crate) because
//! both an app's history and the compositor's room-level timeline need it.
//!
//! A row says what a change did as the change's action record names it
//! ([`sentence::step_summary`]); a change made outside any action (a framework
//! change, or a write that names no finer action than the document) says so
//! in the framework's words, never by guessing from its content. Its op class
//! and text delta are read from the change itself: **structural**, which
//! materialises the window with [`tilo_doc::snapshot::series`] and diffs
//! snapshots for real per-paragraph insert/delete counts on documents carrying
//! the `tilo-doc` schema; and **patch**, which reads the change's own patch
//! set via `AutoCommit::diff`.
//!
//! [`HistoryEntry::id`] is the first sixteen bytes of the change hash, so
//! it's content-addressed and stable across peers, letting
//! [`heads_for_entry`] turn a clicked row back into a document state.

use crate::time::{Duration, SystemTime, UNIX_EPOCH};
use tilo_sort::CompactSort as _;

use crate::widget::EntryKind;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjType, PatchAction, ReadDoc};
use collab_types::ArtifactKind;
use tilo_doc::snapshot::DocSnapshot;

use crate::widget::history::{HistoryEntry, HistoryEntryId};

pub mod authors;
pub mod compare;
pub mod descriptions;
pub mod detail;
pub mod digest;
pub mod facets;
pub mod groups;
mod log;
pub mod narrative;
/// Taking back a run of changes, below the toolkit.
pub use collaboration::history_source::revert;
pub mod run_undo;
pub mod runs;
pub mod schema_ops;
pub mod seen;
pub mod sentence;
pub(crate) use authors::room_agents_revision;
pub use authors::{
    AuthorDirectory, HistoryAgent, RosterAgent, UNKNOWN_AUTHOR_NAME, actor_handle,
    agent_display_label, agent_principal_name, author_coloured, author_for_identity,
    local_identity_author, participant_display_label, set_room_agents, short_id, stable_hue,
};
/// Undo, redo, restore and checkpoints written into the document, below
/// the toolkit.
pub use collaboration::history_source::steps;
/// One person's typing in one field, as History lists it, below the toolkit.
pub use collaboration::history_source::typing;
pub use collaboration::history_source::verbs;
/// What a history verb speaks in, below the toolkit.
pub use collaboration::history_source::words::{self, HistoryWords};
pub use collaboration::history_source::{entry_id_from_hash, is_history_metadata};
pub use compare::{CompareError, HistoryComparison, compare_entries};
pub use detail::{HistoryResourceTarget, HistorySelectionDetail, contains_opaque_id, human_label};
pub use digest::{ActivityBrief, ActivityBriefAction, ActivityDigest};
pub use facets::HistoryFacetIndex;
pub use groups::{
    GROUPS_KEY, clear_name, close_range, is_group_marker, merge_named_ranges, move_named_boundary,
    name_range, read_named_ranges, split_named_range,
};
pub use log::HistoryLog;
pub use narrative::{
    ActivityNarrator, GroundedNarrative, NarrativeRequest, NarrativeSource,
    deterministic_narrative, validate_narrative,
};
pub use revert::{
    ChangeRun, KeptSlot, RevertError, RevertOutcome, Slot, latest_run_by, plan_revert, revert_run,
    revert_step,
};
pub use run_undo::{
    RunUndoReport, StepOutcome, StepState, refuse_run, undo_run, undo_step, unreachable_steps,
};
pub use runs::{
    AgentRun, RUNS_KEY, RunStep, forget_run, is_run_marker, read_runs, record_run,
    record_run_with_details, run_starting_at,
};
pub use schema_ops::{
    SchemaChange, SchemaCreation, SchemaHistoryOp, SchemaObjectId, schema_changes_since,
    schema_operations_since,
};
pub use seen::HistorySeenFrontier;
pub use sentence::ThreadWords;

/// The structural path materialises one snapshot per version, so cost is
/// linear in versions and document size; 200 is well past what a scrubber
/// track can resolve into distinct ticks at common widths. Raise it with
/// [`HistorySource::with_max_entries`] when a caller needs
/// the whole chain.
pub const DEFAULT_MAX_ENTRIES: usize = 200;

/// Automerge's history readers close an open transaction. Inspect a committed
/// copy only while an edit is pending, so browsing never steals the app's action
/// boundary or publishes a temporary, unstamped change as a stable history ID.
/// Idle documents are read without cloning.
fn with_committed_history<R>(doc: &mut AutoCommit, read: impl FnOnce(&mut AutoCommit) -> R) -> R {
    if doc.pending_ops() == 0 {
        return read(doc);
    }
    let mut committed = doc.clone();
    committed.rollback();
    read(&mut committed)
}

/// The committed frontier for history cache invalidation. Unlike
/// `AutoCommit::get_heads`, observing it never commits a pending app action.
pub fn committed_heads(doc: &AutoCommit) -> Vec<ChangeHash> {
    doc.base_heads()
}

/// Materialize a committed tilo-doc history entry without committing live edits.
/// An ID from another document returns `Ok(None)`.
///
/// # Errors
/// Returns the tilo-doc error if the selected version cannot be materialized.
pub fn materialize_entry(
    doc: &mut AutoCommit,
    id: &HistoryEntryId,
) -> Result<Option<DocSnapshot>, tilo_doc::DocError> {
    with_committed_history(doc, |doc| {
        version_index_for_entry(doc, id)
            .map(|version| tilo_doc::snapshot::at(doc, version))
            .transpose()
    })
}

/// Construct one, hand it an [`AuthorDirectory`] so actor ids resolve to
/// people, and call [`HistorySource::entries`] whenever the document changes.
pub struct HistorySource {
    authors: AuthorDirectory,
    artifact_kind: ArtifactKind,
    max_entries: usize,
    record_genesis: bool,
    pass_over: Option<PassOver>,
}

/// Which changes a [`HistorySource`] passes over; see
/// [`HistorySource::passing_over`].
pub type PassOver = fn(&automerge::Change) -> bool;

impl Default for HistorySource {
    fn default() -> Self {
        Self::new()
    }
}

impl HistorySource {
    /// Every actor resolves to an unknown participant until registered.
    pub fn new() -> Self {
        Self {
            authors: AuthorDirectory::new(),
            artifact_kind: ArtifactKind::File,
            max_entries: DEFAULT_MAX_ENTRIES,
            record_genesis: true,
            pass_over: None,
        }
    }

    /// Pass over the changes `pass_over` names: they are part of the
    /// document, and of the heads every later row is read at, but they make
    /// no row and take no place in the window. For a document that also
    /// keeps view state, such as which workspace a desktop shows, so that
    /// moving the view neither shows up as history nor pushes what people
    /// did out of the window. Decided from the change alone, so a reader
    /// that reads the log from scratch decides it for every change.
    #[must_use]
    pub fn passing_over(mut self, pass_over: PassOver) -> Self {
        self.pass_over = Some(pass_over);
        self
    }

    fn passes_over(&self, change: &automerge::Change) -> bool {
        self.pass_over.is_some_and(|pass_over| pass_over(change))
    }

    /// Leave out a record's genesis: the change that created it with its
    /// initial content, made by an actor derived from the record's identity
    /// at time zero (for a package-primary document, which records no
    /// identity, the root change that stamps its store's schema). A
    /// document's own history keeps it as the version to restore to; a feed
    /// of what people did has no one to name for it.
    #[must_use]
    pub fn without_record_genesis(mut self) -> Self {
        self.record_genesis = false;
        self
    }

    #[must_use]
    pub fn with_authors(mut self, authors: AuthorDirectory) -> Self {
        self.authors = authors;
        self
    }

    #[must_use]
    pub fn with_artifact_kind(mut self, kind: ArtifactKind) -> Self {
        self.artifact_kind = kind;
        self
    }

    #[must_use]
    pub fn with_max_entries(mut self, max: usize) -> Self {
        self.max_entries = max.max(1);
        self
    }

    /// So a long-lived producer can learn about a peer that joined after
    /// it was constructed.
    pub fn authors_mut(&mut self) -> &mut AuthorDirectory {
        &mut self.authors
    }

    pub fn authors(&self) -> &AuthorDirectory {
        &self.authors
    }

    /// Widgets sort for display differently (timeline newest-first,
    /// scrubber left-to-right), so this returns the document's own causal
    /// order and lets them decide. Only committed changes are listed; pending
    /// edits stay in the live document until the app commits its action.
    pub fn entries(&self, doc: &mut AutoCommit) -> Vec<HistoryEntry> {
        with_committed_history(doc, |doc| self.committed_entries(doc))
    }
}

/// What a genesis row says: the document was created, by no one in
/// particular.
fn genesis_context() -> tilo_actions::ActionContext {
    tilo_actions::ActionContext {
        facets: vec![crate::widget::history::genesis_facet()],
        ..tilo_actions::ActionContext::default()
    }
}

/// Whether `v` is the genesis of a document its store created: the root
/// change, made at time zero, which stamps the store's schema along with the
/// initial content. Nobody's edit writes the schema stamp.
fn is_store_genesis(doc: &mut AutoCommit, time_ms: i64, hash: &ChangeHash) -> bool {
    time_ms == 0
        && doc
            .get_change_meta_by_hash(hash)
            .is_some_and(|change| change.deps.is_empty())
        && doc.diff(&[], &[*hash]).iter().any(|patch| {
            patch.obj == automerge::ROOT
                && matches!(
                    &patch.action,
                    PatchAction::PutMap { key, .. } if key == app_state::SCHEMA_VERSION_KEY
                )
        })
}

/// The other half of [`HistorySource::entries`]: a restore needs a document
/// state for the [`HistoryEntryId`] a widget hands back. `None` when the id
/// names no change in this document — mixed ids from two artifacts.
pub fn heads_for_entry(doc: &mut AutoCommit, id: &HistoryEntryId) -> Option<Vec<ChangeHash>> {
    with_committed_history(doc, |doc| {
        let index = version_index_for_entry(doc, id)?;
        Some(tilo_resource::history::prefix_heads(doc, index))
    })
}

pub fn version_index_for_entry(doc: &mut AutoCommit, id: &HistoryEntryId) -> Option<usize> {
    with_committed_history(doc, |doc| {
        tilo_resource::history::versions(doc)
            .iter()
            .position(|v| entry_id_from_hash(&v.hash.to_string()) == *id)
    })
}

/// `time_ms` is `0` for a change committed without a timestamp. The epoch is
/// the rendering of "we don't know when this happened": it sorts
/// oldest rather than being quietly filed under today.
fn timestamp_from_ms(time_ms: i64) -> SystemTime {
    if time_ms <= 0 {
        return UNIX_EPOCH;
    }
    // `time_ms` is confirmed positive above, so the cast never loses sign;
    // `checked_add` guards the (unreachable for any real change timestamp)
    // case where the duration would overflow `SystemTime`'s representation.
    let millis = u64::try_from(time_ms).unwrap_or(0);
    UNIX_EPOCH
        .checked_add(Duration::from_millis(millis))
        .unwrap_or(UNIX_EPOCH)
}

/// What a change did to the document, read from the change itself: its op
/// class, its text delta, and whether anything it did shows.
#[derive(Clone, Copy)]
struct Effect {
    entry_kind: EntryKind,
    text_delta: Option<(usize, usize)>,
    visible: bool,
}

impl Effect {
    const fn shows(entry_kind: EntryKind, text_delta: Option<(usize, usize)>) -> Self {
        Self {
            entry_kind,
            text_delta,
            visible: true,
        }
    }

    const fn nothing(text_delta: Option<(usize, usize)>) -> Self {
        Self {
            entry_kind: EntryKind::Details,
            text_delta,
            visible: false,
        }
    }
}

/// The op class follows what actually moved, in the order a reader cares
/// about: a new comment is a comment even if it also touched text; content
/// edits beat pure deletions; a reordering with no content change is a
/// move; anything left is metadata.
fn structural_effect(prev: &DocSnapshot, cur: &DocSnapshot) -> Effect {
    let diff = tilo_doc::diff_snapshots(prev, cur);

    let mut inserted = 0usize;
    let mut deleted = 0usize;
    let mut formatting = 0usize;
    for b in &diff.blocks {
        match b.op {
            tilo_doc::BlockOp::Added => {
                inserted =
                    inserted.saturating_add(b.new_text.as_deref().unwrap_or("").chars().count());
            }
            tilo_doc::BlockOp::Removed => {
                deleted =
                    deleted.saturating_add(b.old_text.as_deref().unwrap_or("").chars().count());
            }
            tilo_doc::BlockOp::Changed | tilo_doc::BlockOp::Equal => {
                for r in &b.runs {
                    match r.op {
                        tilo_doc::RunOp::Insert => {
                            inserted = inserted.saturating_add(r.text.chars().count());
                        }
                        tilo_doc::RunOp::Delete => {
                            deleted = deleted.saturating_add(r.text.chars().count());
                        }
                        tilo_doc::RunOp::Equal => {}
                    }
                }
            }
        }
        formatting = formatting
            .saturating_add(b.attr_changes.len())
            .saturating_add(b.marks_added.len())
            .saturating_add(b.marks_removed.len());
        if b.kind_change.is_some() {
            formatting = formatting.saturating_add(1);
        }
    }

    let blocks_added = diff.summary.added;
    let blocks_removed = diff.summary.removed;
    let comments = cur.comments.len() > prev.comments.len();
    // A removed block also removes its text: it is a deletion, not an edit.
    let pure_removal = blocks_removed > 0 && blocks_added == 0 && inserted == 0;

    // A removed block contributes the text it took with it, so every
    // class below carries counted characters without exception.
    let delta = Some((inserted, deleted));
    if comments {
        return Effect::shows(EntryKind::Comment, delta);
    }
    if pure_removal {
        return Effect::shows(EntryKind::Delete, delta);
    }
    if inserted > 0 || deleted > 0 || blocks_added > 0 {
        return Effect::shows(EntryKind::Text, delta);
    }
    if blocks_removed > 0 {
        return Effect::shows(EntryKind::Delete, delta);
    }
    if block_order_changed(prev, cur) {
        return Effect::shows(EntryKind::Move, delta);
    }
    if formatting > 0 || !diff.properties.is_empty() {
        return Effect::shows(EntryKind::Details, delta);
    }
    Effect::nothing(delta)
}

/// Same set of top-level block ids but a different order — a move rather
/// than an edit.
fn block_order_changed(prev: &DocSnapshot, cur: &DocSnapshot) -> bool {
    let a: Vec<&str> = prev.blocks.iter().map(|b| b.id.as_str()).collect();
    let b: Vec<&str> = cur.blocks.iter().map(|b| b.id.as_str()).collect();
    if a.len() != b.len() {
        return false;
    }
    let mut sa = a.clone();
    let mut sb = b.clone();
    sa.compact_sort();
    sb.compact_sort();
    sa == sb && a != b
}

/// [`patch_effect`] of the patches `before -> after` reads. For documents
/// that don't carry the tilo-doc schema.
fn diff_effect(doc: &mut AutoCommit, before: &[ChangeHash], after: &[ChangeHash]) -> Effect {
    let patches = doc.diff(before, after);
    patch_effect(doc, &patches)
}

/// What patches already read did; `doc` is a replica holding the objects
/// they touch.
fn patch_effect(doc: &AutoCommit, patches: &[automerge::Patch]) -> Effect {
    // A change that only named a version or recorded a run shows nothing,
    // so the caller drops the row.
    if is_history_metadata(patches) {
        return Effect::nothing(None);
    }
    let mut inserted = 0usize;
    let mut text_deleted = 0usize;
    let mut list_inserted = false;
    let mut list_deleted = false;
    let mut fields = false;
    let mut field_deleted = false;
    let mut marks = false;

    for p in patches {
        let is_text = matches!(doc.object_type(&p.obj), Ok(ObjType::Text));
        match &p.action {
            PatchAction::SpliceText { value, .. } => {
                inserted = inserted.saturating_add(value.make_string().chars().count());
            }
            PatchAction::DeleteSeq { length, .. } => {
                if is_text {
                    text_deleted = text_deleted.saturating_add(*length);
                } else {
                    list_deleted = true;
                }
            }
            PatchAction::Insert { .. } | PatchAction::PutSeq { .. } => list_inserted = true,
            // Both only ever mark that a scalar field changed — an increment
            // is bookkeeping the same way a put is.
            PatchAction::PutMap { .. } | PatchAction::Increment { .. } => fields = true,
            PatchAction::DeleteMap { .. } => field_deleted = true,
            PatchAction::Mark { marks: m } => marks |= !m.is_empty(),
            PatchAction::Conflict { .. } => {}
        }
    }

    let delta = Some((inserted, text_deleted));
    if inserted > 0 || text_deleted > 0 || list_inserted {
        return Effect::shows(EntryKind::Text, delta);
    }
    if list_deleted || (field_deleted && !fields) {
        return Effect::shows(EntryKind::Delete, delta);
    }
    if fields || field_deleted || marks {
        return Effect::shows(EntryKind::Details, delta);
    }
    Effect::nothing(delta)
}

#[cfg(test)]
mod tests;
