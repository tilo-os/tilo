//! A compact, deterministic catch-up model over full history.
//!
//! The top bar and the activity pane should not invent separate notions of
//! “what happened”. Both consume [`ActivityDigest`]: the same grouping as the
//! full timeline, reduced to a few truthful briefs. An LLM may rewrite these
//! briefs for tone, but it is never the source of grouping, attribution, or
//! counts.

use crate::time::SystemTime;
use std::collections::HashSet;

use crate::history_source::detail::{contains_opaque_id, human_label};
use crate::history_source::seen::HistorySeenFrontier;
use crate::widget::history::{HistoryAuthor, HistoryEntry, HistoryEntryId};
use crate::widget::history_group::{
    GroupingConfig, HistoryGroup, NamedRange, OpFamily, authors_more, authors_pair, group_entries,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ActivityBriefAction {
    pub author: HistoryAuthor,
    pub summary: String,
    pub timestamp: SystemTime,
}

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
    /// Oldest-first evidence rows for the card's inline expansion. Keeping
    /// these structured avoids forcing Catch up to navigate into Explorer
    /// merely to answer who performed each underlying action.
    pub actions: Vec<ActivityBriefAction>,
    pub first_at: SystemTime,
    pub last_at: SystemTime,
    /// Stable explicit work-group id, when the actions supplied one.
    pub group_id: Option<String>,
    /// Shared record backing a person-corrected work session. Automatic
    /// summaries have no record and therefore no editable boundary.
    pub work_session_id: Option<String>,
    /// Exact evidence backing this brief. Generated prose may cite only this
    /// set, keeping a summary auditable and invalidatable as the feed moves.
    pub entry_ids: Vec<HistoryEntryId>,
}

impl ActivityBrief {
    /// Single line suitable for the passive top-bar glance surface.
    pub fn glance(&self) -> String {
        let who = match self.authors.as_slice() {
            [] => String::new(),
            [author] => author.name.clone(),
            [first, second] => authors_pair(&first.name, &second.name),
            [first, second, rest @ ..] => authors_more(&first.name, &second.name, rest.len()),
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
        Self::from_entries_with_ranges(entries, since, limit, &[])
    }

    /// Build the compact projection using the same person-corrected work
    /// session boundaries as the full history explorer.
    pub fn from_entries_with_ranges(
        entries: &[HistoryEntry],
        since: SystemTime,
        limit: usize,
        named_ranges: &[NamedRange],
    ) -> Self {
        let recent: Vec<HistoryEntry> = entries
            .iter()
            .filter(|entry| entry.timestamp > since)
            .cloned()
            .collect();
        Self::from_recent(&recent, limit, named_ranges)
    }

    /// Causal-id based catch-up. Prefer this for persisted "since you
    /// looked" state; the timestamp constructor remains useful for explicit
    /// date-range summaries.
    pub fn from_unseen(
        entries: &[HistoryEntry],
        frontier: &HistorySeenFrontier,
        limit: usize,
    ) -> Self {
        let recent: Vec<HistoryEntry> = frontier.unseen(entries).into_iter().cloned().collect();
        Self::from_recent(&recent, limit, &[])
    }

    fn from_recent(recent: &[HistoryEntry], limit: usize, named_ranges: &[NamedRange]) -> Self {
        // People are the participants history could name.
        let people = recent
            .iter()
            .filter(|entry| entry.author.name != super::UNKNOWN_AUTHOR_NAME)
            .map(|entry| {
                (
                    entry.author.name.clone(),
                    entry.author.identity_short.clone(),
                )
            })
            .collect::<HashSet<_>>()
            .len();
        let groups = group_entries(recent, named_ranges, &GroupingConfig::default());
        let mut briefs: Vec<ActivityBrief> = groups
            .iter()
            .map(|group| {
                let slice = recent.get(group.start..group.end).unwrap_or(&[]);
                let mut apps = Vec::new();
                let mut resources = Vec::new();
                // Only names a person can read: a resource or app known only
                // by its id is left out rather than shown as a hash.
                for entry in slice {
                    if let Some(label) = entry
                        .context
                        .as_ref()
                        .and_then(|context| context.app.as_ref())
                        .and_then(|app| human_label(&app.label, &app.id))
                        && !apps.iter().any(|known| known == label)
                    {
                        apps.push(label.to_owned());
                    }
                    if let Some(label) = entry
                        .context
                        .as_ref()
                        .and_then(|context| context.resource.as_ref())
                        .and_then(|resource| human_label(&resource.label, resource.id.as_str()))
                        && !resources.iter().any(|known| known == label)
                    {
                        resources.push(label.to_owned());
                    }
                }
                let group_id = slice
                    .first()
                    .and_then(|entry| entry.context.as_ref())
                    .and_then(|context| context.group.as_ref())
                    .map(|group| group.id.clone());
                let mut title = digest_outcome_title(group, recent, &resources);
                if contains_opaque_id(&title) {
                    title = general_outcome_title(&group.families);
                }
                let group_detail = without_opaque_parts(&group.subtitle(recent));
                let detail = if title == group.title(recent) {
                    group_detail
                } else {
                    let count = crate::i18n!("history-group-changes", "count" => group.len());
                    if group_detail.is_empty() {
                        count
                    } else {
                        format!("{count} · {group_detail}")
                    }
                };
                ActivityBrief {
                    title,
                    detail,
                    authors: group.authors.clone(),
                    apps,
                    resources,
                    changes: group.len(),
                    actions: slice
                        .iter()
                        .map(|entry| ActivityBriefAction {
                            author: entry.author.clone(),
                            summary: if contains_opaque_id(&entry.summary) {
                                general_outcome_title(&[OpFamily::of(entry.entry_kind)])
                            } else {
                                entry.summary.clone()
                            },
                            timestamp: entry.timestamp,
                        })
                        .collect(),
                    first_at: group.first_time,
                    last_at: group.last_time,
                    group_id,
                    work_session_id: group.record_id.clone(),
                    entry_ids: slice.iter().map(|entry| entry.id).collect(),
                }
            })
            .collect();
        briefs.sort_by_key(|brief| std::cmp::Reverse(brief.last_at));
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

fn digest_outcome_title(
    group: &HistoryGroup,
    entries: &[HistoryEntry],
    resources: &[String],
) -> String {
    if group.name.is_some() || group.len() <= 1 {
        return group.title(entries);
    }
    let subject = match resources {
        [only] => Some(only.as_str()),
        _ => None,
    };
    let (general, about) = outcome_keys(&group.families);
    match (about, subject) {
        (Some(key), Some(resource)) => crate::i18n!(key, "resource" => resource),
        _ => crate::i18n!(general),
    }
}

/// The outcome title that names no resource, for changes whose own wording
/// would only show identifiers.
fn general_outcome_title(families: &[OpFamily]) -> String {
    crate::i18n!(outcome_keys(families).0)
}

/// A brief's detail line minus the ` · ` parts that carry identifiers.
fn without_opaque_parts(detail: &str) -> String {
    detail
        .split(" \u{00b7} ")
        .filter(|part| !contains_opaque_id(part))
        .collect::<Vec<_>>()
        .join(" \u{00b7} ")
}

fn outcome_keys(families: &[OpFamily]) -> (&'static str, Option<&'static str>) {
    match families {
        [OpFamily::Governance] => ("history-outcome-access", None),
        [OpFamily::Comment] => (
            "history-outcome-discussion",
            Some("history-outcome-discussed"),
        ),
        [OpFamily::Deletion] => (
            "history-outcome-organized-content",
            Some("history-outcome-organized"),
        ),
        [OpFamily::Edit] => (
            "history-outcome-updated-content",
            Some("history-outcome-worked-on"),
        ),
        _ => ("history-outcome-workspace", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Duration;

    use crate::widget::EntryKind;
    use collab_types::ArtifactKind;
    use tilo_actions::ActionContext;

    use crate::widget::{HistoryEntryId, history::HistoryAuthor};

    fn entry(id: u8, author: &str, at: u64) -> HistoryEntry {
        let mut bytes = [0; 16];
        bytes[0] = id;
        HistoryEntry {
            id: HistoryEntryId(bytes),
            author: HistoryAuthor::new(author, 120.0, format!("id{id}")),
            entry_kind: EntryKind::Text,
            artifact_kind: ArtifactKind::File,
            timestamp: SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_secs(at))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            summary: "+1 -0 chars".into(),
            text_delta: Some((1, 0)),
            diff_payload: None,
            redaction: None,
            context: Some(
                ActionContext::for_resource("notes.note:brief", "notes.note", "Launch brief")
                    .with_app("org.tilo.Notes", "Notes")
                    .with_group("issue:42", "Prepare launch brief", "issue"),
            ),
        }
    }

    #[test]
    fn catch_up_names_people_groups_and_outcomes() {
        // phase: catch_up_uses_the_same_explicit_group_across_people
        {
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
            assert_eq!(digest.briefs[0].actions.len(), 2);
            assert_eq!(digest.briefs[0].actions[1].author.name, "Maren");
        }
        // phase: catch_up_names_two_people_and_counts_the_rest
        {
            let entries = vec![
                entry(1, "Xaver", 100),
                entry(2, "Quinten", 101),
                entry(3, "Sarah", 102),
                entry(4, "Ada", 103),
            ];
            let digest = ActivityDigest::from_entries(
                &entries,
                SystemTime::UNIX_EPOCH + Duration::from_secs(90),
                3,
            );
            assert_eq!(
                digest.headline().as_deref(),
                Some("Xaver, Quinten + 2 · Prepare launch brief")
            );
        }
        // phase: a_participant_history_cannot_name_is_not_counted_as_a_person
        {
            let entries = vec![
                entry(1, "Ada", 100),
                entry(2, super::super::UNKNOWN_AUTHOR_NAME, 101),
            ];
            let digest = ActivityDigest::from_entries(
                &entries,
                SystemTime::UNIX_EPOCH + Duration::from_secs(90),
                3,
            );
            assert_eq!(digest.total_changes, 2);
            assert_eq!(digest.people, 1);
        }
        // phase: unnamed_aggregate_titles_describe_outcomes_instead_of_schema_counts
        {
            let mut first = entry(1, "Ada", 100);
            let mut second = entry(2, "Maren", 110);
            // Automatic groups split on author identity; keep both changes in one
            // unnamed group so this exercises the aggregate outcome title.
            second.author = first.author.clone();
            for item in [&mut first, &mut second] {
                item.entry_kind = EntryKind::Access;
                item.text_delta = None;
                if let Some(context) = item.context.as_mut() {
                    context.group = None;
                }
            }
            let digest = ActivityDigest::from_entries(
                &[first, second],
                SystemTime::UNIX_EPOCH + Duration::from_secs(90),
                3,
            );
            assert_eq!(digest.briefs[0].title, "Updated workspace access");
            assert_eq!(digest.briefs[0].changes, 2);
            assert!(
                digest.briefs[0].detail.contains("2 changes"),
                "the precise count remains available beneath the outcome title"
            );
        }
    }

    #[test]
    fn catch_up_sessions_follow_named_boundaries_and_delayed_changes() {
        // phase: unseen_digest_does_not_lose_a_delayed_concurrent_change
        {
            let first = entry(1, "Ada", 100);
            let mut frontier = HistorySeenFrontier::default();
            frontier.mark_seen(std::slice::from_ref(&first));
            let delayed = entry(2, "Maren", 50);
            let digest = ActivityDigest::from_unseen(&[delayed, first], &frontier, 3);
            assert_eq!(digest.total_changes, 1);
            assert_eq!(digest.people, 1);
            assert_eq!(digest.briefs[0].authors[0].name, "Maren");
        }
        // phase: corrected_named_boundaries_redraw_catch_up_sessions_not_action_order
        {
            let mut entries = (1..=4)
                .map(|id| entry(id, "Ada", u64::from(id) * 10))
                .collect::<Vec<_>>();
            for item in &mut entries {
                if let Some(context) = item.context.as_mut() {
                    context.group = None;
                }
            }
            let named = |id: &str, label: &str, start: usize, end: usize| NamedRange {
                id: id.to_string(),
                label: label.to_string(),
                start: entries[start].id,
                end: Some(entries[end].id),
                named_by: None,
                created: SystemTime::UNIX_EPOCH,
            };
            let initial = vec![
                named("finance", "Write Q3 financial report", 0, 1),
                named("onboarding", "Onboard Sarah", 2, 3),
            ];
            let corrected = vec![
                named("finance-v2", "Write Q3 financial report", 0, 0),
                named("onboarding-v2", "Onboard Sarah", 1, 3),
            ];

            let before = ActivityDigest::from_entries_with_ranges(
                &entries,
                SystemTime::UNIX_EPOCH,
                10,
                &initial,
            );
            let after = ActivityDigest::from_entries_with_ranges(
                &entries,
                SystemTime::UNIX_EPOCH,
                10,
                &corrected,
            );
            let changes = |digest: &ActivityDigest, title: &str| {
                digest
                    .briefs
                    .iter()
                    .find(|brief| brief.title == title)
                    .map(|brief| brief.changes)
            };
            assert_eq!(changes(&before, "Write Q3 financial report"), Some(2));
            assert_eq!(changes(&before, "Onboard Sarah"), Some(2));
            assert_eq!(changes(&after, "Write Q3 financial report"), Some(1));
            assert_eq!(changes(&after, "Onboard Sarah"), Some(3));
            let projected_ids = after
                .briefs
                .iter()
                .flat_map(|brief| brief.entry_ids.iter().copied())
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(projected_ids.len(), entries.len());
            assert!(
                entries
                    .iter()
                    .all(|entry| projected_ids.contains(&entry.id))
            );
            assert!(
                after
                    .briefs
                    .iter()
                    .all(|brief| brief.work_session_id.is_some())
            );
        }
    }

    #[test]
    fn catch_up_never_shows_record_ids_as_names() {
        let record = "3f2a9c41d0b7e8aa5c1d";
        let id_only = |id: u8, at: u64, group: Option<&str>| {
            let mut item = entry(id, "Ada", at);
            item.summary = format!("Edited {record}");
            let mut context = ActionContext::for_resource(record, "notes.note", record)
                .with_app("org.tilo.Notes", "org.tilo.Notes");
            if let Some(group) = group {
                context = context.with_group("g", group, "issue");
            }
            item.context = Some(context);
            item
        };
        let mut second = id_only(2, 110, None);
        second.author = id_only(1, 100, None).author;
        for entries in [
            vec![id_only(1, 100, None)],
            vec![id_only(1, 100, None), second],
            vec![id_only(1, 100, Some(record))],
        ] {
            let digest = ActivityDigest::from_entries(&entries, SystemTime::UNIX_EPOCH, 5);
            assert!(!digest.briefs.is_empty(), "the changes stay countable");
            for brief in &digest.briefs {
                let shown = [
                    brief.title.clone(),
                    brief.detail.clone(),
                    brief.glance(),
                    brief.apps.join(" "),
                    brief.resources.join(" "),
                ]
                .into_iter()
                .chain(brief.actions.iter().map(|action| action.summary.clone()))
                .collect::<Vec<_>>();
                for text in shown {
                    assert!(
                        !text.contains(record) && !contains_opaque_id(&text),
                        "an identifier reached the catch-up: {text:?}"
                    );
                }
                assert!(!brief.title.is_empty());
            }
        }
    }

    #[test]
    fn a_german_desktop_reads_its_catch_up_in_german() {
        crate::i18n::I18nManager::install(
            "en-US",
            "de-DE",
            &[
                ("en-US", include_str!("../../locales/en-US.ftl")),
                ("de-DE", include_str!("../../locales/de-DE.ftl")),
            ],
        )
        .unwrap();
        let shared = [entry(1, "Ada", 100), entry(2, "Maren", 110)];
        let digest = ActivityDigest::from_entries(&shared, SystemTime::UNIX_EPOCH, 3);
        assert_eq!(
            digest.headline().as_deref(),
            Some("Ada und Maren · Prepare launch brief")
        );

        let mut first = entry(3, "Ada", 100);
        let mut second = entry(4, "Ada", 110);
        second.author = first.author.clone();
        for item in [&mut first, &mut second] {
            if let Some(context) = item.context.as_mut() {
                context.group = None;
            }
        }
        let digest = ActivityDigest::from_entries(&[first, second], SystemTime::UNIX_EPOCH, 3);
        let brief = &digest.briefs[0];
        assert_eq!(brief.title, "An Launch brief gearbeitet");
        assert_eq!(brief.detail, "2 Änderungen · Ada · +2 −0 Zeichen");

        use crate::widget::activity_timeline::ChipKind;
        assert_eq!(ChipKind::Governance.label(), "Zugriff");
        assert_eq!(EntryKind::Text.label(), "Text und Struktur");
    }
}
