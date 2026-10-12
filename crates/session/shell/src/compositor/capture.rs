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

use ui::floating_surface::{FloatingRole, FloatingSource, FloatingSurfaceRequest};
use ui::offscreen::OffscreenRenderer;
use ui::widget::{
    AudioCaptureMode, CaptureDestination, CaptureExtent, CaptureIntent, CaptureMode,
    CapturePreview, PipLayout, RecordingCommand, ReplayCommand, ScreenshotCommand,
    ScreenshotSendTarget,
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
    /// "Send…" chose a target for the saved screenshot; the compositor
    /// routes it over the device fabric or into the workspace.
    ScreenshotSendRequested {
        path: PathBuf,
        target: ScreenshotSendTarget,
    },
    /// A Screenshot portal selection completed in trusted shell chrome. Kept
    /// distinct from a keyboard-initiated capture so its URI is returned only
    /// to the authenticated request that armed the selector.
    PortalScreenshotSaved(PathBuf),
    PortalScreenshotCancelled,
    Failed(String),
}

#[derive(Default)]
pub struct CaptureExecutor {
    sequence: u64,
    live_mirrors: Vec<LiveMirror>,
    recording: Option<RecordingSession>,
    last_recording: Option<PathBuf>,
    last_screenshot: Option<PathBuf>,
    replay: Option<ReplaySession>,
    /// Requests from sandboxed app service connections. Kept separate from
    /// in-process widget handoffs: external clients cannot participate in the
    /// shell's second widget draw pass, so call/navigation sources become live
    /// mirrors until native sub-surface ownership is negotiated.
    external_floating: Vec<FloatingSurfaceRequest>,
    external_updates: Vec<(String, Rect, String, Rect)>,
    external_dismissals: Vec<String>,
    portal_screenshot_armed: bool,
    /// The desktop as it was when the screenshot picker opened. While the
    /// person chooses, this is what is presented under the scrim and what the
    /// screenshot is cropped from, so the capture is the screen they saw, not
    /// whatever clients drew meanwhile. Held through the frame that confirms
    /// or cancels, then released.
    frozen_desktop: Option<Scene>,
    /// Live window previews for the switcher and the overview, rendered
    /// here because this is where live geometry and a GPU meet.
    window_previews: super::window_previews::WindowPreviewRenderer,
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
    access_unit: Vec<u8>,
    keyframe: bool,
}

struct ReplayAudioTrack {
    source: super::audio_capture::AudioSource,
    capture: super::audio_capture::AudioCapture,
    pcm: VecDeque<u8>,
}

enum RecordingOutput {
    Mp4(super::mp4_recording::Mp4Recording),
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
        Self::Mp4(super::mp4_recording::Mp4Recording::new(
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
    /// # Errors
    ///
    /// Returns an error when no output is captureable or compositor readback
    /// and private PNG persistence cannot complete.
    pub fn capture_portal_screenshot(
        &mut self,
        frame_builder: &mut FrameBuilder,
        clean_screen_scene: &Scene,
        target: crate::portal::desktop_services::ScreenshotTarget,
        include_pointer: bool,
    ) -> Result<PathBuf, String> {
        let (viewport_width, viewport_height) = frame_builder.viewport();
        if !viewport_width.is_finite()
            || !viewport_height.is_finite()
            || viewport_width <= 0.0
            || viewport_height <= 0.0
        {
            return Err("the compositor has no captureable output".into());
        }

        // The portal frontend only supplies an intent, never app-controlled
        // geometry.  Area and window selection are compositor UX choices; at
        // this execution boundary the chosen region has already been reduced
        // to an output-safe rectangle.  A non-interactive request defaults to
        // the visible output, as required by Screenshot v2.
        let region = Rect::new(0.0, 0.0, viewport_width, viewport_height);
        let source = match target {
            crate::portal::desktop_services::ScreenshotTarget::Screen
            | crate::portal::desktop_services::ScreenshotTarget::Area => {
                ui::widget::SourceMode::Region
            }
            crate::portal::desktop_services::ScreenshotTarget::Window
            | crate::portal::desktop_services::ScreenshotTarget::ActiveWindow => {
                // A window target is resolved by trusted chrome, not by the
                // requesting app.  Until a particular shell window is picked,
                // capture the focused visible output rather than fabricating a
                // foreign application's off-screen content.
                ui::widget::SourceMode::Display
            }
        };
        let intent = CaptureIntent {
            mode: CaptureMode::Screenshot,
            source,
            destination: CaptureDestination::File,
            regions: vec![region],
            include_pointer,
            audio: AudioCaptureMode::None,
            extent: CaptureExtent::Visible,
            pip_layout: PipLayout::Auto,
            targets: Vec::new(),
            recording_command: None,
            replay_command: None,
            screenshot_command: None,
        };
        match self.execute_screenshot(frame_builder, clean_screen_scene, &intent)? {
            CaptureOutcome::Saved(path) => Ok(path),
            other => Err(format!(
                "portal screenshot produced an unexpected outcome: {other:?}"
            )),
        }
    }

    /// Pick the compositor pixel currently under the trusted seat pointer.
    /// The coordinate is clamped to the rendered output before indexing, so a
    /// stale pointer position can never address outside the readback buffer.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no pointer/output or GPU readback fails.
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
    ///
    /// # Errors
    ///
    /// Returns an error when the output dimensions overflow, renderer setup
    /// fails, or GPU readback does not contain a complete RGBA frame.
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
    // This is the ordered capture state-machine tick. Splitting it would hide
    // the sequencing constraints between mirrors, recording, and replay.
    #[allow(
        clippy::too_many_lines,
        reason = "the ordered capture tick keeps mirror, recording, and replay transitions in one state-machine boundary"
    )]
    pub fn execute_ready(
        &mut self,
        frame_builder: &mut FrameBuilder,
        clean_screen_scene: &mut Scene,
    ) -> (Vec<CaptureOutcome>, bool) {
        let frozen_swapped = self.hold_desktop_while_choosing(
            frame_builder.capture_choosing_screenshot(),
            clean_screen_scene,
        );
        let (outcomes, overlays_appended) =
            self.execute_ready_frame(frame_builder, clean_screen_scene);
        // The confirming (or cancelling) frame has now read the held scene.
        if !frame_builder.capture_choosing_screenshot() {
            self.frozen_desktop = None;
        }
        (outcomes, overlays_appended || frozen_swapped)
    }

    /// Freeze the desktop while the screenshot picker is choosing: the first
    /// such frame keeps `clean` as the snapshot, later frames (and the frame
    /// that confirms, once the picker has already closed) have `clean`
    /// replaced by it. Returns whether `clean` was replaced, which the caller
    /// must treat as a full repaint.
    fn hold_desktop_while_choosing(&mut self, choosing: bool, clean: &mut Scene) -> bool {
        if choosing && self.frozen_desktop.is_none() {
            self.frozen_desktop = Some(clean.clone());
            return false;
        }
        match self.frozen_desktop.as_ref() {
            Some(frozen) => {
                *clean = frozen.clone();
                true
            }
            None => false,
        }
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
            if matches!(
                request.role,
                FloatingRole::ActiveCall | FloatingRole::Navigation
            ) {
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
                    let saved = next_screenshot_path(self.sequence).and_then(|path| {
                        write_png(&path, export.width, export.height, &export.rgba).map(|()| path)
                    });
                    self.sequence = self.sequence.saturating_add(1);
                    match saved {
                        Ok(path) => {
                            self.last_screenshot = Some(path.clone());
                            frame_builder.host_mut().close_screenshot_markup();
                            CaptureOutcome::Saved(path)
                        }
                        Err(error) => CaptureOutcome::Failed(error),
                    }
                }
                ui::widget::ScreenshotMarkupCommand::Copy(export) => {
                    if ui::clipboard_copy_image(
                        export.width as usize,
                        export.height as usize,
                        &export.rgba,
                    ) {
                        CaptureOutcome::Copied {
                            width: export.width,
                            height: export.height,
                        }
                    } else {
                        CaptureOutcome::Failed("the clipboard refused marked-up pixels".into())
                    }
                }
                ui::widget::ScreenshotMarkupCommand::OpenExternal => self
                    .open_last_screenshot()
                    .unwrap_or_else(CaptureOutcome::Failed),
                ui::widget::ScreenshotMarkupCommand::Cancel => {
                    frame_builder.host_mut().close_screenshot_markup();
                    CaptureOutcome::ScreenshotMarkupClosed
                }
            };
            outcomes.push(outcome);
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
            let outcome = if let Some(command) = intent.screenshot_command {
                match command {
                    ScreenshotCommand::MarkupLast => self
                        .open_last_screenshot_markup(frame_builder, &intent.regions)
                        .unwrap_or_else(CaptureOutcome::Failed),
                    ScreenshotCommand::OpenLast => self
                        .open_last_screenshot()
                        .unwrap_or_else(CaptureOutcome::Failed),
                    ScreenshotCommand::PinLast => {
                        match self.pin_last_screenshot(frame_builder, &intent.regions) {
                            Ok(outcome) => {
                                // The current clean scene already contains the
                                // receipt proxy. Clearing widget state now
                                // makes the next laid-out frame replace it
                                // directly with the newly inserted surface.
                                frame_builder.set_capture_screenshot_pinned();
                                outcome
                            }
                            Err(error) => {
                                frame_builder.set_capture_screenshot_pin_pending(false);
                                CaptureOutcome::Failed(error)
                            }
                        }
                    }
                    ScreenshotCommand::RevealLast => self
                        .reveal_last_screenshot()
                        .unwrap_or_else(CaptureOutcome::Failed),
                    ScreenshotCommand::CopyLast => self
                        .copy_last_screenshot(frame_builder)
                        .unwrap_or_else(CaptureOutcome::Failed),
                    ScreenshotCommand::SendLast(target) => {
                        self.last_screenshot.clone().map_or_else(
                            || {
                                CaptureOutcome::Failed(
                                    "there is no saved screenshot to send".into(),
                                )
                            },
                            |path| CaptureOutcome::ScreenshotSendRequested { path, target },
                        )
                    }
                }
            } else if let Some(command) = intent.replay_command {
                match command {
                    ReplayCommand::Start => self
                        .start_replay(frame_builder, clean_screen_scene, intent)
                        .unwrap_or_else(CaptureOutcome::Failed),
                    ReplayCommand::SaveRecent => self
                        .save_replay(frame_builder)
                        .unwrap_or_else(CaptureOutcome::Failed),
                    ReplayCommand::Stop => self
                        .stop_replay(frame_builder)
                        .unwrap_or_else(CaptureOutcome::Failed),
                }
            } else if let Some(command) = intent.recording_command {
                match command {
                    RecordingCommand::Start => {
                        match self.start_recording(frame_builder, clean_screen_scene, intent) {
                            Ok(outcome) => outcome,
                            Err(error) => {
                                // Clear the immediate, non-modal Preparing UI.
                                // A failed encoder/output setup must never
                                // leave the shell implying work is ongoing.
                                frame_builder.set_capture_recording_active(false);
                                CaptureOutcome::Failed(error)
                            }
                        }
                    }
                    RecordingCommand::Pause => self
                        .set_recording_paused(frame_builder, true)
                        .unwrap_or_else(CaptureOutcome::Failed),
                    RecordingCommand::Resume => self
                        .set_recording_paused(frame_builder, false)
                        .unwrap_or_else(CaptureOutcome::Failed),
                    RecordingCommand::Stop => self
                        .stop_recording(frame_builder)
                        .unwrap_or_else(CaptureOutcome::Failed),
                    RecordingCommand::OpenLast => self
                        .open_last_recording()
                        .unwrap_or_else(CaptureOutcome::Failed),
                    RecordingCommand::RevealLast => self
                        .reveal_last_recording()
                        .unwrap_or_else(CaptureOutcome::Failed),
                }
            } else if intent.destination == CaptureDestination::FloatingWindow {
                let id = format!("capture-mirror:{}", self.sequence);
                self.sequence = self.sequence.saturating_add(1);
                let title = intent.targets.first().map_or_else(
                    || match intent.source {
                        ui::widget::SourceMode::Region => {
                            crate::shell::i18n::t("capture-live-selection")
                        }
                        ui::widget::SourceMode::Window => crate::shell::i18n::t("capture-live-app"),
                        ui::widget::SourceMode::Display => {
                            crate::shell::i18n::t("capture-live-screen")
                        }
                        ui::widget::SourceMode::Multi => crate::shell::i18n::t_args(
                            "capture-live-areas",
                            &[("count", intent.regions.len().into())],
                        ),
                    },
                    |target| target.title.clone(),
                );
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
                CaptureOutcome::Floated { id }
            } else {
                self.execute_screenshot(frame_builder, clean_screen_scene, &intent)
                    .unwrap_or_else(CaptureOutcome::Failed)
            };
            if portal_intent {
                outcomes.push(match outcome {
                    CaptureOutcome::Saved(path) => CaptureOutcome::PortalScreenshotSaved(path),
                    other => other,
                });
            } else {
                outcomes.push(outcome);
            }
        }

        for mirror in &mut self.live_mirrors {
            match refresh_mirror(frame_builder, clean_screen_scene, mirror) {
                Ok(()) => {}
                Err(error) => {
                    outcomes.push(CaptureOutcome::Failed(format!("{}: {error}", mirror.title)));
                }
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
        let source = if regions.len() > 1 {
            ui::widget::SourceMode::Multi
        } else {
            ui::widget::SourceMode::Region
        };
        let spawn_from = region_bounds(&regions);
        self.live_mirrors.push(LiveMirror {
            id: id.clone(),
            title: request.title,
            intent: CaptureIntent {
                mode: CaptureMode::Screenshot,
                source,
                destination: CaptureDestination::FloatingWindow,
                regions,
                include_pointer: false,
                audio: AudioCaptureMode::None,
                extent: CaptureExtent::Visible,
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
        let (_, width, height) = compose_scene(&source, &regions, intent.pip_layout)?;
        // 4:2:0 encoders take even dimensions; an odd canvas is fitted into
        // the next smaller even one.
        let (width, height) = ((width & !1).max(2), (height & !1).max(2));
        let renderer = OffscreenRenderer::try_new(width, height)
            .map_err(|error| format!("recording renderer {width}×{height}: {error}"))?;
        let encoder =
            super::mp4_recording::recording_encoder(width, height, 30, 8_000_000, 60, true)?;
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

    fn stop_recording(
        &mut self,
        frame_builder: &mut FrameBuilder,
    ) -> Result<CaptureOutcome, String> {
        let Some(recording) = self.recording.take() else {
            frame_builder.set_capture_recording_active(false);
            return Err("there is no active recording to stop".into());
        };
        let mut recording = recording;
        let path = recording.path.clone();
        let frames = recording.frames;
        let mut output = recording.output;
        // The encoder may still hold the last frames; a failure to drain
        // them must not cost the frames already written.
        match recording.encoder.finish() {
            Ok(tail) => {
                for unit in tail {
                    output.write_access_unit(&unit.data)?;
                }
            }
            Err(error) => {
                tracing::warn!("capture: recording ends without its last frames: {error}");
            }
        }
        for track in &mut recording.audio_tracks {
            if let Ok(bytes) = track.capture.drain(true)
                && !bytes.is_empty()
            {
                output.write_pcm_audio(track.mux_index, &bytes)?;
            }
        }
        output.finish(&path)?;
        self.last_recording = Some(path.clone());
        frame_builder.set_capture_recording_finished();
        Ok(CaptureOutcome::RecordingStopped { path, frames })
    }

    fn reveal_last_recording(&self) -> Result<CaptureOutcome, String> {
        let path = self
            .last_recording
            .as_ref()
            .ok_or_else(|| "there is no completed recording to reveal".to_string())?;
        let folder = path
            .parent()
            .ok_or_else(|| format!("recording path has no parent: {}", path.display()))?;
        xdg_open(folder).map_err(|error| format!("show recording in Files: {error}"))?;
        Ok(CaptureOutcome::RecordingRevealed(path.clone()))
    }

    fn open_last_recording(&self) -> Result<CaptureOutcome, String> {
        let path = self
            .last_recording
            .as_ref()
            .ok_or_else(|| "there is no completed recording to play".to_string())?;
        xdg_open(path).map_err(|error| format!("play recording {}: {error}", path.display()))?;
        Ok(CaptureOutcome::RecordingOpened(path.clone()))
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
        let encoder = super::mp4_recording::recording_encoder(
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

    fn save_replay(&mut self, frame_builder: &mut FrameBuilder) -> Result<CaptureOutcome, String> {
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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create replay folder {}: {error}", parent.display()))?;
        }
        let audio_track_names = replay
            .audio_tracks
            .iter()
            .map(|track| track.source.track_name().to_string())
            .collect();
        let mut output = RecordingOutput::new(
            &path,
            replay.encoder.codec(),
            replay.width,
            replay.height,
            REPLAY_FPS,
            audio_track_names,
        );
        let audio: Vec<Vec<u8>> = replay
            .audio_tracks
            .iter()
            .map(|track| replay_audio_tail(&track.pcm, replay.frames.len()))
            .collect();
        for (frame_index, frame) in replay.frames.iter().enumerate() {
            output.write_access_unit(&frame.access_unit)?;
            for (track_index, pcm) in audio.iter().enumerate() {
                let start = frame_index.saturating_mul(AUDIO_BYTES_PER_VIDEO_FRAME);
                let end = start.saturating_add(AUDIO_BYTES_PER_VIDEO_FRAME);
                let samples = pcm
                    .get(start..end)
                    .ok_or_else(|| "replay audio frame is incomplete".to_string())?;
                output.write_pcm_audio(track_index, samples)?;
            }
        }
        output.finish(&path)?;
        let seconds = replay
            .frames
            .len()
            .div_ceil(REPLAY_FPS as usize)
            .min(REPLAY_SECONDS) as u8;
        self.last_recording = Some(path.clone());
        frame_builder.set_capture_replay_saved();
        Ok(CaptureOutcome::ReplaySaved { path, seconds })
    }

    fn stop_replay(&mut self, frame_builder: &mut FrameBuilder) -> Result<CaptureOutcome, String> {
        if self.replay.take().is_none() {
            return Err("recent capture is not active".into());
        }
        frame_builder.set_capture_replay_active(false, AudioCaptureMode::None);
        Ok(CaptureOutcome::ReplayStopped)
    }

    fn reveal_last_screenshot(&self) -> Result<CaptureOutcome, String> {
        let path = self
            .last_screenshot
            .as_ref()
            .ok_or_else(|| "there is no saved screenshot to reveal".to_string())?;
        let folder = path
            .parent()
            .ok_or_else(|| format!("screenshot path has no parent: {}", path.display()))?;
        xdg_open(folder).map_err(|error| format!("show screenshot in Files: {error}"))?;
        Ok(CaptureOutcome::ScreenshotRevealed(path.clone()))
    }

    fn open_last_screenshot(&self) -> Result<CaptureOutcome, String> {
        let path = self
            .last_screenshot
            .as_ref()
            .ok_or_else(|| "there is no saved screenshot to open".to_string())?;
        xdg_open(path).map_err(|error| format!("open screenshot {}: {error}", path.display()))?;
        Ok(CaptureOutcome::ScreenshotOpened(path.clone()))
    }

    fn open_last_screenshot_markup(
        &self,
        frame_builder: &mut FrameBuilder,
        spawn_regions: &[Rect],
    ) -> Result<CaptureOutcome, String> {
        let path = self
            .last_screenshot
            .as_ref()
            .ok_or_else(|| "there is no saved screenshot to mark up".to_string())?;
        let (width, height, rgba) = read_png_rgba(path)?;
        frame_builder.host_mut().open_screenshot_markup(
            std::sync::Arc::from(rgba),
            width,
            height,
            region_bounds(spawn_regions),
        );
        Ok(CaptureOutcome::ScreenshotMarkupOpened(path.clone()))
    }

    fn pin_last_screenshot(
        &mut self,
        frame_builder: &mut FrameBuilder,
        spawn_regions: &[Rect],
    ) -> Result<CaptureOutcome, String> {
        let path = self
            .last_screenshot
            .as_ref()
            .ok_or_else(|| "there is no saved screenshot to pin".to_string())?;
        let (width, height, rgba) = read_png_rgba(path)?;
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
                spawn_from: region_bounds(spawn_regions),
                kind: CaptureSurfaceKind::Pinned { preview },
            });
        Ok(CaptureOutcome::Pinned { id })
    }

    fn copy_last_screenshot(
        &self,
        frame_builder: &mut FrameBuilder,
    ) -> Result<CaptureOutcome, String> {
        // Decode the finished artifact rather than recapturing a desktop that
        // may already have changed. Keeping the raw frame resident would cost
        // as much as 256 MiB for an 8192×8192 extended-canvas screenshot.
        let path = self
            .last_screenshot
            .as_ref()
            .ok_or_else(|| "there is no saved screenshot to copy".to_string())?;
        let (width, height, rgba) = read_png_rgba(path)?;
        if !ui::clipboard_copy_image(width as usize, height as usize, &rgba) {
            return Err("the clipboard refused image data".into());
        }
        frame_builder.set_capture_screenshot_finished(
            true,
            width,
            height,
            capture_preview(&rgba, width, height),
        );
        Ok(CaptureOutcome::Copied { width, height })
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
            return Err("the selected source has no paintable pixels".into());
        }
        let (composed, width, height) = compose_scene(&source, &regions, intent.pip_layout)?;
        let mut renderer = OffscreenRenderer::try_new(width, height)
            .map_err(|error| format!("capture renderer {width}×{height}: {error}"))?;
        Ok((render_capture_rgba(&mut renderer, &composed), width, height))
    }

    fn execute_screenshot(
        &mut self,
        frame_builder: &mut FrameBuilder,
        clean_screen_scene: &Scene,
        intent: &CaptureIntent,
    ) -> Result<CaptureOutcome, String> {
        let (rgba, width, height) =
            Self::render_screenshot(frame_builder, clean_screen_scene, intent)?;
        let preview = capture_preview(&rgba, width, height);
        match intent.destination {
            CaptureDestination::Clipboard => {
                let copied = ui::clipboard_copy_image(width as usize, height as usize, &rgba);
                if copied {
                    frame_builder.set_capture_screenshot_finished(true, width, height, preview);
                    play_shutter(frame_builder);
                    Ok(CaptureOutcome::Copied { width, height })
                } else {
                    Err("the clipboard refused image data".into())
                }
            }
            CaptureDestination::File | CaptureDestination::FileAndClipboard => {
                let path = next_screenshot_path(self.sequence)?;
                self.sequence = self.sequence.saturating_add(1);
                write_png(&path, width, height, &rgba)?;
                self.last_screenshot = Some(path.clone());
                // The saved file is the durable result; a clipboard that
                // refuses the pixels leaves it in place, so it is logged
                // rather than failing the capture.
                if intent.destination == CaptureDestination::FileAndClipboard
                    && !ui::clipboard_copy_image(width as usize, height as usize, &rgba)
                {
                    tracing::warn!(path = %path.display(), "screenshot saved but not copied");
                }
                frame_builder.set_capture_screenshot_finished(false, width, height, preview);
                play_shutter(frame_builder);
                Ok(CaptureOutcome::Saved(path))
            }
            CaptureDestination::FloatingWindow => {
                Err("floating captures must be started as live mirrors".into())
            }
        }
    }
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
                access_unit: unit.data,
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

fn capture_source(
    frame_builder: &mut FrameBuilder,
    clean_screen_scene: &Scene,
    intent: &CaptureIntent,
) -> (Scene, Vec<Rect>) {
    let (mut source, regions) = match intent.extent {
        // The clean screen is the presented scene, where each display drew
        // its part of the desktop at its own scale; the regions were chosen
        // on the logical desktop.
        // Protected windows, secret prompts and a locked session leave
        // the display only as the placeholder.
        CaptureExtent::Visible => (
            frame_builder.capture_scene(clean_screen_scene).into_owned(),
            intent
                .regions
                .iter()
                .filter_map(|region| frame_builder.layout_rect_to_scene(*region))
                .collect(),
        ),
        CaptureExtent::FullApp | CaptureExtent::WholeStrip => {
            let ids: Vec<String> = intent
                .targets
                .iter()
                .map(|target| target.id.clone())
                .collect();
            let complete_semantic_content =
                intent.mode == CaptureMode::Screenshot && intent.extent == CaptureExtent::FullApp;
            let (scene, live_rects) =
                frame_builder.paint_capture_targets(&ids, intent.extent, complete_semantic_content);
            // Geometry and pixels must come from the same live paint. Falling
            // back to an intent-time rectangle after its widget disappeared
            // produced a correctly-sized but transparent "successful"
            // screenshot. Whole-strip capture is atomic too: never silently
            // omit an app that closed between confirmation and execution.
            let regions = if live_rects.len() == ids.len() && !ids.is_empty() {
                live_rects
            } else {
                Vec::new()
            };
            (scene, regions)
        }
    };
    if intent.include_pointer
        && let Some(pointer) = frame_builder.capture_pointer_position()
        && let Some(cursor) = frame_builder.pointer_image().bitmap(24)
    {
        let pointer = if intent.extent == CaptureExtent::Visible {
            frame_builder.layout_point_to_scene(pointer)
        } else {
            pointer
        };
        crate::compositor::cursor::draw_scene(&mut source, &cursor, pointer);
    }
    (source, regions)
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

fn next_screenshot_path(sequence: u64) -> Result<PathBuf, String> {
    let root = capture_folder(tilo_paths::picture_dir(), "Screenshots")?;
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
    let stamp = chrono::Local::now().format("%Y-%m-%d at %H.%M.%S");
    let extension = if include_audio { "mov" } else { "mp4" };
    Ok(root.join(format!("Tilo Recording {stamp}-{sequence}.{extension}")))
}

fn next_replay_path(sequence: u64, include_audio: bool) -> Result<PathBuf, String> {
    let root = capture_folder(tilo_paths::video_dir(), "Screen Recordings")?;
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
mod tests {
    #![allow(
        clippy::field_reassign_with_default,
        clippy::too_many_lines,
        reason = "capture integration tests keep their full scenario setup explicit"
    )]
    #![allow(
        clippy::significant_drop_tightening,
        reason = "test fixtures transitively own long-lived permits and lock guards by design"
    )]

    use super::*;
    use crate::shell::state::{AppKind, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree};
    use ui::Widget;
    use ui::widget::TaffyTreeExt;
    use vello::kurbo::RoundedRect;
    use vello::peniko::{Color, Fill};

    /// A window whose document runs four times its own height, which it can
    /// paint whole without scrolling: what a capture of the complete app
    /// asks a window for.
    #[derive(Default)]
    struct LongDocument {
        rect: Rect,
    }

    impl LongDocument {
        fn paint(scene: &mut vello::Scene, transform: Affine, bounds: Rect) {
            scene.fill(
                Fill::NonZero,
                transform,
                Color::new([0.85, 0.85, 0.9, 1.0]),
                None,
                &bounds,
            );
        }
    }

    impl Widget for LongDocument {
        ui::provides!(dyn ui::widget::CompleteContentProvider);

        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            _theme: &ui::theme::Theme,
            node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let style = taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..taffy::Style::default()
            };
            taffy.widget_leaf(style, node_map, self)
        }

        fn apply_layout(
            &mut self,
            taffy: &taffy::TaffyTree<()>,
            node: taffy::NodeId,
            origin: Point,
        ) {
            self.rect = ui::widget::layout_rect(taffy, node, origin);
        }

        fn draw(
            &mut self,
            scene: &mut vello::Scene,
            transform: Affine,
            _text: &mut ui::text_render::TextRenderer,
            _theme: &ui::theme::Theme,
            _presence: &ui::presence::PresenceCtx,
        ) {
            Self::paint(scene, transform, self.rect);
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }

        fn name(&self) -> &'static str {
            "LongDocument"
        }
    }

    impl ui::widget::CompleteContentProvider for LongDocument {
        fn complete_content_size(&self) -> Option<vello::kurbo::Size> {
            (self.rect.height() > 0.0)
                .then(|| vello::kurbo::Size::new(self.rect.width(), self.rect.height() * 4.0))
        }

        fn draw_complete_content(
            &mut self,
            scene: &mut vello::Scene,
            bounds: Rect,
            transform: Affine,
            _text: &mut ui::text_render::TextRenderer,
            _theme: &ui::theme::Theme,
            _presence: &ui::presence::PresenceCtx,
        ) -> bool {
            Self::paint(scene, transform, bounds);
            true
        }
    }

    impl ui::widget::AppWidget for LongDocument {
        fn app_kind(&self) -> ui::widget::AppKind {
            ui::widget::AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Long document"
        }
        fn app_icon(&self) -> ui::widget::IconDescriptor {
            ui::widget::IconDescriptor::Glyph("L")
        }
        fn permission_hint(&self) -> Option<ui::widget::AppKind> {
            None
        }
    }

    fn two_source_cards() -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(220, 48, 64),
            None,
            &RoundedRect::from_rect(Rect::new(-300.0, 20.0, -100.0, 120.0), 12.0),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(48, 96, 220),
            None,
            &RoundedRect::from_rect(Rect::new(900.0, 40.0, 1_100.0, 140.0), 12.0),
        );
        scene
    }

    fn solid(color: Color) -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color,
            None,
            &Rect::new(0.0, 0.0, 10.0, 10.0),
        );
        scene
    }

    fn same_scene(a: &Scene, b: &Scene) -> bool {
        a.encoding().draw_data == b.encoding().draw_data
            && a.encoding().path_data == b.encoding().path_data
    }

    /// The screenshot is the screen the person saw when the picker opened:
    /// what a client draws while they choose does not reach the capture, and
    /// the hold ends with the confirming frame.
    #[test]
    fn the_desktop_is_held_still_while_the_screenshot_picker_chooses() {
        let before = solid(Color::from_rgb8(220, 48, 64));
        let after = solid(Color::from_rgb8(48, 96, 220));
        let mut executor = CaptureExecutor::default();

        // Opening: the live desktop becomes the snapshot.
        let mut clean = before.clone();
        assert!(!executor.hold_desktop_while_choosing(true, &mut clean));
        assert!(same_scene(&clean, &before));

        // Choosing: a changed desktop is replaced by the snapshot.
        let mut clean = after.clone();
        assert!(executor.hold_desktop_while_choosing(true, &mut clean));
        assert!(same_scene(&clean, &before), "client updates stay hidden");
        assert!(!same_scene(&clean, &after));

        // Confirming: the picker has closed but this frame still captures
        // the snapshot.
        let mut clean = after.clone();
        assert!(executor.hold_desktop_while_choosing(false, &mut clean));
        assert!(
            same_scene(&clean, &before),
            "the capture is the frozen frame"
        );
        executor.frozen_desktop = None;

        // Released: the live desktop is untouched again.
        let mut clean = after.clone();
        assert!(!executor.hold_desktop_while_choosing(false, &mut clean));
        assert!(same_scene(&clean, &after));
    }

    #[test]
    fn screenshot_artifacts_keep_exact_pixels_and_exclusive_selection() {
        // Phase `saved_screenshot_round_trips_exact_pixels_for_receipt_copy`.
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("finished-capture.png");
            let pixels = vec![
                18, 32, 64, 255, 240, 120, 30, 255, 4, 220, 180, 96, 255, 255, 255, 0,
            ];

            write_png(&path, 2, 2, &pixels).unwrap();
            let (width, height, decoded) = read_png_rgba(&path).unwrap();

            assert_eq!((width, height), (2, 2));
            assert_eq!(decoded, pixels);
        }
        // Phase `portal_selector_reservation_is_exclusive_and_cancellable`.
        {
            let mut executor = CaptureExecutor::default();
            assert!(executor.arm_portal_screenshot());
            assert!(!executor.arm_portal_screenshot());
            executor.cancel_portal_screenshot();
            assert!(executor.arm_portal_screenshot());
        }
        // Phase `screenshot_open_requires_a_finished_file_artifact`.
        {
            let executor = CaptureExecutor::default();
            assert_eq!(
                executor.open_last_screenshot().unwrap_err(),
                "there is no saved screenshot to open"
            );
        }
        // Phase `pin_uses_finished_pixels_and_does_not_create_a_live_mirror`.
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("finished-capture.png");
            let pixels = vec![18, 32, 64, 255, 240, 120, 30, 255];
            write_png(&path, 2, 1, &pixels).unwrap();

            let mut executor = CaptureExecutor::default();
            executor.last_screenshot = Some(path);
            let mut frame_builder = FrameBuilder::new((800, 600));
            let spawn = Rect::new(280.0, 6.0, 326.0, 30.0);
            let outcome = executor
                .pin_last_screenshot(&mut frame_builder, &[spawn])
                .unwrap();

            assert_eq!(
                outcome,
                CaptureOutcome::Pinned {
                    id: "pinned-capture:0".into()
                }
            );
            assert!(
                executor.live_mirrors.is_empty(),
                "a pinned still must never enter the live refresh loop"
            );
            assert_eq!(frame_builder.host_mut().capture_mirror_count_for_tests(), 1);
        }
        // Phase `screenshot_preview_preserves_spatial_color_structure`.
        {
            let mut rgba = Vec::new();
            for row in 0..4u8 {
                for column in 0..6u8 {
                    rgba.extend_from_slice(&[
                        column.saturating_mul(30),
                        row.saturating_mul(50),
                        column.saturating_add(row).saturating_mul(20),
                        255,
                    ]);
                }
            }
            let preview = capture_preview(&rgba, 6, 4);
            assert_eq!((preview.columns, preview.rows), (6, 4));
            assert_eq!(preview.pixels.len(), 24);
            assert_eq!(preview.pixels[0], [0, 0, 0, 255]);
            assert_eq!(preview.pixels[5], [150, 0, 100, 255]);
            assert_eq!(preview.pixels[18], [0, 150, 60, 255]);
            assert_eq!(preview.pixels[23], [150, 150, 160, 255]);
        }
    }

    #[test]
    fn pip_targets_retire_with_their_app_and_keep_virtual_bounds() {
        // Phase `in_process_pip_retires_when_its_app_activity_disappears`.
        {
            let mut executor = CaptureExecutor::default();
            executor.live_mirrors.push(LiveMirror {
                id: "browser.media-pip".into(),
                title: "Video".into(),
                intent: CaptureIntent {
                    mode: CaptureMode::Pip,
                    source: ui::widget::SourceMode::Region,
                    destination: CaptureDestination::FloatingWindow,
                    regions: vec![Rect::new(100.0, 80.0, 500.0, 305.0)],
                    include_pointer: false,
                    audio: AudioCaptureMode::None,
                    extent: CaptureExtent::Visible,
                    pip_layout: PipLayout::Auto,
                    targets: Vec::new(),
                    recording_command: None,
                    replay_command: None,
                    screenshot_command: None,
                },
                renderer: None,
                preferred_size: Some(Size::new(400.0, 225.0)),
                tracking_source: None,
                close_when_app_inactive: true,
                spawn_from: None,
            });
            let mut frame_builder = FrameBuilder::new((800, 600));
            let mut clean = frame_builder.paint();

            let (outcomes, _capture_overlays_appended) =
                executor.execute_ready(&mut frame_builder, &mut clean);
            assert_eq!(
                outcomes,
                vec![CaptureOutcome::FloatingClosed {
                    id: "browser.media-pip".into()
                }]
            );
            assert!(executor.live_mirrors.is_empty());
        }
        // Phase `offscreen_target_retains_virtual_bounds_for_pip_handoff`.
        {
            let virtual_rect = Rect::new(-1_300.0, 120.0, -300.0, 820.0);
            let intent = CaptureIntent {
                mode: CaptureMode::Pip,
                source: ui::widget::SourceMode::Window,
                destination: CaptureDestination::FloatingWindow,
                regions: Vec::new(),
                include_pointer: false,
                audio: AudioCaptureMode::None,
                extent: CaptureExtent::FullApp,
                pip_layout: PipLayout::Auto,
                targets: vec![ui::widget::CaptureTarget {
                    id: "notes".into(),
                    title: "Notes".into(),
                    app_id: "so.tilo.notes".into(),
                    visible_rect: Rect::ZERO,
                    virtual_rect,
                    complete_rect: Some(Rect::new(-1_300.0, 120.0, 10.0, 1_308.0)),
                }],
                recording_command: None,
                replay_command: None,
                screenshot_command: None,
            };
            assert_eq!(intent_spawn_bounds(&intent), Some(virtual_rect));
        }
    }

    #[test]
    fn spatial_capture_geometry_frames_regions_without_desktop_gaps() {
        // Phase `spatial_multi_capture_leaves_unselected_gap_transparent`.
        fn spatial_multi_capture_leaves_unselected_gap_transparent() {
            let source = two_source_cards();
            let regions = [
                Rect::new(-300.0, 20.0, -100.0, 120.0),
                Rect::new(900.0, 40.0, 1_100.0, 140.0),
            ];
            let (scene, width, height) = compose_scene(&source, &regions, PipLayout::Auto).unwrap();
            let Ok(mut renderer) = OffscreenRenderer::try_new(width, height) else {
                eprintln!("skipping pixel assertion: no offscreen GPU adapter");
                return;
            };
            let rgba = renderer.render_rgba(&scene);
            let alpha = |x: u32, y: u32| rgba[((y * width + x) * 4 + 3) as usize];
            assert!(alpha(100, 50) > 0, "selected left card is present");
            assert_eq!(
                alpha(700, 50),
                0,
                "desktop pixels between selected regions must not leak into export"
            );
            assert!(alpha(1_300, 70) > 0, "selected right card is present");
        }
        // Phase `spatial_capture_can_frame_content_beyond_both_screen_edges`.
        {
            let source = two_source_cards();
            let regions = [
                Rect::new(-300.0, 20.0, -100.0, 120.0),
                Rect::new(900.0, 40.0, 1_100.0, 140.0),
            ];
            let (scene, width, height) = compose_scene(&source, &regions, PipLayout::Auto).unwrap();
            assert_eq!((width, height), (1_400, 120));
            assert!(!scene.encoding().is_empty());
        }
        // Phase `grid_removes_the_unused_virtual_desktop_gap`.
        {
            let source = two_source_cards();
            let regions = [
                Rect::new(-300.0, 20.0, -100.0, 120.0),
                Rect::new(900.0, 40.0, 1_100.0, 140.0),
            ];
            let (_, width, height) = compose_scene(&source, &regions, PipLayout::Grid).unwrap();
            assert_eq!((width, height), (416, 100));
        }
        // Phase `multi_region_handoff_geometry_preserves_source_and_grid_endpoints`.
        {
            let regions = [
                Rect::new(250.0, 180.0, 710.0, 410.0),
                Rect::new(760.0, 310.0, 1_170.0, 590.0),
            ];
            let source_bounds = union_rects(&regions).unwrap();
            let first_source = normalized_rect(regions[0], source_bounds);
            assert_eq!(first_source.x0, 0.0);
            assert_eq!(first_source.y0, 0.0);
            assert!(first_source.x1 < 0.51);

            let (width, height, placements) = grid_geometry(&regions);
            let target_bounds = Rect::new(0.0, 0.0, width, height);
            let first_target = normalized_rect(placements[0], target_bounds);
            let second_target = normalized_rect(placements[1], target_bounds);
            assert!(first_target.x1 < second_target.x0, "grid gap must survive");
            assert!(first_target.height() < 1.0, "wide source is letterboxed");
            assert_eq!(second_target.y0, 0.0);
            assert_eq!(second_target.y1, 1.0);
        }
        // Phase `oversized_capture_scales_down_without_changing_aspect_ratio`.
        {
            let (_, _, scale) = output_dimensions(16_384.0, 8_192.0).unwrap();
            assert_eq!(scale, 0.5);
            assert_eq!(output_dimensions(16_384.0, 8_192.0).unwrap().0, 8_192);
            assert_eq!(output_dimensions(16_384.0, 8_192.0).unwrap().1, 4_096);
        }
        // Phase `spatial_multi_capture_leaves_unselected_gap_transparent` (defined above).
        spatial_multi_capture_leaves_unselected_gap_transparent();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn app_capture_follows_its_live_surface_and_never_fakes_success() {
        // Phase `full_app_capture_paints_a_wholly_offscreen_live_group`.
        fn full_app_capture_paints_a_wholly_offscreen_live_group() {
            let mut tree = WorkspaceTree::bootstrap_demo();
            let (strip_id, target_group, target_tab) = {
                let workspace = tree
                    .desktop
                    .active_workspace_mut()
                    .expect("bootstrap has an active workspace");
                let strip = workspace.strips.first_mut().expect("bootstrap has a strip");
                let target = strip
                    .window_groups
                    .first_mut()
                    .expect("bootstrap has a window group");
                target.tabs.truncate(1);
                target.active_tab = target.tabs[0].id;
                target.width = WindowWidth::Fraction(1_000);
                let target_group = target.id;
                let target_tab = target.active_tab;
                for (group_id, tab_id, label) in
                    [(70_001, 80_001, "Middle"), (70_002, 80_002, "Visible")]
                {
                    strip.window_groups.push(WindowGroup {
                        id: group_id,
                        tabs: vec![Tab {
                            id: tab_id,
                            title: label.into(),
                            tile: TileContent::Stub {
                                kind: AppKind::Settings,
                                label: label.into(),
                            },
                        }],
                        active_tab: tab_id,
                        width: WindowWidth::Fraction(1_000),
                        maximized: false,
                    });
                }
                (strip.id, target_group, target_tab)
            };
            // Keep the strip parked on the target group. The desktop eases its
            // active group into view on a wall-clock animation, so naming the
            // last group active here would let a slow run finish that ease
            // before the explicit pan below and leave nothing to scroll.
            tree.desktop.set_active_group(strip_id, target_group);

            let mut frame_builder = FrameBuilder::with_tree(tree, (800, 600));
            let _ = frame_builder.paint();
            // The target window shows a document four times its own height.
            frame_builder
                .host_mut()
                .group_at_for_tests(strip_id, 0)
                .and_then(|group| group.active_host_mut())
                .expect("target group's window")
                .replace_content(ui::widget::TileContent::App(Box::new(
                    LongDocument::default(),
                )));
            frame_builder.relayout();
            let _ = frame_builder.paint();
            assert!(
                frame_builder
                    .host_mut()
                    .group_at_for_tests(strip_id, 0)
                    .is_some_and(|group| group.complete_capture_rect().is_some()),
                "a long document publishes its complete capture geometry"
            );
            // Focusing the last group already eases the strip toward it on
            // the wall clock, so whether this jump still moves the strip
            // depends on how long the frames above took. Jump to the end of
            // that ease and check where the strip rests instead.
            let _ = frame_builder
                .host_mut()
                .group_row_mut_for_tests(strip_id)
                .expect("live strip row")
                .scroll_group_into_view(70_002);
            frame_builder.relayout();
            let onscreen = frame_builder
                .host_mut()
                .group_row_mut_for_tests(strip_id)
                .expect("live strip row")
                .onscreen_group_ids();
            assert!(
                onscreen.contains(&70_002) && !onscreen.contains(&target_group),
                "the strip must rest on the last group with the target offscreen: {onscreen:?}"
            );
            let clean_screen = frame_builder.paint();
            let target_group_ref = frame_builder
                .host_mut()
                .group_at_for_tests(strip_id, 0)
                .expect("offscreen target group");
            let live_rect = target_group_ref.get_rect();
            let target_rect = target_group_ref
                .complete_capture_rect()
                .expect("a long document has complete capture geometry");
            assert!(
                target_rect.height() > live_rect.height(),
                "semantic app capture must extend below its live window"
            );
            assert_eq!(
                target_rect
                    .intersect(Rect::new(0.0, 0.0, 800.0, 600.0))
                    .area(),
                0.0,
                "precondition: the selected app is wholly outside the physical output; got {target_rect:?}"
            );

            let intent = CaptureIntent {
                mode: ui::widget::CaptureMode::Screenshot,
                source: ui::widget::SourceMode::Window,
                destination: CaptureDestination::File,
                regions: Vec::new(),
                include_pointer: false,
                audio: AudioCaptureMode::None,
                extent: CaptureExtent::FullApp,
                pip_layout: PipLayout::Auto,
                targets: vec![ui::widget::CaptureTarget {
                    id: format!("group:{target_group}:tab:{target_tab}"),
                    title: "Notes".into(),
                    app_id: "so.tilo.notes".into(),
                    visible_rect: Rect::ZERO,
                    // Deliberately stale: the live draw must win over picker-time
                    // geometry after strip motion.
                    virtual_rect: Rect::new(10_000.0, 0.0, 10_100.0, 100.0),
                    complete_rect: Some(Rect::new(10_000.0, 0.0, 10_100.0, 200.0)),
                }],
                recording_command: None,
                replay_command: None,
                screenshot_command: None,
            };
            let (source, regions) = capture_source(&mut frame_builder, &clean_screen, &intent);
            assert_eq!(regions, vec![target_rect]);
            assert!(
                !source.encoding().is_empty(),
                "the offscreen app must paint"
            );
            let (capture, width, height) =
                compose_scene(&source, &regions, PipLayout::Auto).unwrap();
            assert_eq!(width, target_rect.width().ceil() as u32);
            assert_eq!(height, target_rect.height().ceil() as u32);
            let mut live_intent = intent.clone();
            live_intent.mode = CaptureMode::Pip;
            live_intent.destination = CaptureDestination::FloatingWindow;
            let (_, live_regions) = capture_source(&mut frame_builder, &clean_screen, &live_intent);
            assert_eq!(
                live_regions,
                vec![live_rect],
                "PiP must keep the offscreen app window readable rather than flattening the complete document"
            );
            let Ok(mut renderer) = OffscreenRenderer::try_new(width, height) else {
                eprintln!("skipping pixel assertion: no offscreen GPU adapter");
                return;
            };
            let rgba = render_capture_rgba(&mut renderer, &capture);
            assert!(
                rgba.chunks_exact(4).any(|pixel| pixel[3] != 0),
                "the compositor returned dimensions but no captured app pixels"
            );
        }
        // Phase `full_app_capture_paints_a_wholly_offscreen_live_group` (defined above).
        full_app_capture_paints_a_wholly_offscreen_live_group();
        // Phase `vanished_full_app_never_falls_back_to_a_blank_success`.
        {
            let mut frame_builder = FrameBuilder::new((800, 600));
            let clean_screen = frame_builder.paint();
            let intent = CaptureIntent {
                mode: ui::widget::CaptureMode::Screenshot,
                source: ui::widget::SourceMode::Window,
                destination: CaptureDestination::File,
                regions: Vec::new(),
                include_pointer: false,
                audio: AudioCaptureMode::None,
                extent: CaptureExtent::FullApp,
                pip_layout: PipLayout::Auto,
                targets: vec![ui::widget::CaptureTarget {
                    id: "group:999999:tab:999999".into(),
                    title: "Gone".into(),
                    app_id: "so.tilo.gone".into(),
                    visible_rect: Rect::ZERO,
                    virtual_rect: Rect::new(1_000.0, 100.0, 1_800.0, 700.0),
                    complete_rect: None,
                }],
                recording_command: None,
                replay_command: None,
                screenshot_command: None,
            };
            let (_, regions) = capture_source(&mut frame_builder, &clean_screen, &intent);
            assert!(regions.is_empty());
            assert!(
                compose_scene(&Scene::new(), &regions, PipLayout::Auto).is_err(),
                "a vanished target must report failure instead of saving transparent pixels"
            );
        }
        // Phase `tracked_app_region_follows_its_surface_without_changing_local_content`.
        {
            let local_video = Rect::new(80.0, 120.0, 560.0, 390.0);
            let first_app = Rect::new(200.0, 60.0, 1_000.0, 660.0);
            let moved_app = Rect::new(640.0, 90.0, 1_440.0, 690.0);
            let first = app_local_region_on_surface(local_video, first_app).unwrap();
            let moved = app_local_region_on_surface(local_video, moved_app).unwrap();
            assert_eq!(first, Rect::new(280.0, 180.0, 760.0, 450.0));
            assert_eq!(moved, Rect::new(720.0, 210.0, 1_200.0, 480.0));
            assert_eq!(first.size(), moved.size());
        }
        // Phase `semantic_region_update_preserves_element_identity`.
        {
            let updated = updated_local_tracking_source(
                Some(FloatingSource::AppElement {
                    app_id: "so.tilo.browser".into(),
                    element_id: "media:7".into(),
                    region: Rect::new(10.0, 20.0, 410.0, 245.0),
                }),
                "so.tilo.browser".into(),
                Rect::new(24.0, 36.0, 624.0, 374.0),
            );
            assert_eq!(
                updated,
                FloatingSource::AppElement {
                    app_id: "so.tilo.browser".into(),
                    element_id: "media:7".into(),
                    region: Rect::new(24.0, 36.0, 624.0, 374.0),
                }
            );
        }
    }

    #[test]
    fn recording_pauses_and_replay_buffers_stay_bounded() {
        // Phase `paused_recording_never_schedules_an_encoder_frame`.
        assert!(!recording_frame_due(true, Duration::from_secs(30)));
        assert!(!recording_frame_due(false, Duration::from_millis(32)));
        assert!(recording_frame_due(false, Duration::from_millis(33)));
        // Phase `replay_ring_is_bounded_and_always_begins_on_a_keyframe`.
        {
            let mut frames: VecDeque<_> = (0..REPLAY_FRAMES + 17)
                .map(|index| BufferedReplayFrame {
                    access_unit: vec![index as u8],
                    keyframe: index % REPLAY_KEYFRAME_INTERVAL as usize == 0,
                })
                .collect();
            let mut bytes = frames.len();
            trim_replay_video_frames(&mut frames, &mut bytes);
            assert!(frames.len() <= REPLAY_FRAMES);
            assert!(frames.front().is_some_and(|frame| frame.keyframe));
            assert_eq!(bytes, frames.len());
        }
        // Phase `replay_audio_tail_pads_warmup_and_keeps_the_newest_samples`.
        {
            let pcm: VecDeque<u8> = (0..12).collect();
            let padded = replay_audio_tail(&pcm, 1);
            assert_eq!(padded.len(), AUDIO_BYTES_PER_VIDEO_FRAME);
            assert_eq!(&padded[padded.len() - 12..], &(0..12).collect::<Vec<_>>());

            let pcm: VecDeque<u8> = (0..AUDIO_BYTES_PER_VIDEO_FRAME + 8)
                .map(|value| value as u8)
                .collect();
            let tail = replay_audio_tail(&pcm, 1);
            assert_eq!(tail[0], 8);
            assert_eq!(tail.len(), AUDIO_BYTES_PER_VIDEO_FRAME);
        }
    }
}
