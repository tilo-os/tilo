//! Version history derived from the automerge change graph: who/when per
//! change + time-travel (`materialize_at`). The *durable* change-blob chain and
//! the retention coarsening live in the FS layer (Phase 2); this is the read
//! model over a live document.

use crate::document::DocError;
use crate::model::{materialize, open};
use crate::snapshot::DocSnapshot;
use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

/// One entry on the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionMeta {
    pub index: usize,
    /// Hex of the authoring automerge actor (bound to the keypair identity).
    pub actor_hex: String,
    /// Change timestamp in ms (0 until wall-clock stamping is wired in the FS
    /// layer; authorship is always present).
    pub time_ms: i64,
    pub change_hash: String,
}

/// Enumerate the document's history in causal order (oldest first).
pub fn versions(doc: &mut AutoCommit) -> Vec<VersionMeta> {
    doc.get_changes(&[])
        .into_iter()
        .enumerate()
        .map(|(i, ch)| VersionMeta {
            index: i,
            actor_hex: ch.actor_id().to_hex_string(),
            time_ms: ch.timestamp(),
            change_hash: ch.hash().to_string(),
        })
        .collect()
}

/// Reconstruct the document as of `version` by replaying the first `version + 1`
/// changes into a fresh replica. `get_changes` returns a valid causal order, so
/// any prefix is dependency-closed.
pub fn materialize_at(doc: &mut AutoCommit, version: usize) -> Result<DocSnapshot, DocError> {
    let changes = doc.get_changes(&[]);
    let take = (version + 1).min(changes.len());
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
        assert!(vers.len() >= 4, "expected a multi-entry timeline, got {}", vers.len());
        assert!(vers.iter().all(|v| !v.actor_hex.is_empty()), "every change has an author");

        // Time-travel endpoints.
        let latest = d.materialize_at(vers.len() - 1).unwrap();
        assert_eq!(latest.blocks.len(), 2, "latest has both paragraphs");
        let v0 = d.materialize_at(0).unwrap();
        assert_eq!(v0.blocks.len(), 0, "version 0 is just the schema init");

        // Some intermediate version has exactly one block (after the first
        // insert, before the second).
        let saw_one = (0..vers.len()).any(|i| d.materialize_at(i).unwrap().blocks.len() == 1);
        assert!(saw_one, "an intermediate version should have a single block");
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
