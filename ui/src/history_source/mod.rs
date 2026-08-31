//! Turning a live CRDT document into the rows the history widgets render.
//!
//! [`ActivityTimeline`](crate::widget::ActivityTimeline) and
//! [`HistoryScrubber`](crate::widget::HistoryScrubber) both consume
//! `Vec<HistoryEntry>` and are source-agnostic; this module is the producer,
//! walking the change graph via [`qdoc::history::versions`] to emit one
//! [`HistoryEntry`] per change with author, timestamp, op class, and a
//! summary of what changed. It lives in `ui` (not a leaf crate) because
//! both an app's scrubber and the compositor's room-level timeline need it.
//!
//! Two summary strategies, in order of preference: **structural**, which
//! materialises each version with [`qdoc::history::materialize_at`] and
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

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use automerge::{AutoCommit, ChangeHash, ObjType, PatchAction, ReadDoc};
use collab_types::{ArtifactKind, OpClass};
use qdoc::snapshot::DocSnapshot;

use crate::widget::history::{HistoryEntry, HistoryEntryId};

pub mod authors;
pub mod groups;
pub mod revert;
pub mod run_undo;
pub mod runs;
pub use authors::{
    AuthorDirectory, UNKNOWN_AUTHOR_NAME, actor_handle, local_identity_author, short_id, stable_hue,
};
pub use groups::{
    GROUPS_KEY, clear_name, close_range, is_group_marker, name_range, read_named_ranges,
};
pub use revert::{
    ChangeRun, KeptSlot, RevertError, RevertOutcome, Slot, latest_run_by, plan_revert, revert_run,
};
pub use run_undo::{
    RunUndoReport, StepOutcome, StepState, refuse_run, undo_run, undo_step, unreachable_steps,
};
pub use runs::{
    AgentRun, RUNS_KEY, RunStep, forget_run, is_run_marker, read_runs, record_run, run_starting_at,
};

/// The structural path materialises one snapshot per version, so cost is
/// linear in versions and document size; 200 is well past what a scrubber
/// track can resolve into distinct ticks at any realistic width. Raise it
/// with [`HistorySource::with_max_entries`] when a caller genuinely wants
/// the whole chain.
pub const DEFAULT_MAX_ENTRIES: usize = 200;

/// Deliberately not a guess.
pub const NO_VISIBLE_CHANGE: &str = "no visible change";

/// Construct one, hand it an [`AuthorDirectory`] so actor ids resolve to
/// people, and call [`HistorySource::entries`] whenever the document changes.
pub struct HistorySource {
    authors: AuthorDirectory,
    artifact_kind: ArtifactKind,
    max_entries: usize,
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
        }
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
    /// order and lets them decide.
    pub fn entries(&self, doc: &mut AutoCommit) -> Vec<HistoryEntry> {
        let versions = qdoc::history::versions(doc);
        if versions.is_empty() {
            return Vec::new();
        }
        let start = versions.len().saturating_sub(self.max_entries);

        // Plus the one before the window — the patch path diffs between
        // consecutive frontiers.
        let heads = prefix_heads_series(doc, start, versions.len());

        // Needed so the first row in the window still describes a delta.
        let mut prev_snapshot: Option<DocSnapshot> = if start == 0 {
            Some(DocSnapshot::default())
        } else {
            // `start != 0` here, so this never saturates below the value
            // plain subtraction would have produced.
            qdoc::history::materialize_at(doc, start.saturating_sub(1)).ok()
        };

        let mut out = Vec::with_capacity(versions.len().saturating_sub(start));
        for v in versions.iter().skip(start) {
            let cur_snapshot = qdoc::history::materialize_at(doc, v.index).ok();
            let before = heads
                .get(&v.index.wrapping_sub(1))
                .cloned()
                .unwrap_or_default();
            let after = heads.get(&v.index).cloned().unwrap_or_default();
            let (summary, op_class, text_delta) = match (&prev_snapshot, &cur_snapshot) {
                (Some(prev), Some(cur)) => structural_summary(prev, cur),
                _ => patch_summary(doc, &before, &after),
            };
            prev_snapshot = cur_snapshot;

            // The schema genesis is a change nobody made — its containers are
            // written by a fixed actor so two independently created documents
            // merge instead of shadowing each other. Listing it would attribute
            // a revision to a nonexistent person. Skipped here rather than in
            // `versions`, which stays positional (`materialize_at` / `prefix_heads`
            // address a change by its place).
            if qdoc::history::is_schema_genesis(v) {
                continue;
            }

            // Naming a version (or recording a cross-app run) is a change but
            // not a row — the name is already on screen as the band's title.
            // Checked only when there's no other summary, so the common path
            // pays no extra diff.
            if summary == NO_VISIBLE_CHANGE {
                let patches = doc.diff(&before, &after);
                if groups::is_group_marker(&patches) || runs::is_run_marker(&patches) {
                    continue;
                }
            }

            out.push(HistoryEntry {
                id: entry_id_from_hash(&v.change_hash),
                author: self.authors.resolve(&v.actor_hex),
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
            });
        }
        out
    }
}

/// The other half of [`HistorySource::entries`]: a restore needs a document
/// state for the [`HistoryEntryId`] a widget hands back. `None` when the id
/// names no change in this document — mixed ids from two artifacts.
pub fn heads_for_entry(doc: &mut AutoCommit, id: &HistoryEntryId) -> Option<Vec<ChangeHash>> {
    let index = version_index_for_entry(doc, id)?;
    Some(qdoc::history::prefix_heads(doc, index))
}

pub fn version_index_for_entry(doc: &mut AutoCommit, id: &HistoryEntryId) -> Option<usize> {
    qdoc::history::versions(doc)
        .into_iter()
        .find(|v| entry_id_from_hash(&v.change_hash) == *id)
        .map(|v| v.index)
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
/// the honest rendering of "we don't know when this happened": it sorts
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
    let diff = qdoc::diff_snapshots(prev, cur);

    let mut inserted = 0usize;
    let mut deleted = 0usize;
    let mut formatting = 0usize;
    for b in &diff.blocks {
        match b.op {
            qdoc::BlockOp::Added => {
                inserted =
                    inserted.saturating_add(b.new_text.as_deref().unwrap_or("").chars().count());
            }
            qdoc::BlockOp::Removed => {
                deleted =
                    deleted.saturating_add(b.old_text.as_deref().unwrap_or("").chars().count());
            }
            qdoc::BlockOp::Changed | qdoc::BlockOp::Equal => {
                for r in &b.runs {
                    match r.op {
                        qdoc::RunOp::Insert => {
                            inserted = inserted.saturating_add(r.text.chars().count());
                        }
                        qdoc::RunOp::Delete => {
                            deleted = deleted.saturating_add(r.text.chars().count());
                        }
                        qdoc::RunOp::Equal => {}
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
    patch_summary(doc, from, to).0
}

/// Describe a change by reading its own patch set. The fallback for documents
/// that don't carry the qdoc schema.
fn patch_summary(
    doc: &mut AutoCommit,
    before: &[ChangeHash],
    after: &[ChangeHash],
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

    for p in &patches {
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
    if field_deleted > 0 && fields == 0 {
        return (
            plural_signed(negative_count(field_deleted), "field"),
            OpClass::Tombstone,
            delta,
        );
    }
    if fields > 0 || field_deleted > 0 {
        return (
            plural(fields.saturating_add(field_deleted), "field"),
            OpClass::EditMeta,
            delta,
        );
    }
    if marks > 0 {
        return (plural(marks, "formatting change"), OpClass::EditMeta, delta);
    }
    (NO_VISIBLE_CHANGE.to_string(), OpClass::EditMeta, delta)
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
