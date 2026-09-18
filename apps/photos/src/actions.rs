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
use crate::model::{Album, Edits, ImageAnalysis, LibraryPhoto, OpenImage, PhotoLocation, Rect01};

/// Registry `type_id` for every action this app exports.
pub const PHOTOS_ACTION_TYPE: &str = "photos.action";

/// The resource `kind` an open photograph publishes under.
pub const IMAGE_KIND: &str = "photos.image";
/// The derived, durable browse collection for photographs marked favourite.
///
/// Membership itself is shared document state; opening this resource is a
/// device-local view, just like opening an album or a metadata label.
pub const FAVORITES_KIND: &str = "photos.favorites";
/// A derived collection of the explicitly shared photo locations.
pub const PLACES_KIND: &str = "photos.places";
/// The complete chronological browse surface. It is distinct from the
/// library resource so an agent can open a position in time rather than an
/// arbitrary insertion-ordered collection.
pub const TIMELINE_KIND: &str = "photos.timeline";
pub const TIMELINE_MONTH_KIND: &str = "photos.timeline.month";
pub const TIMELINE_DAY_KIND: &str = "photos.timeline.day";
pub const PHOTOS_OPERATION_TYPE: &str = "photos.operation";
/// Contextual enumeration is intentionally small; callers resolve any stable
/// id outside this window through `ResourceProvider::resource`.
const LIBRARY_CONTEXT_LIMIT: usize = 48;
const ALBUM_CONTEXT_LIMIT: usize = 24;
const FOLDER_CONTEXT_LIMIT: usize = 24;
const PERSON_CONTEXT_LIMIT: usize = 24;
const TAG_CONTEXT_LIMIT: usize = 24;
/// A resource survey remains small even when the library has years of daily
/// photographs. Exact stable ids and the search catalogue still resolve any
/// extant boundary.
const TIMELINE_MONTH_CONTEXT_LIMIT: usize = 24;
const TIMELINE_DAY_CONTEXT_LIMIT: usize = 48;
const MAX_RESOURCE_ID_CHARS: usize = 4_096;
const MAX_ALBUM_ID_CHARS: usize = 160;
const MAX_NAME_CHARS: usize = 256;
const MAX_ALBUM_MEMBERS: usize = 4_096;
const MAX_BATCH_PHOTOS: usize = 4_096;
const MAX_ANALYSIS_TEXT_CHARS: usize = crate::analysis::MAX_OCR_CHARS;
const MAX_ANALYSIS_LABELS: usize = 64;
const MAX_ANALYSIS_LABEL_CHARS: usize = 128;
type LabelCounts = Vec<(String, usize)>;

/// A stable, metadata-only position in the chronological timeline.
///
/// No timezone conversion or inferred capture date is involved: `Month` and
/// `Day` occur only when an imported/shared capture value actually starts
/// with that ISO calendar boundary. Non-calendar values are deliberately
/// absent instead of being assigned an invented date.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TimelineAnchor {
    Month { month: String },
    Day { day: String },
}

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
    /// Open the chronological timeline, optionally at one canonical calendar
    /// boundary. This is device-local presentation; capture metadata remains
    /// untouched and the same stable anchor is meaningful to every replica.
    ViewTimeline {
        anchor: Option<TimelineAnchor>,
    },
    /// Open Photos' People collection and device-local review surface.
    ViewPeople,
    /// Open the signed local-model manager for face recognition.
    ManagePeopleRecognition,
    /// Explicit per-device opt-in. This changes no collaborative metadata and
    /// does not begin a scan; it only permits the separate scan action.
    EnablePeopleRecognition,
    /// Scan the next bounded local library batch with the separately signed
    /// face model. Suggestions remain device-local until approved.
    AnalyzePeopleLibrary,
    /// Show the durable favourites collection without changing any shared
    /// favourite marker. This is intentionally its own addressable resource,
    /// rather than a hidden in-app filter, so search, agents, history and a
    /// collaborator can all name the same collection.
    ViewFavorites,
    /// Open the offline-first places browser. This changes only local
    /// presentation; it does not disclose a coordinate to a map provider.
    ViewPlaces,
    /// Open one exact shared place cluster.
    ViewPlace {
        place_key: String,
    },
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
    ViewObject {
        object: String,
    },
    ViewPet {
        pet: String,
    },
    Open {
        path: Option<String>,
    },
    /// Import a folder asynchronously.  With no path the desktop portal
    /// grants one; a supplied path is for an already-authorised host/agent.
    ImportDirectory {
        path: Option<String>,
        retain_exif_location: bool,
    },
    ImportCameraMedia {
        media_id: String,
        name: String,
    },
    /// Re-scan a user-selected directory. This intentionally has the same
    /// explicit path boundary as import; Photos never watches arbitrary local
    /// folders or synchronizes their machine-specific path names.
    ReindexDirectory {
        path: String,
        retain_exif_location: bool,
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
    /// Run shared Tilo OCR against locally available bytes, then ask the
    /// document action boundary to publish the compact result. Invoking this
    /// effect is the user's explicit sharing consent; bytes never leave the
    /// device and only text/provenance enter collaboration.
    AnalyzeText {
        resource_id: String,
        language: String,
    },
    /// Run the selected signed local vision model against bounded local pixels.
    /// This is distinct from OCR: it may return a concise description and
    /// generic labels as well as text, and it is never substituted with a
    /// hosted provider.
    AnalyzeImage {
        resource_id: String,
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
            | PhotosOperation::ViewTimeline { .. }
            | PhotosOperation::ViewPeople
            | PhotosOperation::ViewFavorites
            | PhotosOperation::ViewPlaces
            | PhotosOperation::ViewPlace { .. }
            | PhotosOperation::ViewAlbum { .. }
            | PhotosOperation::ViewFolder { .. }
            | PhotosOperation::ViewPerson { .. }
            | PhotosOperation::ViewTag { .. }
            | PhotosOperation::ViewObject { .. }
            | PhotosOperation::ViewPet { .. }
    ) {
        OperationKind::View
    } else {
        OperationKind::Effect
    };
    let tags = match op {
        PhotosOperation::View
        | PhotosOperation::ViewLibrary
        | PhotosOperation::ViewTimeline { .. }
        | PhotosOperation::ViewPeople
        | PhotosOperation::ViewFavorites
        | PhotosOperation::ViewPlaces
        | PhotosOperation::ViewPlace { .. }
        | PhotosOperation::ViewAlbum { .. }
        | PhotosOperation::ViewFolder { .. }
        | PhotosOperation::ViewPerson { .. }
        | PhotosOperation::ViewTag { .. }
        | PhotosOperation::ViewObject { .. }
        | PhotosOperation::ViewPet { .. } => vec!["device-local".into(), "presentation".into()],
        PhotosOperation::Open { .. }
        | PhotosOperation::ImportDirectory { .. }
        | PhotosOperation::Save { .. }
        | PhotosOperation::ImportCameraMedia { .. }
        | PhotosOperation::ReindexDirectory { .. } => vec![
            "device-local".into(),
            "filesystem".into(),
            "portal".into(),
            "sensitive-path".into(),
        ],
        PhotosOperation::Copy | PhotosOperation::ShareReference { .. } => {
            vec!["device-local".into(), "clipboard".into()]
        }
        PhotosOperation::AnalyzeText { .. } | PhotosOperation::AnalyzeImage { .. } => vec![
            "device-local".into(),
            "privacy-sensitive".into(),
            "local-analysis".into(),
            "explicit-share".into(),
        ],
        PhotosOperation::AnalyzePeopleLibrary => vec![
            "device-local".into(),
            "privacy-sensitive".into(),
            "local-analysis".into(),
            "biometric".into(),
            "explicit-consent".into(),
            "not-shared".into(),
        ],
        PhotosOperation::ManagePeopleRecognition => vec![
            "device-local".into(),
            "privacy-sensitive".into(),
            "signed-model".into(),
            "settings-route".into(),
        ],
        PhotosOperation::EnablePeopleRecognition => vec![
            "device-local".into(),
            "privacy-sensitive".into(),
            "explicit-consent".into(),
        ],
    };
    let contract = if kind == OperationKind::View {
        ActionContract {
            tags,
            ..ActionContract::view()
        }
    } else if matches!(
        op,
        PhotosOperation::AnalyzeText { .. }
            | PhotosOperation::AnalyzeImage { .. }
            | PhotosOperation::AnalyzePeopleLibrary
    ) {
        ActionContract {
            tags,
            execution: ExecutionMode::Background { cancellable: true },
            result: ResultSpec::Job,
            ..ActionContract::default()
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
        | PhotosOperation::ViewTimeline { .. }
        | PhotosOperation::ViewPeople
        | PhotosOperation::ViewFavorites
        | PhotosOperation::ViewPlaces
        | PhotosOperation::ViewPlace { .. }
        | PhotosOperation::ViewAlbum { .. }
        | PhotosOperation::ViewFolder { .. }
        | PhotosOperation::ViewPerson { .. }
        | PhotosOperation::ViewTag { .. }
        | PhotosOperation::ViewObject { .. }
        | PhotosOperation::ViewPet { .. }
        | PhotosOperation::Open { .. }
        | PhotosOperation::Copy
        | PhotosOperation::ShareReference { .. } => spec.retry_policy(RetryPolicy::Idempotent),
        PhotosOperation::Save { .. }
        | PhotosOperation::ImportDirectory { .. }
        | PhotosOperation::ImportCameraMedia { .. }
        | PhotosOperation::ReindexDirectory { .. }
        | PhotosOperation::AnalyzeText { .. }
        | PhotosOperation::AnalyzeImage { .. }
        | PhotosOperation::AnalyzePeopleLibrary => spec.retry_policy(RetryPolicy::AtMostOnce),
        PhotosOperation::ManagePeopleRecognition | PhotosOperation::EnablePeopleRecognition => {
            spec.retry_policy(RetryPolicy::Idempotent)
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
    /// Replace the favourite state for a bounded, explicitly addressed batch.
    /// The selection that produced this list is device-local presentation;
    /// this resulting change is the one shared, replayable document action.
    SetFavorites {
        photo_ids: Vec<String>,
        favorite: bool,
    },
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
    /// Replace one person's complete, explicit membership.  This is the
    /// collaboration boundary for the local People review: face clusters,
    /// embeddings and geometry never enter the document, while a person can
    /// deliberately approve the compact labels that should be shared.
    SetPersonMembership {
        /// The visible, user-approved person label.
        person: String,
        /// The complete set of shared photographs carrying that label.
        photo_ids: Vec<String>,
    },
    /// Rename an explicitly shared person label across its complete current
    /// membership.  Naming a label that already exists is a deliberate merge,
    /// not a hidden local face-cluster operation.
    RenamePerson { from: String, to: String },
    /// Remove an explicitly shared person label from all photographs.  This
    /// deletes no photographs and never touches device-local biometric data.
    RemovePerson { person: String },
    /// Replace a photograph's intentionally shared intelligence result.
    /// The action carries final bounded data rather than an OCR request: every
    /// collaborator converges without re-running a local model or reading a
    /// source path that may not exist on their device.
    SetImageAnalysis {
        photo_id: String,
        analysis: Option<ImageAnalysis>,
    },
    /// Set one explicit, already user-authorized shared location. This action
    /// never reverse-geocodes or uploads; `provenance` tells peers where the
    /// retained coordinates came from.
    SetPhotoLocation {
        photo_id: String,
        location: PhotoLocation,
    },
    /// Remove the complete shared location record for one photo. It is a
    /// distinct canonical action so history and collaborators can distinguish
    /// a deliberate privacy removal from a photo without location metadata.
    ClearPhotoLocation { photo_id: String },
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
        && photo.analysis.as_ref().is_none_or(valid_analysis)
        && photo.location.as_ref().is_none_or(PhotoLocation::is_valid)
}

fn valid_analysis(analysis: &ImageAnalysis) -> bool {
    fn compact_text(value: &str, max: usize) -> bool {
        value.chars().count() <= max && !value.chars().any(char::is_control)
    }
    fn labels(values: &[String]) -> bool {
        values.len() <= MAX_ANALYSIS_LABELS
            && values.iter().all(|value| {
                !value.trim().is_empty() && compact_text(value, MAX_ANALYSIS_LABEL_CHARS)
            })
            && values
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                == values.len()
    }
    let valid_ocr = analysis.ocr_text.chars().count() <= MAX_ANALYSIS_TEXT_CHARS
        && analysis
            .ocr_text
            .chars()
            .all(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'));
    let provenance = &analysis.provenance;
    !analysis.is_empty()
        && valid_ocr
        && compact_text(&analysis.description, 1_024)
        && labels(&analysis.objects)
        && labels(&analysis.pets)
        && labels(&analysis.people)
        && matches!(provenance.scope.as_str(), "shared")
        && matches!(
            provenance.source.as_str(),
            "local-ocr" | "local-vision" | "local-description" | "user"
        )
        && compact_text(&provenance.engine, 128)
        && compact_text(&provenance.model_id, 128)
        && (provenance.model_revision.is_empty()
            || (matches!(provenance.model_revision.len(), 40 | 64)
                && provenance
                    .model_revision
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())))
        && (provenance.source != "local-vision"
            || (!provenance.model_id.is_empty()
                && !provenance.model_revision.is_empty()
                && provenance.engine == "tilo-vision"))
        && compact_text(&provenance.language, 16)
        && (provenance.content.is_empty()
            || (provenance.content.len() == CONTENT_HEX_LEN
                && provenance
                    .content
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))))
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
            Self::SetFavorites {
                photo_ids,
                favorite,
            } => {
                if photo_ids.is_empty()
                    || photo_ids.len() > MAX_BATCH_PHOTOS
                    || photo_ids.iter().any(|id| validate_resource_id(id).is_err())
                    || photo_ids.iter().enumerate().any(|(index, id)| {
                        photo_ids
                            .get(..index)
                            .is_some_and(|previous| previous.contains(id))
                    })
                {
                    return Err(ActionError::Refused(
                        "favorite batch must contain unique, valid photograph ids".into(),
                    ));
                }
                let mut library = PhotosDoc::library(doc);
                if !photo_ids.iter().all(|id| library.photo(id).is_some()) {
                    return Err(ActionError::Refused(
                        "a selected photograph is no longer in the library".into(),
                    ));
                }
                for photo in &mut library.photos {
                    if photo_ids.contains(&photo.id) {
                        photo.favorite = *favorite;
                    }
                }
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
            Self::SetPersonMembership { person, photo_ids } => {
                if !valid_person_label(person)
                    || photo_ids.len() > MAX_BATCH_PHOTOS
                    || photo_ids.iter().any(|id| validate_resource_id(id).is_err())
                    || photo_ids.iter().enumerate().any(|(index, id)| {
                        photo_ids
                            .get(..index)
                            .is_some_and(|prior| prior.contains(id))
                    })
                {
                    return Err(ActionError::Refused(
                        "person membership must name a short label and unique valid photograph ids"
                            .into(),
                    ));
                }
                let person = person.trim();
                let mut library = PhotosDoc::library(doc);
                if !photo_ids.iter().all(|id| library.photo(id).is_some()) {
                    return Err(ActionError::Refused(
                        "a person membership photograph is no longer in the library".into(),
                    ));
                }
                // Refuse rather than silently dropping an existing label when
                // a remote caller asks to add a 33rd person to one photo.
                // The People surface should make that conflict explicit.
                for photo in &library.photos {
                    let mut next_people = photo.people.clone();
                    let has_person = next_people.iter().any(|label| label == person);
                    let should_have_person = photo_ids.contains(&photo.id);
                    if should_have_person && !has_person {
                        next_people.push(person.to_owned());
                    } else if !should_have_person && has_person {
                        next_people.retain(|label| label != person);
                    }
                    if !valid_metadata(
                        photo.folder.as_deref(),
                        &next_people,
                        &photo.tags,
                        photo.captured_at.as_deref(),
                    ) {
                        return Err(ActionError::Refused(
                            "person membership would exceed this photograph's metadata limit"
                                .into(),
                        ));
                    }
                }
                for photo in &mut library.photos {
                    let has_person = photo.people.iter().any(|label| label == person);
                    let should_have_person = photo_ids.contains(&photo.id);
                    if should_have_person && !has_person {
                        photo.people.push(person.trim().to_owned());
                    } else if !should_have_person && has_person {
                        photo.people.retain(|label| label != person);
                    }
                }
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::RenamePerson { from, to } => {
                if !valid_person_label(from) || !valid_person_label(to) {
                    return Err(ActionError::Refused(
                        "person labels must be short, non-empty, and free of control characters"
                            .into(),
                    ));
                }
                let from = from.trim();
                let to = to.trim();
                let mut library = PhotosDoc::library(doc);
                if !library
                    .photos
                    .iter()
                    .any(|photo| photo.people.iter().any(|label| label == from))
                {
                    return Err(ActionError::Refused(
                        "that person is no longer in the shared library".into(),
                    ));
                }
                if from != to {
                    for photo in &library.photos {
                        if photo.people.iter().any(|label| label == from) {
                            let mut next_people: Vec<_> = photo
                                .people
                                .iter()
                                .filter(|label| label.as_str() != from)
                                .cloned()
                                .collect();
                            if !next_people.iter().any(|label| label == to) {
                                next_people.push(to.to_owned());
                            }
                            if !valid_metadata(
                                photo.folder.as_deref(),
                                &next_people,
                                &photo.tags,
                                photo.captured_at.as_deref(),
                            ) {
                                return Err(ActionError::Refused(
                                    "renaming this person would exceed a photograph's metadata limit".into(),
                                ));
                            }
                        }
                    }
                    for photo in &mut library.photos {
                        if photo.people.iter().any(|label| label == from) {
                            photo.people.retain(|label| label != from);
                            if !photo.people.iter().any(|label| label == to) {
                                photo.people.push(to.to_owned());
                            }
                        }
                    }
                }
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::RemovePerson { person } => {
                if !valid_person_label(person) {
                    return Err(ActionError::Refused(
                        "person label must be short, non-empty, and free of control characters"
                            .into(),
                    ));
                }
                let person = person.trim();
                let mut library = PhotosDoc::library(doc);
                if !library
                    .photos
                    .iter()
                    .any(|photo| photo.people.iter().any(|label| label == person))
                {
                    return Err(ActionError::Refused(
                        "that person is no longer in the shared library".into(),
                    ));
                }
                for photo in &mut library.photos {
                    photo.people.retain(|label| label != person);
                }
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::SetImageAnalysis { photo_id, analysis } => {
                if validate_resource_id(photo_id).is_err()
                    || analysis
                        .as_ref()
                        .is_some_and(|value| !valid_analysis(value))
                {
                    return Err(ActionError::Refused(
                        "image analysis must be compact, explicitly shared, and carry safe provenance".into(),
                    ));
                }
                let mut library = PhotosDoc::library(doc);
                let photo = library.photo_mut(photo_id).ok_or_else(|| {
                    ActionError::Refused("that photograph is no longer in the library".into())
                })?;
                if let Some(value) = analysis
                    && !value.provenance.content.is_empty()
                    && photo.image.content.as_deref() != Some(&value.provenance.content)
                {
                    return Err(ActionError::Refused(
                        "analysis provenance does not name this photograph's content".into(),
                    ));
                }
                photo.analysis.clone_from(analysis);
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::SetPhotoLocation { photo_id, location } => {
                if validate_resource_id(photo_id).is_err() || !location.is_valid() {
                    return Err(ActionError::Refused(
                        "a shared location needs finite WGS-84 coordinates, an explicit shared scope, and safe provenance".into(),
                    ));
                }
                let mut library = PhotosDoc::library(doc);
                let photo = library.photo_mut(photo_id).ok_or_else(|| {
                    ActionError::Refused("that photograph is no longer in the library".into())
                })?;
                photo.location = Some(location.clone());
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::ClearPhotoLocation { photo_id } => {
                validate_resource_id(photo_id)?;
                let mut library = PhotosDoc::library(doc);
                let photo = library.photo_mut(photo_id).ok_or_else(|| {
                    ActionError::Refused("that photograph is no longer in the library".into())
                })?;
                if photo.location.take().is_none() {
                    return Err(ActionError::Refused(
                        "that photograph does not have a shared location".into(),
                    ));
                }
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
        if id == "photos.timeline" {
            return Some(timeline_resource(&PhotosDoc::library(doc)));
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
        if id == "photos.places" {
            return Some(places_resource(&PhotosDoc::library(doc)));
        }
        if id == "photos.people" {
            return Some(people_root_resource(&PhotosDoc::library(doc)));
        }
        if let Some(photo) = PhotosDoc::photo(doc, id) {
            return Some(photo_resource(&photo));
        }
        if let Some(album) = PhotosDoc::album(doc, id) {
            return Some(album_resource(&album));
        }
        // Place ids hash an opaque compact cluster key.  Unlike the few
        // contextual children, an exact lookup is allowed to scan bounded
        // document metadata; it never touches images, files, or a network.
        let library = PhotosDoc::library(doc);
        if let Some(resource) = timeline_resource_for_id(&library, id) {
            return Some(resource);
        }
        if let Some((key, label, count)) = place_counts(&library)
            .into_iter()
            .find(|(key, _, _)| place_resource_id(key) == id)
        {
            return Some(place_resource(&key, &label, count));
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
            crate::doc::MetadataViewKind::Object => object_resource(&view.label, view.count),
            crate::doc::MetadataViewKind::Pet => pet_resource(&view.label, view.count),
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
    root = root.with_child(places_resource(&library));
    root = root.with_child(people_root_resource(&library));
    root = root.with_child(timeline_resource(&library));
    for photo in library.photos.iter().take(LIBRARY_CONTEXT_LIMIT) {
        root = root.with_child(photo_resource(photo));
    }
    for album in library.albums.iter().take(ALBUM_CONTEXT_LIMIT) {
        root = root.with_child(album_resource(album));
    }
    let (folders, people, tags, objects, pets) = metadata_label_counts(&library);
    for (folder, count) in folders.into_iter().take(FOLDER_CONTEXT_LIMIT) {
        root = root.with_child(folder_resource(&folder, count));
    }
    for (person, count) in people.into_iter().take(PERSON_CONTEXT_LIMIT) {
        root = root.with_child(person_resource(&person, count));
    }
    for (tag, count) in tags.into_iter().take(TAG_CONTEXT_LIMIT) {
        root = root.with_child(tag_resource(&tag, count));
    }
    for (object, count) in objects.into_iter().take(TAG_CONTEXT_LIMIT) {
        root = root.with_child(object_resource(&object, count));
    }
    for (pet, count) in pets.into_iter().take(TAG_CONTEXT_LIMIT) {
        root = root.with_child(pet_resource(&pet, count));
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
        .with_action(import_directory_spec(false))
        .with_action(import_directory_spec(true))
        .with_action(directory_import_spec())
        .with_action(create_album_spec())
        .with_action(set_favorites_spec());
    root
}

/// Compact, shared-metadata-only counts for timeline navigation.  The vectors
/// are newest-first to match the rendered timeline, while every entry is
/// still addressable by its stable id outside the contextual resource window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TimelineBoundaries {
    pub months: Vec<(String, usize)>,
    pub days: Vec<(String, usize)>,
}

pub(crate) fn timeline_boundaries(library: &crate::model::PhotoLibrary) -> TimelineBoundaries {
    let mut months = std::collections::BTreeMap::<String, usize>::new();
    let mut days = std::collections::BTreeMap::<String, usize>::new();
    for photo in &library.photos {
        let Some(day) = photo
            .captured_at
            .as_deref()
            .and_then(timeline_day_from_capture)
        else {
            continue;
        };
        let month = day[..7].to_owned();
        *months.entry(month).or_default() += 1;
        *days.entry(day).or_default() += 1;
    }
    TimelineBoundaries {
        months: months.into_iter().rev().collect(),
        days: days.into_iter().rev().collect(),
    }
}

pub(crate) fn timeline_day_from_capture(captured_at: &str) -> Option<String> {
    let day = captured_at.get(..10)?;
    let bytes = day.as_bytes();
    let month = std::str::from_utf8(&bytes[5..7]).ok()?.parse::<u8>().ok()?;
    let day_of_month = std::str::from_utf8(&bytes[8..10])
        .ok()?
        .parse::<u8>()
        .ok()?;
    let year = std::str::from_utf8(&bytes[..4]).ok()?.parse::<u16>().ok()?;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    };
    (bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
        && (1..=12).contains(&month)
        && (1..=days_in_month).contains(&day_of_month))
    .then(|| day.to_owned())
}

pub(crate) fn timeline_month_resource_id(month: &str) -> String {
    format!("photos.timeline:month:{month}")
}

pub(crate) fn timeline_day_resource_id(day: &str) -> String {
    format!("photos.timeline:day:{day}")
}

pub(crate) fn timeline_resource(library: &crate::model::PhotoLibrary) -> Resource {
    let boundaries = timeline_boundaries(library);
    timeline_resource_from_search(library.photos.len(), &boundaries)
}

pub(crate) fn timeline_resource_from_search(
    total: usize,
    boundaries: &TimelineBoundaries,
) -> Resource {
    let mut resource = Resource::new(
        "photos.timeline",
        TIMELINE_KIND,
        crate::strings::tr("photos-timeline"),
    )
    .with_child(Resource::new(
        "photos.timeline#count",
        "photos.count",
        format!("{total} {}", crate::strings::tr("photos-photographs")),
    ));
    if let Some(spec) = operation(
        "open",
        &crate::strings::tr("photos-open-timeline"),
        &PhotosOperation::ViewTimeline { anchor: None },
    ) {
        resource = resource
            .with_action(spec.with_description(crate::strings::tr("photos-timeline-description")));
    }
    for (month, count) in boundaries.months.iter().take(TIMELINE_MONTH_CONTEXT_LIMIT) {
        resource = resource.with_child(timeline_month_resource(month, *count));
    }
    for (day, count) in boundaries.days.iter().take(TIMELINE_DAY_CONTEXT_LIMIT) {
        resource = resource.with_child(timeline_day_resource(day, *count));
    }
    resource
}

pub(crate) fn timeline_month_resource(month: &str, count: usize) -> Resource {
    let mut resource = Resource::new(
        timeline_month_resource_id(month),
        TIMELINE_MONTH_KIND,
        month,
    )
    .with_child(Resource::new(
        format!("{}#count", timeline_month_resource_id(month)),
        "photos.count",
        format!("{count} {}", crate::strings::tr("photos-photographs")),
    ));
    if let Some(spec) = operation(
        "open",
        &crate::strings::tr("photos-open-timeline-month"),
        &PhotosOperation::ViewTimeline {
            anchor: Some(TimelineAnchor::Month {
                month: month.to_owned(),
            }),
        },
    ) {
        resource = resource.with_action(
            spec.with_description(crate::strings::tr("photos-timeline-month-description")),
        );
    }
    resource
}

pub(crate) fn timeline_day_resource(day: &str, count: usize) -> Resource {
    let mut resource = Resource::new(timeline_day_resource_id(day), TIMELINE_DAY_KIND, day)
        .with_child(Resource::new(
            format!("{}#count", timeline_day_resource_id(day)),
            "photos.count",
            format!("{count} {}", crate::strings::tr("photos-photographs")),
        ));
    if let Some(spec) = operation(
        "open",
        &crate::strings::tr("photos-open-timeline-day"),
        &PhotosOperation::ViewTimeline {
            anchor: Some(TimelineAnchor::Day {
                day: day.to_owned(),
            }),
        },
    ) {
        resource = resource.with_action(
            spec.with_description(crate::strings::tr("photos-timeline-day-description")),
        );
    }
    resource
}

fn timeline_resource_for_id(library: &crate::model::PhotoLibrary, id: &str) -> Option<Resource> {
    let boundaries = timeline_boundaries(library);
    if let Some(month) = id.strip_prefix("photos.timeline:month:") {
        if let Some((_, count)) = boundaries
            .months
            .iter()
            .find(|(candidate, _)| candidate == month)
        {
            return Some(timeline_month_resource(month, *count));
        }
        return None;
    }
    if let Some(day) = id.strip_prefix("photos.timeline:day:") {
        if let Some((_, count)) = boundaries
            .days
            .iter()
            .find(|(candidate, _)| candidate == day)
        {
            return Some(timeline_day_resource(day, *count));
        }
        return None;
    }
    None
}

fn directory_import_spec() -> ActionSpec {
    ActionSpec::typed_operation(
        "import-folder",
        "Import photo folder",
        "Choose a folder and import its supported photographs in the background",
        OperationKind::Effect,
        PHOTOS_OPERATION_TYPE,
        vec![ActionParam::optional_owned(
            "path",
            ParamType::Text,
            "Authorised folder path",
        )],
        |args: &ActionArgs| {
            EncodedAction::encode_operation(
                PHOTOS_OPERATION_TYPE,
                &PhotosOperation::ImportDirectory {
                    path: args
                        .text("path")
                        .filter(|path| !path.trim().is_empty())
                        .map(str::to_owned),
                    retain_exif_location: true,
                },
            )
            .unwrap_or_else(|error| EncodedAction {
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                payload: format!("photos folder import could not be serialized: {error}")
                    .into_bytes(),
            })
        },
    )
    .with_description("Imports supported photo files with progress; cancelling stops future files")
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

/// First-class People collection derived only from explicitly approved,
/// collaborative labels. Private face proposals never enter this resource.
pub(crate) fn people_root_resource(library: &crate::model::PhotoLibrary) -> Resource {
    let count = crate::people::approved_people(library).len();
    let mut resource =
        Resource::new("photos.people", "photos.people", "People").with_child(Resource::new(
            "photos.people#approved-count",
            "photos.count",
            format!("{count} approved people"),
        ));
    if let Some(spec) = operation("open", "Open People", &PhotosOperation::ViewPeople) {
        resource = resource.with_action(
            spec.with_description("Review local suggestions and browse approved people"),
        );
    }
    if let Some(spec) = operation(
        "manage-recognition",
        "Manage local recognition model",
        &PhotosOperation::ManagePeopleRecognition,
    ) {
        resource = resource.with_action(spec.with_description(
            "Install or manage a signed local face-recognition model in Settings",
        ));
    }
    if let Some(spec) = operation(
        "enable-local-recognition",
        "Enable private People recognition",
        &PhotosOperation::EnablePeopleRecognition,
    ) {
        resource = resource.with_action(spec.with_description(
            "Allow signed local face recognition on this device; it does not scan or share anything yet",
        ));
    }
    if let Some(spec) = operation(
        "scan-local-library",
        "Scan local library for faces",
        &PhotosOperation::AnalyzePeopleLibrary,
    ) {
        resource = resource.with_action(spec.with_description(
            "Use the opt-in signed local face model on a bounded local batch; suggestions remain private until approved",
        ));
    }
    resource
}

/// The stable derived collection of locations that were explicitly shared
/// with this library. It is always present, including when empty, so a global
/// shortcut/agent address never depends on which cards happen to be visible.
pub(crate) fn places_resource(library: &crate::model::PhotoLibrary) -> Resource {
    let count = library
        .photos
        .iter()
        .filter(|photo| photo.location.is_some())
        .count();
    let mut resource = places_resource_from_count(count);
    for (key, label, photos) in place_counts(library).into_iter().take(TAG_CONTEXT_LIMIT) {
        resource = resource.with_child(place_resource(&key, &label, photos));
    }
    resource
}

pub(crate) fn places_resource_from_count(count: usize) -> Resource {
    let mut resource =
        Resource::new("photos.places", PLACES_KIND, "Places").with_child(Resource::new(
            "photos.places#count",
            "photos.count",
            format!("{count} located photographs"),
        ));
    if let Some(spec) = operation("open", "View places", &PhotosOperation::ViewPlaces) {
        resource = resource.with_action(spec.with_description(
            "Browse explicitly shared photo locations on an offline-first map; Photos never uploads coordinates",
        ));
    }
    resource
}

/// Deterministic, bounded place clusters based only on intentional location
/// metadata. The returned label is safe for a resource tree and the key is
/// addressable by search, history and collaborators.
pub(crate) fn place_counts(library: &crate::model::PhotoLibrary) -> Vec<(String, String, usize)> {
    let mut counts = std::collections::BTreeMap::<String, (String, usize)>::new();
    for photo in &library.photos {
        let Some(location) = &photo.location else {
            continue;
        };
        let key = location.place_key();
        let entry = counts
            .entry(key)
            .or_insert_with(|| (location.display_name(), 0));
        entry.1 += 1;
    }
    counts
        .into_iter()
        .map(|(key, (label, count))| (key, label, count))
        .collect()
}

pub(crate) fn place_resource_id(place_key: &str) -> String {
    format!(
        "photos.place:{}",
        qdoc::blobstore::hash_bytes(place_key.as_bytes())
    )
}

pub(crate) fn place_resource(place_key: &str, label: &str, count: usize) -> Resource {
    let id = place_resource_id(place_key);
    let mut resource = Resource::new(&id, "photos.place", label).with_child(Resource::new(
        format!("{id}#count"),
        "photos.count",
        format!("{count} photographs"),
    ));
    if let Some(spec) = operation(
        "open",
        "View place",
        &PhotosOperation::ViewPlace {
            place_key: place_key.to_owned(),
        },
    ) {
        resource = resource.with_action(spec.with_description(
            "Show this offline-capable place cluster; no location is sent to a map service",
        ));
    }
    resource
}

/// Deterministically collect the small virtual browse namespace.  It is
/// derived only from durable metadata and does no filesystem work, so it is
/// safe to publish to search, agents, history, and collaborators.
pub(crate) fn metadata_label_counts(
    library: &crate::model::PhotoLibrary,
) -> (
    LabelCounts,
    LabelCounts,
    LabelCounts,
    LabelCounts,
    LabelCounts,
) {
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
    (
        folders.into_iter().collect(),
        people.into_iter().collect(),
        tags.into_iter().collect(),
        objects.into_iter().collect(),
        pets.into_iter().collect(),
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

pub(crate) fn object_resource_id(object: &str) -> String {
    format!(
        "photos.object:{}",
        qdoc::blobstore::hash_bytes(object.as_bytes())
    )
}

pub(crate) fn pet_resource_id(pet: &str) -> String {
    format!("photos.pet:{}", qdoc::blobstore::hash_bytes(pet.as_bytes()))
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
    item = item
        .with_action(person_membership_spec(person))
        .with_action(person_rename_spec(person));
    if let Some(spec) = direct_spec(
        "remove-person",
        "Remove person label",
        &PhotosAction::RemovePerson {
            person: person.to_owned(),
        },
    ) {
        item =
            item.with_action(spec.destructive().reversible().with_description(
                "Remove this approved person label from every shared photograph",
            ));
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

pub(crate) fn object_resource(object: &str, count: usize) -> Resource {
    let mut item = Resource::new(object_resource_id(object), "photos.object", object).with_child(
        Resource::new(
            format!("{}#count", object_resource_id(object)),
            "photos.count",
            format!("{count} photographs"),
        ),
    );
    if let Some(spec) = operation(
        "open",
        "View object",
        &PhotosOperation::ViewObject {
            object: object.to_owned(),
        },
    ) {
        item = item.with_action(
            spec.with_description("Show photographs with this explicitly shared object label"),
        );
    }
    item
}

pub(crate) fn pet_resource(pet: &str, count: usize) -> Resource {
    let mut item =
        Resource::new(pet_resource_id(pet), "photos.pet", pet).with_child(Resource::new(
            format!("{}#count", pet_resource_id(pet)),
            "photos.count",
            format!("{count} photographs"),
        ));
    if let Some(spec) = operation(
        "open",
        "View pet",
        &PhotosOperation::ViewPet {
            pet: pet.to_owned(),
        },
    ) {
        item = item.with_action(
            spec.with_description("Show photographs with this explicitly shared pet label"),
        );
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
    pub analysis: Option<ImageAnalysis>,
    pub location: Option<PhotoLocation>,
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
            analysis: photo.analysis.clone(),
            location: photo.location.clone(),
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
        if let Some(location) = &self.location {
            parts.push(format!("Place: {}", location.display_name()));
        }
        if let Some(analysis) = &self.analysis {
            if !analysis.description.is_empty() {
                parts.push(format!("Description: {}", analysis.description));
            }
            if !analysis.objects.is_empty() {
                parts.push(format!("Objects: {}", analysis.objects.join(", ")));
            }
            if !analysis.pets.is_empty() {
                parts.push(format!("Pets: {}", analysis.pets.join(", ")));
            }
        }
        parts.join(" · ")
    }
}

pub(crate) fn photo_resource(photo: &LibraryPhoto) -> Resource {
    photo_resource_from_seed(&PhotoResourceSeed::from(photo))
}

/// Return the human-safe reason the explicit local-vision operation cannot be
/// admitted on this device. This reads only Settings' non-secret provider
/// description; discovery never probes an endpoint and never falls back to a
/// remote provider.
pub(crate) fn local_vision_unavailable_reason() -> Option<String> {
    tilo_vision::discover_local_capability(&agent_provider_config::ProviderConfig::load())
        .is_none()
        .then(|| {
            "No verified local image-analysis model is ready. Install and start one in Settings."
                .to_owned()
        })
}

#[allow(
    clippy::too_many_lines,
    reason = "the complete context action surface stays adjacent to its resource declaration"
)]
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
    if let Some(analysis) = &photo.analysis {
        item = item.with_child(Resource::new(
            format!("{}#analysis", photo.id),
            "photos.analysis",
            if analysis.description.is_empty() {
                "Shared image intelligence".to_owned()
            } else {
                analysis.description.clone()
            },
        ));
    }
    if let Some(location) = &photo.location {
        item = item.with_child(Resource::new(
            format!("{}#location", photo.id),
            "photos.location",
            format!(
                "{} · explicitly shared from {}",
                location.display_name(),
                location.provenance.source
            ),
        ));
        if let Some(spec) = operation(
            "view-place",
            "View on map",
            &PhotosOperation::ViewPlace {
                place_key: location.place_key(),
            },
        ) {
            item = item.with_action(spec.with_description(
                "Open the local Places view without sending this coordinate anywhere",
            ));
        }
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
    item = item.with_action(photo_location_spec(&photo.id));
    if photo.location.is_some()
        && let Some(spec) = direct_spec(
            "clear-location",
            "Remove shared location",
            &PhotosAction::ClearPhotoLocation {
                photo_id: photo.id.clone(),
            },
        )
    {
        item = item.with_action(spec.destructive().reversible().with_description(
            "Remove the explicit shared location from this photograph; no files or pixels are changed",
        ));
    }
    if let Some(spec) = operation(
        "recognize-text",
        "Recognize text locally",
        &PhotosOperation::AnalyzeText {
            resource_id: photo.id.clone(),
            language: "eng".to_owned(),
        },
    ) {
        item = item.with_action(spec.with_description(
            "Run local OCR and explicitly share only bounded text and provenance",
        ));
    }
    if let Some(spec) = operation(
        "analyze-image",
        "Analyze image locally",
        &PhotosOperation::AnalyzeImage {
            resource_id: photo.id.clone(),
        },
    ) {
        let unavailable = local_vision_unavailable_reason();
        let description = if let Some(reason) = unavailable.as_deref() {
            format!("Unavailable: {reason}")
        } else {
            "Use the signed local model to describe this image, extract generic labels and recognize text; explicitly share only the bounded result".to_owned()
        };
        let spec = spec.with_description(description);
        item = item.with_action(match unavailable {
            Some(reason) => spec.unavailable("local_vision_unavailable", reason),
            None => spec,
        });
    }
    if photo.analysis.is_some()
        && let Some(spec) = direct_spec(
            "remove-analysis",
            "Remove shared analysis",
            &PhotosAction::SetImageAnalysis {
                photo_id: photo.id.clone(),
                analysis: None,
            },
        )
    {
        item = item.with_action(spec.destructive().reversible());
    }
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

/// A person is a compact user-authored label.  This intentionally has the
/// same limit and character rules as the `people` metadata array so a review
/// action can never create a record an ordinary metadata edit would reject.
fn valid_person_label(value: &str) -> bool {
    !value.trim().is_empty()
        && value.trim().chars().count() <= 128
        && !value.chars().any(char::is_control)
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

fn photo_location_spec(photo_id: &str) -> ActionSpec {
    let photo_id = photo_id.to_owned();
    ActionSpec::typed(
        "set-location",
        "Set shared location",
        "Explicit WGS-84 coordinates and optional place label",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("latitude", ParamType::Number, "latitude from -90 to 90"),
            ActionParam::required("longitude", ParamType::Number, "longitude from -180 to 180"),
            ActionParam::optional("label", ParamType::Text, "optional shared place label"),
            ActionParam::optional(
                "source",
                ParamType::Text,
                "user, exif, or device; defaults to user",
            ),
        ],
        move |args| {
            encode(&PhotosAction::SetPhotoLocation {
                photo_id: photo_id.clone(),
                location: PhotoLocation {
                    latitude: args.number("latitude").unwrap_or(f64::NAN),
                    longitude: args.number("longitude").unwrap_or(f64::NAN),
                    label: optional_metadata_text(args, "label"),
                    provenance: crate::model::LocationProvenance {
                        source: optional_metadata_text(args, "source")
                            .unwrap_or_else(|| "user".to_owned()),
                        scope: "shared".to_owned(),
                    },
                },
            })
        },
    )
    .reversible()
    .with_description(
        "Explicitly share a compact location. Photos never uploads it for geocoding or map tiles.",
    )
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
    args.array("photo_ids")
        .unwrap_or_default()
        .iter()
        // The parameter schema makes every item a non-empty string before
        // this builder runs. Keep an unexpected value as an invalid id rather
        // than dropping it and accidentally changing a caller's request.
        .map(|value| {
            value
                .as_str()
                .map_or_else(|| "\u{0}".to_owned(), str::to_owned)
        })
        .collect()
}

fn photo_ids_param(required: bool, description: &'static str, min_items: usize) -> ActionParam {
    let param = if required {
        ActionParam::required("photo_ids", ParamType::Array, description)
    } else {
        ActionParam::optional("photo_ids", ParamType::Array, description)
    };
    param.with_schema(serde_json::json!({
        "type": "array",
        "minItems": min_items,
        "maxItems": MAX_BATCH_PHOTOS,
        "uniqueItems": true,
        "items": {
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_RESOURCE_ID_CHARS
        }
    }))
}

/// The complete membership is supplied by an explicit People review.  The
/// visible proposal/cluster remains device-local; this action carries only
/// already-approved photograph ids and a label a collaborator can understand.
fn person_membership_spec(person: &str) -> ActionSpec {
    let person = person.to_owned();
    ActionSpec::typed(
        "set-membership",
        "Set person membership",
        "JSON array of approved photos.photo:… ids (an empty array removes the label)",
        ExecutionLocus::ClientLocal,
        vec![photo_ids_param(
            true,
            "JSON array of stable photo ids; an empty array removes this person label",
            0,
        )],
        move |args| {
            encode(&PhotosAction::SetPersonMembership {
                person: person.clone(),
                photo_ids: membership_ids(args),
            })
        },
    )
    .reversible()
    .with_description("Replace this approved person's shared photograph membership atomically")
}

/// Renaming to an existing label deliberately merges explicit labels.  It
/// does not, and cannot, merge private local face clusters.
fn person_rename_spec(person: &str) -> ActionSpec {
    let from = person.to_owned();
    ActionSpec::typed(
        "rename",
        "Rename or merge person",
        "New approved person label; an existing label merges shared memberships",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "name",
            ParamType::Text,
            "new approved person label",
        )],
        move |args| {
            encode(&PhotosAction::RenamePerson {
                from: from.clone(),
                to: args.text("name").unwrap_or_default().trim().to_owned(),
            })
        },
    )
    .reversible()
    .with_description("Rename this approved person label, or merge it into an existing label")
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
            photo_ids_param(false, "JSON array of stable photo ids", 0),
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

/// The agent/search counterpart of the visible library selection command.
/// Selection never leaks into the shared document; callers name exactly the
/// resources whose durable favorite markers should change.
fn set_favorites_spec() -> ActionSpec {
    ActionSpec::typed(
        "set-favorites",
        "Set favorites",
        "Photograph ids and desired favorite state",
        ExecutionLocus::ClientLocal,
        vec![
            photo_ids_param(
                true,
                "non-empty JSON array of stable photograph resource ids",
                1,
            ),
            ActionParam::required("favorite", ParamType::Boolean, "whether to mark favorite"),
        ],
        |args| {
            encode(&PhotosAction::SetFavorites {
                photo_ids: membership_ids(args),
                favorite: args.boolean("favorite").unwrap_or(false),
            })
        },
    )
    .reversible()
    .with_description("Set the favorite marker for a bounded batch of selected photographs")
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
            photo_ids_param(false, "JSON array of stable photo ids", 0),
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
        "JSON array of photos.photo:… ids (an empty array clears the album)",
        ExecutionLocus::ClientLocal,
        vec![photo_ids_param(
            true,
            "JSON array of stable photo ids; an empty array clears the album",
            0,
        )],
        move |args: &ActionArgs| {
            encode(&PhotosAction::SetAlbumMembership {
                album_id: album_id.clone(),
                photo_ids: membership_ids(args),
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

/// The indexer is a local job rather than a document action because directory
/// enumeration depends on this machine's consented filesystem. Each result is
/// later admitted through `ImportPhoto`, preserving collaboration/history.
fn import_directory_spec(reindex: bool) -> ActionSpec {
    let id = if reindex {
        "reindex-directory"
    } else {
        "import-directory"
    };
    let label = if reindex {
        "Reindex photo folder"
    } else {
        "Import photo folder"
    };
    ActionSpec::typed_operation(
        id,
        label,
        "A local folder and EXIF-location choice",
        OperationKind::Effect,
        PHOTOS_OPERATION_TYPE,
        vec![
            ActionParam::required("path", ParamType::Text, "explicit local folder path"),
            ActionParam::optional(
                "retain_exif_location",
                ParamType::Boolean,
                "retain GPS read from imported files; defaults to true",
            ),
        ],
        move |args| {
            let path = args.text("path").unwrap_or_default().trim().to_owned();
            let retain_exif_location = args.boolean("retain_exif_location").unwrap_or(true);
            let operation = if reindex {
                PhotosOperation::ReindexDirectory { path, retain_exif_location }
            } else {
                PhotosOperation::ImportDirectory {
                    path: Some(path),
                    retain_exif_location,
                }
            };
            EncodedAction::encode_operation(PHOTOS_OPERATION_TYPE, &operation).unwrap_or_else(|error| EncodedAction {
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                payload: format!("photos directory operation could not be serialized: {error}").into_bytes(),
            })
        },
    )
    .with_contract(ActionContract {
        retry: RetryPolicy::AtMostOnce,
        execution: ExecutionMode::Background { cancellable: true },
        result: ResultSpec::Job,
        tags: vec!["device-local".into(), "filesystem".into(), "portal".into(), "sensitive-path".into(), "bounded-index".into()],
        ..ActionContract::default()
    })
    .with_description("Recursively index a selected folder with bounded real-file reads; paths never enter collaboration and EXIF GPS is never uploaded")
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
        let img = OpenImage::from_path("/var/tmp/tilo/home/user/harbour.png", 800, 400);
        PhotosDoc::set_image(&mut doc, Some(&img));
        doc
    }

    fn shared_analysis(content: String) -> ImageAnalysis {
        ImageAnalysis {
            ocr_text: "invoice total forty two".to_owned(),
            description: "A dog beside a red bicycle".to_owned(),
            objects: vec!["bicycle".to_owned()],
            pets: vec!["dog".to_owned()],
            people: vec!["Sam".to_owned()],
            provenance: crate::model::AnalysisProvenance {
                source: "local-ocr".to_owned(),
                engine: "tilo-ocr".to_owned(),
                language: "eng".to_owned(),
                content,
                scope: "shared".to_owned(),
                ..crate::model::AnalysisProvenance::default()
            },
        }
    }

    #[test]
    fn explicit_location_is_typed_searchable_addressable_and_can_be_cleared() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let photo = portable_photo("place.png", 40, 20);
        let id = photo.id.clone();
        PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();
        let location = PhotoLocation {
            latitude: 37.7749,
            longitude: -122.4194,
            label: Some("San Francisco".to_owned()),
            provenance: crate::model::LocationProvenance {
                source: "exif".to_owned(),
                scope: "shared".to_owned(),
            },
        };
        PhotosAction::SetPhotoLocation {
            photo_id: id.clone(),
            location: location.clone(),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            PhotosDoc::photo(&doc, &id).unwrap().location,
            Some(location.clone())
        );
        let place_id = place_resource_id(&location.place_key());
        let place = PhotosResources.resource(&doc, &place_id).unwrap();
        assert_eq!(place.kind, "photos.place");
        assert!(place.actions.iter().any(|action| action.id == "open"));
        let photo_resource = PhotosResources.resource(&doc, &id).unwrap();
        assert!(
            photo_resource
                .actions
                .iter()
                .any(|action| action.id == "view-place")
        );
        assert!(
            photo_resource
                .actions
                .iter()
                .any(|action| action.id == "clear-location")
        );
        PhotosAction::ClearPhotoLocation {
            photo_id: id.clone(),
        }
        .apply(&mut doc)
        .unwrap();
        assert!(PhotosDoc::photo(&doc, &id).unwrap().location.is_none());
        assert!(PhotosResources.resource(&doc, &place_id).is_none());
    }

    #[test]
    fn locations_refuse_local_scope_or_out_of_range_coordinates() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let photo = portable_photo("invalid-place.png", 40, 20);
        let id = photo.id.clone();
        PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();
        let invalid = PhotoLocation {
            latitude: 91.0,
            longitude: 0.0,
            label: None,
            provenance: crate::model::LocationProvenance {
                source: "user".to_owned(),
                scope: "local".to_owned(),
            },
        };
        assert!(
            PhotosAction::SetPhotoLocation {
                photo_id: id,
                location: invalid
            }
            .apply(&mut doc)
            .is_err()
        );
    }

    #[test]
    fn explicit_shared_analysis_is_bounded_addressable_and_removable() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let photo = portable_photo("analysis.png", 40, 20);
        let id = photo.id.clone();
        let content = photo.image.content.clone().unwrap();
        PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();
        PhotosAction::SetImageAnalysis {
            photo_id: id.clone(),
            analysis: Some(shared_analysis(content)),
        }
        .apply(&mut doc)
        .unwrap();

        let stored = PhotosDoc::photo(&doc, &id).unwrap();
        assert_eq!(stored.analysis.unwrap().ocr_text, "invoice total forty two");
        assert!(
            PhotosResources
                .resource(&doc, &object_resource_id("bicycle"))
                .is_some()
        );
        assert!(
            PhotosResources
                .resource(&doc, &pet_resource_id("dog"))
                .is_some()
        );
        let resource = PhotosResources.resource(&doc, &id).unwrap();
        assert!(
            resource
                .actions
                .iter()
                .any(|action| action.id == "recognize-text")
        );
        assert!(
            resource
                .actions
                .iter()
                .any(|action| action.id == "remove-analysis")
        );

        let local = ImageAnalysis {
            provenance: crate::model::AnalysisProvenance {
                scope: "local".to_owned(),
                ..crate::model::AnalysisProvenance::default()
            },
            ocr_text: "must not silently sync".to_owned(),
            ..ImageAnalysis::default()
        };
        assert!(
            PhotosAction::SetImageAnalysis {
                photo_id: id,
                analysis: Some(local),
            }
            .apply(&mut doc)
            .is_err()
        );
    }

    #[test]
    fn image_analysis_is_a_cancellable_background_operation() {
        let spec = operation(
            "analyze-image",
            "Analyze image locally",
            &PhotosOperation::AnalyzeImage {
                resource_id: "photos.photo:abc".to_owned(),
            },
        )
        .expect("operation serializes");
        assert_eq!(spec.kind, OperationKind::Effect);
        assert_eq!(
            spec.contract.execution,
            ExecutionMode::Background { cancellable: true }
        );
        assert_eq!(spec.contract.result, ResultSpec::Job);
        assert_eq!(spec.contract.retry, RetryPolicy::AtMostOnce);
        for tag in [
            "device-local",
            "privacy-sensitive",
            "local-analysis",
            "explicit-share",
        ] {
            assert!(spec.contract.tags.iter().any(|candidate| candidate == tag));
        }
    }

    #[test]
    fn vision_analysis_requires_a_pinned_model_provenance() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let photo = portable_photo("vision.png", 40, 20);
        let id = photo.id.clone();
        let content = photo.image.content.clone().unwrap();
        PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();
        let analysis = ImageAnalysis {
            description: "A bicycle outside".to_owned(),
            objects: vec!["bicycle".to_owned()],
            provenance: crate::model::AnalysisProvenance {
                source: "local-vision".to_owned(),
                engine: "tilo-vision".to_owned(),
                model_id: "signed-vision-model".to_owned(),
                model_revision: "a".repeat(64),
                content,
                scope: "shared".to_owned(),
                ..crate::model::AnalysisProvenance::default()
            },
            ..ImageAnalysis::default()
        };
        PhotosAction::SetImageAnalysis {
            photo_id: id.clone(),
            analysis: Some(analysis),
        }
        .apply(&mut doc)
        .unwrap();
        let stored = PhotosDoc::photo(&doc, &id).unwrap().analysis.unwrap();
        assert_eq!(stored.provenance.model_id, "signed-vision-model");
        assert_eq!(stored.provenance.model_revision, "a".repeat(64));

        let rejected = ImageAnalysis {
            description: "A bicycle outside".to_owned(),
            provenance: crate::model::AnalysisProvenance {
                source: "local-vision".to_owned(),
                engine: "tilo-vision".to_owned(),
                content: stored.provenance.content,
                scope: "shared".to_owned(),
                ..crate::model::AnalysisProvenance::default()
            },
            ..ImageAnalysis::default()
        };
        assert!(
            PhotosAction::SetImageAnalysis {
                photo_id: id,
                analysis: Some(rejected),
            }
            .apply(&mut doc)
            .is_err()
        );
    }

    #[test]
    fn approved_people_membership_is_atomic_searchable_and_never_carries_biometrics() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let first = portable_photo("people-one.png", 40, 20);
        let second = portable_photo("people-two.png", 40, 20);
        let first_id = first.id.clone();
        let second_id = second.id.clone();
        PhotosAction::ImportPhoto { photo: first }
            .apply(&mut doc)
            .unwrap();
        PhotosAction::ImportPhoto { photo: second }
            .apply(&mut doc)
            .unwrap();

        PhotosAction::SetPersonMembership {
            person: "Sam".to_owned(),
            photo_ids: vec![first_id.clone(), second_id.clone()],
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(PhotosDoc::photo(&doc, &first_id).unwrap().people, ["Sam"]);
        assert_eq!(PhotosDoc::photo(&doc, &second_id).unwrap().people, ["Sam"]);
        let person = PhotosResources
            .resource(&doc, &person_resource_id("Sam"))
            .expect("approved person is a resource");
        assert!(
            person
                .actions
                .iter()
                .any(|action| action.id == "set-membership")
        );
        assert!(person.actions.iter().any(|action| action.id == "rename"));
        assert!(
            person
                .actions
                .iter()
                .any(|action| action.id == "remove-person")
        );

        PhotosAction::RenamePerson {
            from: "Sam".to_owned(),
            to: "Samantha".to_owned(),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            PhotosDoc::photo(&doc, &first_id).unwrap().people,
            ["Samantha"]
        );
        PhotosAction::RemovePerson {
            person: "Samantha".to_owned(),
        }
        .apply(&mut doc)
        .unwrap();
        assert!(PhotosDoc::photo(&doc, &first_id).unwrap().people.is_empty());
        assert!(
            PhotosResources
                .resource(&doc, &person_resource_id("Samantha"))
                .is_none()
        );
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
    fn timeline_boundaries_are_canonical_actions_with_a_bounded_context_tree() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..80 {
            let mut photo = portable_photo(&format!("timeline-{index}.png"), 40, 20);
            photo.captured_at = Some(format!("2026-{:02}-{:02}", index / 28 + 1, index % 28 + 1));
            library.photos.push(photo);
        }
        // A non-calendar label remains in the library but is not made into a
        // fake chronological boundary.
        let mut undated = portable_photo("undated.png", 40, 20);
        undated.captured_at = Some("summer holidays".into());
        library.photos.push(undated);
        PhotosDoc::set_library(&mut doc, &library);

        let root = PhotosResources
            .resources(&doc)
            .pop()
            .expect("library root is published");
        let timeline = root
            .children
            .iter()
            .find(|child| child.id.0 == "photos.timeline")
            .expect("timeline is discoverable beneath the library");
        assert_eq!(timeline.kind, TIMELINE_KIND);
        assert!(
            timeline.children.len()
                <= 1 + TIMELINE_MONTH_CONTEXT_LIMIT + TIMELINE_DAY_CONTEXT_LIMIT
        );
        let open = timeline
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("timeline has a stable open action");
        let tilo_actions::ActionInput::Immediate(encoded) = &open.input else {
            panic!("timeline opening is a presentation operation");
        };
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(PhotosOperation::ViewTimeline { anchor: None })
        );

        let day_id = timeline_day_resource_id("2026-01-01");
        let day = PhotosResources
            .resource(&doc, &day_id)
            .expect("late timeline day resolves outside the bounded root context");
        let action = day
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("day has a stable open action");
        let tilo_actions::ActionInput::Immediate(encoded) = &action.input else {
            panic!("timeline day opening is a presentation operation");
        };
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(PhotosOperation::ViewTimeline {
                anchor: Some(TimelineAnchor::Day {
                    day: "2026-01-01".into(),
                }),
            })
        );
        assert!(
            PhotosResources
                .resource(&doc, "photos.timeline:day:summer holidays")
                .is_none(),
            "free-form metadata cannot masquerade as a chronological resource"
        );
    }

    #[test]
    fn timeline_boundaries_refuse_invalid_or_free_form_capture_dates() {
        assert_eq!(
            timeline_day_from_capture("2024-02-29 12:00"),
            Some("2024-02-29".into())
        );
        for capture in ["2025-02-29", "2026-13-01", "2026-04-31", "summer 2026"] {
            assert!(
                timeline_day_from_capture(capture).is_none(),
                "{capture} must not be published as a chronological boundary"
            );
        }
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
            "photos.image:/var/tmp/tilo/home/user/harbour.png"
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
        let batch = root
            .actions
            .iter()
            .find(|action| action.id == "set-favorites")
            .expect("the library publishes the batch operation agents and selection use");
        assert!(batch.contract.reversible);
        assert_eq!(batch.params().len(), 2);
        assert_eq!(batch.params()[0].ty, ParamType::Array);
        assert!(
            batch
                .build_args([
                    (
                        "photo_ids".into(),
                        tilo_actions::ArgValue::Array(Vec::new())
                    ),
                    ("favorite".into(), tilo_actions::ArgValue::Boolean(true)),
                ])
                .is_err(),
            "an empty batch is rejected by the public action contract"
        );
        assert!(
            batch
                .build_args([
                    (
                        "photo_ids".into(),
                        tilo_actions::ArgValue::Array(vec![serde_json::Value::from(7)]),
                    ),
                    ("favorite".into(), tilo_actions::ArgValue::Boolean(true)),
                ])
                .is_err(),
            "an invalid array item is refused rather than disappearing from the request"
        );
    }

    #[test]
    fn favorite_batch_is_atomic_replayable_and_refuses_a_stale_selection() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let first = portable_photo("/photos/first.png", 20, 10);
        let second = portable_photo("/photos/second.png", 20, 10);
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![first.clone(), second.clone()],
                albums: Vec::new(),
            },
        );
        let initial = PhotosDoc::library(&doc);
        let batch = PhotosAction::SetFavorites {
            photo_ids: vec![first.id.clone(), second.id],
            favorite: true,
        };
        batch
            .apply(&mut doc)
            .expect("both known ids apply together");
        assert!(
            PhotosDoc::library(&doc)
                .photos
                .iter()
                .all(|photo| photo.favorite)
        );

        let before = PhotosDoc::library(&doc);
        PhotosAction::SetFavorites {
            photo_ids: vec![first.id, "photos.photo:gone".into()],
            favorite: false,
        }
        .apply(&mut doc)
        .expect_err("a stale selection cannot partially change favorites");
        assert_eq!(PhotosDoc::library(&doc), before);

        let encoded = encode(&batch);
        let mut replica = AutoCommit::new();
        PhotosDoc::seed(&mut replica);
        PhotosDoc::set_library(&mut replica, &initial);
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        registry
            .decode(&encoded)
            .expect("the canonical registry decodes the selection result")
            .apply(&mut replica)
            .expect("replaying an absolute favorite state succeeds");
        assert!(
            PhotosDoc::library(&replica)
                .photos
                .iter()
                .all(|photo| photo.favorite)
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
