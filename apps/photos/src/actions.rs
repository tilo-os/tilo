//! What Photos does that is not an edit of its library: what a window
//! shows, getting photographs in and out, and local analysis whose results
//! arrive as the library's ordinary edits and verbs.
//!
//! Zoom, pan and which region the viewport shows are local presentation
//! and never leave the machine. Copying to the clipboard is not a change of
//! the library either: it is an affordance of the window the person is
//! touching. Everything is named by id: a photograph by its content, an
//! album by the id it was made with.

// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource;
use ui::tilo_resource::Effect;

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
    #[effect(view, item = photos)]
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
    /// Read people
    ///
    /// Answers who is named in the library and on how many photographs
    /// each, and whether this device recognises faces. Suggestions this
    /// device has not had approved stay on it. It is the People place's
    /// read: what discloses it, so recognition, a scan and each person's
    /// verbs are offered to whoever may read it, an agent too.
    #[effect(view, row, returns, idempotent, label = "Read people", tag = tilo_actions::DOCUMENT_READ_TAG)]
    ReadPeople,
    /// Read person
    ///
    /// Answers the person's name and the photographs they are named on. It
    /// is the person's read: what discloses them, so renaming them, naming
    /// them on a photograph and taking them off every one are offered to
    /// whoever may read it, an agent too.
    #[effect(view, row, returns, idempotent, label = "Read person", tag = tilo_actions::DOCUMENT_READ_TAG)]
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
    #[effect(view, row, returns, idempotent, label = "Read photo analysis", tag = tilo_actions::DOCUMENT_READ_TAG)]
    ReadLibraryAnalysis,
    /// Analyze unprocessed photographs on this device. Pixels stay local;
    /// each bounded result is shared with `SetImageAnalysis`.
    #[effect(row, job, cancellable, sensitive)]
    AnalyzeLibrary,
    /// Pause photo analysis after the photograph in progress.
    #[effect(row, returns)]
    PauseLibraryAnalysis,
    /// Resume photo analysis without widening the queue.
    #[effect(row, job, cancellable, sensitive)]
    ResumeLibraryAnalysis,
    /// Retry failed photographs from the bounded failure report.
    #[effect(row, job, cancellable, sensitive)]
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
    #[effect(view)]
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
    #[effect(job, cancellable)]
    Import {
        #[param("Absolute filesystem path; omit to use the system file picker")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Import photo folder: recursively index a folder with bounded
    /// real-file reads. Paths never enter collaboration and EXIF GPS is never
    /// uploaded.
    #[effect(job, cancellable, tag = tilo_actions::REQUIRES_NATIVE_PATHS_TAG)]
    ImportDirectory {
        #[param("Absolute authorised folder path; omit to use the system folder picker")]
        path: Option<ui::tilo_resource::AbsolutePath>,
        #[param("Retain GPS read from imported files; defaults to true")]
        retain_exif_location: Option<bool>,
    },
    /// Import Camera media: a verified Camera photograph whose bytes are on
    /// this device.
    #[effect(returns)]
    ImportCameraMedia { media_id: String, name: String },
    /// Reindex photo folder: refresh metadata re-read from a folder's files
    /// without overwriting deliberate organization.
    #[effect(job, cancellable, tag = tilo_actions::REQUIRES_NATIVE_PATHS_TAG)]
    ReindexDirectory {
        #[param("Absolute authorised local folder path")]
        path: ui::tilo_resource::AbsolutePath,
        #[param("Retain GPS read from imported files; defaults to true")]
        retain_exif_location: Option<bool>,
    },
    /// Copy photograph: the photograph this window shows, as edited.
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
        photos: Vec<String>,
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
    ShareShown,
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
    TogglePeopleRecognition,
    /// Stop the People scan after the batch in progress.
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
    RejectSuggestion,
    /// Hide the suggestion reviewed on this device.
    HideSuggestion,
    /// Restore the hidden suggestion reviewed.
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
    #[effect(destructive)]
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
    photo_id: &str,
    library: &crate::model::PhotoLibrary,
    store: &tilo_content::ContentStore,
) -> Result<tilo_actions::ActionOutput, String> {
    let photo = library
        .photo(photo_id)
        .ok_or_else(|| "that photograph is no longer in the library".to_string())?;
    let source_content = &photo.image.content;
    let edits = photo.edits;
    let source = store
        .read(source_content)
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

/// Make thumbnails of `photos` (content hashes or `tilo.content:`
/// addresses) from `store`: the local run of the offloadable
/// `make-thumbnails`, byte for byte what the thumbnails package's
/// `thumbnail-batch` entrypoint makes on another device. As there, content
/// named twice is one input, and a picture that is not an image makes no
/// thumbnail of its own while the rest still do. Its failure is listed in
/// the answer by the photograph the caller named, so the person can see
/// which picture to look at.
///
/// # Errors
///
/// Names content that is not a photograph's content, or a store failure.
pub fn make_thumbnails(
    photos: &[String],
    store: &tilo_content::ContentStore,
) -> Result<tilo_actions::ActionOutput, String> {
    // Each distinct content once, in the order the caller first named it
    // (the entrypoint's order, so the offloaded run answers the same list),
    // with that first name: what a failure is reported against.
    let mut distinct_hashes = std::collections::BTreeSet::new();
    let mut seen: Vec<(&str, &str)> = Vec::with_capacity(photos.len());
    for photo in photos {
        let hash = tilo_actions::content_hash(photo).unwrap_or(photo);
        if !crate::model::is_content_hash(hash) {
            return Err(format!("`{photo}` is not a photograph's content"));
        }
        if distinct_hashes.insert(hash) {
            seen.push((hash, photo));
        }
    }
    let mut inputs = Vec::with_capacity(seen.len());
    let mut missing = Vec::new();
    for (hash, photo) in &seen {
        match store.read(hash) {
            Some(bytes) => inputs.push((*photo, bytes)),
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
                tracing::warn!(photo, %error, "photos: a picture made no thumbnail");
                failed.push(json_failure(photo, "is not an image"));
            }
        }
    }
    Ok(tilo_actions::ActionOutput {
        value: (!failed.is_empty()).then(|| serde_json::json!({ "failed": failed })),
        resources,
    })
}

/// One picture that made no thumbnail, named as the caller named it.
fn json_failure(photo: &str, reason: &str) -> serde_json::Value {
    serde_json::json!({"photo": photo, "reason": reason})
}

/// The resource `kind` a library photograph publishes under: an item of
/// the library's `photos`.
pub const PHOTO_KIND: &str = "photos.library.photos";

/// The content a photograph's id names. A photograph is addressed by what it
/// is, not where a machine put it, so a session that never had a file for it
/// goes from the address to the bytes through its content store.
pub fn content_of(id: &str) -> Option<&str> {
    let tail = id.strip_prefix("photos.photo:")?;
    crate::model::is_content_hash(tail).then_some(tail)
}
