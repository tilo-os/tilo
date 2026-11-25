//! Who made the changes a window's History lists, as the workspace's history names them. The
//! session's activity feed serves the history of the resources the window keeps; History lists the
//! changes its documents hold ([`super::listing`]) and takes from the feed the name, colour and
//! relation of each change's author, and the groups the workspace named.

use crate::remote_actions::{Followed, RemoteActions};
use crate::tilo_actions::InvocationUpdate;
use crate::tilo_actions::remote::workspace_activity as wire;
use crate::widget::{AuthorRelation, HistoryAuthor, HistoryEntry};
use crate::{DocumentId, HistoryEntryId, NamedRange};
use std::collections::HashMap;

/// The workspace's history of the window's resources, kept current while History lists it.
pub(super) struct WindowFeed {
    query: wire::FeedQuery,
    followed: Followed,
    /// The answer revision last taken up.
    taken: Option<u64>,
}

/// What the feed answered last: each change's author, by the change's id in its own resource, and
/// the groups the workspace named over them.
#[derive(Default)]
pub(super) struct Reading {
    authors: HashMap<HistoryEntryId, HistoryAuthor>,
    named: Vec<NamedRange>,
}

impl Reading {
    /// Name each of `entries` by its author as the workspace names them, and add the groups the
    /// workspace named that `named` lacks.
    pub(super) fn apply(&self, entries: &mut [HistoryEntry], named: &mut Vec<NamedRange>) {
        for entry in entries.iter_mut() {
            if let Some(author) = self.authors.get(&entry.id) {
                entry.author = author.clone();
            }
        }
        for range in &self.named {
            if !named.iter().any(|kept| kept.id == range.id) {
                named.push(range.clone());
            }
        }
    }
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
    pub(super) fn take(&mut self) -> Option<Reading> {
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
        Some(read(&feed))
    }
}

fn entry_id(hex: &str) -> Option<HistoryEntryId> {
    wire::entry_id(hex).map(HistoryEntryId::from_bytes)
}

/// `feed` keyed by each change's id in its own resource.
fn read(feed: &wire::Feed) -> Reading {
    let mut own = HashMap::new();
    let mut authors = HashMap::new();
    for entry in &feed.entries {
        let Some(listed) = entry_id(&entry.id) else {
            continue;
        };
        let id = entry.change.as_deref().and_then(entry_id).unwrap_or(listed);
        own.insert(listed, id);
        authors.insert(id, author(&entry.author));
    }
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
    Reading { authors, named }
}

fn author(author: &wire::Author) -> HistoryAuthor {
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
    if let (Some(person), Some(member)) = (
        author
            .on_behalf_of
            .as_ref()
            .filter(|_| author.relation == wire::Relation::Software),
        &author.member,
    ) {
        let mut attribution = crate::widget::history::HistoryAttribution::new(
            crate::tilo_actions::Acting::member(member.clone()),
        );
        attribution.on_behalf_of_name = Some(person.clone());
        by.attribution = Some(attribution);
    }
    by
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod tests;
