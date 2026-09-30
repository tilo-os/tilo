//! Automerge document layer.
//!
//! What the user decided lives in the CRDT: which file is open, and the edits
//! applied to it. That makes every edit undoable through `State::commit_action`
//! and replicable to a collaborator for free.
//!
//! ```text
//! root
//! ├── "image"  (Str)   # serde_json of Option<OpenImage> — atomic editor selection
//! ├── "photos_edits" (Map) # independently mergeable crop + quarter turns
//! └── "photos_library" (Map) # keyed photo/album records, order lists, delete tombstones
//! ```
//!
//! # What is deliberately not here
//!
//! The pixels. The document holds portable display metadata and two small edit
//! fields and never the image itself, which lives in the app's content store
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
/// Field-granular edits. A crop is
/// one coherent rectangle, while rotation is a separate user decision; the
/// two fields therefore merge independently without pretending that the four
/// coordinates of a single crop can safely be composed.
const EDITS_KEY: &str = "photos_edits";
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
        })
    }
}

impl PhotosDoc {
    /// True when the document has never been written to.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        !matches!(
            doc.get(&ROOT, EDITS_KEY),
            Ok(Some((Value::Object(ObjType::Map), _)))
        )
    }

    /// Seed default state: **no image open** and no edits. An app with nothing
    /// open shows an empty state; content arrives from an opened file.
    /// Idempotent — only writes fields that are missing.
    pub fn seed(doc: &mut AutoCommit) {
        if read_str(doc, &ROOT, "image").is_none() {
            Self::write_json(doc, "image", &Option::<OpenImage>::None);
        }
        let _ = Self::ensure_structured_edits(doc);
        let _ = Self::ensure_structured_library(doc);
    }

    /// The photograph currently open, if any.
    ///
    /// Reads **every** value the key holds, not only the one Automerge would
    /// hand back, and a photograph beats no photograph. See
    /// `Self::conflicting` for why that is the difference between a
    /// collaborator's picture surviving and a collaborator's picture being
    /// closed by somebody else's app starting up.
    pub fn image(doc: &AutoCommit) -> Option<OpenImage> {
        Self::conflicting::<Option<OpenImage>>(doc, "image")
            .into_iter()
            .flatten()
            .next()
    }

    /// Open `image`, resetting the edits — a newly opened file starts
    /// untouched, never inheriting the previous one's crop. The image itself
    /// deliberately remains one atomic selection record: its content identity,
    /// display metadata, and reset boundary must change together.
    pub fn set_image(doc: &mut AutoCommit, image: Option<&OpenImage>) {
        let image = image.cloned().map(OpenImage::portable);
        Self::write_json(doc, "image", &image);
        Self::set_edits(doc, &Edits::default());
        if let Some(image) = &image {
            let mut library = Self::library(doc);
            if library.add_photo(LibraryPhoto::from_image(image.clone())) {
                Self::set_library(doc, &library);
            }
        }
    }

    /// The edits in force.
    ///
    /// Crop and rotation resolve independently. An app that has just started
    /// writing untouched rotation therefore cannot erase a concurrent crop.
    pub fn edits(doc: &AutoCommit) -> Edits {
        let roots = Self::edits_roots(doc);
        if roots.is_empty() {
            return Edits::default();
        }
        // Two just-created replicas can concurrently make the map itself.
        // Read every root-map conflict so their independently written fields
        // still merge; subsequent writes target the deterministic first map.
        let crop = roots
            .iter()
            .flat_map(|root| Self::conflicting_at::<crate::model::Rect01>(doc, root, "crop"))
            .into_iter()
            .find(|crop| !crop.is_full())
            .unwrap_or_default();
        let rotate_quarters = roots
            .iter()
            .flat_map(|root| Self::conflicting_at::<u8>(doc, root, "rotate_quarters"))
            .into_iter()
            .find(|turns| !turns.is_multiple_of(4))
            .unwrap_or_default();
        Edits {
            crop,
            rotate_quarters,
        }
    }

    /// Replace changed edit fields only. This delta is the compatibility seam
    /// for the existing absolute `SetEdits` action: a crop writer updates only
    /// `crop`, while a concurrent rotation writer updates only
    /// `rotate_quarters`.
    pub fn set_edits(doc: &mut AutoCommit, edits: &Edits) {
        let before = Self::edits(doc);
        let Some(root) = Self::ensure_structured_edits(doc) else {
            return;
        };
        if before.crop != edits.crop {
            Self::write_json_at(doc, &root, "crop", &edits.crop);
        }
        if before.rotate_quarters != edits.rotate_quarters {
            Self::write_json_at(doc, &root, "rotate_quarters", &edits.rotate_quarters);
        }
    }

    /// The one edits map, created on first write.
    fn ensure_structured_edits(doc: &mut AutoCommit) -> Option<ObjId> {
        let roots = Self::edits_roots(doc);
        if let [root] = roots.as_slice() {
            return Some(root.clone());
        }
        // Concurrent first-run replicas can each create this map. Once an
        // editor has observed both, replace the competing roots with one map
        // containing their deterministic merged projection. This gives a
        // subsequent explicit Reset normal Automerge overwrite semantics
        // instead of leaving an unseen old map to resurrect a crop forever.
        let inherited = (!roots.is_empty()).then(|| Self::edits(doc));
        if !roots.is_empty() {
            let _ = doc.delete(&ROOT, EDITS_KEY);
        }
        let edits = inherited.unwrap_or_default();
        let root = doc.put_object(&ROOT, EDITS_KEY, ObjType::Map).ok()?;
        Self::write_json_at(doc, &root, "crop", &edits.crop);
        Self::write_json_at(doc, &root, "rotate_quarters", &edits.rotate_quarters);
        Some(root)
    }

    fn edits_roots(doc: &AutoCommit) -> Vec<ObjId> {
        doc.get_all(&ROOT, EDITS_KEY)
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

    /// Resolve a hashed metadata browse id from the compact keyed index.
    ///
    /// Unlike [`Self::library`], this never projects every photograph. The
    /// index is derived on each authoritative library write, so deletion of
    /// the final matching photograph removes the entry and stale exact ids
    /// refuse rather than materialising an empty phantom view.
    pub fn metadata_view(doc: &AutoCommit, id: &str) -> Option<MetadataView> {
        let kinds = [
            (
                MetadataViewKind::Folder,
                METADATA_FOLDERS_KEY,
                "photos.folder:",
            ),
            (
                MetadataViewKind::Person,
                METADATA_PEOPLE_KEY,
                "photos.person:",
            ),
            (MetadataViewKind::Tag, METADATA_TAGS_KEY, "photos.tag:"),
            (
                MetadataViewKind::Object,
                METADATA_OBJECTS_KEY,
                "photos.object:",
            ),
            (MetadataViewKind::Pet, METADATA_PETS_KEY, "photos.pet:"),
        ];
        for root in Self::library_roots(doc) {
            for (kind, key, prefix) in kinds {
                let Some(index) = Self::map_at(doc, &root, key) else {
                    continue;
                };
                let Some(entry) = Self::conflicting_at::<MetadataIndexEntry>(doc, &index, id)
                    .into_iter()
                    .next()
                else {
                    continue;
                };
                if entry.label.trim().is_empty()
                    || entry.label.chars().count() > 128
                    || entry.count == 0
                    || metadata_resource_id(prefix, &entry.label) != id
                {
                    continue;
                }
                return Some(MetadataView {
                    kind,
                    label: entry.label,
                    count: entry.count,
                });
            }
        }
        None
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
        // As with edits, concurrent first-run roots are folded as
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
            return;
        };
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

    fn write_json<T: serde::Serialize>(doc: &mut AutoCommit, key: &str, value: &T) {
        if let Ok(s) = serde_json::to_string(value) {
            let _ = doc.put(&ROOT, key, s);
        }
    }

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

    /// Every value `key` currently holds, parsed.
    ///
    /// # Why a key can hold more than one value
    ///
    /// Two replicas that write the same key without having seen each other's
    /// write leave both values in the document. Automerge keeps them all and
    /// `get` returns one — deterministically, so every replica agrees, but it
    /// decides by actor id, which is a coin toss nobody chose.
    ///
    /// That is not an abstract hazard. An app of this kind starts by writing
    /// what it holds, and a second participant's photo editor starting up
    /// holds nothing: it writes "no photograph", concurrently with the
    /// photograph somebody else has open. Half the time the empty write wins
    /// the toss and the picture disappears off both screens, in the middle of
    /// somebody working on it, with nothing anywhere reporting a loss.
    ///
    /// So the choice is made here instead, on what the values *say* rather
    /// than on who wrote them: **a value with something in it beats one with
    /// nothing in it.** That is a property of the data, so it holds whatever
    /// order the documents merged in and whichever way the actor ids happened
    /// to sort. It stays convergent because `get_all` orders its answers by op
    /// id, which every replica sees identically — two replicas holding the
    /// same conflict make the same choice.
    ///
    /// A write that *saw* the other one is not a conflict at all: it
    /// supersedes it and is the only value here. Closing a photograph, or
    /// resetting a crop, therefore still does exactly what it says.
    fn conflicting<T: serde::de::DeserializeOwned>(doc: &AutoCommit, key: &str) -> Vec<T> {
        Self::conflicting_at(doc, &ROOT, key)
    }

    /// As [`Self::conflicting`], but for fields of an independently mergeable
    /// map instead of a root scalar.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::PhotosAction;
    use crate::model::Rect01;
    use tilo_actions::Action;
    use tilo_automerge::ApplyIncoming;

    #[test]
    fn seeding_and_opening_keep_state_and_never_carry_pixels_or_origin_paths() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        assert!(
            PhotosDoc::image(&doc).is_none(),
            "a fresh document must not fabricate a photograph"
        );
        assert!(PhotosDoc::edits(&doc).is_identity());

        {
            let mut doc = AutoCommit::new();
            PhotosDoc::seed(&mut doc);
            let img = OpenImage::from_path("/x/harbour.png", 800, 600);
            PhotosDoc::set_image(&mut doc, Some(&img));
            PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());

            PhotosDoc::seed(&mut doc);
            assert_eq!(PhotosDoc::image(&doc).unwrap().name, "harbour.png");
            assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 1);
        }

        {
            let mut doc = AutoCommit::new();
            PhotosDoc::seed(&mut doc);
            PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
            let img = OpenImage::from_path("/x/other.png", 10, 10);
            PhotosDoc::set_image(&mut doc, Some(&img));
            assert!(
                PhotosDoc::edits(&doc).is_identity(),
                "a newly opened photograph must start untouched"
            );
        }

        {
            let mut doc = AutoCommit::new();
            PhotosDoc::seed(&mut doc);
            let origin = "/var/home/alice/Private/harbour.png";
            let hash = "a".repeat(64);
            let image = OpenImage::from_path(origin, 800, 600).with_content(hash.clone());

            PhotosDoc::set_image(&mut doc, Some(&image));

            let image_json = read_str(&doc, &ROOT, "image").expect("image JSON");
            assert!(!image_json.contains(origin));
            assert!(read_str(&doc, &ROOT, "library").is_none());
            let open = PhotosDoc::image(&doc).expect("imported image");
            assert_eq!(open.path, "harbour.png");
            assert_eq!(open.content.as_deref(), Some(hash.as_str()));
            assert_eq!(PhotosDoc::library(&doc).photos[0].image.path, "harbour.png");
        }

        // The invariant the content store exists to protect, asserted here rather
        // than assumed: a document that has an image open is still tiny, because
        // the pixels are not in it.
        {
            let mut doc = AutoCommit::new();
            PhotosDoc::seed(&mut doc);
            let img = OpenImage::from_path("/var/tmp/tilo/home/user/harbour.png", 4000, 3000);
            PhotosDoc::set_image(&mut doc, Some(&img));
            PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
            let saved = doc.save().len();
            assert!(
                saved < 4096,
                "the document grew to {saved} bytes; an image must never travel inside a change"
            );
        }
    }

    #[test]
    fn edits_round_trip_and_merge_at_separate_fields() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let e = Edits {
            crop: Rect01 {
                x: 0.25,
                y: 0.5,
                w: 0.5,
                h: 0.25,
            },
            rotate_quarters: 3,
        };
        PhotosDoc::set_edits(&mut doc, &e);
        assert_eq!(PhotosDoc::edits(&doc), e);
        assert!(read_str(&doc, &ROOT, "edits").is_none());
        assert!(!PhotosDoc::edits_roots(&doc).is_empty());

        {
            let mut cropper = replica(1);
            let mut rotator = fork(&mut cropper, 2u8);
            let crop = Rect01 {
                x: 0.2,
                y: 0.1,
                w: 0.6,
                h: 0.75,
            };
            PhotosDoc::set_edits(
                &mut cropper,
                &Edits {
                    crop,
                    ..Edits::default()
                },
            );
            PhotosDoc::set_edits(&mut rotator, &Edits::default().rotated_right());

            let mut merged = copy(&mut cropper);
            merged
                .merge_incoming(&mut copy(&mut rotator))
                .expect("independent edit fields merge");
            let edits = PhotosDoc::edits(&merged);
            assert!((edits.crop.x - crop.x).abs() <= f64::EPSILON);
            assert!((edits.crop.y - crop.y).abs() <= f64::EPSILON);
            assert!((edits.crop.w - crop.w).abs() <= f64::EPSILON);
            assert!((edits.crop.h - crop.h).abs() <= f64::EPSILON);
            assert_eq!(edits.rotate_quarters, 1);
        }

        {
            let mut cropper = replica(1);
            let mut rotator = fork(&mut cropper, 2u8);
            PhotosDoc::set_edits(
                &mut cropper,
                &Edits {
                    crop: Rect01 {
                        x: 0.2,
                        y: 0.2,
                        w: 0.5,
                        h: 0.5,
                    },
                    ..Edits::default()
                },
            );
            PhotosDoc::set_edits(&mut rotator, &Edits::default().rotated_right());
            let mut observed = copy(&mut cropper);
            observed
                .merge_incoming(&mut copy(&mut rotator))
                .expect("both edit roots merge for the observing editor");
            assert!(!PhotosDoc::edits(&observed).is_identity());

            PhotosDoc::set_edits(&mut observed, &Edits::default());

            assert!(PhotosDoc::edits(&observed).is_identity());
            assert_eq!(PhotosDoc::edits_roots(&observed).len(), 1);
        }
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

            PhotosDoc::seed(&mut left);

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
        PhotosDoc::seed(&mut doc);
        doc
    }

    /// A participant's editor starting up must not close the photograph
    /// another participant is working on.
    ///
    /// This is data loss between collaborators, and it happened: a second
    /// person's photo editor came up holding nothing, wrote "no photograph"
    /// into the room's document concurrently with the picture that was open in
    /// it, and the picture left both screens mid-edit — the app went from four
    /// operations to none, and the participant that had asked for a crop was
    /// left addressing a window with nothing in it.
    ///
    /// Every way the tie can fall, because none of them is a choice anybody
    /// made. Automerge resolves two concurrent writes to one key by the op's
    /// Lamport stamp — its counter first, the actor id second. The counter is
    /// just how much history that replica's document happens to carry, which
    /// for an app is however much its toolkit wrote before the app itself
    /// wrote anything; the actor id is random. So the empty replica is run
    /// both behind and ahead of the populated one, under both actor
    /// orderings, and both merge directions are checked because it is one
    /// document afterwards and both participants have to be looking at it.
    #[test]
    fn the_room_photograph_survives_an_empty_replica_but_not_a_seen_close() {
        for (open_actor, empty_actor) in [(1u8, 2u8), (2u8, 1u8)] {
            for empty_is_ahead in [false, true] {
                run_the_empty_replica_against_the_open_one(open_actor, empty_actor, empty_is_ahead);
            }
        }

        // And the rule is about concurrency, not about refusing to let go.
        //
        // Closing a photograph is a write that *saw* the open one, so it
        // supersedes it rather than competing with it — there is one value under
        // the key afterwards and it is the close. Without this the fix above
        // would read as "a photograph can never be closed once any replica has
        // seen it", which would be a worse defect than the one it replaces.
        {
            let mut here = replica(1);
            let img = OpenImage::from_path("/var/tmp/tilo/workspace/harbour.png", 800, 600);
            PhotosDoc::set_image(&mut here, Some(&img));
            PhotosDoc::set_edits(&mut here, &Edits::default().rotated_right());

            // A collaborator, holding the same document, closes it and resets the
            // turn — both after seeing what was there.
            let mut there = fork(&mut here, 2u8);
            PhotosDoc::set_image(&mut there, None);

            let mut back = copy(&mut here);
            back.merge_incoming(&mut copy(&mut there))
                .expect("the collaborator's document must merge back");
            assert!(
                PhotosDoc::image(&back).is_none(),
                "a close that saw the open supersedes it; the photograph must go",
            );
            assert!(
                PhotosDoc::edits(&back).is_identity(),
                "and opening or closing a photograph resets the edits with it",
            );
        }
    }

    #[allow(
        clippy::useless_vec,
        reason = "see the comment on the loop below: a fixed-size array of two full \
                  AutoCommit replicas would put this stack frame over clippy's own \
                  large-array threshold"
    )]
    fn run_the_empty_replica_against_the_open_one(
        open_actor: u8,
        empty_actor: u8,
        empty_is_ahead: bool,
    ) {
        {
            // Somebody with a picture open, and part-way through editing it.
            let mut has_picture = replica(open_actor);
            let img = OpenImage::from_path("/var/tmp/tilo/workspace/harbour.png", 800, 600);
            PhotosDoc::set_image(&mut has_picture, Some(&img));
            PhotosDoc::set_edits(&mut has_picture, &Edits::default().rotated_right());

            // Somebody else's editor, just started: its own history, nothing
            // in it, and it has never seen the picture.
            let mut just_started = AutoCommit::new();
            just_started.set_actor(automerge::ActorId::from([empty_actor; 16].as_slice()));
            if empty_is_ahead {
                // The history that replica carried before the app wrote a
                // line of its own — a toolkit seeding its containers, a
                // session that had been up longer. Nobody chose it, and
                // without it the empty write loses the Lamport comparison for
                // a reason that has nothing to do with what either document
                // says.
                for n in 0..8i64 {
                    just_started
                        .put(&ROOT, "toolkit-scratch", n)
                        .expect("a scalar put on a fresh document cannot fail");
                }
            }
            PhotosDoc::seed(&mut just_started);

            let clock = if empty_is_ahead { "ahead" } else { "behind" };
            // The room hands each of them the other's whole document. A `Vec`
            // rather than a fixed-size array: each element is a full
            // `AutoCommit` replica, and two of those inline would put this
            // stack frame over clippy's large-array threshold for no benefit
            // — the pair is only ever iterated once, never indexed.
            for (mut doc, mut incoming, side) in vec![
                (
                    copy(&mut has_picture),
                    copy(&mut just_started),
                    "the screen with the picture on it",
                ),
                (
                    copy(&mut just_started),
                    copy(&mut has_picture),
                    "the screen that had just started",
                ),
            ] {
                doc.merge_incoming(&mut incoming)
                    .expect("two replicas of one app's document must merge");
                assert_eq!(
                    PhotosDoc::image(&doc).map(|i| i.name),
                    Some("harbour.png".to_string()),
                    "actors {open_actor}/{empty_actor}, empty replica {clock}: \
                     an editor starting up with nothing open must not take \
                     the photograph off {side}",
                );
                assert_eq!(
                    PhotosDoc::edits(&doc).rotate_quarters,
                    1,
                    "actors {open_actor}/{empty_actor}, empty replica {clock}: \
                     nor undo the turn somebody had already applied to it, \
                     on {side}",
                );
            }
        }
    }
}
