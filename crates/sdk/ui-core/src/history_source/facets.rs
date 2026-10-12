//! Deterministic browse lenses over action-aware history.
//!
//! The explorer can present dimensions without knowing which apps will add
//! them later. Counts are derived from durable action context; legacy entries
//! simply contribute to people and operation-class lenses.

use std::collections::BTreeMap;

use crate::widget::EntryKind;

use crate::widget::HistoryEntry;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryFacetIndex {
    pub people: BTreeMap<String, usize>,
    pub apps: BTreeMap<String, usize>,
    pub resource_kinds: BTreeMap<String, usize>,
    pub resources: BTreeMap<String, usize>,
    pub facets: BTreeMap<String, BTreeMap<String, usize>>,
    pub operations: BTreeMap<String, usize>,
    pub work_groups: BTreeMap<String, usize>,
    pub unclassified: usize,
}

impl HistoryFacetIndex {
    pub fn from_entries(entries: &[HistoryEntry]) -> Self {
        let mut index = Self::default();
        for entry in entries {
            bump(&mut index.people, &entry.author.name);
            bump(&mut index.operations, &entry.entry_kind.label());
            let Some(context) = entry.context.as_ref() else {
                index.unclassified = index.unclassified.saturating_add(1);
                continue;
            };
            if let Some(app) = context.app.as_ref() {
                bump(&mut index.apps, &app.label);
            }
            if let Some(resource) = context.resource.as_ref() {
                bump(&mut index.resource_kinds, &resource.kind);
                bump(&mut index.resources, &resource.label);
            }
            if let Some(group) = context.group.as_ref() {
                bump(&mut index.work_groups, &group.label);
            }
            for facet in &context.facets {
                bump(
                    index.facets.entry(facet.key.clone()).or_default(),
                    &facet.value,
                );
            }
        }
        index
    }
}

fn bump(map: &mut BTreeMap<String, usize>, key: &str) {
    let count = map.entry(key.to_string()).or_default();
    *count = count.saturating_add(1);
}

impl EntryKind {
    /// What kind of change this is, in the desktop's language.
    #[must_use]
    pub fn label(self) -> String {
        crate::i18n!(match self {
            EntryKind::Text => "history-op-splice",
            EntryKind::Details => "history-op-edit-meta",
            EntryKind::Move => "history-op-move",
            EntryKind::Comment => "history-op-comment",
            EntryKind::Delete => "history-op-tombstone",
            EntryKind::Restore => "history-op-restore",
            EntryKind::Access => "history-op-govern",
            EntryKind::Compaction => "history-op-compaction",
            EntryKind::Redaction => "history-op-redact",
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::time::SystemTime;

    use collab_types::ArtifactKind;
    use tilo_actions::{ActionContext, ActionFacet};

    use super::*;
    use crate::widget::{HistoryAuthor, HistoryEntryId};

    #[test]
    fn indexes_open_action_dimensions_and_keeps_legacy_people() {
        let author = HistoryAuthor::new("Ada", 20.0, "ada");
        let contextual = HistoryEntry {
            id: HistoryEntryId([1; 16]),
            author,
            timestamp: SystemTime::UNIX_EPOCH,
            entry_kind: EntryKind::Details,
            artifact_kind: ArtifactKind::File,
            summary: "Rename".into(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: Some(
                ActionContext::for_resource("1", "file", "brief.md")
                    .with_app("files", "Files")
                    .with_group("issue:42", "Launch brief", "issue")
                    .with_facet(ActionFacet::new("file-type", "Markdown")),
            ),
        };
        let mut legacy = contextual.clone();
        legacy.id = HistoryEntryId([2; 16]);
        legacy.context = None;

        let index = HistoryFacetIndex::from_entries(&[contextual, legacy]);
        assert_eq!(index.people.get("Ada"), Some(&2));
        assert_eq!(index.apps.get("Files"), Some(&1));
        assert_eq!(index.resource_kinds.get("file"), Some(&1));
        assert_eq!(index.resources.get("brief.md"), Some(&1));
        assert_eq!(index.facets["file-type"].get("Markdown"), Some(&1));
        assert_eq!(index.work_groups.get("Launch brief"), Some(&1));
        assert_eq!(index.unclassified, 1);
    }
}
