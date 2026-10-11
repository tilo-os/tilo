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
    Action, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ParamType, Resource, ResultSpec,
};

// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource;
use ui::tilo_resource::{ActionDecl, Effect};

use crate::doc::PhotosDoc;
use crate::model::{Album, Edits, ImageAnalysis, LibraryPhoto, OpenImage, PhotoLocation, Rect01};

/// Registry `type_id` for every action this app exports.
pub const PHOTOS_ACTION_TYPE: &str = "photos.action";

/// The derived, durable browse collection for photographs marked favourite.
///
/// Membership itself is shared document state; opening this resource is a
/// device-local view, just like opening an album or a metadata label.
pub const FAVORITES_KIND: &str = "photos.favorites";
/// The durable collection of collaborative albums. Individual albums remain
/// independently addressable children, while this stable root lets search,
/// accessibility and agents open album browsing itself.
pub const ALBUMS_KIND: &str = "photos.albums";
/// A derived collection of the explicitly shared photo locations.
pub const PLACES_KIND: &str = "photos.places";
/// The complete chronological browse surface. It is distinct from the
/// library resource so an agent can open a position in time rather than an
/// arbitrary insertion-ordered collection.
pub const TIMELINE_KIND: &str = "photos.timeline";
pub const TIMELINE_MONTH_KIND: &str = "photos.timeline.month";
pub const TIMELINE_DAY_KIND: &str = "photos.timeline.day";
pub const EXPORT_EDITED_CONTENT_ACTION_ID: &str = "export-edited-content";
/// Contextual enumeration is intentionally small; callers resolve any stable
/// id outside this window through `PublishesResources::resource`.
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
/// One atomic UI/action turn must remain interactive even in debug and on
/// modest hardware. Larger library-wide jobs page through this bound.
pub(crate) const MAX_BATCH_PHOTOS: usize = 256;
const MAX_ANALYSIS_TEXT_CHARS: usize = crate::analysis::MAX_OCR_CHARS;
const MAX_ANALYSIS_LABELS: usize = 64;
const MAX_ANALYSIS_LABEL_CHARS: usize = 128;
/// Keep the contextual analysis tree compact. Longer OCR remains fully
/// addressable through numbered chunks, and label entries beyond this window
/// resolve through exact resource ids.
const ANALYSIS_TEXT_CHUNK_BYTES: usize = 480;
const ANALYSIS_TEXT_CONTEXT_CHUNKS: usize = 12;
const ANALYSIS_LABEL_CONTEXT_LIMIT: usize = 16;
type LabelCounts = Vec<(String, usize)>;
/// One kind of label, how a label of it is addressed, and its resource.
type LabelledRows<'a> = (
    &'a LabelCounts,
    fn(&str) -> String,
    fn(&str, usize) -> Resource,
);

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

/// Everything Photos does that changes no document by itself: what a window
/// shows, getting photographs in and out, and local analysis whose results
/// arrive as the library's ordinary actions. Every caller — a button, the
/// palette, an agent, a script — reaches the surface's handler as one of
/// these. The ones offered on a row (a photograph, an album, a place) are
/// published bound to it.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "photos.effect")]
pub enum PhotosEffect {
    /// Open photograph: show it in this window. Which photograph a window
    /// shows is its own view state; opening one here changes nothing for a
    /// collaborator.
    #[effect(view, row)]
    View {
        #[param("Id of the photograph")]
        photo_id: String,
    },
    /// View photo library: browse and manage photographs in Photos.
    #[effect(view)]
    ViewLibrary,
    /// Open the chronological timeline, optionally at one canonical calendar
    /// boundary. Capture metadata remains untouched.
    #[effect(view, row)]
    ViewTimeline {
        #[param("A month or day of the timeline; omit for its start")]
        anchor: Option<TimelineAnchor>,
    },
    /// Open People: the People collection and this device's review.
    #[effect(view, row)]
    ViewPeople,
    /// Manage local recognition model: the signed face model, in Settings.
    #[effect(view, row)]
    ManagePeopleRecognition,
    /// Enable private People recognition on this device. Changes no shared
    /// metadata and begins no scan.
    #[effect(row, device_only)]
    EnablePeopleRecognition,
    /// Scan local library for faces: the next bounded local batch, with the
    /// separately signed face model. Suggestions stay on this device until
    /// approved.
    #[effect(row, job, cancellable, device_only)]
    AnalyzePeopleLibrary,
    /// Open photo analysis: local model availability, library coverage,
    /// progress and failures.
    #[effect(view, row)]
    ViewLibraryAnalysis,
    /// Analyze unprocessed photographs on this device. Pixels stay local;
    /// each bounded result is shared with `SetImageAnalysis`.
    #[effect(row, job, cancellable, device_only, sensitive)]
    AnalyzeLibrary,
    /// Pause photo analysis after the photograph in progress.
    #[effect(row, returns, device_only)]
    PauseLibraryAnalysis,
    /// Resume photo analysis without widening the queue.
    #[effect(row, job, cancellable, device_only, sensitive)]
    ResumeLibraryAnalysis,
    /// Retry failed photographs from the bounded failure report.
    #[effect(row, job, cancellable, device_only, sensitive)]
    RetryLibraryAnalysisFailures,
    /// View favorites: photographs marked as favorites in the shared library.
    #[effect(view, row)]
    ViewFavorites,
    /// View albums: the shared library's durable albums.
    #[effect(view, row)]
    ViewAlbums,
    /// View places: explicitly shared locations on an offline-first map.
    #[effect(view, row)]
    ViewPlaces,
    /// View place: one shared place cluster, without sending its location
    /// anywhere.
    #[effect(view, row)]
    ViewPlace { place_key: String },
    /// View album.
    #[effect(view, row)]
    ViewAlbum { album_id: String },
    /// View folder: the photographs filed under this folder label.
    #[effect(view, row)]
    ViewFolder { folder: String },
    /// View person: the photographs carrying this person label.
    #[effect(view, row)]
    ViewPerson { person: String },
    /// View tag: the photographs carrying this tag.
    #[effect(view, row)]
    ViewTag { tag: String },
    /// View object: the photographs analysis shared this object for.
    #[effect(view, row)]
    ViewObject { object: String },
    /// View pet: the photographs analysis shared this pet for.
    #[effect(view, row)]
    ViewPet { pet: String },
    /// Import photograph into the library and show it.
    #[effect(job, cancellable, device_only)]
    Import {
        #[param("Direct filesystem path; omit to use the system file picker")]
        path: Option<String>,
    },
    /// Import photo folder: recursively index a folder with bounded
    /// real-file reads. Paths never enter collaboration and EXIF GPS is never
    /// uploaded.
    #[effect(job, cancellable, device_only)]
    ImportDirectory {
        #[param("Authorised local folder path; omit to use the system folder picker")]
        path: Option<String>,
        #[param("Retain GPS read from imported files; defaults to true")]
        retain_exif_location: Option<bool>,
    },
    /// Import Camera media: a verified Camera photograph whose bytes are on
    /// this device.
    #[effect(returns, device_only)]
    ImportCameraMedia { media_id: String, name: String },
    /// Reindex photo folder: refresh metadata re-read from a folder's files
    /// without overwriting deliberate organization.
    #[effect(job, cancellable, device_only)]
    ReindexDirectory {
        #[param("Explicit authorised local folder path")]
        path: String,
        #[param("Retain GPS read from imported files; defaults to true")]
        retain_exif_location: Option<bool>,
    },
    /// Copy photograph: the photograph this window shows, as edited.
    #[effect(device_only)]
    Copy,
    /// Share photograph reference: copy its portable address to the local
    /// clipboard.
    #[effect(row, device_only)]
    ShareReference { resource_id: String },
    /// Produce edited image content: render one photograph's crop and
    /// rotation into immutable PNG content.
    #[effect(row, returns, idempotent)]
    ExportEditedContent {
        resource_id: String,
        source_content: String,
        edits: Edits,
    },
    /// Save edited photograph: the photograph this window shows.
    #[effect(job, device_only)]
    Save {
        #[param("Direct filesystem path; omit for the suggested path beside the original")]
        path: Option<String>,
    },
    /// Recognize text locally and share only bounded text and provenance.
    #[effect(row, job, cancellable, device_only, sensitive)]
    AnalyzeText {
        resource_id: String,
        language: String,
    },
    /// Analyze image locally with the signed local model; share only the
    /// bounded result.
    #[effect(row, job, cancellable, device_only, sensitive)]
    AnalyzeImage { resource_id: String },
    /// Rotate left: turn the photograph this window shows a quarter turn
    /// anticlockwise.
    RotateShownLeft,
    /// Rotate right: turn the photograph this window shows a quarter turn
    /// clockwise.
    RotateShownRight,
    /// Crop to view: keep what this window frames of its photograph.
    CropShownToView,
    /// Reset edits of the photograph this window shows.
    ResetShownEdits,
    /// Share the photograph this window shows: copy its portable address.
    #[effect(device_only)]
    ShareShown,
    /// More: the commands that do not fit a narrow toolbar.
    #[effect(view)]
    ToggleMoreMenu,
    /// New album: start naming one and choosing its photographs.
    #[effect(view)]
    NewAlbum,
    /// Edit album: the album this window browses.
    #[effect(view)]
    EditAlbum,
    /// Previous page of the library shown.
    #[effect(view)]
    PreviousLibraryPage,
    /// Next page of the library shown.
    #[effect(view)]
    NextLibraryPage,
    /// Save album: the name and photographs being edited.
    SaveAlbumDraft,
    /// Cancel album editing.
    #[effect(view)]
    CancelAlbumDraft,
    /// Delete album: ask to confirm deleting the album being edited.
    #[effect(view)]
    DeleteAlbumDraft,
    /// Confirm deleting the album being edited. Its photographs stay.
    #[effect(destructive)]
    ConfirmAlbumDelete,
    /// Save changes to the photograph details being edited.
    SaveDetails,
    /// Cancel editing photograph details.
    #[effect(view)]
    CancelDetails,
    /// Organize: the folder, people and tags of the details being edited.
    #[effect(view)]
    ShowDetailsOrganize,
    /// Time & place of the details being edited.
    #[effect(view)]
    ShowDetailsTimeAndPlace,
    /// Clear folder of the details being edited.
    #[effect(view)]
    ClearDraftFolder,
    /// Clear people of the details being edited.
    #[effect(view)]
    ClearDraftPeople,
    /// Clear tags of the details being edited.
    #[effect(view)]
    ClearDraftTags,
    /// Clear capture time of the details being edited.
    #[effect(view)]
    ClearDraftCapturedAt,
    /// Clear location of the details being edited.
    #[effect(view)]
    ClearDraftLocation,
    /// Set location from offline map: the centre of the map in the details
    /// being edited.
    #[effect(view)]
    PlaceDraftOnMap,
    /// Show fewer photographs in a timeline row.
    #[effect(view)]
    ShowFewerPhotos,
    /// Show more photographs in a timeline row.
    #[effect(view)]
    ShowMorePhotos,
    /// Previous page of albums.
    #[effect(view)]
    PreviousAlbumsPage,
    /// Next page of albums.
    #[effect(view)]
    NextAlbumsPage,
    /// Previous page of people.
    #[effect(view)]
    PreviousPeoplePage,
    /// Next page of people.
    #[effect(view)]
    NextPeoplePage,
    /// Previous page of places.
    #[effect(view)]
    PreviousPlacesPage,
    /// Next page of places.
    #[effect(view)]
    NextPlacesPage,
    /// Stop the folder import; photographs already imported are kept.
    #[effect(device_only)]
    StopImport,
    /// Details of the folder import.
    #[effect(view)]
    ShowImportDetails,
    /// Dismiss the folder import report.
    #[effect(view)]
    DismissImport,
    /// Back from a person or suggestion to People.
    #[effect(view)]
    PeopleBack,
    /// Turn private People recognition on or off on this device.
    #[effect(device_only)]
    TogglePeopleRecognition,
    /// Stop the People scan after the batch in progress.
    #[effect(device_only)]
    CancelPeopleScan,
    /// View photos of an approved person.
    #[effect(view)]
    OpenApprovedPerson { person: String },
    /// Review a private suggestion.
    #[effect(view)]
    OpenSuggestion { cluster: String },
    /// Browse approved people.
    #[effect(view)]
    BrowseApprovedPeople,
    /// Save the name typed for the person reviewed.
    SavePersonName,
    /// Merge: choose a person to merge the one shown into.
    #[effect(view)]
    BeginPeopleMerge,
    /// Merge the person shown into `person`.
    MergeApprovedInto { person: String },
    /// Approve the suggestion reviewed: share its people label.
    ApproveSuggestion,
    /// Reject the suggestion reviewed on this device.
    #[effect(device_only)]
    RejectSuggestion,
    /// Hide the suggestion reviewed on this device.
    #[effect(device_only)]
    HideSuggestion,
    /// Restore the hidden suggestion reviewed.
    #[effect(device_only)]
    RestoreSuggestion,
    /// Merge the suggestion reviewed into the approved `person`.
    MergeSuggestionWith { person: String },
    /// Show or hide hidden suggestions.
    #[effect(view)]
    ToggleHiddenSuggestions,
    /// Another page of People suggestions.
    #[effect(view)]
    PeoplePage { forward: bool },
    /// Delete local People data: ask to confirm.
    #[effect(view)]
    DeleteLocalPeopleData,
    /// Confirm deleting this device's private People data.
    #[effect(device_only, destructive)]
    ConfirmDeleteLocalPeopleData,
}

/// Render one exact addressed source/edit snapshot into immutable content.
/// Both the windowed lifecycle host and headless/test hosts call this same
/// producer, so no caller may substitute a later mutable Photos replica for
/// the output of the admitted export operation.
///
/// The rendering is an output, not the photograph: it is put in `store` and
/// returned by address, while the photograph itself stays its original content
/// plus the edits recorded against it.
///
/// # Errors
///
/// Returns a user-facing message when the addressed photograph or its edit
/// snapshot is no longer in the library, or when the source bytes cannot be
/// read, decoded, re-encoded, or stored as content.
pub fn export_edited_content(
    expected_photo_id: &str,
    source_content: &str,
    expected_edits: Edits,
    library: &crate::model::PhotoLibrary,
    store: &tilo_content::ContentStore,
) -> Result<tilo_actions::ActionOutput, String> {
    let photo = library
        .photo(expected_photo_id)
        .ok_or_else(|| "that photograph is no longer in the library".to_string())?;
    if photo.image.content.as_deref() != Some(source_content) || photo.edits != expected_edits {
        return Err("the addressed photograph or its edit snapshot is no longer current".into());
    }
    let source = store
        .read(source_content)
        .ok_or_else(|| "the addressed photograph bytes are unavailable".to_string())?;
    let original = crate::pixels::decode(&source)
        .ok_or_else(|| "the addressed photograph bytes are not a supported image".to_string())?;
    let rendered = crate::pixels::render(
        &original,
        expected_edits.crop,
        expected_edits.rotate_quarters,
    );
    let encoded = crate::pixels::encode_png(&rendered)
        .ok_or_else(|| "the edited photograph could not be encoded".to_string())?;
    let hash = store.put(&encoded).map_err(|error| {
        tracing::warn!(%error, "photos: could not store an exported rendering");
        "the edited photograph could not be stored".to_string()
    })?;
    let address = tilo_actions::content_resource(&hash)
        .ok_or_else(|| "the content provider returned an invalid address".to_string())?;
    Ok(tilo_actions::ActionOutput {
        value: None,
        resources: vec![address],
    })
}

/// `effect`, offered bound to one row with `label`: under the effect's own
/// id, so a caller naming the effect on the row reaches it.
fn operation(label: &str, effect: &PhotosEffect) -> ActionSpec {
    let mut spec = effect.bound();
    label.clone_into(&mut spec.label);
    spec
}

/// An explicit field-level decision in an atomic multi-photo metadata edit.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MetadataPatch<T> {
    /// Preserve the value already stored on each addressed photograph.
    Keep,
    /// Remove the value (or empty the complete label list).
    Clear,
    /// Replace the value with this explicit final value.
    Set(T),
}

/// A field-level list decision for people and tags.
///
/// `Add` and `Remove` are intentionally separate from `Set`: a batch editor
/// can add one label to hundreds of photographs without replacing labels a
/// collaborator added while the sheet was open.  The action still carries the
/// complete bounded target selection, so a stale selection refuses atomically.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MetadataLabelsPatch {
    Keep,
    Clear,
    Set(Vec<String>),
    Add(Vec<String>),
    Remove(Vec<String>),
}

/// Every collaborative operation on the open photograph.
///
/// Each variant carries absolute post-action state. See the module docs for
/// why durable changes are deterministic rather than incremental UI commands.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "an action is a short-lived value that is decoded, applied once and dropped; it is never stored in bulk, so boxing would only add an allocation"
)]
pub enum PhotosAction {
    /// Replace one photograph's edits with exactly this value.
    SetEdits {
        photo_id: String,
        /// Region of the original kept, normalized.
        crop: Rect01,
        /// Clockwise quarter turns, `0..=3`.
        rotate_quarters: u8,
    },
    /// Import metadata for bytes already staged in the content store.
    ImportPhoto { photo: LibraryPhoto },
    /// Refresh metadata that can be re-read from the original bytes without
    /// overwriting deliberate organization. Capture time fills an empty
    /// value only; an EXIF location may replace only another EXIF location.
    /// User/device locations, names, tags, favourites and albums are never
    /// changed by a directory rescan.
    RefreshImportedMetadata {
        photo_id: String,
        captured_at: Option<String>,
        location: Option<PhotoLocation>,
    },
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
    /// Atomically edit the chosen metadata dimensions across a bounded,
    /// explicit selection. `Keep` is distinct from `Clear`, so a mixed-value
    /// batch editor never destroys data merely because its field starts
    /// blank. Location belongs in the same action so collaborators cannot
    /// observe labels from one save and coordinates from another.
    SetMetadataBatch {
        photo_ids: Vec<String>,
        folder: MetadataPatch<String>,
        people: MetadataLabelsPatch,
        tags: MetadataLabelsPatch,
        captured_at: MetadataPatch<String>,
        location: MetadataPatch<PhotoLocation>,
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
    /// Merge one analyzer's bounded result into the photograph's existing
    /// intelligence. OCR and vision are independent components: completing
    /// either must not erase the other, irrespective of worker completion
    /// order.
    MergeImageAnalysis {
        photo_id: String,
        analysis: ImageAnalysis,
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
    /// Build the action that lands `edits` on photograph `photo_id`.
    pub fn set(photo_id: &str, edits: &Edits) -> Self {
        Self::SetEdits {
            photo_id: photo_id.to_owned(),
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
                ..
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
    fn valid_provenance(provenance: &crate::model::AnalysisProvenance) -> bool {
        matches!(provenance.scope.as_str(), "shared")
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
    let valid_ocr = analysis.ocr_text.chars().count() <= MAX_ANALYSIS_TEXT_CHARS
        && analysis
            .ocr_text
            .chars()
            .all(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'));
    (!analysis.is_empty()
        || analysis.ocr_provenance.is_some()
        || analysis.vision_provenance.is_some())
        && valid_ocr
        && compact_text(&analysis.description, 1_024)
        && labels(&analysis.objects)
        && labels(&analysis.pets)
        && labels(&analysis.people)
        && valid_provenance(&analysis.provenance)
        && analysis.ocr_provenance.as_ref().is_none_or(|provenance| {
            valid_provenance(provenance)
                && matches!(
                    provenance.source.as_str(),
                    "local-ocr" | "local-vision" | "user"
                )
        })
        && analysis
            .vision_provenance
            .as_ref()
            .is_none_or(|provenance| {
                valid_provenance(provenance)
                    && matches!(
                        provenance.source.as_str(),
                        // `local-ocr` is retained only for pre-component
                        // combined records that were valid in older Photos.
                        "local-ocr" | "local-vision" | "local-description" | "user"
                    )
            })
}

fn analysis_content_matches(photo: &LibraryPhoto, analysis: &ImageAnalysis) -> bool {
    [
        Some(&analysis.provenance),
        analysis.ocr_provenance.as_ref(),
        analysis.vision_provenance.as_ref(),
    ]
    .into_iter()
    .flatten()
    .all(|provenance| {
        provenance.content.is_empty()
            || photo.image.content.as_deref() == Some(provenance.content.as_str())
    })
}

fn analysis_names_exact_content(photo: &LibraryPhoto, analysis: &ImageAnalysis) -> bool {
    let Some(content) = photo.image.content.as_deref() else {
        return false;
    };
    [
        Some(&analysis.provenance),
        analysis.ocr_provenance.as_ref(),
        analysis.vision_provenance.as_ref(),
    ]
    .into_iter()
    .flatten()
    .all(|provenance| provenance.content == content)
}

fn merge_image_analysis(
    existing: Option<&ImageAnalysis>,
    incoming: ImageAnalysis,
    content: Option<&str>,
) -> ImageAnalysis {
    let mut merged = existing.cloned().unwrap_or_default();
    let component_is_current = |provenance: &crate::model::AnalysisProvenance| {
        provenance.content.is_empty() || Some(provenance.content.as_str()) == content
    };
    let ocr_provenance = merged.ocr_provenance.as_ref().unwrap_or(&merged.provenance);
    if !merged.ocr_text.is_empty() && !component_is_current(ocr_provenance) {
        merged.ocr_text.clear();
        merged.ocr_provenance = None;
    }
    let vision_provenance = merged
        .vision_provenance
        .as_ref()
        .unwrap_or(&merged.provenance);
    if (!merged.description.is_empty() || !merged.objects.is_empty() || !merged.pets.is_empty())
        && !component_is_current(vision_provenance)
    {
        merged.description.clear();
        merged.objects.clear();
        merged.pets.clear();
        merged.vision_provenance = None;
    }
    // Hydrate component provenance for pre-component records before changing
    // anything. This prevents a new OCR result from making an older visual
    // description look as though OCR produced it (and vice versa).
    if merged.ocr_provenance.is_none() && !merged.ocr_text.is_empty() {
        merged.ocr_provenance = Some(merged.provenance.clone());
    }
    if merged.vision_provenance.is_none()
        && (!merged.description.is_empty() || !merged.objects.is_empty() || !merged.pets.is_empty())
        && matches!(
            merged.provenance.source.as_str(),
            "local-vision" | "local-description" | "user"
        )
    {
        merged.vision_provenance = Some(merged.provenance.clone());
    }
    match incoming.provenance.source.as_str() {
        "local-ocr" => {
            merged.ocr_text = incoming.ocr_text;
            merged.ocr_provenance = incoming
                .ocr_provenance
                .or_else(|| Some(incoming.provenance.clone()));
        }
        "local-vision" | "local-description" => {
            merged.description = incoming.description;
            merged.objects = incoming.objects;
            merged.pets = incoming.pets;
            // A dedicated OCR result wins over incidental vision OCR. Vision
            // still refreshes OCR produced by an older vision pass, including
            // clearing text the current model no longer observes.
            let dedicated_ocr = merged.ocr_provenance.as_ref().is_some_and(|provenance| {
                matches!(provenance.source.as_str(), "local-ocr" | "user")
            });
            if !dedicated_ocr && incoming.provenance.source == "local-vision" {
                merged.ocr_text = incoming.ocr_text;
                merged.ocr_provenance = incoming
                    .ocr_provenance
                    .or_else(|| (!merged.ocr_text.is_empty()).then(|| incoming.provenance.clone()));
            }
            merged.vision_provenance = incoming
                .vision_provenance
                .or_else(|| Some(incoming.provenance.clone()));
        }
        _ => return incoming,
    }
    // Keep the legacy primary field meaningful for older peers while the
    // component fields retain the complete, non-lossy audit trail.
    merged.provenance = merged
        .vision_provenance
        .clone()
        .or_else(|| merged.ocr_provenance.clone())
        .unwrap_or(incoming.provenance);
    merged
}

impl Action for PhotosAction {
    #[allow(
        clippy::too_many_lines,
        reason = "all durable Photos mutations are kept in one exhaustive match so their atomic validation is auditable"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::SetEdits { photo_id, .. } => {
                let edits = self.edits()?;
                validate_resource_id(photo_id)?;
                let mut library = PhotosDoc::library(doc);
                let photo = library.photo_mut(photo_id).ok_or_else(|| {
                    ActionError::Refused("that photograph is no longer in the library".into())
                })?;
                photo.edits = edits;
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::ImportPhoto { photo } => {
                if !valid_photo(photo) {
                    return Err(ActionError::Refused(
                        "photo metadata must be canonical, portable, and bounded".into(),
                    ));
                }
                // A duplicate content hash preserves its existing library
                // metadata.
                let mut library = PhotosDoc::library(doc);
                library.add_photo(photo.clone());
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::RefreshImportedMetadata {
                photo_id,
                captured_at,
                location,
            } => {
                let captured_at = match captured_at.as_deref() {
                    Some(value) => Some(canonical_capture_at_value(value).ok_or_else(|| {
                        ActionError::Refused(
                            "reindexed capture time is not valid EXIF or ISO metadata".into(),
                        )
                    })?),
                    None => None,
                };
                if validate_resource_id(photo_id).is_err()
                    || !valid_metadata(None, &[], &[], captured_at.as_deref())
                    || location
                        .as_ref()
                        .is_some_and(|value| !value.is_valid() || value.provenance.source != "exif")
                {
                    return Err(ActionError::Refused(
                        "reindexed metadata must be valid local EXIF metadata".into(),
                    ));
                }
                let mut library = PhotosDoc::library(doc);
                let photo = library.photo_mut(photo_id).ok_or_else(|| {
                    ActionError::Refused("that photograph is no longer in the library".into())
                })?;
                if photo.captured_at.is_none() {
                    photo.captured_at = captured_at;
                }
                if photo
                    .location
                    .as_ref()
                    .is_none_or(|current| current.provenance.source == "exif")
                {
                    photo.location.clone_from(location);
                }
                PhotosDoc::set_library(doc, &library);
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
                    || !unique_strings(photo_ids)
                {
                    return Err(ActionError::Refused(
                        "favorite batch must contain unique, valid photograph ids".into(),
                    ));
                }
                let mut library = PhotosDoc::library(doc);
                let selected: std::collections::HashSet<_> =
                    photo_ids.iter().map(String::as_str).collect();
                if library
                    .photos
                    .iter()
                    .filter(|photo| selected.contains(photo.id.as_str()))
                    .count()
                    != selected.len()
                {
                    return Err(ActionError::Refused(
                        "a selected photograph is no longer in the library".into(),
                    ));
                }
                for photo in &mut library.photos {
                    if selected.contains(photo.id.as_str()) {
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
                let captured_at = match captured_at {
                    Some(value) => Some(canonical_capture_at_value(value).ok_or_else(|| {
                        ActionError::Refused(
                            "capture date/time must be a valid ISO 8601 value".into(),
                        )
                    })?),
                    None => None,
                };
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
                photo.captured_at = captured_at;
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::SetMetadataBatch {
                photo_ids,
                folder,
                people,
                tags,
                captured_at,
                location,
            } => {
                if photo_ids.is_empty()
                    || photo_ids.len() > MAX_BATCH_PHOTOS
                    || photo_ids.iter().any(|id| validate_resource_id(id).is_err())
                    || !unique_strings(photo_ids)
                {
                    return Err(ActionError::Refused(
                        "metadata batch must contain unique, valid photograph ids".into(),
                    ));
                }
                let mut library = PhotosDoc::library(doc);
                let selected: std::collections::HashSet<_> =
                    photo_ids.iter().map(String::as_str).collect();
                if library
                    .photos
                    .iter()
                    .filter(|photo| selected.contains(photo.id.as_str()))
                    .count()
                    != selected.len()
                {
                    return Err(ActionError::Refused(
                        "a selected photograph is no longer in the library".into(),
                    ));
                }
                let captured_at = canonical_capture_patch(captured_at)?;

                // Validate the complete post-action record for every photo
                // before changing any of them. This is the atomicity boundary
                // for both collaboration replay and local multi-edit UX.
                for photo in library
                    .photos
                    .iter()
                    .filter(|photo| selected.contains(photo.id.as_str()))
                {
                    let next_folder = match folder {
                        MetadataPatch::Keep => photo.folder.clone(),
                        MetadataPatch::Clear => None,
                        MetadataPatch::Set(value) => Some(value.clone()),
                    };
                    let next_people = apply_label_patch(&photo.people, people);
                    let next_tags = apply_label_patch(&photo.tags, tags);
                    let next_captured_at = match &captured_at {
                        MetadataPatch::Keep => photo.captured_at.clone(),
                        MetadataPatch::Clear => None,
                        MetadataPatch::Set(value) => Some(value.clone()),
                    };
                    let next_location = match location {
                        MetadataPatch::Keep => photo.location.clone(),
                        MetadataPatch::Clear => None,
                        MetadataPatch::Set(value) => Some(value.clone()),
                    };
                    if !valid_metadata(
                        next_folder.as_deref(),
                        &next_people,
                        &next_tags,
                        next_captured_at.as_deref(),
                    ) || next_location
                        .as_ref()
                        .is_some_and(|value| !value.is_valid())
                    {
                        return Err(ActionError::Refused(
                            "batch metadata must be short, valid, and free of duplicate labels"
                                .into(),
                        ));
                    }
                }

                for photo in library
                    .photos
                    .iter_mut()
                    .filter(|photo| selected.contains(photo.id.as_str()))
                {
                    match folder {
                        MetadataPatch::Keep => {}
                        MetadataPatch::Clear => photo.folder = None,
                        MetadataPatch::Set(value) => photo.folder = Some(value.clone()),
                    }
                    photo.people = apply_label_patch(&photo.people, people);
                    photo.tags = apply_label_patch(&photo.tags, tags);
                    match &captured_at {
                        MetadataPatch::Keep => {}
                        MetadataPatch::Clear => photo.captured_at = None,
                        MetadataPatch::Set(value) => photo.captured_at = Some(value.clone()),
                    }
                    match location {
                        MetadataPatch::Keep => {}
                        MetadataPatch::Clear => photo.location = None,
                        MetadataPatch::Set(value) => photo.location = Some(value.clone()),
                    }
                }
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::SetPersonMembership { person, photo_ids } => {
                if !valid_person_label(person)
                    || photo_ids.len() > MAX_BATCH_PHOTOS
                    || photo_ids.iter().any(|id| validate_resource_id(id).is_err())
                    || !unique_strings(photo_ids)
                {
                    return Err(ActionError::Refused(
                        "person membership must name a short label and unique valid photograph ids"
                            .into(),
                    ));
                }
                let person = person.trim();
                let mut library = PhotosDoc::library(doc);
                let selected: std::collections::HashSet<_> =
                    photo_ids.iter().map(String::as_str).collect();
                if library
                    .photos
                    .iter()
                    .filter(|photo| selected.contains(photo.id.as_str()))
                    .count()
                    != selected.len()
                {
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
                    let should_have_person = selected.contains(photo.id.as_str());
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
                    let should_have_person = selected.contains(photo.id.as_str());
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
                if analysis.as_ref().is_some_and(|value| {
                    !analysis_content_matches(photo, value)
                        || (value.provenance.source.starts_with("local-")
                            && !analysis_names_exact_content(photo, value))
                }) {
                    return Err(ActionError::Refused(
                        "analysis provenance does not name this photograph's content".into(),
                    ));
                }
                photo.analysis.clone_from(analysis);
                PhotosDoc::set_library(doc, &library);
                Ok(())
            }
            Self::MergeImageAnalysis { photo_id, analysis } => {
                if validate_resource_id(photo_id).is_err()
                    || !valid_analysis(analysis)
                    || !matches!(
                        analysis.provenance.source.as_str(),
                        "local-ocr" | "local-vision" | "local-description"
                    )
                {
                    return Err(ActionError::Refused(
                        "image analysis must be compact, explicitly shared, and carry analyzer provenance".into(),
                    ));
                }
                let mut library = PhotosDoc::library(doc);
                let photo = library.photo_mut(photo_id).ok_or_else(|| {
                    ActionError::Refused("that photograph is no longer in the library".into())
                })?;
                if !analysis_names_exact_content(photo, analysis) {
                    return Err(ActionError::Refused(
                        "analysis components do not name this photograph's content".into(),
                    ));
                }
                let merged = merge_image_analysis(
                    photo.analysis.as_ref(),
                    analysis.clone(),
                    photo.image.content.as_deref(),
                );
                if !valid_analysis(&merged) {
                    return Err(ActionError::Refused(
                        "merged image analysis is invalid or exceeds a shared-data bound".into(),
                    ));
                }
                photo.analysis = Some(merged);
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

/// The library as the palette, agents and scripts see it: the resource's
/// one publication, read from a document. What a window adds (its live
/// rows, its bound edits) is the framework's to add.
pub struct PhotosResources;

impl PhotosResources {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        library_resources(doc)
    }

    /// The resource `id` of the library in `doc`, wherever it is published.
    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        let state = <crate::resource::Photos as ui::tilo_resource::Resource>::read(doc).ok()?;
        if id == "photos.library" {
            return library_resources(doc).into_iter().next();
        }
        library_child(&state.library, id)
    }
}

/// The library's resource `id`, resolved from the library itself: the
/// bounded page `library_children` publishes, and every photograph, album,
/// analysis, timeline boundary, place and label beyond it.
pub(crate) fn library_child(library: &crate::model::PhotoLibrary, id: &str) -> Option<Resource> {
    if id.chars().take(MAX_RESOURCE_ID_CHARS + 1).count() > MAX_RESOURCE_ID_CHARS {
        return None;
    }
    if let Some(found) = find_resource(&library_children(library), id) {
        return Some(found);
    }
    if let Some((photo_id, _)) = id.split_once("#analysis")
        && let Some(photo) = library.photo(photo_id)
        && let Some(analysis) = photo.analysis.as_ref()
    {
        return analysis_resource_for_id(&photo.id, analysis, id);
    }
    if let Some(photo) = library.photo(id) {
        return Some(photo_resource(photo));
    }
    if let Some(album) = library.album(id) {
        return Some(album_resource(album));
    }
    if let Some(resource) = timeline_resource_for_id(library, id) {
        return Some(resource);
    }
    if let Some((key, label, count)) = place_counts(library)
        .into_iter()
        .find(|(key, _, _)| place_resource_id(key) == id)
    {
        return Some(place_resource(&key, &label, count));
    }
    let (folders, people, tags, objects, pets) = metadata_label_counts(library);
    let labelled: [LabelledRows<'_>; 5] = [
        (&folders, folder_resource_id, folder_resource),
        (&people, person_resource_id, person_resource),
        (&tags, tag_resource_id, tag_resource),
        (&objects, object_resource_id, object_resource),
        (&pets, pet_resource_id, pet_resource),
    ];
    labelled
        .into_iter()
        .find_map(|(counts, address, resource)| {
            counts
                .iter()
                .find(|(label, _)| address(label) == id)
                .map(|(label, count)| resource(label, *count))
        })
}

/// The edits of photograph `photo` offered on its resource: turns, a crop of
/// a region, reset, and producing the edited pixels as content.
fn photo_edit_actions(photo: &PhotoResourceSeed) -> Vec<ActionSpec> {
    let edits = photo.edits;
    let bound = edits_values(&photo.id, &edits);
    let mut actions: Vec<ActionSpec> = ["rotate-right", "rotate-left", "crop"]
        .into_iter()
        .filter_map(|id| row_action(id, &bound))
        .collect();
    if !edits.is_identity() {
        actions.extend(row_action("reset", &photo_value(&photo.id)));
    }
    if let Some(source_content) = photo.content.clone() {
        let spec = operation(
            "Produce edited image content",
            &PhotosEffect::ExportEditedContent {
                resource_id: photo.id.clone(),
                source_content,
                edits,
            },
        );
        actions.push(
            spec.returns(ResultSpec::Resource {
                resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.into(),
            })
            .with_description(crate::strings::tr(
                "photos-act-render-this-photograph-s-current-crop-and",
            )),
        );
    }
    actions
}

/// The durable library and album resource tree. Every photo and album gets a
/// stable resource id even when it is not selected in the editor, allowing
/// agents and search to resolve a late library item without a visual grid
/// having first materialised it.
pub(crate) fn find_resource(resources: &[Resource], id: &str) -> Option<Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_resource(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

fn library_resources(doc: &AutoCommit) -> Vec<Resource> {
    let state =
        <crate::resource::Photos as ui::tilo_resource::Resource>::read(doc).unwrap_or_default();
    let root = ui::tilo_resource::publish::<crate::resource::Photos>(
        "photos.library",
        &crate::strings::tr("photos-act-photo-library"),
        &state,
    );
    vec![root]
}

/// Nested library catalogue rows for [`crate::resource::Photos::children`].
pub(crate) fn library_children(library: &crate::model::PhotoLibrary) -> Vec<Resource> {
    let mut children = vec![
        library_analysis_resource(),
        favorites_resource(library.photos.iter().filter(|photo| photo.favorite).count()),
        albums_resource(library),
        places_resource(library),
        people_root_resource(library),
        timeline_resource(library),
    ];
    for photo in library.photos.iter().take(LIBRARY_CONTEXT_LIMIT) {
        children.push(photo_resource(photo));
    }
    let (folders, people, tags, objects, pets) = metadata_label_counts(library);
    for (folder, count) in folders.into_iter().take(FOLDER_CONTEXT_LIMIT) {
        children.push(folder_resource(&folder, count));
    }
    for (person, count) in people.into_iter().take(PERSON_CONTEXT_LIMIT) {
        children.push(person_resource(&person, count));
    }
    for (tag, count) in tags.into_iter().take(TAG_CONTEXT_LIMIT) {
        children.push(tag_resource(&tag, count));
    }
    for (object, count) in objects.into_iter().take(TAG_CONTEXT_LIMIT) {
        children.push(object_resource(&object, count));
    }
    for (pet, count) in pets.into_iter().take(TAG_CONTEXT_LIMIT) {
        children.push(pet_resource(&pet, count));
    }
    children
}

/// Album and metadata edits of the whole library, and each row's edits,
/// declared by name.
pub(crate) fn library_declarations() -> Vec<ActionDecl<crate::resource::Photos>> {
    let mut declarations = vec![
        create_album_spec(),
        set_favorites_spec(),
        set_metadata_batch_spec(),
    ];
    declarations.extend(row_declarations());
    declarations
}

/// The library's edits of one photograph, album or person, declared by name
/// with the item they act on as a parameter. Each row of the published tree
/// offers the ones that apply to it with that parameter bound
/// ([`items::bound`](ui::tilo_resource::items::bound)), so the palette and
/// agents act on *this* photograph; scripts name the item.
type Decl = ActionDecl<crate::resource::Photos>;

fn photo_param() -> ActionParam {
    ActionParam::required("photo_id", ParamType::Text, "stable photos.photo:… id")
}

fn album_param() -> ActionParam {
    ActionParam::required("album_id", ParamType::Text, "stable photos.album:… id")
}

fn person_param() -> ActionParam {
    ActionParam::required("person", ParamType::Text, "approved person label")
}

fn text(args: &ActionArgs, name: &str) -> String {
    args.text(name).unwrap_or_default().trim().to_owned()
}

/// The edits a turn or crop starts from: the photograph's crop and turn as
/// its row binds them.
fn edits_in(args: &ActionArgs) -> Result<Edits, String> {
    let crop = match args.text("crop") {
        Some(value) => parse_crop_arg(value).ok_or("the crop is not x,y,width,height in 0..1")?,
        None => Rect01::default(),
    };
    let rotate_quarters = args
        .integer("rotate_quarters")
        .and_then(|turns| u8::try_from(turns).ok())
        .unwrap_or_default();
    Ok(Edits {
        crop,
        rotate_quarters,
    })
}

fn edits_params() -> Vec<ActionParam> {
    vec![
        photo_param(),
        ActionParam::optional(
            "crop",
            ParamType::Text,
            "the crop in force, normalized x,y,width,height",
        ),
        ActionParam::optional(
            "rotate_quarters",
            ParamType::Integer,
            "the quarter turns in force",
        ),
    ]
}

/// A photograph's crop and turn, as its row binds them.
pub(crate) fn edits_values(photo: &str, edits: &Edits) -> Vec<(&'static str, serde_json::Value)> {
    let crop = edits.crop;
    vec![
        ("photo_id", serde_json::Value::String(photo.to_owned())),
        (
            "crop",
            serde_json::Value::String(format!("{},{},{},{}", crop.x, crop.y, crop.w, crop.h)),
        ),
        (
            "rotate_quarters",
            serde_json::Value::from(edits.rotate_quarters),
        ),
    ]
}

fn photo_value(photo: &str) -> Vec<(&'static str, serde_json::Value)> {
    vec![("photo_id", serde_json::Value::String(photo.to_owned()))]
}

#[expect(
    clippy::too_many_lines,
    reason = "every row edit of the library is declared side by side"
)]
pub(crate) fn row_declarations() -> Vec<Decl> {
    vec![
        Decl::new("rotate-right", "Rotate right", edits_params(), |args, _| {
            Ok(PhotosAction::set(&text(args, "photo_id"), &edits_in(args)?.rotated_right()))
        })
        .describe("Turn the photograph a quarter turn clockwise"),
        Decl::new("rotate-left", "Rotate left", edits_params(), |args, _| {
            Ok(PhotosAction::set(&text(args, "photo_id"), &edits_in(args)?.rotated_left()))
        })
        .describe("Turn the photograph a quarter turn anticlockwise"),
        Decl::new(
            "crop",
            "Crop to a region",
            {
                let mut params = edits_params();
                params.push(
                    ActionParam::required(
                        "region",
                        ParamType::Text,
                        "normalized x,y,width,height values between 0 and 1",
                    )
                    .with_schema(serde_json::json!({
                        "type": "string",
                        "pattern": "^[[:space:]]*(0(\\.[0-9]+)?|1(\\.0+)?)[[:space:]]*,[[:space:]]*(0(\\.[0-9]+)?|1(\\.0+)?)[[:space:]]*,[[:space:]]*(0(\\.[0-9]+)?|1(\\.0+)?)[[:space:]]*,[[:space:]]*(0(\\.[0-9]+)?|1(\\.0+)?)[[:space:]]*$"
                    })),
                );
                params
            },
            |args, _| {
                let region = args
                    .text("region")
                    .and_then(parse_crop_arg)
                    .ok_or("the region is not x,y,width,height in 0..1")?;
                Ok(PhotosAction::set(
                    &text(args, "photo_id"),
                    &edits_in(args)?.cropped_from_view(region),
                ))
            },
        )
        .describe("Keep a region of what is currently shown, in normalized coordinates"),
        Decl::new("reset", "Reset edits", vec![photo_param()], |args, _| {
            Ok(PhotosAction::set(&text(args, "photo_id"), &Edits::default()))
        })
        .describe("Discard the crop and rotation, returning to the original"),
        Decl::new("favorite", "Favorite", vec![photo_param()], |args, _| {
            Ok(PhotosAction::SetFavorite {
                photo_id: text(args, "photo_id"),
                favorite: true,
            })
        }),
        Decl::new("remove-favorite", "Remove favorite", vec![photo_param()], |args, _| {
            Ok(PhotosAction::SetFavorite {
                photo_id: text(args, "photo_id"),
                favorite: false,
            })
        }),
        Decl::new(
            "set-metadata",
            "Set photo metadata",
            vec![
                photo_param(),
                ActionParam::optional("folder", ParamType::Text, "shared folder label"),
                ActionParam::optional("people", ParamType::Text, "comma-separated people"),
                ActionParam::optional("tags", ParamType::Text, "comma-separated tags"),
                ActionParam::optional("captured_at", ParamType::Text, "capture date or time"),
            ],
            |args, _| {
                Ok(PhotosAction::SetMetadata {
                    photo_id: text(args, "photo_id"),
                    folder: optional_metadata_text(args, "folder"),
                    people: metadata_labels(args, "people"),
                    tags: metadata_labels(args, "tags"),
                    captured_at: optional_metadata_text(args, "captured_at"),
                })
            },
        )
        .describe("Replace this photograph's shared folder, people, tags, and capture date"),
        Decl::new(
            "set-location",
            "Set shared location",
            vec![
                photo_param(),
                ActionParam::required("latitude", ParamType::Number, "latitude from -90 to 90"),
                ActionParam::required("longitude", ParamType::Number, "longitude from -180 to 180"),
                ActionParam::optional("label", ParamType::Text, "optional shared place label"),
                ActionParam::optional(
                    "source",
                    ParamType::Text,
                    "user, exif, or device; defaults to user",
                ),
            ],
            |args, _| {
                Ok(PhotosAction::SetPhotoLocation {
                    photo_id: text(args, "photo_id"),
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
        .describe(
            "Explicitly share a compact location. Photos never uploads it for geocoding or map tiles.",
        ),
        Decl::new(
            "clear-location",
            "Remove shared location",
            vec![photo_param()],
            |args, _| {
                Ok(PhotosAction::ClearPhotoLocation {
                    photo_id: text(args, "photo_id"),
                })
            },
        )
        .destructive()
        .describe(
            "Remove the explicit shared location from this photograph; no files or pixels are changed",
        ),
        Decl::new(
            "remove-analysis",
            "Remove shared analysis",
            vec![photo_param()],
            |args, _| {
                Ok(PhotosAction::SetImageAnalysis {
                    photo_id: text(args, "photo_id"),
                    analysis: None,
                })
            },
        )
        .destructive()
        .describe(
            "Remove recognized text, description, object and pet labels, and their provenance without changing the photograph",
        ),
        Decl::new("remove-photo", "Remove from library", vec![photo_param()], |args, _| {
            Ok(PhotosAction::RemovePhoto {
                photo_id: text(args, "photo_id"),
            })
        })
        .destructive(),
        Decl::new("remove-album", "Remove album", vec![album_param()], |args, _| {
            Ok(PhotosAction::RemoveAlbum {
                id: text(args, "album_id"),
            })
        })
        .destructive(),
        Decl::new(
            "rename-album",
            "Rename album",
            vec![
                album_param(),
                ActionParam::required("name", ParamType::Text, "new album name"),
            ],
            |args, _| {
                Ok(PhotosAction::RenameAlbum {
                    id: text(args, "album_id"),
                    name: text(args, "name"),
                })
            },
        )
        .describe("Change this album's visible name while keeping its stable id"),
        Decl::new(
            "set-album-membership",
            "Set album membership",
            vec![
                album_param(),
                photo_ids_param(
                    true,
                    "JSON array of stable photo ids; an empty array clears the album",
                    0,
                ),
            ],
            |args, _| {
                Ok(PhotosAction::SetAlbumMembership {
                    album_id: text(args, "album_id"),
                    photo_ids: membership_ids(args),
                })
            },
        )
        .describe("Replace this album's photograph membership atomically"),
        Decl::new(
            "save-album",
            "Save album",
            vec![
                album_param(),
                ActionParam::required("name", ParamType::Text, "visible album name"),
                photo_ids_param(false, "JSON array of stable photo ids", 0),
            ],
            |args, _| {
                Ok(PhotosAction::SaveAlbum {
                    id: text(args, "album_id"),
                    name: text(args, "name"),
                    photo_ids: membership_ids(args),
                    create: false,
                })
            },
        )
        .describe("Atomically rename this album and replace its membership"),
        Decl::new(
            "set-person-membership",
            "Set person membership",
            vec![
                person_param(),
                photo_ids_param(
                    true,
                    "JSON array of stable photo ids; an empty array removes this person label",
                    0,
                ),
            ],
            |args, _| {
                Ok(PhotosAction::SetPersonMembership {
                    person: text(args, "person"),
                    photo_ids: membership_ids(args),
                })
            },
        )
        .describe("Create a shared person or replace one person's complete approved membership"),
        Decl::new(
            "rename-person",
            "Rename or merge person",
            vec![
                person_param(),
                ActionParam::required("name", ParamType::Text, "new approved person label"),
            ],
            |args, _| {
                Ok(PhotosAction::RenamePerson {
                    from: text(args, "person"),
                    to: text(args, "name"),
                })
            },
        )
        .describe("Rename this approved person label, or merge it into an existing label"),
        Decl::new("remove-person", "Remove person label", vec![person_param()], |args, _| {
            Ok(PhotosAction::RemovePerson {
                person: text(args, "person"),
            })
        })
        .destructive()
        .describe("Remove this approved person label from every shared photograph"),
    ]
}

/// The row edit `id`, offered with `values` bound.
fn row_action(id: &str, values: &[(&'static str, serde_json::Value)]) -> Option<ActionSpec> {
    row_declarations()
        .into_iter()
        .find(|decl| decl.id == id)
        .map(|decl| ui::tilo_resource::items::bound(&decl, values))
}

/// Canonical address for whole-library local intelligence. Runtime progress is
/// intentionally absent: it is private device state, while these operations
/// remain discoverable to accessibility, automation, search, and agents.
pub(crate) fn library_analysis_resource() -> Resource {
    let mut resource = Resource::new(
        "photos.library-analysis",
        "photos.library-analysis",
        crate::strings::tr("photos-act-photo-analysis"),
    );
    for (label, description, op) in [
        (
            "Open photo analysis",
            "Review local model availability, library coverage, progress, and failures",
            PhotosEffect::ViewLibraryAnalysis,
        ),
        (
            "Analyze unprocessed photographs",
            "Use the verified local model on locally available photographs and share only bounded searchable results",
            PhotosEffect::AnalyzeLibrary,
        ),
        (
            "Pause photo analysis",
            "Stop scheduling photographs after the current local item finishes",
            PhotosEffect::PauseLibraryAnalysis,
        ),
        (
            "Resume photo analysis",
            "Continue the explicitly started queue on this device",
            PhotosEffect::ResumeLibraryAnalysis,
        ),
        (
            "Retry failed photographs",
            "Retry only photographs listed in the bounded local failure report",
            PhotosEffect::RetryLibraryAnalysisFailures,
        ),
    ] {
        {
            let spec = operation(label, &op);
            resource = resource.with_action(spec.with_description(description));
        }
    }
    resource
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
        #[expect(
            clippy::string_slice,
            reason = "timeline_day_from_capture only returns validated ASCII `YYYY-MM-DD`"
        )]
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
    let month = day.get(5..7)?.parse::<u8>().ok()?;
    let day_of_month = day.get(8..10)?.parse::<u8>().ok()?;
    let year = day.get(..4)?.parse::<u16>().ok()?;
    let days_in_month = ui::date::days_in_month(i64::from(year), u32::from(month));
    (bytes.len() == 10
        && bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
        && (1..=12).contains(&month)
        && (1..=days_in_month).contains(&u32::from(day_of_month)))
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
    {
        let spec = operation(
            &crate::strings::tr("photos-open-timeline"),
            &PhotosEffect::ViewTimeline { anchor: None },
        );
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
    {
        let spec = operation(
            &crate::strings::tr("photos-open-timeline-month"),
            &PhotosEffect::ViewTimeline {
                anchor: Some(TimelineAnchor::Month {
                    month: month.to_owned(),
                }),
            },
        );
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
    {
        let spec = operation(
            &crate::strings::tr("photos-open-timeline-day"),
            &PhotosEffect::ViewTimeline {
                anchor: Some(TimelineAnchor::Day {
                    day: day.to_owned(),
                }),
            },
        );
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
    {
        let spec = operation("View favorites", &PhotosEffect::ViewFavorites);
        resource = resource.with_action(
            spec.with_description("Show photographs marked as favorites in the shared library"),
        );
    }
    resource
}

/// The canonical album collection. Its contextual children are deliberately
/// bounded; every album still resolves by stable id through `resource`, and
/// the search catalogue retains compact seeds for the complete collection.
pub(crate) fn albums_resource(library: &crate::model::PhotoLibrary) -> Resource {
    let mut resource = albums_resource_from_count(library.albums.len());
    for album in library.albums.iter().take(ALBUM_CONTEXT_LIMIT) {
        resource = resource.with_child(album_resource(album));
    }
    resource
}

pub(crate) fn albums_resource_from_count(count: usize) -> Resource {
    let count_label = if count == 1 {
        "1 album".to_owned()
    } else {
        format!("{count} albums")
    };
    let mut resource = Resource::new("photos.albums", ALBUMS_KIND, "Albums").with_child(
        Resource::new("photos.albums#count", "photos.count", count_label),
    );
    {
        let spec = operation("View albums", &PhotosEffect::ViewAlbums);
        resource = resource.with_action(
            spec.with_description("Browse and manage the shared library's durable albums"),
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
    {
        let spec = operation("Open People", &PhotosEffect::ViewPeople);
        resource = resource.with_action(
            spec.with_description("Review local suggestions and browse approved people"),
        );
    }
    {
        let spec = operation(
            "Manage local recognition model",
            &PhotosEffect::ManagePeopleRecognition,
        );
        resource = resource.with_action(spec.with_description(
            "Install or manage a signed local face-recognition model in Settings",
        ));
    }
    {
        let spec = operation(
            "Enable private People recognition",
            &PhotosEffect::EnablePeopleRecognition,
        );
        resource = resource.with_action(spec.with_description(
            "Allow signed local face recognition on this device; it does not scan or share anything yet",
        ));
    }
    {
        let spec = operation(
            "Scan local library for faces",
            &PhotosEffect::AnalyzePeopleLibrary,
        );
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
        .filter(|photo| photo.location.as_ref().is_some_and(PhotoLocation::is_valid))
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
    {
        let spec = operation("View places", &PhotosEffect::ViewPlaces);
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
        if !location.is_valid() {
            continue;
        }
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
        tilo_content::hash_bytes(place_key.as_bytes())
    )
}

pub(crate) fn place_resource(place_key: &str, label: &str, count: usize) -> Resource {
    let id = place_resource_id(place_key);
    let mut resource = Resource::new(&id, "photos.place", label).with_child(Resource::new(
        format!("{id}#count"),
        "photos.count",
        format!("{count} photographs"),
    ));
    {
        let spec = operation(
            "View place",
            &PhotosEffect::ViewPlace {
                place_key: place_key.to_owned(),
            },
        );
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
        tilo_content::hash_bytes(folder.as_bytes())
    )
}

pub(crate) fn person_resource_id(person: &str) -> String {
    format!(
        "photos.person:{}",
        tilo_content::hash_bytes(person.as_bytes())
    )
}

/// A tag is durable shared metadata, so its derived browse resource must have
/// the same portable, content-independent identity on every replica.
pub(crate) fn tag_resource_id(tag: &str) -> String {
    format!("photos.tag:{}", tilo_content::hash_bytes(tag.as_bytes()))
}

pub(crate) fn object_resource_id(object: &str) -> String {
    format!(
        "photos.object:{}",
        tilo_content::hash_bytes(object.as_bytes())
    )
}

pub(crate) fn pet_resource_id(pet: &str) -> String {
    format!("photos.pet:{}", tilo_content::hash_bytes(pet.as_bytes()))
}

pub(crate) fn folder_resource(folder: &str, count: usize) -> Resource {
    let mut item = Resource::new(folder_resource_id(folder), "photos.folder", folder).with_child(
        Resource::new(
            format!("{}#count", folder_resource_id(folder)),
            "photos.count",
            format!("{count} photographs"),
        ),
    );
    {
        let spec = operation(
            "Open folder",
            &PhotosEffect::ViewFolder {
                folder: folder.to_owned(),
            },
        );
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
    {
        let spec = operation(
            "View person",
            &PhotosEffect::ViewPerson {
                person: person.to_owned(),
            },
        );
        item = item.with_action(spec.with_description("Show photographs tagged with this person"));
    }
    let bound = [("person", serde_json::Value::String(person.to_owned()))];
    for id in ["set-person-membership", "rename-person", "remove-person"] {
        if let Some(spec) = row_action(id, &bound) {
            item = item.with_action(spec);
        }
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
    {
        let spec = operation(
            "View tag",
            &PhotosEffect::ViewTag {
                tag: tag.to_owned(),
            },
        );
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
    {
        let spec = operation(
            "View object",
            &PhotosEffect::ViewObject {
                object: object.to_owned(),
            },
        );
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
    {
        let spec = operation(
            "View pet",
            &PhotosEffect::ViewPet {
                pet: pet.to_owned(),
            },
        );
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
    pub content: Option<String>,
    pub edits: Edits,
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
            content: photo.image.content.clone(),
            edits: photo.edits,
        }
    }
}

impl PhotoResourceSeed {
    fn metadata_summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(folder) = &self.folder {
            parts.push(format!("{}: {folder}", crate::strings::tr("photos-folder")));
        }
        if !self.people.is_empty() {
            parts.push(format!(
                "{}: {}",
                crate::strings::tr("photos-people"),
                self.people.join(", ")
            ));
        }
        if !self.tags.is_empty() {
            parts.push(format!(
                "{}: {}",
                crate::strings::tr("photos-tags"),
                self.tags.join(", ")
            ));
        }
        if let Some(captured_at) = &self.captured_at {
            parts.push(format!(
                "Captured: {}",
                crate::model::capture_label(captured_at)
            ));
        }
        if let Some(location) = &self.location {
            parts.push(format!(
                "{}: {}",
                crate::strings::tr("photos-place"),
                location.display_name()
            ));
        }
        if let Some(analysis) = &self.analysis {
            if !analysis.description.is_empty() {
                parts.push(format!(
                    "{}: {}",
                    crate::strings::tr("photos-description"),
                    analysis.description
                ));
            }
            if !analysis.objects.is_empty() {
                parts.push(format!(
                    "{}: {}",
                    crate::strings::tr("photos-objects"),
                    analysis.objects.join(", ")
                ));
            }
            if !analysis.pets.is_empty() {
                parts.push(format!(
                    "{}: {}",
                    crate::strings::tr("photos-pets"),
                    analysis.pets.join(", ")
                ));
            }
        }
        parts.join(". ")
    }
}

#[expect(
    clippy::string_slice,
    reason = "every chunk bound is a `char_indices` offset or the string length, so each slice lies on UTF-8 boundaries"
)]
fn bounded_text_chunks(value: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut bytes = 0_usize;
    for (index, character) in value.char_indices() {
        if bytes > 0 && bytes.saturating_add(character.len_utf8()) > ANALYSIS_TEXT_CHUNK_BYTES {
            chunks.push(&value[start..index]);
            start = index;
            bytes = 0;
        }
        bytes += character.len_utf8();
    }
    if start < value.len() {
        chunks.push(&value[start..]);
    }
    chunks
}

fn analysis_text_resource(id: &str, kind: &str, label: &str, text: &str) -> Resource {
    let chunks = bounded_text_chunks(text);
    let mut resource = Resource::new(
        id,
        kind,
        format!(
            "{label}, {} character{} in {} part{}",
            text.chars().count(),
            if text.chars().count() == 1 { "" } else { "s" },
            chunks.len(),
            if chunks.len() == 1 { "" } else { "s" }
        ),
    );
    for (index, chunk) in chunks.iter().take(ANALYSIS_TEXT_CONTEXT_CHUNKS).enumerate() {
        resource = resource.with_child(Resource::new(
            format!("{id}/{index}"),
            format!("{kind}.part"),
            *chunk,
        ));
    }
    resource
}

fn analysis_labels_resource(id: &str, kind: &str, label: &str, values: &[String]) -> Resource {
    let mut resource = Resource::new(
        id,
        kind,
        format!(
            "{} {label}{}",
            values.len(),
            if values.len() == 1 { "" } else { "s" }
        ),
    );
    for (index, value) in values.iter().take(ANALYSIS_LABEL_CONTEXT_LIMIT).enumerate() {
        resource = resource.with_child(Resource::new(
            format!("{id}/{index}"),
            format!("{kind}.label"),
            value,
        ));
    }
    resource
}

fn analysis_scalar_resource(id: &str, kind: &str, field: &str, value: &str) -> Resource {
    let value = if value.is_empty() {
        &crate::strings::tr("photos-act-not-recorded")
    } else {
        value
    };
    let label = format!("{field}: {value}");
    if label.len() <= ANALYSIS_TEXT_CHUNK_BYTES {
        Resource::new(id, kind, label)
    } else {
        analysis_text_resource(id, kind, field, value)
    }
}

fn analysis_provenance_resource(
    id: &str,
    component: &str,
    provenance: &crate::model::AnalysisProvenance,
) -> Resource {
    let mut resource = Resource::new(
        id,
        "photos.analysis.provenance",
        format!("{component} provenance"),
    );
    for (field, value) in [
        ("source", provenance.source.as_str()),
        ("engine", provenance.engine.as_str()),
        ("model", provenance.model_id.as_str()),
        ("revision", provenance.model_revision.as_str()),
        ("language", provenance.language.as_str()),
        ("content", provenance.content.as_str()),
        ("scope", provenance.scope.as_str()),
    ] {
        resource = resource.with_child(analysis_scalar_resource(
            &format!("{id}/{field}"),
            &format!("photos.analysis.provenance.{field}"),
            &match field {
                "model" => crate::strings::tr("photos-act-model"),
                "revision" => crate::strings::tr("photos-act-model-revision"),
                _ => field.to_owned(),
            },
            value,
        ));
    }
    resource
}

fn analysis_component_provenance(
    analysis: &ImageAnalysis,
    vision: bool,
) -> Option<&crate::model::AnalysisProvenance> {
    if vision {
        analysis.vision_provenance.as_ref().or_else(|| {
            (!analysis.description.is_empty()
                || !analysis.objects.is_empty()
                || !analysis.pets.is_empty())
            .then_some(&analysis.provenance)
        })
    } else {
        analysis
            .ocr_provenance
            .as_ref()
            .or_else(|| (!analysis.ocr_text.is_empty()).then_some(&analysis.provenance))
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "each analysis component and its actions are declared side by side with their translated labels"
)]
fn analysis_resource(photo_id: &str, analysis: &ImageAnalysis) -> Resource {
    let id = format!("{photo_id}#analysis");
    let mut root = Resource::new(
        &id,
        "photos.analysis",
        crate::strings::tr("photos-act-shared-image-intelligence"),
    );

    let ocr_id = format!("{id}/ocr");
    let mut ocr = Resource::new(
        &ocr_id,
        "photos.analysis.ocr",
        crate::strings::tr("photos-act-recognized-text"),
    );
    ocr = ocr.with_child(analysis_text_resource(
        &format!("{ocr_id}/text"),
        "photos.analysis.ocr.text",
        &crate::strings::tr("photos-act-recognized-text"),
        &analysis.ocr_text,
    ));
    if let Some(provenance) = analysis_component_provenance(analysis, false) {
        ocr = ocr.with_child(analysis_provenance_resource(
            &format!("{ocr_id}/provenance"),
            "OCR",
            provenance,
        ));
    }
    {
        let spec = operation(
            "Recognize text again locally",
            &PhotosEffect::AnalyzeText {
                resource_id: photo_id.to_owned(),
                language: analysis_component_provenance(analysis, false)
                    .map(|provenance| provenance.language.as_str())
                    .filter(|language| !language.is_empty())
                    .unwrap_or("eng")
                    .to_owned(),
            },
        );
        ocr = ocr.with_action(spec.with_description(
            "Replace only the shared OCR component after a bounded local recognition job",
        ));
    }
    root = root.with_child(ocr);

    let vision_id = format!("{id}/vision");
    let mut vision = Resource::new(
        &vision_id,
        "photos.analysis.vision",
        crate::strings::tr("photos-act-visual-analysis"),
    )
    .with_child(analysis_text_resource(
        &format!("{vision_id}/description"),
        "photos.analysis.description",
        &crate::strings::tr("photos-act-description"),
        &analysis.description,
    ))
    .with_child(analysis_labels_resource(
        &format!("{vision_id}/objects"),
        "photos.analysis.objects",
        "object",
        &analysis.objects,
    ))
    .with_child(analysis_labels_resource(
        &format!("{vision_id}/pets"),
        "photos.analysis.pets",
        "pet",
        &analysis.pets,
    ))
    .with_child(analysis_labels_resource(
        &format!("{vision_id}/people"),
        "photos.analysis.people",
        "person",
        &analysis.people,
    ));
    if let Some(provenance) = analysis_component_provenance(analysis, true) {
        vision = vision.with_child(analysis_provenance_resource(
            &format!("{vision_id}/provenance"),
            &crate::strings::tr("photos-act-visual-analysis"),
            provenance,
        ));
    }
    {
        let spec = operation(
            "Analyze image again locally",
            &PhotosEffect::AnalyzeImage {
                resource_id: photo_id.to_owned(),
            },
        );
        let unavailable = local_vision_unavailable_reason();
        let spec = spec.with_description(match unavailable.as_deref() {
            Some(reason) => crate::strings::tr_with("photos-unavailable", "reason", reason),
            None => {
                crate::strings::tr("photos-act-replace-only-the-shared-description-and-recognized")
            }
        });
        vision = vision.with_action(match unavailable {
            Some(reason) => spec.unavailable("local_vision_unavailable", reason),
            None => spec,
        });
    }
    root = root.with_child(vision);

    // Component-specific deletion is intentionally not synthesized with a
    // read-modify-write `SetImageAnalysis`: replaying that snapshot could
    // erase a concurrent refresh of the other component. Whole-record removal
    // is the only deletion whose current action semantics are unambiguous.
    if let Some(spec) = row_action("remove-analysis", &photo_value(photo_id)) {
        root = root.with_action(spec);
    }
    root
}

fn find_analysis_descendant(resource: &Resource, id: &str) -> Option<Resource> {
    if resource.id.0 == id {
        return Some(resource.clone());
    }
    resource
        .children
        .iter()
        .find_map(|child| find_analysis_descendant(child, id))
}

fn analysis_resource_for_id(
    photo_id: &str,
    analysis: &ImageAnalysis,
    requested_id: &str,
) -> Option<Resource> {
    let root = analysis_resource(photo_id, analysis);
    if let Some(resource) = find_analysis_descendant(&root, requested_id) {
        return Some(resource);
    }

    let base = format!("{photo_id}#analysis");
    for (path, kind, values) in [
        (
            "vision/objects",
            "photos.analysis.objects",
            &analysis.objects,
        ),
        ("vision/pets", "photos.analysis.pets", &analysis.pets),
        ("vision/people", "photos.analysis.people", &analysis.people),
    ] {
        let prefix = format!("{base}/{path}/");
        if let Some(index) = requested_id
            .strip_prefix(&prefix)
            .and_then(|index| index.parse::<usize>().ok())
            && let Some(value) = values.get(index)
        {
            return Some(Resource::new(requested_id, format!("{kind}.label"), value));
        }
    }
    for (path, kind, text) in [
        (
            "ocr/text",
            "photos.analysis.ocr.text",
            analysis.ocr_text.as_str(),
        ),
        (
            "vision/description",
            "photos.analysis.description",
            analysis.description.as_str(),
        ),
    ] {
        let prefix = format!("{base}/{path}/");
        if let Some(index) = requested_id
            .strip_prefix(&prefix)
            .and_then(|index| index.parse::<usize>().ok())
            && let Some(chunk) = bounded_text_chunks(text).get(index)
        {
            return Some(Resource::new(requested_id, format!("{kind}.part"), *chunk));
        }
    }
    None
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
        .then(|| crate::strings::tr("photos-act-no-verified-local-image-analysis-model-is"))
}

#[allow(
    clippy::too_many_lines,
    reason = "the complete context action surface stays adjacent to its resource declaration"
)]
pub(crate) fn photo_resource_from_seed(photo: &PhotoResourceSeed) -> Resource {
    let (width, height) =
        OpenImage::from_path(&photo.name, photo.width, photo.height).edited_size(&photo.edits);
    let mut item = Resource::new(&photo.id, PHOTO_KIND, &photo.name).with_child(Resource::new(
        format!("{}#size", photo.id),
        "photos.size",
        format!("{width} x {height} pixels"),
    ));
    if !photo.metadata_summary().is_empty() {
        item = item.with_child(Resource::new(
            format!("{}#metadata", photo.id),
            "photos.metadata",
            photo.metadata_summary(),
        ));
    }
    if let Some(analysis) = &photo.analysis {
        item = item.with_child(analysis_resource(&photo.id, analysis));
    }
    if let Some(location) = &photo.location {
        item = item.with_child(Resource::new(
            format!("{}#location", photo.id),
            "photos.location",
            format!(
                "{}, explicitly shared from {}",
                location.display_name(),
                location.provenance.source
            ),
        ));
        {
            let spec = operation(
                "View on map",
                &PhotosEffect::ViewPlace {
                    place_key: location.place_key(),
                },
            );
            item = item.with_action(spec.with_description(
                "Open the local Places view without sending this coordinate anywhere",
            ));
        }
    }
    {
        let spec = operation(
            "Open photograph",
            &PhotosEffect::View {
                photo_id: photo.id.clone(),
            },
        );
        item = item.with_action(spec.with_description("Show this photograph in this window"));
    }
    for action in photo_edit_actions(photo) {
        item = item.with_action(action);
    }
    let photo_bound = photo_value(&photo.id);
    if let Some(spec) = row_action(
        if photo.favorite {
            "remove-favorite"
        } else {
            "favorite"
        },
        &photo_bound,
    ) {
        item = item.with_action(spec);
    }
    {
        let spec = operation(
            "Share photograph reference",
            &PhotosEffect::ShareReference {
                resource_id: photo.id.clone(),
            },
        );
        item = item.with_action(spec.with_description(
            "Copy this library photograph's portable resource reference to the local clipboard",
        ));
    }
    for id in ["set-metadata", "set-location"] {
        if let Some(spec) = row_action(id, &photo_bound) {
            item = item.with_action(spec);
        }
    }
    if photo.location.is_some()
        && let Some(spec) = row_action("clear-location", &photo_bound)
    {
        item = item.with_action(spec);
    }
    {
        let spec = operation(
            "Recognize text locally",
            &PhotosEffect::AnalyzeText {
                resource_id: photo.id.clone(),
                language: "eng".to_owned(),
            },
        );
        item = item.with_action(spec.with_description(
            "Run local OCR and explicitly share only bounded text and provenance",
        ));
    }
    {
        let spec = operation(
            "Analyze image locally",
            &PhotosEffect::AnalyzeImage {
                resource_id: photo.id.clone(),
            },
        );
        let unavailable = local_vision_unavailable_reason();
        let description = if let Some(reason) = unavailable.as_deref() {
            crate::strings::tr_with("photos-unavailable", "reason", reason)
        } else {
            crate::strings::tr("photos-act-use-the-signed-local-model-to-describe")
        };
        let spec = spec.with_description(description);
        item = item.with_action(match unavailable {
            Some(reason) => spec.unavailable("local_vision_unavailable", reason),
            None => spec,
        });
    }
    if photo.analysis.is_some()
        && let Some(spec) = row_action("remove-analysis", &photo_bound)
    {
        item = item.with_action(spec);
    }
    if let Some(spec) = row_action("remove-photo", &photo_bound) {
        item = item.with_action(spec);
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
    folder.is_none_or(text)
        && captured_at.is_none_or(canonical_capture_at)
        && labels(people)
        && labels(tags)
}

fn unique_strings(values: &[String]) -> bool {
    let mut seen = std::collections::HashSet::with_capacity(values.len());
    values.iter().all(|value| seen.insert(value.as_str()))
}

/// Return the one wire spelling Photos accepts for a capture value.
///
/// Date-only values deliberately remain date-only; local date-times remain
/// local (there is no invented timezone); and offset date-times retain both
/// their authored UTC offset and their fractional-second precision.  This is
/// a validation boundary, not a display formatter, so a collaborator cannot
/// turn an unknown local time into a different instant by merely opening it.
pub fn canonical_capture_at(value: &str) -> bool {
    canonical_capture_at_value(value).as_deref() == Some(value)
}

pub(crate) fn canonical_capture_at_value(value: &str) -> Option<String> {
    if value.chars().count() > 128 || value.chars().any(char::is_control) || !value.is_ascii() {
        return None;
    }
    // EXIF's `YYYY:MM:DD HH:MM:SS` and a legacy space separator are accepted
    // once at the action boundary and committed as ISO. All durable values
    // therefore converge to one form.
    let mut canonical = value.to_owned();
    if canonical.as_bytes().get(4) == Some(&b':') && canonical.as_bytes().get(7) == Some(&b':') {
        canonical.replace_range(4..5, "-");
        canonical.replace_range(7..8, "-");
    }
    if canonical.as_bytes().get(10) == Some(&b' ') {
        canonical.replace_range(10..11, "T");
    }
    if canonical.len() == 10 {
        return valid_capture_date(&canonical).then_some(canonical);
    }
    let (body, offset) = if let Some(body) = canonical.strip_suffix('Z') {
        (body, Some("Z"))
    } else if let Some((body, suffix)) = canonical
        .len()
        .checked_sub(6)
        .and_then(|split| canonical.split_at_checked(split))
        && matches!(suffix.as_bytes().first(), Some(b'+' | b'-'))
        && suffix.as_bytes().get(3) == Some(&b':')
    {
        (body, Some(suffix))
    } else {
        (canonical.as_str(), None)
    };
    if body.len() < 16
        || !body.get(..10).is_some_and(valid_capture_date)
        || body.as_bytes().get(10) != Some(&b'T')
    {
        return None;
    }
    let (whole_seconds, _fraction) = match body.split_once('.') {
        Some((whole, fraction))
            if fraction.len() <= 9
                && !fraction.is_empty()
                && fraction.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            (whole, Some(fraction.len()))
        }
        Some(_) => return None,
        None => (body, None),
    };
    let minute_precision = whole_seconds.len() == 16;
    let second_precision = whole_seconds.len() == 19;
    if !minute_precision && !second_precision {
        return None;
    }
    // An offset identifies an instant, so require seconds in its wire format.
    if offset.is_some() && !second_precision {
        return None;
    }
    if !valid_capture_time(whole_seconds, second_precision) {
        return None;
    }
    if let Some(offset) = offset
        && offset != "Z"
        && !valid_capture_offset(offset)
    {
        return None;
    }
    let mut result = body.to_owned();
    if offset == Some("+00:00") || offset == Some("-00:00") || offset == Some("Z") {
        result.push('Z');
    } else if let Some(offset) = offset {
        result.push_str(offset);
    }
    Some(result)
}

fn decimal(value: &[u8]) -> Option<u32> {
    (!value.is_empty() && value.iter().all(u8::is_ascii_digit))
        .then(|| std::str::from_utf8(value).ok()?.parse().ok())
        .flatten()
}

fn valid_capture_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return false;
    }
    let (Some(year), Some(month), Some(day)) = (
        bytes.get(..4).and_then(decimal),
        bytes.get(5..7).and_then(decimal),
        bytes.get(8..).and_then(decimal),
    ) else {
        return false;
    };
    (1..=12).contains(&month)
        && (1..=ui::date::days_in_month(i64::from(year), month)).contains(&day)
}

fn valid_capture_time(value: &str, has_seconds: bool) -> bool {
    let bytes = value.as_bytes();
    let needed = if has_seconds { 19 } else { 16 };
    if bytes.len() != needed
        || bytes.get(13) != Some(&b':')
        || (has_seconds && bytes.get(16) != Some(&b':'))
    {
        return false;
    }
    let hour = bytes.get(11..13).and_then(decimal);
    let minute = bytes.get(14..16).and_then(decimal);
    let second = has_seconds
        .then(|| bytes.get(17..19).and_then(decimal))
        .flatten();
    hour.is_some_and(|value| value <= 23)
        && minute.is_some_and(|value| value <= 59)
        && (!has_seconds || second.is_some_and(|value| value <= 59))
}

fn valid_capture_offset(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 6
        || !matches!(bytes.first(), Some(b'+' | b'-'))
        || bytes.get(3) != Some(&b':')
    {
        return false;
    }
    bytes
        .get(1..3)
        .and_then(decimal)
        .is_some_and(|hour| hour <= 23)
        && bytes
            .get(4..)
            .and_then(decimal)
            .is_some_and(|minute| minute <= 59)
}

fn canonical_capture_patch(
    patch: &MetadataPatch<String>,
) -> Result<MetadataPatch<String>, ActionError> {
    match patch {
        MetadataPatch::Keep => Ok(MetadataPatch::Keep),
        MetadataPatch::Clear => Ok(MetadataPatch::Clear),
        MetadataPatch::Set(value) => canonical_capture_at_value(value)
            .filter(|value| canonical_capture_at(value))
            .map(MetadataPatch::Set)
            .ok_or_else(|| {
                ActionError::Refused(
                    "capture date/time must be a valid canonical ISO 8601 value".into(),
                )
            }),
    }
}

fn apply_label_patch(current: &[String], patch: &MetadataLabelsPatch) -> Vec<String> {
    match patch {
        MetadataLabelsPatch::Keep => current.to_vec(),
        MetadataLabelsPatch::Clear => Vec::new(),
        MetadataLabelsPatch::Set(values) => values.clone(),
        MetadataLabelsPatch::Add(values) => {
            let mut next = current.to_vec();
            for value in values {
                if !next.contains(value) {
                    next.push(value.clone());
                }
            }
            next
        }
        MetadataLabelsPatch::Remove(values) => current
            .iter()
            .filter(|value| !values.contains(*value))
            .cloned()
            .collect(),
    }
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

pub(crate) fn album_resource(album: &Album) -> Resource {
    album_resource_from_fields(&album.id, &album.name, album.photo_ids.len())
}

pub(crate) fn album_resource_from_fields(id: &str, name: &str, photo_count: usize) -> Resource {
    let mut item = Resource::new(id, "photos.album", name).with_child(Resource::new(
        format!("{id}#count"),
        "photos.count",
        format!("{photo_count} photographs"),
    ));
    {
        let spec = operation(
            "Open album",
            &PhotosEffect::ViewAlbum {
                album_id: id.to_owned(),
            },
        );
        item = item.with_action(
            spec.with_description("Show this album's photographs without changing membership"),
        );
    }
    let bound = [("album_id", serde_json::Value::String(id.to_owned()))];
    for action in [
        "remove-album",
        "rename-album",
        "set-album-membership",
        "save-album",
    ] {
        if let Some(spec) = row_action(action, &bound) {
            item = item.with_action(spec);
        }
    }
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

fn create_album_spec() -> ActionDecl<crate::resource::Photos> {
    ActionDecl::new(
        "create-album",
        "Create album",
        vec![
            ActionParam::required("name", ParamType::Text, "visible album name"),
            ActionParam::optional(
                "id",
                ParamType::Text,
                "stable album id, for example photos.album:holiday; a new one when absent",
            ),
            photo_ids_param(false, "JSON array of stable photo ids", 0),
        ],
        |args, _| {
            // A new album's id is minted once, here, when the caller names
            // none, so the action replays with the same id everywhere.
            let id = args.text("id").map_or_else(
                || format!("photos.album:{}", uuid::Uuid::new_v4().simple()),
                |id| id.trim().to_owned(),
            );
            Ok(PhotosAction::SaveAlbum {
                id,
                name: args.text("name").unwrap_or_default().trim().to_owned(),
                photo_ids: membership_ids(args),
                create: true,
            })
        },
    )
    .describe("Create an album and its initial photograph membership atomically")
}

/// The agent/search counterpart of the visible library selection command.
/// Selection never leaks into the shared document; callers name exactly the
/// resources whose durable favorite markers should change.
fn set_favorites_spec() -> ActionDecl<crate::resource::Photos> {
    ActionDecl::new(
        "set-favorites",
        "Set favorites",
        vec![
            photo_ids_param(
                true,
                "non-empty JSON array of stable photograph resource ids",
                1,
            ),
            ActionParam::required("favorite", ParamType::Boolean, "whether to mark favorite"),
        ],
        |args, _| {
            Ok(PhotosAction::SetFavorites {
                photo_ids: membership_ids(args),
                favorite: args.boolean("favorite").unwrap_or(false),
            })
        },
    )
    .describe("Set the favorite marker for a bounded batch of selected photographs")
}

/// The same atomic multi-edit exposed by the visible selection inspector.
/// Every dimension has an explicit apply bit, so omitted/mixed values mean
/// keep rather than accidentally clearing collaborators' metadata.
fn set_metadata_batch_spec() -> ActionDecl<crate::resource::Photos> {
    ActionDecl::new(
        "set-metadata-batch",
        "Edit selected photo details",
        vec![
            photo_ids_param(
                true,
                "non-empty JSON array of stable photograph resource ids",
                1,
            ),
            ActionParam::required("apply_folder", ParamType::Boolean, "replace folder values"),
            ActionParam::optional("folder", ParamType::Text, "new folder; omit to clear"),
            ActionParam::required("apply_people", ParamType::Boolean, "apply people operation"),
            ActionParam::optional("people", ParamType::Text, "comma-separated people labels"),
            ActionParam::optional("people_mode", ParamType::Text, "set, add, remove, or clear")
                .with_choices(["set", "add", "remove", "clear"]),
            ActionParam::required("apply_tags", ParamType::Boolean, "apply tag operation"),
            ActionParam::optional("tags", ParamType::Text, "comma-separated tag labels"),
            ActionParam::optional("tags_mode", ParamType::Text, "set, add, remove, or clear")
                .with_choices(["set", "add", "remove", "clear"]),
            ActionParam::required(
                "apply_captured_at",
                ParamType::Boolean,
                "replace capture date/time",
            ),
            ActionParam::optional(
                "captured_at",
                ParamType::Text,
                "new capture date/time; omit to clear",
            ),
            ActionParam::required(
                "apply_location",
                ParamType::Boolean,
                "replace or clear shared location",
            ),
            ActionParam::optional("latitude", ParamType::Number, "latitude from -90 to 90"),
            ActionParam::optional("longitude", ParamType::Number, "longitude from -180 to 180"),
            ActionParam::optional("place", ParamType::Text, "optional shared place label"),
        ],
        |args, _| {
            let text_patch = |apply: &str, value: &str| {
                if args.boolean(apply).unwrap_or(false) {
                    optional_metadata_text(args, value)
                        .map_or(MetadataPatch::Clear, MetadataPatch::Set)
                } else {
                    MetadataPatch::Keep
                }
            };
            let labels_patch = |apply: &str, value: &str, mode: &str| {
                if args.boolean(apply).unwrap_or(false) {
                    let labels = metadata_labels(args, value);
                    match args.text(mode).unwrap_or("set") {
                        "clear" => MetadataLabelsPatch::Clear,
                        "add" => MetadataLabelsPatch::Add(labels),
                        "remove" => MetadataLabelsPatch::Remove(labels),
                        _ if labels.is_empty() => MetadataLabelsPatch::Clear,
                        _ => MetadataLabelsPatch::Set(labels),
                    }
                } else {
                    MetadataLabelsPatch::Keep
                }
            };
            let location = if !args.boolean("apply_location").unwrap_or(false) {
                MetadataPatch::Keep
            } else if let (Some(latitude), Some(longitude)) =
                (args.number("latitude"), args.number("longitude"))
            {
                MetadataPatch::Set(PhotoLocation {
                    latitude,
                    longitude,
                    label: optional_metadata_text(args, "place"),
                    provenance: crate::model::LocationProvenance {
                        source: "user".to_owned(),
                        scope: "shared".to_owned(),
                    },
                })
            } else {
                MetadataPatch::Clear
            };
            Ok(PhotosAction::SetMetadataBatch {
                photo_ids: membership_ids(args),
                folder: text_patch("apply_folder", "folder"),
                people: labels_patch("apply_people", "people", "people_mode"),
                tags: labels_patch("apply_tags", "tags", "tags_mode"),
                captured_at: text_patch("apply_captured_at", "captured_at"),
                location,
            })
        },
    )
    .describe(
        "Atomically change chosen metadata: labels can be set, added, removed, or cleared without replacing unrelated values",
    )
}

/// The resource `kind` a library photograph publishes under.
pub const PHOTO_KIND: &str = "photos.photo";

/// Length of a blake3 hash written as hex, which is what a content-named id
/// carries after its kind.
const CONTENT_HEX_LEN: usize = 64;

/// The content a photograph's id names. A photograph is addressed by what it
/// is, not where a machine put it, so a session that never had a file for it
/// goes from the address to the bytes through its content store.
pub fn content_of(id: &str) -> Option<&str> {
    let tail = id.strip_prefix(PHOTO_KIND)?.strip_prefix(':')?;
    let is_content = tail.len() == CONTENT_HEX_LEN
        && tail
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    is_content.then_some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::{ExecutionMode, OperationKind, RetryPolicy};

    /// The effect an operation payload names, when it is one of Photos'.
    fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<PhotosEffect>, String> {
        if type_id != PhotosEffect::OPERATION {
            return Ok(None);
        }
        PhotosEffect::from_payload(payload).map(Some)
    }
    use tilo_automerge::ApplyIncoming;

    fn portable_photo(path: &str, width: u32, height: u32) -> LibraryPhoto {
        let hash = tilo_content::hash_bytes(path.as_bytes());
        LibraryPhoto::from_image(OpenImage::from_path(path, width, height).with_content(hash))
    }
    use crate::model::OpenImage;
    use tilo_actions::flatten;

    /// The first photograph the library publishes.
    fn published_photo(resources: &[Resource]) -> &Resource {
        resources
            .iter()
            .find(|resource| resource.kind == "photos.library")
            .and_then(|library| {
                library
                    .children
                    .iter()
                    .find(|child| child.kind == PHOTO_KIND)
            })
            .expect("the photograph is published")
    }

    /// Put `image` in the library, returning its id.
    fn add_photo(doc: &mut AutoCommit, image: OpenImage) -> String {
        let photo = LibraryPhoto::from_image(image);
        let id = photo.id.clone();
        let mut library = PhotosDoc::library(doc);
        library.add_photo(photo);
        PhotosDoc::set_library(doc, &library);
        id
    }

    /// The id of the library's first photograph.
    fn first_id(doc: &AutoCommit) -> String {
        PhotosDoc::library(doc).photos[0].id.clone()
    }

    /// The edits of the library's first photograph.
    fn first_edits(doc: &AutoCommit) -> Edits {
        PhotosDoc::library(doc).photos[0].edits
    }

    /// Edit the library's first photograph.
    fn set_first_edits(doc: &mut AutoCommit, edits: Edits) {
        PhotosAction::set(&first_id(doc), &edits)
            .apply(doc)
            .expect("the edit applies");
    }

    fn doc_with_image() -> AutoCommit {
        let mut doc = AutoCommit::new();
        add_photo(
            &mut doc,
            OpenImage::from_path("/var/tmp/tilo/home/user/harbour.png", 800, 400),
        );
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
            ..ImageAnalysis::default()
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn label_edits_preserve_user_work_converge_and_never_carry_biometrics() {
        eprintln!("phase reindex_refreshes_only_machine_metadata_and_preserves_user_work");
        {
            let mut doc = AutoCommit::new();
            let mut photo = portable_photo("reindex.png", 40, 20);
            let id = photo.id.clone();
            photo.favorite = true;
            photo.folder = Some("Family".into());
            photo.people = vec!["Ada".into()];
            photo.tags = vec!["birthday".into()];
            photo.location = Some(PhotoLocation {
                latitude: 1.0,
                longitude: 2.0,
                label: Some("Chosen place".into()),
                provenance: crate::model::LocationProvenance {
                    source: "user".into(),
                    scope: "shared".into(),
                },
            });
            PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();

            PhotosAction::RefreshImportedMetadata {
                photo_id: id.clone(),
                captured_at: Some("2026:09:18 10:20:30".into()),
                location: Some(PhotoLocation {
                    latitude: 50.0,
                    longitude: 4.0,
                    label: None,
                    provenance: crate::model::LocationProvenance {
                        source: "exif".into(),
                        scope: "shared".into(),
                    },
                }),
            }
            .apply(&mut doc)
            .unwrap();

            let refreshed = PhotosDoc::photo(&doc, &id).unwrap();
            assert_eq!(
                refreshed.captured_at.as_deref(),
                Some("2026-09-18T10:20:30")
            );
            assert_eq!(
                refreshed.location.unwrap().label.as_deref(),
                Some("Chosen place")
            );
            assert!(refreshed.favorite);
            assert_eq!(refreshed.folder.as_deref(), Some("Family"));
            assert_eq!(refreshed.people, ["Ada"]);
            assert_eq!(refreshed.tags, ["birthday"]);
        }

        eprintln!("phase metadata_label_deltas_preserve_unrelated_labels_and_canonicalize_time");
        {
            let mut doc = AutoCommit::new();
            let mut photo = portable_photo("/photos/delta.png", 20, 10);
            photo.people = vec!["Sam".into(), "Noor".into()];
            photo.tags = vec!["travel".into()];
            PhotosDoc::set_library(
                &mut doc,
                &crate::model::PhotoLibrary {
                    photos: vec![photo.clone()],
                    albums: Vec::new(),
                },
            );
            PhotosAction::SetMetadataBatch {
                photo_ids: vec![photo.id.clone()],
                folder: MetadataPatch::Keep,
                people: MetadataLabelsPatch::Remove(vec!["Sam".into()]),
                tags: MetadataLabelsPatch::Add(vec!["sunset".into()]),
                captured_at: MetadataPatch::Set("2026-09-18 14:30".into()),
                location: MetadataPatch::Keep,
            }
            .apply(&mut doc)
            .expect("field-level label changes are valid");
            let changed = PhotosDoc::photo(&doc, &photo.id).expect("photo remains");
            assert_eq!(changed.people, ["Noor"]);
            assert_eq!(changed.tags, ["sunset", "travel"]);
            assert_eq!(changed.captured_at.as_deref(), Some("2026-09-18T14:30"));
        }

        eprintln!("phase concurrent_label_adds_converge_without_replacing_each_other");
        {
            let mut base = AutoCommit::new();
            let photo = portable_photo("/photos/concurrent-labels.png", 20, 10);
            PhotosDoc::set_library(
                &mut base,
                &crate::model::PhotoLibrary {
                    photos: vec![photo.clone()],
                    albums: Vec::new(),
                },
            );
            let bytes = base.save();
            let mut left = AutoCommit::load(&bytes).expect("copy left replica");
            left.set_actor(automerge::ActorId::from([1; 16].as_slice()));
            let mut right = AutoCommit::load(&bytes).expect("copy right replica");
            right.set_actor(automerge::ActorId::from([2; 16].as_slice()));
            for (doc, tag) in [(&mut left, "left"), (&mut right, "right")] {
                PhotosAction::SetMetadataBatch {
                    photo_ids: vec![photo.id.clone()],
                    folder: MetadataPatch::Keep,
                    people: MetadataLabelsPatch::Keep,
                    tags: MetadataLabelsPatch::Add(vec![tag.into()]),
                    captured_at: MetadataPatch::Keep,
                    location: MetadataPatch::Keep,
                }
                .apply(doc)
                .expect("offline add applies");
            }
            left.merge_incoming(&mut right).expect("replicas merge");
            assert_eq!(
                PhotosDoc::photo(&left, &photo.id).expect("photo").tags,
                ["left", "right"],
                "each label is a CRDT membership rather than a replaced vector"
            );
        }

        eprintln!(
            "phase approved_people_membership_is_atomic_searchable_and_never_carries_biometrics"
        );
        {
            let mut doc = AutoCommit::new();
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
            let library = PhotosResources
                .publish_resource(&doc, "photos.library")
                .expect("the library is a canonical resource");
            assert!(
                library
                    .actions
                    .iter()
                    .any(|action| action.id == "set-person-membership"
                        && action.params().iter().any(|param| param.name == "person")),
                "a new person is necessary to split an approved mistaken group"
            );
            assert_eq!(PhotosDoc::photo(&doc, &first_id).unwrap().people, ["Sam"]);
            assert_eq!(PhotosDoc::photo(&doc, &second_id).unwrap().people, ["Sam"]);
            let person = PhotosResources
                .publish_resource(&doc, &person_resource_id("Sam"))
                .expect("approved person is a resource");
            assert!(
                person
                    .actions
                    .iter()
                    .any(|action| action.id == "set-person-membership")
            );
            assert!(
                person
                    .actions
                    .iter()
                    .any(|action| action.id == "rename-person")
            );
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
                    .publish_resource(&doc, &person_resource_id("Samantha"))
                    .is_none()
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn locations_are_typed_shared_addressable_and_range_checked() {
        eprintln!("phase explicit_location_is_typed_searchable_addressable_and_can_be_cleared");
        {
            let mut doc = AutoCommit::new();
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
            let place = PhotosResources.publish_resource(&doc, &place_id).unwrap();
            assert_eq!(place.kind, "photos.place");
            assert!(place.actions.iter().any(|action| action.id == "view-place"));
            let photo_resource = PhotosResources.publish_resource(&doc, &id).unwrap();
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
            assert!(PhotosResources.publish_resource(&doc, &place_id).is_none());
        }

        eprintln!("phase locations_refuse_local_scope_or_out_of_range_coordinates");
        {
            let mut doc = AutoCommit::new();
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

        eprintln!("phase deliberate_offline_map_locations_are_shared_and_addressable");
        {
            let mut doc = AutoCommit::new();
            let photo = portable_photo("pinned-place.png", 40, 20);
            let id = photo.id.clone();
            PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();
            let location = PhotoLocation {
                latitude: 52.520_008,
                longitude: 13.404_954,
                label: Some("Meeting point".to_owned()),
                provenance: crate::model::LocationProvenance {
                    source: "map".to_owned(),
                    scope: "shared".to_owned(),
                },
            };
            PhotosAction::SetPhotoLocation {
                photo_id: id,
                location: location.clone(),
            }
            .apply(&mut doc)
            .unwrap();
            assert!(location.is_valid());
            let resource = PhotosResources
                .publish_resource(&doc, &place_resource_id(&location.place_key()))
                .expect("map pin has one canonical place resource");
            assert_eq!(resource.label, "Meeting point");
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == "view-place")
            );
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "end-to-end scenario reads top to bottom through every analysis component resource"
    )]
    #[test]
    fn shared_analysis_is_bounded_typed_pinned_and_cancellable() {
        eprintln!("phase explicit_shared_analysis_is_bounded_addressable_and_removable");
        {
            let mut doc = AutoCommit::new();
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
                    .publish_resource(&doc, &object_resource_id("bicycle"))
                    .is_some()
            );
            assert!(
                PhotosResources
                    .publish_resource(&doc, &pet_resource_id("dog"))
                    .is_some()
            );
            let resource = PhotosResources.publish_resource(&doc, &id).unwrap();
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == "analyze-text")
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

        eprintln!("phase analysis_components_are_bounded_typed_and_exactly_materializable");
        {
            let mut doc = AutoCommit::new();
            let photo = portable_photo("inspectable-analysis.png", 40, 20);
            let id = photo.id.clone();
            let content = photo.image.content.clone().unwrap();
            PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();

            let ocr_provenance = crate::model::AnalysisProvenance {
                source: "local-ocr".into(),
                engine: "tilo-ocr".into(),
                language: "deu".into(),
                content: content.clone(),
                scope: "shared".into(),
                ..Default::default()
            };
            let vision_provenance = crate::model::AnalysisProvenance {
                source: "local-vision".into(),
                engine: "tilo-vision".into(),
                model_id: "local-vision-model".into(),
                model_revision: "a".repeat(64),
                content: content.clone(),
                scope: "shared".into(),
                ..Default::default()
            };
            let analysis = ImageAnalysis {
                ocr_text: "α".repeat(20_000),
                description: "A broad city view with bicycles and dogs. ".repeat(16),
                objects: (0..40).map(|index| format!("object-{index}")).collect(),
                pets: (0..40).map(|index| format!("pet-{index}")).collect(),
                people: vec!["Ada".into(), "Sam".into()],
                provenance: vision_provenance.clone(),
                ocr_provenance: Some(ocr_provenance),
                vision_provenance: Some(vision_provenance),
            };
            PhotosAction::SetImageAnalysis {
                photo_id: id.clone(),
                analysis: Some(analysis),
            }
            .apply(&mut doc)
            .unwrap();

            let analysis_id = format!("{id}#analysis");
            let resource = PhotosResources
                .publish_resource(&doc, &analysis_id)
                .expect("analysis is directly addressable");
            assert_eq!(resource.kind, "photos.analysis");
            assert_eq!(resource.children.len(), 2, "only OCR and vision components");
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == "remove-analysis")
            );

            let ocr = PhotosResources
                .publish_resource(&doc, &format!("{analysis_id}/ocr"))
                .expect("OCR component is directly addressable");
            assert_eq!(ocr.kind, "photos.analysis.ocr");
            let text = ocr
                .children
                .iter()
                .find(|child| child.kind == "photos.analysis.ocr.text")
                .expect("OCR publishes typed text");
            assert_eq!(text.children.len(), ANALYSIS_TEXT_CONTEXT_CHUNKS);
            assert!(
                text.children
                    .iter()
                    .all(|chunk| chunk.label.len() <= ANALYSIS_TEXT_CHUNK_BYTES)
            );
            let refresh = ocr
                .actions
                .iter()
                .find(|action| action.id == "analyze-text")
                .expect("OCR component owns canonical refresh");
            let tilo_actions::ActionInput::Immediate(encoded) = &refresh.input else {
                panic!("refresh is an immediate operation request");
            };
            assert_eq!(
                decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
                Some(PhotosEffect::AnalyzeText {
                    resource_id: id.clone(),
                    language: "deu".into(),
                })
            );

            let late_text_id = format!("{analysis_id}/ocr/text/20");
            let late_text = PhotosResources
                .publish_resource(&doc, &late_text_id)
                .expect("OCR outside the contextual window resolves exactly");
            assert_eq!(late_text.kind, "photos.analysis.ocr.text.part");
            assert!(!late_text.label.is_empty());
            assert!(late_text.label.len() <= ANALYSIS_TEXT_CHUNK_BYTES);

            let late_object_id = format!("{analysis_id}/vision/objects/20");
            let late_object = PhotosResources
                .publish_resource(&doc, &late_object_id)
                .expect("object outside the contextual window resolves exactly");
            assert_eq!(late_object.kind, "photos.analysis.objects.label");
            assert_eq!(late_object.label, "object-20");

            let language = PhotosResources
                .publish_resource(&doc, &format!("{analysis_id}/ocr/provenance/language"))
                .expect("OCR language provenance is inspectable");
            assert_eq!(language.kind, "photos.analysis.provenance.language");
            assert_eq!(language.label, "language: deu");
            let model = PhotosResources
                .publish_resource(&doc, &format!("{analysis_id}/vision/provenance/model"))
                .expect("vision model provenance is inspectable");
            assert_eq!(model.kind, "photos.analysis.provenance.model");
            assert_eq!(model.label, "Model: local-vision-model");
            let revision = PhotosResources
                .publish_resource(&doc, &format!("{analysis_id}/vision/provenance/revision"))
                .expect("vision model revision is inspectable");
            assert_eq!(revision.kind, "photos.analysis.provenance.revision");
            assert_eq!(
                revision.label,
                format!("Model revision: {}", "a".repeat(64))
            );
            let analyzed_content = PhotosResources
                .publish_resource(&doc, &format!("{analysis_id}/ocr/provenance/content"))
                .expect("analyzed content identity is inspectable");
            assert_eq!(analyzed_content.kind, "photos.analysis.provenance.content");
            assert_eq!(analyzed_content.label, format!("content: {content}"));

            let vision = PhotosResources
                .publish_resource(&doc, &format!("{analysis_id}/vision"))
                .expect("vision component is directly addressable");
            let refresh = vision
                .actions
                .iter()
                .find(|action| action.id == "analyze-image")
                .expect("vision component owns canonical refresh");
            let tilo_actions::ActionInput::Immediate(encoded) = &refresh.input else {
                panic!("refresh is an immediate operation request");
            };
            assert_eq!(
                decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
                Some(PhotosEffect::AnalyzeImage { resource_id: id })
            );
            assert!(
                PhotosResources
                    .publish_resource(&doc, &format!("{analysis_id}/vision/objects/40"))
                    .is_none(),
                "exact lookup refuses an out-of-range derived child"
            );
        }

        eprintln!("phase image_analysis_is_a_cancellable_background_operation");
        {
            let spec = operation(
                "Analyze image locally",
                &PhotosEffect::AnalyzeImage {
                    resource_id: "photos.photo:abc".to_owned(),
                },
            );
            assert_eq!(spec.kind, OperationKind::Effect);
            assert_eq!(
                spec.contract.execution,
                ExecutionMode::Background { cancellable: true }
            );
            assert_eq!(spec.contract.result, ResultSpec::Job);
            assert_eq!(spec.contract.retry, RetryPolicy::AtMostOnce);
        }

        eprintln!("phase vision_analysis_requires_a_pinned_model_provenance");
        {
            let mut doc = AutoCommit::new();
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

        eprintln!("phase library_analysis_is_a_first_class_cancellable_resource_surface");
        {
            let doc = AutoCommit::new();
            let resource = PhotosResources
                .publish_resource(&doc, "photos.library-analysis")
                .expect("library analysis resolves without materializing the gallery");
            assert_eq!(resource.kind, "photos.library-analysis");
            for id in [
                "view-library-analysis",
                "analyze-library",
                "pause-library-analysis",
                "resume-library-analysis",
                "retry-library-analysis-failures",
            ] {
                assert!(
                    resource.actions.iter().any(|action| action.id == id),
                    "missing canonical {id} action"
                );
            }
            for id in [
                "analyze-library",
                "resume-library-analysis",
                "retry-library-analysis-failures",
            ] {
                let action = resource
                    .actions
                    .iter()
                    .find(|action| action.id == id)
                    .unwrap();
                assert_eq!(
                    action.contract.execution,
                    ExecutionMode::Background { cancellable: true }
                );
                assert_eq!(action.contract.result, ResultSpec::Job);
            }
            let root = PhotosResources
                .publish_resources(&doc)
                .pop()
                .expect("library root");
            assert!(
                root.children
                    .iter()
                    .any(|child| child.id.0 == "photos.library-analysis")
            );
        }
    }

    fn shared_ocr_component(content: &str, text: &str) -> ImageAnalysis {
        let provenance = crate::model::AnalysisProvenance {
            source: "local-ocr".into(),
            engine: "tilo-ocr".into(),
            language: "eng".into(),
            content: content.into(),
            scope: "shared".into(),
            ..Default::default()
        };
        ImageAnalysis {
            ocr_text: text.into(),
            provenance: provenance.clone(),
            ocr_provenance: Some(provenance),
            ..Default::default()
        }
    }

    fn shared_vision_component(content: &str) -> ImageAnalysis {
        let provenance = crate::model::AnalysisProvenance {
            source: "local-vision".into(),
            engine: "tilo-vision".into(),
            model_id: "signed-vision-model".into(),
            model_revision: "a".repeat(64),
            content: content.into(),
            scope: "shared".into(),
            ..Default::default()
        };
        ImageAnalysis {
            ocr_text: "incidental sign text".into(),
            description: "A dog beside a red bicycle".into(),
            objects: vec!["bicycle".into()],
            pets: vec!["dog".into()],
            provenance: provenance.clone(),
            ocr_provenance: Some(provenance.clone()),
            vision_provenance: Some(provenance),
            ..Default::default()
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn analysis_components_merge_by_content_and_refresh_only_their_own_fields() {
        eprintln!("phase ocr_then_vision_merges_components_without_replacing_dedicated_text");
        {
            let mut doc = AutoCommit::new();
            let photo = portable_photo("ocr-then-vision.png", 40, 20);
            let id = photo.id.clone();
            let content = photo.image.content.clone().unwrap();
            PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();

            for analysis in [
                shared_ocr_component(&content, "dedicated receipt text"),
                shared_vision_component(&content),
            ] {
                PhotosAction::MergeImageAnalysis {
                    photo_id: id.clone(),
                    analysis,
                }
                .apply(&mut doc)
                .unwrap();
            }

            let merged = PhotosDoc::photo(&doc, &id).unwrap().analysis.unwrap();
            assert_eq!(merged.ocr_text, "dedicated receipt text");
            assert_eq!(merged.description, "A dog beside a red bicycle");
            assert_eq!(merged.objects, ["bicycle"]);
            assert_eq!(merged.pets, ["dog"]);
            assert_eq!(merged.ocr_provenance.unwrap().source, "local-ocr");
            assert_eq!(merged.vision_provenance.unwrap().source, "local-vision");
        }

        eprintln!("phase vision_then_ocr_replaces_only_text_and_rejects_cross_content_merges");
        {
            let mut doc = AutoCommit::new();
            let photo = portable_photo("vision-then-ocr.png", 40, 20);
            let id = photo.id.clone();
            let content = photo.image.content.clone().unwrap();
            PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();

            PhotosAction::MergeImageAnalysis {
                photo_id: id.clone(),
                analysis: shared_vision_component(&content),
            }
            .apply(&mut doc)
            .unwrap();
            PhotosAction::MergeImageAnalysis {
                photo_id: id.clone(),
                analysis: shared_ocr_component(&content, "new dedicated text"),
            }
            .apply(&mut doc)
            .unwrap();

            let merged = PhotosDoc::photo(&doc, &id).unwrap().analysis.unwrap();
            assert_eq!(merged.ocr_text, "new dedicated text");
            assert_eq!(merged.description, "A dog beside a red bicycle");
            assert_eq!(merged.objects, ["bicycle"]);
            assert_eq!(merged.ocr_provenance.unwrap().source, "local-ocr");
            assert_eq!(merged.vision_provenance.unwrap().source, "local-vision");

            assert!(
                PhotosAction::MergeImageAnalysis {
                    photo_id: id,
                    analysis: shared_ocr_component(&"f".repeat(64), "wrong bytes"),
                }
                .apply(&mut doc)
                .is_err()
            );
        }

        eprintln!("phase a_new_vision_pass_refreshes_only_incidental_vision_ocr");
        {
            let mut doc = AutoCommit::new();
            let photo = portable_photo("vision-refresh.png", 40, 20);
            let id = photo.id.clone();
            let content = photo.image.content.clone().unwrap();
            PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();

            let mut first = shared_vision_component(&content);
            first.ocr_text = "old sign".into();
            PhotosAction::MergeImageAnalysis {
                photo_id: id.clone(),
                analysis: first,
            }
            .apply(&mut doc)
            .unwrap();
            let mut refreshed = shared_vision_component(&content);
            refreshed.ocr_text.clear();
            refreshed.ocr_provenance = None;
            refreshed.description = "The same scene after a model refresh".into();
            PhotosAction::MergeImageAnalysis {
                photo_id: id.clone(),
                analysis: refreshed,
            }
            .apply(&mut doc)
            .unwrap();

            let merged = PhotosDoc::photo(&doc, &id).unwrap().analysis.unwrap();
            assert!(merged.ocr_text.is_empty());
            assert!(merged.ocr_provenance.is_none());
            assert_eq!(merged.description, "The same scene after a model refresh");
            assert!(merged.vision_provenance.is_some());
        }

        eprintln!(
            "phase legacy_combined_local_ocr_record_can_refresh_ocr_without_losing_visual_fields"
        );
        {
            let mut doc = AutoCommit::new();
            let photo = portable_photo("legacy-combined.png", 40, 20);
            let id = photo.id.clone();
            let content = photo.image.content.clone().unwrap();
            PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();
            PhotosAction::SetImageAnalysis {
                photo_id: id.clone(),
                analysis: Some(shared_analysis(content.clone())),
            }
            .apply(&mut doc)
            .unwrap();

            PhotosAction::MergeImageAnalysis {
                photo_id: id.clone(),
                analysis: shared_ocr_component(&content, "refreshed receipt"),
            }
            .apply(&mut doc)
            .unwrap();
            let merged = PhotosDoc::photo(&doc, &id).unwrap().analysis.unwrap();
            assert_eq!(merged.ocr_text, "refreshed receipt");
            assert_eq!(merged.description, "A dog beside a red bicycle");
            assert_eq!(merged.objects, ["bicycle"]);
        }

        eprintln!(
            "phase fresh_component_replaces_stale_existing_analysis_instead_of_being_blocked"
        );
        {
            let mut doc = AutoCommit::new();
            let photo = portable_photo("stale-analysis.png", 40, 20);
            let id = photo.id.clone();
            let content = photo.image.content.clone().unwrap();
            PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();
            let stale_content = "f".repeat(64);
            let mut library = PhotosDoc::library(&doc);
            library.photo_mut(&id).unwrap().analysis =
                Some(shared_vision_component(&stale_content));
            PhotosDoc::set_library(&mut doc, &library);

            PhotosAction::MergeImageAnalysis {
                photo_id: id.clone(),
                analysis: shared_ocr_component(&content, "current bytes"),
            }
            .apply(&mut doc)
            .unwrap();
            let merged = PhotosDoc::photo(&doc, &id).unwrap().analysis.unwrap();
            assert_eq!(merged.ocr_text, "current bytes");
            assert!(merged.description.is_empty());
            assert!(merged.vision_provenance.is_none());
        }

        eprintln!(
            "phase new_path_only_analysis_is_rejected_but_empty_vision_completion_is_retained"
        );
        {
            let mut doc = AutoCommit::new();
            let path_only = LibraryPhoto::from_image(OpenImage::from_path("legacy.png", 40, 20));
            let path_id = path_only.id.clone();
            PhotosDoc::set_library(
                &mut doc,
                &crate::model::PhotoLibrary {
                    photos: vec![path_only],
                    ..Default::default()
                },
            );
            assert!(
                PhotosAction::MergeImageAnalysis {
                    photo_id: path_id.clone(),
                    analysis: shared_ocr_component("", "unbound text"),
                }
                .apply(&mut doc)
                .is_err()
            );
            assert!(
                PhotosAction::SetImageAnalysis {
                    photo_id: path_id,
                    analysis: Some(shared_ocr_component("", "unbound text")),
                }
                .apply(&mut doc)
                .is_err()
            );

            let photo = portable_photo("blank-vision.png", 40, 20);
            let id = photo.id.clone();
            let content = photo.image.content.clone().unwrap();
            PhotosAction::ImportPhoto { photo }.apply(&mut doc).unwrap();
            let mut blank = shared_vision_component(&content);
            blank.ocr_text.clear();
            blank.description.clear();
            blank.objects.clear();
            blank.pets.clear();
            blank.ocr_provenance = None;
            PhotosAction::MergeImageAnalysis {
                photo_id: id.clone(),
                analysis: blank,
            }
            .apply(&mut doc)
            .unwrap();
            let stored = PhotosDoc::photo(&doc, &id).unwrap().analysis.unwrap();
            assert!(stored.is_empty());
            assert!(stored.vision_provenance.is_some());
        }
    }

    #[test]
    fn photo_resources_are_addressed_by_content_and_published_with_their_facts() {
        // The address of a photograph that names its content is that content.
        //
        // Asserted on the published tree rather than on `resource_id` alone, so
        // it is the id a caller actually resolves against.
        eprintln!("phase a_photograph_that_names_its_content_is_addressed_by_it");
        {
            const HASH: &str = "623fdbf0c19749ddd1116f8c68b561ad648e3023ca4f304a732bf7b06d1ffb82";
            let mut doc = AutoCommit::new();
            let img =
                OpenImage::from_path("/run/user/1000/qlva3099960/tilo-opened/x.png", 800, 400)
                    .with_content(HASH);
            add_photo(&mut doc, img);

            let res = PhotosResources.publish_resources(&doc);
            let image = published_photo(&res);
            assert_eq!(image.id.0, format!("photos.photo:{HASH}"));
            assert_eq!(
                content_of(&image.id.0),
                Some(HASH),
                "the address does not lead back to the content it names"
            );
            // And the file's name is still what a person reads.
            assert_eq!(image.label, "x.png");
        }

        // The path-shaped id an older document produces is not mistaken for a
        // content address. A caller that got one has to fall back to the path,
        // and would otherwise ask the content store for a directory name.
        eprintln!("phase a_path_shaped_address_names_no_content");
        {
            let doc = doc_with_image();
            let res = PhotosResources.publish_resources(&doc);
            assert_eq!(content_of(&published_photo(&res).id.0), None);
            assert_eq!(content_of("photos.photo:"), None);
            assert_eq!(content_of("files.entry:/x"), None);
            // Right length, wrong alphabet: hex is what a hash is written in.
            assert_eq!(
                content_of(&format!("photos.photo:{}", "z".repeat(64))),
                None
            );
        }

        eprintln!("phase a_photograph_is_published_with_its_name_and_size");
        {
            let doc = doc_with_image();
            let res = PhotosResources.publish_resources(&doc);
            let image = published_photo(&res);
            assert_eq!(image.label, "harbour.png");
            assert_eq!(
                image.id.0,
                "photos.photo:path:/var/tmp/tilo/home/user/harbour.png"
            );
            // The size is discoverable from titles alone.
            assert_eq!(image.children[0].label, "800 x 400 pixels");
        }

        eprintln!("phase nothing_open_publishes_an_openable_library_resource");
        {
            let doc = AutoCommit::new();
            let resources = PhotosResources.publish_resources(&doc);
            assert_eq!(resources.len(), 1);
            assert_eq!(resources[0].kind, "photos.library");
            // The library's own effects join it where the framework hosts it.
            let effects: Vec<_> = PhotosEffect::specs()
                .into_iter()
                .map(|spec| spec.id)
                .collect();
            for id in ["view-library", "import", "import-directory", "copy", "save"] {
                assert!(effects.iter().any(|effect| effect == id), "missing {id}");
            }
            for id in ["create-album", "set-favorites", "set-metadata-batch"] {
                assert!(
                    resources[0].actions.iter().any(|action| action.id == id),
                    "published library is missing {id}"
                );
            }
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn collections_and_exact_lookups_are_bounded_and_canonical() {
        eprintln!("phase albums_are_a_canonical_bounded_collection_with_exact_child_resolution");
        {
            let mut doc = AutoCommit::new();
            let mut library = crate::model::PhotoLibrary::default();
            for index in 0..(ALBUM_CONTEXT_LIMIT + 7) {
                library.albums.push(Album {
                    id: format!("photos.album:{index}"),
                    name: format!("Album {index}"),
                    photo_ids: Vec::new(),
                });
            }
            let late_id = library.albums.last().unwrap().id.clone();
            PhotosDoc::set_library(&mut doc, &library);

            let albums = PhotosResources
                .publish_resource(&doc, "photos.albums")
                .expect("the album collection is independently addressable");
            assert_eq!(albums.kind, ALBUMS_KIND);
            assert_eq!(
                albums
                    .children
                    .iter()
                    .filter(|child| child.kind == "photos.album")
                    .count(),
                ALBUM_CONTEXT_LIMIT
            );
            assert!(albums.children.iter().any(|child| {
                child.id.0 == "photos.albums#count"
                    && child.label == format!("{} albums", library.albums.len())
            }));
            let open = albums
                .actions
                .iter()
                .find(|action| action.id == "view-albums")
                .expect("the collection exposes canonical navigation");
            let tilo_actions::ActionInput::Immediate(encoded) = &open.input else {
                panic!("album browsing is a presentation operation");
            };
            assert_eq!(
                decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
                Some(PhotosEffect::ViewAlbums)
            );

            let root = PhotosResources.publish_resources(&doc).pop().unwrap();
            assert!(
                root.children
                    .iter()
                    .any(|child| child.id.0 == "photos.albums")
            );
            assert!(
                root.children
                    .iter()
                    .all(|child| child.kind != "photos.album"),
                "albums belong beneath their collection instead of duplicating the root namespace"
            );
            assert_eq!(
                PhotosResources
                    .publish_resource(&doc, &late_id)
                    .unwrap()
                    .id
                    .0,
                late_id,
                "albums beyond contextual enumeration remain exactly addressable"
            );
        }

        eprintln!("phase favorites_are_a_stable_browse_resource_with_an_honest_count");
        {
            let mut doc = AutoCommit::new();
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
                .publish_resource(&doc, "photos.favorites")
                .expect("favorites is addressable even outside a materialized shelf");
            assert_eq!(favorites.kind, FAVORITES_KIND);
            assert!(
                favorites
                    .actions
                    .iter()
                    .any(|action| action.id == "view-favorites")
            );
            assert!(
                favorites
                    .children
                    .iter()
                    .any(|child| child.label == "1 photographs")
            );
            let root = PhotosResources
                .publish_resources(&doc)
                .pop()
                .expect("library root");
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

        eprintln!("phase large_library_snapshot_is_bounded_but_late_ids_resolve_directly");
        {
            let mut doc = AutoCommit::new();
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
            let root = PhotosResources.publish_resources(&doc).pop().unwrap();
            assert!(
                root.children
                    .iter()
                    .filter(|child| child.kind == "photos.photo")
                    .count()
                    <= LIBRARY_CONTEXT_LIMIT,
                "contextual photos stay bounded even as fixed virtual collections are added"
            );
            let resolved = PhotosResources
                .publish_resource(&doc, &late)
                .expect("late stable id resolves");
            assert_eq!(resolved.id.0, late);
            assert!(resolved.actions.iter().any(|action| action.id == "view"));
            assert!(
                PhotosResources
                    .publish_resource(&doc, "photos.photo:stale")
                    .is_none()
            );
        }

        eprintln!(
            "phase open_editor_resource_resolves_exactly_without_falling_through_to_library_ids"
        );
        {
            let doc = doc_with_image();
            let id = first_id(&doc);
            let resource = PhotosResources
                .publish_resource(&doc, &id)
                .expect("a photograph is independently addressable");
            assert_eq!(resource.kind, PHOTO_KIND);
            assert!(resource.actions.iter().any(|action| action.id == "crop"));
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == "rotate-right")
            );
        }

        eprintln!("phase exact_lookup_rejects_unbounded_untrusted_ids");
        {
            let doc = doc_with_image();
            assert!(
                PhotosResources
                    .publish_resource(&doc, &"x".repeat(MAX_RESOURCE_ID_CHARS + 1))
                    .is_none()
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn capture_dates_and_text_chunks_parse_within_bounds_without_panicking() {
        eprintln!("phase timeline_boundaries_are_canonical_actions_with_a_bounded_context_tree");
        {
            let mut doc = AutoCommit::new();
            let mut library = crate::model::PhotoLibrary::default();
            for index in 0..80 {
                let mut photo = portable_photo(&format!("timeline-{index}.png"), 40, 20);
                photo.captured_at =
                    Some(format!("2026-{:02}-{:02}", index / 28 + 1, index % 28 + 1));
                library.photos.push(photo);
            }
            // A non-calendar label remains in the library but is not made into a
            // fake chronological boundary.
            let mut undated = portable_photo("undated.png", 40, 20);
            undated.captured_at = Some("summer holidays".into());
            library.photos.push(undated);
            PhotosDoc::set_library(&mut doc, &library);

            let root = PhotosResources
                .publish_resources(&doc)
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
                .find(|action| action.id == "view-timeline")
                .expect("timeline has a stable open action");
            let tilo_actions::ActionInput::Immediate(encoded) = &open.input else {
                panic!("timeline opening is a presentation operation");
            };
            assert_eq!(
                decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
                Some(PhotosEffect::ViewTimeline { anchor: None })
            );

            let day_id = timeline_day_resource_id("2026-01-01");
            let day = PhotosResources
                .publish_resource(&doc, &day_id)
                .expect("late timeline day resolves outside the bounded root context");
            let action = day
                .actions
                .iter()
                .find(|action| action.id == "view-timeline")
                .expect("day has a stable open action");
            let tilo_actions::ActionInput::Immediate(encoded) = &action.input else {
                panic!("timeline day opening is a presentation operation");
            };
            assert_eq!(
                decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
                Some(PhotosEffect::ViewTimeline {
                    anchor: Some(TimelineAnchor::Day {
                        day: "2026-01-01".into(),
                    }),
                })
            );
            assert!(
                PhotosResources
                    .publish_resource(&doc, "photos.timeline:day:summer holidays")
                    .is_none(),
                "free-form metadata cannot masquerade as a chronological resource"
            );
        }

        eprintln!("phase timeline_boundaries_refuse_invalid_or_free_form_capture_dates");
        {
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

        eprintln!("phase timeline_day_parsing_handles_short_and_multibyte_captures");
        {
            for capture in [
                "",
                "2024",
                "2024-02-2",
                "2024-0é-01",
                "2024é02-01",
                "2024-02-01é",
            ] {
                let expected = (capture == "2024-02-01é").then(|| "2024-02-01".to_owned());
                assert_eq!(timeline_day_from_capture(capture), expected, "{capture:?}");
            }
            assert_eq!(
                timeline_day_from_capture("2000-02-29"),
                Some("2000-02-29".into())
            );
            assert!(timeline_day_from_capture("1900-02-29").is_none());
            assert!(timeline_day_from_capture("2024+02-01").is_none());
        }

        eprintln!(
            "phase capture_values_with_short_bodies_or_suffixes_are_refused_without_panicking"
        );
        {
            for value in [
                "",
                "Z",
                "+02:00",
                "2026-09-18Z",
                "2026-09-18T14Z",
                "2026-09-18T14:30:01+2:00",
                "2026-09-18T14:30:01+02-00",
                "2026-09-18T14:30:01+24:00",
                "2026-09-18T14:30:01+02:60",
                "2026-09-18T14:30+02:00",
                "2026-09-18T24:00:00",
                "2026-09-18T14:60:00",
                "2026-09-18T14:30:60",
                "2026-09-18é14:30:00",
            ] {
                assert!(canonical_capture_at_value(value).is_none(), "{value:?}");
            }
            assert_eq!(
                canonical_capture_at_value("2026:09:18 14:30:01"),
                Some("2026-09-18T14:30:01".into())
            );
            assert_eq!(
                canonical_capture_at_value("2026-09-18T14:30:01+00:00"),
                Some("2026-09-18T14:30:01Z".into())
            );
            assert_eq!(
                canonical_capture_at_value("2026-09-18T14:30:01-00:00"),
                Some("2026-09-18T14:30:01Z".into())
            );
            assert_eq!(
                canonical_capture_at_value("2026-09-18T14:30:01-05:30"),
                Some("2026-09-18T14:30:01-05:30".into())
            );
            assert_eq!(
                canonical_capture_at_value("2026-09-18"),
                Some("2026-09-18".into())
            );
            assert_eq!(
                canonical_capture_at_value("2026-09-18T14:30"),
                Some("2026-09-18T14:30".into())
            );
        }

        eprintln!("phase text_chunks_respect_utf8_boundaries_and_the_byte_budget");
        {
            assert!(bounded_text_chunks("").is_empty());
            assert_eq!(bounded_text_chunks("short"), vec!["short"]);
            let text = "é".repeat(ANALYSIS_TEXT_CHUNK_BYTES);
            let chunks = bounded_text_chunks(&text);
            assert_eq!(chunks.concat(), text);
            assert!(
                chunks
                    .iter()
                    .all(|chunk| !chunk.is_empty() && chunk.len() <= ANALYSIS_TEXT_CHUNK_BYTES)
            );
            assert_eq!(chunks.len(), 2);
            let exact = "a".repeat(ANALYSIS_TEXT_CHUNK_BYTES);
            assert_eq!(bounded_text_chunks(&exact), vec![exact.as_str()]);
            let over = "a".repeat(ANALYSIS_TEXT_CHUNK_BYTES + 1);
            assert_eq!(bounded_text_chunks(&over).len(), 2);
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn published_actions_have_stable_ids_valid_contracts_and_no_side_effects() {
        eprintln!("phase every_action_is_addressable_by_a_stable_id");
        {
            let doc = doc_with_image();
            let res = PhotosResources.publish_resources(&doc);
            let ids: Vec<&str> = published_photo(&res)
                .actions
                .iter()
                .map(|a| a.id.as_str())
                .collect();
            assert_eq!(
                ids,
                vec![
                    "view",
                    "rotate-right",
                    "rotate-left",
                    "crop",
                    "favorite",
                    "share-reference",
                    "set-metadata",
                    "set-location",
                    "analyze-text",
                    "analyze-image",
                    "remove-photo",
                ]
            );
        }

        eprintln!("phase every_published_action_has_a_valid_machine_readable_contract");
        {
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
            for resource in PhotosResources.publish_resources(&empty) {
                check(&resource);
            }
            for resource in PhotosResources.publish_resources(&doc_with_image()) {
                check(&resource);
            }
        }

        eprintln!("phase exported_photo_operations_declare_arguments_and_execution_contracts");
        {
            let res = PhotosResources.publish_resources(&doc_with_image());
            let image = published_photo(&res);
            let crop = image.actions.iter().find(|a| a.id == "crop").unwrap();
            assert_eq!(crop.params()[0].name.as_ref(), "region");
            assert!(crop.params()[0].schema.is_some());
            assert!(crop.contract.reversible);

            // Opening a photograph shows it in this window: presentation only.
            let view = image.actions.iter().find(|a| a.id == "view").unwrap();
            assert_eq!(view.contract.retry, RetryPolicy::Idempotent);
            assert_eq!(view.kind, OperationKind::View);

            // Importing and getting the shown photograph out are effects of
            // the library's window.
            let effects = PhotosEffect::specs();
            let import = effects.iter().find(|a| a.id == "import").unwrap();
            assert_eq!(
                import.contract.execution,
                tilo_actions::ExecutionMode::Background { cancellable: true }
            );
            assert_eq!(import.contract.result, tilo_actions::ResultSpec::Job);
            assert_eq!(import.params()[0].name.as_ref(), "path");
            assert_eq!(import.kind, OperationKind::Effect);
            assert!(effects.iter().any(|a| a.id == "copy"));
            assert!(effects.iter().any(|a| a.id == "save"));
        }

        // Enumerating must be a pure read. If listing what is on offer could
        // mutate the document, a palette opening would silently edit the user's
        // photograph.
        eprintln!("phase enumeration_fires_no_side_effects");
        {
            let mut doc = doc_with_image();
            let before_heads = doc.get_heads();
            let before_bytes = doc.save();

            let res = PhotosResources.publish_resources(&doc);
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

        eprintln!("phase an_action_round_trips_through_the_registry");
        {
            let registry = ActionRegistry::with(register);
            let mut doc = doc_with_image();
            let original = PhotosAction::set(&first_id(&doc), &Edits::default().rotated_left());
            let decoded = registry.decode(&encode(&original)).expect("decode");
            decoded.apply(&mut doc).unwrap();
            assert_eq!(first_edits(&doc).rotate_quarters, 3);
        }
    }

    #[test]
    fn edit_actions_rotate_compose_crops_and_offer_reset_only_when_edited() {
        eprintln!("phase reset_appears_only_once_there_is_an_edit_to_discard");
        {
            let mut doc = doc_with_image();
            let before = PhotosResources.publish_resources(&doc);
            assert!(
                !published_photo(&before)
                    .actions
                    .iter()
                    .any(|a| a.id == "reset")
            );

            set_first_edits(&mut doc, Edits::default().rotated_right());
            let after = PhotosResources.publish_resources(&doc);
            assert!(
                published_photo(&after)
                    .actions
                    .iter()
                    .any(|a| a.id == "reset")
            );
        }

        eprintln!("phase applying_rotate_right_advances_the_document_by_one_quarter");
        {
            let mut doc = doc_with_image();
            let action = PhotosAction::set(&first_id(&doc), &first_edits(&doc).rotated_right());
            action.apply(&mut doc).unwrap();
            assert_eq!(first_edits(&doc).rotate_quarters, 1);
        }

        eprintln!("phase a_prompted_crop_composes_onto_the_edits_in_force");
        {
            let mut doc = doc_with_image();
            let res = PhotosResources.publish_resources(&doc);
            let crop = published_photo(&res)
                .actions
                .iter()
                .find(|a| a.id == "crop")
                .unwrap();
            let built = crop
                .build_text("0,0,0.5,0.5")
                .expect("crop collects an argument");
            ActionRegistry::with(register)
                .decode(&built)
                .unwrap()
                .apply(&mut doc)
                .unwrap();
            let e = first_edits(&doc);
            assert!((e.crop.w - 0.5).abs() < 1e-9, "{:?}", e.crop);
            assert!((e.crop.h - 0.5).abs() < 1e-9, "{:?}", e.crop);
        }
    }

    #[test]
    fn row_edits_are_declarations_bound_to_their_row_and_kept_out_of_the_palette() {
        let mut doc = doc_with_image();
        let id = first_id(&doc);
        let library = PhotosResources.publish_resources(&doc).pop().unwrap();
        // On the library, a row edit names its item and is for scripts.
        let root_remove = library
            .actions
            .iter()
            .find(|action| action.id == "remove-photo")
            .expect("the declaration stays on the library");
        assert!(root_remove.is_programmatic());
        assert!(
            root_remove
                .params()
                .iter()
                .any(|param| param.name == "photo_id")
        );
        // On the photograph, the same declaration is bound to it.
        let resources = PhotosResources.publish_resources(&doc);
        let remove = published_photo(&resources)
            .actions
            .iter()
            .find(|action| action.id == "remove-photo")
            .expect("the row offers its removal")
            .clone();
        assert!(!remove.is_programmatic());
        assert!(remove.params().is_empty(), "the row names the photograph");
        let tilo_actions::ActionInput::Immediate(encoded) = &remove.input else {
            panic!("a bound removal asks nothing");
        };
        let registry = ActionRegistry::with(register);
        registry.decode(encoded).unwrap().apply(&mut doc).unwrap();
        assert!(PhotosDoc::photo(&doc, &id).is_none());
    }

    #[test]
    fn library_photo_resources_remove_share_and_refuse_hostile_payloads() {
        eprintln!("phase the_selected_photo_can_be_removed_through_its_canonical_resource");
        {
            let mut doc = doc_with_image();
            let id = first_id(&doc);
            let resource = PhotosResources.publish_resource(&doc, &id).unwrap();
            let remove = resource
                .actions
                .iter()
                .find(|action| action.id == "remove-photo")
                .expect("the selected photo still offers removal");
            let registry = ActionRegistry::with(register);
            let tilo_actions::ActionInput::Immediate(encoded) = &remove.input else {
                panic!("removal is immediate");
            };
            registry.decode(encoded).unwrap().apply(&mut doc).unwrap();
            assert!(PhotosDoc::library(&doc).photos.is_empty());
        }

        eprintln!("phase an_unselected_library_photo_exposes_a_share_reference_for_its_own_id");
        {
            let mut doc = AutoCommit::new();
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
                .publish_resource(&doc, &id)
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
                Some(PhotosEffect::ShareReference { resource_id: id })
            );
        }

        eprintln!("phase hostile_durable_photo_and_album_payloads_are_refused");
        {
            let mut doc = AutoCommit::new();
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
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn metadata_batches_are_atomic_bounded_and_validated_as_a_whole() {
        eprintln!(
            "phase shared_metadata_is_bounded_replayable_and_publishes_each_metadata_resource"
        );
        {
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
                .publish_resource(&doc, &photo_id)
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
                    .publish_resource(&doc, &id)
                    .expect("virtual resource resolves");
                assert!(
                    resource
                        .actions
                        .iter()
                        .any(|action| action.kind == OperationKind::View)
                );
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
                    PhotosResources.publish_resource(&doc, &id).is_none(),
                    "stale metadata id {id} must not materialize"
                );
            }
        }

        eprintln!("phase metadata_batch_distinguishes_keep_clear_and_set_atomically");
        {
            let mut doc = AutoCommit::new();
            let mut first = portable_photo("/photos/first.png", 20, 10);
            first.folder = Some("Old folder".into());
            first.people = vec!["Sam".into()];
            first.tags = vec!["old".into()];
            first.captured_at = Some("2024-01-02".into());
            let mut second = portable_photo("/photos/second.png", 20, 10);
            second.folder = Some("Other folder".into());
            second.people = vec!["Noor".into()];
            second.tags = vec!["other".into()];
            PhotosDoc::set_library(
                &mut doc,
                &crate::model::PhotoLibrary {
                    photos: vec![first.clone(), second.clone()],
                    albums: Vec::new(),
                },
            );

            PhotosAction::SetMetadataBatch {
                photo_ids: vec![first.id.clone(), second.id],
                folder: MetadataPatch::Clear,
                people: MetadataLabelsPatch::Keep,
                tags: MetadataLabelsPatch::Set(vec!["trip".into(), "rain".into()]),
                captured_at: MetadataPatch::Set("2026-09-18 14:30".into()),
                location: MetadataPatch::Set(PhotoLocation {
                    latitude: 52.3676,
                    longitude: 4.9041,
                    label: Some("Amsterdam".into()),
                    provenance: crate::model::LocationProvenance {
                        source: "user".into(),
                        scope: "shared".into(),
                    },
                }),
            }
            .apply(&mut doc)
            .expect("a complete valid selection applies as one action");
            let library = PhotosDoc::library(&doc);
            assert!(library.photos.iter().all(|photo| photo.folder.is_none()));
            assert_eq!(library.photos[0].people, ["Sam"]);
            assert_eq!(library.photos[1].people, ["Noor"]);
            assert!(
                library
                    .photos
                    .iter()
                    .all(|photo| photo.tags == ["rain", "trip"])
            );
            assert!(library.photos.iter().all(|photo| {
                photo
                    .location
                    .as_ref()
                    .and_then(|value| value.label.as_deref())
                    == Some("Amsterdam")
            }));

            let before = library;
            PhotosAction::SetMetadataBatch {
                photo_ids: vec![first.id, "photos.photo:gone".into()],
                folder: MetadataPatch::Set("Must not apply".into()),
                people: MetadataLabelsPatch::Keep,
                tags: MetadataLabelsPatch::Keep,
                captured_at: MetadataPatch::Keep,
                location: MetadataPatch::Keep,
            }
            .apply(&mut doc)
            .expect_err("a stale selection refuses the complete batch");
            assert_eq!(PhotosDoc::library(&doc), before);
        }

        eprintln!("phase metadata_capture_validation_refuses_the_entire_batch");
        {
            let mut doc = AutoCommit::new();
            let first = portable_photo("/photos/date-first.png", 20, 10);
            let second = portable_photo("/photos/date-second.png", 20, 10);
            PhotosDoc::set_library(
                &mut doc,
                &crate::model::PhotoLibrary {
                    photos: vec![first.clone(), second.clone()],
                    albums: Vec::new(),
                },
            );
            let before = PhotosDoc::library(&doc);
            PhotosAction::SetMetadataBatch {
                photo_ids: vec![first.id, second.id],
                folder: MetadataPatch::Set("Should not land".into()),
                people: MetadataLabelsPatch::Keep,
                tags: MetadataLabelsPatch::Keep,
                captured_at: MetadataPatch::Set("2026-02-30T08:00Z".into()),
                location: MetadataPatch::Keep,
            }
            .apply(&mut doc)
            .expect_err("an impossible date is rejected before changing any field");
            assert_eq!(PhotosDoc::library(&doc), before);
            assert!(canonical_capture_at("2026-09-18T14:30:01.120+02:00"));
            assert!(!canonical_capture_at("2026-09-18 14:30"));
            assert!(!canonical_capture_at("2026-09-18T14:30Z"));
        }

        eprintln!("phase metadata_batch_accepts_the_full_bounded_selection");
        {
            let mut doc = AutoCommit::new();
            let photos: Vec<_> = (0..MAX_BATCH_PHOTOS)
                .map(|index| portable_photo(&format!("/photos/batch-{index}.png"), 20, 10))
                .collect();
            let ids = photos.iter().map(|photo| photo.id.clone()).collect();
            PhotosDoc::set_library(
                &mut doc,
                &crate::model::PhotoLibrary {
                    photos,
                    albums: Vec::new(),
                },
            );
            PhotosAction::SetMetadataBatch {
                photo_ids: ids,
                folder: MetadataPatch::Set("Archive".into()),
                people: MetadataLabelsPatch::Add(vec!["Sam".into()]),
                tags: MetadataLabelsPatch::Add(vec!["reviewed".into()]),
                captured_at: MetadataPatch::Keep,
                location: MetadataPatch::Keep,
            }
            .apply(&mut doc)
            .expect("the documented maximum selection applies in one action");
            assert!(PhotosDoc::library(&doc).photos.iter().all(|photo| {
                photo.folder.as_deref() == Some("Archive")
                    && photo.people == ["Sam"]
                    && photo.tags == ["reviewed"]
            }));
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn crop_arguments_parse_canonically_and_refuse_malformed_input() {
        eprintln!("phase crop_arguments_parse_and_reject");
        {
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

        eprintln!("phase published_crop_schema_accepts_canonical_wire_coordinates");
        {
            let resources = PhotosResources.publish_resources(&doc_with_image());
            let crop = published_photo(&resources)
                .actions
                .iter()
                .find(|action| action.id == "crop")
                .expect("crop is published");
            let params = crop.params();
            let schema = params[0].schema.as_ref().expect("crop publishes a schema");

            for region in [
                "0.16666666666666666,0,0.6666666666666666,1",
                "0,0,1,1",
                " \t0.25, 0, 0.75, 1\n",
            ] {
                let value = serde_json::json!(region);
                tilo_actions::validate_json_schema(&value, schema)
                    .unwrap_or_else(|error| panic!("schema rejected {region:?}: {error}"));
                let supplied = serde_json::json!({ "region": region });
                ActionArgs::bind_json(params, supplied.as_object().unwrap())
                    .unwrap_or_else(|error| panic!("binding rejected {region:?}: {error}"));
            }

            for value in [
                serde_json::json!(0.5),
                serde_json::json!("0.1,0.2,0.3"),
                serde_json::json!("0.1,wat,0.3,0.4"),
                serde_json::json!("1.1,0,0.5,1"),
                serde_json::json!("-0.1,0,0.5,1"),
            ] {
                assert!(
                    tilo_actions::validate_json_schema(&value, schema).is_err(),
                    "schema accepted invalid crop region {value}"
                );
                let supplied = serde_json::json!({ "region": value });
                assert!(
                    ActionArgs::bind_json(params, supplied.as_object().unwrap()).is_err(),
                    "binding accepted invalid crop arguments {supplied}"
                );
            }
        }

        // A prompted crop that cannot be parsed must leave the photograph alone
        // rather than crop it to nothing.
        eprintln!("phase an_unparseable_crop_argument_is_a_no_op");
        {
            let mut doc = doc_with_image();
            set_first_edits(&mut doc, Edits::default().rotated_right());
            let res = PhotosResources.publish_resources(&doc);
            let crop = published_photo(&res)
                .actions
                .iter()
                .find(|a| a.id == "crop")
                .expect("crop is published");
            let built = crop
                .build_text("nonsense")
                .expect("crop collects an argument");
            let registry = ActionRegistry::with(register);
            assert!(registry.decode(&built).is_err());
            let e = first_edits(&doc);
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

        eprintln!(
            "phase a_malformed_crop_payload_from_a_peer_is_refused_without_mutating_the_document"
        );
        {
            let mut doc = doc_with_image();
            PhotosAction::SetEdits {
                photo_id: first_id(&doc),
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
            let e = first_edits(&doc);
            assert!(
                e.is_identity(),
                "a refused action changed the document: {e:?}"
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn library_batches_are_atomic_replayable_and_refuse_stale_state() {
        eprintln!("phase library_actions_are_atomic_and_refuse_stale_membership");
        {
            let mut doc = AutoCommit::new();
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

        eprintln!("phase saving_an_album_is_atomic_replayable_and_refuses_a_stale_editor");
        {
            let mut doc = AutoCommit::new();
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
            PhotosAction::ImportPhoto {
                photo: portable_photo("/photos/one.png", 20, 10),
            }
            .apply(&mut replica)
            .unwrap();
            ActionRegistry::with(register)
                .decode(&encoded)
                .unwrap()
                .apply(&mut replica)
                .unwrap();
            assert_eq!(PhotosDoc::library(&replica).albums[0].name, "One");
        }

        eprintln!("phase favorite_batch_is_atomic_replayable_and_refuses_a_stale_selection");
        {
            let mut doc = AutoCommit::new();
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
            PhotosDoc::set_library(&mut replica, &initial);
            ActionRegistry::with(register)
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
    }

    const CROP_ARG: &str = "0.25,0.25,0.5,0.5";

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
    }

    fn apply_all(doc: &mut AutoCommit, actions: Vec<PhotosAction>) {
        for action in actions {
            action.apply(doc).expect("seed action applies");
        }
        doc.commit();
    }

    /// A small library built through the actions the app dispatches: two
    /// imported photographs, one rotated, with shared metadata, an
    /// approved person, an album, an explicit location and a shared analysis
    /// — so every kind of published document action has something to act on.
    fn contract_library() -> AutoCommit {
        let mut doc = AutoCommit::new();
        let harbour = portable_photo("/photos/harbour.png", 800, 400);
        let market = portable_photo("/photos/market.png", 640, 480);
        let market_content = market.image.content.clone().expect("portable content");
        let (harbour_id, market_id) = (harbour.id.clone(), market.id.clone());
        apply_all(
            &mut doc,
            vec![
                PhotosAction::ImportPhoto { photo: harbour },
                PhotosAction::ImportPhoto { photo: market },
                PhotosAction::set(&harbour_id, &Edits::default().rotated_right()),
                PhotosAction::SetMetadata {
                    photo_id: harbour_id.clone(),
                    folder: Some("2026 travels".into()),
                    people: Vec::new(),
                    tags: vec!["harbour".into()],
                    captured_at: Some("2026-09-07".into()),
                },
                PhotosAction::SetPersonMembership {
                    person: "Sam".into(),
                    photo_ids: vec![harbour_id.clone(), market_id.clone()],
                },
                PhotosAction::SaveAlbum {
                    id: "photos.album:summer".into(),
                    name: "Summer".into(),
                    photo_ids: vec![harbour_id],
                    create: true,
                },
                PhotosAction::SetPhotoLocation {
                    photo_id: market_id.clone(),
                    location: PhotoLocation {
                        latitude: 37.7749,
                        longitude: -122.4194,
                        label: Some("San Francisco".to_owned()),
                        provenance: crate::model::LocationProvenance {
                            source: "exif".to_owned(),
                            scope: "shared".to_owned(),
                        },
                    },
                },
                PhotosAction::SetImageAnalysis {
                    photo_id: market_id,
                    analysis: Some(shared_analysis(market_content)),
                },
            ],
        );
        doc
    }

    fn nth_photo_id(doc: &AutoCommit, n: usize) -> String {
        PhotosDoc::library(doc).photos[n].id.clone()
    }

    ui::action_contract_tests! {
        seed: contract_library(),
        registry: registry(),
        resources: |doc| PhotosResources.publish_resources(doc),
        palette: ["Rotate right", "Reset edits", "Favorite", "Remove album", "Remove person label"],
        replay: [
            set_edits => |doc| encode(&PhotosAction::SetEdits {
                photo_id: nth_photo_id(doc, 1),
                crop: Rect01 {
                    x: 0.1,
                    y: 0.2,
                    w: 0.5,
                    h: 0.5,
                },
                rotate_quarters: 3,
            }),
            set_favorites => |doc| encode(&PhotosAction::SetFavorites {
                photo_ids: vec![nth_photo_id(doc, 0), nth_photo_id(doc, 1)],
                favorite: true,
            }),
        ],
        undo: [
            set_metadata => |doc| encode(&PhotosAction::SetMetadata {
                photo_id: nth_photo_id(doc, 1),
                folder: Some("Markets".into()),
                people: vec!["Sam".into()],
                tags: vec!["market".into(), "fruit".into()],
                captured_at: None,
            }),
            rename_person => |_| encode(&PhotosAction::RenamePerson {
                from: "Sam".into(),
                to: "Samantha".into(),
            }),
        ],
        encodes: [
            rotate_right => (PHOTO_KIND, "rotate-right", |doc, res| {
                let edits = PhotosDoc::photo(doc, &res.id.0).expect("the photograph").edits;
                encode(&PhotosAction::set(&res.id.0, &edits.rotated_right()))
            }),
            crop => (PHOTO_KIND, "crop", arg: CROP_ARG, |doc, res| {
                let region = parse_crop_arg(CROP_ARG).expect("a valid crop region");
                let edits = PhotosDoc::photo(doc, &res.id.0).expect("the photograph").edits;
                encode(&PhotosAction::set(&res.id.0, &edits.cropped_from_view(region)))
            }),
            favorite => ("photos.photo", "favorite", |_, res| {
                encode(&PhotosAction::SetFavorite {
                    photo_id: res.id.0.clone(),
                    favorite: true,
                })
            }),
            remove_photo => ("photos.photo", "remove-photo", |_, res| {
                encode(&PhotosAction::RemovePhoto {
                    photo_id: res.id.0.clone(),
                })
            }),
            remove_album => ("photos.album:summer", "remove-album", |_, _| {
                encode(&PhotosAction::RemoveAlbum {
                    id: "photos.album:summer".into(),
                })
            }),
        ],
    }
}
