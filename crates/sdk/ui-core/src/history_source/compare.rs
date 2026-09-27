//! Exact before/after materialization for one action or a selected group.

use std::collections::{HashMap, HashSet};

use automerge::{AutoCommit, ChangeHash, ReadDoc};
use tilo_automerge::ApplyIncoming;

use crate::history_source::entry_id_from_hash;
use crate::widget::HistoryEntryId;

#[derive(Clone, Debug)]
pub struct HistoryComparison {
    pub entries: Vec<HistoryEntryId>,
    pub before_heads: Vec<ChangeHash>,
    pub after_heads: Vec<ChangeHash>,
    pub patches: Vec<automerge::Patch>,
    /// Full Automerge snapshots. A resource renderer can load these and
    /// produce its native visual/text/binary comparison without the history
    /// layer understanding the app schema.
    pub before_document: Vec<u8>,
    pub after_document: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompareError {
    EmptySelection,
    UnknownEntry(HistoryEntryId),
    Materialize(String),
}

/// Compare the dependency frontier immediately before the selected changes to
/// the frontier after every selected change. Concurrent changes outside the
/// selection are excluded. Pending edits are neither committed nor included.
///
/// # Errors
///
/// Returns [`CompareError::EmptySelection`] for an empty selection,
/// [`CompareError::UnknownEntry`] for an unknown entry, or
/// [`CompareError::Materialize`] when Automerge cannot build either snapshot.
pub fn compare_entries(
    doc: &mut AutoCommit,
    entries: &[HistoryEntryId],
) -> Result<HistoryComparison, CompareError> {
    if entries.is_empty() {
        return Err(CompareError::EmptySelection);
    }
    super::with_committed_history(doc, |doc| compare_committed_entries(doc, entries))
}

fn compare_committed_entries(
    doc: &mut AutoCommit,
    entries: &[HistoryEntryId],
) -> Result<HistoryComparison, CompareError> {
    let changes = doc.get_changes(&[]);
    let mut selected = Vec::with_capacity(entries.len());
    for id in entries {
        let Some(change) = changes
            .iter()
            .find(|change| entry_id_from_hash(&change.hash().to_string()) == *id)
        else {
            return Err(CompareError::UnknownEntry(*id));
        };
        selected.push(change.clone());
    }
    let selected_hashes: HashSet<ChangeHash> =
        selected.iter().map(automerge::Change::hash).collect();
    let depended_on_inside: HashSet<ChangeHash> = selected
        .iter()
        .flat_map(|change| change.deps().iter().copied())
        .filter(|hash| selected_hashes.contains(hash))
        .collect();
    let after_heads: Vec<ChangeHash> = selected
        .iter()
        .map(automerge::Change::hash)
        .filter(|hash| !depended_on_inside.contains(hash))
        .collect();
    let before_heads: Vec<ChangeHash> = selected
        .iter()
        .flat_map(|change| change.deps().iter().copied())
        .filter(|hash| !selected_hashes.contains(hash))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let patches = doc.diff(&before_heads, &after_heads);
    let before_document = document_at(&changes, doc, &before_heads)?.save();
    let after_document = document_at(&changes, doc, &after_heads)?.save();
    Ok(HistoryComparison {
        entries: entries.to_vec(),
        before_heads,
        after_heads,
        patches,
        before_document,
        after_document,
    })
}

#[cfg(test)]
mod tests {
    use automerge::ReadDoc;
    use automerge::transaction::Transactable;

    use super::*;

    #[test]
    fn group_comparison_materialises_exact_states_and_refuses_empty_selection() {
        // phase: group_comparison_materializes_exact_before_and_after_states
        {
            let mut doc = AutoCommit::new();
            doc.put(automerge::ROOT, "title", "before").unwrap();
            doc.commit();
            doc.put(automerge::ROOT, "title", "during").unwrap();
            let a = doc.commit().unwrap();
            doc.put(automerge::ROOT, "status", "done").unwrap();
            let b = doc.commit().unwrap();
            let ids = [
                entry_id_from_hash(&a.to_string()),
                entry_id_from_hash(&b.to_string()),
            ];

            let comparison = compare_entries(&mut doc, &ids).unwrap();
            assert!(!comparison.patches.is_empty());
            let before = AutoCommit::load(&comparison.before_document).unwrap();
            let after = AutoCommit::load(&comparison.after_document).unwrap();
            assert_eq!(
                before
                    .get(automerge::ROOT, "title")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("before")
            );
            assert_eq!(
                after
                    .get(automerge::ROOT, "title")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("during")
            );
            assert_eq!(
                after
                    .get(automerge::ROOT, "status")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("done")
            );
        }
        // phase: unknown_or_empty_selection_is_refused
        {
            let mut doc = AutoCommit::new();
            assert_eq!(
                compare_entries(&mut doc, &[]).unwrap_err(),
                CompareError::EmptySelection
            );
            assert!(matches!(
                compare_entries(&mut doc, &[HistoryEntryId([9; 16])]),
                Err(CompareError::UnknownEntry(_))
            ));
        }
    }
}

/// The committed document as of exactly `heads`: the dependency closure of
/// those changes replayed, in the document's own causal order, into a fresh
/// replica with the live text encoding. Automerge 0.8's `fork_at` can walk a
/// valid merged graph out of order and panic; this replay cannot.
fn document_at(
    changes: &[automerge::Change],
    doc: &AutoCommit,
    heads: &[ChangeHash],
) -> Result<AutoCommit, CompareError> {
    let by_hash: HashMap<ChangeHash, &automerge::Change> = changes
        .iter()
        .map(|change| (change.hash(), change))
        .collect();
    let mut closure = HashSet::new();
    let mut pending = heads.to_vec();
    while let Some(hash) = pending.pop() {
        let change = by_hash.get(&hash).ok_or_else(|| {
            CompareError::Materialize(format!("change {hash} is not in this document"))
        })?;
        if closure.insert(hash) {
            pending.extend(change.deps().iter().copied());
        }
    }
    let mut out = AutoCommit::new_with_encoding(doc.text_encoding());
    out.apply_incoming(
        changes
            .iter()
            .filter(|change| closure.contains(&change.hash()))
            .cloned(),
    )
    .map_err(|error| CompareError::Materialize(error.to_string()))?;
    Ok(out)
}
