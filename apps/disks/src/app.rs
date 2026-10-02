//! Root widget for the Disks app.
//!
//! `DisksApp` is a single self-drawing "canvas" widget: it owns a parsed
//! cache of the [`Topology`] (the source of truth lives in the CRDT) and
//! paints the whole UI — drive list, partition map, capacity bars, SMART
//! summary and the action confirm modal — directly into the Vello scene.
//! All mutable UI state (selection, active tab, pending confirmation) is
//! written back into the Automerge document so it is undoable and
//! collaborative.

use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;
use ui::widget::{AppModel, ModelPath};

use ui::automerge::{AutoCommit, ObjId};
use ui::format::bytes_si;
use ui::taffy::style_helpers::percent;
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use ui::vello::peniko::{Color, Fill};
use ui::winit::dpi::PhysicalPosition;
use ui::winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Politeness, Role};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::paint::with_alpha;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
#[cfg(test)]
use ui::tilo_actions::{ConfirmationPolicy, ExecutionMode, ResultSpec};
use ui::tilo_actions::{OperationKind, Resource};
use ui::widget::{
    Button, ButtonFace, FlapState, SegmentedFace, SplitLayout, TextInput, ToggleGroup, Widget,
};

use crate::doc::DisksDoc;
use crate::model::{
    AttrSeverity, Drive, DriveKind, PartRole, Partition, SmartAttr, SmartInfo, SmartStatus,
    Topology, fmt_hours,
};

/// Panic-free leaf construction for [`Widget::build_layout`]. `new_leaf`
/// always inserts and returns `Ok`; `Widget::build_layout` returns a bare
/// `NodeId` (not a `Result`) across every implementor, so surfacing a
/// theoretical error here would mean threading a `Result` through the whole
/// toolkit for a branch that can't be taken.
#[allow(clippy::unwrap_used, reason = "new_leaf cannot fail; see doc comment")]
fn leaf_node(taffy: &mut TaffyTree<()>, style: Style) -> NodeId {
    taffy.new_leaf(style).unwrap()
}
use crate::ops::{self, Op};

/// Result of a background action thread, drained in `update`.
struct ActionResult {
    text: String,
    error: Option<String>,
    /// Whether the topology changed and should be re-probed.
    refresh: bool,
}

impl ActionKind {
    /// The real operation this action maps to, if any (Eject/Mount/… all do;
    /// this is a total map today but kept explicit for clarity).
    fn op(self) -> Op {
        match self {
            ActionKind::Mount => Op::Mount,
            ActionKind::Unmount => Op::Unmount,
            ActionKind::Format => Op::Format,
            ActionKind::Benchmark => Op::Benchmark,
            ActionKind::Eject => Op::Eject,
        }
    }
    /// Present-tense verb for the "…in progress" status line.
    fn gerund(self) -> &'static str {
        match self {
            ActionKind::Mount => "Mounting",
            ActionKind::Unmount => "Unmounting",
            ActionKind::Format => "Formatting",
            ActionKind::Benchmark => "Benchmarking",
            ActionKind::Eject => "Ejecting",
        }
    }
    /// Does completing this action change the mount/partition topology?
    fn changes_topology(self) -> bool {
        matches!(
            self,
            ActionKind::Mount | ActionKind::Unmount | ActionKind::Format | ActionKind::Eject
        )
    }
}

// ---- layout constants (logical px) ---------------------------------
const PAD: f64 = 18.0;
const TOPBAR_H: f64 = 60.0;
const LIST_W: f64 = 320.0;
/// Height of the "DRIVES" section header sitting above the drive rows.
const LIST_HEADER_H: f64 = 30.0;
const CARD_H: f64 = 96.0;
const CARD_GAP: f64 = 12.0;
/// Inner left/right inset shared by every detail card — the drive rows, the
/// identity header and the action band — so their text, badges and buttons
/// sit on one vertical line. Deliberately off the 4px scale (14, not 12/16):
/// it is the value the drive-card badge and usage-bar geometry were drawn to.
const CARD_INSET: f64 = 14.0;
/// Identity header (model + meta + chips). Actions moved to their own band.
const HEADER_H: f64 = 96.0;
/// The dedicated action band below the identity header: a target label plus
/// the Mount / Unmount / Format / Benchmark / Eject buttons and a helper line.
const ACTIONBAR_H: f64 = 94.0;
const TAB_H: f64 = 40.0;

/// Below this allocated width (logical px) the drive list stops fitting
/// beside a readable detail pane, so it collapses from a docked column into
/// an overlay flap summoned by the hamburger. Matches the shared toolkit's
/// `DEFAULT_BREAKPOINT`.
const BREAKPOINT: f64 = 900.0;
/// The full slide width of the overlay drive list (its column plus the
/// padding gutters either side), so the eased slide and scrim line up.
const SIDEBAR_W: f64 = PAD + LIST_W + PAD;

/// The theme's type ramp in the renderer's input space. Theme type tokens
/// already carry the accessibility text scale and the size-based renderer
/// APIs (`measure_weighted`, `draw_text_weighted`) apply that scale once
/// more, so every label size is divided by the renderer's scale here — once —
/// and paints at 2× at 200 %, never 4×. Refreshed at the top of every `draw`
/// from the renderer that will paint the frame, and read by the painters
/// (several are associated functions, so a thread-local rather than a field).
#[derive(Clone, Copy, Debug, PartialEq)]
struct TypeRamp {
    xs: f32,
    sm: f32,
    base: f32,
    lg: f32,
    xl: f32,
    x2l: f32,
}

impl TypeRamp {
    /// The default ramp at 1× — what the painters use until the first frame.
    const ONE: Self = Self {
        xs: 10.0,
        sm: 12.0,
        base: 14.0,
        lg: 16.0,
        xl: 20.0,
        x2l: 24.0,
    };

    fn resolve(tr: &TextRenderer, theme: &Theme) -> Self {
        let rel = |resolved: f32| tr.renderer_input_style(&TextStyle::new(resolved)).size;
        let ty = &theme.tokens.typography.scale;
        Self {
            xs: rel(ty.xs),
            sm: rel(ty.sm),
            base: rel(ty.base),
            lg: rel(ty.lg),
            xl: rel(ty.xl),
            x2l: rel(ty.x2l),
        }
    }
}

std::thread_local! {
    static TYPE_RAMP: std::cell::Cell<TypeRamp> = const { std::cell::Cell::new(TypeRamp::ONE) };
}

/// This frame's type ramp (see [`TypeRamp`]).
fn ty() -> TypeRamp {
    TYPE_RAMP.with(std::cell::Cell::get)
}

/// Refresh the frame's type ramp from the painting renderer and theme.
fn refresh_type_ramp(tr: &TextRenderer, theme: &Theme) {
    TYPE_RAMP.with(|c| c.set(TypeRamp::resolve(tr, theme)));
}

/// The renderer's accessibility text scale, sanitised for layout arithmetic.
fn renderer_scale(tr: &TextRenderer) -> f64 {
    let scale = f64::from(tr.text_scale());
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Smart,
    Partitions,
}

impl Tab {
    fn all() -> [Tab; 3] {
        [Tab::Overview, Tab::Smart, Tab::Partitions]
    }
    fn key(self) -> &'static str {
        match self {
            Tab::Overview => "overview",
            Tab::Smart => "smart",
            Tab::Partitions => "partitions",
        }
    }
    fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Smart => "SMART & Health",
            Tab::Partitions => "Partitions",
        }
    }
    fn from_key(k: &str) -> Tab {
        match k {
            "smart" => Tab::Smart,
            "partitions" => Tab::Partitions,
            _ => Tab::Overview,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    Mount,
    Unmount,
    Format,
    Benchmark,
    Eject,
}

impl ActionKind {
    /// The action bar, left-to-right.
    fn bar() -> [ActionKind; 5] {
        [
            ActionKind::Mount,
            ActionKind::Unmount,
            ActionKind::Format,
            ActionKind::Benchmark,
            ActionKind::Eject,
        ]
    }
    pub(crate) fn key(self) -> &'static str {
        match self {
            ActionKind::Mount => "mount",
            ActionKind::Unmount => "unmount",
            ActionKind::Format => "format",
            ActionKind::Benchmark => "benchmark",
            ActionKind::Eject => "eject",
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            ActionKind::Mount => "Mount",
            ActionKind::Unmount => "Unmount",
            ActionKind::Format => "Format…",
            ActionKind::Benchmark => "Benchmark",
            ActionKind::Eject => "Eject",
        }
    }
    /// One-line description of what the action does, shown under the action
    /// bar so every control is self-labelling (no unexplained buttons).
    fn describe(self) -> &'static str {
        match self {
            ActionKind::Mount => "Attach the filesystem so its files become reachable.",
            ActionKind::Unmount => "Safely detach the filesystem before removing the device.",
            ActionKind::Format => "Erase and create a new filesystem — destroys all data.",
            ActionKind::Benchmark => "Measure sequential read / write throughput.",
            ActionKind::Eject => "Power down and safely remove the whole device.",
        }
    }
    pub(crate) fn from_key(k: &str) -> Option<ActionKind> {
        match k {
            "mount" => Some(ActionKind::Mount),
            "unmount" => Some(ActionKind::Unmount),
            "format" => Some(ActionKind::Format),
            "benchmark" => Some(ActionKind::Benchmark),
            "eject" => Some(ActionKind::Eject),
            _ => None,
        }
    }
    /// Destructive actions require an explicit confirm dialog.
    pub(crate) fn destructive(self) -> bool {
        matches!(self, ActionKind::Format)
    }
    fn confirm_title(self, target: &str) -> String {
        match self {
            ActionKind::Format => format!("Format {target}?"),
            ActionKind::Unmount => format!("Unmount {target}?"),
            ActionKind::Mount => format!("Mount {target}?"),
            ActionKind::Benchmark => format!("Benchmark {target}?"),
            ActionKind::Eject => format!("Eject {target}?"),
        }
    }
    fn confirm_body(self, target: &str, live: bool) -> String {
        match self {
            // In the live binary Format really erases the device; only the
            // hermetic demo/--shots build is a no-op, so the "nothing is
            // written" reassurance must never appear when we can actually
            // destroy data.
            ActionKind::Format if live => {
                format!("Formatting erases ALL data on {target}. This cannot be undone.")
            }
            ActionKind::Format => format!(
                "Formatting erases ALL data on {target}. This cannot be undone. \
                 (Demo build — no changes are written to disk.)"
            ),
            ActionKind::Unmount => {
                format!("Detach {target} from the filesystem. Programs using it may lose access.")
            }
            ActionKind::Mount => format!("Attach {target} so its files become available."),
            ActionKind::Benchmark => format!(
                "Run a read/write throughput benchmark on {target}. May briefly slow the system."
            ),
            ActionKind::Eject => format!("Power down and detach {target} so it is safe to unplug."),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    action: ActionKind,
    drive: usize,
    /// Partition the action targets, or `None` for the whole disk.
    part: Option<usize>,
}

/// Computed interactive rectangles for one frame. Pure function of the
/// viewport + current state, so `draw` and `handle_event` agree without
/// any ordering hazard.
struct Layout {
    /// How the drive list relates to the detail at this width: a permanent
    /// docked column (wide) or an overlay flap over the content (narrow).
    split: SplitLayout,
    /// The drive-list panel rect — its column plus gutters. In overlay mode
    /// this slides in from the left edge under the eased reveal.
    sidebar: Rect,
    /// Dimming scrim behind the open overlay flap (`ZERO` when not overlaid).
    scrim: Rect,
    /// Hamburger toggle in the top bar (only painted/hit in overlay mode).
    hamburger: Option<Rect>,
    /// Header label + clipped scroll viewport for the drive rows.
    list_header: Rect,
    list_body: Rect,
    refresh: Rect,
    cards: Vec<Rect>,
    header: Rect,
    /// The action band: its bounding rect plus each button with its
    /// applicability flag (disabled buttons are dimmed and non-interactive).
    action_bar: Rect,
    action_buttons: Vec<(Rect, ActionKind, bool)>,
    tabs: Vec<(Rect, Tab)>,
    content: Rect,
    /// The pill that reselects "Whole disk" (clears the partition selection).
    whole_disk_pill: Rect,
    /// Clickable partition segments in the Overview map (index into partitions).
    part_segments: Vec<(Rect, usize)>,
    /// Clickable partition rows in the Partitions table (index into partitions).
    part_rows: Vec<(Rect, usize)>,
    /// Rescan button in the empty state (only present when no drives exist).
    empty_rescan: Option<Rect>,
    modal: Option<ModalLayout>,
}

#[derive(Clone, Copy)]
struct ModalLayout {
    card: Rect,
    /// Type-to-confirm input box (destructive actions only).
    input: Rect,
    cancel: Rect,
    confirm: Rect,
}

/// One partition's slot in the partitions table: its position in the list,
/// the row's text line, and its selection/hover state.
#[derive(Clone, Copy)]
struct PartitionRow<'a> {
    index: usize,
    partition: &'a Partition,
    y: f64,
    selected: bool,
    hover: bool,
}

/// Column x-positions for the partitions tab's table, resolved once from
/// `area` and shared by the header and every row.
#[derive(Clone, Copy)]
struct PartitionColumns {
    name: f64,
    label: f64,
    fs: f64,
    size: f64,
    mount: f64,
    used: f64,
}

pub struct DisksApp {
    topo: Topology,
    topo_json_cache: String,
    selected: usize,
    /// Selected partition within the current drive, or `None` for the whole
    /// disk. Drives every partition-scoped action + detail highlight.
    selected_part: Option<usize>,
    tab: Tab,
    pending: Option<Pending>,
    /// Typed confirmation text for a destructive (Format) action. The Erase
    /// button only unlocks once this matches the target device name.
    confirm_input: TextInput,
    status: String,
    /// True in the live binary (`DISKS_LIVE`): actions actually execute.
    /// False for `--shots` and unit tests, which stay inert.
    live: bool,
    /// Receiver for the currently-running background action, if any.
    action_rx: Option<mpsc::Receiver<ActionResult>>,
    action_invocation: Option<tilo_actions::InvocationId>,
    completed_action: Option<tilo_actions::InvocationUpdate>,
    /// One-time first-frame setup: the live seed and the initial layout
    /// settle. Grouped so they don't pile up as same-shaped bools next to
    /// `dirty` and `live`.
    init: InitState,
    rect: Rect,
    dirty: bool,
    last_mouse: Point,
    /// Vertical scroll offsets (logical px) for the drive list and the detail
    /// content, so both panes stay usable at small window sizes.
    list_scroll: f64,
    content_scroll: f64,
    /// Content extents measured during the last draw, used to clamp scrolling.
    list_h: f64,
    content_h: f64,
    hover_card: Option<usize>,
    hover_action: Option<ActionKind>,
    hover_tab: Option<Tab>,
    /// Hovered partition (map segment or table row), for a live highlight.
    hover_part: Option<usize>,
    /// Eased open/closed state of the drive-list flap. Open by default when
    /// the list is docked (wide window); closed by default when it collapses
    /// to an overlay (narrow window). The same kernel the shared `NavSplit`
    /// widget drives — adopted directly here because the whole app is one
    /// self-drawing canvas rather than a tree of `ui` elements.
    flap: FlapState,
    /// The renderer's accessibility text scale. Every band designed at 1×
    /// (top bar, drive cards, detail header, action band, tabs) and every
    /// offset inside those bands scales by it. Set by `Widget::measure` and
    /// `draw` from the renderer that paints, so the layout — computed without
    /// a theme on the event path — and the paint agree.
    k: f64,
    /// Hamburger-toggle and whole-disk-pill hover, checked together each
    /// frame when resolving the overlay's hit targets.
    hover_toggle: HoverToggles,
    shared: crate::resource::ActionSink,
}

/// One-time first-frame setup flags for [`DisksApp`] (part of `DisksApp`).
#[derive(Default)]
struct InitState {
    seeded: bool,
    /// `false` until the first layout pass resolves the width. Used to settle
    /// the flap to its resolved default on the very first frame (so a cold
    /// start at a narrow width opens straight to the detail, with no flash of
    /// the list sliding out).
    laid_out: bool,
}

/// Hover state for the overlay hamburger toggle and the whole-disk pill
/// (part of [`DisksApp`]).
#[derive(Default)]
struct HoverToggles {
    /// Whether a hamburger toggle is hovered (overlay mode only).
    menu: bool,
    whole_disk: bool,
}

impl Default for DisksApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Whole-disk pill, partition map segments, and partition table row rects
/// returned by [`DisksApp::compute_content_rects`].
type ContentRects = (Rect, Vec<(Rect, usize)>, Vec<(Rect, usize)>);

impl DisksApp {
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Disks>,
    ) {
        self.shared.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.shared.is_hosted()
    }

    /// Test-only: mark the surface hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.shared.mark_hosted_for_test();
    }

    /// Standalone / `--shots` / unit tests: one tree from [`tilo_resource::publish`].
    pub(crate) fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        use ui::tilo_resource::Resource as _;
        let mut state = crate::resource::Disks::read(doc).unwrap_or_default();
        // Prefer the widget's live topology when the document has not been
        // seeded yet (first-frame tests before sync).
        if state.topology.drives.is_empty() && !self.topo.drives.is_empty() {
            state.topology = self.topo.clone();
        }
        state.live = self.live;
        vec![ui::tilo_resource::publish::<crate::resource::Disks>(
            "disks",
            "Storage devices",
            &state,
        )]
    }

    pub(crate) fn adopt_shared(&mut self, state: &crate::resource::DisksState) {
        if let Ok(json) = serde_json::to_string(&state.topology)
            && json != self.topo_json_cache
        {
            self.topo = state.topology.clone();
            self.topo_json_cache = json;
            self.dirty = true;
        }
        if self.selected != state.selected {
            self.selected = state.selected;
            self.dirty = true;
        }
        if self.selected_part != state.selected_part {
            self.selected_part = state.selected_part;
            self.dirty = true;
        }
        let tab = Tab::from_key(&state.tab);
        if self.tab != tab {
            self.tab = tab;
            self.dirty = true;
        }
        if self.status != state.status {
            self.status.clone_from(&state.status);
            self.dirty = true;
        }
    }

    fn commit_shared(&mut self, doc: &mut AutoCommit, action: &crate::resource::DisksAction) {
        self.shared.dispatch(doc, action);
    }

    fn commit_topology(&mut self, doc: &mut AutoCommit, topo: &Topology) {
        let json = serde_json::to_string(topo).unwrap_or_else(|_| "{\"drives\":[]}".into());
        self.commit_shared(doc, &crate::resource::DisksAction::WriteTopology { json });
    }

    pub fn new() -> Self {
        Self {
            topo: Topology::default(),
            topo_json_cache: String::new(),
            selected: 0,
            selected_part: None,
            tab: Tab::Overview,
            pending: None,
            confirm_input: TextInput::local().with_font_size(14.0),
            status: String::new(),
            live: std::env::var("DISKS_LIVE").is_ok(),
            action_rx: None,
            action_invocation: None,
            completed_action: None,
            init: InitState::default(),
            rect: Rect::ZERO,
            dirty: true,
            last_mouse: Point::ZERO,
            list_scroll: 0.0,
            content_scroll: 0.0,
            list_h: 0.0,
            content_h: 0.0,
            hover_card: None,
            hover_action: None,
            hover_tab: None,
            hover_part: None,
            flap: FlapState::open_when_docked(),
            k: 1.0,
            hover_toggle: HoverToggles::default(),
            shared: crate::resource::ActionSink::default(),
        }
    }

    /// The drive-list layout at the current width: a docked column beside the
    /// detail (wide) or an overlay flap over it (narrow).
    fn split_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), BREAKPOINT)
    }

    /// Force the drive-list flap to a settled open/closed state with no
    /// animation. The single-frame `--shots` renderer calls this so a narrow
    /// capture shows the overlay flap fully revealed over its scrim (rather
    /// than a frame caught mid-slide).
    pub fn snap_sidebar(&mut self, open: bool) {
        self.flap.snap(open);
        self.dirty = true;
    }

    /// Whether the drive list is currently open (resolving the user's
    /// preference against the layout default).
    fn list_open(&self) -> bool {
        self.flap.is_open(self.split_layout())
    }

    fn drive(&self) -> Option<&Drive> {
        self.topo.drives.get(self.selected)
    }

    /// The selected partition index, validated against the current drive so a
    /// stale index (after switching drives) never points out of bounds.
    fn part_index(&self) -> Option<usize> {
        let n = self.drive()?.partitions.len();
        self.selected_part.filter(|&i| i < n)
    }

    /// The selected partition, if any.
    fn part(&self) -> Option<&Partition> {
        let d = self.drive()?;
        self.part_index().and_then(|i| d.partitions.get(i))
    }

    /// Which partition Mount / Unmount act on: the explicitly-selected one,
    /// or — when the whole disk is selected and it carries exactly one
    /// filesystem — that sole partition, so single-fs drives (USB sticks,
    /// data disks) mount without first drilling into the map.
    fn mount_target_part(&self, drive: &Drive) -> Option<usize> {
        if let Some(i) = self.part_index() {
            return Some(i);
        }
        if drive.partitions.len() == 1 {
            Some(0)
        } else {
            None
        }
    }

    /// Whether an action applies to the current selection. Inapplicable
    /// actions render disabled rather than silently no-op'ing.
    fn action_enabled(&self, drive: &Drive, action: ActionKind) -> bool {
        match action {
            ActionKind::Mount => self
                .mount_target_part(drive)
                .and_then(|i| drive.partitions.get(i))
                .is_some_and(|p| p.mountpoint.is_none()),
            ActionKind::Unmount => self
                .mount_target_part(drive)
                .and_then(|i| drive.partitions.get(i))
                .is_some_and(|p| p.mountpoint.is_some()),
            // Format works on either the selected partition or the whole disk.
            ActionKind::Format | ActionKind::Benchmark => true,
            ActionKind::Eject => drive.removable,
        }
    }

    // ---- layout --------------------------------------------------------
    fn compute_layout(&self) -> Layout {
        let v = self.rect;
        let refresh = Rect::new(
            v.x1 - PAD - 110.0 * self.k,
            ((TOPBAR_H * self.k) - 32.0 * self.k) / 2.0,
            v.x1 - PAD,
            f64::midpoint(TOPBAR_H * self.k, 32.0 * self.k),
        );

        let (split, sidebar, scrim, hamburger, list_x0, detail_x0) = self.compute_split(v);
        let (list_header, list_body, cards) = self.compute_drive_list_layout(list_x0, v);

        let dx0 = detail_x0;
        let dx1 = v.x1 - PAD;
        let region_top = (TOPBAR_H * self.k) + PAD;
        let header = Rect::new(dx0, region_top, dx1, region_top + (HEADER_H * self.k));

        let (action_bar, action_buttons) = self.compute_action_bar_layout(dx0, dx1, header);
        let (tabs, tab_top) = Self::compute_tabs_layout(dx0, action_bar, self.k);
        let content = Rect::new(dx0, tab_top + (TAB_H * self.k) + PAD, dx1, v.y1 - PAD);
        let (whole_disk_pill, part_segments, part_rows) = self.compute_content_rects(content);
        let modal = self.compute_modal_layout(v);

        // Empty-state Rescan button, only when there is nothing to show.
        let empty_rescan = if self.topo.drives.is_empty() {
            Some(empty_state_rects(v, header).1)
        } else {
            None
        };

        Layout {
            split,
            sidebar,
            scrim,
            hamburger,
            list_header,
            list_body,
            refresh,
            cards,
            header,
            action_bar,
            action_buttons,
            tabs,
            content,
            whole_disk_pill,
            part_segments,
            part_rows,
            empty_rescan,
            modal,
        }
    }

    /// Resolve the responsive split (part of [`Self::compute_layout`]): in
    /// a wide window the drive list is a permanent column beside the
    /// detail; in a narrow window it collapses to an overlay flap that the
    /// hamburger summons over the full-width detail.
    fn compute_split(&self, v: Rect) -> (SplitLayout, Rect, Rect, Option<Rect>, f64, f64) {
        let split = self.split_layout();
        match split {
            SplitLayout::Docked => {
                // The list column eases in/out by the reveal so toggling it is
                // smooth; the detail follows the list's trailing edge.
                let inset = SIDEBAR_W * self.flap.reveal();
                let sidebar = Rect::new(v.x0, v.y0, v.x0 + inset, v.y1);
                let detail_x0 = v.x0 + inset + if inset > 0.5 { 0.0 } else { PAD };
                (
                    split,
                    sidebar,
                    Rect::ZERO,
                    None,
                    v.x0 + PAD,
                    detail_x0.max(v.x0 + PAD),
                )
            }
            SplitLayout::Overlay => {
                // Detail spans the full width; the list slides in from the
                // left over a scrim. The hamburger lives in the top bar.
                let slide = self.flap.slide();
                let panel_x0 = v.x0 - SIDEBAR_W * (1.0 - slide);
                let sidebar = Rect::new(panel_x0, v.y0, panel_x0 + SIDEBAR_W, v.y1);
                let scrim = if self.flap.reveal() > 0.001 {
                    v
                } else {
                    Rect::ZERO
                };
                let ham = Rect::new(
                    PAD,
                    ((TOPBAR_H * self.k) - 30.0) / 2.0,
                    PAD + 38.0,
                    f64::midpoint(TOPBAR_H * self.k, 30.0),
                );
                (split, sidebar, scrim, Some(ham), panel_x0 + PAD, v.x0 + PAD)
            }
        }
    }

    /// The drive list's "DRIVES" section header, scrolling body, and
    /// per-drive card rects, anchored to the (possibly sliding) list column
    /// (part of [`Self::compute_layout`]).
    fn compute_drive_list_layout(&self, list_x0: f64, v: Rect) -> (Rect, Rect, Vec<Rect>) {
        let list_header = Rect::new(
            list_x0,
            (TOPBAR_H * self.k) + PAD,
            list_x0 + LIST_W,
            (TOPBAR_H * self.k) + PAD + (LIST_HEADER_H * self.k),
        );
        let list_body = Rect::new(list_x0, list_header.y1, list_x0 + LIST_W, v.y1 - PAD);
        let mut cards = Vec::new();
        let mut cy = list_body.y0 - self.list_scroll;
        for _ in &self.topo.drives {
            cards.push(Rect::new(
                list_x0,
                cy,
                list_x0 + LIST_W,
                cy + (CARD_H * self.k),
            ));
            cy += (CARD_H * self.k) + CARD_GAP;
        }
        (list_header, list_body, cards)
    }

    /// The action band rects — Mount / Unmount / Format / Benchmark / Eject,
    /// left-to-right below the identity header, each flagged applicable or
    /// not for the current drive/partition selection (part of
    /// [`Self::compute_layout`]).
    fn compute_action_bar_layout(
        &self,
        dx0: f64,
        dx1: f64,
        header: Rect,
    ) -> (Rect, Vec<(Rect, ActionKind, bool)>) {
        let action_bar = Rect::new(
            dx0,
            header.y1 + 12.0,
            dx1,
            header.y1 + 12.0 + (ACTIONBAR_H * self.k),
        );
        let mut action_buttons = Vec::new();
        let drive_ref = self.drive();
        let btn_h = 32.0 * self.k;
        let btn_y = action_bar.y0 + 34.0 * self.k;
        let mut bx = action_bar.x0 + CARD_INSET;
        for a in ActionKind::bar() {
            let w = ((a.label().len() as f64) * 8.0 + 30.0) * self.k;
            let enabled = drive_ref.is_some_and(|d| self.action_enabled(d, a));
            action_buttons.push((Rect::new(bx, btn_y, bx + w, btn_y + btn_h), a, enabled));
            bx += w + 10.0;
        }
        (action_bar, action_buttons)
    }

    /// The tab bar rects, below the action band (part of
    /// [`Self::compute_layout`]).
    fn compute_tabs_layout(dx0: f64, action_bar: Rect, k: f64) -> (Vec<(Rect, Tab)>, f64) {
        let tab_top = action_bar.y1 + 12.0;
        let mut tabs = Vec::new();
        let mut tx = dx0;
        for t in Tab::all() {
            // A glyph budget designed at 1×, scaled with the text like the label
            // the shared segmented face paints into it.
            let w = ((t.title().len() as f64) * 8.2 + 28.0) * k;
            tabs.push((Rect::new(tx, tab_top, tx + w, tab_top + (TAB_H * k)), t));
            tx += w + 6.0;
        }
        (tabs, tab_top)
    }

    /// Content-scoped interactive rects — the whole-disk pill, partition
    /// map segments, and partition table rows — computed in *unscrolled*
    /// content coordinates; hit-testing offsets the pointer by
    /// `content_scroll` (part of [`Self::compute_layout`]).
    fn compute_content_rects(&self, content: Rect) -> ContentRects {
        let pill_w = 116.0;
        let whole_disk_pill = Rect::new(
            content.x1 - pill_w,
            content.y0 - 3.0,
            content.x1,
            content.y0 + 19.0,
        );
        let mut part_segments = Vec::new();
        let mut part_rows = Vec::new();
        if let Some(d) = self.drive() {
            if self.tab == Tab::Overview {
                let map = Rect::new(
                    content.x0,
                    content.y0 + 20.0,
                    content.x1,
                    content.y0 + 20.0 + 58.0,
                );
                for (r, idx) in partition_segments(d, map) {
                    if let Some(i) = idx {
                        part_segments.push((r, i));
                    }
                }
            } else if self.tab == Tab::Partitions {
                let mut ry = content.y0 + 24.0;
                for i in 0..d.partitions.len() {
                    part_rows.push((Rect::new(content.x0, ry - 6.0, content.x1, ry + 30.0), i));
                    ry += 38.0;
                }
            }
        }
        (whole_disk_pill, part_segments, part_rows)
    }

    /// The confirm-modal rects (type-to-confirm for destructive actions), if
    /// a [`Pending`] action is awaiting confirmation (part of
    /// [`Self::compute_layout`]).
    fn compute_modal_layout(&self, v: Rect) -> Option<ModalLayout> {
        self.pending.map(|_| {
            // A fixed desktop dialog made the card overflow on phone-width
            // windows. Keep a comfortable 12px edge gutter while allowing the
            // same confirmation workflow at every supported viewport width.
            let w = (v.width() - 24.0).clamp(1.0, 480.0);
            let h = 258.0;
            let cx = f64::midpoint(v.x0, v.x1);
            let cy = f64::midpoint(v.y0, v.y1);
            let card = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
            let input = Rect::new(
                card.x0 + 22.0,
                card.y0 + 128.0,
                card.x1 - 22.0,
                card.y0 + 128.0 + 36.0,
            );
            let by = card.y1 - 18.0 - 36.0;
            let confirm = Rect::new(card.x1 - 18.0 - 130.0, by, card.x1 - 18.0, by + 36.0);
            let cancel = Rect::new(confirm.x0 - 12.0 - 110.0, by, confirm.x0 - 12.0, by + 36.0);
            ModalLayout {
                card,
                input,
                cancel,
                confirm,
            }
        })
    }

    // ---- drawing primitives -------------------------------------------
    fn txt(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        s: &str,
        style: &TextStyle,
        color: Color,
        origin: Point,
        max_w: Option<f32>,
    ) {
        tr.draw_style(
            scene,
            s,
            color,
            Affine::translate(origin.to_vec2()),
            max_w,
            style,
        );
    }

    fn txt_right(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        s: &str,
        style: &TextStyle,
        color: Color,
        top_right: Point,
    ) {
        let (w, _) = tr.measure_style(s, None, style);
        let origin = Point::new(top_right.x - f64::from(w), top_right.y);
        Self::txt(scene, tr, s, style, color, origin, None);
    }

    fn txt_center(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        s: &str,
        style: &TextStyle,
        color: Color,
        top_center: Point,
    ) {
        let (w, _) = tr.measure_style(s, None, style);
        let origin = Point::new(top_center.x - f64::from(w) / 2.0, top_center.y);
        Self::txt(scene, tr, s, style, color, origin, None);
    }

    fn fill_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color) {
        scene.fill(
            Fill::NonZero,
            t,
            c,
            None,
            &RoundedRect::from_rect(r, radius),
        );
    }
    fn stroke_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color, w: f64) {
        scene.stroke(
            &Stroke::new(w),
            t,
            c,
            None,
            &RoundedRect::from_rect(r, radius),
        );
    }

    /// Track + filled bar. `frac` in 0..1.
    fn bar(scene: &mut Scene, t: Affine, r: Rect, frac: f64, track: Color, fill: Color) {
        let radius = (r.height() / 2.0).min(6.0);
        Self::fill_rr(scene, t, r, radius, track);
        let frac = frac.clamp(0.0, 1.0);
        if frac > 0.0 {
            let w = (r.width() * frac).max(radius * 2.0).min(r.width());
            let fr = Rect::new(r.x0, r.y0, r.x0 + w, r.y1);
            Self::fill_rr(scene, t, fr, radius, fill);
        }
    }

    // ---- top bar -------------------------------------------------------
    fn draw_topbar(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, TOPBAR_H * self.k);
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &bar);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::chrome::chrome_hairline(theme),
            None,
            &Line::new(Point::new(bar.x0, bar.y1), Point::new(bar.x1, bar.y1)),
        );

        // Hamburger toggle (narrow window only): summons the drive-list flap.
        // It nudges the title rightward so the two never collide.
        let title_x = if let Some(hb) = lay.hamburger {
            let hov = self.hover_toggle.menu;
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(hb, "")
                    .icon(Icon::Menu)
                    .accessible_name("Show drives")
                    .hovered(hov),
            );
            hb.x1 + 12.0
        } else {
            PAD
        };
        // The OS owns the window title/app-name — the header shows only the
        // contextual drive summary (functional status, not an app wordmark).
        let summary = if lay.hamburger.is_some() {
            // The full summary collides with Rescan on phone widths. The
            // compact form still conveys count and capacity, while all usage
            // detail remains one activation away in the drive list.
            format!(
                "{} drives · {}",
                self.topo.drives.len(),
                bytes_si(self.topo.total_size()),
            )
        } else {
            format!(
                "{} drives · {} total · {} in use",
                self.topo.drives.len(),
                bytes_si(self.topo.total_size()),
                bytes_si(self.topo.total_used()),
            )
        };
        Self::txt(
            scene,
            tr,
            &summary,
            &TextStyle::new(ty().base).with_weight(TextWeight::Medium),
            c.text_primary,
            Point::new(title_x, 18.0),
            Some((lay.refresh.x0 - title_x - 12.0).max(0.0) as f32),
        );

        // Refresh button.
        let r = lay.refresh;
        let hov = r.contains(self.last_mouse);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(r, "Rescan")
                .icon(Icon::Refresh)
                .text_size(13.0)
                .hovered(hov),
        );
    }

    // ---- drive list ----------------------------------------------------
    fn draw_drive_list(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        // Section header — anchors the column as an explicit, pickable list.
        self.draw_drive_list_header(scene, tr, theme, lay.list_header);

        // Rows, clipped to the scrolling body so a long list can't spill over
        // the header or the window edge.
        scene.push_layer(
            Fill::NonZero,
            ui::vello::peniko::BlendMode::default(),
            1.0,
            t,
            &lay.list_body,
        );
        for (i, drive) in self.topo.drives.iter().enumerate() {
            let Some(&r) = lay.cards.get(i) else { continue };
            if r.y1 < lay.list_body.y0 || r.y0 > lay.list_body.y1 {
                continue; // fully scrolled out of view
            }
            let selected = i == self.selected;
            let hover = self.hover_card == Some(i);
            Self::draw_drive_row(
                &mut PaintCtx::new(scene, t, tr, theme),
                r,
                drive,
                selected,
                hover,
                self.k,
            );
        }
        scene.pop_layer();
    }

    /// The "DRIVES" section header + drive count (part of
    /// [`Self::draw_drive_list`]).
    fn draw_drive_list_header(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        hr: Rect,
    ) {
        let c = &theme.tokens.colors;
        Self::txt(
            scene,
            tr,
            "DRIVES",
            &TextStyle::new(ty().sm).with_weight(TextWeight::Bold),
            c.text_dim,
            Point::new(hr.x0 + 2.0, hr.y0 + 8.0),
            None,
        );
        Self::txt_right(
            scene,
            tr,
            &format!("{}", self.topo.drives.len()),
            &TextStyle::new(ty().sm).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(hr.x1 - 2.0, hr.y0 + 8.0),
        );
    }

    /// One drive card: kind badge, model/name/size, usage bar, and health
    /// dot (part of [`Self::draw_drive_list`]).
    fn draw_drive_row(
        ctx: &mut PaintCtx<'_>,
        r: Rect,
        drive: &Drive,
        selected: bool,
        hover: bool,
        k: f64,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let bg = if selected {
            with_alpha(theme.accent_primary, 46)
        } else if hover {
            c.neutral[3]
        } else {
            c.surfaces.raised
        };
        let card_radius = f64::from(theme.tokens.radii.lg);
        ui::surface::card(ctx.scene, ctx.transform, r, card_radius, bg);
        if selected {
            Self::stroke_rr(
                ctx.scene,
                ctx.transform,
                r,
                card_radius,
                theme.accent_primary,
                2.0,
            );
        }
        // Left accent bar — the unmistakable "this row is selected" cue.
        if selected {
            Self::fill_rr(
                ctx.scene,
                ctx.transform,
                Rect::new(r.x0, r.y0 + 10.0, r.x0 + 4.0, r.y1 - 10.0),
                2.0,
                theme.accent_primary,
            );
        }

        let inset = if selected { 8.0 } else { 0.0 };
        // Kind badge.
        let badge = Rect::new(
            r.x0 + CARD_INSET + inset,
            r.y0 + 14.0 * k,
            r.x0 + CARD_INSET + inset + 52.0 * k,
            r.y0 + (14.0 + 22.0) * k,
        );
        let badge_fill = kind_color(theme, drive.kind);
        Self::fill_rr(ctx.scene, ctx.transform, badge, 6.0, badge_fill);
        Self::txt_center(
            ctx.scene,
            ctx.text,
            drive.kind.tag(),
            &TextStyle::new(ty().sm).with_weight(TextWeight::Bold),
            c.on_surface(badge_fill).ink,
            Point::new(f64::midpoint(badge.x0, badge.x1), badge.y0 + 4.0 * k),
        );

        // Model + name + size.
        let tx = r.x0 + CARD_INSET + inset;
        // Elided to the card's width rather than wrapped, so a long model name
        // never drops onto the device line beneath it.
        let model = ctx.text.elide_to_width_weighted(
            &drive.model,
            ty().base,
            (r.width() - 28.0 - inset) as f32,
            TextWeight::Semibold,
        );
        Self::txt(
            ctx.scene,
            ctx.text,
            &model,
            &TextStyle::new(ty().base).with_weight(TextWeight::Semibold),
            c.text_primary,
            Point::new(tx, r.y0 + 42.0 * k),
            None,
        );
        let sub = format!("/dev/{}  ·  {}", drive.name, bytes_si(drive.size));
        Self::txt(
            ctx.scene,
            ctx.text,
            &sub,
            &TextStyle::new(ty().sm),
            c.text_dim,
            Point::new(tx, r.y0 + 63.0 * k),
            None,
        );

        // Mini usage bar across drive.
        let used_frac = if drive.size > 0 {
            drive.used_total() as f64 / drive.size as f64
        } else {
            0.0
        };
        let barr = Rect::new(tx, r.y1 - 16.0 * k, r.x1 - CARD_INSET, r.y1 - 10.0 * k);
        Self::bar(
            ctx.scene,
            ctx.transform,
            barr,
            used_frac,
            c.neutral[4],
            usage_color(theme, used_frac),
        );

        // Health dot + short label at a glance.
        let status = drive.smart_status();
        let dot_c = status_color(theme, status);
        let dot = Rect::new(r.x1 - 26.0, r.y0 + 18.0 * k, r.x1 - 16.0, r.y0 + 28.0 * k);
        Self::fill_rr(ctx.scene, ctx.transform, dot, 5.0, dot_c);
        Self::txt_right(
            ctx.scene,
            ctx.text,
            status.label(),
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            dot_c,
            Point::new(r.x1 - 32.0, r.y0 + 17.0 * k),
        );
    }

    // ---- detail header -------------------------------------------------
    fn draw_header(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
        drive: &Drive,
        k: f64,
    ) {
        let c = &theme.tokens.colors;
        let h = lay.header;
        // Same radius and inner inset as the action band below and the drive
        // rows beside it, so the three cards read as one family.
        let card_radius = f64::from(theme.tokens.radii.lg);
        ui::surface::card(scene, t, h, card_radius, c.surfaces.raised);

        let tx = h.x0 + CARD_INSET;
        Self::txt(
            scene,
            tr,
            &drive.model,
            &TextStyle::new(ty().x2l).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(tx, h.y0 + 16.0 * k),
            None,
        );

        // kind badge next to model is overkill; show meta line.
        let meta = format!(
            "{}  ·  /dev/{}  ·  {}{}",
            drive.kind.long(),
            drive.name,
            bytes_si(drive.size),
            drive
                .serial
                .as_ref()
                .map(|s| format!("  ·  S/N {s}"))
                .unwrap_or_default(),
        );
        let meta = tr.elide_to_width_weighted(
            &meta,
            ty().sm,
            (h.width() - 2.0 * CARD_INSET) as f32,
            TextWeight::Regular,
        );
        Self::txt(
            scene,
            tr,
            &meta,
            &TextStyle::new(ty().sm),
            c.text_dim,
            Point::new(tx, h.y0 + 46.0 * k),
            None,
        );

        // Health + connection chips.
        // Each chip starts after the previous chip's measured width (an 8 px
        // gap at 1×), so labels never overprint at any text scale.
        let chip_y = h.y0 + 62.0 * k;
        let chip_gap = 8.0 * k;
        let status = drive.smart_status();
        let health = Self::draw_chip(
            scene,
            tr,
            theme,
            t,
            tx,
            chip_y,
            &format!("● {}", status.label()),
            status_color(theme, status),
        );
        let tran = drive
            .tran
            .clone()
            .unwrap_or_else(|| "—".into())
            .to_uppercase();
        let tran_chip = Self::draw_chip(
            scene,
            tr,
            theme,
            t,
            health.x1 + chip_gap,
            chip_y,
            &tran,
            c.neutral[5],
        );
        if drive.removable {
            Self::draw_chip(
                scene,
                tr,
                theme,
                t,
                tran_chip.x1 + chip_gap,
                chip_y,
                "REMOVABLE",
                c.semantic.info,
            );
        }
    }

    // ---- action band ---------------------------------------------------
    /// The single home for Mount / Unmount / Format / Benchmark / Eject.
    /// It names its target so there is never any doubt what a button acts on,
    /// dims the actions that don't apply, and captions the hovered one.
    fn draw_action_bar(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
        drive: &Drive,
    ) {
        let c = &theme.tokens.colors;
        let bar = lay.action_bar;
        let card_radius = f64::from(theme.tokens.radii.lg);
        ui::surface::card(scene, t, bar, card_radius, c.surfaces.raised);

        self.draw_action_bar_target(scene, tr, theme, t, bar, drive);
        self.draw_action_bar_buttons(scene, tr, theme, t, lay);
        self.draw_action_bar_caption(scene, tr, theme, bar);
    }

    /// The target line: exactly what the buttons below operate on (part of
    /// [`Self::draw_action_bar`]).
    fn draw_action_bar_target(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        bar: Rect,
        drive: &Drive,
    ) {
        let c = &theme.tokens.colors;
        let (scope, detail, scope_c) = match self.part() {
            Some(p) => (
                "PARTITION",
                format!("/dev/{}  ·  {}", p.name, p.display_label()),
                role_color(theme, p.role()),
            ),
            None => (
                "WHOLE DISK",
                format!("/dev/{}  ·  {}", drive.name, drive.model),
                theme.accent_primary,
            ),
        };
        let sx = bar.x0 + CARD_INSET;
        // Small scope tag: the capsule wraps the label's measured line box
        // (18 px at 1×) and both labels centre on the tag's measured band.
        let (tw, th) = tr.measure_weighted(scope, ty().xs, None, TextWeight::Bold);
        let tag_h = f64::from(th) + 5.5;
        let tag = Rect::new(
            sx,
            bar.y0 + 9.0 * self.k,
            sx + f64::from(tw) + 16.0,
            bar.y0 + 9.0 * self.k + tag_h,
        );
        Self::fill_rr(scene, t, tag, 6.0, with_alpha(scope_c, 46));
        Self::txt(
            scene,
            tr,
            scope,
            &TextStyle::new(ty().xs).with_weight(TextWeight::Bold),
            scope_c,
            Point::new(sx + 8.0, tag.y0 + (tag_h - f64::from(th)) / 2.0),
            None,
        );
        let (_, dh) = tr.measure_weighted(&detail, ty().sm, None, TextWeight::Medium);
        Self::txt(
            scene,
            tr,
            &detail,
            &TextStyle::new(ty().sm).with_weight(TextWeight::Medium),
            c.text_primary,
            Point::new(tag.x1 + 10.0, tag.center().y - f64::from(dh) / 2.0),
            Some((bar.width() - (tag.x1 - bar.x0) - 24.0) as f32),
        );
    }

    /// The action buttons row (part of [`Self::draw_action_bar`]).
    fn draw_action_bar_buttons(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let c = &theme.tokens.colors;
        for (r, a, enabled) in &lay.action_buttons {
            let hov = *enabled && self.hover_action == Some(*a);
            if !enabled {
                // Disabled — clearly inert: the rest face *and* its derived
                // edge under `DISABLED_ALPHA` (HIG §9.5), so the inert control
                // is the fainter of the pair, never the crisper.
                let disabled = ui::widget::DISABLED_ALPHA;
                Self::fill_rr(scene, t, *r, 8.0, c.neutral[2].with_alpha_factor(disabled));
                scene.stroke(
                    &Stroke::new(1.0),
                    t,
                    ui::surface::edge_on(c.neutral[2]).with_alpha_factor(disabled),
                    None,
                    &ui::surface::edge_outline(*r, 8.0),
                );
                Self::txt_center(
                    scene,
                    tr,
                    a.label(),
                    &TextStyle::new(ty().sm).with_weight(TextWeight::Medium),
                    with_alpha(c.text_dim, 130),
                    Point::new(f64::midpoint(r.x0, r.x1), r.y0 + 9.0),
                );
                continue;
            }
            let (bg, fg, border) = if a.destructive() {
                (
                    if hov {
                        c.semantic.danger
                    } else {
                        with_alpha(c.semantic.danger, 44)
                    },
                    if hov {
                        c.on_surface(c.semantic.danger).ink
                    } else {
                        c.semantic.danger
                    },
                    with_alpha(c.semantic.danger, 150),
                )
            } else {
                (
                    if hov { c.accent[4] } else { c.neutral[2] },
                    c.text_primary,
                    ui::surface::edge_on(if hov { c.accent[4] } else { c.neutral[2] }),
                )
            };
            Self::fill_rr(scene, t, *r, 8.0, bg);
            Self::stroke_rr(scene, t, *r, 8.0, border, 1.0);
            Self::txt_center(
                scene,
                tr,
                a.label(),
                &TextStyle::new(ty().sm).with_weight(TextWeight::Medium),
                fg,
                Point::new(f64::midpoint(r.x0, r.x1), r.y0 + 9.0),
            );
        }
    }

    /// Describe the hovered action, or the current scope by default (part
    /// of [`Self::draw_action_bar`]).
    fn draw_action_bar_caption(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        bar: Rect,
    ) {
        let c = &theme.tokens.colors;
        let caption = match self.hover_action {
            Some(a) => a.describe().to_string(),
            None => match self.part() {
                Some(_) => {
                    "Pick a partition action, or select the whole disk for device-level actions."
                        .into()
                }
                None => {
                    "Actions apply to the whole disk. Click a partition in the map to target it."
                        .into()
                }
            },
        };
        Self::txt(
            scene,
            tr,
            &caption,
            &TextStyle::new(ty().sm),
            c.text_dim,
            Point::new(bar.x0 + CARD_INSET, bar.y1 - 20.0),
            Some((bar.width() - 28.0) as f32),
        );
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "x/y/w/r are the standard graphics convention: position, measured width, resulting rect"
    )]
    /// Draw a capsule chip at `(x, y)` and return its rect so the caller can
    /// place the next chip after this one's measured width. The capsule wraps
    /// the label's measured line box (24 px at 1×) and centres the label on it.
    fn draw_chip(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        _theme: &Theme,
        t: Affine,
        x: f64,
        y: f64,
        label: &str,
        color: Color,
    ) -> Rect {
        let (w, h) = tr.measure_weighted(label, ty().sm, None, TextWeight::Semibold);
        let cap_h = f64::from(h) + 9.0;
        let r = Rect::new(x, y, x + f64::from(w) + 22.0, y + cap_h);
        Self::fill_rr(scene, t, r, cap_h / 2.0, with_alpha(color, 38));
        Self::stroke_rr(scene, t, r, cap_h / 2.0, with_alpha(color, 120), 1.0);
        Self::txt(
            scene,
            tr,
            label,
            &TextStyle::new(ty().sm).with_weight(TextWeight::Semibold),
            color,
            Point::new(x + 11.0, y + (cap_h - f64::from(h)) / 2.0),
            None,
        );
        r
    }

    // ---- tab bar -------------------------------------------------------
    fn draw_tabs(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        // Shared segmented control (pill/tab-strip style): selected pill in the
        // accent, hovered pill on the shared hover surface, others transparent.
        let rects: Vec<Rect> = lay.tabs.iter().map(|(r, _)| *r).collect();
        let labels: Vec<&str> = lay.tabs.iter().map(|(_, tab)| tab.title()).collect();
        let selected = lay
            .tabs
            .iter()
            .position(|(_, tab)| *tab == self.tab)
            .unwrap_or(0);
        let hovered = self
            .hover_tab
            .and_then(|h| lay.tabs.iter().position(|(_, tab)| *tab == h));
        ToggleGroup::paint(
            scene,
            t,
            tr,
            theme,
            &SegmentedFace::new(&rects, &labels, selected)
                .pill()
                .hovered(hovered)
                .radius(9.0),
        );
    }

    // ---- content: overview --------------------------------------------
    fn draw_overview(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
        drive: &Drive,
    ) {
        let c = &theme.tokens.colors;
        let area = lay.content;
        // Section heading + "Whole disk" selector (top-right).
        Self::txt(
            scene,
            tr,
            "PARTITION MAP",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(area.x0, area.y0),
            None,
        );
        Self::txt(
            scene,
            tr,
            "— click a partition to select it",
            &TextStyle::new(ty().xs),
            with_alpha(c.text_dim, 170),
            Point::new(area.x0 + 110.0, area.y0),
            None,
        );
        let pill = lay.whole_disk_pill;
        let whole = self.part().is_none();
        let pill_hov = self.hover_toggle.whole_disk;
        let pill_c = theme.accent_primary;
        Self::fill_rr(
            scene,
            t,
            pill,
            11.0,
            if whole {
                with_alpha(pill_c, 60)
            } else if pill_hov {
                c.neutral[3]
            } else {
                c.neutral[2]
            },
        );
        Self::stroke_rr(
            scene,
            t,
            pill,
            11.0,
            if whole {
                pill_c
            } else {
                ui::surface::edge_on(if pill_hov { c.neutral[3] } else { c.neutral[2] })
            },
            if whole { 1.6 } else { 1.0 },
        );
        Self::txt_center(
            scene,
            tr,
            "◍ Whole disk",
            &TextStyle::new(ty().sm).with_weight(TextWeight::Semibold),
            if whole { pill_c } else { c.text_dim },
            Point::new(f64::midpoint(pill.x0, pill.x1), pill.y0 + 4.0),
        );

        let map = Rect::new(area.x0, area.y0 + 20.0, area.x1, area.y0 + 20.0 + 58.0);
        self.draw_partition_map(scene, tr, theme, t, map, drive);
        // Legend.
        let legend_y = map.y1 + 12.0;
        Self::draw_legend(scene, tr, theme, t, area.x0, legend_y, area.x1, drive);

        // Two columns below: capacity (left) + SMART summary (right). Sized to
        // their natural content height (not the viewport) so scrolling can
        // reveal the whole thing when the window is short.
        let cols_top = legend_y + 56.0;
        let gap = 16.0;
        let left_w = (area.width() - gap) * 0.56;
        let cols_bottom = cols_top + overview_cols_h(drive);
        let left = Rect::new(area.x0, cols_top, area.x0 + left_w, cols_bottom);
        let right = Rect::new(left.x1 + gap, cols_top, area.x1, cols_bottom);

        self.draw_capacity(scene, tr, theme, t, left, drive);
        Self::draw_smart_card(scene, tr, theme, t, right, drive);
    }

    /// Natural pixel height of the current tab's content, used to size the
    /// scroll range and the Overview columns.
    fn content_height(&self, drive: &Drive) -> f64 {
        match self.tab {
            // heading + map(78) + legend(gap 12 + 56) = 146, then the columns.
            Tab::Overview => 146.0 + overview_cols_h(drive) + 12.0,
            Tab::Smart => {
                let attrs = drive.smart.as_ref().map_or(0, |s| s.attrs.len());
                258.0 + attrs as f64 * 24.0
            }
            Tab::Partitions => 24.0 + drive.partitions.len() as f64 * 38.0 + 16.0,
        }
    }

    fn draw_partition_map(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        map: Rect,
        drive: &Drive,
    ) {
        let c = &theme.tokens.colors;
        // Background track for the whole device, with its derived edge.
        ui::surface::card(scene, t, map, 8.0, c.neutral[2]);

        let sel = self.part_index();
        let any_sel = sel.is_some();
        let segments = partition_segments(drive, map);
        let n = segments.len();
        for (si, (seg, idx)) in segments.iter().enumerate() {
            let (seg, idx) = (*seg, *idx);
            let radius = if si == 0 || si == n - 1 { 6.0 } else { 3.0 };
            let is_sel = idx.is_some() && idx == sel;
            let is_hov = idx.is_some() && idx == self.hover_part;
            if let Some(i) = idx {
                let Some(p) = drive.partitions.get(i) else {
                    continue;
                };
                let mut col = role_color(theme, p.role());
                // Dim the unselected segments once something is selected so
                // the chosen partition reads as the focus.
                if any_sel && !is_sel {
                    col = with_alpha(col, 150);
                }
                Self::fill_rr(scene, t, seg, radius, col);
                if let Some(f) = p.used_frac() {
                    let uh = (seg.height() - 6.0) * f;
                    let used_r = Rect::new(seg.x0, seg.y1 - 3.0 - uh, seg.x1, seg.y1 - 3.0);
                    Self::fill_rr(
                        scene,
                        t,
                        used_r,
                        2.0,
                        ui::tokens::Colors::hairline_on(col, 70),
                    );
                }
                if seg.width() > 64.0 {
                    // Label ink measured against this segment's own colour.
                    let seg_ink = c.on_surface(col).ink;
                    Self::txt(
                        scene,
                        tr,
                        &p.display_label(),
                        &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
                        seg_ink,
                        Point::new(seg.x0 + 8.0, seg.y0 + 8.0),
                        Some((seg.width() - 12.0) as f32),
                    );
                    Self::txt(
                        scene,
                        tr,
                        &bytes_si(p.size),
                        &TextStyle::new(ty().xs),
                        with_alpha(seg_ink, 200),
                        Point::new(seg.x0 + 8.0, seg.y0 + 24.0),
                        None,
                    );
                }
            } else {
                // Free tail.
                Self::fill_rr(scene, t, seg, radius, role_color(theme, PartRole::Free));
                if seg.width() > 64.0 {
                    Self::txt(
                        scene,
                        tr,
                        "Free",
                        &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
                        c.text_dim,
                        Point::new(seg.x0 + 8.0, seg.y0 + 8.0),
                        None,
                    );
                    Self::txt(
                        scene,
                        tr,
                        &bytes_si(drive.free_tail()),
                        &TextStyle::new(ty().xs),
                        c.text_dim,
                        Point::new(seg.x0 + 8.0, seg.y0 + 24.0),
                        None,
                    );
                }
            }
            // Selection / hover outline — lifts the chosen segment above a track.
            if is_sel {
                let ring = seg.inset(-2.0);
                Self::stroke_rr(scene, t, ring, radius + 2.0, theme.accent_primary, 2.4);
            } else if is_hov {
                Self::stroke_rr(
                    scene,
                    t,
                    seg,
                    radius,
                    with_alpha(theme.accent_primary, 170),
                    1.6,
                );
            }
        }
    }

    fn draw_legend(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        x0: f64,
        y: f64,
        x1: f64,
        drive: &Drive,
    ) {
        let c = &theme.tokens.colors;
        let mut x = x0;
        let mut yy = y;
        // Flow the swatches left-to-right, wrapping to a second row (the layout
        // budgets two rows under the map) with x reset to the left margin — so
        // a drive with many partitions never runs its legend off the edge.
        let mut place =
            |label: String, color: Color, sub: String, scene: &mut Scene, tr: &mut TextRenderer| {
                let advance = ((label.len().max(sub.len()) as f64) * 6.5 + 34.0).max(120.0);
                if x + advance > x1 && x > x0 && yy < y + 22.0 {
                    x = x0;
                    yy = y + 22.0;
                }
                // Past the two-row budget the remaining entries would overlap the
                // capacity/SMART columns below — stop rather than draw garbage.
                if yy >= y + 22.0 && x + advance > x1 && x > x0 {
                    return;
                }
                let sw = Rect::new(x, yy + 1.0, x + 12.0, yy + 13.0);
                Self::fill_rr(scene, t, sw, 3.0, color);
                Self::txt(
                    scene,
                    tr,
                    &label,
                    &TextStyle::new(ty().sm).with_weight(TextWeight::Medium),
                    c.text_primary,
                    Point::new(x + 18.0, yy),
                    None,
                );
                Self::txt(
                    scene,
                    tr,
                    &sub,
                    &TextStyle::new(ty().xs),
                    c.text_dim,
                    Point::new(x + 18.0, yy + 14.0),
                    None,
                );
                x += advance;
            };
        for p in &drive.partitions {
            place(
                p.display_label(),
                role_color(theme, p.role()),
                format!("{}  {}", p.role().legend(), bytes_si(p.size)),
                scene,
                tr,
            );
        }
        if drive.free_tail() > 0 {
            place(
                "Free".into(),
                role_color(theme, PartRole::Free),
                bytes_si(drive.free_tail()),
                scene,
                tr,
            );
        }
    }

    fn draw_capacity(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        area: Rect,
        drive: &Drive,
    ) {
        let c = &theme.tokens.colors;
        Self::txt(
            scene,
            tr,
            "FILESYSTEMS & USAGE",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(area.x0, area.y0),
            None,
        );
        let mut y = area.y0 + 24.0;
        let mounted: Vec<&Partition> = drive
            .partitions
            .iter()
            .filter(|p| p.mountpoint.is_some())
            .collect();
        if mounted.is_empty() {
            Self::txt(
                scene,
                tr,
                "No mounted filesystems.",
                &TextStyle::new(ty().sm),
                c.text_dim,
                Point::new(area.x0, y),
                None,
            );
            return;
        }
        let sel_name = self.part().map(|p| p.name.clone());
        for p in mounted {
            if y > area.y1 - 40.0 {
                break;
            }
            // Tie the selected partition's usage back to the map + action bar.
            if sel_name.as_deref() == Some(p.name.as_str()) {
                let hl = Rect::new(area.x0 - 8.0, y - 6.0, area.x1 + 8.0, y + 44.0);
                Self::fill_rr(scene, t, hl, 8.0, with_alpha(theme.accent_primary, 30));
                Self::fill_rr(
                    scene,
                    t,
                    Rect::new(hl.x0, hl.y0 + 4.0, hl.x0 + 3.0, hl.y1 - 4.0),
                    2.0,
                    theme.accent_primary,
                );
            }
            let mp = p.mountpoint.clone().unwrap_or_default();
            Self::txt(
                scene,
                tr,
                &p.display_label(),
                &TextStyle::new(ty().base).with_weight(TextWeight::Semibold),
                c.text_primary,
                Point::new(area.x0, y),
                None,
            );
            Self::txt_right(
                scene,
                tr,
                &mp,
                &TextStyle::new(ty().sm),
                c.text_dim,
                Point::new(area.x1, y + 1.0),
            );
            let barr = Rect::new(area.x0, y + 22.0, area.x1, y + 32.0);
            let frac = p.used_frac().unwrap_or(0.0);
            Self::bar(scene, t, barr, frac, c.neutral[3], usage_color(theme, frac));
            let detail = match p.used {
                Some(u) => format!(
                    "{} used of {}  ·  {} free  ·  {}",
                    bytes_si(u),
                    bytes_si(p.size),
                    bytes_si(p.size.saturating_sub(u)),
                    p.fstype.clone().unwrap_or_default(),
                ),
                None => format!(
                    "{}  ·  {}",
                    bytes_si(p.size),
                    p.fstype.clone().unwrap_or_default()
                ),
            };
            Self::txt(
                scene,
                tr,
                &detail,
                &TextStyle::new(ty().sm),
                c.text_dim,
                Point::new(area.x0, y + 36.0),
                None,
            );
            y += 62.0;
        }
    }

    fn draw_smart_card(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        area: Rect,
        drive: &Drive,
    ) {
        let c = &theme.tokens.colors;
        ui::surface::card(scene, t, area, 12.0, c.neutral[2]);
        let pad = 16.0;
        let x = area.x0 + pad;
        let status = drive.smart_status();
        Self::txt(
            scene,
            tr,
            "SMART HEALTH",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(x, area.y0 + pad),
            None,
        );

        let pill = Self::draw_smart_status_pill(scene, tr, theme, t, area, x, pad, status);

        let Some(smart) = &drive.smart else {
            Self::txt(
                scene,
                tr,
                "No SMART data available.",
                &TextStyle::new(ty().sm),
                c.text_dim,
                Point::new(x, pill.y1 + 14.0),
                None,
            );
            return;
        };

        // No usable telemetry: explain *why* (privilege / bridge / absent)
        // instead of leaving a blank card.
        if smart.temp_c.is_none() && smart.power_on_hours.is_none() && smart.attrs.is_empty() {
            let note = smart
                .detail
                .as_deref()
                .unwrap_or("No SMART data available.");
            Self::txt(
                scene,
                tr,
                note,
                &TextStyle::new(ty().sm),
                c.text_dim,
                Point::new(x, pill.y1 + 14.0),
                Some((area.width() - 2.0 * pad) as f32),
            );
            return;
        }

        Self::draw_smart_card_body(
            &mut PaintCtx::new(scene, t, tr, theme),
            area,
            x,
            pad,
            pill,
            smart,
        );
    }

    /// The status pill under the "SMART HEALTH" label (part of
    /// [`Self::draw_smart_card`]). Returns the pill's rect so the caller can
    /// anchor the rest of the card beneath it.
    fn draw_smart_status_pill(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        area: Rect,
        x: f64,
        pad: f64,
        status: SmartStatus,
    ) -> Rect {
        let sc = status_color(theme, status);
        let pill = Rect::new(
            x,
            area.y0 + pad + 22.0,
            x + 150.0,
            area.y0 + pad + 22.0 + 30.0,
        );
        Self::fill_rr(scene, t, pill, 8.0, with_alpha(sc, 45));
        Self::stroke_rr(scene, t, pill, 8.0, sc, 1.2);
        Self::txt(
            scene,
            tr,
            status.label(),
            &TextStyle::new(ty().base).with_weight(TextWeight::Bold),
            sc,
            Point::new(x + 12.0, pill.y0 + 7.0),
            None,
        );
        pill
    }

    /// The temperature gauge + stat rows below the status pill, once SMART
    /// telemetry is known to have at least one usable value (part of
    /// [`Self::draw_smart_card`]).
    fn draw_smart_card_body(
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        x: f64,
        pad: f64,
        pill: Rect,
        smart: &SmartInfo,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        // Temperature gauge.
        let mut yy = pill.y1 + 18.0;
        if let Some(temp) = smart.temp_c {
            Self::txt(
                ctx.scene,
                ctx.text,
                "Temperature",
                &TextStyle::new(ty().sm),
                c.text_dim,
                Point::new(x, yy),
                None,
            );
            Self::txt_right(
                ctx.scene,
                ctx.text,
                &format!("{temp} °C"),
                &TextStyle::new(ty().sm).with_weight(TextWeight::Semibold),
                c.text_primary,
                Point::new(area.x1 - pad, yy),
            );
            let frac = ((temp as f64 - 20.0) / 50.0).clamp(0.0, 1.0);
            let barr = Rect::new(x, yy + 18.0, area.x1 - pad, yy + 26.0);
            Self::bar(
                ctx.scene,
                ctx.transform,
                barr,
                frac,
                c.neutral[3],
                temp_color(theme, temp),
            );
            yy += 40.0;
        }

        // Stat rows.
        let stat =
            |label: &str, val: String, scene: &mut Scene, tr: &mut TextRenderer, yy: &mut f64| {
                if *yy > area.y1 - 22.0 {
                    return;
                }
                Self::txt(
                    scene,
                    tr,
                    label,
                    &TextStyle::new(ty().sm),
                    c.text_dim,
                    Point::new(x, *yy),
                    None,
                );
                Self::txt_right(
                    scene,
                    tr,
                    &val,
                    &TextStyle::new(ty().sm).with_weight(TextWeight::Semibold),
                    c.text_primary,
                    Point::new(area.x1 - pad, *yy),
                );
                *yy += 22.0;
            };
        if let Some(h) = smart.power_on_hours {
            stat(
                "Power-on time",
                format!("{} ({} h)", fmt_hours(h), h),
                ctx.scene,
                ctx.text,
                &mut yy,
            );
        }
        if let Some(pc) = smart.power_cycles {
            stat("Power cycles", pc.to_string(), ctx.scene, ctx.text, &mut yy);
        }
        if let Some(re) = smart.reallocated {
            stat(
                "Reallocated sectors",
                re.to_string(),
                ctx.scene,
                ctx.text,
                &mut yy,
            );
        }
        if let Some(pu) = smart.pct_used {
            stat(
                "Wear (NVMe used)",
                format!("{pu} %"),
                ctx.scene,
                ctx.text,
                &mut yy,
            );
        }
        if let Some(w) = smart.written_gb {
            stat(
                "Host writes",
                bytes_si((w as u64) * 1_000_000_000),
                ctx.scene,
                ctx.text,
                &mut yy,
            );
        }
    }

    // ---- content: SMART tab -------------------------------------------
    fn draw_smart_tab(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        area: Rect,
        drive: &Drive,
    ) {
        let status = drive.smart_status();
        let banner = Self::draw_smart_tab_banner(scene, tr, theme, t, area, drive, status);

        let Some(smart) = &drive.smart else {
            return;
        };

        let tiles_top = banner.y1 + 16.0;
        let tile_h = 78.0;
        Self::draw_smart_tiles(scene, tr, theme, t, area, smart, tiles_top, tile_h);

        let table_top = tiles_top + tile_h + 18.0;
        Self::draw_smart_attr_table(scene, tr, theme, t, area, smart, table_top);
    }

    /// The overall self-assessment banner (part of [`Self::draw_smart_tab`]).
    /// Returns its rect so the caller can anchor the stat tiles beneath it.
    fn draw_smart_tab_banner(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        area: Rect,
        drive: &Drive,
        status: SmartStatus,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let sc = status_color(theme, status);
        let banner = Rect::new(area.x0, area.y0, area.x1, area.y0 + 64.0);
        Self::fill_rr(scene, t, banner, 12.0, with_alpha(sc, 38));
        Self::stroke_rr(scene, t, banner, 12.0, with_alpha(sc, 120), 1.0);
        Self::txt(
            scene,
            tr,
            &format!("● Overall self-assessment: {}", status.label()),
            &TextStyle::new(ty().lg).with_weight(TextWeight::Bold),
            sc,
            Point::new(banner.x0 + 18.0, banner.y0 + 12.0),
            None,
        );
        let generic = match status {
            SmartStatus::Passed => "All monitored attributes are within nominal thresholds.",
            SmartStatus::Warning => {
                "One or more attributes need attention — back up important data."
            }
            SmartStatus::Failing => {
                "Drive reports imminent failure. Replace it as soon as possible."
            }
            SmartStatus::Unknown => "This device does not expose SMART telemetry.",
        };
        // For an Unknown verdict, prefer the probe's specific reason (needs
        // privilege / USB bridge / virtual device) over the generic line.
        let sub = if status == SmartStatus::Unknown {
            drive
                .smart
                .as_ref()
                .and_then(|s| s.detail.as_deref())
                .unwrap_or(generic)
        } else {
            generic
        };
        Self::txt(
            scene,
            tr,
            sub,
            &TextStyle::new(ty().sm),
            c.text_dim,
            Point::new(banner.x0 + 18.0, banner.y0 + 38.0),
            Some((banner.width() - 36.0) as f32),
        );
        banner
    }

    /// The temperature / power-on / power-cycles / reallocated / wear stat
    /// tiles row (part of [`Self::draw_smart_tab`]).
    fn draw_smart_tiles(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        area: Rect,
        smart: &SmartInfo,
        tiles_top: f64,
        tile_h: f64,
    ) {
        let c = &theme.tokens.colors;
        let tiles: Vec<(String, String)> = vec![
            (
                "Temperature".into(),
                smart
                    .temp_c
                    .map_or_else(|| "—".into(), |v| format!("{v} °C")),
            ),
            (
                "Power-on hours".into(),
                smart
                    .power_on_hours
                    .map_or_else(|| "—".into(), |v| v.to_string()),
            ),
            (
                "Power cycles".into(),
                smart
                    .power_cycles
                    .map_or_else(|| "—".into(), |v| v.to_string()),
            ),
            (
                "Reallocated".into(),
                smart
                    .reallocated
                    .map_or_else(|| "—".into(), |v| v.to_string()),
            ),
            (
                "Wear used".into(),
                smart
                    .pct_used
                    .map_or_else(|| "—".into(), |v| format!("{v} %")),
            ),
        ];
        let n = tiles.len() as f64;
        let gap = 12.0;
        let tw = (area.width() - gap * (n - 1.0)) / n;
        for (i, (label, val)) in tiles.iter().enumerate() {
            let tx = area.x0 + i as f64 * (tw + gap);
            let r = Rect::new(tx, tiles_top, tx + tw, tiles_top + tile_h);
            ui::surface::card(scene, t, r, 10.0, c.surfaces.raised);
            Self::txt(
                scene,
                tr,
                label,
                &TextStyle::new(ty().xs).with_weight(TextWeight::Medium),
                c.text_dim,
                Point::new(r.x0 + 12.0, r.y0 + 12.0),
                None,
            );
            Self::txt(
                scene,
                tr,
                val,
                &TextStyle::new(ty().x2l).with_weight(TextWeight::Bold),
                c.text_primary,
                Point::new(r.x0 + 12.0, r.y0 + 34.0),
                None,
            );
        }
    }

    /// The SMART attributes table (part of [`Self::draw_smart_tab`]).
    /// The column headers for the SMART attribute table (part of
    /// [`Self::draw_smart_attr_table`]). Returns `(header_y, id_x, name_x,
    /// val_x, stat_x)` so each row aligns beneath them.
    fn draw_smart_attr_table_header(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        table_top: f64,
    ) -> (f64, f64, f64, f64, f64) {
        let c = &theme.tokens.colors;
        Self::txt(
            scene,
            tr,
            "SMART ATTRIBUTES",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(area.x0, table_top),
            None,
        );
        let header_y = table_top + 20.0;
        let id_x = area.x0 + 4.0;
        let name_x = area.x0 + 54.0;
        let val_x = area.x1 - 220.0;
        let stat_x = area.x1 - 90.0;
        Self::txt(
            scene,
            tr,
            "ID",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(id_x, header_y),
            None,
        );
        Self::txt(
            scene,
            tr,
            "ATTRIBUTE",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(name_x, header_y),
            None,
        );
        Self::txt(
            scene,
            tr,
            "VALUE",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(val_x, header_y),
            None,
        );
        Self::txt(
            scene,
            tr,
            "STATUS",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(stat_x, header_y),
            None,
        );
        (header_y, id_x, name_x, val_x, stat_x)
    }

    /// One row of the SMART attribute table (part of
    /// [`Self::draw_smart_attr_table`]). `xs` is `(id_x, name_x, val_x,
    /// stat_x)` from [`Self::draw_smart_attr_table_header`].
    fn draw_smart_attr_row(
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        i: usize,
        a: &SmartAttr,
        xs: (f64, f64, f64, f64),
        ry: f64,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let (id_x, name_x, val_x, stat_x) = xs;
        if i.is_multiple_of(2) {
            // Row pitch is 24 px; the zebra band keeps half an `s1` above and
            // below so consecutive bands never touch (HIG §7).
            let g = f64::from(theme.tokens.spacing.s1) / 2.0;
            Self::fill_rr(
                ctx.scene,
                ctx.transform,
                Rect::new(area.x0, ry - 4.0 + g, area.x1, ry + 20.0 - g),
                4.0,
                c.lift(0.35),
            );
        }
        Self::txt(
            ctx.scene,
            ctx.text,
            &a.id.to_string(),
            &TextStyle::new(ty().sm),
            c.text_dim,
            Point::new(id_x, ry),
            None,
        );
        Self::txt(
            ctx.scene,
            ctx.text,
            &a.name,
            &TextStyle::new(ty().sm),
            c.text_primary,
            Point::new(name_x, ry),
            Some((val_x - name_x - 12.0) as f32),
        );
        Self::txt(
            ctx.scene,
            ctx.text,
            &a.value,
            &TextStyle::new(ty().sm).with_weight(TextWeight::Medium),
            c.text_primary,
            Point::new(val_x, ry),
            None,
        );
        let (sl, scl) = match a.severity {
            AttrSeverity::Ok => ("OK", c.semantic.success),
            AttrSeverity::Warn => ("WARN", c.semantic.warning),
            AttrSeverity::Crit => ("CRIT", c.semantic.danger),
        };
        Self::txt(
            ctx.scene,
            ctx.text,
            sl,
            &TextStyle::new(11.5).with_weight(TextWeight::Bold),
            scl,
            Point::new(stat_x, ry),
            None,
        );
    }

    fn draw_smart_attr_table(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        area: Rect,
        smart: &SmartInfo,
        table_top: f64,
    ) {
        let (header_y, id_x, name_x, val_x, stat_x) =
            Self::draw_smart_attr_table_header(scene, tr, theme, area, table_top);
        let mut ry = header_y + 22.0;
        for (i, a) in smart.attrs.iter().enumerate() {
            if ry > area.y1 - 20.0 {
                break;
            }
            Self::draw_smart_attr_row(
                &mut PaintCtx::new(scene, t, tr, theme),
                area,
                i,
                a,
                (id_x, name_x, val_x, stat_x),
                ry,
            );
            ry += 24.0;
        }
    }

    // ---- content: partitions tab --------------------------------------
    fn draw_partitions_tab(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        area: Rect,
        drive: &Drive,
    ) {
        let cols = Self::draw_partitions_header(scene, tr, theme, area);

        let mut ry = area.y0 + 24.0;
        for (i, p) in drive.partitions.iter().enumerate() {
            let selected = self.selected_part == Some(i);
            let hover = self.hover_part == Some(i);
            let row = PartitionRow {
                index: i,
                partition: p,
                y: ry,
                selected,
                hover,
            };
            Self::draw_partitions_row(&mut PaintCtx::new(scene, t, tr, theme), area, &cols, &row);
            ry += 38.0;
        }
    }

    /// The column headers (part of [`Self::draw_partitions_tab`]). Returns
    /// the resolved column x-positions so each row aligns to them.
    fn draw_partitions_header(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) -> PartitionColumns {
        let c = &theme.tokens.colors;
        let cols = PartitionColumns {
            name: area.x0 + 6.0,
            label: area.x0 + 140.0,
            fs: area.x0 + 290.0,
            size: area.x0 + 380.0,
            mount: area.x0 + 470.0,
            used: area.x1 - 6.0,
        };
        let hy = area.y0;
        Self::txt(
            scene,
            tr,
            "DEVICE",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(cols.name, hy),
            None,
        );
        Self::txt(
            scene,
            tr,
            "LABEL",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(cols.label, hy),
            None,
        );
        Self::txt(
            scene,
            tr,
            "FS",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(cols.fs, hy),
            None,
        );
        Self::txt(
            scene,
            tr,
            "SIZE",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(cols.size, hy),
            None,
        );
        Self::txt(
            scene,
            tr,
            "MOUNT",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(cols.mount, hy),
            None,
        );
        Self::txt_right(
            scene,
            tr,
            "USED",
            &TextStyle::new(ty().xs).with_weight(TextWeight::Semibold),
            c.text_dim,
            Point::new(cols.used, hy),
        );
        cols
    }

    /// The row's selection/hover/zebra background plus the role swatch (part
    /// of [`Self::draw_partitions_row`]).
    fn draw_partitions_row_bg(ctx: &mut PaintCtx<'_>, area: Rect, row: &PartitionRow<'_>) {
        let (scene, t, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
        let PartitionRow {
            index: i,
            partition: p,
            y: ry,
            selected,
            hover,
        } = *row;
        let c = &theme.tokens.colors;
        // Row pitch is 38 px (`ry += 38.0` in the caller); the painted band
        // leaves one `s1` of it as the gap to the next row (HIG §7).
        let gap = f64::from(theme.tokens.spacing.s1);
        let rowr = Rect::new(area.x0, ry - 6.0, area.x1, ry - 6.0 + 38.0 - gap);
        if selected {
            Self::fill_rr(scene, t, rowr, 6.0, with_alpha(theme.accent_primary, 40));
            Self::stroke_rr(scene, t, rowr, 6.0, theme.accent_primary, 1.4);
        } else if hover {
            Self::fill_rr(scene, t, rowr, 6.0, c.neutral[3]);
        } else if i.is_multiple_of(2) {
            Self::fill_rr(scene, t, rowr, 6.0, c.lift(0.35));
        }
        // role swatch (thickened when selected → doubles as the accent bar)
        let sw = Rect::new(
            area.x0,
            ry + 2.0,
            area.x0 + if selected { 5.0 } else { 4.0 },
            ry + 18.0,
        );
        Self::fill_rr(
            scene,
            t,
            sw,
            2.0,
            if selected {
                theme.accent_primary
            } else {
                role_color(theme, p.role())
            },
        );
    }

    /// The name/label/fs/size/mount text fields of a partition row (part of
    /// [`Self::draw_partitions_row`]).
    fn draw_partitions_row_fields(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        cols: &PartitionColumns,
        ry: f64,
        p: &Partition,
    ) {
        let c = &theme.tokens.colors;
        Self::txt(
            scene,
            tr,
            &p.name,
            &TextStyle::new(ty().sm).with_weight(TextWeight::Medium),
            c.text_primary,
            Point::new(cols.name, ry),
            None,
        );
        Self::txt(
            scene,
            tr,
            &p.label.clone().unwrap_or_else(|| "—".into()),
            &TextStyle::new(ty().sm),
            c.text_dim,
            Point::new(cols.label, ry),
            Some((cols.fs - cols.label - 8.0) as f32),
        );
        Self::txt(
            scene,
            tr,
            &p.fstype.clone().unwrap_or_else(|| "—".into()),
            &TextStyle::new(ty().sm),
            c.text_dim,
            Point::new(cols.fs, ry),
            None,
        );
        Self::txt(
            scene,
            tr,
            &bytes_si(p.size),
            &TextStyle::new(ty().sm),
            c.text_primary,
            Point::new(cols.size, ry),
            None,
        );
        Self::txt(
            scene,
            tr,
            &p.mountpoint.clone().unwrap_or_else(|| "not mounted".into()),
            &TextStyle::new(ty().sm),
            c.text_dim,
            Point::new(cols.mount, ry),
            Some((cols.used - cols.mount - 70.0) as f32),
        );
    }

    /// The usage mini-bar (or the "—" placeholder when unmounted) at the
    /// right of a partition row (part of [`Self::draw_partitions_row`]).
    fn draw_partitions_row_usage(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        cols: &PartitionColumns,
        ry: f64,
        p: &Partition,
    ) {
        let c = &theme.tokens.colors;
        if let Some(f) = p.used_frac() {
            let barr = Rect::new(cols.used - 64.0, ry + 6.0, cols.used, ry + 14.0);
            Self::bar(scene, t, barr, f, c.neutral[3], usage_color(theme, f));
            Self::txt_right(
                scene,
                tr,
                &format!("{:.0}%", f * 100.0),
                &TextStyle::new(ty().xs),
                c.text_dim,
                Point::new(cols.used, ry - 8.0),
            );
        } else {
            Self::txt_right(
                scene,
                tr,
                "—",
                &TextStyle::new(ty().sm),
                c.text_dim,
                Point::new(cols.used, ry),
            );
        }
    }

    /// One partition row: selection/hover/zebra background, role swatch,
    /// name/label/fs/size/mount, and the usage mini-bar (part of
    /// [`Self::draw_partitions_tab`]).
    fn draw_partitions_row(
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        cols: &PartitionColumns,
        row: &PartitionRow<'_>,
    ) {
        let (ry, p) = (row.y, row.partition);
        Self::draw_partitions_row_bg(ctx, area, row);
        Self::draw_partitions_row_fields(ctx.scene, ctx.text, ctx.theme, cols, ry, p);
        Self::draw_partitions_row_usage(ctx.scene, ctx.text, ctx.theme, ctx.transform, cols, ry, p);
    }

    // ---- modal ---------------------------------------------------------
    fn draw_modal(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        ml: &ModalLayout,
        pending: Pending,
    ) {
        let c = &theme.tokens.colors;
        // `surfaces.overlay` is intentionally opaque in the high-contrast
        // palette, which previously erased all surrounding context whenever a
        // confirmation opened. A translucent, theme-neutral scrim keeps the
        // target storage screen visible while preserving modal separation.
        scene.fill(
            Fill::NonZero,
            t,
            with_alpha(c.surfaces.scrim, 172),
            None,
            &self.rect,
        );
        // Panel role (HIG §9.5): the shared floating-panel ground and its
        // derived border, so the confirm sheet matches every other dialog.
        Self::fill_rr(scene, t, ml.card, 16.0, ui::chrome::panel_ground(theme));
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::chrome::panel_border(theme),
            None,
            &ui::surface::edge_outline(ml.card, 16.0),
        );
        let danger = pending.action.destructive();

        self.draw_modal_card_text(scene, tr, theme, ml.card, pending, danger);
        let ready = self.draw_modal_input(scene, tr, theme, t, ml, pending);
        self.draw_modal_buttons(scene, tr, theme, t, ml, danger, ready);
    }

    /// The title + body text on the confirm card (part of
    /// [`Self::draw_modal`]).
    fn draw_modal_card_text(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        pending: Pending,
        danger: bool,
    ) {
        let c = &theme.tokens.colors;
        let target = self.pending_target_name(pending);
        let title_c = if danger {
            c.semantic.danger
        } else {
            c.text_primary
        };
        Self::txt(
            scene,
            tr,
            &pending.action.confirm_title(&target),
            &TextStyle::new(ty().xl).with_weight(TextWeight::Bold),
            title_c,
            Point::new(card.x0 + 22.0, card.y0 + 22.0),
            None,
        );
        Self::txt(
            scene,
            tr,
            &pending.action.confirm_body(&target, self.live),
            &TextStyle::new(ty().sm),
            c.text_dim,
            Point::new(card.x0 + 22.0, card.y0 + 54.0),
            Some((card.width() - 44.0) as f32),
        );
    }

    /// The type-to-confirm gate: the user must type the device name exactly
    /// (part of [`Self::draw_modal`]). Returns whether the typed text
    /// currently matches.
    fn draw_modal_input(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        ml: &ModalLayout,
        pending: Pending,
    ) -> bool {
        let c = &theme.tokens.colors;
        let card = ml.card;
        let inp = ml.input;
        let expected = self.pending_target_leaf(pending);
        let ready = self.confirm_ready();
        Self::txt(
            scene,
            tr,
            &format!("Type “{expected}” to confirm:"),
            &TextStyle::new(ty().sm).with_weight(TextWeight::Medium),
            c.text_dim,
            Point::new(card.x0 + 22.0, inp.y0 - 20.0),
            None,
        );
        self.confirm_input.set_placeholder(&expected);
        self.confirm_input.apply_layout_external(inp);
        self.confirm_input
            .draw(scene, t, tr, theme, &PresenceCtx::empty());
        if ready {
            // Keep the modal's positive match feedback around the shared field.
            Self::stroke_rr(scene, t, inp, 8.0, c.semantic.success, 1.4);
            Self::txt_right(
                scene,
                tr,
                "✓ match",
                &TextStyle::new(ty().sm).with_weight(TextWeight::Semibold),
                c.semantic.success,
                Point::new(inp.x1 - 12.0, inp.y0 + 10.0),
            );
        }
        ready
    }

    /// The Cancel + Confirm buttons — Confirm is locked (dimmed, inert)
    /// until the typed name matches (part of [`Self::draw_modal`]).
    fn draw_modal_buttons(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        ml: &ModalLayout,
        danger: bool,
        ready: bool,
    ) {
        let cancel_hov = ml.cancel.contains(self.last_mouse);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(ml.cancel, "Cancel")
                .radius(9.0)
                .hovered(cancel_hov),
        );

        // Confirm — locked (dimmed, inert) until the typed name matches.
        let confirm_hov = ready && ml.confirm.contains(self.last_mouse);
        let label = if danger { "Erase" } else { "Confirm" };
        let face = if danger {
            ButtonFace::new(ml.confirm, label).danger()
        } else {
            ButtonFace::new(ml.confirm, label).primary()
        };
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &face
                .radius(9.0)
                .weight(TextWeight::Semibold)
                .hovered(confirm_hov)
                .disabled(!ready),
        );
    }

    // ---- empty state ---------------------------------------------------
    /// Shown when the live probe reports no block devices (or the tools are
    /// missing). It is deliberately honest: no fabricated drives, just an
    /// explanation and the Rescan affordance.
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let c = &theme.tokens.colors;
        // A centred card in the detail region (geometry shared with layout so
        // the Rescan button hit-tests exactly where it is painted).
        let (card, btn) = empty_state_rects(self.rect, lay.header);
        let cx = f64::midpoint(card.x0, card.x1);
        ui::surface::card(scene, t, card, 16.0, c.surfaces.raised);

        // A simple disk glyph (stacked platters) drawn from primitives so the
        // empty state doesn't depend on a font's emoji coverage.
        let gx = cx;
        let gy = card.y0 + 44.0;
        for i in 0..3 {
            let yy = gy + f64::from(i) * 11.0;
            let ell = Rect::new(gx - 26.0, yy, gx + 26.0, yy + 16.0);
            Self::fill_rr(scene, t, ell, 8.0, with_alpha(c.text_dim, 40));
            Self::stroke_rr(scene, t, ell, 8.0, with_alpha(c.text_dim, 110), 1.2);
        }

        Self::txt_center(
            scene,
            tr,
            "No storage devices found",
            &TextStyle::new(ty().lg).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(cx, card.y0 + 96.0),
        );
        let line = if std::env::var("DISKS_LIVE").is_ok() {
            "The system reported no block devices. Attach a drive, then Rescan."
        } else {
            "No devices to display."
        };
        Self::txt_center(
            scene,
            tr,
            line,
            &TextStyle::new(ty().sm),
            c.text_dim,
            Point::new(cx, card.y0 + 124.0),
        );
        Self::txt_center(
            scene,
            tr,
            "Reflecting the live system — nothing is invented here.",
            &TextStyle::new(ty().sm),
            with_alpha(c.text_dim, 180),
            Point::new(cx, card.y0 + 144.0),
        );

        // Rescan — re-runs the live probe (same path as the top-bar button).
        let hov = btn.contains(self.last_mouse);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(btn, "Rescan")
                .icon(Icon::Refresh)
                .radius(9.0)
                .text_size(13.0)
                .hovered(hov),
        );
    }

    // ---- status line ---------------------------------------------------
    fn draw_statusline(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine) {
        if self.status.is_empty() {
            return;
        }
        let c = &theme.tokens.colors;
        let lower = self.status.to_ascii_lowercase();
        let tone = if lower.contains("failed") || lower.contains("error") {
            c.semantic.danger
        } else if lower.contains("mounted")
            || lower.contains("formatted")
            || lower.contains("ejected")
            || lower.contains("rescanned")
            || lower.contains("complete")
        {
            c.semantic.success
        } else {
            c.semantic.info
        };
        // An elevated, bounded toast keeps completion and failure feedback
        // readable above scrolled content rather than letting it disappear
        // into whichever partition row happens to sit at the window bottom.
        let r = Rect::new(
            PAD,
            self.rect.y1 - 48.0,
            (PAD + 760.0).min(self.rect.x1 - PAD),
            self.rect.y1 - PAD,
        );
        Self::fill_rr(scene, t, r, 9.0, c.surfaces.raised);
        Self::stroke_rr(scene, t, r, 9.0, tone, 1.0);
        Self::txt(
            scene,
            tr,
            &self.status,
            &TextStyle::new(ty().sm).with_weight(TextWeight::Medium),
            tone,
            Point::new(r.x0 + 12.0, r.y0 + 9.0),
            Some((r.width() - 24.0) as f32),
        );
    }

    // ---- interaction ---------------------------------------------------
    /// The `/dev/…` name a *pending* action targets (partition if one was
    /// captured with the action, else the whole disk).
    fn pending_target_name(&self, p: Pending) -> String {
        format!("/dev/{}", self.pending_target_leaf(p))
    }

    /// The bare kernel leaf name (`nvme0n1p3`, `sdc`) a pending action targets.
    /// This is also the string the user must type to unlock a destructive op.
    fn pending_target_leaf(&self, p: Pending) -> String {
        let Some(d) = self.topo.drives.get(p.drive) else {
            return String::new();
        };
        match p.part.and_then(|i| d.partitions.get(i)) {
            Some(part) => part.name.clone(),
            None => d.name.clone(),
        }
    }

    /// True once the typed confirmation matches the target — gates Erase.
    fn confirm_ready(&self) -> bool {
        match self.pending {
            Some(p) => {
                !self.confirm_input.value().trim().is_empty()
                    && self.confirm_input.value().trim() == self.pending_target_leaf(p)
            }
            None => false,
        }
    }

    fn trigger_action(&mut self, state: &mut AutoCommit, action: ActionKind) {
        let Some(drive) = self.drive() else { return };
        if !self.action_enabled(drive, action) {
            return; // inapplicable buttons are inert
        }
        // Resolve what the action targets: Mount/Unmount snap to a filesystem;
        // Eject always the whole device; Format/Benchmark follow the selection.
        let part = match action {
            ActionKind::Mount | ActionKind::Unmount => self.mount_target_part(drive),
            ActionKind::Eject => None,
            ActionKind::Format | ActionKind::Benchmark => self.part_index(),
        };

        if action.destructive() {
            // Open the type-to-confirm modal — nothing executes until the user
            // types the device name and confirms.
            self.pending = Some(Pending {
                action,
                drive: self.selected,
                part,
            });
            self.confirm_input.set_text("");
            self.confirm_input.set_focus(true);
            self.commit_shared(
                state,
                &crate::resource::DisksAction::SetPending {
                    pending: format!(
                        "{}:{}:{}",
                        action.key(),
                        self.selected,
                        part.map_or(-1, |i| i as i64)
                    ),
                },
            );
            self.dirty = true;
        } else {
            self.dispatch_action(state, action, part);
        }
    }

    /// Actually perform (or, in hermetic mode, describe) an action. Live runs
    /// spawn a worker thread whose result is drained in `sync`.
    fn dispatch_action(&mut self, state: &mut AutoCommit, action: ActionKind, part: Option<usize>) {
        // Snapshot the target as owned strings so the drive borrow ends before
        // we mutate `self` / spawn a thread.
        let (leaf, fstype) = {
            let Some(drive) = self.drive() else { return };
            let p = part.and_then(|i| drive.partitions.get(i));
            let leaf = p.map_or_else(|| drive.name.clone(), |p| p.name.clone());
            let fstype = p.and_then(|p| p.fstype.clone());
            (leaf, fstype)
        };
        let dev = format!("/dev/{leaf}");

        if self.live {
            let (tx, rx) = mpsc::channel();
            let op = action.op();
            let refresh = action.changes_topology();
            let done_label = action.label().trim_end_matches('…').to_string();
            thread::spawn(move || {
                let (text, error) = match ops::run(op, &leaf, fstype.as_deref()) {
                    Ok(msg) => (msg, None),
                    Err(e) => {
                        let text = format!("{done_label} failed — {e}");
                        (text, Some(e))
                    }
                };
                let _ = tx.send(ActionResult {
                    text,
                    error,
                    refresh,
                });
            });
            self.action_rx = Some(rx);
            let msg = format!("{} {dev}…", action.gerund());
            self.status.clone_from(&msg);
            self.commit_shared(
                state,
                &crate::resource::DisksAction::SetStatus {
                    status: msg.clone(),
                },
            );
        } else {
            // Hermetic build / --shots: state what *would* run, no side effects.
            let plan = match action {
                ActionKind::Mount => format!(
                    "Would run udisks2 Filesystem.Mount on {dev} (set DISKS_LIVE to execute)"
                ),
                ActionKind::Unmount => format!(
                    "Would run udisks2 Filesystem.Unmount on {dev} (set DISKS_LIVE to execute)"
                ),
                ActionKind::Format => {
                    format!("Would run udisks2 Block.Format on {dev} (set DISKS_LIVE to execute)")
                }
                ActionKind::Benchmark => {
                    format!("Would run a read/seek benchmark on {dev} (set DISKS_LIVE to execute)")
                }
                ActionKind::Eject => {
                    format!("Would power off {dev} via udisks2 (set DISKS_LIVE to execute)")
                }
            };
            self.status.clone_from(&plan);
            self.commit_shared(
                state,
                &crate::resource::DisksAction::SetStatus {
                    status: plan.clone(),
                },
            );
        }
        self.dirty = true;
    }

    fn confirm_pending(&mut self, state: &mut AutoCommit) {
        // Locked until the typed name matches the target device.
        if !self.confirm_ready() {
            return;
        }
        let Some(p) = self.pending.take() else { return };
        self.confirm_input.set_text("");
        self.confirm_input.set_focus(false);
        self.commit_shared(
            state,
            &crate::resource::DisksAction::SetPending {
                pending: String::new(),
            },
        );
        self.dispatch_action(state, p.action, p.part);
    }

    fn cancel_pending(&mut self, state: &mut AutoCommit) {
        self.pending = None;
        self.confirm_input.set_text("");
        self.confirm_input.set_focus(false);
        self.commit_shared(
            state,
            &crate::resource::DisksAction::SetPending {
                pending: String::new(),
            },
        );
    }

    fn select(&mut self, state: &mut AutoCommit, idx: usize) {
        if idx < self.topo.drives.len() {
            let changed = idx != self.selected;
            self.selected = idx;
            self.commit_shared(
                state,
                &crate::resource::DisksAction::SetSelected { index: idx as u64 },
            );
            // A new drive resets the partition focus to the whole disk so the
            // detail + action bar can never point at a stale partition.
            if changed {
                self.selected_part = None;
                self.commit_shared(
                    state,
                    &crate::resource::DisksAction::SetSelectedPart { part: -1 },
                );
                self.content_scroll = 0.0;
            }
            self.dirty = true;
        }
    }

    /// Select a partition within the current drive, or the whole disk (`None`).
    /// Clicking the already-selected partition toggles back to whole-disk.
    fn select_part(&mut self, state: &mut AutoCommit, part: Option<usize>) {
        let next = match part {
            Some(i) if self.selected_part == Some(i) => None,
            other => other,
        };
        self.selected_part = next;
        self.commit_shared(
            state,
            &crate::resource::DisksAction::SetSelectedPart {
                part: next.map_or(-1, |index| index as i64),
            },
        );
        self.dirty = true;
    }

    fn set_tab(&mut self, state: &mut AutoCommit, tab: Tab) {
        self.tab = tab;
        self.commit_shared(
            state,
            &crate::resource::DisksAction::SetTab {
                tab: tab.key().to_string(),
            },
        );
        self.content_scroll = 0.0;
        self.dirty = true;
    }

    /// Resolve an operation payload against the latest locally-probed
    /// topology. Resource invocations may arrive after a collaborator's
    /// document snapshot or a search result has gone stale, so a leaf name in
    /// a payload is never authority to touch an arbitrary `/dev/*` node.
    ///
    /// The return value deliberately derives the filesystem type from the
    /// current probe instead of trusting the serialized payload.
    fn revalidate_operation_target(
        &self,
        action: ActionKind,
        leaf: &str,
    ) -> Result<Option<String>, String> {
        for drive in &self.topo.drives {
            if drive.name == leaf {
                return match action {
                    ActionKind::Format | ActionKind::Benchmark => Ok(None),
                    ActionKind::Eject if drive.removable => Ok(None),
                    ActionKind::Eject => Err(format!("/dev/{leaf} is not removable")),
                    ActionKind::Mount | ActionKind::Unmount => Err(format!(
                        "/dev/{leaf} is a whole disk; select one of its filesystems"
                    )),
                };
            }
            for part in &drive.partitions {
                if part.name != leaf {
                    continue;
                }
                return match action {
                    ActionKind::Mount if part.mountpoint.is_none() => Ok(part.fstype.clone()),
                    ActionKind::Unmount if part.mountpoint.is_some() => Ok(part.fstype.clone()),
                    ActionKind::Mount => Err(format!("/dev/{leaf} is already mounted")),
                    ActionKind::Unmount => Err(format!("/dev/{leaf} is not mounted")),
                    ActionKind::Format | ActionKind::Benchmark => Ok(part.fstype.clone()),
                    ActionKind::Eject => Err("eject targets a whole removable drive".into()),
                };
            }
        }
        Err(format!(
            "/dev/{leaf} is no longer present; rescan storage and try again"
        ))
    }
}

/// Centred empty-state card + its Rescan button, given the viewport and the
/// (drive-less) header rect. Shared by `draw_empty_state` and `compute_layout`
/// so the painted button and its hit target never drift.
fn empty_state_rects(viewport: Rect, header: Rect) -> (Rect, Rect) {
    let region = Rect::new(header.x0, header.y0, header.x1, viewport.y1 - PAD);
    let cw = 460.0_f64.min(region.width().max(1.0));
    let ch = 210.0;
    let cx = f64::midpoint(region.x0, region.x1);
    let cy = f64::midpoint(region.y0, region.y1);
    let card = Rect::new(cx - cw / 2.0, cy - ch / 2.0, cx + cw / 2.0, cy + ch / 2.0);
    let bw = 130.0;
    let btn = Rect::new(cx - bw / 2.0, card.y1 - 46.0, cx + bw / 2.0, card.y1 - 14.0);
    (card, btn)
}

/// Height of the Overview's capacity / SMART column block, sized to whichever
/// column is taller so the two-up layout never clips a mounted filesystem row.
fn overview_cols_h(drive: &Drive) -> f64 {
    let mounted = drive
        .partitions
        .iter()
        .filter(|p| p.mountpoint.is_some())
        .count();
    let cap_h = (24.0 + (mounted.max(1) as f64) * 62.0).max(90.0);
    let smart_h = 258.0;
    cap_h.max(smart_h)
}

/// Proportional partition segment rects across the device map, in draw order.
/// `(rect, Some(index))` for real partitions and a trailing `(rect, None)` for
/// the unallocated free tail (when present). Tiny partitions are widened to a
/// 6px floor so they stay both visible and clickable. Shared by `compute_layout`
/// (hit-testing) and `draw_partition_map` so geometry never drifts.
fn partition_segments(drive: &Drive, map: Rect) -> Vec<(Rect, Option<usize>)> {
    let total = drive.size.max(1) as f64;
    let mut out = Vec::new();
    let mut x = map.x0;
    for (i, p) in drive.partitions.iter().enumerate() {
        let frac = p.size as f64 / total;
        let mut w = frac * map.width();
        if w < 6.0 {
            w = 6.0;
        }
        out.push((
            Rect::new(x, map.y0 + 4.0, x + w - 2.0, map.y1 - 4.0),
            Some(i),
        ));
        x += w;
    }
    if drive.free_tail() > 0 && x < map.x1 - 2.0 {
        out.push((Rect::new(x, map.y0 + 4.0, map.x1 - 2.0, map.y1 - 4.0), None));
    }
    out
}

// ---- theme-derived colour helpers --------------------------------------

fn kind_color(theme: &Theme, kind: DriveKind) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        DriveKind::Nvme => c.accent[5],
        DriveKind::Ssd => c.semantic.info,
        DriveKind::Hdd => c.neutral[5],
        DriveKind::Usb => c.semantic.success,
        DriveKind::Zram => c.accent[7],
        DriveKind::Virtual | DriveKind::Other => c.neutral[4],
    }
}

fn role_color(theme: &Theme, role: PartRole) -> Color {
    let c = &theme.tokens.colors;
    match role {
        PartRole::Efi => c.semantic.info,
        PartRole::Boot => c.accent[7],
        PartRole::Root => c.accent[5],
        PartRole::Home => c.semantic.success,
        PartRole::Swap => c.semantic.warning,
        PartRole::Data => c.accent[6],
        PartRole::Free => c.neutral[3],
    }
}

fn usage_color(theme: &Theme, frac: f64) -> Color {
    let c = &theme.tokens.colors;
    if frac >= 0.9 {
        c.semantic.danger
    } else if frac >= 0.7 {
        c.semantic.warning
    } else {
        c.semantic.success
    }
}

fn temp_color(theme: &Theme, temp: i64) -> Color {
    let c = &theme.tokens.colors;
    if temp >= 60 {
        c.semantic.danger
    } else if temp >= 48 {
        c.semantic.warning
    } else {
        c.semantic.info
    }
}

fn status_color(theme: &Theme, status: SmartStatus) -> Color {
    let c = &theme.tokens.colors;
    match status {
        SmartStatus::Passed => c.semantic.success,
        SmartStatus::Warning => c.semantic.warning,
        SmartStatus::Failing => c.semantic.danger,
        SmartStatus::Unknown => c.neutral[5],
    }
}

impl Widget for DisksApp {
    ui::provides!(dyn ui::resources::PublishesResources);

    fn measure(
        &mut self,
        _known: ui::taffy::Size<Option<f32>>,
        _available: ui::taffy::Size<ui::taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> ui::taffy::Size<f32> {
        // The band scale follows the renderer that will paint; measuring here
        // means the event path never lays out 1× bands under a 200 % frame.
        self.k = renderer_scale(text_renderer);
        ui::taffy::Size::ZERO
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["disks.operation"]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self
            .completed_action
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return self.completed_action.take();
        }
        None
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one ordered reconciliation pass keeps the CRDT projection and operation lifecycle atomic"
    )]
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        if !self.init.seeded {
            if DisksDoc::is_empty(state) {
                // Live: reflect the real system only. If the probe finds
                // nothing (or the tools are missing) we seed an *empty*
                // topology and show an honest empty state — never the curated
                // sample, which would fabricate devices that don't exist.
                // The sample is reserved for `--shots`/demo (non-live) builds.
                let topo = if std::env::var("DISKS_LIVE").is_ok() {
                    crate::probe::probe().unwrap_or_default()
                } else {
                    Topology::sample()
                };
                self.commit_topology(state, &topo);
                self.commit_shared(
                    state,
                    &crate::resource::DisksAction::SetTab {
                        tab: "overview".into(),
                    },
                );
                self.commit_shared(
                    state,
                    &crate::resource::DisksAction::SetSelected { index: 0 },
                );
                self.commit_shared(
                    state,
                    &crate::resource::DisksAction::SetSelectedPart { part: -1 },
                );
                self.commit_shared(
                    state,
                    &crate::resource::DisksAction::SetPending {
                        pending: String::new(),
                    },
                );
                self.commit_shared(
                    state,
                    &crate::resource::DisksAction::SetStatus {
                        status: String::new(),
                    },
                );
            }
            self.init.seeded = true;
        }

        if let Some(json) = DisksDoc::topology_json(state)
            && json != self.topo_json_cache
            && let Ok(t) = serde_json::from_str::<Topology>(&json)
        {
            self.topo = t;
            self.topo_json_cache = json;
            self.dirty = true;
        }

        let sel = DisksDoc::selected(state).min(self.topo.drives.len().saturating_sub(1));
        if sel != self.selected {
            self.selected = sel;
            self.dirty = true;
        }
        // Partition selection, clamped against the current drive's partitions.
        let part = DisksDoc::selected_part(state)
            .filter(|&i| self.drive().is_some_and(|d| i < d.partitions.len()));
        if part != self.selected_part {
            self.selected_part = part;
            self.dirty = true;
        }
        let tab = Tab::from_key(&DisksDoc::tab(state));
        if tab != self.tab {
            self.tab = tab;
            self.dirty = true;
        }
        let pending = DisksDoc::pending(state);
        let parsed = parse_pending(&pending);
        if parsed.is_some() != self.pending.is_some() {
            if parsed.is_some() {
                self.confirm_input.set_text("");
                self.confirm_input.set_focus(true);
            } else {
                self.confirm_input.set_focus(false);
            }
            self.pending = parsed;
            self.dirty = true;
        }
        let status = DisksDoc::status(state);
        if status != self.status {
            self.status = status;
            self.dirty = true;
        }

        // Drain a completed background action (the root widget's `update` is
        // not pumped by the runtime, so we poll here — `sync` runs every frame
        // while `needs_animation_frame` holds).
        let mut result = None;
        let mut clear = false;
        if let Some(rx) = &self.action_rx {
            match rx.try_recv() {
                Ok(res) => {
                    result = Some(res);
                    clear = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => clear = true,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let had_result = result.is_some();
        if let Some(res) = result {
            if let Some(invocation) = self.action_invocation.take() {
                self.completed_action = Some(match &res.error {
                    Some(error) => tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "disks.operation_failed".into(),
                            message: error.clone(),
                            details: None,
                            retryable: false,
                        },
                    },
                    None => tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    },
                });
            }
            self.status.clone_from(&res.text);
            self.commit_shared(
                state,
                &crate::resource::DisksAction::SetStatus {
                    status: res.text.clone(),
                },
            );
            // A mount/format/unmount/eject changed the on-disk topology.
            if res.refresh
                && self.live
                && let Some(topo) = crate::probe::probe()
            {
                self.commit_topology(state, &topo);
            }
            self.dirty = true;
        }
        if clear {
            self.action_rx = None;
            if !had_result && let Some(invocation) = self.action_invocation.take() {
                self.completed_action = Some(tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "disks.worker_disconnected".into(),
                        message: "disk operation worker disconnected without a result".into(),
                        details: None,
                        retryable: false,
                    },
                });
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = leaf_node(
            taffy,
            Style {
                size: ui::taffy::Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                flex_grow: 1.0,
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // Now that the width is known, reconcile the flap so a window resize
        // across the breakpoint drops a stale toggle and returns to the new
        // layout's default (wide ⇒ list docked-open, narrow ⇒ overlay-closed).
        let split = self.split_layout();
        self.flap.reconcile(split);
        // Settle the very first frame to its resolved default so a cold start
        // at any width paints a static layout (no list sliding in/out).
        if !self.init.laid_out {
            self.flap.snap(self.flap.is_open(split));
            self.init.laid_out = true;
        }
    }

    fn update(&mut self, _state: &mut AppModel, _path: &ModelPath) -> bool {
        // Advance the eased slide every frame so the flap opens/collapses
        // smoothly. Mirrors the shared `NavSplit`'s fixed-step tick.
        let animating = self.flap.tick(self.split_layout(), 1.0 / 60.0);
        if animating {
            self.dirty = true;
        }
        animating
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames (and thus `sync` polling) coming while a background
        // action is in flight, so its result surfaces promptly.
        self.flap.is_animating(self.split_layout()) || self.action_rx.is_some()
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Every label size and every 1×-designed band reads this frame's
        // renderer scale; set it before the layout below is computed.
        refresh_type_ramp(text_renderer, theme);
        self.k = renderer_scale(text_renderer);
        let c = &theme.tokens.colors;
        // App background.
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);

        // Measure content extents + clamp scroll before laying out (the list
        // rows are positioned from `list_scroll`).
        let n = self.topo.drives.len();
        let (ch, lh) = match self.drive() {
            Some(d) => (
                self.content_height(d),
                n as f64 * (CARD_H * self.k) + n.saturating_sub(1) as f64 * CARD_GAP,
            ),
            None => (0.0, 0.0),
        };
        self.content_h = ch;
        self.list_h = lh;
        let v = self.rect;
        let list_body_h = (v.y1 - PAD) - ((TOPBAR_H * self.k) + PAD + (LIST_HEADER_H * self.k));
        let content_top = (TOPBAR_H * self.k)
            + PAD
            + (HEADER_H * self.k)
            + 12.0
            + (ACTIONBAR_H * self.k)
            + 12.0
            + (TAB_H * self.k)
            + PAD;
        let content_avail = (v.y1 - PAD) - content_top;
        self.content_scroll = self
            .content_scroll
            .clamp(0.0, (ch - content_avail).max(0.0));
        self.list_scroll = self.list_scroll.clamp(0.0, (lh - list_body_h).max(0.0));

        let lay = self.compute_layout();
        let tr = text_renderer;

        // Detail pane (drawn first; in overlay mode the flap sits on top).
        if let Some(drive) = self.drive() {
            Self::draw_header(scene, tr, theme, transform, &lay, drive, self.k);
            self.draw_action_bar(scene, tr, theme, transform, &lay, drive);
            self.draw_tabs(scene, tr, theme, transform, &lay);
            // Content scrolls within its own clip so it stays usable when the
            // window is short/narrow. `ct` shifts the content up by the scroll;
            // interactive rects live in unscrolled coords (see handle_event).
            let ct = transform * Affine::translate((0.0, -self.content_scroll));
            let natural_bottom = (lay.content.y0 + self.content_h).max(lay.content.y1);
            let ext = Rect::new(
                lay.content.x0,
                lay.content.y0,
                lay.content.x1,
                natural_bottom,
            );
            scene.push_layer(
                Fill::NonZero,
                ui::vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &lay.content,
            );
            match self.tab {
                Tab::Overview => self.draw_overview(scene, tr, theme, ct, &lay, drive),
                Tab::Smart => Self::draw_smart_tab(scene, tr, theme, ct, ext, drive),
                Tab::Partitions => self.draw_partitions_tab(scene, tr, theme, ct, ext, drive),
            }
            scene.pop_layer();
        } else {
            self.draw_empty_state(scene, tr, theme, transform, &lay);
        }

        self.draw_statusline(scene, tr, theme, transform);

        // Dimming scrim behind the open overlay flap (eased with the reveal).
        if lay.scrim.width() > 0.0 {
            let alpha = (self.flap.reveal() * 0.5 * 255.0) as u8;
            scene.fill(
                Fill::NonZero,
                transform,
                with_alpha(c.surfaces.scrim, alpha),
                None,
                &lay.scrim,
            );
        }

        // Drive-list flap. In overlay mode it floats above the content with an
        // opaque backdrop so the list reads cleanly; in docked mode it is the
        // permanent left column and is only hidden mid-collapse.
        if self.flap.reveal() > 0.001 {
            if lay.split == SplitLayout::Overlay {
                let panel = lay.sidebar.intersect(self.rect);
                scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &panel);
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    ui::surface::edge_on(c.surfaces.base),
                    None,
                    &Line::new(
                        Point::new(panel.x1, panel.y0),
                        Point::new(panel.x1, panel.y1),
                    ),
                );
            }
            self.draw_drive_list(scene, tr, theme, transform, &lay);
        }

        // Top bar last so the hamburger + title stay above the flap panel.
        self.draw_topbar(scene, tr, theme, transform, &lay);

        if let (Some(ml), Some(p)) = (lay.modal, self.pending) {
            self.draw_modal(scene, tr, theme, transform, &ml, p);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let lay = self.compute_layout();

        // The confirmation text is deliberately local-only: it gates a
        // machine-local destructive operation and must never enter the CRDT.
        // Forward the complete event stream so selection, clipboard, IME and
        // drag behavior come from the shared editor rather than a key shim.
        if self.pending.is_some() {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.handle_cursor_moved(*position, ctx.scale_factor, &lay);
            }
            if let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
                && matches!(logical_key, Key::Named(NamedKey::Escape | NamedKey::Enter))
            {
                self.handle_key_press(logical_key, ctx.state, &lay, ctx.handled);
                return;
            }
            self.confirm_input.handle_event(event, ctx);
            if self.confirm_input.take_dirty().is_some() {
                self.dirty = true;
            }
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            ) {
                self.handle_mouse_click(ctx.state, &lay, ctx.handled);
            } else {
                // The open modal is fully blocking, including IME events and
                // clicks/releases that the editor itself did not consume.
                *ctx.handled = true;
            }
            return;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(*position, ctx.scale_factor, &lay);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta, &lay, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_mouse_click(ctx.state, &lay, ctx.handled);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.handle_key_press(logical_key, ctx.state, &lay, ctx.handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "DisksApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the custom canvas exports its complete semantic tree beside its visual layout"
    )]
    fn accessibility(&self) -> Option<AccessNode> {
        let lay = self.compute_layout();
        let mut root = AccessNode::new(Role::Window)
            .with_name("Disks")
            .with_bounds(self.rect);

        // A custom-painted canvas has no retained children for AccessKit to
        // discover. Mirror every visible, meaningful control as a stable
        // synthetic node so keyboard users, screen readers and agent driving
        // all address the exact same operations as pointer input.
        if let (Some(ml), Some(pending)) = (lay.modal, self.pending) {
            let expected = self.pending_target_leaf(pending);
            let ready = self.confirm_ready();
            let mut dialog = AccessNode::new(Role::Dialog)
                .with_name(pending.action.confirm_title(&format!("/dev/{expected}")))
                .with_description(
                    pending
                        .action
                        .confirm_body(&format!("/dev/{expected}"), self.live),
                )
                .with_bounds(ml.card)
                .with_hashed_key("disks.confirmation-dialog")
                .with_state(AccessState::new().modal(true));
            dialog.children.push(
                AccessNode::new(Role::TextInput)
                    .with_name(format!("Type {expected} to confirm"))
                    .with_description("The erase button unlocks only after an exact match.")
                    .with_value(self.confirm_input.value())
                    .with_bounds(ml.input)
                    .with_action_target("disks.modal.confirm-input")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.confirm_input.is_focused()),
                    ),
            );
            dialog.children.push(
                AccessNode::new(Role::Button)
                    .with_name("Cancel")
                    .with_bounds(ml.cancel)
                    .with_action_target("disks.modal.cancel")
                    .with_state(AccessState::new().focusable(true)),
            );
            dialog.children.push(
                AccessNode::new(Role::Button)
                    .with_name(if pending.action.destructive() {
                        "Erase"
                    } else {
                        "Confirm"
                    })
                    .with_description(if ready {
                        "Confirmation text matches the target device."
                    } else {
                        "Type the exact device name to enable this action."
                    })
                    .with_bounds(ml.confirm)
                    .with_action_target("disks.modal.confirm")
                    .with_state(AccessState::new().focusable(true).disabled(!ready)),
            );
            root.children.push(dialog);
        } else {
            // Grouping nodes carry their painted bounds too: a geometry audit
            // (and a screen reader's spatial navigation) only descends into
            // a container it can place, so bounded controls under an
            // unbounded Toolbar / List / TabBar were invisible to both.
            let mut toolbar = AccessNode::new(Role::Toolbar)
                .with_name("Storage controls")
                .with_hashed_key("disks.toolbar")
                .with_bounds(Rect::new(
                    self.rect.x0,
                    self.rect.y0,
                    self.rect.x1,
                    self.rect.y0 + TOPBAR_H * self.k,
                ));
            if let Some(menu) = lay.hamburger {
                toolbar.children.push(
                    AccessNode::new(Role::Button)
                        .with_name(if self.list_open() {
                            "Hide drives"
                        } else {
                            "Show drives"
                        })
                        .with_bounds(menu)
                        .with_action_target("disks.menu")
                        .with_state(AccessState::new().focusable(true)),
                );
            }
            toolbar.children.push(
                AccessNode::new(Role::Button)
                    .with_name("Rescan storage devices")
                    .with_description("Refresh the local block-device inventory.")
                    .with_bounds(lay.refresh)
                    .with_action_target("disks.rescan")
                    .with_state(AccessState::new().focusable(true)),
            );
            root.children.push(toolbar);

            if self.list_open() {
                let mut list = AccessNode::new(Role::List)
                    .with_name("Drives")
                    .with_hashed_key("disks.drives")
                    .with_bounds(lay.list_header.union(lay.list_body));
                let n = self.topo.drives.len();
                for (i, drive) in self.topo.drives.iter().enumerate() {
                    let Some(&bounds) = lay.cards.get(i) else {
                        continue;
                    };
                    if bounds.y1 < lay.list_body.y0 || bounds.y0 > lay.list_body.y1 {
                        continue;
                    }
                    list.children.push(
                        AccessNode::new(Role::ListItem)
                            .with_name(format!("{} /dev/{}", drive.model, drive.name))
                            // The painted detail header elides a long serial to
                            // its line; the unelided serial stays reachable here
                            // for assistive technology and automation.
                            .with_description(format!(
                                "{}; {}; {}{}",
                                drive.kind.long(),
                                bytes_si(drive.size),
                                drive.smart_status().label(),
                                drive
                                    .serial
                                    .as_ref()
                                    .map(|s| format!("; S/N {s}"))
                                    .unwrap_or_default()
                            ))
                            .with_position(i + 1, n)
                            .with_bounds(bounds)
                            .with_action_target(format!("disks.drive:{}", drive.name))
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .selected(i == self.selected),
                            ),
                    );
                }
                root.children.push(list);
            }

            let mut tabs = AccessNode::new(Role::TabBar)
                .with_name("Drive details")
                .with_hashed_key("disks.drive-details");
            if let Some(strip) = lay.tabs.iter().map(|(r, _)| *r).reduce(|a, b| a.union(b)) {
                tabs = tabs.with_bounds(strip);
            }
            for (bounds, tab) in &lay.tabs {
                tabs.children.push(
                    AccessNode::new(Role::Tab)
                        .with_name(tab.title())
                        .with_bounds(*bounds)
                        .with_action_target(format!("disks.tab:{}", tab.key()))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .selected(*tab == self.tab),
                        ),
                );
            }
            root.children.push(tabs);

            if let Some(drive) = self.drive() {
                let mut actions = AccessNode::new(Role::Group)
                    .with_name(format!("Actions for /dev/{}", drive.name))
                    .with_hashed_key("disks.drive-actions")
                    .with_bounds(lay.action_bar);
                for (bounds, action, enabled) in &lay.action_buttons {
                    actions.children.push(
                        AccessNode::new(Role::Button)
                            .with_name(action.label())
                            .with_description(action.describe())
                            .with_bounds(*bounds)
                            .with_action_target(format!("disks.action:{}", action.key()))
                            .with_state(AccessState::new().focusable(*enabled).disabled(!enabled)),
                    );
                }
                root.children.push(actions);

                if self.tab == Tab::Overview {
                    root.children.push(
                        AccessNode::new(Role::Button)
                            .with_name("Select whole disk")
                            .with_bounds(lay.whole_disk_pill)
                            .with_action_target("disks.part:whole")
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .selected(self.part_index().is_none()),
                            ),
                    );
                }
                let parts: &[(Rect, usize)] = match self.tab {
                    Tab::Overview => lay.part_segments.as_slice(),
                    Tab::Partitions => lay.part_rows.as_slice(),
                    Tab::Smart => &[],
                };
                for (bounds, i) in parts {
                    let Some(part) = drive.partitions.get(*i) else {
                        continue;
                    };
                    root.children.push(
                        AccessNode::new(Role::Button)
                            .with_name(format!("Select partition /dev/{}", part.name))
                            .with_description(format!(
                                "{}; {}",
                                part.display_label(),
                                bytes_si(part.size)
                            ))
                            .with_bounds(*bounds)
                            .with_action_target(format!("disks.part:{}", part.name))
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .selected(self.part_index() == Some(*i)),
                            ),
                    );
                }
            }
        }
        if !self.status.is_empty() {
            root.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(self.status.clone())
                    .with_hashed_key("disks.status")
                    .with_live(Politeness::Polite),
            );
        }
        Some(root)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &A11yAction,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        if let A11yAction::SetValue(value) = action
            && target == "disks.modal.confirm-input"
            && self.pending.is_some()
        {
            self.confirm_input.set_text(value);
            self.confirm_input.set_focus(true);
            self.dirty = true;
            return true;
        }
        if !matches!(action, A11yAction::Activate) {
            return matches!(action, A11yAction::Focus);
        }
        match target {
            "disks.menu" if self.compute_layout().hamburger.is_some() => {
                self.flap.toggle(self.split_layout());
            }
            "disks.rescan" if self.pending.is_none() => self.do_refresh(state),
            "disks.modal.cancel" if self.pending.is_some() => self.cancel_pending(state),
            "disks.modal.confirm" if self.pending.is_some() => self.confirm_pending(state),
            "disks.part:whole" if self.pending.is_none() => self.select_part(state, None),
            _ if self.pending.is_none() => {
                if let Some(name) = target.strip_prefix("disks.drive:") {
                    if let Some(i) = self.topo.drives.iter().position(|drive| drive.name == name) {
                        self.select(state, i);
                    } else {
                        return false;
                    }
                } else if let Some(key) = target.strip_prefix("disks.tab:") {
                    self.set_tab(state, Tab::from_key(key));
                } else if let Some(key) = target.strip_prefix("disks.action:") {
                    let Some(kind) = ActionKind::from_key(key) else {
                        return false;
                    };
                    if !self
                        .drive()
                        .is_some_and(|drive| self.action_enabled(drive, kind))
                    {
                        return false;
                    }
                    self.trigger_action(state, kind);
                } else if let Some(name) = target.strip_prefix("disks.part:") {
                    let Some(i) = self.drive().and_then(|drive| {
                        drive.partitions.iter().position(|part| part.name == name)
                    }) else {
                        return false;
                    };
                    self.select_part(state, Some(i));
                } else {
                    return false;
                }
            }
            _ => return false,
        }
        self.dirty = true;
        true
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

impl ui::resources::PublishesResources for DisksApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the topology door — keep only live extras (none today).
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if self.is_hosted() && id == "disks" {
            return None;
        }
        fn find(resources: &[Resource], id: &str) -> Option<Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource.clone());
                }
                if let Some(found) = find(&resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        find(&self.publish_resources(doc), id)
    }
}

impl DisksApp {
    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != OperationKind::Effect || type_id != "disks.operation" {
            return Ok(None);
        }
        if self.action_rx.is_some() {
            return Err("another disk operation is still running".into());
        }
        let args: Vec<String> = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        let action = ActionKind::from_key(args.first().map_or("", String::as_str))
            .ok_or_else(|| "unknown disk operation".to_string())?;
        let leaf = args
            .get(1)
            .ok_or_else(|| "missing device".to_string())?
            .clone();
        if !self.live {
            return Err("live disk access is disabled".into());
        }
        // Re-check against the locally probed inventory immediately before
        // execution. This rejects stale remote/search invocations and ignores
        // a caller-supplied filesystem type in favour of current system data.
        let fstype = self.revalidate_operation_target(action, &leaf)?;
        let (tx, rx) = mpsc::channel();
        let refresh = action.changes_topology();
        thread::spawn(move || {
            let (text, error) = match ops::run(action.op(), &leaf, fstype.as_deref()) {
                Ok(text) => (text, None),
                Err(error) => (format!("{} failed — {error}", action.label()), Some(error)),
            };
            let _ = tx.send(ActionResult {
                text,
                error,
                refresh,
            });
        });
        self.action_rx = Some(rx);
        self.action_invocation = Some(invocation);
        self.dirty = true;
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

impl DisksApp {
    /// Track the pointer and recompute every hover flag (part of
    /// [`Widget::handle_event`]).
    fn handle_cursor_moved(
        &mut self,
        position: PhysicalPosition<f64>,
        scale_factor: f64,
        lay: &Layout,
    ) {
        let p = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_mouse = p;
        // Content-space point: content is drawn shifted up by the scroll,
        // so unshift the pointer before testing content-scoped rects.
        let pc = Point::new(p.x, p.y + self.content_scroll);
        let in_content = p.y >= lay.content.y0 - 6.0
            && p.y <= lay.content.y1
            && p.x >= lay.content.x0 - 8.0
            && p.x <= lay.content.x1 + 8.0;
        let prev = (
            self.hover_card,
            self.hover_action,
            self.hover_tab,
            self.hover_toggle.menu,
            self.hover_part,
            self.hover_toggle.whole_disk,
        );
        // The drive list only receives hover when it's actually shown.
        let list_shown = self.list_open();
        self.hover_card = if list_shown && lay.list_body.contains(p) {
            lay.cards.iter().position(|r| r.contains(p))
        } else {
            None
        };
        // Only applicable action buttons light up.
        self.hover_action = lay
            .action_buttons
            .iter()
            .find(|(r, _, en)| *en && r.contains(p))
            .map(|(_, a, _)| *a);
        self.hover_tab = lay
            .tabs
            .iter()
            .find(|(r, _)| r.contains(p))
            .map(|(_, t)| *t);
        self.hover_toggle.menu = lay.hamburger.is_some_and(|hb| hb.contains(p));
        self.hover_part = if in_content {
            match self.tab {
                Tab::Overview => lay
                    .part_segments
                    .iter()
                    .find(|(r, _)| r.contains(pc))
                    .map(|(_, i)| *i),
                Tab::Partitions => lay
                    .part_rows
                    .iter()
                    .find(|(r, _)| r.contains(pc))
                    .map(|(_, i)| *i),
                Tab::Smart => None,
            }
        } else {
            None
        };
        self.hover_toggle.whole_disk =
            in_content && self.tab == Tab::Overview && lay.whole_disk_pill.contains(pc);
        let now = (
            self.hover_card,
            self.hover_action,
            self.hover_tab,
            self.hover_toggle.menu,
            self.hover_part,
            self.hover_toggle.whole_disk,
        );
        if prev != now {
            self.dirty = true;
        }
    }

    /// Scroll the drive list or the detail content, whichever the pointer
    /// is over (part of [`Widget::handle_event`]).
    fn handle_mouse_wheel(&mut self, delta: &MouseScrollDelta, lay: &Layout, handled: &mut bool) {
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(-y * 48.0),
            MouseScrollDelta::PixelDelta(pos) => -pos.y,
        };
        let p = self.last_mouse;
        if self.list_open() && lay.list_body.contains(p) {
            let max = (self.list_h - lay.list_body.height()).max(0.0);
            self.list_scroll = (self.list_scroll + dy).clamp(0.0, max);
            *handled = true;
        } else if lay.content.contains(p) {
            let max = (self.content_h - lay.content.height()).max(0.0);
            self.content_scroll = (self.content_scroll + dy).clamp(0.0, max);
            *handled = true;
        }
        self.dirty = true;
    }

    /// A left click: modal buttons, the hamburger, the overlay scrim, the
    /// list rows, the tabs, the action bar, and content-scoped hit targets
    /// (whole-disk pill / partition map / partition rows), in priority
    /// order (part of [`Widget::handle_event`]).
    fn handle_mouse_click(&mut self, state: &mut AutoCommit, lay: &Layout, handled: &mut bool) {
        let p = self.last_mouse;
        // Modal first — it is fully blocking.
        if let Some(ml) = lay.modal {
            if ml.confirm.contains(p) {
                self.confirm_pending(state);
            } else if ml.cancel.contains(p) {
                self.cancel_pending(state);
            }
            // Clicks anywhere else while modal is open are swallowed.
            *handled = true;
            self.dirty = true;
            return;
        }
        // Hamburger toggles the overlay flap (narrow window only).
        if lay.hamburger.is_some_and(|hb| hb.contains(p)) {
            self.flap.toggle(self.split_layout());
            *handled = true;
            self.dirty = true;
            return;
        }
        // While the overlay flap is open, a click on the scrim (i.e.
        // outside the sliding list panel) dismisses it.
        if lay.split == SplitLayout::Overlay
            && self.list_open()
            && lay.scrim.width() > 0.0
            && !lay.sidebar.contains(p)
        {
            self.flap.close();
            *handled = true;
            self.dirty = true;
            return;
        }
        if lay.refresh.contains(p) {
            self.do_refresh(state);
            *handled = true;
            self.dirty = true;
            return;
        }
        // Empty-state Rescan (only present when no drives are listed).
        if lay.empty_rescan.is_some_and(|b| b.contains(p)) {
            self.do_refresh(state);
            *handled = true;
            self.dirty = true;
            return;
        }
        // Drive cards are only clickable when the list is shown.
        if self.list_open()
            && let Some(i) = lay.cards.iter().position(|r| r.contains(p))
        {
            self.select(state, i);
            // Selecting from the overlay flap dismisses it so the
            // detail is immediately visible (single-pane feel).
            if lay.split == SplitLayout::Overlay {
                self.flap.close();
            }
            *handled = true;
            self.dirty = true;
            return;
        }
        if let Some((_, tab)) = lay.tabs.iter().find(|(r, _)| r.contains(p)) {
            self.set_tab(state, *tab);
            *handled = true;
            return;
        }
        // Action-bar buttons — only the applicable ones respond.
        if let Some((_, a, _)) = lay
            .action_buttons
            .iter()
            .find(|(r, _, en)| *en && r.contains(p))
        {
            self.trigger_action(state, *a);
            *handled = true;
            self.dirty = true;
            return;
        }
        // Content selection: whole-disk pill, partition map segment, or
        // partition table row. Test in content space (unshift scroll).
        let pc = Point::new(p.x, p.y + self.content_scroll);
        if self.tab == Tab::Overview && lay.whole_disk_pill.contains(pc) {
            self.select_part(state, None);
            *handled = true;
            self.dirty = true;
            return;
        }
        if lay.content.contains(p) {
            if self.tab == Tab::Overview {
                if let Some((_, i)) = lay.part_segments.iter().find(|(r, _)| r.contains(pc)) {
                    self.select_part(state, Some(*i));
                    *handled = true;
                    self.dirty = true;
                }
            } else if self.tab == Tab::Partitions
                && let Some((_, i)) = lay.part_rows.iter().find(|(r, _)| r.contains(pc))
            {
                self.select_part(state, Some(*i));
                *handled = true;
                self.dirty = true;
            }
        }
    }

    /// A key press: while a destructive action is pending, Escape cancels and
    /// Enter confirms; the shared [`TextInput`] owns every editing key.
    /// Otherwise Escape dismisses an open overlay flap and the rest are
    /// navigation shortcuts (part of [`Widget::handle_event`]).
    fn handle_key_press(
        &mut self,
        logical_key: &Key,
        state: &mut AutoCommit,
        lay: &Layout,
        handled: &mut bool,
    ) {
        // Modal key handling — editing keys were already routed through the
        // shared field by `handle_event`.
        if self.pending.is_some() {
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.cancel_pending(state);
                    *handled = true;
                    self.dirty = true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.confirm_pending(state);
                    *handled = true;
                    self.dirty = true;
                }
                _ => {}
            }
            return;
        }
        // Esc dismisses an open overlay flap before any other binding.
        if matches!(logical_key, Key::Named(NamedKey::Escape))
            && lay.split == SplitLayout::Overlay
            && self.list_open()
        {
            self.flap.close();
            *handled = true;
            self.dirty = true;
            return;
        }
        match logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                let n = self.topo.drives.len();
                if n > 0 {
                    self.select(state, (self.selected + 1) % n);
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                let n = self.topo.drives.len();
                if n > 0 {
                    self.select(state, (self.selected + n - 1) % n);
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                let tabs = Tab::all();
                let i = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
                // `(i + 1) % tabs.len()` is always in bounds for a
                // non-empty `tabs`, but `.get` degrades to a no-op
                // instead of indexing blindly.
                if let Some(&next) = tabs.get((i + 1) % tabs.len()) {
                    self.set_tab(state, next);
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowLeft) => {
                let tabs = Tab::all();
                let i = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
                if let Some(&prev) = tabs.get((i + tabs.len() - 1) % tabs.len()) {
                    self.set_tab(state, prev);
                    *handled = true;
                }
            }
            Key::Character(s) => match s.as_str() {
                "1" => {
                    self.set_tab(state, Tab::Overview);
                    *handled = true;
                }
                "2" => {
                    self.set_tab(state, Tab::Smart);
                    *handled = true;
                }
                "3" => {
                    self.set_tab(state, Tab::Partitions);
                    *handled = true;
                }
                "r" | "R" => {
                    self.do_refresh(state);
                    *handled = true;
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Re-probe the live system (no-op fallback to sample). Used by the
    /// Rescan button + `r` key.
    fn do_refresh(&mut self, state: &mut AutoCommit) {
        let topo = if std::env::var("DISKS_LIVE").is_ok() {
            crate::probe::probe().unwrap_or_else(|| self.topo.clone())
        } else {
            // In demo/offscreen mode, just confirm on the status line.
            self.status = "Rescanned storage devices".to_string();
            self.commit_shared(
                state,
                &crate::resource::DisksAction::SetStatus {
                    status: self.status.clone(),
                },
            );
            return;
        };
        self.commit_topology(state, &topo);
        self.status = format!("Rescanned — {} devices", topo.drives.len());
        self.commit_shared(
            state,
            &crate::resource::DisksAction::SetStatus {
                status: self.status.clone(),
            },
        );
        self.dirty = true;
    }
}

fn parse_pending(s: &str) -> Option<Pending> {
    // Format: "<action>:<drive>[:<part|-1>]". The partition field is optional
    // so older docs (drive-only) still parse.
    let mut it = s.split(':');
    let action = ActionKind::from_key(it.next()?)?;
    let drive = it.next()?.parse::<usize>().ok()?;
    let part = match it.next() {
        Some(p) => match p.parse::<i64>().ok()? {
            n if n < 0 => None,
            n => Some(n as usize),
        },
        None => None,
    };
    Some(Pending {
        action,
        drive,
        part,
    })
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use ui::headless::Framed;

    fn seeded_app() -> (DisksApp, AutoCommit) {
        let mut app = DisksApp::new();
        let mut doc = AutoCommit::new();
        let root = ui::automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc)
    }

    fn target_exists(node: &AccessNode, target: &str) -> bool {
        node.action_target.as_deref() == Some(target)
            || node
                .children
                .iter()
                .any(|child| target_exists(child, target))
    }

    /// Sync seeds the sample topology and drive selection round-trips through the CRDT.
    #[test]
    fn sync_and_selection_persist_through_the_document() {
        // --- phase: sync_seeds_sample_topology ---
        {
            let (app, _doc) = seeded_app();
            assert_eq!(app.topo.drives.len(), 4);
            assert_eq!(app.selected, 0);
            assert_eq!(app.tab, Tab::Overview);
        }

        // --- phase: select_persists_to_doc_and_back ---
        {
            let (mut app, mut doc) = seeded_app();
            app.select(&mut doc, 2);
            assert_eq!(DisksDoc::selected(&doc), 2);
            // A fresh app syncing the same doc adopts the selection.
            let mut app2 = DisksApp::new();
            let root = ui::automerge::ROOT;
            app2.sync(&mut doc, &root);
            assert_eq!(app2.selected, 2);
        }
    }

    /// A disk operation from request (modal or direct) through confirmation and the worker result to the reported status.
    #[test]
    fn disk_operation_lifecycle_from_request_to_status() {
        // --- phase: destructive_action_opens_modal ---
        {
            let (mut app, mut doc) = seeded_app();
            app.trigger_action(&mut doc, ActionKind::Format);
            assert!(app.pending.is_some());
            assert!(DisksDoc::pending(&doc).starts_with("format:"));
        }

        // --- phase: non_destructive_action_sets_status_no_modal ---
        {
            let (mut app, mut doc) = seeded_app();
            // Benchmark always applies, so it reports on the status line with no modal.
            app.trigger_action(&mut doc, ActionKind::Benchmark);
            assert!(app.pending.is_none());
            assert!(!DisksDoc::status(&doc).is_empty());
        }

        // --- phase: confirm_requires_typed_name_then_clears_pending ---
        {
            let (mut app, mut doc) = seeded_app();
            app.trigger_action(&mut doc, ActionKind::Format); // whole nvme disk
            // The Erase gate is locked until the exact device name is typed.
            assert!(!app.confirm_ready(), "empty input does not unlock");
            app.confirm_pending(&mut doc);
            assert!(app.pending.is_some(), "confirm is a no-op while locked");

            app.confirm_input.set_text("wrong");
            assert!(!app.confirm_ready());
            app.confirm_pending(&mut doc);
            assert!(app.pending.is_some(), "a mismatched name stays locked");

            app.confirm_input.set_text("nvme0n1"); // the whole-disk device name
            assert!(app.confirm_ready(), "the exact name unlocks Erase");
            app.confirm_pending(&mut doc);
            assert!(app.pending.is_none(), "confirming clears the modal");
            assert_eq!(DisksDoc::pending(&doc), "");
            // Hermetic build reports the plan rather than executing.
            assert!(DisksDoc::status(&doc).contains("Block.Format"));
        }

        // --- phase: worker_result_becomes_terminal_lifecycle_update ---
        {
            let (mut app, mut doc) = seeded_app();
            let invocation = tilo_actions::InvocationId::new();
            let (tx, rx) = mpsc::channel();
            app.action_invocation = Some(invocation);
            app.action_rx = Some(rx);
            tx.send(ActionResult {
                text: "mounted".into(),
                error: None,
                refresh: false,
            })
            .unwrap();
            app.sync(&mut doc, &ui::automerge::ROOT);
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &doc, &ui::automerge::ROOT),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
        }

        // --- phase: root_accessibility_reports_disk_operation_status ---
        {
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
            app.status = "Rescan complete".into();
            let node = app.accessibility().expect("disks exports a window");
            assert_eq!(node.role, Role::Window);
            assert!(
                node.children
                    .iter()
                    .any(|child| child.name.as_deref() == Some("Rescan complete"))
            );
        }
    }

    /// Exported disk resources publish their operations as background jobs whose targets are revalidated against the live topology.
    #[test]
    fn exported_disk_resources_declare_revalidated_background_actions() {
        // --- phase: exported_disk_actions_declare_background_jobs ---
        {
            let (app, doc) = seeded_app();
            for action in ui::resources::resources(&app, &doc)
                .iter()
                .flat_map(|root| root.children.iter())
                .flat_map(|drive| {
                    drive
                        .actions
                        .iter()
                        .chain(drive.children.iter().flat_map(|p| p.actions.iter()))
                })
                .filter(|action| action.kind == OperationKind::Effect)
            {
                assert!(matches!(
                    action.contract.execution,
                    ExecutionMode::Background { cancellable: false }
                ));
                assert_eq!(action.contract.result, ResultSpec::Job);
            }
        }

        // --- phase: disk_resources_publish_whole_drive_format_and_benchmark ---
        {
            let (app, doc) = seeded_app();
            let resources = ui::resources::resources(&app, &doc);
            let disk = resources
                .first()
                .and_then(|root| root.children.first())
                .expect("sample exposes its first disk resource");
            for id in ["format", "benchmark"] {
                assert!(
                    disk.actions.iter().any(|action| action.id == id),
                    "whole-drive resource must publish {id}"
                );
            }
            let format = disk
                .actions
                .iter()
                .find(|action| action.id == "format")
                .expect("format action");
            assert!(matches!(
                format.contract.confirmation,
                ConfirmationPolicy::Phrase { .. }
            ));
        }

        // --- phase: resource_operation_targets_are_revalidated_against_current_topology ---
        {
            let (app, _doc) = seeded_app();
            assert!(
                app.revalidate_operation_target(ActionKind::Benchmark, "nvme0n1")
                    .is_ok()
            );
            assert!(
                app.revalidate_operation_target(ActionKind::Mount, "nvme0n1")
                    .unwrap_err()
                    .contains("whole disk")
            );
            assert!(
                app.revalidate_operation_target(ActionKind::Format, "no-longer-present")
                    .unwrap_err()
                    .contains("no longer present")
            );
        }
    }

    /// Canvas-drawn controls are exposed to accessibility with spoken names and route activation.
    #[test]
    fn accessibility_names_and_routes_canvas_controls() {
        // --- phase: accessibility_exposes_and_routes_canvas_controls ---
        {
            let mut framed = framed(seeded_app(), 1280.0, 800.0);
            let (app, state) = Framed::split(&mut framed);
            let doc = &mut state.doc;
            let node = app.accessibility().expect("disks accessibility tree");
            assert!(target_exists(&node, "disks.rescan"));
            assert!(target_exists(&node, "disks.drive:nvme0n1"));
            assert!(target_exists(&node, "disks.action:format"));
            assert!(Widget::perform_semantic_action(
                app,
                "disks.tab:smart",
                &A11yAction::Activate,
                doc,
                &ui::automerge::ROOT,
            ));
            assert_eq!(app.tab, Tab::Smart);
        }

        // --- phase: icon_only_hamburger_face_has_a_spoken_name ---
        {
            let face = ButtonFace::new(Rect::ZERO, "")
                .icon(Icon::Menu)
                .accessible_name("Show drives");
            assert_eq!(face.spoken_name(), "Show drives");
            assert_eq!(
                ButtonFace::new(Rect::ZERO, "Rescan")
                    .icon(Icon::Refresh)
                    .spoken_name(),
                "Rescan"
            );
        }
    }

    /// The detail pane lays out one card per drive on a shared left edge and inset.
    #[test]
    fn drive_cards_align_to_a_shared_grid() {
        // --- phase: layout_has_one_card_per_drive ---
        {
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let lay = app.compute_layout();
            assert_eq!(lay.cards.len(), 4);
            assert_eq!(lay.tabs.len(), 3);
            assert_eq!(lay.action_buttons.len(), 5);
        }

        // --- phase: detail_cards_share_one_left_edge_and_inner_inset ---
        // The identity header and the action band are stacked cards sharing one
        // left edge, and the band's first button sits on the same inner inset
        // the drive rows and the header text use — one vertical line for the
        // eye to follow down the detail pane.
        {
            let app = at_width(1280.0);
            let lay = app.compute_layout();
            assert!(
                (lay.header.x0 - lay.action_bar.x0).abs() < f64::EPSILON
                    && (lay.header.x1 - lay.action_bar.x1).abs() < f64::EPSILON,
                "header {:?} and action band {:?} share their edges",
                lay.header,
                lay.action_bar
            );
            let first_button = lay
                .action_buttons
                .first()
                .map(|(r, _, _)| *r)
                .expect("buttons");
            assert!(
                (first_button.x0 - (lay.action_bar.x0 + CARD_INSET)).abs() < f64::EPSILON,
                "first action button {first_button:?} starts on the shared card inset"
            );
        }
    }

    /// Lay the app out at a given width through the real `build_layout` +
    /// `apply_layout` passes, so these tests exercise the production
    /// responsiveness path (reconcile + first-frame settle) rather than a
    /// synthetic shortcut.
    fn at_width(width: f64) -> Framed<DisksApp> {
        framed(seeded_app(), width, 800.0)
    }

    /// Mount a seeded app over its document in the shared frame harness and
    /// run its first frame at `w`x`h`.
    fn framed((app, doc): (DisksApp, AutoCommit), w: f64, h: f64) -> Framed<DisksApp> {
        let mut state = ui::State::new();
        assert!(state.adopt_doc(doc));
        Framed::new(state, app, ui::headless::viewport((w, h)), Theme::dark())
    }

    fn layout_at(app: &mut Framed<DisksApp>, width: f64) {
        Framed::harness_mut(app)
            .relayout_at(ui::headless::viewport((width, 800.0)))
            .expect("layout settles");
    }

    /// The drive list docks beside the detail when wide, collapses to an overlay flap when narrow, restores across breakpoints and scrolls.
    #[test]
    fn responsive_layout_docks_collapses_and_scrolls_the_drive_list() {
        // --- phase: wide_window_docks_the_list_beside_the_detail ---
        {
            let app = at_width(1280.0);
            assert_eq!(app.split_layout(), SplitLayout::Docked);
            assert!(app.list_open(), "docked list is open by default");
            let lay = app.compute_layout();
            assert!(lay.hamburger.is_none(), "no hamburger when docked");
            assert_eq!(lay.scrim.width(), 0.0, "no scrim when docked");
            // The detail header begins to the right of the docked list column.
            assert!(
                lay.header.x0 >= PAD + LIST_W,
                "detail is inset past the docked list, got x0={}",
                lay.header.x0
            );
        }

        // --- phase: narrow_window_collapses_list_to_a_closed_overlay ---
        {
            let app = at_width(640.0);
            assert_eq!(app.split_layout(), SplitLayout::Overlay);
            assert!(!app.list_open(), "overlay list is closed by default");
            let lay = app.compute_layout();
            assert!(lay.hamburger.is_some(), "narrow window shows a hamburger");
            assert_eq!(lay.scrim.width(), 0.0, "closed flap paints no scrim");
            // Detail uses the full width (minus the usual padding) when collapsed.
            assert!(
                (lay.content.width() - (app.rect.width() - 2.0 * PAD)).abs() < 1.0,
                "collapsed detail spans the full width"
            );
            // The list panel is slid fully off the left edge while closed.
            assert!(
                lay.sidebar.x1 <= app.rect.x0 + 0.5,
                "closed flap is off-screen left"
            );
        }

        // --- phase: hamburger_summons_overlay_flap_with_scrim ---
        {
            let mut app = at_width(640.0);
            app.flap.snap(true); // settle open deterministically
            assert!(app.list_open());
            let lay = app.compute_layout();
            assert!(
                lay.scrim.width() > 0.0,
                "open overlay flap dims the content behind a scrim"
            );
            // Fully revealed, the list panel sits flush at the left edge.
            assert!(
                (lay.sidebar.x0 - app.rect.x0).abs() < 1.0,
                "revealed overlay list is flush left, got x0={}",
                lay.sidebar.x0
            );
            // Cards ride along with the panel.
            assert_eq!(lay.cards.len(), 4);
            assert!((lay.cards[0].x0 - (app.rect.x0 + PAD)).abs() < 1.0);
        }

        // --- phase: crossing_breakpoint_restores_the_layout_default ---
        {
            // User hides the docked list, then the window narrows: the stale
            // preference is dropped and the overlay default (closed) applies;
            // widening again returns to the docked default (open).
            let mut app = at_width(1280.0);
            app.flap.close();
            assert!(!app.list_open());
            layout_at(&mut app, 640.0);
            assert!(!app.list_open(), "narrow default is closed");
            layout_at(&mut app, 1280.0);
            assert!(app.list_open(), "widening restores the docked-open default");
        }

        // --- phase: narrow_layout_scrolls_both_the_list_and_the_content ---
        {
            // A short window overflows both panes; the wheel must move them.
            let mut h = Harness::new(640.0, 420.0);
            h.app.snap_sidebar(true); // reveal the overlay drive list
            h.relayout();
            assert!(
                h.app.content_h > h.lay().content.height(),
                "content overflows the short window"
            );

            // Scroll the content pane down, then back to the top (clamped at 0).
            // Aim at the content's right edge so the open overlay list (which
            // covers the left) doesn't intercept the wheel.
            let content = h.lay().content;
            let cpt = Point::new(content.x1 - 24.0, content.y0 + 18.0);
            h.wheel(cpt, -3.0);
            assert!(
                h.app.content_scroll > 0.0,
                "wheel scrolls the detail content down"
            );
            h.wheel(cpt, 30.0);
            assert_eq!(h.app.content_scroll, 0.0, "scrolling up clamps at the top");

            // Scroll the drive list.
            assert!(
                h.app.list_h > h.lay().list_body.height(),
                "the drive list overflows too"
            );
            let lpt = center(h.lay().list_body);
            h.wheel(lpt, -3.0);
            assert!(h.app.list_scroll > 0.0, "wheel scrolls the drive list");
        }
    }

    // ================================================================
    // Selection-model behaviour, driven through the production
    // `Widget::handle_event` path (real clicks / wheel), not shortcuts.
    // ================================================================

    use ui::winit::dpi::PhysicalPosition;
    use ui::winit::event::{
        DeviceId, ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent,
    };

    fn device_id() -> DeviceId {
        // winit exposes no public constructor; the all-zero pattern is inert
        // for our widget (the same trick the contacts/terminal harnesses use).
        unsafe { std::mem::zeroed() }
    }

    /// A laid-out + drawn app that pumps real winit events. The draw pass
    /// populates the scroll extents (`content_h` / `list_h`) exactly as the
    /// live renderer does each frame.
    struct Harness {
        app: Framed<DisksApp>,
        w: f64,
        h: f64,
    }

    impl Harness {
        fn new(w: f64, h: f64) -> Self {
            Harness {
                app: framed(seeded_app(), w, h),
                w,
                h,
            }
        }

        fn relayout(&mut self) {
            Framed::harness_mut(&mut self.app)
                .relayout_at(ui::headless::viewport((self.w, self.h)))
                .expect("layout settles");
        }

        /// The app's live document.
        fn doc(&self) -> &AutoCommit {
            &Framed::harness(&self.app).state().doc
        }

        /// The app and its live document, for calls that take both.
        fn app_and_doc(&mut self) -> (&mut DisksApp, &mut AutoCommit) {
            let (app, state) = Framed::split(&mut self.app);
            (app, &mut state.doc)
        }

        /// A real draw pass — settles `content_h` / `list_h` for scroll clamps.
        fn draw(&mut self) {
            let theme = Theme::dark();
            let presence = PresenceCtx::empty();
            let permission = PermissionContext::permissive();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            self.app.draw_with_presence(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &presence,
                &permission,
            );
        }

        fn dispatch(&mut self, event: &WindowEvent) -> bool {
            self.dispatch_with_modifiers(event, Modifiers::default())
        }

        fn dispatch_with_modifiers(&mut self, event: &WindowEvent, mods: Modifiers) -> bool {
            Framed::harness_mut(&mut self.app).dispatch_to_root_with(event, &mods)
        }

        fn move_to(&mut self, p: Point) {
            self.dispatch(&WindowEvent::CursorMoved {
                device_id: device_id(),
                position: PhysicalPosition::new(p.x, p.y),
            });
        }

        fn click(&mut self, p: Point) -> bool {
            self.move_to(p);
            self.dispatch(&WindowEvent::MouseInput {
                device_id: device_id(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            })
        }

        fn wheel(&mut self, p: Point, lines: f32) {
            self.move_to(p);
            self.dispatch(&WindowEvent::MouseWheel {
                device_id: device_id(),
                delta: MouseScrollDelta::LineDelta(0.0, lines),
                phase: ui::winit::event::TouchPhase::Moved,
            });
        }

        fn lay(&self) -> Layout {
            self.app.compute_layout()
        }
    }

    fn center(r: Rect) -> Point {
        Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
    }

    fn char_key(text: &str) -> WindowEvent {
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = Key::Character(text.into());
        event.physical_key = ui::winit::keyboard::PhysicalKey::Unidentified(
            ui::winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.location = ui::winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = Some(text.into());
        WindowEvent::KeyboardInput {
            device_id: device_id(),
            event,
            is_synthetic: false,
        }
    }

    /// Clicking drives, tabs, partition segments, rows and the whole-disk pill repoints the detail and scopes which actions apply.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn pointer_selection_scopes_the_detail_and_its_actions() {
        // --- phase: clicking_a_drive_row_selects_it_and_repoints_the_detail ---
        {
            let mut h = Harness::new(1280.0, 800.0);
            let target = h.lay().cards[2];
            assert!(h.click(center(target)), "a drive row is clickable");
            assert_eq!(h.app.selected, 2, "the clicked row becomes the selection");
            // The detail now points at that exact drive.
            assert_eq!(h.app.drive().unwrap().name, h.app.topo.drives[2].name);
            assert_eq!(
                h.app.selected_part, None,
                "a fresh drive resets to whole-disk"
            );
            assert_eq!(
                DisksDoc::selected(h.doc()),
                2,
                "selection persisted to the CRDT"
            );
        }

        // --- phase: smart_tab_activation_persists_and_draws_its_full_content ---
        {
            let mut h = Harness::new(1280.0, 800.0);
            let selected_before = h.app.selected;
            let smart_tab = h
                .lay()
                .tabs
                .iter()
                .find(|(_, tab)| *tab == Tab::Smart)
                .map(|(rect, _)| *rect)
                .expect("SMART tab has a hit target");

            assert!(h.click(center(smart_tab)), "the SMART tab claims its click");
            assert_eq!(h.app.tab, Tab::Smart);
            assert_eq!(DisksDoc::tab(h.doc()), Tab::Smart.key());
            assert_eq!(
                h.app.selected, selected_before,
                "changing detail tabs must not retarget the selected drive"
            );

            h.draw();
            let expected_height = h
                .app
                .drive()
                .map(|drive| h.app.content_height(drive))
                .expect("the seeded app has a selected drive");
            assert_eq!(
                h.app.content_h, expected_height,
                "the real draw pass publishes the SMART tab's complete scroll extent"
            );
        }

        // --- phase: clicking_a_partition_segment_selects_it_and_scopes_actions ---
        {
            let mut h = Harness::new(1280.0, 800.0);
            // nvme partitions: 0=EFI 1=root 2=home 3=swap. Click the root segment.
            let seg = h
                .lay()
                .part_segments
                .iter()
                .find(|(_, i)| *i == 1)
                .map(|(r, _)| *r)
                .unwrap();
            assert!(h.click(center(seg)), "a map segment is clickable");
            assert_eq!(
                h.app.selected_part,
                Some(1),
                "the segment becomes the partition selection"
            );
            assert_eq!(
                DisksDoc::selected_part(h.doc()),
                Some(1),
                "persisted to the CRDT"
            );
            // Actions now target that mounted partition.
            let drive = h.app.drive().unwrap();
            assert!(
                h.app.action_enabled(drive, ActionKind::Unmount),
                "mounted → Unmount applies"
            );
            assert!(
                !h.app.action_enabled(drive, ActionKind::Mount),
                "already mounted → Mount inert"
            );
        }

        // --- phase: clicking_a_partition_row_in_the_table_selects_it ---
        {
            let mut h = Harness::new(1280.0, 800.0);
            let (app, doc) = h.app_and_doc();
            app.set_tab(doc, Tab::Partitions);
            h.relayout();
            let row = h
                .lay()
                .part_rows
                .iter()
                .find(|(_, i)| *i == 2)
                .map(|(r, _)| *r)
                .unwrap();
            assert!(h.click(center(row)));
            assert_eq!(
                h.app.selected_part,
                Some(2),
                "table row selects the partition"
            );
        }

        // --- phase: whole_disk_pill_clears_the_partition_selection ---
        {
            let mut h = Harness::new(1280.0, 800.0);
            let seg = h
                .lay()
                .part_segments
                .iter()
                .find(|(_, i)| *i == 1)
                .map(|(r, _)| *r)
                .unwrap();
            h.click(center(seg));
            assert_eq!(h.app.selected_part, Some(1));
            let pill = h.lay().whole_disk_pill;
            assert!(h.click(center(pill)), "the Whole disk pill is clickable");
            assert_eq!(
                h.app.selected_part, None,
                "clicking it returns to whole-disk scope"
            );
        }

        // --- phase: multipartition_whole_disk_gates_mount_unmount ---
        {
            let (mut app, mut doc) = seeded_app(); // nvme (4 partitions) selected
            let d = app.drive().unwrap();
            assert!(
                !app.action_enabled(d, ActionKind::Mount),
                "no partition chosen → Mount inert"
            );
            assert!(
                !app.action_enabled(d, ActionKind::Unmount),
                "no partition chosen → Unmount inert"
            );
            // Choosing the EFI partition (mounted) enables Unmount.
            app.select_part(&mut doc, Some(0));
            let d = app.drive().unwrap();
            assert!(app.action_enabled(d, ActionKind::Unmount));
            assert!(!app.action_enabled(d, ActionKind::Mount));
        }

        // --- phase: single_filesystem_disk_targets_its_sole_partition ---
        {
            let (mut app, mut doc) = seeded_app();
            app.select(&mut doc, 3); // USB stick: one mounted partition, removable
            let d = app.drive().unwrap();
            assert!(
                app.action_enabled(d, ActionKind::Unmount),
                "sole fs is the implicit target"
            );
            assert!(
                app.action_enabled(d, ActionKind::Eject),
                "removable → Eject applies"
            );
            // A fixed, non-removable disk cannot be ejected.
            app.select(&mut doc, 1); // SATA SSD
            assert!(!app.action_enabled(app.drive().unwrap(), ActionKind::Eject));
        }
    }

    /// The format confirmation modal targets the partition, is honest about live vs demo, and gates confirmation on the typed name through shared input.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn format_confirmation_modal_gates_the_destructive_action() {
        // --- phase: clicking_format_prompts_confirmation_targeting_the_partition ---
        {
            let mut h = Harness::new(1280.0, 800.0);
            // Select the home partition, then click the Format button.
            let seg = h
                .lay()
                .part_segments
                .iter()
                .find(|(_, i)| *i == 2)
                .map(|(r, _)| *r)
                .unwrap();
            h.click(center(seg));
            let fmt = h
                .lay()
                .action_buttons
                .iter()
                .find(|(_, a, _)| *a == ActionKind::Format)
                .map(|(r, _, _)| *r)
                .unwrap();
            assert!(h.click(center(fmt)), "Format is clickable");
            let pending = h.app.pending.expect("format opens a confirm modal");
            assert_eq!(pending.action, ActionKind::Format);
            assert_eq!(
                pending.part,
                Some(2),
                "the confirmation targets the selected partition"
            );
            assert_eq!(DisksDoc::pending(h.doc()), "format:0:2");
            // The modal is now part of the layout and blocks the app.
            assert!(h.lay().modal.is_some());
        }

        // --- phase: format_confirm_body_is_honest_about_live_vs_demo ---
        {
            // The reassurance that nothing is written must appear ONLY in the
            // hermetic demo/--shots build. In the live binary Format really
            // erases the device, so the confirm must never claim it is inert.
            let demo = ActionKind::Format.confirm_body("/dev/sdz1", false);
            assert!(
                demo.contains("no changes are written to disk"),
                "the demo build reassures the reader it is inert: {demo}"
            );
            let live = ActionKind::Format.confirm_body("/dev/sdz1", true);
            assert!(
                !live.contains("no changes are written to disk"),
                "the live build must not claim the format is inert: {live}"
            );
            assert!(
                live.contains("erases ALL data") && live.contains("cannot be undone"),
                "the live build still warns the format is destructive: {live}"
            );
        }

        // --- phase: disabled_action_button_does_not_fire ---
        {
            let mut h = Harness::new(1280.0, 800.0);
            // Whole nvme selected → Mount is disabled; clicking it is a no-op.
            let lay = h.lay();
            let mount = *lay
                .action_buttons
                .iter()
                .find(|(_, a, _)| *a == ActionKind::Mount)
                .unwrap();
            assert!(
                !mount.2,
                "Mount is disabled for the multi-partition whole disk"
            );
            drop(lay);
            h.click(center(mount.0));
            assert!(
                DisksDoc::status(h.doc()).is_empty(),
                "a disabled button reports nothing"
            );
            assert!(h.app.pending.is_none());
        }

        // --- phase: modal_confirm_face_and_hit_target_share_the_type_gate ---
        {
            let mut h = Harness::new(1280.0, 800.0);
            let (app, doc) = h.app_and_doc();
            app.trigger_action(doc, ActionKind::Format);
            h.relayout();
            let confirm = h.lay().modal.unwrap().confirm;

            assert!(h.click(center(confirm)), "the modal consumes the click");
            assert!(
                h.app.pending.is_some(),
                "the disabled shared face remains inert until the name matches"
            );

            let target = h.app.pending_target_leaf(h.app.pending.unwrap());
            h.app.confirm_input.set_text(&target);
            assert!(h.app.confirm_ready());
            assert!(h.click(center(confirm)));
            assert!(
                h.app.pending.is_none(),
                "the same rect activates once the shared face is enabled"
            );
        }

        // --- phase: format_modal_routes_pointer_selection_and_ime_through_shared_input ---
        {
            let mut h = Harness::new(1280.0, 800.0);
            let (app, doc) = h.app_and_doc();
            app.trigger_action(doc, ActionKind::Format);
            h.relayout();
            let modal = h.lay().modal.unwrap();

            // Moving then clicking Cancel must use the current modal pointer, not
            // the position cached before the overlay opened.
            assert!(h.click(center(modal.cancel)));
            assert!(h.app.pending.is_none());

            let (app, doc) = h.app_and_doc();
            app.trigger_action(doc, ActionKind::Format);
            h.relayout();
            let modal = h.lay().modal.unwrap();
            let expected = h.app.pending_target_leaf(h.app.pending.unwrap());

            assert!(h.dispatch(&WindowEvent::Ime(ui::winit::event::Ime::Preedit(
                expected.clone(),
                Some((0, expected.len())),
            ))));
            assert_eq!(h.app.confirm_input.value(), "", "preedit is not committed");
            assert!(h.dispatch(&WindowEvent::Ime(ui::winit::event::Ime::Commit(
                expected.clone(),
            ))));
            assert!(h.app.confirm_ready());

            let control = Modifiers::from(ui::winit::keyboard::ModifiersState::CONTROL);
            assert!(h.dispatch_with_modifiers(&char_key("a"), control));
            assert_eq!(
                h.app.confirm_input.selection_range_public(),
                Some((0, expected.len()))
            );
            assert!(h.dispatch(&WindowEvent::Ime(ui::winit::event::Ime::Commit(expected,))));
            assert!(h.click(center(modal.confirm)));
            assert!(
                h.app.pending.is_none(),
                "Erase runs only after an exact match"
            );
        }
    }

    /// Seed a harness with whatever the *real* probe returns (falling back to
    /// the sample where there is no hardware/lsblk) and drive the production
    /// event path over it — the "drive it, don't fake it" verification.
    fn real_or_sample_harness() -> (Harness, bool) {
        let mut h = Harness::new(1280.0, 800.0);
        let live = if let Some(topo) = crate::probe::probe() {
            DisksDoc::write_topology(h.app_and_doc().1, &topo);
            let (app, doc) = h.app_and_doc();
            app.sync(doc, &ui::automerge::ROOT);
            h.relayout();
            true
        } else {
            false
        };
        (h, live)
    }

    /// A real probed topology drives the app end to end; an empty probe shows an honest empty state rather than a sample.
    #[test]
    fn probed_topology_drives_the_app_or_an_honest_empty_state() {
        // --- phase: drives_real_probed_topology_end_to_end ---
        {
            let (mut h, live) = real_or_sample_harness();
            let names: Vec<String> = h.app.topo.drives.iter().map(|d| d.name.clone()).collect();
            assert!(
                !names.is_empty(),
                "there is always at least one device to drive"
            );

            // One card per device; clicking each repoints the detail at that exact
            // device with no fabricated fields.
            for (i, name) in names.iter().enumerate() {
                let card = h.lay().cards[i];
                assert!(h.click(center(card)), "drive row {i} is clickable");
                assert_eq!(h.app.selected, i);
                let d = h.app.drive().unwrap();
                assert_eq!(&d.name, name, "detail points at the clicked device");
                assert!(!d.name.is_empty(), "no nameless devices");
                assert!(d.size > 0, "no zero-size phantom devices");
            }

            // Inspect each tab on the first device — real render passes must not panic.
            let (app, doc) = h.app_and_doc();
            app.select(doc, 0);
            for tab in [Tab::Overview, Tab::Smart, Tab::Partitions] {
                let (app, doc) = h.app_and_doc();
                app.set_tab(doc, tab);
                h.relayout(); // includes a full draw pass
                assert_eq!(h.app.tab, tab);
            }

            // Mount/Unmount enablement must mirror the real mount state, and Eject
            // only where the device is genuinely removable.
            for i in 0..h.app.topo.drives.len() {
                let (app, doc) = h.app_and_doc();
                app.select(doc, i);
                let d = h.app.drive().unwrap().clone();
                if d.partitions.len() == 1 {
                    let mounted = d.partitions[0].mountpoint.is_some();
                    let dr = h.app.drive().unwrap();
                    assert_eq!(h.app.action_enabled(dr, ActionKind::Unmount), mounted);
                    assert_eq!(h.app.action_enabled(dr, ActionKind::Mount), !mounted);
                }
                let dr = h.app.drive().unwrap();
                assert_eq!(h.app.action_enabled(dr, ActionKind::Eject), d.removable);
            }
            let _ = live;
        }

        // --- phase: empty_probe_shows_honest_empty_state_not_a_sample ---
        {
            // Simulate the live path finding nothing: an empty topology must NOT
            // fall back to fabricated devices, and the empty-state Rescan hit
            // target must exist.
            let (mut app, mut doc) = seeded_app();
            DisksDoc::write_topology(&mut doc, &Topology::default());
            app.sync(&mut doc, &ui::automerge::ROOT);
            assert!(
                app.topo.drives.is_empty(),
                "empty topology stays empty — no sample"
            );
            assert!(app.drive().is_none());
            app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let lay = app.compute_layout();
            assert!(
                lay.empty_rescan.is_some(),
                "empty state offers a Rescan button"
            );
            assert!(
                lay.cards.is_empty(),
                "no drive cards when there are no drives"
            );
        }
    }
}
