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
use qos_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

use crate::actions::{folder_resource_id, person_resource_id};
use crate::doc::PhotosDoc;

/// Search group label for Photos results.
pub const PHOTO_SEARCH_KIND: &str = "Photos";

/// A complete, bounded-query catalogue of the currently published library.
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

    /// Index durable records directly. Resource snapshots are deliberately
    /// contextual, so searching through one would hide late library entries.
    pub fn new(doc: &AutoCommit) -> Self {
        let provider = Self::empty();
        let library = PhotosDoc::library(doc);
        let root = SearchResult::for_action(
            PHOTO_SEARCH_KIND,
            "Photo library",
            ActionAddress::new("photos.library", "open"),
        )
        .with_subtitle("Import and browse photographs")
        .with_icon(Icon::Image);
        let _ = provider.catalog.upsert(
            "photos.library",
            "Photo library\0photos.library\0open\0Import photograph",
            root,
        );
        for photo in &library.photos {
            let result = SearchResult::for_action(
                PHOTO_SEARCH_KIND,
                &photo.image.name,
                ActionAddress::new(&photo.id, "open"),
            )
            .with_subtitle(format!(
                "{} x {} pixels",
                photo.image.width, photo.image.height
            ))
            .with_icon(Icon::Image);
            let searchable = format!(
                "{}\0photos.photo\0{} x {} pixels\0open\0favorite\0clockwise\0{}\0{}\0{}\0{}\0{}",
                photo.image.name,
                photo.image.width,
                photo.image.height,
                if photo.favorite {
                    "favorite"
                } else {
                    "not favorite"
                },
                photo.folder.as_deref().unwrap_or_default(),
                photo.people.join(" "),
                photo.tags.join(" "),
                photo.captured_at.as_deref().unwrap_or_default(),
            );
            let _ = provider
                .catalog
                .upsert(photo.id.clone(), searchable, result);
        }
        for album in &library.albums {
            let result = SearchResult::for_action(
                PHOTO_SEARCH_KIND,
                &album.name,
                ActionAddress::new(&album.id, "open"),
            )
            .with_subtitle(format!("{} photographs", album.photo_ids.len()))
            .with_icon(Icon::Image);
            let searchable = format!(
                "{}\0photos.album\0{} photographs\0open\0remove",
                album.name,
                album.photo_ids.len()
            );
            let _ = provider
                .catalog
                .upsert(album.id.clone(), searchable, result);
        }
        provider.index_metadata_views(&library);
        provider
    }

    /// Folder and person resources are virtual, but their ids and open
    /// actions are still canonical. Index them separately to keep the main
    /// photo/album catalogue construction readable and bounded.
    fn index_metadata_views(&self, library: &crate::model::PhotoLibrary) {
        let mut folders = std::collections::BTreeMap::<String, usize>::new();
        let mut people = std::collections::BTreeMap::<String, usize>::new();
        for photo in &library.photos {
            if let Some(folder) = &photo.folder {
                *folders.entry(folder.clone()).or_default() += 1;
            }
            for person in &photo.people {
                *people.entry(person.clone()).or_default() += 1;
            }
        }
        for (folder, count) in folders {
            let id = folder_resource_id(&folder);
            let result = SearchResult::for_action(
                PHOTO_SEARCH_KIND,
                &folder,
                ActionAddress::new(&id, "open"),
            )
            .with_subtitle(format!("Folder · {count} photographs"))
            .with_icon(Icon::Folder);
            let _ = self.catalog.upsert(
                id,
                format!("{folder}\0photos.folder\0folder\0{count} photographs\0open"),
                result,
            );
        }
        for (person, count) in people {
            let id = person_resource_id(&person);
            let result = SearchResult::for_action(
                PHOTO_SEARCH_KIND,
                &person,
                ActionAddress::new(&id, "open"),
            )
            .with_subtitle(format!("Person · {count} photographs"))
            .with_icon(Icon::Image);
            let _ = self.catalog.upsert(
                id,
                format!("{person}\0photos.person\0person\0{count} photographs\0open"),
                result,
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

impl SearchProvider for PhotosSearchProvider {
    fn name(&self) -> &str {
        PHOTO_SEARCH_KIND
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::PhotosResources;
    use crate::doc::PhotosDoc;
    use crate::model::OpenImage;
    use qos_actions::ActionAddress;
    use qos_actions::ResourceProvider;

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
            assert!(!hits.is_empty(), "query {query:?} did not find the photo");
            assert!(hits.iter().all(|hit| hit.kind == PHOTO_SEARCH_KIND));
            assert!(hits.iter().any(|hit| matches!(
                hit.action_address().unwrap().action.as_str(),
                "view" | "open"
            )));
        }
        assert!(
            provider.stats().records >= 2,
            "library and photo are indexed"
        );
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
        let address = page.results[0].action_address().unwrap();
        let resource = PhotosResources
            .resource(&doc, &address.resource)
            .expect("search address resolves through the canonical provider");
        assert!(
            resource
                .actions
                .iter()
                .any(|action| ActionAddress::new(&resource.id.0, &action.id) == address)
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

    #[test]
    fn a_late_unicode_library_match_is_found_with_a_tiny_query_budget() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..3_000 {
            let name = if index == 2_999 {
                "旅行-needle-夏.png".to_string()
            } else {
                format!("photo-{index}.png")
            };
            library.photos.push(crate::model::LibraryPhoto::from_image(
                OpenImage::from_path(&format!("/pictures/{name}"), 64, 64),
            ));
        }
        PhotosDoc::set_library(&mut doc, &library);
        let page = PhotosSearchProvider::new(&doc).search_page("NEEDLE-夏", None, 2, 1);
        assert_eq!(page.results.len(), 1);
        assert!(page.inspected <= 2, "query work exceeded its hard budget");
        assert_eq!(page.results[0].title, "旅行-needle-夏.png");
        assert_eq!(page.results[0].action_address().unwrap().action, "open");
    }

    #[test]
    fn a_late_album_search_result_opens_the_safe_canonical_resource_action() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..80 {
            library.albums.push(crate::model::Album {
                id: format!("photos.album:{index}"),
                name: if index == 79 {
                    "旅行-needle-夏".into()
                } else {
                    format!("Album {index}")
                },
                photo_ids: Vec::new(),
            });
        }
        PhotosDoc::set_library(&mut doc, &library);
        let hit = PhotosSearchProvider::new(&doc)
            .search_page("NEEDLE-夏", None, 2, 1)
            .results
            .pop()
            .expect("late album remains searchable outside the contextual snapshot");
        let address = hit.action_address().expect("album hit has an action route");
        assert_eq!(address.resource, "photos.album:79");
        assert_eq!(
            address.action, "open",
            "search activation must not mutate membership"
        );
        let resource = PhotosResources
            .resource(&doc, &address.resource)
            .expect("the late stable album id resolves lazily");
        assert!(resource.actions.iter().any(|action| action.id == "open"));
    }

    #[test]
    fn metadata_search_hits_resolve_to_the_same_folder_and_person_resources() {
        let mut doc = open_doc("harbour.png");
        let mut library = PhotosDoc::library(&doc);
        let photo = library.photos.first_mut().expect("opening imports a photo");
        photo.folder = Some("旅行-夏".into());
        photo.people = vec!["Élodie".into()];
        photo.tags = vec!["golden-hour".into()];
        photo.captured_at = Some("2026-09-07".into());
        PhotosDoc::set_library(&mut doc, &library);

        let provider = PhotosSearchProvider::new(&doc);
        for (query, expected_resource) in [
            ("旅行-夏", crate::actions::folder_resource_id("旅行-夏")),
            ("ÉLODIE", crate::actions::person_resource_id("Élodie")),
            ("golden-hour", library.photos[0].id.clone()),
            ("2026-09-07", library.photos[0].id.clone()),
        ] {
            let hit = provider
                .search(query)
                .into_iter()
                .find(|hit| {
                    hit.action_address()
                        .is_some_and(|address| address.resource == expected_resource)
                })
                .unwrap_or_else(|| panic!("missing canonical search hit for {query}"));
            let address = hit.action_address().unwrap();
            let resource = PhotosResources
                .resource(&doc, &address.resource)
                .unwrap_or_else(|| panic!("{query} hit points outside the resource tree"));
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == address.action)
            );
        }
    }
}
