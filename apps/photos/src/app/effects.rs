//! Answering effects: what a control, the palette, an agent or a script asks
//! of Photos, how its controls bind to actions, and how long-running
//! invocations settle.

use crate::actions::PhotosEffect;
use crate::model::{PhotoLibrary, Rect01};
use crate::strings::tr;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::tilo_resource::effect::Answers;
use ui::tilo_resource::{Effect as _, Id};

use super::directory_import::build_directory_import_buttons;
use super::editors::build_metadata_editor_buttons;
use super::navigation::build_collection_page_buttons;
use super::{
    AlbumEditorHit, AnalysisJob, AnalysisJobKind, CollectionPageHit, DirectoryImportMode,
    DirectoryStatusHit, Hit, LibraryAnalysisHit, LibraryDestination, LiveContent,
    MetadataEditorHit, PeopleIntent, Photos, build_library_analysis_buttons,
};

impl Photos {
    /// Build the retained controls again, bound to their effects.
    pub(super) fn bind_controls(&mut self) {
        let actions = self.actions.as_ref();
        self.directory_import_buttons = build_directory_import_buttons(actions);
        self.library_analysis_buttons = build_library_analysis_buttons(actions);
        self.collection_page_buttons = build_collection_page_buttons(actions);
        self.metadata_editor_buttons = build_metadata_editor_buttons(actions);
        // People controls are rebuilt on their next refresh.
        self.people_control_signature.clear();
    }

    /// Answer the effect of a retained control: post its intent for the
    /// next sync, which has the document. `false` when `effect` is not one.
    fn post_control(&self, effect: &PhotosEffect) -> bool {
        let Some(control) = Control::of(effect) else {
            return false;
        };
        match control {
            Control::Toolbar(hit) => self.pending.set(Some(hit)),
            Control::Album(hit) => self.album_editor_pending.set(Some(hit)),
            Control::Details(hit) => self.metadata_editor_pending.set(Some(hit)),
            Control::Collection(hit) => self.collection_page_pending.set(Some(hit)),
            Control::Directory(hit) => self.directory_import_pending.set(Some(hit)),
            Control::People(intent) => *self.people_pending.borrow_mut() = Some(intent),
        }
        true
    }

    /// What shows the photograph or album a search hit or an agent's `open`
    /// names, for [`ViewCx::on_open_item`](ui::framework::ViewCx::on_open_item).
    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&Id) -> Result<(), ui::tilo_resource::Refusal> + 'static {
        let opens = Rc::clone(&self.opens);
        move |id| {
            if !id.as_str().starts_with("photos.photo:")
                && !id.as_str().starts_with("photos.album:")
            {
                return Err(ui::tilo_resource::Refusal::new(
                    "only a photograph or an album opens",
                ));
            }
            opens.borrow_mut().push(id.clone());
            Ok(())
        }
    }

    /// Show what was asked to be opened since the last sync.
    pub(super) fn drain_opens(&mut self) {
        let opens = std::mem::take(&mut *self.opens.borrow_mut());
        for id in opens {
            let library = self.presented();
            let effect = if id.as_str().starts_with("photos.album:") {
                PhotosEffect::ViewAlbum {
                    album_id: id.as_str().to_owned(),
                }
            } else {
                PhotosEffect::View {
                    photo_id: id.as_str().to_owned(),
                }
            };
            if let Err(error) = self.perform_effect(effect, &library) {
                self.notice = Some(error);
            }
        }
    }

    /// Perform a device-local operation and surface failures to canonical
    /// operation dispatch. A local notice alone is not an invocation result.
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match over device-local Photos operations keeps their effects auditable"
    )]
    fn perform_effect(
        &mut self,
        operation: PhotosEffect,
        library: &crate::model::PhotoLibrary,
    ) -> Result<(), String> {
        match operation {
            // Resource/search routing has already brought this Photos
            // instance forward. Viewing does not alter document or OS state.
            PhotosEffect::Import { path: None } => {
                self.import_content = Some(LiveContent {
                    library: library.clone(),
                });
                self.picker_stale = false;
                let filter = ui::portal::FileFilter::new(&tr("photos-filter-images"))
                    .mimetype("image/png")
                    .mimetype("image/jpeg")
                    .mimetype("image/webp");
                if tilo_files::available(tilo_files::Feature::NativePaths) {
                    self.picker = Some(
                        ui::portal::PickRequest::open(&tr("photos-open-title"), &tr("photos-open"))
                            .thread_name("photos-portal-open")
                            .filter(filter)
                            .all_files()
                            .send(),
                    );
                } else {
                    self.contents_picker = Some(
                        ui::portal::PickRequest::open_multiple(
                            &tr("photos-open-title"),
                            &tr("photos-open"),
                        )
                        .filter(filter)
                        .send_contents(),
                    );
                }
            }
            PhotosEffect::ImportDirectory {
                path: None,
                retain_exif_location,
            } => {
                self.directory_picker_retain_exif_location = retain_exif_location.unwrap_or(true);
                self.directory_picker = Some(
                    ui::portal::PickRequest::directory(
                        &tr("photos-import-pick-title"),
                        &tr("photos-import-pick-accept"),
                    )
                    .thread_name("photos-portal-directory-import")
                    .send(),
                );
            }
            PhotosEffect::ImportDirectory {
                path: Some(path),
                retain_exif_location,
            } => {
                self.begin_directory_import(
                    path.into_path_buf(),
                    None,
                    DirectoryImportMode::Import,
                    retain_exif_location.unwrap_or(true),
                );
            }
            PhotosEffect::ImportCameraMedia { media_id, name } => {
                return Err(format!(
                    "Camera import requires document dispatch: {media_id} ({name})"
                ));
            }
            PhotosEffect::ReindexDirectory {
                path,
                retain_exif_location,
            } => {
                self.begin_directory_import(
                    path.into_path_buf(),
                    None,
                    DirectoryImportMode::Reindex,
                    retain_exif_location.unwrap_or(true),
                );
            }
            PhotosEffect::AnalyzeText { .. }
            | PhotosEffect::AnalyzeImage { .. }
            | PhotosEffect::AnalyzePeopleLibrary
            | PhotosEffect::AnalyzeLibrary
            | PhotosEffect::PauseLibraryAnalysis
            | PhotosEffect::ResumeLibraryAnalysis
            | PhotosEffect::RetryLibraryAnalysisFailures => {
                return Err("local image analysis requires document dispatch".to_owned());
            }
            PhotosEffect::ReadPeople
            | PhotosEffect::ReadPerson { .. }
            | PhotosEffect::ReadLibraryAnalysis => {
                return Err("a read answers through effect dispatch".to_owned());
            }
            PhotosEffect::Copy => {
                self.copy_to_clipboard();
            }
            PhotosEffect::Save { path } => {
                if let Some(open) = self.shown_photo_in(library) {
                    match path {
                        Some(path) => {
                            self.save_to(&path);
                        }
                        None => {
                            self.choose_save_destination(&open.image, None);
                        }
                    }
                }
            }
            PhotosEffect::ExportEditedContent { .. } => {
                return Err("edited content export requires lifecycle dispatch".into());
            }
            PhotosEffect::MakeThumbnails { .. } => {
                return Err("making thumbnails requires lifecycle dispatch".into());
            }
            PhotosEffect::View { photo_id } => {
                self.view_photo(library, &photo_id)?;
            }
            PhotosEffect::ViewLibrary => self.show_library(),
            PhotosEffect::ViewAlbums => self.show_albums(),
            PhotosEffect::ViewTimeline { anchor } => {
                self.view_timeline(library, anchor.as_ref())?;
            }
            PhotosEffect::ViewPeople => {
                self.show_library();
                self.people_mode = true;
                self.library_destination = LibraryDestination::People;
            }
            PhotosEffect::ViewLibraryAnalysis => {
                self.show_library_analysis();
            }
            PhotosEffect::ManagePeopleRecognition => {
                self.notice = Some(
                    "Open Models in Settings to install and start a signed local face-recognition model"
                        .to_owned(),
                );
            }
            PhotosEffect::EnablePeopleRecognition => {
                self.people_controller.set_recognition_enabled(true);
                self.people_controller
                    .discover_face_capability(&agent_provider_config::ProviderConfig::load());
                self.notice = Some(self.people_controller.capability().summary());
            }
            PhotosEffect::ViewFavorites => self.view_favorites(library)?,
            PhotosEffect::ViewPlaces => {
                self.show_library();
                self.library_destination = LibraryDestination::Places;
                self.places_open_key = None;
                self.places_fit_pending = true;
            }
            PhotosEffect::ViewPlace { place_key } => {
                self.open_place_collection(library, &place_key)?;
            }
            // Views with a library to check, a direct import and the
            // retained controls' effects are answered before this.
            _ => {}
        }
        Ok(())
    }

    /// Drain a click that a button posted, and act on it.
    pub(super) fn drain_pending(&mut self) {
        let Some(hit) = self.pending.take() else {
            return;
        };
        let edits = self
            .shown_photo()
            .map(|photo| photo.edits)
            .unwrap_or_default();
        match hit {
            Hit::Open => {
                let _ = self.perform_effect(PhotosEffect::Import { path: None }, &self.presented());
            }
            Hit::Library if self.library_mode && self.people_selection.is_some() => {
                self.handle_people_intent(PeopleIntent::Back);
            }
            Hit::Library => {
                let _ = self.perform_effect(PhotosEffect::ViewLibrary, &self.presented());
            }
            Hit::RotateLeft => self.edit_shown(|photo| PhotoLibrary::ROTATE_LEFT.with(photo)),
            Hit::RotateRight => self.edit_shown(|photo| PhotoLibrary::ROTATE_RIGHT.with(photo)),
            Hit::CropToView => {
                let view = self.visible_rect();
                // Cropping to the whole image is a no-op; say so rather than
                // writing an edit that changes nothing.
                if view.is_full() {
                    self.notice = Some(tr("photos-crop-noop"));
                } else {
                    self.edit_shown(|photo| PhotoLibrary::CROP.with((photo, view)));
                }
            }
            Hit::Reset if !edits.is_identity() => self.edit_shown(|photo| {
                let paths = crate::model::photo_paths(photo.as_str()).edits;
                ui::tilo_resource::Invoke::group(
                    [paths.crop.set(Rect01::FULL), paths.rotate_quarters.set(0)]
                        .iter()
                        .filter_map(|edit| ui::tilo_resource::Invoke::edit(edit).ok()),
                )
            }),
            Hit::Reset => {}
            Hit::Copy => {
                let _ = self.perform_effect(PhotosEffect::Copy, &self.presented());
            }
            Hit::Share => self.share_open_photo(),
            Hit::Save => {
                let _ = self.perform_effect(PhotosEffect::Save { path: None }, &self.presented());
            }
            Hit::Select => {
                if self.selection_mode {
                    self.finish_selection();
                } else {
                    self.begin_selection();
                }
            }
            Hit::PreviousPage => self.page_library(false),
            Hit::NextPage => self.page_library(true),
            Hit::NewAlbum => self.begin_new_album(),
            Hit::EditAlbum => self.begin_edit_album(),
            Hit::Smaller => {
                self.adjust_timeline_density(true);
            }
            Hit::Larger => {
                self.adjust_timeline_density(false);
            }
            Hit::PeopleHidden => self.handle_people_intent(PeopleIntent::ToggleHidden),
            Hit::PlacesList => self.places_list_mode = !self.places_list_mode,
            Hit::PlacesZoomIn => self.zoom_places_by(1.0),
            Hit::PlacesZoomOut => self.zoom_places_by(-1.0),
            Hit::ZoomIn => {
                self.zoom_photo(ui::content_zoom::ZoomStep::In);
            }
            Hit::ZoomOut => {
                self.zoom_photo(ui::content_zoom::ZoomStep::Out);
            }
            Hit::ZoomReset => {
                self.zoom_photo(ui::content_zoom::ZoomStep::Reset);
            }
            Hit::Present => self.toggle_presentation(),
            Hit::SlideShow => self.toggle_slide_show(),
            Hit::Selection(hit) => self.handle_selection_hit(hit),
        }
    }
}

impl Hit {
    pub(super) fn effect(self) -> Option<PhotosEffect> {
        Some(match self {
            Self::Open => PhotosEffect::Import { path: None },
            Self::Library => PhotosEffect::ViewLibrary,
            Self::RotateLeft => PhotosEffect::RotateShownLeft,
            Self::RotateRight => PhotosEffect::RotateShownRight,
            Self::CropToView => PhotosEffect::CropShownToView,
            Self::Reset => PhotosEffect::ResetShownEdits,
            Self::Copy => PhotosEffect::Copy,
            Self::Share => PhotosEffect::ShareShown,
            Self::Save => PhotosEffect::Save { path: None },
            Self::PreviousPage => PhotosEffect::PreviousLibraryPage,
            Self::NextPage => PhotosEffect::NextLibraryPage,
            Self::NewAlbum => PhotosEffect::NewAlbum,
            Self::EditAlbum => PhotosEffect::EditAlbum,
            Self::Smaller => PhotosEffect::ShowFewerPhotos,
            Self::Larger => PhotosEffect::ShowMorePhotos,
            Self::PeopleHidden => PhotosEffect::ToggleHiddenSuggestions,
            _ => return None,
        })
    }
}

impl LibraryAnalysisHit {
    pub(super) fn effect(self) -> PhotosEffect {
        match self {
            Self::Start => PhotosEffect::AnalyzeLibrary,
            Self::Pause => PhotosEffect::PauseLibraryAnalysis,
            Self::Resume => PhotosEffect::ResumeLibraryAnalysis,
            Self::RetryFailures => PhotosEffect::RetryLibraryAnalysisFailures,
        }
    }
}

/// The intent of the retained control `effect` stands for, when it is one.
enum Control {
    Toolbar(Hit),
    Album(AlbumEditorHit),
    Details(MetadataEditorHit),
    Collection(CollectionPageHit),
    Directory(DirectoryStatusHit),
    People(PeopleIntent),
}

impl Control {
    fn of(effect: &PhotosEffect) -> Option<Self> {
        use PhotosEffect as E;
        Some(match effect.clone() {
            E::RotateShownLeft => Self::Toolbar(Hit::RotateLeft),
            E::RotateShownRight => Self::Toolbar(Hit::RotateRight),
            E::CropShownToView => Self::Toolbar(Hit::CropToView),
            E::ResetShownEdits => Self::Toolbar(Hit::Reset),
            E::ShareShown => Self::Toolbar(Hit::Share),
            E::NewAlbum => Self::Album(AlbumEditorHit::New),
            E::EditAlbum => Self::Album(AlbumEditorHit::Edit),
            E::PreviousLibraryPage => Self::Album(AlbumEditorHit::PreviousPage),
            E::NextLibraryPage => Self::Album(AlbumEditorHit::NextPage),
            E::SaveAlbumDraft => Self::Album(AlbumEditorHit::Save),
            E::CancelAlbumDraft => Self::Album(AlbumEditorHit::Cancel),
            E::DeleteAlbumDraft => Self::Album(AlbumEditorHit::Delete),
            E::ConfirmAlbumDelete => Self::Album(AlbumEditorHit::ConfirmDelete),
            E::SaveDetails => Self::Details(MetadataEditorHit::Save),
            E::CancelDetails => Self::Details(MetadataEditorHit::Cancel),
            E::ShowDetailsOrganize => Self::Details(MetadataEditorHit::Organize),
            E::ShowDetailsTimeAndPlace => Self::Details(MetadataEditorHit::TimeAndPlace),
            E::ClearDraftFolder => Self::Details(MetadataEditorHit::ClearFolder),
            E::ClearDraftPeople => Self::Details(MetadataEditorHit::ClearPeople),
            E::ClearDraftTags => Self::Details(MetadataEditorHit::ClearTags),
            E::ClearDraftCapturedAt => Self::Details(MetadataEditorHit::ClearCapturedAt),
            E::ClearDraftLocation => Self::Details(MetadataEditorHit::ClearLocation),
            E::PlaceDraftOnMap => Self::Details(MetadataEditorHit::PlaceOnMap),
            E::ShowFewerPhotos => Self::Toolbar(Hit::Smaller),
            E::ShowMorePhotos => Self::Toolbar(Hit::Larger),
            E::PreviousAlbumsPage => Self::Collection(CollectionPageHit::AlbumsPrevious),
            E::NextAlbumsPage => Self::Collection(CollectionPageHit::AlbumsNext),
            E::PreviousPeoplePage => Self::Collection(CollectionPageHit::PeoplePrevious),
            E::NextPeoplePage => Self::Collection(CollectionPageHit::PeopleNext),
            E::PreviousPlacesPage => Self::Collection(CollectionPageHit::PlacesPrevious),
            E::NextPlacesPage => Self::Collection(CollectionPageHit::PlacesNext),
            E::StopImport => Self::Directory(DirectoryStatusHit::Stop),
            E::ShowImportDetails => Self::Directory(DirectoryStatusHit::Details),
            E::DismissImport => Self::Directory(DirectoryStatusHit::Dismiss),
            E::PeopleBack => Self::People(PeopleIntent::Back),
            E::TogglePeopleRecognition => Self::People(PeopleIntent::ToggleRecognition),
            E::CancelPeopleScan => Self::People(PeopleIntent::CancelScan),
            E::OpenApprovedPerson { person } => Self::People(PeopleIntent::OpenApproved(person)),
            E::OpenSuggestion { cluster } => Self::People(PeopleIntent::OpenSuggestion(cluster)),
            E::BrowseApprovedPeople => Self::People(PeopleIntent::BrowseApproved),
            E::SavePersonName => Self::People(PeopleIntent::SaveName),
            E::BeginPeopleMerge => Self::People(PeopleIntent::BeginMerge),
            E::MergeApprovedInto { person } => {
                Self::People(PeopleIntent::MergeApprovedInto(person))
            }
            E::ApproveSuggestion => Self::People(PeopleIntent::ApproveSuggestion),
            E::RejectSuggestion => Self::People(PeopleIntent::RejectSuggestion),
            E::HideSuggestion => Self::People(PeopleIntent::HideSuggestion),
            E::RestoreSuggestion => Self::People(PeopleIntent::RestoreSuggestion),
            E::MergeSuggestionWith { person } => {
                Self::People(PeopleIntent::MergeSuggestionWith(person))
            }
            E::ToggleHiddenSuggestions => Self::People(PeopleIntent::ToggleHidden),
            E::PeoplePage { forward } => Self::People(PeopleIntent::Page(forward)),
            E::DeleteLocalPeopleData => Self::People(PeopleIntent::DeleteLocalData),
            E::ConfirmDeleteLocalPeopleData => Self::People(PeopleIntent::ConfirmDeleteLocalData),
            _ => return None,
        })
    }
}

impl Photos {
    /// Apply callbacks after the host has admitted and dispatched an emitted
    /// control action. Pointer handling only queues the action; the callback
    /// runs after that event has returned, so `sync` is the common completion
    /// boundary for pointer, keyboard, accessibility, and agent activation.
    pub(super) fn drain_action_control_pending(&mut self) {
        self.drain_pending();
        if let Some(hit) = self.directory_import_pending.take() {
            self.activate_directory_status(hit);
        }
        if let Some(hit) = self.library_analysis_pending.take() {
            self.activate_library_analysis(hit);
        }
        if let Some(hit) = self.collection_page_pending.take() {
            self.page_collection(hit);
        }
        if let Some(hit) = self.metadata_editor_pending.take() {
            self.activate_metadata_editor_hit(hit);
        }
        if let Some(hit) = self.album_editor_pending.take() {
            match hit {
                AlbumEditorHit::New => self.begin_new_album(),
                AlbumEditorHit::Edit => self.begin_edit_album(),
                AlbumEditorHit::Save => self.save_album_editor(),
                AlbumEditorHit::Cancel => self.cancel_album_editor(),
                AlbumEditorHit::Delete => {
                    self.album_delete_confirm = true;
                    self.notice = Some(
                        "Press Delete album? again to keep photographs and delete only the album"
                            .to_owned(),
                    );
                }
                AlbumEditorHit::ConfirmDelete => self.delete_active_album(),
                AlbumEditorHit::PreviousPage => self.page_library(false),
                AlbumEditorHit::NextPage => self.page_library(true),
            }
        }
        let people = self.people_pending.borrow_mut().take();
        if let Some(intent) = people {
            self.handle_people_intent(intent);
        }
    }
}

impl Photos {
    /// Answer `effect` for the view's effect handler: a job's answer is kept
    /// under its invocation, and settles as its work reports.
    pub(crate) fn effect(
        &mut self,
        effect: PhotosEffect,
        cx: &mut EffectCx<PhotoLibrary>,
    ) -> EffectAnswer {
        let job = PhotosEffect::declarations()
            .iter()
            .any(|decl| decl.id == effect.id() && matches!(decl.answers, Answers::Later { .. }));
        let invocation = cx.invocation().unwrap_or_default();
        // A job's work reports under its invocation from the moment it
        // starts, so its answer is kept before the work begins.
        if job {
            let reply = cx.reply();
            self.register_reply(invocation, reply);
        }
        let state = cx.live().get();
        match self.answer_effect(effect, invocation, &state) {
            Ok(Some(output)) if job => {
                self.settle_now(invocation, Ok(output));
                Ok(Effected::Pending)
            }
            Ok(Some(output)) => Ok(Effected::Done(output)),
            Ok(None) if job => Ok(Effected::Pending),
            Ok(None) => Ok(Effected::done()),
            Err(error) => {
                self.forget_reply(invocation);
                Err(error)
            }
        }
    }

    /// Answer `effect` — invoked by a control, the palette, an agent or a
    /// script — against `library` as it is now. `Ok(None)` when its work
    /// continues: it reports through [`Self::settle`] under `invocation`
    /// and changes the library with ordinary actions as it goes.
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match over every Photos effect keeps their answers auditable"
    )]
    pub(crate) fn answer_effect(
        &mut self,
        effect: PhotosEffect,
        invocation: tilo_actions::InvocationId,
        library: &crate::model::PhotoLibrary,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        let done = || Ok(Some(tilo_actions::ActionOutput::default()));
        if matches!(
            effect,
            PhotosEffect::RotateShownLeft
                | PhotosEffect::RotateShownRight
                | PhotosEffect::CropShownToView
                | PhotosEffect::ResetShownEdits
                | PhotosEffect::ShareShown
        ) && self.shown_photo().is_none()
        {
            return Err(tr("photos-empty-title"));
        }
        if self.post_control(&effect) {
            return done();
        }
        match effect {
            PhotosEffect::View { photo_id } => self.view_photo(library, &photo_id)?,
            PhotosEffect::ViewLibrary => self.show_library(),
            PhotosEffect::ViewAlbums => self.show_albums(),
            PhotosEffect::ViewTimeline { anchor } => {
                self.view_timeline(library, anchor.as_ref())?;
            }
            PhotosEffect::ViewPeople => {
                self.show_people();
                self.notice = Some(
                    "People is local-first: install a signed face model, then review every suggestion before sharing labels".to_owned(),
                );
            }
            PhotosEffect::ViewLibraryAnalysis => {
                self.show_library_analysis();
            }
            PhotosEffect::ManagePeopleRecognition => {
                self.notice = Some(
                    "Open Models in Settings to install and start a signed local face-recognition model"
                        .to_owned(),
                );
            }
            PhotosEffect::EnablePeopleRecognition => {
                self.people_controller.set_recognition_enabled(true);
                self.people_controller
                    .discover_face_capability(&agent_provider_config::ProviderConfig::load());
                self.notice = Some(self.people_controller.capability().summary());
            }
            PhotosEffect::ViewFavorites => self.view_favorites(library)?,
            PhotosEffect::ViewPlaces => {
                self.show_library();
                self.library_destination = LibraryDestination::Places;
                self.places_open_key = None;
                self.places_fit_pending = true;
            }
            PhotosEffect::ViewPlace { place_key } => {
                self.open_place_collection(library, &place_key)?;
            }
            PhotosEffect::ViewAlbum { album_id } => self.view_album(library, &album_id)?,
            PhotosEffect::ViewFolder { folder } => {
                self.view_metadata_filter(library, Some(folder), None, None)?;
            }
            PhotosEffect::ViewPerson { person } => {
                self.view_metadata_filter(library, None, Some(person), None)?;
            }
            PhotosEffect::ViewTag { tag } => {
                self.view_metadata_filter(library, None, None, Some(tag))?;
            }
            PhotosEffect::ViewObject { object } => {
                self.view_analysis_filter(library, object, false)?;
            }
            PhotosEffect::ViewPet { pet } => self.view_analysis_filter(library, pet, true)?,
            PhotosEffect::AnalyzeLibrary => {
                self.begin_library_analysis(invocation, library, false)?;
                return Ok(None);
            }
            PhotosEffect::ResumeLibraryAnalysis => {
                self.resume_library_analysis(invocation, library)?;
                return Ok(None);
            }
            PhotosEffect::RetryLibraryAnalysisFailures => {
                self.begin_library_analysis(invocation, library, true)?;
                return Ok(None);
            }
            PhotosEffect::ReadPeople => return Ok(Some(answered(self.read_people(library)))),
            PhotosEffect::ReadPerson { person } => {
                return read_person(library, &person).map(|read| Some(answered(read)));
            }
            PhotosEffect::ReadLibraryAnalysis => {
                return Ok(Some(answered(self.read_library_analysis())));
            }
            PhotosEffect::PauseLibraryAnalysis => {
                if !self.pause_library_analysis() {
                    return Err("Photo analysis is not running".to_owned());
                }
                return Ok(Some(tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({
                        "completed": self.library_analysis.completed,
                        "remaining": self.library_analysis.remaining(),
                    })),
                    resources: vec!["photos.library-analysis".to_owned()],
                }));
            }
            PhotosEffect::AnalyzeText {
                resource_id,
                language,
            } => {
                let photo = library
                    .photo(&resource_id)
                    .ok_or_else(|| "that photograph is no longer in the library".to_owned())?;
                let sender = self.analysis_sender().ok_or_else(|| {
                    "local text recognition is unavailable on this device".to_owned()
                })?;
                sender
                    .try_send(AnalysisJob {
                        invocation,
                        photo_id: resource_id,
                        image: photo.image.clone(),
                        kind: AnalysisJobKind::Ocr { language },
                    })
                    .map_err(|_| {
                        "text recognition is busy; wait for the current photos to finish".to_owned()
                    })?;
                self.active_analysis.insert(invocation);
                return Ok(None);
            }
            PhotosEffect::AnalyzeImage { resource_id } => {
                let photo = library
                    .photo(&resource_id)
                    .ok_or_else(|| "that photograph is no longer in the library".to_owned())?;
                let capability = tilo_vision::discover_local_capability(
                    &agent_provider_config::ProviderConfig::load(),
                )
                .ok_or_else(|| {
                    "No verified local image-analysis model is ready. Install and start one in Settings."
                        .to_owned()
                })?;
                let sender = self.analysis_sender().ok_or_else(|| {
                    "local image analysis is unavailable on this device".to_owned()
                })?;
                sender
                    .try_send(AnalysisJob {
                        invocation,
                        photo_id: resource_id,
                        image: photo.image.clone(),
                        kind: AnalysisJobKind::Vision { capability },
                    })
                    .map_err(|_| {
                        "local image analysis is busy; wait for the current photos to finish"
                            .to_owned()
                    })?;
                self.active_analysis.insert(invocation);
                return Ok(None);
            }
            PhotosEffect::AnalyzePeopleLibrary => {
                self.schedule_people_scan_batch(invocation, library)?;
                return Ok(None);
            }
            PhotosEffect::ImportCameraMedia { media_id, name } => {
                // Verify the portable Camera identity and bytes before the
                // one idempotent library import: a missing local blob is a
                // refusal, never a phantom entry.
                let photo = crate::camera_import_photo(&media_id, &name, &self.content)?;
                let id = photo.id.clone();
                let dispatch = self
                    .dispatch
                    .as_ref()
                    .ok_or_else(|| "this window edits no library".to_owned())?;
                dispatch.submit(crate::resource::import(&photo));
                self.notice = Some("Imported Camera photo".to_owned());
                return Ok(Some(tilo_actions::ActionOutput {
                    value: Some(serde_json::Value::String(id.clone())),
                    resources: vec![id],
                }));
            }
            PhotosEffect::ExportEditedContent { photo } => {
                return crate::actions::export_edited_content(
                    photo.as_str(),
                    library,
                    &self.content,
                )
                .map(Some);
            }
            PhotosEffect::MakeThumbnails { photos } => {
                return crate::actions::make_thumbnails(&photos, &self.content).map(Some);
            }
            PhotosEffect::Import { path: Some(file) } => {
                self.import_content = Some(LiveContent {
                    library: library.clone(),
                });
                self.pending_resource_open = Some((invocation, file.to_string_lossy()));
                return Ok(None);
            }
            PhotosEffect::Save { path } => {
                let open = self
                    .shown_photo_in(library)
                    .ok_or_else(|| "no photograph is open".to_string())?;
                let Some(path) = path else {
                    if !self.choose_save_destination(&open.image, Some(invocation)) {
                        return Err(tr("photos-save-failed"));
                    }
                    return Ok(None);
                };
                if !self.save_to(&path) {
                    return Err(tr("photos-save-failed"));
                }
            }
            other => {
                self.perform_effect(other, library)?;
                if self.picker.is_some()
                    || self.contents_picker.is_some()
                    || self.directory_picker.is_some()
                    || self.directory_import.is_some()
                {
                    self.resource_invocation = Some(invocation);
                    if let Some(import) = self.directory_import.as_mut() {
                        import.invocation = Some(invocation);
                    }
                    return Ok(None);
                }
            }
        }
        done()
    }

    /// Keep `reply`, owed to the caller of the effect whose work reports
    /// under `invocation`.
    pub(crate) fn register_reply(
        &mut self,
        invocation: tilo_actions::InvocationId,
        reply: ui::framework::EffectReply,
    ) {
        self.replies.insert(invocation, reply);
    }

    /// Answer the caller of `invocation` now.
    pub(crate) fn settle_now(
        &mut self,
        invocation: tilo_actions::InvocationId,
        result: Result<tilo_actions::ActionOutput, String>,
    ) {
        if let Some(reply) = self.replies.remove(&invocation) {
            reply.finish(result);
        }
    }

    /// Drop the reply owed under `invocation`: the effect failed at once,
    /// and its caller learns that from the failure itself.
    pub(crate) fn forget_reply(&mut self, invocation: tilo_actions::InvocationId) {
        self.replies.remove(&invocation);
    }

    /// What the job answering `update`'s invocation reports: its progress,
    /// or its end, told to whoever invoked it.
    pub(super) fn settle(&mut self, update: tilo_actions::InvocationUpdate) {
        let invocation = update.invocation();
        #[cfg(test)]
        self.settled.push(update.clone());
        match update {
            tilo_actions::InvocationUpdate::Progress {
                phase,
                completed,
                total,
                ..
            } => {
                if let Some(reply) = self.replies.get(&invocation) {
                    reply.progress().report(phase, completed, total);
                }
            }
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                if let Some(reply) = self.replies.remove(&invocation) {
                    reply.finish(Ok(output));
                }
            }
            tilo_actions::InvocationUpdate::Failed { error, .. } => {
                if let Some(reply) = self.replies.remove(&invocation) {
                    reply.answer(Err(error));
                }
            }
            tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                if let Some(reply) = self.replies.remove(&invocation) {
                    reply.finish(Err(message.unwrap_or_else(|| "cancelled".to_owned())));
                }
            }
            _ => {}
        }
    }

    /// The last report settled for `invocation`, taken.
    #[cfg(test)]
    pub(crate) fn take_settled(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let at = self
            .settled
            .iter()
            .rposition(|update| update.invocation() == invocation)?;
        Some(self.settled.remove(at))
    }

    /// Stop the work its callers cancelled. The framework has told them;
    /// what is stopped here is the work itself.
    pub(super) fn follow_cancellations(&mut self) {
        let cancelled: Vec<_> = self
            .replies
            .iter()
            .filter(|(_, reply)| reply.is_cancelled())
            .map(|(invocation, _)| *invocation)
            .collect();
        for invocation in cancelled {
            self.replies.remove(&invocation);
            self.cancel_invocation(invocation);
        }
    }

    /// Stop the work answering `invocation`.
    pub(super) fn cancel_invocation(&mut self, invocation: tilo_actions::InvocationId) {
        if self.library_analysis.is_running_invocation(invocation) {
            // The photograph in progress may finish safely.
            self.pause_library_analysis();
        }
        if self.active_analysis.remove(&invocation) {
            self.cancelled_analysis.insert(invocation);
            if self.active_people_scans.remove(&invocation) {
                self.pending_people_scans.remove(&invocation);
                "cancelled".clone_into(&mut self.people_scan_progress.status);
                self.save_people_scan_progress();
            }
        }
        if self
            .pending_resource_open
            .as_ref()
            .is_some_and(|(pending, _)| *pending == invocation)
        {
            self.pending_resource_open = None;
        }
        if self
            .save_chooser
            .as_ref()
            .is_some_and(|(_, pending)| *pending == Some(invocation))
        {
            // The chooser may still write what it was handed; the caller
            // only stops waiting for its answer.
            self.save_chooser = None;
        }
        if self.resource_invocation == Some(invocation) {
            // Dropping the receiver closes this app's portal request channel;
            // a late response can no longer open a file after the caller
            // cancelled. Completed photographs of a folder are kept.
            self.picker = None;
            self.contents_picker = None;
            self.directory_picker = None;
            if let Some(import) = self.directory_import.as_mut() {
                import.cancel.store(true, Ordering::Relaxed);
                import.invocation = None;
                import.cancelled = true;
                import.pending.clear();
            }
            self.resource_invocation = None;
        }
    }

    /// Take the Save chooser's answer, if it has arrived: say how the save
    /// went and settle the invocation that asked for it.
    pub(super) fn drain_save_chooser(&mut self) {
        let Some((rx, invocation)) = self.save_chooser.as_ref() else {
            return;
        };
        let invocation = *invocation;
        let outcome = match rx.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ui::portal::SaveOutcome::Failed(tr("photos-picker-failed"))
            }
        };
        self.save_chooser = None;
        let update = match outcome {
            ui::portal::SaveOutcome::Saved => {
                self.notice = Some(tr("photos-saved-edited"));
                invocation.map(|invocation| tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                })
            }
            ui::portal::SaveOutcome::Cancelled => {
                invocation.map(|invocation| tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some(tr("photos-save-cancelled")),
                })
            }
            ui::portal::SaveOutcome::Failed(message) => {
                self.notice = Some(format!("{}: {message}", tr("photos-save-failed")));
                invocation.map(|invocation| tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "photos.save_failed".into(),
                        message,
                        details: None,
                        retryable: true,
                    },
                })
            }
        };
        if let Some(update) = update {
            self.settle(update);
        }
    }
}

impl Photos {
    /// Who is named in `library` and on how many photographs, and whether
    /// this device recognises faces: the People place's read. This
    /// device's unapproved suggestions are not part of it.
    fn read_people(&self, library: &crate::model::PhotoLibrary) -> serde_json::Value {
        use crate::people::FaceCapability;
        let capability = self.people_controller.capability();
        serde_json::json!({
            "recognition": {
                "enabled": self.people_controller.privacy().recognition_enabled,
                "state": match capability {
                    FaceCapability::DisabledByOwner => "off",
                    FaceCapability::NoVerifiedFaceModel => "no-model",
                    FaceCapability::Ready { .. } => "ready",
                },
                "summary": capability.summary(),
            },
            "people": crate::people::approved_people(library)
                .iter()
                .map(|person| serde_json::json!({
                    "name": person.label,
                    "photos": person.count(),
                }))
                .collect::<Vec<_>>(),
        })
    }

    /// Where this device's analysis of the library stands: the analysis
    /// place's read.
    fn read_library_analysis(&self) -> serde_json::Value {
        use crate::library_analysis::LibraryAnalysisPhase;
        let analysis = &self.library_analysis;
        serde_json::json!({
            "phase": match analysis.phase {
                LibraryAnalysisPhase::Idle => "idle",
                LibraryAnalysisPhase::Running => "running",
                LibraryAnalysisPhase::Paused => "paused",
                LibraryAnalysisPhase::Complete => "complete",
            },
            "model": analysis.model_label,
            "model_revision": analysis.model_revision,
            "total": analysis.total,
            "completed": analysis.completed,
            "succeeded": analysis.succeeded,
            "failed": analysis.failed,
            "unavailable": analysis.unavailable,
            "remaining": analysis.remaining(),
            "current": analysis.current_name,
            "failures": analysis
                .failures()
                .map(|failure| serde_json::json!({
                    "photo": failure.photo_id,
                    "name": failure.name,
                    "message": failure.message,
                }))
                .collect::<Vec<_>>(),
        })
    }
}

/// `value` as a read's answer.
fn answered(value: serde_json::Value) -> tilo_actions::ActionOutput {
    tilo_actions::ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    }
}

/// The person named `person` in `library` and the photographs they are
/// named on: the person's read.
fn read_person(
    library: &crate::model::PhotoLibrary,
    person: &str,
) -> Result<serde_json::Value, String> {
    let person = crate::people::approved_people(library)
        .into_iter()
        .find(|named| named.label == person)
        .ok_or_else(|| "no one by that name is in the library".to_owned())?;
    Ok(serde_json::json!({
        "name": person.label,
        "photos": person.photo_ids,
    }))
}
