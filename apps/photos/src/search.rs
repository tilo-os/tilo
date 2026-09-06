//! Global-search projection for the photograph currently in Photos.
//!
//! The app deliberately edits one addressed photograph at a time, rather than
//! keeping a second, lossy recent-files list. Search therefore indexes the
//! complete resource that is actually exported now: its Unicode filename,
//! dimensions/content metadata, and every published action. The result itself
//! uses the harmless canonical `view` action; activating a search row never
//! accidentally rotates, crops, copies, or saves a picture just because the
//! query happened to match one of those words.

use automerge::AutoCommit;
use qos_actions::{ActionAddress, ResourceProvider};
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

use crate::actions::{IMAGE_KIND, PhotosResources};

/// Search group label for Photos results.
pub const PHOTO_SEARCH_KIND: &str = "Photos";

/// A complete, bounded-query catalogue of the currently published photo.
///
/// This is built from the canonical resource tree, never from widget state or
/// a filesystem scan. As a result, a result's address is exactly the resource
/// address agents, accessibility, and collaboration resolve, and changing the
/// open photograph makes old search targets fail normal fresh-tree resolution.
#[derive(Clone)]
pub struct PhotosSearchProvider {
    catalog: SearchCatalog,
}

impl PhotosSearchProvider {
    /// An empty provider used only before the first document synchronization.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            catalog: SearchCatalog::new(PHOTO_SEARCH_KIND),
        }
    }

    /// Index every searchable field of the current photo resource.
    pub fn new(doc: &AutoCommit) -> Self {
        let provider = Self::empty();
        let Some(resource) = PhotosResources
            .resources(doc)
            .into_iter()
            .find(|resource| resource.kind == IMAGE_KIND)
        else {
            return provider;
        };
        // `view` is published by PhotosResources alongside the edit/effect
        // actions. Do not manufacture a route if a future resource revision
        // has deliberately withdrawn it.
        if !resource.actions.iter().any(|action| action.id == "view") {
            return provider;
        }
        let mut searchable = format!("{}\0{}", resource.label, resource.kind);
        for child in &resource.children {
            searchable.push('\0');
            searchable.push_str(&child.kind);
            searchable.push('\0');
            searchable.push_str(&child.label);
        }
        for action in &resource.actions {
            searchable.push('\0');
            searchable.push_str(&action.id);
            searchable.push('\0');
            searchable.push_str(&action.label);
            if let Some(description) = &action.description {
                searchable.push('\0');
                searchable.push_str(description);
            }
        }
        let result = SearchResult::for_action(
            PHOTO_SEARCH_KIND,
            &resource.label,
            ActionAddress::new(&resource.id.0, "view"),
        )
        .with_subtitle(
            resource
                .children
                .first()
                .map_or_else(|| "Photograph".to_string(), |child| child.label.clone()),
        )
        .with_icon(Icon::Image);
        // `SearchCatalog` validates that this row's canonical action names
        // the same stable resource being indexed. Its Unicode 1/2/3-gram
        // index and continuation cursor bound each query page.
        let _ = provider.catalog.upsert(resource.id.0, searchable, result);
        provider
    }

    #[cfg(test)]
    fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

impl SearchProvider for PhotosSearchProvider {
    fn name(&self) -> &str {
        PHOTO_SEARCH_KIND
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::PhotosDoc;
    use crate::model::OpenImage;
    use qos_actions::ActionAddress;

    fn open_doc(name: &str) -> AutoCommit {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        PhotosDoc::set_image(
            &mut doc,
            Some(&OpenImage::from_path(
                &format!("/pictures/{name}"),
                800,
                400,
            )),
        );
        doc
    }

    #[test]
    fn search_is_unicode_safe_and_includes_metadata_and_actions() {
        let doc = open_doc("München-夏.png");
        let provider = PhotosSearchProvider::new(&doc);
        for query in ["MÜNCHEN", "夏", "800 x 400", "clockwise"] {
            let hits = provider.search(query);
            assert_eq!(hits.len(), 1, "query {query:?} did not find the photo");
            assert_eq!(hits[0].kind, PHOTO_SEARCH_KIND);
            assert_eq!(hits[0].action_address().unwrap().action, "view");
        }
        assert_eq!(provider.stats().records, 1);
    }

    #[test]
    fn pages_are_bounded_and_the_result_targets_an_exported_action() {
        let doc = open_doc("harbour.png");
        let provider = PhotosSearchProvider::new(&doc);
        assert!(provider.search_page("", None, 0, 1).results.is_empty());
        assert!(provider.search_page("", None, 1, 0).results.is_empty());

        let page = provider.search_page("harbour", None, 1, 1);
        assert_eq!(page.inspected, 1);
        assert_eq!(page.results.len(), 1);
        assert!(page.next.is_none(), "one-record catalogue is exhausted");
        let address = page.results[0].action_address().unwrap();
        let resources = PhotosResources.resources(&doc);
        assert!(
            resources[0]
                .actions
                .iter()
                .any(|action| ActionAddress::new(&resources[0].id.0, &action.id) == address)
        );
    }

    #[test]
    fn a_target_for_the_previous_photo_is_not_exported_after_open_changes() {
        let before = open_doc("before.png");
        let address = PhotosSearchProvider::new(&before)
            .search("before")
            .pop()
            .and_then(|result| result.action_address())
            .unwrap();
        let after = open_doc("after.png");
        assert!(
            PhotosResources
                .resources(&after)
                .iter()
                .all(|resource| resource.id.0 != address.resource)
        );
    }
}
