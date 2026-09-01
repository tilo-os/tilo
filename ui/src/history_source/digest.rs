//! A compact, deterministic catch-up model over full history.
//!
//! The top bar and the activity pane should not invent separate notions of
//! “what happened”. Both consume [`ActivityDigest`]: the same grouping as the
//! full timeline, reduced to a few truthful briefs. An LLM may rewrite these
//! briefs for tone, but it is never the source of grouping, attribution, or
//! counts.

use std::collections::HashSet;
use std::time::SystemTime;

use crate::widget::history::{HistoryAuthor, HistoryEntry};
use crate::widget::history_group::{GroupingConfig, group_entries};

#[derive(Clone, Debug, PartialEq)]
pub struct ActivityBrief {
    /// User/agent-supplied group name, or the deterministic fallback title.
    pub title: String,
    /// People and measured deltas, in the timeline's own words.
    pub detail: String,
    pub authors: Vec<HistoryAuthor>,
    pub apps: Vec<String>,
    pub resources: Vec<String>,
    pub changes: usize,
    pub first_at: SystemTime,
    pub last_at: SystemTime,
    /// Stable explicit work-group id, when the actions supplied one.
    pub group_id: Option<String>,
}

impl ActivityBrief {
    /// Single line suitable for the passive top-bar glance surface.
    pub fn glance(&self) -> String {
        let who = match self.authors.as_slice() {
            [] => String::new(),
            [author] => author.name.clone(),
            [first, second] => format!("{} and {}", first.name, second.name),
            [first, ..] => format!(
                "{} and {} others",
                first.name,
                self.authors.len().saturating_sub(1)
            ),
        };
        if who.is_empty() {
            self.title.clone()
        } else {
            format!("{who} · {}", self.title)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActivityDigest {
    pub briefs: Vec<ActivityBrief>,
    pub total_changes: usize,
    pub people: usize,
}

impl ActivityDigest {
    /// Build newest-first briefs for changes strictly after `since`. `limit`
    /// bounds what a glance/catch-up surface needs without changing the full
    /// timeline underneath it.
    pub fn from_entries(entries: &[HistoryEntry], since: SystemTime, limit: usize) -> Self {
        let recent: Vec<HistoryEntry> = entries
            .iter()
            .filter(|entry| entry.timestamp > since)
            .cloned()
            .collect();
        let people = recent
            .iter()
            .map(|entry| {
                (
                    entry.author.name.clone(),
                    entry.author.identity_short.clone(),
                )
            })
            .collect::<HashSet<_>>()
            .len();
        let groups = group_entries(&recent, &[], &GroupingConfig::default());
        let mut briefs: Vec<ActivityBrief> = groups
            .iter()
            .map(|group| {
                let slice = recent.get(group.start..group.end).unwrap_or(&[]);
                let mut apps = Vec::new();
                let mut resources = Vec::new();
                for entry in slice {
                    if let Some(app) = entry
                        .context
                        .as_ref()
                        .and_then(|context| context.app.as_ref())
                        && !apps.contains(&app.label)
                    {
                        apps.push(app.label.clone());
                    }
                    if let Some(resource) = entry
                        .context
                        .as_ref()
                        .and_then(|context| context.resource.as_ref())
                        && !resources.contains(&resource.label)
                    {
                        resources.push(resource.label.clone());
                    }
                }
                let group_id = slice
                    .first()
                    .and_then(|entry| entry.context.as_ref())
                    .and_then(|context| context.group.as_ref())
                    .map(|group| group.id.clone());
                ActivityBrief {
                    title: group.title(&recent),
                    detail: group.subtitle(&recent),
                    authors: group.authors.clone(),
                    apps,
                    resources,
                    changes: group.len(),
                    first_at: group.first_time,
                    last_at: group.last_time,
                    group_id,
                }
            })
            .collect();
        briefs.sort_by(|a, b| b.last_at.cmp(&a.last_at));
        briefs.truncate(limit);
        Self {
            briefs,
            total_changes: recent.len(),
            people,
        }
    }

    pub fn headline(&self) -> Option<String> {
        self.briefs.first().map(ActivityBrief::glance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use collab_types::{ArtifactKind, OpClass};
    use qos_actions::ActionContext;

    use crate::widget::{HistoryEntryId, history::HistoryAuthor};

    fn entry(id: u8, author: &str, at: u64) -> HistoryEntry {
        let mut bytes = [0; 16];
        bytes[0] = id;
        HistoryEntry {
            id: HistoryEntryId(bytes),
            author: HistoryAuthor::new(author, 120.0, format!("id{id}")),
            op_class: OpClass::Splice,
            artifact_kind: ArtifactKind::File,
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(at),
            summary: "+1 -0 chars".into(),
            text_delta: Some((1, 0)),
            diff_payload: None,
            redaction: None,
            context: Some(
                ActionContext::for_resource(
                    "Edit",
                    "notes.note:brief",
                    "notes.note",
                    "Launch brief",
                )
                .with_app("org.qos.Notes", "Notes")
                .with_group("issue:42", "Prepare launch brief", "issue"),
            ),
        }
    }

    #[test]
    fn catch_up_uses_the_same_explicit_group_across_people() {
        let entries = vec![entry(1, "Ada", 100), entry(2, "Maren", 110)];
        let digest = ActivityDigest::from_entries(
            &entries,
            SystemTime::UNIX_EPOCH + Duration::from_secs(90),
            3,
        );
        assert_eq!(digest.total_changes, 2);
        assert_eq!(digest.people, 2);
        assert_eq!(digest.briefs.len(), 1);
        assert_eq!(digest.briefs[0].title, "Prepare launch brief");
        assert_eq!(
            digest.headline().as_deref(),
            Some("Ada and Maren · Prepare launch brief")
        );
        assert_eq!(digest.briefs[0].apps, vec!["Notes"]);
        assert_eq!(digest.briefs[0].resources, vec!["Launch brief"]);
    }
}
