//! An export that runs a step at a time on the UI thread, for a build
//! without the `GStreamer` encoder: a browser page, whose one thread may
//! never wait.
//!
//! Each step composites at most one frame through the same GPU compositor
//! the preview uses (so the file is what the preview shows), hands a frame
//! whose pixels have come back to a [`media::edit::ClipEncoder`], and
//! returns. A frame whose sources have not reached their exact picture yet is
//! composited again on a later step — never written with a stand-in. When
//! every frame is encoded the file is written to the destination the person
//! chose before the render began: a page saves by contents, and its Save
//! dialog needs the press that started the export, not one a minute later.

use std::sync::atomic::Ordering;

use media::edit::{ClipEncoder, EncoderState};

use super::{CANCELLED, ExportSettings, ExportStatus, JobState, RenderPlan, output_dims};
use crate::gpu::seek_source::{Fetch, SeekFrameSource};
use crate::gpu::{HeadlessCompositor, PendingFrame};
use crate::model::Project;

/// How many frames may wait in the encoder before compositing pauses.
const BACKLOG: u32 = 8;

/// Seconds between key frames, so a player can seek in the file.
const KEY_EVERY_S: f64 = 2.0;

/// A render in progress; [`Self::step`] advances it.
pub(super) struct SteppedRender {
    project: Project,
    plan: RenderPlan,
    width: u32,
    height: u32,
    fps: f64,
    compositor: Option<HeadlessCompositor>,
    sources: SeekFrameSource,
    encoder: ClipEncoder,
    destination: ui::portal::SaveDestination,
    /// The next frame to composite, and the composite of it on its way back.
    next: u64,
    pending: Option<PendingFrame>,
    /// Since when the next frame has been waiting for its sources.
    waiting_since: Option<ui::time::Instant>,
    saving: Option<ui::portal::SaveChannel>,
    patience: std::time::Duration,
}

impl SteppedRender {
    /// Begin rendering `project` for `destination`.
    ///
    /// # Errors
    /// Why the render cannot start here.
    pub(super) fn start(
        project: &Project,
        settings: ExportSettings,
        destination: ui::portal::SaveDestination,
        state: &JobState,
    ) -> Result<Self, String> {
        let missing = super::unreadable_sources(project);
        if !missing.is_empty() {
            return Err(super::unreadable_reason(&missing));
        }
        let (width, height) = output_dims(&settings, project);
        let fps = settings.frame_rate.fps();
        let plan = RenderPlan::new(project, fps);
        state.total.store(plan.total, Ordering::Relaxed);
        let encoder = ClipEncoder::start(
            width,
            height,
            fps,
            settings.resolution.target_mbps() * 1_000_000.0,
        )
        .map_err(|why| format!("Cannot export: {why}"))?;
        Ok(Self {
            project: project.clone(),
            plan,
            width,
            height,
            fps,
            compositor: None,
            sources: SeekFrameSource::new(project, Fetch::Exact)
                .strict()
                .with_max_side(width.max(height)),
            encoder,
            destination,
            next: 0,
            pending: None,
            waiting_since: None,
            saving: None,
            patience: state.frame_patience,
        })
    }

    /// Advance the render without waiting; the outcome lands in `state`.
    pub(super) fn step(&mut self, state: &JobState) {
        if let Err(status) = self.advance(state)
            && let Ok(mut slot) = state.outcome.lock()
        {
            *slot = Some(status);
        }
    }

    fn advance(&mut self, state: &JobState) -> Result<(), ExportStatus> {
        let failed = |reason: String| ExportStatus::Failed { reason };
        if self.saving.is_none() && state.cancel.load(Ordering::Relaxed) {
            return Err(failed(CANCELLED.to_string()));
        }
        if let Some(saving) = &self.saving {
            return match saving.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => Ok(()),
                Ok(ui::portal::SaveOutcome::Saved) => Err(ExportStatus::Done {
                    path: if crate::audio::has_audio(&self.project) {
                        // Said rather than left for the person to discover.
                        format!("{} (picture only, without sound)", self.destination.name())
                    } else {
                        self.destination.name().to_string()
                    },
                }),
                Ok(ui::portal::SaveOutcome::Cancelled) => Err(failed(CANCELLED.to_string())),
                Ok(ui::portal::SaveOutcome::Failed(why)) => {
                    Err(failed(format!("The video could not be saved: {why}")))
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(failed(
                    "The video could not be saved: the save stopped answering".into(),
                )),
            };
        }
        match self.encoder.state() {
            EncoderState::Opening | EncoderState::Finishing => return Ok(()),
            EncoderState::Failed(why) => return Err(failed(format!("Cannot export: {why}"))),
            EncoderState::Done => {
                let bytes = self.encoder.take_bytes();
                self.saving = Some(self.destination.write(bytes));
                return Ok(());
            }
            EncoderState::Ready => {}
        }
        if let Some(pending) = &self.pending {
            match pending.try_take() {
                None => return Ok(()),
                Some(Err(why)) => return Err(failed(format!("Cannot export: {why}"))),
                Some(Ok(rgba)) => {
                    let key = self.next.is_multiple_of(self.key_interval());
                    if !self.encoder.encode(&rgba, self.next, key) {
                        // The encoder says why on the next step.
                        return Ok(());
                    }
                    self.pending = None;
                    self.waiting_since = None;
                    self.next += 1;
                    state.wrote(self.next);
                }
            }
        }
        if self.next >= self.plan.total {
            self.encoder.finish();
            return Ok(());
        }
        if self.encoder.backlog() > BACKLOG {
            return Ok(());
        }
        if self.compositor.is_none() {
            self.compositor =
                Some(HeadlessCompositor::new().map_err(|e| failed(format!("Cannot export: {e}")))?);
        }
        let Some(compositor) = self.compositor.as_mut() else {
            return Ok(());
        };
        let at = self.plan.time_of(self.next);
        let pending = compositor.frame_later(
            &self.project,
            at,
            self.width,
            self.height,
            &mut self.sources,
        );
        if let Some(media) = self.sources.failure() {
            let name = self
                .project
                .media(media)
                .map_or_else(|| "a source".to_string(), |m| format!("\"{}\"", m.name));
            return Err(failed(format!(
                "Cannot export: {name} cannot be decoded in this browser."
            )));
        }
        if let Some(&(media, source_time)) = self.sources.missing_pictures().first() {
            // Not every source holds this frame's picture yet: ask again on
            // the next step, when the seekers have moved.
            let since = *self.waiting_since.get_or_insert_with(ui::time::now);
            if since.elapsed() > self.patience {
                return Err(failed(super::no_picture_reason(
                    &self.project,
                    media,
                    source_time,
                    self.patience,
                )));
            }
            return Ok(());
        }
        self.pending = Some(pending);
        Ok(())
    }

    /// Frames between key frames.
    fn key_interval(&self) -> u64 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a small positive whole number of frames"
        )]
        let frames = (self.fps * KEY_EVERY_S).round().max(1.0) as u64;
        frames
    }
}
