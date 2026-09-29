//! The entries a host search holds right now, and what changed since the
//! last catalogue walk.

use std::collections::BTreeMap;

use tilo_search::{Document, FieldWeight, Matching, Page};

use crate::entry::{EntryId, SearchEntry};

/// What a host search that keeps its own copy (Spotlight) must change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Entries that are new or show differently.
    pub indexed: Vec<SearchEntry>,
    /// Entries no longer offered: deleted, closed, or no longer permitted.
    pub removed: Vec<EntryId>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.indexed.is_empty() && self.removed.is_empty()
    }
}

/// Every entry the last walk found, and a text index over them for the
/// hosts that ask this runtime to match (GNOME Shell, `KRunner`).
pub struct ResourceIndex {
    entries: BTreeMap<EntryId, SearchEntry>,
    text: tilo_search::SearchIndex,
}

impl Default for ResourceIndex {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            // Names a person half remembers: words across fields and typos.
            text: tilo_search::SearchIndex::new(Matching::LOOSE),
        }
    }
}

impl ResourceIndex {
    /// Make `entries` the whole index, answering what a host copy must change.
    pub fn replace(&mut self, entries: impl IntoIterator<Item = SearchEntry>) -> Changes {
        let mut next = BTreeMap::new();
        for entry in entries {
            // Two live instances of one app offer the same resource once.
            next.entry(entry.id).or_insert(entry);
        }
        let mut changes = Changes::default();
        for id in self.entries.keys() {
            if !next.contains_key(id) {
                changes.removed.push(*id);
                self.text.remove(id.key());
            }
        }
        for (id, entry) in &next {
            if self
                .entries
                .get(id)
                .is_none_or(|held| !held.same_presentation(entry))
            {
                changes.indexed.push(entry.clone());
                self.text.upsert(document(entry));
            }
        }
        self.entries = next;
        if let Err(error) = self.text.commit() {
            tracing::warn!(%error, "could not index the resources for host search");
        }
        changes
    }

    pub fn get(&self, id: EntryId) -> Option<&SearchEntry> {
        self.entries.get(&id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The best `limit` entries for what a person typed, best first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&SearchEntry> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        self.text
            .search(query, Page::first(limit))
            .hits
            .into_iter()
            .filter_map(|hit| self.entries.get(&EntryId::from_key(hit.key)))
            .collect()
    }
}

fn document(entry: &SearchEntry) -> Document {
    let document = Document::new(entry.id.key()).field(entry.title.as_str(), FieldWeight::TITLE);
    match &entry.detail {
        Some(detail) => document.field(detail.as_str(), FieldWeight::SUBTITLE),
        None => document,
    }
}
