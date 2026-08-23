//! Turning a live CRDT document into the rows the history widgets render.
//!
//! [`ActivityTimeline`](crate::widget::ActivityTimeline) and
//! [`HistoryScrubber`](crate::widget::HistoryScrubber) both consume
//! `Vec<HistoryEntry>` and are deliberately source-agnostic. This module is the
//! producer: given an `automerge::AutoCommit`, it walks the change graph
//! through [`qdoc::history::versions`] and emits one [`HistoryEntry`] per
//! change, with a real author, a real timestamp, a real op class, and a summary
//! derived from what the change actually did to the document.
//!
//! It lives in `ui` rather than in a leaf crate because both halves of the
//! system need it and both already depend on `ui`: an app renders the scrubber
//! over its own document, and the compositor renders the room-level timeline
//! over the chrome document. Putting it anywhere else would mean a second copy.
//!
//! # How a summary is derived
//!
//! Two strategies, in order of preference:
//!
//! 1. **Structural.** When the document carries the `qdoc` schema, each version
//!    is materialised with [`qdoc::history::materialize_at`] and consecutive
//!    snapshots are compared with [`qdoc::diff_snapshots`]. That yields real
//!    per-paragraph insert/delete runs, so a row reads `"+12 -3 chars"` because
//!    twelve characters were inserted and three deleted — not because a change
//!    happened to contain fifteen ops.
//! 2. **Patch.** For any other document (a plain `ui::State`, the compositor's
//!    chrome doc) the change's own patch set is read with `AutoCommit::diff`
//!    between the version's bracketing heads. Text splices still give exact
//!    character counts; map and list edits give element counts. This is weaker
//!    than the structural path only in that it cannot see qdoc-level concepts
//!    like comments or block moves.
//!
//! Neither path guesses. A change whose effect the producer cannot describe
//! gets the summary `"no visible change"` rather than an invented one.
//!
//! # Addressing a version
//!
//! [`HistoryEntry::id`] is the first sixteen bytes of the change hash, so it is
//! content-addressed and stable across peers — which is what makes
//! [`heads_for_entry`] able to turn a row the user clicked back into a document
//! state, and hence into a restore.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use automerge::{AutoCommit, ChangeHash, ObjType, PatchAction, ReadDoc};
use collab_types::{ArtifactKind, OpClass};
use qdoc::snapshot::DocSnapshot;

use crate::widget::history::{HistoryEntry, HistoryEntryId};

pub mod authors;
pub mod groups;
pub use authors::{
    actor_handle, local_identity_author, short_id, stable_hue, AuthorDirectory, UNKNOWN_AUTHOR_NAME,
};
pub use groups::{
    clear_name, close_range, is_group_marker, name_range, read_named_ranges, GROUPS_KEY,
};

/// Default ceiling on how many of the most recent versions are turned into
/// rows.
///
/// The structural path materialises one snapshot per version, so the cost is
/// linear in versions and in document size. Two hundred rows is well past what
/// a scrubber track can resolve into distinct ticks at any realistic width, and
/// keeps a long-lived document's history pane responsive. Raise it with
/// [`HistorySource::with_max_entries`] when a caller genuinely wants the whole
/// chain.
pub const DEFAULT_MAX_ENTRIES: usize = 200;

/// Summary text for a change whose effect the producer could not describe.
/// Deliberately not a guess.
pub const NO_VISIBLE_CHANGE: &str = "no visible change";

/// Builds [`HistoryEntry`] rows from a live document.
///
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
    /// A producer with an empty author directory — every actor will resolve to
    /// an unknown participant until something is registered.
    pub fn new() -> Self {
        Self {
            authors: AuthorDirectory::new(),
            artifact_kind: ArtifactKind::File,
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }

    /// Builder: install the directory that resolves actor ids to people.
    pub fn with_authors(mut self, authors: AuthorDirectory) -> Self {
        self.authors = authors;
        self
    }

    /// Builder: tag every produced row with this artifact kind.
    pub fn with_artifact_kind(mut self, kind: ArtifactKind) -> Self {
        self.artifact_kind = kind;
        self
    }

    /// Builder: override [`DEFAULT_MAX_ENTRIES`].
    pub fn with_max_entries(mut self, max: usize) -> Self {
        self.max_entries = max.max(1);
        self
    }

    /// Mutable access to the directory, so a long-lived producer can learn
    /// about a peer that joined after it was constructed.
    pub fn authors_mut(&mut self) -> &mut AuthorDirectory {
        &mut self.authors
    }

    /// Read-only access to the directory.
    pub fn authors(&self) -> &AuthorDirectory {
        &self.authors
    }

    /// Produce the timeline, oldest first.
    ///
    /// The widgets sort for display (the timeline defaults to newest-first, the
    /// scrubber runs the track left-to-right in causal order), so this returns
    /// the document's own causal order and lets them decide.
    pub fn entries(&self, doc: &mut AutoCommit) -> Vec<HistoryEntry> {
        let versions = qdoc::history::versions(doc);
        if versions.is_empty() {
            return Vec::new();
        }
        let start = versions.len().saturating_sub(self.max_entries);

        // Head sets for every version in the window, plus the one before it —
        // the patch path diffs between consecutive frontiers.
        let heads = prefix_heads_series(doc, start, versions.len());

        // The structural path needs the snapshot just before the window opens
        // so the first row in the window still describes a delta.
        let mut prev_snapshot: Option<DocSnapshot> = if start == 0 {
            Some(DocSnapshot::default())
        } else {
            qdoc::history::materialize_at(doc, start - 1).ok()
        };

        let mut out = Vec::with_capacity(versions.len() - start);
        for v in versions.iter().skip(start) {
            // The schema genesis is a change in the graph that no person wrote —
            // every document in the world carries the same one. It is a point on
            // the timeline you can travel to, but it is not a revision, and a row
            // for it credits an author who does not exist. The snapshot still
            // advances so the next row is a delta from it and not from nothing.
            if qdoc::history::is_schema_genesis(v) {
                prev_snapshot = qdoc::history::materialize_at(doc, v.index).ok();
                continue;
            }
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

            // Naming a version is a change like any other, but it is not a row:
            // the name is already on screen as the band's title, and a "1 field"
            // row directly under the band it named says nothing. The check runs
            // only for changes that described nothing else, so the common path
            // pays no extra diff.
            if summary == NO_VISIBLE_CHANGE && groups::is_group_marker(&doc.diff(&before, &after)) {
                continue;
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
                // `heads_for_entry` rather than by cracking open a payload, so
                // there is nothing honest to put here yet. Leaving it `None` is
                // the documented "no per-entry payload" case.
                diff_payload: None,
                // Redaction display is projected from the broker's redaction
                // log, which is keyed by artifact and not reachable from a bare
                // document. A caller that has one overwrites this field.
                redaction: None,
            });
        }
        out
    }
}

/// Resolve a row the user clicked back to the document heads that row produced.
///
/// This is the other half of [`HistorySource::entries`]: the widgets hand back
/// a [`HistoryEntryId`], and a restore needs a document state. Returns `None`
/// when the id names no change in this document — which is what happens if the
/// caller mixes ids from two different artifacts.
pub fn heads_for_entry(doc: &mut AutoCommit, id: &HistoryEntryId) -> Option<Vec<ChangeHash>> {
    let index = version_index_for_entry(doc, id)?;
    Some(qdoc::history::prefix_heads(doc, index))
}

/// Resolve a row to its index in the document's causal change order.
pub fn version_index_for_entry(doc: &mut AutoCommit, id: &HistoryEntryId) -> Option<usize> {
    qdoc::history::versions(doc)
        .into_iter()
        .find(|v| entry_id_from_hash(&v.change_hash) == *id)
        .map(|v| v.index)
}

/// First sixteen bytes of a change hash, as the widgets' opaque row id.
///
/// The hash is the document's own content address for that change, so two peers
/// independently building a timeline over the same history agree on the ids —
/// which is what lets one peer's `[Revert]` mean anything to another.
pub fn entry_id_from_hash(change_hash_hex: &str) -> HistoryEntryId {
    let mut bytes = [0u8; 16];
    let src = change_hash_hex.as_bytes();
    for (i, slot) in bytes.iter_mut().enumerate() {
        let (hi, lo) = (src.get(i * 2), src.get(i * 2 + 1));
        let hi = hi.and_then(|c| (*c as char).to_digit(16)).unwrap_or(0);
        let lo = lo.and_then(|c| (*c as char).to_digit(16)).unwrap_or(0);
        *slot = (hi * 16 + lo) as u8;
    }
    HistoryEntryId(bytes)
}

/// A change's recorded wall clock, or the Unix epoch when it carries none.
///
/// `qdoc::history::VersionMeta::time_ms` is `0` for a change committed without
/// a timestamp (plain `AutoCommit::commit`, or a peer that does not stamp). The
/// epoch is the honest rendering of "we do not know when this happened": it
/// sorts oldest and it falls outside any real date-range filter, rather than
/// being quietly filed under today.
fn timestamp_from_ms(time_ms: i64) -> SystemTime {
    if time_ms <= 0 {
        return UNIX_EPOCH;
    }
    UNIX_EPOCH + Duration::from_millis(time_ms as u64)
}

/// Head sets for versions `start - 1 ..= end - 1`, computed in one pass.
///
/// `qdoc::history::prefix_heads` re-reads the change list per call; a timeline
/// asks for every version, so this builds the whole series from a single read.
fn prefix_heads_series(
    doc: &mut AutoCommit,
    start: usize,
    end: usize,
) -> HashMap<usize, Vec<ChangeHash>> {
    let changes = doc.get_changes(&[]);
    let mut out = HashMap::new();
    let lo = start.saturating_sub(1);
    for i in lo..end.min(changes.len()) {
        let prefix = &changes[..=i];
        let depended_on: std::collections::HashSet<ChangeHash> =
            prefix.iter().flat_map(|c| c.deps().iter().copied()).collect();
        out.insert(
            i,
            prefix
                .iter()
                .map(|c| c.hash())
                .filter(|h| !depended_on.contains(h))
                .collect(),
        );
    }
    out
}

// =====================================================================
// Summaries
// =====================================================================

/// Describe the step from `prev` to `cur` using qdoc's structural diff.
///
/// The op class follows what actually moved, in the order a reader cares about:
/// a new comment is a comment even if it also touched text; content edits beat
/// pure deletions; a reordering with no content change is a move; anything left
/// is metadata.
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
            qdoc::BlockOp::Added => inserted += b.new_text.as_deref().unwrap_or("").chars().count(),
            qdoc::BlockOp::Removed => deleted += b.old_text.as_deref().unwrap_or("").chars().count(),
            qdoc::BlockOp::Changed | qdoc::BlockOp::Equal => {
                for r in &b.runs {
                    match r.op {
                        qdoc::RunOp::Insert => inserted += r.text.chars().count(),
                        qdoc::RunOp::Delete => deleted += r.text.chars().count(),
                        qdoc::RunOp::Equal => {}
                    }
                }
            }
        }
        formatting += b.attr_changes.len() + b.marks_added.len() + b.marks_removed.len();
        if b.kind_change.is_some() {
            formatting += 1;
        }
    }

    let blocks_added = diff.summary.added;
    let blocks_removed = diff.summary.removed;
    let props = diff.properties.len();
    let comments = cur.comments.len().saturating_sub(prev.comments.len());
    let pure_reorder = blocks_added == 0 && blocks_removed == 0 && block_order_changed(prev, cur);
    // A change that took blocks away and put nothing back is a deletion, not an
    // edit — that is what `OpClass::Tombstone` names, and what puts the row
    // under the timeline's "Deletes" chip. It is checked before the character
    // counts precisely because a removed block also removes its text, and
    // "-1 block" is what the reader wants to see, not "-6 chars".
    let pure_removal = blocks_removed > 0 && blocks_added == 0 && inserted == 0;

    // The character counts are complete on this path — a removed block
    // contributes the text it took with it — so every branch can report them,
    // and a group can aggregate across branches without mixing counted and
    // uncounted members.
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
            plural_signed(-(blocks_removed as i64), "block"),
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
            plural_signed(blocks_added as i64, "block"),
            OpClass::Splice,
            delta,
        );
    }
    if blocks_removed > 0 {
        return (
            plural_signed(-(blocks_removed as i64), "block"),
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

/// True when both snapshots hold the same set of top-level block ids but in a
/// different order — a move rather than an edit.
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

/// Describe a change by reading its own patch set. The fallback for documents
/// that do not carry the qdoc schema.
fn patch_summary(
    doc: &mut AutoCommit,
    before: &[ChangeHash],
    after: &[ChangeHash],
) -> (String, OpClass, Option<(usize, usize)>) {
    let patches = doc.diff(before, after);
    // A change that only named a version reports as nothing visible; the caller
    // recognises that and drops the row rather than showing "1 field" under the
    // band the name just created.
    if groups::is_group_marker(&patches) {
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
            PatchAction::SpliceText { value, .. } => inserted += value.make_string().chars().count(),
            PatchAction::DeleteSeq { length, .. } => {
                if is_text {
                    text_deleted += length;
                } else {
                    list_deleted += length;
                }
            }
            PatchAction::Insert { values, .. } => list_inserted += values.len(),
            PatchAction::PutMap { .. } => fields += 1,
            PatchAction::DeleteMap { .. } => field_deleted += 1,
            PatchAction::PutSeq { .. } => list_inserted += 1,
            PatchAction::Increment { .. } => fields += 1,
            PatchAction::Mark { marks: m } => marks += m.len(),
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
            plural_signed(list_inserted as i64, "item"),
            OpClass::Splice,
            delta,
        );
    }
    if list_deleted > 0 {
        return (
            plural_signed(-(list_deleted as i64), "item"),
            OpClass::Tombstone,
            delta,
        );
    }
    if field_deleted > 0 && fields == 0 {
        return (
            plural_signed(-(field_deleted as i64), "field"),
            OpClass::Tombstone,
            delta,
        );
    }
    if fields > 0 || field_deleted > 0 {
        return (
            plural(fields + field_deleted, "field"),
            OpClass::EditMeta,
            delta,
        );
    }
    if marks > 0 {
        return (
            plural(marks, "formatting change"),
            OpClass::EditMeta,
            delta,
        );
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

fn plural_signed(n: i64, noun: &str) -> String {
    let mag = n.unsigned_abs() as usize;
    let sign = if n < 0 { '-' } else { '+' };
    if mag == 1 {
        format!("{sign}1 {noun}")
    } else {
        format!("{sign}{mag} {noun}s")
    }
}

#[cfg(test)]
mod tests;
