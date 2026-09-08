//! Automerge document layer.
//!
//! What the user decided lives in the CRDT: which file is open, and the edits
//! applied to it. That makes every edit undoable through `State::commit_action`
//! and replicable to a collaborator for free.
//!
//! ```text
//! root
//! ├── "image"  (Str)   # serde_json of Option<OpenImage>  — name + content + intrinsic size
//! ├── "image"  (Str)   # serde_json of Option<OpenImage> — atomic editor selection
//! ├── "photos_edits" (Map) # independently mergeable crop + quarter turns
//! └── "photos_library" (Map) # keyed photo/album records, order lists, delete tombstones
//! ```
//!
//! # What is deliberately not here
//!
//! The pixels. The document holds portable display metadata and two small edit
//! fields and never the image itself, which is the same invariant
//! `crates/doc/qdoc/src/blobstore.rs` documents for a document's blobs: an
//! automerge change carrying a multi-megabyte payload bloats every replica and
//! durable host log and makes later edits wait behind the media. A content
//! address plus a crop rect is a change of a few dozen bytes.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{Edits, LibraryPhoto, OpenImage, PhotoLibrary};

/// The bytes of the picture `open` names, from wherever this machine has them.
///
/// Content first: it is the portable identity every current import writes.
/// Path-only legacy records retain their old local fallback, but a new shared
/// record never causes this device to probe another participant's filesystem.
/// Resolving the hash is what turns "a photograph a collaborator opened" into
/// pixels here rather than into an empty canvas.
///
/// Missing both is not an error to report: `blobstore::get` asks the room for
/// the content on its way through, so the honest answer now is no picture, and
/// the moment after the bytes land there is one.
///
/// Public, and the only answer to this question, because the app is not the
/// only thing that asks it. Anything acting on the open photograph — the
/// editor drawing it, a participant with no window rendering the crop it was
/// asked for — needs the same bytes, and a second copy of "read the path" is
/// how one of those callers ends up reading a path that is true on another
/// machine.
pub fn bytes_of(open: &OpenImage) -> Option<Vec<u8>> {
    if let Some(content) = &open.content {
        return qdoc::blobstore::get(content);
    }
    std::fs::read(&open.path).ok()
}

pub struct PhotosDoc;

/// Structured library projection. The former `library` JSON scalar is read
/// only long enough to migrate old documents; all new library writes use
/// independently mergeable maps and lists below this key.
const LIBRARY_KEY: &str = "photos_library";
/// Field-granular replacement for the legacy `edits` JSON scalar. A crop is
/// one coherent rectangle, while rotation is a separate user decision; the
/// two fields therefore merge independently without pretending that the four
/// coordinates of a single crop can safely be composed.
const EDITS_KEY: &str = "photos_edits";

impl PhotosDoc {
    /// True when the document has never been written to.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        !matches!(
            doc.get(&ROOT, EDITS_KEY),
            Ok(Some((Value::Object(ObjType::Map), _)))
        ) && !matches!(doc.get(&ROOT, "edits"), Ok(Some((Value::Scalar(_), _))))
    }

    /// Seed default state: **no image open** and no edits.
    ///
    /// There is no sample photograph here and no fabricated recent list. An
    /// app with nothing open shows an empty state that says so; content only
    /// ever arrives from a file the user actually opened.
    /// Idempotent — only writes fields that are missing.
    pub fn seed(doc: &mut AutoCommit) {
        if Self::get_str(doc, "image").is_none() {
            Self::write_json(doc, "image", &Option::<OpenImage>::None);
        }
        let _ = Self::ensure_structured_edits(doc);
        let _ = Self::ensure_structured_library(doc);
    }

    /// The photograph currently open, if any.
    ///
    /// Reads **every** value the key holds, not only the one Automerge would
    /// hand back, and a photograph beats no photograph. See
    /// [`Self::conflicting`] for why that is the difference between a
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
            return Self::conflicting::<Edits>(doc, "edits")
                .into_iter()
                .find(|edits| !edits.is_identity())
                .unwrap_or_default();
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

    /// Migrate the legacy whole-edits scalar exactly once, then remove it so
    /// future documents never emit a blob that could make crop and rotation
    /// overwrite one another.
    fn ensure_structured_edits(doc: &mut AutoCommit) -> Option<ObjId> {
        let roots = Self::edits_roots(doc);
        if let [root] = roots.as_slice() {
            if Self::get_str(doc, "edits").is_some() {
                let _ = doc.delete(&ROOT, "edits");
            }
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
        let legacy = Self::conflicting::<Edits>(doc, "edits")
            .into_iter()
            .find(|edits| !edits.is_identity())
            .or(inherited)
            .unwrap_or_default();
        let root = doc.put_object(&ROOT, EDITS_KEY, ObjType::Map).ok()?;
        Self::write_json_at(doc, &root, "crop", &legacy.crop);
        Self::write_json_at(doc, &root, "rotate_quarters", &legacy.rotate_quarters);
        if Self::get_str(doc, "edits").is_some() {
            let _ = doc.delete(&ROOT, "edits");
        }
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
    /// maps. The legacy JSON scalar is a read-only migration input.
    pub fn library(doc: &AutoCommit) -> PhotoLibrary {
        Self::structured_library(doc).unwrap_or_else(|| Self::legacy_library(doc))
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

    fn legacy_library(doc: &AutoCommit) -> PhotoLibrary {
        Self::conflicting::<PhotoLibrary>(doc, "library")
            .into_iter()
            .next()
            .unwrap_or_default()
    }

    /// Create the structured root once, importing then removing a legacy
    /// scalar. Removing the scalar matters: after migration future edits never
    /// rewrite or compete with a whole-library JSON blob.
    fn ensure_structured_library(doc: &mut AutoCommit) -> Option<ObjId> {
        let roots = Self::library_roots(doc);
        if let [root] = roots.as_slice() {
            if Self::get_str(doc, "library").is_some() {
                let _ = doc.delete(&ROOT, "library");
            }
            return Some(root.clone());
        }
        // As with edits, concurrent first-run/migration roots are folded as
        // soon as an editor observes them. Until then `structured_library`
        // reads their additive projection, so no independently imported
        // record is stranded behind Automerge's single-value map lookup.
        let inherited = (!roots.is_empty())
            .then(|| Self::structured_library(doc))
            .flatten();
        if !roots.is_empty() {
            let _ = doc.delete(&ROOT, LIBRARY_KEY);
        }
        let library = inherited.unwrap_or_else(|| Self::legacy_library(doc));
        let root = doc.put_object(&ROOT, LIBRARY_KEY, ObjType::Map).ok()?;
        let _ = Self::map_or_make(doc, &root, "photos");
        let _ = Self::map_or_make(doc, &root, "photo_core");
        let _ = Self::map_or_make(doc, &root, "photo_favorite");
        let _ = Self::map_or_make(doc, &root, "photo_folder");
        let _ = Self::map_or_make(doc, &root, "photo_people");
        let _ = Self::map_or_make(doc, &root, "photo_tags");
        let _ = Self::map_or_make(doc, &root, "photo_captured_at");
        let _ = Self::map_or_make(doc, &root, "albums");
        let _ = Self::map_or_make(doc, &root, "removed_photos");
        let _ = Self::map_or_make(doc, &root, "removed_albums");
        let _ = Self::list_or_make(doc, &root, "photo_order");
        let _ = Self::list_or_make(doc, &root, "album_order");
        if !library.photos.is_empty() || !library.albums.is_empty() {
            Self::sync_library(doc, &root, &PhotoLibrary::default(), &library.portable());
        }
        // A non-empty legacy value has now been copied fieldwise. Delete an
        // empty scalar too, so a newly seeded document has exactly one library
        // representation and no later code can accidentally write the blob.
        if Self::get_str(doc, "library").is_some() {
            let _ = doc.delete(&ROOT, "library");
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
        let photos = Self::map_at(doc, root, "photos")?;
        let photo_core = Self::map_at(doc, root, "photo_core");
        let albums = Self::map_at(doc, root, "albums")?;
        let removed_photos = Self::map_at(doc, root, "removed_photos");
        let removed_albums = Self::map_at(doc, root, "removed_albums");
        let photo_order = Self::string_list(doc, root, "photo_order");
        let album_order = Self::string_list(doc, root, "album_order");

        let mut library = PhotoLibrary::default();
        let photo_ids = photo_core.as_ref().map_or_else(
            || Self::ordered_keys(doc, &photos, photo_order.clone()),
            |core| Self::ordered_keys(doc, core, photo_order.clone()),
        );
        for id in photo_ids {
            if Self::marked_removed(doc, removed_photos.as_ref(), &id) {
                continue;
            }
            if let Some(photo) = photo_core
                .as_ref()
                .and_then(|core| Self::read_columnar_photo(doc, root, core, &id))
                .or_else(|| Self::read_photo(doc, &photos, &id))
            {
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
    /// for a first-run or legacy migration race before either participant has
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

    fn read_photo(doc: &AutoCommit, photos: &ObjId, id: &str) -> Option<LibraryPhoto> {
        let row = Self::map_at(doc, photos, id)?;
        let name = Self::str_at(doc, &row, "name")?;
        let width = Self::int_at(doc, &row, "width").unwrap_or(0).max(0) as u32;
        let height = Self::int_at(doc, &row, "height").unwrap_or(0).max(0) as u32;
        Some(LibraryPhoto {
            id: id.to_owned(),
            image: OpenImage {
                path: Self::str_at(doc, &row, "path").unwrap_or_else(|| name.clone()),
                name,
                width,
                height,
                content: Self::str_at(doc, &row, "content"),
            },
            favorite: Self::bool_at(doc, &row, "favorite").unwrap_or(false),
            folder: Self::str_at(doc, &row, "folder"),
            people: Self::string_list(doc, &row, "people"),
            tags: Self::string_list(doc, &row, "tags"),
            captured_at: Self::str_at(doc, &row, "captured_at"),
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
                .and_then(|map| {
                    Self::conflicting_at::<Vec<String>>(doc, &map, id)
                        .into_iter()
                        .next()
                })
                .unwrap_or_default(),
            tags: field("photo_tags")
                .and_then(|map| {
                    Self::conflicting_at::<Vec<String>>(doc, &map, id)
                        .into_iter()
                        .next()
                })
                .unwrap_or_default(),
            captured_at: field("photo_captured_at").and_then(|map| {
                Self::conflicting_at::<Option<String>>(doc, &map, id)
                    .into_iter()
                    .next()
                    .flatten()
            }),
        })
    }

    fn read_album(doc: &AutoCommit, albums: &ObjId, id: &str) -> Option<crate::model::Album> {
        let row = Self::map_at(doc, albums, id)?;
        Some(crate::model::Album {
            id: id.to_owned(),
            name: Self::str_at(doc, &row, "name")?,
            photo_ids: Self::string_list(doc, &row, "photo_ids"),
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
            .filter_map(|index| match doc.get(&list, index).ok().flatten() {
                Some((Value::Scalar(value), _)) => value.to_str().map(str::to_owned),
                _ => None,
            })
            .collect()
    }

    fn str_at(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<String> {
        match doc.get(parent, key) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Str(value) => Some(value.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn bool_at(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<bool> {
        match doc.get(parent, key) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Boolean(value) => Some(*value),
                _ => None,
            },
            _ => None,
        }
    }

    fn int_at(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<i64> {
        match doc.get(parent, key) {
            Ok(Some((Value::Scalar(value), _))) => value.to_i64(),
            _ => None,
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
        let Some(photos) = Self::map_or_make(doc, root, "photos") else {
            return;
        };
        let Some(photo_core) = Self::map_or_make(doc, root, "photo_core") else {
            return;
        };
        let Some(photo_favorite) = Self::map_or_make(doc, root, "photo_favorite") else {
            return;
        };
        let Some(photo_folder) = Self::map_or_make(doc, root, "photo_folder") else {
            return;
        };
        let Some(photo_people) = Self::map_or_make(doc, root, "photo_people") else {
            return;
        };
        let Some(photo_tags) = Self::map_or_make(doc, root, "photo_tags") else {
            return;
        };
        let Some(photo_captured_at) = Self::map_or_make(doc, root, "photo_captured_at") else {
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
        if before.photos.is_empty() && after.albums.is_empty() && doc.keys(&photos).next().is_none()
        {
            for (order_index, photo) in (doc.length(&photo_order)..).zip(after.photos.iter()) {
                Self::write_columnar_photo_delta(
                    doc,
                    &photo_core,
                    &photo_favorite,
                    &photo_folder,
                    &photo_people,
                    &photo_tags,
                    &photo_captured_at,
                    None,
                    photo,
                );
                let _ = doc.insert(&photo_order, order_index, &photo.id);
            }
            return;
        }
        // Materialize lookup/order sets once. Bulk import and migration must
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
            Self::write_columnar_photo_delta(
                doc,
                &photo_core,
                &photo_favorite,
                &photo_folder,
                &photo_people,
                &photo_tags,
                &photo_captured_at,
                old,
                photo,
            );
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
    }

    #[allow(
        dead_code,
        reason = "kept temporarily as the nested-schema migration reference"
    )]
    fn write_photo_delta(
        doc: &mut AutoCommit,
        photos: &ObjId,
        old: Option<&LibraryPhoto>,
        photo: &LibraryPhoto,
    ) {
        let Some(row) = Self::map_or_make(doc, photos, &photo.id) else {
            return;
        };
        let old_image = old.map(|old| &old.image);
        if old_image.is_none_or(|image| image.path != photo.image.path) {
            let _ = doc.put(&row, "path", &photo.image.path);
        }
        if old_image.is_none_or(|image| image.name != photo.image.name) {
            let _ = doc.put(&row, "name", &photo.image.name);
        }
        if old_image.is_none_or(|image| image.width != photo.image.width) {
            let _ = doc.put(&row, "width", i64::from(photo.image.width));
        }
        if old_image.is_none_or(|image| image.height != photo.image.height) {
            let _ = doc.put(&row, "height", i64::from(photo.image.height));
        }
        if old_image.and_then(|image| image.content.as_ref()) != photo.image.content.as_ref() {
            Self::put_optional_string(doc, &row, "content", photo.image.content.as_deref());
        }
        if old.is_none_or(|value| value.favorite != photo.favorite) {
            let _ = doc.put(&row, "favorite", photo.favorite);
        }
        if old.and_then(|value| value.folder.as_ref()) != photo.folder.as_ref() {
            Self::put_optional_string(doc, &row, "folder", photo.folder.as_deref());
        }
        if old.map(|value| &value.people) != Some(&photo.people) {
            Self::replace_string_list(doc, &row, "people", &photo.people);
        }
        if old.map(|value| &value.tags) != Some(&photo.tags) {
            Self::replace_string_list(doc, &row, "tags", &photo.tags);
        }
        if old.and_then(|value| value.captured_at.as_ref()) != photo.captured_at.as_ref() {
            Self::put_optional_string(doc, &row, "captured_at", photo.captured_at.as_deref());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_columnar_photo_delta(
        doc: &mut AutoCommit,
        core: &ObjId,
        favorite: &ObjId,
        folder: &ObjId,
        people: &ObjId,
        tags: &ObjId,
        captured: &ObjId,
        old: Option<&LibraryPhoto>,
        photo: &LibraryPhoto,
    ) {
        if old.is_none_or(|value| value.image != photo.image) {
            Self::write_json_at(doc, core, &photo.id, &photo.image);
        }
        // Fresh imports carry only immutable core + order when metadata is at
        // its model default; retain non-default imported annotations.
        if old.is_none() {
            if photo.favorite {
                Self::write_json_at(doc, favorite, &photo.id, &true);
            }
            if photo.folder.is_some() {
                Self::write_json_at(doc, folder, &photo.id, &photo.folder);
            }
            if !photo.people.is_empty() {
                Self::write_json_at(doc, people, &photo.id, &photo.people);
            }
            if !photo.tags.is_empty() {
                Self::write_json_at(doc, tags, &photo.id, &photo.tags);
            }
            if photo.captured_at.is_some() {
                Self::write_json_at(doc, captured, &photo.id, &photo.captured_at);
            }
            return;
        }
        if old.is_none_or(|value| value.favorite != photo.favorite) {
            Self::write_json_at(doc, favorite, &photo.id, &photo.favorite);
        }
        if old.and_then(|value| value.folder.as_ref()) != photo.folder.as_ref() {
            Self::write_json_at(doc, folder, &photo.id, &photo.folder);
        }
        if old.map(|value| &value.people) != Some(&photo.people) {
            Self::write_json_at(doc, people, &photo.id, &photo.people);
        }
        if old.map(|value| &value.tags) != Some(&photo.tags) {
            Self::write_json_at(doc, tags, &photo.id, &photo.tags);
        }
        if old.and_then(|value| value.captured_at.as_ref()) != photo.captured_at.as_ref() {
            Self::write_json_at(doc, captured, &photo.id, &photo.captured_at);
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
            Self::replace_string_list(doc, &row, "photo_ids", &album.photo_ids);
        }
    }

    #[allow(dead_code, reason = "used by the nested-schema migration reference")]
    fn put_optional_string(doc: &mut AutoCommit, parent: &ObjId, key: &str, value: Option<&str>) {
        if let Some(value) = value {
            let _ = doc.put(parent, key, value);
        } else {
            let _ = doc.delete(parent, key);
        }
    }

    fn replace_string_list(doc: &mut AutoCommit, parent: &ObjId, key: &str, values: &[String]) {
        let Some(list) = Self::list_or_make(doc, parent, key) else {
            return;
        };
        while doc.length(&list) > 0 {
            let _ = doc.delete(&list, 0);
        }
        for value in values {
            let _ = doc.insert(&list, doc.length(&list), value);
        }
    }

    fn string_values(doc: &AutoCommit, list: &ObjId) -> Vec<String> {
        (0..doc.length(list))
            .filter_map(|index| match doc.get(list, index).ok().flatten() {
                Some((Value::Scalar(value), _)) => value.to_str().map(str::to_owned),
                _ => None,
            })
            .collect()
    }

    // -- scalar helpers ----------------------------------------------------

    fn get_str(doc: &AutoCommit, key: &str) -> Option<String> {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Scalar(s), _))) => s.to_str().map(std::string::ToString::to_string),
            _ => None,
        }
    }

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
                Value::Scalar(s) => s.to_str().and_then(|raw| serde_json::from_str(raw).ok()),
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
    use qos_actions::Action;

    #[test]
    fn a_seeded_document_has_no_image_open() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        assert!(
            PhotosDoc::image(&doc).is_none(),
            "a fresh document must not fabricate a photograph"
        );
        assert!(PhotosDoc::edits(&doc).is_identity());
    }

    #[test]
    fn seeding_twice_does_not_discard_state() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/x/harbour.png", 800, 600);
        PhotosDoc::set_image(&mut doc, Some(&img));
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());

        PhotosDoc::seed(&mut doc);
        assert_eq!(PhotosDoc::image(&doc).unwrap().name, "harbour.png");
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 1);
    }

    #[test]
    fn edits_round_trip_through_the_crdt() {
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
        assert!(PhotosDoc::get_str(&doc, "edits").is_none());
        assert!(!PhotosDoc::edits_roots(&doc).is_empty());
    }

    #[test]
    fn legacy_edits_scalar_migrates_to_independent_fields_without_reemitting_it() {
        let mut doc = AutoCommit::new();
        let legacy = Edits {
            crop: Rect01 {
                x: 0.125,
                y: 0.25,
                w: 0.5,
                h: 0.625,
            },
            rotate_quarters: 3,
        };
        PhotosDoc::write_json(&mut doc, "edits", &legacy);

        PhotosDoc::seed(&mut doc);

        let restored = PhotosDoc::edits(&doc);
        assert!((restored.crop.x - legacy.crop.x).abs() <= f64::EPSILON);
        assert!((restored.crop.y - legacy.crop.y).abs() <= f64::EPSILON);
        assert!((restored.crop.w - legacy.crop.w).abs() <= f64::EPSILON);
        assert!((restored.crop.h - legacy.crop.h).abs() <= f64::EPSILON);
        assert_eq!(restored.rotate_quarters, legacy.rotate_quarters);
        assert!(PhotosDoc::get_str(&doc, "edits").is_none());
        assert!(!PhotosDoc::edits_roots(&doc).is_empty());
    }

    #[test]
    fn concurrent_crop_and_rotation_merge_at_their_separate_crdt_fields() {
        let mut cropper = replica(1);
        let mut rotator = copy(&mut cropper);
        rotator.set_actor(automerge::ActorId::from([2u8; 16].as_slice()));
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
            .merge(&mut copy(&mut rotator))
            .expect("independent edit fields merge");
        let edits = PhotosDoc::edits(&merged);
        assert!((edits.crop.x - crop.x).abs() <= f64::EPSILON);
        assert!((edits.crop.y - crop.y).abs() <= f64::EPSILON);
        assert!((edits.crop.w - crop.w).abs() <= f64::EPSILON);
        assert!((edits.crop.h - crop.h).abs() <= f64::EPSILON);
        assert_eq!(edits.rotate_quarters, 1);
    }

    #[test]
    fn an_observed_reset_consolidates_competing_edit_roots_and_clears_them() {
        let mut cropper = replica(1);
        let mut rotator = copy(&mut cropper);
        rotator.set_actor(automerge::ActorId::from([2u8; 16].as_slice()));
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
            .merge(&mut copy(&mut rotator))
            .expect("both edit roots merge for the observing editor");
        assert!(!PhotosDoc::edits(&observed).is_identity());

        PhotosDoc::set_edits(&mut observed, &Edits::default());

        assert!(PhotosDoc::edits(&observed).is_identity());
        assert_eq!(PhotosDoc::edits_roots(&observed).len(), 1);
    }

    #[test]
    fn opening_a_new_image_clears_the_previous_crop() {
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

    #[test]
    fn content_addressed_imports_do_not_replicate_the_origin_path() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let origin = "/var/home/alice/Private/harbour.png";
        let hash = "a".repeat(64);
        let image = OpenImage::from_path(origin, 800, 600).with_content(hash.clone());

        PhotosDoc::set_image(&mut doc, Some(&image));

        let image_json = PhotosDoc::get_str(&doc, "image").expect("image JSON");
        assert!(!image_json.contains(origin));
        assert!(PhotosDoc::get_str(&doc, "library").is_none());
        let open = PhotosDoc::image(&doc).expect("imported image");
        assert_eq!(open.path, "harbour.png");
        assert_eq!(open.content.as_deref(), Some(hash.as_str()));
        assert_eq!(PhotosDoc::library(&doc).photos[0].image.path, "harbour.png");
    }

    #[test]
    fn legacy_library_scalar_migrates_once_to_keyed_records() {
        let mut doc = AutoCommit::new();
        let photo = LibraryPhoto::from_image(
            OpenImage::from_path("/legacy/harbour.png", 800, 600).with_content("b".repeat(64)),
        );
        let photo_id = photo.id.clone();
        let library = PhotoLibrary {
            photos: vec![photo],
            albums: vec![crate::model::Album {
                id: "photos.album:legacy".into(),
                name: "Legacy".into(),
                photo_ids: vec![photo_id],
            }],
        };
        PhotosDoc::write_json(&mut doc, "library", &library);

        PhotosDoc::seed(&mut doc);

        assert!(PhotosDoc::get_str(&doc, "library").is_none());
        let root = PhotosDoc::library_roots(&doc)
            .into_iter()
            .next()
            .expect("structured library root");
        assert!(PhotosDoc::map_at(&doc, &root, "photos").is_some());
        assert_eq!(PhotosDoc::library(&doc).portable(), library.portable());
    }

    fn photo(id: &str) -> LibraryPhoto {
        let hash = format!("{id:0<64}");
        let mut image = OpenImage::from_path(&format!("/private/{id}.png"), 800, 600);
        image.name = format!("{id}.png");
        image.content = Some(hash);
        LibraryPhoto::from_image(image)
    }

    fn merge(left: &mut AutoCommit, right: &mut AutoCommit) -> PhotoLibrary {
        left.merge(right).expect("replicas merge");
        PhotosDoc::library(left)
    }

    #[test]
    fn concurrent_independent_photo_fields_merge_without_rewriting_neighbors() {
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
        let mut favorite = copy(&mut base);
        favorite.set_actor(automerge::ActorId::from([2; 16].as_slice()));
        let mut metadata = copy(&mut base);
        metadata.set_actor(automerge::ActorId::from([3; 16].as_slice()));
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

    #[test]
    fn same_photo_favorite_and_metadata_merge_at_distinct_fields() {
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
        let mut favorite = copy(&mut base);
        favorite.set_actor(automerge::ActorId::from([2; 16].as_slice()));
        let mut metadata = copy(&mut base);
        metadata.set_actor(automerge::ActorId::from([3; 16].as_slice()));
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

    #[test]
    fn concurrent_album_membership_and_rename_merge() {
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
        let mut membership = copy(&mut base);
        membership.set_actor(automerge::ActorId::from([2; 16].as_slice()));
        let mut rename = copy(&mut base);
        rename.set_actor(automerge::ActorId::from([3; 16].as_slice()));
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
    }

    #[test]
    fn concurrent_library_root_creation_keeps_both_imports_then_consolidates() {
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
        left.merge(&mut right)
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

    #[test]
    fn concurrent_delete_wins_over_stale_import_or_metadata() {
        let item = photo("removed");
        let mut base = replica(1);
        PhotosDoc::set_library(
            &mut base,
            &PhotoLibrary {
                photos: vec![item],
                ..Default::default()
            },
        );
        let mut delete = copy(&mut base);
        delete.set_actor(automerge::ActorId::from([2; 16].as_slice()));
        let mut stale_import = copy(&mut base);
        stale_import.set_actor(automerge::ActorId::from([3; 16].as_slice()));
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
    /// for a real app is however much its toolkit wrote before the app itself
    /// wrote anything; the actor id is random. So the empty replica is run
    /// both behind and ahead of the populated one, under both actor
    /// orderings, and both merge directions are checked because it is one
    /// document afterwards and both participants have to be looking at it.
    #[test]
    fn a_replica_that_opens_empty_never_closes_the_photograph_the_room_holds() {
        for (open_actor, empty_actor) in [(1u8, 2u8), (2u8, 1u8)] {
            for empty_is_ahead in [false, true] {
                run_the_empty_replica_against_the_open_one(open_actor, empty_actor, empty_is_ahead);
            }
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
            let img = OpenImage::from_path("/var/tmp/qos/workspace/harbour.png", 800, 600);
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
                doc.merge(&mut incoming)
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

    /// And the rule is about concurrency, not about refusing to let go.
    ///
    /// Closing a photograph is a write that *saw* the open one, so it
    /// supersedes it rather than competing with it — there is one value under
    /// the key afterwards and it is the close. Without this the fix above
    /// would read as "a photograph can never be closed once any replica has
    /// seen it", which would be a worse defect than the one it replaces.
    #[test]
    fn a_participant_who_closes_the_photograph_closes_it_everywhere() {
        let mut here = replica(1);
        let img = OpenImage::from_path("/var/tmp/qos/workspace/harbour.png", 800, 600);
        PhotosDoc::set_image(&mut here, Some(&img));
        PhotosDoc::set_edits(&mut here, &Edits::default().rotated_right());

        // A collaborator, holding the same document, closes it and resets the
        // turn — both after seeing what was there.
        let mut there = copy(&mut here);
        there.set_actor(automerge::ActorId::from([2u8; 16].as_slice()));
        PhotosDoc::set_image(&mut there, None);

        let mut back = copy(&mut here);
        back.merge(&mut copy(&mut there))
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

    /// The invariant the blob store exists to protect, asserted here rather
    /// than assumed: a document that has an image open is still tiny, because
    /// the pixels are not in it.
    #[test]
    fn the_document_never_carries_the_pixels() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/var/tmp/qos/home/quinten/harbour.png", 4000, 3000);
        PhotosDoc::set_image(&mut doc, Some(&img));
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        let saved = doc.save().len();
        assert!(
            saved < 4096,
            "the document grew to {saved} bytes; an image must never travel inside a change"
        );
    }
}
