//! The library as search: the resource's search entries, and the index the
//! timeline filters by.
//!
//! Every photograph, album, timeline month and day, place, folder, person,
//! tag, object and pet is a row found by its Unicode name, dimensions,
//! capture date, location and analysed content. A hit uses the harmless
//! canonical `open` action; choosing one never rotates, crops, copies or
//! saves a picture just because the query matched one of those words.
//!
//! The same rows are the library's search entries
//! ([`library_search_entries`], the app's one search source) and
//! [`LibraryIndex`], which the timeline pages through as you type and which
//! materializes any row's exact resource by id.

use automerge::AutoCommit;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};
use tilo_actions::{ActionAddress, Resource};
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCatalogError,
    SearchCursor, SearchResult,
};
use ui::tilo_resource::SearchEntry;

use crate::actions::{
    PhotoResourceSeed, album_resource_from_fields, albums_resource_from_count, folder_resource,
    folder_resource_id, library_analysis_resource, metadata_label_counts, object_resource,
    object_resource_id, person_resource, person_resource_id, pet_resource, pet_resource_id,
    photo_resource_from_seed, place_counts, place_resource, place_resource_id,
    places_resource_from_count, tag_resource, tag_resource_id, timeline_boundaries,
    timeline_day_resource, timeline_day_resource_id, timeline_month_resource,
    timeline_month_resource_id, timeline_resource_from_search,
};
use crate::doc::PhotosDoc;

/// Search group label for Photos results.
pub const PHOTO_SEARCH_KIND: &str = "Photos";

/// A complete, bounded-query index of the library the timeline filters by,
/// and the compact seeds every row's exact resource materializes from.
///
/// This is built from the document, never from widget state or a filesystem
/// scan. A row's address is exactly the resource address agents,
/// accessibility and collaboration resolve, and a library change makes old
/// targets fail normal fresh-tree resolution.
#[derive(Clone)]
pub struct LibraryIndex {
    state: Arc<RwLock<ProviderState>>,
}

struct ProviderState {
    catalog: SearchCatalog,
    seeds: HashMap<String, MaterializationSeed>,
}

/// Compact authoritative data retained for exact materialisation. Crucially,
/// this is not a `Resource`: no `ActionSpec`, closure, child tree, or encoded
/// action is allocated for the 100k rows that search may index.
#[derive(Clone)]
enum MaterializationSeed {
    Library,
    Timeline {
        total: usize,
        boundaries: crate::actions::TimelineBoundaries,
    },
    TimelineMonth {
        month: String,
        count: usize,
    },
    TimelineDay {
        day: String,
        count: usize,
    },
    Places {
        located_count: usize,
    },
    Albums {
        count: usize,
    },
    Photo(Box<PhotoResourceSeed>),
    Album {
        id: String,
        name: String,
        photo_count: usize,
    },
    Folder {
        label: String,
        count: usize,
    },
    Person {
        label: String,
        count: usize,
    },
    Tag {
        label: String,
        count: usize,
    },
    Object {
        label: String,
        count: usize,
    },
    Pet {
        label: String,
        count: usize,
    },
    Place {
        key: String,
        label: String,
        count: usize,
    },
}

impl MaterializationSeed {
    fn materialize(self) -> Resource {
        match self {
            Self::Library => {
                let mut root = ui::tilo_resource::publish::<crate::resource::Photos>(
                    "photos.library",
                    "Photo library",
                    &crate::resource::PhotosState::default(),
                );
                root.children.insert(0, library_analysis_resource());
                root
            }
            Self::Timeline { total, boundaries } => {
                timeline_resource_from_search(total, &boundaries)
            }
            Self::TimelineMonth { month, count } => timeline_month_resource(&month, count),
            Self::TimelineDay { day, count } => timeline_day_resource(&day, count),
            Self::Places { located_count } => places_resource_from_count(located_count),
            Self::Albums { count } => albums_resource_from_count(count),
            Self::Photo(photo) => photo_resource_from_seed(&photo),
            Self::Album {
                id,
                name,
                photo_count,
            } => album_resource_from_fields(&id, &name, photo_count),
            Self::Folder { label, count } => folder_resource(&label, count),
            Self::Person { label, count } => person_resource(&label, count),
            Self::Tag { label, count } => tag_resource(&label, count),
            Self::Object { label, count } => object_resource(&label, count),
            Self::Pet { label, count } => pet_resource(&label, count),
            Self::Place { key, label, count } => place_resource(&key, &label, count),
        }
    }
}

/// One searchable row of the library.
struct LibraryRow {
    /// What the row is found by, beside its title.
    text: String,
    title: String,
    subtitle: String,
    /// The lucide name of the glyph its hit leads with.
    icon: &'static str,
    action: ActionAddress,
    /// The id a system-wide search opens, for a photograph.
    anchor: Option<String>,
    seed: MaterializationSeed,
}

impl MaterializationSeed {
    /// The id of the effect the row's resource offers to open it: a hit
    /// invokes it under that id.
    fn opened_by(&self) -> &'static str {
        match self {
            Self::Library => "view-library",
            Self::Timeline { .. } | Self::TimelineMonth { .. } | Self::TimelineDay { .. } => {
                "view-timeline"
            }
            Self::Places { .. } => "view-places",
            Self::Albums { .. } => "view-albums",
            Self::Photo(_) => "view",
            Self::Album { .. } => "view-album",
            Self::Folder { .. } => "view-folder",
            Self::Person { .. } => "view-person",
            Self::Tag { .. } => "view-tag",
            Self::Object { .. } => "view-object",
            Self::Pet { .. } => "view-pet",
            Self::Place { .. } => "view-place",
        }
    }
}

impl LibraryRow {
    fn open(id: &str, title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        let seed = MaterializationSeed::Library;
        Self {
            text: String::new(),
            title: title.into(),
            subtitle: subtitle.into(),
            icon: "image",
            action: ActionAddress::new(id, seed.opened_by()),
            anchor: None,
            seed: MaterializationSeed::Library,
        }
    }

    fn found_by(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    fn seeded(mut self, seed: MaterializationSeed) -> Self {
        self.action = ActionAddress::new(self.action.resource.clone(), seed.opened_by());
        self.seed = seed;
        self
    }

    fn result(&self) -> SearchResult {
        SearchResult::for_action(PHOTO_SEARCH_KIND, &self.title, &self.action)
            .with_subtitle(&self.subtitle)
            .with_icon(Icon::from_name(self.icon).unwrap_or(Icon::Image))
    }

    /// The text the index finds the row by: its title first.
    fn searchable(&self) -> String {
        format!("{}\0{}", self.title, self.text)
    }

    fn entry(&self) -> SearchEntry {
        let mut entry = SearchEntry::new(self.title.clone(), self.text.replace('\0', "\n"))
            .invoking(self.action.clone())
            .grouped(PHOTO_SEARCH_KIND)
            .subtitled(self.subtitle.clone())
            .with_icon(self.icon);
        if let Some(anchor) = &self.anchor {
            entry = entry.anchored(anchor.clone());
        }
        entry
    }
}

/// The library's search entries: every row, each hit opening its exact
/// resource. What [`Resource::search`](ui::tilo_resource::Resource::search)
/// answers for the photos library.
pub fn library_search_entries(library: &crate::model::PhotoLibrary) -> Vec<SearchEntry> {
    library_rows(library)
        .values()
        .map(LibraryRow::entry)
        .collect()
}

/// Every searchable row of `library`, keyed by resource id. A hostile or
/// corrupt document can claim one stable id for two kinds; neither ambiguous
/// row is safe to advertise, so both are left out.
#[allow(
    clippy::too_many_lines,
    reason = "one builder keeps every searchable resource kind in a single collision boundary"
)]
fn library_rows(library: &crate::model::PhotoLibrary) -> BTreeMap<String, LibraryRow> {
    let mut rows = BTreeMap::<String, LibraryRow>::new();
    let mut collisions = HashSet::<String>::new();
    let mut insert = |id: String, row: LibraryRow| {
        if collisions.contains(&id) {
            return;
        }
        if rows.insert(id.clone(), row).is_some() {
            rows.remove(&id);
            collisions.insert(id);
        }
    };
    insert(
        "photos.library".to_owned(),
        LibraryRow::open(
            "photos.library",
            "Photo library",
            "Import and browse photographs",
        )
        .found_by("photos.library\0open\0Import photograph"),
    );
    let timeline = timeline_boundaries(library);
    insert(
        "photos.timeline".to_owned(),
        LibraryRow::open(
            "photos.timeline",
            crate::strings::tr("photos-timeline"),
            format!("{} photographs · chronological", library.photos.len()),
        )
        .found_by(format!(
            "Timeline\0photos.timeline\0chronological\0{} photographs\0open",
            library.photos.len()
        ))
        .seeded(MaterializationSeed::Timeline {
            total: library.photos.len(),
            boundaries: timeline.clone(),
        }),
    );
    for (month, count) in &timeline.months {
        let id = timeline_month_resource_id(month);
        insert(
            id.clone(),
            LibraryRow::open(&id, month, format!("Timeline month · {count} photographs"))
                .found_by(format!("timeline\0month\0{count} photographs\0open"))
                .seeded(MaterializationSeed::TimelineMonth {
                    month: month.clone(),
                    count: *count,
                }),
        );
    }
    for (day, count) in &timeline.days {
        let id = timeline_day_resource_id(day);
        insert(
            id.clone(),
            LibraryRow::open(&id, day, format!("Timeline day · {count} photographs"))
                .found_by(format!("timeline\0day\0{count} photographs\0open"))
                .seeded(MaterializationSeed::TimelineDay {
                    day: day.clone(),
                    count: *count,
                }),
        );
    }
    let located_count = library
        .photos
        .iter()
        .filter(|photo| {
            photo
                .location
                .as_ref()
                .is_some_and(crate::model::PhotoLocation::is_valid)
        })
        .count();
    insert(
        "photos.places".to_owned(),
        LibraryRow::open(
            "photos.places",
            "Places",
            format!("Offline map · {located_count} located photographs"),
        )
        .found_by(format!(
            "photos.places\0map\0location\0{located_count} photographs\0open"
        ))
        .seeded(MaterializationSeed::Places { located_count }),
    );
    insert(
        "photos.albums".to_owned(),
        LibraryRow::open(
            "photos.albums",
            "Albums",
            if library.albums.len() == 1 {
                "1 shared album".to_owned()
            } else {
                format!("{} shared albums", library.albums.len())
            },
        )
        .found_by(format!(
            "photos.albums\0collection\0{} shared albums\0open\0browse",
            library.albums.len()
        ))
        .seeded(MaterializationSeed::Albums {
            count: library.albums.len(),
        }),
    );
    for photo in &library.photos {
        // Omit repeated kind/action boilerplate: it creates huge shared
        // postings in a large library without helping find a photograph.
        let mut row = LibraryRow::open(
            &photo.id,
            photo.image.name.clone(),
            format!("{} x {} pixels", photo.image.width, photo.image.height),
        )
        .found_by(format!(
            "{} x {} pixels\0clockwise\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
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
            photo
                .location
                .as_ref()
                .map_or_else(String::new, crate::model::PhotoLocation::display_name),
            photo
                .analysis
                .as_ref()
                .map_or_else(String::new, crate::model::ImageAnalysis::searchable_text),
        ))
        .seeded(MaterializationSeed::Photo(Box::new(
            PhotoResourceSeed::from(photo),
        )));
        row.anchor = Some(photo.id.clone());
        insert(photo.id.clone(), row);
    }
    for album in &library.albums {
        insert(
            album.id.clone(),
            LibraryRow::open(
                &album.id,
                album.name.clone(),
                format!("{} photographs", album.photo_ids.len()),
            )
            .found_by(format!(
                "photos.album\0{} photographs\0open\0remove",
                album.photo_ids.len()
            ))
            .seeded(MaterializationSeed::Album {
                id: album.id.clone(),
                name: album.name.clone(),
                photo_count: album.photo_ids.len(),
            }),
        );
    }
    let (folders, people, tags, objects, pets) = metadata_label_counts(library);
    for (key, label, count) in place_counts(library) {
        let id = place_resource_id(&key);
        insert(
            id.clone(),
            LibraryRow::open(
                &id,
                label.clone(),
                format!("Place · {count} photographs · offline map"),
            )
            .found_by(format!(
                "photos.place\0place\0{count} photographs\0map\0open"
            ))
            .seeded(MaterializationSeed::Place { key, label, count }),
        );
    }
    for (folder, count) in folders {
        let id = folder_resource_id(&folder);
        let mut row =
            LibraryRow::open(&id, folder.clone(), format!("Folder · {count} photographs"))
                .found_by(format!("photos.folder\0folder\0{count} photographs\0open"))
                .seeded(MaterializationSeed::Folder {
                    label: folder,
                    count,
                });
        row.icon = "folder";
        insert(id, row);
    }
    for (person, count) in people {
        let id = person_resource_id(&person);
        insert(
            id.clone(),
            LibraryRow::open(&id, person.clone(), format!("Person · {count} photographs"))
                .found_by(format!("photos.person\0person\0{count} photographs\0open"))
                .seeded(MaterializationSeed::Person {
                    label: person,
                    count,
                }),
        );
    }
    for (tag, count) in tags {
        let id = tag_resource_id(&tag);
        insert(
            id.clone(),
            LibraryRow::open(&id, tag.clone(), format!("Tag · {count} photographs"))
                .found_by(format!("photos.tag\0tag\0{count} photographs\0open"))
                .seeded(MaterializationSeed::Tag { label: tag, count }),
        );
    }
    for (object, count) in objects {
        let id = object_resource_id(&object);
        insert(
            id.clone(),
            LibraryRow::open(&id, object.clone(), format!("Object · {count} photographs"))
                .found_by(format!("photos.object\0object\0{count} photographs\0open"))
                .seeded(MaterializationSeed::Object {
                    label: object,
                    count,
                }),
        );
    }
    for (pet, count) in pets {
        let id = pet_resource_id(&pet);
        insert(
            id.clone(),
            LibraryRow::open(&id, pet.clone(), format!("Pet · {count} photographs"))
                .found_by(format!("photos.pet\0pet\0{count} photographs\0open"))
                .seeded(MaterializationSeed::Pet { label: pet, count }),
        );
    }
    rows
}

impl LibraryIndex {
    /// An empty provider used only before the first document synchronization.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            state: Arc::new(RwLock::new(ProviderState {
                catalog: SearchCatalog::new(PHOTO_SEARCH_KIND),
                seeds: HashMap::new(),
            })),
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
        provider.replace_from_library(&library)?;
        Ok(provider)
    }

    /// Refresh this retained provider to one document generation.
    ///
    /// # Errors
    ///
    /// Returns a catalogue contract error if the replacement snapshot cannot
    /// be admitted; the previous complete generation remains available.
    pub fn sync(&self, library: &crate::model::PhotoLibrary) -> Result<(), SearchCatalogError> {
        self.replace_from_library(library)
    }

    /// Resolve one resource from the retained document generation without a
    /// CRDT walk. The widget uses this for agent/history lookups, including
    /// hashed metadata views whose labels cannot safely be reversed from ids.
    pub fn resource(&self, id: &str) -> Option<Resource> {
        let seed = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .seeds
            .get(id)?
            .clone();
        Some(seed.materialize())
    }

    /// One bounded page of canonical photograph ids matching the same
    /// filename, metadata, OCR and description index used by system search.
    /// The in-app timeline consumes these pages incrementally instead of
    /// rebuilding and scanning a lowercase string for every photo on every
    /// frame.
    pub(crate) fn photo_matches_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
    ) -> (Vec<String>, Option<SearchCursor>, usize) {
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let page = state.catalog.query_page(
            query,
            cursor,
            DEFAULT_SEARCH_CANDIDATE_LIMIT,
            DEFAULT_SEARCH_RESULT_LIMIT,
        );
        let ids = page
            .results
            .iter()
            .filter_map(SearchResult::action_address)
            .filter(|address| {
                matches!(
                    state.seeds.get(&address.resource),
                    Some(MaterializationSeed::Photo(_))
                )
            })
            .map(|address| address.resource)
            .collect();
        drop(state);
        (ids, page.next, page.inspected)
    }

    pub(crate) fn generation(&self) -> u64 {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .stats()
            .generation
    }

    #[cfg(test)]
    fn from_library(
        doc: &AutoCommit,
        library: &crate::model::PhotoLibrary,
    ) -> Result<Self, SearchCatalogError> {
        let provider = Self::empty();
        let _ = doc;
        provider.replace_from_library(library)?;
        Ok(provider)
    }

    fn replace_from_library(
        &self,
        library: &crate::model::PhotoLibrary,
    ) -> Result<(), SearchCatalogError> {
        self.replace_from_library_before_publish(library, || {})
    }

    /// Construct a complete replacement before taking the public snapshot
    /// lock. The hook makes the lock boundary directly regression-testable:
    /// a query must remain able to read the previous generation after all
    /// expensive catalogue work has completed but before publication.
    fn replace_from_library_before_publish<F>(
        &self,
        library: &crate::model::PhotoLibrary,
        before_publish: F,
    ) -> Result<(), SearchCatalogError>
    where
        F: FnOnce(),
    {
        let rows = library_rows(library);
        let seeds = rows
            .iter()
            .map(|(id, row)| (id.clone(), row.seed.clone()))
            .collect();

        // Build postings, grams, and tokens without holding the public
        // snapshot lock. A large import must not turn Ctrl+K or an exact
        // resource lookup into a wait for a 100k-row index rebuild. The
        // short lock below publishes the catalogue and its matching seeds as
        // one generation, so callers never observe fresh hits with stale
        // exact materialisation data.
        let catalog = SearchCatalog::new(PHOTO_SEARCH_KIND);
        catalog.replace_all(
            rows.iter()
                .map(|(id, row)| (id.clone(), row.searchable(), row.result())),
        )?;
        before_publish();
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // `replace_all` gives a new catalogue its own initial generation;
        // align it with the generation immediately following the published
        // one before making either half visible. An exhausted generation
        // leaves the old snapshot intact.
        catalog.advance_after(state.catalog.stats().generation)?;
        state.catalog = catalog;
        state.seeds = seeds;
        drop(state);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> ui::search::SearchCatalogStats {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .stats()
    }

    #[cfg(test)]
    fn retained_seed_stats(&self) -> (usize, usize) {
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The second value is intentionally structural: seeds cannot contain
        // a Resource or ActionSpec, so retained prebuilt action trees are zero.
        (state.seeds.len(), 0)
    }

    #[cfg(test)]
    fn reject_malformed_snapshot_for_test(&self) -> Result<(), SearchCatalogError> {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.catalog.replace_all([(
            "photos.photo:malformed".to_owned(),
            "malformed".to_owned(),
            SearchResult::new(PHOTO_SEARCH_KIND, "Malformed", "legacy-action"),
        )])?;
        state.seeds.clear();
        drop(state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::PhotosResources;
    use crate::doc::PhotosDoc;
    use crate::model::{
        Album, AnalysisProvenance, ImageAnalysis, LibraryPhoto, LocationProvenance, OpenImage,
        PhotoLibrary, PhotoLocation,
    };
    use tilo_actions::ActionAddress;

    use ui::search::SearchProvider;
    use ui::search::testing::assert_late_hit_reachable;

    /// The app's search source over the library `doc` holds, as the
    /// framework builds it from the resource's entries.
    fn source(doc: &AutoCommit) -> impl SearchProvider + use<> {
        source_of(&PhotosDoc::library(doc))
    }

    fn source_of(library: &PhotoLibrary) -> impl SearchProvider + use<> {
        ui::framework::testing::search_source::<crate::resource::Photos>(library_search_entries(
            library,
        ))
    }

    /// The resource a search hit opens, as the library publishes it.
    fn opened(doc: &AutoCommit, address: &ActionAddress) -> Option<Resource> {
        PhotosResources
            .publish_resource(doc, &address.resource)
            .filter(|resource| {
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == address.action)
            })
    }

    fn open_doc(name: &str) -> AutoCommit {
        let mut doc = AutoCommit::new();
        PhotosDoc::put_image(
            &mut doc,
            &OpenImage::from_path(&format!("/pictures/{name}"), 800, 400),
        );
        doc
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn photo_search_is_unicode_safe_and_hits_resolve_to_canonical_resources() {
        let doc = open_doc("München-夏.png");
        let provider = source(&doc);
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
            library_search_entries(&PhotosDoc::library(&doc)).len() >= 2,
            "library and photo are indexed"
        );

        {
            let mut doc = open_doc("harbour.png");
            let mut library = PhotosDoc::library(&doc);
            let photo = library.photos.first_mut().expect("opening imports a photo");
            photo.folder = Some("旅行-夏".into());
            photo.people = vec!["Élodie".into()];
            photo.tags = vec!["golden-hour".into()];
            photo.captured_at = Some("2026-09-07".into());
            PhotosDoc::set_library(&mut doc, &library);

            let provider = source(&doc);
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
                    .publish_resource(&doc, &address.resource)
                    .unwrap_or_else(|| panic!("{query} hit points outside the resource tree"));
                assert!(
                    resource
                        .actions
                        .iter()
                        .any(|action| action.id == address.action)
                );
            }
        }

        {
            let mut doc = open_doc("timeline.png");
            let mut library = PhotosDoc::library(&doc);
            let photo = library.photos.first_mut().expect("opening imports a photo");
            photo.captured_at = Some("2026-09-07 08:14".into());
            PhotosDoc::set_library(&mut doc, &library);

            let provider = source(&doc);
            for (query, expected_resource) in [
                (
                    "2026-09",
                    crate::actions::timeline_month_resource_id("2026-09"),
                ),
                (
                    "2026-09-07",
                    crate::actions::timeline_day_resource_id("2026-09-07"),
                ),
            ] {
                let page = provider.search_page(
                    query,
                    None,
                    DEFAULT_SEARCH_CANDIDATE_LIMIT,
                    DEFAULT_SEARCH_RESULT_LIMIT,
                );
                assert!(page.results.len() <= DEFAULT_SEARCH_RESULT_LIMIT);
                assert!(page.inspected <= DEFAULT_SEARCH_CANDIDATE_LIMIT);
                let address = page
                    .results
                    .iter()
                    .find(|hit| {
                        hit.action_address()
                            .is_some_and(|address| address.resource == expected_resource)
                    })
                    .and_then(SearchResult::action_address)
                    .unwrap_or_else(|| panic!("missing timeline search result for {query}"));
                assert_eq!(address.action, "view-timeline");
                let resource = PhotosResources
                    .publish_resource(&doc, &address.resource)
                    .expect("search must resolve the exact timeline resource");
                assert!(
                    resource
                        .actions
                        .iter()
                        .any(|action| action.id == "view-timeline")
                );
            }
        }

        {
            let mut doc = open_doc("receipt.png");
            let mut library = PhotosDoc::library(&doc);
            let photo = library.photos.first_mut().unwrap();
            let content = photo.image.content.clone().unwrap_or_default();
            photo.analysis = Some(ImageAnalysis {
                ocr_text: "invoice 42 north harbour".into(),
                description: "A dog beside a bicycle".into(),
                objects: vec!["bicycle".into()],
                pets: vec!["dog".into()],
                people: vec!["Sam".into()],
                provenance: AnalysisProvenance {
                    source: "local-ocr".into(),
                    engine: "tilo-ocr".into(),
                    language: "eng".into(),
                    content,
                    scope: "shared".into(),
                    ..AnalysisProvenance::default()
                },
                ..ImageAnalysis::default()
            });
            PhotosDoc::set_library(&mut doc, &library);

            let provider = source(&doc);
            for (query, resource) in [
                ("invoice 42", library.photos[0].id.clone()),
                ("bicycle", library.photos[0].id.clone()),
                ("dog", library.photos[0].id.clone()),
            ] {
                let address = provider
                    .search(query)
                    .into_iter()
                    .find_map(|hit| {
                        hit.action_address()
                            .filter(|address| address.resource == resource)
                    })
                    .unwrap_or_else(|| {
                        panic!("missing canonical intelligence search result for {query}")
                    });
                assert!(opened(&doc, &address).is_some());
            }
            let index = LibraryIndex::new(&doc).unwrap();
            let (ids, next, inspected) = index.photo_matches_page("invoice 42", None);
            assert_eq!(ids, [library.photos[0].id.clone()]);
            assert!(next.is_none());
            assert!(inspected <= DEFAULT_SEARCH_CANDIDATE_LIMIT);
        }
    }

    #[test]
    fn search_pages_and_caller_limits_are_bounded_and_target_exported_actions() {
        let doc = open_doc("harbour.png");
        let provider = source(&doc);
        assert!(provider.search_page("", None, 0, 1).results.is_empty());
        assert!(provider.search_page("", None, 1, 0).results.is_empty());

        let page = provider.search_page("harbour", None, 1, 1);
        assert_eq!(page.inspected, 1);
        assert_eq!(page.results.len(), 1);
        let address = page.results[0].action_address().unwrap();
        let resource = PhotosResources
            .publish_resource(&doc, &address.resource)
            .expect("search address resolves through the canonical provider");
        assert!(
            resource
                .actions
                .iter()
                .any(|action| ActionAddress::new(&resource.id.0, &action.id) == address)
        );

        {
            let doc = open_doc("harbour.png");
            let provider = source(&doc);
            let page = provider.search_page(
                "harbour",
                None,
                DEFAULT_SEARCH_CANDIDATE_LIMIT,
                DEFAULT_SEARCH_RESULT_LIMIT,
            );
            assert!(page.results.len() <= DEFAULT_SEARCH_RESULT_LIMIT);
            assert!(page.inspected <= DEFAULT_SEARCH_CANDIDATE_LIMIT);
            let address = page.results[0].action_address().unwrap();
            let resource = LibraryIndex::new(&doc)
                .unwrap()
                .resource(&address.resource)
                .expect("the library index materializes the hit's canonical resource");
            assert_eq!(resource.id.0, address.resource);
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == address.action)
            );
            assert!(
                opened(&doc, &ActionAddress::new(address.resource, "remove")).is_some(),
                "all actions on the materialized photograph remain agent-addressable"
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn collection_and_late_search_hits_materialize_their_exact_resources() {
        let mut doc = AutoCommit::new();
        let location = PhotoLocation {
            latitude: 48.8566,
            longitude: 2.3522,
            label: Some("Offline Paris".to_owned()),
            provenance: LocationProvenance::default(),
        };
        let mut first = LibraryPhoto::from_image(OpenImage::from_path("/pictures/one.png", 64, 64));
        first.location = Some(location.clone());
        let mut second =
            LibraryPhoto::from_image(OpenImage::from_path("/pictures/two.png", 64, 64));
        second.location = Some(location.clone());
        PhotosDoc::set_library(
            &mut doc,
            &PhotoLibrary {
                photos: vec![first, second],
                albums: Vec::new(),
            },
        );
        let address = source(&doc)
            .search("Offline Paris")
            .into_iter()
            .filter_map(|result| result.action_address())
            .find(|address| address.resource.starts_with("photos.place:"))
            .expect("place is globally searchable");
        assert_eq!(address.resource, place_resource_id(&location.place_key()));
        let resource = LibraryIndex::new(&doc)
            .unwrap()
            .resource(&address.resource)
            .expect("exact search target materializes");
        assert_eq!(resource.kind, "photos.place");
        assert!(
            resource
                .children
                .iter()
                .any(|child| child.label == "2 photographs")
        );

        {
            let mut doc = AutoCommit::new();
            let mut library = PhotoLibrary::default();
            for index in 0..80 {
                library.albums.push(Album {
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
            let hit = source(&doc)
                .search_page("NEEDLE-夏", None, 2, 1)
                .results
                .pop()
                .expect("late album remains searchable outside the contextual snapshot");
            let address = hit.action_address().expect("album hit has an action route");
            assert_eq!(address.resource, "photos.album:79");
            assert_eq!(
                address.action, "view-album",
                "search activation must not mutate membership"
            );
            let resource = PhotosResources
                .publish_resource(&doc, &address.resource)
                .expect("the late stable album id resolves lazily");
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == "view-album")
            );
        }

        {
            let mut doc = AutoCommit::new();
            let library = PhotoLibrary {
                photos: Vec::new(),
                albums: vec![Album {
                    id: "photos.album:journey".into(),
                    name: "Journey".into(),
                    photo_ids: Vec::new(),
                }],
            };
            PhotosDoc::set_library(&mut doc, &library);

            let address = source(&doc)
                .search("shared albums")
                .into_iter()
                .find_map(|hit| {
                    hit.action_address()
                        .filter(|address| address.resource == "photos.albums")
                })
                .expect("album browsing is a global-search destination");
            assert_eq!(address.action, "view-albums");
            let resource = LibraryIndex::new(&doc)
                .unwrap()
                .resource(&address.resource)
                .expect("the compact search seed materializes the album collection");
            assert_eq!(resource.kind, crate::actions::ALBUMS_KIND);
            assert!(
                resource.children.iter().any(|child| {
                    child.id.0 == "photos.albums#count" && child.label == "1 album"
                })
            );
        }

        {
            let mut doc = AutoCommit::new();
            let mut library = PhotoLibrary::default();
            for index in 0..3_000 {
                let name = if index == 2_999 {
                    "旅行-needle-夏.png".to_string()
                } else {
                    format!("photo-{index}.png")
                };
                library
                    .photos
                    .push(LibraryPhoto::from_image(OpenImage::from_path(
                        &format!("/pictures/{name}"),
                        64,
                        64,
                    )));
            }
            PhotosDoc::set_library(&mut doc, &library);
            let page = source(&doc).search_page("NEEDLE-夏", None, 2, 1);
            assert_eq!(page.results.len(), 1);
            assert!(page.inspected <= 2, "query work exceeded its hard budget");
            assert_eq!(page.results[0].title, "旅行-needle-夏.png");
            assert_eq!(page.results[0].action_address().unwrap().action, "view");
        }

        {
            let mut expected_records = 0;
            let mut built = PhotoLibrary::default();
            let (provider, hit) = assert_late_hit_reachable(
                |corpus| {
                    let doc = AutoCommit::new();
                    let mut library = PhotoLibrary::default();
                    for row in corpus {
                        let stem = row.text.replace(' ', "-");
                        let name = if row.is_target {
                            format!("{stem}-夏.png")
                        } else {
                            format!("{stem}.png")
                        };
                        library
                            .photos
                            .push(LibraryPhoto::from_image(OpenImage::from_path(
                                &format!("/pictures/{name}"),
                                1,
                                1,
                            )));
                    }
                    expected_records = library.photos.len() + 4; // library + timeline + places + albums
                    built = library.clone();
                    let _ = doc;
                    source_of(&library)
                },
                3_000,
            );
            let index = LibraryIndex::from_library(&AutoCommit::new(), &built).unwrap();
            assert_eq!(index.stats().records, expected_records);
            let page = provider.search_page("needle-夏", None, 2, 1);
            assert_eq!(page.results, vec![hit.clone()]);
            assert!(page.inspected <= 2);
            let address = hit.action_address().unwrap();
            assert_eq!(address.action, "view");
            assert!(index.resource(&address.resource).is_some());
            let (seeds, prebuilt_action_trees) = index.retained_seed_stats();
            assert_eq!(seeds, expected_records);
            assert_eq!(prebuilt_action_trees, 0);
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn stale_or_colliding_targets_fail_closed_and_rebuilds_publish_atomically() {
        let before = open_doc("before.png");
        let address = source(&before)
            .search("before")
            .pop()
            .and_then(|result| result.action_address())
            .unwrap();
        let after = open_doc("after.png");
        assert!(
            PhotosResources
                .publish_resources(&after)
                .iter()
                .all(|resource| resource.id.0 != address.resource)
        );

        {
            let mut doc = open_doc("before.png");
            let index = LibraryIndex::new(&doc).unwrap();
            let generation = index.generation();
            let address = source(&doc).search("before")[0].action_address().unwrap();

            PhotosDoc::set_library(&mut doc, &PhotoLibrary::default());
            index.sync(&PhotosDoc::library(&doc)).unwrap();
            assert_ne!(index.generation(), generation, "the old pages are stale");
            assert!(index.resource(&address.resource).is_none());
            assert!(opened(&doc, &address).is_none());
        }

        {
            let doc = AutoCommit::new();
            let mut photo =
                LibraryPhoto::from_image(OpenImage::from_path("/pictures/collision.png", 1, 1));
            photo.id = "photos.album:collision".into();
            let library = PhotoLibrary {
                photos: vec![photo],
                albums: vec![Album {
                    id: "photos.album:collision".into(),
                    name: "Collision".into(),
                    photo_ids: Vec::new(),
                }],
            };
            assert!(source_of(&library).search("collision").is_empty());
            assert!(
                LibraryIndex::from_library(&doc, &library)
                    .unwrap()
                    .resource("photos.album:collision")
                    .is_none()
            );
        }

        {
            let doc = open_doc("last-good.png");
            let provider = LibraryIndex::new(&doc).unwrap();
            let before_generation = provider.generation();
            let address = source(&doc).search("last-good")[0]
                .action_address()
                .unwrap();
            let before = provider.resource(&address.resource).unwrap();
            let before_seed_stats = provider.retained_seed_stats();

            assert_eq!(
                provider.reject_malformed_snapshot_for_test(),
                Err(SearchCatalogError::NonCanonicalResult)
            );
            assert_eq!(provider.generation(), before_generation);
            assert_eq!(provider.retained_seed_stats(), before_seed_stats);
            let after = provider.resource(&address.resource).unwrap();
            assert_eq!(after.id, before.id);
            assert_eq!(after.actions.len(), before.actions.len());
        }

        {
            let doc = open_doc("before.png");
            let provider = LibraryIndex::new(&doc).unwrap();
            let before_address = source(&doc)
                .search("before")
                .into_iter()
                .find_map(|hit| hit.action_address())
                .expect("fixture photograph is searchable");

            let mut replacement = PhotosDoc::library(&doc);
            replacement
                .photos
                .push(LibraryPhoto::from_image(OpenImage::from_path(
                    "/pictures/after.png",
                    16,
                    16,
                )));
            let (built_tx, built_rx) = std::sync::mpsc::sync_channel(0);
            let (publish_tx, publish_rx) = std::sync::mpsc::sync_channel(0);
            let replacement_provider = provider.clone();

            std::thread::scope(|scope| {
                scope.spawn(move || {
                    replacement_provider
                        .replace_from_library_before_publish(&replacement, || {
                            built_tx.send(()).expect("test observes completed rebuild");
                            publish_rx.recv().expect("test releases publication");
                        })
                        .expect("valid replacement publishes");
                });
                built_rx.recv().expect("replacement finished off-lock work");

                // This runs exactly in the window after the new postings/seeds
                // exist but before the short atomic swap. If rebuild work ever
                // re-enters the public state lock, either operation deadlocks
                // here instead of continuing against the prior generation.
                assert!(
                    provider
                        .photo_matches_page("before", None)
                        .0
                        .contains(&before_address.resource)
                );
                assert!(provider.resource(&before_address.resource).is_some());
                publish_tx.send(()).expect("release publication");
            });

            assert!(
                !provider.photo_matches_page("after", None).0.is_empty(),
                "the published replacement finds the new photograph"
            );
        }
    }
}
