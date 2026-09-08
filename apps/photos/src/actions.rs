//! The exported **actions** and the **resource tree** the app publishes.
//!
//! ## What is an action here, and what is not
//!
//! An [`Action`] is a value that deterministically mutates the shared CRDT.
//! Sorting this app's interactions by that rule leaves a short, honest list:
//!
//! - **Actions.** Rotating, cropping, and resetting. Each is a user-authored
//!   decision about the photograph that survives a restart, replicates to a
//!   collaborator, and belongs on the undo stack. Each carries the **absolute**
//!   resulting [`Edits`] value rather than a delta, so [`Action::apply`] is a
//!   pure function of `(payload, document)` — no wall-clock, no RNG, no read of
//!   local view state — and two replicas replaying it land on the identical
//!   document. A rotate authored as "turn by one" would instead depend on how
//!   many turns the *receiving* replica had already seen, which is exactly the
//!   divergence the model exists to prevent.
//!
//! - **Not actions.** Zoom, pan, and which region the viewport happens to be
//!   showing. They are local presentation and never leave the machine.
//!   Copying to the clipboard is not one either, and that is a deliberate
//!   call rather than an omission: an action replays on *every* peer, so a
//!   clipboard write modelled as one would reach into the selection of every
//!   machine in the room the moment one person pressed Copy. It stays an
//!   affordance of the app instance the user is touching.
//!
//! ## Addressing
//!
//! Everything is named by id. The open photograph is a resource whose id is
//! derived from **its content**, and each edit is an [`ActionSpec`] with a
//! stable id (`rotate-right`, `crop`, …). No screen position appears anywhere
//! in this surface: a caller with no display enumerates by id and title, and
//! invokes by id, which is what lets the palette, a remote peer, and a
//! headless agent all drive the same operations.
//!
//! The content rather than the path, because an address is only worth having
//! if it means the same thing to everybody holding it — see [`resource_id`].

use automerge::AutoCommit;

use qos_actions::{
    Action, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource, ResourceProvider,
    ResultSpec, RetryPolicy,
};

use crate::doc::PhotosDoc;
use crate::model::{Album, Edits, LibraryPhoto, OpenImage, Rect01};

/// Registry `type_id` for every action this app exports.
pub const PHOTOS_ACTION_TYPE: &str = "photos.action";

/// The resource `kind` an open photograph publishes under.
pub const IMAGE_KIND: &str = "photos.image";
pub const PHOTOS_OPERATION_TYPE: &str = "photos.operation";
/// Contextual enumeration is intentionally small; callers resolve any stable
/// id outside this window through `ResourceProvider::resource`.
const LIBRARY_CONTEXT_LIMIT: usize = 48;
const ALBUM_CONTEXT_LIMIT: usize = 24;
const FOLDER_CONTEXT_LIMIT: usize = 24;
const PERSON_CONTEXT_LIMIT: usize = 24;
type LabelCounts = Vec<(String, usize)>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PhotosOperation {
    /// Bring the addressed photograph forward without changing its pixels.
    ///
    /// This is the safe canonical activation used by global search. The host
    /// has already routed the request to the Photos instance that publishes
    /// the resource, so accepting it is enough to reveal the current view.
    View,
    /// Bring a durable album's filtered grid into view. This is presentation
    /// state only: unlike membership actions, activating it never edits the
    /// shared library.
    ViewAlbum {
        album_id: String,
    },
    /// Show the bounded library slice carrying this durable folder label.
    ViewFolder {
        folder: String,
    },
    /// Show the bounded library slice carrying this durable person label.
    ViewPerson {
        person: String,
    },
    Open {
        path: Option<String>,
    },
    Copy,
    /// Copy a portable resource reference to the local clipboard. This is an
    /// explicit share handoff, not a claim that a remote recipient accepted it.
    /// The target is carried explicitly so a library photograph can be shared
    /// without first changing the shared open-editor selection.
    ShareReference {
        resource_id: String,
    },
    Save {
        path: Option<String>,
    },
}

/// Decode an operation payload owned by the Photos app.
///
/// # Errors
///
/// Returns an error when a Photos operation payload is not valid JSON for
/// [`PhotosOperation`]. Unknown operation types return `Ok(None)`.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<PhotosOperation>, String> {
    if type_id != PHOTOS_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn operation(id: &str, label: &str, op: &PhotosOperation) -> Option<ActionSpec> {
    let spec =
        ActionSpec::operation(id, label, OperationKind::Effect, PHOTOS_OPERATION_TYPE, op).ok()?;
    Some(match op {
        PhotosOperation::View
        | PhotosOperation::ViewAlbum { .. }
        | PhotosOperation::ViewFolder { .. }
        | PhotosOperation::ViewPerson { .. }
        | PhotosOperation::Open { .. }
        | PhotosOperation::Copy
        | PhotosOperation::ShareReference { .. } => spec.retry_policy(RetryPolicy::Idempotent),
        PhotosOperation::Save { .. } => spec.retry_policy(RetryPolicy::AtMostOnce),
    })
}

/// Every collaborative operation on the open photograph.
///
/// One variant, carrying the absolute post-edit state. See the module docs for
/// why it is absolute rather than incremental.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PhotosAction {
    /// Replace the edits in force with exactly this value.
    SetEdits {
        /// Region of the original kept, normalized.
        crop: Rect01,
        /// Clockwise quarter turns, `0..=3`.
        rotate_quarters: u8,
    },
    /// Import metadata for bytes already staged in the content store.
    ImportPhoto { photo: LibraryPhoto },
    /// Select a stable library photo for editing.
    SelectPhoto { photo_id: String },
    /// Remove a photo and every album membership that points to it.
    RemovePhoto { photo_id: String },
    /// Toggle a durable favourite marker.
    SetFavorite { photo_id: String, favorite: bool },
    /// Replace the complete small metadata record for one photograph.  One
    /// absolute payload avoids a concurrent series of add/remove operations
    /// exposing a half-edited tag list to collaborators.
    SetMetadata {
        photo_id: String,
        folder: Option<String>,
        people: Vec<String>,
        tags: Vec<String>,
        captured_at: Option<String>,
    },
    /// Create a named collection with a caller-supplied stable id.
    CreateAlbum { id: String, name: String },
    /// Rename a collection without changing its stable address or membership.
    RenameAlbum { id: String, name: String },
    /// Remove a collection without removing photographs.
    RemoveAlbum { id: String },
    /// Replace one album's membership atomically after validating every id.
    SetAlbumMembership {
        album_id: String,
        photo_ids: Vec<String>,
    },
    /// Atomically create or update the complete visible album record.  The
    /// library editor uses this rather than issuing a rename followed by a
    /// membership replacement, so collaborators never observe a half-saved
    /// collection.
    SaveAlbum {
        id: String,
        name: String,
        photo_ids: Vec<String>,
        /// `true` means the stable id must be new; `false` means it must
        /// already exist.  This makes stale editor submits refuse explicitly.
        create: bool,
    },
}

impl PhotosAction {
    /// Build the action that lands `edits`.
    pub fn set(edits: &Edits) -> Self {
        Self::SetEdits {
            crop: edits.crop,
            rotate_quarters: edits.rotate_quarters,
        }
    }

    /// The edit state this action produces, after rejecting malformed durable
    /// input rather than coercing it into a different edit.
    fn edits(&self) -> Result<Edits, ActionError> {
        match self {
            Self::SetEdits {
                crop,
                rotate_quarters,
            } if crop.is_valid_selection() && *rotate_quarters < 4 => Ok(Edits {
                crop: *crop,
                rotate_quarters: *rotate_quarters,
            }),
            Self::SetEdits { .. } => Err(ActionError::Refused(
                "photo edits must name a non-empty crop inside the image and a quarter turn from 0 to 3".into(),
            )),
            _ => Err(ActionError::Refused("this is not an edit action".into())),
        }
    }
}

impl Action for PhotosAction {
    #[allow(
        clippy::too_many_lines,
        reason = "all durable Photos mutations are kept in one exhaustive match so their atomic validation is auditable"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::SetEdits { .. } => {
                PhotosDoc::set_edits(doc, &self.edits()?);
                Ok(())
            }
            Self::ImportPhoto { photo } => {
                if photo.id.is_empty() || photo.image.name.trim().is_empty() {
                    return Err(ActionError::Refused("photo metadata is incomplete".into()));
                }
                let mut library = PhotosDoc::library(doc);
                library.add_photo(photo.clone());
                PhotosDoc::set_library(doc, &library);
                PhotosDoc::set_image(doc, Some(&photo.image));
                Ok(())
            }
            Self::SelectPhoto { photo_id } => {
                let library = PhotosDoc::library(doc);
                let photo = library.photo(photo_id).ok_or_else(|| {
                    ActionError::Refused("that photograph is no longer in the library".into())
                })?;
                PhotosDoc::set_image(doc, Some(&photo.image));
                Ok(())
            }
            Self::RemovePhoto { photo_id } => {
                let mut library = PhotosDoc::library(doc);
                let before = library.photos.len();
                library.photos.retain(|photo| photo.id != *photo_id);
                if library.photos.len() == before {
                    return Err(ActionError::Refused(
                        "that photograph is no longer in the library".into(),
                    ));
                }
                for album in &mut library.albums {
                    album.photo_ids.retain(|id| id != photo_id);
                }
                PhotosDoc::set_library(doc, &library);
                if PhotosDoc::image(doc)
                    .as_ref()
                    .is_some_and(|image| LibraryPhoto::from_image(image.clone()).id == *photo_id)
                {
                    PhotosDoc::set_image(doc, None);
                }
                Ok(())
            }
            Self::SetFavorite { photo_id, favorite } => {
                let mut library = PhotosDoc::library(doc);
                let photo = library
                    .photos
                    .iter_mut()
                    .find(|photo| photo.id == *photo_id)
                    .ok_or_else(|| {
                        ActionError::Refused("that photograph is no longer in the library".into())
                    })?;
                photo.favorite = *favorite;
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::SetMetadata {
                photo_id,
                folder,
                people,
                tags,
                captured_at,
            } => {
                if !valid_metadata(folder.as_deref(), people, tags, captured_at.as_deref()) {
                    return Err(ActionError::Refused(
                        "photo metadata must be short, non-empty labels without duplicates".into(),
                    ));
                }
                let mut library = PhotosDoc::library(doc);
                let photo = library
                    .photos
                    .iter_mut()
                    .find(|photo| photo.id == *photo_id)
                    .ok_or_else(|| {
                        ActionError::Refused("that photograph is no longer in the library".into())
                    })?;
                photo.folder.clone_from(folder);
                photo.people.clone_from(people);
                photo.tags.clone_from(tags);
                photo.captured_at.clone_from(captured_at);
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::CreateAlbum { id, name } => {
                if id.trim().is_empty() || name.trim().is_empty() {
                    return Err(ActionError::Refused(
                        "an album needs an id and a name".into(),
                    ));
                }
                let mut library = PhotosDoc::library(doc);
                if library.album(id).is_some() {
                    return Err(ActionError::Refused("that album already exists".into()));
                }
                library.albums.push(Album {
                    id: id.clone(),
                    name: name.trim().to_owned(),
                    photo_ids: Vec::new(),
                });
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::RenameAlbum { id, name } => {
                if name.trim().is_empty() {
                    return Err(ActionError::Refused("an album needs a name".into()));
                }
                let mut library = PhotosDoc::library(doc);
                let album = library
                    .albums
                    .iter_mut()
                    .find(|album| album.id == *id)
                    .ok_or_else(|| {
                        ActionError::Refused("that album is no longer in the library".into())
                    })?;
                album.name.clone_from(name);
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::RemoveAlbum { id } => {
                let mut library = PhotosDoc::library(doc);
                let before = library.albums.len();
                library.albums.retain(|album| album.id != *id);
                if library.albums.len() == before {
                    return Err(ActionError::Refused(
                        "that album is no longer in the library".into(),
                    ));
                }
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::SetAlbumMembership {
                album_id,
                photo_ids,
            } => {
                let mut library = PhotosDoc::library(doc);
                let valid = photo_ids.iter().all(|id| library.photo(id).is_some())
                    && photo_ids.iter().enumerate().all(|(index, id)| {
                        photo_ids
                            .get(..index)
                            .is_none_or(|prior| !prior.contains(id))
                    });
                if !valid {
                    return Err(ActionError::Refused(
                        "album membership contains an unknown or duplicate photograph".into(),
                    ));
                }
                let album = library
                    .albums
                    .iter_mut()
                    .find(|album| album.id == *album_id)
                    .ok_or_else(|| {
                        ActionError::Refused("that album is no longer in the library".into())
                    })?;
                album.photo_ids.clone_from(photo_ids);
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::SaveAlbum {
                id,
                name,
                photo_ids,
                create,
            } => {
                let name = name.trim();
                if id.trim().is_empty() || name.is_empty() {
                    return Err(ActionError::Refused(
                        "an album needs an id and a name".into(),
                    ));
                }
                let mut library = PhotosDoc::library(doc);
                let valid_members = photo_ids.len()
                    == photo_ids
                        .iter()
                        .collect::<std::collections::HashSet<_>>()
                        .len()
                    && photo_ids
                        .iter()
                        .all(|photo_id| library.photo(photo_id).is_some());
                if !valid_members {
                    return Err(ActionError::Refused(
                        "album membership contains an unknown or duplicate photograph".into(),
                    ));
                }
                match (
                    library.albums.iter_mut().find(|album| album.id == *id),
                    create,
                ) {
                    (Some(_), true) => {
                        return Err(ActionError::Refused("that album already exists".into()));
                    }
                    (None, false) => {
                        return Err(ActionError::Refused(
                            "that album is no longer in the library".into(),
                        ));
                    }
                    (Some(album), false) => {
                        name.clone_into(&mut album.name);
                        album.photo_ids.clone_from(photo_ids);
                    }
                    (None, true) => library.albums.push(Album {
                        id: id.clone(),
                        name: name.to_owned(),
                        photo_ids: photo_ids.clone(),
                    }),
                }
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
        }
    }

    fn locus(&self) -> ExecutionLocus {
        // The edit is a small mutation of a document this client already holds;
        // there is nothing a host could do better.
        ExecutionLocus::ClientLocal
    }
}

/// Register the decoder so a replayed envelope reconstructs the concrete type.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<PhotosAction>(PHOTOS_ACTION_TYPE);
}

/// Encode into the wire form the palette and the on-screen buttons both
/// dispatch — the one place an action becomes a replayable envelope.
///
/// # Panics
///
/// Never in practice: [`PhotosAction`] holds only [`Rect01`] and `u8` fields,
/// which always serialize. `ActionSpec::prompted`'s `build` closure must
/// return an `EncodedAction` directly rather than a `Result`, so there is no
/// error path here to propagate to instead.
#[allow(
    clippy::expect_used,
    reason = "PhotosAction always serializes (Rect01/u8 fields only); the \
              prompted-builder closure this feeds must return EncodedAction \
              directly, not a Result, so there is nowhere to propagate a failure"
)]
pub fn encode(action: &PhotosAction) -> EncodedAction {
    EncodedAction::encode(PHOTOS_ACTION_TYPE, action).expect("PhotosAction is always serializable")
}

/// `ActionSpec::immediate` for a [`PhotosAction`] that lands `next` outright.
/// Every field it carries always serializes — see [`encode`].
#[allow(
    clippy::expect_used,
    reason = "PhotosAction always serializes (Rect01/u8 fields only); see encode()"
)]
fn immediate_photos_spec(id: &str, label: &str, next: Edits) -> ActionSpec {
    ActionSpec::immediate(id, label, PHOTOS_ACTION_TYPE, &PhotosAction::set(&next))
        .expect("PhotosAction is always serializable")
        .reversible()
}

/// Parse the `x,y,w,h` argument a prompted crop collects.
///
/// Accepts four comma-separated numbers in `0..1`. Returns `None` for anything
/// else, which the caller turns into "leave the photograph alone" rather than a
/// panic — this string comes from a human or a remote caller.
pub fn parse_crop_arg(arg: &str) -> Option<Rect01> {
    let parts: Vec<f64> = arg
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    let [x, y, w, h] = parts.as_slice() else {
        return None;
    };
    if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite()) {
        return None;
    }
    // Refuse rather than clamp an out-of-range rectangle. Clamping `1,1,1,1`
    // used to produce a zero-area crop which later became an arbitrary
    // single pixel; a caller gets either the rectangle it requested or no
    // action at all.
    if *x < 0.0
        || *y < 0.0
        || *w <= 0.0
        || *h <= 0.0
        || *x > 1.0
        || *y > 1.0
        || *x + *w > 1.0
        || *y + *h > 1.0
    {
        return None;
    }
    Some(Rect01 {
        x: *x,
        y: *y,
        w: *w,
        h: *h,
    })
}

/// Publishes the open photograph and the edits it supports.
///
/// Enumeration is a **read**: [`resources`](ResourceProvider::resources) takes
/// the document by shared reference, builds specs from values already in it,
/// and performs no I/O — listing what is available never rotates anything, and
/// never touches the file on disk.
pub struct PhotosResources;

impl ResourceProvider for PhotosResources {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        // Nothing open: there is no addressed photograph yet. The shell's app
        // launcher remains the entry point for opening the first one.
        let Some(image) = PhotosDoc::image(doc) else {
            return library_resources(doc);
        };
        let edits = PhotosDoc::edits(doc);
        let (w, h) = image.edited_size(&edits);
        // Minted once and used for the resource and every child, so the parent
        // and its facts cannot drift apart.
        let id = resource_id(&image);

        let mut resource = Resource::new(id.clone(), IMAGE_KIND, &image.name);
        if let Some(spec) = operation("view", "View photograph", &PhotosOperation::View) {
            resource =
                resource.with_action(spec.with_description("Bring this photograph into view"));
        }
        resource = resource
            .with_action(
                immediate_photos_spec("rotate-right", "Rotate right", edits.rotated_right())
                    .with_description("Turn the photograph a quarter turn clockwise"),
            )
            .with_action(
                immediate_photos_spec("rotate-left", "Rotate left", edits.rotated_left())
                    .with_description("Turn the photograph a quarter turn anticlockwise"),
            )
            .with_action(
                ActionSpec::typed(
                    "crop",
                    "Crop to a region",
                    "0.25,0.25,0.5,0.5",
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required(
                        "region",
                        ParamType::Text,
                        "normalized x,y,width,height values between 0 and 1",
                    )
                    .with_schema(serde_json::json!({
                        "type": "string",
                        "pattern": "^[[:space:]]*(0(\\.[0-9]+)?|1(\\.0+)?)[[:space:]]*,[[:space:]]*(0(\\.[0-9]+)?|1(\\.0+)?)[[:space:]]*,[[:space:]]*(0(\\.[0-9]+)?|1(\\.0+)?)[[:space:]]*,[[:space:]]*(0(\\.[0-9]+)?|1(\\.0+)?)[[:space:]]*$"
                    }))],
                    move |args: &ActionArgs| {
                        // An unparseable argument leaves the photograph exactly
                        // as it is; it never crops to nothing.
                        let next = match args.text("region").and_then(parse_crop_arg) {
                            Some(rect) => edits.cropped_from_view(rect),
                            None => edits,
                        };
                        encode(&PhotosAction::set(&next))
                    },
                )
                .reversible()
                .with_description(
                    "Keep a region of what is currently shown, in normalized coordinates",
                ),
            );
        resource = resource.with_action(file_operation("open", "Open another photograph", true));
        for (id, label, op) in [("copy", "Copy photograph", PhotosOperation::Copy)] {
            if let Some(spec) = operation(id, label, &op) {
                resource = resource.with_action(spec);
            }
        }
        if let Some(spec) = operation(
            "share-reference",
            "Share photograph reference",
            &PhotosOperation::ShareReference {
                resource_id: id.clone(),
            },
        ) {
            resource = resource.with_action(spec);
        }
        resource = resource.with_action(file_operation("save", "Save edited photograph", false));

        // Reset only appears when there is something to undo, so the surface
        // never offers an operation that would do nothing.
        if !edits.is_identity() {
            resource = resource.with_action(
                immediate_photos_spec("reset", "Reset edits", Edits::default())
                    .with_description("Discard the crop and rotation, returning to the original"),
            );
        }

        // The dimensions ride along in the label of a child resource rather
        // than in a side channel, so a caller that can only read ids and
        // titles still learns the size it is working with.
        resource = resource.with_child(Resource::new(
            format!("{id}#size"),
            "photos.size",
            format!("{w} x {h} pixels"),
        ));

        let mut resources = vec![resource];
        resources.extend(library_resources(doc));
        resources
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if id == "photos.library" {
            return library_resources(doc).into_iter().next();
        }
        let library = PhotosDoc::library(doc);
        if let Some(photo) = library.photos.iter().find(|photo| photo.id == id) {
            return Some(photo_resource(doc, photo));
        }
        library
            .albums
            .iter()
            .find(|album| album.id == id)
            .map(album_resource)
            .or_else(|| {
                folder_and_people(&library)
                    .0
                    .into_iter()
                    .find(|(folder, _)| folder_resource_id(folder) == id)
                    .map(|(folder, count)| folder_resource(&folder, count))
            })
            .or_else(|| {
                folder_and_people(&library)
                    .1
                    .into_iter()
                    .find(|(person, _)| person_resource_id(person) == id)
                    .map(|(person, count)| person_resource(&person, count))
            })
    }
}

/// The durable library and album resource tree. Every photo and album gets a
/// stable resource id even when it is not selected in the editor, allowing
/// agents and search to resolve a late library item without a visual grid
/// having first materialised it.
fn library_resources(doc: &AutoCommit) -> Vec<Resource> {
    let library = PhotosDoc::library(doc);
    let mut root = Resource::new("photos.library", "photos.library", "Photo library")
        .with_action(file_operation("open", "Import photograph", true))
        .with_action(create_album_spec());
    for photo in library.photos.iter().take(LIBRARY_CONTEXT_LIMIT) {
        root = root.with_child(photo_resource(doc, photo));
    }
    for album in library.albums.iter().take(ALBUM_CONTEXT_LIMIT) {
        root = root.with_child(album_resource(album));
    }
    let (folders, people) = folder_and_people(&library);
    for (folder, count) in folders.into_iter().take(FOLDER_CONTEXT_LIMIT) {
        root = root.with_child(folder_resource(&folder, count));
    }
    for (person, count) in people.into_iter().take(PERSON_CONTEXT_LIMIT) {
        root = root.with_child(person_resource(&person, count));
    }
    vec![root]
}

/// Deterministically collect the small virtual browse namespace.  It is
/// derived only from durable metadata and does no filesystem work, so it is
/// safe to publish to search, agents, history, and collaborators.
fn folder_and_people(library: &crate::model::PhotoLibrary) -> (LabelCounts, LabelCounts) {
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
    (folders.into_iter().collect(), people.into_iter().collect())
}

pub(crate) fn folder_resource_id(folder: &str) -> String {
    format!(
        "photos.folder:{}",
        qdoc::blobstore::hash_bytes(folder.as_bytes())
    )
}

pub(crate) fn person_resource_id(person: &str) -> String {
    format!(
        "photos.person:{}",
        qdoc::blobstore::hash_bytes(person.as_bytes())
    )
}

fn folder_resource(folder: &str, count: usize) -> Resource {
    let mut item = Resource::new(folder_resource_id(folder), "photos.folder", folder).with_child(
        Resource::new(
            format!("{}#count", folder_resource_id(folder)),
            "photos.count",
            format!("{count} photographs"),
        ),
    );
    if let Some(spec) = operation(
        "open",
        "Open folder",
        &PhotosOperation::ViewFolder {
            folder: folder.to_owned(),
        },
    ) {
        item = item.with_action(spec.with_description("Show photographs in this shared folder"));
    }
    item
}

fn person_resource(person: &str, count: usize) -> Resource {
    let mut item = Resource::new(person_resource_id(person), "photos.person", person).with_child(
        Resource::new(
            format!("{}#count", person_resource_id(person)),
            "photos.count",
            format!("{count} photographs"),
        ),
    );
    if let Some(spec) = operation(
        "open",
        "View person",
        &PhotosOperation::ViewPerson {
            person: person.to_owned(),
        },
    ) {
        item = item.with_action(spec.with_description("Show photographs tagged with this person"));
    }
    item
}

fn photo_resource(_doc: &AutoCommit, photo: &LibraryPhoto) -> Resource {
    let mut item =
        Resource::new(&photo.id, "photos.photo", &photo.image.name).with_child(Resource::new(
            format!("{}#size", photo.id),
            "photos.size",
            format!("{} x {} pixels", photo.image.width, photo.image.height),
        ));
    if !photo.metadata_summary().is_empty() {
        item = item.with_child(Resource::new(
            format!("{}#metadata", photo.id),
            "photos.metadata",
            photo.metadata_summary(),
        ));
    }
    if let Some(spec) = direct_spec(
        "open",
        "Open photograph",
        &PhotosAction::SelectPhoto {
            photo_id: photo.id.clone(),
        },
    ) {
        item = item.with_action(spec);
    }
    if let Some(spec) = direct_spec(
        "favorite",
        if photo.favorite {
            "Remove favorite"
        } else {
            "Favorite"
        },
        &PhotosAction::SetFavorite {
            photo_id: photo.id.clone(),
            favorite: !photo.favorite,
        },
    ) {
        item = item.with_action(spec.reversible());
    }
    if let Some(spec) = operation(
        "share-reference",
        "Share photograph reference",
        &PhotosOperation::ShareReference {
            resource_id: photo.id.clone(),
        },
    ) {
        item = item.with_action(spec.with_description(
            "Copy this library photograph's portable resource reference to the local clipboard",
        ));
    }
    item = item.with_action(photo_metadata_spec(photo));
    if let Some(spec) = direct_spec(
        "remove",
        "Remove from library",
        &PhotosAction::RemovePhoto {
            photo_id: photo.id.clone(),
        },
    ) {
        item = item.with_action(spec.destructive().reversible());
    }
    item
}

/// Keep user-authored metadata compact enough that a library remains a small
/// CRDT record, and reject (rather than normalize) malformed remote commands
/// so every replica either applies exactly the same labels or refuses them.
fn valid_metadata(
    folder: Option<&str>,
    people: &[String],
    tags: &[String],
    captured_at: Option<&str>,
) -> bool {
    fn text(value: &str) -> bool {
        !value.trim().is_empty() && value.chars().count() <= 128
    }
    fn labels(values: &[String]) -> bool {
        values.len() <= 32
            && values.iter().all(|value| text(value))
            && values
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                == values.len()
    }
    folder.is_none_or(text) && captured_at.is_none_or(text) && labels(people) && labels(tags)
}

fn metadata_labels(args: &ActionArgs, name: &str) -> Vec<String> {
    args.text(name)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn optional_metadata_text(args: &ActionArgs, name: &str) -> Option<String> {
    args.text(name)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn photo_metadata_spec(photo: &LibraryPhoto) -> ActionSpec {
    let photo_id = photo.id.clone();
    ActionSpec::typed(
        "set-metadata",
        "Set photo metadata",
        "Folder, people, tags, and capture date",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::optional("folder", ParamType::Text, "shared folder label"),
            ActionParam::optional("people", ParamType::Text, "comma-separated people"),
            ActionParam::optional("tags", ParamType::Text, "comma-separated tags"),
            ActionParam::optional("captured_at", ParamType::Text, "capture date or time"),
        ],
        move |args| {
            encode(&PhotosAction::SetMetadata {
                photo_id: photo_id.clone(),
                folder: optional_metadata_text(args, "folder"),
                people: metadata_labels(args, "people"),
                tags: metadata_labels(args, "tags"),
                captured_at: optional_metadata_text(args, "captured_at"),
            })
        },
    )
    .reversible()
    .with_description("Replace this photograph's shared folder, people, tags, and capture date")
}

fn album_resource(album: &Album) -> Resource {
    let mut item = Resource::new(&album.id, "photos.album", &album.name).with_child(Resource::new(
        format!("{}#count", album.id),
        "photos.count",
        format!("{} photographs", album.photo_ids.len()),
    ));
    if let Some(spec) = operation(
        "open",
        "Open album",
        &PhotosOperation::ViewAlbum {
            album_id: album.id.clone(),
        },
    ) {
        item = item.with_action(
            spec.with_description("Show this album's photographs without changing membership"),
        );
    }
    if let Some(spec) = direct_spec(
        "remove",
        "Remove album",
        &PhotosAction::RemoveAlbum {
            id: album.id.clone(),
        },
    ) {
        item = item.with_action(spec.destructive().reversible());
    }
    item = item.with_action(album_rename_spec(album));
    item = item.with_action(album_membership_spec(album));
    item = item.with_action(album_save_spec(album));
    item
}

fn membership_ids(args: &ActionArgs) -> Vec<String> {
    args.text("photo_ids")
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn create_album_spec() -> ActionSpec {
    ActionSpec::typed(
        "create-album",
        "Create album",
        "Stable album id and visible name",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required(
                "id",
                ParamType::Text,
                "stable album id, for example photos.album:holiday",
            ),
            ActionParam::required("name", ParamType::Text, "visible album name"),
            ActionParam::optional(
                "photo_ids",
                ParamType::Text,
                "comma-separated stable photo ids",
            ),
        ],
        |args| {
            encode(&PhotosAction::SaveAlbum {
                id: args.text("id").unwrap_or_default().trim().to_owned(),
                name: args.text("name").unwrap_or_default().trim().to_owned(),
                photo_ids: membership_ids(args),
                create: true,
            })
        },
    )
    .reversible()
    .with_description("Create an album and its initial photograph membership atomically")
}

fn album_save_spec(album: &Album) -> ActionSpec {
    let id = album.id.clone();
    ActionSpec::typed(
        "save",
        "Save album",
        "Name and complete membership",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("name", ParamType::Text, "visible album name"),
            ActionParam::optional(
                "photo_ids",
                ParamType::Text,
                "comma-separated stable photo ids",
            ),
        ],
        move |args| {
            encode(&PhotosAction::SaveAlbum {
                id: id.clone(),
                name: args.text("name").unwrap_or_default().trim().to_owned(),
                photo_ids: membership_ids(args),
                create: false,
            })
        },
    )
    .reversible()
    .with_description("Atomically rename this album and replace its membership")
}

fn album_rename_spec(album: &Album) -> ActionSpec {
    let album_id = album.id.clone();
    ActionSpec::typed(
        "rename",
        "Rename album",
        "New album name",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "name",
            ParamType::Text,
            "new album name",
        )],
        move |args: &ActionArgs| {
            encode(&PhotosAction::RenameAlbum {
                id: album_id.clone(),
                name: args.text("name").unwrap_or_default().trim().to_owned(),
            })
        },
    )
    .reversible()
    .with_description("Change this album's visible name while keeping its stable id")
}

fn album_membership_spec(album: &Album) -> ActionSpec {
    let album_id = album.id.clone();
    ActionSpec::typed(
        "set-membership",
        "Set album membership",
        "photos.photo:… , photos.photo:… (empty clears the album)",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "photo_ids",
            ParamType::Text,
            "comma-separated stable photo ids; empty clears the album",
        )],
        move |args: &ActionArgs| {
            let ids = args
                .text("photo_ids")
                .map(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            encode(&PhotosAction::SetAlbumMembership {
                album_id: album_id.clone(),
                photo_ids: ids,
            })
        },
    )
    .reversible()
    .with_description("Replace this album's photograph membership atomically")
}

fn direct_spec(id: &str, label: &str, action: &PhotosAction) -> Option<ActionSpec> {
    ActionSpec::immediate(id, label, PHOTOS_ACTION_TYPE, action).ok()
}

fn file_operation(id: &str, label: &str, open: bool) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "path (omit to use the system file picker or suggested save path)",
        OperationKind::Effect,
        PHOTOS_OPERATION_TYPE,
        vec![ActionParam::optional_owned(
            "path",
            ParamType::Text,
            "Direct filesystem path",
        )],
        move |args: &ActionArgs| {
            let path = args
                .text("path")
                .filter(|p| !p.trim().is_empty())
                .map(str::to_string);
            EncodedAction::encode_operation(
                PHOTOS_OPERATION_TYPE,
                &if open {
                    PhotosOperation::Open { path }
                } else {
                    PhotosOperation::Save { path }
                },
            )
            .unwrap_or_else(|error| EncodedAction {
                type_id: qos_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                payload: format!("photos file operation could not be serialized: {error}")
                    .into_bytes(),
            })
        },
    )
    .execution(ExecutionMode::Background { cancellable: false })
    .returns(ResultSpec::Job)
}

/// The address of a photograph: what it is, not where a machine put it.
///
/// # Why the path was the wrong name
///
/// This used to be `format!("{IMAGE_KIND}:{path}")`, and the path is true on
/// exactly one machine. Content handed to a room is staged by each session
/// under its own runtime directory, so one photograph had one address per
/// participant — `…/qlva…/qos-opened/<hash>.png` here, `…/qlvx…/` there,
/// `…/qlvq…/` in the third. An address is resolved by string equality
/// ([`qos_actions::door::resolve`]), so an operation invoked on one of them
/// reached exactly one session: the one whose path had won. An agent cropped
/// the editor in `qlva…` while the person watching the editor in `qlvx…` saw
/// their picture sit there unchanged. The run before, the same invocation
/// worked — because that time the id happened to name the watcher's copy.
///
/// So the name is the content's, through the hash the document already
/// carries beside the path (`OpenImage::content`). Content is the same word in
/// every session, which is the property an address has to have before anyone
/// can act through one: everybody says the same thing about the picture, and
/// the operation lands on it wherever it is open.
///
/// # Why the file's name is not in it
///
/// Because a name is a property of whoever wrote it down, exactly as the path
/// was. A participant who opened `harbour.png` off their own disk and one who
/// was handed the same bytes staged as `<hash>.png` would be back to two
/// addresses for one picture. What a person reads is the resource's **label**,
/// which is the file's own name and travels beside the id everywhere one is
/// rendered — the palette row, `qos_actions::FlatAction::resource_label`, the
/// agent's survey line, the chip on a remark.
///
/// # A photograph with no content hash
///
/// Named by its path, as it always was. A document written before the hash
/// existed parses with `content: None`, and the path is the only thing such a
/// picture has: as good an address as it ever was in the one session the path
/// is true of, and better than publishing nothing. Nothing is migrated,
/// because there is nothing stored to migrate — this tree is a pure function
/// of the document, rebuilt every frame, so a picture gains its content
/// address the moment its bytes reach the store.
pub fn resource_id(image: &OpenImage) -> String {
    match &image.content {
        Some(hash) => content_address(hash),
        None => format!("{IMAGE_KIND}:{}", image.path),
    }
}

/// The address the content named by `hash` is published under.
///
/// Split out so a caller holding the bytes but no document — the offscreen
/// renderer posing a scenario, a host staging content for an app — spells the
/// address the one way, rather than assembling a second spelling of it that
/// can drift from the one the app answers to.
pub fn content_address(hash: &str) -> String {
    format!("{IMAGE_KIND}:{hash}")
}

/// Length of a blake3 hash written as hex, which is what a content-named id
/// carries after its kind.
const CONTENT_HEX_LEN: usize = 64;

/// The content a resource id names, if it names content.
///
/// The inverse of [`resource_id`] for the ids minted from a hash, and `None`
/// for the path-shaped id a document written before content hashes still
/// produces. This is what keeps a portable address useful rather than merely
/// identical everywhere: a session that was handed the picture and never had a
/// file for it goes from the address to the bytes through
/// `qdoc::blobstore::get`, the same route [`crate::doc::bytes_of`] takes for
/// the photograph it is drawing.
pub fn content_of(id: &str) -> Option<&str> {
    let tail = id.strip_prefix(IMAGE_KIND)?.strip_prefix(':')?;
    let is_content = tail.len() == CONTENT_HEX_LEN
        && tail
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    is_content.then_some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OpenImage;
    use qos_actions::flatten;

    fn doc_with_image() -> AutoCommit {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/var/tmp/qos/home/user/harbour.png", 800, 400);
        PhotosDoc::set_image(&mut doc, Some(&img));
        doc
    }

    /// The address of a photograph that names its content is that content.
    ///
    /// Asserted on the published tree rather than on `resource_id` alone, so
    /// it is the id a caller actually resolves against.
    #[test]
    fn a_photograph_that_names_its_content_is_addressed_by_it() {
        const HASH: &str = "623fdbf0c19749ddd1116f8c68b561ad648e3023ca4f304a732bf7b06d1ffb82";
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/run/user/1000/qlva3099960/qos-opened/x.png", 800, 400)
            .with_content(HASH);
        PhotosDoc::set_image(&mut doc, Some(&img));

        let res = PhotosResources.resources(&doc);
        assert_eq!(res[0].id.0, format!("photos.image:{HASH}"));
        assert_eq!(
            content_of(&res[0].id.0),
            Some(HASH),
            "the address does not lead back to the content it names"
        );
        // And the file's name is still what a person reads.
        assert_eq!(res[0].label, "x.png");
    }

    /// The path-shaped id an older document produces is not mistaken for a
    /// content address. A caller that got one has to fall back to the path,
    /// and would otherwise ask the content store for a directory name.
    #[test]
    fn a_path_shaped_address_names_no_content() {
        let doc = doc_with_image();
        let res = PhotosResources.resources(&doc);
        assert_eq!(content_of(&res[0].id.0), None);
        assert_eq!(content_of("photos.image:"), None);
        assert_eq!(content_of("files.entry:/x"), None);
        // Right length, wrong alphabet: hex is what a hash is written in.
        assert_eq!(
            content_of(&format!("photos.image:{}", "z".repeat(64))),
            None
        );
    }

    #[test]
    fn nothing_open_publishes_an_openable_library_resource() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let resources = PhotosResources.resources(&doc);
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].kind, "photos.library");
        assert_eq!(resources[0].actions[0].id, "open");
    }

    #[test]
    fn the_open_photograph_is_published_with_its_name_and_size() {
        let doc = doc_with_image();
        let res = PhotosResources.resources(&doc);
        let image = res
            .iter()
            .find(|resource| resource.kind == IMAGE_KIND)
            .unwrap();
        assert_eq!(image.label, "harbour.png");
        assert_eq!(
            image.id.0,
            "photos.image:/var/tmp/qos/home/user/harbour.png"
        );
        // The size is discoverable from titles alone.
        assert_eq!(image.children.len(), 1);
        assert_eq!(image.children[0].label, "800 x 400 pixels");
    }

    #[test]
    fn every_action_is_addressable_by_a_stable_id() {
        let doc = doc_with_image();
        let res = PhotosResources.resources(&doc);
        let ids: Vec<&str> = res[0].actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "view",
                "rotate-right",
                "rotate-left",
                "crop",
                "open",
                "copy",
                "share-reference",
                "save",
            ]
        );
    }

    #[test]
    fn reset_appears_only_once_there_is_an_edit_to_discard() {
        let mut doc = doc_with_image();
        let before = PhotosResources.resources(&doc);
        assert!(!before[0].actions.iter().any(|a| a.id == "reset"));

        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        let after = PhotosResources.resources(&doc);
        assert!(after[0].actions.iter().any(|a| a.id == "reset"));
    }

    #[test]
    fn the_selected_photo_can_be_removed_through_its_canonical_resource() {
        let mut doc = doc_with_image();
        let selected = PhotosDoc::image(&doc).unwrap();
        let id = LibraryPhoto::from_image(selected).id;
        let resource = PhotosResources.resource(&doc, &id).unwrap();
        let remove = resource
            .actions
            .iter()
            .find(|action| action.id == "remove")
            .expect("the selected photo still offers removal");
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        let qos_actions::ActionInput::Immediate(encoded) = &remove.input else {
            panic!("removal is immediate");
        };
        registry.decode(encoded).unwrap().apply(&mut doc).unwrap();
        assert!(PhotosDoc::image(&doc).is_none());
        assert!(PhotosDoc::library(&doc).photos.is_empty());
    }

    #[test]
    fn an_unselected_library_photo_exposes_a_share_reference_for_its_own_id() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let image = OpenImage::from_path("/photos/library-only.png", 20, 10);
        let photo = LibraryPhoto::from_image(image);
        let id = photo.id.clone();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![photo],
                ..Default::default()
            },
        );

        let resource = PhotosResources
            .resource(&doc, &id)
            .expect("library photo has a canonical resource");
        let share = resource
            .actions
            .iter()
            .find(|action| action.id == "share-reference")
            .expect("an addressable library photo can be shared");
        let qos_actions::ActionInput::Immediate(encoded) = &share.input else {
            panic!("share is a local effect operation");
        };
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(PhotosOperation::ShareReference { resource_id: id })
        );
    }

    #[test]
    fn shared_metadata_is_bounded_replayable_and_publishes_folder_and_person_resources() {
        let mut doc = doc_with_image();
        let photo_id = PhotosDoc::library(&doc).photos[0].id.clone();
        let action = PhotosAction::SetMetadata {
            photo_id: photo_id.clone(),
            folder: Some("2026 travels".into()),
            people: vec!["München-夏".into()],
            tags: vec!["harbour".into(), "sunset".into()],
            captured_at: Some("2026-09-07".into()),
        };
        action.apply(&mut doc).expect("valid metadata applies");

        let photo = PhotosResources
            .resource(&doc, &photo_id)
            .expect("photo resolves");
        assert!(
            photo
                .actions
                .iter()
                .any(|action| action.id == "set-metadata")
        );
        assert!(
            photo
                .children
                .iter()
                .any(|child| child.kind == "photos.metadata" && child.label.contains("harbour"))
        );

        let folder_id = folder_resource_id("2026 travels");
        let person_id = person_resource_id("München-夏");
        for id in [folder_id, person_id] {
            let resource = PhotosResources
                .resource(&doc, &id)
                .expect("virtual resource resolves");
            assert!(resource.actions.iter().any(|action| action.id == "open"));
            assert!(
                resource
                    .children
                    .iter()
                    .any(|child| child.kind == "photos.count")
            );
        }

        let before = PhotosDoc::library(&doc);
        PhotosAction::SetMetadata {
            photo_id,
            folder: Some(" ".into()),
            people: vec!["same".into(), "same".into()],
            tags: Vec::new(),
            captured_at: None,
        }
        .apply(&mut doc)
        .expect_err("malformed metadata must be refused, not normalized");
        assert_eq!(PhotosDoc::library(&doc), before);
    }

    /// Enumerating must be a pure read. If listing what is on offer could
    /// mutate the document, a palette opening would silently edit the user's
    /// photograph.
    #[test]
    fn enumeration_fires_no_side_effects() {
        let mut doc = doc_with_image();
        let before_heads = doc.get_heads();
        let before_bytes = doc.save();

        let res = PhotosResources.resources(&doc);
        let flat = flatten(&res);
        // Touch every field a caller would read, including building the label
        // of each row.
        for f in &flat {
            let _ = (&f.key, &f.resource_label, &f.action.label, f.action.locus);
        }
        assert!(!flat.is_empty(), "there was nothing to enumerate");

        assert_eq!(
            doc.get_heads(),
            before_heads,
            "enumeration moved the document"
        );
        assert_eq!(doc.save(), before_bytes, "enumeration changed the document");
    }

    #[test]
    fn applying_rotate_right_advances_the_document_by_one_quarter() {
        let mut doc = doc_with_image();
        let action = PhotosAction::set(&PhotosDoc::edits(&doc).rotated_right());
        action.apply(&mut doc).unwrap();
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 1);
    }

    /// The determinism contract: the same envelope applied to two replicas
    /// that started equal leaves them equal.
    #[test]
    fn the_same_action_replays_to_the_same_state() {
        let mut a = doc_with_image();
        let mut b = doc_with_image();
        let action = PhotosAction::SetEdits {
            crop: Rect01 {
                x: 0.1,
                y: 0.2,
                w: 0.5,
                h: 0.5,
            },
            rotate_quarters: 3,
        };
        action.apply(&mut a).unwrap();
        action.apply(&mut b).unwrap();
        assert_eq!(PhotosDoc::edits(&a), PhotosDoc::edits(&b));
    }

    #[test]
    fn an_action_round_trips_through_the_registry() {
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        let original = PhotosAction::set(&Edits::default().rotated_left());
        let decoded = registry.decode(&encode(&original)).expect("decode");
        let mut doc = doc_with_image();
        decoded.apply(&mut doc).unwrap();
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 3);
    }

    #[test]
    fn a_malformed_crop_payload_from_a_peer_is_refused_without_mutating_the_document() {
        let mut doc = doc_with_image();
        PhotosAction::SetEdits {
            crop: Rect01 {
                x: -2.0,
                y: 0.5,
                w: 9.0,
                h: 9.0,
            },
            rotate_quarters: 250,
        }
        .apply(&mut doc)
        .expect_err("malformed durable edits must be refused");
        let e = PhotosDoc::edits(&doc);
        assert!(
            e.is_identity(),
            "a refused action changed the document: {e:?}"
        );
    }

    #[test]
    fn crop_arguments_parse_and_reject() {
        let r = parse_crop_arg("0.25, 0.25, 0.5, 0.5").expect("four numbers");
        assert!((r.w - 0.5).abs() < 1e-9);
        assert!(parse_crop_arg("").is_none());
        assert!(
            parse_crop_arg("0.1,0.2,0.3").is_none(),
            "three is not a rect"
        );
        assert!(parse_crop_arg("a,b,c,d").is_none());
        assert!(
            parse_crop_arg("0,0,0,0").is_none(),
            "an empty crop is refused"
        );
        assert!(parse_crop_arg("1,1,1,1").is_none(), "a crop must fit");
    }

    /// A prompted crop that cannot be parsed must leave the photograph alone
    /// rather than crop it to nothing.
    #[test]
    fn an_unparseable_crop_argument_is_a_no_op() {
        let mut doc = doc_with_image();
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        let res = PhotosResources.resources(&doc);
        let crop = res[0]
            .actions
            .iter()
            .find(|a| a.id == "crop")
            .expect("crop is published");
        let qos_actions::ActionInput::Prompted { build, .. } = &crop.input else {
            panic!("crop collects an argument");
        };
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        assert!(registry.decode(&build("nonsense")).is_err());
        let e = PhotosDoc::edits(&doc);
        assert!(
            e.crop.is_full(),
            "a bad argument cropped the photograph: {:?}",
            e.crop
        );
        assert_eq!(
            e.rotate_quarters, 1,
            "and it must not undo the rotation either"
        );
    }

    #[test]
    fn a_prompted_crop_composes_onto_the_edits_in_force() {
        let mut doc = doc_with_image();
        let res = PhotosResources.resources(&doc);
        let crop = res[0].actions.iter().find(|a| a.id == "crop").unwrap();
        let qos_actions::ActionInput::Prompted { build, .. } = &crop.input else {
            panic!("crop collects an argument");
        };
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        registry
            .decode(&build("0,0,0.5,0.5"))
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        let e = PhotosDoc::edits(&doc);
        assert!((e.crop.w - 0.5).abs() < 1e-9, "{:?}", e.crop);
        assert!((e.crop.h - 0.5).abs() < 1e-9, "{:?}", e.crop);
    }

    #[test]
    fn exported_photo_operations_declare_arguments_and_execution_contracts() {
        let res = PhotosResources.resources(&doc_with_image());
        let crop = res[0].actions.iter().find(|a| a.id == "crop").unwrap();
        assert_eq!(crop.params()[0].name.as_ref(), "region");
        assert!(crop.params()[0].schema.is_some());
        assert!(crop.contract.reversible);

        let open = res[0].actions.iter().find(|a| a.id == "open").unwrap();
        assert_eq!(
            open.contract.execution,
            qos_actions::ExecutionMode::Background { cancellable: false }
        );
        assert_eq!(open.contract.result, qos_actions::ResultSpec::Job);
        assert_eq!(open.params()[0].name.as_ref(), "path");
        let copy = res[0].actions.iter().find(|a| a.id == "copy").unwrap();
        assert_eq!(copy.contract.retry, RetryPolicy::Idempotent);
        let view = res[0].actions.iter().find(|a| a.id == "view").unwrap();
        assert_eq!(view.contract.retry, RetryPolicy::Idempotent);
    }

    #[test]
    fn every_published_action_has_a_valid_machine_readable_contract() {
        fn check(resource: &Resource) {
            for action in &resource.actions {
                assert!(
                    action.contract.validate_declaration().is_empty(),
                    "{} on {} has an invalid contract: {:?}",
                    action.id,
                    resource.id.0,
                    action.contract.validate_declaration()
                );
            }
            for child in &resource.children {
                check(child);
            }
        }

        let empty = AutoCommit::new();
        for resource in PhotosResources.resources(&empty) {
            check(&resource);
        }
        for resource in PhotosResources.resources(&doc_with_image()) {
            check(&resource);
        }
    }

    #[test]
    fn library_actions_are_atomic_and_refuse_stale_membership() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/夏.png", 20, 10));
        PhotosAction::ImportPhoto {
            photo: photo.clone(),
        }
        .apply(&mut doc)
        .unwrap();
        PhotosAction::CreateAlbum {
            id: "album:summer".into(),
            name: "夏".into(),
        }
        .apply(&mut doc)
        .unwrap();
        PhotosAction::SetAlbumMembership {
            album_id: "album:summer".into(),
            photo_ids: vec![photo.id.clone()],
        }
        .apply(&mut doc)
        .unwrap();
        let before = PhotosDoc::library(&doc);
        PhotosAction::SetAlbumMembership {
            album_id: "album:summer".into(),
            photo_ids: vec!["photos.photo:gone".into()],
        }
        .apply(&mut doc)
        .expect_err("a stale target must not partially change membership");
        assert_eq!(PhotosDoc::library(&doc), before);
        PhotosAction::RemovePhoto { photo_id: photo.id }
            .apply(&mut doc)
            .unwrap();
        assert!(PhotosDoc::library(&doc).albums[0].photo_ids.is_empty());
    }

    #[test]
    fn saving_an_album_is_atomic_replayable_and_refuses_a_stale_editor() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/one.png", 20, 10));
        PhotosAction::ImportPhoto {
            photo: photo.clone(),
        }
        .apply(&mut doc)
        .unwrap();
        let create = PhotosAction::SaveAlbum {
            id: "photos.album:one".into(),
            name: "One".into(),
            photo_ids: vec![photo.id.clone()],
            create: true,
        };
        create.apply(&mut doc).unwrap();
        let before = PhotosDoc::library(&doc);
        PhotosAction::SaveAlbum {
            id: "photos.album:gone".into(),
            name: "Gone".into(),
            photo_ids: vec![photo.id],
            create: false,
        }
        .apply(&mut doc)
        .expect_err("a stale editor cannot create a replacement album");
        assert_eq!(PhotosDoc::library(&doc), before);
        let encoded = encode(&create);
        let mut replica = AutoCommit::new();
        PhotosDoc::seed(&mut replica);
        PhotosAction::ImportPhoto {
            photo: LibraryPhoto::from_image(OpenImage::from_path("/photos/one.png", 20, 10)),
        }
        .apply(&mut replica)
        .unwrap();
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        registry
            .decode(&encoded)
            .unwrap()
            .apply(&mut replica)
            .unwrap();
        assert_eq!(PhotosDoc::library(&replica).albums[0].name, "One");
    }

    #[test]
    fn large_library_snapshot_is_bounded_but_late_ids_resolve_directly() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..300 {
            library
                .photos
                .push(LibraryPhoto::from_image(OpenImage::from_path(
                    &format!("/photos/{index}.png"),
                    10,
                    10,
                )));
        }
        let late = library.photos.last().unwrap().id.clone();
        PhotosDoc::set_library(&mut doc, &library);
        let root = PhotosResources.resources(&doc).pop().unwrap();
        assert!(root.children.len() <= LIBRARY_CONTEXT_LIMIT);
        let resolved = PhotosResources
            .resource(&doc, &late)
            .expect("late stable id resolves");
        assert_eq!(resolved.id.0, late);
        assert!(resolved.actions.iter().any(|action| action.id == "open"));
        assert!(
            PhotosResources
                .resource(&doc, "photos.photo:stale")
                .is_none()
        );
    }
}
