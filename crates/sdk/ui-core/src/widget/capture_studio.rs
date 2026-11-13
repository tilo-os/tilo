//! The capture tool: one see-through bar to take a screenshot, record, or float a live picture in
//! picture of a selection, a window, the screen or the strip.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "immediate-mode capture geometry uses tiny fixed arrays and bounded screen coordinates"
)]

use crate::{
    icons::Icon,
    paint::PaintCtx,
    presence::PresenceCtx,
    text_render::TextRenderer,
    theme::Theme,
    widget::{
        Button, ButtonFace, CaptureButtonFace, CaptureKind, ControlSize, IconButton, TaffyTreeExt,
        Widget,
        selection_handles::{Handle, SelectionHandles},
        toggle_group::{Segment, SegmentedFace, SegmentedStyle},
    },
};
use automerge::ObjId;
use std::collections::HashMap;

use crate::time::{Duration, Instant};
use crate::tokens::TextRole;
use taffy::{Dimension, NodeId, Size as TaffySize, Style, TaffyTree};
use vello::{
    Scene,
    kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke, Vec2},
    peniko::{BlendMode, Fill},
};
use winit::{
    event::{ElementState, KeyEvent, MouseButton, WindowEvent},
    keyboard::{Key, NamedKey},
};

const SCREENSHOT_RECEIPT_DURATION: Duration = Duration::from_millis(5_500);
const RECORDING_RECEIPT_DURATION: Duration = Duration::from_millis(4_000);
/// The delays a capture can wait, in seconds; `0` is none.
const DELAY_CHOICES: [u8; 4] = [0, 3, 5, 10];
/// The smallest selection a drag makes.
const MIN_REGION: f64 = crate::tokens::Spacing::BASE.s3 as f64;
/// How much of the theme's modal scrim dims what is not being captured: the
/// rest of the screen stays readable, the chosen thing stands out.
const SCRIM_SHARE: f32 = 0.5;

/// The outputs, in the order the bar shows them.
const OUTPUTS: [CaptureMode; 3] = [
    CaptureMode::Screenshot,
    CaptureMode::Recording,
    CaptureMode::Pip,
];
/// The targets, in the order the bar shows them.
const TARGETS: [SourceMode; 4] = [
    SourceMode::Region,
    SourceMode::Window,
    SourceMode::Display,
    SourceMode::Strip,
];

#[derive(Clone, Debug)]
struct ArmedCapture {
    intent: CaptureIntent,
    started_at: Instant,
    duration: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturePreview {
    pub columns: u8,
    pub rows: u8,
    pub pixels: Vec<[u8; 4]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScreenshotKept {
    pub saved: bool,
    pub copied: bool,
}

impl ScreenshotKept {
    pub const SAVED_AND_COPIED: Self = Self {
        saved: true,
        copied: true,
    };
    pub const SAVED: Self = Self {
        saved: true,
        copied: false,
    };
    pub const COPIED: Self = Self {
        saved: false,
        copied: true,
    };

    fn status_key(self) -> &'static str {
        match (self.saved, self.copied) {
            (true, true) => "widget-screenshot-copied-saved",
            (false, true) => "widget-screenshot-copied",
            _ => "widget-screenshot-saved",
        }
    }
}

#[derive(Clone, Debug)]
struct ScreenshotReceipt {
    kept: ScreenshotKept,
    width: u32,
    height: u32,
    preview: CapturePreview,
    shown_at: Instant,
    origin: Point,
    progress_override: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureMode {
    #[default]
    Screenshot,
    Recording,
    Pip,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceMode {
    #[default]
    Region,
    Window,
    Display,
    Strip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureDestination {
    File,
    Clipboard,
    FloatingWindow,
    FileAndClipboard,
}

#[derive(Clone, Copy, Debug)]
pub struct LiveCapture<'a> {
    pub target_ids: &'a [String],
    /// Paint a window's complete semantic content (page, document, canvas)
    /// where its app provides one, rather than its live surface: what a
    /// screenshot of a whole window takes.
    pub complete_semantic_content: bool,
    /// Take the window's content without its tab decoration: what a live
    /// picture in picture or a recording of a window shows, a view of the
    /// app rather than of its frame.
    pub content_only: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PipLayout {
    /// Where they are relative to each other.
    #[default]
    Auto,
    Grid,
    Stack,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AudioCaptureMode {
    #[default]
    None,
    System,
    Microphone,
    SystemAndMicrophone,
}

impl AudioCaptureMode {
    pub fn includes_system(self) -> bool {
        matches!(self, Self::System | Self::SystemAndMicrophone)
    }

    pub fn includes_microphone(self) -> bool {
        matches!(self, Self::Microphone | Self::SystemAndMicrophone)
    }

    fn with_system(self, enabled: bool) -> Self {
        Self::of(enabled, self.includes_microphone())
    }

    fn with_microphone(self, enabled: bool) -> Self {
        Self::of(self.includes_system(), enabled)
    }

    fn of(system: bool, microphone: bool) -> Self {
        match (system, microphone) {
            (false, false) => Self::None,
            (false, true) => Self::Microphone,
            (true, false) => Self::System,
            (true, true) => Self::SystemAndMicrophone,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaptureTarget {
    pub id: String,
    pub title: String,
    pub app_id: String,
    pub visible_rect: Rect,
    pub virtual_rect: Rect,
    /// Complete semantic page/document/canvas bounds supplied by the app.
    /// Absent for ordinary and foreign windows, where `virtual_rect` remains
    /// the complete surface. Metadata only: execution asks the live window
    /// to paint again so stale geometry never wins.
    pub complete_rect: Option<Rect>,
    pub focused: bool,
    pub on_active_strip: bool,
}

impl CaptureTarget {
    pub fn full_rect(&self) -> Rect {
        self.complete_rect.unwrap_or(self.virtual_rect)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegionAnchor {
    pub target: String,
    pub local: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingCommand {
    Start,
    Pause,
    Resume,
    Stop,
    OpenLast,
    RevealLast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayCommand {
    Start,
    SaveRecent,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenshotSendTarget {
    Workspace,
    Device(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenshotSendDevice {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenshotCommand {
    MarkupLast,
    OpenLast,
    /// Turn the exact finished pixels into a frozen reference surface. This
    /// must never be implemented as a live recapture/`PiP` source.
    PinLast,
    RevealLast,
    /// Copy the already-rendered last screenshot without recapturing a live
    /// scene that may have changed in the meantime.
    CopyLast,
    SendLast(ScreenshotSendTarget),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaptureIntent {
    pub mode: CaptureMode,
    pub source: SourceMode,
    pub destination: CaptureDestination,
    /// On-screen rects of what was chosen when it was chosen: the output for
    /// Screen, each selection for Selection, the visible part of each window
    /// for Window and Strip. Where a capture grows from; for Screen and a
    /// screen-region selection, also what is captured.
    pub regions: Vec<Rect>,
    /// For a Selection made wholly inside windows, one anchor per region in
    /// `regions`, in the same order: what is captured is that part of that
    /// window, wherever it is. Empty when any selection is not inside a
    /// window: the selections are then regions of the screen.
    pub anchors: Vec<RegionAnchor>,
    pub include_pointer: bool,
    pub audio: AudioCaptureMode,
    pub pip_layout: PipLayout,
    pub targets: Vec<CaptureTarget>,
    /// `Some` only for recorder lifecycle work. Source fields remain
    /// populated on Start and may be empty for Stop.
    pub recording_command: Option<RecordingCommand>,
    pub replay_command: Option<ReplayCommand>,
    /// Post-capture actions are emitted separately from a new screenshot so
    /// clicking its receipt never accidentally reuses stale selection state.
    pub screenshot_command: Option<ScreenshotCommand>,
}

impl CaptureIntent {
    /// An intent carrying only a command, with no source.
    fn command(mode: CaptureMode, source: SourceMode) -> Self {
        Self {
            mode,
            source,
            destination: CaptureDestination::File,
            regions: Vec::new(),
            anchors: Vec::new(),
            include_pointer: false,
            audio: AudioCaptureMode::None,
            pip_layout: PipLayout::Auto,
            targets: Vec::new(),
            recording_command: None,
            replay_command: None,
            screenshot_command: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    Create {
        start: Point,
        current: Point,
    },
    Move {
        index: usize,
        original: Rect,
        start: Point,
        current: Point,
    },
    Resize {
        index: usize,
        original: Rect,
        handle: Handle,
        current: Point,
    },
}

impl Drag {
    fn set_current(&mut self, point: Point) {
        match self {
            Self::Create { current, .. }
            | Self::Move { current, .. }
            | Self::Resize { current, .. } => *current = point,
        }
    }
}

/// State of one recording take, from the request until it stops.
#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a take is requested, active, paused and stopping independently"
)]
struct RecordingTake {
    /// Requested; the compositor has not yet confirmed the encoder. The bar
    /// shows at once so a slow start never reads as a missed click.
    start_pending: bool,
    active: bool,
    started_at: Option<Instant>,
    stop_pending: bool,
    paused: bool,
    paused_at: Option<Instant>,
    paused_total: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OptionChoice {
    Delay(u8),
    Pointer,
    SystemAudio,
    Microphone,
    KeepRecent,
    Layout(PipLayout),
}

type OptionRow = (String, OptionChoice, Option<bool>);
type OptionSection = (Option<String>, Vec<OptionRow>);

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent capture, recording, replay and receipt states are retained together"
)]
#[crate::component::not_a_component(system, "screen capture is the system's")]
pub struct CaptureStudio {
    rect: Rect,
    overlay_rect: Rect,
    open: bool,
    /// The person put the bar down themselves (Escape, the receipt's own ✕),
    /// rather than the bar going down to hand the capture on: what it was
    /// still saying goes down with it.
    put_away: bool,
    opened_at: Option<Instant>,
    mode: CaptureMode,
    source: SourceMode,
    regions: Vec<Rect>,
    drag: Option<Drag>,
    cursor: Point,
    include_pointer: bool,
    audio: AudioCaptureMode,
    system_audio_available: bool,
    microphone_available: bool,
    pip_layout: PipLayout,
    candidates: Vec<CaptureTarget>,
    selected_target: Option<String>,
    window_pick: Option<String>,
    pending: Vec<CaptureIntent>,
    take: RecordingTake,
    recording_finished_at: Option<Instant>,
    recording_skipped: u64,
    /// Why the recording that ended could not be finished, while its
    /// receipt says so.
    recording_failure: Option<String>,
    /// Whether the recording that ended left a file: one that was not
    /// finished leaves what was written, unless it wrote nothing.
    recording_kept: bool,
    replay_active: bool,
    replay_buffered_seconds: u8,
    replay_audio: AudioCaptureMode,
    screenshot_receipt: Option<ScreenshotReceipt>,
    /// Keeps the receipt alive as the visual source proxy until the shell has
    /// inserted the frozen surface into its own layout tree.
    screenshot_pin_pending: bool,
    screenshot_origin: Option<Point>,
    /// Online devices "Send…" may offer, refreshed by the shell.
    send_devices: Vec<ScreenshotSendDevice>,
    /// The open "Send…" target picker and the target behind each row.
    send_menu: Option<crate::widget::context_menu::ContextMenu>,
    send_menu_targets: Vec<Option<ScreenshotSendTarget>>,
    /// The open options popover and what each of its rows does.
    options_menu: Option<crate::widget::context_menu::ContextMenu>,
    options_choices: Vec<Option<OptionChoice>>,
    delay_seconds: u8,
    armed_capture: Option<ArmedCapture>,
    /// What the recording (or the kept last 30 seconds) takes, outlined
    /// while it runs.
    scope: Option<CaptureScope>,
    status_offset: Vec2,
    status_drag: Option<(Point, Vec2)>,
    /// Render caches only.
    metrics: BarMetrics,
    receipt_widths: ReceiptWidths,

    dirty: bool,
}

impl Default for CaptureStudio {
    fn default() -> Self {
        Self {
            dirty: true,
            rect: Rect::ZERO,
            overlay_rect: Rect::ZERO,
            open: false,
            put_away: false,
            opened_at: None,
            mode: CaptureMode::Screenshot,
            source: SourceMode::Region,
            regions: Vec::new(),
            drag: None,
            cursor: Point::ZERO,
            include_pointer: false,
            audio: AudioCaptureMode::None,
            system_audio_available: false,
            microphone_available: false,
            pip_layout: PipLayout::Auto,
            candidates: Vec::new(),
            selected_target: None,
            window_pick: None,
            pending: Vec::new(),
            take: RecordingTake::default(),
            recording_finished_at: None,
            recording_skipped: 0,
            recording_failure: None,
            recording_kept: true,
            replay_active: false,
            replay_buffered_seconds: 0,
            replay_audio: AudioCaptureMode::None,
            screenshot_receipt: None,
            screenshot_pin_pending: false,
            screenshot_origin: None,
            send_devices: Vec::new(),
            send_menu: None,
            send_menu_targets: Vec::new(),
            options_menu: None,
            options_choices: Vec::new(),
            delay_seconds: 0,
            armed_capture: None,
            scope: None,
            status_offset: Vec2::ZERO,
            status_drag: None,
            metrics: BarMetrics::default(),
            receipt_widths: ReceiptWidths::default(),
        }
    }
}

impl CaptureStudio {
    pub fn set_overlay_rect(&mut self, rect: Rect) {
        self.dirty = true;
        self.overlay_rect = rect;
    }

    fn status_rect(&self) -> Rect {
        if self.overlay_rect.area() > 0.0 {
            self.overlay_rect
        } else {
            self.rect
        }
    }

    pub fn open(&mut self) {
        if self.take.start_pending {
            // Repeating the shortcut while encoder setup is in flight must
            // not restore the modal selector over the desktop.
            return;
        }
        self.armed_capture = None;
        self.end_recording_receipt();
        self.screenshot_receipt = None;
        self.screenshot_pin_pending = false;
        self.options_menu = None;
        self.close_send_menu();
        self.open = true;
        self.put_away = false;
        self.opened_at = Some(crate::time::now());
        self.regions.clear();
        self.drag = None;
        if self.take.active {
            self.mode = CaptureMode::Recording;
        }
        if self.source == SourceMode::Window {
            self.select_focused_window();
        }
    }

    pub fn open_recording_setup(&mut self) {
        self.open();
        if !self.take.active {
            self.set_mode(CaptureMode::Recording);
        }
    }

    pub fn open_pip_setup(&mut self) {
        self.open();
        if !self.take.active {
            self.set_mode(CaptureMode::Pip);
        }
    }

    /// Save the kept last 30 seconds through the same intent as the bar's
    /// Save. Returns whether a ready buffer accepted the action.
    pub fn save_recent_replay(&mut self) -> bool {
        if !self.replay_active || self.replay_buffered_seconds == 0 {
            return false;
        }
        self.queue_replay_command(ReplayCommand::SaveRecent);
        true
    }

    pub fn close(&mut self) {
        self.put_away = true;
        self.open = false;
        self.drag = None;
        self.armed_capture = None;
        self.end_recording_receipt();
        self.options_menu = None;
    }

    pub fn take_put_away(&mut self) -> bool {
        std::mem::take(&mut self.put_away)
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn mode(&self) -> CaptureMode {
        self.mode
    }

    pub fn source_mode(&self) -> SourceMode {
        self.source
    }

    pub fn regions(&self) -> &[Rect] {
        &self.regions
    }

    fn choosing(&self) -> bool {
        self.open
            && !self.take.start_pending
            && !self.take.active
            && !self.replay_active
            && self.screenshot_receipt.is_none()
            && self.recording_finished_at.is_none()
            && self.armed_capture.is_none()
    }

    /// Whether the person is choosing a screenshot, whatever its target.
    /// The compositor holds every window's content still for exactly this
    /// span, so changing the target never starts or ends the hold. Windows
    /// still move: picking one scrolls the strip to it as usual, carrying
    /// its frozen content. A recording or picture in picture stays live, and
    /// so does the countdown of a delayed screenshot.
    pub fn is_choosing_screenshot(&self) -> bool {
        self.mode == CaptureMode::Screenshot && self.choosing()
    }

    pub fn lets_desktop_navigation_through(&self) -> bool {
        self.choosing() && self.source == SourceMode::Window
    }

    pub fn is_recording_active(&self) -> bool {
        self.take.active
    }

    pub fn is_recording_start_pending(&self) -> bool {
        self.take.start_pending
    }

    pub fn is_recording_paused(&self) -> bool {
        self.take.paused
    }

    pub fn is_replay_active(&self) -> bool {
        self.replay_active
    }

    pub fn set_mode(&mut self, mode: CaptureMode) {
        self.dirty = true;
        self.mode = mode;
        self.options_menu = None;
        if mode == CaptureMode::Pip {
            self.include_pointer = false;
            self.audio = AudioCaptureMode::None;
        }
    }

    pub fn set_source_mode(&mut self, source: SourceMode) {
        self.dirty = true;
        self.source = source;
        self.regions.clear();
        self.drag = None;
        self.options_menu = None;
        if source == SourceMode::Window {
            self.select_focused_window();
        }
    }

    pub fn set_audio_available(&mut self, system: bool, microphone: bool) {
        self.dirty = true;
        self.system_audio_available = system;
        self.microphone_available = microphone;
        self.audio = AudioCaptureMode::of(
            system && self.audio.includes_system(),
            microphone && self.audio.includes_microphone(),
        );
    }

    /// The live windows, refreshed by the shell every update. While Window
    /// is chosen, the picked window follows the keyboard focus, so moving
    /// focus the usual way picks another one.
    pub fn set_candidates(&mut self, candidates: Vec<CaptureTarget>) {
        self.dirty = true;
        self.candidates = candidates;
        if self.source == SourceMode::Window {
            self.select_focused_window();
        }
    }

    fn select_focused_window(&mut self) {
        if let Some(focused) = self.candidates.iter().find(|target| target.focused) {
            self.selected_target = Some(focused.id.clone());
            return;
        }
        let alive = self
            .selected_target
            .as_ref()
            .is_some_and(|id| self.candidates.iter().any(|target| &target.id == id));
        if !alive {
            self.selected_target = self
                .candidates
                .iter()
                .find(|target| target.visible_rect.area() > 0.0)
                .or_else(|| self.candidates.first())
                .map(|target| target.id.clone());
        }
    }

    pub fn take_window_pick(&mut self) -> Option<String> {
        self.window_pick.take()
    }

    pub fn take_intents(&mut self) -> Vec<CaptureIntent> {
        if self.armed_capture.as_ref().is_some_and(|armed| {
            crate::time::now().saturating_duration_since(armed.started_at) >= armed.duration
        }) && let Some(armed) = self.armed_capture.take()
        {
            let starts_recording = armed.intent.recording_command == Some(RecordingCommand::Start);
            self.pending.push(armed.intent);
            self.open = false;
            if starts_recording {
                self.begin_recording_request();
            }
        }
        std::mem::take(&mut self.pending)
    }

    fn begin_recording_request(&mut self) {
        self.take = RecordingTake {
            start_pending: true,
            ..RecordingTake::default()
        };
        self.end_recording_receipt();
        self.open = true;
        self.mode = CaptureMode::Recording;
    }

    pub fn set_recording_active(&mut self, active: bool) {
        self.dirty = true;
        if active && !self.take.active {
            self.take = RecordingTake {
                active: true,
                started_at: Some(crate::time::now()),
                ..RecordingTake::default()
            };
            self.end_recording_receipt();
            self.open = true;
            self.mode = CaptureMode::Recording;
        } else if !active {
            self.take = RecordingTake::default();
            self.end_recording_receipt();
            self.open = self.replay_active;
            if !self.replay_active {
                self.scope = None;
            }
        }
    }

    fn end_recording_receipt(&mut self) {
        self.recording_finished_at = None;
        self.recording_skipped = 0;
        self.recording_failure = None;
        self.recording_kept = true;
    }

    pub fn set_recording_finished(&mut self, skipped: u64) {
        self.dirty = true;
        self.take = RecordingTake::default();
        self.recording_skipped = skipped;
        self.recording_finished_at = Some(crate::time::now());
        self.open = true;
        if !self.replay_active {
            self.scope = None;
        }
    }

    /// The recording's file could not be finished: the same receipt says
    /// why, and keeps what was written rather than playing a file that is
    /// not whole. `kept` says whether a file is there to show.
    pub fn set_recording_failed(&mut self, reason: String, kept: bool) {
        self.dirty = true;
        self.take = RecordingTake::default();
        self.recording_failure = Some(reason);
        self.recording_kept = kept;
        self.recording_finished_at = Some(crate::time::now());
        self.open = true;
        if !self.replay_active {
            self.scope = None;
        }
    }

    /// How many frames the recording has had to skip so far because the disk
    /// could not keep up.
    pub fn set_recording_skipped(&mut self, skipped: u64) {
        if self.recording_skipped == skipped {
            return;
        }
        self.dirty = true;
        self.recording_skipped = skipped;
    }

    pub fn set_recording_paused(&mut self, paused: bool) {
        self.dirty = true;
        if !self.take.active || paused == self.take.paused {
            return;
        }
        let now = crate::time::now();
        if paused {
            self.take.paused_at = Some(now);
        } else if let Some(paused_at) = self.take.paused_at.take() {
            self.take.paused_total += now.saturating_duration_since(paused_at);
        }
        self.take.paused = paused;
    }

    fn recording_elapsed(&self) -> Duration {
        let Some(started) = self.take.started_at else {
            return Duration::ZERO;
        };
        let now = crate::time::now();
        let current_pause = self.take.paused_at.map_or(Duration::ZERO, |paused_at| {
            now.saturating_duration_since(paused_at)
        });
        now.saturating_duration_since(started)
            .saturating_sub(self.take.paused_total)
            .saturating_sub(current_pause)
    }

    pub fn set_replay_active(&mut self, active: bool, audio: AudioCaptureMode) {
        self.dirty = true;
        self.replay_active = active;
        self.replay_audio = if active {
            audio
        } else {
            AudioCaptureMode::None
        };
        self.replay_buffered_seconds = 0;
        if !active && !self.take.active {
            self.scope = None;
        }
        if active {
            self.open = true;
        } else if !self.take.active && self.recording_finished_at.is_none() {
            self.open = false;
        }
    }

    pub fn set_replay_buffered_seconds(&mut self, seconds: u8) {
        self.dirty = true;
        if self.replay_active {
            self.replay_buffered_seconds = seconds.min(30);
        }
    }

    pub fn set_replay_saved(&mut self) {
        self.dirty = true;
        if self.replay_active {
            self.recording_skipped = 0;
            self.recording_finished_at = Some(crate::time::now());
        }
    }

    fn queue_recording_command(&mut self, command: RecordingCommand) {
        if self.take.stop_pending
            && !matches!(
                command,
                RecordingCommand::OpenLast | RecordingCommand::RevealLast
            )
        {
            return;
        }
        let mut intent = CaptureIntent::command(CaptureMode::Recording, self.source);
        intent.include_pointer = self.include_pointer;
        intent.recording_command = Some(command);
        self.pending.push(intent);
        if command == RecordingCommand::Stop {
            self.take.stop_pending = true;
        }
    }

    fn queue_replay_command(&mut self, command: ReplayCommand) {
        let mut intent = CaptureIntent::command(CaptureMode::Recording, self.source);
        if command == ReplayCommand::Start {
            let Some(source) = self.chosen_source() else {
                return;
            };
            self.scope = Some(self.scope_of(&source));
            intent.regions = source.regions;
            intent.anchors = source.anchors;
            intent.targets = source.targets;
            intent.audio = self.audio;
            intent.pip_layout = self.combine();
        }
        intent.include_pointer = self.include_pointer;
        intent.replay_command = Some(command);
        self.pending.push(intent);
        if command == ReplayCommand::Start {
            self.close();
        }
    }

    /// The devices "Send…" lists. Called by the shell whenever a screenshot
    /// is saved, so the picker names who is online now.
    pub fn set_screenshot_send_devices(&mut self, devices: Vec<ScreenshotSendDevice>) {
        self.dirty = true;
        self.send_devices = devices;
    }

    pub fn send_last_screenshot(&mut self, target: ScreenshotSendTarget) {
        self.close_send_menu();
        self.queue_screenshot_command(ScreenshotCommand::SendLast(target), None);
    }

    pub fn send_menu_open(&self) -> bool {
        self.send_menu
            .as_ref()
            .is_some_and(|menu| menu.is_showing())
    }

    fn close_send_menu(&mut self) {
        self.send_menu = None;
        self.send_menu_targets.clear();
    }

    /// Open the target picker under the receipt's Send button: the workspace
    /// first (always available), then each online device.
    fn open_send_menu(&mut self, anchor: Rect) {
        use crate::widget::context_menu::{ContextMenu, MenuItem};
        let mut menu = ContextMenu::new();
        let mut targets = Vec::new();
        menu.push(MenuItem::icon_action(
            Icon::FolderOpen,
            studio_text("widget-screenshot-send-workspace"),
            || {},
        ));
        targets.push(Some(ScreenshotSendTarget::Workspace));
        menu.push(MenuItem::separator());
        targets.push(None);
        if self.send_devices.is_empty() {
            menu.push(
                MenuItem::action(studio_text("widget-screenshot-send-no-device"), || {})
                    .with_enabled(false),
            );
            targets.push(None);
        }
        for device in &self.send_devices {
            menu.push(MenuItem::icon_action(
                Icon::Send,
                crate::i18n::widget_format(
                    "widget-screenshot-send-device",
                    &[("device", device.label.as_str().into())],
                ),
                || {},
            ));
            targets.push(Some(ScreenshotSendTarget::Device(device.id.clone())));
        }
        menu.open_anchored(anchor, self.status_rect());
        menu.layout_at_position();
        menu.focus_first_action();
        self.send_menu = Some(menu);
        self.send_menu_targets = targets;
    }

    fn choose_send_target(&mut self, index: usize) {
        let Some(Some(target)) = self.send_menu_targets.get(index).cloned() else {
            return;
        };
        self.send_last_screenshot(target);
        self.screenshot_receipt = None;
        self.open = false;
    }

    pub fn screenshot_kept(&self) -> Option<ScreenshotKept> {
        self.screenshot_receipt.as_ref().map(|receipt| receipt.kept)
    }

    pub fn set_screenshot_finished(
        &mut self,
        kept: ScreenshotKept,
        width: u32,
        height: u32,
        preview: CapturePreview,
    ) {
        self.dirty = true;
        let origin = self
            .screenshot_origin
            .take()
            .unwrap_or_else(|| self.rect.center());
        self.screenshot_receipt = Some(ScreenshotReceipt {
            kept,
            width,
            height,
            preview,
            shown_at: crate::time::now(),
            origin,
            progress_override: None,
        });
        self.screenshot_pin_pending = false;
        self.open = true;
    }

    pub fn set_screenshot_pin_pending(&mut self, pending: bool) {
        self.dirty = true;
        self.screenshot_pin_pending = pending;
    }

    pub fn set_screenshot_pinned(&mut self) {
        self.dirty = true;
        self.screenshot_pin_pending = false;
        self.screenshot_receipt = None;
        self.open = false;
    }

    pub fn set_screenshot_transition_progress(&mut self, permille: u16) {
        self.dirty = true;
        let Some(receipt) = self.screenshot_receipt.as_mut() else {
            return;
        };
        receipt.progress_override = Some(f64::from(permille.min(1_000)) / 1_000.0);
    }

    fn queue_screenshot_command(&mut self, command: ScreenshotCommand, spawn_from: Option<Rect>) {
        let mut intent = CaptureIntent::command(CaptureMode::Screenshot, self.source);
        // Post-capture commands never use this as a recapture source. A
        // command that opens a surface carries only the receipt thumbnail's
        // screen-space rect so the surface can grow out of it.
        intent.regions = spawn_from.into_iter().collect();
        intent.screenshot_command = Some(command);
        self.pending.push(intent);
    }

    fn selected_candidate(&self) -> Option<&CaptureTarget> {
        let id = self.selected_target.as_ref()?;
        self.candidates.iter().find(|target| &target.id == id)
    }

    fn strip_targets(&self) -> Vec<CaptureTarget> {
        self.candidates
            .iter()
            .filter(|target| target.on_active_strip)
            .cloned()
            .collect()
    }

    fn window_holding(&self, region: Rect) -> Option<&CaptureTarget> {
        const SLACK: f64 = 0.5;
        self.candidates.iter().find(|target| {
            let window = target.visible_rect.inflate(SLACK, SLACK);
            window.contains(region.origin()) && window.contains(Point::new(region.x1, region.y1))
        })
    }

    fn chosen_source(&self) -> Option<ChosenSource> {
        match self.source {
            SourceMode::Region => {
                if self.regions.is_empty() {
                    return None;
                }
                let held: Vec<Option<&CaptureTarget>> = self
                    .regions
                    .iter()
                    .map(|region| self.window_holding(*region))
                    .collect();
                let (anchors, targets) = if held.iter().all(Option::is_some) {
                    let mut targets: Vec<CaptureTarget> = Vec::new();
                    let anchors = self
                        .regions
                        .iter()
                        .zip(held.into_iter().flatten())
                        .map(|(region, target)| {
                            if !targets.iter().any(|known| known.id == target.id) {
                                targets.push(target.clone());
                            }
                            RegionAnchor {
                                target: target.id.clone(),
                                local: *region - target.virtual_rect.origin().to_vec2(),
                            }
                        })
                        .collect();
                    (anchors, targets)
                } else {
                    (Vec::new(), Vec::new())
                };
                Some(ChosenSource {
                    regions: self.regions.clone(),
                    anchors,
                    targets,
                })
            }
            SourceMode::Window => {
                let target = self.selected_candidate()?.clone();
                Some(ChosenSource {
                    regions: visible_parts(std::slice::from_ref(&target)),
                    anchors: Vec::new(),
                    targets: vec![target],
                })
            }
            SourceMode::Display => Some(ChosenSource {
                regions: vec![self.status_rect()],
                anchors: Vec::new(),
                targets: Vec::new(),
            }),
            SourceMode::Strip => {
                let targets = self.strip_targets();
                if targets.is_empty() {
                    return None;
                }
                Some(ChosenSource {
                    regions: visible_parts(&targets),
                    anchors: Vec::new(),
                    targets,
                })
            }
        }
    }

    fn scope_of(&self, source: &ChosenSource) -> CaptureScope {
        match self.source {
            SourceMode::Display => CaptureScope::Screen,
            SourceMode::Region if source.anchors.is_empty() => {
                CaptureScope::Regions(source.regions.clone())
            }
            SourceMode::Region => CaptureScope::Anchored(source.anchors.clone()),
            SourceMode::Window | SourceMode::Strip => CaptureScope::Windows(
                source
                    .targets
                    .iter()
                    .map(|target| target.id.clone())
                    .collect(),
            ),
        }
    }

    fn scope_rects(&self) -> Vec<Rect> {
        let window = |id: &str| self.candidates.iter().find(|target| target.id == id);
        let rects = match self.scope.as_ref() {
            None => Vec::new(),
            Some(CaptureScope::Screen) => vec![self.status_rect().inset(-3.0)],
            Some(CaptureScope::Regions(regions)) => regions.clone(),
            Some(CaptureScope::Anchored(anchors)) => anchors
                .iter()
                .filter_map(|anchor| {
                    window(&anchor.target).map(|w| anchor.local + w.virtual_rect.origin().to_vec2())
                })
                .collect(),
            Some(CaptureScope::Windows(ids)) => ids
                .iter()
                .filter_map(|id| window(id).map(|w| w.visible_rect))
                .collect(),
        };
        rects
            .into_iter()
            .map(|rect| rect.intersect(self.status_rect()))
            .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)
            .collect()
    }

    fn has_capture_source(&self) -> bool {
        self.chosen_source().is_some()
    }

    fn active_regions(&self) -> Vec<Rect> {
        let mut out = self.regions.clone();
        if let Some(drag) = self.drag {
            let (index, rect) = self.drag_rect(drag);
            if rect.width() >= MIN_REGION && rect.height() >= MIN_REGION {
                match index.and_then(|index| out.get_mut(index)) {
                    Some(region) => *region = rect,
                    None => out.push(rect),
                }
            }
        }
        out
    }

    /// What the bar must not cover: the committed selections, the picked
    /// window, the strip.
    fn avoid_rect(&self) -> Option<Rect> {
        let rects: Vec<Rect> = match self.source {
            SourceMode::Region => self.regions.clone(),
            SourceMode::Window => self
                .selected_candidate()
                .map(|target| target.visible_rect)
                .into_iter()
                .collect(),
            SourceMode::Strip => visible_parts(&self.strip_targets()),
            SourceMode::Display => Vec::new(),
        };
        rects
            .into_iter()
            .filter(|rect| rect.area() > 0.0)
            .reduce(|bounds, rect| bounds.union(rect))
    }

    fn combine(&self) -> PipLayout {
        if self.source == SourceMode::Region {
            self.pip_layout
        } else {
            PipLayout::Auto
        }
    }

    fn primary_destination(&self) -> CaptureDestination {
        match self.mode {
            CaptureMode::Pip => CaptureDestination::FloatingWindow,
            CaptureMode::Screenshot => CaptureDestination::FileAndClipboard,
            CaptureMode::Recording => CaptureDestination::File,
        }
    }

    fn commit(&mut self, destination: CaptureDestination) {
        let Some(source) = self.chosen_source() else {
            return;
        };
        if self.mode == CaptureMode::Screenshot {
            let center = source
                .regions
                .iter()
                .copied()
                .filter(|rect| rect.area() > 0.0)
                .reduce(|bounds, rect| bounds.union(rect))
                .map_or_else(|| self.rect.center(), |rect| rect.center());
            self.screenshot_origin = Some(center);
        }
        if self.mode == CaptureMode::Recording {
            self.scope = Some(self.scope_of(&source));
        }
        let intent = CaptureIntent {
            mode: self.mode,
            source: self.source,
            destination,
            regions: source.regions,
            anchors: source.anchors,
            include_pointer: self.include_pointer,
            audio: self.audio,
            pip_layout: self.combine(),
            targets: source.targets,
            recording_command: (self.mode == CaptureMode::Recording)
                .then_some(RecordingCommand::Start),
            replay_command: None,
            screenshot_command: None,
        };
        self.drag = None;
        self.options_menu = None;
        if self.delay_seconds > 0 && self.mode != CaptureMode::Pip {
            self.armed_capture = Some(ArmedCapture {
                intent,
                started_at: crate::time::now(),
                duration: Duration::from_secs(u64::from(self.delay_seconds)),
            });
            return;
        }
        let starts_recording = intent.recording_command == Some(RecordingCommand::Start);
        self.pending.push(intent);
        self.close();
        if starts_recording {
            self.begin_recording_request();
        }
    }

    fn selection_bounds(&self) -> Rect {
        self.rect
    }

    fn handles_of(region: Rect) -> SelectionHandles {
        SelectionHandles::new(region, &crate::tokens::Metrics::BASE)
            .with_edges()
            .without_rotation()
    }

    fn handle_at(&self, point: Point) -> Option<(usize, Handle)> {
        self.regions
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, region)| {
                Self::handles_of(*region)
                    .handle_at(point, 1.0)
                    .map(|handle| (index, handle))
            })
    }

    /// Each selection's remove control, just outside its top-right corner,
    /// clear of the corner's handle: only while there are several.
    fn remove_rects(&self) -> Vec<Rect> {
        if self.regions.len() < 2 {
            return Vec::new();
        }
        let side = f64::from(crate::tokens::Metrics::BASE.bar_control_height);
        let gap = f64::from(crate::tokens::Spacing::BASE.s1);
        let bounds = self.selection_bounds();
        self.regions
            .iter()
            .map(|region| {
                let x = (region.x1 + gap).clamp(bounds.x0, bounds.x1 - side);
                let y = (region.y0 - gap - side).clamp(bounds.y0, bounds.y1 - side);
                Rect::new(x, y, x + side, y + side)
            })
            .collect()
    }

    fn remove_shown(&self, index: usize) -> bool {
        let (Some(region), Some(remove)) = (
            self.regions.get(index),
            self.remove_rects().get(index).copied(),
        ) else {
            return false;
        };
        self.drag.is_none() && (region.contains(self.cursor) || remove.contains(self.cursor))
    }

    fn moved_rect(original: Rect, start: Point, current: Point, bounds: Rect) -> Rect {
        let delta = current - start;
        let dx = delta
            .x
            .clamp(bounds.x0 - original.x0, bounds.x1 - original.x1);
        let dy = delta
            .y
            .clamp(bounds.y0 - original.y0, bounds.y1 - original.y1);
        original + Vec2::new(dx, dy)
    }

    fn resized_rect(original: Rect, handle: Handle, current: Point, bounds: Rect) -> Rect {
        let mut rect = original;
        if matches!(handle, Handle::TopLeft | Handle::Left | Handle::BottomLeft) {
            rect.x0 = current.x.clamp(bounds.x0, original.x1 - MIN_REGION);
        }
        if matches!(
            handle,
            Handle::TopRight | Handle::Right | Handle::BottomRight
        ) {
            rect.x1 = current.x.clamp(original.x0 + MIN_REGION, bounds.x1);
        }
        if matches!(handle, Handle::TopLeft | Handle::Top | Handle::TopRight) {
            rect.y0 = current.y.clamp(bounds.y0, original.y1 - MIN_REGION);
        }
        if matches!(
            handle,
            Handle::BottomLeft | Handle::Bottom | Handle::BottomRight
        ) {
            rect.y1 = current.y.clamp(original.y0 + MIN_REGION, bounds.y1);
        }
        rect
    }

    fn drag_rect(&self, drag: Drag) -> (Option<usize>, Rect) {
        let bounds = self.selection_bounds();
        match drag {
            Drag::Create { start, current } => {
                (None, normal_rect(start, current).intersect(bounds))
            }
            Drag::Move {
                index,
                original,
                start,
                current,
            } => (
                Some(index),
                Self::moved_rect(original, start, current, bounds),
            ),
            Drag::Resize {
                index,
                original,
                handle,
                current,
            } => (
                Some(index),
                Self::resized_rect(original, handle, current, bounds),
            ),
        }
    }

    fn finish_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let (index, rect) = self.drag_rect(drag);
        if rect.width() < MIN_REGION || rect.height() < MIN_REGION {
            return;
        }
        match index.and_then(|index| self.regions.get_mut(index)) {
            Some(region) => *region = rect,
            None => self.regions.push(rect),
        }
    }

    fn nudge_region(&mut self, key: NamedKey, amount: f64) -> bool {
        if self.source != SourceMode::Region {
            return false;
        }
        let delta = match key {
            NamedKey::ArrowLeft => Vec2::new(-amount, 0.0),
            NamedKey::ArrowRight => Vec2::new(amount, 0.0),
            NamedKey::ArrowUp => Vec2::new(0.0, -amount),
            NamedKey::ArrowDown => Vec2::new(0.0, amount),
            _ => return false,
        };
        let bounds = self.selection_bounds();
        let Some(region) = self.regions.last_mut() else {
            return false;
        };
        *region = Self::moved_rect(*region, Point::ZERO, Point::ZERO + delta, bounds);
        true
    }

    fn option_sections(&self) -> Vec<OptionSection> {
        let mut sections = Vec::new();
        if self.mode != CaptureMode::Pip {
            sections.push((
                Some(studio_text("widget-capture-delay")),
                DELAY_CHOICES
                    .iter()
                    .map(|seconds| {
                        (
                            delay_choice(*seconds),
                            OptionChoice::Delay(*seconds),
                            Some(*seconds == self.delay_seconds),
                        )
                    })
                    .collect(),
            ));
            let mut include = vec![(
                studio_text("widget-capture-pointer"),
                OptionChoice::Pointer,
                Some(self.include_pointer),
            )];
            if self.mode == CaptureMode::Recording && self.system_audio_available {
                include.push((
                    studio_text("widget-capture-system-audio"),
                    OptionChoice::SystemAudio,
                    Some(self.audio.includes_system()),
                ));
            }
            if self.mode == CaptureMode::Recording && self.microphone_available {
                include.push((
                    studio_text("widget-capture-microphone"),
                    OptionChoice::Microphone,
                    Some(self.audio.includes_microphone()),
                ));
            }
            sections.push((None, include));
        }
        if self.mode == CaptureMode::Recording {
            sections.push((
                None,
                vec![(
                    studio_text("widget-capture-keep-recent"),
                    OptionChoice::KeepRecent,
                    None,
                )],
            ));
        }
        if self.source == SourceMode::Region && self.regions.len() >= 2 {
            sections.push((
                Some(studio_text("widget-capture-combine")),
                [PipLayout::Auto, PipLayout::Grid, PipLayout::Stack]
                    .into_iter()
                    .map(|layout| {
                        (
                            studio_text(layout_key(layout)),
                            OptionChoice::Layout(layout),
                            Some(layout == self.pip_layout),
                        )
                    })
                    .collect(),
            ));
        }
        sections
    }

    fn open_options_menu(&mut self, anchor: Rect) {
        use crate::widget::context_menu::{ContextMenu, MenuItem};
        let sections = self.option_sections();
        if sections.is_empty() {
            return;
        }
        let mut menu = ContextMenu::new();
        let mut choices = Vec::new();
        for (index, (caption, rows)) in sections.into_iter().enumerate() {
            if index > 0 {
                menu.push(MenuItem::separator());
                choices.push(None);
            }
            if let Some(caption) = caption {
                menu.push(MenuItem::caption(caption));
                choices.push(None);
            }
            for (label, choice, checked) in rows {
                let item = match checked {
                    Some(checked) => MenuItem::selection(label).with_checked(checked),
                    None => MenuItem::action(label, || {}).with_enabled(self.has_capture_source()),
                };
                menu.push(item);
                choices.push(Some(choice));
            }
        }
        menu.open_anchored(anchor, self.rect);
        menu.layout_at_position();
        menu.focus_first_action();
        self.options_menu = Some(menu);
        self.options_choices = choices;
    }

    fn choose_option(&mut self, index: usize) {
        let choice = self.options_choices.get(index).copied().flatten();
        self.options_menu = None;
        match choice {
            Some(OptionChoice::Delay(seconds)) => self.delay_seconds = seconds,
            Some(OptionChoice::Pointer) => self.include_pointer = !self.include_pointer,
            Some(OptionChoice::SystemAudio) => {
                self.audio = self.audio.with_system(!self.audio.includes_system());
            }
            Some(OptionChoice::Microphone) => {
                self.audio = self
                    .audio
                    .with_microphone(!self.audio.includes_microphone());
            }
            Some(OptionChoice::KeepRecent) => self.queue_replay_command(ReplayCommand::Start),
            Some(OptionChoice::Layout(layout)) => self.pip_layout = layout,
            None => {}
        }
    }

    fn click(&mut self, p: Point, additive: bool) {
        if self.armed_capture.is_some() {
            return;
        }
        if let Some((item, rect)) = self.bar_items().into_iter().find(|(_, r)| r.contains(p)) {
            match item {
                BarItem::Output(i) => self.set_mode(OUTPUTS[i]),
                BarItem::Target(i) => self.set_source_mode(TARGETS[i]),
                BarItem::Shutter => self.commit(self.primary_destination()),
                BarItem::Options => self.open_options_menu(rect),
                BarItem::Time | BarItem::Save | BarItem::Skipped => {}
            }
            return;
        }
        if self.setup_bar().contains(p) || !self.selection_bounds().contains(p) {
            return;
        }
        match self.source {
            SourceMode::Window => {
                if let Some(target) = self
                    .candidates
                    .iter()
                    .find(|target| target.visible_rect.contains(p))
                {
                    self.selected_target = Some(target.id.clone());
                    self.window_pick = Some(target.id.clone());
                }
            }
            SourceMode::Region => self.press_canvas(p, additive),
            SourceMode::Display | SourceMode::Strip => {}
        }
    }

    fn press_canvas(&mut self, p: Point, additive: bool) {
        if let Some(index) = self
            .remove_rects()
            .iter()
            .enumerate()
            .position(|(index, r)| r.contains(p) && self.remove_shown(index))
        {
            self.regions.remove(index);
            return;
        }
        if !additive {
            if let Some((index, handle)) = self.handle_at(p) {
                self.drag = Some(Drag::Resize {
                    index,
                    original: self.regions[index],
                    handle,
                    current: p,
                });
                return;
            }
            if let Some((index, original)) = self
                .regions
                .iter()
                .copied()
                .enumerate()
                .rev()
                .find(|(_, region)| region.contains(p))
            {
                self.drag = Some(Drag::Move {
                    index,
                    original,
                    start: p,
                    current: p,
                });
                return;
            }
            self.regions.clear();
        }
        self.drag = Some(Drag::Create {
            start: p,
            current: p,
        });
    }

    fn press_receipt(&mut self, p: Point) {
        if let Some(receipt) = self.screenshot_receipt.as_ref() {
            if self.screenshot_pin_pending {
                return;
            }
            let saved = receipt.kept.saved;
            let layout = self.screenshot_receipt_layout();
            let action = layout
                .actions
                .iter()
                .find(|(_, rect)| rect.contains(p))
                .map(|(action, rect)| (*action, *rect));
            let command = match action {
                None if saved && layout.preview.contains(p) => {
                    Some((ScreenshotCommand::MarkupLast, Some(layout.preview)))
                }
                Some((ReceiptAction::Edit, _)) => {
                    Some((ScreenshotCommand::MarkupLast, Some(layout.preview)))
                }
                Some((ReceiptAction::Send, rect)) => {
                    self.open_send_menu(rect);
                    return;
                }
                Some((ReceiptAction::Copy, _)) => Some((ScreenshotCommand::CopyLast, None)),
                Some((ReceiptAction::Open, _)) => Some((ScreenshotCommand::OpenLast, None)),
                Some((ReceiptAction::Reveal, _)) => Some((ScreenshotCommand::RevealLast, None)),
                None if layout.dismiss.contains(p) => None,
                Some((ReceiptAction::Play, _)) | None => return,
            };
            if let Some((command, spawn)) = command {
                self.queue_screenshot_command(command, spawn);
            }
            self.screenshot_receipt = None;
            self.open = self.shows_status();
            self.put_away |= !self.open;
            return;
        }
        if self.recording_finished_at.is_some() {
            let layout = self.recording_receipt_layout();
            let action = layout
                .actions
                .iter()
                .find(|(_, rect)| rect.contains(p))
                .map(|(action, _)| *action);
            match action {
                Some(ReceiptAction::Play) => {
                    self.queue_recording_command(RecordingCommand::OpenLast);
                }
                Some(ReceiptAction::Reveal) => {
                    self.queue_recording_command(RecordingCommand::RevealLast);
                }
                _ if layout.dismiss.contains(p) => {}
                _ => return,
            }
            self.end_recording_receipt();
            self.open = self.shows_status();
            self.put_away |= !self.open;
        }
    }

    fn receipt_pill(&self) -> Option<Rect> {
        if self.screenshot_receipt.is_some() {
            return Some(self.screenshot_receipt_layout().pill);
        }
        self.recording_finished_at
            .map(|_| self.recording_receipt_layout().pill)
    }

    fn appear_progress(&self, theme: &Theme) -> f32 {
        if theme.reduce_motion {
            return 1.0;
        }
        crate::motion::unit_opacity(self.opened_at.map_or(1.0, |at| {
            studio_progress(
                crate::motion::Appear::SPEC,
                crate::time::now().saturating_duration_since(at),
            )
        }))
    }

    /// Dim everything but `holes`: what is chosen stands out, the rest stays
    /// readable.
    fn paint_scrim(&self, scene: &mut Scene, transform: Affine, theme: &Theme, holes: &[Rect]) {
        let mut outside = BezPath::new();
        add_rect(&mut outside, self.rect);
        for hole in holes {
            add_rect(&mut outside, *hole);
        }
        let surfaces = &theme.tokens.colors.surfaces;
        scene.fill(
            Fill::EvenOdd,
            transform,
            surfaces
                .scrim
                .with_alpha(surfaces.scrim_strength * SCRIM_SHARE),
            None,
            &outside,
        );
    }

    fn paint_outline(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
        let radius = f64::from(theme.tokens.radii.tile);
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            theme.border_focus,
            None,
            &RoundedRect::from_rect(rect.inflate(1.0, 1.0), radius + 1.0),
        );
    }

    fn paint_recording_outline(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
        let rule = f64::from(theme.tokens.metrics.chrome_rule_px).max(1.0);
        let radius = f64::from(theme.tokens.radii.tile);
        scene.stroke(
            &Stroke::new(rule * 2.0),
            transform,
            theme.tokens.colors.semantic.danger,
            None,
            &RoundedRect::from_rect(rect.inflate(rule, rule), radius + rule),
        );
    }

    fn paint_canvas(&self, ctx: &mut PaintCtx<'_>) {
        let PaintCtx {
            scene,
            transform,
            text: tr,
            theme,
        } = ctx.reborrow();
        match self.source {
            SourceMode::Region => {
                let regions = self.active_regions();
                if regions.is_empty() {
                    return;
                }
                self.paint_scrim(scene, transform, theme, &regions);
                for region in &regions {
                    Self::handles_of(*region).draw(scene, transform, 1.0, theme);
                }
                let name = studio_text("widget-capture-remove-selection");
                let removes = self.remove_rects();
                for (index, remove) in removes.into_iter().enumerate() {
                    if !self.remove_shown(index) {
                        continue;
                    }
                    let on = crate::widget::paint_see_through(
                        scene,
                        transform,
                        remove,
                        remove.height() / 2.0,
                        theme,
                    );
                    let face = IconButton::face(remove, Icon::X, &name)
                        .control_size(ControlSize::Small)
                        .hovered(remove.contains(self.cursor));
                    Button::paint(scene, transform, tr, &on, &face);
                }
            }
            SourceMode::Window => {
                // Only the picked window: no labels, no list, no panel.
                let Some(target) = self.selected_candidate() else {
                    return;
                };
                let rect = target.visible_rect.intersect(self.rect);
                if rect.area() <= 0.0 {
                    return;
                }
                self.paint_scrim(scene, transform, theme, &[rect]);
                Self::paint_outline(scene, transform, theme, rect);
            }
            SourceMode::Strip => {
                let parts = visible_parts(&self.strip_targets());
                let Some(union) = parts
                    .iter()
                    .copied()
                    .reduce(|bounds, rect| bounds.union(rect))
                else {
                    return;
                };
                let union = union.intersect(self.rect);
                self.paint_scrim(scene, transform, theme, &[union]);
                Self::paint_outline(scene, transform, theme, union);
            }
            SourceMode::Display => {
                Self::paint_outline(scene, transform, theme, self.rect.inset(-3.0));
            }
        }
    }

    /// Paint everything that is not the setup bar: the status bar, the
    /// receipts and the menus. Drawn by the shell after the clean frame a
    /// capture reads, so none of it is ever in a screenshot or a recording.
    pub fn draw_status(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.open {
            return;
        }
        if self.shows_status() {
            // What is being recorded, outlined; painted after the frame the
            // recording reads, so the outline is never in it.
            if self.take.active || self.take.start_pending || self.replay_active {
                for rect in self.scope_rects() {
                    Self::paint_recording_outline(scene, transform, theme, rect);
                }
            }
            self.measure_bar(tr, theme);
            self.paint_bar(&mut PaintCtx::new(scene, transform, tr, theme));
        }
        if self.screenshot_receipt.is_some() {
            self.draw_screenshot_receipt(scene, transform, tr, theme);
        } else if self.recording_finished_at.is_some() {
            self.draw_recording_receipt(scene, transform, tr, theme);
        }
        if let Some(menu) = self.send_menu.as_mut() {
            menu.layout_within(tr, theme);
            menu.draw_overlay(scene, transform, tr, theme, &PresenceCtx::default());
        }
    }

    fn route_menu_event(&mut self, e: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) -> bool {
        let options = self.options_menu.is_some();
        let Some(menu) = self.options_menu.as_mut().or(self.send_menu.as_mut()) else {
            return false;
        };
        let outcome = drive_menu(
            menu,
            self.cursor,
            e,
            (ctx.scale_factor, *ctx.modifiers),
            (&mut ctx.state, ctx.path),
        );
        match (options, outcome.chosen, outcome.dismissed) {
            (true, Some(index), _) => self.choose_option(index),
            (true, None, true) => self.options_menu = None,
            (false, Some(index), _) => self.choose_send_target(index),
            (false, None, true) => self.close_send_menu(),
            (_, None, false) => {}
        }
        outcome.handled
    }

    fn key(&mut self, key: &Key, shift: bool) -> bool {
        match key {
            Key::Named(NamedKey::Escape) => {
                if self.drag.take().is_none() {
                    self.close();
                }
                true
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                self.commit(self.primary_destination());
                true
            }
            Key::Named(
                named @ (NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown),
            ) if self.source == SourceMode::Region => {
                self.nudge_region(*named, if shift { 10.0 } else { 1.0 });
                true
            }
            Key::Named(NamedKey::Backspace | NamedKey::Delete)
                if self.source == SourceMode::Region =>
            {
                self.regions.pop();
                true
            }
            Key::Character(ch) => {
                match ch.as_str().to_ascii_lowercase().as_str() {
                    "s" => self.set_mode(CaptureMode::Screenshot),
                    "r" => self.set_mode(CaptureMode::Recording),
                    "p" => self.set_mode(CaptureMode::Pip),
                    "1" => self.set_source_mode(SourceMode::Region),
                    "2" => self.set_source_mode(SourceMode::Window),
                    "3" => self.set_source_mode(SourceMode::Display),
                    "4" => self.set_source_mode(SourceMode::Strip),
                    _ => return !self.lets_desktop_navigation_through(),
                }
                true
            }
            _ => !self.lets_desktop_navigation_through(),
        }
    }

    /// An event while the bar shows its status form or a receipt shows.
    /// Neither is modal: only what lands on them is theirs.
    fn handle_status_event(&mut self, e: &WindowEvent) -> bool {
        let over = |studio: &Self| {
            (studio.shows_status() && studio.status_bar().contains(studio.cursor))
                || studio
                    .receipt_pill()
                    .is_some_and(|pill| pill.contains(studio.cursor))
        };
        match e {
            WindowEvent::CursorMoved { .. } => {
                if let Some((start, offset)) = self.status_drag {
                    self.status_offset = offset + (self.cursor - start);
                    return true;
                }
                over(self)
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if over(self) => {
                let p = self.cursor;
                if self.receipt_pill().is_some_and(|pill| pill.contains(p)) {
                    self.press_receipt(p);
                } else {
                    self.press_status(p);
                }
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.status_drag.is_some() => {
                self.status_drag = None;
                true
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.armed_capture.take().is_some() {
                    self.open = self.shows_status();
                    return true;
                }
                if self.screenshot_receipt.take().is_some() {
                    self.open = self.shows_status();
                    return true;
                }
                if self.recording_finished_at.is_some() {
                    self.end_recording_receipt();
                    self.open = self.shows_status();
                    return true;
                }
                false
            }
            _ => false,
        }
    }
}

/// What a running recording takes, by what it is, so its outline follows
/// the windows it names.
#[derive(Clone, Debug, PartialEq)]
enum CaptureScope {
    Screen,
    Regions(Vec<Rect>),
    Anchored(Vec<RegionAnchor>),
    Windows(Vec<String>),
}

struct ChosenSource {
    regions: Vec<Rect>,
    anchors: Vec<RegionAnchor>,
    targets: Vec<CaptureTarget>,
}

fn visible_parts(targets: &[CaptureTarget]) -> Vec<Rect> {
    targets
        .iter()
        .map(|target| target.visible_rect)
        .filter(|rect| rect.area() > 0.0)
        .collect()
}

fn overlap(a: Rect, b: Rect) -> f64 {
    let both = a.intersect(b);
    if both.width() > 0.0 && both.height() > 0.0 {
        both.area()
    } else {
        0.0
    }
}

impl Widget for CaptureStudio {
    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        _: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let n = t.leaf_node(Style {
            position: taffy::Position::Absolute,
            size: TaffySize {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        map.insert(n, std::ptr::from_mut::<dyn Widget>(self));
        n
    }

    crate::rect_geometry!();

    fn is_layout_dirty(&self) -> bool {
        false
    }
    fn draw(
        &mut self,
        s: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _: &PresenceCtx,
    ) {
        if !self.choosing() {
            return;
        }
        self.measure_bar(tr, theme);
        // A full-screen surface only fades in (`motion::Appear`).
        s.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            self.appear_progress(theme),
            t,
            &self.rect,
        );
        self.paint_canvas(&mut PaintCtx::new(s, t, tr, theme));
        self.paint_bar(&mut PaintCtx::new(s, t, tr, theme));
        if let Some(menu) = self.options_menu.as_mut() {
            menu.layout_within(tr, theme);
            menu.draw_overlay(s, t, tr, theme, &PresenceCtx::default());
        }
        s.pop_layer();
    }

    fn handle_event(&mut self, e: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.dirty = true;
        if !self.open {
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = e {
            self.cursor = ctx.logical(position);
        }
        if (self.options_menu.is_some() || self.send_menu.is_some())
            && self.route_menu_event(e, ctx)
        {
            *ctx.handled = true;
            return;
        }
        if !self.choosing() {
            if self.handle_status_event(e) {
                *ctx.handled = true;
            }
            return;
        }
        let through = self.lets_desktop_navigation_through();
        let consumed = match e {
            WindowEvent::CursorMoved { .. } => {
                if let Some(drag) = self.drag.as_mut() {
                    drag.set_current(self.cursor);
                }
                !through
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.click(self.cursor, ctx.modifiers.state().shift_key());
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.finish_drag();
                true
            }
            WindowEvent::MouseInput { .. } => true,
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.key(logical_key, ctx.modifiers.state().shift_key()),
            _ => !through,
        };
        *ctx.handled = consumed;
    }

    crate::redraw_flag!();

    fn name(&self) -> &'static str {
        "CaptureStudio"
    }

    fn test_id(&self) -> Option<&str> {
        Some("shell.capture-studio")
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.studio_accessibility()
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.studio_control_rect(path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        self.perform_studio_control(target, action)
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.choosing().then_some(self.rect)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag.is_some() || self.status_drag.is_some()
    }

    fn needs_animation_frame(&self) -> bool {
        self.take.start_pending
            || self.take.active
            || self.replay_active
            || self.armed_capture.is_some()
            || self.screenshot_receipt.as_ref().is_some_and(|receipt| {
                crate::time::now().saturating_duration_since(receipt.shown_at)
                    < SCREENSHOT_RECEIPT_DURATION
            })
            // Frames carry a saved recording's receipt out; one that could not
            // be finished stays as it is until it is put away.
            || (self.recording_finished_at.is_some() && self.recording_failure.is_none())
            || (self.open
                && self.opened_at.is_some_and(|at| {
                    crate::time::now().saturating_duration_since(at)
                        < studio_motion_time(crate::motion::Appear::SPEC)
                }))
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        if !self.open {
            return Vec::new();
        }
        let mut o: Vec<(String, Rect)> = Vec::new();
        if let Some(receipt) = self.screenshot_receipt.as_ref() {
            let layout = self.screenshot_receipt_layout();
            if receipt.kept.saved {
                o.push(("capture.screenshot-thumbnail".into(), layout.preview));
            }
            for (action, rect) in &layout.actions {
                let name = match action {
                    ReceiptAction::Copy => "capture.copy-screenshot",
                    ReceiptAction::Edit => "capture.edit-screenshot",
                    ReceiptAction::Open => "capture.open-screenshot",
                    ReceiptAction::Reveal => "capture.show-screenshot",
                    ReceiptAction::Send => "capture.send-screenshot",
                    ReceiptAction::Play => continue,
                };
                o.push((name.into(), *rect));
            }
            o.push(("capture.dismiss-screenshot".into(), layout.dismiss));
            if let Some(menu) = self.send_menu.as_ref().filter(|menu| menu.is_open()) {
                for (index, target) in self.send_menu_targets.iter().enumerate() {
                    let (Some(target), Some(rect)) = (target, menu.item_rect(index)) else {
                        continue;
                    };
                    o.push((semantics::send_target(target), rect));
                }
            }
        } else if self.recording_finished_at.is_some() {
            let layout = self.recording_receipt_layout();
            for (action, rect) in &layout.actions {
                let name = match action {
                    ReceiptAction::Play => "capture.play-recording",
                    _ => "capture.show-recording",
                };
                o.push((name.into(), *rect));
            }
            o.push(("capture.dismiss-recording".into(), layout.dismiss));
        }
        if self.shows_status() {
            o.push(("capture.status".into(), self.status_bar()));
            for (item, rect) in self.bar_items() {
                let name = match item {
                    BarItem::Shutter if self.replay_active => "capture.stop-replay",
                    BarItem::Shutter => "capture.stop",
                    BarItem::Save => "capture.save-replay",
                    BarItem::Skipped => "capture.recording-skipped",
                    _ => continue,
                };
                o.push((name.into(), rect));
            }
        }
        if !self.choosing() {
            return o;
        }
        o.push(("capture.canvas".into(), self.selection_bounds()));
        o.push(("capture.bar".into(), self.setup_bar()));
        for (item, rect) in self.bar_items() {
            if let Some(name) = semantics::bar_target(item) {
                o.push((name.into(), rect));
            }
        }
        if let Some(menu) = self.options_menu.as_ref().filter(|menu| menu.is_open()) {
            for (index, choice) in self.options_choices.iter().enumerate() {
                let (Some(choice), Some(rect)) = (choice, menu.item_rect(index)) else {
                    continue;
                };
                let name = semantics::option_target(*choice);
                o.push((name, rect));
            }
        }
        match self.source {
            SourceMode::Region => {
                let removes = self.remove_rects();
                for (index, region) in self.regions.iter().enumerate() {
                    o.push((format!("capture.selection.{index}.move"), *region));
                    if let Some(remove) = removes.get(index) {
                        o.push((format!("capture.selection.{index}.remove"), *remove));
                    }
                    let handles = Self::handles_of(*region);
                    for handle in handles.handles(1.0) {
                        let name = match handle {
                            Handle::TopLeft => "nw",
                            Handle::Top => "n",
                            Handle::TopRight => "ne",
                            Handle::Right => "e",
                            Handle::BottomRight => "se",
                            Handle::Bottom => "s",
                            Handle::BottomLeft => "sw",
                            Handle::Left => "w",
                            Handle::Rotate => continue,
                        };
                        let side = f64::from(crate::tokens::Metrics::BASE.min_pointer_target);
                        o.push((
                            format!("capture.selection.{index}.{name}"),
                            Rect::from_center_size(handles.position(handle, 1.0), (side, side)),
                        ));
                    }
                }
            }
            SourceMode::Window => {
                if let Some(target) = self.selected_candidate() {
                    o.push(("capture.window-pick".into(), target.visible_rect));
                }
                for target in &self.candidates {
                    let rect = target.visible_rect.intersect(self.rect);
                    if rect.area() > 0.0 {
                        o.push((format!("capture.app:{}", target.title), rect));
                    }
                }
            }
            SourceMode::Display | SourceMode::Strip => {}
        }
        o
    }
}

struct MenuOutcome {
    handled: bool,
    dismissed: bool,
    chosen: Option<usize>,
}

fn drive_menu(
    menu: &mut crate::widget::context_menu::ContextMenu,
    cursor: Point,
    e: &WindowEvent,
    (scale, modifiers): (f64, winit::event::Modifiers),
    (state, path): (&mut crate::widget::Edits<'_>, &ObjId),
) -> MenuOutcome {
    let dismissed = MenuOutcome {
        handled: true,
        dismissed: true,
        chosen: None,
    };
    if let WindowEvent::KeyboardInput {
        event:
            KeyEvent {
                logical_key: Key::Named(NamedKey::Escape),
                state: ElementState::Pressed,
                ..
            },
        ..
    } = e
    {
        return dismissed;
    }
    if let WindowEvent::MouseInput {
        state: ElementState::Pressed,
        button: MouseButton::Left,
        ..
    } = e
        && !menu.contains([cursor.x, cursor.y])
    {
        return dismissed;
    }
    let mut taffy = TaffyTree::new();
    let Ok(node) = taffy.new_leaf(Style::default()) else {
        return dismissed;
    };
    let mut handled = false;
    let mut focus = None;
    menu.handle_event(
        e,
        &mut crate::widget::EventCtx {
            scale_factor: scale,
            modifiers: &modifiers,
            state: state.reborrow(),
            path,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
    // The menu leaves motion unhandled so presence can follow it; over the
    // menu the motion is still the menu's, not the widgets below.
    if matches!(e, WindowEvent::CursorMoved { .. }) && menu.contains([cursor.x, cursor.y]) {
        handled = true;
    }
    MenuOutcome {
        handled,
        dismissed: false,
        chosen: menu.take_selection(),
    }
}

mod bar;
mod geometry;
mod motion;
mod receipt;
use bar::{BarItem, BarMetrics, delay_choice, layout_key, studio_text};
use geometry::{add_rect, normal_rect};
use motion::{leaving_opacity, studio_motion_time, studio_progress};
use receipt::{ReceiptAction, ReceiptWidths};
mod semantics;

#[cfg(test)]
mod tests;
