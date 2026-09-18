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

use tilo_actions::{
    Action, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource,
    ResourceProvider, ResultSpec, RetryPolicy,
};

use crate::doc::PhotosDoc;
use crate::model::{Album, Edits, LibraryPhoto, OpenImage, Rect01};

/// Registry `type_id` for every action this app exports.
pub const PHOTOS_ACTION_TYPE: &str = "photos.action";

/// The resource `kind` an open photograph publishes under.
pub const IMAGE_KIND: &str = "photos.image";
/// The derived, durable browse collection for photographs marked favourite.
///
/// Membership itself is shared document state; opening this resource is a
/// device-local view, just like opening an album or a metadata label.
pub const FAVORITES_KIND: &str = "photos.favorites";
pub const PHOTOS_OPERATION_TYPE: &str = "photos.operation";
/// Contextual enumeration is intentionally small; callers resolve any stable
/// id outside this window through `ResourceProvider::resource`.
const LIBRARY_CONTEXT_LIMIT: usize = 48;
const ALBUM_CONTEXT_LIMIT: usize = 24;
const FOLDER_CONTEXT_LIMIT: usize = 24;
const PERSON_CONTEXT_LIMIT: usize = 24;
const TAG_CONTEXT_LIMIT: usize = 24;
const MAX_RESOURCE_ID_CHARS: usize = 4_096;
const MAX_ALBUM_ID_CHARS: usize = 160;
const MAX_NAME_CHARS: usize = 256;
const MAX_ALBUM_MEMBERS: usize = 4_096;
type LabelCounts = Vec<(String, usize)>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PhotosOperation {
    /// Bring the addressed photograph forward without changing its pixels.
    ///
    /// This is the safe canonical activation used by global search. The host
    /// has already routed the request to the Photos instance that publishes
    /// the resource, so accepting it is enough to reveal the current view.
    View,
    /// Reveal the Photos-owned browsing and management surface without
    /// opening a chooser or mutating the shared library.
    ViewLibrary,
    /// Show the durable favourites collection without changing any shared
    /// favourite marker. This is intentionally its own addressable resource,
    /// rather than a hidden in-app filter, so search, agents, history and a
    /// collaborator can all name the same collection.
    ViewFavorites,
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
    /// Show the bounded library slice carrying this durable tag label.
    ViewTag {
        tag: String,
    },
    Open {
        path: Option<String>,
    },
    ImportCameraMedia {
        media_id: String,
        name: String,
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
    let kind = if matches!(
        op,
        PhotosOperation::View
            | PhotosOperation::ViewLibrary
            | PhotosOperation::ViewFavorites
            | PhotosOperation::ViewAlbum { .. }
            | PhotosOperation::ViewFolder { .. }
            | PhotosOperation::ViewPerson { .. }
            | PhotosOperation::ViewTag { .. }
    ) {
        OperationKind::View
    } else {
        OperationKind::Effect
    };
    let tags = match op {
        PhotosOperation::View
        | PhotosOperation::ViewLibrary
        | PhotosOperation::ViewFavorites
        | PhotosOperation::ViewAlbum { .. }
        | PhotosOperation::ViewFolder { .. }
        | PhotosOperation::ViewPerson { .. }
        | PhotosOperation::ViewTag { .. } => vec!["device-local".into(), "presentation".into()],
        PhotosOperation::Open { .. }
        | PhotosOperation::Save { .. }
        | PhotosOperation::ImportCameraMedia { .. } => vec![
            "device-local".into(),
            "filesystem".into(),
            "portal".into(),
            "sensitive-path".into(),
        ],
        PhotosOperation::Copy | PhotosOperation::ShareReference { .. } => {
            vec!["device-local".into(), "clipboard".into()]
        }
    };
    let contract = if kind == OperationKind::View {
        ActionContract {
            tags,
            ..ActionContract::view()
        }
    } else {
        ActionContract {
            tags,
            ..ActionContract::default()
        }
    };
    let spec = ActionSpec::operation(id, label, kind, PHOTOS_OPERATION_TYPE, op)
        .ok()?
        .with_contract(contract);
    Some(match op {
        PhotosOperation::View
        | PhotosOperation::ViewLibrary
        | PhotosOperation::ViewFavorites
        | PhotosOperation::ViewAlbum { .. }
        | PhotosOperation::ViewFolder { .. }
        | PhotosOperation::ViewPerson { .. }
        | PhotosOperation::ViewTag { .. }
        | PhotosOperation::Open { .. }
        | PhotosOperation::Copy
        | PhotosOperation::ShareReference { .. } => spec.retry_policy(RetryPolicy::Idempotent),
        PhotosOperation::Save { .. } | PhotosOperation::ImportCameraMedia { .. } => {
            spec.retry_policy(RetryPolicy::AtMostOnce)
        }
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

fn valid_name(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= MAX_NAME_CHARS
        && !value.chars().any(char::is_control)
}

fn valid_album_id(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("photos.album:") else {
        return false;
    };
    !suffix.is_empty()
        && value.chars().count() <= MAX_ALBUM_ID_CHARS
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_resource_id(value: &str) -> Result<(), ActionError> {
    if value.trim().is_empty()
        || value.chars().count() > MAX_RESOURCE_ID_CHARS
        || value.chars().any(char::is_control)
    {
        Err(ActionError::Refused(
            "photograph resource id is invalid or too long".into(),
        ))
    } else {
        Ok(())
    }
}

fn valid_photo(photo: &LibraryPhoto) -> bool {
    let canonical_id = LibraryPhoto::from_image(photo.image.clone()).id;
    let valid_content = photo.image.content.as_ref().is_some_and(|hash| {
        hash.len() == CONTENT_HEX_LEN
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    let dimensions = u64::from(photo.image.width).saturating_mul(u64::from(photo.image.height));
    photo.id == canonical_id
        && validate_resource_id(&photo.id).is_ok()
        && valid_name(&photo.image.name)
        && !photo.image.path.trim().is_empty()
        && photo.image.path.chars().count() <= MAX_RESOURCE_ID_CHARS
        && !photo.image.path.chars().any(char::is_control)
        && photo.image.width > 0
        && photo.image.height > 0
        && dimensions <= crate::pixels::MAX_DECODED_PIXELS
        && valid_content
        && valid_metadata(
            photo.folder.as_deref(),
            &photo.people,
            &photo.tags,
            photo.captured_at.as_deref(),
        )
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
                if !valid_photo(photo) {
                    return Err(ActionError::Refused(
                        "photo metadata must be canonical, portable, and bounded".into(),
                    ));
                }
                let mut library = PhotosDoc::library(doc);
                library.add_photo(photo.clone());
                let selected = library.photo(&photo.id).ok_or_else(|| {
                    ActionError::Refused("photo import did not produce a library entry".into())
                })?;
                PhotosDoc::set_library(doc, &library);
                // A duplicate content hash preserves its existing library
                // metadata, and selection follows that canonical record too.
                PhotosDoc::set_image(doc, Some(&selected.image));
                Ok(())
            }
            Self::SelectPhoto { photo_id } => {
                validate_resource_id(photo_id)?;
                let library = PhotosDoc::library(doc);
                let photo = library.photo(photo_id).ok_or_else(|| {
                    ActionError::Refused("that photograph is no longer in the library".into())
                })?;
                PhotosDoc::set_image(doc, Some(&photo.image));
                Ok(())
            }
            Self::RemovePhoto { photo_id } => {
                validate_resource_id(photo_id)?;
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
                validate_resource_id(photo_id)?;
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
                if validate_resource_id(photo_id).is_err()
                    || !valid_metadata(folder.as_deref(), people, tags, captured_at.as_deref())
                {
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
                if !valid_album_id(id) || !valid_name(name) {
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
                if !valid_album_id(id) || !valid_name(name) {
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
                if !valid_album_id(id) {
                    return Err(ActionError::Refused("that album id is invalid".into()));
                }
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
                if !valid_album_id(album_id) || photo_ids.len() > MAX_ALBUM_MEMBERS {
                    return Err(ActionError::Refused(
                        "album membership or stable id is invalid".into(),
                    ));
                }
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
                if !valid_album_id(id) || !valid_name(name) || photo_ids.len() > MAX_ALBUM_MEMBERS {
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
        let mut resources = vec![open_image_resource(doc, &image)];
        resources.extend(library_resources(doc));
        resources
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if id.chars().take(MAX_RESOURCE_ID_CHARS + 1).count() > MAX_RESOURCE_ID_CHARS {
            return None;
        }
        if let Some(image) = PhotosDoc::image(doc).filter(|image| resource_id(image) == id) {
            return Some(open_image_resource(doc, &image));
        }
        if id == "photos.library" {
            return Some(library_root_resource());
        }
        if id == "photos.favorites" {
            return Some(favorites_resource(
                PhotosDoc::library(doc)
                    .photos
                    .iter()
                    .filter(|photo| photo.favorite)
                    .count(),
            ));
        }
        if let Some(photo) = PhotosDoc::photo(doc, id) {
            return Some(photo_resource(&photo));
        }
        if let Some(album) = PhotosDoc::album(doc, id) {
            return Some(album_resource(&album));
        }
        // Exact metadata ids are opaque collision-resistant hashes, so do
        // not attempt to recover them by scanning every photo. The document
        // retains a small label/count projection keyed by these ids; it is
        // updated with the authoritative library write and removes stale
        // labels when their final photo disappears.
        PhotosDoc::metadata_view(doc, id).map(|view| match view.kind {
            crate::doc::MetadataViewKind::Folder => folder_resource(&view.label, view.count),
            crate::doc::MetadataViewKind::Person => person_resource(&view.label, view.count),
            crate::doc::MetadataViewKind::Tag => tag_resource(&view.label, view.count),
        })
    }
}

fn open_image_resource(doc: &AutoCommit, image: &OpenImage) -> Resource {
    let edits = PhotosDoc::edits(doc);
    let (w, h) = image.edited_size(&edits);
    let id = resource_id(image);
    let mut resource = Resource::new(id.clone(), IMAGE_KIND, &image.name);
    if let Some(spec) = operation("view", "View photograph", &PhotosOperation::View) {
        resource = resource.with_action(spec.with_description("Bring this photograph into view"));
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
        )
        .with_action(file_operation("open", "Open another photograph", true));
    if let Some(spec) = operation("copy", "Copy photograph", &PhotosOperation::Copy) {
        resource = resource.with_action(spec);
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
    if !edits.is_identity() {
        resource = resource.with_action(
            immediate_photos_spec("reset", "Reset edits", Edits::default())
                .with_description("Discard the crop and rotation, returning to the original"),
        );
    }
    resource.with_child(Resource::new(
        format!("{id}#size"),
        "photos.size",
        format!("{w} x {h} pixels"),
    ))
}

/// The durable library and album resource tree. Every photo and album gets a
/// stable resource id even when it is not selected in the editor, allowing
/// agents and search to resolve a late library item without a visual grid
/// having first materialised it.
fn library_resources(doc: &AutoCommit) -> Vec<Resource> {
    let library = PhotosDoc::library(doc);
    let mut root = library_root_resource();
    root = root.with_child(favorites_resource(
        library.photos.iter().filter(|photo| photo.favorite).count(),
    ));
    for photo in library.photos.iter().take(LIBRARY_CONTEXT_LIMIT) {
        root = root.with_child(photo_resource(photo));
    }
    for album in library.albums.iter().take(ALBUM_CONTEXT_LIMIT) {
        root = root.with_child(album_resource(album));
    }
    let (folders, people, tags) = metadata_label_counts(&library);
    for (folder, count) in folders.into_iter().take(FOLDER_CONTEXT_LIMIT) {
        root = root.with_child(folder_resource(&folder, count));
    }
    for (person, count) in people.into_iter().take(PERSON_CONTEXT_LIMIT) {
        root = root.with_child(person_resource(&person, count));
    }
    for (tag, count) in tags.into_iter().take(TAG_CONTEXT_LIMIT) {
        root = root.with_child(tag_resource(&tag, count));
    }
    vec![root]
}

pub(crate) fn library_root_resource() -> Resource {
    let mut root = Resource::new("photos.library", "photos.library", "Photo library");
    if let Some(spec) = operation("open", "View photo library", &PhotosOperation::ViewLibrary) {
        root = root.with_action(spec.with_description("Browse and manage photographs in Photos"));
    }
    root = root
        .with_action(file_operation("import", "Import photograph", true))
        .with_action(create_album_spec());
    root
}

/// The stable virtual collection a person expects the star controls to feed.
///
/// This deliberately exists even when it is empty: an empty favourite view is
/// useful as a predictable destination, and it does not falsely imply there
/// are hidden entries waiting to load.
pub(crate) fn favorites_resource(count: usize) -> Resource {
    let mut resource =
        Resource::new("photos.favorites", FAVORITES_KIND, "Favorites").with_child(Resource::new(
            "photos.favorites#count",
            "photos.count",
            format!("{count} photographs"),
        ));
    if let Some(spec) = operation("open", "View favorites", &PhotosOperation::ViewFavorites) {
        resource = resource.with_action(
            spec.with_description("Show photographs marked as favorites in the shared library"),
        );
    }
    resource
}

/// Deterministically collect the small virtual browse namespace.  It is
/// derived only from durable metadata and does no filesystem work, so it is
/// safe to publish to search, agents, history, and collaborators.
pub(crate) fn metadata_label_counts(
    library: &crate::model::PhotoLibrary,
) -> (LabelCounts, LabelCounts, LabelCounts) {
    let mut folders = std::collections::BTreeMap::<String, usize>::new();
    let mut people = std::collections::BTreeMap::<String, usize>::new();
    let mut tags = std::collections::BTreeMap::<String, usize>::new();
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
    }
    (
        folders.into_iter().collect(),
        people.into_iter().collect(),
        tags.into_iter().collect(),
    )
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

/// A tag is durable shared metadata, so its derived browse resource must have
/// the same portable, content-independent identity on every replica.
pub(crate) fn tag_resource_id(tag: &str) -> String {
    format!("photos.tag:{}", qdoc::blobstore::hash_bytes(tag.as_bytes()))
}

pub(crate) fn folder_resource(folder: &str, count: usize) -> Resource {
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

pub(crate) fn person_resource(person: &str, count: usize) -> Resource {
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

pub(crate) fn tag_resource(tag: &str, count: usize) -> Resource {
    let mut item =
        Resource::new(tag_resource_id(tag), "photos.tag", tag).with_child(Resource::new(
            format!("{}#count", tag_resource_id(tag)),
            "photos.count",
            format!("{count} photographs"),
        ));
    if let Some(spec) = operation(
        "open",
        "View tag",
        &PhotosOperation::ViewTag {
            tag: tag.to_owned(),
        },
    ) {
        item = item.with_action(spec.with_description("Show photographs carrying this shared tag"));
    }
    item
}

/// The authoritative subset needed to materialize a photo resource. Search
/// retains this rather than a complete library record, so machine-local path
/// hints and content-store identifiers are not duplicated into its index.
#[derive(Clone)]
pub(crate) struct PhotoResourceSeed {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub favorite: bool,
    pub folder: Option<String>,
    pub people: Vec<String>,
    pub tags: Vec<String>,
    pub captured_at: Option<String>,
}

impl From<&LibraryPhoto> for PhotoResourceSeed {
    fn from(photo: &LibraryPhoto) -> Self {
        Self {
            id: photo.id.clone(),
            name: photo.image.name.clone(),
            width: photo.image.width,
            height: photo.image.height,
            favorite: photo.favorite,
            folder: photo.folder.clone(),
            people: photo.people.clone(),
            tags: photo.tags.clone(),
            captured_at: photo.captured_at.clone(),
        }
    }
}

impl PhotoResourceSeed {
    fn metadata_summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(folder) = &self.folder {
            parts.push(format!("Folder: {folder}"));
        }
        if !self.people.is_empty() {
            parts.push(format!("People: {}", self.people.join(", ")));
        }
        if !self.tags.is_empty() {
            parts.push(format!("Tags: {}", self.tags.join(", ")));
        }
        if let Some(captured_at) = &self.captured_at {
            parts.push(format!("Captured: {captured_at}"));
        }
        parts.join(" · ")
    }
}

pub(crate) fn photo_resource(photo: &LibraryPhoto) -> Resource {
    photo_resource_from_seed(&PhotoResourceSeed::from(photo))
}

pub(crate) fn photo_resource_from_seed(photo: &PhotoResourceSeed) -> Resource {
    let mut item = Resource::new(&photo.id, "photos.photo", &photo.name).with_child(Resource::new(
        format!("{}#size", photo.id),
        "photos.size",
        format!("{} x {} pixels", photo.width, photo.height),
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
    item = item.with_action(photo_metadata_spec(&photo.id));
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

/// A bounded, non-sensitive explanation of a rejected search refresh. It is
/// addressable even while the last-good search generation remains active.
pub(crate) fn search_status_resource(message: &str) -> Resource {
    let mut resource = Resource::new(
        "photos.search-status",
        "photos.diagnostic",
        "Photo search status",
    )
    .with_child(Resource::new(
        "photos.search-status#message",
        "photos.status",
        message,
    ));
    if let Some(spec) = operation("open", "Open photo library", &PhotosOperation::ViewLibrary) {
        resource = resource.with_action(spec);
    }
    resource
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

fn photo_metadata_spec(photo_id: &str) -> ActionSpec {
    let photo_id = photo_id.to_owned();
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

pub(crate) fn album_resource(album: &Album) -> Resource {
    album_resource_from_fields(&album.id, &album.name, album.photo_ids.len())
}

pub(crate) fn album_resource_from_fields(id: &str, name: &str, photo_count: usize) -> Resource {
    let mut item = Resource::new(id, "photos.album", name).with_child(Resource::new(
        format!("{id}#count"),
        "photos.count",
        format!("{photo_count} photographs"),
    ));
    if let Some(spec) = operation(
        "open",
        "Open album",
        &PhotosOperation::ViewAlbum {
            album_id: id.to_owned(),
        },
    ) {
        item = item.with_action(
            spec.with_description("Show this album's photographs without changing membership"),
        );
    }
    if let Some(spec) = direct_spec(
        "remove",
        "Remove album",
        &PhotosAction::RemoveAlbum { id: id.to_owned() },
    ) {
        item = item.with_action(spec.destructive().reversible());
    }
    item = item.with_action(album_rename_spec(id));
    item = item.with_action(album_membership_spec(id));
    item = item.with_action(album_save_spec(id));
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

fn album_save_spec(album_id: &str) -> ActionSpec {
    let id = album_id.to_owned();
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

fn album_rename_spec(album_id: &str) -> ActionSpec {
    let album_id = album_id.to_owned();
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

fn album_membership_spec(album_id: &str) -> ActionSpec {
    let album_id = album_id.to_owned();
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
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                payload: format!("photos file operation could not be serialized: {error}")
                    .into_bytes(),
            })
        },
    )
    .with_contract(ActionContract {
        retry: RetryPolicy::AtMostOnce,
        execution: ExecutionMode::Background { cancellable: true },
        result: ResultSpec::Job,
        tags: vec![
            "device-local".into(),
            "filesystem".into(),
            "portal".into(),
            "sensitive-path".into(),
        ],
        ..ActionContract::default()
    })
}

/// The address of a photograph: what it is, not where a machine put it.
///
/// # Why the path was the wrong name
///
/// This used to be `format!("{IMAGE_KIND}:{path}")`, and the path is true on
/// exactly one machine. Content handed to a room is staged by each session
/// under its own runtime directory, so one photograph had one address per
/// participant — `…/qlva…/tilo-opened/<hash>.png` here, `…/qlvx…/` there,
/// `…/qlvq…/` in the third. An address is resolved by string equality
/// ([`tilo_actions::door::resolve`]), so an operation invoked on one of them
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
/// rendered — the palette row, `tilo_actions::FlatAction::resource_label`, the
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

    fn portable_photo(path: &str, width: u32, height: u32) -> LibraryPhoto {
        let hash = qdoc::blobstore::hash_bytes(path.as_bytes());
        LibraryPhoto::from_image(OpenImage::from_path(path, width, height).with_content(hash))
    }
    use crate::model::OpenImage;
    use tilo_actions::flatten;

    fn doc_with_image() -> AutoCommit {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/var/tmp/tilo/home/quinten/harbour.png", 800, 400);
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
        let img = OpenImage::from_path("/run/user/1000/qlva3099960/tilo-opened/x.png", 800, 400)
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
        let tilo_actions::ActionInput::Immediate(open) = &resources[0].actions[0].input else {
            panic!("library view is an immediate local operation");
        };
        assert_eq!(
            decode_operation(&open.type_id, &open.payload).unwrap(),
            Some(PhotosOperation::ViewLibrary)
        );
        assert!(
            resources[0]
                .actions
                .iter()
                .any(|action| action.id == "import")
        );
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
            "photos.image:/var/tmp/tilo/home/quinten/harbour.png"
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
        let tilo_actions::ActionInput::Immediate(encoded) = &remove.input else {
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
        let tilo_actions::ActionInput::Immediate(encoded) = &share.input else {
            panic!("share is a local effect operation");
        };
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(PhotosOperation::ShareReference { resource_id: id })
        );
    }

    #[test]
    fn shared_metadata_is_bounded_replayable_and_publishes_each_metadata_resource() {
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
        let tag_id = tag_resource_id("harbour");
        for id in [folder_id, person_id, tag_id] {
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
            photo_id: photo_id.clone(),
            folder: Some(" ".into()),
            people: vec!["same".into(), "same".into()],
            tags: Vec::new(),
            captured_at: None,
        }
        .apply(&mut doc)
        .expect_err("malformed metadata must be refused, not normalized");
        assert_eq!(PhotosDoc::library(&doc), before);

        // Exact virtual ids use the compact keyed projection, not a fallback
        // scan of every photo. Removing the final metadata values removes
        // their index rows too, so a stale agent/history address refuses.
        PhotosAction::SetMetadata {
            photo_id,
            folder: None,
            people: Vec::new(),
            tags: Vec::new(),
            captured_at: None,
        }
        .apply(&mut doc)
        .expect("clearing valid metadata applies");
        for id in [
            folder_resource_id("2026 travels"),
            person_resource_id("München-夏"),
            tag_resource_id("harbour"),
        ] {
            assert!(
                PhotosResources.resource(&doc, &id).is_none(),
                "stale metadata id {id} must not materialize"
            );
        }
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
        let tilo_actions::ActionInput::Prompted { build, .. } = &crop.input else {
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
        let tilo_actions::ActionInput::Prompted { build, .. } = &crop.input else {
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
            tilo_actions::ExecutionMode::Background { cancellable: true }
        );
        assert_eq!(open.contract.result, tilo_actions::ResultSpec::Job);
        assert_eq!(open.params()[0].name.as_ref(), "path");
        let copy = res[0].actions.iter().find(|a| a.id == "copy").unwrap();
        assert_eq!(copy.contract.retry, RetryPolicy::Idempotent);
        let view = res[0].actions.iter().find(|a| a.id == "view").unwrap();
        assert_eq!(view.contract.retry, RetryPolicy::Idempotent);
        assert_eq!(view.kind, OperationKind::View);
        assert!(view.contract.tags.iter().any(|tag| tag == "presentation"));
        assert!(view.contract.tags.iter().any(|tag| tag == "device-local"));
        assert_eq!(open.kind, OperationKind::Effect);
        for tag in ["device-local", "filesystem", "portal", "sensitive-path"] {
            assert!(
                open.contract.tags.iter().any(|candidate| candidate == tag),
                "open must disclose its {tag} boundary"
            );
        }
    }

    #[test]
    fn hostile_durable_photo_and_album_payloads_are_refused() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let path_only = LibraryPhoto::from_image(OpenImage::from_path(
            "/private/should-not-be-probed.png",
            20,
            10,
        ));
        assert!(
            PhotosAction::ImportPhoto { photo: path_only }
                .apply(&mut doc)
                .is_err(),
            "a replayed import may not ask another device to probe a local path"
        );
        let hash = "a".repeat(CONTENT_HEX_LEN);
        let image = OpenImage::from_path("/private/harbour.png", 20, 10).with_content(hash);
        let mut photo = LibraryPhoto::from_image(image);
        photo.id = "photos.photo:not-the-content".into();
        assert!(PhotosAction::ImportPhoto { photo }.apply(&mut doc).is_err());

        let huge_name = "x".repeat(MAX_NAME_CHARS + 1);
        assert!(
            PhotosAction::CreateAlbum {
                id: "photos.album:bounded".into(),
                name: huge_name,
            }
            .apply(&mut doc)
            .is_err()
        );
        assert!(
            PhotosAction::CreateAlbum {
                id: "../not-a-stable-album".into(),
                name: "Album".into(),
            }
            .apply(&mut doc)
            .is_err()
        );
        assert!(
            PhotosAction::SetAlbumMembership {
                album_id: "photos.album:bounded".into(),
                photo_ids: vec!["photos.photo:x".into(); MAX_ALBUM_MEMBERS + 1],
            }
            .apply(&mut doc)
            .is_err()
        );
        assert!(PhotosDoc::library(&doc).photos.is_empty());
        assert!(PhotosDoc::library(&doc).albums.is_empty());
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
        let photo = portable_photo("/photos/夏.png", 20, 10);
        PhotosAction::ImportPhoto {
            photo: photo.clone(),
        }
        .apply(&mut doc)
        .unwrap();
        PhotosAction::CreateAlbum {
            id: "photos.album:summer".into(),
            name: "夏".into(),
        }
        .apply(&mut doc)
        .unwrap();
        PhotosAction::SetAlbumMembership {
            album_id: "photos.album:summer".into(),
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
        let photo = portable_photo("/photos/one.png", 20, 10);
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
            photo: portable_photo("/photos/one.png", 20, 10),
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
        assert!(
            root.children
                .iter()
                .filter(|child| child.kind == "photos.photo")
                .count()
                <= LIBRARY_CONTEXT_LIMIT,
            "contextual photos stay bounded even as fixed virtual collections are added"
        );
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

    #[test]
    fn favorites_are_a_stable_browse_resource_with_an_honest_count() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let mut favorite = portable_photo("/photos/favorite.png", 20, 10);
        favorite.favorite = true;
        let ordinary = portable_photo("/photos/ordinary.png", 20, 10);
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![favorite, ordinary],
                albums: Vec::new(),
            },
        );

        let favorites = PhotosResources
            .resource(&doc, "photos.favorites")
            .expect("favorites is addressable even outside a materialized shelf");
        assert_eq!(favorites.kind, FAVORITES_KIND);
        assert!(favorites.actions.iter().any(|action| action.id == "open"));
        assert!(
            favorites
                .children
                .iter()
                .any(|child| child.label == "1 photographs")
        );
        let root = PhotosResources.resources(&doc).pop().expect("library root");
        assert!(
            root.children
                .iter()
                .any(|child| child.id.0 == "photos.favorites")
        );
    }

    #[test]
    fn open_editor_resource_resolves_exactly_without_falling_through_to_library_ids() {
        let doc = doc_with_image();
        let image = PhotosDoc::image(&doc).unwrap();
        let id = resource_id(&image);
        let resource = PhotosResources
            .resource(&doc, &id)
            .expect("the open editor resource is independently addressable");
        assert_eq!(resource.kind, IMAGE_KIND);
        assert!(resource.actions.iter().any(|action| action.id == "crop"));
        assert!(resource.actions.iter().any(|action| action.id == "save"));
    }

    #[test]
    fn exact_lookup_rejects_unbounded_untrusted_ids() {
        let doc = doc_with_image();
        assert!(
            PhotosResources
                .resource(&doc, &"x".repeat(MAX_RESOURCE_ID_CHARS + 1))
                .is_none()
        );
    }
}
