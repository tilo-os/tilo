//! What Photos does that is not an edit of its library: what a window shows, getting photographs in
//! and out, and local analysis whose results arrive as the library's ordinary edits and verbs.

use ui::tilo_resource;
use ui::tilo_resource::{Bytes, Effect};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TimelineAnchor {
    Month { month: String },
    Day { day: String },
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "photos.effect")]
pub enum PhotosEffect {
    /// Open photograph: show it in this window. Which photograph a window
    /// shows is its own view state; opening one here changes nothing for a
    /// collaborator.
    #[effect(view, item = photos)]
    #[effect(label = "photos-a11y-open-photo")]
    View {
        #[param("Id of the photograph")]
        photo_id: String,
    },
    /// View photo library: browse and manage photographs in Photos.
    #[effect(view)]
    #[effect(label = "photos-search-library")]
    ViewLibrary,
    /// Open the chronological timeline, optionally at one canonical calendar
    /// boundary. Capture metadata remains untouched.
    #[effect(view, row)]
    #[effect(label = "photos-timeline")]
    ViewTimeline {
        #[param("A month or day of the timeline; omit for its start")]
        anchor: Option<TimelineAnchor>,
    },
    /// Open People: the People collection and this device's review.
    #[effect(view, row)]
    #[effect(label = "photos-people")]
    ViewPeople,
    /// Read people
    ///
    /// Answers who is named in the library and on how many photographs
    /// each, and whether this device recognises faces. Suggestions this
    /// device has not had approved stay on it. It is the People place's
    /// read: what discloses it, so recognition, a scan and each person's
    /// verbs are offered to whoever may read it, an agent too.
    #[effect(view, row, returns, idempotent, tag = tilo_actions::DOCUMENT_READ_TAG)]
    ReadPeople,
    /// Read person
    ///
    /// Answers the person's name and the photographs they are named on. It
    /// is the person's read: what discloses them, so renaming them, naming
    /// them on a photograph and taking them off every one are offered to
    /// whoever may read it, an agent too.
    #[effect(view, row, returns, idempotent, tag = tilo_actions::DOCUMENT_READ_TAG)]
    ReadPerson {
        #[param("The person's name")]
        person: String,
    },
    /// Manage local recognition model: the signed face model, in Settings.
    #[effect(view, row)]
    ManagePeopleRecognition,
    /// Enable private People recognition on this device. Changes no shared
    /// metadata and begins no scan.
    #[effect(row)]
    EnablePeopleRecognition,
    /// Scan local library for faces: the next bounded local batch, with the
    /// separately signed face model. Suggestions stay on this device until
    /// approved.
    #[effect(row, job, cancellable)]
    AnalyzePeopleLibrary,
    /// Open photo analysis: local model availability, library coverage,
    /// progress and failures.
    #[effect(view, row)]
    #[effect(label = "photos-analysis-label")]
    ViewLibraryAnalysis,
    /// Read photo analysis
    ///
    /// Answers where this device's analysis of the library stands: whether
    /// it is running, paused or complete, with which model, how many
    /// photographs it has done, how many succeeded, failed or could not be
    /// analysed, how many remain, and the failures it reported. It is the
    /// analysis place's read: what discloses it, so starting, pausing,
    /// resuming and retrying it are offered to whoever may read it, an
    /// agent too.
    #[effect(view, row, returns, idempotent, tag = tilo_actions::DOCUMENT_READ_TAG)]
    ReadLibraryAnalysis,
    /// Analyze unprocessed photographs on this device. Pixels stay local;
    /// each bounded result is shared with `SetImageAnalysis`.
    #[effect(row, job, cancellable, sensitive)]
    #[effect(label = "photos-analysis-start")]
    AnalyzeLibrary,
    /// Pause photo analysis after the photograph in progress.
    #[effect(row, returns)]
    PauseLibraryAnalysis,
    /// Resume photo analysis without widening the queue.
    #[effect(row, job, cancellable, sensitive)]
    ResumeLibraryAnalysis,
    /// Retry failed photographs from the bounded failure report.
    #[effect(row, job, cancellable, sensitive)]
    #[effect(label = "photos-analysis-retry")]
    RetryLibraryAnalysisFailures,
    /// View favorites: photographs marked as favorites in the shared library.
    #[effect(view, row)]
    #[effect(label = "photos-favorites")]
    ViewFavorites,
    /// View albums: the shared library's durable albums.
    #[effect(view, row)]
    #[effect(label = "photos-albums")]
    ViewAlbums,
    /// View places: explicitly shared locations on an offline-first map.
    #[effect(view, row)]
    #[effect(label = "photos-places")]
    ViewPlaces,
    /// View place: one shared place cluster, without sending its location
    /// anywhere.
    #[effect(view, row)]
    ViewPlace {
        place_key: String,
    },
    /// View album.
    #[effect(view)]
    ViewAlbum {
        album_id: String,
    },
    /// View folder: the photographs filed under this folder label.
    #[effect(view, row)]
    ViewFolder {
        folder: String,
    },
    /// View person: the photographs carrying this person label.
    #[effect(view, row)]
    ViewPerson {
        person: String,
    },
    /// View tag: the photographs carrying this tag.
    #[effect(view, row)]
    ViewTag {
        tag: String,
    },
    /// View object: the photographs analysis shared this object for.
    #[effect(view, row)]
    ViewObject {
        object: String,
    },
    /// View pet: the photographs analysis shared this pet for.
    #[effect(view, row)]
    ViewPet {
        pet: String,
    },
    /// Import photograph into the library and show it.
    #[effect(job, cancellable)]
    Import {
        #[param("Absolute filesystem path; omit to use the system file picker")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Import photo folder: recursively index a folder with bounded
    /// real-file reads. Paths never enter collaboration and EXIF GPS is never
    /// uploaded.
    #[effect(job, cancellable, tag = tilo_actions::REQUIRES_NATIVE_PATHS_TAG)]
    #[effect(label = "photos-import-pick-accept")]
    ImportDirectory {
        #[param("Absolute authorised folder path; omit to use the system folder picker")]
        path: Option<ui::tilo_resource::AbsolutePath>,
        #[param("Retain GPS read from imported files; defaults to true")]
        retain_exif_location: Option<bool>,
    },
    /// Import Camera media: a verified Camera photograph whose bytes are on
    /// this device.
    #[effect(returns)]
    ImportCameraMedia {
        media_id: String,
        name: String,
    },
    /// Reindex photo folder: refresh metadata re-read from a folder's files
    /// without overwriting deliberate organization.
    #[effect(job, cancellable, tag = tilo_actions::REQUIRES_NATIVE_PATHS_TAG)]
    ReindexDirectory {
        #[param("Absolute authorised local folder path")]
        path: ui::tilo_resource::AbsolutePath,
        #[param("Retain GPS read from imported files; defaults to true")]
        retain_exif_location: Option<bool>,
    },
    Copy,
    /// Produce edited image content: render the photograph's crop and
    /// rotation, as they are now, into immutable PNG content.
    #[effect(content, idempotent, kind = "read", item = photos)]
    ExportEditedContent {
        #[param("The photograph")]
        photo: ui::tilo_resource::Id,
    },
    /// Make thumbnails: a small PNG of each photograph, in order. The same
    /// bytes whichever of your devices makes them, so the system may run it
    /// on a better-placed one.
    #[effect(
        content,
        idempotent,
        kind = "read",
        offload(
            app = "so.tilo.thumbnails",
            entrypoint = "thumbnail-batch",
            cpu_ms = 120000,
            memory_mb = 1024,
            output_mb = 16
        )
    )]
    MakeThumbnails {
        #[param(content, "The photographs' content, in order")]
        photos: Vec<Bytes>,
    },
    /// Save edited photograph: the photograph this window shows.
    #[effect(job)]
    Save {
        #[param("Absolute filesystem path; omit to ask where with the Save chooser")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Recognize text locally and share only bounded text and provenance.
    #[effect(job, cancellable, sensitive)]
    AnalyzeText {
        resource_id: String,
        language: String,
    },
    /// Analyze image locally with the signed local model; share only the
    /// bounded result.
    #[effect(job, cancellable, sensitive)]
    AnalyzeImage {
        resource_id: String,
    },
    /// Rotate left: turn the photograph this window shows a quarter turn
    /// anticlockwise.
    #[effect(label = "photos-command-rotate-left")]
    RotateShownLeft,
    /// Rotate right: turn the photograph this window shows a quarter turn
    /// clockwise.
    #[effect(label = "photos-command-rotate-right")]
    RotateShownRight,
    /// Crop to view: keep what this window frames of its photograph.
    #[effect(label = "photos-command-crop")]
    CropShownToView,
    ResetShownEdits,
    ShareShown,
    /// New album: start naming one and choosing its photographs.
    #[effect(view)]
    #[effect(label = "photos-new-album")]
    NewAlbum,
    /// Edit album: the album this window browses.
    #[effect(view)]
    #[effect(label = "photos-edit-album")]
    EditAlbum,
    /// Previous page of the library shown.
    #[effect(view)]
    PreviousLibraryPage,
    /// Next page of the library shown.
    #[effect(view)]
    NextLibraryPage,
    /// Save album: the name and photographs being edited.
    #[effect(label = "photos-a11y-save-album")]
    SaveAlbumDraft,
    /// Cancel album editing.
    #[effect(view)]
    #[effect(label = "photos-a11y-cancel-album")]
    CancelAlbumDraft,
    /// Delete album: ask to confirm deleting the album being edited.
    #[effect(view)]
    #[effect(label = "photos-delete-album")]
    DeleteAlbumDraft,
    /// Confirm deleting the album being edited. Its photographs stay.
    #[effect(destructive)]
    ConfirmAlbumDelete,
    SaveDetails,
    /// Cancel editing photograph details.
    #[effect(view)]
    CancelDetails,
    /// Organize: the folder, people and tags of the details being edited.
    #[effect(view)]
    #[effect(label = "photos-organize")]
    ShowDetailsOrganize,
    /// Time & place of the details being edited.
    #[effect(view)]
    #[effect(label = "photos-time-and-place")]
    ShowDetailsTimeAndPlace,
    /// Clear folder of the details being edited.
    #[effect(view)]
    #[effect(label = "photos-clear-folder")]
    ClearDraftFolder,
    /// Clear people of the details being edited.
    #[effect(view)]
    #[effect(label = "photos-clear-people")]
    ClearDraftPeople,
    /// Clear tags of the details being edited.
    #[effect(view)]
    #[effect(label = "photos-clear-tags")]
    ClearDraftTags,
    /// Clear capture time of the details being edited.
    #[effect(view)]
    #[effect(label = "photos-clear-captured")]
    ClearDraftCapturedAt,
    /// Clear location of the details being edited.
    #[effect(view)]
    #[effect(label = "photos-clear-location")]
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
    TogglePeopleRecognition,
    CancelPeopleScan,
    /// View photos of an approved person.
    #[effect(view)]
    OpenApprovedPerson {
        person: String,
    },
    /// Review a private suggestion.
    #[effect(view)]
    OpenSuggestion {
        cluster: String,
    },
    /// Browse approved people.
    #[effect(view)]
    BrowseApprovedPeople,
    /// Save the name typed for the person reviewed.
    #[effect(label = "photos-people-save-name")]
    SavePersonName,
    /// Merge: choose a person to merge the one shown into.
    #[effect(view)]
    BeginPeopleMerge,
    MergeApprovedInto {
        person: String,
    },
    ApproveSuggestion,
    RejectSuggestion,
    HideSuggestion,
    RestoreSuggestion,
    MergeSuggestionWith {
        person: String,
    },
    /// Show or hide hidden suggestions.
    #[effect(view)]
    ToggleHiddenSuggestions,
    /// Another page of People suggestions.
    #[effect(view)]
    PeoplePage {
        forward: bool,
    },
    /// Delete local People data: ask to confirm.
    #[effect(view)]
    DeleteLocalPeopleData,
    /// Confirm deleting this device's private People data.
    #[effect(destructive)]
    ConfirmDeleteLocalPeopleData,
}

pub fn export_edited_content(
    photo_id: &str,
    library: &crate::model::PhotoLibrary,
    store: &tilo_content::ContentStore,
) -> Result<tilo_actions::ActionOutput, String> {
    let photo = library
        .photo(photo_id)
        .ok_or_else(|| "that photograph is no longer in the library".to_string())?;
    let edits = photo.edits;
    let source = photo
        .image
        .content
        .read(store)
        .ok_or_else(|| "the addressed photograph bytes are unavailable".to_string())?;
    let original = crate::pixels::decode(&source)
        .ok_or_else(|| "the addressed photograph bytes are not a supported image".to_string())?;
    let rendered = crate::pixels::render(&original, edits.crop, edits.rotate_quarters);
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

pub fn make_thumbnails(
    photos: &[Bytes],
    store: &tilo_content::ContentStore,
) -> Result<tilo_actions::ActionOutput, String> {
    let mut distinct = std::collections::BTreeSet::new();
    let mut named: Vec<(String, &Bytes)> = Vec::with_capacity(photos.len());
    for photo in photos {
        let hash = photo.hash();
        if distinct.insert(hash) {
            named.push((crate::model::content_hex(&hash), photo));
        }
    }
    let mut inputs = Vec::with_capacity(named.len());
    let mut missing = Vec::new();
    for (photo, content) in &named {
        match content.read(store) {
            Some(bytes) => inputs.push((photo.as_str(), bytes)),
            None => missing.push(json_failure(photo, "is not on this device")),
        }
    }
    let mut resources = Vec::with_capacity(inputs.len());
    let mut failed = missing;
    let distinct: Vec<Vec<u8>> = inputs.iter().map(|(_, bytes)| bytes.clone()).collect();
    for ((photo, _), outcome) in inputs
        .iter()
        .zip(thumbnail::batch::thumbnail_batch(&distinct))
    {
        match outcome {
            Ok(thumbnail) => {
                let hash = store.put(&thumbnail).map_err(|error| {
                    tracing::warn!(%error, "photos: could not store a thumbnail");
                    "a thumbnail could not be stored".to_string()
                })?;
                resources.push(tilo_actions::content_resource(&hash).ok_or_else(|| {
                    "the content provider returned an invalid address".to_string()
                })?);
            }
            Err(error) => {
                tracing::warn!(%photo, %error, "photos: a picture made no thumbnail");
                failed.push(json_failure(photo, "is not an image"));
            }
        }
    }
    Ok(tilo_actions::ActionOutput {
        value: (!failed.is_empty()).then(|| serde_json::json!({ "failed": failed })),
        resources,
    })
}

fn json_failure(photo: &str, reason: &str) -> serde_json::Value {
    serde_json::json!({"photo": photo, "reason": reason})
}

/// The resource `kind` a library photograph publishes under: an item of the library's `photos`.
pub const PHOTO_KIND: &str = "photos.library.photos";

pub fn content_of(id: &str) -> Option<&str> {
    let tail = id.strip_prefix("photos.photo:")?;
    crate::model::is_content_hash(tail).then_some(tail)
}
