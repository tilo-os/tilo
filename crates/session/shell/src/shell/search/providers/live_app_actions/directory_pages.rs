//! The app directory pages the live action provider holds, in two kinds
//! kept apart so one never costs the other its place.
//!
//! - A connection's **listing**: its pages for the empty query, which an
//!   enumeration of every action reads. They stay for as long as the
//!   connection does (or until its publication changes), whatever searches
//!   come and go, so a complete listing stays complete.
//! - **Search** pages: those for a query's text. Bounded, and the least
//!   recently used goes first, among search pages only.

use std::cell::Cell;
use std::collections::HashMap;

use tilo_actions::remote::CataloguePage;

use super::DirectoryCacheKey;
use crate::shell::collab_app_broker::{ActionDirectoryRoot, PublishedSurface};
use crate::shell::collab_broker::AppActionBroker;

/// What one connection publishes: its surfaces and its directory root. Its
/// cached directory pages hold while this does not change.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct DirectoryPublication {
    surfaces: Vec<PublishedSurface>,
    root: Option<ActionDirectoryRoot>,
}

/// What each connection publishes now on `broker`: its surfaces, and its
/// directory root if it has one.
pub(super) fn publications(broker: &AppActionBroker) -> HashMap<u64, DirectoryPublication> {
    let mut now: HashMap<u64, DirectoryPublication> = HashMap::new();
    for surface in broker.surfaces() {
        now.entry(surface.peer_id)
            .or_default()
            .surfaces
            .push(surface);
    }
    let mut after = None;
    loop {
        let page = broker.action_directory_root_page(after, 256);
        for root in page.roots {
            let peer = root.peer_id;
            now.entry(peer).or_default().root = Some(root);
        }
        match page.next_after_peer {
            Some(next) if Some(next) != after => after = Some(next),
            _ => break,
        }
    }
    for publication in now.values_mut() {
        publication.surfaces.sort_by(|a, b| {
            (&a.app_id, &a.route.cluster_id, &a.route.record_id).cmp(&(
                &b.app_id,
                &b.route.cluster_id,
                &b.route.record_id,
            ))
        });
    }
    now
}

/// Whether `key` is a page of a connection's listing: the empty query's.
pub(super) fn is_listing(key: &DirectoryCacheKey) -> bool {
    key.2.is_empty()
}

pub(super) struct DirectoryPages {
    listings: HashMap<DirectoryCacheKey, CataloguePage>,
    /// Each search page with when it was last stored or read.
    searches: HashMap<DirectoryCacheKey, (CataloguePage, Cell<u64>)>,
    clock: Cell<u64>,
    search_capacity: usize,
}

impl DirectoryPages {
    pub(super) fn new(search_capacity: usize) -> Self {
        Self {
            listings: HashMap::new(),
            searches: HashMap::new(),
            clock: Cell::new(0),
            search_capacity,
        }
    }

    fn tick(&self) -> u64 {
        let now = self.clock.get().wrapping_add(1);
        self.clock.set(now);
        now
    }

    /// The page at `key`; a search page read now counts as used.
    pub(super) fn get(&self, key: &DirectoryCacheKey) -> Option<&CataloguePage> {
        if is_listing(key) {
            return self.listings.get(key);
        }
        let (page, used) = self.searches.get(key)?;
        used.set(self.tick());
        Some(page)
    }

    pub(super) fn contains_key(&self, key: &DirectoryCacheKey) -> bool {
        if is_listing(key) {
            self.listings.contains_key(key)
        } else {
            self.searches.contains_key(key)
        }
    }

    /// Store `page`. A search page past the bound replaces the least
    /// recently used search page; a listing's page never displaces one, nor
    /// is displaced by one.
    pub(super) fn insert(&mut self, key: DirectoryCacheKey, page: CataloguePage) {
        if is_listing(&key) {
            self.listings.insert(key, page);
            return;
        }
        if !self.searches.contains_key(&key)
            && self.searches.len() >= self.search_capacity
            && let Some(oldest) = self
                .searches
                .iter()
                .min_by_key(|(_, (_, used))| used.get())
                .map(|(key, _)| key.clone())
        {
            self.searches.remove(&oldest);
        }
        let used = Cell::new(self.tick());
        self.searches.insert(key, (page, used));
    }

    pub(super) fn remove(&mut self, key: &DirectoryCacheKey) -> Option<CataloguePage> {
        if is_listing(key) {
            self.listings.remove(key)
        } else {
            self.searches.remove(key).map(|(page, _)| page)
        }
    }

    pub(super) fn retain(
        &mut self,
        mut keep: impl FnMut(&DirectoryCacheKey, &CataloguePage) -> bool,
    ) {
        self.listings.retain(|key, page| keep(key, page));
        self.searches.retain(|key, (page, _)| keep(key, page));
    }

    pub(super) fn clear(&mut self) {
        self.listings.clear();
        self.searches.clear();
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&DirectoryCacheKey, &CataloguePage)> {
        self.listings
            .iter()
            .chain(self.searches.iter().map(|(key, (page, _))| (key, page)))
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.listings.is_empty() && self.searches.is_empty()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.listings.len().saturating_add(self.searches.len())
    }
}
