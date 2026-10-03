//! Global-search projection for the photograph currently in Photos.
//!
//! The app edits one addressed photograph at a time, rather than keeping a
//! second, lossy recent-files list. Search therefore indexes the complete
//! exported resource: its Unicode filename,
//! dimensions/content metadata, and every published action. The result itself
//! uses the harmless canonical `view` action; activating a search row never
//! accidentally rotates, crops, copies, or saves a picture just because the
//! query happened to match one of those words.

use automerge::AutoCommit;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use tilo_actions::{ActionAddress, Resource};
use tilo_content::ContentStore;
use ui::icons::{AppImage, AppImageSource, Icon};
use ui::search::{
    CatalogSearchProvider, Facet, SearchCatalog, SearchCatalogError, SearchQuery, SearchResult,
    SearchRow,
};

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

/// A complete, bounded-query catalogue of the currently published library.
///
/// This is built from the canonical resource tree, never from widget state or
/// a filesystem scan. As a result, a result's address is exactly the resource
/// address agents, accessibility, and collaboration resolve, and changing the
/// open photograph makes old search targets fail normal fresh-tree resolution.
#[derive(Clone)]
pub struct PhotosSearchProvider {
    state: Arc<RwLock<ProviderState>>,
}

struct ProviderState {
    catalog: SearchCatalog,
    seeds: HashMap<String, MaterializationSeed>,
    /// The people, places and tags a search can be narrowed to, and whether
    /// a photo holds recognized text or is a favorite.
    facets: Vec<Facet>,
    /// Where photo hits read their thumbnails from; `None` shows hits
    /// without one.
    content: Option<ContentStore>,
}

/// Facet keys of a photo hit.
const FACET_PERSON: &str = "person";
const FACET_PLACE: &str = "place";
const FACET_TAG: &str = "tag";
const FACET_HAS: &str = "has";

/// A photo hit's thumbnail, decoded from the content store the first time
/// the palette paints it and kept for the next frames.
struct PhotoThumbnail {
    key: String,
    image: crate::model::OpenImage,
    content: ContentStore,
    decoded: Mutex<Option<(u32, Option<vello::peniko::ImageData>)>>,
}

impl AppImageSource for PhotoThumbnail {
    fn key(&self) -> &str {
        &self.key
    }

    fn pixels(&self, device_px: u32) -> Option<vello::peniko::ImageData> {
        let mut decoded = self
            .decoded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((size, pixels)) = decoded.as_ref()
            && *size == device_px
        {
            return pixels.clone();
        }
        let pixels = crate::doc::bytes_of(&self.image, &self.content)
            .and_then(|bytes| crate::pixels::decode(&bytes))
            .map(|rgba| {
                crate::pixels::to_peniko(&crate::pixels::thumbnail(&rgba, device_px, device_px))
            });
        *decoded = Some((device_px, pixels.clone()));
        pixels
    }
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

impl PhotosSearchProvider {
    /// An empty provider used only before the first document synchronization.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            state: Arc::new(RwLock::new(ProviderState {
                catalog: SearchCatalog::new(PHOTO_SEARCH_KIND),
                seeds: HashMap::new(),
                facets: Vec::new(),
                content: None,
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
    pub fn sync(&self, doc: &AutoCommit) -> Result<(), SearchCatalogError> {
        let library = PhotosDoc::library(doc);
        self.replace_from_library(&library)
    }

    /// Read photo hit thumbnails from `content`.
    pub fn set_content(&self, content: ContentStore) {
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .content = Some(content);
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
    #[allow(
        clippy::too_many_lines,
        reason = "one snapshot builder keeps every searchable resource kind in a single collision and admission boundary"
    )]
    fn replace_from_library_before_publish<F>(
        &self,
        library: &crate::model::PhotoLibrary,
        before_publish: F,
    ) -> Result<(), SearchCatalogError>
    where
        F: FnOnce(),
    {
        let mut entries = BTreeMap::<String, (String, SearchResult, MaterializationSeed)>::new();
        let mut collisions = HashSet::<String>::new();
        let mut insert =
            |id: String, searchable: String, result: SearchResult, seed: MaterializationSeed| {
                if collisions.contains(&id) {
                    return;
                }
                if entries
                    .insert(id.clone(), (searchable, result, seed))
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
        insert(
            "photos.library".to_owned(),
            "Photo library\0photos.library\0open\0Import photograph".to_owned(),
            root,
            MaterializationSeed::Library,
        );
        let timeline = timeline_boundaries(library);
        let timeline_root = SearchResult::for_action(
            PHOTO_SEARCH_KIND,
            crate::strings::tr("photos-timeline"),
            ActionAddress::new("photos.timeline", "open"),
        )
        .with_subtitle(crate::app::count_photos(library.photos.len()))
        .with_icon(Icon::Image);
        insert(
            "photos.timeline".to_owned(),
            format!(
                "Timeline\0photos.timeline\0chronological\0{} photographs\0open",
                library.photos.len()
            ),
            timeline_root,
            MaterializationSeed::Timeline {
                total: library.photos.len(),
                boundaries: timeline.clone(),
            },
        );
        for (month, count) in &timeline.months {
            let id = timeline_month_resource_id(month);
            let result =
                SearchResult::for_action(PHOTO_SEARCH_KIND, month, ActionAddress::new(&id, "open"))
                    .with_subtitle(crate::app::count_photos(*count))
                    .with_icon(Icon::Image);
            insert(
                id.clone(),
                format!("{month}\0timeline\0month\0{count} photographs\0open"),
                result,
                MaterializationSeed::TimelineMonth {
                    month: month.clone(),
                    count: *count,
                },
            );
        }
        for (day, count) in &timeline.days {
            let id = timeline_day_resource_id(day);
            let result =
                SearchResult::for_action(PHOTO_SEARCH_KIND, day, ActionAddress::new(&id, "open"))
                    .with_subtitle(crate::app::count_photos(*count))
                    .with_icon(Icon::Image);
            insert(
                id.clone(),
                format!("{day}\0timeline\0day\0{count} photographs\0open"),
                result,
                MaterializationSeed::TimelineDay {
                    day: day.clone(),
                    count: *count,
                },
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
        let places = SearchResult::for_action(
            PHOTO_SEARCH_KIND,
            "Places",
            ActionAddress::new("photos.places", "open"),
        )
        .with_subtitle(format!(
            "{} with a place",
            crate::app::count_photos(located_count)
        ))
        .with_icon(Icon::Image);
        insert(
            "photos.places".to_owned(),
            format!("Places\0photos.places\0map\0location\0{located_count} photographs\0open"),
            places,
            MaterializationSeed::Places { located_count },
        );
        let albums = SearchResult::for_action(
            PHOTO_SEARCH_KIND,
            "Albums",
            ActionAddress::new("photos.albums", "open"),
        )
        .with_subtitle(if library.albums.len() == 1 {
            "1 shared album".to_owned()
        } else {
            format!("{} shared albums", library.albums.len())
        })
        .with_icon(Icon::Image);
        insert(
            "photos.albums".to_owned(),
            format!(
                "Albums\0photos.albums\0collection\0{} shared albums\0open\0browse",
                library.albums.len()
            ),
            albums,
            MaterializationSeed::Albums {
                count: library.albums.len(),
            },
        );
        let content = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .content
            .clone();
        let mut facet_people = BTreeSet::new();
        let mut facet_places = BTreeMap::new();
        let mut facet_tags = BTreeSet::new();
        for photo in &library.photos {
            facet_people.extend(photo.people.iter().cloned());
            facet_tags.extend(photo.tags.iter().cloned());
            if let Some(location) = photo
                .location
                .as_ref()
                .filter(|location| location.is_valid())
            {
                facet_places.insert(location.place_key(), location.display_name());
            }
            let byline = photo
                .captured_at
                .as_deref()
                .map(crate::app::date_label)
                .or_else(|| {
                    photo
                        .location
                        .as_ref()
                        .map(crate::model::PhotoLocation::display_name)
                })
                .unwrap_or_else(|| {
                    crate::app::dimensions_label(photo.image.width, photo.image.height)
                });
            let mut row = SearchRow::media(&photo.image.name).byline(byline);
            if let Some(content) = &content {
                row = row.thumbnail(AppImage::new(PhotoThumbnail {
                    key: format!("photos:{}", photo.id),
                    image: photo.image.clone(),
                    content: content.clone(),
                    decoded: Mutex::new(None),
                }));
            }
            let result = SearchResult::for_action(
                PHOTO_SEARCH_KIND,
                &photo.image.name,
                ActionAddress::new(&photo.id, "open"),
            )
            .with_subtitle(format!(
                "{} pixels",
                crate::app::dimensions_label(photo.image.width, photo.image.height)
            ))
            .with_icon(Icon::Image)
            .with_row(row);
            // Omit repeated kind/action boilerplate: it creates huge shared
            // postings in a large library without helping find a photograph.
            let searchable = format!(
                "{}\0{} x {} pixels\0clockwise\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
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
                photo
                    .location
                    .as_ref()
                    .map_or_else(String::new, crate::model::PhotoLocation::display_name),
                photo
                    .analysis
                    .as_ref()
                    .map_or_else(String::new, crate::model::ImageAnalysis::searchable_text),
            );
            insert(
                photo.id.clone(),
                searchable,
                result,
                MaterializationSeed::Photo(Box::new(PhotoResourceSeed::from(photo))),
            );
        }
        for album in &library.albums {
            let result = SearchResult::for_action(
                PHOTO_SEARCH_KIND,
                &album.name,
                ActionAddress::new(&album.id, "open"),
            )
            .with_subtitle(crate::app::count_photos(album.photo_ids.len()))
            .with_icon(Icon::Image);
            let searchable = format!(
                "{}\0photos.album\0{} photographs\0open\0remove",
                album.name,
                album.photo_ids.len()
            );
            insert(
                album.id.clone(),
                searchable,
                result,
                MaterializationSeed::Album {
                    id: album.id.clone(),
                    name: album.name.clone(),
                    photo_count: album.photo_ids.len(),
                },
            );
        }
        let (folders, people, tags, objects, pets) = metadata_label_counts(library);
        for (key, label, count) in place_counts(library) {
            let id = place_resource_id(&key);
            let result = SearchResult::for_action(
                PHOTO_SEARCH_KIND,
                &label,
                ActionAddress::new(&id, "open"),
            )
            .with_subtitle(crate::app::count_photos(count))
            .with_icon(Icon::Image);
            insert(
                id.clone(),
                format!("{label}\0photos.place\0place\0{count} photographs\0map\0open"),
                result,
                MaterializationSeed::Place { key, label, count },
            );
        }
        for (folder, count) in folders {
            let id = folder_resource_id(&folder);
            let result = SearchResult::for_action(
                PHOTO_SEARCH_KIND,
                &folder,
                ActionAddress::new(&id, "open"),
            )
            .with_subtitle(format!("Folder with {}", crate::app::count_photos(count)))
            .with_icon(Icon::Folder);
            insert(
                id.clone(),
                format!("{folder}\0photos.folder\0folder\0{count} photographs\0open"),
                result,
                MaterializationSeed::Folder {
                    label: folder,
                    count,
                },
            );
        }
        for (person, count) in people {
            let id = person_resource_id(&person);
            let result = SearchResult::for_action(
                PHOTO_SEARCH_KIND,
                &person,
                ActionAddress::new(&id, "open"),
            )
            .with_subtitle(crate::app::count_photos(count))
            .with_icon(Icon::Image);
            insert(
                id.clone(),
                format!("{person}\0photos.person\0person\0{count} photographs\0open"),
                result,
                MaterializationSeed::Person {
                    label: person,
                    count,
                },
            );
        }
        for (tag, count) in tags {
            let id = tag_resource_id(&tag);
            let result =
                SearchResult::for_action(PHOTO_SEARCH_KIND, &tag, ActionAddress::new(&id, "open"))
                    .with_subtitle(format!("Tag on {}", crate::app::count_photos(count)))
                    .with_icon(Icon::Image);
            insert(
                id.clone(),
                format!("{tag}\0photos.tag\0tag\0{count} photographs\0open"),
                result,
                MaterializationSeed::Tag { label: tag, count },
            );
        }
        for (object, count) in objects {
            let id = object_resource_id(&object);
            let result = SearchResult::for_action(
                PHOTO_SEARCH_KIND,
                &object,
                ActionAddress::new(&id, "open"),
            )
            .with_subtitle(crate::app::count_photos(count))
            .with_icon(Icon::Image);
            insert(
                id.clone(),
                format!("{object}\0photos.object\0object\0{count} photographs\0open"),
                result,
                MaterializationSeed::Object {
                    label: object,
                    count,
                },
            );
        }
        for (pet, count) in pets {
            let id = pet_resource_id(&pet);
            let result =
                SearchResult::for_action(PHOTO_SEARCH_KIND, &pet, ActionAddress::new(&id, "open"))
                    .with_subtitle(crate::app::count_photos(count))
                    .with_icon(Icon::Image);
            insert(
                id.clone(),
                format!("{pet}\0photos.pet\0pet\0{count} photographs\0open"),
                result,
                MaterializationSeed::Pet { label: pet, count },
            );
        }
        let seeds = entries
            .iter()
            .map(|(id, (_, _, seed))| (id.clone(), seed.clone()))
            .collect();
        let mut facets = Vec::new();
        if !facet_people.is_empty() {
            facets.push(facet_people.iter().fold(
                Facet::new(FACET_PERSON, crate::strings::tr("photos-people")),
                |facet, person| facet.value(person.clone(), person.clone()),
            ));
        }
        if !facet_places.is_empty() {
            facets.push(facet_places.iter().fold(
                Facet::new(FACET_PLACE, crate::strings::tr("photos-places")),
                |facet, (key, label)| facet.value(key.clone(), label.clone()),
            ));
        }
        if !facet_tags.is_empty() {
            facets.push(facet_tags.iter().fold(
                Facet::new(FACET_TAG, crate::strings::tr("photos-tags")),
                |facet, tag| facet.value(tag.clone(), tag.clone()),
            ));
        }
        facets.push(
            Facet::new(FACET_HAS, "Has")
                .value("text", "Text in the photo")
                .value("favorite", crate::strings::tr("photos-favorites")),
        );

        // Build postings, grams, and tokens without holding the public
        // snapshot lock. A large import must not turn Ctrl+K or an exact
        // resource lookup into a wait for a 100k-row index rebuild. The
        // short lock below publishes the catalogue and its matching seeds as
        // one generation, so callers never observe fresh hits with stale
        // exact materialisation data.
        let catalog = SearchCatalog::new(PHOTO_SEARCH_KIND);
        catalog.replace_all(
            entries
                .iter()
                .map(|(id, (text, result, _))| (id.clone(), text.clone(), result.clone())),
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
        state.facets = facets;
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

impl CatalogSearchProvider for PhotosSearchProvider {
    fn provider_name(&self) -> &str {
        PHOTO_SEARCH_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(
            self.state
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .catalog
                .clone(),
        )
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<Resource> {
        let resource = self.resource(&address.resource)?;
        resource
            .actions
            .iter()
            .any(|action| action.id == address.action)
            .then_some(resource)
    }

    fn facets(&self) -> Vec<Facet> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .facets
            .clone()
    }

    /// A narrowed search finds photos: a photo passes when it shows the
    /// chosen people, at the chosen places, with the chosen tags.
    fn admits(&self, hit: &SearchResult, query: &SearchQuery) -> bool {
        if query.filters().is_empty() {
            return true;
        }
        let seed = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .seeds
            .get(&hit.anchor())
            .cloned();
        let Some(MaterializationSeed::Photo(photo)) = seed else {
            return false;
        };
        let people: Vec<&str> = photo.people.iter().map(String::as_str).collect();
        let tags: Vec<&str> = photo.tags.iter().map(String::as_str).collect();
        let place = photo
            .location
            .as_ref()
            .filter(|location| location.is_valid())
            .map(crate::model::PhotoLocation::place_key);
        let places: Vec<&str> = place.iter().map(String::as_str).collect();
        let mut has = Vec::new();
        if photo.favorite {
            has.push("favorite");
        }
        if photo
            .analysis
            .as_ref()
            .is_some_and(|analysis| !analysis.ocr_text.trim().is_empty())
        {
            has.push("text");
        }
        query.admits(FACET_PERSON, &people)
            && query.admits(FACET_PLACE, &places)
            && query.admits(FACET_TAG, &tags)
            && query.admits(FACET_HAS, &has)
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

    use ui::search::testing::assert_late_hit_reachable;
    use ui::search::{DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider};

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

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn photo_search_is_unicode_safe_and_hits_resolve_to_canonical_resources() {
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

        {
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

            let provider = PhotosSearchProvider::new(&doc).unwrap();
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
                let page =
                    provider.search_page(&SearchQuery::new(query), None, usize::MAX, usize::MAX);
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
                assert_eq!(address.action, "open");
                let resource = PhotosResources
                    .publish_resource(&doc, &address.resource)
                    .expect("search must resolve the exact timeline resource");
                assert!(resource.actions.iter().any(|action| action.id == "open"));
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

            let provider = PhotosSearchProvider::new(&doc).unwrap();
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
                assert!(provider.resolve_action_resource(&address).is_some());
            }
        }
    }

    #[test]
    fn search_pages_and_caller_limits_are_bounded_and_target_exported_actions() {
        let doc = open_doc("harbour.png");
        let provider = PhotosSearchProvider::new(&doc).unwrap();
        assert!(
            provider
                .search_page(&SearchQuery::new(""), None, 0, 1)
                .results
                .is_empty()
        );
        assert!(
            provider
                .search_page(&SearchQuery::new(""), None, 1, 0)
                .results
                .is_empty()
        );

        let page = provider.search_page(&SearchQuery::new("harbour"), None, 1, 1);
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
            let provider = PhotosSearchProvider::new(&doc).unwrap();
            assert!(provider.search_limited("", usize::MAX).len() <= DEFAULT_SEARCH_RESULT_LIMIT);
            let page =
                provider.search_page(&SearchQuery::new("harbour"), None, usize::MAX, usize::MAX);
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
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn collection_and_late_search_hits_materialize_their_exact_resources() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
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
        let provider = PhotosSearchProvider::new(&doc).unwrap();
        let address = provider
            .search("Offline Paris")
            .into_iter()
            .filter_map(|result| result.action_address())
            .find(|address| address.resource.starts_with("photos.place:"))
            .expect("place is globally searchable");
        assert_eq!(address.resource, place_resource_id(&location.place_key()));
        let resource = provider
            .resolve_action_resource(&address)
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
            PhotosDoc::seed(&mut doc);
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
            let hit = PhotosSearchProvider::new(&doc)
                .unwrap()
                .search_page(&SearchQuery::new("NEEDLE-夏"), None, 2, 1)
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
                .publish_resource(&doc, &address.resource)
                .expect("the late stable album id resolves lazily");
            assert!(resource.actions.iter().any(|action| action.id == "open"));
        }

        {
            let mut doc = AutoCommit::new();
            PhotosDoc::seed(&mut doc);
            let library = PhotoLibrary {
                photos: Vec::new(),
                albums: vec![Album {
                    id: "photos.album:journey".into(),
                    name: "Journey".into(),
                    photo_ids: Vec::new(),
                }],
            };
            PhotosDoc::set_library(&mut doc, &library);

            let provider = PhotosSearchProvider::new(&doc).unwrap();
            let address = provider
                .search("shared albums")
                .into_iter()
                .find_map(|hit| {
                    hit.action_address()
                        .filter(|address| address.resource == "photos.albums")
                })
                .expect("album browsing is a global-search destination");
            assert_eq!(address.action, "open");
            let resource = provider
                .resolve_action_resource(&address)
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
            PhotosDoc::seed(&mut doc);
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
            let page = PhotosSearchProvider::new(&doc).unwrap().search_page(
                &SearchQuery::new("NEEDLE-夏"),
                None,
                2,
                1,
            );
            assert_eq!(page.results.len(), 1);
            assert!(page.inspected <= 2, "query work exceeded its hard budget");
            assert_eq!(page.results[0].title, "旅行-needle-夏.png");
            assert_eq!(page.results[0].action_address().unwrap().action, "open");
        }

        {
            let mut expected_records = 0;
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
                    PhotosSearchProvider::from_library(&doc, &library).unwrap()
                },
                3_000,
            );
            assert_eq!(provider.stats().records, expected_records);
            let page = provider.search_page(&SearchQuery::new("needle-夏"), None, 2, 1);
            assert_eq!(page.results, vec![hit.clone()]);
            assert!(page.inspected <= 2);
            let address = hit.action_address().unwrap();
            assert_eq!(address.action, "open");
            assert!(provider.resolve_action_resource(&address).is_some());
            let (seeds, prebuilt_action_trees) = provider.retained_seed_stats();
            assert_eq!(seeds, expected_records);
            assert_eq!(prebuilt_action_trees, 0);
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn stale_or_colliding_targets_fail_closed_and_rebuilds_publish_atomically() {
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
                .publish_resources(&after)
                .iter()
                .all(|resource| resource.id.0 != address.resource)
        );

        {
            let mut doc = open_doc("before.png");
            let provider = PhotosSearchProvider::new(&doc).unwrap();
            let first = provider.search_page(&SearchQuery::new(""), None, 1, 1);
            let cursor = first.next.expect("the library has another browse row");
            let address = provider.search("before")[0].action_address().unwrap();

            PhotosDoc::set_library(&mut doc, &PhotoLibrary::default());
            PhotosDoc::set_image(&mut doc, None);
            provider.sync(&doc).unwrap();
            assert!(!provider.action_directory_cursor_is_current(&cursor));
            assert!(provider.resolve_action_resource(&address).is_none());
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
            let provider = PhotosSearchProvider::from_library(&doc, &library).unwrap();
            assert!(provider.search("collision").is_empty());
            assert!(
                provider
                    .resolve_action_resource(&ActionAddress::new("photos.album:collision", "open"))
                    .is_none()
            );
        }

        {
            let doc = open_doc("last-good.png");
            let provider = PhotosSearchProvider::new(&doc).unwrap();
            let before_generation = provider.action_directory_generation();
            let address = provider.search("last-good")[0].action_address().unwrap();
            let before = provider.resource(&address.resource).unwrap();
            let before_seed_stats = provider.retained_seed_stats();

            assert_eq!(
                provider.reject_malformed_snapshot_for_test(),
                Err(SearchCatalogError::NonCanonicalResult)
            );
            assert_eq!(provider.action_directory_generation(), before_generation);
            assert_eq!(provider.retained_seed_stats(), before_seed_stats);
            let after = provider.resource(&address.resource).unwrap();
            assert_eq!(after.id, before.id);
            assert_eq!(after.actions.len(), before.actions.len());
        }

        {
            let doc = open_doc("before.png");
            let provider = PhotosSearchProvider::new(&doc).unwrap();
            let before_address = provider
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
                        .search("before")
                        .iter()
                        .any(|hit| hit.action_address() == Some(before_address.clone()))
                );
                assert!(provider.resource(&before_address.resource).is_some());
                publish_tx.send(()).expect("release publication");
            });

            assert!(provider.search("after").iter().any(|hit| {
                hit.action_address()
                    .is_some_and(|address| address.resource.contains("after.png"))
            }));
        }
    }
}
