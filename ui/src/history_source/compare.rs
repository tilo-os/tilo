//! Exact before/after materialization for one action or a selected group.

use std::collections::HashSet;

use automerge::{AutoCommit, ChangeHash};

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
/// selection are excluded.
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
    let changes = doc.get_changes(&[]);
    let mut selected = Vec::with_capacity(entries.len());
    for id in entries {
        let Some(change) = changes
            .iter()
            .find(|change| entry_id_from_hash(&change.hash().to_string()) == *id)
        else {
            return Err(CompareError::UnknownEntry(*id));
        };
        selected.push((*change).clone());
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
    let before_document = doc
        .fork_at(&before_heads)
        .map_err(|error| CompareError::Materialize(error.to_string()))?
        .save();
    let after_document = doc
        .fork_at(&after_heads)
        .map_err(|error| CompareError::Materialize(error.to_string()))?
        .save();
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
    fn group_comparison_materializes_exact_before_and_after_states() {
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
                .to_str(),
            Some("before")
        );
        assert_eq!(
            after
                .get(automerge::ROOT, "title")
                .unwrap()
                .unwrap()
                .0
                .to_str(),
            Some("during")
        );
        assert_eq!(
            after
                .get(automerge::ROOT, "status")
                .unwrap()
                .unwrap()
                .0
                .to_str(),
            Some("done")
        );
    }

    #[test]
    fn unknown_or_empty_selection_is_refused() {
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
