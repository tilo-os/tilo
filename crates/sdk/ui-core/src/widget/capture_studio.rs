//! Reusable shell-grade screenshot, recording and arbitrary-region `PiP` selector.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    clippy::semicolon_if_nothing_returned,
    clippy::too_many_lines,
    reason = "immediate-mode capture geometry uses tiny fixed arrays and bounded screen coordinates"
)]
//! Pixel readback stays in the compositor; this widget emits semantic intents.

use crate::{
    ColorCompat,
    icons::{self, Icon},
    paint::PaintCtx,
    presence::PresenceCtx,
    surface,
    text_render::{TextRenderer, TextStyle, TextWeight},
    theme::Theme,
    widget::{Button, ButtonFace, ChipFace, ChipLead, ChipStyle, TaffyTreeExt, Tone, Widget},
};
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;

use crate::time::{Duration, Instant};
use crate::tokens::TextRole;
use crate::widget::ButtonRole;
use taffy::{Dimension, NodeId, Size as TaffySize, Style, TaffyTree};
use vello::{
    Scene,
    kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Size as KurboSize, Stroke},
    peniko::{BlendMode, Color, Fill},
};
use winit::{
    event::{ElementState, KeyEvent, MouseButton, WindowEvent},
    keyboard::{Key, NamedKey},
};

const OPEN_DURATION: Duration = Duration::from_millis(180);
const RECORDING_HOLD: Duration = Duration::from_millis(240);
const RECORDING_SETTLE: Duration = Duration::from_millis(620);
const RECORDING_SCOPE_CONFIRMATION: Duration = Duration::from_millis(820);
const RECORDING_STATE_SETTLE: Duration = Duration::from_millis(220);
const RECORDING_RECEIPT_DURATION: Duration = Duration::from_millis(3_000);
/// Long enough to acquire either follow-through action after saving a replay,
/// without permanently replacing the rolling buffer's primary Save action.
const REPLAY_SAVED_ACTION_DURATION: Duration = Duration::from_millis(3_000);
const RECORDING_RECEIPT_ENTER: Duration = Duration::from_millis(240);
const RECORDING_RECEIPT_EXIT: Duration = Duration::from_millis(320);
const SCREENSHOT_RECEIPT_DURATION: Duration = Duration::from_millis(5_500);
const SCREENSHOT_RECEIPT_ENTER: Duration = Duration::from_millis(520);
const SCREENSHOT_RECEIPT_EXIT: Duration = Duration::from_millis(300);
/// Top-bar selector geometry, shared by the mode and source groups so the
/// two read as one control family, derived from the tokens because the bar
/// has no theme when it lays out: each group is inset `s2` all round and
/// the groups sit `s2` apart; the bar is a `bar_height` button row plus
/// that inset top and bottom (56 px); a symbol button is as wide as the
/// bar is tall and neighbours sit on a hairline gap of half the smallest
/// step (2 px).
const TOP_GROUP_PAD: f64 = crate::tokens::Spacing::BASE.s2 as f64;
const TOP_GROUP_GAP: f64 = crate::tokens::Spacing::BASE.s2 as f64;
const TOP_BAR_H: f64 = crate::tokens::Metrics::BASE.bar_height as f64 + TOP_GROUP_PAD * 2.0;
const TOP_BUTTON_W: f64 = TOP_BAR_H;
/// Neighbouring buttons in a group keep one `s1`: the smallest token step,
/// so two lifted faces never read as one bar (HIG §7 "nothing touches").
const TOP_BUTTON_GAP: f64 = crate::tokens::Spacing::BASE.s1 as f64;
const MIN_REGION: f64 = crate::tokens::Spacing::BASE.s3 as f64;

/// Height of one line of `size`-px text exactly as the renderer paints it.
/// The line box is `line_height × size` (1.25 by default) and carries the
/// renderer's accessibility scale, so centring on this — never on the font
/// size, and never on a hand-tuned fraction of it — keeps a label centred
/// in its pill at every type scale.
fn line_height(tr: &mut TextRenderer, style: &TextStyle) -> f64 {
    f64::from(tr.measure_style("Ag", None, style).1)
}

/// Top edge that centres one line of `size`-px text on `center_y`.
fn centered_text_top(tr: &mut TextRenderer, style: &TextStyle, center_y: f64) -> f64 {
    center_y - line_height(tr, style) / 2.0
}

/// `text`, or as much of it as fits `max_width` with a trailing ellipsis,
/// measured at the size *and weight* it will be painted with. Every pill and
/// chip label goes through this and is then painted without a wrap width:
/// the legacy `max_width` argument wraps, and a wrapped label is a second
/// line below the pill — which is exactly what showed at 2× type. The
/// ellipsis policy is the renderer's, shared with every other widget.
fn fit_label(tr: &mut TextRenderer, text: &str, style: &TextStyle, max_width: f64) -> String {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a logical-pixel budget well inside f32's exact range"
    )]
    tr.elide_to_width_style(text, max_width.max(0.0) as f32, style)
}

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

#[derive(Clone, Debug)]
struct ScreenshotReceipt {
    copied: bool,
    width: u32,
    height: u32,
    preview: CapturePreview,
    shown_at: Instant,
    origin: Point,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureMode {
    #[default]
    Screenshot,
    Recording,
    /// A live, continuously refreshed floating surface. Kept distinct from a
    /// screenshot because its selection, composition and primary action have
    /// a different lifecycle even though both originate from compositor pixels.
    Pip,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceMode {
    #[default]
    Region,
    Window,
    Display,
    Multi,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureDestination {
    File,
    Clipboard,
    FloatingWindow,
    /// Save to the screenshots folder *and* put the pixels on the
    /// clipboard: the instant Shift/Alt+Print captures, which have no
    /// chooser in which to pick one.
    FileAndClipboard,
}

/// How far beyond the pixels currently visible on the physical display the
/// compositor should follow the selected source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureExtent {
    #[default]
    Visible,
    /// The app's full virtual surface, including off-screen/scrolling content.
    FullApp,
    /// Every app in the current horizontal desktop strip, including portions
    /// outside the monitor viewport.
    WholeStrip,
}

/// The live apps a `FullApp` or `WholeStrip` capture repaints outside the
/// output clip.
#[derive(Clone, Copy, Debug)]
pub struct LiveCapture<'a> {
    /// `group:<group>:tab:<tab>` ids of the selected apps.
    pub target_ids: &'a [String],
    pub extent: CaptureExtent,
    /// Paint a `FullApp` target's complete semantic content (page, document,
    /// canvas) where its app provides one, rather than its virtual surface.
    pub complete_semantic_content: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PipLayout {
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
        match (enabled, self.includes_microphone()) {
            (false, false) => Self::None,
            (false, true) => Self::Microphone,
            (true, false) => Self::System,
            (true, true) => Self::SystemAndMicrophone,
        }
    }

    fn with_microphone(self, enabled: bool) -> Self {
        match (self.includes_system(), enabled) {
            (false, false) => Self::None,
            (false, true) => Self::Microphone,
            (true, false) => Self::System,
            (true, true) => Self::SystemAndMicrophone,
        }
    }
}

/// A live shell/app surface that can be selected without first moving it
/// fully onto the physical display. `virtual_rect` is its place in Tilo's
/// continuous desktop; `visible_rect` is the portion currently paintable on
/// this output.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureTarget {
    pub id: String,
    pub title: String,
    pub app_id: String,
    pub visible_rect: Rect,
    pub virtual_rect: Rect,
    /// Complete semantic page/document/canvas bounds supplied by the app.
    /// Absent for ordinary and foreign windows, where `virtual_rect` remains
    /// the complete surface. This is metadata only; execution asks the
    /// live provider to paint again so stale picker geometry never wins.
    pub complete_rect: Option<Rect>,
}

impl CaptureTarget {
    pub fn full_rect(&self) -> Rect {
        self.complete_rect.unwrap_or(self.virtual_rect)
    }

    /// Workflow-appropriate meaning of “App”. A still image may expand an
    /// app-provided document/canvas beyond its live window; video and `PiP` must
    /// preserve the live window itself or a tall document becomes an
    /// unreadable strip that changes aspect while recording.
    pub fn full_app_rect(&self, mode: CaptureMode) -> Rect {
        if mode == CaptureMode::Screenshot {
            self.full_rect()
        } else {
            self.virtual_rect
        }
    }
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
    pub regions: Vec<Rect>,
    pub include_pointer: bool,
    pub audio: AudioCaptureMode,
    pub extent: CaptureExtent,
    pub pip_layout: PipLayout,
    pub targets: Vec<CaptureTarget>,
    /// `Some` only for recorder lifecycle work. Source fields remain
    /// populated on Start and may be empty for Pause/Resume/Stop.
    pub recording_command: Option<RecordingCommand>,
    /// Retrospective capture is deliberately a separate lifecycle: arming a
    /// bounded private buffer is not presented as starting a recording.
    pub replay_command: Option<ReplayCommand>,
    /// Post-capture actions are emitted separately from a new screenshot so
    /// clicking its receipt never accidentally reuses stale selection state.
    pub screenshot_command: Option<ScreenshotCommand>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionHandle {
    NorthWest,
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
}

#[derive(Clone, Copy, Debug)]
enum DragSelection {
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
        handle: SelectionHandle,
        current: Point,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RegionChrome {
    metadata: Rect,
    remove: Rect,
    outside: bool,
}

impl DragSelection {
    fn set_current(&mut self, point: Point) {
        match self {
            Self::Create { current, .. }
            | Self::Move { current, .. }
            | Self::Resize { current, .. } => *current = point,
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent capture, recording, replay, and receipt states are retained together"
)]
pub struct CaptureStudio {
    rect: Rect,
    overlay_rect: Rect,
    open: bool,
    opened_at: Option<Instant>,
    mode: CaptureMode,
    source: SourceMode,
    regions: Vec<Rect>,
    region_remove_rects: Vec<Rect>,
    drag: Option<DragSelection>,
    cursor: Point,
    include_pointer: bool,
    audio: AudioCaptureMode,
    system_audio_available: bool,
    microphone_available: bool,
    extent: CaptureExtent,
    pip_layout: PipLayout,
    candidates: Vec<CaptureTarget>,
    selected_target: Option<String>,
    pending: Vec<CaptureIntent>,
    /// A recording was requested, but encoder/output setup has not yet been
    /// confirmed by the compositor. Acknowledging this state immediately
    /// prevents a slow start from reading as a missed click without falsely
    /// claiming that pixels are already being recorded.
    recording_start_pending: bool,
    recording_request_started_at: Option<Instant>,
    recording_active: bool,
    recording_started_at: Option<Instant>,
    recording_scope: String,
    recording_confirmation_regions: Vec<Rect>,
    /// Which sides of the physical output the selected virtual source keeps
    /// going past. This is snapshotted at recorder start so the brief spatial
    /// confirmation cannot drift if the strip moves underneath it.
    /// Order: left, top, right, bottom.
    recording_continuation_edges: [bool; 4],
    stop_pending: bool,
    recording_paused: bool,
    pause_pending: bool,
    recording_paused_at: Option<Instant>,
    recording_paused_total: Duration,
    recording_state_changed_at: Option<Instant>,
    recording_transition_from_paused: Option<bool>,
    /// Exact visual-harness phase for pause/resume. Production follows
    /// `recording_state_changed_at`; PNG renders must not advance it merely
    /// because GPU readback is slow.
    recording_state_progress_override: Option<f64>,
    recording_finished_at: Option<Instant>,
    /// Active take duration snapshotted before the live timer is cleared, so
    /// the finish receipt can confirm both successful persistence and length.
    recording_finished_duration: Duration,
    /// Exact visual-harness phase for the saved receipt's entrance.
    recording_receipt_progress_override: Option<f64>,
    replay_active: bool,
    replay_started_at: Option<Instant>,
    replay_buffered_seconds: u8,
    replay_audio: AudioCaptureMode,
    replay_saved_at: Option<Instant>,
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
    delay_seconds: u8,
    armed_capture: Option<ArmedCapture>,
    /// Content-driven floor for the docked recorder's width above 1× type;
    /// see [`Self::remember_docked_content_widths`]. Render caches only.
    docked_pill_min_width: f64,
    docked_stop_width: f64,
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
            region_remove_rects: Vec::new(),
            drag: None,
            cursor: Point::ZERO,
            include_pointer: false,
            audio: AudioCaptureMode::None,
            system_audio_available: false,
            microphone_available: false,
            extent: CaptureExtent::Visible,
            pip_layout: PipLayout::Auto,
            candidates: Vec::new(),
            selected_target: None,
            pending: Vec::new(),
            recording_start_pending: false,
            recording_request_started_at: None,
            recording_active: false,
            recording_started_at: None,
            recording_scope: String::new(),
            recording_confirmation_regions: Vec::new(),
            recording_continuation_edges: [false; 4],
            stop_pending: false,
            recording_paused: false,
            pause_pending: false,
            recording_paused_at: None,
            recording_paused_total: Duration::ZERO,
            recording_state_changed_at: None,
            recording_transition_from_paused: None,
            recording_state_progress_override: None,
            recording_finished_at: None,
            recording_finished_duration: Duration::ZERO,
            recording_receipt_progress_override: None,
            replay_active: false,
            replay_started_at: None,
            replay_buffered_seconds: 0,
            replay_audio: AudioCaptureMode::None,
            replay_saved_at: None,
            screenshot_receipt: None,
            screenshot_pin_pending: false,
            send_devices: Vec::new(),
            send_menu: None,
            send_menu_targets: Vec::new(),
            screenshot_origin: None,
            delay_seconds: 0,
            armed_capture: None,
            docked_pill_min_width: 0.0,
            docked_stop_width: 0.0,
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
        if self.recording_start_pending {
            // Repeating the shortcut while encoder setup is in flight must
            // not restore the modal selector over the desktop.
            return;
        }
        self.armed_capture = None;
        self.recording_finished_at = None;
        self.screenshot_receipt = None;
        self.screenshot_pin_pending = false;
        self.open = true;
        self.opened_at = Some(crate::time::now());
        self.regions.clear();
        self.region_remove_rects.clear();
        self.drag = None;
        if self.recording_active {
            self.mode = CaptureMode::Recording;
        }
    }

    /// Global replay shortcut entry point. The first use lands directly in
    /// recording setup; once the private rolling buffer is armed, the same
    /// chord saves it without reopening modal chrome.
    pub fn open_recording_setup(&mut self) {
        self.open();
        if !self.recording_active {
            self.set_mode(CaptureMode::Recording);
        }
    }

    /// Global `PiP` shortcut entry point. `PiP` is a frequent live-workflow, not
    /// a screenshot destination hidden one mode-switch away, so its shell
    /// chord lands directly in the region picker with `PiP`-safe defaults.
    pub fn open_pip_setup(&mut self) {
        self.open();
        if !self.recording_active {
            self.set_mode(CaptureMode::Pip);
        }
    }

    /// Save the currently buffered replay through the same semantic intent as
    /// its top-bar button. Returns whether a ready buffer accepted the action.
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
            "Save to this workspace's files",
            || {},
        ));
        targets.push(Some(ScreenshotSendTarget::Workspace));
        menu.push(MenuItem::separator());
        targets.push(None);
        if self.send_devices.is_empty() {
            menu.push(MenuItem::action("No other device is online", || {}).with_enabled(false));
            targets.push(None);
        }
        for device in &self.send_devices {
            menu.push(MenuItem::icon_action(
                Icon::Send,
                format!("Send to {}", device.label),
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

    /// Route one event to the open target picker. Returns whether it was
    /// consumed; a pick queues the send and retires the receipt.
    fn route_send_menu_event(
        &mut self,
        e: &WindowEvent,
        scale: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        let Some(menu) = self.send_menu.as_mut() else {
            return false;
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
            self.close_send_menu();
            return true;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = e
            && !menu.contains([self.cursor.x, self.cursor.y])
        {
            // A press outside the picker dismisses it and nothing else.
            self.close_send_menu();
            return true;
        }
        let mut taffy = TaffyTree::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return true;
        };
        let mut handled = false;
        let mut focus = None;
        menu.handle_event(
            e,
            &mut crate::widget::EventCtx {
                scale_factor: scale,
                modifiers: &modifiers,
                state,
                path,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        if let Some(index) = menu.take_selection() {
            self.choose_send_target(index);
        }
        handled
    }

    fn choose_send_target(&mut self, index: usize) {
        let Some(Some(target)) = self.send_menu_targets.get(index).cloned() else {
            return;
        };
        self.send_last_screenshot(target);
        self.screenshot_receipt = None;
        self.open = false;
    }

    pub fn set_screenshot_finished(
        &mut self,
        copied: bool,
        width: u32,
        height: u32,
        preview: CapturePreview,
    ) {
        let origin = self
            .screenshot_origin
            .take()
            .or_else(|| {
                self.active_regions()
                    .into_iter()
                    .reduce(|bounds, region| bounds.union(region))
                    .map(|rect| rect.center())
            })
            .unwrap_or_else(|| self.rect.center());
        self.screenshot_receipt = Some(ScreenshotReceipt {
            copied,
            width,
            height,
            preview,
            shown_at: crate::time::now(),
            origin,
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
    /// review. Live presentation continues to use elapsed time; software PNG
    /// rendering can otherwise outlive the complete transition between two
    /// sampled frames.
    pub fn set_screenshot_transition_progress(&mut self, permille: u16) {
        let Some(receipt) = self.screenshot_receipt.as_mut() else {
            return;
        };
        let progress = f64::from(permille.min(1_000)) / 1_000.0;
        receipt.shown_at = crate::time::now()
            .checked_sub(SCREENSHOT_RECEIPT_ENTER.mul_f64(progress))
            .unwrap_or_else(crate::time::now);
    }
    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn mode(&self) -> CaptureMode {
        self.mode
    }
    pub fn is_recording_active(&self) -> bool {
        self.recording_active
    }
    pub fn is_recording_start_pending(&self) -> bool {
        self.recording_start_pending
    }
    pub fn is_recording_paused(&self) -> bool {
        self.recording_paused
    }
    pub fn is_replay_active(&self) -> bool {
        self.replay_active
    }
    pub fn source_mode(&self) -> SourceMode {
        self.source
    }
    pub fn regions(&self) -> &[Rect] {
        &self.regions
    }
    pub fn set_source_mode(&mut self, source: SourceMode) {
        self.source = source;
        self.regions.clear();
        self.region_remove_rects.clear();
        self.selected_target = if source == SourceMode::Window {
            self.candidates.first().map(|target| target.id.clone())
        } else {
            None
        };
    }
    pub fn set_mode(&mut self, mode: CaptureMode) {
        self.mode = mode;
        if mode == CaptureMode::Pip {
            // Pointer trails are useful capture metadata, but surprising in a
            // persistent live surface. PiP starts clean unless we later expose
            // an explicit live-pointer affordance for that workflow.
            self.include_pointer = false;
            self.audio = AudioCaptureMode::None;
        }
    }
    pub fn set_recording_active(&mut self, active: bool) {
        if active && !self.recording_active {
            self.recording_start_pending = false;
            self.recording_request_started_at = None;
            self.recording_finished_at = None;
            self.recording_finished_duration = Duration::ZERO;
            self.recording_started_at = Some(crate::time::now());
            self.recording_scope = self.recording_scope_label();
            self.recording_confirmation_regions = self.current_recording_regions();
            self.recording_continuation_edges = self.current_recording_continuation_edges();
            self.stop_pending = false;
            self.recording_paused = false;
            self.pause_pending = false;
            self.recording_paused_at = None;
            self.recording_paused_total = Duration::ZERO;
            self.recording_state_changed_at = None;
            self.recording_transition_from_paused = None;
            self.recording_state_progress_override = None;
            self.recording_receipt_progress_override = None;
            // The picker has already closed before the compositor starts the
            // encoder. Re-open only as a non-modal status surface.
            self.open = true;
        } else if !active {
            self.recording_start_pending = false;
            self.recording_request_started_at = None;
            self.recording_started_at = None;
            self.recording_confirmation_regions.clear();
            self.recording_continuation_edges = [false; 4];
            self.stop_pending = false;
            self.recording_paused = false;
            self.pause_pending = false;
            self.recording_paused_at = None;
            self.recording_paused_total = Duration::ZERO;
            self.recording_state_changed_at = None;
            self.recording_transition_from_paused = None;
            self.recording_state_progress_override = None;
            self.recording_finished_at = None;
            self.recording_finished_duration = Duration::ZERO;
            self.recording_receipt_progress_override = None;
            self.open = false;
        }
        self.recording_active = active;
        if active {
            self.mode = CaptureMode::Recording;
        }
    }

    /// Deterministic visual-harness control for reviewing the complete
    /// launch-to-top-bar motion even when software rendering takes longer
    /// than the animation itself. Production code advances from `Instant`.
    #[doc(hidden)]
    pub fn set_recording_transition_progress(&mut self, permille: u16) {
        if !self.recording_active {
            return;
        }
        let progress = f64::from(permille.min(1_000)) / 1_000.0;
        let offset = RECORDING_HOLD + RECORDING_SETTLE.mul_f64(progress);
        self.recording_started_at = crate::time::now().checked_sub(offset);
    }

    /// Keep the compositor's spatial recording affordance alive just long
    /// enough to confirm a successful flush and offer the useful next action.
    /// Failures deliberately use `set_recording_active(false)` instead, so a
    /// green receipt can never be shown for an incomplete file.
    pub fn set_recording_finished(&mut self) {
        if !self.recording_active {
            return;
        }
        self.recording_finished_duration = self.recording_elapsed();
        self.recording_active = false;
        self.recording_start_pending = false;
        self.recording_request_started_at = None;
        self.recording_started_at = None;
        self.recording_confirmation_regions.clear();
        self.recording_continuation_edges = [false; 4];
        self.stop_pending = false;
        self.recording_paused = false;
        self.pause_pending = false;
        self.recording_paused_at = None;
        self.recording_paused_total = Duration::ZERO;
        self.recording_state_changed_at = None;
        self.recording_transition_from_paused = None;
        self.recording_state_progress_override = None;
        self.recording_finished_at = Some(crate::time::now());
        self.recording_receipt_progress_override = None;
        self.open = true;
    }

    pub fn set_replay_active(&mut self, active: bool, audio: AudioCaptureMode) {
        self.replay_active = active;
        self.replay_audio = if active {
            audio
        } else {
            AudioCaptureMode::None
        };
        self.replay_buffered_seconds = 0;
        self.replay_saved_at = None;
        self.replay_started_at = active.then(crate::time::now);
        if active {
            self.open = true;
        } else if !self.recording_active && self.recording_finished_at.is_none() {
            self.open = false;
        }
    }

    pub fn set_replay_buffered_seconds(&mut self, seconds: u8) {
        if self.replay_active {
            self.replay_buffered_seconds = seconds.min(30);
        }
    }

    pub fn set_replay_saved(&mut self) {
        if self.replay_active {
            self.replay_saved_at = Some(crate::time::now());
        }
    }
    pub fn set_recording_paused(&mut self, paused: bool) {
        if !self.recording_active {
            return;
        }
        if paused != self.recording_paused {
            let now = crate::time::now();
            self.recording_transition_from_paused = Some(self.recording_paused);
            if paused {
                self.recording_paused_at = Some(now);
            } else if let Some(paused_at) = self.recording_paused_at.take() {
                self.recording_paused_total += now.saturating_duration_since(paused_at);
            }
            self.recording_paused = paused;
            self.recording_state_changed_at = Some(now);
            self.recording_state_progress_override = None;
        }
        self.pause_pending = false;
    }

    /// Freeze the pause/resume crossfade at an exact phase for deterministic
    /// visual review. Live transitions remain elapsed-time driven.
    #[doc(hidden)]
    pub fn set_recording_state_transition_progress(&mut self, permille: u16) {
        if self.recording_active && self.recording_state_changed_at.is_some() {
            self.recording_state_progress_override = Some(f64::from(permille.min(1_000)) / 1_000.0);
        }
    }

    /// Freeze the compositor-confirmed saved receipt's entrance at an exact
    /// phase. This prevents slow PNG readback from consuming its entire
    /// three-second live lifetime between deterministic snapshots.
    #[doc(hidden)]
    pub fn set_recording_receipt_transition_progress(&mut self, permille: u16) {
        if self.recording_finished_at.is_some() {
            self.recording_receipt_progress_override =
                Some(f64::from(permille.min(1_000)) / 1_000.0);
        }
    }
    pub fn set_audio_available(&mut self, system: bool, microphone: bool) {
        self.system_audio_available = system;
        self.microphone_available = microphone;
        self.audio = self
            .audio
            .with_system(system && self.audio.includes_system())
            .with_microphone(microphone && self.audio.includes_microphone());
    }
    pub fn set_extent(&mut self, extent: CaptureExtent) {
        self.extent = extent;
    }
    pub fn set_pip_layout(&mut self, layout: PipLayout) {
        self.pip_layout = layout;
    }
    pub fn set_candidates(&mut self, candidates: Vec<CaptureTarget>) {
        self.candidates = candidates;
        let selection_exists = self
            .selected_target
            .as_ref()
            .is_some_and(|id| self.candidates.iter().any(|candidate| &candidate.id == id));
        if self.source == SourceMode::Window && !selection_exists {
            self.selected_target = self.candidates.first().map(|target| target.id.clone());
        }
    }
    pub fn take_intents(&mut self) -> Vec<CaptureIntent> {
        if self
            .armed_capture
            .as_ref()
            .is_some_and(|armed| armed.started_at.elapsed() >= armed.duration)
            && let Some(armed) = self.armed_capture.take()
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
        self.recording_scope = self.recording_scope_label();
        self.recording_confirmation_regions = self.current_recording_regions();
        self.recording_continuation_edges = self.current_recording_continuation_edges();
        self.recording_start_pending = true;
        self.recording_request_started_at = Some(crate::time::now());
        self.recording_finished_at = None;
        self.open = true;
        self.mode = CaptureMode::Recording;
    }

    /// Width of a top-bar group holding `buttons` buttons: one inset all
    /// round, one gap between neighbours — so the group is exactly as wide
    /// as what it holds. The old fixed spans (180 / 300 in a 488 bar) left
    /// the mode group with no trailing inset and the source group with
    /// ~50 px of dead space after its last button.
    fn top_group_width(buttons: usize) -> f64 {
        let n = buttons as f64;
        TOP_GROUP_PAD * 2.0 + n * TOP_BUTTON_W + (n - 1.0).max(0.0) * TOP_BUTTON_GAP
    }
    fn top_bar(&self) -> Rect {
        let w = (Self::top_group_width(3) + TOP_GROUP_GAP + Self::top_group_width(4))
            .min(self.rect.width() - 32.0);
        Rect::new(
            self.rect.center().x - w / 2.0,
            self.rect.y0 + 22.0,
            self.rect.center().x + w / 2.0,
            self.rect.y0 + 22.0 + TOP_BAR_H,
        )
    }
    fn mode_group_rect(&self) -> Rect {
        let b = self.top_bar();
        Rect::new(b.x0, b.y0, b.x0 + Self::top_group_width(3), b.y1)
    }
    fn source_group_rect(&self) -> Rect {
        let b = self.top_bar();
        let x0 = b.x0 + Self::top_group_width(3) + TOP_GROUP_GAP;
        Rect::new(x0, b.y0, x0 + Self::top_group_width(4), b.y1)
    }
    /// The `i`th button of a group whose left edge is `group_x0`.
    fn top_button_rect(&self, group_x0: f64, i: usize) -> Rect {
        let b = self.top_bar();
        let x = group_x0 + TOP_GROUP_PAD + i as f64 * (TOP_BUTTON_W + TOP_BUTTON_GAP);
        Rect::new(
            x,
            b.y0 + TOP_GROUP_PAD,
            x + TOP_BUTTON_W,
            b.y1 - TOP_GROUP_PAD,
        )
    }
    fn bottom_bar(&self) -> Rect {
        let has_options = matches!(self.source, SourceMode::Window | SourceMode::Multi);
        let extra_audio_controls = 42.0 * self.audio_control_count() as f64;
        let desired: f64 = match (self.mode, has_options) {
            (CaptureMode::Screenshot, false) => 300.0,
            (CaptureMode::Screenshot, true) => 566.0,
            (CaptureMode::Recording, false) => 318.0 + extra_audio_controls,
            (CaptureMode::Recording, true) => 584.0 + extra_audio_controls,
            (CaptureMode::Pip, false) => 162.0,
            (CaptureMode::Pip, true) => 432.0,
        };
        let w = desired.min(self.rect.width() - 32.0);
        Rect::new(
            self.rect.center().x - w / 2.0,
            self.rect.y1 - 82.0,
            self.rect.center().x + w / 2.0,
            self.rect.y1 - 22.0,
        )
    }
    fn option_rects(&self) -> [Rect; 3] {
        let b = self.bottom_bar();
        let x = b.x0
            + if self.mode == CaptureMode::Pip {
                12.0
            } else if self.mode == CaptureMode::Recording {
                104.0 + 42.0 * self.audio_control_count() as f64
            } else {
                104.0
            };
        let each = 88.0;
        std::array::from_fn(|i| {
            Rect::new(
                x + i as f64 * each,
                b.y0 + 8.0,
                x + (i + 1) as f64 * each - 6.0,
                b.y1 - 8.0,
            )
        })
    }
    fn mode_rects(&self) -> [Rect; 3] {
        let group = self.mode_group_rect();
        std::array::from_fn(|i| self.top_button_rect(group.x0, i))
    }
    fn source_rects(&self) -> [Rect; 4] {
        let group = self.source_group_rect();
        std::array::from_fn(|i| self.top_button_rect(group.x0, i))
    }
    fn candidate_picker_rects(&self) -> Vec<Rect> {
        let count = self.candidates.len().min(5);
        if count == 0 {
            return Vec::new();
        }
        let each = 132.0;
        let gap = 6.0;
        let total = count as f64 * each + count.saturating_sub(1) as f64 * gap;
        let x0 = self.rect.center().x - total / 2.0;
        (0..count)
            .map(|i| {
                let x = x0 + i as f64 * (each + gap);
                Rect::new(
                    x,
                    self.top_bar().y1 + 8.0,
                    x + each,
                    self.top_bar().y1 + 40.0,
                )
            })
            .collect()
    }
    fn action_rects(&self) -> [Rect; 3] {
        let b = self.bottom_bar();
        if self.recording_active {
            let (_, _, stop) = self.recording_indicator_rects();
            return [Rect::ZERO, Rect::ZERO, stop];
        }
        match self.mode {
            CaptureMode::Screenshot => [
                Rect::new(b.x1 - 190.0, b.y0 + 8.0, b.x1 - 100.0, b.y1 - 8.0),
                Rect::ZERO,
                Rect::new(b.x1 - 94.0, b.y0 + 8.0, b.x1 - 8.0, b.y1 - 8.0),
            ],
            CaptureMode::Recording => [
                Rect::ZERO,
                Rect::new(b.x1 - 208.0, b.y0 + 8.0, b.x1 - 100.0, b.y1 - 8.0),
                Rect::new(b.x1 - 94.0, b.y0 + 8.0, b.x1 - 8.0, b.y1 - 8.0),
            ],
            CaptureMode::Pip => [
                Rect::ZERO,
                Rect::new(b.x1 - 146.0, b.y0 + 8.0, b.x1 - 8.0, b.y1 - 8.0),
                Rect::ZERO,
            ],
        }
    }
    fn audio_control_count(&self) -> usize {
        if self.mode != CaptureMode::Recording {
            return 0;
        }
        usize::from(self.system_audio_available) + usize::from(self.microphone_available)
    }

    fn control_slot_rect(&self, slot: usize) -> Rect {
        let b = self.bottom_bar();
        let x = b.x0 + 12.0 + slot as f64 * 42.0;
        Rect::new(x, b.y0 + 12.0, x + 36.0, b.y1 - 12.0)
    }

    fn pointer_rect(&self) -> Rect {
        self.control_slot_rect(0)
    }

    fn system_audio_rect(&self) -> Rect {
        if self.mode != CaptureMode::Recording || !self.system_audio_available {
            return Rect::ZERO;
        }
        self.control_slot_rect(1)
    }

    fn microphone_rect(&self) -> Rect {
        if self.mode != CaptureMode::Recording || !self.microphone_available {
            return Rect::ZERO;
        }
        self.control_slot_rect(1 + usize::from(self.system_audio_available))
    }

    fn delay_rect(&self) -> Rect {
        if self.mode == CaptureMode::Pip {
            return Rect::ZERO;
        }
        self.control_slot_rect(1 + self.audio_control_count())
    }

    fn cycle_delay(&mut self) {
        self.delay_seconds = match self.delay_seconds {
            0 => 3,
            3 => 10,
            _ => 0,
        };
    }

    fn navigate_picker(&mut self, key: NamedKey) -> bool {
        let forward = matches!(key, NamedKey::ArrowRight | NamedKey::ArrowDown);
        match (self.source, key) {
            (SourceMode::Window, NamedKey::ArrowLeft | NamedKey::ArrowRight) => {
                if self.candidates.is_empty() {
                    return false;
                }
                let current = self
                    .selected_target
                    .as_ref()
                    .and_then(|id| {
                        self.candidates
                            .iter()
                            .position(|candidate| &candidate.id == id)
                    })
                    .unwrap_or(0);
                let next = wrapped_picker_index(current, self.candidates.len(), forward);
                self.selected_target = Some(self.candidates[next].id.clone());
                true
            }
            (SourceMode::Window, NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                let options = [
                    CaptureExtent::Visible,
                    CaptureExtent::FullApp,
                    CaptureExtent::WholeStrip,
                ];
                let current = options
                    .iter()
                    .position(|extent| *extent == self.extent)
                    .unwrap_or(0);
                self.extent = options[wrapped_picker_index(current, options.len(), forward)];
                true
            }
            (
                SourceMode::Multi,
                NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown,
            ) => {
                let options = [PipLayout::Auto, PipLayout::Grid, PipLayout::Stack];
                let current = options
                    .iter()
                    .position(|layout| *layout == self.pip_layout)
                    .unwrap_or(0);
                self.pip_layout = options[wrapped_picker_index(current, options.len(), forward)];
                true
            }
            _ => false,
        }
    }
    fn selection_bounds(&self) -> Rect {
        // Picker chrome floats over the clean compositor scene and disappears
        // before capture execution. It must not make the physical output's
        // top and bottom bands impossible to select.
        self.rect
    }

    fn selection_handle_points(region: Rect) -> [(SelectionHandle, Point); 8] {
        [
            (SelectionHandle::NorthWest, Point::new(region.x0, region.y0)),
            (
                SelectionHandle::North,
                Point::new(region.center().x, region.y0),
            ),
            (SelectionHandle::NorthEast, Point::new(region.x1, region.y0)),
            (
                SelectionHandle::East,
                Point::new(region.x1, region.center().y),
            ),
            (SelectionHandle::SouthEast, Point::new(region.x1, region.y1)),
            (
                SelectionHandle::South,
                Point::new(region.center().x, region.y1),
            ),
            (SelectionHandle::SouthWest, Point::new(region.x0, region.y1)),
            (
                SelectionHandle::West,
                Point::new(region.x0, region.center().y),
            ),
        ]
    }

    fn selection_handle_at(&self, point: Point) -> Option<(usize, SelectionHandle)> {
        self.regions
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, region)| {
                Self::selection_handle_points(*region)
                    .into_iter()
                    .find_map(|(handle, center)| {
                        Rect::from_center_size(center, (20.0, 20.0))
                            .contains(point)
                            .then_some((index, handle))
                    })
            })
    }

    fn moved_selection_rect(original: Rect, start: Point, current: Point, bounds: Rect) -> Rect {
        let delta = current - start;
        let dx = delta
            .x
            .clamp(bounds.x0 - original.x0, bounds.x1 - original.x1);
        let dy = delta
            .y
            .clamp(bounds.y0 - original.y0, bounds.y1 - original.y1);
        original + vello::kurbo::Vec2::new(dx, dy)
    }

    fn resized_selection_rect(
        original: Rect,
        handle: SelectionHandle,
        current: Point,
        bounds: Rect,
    ) -> Rect {
        let mut rect = original;
        if matches!(
            handle,
            SelectionHandle::NorthWest | SelectionHandle::West | SelectionHandle::SouthWest
        ) {
            rect.x0 = current.x.clamp(bounds.x0, original.x1 - MIN_REGION);
        }
        if matches!(
            handle,
            SelectionHandle::NorthEast | SelectionHandle::East | SelectionHandle::SouthEast
        ) {
            rect.x1 = current.x.clamp(original.x0 + MIN_REGION, bounds.x1);
        }
        if matches!(
            handle,
            SelectionHandle::NorthWest | SelectionHandle::North | SelectionHandle::NorthEast
        ) {
            rect.y0 = current.y.clamp(bounds.y0, original.y1 - MIN_REGION);
        }
        if matches!(
            handle,
            SelectionHandle::SouthWest | SelectionHandle::South | SelectionHandle::SouthEast
        ) {
            rect.y1 = current.y.clamp(original.y0 + MIN_REGION, bounds.y1);
        }
        rect
    }

    fn drag_selection_rect(&self, drag: DragSelection) -> (Option<usize>, Rect) {
        let bounds = self.selection_bounds();
        match drag {
            DragSelection::Create { start, current } => {
                (None, normal_rect(start, current).intersect(bounds))
            }
            DragSelection::Move {
                index,
                original,
                start,
                current,
            } => (
                Some(index),
                Self::moved_selection_rect(original, start, current, bounds),
            ),
            DragSelection::Resize {
                index,
                original,
                handle,
                current,
            } => (
                Some(index),
                Self::resized_selection_rect(original, handle, current, bounds),
            ),
        }
    }

    fn begin_region_edit(&mut self, point: Point) -> bool {
        if let Some((index, handle)) = self.selection_handle_at(point) {
            self.drag = Some(DragSelection::Resize {
                index,
                original: self.regions[index],
                handle,
                current: point,
            });
            return true;
        }
        if let Some((index, original)) = self
            .regions
            .iter()
            .copied()
            .enumerate()
            .rev()
            .find(|(_, region)| region.contains(point))
        {
            self.drag = Some(DragSelection::Move {
                index,
                original,
                start: point,
                current: point,
            });
            return true;
        }
        false
    }

    fn finish_region_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let (index, rect) = self.drag_selection_rect(drag);
        if rect.width() < MIN_REGION || rect.height() < MIN_REGION {
            return;
        }
        if let Some(index) = index {
            if let Some(region) = self.regions.get_mut(index) {
                *region = rect;
            }
        } else {
            self.regions.push(rect);
        }
    }

    fn nudge_region(&mut self, key: NamedKey, amount: f64) -> bool {
        if self.source != SourceMode::Region {
            return false;
        }
        let bounds = self.selection_bounds();
        let Some(region) = self.regions.last_mut() else {
            return false;
        };
        let delta = match key {
            NamedKey::ArrowLeft => vello::kurbo::Vec2::new(-amount, 0.0),
            NamedKey::ArrowRight => vello::kurbo::Vec2::new(amount, 0.0),
            NamedKey::ArrowUp => vello::kurbo::Vec2::new(0.0, -amount),
            NamedKey::ArrowDown => vello::kurbo::Vec2::new(0.0, amount),
            _ => return false,
        };
        *region = Self::moved_selection_rect(*region, Point::ZERO, Point::ZERO + delta, bounds);
        true
    }
    fn region_chrome_layout(
        region: Rect,
        metadata_width: f64,
        bounds: Rect,
        occupied: &[Rect],
    ) -> RegionChrome {
        const HEIGHT: f64 = crate::tokens::Spacing::BASE.s6 as f64;
        const GAP: f64 = crate::tokens::Metrics::BASE.icon_label_gap as f64;
        const REMOVE_WIDTH: f64 = crate::tokens::Spacing::BASE.s6 as f64;
        let total_width = metadata_width + GAP + REMOVE_WIDTH;
        let clamp_x = |x: f64| x.clamp(bounds.x0, (bounds.x1 - total_width).max(bounds.x0));
        let clamp_y = |y: f64| y.clamp(bounds.y0, (bounds.y1 - HEIGHT).max(bounds.y0));
        let candidates = [
            Rect::new(
                clamp_x(region.x0),
                region.y0 - GAP - HEIGHT,
                clamp_x(region.x0) + total_width,
                region.y0 - GAP,
            ),
            Rect::new(
                clamp_x(region.x0),
                region.y1 + GAP,
                clamp_x(region.x0) + total_width,
                region.y1 + GAP + HEIGHT,
            ),
            Rect::new(
                region.x1 + GAP,
                clamp_y(region.y0),
                region.x1 + GAP + total_width,
                clamp_y(region.y0) + HEIGHT,
            ),
            Rect::new(
                region.x0 - GAP - total_width,
                clamp_y(region.y0),
                region.x0 - GAP,
                clamp_y(region.y0) + HEIGHT,
            ),
        ];
        let group = candidates.into_iter().find(|candidate| {
            bounds.contains(candidate.origin())
                && bounds.contains(Point::new(candidate.x1, candidate.y1))
                && occupied
                    .iter()
                    .all(|other| candidate.intersect(*other).area() <= 0.0)
        });
        let (group, outside) = group.map_or_else(
            || {
                let x0 = clamp_x(region.x0 + 8.0);
                let y0 = clamp_y(region.y0 + 8.0);
                (Rect::new(x0, y0, x0 + total_width, y0 + HEIGHT), false)
            },
            |group| (group, true),
        );
        RegionChrome {
            metadata: Rect::new(group.x0, group.y0, group.x0 + metadata_width, group.y1),
            remove: Rect::new(group.x1 - REMOVE_WIDTH, group.y0, group.x1, group.y1),
            outside,
        }
    }

    fn region_chrome(
        &self,
        region: Rect,
        index: usize,
        regions: &[Rect],
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> (Option<String>, Option<String>, String, RegionChrome) {
        let multiple = regions.len() > 1;
        let dimensions = format!(
            "{} × {}",
            region.width().round() as i64,
            region.height().round() as i64
        );
        let live = (self.mode == CaptureMode::Pip).then(|| "LIVE".to_string());
        let ordinal = multiple.then(|| (index + 1).to_string());
        let labels = [
            live.as_deref(),
            ordinal.as_deref(),
            Some(dimensions.as_str()),
        ];
        let visible = labels.into_iter().flatten().collect::<Vec<_>>();
        let width = visible
            .iter()
            .map(|label| {
                ChipFace::intrinsic_width(label, ChipLead::None, false, text_renderer, theme)
            })
            .sum::<f64>()
            + visible.len().saturating_sub(1) as f64 * 4.0;
        let chrome = Self::region_chrome_layout(region, width, self.selection_bounds(), regions);
        (live, ordinal, dimensions, chrome)
    }
    fn selected_candidate(&self) -> Option<&CaptureTarget> {
        let id = self.selected_target.as_ref()?;
        self.candidates.iter().find(|target| &target.id == id)
    }
    fn target_has_useful_aperture(target: &CaptureTarget, viewport: Rect) -> bool {
        let visible = target.visible_rect.intersect(viewport);
        visible.width() >= 180.0 && visible.height() >= 120.0
    }
    fn offscreen_guidance(mode: CaptureMode, extent: CaptureExtent) -> &'static str {
        if extent == CaptureExtent::Visible {
            "Choose App below to include its full canvas"
        } else {
            match mode {
                CaptureMode::Screenshot => "Capture it without moving it onto this screen",
                CaptureMode::Recording => "Record it without moving it onto this screen",
                CaptureMode::Pip => "Float it live without moving it onto this screen",
            }
        }
    }
    fn active_regions(&self) -> Vec<Rect> {
        let mut out = self.regions.clone();
        if let Some(d) = self.drag {
            let (index, r) = self.drag_selection_rect(d);
            if r.width() >= MIN_REGION && r.height() >= MIN_REGION {
                if let Some(index) = index {
                    if let Some(region) = out.get_mut(index) {
                        *region = r;
                    }
                } else {
                    out.push(r)
                }
            }
        }
        if self.source == SourceMode::Display && out.is_empty() {
            // Picker chrome is temporary shell UI, not a hole in the source.
            // It is gone before capture execution and is composited after the
            // clean recording frame, so "Screen" must mean the whole output.
            out.push(self.status_rect())
        }
        if self.source == SourceMode::Window {
            out.clear();
            if let Some(target) = self.selected_candidate() {
                // Likewise, an app remains selectable beneath the temporary
                // picker bars. The picker may avoid placing handles there,
                // but capture geometry must never crop those pixels away.
                let visible = target.visible_rect.intersect(self.status_rect());
                if visible.area() > 0.0 {
                    out.push(visible);
                }
            }
        }
        out
    }
    fn has_capture_source(&self) -> bool {
        !self.active_regions().is_empty()
            || (self.source == SourceMode::Window
                && self.extent != CaptureExtent::Visible
                && self.selected_candidate().is_some())
    }

    fn default_picker_context(&self) -> String {
        // Use committed regions here. The in-canvas dimension badge follows
        // every pointer move; making this pill resize for every digit during a
        // drag adds visual chatter rather than useful feedback.
        let regions = &self.regions;
        if self.source == SourceMode::Region
            && let Some(region) = regions.last()
        {
            return format!(
                "{} × {} selected",
                region.width().round() as i64,
                region.height().round() as i64,
            );
        }
        if self.source == SourceMode::Multi && !regions.is_empty() {
            let noun = if regions.len() == 1 { "area" } else { "areas" };
            return format!(
                "{} {noun} selected. Shift-drag to add another.",
                regions.len()
            );
        }
        match (self.mode, self.source) {
            (CaptureMode::Screenshot, SourceMode::Region) => "Drag to select",
            (CaptureMode::Screenshot, SourceMode::Window) => "Choose an app",
            (CaptureMode::Screenshot, SourceMode::Display) => "Capture this screen",
            (CaptureMode::Screenshot, SourceMode::Multi) => "Combine several areas",
            (CaptureMode::Recording, SourceMode::Region) => "Drag an area to record",
            (CaptureMode::Recording, SourceMode::Window) => "App to record",
            (CaptureMode::Recording, SourceMode::Display) => "Record this screen",
            (CaptureMode::Recording, SourceMode::Multi) => "Areas to record together",
            (CaptureMode::Pip, SourceMode::Region) => "Drag an area to keep live",
            (CaptureMode::Pip, SourceMode::Window) => "App to float live",
            (CaptureMode::Pip, SourceMode::Display) => "Keep this screen live",
            (CaptureMode::Pip, SourceMode::Multi) => "Combine live areas",
        }
        .to_string()
    }

    fn recording_scope_label(&self) -> String {
        match self.extent {
            CaptureExtent::FullApp => self.selected_candidate().map_or_else(
                || "App window".to_string(),
                |target| format!("{} · app window", target.title),
            ),
            CaptureExtent::WholeStrip => format!("Strip · {} apps", self.candidates.len()),
            CaptureExtent::Visible => match self.source {
                SourceMode::Region => "Selection".into(),
                SourceMode::Window => self
                    .selected_candidate()
                    .map_or_else(|| "App".to_string(), |target| target.title.clone()),
                SourceMode::Display => "Screen".into(),
                SourceMode::Multi => format!("{} areas", self.active_regions().len()),
            },
        }
    }

    fn current_recording_regions(&self) -> Vec<Rect> {
        let regions = match self.extent {
            CaptureExtent::FullApp => self
                .selected_candidate()
                .map(|target| vec![target.visible_rect])
                .unwrap_or_default(),
            CaptureExtent::WholeStrip => self
                .candidates
                .iter()
                .map(|target| target.visible_rect)
                .collect(),
            CaptureExtent::Visible => self.active_regions(),
        };
        regions
            .into_iter()
            .map(|region| region.intersect(self.status_rect()))
            .filter(|region| region.width() > 1.0 && region.height() > 1.0)
            .collect()
    }

    fn current_recording_continuation_edges(&self) -> [bool; 4] {
        let virtual_scope = match self.extent {
            CaptureExtent::FullApp => self
                .selected_candidate()
                .map(|target| target.full_app_rect(CaptureMode::Recording)),
            CaptureExtent::WholeStrip => self
                .candidates
                .iter()
                .map(|target| target.virtual_rect)
                .reduce(|scope, rect| scope.union(rect)),
            CaptureExtent::Visible => None,
        };
        let Some(scope) = virtual_scope else {
            return [false; 4];
        };
        let bounds = self.status_rect();
        [
            scope.x0 < bounds.x0 - 0.5,
            scope.y0 < bounds.y0 - 0.5,
            scope.x1 > bounds.x1 + 0.5,
            scope.y1 > bounds.y1 + 0.5,
        ]
    }

    fn recording_motion(&self) -> f64 {
        let Some(started) = self.recording_started_at else {
            return 1.0;
        };
        if started.elapsed() <= RECORDING_HOLD {
            return 0.0;
        }
        let raw = started
            .elapsed()
            .saturating_sub(RECORDING_HOLD)
            .as_secs_f64()
            / RECORDING_SETTLE.as_secs_f64();
        let t = raw.clamp(0.0, 1.0);
        // Quintic smoothstep: zero velocity at both ends keeps the handoff
        // from reading as a toast that simply teleports into the bar.
        t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
    }

    /// Opacity for the short-lived outline that confirms what the encoder
    /// actually adopted. Wall time is the production clock; the transition
    /// override is also folded in so deterministic visual playback cannot
    /// leave a stale red perimeter around an already-docked recorder.
    fn recording_scope_confirmation_alpha(&self) -> f32 {
        let Some(started_at) = self.recording_started_at else {
            return 0.0;
        };
        let elapsed_t = (started_at.elapsed().as_secs_f64()
            / RECORDING_SCOPE_CONFIRMATION.as_secs_f64())
        .clamp(0.0, 1.0);
        let elapsed_eased = elapsed_t * elapsed_t * (3.0 - 2.0 * elapsed_t);

        // The source outline should be gone before the recorder reaches the
        // clock lane. This makes the two affordances read sequentially:
        // first confirm the source, then leave only the durable shell state.
        let flight_t = (self.recording_motion() / 0.62).clamp(0.0, 1.0);
        let flight_eased = flight_t * flight_t * (3.0 - 2.0 * flight_t);
        ((1.0 - elapsed_eased) * (1.0 - flight_eased)) as f32
    }

    fn replay_motion(&self) -> f64 {
        let Some(started) = self.replay_started_at else {
            return 1.0;
        };
        if started.elapsed() <= RECORDING_HOLD {
            return 0.0;
        }
        let raw = started
            .elapsed()
            .saturating_sub(RECORDING_HOLD)
            .as_secs_f64()
            / RECORDING_SETTLE.as_secs_f64();
        let t = raw.clamp(0.0, 1.0);
        t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
    }

    fn replay_indicator_rects(&self) -> (Rect, Rect, Rect) {
        self.replay_indicator_rects_at(self.replay_motion())
    }

    fn replay_indicator_rects_at(&self, p: f64) -> (Rect, Rect, Rect) {
        let bounds = self.status_rect();
        let compact = bounds.width() < 1000.0;
        let start_center_x = bounds.center().x;
        let final_width = if compact { 360.0 } else { 276.0 };
        let width = 268.0 + (final_width - 268.0) * p;
        let requested_center = if compact {
            start_center_x
        } else {
            // Mirror the normal recorder across the clock: replay is a
            // standing background capability, not an active take.
            start_center_x - 330.0
        };
        let edge_inset = width / 2.0 + 6.0;
        let end_center_x = requested_center
            .min(bounds.x1 - edge_inset)
            .max(bounds.x0 + edge_inset);
        let center_x = start_center_x + (end_center_x - start_center_x) * p;
        let final_y0 = bounds.y0 + if compact { 6.0 } else { 7.0 };
        let final_height = if compact { 28.0 } else { 34.0 };
        let y0 = bounds.y0 + 76.0 + (final_y0 - (bounds.y0 + 76.0)) * p;
        let height = 50.0 + (final_height - 50.0) * p;
        let pill = Rect::from_center_size(Point::new(center_x, y0 + height / 2.0), (width, height));
        let inset = 4.0 + if compact { 0.0 } else { 2.0 * p };
        let stop = Rect::new(
            pill.x1 - 34.0 - inset,
            pill.y0 + inset,
            pill.x1 - inset,
            pill.y1 - inset,
        );
        let save = Rect::new(stop.x0 - 72.0, stop.y0, stop.x0 - 4.0, stop.y1);
        (pill, save, stop)
    }

    fn screenshot_receipt_motion(receipt: &ScreenshotReceipt) -> f64 {
        let t = (receipt.shown_at.elapsed().as_secs_f64() / SCREENSHOT_RECEIPT_ENTER.as_secs_f64())
            .clamp(0.0, 1.0);
        // A spatial handoff from selected pixels to shell chrome needs zero
        // velocity at both ends or the thumbnail appears to snap into place.
        t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
    }

    fn screenshot_receipt_rects(&self) -> (Rect, Rect, Rect, Rect, Rect) {
        let Some(receipt) = self.screenshot_receipt.as_ref() else {
            return (Rect::ZERO, Rect::ZERO, Rect::ZERO, Rect::ZERO, Rect::ZERO);
        };
        self.screenshot_receipt_rects_at(Self::screenshot_receipt_motion(receipt), receipt.origin)
    }

    fn screenshot_receipt_rects_at(&self, p: f64, origin: Point) -> (Rect, Rect, Rect, Rect, Rect) {
        let Some(receipt) = self.screenshot_receipt.as_ref() else {
            return (Rect::ZERO, Rect::ZERO, Rect::ZERO, Rect::ZERO, Rect::ZERO);
        };
        let bounds = self.status_rect();
        let compact = bounds.width() < 1000.0;
        let final_width = if compact { 394.0 } else { 354.0 };
        // Begin as a recognizable thumbnail rather than a nearly empty tiny
        // capsule. The object compresses vertically while it gains enough
        // horizontal room for metadata and actions in the top bar.
        let start_preview =
            Self::fit_screenshot_preview(receipt.width, receipt.height, 144.0, 66.0);
        let start_width = start_preview.width + 12.0;
        let width = start_width + (final_width - start_width) * p;
        let final_height = if compact { 28.0 } else { 34.0 };
        let start_height = start_preview.height + 12.0;
        let height = start_height + (final_height - start_height) * p;
        let clock_right = bounds.center().x + 180.0;
        let requested_x = if compact {
            bounds.center().x
        } else {
            clock_right + 12.0 + final_width / 2.0
        };
        let edge_inset = final_width / 2.0 + 6.0;
        let final_x = requested_x
            .min(bounds.x1 - edge_inset)
            .max(bounds.x0 + edge_inset);
        let final_y = bounds.y0 + 20.0;
        let start = Point::new(
            origin.x.clamp(bounds.x0 + 32.0, bounds.x1 - 32.0),
            origin.y.clamp(bounds.y0 + 20.0, bounds.y1 - 20.0),
        );
        let center = Point::new(
            start.x + (final_x - start.x) * p,
            start.y + (final_y - start.y) * p,
        );
        let pill = Rect::from_center_size(center, (width, height));
        let inset = 4.0 + if compact { 0.0 } else { 2.0 * p };
        let dismiss = Rect::new(
            pill.x1 - 28.0 - inset,
            pill.y0 + inset,
            pill.x1 - inset,
            pill.y1 - inset,
        );
        let reveal = Rect::new(dismiss.x0 - 34.0, dismiss.y0, dismiss.x0 - 4.0, dismiss.y1);
        let copy = Rect::new(reveal.x0 - 34.0, reveal.y0, reveal.x0 - 4.0, reveal.y1);
        let pin = Rect::new(copy.x0 - 34.0, copy.y0, copy.x0 - 4.0, copy.y1);
        (pill, pin, copy, reveal, dismiss)
    }

    /// The receipt's Send button, left of Pin.
    fn screenshot_receipt_send_rect(pin: Rect) -> Rect {
        Rect::new(pin.x0 - 34.0, pin.y0, pin.x0 - 4.0, pin.y1)
    }

    fn fit_screenshot_preview(
        width: u32,
        height: u32,
        max_width: f64,
        max_height: f64,
    ) -> KurboSize {
        let aspect = f64::from(width.max(1)) / f64::from(height.max(1));
        if max_width / max_height > aspect {
            KurboSize::new(max_height * aspect, max_height)
        } else {
            KurboSize::new(max_width, max_width / aspect)
        }
    }

    fn screenshot_receipt_preview_rect(
        p: f64,
        pill: Rect,
        capture_width: u32,
        capture_height: u32,
    ) -> Rect {
        let start = Self::fit_screenshot_preview(capture_width, capture_height, 144.0, 66.0);
        let end = Self::fit_screenshot_preview(capture_width, capture_height, 46.0, 24.0);
        let width = start.width + (end.width - start.width) * p;
        let height = start.height + (end.height - start.height) * p;
        let inset = 6.0 + 2.0 * p;
        Rect::from_center_size(
            Point::new(pill.x0 + inset + width / 2.0, pill.center().y),
            (width, height),
        )
    }

    fn recording_elapsed(&self) -> Duration {
        let Some(started) = self.recording_started_at else {
            return Duration::ZERO;
        };
        let now = crate::time::now();
        let current_pause = self
            .recording_paused_at
            .map_or(Duration::ZERO, |paused_at| {
                now.saturating_duration_since(paused_at)
            });
        now.saturating_duration_since(started)
            .saturating_sub(self.recording_paused_total)
            .saturating_sub(current_pause)
    }

    fn recording_scope_icon(&self) -> Icon {
        match self.recording_continuation_edges {
            [true, false, false, false] => Icon::ChevronLeft,
            [false, true, false, false] => Icon::ChevronUp,
            [false, false, true, false] => Icon::ChevronRight,
            [false, false, false, true] => Icon::ChevronDown,
            edges if edges.into_iter().any(|continues| continues) => Icon::Layers,
            _ => match self.extent {
                CaptureExtent::FullApp => Icon::GalleryVerticalEnd,
                CaptureExtent::WholeStrip => Icon::Layers,
                CaptureExtent::Visible => match self.source {
                    SourceMode::Region => Icon::Scissors,
                    SourceMode::Window => Icon::GalleryVerticalEnd,
                    SourceMode::Display => Icon::Monitor,
                    SourceMode::Multi => Icon::Layers,
                },
            },
        }
    }

    fn compact_recording_scope(&self, character_limit: usize) -> String {
        let scope = self
            .recording_scope
            .strip_suffix(" · app window")
            .unwrap_or(&self.recording_scope);
        scope.chars().take(character_limit).collect()
    }

    fn recording_status_detail(&self) -> String {
        let scope = if self.recording_scope.is_empty() {
            "Recording"
        } else {
            &self.recording_scope
        };
        let audio = match self.audio {
            AudioCaptureMode::None => "no audio",
            AudioCaptureMode::System => "system audio",
            AudioCaptureMode::Microphone => "microphone",
            AudioCaptureMode::SystemAndMicrophone => "system audio + microphone",
        };
        if self.include_pointer {
            format!("{scope} · pointer · {audio}")
        } else {
            format!("{scope} · {audio}")
        }
    }

    fn replay_status_label(&self, saved: bool) -> String {
        if saved {
            format!("Saved · {}s", self.replay_buffered_seconds.max(1))
        } else if self.replay_buffered_seconds == 0 {
            "Buffering…".to_string()
        } else if self.replay_buffered_seconds < 30 {
            format!("Buffering · {}s", self.replay_buffered_seconds)
        } else {
            "Replay · last 30s".to_string()
        }
    }

    fn replay_saved_actions_visible(&self) -> bool {
        self.replay_saved_at
            .is_some_and(|at| at.elapsed() < REPLAY_SAVED_ACTION_DURATION)
    }

    fn replay_saved_action_rects(&self) -> (Rect, Rect) {
        let (_, action, _) = self.replay_indicator_rects();
        let gap = 4.0;
        let half = (action.width() - gap) / 2.0;
        (
            Rect::new(action.x0, action.y0, action.x0 + half, action.y1),
            Rect::new(action.x0 + half + gap, action.y0, action.x1, action.y1),
        )
    }

    fn replay_status_detail(&self) -> String {
        let audio = match self.replay_audio {
            AudioCaptureMode::None => "no audio",
            AudioCaptureMode::System => "system audio",
            AudioCaptureMode::Microphone => "microphone",
            AudioCaptureMode::SystemAndMicrophone => "system audio + microphone",
        };
        format!("30-second rolling buffer · {audio} · Super+Shift+R saves now")
    }

    fn draw_status_tooltip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        anchor: Rect,
        detail: &str,
    ) {
        let colors = &theme.tokens.colors;
        let (detail_width, _) = text_renderer.measure_style(
            detail,
            None,
            &theme
                .text(TextRole::Caption)
                .with_weight(TextWeight::Medium),
        );
        let bounds = self.status_rect();
        let tooltip_width =
            (f64::from(detail_width) + 24.0).clamp(190.0, (bounds.width() - 24.0).min(420.0));
        let tooltip_x0 = (anchor.center().x - tooltip_width / 2.0)
            .clamp(bounds.x0 + 12.0, bounds.x1 - tooltip_width - 12.0);
        let tooltip = Rect::new(
            tooltip_x0,
            anchor.y1 + 8.0,
            tooltip_x0 + tooltip_width,
            anchor.y1 + 36.0,
        );
        surface::shadow(
            scene,
            transform,
            tooltip,
            14.0,
            &theme.tokens.elevation.floating,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(tooltip, 14.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::tokens::colors::Colors::hairline_on(colors.surfaces.raised, 26),
            None,
            &RoundedRect::from_rect(tooltip.inflate(-0.5, -0.5), 13.5),
        );
        let detail_top = centered_text_top(
            text_renderer,
            &theme.text(TextRole::Label),
            tooltip.center().y,
        );
        let shown = fit_label(
            text_renderer,
            detail,
            &theme.text(TextRole::Label),
            tooltip.width() - 24.0,
        );
        text_renderer.draw_style(
            scene,
            &shown,
            colors.text_dim,
            transform.pre_translate((tooltip.x0 + 12.0, detail_top).into()),
            None,
            &theme
                .text(TextRole::Caption)
                .with_weight(TextWeight::Medium),
        );
    }

    fn recording_state_motion(&self) -> f64 {
        if let Some(progress) = self.recording_state_progress_override {
            return progress;
        }
        let Some(started) = self.recording_state_changed_at else {
            return 1.0;
        };
        let raw = started.elapsed().as_secs_f64() / RECORDING_STATE_SETTLE.as_secs_f64();
        let t = raw.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    fn recording_label_alphas(progress: f64) -> (f32, f32) {
        let intro_raw = ((progress - 0.30) / 0.28).clamp(0.0, 1.0);
        let intro_eased = intro_raw * intro_raw * (3.0 - 2.0 * intro_raw);
        let settled_raw = ((progress - 0.40) / 0.26).clamp(0.0, 1.0);
        let settled_eased = settled_raw * settled_raw * (3.0 - 2.0 * settled_raw);
        let intro = 1.0 - intro_eased;
        let settled = settled_eased;
        let total = (intro + settled).max(f64::EPSILON);
        ((intro / total) as f32, (settled / total) as f32)
    }

    fn recording_indicator_rects(&self) -> (Rect, Rect, Rect) {
        self.recording_indicator_rects_at(self.recording_motion())
    }

    fn recording_receipt_rect(&self) -> Rect {
        self.recording_indicator_rects_at(1.0).0
    }

    fn recording_receipt_rects(&self) -> (Rect, Rect, Rect, Rect) {
        let pill = self.recording_receipt_rect();
        let compact = self.status_rect().width() < 1000.0;
        let (open, reveal, dismiss) = Self::recording_receipt_action_rects(pill, compact);
        (pill, open, reveal, dismiss)
    }

    fn recording_receipt_action_rects(pill: Rect, compact: bool) -> (Rect, Rect, Rect) {
        let inset = if compact { 3.0 } else { 5.0 };
        let size = pill.height() - inset * 2.0;
        let dismiss = Rect::new(
            pill.x1 - inset - size,
            pill.y0 + inset,
            pill.x1 - inset,
            pill.y1 - inset,
        );
        let reveal = Rect::new(
            dismiss.x0 - 4.0 - size,
            dismiss.y0,
            dismiss.x0 - 4.0,
            dismiss.y1,
        );
        let open = Rect::new(
            reveal.x0 - 4.0 - size,
            reveal.y0,
            reveal.x0 - 4.0,
            reveal.y1,
        );
        (open, reveal, dismiss)
    }

    fn recording_indicator_rects_at(&self, p: f64) -> (Rect, Rect, Rect) {
        let bounds = self.status_rect();
        let p = p.clamp(0.0, 1.0);
        // Leave the app's tab row immediately, travel in the quiet lane just
        // beneath the top bar, then dock. Moving sideways first made the
        // confirmation pill sweep across tab labels; moving diagonally made
        // it cut through the clock. These three continuous stages keep one
        // object visible while avoiding both pieces of live UI.
        let depart_raw = (p / 0.28).clamp(0.0, 1.0);
        let depart_p = depart_raw * depart_raw * (3.0 - 2.0 * depart_raw);
        let horizontal_raw = ((p - 0.18) / 0.62).clamp(0.0, 1.0);
        let horizontal_p = horizontal_raw * horizontal_raw * (3.0 - 2.0 * horizontal_raw);
        let dock_raw = ((p - 0.76) / 0.24).clamp(0.0, 1.0);
        let dock_p = dock_raw * dock_raw * (3.0 - 2.0 * dock_raw);
        let start_center_x = bounds.center().x;
        let designed_width: f64 = if bounds.width() >= 1000.0 {
            252.0
        } else {
            // Match the compact clock capsule exactly. The recording state
            // should read as the clock transforming, never as a pill nested
            // inside another pill.
            360.0
        };
        // At 1× the designed width wins — it trades the tail of a long
        // scope for the clock alignment. Above 1× the content can no longer
        // fit even the timer, so the pill grows to what the painter measured
        // last frame (`docked_pill_min_width`, zero at 1×).
        let final_width = designed_width.max(self.docked_pill_min_width);
        // The confirmation starts roomy enough to name the selected source,
        // then sheds that explanatory width as it joins the persistent bar.
        let width = 304.0 + (final_width - 304.0) * horizontal_p;
        // Wide bars have a stable slot immediately right of the center
        // widget. Compact bars do not; there the recording state deliberately
        // takes over the clock position instead of colliding with clock/tray.
        let available_shift = if bounds.width() >= 1000.0 {
            // The center clock capsule ends around `center + 180`; a 318px
            // shift leaves a deliberate 12px breathing gap beside this
            // 252px settled pill instead of visually welding the two shapes.
            318.0
        } else {
            0.0
        };
        let edge_inset = width / 2.0 + 6.0;
        let end_center_x = (start_center_x + available_shift)
            .min(bounds.x1 - edge_inset)
            .max(bounds.x0 + edge_inset);
        let center_x = start_center_x + (end_center_x - start_center_x) * horizontal_p;
        let compact = bounds.width() < 1000.0;
        let final_y0 = bounds.y0 + if compact { 6.0 } else { 7.0 };
        let final_height = if compact { 28.0 } else { 34.0 };
        // Launch inside the selected content, below its tab strip. The old
        // y=76 origin visually welded the shell recorder to app tabs and made
        // the control look app-owned for its first few frames.
        let start_y0 = bounds.y0 + 136.0;
        let lane_y0 = bounds.y0 + 42.0;
        let y0 = start_y0 + (lane_y0 - start_y0) * depart_p + (final_y0 - lane_y0) * dock_p;
        let lane_height = 42.0;
        let height = 50.0 + (lane_height - 50.0) * depart_p + (final_height - lane_height) * dock_p;
        let pill = Rect::new(
            center_x - width / 2.0,
            y0,
            center_x + width / 2.0,
            y0 + height,
        );
        let stop_width = 64.0_f64.max(self.docked_stop_width);
        let inset = 4.0 + if compact { 0.0 } else { 2.0 * p };
        let stop = Rect::new(
            pill.x1 - stop_width - inset,
            pill.y0 + inset,
            pill.x1 - inset,
            pill.y1 - inset,
        );
        let pause = Rect::new(stop.x0 - 34.0, stop.y0, stop.x0 - 4.0, stop.y1);
        (pill, pause, stop)
    }

    /// Record what the docked recorder's content needs at the current type
    /// magnification, so [`Self::recording_indicator_rects_at`] can grow the
    /// pill and its Stop button around it. `label_w` is the widest settled
    /// label, `stop_label_w` the "Stop" caption, both measured at the size
    /// and weight they are painted with. Zero at 1× — the designed widths
    /// are the 1× layout — so nothing moves until the type does.
    fn remember_docked_content_widths(
        &mut self,
        text_renderer: &TextRenderer,
        theme: &Theme,
        label_w: f64,
        stop_label_w: f64,
    ) {
        // The same magnification rule the presence pills use: the theme's
        // type step against the default, so a type grown by `with_ui_scale`
        // (which leaves `Theme::text_scale` at 1) counts as well as one grown
        // by the text scale. A renderer scaled without the theme no longer
        // magnifies glyphs (they are drawn at the renderer-relative size), so
        // it does not count.
        let magnification = crate::presence::label_magnification(theme, text_renderer);
        if magnification <= 1.0 + 1e-6 {
            self.reset_docked_content_widths();
            return;
        }
        // Both floors are monotone for the life of a recording: the label
        // alternates between the timer, "Paused · …" and "Resumed · …" and
        // its digits change width, and a pill that re-fitted itself every
        // frame would slide Pause and Stop under a resting pointer. They are
        // cleared when the recording ends (`reset_docked_content_widths`).
        //
        // "Stop" is painted at `stop.x0 + 22`; keep the same trailing room
        // the 64 px button gives a 1× caption.
        let stop_width = (22.0 + stop_label_w + 8.0).max(64.0);
        self.docked_stop_width = self.docked_stop_width.max(stop_width);
        // The settled label starts at `pill.x0 + 43`, ends 8 px before the
        // pause button (30 px + 4 px gap), which sits ahead of the stop
        // button and its 6 px inset — the sum the painter's `fit_label`
        // budget is derived from.
        let pill_width = 43.0 + label_w + 8.0 + 34.0 + self.docked_stop_width + 6.0;
        self.docked_pill_min_width = self.docked_pill_min_width.max(pill_width);
    }

    /// Forget the content-driven floors; the next recording measures afresh.
    fn reset_docked_content_widths(&mut self) {
        self.docked_stop_width = 0.0;
        self.docked_pill_min_width = 0.0;
    }

    fn queue_recording_command(&mut self, command: RecordingCommand) {
        if self.stop_pending
            || (self.pause_pending
                && !matches!(
                    command,
                    RecordingCommand::Stop
                        | RecordingCommand::OpenLast
                        | RecordingCommand::RevealLast
                ))
        {
            return;
        }
        self.pending.push(CaptureIntent {
            mode: CaptureMode::Recording,
            source: self.source,
            destination: CaptureDestination::File,
            regions: Vec::new(),
            include_pointer: self.include_pointer,
            audio: AudioCaptureMode::None,
            extent: self.extent,
            pip_layout: self.pip_layout,
            targets: Vec::new(),
            recording_command: Some(command),
            replay_command: None,
            screenshot_command: None,
        });
        match command {
            RecordingCommand::Pause | RecordingCommand::Resume => self.pause_pending = true,
            RecordingCommand::Stop => self.stop_pending = true,
            RecordingCommand::Start | RecordingCommand::OpenLast | RecordingCommand::RevealLast => {
            }
        }
    }

    fn queue_replay_command(&mut self, command: ReplayCommand) {
        if matches!(command, ReplayCommand::Start) && !self.has_capture_source() {
            return;
        }
        let starting = matches!(command, ReplayCommand::Start);
        self.pending.push(CaptureIntent {
            mode: CaptureMode::Recording,
            source: self.source,
            destination: CaptureDestination::File,
            regions: if starting {
                self.active_regions()
            } else {
                Vec::new()
            },
            include_pointer: self.include_pointer,
            audio: if starting {
                self.audio
            } else {
                AudioCaptureMode::None
            },
            extent: self.extent,
            pip_layout: self.pip_layout,
            targets: if starting {
                if self.extent == CaptureExtent::WholeStrip {
                    self.candidates.clone()
                } else {
                    self.selected_candidate().cloned().into_iter().collect()
                }
            } else {
                Vec::new()
            },
            recording_command: None,
            replay_command: Some(command),
            screenshot_command: None,
        });
        if starting {
            self.close();
        }
    }

    fn queue_screenshot_command(&mut self, command: ScreenshotCommand, spawn_from: Option<Rect>) {
        self.pending.push(CaptureIntent {
            mode: CaptureMode::Screenshot,
            source: self.source,
            destination: CaptureDestination::File,
            // Post-capture commands never use this as a recapture source. A
            // pinned still carries only the receipt thumbnail's current
            // screen-space rect so the new surface can grow out of it.
            regions: spawn_from.into_iter().collect(),
            include_pointer: false,
            audio: AudioCaptureMode::None,
            extent: CaptureExtent::Visible,
            pip_layout: PipLayout::Auto,
            targets: Vec::new(),
            recording_command: None,
            replay_command: None,
            screenshot_command: Some(command),
        });
    }

    fn commit(&mut self, destination: CaptureDestination) {
        let regions = self.active_regions();
        if self.mode == CaptureMode::Screenshot {
            let source_bounds = regions
                .iter()
                .copied()
                .reduce(|bounds, region| bounds.union(region))
                .or_else(|| {
                    self.selected_candidate()
                        .map(|target| target.visible_rect)
                        .filter(|rect| rect.area() > 0.0)
                });
            let center = source_bounds.map_or_else(|| self.rect.center(), |rect| rect.center());
            self.screenshot_origin = Some(Point::new(
                center.x.clamp(self.rect.x0 + 24.0, self.rect.x1 - 24.0),
                center.y.clamp(self.rect.y0 + 24.0, self.rect.y1 - 24.0),
            ));
        }
        let stopping = self.mode == CaptureMode::Recording
            && self.recording_active
            && destination == CaptureDestination::File;
        if stopping && self.stop_pending {
            return;
        }
        if stopping {
            self.queue_recording_command(RecordingCommand::Stop);
            return;
        }
        if !self.has_capture_source() && !stopping {
            return;
        }
        let intent = CaptureIntent {
            mode: self.mode,
            source: self.source,
            destination,
            regions,
            include_pointer: self.include_pointer,
            audio: self.audio,
            extent: self.extent,
            pip_layout: self.pip_layout,
            targets: if self.extent == CaptureExtent::WholeStrip {
                self.candidates.clone()
            } else {
                self.selected_candidate().cloned().into_iter().collect()
            },
            recording_command: (self.mode == CaptureMode::Recording)
                .then_some(RecordingCommand::Start),
            replay_command: None,
            screenshot_command: None,
        };
        if self.delay_seconds > 0 && self.mode != CaptureMode::Pip {
            self.drag = None;
            self.armed_capture = Some(ArmedCapture {
                intent,
                started_at: crate::time::now(),
                duration: Duration::from_secs(u64::from(self.delay_seconds)),
            });
        } else {
            let starts_recording = intent.recording_command == Some(RecordingCommand::Start);
            self.pending.push(intent);
            self.close();
            if starts_recording {
                self.begin_recording_request();
            }
        }
    }

    fn click(&mut self, p: Point) {
        self.click_with_additive(p, false);
    }

    fn click_with_additive(&mut self, p: Point, additive: bool) {
        if self.replay_active {
            let (_, save, stop) = self.replay_indicator_rects();
            if self.replay_saved_actions_visible() {
                let (play, reveal) = self.replay_saved_action_rects();
                if play.contains(p) {
                    self.queue_recording_command(RecordingCommand::OpenLast);
                } else if reveal.contains(p) {
                    self.queue_recording_command(RecordingCommand::RevealLast);
                } else if stop.contains(p) {
                    self.queue_replay_command(ReplayCommand::Stop);
                }
            } else if save.contains(p) && self.replay_buffered_seconds > 0 {
                self.queue_replay_command(ReplayCommand::SaveRecent);
            } else if stop.contains(p) {
                self.queue_replay_command(ReplayCommand::Stop);
            }
            return;
        }
        if let Some(receipt) = self.screenshot_receipt.as_ref() {
            if self.screenshot_pin_pending {
                return;
            }
            let origin = receipt.origin;
            let (pill, pin, copy, reveal, dismiss) = self.screenshot_receipt_rects();
            let preview = Self::screenshot_receipt_preview_rect(
                Self::screenshot_receipt_motion(receipt),
                pill,
                receipt.width,
                receipt.height,
            );
            if !receipt.copied && preview.contains(p) {
                self.queue_screenshot_command(ScreenshotCommand::MarkupLast, Some(preview));
                self.screenshot_receipt = None;
                self.open = false;
            } else if !receipt.copied && Self::screenshot_receipt_send_rect(pin).contains(p) {
                self.open_send_menu(Self::screenshot_receipt_send_rect(pin));
            } else if !receipt.copied && pin.contains(p) {
                self.queue_screenshot_command(ScreenshotCommand::PinLast, Some(preview));
                self.screenshot_pin_pending = true;
            } else if !receipt.copied && copy.contains(p) {
                self.screenshot_origin = Some(origin);
                self.queue_screenshot_command(ScreenshotCommand::CopyLast, None);
                self.screenshot_receipt = None;
                self.open = false;
            } else if !receipt.copied && reveal.contains(p) {
                self.queue_screenshot_command(ScreenshotCommand::RevealLast, None);
                self.screenshot_receipt = None;
                self.open = false;
            } else if dismiss.contains(p) {
                self.screenshot_receipt = None;
                self.open = false;
            }
            return;
        }
        if self.recording_finished_at.is_some() {
            let (_, open, reveal, dismiss) = self.recording_receipt_rects();
            if open.contains(p) {
                self.queue_recording_command(RecordingCommand::OpenLast);
                self.recording_finished_at = None;
                self.open = false;
            } else if reveal.contains(p) {
                self.queue_recording_command(RecordingCommand::RevealLast);
                self.recording_finished_at = None;
                self.open = false;
            } else if dismiss.contains(p) {
                self.recording_finished_at = None;
                self.open = false;
            }
            return;
        }
        if self.recording_active {
            let (_, pause, stop) = self.recording_indicator_rects();
            if !self.stop_pending && !self.pause_pending && pause.contains(p) {
                self.queue_recording_command(if self.recording_paused {
                    RecordingCommand::Resume
                } else {
                    RecordingCommand::Pause
                });
            } else if !self.stop_pending && stop.contains(p) {
                self.commit(CaptureDestination::File);
            }
            return;
        }
        if self.armed_capture.is_some() {
            return;
        }
        for (i, r) in self.mode_rects().into_iter().enumerate() {
            if r.contains(p) {
                self.set_mode(
                    [
                        CaptureMode::Screenshot,
                        CaptureMode::Recording,
                        CaptureMode::Pip,
                    ][i],
                );
                return;
            }
        }
        for (i, r) in self.source_rects().into_iter().enumerate() {
            if r.contains(p) {
                self.set_source_mode(
                    [
                        SourceMode::Region,
                        SourceMode::Window,
                        SourceMode::Display,
                        SourceMode::Multi,
                    ][i],
                );
                return;
            }
        }
        if self.source == SourceMode::Window {
            let picked = self
                .candidates
                .iter()
                .zip(self.candidate_picker_rects())
                .find(|(_, rect)| rect.contains(p))
                .map(|(target, _)| target.id.clone());
            if let Some(id) = picked {
                self.selected_target = Some(id);
                return;
            }
        }
        if matches!(self.source, SourceMode::Window | SourceMode::Multi) {
            for (i, r) in self.option_rects().into_iter().enumerate() {
                if r.contains(p) {
                    if self.source == SourceMode::Multi {
                        self.pip_layout = [PipLayout::Auto, PipLayout::Grid, PipLayout::Stack][i];
                    } else {
                        self.extent = [
                            CaptureExtent::Visible,
                            CaptureExtent::FullApp,
                            CaptureExtent::WholeStrip,
                        ][i];
                    }
                    return;
                }
            }
        }
        let pointer = self.pointer_rect();
        if self.mode != CaptureMode::Pip && pointer.contains(p) {
            self.include_pointer = !self.include_pointer;
            return;
        }
        if self.system_audio_rect().contains(p) {
            self.audio = self.audio.with_system(!self.audio.includes_system());
            return;
        }
        if self.microphone_rect().contains(p) {
            self.audio = self
                .audio
                .with_microphone(!self.audio.includes_microphone());
            return;
        }
        if self.delay_rect().contains(p) {
            self.cycle_delay();
            return;
        }
        for (i, r) in self.action_rects().into_iter().enumerate() {
            if r.area() > 0.0 && r.contains(p) {
                if self.mode == CaptureMode::Recording && i == 1 {
                    self.queue_replay_command(ReplayCommand::Start);
                    return;
                }
                self.commit(
                    [
                        CaptureDestination::Clipboard,
                        CaptureDestination::FloatingWindow,
                        CaptureDestination::File,
                    ][i],
                );
                return;
            }
        }
        if self.selection_bounds().contains(p) || self.selection_handle_at(p).is_some() {
            if self.source == SourceMode::Window {
                if let Some(target) = self
                    .candidates
                    .iter()
                    .rev()
                    .find(|target| target.visible_rect.contains(p))
                {
                    self.selected_target = Some(target.id.clone());
                }
                return;
            }
            if matches!(self.source, SourceMode::Region | SourceMode::Multi)
                && let Some(index) = self
                    .region_remove_rects
                    .iter()
                    .position(|remove| remove.contains(p))
            {
                self.regions.remove(index);
                self.region_remove_rects.clear();
                return;
            }
            if matches!(self.source, SourceMode::Region | SourceMode::Multi)
                && self.begin_region_edit(p)
            {
                return;
            }
            if self.source != SourceMode::Multi || !additive {
                self.regions.clear()
            }
            self.drag = Some(DragSelection::Create {
                start: p,
                current: p,
            })
        }
    }

    fn symbol_button(
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        r: Rect,
        active: bool,
        hovered: bool,
        icon: Icon,
        active_fill: Color,
    ) {
        let c = &theme.tokens.colors;
        let fill = if active {
            active_fill
        } else if hovered {
            c.lift(0.72)
        } else {
            c.lift(0.35)
        };
        // A resting face keeps a derived edge (HIG §9.5 Button Neutral): the
        // 0.35 lift alone is a whisper on a white group. The active face is
        // its own contrast step.
        if active {
            scene.fill(
                Fill::NonZero,
                t,
                fill,
                None,
                &RoundedRect::from_rect(r, 10.0),
            );
        } else {
            surface::card(scene, t, r, 10.0, fill);
        }
        icons::draw_icon(
            scene,
            t,
            Rect::new(
                r.center().x - 9.0,
                r.center().y - 9.0,
                r.center().x + 9.0,
                r.center().y + 9.0,
            ),
            icon,
            if active || hovered {
                c.text_primary
            } else {
                c.text_dim
            },
            icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }
    fn toggle_button(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
        on: bool,
        hovered: bool,
        icon: Icon,
    ) {
        let face = if on {
            ButtonFace::new(r, "").role(ButtonRole::Primary)
        } else {
            ButtonFace::new(r, "")
        }
        .icon(icon)
        .hovered(hovered);
        Button::paint(scene, t, tr, theme, &face);
    }

    /// The tint a capture in the current mode marks its selection with.
    fn selection_color(&self, theme: &Theme) -> Color {
        let c = &theme.tokens.colors;
        match self.mode {
            CaptureMode::Screenshot => c.intent.inverted_hover,
            CaptureMode::Recording => c.semantic.danger,
            CaptureMode::Pip => c.semantic.info,
        }
    }

    /// The highest a window candidate's badge may sit: clear of the
    /// candidate picker, or of the top bar when there is none.
    fn min_target_badge_y(&self) -> f64 {
        self.candidate_picker_rects()
            .first()
            .map_or_else(|| self.top_bar().y1 + 8.0, |picker| picker.y1 + 8.0)
    }

    fn target_badge(
        &self,
        ctx: &mut PaintCtx<'_>,
        target: &CaptureTarget,
        rect: Rect,
        selected: bool,
    ) {
        let PaintCtx {
            scene,
            transform: t,
            text: tr,
            theme,
        } = ctx.reborrow();
        let (mode, full_app) = (self.mode, self.extent == CaptureExtent::FullApp);
        let selection_color = self.selection_color(theme);
        let min_badge_y = self.min_target_badge_y();
        let c = &theme.tokens.colors;
        let prefix = if selected && mode == CaptureMode::Pip {
            "LIVE · "
        } else {
            ""
        };
        let suffix = if selected && full_app {
            let full = target.full_app_rect(mode);
            let scope = if mode == CaptureMode::Screenshot {
                "FULL APP"
            } else {
                "APP WINDOW"
            };
            format!(
                "  {scope}  {}×{}",
                full.width().round() as i64,
                full.height().round() as i64
            )
        } else {
            String::new()
        };
        let label = format!("{prefix}{}{}", target.title, suffix);
        let (tw, _) = tr.measure_style(
            &label,
            None,
            &theme
                .text(TextRole::Caption)
                .with_weight(TextWeight::Semibold),
        );
        let width = (f64::from(tw) + 48.0).min((rect.width() - 20.0).max(80.0));
        let badge_y = (rect.y0 + 10.0)
            .max(min_badge_y)
            .min((rect.y1 - 44.0).max(rect.y0 + 10.0));
        let badge = Rect::new(
            rect.x0 + 10.0,
            badge_y,
            rect.x0 + 10.0 + width,
            badge_y + 34.0,
        );
        scene.fill(
            Fill::NonZero,
            t,
            if selected {
                selection_color
            } else {
                c.surfaces.raised
            },
            None,
            &RoundedRect::from_rect(badge, 12.0),
        );
        icons::draw_icon(
            scene,
            t,
            Rect::new(
                badge.x0 + 10.0,
                badge.y0 + 8.0,
                badge.x0 + 28.0,
                badge.y0 + 26.0,
            ),
            Icon::GalleryVerticalEnd,
            if selected { c.text_primary } else { c.text_dim },
            1.5,
        );
        let label_top = centered_text_top(
            tr,
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
            badge.center().y,
        );
        let shown = fit_label(
            tr,
            &label,
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
            badge.width() - 42.0,
        );
        tr.draw_style(
            scene,
            &shown,
            if selected { c.text_primary } else { c.text_dim },
            t.pre_translate((badge.x0 + 34.0, label_top).into()),
            None,
            &theme
                .text(TextRole::Caption)
                .with_weight(TextWeight::Semibold),
        );
    }

    fn virtual_continuation_edges(
        target: &CaptureTarget,
        mode: CaptureMode,
        bounds: Rect,
    ) -> [bool; 4] {
        let scope = target.full_app_rect(mode);
        [
            scope.x0 < bounds.x0 - 0.5,
            scope.y0 < bounds.y0 - 0.5,
            scope.x1 > bounds.x1 + 0.5,
            scope.y1 > bounds.y1 + 0.5,
        ]
    }

    fn draw_continuation_markers(
        scene: &mut Scene,
        transform: Affine,
        bounds: Rect,
        edges: [bool; 4],
        accent: Color,
    ) {
        let [left, top, right, bottom] = edges;
        let center = bounds.center();
        let markers = [
            (
                left,
                Rect::new(
                    bounds.x0 + 3.0,
                    center.y - 20.0,
                    bounds.x0 + 6.0,
                    center.y + 20.0,
                ),
                Rect::from_center_size(Point::new(bounds.x0 + 14.0, center.y), (16.0, 16.0)),
                Icon::ChevronLeft,
            ),
            (
                top,
                Rect::new(
                    center.x - 20.0,
                    bounds.y0 + 3.0,
                    center.x + 20.0,
                    bounds.y0 + 6.0,
                ),
                Rect::from_center_size(Point::new(center.x, bounds.y0 + 14.0), (16.0, 16.0)),
                Icon::ChevronUp,
            ),
            (
                right,
                Rect::new(
                    bounds.x1 - 6.0,
                    center.y - 20.0,
                    bounds.x1 - 3.0,
                    center.y + 20.0,
                ),
                Rect::from_center_size(Point::new(bounds.x1 - 14.0, center.y), (16.0, 16.0)),
                Icon::ChevronRight,
            ),
            (
                bottom,
                Rect::new(
                    center.x - 20.0,
                    bounds.y1 - 6.0,
                    center.x + 20.0,
                    bounds.y1 - 3.0,
                ),
                Rect::from_center_size(Point::new(center.x, bounds.y1 - 14.0), (16.0, 16.0)),
                Icon::ChevronDown,
            ),
        ];
        for (visible, rail, icon_rect, icon) in markers {
            if visible {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    accent,
                    None,
                    &RoundedRect::from_rect(rail, 1.5),
                );
                icons::draw_icon(scene, transform, icon_rect, icon, accent, 1.8);
            }
        }
    }

    /// A card in the selection bounds standing in for a selected `target`
    /// with no useful on-screen aperture.
    fn draw_offscreen_target(&self, ctx: &mut PaintCtx<'_>, target: &CaptureTarget) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let (bounds, mode, extent) = (self.selection_bounds(), self.mode, self.extent);
        let accent = self.selection_color(theme);
        let colors = &theme.tokens.colors;
        let width = 430.0_f64.min(bounds.width() - 40.0);
        let card = Rect::from_center_size(bounds.center(), (width, 132.0));
        surface::shadow(
            scene,
            transform,
            card,
            18.0,
            &theme.tokens.elevation.floating,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised.with_alpha_factor(0.97),
            None,
            &RoundedRect::from_rect(card, 18.0),
        );
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            accent.with_alpha_factor(0.72),
            None,
            &RoundedRect::from_rect(card.inflate(-0.75, -0.75), 17.25),
        );
        let icon = Rect::new(
            card.x0 + 20.0,
            card.y0 + 22.0,
            card.x0 + 64.0,
            card.y0 + 66.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            accent.with_alpha_factor(0.16),
            None,
            &RoundedRect::from_rect(icon, 14.0),
        );
        icons::draw_icon(
            scene,
            transform,
            icon.inset(-12.0),
            Icon::GalleryVerticalEnd,
            accent,
            1.7,
        );
        text_renderer.draw_style(
            scene,
            &target.title,
            colors.text_primary,
            transform.pre_translate((card.x0 + 78.0, card.y0 + 22.0).into()),
            Some((card.width() - 100.0) as f32),
            &theme.text(TextRole::Heading),
        );
        let scope = match (mode, extent) {
            (CaptureMode::Screenshot, CaptureExtent::FullApp) => "Full app · off-screen",
            (_, CaptureExtent::FullApp) => "App window · off-screen",
            (_, CaptureExtent::WholeStrip) => "Strip content · off-screen",
            (_, CaptureExtent::Visible) => "Off-screen",
        };
        let full = if extent == CaptureExtent::FullApp {
            target.full_app_rect(mode)
        } else {
            target.virtual_rect
        };
        text_renderer.draw_style(
            scene,
            &format!(
                "{scope} · {} × {}",
                full.width().round() as i64,
                full.height().round() as i64
            ),
            accent,
            transform.pre_translate((card.x0 + 78.0, card.y0 + 51.0).into()),
            None,
            &theme
                .text(TextRole::Caption)
                .with_weight(TextWeight::Medium),
        );
        let guidance = Self::offscreen_guidance(mode, extent);
        text_renderer.draw_style(
            scene,
            guidance,
            colors.text_dim,
            transform.pre_translate((card.x0 + 22.0, card.y0 + 94.0).into()),
            Some((card.width() - 44.0) as f32),
            &theme.text(TextRole::Caption),
        );
    }

    fn delay_button(
        scene: &mut Scene,
        t: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        seconds: u8,
        hovered: bool,
    ) {
        let colors = &theme.tokens.colors;
        let active = seconds > 0;
        let fill = if active {
            colors.intent.inverted
        } else if hovered {
            colors.lift(0.72)
        } else {
            colors.lift(0.35)
        };
        if active {
            scene.fill(
                Fill::NonZero,
                t,
                fill,
                None,
                &RoundedRect::from_rect(rect, 10.0),
            );
        } else {
            surface::card(scene, t, rect, 10.0, fill);
        }
        icons::draw_icon(
            scene,
            t,
            Rect::from_center_size(
                Point::new(
                    rect.center().x - if active { 3.0 } else { 0.0 },
                    rect.center().y,
                ),
                (16.0, 16.0),
            ),
            Icon::Timer,
            if active || hovered {
                colors.text_primary
            } else {
                colors.text_dim
            },
            1.5,
        );
        if active {
            // The count is a badge in the button's top-right corner, at the
            // floor of the type scale rather than a 7 px glyph under it, and
            // right-aligned by measurement so "3" and "10" share one edge
            // instead of two hand-picked offsets. At 1× the badge and the
            // 16 px icon (nudged 3 px left while armed) are disjoint; the
            // slot itself is a fixed 36 px like the rest of this bar, so at
            // 2× type the two meet — the bar's scaling is the shell's
            // decision (see the F5 round-two report, R7).
            let label = seconds.to_string();
            let style = theme.text(TextRole::Caption);
            let (lw, _) = text_renderer.measure_style(
                &label,
                None,
                &style.clone().with_weight(TextWeight::Semibold),
            );
            text_renderer.draw_style(
                scene,
                &label,
                colors.text_primary,
                t.pre_translate((rect.x1 - 3.0 - f64::from(lw), rect.y0 + 2.0).into()),
                None,
                &style.clone().with_weight(TextWeight::Semibold),
            );
        }
    }

    fn option_face(rect: Rect, label: &str, icon: Icon, active: bool) -> ButtonFace<'_> {
        if active {
            ButtonFace::new(rect, label).role(ButtonRole::Primary)
        } else {
            ButtonFace::new(rect, label)
        }
        .icon(icon)
    }

    fn scrim_path(&self, regions: &[Rect]) -> BezPath {
        let mut p = BezPath::new();
        add_rect(&mut p, self.rect);
        for region in regions {
            add_rect(&mut p, *region);
        }
        p
    }

    fn draw_selection_handles(scene: &mut Scene, t: Affine, theme: &Theme, r: Rect, cursor: Point) {
        let c = &theme.tokens.colors;
        for (_, point) in Self::selection_handle_points(r) {
            let hovered = Rect::from_center_size(point, (20.0, 20.0)).contains(cursor);
            let radius = if hovered { 4.75 } else { 3.5 };
            let handle = Rect::new(
                point.x - radius,
                point.y - radius,
                point.x + radius,
                point.y + radius,
            );
            scene.fill(
                Fill::NonZero,
                t,
                if hovered {
                    c.intent.inverted_hover
                } else {
                    c.text_primary
                },
                None,
                &RoundedRect::from_rect(handle, radius),
            );
            scene.stroke(
                &Stroke::new(1.5),
                t,
                c.intent.inverted,
                None,
                &RoundedRect::from_rect(handle, radius),
            );
        }
    }

    /// Paint the delayed-capture countdown after the clean source has been
    /// built. The capsule is therefore visible to the person staging the shot
    /// but can never leak into a screenshot or the first encoded frame.
    pub fn draw_countdown_indicator(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(armed) = self.armed_capture.as_ref() else {
            return;
        };
        let elapsed = armed.started_at.elapsed();
        if elapsed >= armed.duration {
            return;
        }
        let remaining = armed.duration.saturating_sub(elapsed).as_secs_f64();
        #[allow(
            clippy::cast_sign_loss,
            reason = "remaining capture delay is non-negative and clamped above one second"
        )]
        let count = remaining.ceil().max(1.0) as u64;
        let second_progress = remaining.fract();
        let pulse = if theme.reduce_motion {
            0.0
        } else {
            (elapsed.as_secs_f64() * std::f64::consts::TAU).sin() * 0.75
        };
        let center = Point::new(self.rect.center().x, self.rect.y0 + 101.0);
        let pill = Rect::from_center_size(center, (94.0 + pulse, 50.0 + pulse * 0.35));
        let colors = &theme.tokens.colors;
        let accent = match armed.intent.mode {
            CaptureMode::Recording => colors.semantic.danger,
            CaptureMode::Screenshot => colors.intent.inverted_hover,
            CaptureMode::Pip => colors.semantic.info,
        };
        surface::shadow(
            scene,
            transform,
            pill,
            pill.height() / 2.0,
            &theme.tokens.elevation.floating,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(pill, pill.height() / 2.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            accent.with_alpha_factor(0.48),
            None,
            &RoundedRect::from_rect(pill.inflate(-0.5, -0.5), pill.height() / 2.0 - 0.5),
        );
        icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(Point::new(pill.x0 + 24.0, pill.center().y), (17.0, 17.0)),
            Icon::Timer,
            accent,
            1.65,
        );
        let count_top = centered_text_top(
            text_renderer,
            &theme.text(TextRole::Heading),
            pill.center().y,
        );
        text_renderer.draw_style(
            scene,
            &count.to_string(),
            colors.text_primary,
            transform.pre_translate((pill.x0 + 48.0, count_top).into()),
            None,
            &theme.text(TextRole::Heading),
        );
        let track = Rect::new(pill.x0 + 11.0, pill.y1 - 6.0, pill.x1 - 11.0, pill.y1 - 4.0);
        scene.fill(
            Fill::NonZero,
            transform,
            crate::tokens::colors::Colors::hairline_on(colors.surfaces.raised, 26),
            None,
            &RoundedRect::from_rect(track, 1.0),
        );
        let progress = Rect::new(
            track.x0,
            track.y0,
            track.x0 + track.width() * second_progress,
            track.y1,
        );
        if progress.width() > 0.5 {
            scene.fill(
                Fill::NonZero,
                transform,
                accent,
                None,
                &RoundedRect::from_rect(progress, 1.0),
            );
        }
    }

    fn draw_recording_scope_confirmation(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
    ) {
        let alpha = self.recording_scope_confirmation_alpha();
        if alpha <= 0.01 {
            return;
        }
        let bounds = self.status_rect();
        for region in &self.recording_confirmation_regions {
            let mut visible = region.intersect(bounds);
            if visible.area() <= 1.0 {
                continue;
            }
            // Keep strokes inside the output so a full-screen source gets a
            // complete perimeter instead of four clipped half-strokes.
            if visible.x0 <= bounds.x0 + 0.5
                || visible.y0 <= bounds.y0 + 0.5
                || visible.x1 >= bounds.x1 - 0.5
                || visible.y1 >= bounds.y1 - 0.5
            {
                visible = visible.inflate(-3.0, -3.0);
            }
            let radius = visible
                .width()
                .min(visible.height())
                .mul_add(0.025, 6.0)
                .min(18.0);
            scene.stroke(
                &Stroke::new(5.0),
                transform,
                theme
                    .tokens
                    .colors
                    .semantic
                    .danger
                    .with_alpha_factor(0.07 * alpha),
                None,
                &RoundedRect::from_rect(visible, radius),
            );
            scene.stroke(
                &Stroke::new(1.25),
                transform,
                theme
                    .tokens
                    .colors
                    .semantic
                    .danger
                    .with_alpha_factor(0.66 * alpha),
                None,
                &RoundedRect::from_rect(visible, radius),
            );
        }

        // Tilo can record virtual content that is not presently on this
        // monitor. A paired edge rail + chevron confirms that the recording
        // continues there without dimming, moving, or annotating the app.
        // These marks share the short-lived scope-confirmation alpha and are
        // shell-only, just like the perimeter above.
        let accent = theme
            .tokens
            .colors
            .semantic
            .danger
            .with_alpha_factor(0.82 * alpha);
        let [left, top, right, bottom] = self.recording_continuation_edges;
        let center = bounds.center();
        let markers = [
            (
                left,
                Rect::new(
                    bounds.x0 + 3.0,
                    center.y - 20.0,
                    bounds.x0 + 6.0,
                    center.y + 20.0,
                ),
                Rect::from_center_size(Point::new(bounds.x0 + 14.0, center.y), (16.0, 16.0)),
                Icon::ChevronLeft,
            ),
            (
                top,
                Rect::new(
                    center.x - 20.0,
                    bounds.y0 + 3.0,
                    center.x + 20.0,
                    bounds.y0 + 6.0,
                ),
                Rect::from_center_size(Point::new(center.x, bounds.y0 + 14.0), (16.0, 16.0)),
                Icon::ChevronUp,
            ),
            (
                right,
                Rect::new(
                    bounds.x1 - 6.0,
                    center.y - 20.0,
                    bounds.x1 - 3.0,
                    center.y + 20.0,
                ),
                Rect::from_center_size(Point::new(bounds.x1 - 14.0, center.y), (16.0, 16.0)),
                Icon::ChevronRight,
            ),
            (
                bottom,
                Rect::new(
                    center.x - 20.0,
                    bounds.y1 - 6.0,
                    center.x + 20.0,
                    bounds.y1 - 3.0,
                ),
                Rect::from_center_size(Point::new(center.x, bounds.y1 - 14.0), (16.0, 16.0)),
                Icon::ChevronDown,
            ),
        ];
        for (visible, rail, icon_rect, icon) in markers {
            if !visible {
                continue;
            }
            scene.fill(
                Fill::NonZero,
                transform,
                accent,
                None,
                &RoundedRect::from_rect(rail, 1.5),
            );
            icons::draw_icon(scene, transform, icon_rect, icon, accent, 1.8);
        }
    }

    /// Immediate, truthful acknowledgement while the compositor creates the
    /// encoder and output. It occupies the exact launch geometry of the live
    /// recorder, so success replaces it in place instead of producing a
    /// disappear/reappear flash. It is intentionally input-transparent.
    fn draw_recording_starting(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let pill = self.recording_indicator_rects_at(0.0).0;
        let colors = &theme.tokens.colors;
        let radius = pill.height() / 2.0;
        surface::shadow(
            scene,
            transform,
            pill,
            radius,
            &theme.tokens.elevation.floating,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(pill, radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::tokens::colors::Colors::hairline_on(colors.surfaces.raised, 26),
            None,
            &RoundedRect::from_rect(pill.inflate(-0.5, -0.5), radius - 0.5),
        );

        let icon_badge =
            Rect::from_center_size(Point::new(pill.x0 + 23.0, pill.center().y), (30.0, 30.0));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.semantic.danger.with_alpha_factor(0.14),
            None,
            &RoundedRect::from_rect(icon_badge, 15.0),
        );
        icons::draw_icon(
            scene,
            transform,
            icon_badge.inset(-8.0),
            Icon::Film,
            colors.semantic.danger,
            1.55,
        );

        let scope = if self.recording_scope.is_empty() {
            "recording".to_string()
        } else if self.status_rect().width() < 1000.0 {
            self.compact_recording_scope(12)
        } else {
            self.recording_scope.clone()
        };
        let starting_top = centered_text_top(
            text_renderer,
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
            pill.center().y,
        );
        let shown = fit_label(
            text_renderer,
            &format!("Starting · {scope}"),
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
            (pill.width() - 98.0).max(40.0),
        );
        text_renderer.draw_style(
            scene,
            &shown,
            colors.text_primary,
            transform.pre_translate((pill.x0 + 45.0, starting_top).into()),
            None,
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
        );

        // A restrained travelling emphasis across three dots communicates
        // genuine setup work without a spinner dominating this short state.
        let phase = self.recording_request_started_at.map_or(0.0, |started| {
            (started.elapsed().as_secs_f64() / 0.72) % 1.0
        });
        for (index, x) in [pill.x1 - 35.0, pill.x1 - 27.0, pill.x1 - 19.0]
            .into_iter()
            .enumerate()
        {
            let target = index as f64 / 3.0;
            let raw_distance = (phase - target).abs();
            let distance = raw_distance.min(1.0 - raw_distance);
            let emphasis = (1.0 - distance / 0.34).clamp(0.0, 1.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors
                    .text_dim
                    .with_alpha_factor((0.28 + 0.62 * emphasis) as f32),
                None,
                &vello::kurbo::Circle::new((x, pill.center().y), 2.0),
            );
        }
    }

    /// Paint recording status after the clean capture source has been
    /// consumed, so shell chrome never becomes part of the video.
    pub fn draw_recording_indicator(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.open {
            return;
        }
        if self.replay_active {
            self.draw_replay_indicator(scene, transform, text_renderer, theme);
            return;
        }
        if self.screenshot_receipt.is_some() {
            self.draw_screenshot_receipt(scene, transform, text_renderer, theme);
            if let Some(menu) = self.send_menu.as_mut() {
                menu.layout_within(text_renderer, theme);
                menu.draw(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &PresenceCtx::default(),
                );
            }
            return;
        }
        if self.recording_start_pending {
            self.draw_recording_starting(scene, transform, text_renderer, theme);
            return;
        }
        if let Some(finished_at) = self.recording_finished_at {
            self.reset_docked_content_widths();
            self.draw_recording_receipt(scene, transform, text_renderer, theme, finished_at);
            return;
        }
        if !self.recording_active {
            self.reset_docked_content_widths();
            return;
        }

        // Confirm the exact source with a fading perimeter. This is an
        // overlay-only stroke: it never darkens unrelated apps and is painted
        // after the clean frame has already gone to the encoder.
        self.draw_recording_scope_confirmation(scene, transform, theme);

        let p = if theme.reduce_motion {
            1.0
        } else {
            self.recording_motion()
        };
        let compact = self.status_rect().width() < 1000.0;

        // The labels are built before the geometry so the pill can be sized
        // around what it will carry (see `remember_docked_content_widths`).
        let elapsed = self.recording_elapsed().as_secs();
        // The compact clock replacement is wider than the separate wide-bar
        // recorder, so it can carry a useful source name instead of showing
        // the awkward "Selectio…" truncation in its most spacious variant.
        let scope_limit = if compact { 12 } else { 10 };
        let compact_scope = self.compact_recording_scope(scope_limit);
        let compact_source = self
            .recording_scope
            .strip_suffix(" · app window")
            .unwrap_or(&self.recording_scope);
        let was_truncated = compact_source.chars().count() > compact_scope.chars().count();
        let normal_label = || {
            if compact_scope.is_empty() {
                crate::format::timecode(elapsed)
            } else {
                format!(
                    "{} · {}{}",
                    crate::format::timecode(elapsed),
                    compact_scope,
                    if was_truncated { "…" } else { "" }
                )
            }
        };
        let settled_label = if self.stop_pending {
            "Saving…".to_string()
        } else if self.recording_paused {
            format!("Paused · {}", crate::format::timecode(elapsed))
        } else if self
            .recording_state_changed_at
            .is_some_and(|at| at.elapsed() < Duration::from_millis(900))
        {
            format!("Resumed · {}", crate::format::timecode(elapsed))
        } else {
            normal_label()
        };
        let previous_label = self.recording_transition_from_paused.map(|was_paused| {
            if was_paused {
                format!("Paused · {}", crate::format::timecode(elapsed))
            } else {
                normal_label()
            }
        });
        {
            // Measured at the size the captions are painted with (the theme
            // step handed to the renderer once), so the widths remembered
            // below are the widths the painter lays down.
            let style = theme.text(TextRole::Caption);
            let mut label_w = text_renderer
                .measure_style(
                    &settled_label,
                    None,
                    &style.clone().with_weight(TextWeight::Semibold),
                )
                .0;
            if let Some(previous) = &previous_label {
                label_w = label_w.max(
                    text_renderer
                        .measure_style(
                            previous,
                            None,
                            &style.clone().with_weight(TextWeight::Semibold),
                        )
                        .0,
                );
            }
            let stop_label_w = text_renderer
                .measure_style(
                    "Stop",
                    None,
                    &style.clone().with_weight(TextWeight::Semibold),
                )
                .0;
            self.remember_docked_content_widths(
                text_renderer,
                theme,
                f64::from(label_w),
                f64::from(stop_label_w),
            );
        }
        let docking_label = if p < 0.66 && !self.stop_pending && !self.recording_paused {
            crate::format::timecode(elapsed)
        } else {
            settled_label
        };

        let (pill, pause, stop) = self.recording_indicator_rects_at(p);
        let colors = &theme.tokens.colors;
        let radius = pill.height() / 2.0;
        surface::shadow(
            scene,
            transform,
            pill,
            radius,
            &theme.tokens.elevation.floating,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            // Opaque on purpose: at compact widths this pill replaces the
            // center clock, so the previous clock glyphs must not ghost
            // through the recording state.
            colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(pill, radius),
        );
        let border_rect = if compact {
            pill
        } else {
            pill.inflate(-0.5, -0.5)
        };
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::tokens::colors::Colors::hairline_on(colors.surfaces.raised, 26),
            None,
            &RoundedRect::from_rect(border_rect, radius - if compact { 0.0 } else { 0.5 }),
        );

        // The resting pill stays symbolic and glanceable. Hovering its
        // status half reveals the exact source and privacy-relevant inputs
        // without permanently spending scarce top-bar width on prose.
        let status_hovered = p > 0.98
            && pill.contains(self.cursor)
            && !pause.contains(self.cursor)
            && !stop.contains(self.cursor);
        if status_hovered {
            let detail = self.recording_status_detail();
            self.draw_status_tooltip(scene, transform, text_renderer, theme, pill, &detail);
        }

        let dot_center = Point::new(pill.x0 + 18.0 + 4.0 * (1.0 - p), pill.center().y);
        let state_p = if theme.reduce_motion {
            1.0
        } else {
            self.recording_state_motion()
        };
        // Pause/resume copy and symbols hand off through the same tiny quiet
        // beat. This prevents double-exposed strokes at top-bar scale.
        let state_out_raw = (state_p / 0.46).clamp(0.0, 1.0);
        let state_out_eased = state_out_raw * state_out_raw * (3.0 - 2.0 * state_out_raw);
        let state_in_raw = ((state_p - 0.54) / 0.46).clamp(0.0, 1.0);
        let state_in_eased = state_in_raw * state_in_raw * (3.0 - 2.0 * state_in_raw);
        let state_out_alpha = 1.0 - state_out_eased;
        let state_in_alpha = state_in_eased;
        if p < 0.9 && !self.recording_paused {
            scene.fill(
                Fill::NonZero,
                transform,
                colors
                    .semantic
                    .danger
                    .with_alpha_factor((0.13 * (1.0 - p)) as f32),
                None,
                &vello::kurbo::Circle::new(dot_center, 8.0 + 4.0 * (1.0 - p)),
            );
        }
        let mut draw_state_mark = |paused: bool, alpha: f32| {
            if alpha <= 0.01 {
                return;
            }
            if paused {
                for x in [dot_center.x - 3.5, dot_center.x + 1.0] {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        colors.semantic.warning.with_alpha_factor(alpha),
                        None,
                        &RoundedRect::from_rect(
                            Rect::new(x, dot_center.y - 5.0, x + 2.5, dot_center.y + 5.0),
                            1.25,
                        ),
                    );
                }
            } else {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.semantic.danger.with_alpha_factor(alpha),
                    None,
                    &vello::kurbo::Circle::new(dot_center, 4.5),
                );
            }
        };
        if state_p < 0.999
            && let Some(was_paused) = self.recording_transition_from_paused
        {
            draw_state_mark(was_paused, state_out_alpha as f32);
            draw_state_mark(self.recording_paused, state_in_alpha as f32);
        } else {
            draw_state_mark(self.recording_paused, 1.0);
        }

        // The explanatory sentence gives way to the compact timer while the
        // pill flies. Their staggered crossfade keeps at least one semantic
        // label visible at every frame; a twelve-pixel rolling separation
        // prevents the two different type sizes from ghosting on one baseline.
        let (intro_alpha, settled_alpha) = Self::recording_label_alphas(p);
        // Two type sizes share the pill's centre line: the explanatory
        // sentence is `sm`, the docked timer `xs`. Each is centred on its own
        // measured line box, so neither needs a per-size offset.
        let intro_y = centered_text_top(
            text_renderer,
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
            pill.center().y,
        );
        let settled_y = centered_text_top(
            text_renderer,
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
            pill.center().y,
        );
        if intro_alpha > 0.01 {
            let intro_scope = if compact {
                compact_scope.clone()
            } else {
                self.recording_scope.clone()
            };
            let intro_label = if intro_scope.is_empty() {
                "Recording started".to_string()
            } else {
                format!("Recording {intro_scope}")
            };
            let intro_x = pill.x0 + 38.0;
            let shown = fit_label(
                text_renderer,
                &intro_label,
                &theme
                    .text(TextRole::Label)
                    .with_weight(TextWeight::Semibold),
                (pause.x0 - intro_x - 8.0).max(40.0),
            );
            text_renderer.draw_style(
                scene,
                &shown,
                colors.text_primary.with_alpha_factor(intro_alpha),
                transform
                    .pre_translate((intro_x, intro_y - 12.0 * f64::from(settled_alpha)).into()),
                None,
                &theme
                    .text(TextRole::Label)
                    .with_weight(TextWeight::Semibold),
            );
        }
        if settled_alpha > 0.01 {
            let scope_icon = self.recording_scope_icon();
            let continues_offscreen = self
                .recording_continuation_edges
                .into_iter()
                .any(|continues| continues);
            icons::draw_icon(
                scene,
                transform,
                Rect::from_center_size(Point::new(pill.x0 + 31.0, pill.center().y), (12.0, 12.0)),
                scope_icon,
                if continues_offscreen {
                    colors.semantic.danger.with_alpha_factor(settled_alpha)
                } else {
                    colors.text_dim.with_alpha_factor(settled_alpha)
                },
                1.45,
            );
            if state_p < 0.999
                && let Some(previous_label) = previous_label
            {
                let shown = fit_label(
                    text_renderer,
                    &previous_label,
                    &theme
                        .text(TextRole::Label)
                        .with_weight(TextWeight::Semibold),
                    (pause.x0 - pill.x0 - 51.0).max(20.0),
                );
                text_renderer.draw_style(
                    scene,
                    &shown,
                    colors
                        .text_primary
                        .with_alpha_factor(settled_alpha * state_out_alpha as f32),
                    transform
                        .pre_translate((pill.x0 + 43.0, settled_y - 3.0 * state_out_eased).into()),
                    None,
                    &theme
                        .text(TextRole::Caption)
                        .with_weight(TextWeight::Semibold),
                );
            }
            let shown = fit_label(
                text_renderer,
                &docking_label,
                &theme
                    .text(TextRole::Label)
                    .with_weight(TextWeight::Semibold),
                (pause.x0 - pill.x0 - 51.0).max(20.0),
            );
            text_renderer.draw_style(
                scene,
                &shown,
                colors
                    .text_primary
                    .with_alpha_factor(settled_alpha * state_in_alpha as f32),
                transform.pre_translate(
                    (
                        pill.x0 + 43.0,
                        settled_y
                            + 12.0 * (1.0 - f64::from(settled_alpha))
                            + 3.5 * (1.0 - state_in_eased),
                    )
                        .into(),
                ),
                None,
                &theme
                    .text(TextRole::Caption)
                    .with_weight(TextWeight::Semibold),
            );
        }

        let pause_hovered =
            pause.contains(self.cursor) && !self.pause_pending && !self.stop_pending;
        scene.fill(
            Fill::NonZero,
            transform,
            if self.pause_pending || self.stop_pending {
                colors.lift(0.35)
            } else if pause_hovered {
                colors.semantic.info.with_alpha_factor(0.22)
            } else {
                colors.lift(0.35)
            },
            None,
            &RoundedRect::from_rect(pause, pause.height() / 2.0),
        );
        if !self.stop_pending {
            if state_p < 0.999
                && let Some(was_paused) = self.recording_transition_from_paused
            {
                icons::draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(pause.center(), (12.0, 12.0)),
                    if was_paused { Icon::Play } else { Icon::Pause },
                    colors.text_dim.with_alpha_factor(state_out_alpha as f32),
                    1.7,
                );
            }
            icons::draw_icon(
                scene,
                transform,
                Rect::from_center_size(pause.center(), (12.0, 12.0)),
                if self.recording_paused {
                    Icon::Play
                } else {
                    Icon::Pause
                },
                if self.pause_pending {
                    colors
                        .text_dim
                        .with_alpha_factor(0.55 * state_in_alpha as f32)
                } else if self.recording_paused {
                    colors
                        .semantic
                        .info
                        .with_alpha_factor(state_in_alpha as f32)
                } else {
                    colors.text_dim.with_alpha_factor(state_in_alpha as f32)
                },
                1.7,
            );
        }

        let hovered = stop.contains(self.cursor) && !self.stop_pending;
        scene.fill(
            Fill::NonZero,
            transform,
            if self.stop_pending {
                colors.lift(0.35)
            } else if hovered {
                colors.semantic.danger.with_alpha_factor(0.24)
            } else {
                colors.semantic.danger.with_alpha_factor(0.14)
            },
            None,
            &RoundedRect::from_rect(stop, stop.height() / 2.0),
        );
        if !self.stop_pending {
            let square = Rect::new(
                stop.x0 + 9.0,
                stop.center().y - 3.5,
                stop.x0 + 16.0,
                stop.center().y + 3.5,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.semantic.danger,
                None,
                &RoundedRect::from_rect(square, 1.8),
            );
            let stop_top = centered_text_top(
                text_renderer,
                &theme
                    .text(TextRole::Label)
                    .with_weight(TextWeight::Semibold),
                stop.center().y,
            );
            text_renderer.draw_style(
                scene,
                "Stop",
                colors.semantic.danger,
                transform.pre_translate((stop.x0 + 22.0, stop_top).into()),
                None,
                &theme
                    .text(TextRole::Caption)
                    .with_weight(TextWeight::Semibold),
            );
        }
    }

    fn draw_recording_receipt(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        finished_at: Instant,
    ) {
        let elapsed = finished_at.elapsed();
        if self.recording_receipt_progress_override.is_none()
            && elapsed >= RECORDING_RECEIPT_DURATION
        {
            self.recording_finished_at = None;
            self.open = false;
            return;
        }
        let enter = if let Some(progress) = self.recording_receipt_progress_override {
            progress
        } else if theme.reduce_motion {
            1.0
        } else {
            let t = (elapsed.as_secs_f64() / RECORDING_RECEIPT_ENTER.as_secs_f64()).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let remaining = RECORDING_RECEIPT_DURATION.saturating_sub(elapsed);
        let exit = if self.recording_receipt_progress_override.is_some() || theme.reduce_motion {
            1.0
        } else {
            let t =
                (remaining.as_secs_f64() / RECORDING_RECEIPT_EXIT.as_secs_f64()).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let alpha = (enter * exit) as f32;
        let settled = self.recording_receipt_rect();
        let width = (settled.width() - 20.0) + 20.0 * enter;
        let lift = -4.0 * (1.0 - exit);
        let pill = Rect::from_center_size(
            Point::new(settled.center().x, settled.center().y + lift),
            (width, settled.height()),
        );
        let colors = &theme.tokens.colors;
        let radius = pill.height() / 2.0;
        let background_alpha = if self.status_rect().width() < 1000.0 {
            // This exactly covers the clock capsule. Keep its shared shell
            // opaque while receipt content fades so clock text cannot ghost
            // through the hand-back frame.
            1.0
        } else {
            alpha
        };
        surface::shadow(
            scene,
            transform,
            pill,
            radius,
            &theme
                .tokens
                .elevation
                .floating
                .with_alpha_scaled(background_alpha),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised.with_alpha_factor(background_alpha),
            None,
            &RoundedRect::from_rect(pill, radius),
        );
        let compact = self.status_rect().width() < 1000.0;
        let border_rect = if compact {
            pill
        } else {
            pill.inflate(-0.5, -0.5)
        };
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::tokens::colors::Colors::hairline_on(colors.surfaces.raised, 26)
                .with_alpha_factor(background_alpha),
            None,
            &RoundedRect::from_rect(border_rect, radius - if compact { 0.0 } else { 0.5 }),
        );

        let check =
            Rect::from_center_size(Point::new(pill.x0 + 18.0, pill.center().y), (17.0, 17.0));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.semantic.success.with_alpha_factor(0.18 * alpha),
            None,
            &vello::kurbo::Circle::new(check.center(), 8.5),
        );
        icons::draw_icon(
            scene,
            transform,
            check.inset(-3.0),
            Icon::Check,
            colors.semantic.success.with_alpha_factor(alpha),
            1.8,
        );
        let duration = crate::format::timecode(self.recording_finished_duration.as_secs());
        let saved_top = centered_text_top(
            text_renderer,
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
            pill.center().y,
        );
        let shown = fit_label(
            text_renderer,
            &format!("Saved · {duration}"),
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
            (pill.width() - 126.0).max(50.0),
        );
        text_renderer.draw_style(
            scene,
            &shown,
            colors.text_primary.with_alpha_factor(alpha),
            transform.pre_translate((pill.x0 + 32.0, saved_top).into()),
            None,
            &theme
                .text(TextRole::Caption)
                .with_weight(TextWeight::Semibold),
        );

        let (open, reveal, dismiss) = Self::recording_receipt_action_rects(pill, compact);
        for (rect, icon, accent) in [
            (open, Icon::Play, true),
            (reveal, Icon::FolderOpen, false),
            (dismiss, Icon::X, false),
        ] {
            let hovered = rect.contains(self.cursor);
            scene.fill(
                Fill::NonZero,
                transform,
                if hovered && accent {
                    colors.semantic.success.with_alpha_factor(0.20 * alpha)
                } else if hovered {
                    colors.lift(0.72).with_alpha_factor(alpha)
                } else if icon == Icon::X {
                    Color::TRANSPARENT
                } else {
                    colors.lift(0.35).with_alpha_factor(alpha)
                },
                None,
                &RoundedRect::from_rect(rect, rect.height() / 2.0),
            );
            icons::draw_icon(
                scene,
                transform,
                rect.inset(-5.0),
                icon,
                if hovered && accent {
                    colors.semantic.success.with_alpha_factor(alpha)
                } else {
                    colors.text_dim.with_alpha_factor(alpha)
                },
                if icon == Icon::X { 1.4 } else { 1.5 },
            );
        }
        if let Some((anchor, label)) = [
            (open, "Play recording"),
            (reveal, "Show in Files"),
            (dismiss, "Dismiss"),
        ]
        .into_iter()
        .find(|(rect, _)| rect.contains(self.cursor))
        {
            self.draw_status_tooltip(scene, transform, text_renderer, theme, anchor, label);
        }
    }

    fn draw_screenshot_receipt(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(receipt) = self.screenshot_receipt.clone() else {
            return;
        };
        let elapsed = receipt.shown_at.elapsed();
        // Choosing a Send target holds the receipt fully shown.
        let choosing = self.send_menu.is_some();
        let elapsed = if choosing {
            SCREENSHOT_RECEIPT_ENTER
        } else {
            elapsed
        };
        if elapsed >= SCREENSHOT_RECEIPT_DURATION && !self.screenshot_pin_pending {
            self.screenshot_receipt = None;
            self.open = false;
            return;
        }
        let p = if theme.reduce_motion {
            1.0
        } else {
            Self::screenshot_receipt_motion(&receipt)
        };
        let remaining = SCREENSHOT_RECEIPT_DURATION.saturating_sub(elapsed);
        let exit = if theme.reduce_motion {
            1.0
        } else {
            let t =
                (remaining.as_secs_f64() / SCREENSHOT_RECEIPT_EXIT.as_secs_f64()).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let alpha = exit as f32;
        let content_alpha = (((p - 0.32) / 0.68).clamp(0.0, 1.0) * exit) as f32;
        let (pill, pin, copy, reveal, dismiss) =
            self.screenshot_receipt_rects_at(p, receipt.origin);
        let colors = &theme.tokens.colors;
        let radius = 12.0 + (pill.height() / 2.0 - 12.0) * p;
        surface::shadow(
            scene,
            transform,
            pill,
            radius,
            &theme.tokens.elevation.floating.with_alpha_scaled(alpha),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised.with_alpha_factor(alpha),
            None,
            &RoundedRect::from_rect(pill, radius),
        );
        let compact = self.status_rect().width() < 1000.0;
        let border_rect = if compact {
            pill
        } else {
            pill.inflate(-0.5, -0.5)
        };
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.intent.inverted_hover.with_alpha_factor(0.30 * alpha),
            None,
            &RoundedRect::from_rect(border_rect, radius - if compact { 0.0 } else { 0.5 }),
        );

        let preview = Self::screenshot_receipt_preview_rect(p, pill, receipt.width, receipt.height);
        let preview_hovered = !receipt.copied && preview.contains(self.cursor);
        let clip = RoundedRect::from_rect(preview, 5.0);
        scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, transform, &clip);
        let columns = usize::from(receipt.preview.columns.max(1));
        let rows = usize::from(receipt.preview.rows.max(1));
        let cell_width = preview.width() / columns as f64;
        let cell_height = preview.height() / rows as f64;
        for (index, rgba) in receipt.preview.pixels.iter().enumerate() {
            let column = index % columns;
            let row = index / columns;
            if row >= rows {
                break;
            }
            let cell = Rect::new(
                preview.x0 + column as f64 * cell_width,
                preview.y0 + row as f64 * cell_height,
                preview.x0 + (column + 1) as f64 * cell_width + 0.25,
                preview.y0 + (row + 1) as f64 * cell_height + 0.25,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]),
                None,
                &cell,
            );
        }
        scene.pop_layer();
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            if preview_hovered {
                colors
                    .intent
                    .inverted_hover
                    .with_alpha_factor(0.72 * content_alpha)
            } else {
                crate::tokens::colors::Colors::hairline_on(colors.surfaces.raised, 26)
                    .with_alpha_factor(alpha)
            },
            None,
            &RoundedRect::from_rect(preview.inflate(-0.5, -0.5), 4.5),
        );
        if preview_hovered {
            let badge = Rect::from_center_size(preview.center(), (22.0, 22.0));
            scene.fill(
                Fill::NonZero,
                transform,
                colors
                    .surfaces
                    .raised
                    .with_alpha_factor(0.88 * content_alpha),
                None,
                &RoundedRect::from_rect(badge, badge.height() / 2.0),
            );
            icons::draw_icon(
                scene,
                transform,
                badge.inset(-5.0),
                Icon::Pencil,
                colors.text_primary.with_alpha_factor(content_alpha),
                1.45,
            );
        }

        let label_x = preview.x1 + 9.0;
        let action_limit = if receipt.copied {
            dismiss.x0
        } else {
            Self::screenshot_receipt_send_rect(pin).x0
        };
        let status = if receipt.copied { "Copied" } else { "Saved" };
        let dimensions = format!("{} × {}", receipt.width, receipt.height);
        let (status_width, status_height) = text_renderer.measure_style(
            status,
            None,
            &theme
                .text(TextRole::Caption)
                .with_weight(TextWeight::Semibold),
        );
        text_renderer.draw_style(
            scene,
            status,
            colors.text_primary.with_alpha_factor(content_alpha),
            transform
                .pre_translate((label_x, pill.center().y - f64::from(status_height) / 2.0).into()),
            None,
            &theme
                .text(TextRole::Caption)
                .with_weight(TextWeight::Semibold),
        );
        let dimensions_x = label_x + f64::from(status_width) + 8.0;
        let dimensions_width = (action_limit - dimensions_x - 5.0).max(0.0);
        if dimensions_width >= 24.0 {
            let (_, dimensions_height) = text_renderer.measure_style(
                &dimensions,
                Some(dimensions_width as f32),
                &theme.text(TextRole::Caption),
            );
            text_renderer.draw_style(
                scene,
                &dimensions,
                colors.text_dim.with_alpha_factor(content_alpha),
                transform.pre_translate(
                    (
                        dimensions_x,
                        pill.center().y - f64::from(dimensions_height) / 2.0,
                    )
                        .into(),
                ),
                Some(dimensions_width as f32),
                &theme.text(TextRole::Caption),
            );
        }

        if !receipt.copied {
            let send = Self::screenshot_receipt_send_rect(pin);
            let send_active = send.contains(self.cursor) || choosing;
            scene.fill(
                Fill::NonZero,
                transform,
                if send_active {
                    colors
                        .intent
                        .inverted_hover
                        .with_alpha_factor(0.20 * content_alpha)
                } else {
                    colors.lift(0.35).with_alpha_factor(content_alpha)
                },
                None,
                &RoundedRect::from_rect(send, send.height() / 2.0),
            );
            icons::draw_icon(
                scene,
                transform,
                send.inset(-6.0),
                Icon::Send,
                colors.text_dim.with_alpha_factor(content_alpha),
                1.45,
            );
            let pin_hovered = pin.contains(self.cursor);
            scene.fill(
                Fill::NonZero,
                transform,
                if self.screenshot_pin_pending {
                    colors
                        .intent
                        .inverted_hover
                        .with_alpha_factor(0.28 * content_alpha)
                } else if pin_hovered {
                    colors
                        .intent
                        .inverted_hover
                        .with_alpha_factor(0.20 * content_alpha)
                } else {
                    colors.lift(0.35).with_alpha_factor(content_alpha)
                },
                None,
                &RoundedRect::from_rect(pin, pin.height() / 2.0),
            );
            icons::draw_icon(
                scene,
                transform,
                pin.inset(-6.0),
                Icon::Pin,
                if self.screenshot_pin_pending {
                    colors
                        .intent
                        .inverted_hover
                        .with_alpha_factor(content_alpha)
                } else {
                    colors.text_dim.with_alpha_factor(content_alpha)
                },
                1.45,
            );
            let copy_hovered = copy.contains(self.cursor);
            scene.fill(
                Fill::NonZero,
                transform,
                if copy_hovered {
                    colors
                        .intent
                        .inverted_hover
                        .with_alpha_factor(0.20 * content_alpha)
                } else {
                    colors.lift(0.35).with_alpha_factor(content_alpha)
                },
                None,
                &RoundedRect::from_rect(copy, copy.height() / 2.0),
            );
            icons::draw_icon(
                scene,
                transform,
                copy.inset(-6.0),
                Icon::Copy,
                colors.text_dim.with_alpha_factor(content_alpha),
                1.45,
            );
            let hovered = reveal.contains(self.cursor);
            scene.fill(
                Fill::NonZero,
                transform,
                if hovered {
                    colors
                        .intent
                        .inverted_hover
                        .with_alpha_factor(0.20 * content_alpha)
                } else {
                    colors.lift(0.35).with_alpha_factor(content_alpha)
                },
                None,
                &RoundedRect::from_rect(reveal, reveal.height() / 2.0),
            );
            icons::draw_icon(
                scene,
                transform,
                reveal.inset(-6.0),
                Icon::FolderOpen,
                colors.text_dim.with_alpha_factor(content_alpha),
                1.45,
            );
        }
        let dismiss_hovered = dismiss.contains(self.cursor);
        scene.fill(
            Fill::NonZero,
            transform,
            if dismiss_hovered {
                colors.lift(0.72).with_alpha_factor(content_alpha)
            } else {
                Color::TRANSPARENT
            },
            None,
            &RoundedRect::from_rect(dismiss, dismiss.height() / 2.0),
        );
        icons::draw_icon(
            scene,
            transform,
            dismiss.inset(-6.0),
            Icon::X,
            colors.text_dim.with_alpha_factor(content_alpha),
            1.4,
        );
    }

    fn draw_replay_indicator(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let p = if theme.reduce_motion {
            1.0
        } else {
            self.replay_motion()
        };
        let (pill, save, stop) = self.replay_indicator_rects_at(p);
        let colors = &theme.tokens.colors;
        let radius = pill.height() / 2.0;
        surface::shadow(
            scene,
            transform,
            pill,
            radius,
            &theme.tokens.elevation.floating,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(pill, radius),
        );
        let compact = self.status_rect().width() < 1000.0;
        let border_rect = if compact {
            pill
        } else {
            pill.inflate(-0.5, -0.5)
        };
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.semantic.info.with_alpha_factor(0.30),
            None,
            &RoundedRect::from_rect(border_rect, radius - if compact { 0.0 } else { 0.5 }),
        );

        let saved = self.replay_saved_actions_visible();
        let lead =
            Rect::from_center_size(Point::new(pill.x0 + 18.0, pill.center().y), (17.0, 17.0));
        scene.fill(
            Fill::NonZero,
            transform,
            if saved {
                colors.semantic.success.with_alpha_factor(0.18)
            } else {
                colors.semantic.info.with_alpha_factor(0.16)
            },
            None,
            &vello::kurbo::Circle::new(lead.center(), 8.5),
        );
        icons::draw_icon(
            scene,
            transform,
            lead.inset(-3.0),
            if saved { Icon::Check } else { Icon::History },
            if saved {
                colors.semantic.success
            } else {
                colors.semantic.info
            },
            1.65,
        );
        let label = self.replay_status_label(saved);

        let mut audio_x = save.x0 - 8.0;
        for (included, icon) in [
            (self.replay_audio.includes_microphone(), Icon::Mic),
            (self.replay_audio.includes_system(), Icon::Volume2),
        ] {
            if included {
                audio_x -= 16.0;
                icons::draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(
                        Point::new(audio_x + 6.0, pill.center().y),
                        (11.0, 11.0),
                    ),
                    icon,
                    colors.text_dim,
                    1.35,
                );
            }
        }
        let replay_top = centered_text_top(
            text_renderer,
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
            pill.center().y,
        );
        let shown = fit_label(
            text_renderer,
            &label,
            &theme
                .text(TextRole::Label)
                .with_weight(TextWeight::Semibold),
            (audio_x - pill.x0 - 38.0).max(24.0),
        );
        text_renderer.draw_style(
            scene,
            &shown,
            colors.text_primary,
            transform.pre_translate((pill.x0 + 32.0, replay_top).into()),
            None,
            &theme
                .text(TextRole::Caption)
                .with_weight(TextWeight::Semibold),
        );

        let status_hovered = p > 0.98
            && pill.contains(self.cursor)
            && !save.contains(self.cursor)
            && !stop.contains(self.cursor);
        if status_hovered {
            let detail = self.replay_status_detail();
            self.draw_status_tooltip(scene, transform, text_renderer, theme, pill, &detail);
        }

        if saved {
            let (play, reveal) = self.replay_saved_action_rects();
            for (rect, icon, hovered) in [
                (play, Icon::Play, play.contains(self.cursor)),
                (reveal, Icon::FolderOpen, reveal.contains(self.cursor)),
            ] {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if hovered {
                        colors.semantic.success.with_alpha_factor(0.22)
                    } else {
                        colors.lift(0.35)
                    },
                    None,
                    &RoundedRect::from_rect(rect, rect.height() / 2.0),
                );
                icons::draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(rect.center(), (12.0, 12.0)),
                    icon,
                    if hovered {
                        colors.semantic.success
                    } else {
                        colors.text_primary
                    },
                    1.5,
                );
            }
        } else {
            let save_ready = self.replay_buffered_seconds > 0;
            let save_hovered = save_ready && save.contains(self.cursor);
            scene.fill(
                Fill::NonZero,
                transform,
                if save_hovered {
                    colors.semantic.info.with_alpha_factor(0.22)
                } else {
                    colors.lift(0.35)
                },
                None,
                &RoundedRect::from_rect(save, save.height() / 2.0),
            );
            icons::draw_icon(
                scene,
                transform,
                Rect::from_center_size(Point::new(save.x0 + 14.0, save.center().y), (12.0, 12.0)),
                Icon::Download,
                if save_ready {
                    colors.semantic.info
                } else {
                    colors.text_dim
                },
                1.5,
            );
            let save_top = centered_text_top(
                text_renderer,
                &theme
                    .text(TextRole::Label)
                    .with_weight(TextWeight::Semibold),
                save.center().y,
            );
            text_renderer.draw_style(
                scene,
                "Save",
                if save_ready {
                    colors.text_primary
                } else {
                    colors.text_dim
                },
                transform.pre_translate((save.x0 + 25.0, save_top).into()),
                None,
                &theme
                    .text(TextRole::Caption)
                    .with_weight(TextWeight::Semibold),
            );
        }

        let stop_hovered = stop.contains(self.cursor);
        scene.fill(
            Fill::NonZero,
            transform,
            if stop_hovered {
                colors.semantic.danger.with_alpha_factor(0.22)
            } else {
                colors.lift(0.35)
            },
            None,
            &RoundedRect::from_rect(stop, stop.height() / 2.0),
        );
        // This ends the rolling capture; a close × would imply that only the
        // indicator is hidden. A filled stop square makes the privacy effect
        // explicit even before hover.
        scene.fill(
            Fill::NonZero,
            transform,
            if stop_hovered {
                colors.semantic.danger
            } else {
                colors.text_dim
            },
            None,
            &RoundedRect::from_rect(Rect::from_center_size(stop.center(), (8.0, 8.0)), 2.0),
        );
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
    fn apply_layout(&mut self, t: &TaffyTree<()>, n: NodeId, o: Point) {
        if let Ok(l) = t.layout(n) {
            self.rect = Rect::new(
                o.x + f64::from(l.location.x),
                o.y + f64::from(l.location.y),
                o.x + f64::from(l.location.x + l.size.width),
                o.y + f64::from(l.location.y + l.size.height),
            )
        }
    }
    fn draw(
        &mut self,
        s: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _: &PresenceCtx,
    ) {
        if !self.open
            || self.recording_start_pending
            || self.recording_active
            || self.replay_active
            || self.screenshot_receipt.is_some()
            || self.recording_finished_at.is_some()
            || self.armed_capture.is_some()
        {
            return;
        }
        let progress = if theme.reduce_motion {
            1.0
        } else {
            self.opened_at.map_or(1.0, |at| {
                (at.elapsed().as_secs_f32() / OPEN_DURATION.as_secs_f32()).clamp(0.0, 1.0)
            })
        };
        let eased = 1.0 - (1.0 - progress).powi(3);
        let top_t = t.pre_translate((0.0, -10.0 * f64::from(1.0 - eased)).into());
        let bottom_t = t.pre_translate((0.0, 12.0 * f64::from(1.0 - eased)).into());
        let c = &theme.tokens.colors;
        let selection_color = self.selection_color(theme);
        let active_control_fill = match self.mode {
            CaptureMode::Screenshot => c.intent.inverted,
            CaptureMode::Recording => c.semantic.danger.with_alpha_factor(0.28),
            CaptureMode::Pip => c.semantic.info.with_alpha_factor(0.32),
        };
        s.push_layer(Fill::NonZero, BlendMode::default(), progress, t, &self.rect);
        let regions = self.active_regions();
        let mut base_scrim_alpha = match self.mode {
            CaptureMode::Screenshot => 0.62,
            CaptureMode::Recording => 0.56,
            CaptureMode::Pip => 0.48,
        };
        if matches!(self.source, SourceMode::Region | SourceMode::Multi) && regions.is_empty() {
            // Before the first drag the desktop is the picker: keep enough
            // contrast for the shell controls without making the content the
            // user is trying to locate illegible. Once an aperture exists the
            // stronger outside scrim returns and visually locks the source.
            base_scrim_alpha *= 0.64;
        }
        // A fully off-screen app—or a continuation sliver too narrow to label
        //—has no useful aperture in the scrim. Keep the desktop
        // contextual; the centered source card and picker chip carry focus.
        let selected_app_needs_card = self.source == SourceMode::Window
            && self.selected_candidate().is_some_and(|target| {
                !Self::target_has_useful_aperture(target, self.status_rect())
            });
        let scrim_alpha = if selected_app_needs_card {
            base_scrim_alpha * 0.30
        } else {
            base_scrim_alpha
        };
        s.fill(
            Fill::EvenOdd,
            t,
            c.surfaces.overlay.with_alpha_factor(scrim_alpha),
            None,
            &self.scrim_path(&regions),
        );
        if self.source == SourceMode::Window {
            for target in &self.candidates {
                let rect = target.visible_rect.intersect(self.selection_bounds());
                if rect.area() <= 0.0 {
                    continue;
                }
                let selected = self.selected_target.as_ref() == Some(&target.id);
                let hovered = rect.contains(self.cursor);
                if selected || hovered {
                    s.fill(
                        Fill::NonZero,
                        t,
                        selection_color.with_alpha_factor(if selected { 0.10 } else { 0.05 }),
                        None,
                        &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.tile)),
                    );
                }
                let stroke_width = if selected {
                    3.0
                } else if hovered {
                    2.0
                } else {
                    1.0
                };
                let half = stroke_width / 2.0;
                s.stroke(
                    &Stroke::new(stroke_width),
                    t,
                    if selected {
                        selection_color
                    } else if hovered {
                        selection_color.with_alpha_factor(0.70)
                    } else {
                        c.text_dim.with_alpha_factor(0.42)
                    },
                    None,
                    &RoundedRect::from_rect(
                        rect.inflate(half, half),
                        f64::from(theme.tokens.radii.tile) + half,
                    ),
                );
                if !selected || Self::target_has_useful_aperture(target, self.status_rect()) {
                    self.target_badge(&mut PaintCtx::new(s, t, tr, theme), target, rect, selected);
                }
                if selected && self.extent == CaptureExtent::FullApp {
                    Self::draw_continuation_markers(
                        s,
                        t,
                        self.status_rect(),
                        Self::virtual_continuation_edges(target, self.mode, self.status_rect()),
                        selection_color.with_alpha_factor(0.88),
                    );
                }
            }
            if let Some(target) = self.selected_candidate().cloned()
                && !Self::target_has_useful_aperture(&target, self.status_rect())
            {
                self.draw_offscreen_target(&mut PaintCtx::new(s, t, tr, theme), &target);
                if self.extent == CaptureExtent::FullApp {
                    Self::draw_continuation_markers(
                        s,
                        t,
                        self.status_rect(),
                        Self::virtual_continuation_edges(&target, self.mode, self.status_rect()),
                        selection_color.with_alpha_factor(0.88),
                    );
                }
            }
        }
        if self.source != SourceMode::Window {
            self.region_remove_rects.clear();
            for (i, r) in regions.iter().copied().enumerate() {
                s.fill(
                    Fill::NonZero,
                    t,
                    selection_color.with_alpha_factor(0.10),
                    None,
                    &r,
                );
                s.stroke(
                    &Stroke::new(2.0),
                    t,
                    selection_color,
                    None,
                    &RoundedRect::from_rect(r, 8.0),
                );
                if self.mode == CaptureMode::Screenshot && r.width() > 180.0 && r.height() > 140.0 {
                    for x in [r.x0 + r.width() / 3.0, r.x0 + r.width() * 2.0 / 3.0] {
                        s.stroke(
                            &Stroke::new(0.7),
                            t,
                            c.text_primary.with_alpha_factor(0.24),
                            None,
                            &vello::kurbo::Line::new((x, r.y0), (x, r.y1)),
                        );
                    }
                    for y in [r.y0 + r.height() / 3.0, r.y0 + r.height() * 2.0 / 3.0] {
                        s.stroke(
                            &Stroke::new(0.7),
                            t,
                            c.text_primary.with_alpha_factor(0.24),
                            None,
                            &vello::kurbo::Line::new((r.x0, y), (r.x1, y)),
                        );
                    }
                }
                Self::draw_selection_handles(s, t, theme, r, self.cursor);
                let (live, ordinal, dimensions, chrome) =
                    self.region_chrome(r, i, &regions, theme, tr);
                let mut metadata_x = chrome.metadata.x0;
                for label in [
                    live.as_deref(),
                    ordinal.as_deref(),
                    Some(dimensions.as_str()),
                ]
                .into_iter()
                .flatten()
                {
                    let width = ChipFace::intrinsic_width(label, ChipLead::None, false, tr, theme);
                    let rect = Rect::new(
                        metadata_x,
                        chrome.metadata.y0,
                        metadata_x + width,
                        chrome.metadata.y1,
                    );
                    ChipFace::new(rect, label)
                        .style(ChipStyle::Tinted(Tone::Presence(selection_color)))
                        .paint(s, t, tr, theme);
                    metadata_x = rect.x1 + 4.0;
                }
                let hovered = chrome.remove.contains(self.cursor);
                let face = if hovered {
                    ButtonFace::new(chrome.remove, "")
                        .role(ButtonRole::Destructive)
                        .icon(Icon::X)
                        .accessible_name("Remove selection")
                } else {
                    ButtonFace::new(chrome.remove, "")
                        .icon(Icon::X)
                        .accessible_name("Remove selection")
                }
                .hovered(hovered);
                Button::paint(s, t, tr, theme, &face);
                self.region_remove_rects.push(chrome.remove);
            }
        }
        let top = self.top_bar();
        for group in [self.mode_group_rect(), self.source_group_rect()] {
            surface::shadow(s, top_t, group, 14.0, &theme.tokens.elevation.floating);
            // Raised group under an edge derived from it (HIG §9.5): over the
            // scrim the fill alone had no closed edge on the light preset.
            surface::card(s, top_t, group, 14.0, c.surfaces.raised);
        }
        let [m0, m1, m2] = self.mode_rects();
        Self::symbol_button(
            s,
            top_t,
            theme,
            m0,
            self.mode == CaptureMode::Screenshot,
            m0.contains(self.cursor),
            Icon::Camera,
            c.intent.inverted,
        );
        Self::symbol_button(
            s,
            top_t,
            theme,
            m1,
            self.mode == CaptureMode::Recording,
            m1.contains(self.cursor),
            Icon::Film,
            c.semantic.danger.with_alpha_factor(0.28),
        );
        Self::symbol_button(
            s,
            top_t,
            theme,
            m2,
            self.mode == CaptureMode::Pip,
            m2.contains(self.cursor),
            Icon::PictureInPicture2,
            c.semantic.info.with_alpha_factor(0.32),
        );
        if self.mode == CaptureMode::Recording {
            s.fill(
                Fill::NonZero,
                top_t,
                c.semantic.danger,
                None,
                &vello::kurbo::Circle::new((m1.x0 + 9.0, m1.y0 + 9.0), 3.0),
            );
        }
        for ((r, m), ic) in self
            .source_rects()
            .into_iter()
            .zip([
                SourceMode::Region,
                SourceMode::Window,
                SourceMode::Display,
                SourceMode::Multi,
            ])
            .zip([
                Icon::Scissors,
                Icon::GalleryVerticalEnd,
                Icon::Monitor,
                Icon::Layers,
            ])
        {
            Self::symbol_button(
                s,
                top_t,
                theme,
                r,
                self.source == m,
                r.contains(self.cursor),
                ic,
                active_control_fill,
            )
        }
        let mut context = self.default_picker_context();
        if let Some((_, label)) = self
            .mode_rects()
            .into_iter()
            .zip([
                "Screenshot · Super+Shift+S",
                "Screen recording",
                "Picture in Picture · Super+Shift+P",
            ])
            .find(|(rect, _)| rect.contains(self.cursor))
        {
            context = label.to_string();
        } else if let Some((_, label)) = self
            .source_rects()
            .into_iter()
            .zip([
                "Select an area",
                "Choose an app",
                "Whole screen",
                "Combine areas",
            ])
            .find(|(rect, _)| rect.contains(self.cursor))
        {
            context = label.to_string();
        } else if self.mode != CaptureMode::Pip && self.pointer_rect().contains(self.cursor) {
            context = if self.include_pointer {
                "Pointer included"
            } else {
                "Include the pointer"
            }
            .to_string();
        } else if self.system_audio_available && self.system_audio_rect().contains(self.cursor) {
            context = if self.audio.includes_system() {
                "Desktop sound included"
            } else {
                "Include desktop sound"
            }
            .to_string();
        } else if self.microphone_available && self.microphone_rect().contains(self.cursor) {
            context = if self.audio.includes_microphone() {
                "Microphone included"
            } else {
                "Include the microphone"
            }
            .to_string();
        } else if self.mode != CaptureMode::Pip && self.delay_rect().contains(self.cursor) {
            context = match self.delay_seconds {
                0 => "Start immediately",
                3 => "Start after 3 seconds",
                _ => "Start after 10 seconds",
            }
            .to_string();
        } else if self.mode == CaptureMode::Recording
            && self.action_rects()[1].contains(self.cursor)
        {
            context = "Keep the most recent 30 seconds · Super+Shift+R".to_string();
        }
        if self.source == SourceMode::Window && !self.candidates.is_empty() {
            for (target, rect) in self.candidates.iter().zip(self.candidate_picker_rects()) {
                let selected = self.selected_target.as_ref() == Some(&target.id);
                let hovered = rect.contains(self.cursor);
                s.fill(
                    Fill::NonZero,
                    top_t,
                    if selected {
                        selection_color
                    } else if hovered {
                        c.lift(0.72)
                    } else {
                        c.surfaces.raised
                    },
                    None,
                    &RoundedRect::from_rect(rect, 12.0),
                );
                icons::draw_icon(
                    s,
                    top_t,
                    Rect::new(
                        rect.x0 + 10.0,
                        rect.y0 + 8.0,
                        rect.x0 + 26.0,
                        rect.y0 + 24.0,
                    ),
                    Icon::GalleryVerticalEnd,
                    if selected || hovered {
                        c.text_primary
                    } else {
                        c.text_dim
                    },
                    1.4,
                );
                let suffix = if target.visible_rect.area() <= 0.0 {
                    " · off-screen"
                } else {
                    ""
                };
                let title_top =
                    centered_text_top(tr, &theme.text(TextRole::Label), rect.center().y);
                let shown = fit_label(
                    tr,
                    &format!("{}{suffix}", target.title),
                    &theme.text(TextRole::Label),
                    rect.width() - 40.0,
                );
                tr.draw_style(
                    s,
                    &shown,
                    if selected || hovered {
                        c.text_primary
                    } else {
                        c.text_dim
                    },
                    top_t.pre_translate((rect.x0 + 32.0, title_top).into()),
                    None,
                    &theme
                        .text(TextRole::Caption)
                        .with_weight(TextWeight::Medium),
                );
            }
        } else {
            let (context_w, _) = tr.measure_style(
                &context,
                None,
                &theme
                    .text(TextRole::Caption)
                    .with_weight(TextWeight::Medium),
            );
            let bottom = self.bottom_bar();
            let context_rect = Rect::new(
                top.center().x - f64::from(context_w) / 2.0 - 14.0,
                bottom.y0 - 44.0,
                top.center().x + f64::from(context_w) / 2.0 + 14.0,
                bottom.y0 - 16.0,
            );
            s.fill(
                Fill::NonZero,
                top_t,
                c.surfaces.raised.with_alpha_factor(0.92),
                None,
                &RoundedRect::from_rect(context_rect, 14.0),
            );
            let context_top =
                centered_text_top(tr, &theme.text(TextRole::Label), context_rect.center().y);
            tr.draw_style(
                s,
                &context,
                c.text_dim,
                top_t.pre_translate((context_rect.x0 + 14.0, context_top).into()),
                None,
                &theme
                    .text(TextRole::Caption)
                    .with_weight(TextWeight::Medium),
            );
        }
        let bottom = self.bottom_bar();
        surface::shadow(s, bottom_t, bottom, 14.0, &theme.tokens.elevation.floating);
        // Raised bar under an edge derived from it (HIG §9.5).
        surface::card(s, bottom_t, bottom, 14.0, c.surfaces.raised);
        if matches!(self.source, SourceMode::Window | SourceMode::Multi) {
            let option_labels = if self.source == SourceMode::Multi {
                ["Auto", "Grid", "Stack"]
            } else {
                ["Selection", "App", "Strip"]
            };
            let option_icons = if self.source == SourceMode::Multi {
                [Icon::Sparkles, Icon::LayoutGrid, Icon::Layers]
            } else {
                [Icon::Scissors, Icon::GalleryVerticalEnd, Icon::Layers]
            };
            for (i, ((r, label), icon)) in self
                .option_rects()
                .into_iter()
                .zip(option_labels)
                .zip(option_icons)
                .enumerate()
            {
                let active = if self.source == SourceMode::Multi {
                    self.pip_layout == [PipLayout::Auto, PipLayout::Grid, PipLayout::Stack][i]
                } else {
                    self.extent
                        == [
                            CaptureExtent::Visible,
                            CaptureExtent::FullApp,
                            CaptureExtent::WholeStrip,
                        ][i]
                };
                let face =
                    Self::option_face(r, label, icon, active).hovered(r.contains(self.cursor));
                Button::paint(s, bottom_t, tr, theme, &face);
            }
        }
        let pointer = self.pointer_rect();
        if self.mode != CaptureMode::Pip {
            Self::toggle_button(
                s,
                bottom_t,
                tr,
                theme,
                pointer,
                self.include_pointer,
                pointer.contains(self.cursor),
                Icon::MousePointer2,
            );
        }
        if self.system_audio_available {
            Self::toggle_button(
                s,
                bottom_t,
                tr,
                theme,
                self.system_audio_rect(),
                self.audio.includes_system(),
                self.system_audio_rect().contains(self.cursor),
                Icon::Volume2,
            );
        }
        if self.microphone_available {
            Self::toggle_button(
                s,
                bottom_t,
                tr,
                theme,
                self.microphone_rect(),
                self.audio.includes_microphone(),
                self.microphone_rect().contains(self.cursor),
                Icon::Mic,
            );
        }
        if self.mode != CaptureMode::Pip {
            Self::delay_button(
                s,
                bottom_t,
                tr,
                theme,
                self.delay_rect(),
                self.delay_seconds,
                self.delay_rect().contains(self.cursor),
            );
        }
        let action_icons = if self.mode == CaptureMode::Recording {
            [Icon::Copy, Icon::History, Icon::Download]
        } else {
            [Icon::Copy, Icon::PictureInPicture2, Icon::Download]
        };
        for (i, (r, icon)) in self
            .action_rects()
            .into_iter()
            .zip(action_icons)
            .enumerate()
        {
            if r.area() <= 0.0 {
                continue;
            }
            let active = self.has_capture_source()
                || (self.mode == CaptureMode::Recording && self.recording_active && i == 2);
            let hovered = active && r.contains(self.cursor);
            let emphasized = match self.mode {
                CaptureMode::Screenshot | CaptureMode::Recording => i == 2,
                CaptureMode::Pip => i == 1,
            };
            let danger_action = self.mode == CaptureMode::Recording && i == 2;
            let labeled = emphasized || (self.mode == CaptureMode::Recording && i == 1);
            let label = if labeled {
                match (self.mode, i) {
                    (CaptureMode::Recording, 1) => "Last 30s",
                    (CaptureMode::Screenshot, _) => "Save",
                    (CaptureMode::Recording, _) if self.recording_active => "Stop",
                    (CaptureMode::Recording, _) => "Record",
                    (CaptureMode::Pip, _) => "Create PiP",
                }
            } else {
                ""
            };
            let face = if danger_action && emphasized {
                ButtonFace::new(r, label).role(ButtonRole::Destructive)
            } else if emphasized {
                ButtonFace::new(r, label).role(ButtonRole::Primary)
            } else {
                ButtonFace::new(r, label)
            }
            .icon(icon)
            .hovered(hovered)
            .disabled(!active);
            Button::paint(s, bottom_t, tr, theme, &face);
        }
        s.pop_layer()
    }
    fn handle_event(&mut self, e: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let state = &mut *ctx.state;
        if !self.open {
            return;
        }
        if self.armed_capture.is_some() {
            if matches!(
                e,
                WindowEvent::KeyboardInput {
                    event: KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                    ..
                }
            ) {
                self.close();
                *ctx.handled = true;
            }
            return;
        }
        if self.recording_start_pending {
            // Encoder setup status is intentionally non-modal and has no
            // speculative cancel control. Let every event continue to the
            // application beneath it until success or failure is confirmed.
            return;
        }
        if self.replay_active {
            match e {
                WindowEvent::CursorMoved { position, .. } => {
                    self.cursor =
                        Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                    *ctx.handled = self.replay_indicator_rects().0.contains(self.cursor);
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } if self.replay_indicator_rects().0.contains(self.cursor) => {
                    *ctx.handled = true;
                    self.click(self.cursor);
                }
                _ => {}
            }
            return;
        }
        if self.screenshot_receipt.is_some() {
            if let WindowEvent::CursorMoved { position, .. } = e {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            if self.send_menu.is_some() {
                *ctx.handled = self.route_send_menu_event(
                    e,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    state,
                    ctx.path,
                );
                return;
            }
            match e {
                WindowEvent::CursorMoved { .. } => {
                    *ctx.handled = self.screenshot_receipt_rects().0.contains(self.cursor);
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } if self.screenshot_receipt_rects().0.contains(self.cursor) => {
                    *ctx.handled = true;
                    self.click(self.cursor);
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
                    self.screenshot_receipt = None;
                    self.open = false;
                    *ctx.handled = true;
                }
                _ => {}
            }
            return;
        }
        if self.recording_finished_at.is_some() {
            match e {
                WindowEvent::CursorMoved { position, .. } => {
                    self.cursor =
                        Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                    *ctx.handled = self.recording_receipt_rect().contains(self.cursor);
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } if self.recording_receipt_rect().contains(self.cursor) => {
                    *ctx.handled = true;
                    self.click(self.cursor);
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
                    self.recording_finished_at = None;
                    self.open = false;
                    *ctx.handled = true;
                }
                _ => {}
            }
            return;
        }
        if self.recording_active {
            match e {
                WindowEvent::CursorMoved { position, .. } => {
                    self.cursor =
                        Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                    *ctx.handled = self.recording_indicator_rects().0.contains(self.cursor);
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                } if self.recording_indicator_rects().0.contains(self.cursor) => {
                    *ctx.handled = true;
                    if *state == ElementState::Pressed {
                        self.click(self.cursor);
                    }
                }
                _ => {}
            }
            return;
        }
        *ctx.handled = true;
        match e {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if let Some(d) = self.drag.as_mut() {
                    d.set_current(self.cursor)
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.click_with_additive(self.cursor, ctx.modifiers.state().shift_key()),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.finish_region_drag(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.drag.take().is_none() {
                    self.close();
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key:
                            Key::Named(
                                key @ (NamedKey::ArrowLeft
                                | NamedKey::ArrowRight
                                | NamedKey::ArrowUp
                                | NamedKey::ArrowDown),
                            ),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if !self.navigate_picker(*key) {
                    let amount = if ctx.modifiers.state().shift_key() {
                        10.0
                    } else {
                        1.0
                    };
                    self.nudge_region(*key, amount);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Backspace | NamedKey::Delete),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.source == SourceMode::Multi => {
                self.regions.pop();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Enter),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.commit(if self.mode == CaptureMode::Pip {
                CaptureDestination::FloatingWindow
            } else {
                CaptureDestination::File
            }),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Character(ch),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match ch.as_str().to_ascii_lowercase().as_str() {
                "1" => self.set_source_mode(SourceMode::Region),
                "2" => self.set_source_mode(SourceMode::Window),
                "3" => self.set_source_mode(SourceMode::Display),
                "4" => self.set_source_mode(SourceMode::Multi),
                "s" if !self.recording_active => self.set_mode(CaptureMode::Screenshot),
                "r" if !self.recording_active => self.set_mode(CaptureMode::Recording),
                "p" if !self.recording_active => self.set_mode(CaptureMode::Pip),
                "t" if self.mode != CaptureMode::Pip => self.cycle_delay(),
                "c" if self.mode == CaptureMode::Screenshot => {
                    self.commit(CaptureDestination::Clipboard)
                }
                _ => {}
            },
            _ => {}
        }
    }
    fn name(&self) -> &'static str {
        "CaptureStudio"
    }
    fn test_id(&self) -> Option<&str> {
        Some("shell.capture-studio")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn modal_capture(&self) -> Option<Rect> {
        (self.open
            && !self.recording_start_pending
            && !self.recording_active
            && !self.replay_active
            && self.screenshot_receipt.is_none()
            && self.recording_finished_at.is_none()
            && self.armed_capture.is_none())
        .then_some(self.rect)
    }
    fn needs_animation_frame(&self) -> bool {
        self.recording_start_pending
            || self.recording_active
            || self.replay_active
            || self
                .screenshot_receipt
                .as_ref()
                .is_some_and(|receipt| receipt.shown_at.elapsed() < SCREENSHOT_RECEIPT_DURATION)
            || self.recording_finished_at.is_some_and(|at| {
                self.recording_receipt_progress_override.is_none()
                    && at.elapsed() < RECORDING_RECEIPT_DURATION
            })
            || self.armed_capture.is_some()
            || (self.open
                && self
                    .opened_at
                    .is_some_and(|at| at.elapsed() < OPEN_DURATION))
    }
    fn test_rects(&self) -> Vec<(String, Rect)> {
        if !self.open {
            return Vec::new();
        }
        if self.recording_start_pending {
            return Vec::new();
        }
        if self.replay_active {
            let (pill, save, stop) = self.replay_indicator_rects();
            let mut rects = vec![("capture.replay-status".into(), pill)];
            if self.replay_saved_actions_visible() {
                let (play, reveal) = self.replay_saved_action_rects();
                rects.push(("capture.play-replay".into(), play));
                rects.push(("capture.show-replay".into(), reveal));
            } else {
                rects.push(("capture.save-replay".into(), save));
            }
            rects.push(("capture.stop-replay".into(), stop));
            return rects;
        }
        if let Some(receipt) = self.screenshot_receipt.as_ref() {
            let (pill, pin, copy, reveal, dismiss) = self.screenshot_receipt_rects();
            let mut rects = Vec::with_capacity(5);
            if !receipt.copied {
                rects.push((
                    "capture.open-screenshot".into(),
                    Self::screenshot_receipt_preview_rect(
                        Self::screenshot_receipt_motion(receipt),
                        pill,
                        receipt.width,
                        receipt.height,
                    ),
                ));
                rects.push((
                    "capture.send-screenshot".into(),
                    Self::screenshot_receipt_send_rect(pin),
                ));
                rects.push(("capture.pin-screenshot".into(), pin));
                rects.push(("capture.copy-screenshot".into(), copy));
                rects.push(("capture.show-screenshot".into(), reveal));
            }
            rects.push(("capture.dismiss-screenshot".into(), dismiss));
            return rects;
        }
        if self.recording_finished_at.is_some() {
            let (_, open, reveal, dismiss) = self.recording_receipt_rects();
            return vec![
                ("capture.play-recording".into(), open),
                ("capture.show-recording".into(), reveal),
                ("capture.dismiss-recording".into(), dismiss),
            ];
        }
        if self.recording_active {
            let (pill, pause, stop) = self.recording_indicator_rects();
            return vec![
                (
                    if self.recording_paused {
                        "capture.resume"
                    } else {
                        "capture.pause"
                    }
                    .into(),
                    pause,
                ),
                ("capture.stop".into(), stop),
                ("capture.recording-status".into(), pill),
            ];
        }
        if self.armed_capture.is_some() {
            return Vec::new();
        }
        let mut o = vec![("capture.canvas".into(), self.selection_bounds())];
        for (n, r) in ["shot", "record", "pip"].into_iter().zip(self.mode_rects()) {
            o.push((format!("capture.{n}"), r))
        }
        for (n, r) in ["area", "window", "screen", "multi"]
            .into_iter()
            .zip(self.source_rects())
        {
            o.push((format!("capture.{n}"), r))
        }
        if self.mode != CaptureMode::Pip {
            o.push(("capture.delay".into(), self.delay_rect()));
            o.push(("capture.pointer".into(), self.pointer_rect()));
        }
        if self.mode == CaptureMode::Recording && self.system_audio_available {
            o.push(("capture.system-audio".into(), self.system_audio_rect()));
        }
        if self.mode == CaptureMode::Recording && self.microphone_available {
            o.push(("capture.microphone".into(), self.microphone_rect()));
        }
        let action_names = match self.mode {
            CaptureMode::Pip => ["copy", "create-pip", "save"],
            CaptureMode::Recording => ["copy", "start-replay", "save"],
            CaptureMode::Screenshot => ["copy", "float", "save"],
        };
        for (n, r) in action_names.into_iter().zip(self.action_rects()) {
            if r.area() > 0.0 {
                o.push((format!("capture.{n}"), r))
            }
        }
        if matches!(self.source, SourceMode::Region | SourceMode::Multi) {
            for (index, region) in self.regions.iter().enumerate() {
                o.push((format!("capture.selection.{index}.move"), *region));
                if let Some(remove) = self.region_remove_rects.get(index) {
                    o.push((format!("capture.selection.{index}.remove"), *remove));
                }
                for (handle, point) in Self::selection_handle_points(*region) {
                    let name = match handle {
                        SelectionHandle::NorthWest => "nw",
                        SelectionHandle::North => "n",
                        SelectionHandle::NorthEast => "ne",
                        SelectionHandle::East => "e",
                        SelectionHandle::SouthEast => "se",
                        SelectionHandle::South => "s",
                        SelectionHandle::SouthWest => "sw",
                        SelectionHandle::West => "w",
                    };
                    o.push((
                        format!("capture.selection.{index}.{name}"),
                        Rect::from_center_size(point, (20.0, 20.0)),
                    ));
                }
            }
        }
        if matches!(self.source, SourceMode::Window | SourceMode::Multi) {
            let option_names = if self.source == SourceMode::Multi {
                ["pip.auto", "pip.grid", "pip.stack"]
            } else {
                ["extent.visible", "extent.full-app", "extent.whole-strip"]
            };
            for (name, r) in option_names.into_iter().zip(self.option_rects()) {
                o.push((format!("capture.{name}"), r));
            }
        }
        if self.source == SourceMode::Window {
            for (target, rect) in self.candidates.iter().zip(self.candidate_picker_rects()) {
                o.push((format!("capture.picker:{}", target.title), rect));
            }
        }
        for target in &self.candidates {
            let rect = target.visible_rect.intersect(self.selection_bounds());
            if rect.area() > 0.0 {
                o.push((format!("capture.app:{}", target.title), rect));
            }
        }
        o
    }
}

fn normal_rect(a: Point, b: Point) -> Rect {
    Rect::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
}

fn wrapped_picker_index(current: usize, len: usize, forward: bool) -> usize {
    if len <= 1 {
        return 0;
    }
    if forward {
        (current + 1) % len
    } else if current == 0 {
        len - 1
    } else {
        current - 1
    }
}

fn add_rect(path: &mut BezPath, r: Rect) {
    path.move_to((r.x0, r.y0));
    path.line_to((r.x1, r.y0));
    path.line_to((r.x1, r.y1));
    path.line_to((r.x0, r.y1));
    path.close_path();
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::field_reassign_with_default,
        clippy::unchecked_time_subtraction,
        reason = "tests construct and age retained state directly to exercise visual phases"
    )]
    use super::*;
    #[test]
    fn app_picker_names_workflows_and_commits_full_targets() {
        fn picker_context_advances_from_instruction_to_ready_state() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1200., 800.);
            assert_eq!(s.default_picker_context(), "Drag to select");

            s.regions.push(Rect::new(20., 100., 640., 470.));
            assert_eq!(s.default_picker_context(), "620 × 370 selected");

            s.set_mode(CaptureMode::Recording);
            assert_eq!(s.default_picker_context(), "620 × 370 selected");

            s.set_mode(CaptureMode::Pip);
            s.set_source_mode(SourceMode::Multi);
            s.regions.extend([
                Rect::new(20., 100., 640., 470.),
                Rect::new(700., 120., 1050., 420.),
            ]);
            assert_eq!(
                s.default_picker_context(),
                "2 areas selected. Shift-drag to add another."
            );
        }

        fn tiny_app_continuation_uses_symbolic_card_instead_of_cramped_badge() {
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            let mut target = CaptureTarget {
                id: "notes".into(),
                title: "Notes".into(),
                app_id: "so.tilo.notes".into(),
                visible_rect: Rect::new(0.0, 90.0, 84.0, 590.0),
                virtual_rect: Rect::new(-916.0, 90.0, 84.0, 590.0),
                complete_rect: Some(Rect::new(-916.0, 90.0, 394.0, 1_278.0)),
            };
            assert!(!CaptureStudio::target_has_useful_aperture(
                &target, viewport
            ));

            target.visible_rect = Rect::new(80.0, 90.0, 500.0, 590.0);
            assert!(CaptureStudio::target_has_useful_aperture(&target, viewport));
        }

        fn offscreen_app_card_names_each_workflow_honestly() {
            assert_eq!(
                CaptureStudio::offscreen_guidance(CaptureMode::Screenshot, CaptureExtent::FullApp),
                "Capture it without moving it onto this screen"
            );
            assert_eq!(
                CaptureStudio::offscreen_guidance(CaptureMode::Recording, CaptureExtent::FullApp),
                "Record it without moving it onto this screen"
            );
            assert_eq!(
                CaptureStudio::offscreen_guidance(CaptureMode::Pip, CaptureExtent::FullApp),
                "Float it live without moving it onto this screen"
            );
        }

        fn symbolic_app_picker_commits_the_same_full_target_for_all_workflows() {
            let target = CaptureTarget {
                id: "notes".into(),
                title: "Notes".into(),
                app_id: "so.tilo.notes".into(),
                visible_rect: Rect::new(0.0, 90.0, 84.0, 590.0),
                virtual_rect: Rect::new(-916.0, 90.0, 84.0, 590.0),
                complete_rect: Some(Rect::new(-916.0, 90.0, 394.0, 1_278.0)),
            };
            for (mode, destination) in [
                (CaptureMode::Screenshot, CaptureDestination::File),
                (CaptureMode::Recording, CaptureDestination::File),
                (CaptureMode::Pip, CaptureDestination::FloatingWindow),
            ] {
                let mut studio = CaptureStudio::default();
                studio.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
                studio.set_candidates(vec![target.clone()]);
                studio.set_source_mode(SourceMode::Window);
                studio.set_extent(CaptureExtent::FullApp);
                studio.set_mode(mode);
                studio.commit(destination);

                let intent = studio.take_intents().pop().expect("capture intent");
                assert_eq!(intent.mode, mode);
                assert_eq!(intent.destination, destination);
                assert_eq!(intent.extent, CaptureExtent::FullApp);
                assert_eq!(intent.targets, vec![target.clone()]);
                assert_eq!(
                    intent.recording_command,
                    (mode == CaptureMode::Recording).then_some(RecordingCommand::Start)
                );
            }
        }

        fn full_app_can_select_a_completely_offscreen_candidate() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1200., 800.);
            s.set_candidates(vec![CaptureTarget {
                id: "offscreen-notes".into(),
                title: "Notes".into(),
                app_id: "org.tilo.Notes".into(),
                visible_rect: Rect::ZERO,
                virtual_rect: Rect::new(1400., 80., 2300., 720.),
                complete_rect: None,
            }]);
            s.set_source_mode(SourceMode::Window);
            s.set_extent(CaptureExtent::FullApp);
            assert!(s.active_regions().is_empty());
            assert!(s.has_capture_source());
            s.commit(CaptureDestination::File);
            let intent = s.take_intents().pop().expect("full offscreen app intent");
            assert_eq!(intent.targets[0].id, "offscreen-notes");
            assert!(intent.regions.is_empty());
        }

        fn arrows_navigate_app_targets_scope_and_multi_layout_with_wraparound() {
            let mut s = CaptureStudio::default();
            s.set_candidates(vec![
                CaptureTarget {
                    id: "notes".into(),
                    title: "Notes".into(),
                    app_id: "org.tilo.Notes".into(),
                    visible_rect: Rect::new(0.0, 0.0, 400.0, 400.0),
                    virtual_rect: Rect::new(0.0, 0.0, 400.0, 400.0),
                    complete_rect: None,
                },
                CaptureTarget {
                    id: "files".into(),
                    title: "Files".into(),
                    app_id: "org.tilo.Files".into(),
                    visible_rect: Rect::new(400.0, 0.0, 800.0, 400.0),
                    virtual_rect: Rect::new(400.0, 0.0, 800.0, 400.0),
                    complete_rect: None,
                },
            ]);
            s.set_source_mode(SourceMode::Window);

            assert_eq!(s.selected_target.as_deref(), Some("notes"));
            assert!(s.navigate_picker(NamedKey::ArrowRight));
            assert_eq!(s.selected_target.as_deref(), Some("files"));
            assert!(s.navigate_picker(NamedKey::ArrowRight));
            assert_eq!(s.selected_target.as_deref(), Some("notes"));
            assert!(s.navigate_picker(NamedKey::ArrowLeft));
            assert_eq!(s.selected_target.as_deref(), Some("files"));

            assert_eq!(s.extent, CaptureExtent::Visible);
            assert!(s.navigate_picker(NamedKey::ArrowDown));
            assert_eq!(s.extent, CaptureExtent::FullApp);
            assert!(s.navigate_picker(NamedKey::ArrowDown));
            assert_eq!(s.extent, CaptureExtent::WholeStrip);
            assert!(s.navigate_picker(NamedKey::ArrowDown));
            assert_eq!(s.extent, CaptureExtent::Visible);
            assert!(s.navigate_picker(NamedKey::ArrowUp));
            assert_eq!(s.extent, CaptureExtent::WholeStrip);

            s.set_source_mode(SourceMode::Multi);
            assert_eq!(s.pip_layout, PipLayout::Auto);
            assert!(s.navigate_picker(NamedKey::ArrowRight));
            assert_eq!(s.pip_layout, PipLayout::Grid);
            assert!(s.navigate_picker(NamedKey::ArrowDown));
            assert_eq!(s.pip_layout, PipLayout::Stack);
            assert!(s.navigate_picker(NamedKey::ArrowRight));
            assert_eq!(s.pip_layout, PipLayout::Auto);
            assert!(s.navigate_picker(NamedKey::ArrowLeft));
            assert_eq!(s.pip_layout, PipLayout::Stack);
        }

        picker_context_advances_from_instruction_to_ready_state();
        tiny_app_continuation_uses_symbolic_card_instead_of_cramped_badge();
        offscreen_app_card_names_each_workflow_honestly();
        symbolic_app_picker_commits_the_same_full_target_for_all_workflows();
        full_app_can_select_a_completely_offscreen_candidate();
        arrows_navigate_app_targets_scope_and_multi_layout_with_wraparound();
    }

    #[test]
    fn region_selection_multi_edits_and_offscreen_extents() {
        fn multi_keeps_each_region() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1200., 800.);
            s.open();
            s.set_mode(CaptureMode::Pip);
            s.set_source_mode(SourceMode::Multi);
            s.regions.extend([
                Rect::new(20., 100., 300., 300.),
                Rect::new(500., 120., 900., 420.),
            ]);
            s.commit(CaptureDestination::FloatingWindow);
            let i = s.take_intents().pop().unwrap();
            assert_eq!(i.regions.len(), 2);
            assert_eq!(i.destination, CaptureDestination::FloatingWindow);
            assert_eq!(i.source, SourceMode::Multi);
            assert_eq!(i.mode, CaptureMode::Pip);
        }

        fn multi_new_drag_replaces_unless_shift_extends_selection() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
            s.open();
            s.set_source_mode(SourceMode::Multi);
            s.regions.push(Rect::new(80.0, 100.0, 260.0, 240.0));

            s.click_with_additive(Point::new(420.0, 180.0), false);
            s.drag
                .as_mut()
                .unwrap()
                .set_current(Point::new(620.0, 360.0));
            s.finish_region_drag();
            assert_eq!(s.regions, vec![Rect::new(420.0, 180.0, 620.0, 360.0)]);

            s.click_with_additive(Point::new(90.0, 390.0), true);
            s.drag
                .as_mut()
                .unwrap()
                .set_current(Point::new(290.0, 560.0));
            s.finish_region_drag();
            assert_eq!(s.regions.len(), 2, "Shift extends the existing selection");
            assert_eq!(s.regions[0], Rect::new(420.0, 180.0, 620.0, 360.0));
            assert_eq!(s.regions[1], Rect::new(90.0, 390.0, 290.0, 560.0));
        }

        fn region_selection_reaches_output_edges_and_chrome_prefers_outside() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
            s.open();
            s.click(Point::new(200.0, 300.0));
            s.drag.as_mut().unwrap().set_current(Point::new(500.0, 0.0));
            assert_eq!(s.active_regions()[0].y0, 0.0);
            s.drag
                .as_mut()
                .unwrap()
                .set_current(Point::new(500.0, 600.0));
            assert_eq!(s.active_regions()[0].y1, 600.0);

            let middle = Rect::new(180.0, 180.0, 500.0, 380.0);
            let outside = CaptureStudio::region_chrome_layout(middle, 90.0, s.rect, &[middle]);
            assert!(outside.outside);
            assert_eq!(outside.metadata.y1, middle.y0 - 6.0);

            let full = Rect::new(0.0, 0.0, 800.0, 600.0);
            let inside = CaptureStudio::region_chrome_layout(full, 90.0, s.rect, &[full]);
            assert!(
                !inside.outside,
                "a full-output selection has no outside lane"
            );
            assert!(full.contains(inside.metadata.center()));
        }

        fn existing_region_moves_resizes_nudges_and_clamps_without_restarting() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
            s.open();
            s.set_mode(CaptureMode::Screenshot);
            s.set_source_mode(SourceMode::Region);
            s.regions.push(Rect::new(100.0, 150.0, 420.0, 390.0));

            let original = s.regions[0];
            s.click(original.center());
            assert!(matches!(s.drag, Some(DragSelection::Move { .. })));
            s.drag
                .as_mut()
                .unwrap()
                .set_current(original.center() + vello::kurbo::Vec2::new(55.0, -24.0));
            assert_eq!(
                s.active_regions(),
                vec![original + vello::kurbo::Vec2::new(55.0, -24.0)]
            );
            s.finish_region_drag();
            assert_eq!(s.regions, s.active_regions());

            let moved = s.regions[0];
            let south_east = Point::new(moved.x1, moved.y1);
            s.click(south_east);
            assert!(matches!(s.drag, Some(DragSelection::Resize { .. })));
            s.drag
                .as_mut()
                .unwrap()
                .set_current(Point::new(900.0, 900.0));
            let resized = s.active_regions()[0];
            assert_eq!(resized.x0, moved.x0);
            assert_eq!(resized.y0, moved.y0);
            assert_eq!(resized.x1, s.selection_bounds().x1);
            assert_eq!(resized.y1, s.selection_bounds().y1);
            s.finish_region_drag();

            let before_nudge = s.regions[0];
            assert!(s.nudge_region(NamedKey::ArrowLeft, 10.0));
            assert_eq!(s.regions[0].x0, before_nudge.x0 - 10.0);
            assert_eq!(s.regions[0].x1, before_nudge.x1 - 10.0);

            // Escape during a correction cancels only that correction; the picker
            // and last committed geometry remain intact.
            let committed = s.regions[0];
            s.click(committed.center());
            assert!(s.drag.take().is_some());
            assert_eq!(s.regions[0], committed);
            assert!(s.is_open());
        }

        fn pip_has_one_live_primary_action_and_region_removal() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1200., 800.);
            s.open();
            s.include_pointer = true;
            s.set_mode(CaptureMode::Pip);
            s.set_source_mode(SourceMode::Multi);
            s.regions.extend([
                Rect::new(100., 120., 420., 340.),
                Rect::new(500., 180., 900., 520.),
            ]);
            let actions = s.action_rects();
            assert_eq!(actions[0], Rect::ZERO);
            assert!(actions[1].area() > 0.0);
            assert_eq!(actions[2], Rect::ZERO);
            assert!(
                !s.include_pointer,
                "PiP does not inherit screenshot cursor trails"
            );

            let first = s.regions[0];
            let remove =
                CaptureStudio::region_chrome_layout(first, 96.0, s.rect, &s.regions).remove;
            s.region_remove_rects = vec![remove, Rect::ZERO];
            s.click(remove.center());
            assert_eq!(s.regions, vec![Rect::new(500., 180., 900., 520.)]);
        }

        fn offscreen_extent_survives_into_compositor_intent() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1200., 800.);
            s.open();
            s.set_candidates(vec![CaptureTarget {
                id: "notes".into(),
                title: "Notes".into(),
                app_id: "org.tilo.Notes".into(),
                visible_rect: Rect::new(100., 100., 900., 600.),
                virtual_rect: Rect::new(100., 100., 1500., 900.),
                complete_rect: Some(Rect::new(100., 100., 1500., 1900.)),
            }]);
            s.set_source_mode(SourceMode::Window);
            s.set_extent(CaptureExtent::FullApp);
            s.commit(CaptureDestination::File);
            let i = s.take_intents().pop().unwrap();
            assert_eq!(i.source, SourceMode::Window);
            assert_eq!(i.extent, CaptureExtent::FullApp);
            assert_eq!(
                i.targets[0].virtual_rect,
                Rect::new(100., 100., 1500., 900.)
            );
            assert_eq!(
                i.targets[0].full_rect(),
                Rect::new(100., 100., 1500., 1900.),
                "App selection keeps the provider's complete document geometry",
            );
            assert_eq!(
                i.targets[0].full_app_rect(CaptureMode::Recording),
                i.targets[0].virtual_rect,
                "live video follows the app window rather than squeezing a complete document",
            );
            assert_eq!(
                i.targets[0].full_app_rect(CaptureMode::Pip),
                i.targets[0].virtual_rect,
            );
            assert_eq!(
                i.targets[0].full_app_rect(CaptureMode::Screenshot),
                i.targets[0].full_rect(),
                "still capture retains the creative complete-document export",
            );
        }

        multi_keeps_each_region();
        multi_new_drag_replaces_unless_shift_extends_selection();
        region_selection_reaches_output_edges_and_chrome_prefers_outside();
        existing_region_moves_resizes_nudges_and_clamps_without_restarting();
        pip_has_one_live_primary_action_and_region_removal();
        offscreen_extent_survives_into_compositor_intent();
    }

    #[test]
    fn capture_options_delay_audio_and_display_sources() {
        fn delayed_capture_is_non_modal_and_only_queues_after_expiry() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1200., 800.);
            s.open();
            s.set_source_mode(SourceMode::Display);
            s.click(s.delay_rect().center());
            assert_eq!(s.delay_seconds, 3);

            s.commit(CaptureDestination::File);
            assert!(s.open, "the lightweight countdown remains present");
            assert!(s.modal_capture().is_none(), "apps must remain interactive");
            assert!(s.take_intents().is_empty(), "capture is not queued early");

            s.armed_capture.as_mut().unwrap().started_at =
                crate::time::now() - Duration::from_secs(4);
            let intent = s.take_intents().pop().expect("expired delayed intent");
            assert_eq!(intent.destination, CaptureDestination::File);
            assert_eq!(intent.source, SourceMode::Display);
            assert!(!s.open);
        }

        fn audio_sources_and_delay_keep_independent_slots() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1200., 800.);
            s.open();
            s.set_mode(CaptureMode::Recording);
            s.set_audio_available(true, true);
            let pointer = s.pointer_rect();
            let system = s.system_audio_rect();
            let microphone = s.microphone_rect();
            let delay = s.delay_rect();
            assert!(pointer.x1 < system.x0);
            assert!(system.x1 < microphone.x0);
            assert!(microphone.x1 < delay.x0);
            assert!(delay.x1 < s.action_rects()[2].x0);

            s.click(system.center());
            assert_eq!(s.audio, AudioCaptureMode::System);
            s.click(microphone.center());
            assert_eq!(s.audio, AudioCaptureMode::SystemAndMicrophone);

            s.click(delay.center());
            assert_eq!(s.delay_seconds, 3);
            s.click(delay.center());
            assert_eq!(s.delay_seconds, 10);
            s.click(delay.center());
            assert_eq!(s.delay_seconds, 0);
        }

        fn display_source_is_whole_output_after_picker_controls_leave() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1000., 700.);
            s.open();
            s.set_source_mode(SourceMode::Display);
            let r = s.active_regions()[0];
            assert_eq!(r, s.rect);
        }

        fn screen_and_app_capture_do_not_inherit_picker_safe_insets() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            s.set_overlay_rect(s.rect);

            s.set_source_mode(SourceMode::Display);
            assert_eq!(
                s.active_regions(),
                vec![s.rect],
                "Screen means the complete clean output, not the area between picker bars"
            );

            let app = Rect::new(18.0, 12.0, 1_582.0, 888.0);
            s.set_candidates(vec![CaptureTarget {
                id: "notes".into(),
                title: "Notes".into(),
                app_id: "so.tilo.notes".into(),
                visible_rect: app,
                virtual_rect: app,
                complete_rect: None,
            }]);
            s.set_source_mode(SourceMode::Window);
            assert_eq!(
                s.active_regions(),
                vec![app],
                "temporary picker chrome must not crop a visible app"
            );
        }

        fn full_app_start_confirms_which_screen_edges_continue() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            s.set_overlay_rect(s.rect);
            s.set_candidates(vec![CaptureTarget {
                id: "canvas".into(),
                title: "Canvas".into(),
                app_id: "so.tilo.canvas".into(),
                visible_rect: Rect::new(0.0, 80.0, 1_600.0, 900.0),
                virtual_rect: Rect::new(-720.0, 80.0, 2_240.0, 1_480.0),
                complete_rect: None,
            }]);
            s.set_source_mode(SourceMode::Window);
            s.set_extent(CaptureExtent::FullApp);
            s.set_recording_active(true);

            assert_eq!(s.recording_continuation_edges, [true, false, true, true]);
            assert_eq!(s.recording_scope, "Canvas · app window");
            assert_eq!(s.recording_scope_icon(), Icon::Layers);
        }

        delayed_capture_is_non_modal_and_only_queues_after_expiry();
        audio_sources_and_delay_keep_independent_slots();
        display_source_is_whole_output_after_picker_controls_leave();
        screen_and_app_capture_do_not_inherit_picker_safe_insets();
        full_app_start_confirms_which_screen_edges_continue();
    }

    #[test]
    fn active_recording_stops_pauses_and_leaves_non_modal_receipts() {
        fn active_recording_can_stop_without_reselecting_a_source() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1000., 700.);
            s.set_recording_active(true);
            assert!(s.is_open(), "recording confirmation opens automatically");
            assert!(s.modal_capture().is_none(), "recording status is non-modal");
            s.regions.clear();
            s.commit(CaptureDestination::File);
            s.commit(CaptureDestination::File);
            assert_eq!(s.pending.len(), 1, "Stop is idempotent while flushing");
            let intent = s.take_intents().pop().expect("stop intent");
            assert_eq!(intent.mode, CaptureMode::Recording);
            assert_eq!(intent.recording_command, Some(RecordingCommand::Stop));
            assert!(intent.regions.is_empty());
            assert!(
                s.is_open(),
                "indicator remains until compositor confirmation"
            );
            s.set_recording_active(false);
            assert!(!s.is_open());
        }

        fn successful_stop_becomes_non_modal_recording_actions_receipt() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1200., 800.);
            s.set_recording_active(true);
            s.recording_started_at = Some(crate::time::now() - Duration::from_secs(42));
            s.set_recording_finished();

            assert!(!s.is_recording_active());
            assert!(s.is_open());
            assert!(s.modal_capture().is_none());
            assert_eq!(s.recording_finished_duration.as_secs(), 42);
            assert_eq!(s.test_rects().len(), 3);
            let (_, open, _, _) = s.recording_receipt_rects();
            s.click(open.center());
            let intent = s.take_intents().pop().expect("open intent");
            assert_eq!(intent.recording_command, Some(RecordingCommand::OpenLast));
            assert!(!s.is_open());

            s.set_recording_active(true);
            s.set_recording_finished();
            let (_, _, reveal, _) = s.recording_receipt_rects();
            s.click(reveal.center());
            let intent = s.take_intents().pop().expect("reveal intent");
            assert_eq!(intent.recording_command, Some(RecordingCommand::RevealLast));
            assert!(!s.is_open());

            s.set_recording_active(true);
            s.set_recording_finished();
            let (_, _, _, dismiss) = s.recording_receipt_rects();
            s.click(dismiss.center());
            assert!(s.take_intents().is_empty());
            assert!(!s.is_open());
        }

        fn active_recording_pause_resume_is_explicit_and_timer_excludes_pause() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1000., 700.);
            s.set_recording_active(true);
            s.recording_started_at = Some(crate::time::now() - Duration::from_secs(10));

            let (_, pause, _) = s.recording_indicator_rects_at(1.0);
            s.click(pause.center());
            s.click(pause.center());
            assert_eq!(s.pending.len(), 1, "pause is idempotent while pending");
            assert_eq!(
                s.take_intents()[0].recording_command,
                Some(RecordingCommand::Pause)
            );
            s.set_recording_paused(true);
            let frozen = s.recording_elapsed();
            std::thread::sleep(Duration::from_millis(12));
            assert!(
                s.recording_elapsed().saturating_sub(frozen) < Duration::from_millis(2),
                "elapsed recording time freezes while paused"
            );
            assert_eq!(s.test_rects()[0].0, "capture.resume");

            let (_, resume, _) = s.recording_indicator_rects_at(1.0);
            s.click(resume.center());
            assert_eq!(
                s.take_intents()[0].recording_command,
                Some(RecordingCommand::Resume)
            );
            s.set_recording_paused(false);
            assert!(!s.is_recording_paused());
            assert_eq!(s.test_rects()[0].0, "capture.pause");
        }

        fn settled_recording_controls_keep_spacing_at_wide_and_compact_widths() {
            let mut s = CaptureStudio::default();
            for width in [800.0, 1600.0] {
                s.rect = Rect::new(0.0, 0.0, width, 700.0);
                let (pill, pause, stop) = s.recording_indicator_rects_at(1.0);
                assert!(pause.x0 >= pill.x0 && pause.x1 <= pill.x1);
                assert!(pause.y0 >= pill.y0 && pause.y1 <= pill.y1);
                assert!(stop.x0 >= pill.x0 && stop.x1 <= pill.x1);
                assert!(stop.y0 >= pill.y0 && stop.y1 <= pill.y1);
                assert!(
                    pause.x1 + 3.9 <= stop.x0,
                    "pause and Stop need a visual gap"
                );
                assert!(pause.width() >= 30.0 && stop.width() >= 64.0);
                assert_eq!(pill.width(), if width < 1000.0 { 360.0 } else { 252.0 });
                assert_eq!(pill.height(), if width < 1000.0 { 28.0 } else { 34.0 });
                if width < 1000.0 {
                    assert_eq!(pill.center().x, s.rect.center().x);
                    assert_eq!(pill.y0, s.rect.y0 + 6.0);
                }
            }

            let launch = s.recording_indicator_rects_at(0.0).0;
            assert!(
                launch.y0 >= s.rect.y0 + 120.0,
                "launch confirmation belongs below app tabs: {launch:?}"
            );

            s.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
            let (pill, _, _) = s.recording_indicator_rects_at(1.0);
            let clock_right = s.rect.center().x + 180.0;
            assert_eq!(pill.x0 - clock_right, 12.0, "wide capsules must not touch");

            let clock = Rect::new(
                s.rect.center().x - 180.0,
                s.rect.y0 + 6.0,
                clock_right,
                s.rect.y0 + 34.0,
            );
            let halfway = s.recording_indicator_rects_at(0.5).0;
            assert!(
                halfway.y1 <= s.rect.y0 + 85.0,
                "the travelling pill must clear the app tab row before moving sideways: {halfway:?}"
            );
            assert!(
                halfway.x0 > s.rect.center().x - 152.0,
                "the pill should make visible horizontal progress in the quiet lane: {halfway:?}"
            );
            for step in 0..=100 {
                let moving = s.recording_indicator_rects_at(f64::from(step) / 100.0).0;
                if moving.y0 < clock.y1 && moving.y1 > clock.y0 {
                    assert!(
                        moving.x0 >= clock.x1 + 8.0,
                        "pill must clear the clock before rising into its lane: step={step}, \
                     pill={moving:?}, clock={clock:?}"
                    );
                }
            }
        }

        fn settled_recording_scope_uses_symbolic_source_identity() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            s.set_overlay_rect(s.rect);
            s.set_source_mode(SourceMode::Display);
            s.set_recording_active(true);

            assert_eq!(s.recording_scope, "Screen");
            assert_eq!(s.recording_scope_icon(), Icon::Monitor);
            assert_eq!(s.compact_recording_scope(10), "Screen");
            assert_eq!(s.recording_status_detail(), "Screen · no audio");

            s.include_pointer = true;
            s.audio = AudioCaptureMode::SystemAndMicrophone;
            assert_eq!(
                s.recording_status_detail(),
                "Screen · pointer · system audio + microphone"
            );
        }

        active_recording_can_stop_without_reselecting_a_source();
        successful_stop_becomes_non_modal_recording_actions_receipt();
        active_recording_pause_resume_is_explicit_and_timer_excludes_pause();
        settled_recording_controls_keep_spacing_at_wide_and_compact_widths();
        settled_recording_scope_uses_symbolic_source_identity();
    }

    #[test]
    fn replay_and_screenshot_receipts_are_non_modal() {
        fn replay_arming_is_distinct_from_starting_a_recording() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1200., 800.);
            s.open();
            s.set_mode(CaptureMode::Recording);
            s.set_source_mode(SourceMode::Display);
            s.click(s.action_rects()[1].center());

            let intent = s.take_intents().pop().expect("replay start intent");
            assert_eq!(intent.replay_command, Some(ReplayCommand::Start));
            assert_eq!(intent.recording_command, None);
            assert_eq!(intent.source, SourceMode::Display);
            assert!(!s.is_open());
        }

        fn screenshot_receipt_is_visual_non_modal_and_revealable() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 1600., 900.);
            let preview = CapturePreview {
                columns: 2,
                rows: 2,
                pixels: vec![
                    [12, 18, 28, 255],
                    [48, 96, 180, 255],
                    [26, 30, 38, 255],
                    [84, 138, 220, 255],
                ],
            };
            s.set_screenshot_finished(false, 1_584, 716, preview.clone());
            let origin = s.screenshot_receipt.as_ref().unwrap().origin;
            let start = s.screenshot_receipt_rects_at(0.0, origin).0;
            let start_preview =
                CaptureStudio::screenshot_receipt_preview_rect(0.0, start, 1_584, 716);
            assert_eq!(start.width(), 156.0);
            assert!((start.height() - 77.091).abs() < 0.01);
            assert_eq!(start_preview.width(), 144.0);
            assert!((start_preview.height() - 65.091).abs() < 0.01);
            assert!(start.contains(start_preview.origin()));
            assert!(start.contains(Point::new(start_preview.x1, start_preview.y1)));
            s.screenshot_receipt.as_mut().unwrap().shown_at -= Duration::from_secs(1);
            assert!(s.modal_capture().is_none());
            let (pill, pin, copy, reveal, dismiss) = s.screenshot_receipt_rects();
            assert_eq!(pill.width(), 354.0);
            assert!(pill.contains(copy.center()));
            assert!(pill.contains(pin.center()));
            assert!(pill.contains(reveal.center()));
            assert!(pill.contains(dismiss.center()));
            assert_eq!(pill.x0 - (s.rect.center().x + 180.0), 12.0);
            assert_eq!(s.test_rects().len(), 6);
            let preview_rect =
                CaptureStudio::screenshot_receipt_preview_rect(1.0, pill, 1_584, 716);
            s.click(preview_rect.center());
            assert_eq!(
                s.take_intents().pop().unwrap().screenshot_command,
                Some(ScreenshotCommand::MarkupLast)
            );
            assert!(!s.is_open());

            s.set_screenshot_finished(false, 1_584, 716, preview.clone());
            s.screenshot_receipt.as_mut().unwrap().shown_at -= Duration::from_secs(1);
            let (pill, pin, _, _, _) = s.screenshot_receipt_rects();
            let pin_spawn = CaptureStudio::screenshot_receipt_preview_rect(1.0, pill, 1_584, 716);
            s.click(pin.center());
            let pin_intent = s.take_intents().pop().unwrap();
            assert_eq!(
                pin_intent.screenshot_command,
                Some(ScreenshotCommand::PinLast)
            );
            assert_eq!(pin_intent.regions, vec![pin_spawn]);
            assert!(
                s.is_open(),
                "the receipt proxies the source until shell adoption"
            );
            assert!(s.screenshot_pin_pending);
            s.set_screenshot_pinned();
            assert!(!s.is_open());

            s.set_screenshot_finished(false, 1_584, 716, preview.clone());
            s.screenshot_receipt.as_mut().unwrap().shown_at -= Duration::from_secs(1);
            let (_, _, copy, _, _) = s.screenshot_receipt_rects();
            s.click(copy.center());
            assert_eq!(
                s.take_intents().pop().unwrap().screenshot_command,
                Some(ScreenshotCommand::CopyLast)
            );
            assert!(!s.is_open());

            s.set_screenshot_finished(false, 1_584, 716, preview.clone());
            s.screenshot_receipt.as_mut().unwrap().shown_at -= Duration::from_secs(1);
            let (_, _, _, reveal, _) = s.screenshot_receipt_rects();
            s.click(reveal.center());
            assert_eq!(
                s.take_intents().pop().unwrap().screenshot_command,
                Some(ScreenshotCommand::RevealLast)
            );
            assert!(!s.is_open());

            // The picker node may be relaid out into a wider content strip after
            // modal chrome closes; status geometry must still use the compositor
            // viewport supplied by its overlay pass.
            s.rect = Rect::new(100., 43., 1_300., 643.);
            s.set_overlay_rect(Rect::new(0., 0., 800., 600.));
            s.set_screenshot_finished(true, 800, 600, preview);
            s.screenshot_receipt.as_mut().unwrap().shown_at -= Duration::from_secs(1);
            let (pill, _, _, _, dismiss) = s.screenshot_receipt_rects();
            assert_eq!(pill.width(), 394.0);
            assert_eq!(pill.height(), 28.0);
            assert_eq!(pill.center().x, 400.0);
            assert_eq!(pill.y0, 6.0);
            assert_eq!(s.test_rects().len(), 1, "copied shots need only Dismiss");
            s.click(dismiss.center());
            assert!(s.take_intents().is_empty());
            assert!(!s.is_open());
        }

        fn replay_indicator_is_non_modal_and_supports_repeat_save_then_stop() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0., 0., 800., 600.);
            s.set_replay_active(true, AudioCaptureMode::SystemAndMicrophone);
            s.set_replay_buffered_seconds(30);
            assert!(s.is_replay_active());
            assert!(s.modal_capture().is_none());
            assert_eq!(s.replay_indicator_rects_at(1.0).0.width(), 360.0);
            assert_eq!(s.replay_status_label(false), "Replay · last 30s");
            assert_eq!(
                s.replay_status_detail(),
                "30-second rolling buffer · system audio + microphone · Super+Shift+R saves now"
            );

            s.set_replay_buffered_seconds(0);
            assert_eq!(s.replay_status_label(false), "Buffering…");
            s.set_replay_buffered_seconds(4);
            assert_eq!(s.replay_status_label(false), "Buffering · 4s");
            assert_eq!(s.replay_status_label(true), "Saved · 4s");
            s.set_replay_buffered_seconds(30);

            let (_, save, stop) = s.replay_indicator_rects();
            s.click(save.center());
            assert_eq!(
                s.take_intents().pop().unwrap().replay_command,
                Some(ReplayCommand::SaveRecent)
            );
            assert!(s.is_replay_active(), "saving must not disarm the buffer");

            s.set_replay_saved();
            let (play, reveal) = s.replay_saved_action_rects();
            s.click(play.center());
            assert_eq!(
                s.take_intents().pop().unwrap().recording_command,
                Some(RecordingCommand::OpenLast)
            );
            assert!(s.is_replay_active(), "playing must not disarm the buffer");
            s.click(reveal.center());
            assert_eq!(
                s.take_intents().pop().unwrap().recording_command,
                Some(RecordingCommand::RevealLast)
            );
            assert_eq!(
                s.test_rects()
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
                vec![
                    "capture.replay-status",
                    "capture.play-replay",
                    "capture.show-replay",
                    "capture.stop-replay"
                ]
            );

            s.click(stop.center());
            assert_eq!(
                s.take_intents().pop().unwrap().replay_command,
                Some(ReplayCommand::Stop)
            );

            for width in [800.0, 1600.0] {
                s.rect = Rect::new(0.0, 0.0, width, 700.0);
                let (pill, save, stop) = s.replay_indicator_rects_at(1.0);
                assert!(pill.contains(save.center()));
                assert!(pill.contains(stop.center()));
                assert!(save.x1 + 3.9 <= stop.x0, "Save and Stop need separation");
                assert_eq!(pill.width(), if width < 1000.0 { 360.0 } else { 276.0 });
                assert_eq!(pill.height(), if width < 1000.0 { 28.0 } else { 34.0 });
            }

            s.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
            let (pill, _, _) = s.replay_indicator_rects_at(1.0);
            let clock_left = s.rect.center().x - 180.0;
            assert_eq!(clock_left - pill.x1, 12.0, "wide capsules must not touch");
        }

        replay_arming_is_distinct_from_starting_a_recording();
        screenshot_receipt_is_visual_non_modal_and_revealable();
        replay_indicator_is_non_modal_and_supports_repeat_save_then_stop();
    }

    #[test]
    fn visual_harness_pins_receipt_and_recording_phases() {
        fn visual_harness_can_pin_screenshot_receipt_transition() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            s.set_screenshot_finished(
                true,
                640,
                360,
                CapturePreview {
                    columns: 1,
                    rows: 1,
                    pixels: vec![[12, 18, 28, 255]],
                },
            );
            s.set_screenshot_transition_progress(500);
            let receipt = s.screenshot_receipt.as_ref().unwrap();
            assert!((CaptureStudio::screenshot_receipt_motion(receipt) - 0.5).abs() < 0.02);
        }

        fn visual_harness_can_pin_each_recording_transition_phase() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            s.set_recording_active(true);

            for (permille, expected) in [(0, 0.0), (500, 0.5), (1_000, 1.0)] {
                s.set_recording_transition_progress(permille);
                assert!((s.recording_motion() - expected).abs() < 0.01);
            }
        }

        fn visual_harness_pause_phase_does_not_drift_with_wall_time() {
            let mut s = CaptureStudio::default();
            s.set_recording_active(true);
            s.set_recording_paused(true);
            s.set_recording_state_transition_progress(500);

            let first = s.recording_state_motion();
            std::thread::sleep(Duration::from_millis(12));
            assert_eq!(first, 0.5);
            assert_eq!(s.recording_state_motion(), first);
        }

        fn visual_harness_receipt_phase_outlives_slow_png_rendering() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            s.set_overlay_rect(s.rect);
            s.set_recording_active(true);
            s.set_recording_finished();
            s.set_recording_receipt_transition_progress(500);
            s.recording_finished_at = Some(crate::time::now() - RECORDING_RECEIPT_DURATION * 2);

            s.draw_recording_indicator(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
            );

            assert!(s.recording_finished_at.is_some());
            assert!(s.is_open());
            assert_eq!(s.recording_receipt_progress_override, Some(0.5));
            assert!(!s.needs_animation_frame());
        }

        visual_harness_can_pin_screenshot_receipt_transition();
        visual_harness_can_pin_each_recording_transition_phase();
        visual_harness_pause_phase_does_not_drift_with_wall_time();
        visual_harness_receipt_phase_outlives_slow_png_rendering();
    }

    #[test]
    fn recording_start_acknowledges_outlines_and_docks() {
        fn recording_start_names_and_outlines_the_exact_source_without_a_scrim() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            s.set_overlay_rect(s.rect);
            s.set_candidates(vec![CaptureTarget {
                id: "notes".into(),
                title: "Notes".into(),
                app_id: "so.tilo.notes".into(),
                visible_rect: Rect::new(240.0, 120.0, 1_280.0, 820.0),
                virtual_rect: Rect::new(-400.0, 120.0, 1_280.0, 820.0),
                complete_rect: None,
            }]);
            s.set_source_mode(SourceMode::Window);
            s.set_extent(CaptureExtent::FullApp);
            s.set_recording_active(true);

            assert_eq!(s.recording_scope, "Notes · app window");
            assert_eq!(s.compact_recording_scope(10), "Notes");
            assert_eq!(s.recording_scope_icon(), Icon::ChevronLeft);
            assert_eq!(
                s.recording_confirmation_regions,
                vec![Rect::new(240.0, 120.0, 1_280.0, 820.0)]
            );
            assert_eq!(s.recording_indicator_rects_at(0.0).0.width(), 304.0);
            assert!(
                s.modal_capture().is_none(),
                "a running recording must never retain the picker scrim"
            );

            s.set_recording_active(false);
            assert!(s.recording_confirmation_regions.is_empty());
            assert_eq!(s.recording_continuation_edges, [false; 4]);
        }

        fn recording_request_is_acknowledged_without_claiming_success_or_blocking_apps() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            s.set_overlay_rect(s.rect);
            s.set_mode(CaptureMode::Recording);
            s.set_source_mode(SourceMode::Display);
            let launch_rect = s.recording_indicator_rects_at(0.0).0;

            s.commit(CaptureDestination::File);

            assert!(s.is_recording_start_pending());
            assert!(!s.is_recording_active());
            assert!(s.is_open());
            assert!(s.modal_capture().is_none(), "Preparing must be non-modal");
            assert!(
                s.test_rects().is_empty(),
                "Preparing has no speculative controls"
            );
            assert_eq!(s.recording_scope, "Screen");
            assert_eq!(s.recording_indicator_rects_at(0.0).0, launch_rect);
            assert_eq!(
                s.take_intents()[0].recording_command,
                Some(RecordingCommand::Start)
            );
            assert!(
                s.is_recording_start_pending(),
                "taking the intent is not compositor success"
            );

            s.open();
            assert!(
                s.is_recording_start_pending(),
                "repeating the shortcut must not reopen the picker"
            );
            s.set_recording_active(true);
            assert!(!s.is_recording_start_pending());
            assert!(s.is_recording_active());
            assert_eq!(
                s.recording_indicator_rects_at(0.0).0,
                launch_rect,
                "Preparing and Recording must hand off in the same geometry"
            );
        }

        fn failed_recording_setup_removes_the_preparing_surface() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            s.set_mode(CaptureMode::Recording);
            s.set_source_mode(SourceMode::Display);
            s.commit(CaptureDestination::File);
            assert!(s.is_recording_start_pending());

            s.set_recording_active(false);

            assert!(!s.is_recording_start_pending());
            assert!(!s.is_recording_active());
            assert!(!s.is_open());
        }

        fn recording_dock_transition_never_loses_its_semantic_label() {
            for step in 0..=100 {
                let progress = f64::from(step) / 100.0;
                let (intro, settled) = CaptureStudio::recording_label_alphas(progress);
                assert!((intro + settled - 1.0).abs() < 0.001);
            }
        }

        fn source_confirmation_yields_to_the_docked_recording_pill() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            s.set_recording_active(true);

            s.set_recording_transition_progress(0);
            assert!(s.recording_scope_confirmation_alpha() > 0.70);
            s.set_recording_transition_progress(500);
            assert!(
                s.recording_scope_confirmation_alpha() < 0.10,
                "source confirmation should be nearly gone halfway to the top bar"
            );
            s.set_recording_transition_progress(1_000);
            assert_eq!(s.recording_scope_confirmation_alpha(), 0.0);
        }

        recording_start_names_and_outlines_the_exact_source_without_a_scrim();
        recording_request_is_acknowledged_without_claiming_success_or_blocking_apps();
        failed_recording_setup_removes_the_preparing_surface();
        recording_dock_transition_never_loses_its_semantic_label();
        source_confirmation_yields_to_the_docked_recording_pill();
    }

    #[test]
    fn top_bar_and_pill_geometry_follow_tokens_and_text_scale() {
        /// The centring helper follows the renderer's measured line box — not
        /// the font size, not a fraction of it — so it holds when the renderer
        /// scales type. `(top + height / 2) == centre` at both scales.
        /// Both top-bar groups hug their buttons: one inset before the first,
        /// one after the last, one gap between neighbours — the source group
        /// used to trail ~50 px of empty pill after its last button and the
        /// mode group had no trailing inset at all.
        fn top_bar_groups_hug_their_buttons_with_one_inset() {
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let groups = [
                (s.mode_group_rect(), s.mode_rects().to_vec()),
                (s.source_group_rect(), s.source_rects().to_vec()),
            ];
            for (group, buttons) in &groups {
                let first = buttons.first().expect("a group has buttons");
                let last = buttons.last().expect("a group has buttons");
                assert!(
                    (first.x0 - group.x0 - TOP_GROUP_PAD).abs() < 1e-9,
                    "leading inset of {group:?} is {}",
                    first.x0 - group.x0
                );
                assert!(
                    (group.x1 - last.x1 - TOP_GROUP_PAD).abs() < 1e-9,
                    "trailing inset of {group:?} is {}",
                    group.x1 - last.x1
                );
                for pair in buttons.windows(2) {
                    assert!((pair[1].x0 - pair[0].x1 - TOP_BUTTON_GAP).abs() < 1e-9);
                }
                for b in buttons {
                    assert!((b.width() - TOP_BUTTON_W).abs() < 1e-9);
                    assert!(
                        b.height() >= 32.0,
                        "{b:?} is under the minimum pointer target"
                    );
                }
            }
            let (mode, source) = (s.mode_group_rect(), s.source_group_rect());
            assert!((source.x0 - mode.x1 - TOP_GROUP_GAP).abs() < 1e-9);
            let bar = s.top_bar();
            assert!((bar.x0 - mode.x0).abs() < 1e-9 && (bar.x1 - source.x1).abs() < 1e-9);
        }

        /// The bar's geometry is the tokens' arithmetic, not a restatement of
        /// it: the buttons come out exactly one `bar_height` tall inside a bar
        /// that is that plus an `s2` inset top and bottom, and the group inset
        /// and gap are the same `s2`.
        fn top_bar_buttons_are_one_bar_height_tall_inside_token_insets() {
            let theme = Theme::dark();
            let tokens = &theme.tokens;
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1400.0, 900.0);
            let bar = s.top_bar();
            let button = s.mode_rects()[0];
            assert!((button.height() - f64::from(tokens.metrics.bar_height)).abs() < 1e-9);
            assert!(
                (bar.height() - button.height() - 2.0 * f64::from(tokens.spacing.s2)).abs() < 1e-9
            );
            assert!((TOP_GROUP_PAD - f64::from(tokens.spacing.s2)).abs() < 1e-9);
            assert!((TOP_GROUP_GAP - f64::from(tokens.spacing.s2)).abs() < 1e-9);
            assert!((TOP_BUTTON_W - bar.height()).abs() < 1e-9);
        }

        /// The docked recorder keeps its designed width at 1× and grows only
        /// when the type does: at 2× the pill widens around its label and the
        /// Stop button around its caption, and every control stays inside.
        fn docked_recording_pill_grows_with_its_content_above_one_x() {
            let mut widths = Vec::new();
            for scale in [1.0_f32, 2.0] {
                // A matched host: the theme's type steps carry the scale the
                // renderer is set to, so captions paint at 2x, not 4x (X1).
                let theme = Theme::dark()
                    .with_reduce_motion(true)
                    .with_text_scale(scale);
                let mut s = CaptureStudio::default();
                s.rect = Rect::new(0.0, 0.0, 1400.0, 900.0);
                s.open();
                s.set_mode(CaptureMode::Recording);
                s.recording_scope = "Terminal · app window".to_string();
                s.set_recording_active(true);
                s.set_recording_transition_progress(1000);
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                let mut scene = Scene::new();
                s.draw_recording_indicator(&mut scene, Affine::IDENTITY, &mut tr, &theme);
                let (pill, pause, stop) = s.recording_indicator_rects();
                widths.push(pill.width());
                assert!(
                    pause.x0 >= pill.x0 && stop.x1 <= pill.x1 && pause.x1 + 3.9 <= stop.x0,
                    "at {scale}x the controls left the pill: {pill:?} {pause:?} {stop:?}"
                );
                let (stop_w, _) = tr.measure_style(
                    "Stop",
                    None,
                    &theme
                        .text(TextRole::Caption)
                        .with_weight(TextWeight::Semibold),
                );
                assert!(
                    stop.width() >= 22.0 + f64::from(stop_w) + 8.0 - 1e-6,
                    "at {scale}x Stop {stop:?} cannot hold its {stop_w}px caption"
                );
            }
            assert!(
                (widths[0] - 252.0).abs() < 1e-9,
                "the 1x pill must keep its designed width, got {}",
                widths[0]
            );
            assert!(
                widths[1] > widths[0],
                "the 2x pill ({}) did not grow around its content",
                widths[1]
            );
        }

        /// The delay count is type, not a glyph decoration: nothing in the
        /// button paints under the floor of the type scale.
        fn delay_count_is_painted_at_the_type_floor_or_above() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            for seconds in [3_u8, 10] {
                let mut scene = Scene::new();
                CaptureStudio::delay_button(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    Rect::new(0.0, 0.0, 36.0, 36.0),
                    seconds,
                    false,
                );
                let runs = &scene.encoding().resources.glyph_runs;
                assert!(!runs.is_empty(), "the count {seconds} was not painted");
                for run in runs {
                    assert!(
                        run.font_size >= theme.tokens.typography.size(TextRole::Caption) - 1e-6,
                        "count {seconds} painted at {}px, under the {}px floor",
                        run.font_size,
                        theme.tokens.typography.size(TextRole::Caption)
                    );
                }
            }
        }

        fn label_centring_tracks_the_measured_line_box_at_every_text_scale() {
            let theme = Theme::dark();
            for scale in [1.0_f32, 2.0] {
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                for role in [TextRole::Caption, TextRole::Label, TextRole::Heading] {
                    let style = theme.text(role).with_weight(TextWeight::Semibold);
                    let center = 100.0;
                    let top = centered_text_top(&mut tr, &style, center);
                    let (_, h) = tr.measure_style("Ag", None, &style);
                    assert!(
                        (top + f64::from(h) / 2.0 - center).abs() < 1e-6,
                        "{role:?} at {scale}x: top {top} + half of {h} misses centre {center}"
                    );
                    // And it is genuinely the line box, not the font size.
                    assert!(
                        f64::from(h) > f64::from(style.size),
                        "measured height {h} should exceed the bare size {}",
                        style.size
                    );
                }
            }
        }

        /// The docked recording pill's timer stays inside the pill when the
        /// renderer doubles type. With the old `centre − 0.52 × sm` top the 2×
        /// label's line box ran out below the pill.
        fn docked_recording_label_stays_inside_its_pill_at_two_x() {
            use crate::offscreen::OffscreenRenderer;
            let theme = Theme::dark();
            let mut s = CaptureStudio::default();
            s.rect = Rect::new(0.0, 0.0, 1400.0, 200.0);
            s.open();
            s.set_mode(CaptureMode::Recording);
            s.set_recording_active(true);
            s.set_recording_transition_progress(1000);
            let (pill, pause, _stop) = s.recording_indicator_rects_at(1.0);

            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let mut scene = Scene::new();
            s.draw_recording_indicator(&mut scene, Affine::IDENTITY, &mut tr, &theme);
            let (w, h) = (1400_u32, 200_u32);
            let rgba = OffscreenRenderer::new(w, h).render(&scene, Color::BLACK);

            // The timer occupies the pill between the state mark and the pause
            // button; look for its light ink in that column band, over a
            // generous vertical window either side of the pill.
            let x_lo = (pill.x0 + 43.0) as usize;
            let x_hi = (pause.x0 - 4.0) as usize;
            let y_lo = (pill.y0 - 30.0).max(0.0) as usize;
            let y_hi = (pill.y1 + 30.0).min(f64::from(h)) as usize;
            let mut ink_rows = Vec::new();
            for y in y_lo..y_hi {
                for x in x_lo..x_hi {
                    let i = (y * w as usize + x) * 4;
                    if rgba[i] > 170 && rgba[i + 1] > 170 && rgba[i + 2] > 170 {
                        ink_rows.push(y);
                        break;
                    }
                }
            }
            let top = *ink_rows.first().expect("the timer painted ink");
            let bottom = *ink_rows.last().expect("the timer painted ink");
            assert!(
                f64::from(top as u32) >= pill.y0.floor()
                    && f64::from(bottom as u32) < pill.y1.ceil(),
                "timer ink rows {top}..={bottom} leave the pill {pill:?} at 2x"
            );
        }

        /// The floating groups and every resting button inside them keep a
        /// **visible derived edge** at 1× on both presets, and neighbouring
        /// buttons keep one `s1` between them (HIG §7, §9.5). Before, the groups
        /// were bare raised fills over the scrim and the buttons a 0.35 lift
        /// 2 px apart; on the light preset the shadow tint was `neutral[0]` —
        /// that palette's paper — so the groups floated with no shadow either.
        ///
        /// Writes `$UX1X_F4_OUT/<theme>/capture_studio.png` plus the harness
        /// inspection when that variable is set; asserts regardless.
        fn ux1x_groups_and_resting_buttons_keep_a_derived_edge_on_both_presets() {
            use crate::offscreen::OffscreenRenderer;
            const EDGE_MIN: f64 = 0.008;
            let (w, h) = (1400.0, 900.0);
            // Evidence first, for both presets, so a failing assertion on one
            // preset still leaves the other's PNG and inspection on disk.
            for theme in [Theme::dark(), Theme::light()] {
                let theme = theme.with_reduce_motion(true);
                if let Some(png) = ux1x_out(&theme, "capture_studio.png") {
                    let harness = crate::headless::FrameHarness::new_with_theme(
                        crate::State::new(),
                        Box::new(ux1x_fixture()),
                        (1400, 900),
                        theme.clone(),
                    );
                    harness.render_png(&png);
                    if let Ok(json) = harness.inspection_json() {
                        std::fs::write(png.with_extension("inspection.json"), json)
                            .expect("write inspection");
                    }
                }
            }
            for theme in [Theme::dark(), Theme::light()] {
                let theme = theme.with_reduce_motion(true);
                let mut s = ux1x_fixture();
                s.rect = Rect::new(0.0, 0.0, w, h);
                let mut tr = TextRenderer::new();
                let presence = PresenceCtx::default();
                let mut scene = Scene::new();
                Widget::draw(
                    &mut s,
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &presence,
                );
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a 1400x900 test surface"
                )]
                let rgba = OffscreenRenderer::new(w as u32, h as u32)
                    .render(&scene, theme.tokens.colors.surfaces.content);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a 1400x900 test surface"
                )]
                let lum = |x: f64, y: f64| lum_at(&rgba, w as u32, x, y);

                // Group: its left edge against its own fill, on the group's centre
                // line (clear of the 14 px corner arcs; the first button starts
                // one `TOP_GROUP_PAD` in, so the columns beside the edge are fill).
                let group = s.mode_group_rect();
                let y = group.center().y;
                let edge = lum(group.x0, y);
                let fill = lum(group.x0 + 3.0, y);
                assert!(
                    (edge - fill).abs() >= EDGE_MIN,
                    "{}: group edge {edge:.4} vs fill {fill:.4} did not register",
                    theme.name
                );

                // Resting button: the second mode button (recording, not the
                // active screenshot one) — its left edge against its own face.
                let button = s.mode_rects()[1];
                let bedge = lum(button.x0, button.center().y);
                let face = lum(button.x0 + 3.0, button.center().y);
                assert!(
                    (bedge - face).abs() >= EDGE_MIN,
                    "{}: button edge {bedge:.4} vs face {face:.4} did not register",
                    theme.name
                );

                // Nothing touches: neighbours keep the smallest token step.
                let s1 = f64::from(theme.tokens.spacing.s1);
                for pair in s.mode_rects().windows(2) {
                    let gap = pair[1].x0 - pair[0].x1;
                    assert!(
                        gap >= s1 - 1e-9,
                        "{}: mode buttons {gap} px apart",
                        theme.name
                    );
                }
                for pair in s.source_rects().windows(2) {
                    let gap = pair[1].x0 - pair[0].x1;
                    assert!(
                        gap >= s1 - 1e-9,
                        "{}: source buttons {gap} px apart",
                        theme.name
                    );
                }
                let (mode, source) = (s.mode_group_rect(), s.source_group_rect());
                assert!(
                    source.x0 - mode.x1 >= s1 - 1e-9,
                    "{}: groups touch",
                    theme.name
                );
            }
        }

        top_bar_groups_hug_their_buttons_with_one_inset();
        top_bar_buttons_are_one_bar_height_tall_inside_token_insets();
        docked_recording_pill_grows_with_its_content_above_one_x();
        delay_count_is_painted_at_the_type_floor_or_above();
        label_centring_tracks_the_measured_line_box_at_every_text_scale();
        docked_recording_label_stays_inside_its_pill_at_two_x();
        ux1x_groups_and_resting_buttons_keep_a_derived_edge_on_both_presets();
    }

    /// Relative luminance of the composited pixel at (`x`, `y`).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "probe coordinates are non-negative pixel positions inside the test surface"
    )]
    fn lum_at(rgba: &[u8], w: u32, x: f64, y: f64) -> f64 {
        let (r, g, b, _) = crate::offscreen::OffscreenRenderer::sample(
            rgba,
            w,
            x.floor() as u32,
            y.floor() as u32,
        );
        crate::a11y::relative_luminance(Color::from_rgb8(r, g, b))
    }

    /// `$UX1X_F4_OUT/<theme>/<name>` when the 1× review asked for evidence.
    fn ux1x_out(theme: &Theme, name: &str) -> Option<std::path::PathBuf> {
        let dir = std::path::PathBuf::from(std::env::var_os("UX1X_F4_OUT")?)
            .join(theme.name.to_lowercase());
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join(name))
    }

    /// The studio open over an empty desktop in screenshot mode: both
    /// top-bar groups, the bottom bar with its option and action buttons.
    fn ux1x_fixture() -> CaptureStudio {
        let mut s = CaptureStudio::default();
        s.open();
        s.set_mode(CaptureMode::Screenshot);
        s.set_source_mode(SourceMode::Window);
        s
    }

    #[test]
    fn send_offers_the_workspace_and_online_devices_and_emits_the_pick() {
        let mut s = CaptureStudio::default();
        s.rect = Rect::new(0.0, 0.0, 1_600.0, 900.0);
        s.set_screenshot_send_devices(vec![ScreenshotSendDevice {
            id: "device-laptop".into(),
            label: "Ada's laptop".into(),
        }]);
        let preview = CapturePreview {
            columns: 1,
            rows: 1,
            pixels: vec![[20, 40, 60, 255]],
        };
        s.set_screenshot_finished(false, 800, 600, preview.clone());
        s.screenshot_receipt.as_mut().unwrap().shown_at -= Duration::from_secs(1);
        let (pill, pin, ..) = s.screenshot_receipt_rects();
        let send = CaptureStudio::screenshot_receipt_send_rect(pin);
        assert!(pill.contains(send.center()), "Send sits inside the receipt");
        assert!(
            s.test_rects()
                .iter()
                .any(|(name, rect)| name == "capture.send-screenshot" && *rect == send)
        );

        s.click(send.center());
        assert!(s.send_menu_open(), "Send opens the target picker");
        assert!(s.take_intents().is_empty(), "nothing is sent before a pick");
        let labels: Vec<String> = s
            .send_menu
            .as_ref()
            .unwrap()
            .items()
            .iter()
            .filter_map(|item| item.label().map(str::to_string))
            .collect();
        assert_eq!(
            labels,
            vec!["Save to this workspace's files", "Send to Ada's laptop"]
        );
        // Picking stays alive past the receipt's normal lifetime.
        s.screenshot_receipt.as_mut().unwrap().shown_at -= SCREENSHOT_RECEIPT_DURATION;
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        s.draw_recording_indicator(&mut scene, Affine::IDENTITY, &mut text, &Theme::dark());
        assert!(s.screenshot_receipt.is_some() && s.send_menu_open());

        let device_row = s.send_menu_targets.iter().position(|target| {
            target == &Some(ScreenshotSendTarget::Device("device-laptop".into()))
        });
        s.choose_send_target(device_row.unwrap());
        let intent = s.take_intents().pop().expect("the pick is an intent");
        assert_eq!(
            intent.screenshot_command,
            Some(ScreenshotCommand::SendLast(ScreenshotSendTarget::Device(
                "device-laptop".into()
            )))
        );
        assert!(intent.regions.is_empty(), "a send never recaptures");
        assert!(!s.is_open() && !s.send_menu_open());

        // Without devices the workspace is still offered, and Escape backs
        // out of the picker without sending.
        s.set_screenshot_send_devices(Vec::new());
        s.set_screenshot_finished(false, 800, 600, preview);
        s.screenshot_receipt.as_mut().unwrap().shown_at -= Duration::from_secs(1);
        let (_, pin, ..) = s.screenshot_receipt_rects();
        s.click(CaptureStudio::screenshot_receipt_send_rect(pin).center());
        assert_eq!(
            s.send_menu_targets
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>(),
            vec![ScreenshotSendTarget::Workspace]
        );
        s.close_send_menu();
        assert!(
            s.screenshot_receipt.is_some(),
            "backing out keeps the receipt"
        );

        s.send_last_screenshot(ScreenshotSendTarget::Workspace);
        assert_eq!(
            s.take_intents().pop().unwrap().screenshot_command,
            Some(ScreenshotCommand::SendLast(ScreenshotSendTarget::Workspace))
        );
    }
}
