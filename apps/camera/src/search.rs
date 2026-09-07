//! Bounded global-search index for Camera's canonical capture resources.
//!
//! The open resource page is intentionally small, while the retained catalog
//! indexes the complete bounded library. Results point at the exact `open`
//! action exported by the corresponding photo/video resource; there is no
//! parallel search-only navigation path.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use qos_actions::{ActionAddress, Resource};
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

use crate::actions::{GalleryItem, capture_resource, capture_resource_id};

pub const CAMERA_SEARCH_KIND: &str = "Camera captures";

/// A retained index whose generation changes in place. Clones handed to the
/// host share both the catalog and the exact-resource map, so continuation
/// cursors cannot accidentally become current again after a same-size update.
#[derive(Clone)]
pub struct CameraSearchProvider {
    catalog: SearchCatalog,
    resources: Arc<RwLock<HashMap<String, Resource>>>,
}

impl CameraSearchProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            catalog: SearchCatalog::new(CAMERA_SEARCH_KIND),
            resources: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn sync(&self, items: &[GalleryItem]) {
        let mut resources = self
            .resources
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let next: HashSet<String> = items.iter().map(capture_resource_id).collect();
        let stale: Vec<String> = resources
            .keys()
            .filter(|id| !next.contains(*id))
            .cloned()
            .collect();
        for id in stale {
            resources.remove(&id);
            self.catalog.remove(&id);
        }
        for item in items {
            let id = capture_resource_id(item);
            let media_kind = if item.is_video { "Video" } else { "Photo" };
            let result = SearchResult::for_action(
                CAMERA_SEARCH_KIND,
                &item.label,
                ActionAddress::new(&id, "open"),
            )
            .with_subtitle(if item.favorite {
                format!("{media_kind} · Favorite")
            } else {
                media_kind.to_string()
            })
            .with_icon(Icon::Image);
            let searchable = format!(
                "{}\0{}\0{}\0open\0share\0favorite\0delete",
                item.label,
                item.id,
                if item.is_video {
                    "video recording"
                } else {
                    "photo image"
                }
            );
            if self.catalog.upsert(&id, searchable, result).is_ok() {
                resources.insert(id, capture_resource(item));
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

impl Default for CameraSearchProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for CameraSearchProvider {
    fn name(&self) -> &str {
        CAMERA_SEARCH_KIND
    }

    fn action_directory_generation(&self) -> Option<u64> {
        Some(self.catalog.stats().generation)
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.catalog.cursor_is_current(cursor)
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.catalog.search_limited(query, limit)
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.catalog
            .query_page(query, cursor, work_budget, result_limit)
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        let resources = self
            .resources
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let resource = resources.get(address.resource.as_str())?.clone();
        drop(resources);
        resource
            .actions
            .iter()
            .any(|action| action.id == address.action)
            .then_some(resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, label: &str, favorite: bool) -> GalleryItem {
        GalleryItem {
            id: id.into(),
            label: label.into(),
            favorite,
            is_video: false,
        }
    }

    #[test]
    fn unicode_search_is_bounded_and_resolves_the_canonical_open_action() {
        let provider = CameraSearchProvider::new();
        provider.sync(&[
            item("munich.jpg", "München 夏", true),
            item("coast.jpg", "Coast", false),
        ]);
        let hits = provider.search_limited("MÜNCHEN", 1);
        assert_eq!(hits.len(), 1);
        let address = hits[0].action_address().expect("canonical action");
        let resource = provider
            .resolve_action_resource(&address)
            .expect("exact resource");
        assert_eq!(resource.id.as_str(), "camera.photo:munich.jpg");
        assert!(resource.actions.iter().any(|action| action.id == "open"));
    }

    #[test]
    fn syncing_removes_stale_resources_and_invalidates_the_generation() {
        let provider = CameraSearchProvider::new();
        provider.sync(&[item("old.jpg", "Old", false)]);
        let before = provider.stats().generation;
        provider.sync(&[item("new.jpg", "New", false)]);
        assert!(provider.stats().generation > before);
        assert!(provider.search("Old").is_empty());
        assert_eq!(provider.search("New").len(), 1);
    }
}
