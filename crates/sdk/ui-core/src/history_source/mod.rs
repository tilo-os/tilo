//! Turning a live CRDT document into the rows the history widgets render.
//!
//! [`ActivityTimeline`](crate::widget::ActivityTimeline) and
//! [`HistoryScrubber`](crate::widget::HistoryScrubber) both consume
//! `Vec<HistoryEntry>` and are source-agnostic; this module is the producer,
//! walking the change graph via [`tilo_resource::history::versions`] to emit one
//! [`HistoryEntry`] per change with author, timestamp, op class, and a
//! summary of what changed. It lives in `ui` (not a leaf crate) because
//! both an app's scrubber and the compositor's room-level timeline need it.
//!
//! Two summary strategies, in order of preference: **structural**, which
//! materialises the window with [`tilo_doc::snapshot::series`] and
//! diffs snapshots for real per-paragraph insert/delete counts on
//! documents carrying the `qdoc` schema; and **patch**, the fallback that
//! reads the change's own patch set via `AutoCommit::diff`, weaker only in
//! that it can't see qdoc-level concepts like comments or block moves.
//! Neither path guesses — an indescribable change gets
//! `"no visible change"` rather than an invented summary.
//!
//! [`HistoryEntry::id`] is the first sixteen bytes of the change hash, so
//! it's content-addressed and stable across peers, letting
//! [`heads_for_entry`] turn a clicked row back into a document state.

use crate::time::{Duration, SystemTime, UNIX_EPOCH};
use std::collections::HashMap;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjType, PatchAction, ReadDoc};
use collab_types::{ArtifactKind, OpClass};
use tilo_doc::snapshot::DocSnapshot;

use crate::widget::history::{HistoryEntry, HistoryEntryId};

pub mod authors;
pub mod compare;
pub mod detail;
pub mod digest;
pub mod facets;
pub mod groups;
pub mod narrative;
pub mod revert;
pub mod run_undo;
pub mod runs;
pub mod schema_ops;
pub mod seen;
pub use authors::{
    AuthorDirectory, UNKNOWN_AUTHOR_NAME, actor_handle, agent_display_label, agent_principal_name,
    author_coloured, author_for_identity, local_identity_author, participant_display_label,
    short_id, stable_hue,
};
pub use compare::{CompareError, HistoryComparison, compare_entries};
pub use detail::{HistoryResourceTarget, HistorySelectionDetail};
pub use digest::{ActivityBrief, ActivityBriefAction, ActivityDigest};
pub use facets::{HistoryFacetIndex, op_class_label};
pub use groups::{
    GROUPS_KEY, clear_name, close_range, is_group_marker, merge_named_ranges, move_named_boundary,
    name_range, read_named_ranges, split_named_range,
};
pub use narrative::{
    ActivityNarrator, GroundedNarrative, NarrativeRequest, NarrativeSource,
    deterministic_narrative, validate_narrative,
};
pub use revert::{
    ChangeRun, KeptSlot, RevertError, RevertOutcome, Slot, latest_run_by, plan_revert, revert_run,
};
pub use run_undo::{
    RunUndoReport, StepOutcome, StepState, refuse_run, undo_run, undo_step, unreachable_steps,
};
pub use runs::{
    AgentRun, RUNS_KEY, RunStep, forget_run, is_run_marker, read_runs, record_run,
    record_run_with_details, run_starting_at,
};
pub use schema_ops::{SchemaHistoryOp, SchemaObjectId, schema_operations_since};
pub use seen::HistorySeenFrontier;

/// The structural path materialises one snapshot per version, so cost is
/// linear in versions and document size; 200 is well past what a scrubber
/// track can resolve into distinct ticks at common widths. Raise it with
/// [`HistorySource::with_max_entries`] when a caller needs
/// the whole chain.
pub const DEFAULT_MAX_ENTRIES: usize = 200;

/// Deliberately not a guess.
pub const NO_VISIBLE_CHANGE: &str = "no visible change";

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

/// Materialize a committed qdoc history entry without committing live edits.
/// An ID from another document returns `Ok(None)`.
///
/// # Errors
/// Returns the qdoc error if the selected version cannot be materialized.
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
    field_paths: bool,
}

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
            field_paths: true,
        }
    }

    /// Describe a change that edited a document's fields by how many it
    /// edited, never by their paths. A document's own history names the
    /// fields for whoever inspects it; a feed read by people shows no
    /// storage keys.
    #[must_use]
    pub fn without_field_paths(mut self) -> Self {
        self.field_paths = false;
        self
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

    fn committed_entries(&self, doc: &mut AutoCommit) -> Vec<HistoryEntry> {
        let versions = tilo_resource::history::versions(doc);
        if versions.is_empty() {
            return Vec::new();
        }
        let start = versions.len().saturating_sub(self.max_entries);
        let genesis_actor = crate::record::genesis_actor(doc).map(|actor| actor.to_hex_string());

        // Plus the one before the window — the patch path diffs between
        // consecutive frontiers.
        let heads = prefix_heads_series(doc, start, versions.len());

        // One replay of the change log yields every snapshot in the window,
        // plus the one before it, so the first row still describes a delta.
        let before_window = start.checked_sub(1);
        let indices: Vec<usize> = before_window
            .into_iter()
            .chain(start..versions.len())
            .collect();
        let mut snapshots = tilo_doc::snapshot::series(doc, &indices).into_iter();
        let mut prev_snapshot: Option<DocSnapshot> = match before_window {
            None => Some(DocSnapshot::default()),
            Some(_) => snapshots.next().flatten(),
        };

        let mut out = Vec::with_capacity(versions.len().saturating_sub(start));
        for (index, v) in versions.iter().enumerate().skip(start) {
            let cur_snapshot = snapshots.next().flatten();
            let before = heads
                .get(&index.wrapping_sub(1))
                .cloned()
                .unwrap_or_default();
            let after = heads.get(&index).cloned().unwrap_or_default();
            let (structural_summary, op_class, text_delta) = match (&prev_snapshot, &cur_snapshot) {
                (Some(prev), Some(cur)) => structural_summary(prev, cur),
                _ => patch_summary(doc, &before, &after, self.field_paths),
            };
            prev_snapshot = cur_snapshot;

            // The schema genesis is a change nobody made — its containers are
            // written by a fixed actor so two independently created documents
            // merge instead of shadowing each other. Listing it would attribute
            // a revision to a nonexistent person. Skipped here rather than in
            // `versions`, which stays positional (`materialize_at` / `prefix_heads`
            // address a change by its place).
            if tilo_doc::model::is_schema_genesis(v) {
                continue;
            }
            // A record's genesis is the same kind of change; so is a
            // package-primary document's, which holds no identity to derive
            // its actor from. The document's own history keeps it, marked,
            // as the version to restore to.
            let genesis =
                genesis_actor.as_deref() == Some(v.actor.as_str()) || is_store_genesis(doc, v);
            if genesis && !self.record_genesis {
                continue;
            }

            // Naming a version (or recording a cross-app run) is a change but
            // not a row — the name is already on screen as the band's title.
            // Checked only when there's no other summary, so the common path
            // pays no extra diff.
            if structural_summary == NO_VISIBLE_CHANGE {
                let patches = doc.diff(&before, &after);
                if groups::is_group_marker(&patches) || runs::is_run_marker(&patches) {
                    continue;
                }
            }

            let context = if genesis {
                Some(genesis_context())
            } else {
                v.message
                    .as_deref()
                    .and_then(tilo_actions::ActionContext::from_change_message)
            };
            let summary = context
                .as_ref()
                .and_then(tilo_actions::ActionContext::summary)
                .unwrap_or(structural_summary);

            out.push(HistoryEntry {
                id: entry_id_from_hash(&v.hash.to_string()),
                author: self.authors.resolve(&v.actor),
                op_class,
                artifact_kind: self.artifact_kind,
                timestamp: timestamp_from_ms(v.time_ms),
                summary,
                text_delta,
                // The scrubber re-derives state via `materialize_at` /
                // `heads_for_entry` rather than cracking open a payload.
                diff_payload: None,
                // Projected from the broker's redaction log, keyed by
                // artifact and not reachable from a bare document.
                redaction: None,
                context,
            });
        }
        out
    }
}

/// What a genesis row says: the document was created, by no one in
/// particular.
fn genesis_context() -> tilo_actions::ActionContext {
    tilo_actions::ActionContext {
        action_label: crate::i18n::I18nManager::lookup_plain("history-genesis"),
        facets: vec![crate::widget::history::genesis_facet()],
        ..tilo_actions::ActionContext::default()
    }
}

/// Whether `v` is the genesis of a document its store created: the root
/// change, made at time zero, which stamps the store's schema along with the
/// initial content. Nobody's edit writes the schema stamp.
fn is_store_genesis(doc: &mut AutoCommit, v: &tilo_resource::history::Version) -> bool {
    v.time_ms == 0
        && doc
            .get_change_by_hash(&v.hash)
            .is_some_and(|change| change.deps().is_empty())
        && doc.diff(&[], &[v.hash]).iter().any(|patch| {
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

/// The hash is the document's own content address, so two peers
/// independently building a timeline over the same history agree on the
/// ids — what lets one peer's `[Revert]` mean anything to another.
pub fn entry_id_from_hash(change_hash_hex: &str) -> HistoryEntryId {
    let mut bytes = [0u8; 16];
    let src = change_hash_hex.as_bytes();
    for (i, slot) in bytes.iter_mut().enumerate() {
        // `i` walks a fixed 16-byte array, so `idx` never approaches
        // `usize::MAX`; `saturating_mul`/`saturating_add` are a no-op here
        // and only exist to satisfy the arithmetic-side-effects lint.
        let idx = i.saturating_mul(2);
        let (hi, lo) = (src.get(idx), src.get(idx.saturating_add(1)));
        let hi = hi.and_then(|c| (*c as char).to_digit(16)).unwrap_or(0);
        let lo = lo.and_then(|c| (*c as char).to_digit(16)).unwrap_or(0);
        // `to_digit(16)` only ever returns `0..=15`, so `combined` is always
        // `0..=255` and the `try_from` below can never actually fail.
        let combined = hi.saturating_mul(16).saturating_add(lo);
        *slot = u8::try_from(combined).unwrap_or(u8::MAX);
    }
    HistoryEntryId(bytes)
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

/// Versions `start - 1 ..= end - 1`, computed in one pass — `prefix_heads`
/// re-reads the change list per call, so a timeline asking for every
/// version builds the series from one read instead.
fn prefix_heads_series(
    doc: &mut AutoCommit,
    start: usize,
    end: usize,
) -> HashMap<usize, Vec<ChangeHash>> {
    let changes = doc.get_changes(&[]);
    let mut out = HashMap::new();
    let lo = start.saturating_sub(1);
    for i in lo..end.min(changes.len()) {
        // The loop bound guarantees `i < changes.len()`, so `..=i` is
        // always in range; `get` plus `continue` keeps that provable
        // rather than assumed.
        let Some(prefix) = changes.get(..=i) else {
            continue;
        };
        let depended_on: std::collections::HashSet<ChangeHash> = prefix
            .iter()
            .flat_map(|c| c.deps().iter().copied())
            .collect();
        out.insert(
            i,
            prefix
                .iter()
                .map(automerge::Change::hash)
                .filter(|h| !depended_on.contains(h))
                .collect(),
        );
    }
    out
}

/// The op class follows what actually moved, in the order a reader cares
/// about: a new comment is a comment even if it also touched text; content
/// edits beat pure deletions; a reordering with no content change is a
/// move; anything left is metadata.
fn structural_summary(
    prev: &DocSnapshot,
    cur: &DocSnapshot,
) -> (String, OpClass, Option<(usize, usize)>) {
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
    let props = diff.properties.len();
    let comments = cur.comments.len().saturating_sub(prev.comments.len());
    let pure_reorder = blocks_added == 0 && blocks_removed == 0 && block_order_changed(prev, cur);
    // Checked before the character counts: a removed block also removes its
    // text, and "-1 block" is what the reader wants, not "-6 chars".
    let pure_removal = blocks_removed > 0 && blocks_added == 0 && inserted == 0;

    // A removed block contributes the text it took with it, so every
    // branch below can report counted characters without exception.
    let delta = Some((inserted, deleted));
    if comments > 0 {
        let s = if comments == 1 {
            "+1 comment".to_string()
        } else {
            format!("+{comments} comments")
        };
        return (s, OpClass::Comment, delta);
    }
    if pure_removal {
        return (
            plural_signed(negative_count(blocks_removed), "block"),
            OpClass::Tombstone,
            delta,
        );
    }
    if inserted > 0 || deleted > 0 {
        return (
            format!("+{inserted} -{deleted} chars"),
            OpClass::Splice,
            delta,
        );
    }
    if blocks_added > 0 {
        return (
            plural_signed(as_i64_count(blocks_added), "block"),
            OpClass::Splice,
            delta,
        );
    }
    if blocks_removed > 0 {
        return (
            plural_signed(negative_count(blocks_removed), "block"),
            OpClass::Tombstone,
            delta,
        );
    }
    if pure_reorder {
        return ("reordered".to_string(), OpClass::Move, delta);
    }
    if formatting > 0 {
        return (
            plural(formatting, "formatting change"),
            OpClass::EditMeta,
            delta,
        );
    }
    if props > 0 {
        return (plural(props, "property"), OpClass::EditMeta, delta);
    }
    (NO_VISIBLE_CHANGE.to_string(), OpClass::EditMeta, delta)
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
    sa.sort_unstable();
    sb.sort_unstable();
    sa == sb && a != b
}

/// Describe, in one short phrase, what moving `doc` from `from` to `to` does:
/// `"+12 -3 chars"`, `"+1 item"`, `"2 fields"`.
///
/// The public face of the same producer the history rows are built from, so an
/// Undo control and the timeline describe an action identically rather than
/// each inventing wording. Like every summary here it never guesses: a change
/// whose effect cannot be described comes back as [`NO_VISIBLE_CHANGE`].
///
/// `&mut AutoCommit` because reading a diff flushes the open transaction; the
/// document is not otherwise modified.
pub fn change_summary(doc: &mut AutoCommit, from: &[ChangeHash], to: &[ChangeHash]) -> String {
    // A change made through an action says what it did. Reading that costs
    // one change's metadata; diffing the span costs a scan of the document,
    // which an undo control asking every frame cannot afford.
    if let [head] = to
        && !from.contains(head)
        && let Some(summary) = doc
            .get_change_meta_by_hash(head)
            .and_then(|meta| meta.message.as_deref().map(str::to_owned))
            .as_deref()
            .and_then(tilo_actions::ActionContext::from_change_message)
            .and_then(|context| context.summary())
    {
        return summary;
    }
    patch_summary(doc, from, to, true).0
}

/// Describe a change by reading its own patch set. The fallback for documents
/// that don't carry the qdoc schema. With `paths`, edited fields are named by
/// their document paths; without, counted.
fn patch_summary(
    doc: &mut AutoCommit,
    before: &[ChangeHash],
    after: &[ChangeHash],
    paths: bool,
) -> (String, OpClass, Option<(usize, usize)>) {
    let patches = doc.diff(before, after);
    // A change that only named a version or recorded a run reports as
    // nothing visible, so the caller drops the row.
    if groups::is_group_marker(&patches) || runs::is_run_marker(&patches) {
        return (NO_VISIBLE_CHANGE.to_string(), OpClass::EditMeta, None);
    }
    let mut inserted = 0usize;
    let mut text_deleted = 0usize;
    let mut list_inserted = 0usize;
    let mut list_deleted = 0usize;
    let mut fields = 0usize;
    let mut field_deleted = 0usize;
    let mut marks = 0usize;
    let mut field_paths: Vec<String> = Vec::new();

    for p in &patches {
        if let Some(path) = field_path(p)
            && !field_paths.contains(&path)
        {
            field_paths.push(path);
        }
        let is_text = matches!(doc.object_type(&p.obj), Ok(ObjType::Text));
        match &p.action {
            PatchAction::SpliceText { value, .. } => {
                inserted = inserted.saturating_add(value.make_string().chars().count());
            }
            PatchAction::DeleteSeq { length, .. } => {
                if is_text {
                    text_deleted = text_deleted.saturating_add(*length);
                } else {
                    list_deleted = list_deleted.saturating_add(*length);
                }
            }
            PatchAction::Insert { values, .. } => {
                list_inserted = list_inserted.saturating_add(values.len());
            }
            // Both arms only ever mark that a scalar field changed — an
            // increment is bookkeeping the same way a put is.
            PatchAction::PutMap { .. } | PatchAction::Increment { .. } => {
                fields = fields.saturating_add(1);
            }
            PatchAction::DeleteMap { .. } => field_deleted = field_deleted.saturating_add(1),
            PatchAction::PutSeq { .. } => list_inserted = list_inserted.saturating_add(1),
            PatchAction::Mark { marks: m } => marks = marks.saturating_add(m.len()),
            PatchAction::Conflict { .. } => {}
        }
    }

    let delta = Some((inserted, text_deleted));
    if inserted > 0 || text_deleted > 0 {
        return (
            format!("+{inserted} -{text_deleted} chars"),
            OpClass::Splice,
            delta,
        );
    }
    if list_inserted > 0 {
        return (
            plural_signed(as_i64_count(list_inserted), "item"),
            OpClass::Splice,
            delta,
        );
    }
    if list_deleted > 0 {
        return (
            plural_signed(negative_count(list_deleted), "item"),
            OpClass::Tombstone,
            delta,
        );
    }
    if (fields > 0 || field_deleted > 0)
        && let Some((first, rest)) = field_paths.split_first()
    {
        let removed = fields == 0;
        let class = if removed {
            OpClass::Tombstone
        } else {
            OpClass::EditMeta
        };
        let label = if paths {
            edited_fields_label(first, rest.len(), removed)
        } else {
            counted_fields_label(field_paths.len(), removed)
        };
        return (label, class, delta);
    }
    if fields > 0 {
        return (plural(fields, "field"), OpClass::EditMeta, delta);
    }
    if marks > 0 {
        return (plural(marks, "formatting change"), OpClass::EditMeta, delta);
    }
    (NO_VISIBLE_CHANGE.to_string(), OpClass::EditMeta, delta)
}

/// The dotted document path of the map entry `patch` writes, such as
/// `settings.theme`. Sequence positions are omitted: an index names where a
/// value sat, not what it is.
fn field_path(patch: &automerge::Patch) -> Option<String> {
    let leaf = match &patch.action {
        PatchAction::PutMap { key, .. }
        | PatchAction::DeleteMap { key }
        | PatchAction::Increment {
            prop: automerge::Prop::Map(key),
            ..
        } => key.clone(),
        _ => return None,
    };
    let mut parts: Vec<&str> = patch
        .path
        .iter()
        .filter_map(|(_, prop)| match prop {
            automerge::Prop::Map(key) => Some(key.as_str()),
            automerge::Prop::Seq(_) => None,
        })
        .collect();
    parts.push(&leaf);
    Some(parts.join("."))
}

/// "Edited title" / "Edited title and 2 more", or "Removed …" when the change
/// only deleted entries.
fn edited_fields_label(path: &str, more: usize, removed: bool) -> String {
    crate::i18n::I18nManager::format(
        if removed {
            "history-removed-field"
        } else {
            "history-edited-field"
        },
        &[("path", path.into()), ("more", more.into())],
    )
}

/// "Edited 3 fields", or "Removed …" when the change only deleted entries.
fn counted_fields_label(count: usize, removed: bool) -> String {
    crate::i18n::I18nManager::format(
        if removed {
            "history-removed-fields"
        } else {
            "history-edited-fields"
        },
        &[("count", count.into())],
    )
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Converts a count to a non-negative `i64` for [`plural_signed`],
/// saturating at `i64::MAX` instead of risking a wrap — no realistic
/// block/item/field count on a document gets anywhere near that bound.
fn as_i64_count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Converts a count to a negative `i64` for [`plural_signed`]. Saturates at
/// `i64::MIN` rather than risking overflow on the negation; unreachable in
/// practice for the same reason as [`as_i64_count`].
fn negative_count(n: usize) -> i64 {
    as_i64_count(n).checked_neg().unwrap_or(i64::MIN)
}

fn plural_signed(n: i64, noun: &str) -> String {
    let mag = usize::try_from(n.unsigned_abs()).unwrap_or(usize::MAX);
    let sign = if n < 0 { '-' } else { '+' };
    if mag == 1 {
        format!("{sign}1 {noun}")
    } else {
        format!("{sign}{mag} {noun}s")
    }
}

#[cfg(test)]
mod tests;
