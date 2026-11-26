//! Lossless presentation model for the explorer's selection detail.

use std::collections::HashSet;

use tilo_actions::ActionFacet;

use crate::widget::HistoryEntry;
use tilo_ui_base::widget::HistoryEntryId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryResourceTarget {
    pub app_id: Option<String>,
    pub app_label: Option<String>,
    pub id: String,
    pub kind: String,
    pub label: String,
}

impl HistoryResourceTarget {
    /// The name a person can read for this resource: its own label when that
    /// is a title, else the app that holds it. `None` when only identifiers
    /// are known, so no surface falls back to showing a record id.
    pub fn display_name(&self) -> Option<&str> {
        human_label(&self.label, &self.id).or_else(|| {
            self.app_label
                .as_deref()
                .and_then(|label| human_label(label, self.app_id.as_deref().unwrap_or_default()))
        })
    }
}

/// The app a change was made in, as a person names it: its own name, or for a first-party app
/// that gave only its id, the name the desktop shows it by.
pub fn place_app(app: &tilo_actions::ActionApp) -> Option<String> {
    let named = human_label(&app.label, &app.id)
        .filter(|label| *label != app.id && !label.contains('.'))
        .map(str::to_owned);
    named.or_else(|| tilo_app_id::apps::builtin_label_for_app_id(&app.id))
}

/// The record a change was made in, when it is one a person knows by its title: a note, a
/// channel, a document. An app's own collection (its messages, its issues, its folder) is named
/// by its kind, not a title, and the change reads as made in the app instead.
pub fn place_record(context: &tilo_actions::ActionContext) -> Option<String> {
    let resource = context.resource.as_ref()?;
    let label = human_label(&resource.label, &resource.id)?;
    let kind_word = resource
        .kind
        .rsplit(['.', ':', '/'])
        .next()
        .unwrap_or_default();
    let names_kind = |word: &str| {
        let label = label.trim().to_lowercase();
        let word = word.trim().to_lowercase();
        !word.is_empty()
            && (label == word
                || label.strip_suffix('s') == Some(word.as_str())
                || word.strip_suffix('s') == Some(label.as_str()))
    };
    let generic = resource.id == resource.kind
        || names_kind(kind_word)
        || context.app.as_ref().is_some_and(|app| {
            names_kind(&app.label) || place_app(app).is_some_and(|name| names_kind(&name))
        });
    (!generic).then(|| label.to_owned())
}

pub use tilo_ui_base::{presence::contains_opaque_id, presence::human_label};

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
                if detail.group_label.is_none() {
                    detail.group_label.clone_from(&group.label);
                }
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
            target: None,
        }
    }

    tilo_test_env::scenario! {
        fn a_selections_detail_keeps_its_actions_and_never_names_an_identifier;

        phase detail_deduplicates_shared_provenance_without_losing_actions;
        {
            let entries = [entry(1, "Ada", "brief.md"), entry(2, "Maren", "brief.md")];
            let detail = HistorySelectionDetail::from_range(&entries, (0, 1)).unwrap();
            assert_eq!(detail.entry_ids.len(), 2);
            assert_eq!(detail.authors, ["Ada", "Maren"]);
            assert_eq!(detail.resources.len(), 1);
            assert_eq!(detail.group_label.as_deref(), Some("Launch"));
            assert_eq!(detail.title, "Launch");
            assert!(detail.can_compare && detail.can_undo);
        }

        phase identifiers_are_not_names;
        {
            for opaque in [
                "3f2a9c41d0b7e8aa",
                "notes.note/3f2a9c41d0b7e8aa5c1d",
                "8d0c6a8e-2f1b-4c3d-9e4f-5a6b7c8d9e0f",
                "bafkreigh2akiscaildc4qp7k3ak7w3y6p4",
            ] {
                assert!(contains_opaque_id(opaque), "{opaque}");
                assert_eq!(human_label(opaque, "other"), None, "{opaque}");
            }
            for readable in ["Launch brief", "Q3 2026 plan", "brief.md", "Notes"] {
                assert!(!contains_opaque_id(readable), "{readable}");
            }
            assert_eq!(human_label("note-7", "note-7"), None);
            assert_eq!(human_label("  ", "x"), None);
            let target = HistoryResourceTarget {
                app_id: Some("org.tilo.Notes".into()),
                app_label: Some("Notes".into()),
                id: "3f2a9c41d0b7e8aa".into(),
                kind: "notes.note".into(),
                label: "3f2a9c41d0b7e8aa".into(),
            };
            assert_eq!(target.display_name(), Some("Notes"));
        }
    }
}

/// What a screen reader says for the agent `name`: the name with what it is, since the agent
/// mark beside it on screen is nothing a reader can hear. Never shown as a name.
pub fn agent_spoken_label(name: &str) -> String {
    tilo_ui_base::i18n::widget_format("member-agent-spoken", &[("name", name.into())])
}
