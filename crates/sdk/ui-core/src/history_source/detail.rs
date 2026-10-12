//! Lossless presentation model for the explorer's selection detail.

use std::collections::HashSet;

use tilo_actions::ActionFacet;

use crate::widget::{HistoryEntry, HistoryEntryId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryResourceTarget {
    pub app_id: Option<String>,
    pub app_label: Option<String>,
    pub id: String,
    pub kind: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistorySelectionDetail {
    pub entry_ids: Vec<HistoryEntryId>,
    pub title: String,
    pub summaries: Vec<String>,
    pub authors: Vec<String>,
    pub resources: Vec<HistoryResourceTarget>,
    pub facets: Vec<ActionFacet>,
    pub group_id: Option<String>,
    pub group_label: Option<String>,
    pub purpose: Option<String>,
    pub can_compare: bool,
    pub can_undo: bool,
    pub contains_unclassified: bool,
}

impl HistorySelectionDetail {
    /// Inclusive entry-index selection. Invalid ranges return no detail rather
    /// than silently clamping onto a different action.
    pub fn from_range(entries: &[HistoryEntry], range: (usize, usize)) -> Option<Self> {
        let (start, end) = (range.0.min(range.1), range.0.max(range.1));
        let selected = entries.get(start..=end)?;
        let mut detail = Self {
            entry_ids: selected.iter().map(|entry| entry.id).collect(),
            title: selected
                .first()
                .map(|entry| entry.summary.clone())
                .unwrap_or_default(),
            summaries: selected.iter().map(|entry| entry.summary.clone()).collect(),
            can_compare: !selected.is_empty(),
            can_undo: !selected.is_empty()
                && selected
                    .iter()
                    .all(super::super::widget::history::HistoryEntry::is_revertible),
            contains_unclassified: selected.iter().any(|entry| entry.context.is_none()),
            ..Self::default()
        };
        let mut authors = HashSet::new();
        let mut resources = HashSet::new();
        let mut facets = HashSet::new();
        for entry in selected {
            if authors.insert(entry.author.name.clone()) {
                detail.authors.push(entry.author.name.clone());
            }
            let Some(context) = entry.context.as_ref() else {
                continue;
            };
            if detail.purpose.is_none() {
                detail.purpose.clone_from(&context.purpose);
            }
            if let Some(group) = context.group.as_ref() {
                detail.group_id.get_or_insert_with(|| group.id.clone());
                detail
                    .group_label
                    .get_or_insert_with(|| group.label.clone());
            }
            if let Some(resource) = context.resource.as_ref()
                && resources.insert((resource.id.clone(), resource.kind.clone()))
            {
                detail.resources.push(HistoryResourceTarget {
                    app_id: context.app.as_ref().map(|app| app.id.clone()),
                    app_label: context.app.as_ref().map(|app| app.label.clone()),
                    id: resource.id.clone(),
                    kind: resource.kind.clone(),
                    label: resource.label.clone(),
                });
            }
            for facet in &context.facets {
                if facets.insert((facet.key.clone(), facet.value.clone())) {
                    detail.facets.push(facet.clone());
                }
            }
        }
        if let Some(label) = detail.group_label.as_ref() {
            detail.title.clone_from(label);
        } else if let Some(purpose) = detail.purpose.as_ref() {
            detail.title.clone_from(purpose);
        }
        Some(detail)
    }
}

#[cfg(test)]
mod tests {
    use crate::time::SystemTime;

    use crate::widget::EntryKind;
    use collab_types::ArtifactKind;
    use tilo_actions::{ActionContext, ActionFacet};

    use super::*;
    use crate::widget::HistoryAuthor;

    fn entry(id: u8, author: &str, resource: &str) -> HistoryEntry {
        HistoryEntry {
            id: HistoryEntryId([id; 16]),
            author: HistoryAuthor::new(author, 0.0, author),
            entry_kind: EntryKind::Details,
            artifact_kind: ArtifactKind::File,
            timestamp: SystemTime::UNIX_EPOCH,
            summary: format!("Edited {resource}"),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context: Some(
                ActionContext::for_resource(resource, "file", resource)
                    .with_app("files", "Files")
                    .with_group("issue:42", "Launch", "issue")
                    .with_facet(ActionFacet::new("file-type", "Markdown")),
            ),
        }
    }

    #[test]
    fn detail_deduplicates_shared_provenance_without_losing_actions() {
        let entries = [entry(1, "Ada", "brief.md"), entry(2, "Maren", "brief.md")];
        let detail = HistorySelectionDetail::from_range(&entries, (0, 1)).unwrap();
        assert_eq!(detail.entry_ids.len(), 2);
        assert_eq!(detail.authors, ["Ada", "Maren"]);
        assert_eq!(detail.resources.len(), 1);
        assert_eq!(detail.group_label.as_deref(), Some("Launch"));
        assert_eq!(detail.title, "Launch");
        assert!(detail.can_compare && detail.can_undo);
    }
}
