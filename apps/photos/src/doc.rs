//! Automerge document layer.
//!
//! What the user decided lives in the CRDT: the photographs of the library,
//! how each is filed, and the crop and turn each is shown with. That makes
//! every edit undoable and replicable to a collaborator for free. Which
//! photograph a window shows is that window's own view state, never shared.
//!
//! ```text
//! root
//! └── "photos_library" (Map) # keyed photo/album records, one column map per
//!                            # field (crop and turn among them), order
//!                            # lists, delete tombstones
//! ```
//!
//! # What is deliberately not here
//!
//! The pixels. The document holds portable display metadata and two small edit
//! fields per photograph and never the image itself, which lives in the app's content store
//! (`tilo-content`) under its hash: an automerge change carrying a multi-megabyte payload bloats every replica and
//! durable host log and makes later edits wait behind the media. A content
//! address plus a crop rect is a change of a few dozen bytes.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::doc::read_str;

use crate::model::{Edits, ImageAnalysis, LibraryPhoto, OpenImage, PhotoLibrary, PhotoLocation};

/// The bytes of the picture `open` names, from wherever this machine has them.
///
/// Content first: it is the portable identity every current import writes.
/// Path-only legacy records retain their old local fallback, but a new shared
/// record never causes this device to probe another participant's filesystem.
/// Resolving the hash is what turns "a photograph a collaborator opened" into
/// pixels here rather than into an empty canvas.
///
/// Missing both is not an error to report: content a collaborator imported may
/// simply not have reached this device's store yet, so the honest answer now is
/// no picture, and the moment after the bytes land there is one.
///
/// Public, and the only answer to this question, because the app is not the
/// only thing that asks it. Anything acting on the open photograph — the
/// editor drawing it, a participant with no window rendering the crop it was
/// asked for — needs the same bytes, and a second copy of "read the path" is
/// how one of those callers ends up reading a path that is true on another
/// machine.
pub fn bytes_of(open: &OpenImage, store: &tilo_content::ContentStore) -> Option<Vec<u8>> {
    if let Some(content) = &open.content {
        return crate::pixels::read_bounded(store.open(content)?, crate::pixels::MAX_ENCODED_BYTES);
    }
    crate::files::read_native(
        std::path::Path::new(&open.path),
        crate::pixels::MAX_ENCODED_BYTES as u64,
    )
}

pub struct PhotosDoc;

/// The durable metadata namespace behind a virtual Photos browse resource.
///
/// These entries retain only a public label and its live photograph count;
/// they deliberately do not duplicate image paths, content references, or
/// action trees. Keeping this compact keyed projection in the document makes
/// an exact folder/person/tag address resolvable without walking every photo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataViewKind {
    Folder,
    Person,
    Tag,
    Object,
    Pet,
}

/// One exact metadata label resolved from the retained CRDT projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataView {
    pub kind: MetadataViewKind,
    pub label: String,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct MetadataIndexEntry {
    label: String,
    count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct OcrAnalysisComponent {
    text: String,
    provenance: crate::model::AnalysisProvenance,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct VisionAnalysisComponent {
    description: String,
    objects: Vec<String>,
    pets: Vec<String>,
    provenance: crate::model::AnalysisProvenance,
}

const PHOTO_ANALYSIS_OCR_KEY: &str = "photo_analysis_ocr";
const PHOTO_ANALYSIS_VISION_KEY: &str = "photo_analysis_vision";

/// Structured library projection: independently mergeable maps and lists
/// below this key.
const LIBRARY_KEY: &str = "photos_library";
/// A photograph's crop and its turn are separate columns: a crop is one
/// coherent rectangle, while rotation is a separate decision, so the two
/// merge independently without pretending that the four coordinates of one
/// crop could be composed.
const PHOTO_CROP_KEY: &str = "photo_crop";
const PHOTO_ROTATION_KEY: &str = "photo_rotation";
const METADATA_FOLDERS_KEY: &str = "metadata_folders";
const METADATA_PEOPLE_KEY: &str = "metadata_people";
const METADATA_TAGS_KEY: &str = "metadata_tags";
const METADATA_OBJECTS_KEY: &str = "metadata_objects";
const METADATA_PETS_KEY: &str = "metadata_pets";

fn metadata_resource_id(prefix: &str, label: &str) -> String {
    format!("{prefix}{}", tilo_content::hash_bytes(label.as_bytes()))
}

/// The per-field column maps a photo's metadata is split across, one map
/// per field keyed by photo id.
struct PhotoColumns {
    core: ObjId,
    favorite: ObjId,
    folder: ObjId,
    people: ObjId,
    tags: ObjId,
    captured: ObjId,
    analysis: ObjId,
    analysis_ocr: ObjId,
    analysis_vision: ObjId,
    location: ObjId,
    crop: ObjId,
    rotation: ObjId,
}

impl PhotoColumns {
    /// Open (creating as needed) every column map under `root`, in schema
    /// order.
    fn open(doc: &mut AutoCommit, root: &ObjId) -> Option<Self> {
        Some(Self {
            core: PhotosDoc::map_or_make(doc, root, "photo_core")?,
            favorite: PhotosDoc::map_or_make(doc, root, "photo_favorite")?,
            folder: PhotosDoc::map_or_make(doc, root, "photo_folder")?,
            people: PhotosDoc::map_or_make(doc, root, "photo_people")?,
            tags: PhotosDoc::map_or_make(doc, root, "photo_tags")?,
            captured: PhotosDoc::map_or_make(doc, root, "photo_captured_at")?,
            analysis: PhotosDoc::map_or_make(doc, root, "photo_analysis")?,
            analysis_ocr: PhotosDoc::map_or_make(doc, root, PHOTO_ANALYSIS_OCR_KEY)?,
            analysis_vision: PhotosDoc::map_or_make(doc, root, PHOTO_ANALYSIS_VISION_KEY)?,
            location: PhotosDoc::map_or_make(doc, root, "photo_location")?,
            crop: PhotosDoc::map_or_make(doc, root, PHOTO_CROP_KEY)?,
            rotation: PhotosDoc::map_or_make(doc, root, PHOTO_ROTATION_KEY)?,
        })
    }
}

impl PhotosDoc {
    /// True when the document holds no library yet.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        Self::library_roots(doc).is_empty()
    }

    /// Complete durable library metadata, materialized from per-record CRDT
    /// maps.
    pub fn library(doc: &AutoCommit) -> PhotoLibrary {
        Self::structured_library(doc).unwrap_or_default()
    }

    /// Resolve one photograph by its stable id without materialising the
    /// complete library.  This is the path used by resource/action lookup:
    /// opening a palette result near the end of a 100k-photo library must not
    /// first allocate 100k unrelated records.
    pub fn photo(doc: &AutoCommit, id: &str) -> Option<LibraryPhoto> {
        let roots = Self::library_roots(doc);
        if roots.is_empty() {
            return None;
        }
        if roots.iter().any(|root| {
            Self::map_at(doc, root, "removed_photos")
                .as_ref()
                .is_some_and(|removed| Self::marked_removed(doc, Some(removed), id))
        }) {
            return None;
        }
        let mut matches = roots.iter().filter_map(|root| {
            Self::map_at(doc, root, "photo_core")
                .and_then(|core| Self::read_columnar_photo(doc, root, &core, id))
        });
        let mut photo = matches.next()?;
        for next in matches {
            photo.favorite |= next.favorite;
            if photo.folder.is_none() {
                photo.folder = next.folder;
            }
            for person in next.people {
                if !photo.people.contains(&person) {
                    photo.people.push(person);
                }
            }
            for tag in next.tags {
                if !photo.tags.contains(&tag) {
                    photo.tags.push(tag);
                }
            }
            if photo.captured_at.is_none() {
                photo.captured_at = next.captured_at;
            }
            if photo.analysis.is_none() {
                photo.analysis = next.analysis;
            }
        }
        Some(photo)
    }

    /// Resolve one album directly from its keyed CRDT row. Membership is
    /// capped by the action contract, so filtering deleted photo ids remains
    /// bounded independently of total library size.
    pub fn album(doc: &AutoCommit, id: &str) -> Option<crate::model::Album> {
        let roots = Self::library_roots(doc);
        if roots.is_empty() {
            return None;
        }
        if roots.iter().any(|root| {
            Self::map_at(doc, root, "removed_albums")
                .as_ref()
                .is_some_and(|removed| Self::marked_removed(doc, Some(removed), id))
        }) {
            return None;
        }
        let mut matches = roots.iter().filter_map(|root| {
            Self::map_at(doc, root, "albums").and_then(|albums| Self::read_album(doc, &albums, id))
        });
        let mut album = matches.next()?;
        for next in matches {
            for photo_id in next.photo_ids {
                if !album.photo_ids.contains(&photo_id) {
                    album.photo_ids.push(photo_id);
                }
            }
        }
        album
            .photo_ids
            .retain(|photo_id| Self::photo(doc, photo_id).is_some());
        Some(album)
    }

    /// Apply a validated projection as granular record/field deltas. This is
    /// intentionally the compatibility seam used by existing canonical
    /// actions: they can keep their replayable pure model mutation while the
    /// document preserves unrelated concurrent fields and records.
    pub fn set_library(doc: &mut AutoCommit, library: &PhotoLibrary) {
        let before = Self::library(doc);
        let Some(root) = Self::ensure_structured_library(doc) else {
            return;
        };
        Self::sync_library(doc, &root, &before, &library.portable());
    }

    /// The one structured library root, created on first write.
    fn ensure_structured_library(doc: &mut AutoCommit) -> Option<ObjId> {
        let roots = Self::library_roots(doc);
        if let [root] = roots.as_slice() {
            return Some(root.clone());
        }
        // Concurrent first-run roots are folded as
        // soon as an editor observes them. Until then `structured_library`
        // reads their additive projection, so no independently imported
        // record is stranded behind Automerge's single-value map lookup.
        let inherited = (!roots.is_empty())
            .then(|| Self::structured_library(doc))
            .flatten();
        if !roots.is_empty() {
            let _ = doc.delete(&ROOT, LIBRARY_KEY);
        }
        let library = inherited.unwrap_or_default();
        let root = doc.put_object(&ROOT, LIBRARY_KEY, ObjType::Map).ok()?;
        let _ = Self::map_or_make(doc, &root, "photo_core");
        let _ = Self::map_or_make(doc, &root, "photo_favorite");
        let _ = Self::map_or_make(doc, &root, "photo_folder");
        let _ = Self::map_or_make(doc, &root, "photo_people");
        let _ = Self::map_or_make(doc, &root, "photo_tags");
        let _ = Self::map_or_make(doc, &root, "photo_captured_at");
        let _ = Self::map_or_make(doc, &root, "photo_analysis");
        let _ = Self::map_or_make(doc, &root, PHOTO_ANALYSIS_OCR_KEY);
        let _ = Self::map_or_make(doc, &root, PHOTO_ANALYSIS_VISION_KEY);
        let _ = Self::map_or_make(doc, &root, "photo_location");
        let _ = Self::map_or_make(doc, &root, PHOTO_CROP_KEY);
        let _ = Self::map_or_make(doc, &root, PHOTO_ROTATION_KEY);
        let _ = Self::map_or_make(doc, &root, "albums");
        let _ = Self::map_or_make(doc, &root, "removed_photos");
        let _ = Self::map_or_make(doc, &root, "removed_albums");
        let _ = Self::map_or_make(doc, &root, METADATA_FOLDERS_KEY);
        let _ = Self::map_or_make(doc, &root, METADATA_PEOPLE_KEY);
        let _ = Self::map_or_make(doc, &root, METADATA_TAGS_KEY);
        let _ = Self::map_or_make(doc, &root, METADATA_OBJECTS_KEY);
        let _ = Self::map_or_make(doc, &root, METADATA_PETS_KEY);
        let _ = Self::list_or_make(doc, &root, "photo_order");
        let _ = Self::list_or_make(doc, &root, "album_order");
        if !library.photos.is_empty() || !library.albums.is_empty() {
            Self::sync_library(doc, &root, &PhotoLibrary::default(), &library.portable());
        }
        Some(root)
    }

    fn library_roots(doc: &AutoCommit) -> Vec<ObjId> {
        doc.get_all(&ROOT, LIBRARY_KEY)
            .map(|values| {
                values
                    .into_iter()
                    .filter_map(|(value, id)| {
                        matches!(value, Value::Object(ObjType::Map)).then_some(id)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn map_or_make(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
        match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => doc.put_object(parent, key, ObjType::Map).ok(),
        }
    }

    fn list_or_make(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
        match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
            _ => doc.put_object(parent, key, ObjType::List).ok(),
        }
    }

    fn structured_library(doc: &AutoCommit) -> Option<PhotoLibrary> {
        let roots = Self::library_roots(doc);
        if roots.is_empty() {
            return None;
        }
        let mut combined = PhotoLibrary::default();
        for root in &roots {
            let partial = Self::structured_library_at(doc, root)?;
            Self::merge_library_projection(&mut combined, partial);
        }
        // Tombstones are delete-wins across *all* concurrently-created roots,
        // not only the root whose record happened to be projected first.
        combined.photos.retain(|photo| {
            !roots.iter().any(|root| {
                Self::map_at(doc, root, "removed_photos")
                    .as_ref()
                    .is_some_and(|removed| Self::marked_removed(doc, Some(removed), &photo.id))
            })
        });
        combined.albums.retain(|album| {
            !roots.iter().any(|root| {
                Self::map_at(doc, root, "removed_albums")
                    .as_ref()
                    .is_some_and(|removed| Self::marked_removed(doc, Some(removed), &album.id))
            })
        });
        let live_photo_ids: std::collections::HashSet<_> =
            combined.photos.iter().map(|photo| &photo.id).collect();
        for album in &mut combined.albums {
            album
                .photo_ids
                .retain(|photo_id| live_photo_ids.contains(photo_id));
        }
        Some(combined)
    }

    fn structured_library_at(doc: &AutoCommit, root: &ObjId) -> Option<PhotoLibrary> {
        let photo_core = Self::map_at(doc, root, "photo_core")?;
        let albums = Self::map_at(doc, root, "albums")?;
        let removed_photos = Self::map_at(doc, root, "removed_photos");
        let removed_albums = Self::map_at(doc, root, "removed_albums");
        let photo_order = Self::string_list(doc, root, "photo_order");
        let album_order = Self::string_list(doc, root, "album_order");

        let mut library = PhotoLibrary::default();
        for id in Self::ordered_keys(doc, &photo_core, photo_order) {
            if Self::marked_removed(doc, removed_photos.as_ref(), &id) {
                continue;
            }
            if let Some(photo) = Self::read_columnar_photo(doc, root, &photo_core, &id) {
                library.photos.push(photo);
            }
        }
        for id in Self::ordered_keys(doc, &albums, album_order) {
            if Self::marked_removed(doc, removed_albums.as_ref(), &id) {
                continue;
            }
            if let Some(mut album) = Self::read_album(doc, &albums, &id) {
                album
                    .photo_ids
                    .retain(|photo_id| library.photo(photo_id).is_some());
                library.albums.push(album);
            }
        }
        Some(library)
    }

    /// Merge projections from concurrently-created top-level maps. The normal
    /// path never needs this (record maps merge directly); it exists solely
    /// for a first-run race before either participant has
    /// observed the other's root. Additive metadata is retained, and the
    /// following consolidating write returns the document to one normal root.
    fn merge_library_projection(into: &mut PhotoLibrary, from: PhotoLibrary) {
        for photo in from.photos {
            if let Some(existing) = into.photo_mut(&photo.id) {
                existing.favorite |= photo.favorite;
                if existing.folder.is_none() {
                    existing.folder = photo.folder;
                }
                for value in photo.people {
                    if !existing.people.contains(&value) {
                        existing.people.push(value);
                    }
                }
                for value in photo.tags {
                    if !existing.tags.contains(&value) {
                        existing.tags.push(value);
                    }
                }
                if existing.captured_at.is_none() {
                    existing.captured_at = photo.captured_at;
                }
                if existing.analysis.is_none() {
                    existing.analysis = photo.analysis;
                }
                if existing.location.is_none() {
                    existing.location = photo.location;
                }
                if existing.edits.is_identity() {
                    existing.edits = photo.edits;
                }
            } else {
                into.photos.push(photo);
            }
        }
        for album in from.albums {
            if let Some(existing) = into.album_mut(&album.id) {
                for photo_id in album.photo_ids {
                    if !existing.photo_ids.contains(&photo_id) {
                        existing.photo_ids.push(photo_id);
                    }
                }
            } else {
                into.albums.push(album);
            }
        }
    }

    fn map_at(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
        match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => None,
        }
    }

    fn maps_at(doc: &AutoCommit, parent: &ObjId, key: &str) -> Vec<ObjId> {
        doc.get_all(parent, key)
            .map(|values| {
                values
                    .into_iter()
                    .filter_map(|(value, id)| {
                        matches!(value, Value::Object(ObjType::Map)).then_some(id)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn ordered_keys(doc: &AutoCommit, map: &ObjId, order: Vec<String>) -> Vec<String> {
        let mut ids = order;
        let mut seen: std::collections::HashSet<_> = ids.iter().cloned().collect();
        for key in doc.keys(map) {
            if seen.insert(key.clone()) {
                ids.push(key);
            }
        }
        ids
    }

    fn marked_removed(doc: &AutoCommit, removed: Option<&ObjId>, id: &str) -> bool {
        // A concurrent stale import can retain an older false/absent view of
        // this key. Any live `true` tombstone wins over that conflict, making
        // removal deterministic rather than dependent on an actor id.
        removed.is_some_and(|map| {
            doc.get_all(map, id).is_ok_and(|values| {
                values.into_iter().any(|(value, _)| {
                    matches!(value, Value::Scalar(value) if matches!(value.as_ref(), ScalarValue::Boolean(true)))
                })
            })
        })
    }

    fn read_columnar_photo(
        doc: &AutoCommit,
        root: &ObjId,
        core: &ObjId,
        id: &str,
    ) -> Option<LibraryPhoto> {
        let image = Self::conflicting_at::<OpenImage>(doc, core, id)
            .into_iter()
            .next()?;
        let field = |name: &str| Self::map_at(doc, root, name);
        Some(LibraryPhoto {
            id: id.to_owned(),
            image,
            favorite: field("photo_favorite")
                .and_then(|map| {
                    Self::conflicting_at::<bool>(doc, &map, id)
                        .into_iter()
                        .next()
                })
                .unwrap_or(false),
            folder: field("photo_folder").and_then(|map| {
                Self::conflicting_at::<Option<String>>(doc, &map, id)
                    .into_iter()
                    .next()
                    .flatten()
            }),
            people: field("photo_people")
                .map(|map| Self::label_membership(doc, &map, id))
                .unwrap_or_default(),
            tags: field("photo_tags")
                .map(|map| Self::label_membership(doc, &map, id))
                .unwrap_or_default(),
            captured_at: field("photo_captured_at").and_then(|map| {
                Self::conflicting_at::<Option<String>>(doc, &map, id)
                    .into_iter()
                    .next()
                    .flatten()
            }),
            analysis: Self::read_analysis_components(doc, root, id),
            location: field("photo_location").and_then(|map| {
                Self::conflicting_at::<Option<PhotoLocation>>(doc, &map, id)
                    .into_iter()
                    .next()
                    .flatten()
            }),
            edits: Edits {
                // A real crop beats an untouched one and a turn beats none,
                // so a collaborator's edit survives a concurrent reset.
                crop: field(PHOTO_CROP_KEY)
                    .map(|map| Self::conflicting_at::<crate::model::Rect01>(doc, &map, id))
                    .unwrap_or_default()
                    .into_iter()
                    .find(|crop| !crop.is_full())
                    .unwrap_or_default(),
                rotate_quarters: field(PHOTO_ROTATION_KEY)
                    .map(|map| Self::conflicting_at::<u8>(doc, &map, id))
                    .unwrap_or_default()
                    .into_iter()
                    .find(|turns| !turns.is_multiple_of(4))
                    .unwrap_or_default(),
            },
        })
    }

    fn ocr_component(analysis: &ImageAnalysis) -> Option<OcrAnalysisComponent> {
        let provenance = analysis
            .ocr_provenance
            .clone()
            .or_else(|| (!analysis.ocr_text.is_empty()).then(|| analysis.provenance.clone()))?;
        Some(OcrAnalysisComponent {
            text: analysis.ocr_text.clone(),
            provenance,
        })
    }

    fn vision_component(analysis: &ImageAnalysis) -> Option<VisionAnalysisComponent> {
        let provenance = analysis.vision_provenance.clone().or_else(|| {
            (!analysis.description.is_empty()
                || !analysis.objects.is_empty()
                || !analysis.pets.is_empty())
            .then(|| analysis.provenance.clone())
        })?;
        Some(VisionAnalysisComponent {
            description: analysis.description.clone(),
            objects: analysis.objects.clone(),
            pets: analysis.pets.clone(),
            provenance,
        })
    }

    fn deterministic<T: serde::Serialize>(values: Vec<T>) -> Option<T> {
        values.into_iter().max_by(|left, right| {
            serde_json::to_string(left)
                .unwrap_or_default()
                .cmp(&serde_json::to_string(right).unwrap_or_default())
        })
    }

    #[expect(
        clippy::option_option,
        reason = "distinguishes an absent component (None) from an explicit removal (Some(None))"
    )]
    fn component_value<T>(doc: &AutoCommit, maps: &[ObjId], id: &str) -> Option<Option<T>>
    where
        T: serde::de::DeserializeOwned + serde::Serialize,
    {
        let values = maps
            .iter()
            .flat_map(|map| Self::conflicting_at::<Option<T>>(doc, map, id))
            .collect::<Vec<_>>();
        if values.is_empty() {
            None
        } else if values.iter().any(Option::is_none) {
            // An explicit removal wins a concurrent stale analyzer for the
            // same component. Privacy deletion must not depend on actor id.
            Some(None)
        } else {
            Some(Self::deterministic(values.into_iter().flatten().collect()))
        }
    }

    fn read_analysis_components(doc: &AutoCommit, root: &ObjId, id: &str) -> Option<ImageAnalysis> {
        let legacy = Self::map_at(doc, root, "photo_analysis")
            .map(|map| Self::conflicting_at::<ImageAnalysis>(doc, &map, id))
            .unwrap_or_default();
        let mut analysis = Self::deterministic(legacy.clone()).unwrap_or_default();

        let legacy_ocr = Self::deterministic(
            legacy
                .iter()
                .filter_map(Self::ocr_component)
                .collect::<Vec<_>>(),
        );
        let legacy_vision = Self::deterministic(
            legacy
                .iter()
                .filter_map(Self::vision_component)
                .collect::<Vec<_>>(),
        );
        let ocr_state = Self::component_value::<OcrAnalysisComponent>(
            doc,
            &Self::maps_at(doc, root, PHOTO_ANALYSIS_OCR_KEY),
            id,
        );
        let vision_state = Self::component_value::<VisionAnalysisComponent>(
            doc,
            &Self::maps_at(doc, root, PHOTO_ANALYSIS_VISION_KEY),
            id,
        );
        let ocr = ocr_state.clone().unwrap_or(legacy_ocr);
        let vision = vision_state.clone().unwrap_or(legacy_vision);

        analysis.ocr_text.clear();
        analysis.description.clear();
        analysis.objects.clear();
        analysis.pets.clear();
        analysis.ocr_provenance = None;
        analysis.vision_provenance = None;
        if let Some(component) = ocr {
            analysis.ocr_text = component.text;
            analysis.ocr_provenance = Some(component.provenance);
        }
        if let Some(component) = vision {
            analysis.description = component.description;
            analysis.objects = component.objects;
            analysis.pets = component.pets;
            analysis.vision_provenance = Some(component.provenance);
        }
        analysis.provenance = analysis
            .vision_provenance
            .clone()
            .or_else(|| analysis.ocr_provenance.clone())
            .unwrap_or_else(|| analysis.provenance.clone());

        let explicit_clear = matches!(ocr_state, Some(None))
            && matches!(vision_state, Some(None))
            && analysis.people.is_empty();
        (!explicit_clear
            && (!analysis.is_empty()
                || analysis.ocr_provenance.is_some()
                || analysis.vision_provenance.is_some()))
        .then_some(analysis)
    }

    fn read_album(doc: &AutoCommit, albums: &ObjId, id: &str) -> Option<crate::model::Album> {
        let row = Self::map_at(doc, albums, id)?;
        Some(crate::model::Album {
            id: id.to_owned(),
            name: read_str(doc, &row, "name")?,
            // The complete absolute membership is one scalar, so concurrent
            // replacements resolve to one deterministic authored set instead
            // of Automerge interleaving two independently rewritten lists.
            photo_ids: Self::conflicting_at::<Vec<String>>(doc, &row, "photo_ids")
                .into_iter()
                .next()
                .unwrap_or_default(),
        })
    }

    fn string_list(doc: &AutoCommit, parent: &ObjId, key: &str) -> Vec<String> {
        let Some(list) = (match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
            _ => None,
        }) else {
            return Vec::new();
        };
        (0..doc.length(&list))
            .filter_map(|index| read_str(doc, &list, index))
            .collect()
    }

    /// Read one labels field: each membership is its own CRDT key.
    fn label_membership(doc: &AutoCommit, parent: &ObjId, photo_id: &str) -> Vec<String> {
        let rows: Vec<_> = doc
            .get_all(parent, photo_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(value, id)| matches!(value, Value::Object(ObjType::Map)).then_some(id))
            .collect();
        let mut candidates = std::collections::BTreeSet::new();
        for row in &rows {
            candidates.extend(doc.keys(row));
        }
        candidates
            .into_iter()
            .filter(|label| {
                rows.iter().any(|row| {
                    doc.get_all(row, label).is_ok_and(|values| {
                        // Add wins a concurrent removal, including across
                        // conflicting map objects two replicas created for
                        // the same photo.
                        values.into_iter().any(|(value, _)| {
                            matches!(value, Value::Scalar(value) if matches!(value.as_ref(), ScalarValue::Boolean(true)))
                        })
                    })
                })
            })
            .collect()
    }

    /// Project list changes to independent membership keys.  This makes a
    /// tag added offline on one replica commute with a different tag added on
    /// another, while `Set`/`Clear` remain explicit writes of the memberships
    /// they actually change.
    fn write_label_delta(
        doc: &mut AutoCommit,
        parent: &ObjId,
        photo_id: &str,
        before: &[String],
        after: &[String],
    ) {
        let rows: Vec<_> = doc
            .get_all(parent, photo_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(value, id)| matches!(value, Value::Object(ObjType::Map)).then_some(id))
            .collect();
        if rows.is_empty() {
            let labels = after
                .iter()
                .cloned()
                .map(|label| {
                    (
                        label,
                        automerge::hydrate::Value::Scalar(ScalarValue::Boolean(true)),
                    )
                })
                .collect::<std::collections::HashMap<_, _>>();
            let value = automerge::hydrate::Value::Map(labels.into());
            let Some(_row) = doc
                .batch_create_object(parent, photo_id, &value, false)
                .ok()
            else {
                return;
            };
            return;
        }
        for label in before.iter().filter(|label| !after.contains(*label)) {
            for row in &rows {
                let _ = doc.put(row, label, false);
            }
        }
        for label in after.iter().filter(|label| !before.contains(*label)) {
            for row in &rows {
                let _ = doc.put(row, label, true);
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the schema boundary keeps all columnar maps and their delta application auditable together"
    )]
    fn sync_library(
        doc: &mut AutoCommit,
        root: &ObjId,
        before: &PhotoLibrary,
        after: &PhotoLibrary,
    ) {
        let Some(columns) = PhotoColumns::open(doc, root) else {
            return;
        };
        let Some(albums) = Self::map_or_make(doc, root, "albums") else {
            return;
        };
        let Some(removed_photos) = Self::map_or_make(doc, root, "removed_photos") else {
            return;
        };
        let Some(removed_albums) = Self::map_or_make(doc, root, "removed_albums") else {
            return;
        };
        let Some(photo_order) = Self::list_or_make(doc, root, "photo_order") else {
            return;
        };
        let Some(album_order) = Self::list_or_make(doc, root, "album_order") else {
            return;
        };
        // A first bulk import has no existing record maps to merge against.
        // Write that snapshot in one AutoCommit transaction without a read
        // between every row; otherwise each `map_or_make` closes the implicit
        // transaction and turns a 3k import into thousands of commits.
        if before.photos.is_empty() && after.albums.is_empty() {
            for photo in &after.photos {
                Self::write_columnar_photo_delta(doc, &columns, None, photo);
            }
            let order_values: Vec<_> = after
                .photos
                .iter()
                .map(|photo| automerge::hydrate::Value::Scalar(photo.id.clone().into()))
                .collect();
            let _ = doc.splice(&photo_order, 0, 0, order_values);
            Self::sync_metadata_index(doc, root, after);
            return;
        }
        // Materialize lookup/order sets once. Bulk import must
        // be linear in the bounded metadata count, not repeatedly scan the
        // entire library while adding each record.
        let before_photos: std::collections::HashMap<_, _> = before
            .photos
            .iter()
            .map(|photo| (photo.id.as_str(), photo))
            .collect();
        let after_photo_ids: std::collections::HashSet<_> =
            after.photos.iter().map(|photo| photo.id.as_str()).collect();
        let mut ordered_photos: std::collections::HashSet<_> =
            Self::string_values(doc, &photo_order).into_iter().collect();

        for photo in &after.photos {
            let old = before_photos.get(photo.id.as_str()).copied();
            if old.is_none() {
                let _ = doc.delete(&removed_photos, &photo.id);
            }
            Self::write_columnar_photo_delta(doc, &columns, old, photo);
            if ordered_photos.insert(photo.id.clone()) {
                let _ = doc.insert(&photo_order, doc.length(&photo_order), &photo.id);
            }
        }
        for photo in &before.photos {
            if !after_photo_ids.contains(photo.id.as_str()) {
                let _ = doc.put(&removed_photos, &photo.id, true);
            }
        }
        let before_albums: std::collections::HashMap<_, _> = before
            .albums
            .iter()
            .map(|album| (album.id.as_str(), album))
            .collect();
        let after_album_ids: std::collections::HashSet<_> =
            after.albums.iter().map(|album| album.id.as_str()).collect();
        let mut ordered_albums: std::collections::HashSet<_> =
            Self::string_values(doc, &album_order).into_iter().collect();
        for album in &after.albums {
            let old = before_albums.get(album.id.as_str()).copied();
            if old.is_none() {
                let _ = doc.delete(&removed_albums, &album.id);
            }
            Self::write_album_delta(doc, &albums, old, album);
            if ordered_albums.insert(album.id.clone()) {
                let _ = doc.insert(&album_order, doc.length(&album_order), &album.id);
            }
        }
        for album in &before.albums {
            if !after_album_ids.contains(album.id.as_str()) {
                let _ = doc.put(&removed_albums, &album.id, true);
            }
        }
        Self::sync_metadata_index(doc, root, after);
    }

    /// Update the compact exact-lookup projection after a library write.
    /// This retains labels/counts only, never a second photo or resource tree.
    fn sync_metadata_index(doc: &mut AutoCommit, root: &ObjId, library: &PhotoLibrary) {
        let mut folders = std::collections::BTreeMap::<String, usize>::new();
        let mut people = std::collections::BTreeMap::<String, usize>::new();
        let mut tags = std::collections::BTreeMap::<String, usize>::new();
        let mut objects = std::collections::BTreeMap::<String, usize>::new();
        let mut pets = std::collections::BTreeMap::<String, usize>::new();
        for photo in &library.photos {
            if let Some(folder) = &photo.folder {
                *folders.entry(folder.clone()).or_default() += 1;
            }
            for person in &photo.people {
                *people.entry(person.clone()).or_default() += 1;
            }
            for tag in &photo.tags {
                *tags.entry(tag.clone()).or_default() += 1;
            }
            if let Some(analysis) = &photo.analysis {
                for object in &analysis.objects {
                    *objects.entry(object.clone()).or_default() += 1;
                }
                for pet in &analysis.pets {
                    *pets.entry(pet.clone()).or_default() += 1;
                }
            }
        }
        Self::sync_metadata_kind(doc, root, METADATA_FOLDERS_KEY, "photos.folder:", folders);
        Self::sync_metadata_kind(doc, root, METADATA_PEOPLE_KEY, "photos.person:", people);
        Self::sync_metadata_kind(doc, root, METADATA_TAGS_KEY, "photos.tag:", tags);
        Self::sync_metadata_kind(doc, root, METADATA_OBJECTS_KEY, "photos.object:", objects);
        Self::sync_metadata_kind(doc, root, METADATA_PETS_KEY, "photos.pet:", pets);
    }

    fn sync_metadata_kind(
        doc: &mut AutoCommit,
        root: &ObjId,
        key: &str,
        prefix: &str,
        labels: std::collections::BTreeMap<String, usize>,
    ) {
        let Some(index) = Self::map_or_make(doc, root, key) else {
            return;
        };
        let entries: std::collections::BTreeMap<_, _> = labels
            .into_iter()
            .map(|(label, count)| {
                (
                    metadata_resource_id(prefix, &label),
                    MetadataIndexEntry { label, count },
                )
            })
            .collect();
        let stale: Vec<_> = doc
            .keys(&index)
            .filter(|id| !entries.contains_key(id))
            .collect();
        for id in stale {
            let _ = doc.delete(&index, id);
        }
        for (id, entry) in entries {
            if Self::conflicting_at::<MetadataIndexEntry>(doc, &index, &id).first() != Some(&entry)
            {
                Self::write_json_at(doc, &index, &id, &entry);
            }
        }
    }

    fn write_columnar_photo_delta(
        doc: &mut AutoCommit,
        columns: &PhotoColumns,
        old: Option<&LibraryPhoto>,
        photo: &LibraryPhoto,
    ) {
        let PhotoColumns {
            core,
            favorite,
            folder,
            people,
            tags,
            captured,
            analysis,
            analysis_ocr,
            analysis_vision,
            location,
            crop,
            rotation,
        } = columns;
        if old.is_none_or(|value| value.image != photo.image) {
            Self::write_json_at(doc, core, &photo.id, &photo.image);
        }
        // Fresh imports carry only immutable core + order when metadata is at
        // its model default; retain non-default imported annotations.
        let Some(old) = old else {
            if photo.favorite {
                Self::write_json_at(doc, favorite, &photo.id, &true);
            }
            if photo.folder.is_some() {
                Self::write_json_at(doc, folder, &photo.id, &photo.folder);
            }
            if !photo.people.is_empty() {
                Self::write_label_delta(doc, people, &photo.id, &[], &photo.people);
            }
            if !photo.tags.is_empty() {
                Self::write_label_delta(doc, tags, &photo.id, &[], &photo.tags);
            }
            if photo.captured_at.is_some() {
                Self::write_json_at(doc, captured, &photo.id, &photo.captured_at);
            }
            if photo.analysis.is_some() {
                Self::write_json_at(doc, analysis, &photo.id, &photo.analysis);
                Self::write_json_at(
                    doc,
                    analysis_ocr,
                    &photo.id,
                    &photo.analysis.as_ref().and_then(Self::ocr_component),
                );
                Self::write_json_at(
                    doc,
                    analysis_vision,
                    &photo.id,
                    &photo.analysis.as_ref().and_then(Self::vision_component),
                );
            }
            if photo.location.is_some() {
                Self::write_json_at(doc, location, &photo.id, &photo.location);
            }
            if !photo.edits.crop.is_full() {
                Self::write_json_at(doc, crop, &photo.id, &photo.edits.crop);
            }
            if photo.edits.rotate_quarters != 0 {
                Self::write_json_at(doc, rotation, &photo.id, &photo.edits.rotate_quarters);
            }
            return;
        };
        if old.edits.crop != photo.edits.crop {
            Self::write_json_at(doc, crop, &photo.id, &photo.edits.crop);
        }
        if old.edits.rotate_quarters != photo.edits.rotate_quarters {
            Self::write_json_at(doc, rotation, &photo.id, &photo.edits.rotate_quarters);
        }
        if old.favorite != photo.favorite {
            Self::write_json_at(doc, favorite, &photo.id, &photo.favorite);
        }
        if old.folder != photo.folder {
            Self::write_json_at(doc, folder, &photo.id, &photo.folder);
        }
        if old.people != photo.people {
            Self::write_label_delta(doc, people, &photo.id, &old.people, &photo.people);
        }
        if old.tags != photo.tags {
            Self::write_label_delta(doc, tags, &photo.id, &old.tags, &photo.tags);
        }
        if old.captured_at != photo.captured_at {
            Self::write_json_at(doc, captured, &photo.id, &photo.captured_at);
        }
        if old.analysis != photo.analysis {
            Self::write_json_at(doc, analysis, &photo.id, &photo.analysis);
        }
        let old_ocr = old.analysis.as_ref().and_then(Self::ocr_component);
        let next_ocr = photo.analysis.as_ref().and_then(Self::ocr_component);
        if old_ocr != next_ocr {
            Self::write_json_at(doc, analysis_ocr, &photo.id, &next_ocr);
        }
        let old_vision = old.analysis.as_ref().and_then(Self::vision_component);
        let next_vision = photo.analysis.as_ref().and_then(Self::vision_component);
        if old_vision != next_vision {
            Self::write_json_at(doc, analysis_vision, &photo.id, &next_vision);
        }
        if old.location != photo.location {
            Self::write_json_at(doc, location, &photo.id, &photo.location);
        }
    }

    fn write_album_delta(
        doc: &mut AutoCommit,
        albums: &ObjId,
        old: Option<&crate::model::Album>,
        album: &crate::model::Album,
    ) {
        let Some(row) = Self::map_or_make(doc, albums, &album.id) else {
            return;
        };
        if old.is_none_or(|value| value.name != album.name) {
            let _ = doc.put(&row, "name", &album.name);
        }
        if old.map(|value| &value.photo_ids) != Some(&album.photo_ids) {
            Self::write_json_at(doc, &row, "photo_ids", &album.photo_ids);
        }
    }

    fn string_values(doc: &AutoCommit, list: &ObjId) -> Vec<String> {
        (0..doc.length(list))
            .filter_map(|index| read_str(doc, list, index))
            .collect()
    }

    // -- scalar helpers ----------------------------------------------------

    fn write_json_at<T: serde::Serialize>(
        doc: &mut AutoCommit,
        parent: &ObjId,
        key: &str,
        value: &T,
    ) {
        if let Ok(serialized) = serde_json::to_string(value) {
            let _ = doc.put(parent, key, serialized);
        }
    }

    /// Every value `key` of `parent` currently holds, parsed.
    ///
    /// Two replicas that write the same key without having seen each other's
    /// write leave both values in the document; `get` would return one by
    /// actor id, a coin toss nobody chose. Readers choose instead on what the
    /// values *say* (a real crop beats an untouched one), which holds
    /// whatever order the documents merged in. It stays convergent because
    /// `get_all` orders its answers by op id, which every replica sees
    /// identically. A write that *saw* the other one is not a conflict: it
    /// supersedes it, so a reset still does exactly what it says.
    fn conflicting_at<T: serde::de::DeserializeOwned>(
        doc: &AutoCommit,
        parent: &ObjId,
        key: &str,
    ) -> Vec<T> {
        let Ok(values) = doc.get_all(parent, key) else {
            return Vec::new();
        };
        values
            .into_iter()
            .filter_map(|(value, _)| match value {
                Value::Scalar(s) => s.as_str().and_then(|raw| serde_json::from_str(raw).ok()),
                Value::Object(_) => None,
            })
            .collect()
    }
}

/// What tests build their documents with: a library holding a photograph,
/// and its edits, written as the actions do.
#[cfg(test)]
impl PhotosDoc {
    /// Put the photograph `image` is in the library, returning its id.
    pub(crate) fn put_image(doc: &mut AutoCommit, image: &OpenImage) -> String {
        let photo = LibraryPhoto::from_image(image.clone());
        let id = photo.id.clone();
        let mut library = Self::library(doc);
        library.add_photo(photo);
        Self::set_library(doc, &library);
        id
    }

    /// The edits of photograph `id`; untouched when it is not in the library.
    pub(crate) fn photo_edits(doc: &AutoCommit, id: &str) -> Edits {
        Self::photo(doc, id)
            .map(|photo| photo.edits)
            .unwrap_or_default()
    }

    /// Give photograph `id` `edits`.
    pub(crate) fn set_photo_edits(doc: &mut AutoCommit, id: &str, edits: Edits) {
        let mut library = Self::library(doc);
        if let Some(photo) = library.photo_mut(id) {
            photo.edits = edits;
            Self::set_library(doc, &library);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::PhotosAction;
    use crate::model::Rect01;
    use tilo_actions::Action;
    use tilo_automerge::ApplyIncoming;

    /// A library holding `image`, and the id it gave it.
    fn with_photo(image: OpenImage) -> (AutoCommit, String) {
        let mut doc = AutoCommit::new();
        let photo = LibraryPhoto::from_image(image);
        let id = photo.id.clone();
        let mut library = PhotosDoc::library(&doc);
        library.add_photo(photo);
        PhotosDoc::set_library(&mut doc, &library);
        (doc, id)
    }

    fn set_edits(doc: &mut AutoCommit, id: &str, edits: Edits) {
        PhotosAction::set(id, &edits)
            .apply(doc)
            .expect("an edit of a library photograph applies");
    }

    fn edits(doc: &AutoCommit, id: &str) -> Edits {
        PhotosDoc::photo(doc, id).expect("the photograph").edits
    }

    #[test]
    fn photographs_keep_their_own_edits_and_never_carry_pixels_or_origin_paths() {
        let doc = AutoCommit::new();
        assert!(PhotosDoc::is_empty(&doc));
        assert!(
            PhotosDoc::library(&doc).photos.is_empty(),
            "a fresh document must not fabricate a photograph"
        );

        // Editing one photograph leaves every other photograph alone: a crop
        // of the harbour survives opening, importing and editing another.
        {
            let (mut doc, harbour) = with_photo(OpenImage::from_path("/x/harbour.png", 800, 600));
            set_edits(&mut doc, &harbour, Edits::default().rotated_right());
            let other = LibraryPhoto::from_image(OpenImage::from_path("/x/other.png", 10, 10));
            let other_id = other.id.clone();
            let mut library = PhotosDoc::library(&doc);
            library.add_photo(other);
            PhotosDoc::set_library(&mut doc, &library);
            assert!(
                edits(&doc, &other_id).is_identity(),
                "a new photograph starts untouched"
            );
            set_edits(
                &mut doc,
                &other_id,
                Edits::default().cropped_from_view(Rect01 {
                    x: 0.1,
                    y: 0.1,
                    w: 0.5,
                    h: 0.5,
                }),
            );
            assert_eq!(edits(&doc, &harbour).rotate_quarters, 1);
            assert!(edits(&doc, &harbour).crop.is_full());
            assert!(!edits(&doc, &other_id).crop.is_full());
            assert!(
                PhotosAction::set("photos.photo:gone", &Edits::default())
                    .apply(&mut doc)
                    .is_err(),
                "an edit of a photograph the library lacks refuses"
            );
        }

        {
            let origin = "/var/home/alice/Private/harbour.png";
            let hash = "a".repeat(64);
            let (mut doc, id) =
                with_photo(OpenImage::from_path(origin, 800, 600).with_content(hash.clone()));
            let photo = PhotosDoc::photo(&doc, &id).expect("imported photograph");
            assert_eq!(photo.image.path, "harbour.png");
            assert_eq!(photo.image.content.as_deref(), Some(hash.as_str()));
            assert!(!String::from_utf8_lossy(&doc.save()).contains(origin));
        }

        // The invariant the content store exists to protect, asserted here rather
        // than assumed: a document holding an edited photograph is still tiny,
        // because the pixels are not in it.
        {
            let (mut doc, id) = with_photo(OpenImage::from_path(
                "/var/tmp/tilo/home/user/harbour.png",
                4000,
                3000,
            ));
            set_edits(&mut doc, &id, Edits::default().rotated_right());
            let saved = doc.save().len();
            assert!(
                saved < 4096,
                "the document grew to {saved} bytes; an image must never travel inside a change"
            );
        }
    }

    #[test]
    fn a_photographs_crop_and_turn_merge_independently_and_reset_supersedes_what_it_saw() {
        let e = Edits {
            crop: Rect01 {
                x: 0.25,
                y: 0.5,
                w: 0.5,
                h: 0.25,
            },
            rotate_quarters: 3,
        };
        let (mut doc, id) = with_photo(OpenImage::from_path("/x/harbour.png", 800, 600));
        set_edits(&mut doc, &id, e);
        assert_eq!(edits(&doc, &id), e);

        set_edits(&mut doc, &id, Edits::default());
        let mut cropper = fork(&mut doc, 1);
        let mut rotator = fork(&mut doc, 2);
        let crop = Rect01 {
            x: 0.2,
            y: 0.1,
            w: 0.6,
            h: 0.75,
        };
        set_edits(
            &mut cropper,
            &id,
            Edits {
                crop,
                ..Edits::default()
            },
        );
        set_edits(&mut rotator, &id, Edits::default().rotated_right());

        let mut merged = copy(&mut cropper);
        merged
            .merge_incoming(&mut copy(&mut rotator))
            .expect("independent edit fields merge");
        let both = edits(&merged, &id);
        assert!((both.crop.x - crop.x).abs() <= f64::EPSILON);
        assert!((both.crop.w - crop.w).abs() <= f64::EPSILON);
        assert_eq!(both.rotate_quarters, 1);

        // A reset that saw both supersedes them.
        set_edits(&mut merged, &id, Edits::default());
        assert!(edits(&merged, &id).is_identity());
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn library_records_merge_independent_photo_fields() {
        {
            use crate::actions::{MetadataLabelsPatch, MetadataPatch};

            // Two offline replicas add the same photograph, each creating
            // its own membership map for it, and then tag it differently.
            let mut base = replica(1);
            let item = photo("concurrent-labels");
            let mut left = fork(&mut base, 2u8);
            let mut right = fork(&mut base, 3u8);
            for doc in [&mut left, &mut right] {
                PhotosDoc::set_library(
                    doc,
                    &PhotoLibrary {
                        photos: vec![item.clone()],
                        ..Default::default()
                    },
                );
            }
            for (doc, tag) in [(&mut left, "left"), (&mut right, "right")] {
                PhotosAction::SetMetadataBatch {
                    photo_ids: vec![item.id.clone()],
                    folder: MetadataPatch::Keep,
                    people: MetadataLabelsPatch::Keep,
                    tags: MetadataLabelsPatch::Add(vec![tag.into()]),
                    captured_at: MetadataPatch::Keep,
                    location: MetadataPatch::Keep,
                }
                .apply(doc)
                .expect("offline tagging applies");
            }

            let mut left_first = copy(&mut left);
            let mut right_for_left = copy(&mut right);
            let left_result = merge(&mut left_first, &mut right_for_left);
            let mut right_first = copy(&mut right);
            let mut left_for_right = copy(&mut left);
            let right_result = merge(&mut right_first, &mut left_for_right);
            for library in [left_result, right_result] {
                assert_eq!(
                    library.photo(&item.id).expect("photo").tags,
                    ["left", "right"],
                    "conflicting membership maps must be unioned in either merge order"
                );
            }
        }

        {
            let first = photo("first");
            let second = photo("second");
            let mut base = replica(1);
            PhotosDoc::set_library(
                &mut base,
                &PhotoLibrary {
                    photos: vec![first.clone(), second.clone()],
                    ..Default::default()
                },
            );
            let mut favorite = fork(&mut base, 2);
            let mut metadata = fork(&mut base, 3);
            let mut left = PhotosDoc::library(&favorite);
            left.photo_mut(&first.id).expect("first").favorite = true;
            PhotosDoc::set_library(&mut favorite, &left);
            let mut right = PhotosDoc::library(&metadata);
            right.photo_mut(&second.id).expect("second").tags = vec!["sunset".into()];
            PhotosDoc::set_library(&mut metadata, &right);

            let merged = merge(&mut favorite, &mut metadata);
            assert!(merged.photo(&first.id).expect("first").favorite);
            assert_eq!(merged.photo(&second.id).expect("second").tags, ["sunset"]);
        }

        {
            let item = photo("same");
            let item_id = item.id.clone();
            let mut base = replica(1);
            PhotosDoc::set_library(
                &mut base,
                &PhotoLibrary {
                    photos: vec![item],
                    ..Default::default()
                },
            );
            let mut favorite = fork(&mut base, 2);
            let mut metadata = fork(&mut base, 3);
            let mut left = PhotosDoc::library(&favorite);
            left.photo_mut(&item_id).expect("photo").favorite = true;
            PhotosDoc::set_library(&mut favorite, &left);
            let mut right = PhotosDoc::library(&metadata);
            right.photo_mut(&item_id).expect("photo").folder = Some("Travel".into());
            PhotosDoc::set_library(&mut metadata, &right);

            let merged = merge(&mut favorite, &mut metadata);
            let item = merged.photo(&item_id).expect("photo");
            assert!(item.favorite);
            assert_eq!(item.folder.as_deref(), Some("Travel"));
        }

        {
            let item = photo("concurrent-analysis");
            let item_id = item.id.clone();
            let content = item.image.content.clone().unwrap();
            let mut base = replica(1);
            PhotosDoc::set_library(
                &mut base,
                &PhotoLibrary {
                    photos: vec![item],
                    ..Default::default()
                },
            );
            let mut ocr = fork(&mut base, 2);
            let mut vision = fork(&mut base, 3);

            let ocr_provenance = crate::model::AnalysisProvenance {
                source: "local-ocr".into(),
                engine: "tilo-ocr".into(),
                language: "eng".into(),
                content: content.clone(),
                scope: "shared".into(),
                ..Default::default()
            };
            let mut library = PhotosDoc::library(&ocr);
            library.photo_mut(&item_id).unwrap().analysis = Some(ImageAnalysis {
                ocr_text: "concurrent receipt".into(),
                provenance: ocr_provenance.clone(),
                ocr_provenance: Some(ocr_provenance),
                ..Default::default()
            });
            PhotosDoc::set_library(&mut ocr, &library);

            let vision_provenance = crate::model::AnalysisProvenance {
                source: "local-vision".into(),
                engine: "tilo-vision".into(),
                model_id: "signed-model".into(),
                model_revision: "a".repeat(64),
                content,
                scope: "shared".into(),
                ..Default::default()
            };
            let mut library = PhotosDoc::library(&vision);
            library.photo_mut(&item_id).unwrap().analysis = Some(ImageAnalysis {
                description: "A bicycle beside a lake".into(),
                objects: vec!["bicycle".into()],
                provenance: vision_provenance.clone(),
                vision_provenance: Some(vision_provenance),
                ..Default::default()
            });
            PhotosDoc::set_library(&mut vision, &library);

            let mut left = copy(&mut ocr);
            let mut right_for_left = copy(&mut vision);
            let left_result = merge(&mut left, &mut right_for_left);
            let mut right = copy(&mut vision);
            let mut left_for_right = copy(&mut ocr);
            let right_result = merge(&mut right, &mut left_for_right);
            for result in [left_result, right_result] {
                let analysis = result.photo(&item_id).unwrap().analysis.as_ref().unwrap();
                assert_eq!(analysis.ocr_text, "concurrent receipt");
                assert_eq!(analysis.description, "A bicycle beside a lake");
                assert_eq!(analysis.objects, ["bicycle"]);
                assert_eq!(
                    analysis.ocr_provenance.as_ref().unwrap().source,
                    "local-ocr"
                );
                assert_eq!(
                    analysis.vision_provenance.as_ref().unwrap().source,
                    "local-vision"
                );
            }
        }
    }

    fn photo(id: &str) -> LibraryPhoto {
        let hash = tilo_content::hash_bytes(id.as_bytes());
        let mut image = OpenImage::from_path(&format!("/private/{id}.png"), 800, 600);
        image.name = format!("{id}.png");
        image.content = Some(hash);
        LibraryPhoto::from_image(image)
    }

    fn merge(left: &mut AutoCommit, right: &mut AutoCommit) -> PhotoLibrary {
        left.merge_incoming(right).expect("replicas merge");
        PhotosDoc::library(left)
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn albums_library_roots_and_deletes_converge_across_replicas() {
        let item = photo("member");
        let item_id = item.id.clone();
        let album = crate::model::Album {
            id: "photos.album:trip".into(),
            name: "Trip".into(),
            photo_ids: vec![],
        };
        let mut base = replica(1);
        PhotosDoc::set_library(
            &mut base,
            &PhotoLibrary {
                photos: vec![item],
                albums: vec![album.clone()],
            },
        );
        let mut membership = fork(&mut base, 2);
        let mut rename = fork(&mut base, 3);
        let mut left = PhotosDoc::library(&membership);
        left.album_mut(&album.id)
            .expect("album")
            .photo_ids
            .push(item_id.clone());
        PhotosDoc::set_library(&mut membership, &left);
        let mut right = PhotosDoc::library(&rename);
        right.album_mut(&album.id).expect("album").name = "Summer trip".into();
        PhotosDoc::set_library(&mut rename, &right);

        let merged = merge(&mut membership, &mut rename);
        let album = merged.album(&album.id).expect("album");
        assert_eq!(album.name, "Summer trip");
        assert_eq!(album.photo_ids, [item_id]);

        {
            let first = photo("first-member");
            let second = photo("second-member");
            let third = photo("third-member");
            let album_id = "photos.album:shared".to_owned();
            let mut base = replica(1);
            PhotosDoc::set_library(
                &mut base,
                &PhotoLibrary {
                    photos: vec![first.clone(), second.clone(), third.clone()],
                    albums: vec![crate::model::Album {
                        id: album_id.clone(),
                        name: "Shared".into(),
                        photo_ids: vec![first.id],
                    }],
                },
            );
            let mut left = fork(&mut base, 2);
            let mut right = fork(&mut base, 3);
            let left_members = vec![second.id];
            let right_members = vec![third.id];
            let mut library = PhotosDoc::library(&left);
            library.album_mut(&album_id).expect("album").photo_ids = left_members.clone();
            PhotosDoc::set_library(&mut left, &library);
            let mut library = PhotosDoc::library(&right);
            library.album_mut(&album_id).expect("album").photo_ids = right_members.clone();
            PhotosDoc::set_library(&mut right, &library);

            let mut left_first = copy(&mut left);
            let mut right_for_left = copy(&mut right);
            let merged_left = merge(&mut left_first, &mut right_for_left);
            let mut right_first = copy(&mut right);
            let mut left_for_right = copy(&mut left);
            let merged_right = merge(&mut right_first, &mut left_for_right);
            let left_result = &merged_left.album(&album_id).expect("album").photo_ids;
            let right_result = &merged_right.album(&album_id).expect("album").photo_ids;
            assert_eq!(left_result, right_result, "merge order must not matter");
            assert!(
                left_result == &left_members || left_result == &right_members,
                "absolute replacements must not interleave into {left_result:?}"
            );
        }

        {
            let mut left = replica(1);
            let mut right = replica(2);
            let left_photo = photo("left-root");
            let right_photo = photo("right-root");
            PhotosDoc::set_library(
                &mut left,
                &PhotoLibrary {
                    photos: vec![left_photo.clone()],
                    ..Default::default()
                },
            );
            PhotosDoc::set_library(
                &mut right,
                &PhotoLibrary {
                    photos: vec![right_photo.clone()],
                    ..Default::default()
                },
            );
            left.merge_incoming(&mut right)
                .expect("independently created library roots merge");
            let merged = PhotosDoc::library(&left);
            assert!(merged.photo(&left_photo.id).is_some());
            assert!(merged.photo(&right_photo.id).is_some());

            let library = PhotosDoc::library(&left);
            PhotosDoc::set_library(&mut left, &library);

            assert_eq!(PhotosDoc::library_roots(&left).len(), 1);
            let consolidated = PhotosDoc::library(&left);
            assert!(consolidated.photo(&left_photo.id).is_some());
            assert!(consolidated.photo(&right_photo.id).is_some());
        }

        {
            let item = photo("removed");
            let mut base = replica(1);
            PhotosDoc::set_library(
                &mut base,
                &PhotoLibrary {
                    photos: vec![item],
                    ..Default::default()
                },
            );
            let mut delete = fork(&mut base, 2);
            let mut stale_import = fork(&mut base, 3);
            let importing = PhotosDoc::library(&stale_import).photos[0].clone();
            let importing_id = importing.id.clone();
            PhotosAction::RemovePhoto {
                photo_id: importing_id,
            }
            .apply(&mut delete)
            .expect("canonical delete applies");
            assert!(PhotosDoc::library(&delete).photos.is_empty());
            // This models an import from a replica that still held the same
            // content-addressed id. It does not get to resurrect a delete it has
            // not observed; explicit re-import after observing the tombstone does.
            PhotosAction::ImportPhoto { photo: importing }
                .apply(&mut stale_import)
                .expect("canonical stale import remains replayable");

            let merged = merge(&mut delete, &mut stale_import);
            assert!(merged.photos.is_empty());
        }
    }

    /// Another participant's replica of `doc`, with its own fixed actor id.
    fn fork(doc: &mut AutoCommit, actor: u8) -> AutoCommit {
        let mut replica = copy(doc);
        replica.set_actor(automerge::ActorId::from([actor; 16].as_slice()));
        replica
    }

    /// A copy of `doc` as another machine would hold it, for merging back.
    fn copy(doc: &mut AutoCommit) -> AutoCommit {
        AutoCommit::load(&doc.save()).expect("a document this side just wrote must load")
    }

    /// One replica, with an actor id fixed so the test decides the tie rather
    /// than the random id the run happens to mint.
    fn replica(actor: u8) -> AutoCommit {
        let mut doc = AutoCommit::new();
        doc.set_actor(automerge::ActorId::from([actor; 16].as_slice()));
        doc
    }
}
