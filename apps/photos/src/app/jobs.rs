//! Work that runs off the frame: thumbnails, per-photo analysis, the library-wide analysis and
//! the batched People scan. Each worker answers over a channel; these methods queue its jobs and
//! fold its results back into the library.

use crate::model::{Edits, LibraryPhoto, OpenImage, PhotoLibrary};
use crate::pixels;
use crate::strings::{tr, tr_args, tr_count, tr_with};
use std::sync::mpsc::{Receiver, SyncSender};
use tilo_content::ContentStore;
use ui::ArgValue;
use ui::tilo_resource::Id;
use vello::peniko::ImageData;

use super::{
    ANALYSIS_QUEUE_LIMIT, AnalysisJob, AnalysisJobKind, AnalysisOutcome, AnalysisResult,
    ImageAnalysisComponent, LibraryAnalysisHit, LiveContent, PendingPeopleScan, PeopleScanProgress,
    Photos, THUMBNAIL_QUEUE_LIMIT, THUMBNAIL_SIZE, ThumbnailResult, thumbnail_key, to_peniko,
};

impl PeopleScanProgress {
    pub(super) fn batch_range(&self, available: usize, limit: usize) -> Option<(usize, usize)> {
        (available > 0).then(|| {
            let start = self.next_offset % available;
            (start, limit.min(available.saturating_sub(start)))
        })
    }

    pub(super) fn advance_after_success(&mut self, start: usize, scanned: usize, total: usize) {
        if total == 0 {
            return;
        }
        self.next_offset = start.saturating_add(scanned) % total;
        self.last_library_size = total;
        self.completed_batches = self.completed_batches.saturating_add(1);
        if self.next_offset == 0 {
            "complete"
        } else {
            "paused"
        }
        .clone_into(&mut self.status);
        self.last_error = None;
    }

    pub(super) fn record_failure(&mut self, error: String) {
        "error".clone_into(&mut self.status);
        self.last_error = Some(error);
    }
    pub(super) fn load() -> Self {
        Self::file()
            .and_then(|file| file.read())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub(super) fn save(&self) -> Result<(), String> {
        let file = Self::file().ok_or_else(|| "no state location".to_owned())?;
        file.write_json(self)
    }

    fn file() -> Option<crate::files::StateFile> {
        crate::files::StateFile::in_state("people-scan.json")
    }
}

fn analysis_failure(
    invocation: tilo_actions::InvocationId,
    message: String,
) -> tilo_actions::InvocationUpdate {
    tilo_actions::InvocationUpdate::Failed {
        invocation,
        error: tilo_actions::ActionFailure {
            code: "photos.analysis_failed".into(),
            message,
            details: None,
            retryable: true,
        },
    }
}

fn mark_analysis_shared(analysis: &mut crate::model::ImageAnalysis) {
    "shared".clone_into(&mut analysis.provenance.scope);
    if let Some(provenance) = &mut analysis.ocr_provenance {
        "shared".clone_into(&mut provenance.scope);
    }
    if let Some(provenance) = &mut analysis.vision_provenance {
        "shared".clone_into(&mut provenance.scope);
    }
}

fn analysis_worker(
    rx: &Receiver<AnalysisJob>,
    tx: &ui::wake::WakingSyncSender<AnalysisResult>,
    content: &ContentStore,
) {
    while let Ok(job) = rx.recv() {
        if tx
            .send(AnalysisResult {
                invocation: job.invocation,
                photo_id: job.photo_id,
                content: job.image.hash(),
                result: match job.kind {
                    AnalysisJobKind::Ocr { language } => AnalysisOutcome::Image {
                        component: ImageAnalysisComponent::Ocr,
                        result: crate::analysis::recognize_text(&job.image, &language, content),
                    },
                    AnalysisJobKind::Vision { capability } => AnalysisOutcome::Image {
                        component: ImageAnalysisComponent::Vision,
                        result: crate::analysis::analyze_image(&job.image, &capability, content),
                    },
                    AnalysisJobKind::People { capability, images } => AnalysisOutcome::People(
                        crate::analysis::detect_people(images, &capability, content),
                    ),
                },
            })
            .is_err()
        {
            return;
        }
    }
}

pub(super) fn with_thumbnail_wake(
    service: thumbnail::ThumbnailService,
) -> thumbnail::ThumbnailService {
    let wake = ui::wake::WakeHandle::new();
    service.set_wake(move || wake.wake());
    service
}

pub(super) fn thumbnail_request(
    image: &OpenImage,
    content: &ContentStore,
) -> thumbnail::ThumbRequest {
    let key = thumbnail::ThumbKey::of_hash(image.content.hash(), THUMBNAIL_SIZE);
    let (image, content) = (image.clone(), content.clone());
    thumbnail::ThumbRequest::load(key, thumbnail::FileCategory::Image, move || {
        crate::model::bytes_of(&image, &content)
    })
}

impl Photos {
    pub(super) fn save_people_scan_progress(&mut self) {
        if let Err(error) = self.people_scan_progress.save() {
            self.state_save_failure = Some(error);
        }
    }

    pub(super) fn report_state_save_failure(&mut self) {
        let scan = self.state_save_failure.take();
        let people = self.people_controller.take_save_failure();
        if let Some(error) = scan.or(people) {
            self.notice = Some(tr_with("photos-state-not-kept", "reason", &error));
        }
    }
    fn analysis_finished_notice(&self) -> String {
        tr_args(
            "photos-analysis-finished-notice",
            &[
                (
                    "analyzed",
                    ArgValue::Number(self.library_analysis.succeeded as f64),
                ),
                (
                    "failed",
                    ArgValue::Number(self.library_analysis.failed as f64),
                ),
            ],
        )
    }

    pub(super) fn discover_library_analysis_capability()
    -> Result<tilo_vision::LocalVisionCapability, String> {
        tilo_vision::discover_local_capability(&agent_provider_config::ProviderConfig::load())
            .ok_or_else(|| tr("photos-analysis-error-no-model"))
    }

    pub(super) fn begin_library_analysis(
        &mut self,
        invocation: tilo_actions::InvocationId,
        library: &crate::model::PhotoLibrary,
        retry_failures: bool,
    ) -> Result<(), String> {
        if self.library_analysis.phase == crate::library_analysis::LibraryAnalysisPhase::Running {
            return Err(tr("photos-analysis-error-running"));
        }
        let capability = Self::discover_library_analysis_capability()?;
        if retry_failures {
            if !self.library_analysis.begin_retry(
                invocation,
                capability.model_label.clone(),
                capability.revision.clone(),
            ) {
                return Err(tr("photos-analysis-error-no-failures"));
            }
        } else {
            let mut unavailable = 0;
            let targets = library
                .photos
                .values()
                .filter(|photo| {
                    !photo.analysis().is_some_and(|analysis| {
                        analysis.has_vision_component(&photo.image.hash(), &capability.revision)
                    })
                })
                .filter_map(|photo| {
                    if crate::model::bytes_of(&photo.image, &self.content).is_some() {
                        Some(photo.id.clone())
                    } else {
                        unavailable += 1;
                        None
                    }
                })
                .collect::<Vec<_>>();
            self.library_analysis.begin(
                invocation,
                targets,
                unavailable,
                capability.model_label.clone(),
                capability.revision.clone(),
            );
        }
        self.library_analysis_capability = Some(capability);
        self.active_analysis.insert(invocation);
        self.schedule_next_library_analysis(library)?;
        Ok(())
    }

    pub(super) fn resume_library_analysis(
        &mut self,
        invocation: tilo_actions::InvocationId,
        library: &crate::model::PhotoLibrary,
    ) -> Result<(), String> {
        let capability = Self::discover_library_analysis_capability()?;
        if self.library_analysis.model_revision.as_deref() != Some(capability.revision.as_str()) {
            return Err(tr("photos-analysis-error-model-changed"));
        }
        if !self.library_analysis.resume(invocation) {
            return Err(tr("photos-analysis-error-not-paused"));
        }
        self.library_analysis_capability = Some(capability);
        self.active_analysis.insert(invocation);
        self.schedule_next_library_analysis(library)?;
        Ok(())
    }

    pub(super) fn pause_library_analysis(&mut self) -> bool {
        let invocation = self.library_analysis.invocation;
        let paused = self.library_analysis.pause();
        if paused {
            if let Some(invocation) = invocation {
                self.active_analysis.remove(&invocation);
            }
            self.notice = Some(tr("photos-analysis-pausing"));
        }
        paused
    }

    pub(super) fn activate_library_analysis(&mut self, hit: LibraryAnalysisHit) -> bool {
        let invocation = tilo_actions::InvocationId::new();
        let library = self.presented();
        let result = match hit {
            LibraryAnalysisHit::Start => self.begin_library_analysis(invocation, &library, false),
            LibraryAnalysisHit::Pause => self
                .pause_library_analysis()
                .then_some(())
                .ok_or_else(|| "Photo analysis is not running".to_owned()),
            LibraryAnalysisHit::Resume => self.resume_library_analysis(invocation, &library),
            LibraryAnalysisHit::RetryFailures => {
                self.begin_library_analysis(invocation, &library, true)
            }
        };
        if let Err(error) = result {
            self.notice = Some(error);
            return false;
        }
        true
    }

    fn finish_library_analysis(&mut self, invocation: tilo_actions::InvocationId) {
        self.active_analysis.remove(&invocation);
        self.settle(tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput {
                value: Some(serde_json::json!({
                    "analyzed": self.library_analysis.succeeded,
                    "failed": self.library_analysis.failed,
                    "unavailable": self.library_analysis.unavailable,
                })),
                resources: vec!["photos.library-analysis".to_owned()],
            },
        });
        self.notice = Some(self.analysis_finished_notice());
    }

    fn schedule_next_library_analysis(
        &mut self,
        library: &crate::model::PhotoLibrary,
    ) -> Result<(), String> {
        loop {
            let Some(photo_id) = self
                .library_analysis
                .take_next(|id| library.photo(id).cloned().map(|photo| photo.image.name))
            else {
                if self.library_analysis.finish_if_empty()
                    && let Some(invocation) = self.library_analysis.invocation
                {
                    self.finish_library_analysis(invocation);
                }
                return Ok(());
            };
            let Some(photo) = library.photo(&photo_id).cloned() else {
                let _ = self.library_analysis.record_failure(
                    crate::library_analysis::LibraryAnalysisFailure {
                        photo_id: photo_id.clone(),
                        name: photo_id,
                        message: tr("photos-analysis-error-removed"),
                    },
                );
                continue;
            };
            let capability = self
                .library_analysis_capability
                .clone()
                .ok_or_else(|| tr("photos-analysis-error-model-gone"))?;
            let invocation = self
                .library_analysis
                .invocation
                .ok_or_else(|| tr("photos-analysis-error-lost"))?;
            let sender = self
                .analysis_sender()
                .ok_or_else(|| "local image analysis is unavailable on this device".to_owned())?;
            sender
                .try_send(AnalysisJob {
                    invocation,
                    photo_id,
                    image: photo.image,
                    kind: AnalysisJobKind::Vision { capability },
                })
                .map_err(|_| tr("photos-analysis-error-busy"))?;
            return Ok(());
        }
    }

    pub(super) fn drain_library_analysis_result(&mut self, completed: AnalysisResult) {
        let photo_name = self
            .library_analysis
            .current_name
            .clone()
            .unwrap_or_else(|| completed.photo_id.clone());
        let stale = self
            .presented()
            .photo(&completed.photo_id)
            .cloned()
            .as_ref()
            .is_none_or(|photo| photo.image.hash() != completed.content);
        let item_result = match completed.result {
            AnalysisOutcome::Image {
                component: ImageAnalysisComponent::Vision,
                result: Ok(mut analysis),
            } if !stale => {
                mark_analysis_shared(&mut analysis);
                self.submit(
                    PhotoLibrary::MERGE_IMAGE_ANALYSIS
                        .with((Id::from_raw(completed.photo_id.clone()), analysis)),
                )
            }
            AnalysisOutcome::Image {
                component: ImageAnalysisComponent::Ocr,
                ..
            }
            | AnalysisOutcome::People(_) => Err(tr("photos-analysis-error-wrong-result")),
            AnalysisOutcome::Image { result: Ok(_), .. } => {
                Err(tr("photos-analysis-error-changed"))
            }
            AnalysisOutcome::Image {
                result: Err(error), ..
            } => Err(error),
        };
        match item_result {
            Ok(()) => {
                let _ = self.library_analysis.record_success(&completed.photo_id);
            }
            Err(message) => {
                let _ = self.library_analysis.record_failure(
                    crate::library_analysis::LibraryAnalysisFailure {
                        photo_id: completed.photo_id,
                        name: photo_name,
                        message,
                    },
                );
            }
        }
        if let Some(invocation) = self.library_analysis.invocation {
            if self.library_analysis.phase
                == crate::library_analysis::LibraryAnalysisPhase::Complete
            {
                self.finish_library_analysis(invocation);
            } else {
                self.settle(tilo_actions::InvocationUpdate::Progress {
                    invocation,
                    sequence: u64::try_from(self.library_analysis.completed).unwrap_or(u64::MAX),
                    phase: if self.library_analysis.phase
                        == crate::library_analysis::LibraryAnalysisPhase::Paused
                    {
                        "paused".to_owned()
                    } else {
                        "analyzing".to_owned()
                    },
                    completed: Some(
                        u64::try_from(self.library_analysis.completed).unwrap_or(u64::MAX),
                    ),
                    total: Some(u64::try_from(self.library_analysis.total).unwrap_or(u64::MAX)),
                    message: Some(tr_args(
                        "photos-analysis-tally",
                        &[
                            (
                                "analyzed",
                                ArgValue::Number(self.library_analysis.succeeded as f64),
                            ),
                            (
                                "failed",
                                ArgValue::Number(self.library_analysis.failed as f64),
                            ),
                        ],
                    )),
                });
            }
        }
        if self.library_analysis.phase == crate::library_analysis::LibraryAnalysisPhase::Running
            && let Err(error) = self.schedule_next_library_analysis(&self.presented())
        {
            self.library_analysis.pause();
            self.notice = Some(tr_with("photos-analysis-paused-because", "reason", &error));
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one drain loop applies every completed analysis job kind with the same stale/cancelled checks"
    )]
    pub(super) fn drain_analysis_results(&mut self) {
        while let Ok(completed) = self.analysis_rx.try_recv() {
            if self.library_analysis.invocation == Some(completed.invocation) {
                self.drain_library_analysis_result(completed);
                continue;
            }
            self.active_analysis.remove(&completed.invocation);
            self.active_people_scans.remove(&completed.invocation);
            let pending_people_scan = self.pending_people_scans.remove(&completed.invocation);
            if self.cancelled_analysis.remove(&completed.invocation) {
                continue;
            }
            let stale = matches!(&completed.result, AnalysisOutcome::Image { .. })
                && self
                    .presented()
                    .photo(&completed.photo_id)
                    .cloned()
                    .as_ref()
                    .is_none_or(|photo| photo.image.hash() != completed.content);
            let update = if stale {
                tilo_actions::InvocationUpdate::Failed {
                    invocation: completed.invocation,
                    error: tilo_actions::ActionFailure {
                        code: "photos.analysis_stale".into(),
                        message: tr("photos-analysis-error-changed-retry"),
                        details: None,
                        retryable: true,
                    },
                }
            } else {
                match completed.result {
                    AnalysisOutcome::Image {
                        component: _,
                        result: Ok(mut analysis),
                    } => {
                        mark_analysis_shared(&mut analysis);
                        match self.submit(
                            PhotoLibrary::MERGE_IMAGE_ANALYSIS
                                .with((Id::from_raw(completed.photo_id), analysis)),
                        ) {
                            Ok(()) => {
                                self.notice = Some(tr("photos-analysis-shared"));
                                tilo_actions::InvocationUpdate::Succeeded {
                                    invocation: completed.invocation,
                                    output: tilo_actions::ActionOutput::default(),
                                }
                            }
                            Err(error) => analysis_failure(completed.invocation, error),
                        }
                    }
                    AnalysisOutcome::Image {
                        result: Err(error), ..
                    } => analysis_failure(completed.invocation, error),
                    AnalysisOutcome::People(Err(error)) => {
                        self.people_scan_progress.record_failure(error.clone());
                        self.save_people_scan_progress();
                        analysis_failure(completed.invocation, error)
                    }
                    AnalysisOutcome::People(Ok(batch)) => {
                        let added = self.people_controller.ingest_verified_face_clusters(batch);
                        if let Some(pending) = pending_people_scan {
                            self.people_scan_progress.advance_after_success(
                                pending.start,
                                pending.scanned,
                                pending.total,
                            );
                            self.save_people_scan_progress();
                            self.notice = Some(if added == 0 {
                                tr("photos-people-scan-none")
                            } else {
                                tr_count("photos-people-scan-found", added)
                            });
                            if self.people_scan_progress.next_offset != 0 {
                                if let Err(error) = self.schedule_people_scan_batch(
                                    completed.invocation,
                                    &self.presented(),
                                ) {
                                    self.people_scan_progress.record_failure(error.clone());
                                    self.save_people_scan_progress();
                                    analysis_failure(completed.invocation, error)
                                } else {
                                    tilo_actions::InvocationUpdate::Progress {
                                        invocation: completed.invocation,
                                        sequence: self.people_scan_progress.completed_batches,
                                        phase: "scanning".to_owned(),
                                        completed: Some(
                                            u64::try_from(self.people_scan_progress.next_offset)
                                                .unwrap_or(u64::MAX),
                                        ),
                                        total: Some(
                                            u64::try_from(
                                                self.people_scan_progress.last_library_size,
                                            )
                                            .unwrap_or(u64::MAX),
                                        ),
                                        message: Some(tr("photos-people-scan-next")),
                                    }
                                }
                            } else {
                                tilo_actions::InvocationUpdate::Succeeded {
                                    invocation: completed.invocation,
                                    output: tilo_actions::ActionOutput::default(),
                                }
                            }
                        } else {
                            self.people_scan_progress
                                .record_failure(tr("photos-people-scan-lost"));
                            self.save_people_scan_progress();
                            analysis_failure(completed.invocation, tr("photos-people-scan-lost"))
                        }
                    }
                }
            };
            self.settle(update);
        }
    }

    pub(super) fn analysis_sender(&mut self) -> Option<SyncSender<AnalysisJob>> {
        if self.analysis_tx.is_none() {
            let (job_tx, job_rx) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
            let results = self.analysis_result_tx.clone();
            let content = self.content.clone();
            self.analysis_tx = ui::rt::spawn_thread("photos-local-analysis", move || {
                analysis_worker(&job_rx, &results, &content);
            })
            .ok()
            .map(|_| job_tx);
        }
        self.analysis_tx.clone()
    }
}

impl Photos {
    pub(super) fn reconcile_content(&mut self) {
        let current = LiveContent {
            library: (*self.presented()).clone(),
        };
        if self
            .import_content
            .as_ref()
            .is_some_and(|old| old != &current)
        {
            self.picker_stale = true;
            if let Some((invocation, _)) = self.pending_resource_open.take() {
                self.settle(tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some(tr("photos-import-stale")),
                });
                self.import_content = None;
            }
        }
        if let Some(old) = &self.observed_content
            && old != &current
        {
            self.pending.take();
            self.library_cards.clear();
            self.library_details_cards.clear();
            let filed = |library: &crate::model::PhotoLibrary, id: &str| {
                library.photo(id).cloned().map(|mut photo| {
                    photo.edits = Edits::default();
                    photo
                })
            };
            let metadata_changed = self
                .metadata_editor_ids
                .iter()
                .any(|id| filed(&old.library, id) != filed(&current.library, id));
            let album_changed = self
                .album_editor_id
                .as_ref()
                .is_some_and(|id| old.library.album(id) != current.library.album(id))
                || self
                    .album_editor_members
                    .iter()
                    .any(|id| current.library.photo(id).is_none());
            let shown_removed = self.open_photo.as_ref().is_some_and(|id| {
                old.library.photo(id).is_some() && current.library.photo(id).is_none()
            });
            if shown_removed {
                self.open_photo = None;
                self.library_mode = true;
            }
            if metadata_changed {
                self.cancel_metadata_editor();
                self.notice = Some(tr("photos-details-conflict"));
            }
            if album_changed {
                self.cancel_album_editor();
                self.notice = Some(tr("photos-album-changed-elsewhere"));
            }
            if self
                .active_album
                .as_ref()
                .is_some_and(|id| current.library.album(id).is_none())
            {
                self.active_album = None;
            }
        }
        self.observed_content = Some(current);
        self.refresh();
    }

    pub(super) fn drain_thumbnail_results(&mut self) {
        let Some(generation) = self.thumbnail_generation else {
            return;
        };
        let settled: Vec<(String, Option<thumbnail::ThumbState>)> = self
            .thumbnail_keys
            .iter()
            .filter_map(|(id, key)| match self.thumbnail_service.peek(key) {
                Some(state) if state.is_loading() => None,
                state => Some((id.clone(), state)),
            })
            .collect();
        for (id, state) in settled {
            self.thumbnail_keys.remove(&id);
            match state {
                None => {
                    self.thumbnail_queued.remove(&id);
                }
                Some(state) => self.accept_thumbnail_result(ThumbnailResult {
                    generation,
                    id,
                    preview: state.ready().and_then(|preview| {
                        pixels::Rgba::new(preview.width, preview.height, preview.rgba.clone())
                    }),
                }),
            }
        }
    }

    pub(super) fn thumbnail_for(&self, photo_id: &str) -> Option<&ImageData> {
        self.thumbnail_resource_keys
            .get(photo_id)
            .and_then(|key| self.library_thumbnails.get(key))
    }

    pub(super) fn accept_thumbnail_result(&mut self, result: ThumbnailResult) {
        if Some(result.generation) != self.thumbnail_generation
            || !self.thumbnail_visible_ids.contains(&result.id)
        {
            return;
        }
        self.thumbnail_queued.remove(&result.id);
        if let Some(preview) = result.preview {
            self.library_thumbnails
                .insert(result.id, to_peniko(&preview));
        } else {
            self.thumbnail_unavailable.insert(result.id);
        }
    }

    pub(super) fn schedule_library_thumbnail(
        &mut self,
        sources: Vec<LibraryPhoto>,
        document_changed: bool,
    ) {
        self.thumbnail_resource_keys = sources
            .iter()
            .map(|photo| (photo.id.clone(), thumbnail_key(&photo.image)))
            .collect();
        let ids: Vec<_> = sources
            .iter()
            .map(|photo| thumbnail_key(&photo.image))
            .collect();
        if document_changed || ids != self.thumbnail_visible_ids {
            let Some(next_generation) = self
                .thumbnail_generation
                .and_then(|generation| generation.checked_add(1))
            else {
                self.withdraw_thumbnail_requests(&[]);
                self.thumbnail_generation = None;
                self.thumbnail_visible_ids = ids;
                self.thumbnail_queued.clear();
                self.library_thumbnails.clear();
                self.thumbnail_unavailable.clear();
                return;
            };
            self.withdraw_thumbnail_requests(&ids);
            self.thumbnail_generation = Some(next_generation);
            self.thumbnail_visible_ids = ids;
            self.thumbnail_queued.clear();
            if document_changed {
                self.library_thumbnails.clear();
                for photo in &sources {
                    self.thumbnail_service
                        .forget(&thumbnail_request(&photo.image, &self.content).key());
                }
            }
            self.library_thumbnails
                .retain(|id, _| self.thumbnail_visible_ids.contains(id));
            self.thumbnail_unavailable
                .retain(|id| self.thumbnail_visible_ids.contains(id));
        }
        if self.thumbnail_generation.is_none() {
            return;
        }
        for photo in sources {
            if self.thumbnail_queued.len() >= THUMBNAIL_QUEUE_LIMIT {
                break;
            }
            if self
                .library_thumbnails
                .contains_key(&thumbnail_key(&photo.image))
                || self
                    .thumbnail_unavailable
                    .contains(&thumbnail_key(&photo.image))
                || self.thumbnail_queued.contains(&thumbnail_key(&photo.image))
            {
                continue;
            }
            let id = thumbnail_key(&photo.image);
            let request = thumbnail_request(&photo.image, &self.content);
            self.thumbnail_keys.insert(id.clone(), request.key());
            self.thumbnail_service.request(request);
            self.thumbnail_queued.insert(id);
        }
    }

    fn withdraw_thumbnail_requests(&mut self, still_visible: &[String]) {
        let service = &self.thumbnail_service;
        self.thumbnail_keys.retain(|id, key| {
            let keep = still_visible.contains(id);
            if !keep {
                service.cancel(key);
            }
            keep
        });
    }
}

impl Photos {
    pub(super) fn schedule_people_scan_batch(
        &mut self,
        invocation: tilo_actions::InvocationId,
        library: &crate::model::PhotoLibrary,
    ) -> Result<(), String> {
        if !self.people_controller.privacy().recognition_enabled {
            return Err(tr("photos-people-error-off"));
        }
        let config = agent_provider_config::ProviderConfig::load();
        self.people_controller.discover_face_capability(&config);
        let capability = tilo_vision::discover_local_face_capability(&config).ok_or_else(|| {
            "No verified local face model is ready. Install and start one in Settings.".to_owned()
        })?;
        let locally_available: Vec<_> = library
            .photos
            .values()
            .filter(|photo| crate::model::bytes_of(&photo.image, &self.content).is_some())
            .cloned()
            .map(|photo| (photo.id, photo.image))
            .collect();
        let anchor_ids = self.people_controller.reconciliation_anchor_photo_ids(2);
        let anchor_count = anchor_ids
            .iter()
            .filter(|id| {
                locally_available
                    .iter()
                    .any(|(available, _)| available == *id)
            })
            .count()
            .min(tilo_vision::MAX_FACE_BATCH.saturating_sub(1));
        let (start, scan_len) = self
            .people_scan_progress
            .batch_range(
                locally_available.len(),
                tilo_vision::MAX_FACE_BATCH.saturating_sub(anchor_count),
            )
            .ok_or_else(|| tr("photos-people-error-no-photos"))?;
        let mut photos: Vec<_> = locally_available
            .iter()
            .skip(start)
            .take(scan_len)
            .cloned()
            .collect();
        for id in anchor_ids {
            if photos.iter().any(|(photo_id, _)| photo_id == &id) {
                continue;
            }
            if let Some(anchor) = locally_available
                .iter()
                .find(|(photo_id, _)| photo_id == &id)
            {
                photos.push(anchor.clone());
            }
        }
        let Some((_, first)) = photos.first() else {
            return Err(tr("photos-people-error-no-photos"));
        };
        let sender = self
            .analysis_sender()
            .ok_or_else(|| "local People recognition is unavailable on this device".to_owned())?;
        sender
            .try_send(AnalysisJob {
                invocation,
                photo_id: "photos.people".to_owned(),
                image: first.clone(),
                kind: AnalysisJobKind::People {
                    capability,
                    images: photos,
                },
            })
            .map_err(|_| tr("photos-analysis-error-busy"))?;
        "running".clone_into(&mut self.people_scan_progress.status);
        self.people_scan_progress.last_error = None;
        self.people_scan_progress.last_library_size = locally_available.len();
        self.save_people_scan_progress();
        self.active_analysis.insert(invocation);
        self.active_people_scans.insert(invocation);
        self.pending_people_scans.insert(
            invocation,
            PendingPeopleScan {
                start,
                scanned: scan_len,
                total: locally_available.len(),
            },
        );
        Ok(())
    }
}
