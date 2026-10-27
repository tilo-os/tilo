//! Production execution for the shell capture studio.
//!
//! The widget emits semantic [`CaptureIntent`]s. This module is the compositor
//! side: it chooses the unclipped live source, builds the requested composition,
//! performs GPU readback, and writes/copies the resulting pixels. Keeping this
//! here (rather than in the widget) is what lets a full-app capture include
//! content outside the physical monitor.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "capture geometry is clamped to finite positive pixel dimensions before conversion"
)]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use ui::num::f64_of_usize;

use ui::floating_surface::{FloatingSource, FloatingSurfaceRequest};
use ui::offscreen::OffscreenRenderer;
use ui::widget::{
    AudioCaptureMode, CaptureDestination, CaptureIntent, CaptureMode, CapturePreview, PipLayout,
    RecordingCommand, ReplayCommand, ScreenshotCommand, ScreenshotSendTarget, SourceMode,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Size};
use vello::peniko::{BlendMode, Fill};

use crate::compositor::frame::FrameBuilder;
use crate::shell::capture_surface::{
    CaptureMirrorHandoffLayer, CaptureSurfaceKind, CaptureSurfaceRequest,
};

const MAX_CAPTURE_EDGE: f64 = 8_192.0;
const GRID_GAP: f64 = 16.0;
const STACK_OFFSET: f64 = 28.0;
const REPLAY_FPS: u32 = 30;
const REPLAY_SECONDS: usize = 30;
const REPLAY_FRAMES: usize = REPLAY_SECONDS * REPLAY_FPS as usize;
const REPLAY_KEYFRAME_INTERVAL: u32 = REPLAY_FPS * 2;
const REPLAY_MAX_VIDEO_BYTES: usize = 128 * 1024 * 1024;
const AUDIO_BYTES_PER_VIDEO_FRAME: usize = 48_000 / REPLAY_FPS as usize * 4;
const REPLAY_MAX_AUDIO_BYTES: usize = REPLAY_SECONDS * 48_000 * 4;

pub fn recording_audio_availability() -> (bool, bool) {
    let availability = super::audio_capture::availability();
    (availability.system, availability.microphone)
}

#[derive(Debug, Clone, PartialEq)]
pub enum CaptureOutcome {
    Saved(PathBuf),
    Copied {
        width: u32,
        height: u32,
    },
    Floated {
        id: String,
    },
    Pinned {
        id: String,
    },
    FloatingClosed {
        id: String,
    },
    RecordingStarted(PathBuf),
    RecordingPaused {
        path: PathBuf,
        paused: bool,
    },
    RecordingAudioLost {
        path: PathBuf,
        reason: String,
    },
    RecordingStopped {
        path: PathBuf,
        frames: u64,
    },
    RecordingOpened(PathBuf),
    RecordingRevealed(PathBuf),
    ReplayStarted,
    ReplaySaved {
        path: PathBuf,
        seconds: u8,
    },
    ReplayStopped,
    ReplayAudioLost(String),
    ScreenshotOpened(PathBuf),
    ScreenshotMarkupOpened(PathBuf),
    ScreenshotMarkupClosed,
    ScreenshotRevealed(PathBuf),
    /// "Send…" chose a device for the saved screenshot, and the capture
    /// worker read it; the compositor offers it over the device fabric.
    ScreenshotSendRequested {
        device: String,
        screenshot: crate::shell::screenshot_send::PreparedScreenshot,
    },
    /// "Send…" finished off the loop: the screenshot was copied into the
    /// workspace files, or why not.
    ScreenshotSent(Result<crate::shell::screenshot_send::SendReceipt, String>),
    /// A Screenshot portal selection completed in trusted shell chrome. Kept
    /// distinct from a keyboard-initiated capture so its URI is returned only
    /// to the authenticated request that armed the selector.
    PortalScreenshotSaved(PathBuf),
    PortalScreenshotCancelled,
    /// What was chosen has nothing to capture now (its window closed, or
    /// was let go with its workspace): the person is told plainly and
    /// offered the screen instead.
    Uncapturable(SourceMode),
    Failed(String),
}

/// Why a capture found nothing to read: its source resolved to no rect.
/// Matched by `failure_of` to tell the person plainly.
pub const NOTHING_TO_CAPTURE: &str = "the selected source has no paintable pixels";

/// The outcome for a failed capture of `source`: [`CaptureOutcome::Uncapturable`]
/// when nothing was there to read, else the failure itself.
fn failure_of(source: SourceMode, error: String) -> CaptureOutcome {
    if error == NOTHING_TO_CAPTURE {
        CaptureOutcome::Uncapturable(source)
    } else {
        CaptureOutcome::Failed(error)
    }
}

/// What a capture job hands back to the loop: applied there, in submission
/// order, to the executor and the frame being built, and turned into the
/// outcome the person sees.
struct CaptureDone(ApplyOnLoop);

/// The loop's half of a capture job.
type ApplyOnLoop =
    Box<dyn FnOnce(&mut CaptureExecutor, &mut FrameBuilder) -> Option<CaptureOutcome> + Send>;

impl CaptureDone {
    fn new(
        apply: impl FnOnce(&mut CaptureExecutor, &mut FrameBuilder) -> Option<CaptureOutcome>
        + Send
        + 'static,
    ) -> Self {
        Self(Box::new(apply))
    }

    fn outcome(outcome: CaptureOutcome) -> Self {
        Self::new(move |_, _| Some(outcome))
    }

    /// `result` as the outcome, or the failure it carries.
    fn result(result: Result<CaptureOutcome, String>) -> Self {
        Self::outcome(result.unwrap_or_else(CaptureOutcome::Failed))
    }
}

impl super::capture_io::JobResult for CaptureDone {
    fn panicked(message: String) -> Self {
        Self::outcome(CaptureOutcome::Failed(message))
    }
}

/// The newest finished artifact of one kind. Set by the capture worker only
/// once its file is complete, and read by later jobs on the same worker, so
/// "the last screenshot" is never a file still being written.
#[derive(Clone, Default)]
struct LastArtifact(std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>);

impl LastArtifact {
    fn get(&self) -> Option<PathBuf> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set(&self, path: PathBuf) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path);
    }
}

/// Who hears that a screenshot file was written.
enum ScreenshotReceipt {
    /// The shell's own capture: the receipt chrome and a `Saved` outcome.
    Shell,
    /// A portal caller's interactive selection, answered through the
    /// `PortalScreenshotSaved` outcome.
    PortalSelection,
    /// A non-interactive portal request, answered once the file is complete
    /// (or the capture failed). Runs on the capture worker.
    PortalRequest(Box<dyn FnOnce(Result<PathBuf, String>) + Send>),
}

impl ScreenshotReceipt {
    /// The capture failed before a file was started.
    fn failed(self, source: SourceMode, error: String) -> Option<CaptureOutcome> {
        match self {
            Self::PortalRequest(deliver) => {
                deliver(Err(error));
                None
            }
            Self::Shell | Self::PortalSelection => Some(failure_of(source, error)),
        }
    }
}

/// A rendered screenshot whose file is still to be written.
struct ScreenshotToWrite {
    path: PathBuf,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    preview: CapturePreview,
}

#[derive(Default)]
pub struct CaptureExecutor {
    sequence: u64,
    live_mirrors: Vec<LiveMirror>,
    recording: Option<RecordingSession>,
    last_recording: LastArtifact,
    last_screenshot: LastArtifact,
    /// Finishing recordings and writing or reading screenshots: work that
    /// waits on the disk or a muxer, kept off the loop.
    io: super::capture_io::CaptureIo<CaptureDone>,
    replay: Option<ReplaySession>,
    /// Requests from sandboxed app service connections. Kept separate from
    /// in-process widget handoffs: external clients cannot participate in the
    /// shell's second widget draw pass, so call/navigation sources become live
    /// mirrors until native sub-surface ownership is negotiated.
    external_floating: Vec<FloatingSurfaceRequest>,
    external_updates: Vec<(String, Rect, String, Rect)>,
    external_dismissals: Vec<String>,
    portal_screenshot_armed: bool,
    /// Whether this executor holds every window's frame still
    /// ([`crate::shell::foreign::ForeignFrameStore::hold_frames`]): from the
    /// frame a screenshot opens on, whatever its target, through the frame
    /// that confirms or cancels it. Tiles, the screen and the capture all
    /// draw the frames of that moment, while windows may still move.
    holds_windows: bool,
    /// Live window previews for the switcher and the overview, rendered
    /// here because this is where live geometry and a GPU meet.
    window_previews: super::window_previews::WindowPreviewRenderer,
    /// The folder the Screenshots folder is made in; `None`: the person's
    /// Pictures folder (else their home).
    pictures: Option<PathBuf>,
}

struct LiveMirror {
    id: String,
    title: String,
    intent: CaptureIntent,
    renderer: Option<OffscreenRenderer>,
    preferred_size: Option<Size>,
    /// App-local semantic binding re-resolved against the owner's current
    /// compositor surface each frame. Generic `PiP` therefore follows strip
    /// movement and window resizing instead of sampling stale desktop pixels.
    tracking_source: Option<FloatingSource>,
    /// In-process app mirrors expose their activity edge through the shared
    /// widget tree. Shell-created and service-owned mirrors have separate
    /// lifecycles and must not be retired by that set.
    close_when_app_inactive: bool,
    /// Consumed on first presentation so the `PiP` grows out of the selected
    /// pixels instead of appearing as an unrelated centered window.
    spawn_from: Option<Rect>,
}

struct RecordingSession {
    intent: CaptureIntent,
    renderer: OffscreenRenderer,
    encoder: crate::network::encoder::VideoEncoder,
    output: RecordingOutput,
    path: PathBuf,
    width: u32,
    height: u32,
    last_frame: Instant,
    frames: u64,
    paused: bool,
    audio_tracks: Vec<ActiveAudioTrack>,
    audio_warning: Option<String>,
}

struct ActiveAudioTrack {
    source: super::audio_capture::AudioSource,
    capture: super::audio_capture::AudioCapture,
    mux_index: usize,
}

struct ReplaySession {
    intent: CaptureIntent,
    renderer: OffscreenRenderer,
    encoder: crate::network::encoder::VideoEncoder,
    width: u32,
    height: u32,
    last_frame: Instant,
    frames: VecDeque<BufferedReplayFrame>,
    video_bytes: usize,
    audio_tracks: Vec<ReplayAudioTrack>,
    audio_warning: Option<String>,
}

struct BufferedReplayFrame {
    /// Shared so saving "recent" hands the ring to the capture worker
    /// without copying it.
    access_unit: std::sync::Arc<[u8]>,
    keyframe: bool,
}

struct ReplayAudioTrack {
    source: super::audio_capture::AudioSource,
    capture: super::audio_capture::AudioCapture,
    pcm: VecDeque<u8>,
}

enum RecordingOutput {
    Mp4(tilo_callstream::mp4_recording::Mp4Recording),
}

impl RecordingOutput {
    fn new(
        path: &Path,
        codec: media::StreamCodec,
        width: u32,
        height: u32,
        fps: u32,
        audio_track_names: Vec<String>,
    ) -> Self {
        Self::Mp4(tilo_callstream::mp4_recording::Mp4Recording::new(
            path.to_path_buf(),
            codec,
            width,
            height,
            fps,
            audio_track_names,
        ))
    }

    fn write_access_unit(&mut self, bytes: &[u8]) -> Result<(), String> {
        match self {
            Self::Mp4(output) => output.write_access_unit(bytes),
        }
    }

    fn write_pcm_audio(&mut self, track_index: usize, bytes: &[u8]) -> Result<(), String> {
        let Self::Mp4(output) = self;
        output.write_pcm_audio(track_index, bytes)
    }

    fn finish(self, path: &Path) -> Result<(), String> {
        match self {
            Self::Mp4(output) => {
                let _ = path;
                output.finish()
            }
        }
    }
}

impl CaptureExecutor {
    /// Reserve the next trusted Capture Studio screenshot intent for a portal
    /// request. The native bridge allows only one such selection at a time.
    pub fn arm_portal_screenshot(&mut self) -> bool {
        if self.portal_screenshot_armed {
            return false;
        }
        self.portal_screenshot_armed = true;
        true
    }

    pub fn cancel_portal_screenshot(&mut self) {
        self.portal_screenshot_armed = false;
    }
    /// Capture a portal-requested still from the compositor's clean scene.
    ///
    /// This deliberately goes through the same Vello readback and PNG writer
    /// as the shell's Capture Studio.  In particular, callers never receive a
    /// Wayland surface buffer (which could contain pixels outside the selected
    /// output) and the result is a finished file before its URI is returned.
    ///
    /// The pixels are read back now; the file is written on the capture
    /// worker, and `deliver` is called there with its path once it is
    /// complete, or with the error when no output is captureable or
    /// compositor readback and private PNG persistence cannot complete.
    pub fn capture_portal_screenshot(
        &mut self,
        frame_builder: &mut FrameBuilder,
        clean_screen_scene: &Scene,
        target: crate::portal::desktop_services::ScreenshotTarget,
        include_pointer: bool,
        deliver: impl FnOnce(Result<PathBuf, String>) + Send + 'static,
    ) {
        let (viewport_width, viewport_height) = frame_builder.viewport();
        if !viewport_width.is_finite()
            || !viewport_height.is_finite()
            || viewport_width <= 0.0
            || viewport_height <= 0.0
        {
            deliver(Err("the compositor has no captureable output".into()));
            return;
        }

        // The portal frontend only supplies an intent, never app-controlled
        // geometry.  Area and window selection are compositor UX choices; at
        // this execution boundary the chosen region has already been reduced
        // to an output-safe rectangle.  A non-interactive request defaults to
        // the visible output, as required by Screenshot v2.
        let region = Rect::new(0.0, 0.0, viewport_width, viewport_height);
        let source = match target {
            crate::portal::desktop_services::ScreenshotTarget::Screen
            | crate::portal::desktop_services::ScreenshotTarget::Area => SourceMode::Region,
            crate::portal::desktop_services::ScreenshotTarget::Window
            | crate::portal::desktop_services::ScreenshotTarget::ActiveWindow => {
                // A window target is resolved by trusted chrome, not by the
                // requesting app.  Until a particular shell window is picked,
                // capture the focused visible output rather than fabricating a
                // foreign application's off-screen content.
                SourceMode::Display
            }
        };
        let intent = CaptureIntent {
            mode: CaptureMode::Screenshot,
            source,
            destination: CaptureDestination::FileAndClipboard,
            regions: vec![region],
            anchors: Vec::new(),
            include_pointer,
            audio: AudioCaptureMode::None,
            pip_layout: PipLayout::Auto,
            targets: Vec::new(),
            recording_command: None,
            replay_command: None,
            screenshot_command: None,
        };
        // A file destination answers only through `deliver`.
        let _ = self.execute_screenshot(
            frame_builder,
            clean_screen_scene,
            &intent,
            ScreenshotReceipt::PortalRequest(Box::new(deliver)),
        );
    }

    /// Pick the compositor pixel currently under the trusted seat pointer.
    /// The coordinate is clamped to the rendered output before indexing, so a
    /// stale pointer position can never address outside the readback buffer.
    pub fn pick_portal_color(
        &mut self,
        frame_builder: &mut FrameBuilder,
        clean_screen_scene: &Scene,
    ) -> Result<(f64, f64, f64), String> {
        let (width, height) = frame_builder.scene_size();
        let width = width.round().clamp(1.0, MAX_CAPTURE_EDGE) as u32;
        let height = height.round().clamp(1.0, MAX_CAPTURE_EDGE) as u32;
        let pointer = frame_builder
            .capture_pointer_position()
            .map(|p| frame_builder.layout_point_to_scene(p))
            .ok_or_else(|| "move the pointer over the colour to pick it".to_string())?;
        let x = pointer
            .x
            .floor()
            .clamp(0.0, f64::from(width.saturating_sub(1))) as usize;
        let y = pointer
            .y
            .floor()
            .clamp(0.0, f64::from(height.saturating_sub(1))) as usize;
        let mut renderer = OffscreenRenderer::try_new(width, height)
            .map_err(|error| format!("colour picker renderer {width}×{height}: {error}"))?;
        let source = frame_builder.capture_scene(clean_screen_scene);
        let rgba = render_capture_rgba(&mut renderer, &source);
        let offset = y
            .checked_mul(width as usize)
            .and_then(|pixels| pixels.checked_add(x))
            .and_then(|pixel| pixel.checked_mul(4))
            .ok_or_else(|| "colour picker coordinate overflow".to_string())?;
        let rgb: [u8; 3] = rgba
            .get(offset..offset.saturating_add(3))
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| "colour picker readback was incomplete".to_string())?;
        Ok((
            f64::from(rgb[0]) / 255.0,
            f64::from(rgb[1]) / 255.0,
            f64::from(rgb[2]) / 255.0,
        ))
    }

    /// Render the current composed desktop once for all active `PipeWire` portal
    /// nodes.  The producer retains only this newest immutable frame.
    pub fn render_portal_screencast_frame(
        &mut self,
        frame_builder: &mut FrameBuilder,
        scene: &Scene,
    ) -> Result<(u32, u32, std::sync::Arc<[u8]>), String> {
        let (width, height) = frame_builder.scene_size();
        let width = width.round().clamp(1.0, MAX_CAPTURE_EDGE) as u32;
        let height = height.round().clamp(1.0, MAX_CAPTURE_EDGE) as u32;
        let mut renderer = OffscreenRenderer::try_new(width, height)
            .map_err(|error| format!("screen-share renderer {width}×{height}: {error}"))?;
        let source = frame_builder.capture_scene(scene);
        let rgba = render_capture_rgba(&mut renderer, &source);
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "screen-share frame dimensions overflow".to_string())?;
        if rgba.len() != expected {
            return Err("screen-share renderer returned an incomplete frame".into());
        }
        Ok((width, height, std::sync::Arc::from(rgba)))
    }

    pub fn queue_external_floating(&mut self, request: FloatingSurfaceRequest) {
        self.external_floating.push(request);
    }

    pub fn update_external_floating_region(
        &mut self,
        id: String,
        region: Rect,
        app_id: String,
        local_region: Rect,
    ) {
        self.external_updates
            .push((id, region, app_id, local_region));
    }

    pub fn dismiss_external_floating(&mut self, id: String) {
        self.external_dismissals.push(id);
    }

    /// Drain picker intents and advance continuous recording/mirror work.
    pub fn execute_ready(
        &mut self,
        frame_builder: &mut FrameBuilder,
        clean_screen_scene: &mut Scene,
    ) -> (Vec<CaptureOutcome>, bool) {
        self.hold_windows_while_choosing(frame_builder);
        let (outcomes, overlays_appended) =
            self.execute_ready_frame(frame_builder, clean_screen_scene);
        // The confirming (or cancelling) frame has now read the held frames.
        if !frame_builder.capture_choosing_screenshot() {
            self.release_held_windows(frame_builder);
        }
        (outcomes, overlays_appended)
    }

    /// Hold every window still from the first frame a screenshot is chosen
    /// on: client windows' frames and the windows of apps the shell runs
    /// itself, as that frame drew them. Changing the target never starts or
    /// ends the hold.
    fn hold_windows_while_choosing(&mut self, frame_builder: &mut FrameBuilder) {
        if frame_builder.capture_choosing_screenshot() && !self.holds_windows {
            crate::shell::foreign::store().hold_frames();
            frame_builder.hold_windows();
            self.holds_windows = true;
        }
    }

    /// End the hold: the windows' live frames show again from the next
    /// frame, and the frames and client buffers only the hold kept go back.
    fn release_held_windows(&mut self, frame_builder: &mut FrameBuilder) {
        if !std::mem::take(&mut self.holds_windows) {
            return;
        }
        crate::shell::foreign::store().release_frames();
        frame_builder.release_windows();
        // Nothing else may ask for the frame that shows the live windows.
        frame_builder.with_chrome(crate::shell::chrome_state::ChromeState::bump);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one frame: capture, scene, present and retirement stay in order"
    )]
    fn execute_ready_frame(
        &mut self,
        frame_builder: &mut FrameBuilder,
        clean_screen_scene: &mut Scene,
    ) -> (Vec<CaptureOutcome>, bool) {
        let closed = frame_builder.take_closed_capture_mirrors();
        let mut outcomes: Vec<CaptureOutcome> = closed
            .iter()
            .cloned()
            .map(|id| CaptureOutcome::FloatingClosed { id })
            .collect();
        let finished = self.io.take_finished();
        outcomes.extend(self.apply_finished(frame_builder, finished));
        for id in &closed {
            frame_builder.on_floating_surface_host_message(
                &ui::floating_surface::FloatingSurfaceHostMessage::Closed { id: id.clone() },
            );
        }
        self.live_mirrors
            .retain(|mirror| !closed.iter().any(|id| id == &mirror.id));

        let dismissals = std::mem::take(&mut self.external_dismissals);
        self.live_mirrors
            .retain(|mirror| !dismissals.iter().any(|id| id == &mirror.id));

        let active_app_floats: std::collections::HashSet<String> = frame_builder
            .active_floating_surface_ids()
            .into_iter()
            .collect();
        let app_closed = self
            .live_mirrors
            .iter()
            .filter(|mirror| {
                mirror.close_when_app_inactive && !active_app_floats.contains(&mirror.id)
            })
            .map(|mirror| mirror.id.clone())
            .collect::<Vec<_>>();
        self.live_mirrors
            .retain(|mirror| !app_closed.iter().any(|id| id == &mirror.id));
        for id in &app_closed {
            frame_builder.on_floating_surface_host_message(
                &ui::floating_surface::FloatingSurfaceHostMessage::Closed { id: id.clone() },
            );
        }
        outcomes.extend(
            app_closed
                .into_iter()
                .map(|id| CaptureOutcome::FloatingClosed { id }),
        );

        let mut app_surface_adopted = false;
        let mirrors_before: Vec<String> = self
            .live_mirrors
            .iter()
            .map(|mirror| mirror.id.clone())
            .collect();
        for (id, region, app_id, local_region) in std::mem::take(&mut self.external_updates) {
            if let Some(mirror) = self.live_mirrors.iter_mut().find(|mirror| mirror.id == id) {
                mirror.intent.regions = vec![region];
                mirror.tracking_source = Some(updated_local_tracking_source(
                    mirror.tracking_source.take(),
                    app_id,
                    local_region,
                ));
            }
        }
        for (id, region) in frame_builder.take_floating_surface_source_updates() {
            if let Some(mirror) = self
                .live_mirrors
                .iter_mut()
                .find(|mirror| mirror.id == id && mirror.close_when_app_inactive)
            {
                mirror.intent.regions = vec![region];
            }
        }

        for request in std::mem::take(&mut self.external_floating) {
            match self.adopt_floating_request(frame_builder, request, false) {
                Ok(Some(id)) => outcomes.push(CaptureOutcome::Floated { id }),
                Ok(None) => {}
                Err(error) => outcomes.push(CaptureOutcome::Failed(error)),
            }
        }
        for request in frame_builder.take_floating_surface_requests() {
            let public_id = request.id.clone();
            let adopted_rect = request.initial_rect.map_or(
                [0.0, 0.0, 1.0, 1.0],
                ui::floating_surface::protocol_rect_array,
            );
            // Calls use the exact app widget in the shell's second paint pass,
            // retaining controls and pointer capture. Other app-region/media
            // sources use the generic live mirror host below.
            if request.role.uses_app_owned_host() {
                frame_builder.on_floating_surface_host_message(
                    &ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
                        id: public_id,
                        rect: adopted_rect,
                    },
                );
                tracing::debug!(id = %request.id, ?request.role, "shell adopted live app-owned surface");
                app_surface_adopted = true;
                continue;
            }
            match self.adopt_floating_request(frame_builder, request, true) {
                Ok(Some(id)) => {
                    frame_builder.on_floating_surface_host_message(
                        &ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
                            id: public_id,
                            rect: adopted_rect,
                        },
                    );
                    outcomes.push(CaptureOutcome::Floated { id });
                }
                Ok(None) => {}
                Err(error) => {
                    frame_builder.on_floating_surface_host_message(
                        &ui::floating_surface::FloatingSurfaceHostMessage::Closed { id: public_id },
                    );
                    outcomes.push(CaptureOutcome::Failed(error));
                }
            }
        }

        for command in frame_builder.host_mut().take_screenshot_markup_commands() {
            let outcome = match command {
                ui::widget::ScreenshotMarkupCommand::Save(export) => {
                    match next_screenshot_path(self.pictures.clone(), self.sequence) {
                        Ok(path) => {
                            self.sequence = self.sequence.saturating_add(1);
                            // The markup stays open until its pixels are on
                            // disk, so a failed save loses no edits.
                            let last = self.last_screenshot.clone();
                            self.io.submit(move || {
                                let written =
                                    write_png(&path, export.width, export.height, &export.rgba);
                                if written.is_ok() {
                                    last.set(path.clone());
                                }
                                CaptureDone::new(move |_, frame_builder| {
                                    Some(match written {
                                        Ok(()) => {
                                            frame_builder.host_mut().close_screenshot_markup();
                                            CaptureOutcome::Saved(path)
                                        }
                                        Err(error) => CaptureOutcome::Failed(error),
                                    })
                                })
                            });
                            None
                        }
                        Err(error) => {
                            self.sequence = self.sequence.saturating_add(1);
                            Some(CaptureOutcome::Failed(error))
                        }
                    }
                }
                ui::widget::ScreenshotMarkupCommand::Copy(export) => {
                    if ui::clipboard_copy_image(
                        export.width as usize,
                        export.height as usize,
                        &export.rgba,
                    )
                    .outcome()
                        == Some(true)
                    {
                        Some(CaptureOutcome::Copied {
                            width: export.width,
                            height: export.height,
                        })
                    } else {
                        Some(CaptureOutcome::Failed(
                            "the clipboard refused marked-up pixels".into(),
                        ))
                    }
                }
                ui::widget::ScreenshotMarkupCommand::OpenExternal => {
                    self.open_last_screenshot();
                    None
                }
                ui::widget::ScreenshotMarkupCommand::Cancel => {
                    frame_builder.host_mut().close_screenshot_markup();
                    Some(CaptureOutcome::ScreenshotMarkupClosed)
                }
            };
            outcomes.extend(outcome);
        }

        let intents = frame_builder.take_capture_intents();
        if self.portal_screenshot_armed
            && intents.is_empty()
            && !frame_builder.capture_studio_open()
        {
            self.portal_screenshot_armed = false;
            outcomes.push(CaptureOutcome::PortalScreenshotCancelled);
        }
        for intent in intents {
            let portal_intent = self.portal_screenshot_armed
                && intent.mode == CaptureMode::Screenshot
                && intent.screenshot_command.is_none();
            if portal_intent {
                self.portal_screenshot_armed = false;
            }
            // Work that reads or writes a capture file runs on the capture
            // worker and reports on a later frame: `None` here.
            let outcome = if let Some(command) = intent.screenshot_command {
                match command {
                    ScreenshotCommand::MarkupLast => {
                        self.open_last_screenshot_markup(&intent.regions);
                        None
                    }
                    ScreenshotCommand::OpenLast => {
                        self.open_last_screenshot();
                        None
                    }
                    ScreenshotCommand::PinLast => {
                        self.pin_last_screenshot(&intent.regions);
                        None
                    }
                    ScreenshotCommand::RevealLast => {
                        self.reveal_last_screenshot();
                        None
                    }
                    ScreenshotCommand::CopyLast => {
                        self.copy_last_screenshot();
                        None
                    }
                    ScreenshotCommand::SendLast(target) => {
                        self.send_last_screenshot(
                            target,
                            crate::shell::screenshot_send::workspace_files_root(),
                        );
                        None
                    }
                }
            } else if let Some(command) = intent.replay_command {
                match command {
                    ReplayCommand::Start => Some(
                        self.start_replay(frame_builder, clean_screen_scene, intent)
                            .unwrap_or_else(CaptureOutcome::Failed),
                    ),
                    ReplayCommand::SaveRecent => {
                        self.save_replay().err().map(CaptureOutcome::Failed)
                    }
                    ReplayCommand::Stop => Some(
                        self.stop_replay(frame_builder)
                            .unwrap_or_else(CaptureOutcome::Failed),
                    ),
                }
            } else if let Some(command) = intent.recording_command {
                match command {
                    RecordingCommand::Start => {
                        let source = intent.source;
                        Some(
                            match self.start_recording(frame_builder, clean_screen_scene, intent) {
                                Ok(outcome) => outcome,
                                Err(error) => {
                                    // Clear the immediate, non-modal Preparing UI.
                                    // A failed encoder/output setup must never
                                    // leave the shell implying work is ongoing.
                                    frame_builder.set_capture_recording_active(false);
                                    failure_of(source, error)
                                }
                            },
                        )
                    }
                    RecordingCommand::Pause => Some(
                        self.set_recording_paused(frame_builder, true)
                            .unwrap_or_else(CaptureOutcome::Failed),
                    ),
                    RecordingCommand::Resume => Some(
                        self.set_recording_paused(frame_builder, false)
                            .unwrap_or_else(CaptureOutcome::Failed),
                    ),
                    RecordingCommand::Stop => self
                        .stop_recording(frame_builder)
                        .err()
                        .map(CaptureOutcome::Failed),
                    RecordingCommand::OpenLast => {
                        self.open_last_recording();
                        None
                    }
                    RecordingCommand::RevealLast => {
                        self.reveal_last_recording();
                        None
                    }
                }
            } else if intent.destination == CaptureDestination::FloatingWindow {
                let id = format!("capture-mirror:{}", self.sequence);
                self.sequence = self.sequence.saturating_add(1);
                let title = live_title(&intent);
                let spawn_from = intent_spawn_bounds(&intent);
                self.live_mirrors.push(LiveMirror {
                    id: id.clone(),
                    title,
                    intent,
                    renderer: None,
                    preferred_size: None,
                    spawn_from,
                    tracking_source: None,
                    close_when_app_inactive: false,
                });
                Some(CaptureOutcome::Floated { id })
            } else {
                let receipt = if portal_intent {
                    ScreenshotReceipt::PortalSelection
                } else {
                    ScreenshotReceipt::Shell
                };
                self.execute_screenshot(frame_builder, clean_screen_scene, &intent, receipt)
            };
            outcomes.extend(outcome);
        }

        // A picture in picture that cannot show anything is closed once,
        // with the reason, rather than failing again every frame.
        let mut broken = Vec::new();
        for mirror in &mut self.live_mirrors {
            if let Err(error) = refresh_mirror(frame_builder, clean_screen_scene, mirror) {
                tracing::warn!(%error, title = %mirror.title, "picture in picture failed");
                outcomes.push(failure_of(mirror.intent.source, error));
                broken.push(mirror.id.clone());
            }
        }
        if !broken.is_empty() {
            self.live_mirrors
                .retain(|mirror| !broken.contains(&mirror.id));
            for id in broken {
                frame_builder.host_mut().close_capture_surface(&id);
                outcomes.push(CaptureOutcome::FloatingClosed { id });
            }
        }
        if let Some(mut recording) = self.recording.take() {
            match record_frame(frame_builder, clean_screen_scene, &mut recording) {
                Ok(()) => {
                    if let Some(reason) = recording.audio_warning.take() {
                        outcomes.push(CaptureOutcome::RecordingAudioLost {
                            path: recording.path.clone(),
                            reason,
                        });
                    }
                    self.recording = Some(recording);
                }
                Err(error) => {
                    let path = recording.path.clone();
                    frame_builder.set_capture_recording_active(false);
                    outcomes.push(CaptureOutcome::Failed(format!(
                        "recording {} stopped: {error}",
                        path.display()
                    )));
                }
            }
        }
        if let Some(mut replay) = self.replay.take() {
            match record_replay_frame(frame_builder, clean_screen_scene, &mut replay) {
                Ok(()) => {
                    frame_builder.set_capture_replay_buffered_seconds(
                        (replay.frames.len() / REPLAY_FPS as usize).min(REPLAY_SECONDS) as u8,
                    );
                    if let Some(reason) = replay.audio_warning.take() {
                        outcomes.push(CaptureOutcome::ReplayAudioLost(reason));
                    }
                    self.replay = Some(replay);
                }
                Err(error) => {
                    frame_builder.set_capture_replay_active(false, AudioCaptureMode::None);
                    outcomes.push(CaptureOutcome::Failed(format!(
                        "recent capture stopped: {error}"
                    )));
                }
            }
        }
        // A just-created mirror was inserted after the clean desktop's layout
        // pass. Reconcile structural nodes now so its very first appended
        // frame has real geometry at `spawn_from` rather than Rect::ZERO.
        // This shared gate removes the blank first frame for both live PiP and
        // frozen screenshot pins. A frame that added no surface keeps the
        // layout it already has: laying everything out a second time can size
        // a window differently from the first pass and resize it every frame.
        let surface_added = app_surface_adopted
            || outcomes.iter().any(|outcome| {
                matches!(
                    outcome,
                    CaptureOutcome::Pinned { .. } | CaptureOutcome::Floated { .. }
                )
            });
        if surface_added
            || self
                .live_mirrors
                .iter()
                .any(|mirror| !mirrors_before.contains(&mirror.id))
        {
            frame_builder.relayout();
        }
        // Previews render from the frame store and a live repaint of each
        // window, never from the presented scene, so chrome over a window
        // never shows up inside its own preview.
        let _ = self
            .window_previews
            .refresh(frame_builder, std::time::Instant::now());
        // Appended only after every mirror sampled the clean desktop, so a
        // region overlapping its own PiP never feeds back recursively.
        let capture_overlays_appended = frame_builder.draw_capture_overlays(clean_screen_scene);
        (outcomes, capture_overlays_appended)
    }

    pub fn has_live_mirrors(&self) -> bool {
        !self.live_mirrors.is_empty()
    }

    /// Whether capture work finished off the loop and waits for the next
    /// frame to apply it: a finished recording's receipt, a written
    /// screenshot, a decoded one to copy or pin.
    pub fn has_finished_io(&self) -> bool {
        self.io.has_finished()
    }

    /// Wait for the capture work still running off the loop and apply it.
    /// Blocks: for offline renderers only, never the compositor loop.
    pub fn finish_io_blocking(&mut self, frame_builder: &mut FrameBuilder) -> Vec<CaptureOutcome> {
        let finished = self.io.wait_for_all();
        self.apply_finished(frame_builder, finished)
    }

    fn apply_finished(
        &mut self,
        frame_builder: &mut FrameBuilder,
        finished: Vec<CaptureDone>,
    ) -> Vec<CaptureOutcome> {
        finished
            .into_iter()
            .filter_map(|done| (done.0)(self, frame_builder))
            .collect()
    }

    /// Run `job` on the capture worker with the newest finished artifact in
    /// `last`, after every write submitted before it; `missing` is the
    /// failure when there is none.
    fn with_last_artifact(
        &mut self,
        last: &LastArtifact,
        missing: &'static str,
        job: impl FnOnce(PathBuf) -> CaptureDone + Send + 'static,
    ) {
        let last = last.clone();
        self.io.submit(move || match last.get() {
            Some(path) => job(path),
            None => CaptureDone::outcome(CaptureOutcome::Failed(missing.into())),
        });
    }

    /// Decode the last screenshot on the capture worker, then hand its path
    /// and pixels (or why there are none) to `apply` on the loop.
    fn with_last_screenshot_pixels(
        &mut self,
        missing: &'static str,
        apply: impl FnOnce(
            &mut CaptureExecutor,
            &mut FrameBuilder,
            Result<(PathBuf, DecodedScreenshot), String>,
        ) -> CaptureOutcome
        + Send
        + 'static,
    ) {
        let last = self.last_screenshot.clone();
        self.io.submit(move || {
            let decoded = last
                .get()
                .ok_or_else(|| missing.to_string())
                .and_then(|path| {
                    let pixels = read_png_rgba(&path)?;
                    Ok((path, pixels))
                });
            CaptureDone::new(move |executor, frame_builder| {
                Some(apply(executor, frame_builder, decoded))
            })
        });
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    fn adopt_floating_request(
        &mut self,
        frame_builder: &mut FrameBuilder,
        request: FloatingSurfaceRequest,
        close_when_app_inactive: bool,
    ) -> Result<Option<String>, String> {
        if self
            .live_mirrors
            .iter()
            .any(|mirror| mirror.id == request.id)
        {
            return Ok(None);
        }
        let regions = floating_source_regions(frame_builder, &request.source);
        if regions.is_empty() {
            return Err(format!("{} has no visible source to float", request.title));
        }
        let id = request.id;
        let spawn_from = region_bounds(&regions);
        self.live_mirrors.push(LiveMirror {
            id: id.clone(),
            title: request.title,
            intent: CaptureIntent {
                mode: CaptureMode::Screenshot,
                source: SourceMode::Region,
                destination: CaptureDestination::FloatingWindow,
                regions,
                anchors: Vec::new(),
                include_pointer: false,
                audio: AudioCaptureMode::None,
                pip_layout: PipLayout::Auto,
                targets: Vec::new(),
                recording_command: None,
                replay_command: None,
                screenshot_command: None,
            },
            renderer: None,
            preferred_size: Some(request.preferred_size),
            spawn_from,
            tracking_source: request.tracking_source,
            close_when_app_inactive,
        });
        Ok(Some(id))
    }

    fn start_recording(
        &mut self,
        frame_builder: &mut FrameBuilder,
        clean_screen_scene: &Scene,
        intent: CaptureIntent,
    ) -> Result<CaptureOutcome, String> {
        if self.recording.is_some() {
            return Err("a recording is already active; use Stop before starting another".into());
        }
        if self.replay.is_some() {
            return Err("turn off recent capture before starting a recording".into());
        }
        let (source, regions) = capture_source(frame_builder, clean_screen_scene, &intent);
        if regions.is_empty() {
            return Err(NOTHING_TO_CAPTURE.into());
        }
        let (_, width, height) = compose_scene(&source, &regions, intent.pip_layout)?;
        // 4:2:0 encoders take even dimensions; an odd canvas is fitted into
        // the next smaller even one.
        let (width, height) = ((width & !1).max(2), (height & !1).max(2));
        let renderer = OffscreenRenderer::try_new(width, height)
            .map_err(|error| format!("recording renderer {width}×{height}: {error}"))?;
        let encoder = tilo_callstream::mp4_recording::recording_encoder(
            width, height, 30, 8_000_000, 60, true,
        )?;
        let codec = encoder.codec();
        let path = next_recording_path(self.sequence, intent.audio != AudioCaptureMode::None)?;
        self.sequence = self.sequence.saturating_add(1);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!("create recording folder {}: {error}", parent.display())
            })?;
        }
        let audio_sources = selected_audio_sources(intent.audio);
        let mut audio_tracks = Vec::with_capacity(audio_sources.len());
        for (mux_index, source) in audio_sources.iter().copied().enumerate() {
            audio_tracks.push(ActiveAudioTrack {
                source,
                capture: super::audio_capture::AudioCapture::start(source)?,
                mux_index,
            });
        }
        let audio_track_names = audio_sources
            .iter()
            .map(|source| source.track_name().to_string())
            .collect();
        let output = RecordingOutput::new(&path, codec, width, height, 30, audio_track_names);
        self.recording = Some(RecordingSession {
            intent,
            renderer,
            encoder,
            output,
            path: path.clone(),
            width,
            height,
            last_frame: immediately_due_instant(),
            frames: 0,
            paused: false,
            audio_tracks,
            audio_warning: None,
        });
        frame_builder.set_capture_recording_active(true);
        Ok(CaptureOutcome::RecordingStarted(path))
    }

    fn set_recording_paused(
        &mut self,
        frame_builder: &mut FrameBuilder,
        paused: bool,
    ) -> Result<CaptureOutcome, String> {
        let Some(recording) = self.recording.as_mut() else {
            frame_builder.set_capture_recording_active(false);
            return Err("there is no active recording to pause or resume".into());
        };
        recording.paused = paused;
        drain_audio_tracks(recording, false)?;
        if !paused {
            // Resume with a fresh frame immediately; never make the user wait
            // for the cadence left over from before the pause.
            recording.last_frame = immediately_due_instant();
        }
        frame_builder.set_capture_recording_paused(paused);
        Ok(CaptureOutcome::RecordingPaused {
            path: recording.path.clone(),
            paused,
        })
    }

    /// Stop taking frames now and finish the file on the capture worker:
    /// draining the encoder and the muxer's end of stream wait up to 30 s.
    /// The pill reads "Stopping…" until the finished file is reported.
    fn stop_recording(&mut self, frame_builder: &mut FrameBuilder) -> Result<(), String> {
        let Some(mut recording) = self.recording.take() else {
            frame_builder.set_capture_recording_active(false);
            return Err("there is no active recording to stop".into());
        };
        // The audio captured up to the stop belongs in the file.
        let audio: Vec<(usize, Vec<u8>)> = recording
            .audio_tracks
            .iter_mut()
            .filter_map(|track| {
                track
                    .capture
                    .drain(true)
                    .ok()
                    .filter(|bytes| !bytes.is_empty())
                    .map(|bytes| (track.mux_index, bytes))
            })
            .collect();
        let RecordingSession {
            encoder,
            output,
            path,
            frames,
            ..
        } = recording;
        let last = self.last_recording.clone();
        self.io.submit(move || {
            let finished = finish_recording(encoder, output, &path, &audio);
            if finished.is_ok() {
                last.set(path.clone());
            }
            CaptureDone::new(move |_, frame_builder| {
                Some(match finished {
                    Ok(()) => {
                        frame_builder.set_capture_recording_finished();
                        CaptureOutcome::RecordingStopped { path, frames }
                    }
                    Err(error) => {
                        // Never a green receipt for an incomplete file.
                        frame_builder.set_capture_recording_active(false);
                        CaptureOutcome::Failed(error)
                    }
                })
            })
        });
        Ok(())
    }

    fn reveal_last_recording(&mut self) {
        let last = self.last_recording.clone();
        self.with_last_artifact(&last, "there is no completed recording to reveal", |path| {
            CaptureDone::result(
                path.parent()
                    .ok_or_else(|| format!("recording path has no parent: {}", path.display()))
                    .and_then(|folder| {
                        xdg_open(folder)
                            .map_err(|error| format!("show recording in Files: {error}"))
                    })
                    .map(|_| CaptureOutcome::RecordingRevealed(path.clone())),
            )
        });
    }

    fn open_last_recording(&mut self) {
        let last = self.last_recording.clone();
        self.with_last_artifact(&last, "there is no completed recording to play", |path| {
            CaptureDone::result(
                xdg_open(&path)
                    .map_err(|error| format!("play recording {}: {error}", path.display()))
                    .map(|_| CaptureOutcome::RecordingOpened(path.clone())),
            )
        });
    }

    fn start_replay(
        &mut self,
        frame_builder: &mut FrameBuilder,
        clean_screen_scene: &Scene,
        intent: CaptureIntent,
    ) -> Result<CaptureOutcome, String> {
        if self.replay.is_some() {
            return Err("recent capture is already active".into());
        }
        if self.recording.is_some() {
            return Err("stop the active recording before enabling recent capture".into());
        }
        let (source, regions) = capture_source(frame_builder, clean_screen_scene, &intent);
        let (_, width, height) = compose_scene(&source, &regions, intent.pip_layout)?;
        let (width, height) = ((width & !1).max(2), (height & !1).max(2));
        let renderer = OffscreenRenderer::try_new(width, height)
            .map_err(|error| format!("recent-capture renderer {width}×{height}: {error}"))?;
        let encoder = tilo_callstream::mp4_recording::recording_encoder(
            width,
            height,
            REPLAY_FPS,
            4_000_000,
            REPLAY_KEYFRAME_INTERVAL,
            true,
        )?;
        let mut audio_tracks = Vec::new();
        for source in selected_audio_sources(intent.audio) {
            audio_tracks.push(ReplayAudioTrack {
                source,
                capture: super::audio_capture::AudioCapture::start(source)?,
                pcm: VecDeque::new(),
            });
        }
        let audio = intent.audio;
        self.replay = Some(ReplaySession {
            intent,
            renderer,
            encoder,
            width,
            height,
            last_frame: immediately_due_instant(),
            frames: VecDeque::with_capacity(REPLAY_FRAMES + REPLAY_KEYFRAME_INTERVAL as usize),
            video_bytes: 0,
            audio_tracks,
            audio_warning: None,
        });
        frame_builder.set_capture_replay_active(true, audio);
        Ok(CaptureOutcome::ReplayStarted)
    }

    /// Save the buffered "recent" seconds: the ring is handed to the capture
    /// worker, which pushes its frames into a fresh muxer and finishes the
    /// file while buffering goes on.
    fn save_replay(&mut self) -> Result<(), String> {
        let sequence = self.sequence;
        self.sequence = self.sequence.saturating_add(1);
        let replay = self
            .replay
            .as_mut()
            .ok_or_else(|| "recent capture is not active".to_string())?;
        if replay.frames.is_empty() {
            return Err("recent capture is still warming up".into());
        }
        let include_audio = !replay.audio_tracks.is_empty();
        let path = next_replay_path(sequence, include_audio)?;
        let clip = ReplayClip {
            codec: replay.encoder.codec(),
            width: replay.width,
            height: replay.height,
            audio_track_names: replay
                .audio_tracks
                .iter()
                .map(|track| track.source.track_name().to_string())
                .collect(),
            frames: replay
                .frames
                .iter()
                .map(|frame| std::sync::Arc::clone(&frame.access_unit))
                .collect(),
            audio: replay
                .audio_tracks
                .iter()
                .map(|track| replay_audio_tail(&track.pcm, replay.frames.len()))
                .collect(),
        };
        let seconds = clip
            .frames
            .len()
            .div_ceil(REPLAY_FPS as usize)
            .min(REPLAY_SECONDS) as u8;
        let last = self.last_recording.clone();
        self.io.submit(move || {
            let saved = clip.write(&path);
            if saved.is_ok() {
                last.set(path.clone());
            }
            CaptureDone::new(move |_, frame_builder| {
                Some(match saved {
                    Ok(()) => {
                        frame_builder.set_capture_replay_saved();
                        CaptureOutcome::ReplaySaved { path, seconds }
                    }
                    Err(error) => CaptureOutcome::Failed(error),
                })
            })
        });
        Ok(())
    }

    fn stop_replay(&mut self, frame_builder: &mut FrameBuilder) -> Result<CaptureOutcome, String> {
        if self.replay.take().is_none() {
            return Err("recent capture is not active".into());
        }
        frame_builder.set_capture_replay_active(false, AudioCaptureMode::None);
        Ok(CaptureOutcome::ReplayStopped)
    }

    /// "Send…" for the last screenshot, after any screenshot still being
    /// written, so the one sent is the one the person just took. Saving into
    /// the workspace files copies onto the workspace mount, which may be
    /// remote, so the copy runs on the capture worker. A device send reads
    /// the screenshot there too, and hands what it read to the loop, which
    /// owns the device fabric.
    fn send_last_screenshot(&mut self, target: ScreenshotSendTarget, workspace_root: PathBuf) {
        let last = self.last_screenshot.clone();
        self.with_last_artifact(&last, "there is no saved screenshot to send", move |path| {
            CaptureDone::outcome(match target {
                ScreenshotSendTarget::Workspace => {
                    CaptureOutcome::ScreenshotSent(crate::shell::screenshot_send::route(
                        &path,
                        &target,
                        &mut crate::shell::screenshot_send::WorkspaceFilesBridge { workspace_root },
                    ))
                }
                ScreenshotSendTarget::Device(device) => CaptureOutcome::ScreenshotSendRequested {
                    device,
                    screenshot: crate::shell::screenshot_send::PreparedScreenshot::new(
                        crate::shell::screenshot_send::prepare_device_send(&path),
                    ),
                },
            })
        });
    }

    fn reveal_last_screenshot(&mut self) {
        let last = self.last_screenshot.clone();
        self.with_last_artifact(&last, "there is no saved screenshot to reveal", |path| {
            CaptureDone::result(
                path.parent()
                    .ok_or_else(|| format!("screenshot path has no parent: {}", path.display()))
                    .and_then(|folder| {
                        xdg_open(folder)
                            .map_err(|error| format!("show screenshot in Files: {error}"))
                    })
                    .map(|_| CaptureOutcome::ScreenshotRevealed(path.clone())),
            )
        });
    }

    fn open_last_screenshot(&mut self) {
        let last = self.last_screenshot.clone();
        self.with_last_artifact(&last, "there is no saved screenshot to open", |path| {
            CaptureDone::result(
                xdg_open(&path)
                    .map_err(|error| format!("open screenshot {}: {error}", path.display()))
                    .map(|_| CaptureOutcome::ScreenshotOpened(path.clone())),
            )
        });
    }

    fn open_last_screenshot_markup(&mut self, spawn_regions: &[Rect]) {
        let spawn_from = region_bounds(spawn_regions);
        self.with_last_screenshot_pixels(
            "there is no saved screenshot to mark up",
            move |_, frame_builder, decoded| match decoded {
                Ok((path, (width, height, rgba))) => {
                    frame_builder.host_mut().open_screenshot_markup(
                        std::sync::Arc::from(rgba),
                        width,
                        height,
                        spawn_from,
                    );
                    CaptureOutcome::ScreenshotMarkupOpened(path)
                }
                Err(error) => CaptureOutcome::Failed(error),
            },
        );
    }

    fn pin_last_screenshot(&mut self, spawn_regions: &[Rect]) {
        let spawn_from = region_bounds(spawn_regions);
        self.with_last_screenshot_pixels(
            "there is no saved screenshot to pin",
            move |executor, frame_builder, decoded| match decoded {
                Ok((_, pixels)) => {
                    let outcome = executor.pin_screenshot(frame_builder, spawn_from, pixels);
                    // The current clean scene already contains the receipt
                    // proxy. Clearing widget state now makes the next laid-out
                    // frame replace it directly with the newly inserted
                    // surface.
                    frame_builder.set_capture_screenshot_pinned();
                    outcome
                }
                Err(error) => {
                    frame_builder.set_capture_screenshot_pin_pending(false);
                    CaptureOutcome::Failed(error)
                }
            },
        );
    }

    fn pin_screenshot(
        &mut self,
        frame_builder: &mut FrameBuilder,
        spawn_from: Option<Rect>,
        (width, height, rgba): DecodedScreenshot,
    ) -> CaptureOutcome {
        let preview = capture_preview(&rgba, width, height);
        let id = format!("pinned-capture:{}", self.sequence);
        self.sequence = self.sequence.saturating_add(1);
        let title = crate::shell::i18n::t_args(
            "capture-pinned-title",
            &[
                ("width", width.to_string().into()),
                ("height", height.to_string().into()),
            ],
        );
        frame_builder
            .host_mut()
            .upsert_capture_surface(CaptureSurfaceRequest {
                id: &id,
                title: &title,
                rgba: std::sync::Arc::from(rgba),
                width,
                height,
                spawn_from,
                kind: CaptureSurfaceKind::Pinned { preview },
            });
        CaptureOutcome::Pinned { id }
    }

    fn copy_last_screenshot(&mut self) {
        // Decode the finished artifact rather than recapturing a desktop that
        // may already have changed. Keeping the raw frame resident would cost
        // as much as 256 MiB for an 8192×8192 extended-canvas screenshot.
        self.with_last_screenshot_pixels(
            "there is no saved screenshot to copy",
            |_, frame_builder, decoded| {
                let copied = decoded.and_then(|(_, (width, height, rgba))| {
                    if ui::clipboard_copy_image(width as usize, height as usize, &rgba).outcome()
                        != Some(true)
                    {
                        return Err("the clipboard refused image data".to_string());
                    }
                    // The copy is of the saved file: it is both now.
                    frame_builder.set_capture_screenshot_finished(
                        ui::widget::ScreenshotKept::SAVED_AND_COPIED,
                        width,
                        height,
                        capture_preview(&rgba, width, height),
                    );
                    Ok(CaptureOutcome::Copied { width, height })
                });
                copied.unwrap_or_else(CaptureOutcome::Failed)
            },
        );
    }

    /// The pixels a screenshot of `intent` holds — straight RGBA with its
    /// width and height — exactly as it is then saved, copied or handed to
    /// a portal caller.
    ///
    /// # Errors
    ///
    /// Returns an error when the selected source has nothing to paint or the
    /// capture renderer cannot be created.
    pub fn render_screenshot(
        frame_builder: &mut FrameBuilder,
        clean_screen_scene: &Scene,
        intent: &CaptureIntent,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        let (source, regions) = capture_source(frame_builder, clean_screen_scene, intent);
        if regions.is_empty() {
            return Err(NOTHING_TO_CAPTURE.into());
        }
        let (composed, width, height) = compose_scene(&source, &regions, intent.pip_layout)?;
        let mut renderer = OffscreenRenderer::try_new(width, height)
            .map_err(|error| format!("capture renderer {width}×{height}: {error}"))?;
        Ok((render_capture_rgba(&mut renderer, &composed), width, height))
    }

    /// Take the screenshot `intent` asks for. The pixels are read back now;
    /// a file is encoded and written on the capture worker, and its receipt
    /// lands on a later frame (`None` here).
    fn execute_screenshot(
        &mut self,
        frame_builder: &mut FrameBuilder,
        clean_screen_scene: &Scene,
        intent: &CaptureIntent,
        receipt: ScreenshotReceipt,
    ) -> Option<CaptureOutcome> {
        let rendered = Self::render_screenshot(frame_builder, clean_screen_scene, intent);
        let (rgba, width, height) = match rendered {
            Ok(rendered) => rendered,
            Err(error) => return receipt.failed(intent.source, error),
        };
        let preview = capture_preview(&rgba, width, height);
        match intent.destination {
            CaptureDestination::Clipboard => {
                let copied = ui::clipboard_copy_image(width as usize, height as usize, &rgba)
                    .outcome()
                    == Some(true);
                if copied {
                    frame_builder.set_capture_screenshot_finished(
                        ui::widget::ScreenshotKept::COPIED,
                        width,
                        height,
                        preview,
                    );
                    play_shutter(frame_builder);
                    Some(CaptureOutcome::Copied { width, height })
                } else {
                    receipt.failed(intent.source, "the clipboard refused image data".into())
                }
            }
            CaptureDestination::File | CaptureDestination::FileAndClipboard => {
                let path = match next_screenshot_path(self.pictures.clone(), self.sequence) {
                    Ok(path) => path,
                    Err(error) => return receipt.failed(intent.source, error),
                };
                self.sequence = self.sequence.saturating_add(1);
                // The shutter answers the press now; the receipt waits for
                // the file.
                play_shutter(frame_builder);
                let also_copy = intent.destination == CaptureDestination::FileAndClipboard;
                self.write_screenshot(
                    ScreenshotToWrite {
                        path,
                        width,
                        height,
                        rgba,
                        preview,
                    },
                    also_copy,
                    receipt,
                );
                None
            }
            CaptureDestination::FloatingWindow => receipt.failed(
                intent.source,
                "floating captures must be started as live mirrors".into(),
            ),
        }
    }

    /// Encode and write `shot` on the capture worker. Once the file is
    /// complete it becomes the last screenshot and `receipt` hears of it.
    fn write_screenshot(
        &mut self,
        shot: ScreenshotToWrite,
        also_copy: bool,
        receipt: ScreenshotReceipt,
    ) {
        let last = self.last_screenshot.clone();
        self.io.submit(move || {
            let ScreenshotToWrite {
                path,
                width,
                height,
                rgba,
                preview,
            } = shot;
            let written = write_png(&path, width, height, &rgba);
            if written.is_ok() {
                last.set(path.clone());
            }
            let announce = match receipt {
                ScreenshotReceipt::PortalRequest(deliver) => {
                    deliver(written.clone().map(|()| path.clone()));
                    None
                }
                ScreenshotReceipt::Shell => Some(CaptureOutcome::Saved(path.clone())),
                ScreenshotReceipt::PortalSelection => {
                    Some(CaptureOutcome::PortalScreenshotSaved(path.clone()))
                }
            };
            CaptureDone::new(move |_, frame_builder| match written {
                Ok(()) => {
                    // The saved file is the durable result; a clipboard that
                    // refuses the pixels leaves it in place, so it is logged
                    // rather than failing the capture.
                    let copied = also_copy
                        && ui::clipboard_copy_image(width as usize, height as usize, &rgba)
                            .outcome()
                            == Some(true);
                    if also_copy && !copied {
                        tracing::warn!(path = %path.display(), "screenshot saved but not copied");
                    }
                    frame_builder.set_capture_screenshot_finished(
                        ui::widget::ScreenshotKept {
                            saved: true,
                            copied,
                        },
                        width,
                        height,
                        preview,
                    );
                    announce
                }
                // A portal request heard its failure on the worker.
                Err(error) => announce.map(|_| CaptureOutcome::Failed(error)),
            })
        });
    }
}

/// A decoded screenshot: width, height and straight RGBA.
type DecodedScreenshot = (u32, u32, Vec<u8>);

/// The buffered "recent" seconds, handed to the capture worker to write.
struct ReplayClip {
    codec: media::StreamCodec,
    width: u32,
    height: u32,
    audio_track_names: Vec<String>,
    frames: Vec<std::sync::Arc<[u8]>>,
    /// Per audio track, the PCM for the frames.
    audio: Vec<Vec<u8>>,
}

impl ReplayClip {
    /// Push every frame and its audio into a fresh muxer and finish the
    /// file at `path`. Blocks on the disk and the muxer: worker only.
    fn write(self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create replay folder {}: {error}", parent.display()))?;
        }
        let mut output = RecordingOutput::new(
            path,
            self.codec,
            self.width,
            self.height,
            REPLAY_FPS,
            self.audio_track_names,
        );
        for (frame_index, frame) in self.frames.iter().enumerate() {
            output.write_access_unit(frame)?;
            for (track_index, pcm) in self.audio.iter().enumerate() {
                let start = frame_index.saturating_mul(AUDIO_BYTES_PER_VIDEO_FRAME);
                let end = start.saturating_add(AUDIO_BYTES_PER_VIDEO_FRAME);
                let samples = pcm
                    .get(start..end)
                    .ok_or_else(|| "replay audio frame is incomplete".to_string())?;
                output.write_pcm_audio(track_index, samples)?;
            }
        }
        output.finish(path)
    }
}

/// Drain the encoder's last frames into `output`, add the audio captured up
/// to the stop, and finish the file at `path`. Blocks on the encoder and the
/// muxer for up to 30 s each: worker only.
fn finish_recording(
    encoder: crate::network::encoder::VideoEncoder,
    mut output: RecordingOutput,
    path: &Path,
    audio: &[(usize, Vec<u8>)],
) -> Result<(), String> {
    // The encoder may still hold the last frames; a failure to drain them
    // must not cost the frames already written.
    match encoder.finish() {
        Ok(tail) => {
            for unit in tail {
                output.write_access_unit(&unit.data)?;
            }
        }
        Err(error) => {
            tracing::warn!("capture: recording ends without its last frames: {error}");
        }
    }
    for (mux_index, bytes) in audio {
        output.write_pcm_audio(*mux_index, bytes)?;
    }
    output.finish(path)
}

/// The shutter sound for a screenshot the person just took.
fn play_shutter(frame_builder: &mut FrameBuilder) {
    frame_builder.with_chrome(|chrome| {
        let _ =
            chrome.play_system_sound(settings_store::system_sounds::SystemSoundEvent::Screenshot);
    });
}

fn refresh_mirror(
    frame_builder: &mut FrameBuilder,
    clean_screen_scene: &Scene,
    mirror: &mut LiveMirror,
) -> Result<(), String> {
    if let Some(source) = mirror.tracking_source.as_ref() {
        let tracked = tracked_source_regions(frame_builder, source);
        if !tracked.is_empty() {
            mirror.intent.regions = tracked;
        }
    }
    let (source, regions) = capture_source(frame_builder, clean_screen_scene, &mirror.intent);
    if regions.is_empty() {
        if mirror.renderer.is_some() {
            // A window this picture shows cannot be painted this frame (its
            // workspace's content was let go): keep its last picture rather
            // than replacing it with some other pixels.
            return Ok(());
        }
        return Err(NOTHING_TO_CAPTURE.into());
    }
    let first_presentation = mirror.spawn_from.is_some();
    // Grid and Stack deliberately rearrange disjoint sources. Keep each
    // selected region as an independent layer for the first presentation so
    // it can travel from its exact source coordinates into its final slot.
    let handoff_layers =
        if first_presentation && regions.len() > 1 && mirror.intent.pip_layout != PipLayout::Auto {
            build_handoff_layers(&source, &regions, mirror.intent.pip_layout)?
        } else {
            Vec::new()
        };
    let (composed, width, height) = compose_scene(&source, &regions, mirror.intent.pip_layout)?;
    let stale = mirror
        .renderer
        .as_ref()
        .is_none_or(|renderer| renderer.width != width || renderer.height != height);
    if stale {
        mirror.renderer = Some(
            OffscreenRenderer::try_new(width, height)
                .map_err(|error| format!("live mirror renderer {width}×{height}: {error}"))?,
        );
    }
    let rgba = mirror
        .renderer
        .as_mut()
        .map(|renderer| render_capture_rgba(renderer, &composed))
        .ok_or_else(|| "live mirror renderer disappeared".to_string())?;
    frame_builder
        .host_mut()
        .upsert_capture_surface(CaptureSurfaceRequest {
            id: &mirror.id,
            title: &mirror.title,
            rgba: std::sync::Arc::from(rgba),
            width,
            height,
            spawn_from: mirror.spawn_from.take(),
            kind: CaptureSurfaceKind::Live {
                preferred_size: mirror.preferred_size,
                handoff_layers,
            },
        });
    Ok(())
}

fn build_handoff_layers(
    source: &Scene,
    regions: &[Rect],
    layout: PipLayout,
) -> Result<Vec<CaptureMirrorHandoffLayer>, String> {
    let source_bounds =
        union_rects(regions).ok_or_else(|| "capture regions are empty".to_string())?;
    let (target_width, target_height, target_rects) = match layout {
        PipLayout::Grid => grid_geometry(regions),
        PipLayout::Stack => stack_geometry(regions),
        PipLayout::Auto => return Ok(Vec::new()),
    };
    let mut layers = Vec::with_capacity(regions.len());
    for (region, target) in regions.iter().zip(target_rects) {
        let (single, width, height) = compose_spatial(source, &[*region])?;
        let mut renderer = OffscreenRenderer::try_new(width, height).map_err(|error| {
            format!("live mirror region handoff renderer {width}×{height}: {error}")
        })?;
        layers.push(CaptureMirrorHandoffLayer {
            rgba: std::sync::Arc::from(render_capture_rgba(&mut renderer, &single)),
            width,
            height,
            source: normalized_rect(*region, source_bounds),
            target: normalized_rect(target, Rect::new(0.0, 0.0, target_width, target_height)),
        });
    }
    Ok(layers)
}

fn normalized_rect(rect: Rect, bounds: Rect) -> Rect {
    Rect::new(
        (rect.x0 - bounds.x0) / bounds.width(),
        (rect.y0 - bounds.y0) / bounds.height(),
        (rect.x1 - bounds.x0) / bounds.width(),
        (rect.y1 - bounds.y0) / bounds.height(),
    )
}

fn region_bounds(regions: &[Rect]) -> Option<Rect> {
    let mut iter = regions.iter().copied().filter(|region| region.area() > 0.0);
    let first = iter.next()?;
    Some(iter.fold(first, |bounds, region| bounds.union(region)))
}

fn intent_spawn_bounds(intent: &CaptureIntent) -> Option<Rect> {
    region_bounds(&intent.regions).or_else(|| {
        // A wholly off-screen app has deliberately empty visible regions in
        // the picker. Its target still carries continuous-desktop geometry;
        // preserve that provenance so the shell can animate the PiP through
        // the correct physical edge instead of treating it as source-less.
        let virtual_regions: Vec<Rect> = intent
            .targets
            .iter()
            .map(|target| target.virtual_rect)
            .collect();
        region_bounds(&virtual_regions)
    })
}

fn updated_local_tracking_source(
    source: Option<FloatingSource>,
    app_id: String,
    region: Rect,
) -> FloatingSource {
    match source {
        Some(FloatingSource::AppElement {
            app_id: source_app,
            element_id,
            ..
        }) if source_app == app_id => FloatingSource::AppElement {
            app_id,
            element_id,
            region,
        },
        _ => FloatingSource::AppRegion { app_id, region },
    }
}

fn floating_source_regions(frame_builder: &mut FrameBuilder, source: &FloatingSource) -> Vec<Rect> {
    match source {
        FloatingSource::AppRegion { region, .. } | FloatingSource::AppElement { region, .. } => {
            vec![*region]
        }
        FloatingSource::Surface { surface_id } => frame_builder
            .foreign_surface_rect(*surface_id)
            .into_iter()
            .collect(),
        FloatingSource::Composite(sources) => sources
            .iter()
            .flat_map(|source| floating_source_regions(frame_builder, source))
            .collect(),
    }
}

fn tracked_source_regions(frame_builder: &mut FrameBuilder, source: &FloatingSource) -> Vec<Rect> {
    match source {
        FloatingSource::AppRegion { app_id, region } => frame_builder
            .foreign_surface_for_app(app_id)
            .and_then(|(_, app_rect)| app_local_region_on_surface(*region, app_rect))
            .into_iter()
            .collect(),
        FloatingSource::Surface { surface_id } => frame_builder
            .foreign_surface_rect(*surface_id)
            .into_iter()
            .collect(),
        FloatingSource::Composite(sources) => sources
            .iter()
            .flat_map(|source| tracked_source_regions(frame_builder, source))
            .collect(),
        FloatingSource::AppElement { app_id, region, .. } => frame_builder
            .foreign_surface_for_app(app_id)
            .and_then(|(_, app_rect)| app_local_region_on_surface(*region, app_rect))
            .into_iter()
            .collect(),
    }
}

fn app_local_region_on_surface(region: Rect, app_rect: Rect) -> Option<Rect> {
    let local_bounds = Rect::from_origin_size(Point::ZERO, app_rect.size());
    let local = region.intersect(local_bounds);
    (local.area() > 0.0).then_some(local + app_rect.origin().to_vec2())
}

fn record_frame(
    frame_builder: &mut FrameBuilder,
    clean_screen_scene: &Scene,
    recording: &mut RecordingSession,
) -> Result<(), String> {
    if recording.paused {
        drain_audio_tracks(recording, false)?;
        return Ok(());
    }
    if recording_frame_due(false, recording.last_frame.elapsed()) {
        let (source, regions) =
            capture_source(frame_builder, clean_screen_scene, &recording.intent);
        let (composed, current_w, current_h) =
            compose_scene(&source, &regions, recording.intent.pip_layout)?;
        let frame_scene = if current_w == recording.width && current_h == recording.height {
            composed
        } else {
            // H.264 streams have fixed dimensions. If the selected app resizes,
            // fit its new geometry into the original recording canvas instead of
            // aborting or changing the encoded sequence mid-file.
            let sx = f64::from(recording.width) / f64::from(current_w.max(1));
            let sy = f64::from(recording.height) / f64::from(current_h.max(1));
            let scale = sx.min(sy);
            let dx = (f64::from(recording.width) - f64::from(current_w) * scale) / 2.0;
            let dy = (f64::from(recording.height) - f64::from(current_h) * scale) / 2.0;
            let mut fitted = Scene::new();
            fitted.append(
                &composed,
                Some(Affine::translate((dx, dy)) * Affine::scale(scale)),
            );
            fitted
        };
        let rgba = render_capture_rgba(&mut recording.renderer, &frame_scene);
        let units = recording
            .encoder
            .encode(&rgba)
            .map_err(|error| format!("encode frame: {error}"))?;
        for unit in units {
            recording.output.write_access_unit(&unit.data)?;
        }
        recording.frames = recording.frames.saturating_add(1);
        recording.last_frame = Instant::now();
    }
    drain_audio_tracks(recording, true)?;
    Ok(())
}

fn record_replay_frame(
    frame_builder: &mut FrameBuilder,
    clean_screen_scene: &Scene,
    replay: &mut ReplaySession,
) -> Result<(), String> {
    if replay.last_frame.elapsed() >= Duration::from_millis(33) {
        let (source, regions) = capture_source(frame_builder, clean_screen_scene, &replay.intent);
        let (composed, current_w, current_h) =
            compose_scene(&source, &regions, replay.intent.pip_layout)?;
        let frame_scene = if current_w == replay.width && current_h == replay.height {
            composed
        } else {
            let sx = f64::from(replay.width) / f64::from(current_w.max(1));
            let sy = f64::from(replay.height) / f64::from(current_h.max(1));
            let scale = sx.min(sy);
            let dx = (f64::from(replay.width) - f64::from(current_w) * scale) / 2.0;
            let dy = (f64::from(replay.height) - f64::from(current_h) * scale) / 2.0;
            let mut fitted = Scene::new();
            fitted.append(
                &composed,
                Some(Affine::translate((dx, dy)) * Affine::scale(scale)),
            );
            fitted
        };
        let rgba = render_capture_rgba(&mut replay.renderer, &frame_scene);
        let units = replay
            .encoder
            .encode(&rgba)
            .map_err(|error| format!("encode recent frame: {error}"))?;
        for unit in units.into_iter().filter(|unit| !unit.data.is_empty()) {
            let keyframe = media::realtime::is_keyframe(replay.encoder.codec(), &unit.data);
            replay.video_bytes = replay.video_bytes.saturating_add(unit.data.len());
            replay.frames.push_back(BufferedReplayFrame {
                access_unit: std::sync::Arc::from(unit.data),
                keyframe,
            });
        }
        trim_replay_video_frames(&mut replay.frames, &mut replay.video_bytes);
        replay.last_frame = Instant::now();
    }
    drain_replay_audio(replay);
    Ok(())
}

fn trim_replay_video_frames(frames: &mut VecDeque<BufferedReplayFrame>, video_bytes: &mut usize) {
    while frames.len() > REPLAY_FRAMES || *video_bytes > REPLAY_MAX_VIDEO_BYTES {
        let Some(frame) = frames.pop_front() else {
            break;
        };
        *video_bytes = (*video_bytes).saturating_sub(frame.access_unit.len());
    }
    // A decoder may only enter on an IDR. Sacrifice at most one GOP rather
    // than writing a nominally 30-second clip whose opening seconds are gray.
    while frames.front().is_some_and(|frame| !frame.keyframe) {
        let Some(frame) = frames.pop_front() else {
            break;
        };
        *video_bytes = (*video_bytes).saturating_sub(frame.access_unit.len());
    }
}

fn drain_replay_audio(replay: &mut ReplaySession) {
    let mut ended = Vec::new();
    for (index, track) in replay.audio_tracks.iter_mut().enumerate() {
        match track.capture.drain(true) {
            Ok(bytes) => {
                track.pcm.extend(bytes);
                if track.pcm.len() > REPLAY_MAX_AUDIO_BYTES {
                    let excess = (track.pcm.len() - REPLAY_MAX_AUDIO_BYTES) / 4 * 4;
                    track.pcm.drain(..excess);
                }
            }
            Err(error) => ended.push((index, track.source.track_name(), error)),
        }
    }
    if !ended.is_empty() {
        let reason = ended
            .iter()
            .map(|(_, name, error)| format!("{name}: {error}"))
            .collect::<Vec<_>>()
            .join("; ");
        for (index, _, _) in ended.into_iter().rev() {
            replay.audio_tracks.remove(index);
        }
        replay.audio_warning = Some(reason);
    }
}

fn replay_audio_tail(pcm: &VecDeque<u8>, video_frames: usize) -> Vec<u8> {
    let target_len = video_frames.saturating_mul(AUDIO_BYTES_PER_VIDEO_FRAME);
    let available = pcm.len().min(target_len);
    let mut out = vec![0; target_len];
    let source_start = pcm.len().saturating_sub(available);
    let target_start = target_len.saturating_sub(available);
    let Some(targets) = out.get_mut(target_start..) else {
        return out;
    };
    for (target, source) in targets.iter_mut().zip(pcm.iter().skip(source_start)) {
        *target = *source;
    }
    out
}

fn selected_audio_sources(mode: AudioCaptureMode) -> Vec<super::audio_capture::AudioSource> {
    let mut sources = Vec::with_capacity(2);
    if mode.includes_system() {
        sources.push(super::audio_capture::AudioSource::System);
    }
    if mode.includes_microphone() {
        sources.push(super::audio_capture::AudioSource::Microphone);
    }
    sources
}

fn drain_audio_tracks(recording: &mut RecordingSession, keep: bool) -> Result<(), String> {
    let mut ended = Vec::new();
    for (index, track) in recording.audio_tracks.iter_mut().enumerate() {
        match track.capture.drain(keep) {
            Ok(bytes) if keep && !bytes.is_empty() => {
                recording.output.write_pcm_audio(track.mux_index, &bytes)?;
            }
            Ok(_) => {}
            Err(error) => ended.push((index, track.source.track_name(), error)),
        }
    }
    if !ended.is_empty() {
        let reasons = ended
            .iter()
            .map(|(_, name, error)| format!("{name}: {error}"))
            .collect::<Vec<_>>()
            .join("; ");
        tracing::warn!(reason = %reasons, "audio source ended during recording");
        for (index, _, _) in ended.into_iter().rev() {
            recording.audio_tracks.remove(index);
        }
        recording.audio_warning = Some(reasons);
    }
    Ok(())
}

fn recording_frame_due(paused: bool, since_last_frame: Duration) -> bool {
    !paused && since_last_frame >= Duration::from_millis(33)
}

fn immediately_due_instant() -> Instant {
    let now = Instant::now();
    now.checked_sub(Duration::from_millis(34)).unwrap_or(now)
}

/// What a capture reads, and where in it. The screen and a selection that
/// is a region of the screen read the clean screen. A window, the strip and
/// a selection anchored inside a window read a fresh paint of those windows,
/// whole and wherever they are: on screen, scrolled off it, on another strip
/// or on a workspace kept off screen.
fn capture_source(
    frame_builder: &mut FrameBuilder,
    clean_screen_scene: &Scene,
    intent: &CaptureIntent,
) -> (Scene, Vec<Rect>) {
    let of_windows = match intent.source {
        SourceMode::Window | SourceMode::Strip => true,
        SourceMode::Region => !intent.anchors.is_empty(),
        SourceMode::Display => false,
    };
    let (mut source, regions) = if of_windows {
        window_source(frame_builder, intent)
    } else {
        // The clean screen is the presented scene, where each display drew
        // its part of the desktop at its own scale; the regions were chosen
        // on the logical desktop. Protected windows, secret prompts and a
        // locked session leave the display only as the placeholder.
        (
            frame_builder.capture_scene(clean_screen_scene).into_owned(),
            intent
                .regions
                .iter()
                .filter_map(|region| frame_builder.layout_rect_to_scene(*region))
                .collect(),
        )
    };
    if intent.include_pointer
        && let Some(pointer) = frame_builder.capture_pointer_position()
        && let Some(cursor) = frame_builder.pointer_image().bitmap(24)
    {
        let pointer = if of_windows {
            pointer
        } else {
            frame_builder.layout_point_to_scene(pointer)
        };
        crate::compositor::cursor::draw_scene(&mut source, &cursor, pointer);
    }
    (source, regions)
}

/// A fresh paint of the windows `intent` takes and the rects in it it
/// captures: each whole window, or each anchored selection at its place in
/// its window now. A screenshot of one window takes the window's complete
/// content (the whole document, canvas or page) where its app offers it; a
/// picture in picture or recording of one takes its content, without the
/// window's frame.
fn window_source(frame_builder: &mut FrameBuilder, intent: &CaptureIntent) -> (Scene, Vec<Rect>) {
    let mut ids: Vec<String> = Vec::new();
    let wanted = intent
        .anchors
        .iter()
        .map(|anchor| &anchor.target)
        .chain(intent.targets.iter().map(|target| &target.id));
    for id in wanted {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    let window = intent.source == SourceMode::Window;
    let screenshot = intent.mode == CaptureMode::Screenshot;
    let (scene, painted) = frame_builder.paint_capture_targets(
        &ui::widget::LiveCapture {
            target_ids: &ids,
            complete_semantic_content: window && screenshot,
            // A live picture or a recording of a window is a view of the
            // app: its content, without the window's frame.
            content_only: window && !screenshot,
        },
        true,
    );
    let rect_of = |id: &str| {
        painted
            .iter()
            .find(|(painted, _)| painted == id)
            .map(|(_, rect)| *rect)
    };
    // Geometry and pixels come from the same live paint, and the capture is
    // atomic: a window that closed between choosing and executing yields no
    // regions rather than a silently partial or transparent capture.
    let regions: Option<Vec<Rect>> = if intent.anchors.is_empty() {
        ids.iter().map(|id| rect_of(id)).collect()
    } else {
        intent
            .anchors
            .iter()
            .map(|anchor| {
                rect_of(&anchor.target).map(|window| anchor.local + window.origin().to_vec2())
            })
            .collect()
    };
    (scene, regions.unwrap_or_default())
}

/// The title of a picture in picture: the window it shows, or what part of
/// the screen it shows when it is not of a window.
fn live_title(intent: &CaptureIntent) -> String {
    let window = intent.targets.first().map(|target| target.title.clone());
    match intent.source {
        SourceMode::Window => {
            window.unwrap_or_else(|| crate::shell::i18n::t("capture-live-window"))
        }
        SourceMode::Region if !intent.anchors.is_empty() => {
            window.unwrap_or_else(|| crate::shell::i18n::t("capture-live-window"))
        }
        SourceMode::Region if intent.regions.len() > 1 => crate::shell::i18n::t_args(
            "capture-live-screen-regions",
            &[("count", intent.regions.len().into())],
        ),
        SourceMode::Region => crate::shell::i18n::t("capture-live-screen-region"),
        SourceMode::Display => crate::shell::i18n::t("capture-live-screen"),
        SourceMode::Strip => crate::shell::i18n::t("capture-live-strip"),
    }
}

/// Captures do not have an interactive renderer's throwaway warm-up frame.
/// Route every screenshot/mirror/video frame through the shared bounded
/// resilience path so a Vello bump-buffer growth is never encoded as black.
fn render_capture_rgba(renderer: &mut OffscreenRenderer, scene: &Scene) -> Vec<u8> {
    renderer.render_rgba_resilient(scene)
}

pub(crate) fn append_clipped(scene: &mut Scene, source: &Scene, region: Rect, transform: Affine) {
    let mut clipped = Scene::new();
    clipped.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &region,
    );
    clipped.append(source, None);
    clipped.pop_layer();
    scene.append(&clipped, Some(transform));
}

/// Reframe source-space regions into a compact capture scene. `Auto` preserves
/// their spatial relationship; Grid and Stack intentionally remove large
/// desktop gaps so multi-window captures remain useful rather than mostly
/// transparent pixels.
fn compose_scene(
    source: &Scene,
    regions: &[Rect],
    layout: PipLayout,
) -> Result<(Scene, u32, u32), String> {
    let regions: Vec<Rect> = regions
        .iter()
        .copied()
        .filter(|rect| rect.is_finite() && rect.area() > 0.0)
        .collect();
    if regions.is_empty() {
        return Err("capture regions are empty".into());
    }
    match layout {
        PipLayout::Auto => compose_spatial(source, &regions),
        PipLayout::Grid => compose_placed(source, &regions, grid_geometry(&regions)),
        PipLayout::Stack => compose_placed(source, &regions, stack_geometry(&regions)),
    }
}

fn compose_spatial(source: &Scene, regions: &[Rect]) -> Result<(Scene, u32, u32), String> {
    let bounds = union_rects(regions).ok_or_else(|| "capture regions are empty".to_string())?;
    let (width, height, scale) = output_dimensions(bounds.width(), bounds.height())?;
    let mut scene = Scene::new();
    let transform = Affine::scale(scale) * Affine::translate((-bounds.x0, -bounds.y0));
    for region in regions {
        append_clipped(&mut scene, source, *region, transform);
    }
    Ok((scene, width, height))
}

/// Hand `target` to the desktop's default handler, detached from the
/// compositor's stdio.
fn xdg_open(target: &std::path::Path) -> std::io::Result<std::process::Child> {
    std::process::Command::new("xdg-open")
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
}

/// Draw each region scaled into its placement from `geometry`, which is
/// the laid-out size and one placement per region.
fn compose_placed(
    source: &Scene,
    regions: &[Rect],
    (raw_w, raw_h, placements): (f64, f64, Vec<Rect>),
) -> Result<(Scene, u32, u32), String> {
    let (width, height, global_scale) = output_dimensions(raw_w, raw_h)?;
    let mut scene = Scene::new();
    for (region, placement) in regions.iter().zip(placements) {
        let local_scale = placement.width() / region.width();
        let transform = Affine::scale(global_scale)
            * Affine::translate((placement.x0, placement.y0))
            * Affine::scale(local_scale)
            * Affine::translate((-region.x0, -region.y0));
        append_clipped(&mut scene, source, *region, transform);
    }
    Ok((scene, width, height))
}

// Widget counts are bounded by the visible capture selection, far below the
// integer range where conversion to layout-space f64 can lose meaningful data.

fn grid_geometry(regions: &[Rect]) -> (f64, f64, Vec<Rect>) {
    let count = regions.len();
    let cols = f64_of_usize(count).sqrt().ceil().max(1.0) as usize;
    let rows = count.div_ceil(cols);
    let cell_w = regions.iter().map(Rect::width).fold(1.0_f64, f64::max);
    let cell_h = regions.iter().map(Rect::height).fold(1.0_f64, f64::max);
    let raw_w = f64_of_usize(cols) * cell_w + f64_of_usize(cols.saturating_sub(1)) * GRID_GAP;
    let raw_h = f64_of_usize(rows) * cell_h + f64_of_usize(rows.saturating_sub(1)) * GRID_GAP;
    let placements = regions
        .iter()
        .enumerate()
        .map(|(index, region)| {
            let col = index % cols;
            let row = index / cols;
            let local_scale = (cell_w / region.width()).min(cell_h / region.height());
            let draw_w = region.width() * local_scale;
            let draw_h = region.height() * local_scale;
            let x = f64_of_usize(col) * (cell_w + GRID_GAP) + (cell_w - draw_w) / 2.0;
            let y = f64_of_usize(row) * (cell_h + GRID_GAP) + (cell_h - draw_h) / 2.0;
            Rect::new(x, y, x + draw_w, y + draw_h)
        })
        .collect();
    (raw_w, raw_h, placements)
}

fn stack_geometry(regions: &[Rect]) -> (f64, f64, Vec<Rect>) {
    let card_w = regions.iter().map(Rect::width).fold(1.0_f64, f64::max);
    let card_h = regions.iter().map(Rect::height).fold(1.0_f64, f64::max);
    let depth = f64_of_usize(regions.len().saturating_sub(1)) * STACK_OFFSET;
    let placements = regions
        .iter()
        .enumerate()
        .map(|(index, region)| {
            let local_scale = (card_w / region.width()).min(card_h / region.height());
            let offset = f64_of_usize(index) * STACK_OFFSET;
            Rect::new(
                offset,
                offset,
                offset + region.width() * local_scale,
                offset + region.height() * local_scale,
            )
        })
        .collect();
    (card_w + depth, card_h + depth, placements)
}

fn union_rects(rects: &[Rect]) -> Option<Rect> {
    let first = *rects.first()?;
    Some(rects.iter().skip(1).fold(first, |bounds, rect| {
        Rect::new(
            bounds.x0.min(rect.x0),
            bounds.y0.min(rect.y0),
            bounds.x1.max(rect.x1),
            bounds.y1.max(rect.y1),
        )
    }))
}

fn output_dimensions(width: f64, height: f64) -> Result<(u32, u32, f64), String> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err(format!("invalid capture size {width}×{height}"));
    }
    let scale = (MAX_CAPTURE_EDGE / width)
        .min(MAX_CAPTURE_EDGE / height)
        .min(1.0);
    let width = (width * scale).ceil().clamp(1.0, MAX_CAPTURE_EDGE) as u32;
    let height = (height * scale).ceil().clamp(1.0, MAX_CAPTURE_EDGE) as u32;
    Ok((width, height, scale))
}

/// The person's own folder for captures. Without one a capture fails rather
/// than landing in the shared temp directory, where other users could read it.
fn capture_folder(user_dir: Option<PathBuf>, name: &str) -> Result<PathBuf, String> {
    user_dir
        .or_else(tilo_paths::home)
        .map(|root| root.join(name))
        .ok_or_else(|| format!("no home folder to save {name} in"))
}

fn next_screenshot_path(pictures: Option<PathBuf>, sequence: u64) -> Result<PathBuf, String> {
    let root = capture_folder(pictures.or_else(tilo_paths::picture_dir), "Screenshots")?;
    // format: machine (file names sort by it and never change with the formats)
    let stamp = chrono::Local::now().format("%Y-%m-%d at %H.%M.%S");
    let suffix = if sequence == 0 {
        String::new()
    } else {
        format!("-{sequence}")
    };
    Ok(root.join(format!("Tilo Screenshot {stamp}{suffix}.png")))
}

fn next_recording_path(sequence: u64, include_audio: bool) -> Result<PathBuf, String> {
    let root = capture_folder(tilo_paths::video_dir(), "Screen Recordings")?;
    // format: machine (file names sort by it and never change with the formats)
    let stamp = chrono::Local::now().format("%Y-%m-%d at %H.%M.%S");
    let extension = if include_audio { "mov" } else { "mp4" };
    Ok(root.join(format!("Tilo Recording {stamp}-{sequence}.{extension}")))
}

fn next_replay_path(sequence: u64, include_audio: bool) -> Result<PathBuf, String> {
    let root = capture_folder(tilo_paths::video_dir(), "Screen Recordings")?;
    // format: machine (file names sort by it and never change with the formats)
    let stamp = chrono::Local::now().format("%Y-%m-%d at %H.%M.%S");
    let extension = if include_audio { "mov" } else { "mp4" };
    Ok(root.join(format!("Tilo Recent {stamp}-{sequence}.{extension}")))
}

// The fixed 6-by-4 preview grid takes four sub-samples per cell. Dimensions
// are bounded and pixel access is checked; direct arithmetic keeps the
// sampling geometry readable.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::manual_checked_ops,
    reason = "fixed grid constants and checked pixel access bound every preview-sampling operation"
)]
fn capture_preview(rgba: &[u8], width: u32, height: u32) -> CapturePreview {
    const COLUMNS: usize = 6;
    const ROWS: usize = 4;
    const SAMPLES: usize = 4;
    let width_usize = width as usize;
    let height_usize = height as usize;
    let mut pixels = Vec::with_capacity(COLUMNS * ROWS);
    for row in 0..ROWS {
        for column in 0..COLUMNS {
            let x0 = column * width_usize / COLUMNS;
            let x1 = ((column + 1) * width_usize / COLUMNS).max(x0 + 1);
            let y0 = row * height_usize / ROWS;
            let y1 = ((row + 1) * height_usize / ROWS).max(y0 + 1);
            let mut sums = [0u32; 4];
            let mut count = 0u32;
            for sample_y in 0..SAMPLES {
                let y = (y0 + (2 * sample_y + 1) * (y1 - y0) / (2 * SAMPLES))
                    .min(height_usize.saturating_sub(1));
                for sample_x in 0..SAMPLES {
                    let x = (x0 + (2 * sample_x + 1) * (x1 - x0) / (2 * SAMPLES))
                        .min(width_usize.saturating_sub(1));
                    let index = (y * width_usize + x) * 4;
                    if let Some(sample) = rgba.get(index..index + 4) {
                        for channel in 0..4 {
                            sums[channel] =
                                sums[channel].saturating_add(u32::from(sample[channel]));
                        }
                        count = count.saturating_add(1);
                    }
                }
            }
            pixels.push(if count == 0 {
                [0, 0, 0, 255]
            } else {
                [
                    (sums[0] / count) as u8,
                    (sums[1] / count) as u8,
                    (sums[2] / count) as u8,
                    (sums[3] / count) as u8,
                ]
            });
        }
    }
    CapturePreview {
        columns: COLUMNS as u8,
        rows: ROWS as u8,
        pixels,
    }
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create screenshot folder {}: {error}", parent.display()))?;
    }
    ui::png::write_rgba8(path, width, height, rgba)
        .map_err(|error| format!("write screenshot {}: {error}", path.display()))
}

/// Read back one of our RGBA8 screenshot artifacts for receipt actions. This
/// preserves the exact captured pixels while keeping large, off-screen app and
/// strip captures out of the shell's steady-state memory footprint.
fn read_png_rgba(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let decoded = ui::png::read_rgba8(path)
        .map_err(|error| format!("read screenshot {}: {error}", path.display()))?;
    Ok((decoded.width, decoded.height, decoded.rgba))
}

#[cfg(test)]
mod freeze_tests;
#[cfg(test)]
mod tests;
