//! Version history derived from the automerge change graph: who/when per change + time-travel
//! (`materialize_at`). The durable change-blob chain and retention coarsening live in the FS layer; this is the read model over a live document.

use crate::document::DocError;
use crate::model::{materialize, open};
use crate::snapshot::DocSnapshot;
use automerge::transaction::CommitOptions;
use automerge::{AutoCommit, ChangeHash};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// One entry on the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionMeta {
    pub index: usize,
    /// Hex of the authoring automerge actor (bound to the keypair identity).
    pub actor_hex: String,
    /// Wall-clock time of the change, in milliseconds since the Unix epoch. Automerge stores a
    /// change's advisory timestamp in *seconds* and leaves it at `0` unless the committer supplies
    /// one, so `0` means "this change carries no recorded wall clock" — commit through
    /// [`commit_stamped`] for a real one; consumers must render an unstamped peer's change as "time unknown" rather than as 1970.
    pub time_ms: i64,
    pub change_hash: String,
    /// Optional Automerge commit message. qOS action dispatch uses this for
    /// durable semantic provenance; ordinary app commits may use it freely.
    pub message: Option<String>,
}

/// Commit the open transaction with a wall-clock timestamp attached. `AutoCommit::commit()`
/// leaves the change's advisory `time` field at `0`, which is why routing commits through here is
/// what makes [`VersionMeta::time_ms`] — and any date-range filter over the history — mean
/// something. Returns `None` when there was nothing to commit, exactly like `AutoCommit::commit`.
pub fn commit_stamped(doc: &mut AutoCommit) -> Option<ChangeHash> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    commit_stamped_at_with_message(doc, secs, None)
}

/// As [`commit_stamped`], but with a caller-supplied Unix time in **seconds** (automerge's own
/// unit). For hosts that own a clock (the FS layer replaying a durable change chain, a test spreading versions across a day) so the recorded time is when the edit happened.
pub fn commit_stamped_at(doc: &mut AutoCommit, unix_secs: i64) -> Option<ChangeHash> {
    commit_stamped_at_with_message(doc, unix_secs, None)
}

/// Commit a native document edit with both a timestamp and an optional
/// semantic message. This is the commit-boundary counterpart to action
/// dispatch: flowing CRDT text remains a native splice while history still
/// learns which app/resource the edit belongs to.
pub fn commit_stamped_with_message(
    doc: &mut AutoCommit,
    message: Option<String>,
) -> Option<ChangeHash> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    commit_stamped_at_with_message(doc, secs, message)
}

fn commit_stamped_at_with_message(
    doc: &mut AutoCommit,
    unix_secs: i64,
    message: Option<String>,
) -> Option<ChangeHash> {
    let mut options = CommitOptions::default().with_time(unix_secs);
    if let Some(message) = message {
        options = options.with_message(message);
    }
    doc.commit_with(options)
}

/// Enumerate the document's history in causal order (oldest first), including the schema genesis:
/// the list is positional and [`materialize_at`]/[`prefix_heads`] address a version by its place
/// in it, so nothing may be skipped here. A view of *authored revisions* filters the genesis out itself with [`is_schema_genesis`].
pub fn versions(doc: &mut AutoCommit) -> Vec<VersionMeta> {
    doc.get_changes(&[])
        .into_iter()
        .enumerate()
        .map(|(i, ch)| VersionMeta {
            index: i,
            actor_hex: ch.actor_id().to_hex_string(),
            time_ms: ch.timestamp().saturating_mul(1000),
            change_hash: ch.hash().to_string(),
            message: ch.message().cloned(),
        })
        .collect()
}

/// Whether `meta` describes the schema genesis rather than something a person
/// wrote. See [`crate::model::is_schema_genesis_actor`].
pub fn is_schema_genesis(meta: &VersionMeta) -> bool {
    meta.actor_hex == crate::model::schema_genesis_actor_hex()
}

/// Heads of the document as of the first `upto + 1` changes in causal order. `get_changes(&[])`
/// yields a dependency-closed causal order, so any prefix is a valid document state — but with two
/// collaborators that prefix can have several concurrent frontiers, so "the heads" is a set, not
/// the last hash (a change is a head of the prefix iff no other change *in the prefix* names it as
/// a dependency). Lets a caller address a historical version with `AutoCommit::diff` instead of only with `materialize_at`.
pub fn prefix_heads(doc: &mut AutoCommit, upto: usize) -> Vec<ChangeHash> {
    let changes = doc.get_changes(&[]);
    let take = upto.saturating_add(1).min(changes.len());
    // `take` is clamped to the length, so the prefix always exists.
    let prefix = changes.get(..take).unwrap_or(&[]);
    let depended_on: std::collections::HashSet<ChangeHash> = prefix
        .iter()
        .flat_map(|c| c.deps().iter().copied())
        .collect();
    prefix
        .iter()
        .map(automerge::Change::hash)
        .filter(|h| !depended_on.contains(h))
        .collect()
}

/// Reconstruct the document as of `version` by replaying the first `version + 1` changes into a fresh replica (`get_changes` returns a valid causal order, so any prefix is dependency-closed).
///
/// # Errors
/// Returns [`DocError`] if automerge rejects the replayed prefix, or if the
/// rebuilt replica has no readable document root.
pub fn materialize_at(doc: &mut AutoCommit, version: usize) -> Result<DocSnapshot, DocError> {
    let changes = doc.get_changes(&[]);
    let take = version.saturating_add(1).min(changes.len());
    let mut rebuilt = AutoCommit::new();
    rebuilt
        .apply_changes(changes.into_iter().take(take))
        .map_err(|e| DocError::Invalid(format!("apply_changes: {e}")))?;
    let c = open(&rebuilt, &automerge::ROOT)?;
    Ok(materialize(&rebuilt, &c))
}

#[cfg(test)]
mod tests {
    use crate::Document;

    #[test]
    fn version_chain_and_time_travel() {
        let mut d = Document::new();
        let a = d.insert_block(None, "paragraph");
        d.splice_text(&a, 0, 0, "first");
        d.splice_text(&a, 5, 0, " edit");
        let b = d.insert_block(Some(&a), "paragraph");
        d.splice_text(&b, 0, 0, "second");

        let vers = d.versions();
        // schema-init + 5 edits → several authored changes.
        assert!(
            vers.len() >= 4,
            "expected a multi-entry timeline, got {}",
            vers.len()
        );
        assert!(
            vers.iter().all(|v| !v.actor_hex.is_empty()),
            "every change has an author"
        );

        let latest = d.materialize_at(vers.len() - 1).unwrap();
        assert_eq!(latest.blocks.len(), 2, "latest has both paragraphs");
        let v0 = d.materialize_at(0).unwrap();
        assert_eq!(v0.blocks.len(), 0, "version 0 is just the schema init");

        // Some intermediate version has exactly one block (after the first insert, before the second).
        let saw_one = (0..vers.len()).any(|i| d.materialize_at(i).unwrap().blocks.len() == 1);
        assert!(
            saw_one,
            "an intermediate version should have a single block"
        );
    }

    #[test]
    fn time_travel_shows_text_growth() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "hello");
        let mid = d.versions().len() - 1; // after "hello"
        d.splice_text(&p, 5, 0, " world");

        let at_mid = d.materialize_at(mid).unwrap();
        assert_eq!(at_mid.blocks[0].text, "hello");
        let last = d.versions().len() - 1;
        let now = d.materialize_at(last).unwrap();
        assert_eq!(now.blocks[0].text, "hello world");
    }
}
