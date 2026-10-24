//! The capture tool: one see-through bar to take a screenshot, record, or
//! float a live picture in picture of a selection, a window, the screen or
//! the strip.
//!
//! The bar holds two segmented controls (what comes out, what it is of), the
//! shutter and an options popover; nothing else. A capture is of a thing,
//! never of pixels at a position: a window is captured whole wherever it is,
//! a selection made inside a window stays anchored to that window, and only
//! a selection that is not inside any window is a region of the screen.
//! Pixel readback stays in the compositor; this widget emits semantic
//! intents.
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

/// What became of a screenshot: kept as a file in Screenshots, put on the
/// clipboard, or both (what the shutter and Print Screen do).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScreenshotKept {
    pub saved: bool,
    /// On the clipboard.
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

    /// The receipt's status line.
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
    /// Pinned flight progress for deterministic review.
    progress_override: Option<f64>,
}

/// What a capture puts out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureMode {
    #[default]
    Screenshot,
    Recording,
    /// A live, continuously refreshed floating surface of the target.
    Pip,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceMode {
    /// One or more dragged selections. Hold Shift to add another.
    #[default]
    Region,
    /// One whole window, including what is scrolled off screen.
    Window,
    /// The whole output.
    Display,
    /// The whole strip of the active workspace, every window side by side.
    Strip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureDestination {
    File,
    Clipboard,
    FloatingWindow,
    /// Save to the screenshots folder *and* put the pixels on the
    /// clipboard: what the shutter and the instant Shift/Alt+Print captures
    /// do.
    FileAndClipboard,
}

/// The live windows a capture repaints whole, outside the output clip.
#[derive(Clone, Copy, Debug)]
pub struct LiveCapture<'a> {
    /// `group:<group>:tab:<tab>` ids of the windows.
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

/// How several selections are combined into one capture.
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

/// A live window the capture tool can take, whether or not it is on this
/// output now. `virtual_rect` is its place in Tilo's continuous desktop;
/// `visible_rect` is the part of it on this output.
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
    /// The window holds the keyboard focus: the one Window captures.
    pub focused: bool,
    /// The window is on the active workspace's active strip: part of what
    /// Strip captures.
    pub on_active_strip: bool,
}

impl CaptureTarget {
    pub fn full_rect(&self) -> Rect {
        self.complete_rect.unwrap_or(self.virtual_rect)
    }
}

/// A selection made inside a window, in that window's own coordinates: it
/// goes with the window wherever the window moves or scrolls to.
#[derive(Clone, Debug, PartialEq)]
pub struct RegionAnchor {
    /// The window's [`CaptureTarget::id`].
    pub target: String,
    /// The selection relative to the window's top-left corner.
    pub local: Rect,
}

/// Commands for the compositor-owned recording session. Keeping controls in
/// the same semantic intent stream as Start makes shell chrome, shortcuts and
/// future remote controls share one state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingCommand {
    Start,
    Pause,
    Resume,
    Stop,
    /// Open the finished video itself in the user's preferred player.
    OpenLast,
    RevealLast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayCommand {
    Start,
    SaveRecent,
    Stop,
}

/// Where "Send…" delivers the last screenshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenshotSendTarget {
    /// Save a copy into the active workspace's shared files.
    Workspace,
    /// Offer it to one of your certified devices; the receiver still decides.
    Device(String),
}

/// One of your online devices, as the shell offers it to "Send…".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenshotSendDevice {
    /// Opaque device-fabric identity the shell resolves again when sending.
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenshotCommand {
    /// Open the exact saved artifact in the shared native markup workspace.
    MarkupLast,
    /// Open the finished artifact itself in the user's preferred image app.
    /// This is distinct from `RevealLast`, which opens its containing folder.
    OpenLast,
    /// Turn the exact finished pixels into a frozen reference surface. This
    /// must never be implemented as a live recapture/`PiP` source.
    PinLast,
    RevealLast,
    /// Copy the already-rendered last screenshot without recapturing a live
    /// scene that may have changed in the meantime.
    CopyLast,
    /// Deliver the saved artifact itself to a device or the workspace.
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
    /// How several selections are combined.
    pub pip_layout: PipLayout,
    /// The windows captured whole (Window, Strip) or that anchored
    /// selections are in.
    pub targets: Vec<CaptureTarget>,
    /// `Some` only for recorder lifecycle work. Source fields remain
    /// populated on Start and may be empty for Stop.
    pub recording_command: Option<RecordingCommand>,
    /// Retrospective capture is deliberately a separate lifecycle: arming a
    /// bounded private buffer is not presented as starting a recording.
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

/// A drag in progress on the selection canvas.
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

/// What one row of the options popover does when chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OptionChoice {
    Delay(u8),
    Pointer,
    SystemAudio,
    Microphone,
    KeepRecent,
    Layout(PipLayout),
}

/// One row of the options popover: its label, what it does, and whether it
/// is checked (`None` for an action).
type OptionRow = (String, OptionChoice, Option<bool>);
/// A section of the options popover: its caption and rows.
type OptionSection = (Option<String>, Vec<OptionRow>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarItem {
    Output(usize),
    Target(usize),
    Shutter,
    Options,
    /// The elapsed time of a recording, or the seconds left of a delay.
    Time,
    /// Save the kept last 30 seconds.
    Save,
}

/// The bar's measures in the theme it was last painted with. Render caches
/// only; before the first paint, the stock theme's.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BarMetrics {
    pad: f64,
    gap: f64,
    margin: f64,
    radius: f64,
    strip_h: f64,
    output_w: f64,
    target_w: f64,
    shutter: f64,
    icon_button: f64,
    time_w: f64,
    save_w: f64,
}

impl Default for BarMetrics {
    fn default() -> Self {
        let spacing = crate::tokens::Spacing::BASE;
        let metrics = crate::tokens::Metrics::BASE;
        Self {
            pad: f64::from(spacing.s2),
            gap: f64::from(spacing.s4),
            margin: f64::from(spacing.s6),
            radius: f64::from(crate::tokens::Radii::default().panel),
            strip_h: 56.0,
            output_w: 96.0,
            target_w: 80.0,
            shutter: f64::from(metrics.min_pointer_target) * 2.0,
            icon_button: f64::from(metrics.min_pointer_target),
            time_w: 64.0,
            save_w: 150.0,
        }
    }
}

/// A string from the toolkit's catalogue.
fn studio_text(key: &str) -> String {
    crate::i18n::widget_format(key, &[])
}

fn output_key(mode: CaptureMode) -> &'static str {
    match mode {
        CaptureMode::Screenshot => "widget-capture-mode-screenshot",
        CaptureMode::Recording => "widget-capture-mode-recording",
        CaptureMode::Pip => "widget-capture-mode-pip",
    }
}

fn output_icon(mode: CaptureMode) -> Icon {
    match mode {
        CaptureMode::Screenshot => Icon::Camera,
        CaptureMode::Recording => Icon::CircleDot,
        CaptureMode::Pip => Icon::PictureInPicture2,
    }
}

/// The output's full name, for its tooltip: the bar's label for picture in
/// picture is its short form.
fn output_name(mode: CaptureMode) -> String {
    match mode {
        CaptureMode::Pip => studio_text("widget-capture-mode-pip-name"),
        mode => studio_text(output_key(mode)),
    }
}

fn target_key(source: SourceMode) -> &'static str {
    match source {
        SourceMode::Region => "widget-capture-source-region",
        SourceMode::Window => "widget-capture-source-window",
        SourceMode::Display => "widget-capture-source-display",
        SourceMode::Strip => "widget-capture-source-strip",
    }
}

fn target_icon(source: SourceMode) -> Icon {
    match source {
        SourceMode::Region => Icon::SquareDashed,
        SourceMode::Window => Icon::AppWindow,
        SourceMode::Display => Icon::Monitor,
        SourceMode::Strip => Icon::GalleryHorizontal,
    }
}

fn output_segments() -> Vec<Segment> {
    OUTPUTS
        .iter()
        .map(|mode| Segment::new(studio_text(output_key(*mode))).with_icon(output_icon(*mode)))
        .collect()
}

fn target_segments() -> Vec<Segment> {
    TARGETS
        .iter()
        .map(|source| {
            Segment::new(studio_text(target_key(*source))).with_icon(target_icon(*source))
        })
        .collect()
}

/// A delay's row in the options popover.
fn delay_choice(seconds: u8) -> String {
    if seconds == 0 {
        studio_text("widget-capture-delay-none")
    } else {
        crate::i18n::widget_format(
            "widget-capture-delay-choice",
            &[("seconds", i64::from(seconds).into())],
        )
    }
}

fn layout_key(layout: PipLayout) -> &'static str {
    match layout {
        PipLayout::Auto => "widget-capture-layout-auto",
        PipLayout::Grid => "widget-capture-layout-grid",
        PipLayout::Stack => "widget-capture-layout-stack",
    }
}

/// `label  shortcut`, the way the shell's tooltips name a shortcut.
fn ui_count(n: usize) -> f64 {
    crate::num::f64_of_usize(n)
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent capture, recording, replay and receipt states are retained together"
)]
pub struct CaptureStudio {
    rect: Rect,
    overlay_rect: Rect,
    open: bool,
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
    /// A window the person clicked, for the shell to focus and scroll into
    /// view ([`Self::take_window_pick`]).
    window_pick: Option<String>,
    pending: Vec<CaptureIntent>,
    /// The current take; reset whole whenever one starts or ends.
    take: RecordingTake,
    recording_finished_at: Option<Instant>,
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
    /// Where the person moved the bar while recording, from its place at
    /// the bottom centre.
    status_offset: Vec2,
    /// A drag of the bar: where it started and the offset then.
    status_drag: Option<(Point, Vec2)>,
    /// Render caches only.
    metrics: BarMetrics,
    receipt_widths: ReceiptWidths,
}

impl Default for CaptureStudio {
    fn default() -> Self {
        Self {
            rect: Rect::ZERO,
            overlay_rect: Rect::ZERO,
            open: false,
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
    /// Capture status belongs to compositor space, not to the picker node's
    /// current layout parent. The latter can move back into the content area
    /// as soon as modal selection closes.
    pub fn set_overlay_rect(&mut self, rect: Rect) {
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
        self.recording_finished_at = None;
        self.screenshot_receipt = None;
        self.screenshot_pin_pending = false;
        self.options_menu = None;
        self.open = true;
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

    /// Global recording shortcut entry point.
    pub fn open_recording_setup(&mut self) {
        self.open();
        if !self.take.active {
            self.set_mode(CaptureMode::Recording);
        }
    }

    /// Global picture-in-picture shortcut entry point.
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
        self.open = false;
        self.drag = None;
        self.armed_capture = None;
        self.recording_finished_at = None;
        self.options_menu = None;
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

    /// The bar is up and the person is choosing what to capture.
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

    /// Whether the desktop's own navigation (moving focus with the keys,
    /// scrolling the strip) reaches the desktop while the tool is up: while
    /// a window is being chosen, that navigation is how one is chosen.
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
        self.mode = mode;
        self.options_menu = None;
        if mode == CaptureMode::Pip {
            // A live surface shows no pointer trail and plays no audio.
            self.include_pointer = false;
            self.audio = AudioCaptureMode::None;
        }
    }

    pub fn set_source_mode(&mut self, source: SourceMode) {
        self.source = source;
        self.regions.clear();
        self.drag = None;
        self.options_menu = None;
        if source == SourceMode::Window {
            self.select_focused_window();
        }
    }

    pub fn set_audio_available(&mut self, system: bool, microphone: bool) {
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
        self.candidates = candidates;
        if self.source == SourceMode::Window {
            self.select_focused_window();
        }
    }

    /// Pick the focused window, else keep the picked one while it lives,
    /// else the first one on screen.
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

    /// The window the person clicked while choosing one: the shell focuses
    /// it and scrolls it into view with the standard motion.
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
        self.recording_finished_at = None;
        self.open = true;
        self.mode = CaptureMode::Recording;
    }

    pub fn set_recording_active(&mut self, active: bool) {
        if active && !self.take.active {
            self.take = RecordingTake {
                active: true,
                started_at: Some(crate::time::now()),
                ..RecordingTake::default()
            };
            self.recording_finished_at = None;
            // The picker closed before the compositor started the encoder;
            // the bar comes back as a non-modal status.
            self.open = true;
            self.mode = CaptureMode::Recording;
        } else if !active {
            self.take = RecordingTake::default();
            self.recording_finished_at = None;
            self.open = self.replay_active;
            if !self.replay_active {
                self.scope = None;
            }
        }
    }

    /// The compositor flushed the recording: the bar gives way to a receipt
    /// that plays it or shows it in Files. Failures use
    /// `set_recording_active(false)` instead, so a receipt never shows for an
    /// incomplete file.
    pub fn set_recording_finished(&mut self) {
        if !self.take.active {
            return;
        }
        self.take = RecordingTake::default();
        self.recording_finished_at = Some(crate::time::now());
        self.open = true;
        if !self.replay_active {
            self.scope = None;
        }
    }

    pub fn set_recording_paused(&mut self, paused: bool) {
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
        if self.replay_active {
            self.replay_buffered_seconds = seconds.min(30);
        }
    }

    /// The kept seconds were saved: the receipt offers to play them.
    pub fn set_replay_saved(&mut self) {
        if self.replay_active {
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
        self.send_devices = devices;
    }

    /// Send the last saved screenshot without the picker — the action door
    /// for automation and assistive clients, and the picker's own result.
    pub fn send_last_screenshot(&mut self, target: ScreenshotSendTarget) {
        self.close_send_menu();
        self.queue_screenshot_command(ScreenshotCommand::SendLast(target), None);
    }

    /// Whether the "Send…" target picker is open.
    pub fn send_menu_open(&self) -> bool {
        self.send_menu.as_ref().is_some_and(|menu| menu.is_open())
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

    /// What the receipt shown says became of the screenshot; `None` while
    /// none shows.
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
        self.screenshot_pin_pending = pending;
    }

    pub fn set_screenshot_pinned(&mut self) {
        self.screenshot_pin_pending = false;
        self.screenshot_receipt = None;
        self.open = false;
    }

    /// Pin the selected-pixels-to-receipt handoff for deterministic visual
    /// review. Live presentation continues to use elapsed time.
    pub fn set_screenshot_transition_progress(&mut self, permille: u16) {
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

    /// The windows of the active strip, side by side.
    fn strip_targets(&self) -> Vec<CaptureTarget> {
        self.candidates
            .iter()
            .filter(|target| target.on_active_strip)
            .cloned()
            .collect()
    }

    /// The window `region` lies wholly inside, if any.
    fn window_holding(&self, region: Rect) -> Option<&CaptureTarget> {
        const SLACK: f64 = 0.5;
        self.candidates.iter().find(|target| {
            let window = target.visible_rect.inflate(SLACK, SLACK);
            window.contains(region.origin()) && window.contains(Point::new(region.x1, region.y1))
        })
    }

    /// What the current target captures, or `None` while nothing is chosen.
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

    /// Where what is being recorded is on screen now.
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

    /// The selections as they are now, the one being dragged included.
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

    /// How the chosen things are combined: several selections as the
    /// person asked, the strip's windows always as they stand.
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

    /// Whether selection `index`'s remove control shows: while the pointer
    /// is on that selection or on the control.
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

    fn measure_bar(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let spacing = &theme.tokens.spacing;
        let outputs: Vec<String> = OUTPUTS
            .iter()
            .map(|mode| studio_text(output_key(*mode)))
            .collect();
        let targets: Vec<String> = TARGETS
            .iter()
            .map(|source| studio_text(target_key(*source)))
            .collect();
        let outputs: Vec<&str> = outputs.iter().map(String::as_str).collect();
        let targets: Vec<&str> = targets.iter().map(String::as_str).collect();
        let size = ControlSize::Regular;
        let label = theme.text(TextRole::Label);
        let time_w = f64::from(tr.measure_style("0:00:00", None, &label).0).ceil();
        let save_label = studio_text("widget-replay-save");
        let save_w = ButtonFace::new(Rect::ZERO, &save_label)
            .icon(Icon::Download)
            .control_size(size)
            .intrinsic_width(tr, theme);
        self.metrics = BarMetrics {
            pad: f64::from(spacing.s2),
            gap: f64::from(spacing.s4),
            margin: f64::from(spacing.s6),
            radius: f64::from(theme.tokens.radii.panel),
            strip_h: SegmentedFace::stacked_height(size, theme),
            output_w: SegmentedFace::stacked_segment_width(&outputs, size, tr, theme),
            target_w: SegmentedFace::stacked_segment_width(&targets, size, tr, theme),
            shutter: CaptureButtonFace::diameter(theme),
            icon_button: size.height(theme),
            time_w,
            save_w,
        };
    }

    /// The bar's height: its tallest part and its padding.
    fn bar_height(&self) -> f64 {
        let m = &self.metrics;
        m.strip_h.max(m.shutter) + 2.0 * m.pad
    }

    /// The setup bar: bottom centre, moved to the top when it would cover
    /// what is chosen and the top is clear of it.
    fn setup_bar(&self) -> Rect {
        let m = &self.metrics;
        let w = 2.0 * m.pad + 2.0 * self.setup_side() + m.shutter;
        let w = w.min(self.rect.width());
        let h = self.bar_height();
        let cx = self.rect.center().x;
        let bottom = Rect::new(
            cx - w / 2.0,
            self.rect.y1 - m.margin - h,
            cx + w / 2.0,
            self.rect.y1 - m.margin,
        );
        let top = Rect::new(
            cx - w / 2.0,
            self.rect.y0 + m.margin,
            cx + w / 2.0,
            self.rect.y0 + m.margin + h,
        );
        match self.avoid_rect() {
            // Only to a place that is clear: over a window filling the
            // screen, both cover it, and the bottom is where it rests.
            Some(avoid) if overlap(bottom, avoid) > 0.0 && overlap(top, avoid) <= 0.0 => top,
            _ => bottom,
        }
    }

    /// Whether the tool's floating parts are at `p`: a screenshot's or a
    /// recording's receipt, or the bar while recording. They hang over the
    /// windows, so a press there, from the pointer or a finger, is the
    /// tool's and never the window's beneath.
    pub fn takes_pointer_at(&self, p: Point) -> bool {
        self.open
            && ((self.screenshot_receipt.is_some()
                && self.screenshot_receipt_layout().pill.contains(p))
                || (self.recording_finished_at.is_some()
                    && self.recording_receipt_layout().pill.contains(p))
                || (self.shows_status() && self.status_bar().contains(p)))
    }

    /// How far each half of the setup bar reaches from the shutter: the
    /// wider of the outputs, or the targets and the options, with the gap
    /// before the shutter. Both halves are this wide, so the shutter is the
    /// bar's centre whatever is chosen; each group packs toward the shutter
    /// at its own width, and the bar's outer padding takes the difference.
    fn setup_side(&self) -> f64 {
        let m = &self.metrics;
        let outputs = OUTPUTS.len() as f64 * m.output_w;
        let targets = TARGETS.len() as f64 * m.target_w + m.gap + m.icon_button;
        outputs.max(targets) + m.gap
    }

    /// The bar while recording, counting down or keeping the last 30
    /// seconds: the shutter and a time (or Save), where the person moved it.
    fn status_bar(&self) -> Rect {
        let m = &self.metrics;
        let content = if self.replay_active {
            m.save_w
        } else {
            m.time_w
        };
        let w = 2.0 * m.pad + content + m.gap + m.shutter;
        let h = self.bar_height();
        let bounds = self.status_rect();
        let x0 = bounds.center().x - w / 2.0 + self.status_offset.x;
        let y0 = bounds.y1 - m.margin - h + self.status_offset.y;
        let x0 = x0.clamp(bounds.x0, (bounds.x1 - w).max(bounds.x0));
        let y0 = y0.clamp(bounds.y0, (bounds.y1 - h).max(bounds.y0));
        Rect::new(x0, y0, x0 + w, y0 + h)
    }

    /// The bar shows its status form: a recording, its start, a countdown,
    /// or the kept last 30 seconds.
    fn shows_status(&self) -> bool {
        self.open
            && (self.take.start_pending
                || self.take.active
                || self.replay_active
                || self.armed_capture.is_some())
    }

    /// Where each thing on the bar sits now.
    fn bar_items(&self) -> Vec<(BarItem, Rect)> {
        let m = self.metrics;
        if self.shows_status() {
            let bar = self.status_bar();
            let cy = bar.center().y;
            let mut x = bar.x0 + m.pad;
            let mut items = Vec::with_capacity(2);
            if self.replay_active {
                let h = m.icon_button;
                items.push((
                    BarItem::Save,
                    Rect::new(x, cy - h / 2.0, x + m.save_w, cy + h / 2.0),
                ));
                x += m.save_w;
            } else {
                items.push((
                    BarItem::Time,
                    Rect::new(x, bar.y0 + m.pad, x + m.time_w, bar.y1 - m.pad),
                ));
                x += m.time_w;
            }
            x += m.gap;
            items.push((
                BarItem::Shutter,
                Rect::from_center_size(Point::new(x + m.shutter / 2.0, cy), (m.shutter, m.shutter)),
            ));
            return items;
        }
        if !self.choosing() {
            return Vec::new();
        }
        let bar = self.setup_bar();
        let cy = bar.center().y;
        let strip =
            |x: f64, w: f64| Rect::new(x, cy - m.strip_h / 2.0, x + w, cy + m.strip_h / 2.0);
        // The shutter is the bar's centre: what comes out on its left, what
        // it is of and the options on its right.
        let cx = bar.center().x;
        let mut items = Vec::with_capacity(9);
        let mut x = cx - m.shutter / 2.0 - m.gap - OUTPUTS.len() as f64 * m.output_w;
        for i in 0..OUTPUTS.len() {
            items.push((BarItem::Output(i), strip(x, m.output_w)));
            x += m.output_w;
        }
        items.push((
            BarItem::Shutter,
            Rect::from_center_size(Point::new(cx, cy), (m.shutter, m.shutter)),
        ));
        x = cx + m.shutter / 2.0 + m.gap;
        for i in 0..TARGETS.len() {
            items.push((BarItem::Target(i), strip(x, m.target_w)));
            x += m.target_w;
        }
        x += m.gap;
        items.push((
            BarItem::Options,
            Rect::new(
                x,
                cy - m.icon_button / 2.0,
                x + m.icon_button,
                cy + m.icon_button / 2.0,
            ),
        ));
        items
    }

    fn bar_item(&self, item: BarItem) -> Rect {
        self.bar_items()
            .into_iter()
            .find(|(it, _)| *it == item)
            .map_or(Rect::ZERO, |(_, rect)| rect)
    }

    /// What the shutter does now.
    fn shutter_kind(&self) -> CaptureKind {
        if self.take.active || self.take.start_pending || self.replay_active {
            return CaptureKind::Recording;
        }
        match self.mode {
            CaptureMode::Screenshot => CaptureKind::Photo,
            CaptureMode::Recording => CaptureKind::Video,
            CaptureMode::Pip => CaptureKind::Pip,
        }
    }

    fn shutter_name(&self) -> String {
        if self.replay_active {
            return studio_text("widget-replay-stop");
        }
        if self.take.active || self.take.start_pending {
            return studio_text("widget-capture-stop");
        }
        studio_text(match self.mode {
            CaptureMode::Screenshot => "widget-capture-take",
            CaptureMode::Recording => "widget-capture-record",
            CaptureMode::Pip => "widget-capture-create-pip",
        })
    }

    /// Whether the shutter can be pressed now.
    fn shutter_ready(&self) -> bool {
        if self.replay_active {
            return true;
        }
        if self.take.start_pending || self.armed_capture.is_some() {
            return false;
        }
        if self.take.active {
            return !self.take.stop_pending;
        }
        self.has_capture_source()
    }

    /// The tooltip of the bar's item under the pointer: what it is and its
    /// shortcut.
    fn bar_tip(&self) -> Option<(Rect, String)> {
        if self.options_menu.is_some() || self.send_menu.is_some() {
            return None;
        }
        let (item, rect) = self
            .bar_items()
            .into_iter()
            .find(|(_, rect)| rect.contains(self.cursor))?;
        // No shortcut helpers in this tool. The outputs and targets carry
        // their labels, so only the unlabelled shutter and options are named,
        // and an output whose label is a short form (PiP) spells it out.
        let tip = match item {
            BarItem::Shutter => self.shutter_name(),
            BarItem::Options => studio_text("widget-capture-options"),
            BarItem::Output(i) => {
                let name = output_name(OUTPUTS[i]);
                if name == studio_text(output_key(OUTPUTS[i])) {
                    return None;
                }
                name
            }
            BarItem::Target(_) | BarItem::Time | BarItem::Save => return None,
        };
        Some((rect, tip))
    }

    /// The options that apply now, in sections.
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
                BarItem::Time | BarItem::Save => {}
            }
            return;
        }
        // The bar's own ground is not the canvas.
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

    /// A press on the selection canvas: a selection's remove control, a
    /// handle, a selection to move, or a new selection. Shift adds a new
    /// selection to the ones there are; without it a new one replaces them.
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

    /// A press on the status bar: the shutter stops, Save saves, anywhere
    /// else picks the bar up to move it out of the way.
    fn press_status(&mut self, p: Point) {
        let items = self.bar_items();
        match items
            .iter()
            .find(|(_, rect)| rect.contains(p))
            .map(|(i, _)| *i)
        {
            Some(BarItem::Shutter) if self.replay_active => {
                self.queue_replay_command(ReplayCommand::Stop);
            }
            Some(BarItem::Shutter) if self.take.active && !self.take.stop_pending => {
                self.queue_recording_command(RecordingCommand::Stop);
            }
            Some(BarItem::Save) => {
                let _ = self.save_recent_replay();
            }
            Some(BarItem::Shutter) => {}
            _ => self.status_drag = Some((p, self.status_offset)),
        }
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
            self.recording_finished_at = None;
            self.open = self.shows_status();
        }
    }

    /// The receipt shown now, if any: where it is.
    fn receipt_pill(&self) -> Option<Rect> {
        if self.screenshot_receipt.is_some() {
            return Some(self.screenshot_receipt_layout().pill);
        }
        self.recording_finished_at
            .map(|_| self.recording_receipt_layout().pill)
    }

    /// Paint the bar: the setup form while choosing, the status form while
    /// recording, counting down or keeping the last 30 seconds.
    fn paint_bar(&self, ctx: &mut PaintCtx<'_>) {
        let PaintCtx {
            scene,
            transform,
            text: tr,
            theme,
        } = ctx.reborrow();
        let status = self.shows_status();
        let bar = if status {
            self.status_bar()
        } else {
            self.setup_bar()
        };
        let on =
            crate::widget::paint_see_through(scene, transform, bar, self.metrics.radius, theme);
        let items = self.bar_items();
        let hovered = |item: BarItem| {
            items
                .iter()
                .any(|(it, rect)| *it == item && rect.contains(self.cursor))
        };
        let rects = |pick: fn(BarItem) -> bool| -> Vec<Rect> {
            items
                .iter()
                .filter(|(item, _)| pick(*item))
                .map(|(_, rect)| *rect)
                .collect()
        };
        if !status {
            let outputs = rects(|item| matches!(item, BarItem::Output(_)));
            let output_items = output_segments();
            let chosen = OUTPUTS.iter().position(|m| *m == self.mode).unwrap_or(0);
            crate::widget::ToggleGroup::paint(
                scene,
                transform,
                tr,
                &on,
                &SegmentedFace::of_segments(&outputs, &output_items, chosen)
                    .stacked(true)
                    .style(SegmentedStyle::Quiet)
                    .hovered(outputs.iter().position(|r| r.contains(self.cursor))),
            );
            let targets = rects(|item| matches!(item, BarItem::Target(_)));
            let target_items = target_segments();
            let chosen = TARGETS.iter().position(|s| *s == self.source).unwrap_or(0);
            crate::widget::ToggleGroup::paint(
                scene,
                transform,
                tr,
                &on,
                &SegmentedFace::of_segments(&targets, &target_items, chosen)
                    .stacked(true)
                    .style(SegmentedStyle::Quiet)
                    .hovered(targets.iter().position(|r| r.contains(self.cursor))),
            );
            // Two separate choices with the shutter between them, set off
            // by a rule on each side: what comes out, then what it is of.
            let ground = crate::chrome::panel_ground(theme);
            let inset = f64::from(theme.tokens.spacing.s2);
            for (left, right) in [
                (BarItem::Output(OUTPUTS.len() - 1), BarItem::Shutter),
                (BarItem::Shutter, BarItem::Target(0)),
            ] {
                let x = (self.bar_item(left).x1 + self.bar_item(right).x0) / 2.0;
                crate::surface::vertical_separator(
                    scene, transform, x, bar.y0, bar.y1, inset, ground,
                );
            }
            let options = self.bar_item(BarItem::Options);
            let name = studio_text("widget-capture-options");
            let enabled = !self.option_sections().is_empty();
            let face = IconButton::face(options, Icon::MoreHorizontal, &name)
                .control_size(ControlSize::Regular)
                .toggled(self.options_menu.is_some())
                .hovered(enabled && hovered(BarItem::Options))
                .disabled(!enabled);
            Button::paint(scene, transform, tr, &on, &face);
        }
        for (item, rect) in &items {
            match item {
                BarItem::Time => {
                    let text = match self.armed_capture.as_ref() {
                        Some(armed) => {
                            let left = armed.duration.saturating_sub(
                                crate::time::now().saturating_duration_since(armed.started_at),
                            );
                            left.as_secs_f64().ceil().max(1.0).to_string()
                        }
                        None => crate::format::timecode(self.recording_elapsed().as_secs()),
                    };
                    let style = on.text(TextRole::Label);
                    let (w, h) = tr.measure_style(&text, None, &style);
                    tr.draw_style(
                        scene,
                        &text,
                        on.tokens.colors.text_primary,
                        transform.pre_translate(
                            (
                                rect.center().x - f64::from(w) / 2.0,
                                rect.center().y - f64::from(h) / 2.0,
                            )
                                .into(),
                        ),
                        None,
                        &style,
                    );
                }
                BarItem::Save => {
                    let label = studio_text("widget-replay-save");
                    let ready = self.replay_buffered_seconds > 0;
                    let face = ButtonFace::new(*rect, &label)
                        .icon(Icon::Download)
                        .control_size(ControlSize::Regular)
                        .hovered(ready && rect.contains(self.cursor))
                        .disabled(!ready);
                    Button::paint(scene, transform, tr, &on, &face);
                }
                BarItem::Shutter => {
                    CaptureButtonFace::new(self.shutter_kind())
                        .on_chrome(true)
                        .live(self.shutter_ready())
                        .hovered(self.shutter_ready() && rect.contains(self.cursor))
                        .paint(&mut PaintCtx::new(scene, transform, tr, &on), rect.center());
                }
                BarItem::Output(_) | BarItem::Target(_) | BarItem::Options => {}
            }
        }
        if let Some((anchor, tip)) = self.bar_tip() {
            super::tooltip::draw_hover_tooltip(
                scene,
                transform,
                tr,
                theme,
                anchor,
                &tip,
                self.status_rect(),
            );
        }
    }

    /// How far the tool has appeared (`motion::Appear`).
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
        // One flat wash in the theme's scrim hue: it dims the desktop the
        // tool draws over without needing that desktop in its own layer.
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

    /// The design system's focus treatment round what a capture takes: your
    /// colour, just outside it.
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

    /// The outline round what a running recording takes: the recording
    /// colour, thin, just outside it.
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

    /// Paint what is being chosen: the selections with their handles, the
    /// picked window, the strip or the screen.
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

    /// Route one event to an open menu. Returns whether it was consumed.
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

    /// A key while choosing. Returns whether it was the tool's.
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
                if self.screenshot_receipt.take().is_some()
                    || self.recording_finished_at.take().is_some()
                {
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

/// What a chosen target captures.
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

    /// Sized by its style alone (no `measure`): nothing it changes needs
    /// taffy to measure it again, and saying it does re-solved the layout of
    /// everything above it on every frame.
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
            || self.recording_finished_at.is_some_and(|at| {
                crate::time::now().saturating_duration_since(at) < RECORDING_RECEIPT_DURATION
            })
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

/// What one event did to an open menu.
struct MenuOutcome {
    handled: bool,
    /// Escape, or a press outside it.
    dismissed: bool,
    chosen: Option<usize>,
}

/// Drive an open menu with one event, the pointer at `cursor`: Escape or a
/// press outside dismisses it, anything else goes to the menu.
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
        // A press outside the menu dismisses it and nothing else.
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

fn normal_rect(a: Point, b: Point) -> Rect {
    Rect::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
}

fn add_rect(path: &mut BezPath, r: Rect) {
    path.move_to((r.x0, r.y0));
    path.line_to((r.x1, r.y0));
    path.line_to((r.x1, r.y1));
    path.line_to((r.x0, r.y1));
    path.close_path();
}

fn studio_progress(spec: crate::motion::MotionSpec, elapsed: Duration) -> f64 {
    spec.progress_after(
        crate::motion::Direction::Enter,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Pointer,
        elapsed,
    )
}

/// How long one of the tool's motions runs.
fn studio_motion_time(spec: crate::motion::MotionSpec) -> Duration {
    spec.settle_time(
        crate::motion::Direction::Enter,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Pointer,
    )
}

/// A receipt's opacity: there at once, leaving on the standard transient
/// when its time is up.
fn leaving_opacity(shown: Instant, lasts: Duration) -> f32 {
    let now = crate::time::now();
    let fade_from = crate::motion::Transient::exit_starts(
        crate::motion::Transient::READOUT,
        shown + lasts,
        crate::tokens::EffectiveMotion::default(),
    );
    if now < fade_from {
        return 1.0;
    }
    crate::motion::unit_opacity(f64::from(crate::motion::Transient::opacity(
        crate::motion::Transient::READOUT,
        (shown, shown + lasts),
        now,
        crate::tokens::EffectiveMotion::default(),
    )))
}

mod receipt;
use receipt::{ReceiptAction, ReceiptWidths};
mod semantics;

#[cfg(test)]
mod tests;
