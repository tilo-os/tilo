//! What a window's History lists: the workspace's history of the resources the window keeps, as the
//! session's activity feed serves it to every reader. The window names those resources; the feed
//! names who changed them, by the workspace's own rules, and says what each change did. A row
//! carries the change's id in the window's own resource, so choosing it previews that resource.

use crate::remote_actions::{Followed, RemoteActions};
use crate::tilo_actions::remote::workspace_activity as wire;
use crate::tilo_actions::{ActionApp, ActionContext, InvocationUpdate};
use crate::widget::{AuthorRelation, EntryKind, HistoryAuthor, HistoryEntry};
use crate::{DocumentId, HistoryEntryId, NamedRange};
use collab_types::ArtifactKind;

/// The window's History as the feed answers it, kept current while History is open.
pub(super) struct WindowFeed {
    query: wire::FeedQuery,
    followed: Followed,
    /// The answer revision last taken up.
    taken: Option<u64>,
}

/// What the feed listed last: its rows in causal order, the named groups over them, and whether
/// older changes exist.
#[derive(Default)]
pub(super) struct Listed {
    pub(super) entries: Vec<HistoryEntry>,
    pub(super) named: Vec<NamedRange>,
    pub(super) has_earlier: bool,
}

impl WindowFeed {
    /// Follow the history of `resources`, of the app `app`, newest `limit` changes first.
    pub(super) fn follow(app: &str, resources: Vec<DocumentId>, limit: usize) -> Self {
        let query = wire::FeedQuery {
            resources: Some(wire::WindowResources {
                app: app.to_owned(),
                resources: resources.iter().map(|id| id.as_str().to_owned()).collect(),
                item: None,
            }),
            limit,
            ..wire::FeedQuery::default()
        };
        let followed =
            RemoteActions::current().follow(wire::target(wire::FEED), wire::argument(&query));
        Self {
            query,
            followed,
            taken: None,
        }
    }

    /// Whether this follows the history of exactly `resources`, newest `limit` first.
    pub(super) fn reads(&self, resources: &[DocumentId], limit: usize) -> bool {
        self.query.limit == limit
            && self.query.resources.as_ref().is_some_and(|window| {
                window.resources.len() == resources.len()
                    && resources
                        .iter()
                        .all(|id| window.resources.iter().any(|kept| kept == id.as_str()))
            })
    }

    /// Whether the feed answered since its answer was last taken up.
    pub(super) fn has_news(&self) -> bool {
        self.taken != Some(self.followed.call().revision())
    }

    /// The feed's newest answer, once: `None` when it has not answered since.
    pub(super) fn take(&mut self) -> Option<Listed> {
        let revision = self.followed.call().revision();
        if self.taken == Some(revision) {
            return None;
        }
        self.taken = Some(revision);
        // Each new reading is an item; a provider that answers once completes with it.
        let value = self.followed.call().with(|arrival| match arrival.ready() {
            Some(InvocationUpdate::Item { value, .. }) => Some(value.clone()),
            Some(InvocationUpdate::Succeeded { output, .. }) => output.value.clone(),
            _ => None,
        })?;
        let feed: wire::Feed = serde_json::from_value(value).ok()?;
        Some(listed(&feed))
    }
}

fn entry_id(hex: &str) -> Option<HistoryEntryId> {
    wire::entry_id(hex).map(HistoryEntryId::from_bytes)
}

/// `feed` as History lists it: each row under the change's id in its own resource.
fn listed(feed: &wire::Feed) -> Listed {
    let mut own = std::collections::HashMap::new();
    let entries: Vec<HistoryEntry> = feed
        .entries
        .iter()
        .rev()
        .filter_map(|entry| {
            let listed = entry_id(&entry.id)?;
            let id = entry.change.as_deref().and_then(entry_id).unwrap_or(listed);
            own.insert(listed, id);
            Some(row(entry, id))
        })
        .collect();
    let named = feed
        .groups
        .iter()
        .filter_map(|group| {
            Some(NamedRange {
                id: group.id.clone(),
                label: group.label.clone(),
                start: *own.get(&entry_id(&group.start)?)?,
                end: group
                    .end
                    .as_deref()
                    .and_then(entry_id)
                    .and_then(|end| own.get(&end).copied()),
                named_by: group
                    .named_by
                    .as_ref()
                    .map(|name| HistoryAuthor::new(name.clone(), 0.0, String::new())),
                created: crate::time::SystemTime::UNIX_EPOCH,
            })
        })
        .collect();
    Listed {
        has_earlier: feed.total > entries.len(),
        entries,
        named,
    }
}

fn row(entry: &wire::Entry, id: HistoryEntryId) -> HistoryEntry {
    let author = &entry.author;
    let [red, green, blue] = author.rgb;
    let mut by = HistoryAuthor::new(author.name.clone(), 0.0, author.identity.clone())
        .with_rgb((red, green, blue))
        .with_relation(match author.relation {
            wire::Relation::Reader => AuthorRelation::Reader,
            wire::Relation::Person => AuthorRelation::Person,
            wire::Relation::Software => AuthorRelation::Software,
        });
    by.for_reader = author.for_reader;
    by.task.clone_from(&author.task);
    let at = u64::try_from(entry.at_ms).unwrap_or_default();
    HistoryEntry {
        id,
        author: by,
        entry_kind: match entry.kind {
            wire::Kind::Text => EntryKind::Text,
            wire::Kind::Move => EntryKind::Move,
            wire::Kind::Details => EntryKind::Details,
            wire::Kind::Comment => EntryKind::Comment,
            wire::Kind::Delete => EntryKind::Delete,
            wire::Kind::Restore => EntryKind::Restore,
            wire::Kind::Access => EntryKind::Access,
            wire::Kind::Compaction => EntryKind::Compaction,
            wire::Kind::Redaction => EntryKind::Redaction,
        },
        artifact_kind: ArtifactKind::File,
        timestamp: crate::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(at),
        summary: entry.summary.clone(),
        text_delta: None,
        diff_payload: None,
        redaction: None,
        context: entry.app.as_ref().map(|app| ActionContext {
            app: Some(ActionApp {
                id: String::new(),
                label: app.clone(),
            }),
            ..ActionContext::default()
        }),
        target: None,
    }
}

#[cfg(test)]
#[path = "feed_tests.rs"]
pub(crate) mod tests;
