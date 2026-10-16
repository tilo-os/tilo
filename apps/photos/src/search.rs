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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};
use tilo_actions::{ActionAddress, Resource};
use ui::icons::Icon;
#[cfg(test)]
use ui::search::{DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchCursor};
use ui::search::{SearchCatalog, SearchCatalogError, SearchResult};
use ui::tilo_resource::SearchEntry;

use crate::derived::{
    folder_resource_id, metadata_label_counts, object_resource_id, person_resource_id,
    pet_resource_id, place_counts, place_resource_id, tag_resource_id, timeline_boundaries,
    timeline_day_resource_id, timeline_month_resource_id,
};
use crate::model::{LibraryPhoto, PhotoLibrary};
use crate::resource::{album_resource_id, derived_resource_id, photo_resource_id};
use ui::lock::RwLockRecover as _;

/// Search group label for Photos results.
pub const PHOTO_SEARCH_KIND: &str = "Photos";

/// The library's own resource id.
pub const LIBRARY_ID: &str = "photos.library";

/// A complete, bounded-query index of the library the timeline filters by,
/// and the library its rows' resources are published from.
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
    rows: HashMap<String, RowKind>,
    library: Arc<PhotoLibrary>,
}

/// What a row is, as the timeline filters them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    Library,
    Timeline,
    Places,
    Albums,
    Photo,
    Album,
    Folder,
    Person,
    Tag,
    Object,
    Pet,
    Place,
}

impl RowKind {
    /// The id of the view the row's resource offers to open it, which the
    /// window's opener runs for a hit anchored at it. A photograph or an
    /// album is opened as the item it is.
    fn opened_by(self) -> &'static str {
        match self {
            Self::Library => "view-library",
            Self::Timeline => "view-timeline",
            Self::Places => "view-places",
            Self::Albums => "view-albums",
            Self::Photo => "view",
            Self::Album => "view-album",
            Self::Folder => "view-folder",
            Self::Person => "view-person",
            Self::Tag => "view-tag",
            Self::Object => "view-object",
            Self::Pet => "view-pet",
            Self::Place => "view-place",
        }
    }

    /// Whether the row is an item of the library, opened by its anchor.
    fn item(self) -> bool {
        matches!(self, Self::Photo | Self::Album)
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
    /// The resource it opens: the anchor of its hit.
    id: String,
    kind: RowKind,
    /// The facets a photograph is filtered by: its people, tags and place.
    facets: Vec<(&'static str, String)>,
}

impl LibraryRow {
    fn open(
        id: &str,
        kind: RowKind,
        title: impl Into<String>,
        subtitle: impl Into<String>,
    ) -> Self {
        Self {
            text: String::new(),
            title: title.into(),
            subtitle: subtitle.into(),
            icon: "image",
            id: id.to_owned(),
            kind,
            facets: Vec::new(),
        }
    }

    fn found_by(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    fn result(&self) -> SearchResult {
        // The index the timeline filters by keeps canonical addresses.
        SearchResult::for_action(
            PHOTO_SEARCH_KIND,
            &self.title,
            ActionAddress::new(&self.id, self.kind.opened_by()),
        )
        .with_subtitle(&self.subtitle)
        .with_icon(Icon::from_name(self.icon).unwrap_or(Icon::Image))
    }

    /// The text the index finds the row by: its title first.
    fn searchable(&self) -> String {
        format!("{}\0{}", self.title, self.text)
    }

    fn entry(&self) -> SearchEntry {
        let mut entry = SearchEntry::new(self.title.clone(), self.text.replace('\0', "\n"))
            .grouped(PHOTO_SEARCH_KIND)
            .subtitled(self.subtitle.clone())
            .with_icon(self.icon);
        // An item opens as the item it is; any other place by its view
        // ([`opened`]).
        entry = entry.anchored(self.id.clone());
        for (facet, value) in &self.facets {
            let group = match *facet {
                "person" => "photos-people",
                "tag" => "photos-tags",
                _ => "photos-place",
            };
            entry = entry.facet(*facet, crate::strings::tr(group), value);
        }
        if self.kind == RowKind::Photo {
            entry.row = Some(
                ui::tilo_resource::SearchRow::shaped(ui::tilo_resource::SearchRowShape::Media {
                    byline: Some(self.subtitle.clone()),
                    duration: None,
                })
                .icon(self.icon),
            );
        }
        entry
    }
}

/// The searchable row of `photo`.
fn photo_row(photo: &LibraryPhoto) -> LibraryRow {
    // Omit repeated kind/action boilerplate: it creates huge shared
    // postings in a large library without helping find a photograph.
    let mut row = LibraryRow::open(
        &photo_resource_id(&photo.id),
        RowKind::Photo,
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
        photo.people().join(" "),
        photo.tags().join(" "),
        photo.captured_at.as_deref().unwrap_or_default(),
        photo
            .location
            .as_ref()
            .map_or_else(String::new, crate::model::PhotoLocation::display_name),
        photo
            .analysis()
            .as_ref()
            .map_or_else(String::new, crate::model::ImageAnalysis::searchable_text),
    ));
    row.facets
        .extend(photo.people.iter().map(|person| ("person", person.clone())));
    row.facets
        .extend(photo.tags.iter().map(|tag| ("tag", tag.clone())));
    if let Some(place) = &photo.location {
        row.facets.push(("place", place.display_name()));
    }
    row
}

/// The view that shows the row anchored at `anchor` of `library`: the one
/// its published place offers. `None` for an item (opened as the item it
/// is) and for an anchor that names no row any more.
pub fn opened(library: &PhotoLibrary, anchor: &str) -> Option<crate::actions::PhotosEffect> {
    use ui::tilo_resource::Effect as _;
    let kind = library_rows(library).get(anchor)?.kind;
    if kind.item() {
        return None;
    }
    let published = if anchor == LIBRARY_ID {
        ui::tilo_resource::publish::<PhotoLibrary>(LIBRARY_ID, "Photos", library)
    } else {
        <PhotoLibrary as ui::tilo_resource::Resource>::child(library, anchor)?
    };
    let spec = published
        .actions
        .iter()
        .find(|action| action.id == kind.opened_by())?;
    let encoded = spec.build_json(&serde_json::Map::new()).ok()?;
    crate::actions::PhotosEffect::from_payload(&encoded.payload).ok()
}

/// The library's search entries: every row, each hit opening its exact
/// resource. What [`Resource::search`](ui::tilo_resource::Resource::search)
/// answers for the photos library.
pub fn library_search_entries(library: &PhotoLibrary) -> Vec<SearchEntry> {
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
fn library_rows(library: &PhotoLibrary) -> BTreeMap<String, LibraryRow> {
    let mut rows = BTreeMap::<String, LibraryRow>::new();
    let mut collisions = HashSet::<String>::new();
    let mut insert = |row: LibraryRow| {
        let id = row.id.clone();
        if collisions.contains(&id) {
            return;
        }
        if rows.insert(id.clone(), row).is_some() {
            rows.remove(&id);
            collisions.insert(id);
        }
    };
    insert(
        LibraryRow::open(
            LIBRARY_ID,
            RowKind::Library,
            "Photo library",
            "Import and browse photographs",
        )
        .found_by("photos.library\0open\0Import photograph"),
    );
    let timeline = timeline_boundaries(library);
    let total = library.photos.len();
    insert(
        LibraryRow::open(
            &derived_resource_id("timeline"),
            RowKind::Timeline,
            crate::strings::tr("photos-timeline"),
            format!("{total} photographs in chronological order"),
        )
        .found_by(format!(
            "Timeline\0photos.timeline\0chronological\0{total} photographs\0open"
        )),
    );
    for (month, count) in &timeline.months {
        insert(
            LibraryRow::open(
                &timeline_month_resource_id(month),
                RowKind::Timeline,
                month,
                format!("{count} photographs in this month"),
            )
            .found_by(format!("timeline\0month\0{count} photographs\0open")),
        );
    }
    for (day, count) in &timeline.days {
        insert(
            LibraryRow::open(
                &timeline_day_resource_id(day),
                RowKind::Timeline,
                day,
                format!("{count} photographs on this day"),
            )
            .found_by(format!("timeline\0day\0{count} photographs\0open")),
        );
    }
    let located_count = library
        .all_photos()
        .filter(|photo| {
            photo
                .location
                .as_ref()
                .is_some_and(crate::model::PhotoLocation::is_valid)
        })
        .count();
    insert(
        LibraryRow::open(
            &derived_resource_id("places"),
            RowKind::Places,
            "Places",
            format!("Offline map with {located_count} located photographs"),
        )
        .found_by(format!(
            "photos.places\0map\0location\0{located_count} photographs\0open"
        )),
    );
    let albums = library.albums.len();
    insert(
        LibraryRow::open(
            &derived_resource_id("albums"),
            RowKind::Albums,
            "Albums",
            if albums == 1 {
                "1 shared album".to_owned()
            } else {
                format!("{albums} shared albums")
            },
        )
        .found_by(format!(
            "photos.albums\0collection\0{albums} shared albums\0open\0browse"
        )),
    );
    for photo in library.all_photos() {
        insert(photo_row(photo));
    }
    for album in library.all_albums() {
        let count = library.album_photos(album).count();
        insert(
            LibraryRow::open(
                &album_resource_id(&album.id),
                RowKind::Album,
                album.name.clone(),
                format!("{count} photographs"),
            )
            .found_by(format!("photos.album\0{count} photographs\0open\0remove")),
        );
    }
    let (folders, people, tags, objects, pets) = metadata_label_counts(library);
    for (key, label, count) in place_counts(library) {
        insert(
            LibraryRow::open(
                &place_resource_id(&key),
                RowKind::Place,
                label,
                format!("{count} photographs at this place, available on the offline map"),
            )
            .found_by(format!(
                "photos.place\0place\0{count} photographs\0map\0open"
            )),
        );
    }
    for (folder, count) in folders {
        let mut row = LibraryRow::open(
            &folder_resource_id(&folder),
            RowKind::Folder,
            folder,
            format!("Folder containing {count} photographs"),
        )
        .found_by(format!("photos.folder\0folder\0{count} photographs\0open"));
        row.icon = "folder";
        insert(row);
    }
    for (person, count) in people {
        insert(
            LibraryRow::open(
                &person_resource_id(&person),
                RowKind::Person,
                person,
                format!("Person identified in {count} photographs"),
            )
            .found_by(format!("photos.person\0person\0{count} photographs\0open")),
        );
    }
    for (tag, count) in tags {
        insert(
            LibraryRow::open(
                &tag_resource_id(&tag),
                RowKind::Tag,
                tag,
                format!("Tag used on {count} photographs"),
            )
            .found_by(format!("photos.tag\0tag\0{count} photographs\0open")),
        );
    }
    for (object, count) in objects {
        insert(
            LibraryRow::open(
                &object_resource_id(&object),
                RowKind::Object,
                object,
                format!("Object identified in {count} photographs"),
            )
            .found_by(format!("photos.object\0object\0{count} photographs\0open")),
        );
    }
    for (pet, count) in pets {
        insert(
            LibraryRow::open(
                &pet_resource_id(&pet),
                RowKind::Pet,
                pet,
                format!("Pet identified in {count} photographs"),
            )
            .found_by(format!("photos.pet\0pet\0{count} photographs\0open")),
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
                rows: HashMap::new(),
                library: Arc::default(),
            })),
        }
    }

    /// Index `library`'s records directly. Resource snapshots are
    /// deliberately contextual, so searching through one would hide late
    /// library entries.
    ///
    /// # Errors
    ///
    /// Returns a catalogue contract error if a generated canonical row cannot
    /// be admitted. Ambiguous resource-id collisions are excluded safely.
    pub fn new(library: &PhotoLibrary) -> Result<Self, SearchCatalogError> {
        let provider = Self::empty();
        provider.replace_from_library(library)?;
        Ok(provider)
    }

    /// Refresh this retained provider to one document generation.
    ///
    /// # Errors
    ///
    /// Returns a catalogue contract error if the replacement snapshot cannot
    /// be admitted; the previous complete generation remains available.
    pub fn sync(&self, library: &PhotoLibrary) -> Result<(), SearchCatalogError> {
        self.replace_from_library(library)
    }

    /// The resource `id` as the library publishes it, from the retained
    /// generation: what the widget resolves for agent and history lookups.
    pub fn resource(&self, id: &str) -> Option<Resource> {
        let state = self.state.read_recover();
        state.rows.get(id)?;
        if id == LIBRARY_ID {
            return Some(ui::tilo_resource::publish::<PhotoLibrary>(
                LIBRARY_ID,
                &crate::strings::tr("photos-act-photo-library"),
                &state.library,
            ));
        }
        <PhotoLibrary as ui::tilo_resource::Resource>::child(&state.library, id)
    }

    #[cfg(test)]
    pub(crate) fn photo_matches_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
    ) -> (Vec<String>, Option<SearchCursor>, usize) {
        let state = self.state.read_recover();
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
            .filter(|address| state.rows.get(&address.resource) == Some(&RowKind::Photo))
            .map(|address| address.resource)
            .collect();
        drop(state);
        (ids, page.next, page.inspected)
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.state.read_recover().catalog.stats().generation
    }

    fn replace_from_library(&self, library: &PhotoLibrary) -> Result<(), SearchCatalogError> {
        self.replace_from_library_before_publish(library, || {})
    }

    /// Build the library's rows before taking the public snapshot lock,
    /// then publish them with the library they came from as one generation.
    /// The catalogue indexes again only the rows that changed, so one
    /// photo's edit or analysis result costs its own rows, not a rebuild of
    /// every row the library has. The hook makes the lock boundary directly
    /// regression-testable: a query must remain able to read the previous
    /// generation after the rows are built but before publication.
    fn replace_from_library_before_publish<F>(
        &self,
        library: &PhotoLibrary,
        before_publish: F,
    ) -> Result<(), SearchCatalogError>
    where
        F: FnOnce(),
    {
        let rows = library_rows(library);
        let kinds = rows
            .iter()
            .map(|(id, row)| (id.clone(), row.kind))
            .collect();
        let library = Arc::new(library.clone());
        before_publish();
        let mut state = self.state.write_recover();
        // Callers never observe fresh hits with stale resources: the rows
        // and their library change under one lock. A refused snapshot
        // leaves the catalogue, and so the old snapshot, intact.
        state.catalog.sync(
            rows.iter()
                .map(|(id, row)| (id.clone(), row.searchable(), row.result())),
        )?;
        state.rows = kinds;
        state.library = library;
        drop(state);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> ui::search::SearchCatalogStats {
        self.state.read_recover().catalog.stats()
    }

    #[cfg(test)]
    fn reject_malformed_snapshot_for_test(&self) -> Result<(), SearchCatalogError> {
        let mut state = self.state.write_recover();
        state.catalog.replace_all([(
            "photos.photo:malformed".to_owned(),
            "malformed".to_owned(),
            SearchResult::new(PHOTO_SEARCH_KIND, "Malformed", "legacy-action"),
        )])?;
        state.rows.clear();
        drop(state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Album, AnalysisProvenance, ImageAnalysis, LibraryPhoto, LocationProvenance, OpenImage,
        PhotoLibrary, PhotoLocation,
    };
    use automerge::AutoCommit;
    use ui::search::SearchQuery;

    use ui::search::testing::assert_late_hit_reachable;
    use ui::search::{DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider};

    /// The app's search source over the library `doc` holds, as the
    /// framework builds it from the resource's entries.
    fn source(doc: &AutoCommit) -> impl SearchProvider + use<> {
        source_of(&crate::test_doc::library(doc))
    }

    fn source_of(library: &PhotoLibrary) -> impl SearchProvider + use<> {
        ui::framework::testing::search_source::<PhotoLibrary>(library_search_entries(library))
    }

    /// Where a hit goes: the resource it is anchored at. The view that
    /// shows it is the window's opener's ([`super::opened`]).
    fn target(hit: &SearchResult) -> (String, Option<String>) {
        (hit.action_id.clone(), None)
    }

    /// The view the window's opener runs for a hit anchored at `anchor`.
    fn view_of(doc: &AutoCommit, anchor: &str) -> Option<String> {
        use ui::tilo_resource::Effect as _;
        super::opened(&crate::test_doc::library(doc), anchor).map(|effect| effect.id().to_owned())
    }

    /// The resource `id` as the library `doc` holds publishes it.
    fn published(doc: &AutoCommit, id: &str) -> Option<Resource> {
        let library = crate::test_doc::library(doc);
        if id == LIBRARY_ID {
            return Some(ui::tilo_resource::publish::<PhotoLibrary>(
                LIBRARY_ID, "Photos", &library,
            ));
        }
        <PhotoLibrary as ui::tilo_resource::Resource>::child(&library, id)
    }

    /// The resource a search hit opens, as the library publishes it, when
    /// the window can show it: an item as itself, any other place through
    /// the view it offers.
    fn opened(doc: &AutoCommit, (resource, _): &(String, Option<String>)) -> Option<Resource> {
        let library = crate::test_doc::library(doc);
        let item = library_rows(&library).get(resource)?.kind.item();
        let published = published(doc, resource)?;
        (item || super::opened(&library, resource).is_some()).then_some(published)
    }

    fn open_doc(name: &str) -> AutoCommit {
        let mut doc = crate::test_doc::new_doc();
        crate::test_doc::put_image(
            &mut doc,
            &OpenImage::fixture(&format!("/pictures/{name}"), 800, 400),
        );
        doc
    }

    fn photo_of(name: &str, side: u32) -> LibraryPhoto {
        LibraryPhoto::from_image(OpenImage::fixture(&format!("/pictures/{name}"), side, side))
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn photo_search_is_unicode_safe_and_hits_resolve_to_canonical_resources() {
        let doc = open_doc("München-夏.png");
        let provider = source(&doc);
        let photo = crate::test_doc::library(&doc)
            .all_photos()
            .next()
            .unwrap()
            .id
            .clone();
        for query in ["MÜNCHEN", "夏", "800 x 400", "clockwise"] {
            let hits = provider.search(query);
            assert!(!hits.is_empty(), "query {query:?} did not find the photo");
            assert!(hits.iter().all(|hit| hit.kind == PHOTO_SEARCH_KIND));
            assert!(
                hits.iter()
                    .any(|hit| target(hit) == (crate::resource::photo_resource_id(&photo), None)),
                "a photograph's hit opens the photograph"
            );
        }
        assert!(
            library_search_entries(&crate::test_doc::library(&doc)).len() >= 2,
            "library and photo are indexed"
        );

        {
            let mut doc = open_doc("harbour.png");
            let mut library = crate::test_doc::library(&doc);
            let (_, photo) = library
                .photos
                .iter_mut()
                .next()
                .expect("opening imports a photo");
            photo.folder = Some("旅行-夏".into());
            photo.people = crate::model::labels(["Élodie"]);
            photo.tags = crate::model::labels(["golden-hour"]);
            photo.captured_at = Some("2026-09-07".into());
            let id = photo.id.clone();
            crate::test_doc::set_library(&mut doc, &library);

            let provider = source(&doc);
            for (query, expected_resource) in [
                ("旅行-夏", crate::derived::folder_resource_id("旅行-夏")),
                ("ÉLODIE", crate::derived::person_resource_id("Élodie")),
                (
                    "golden-hour",
                    crate::derived::tag_resource_id("golden-hour"),
                ),
                ("2026-09-07", crate::resource::photo_resource_id(&id)),
            ] {
                let hit = provider
                    .search(query)
                    .into_iter()
                    .map(|hit| target(&hit))
                    .find(|(resource, _)| *resource == expected_resource)
                    .unwrap_or_else(|| panic!("missing canonical search hit for {query}"));
                assert!(
                    opened(&doc, &hit).is_some(),
                    "{query} hit points outside the resource tree"
                );
            }
        }

        {
            let mut doc = open_doc("timeline.png");
            let mut library = crate::test_doc::library(&doc);
            let (_, photo) = library
                .photos
                .iter_mut()
                .next()
                .expect("opening imports a photo");
            photo.captured_at = Some("2026-09-07T08:14".into());
            crate::test_doc::set_library(&mut doc, &library);

            let provider = source(&doc);
            for (query, expected_resource) in [
                (
                    "2026-09",
                    crate::derived::timeline_month_resource_id("2026-09"),
                ),
                (
                    "2026-09-07",
                    crate::derived::timeline_day_resource_id("2026-09-07"),
                ),
            ] {
                let page = provider.search_page(
                    &SearchQuery::new(query),
                    None,
                    DEFAULT_SEARCH_CANDIDATE_LIMIT,
                    DEFAULT_SEARCH_RESULT_LIMIT,
                );
                assert!(page.results.len() <= DEFAULT_SEARCH_RESULT_LIMIT);
                assert!(page.inspected <= DEFAULT_SEARCH_CANDIDATE_LIMIT);
                let hit = page
                    .results
                    .iter()
                    .map(target)
                    .find(|(resource, _)| *resource == expected_resource)
                    .unwrap_or_else(|| panic!("missing timeline search result for {query}"));
                assert_eq!(view_of(&doc, &hit.0).as_deref(), Some("view-timeline"));
                assert!(
                    opened(&doc, &hit).is_some(),
                    "search must resolve the exact timeline resource"
                );
            }
        }

        {
            let mut doc = open_doc("receipt.png");
            let mut library = crate::test_doc::library(&doc);
            let (_, photo) = library.photos.iter_mut().next().unwrap();
            let content = photo.image.content.clone();
            photo.set_analysis(Some(ImageAnalysis {
                ocr_text: "invoice 42 north harbour".into(),
                description: "A dog beside a bicycle".into(),
                objects: vec!["bicycle".into()],
                pets: vec!["dog".into()],
                provenance: AnalysisProvenance {
                    source: "local-ocr".into(),
                    engine: "tilo-ocr".into(),
                    language: "eng".into(),
                    content,
                    scope: "shared".into(),
                    ..AnalysisProvenance::default()
                },
                ..ImageAnalysis::default()
            }));
            let id = photo.id.clone();
            crate::test_doc::set_library(&mut doc, &library);

            let provider = source(&doc);
            let resource = crate::resource::photo_resource_id(&id);
            for query in ["invoice 42", "bicycle", "dog"] {
                let hit = provider
                    .search(query)
                    .into_iter()
                    .map(|hit| target(&hit))
                    .find(|(found, _)| *found == resource)
                    .unwrap_or_else(|| {
                        panic!("missing canonical intelligence search result for {query}")
                    });
                assert!(opened(&doc, &hit).is_some());
            }
            let index = LibraryIndex::new(&library).unwrap();
            let (ids, next, inspected) = index.photo_matches_page("invoice 42", None);
            assert_eq!(ids, [resource]);
            assert!(next.is_none());
            assert!(inspected <= DEFAULT_SEARCH_CANDIDATE_LIMIT);
        }
    }

    #[test]
    fn search_pages_and_caller_limits_are_bounded_and_target_exported_actions() {
        let doc = open_doc("harbour.png");
        let provider = source(&doc);
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
        let hit = target(&page.results[0]);
        assert!(
            opened(&doc, &hit).is_some(),
            "search address resolves through the canonical publication"
        );

        {
            let doc = open_doc("harbour.png");
            let provider = source(&doc);
            let page = provider.search_page(
                &SearchQuery::new("harbour"),
                None,
                DEFAULT_SEARCH_CANDIDATE_LIMIT,
                DEFAULT_SEARCH_RESULT_LIMIT,
            );
            assert!(page.results.len() <= DEFAULT_SEARCH_RESULT_LIMIT);
            assert!(page.inspected <= DEFAULT_SEARCH_CANDIDATE_LIMIT);
            let (resource_id, _) = target(&page.results[0]);
            let resource = LibraryIndex::new(&crate::test_doc::library(&doc))
                .unwrap()
                .resource(&resource_id)
                .expect("the library index publishes the hit's canonical resource");
            assert_eq!(resource.id.0, resource_id);
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == "remove-photo"),
                "every verb on the published photograph remains agent-addressable"
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn collection_and_late_search_hits_materialize_their_exact_resources() {
        let mut doc = crate::test_doc::new_doc();
        let location = PhotoLocation {
            latitude: 48.8566,
            longitude: 2.3522,
            label: Some("Offline Paris".to_owned()),
            provenance: LocationProvenance::default(),
        };
        let mut first = photo_of("one.png", 64);
        first.location = Some(location.clone());
        let mut second = photo_of("two.png", 64);
        second.location = Some(location.clone());
        let library = PhotoLibrary::of(vec![first, second], Vec::new());
        crate::test_doc::set_library(&mut doc, &library);
        let place = place_resource_id(&location.place_key());
        let hit = source(&doc)
            .search("Offline Paris")
            .iter()
            .map(target)
            .find(|(resource, _)| *resource == place)
            .expect("place is globally searchable");
        assert_eq!(view_of(&doc, &hit.0).as_deref(), Some("view-place"));
        let resource = LibraryIndex::new(&library)
            .unwrap()
            .resource(&hit.0)
            .expect("exact search target is published");
        assert_eq!(resource.kind, "photos.library.place");
        assert!(
            resource
                .children
                .iter()
                .any(|child| child.label == "2 photos")
        );

        {
            let mut doc = crate::test_doc::new_doc();
            let mut library = PhotoLibrary::default();
            for index in 0..80 {
                library.add_album(Album {
                    id: format!("photos.album:{index}"),
                    name: if index == 79 {
                        "旅行-needle-夏".into()
                    } else {
                        format!("Album {index}")
                    },
                    photo_ids: ui::tilo_resource::Set::new(),
                });
            }
            crate::test_doc::set_library(&mut doc, &library);
            let hit = source(&doc)
                .search_page(&SearchQuery::new("NEEDLE-夏"), None, 2, 1)
                .results
                .pop()
                .expect("late album remains searchable outside the contextual snapshot");
            let hit = target(&hit);
            assert_eq!(
                hit,
                (crate::resource::album_resource_id("photos.album:79"), None),
                "search activation opens the album, never changes its membership"
            );
            let resource = opened(&doc, &hit).expect("the late stable album id resolves lazily");
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == "remove-album"),
                "the album offers its verbs"
            );
        }

        {
            let mut doc = crate::test_doc::new_doc();
            let library = PhotoLibrary::of(
                Vec::new(),
                vec![Album::named("photos.album:journey", "Journey")],
            );
            crate::test_doc::set_library(&mut doc, &library);

            let albums = derived_resource_id("albums");
            let hit = source(&doc)
                .search("shared albums")
                .iter()
                .map(target)
                .find(|(resource, _)| *resource == albums)
                .expect("album browsing is a global-search destination");
            assert_eq!(view_of(&doc, &hit.0).as_deref(), Some("view-albums"));
            let resource = LibraryIndex::new(&library)
                .unwrap()
                .resource(&hit.0)
                .expect("the album collection is published");
            assert_eq!(resource.kind, "photos.library.albums");
            assert!(
                resource
                    .children
                    .iter()
                    .any(|child| child.label == "1 album")
            );
        }

        {
            let mut doc = crate::test_doc::new_doc();
            let mut library = PhotoLibrary::default();
            for index in 0..3_000 {
                let name = if index == 2_999 {
                    "旅行-needle-夏.png".to_string()
                } else {
                    format!("photo-{index}.png")
                };
                library.add_photo(photo_of(&name, 64));
            }
            crate::test_doc::set_library(&mut doc, &library);
            let page = source(&doc).search_page(&SearchQuery::new("NEEDLE-夏"), None, 2, 1);
            assert_eq!(page.results.len(), 1);
            assert!(page.inspected <= 2, "query work exceeded its hard budget");
            assert_eq!(page.results[0].title, "旅行-needle-夏.png");
            assert_eq!(
                target(&page.results[0]).1,
                None,
                "a photograph opens as itself"
            );
        }

        {
            let mut expected_records = 0;
            let mut built = PhotoLibrary::default();
            let (provider, hit) = assert_late_hit_reachable(
                |corpus| {
                    let mut library = PhotoLibrary::default();
                    for row in corpus {
                        let stem = row.text.replace(' ', "-");
                        let name = if row.is_target {
                            format!("{stem}-夏.png")
                        } else {
                            format!("{stem}.png")
                        };
                        library.add_photo(photo_of(&name, 1));
                    }
                    expected_records = library.photos.len() + 4; // library + timeline + places + albums
                    built = library.clone();
                    source_of(&library)
                },
                3_000,
            );
            let index = LibraryIndex::new(&built).unwrap();
            assert_eq!(index.stats().records, expected_records);
            let page = provider.search_page(&SearchQuery::new("needle-夏"), None, 2, 1);
            assert_eq!(page.results, vec![hit.clone()]);
            assert!(page.inspected <= 2);
            let (resource, action) = target(&hit);
            assert_eq!(action, None);
            assert!(index.resource(&resource).is_some());
        }
    }

    #[test]
    fn stale_or_colliding_targets_fail_closed_and_rebuilds_publish_atomically() {
        let before = open_doc("before.png");
        let hit = target(&source(&before).search("before").pop().unwrap());
        let after = open_doc("after.png");
        assert!(opened(&after, &hit).is_none());

        {
            let mut doc = open_doc("before.png");
            let index = LibraryIndex::new(&crate::test_doc::library(&doc)).unwrap();
            let generation = index.generation();
            let hit = target(&source(&doc).search("before")[0]);

            crate::test_doc::set_library(&mut doc, &PhotoLibrary::default());
            index.sync(&crate::test_doc::library(&doc)).unwrap();
            assert_ne!(index.generation(), generation, "the old pages are stale");
            assert!(index.resource(&hit.0).is_none());
            assert!(opened(&doc, &hit).is_none());
        }

        {
            let mut place = photo_of("collision.png", 1);
            place.folder = Some("Collision".into());
            let mut tagged = photo_of("tagged.png", 1);
            tagged.tags = crate::model::labels(["Collision"]);
            let library = PhotoLibrary::of(vec![place, tagged], Vec::new());
            let folder = crate::derived::folder_resource_id("Collision");
            assert!(
                LibraryIndex::new(&library)
                    .unwrap()
                    .resource(&folder)
                    .is_some(),
                "a folder and a tag of one name are two places"
            );
        }

        {
            let doc = open_doc("last-good.png");
            let library = crate::test_doc::library(&doc);
            let provider = LibraryIndex::new(&library).unwrap();
            let before_generation = provider.generation();
            let (resource, _) = target(&source(&doc).search("last-good")[0]);
            let before = provider.resource(&resource).unwrap();
            let before_records = provider.stats().records;

            assert_eq!(
                provider.reject_malformed_snapshot_for_test(),
                Err(SearchCatalogError::NonCanonicalResult)
            );
            assert_eq!(provider.generation(), before_generation);
            assert_eq!(provider.stats().records, before_records);
            let after = provider.resource(&resource).unwrap();
            assert_eq!(after.id, before.id);
            assert_eq!(after.actions.len(), before.actions.len());
        }

        {
            let doc = open_doc("before.png");
            let provider = LibraryIndex::new(&crate::test_doc::library(&doc)).unwrap();
            let (before_resource, _) = target(&source(&doc).search("before")[0]);

            let mut replacement = crate::test_doc::library(&doc);
            replacement.add_photo(photo_of("after.png", 16));
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

                // This runs exactly in the window after the replacement rows
                // exist but before the short publication. If that work ever
                // re-enters the public state lock, either operation deadlocks
                // here instead of continuing against the prior generation.
                assert!(
                    provider
                        .photo_matches_page("before", None)
                        .0
                        .contains(&before_resource)
                );
                assert!(provider.resource(&before_resource).is_some());
                publish_tx.send(()).expect("release publication");
            });

            assert!(
                !provider.photo_matches_page("after", None).0.is_empty(),
                "the published replacement finds the new photograph"
            );
        }
    }

    #[test]
    fn a_synced_library_answers_as_a_fresh_index() {
        let mut photos: Vec<LibraryPhoto> = (0..40)
            .map(|at| photo_of(&format!("harbour-{at:02}.png"), 1 + at % 7))
            .collect();
        let provider = LibraryIndex::new(&PhotoLibrary::of(photos.clone(), Vec::new())).unwrap();
        let queries = ["harbour", "harbour-3", "golden", "Élodie", "trip", "png"];
        for step in 0..12_usize {
            match step % 4 {
                0 => photos.push(photo_of(&format!("trip-{step}.png"), 3)),
                1 => photos[step].tags = crate::model::labels(["golden-hour"]),
                2 => photos[step].people = crate::model::labels(["Élodie"]),
                _ => {
                    photos.remove(step);
                }
            }
            let library = PhotoLibrary::of(photos.clone(), Vec::new());
            let generation = provider.generation();
            provider.sync(&library).unwrap();
            assert_ne!(
                provider.generation(),
                generation,
                "step {step} changed the library"
            );
            let fresh = LibraryIndex::new(&library).unwrap();
            for query in queries {
                assert_eq!(
                    provider.photo_matches_page(query, None).0,
                    fresh.photo_matches_page(query, None).0,
                    "step {step}, query {query:?}"
                );
            }
            // The same library again changes nothing: cursors stay current.
            let generation = provider.generation();
            provider.sync(&library).unwrap();
            assert_eq!(provider.generation(), generation);
        }
    }
}
