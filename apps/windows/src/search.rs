//! Platform search over the installed Windows apps.
//!
//! The catalogue is a live projection of the compatibility service, not
//! document state, so the window hands search a snapshot of its apps and
//! takes a picked hit back in [`WindowsApp`](crate::WindowsApp)'s
//! `activate_search_result`, which shows the app as choosing its row does.

use std::sync::Arc;

use ui::search::{FieldWeight, ListIndex, SearchProvider, SearchResult};

use crate::model::Catalogue;

const KIND: &str = "Windows apps";

/// The anchor a hit for app `id` opens.
pub fn app_anchor(id: &str) -> String {
    format!("windows.app:{id}")
}

/// The app id an anchor points to, if it is one of ours.
pub fn app_from_anchor(anchor: &str) -> Option<&str> {
    anchor.strip_prefix("windows.app:")
}

/// One findable app: what a hit shows and the words it is found by.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    id: String,
    name: String,
    publisher: String,
    route: &'static str,
}

/// The apps as search finds them, rebuilt when the catalogue changes.
#[derive(Clone, Debug, Default)]
pub struct AppsSearch {
    entries: Arc<Vec<Entry>>,
    index: Arc<ListIndex>,
}

impl AppsSearch {
    pub fn new(catalogue: &Catalogue) -> Self {
        let mut search = Self::default();
        search.sync(catalogue);
        search
    }

    /// Take the catalogue's apps. Called on a catalogue refresh, never from
    /// a keypress.
    pub fn sync(&mut self, catalogue: &Catalogue) {
        let entries: Vec<Entry> = catalogue
            .apps
            .iter()
            .map(|app| Entry {
                id: app.id.clone(),
                name: app.name.clone(),
                publisher: app.publisher.clone(),
                route: app.route.label(),
            })
            .collect();
        if *self.entries != entries {
            self.entries = Arc::new(entries);
        }
    }

    pub fn provider(&self) -> AppsSearchProvider {
        AppsSearchProvider {
            entries: Arc::clone(&self.entries),
            index: Arc::clone(&self.index),
        }
    }
}

pub struct AppsSearchProvider {
    entries: Arc<Vec<Entry>>,
    index: Arc<ListIndex>,
}

impl SearchProvider for AppsSearchProvider {
    fn name(&self) -> &str {
        KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let ranks = self.index.ranks(
            query,
            self.entries.iter().map(|entry| {
                [
                    (entry.name.as_str(), FieldWeight::TITLE),
                    (entry.publisher.as_str(), FieldWeight::SUBTITLE),
                    (entry.route, FieldWeight::METADATA),
                ]
            }),
        );
        let mut hits: Vec<SearchResult> = self
            .entries
            .iter()
            .zip(ranks)
            .filter_map(|(entry, rank)| {
                rank.map(|rank| {
                    SearchResult::new(KIND, &entry.name, app_anchor(&entry.id))
                        .with_subtitle(entry.publisher.clone())
                        .with_score(rank)
                })
            })
            .collect();
        hits.sort_by(|left, right| right.score.cmp(&left.score));
        hits
    }
}
