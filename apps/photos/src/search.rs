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
use qos_actions::{ActionAddress, Resource, ResourceProvider};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCatalogError,
    SearchCursor, SearchPage, SearchProvider, SearchResult,
};

use crate::actions::{
    PhotosResources, album_resource, folder_resource, folder_resource_id, metadata_label_counts,
    person_resource, person_resource_id, photo_resource, tag_resource, tag_resource_id,
};
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
    /// Exact canonical materialisation for the same immutable document
    /// snapshot as `catalog`. Search activation never re-enumerates the CRDT
    /// or fabricates a parallel resource shape.
    resources: Arc<RwLock<HashMap<String, Resource>>>,
}

impl PhotosSearchProvider {
    /// An empty provider used only before the first document synchronization.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            catalog: SearchCatalog::new(PHOTO_SEARCH_KIND),
            resources: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Index durable records directly. Resource snapshots are deliberately
    /// contextual, so searching through one would hide late library entries.
    ///
    /// # Errors
    ///
    /// Returns a catalogue contract error if a generated canonical row cannot
    /// be admitted. Ambiguous resource-id collisions are excluded safely.
    pub fn new(doc: &AutoCommit) -> Result<Self, SearchCatalogError> {
        let provider = Self::empty();
        let library = PhotosDoc::library(doc);
        provider.replace_from_library(doc, &library)?;
        Ok(provider)
    }

    /// Refresh this retained provider to one document generation.
    ///
    /// # Errors
    ///
    /// Returns a catalogue contract error if the replacement snapshot cannot
    /// be admitted; the previous complete generation remains available.
    pub fn sync(&self, doc: &AutoCommit) -> Result<(), SearchCatalogError> {
        let library = PhotosDoc::library(doc);
        self.replace_from_library(doc, &library)
    }

    /// Resolve one resource from the retained document generation without a
    /// CRDT walk. The widget uses this for agent/history lookups, including
    /// hashed metadata views whose labels cannot safely be reversed from ids.
    pub fn resource(&self, id: &str) -> Option<Resource> {
        self.resources
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    #[cfg(test)]
    fn from_library(
        doc: &AutoCommit,
        library: &crate::model::PhotoLibrary,
    ) -> Result<Self, SearchCatalogError> {
        let provider = Self::empty();
        provider.replace_from_library(doc, library)?;
        Ok(provider)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one snapshot builder keeps every searchable resource kind in a single collision and admission boundary"
    )]
    fn replace_from_library(
        &self,
        doc: &AutoCommit,
        library: &crate::model::PhotoLibrary,
    ) -> Result<(), SearchCatalogError> {
        let mut entries = BTreeMap::<String, (String, SearchResult, Resource)>::new();
        let mut collisions = HashSet::<String>::new();
        let mut insert =
            |id: String, searchable: String, result: SearchResult, resource: Resource| {
                if collisions.contains(&id) {
                    return;
                }
                if entries
                    .insert(id.clone(), (searchable, result, resource))
                    .is_some()
                {
                    // A hostile/corrupt document can claim the same stable id for
                    // two kinds. Neither ambiguous action is safe to advertise.
                    entries.remove(&id);
                    collisions.insert(id);
                }
            };
        let root = SearchResult::for_action(
            PHOTO_SEARCH_KIND,
            "Photo library",
            ActionAddress::new("photos.library", "open"),
        )
        .with_subtitle("Import and browse photographs")
        .with_icon(Icon::Image);
        if let Some(resource) = PhotosResources.resource(doc, "photos.library") {
            insert(
                "photos.library".to_owned(),
                "Photo library\0photos.library\0open\0Import photograph".to_owned(),
                root,
                resource,
            );
        }
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
            // Omit repeated kind/action boilerplate: it creates huge shared
            // postings in a large library without helping find a photograph.
            let searchable = format!(
                "{}\0{} x {} pixels\0clockwise\0{}\0{}\0{}\0{}\0{}",
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
            insert(
                photo.id.clone(),
                searchable,
                result,
                photo_resource(doc, photo),
            );
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
            insert(album.id.clone(), searchable, result, album_resource(album));
        }
        let (folders, people, tags) = metadata_label_counts(library);
        for (folder, count) in folders {
            let id = folder_resource_id(&folder);
            let result = SearchResult::for_action(
                PHOTO_SEARCH_KIND,
                &folder,
                ActionAddress::new(&id, "open"),
            )
            .with_subtitle(format!("Folder · {count} photographs"))
            .with_icon(Icon::Folder);
            insert(
                id.clone(),
                format!("{folder}\0photos.folder\0folder\0{count} photographs\0open"),
                result,
                folder_resource(&folder, count),
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
            insert(
                id.clone(),
                format!("{person}\0photos.person\0person\0{count} photographs\0open"),
                result,
                person_resource(&person, count),
            );
        }
        for (tag, count) in tags {
            let id = tag_resource_id(&tag);
            let result =
                SearchResult::for_action(PHOTO_SEARCH_KIND, &tag, ActionAddress::new(&id, "open"))
                    .with_subtitle(format!("Tag · {count} photographs"))
                    .with_icon(Icon::Image);
            insert(
                id.clone(),
                format!("{tag}\0photos.tag\0tag\0{count} photographs\0open"),
                result,
                tag_resource(&tag, count),
            );
        }
        self.catalog.replace_all(
            entries
                .iter()
                .map(|(id, (text, result, _))| (id.clone(), text.clone(), result.clone())),
        )?;
        let resources = entries
            .into_iter()
            .map(|(id, (_, _, resource))| (id, resource))
            .collect();
        *self
            .resources
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = resources;
        Ok(())
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
        self.catalog
            .search_limited(query, limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.catalog.query_page(
            query,
            cursor,
            work_budget.min(DEFAULT_SEARCH_CANDIDATE_LIMIT),
            result_limit.min(DEFAULT_SEARCH_RESULT_LIMIT),
        )
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        let resource = self.resource(&address.resource)?;
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
        let provider = PhotosSearchProvider::new(&doc).unwrap();
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
        let provider = PhotosSearchProvider::new(&doc).unwrap();
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
            .unwrap()
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
        let page = PhotosSearchProvider::new(&doc)
            .unwrap()
            .search_page("NEEDLE-夏", None, 2, 1);
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
            .unwrap()
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
    fn metadata_search_hits_resolve_to_their_canonical_resources() {
        let mut doc = open_doc("harbour.png");
        let mut library = PhotosDoc::library(&doc);
        let photo = library.photos.first_mut().expect("opening imports a photo");
        photo.folder = Some("旅行-夏".into());
        photo.people = vec!["Élodie".into()];
        photo.tags = vec!["golden-hour".into()];
        photo.captured_at = Some("2026-09-07".into());
        PhotosDoc::set_library(&mut doc, &library);

        let provider = PhotosSearchProvider::new(&doc).unwrap();
        for (query, expected_resource) in [
            ("旅行-夏", crate::actions::folder_resource_id("旅行-夏")),
            ("ÉLODIE", crate::actions::person_resource_id("Élodie")),
            (
                "golden-hour",
                crate::actions::tag_resource_id("golden-hour"),
            ),
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

    #[test]
    fn caller_limits_are_clamped_and_hits_materialize_the_exact_action_resource() {
        let doc = open_doc("harbour.png");
        let provider = PhotosSearchProvider::new(&doc).unwrap();
        assert!(provider.search_limited("", usize::MAX).len() <= DEFAULT_SEARCH_RESULT_LIMIT);
        let page = provider.search_page("harbour", None, usize::MAX, usize::MAX);
        assert!(page.results.len() <= DEFAULT_SEARCH_RESULT_LIMIT);
        assert!(page.inspected <= DEFAULT_SEARCH_CANDIDATE_LIMIT);
        let address = page.results[0].action_address().unwrap();
        let resource = provider
            .resolve_action_resource(&address)
            .expect("the search snapshot materializes its canonical resource");
        assert_eq!(resource.id.0, address.resource);
        assert!(
            resource
                .actions
                .iter()
                .any(|action| action.id == address.action)
        );
        assert!(
            provider
                .resolve_action_resource(&ActionAddress::new(address.resource, "remove"))
                .is_some(),
            "all actions on the materialized photograph remain agent-addressable"
        );
    }

    #[test]
    fn deletion_invalidates_cursors_and_removes_old_exact_materialization() {
        let mut doc = open_doc("before.png");
        let provider = PhotosSearchProvider::new(&doc).unwrap();
        let first = provider.search_page("", None, 1, 1);
        let cursor = first.next.expect("the library has another browse row");
        let address = provider.search("before")[0].action_address().unwrap();

        PhotosDoc::set_library(&mut doc, &crate::model::PhotoLibrary::default());
        PhotosDoc::set_image(&mut doc, None);
        provider.sync(&doc).unwrap();
        assert!(!provider.action_directory_cursor_is_current(&cursor));
        assert!(provider.resolve_action_resource(&address).is_none());
    }

    #[test]
    fn colliding_resource_ids_are_refused_instead_of_routing_to_the_wrong_kind() {
        let doc = AutoCommit::new();
        let mut photo = crate::model::LibraryPhoto::from_image(OpenImage::from_path(
            "/pictures/collision.png",
            1,
            1,
        ));
        photo.id = "photos.album:collision".into();
        let library = crate::model::PhotoLibrary {
            photos: vec![photo],
            albums: vec![crate::model::Album {
                id: "photos.album:collision".into(),
                name: "Collision".into(),
                photo_ids: Vec::new(),
            }],
        };
        let provider = PhotosSearchProvider::from_library(&doc, &library).unwrap();
        assert!(provider.search("collision").is_empty());
        assert!(
            provider
                .resolve_action_resource(&ActionAddress::new("photos.album:collision", "open"))
                .is_none()
        );
    }

    #[test]
    fn exact_unique_match_beyond_one_hundred_thousand_is_bounded_and_reachable() {
        let doc = AutoCommit::new();
        let mut library = crate::model::PhotoLibrary::default();
        library.photos.reserve(100_001);
        for index in 0..100_001 {
            let name = if index == 100_000 {
                "needle-beyond-100k-夏.png".to_owned()
            } else {
                format!("photo-{index}.png")
            };
            library.photos.push(crate::model::LibraryPhoto::from_image(
                OpenImage::from_path(&format!("/pictures/{name}"), 1, 1),
            ));
        }
        let provider = PhotosSearchProvider::from_library(&doc, &library).unwrap();
        assert_eq!(provider.stats().records, 100_002);
        let page = provider.search_page("needle-beyond-100k-夏", None, 2, 1);
        assert_eq!(page.results.len(), 1);
        assert!(page.inspected <= 2);
        let address = page.results[0].action_address().unwrap();
        assert_eq!(address.action, "open");
        assert!(provider.resolve_action_resource(&address).is_some());
    }
}
