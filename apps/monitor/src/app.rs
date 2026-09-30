//! The Monitor root widget: a two-tab activity monitor (Processes /
//! Resources). The system stats are treated as **content**, not navigation —
//! there is no sidebar. The Processes tab is a full-width process table under
//! a compact horizontal summary strip; the Resources tab is a reflowing
//! dashboard of gauges and charts. Shared view preferences (tab, sort, tree
//! mode, filter, selection) live on [`crate::resource::Monitor`] via
//! [`tilo_resource::publish`]; the live process snapshot and device-observation
//! prefs ([`crate::prefs::Prefs`]) stay host-local.
//!
//! The widget owns its own geometry: `build_layout` is a single
//! full-bleed leaf, and `draw` lays out every region by hand and renders
//! it into the Vello scene. Hit rectangles computed during `draw` are
//! cached in `Hit` and consulted by `handle_event` on the next input —
//! the same pattern the toolkit's `DataTable` uses for its column rects.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchProvider, SearchResult};
use ui::time::{Duration, Instant, UNIX_EPOCH};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use std::cell::Cell;
use std::rc::Rc;
use ui::format::bytes as format_bytes;
use ui::icons::{self, Icon};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tilo_actions::{
    ActionAddress, ActionContract, ActionSpec, ConfirmationPolicy, ControlInvocation,
    OperationKind, PermissionRequirement, Resource, RetryPolicy, RiskLevel,
};
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::popover::Popover;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::tooltip::{TooltipHost, draw_hover_tooltip};
use ui::widget::{
    Button, ButtonFace, ButtonStyle, ModalDialog, Text, TextInput, Toggle, ToggleFace, Widget,
};

use crate::layout;
use crate::model::{MonitorModel, ProcessDataStatus, ProcessInfo, fmt_rate, fmt_uptime};
use crate::paint::{self, ts, with_alpha};
use crate::prefs::Prefs;
use crate::sampler::{Sampler, StopKind};
use crate::tree::{FlattenOpts, Forest, Row, SumMode};
use ui::charts::{
    BarGauge, Chart, ChartLabels, ChartStyle, GaugeReading, GaugeStyle, RingGauge, RollingSeries,
    Series, Sparkline, TrendRange,
};
use ui::widget::{PushNav, PushPane};

/// A row action chosen from the right-click context menu. The menu's action
/// callbacks are `Fn()` with no access to `&mut self`, so they record the
/// chosen action into a shared cell that `on_click` drains and applies after
/// dispatch. The app owns the input routing into the menu because the menu is
/// hand-drawn as a child of this widget rather than mounted in the host's
/// widget tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProcAction {
    Stop(u32),
    Kill(u32),
    CopyPid(u32),
    ShowDetails(u32),
}

/// Canonical wire operation for every system effect Monitor exposes through
/// its resource tree. Keeping the pid and signal shape typed avoids a palette
/// or agent inventing positional string arguments that disagree with the
/// visible Stop/Kill affordances.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum MonitorOperation {
    /// Select one exact process lifetime in the Monitor view. This is a safe
    /// view operation used by global search; it never sends an OS signal.
    Inspect {
        pid: u32,
        started_at: u64,
    },
    Refresh,
    /// Copy a process's PID through the platform clipboard bridge. Although
    /// safe, this is still an OS-facing effect and must use the same stable
    /// process generation check as signal actions.
    CopyPid {
        pid: u32,
        started_at: u64,
    },
    Signal {
        pid: u32,
        /// Kernel start-time tick captured with the resource. It binds the
        /// operation to one PID lifetime, preventing delayed actions from
        /// targeting a process that reused the same PID.
        started_at: u64,
        signal: MonitorSignal,
    },
}

/// The two process signals Monitor deliberately supports. This is distinct
/// from [`StopKind`], which is the sampler's platform-facing representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MonitorSignal {
    Stop,
    Kill,
}

impl MonitorSignal {
    const fn stop_kind(self) -> StopKind {
        match self {
            Self::Stop => StopKind::Term,
            Self::Kill => StopKind::Kill,
        }
    }
}

pub(crate) const MONITOR_OPERATION_TYPE: &str = "monitor.operation";

/// A process is only the same action resource for the lifetime identified by
/// its kernel start-time tick.  In particular, do not use a bare PID here:
/// PIDs can be recycled while a palette entry, history item, or collaborator's
/// action is waiting to execute.
fn process_resource_id(pid: u32, started_at: u64) -> String {
    format!("process:{pid}:started:{started_at}")
}

fn monitor_process_target(pid: u32, started_at: u64) -> String {
    format!("monitor.{}", process_resource_id(pid, started_at))
}

/// Decode the canonical, wire-safe process resource component used by
/// semantic targets.  Keep parsing deliberately strict so a malformed target
/// can never silently fall back to a PID-only action.
fn parse_process_resource_id(id: &str) -> Option<(u32, u64)> {
    let (pid, started_at) = id.strip_prefix("process:")?.split_once(":started:")?;
    Some((pid.parse().ok()?, started_at.parse().ok()?))
}

/// Why a signal operation is being executed. Visible UI honors the user's
/// local confirmation preference; the resource-action door already
/// enforces the operation contract before it calls the app, so it must execute
/// the approved effect instead of reporting success for merely opening a UI
/// dialog that an agent cannot operate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SignalConfirmation {
    LocalPreference,
    ActionContract,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Processes,
    Resources,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Self::Processes => "Processes",
            Self::Resources => "Resources",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Processes => Icon::Layers,
            Self::Resources => Icon::BarChart,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Processes => "processes",
            Self::Resources => "resources",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "resources" => Self::Resources,
            _ => Self::Processes,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Pid,
    Name,
    User,
    Cpu,
    Mem,
    Threads,
}

impl SortKey {
    /// Numeric columns default to descending (biggest first); text
    /// columns default to ascending.
    fn default_desc(self) -> bool {
        !matches!(self, SortKey::Name | SortKey::User)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pid => "pid",
            Self::Name => "name",
            Self::User => "user",
            Self::Cpu => "cpu",
            Self::Mem => "mem",
            Self::Threads => "threads",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "pid" => Self::Pid,
            "name" => Self::Name,
            "user" => Self::User,
            "mem" => Self::Mem,
            "threads" => Self::Threads,
            _ => Self::Cpu,
        }
    }
}

// ---- layout constants ----------------------------------------------------
const OUTER_PAD: f64 = 14.0;
const HEADER_H: f64 = 56.0;
const GAP: f64 = 14.0;
/// The painted band (and hit target) of a process-table row or column
/// header at 1×: the geometry lint's `target_size_24` floor.
const ROW_BAND_H: f64 = 24.0;
/// Process-table row *pitch* at 1×: the band plus one `s1` so neighbouring
/// rows keep a token-step gap instead of touching (HIG §7). `Density` re-derives
/// this from the *scaled* `s1` each frame (`row_gap`), so pitch and gap move
/// together under `ui_scale` and a custom `spacing_px`.
const ROW_H: f64 = ROW_BAND_H + ui::tokens::spacing::Spacing::BASE.s1 as f64;
/// Column-header pitch at 1×: the same band + `s1` as a row.
const TABLE_HEADER_H: f64 = ROW_BAND_H + ui::tokens::spacing::Spacing::BASE.s1 as f64;
const DETAIL_H: f64 = 96.0;
/// Per-depth indentation of a tree row's NAME column.
const TREE_INDENT: f64 = 16.0;
/// Width of the clickable expand/collapse triangle hit target.
const TWISTY_W: f64 = 16.0;

// ---- toolbar (search + filter pills) -------------------------------------
/// Height of one toolbar row (the search box / a row of filter pills).
const TOOLBAR_ROW_H: f64 = 36.0;
/// Vertical gap between the two toolbar rows when it stacks on narrow widths.
const TOOLBAR_ROW_GAP: f64 = 8.0;
/// Horizontal gap between adjacent filter pills.
const TOOLBAR_PILL_GAP: f64 = 10.0;
/// Smallest the search box may shrink beside the pill cluster before the
/// toolbar stacks the search box onto its own row above the pills.
const SEARCH_MIN_W: f64 = 160.0;
/// Preferred search-box width when there is room to spare (wide windows).
const SEARCH_PREF_W: f64 = 320.0;

/// How long the "PID copied" toast stays up after a Copy PID action before it
/// expires on its own. The redraw loop keeps ticking (`needs_animation_frame`
/// is always true), so the toast clears itself without any further input.
const TOAST_MS: u64 = 2200;

/// Refresh cadences offered by the interval control, smallest → largest. A
/// quick click on the pill cycles through them; a long-press opens a popover
/// listing them all. Kept in one place so the pill label, the cycle order and
/// the popover options stay in lock-step.
const INTERVAL_OPTIONS: [u64; 5] = [1000, 1500, 2000, 3000, 5000];

/// A process table can contain thousands of kernel entries. Keep the
/// contextual resource projection small; exact process resources remain
/// resolvable on demand through [`Widget::resource`].
const MAX_CONTEXT_PROCESS_RESOURCES: usize = 64;
/// Process names originate with the operating system and are rendered in
/// global/remote action surfaces. Bound and flatten them before publishing so
/// an unusual process name cannot force unbounded text shaping or line breaks.
const MAX_PROCESS_RESOURCE_LABEL_CHARS: usize = 160;

/// Press-and-hold duration on the interval pill that opens the option popover
/// instead of cycling. A release before this fires the plain click-to-cycle.
const LONG_PRESS_MS: u128 = 350;

/// Sample depth of the per-process CPU sparkline shown in the detail pane —
/// a shorter window than the whole-system charts so the mini-line stays punchy.
const PROC_SPARK_CAP: usize = 48;

/// The monitor's gauges sit dense on a dashboard: a firmer track so a thin
/// ring or bar still reads beside its fill, and compact bar labels.
const GAUGE: GaugeStyle = GaugeStyle {
    track_alpha: 56,
    compact_labels: true,
};

/// Height of the pinned action bar at the bottom of the detail pane. The
/// Kill/Stop buttons live here and nowhere else, so process content above can
/// never collide with them however narrow the pane gets.
const DETAIL_ACTION_H: f64 = 46.0;

/// Layout of the tall detail pane's two-column stat grid (MEMORY / VIRTUAL /
/// THREADS / OPEN FILES / STATUS / RUNNING FOR). Shared between the
/// sparkline pass (which needs the grid's total height to size itself) and
/// the grid pass itself, so the two never disagree.
const DETAIL_STAT_COUNT: usize = 6;
const DETAIL_STAT_COLS: usize = 2;
const DETAIL_STAT_ROW_H: f64 = 48.0;

/// Vertical pitches of every text-bearing band, derived once per frame from
/// the measured line boxes of the type sizes each band stacks. The `*_H`
/// constants above are the 1× design; each band grows by exactly the extra
/// height its lines gain at the current accessibility scale, so layout,
/// paint and hit-testing (`rows_per_page`, wheel-to-rows, the seeded context
/// menu anchor) all read one pitch. At 1× this is [`Density::ONE`].
#[derive(Clone, Copy, Debug, PartialEq)]
struct Density {
    /// Process-table row pitch (`sm` cells): [`Self::row_gap`] plus the band.
    row_h: f64,
    /// Gap between neighbouring row bands and between sort-header targets —
    /// this frame's `spacing.s1`, so it follows `ui_scale`. Half of it insets
    /// the painted band / hit rect from the pitch on each side.
    row_gap: f64,
    /// Process-table header row (`xs` labels).
    table_header_h: f64,
    /// One toolbar row (search box / pill row, `sm` labels).
    toolbar_row_h: f64,
    /// Top header card: clock (`lg`) over uptime (`xs`).
    header_h: f64,
    /// Wide-layout detail strip: name (`lg`), identity (`xs`), chip label
    /// (`xs`) and chip value (`base`) stacked.
    detail_h: f64,
    /// Tall-detail stat grid row: `xs` label over `base` value.
    stat_row_h: f64,
    /// Summary-strip cell: an `xs` label and an `sm` value (stacked when the
    /// cell is tight) over a sparkline.
    strip_cell_h: f64,
    /// Extra height one per-core gauge row needs for its `xs` label.
    core_extra: f64,
    /// Growth of one `sm` line over its 1× box — what a 1×-designed control
    /// (header segment, pill) adds to keep seating its label.
    sm_extra: f64,
    /// The renderer's accessibility text scale. Glyph-budgeted widths (the
    /// process-table columns) are laid out in 1× space and scaled by this, so
    /// a column that seats `622.0 MiB` at 1× still seats it at 200 %.
    text_k: f64,
}

impl Density {
    /// The 1× design metrics — also what the constructor seeds before the
    /// first frame has measured anything.
    const ONE: Self = Self {
        row_h: ROW_H,
        row_gap: ui::tokens::spacing::Spacing::BASE.s1 as f64,
        table_header_h: TABLE_HEADER_H,
        toolbar_row_h: TOOLBAR_ROW_H,
        header_h: HEADER_H,
        detail_h: DETAIL_H,
        stat_row_h: DETAIL_STAT_ROW_H,
        strip_cell_h: layout::STRIP_CELL_H,
        core_extra: 0.0,
        sm_extra: 0.0,
        text_k: 1.0,
    };

    /// Measure with the renderer that will paint this frame. A line's growth
    /// over its 1× box is `line − line / scale`; a band gains one growth per
    /// line it stacks.
    fn measure(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let scale = f64::from(tr.text_scale()).max(f32::EPSILON.into());
        let grow = |line: f64| (line - line / scale).max(0.0);
        let ty = &theme.tokens.typography.scale;
        let xs = grow(paint::line_h(tr, ty.xs));
        let sm = grow(paint::line_h(tr, ty.sm));
        let base = grow(paint::line_h(tr, ty.base));
        let lg = grow(paint::line_h(tr, ty.lg));
        // Pitch = band + this frame's scaled `s1`: the gap and the pitch are
        // one value, so a scaled theme cannot shrink the band below its floor.
        let row_gap = f64::from(theme.tokens.spacing.s1);
        Self {
            row_h: ROW_BAND_H + row_gap + sm,
            row_gap,
            table_header_h: ROW_BAND_H + row_gap + xs,
            toolbar_row_h: TOOLBAR_ROW_H + sm,
            header_h: HEADER_H + lg + xs,
            detail_h: DETAIL_H + lg + xs + xs + base,
            stat_row_h: DETAIL_STAT_ROW_H + xs + base,
            // The tight branch stacks the `xs` label over the `sm` value, so the
            // cell grows by both lines and the sparkline keeps its 1× height.
            strip_cell_h: layout::STRIP_CELL_H + xs + sm,
            core_extra: xs,
            sm_extra: sm,
            text_k: scale,
        }
    }
}

/// Human label for a refresh cadence in ms: `1s`, `1.5s`, `2s`. Whole seconds
/// drop the decimal. Mirrors [`crate::prefs::Prefs::interval_label`] so the
/// pill and the popover options read identically.
fn interval_label(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{:.1}s", ms as f32 / 1000.0)
    }
}

/// A stable one-line label for a process resource. The identity stays in the
/// opaque PID-generation id; this display label is deliberately bounded.
fn process_resource_label(process: &ProcessInfo) -> String {
    let mut label = String::new();
    let mut chars = process.name.chars();
    for _ in 0..MAX_PROCESS_RESOURCE_LABEL_CHARS {
        let Some(ch) = chars.next() else {
            return format!("{} ({})", label, process.pid);
        };
        label.push(if ch.is_control() { ' ' } else { ch });
    }
    if chars.next().is_some() {
        label.push('…');
    }
    format!("{} ({})", label, process.pid)
}

/// The always-addressable root of Monitor's live process catalogue.
pub(crate) fn monitor_process_root_resource() -> Resource {
    let mut root = Resource::new("processes", "monitor.processes", "Processes");
    if let Ok(refresh) = ActionSpec::operation(
        "refresh",
        "Refresh processes",
        OperationKind::Effect,
        MONITOR_OPERATION_TYPE,
        &MonitorOperation::Refresh,
    ) {
        root.actions.push(refresh.with_contract(ActionContract {
            permission: PermissionRequirement::Read,
            retry: RetryPolicy::Idempotent,
            ..ActionContract::default()
        }));
    }
    root
}

/// One exact, PID-generation-bound process resource. This is shared by the
/// bounded contextual catalogue and lazy direct resolution, keeping every
/// visible, searched, and agent-invoked action contract identical.
fn monitor_process_resource(process: &ProcessInfo) -> Resource {
    let mut resource = Resource::new(
        process_resource_id(process.pid, process.started_at),
        "monitor.process",
        process_resource_label(process),
    );
    if let Ok(inspect) = ActionSpec::operation(
        "inspect",
        "Show process details",
        OperationKind::View,
        MONITOR_OPERATION_TYPE,
        &MonitorOperation::Inspect {
            pid: process.pid,
            started_at: process.started_at,
        },
    ) {
        resource
            .actions
            .push(inspect.with_contract(ActionContract::view()));
    }
    if let Ok(copy) = ActionSpec::operation(
        "copy-pid",
        "Copy PID",
        OperationKind::Effect,
        MONITOR_OPERATION_TYPE,
        &MonitorOperation::CopyPid {
            pid: process.pid,
            started_at: process.started_at,
        },
    ) {
        resource.actions.push(copy.with_contract(ActionContract {
            permission: PermissionRequirement::Read,
            retry: RetryPolicy::Idempotent,
            ..ActionContract::default()
        }));
    }
    for (verb, label, signal) in [
        ("stop", "Stop process", MonitorSignal::Stop),
        ("kill", "Force kill", MonitorSignal::Kill),
    ] {
        if let Ok(spec) = ActionSpec::operation(
            verb,
            label,
            OperationKind::Effect,
            MONITOR_OPERATION_TYPE,
            &MonitorOperation::Signal {
                pid: process.pid,
                started_at: process.started_at,
                signal,
            },
        ) {
            resource.actions.push(spec.with_contract(ActionContract {
                permission: PermissionRequirement::Admin,
                risk: if signal == MonitorSignal::Kill {
                    RiskLevel::Destructive
                } else {
                    RiskLevel::Privileged
                },
                // Signalling another process is consequential either way; the
                // protocol (and the broker) refuse a privileged action that
                // skips confirmation.
                confirmation: ConfirmationPolicy::Confirm,
                retry: RetryPolicy::AtMostOnce,
                ..ActionContract::default()
            }));
        }
    }
    resource
}

/// Retained, bounded-search index over the current live process snapshot.
/// It is refreshed only when the sampler replaces that snapshot, never while
/// the user types, and search hits resolve back to the exact PID generation.
#[derive(Clone)]
struct MonitorProcessSearchProvider {
    catalog: SearchCatalog,
    snapshot: Arc<RwLock<HashMap<String, ProcessInfo>>>,
}

impl MonitorProcessSearchProvider {
    fn new(processes: &[ProcessInfo]) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new("Processes"),
            snapshot: Arc::new(RwLock::new(HashMap::new())),
        };
        provider.refresh(processes);
        provider
    }

    fn indexed_text(value: &str) -> String {
        value
            .chars()
            .take(MAX_PROCESS_RESOURCE_LABEL_CHARS)
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect()
    }

    fn refresh(&self, processes: &[ProcessInfo]) {
        let mut current = HashMap::with_capacity(processes.len());
        for process in processes {
            let id = process_resource_id(process.pid, process.started_at);
            let _ = self.catalog.upsert(
                id.clone(),
                format!(
                    "{}\u{1f}{}\u{1f}{}",
                    Self::indexed_text(&process.name),
                    Self::indexed_text(&process.user),
                    process.pid
                ),
                SearchResult::for_action(
                    "Processes",
                    process_resource_label(process),
                    ActionAddress::new(id.clone(), "inspect"),
                )
                .with_subtitle(format!(
                    "PID {} · {}",
                    process.pid,
                    Self::indexed_text(&process.user)
                )),
            );
            current.insert(id, process.clone());
        }
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for id in snapshot.keys().filter(|id| !current.contains_key(*id)) {
            self.catalog.remove(id);
        }
        *snapshot = current;
    }
}

impl CatalogSearchProvider for MonitorProcessSearchProvider {
    fn provider_name(&self) -> &'static str {
        "Processes"
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<Resource> {
        (address.action == "inspect")
            .then(|| {
                self.snapshot
                    .read()
                    .ok()?
                    .get(&address.resource)
                    .map(monitor_process_resource)
            })
            .flatten()
    }
}

/// A scrollbar's on-screen geometry, recorded during `draw` so `handle_event`
/// can hit-test its thumb/track and map a thumb drag to a scroll value.
/// `max_value` is the scroll range's ceiling in that bar's own units — rows
/// for the vertical row bar, logical px for the column and page bars.
#[derive(Clone, Copy, Debug)]
struct ScrollBarHit {
    track: Rect,
    thumb: Rect,
    max_value: f64,
}

/// Process table layout geometry, computed once per frame so the header,
/// rows and scrollbar passes each take one struct instead of a long, mostly
/// duplicated parameter list.
#[derive(Clone, Debug)]
struct TableGeom {
    /// The visible columns, in content space and scaled by the text scale.
    columns: Vec<layout::Column>,
    /// The card's full inner rect (post-panel padding).
    ti: Rect,
    /// The padded column band — header text and row cells clip to this so an
    /// overflowing column is hidden past the edge, not overlapping the border.
    band: Rect,
    hdr: Rect,
    /// The rows viewport, above any horizontal scrollbar.
    view: Rect,
    /// Left edge of content-space column 0, before the horizontal scroll.
    cols_left: f64,
    /// Horizontal scroll offset, in logical px.
    sx: f64,
    /// Height reserved for the horizontal scrollbar (0 when columns fit).
    hbar_h: f64,
    /// Total intrinsic width of every column.
    content_w: f64,
    /// The columns viewport width (the padded band's width).
    avail_w: f64,
    /// How far the columns overflow `avail_w` (0 when they fit).
    h_overflow: f64,
}

/// Static face spec for one detail-pane action button (Stop/Kill), bundled
/// so `draw_action_button` takes one struct instead of three label/icon/style
/// arguments.
#[derive(Clone, Copy)]
struct ActionButtonSpec<'a> {
    label: &'a str,
    icon: Icon,
    style: ButtonStyle,
}

/// Which scrollbar the user is dragging, plus where within its thumb they
/// grabbed (logical px along the bar's axis) so the thumb tracks the cursor
/// without jumping to it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ScrollDrag {
    /// The process table's vertical row scrollbar (value = fractional row).
    Rows { grab: f64 },
    /// The process table's horizontal column scrollbar (value = px).
    Cols { grab: f64 },
    /// The whole-view page scrollbar (value = px).
    Page { grab: f64 },
}

/// The live stop-confirmation dialog: the shared [`ModalDialog`] plus the
/// `confirm` / `cancel` cells its action callbacks flip (drained each event by
/// the host). The message body is a plain [`Text`]; the dialog owns the scrim,
/// box, footer buttons, Esc, focus trap, and a11y role.
struct ConfirmDialog {
    dialog: ModalDialog<Text>,
    confirm: Rc<Cell<bool>>,
    cancel: Rc<Cell<bool>>,
}

/// Hit rectangles recorded during `draw`, consumed by `handle_event`.
struct Hit {
    tab_proc: Rect,
    tab_res: Rect,
    /// Manual one-shot sample; exported through the root resource action.
    refresh: Rect,
    /// Persisted periodic-sampling pause/resume control.
    auto_refresh: Rect,
    interval: Rect,
    search: Rect,
    kernel_toggle: Rect,
    sort_headers: Vec<(SortKey, Rect)>,
    rows: Vec<(u32, Rect)>,
    /// Expand/collapse triangles in the tree's NAME column, per parent pid.
    twisties: Vec<(u32, Rect)>,
    /// The "Sum subtree / Per-process" toggle pill in the toolbar.
    sum_toggle: Rect,
    /// The "Tree / Flat" view toggle pill in the toolbar.
    tree_toggle: Rect,
    kill_btn: Rect,
    /// The "Stop" (SIGTERM) button in the detail pane's action bar.
    stop_btn: Rect,
    /// Recovery action for a process-source permission/error state.
    retry_process_source: Rect,
    /// Context-menu item rects, recorded while the menu is open.
    menu_items: Vec<(usize, Rect)>,
    /// Summary-strip stat cells (Processes tab). Clicking one jumps to the
    /// Resources dashboard, where that stat's full gauge/chart lives.
    summary_cells: Vec<Rect>,
    /// Back affordance returning from the pushed detail to the table (narrow).
    detail_back: Rect,
    /// The "Confirm kills" preference toggle pill in the toolbar — the only UI
    /// that reaches the local `confirm_kill` preference.
    confirm_toggle: Rect,
    /// The process table's vertical row scrollbar (present when rows overflow).
    v_scrollbar: Option<ScrollBarHit>,
    /// The process table's horizontal column scrollbar (present on overflow).
    h_scrollbar: Option<ScrollBarHit>,
    /// The whole-view page scrollbar (present when the Resources body overflows).
    page_scrollbar: Option<ScrollBarHit>,
}

impl Hit {
    fn new() -> Self {
        Self {
            tab_proc: Rect::ZERO,
            tab_res: Rect::ZERO,
            refresh: Rect::ZERO,
            auto_refresh: Rect::ZERO,
            interval: Rect::ZERO,
            search: Rect::ZERO,
            kernel_toggle: Rect::ZERO,
            sort_headers: Vec::new(),
            rows: Vec::new(),
            twisties: Vec::new(),
            sum_toggle: Rect::ZERO,
            tree_toggle: Rect::ZERO,
            kill_btn: Rect::ZERO,
            stop_btn: Rect::ZERO,
            retry_process_source: Rect::ZERO,
            menu_items: Vec::new(),
            summary_cells: Vec::new(),
            detail_back: Rect::ZERO,
            confirm_toggle: Rect::ZERO,
            v_scrollbar: None,
            h_scrollbar: None,
            page_scrollbar: None,
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (sort direction, tree/flat view, one-shot popover seed, shots-only sampling gate); pairing them into enums would be artificial"
)]
pub struct MonitorApp {
    model: MonitorModel,
    sampler: Sampler,
    /// Retained global-search/directory index. Its entries carry the same
    /// PID-generation ids as every resource action and update only per sample.
    process_search: MonitorProcessSearchProvider,
    prefs: Prefs,

    // view state
    tab: Tab,
    sort_key: SortKey,
    sort_desc: bool,
    /// Shared local editor for the process filter. Monitor owns filtering and
    /// toolbar composition; the toolkit owns caret, selection, clipboard, IME
    /// and accessibility behavior.
    search_input: TextInput,
    selected_pid: Option<u32>,
    scroll_off: f32,
    /// Horizontal scroll of the process table, in logical px, when its
    /// columns are wider than the viewport (clamped to the overflow).
    table_scroll_x: f32,
    /// Vertical scroll of the *whole* view, in logical px, when the laid-out
    /// Resources dashboard (overview row + per-core grid + chart grid) is
    /// taller than the body viewport. Clamped each frame to the overflow.
    view_scroll_y: f32,
    /// Last frame's content/viewport heights, recorded during `draw` so
    /// wheel + clamp can run in `handle_event` without re-deriving layout.
    content_h: f64,
    viewport_h: f64,
    /// A pending confirmation: the process and which stop signal it will get.
    /// Set only for the `Kill` (SIGKILL) path; `Stop` (SIGTERM) never gates.
    confirm_stop: Option<(u32, u64, StopKind)>,
    /// The live confirmation dialog (shared [`ModalDialog`]) mirroring
    /// `confirm_stop`. Built lazily when a confirm is pending and dropped when
    /// it resolves; owns the scrim, centered box, footer buttons, Esc, focus
    /// trap, and a11y `Dialog` role.
    confirm_dialog: Option<ConfirmDialog>,

    // process tree
    /// Tree view (parent→child with twisties) vs a flat sorted list.
    tree_mode: bool,
    /// Whether a parent row shows its summed subtree usage or only its own.
    /// Defaults to [`SumMode::SumSubtree`] per the spec.
    sum_mode: SumMode,
    /// pids the user has expanded in the tree (collapsed otherwise).
    expanded: HashSet<u32>,
    /// Right-click action menu over a process row (the self-routing
    /// `ui::widget::ContextMenu`; this app supplies the input routing).
    context: ContextMenu,
    /// The pid the open context menu targets.
    context_pid: Option<u32>,
    /// Shared sink the menu's `Fn()` callbacks write the chosen action into;
    /// drained + applied by `on_click` after dispatch (see [`ProcAction`]).
    /// `Arc<Mutex<..>>` because `ContextMenu` requires `Send + Sync` actions.
    pending_action: Arc<Mutex<Option<ProcAction>>>,
    /// Visible process controls emit inert resource addresses; the host
    /// resolves the latest action contract before any OS signal is sent.
    pending_control_invocation: Option<ControlInvocation>,
    /// The pid most recently "Copy PID"'d. The pid itself is written to the OS
    /// clipboard via [`ui::clipboard_copy`]; this field records it so the
    /// transient "PID N copied" toast (and tests) can observe the action.
    copied_pid: Option<u32>,
    /// When the last Copy PID happened — drives the auto-expiring toast.
    copied_at: Option<Instant>,
    /// An in-progress scrollbar thumb drag, if any (started on mouse-press over
    /// a thumb, updated on cursor-move, cleared on release).
    scroll_drag: Option<ScrollDrag>,
    /// A `--shots` request to open the context menu over a pid once its row
    /// has been laid out (the anchor isn't known until `draw` computes the row
    /// rects). Consumed on the first frame that finds the row.
    context_menu_seed: Option<u32>,

    // interval control
    /// The refresh-interval option menu opened by long-pressing the pill (the
    /// reusable `ui::widget::Popover`; this app supplies the input routing, the
    /// same way it drives its `ContextMenu`).
    interval_popover: Popover,
    /// When the left button went down on the interval pill (Some while held).
    /// A quick release cycles; a hold past [`LONG_PRESS_MS`] opens the popover.
    interval_press: Option<Instant>,
    /// Whether the current hold already opened the popover (so the release
    /// doesn't also cycle).
    interval_hold_opened: bool,
    /// A `--shots` request to open the interval popover on the next frame, once
    /// the pill's hit rect is known from `draw_header`.
    interval_popover_seed: bool,
    /// Rolling per-process CPU history, keyed by the kernel process generation,
    /// feeding the detail-pane sparkline. A PID alone is not an identity: using
    /// the same `(pid, started_at)` tuple as resource actions prevents a new
    /// process inheriting a recycled PID's history after a refresh.
    proc_cpu_hist: HashMap<(u32, u64), RollingSeries>,

    // responsive layout
    /// The Processes master-detail push flow used only when narrow.
    proc_push: PushNav,
    /// One shared delayed tooltip controller for the icon-only narrow header
    /// controls. Wide layouts keep their text labels visible instead.
    tooltips: TooltipHost,

    // frame caches
    rect: Rect,
    last_cursor: Point,
    last_refresh: Instant,
    /// Never sample the live machine — the seeded snapshot is the whole model.
    /// `last_refresh` alone cannot promise this: it only delays the first
    /// sample, so any frame that takes longer than `interval_ms` (a loaded
    /// machine, a debug build) swaps the deterministic rows for the real
    /// process table underneath whatever is reading them.
    sample_only: bool,
    rows_per_page: usize,
    /// Text-derived band pitches for this frame (see [`Density`]).
    density: Density,
    /// The process table's clipped row viewport — wheel events over it scroll
    /// the rows/columns; outside it (and on the Resources tab) the wheel
    /// scrolls the whole page. Recorded during `draw_process_table`.
    table_viewport: Rect,
    /// Intrinsic content width of the process table's columns. When it
    /// exceeds the table viewport width a horizontal scrollbar appears and
    /// `table_scroll_x` can range over the overflow.
    table_content_w: f64,
    /// The *columns* viewport width — the padded inner band the columns are
    /// drawn into (`table_viewport.width() − 2·TABLE_PAD`). The horizontal
    /// scroll clamps against `table_content_w − table_cols_w` so the final
    /// column (plus its right padding) scrolls fully into view; using the
    /// un-padded `table_viewport.width()` here left the last `2·TABLE_PAD`
    /// unreachable (the h-scroll bug).
    table_cols_w: f64,
    /// Text scale sampled from the current theme during draw. The Resources
    /// page uses it for immediate-mode grid geometry as well as text, so
    /// 200% text reflows into additional rows instead of overlapping gauges.
    layout_text_scale: f64,
    hit: Hit,
    /// Document actions: local host for standalone / `--shots` / unit tests;
    /// framework dispatcher when [`crate::resource::Monitor`] hosts the surface.
    actions: crate::resource::ActionSink,
}

impl Default for MonitorApp {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitorApp {
    /// Construct with the deterministic, fully-populated sample snapshot. This
    /// is what `--shots` and the tests render: a single offscreen frame never
    /// ticks the sampler, so the seeded sample is what appears — hermetic and
    /// reproducible offline. The live window uses [`Self::new_live`] instead.
    pub fn new() -> Self {
        let model = MonitorModel::sample();
        // "now" ⇒ the first (screenshot) frame skips the live refresh and
        // renders the seeded sample untouched.
        let mut app = Self::build(model, None, ui::time::now(), Sampler::inert());
        // The default tree starts collapsed. Select a row from that *visible*
        // projection rather than the globally busiest process, which may be a
        // hidden descendant. A detail pane with destructive controls must
        // never refer to a process the user cannot see in the table.
        app.selected_pid = app.busiest_visible_pid();
        app.sample_only = true;
        // Seed a synthetic CPU history for the pre-selected process so the
        // detail pane's sparkline reads as "alive" in the hermetic shot frame
        // (which never ticks the live sampler that would fill it for real).
        app.seed_selected_spark();
        app
    }

    /// Construct for the live window: start from an empty model and force the
    /// first frame to sample real `sysinfo` data immediately, so the window
    /// shows the user's actual processes and gauges from the first paint rather
    /// than flashing fabricated sample rows. Sampling failures degrade to empty
    /// tables (honest) rather than fake content.
    pub fn new_live() -> Self {
        // A `last_refresh` well in the past makes the first `maybe_refresh`
        // fire on the first draw, replacing the empty seed with real data.
        let stale = ui::time::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        Self::build(MonitorModel::empty(), None, stale, Sampler::new())
    }

    fn build(
        model: MonitorModel,
        selected_pid: Option<u32>,
        last_refresh: Instant,
        sampler: Sampler,
    ) -> Self {
        let process_search = MonitorProcessSearchProvider::new(&model.processes);
        Self {
            model,
            sampler,
            process_search,
            prefs: Prefs::default(),
            tab: Tab::Processes,
            sort_key: SortKey::Cpu,
            sort_desc: true,
            search_input: TextInput::local()
                .with_kind(ui::widget::TextInputKind::Search)
                .with_id("monitor.search")
                .with_placeholder("Search processes…")
                .with_plain_chrome(),
            selected_pid,
            scroll_off: 0.0,
            table_scroll_x: 0.0,
            view_scroll_y: 0.0,
            content_h: 0.0,
            viewport_h: 0.0,
            confirm_stop: None,
            confirm_dialog: None,
            tree_mode: true,
            sum_mode: SumMode::SumSubtree,
            expanded: HashSet::new(),
            context: ContextMenu::new(),
            context_pid: None,
            pending_action: Arc::new(Mutex::new(None)),
            pending_control_invocation: None,
            copied_pid: None,
            copied_at: None,
            scroll_drag: None,
            context_menu_seed: None,
            interval_popover: Popover::new().with_label("Refresh interval"),
            interval_press: None,
            interval_hold_opened: false,
            interval_popover_seed: false,
            proc_cpu_hist: HashMap::new(),
            proc_push: PushNav::default(),
            tooltips: TooltipHost::new(),
            rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            last_refresh,
            sample_only: false,
            rows_per_page: 1,
            density: Density::ONE,
            table_viewport: Rect::ZERO,
            table_content_w: 0.0,
            table_cols_w: 0.0,
            layout_text_scale: 1.0,
            hit: Hit::new(),
            actions: crate::resource::ActionSink::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn history_test_with_pending_kill(
        signal_probe: crate::sampler::SignalProbe,
    ) -> (Self, u32, u64) {
        let mut app = Self::build(
            MonitorModel::sample(),
            None,
            ui::time::now(),
            Sampler::controlled(signal_probe),
        );
        app.prefs.auto_refresh = false;
        let process = app.model.processes.first().expect("sample process").clone();
        app.selected_pid = Some(process.pid);
        app.perform_operation(
            &MonitorOperation::Signal {
                pid: process.pid,
                started_at: process.started_at,
                signal: MonitorSignal::Kill,
            },
            SignalConfirmation::LocalPreference,
        )
        .expect("sample kill confirmation");
        (app, process.pid, process.started_at)
    }

    /// Refresh from sysinfo at most once per preference interval. The very
    /// first draw never trips this (elapsed ≈ 0), so `--shots` shows the
    /// sample snapshot; the live loop updates a beat later.
    fn maybe_refresh(&mut self) {
        if self.sample_only {
            return;
        }
        if self.prefs.auto_refresh
            && self.last_refresh.elapsed() >= Duration::from_millis(self.prefs.interval_ms)
        {
            self.refresh_live_snapshot();
        }
    }

    /// Refresh system data while preserving the identity boundary of the
    /// selected detail. If an OS has recycled a selected PID between samples,
    /// leave no action surface silently pointed at the replacement process.
    fn refresh_live_snapshot(&mut self) {
        let selected_before = self.selected_identity();
        self.sampler.refresh(&mut self.model);
        self.process_search.refresh(&self.model.processes);
        self.last_refresh = ui::time::now();
        self.clear_recycled_selection(selected_before);
        self.record_proc_cpu();
        self.clear_selection_if_filtered_out();
        self.clamp_scroll();
    }

    /// Clear a selection when the process snapshot no longer contains the
    /// exact lifetime that was selected before it was refreshed.
    fn clear_recycled_selection(&mut self, selected_before: Option<(u32, u64)>) {
        if selected_before.is_some_and(|(pid, started_at)| {
            !self
                .model
                .processes
                .iter()
                .any(|p| p.pid == pid && p.started_at == started_at)
        }) {
            self.selected_pid = None;
            if self.is_narrow() {
                self.proc_push.show_master();
            }
        }
    }

    /// Fold this refresh's per-process CPU into the sparkline histories, then
    /// prune rings for processes that have since exited so the map stays bound
    /// to the live process set.
    fn record_proc_cpu(&mut self) {
        for p in &self.model.processes {
            self.proc_cpu_hist
                .entry((p.pid, p.started_at))
                .or_insert_with(|| RollingSeries::new(PROC_SPARK_CAP))
                .push(p.cpu);
        }
        let live: HashSet<(u32, u64)> = self
            .model
            .processes
            .iter()
            .map(|p| (p.pid, p.started_at))
            .collect();
        self.proc_cpu_hist
            .retain(|identity, _| live.contains(identity));
    }

    /// Seed the currently-selected process's CPU ring with a synthetic wave
    /// around its present load. Used only by the hermetic `--shots`/sample
    /// constructor so the detail sparkline has data before any live refresh.
    fn seed_selected_spark(&mut self) {
        let Some(pid) = self.selected_pid else { return };
        let Some(p) = self.model.processes.iter().find(|p| p.pid == pid) else {
            return;
        };
        let base = p.cpu;
        let ring = RollingSeries::from_samples(
            PROC_SPARK_CAP,
            (0..PROC_SPARK_CAP).map(|i| {
                let t = i as f32;
                (base + 9.0 * (t * 0.31).sin() + 4.0 * (t * 0.73).cos())
                    .clamp(0.0, base.max(4.0) * 1.6)
            }),
        );
        self.proc_cpu_hist.insert((pid, p.started_at), ring);
    }

    /// The filtered + sorted process list currently displayed.
    fn visible_processes(&self) -> Vec<ProcessInfo> {
        let f = self.search_input.value().to_lowercase();
        let mut v: Vec<ProcessInfo> = self
            .model
            .processes
            .iter()
            .filter(|p| self.prefs.show_kernel_threads || !p.kernel)
            .filter(|p| {
                f.is_empty()
                    || p.name.to_lowercase().contains(&f)
                    || p.user.to_lowercase().contains(&f)
                    || p.pid.to_string().contains(&f)
            })
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            let o = match self.sort_key {
                SortKey::Pid => a.pid.cmp(&b.pid),
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::User => a.user.to_lowercase().cmp(&b.user.to_lowercase()),
                SortKey::Cpu => a.cpu.total_cmp(&b.cpu),
                SortKey::Mem => a.mem_bytes.cmp(&b.mem_bytes),
                SortKey::Threads => a.threads.cmp(&b.threads),
            };
            if self.sort_desc { o.reverse() } else { o }
        });
        v
    }

    fn selected_process(&self) -> Option<ProcessInfo> {
        let pid = self.selected_pid?;
        self.model.processes.iter().find(|p| p.pid == pid).cloned()
    }

    /// Identity of the current selection at this instant. This exists beside
    /// the PID-only presentation selection so refresh can prove a process is
    /// still the same kernel lifetime before retaining its detail pane.
    fn selected_identity(&self) -> Option<(u32, u64)> {
        self.selected_process().map(|p| (p.pid, p.started_at))
    }

    /// A text filter is a view boundary: do not leave an actionable detail
    /// card for a process that is no longer represented in the table. Besides
    /// avoiding confusing "no results" screens with stale detail below them,
    /// this ensures Stop/Kill controls always correspond to a visible row.
    fn clear_selection_if_filtered_out(&mut self) {
        let Some(pid) = self.selected_pid else {
            return;
        };
        if !self.visible_rows().iter().any(|row| row.pid == pid) {
            self.selected_pid = None;
            if self.is_narrow() {
                self.proc_push.show_master();
            }
        }
    }

    /// Build the rows the process table renders this frame. In tree mode this
    /// is the flattened parent→child forest (honoring collapse state, the
    /// sum/per-process mode, sort, kernel pref and filter); in flat mode it's
    /// the sorted list rendered at depth 0 with no twisties. Both paths share
    /// the same [`Row`] shape so `draw_process_table` is mode-agnostic.
    fn visible_rows(&self) -> Vec<Row> {
        if self.tree_mode {
            let forest = Forest::build(&self.model.processes);
            let filter = self.search_input.value().to_lowercase();
            forest.flatten(&FlattenOpts {
                sum_mode: self.sum_mode,
                sort_key: self.sort_key,
                sort_desc: self.sort_desc,
                filter: &filter,
                expanded: &self.expanded,
                show_kernel: self.prefs.show_kernel_threads,
            })
        } else {
            // Flat: the same sorted/filtered list the old table showed, but
            // expressed as depth-0 rows. Per-process always in flat mode (no
            // parent to sum into).
            self.visible_processes()
                .into_iter()
                .map(|p| Row {
                    pid: p.pid,
                    name: p.name,
                    user: p.user,
                    cpu: p.cpu,
                    mem_bytes: p.mem_bytes,
                    threads: p.threads,
                    status: p.status,
                    depth: 0,
                    has_children: false,
                    expanded: false,
                    descendants: 0,
                })
                .collect()
        }
    }

    fn clamp_scroll(&mut self) {
        let n = self.visible_rows().len();
        let max = n.saturating_sub(self.rows_per_page) as f32;
        self.scroll_off = self.scroll_off.clamp(0.0, max);
    }

    /// Toggle a parent pid's expand/collapse state.
    fn toggle_expand(&mut self, pid: u32) {
        if !self.expanded.insert(pid) {
            self.expanded.remove(&pid);
        }
        self.clear_selection_if_filtered_out();
        self.clamp_scroll();
    }

    /// Whether `pid` has at least one child in the current process set —
    /// honoring the kernel-thread pref so a parent whose only children are
    /// hidden kernel threads doesn't claim a twisty.
    fn row_has_children(&self, pid: u32) -> bool {
        self.model.processes.iter().any(|c| {
            c.ppid == Some(pid) && c.pid != pid && (self.prefs.show_kernel_threads || !c.kernel)
        })
    }

    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Monitor>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Test-only: mark the surface hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Adopt collaborative view prefs; never clear the local process catalogue.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::MonitorState) {
        self.tab = Tab::parse(&state.tab);
        self.sort_key = SortKey::parse(&state.sort_key);
        self.sort_desc = state.sort_desc;
        self.tree_mode = state.tree_mode;
        if self.search_input.value() != state.search {
            self.search_input.set_text(&state.search);
        }
        self.selected_pid = state.selected_pid;
    }

    pub(crate) fn commit_view(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::resource::MonitorAction,
    ) {
        self.actions.dispatch(doc, action);
    }

    #[cfg(test)]
    pub(crate) fn last_document_action(&self) -> Option<&ui::tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    #[cfg(test)]
    pub(crate) fn model_process_count(&self) -> usize {
        self.model.processes.len()
    }

    /// Standalone / `--shots` / unit tests: one tree from [`tilo_resource::publish`]
    /// with live process children overlaid.
    pub(crate) fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        use ui::tilo_resource::Resource as _;
        let state = crate::resource::Monitor::read(doc).unwrap_or_default();
        let mut root = ui::tilo_resource::publish::<crate::resource::Monitor>(
            "processes",
            "Processes",
            &state,
        );
        root.children = self.live_process_extras();
        vec![root]
    }

    /// Live process rows without a `monitor.processes` root.
    pub(crate) fn live_process_extras(&self) -> Vec<Resource> {
        self.model
            .processes
            .iter()
            .take(MAX_CONTEXT_PROCESS_RESOURCES)
            .map(monitor_process_resource)
            .collect()
    }

    fn commit_tab(&mut self, doc: &mut AutoCommit, tab: Tab) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.commit_view(
            doc,
            &crate::resource::MonitorAction::SetTab {
                tab: tab.as_str().into(),
            },
        );
    }

    fn commit_tree_mode(&mut self, doc: &mut AutoCommit, tree: bool) {
        if self.tree_mode == tree {
            return;
        }
        self.tree_mode = tree;
        self.scroll_off = 0.0;
        self.commit_view(doc, &crate::resource::MonitorAction::SetTreeMode { tree });
        self.clamp_scroll();
    }

    fn commit_selected(&mut self, doc: &mut AutoCommit, pid: Option<u32>) {
        if self.selected_pid == pid {
            return;
        }
        self.selected_pid = pid;
        self.commit_view(
            doc,
            &crate::resource::MonitorAction::SetSelected {
                pid: pid.map(u64::from),
            },
        );
    }

    fn set_sort(&mut self, doc: &mut AutoCommit, key: SortKey) {
        if self.sort_key == key {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort_key = key;
            self.sort_desc = key.default_desc();
        }
        self.commit_view(
            doc,
            &crate::resource::MonitorAction::SetSort {
                key: key.as_str().into(),
                desc: self.sort_desc,
            },
        );
    }

    /// Queue the exact resource address every visible process control uses.
    /// The host re-resolves it against a fresh catalogue and enforces the
    /// published permission, confirmation, and retry contract before calling
    /// [`Self::answer_resource_operation`].
    fn request_process_control(&mut self, pid: u32, action: &'static str) {
        if let Some(process) = self
            .model
            .processes
            .iter()
            .find(|process| process.pid == pid)
        {
            self.pending_control_invocation = Some(ControlInvocation {
                address: ActionAddress::new(process_resource_id(pid, process.started_at), action),
                arguments: serde_json::Map::new(),
            });
        }
    }

    /// Queue a root-level Monitor action rather than bypassing the shared
    /// action broker. Refresh is therefore available to palette search,
    /// history, collaboration, and agents through the same contract as the
    /// visible header button.
    fn request_monitor_control(&mut self, action: &'static str) {
        self.pending_control_invocation = Some(ControlInvocation {
            address: ActionAddress::new("processes", action),
            arguments: serde_json::Map::new(),
        });
    }

    /// Execute the typed operation emitted by the resource catalogue. All
    /// process controls delegate here; only the confirmation source differs
    /// between a visible UI request and an already-confirmed action contract.
    fn perform_operation(
        &mut self,
        operation: &MonitorOperation,
        confirmation: SignalConfirmation,
    ) -> Result<(), String> {
        match operation {
            MonitorOperation::Inspect { pid, started_at } => {
                let current = self
                    .model
                    .processes
                    .iter()
                    .any(|process| process.pid == *pid && process.started_at == *started_at);
                if !current {
                    return Err("process is no longer running".into());
                }
                self.selected_pid = Some(*pid);
                if self.is_narrow() {
                    self.proc_push.show_detail();
                }
                Ok(())
            }
            MonitorOperation::Refresh => {
                self.refresh_live_snapshot();
                Ok(())
            }
            MonitorOperation::CopyPid { pid, started_at } => {
                let Some(process) = self
                    .model
                    .processes
                    .iter()
                    .find(|process| process.pid == *pid && process.started_at == *started_at)
                else {
                    return Err("process is no longer running".into());
                };
                self.copy_pid(process.pid);
                Ok(())
            }
            MonitorOperation::Signal {
                pid,
                started_at,
                signal,
            } => {
                let kind = signal.stop_kind();
                let Some(process) = self
                    .model
                    .processes
                    .iter()
                    .find(|process| process.pid == *pid && process.started_at == *started_at)
                else {
                    return Err("process is no longer running".into());
                };
                if kind == StopKind::Kill
                    && confirmation == SignalConfirmation::LocalPreference
                    && self.prefs.confirm_kill
                {
                    self.confirm_stop = Some((process.pid, process.started_at, kind));
                    return Ok(());
                }
                if self.do_stop(process.pid, process.started_at, kind) {
                    Ok(())
                } else {
                    Err("the operating system refused the process signal".into())
                }
            }
        }
    }

    /// Dispatch the real signal and reflect it in the local model. Live: the
    /// sampler sends the actual SIGTERM/SIGKILL via the process handle, then
    /// immediately rebuilds the snapshot.  Dispatch acceptance is not proof
    /// that a process has exited (even SIGKILL may still be observable until
    /// the kernel reaps it), so only that fresh snapshot may remove the row.
    /// Sample/offscreen: the signal call is a no-op (the pid isn't a real
    /// process), so drop the seeded row locally to make the deterministic
    /// interaction observable.
    fn do_stop(&mut self, pid: u32, started_at: u64, kind: StopKind) -> bool {
        // Sample/offscreen rows are deliberately synthetic, so a system call
        // cannot find their pids. Treat their action as a local simulation;
        // live rows only disappear when the following sampler snapshot no
        // longer contains that exact process generation.
        if !self.sample_only && !self.sampler.signal_if_current(pid, started_at, kind) {
            return false;
        }
        if self.sample_only {
            self.model
                .processes
                .retain(|p| p.pid != pid || p.started_at != started_at);
            self.process_search.refresh(&self.model.processes);
            if self.selected_pid == Some(pid) {
                self.selected_pid = None;
            }
        } else {
            // This preserves a still-running process in the detail pane and
            // refreshes search from exactly the snapshot reflected on screen.
            self.refresh_live_snapshot();
        }
        self.confirm_stop = None;
        self.confirm_dialog = None;
        self.clamp_scroll();
        true
    }

    /// Complete the safe clipboard effect after the action broker has
    /// resolved the current process resource and its availability contract.
    fn copy_pid(&mut self, pid: u32) {
        ui::clipboard_copy(&pid.to_string());
        self.copied_pid = Some(pid);
        self.copied_at = Some(ui::time::now());
        self.selected_pid = Some(pid);
    }

    // ---- right-click context menu ----------------------------------------
    /// Open the row action menu for `pid`, anchored at `pos`. Builds the
    /// item list fresh each time so the labels carry the target process's
    /// name, and wires each action's `Fn()` callback to record a
    /// [`ProcAction`] into the shared `pending_action` cell that `on_click`
    /// drains after dispatch.
    fn open_context_menu(&mut self, pid: u32, pos: Point) {
        let name = self
            .model
            .processes
            .iter()
            .find(|p| p.pid == pid)
            .map_or_else(|| "process".into(), |p| p.name.clone());

        let mut menu = ContextMenu::new();
        let sink = &self.pending_action;
        let set = |a: ProcAction| {
            let s = sink.clone();
            move || {
                if let Ok(mut g) = s.lock() {
                    *g = Some(a);
                }
            }
        };
        // "Show details" only earns a place on narrow windows, where it pushes
        // the detail pane into view. On wide windows the detail strip is always
        // visible and the right-click already selected the row, so the item
        // would be a no-op — so it's omitted rather than offered as a lie.
        if self.is_narrow() {
            menu.push(MenuItem::action(
                "Show details",
                set(ProcAction::ShowDetails(pid)),
            ));
        }
        menu.push(MenuItem::action("Copy PID", set(ProcAction::CopyPid(pid))));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action(
            format!("Stop {name}  (SIGTERM)"),
            set(ProcAction::Stop(pid)),
        ));
        menu.push(MenuItem::action(
            format!("Kill {name}  (SIGKILL)"),
            set(ProcAction::Kill(pid)),
        ));

        // The shared primitive owns the menu's geometry: it anchors the sheet
        // at the pointer, keeps it inside this app's viewport (flipping above
        // the anchor when it would spill past the bottom), and settles its
        // measured row heights on the next draw. Hit-testing reads the same
        // rectangles back through `item_rect`, so the app never mirrors the
        // primitive's private constants.
        menu.open_anchored(Rect::from_origin_size(pos, (0.0, 0.0)), self.rect);
        self.context = menu;
        self.context_pid = Some(pid);
        // Selection is committed by the right-click caller that has the doc.
        self.selected_pid = Some(pid);
    }

    fn close_context_menu(&mut self) {
        self.context.close();
        self.context_pid = None;
    }

    // ---- interval option popover -----------------------------------------

    /// Open the refresh-interval popover anchored under the pill, with the
    /// current cadence pre-selected. Seeds the reusable `Popover` primitive
    /// with the [`INTERVAL_OPTIONS`] labels and hands it the window rect as its
    /// viewport so it flips/clamps to stay on-screen.
    fn open_interval_popover(&mut self) {
        let labels: Vec<String> = INTERVAL_OPTIONS
            .iter()
            .map(|ms| interval_label(*ms))
            .collect();
        let selected = INTERVAL_OPTIONS
            .iter()
            .position(|ms| *ms == self.prefs.interval_ms);
        self.interval_popover.set_items(labels);
        self.interval_popover.set_viewport(self.rect);
        // The pill's hit rect is known after `draw_header`; fall back to a
        // sensible header-corner anchor if the popover is opened outside a draw
        // (headless tests that never paint a frame).
        let anchor = if self.hit.interval == Rect::ZERO {
            Rect::new(
                self.rect.x1 - 140.0,
                self.rect.y0 + 20.0,
                self.rect.x1 - 40.0,
                self.rect.y0 + 48.0,
            )
        } else {
            self.hit.interval
        };
        self.interval_popover.open_at(anchor, selected);
    }

    /// Apply the interval option the popover just yielded. This changes only
    /// the current window's device-observation cadence.
    fn apply_interval_selection(&mut self, _doc: &mut AutoCommit, _path: &ObjId) {
        if let Some(sel) = self.interval_popover.take_selection()
            && let Some(&ms) = INTERVAL_OPTIONS.get(sel)
        {
            self.prefs.interval_ms = ms;
        }
    }

    /// Resolve a completed press on the interval pill. A release *after* the
    /// long-press threshold has already opened the popover (via the hold check
    /// in `draw`) leaves it up; a release *before* the threshold cycles the
    /// cadence — the plain click. A release that crosses the threshold with no
    /// paint in between (headless) opens the popover now. Returns whether a
    /// pending press was consumed.
    fn release_interval_press(&mut self, _doc: &mut AutoCommit, _path: &ObjId) -> bool {
        let Some(started) = self.interval_press.take() else {
            return false;
        };
        if self.interval_hold_opened {
            self.interval_hold_opened = false;
            return true;
        }
        if started.elapsed().as_millis() >= LONG_PRESS_MS {
            self.open_interval_popover();
        } else {
            self.prefs.cycle_interval();
        }
        true
    }

    // ---- shot-scenario seed helpers --------------------------------------
    /// Expand every process whose name is in `names` (for the tree-expanded
    /// shots). Matches on the live model names so the seed survives a relabel.
    fn expand_by_name(&mut self, names: &[&str]) {
        let pids: Vec<u32> = self
            .model
            .processes
            .iter()
            .filter(|p| names.contains(&p.name.as_str()))
            .map(|p| p.pid)
            .collect();
        self.expanded.extend(pids);
    }

    /// The pid of the busiest process with this exact name, if present.
    fn busiest_pid_named(&self, name: &str) -> Option<u32> {
        self.model
            .processes
            .iter()
            .filter(|p| p.name == name)
            .max_by(|a, b| a.cpu.total_cmp(&b.cpu))
            .map(|p| p.pid)
    }

    /// The busiest process pid currently visible in the tree.
    fn busiest_visible_pid(&self) -> Option<u32> {
        self.visible_rows()
            .into_iter()
            .max_by(|a, b| a.cpu.total_cmp(&b.cpu))
            .map(|r| r.pid)
    }

    /// Apply a [`ProcAction`] the context menu recorded. The `CopyPid` path
    /// writes the pid to the real OS clipboard via [`ui::clipboard_copy`] (a
    /// silent no-op in a headless/locked-down session) and records it so the
    /// transient toast and tests can observe the action.
    fn apply_action(&mut self, action: ProcAction, doc: &mut AutoCommit, _path: &ObjId) {
        match action {
            ProcAction::Stop(pid) => self.request_process_control(pid, "stop"),
            ProcAction::Kill(pid) => self.request_process_control(pid, "kill"),
            ProcAction::CopyPid(pid) => self.request_process_control(pid, "copy-pid"),
            ProcAction::ShowDetails(pid) => {
                self.commit_selected(doc, Some(pid));
                if self.is_narrow() {
                    self.proc_push.show_detail();
                }
            }
        }
    }

    /// Per-item rects of the open context menu, read back from the shared
    /// primitive's own painted geometry so a click maps to the row under the
    /// cursor. Returns `(item_index, rect)` for action rows only (separators
    /// aren't clickable).
    fn context_menu_item_rects(&self) -> Vec<(usize, Rect)> {
        self.context
            .items()
            .iter()
            .enumerate()
            .filter_map(|(idx, _)| self.context.item_rect(idx).map(|rect| (idx, rect)))
            .collect()
    }

    // ---- text helpers ----------------------------------------------------
    fn truncate(tr: &mut TextRenderer, s: &str, size: f32, max_w: f64) -> String {
        if max_w <= 0.0 {
            return String::new();
        }
        if f64::from(tr.measure(s, size, None).0) <= max_w {
            return s.to_string();
        }
        let mut out = s.to_string();
        while !out.is_empty() {
            out.pop();
            let cand = format!("{out}…");
            if f64::from(tr.measure(&cand, size, None).0) <= max_w {
                return cand;
            }
        }
        "…".to_string()
    }

    fn clock() -> String {
        let secs = ui::time::wall()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        format!(
            "{:02}:{:02}:{:02}",
            (secs / 3600) % 24,
            (secs / 60) % 60,
            secs % 60
        )
    }

    // ---- responsive helpers ----------------------------------------------
    /// `true` when the window is narrow enough for the single-pane Processes
    /// push flow (table OR detail, not both stacked).
    fn is_narrow(&self) -> bool {
        crate::responsive::is_narrow(self.rect.width())
    }

    // ---- content-height helpers (drive the whole-view vertical scroll) ---
    /// Natural height of the Resources dashboard body at content width `w`:
    /// the overview row (Processor / Memory / System) + the per-core card
    /// (sized to its row count) + the chart grid, each reflowed for `w` with
    /// a legible minimum chart height.
    fn resources_natural_h(&self, w: f64) -> f64 {
        let n = self.model.metrics.cores.len().max(1);
        let grid = layout::per_core_grid(n, w - 2.0 * layout::RAIL_CARD_PAD);
        let core_h = self.core_card_h(grid);
        let rows = layout::charts_rows(w);
        let min_chart_h = 150.0;
        let charts_h = rows as f64 * min_chart_h + (rows as f64 - 1.0) * GAP;
        self.resources_overview_h(w) + GAP + core_h + GAP + charts_h
    }

    /// Immediate-mode dashboard cards have fixed baseline geometry, so large
    /// text adopts the same one-column progression as a narrow display and
    /// scales its card heights. That makes additional height real page content
    /// (and therefore scrollable) rather than letting glyphs collide inside a
    /// desktop-density card.
    fn resources_overview_h(&self, w: f64) -> f64 {
        if self.layout_text_scale >= 1.5 {
            let scale = self.layout_text_scale;
            (layout::OVERVIEW_RING_H + GAP + layout::OVERVIEW_MEM_H + GAP + layout::OVERVIEW_SYS_H)
                * scale
        } else {
            layout::overview_h(w)
        }
    }

    /// The active tab's intrinsic content height, against which
    /// `view_scroll_y` is clamped. `main_w` is the body content width.
    fn content_natural_h(&self, main_w: f64) -> f64 {
        match self.tab {
            // Processes is a self-contained, internally-scrolling table; its
            // body fills the viewport and never drives the page scroll.
            Tab::Processes => 0.0,
            Tab::Resources => self.resources_natural_h(main_w),
        }
    }

    /// The page-scroll offset that brings summary stat `i`'s Resources chart
    /// (0 CPU · 1 Memory · 2 Network · 3 Disk) to the top of the viewport,
    /// clamped to the page overflow at the current body size. Clicking a
    /// summary-strip cell switches to Resources and scrolls here, so each of
    /// the four cells lands on its own chart rather than every one dumping the
    /// user at the top. Uses the exact chart-grid arithmetic `draw_resources`
    /// lays out against, so the target lands on the real chart.
    fn resources_scroll_for_stat(&self, i: usize, body_w: f64, viewport_h: f64) -> f32 {
        let n = self.model.metrics.cores.len().max(1);
        let grid = layout::per_core_grid(n, body_w - 2.0 * layout::RAIL_CARD_PAD);
        let core_h = self.core_card_h(grid);
        let charts_top = self.resources_overview_h(body_w) + GAP + core_h + GAP;
        let cols = layout::charts_cols(body_w);
        let row = (i / cols) as f64;
        let min_chart_h = 150.0;
        let target = charts_top + row * (min_chart_h + GAP);
        let overflow = (self.resources_natural_h(body_w) - viewport_h).max(0.0);
        (target as f32).clamp(0.0, overflow as f32)
    }

    /// The body content width and viewport height the page-scroll math uses,
    /// derived from the current outer `rect` (mirrors `draw`'s split of the
    /// frame into header + body).
    fn body_metrics(&self) -> (f64, f64) {
        let body_w = (self.rect.width() - 2.0 * OUTER_PAD).max(1.0);
        let viewport_h =
            (self.rect.height() - 2.0 * OUTER_PAD - self.density.header_h - GAP).max(1.0);
        (body_w, viewport_h)
    }

    /// Height of the summary strip wrapped to `g` — `layout::summary_strip_h`
    /// with each row grown by this frame's measured cell height.
    fn strip_h(&self, g: layout::StripGrid) -> f64 {
        g.rows as f64 * self.density.strip_cell_h + (g.rows as f64 - 1.0) * GAP
    }

    /// Height of the per-core card for `grid` — `layout::per_core_card_h` with
    /// every gauge row grown by its measured label height.
    fn core_card_h(&self, grid: layout::CoreGrid) -> f64 {
        layout::per_core_card_h(grid) + grid.rows as f64 * self.density.core_extra
    }

    /// Whether the "PID copied" toast should currently show — true for a short
    /// window after a Copy PID action, then it expires on its own.
    fn copy_toast_active(&self) -> bool {
        self.copied_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(TOAST_MS))
    }

    // ---- headless screenshot scenarios -----------------------------------
    /// Seed a deterministic interaction state for a headless `--shots` frame.
    /// `--shots` renders a single frame with no event loop, so a scenario that
    /// would normally require live input (an open overlay rail, a pushed
    /// detail pane) is snapped here instead. Unknown names render the default
    /// frame — at a narrow viewport that is the closed-rail process table.
    ///
    /// Recognised scenarios:
    /// * `"detail"` — snap the narrow Processes push flow to the pushed detail
    ///   pane (otherwise a narrow frame opens to the table, as a fresh window
    ///   would).
    /// * `"interval-popover"` — open the refresh-interval option popover under
    ///   the header pill (long-press invoked at runtime).
    /// * `"resources"`      — switch to the Resources dashboard so a shot shows
    ///   the overview row + per-core grid + chart grid.
    /// * `"resources-many"` — Resources tab seeded with a high core count so a
    ///   short/narrow shot demonstrates the per-core card growing to fit every
    ///   core and the whole view becoming vertically scrollable.
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        match scenario {
            "detail" => self.proc_push.snap(PushPane::Detail),
            // Open the refresh-interval option popover (anchored under the pill,
            // which isn't positioned until `draw_header` runs, so defer it).
            "interval-popover" => self.interval_popover_seed = true,
            "resources" => self.tab = Tab::Resources,
            // Sparse-state coverage: an honest no-data panel and a filter
            // no-match panel have distinct recovery copy, both suitable for
            // deterministic visual review.
            "empty" => {
                self.model.processes.clear();
                self.model.process_status = ProcessDataStatus::Empty;
                self.selected_pid = None;
            }
            "process-loading" => {
                self.model.processes.clear();
                self.model.process_status = ProcessDataStatus::Loading;
                self.selected_pid = None;
            }
            "process-permission-error" => {
                self.model.processes.clear();
                self.model.process_status = ProcessDataStatus::PermissionDenied;
                self.selected_pid = None;
            }
            "no-results" => {
                self.search_input.set_text("__no_process_matches__");
                self.clear_selection_if_filtered_out();
            }
            // ---- process-tree scenarios -----------------------------------
            // The default frame is already the tree COLLAPSED with summed
            // parent totals (tree_mode on, expanded empty, Sum default). The
            // scenarios below snap the other states a single headless frame
            // can't reach through live input.
            "tree-collapsed" => {
                // Explicit: collapse everything, Sum mode (the default), so a
                // shot shows parents carrying their full subtree totals.
                self.expanded.clear();
                self.sum_mode = SumMode::SumSubtree;
            }
            "tree-expanded" => {
                // Expand the heavy app subtrees so children appear with their
                // OWN usage beneath the (still-summed) parents.
                self.sum_mode = SumMode::SumSubtree;
                self.expand_by_name(&["shell", "browser", "code", "cargo", "systemd"]);
            }
            "per-process" => {
                // Same expanded tree, but Per-process: every row (parent or
                // child) shows only its own usage.
                self.sum_mode = SumMode::PerProcess;
                self.expand_by_name(&["shell", "browser", "code", "cargo", "systemd"]);
            }
            "per-process-collapsed" => {
                // Collapsed + Per-process: parents show ONLY their own usage,
                // so the same rows as "tree-collapsed" read much lower.
                self.expanded.clear();
                self.sum_mode = SumMode::PerProcess;
            }
            "context-menu" => {
                // Open the right-click action menu over the busiest expanded
                // child so the menu + a populated tree both show. Expand cargo
                // too so the seeded `rustc` row is actually visible.
                self.sum_mode = SumMode::SumSubtree;
                self.expand_by_name(&["shell", "browser", "code", "cargo"]);
                // Seed by name when visible, else the busiest visible row.
                self.context_menu_seed = self
                    .busiest_pid_named("rustc")
                    .or_else(|| self.busiest_visible_pid());
            }
            "resources-many" => {
                self.tab = Tab::Resources;
                // 24 cores so the grid wraps to several rows — the card must
                // grow to hold them and the page must scroll on a short shot.
                self.model.metrics.cores = (0..24)
                    .map(|i| {
                        let t = i as f32;
                        (30.0 + 55.0 * (t * 0.7).sin() + 20.0 * (t * 0.3).cos()).clamp(2.0, 99.0)
                    })
                    .collect();
            }
            // The shared kill/stop confirm dialog over the process table.
            "kill-confirm" => {
                if let Some(process) = self
                    .selected_pid
                    .and_then(|pid| self.model.processes.iter().find(|p| p.pid == pid))
                    .or_else(|| self.model.processes.first())
                {
                    self.confirm_stop = Some((process.pid, process.started_at, StopKind::Kill));
                }
            }
            _ => {}
        }
    }
}

// ==========================================================================
// Drawing
// ==========================================================================
impl MonitorApp {
    #[allow(
        clippy::too_many_lines,
        reason = "the header measures, lays out, paints, and records all three related controls together so their narrow-width collision guarantees stay local"
    )]
    fn draw_header(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let c = &theme.tokens.colors;
        let inner = paint::panel(scene, t, r, theme, 12.0);
        let xs = ts(tr, theme.tokens.typography.scale.xs);
        let sm = ts(tr, theme.tokens.typography.scale.sm);
        // Segment/pill height: the 1× 30 px control grown by its `sm` label's
        // measured line growth, so the tabs and the interval pill keep seating
        // their text at every accessibility scale.
        let seg_h = 30.0 + self.density.sm_extra;
        let cy = inner.y0 + (inner.height() - seg_h) / 2.0;

        // The header lays out two groups: centred tabs and the clock/interval
        // block — and they must never overlap, however narrow the window. We
        // measure each, then shrink the least essential parts (the uptime
        // caption, finally the tab labels → icon-only pills) until they fit. The
        // tab control and the clock+interval are always present.
        // At 200% text the header has the same information hierarchy as a
        // narrow window: tabs and the three direct controls remain, while
        // their visible labels/clock yield to the OS chrome and a11y names.
        // Keeping every full-size label here would overlap the clock, controls
        // and tabs before normal responsive width breakpoints apply.
        let large_text = theme.text_scale >= 1.5;
        let header_narrow = inner.width() < 560.0 || large_text;

        // ---- right block: clock (top) + uptime caption (bottom) + pill ----
        let clock = Self::clock();
        let cs = ts(tr, theme.tokens.typography.scale.lg);
        let (clw, _) = tr.measure_weighted(&clock, cs, None, TextWeight::Bold);
        let up = fmt_uptime(self.model.metrics.uptime_secs);
        let up_caption = format!("uptime {up}");
        let (upw, _) = tr.measure(&up_caption, xs, None);
        let right_block_w = if large_text {
            0.0
        } else {
            f64::from(clw).max(f64::from(upw))
        };
        let right_block_x0 = inner.x1 - right_block_w;

        // Measured at the size the pill draws its label (`sm` below), so the
        // capsule always seats it.
        let il = format!("⟳ {}", self.prefs.interval_label());
        let iw = f64::from(tr.measure_weighted(&il, sm, None, TextWeight::Medium).0) + 22.0;
        let pill_x1 = right_block_x0 - 16.0;
        let pill_x0 = pill_x1 - iw;
        // One-shot refresh and pause/resume stay beside the cadence chooser:
        // they are small, icon-only controls with full accessible names. This
        // gives a paused monitor an obvious recovery path without turning the
        // title bar into a toolbar.
        let show_tool_labels = inner.width() >= 900.0 && !large_text;
        let refresh_label = "Refresh";
        let auto_refresh_label = if self.prefs.auto_refresh {
            "Pause"
        } else {
            "Resume"
        };
        let mut control_w = |label: &str| {
            if show_tool_labels {
                f64::from(
                    tr.measure_weighted(
                        label,
                        ts(tr, theme.tokens.typography.scale.sm),
                        None,
                        TextWeight::Medium,
                    )
                    .0,
                ) + 38.0
            } else {
                32.0
            }
        };
        let refresh_w = control_w(refresh_label);
        let auto_refresh_w = control_w(auto_refresh_label);
        let tool_gap = 8.0;
        let refresh_r = Rect::new(
            pill_x0 - tool_gap - refresh_w,
            cy + 1.0,
            pill_x0 - tool_gap,
            cy + seg_h - 1.0,
        );
        let auto_refresh_r = Rect::new(
            refresh_r.x0 - tool_gap - auto_refresh_w,
            cy + 1.0,
            refresh_r.x0 - tool_gap,
            cy + seg_h - 1.0,
        );

        // The OS owns window chrome (app-name/logo/subtitle) — the header draws
        // only its functional controls (tabs, clock, interval pill), so the tab
        // control gets the full width from the left edge.
        let brand_right = inner.x0;

        // ---- centre tab segmented control ---------------------------------
        // Tabs want a comfortable width; clamp them into the gap between the
        // brand and the interval pill, and fall back to icon-only pills when
        // even the clamped width can't seat the labels.
        let gap_l = brand_right + 16.0;
        let gap_r = auto_refresh_r.x0 - 12.0;
        let avail_tabs = (gap_r - gap_l).max(0.0);
        // The 1× design: 124 px segments that fold to icon-only pills below a
        // 96 px pair. Both grow by exactly the labels' measured growth over
        // their 1× width, so the 1× thresholds are untouched and a 200 % bar
        // still seats `Processes` / `Resources` before it folds.
        let (pw, _) = tr.measure_weighted(Tab::Processes.label(), sm, None, TextWeight::Semibold);
        let (rw, _) = tr.measure_weighted(Tab::Resources.label(), sm, None, TextWeight::Semibold);
        let label_w = f64::from(pw.max(rw));
        let label_growth = (label_w - label_w / self.density.text_k).max(0.0);
        let want_seg_w = 124.0 + label_growth;
        let compact = avail_tabs < 2.0 * (96.0 + label_growth) + 6.0;
        let seg_w = if compact {
            ((avail_tabs - 6.0) / 2.0).clamp(40.0, 64.0)
        } else {
            want_seg_w.min((avail_tabs - 6.0) / 2.0)
        };
        let pair_w = 2.0 * seg_w + 6.0;
        // Centre the pair in the gap, but never let it cross either edge.
        let centre = f64::midpoint(inner.x0, inner.x1);
        let cx = (centre - pair_w / 2.0).clamp(gap_l, (gap_r - pair_w).max(gap_l));
        let proc_r = Rect::new(cx, cy, cx + seg_w, cy + seg_h);
        let res_r = Rect::new(cx + seg_w + 6.0, cy, cx + 2.0 * seg_w + 6.0, cy + seg_h);
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        self.draw_tab_pill(&mut ctx, proc_r, Tab::Processes, compact);
        self.draw_tab_pill(&mut ctx, res_r, Tab::Resources, compact);
        self.hit.tab_proc = proc_r;
        self.hit.tab_res = res_r;

        // Uptime caption only when it clears the tab control on its line.
        let show_uptime = !header_narrow && right_block_x0 > res_r.x1 + 16.0;

        // Clock (always shown, top line, right-aligned in the block).
        tr.draw_text_weighted(
            scene,
            &clock,
            cs,
            c.text_primary,
            t * Affine::translate((inner.x1 - f64::from(clw), inner.y0 + 1.0)),
            None,
            TextWeight::Bold,
        );
        // Uptime caption (second line) — dropped when space is tight. It
        // stacks on the clock's measured line box so the pair never overlaps.
        if show_uptime {
            let caption_top = inner.y0 + 1.0 + paint::line_box(tr, cs) + 1.0;
            tr.draw_text(
                scene,
                &up_caption,
                xs,
                c.text_dim,
                t * Affine::translate((inner.x1 - f64::from(upw), caption_top)),
                None,
            );
        }

        // Interval pill, left of the right block.
        let interval_r = Rect::new(pill_x0, cy + 1.0, pill_x1, cy + seg_h - 1.0);
        ui::surface::card(scene, t, interval_r, seg_h / 2.0, c.lift(0.35));
        let (tw, th) = tr.measure_weighted(
            &il,
            ts(tr, theme.tokens.typography.scale.sm),
            None,
            TextWeight::Medium,
        );
        tr.draw_text_weighted(
            scene,
            &il,
            ts(tr, theme.tokens.typography.scale.sm),
            c.text_primary,
            t * Affine::translate((
                interval_r.x0 + (interval_r.width() - f64::from(tw)) / 2.0,
                interval_r.y0 + (interval_r.height() - f64::from(th)) / 2.0,
            )),
            None,
            TextWeight::Medium,
        );
        self.hit.interval = interval_r;
        for (rect, icon, label, active) in [
            (refresh_r, Icon::Refresh, refresh_label, false),
            (
                auto_refresh_r,
                if self.prefs.auto_refresh {
                    Icon::Pause
                } else {
                    Icon::Play
                },
                auto_refresh_label,
                self.prefs.auto_refresh,
            ),
        ] {
            paint::fill_rrect(
                scene,
                t,
                rect,
                6.0,
                c.lift(if active { 0.55 } else { 0.35 }),
            );
            paint::stroke_rrect(
                scene,
                t,
                rect,
                6.0,
                if rect.contains(self.last_cursor) {
                    c.accent[5]
                } else {
                    ui::surface::edge_on(c.lift(if active { 0.55 } else { 0.35 }))
                },
                if rect.contains(self.last_cursor) {
                    1.5
                } else {
                    1.0
                },
            );
            let icon_r = Rect::new(
                rect.x0 + 8.0,
                rect.center().y - 7.0,
                rect.x0 + 22.0,
                rect.center().y + 7.0,
            );
            icons::draw_icon(scene, t, icon_r, icon, c.text_primary, 1.6);
            if show_tool_labels {
                let (_, th) = tr.measure_weighted(
                    label,
                    ts(tr, theme.tokens.typography.scale.sm),
                    None,
                    TextWeight::Medium,
                );
                tr.draw_text_weighted(
                    scene,
                    label,
                    ts(tr, theme.tokens.typography.scale.sm),
                    c.text_primary,
                    t * Affine::translate((icon_r.x1 + 5.0, rect.center().y - f64::from(th) / 2.0)),
                    None,
                    TextWeight::Medium,
                );
            }
        }
        self.hit.refresh = refresh_r;
        self.hit.auto_refresh = auto_refresh_r;
    }

    /// Draw `tab`'s pill in `r`, filled when it is the open tab. `compact`
    /// draws an icon-only pill (the label is dropped) so the tab control still
    /// fits when the header is narrow.
    fn draw_tab_pill(&self, ctx: &mut PaintCtx<'_>, r: Rect, tab: Tab, compact: bool) {
        let PaintCtx {
            scene,
            transform: t,
            text: tr,
            theme,
        } = ctx.reborrow();
        let (icon, label, active) = (tab.icon(), tab.label(), self.tab == tab);
        let c = &theme.tokens.colors;
        let radius = r.height() / 2.0;
        if active {
            paint::fill_rrect(scene, t, r, radius, c.accent[5]);
        } else {
            ui::surface::card(scene, t, r, radius, c.lift(0.35));
        }
        let text_color = if active {
            c.on_surface(c.accent[5]).ink
        } else {
            c.text_dim
        };
        if compact {
            // Centre the icon; no label.
            let icon_r = Rect::new(
                r.center().x - 8.0,
                r.center().y - 8.0,
                r.center().x + 8.0,
                r.center().y + 8.0,
            );
            icons::draw_icon(scene, t, icon_r, icon, text_color, 1.6);
            return;
        }
        let icon_r = Rect::new(
            r.x0 + 14.0,
            r.center().y - 8.0,
            r.x0 + 30.0,
            r.center().y + 8.0,
        );
        icons::draw_icon(scene, t, icon_r, icon, text_color, 1.6);
        let fs = ts(tr, theme.tokens.typography.scale.sm);
        let (_, th) = tr.measure_weighted(label, fs, None, TextWeight::Semibold);
        tr.draw_text_weighted(
            scene,
            label,
            fs,
            text_color,
            t * Affine::translate((icon_r.x1 + 8.0, r.center().y - f64::from(th) / 2.0)),
            None,
            TextWeight::Semibold,
        );
    }

    /// The compact horizontal **summary strip** at the top of the Processes
    /// tab: four small readouts — CPU / Memory / Network / Disk — each a
    /// label, a headline number and a sparkline. This is the old left rail's
    /// content, rehomed as a slim content band above the full-width table
    /// instead of a fake sidebar. It stays a single row at every width
    /// (`layout::summary_grid`) so the table below keeps its height; the cells
    /// share the width and stack their value under the label when narrow.
    ///
    /// These intentionally are not [`ui::widget::StatTile`]s: `StatTile` is
    /// an informational, fixed-content metric card, while each of these is a
    /// compact navigation control with a live sparkline footer and a distinct
    /// destination in the Resources dashboard. Replacing it would remove the
    /// trend visual or its useful direct-manipulation behaviour; the semantic
    /// navigation contract is covered by `summary_cell_click_switches_and_scrolls_to_that_stat`.
    /// Clicking a cell jumps to the
    /// Resources dashboard where that stat's full gauge/chart lives; the cell
    /// hit rects are recorded for `on_click`.
    #[allow(
        clippy::many_single_char_names,
        reason = "t/r/g/c/i are this file's established short names for transform/rect/grid/colors/index, used the same way throughout every draw_* function"
    )]
    fn draw_summary_strip(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
        g: layout::StripGrid,
    ) {
        // Own the per-cell spec up front so the immutable metrics borrow is
        // released before we push into `self.hit`.
        struct Cell {
            label: &'static str,
            value: String,
            vcolor: Color,
            data: Vec<f32>,
            max: f32,
            spark: Color,
        }
        let c = &theme.tokens.colors;
        let cells = {
            let m = &self.model.metrics;
            let overall = m.cpu_overall();
            let net_max = m.net_rx_hist.max().max(m.net_tx_hist.max()).max(1.0);
            let disk_max = m.disk_read_hist.max().max(m.disk_write_hist.max()).max(1.0);
            [
                Cell {
                    label: "CPU",
                    value: format!("{overall:.0}%"),
                    vcolor: paint::load_color(theme, overall / 100.0),
                    data: m.cpu_hist.as_slice().to_vec(),
                    max: 100.0,
                    spark: c.accent[5],
                },
                Cell {
                    label: "Memory",
                    value: format_bytes(m.mem_used),
                    vcolor: c.text_primary,
                    data: m.mem_hist.as_slice().to_vec(),
                    max: 1.0,
                    spark: c.semantic.success,
                },
                Cell {
                    label: "Network",
                    value: fmt_rate(m.net_rx + m.net_tx),
                    vcolor: c.text_primary,
                    data: m.net_rx_hist.as_slice().to_vec(),
                    max: net_max,
                    spark: c.semantic.info,
                },
                Cell {
                    label: "Disk",
                    value: fmt_rate(m.disk_read + m.disk_write),
                    vcolor: c.text_primary,
                    data: m.disk_read_hist.as_slice().to_vec(),
                    max: disk_max,
                    spark: c.semantic.warning,
                },
            ]
        };

        self.hit.summary_cells.clear();
        let cols = g.cols;
        let cell_w = (r.width() - (cols as f64 - 1.0) * GAP) / cols as f64;
        let xs = ts(tr, theme.tokens.typography.scale.xs);
        let sm = ts(tr, theme.tokens.typography.scale.sm);
        for (i, cell) in cells.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let x0 = r.x0 + col as f64 * (cell_w + GAP);
            let y0 = r.y0 + row as f64 * (self.density.strip_cell_h + GAP);
            let rect = Rect::new(x0, y0, x0 + cell_w, y0 + self.density.strip_cell_h);
            let inner = paint::panel(scene, t, rect, theme, layout::STRIP_CELL_PAD);
            // Label (dim, left). The single-row strip keeps a constant height so
            // the table below never loses height; the cell instead adapts
            // horizontally — value beside the label when it fits, else stacked
            // beneath it — so a narrow cell never overlaps its own text.
            let (lw, lh) = tr.measure(cell.label, xs, None);
            let (vw, vh) = tr.measure_weighted(&cell.value, sm, None, TextWeight::Bold);
            // The label and the (larger) value share one centre line, so the
            // two sizes sit level rather than aligning their tops.
            let line_c = inner.y0 + f64::from(lh.max(vh)) / 2.0;
            tr.draw_text(
                scene,
                cell.label,
                xs,
                c.text_dim,
                t * Affine::translate((inner.x0, line_c - f64::from(lh) / 2.0)),
                None,
            );
            // Both branches step by the measured line boxes (22 / 34 px at 1×),
            // so the sparkline clears the text at every accessibility scale.
            let spark_top = if f64::from(lw) + 8.0 + f64::from(vw) <= inner.width() {
                // Roomy: headline value right-aligned on the label's line.
                tr.draw_text_weighted(
                    scene,
                    &cell.value,
                    sm,
                    cell.vcolor,
                    t * Affine::translate((inner.x1 - f64::from(vw), line_c - f64::from(vh) / 2.0)),
                    None,
                    TextWeight::Bold,
                );
                inner.y0 + f64::from(lh.max(vh)) + 7.0
            } else {
                // Tight: value drops onto its own line under the label so the
                // two never collide, and the sparkline shifts down to clear it.
                let value_top = inner.y0 + f64::from(lh) + 1.5;
                tr.draw_text_weighted(
                    scene,
                    &cell.value,
                    sm,
                    cell.vcolor,
                    t * Affine::translate((inner.x0, value_top)),
                    None,
                    TextWeight::Bold,
                );
                value_top + f64::from(vh) + 5.0
            };
            // Sparkline fills the remaining lower portion of the cell.
            let sr = Rect::new(inner.x0, spark_top.min(inner.y1), inner.x1, inner.y1);
            Sparkline::draw_at(scene, t, sr, &cell.data, cell.max, cell.spark);
            self.hit.summary_cells.push(rect);
        }
    }

    /// The processor card's CPU-load ring, centred on `center` and sized for
    /// the text `scale`.
    fn cpu_ring(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        center: Point,
        scale: f64,
        overall: f32,
    ) {
        RingGauge::draw_styled(
            &mut PaintCtx::new(scene, t, tr, theme),
            Circle::new(center, 44.0 * scale),
            9.0 * scale,
            &GaugeReading {
                frac: overall / 100.0,
                color: paint::load_color(theme, overall / 100.0),
                label: "CPU LOAD",
                value: &format!("{overall:.0}%"),
            },
            &GAUGE,
        );
    }

    /// The Resources overview **Processor** card: a CPU-load ring gauge, plus
    /// (when the card is wide enough) a compact stats column — load average,
    /// process and thread counts. This is the old rail's top section as
    /// dashboard content.
    fn draw_processor_card(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let scale = f64::from(theme.text_scale);
        let c = &theme.tokens.colors;
        let m = &self.model.metrics;
        let inner = paint::panel(scene, t, rect, theme, 12.0);
        tr.draw_text_weighted(
            scene,
            "Processor",
            ts(tr, theme.tokens.typography.scale.sm),
            c.text_primary,
            t * Affine::translate((inner.x0, inner.y0)),
            None,
            TextWeight::Semibold,
        );
        let overall = m.cpu_overall();
        let xs = ts(tr, theme.tokens.typography.scale.xs);
        let sm = ts(tr, theme.tokens.typography.scale.sm);
        if inner.width() >= 300.0 {
            // Ring on the left, stats column on the right.
            // Every immediate-mode offset follows the text scale too. The
            // card's height already scales in `resources_overview_h`; without
            // scaling these positions a 200% card left its labels piled up at
            // the top and most of the card empty.
            let center = Point::new(inner.x0 + 62.0 * scale, inner.center().y + 8.0 * scale);
            Self::cpu_ring(scene, t, tr, theme, center, scale, overall);
            let sx = inner.x0 + 138.0 * scale;
            let procs = self.model.processes.len();
            let threads: usize = self.model.processes.iter().map(|p| p.threads).sum();
            let stats = [
                (
                    "Load average",
                    format!(
                        "{:.2}  {:.2}  {:.2}",
                        m.load_avg[0], m.load_avg[1], m.load_avg[2]
                    ),
                ),
                ("Processes", format!("{procs}")),
                ("Threads", format!("{threads}")),
            ];
            let mut sy = inner.y0 + 30.0 * scale;
            for (label, value) in stats {
                tr.draw_text(
                    scene,
                    label,
                    xs,
                    c.text_dim,
                    t * Affine::translate((sx, sy)),
                    None,
                );
                let value = Self::truncate(tr, &value, sm, inner.x1 - sx);
                tr.draw_text_weighted(
                    scene,
                    &value,
                    sm,
                    c.text_primary,
                    t * Affine::translate((sx, sy + 15.0 * scale)),
                    None,
                    TextWeight::Semibold,
                );
                sy += 40.0 * scale;
            }
        } else {
            // Narrow/stacked: centred ring with the load average beneath it.
            let center = Point::new(inner.center().x, inner.y0 + 84.0 * scale);
            Self::cpu_ring(scene, t, tr, theme, center, scale, overall);
            let la = format!(
                "load  {:.2}  {:.2}  {:.2}",
                m.load_avg[0], m.load_avg[1], m.load_avg[2]
            );
            let (lw, _) = tr.measure(&la, xs, None);
            tr.draw_text(
                scene,
                &la,
                xs,
                c.text_dim,
                t * Affine::translate((
                    inner.center().x - f64::from(lw) / 2.0,
                    inner.y1 - 12.0 * scale,
                )),
                None,
            );
        }
    }

    /// The Resources overview **Memory** card: RAM + swap bar gauges, plus a
    /// memory-history sparkline when the card is tall enough.
    fn draw_memory_card(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let c = &theme.tokens.colors;
        let m = &self.model.metrics;
        let mi = paint::panel(scene, t, rect, theme, 12.0);
        tr.draw_text_weighted(
            scene,
            "Memory",
            ts(tr, theme.tokens.typography.scale.sm),
            c.text_primary,
            t * Affine::translate((mi.x0, mi.y0)),
            None,
            TextWeight::Semibold,
        );
        let scale = f64::from(theme.text_scale);
        let gauge_h = 22.0 * scale;
        let ram_y = mi.y0 + 24.0 * scale;
        let ram_label = format!(
            "{} / {}",
            format_bytes(m.mem_used),
            format_bytes(m.mem_total)
        );
        BarGauge::draw_styled(
            &mut PaintCtx::new(scene, t, tr, theme),
            Rect::new(mi.x0, ram_y, mi.x1, ram_y + gauge_h),
            &GaugeReading {
                frac: m.mem_frac(),
                color: paint::load_color(theme, m.mem_frac()),
                label: "RAM",
                value: &ram_label,
            },
            &GAUGE,
        );
        let swap_label = format!(
            "{} / {}",
            format_bytes(m.swap_used),
            format_bytes(m.swap_total)
        );
        BarGauge::draw_styled(
            &mut PaintCtx::new(scene, t, tr, theme),
            Rect::new(
                mi.x0,
                ram_y + gauge_h + 10.0 * scale,
                mi.x1,
                ram_y + 2.0 * gauge_h + 10.0 * scale,
            ),
            &GaugeReading {
                frac: m.swap_frac(),
                color: c.accent[5],
                label: "Swap",
                value: &swap_label,
            },
            &GAUGE,
        );
        // A memory sparkline fills the remaining height on the taller (abreast
        // or fixed-height) card.
        if mi.height() >= 108.0 * scale {
            let sr = Rect::new(mi.x0, ram_y + 2.0 * gauge_h + 24.0 * scale, mi.x1, mi.y1);
            Sparkline::draw_at(scene, t, sr, m.mem_hist.as_slice(), 1.0, c.semantic.success);
        }
    }

    /// The Resources overview **System** card: host / kernel / cores / uptime
    /// and the CPU brand — the old rail info card, now dashboard content.
    fn draw_system_card(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let scale = f64::from(theme.text_scale);
        let c = &theme.tokens.colors;
        let m = &self.model.metrics;
        let ii = paint::panel(scene, t, rect, theme, 12.0);
        tr.draw_text_weighted(
            scene,
            "System",
            ts(tr, theme.tokens.typography.scale.sm),
            c.text_primary,
            t * Affine::translate((ii.x0, ii.y0)),
            None,
            TextWeight::Semibold,
        );
        let fs = ts(tr, theme.tokens.typography.scale.xs);
        let mut y = ii.y0 + 24.0;
        let line = |scene: &mut Scene, tr: &mut TextRenderer, k: &str, v: String, y: &mut f64| {
            tr.draw_text(
                scene,
                k,
                fs,
                c.text_dim,
                t * Affine::translate((ii.x0, *y)),
                None,
            );
            let vv = Self::truncate(tr, &v, fs, ii.width() - 60.0);
            let (vw, _) = tr.measure(&vv, fs, None);
            tr.draw_text(
                scene,
                &vv,
                fs,
                c.text_primary,
                t * Affine::translate((ii.x1 - f64::from(vw), *y)),
                None,
            );
            *y += 18.0 * scale;
        };
        line(scene, tr, "host", m.host.clone(), &mut y);
        line(scene, tr, "kernel", m.kernel.clone(), &mut y);
        line(scene, tr, "cores", format!("{}", m.cores.len()), &mut y);
        line(scene, tr, "uptime", fmt_uptime(m.uptime_secs), &mut y);
        if y + 14.0 * scale <= ii.y1 {
            let cpu = Self::truncate(tr, &m.cpu_brand, fs, ii.width());
            tr.draw_text(
                scene,
                &cpu,
                fs,
                c.text_dim,
                t * Affine::translate((ii.x0, y)),
                None,
            );
        }
    }

    /// The Resources **per-core utilisation** card: one bar gauge per core in a
    /// grid that wraps to as many rows as the core count needs, sized so the
    /// last row renders inside the card (`layout::per_core_card_h`).
    fn draw_per_core(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        core_panel: Rect,
        grid: layout::CoreGrid,
    ) {
        let c = &theme.tokens.colors;
        let n = self.model.metrics.cores.len().max(1);
        let ci = paint::panel(scene, t, core_panel, theme, layout::RAIL_CARD_PAD);
        tr.draw_text_weighted(
            scene,
            &format!("Per-Core Utilisation  ·  {n} threads"),
            ts(tr, theme.tokens.typography.scale.sm),
            c.text_primary,
            t * Affine::translate((ci.x0, ci.y0)),
            None,
            TextWeight::Semibold,
        );
        let scale = f64::from(theme.text_scale);
        let grid_top = ci.y0 + layout::CORE_TITLE_H * scale;
        let cols = grid.cols;
        let cell_w = (ci.width() - (cols as f64 - 1.0) * layout::CORE_GAP) / cols as f64;
        for (i, &usage) in self.model.metrics.cores.iter().enumerate() {
            let col = i % cols;
            let rowi = i / cols;
            let x0 = ci.x0 + col as f64 * (cell_w + layout::CORE_GAP);
            let y0 = grid_top + rowi as f64 * (layout::CORE_ROW_STRIDE + self.density.core_extra);
            BarGauge::draw_styled(
                &mut PaintCtx::new(scene, t, tr, theme),
                Rect::new(x0, y0, x0 + cell_w, y0 + 22.0 * scale),
                &GaugeReading {
                    frac: usage / 100.0,
                    color: paint::load_color(theme, usage / 100.0),
                    label: &format!("C{i}"),
                    value: &format!("{usage:.0}%"),
                },
                &GAUGE,
            );
        }
    }

    /// The Processes tab: a compact summary strip, a search/sort toolbar, and
    /// a **full-width** process table — the table is the content. Wide: the
    /// selected process's detail strip stacks below the table. Narrow:
    /// single-pane push — the strip+toolbar+table OR the detail, sliding
    /// horizontally as one swaps for the other (driven by [`PushNav`]).
    /// Selecting a row pushes the detail in with a Back affordance; Back (or
    /// Esc) slides the table back.
    fn draw_processes(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let strip_g = layout::summary_grid(r.width());
        let strip_h = self.strip_h(strip_g);

        if !self.is_narrow() {
            // Wide: summary strip, toolbar, table, detail strip stacked.
            let strip = Rect::new(r.x0, r.y0, r.x1, r.y0 + strip_h);
            self.draw_summary_strip(scene, t, tr, theme, strip, strip_g);
            let toolbar_h = self.toolbar_height(tr, theme, r.width());
            let toolbar = Rect::new(r.x0, strip.y1 + GAP, r.x1, strip.y1 + GAP + toolbar_h);
            self.draw_toolbar(scene, t, tr, theme, toolbar);
            let table_top = toolbar.y1 + GAP;
            let table_bottom = r.y1 - self.density.detail_h - GAP;
            let table_panel = Rect::new(r.x0, table_top, r.x1, table_bottom);
            self.draw_process_table(scene, t, tr, theme, table_panel);
            let detail = Rect::new(r.x0, table_bottom + GAP, r.x1, r.y1);
            self.draw_detail(scene, t, tr, theme, detail);
            return;
        }

        // Narrow: single-pane push. The table slab and detail slab each fill
        // the whole content area and slide horizontally; `slide` (0 = table,
        // 1 = detail) offsets both, so the panes glide as one swaps for the
        // other. Clip to the content so the off-screen slab stays hidden.
        let slide = self.proc_push.slide();
        let table_x = r.x0 - r.width() * slide; // slides off to the left
        let detail_x = r.x0 + r.width() * (1.0 - slide); // slides in from the right
        let table_slab = Rect::new(table_x, r.y0, table_x + r.width(), r.y1);
        let detail_slab = Rect::new(detail_x, r.y0, detail_x + r.width(), r.y1);

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &r);

        // Table slab: summary strip + toolbar + full-height table (no detail
        // reservation — the detail is its own pushed pane on narrow).
        if slide < 0.999 {
            let strip = Rect::new(
                table_slab.x0,
                table_slab.y0,
                table_slab.x1,
                table_slab.y0 + strip_h,
            );
            self.draw_summary_strip(scene, t, tr, theme, strip, strip_g);
            let toolbar_h = self.toolbar_height(tr, theme, table_slab.width());
            let toolbar = Rect::new(
                table_slab.x0,
                strip.y1 + GAP,
                table_slab.x1,
                strip.y1 + GAP + toolbar_h,
            );
            self.draw_toolbar(scene, t, tr, theme, toolbar);
            let table_panel = Rect::new(
                table_slab.x0,
                toolbar.y1 + GAP,
                table_slab.x1,
                table_slab.y1,
            );
            self.draw_process_table(scene, t, tr, theme, table_panel);
        }

        // Detail slab: a Back affordance header above a full-area detail card.
        if slide > 0.001 {
            self.draw_detail_pane(scene, t, tr, theme, detail_slab);
        }

        scene.pop_layer();
    }

    /// Draw the process table (header row + scrollable rows + scrollbar) into
    /// `table_panel`. Shared by the wide stacked layout and the narrow push
    /// table slab; records the sort-header and row hit rects.
    fn draw_process_table(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        table_panel: Rect,
    ) {
        let ti = paint::panel(scene, t, table_panel, theme, 0.0);
        let pad = layout::TABLE_PAD;
        // The columns' content viewport: the card inner box minus left/right
        // padding. Columns are laid out in *content space* (x from 0) and the
        // whole strip is shifted left by `table_scroll_x` when it overflows.
        let cols_left = ti.x0 + pad;
        let cols_right = ti.x1 - pad;
        let avail_w = (cols_right - cols_left).max(1.0);

        // Column widths are glyph budgets designed at 1×: lay the set out in
        // 1× space and scale it by the text scale, so a column that seats
        // `622.0 MiB` at 1× still seats it at 200 % (and the width-dependent
        // column set folds exactly as it would in a proportionally narrower
        // 1× window). Header and cell hit rects read these scaled columns.
        let k = self.density.text_k;
        let mut columns = layout::process_columns(avail_w / k);
        for col in &mut columns {
            col.x0 *= k;
            col.w *= k;
        }
        let content_w = layout::process_columns_total_w(&columns);
        self.table_content_w = content_w;
        // The columns' viewport width is the padded band (`avail_w`), NOT the
        // card's full inner width — the wheel handler clamps the horizontal
        // scroll against `content_w − table_cols_w`, so recording the padded
        // width here is what makes the final column reachable.
        self.table_cols_w = avail_w;
        // Reserve a sliver at the bottom for a horizontal scrollbar when the
        // columns overflow, so rows don't draw under it.
        let h_overflow = (content_w - avail_w).max(0.0);
        let hbar_h = if h_overflow > 0.5 { 7.0 } else { 0.0 };
        self.table_scroll_x = self.table_scroll_x.clamp(0.0, h_overflow as f32);
        let sx = f64::from(self.table_scroll_x);

        // The horizontal clip: header text and row cells are clipped to the
        // padded column band so an overflowing column (e.g. THR when narrow)
        // is hidden past the right edge instead of overlapping the border.
        let band = Rect::new(cols_left, ti.y0, cols_right, ti.y1);
        let hdr = Rect::new(ti.x0, ti.y0, ti.x1, ti.y0 + self.density.table_header_h);
        // Rows region (above any horizontal scrollbar).
        let view = Rect::new(ti.x0, hdr.y1, ti.x1, ti.y1 - hbar_h);
        let geom = TableGeom {
            columns,
            ti,
            band,
            hdr,
            view,
            cols_left,
            sx,
            hbar_h,
            content_w,
            avail_w,
            h_overflow,
        };

        self.draw_table_header(scene, t, tr, theme, &geom);
        let rows_len = self.draw_table_rows(scene, t, tr, theme, &geom);
        self.draw_table_scrollbars(scene, t, theme, &geom, rows_len);
    }

    fn draw_table_header(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        geom: &TableGeom,
    ) {
        let c = &theme.tokens.colors;
        let hdr = geom.hdr;
        paint::fill_rrect(scene, t, hdr, 0.0, c.surfaces.base);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::surface::edge_on(c.surfaces.base),
            None,
            &Line::new((hdr.x0, hdr.y1), (hdr.x1, hdr.y1)),
        );
        let hfs = ts(tr, theme.tokens.typography.scale.xs);
        self.hit.sort_headers.clear();
        let hdr_clip = Rect::new(geom.band.x0, hdr.y0, geom.band.x1, hdr.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &hdr_clip);
        for col in &geom.columns {
            let x0 = geom.cols_left + col.x0 - geom.sx;
            let active = self.sort_key == col.key;
            let color = if active { c.text_primary } else { c.text_dim };
            let mut text = col.header.to_string();
            if active {
                text.push(' ');
                text.push(if self.sort_desc { '▼' } else { '▲' });
            }
            let (lw, lh) = tr.measure_weighted(&text, hfs, None, TextWeight::Semibold);
            // Tree mode reserves a twisty gutter ahead of every NAME cell, so
            // the header label shares the cells' text edge rather than sitting
            // a gutter to their left.
            let name_gutter = if col.key == SortKey::Name && self.tree_mode {
                TWISTY_W
            } else {
                0.0
            };
            let tx = if col.right {
                x0 + col.w - f64::from(lw) - 6.0
            } else {
                x0 + 2.0 + name_gutter
            };
            tr.draw_text_weighted(
                scene,
                &text,
                hfs,
                color,
                t * Affine::translate((tx, hdr.center().y - f64::from(lh) / 2.0)),
                None,
                TextWeight::Semibold,
            );
            // Hit rect clamped to the visible band so a click maps to the
            // column under the cursor, not its off-screen extent.
            let hx0 = x0.max(geom.band.x0);
            let hx1 = (x0 + col.w).min(geom.band.x1);
            if hx1 > hx0 {
                // Half the row gap in from the column and the header band on
                // every side, so adjacent sort targets — and the first row
                // below — keep a token-step gap rather than touching.
                let g = self.density.row_gap / 2.0;
                self.hit
                    .sort_headers
                    .push((col.key, Rect::new(hx0 + g, hdr.y0 + g, hx1 - g, hdr.y1 - g)));
            }
        }
        scene.pop_layer();
    }

    /// Lay out and draw the visible rows, returning the total (unpaged) row
    /// count so the scrollbar pass can size its thumb.
    #[allow(
        clippy::too_many_lines,
        reason = "the sparse-state recovery controls share the table's clip, hit-geometry, and text context; keeping them with the normal row pass prevents two divergent table layouts"
    )]
    fn draw_table_rows(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        geom: &TableGeom,
    ) -> usize {
        self.rows_per_page = (geom.view.height() / self.density.row_h).floor().max(1.0) as usize;
        let rows = self.visible_rows();
        let max_scroll = rows.len().saturating_sub(self.rows_per_page) as f32;
        self.scroll_off = self.scroll_off.clamp(0.0, max_scroll);
        let start = self.scroll_off as usize;
        // Record the viewport so wheel routing can tell table-scroll from
        // page-scroll.
        self.table_viewport = geom.view;

        // Clip rows to the view (both axes — vertical for row scroll, and the
        // padded band horizontally so overflowing columns stay inside).
        let rows_clip = Rect::new(geom.band.x0, geom.view.y0, geom.band.x1, geom.view.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &rows_clip);
        self.hit.rows.clear();
        self.hit.twisties.clear();
        self.hit.retry_process_source = Rect::ZERO;
        if rows.is_empty() {
            let c = &theme.tokens.colors;
            let (title, body, can_retry) = if self.model.processes.is_empty() {
                match self.model.process_status {
                    ProcessDataStatus::Loading => (
                        "Loading processes",
                        "Collecting the current process list…",
                        false,
                    ),
                    ProcessDataStatus::Empty | ProcessDataStatus::Ready => (
                        "No processes are running",
                        "The operating system returned an empty process list.",
                        false,
                    ),
                    ProcessDataStatus::PermissionDenied => (
                        "Process access is restricted",
                        "Monitor could not read the system process list. Grant process-inspection permission, then retry.",
                        true,
                    ),
                    ProcessDataStatus::Unavailable => (
                        "Process data is unavailable",
                        "The operating system did not provide a process snapshot. Try again in a moment.",
                        true,
                    ),
                }
            } else {
                (
                    "No processes match this search",
                    "Clear or broaden the process filter to see more results.",
                    false,
                )
            };
            let base = ts(tr, theme.tokens.typography.scale.base);
            let sm = ts(tr, theme.tokens.typography.scale.sm);
            let (tw, _) = tr.measure_weighted(title, base, None, TextWeight::Semibold);
            let (bw, _) = tr.measure(body, sm, None);
            let cx = geom.view.center().x;
            let cy = geom.view.center().y;
            let title_top = cy - paint::line_box(tr, base) - 0.5;
            tr.draw_text_weighted(
                scene,
                title,
                base,
                c.text_primary,
                t * Affine::translate((cx - f64::from(tw) / 2.0, title_top)),
                None,
                TextWeight::Semibold,
            );
            tr.draw_text(
                scene,
                body,
                sm,
                c.text_dim,
                t * Affine::translate((cx - f64::from(bw) / 2.0, cy + 4.0)),
                None,
            );
            if can_retry {
                let button_w = 116.0;
                let button_h = 32.0;
                let button = Rect::new(
                    cx - button_w / 2.0,
                    cy + 28.0,
                    cx + button_w / 2.0,
                    cy + 28.0 + button_h,
                );
                paint::fill_rrect(scene, t, button, 7.0, c.accent[5]);
                paint::stroke_rrect(scene, t, button, 7.0, c.accent[6], 1.0);
                let icon = Rect::new(
                    button.x0 + 13.0,
                    button.center().y - 7.0,
                    button.x0 + 27.0,
                    button.center().y + 7.0,
                );
                icons::draw_icon(
                    scene,
                    t,
                    icon,
                    Icon::Refresh,
                    c.on_surface(c.accent[5]).ink,
                    1.5,
                );
                let label = "Retry refresh";
                let (_, label_h) = tr.measure_weighted(label, sm, None, TextWeight::Medium);
                tr.draw_text_weighted(
                    scene,
                    label,
                    sm,
                    c.on_surface(c.accent[5]).ink,
                    t * Affine::translate((
                        icon.x1 + 6.0,
                        button.center().y - f64::from(label_h) / 2.0,
                    )),
                    None,
                    TextWeight::Medium,
                );
                self.hit.retry_process_source = button;
            }
        }
        for i in 0..self.rows_per_page {
            let idx = start + i;
            let Some(p) = rows.get(idx) else { break };
            let y0 = geom.view.y0 + i as f64 * self.density.row_h;
            // The painted band and hit target sit inside the pitch, half the
            // row gap above and below: neighbouring rows keep a token-step gap
            // (HIG §7) while the pitch stays the paging unit.
            let inset = self.density.row_gap / 2.0;
            let row = Rect::new(
                geom.view.x0,
                y0 + inset,
                geom.view.x1,
                y0 + self.density.row_h - inset,
            );
            self.draw_table_row(
                &mut PaintCtx::new(scene, t, tr, theme),
                geom,
                p,
                row,
                idx % 2 == 1,
            );
        }
        scene.pop_layer();
        rows.len()
    }

    /// Top edge that centres one line of `size` text in `row`, measured with
    /// the renderer that paints it (its line height, not the nominal font
    /// size), so cells and their header share one centre line at every scale.
    fn row_text_top(tr: &mut TextRenderer, row: Rect, size: f32) -> f64 {
        let (_, line_h) = tr.measure("0", size, None);
        row.center().y - f64::from(line_h) / 2.0
    }

    /// Draw one process row: selection/zebra background, the NAME column's
    /// tree indent + twisty, and every other visible column's cell.
    #[allow(
        clippy::many_single_char_names,
        reason = "t/c/p/x/s/k are this file's established short names for transform/colors/row/coordinate/string/key"
    )]
    fn draw_table_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        geom: &TableGeom,
        p: &Row,
        row: Rect,
        alt: bool,
    ) {
        let PaintCtx {
            scene,
            transform: t,
            text: tr,
            theme,
        } = ctx.reborrow();
        let c = &theme.tokens.colors;
        let selected = self.selected_pid == Some(p.pid);
        if selected {
            paint::fill_rrect(
                scene,
                t,
                Rect::new(row.x0, row.y0, row.x1, row.y1),
                0.0,
                with_alpha(c.accent[5], 60),
            );
            let bar = Rect::new(row.x0, row.y0, row.x0 + 3.0, row.y1);
            scene.fill(Fill::NonZero, t, c.accent[5], None, &bar);
        } else if alt {
            paint::fill_rrect(scene, t, row, 0.0, c.neutral[2]);
        }

        let fs = ts(tr, theme.tokens.typography.scale.sm);
        let ty = Self::row_text_top(tr, row, fs);
        let txt = if selected { c.text_primary } else { c.text_dim };

        // Absolute x of a content-space column edge after the horizontal scroll.
        let col_abs = |x: f64| geom.cols_left + x - geom.sx;
        // helper to draw a cell at a content-space column.
        let cell = |tr: &mut TextRenderer,
                    scene: &mut Scene,
                    s: &str,
                    col: &layout::Column,
                    color: Color,
                    weight: TextWeight| {
            let x0 = col_abs(col.x0);
            let s = Self::truncate(tr, s, fs, col.w - 8.0);
            let (sw, _) = tr.measure_weighted(&s, fs, None, weight);
            let x = if col.right {
                x0 + col.w - f64::from(sw) - 6.0
            } else {
                x0 + 2.0
            };
            tr.draw_text_weighted(
                scene,
                &s,
                fs,
                color,
                t * Affine::translate((x, ty)),
                None,
                weight,
            );
        };
        // Look a column up by key — the set is now width-dependent (USER is
        // dropped at narrow widths), so cells are addressed by identity, not
        // a fixed index.
        let column = |k: SortKey| geom.columns.iter().find(|col| col.key == k);
        // CPU% coloured by load. A collapsed parent's summed total can exceed
        // 100% (multi-core); colour against a single core's scale but clamp
        // the hue so a hot subtree reads as fully loaded.
        let cpu_frac = (p.cpu / 100.0).clamp(0.0, 1.0);
        let cpu_col = if p.cpu >= 1.0 {
            paint::load_color(theme, cpu_frac)
        } else {
            txt
        };
        let entries = [
            (SortKey::Pid, p.pid.to_string(), txt, TextWeight::Regular),
            (SortKey::User, p.user.clone(), txt, TextWeight::Regular),
            (
                SortKey::Cpu,
                format!("{:.1}", p.cpu),
                cpu_col,
                TextWeight::Medium,
            ),
            (
                SortKey::Mem,
                format_bytes(p.mem_bytes),
                txt,
                TextWeight::Regular,
            ),
            (
                SortKey::Threads,
                p.threads.to_string(),
                txt,
                TextWeight::Regular,
            ),
        ];
        for (key, text, color, weight) in entries {
            if let Some(col) = column(key) {
                cell(tr, scene, &text, col, color, weight);
            }
        }

        // NAME column: tree indent + twisty + name, all in the NAME band.
        self.draw_table_row_name(ctx, geom, p, row);

        self.hit.rows.push((p.pid, row));
    }

    /// Draw the NAME column's tree indent + twisty + name text for one row.
    /// `layout::process_columns` always includes NAME; if a future layout
    /// change ever dropped it, this is a no-op rather than a panic — the
    /// row's other cells and hit rect still render regardless.
    fn draw_table_row_name(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        geom: &TableGeom,
        p: &Row,
        row: Rect,
    ) {
        let Some(name_col) = geom.columns.iter().find(|col| col.key == SortKey::Name) else {
            return;
        };
        let PaintCtx {
            scene,
            transform: t,
            text: tr,
            theme,
        } = ctx.reborrow();
        let selected = self.selected_pid == Some(p.pid);
        let c = &theme.tokens.colors;
        let fs = ts(tr, theme.tokens.typography.scale.sm);
        let ty = Self::row_text_top(tr, row, fs);
        let col_abs = |x: f64| geom.cols_left + x - geom.sx;
        let indent = p.depth as f64 * TREE_INDENT;
        let name_x0 = col_abs(name_col.x0) + 2.0 + indent;
        if self.tree_mode && p.has_children {
            // Expand/collapse triangle (▸ collapsed, ▾ expanded).
            let tw = Rect::new(name_x0, row.y0, name_x0 + TWISTY_W, row.y1);
            Self::draw_twisty(
                scene,
                t,
                tw,
                p.expanded,
                if selected { c.text_primary } else { c.text_dim },
            );
            // Hit rect for the twisty (clamped to the visible band).
            let hx0 = tw.x0.max(geom.band.x0);
            let hx1 = tw.x1.min(geom.band.x1);
            if hx1 > hx0 {
                self.hit
                    .twisties
                    .push((p.pid, Rect::new(hx0, row.y0, hx1, row.y1)));
            }
        }
        let text_x0 = name_x0 + if self.tree_mode { TWISTY_W } else { 0.0 };
        // Width left for the name text after the indent + twisty.
        let name_avail = (col_abs(name_col.x0) + name_col.w - text_x0 - 4.0).max(8.0);
        let label = Self::truncate(tr, &p.name, fs, name_avail);
        tr.draw_text_weighted(
            scene,
            &label,
            fs,
            c.text_primary,
            t * Affine::translate((text_x0, ty)),
            None,
            TextWeight::Medium,
        );
    }

    fn draw_table_scrollbars(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        geom: &TableGeom,
        rows_len: usize,
    ) {
        let c = &theme.tokens.colors;
        // Vertical row scrollbar — a draggable thumb over a page-able track.
        if rows_len > self.rows_per_page {
            let view = geom.view;
            let track = Rect::new(view.x1 - 8.0, view.y0, view.x1 - 1.0, view.y1);
            paint::fill_rrect(scene, t, track, 2.0, paint::inner_rule(theme, 40));
            let frac_v = self.rows_per_page as f64 / rows_len as f64;
            let thumb_h = (track.height() * frac_v).max(24.0);
            let max_off = (rows_len - self.rows_per_page) as f64;
            let pos = if max_off > 0.0 {
                f64::from(self.scroll_off) / max_off
            } else {
                0.0
            };
            let ty0 = track.y0 + (track.height() - thumb_h) * pos;
            let thumb = Rect::new(track.x0, ty0, track.x1, ty0 + thumb_h);
            paint::fill_rrect(scene, t, thumb, 2.0, with_alpha(c.text_dim, 160));
            self.hit.v_scrollbar = Some(ScrollBarHit {
                track,
                thumb,
                max_value: max_off,
            });
        } else {
            self.hit.v_scrollbar = None;
        }

        // Horizontal scrollbar — appears only when the columns overflow the
        // viewport (a narrow window), so THR no longer overlaps the border:
        // the user scrolls right to reach it instead.
        if geom.h_overflow > 0.5 {
            let track = Rect::new(
                geom.band.x0,
                geom.ti.y1 - geom.hbar_h + 1.0,
                geom.band.x1,
                geom.ti.y1 - 1.0,
            );
            paint::fill_rrect(scene, t, track, 2.0, paint::inner_rule(theme, 40));
            let frac_h = (geom.avail_w / geom.content_w).clamp(0.0, 1.0);
            let thumb_w = (track.width() * frac_h).max(24.0);
            let pos = (geom.sx / geom.h_overflow).clamp(0.0, 1.0);
            let tx0 = track.x0 + (track.width() - thumb_w) * pos;
            let thumb = Rect::new(tx0, track.y0, tx0 + thumb_w, track.y1);
            paint::fill_rrect(scene, t, thumb, 2.0, with_alpha(c.text_dim, 160));
            self.hit.h_scrollbar = Some(ScrollBarHit {
                track,
                thumb,
                max_value: geom.h_overflow,
            });
        } else {
            self.hit.h_scrollbar = None;
        }
    }

    /// Draw a small expand/collapse triangle centred in `r`. Points right (▸)
    /// when collapsed, down (▾) when expanded — the standard tree affordance.
    fn draw_twisty(scene: &mut Scene, t: Affine, r: Rect, expanded: bool, color: Color) {
        let cx = r.center().x;
        let cy = r.center().y;
        let s = 3.6; // half-extent of the glyph
        let mut path = BezPath::new();
        if expanded {
            // ▾ pointing down.
            path.move_to((cx - s, cy - s * 0.7));
            path.line_to((cx + s, cy - s * 0.7));
            path.line_to((cx, cy + s * 0.9));
        } else {
            // ▸ pointing right.
            path.move_to((cx - s * 0.7, cy - s));
            path.line_to((cx + s * 0.9, cy));
            path.line_to((cx - s * 0.7, cy + s));
        }
        path.close_path();
        scene.fill(Fill::NonZero, t, color, None, &path);
    }

    /// The narrow push detail pane: a Back affordance row above a full-area
    /// detail card. Returning Back (or Esc) slides the table back. Records the
    /// Back hit rect.
    fn draw_detail_pane(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let c = &theme.tokens.colors;
        let bar_h = 36.0;

        // Back chip: chevron + "Processes", top-left of the pushed detail pane.
        let label = "Processes";
        let fs = ts(tr, theme.tokens.typography.scale.sm);
        let (lw, lh) = tr.measure_weighted(label, fs, None, TextWeight::Semibold);
        let back_x0 = r.x0;
        let back = Rect::new(
            back_x0,
            r.y0,
            back_x0 + 34.0 + f64::from(lw) + 14.0,
            r.y0 + bar_h - 4.0,
        );
        ui::surface::card(
            scene,
            t,
            back,
            f64::from(theme.tokens.radii.md),
            c.surfaces.raised,
        );
        let chev = Rect::new(
            back.x0 + 8.0,
            back.center().y - 8.0,
            back.x0 + 24.0,
            back.center().y + 8.0,
        );
        icons::draw_icon(scene, t, chev, Icon::ChevronLeft, c.text_primary, 1.8);
        tr.draw_text_weighted(
            scene,
            label,
            fs,
            c.text_primary,
            t * Affine::translate((chev.x1 + 4.0, back.center().y - f64::from(lh) / 2.0)),
            None,
            TextWeight::Semibold,
        );
        self.hit.detail_back = back;

        // Full-area detail card below the Back bar.
        let detail = Rect::new(r.x0, r.y0 + bar_h, r.x1, r.y1);
        self.draw_detail(scene, t, tr, theme, detail);
    }

    /// Natural width of the right-hand filter-pill cluster (Tree/Flat pill,
    /// the optional Sum-mode pill, and the kernel-threads toggle) including the
    /// inter-pill gaps. The search box is sized to stop before this cluster so
    /// the two never overlap, and the toolbar stacks when even a minimum-width
    /// search box won't fit beside it.
    fn pills_cluster_width(&self, tr: &mut TextRenderer, theme: &Theme) -> f64 {
        let fs = ts(tr, theme.tokens.typography.scale.sm);
        // Kernel-threads toggle (switch knob + label).
        let mut w = f64::from(tr.measure("kernel threads", fs, None).0) + 44.0;
        // Sum-mode pill exists only in tree mode.
        if self.tree_mode {
            let sum_text = format!("Σ {}", self.sum_mode.label());
            w += TOOLBAR_PILL_GAP + f64::from(tr.measure(&sum_text, fs, None).0) + 24.0;
        }
        // Tree/Flat pill.
        let tree_text = if self.tree_mode {
            "⊟ Tree"
        } else {
            "≣ Flat"
        };
        w += TOOLBAR_PILL_GAP + f64::from(tr.measure(tree_text, fs, None).0) + 24.0;
        // Confirm-kills pill (reserved so the toolbar stacks before the pill
        // cluster crowds the search box; dropped only at extreme-narrow widths).
        w += TOOLBAR_PILL_GAP + f64::from(tr.measure("Confirm kills", fs, None).0) + 24.0;
        w
    }

    /// The toolbar stacks the search box onto its own row above the filter
    /// pills once a single row can no longer seat a minimum-width search box
    /// beside the pill cluster. Re-derived from the CURRENT width each frame,
    /// so resizing between one and two rows reflows immediately.
    fn toolbar_stacked(&self, tr: &mut TextRenderer, theme: &Theme, avail_w: f64) -> bool {
        avail_w - self.pills_cluster_width(tr, theme) - GAP < SEARCH_MIN_W
    }

    /// Width of the two process-view controls that stay together in the
    /// compact toolbar row. The kernel-thread preference gets its own row
    /// below this pair, which prevents the long labelled controls from being
    /// squeezed or painted past the left edge on phone-sized windows.
    fn primary_filter_row_width(&self, tr: &mut TextRenderer, theme: &Theme) -> f64 {
        let fs = ts(tr, theme.tokens.typography.scale.sm);
        let tree_text = if self.tree_mode {
            "⊟ Tree"
        } else {
            "≣ Flat"
        };
        let tree_w = f64::from(tr.measure(tree_text, fs, None).0) + 24.0;
        if self.tree_mode {
            let sum_text = format!("Σ {}", self.sum_mode.label());
            tree_w + TOOLBAR_PILL_GAP + f64::from(tr.measure(&sum_text, fs, None).0) + 24.0
        } else {
            tree_w
        }
    }

    fn kernel_filter_width(tr: &mut TextRenderer, theme: &Theme) -> f64 {
        f64::from(
            tr.measure(
                "kernel threads",
                ts(tr, theme.tokens.typography.scale.sm),
                None,
            )
            .0,
        ) + 44.0
    }

    fn confirm_filter_width(tr: &mut TextRenderer, theme: &Theme) -> f64 {
        f64::from(
            tr.measure(
                "Confirm kills",
                ts(tr, theme.tokens.typography.scale.sm),
                None,
            )
            .0,
        ) + 24.0
    }

    /// On very narrow windows a search row plus a single filters row is not
    /// enough: the labelled tree, aggregation and preferences would run out
    /// of the toolbar. Give the two preferences a third row instead of
    /// clipping controls or making their hit targets overlap.
    fn toolbar_compact_filters(&self, tr: &mut TextRenderer, theme: &Theme, avail_w: f64) -> bool {
        self.toolbar_stacked(tr, theme, avail_w)
            && avail_w
                < self.primary_filter_row_width(tr, theme)
                    + Self::kernel_filter_width(tr, theme)
                    + Self::confirm_filter_width(tr, theme)
                    + 2.0 * TOOLBAR_PILL_GAP
    }

    /// Height the toolbar occupies at `avail_w`: one row when the search box
    /// and pills fit side by side, two rows (search above pills) when narrow.
    fn toolbar_height(&self, tr: &mut TextRenderer, theme: &Theme, avail_w: f64) -> f64 {
        if self.toolbar_compact_filters(tr, theme, avail_w) {
            3.0 * self.density.toolbar_row_h + 2.0 * TOOLBAR_ROW_GAP
        } else if self.toolbar_stacked(tr, theme, avail_w) {
            2.0 * self.density.toolbar_row_h + TOOLBAR_ROW_GAP
        } else {
            self.density.toolbar_row_h
        }
    }

    /// Draw one centered-label toggle pill: filled/stroked capsule, accent
    /// tinted when `on`. Shared by the sum/tree/confirm-kills toolbar pills —
    /// the kernel-threads toggle has its own switch-knob visual and isn't
    /// drawn through this.
    fn draw_toggle_pill(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        on: bool,
    ) {
        let c = &theme.tokens.colors;
        let fs = ts(tr, theme.tokens.typography.scale.sm);
        let radius = rect.height() / 2.0;
        paint::fill_rrect(
            scene,
            t,
            rect,
            radius,
            if on {
                with_alpha(c.accent[5], 60)
            } else {
                c.lift(0.35)
            },
        );
        paint::stroke_rrect(
            scene,
            t,
            rect,
            radius,
            if on {
                c.accent[5]
            } else {
                ui::surface::edge_on(c.lift(0.35))
            },
            1.0,
        );
        let (lw, _) = tr.measure(label, fs, None);
        let label_top = rect.center().y - paint::line_box(tr, fs) / 2.0;
        tr.draw_text(
            scene,
            label,
            fs,
            if on { c.text_primary } else { c.text_dim },
            t * Affine::translate((rect.center().x - f64::from(lw) / 2.0, label_top)),
            None,
        );
    }

    /// Draw the kernel-threads switch pill (rightmost in `pills_row`) and
    /// return its rect, so the next pill to its left can butt against it.
    fn draw_kernel_toggle_pill(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pills_row: Rect,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let fs = ts(tr, theme.tokens.typography.scale.sm);
        let kt_label = "kernel threads";
        let ktw = f64::from(tr.measure(kt_label, fs, None).0) + 44.0;
        let kt_r = Rect::new(
            pills_row.x1 - ktw,
            pills_row.y0 + 2.0,
            pills_row.x1,
            pills_row.y1 - 2.0,
        );
        let on = self.prefs.show_kernel_threads;
        paint::fill_rrect(
            scene,
            t,
            kt_r,
            kt_r.height() / 2.0,
            if on {
                with_alpha(c.accent[5], 60)
            } else {
                c.lift(0.35)
            },
        );
        paint::stroke_rrect(
            scene,
            t,
            kt_r,
            kt_r.height() / 2.0,
            if on {
                c.accent[5]
            } else {
                ui::surface::edge_on(c.lift(0.35))
            },
            1.0,
        );
        // Keep the app-specific labeled pill, but use the shared switch face
        // for its compact state indicator.
        let knob_track = Rect::new(
            kt_r.x0 + 10.0,
            kt_r.center().y - 6.0,
            kt_r.x0 + 30.0,
            kt_r.center().y + 6.0,
        );
        Toggle::paint(
            scene,
            t,
            theme,
            &ToggleFace::new(knob_track, on).on_color(c.accent[5]),
        );
        let kt_label_top = kt_r.center().y - paint::line_box(tr, fs) / 2.0;
        tr.draw_text(
            scene,
            kt_label,
            fs,
            if on { c.text_primary } else { c.text_dim },
            t * Affine::translate((knob_track.x1 + 6.0, kt_label_top)),
            None,
        );
        self.hit.kernel_toggle = kt_r;
        kt_r
    }

    /// Draw the "Σ Sum subtree"/"Σ Per-process" pill, left of the kernel
    /// toggle. Hidden in flat mode (there's nothing to sum into when there's
    /// no tree). Returns the x1 the next pill to its left should butt against.
    fn draw_sum_mode_pill(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pills_row: Rect,
        next_x1: f64,
    ) -> f64 {
        if !self.tree_mode {
            self.hit.sum_toggle = Rect::ZERO;
            return next_x1;
        }
        let fs = ts(tr, theme.tokens.typography.scale.sm);
        let sum_label = self.sum_mode.label();
        let sum_text = format!("Σ {sum_label}");
        let sw = f64::from(tr.measure(&sum_text, fs, None).0) + 24.0;
        let sum_r = Rect::new(
            next_x1 - sw,
            pills_row.y0 + 2.0,
            next_x1,
            pills_row.y1 - 2.0,
        );
        let summing = self.sum_mode == SumMode::SumSubtree;
        Self::draw_toggle_pill(scene, t, tr, theme, sum_r, &sum_text, summing);
        self.hit.sum_toggle = sum_r;
        sum_r.x0 - TOOLBAR_PILL_GAP
    }

    /// Draw the Tree/Flat view pill, left of the sum pill. Returns its rect
    /// (the confirm-kills pill's right edge).
    fn draw_tree_flat_pill(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pills_row: Rect,
        next_x1: f64,
    ) -> Rect {
        let fs = ts(tr, theme.tokens.typography.scale.sm);
        let tree_text = if self.tree_mode {
            "⊟ Tree"
        } else {
            "≣ Flat"
        };
        let tw_ = f64::from(tr.measure(tree_text, fs, None).0) + 24.0;
        let tree_r = Rect::new(
            next_x1 - tw_,
            pills_row.y0 + 2.0,
            next_x1,
            pills_row.y1 - 2.0,
        );
        Self::draw_toggle_pill(scene, t, tr, theme, tree_r, tree_text, self.tree_mode);
        self.hit.tree_toggle = tree_r;
        tree_r
    }

    /// Draw the "Confirm kills" preference pill, left of the tree pill — the
    /// only affordance that reaches the local `confirm_kill` preference.
    /// Drawn whenever it fits within the pills row; at extreme-narrow widths
    /// (where the toolbar is already stacked) it's dropped rather than
    /// clipped off the left edge. Returns the left edge of the pill cluster,
    /// so the search box stops before it and the two can never overlap
    /// however narrow the window.
    fn draw_confirm_kills_pill(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pills_row: Rect,
        tree_r: Rect,
    ) -> f64 {
        let fs = ts(tr, theme.tokens.typography.scale.sm);
        let confirm_text = "Confirm kills";
        let cfw = f64::from(tr.measure(confirm_text, fs, None).0) + 24.0;
        let confirm_x1 = tree_r.x0 - TOOLBAR_PILL_GAP;
        let confirm_x0 = confirm_x1 - cfw;
        if confirm_x0 < pills_row.x0 {
            self.hit.confirm_toggle = Rect::ZERO;
            return tree_r.x0;
        }
        let confirm_r = Rect::new(
            confirm_x0,
            pills_row.y0 + 2.0,
            confirm_x1,
            pills_row.y1 - 2.0,
        );
        let ck = self.prefs.confirm_kill;
        Self::draw_toggle_pill(scene, t, tr, theme, confirm_r, confirm_text, ck);
        self.hit.confirm_toggle = confirm_r;
        confirm_r.x0
    }

    /// Draw the search box in `search_row`. Stacked: full width on its own
    /// row. Side-by-side: fill from the left up to the pill cluster (capped
    /// at a comfortable preferred width), so the box and the pills can never
    /// overlap however narrow the window. Returns its rect, for the count
    /// label.
    fn draw_toolbar_search_box(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        search_row: Rect,
        stacked: bool,
        cluster_left: f64,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let search_r = if stacked {
            Rect::new(search_row.x0, search_row.y0, search_row.x1, search_row.y1)
        } else {
            let max_right = cluster_left - GAP;
            let right = (search_row.x0 + SEARCH_PREF_W).min(max_right);
            Rect::new(search_row.x0, search_row.y0, right, search_row.y1)
        };
        let radius = search_r.height() / 2.0;
        paint::fill_rrect(scene, t, search_r, radius, c.surfaces.base);
        let search_focused = self.search_input.is_focused();
        let border = if search_focused {
            c.border_focus
        } else {
            ui::surface::edge_on(c.surfaces.base)
        };
        paint::stroke_rrect(
            scene,
            t,
            search_r,
            radius,
            border,
            if search_focused { 1.6 } else { 1.0 },
        );
        let icon_r = Rect::new(
            search_r.x0 + 10.0,
            search_r.center().y - 8.0,
            search_r.x0 + 26.0,
            search_r.center().y + 8.0,
        );
        icons::draw_icon(scene, t, icon_r, Icon::Search, c.text_dim, 1.6);
        let tx = icon_r.x1 + 8.0;
        self.search_input.apply_layout_external(Rect::new(
            tx,
            search_r.y0,
            search_r.x1 - 8.0,
            search_r.y1,
        ));
        let presence = PresenceCtx::empty();
        self.search_input.draw(scene, t, tr, theme, &presence);
        self.hit.search = search_r;
        search_r
    }

    /// Count label centred in the slack between the search box and the pill
    /// cluster — only drawn when it genuinely fits, so it never overlaps a
    /// pill (the caller only calls this on a single, unstacked row).
    fn draw_toolbar_count_label(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pills_row: Rect,
        search_r: Rect,
        cluster_left: f64,
    ) {
        let c = &theme.tokens.colors;
        let fs = ts(tr, theme.tokens.typography.scale.sm);
        let total = self
            .model
            .processes
            .iter()
            .filter(|p| self.prefs.show_kernel_threads || !p.kernel)
            .count();
        let shown = self.visible_rows().len();
        let count = format!("{shown} of {total} shown");
        let (cw, ch) = tr.measure(&count, fs, None);
        let cxx = f64::midpoint(search_r.x1, cluster_left) - f64::from(cw) / 2.0;
        if cluster_left - (search_r.x1 + 10.0) > f64::from(cw) + 8.0 {
            tr.draw_text(
                scene,
                &count,
                fs,
                c.text_dim,
                t * Affine::translate((
                    cxx.max(search_r.x1 + 10.0),
                    pills_row.center().y - f64::from(ch) / 2.0,
                )),
                None,
            );
        }
    }

    fn draw_toolbar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        // One row (search beside pills), two (search above filters), or three
        // (search, view controls, preferences). Deriving this from
        // `r.width()` — the width the host allocated this frame — keeps the
        // toolbar reflowing on every resize rather than freezing to whatever
        // the first frame chose.
        let stacked = self.toolbar_stacked(tr, theme, r.width());
        let compact_filters = self.toolbar_compact_filters(tr, theme, r.width());
        if compact_filters {
            let search_row = Rect::new(r.x0, r.y0, r.x1, r.y0 + self.density.toolbar_row_h);
            let view_row = Rect::new(
                r.x0,
                search_row.y1 + TOOLBAR_ROW_GAP,
                r.x1,
                search_row.y1 + TOOLBAR_ROW_GAP + self.density.toolbar_row_h,
            );
            let kernel_row = Rect::new(
                r.x0,
                view_row.y1 + TOOLBAR_ROW_GAP,
                r.x1,
                view_row.y1 + TOOLBAR_ROW_GAP + self.density.toolbar_row_h,
            );

            // The per-process view controls retain their relationship, while
            // both device-local preferences remain directly reachable below.
            let next_x1 = self.draw_sum_mode_pill(scene, t, tr, theme, view_row, view_row.x1);
            self.draw_tree_flat_pill(scene, t, tr, theme, view_row, next_x1);
            let kernel = self.draw_kernel_toggle_pill(scene, t, tr, theme, kernel_row);
            self.draw_confirm_kills_pill(scene, t, tr, theme, kernel_row, kernel);
            self.draw_toolbar_search_box(scene, t, tr, theme, search_row, true, search_row.x1);
            return;
        }
        let pills_row = if stacked {
            Rect::new(r.x0, r.y1 - self.density.toolbar_row_h, r.x1, r.y1)
        } else {
            Rect::new(r.x0, r.y0, r.x1, r.y0 + self.density.toolbar_row_h)
        };
        let search_row = if stacked {
            Rect::new(r.x0, r.y0, r.x1, r.y0 + self.density.toolbar_row_h)
        } else {
            pills_row
        };

        // ---- filter pills, laid right→left in `pills_row` -----------------
        let kt_r = self.draw_kernel_toggle_pill(scene, t, tr, theme, pills_row);
        let next_x1 =
            self.draw_sum_mode_pill(scene, t, tr, theme, pills_row, kt_r.x0 - TOOLBAR_PILL_GAP);
        let tree_r = self.draw_tree_flat_pill(scene, t, tr, theme, pills_row, next_x1);
        let cluster_left = self.draw_confirm_kills_pill(scene, t, tr, theme, pills_row, tree_r);

        // ---- search box in `search_row` -----------------------------------
        let search_r =
            self.draw_toolbar_search_box(scene, t, tr, theme, search_row, stacked, cluster_left);

        if !stacked {
            self.draw_toolbar_count_label(scene, t, tr, theme, pills_row, search_r, cluster_left);
        }
    }

    /// The per-process detail pane. Reserves a pinned action bar (Stop + Kill)
    /// at the bottom so process content can never collide with the buttons.
    /// When the pane is tall (the narrow pushed detail), fills the space with
    /// the command line, identity, a live CPU sparkline and a two-column stat
    /// grid ([`Self::draw_detail_full`]); when it's the short wide strip,
    /// keeps a compact chip row clear of the reserved buttons
    /// ([`Self::draw_detail_compact`]).
    fn draw_detail(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let c = &theme.tokens.colors;
        let inner = paint::panel(scene, t, r, theme, 12.0);
        self.hit.kill_btn = Rect::ZERO;
        self.hit.stop_btn = Rect::ZERO;
        let Some(p) = self.selected_process() else {
            let hint = "Select a process to view details and actions.";
            let sm = ts(tr, theme.tokens.typography.scale.sm);
            let (_, hint_h) = tr.measure(hint, sm, None);
            tr.draw_text(
                scene,
                hint,
                sm,
                c.text_dim,
                t * Affine::translate((inner.x0, inner.center().y - f64::from(hint_h) / 2.0)),
                None,
            );
            return;
        };
        if inner.height() > 150.0 {
            self.draw_detail_full(scene, t, tr, theme, inner, &p);
        } else {
            self.draw_detail_compact(scene, t, tr, theme, inner, &p);
        }
    }

    /// Full detail: reserve a bottom action bar, fill above it with the
    /// header, a live CPU sparkline and a two-column stat grid.
    fn draw_detail_full(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        inner: Rect,
        p: &ProcessInfo,
    ) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(inner.x0, inner.y1 - DETAIL_ACTION_H, inner.x1, inner.y1);
        let content = Rect::new(inner.x0, inner.y0, inner.x1, bar.y0 - 12.0);
        let y = Self::draw_detail_header(scene, t, tr, theme, content, p);
        let y = self.draw_detail_sparkline(scene, t, tr, theme, content, p, y);
        Self::draw_detail_stat_grid(scene, t, tr, theme, content, p, y, self.density.stat_row_h);

        // Reserved action bar: hairline top, right-aligned Stop + Kill.
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &Line::new((bar.x0, bar.y0), (bar.x1, bar.y0)),
        );
        self.draw_detail_actions(scene, t, tr, theme, bar);
    }

    /// Name + identity + command header. Returns the y position for whatever
    /// draws next.
    fn draw_detail_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        p: &ProcessInfo,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let xs = ts(tr, theme.tokens.typography.scale.xs);
        let sm = ts(tr, theme.tokens.typography.scale.sm);
        let lg = ts(tr, theme.tokens.typography.scale.lg);
        let ppid = p.ppid.map_or_else(|| "—".into(), |x| x.to_string());
        let mut y = content.y0;

        tr.draw_text_weighted(
            scene,
            &p.name,
            lg,
            c.text_primary,
            t * Affine::translate((content.x0, y)),
            None,
            TextWeight::Bold,
        );
        y += paint::line_box(tr, lg) + 4.0;
        let ident = format!("PID {}   ·   PPID {}   ·   {}", p.pid, ppid, p.user);
        tr.draw_text(
            scene,
            &ident,
            xs,
            c.text_dim,
            t * Affine::translate((content.x0, y)),
            None,
        );
        y += paint::line_box(tr, xs) + 11.5;

        tr.draw_text_weighted(
            scene,
            "COMMAND",
            xs,
            c.text_dim,
            t * Affine::translate((content.x0, y)),
            None,
            TextWeight::Semibold,
        );
        y += paint::line_box(tr, xs) + 1.5;
        let cmd = Self::truncate(tr, &p.cmd, sm, content.width());
        tr.draw_text(
            scene,
            &cmd,
            sm,
            c.text_primary,
            t * Affine::translate((content.x0, y)),
            None,
        );
        y += paint::line_box(tr, sm) + 11.0;

        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &Line::new((content.x0, y), (content.x1, y)),
        );
        y + 14.0
    }

    /// CPU label + current value over a mini area line, sized to absorb
    /// whatever vertical slack remains above the stat grid so a tall pane
    /// fills top-to-bottom cleanly instead of leaving gaps. Returns the y
    /// position for the stat grid.
    fn draw_detail_sparkline(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        p: &ProcessInfo,
        y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let xs = ts(tr, theme.tokens.typography.scale.xs);
        let base = ts(tr, theme.tokens.typography.scale.base);
        let cpu_col = paint::load_color(theme, (p.cpu / 100.0).clamp(0.0, 1.0));

        tr.draw_text_weighted(
            scene,
            // Tree rows can display subtree sums; the detail is deliberately
            // the selected process's own sample, not the aggregate.
            "CPU (PROCESS)",
            xs,
            c.text_dim,
            t * Affine::translate((content.x0, y)),
            None,
            TextWeight::Semibold,
        );
        let cpu_val = format!("{:.1}%", p.cpu);
        let (cvw, _) = tr.measure_weighted(&cpu_val, base, None, TextWeight::Bold);
        tr.draw_text_weighted(
            scene,
            &cpu_val,
            base,
            cpu_col,
            t * Affine::translate((content.x1 - f64::from(cvw), y - 2.0)),
            None,
            TextWeight::Bold,
        );
        let y = y + paint::line_box(tr, base) + 2.5;
        let grid_h = DETAIL_STAT_COUNT.div_ceil(DETAIL_STAT_COLS) as f64 * self.density.stat_row_h;
        // Sparkline fills the space between the CPU value line and the grid
        // (minus the two 14px divider gaps around it), floored so it stays
        // legible on a short pane.
        let spark_h = (content.y1 - y - grid_h - 28.0).max(46.0);
        let spark = Rect::new(content.x0, y, content.x1, y + spark_h);
        // A faint well inside the detail card: a contrast step, not a second
        // edge (HIG §9.5 — a section inside a card gets space, not a box).
        paint::fill_rrect(scene, t, spark, 6.0, paint::inner_rule(theme, 12));
        if let Some(ring) = self.proc_cpu_hist.get(&(p.pid, p.started_at)) {
            let smax = ring.max().max(100.0);
            Sparkline::draw_at(
                scene,
                t,
                layout::inner(spark, 5.0),
                ring.as_slice(),
                smax,
                cpu_col,
            );
        }
        let y = y + spark_h + 14.0;

        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &Line::new((content.x0, y), (content.x1, y)),
        );
        y + 14.0
    }

    /// The two-column MEMORY/VIRTUAL/THREADS/OPEN FILES/STATUS/RUNNING FOR
    /// stat grid, starting at `y`.
    #[allow(
        clippy::many_single_char_names,
        reason = "t/y/c/p/v are the standard graphics convention: transform, position, colour tokens, and this row's process/value"
    )]
    fn draw_detail_stat_grid(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        p: &ProcessInfo,
        y: f64,
        stat_row_h: f64,
    ) {
        let c = &theme.tokens.colors;
        let xs = ts(tr, theme.tokens.typography.scale.xs);
        let base = ts(tr, theme.tokens.typography.scale.base);
        let stats: [(&str, String, Color); DETAIL_STAT_COUNT] = [
            ("MEMORY (RSS)", format_bytes(p.mem_bytes), c.text_primary),
            ("VIRTUAL", format_bytes(p.virt_bytes), c.text_primary),
            ("THREADS", p.threads.to_string(), c.text_primary),
            (
                "OPEN FILES",
                p.open_files.map_or_else(|| "—".into(), |n| n.to_string()),
                c.text_primary,
            ),
            ("STATUS", p.status.clone(), c.semantic.info),
            ("RUNNING FOR", fmt_uptime(p.run_secs), c.text_primary),
        ];
        let col_gap = 18.0;
        let cell_w =
            (content.width() - (DETAIL_STAT_COLS as f64 - 1.0) * col_gap) / DETAIL_STAT_COLS as f64;
        for (idx, (label, value, color)) in stats.iter().enumerate() {
            let col = idx % DETAIL_STAT_COLS;
            let row = idx / DETAIL_STAT_COLS;
            let cx = content.x0 + col as f64 * (cell_w + col_gap);
            let cy = y + row as f64 * stat_row_h;
            tr.draw_text(
                scene,
                label,
                xs,
                c.text_dim,
                t * Affine::translate((cx, cy)),
                None,
            );
            let v = Self::truncate(tr, value, base, cell_w);
            let value_top = cy + paint::line_box(tr, xs) + 2.5;
            tr.draw_text_weighted(
                scene,
                &v,
                base,
                *color,
                t * Affine::translate((cx, value_top)),
                None,
                TextWeight::Semibold,
            );
        }
    }

    /// Compact strip: name + identity + a chip row clamped left of the
    /// Stop/Kill button cluster.
    fn draw_detail_compact(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        inner: Rect,
        p: &ProcessInfo,
    ) {
        let c = &theme.tokens.colors;
        let xs = ts(tr, theme.tokens.typography.scale.xs);
        let base = ts(tr, theme.tokens.typography.scale.base);
        let lg = ts(tr, theme.tokens.typography.scale.lg);
        let ppid = p.ppid.map_or_else(|| "—".into(), |x| x.to_string());
        let cpu_col = paint::load_color(theme, (p.cpu / 100.0).clamp(0.0, 1.0));

        let cluster_w = 92.0 + 8.0 + 92.0;
        let bar = Rect::new(inner.x1 - cluster_w, inner.y1 - 32.0, inner.x1, inner.y1);
        let chips_right = bar.x0 - 14.0;

        tr.draw_text_weighted(
            scene,
            &p.name,
            lg,
            c.text_primary,
            t * Affine::translate((inner.x0, inner.y0)),
            None,
            TextWeight::Bold,
        );
        let ident = format!(
            "PID {}  ·  PPID {}  ·  {}  ·  {}",
            p.pid, ppid, p.user, p.status
        );
        let ident = Self::truncate(tr, &ident, xs, chips_right - inner.x0);
        let ident_top = inner.y0 + paint::line_box(tr, lg);
        tr.draw_text(
            scene,
            &ident,
            xs,
            c.text_dim,
            t * Affine::translate((inner.x0, ident_top)),
            None,
        );

        // Chip row along the bottom, stopping before the button cluster. The
        // value line sits on the card's bottom edge and the label stacks on
        // its measured line box, so the pair stays a pair at every text scale.
        let value_top = inner.y1 - paint::line_box(tr, base) + 1.5;
        let label_top = value_top - paint::line_box(tr, xs) - 1.5;
        let chip = |scene: &mut Scene,
                    tr: &mut TextRenderer,
                    x: f64,
                    label: &str,
                    value: &str,
                    color: Color|
         -> f64 {
            if x > chips_right {
                return x;
            }
            tr.draw_text(
                scene,
                label,
                xs,
                c.text_dim,
                t * Affine::translate((x, label_top)),
                None,
            );
            tr.draw_text_weighted(
                scene,
                value,
                base,
                color,
                t * Affine::translate((x, value_top)),
                None,
                TextWeight::Bold,
            );
            let w1 = f64::from(tr.measure(label, xs, None).0);
            let w2 = f64::from(tr.measure_weighted(value, base, None, TextWeight::Bold).0);
            x + w1.max(w2) + 24.0
        };
        let chips = [
            // Keep the compact detail honest when Tree / Sum subtree makes
            // the selected table row's CPU larger than this process's sample.
            ("CPU (PROCESS)", format!("{:.1}%", p.cpu), cpu_col),
            ("RSS", format_bytes(p.mem_bytes), c.text_primary),
            ("VIRT", format_bytes(p.virt_bytes), c.text_primary),
            ("THREADS", p.threads.to_string(), c.text_primary),
            (
                "FILES",
                p.open_files.map_or_else(|| "—".into(), |n| n.to_string()),
                c.text_primary,
            ),
        ];
        let mut x = inner.x0;
        for (label, value, color) in &chips {
            x = chip(scene, tr, x, label, value.as_str(), *color);
        }

        self.draw_detail_actions(scene, t, tr, theme, bar);
    }

    /// Draw the detail pane's Stop (SIGTERM) + Kill (SIGKILL) buttons,
    /// right-aligned inside `bar`, and record their hit rects. Both fit within
    /// `bar` so they occupy only the reserved area.
    fn draw_detail_actions(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        bar: Rect,
    ) {
        let bh = (bar.height() - 8.0).clamp(26.0, 34.0);
        let by0 = bar.center().y - bh / 2.0;
        let bw = 92.0_f64.min((bar.width() - 8.0) / 2.0);
        let kill_r = Rect::new(bar.x1 - bw, by0, bar.x1, by0 + bh);
        let stop_r = Rect::new(kill_r.x0 - 8.0 - bw, by0, kill_r.x0 - 8.0, by0 + bh);
        let cur = self.last_cursor;
        Self::draw_action_button(
            scene,
            t,
            tr,
            theme,
            stop_r,
            ActionButtonSpec {
                label: "Stop",
                icon: Icon::Power,
                style: ButtonStyle::Neutral,
            },
            stop_r.contains(cur),
        );
        Self::draw_action_button(
            scene,
            t,
            tr,
            theme,
            kill_r,
            ActionButtonSpec {
                label: "Kill",
                icon: Icon::X,
                style: ButtonStyle::Danger,
            },
            kill_r.contains(cur),
        );
        self.hit.stop_btn = stop_r;
        self.hit.kill_btn = kill_r;
    }

    /// One rounded action button (the shared Button face): a centred
    /// icon-then-label pair. `Neutral` for Stop, `Danger` for the destructive
    /// Kill; hover/press feedback comes from the themed face.
    fn draw_action_button(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        spec: ActionButtonSpec,
        hovered: bool,
    ) {
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(rect, spec.label)
                .radius(f64::from(theme.tokens.radii.md))
                .text_size(ts(tr, theme.tokens.typography.scale.sm))
                .weight(TextWeight::Semibold)
                .icon(spec.icon)
                .style(spec.style)
                .hovered(hovered),
        );
    }

    /// The Resources dashboard. Top-to-bottom: an **overview row** (Processor
    /// ring, Memory gauges, System info) that sits three abreast when wide and
    /// stacks when narrow; a **full-width per-core** utilisation grid; then a
    /// **chart grid** (CPU / Memory / Network / Disk) that is two-up when wide
    /// and one column when narrow. Every band reflows with the width and the
    /// whole view page-scrolls when the content is taller than the viewport
    /// (`content_natural_h` → `resources_natural_h`).
    fn draw_resources(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let y = self.draw_resources_overview(scene, t, tr, theme, r);

        // ---- per-core utilisation grid (full width) -----------------------
        let n = self.model.metrics.cores.len().max(1);
        let grid = layout::per_core_grid(n, r.width() - 2.0 * layout::RAIL_CARD_PAD);
        let core_h = self.core_card_h(grid);
        self.draw_per_core(
            scene,
            t,
            tr,
            theme,
            Rect::new(r.x0, y, r.x1, y + core_h),
            grid,
        );

        self.draw_resources_charts(scene, t, tr, theme, r, y + core_h + GAP);
    }

    /// The overview row: Processor / Memory / System, three abreast when
    /// wide and stacked when narrow. Returns the y position below it.
    fn draw_resources_overview(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) -> f64 {
        let mut y = r.y0;
        let large_text = theme.text_scale >= 1.5;
        if layout::overview_cols(r.width()) == 3 && !large_text {
            let card_w = (r.width() - 2.0 * GAP) / 3.0;
            let h = layout::OVERVIEW_H;
            let x0 = r.x0;
            let x1 = x0 + card_w + GAP;
            let x2 = x1 + card_w + GAP;
            self.draw_processor_card(scene, t, tr, theme, Rect::new(x0, y, x0 + card_w, y + h));
            self.draw_memory_card(scene, t, tr, theme, Rect::new(x1, y, x1 + card_w, y + h));
            self.draw_system_card(scene, t, tr, theme, Rect::new(x2, y, r.x1, y + h));
            y += h + GAP;
        } else {
            let scale = f64::from(theme.text_scale);
            self.draw_processor_card(
                scene,
                t,
                tr,
                theme,
                Rect::new(r.x0, y, r.x1, y + layout::OVERVIEW_RING_H * scale),
            );
            y += layout::OVERVIEW_RING_H * scale + GAP;
            self.draw_memory_card(
                scene,
                t,
                tr,
                theme,
                Rect::new(r.x0, y, r.x1, y + layout::OVERVIEW_MEM_H * scale),
            );
            y += layout::OVERVIEW_MEM_H * scale + GAP;
            self.draw_system_card(
                scene,
                t,
                tr,
                theme,
                Rect::new(r.x0, y, r.x1, y + layout::OVERVIEW_SYS_H * scale),
            );
            y += layout::OVERVIEW_SYS_H * scale + GAP;
        }
        y
    }

    /// One rolling chart on a raised card: a semibold title, and a faint grid
    /// that stays inside the card instead of competing with its edge.
    fn area_chart(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        series: &[Series],
        max: f32,
        labels: ChartLabels<'_>,
    ) {
        let theme = ctx.theme;
        let inner = paint::panel(ctx.scene, ctx.transform, rect, theme, 12.0);
        let style = ChartStyle::default()
            .with_grid(4, Some(paint::inner_rule(theme, 26)))
            .with_title_weight(TextWeight::Semibold);
        Chart::draw_styled(
            ctx,
            inner,
            series,
            TrendRange::up_to(f64::from(max)),
            labels,
            &style,
        );
    }

    /// The chart grid: CPU / Memory / Network / Disk, two-up when wide and
    /// one column when narrow, starting at `top`. Each cell keeps a legible
    /// minimum height; when the viewport is short the page scrolls.
    fn draw_resources_charts(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
        top: f64,
    ) {
        let c = &theme.tokens.colors;
        let cols = layout::charts_cols(r.width());
        let rows = layout::charts_rows(r.width());
        let min_chart_h = 150.0;
        let avail_h = (r.y1 - top).max(rows as f64 * min_chart_h + (rows as f64 - 1.0) * GAP);
        let cw = (r.width() - (cols as f64 - 1.0) * GAP) / cols as f64;
        let ch = (avail_h - (rows as f64 - 1.0) * GAP) / rows as f64;
        let cell = |idx: usize| {
            let col = idx % cols;
            let row = idx / cols;
            let x0 = r.x0 + col as f64 * (cw + GAP);
            let y0 = top + row as f64 * (ch + GAP);
            Rect::new(x0, y0, x0 + cw, y0 + ch)
        };

        let metrics = &self.model.metrics;
        // CPU%.
        Self::area_chart(
            &mut PaintCtx::new(scene, t, tr, theme),
            cell(0),
            &[
                Series::new(metrics.cpu_hist.as_slice().to_vec(), c.accent[5])
                    .with_legend(format!("{:.0}%", metrics.cpu_overall())),
            ],
            100.0,
            ChartLabels {
                title: "CPU",
                axis_hi: "100%",
                axis_lo: "0%",
            },
        );
        // Memory%.
        Self::area_chart(
            &mut PaintCtx::new(scene, t, tr, theme),
            cell(1),
            &[
                Series::new(metrics.mem_hist.as_slice().to_vec(), c.semantic.success)
                    .with_legend(format_bytes(metrics.mem_used)),
            ],
            1.0,
            ChartLabels {
                title: "Memory",
                axis_hi: "100%",
                axis_lo: "0%",
            },
        );
        self.draw_resources_net_disk_charts(scene, t, tr, theme, cell(2), cell(3));
    }

    /// The Network (rx/tx) and Disk I/O (read/write) charts.
    fn draw_resources_net_disk_charts(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        net_cell: Rect,
        disk_cell: Rect,
    ) {
        let c = &theme.tokens.colors;
        let metrics = &self.model.metrics;
        let net_max = metrics
            .net_rx_hist
            .max()
            .max(metrics.net_tx_hist.max())
            .max(1.0)
            * 1.2;
        Self::area_chart(
            &mut PaintCtx::new(scene, t, tr, theme),
            net_cell,
            &[
                Series::new(metrics.net_rx_hist.as_slice().to_vec(), c.semantic.info)
                    .with_legend(format!("↓ {}", fmt_rate(metrics.net_rx))),
                Series::new(metrics.net_tx_hist.as_slice().to_vec(), c.accent[6])
                    .with_legend(format!("↑ {}", fmt_rate(metrics.net_tx))),
            ],
            net_max,
            ChartLabels {
                title: "Network",
                axis_hi: &fmt_rate(net_max),
                axis_lo: "0",
            },
        );
        let disk_max = metrics
            .disk_read_hist
            .max()
            .max(metrics.disk_write_hist.max())
            .max(1.0)
            * 1.2;
        Self::area_chart(
            &mut PaintCtx::new(scene, t, tr, theme),
            disk_cell,
            &[
                Series::new(
                    metrics.disk_read_hist.as_slice().to_vec(),
                    c.semantic.warning,
                )
                .with_legend(format!("R {}", fmt_rate(metrics.disk_read))),
                Series::new(
                    metrics.disk_write_hist.as_slice().to_vec(),
                    c.semantic.danger,
                )
                .with_legend(format!("W {}", fmt_rate(metrics.disk_write))),
            ],
            disk_max,
            ChartLabels {
                title: "Disk I/O",
                axis_hi: &fmt_rate(disk_max),
                axis_lo: "0",
            },
        );
    }

    /// The title, one-line body, and affirmative-button label for a pending
    /// stop confirmation. Shared by the dialog builder so the copy lives in one
    /// place.
    fn confirm_copy(&self, pid: u32, kind: StopKind) -> (&'static str, String, &'static str) {
        let name = self
            .model
            .processes
            .iter()
            .find(|p| p.pid == pid)
            .map_or_else(|| "process".into(), |p| p.name.clone());
        let (title, signal_name, confirm_label) = match kind {
            StopKind::Term => ("Stop process?", "SIGTERM", "Stop"),
            StopKind::Kill => ("Kill process?", "SIGKILL", "Kill"),
        };
        let body =
            format!("{name} (PID {pid}) will be sent {signal_name}. Unsaved work will be lost.");
        (title, body, confirm_label)
    }

    /// Build the shared confirm dialog for a pending stop. The affirmative
    /// button is destructive (danger-coloured); `confirm` / `cancel` cells are
    /// flipped by the dialog's action callbacks and drained by the host.
    fn build_confirm_dialog(&self, pid: u32, kind: StopKind) -> ConfirmDialog {
        let (title, body, confirm_label) = self.confirm_copy(pid, kind);
        let confirm = Rc::new(Cell::new(false));
        let cancel = Rc::new(Cell::new(false));
        let dialog = ModalDialog::new(title, Text::new(&body).with_size(13.5))
            .open(true)
            .with_max_size(420.0, 190.0)
            .with_destructive(true)
            .primary_action(confirm_label, {
                let c = confirm.clone();
                move || c.set(true)
            })
            .secondary_action("Cancel", {
                let c = cancel.clone();
                move || c.set(true)
            })
            .on_dismiss({
                let c = cancel.clone();
                move || c.set(true)
            });
        ConfirmDialog {
            dialog,
            confirm,
            cancel,
        }
    }

    /// Ensure the confirm dialog exists while a stop is pending.
    fn ensure_confirm_dialog(&mut self, pid: u32, kind: StopKind) {
        if self.confirm_dialog.is_none() {
            self.confirm_dialog = Some(self.build_confirm_dialog(pid, kind));
        }
    }

    /// Lay the confirm dialog out against the app rect so its box + footer
    /// buttons resolve for hit-testing and paint.
    fn layout_confirm_dialog(&mut self) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let Some(cd) = self.confirm_dialog.as_mut() else {
            return;
        };
        let theme = Theme::dark();
        let mut text_renderer = TextRenderer::new();
        cd.dialog.layout_in(self.rect, &theme, &mut text_renderer);
    }

    /// Route an event into the open confirm dialog and act on the flags its
    /// callbacks set. Enter is a submit convenience; the dialog itself owns
    /// Escape, the backdrop click, and the footer buttons.
    fn route_confirm_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        let Some((pid, started_at, kind)) = self.confirm_stop else {
            return;
        };
        if let WindowEvent::KeyboardInput { event: ke, .. } = event
            && ke.state == ElementState::Pressed
            && matches!(ke.logical_key, Key::Named(NamedKey::Enter))
        {
            let operation = MonitorOperation::Signal {
                pid,
                started_at,
                signal: match kind {
                    StopKind::Term => MonitorSignal::Stop,
                    StopKind::Kill => MonitorSignal::Kill,
                },
            };
            let _ = self.perform_operation(&operation, SignalConfirmation::ActionContract);
            *handled = true;
            return;
        }
        self.ensure_confirm_dialog(pid, kind);
        self.layout_confirm_dialog();
        let (confirm, cancel) = {
            let Some(cd) = self.confirm_dialog.as_mut() else {
                return;
            };
            *handled = cd
                .dialog
                .handle_detached(event, scale_factor, modifiers, doc, path);
            (cd.confirm.replace(false), cd.cancel.replace(false))
        };
        if confirm {
            let operation = MonitorOperation::Signal {
                pid,
                started_at,
                signal: match kind {
                    StopKind::Term => MonitorSignal::Stop,
                    StopKind::Kill => MonitorSignal::Kill,
                },
            };
            let _ = self.perform_operation(&operation, SignalConfirmation::ActionContract);
        } else if cancel {
            self.confirm_stop = None;
            self.confirm_dialog = None;
        }
        *handled = true;
    }

    /// The transient "PID N copied" toast — a small pill centred near the
    /// bottom of the window for a short window after a Copy PID action, then
    /// it expires on its own (the redraw loop keeps ticking). Real transient
    /// feedback, not text jammed into the toolbar count label.
    fn draw_toast(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        if !self.copy_toast_active() {
            return;
        }
        let Some(pid) = self.copied_pid else { return };
        let label = format!("PID {pid} copied to clipboard");
        paint_transient(
            scene,
            t,
            tr,
            theme,
            self.rect,
            ToastAnchor::BottomCenter,
            &ToastCard::new(&label, ToastKind::Success),
        );
    }
}

// ==========================================================================
// Event handling
// ==========================================================================
impl MonitorApp {
    /// If `p` lands on a scrollbar, either begin a thumb drag (press on the
    /// thumb) or page toward the click (press on the track). Returns whether it
    /// consumed the press. Checked before row/summary hit-testing so dragging
    /// the row scrollbar never selects the row it overlaps.
    fn begin_scroll_interaction(&mut self, p: Point) -> bool {
        // Page bar (Resources whole-view). Present only on the Resources tab.
        if let Some(sb) = self.hit.page_scrollbar
            && sb.track.contains(p)
        {
            if sb.thumb.contains(p) {
                self.scroll_drag = Some(ScrollDrag::Page {
                    grab: p.y - sb.thumb.y0,
                });
            } else {
                let dir = if p.y < sb.thumb.y0 { -1.0 } else { 1.0 };
                let page = (self.viewport_h * 0.9) as f32;
                let max_y = sb.max_value as f32;
                self.view_scroll_y = (self.view_scroll_y + dir * page).clamp(0.0, max_y);
            }
            return true;
        }
        // Vertical row bar. Present only on the Processes tab.
        if let Some(sb) = self.hit.v_scrollbar
            && sb.track.contains(p)
        {
            if sb.thumb.contains(p) {
                self.scroll_drag = Some(ScrollDrag::Rows {
                    grab: p.y - sb.thumb.y0,
                });
            } else {
                let dir = if p.y < sb.thumb.y0 { -1.0 } else { 1.0 };
                self.scroll_off = (self.scroll_off + dir * self.rows_per_page as f32).max(0.0);
                self.clamp_scroll();
            }
            return true;
        }
        // Horizontal column bar.
        if let Some(sb) = self.hit.h_scrollbar
            && sb.track.contains(p)
        {
            if sb.thumb.contains(p) {
                self.scroll_drag = Some(ScrollDrag::Cols {
                    grab: p.x - sb.thumb.x0,
                });
            } else {
                let dir = if p.x < sb.thumb.x0 { -1.0 } else { 1.0 };
                let max_x = sb.max_value as f32;
                self.table_scroll_x =
                    (self.table_scroll_x + dir * self.table_cols_w as f32).clamp(0.0, max_x);
            }
            return true;
        }
        false
    }

    /// Advance an in-progress scrollbar drag to the current cursor position,
    /// mapping the thumb's grabbed point to a scroll value along the bar axis.
    fn apply_scroll_drag(&mut self, p: Point) {
        let Some(drag) = self.scroll_drag else { return };
        match drag {
            ScrollDrag::Rows { grab } => {
                if let Some(sb) = self.hit.v_scrollbar {
                    let travel = (sb.track.height() - sb.thumb.height()).max(1.0);
                    let frac = ((p.y - grab - sb.track.y0) / travel).clamp(0.0, 1.0);
                    self.scroll_off = (frac * sb.max_value) as f32;
                    self.clamp_scroll();
                }
            }
            ScrollDrag::Cols { grab } => {
                if let Some(sb) = self.hit.h_scrollbar {
                    let travel = (sb.track.width() - sb.thumb.width()).max(1.0);
                    let frac = ((p.x - grab - sb.track.x0) / travel).clamp(0.0, 1.0);
                    let max_x = (self.table_content_w - self.table_cols_w).max(0.0) as f32;
                    self.table_scroll_x = ((frac * sb.max_value) as f32).clamp(0.0, max_x);
                }
            }
            ScrollDrag::Page { grab } => {
                if let Some(sb) = self.hit.page_scrollbar {
                    let travel = (sb.track.height() - sb.thumb.height()).max(1.0);
                    let frac = ((p.y - grab - sb.track.y0) / travel).clamp(0.0, 1.0);
                    let max_y = (self.content_h - self.viewport_h).max(0.0) as f32;
                    self.view_scroll_y = ((frac * sb.max_value) as f32).clamp(0.0, max_y);
                }
            }
        }
    }

    fn on_click(&mut self, p: Point, doc: &mut AutoCommit, path: &ObjId, handled: &mut bool) {
        // The stop-confirmation modal is handled in `handle_event` before
        // dispatch reaches here (the shared dialog captures all input while
        // open), so `on_click` never runs with a confirm pending.
        if self.click_interval_popover(p, doc, path) {
            *handled = true;
            return;
        }
        if self.click_context_menu(p, doc, path) {
            *handled = true;
            return;
        }
        // Scrollbar thumbs/tracks take precedence over the content beneath them,
        // so a press on the row scrollbar drags/pages instead of selecting the
        // row it overlaps.
        if self.begin_scroll_interaction(p) {
            *handled = true;
            return;
        }

        // Lose search focus unless the search box itself is clicked.
        let clicked_search = self.hit.search.contains(p);
        self.search_input.set_focus(clicked_search);

        if self.click_nav_chrome(p, doc, path) {
            *handled = true;
            return;
        }
        if self.tab == Tab::Processes && self.click_process_table(p, clicked_search, doc, path) {
            *handled = true;
        }
    }

    /// An open interval popover captures clicks: a press on an option row
    /// selects that cadence, a press anywhere else dismisses. Returns `true`
    /// (always) when the popover was open — either way the click is consumed
    /// so it doesn't fall through to the content.
    fn click_interval_popover(&mut self, p: Point, doc: &mut AutoCommit, path: &ObjId) -> bool {
        if !self.interval_popover.is_open() {
            return false;
        }
        if let Some(idx) = self.interval_popover.item_at(p) {
            self.interval_popover.select(idx);
            self.apply_interval_selection(doc, path);
        } else {
            self.interval_popover.close();
        }
        true
    }

    /// An open context menu floats above the rest of the content. A click on
    /// one of its action rows dispatches the action (and the action's
    /// callback records a `ProcAction` we then drain + apply); a click
    /// anywhere else dismisses it. Returns `true` (always) when the menu was
    /// open — either way the click is consumed.
    fn click_context_menu(&mut self, p: Point, doc: &mut AutoCommit, path: &ObjId) -> bool {
        if !self.context.is_open() {
            return false;
        }
        let mut hit_idx = None;
        for (idx, rect) in self.context_menu_item_rects() {
            if rect.contains(p) {
                hit_idx = Some(idx);
                break;
            }
        }
        if let Some(idx) = hit_idx {
            self.activate_context_item(idx, doc, path);
        }
        self.close_context_menu();
        true
    }

    /// Route a context-menu choice through the same callback sink used by a
    /// pointer click. This small shared path lets accessibility invoke the
    /// menu without re-implementing (and eventually drifting from) its action
    /// mapping.
    fn activate_context_item(&mut self, idx: usize, doc: &mut AutoCommit, path: &ObjId) {
        self.context.click_item(idx); // fires the callback → pending_action
        let action = self.pending_action.lock().ok().and_then(|mut g| g.take());
        if let Some(action) = action {
            self.apply_action(action, doc, path);
        }
    }

    /// Non-tab-specific chrome: the detail-pane Back affordance, a
    /// summary-strip cell jump, the tab pills, and starting the interval
    /// pill's press-and-hold timer. Returns whether the click was consumed.
    fn click_nav_chrome(&mut self, p: Point, doc: &mut AutoCommit, _path: &ObjId) -> bool {
        // Back affordance in the narrow pushed detail pane → return to table.
        if self.hit.detail_back.contains(p) {
            self.proc_push.show_master();
            return true;
        }

        // A click on a summary-strip cell (Processes) jumps to the Resources
        // dashboard AND scrolls that stat's own chart into view — so the four
        // cells land on four different charts rather than all dumping the user
        // at the top.
        for (i, cell) in self.hit.summary_cells.clone().into_iter().enumerate() {
            if cell.contains(p) {
                self.commit_tab(doc, Tab::Resources);
                let (body_w, viewport_h) = self.body_metrics();
                self.view_scroll_y = self.resources_scroll_for_stat(i, body_w, viewport_h);
                return true;
            }
        }

        if self.hit.tab_proc.contains(p) {
            if self.tab != Tab::Processes {
                self.view_scroll_y = 0.0;
            }
            self.commit_tab(doc, Tab::Processes);
            return true;
        }
        if self.hit.tab_res.contains(p) {
            if self.tab != Tab::Resources {
                self.view_scroll_y = 0.0;
            }
            self.commit_tab(doc, Tab::Resources);
            return true;
        }
        if self.hit.refresh.contains(p) {
            self.request_monitor_control("refresh");
            return true;
        }
        if self.hit.auto_refresh.contains(p) {
            self.prefs.auto_refresh = !self.prefs.auto_refresh;
            // Resuming starts a fresh cadence window. That avoids an immediate
            // surprise sample after the user intentionally paused inspection.
            if self.prefs.auto_refresh {
                self.last_refresh = ui::time::now();
            }
            return true;
        }
        if self.hit.interval.contains(p) {
            // The interval pill distinguishes a quick click (cycle) from a
            // press-and-hold (open the option popover). Start the hold timer on
            // press; `draw` opens the popover once it crosses the threshold, and
            // `release_interval_press` cycles on an early release.
            self.interval_press = Some(ui::time::now());
            self.interval_hold_opened = false;
            return true;
        }
        if self.hit.retry_process_source.contains(p) {
            self.request_monitor_control("refresh");
            return true;
        }
        false
    }

    /// Click targets specific to the Processes tab, tried once search focus
    /// is already resolved: kernel/confirm/tree/sum toggles, sort headers,
    /// the Stop/Kill buttons, twisties and rows. Returns whether the click
    /// was consumed.
    fn click_process_table(
        &mut self,
        p: Point,
        clicked_search: bool,
        doc: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        if clicked_search {
            return true;
        }
        if self.hit.kernel_toggle.contains(p) {
            self.prefs.show_kernel_threads = !self.prefs.show_kernel_threads;
            self.clear_selection_if_filtered_out();
            self.clamp_scroll();
            return true;
        }
        // Confirm-kills preference toggle (local to this window/device).
        if self.hit.confirm_toggle.contains(p) {
            self.prefs.confirm_kill = !self.prefs.confirm_kill;
            return true;
        }
        // Tree/Flat view toggle.
        if self.hit.tree_toggle.contains(p) {
            self.commit_tree_mode(doc, !self.tree_mode);
            return true;
        }
        // Sum subtree / Per-process toggle (tree mode only).
        if self.tree_mode && self.hit.sum_toggle.contains(p) {
            self.sum_mode = self.sum_mode.toggled();
            return true;
        }
        for (key, rect) in self.hit.sort_headers.clone() {
            if rect.contains(p) {
                self.set_sort(doc, key);
                return true;
            }
        }
        if self.hit.kill_btn.contains(p) {
            if let Some(pid) = self.selected_pid {
                self.request_process_control(pid, "kill");
            }
            return true;
        }
        if self.hit.stop_btn.contains(p) {
            if let Some(pid) = self.selected_pid {
                self.request_process_control(pid, "stop");
            }
            return true;
        }
        // Expand/collapse triangles take precedence over the row body, so
        // clicking a twisty toggles the subtree without selecting/pushing.
        for (pid, rect) in self.hit.twisties.clone() {
            if rect.contains(p) {
                self.toggle_expand(pid);
                return true;
            }
        }
        for (pid, rect) in self.hit.rows.clone() {
            if rect.contains(p) {
                self.commit_selected(doc, Some(pid));
                // On narrow windows the detail is its own pushed pane;
                // selecting a row slides it in.
                if self.is_narrow() {
                    self.proc_push.show_detail();
                }
                return true;
            }
        }
        false
    }

    /// A right-click (secondary button) over a process row opens the action
    /// context menu anchored at the cursor. Right-clicking empty space (or
    /// off the Processes tab) is ignored. Returns whether it was consumed.
    fn on_right_click(
        &mut self,
        p: Point,
        _doc: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
    ) {
        // A right-click anywhere first dismisses an open menu.
        if self.context.is_open() {
            self.close_context_menu();
        }
        if self.tab != Tab::Processes {
            return;
        }
        for (pid, rect) in self.hit.rows.clone() {
            if rect.contains(p) {
                self.commit_selected(_doc, Some(pid));
                self.open_context_menu(pid, p);
                *handled = true;
                return;
            }
        }
    }

    fn move_selection(&mut self, doc: &mut AutoCommit, delta: i32) {
        let procs = self.visible_rows();
        if procs.is_empty() {
            return;
        }
        let cur = self
            .selected_pid
            .and_then(|pid| procs.iter().position(|p| p.pid == pid));
        let next = match cur {
            Some(i) => (i as i32 + delta).clamp(0, procs.len() as i32 - 1) as usize,
            None => {
                if delta >= 0 {
                    0
                } else {
                    procs.len() - 1
                }
            }
        };
        let Some(next_row) = procs.get(next) else {
            return;
        };
        self.commit_selected(doc, Some(next_row.pid));
        // Keep the selection in view.
        let next_f = next as f32;
        if next_f < self.scroll_off {
            self.scroll_off = next_f;
        } else if next_f >= self.scroll_off + self.rows_per_page as f32 {
            self.scroll_off = next_f - self.rows_per_page as f32 + 1.0;
        }
        self.clamp_scroll();
    }

    fn on_key(
        &mut self,
        key: &Key,
        _text: Option<&str>,
        ctrl: bool,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        // Global-search seam: leave Ctrl+K (and Ctrl+/) untouched so the
        // forthcoming shared global-search framework can claim it. We do NOT
        // build a bespoke search here — just refuse to swallow the chord so
        // it bubbles up unhandled. (Bare "/" still focuses the local process
        // filter below; only the Ctrl-modified chord is reserved.)
        if ctrl
            && let Key::Character(s) = key
            && matches!(s.as_str(), "k" | "K" | "/")
        {
            // `handled` stays false → event propagates to the host.
            return;
        }

        if self.key_interval_popover(key, doc, path) {
            *handled = true;
            return;
        }
        if self.key_context_menu(key) {
            *handled = true;
            return;
        }
        // The stop-confirmation modal is handled in `handle_event` before
        // dispatch reaches here, so `on_key` never runs with a confirm pending.
        self.key_global(key, doc, path, handled);
    }

    /// Interval popover open: full keyboard nav (↑/↓/Home/End move, Enter
    /// selects, Esc closes). Every key is swallowed so none leaks to the
    /// table behind the overlay — the primitive's focus-capture contract.
    /// Returns `true` (always) when the popover was open.
    fn key_interval_popover(&mut self, key: &Key, doc: &mut AutoCommit, path: &ObjId) -> bool {
        if !self.interval_popover.is_open() {
            return false;
        }
        match key {
            Key::Named(NamedKey::ArrowDown) => self.interval_popover.move_highlight(1),
            Key::Named(NamedKey::ArrowUp) => self.interval_popover.move_highlight(-1),
            Key::Named(NamedKey::Home) => self.interval_popover.highlight_first(),
            Key::Named(NamedKey::End) => self.interval_popover.highlight_last(),
            Key::Named(NamedKey::Enter) => {
                self.interval_popover.select_highlighted();
                self.apply_interval_selection(doc, path);
            }
            Key::Named(NamedKey::Escape) => self.interval_popover.close(),
            _ => {}
        }
        true
    }

    /// Context menu open: Esc dismisses it, everything else is swallowed.
    /// Returns `true` (always) when the menu was open.
    fn key_context_menu(&mut self, key: &Key) -> bool {
        if !self.context.is_open() {
            return false;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.close_context_menu();
        }
        true
    }

    /// Global keys: Esc, selection nav, tree expand/collapse, Kill, and
    /// single-character shortcuts.
    fn key_global(&mut self, key: &Key, doc: &mut AutoCommit, path: &ObjId, handled: &mut bool) {
        match key {
            // Esc pops a pushed detail pane back to the table (narrow).
            Key::Named(NamedKey::Escape) => {
                if self.is_narrow() && self.proc_push.pane() == PushPane::Detail {
                    self.proc_push.show_master();
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowUp) if self.tab == Tab::Processes => {
                self.move_selection(doc, -1);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) if self.tab == Tab::Processes => {
                self.move_selection(doc, 1);
                *handled = true;
            }
            // Right/Left expand/collapse the selected tree node (when it has
            // children); a no-op in flat mode.
            Key::Named(NamedKey::ArrowRight) if self.tab == Tab::Processes && self.tree_mode => {
                if let Some(pid) = self.selected_pid {
                    if self.row_has_children(pid) && self.expanded.insert(pid) {
                        self.clamp_scroll();
                    }
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowLeft) if self.tab == Tab::Processes && self.tree_mode => {
                if let Some(pid) = self.selected_pid {
                    if self.expanded.remove(&pid) {
                        self.clear_selection_if_filtered_out();
                        self.clamp_scroll();
                    }
                    *handled = true;
                }
            }
            // Enter and Delete both kill the selected process.
            Key::Named(NamedKey::Enter | NamedKey::Delete) if self.tab == Tab::Processes => {
                if let Some(pid) = self.selected_pid {
                    self.request_process_control(pid, "kill");
                    *handled = true;
                }
            }
            Key::Character(s) => {
                *handled = self.key_character_shortcut(s.as_str(), doc, path);
            }
            _ => {}
        }
    }

    /// Single-character keyboard shortcuts: tab switch, search focus, sort
    /// keys, Stop, and the tree/sum toggles. Returns whether the character
    /// was recognised.
    fn key_character_shortcut(&mut self, s: &str, doc: &mut AutoCommit, _path: &ObjId) -> bool {
        match s {
            "1" => self.commit_tab(doc, Tab::Processes),
            "2" => self.commit_tab(doc, Tab::Resources),
            "/" => {
                self.commit_tab(doc, Tab::Processes);
                self.search_input.set_focus(true);
            }
            "c" | "C" => self.set_sort(doc, SortKey::Cpu),
            "m" | "M" => self.set_sort(doc, SortKey::Mem),
            "p" | "P" => self.set_sort(doc, SortKey::Pid),
            "n" | "N" => self.set_sort(doc, SortKey::Name),
            "r" | "R" => self.request_monitor_control("refresh"),
            "a" | "A" => {
                self.prefs.auto_refresh = !self.prefs.auto_refresh;
                if self.prefs.auto_refresh {
                    self.last_refresh = ui::time::now();
                }
            }
            // "s" sends SIGTERM (Stop) to the selection; "g" toggles the
            // subtree-sum mode; "t" toggles tree/flat.
            "s" | "S" => {
                if self.tab == Tab::Processes
                    && let Some(pid) = self.selected_pid
                {
                    self.request_process_control(pid, "stop");
                }
            }
            "g" | "G" => {
                if self.tree_mode {
                    self.sum_mode = self.sum_mode.toggled();
                }
            }
            "t" | "T" => self.commit_tree_mode(doc, !self.tree_mode),
            _ => return false,
        }
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the hand-painted Monitor surface deliberately keeps every semantic node beside its matching hit geometry"
    )]
    fn accessibility_targets(&self) -> Vec<(String, Rect, ui::a11y::AccessNode)> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let button = |target: &str, rect: Rect, label: &str| {
            (
                target.to_string(),
                rect,
                AccessNode::new(Role::Button)
                    .with_name(label)
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(rect),
            )
        };
        let mut targets = vec![
            (
                "monitor.tab.processes".to_string(),
                self.hit.tab_proc,
                AccessNode::new(Role::Tab)
                    .with_name("Processes")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.tab == Tab::Processes),
                    )
                    .with_bounds(self.hit.tab_proc),
            ),
            (
                "monitor.tab.resources".to_string(),
                self.hit.tab_res,
                AccessNode::new(Role::Tab)
                    .with_name("Resources")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.tab == Tab::Resources),
                    )
                    .with_bounds(self.hit.tab_res),
            ),
            (
                "monitor.interval".to_string(),
                self.hit.interval,
                AccessNode::new(Role::ComboBox)
                    .with_name("Refresh interval")
                    .with_value(interval_label(self.prefs.interval_ms))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .expanded(self.interval_popover.is_open()),
                    )
                    .with_bounds(self.hit.interval),
            ),
            (
                "monitor.refresh".to_string(),
                self.hit.refresh,
                AccessNode::new(Role::Button)
                    .with_name("Refresh system metrics and processes")
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(self.hit.refresh),
            ),
            (
                "monitor.auto-refresh".to_string(),
                self.hit.auto_refresh,
                AccessNode::new(Role::ToggleButton)
                    .with_name("Automatic refresh")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(self.prefs.auto_refresh),
                    )
                    .with_bounds(self.hit.auto_refresh),
            ),
            (
                "monitor.kernel-threads".to_string(),
                self.hit.kernel_toggle,
                AccessNode::new(Role::Switch)
                    .with_name("Show kernel processes")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(self.prefs.show_kernel_threads),
                    )
                    .with_bounds(self.hit.kernel_toggle),
            ),
            (
                "monitor.tree-mode".to_string(),
                self.hit.tree_toggle,
                AccessNode::new(Role::ToggleButton)
                    .with_name("Tree process view")
                    .with_state(AccessState::new().focusable(true).checked(self.tree_mode))
                    .with_bounds(self.hit.tree_toggle),
            ),
            (
                "monitor.sum-mode".to_string(),
                self.hit.sum_toggle,
                AccessNode::new(Role::ToggleButton)
                    .with_name("Sum subtree totals")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(self.sum_mode == SumMode::SumSubtree),
                    )
                    .with_bounds(self.hit.sum_toggle),
            ),
        ];
        if self.hit.retry_process_source.area() > 0.0 {
            targets.push(button(
                "monitor.process-source.retry",
                self.hit.retry_process_source,
                "Retry loading the process list",
            ));
        }
        if self.interval_popover.is_open() {
            for (index, ms) in INTERVAL_OPTIONS.iter().copied().enumerate() {
                // The reusable menu owns its own scale-aware row geometry.
                // Reusing it here keeps semantic hit bounds aligned with the
                // painted/clickable rows when Popover's spacing evolves.
                let Some(row) = self.interval_popover.item_rect(index) else {
                    continue;
                };
                targets.push((
                    format!("monitor.interval:{ms}"),
                    row,
                    AccessNode::new(Role::RadioButton)
                        .with_name(interval_label(ms))
                        .with_semantic_key(format!("interval:{ms}"))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .checked(self.prefs.interval_ms == ms),
                        )
                        .with_bounds(row),
                ));
            }
        }
        if self.hit.confirm_toggle.area() > 0.0 {
            targets.push((
                "monitor.confirm-kills".to_string(),
                self.hit.confirm_toggle,
                AccessNode::new(Role::ToggleButton)
                    .with_name("Confirm force kills")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(self.prefs.confirm_kill),
                    )
                    .with_bounds(self.hit.confirm_toggle),
            ));
        }
        if self.hit.search.area() > 0.0 {
            let search = self
                .search_input
                .accessibility()
                .unwrap_or_else(|| AccessNode::new(Role::TextInput).with_name("Search processes"))
                .with_bounds(self.hit.search);
            targets.push(("monitor.search".to_string(), self.hit.search, search));
        }
        for (sort, rect) in &self.hit.sort_headers {
            let label = match sort {
                SortKey::Pid => "PID",
                SortKey::Name => "name",
                SortKey::User => "user",
                SortKey::Cpu => "CPU",
                SortKey::Mem => "memory",
                SortKey::Threads => "threads",
            };
            targets.push(button(
                &format!("monitor.sort.{label}"),
                *rect,
                &format!("Sort by {label}"),
            ));
        }
        // The four process-summary cards are navigation controls, not merely
        // decoration: each lands on its matching Resources chart. Expose the
        // same destinations to screen readers and semantic drivers.
        for (index, rect) in self.hit.summary_cells.iter().enumerate() {
            let label = ["CPU", "Memory", "Network", "Disk"]
                .get(index)
                .copied()
                .unwrap_or("system metric");
            targets.push(button(
                &format!("monitor.summary.{index}"),
                *rect,
                &format!("Open {label} resources"),
            ));
        }
        if self.hit.detail_back.area() > 0.0 {
            targets.push(button(
                "monitor.detail-back",
                self.hit.detail_back,
                "Back to processes",
            ));
        }
        for (pid, rect) in &self.hit.rows {
            let process = self
                .model
                .processes
                .iter()
                .find(|process| process.pid == *pid)
                .cloned();
            let (name, started_at) = process.map_or_else(
                || (format!("Process {pid}"), 0),
                |process| (format!("{} — PID {pid}", process.name), process.started_at),
            );
            targets.push((
                monitor_process_target(*pid, started_at),
                *rect,
                AccessNode::new(Role::ListItem)
                    .with_name(name)
                    .with_semantic_key(process_resource_id(*pid, started_at))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.selected_pid == Some(*pid)),
                    )
                    .with_bounds(*rect),
            ));
        }
        for (pid, rect) in &self.hit.twisties {
            if let Some(process) = self
                .model
                .processes
                .iter()
                .find(|process| process.pid == *pid)
            {
                let expanded = self.expanded.contains(pid);
                targets.push((
                    format!(
                        "monitor.tree-toggle:{}",
                        process_resource_id(*pid, process.started_at)
                    ),
                    *rect,
                    AccessNode::new(Role::Button)
                        .with_name(format!(
                            "{} {}",
                            if expanded { "Collapse" } else { "Expand" },
                            process.name
                        ))
                        .with_semantic_key(format!(
                            "tree-toggle:{}",
                            process_resource_id(*pid, process.started_at)
                        ))
                        .with_state(AccessState::new().focusable(true).expanded(expanded))
                        .with_bounds(*rect),
                ));
            }
        }
        if self.context.is_open()
            && let Some(pid) = self.context_pid
            && let Some(process) = self
                .model
                .processes
                .iter()
                .find(|process| process.pid == pid)
        {
            let resource_id = process_resource_id(pid, process.started_at);
            for (item, rect) in self.context_menu_item_rects() {
                if let Some(label) = self.context.items().get(item).and_then(MenuItem::label) {
                    targets.push((
                        format!("monitor.context:{resource_id}:{item}"),
                        rect,
                        AccessNode::new(Role::MenuItem)
                            .with_name(label)
                            .with_semantic_key(format!("context:{resource_id}:{item}"))
                            .with_state(AccessState::new().focusable(true))
                            .with_bounds(rect),
                    ));
                }
            }
        }
        if self.hit.stop_btn.area() > 0.0
            && let Some(process) = self.selected_process()
        {
            targets.push(button(
                &format!(
                    "{}:stop",
                    monitor_process_target(process.pid, process.started_at)
                ),
                self.hit.stop_btn,
                "Stop selected process",
            ));
        }
        if self.hit.kill_btn.area() > 0.0
            && let Some(process) = self.selected_process()
        {
            targets.push(button(
                &format!(
                    "{}:kill",
                    monitor_process_target(process.pid, process.started_at)
                ),
                self.hit.kill_btn,
                "Force kill selected process",
            ));
        }
        targets
            .into_iter()
            .filter(|(_, rect, _)| rect.area() > 0.0)
            .collect()
    }
}

impl MonitorApp {
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if type_id != MONITOR_OPERATION_TYPE {
            return Ok(false);
        }
        let operation: MonitorOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let expected_kind = match &operation {
            MonitorOperation::Inspect { .. } => OperationKind::View,
            MonitorOperation::Refresh
            | MonitorOperation::CopyPid { .. }
            | MonitorOperation::Signal { .. } => OperationKind::Effect,
        };
        if kind != expected_kind {
            return Err("Monitor operation kind does not match its payload".into());
        }
        self.perform_operation(&operation, SignalConfirmation::ActionContract)?;
        if let MonitorOperation::Inspect { pid, .. } | MonitorOperation::CopyPid { pid, .. } =
            &operation
        {
            // perform_operation already updated the local selection; publish it.
            let selected = Some(*pid);
            if self.selected_pid != selected {
                self.selected_pid = selected;
            }
            self.commit_view(
                doc,
                &crate::resource::MonitorAction::SetSelected {
                    pid: Some(u64::from(*pid)),
                },
            );
        }
        Ok(true)
    }
}

impl Widget for MonitorApp {
    ui::provides!(
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource,
        dyn ui::undo::UndoPoints
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[MONITOR_OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {
        // Monitor's observation and safety preferences are device-local, so a
        // shared document sync must never overwrite this window's state.
        self.clear_selection_if_filtered_out();
    }

    #[allow(
        clippy::unwrap_used,
        reason = "Widget::build_layout must return a NodeId with no fallible path to propagate to; new_leaf on a fresh, empty-style node cannot fail short of the taffy arena itself being corrupted"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // A missing layout would mean `node` came from a different tree than
        // the one `compute_layout` ran over — keep the previous frame's
        // `rect` rather than panic.
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.maybe_refresh();
        self.layout_text_scale = f64::from(theme.text_scale);
        let r = self.rect;
        if r.width() <= 4.0 || r.height() <= 4.0 {
            return;
        }
        let t = transform;
        self.hit = Hit::new();

        // App background.
        scene.fill(
            Fill::NonZero,
            t,
            theme.tokens.colors.surfaces.base,
            None,
            &r,
        );

        // Advance the eased push flap only while it has motion to settle.
        // The shared reduced-motion policy snaps it to its requested pane.
        self.proc_push.tick(theme.reduce_motion);

        // Every text-bearing band takes its pitch from this frame's measured
        // line boxes; the event path (wheel-to-rows, hit rows) reads the same
        // cached values, so paint and pointer routing never disagree.
        self.density = Density::measure(text_renderer, theme);

        let content = Rect::new(
            r.x0 + OUTER_PAD,
            r.y0 + OUTER_PAD,
            r.x1 - OUTER_PAD,
            r.y1 - OUTER_PAD,
        );
        let header = Rect::new(
            content.x0,
            content.y0,
            content.x1,
            content.y0 + self.density.header_h,
        );
        let body_top = header.y1 + GAP;
        let body = Rect::new(content.x0, body_top, content.x1, content.y1);

        self.draw_header(scene, t, text_renderer, theme, header);
        self.tick_interval_press_hold();

        self.draw_scrolled_body(scene, t, text_renderer, theme, body);
        self.draw_overlays(scene, t, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // A pending stop confirmation is modal: while it's up the shared
        // dialog captures every event (typing, clicks, Esc) and nothing leaks
        // to the process table behind the scrim.
        if self.confirm_stop.is_some() {
            self.route_confirm_event(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                ctx.state,
                ctx.path,
                ctx.handled,
            );
            return;
        }
        if self.route_search(event, ctx) {
            return;
        }
        self.dispatch_event(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            ctx.state,
            ctx.path,
            ctx.handled,
        );
    }

    fn name(&self) -> &'static str {
        "MonitorApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("monitor_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.search_input.ime_cursor_rect()
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.search_input.ime_purpose()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.search_input.cursor_shape(point)
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        if let Some(confirm) = self.confirm_dialog.as_ref() {
            return confirm.dialog.accessibility();
        }
        let children = self
            .accessibility_targets()
            .into_iter()
            .map(|(target, _, node)| node.with_action_target(target))
            .collect();
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Group)
                .with_name("System Monitor")
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else {
            return None;
        };
        self.accessibility_targets()
            .get(*index)
            .map(|(_, rect, _)| *rect)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "semantic targets intentionally live beside their direct state mutations, preserving a complete auditable mapping for every custom-painted control"
    )]
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action;
        let (state, path) = ui::doc::document(state, path);

        let activated = matches!(action, Action::Activate | Action::ToggleCheck);
        match target {
            "monitor.search" => return self.search_input.perform_action(action, state, path),
            "monitor.tab.processes" if activated => self.commit_tab(state, Tab::Processes),
            "monitor.tab.resources" if activated => self.commit_tab(state, Tab::Resources),
            "monitor.detail-back" if activated => self.proc_push.show_master(),
            "monitor.interval" if matches!(action, Action::Activate | Action::Expand) => {
                self.open_interval_popover();
            }
            "monitor.refresh" if activated => self.request_monitor_control("refresh"),
            "monitor.process-source.retry" if activated => {
                self.request_monitor_control("refresh");
            }
            "monitor.auto-refresh" if activated => {
                self.prefs.auto_refresh = !self.prefs.auto_refresh;
                if self.prefs.auto_refresh {
                    self.last_refresh = ui::time::now();
                }
            }
            "monitor.kernel-threads" if activated => {
                self.prefs.show_kernel_threads = !self.prefs.show_kernel_threads;
                self.clear_selection_if_filtered_out();
                self.clamp_scroll();
            }
            "monitor.tree-mode" if activated => {
                self.commit_tree_mode(state, !self.tree_mode);
            }
            "monitor.sum-mode" if activated && self.tree_mode => {
                self.sum_mode = self.sum_mode.toggled();
            }
            "monitor.confirm-kills" if activated => {
                self.prefs.confirm_kill = !self.prefs.confirm_kill;
            }
            _ => {
                if let Some(index) = target.strip_prefix("monitor.summary.") {
                    let Ok(index) = index.parse::<usize>() else {
                        return false;
                    };
                    if activated && index < 4 {
                        self.commit_tab(state, Tab::Resources);
                        let (body_w, viewport_h) = self.body_metrics();
                        self.view_scroll_y =
                            self.resources_scroll_for_stat(index, body_w, viewport_h);
                        return true;
                    }
                    return false;
                }
                if let Some(resource_id) = target.strip_prefix("monitor.tree-toggle:") {
                    let Some((pid, started_at)) = parse_process_resource_id(resource_id) else {
                        return false;
                    };
                    if activated
                        && self.tree_mode
                        && self.row_has_children(pid)
                        && self
                            .model
                            .processes
                            .iter()
                            .any(|process| process.pid == pid && process.started_at == started_at)
                    {
                        self.toggle_expand(pid);
                        return true;
                    }
                    return false;
                }
                if let Some(context_target) = target.strip_prefix("monitor.context:") {
                    let Some((resource_id, item)) = context_target.rsplit_once(':') else {
                        return false;
                    };
                    let Some((pid, started_at)) = parse_process_resource_id(resource_id) else {
                        return false;
                    };
                    let Ok(item) = item.parse::<usize>() else {
                        return false;
                    };
                    let process_is_current = self
                        .model
                        .processes
                        .iter()
                        .any(|process| process.pid == pid && process.started_at == started_at);
                    if activated
                        && process_is_current
                        && self.context.is_open()
                        && self.context_pid == Some(pid)
                        && matches!(
                            self.context.items().get(item),
                            Some(MenuItem::Action { .. })
                        )
                    {
                        self.activate_context_item(item, state, path);
                        self.close_context_menu();
                        return true;
                    }
                    return false;
                }
                if let Some(label) = target.strip_prefix("monitor.sort.") {
                    let sort = match label {
                        "PID" => SortKey::Pid,
                        "name" => SortKey::Name,
                        "user" => SortKey::User,
                        "CPU" => SortKey::Cpu,
                        "memory" => SortKey::Mem,
                        "threads" => SortKey::Threads,
                        _ => return false,
                    };
                    if activated {
                        self.set_sort(state, sort);
                        return true;
                    }
                    return false;
                }
                if let Some(ms) = target.strip_prefix("monitor.interval:") {
                    let Ok(ms) = ms.parse::<u64>() else {
                        return false;
                    };
                    if activated && INTERVAL_OPTIONS.contains(&ms) {
                        self.prefs.interval_ms = ms;
                        self.interval_popover.close();
                        return true;
                    }
                    return false;
                }
                let Some(process_target) = target.strip_prefix("monitor.process:") else {
                    return false;
                };
                let (resource_id, command) = match process_target.rsplit_once(':') {
                    Some((resource_id, command @ ("stop" | "kill"))) => {
                        (format!("process:{resource_id}"), Some(command))
                    }
                    _ => (format!("process:{process_target}"), None),
                };
                let Some((pid, started_at)) = parse_process_resource_id(&resource_id) else {
                    return false;
                };
                if !self
                    .model
                    .processes
                    .iter()
                    .any(|process| process.pid == pid && process.started_at == started_at)
                {
                    return false;
                }
                match (command, action) {
                    (None, Action::Focus | Action::ScrollIntoView) => {
                        self.commit_selected(state, Some(pid));
                        return true;
                    }
                    (None, Action::Activate) => {
                        self.commit_selected(state, Some(pid));
                        if self.is_narrow() {
                            self.proc_push.show_detail();
                        }
                        return true;
                    }
                    (Some("stop"), Action::Activate) => {
                        self.request_process_control(pid, "stop");
                        return true;
                    }
                    (Some("kill"), Action::Activate) => {
                        self.request_process_control(pid, "kill");
                        return true;
                    }
                    _ => return false,
                }
            }
        }
        true
    }

    /// Ask for a 60 Hz frame only for a short-lived visual need. Periodic
    /// sampling uses [`Self::next_wake_deadline`] instead, so an idle Monitor
    /// sleeps between its selected sampling intervals rather than repainting
    /// the entire dashboard continuously.
    fn needs_animation_frame(&self) -> bool {
        self.proc_push.is_animating()
            || self.copy_toast_active()
            || self.interval_press.is_some()
            || self.search_input.needs_animation_frame()
            || self.tooltips.needs_frame()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let mut next = None;
        let mut consider = |deadline: Instant| {
            next = Some(next.map_or(deadline, |current: Instant| current.min(deadline)));
        };
        if !self.sample_only {
            // `last_refresh` is advanced only after sampling, making this a
            // bounded, cadence-aligned wake instead of a polling frame loop.
            consider(
                self.last_refresh
                    .checked_add(Duration::from_millis(self.prefs.interval_ms))
                    .unwrap_or_else(Instant::now),
            );
        }
        if let Some(started) = self.interval_press {
            consider(
                started
                    .checked_add(Duration::from_millis(LONG_PRESS_MS as u64))
                    .unwrap_or_else(Instant::now),
            );
        }
        if let Some(copied_at) = self.copied_at {
            consider(
                copied_at
                    .checked_add(Duration::from_millis(TOAST_MS))
                    .unwrap_or_else(Instant::now),
            );
        }
        next
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        if reduced {
            self.proc_push.snap(self.proc_push.pane());
        }
    }
}

impl ui::undo::UndoPoints for MonitorApp {
    fn needs_commit(&self) -> bool {
        self.search_input.needs_commit()
    }
    fn on_commit(&mut self) {
        self.search_input.on_commit();
    }
}

impl ui::search::capability::SearchSource for MonitorApp {
    fn search_providers(&self) -> Vec<Box<dyn SearchProvider>> {
        vec![Box::new(self.process_search.clone())]
    }
}

impl ui::resources::PublishesResources for MonitorApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the processes door — keep only live process rows.
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }
    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui::widget::ActionDirectoryResourcePage> {
        // One compact root exposes the safe contextual sample. Individual
        // process ids are resolved lazily below, avoiding an all-process
        // materialization in the broker's cursor-based directory.
        let resources = if cursor.is_empty() && max_results > 0 {
            self.resources(doc)
        } else {
            Vec::new()
        };
        Some(ui::widget::ActionDirectoryResourcePage {
            resources,
            next_cursor: Vec::new(),
            exhausted: true,
        })
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if id == "processes" {
            if self.is_hosted() {
                return None;
            }
            return self.publish_resources(doc).into_iter().next();
        }
        let (pid, started_at) = parse_process_resource_id(id)?;
        self.model
            .processes
            .iter()
            .find(|process| process.pid == pid && process.started_at == started_at)
            .map(monitor_process_resource)
    }
}

impl ui::widget::controls::ActionControls for MonitorApp {
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.pending_control_invocation.take()
    }
}

impl ui::history_mode::capability::DocumentHistory for MonitorApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

impl MonitorApp {
    /// A press-and-hold on the interval pill opens the option popover while
    /// still held (the pill's hit rect is now known from `draw_header`). The
    /// redraw loop keeps pumping — `needs_animation_frame` is always true —
    /// so this check fires mid-hold without waiting for another event. Also
    /// drains a headless `--shots` seed to open the popover deterministically
    /// once the pill is positioned.
    fn tick_interval_press_hold(&mut self) {
        if let Some(started) = self.interval_press
            && !self.interval_hold_opened
            && !self.interval_popover.is_open()
            && started.elapsed().as_millis() >= LONG_PRESS_MS
        {
            self.open_interval_popover();
            self.interval_hold_opened = true;
        }
        if std::mem::take(&mut self.interval_popover_seed) {
            self.open_interval_popover();
        }
    }

    /// The Resources dashboard can be taller than its viewport — the
    /// overview row + per-core grid + chart grid on a large machine or a
    /// short/narrow window. Mirrors `ui::widget::Scrollable`'s model: draw
    /// the body content at `(… − offset)` inside a clip layer the size of
    /// the viewport, clamp the offset to `content − viewport`, and paint a
    /// scrollbar. The header stays pinned above the scroll. The Processes
    /// tab manages its own internal row scroll, so it reports zero
    /// page-content height and never page-scrolls.
    fn draw_scrolled_body(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
    ) {
        let viewport_h = body.height();
        let content_natural_h = self.content_natural_h(body.width());
        let page_overflow = (content_natural_h - viewport_h).max(0.0);
        self.content_h = content_natural_h.max(viewport_h);
        self.viewport_h = viewport_h;
        self.view_scroll_y = self.view_scroll_y.clamp(0.0, page_overflow as f32);
        let dy = f64::from(self.view_scroll_y);

        // Body content is laid out from `body.y0 − dy` and clipped to the
        // body viewport. Shifting the *rects* (rather than the transform)
        // keeps the hit rectangles the draw functions record in real screen
        // space, so clicks land correctly while scrolled. The draw functions
        // receive rects extended to the natural content height so nothing
        // they lay out gets truncated by the viewport — the clip is what
        // hides the off-screen part.
        let scrolling = page_overflow > 0.5;
        if scrolling {
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &body);
        }
        let top = body.y0 - dy;
        let content_bottom = top + self.content_h;
        let body_scrolled = Rect::new(body.x0, top, body.x1, content_bottom);

        match self.tab {
            Tab::Processes => self.draw_processes(scene, t, text_renderer, theme, body_scrolled),
            Tab::Resources => self.draw_resources(scene, t, text_renderer, theme, body_scrolled),
        }

        if scrolling {
            scene.pop_layer();
            self.draw_page_scrollbar(scene, t, theme, body, viewport_h, page_overflow, dy);
        }
    }

    /// Page scrollbar down the right edge of the body viewport — a
    /// draggable thumb over a page-able track.
    fn draw_page_scrollbar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        body: Rect,
        viewport_h: f64,
        page_overflow: f64,
        dy: f64,
    ) {
        let track = Rect::new(body.x1 - 8.0, body.y0, body.x1 - 1.0, body.y1);
        let c = &theme.tokens.colors;
        paint::fill_rrect(scene, t, track, 2.0, paint::inner_rule(theme, 40));
        let frac_v = (viewport_h / self.content_h).clamp(0.0, 1.0);
        let thumb_h = (track.height() * frac_v).max(24.0);
        let pos = if page_overflow > 0.0 {
            dy / page_overflow
        } else {
            0.0
        };
        let ty0 = track.y0 + (track.height() - thumb_h) * pos;
        let thumb = Rect::new(track.x0, ty0, track.x1, ty0 + thumb_h);
        paint::fill_rrect(scene, t, thumb, 2.0, with_alpha(c.text_dim, 160));
        self.hit.page_scrollbar = Some(ScrollBarHit {
            track,
            thumb,
            max_value: page_overflow,
        });
    }

    /// Everything painted above the tab content: the context menu, the
    /// interval popover, the copy toast, and the stop-confirmation dialog.
    fn draw_overlays(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.seed_context_menu_from_shots();
        self.draw_context_menu_overlay(scene, t, text_renderer, theme, presence);

        // The refresh-interval option popover floats above the content (below
        // the confirm modal). The reusable primitive positions + paints itself
        // against the pill anchor; the app routes clicks/keys into it.
        if self.interval_popover.is_open() {
            self.interval_popover
                .draw(scene, t, text_renderer, theme, presence);
        }

        // The "PID copied" toast floats above the content, below the modal.
        self.draw_toast(scene, t, text_renderer, theme);

        // `needs_animation_frame` holds the host long enough to reach the
        // dwell deadline; this tick makes the tooltip visible when due.
        let _ = self.tooltips.tick(ui::time::now());
        if let Some((label, anchor)) = self.tooltips.visible() {
            draw_hover_tooltip(scene, t, text_renderer, theme, anchor, label, self.rect);
        }

        if let Some((pid, _started_at, kind)) = self.confirm_stop {
            self.ensure_confirm_dialog(pid, kind);
            self.layout_confirm_dialog();
            if let Some(cd) = self.confirm_dialog.as_mut() {
                cd.dialog.draw(scene, t, text_renderer, theme, presence);
            }
        } else {
            self.confirm_dialog = None;
        }
    }

    /// A `--shots` request to open the context menu over a seeded pid: now
    /// that the table rows are laid out (`hit.rows` is populated), anchor the
    /// menu at that row and clear the seed. If the seeded pid isn't a visible
    /// row this frame, fall back to the first laid-out row so the shot still
    /// demonstrates the menu.
    fn seed_context_menu_from_shots(&mut self) {
        let Some(seed) = self.context_menu_seed.take() else {
            return;
        };
        let target = self
            .hit
            .rows
            .iter()
            .find(|(rp, _)| *rp == seed)
            .or_else(|| self.hit.rows.first())
            .map(|(rp, rect)| (*rp, *rect));
        if let Some((pid, rect)) = target {
            let anchor = Point::new(rect.x0 + 60.0, rect.y0 + self.density.row_h);
            self.open_context_menu(pid, anchor);
            // Rest the pointer inside the last action row (Kill), near its
            // leading edge so it stays within the row once the primitive has
            // measured its labels and settled a narrower sheet.
            if let Some((_, row)) = self.context_menu_item_rects().last() {
                self.last_cursor = Point::new(row.x0 + 20.0, row.center().y);
            }
        }
    }

    /// The right-click action menu floats above the content (but below the
    /// confirm modal). The primitive renders itself at its stored anchor; we
    /// draw it directly here, the same hand-drawn way as the rest of the app,
    /// then record its item hit rects for `on_click` routing.
    fn draw_context_menu_overlay(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.context.is_open() {
            self.hit.menu_items.clear();
            return;
        }
        self.context.draw(scene, t, text_renderer, theme, presence);
        self.hit.menu_items = self.context_menu_item_rects();
        // Highlight the row under the cursor for affordance.
        let cur = self.last_cursor;
        let c = &theme.tokens.colors;
        for (_idx, rect) in &self.hit.menu_items {
            if rect.contains(cur) {
                paint::fill_rrect(scene, t, *rect, 4.0, with_alpha(c.accent[5], 70));
            }
        }
        // Re-draw labels over the highlight so they stay legible.
        self.context.draw(scene, t, text_renderer, theme, presence);
    }

    /// Resolve search-box focus for this event, and either dispatch it into
    /// the search box or hand back control. Returns whether `handle_event`
    /// should return immediately — either because the search box consumed
    /// the event, or because a reserved global chord is deliberately left
    /// unhandled so it bubbles to the host.
    fn route_search(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        let search_rect = self.hit.search;
        self.search_input.apply_layout_external(Rect::new(
            search_rect.x0 + 34.0,
            search_rect.y0,
            search_rect.x1 - 8.0,
            search_rect.y1,
        ));
        let left_press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        if left_press {
            self.search_input
                .set_focus(search_rect.contains(self.last_cursor));
        }

        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
        {
            let ctrl = ctx.modifiers.state().control_key();
            if ctrl
                && matches!(&key.logical_key, Key::Character(s) if matches!(s.as_str(), "k" | "K" | "/"))
            {
                // Keep the host's global-search chords available even when
                // the local process filter owns keyboard focus. `handled`
                // stays false → event propagates to the host.
                return true;
            }
            if self.search_input.is_focused()
                && matches!(
                    &key.logical_key,
                    Key::Named(NamedKey::Escape | NamedKey::Enter)
                )
            {
                self.search_input.set_focus(false);
                *ctx.handled = true;
                return true;
            }
        }

        let route_to_search = self.search_input.is_focused()
            || matches!(event, WindowEvent::CursorMoved { .. })
            || (left_press && search_rect.contains(self.last_cursor));
        if !route_to_search {
            return false;
        }
        let mut input_handled = false;
        self.search_input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                handled: &mut input_handled,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        if self.search_input.take_dirty().is_some() {
            self.scroll_off = 0.0;
            self.clear_selection_if_filtered_out();
            let (doc, _) = ui::doc::document(ctx.state, ctx.path);
            let query = self.search_input.value().to_string();
            self.commit_view(doc, &crate::resource::MonitorAction::SetSearch { query });
            // Keep the shared selection aligned when the filter drops the row.
            self.commit_view(
                doc,
                &crate::resource::MonitorAction::SetSelected {
                    pid: self.selected_pid.map(u64::from),
                },
            );
            input_handled = true;
        }
        if input_handled && !matches!(event, WindowEvent::CursorMoved { .. }) {
            *ctx.handled = true;
            return true;
        }
        false
    }

    /// The main event dispatch once the confirm modal and the search box
    /// have both had first refusal.
    fn dispatch_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        if !matches!(event, WindowEvent::CursorMoved { .. }) {
            let _ = self.tooltips.dismiss();
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                let tooltip = [
                    (self.hit.refresh, "Refresh now"),
                    (
                        self.hit.auto_refresh,
                        if self.prefs.auto_refresh {
                            "Pause automatic refresh"
                        } else {
                            "Resume automatic refresh"
                        },
                    ),
                ]
                .into_iter()
                .find(|(rect, _)| rect.contains(self.last_cursor))
                .map(|(rect, label)| (rect, label.to_string()));
                let _ = self.tooltips.on_pointer(tooltip, ui::time::now());
                // A live scrollbar thumb drag follows the cursor.
                if self.scroll_drag.is_some() {
                    self.apply_scroll_drag(self.last_cursor);
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_wheel(*delta, modifiers.state().shift_key(), handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_cursor;
                self.on_click(p, doc, path, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let p = self.last_cursor;
                self.on_right_click(p, doc, path, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                // End any scrollbar thumb drag, otherwise resolve a pending
                // interval-pill press (quick release cycles; a held release
                // that already opened the popover leaves it up). `||`
                // short-circuits exactly like the `if`/`else if` this
                // replaces: `release_interval_press` only runs when no drag
                // was active.
                if self.scroll_drag.take().is_some() || self.release_interval_press(doc, path) {
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: ke, .. } if ke.state == ElementState::Pressed => {
                let text = ke.text.as_ref().map(winit::keyboard::SmolStr::as_str);
                let ctrl = modifiers.state().control_key();
                let prev_tab = self.tab;
                self.on_key(&ke.logical_key, text, ctrl, doc, path, handled);
                // Switching tabs resets the page scroll so the new tab opens
                // at the top rather than inheriting a stale offset.
                if self.tab != prev_tab {
                    self.view_scroll_y = 0.0;
                }
            }
            _ => {}
        }
    }

    /// A mouse-wheel notch: scrolls the process table (rows, or sideways
    /// through overflowing columns on Shift) when the cursor is over it on
    /// the Processes tab, otherwise scrolls the whole page. A no-op while a
    /// stop confirmation is up (the modal owns input).
    fn handle_wheel(&mut self, delta: MouseScrollDelta, shift: bool, handled: &mut bool) {
        if self.confirm_stop.is_some() {
            return;
        }
        // Pixels of scroll this notch represents (≈60px/line desktop
        // convention; touchpad pixel deltas pass through 1×).
        let (px_x, px_y) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (-x * 60.0, -y * 60.0),
            MouseScrollDelta::PixelDelta(pos) => (-(pos.x as f32), -(pos.y as f32)),
        };
        let over_table = self.tab == Tab::Processes
            && self.proc_push.slide() < 0.5
            && self.table_viewport.contains(self.last_cursor);
        if over_table {
            // Shift-wheel (or a horizontal touchpad delta) scrolls the table
            // sideways through its overflowing columns; a plain vertical
            // wheel scrolls the rows.
            if shift || px_x.abs() > px_y.abs() {
                let dx = if shift && px_x == 0.0 { px_y } else { px_x };
                // Clamp against the *columns* viewport (`table_cols_w`, the
                // padded band), NOT the card's full inner width, so the
                // final column + its right padding scroll into view.
                let max_x = (self.table_content_w - self.table_cols_w).max(0.0) as f32;
                self.table_scroll_x = (self.table_scroll_x + dx).clamp(0.0, max_x);
            } else {
                let rows = px_y / self.density.row_h as f32;
                self.scroll_off = (self.scroll_off + rows).max(0.0);
                self.clamp_scroll();
            }
            *handled = true;
        } else {
            // Anywhere else (the Resources dashboard, the summary strip, the
            // toolbar) scrolls the whole page.
            let max_y = (self.content_h - self.viewport_h).max(0.0) as f32;
            let before = self.view_scroll_y;
            self.view_scroll_y = (self.view_scroll_y + px_y).clamp(0.0, max_y);
            if (self.view_scroll_y - before).abs() > f32::EPSILON {
                *handled = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::text_render::TextRenderer;

    fn app() -> MonitorApp {
        MonitorApp::new()
    }

    fn resource_id_for(a: &MonitorApp, pid: u32) -> String {
        let started_at = a
            .model
            .processes
            .iter()
            .find(|process| process.pid == pid)
            .expect("test process remains present")
            .started_at;
        process_resource_id(pid, started_at)
    }

    /// Lay the app out at `(w, h)` and run one real `draw` so the frame caches
    /// (`table_content_w`, `table_cols_w`, `table_viewport`, the hit rects)
    /// are populated exactly as the live renderer would. No GPU — `draw` only
    /// writes into a CPU `Scene`.
    fn draw_at(a: &mut MonitorApp, w: f64, h: f64) {
        a.rect = Rect::new(0.0, 0.0, w, h);
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        a.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    /// Whether two rects share more than a hairline of area (a real overlap).
    fn rects_overlap(a: Rect, b: Rect) -> bool {
        let x0 = a.x0.max(b.x0);
        let y0 = a.y0.max(b.y0);
        let x1 = a.x1.min(b.x1);
        let y1 = a.y1.min(b.y1);
        x1 > x0 + 0.5 && y1 > y0 + 0.5
    }

    /// `draw_at` under a 200 % accessibility text scale: the theme carries the
    /// scaled tokens and the renderer carries the same multiplier, exactly as
    /// the live `ui::App` configures them.
    fn draw_at_scaled(a: &mut MonitorApp, w: f64, h: f64, scale: f32) {
        a.rect = Rect::new(0.0, 0.0, w, h);
        let theme = Theme::dark().with_text_scale(scale);
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        tr.set_text_scale(scale);
        let mut scene = Scene::new();
        a.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    /// Density is the design constants at 1x, grows with measured lines at 2x, and
    /// table rows and sort headers keep an s1 gap under every scale.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn density_and_table_rows_follow_measured_metrics_and_keep_s1_gaps() {
        // Phase `density_at_1x_is_the_design_constants`.
        // The 1× design is exact: at text scale 1 the measured density is the
        // constant set, so nothing about the 1× frame moved when the bands
        // started following the measured line boxes.
        {
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            let d = Density::measure(&mut tr, &theme);
            assert_eq!(
                d,
                Density::ONE,
                "1× density must equal the design constants"
            );
        }

        // Phase `density_at_2x_grows_rows_by_their_measured_lines_and_rows_stay_seated`.
        // At 200 % every text-bearing band grows by at least the extra height of
        // the lines it stacks, and the row pitch the table pages by is the same
        // pitch the rows were painted at — layout, paint and the wheel-to-rows
        // hit math read one number.
        {
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let theme = Theme::dark().with_text_scale(2.0);
            let d = Density::measure(&mut tr, &theme);
            let sm = ts(&tr, theme.tokens.typography.scale.sm);
            let sm_line = paint::line_box(&mut tr, sm);
            assert!(
                (sm - theme.tokens.typography.scale.sm / 2.0).abs() < 1e-4,
                "the token is fed to the renderer at half size so it paints once at 2×: {sm}"
            );
            assert!(
                d.row_h >= sm_line + 2.0,
                "a 2× row ({}) must seat its 2× line ({sm_line}) with padding",
                d.row_h
            );
            assert!(d.row_h > ROW_H && d.table_header_h > TABLE_HEADER_H);
            assert!(d.toolbar_row_h > TOOLBAR_ROW_H && d.header_h > HEADER_H);
            assert!(d.detail_h > DETAIL_H && d.strip_cell_h > layout::STRIP_CELL_H);
            assert!((d.text_k - 2.0).abs() < 1e-6);

            // The production frame: rows_per_page and every recorded row rect use
            // the measured pitch, and no two row hit rects overlap.
            let mut a = MonitorApp::new();
            a.seed_shot_scenario("detail");
            draw_at_scaled(&mut a, 800.0, 900.0, 2.0);
            assert_eq!(a.density, d, "draw caches the density it painted with");
            let rows: Vec<Rect> = a.hit.rows.iter().map(|(_, r)| *r).collect();
            assert!(
                rows.len() >= 2,
                "the seeded table paints rows: {}",
                rows.len()
            );
            let gap = d.row_gap;
            assert!((gap - f64::from(theme.tokens.spacing.s1)).abs() < 1e-6);
            for pair in rows.windows(2) {
                let (top, next) = (pair[0], pair[1]);
                assert!(
                    (top.height() - (d.row_h - gap)).abs() < 1e-6,
                    "row band {} must be the measured pitch {} less the row gap {gap}",
                    top.height(),
                    d.row_h
                );
                assert!(
                    next.y0 - top.y1 >= gap - 1e-6,
                    "adjacent rows keep an s1 gap and never touch: {top:?} vs {next:?}"
                );
            }
            assert!(
                a.rows_per_page as f64 * d.row_h <= a.table_viewport.height() + 1e-6,
                "the page count is derived from the same pitch the rows are painted at"
            );
        }

        // Phase `table_rows_and_sort_headers_keep_an_s1_gap_at_1x`.
        // Rule 1 of the 1× bar on the process table: every row band is at least
        // the 24 px touch floor, consecutive rows keep exactly one `s1` between
        // them, and the sort headers clear each other and the first row by a
        // token step — so nothing in the table touches at 100 %. The same holds
        // under `ui_scale` 2 (s1 = 8): the pitch grows with the gap, so the band
        // never drops below its floor.
        {
            for theme in [Theme::dark(), Theme::dark().with_ui_scale(2.0)] {
                table_rows_keep_an_s1_gap_under(&theme);
            }
        }
    }

    /// `draw_at` under an arbitrary theme (a ui-scaled or custom-spacing one).
    fn draw_with_theme(a: &mut MonitorApp, w: f64, h: f64, theme: &Theme) {
        a.rect = Rect::new(0.0, 0.0, w, h);
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        a.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    }

    fn table_rows_keep_an_s1_gap_under(theme: &Theme) {
        let gap = f64::from(theme.tokens.spacing.s1);
        let mut a = MonitorApp::new();
        a.seed_shot_scenario("detail");
        draw_with_theme(&mut a, 1280.0, 800.0, theme);
        assert!(
            (a.density.row_gap - gap).abs() < 1e-6,
            "the painted row gap is this theme's s1 ({gap}), got {}",
            a.density.row_gap
        );
        let rows: Vec<Rect> = a.hit.rows.iter().map(|(_, r)| *r).collect();
        assert!(
            rows.len() >= 2,
            "the seeded table paints rows: {}",
            rows.len()
        );
        for r in &rows {
            assert!(
                r.height() >= 24.0 - 1e-6,
                "row band {} under the 24 px floor",
                r.height()
            );
        }
        for pair in rows.windows(2) {
            let (top, next) = (pair[0], pair[1]);
            assert!(
                (next.y0 - top.y1 - gap).abs() < 1e-6,
                "rows step by exactly one s1: {top:?} vs {next:?}"
            );
        }
        let mut headers: Vec<Rect> = a.hit.sort_headers.iter().map(|(_, r)| *r).collect();
        headers.sort_by(|x, y| x.x0.total_cmp(&y.x0));
        assert!(
            headers.len() >= 2,
            "sort headers recorded: {}",
            headers.len()
        );
        for pair in headers.windows(2) {
            assert!(
                pair[1].x0 - pair[0].x1 >= gap - 1e-6,
                "sort headers keep an s1 gap: {:?} vs {:?}",
                pair[0],
                pair[1]
            );
        }
        let first_row = rows[0];
        for h in &headers {
            assert!(
                h.height() >= 24.0 - 1e-6,
                "header target {} under the 24 px floor",
                h.height()
            );
            assert!(
                first_row.y0 - h.y1 >= gap - 1e-6,
                "header {h:?} clears the first row {first_row:?} by an s1"
            );
        }
    }

    /// The process list starts CPU-sorted, re-sorts and filters (flat and tree), and
    /// hides kernel threads by default.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn process_list_sorts_and_filters_visible_rows() {
        // Phase `starts_on_processes_with_cpu_sort_and_a_selection`.
        {
            let a = app();
            assert!(matches!(a.tab, Tab::Processes));
            assert!(matches!(a.sort_key, SortKey::Cpu));
            assert!(a.sort_desc);
            assert!(a.selected_pid.is_some());
        }

        // Phase `cpu_sort_descending_orders_busiest_first`.
        {
            let a = app();
            let v = a.visible_processes();
            assert!(v.len() >= 2);
            assert!(v[0].cpu >= v[1].cpu, "first row must be the busiest");
        }

        // Phase `toggling_sort_key_flips_direction_on_repeat`.
        {
            let mut a = app();
            a.set_sort(&mut AutoCommit::new(), SortKey::Name); // text → ascending
            assert!(!a.sort_desc);
            a.set_sort(&mut AutoCommit::new(), SortKey::Name); // repeat → flip
            assert!(a.sort_desc);
        }

        // Phase `filter_narrows_the_list`.
        {
            let mut a = app();
            let all = a.visible_processes().len();
            a.search_input.set_text("browser");
            let some = a.visible_processes().len();
            assert!(some >= 1 && some < all);
            assert!(a.visible_processes().iter().all(|p| {
                p.name.to_lowercase().contains("browser")
                    || p.user.to_lowercase().contains("browser")
                    || p.pid.to_string().contains("browser")
            }));
        }

        // Phase `kernel_threads_hidden_by_default`.
        {
            let mut a = app();
            assert!(a.visible_processes().iter().all(|p| !p.kernel));
            a.prefs.show_kernel_threads = true;
            assert!(a.visible_processes().iter().any(|p| p.kernel));
        }

        // Phase `filter_works_over_the_tree`.
        // Search/filter still works over the tree: filtering to a deep child
        // keeps its ancestors visible (so the match is reachable) and prunes
        // unrelated branches.
        {
            let mut a = app();
            a.search_input.set_text("rustc");
            let rows = a.visible_rows();
            let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
            assert!(names.contains(&"rustc"), "match present: {names:?}");
            // rustc's ancestor chain (cargo → code → shell) is force-shown.
            assert!(names.contains(&"cargo"), "ancestor cargo kept: {names:?}");
            assert!(names.contains(&"code"), "ancestor code kept: {names:?}");
            assert!(names.contains(&"shell"), "ancestor shell kept: {names:?}");
            // An unrelated branch (systemd's services) is pruned.
            assert!(
                !names.contains(&"chronyd"),
                "unrelated branch pruned: {names:?}"
            );
        }
    }

    /// Selection follows the visible process generation: filtering or PID reuse clears it,
    /// arrows move it, and the detail always names a visible tree row.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn selection_tracks_visible_process_identity() {
        // Phase `filtering_away_selected_process_clears_its_actionable_detail`.
        {
            let mut a = app();
            assert!(a.selected_pid.is_some(), "sample starts with a selection");
            a.search_input.set_text("__no_process_matches__");
            a.clear_selection_if_filtered_out();
            assert!(a.selected_pid.is_none());
        }

        // Phase `recycled_pid_clears_selection_and_does_not_inherit_cpu_history`.
        {
            let mut a = app();
            let selected = a.selected_process().expect("sample has a selection");
            let old_identity = (selected.pid, selected.started_at);
            a.record_proc_cpu();
            assert!(a.proc_cpu_hist.contains_key(&old_identity));

            // Model the next OS sample containing a different process that reused
            // the same numeric PID. Neither the detail selection nor a sparkline
            // is allowed to cross that generation boundary.
            let replacement = a
                .model
                .processes
                .iter_mut()
                .find(|p| p.pid == selected.pid)
                .expect("selected process is in the sample");
            replacement.started_at = replacement.started_at.saturating_add(1);
            let new_identity = (replacement.pid, replacement.started_at);
            a.clear_recycled_selection(Some(old_identity));
            a.record_proc_cpu();

            assert_eq!(
                a.selected_pid, None,
                "selection does not jump to a PID reuse"
            );
            assert!(
                !a.proc_cpu_hist.contains_key(&old_identity),
                "old process history is pruned"
            );
            assert!(
                a.proc_cpu_hist.contains_key(&new_identity),
                "replacement process begins its own history"
            );
        }

        // Phase `arrow_navigation_moves_selection`.
        {
            let mut a = app();
            a.tree_mode = false; // flat list so visible_rows order == sorted list
            a.rows_per_page = 10;
            let v = a.visible_rows();
            a.selected_pid = Some(v[0].pid);
            a.move_selection(&mut AutoCommit::new(), 1);
            assert_eq!(a.selected_pid, Some(v[1].pid));
            a.move_selection(&mut AutoCommit::new(), -1);
            assert_eq!(a.selected_pid, Some(v[0].pid));
        }

        // Phase `selected_detail_always_refers_to_a_visible_tree_row`.
        {
            let mut a = app();
            draw_at(&mut a, 1_280.0, 720.0);
            let selected = a.selected_pid.expect("sample selects a visible row");
            assert!(
                a.hit.rows.iter().any(|(pid, _)| *pid == selected),
                "a selected detail must not expose actions for a collapsed descendant"
            );

            // Collapsing a parent that contains the selection clears that stale
            // detail rather than leaving hidden Stop/Kill controls behind.
            let parent = a
                .model
                .processes
                .iter()
                .find(|process| process.ppid.is_none() && a.row_has_children(process.pid))
                .expect("sample has a tree parent")
                .pid;
            a.toggle_expand(parent);
            let child = a
                .model
                .processes
                .iter()
                .find(|process| process.ppid == Some(parent))
                .expect("parent has a child")
                .pid;
            a.selected_pid = Some(child);
            a.toggle_expand(parent);
            assert_eq!(a.selected_pid, None);
        }
    }

    /// Stop/Kill controls (detail bar, buttons, confirm modal) queue canonical
    /// resource addresses under the typed confirmed contract.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn stop_and_kill_controls_queue_canonical_confirmed_invocations() {
        // Phase `kill_removes_the_row_and_clears_selection`.
        {
            let mut a = app();
            let pid = a.selected_pid.unwrap();
            let started_at = a
                .model
                .processes
                .iter()
                .find(|process| process.pid == pid)
                .unwrap()
                .started_at;
            let before = a.model.processes.len();
            assert!(a.do_stop(pid, started_at, StopKind::Kill));
            assert_eq!(a.model.processes.len(), before - 1);
            assert!(a.selected_pid.is_none());
            assert!(a.model.processes.iter().all(|p| p.pid != pid));
        }

        // Phase `stop_and_kill_controls_queue_canonical_addresses`.
        // Stop and Kill controls queue the same canonical resource addresses the
        // host resolves for palette and agent execution.
        {
            let mut a = app();
            let pid = a.visible_rows()[0].pid;
            a.request_process_control(pid, "stop");
            assert_eq!(
                ui::widget::controls::take_control_invocation(&mut a)
                    .map(|invocation| invocation.address),
                Some(ActionAddress::new(resource_id_for(&a, pid), "stop"))
            );
            a.request_process_control(pid, "kill");
            assert_eq!(
                ui::widget::controls::take_control_invocation(&mut a)
                    .map(|invocation| invocation.address),
                Some(ActionAddress::new(resource_id_for(&a, pid), "kill"))
            );
        }

        // Phase `force_kill_contract_does_not_follow_local_dialog_preference`.
        {
            let mut a = app();
            let pid = a.selected_pid.unwrap();
            a.prefs.confirm_kill = true;
            a.prefs.confirm_kill = false;
            let resources = ui::resources::resources(&a, &AutoCommit::new());
            let process = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == resource_id_for(&a, pid))
                .expect("process resource");
            assert_eq!(
                process
                    .actions
                    .iter()
                    .find(|action| action.id == "kill")
                    .expect("kill action")
                    .contract
                    .confirmation,
                ConfirmationPolicy::Confirm
            );
        }

        // Phase `detail_kill_button_sits_in_reserved_action_bar`.
        // The narrow detail pane reserves a bottom action bar so the Kill button
        // can never overlap the stat content — the exact collision the rework
        // fixes. Asserted against the real drawn hit rects at a narrow, tall
        // viewport with the detail pane pushed in: the buttons sit inside the
        // reserved band pinned to the pane bottom, and don't overlap each other.
        {
            let mut a = app();
            a.proc_push.snap(PushPane::Detail);
            draw_at(&mut a, 480.0, 820.0);
            let kill = a.hit.kill_btn;
            let stop = a.hit.stop_btn;
            assert!(
                kill.area() > 0.0,
                "the Kill button is drawn in the pushed detail pane"
            );
            assert!(stop.area() > 0.0, "the Stop button is drawn too");
            // The two action buttons don't overlap each other.
            assert!(!rects_overlap(stop, kill), "Stop and Kill must not overlap");
            assert!(
                stop.x1 <= kill.x0 + 0.5,
                "Stop ({stop:?}) must sit left of Kill ({kill:?})"
            );
            // The pane's inner bottom = window − outer pad − panel pad. The buttons
            // live entirely inside the reserved `DETAIL_ACTION_H` band pinned there,
            // so all stat content laid out above the band is clear of them.
            let pane_bottom = 820.0 - OUTER_PAD - 12.0;
            assert!(
                kill.y1 <= pane_bottom + 1.0,
                "Kill bottom ({}) stays inside the pane ({pane_bottom})",
                kill.y1
            );
            assert!(
                kill.y0 >= pane_bottom - DETAIL_ACTION_H - 1.0,
                "Kill top ({}) stays within the reserved action band (>= {})",
                kill.y0,
                pane_bottom - DETAIL_ACTION_H
            );
        }

        // Phase `kill_button_click_queues_canonical_control`.
        // Clicking the detail-strip Kill button queues the canonical resource
        // action rather than sending a process signal from the paint surface.
        {
            let mut a = app();
            a.prefs.confirm_kill = true;
            let pid = a.selected_pid.unwrap();
            a.hit.kill_btn = Rect::new(0.0, 0.0, 96.0, 32.0);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h = false;
            a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
            assert!(h);
            assert_eq!(
                ui::widget::controls::take_control_invocation(&mut a)
                    .map(|invocation| invocation.address),
                Some(ActionAddress::new(resource_id_for(&a, pid), "kill"))
            );
        }

        // Phase `confirm_modal_buttons_confirm_and_cancel`.
        // The shared confirm dialog's destructive primary dispatches the signal;
        // its secondary (Cancel) dismisses without killing — both driven through
        // the real `handle_event` → `ModalDialog` path (footer button rects
        // resolved by a live layout pass, then a click at their centre).
        {
            let mut a = app();
            let pid = a.visible_rows()[0].pid;
            a.selected_pid = Some(pid);
            let started_at = a
                .model
                .processes
                .iter()
                .find(|process| process.pid == pid)
                .unwrap()
                .started_at;
            a.confirm_stop = Some((pid, started_at, StopKind::Kill));
            // A real frame builds + lays out the shared dialog so its footer button
            // rects resolve exactly as the live renderer would.
            draw_at(&mut a, 900.0, 600.0);
            let primary = a.confirm_dialog.as_ref().unwrap().dialog.primary_rect();
            dispatch_ev(
                &mut a,
                &synth_cursor(primary.center().x, primary.center().y),
            );
            let h = dispatch_ev(&mut a, &synth_press());
            assert!(h);
            assert!(a.confirm_stop.is_none(), "modal dismissed after confirm");
            assert!(
                a.model.processes.iter().all(|p| p.pid != pid),
                "process killed on confirm"
            );

            let mut b = app();
            let pid2 = b.visible_rows()[0].pid;
            let started_at2 = b
                .model
                .processes
                .iter()
                .find(|process| process.pid == pid2)
                .unwrap()
                .started_at;
            b.confirm_stop = Some((pid2, started_at2, StopKind::Kill));
            draw_at(&mut b, 900.0, 600.0);
            let secondary = b.confirm_dialog.as_ref().unwrap().dialog.secondary_rect();
            dispatch_ev(
                &mut b,
                &synth_cursor(secondary.center().x, secondary.center().y),
            );
            let h2 = dispatch_ev(&mut b, &synth_press());
            assert!(h2);
            assert!(b.confirm_stop.is_none(), "Cancel dismissed the modal");
            assert!(
                b.model.processes.iter().any(|p| p.pid == pid2),
                "Cancel did not kill"
            );
        }

        // Phase `confirm_modal_keyboard_enter_confirms_escape_cancels`.
        // The confirm dialog's keyboard: Enter confirms the kill, Escape cancels —
        // both through the real `handle_event` → `ModalDialog` routing.
        {
            let mut a = app();
            let pid = a.visible_rows()[0].pid;
            let started_at = a
                .model
                .processes
                .iter()
                .find(|process| process.pid == pid)
                .unwrap()
                .started_at;
            a.confirm_stop = Some((pid, started_at, StopKind::Kill));
            let h = dispatch_ev(&mut a, &synth_key(NamedKey::Enter));
            assert!(h);
            assert!(a.confirm_stop.is_none());
            assert!(
                a.model.processes.iter().all(|p| p.pid != pid),
                "Enter confirmed the kill"
            );

            let mut b = app();
            let pid2 = b.visible_rows()[0].pid;
            let started_at2 = b
                .model
                .processes
                .iter()
                .find(|process| process.pid == pid2)
                .unwrap()
                .started_at;
            b.confirm_stop = Some((pid2, started_at2, StopKind::Kill));
            let h2 = dispatch_ev(&mut b, &synth_key(NamedKey::Escape));
            assert!(h2);
            assert!(b.confirm_stop.is_none());
            assert!(
                b.model.processes.iter().any(|p| p.pid == pid2),
                "Escape cancelled the kill"
            );
        }
    }

    // ---- responsive layout fixes (issues 1-5) ---------------------------

    /// Responsive layout fixes (issues 1-5): the resources page fits every core at
    /// every count and text scale, grows and scrolls, and page scroll clamps/resets.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn resources_page_fits_every_core_and_scrolls_within_its_overflow() {
        // Phase `per_core_grid_fits_every_core_for_varied_counts`.
        // Issue 4: the per-core card the renderer sizes (`resources_natural_h`
        // uses `per_core_card_h`) must hold a cell for *every* core in the
        // model, for a range of core counts — not just the default 8. Assert on
        // the production grid the draw path computes, so the last cores can
        // never land below the card background.
        {
            for n in [1usize, 4, 8, 16, 32, 48] {
                let mut a = app();
                a.model.metrics.cores = (0..n).map(|i| (i as f32 * 3.0) % 100.0).collect();
                // The grid the renderer lays out (same width math as draw_resources).
                let w = 900.0;
                let grid = layout::per_core_grid(
                    a.model.metrics.cores.len(),
                    w - 2.0 * layout::RAIL_CARD_PAD,
                );
                assert!(
                    grid.cells() >= n,
                    "grid must hold every core: n={n} cells={}",
                    grid.cells()
                );
                // The card the renderer sizes holds the last grid row inside it.
                let card_h = layout::per_core_card_h(grid);
                let last_row_bottom =
                    layout::CORE_TITLE_H + grid.rows as f64 * layout::CORE_ROW_STRIDE;
                assert!(
                    last_row_bottom <= card_h + 0.001,
                    "n={n}: last core row {last_row_bottom} spills past card {card_h}"
                );
            }
        }

        // Phase `large_text_reflows_per_core_gauges_and_expands_page_height`.
        {
            let mut a = app();
            a.model.metrics.cores = vec![10.0; 8];
            let width = 1_252.0;
            let normal_h = a.resources_natural_h(width);
            a.layout_text_scale = 2.0;
            let large_grid = layout::per_core_grid(
                a.model.metrics.cores.len(),
                (width - 2.0 * layout::RAIL_CARD_PAD) / a.layout_text_scale,
            );

            assert!(
                large_grid.rows > 1,
                "200% text must reflow gauges instead of overlapping one wide row"
            );
            assert!(
                a.resources_natural_h(width) > normal_h,
                "the page must grow and scroll to contain enlarged gauge rows"
            );
        }

        // Phase `resources_height_grows_with_core_count`.
        // Issue 4 + 5: more cores ⇒ a taller Resources body. The natural height
        // the page-scroll clamps against must grow with the core count, proving
        // the grid drives the page height rather than being clipped.
        {
            let mut a8 = app();
            a8.model.metrics.cores = vec![10.0; 8];
            let mut a64 = app();
            a64.model.metrics.cores = vec![10.0; 64];
            assert!(
                a64.resources_natural_h(900.0) > a8.resources_natural_h(900.0),
                "64-core Resources body must be taller than 8-core"
            );
        }

        // Phase `short_viewport_makes_resources_page_scrollable`.
        // Issue 5: on a short viewport the Resources content exceeds the body,
        // so `content_natural_h` reports more than the viewport height — the
        // signal the page scrollbar + wheel routing key off.
        {
            let mut a = app();
            a.tab = Tab::Resources;
            a.model.metrics.cores = vec![10.0; 32];
            let main_w = 900.0;
            let natural = a.content_natural_h(main_w);
            let short_viewport = 300.0;
            assert!(
                natural > short_viewport,
                "32-core Resources ({natural}px) must overflow a {short_viewport}px viewport"
            );
        }

        // Phase `page_scroll_clamps_to_overflow`.
        // Issue 5: page-scroll offset clamps to the overflow — it can't scroll
        // past the content or above the top. Drives the same clamp the wheel
        // handler uses.
        {
            let mut a = app();
            a.content_h = 1000.0;
            a.viewport_h = 400.0;
            let max_y = (a.content_h - a.viewport_h) as f32; // 600
            // Over-scroll down is clamped to max.
            a.view_scroll_y = (a.view_scroll_y + 5000.0).clamp(0.0, max_y);
            assert_eq!(a.view_scroll_y, 600.0);
            // Over-scroll up is clamped to 0.
            a.view_scroll_y = (a.view_scroll_y - 5000.0).clamp(0.0, max_y);
            assert_eq!(a.view_scroll_y, 0.0);
        }

        // Phase `clicking_a_tab_resets_page_scroll`.
        // Switching tabs via the production click path resets the page scroll so
        // the new tab opens at the top rather than inheriting a stale offset.
        {
            let mut a = app();
            a.tab = Tab::Resources;
            a.view_scroll_y = 250.0;
            // Seat the Processes tab hit rect and click inside it via on_click.
            a.hit.tab_proc = Rect::new(0.0, 0.0, 100.0, 30.0);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut handled = false;
            a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut handled);
            assert!(handled);
            assert_eq!(a.tab, Tab::Processes);
            assert_eq!(
                a.view_scroll_y, 0.0,
                "tab switch must reset the page scroll"
            );
        }
    }

    /// Toolbar, summary strip and table keep every control visible and
    /// non-overlapping from wide to very narrow widths.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn table_columns_and_toolbar_search_fit_down_to_very_narrow_widths() {
        // Phase `narrow_table_keeps_primary_columns_in_view`.
        // Narrow process windows switch to a compact table rather than asking the
        // user to scroll sideways to identify the busy process.
        {
            let mut a = app();
            draw_at(&mut a, 360.0, 700.0);
            assert!(
                a.table_content_w <= a.table_cols_w + 0.5,
                "compact table must fit its visible band ({} vs {})",
                a.table_content_w,
                a.table_cols_w
            );
            assert!(
                a.hit.h_scrollbar.is_none(),
                "primary data needs no h-scroll"
            );
        }

        // Phase `toolbar_search_never_overlaps_pills`.
        // Defect 1: at no width — down to well below the narrow breakpoint — may
        // the search box overlap any filter pill. Overlapping hit rects would make
        // a pill unclickable (a click in the shared area is consumed as search),
        // so this asserts the real hit rectangles the click path uses are
        // pairwise disjoint across a width sweep, on the production draw path.
        {
            for w in [
                1280.0, 1024.0, 900.0, 760.0, 700.0, 640.0, 560.0, 480.0, 420.0,
            ] {
                let mut a = app();
                draw_at(&mut a, w, 720.0);
                let search = a.hit.search;
                for (name, pill) in [
                    ("tree", a.hit.tree_toggle),
                    ("sum", a.hit.sum_toggle),
                    ("kernel", a.hit.kernel_toggle),
                    ("confirm", a.hit.confirm_toggle),
                ] {
                    if pill == Rect::ZERO {
                        continue; // sum pill is absent in flat mode; confirm is dropped at extreme narrow
                    }
                    assert!(
                        !overlaps(search, pill),
                        "w={w}: search {search:?} overlaps {name} pill {pill:?}"
                    );
                }
            }
        }

        // Phase `toolbar_stacks_on_very_narrow_widths`.
        // Defect 1: once the search box can no longer sit beside the pills, the
        // toolbar stacks — the search box moves to its own row *above* the pills,
        // and the toolbar grows to two rows. Proven on the real draw path.
        {
            let mut a = app();
            draw_at(&mut a, 440.0, 720.0);
            // The pills and the search box are on different rows (search above).
            assert!(
                a.hit.search.y1 <= a.hit.tree_toggle.y0 + 0.5,
                "stacked toolbar puts search above the pills: search {:?} tree {:?}",
                a.hit.search,
                a.hit.tree_toggle
            );
            // And the toolbar reports a two-row height.
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            let content_w = 440.0 - 2.0 * OUTER_PAD;
            assert!(
                a.toolbar_height(&mut tr, &theme, content_w) > TOOLBAR_ROW_H + 1.0,
                "narrow toolbar must be two rows tall"
            );
            // A wide toolbar stays a single row.
            assert_eq!(
                a.toolbar_height(&mut tr, &theme, 1252.0),
                TOOLBAR_ROW_H,
                "wide toolbar is a single row"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn compact_toolbar_summary_strip_and_confirm_pill_stay_laid_out() {
        // Phase `compact_toolbar_keeps_every_visible_control_on_screen`.
        // Below the width where all labelled filters fit on one line, controls
        // get their own structural rows. Their real hit rectangles must remain
        // visible and disjoint; a clipped or overlapping filter is functionally
        // unavailable even if its pixels happen to be painted.
        {
            for w in [320.0, 360.0, 400.0] {
                let mut a = app();
                draw_at(&mut a, w, 720.0);
                let content = Rect::new(OUTER_PAD, 0.0, w - OUTER_PAD, 720.0);
                assert!(
                    a.hit.confirm_toggle != Rect::ZERO,
                    "w={w}: the persisted Confirm kills preference must stay reachable"
                );
                let controls = [
                    ("search", a.hit.search),
                    ("tree", a.hit.tree_toggle),
                    ("sum", a.hit.sum_toggle),
                    ("kernel", a.hit.kernel_toggle),
                    ("confirm", a.hit.confirm_toggle),
                ];
                for (name, r) in controls {
                    if r == Rect::ZERO {
                        continue;
                    }
                    assert!(
                        r.x0 >= content.x0 - 0.5 && r.x1 <= content.x1 + 0.5,
                        "w={w}: {name} is outside the content band: {r:?}"
                    );
                }
                for (i, (left_name, left)) in controls.iter().enumerate() {
                    if *left == Rect::ZERO {
                        continue;
                    }
                    for (right_name, right) in controls.iter().skip(i + 1) {
                        if *right == Rect::ZERO {
                            continue;
                        }
                        assert!(
                            !overlaps(*left, *right),
                            "w={w}: {left_name} overlaps {right_name}"
                        );
                    }
                }
            }
        }

        // Phase `summary_strip_stays_one_row_and_table_keeps_height`.
        // The summary strip's cards grow with the window width but stay on a
        // **single row** at every width — so narrowing the window can never steal
        // vertical height from the process table below. Critically the width must
        // keep re-deriving on resize of the SAME instance (not freeze at the first
        // frame). Drives the real draw path, reading back the card hit rects it
        // records, and asserts the table keeps its height across the narrowing.
        {
            let mut a = app();

            // Wide: four cards abreast on one row.
            draw_at(&mut a, 1280.0, 720.0);
            let wide = a.hit.summary_cells.clone();
            assert_eq!(wide.len(), 4, "four summary cards");
            assert_eq!(distinct_rows(&wide), 1, "four abreast when wide");
            let wide_card_w = wide[0].width();
            let wide_table_h = a.table_viewport.height();
            // The four cards fill the content width (within one gap of it).
            let content_w = 1280.0 - 2.0 * OUTER_PAD;
            let filled = wide.iter().map(vello::kurbo::Rect::width).sum::<f64>() + 3.0 * GAP;
            assert!(
                (filled - content_w).abs() < 2.0,
                "cards fill the width: {filled} vs {content_w}"
            );

            // Same instance, narrower (800px): the cards must re-derive a SMALLER
            // width from the new allocation — the "grows with width" property. A
            // frozen-at-first-frame layout would keep the wide card width and fail.
            draw_at(&mut a, 800.0, 720.0);
            let mid = a.hit.summary_cells.clone();
            assert_eq!(mid.len(), 4);
            assert_eq!(distinct_rows(&mid), 1, "still four abreast at 800px");
            assert!(
                mid[0].width() < wide_card_w - 1.0,
                "cards shrank with the window (grow-with-width): {} !< {}",
                mid[0].width(),
                wide_card_w
            );

            // Phone-narrow (480px): the strip STAYS one row (the fix — it must not
            // wrap to a second row and steal the table's height), and the process
            // table must not collapse vertically — at the same window height it is
            // at least as tall as in the wide layout (the wide layout even reserves
            // a detail strip, so the narrow table has more, not less, room).
            draw_at(&mut a, 480.0, 720.0);
            let narrow = a.hit.summary_cells.clone();
            assert_eq!(narrow.len(), 4);
            assert_eq!(
                distinct_rows(&narrow),
                1,
                "strip stays a single row when narrow"
            );
            assert!(
                a.table_viewport.height() >= wide_table_h - 1.0,
                "the table must keep its height at narrow widths (narrow {} vs wide {})",
                a.table_viewport.height(),
                wide_table_h
            );

            // Widening straight back keeps one row — no frozen mode — and the card
            // width returns to its wide value.
            draw_at(&mut a, 1280.0, 720.0);
            assert_eq!(
                distinct_rows(&a.hit.summary_cells),
                1,
                "one row at every width"
            );
            assert!(
                (a.hit.summary_cells[0].width() - wide_card_w).abs() < 1.0,
                "card width returns to its wide value after resize round-trip"
            );
        }

        // Phase `confirm_pill_is_laid_out_on_wide_toolbar`.
        // The confirm pill is actually laid out on the real draw path at a normal
        // width (not just a hand-seated rect).
        {
            let mut a = app();
            draw_at(&mut a, 1280.0, 720.0);
            assert_ne!(
                a.hit.confirm_toggle,
                Rect::ZERO,
                "confirm pill drawn at a normal width"
            );
            assert!(
                !overlaps(a.hit.confirm_toggle, a.hit.search),
                "confirm pill clears the search box"
            );
            assert!(
                !overlaps(a.hit.confirm_toggle, a.hit.tree_toggle),
                "confirm pill clears the tree pill"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn keyboard_routes_search_editing_and_leaves_global_keys_to_the_toolkit() {
        // Phase `ctrl_k_is_left_unhandled_for_global_search`.
        // Issue 6: Ctrl+K is left for the global-search framework — `on_key`
        // must NOT consume it (so it bubbles up), while a bare "/" still focuses
        // the local process filter.
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let key = Key::Character("k".into());
            let mut handled = false;
            a.on_key(&key, Some("k"), true, &mut doc, &root, &mut handled);
            assert!(!handled, "Ctrl+K must bubble up to the global-search seam");
            // A bare "/" (no ctrl) still drives the local filter focus.
            let slash = Key::Character("/".into());
            let mut handled2 = false;
            a.on_key(&slash, Some("/"), false, &mut doc, &root, &mut handled2);
            assert!(
                handled2 && a.search_input.is_focused(),
                "bare / still focuses local search"
            );
        }

        // Phase `tab_key_is_left_for_toolkit_focus_navigation`.
        {
            let mut a = app();
            let original = a.tab;
            let mut handled = false;
            a.on_key(
                &Key::Named(NamedKey::Tab),
                None,
                false,
                &mut AutoCommit::new(),
                &automerge::ROOT,
                &mut handled,
            );
            assert_eq!(a.tab, original, "Tab must not silently switch dashboards");
            assert!(!handled, "the toolkit owns Tab focus traversal");
        }

        // Phase `search_click_focus_and_keyboard_editing_filters`.
        // Clicking the search box focuses it, and the keyboard editing path
        // (chars, Space, Backspace, Escape) drives the real filter.
        {
            let mut a = app();
            a.hit.search = Rect::new(0.0, 0.0, 300.0, 30.0);
            dispatch_ev(&mut a, &synth_cursor(50.0, 15.0));
            let h = dispatch_ev(&mut a, &synth_press());
            assert!(
                h && a.search_input.is_focused(),
                "clicking the search box focuses its shared editor"
            );
            for ch in "brow".chars() {
                let s = ch.to_string();
                let hk = dispatch_ev(&mut a, &synth_text(&s));
                assert!(hk, "typed char handled");
            }
            assert_eq!(a.search_input.value(), "brow");
            assert!(dispatch_ev(&mut a, &synth_text(" ")));
            assert_eq!(a.search_input.value(), "brow ");
            assert!(dispatch_ev(&mut a, &synth_key(NamedKey::Backspace)));
            assert_eq!(a.search_input.value(), "brow");
            assert!(
                a.visible_processes()
                    .iter()
                    .all(|p| p.name.to_lowercase().contains("brow")
                        || p.user.to_lowercase().contains("brow")
                        || p.pid.to_string().contains("brow")),
                "typed filter actually narrows the list"
            );
            let hesc = dispatch_ev(&mut a, &synth_key(NamedKey::Escape));
            assert!(
                hesc && !a.search_input.is_focused(),
                "Escape leaves the search box"
            );
        }
    }

    // ---- tree, sum, context menu -----------------------------------------

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn process_tree_collapses_expands_sums_and_toggles_mode() {
        // Phase `tree_defaults_collapsed_summed`.
        // The tree defaults on, Sum subtree by default, and collapsed — so the
        // first frame shows parents carrying their full subtree totals.
        {
            let a = app();
            assert!(a.tree_mode, "tree on by default");
            assert_eq!(a.sum_mode, SumMode::SumSubtree, "Sum is the default");
            assert!(a.expanded.is_empty(), "collapsed by default");
            // A collapsed parent (shell) shows MORE than its own CPU because it
            // sums its subtree.
            let rows = a.visible_rows();
            let shell = rows.iter().find(|r| r.name == "shell").unwrap();
            let own = a
                .model
                .processes
                .iter()
                .find(|p| p.name == "shell")
                .unwrap()
                .cpu;
            assert!(
                shell.cpu > own + 1.0,
                "collapsed shell {} sums subtree (own {own})",
                shell.cpu
            );
            assert!(shell.has_children && !shell.expanded);
        }

        // Phase `clicking_twisty_expands_and_children_appear`.
        // Clicking a twisty (via the production `on_click` path) expands the node
        // and its children appear in the next `visible_rows`.
        {
            let mut a = app();
            let shell_pid = a
                .model
                .processes
                .iter()
                .find(|p| p.name == "shell")
                .unwrap()
                .pid;
            // Seat the twisty hit rect the renderer would record and click it.
            a.hit.twisties = vec![(shell_pid, Rect::new(0.0, 0.0, 16.0, 22.0))];
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut handled = false;
            let before = a.visible_rows().len();
            a.on_click(Point::new(8.0, 11.0), &mut doc, &root, &mut handled);
            assert!(handled, "twisty click consumed");
            assert!(a.expanded.contains(&shell_pid), "shell expanded");
            let rows = a.visible_rows();
            assert!(rows.len() > before, "expanding reveals children");
            // A direct child of shell (e.g. browser) now appears at depth 1.
            let browser = rows.iter().find(|r| r.name == "browser");
            assert!(browser.is_some(), "browser child visible after expand");
            assert_eq!(browser.unwrap().depth, 1, "child indented one level");
        }

        // Phase `toggling_sum_changes_parent_total`.
        // Toggling Sum→Per-process changes a collapsed parent's displayed total
        // (its summed subtree usage drops to its own usage). Drives the exact
        // production toggle path `on_click` runs for the sum pill.
        {
            let mut a = app();
            let shell_summed = {
                let rows = a.visible_rows();
                rows.iter().find(|r| r.name == "shell").unwrap().cpu
            };
            // Click the sum-toggle pill via on_click.
            a.hit.sum_toggle = Rect::new(0.0, 0.0, 120.0, 30.0);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut handled = false;
            a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut handled);
            assert!(handled);
            assert_eq!(a.sum_mode, SumMode::PerProcess);
            let shell_own = a
                .visible_rows()
                .iter()
                .find(|r| r.name == "shell")
                .unwrap()
                .cpu;
            assert!(
                shell_summed > shell_own + 1.0,
                "summed total {shell_summed} must exceed per-process {shell_own}"
            );
            let real_own = a
                .model
                .processes
                .iter()
                .find(|p| p.name == "shell")
                .unwrap()
                .cpu;
            assert!(
                (shell_own - real_own).abs() < 1e-3,
                "per-process == own usage"
            );
        }

        // Phase `tree_flat_pill_click_toggles_mode`.
        // Clicking the Tree/Flat pill toggles the view; flat rows are all depth 0.
        {
            let mut a = app();
            assert!(a.tree_mode);
            a.hit.tree_toggle = Rect::new(0.0, 0.0, 80.0, 30.0);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h = false;
            a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
            assert!(h);
            assert!(!a.tree_mode, "toggled to flat");
            assert!(
                a.visible_rows().iter().all(|r| r.depth == 0),
                "flat rows carry no tree depth"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn context_menu_opens_per_row_and_queues_canonical_actions() {
        // Phase `right_click_opens_context_menu_for_row`.
        // A right-click over a process row opens the action context menu anchored
        // at the cursor, targeting that row's pid.
        {
            let mut a = app();
            a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let pid = a.model.processes[5].pid;
            a.hit.rows = vec![(pid, Rect::new(100.0, 200.0, 600.0, 222.0))];
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut handled = false;
            a.on_right_click(Point::new(150.0, 210.0), &mut doc, &root, &mut handled);
            assert!(handled, "right-click on a row consumed");
            assert!(a.context.is_open(), "menu opened");
            assert_eq!(a.context_pid, Some(pid), "menu targets the right pid");
            // The menu has the expected actions (5 items incl. a separator).
            let labels: Vec<String> = a
                .context
                .items()
                .iter()
                .filter_map(|i| i.label().map(std::string::ToString::to_string))
                .collect();
            assert!(
                labels.iter().any(|l| l.starts_with("Stop")),
                "has Stop: {labels:?}"
            );
            assert!(
                labels.iter().any(|l| l.starts_with("Kill")),
                "has Kill: {labels:?}"
            );
            assert!(labels.iter().any(|l| l == "Copy PID"), "has Copy PID");
            // On a WIDE window the detail strip is always visible and the
            // right-click already selected the row, so "Show details" is a no-op
            // and is deliberately omitted (it would otherwise be a dead action).
            assert!(
                !labels.iter().any(|l| l == "Show details"),
                "Show details is omitted on wide (it would be a no-op): {labels:?}"
            );
        }

        // Phase `context_menu_stop_queues_canonical_control`.
        // Clicking the "Stop" item in the open menu queues the canonical action
        // address; the host owns the actual system effect.
        {
            let mut a = app();
            a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let pid = a.model.processes[5].pid;
            a.hit.rows = vec![(pid, Rect::new(100.0, 200.0, 600.0, 222.0))];
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h = false;
            a.on_right_click(Point::new(150.0, 210.0), &mut doc, &root, &mut h);
            // Find the "Stop" item's rect from the production geometry and click it.
            let stop_idx = a
                .context
                .items()
                .iter()
                .position(|i| i.label().is_some_and(|l| l.starts_with("Stop")))
                .unwrap();
            let rect = a
                .context_menu_item_rects()
                .into_iter()
                .find(|(idx, _)| *idx == stop_idx)
                .map(|(_, r)| r)
                .unwrap();
            let mut h2 = false;
            a.on_click(rect.center(), &mut doc, &root, &mut h2);
            assert!(h2, "menu item click consumed");
            assert!(!a.context.is_open(), "menu closed after action");
            assert_eq!(
                ui::widget::controls::take_control_invocation(&mut a)
                    .map(|invocation| invocation.address),
                Some(ActionAddress::new(resource_id_for(&a, pid), "stop"))
            );
        }

        // Phase `context_menu_kill_queues_confirmed_contract`.
        // Clicking "Kill" in the menu queues the same force-kill action whose
        // resource contract requires host-owned confirmation.
        {
            let mut a = app();
            a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            a.prefs.confirm_kill = true;
            let pid = a.model.processes[5].pid;
            a.hit.rows = vec![(pid, Rect::new(100.0, 200.0, 600.0, 222.0))];
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h = false;
            a.on_right_click(Point::new(150.0, 210.0), &mut doc, &root, &mut h);
            let kill_idx = a
                .context
                .items()
                .iter()
                .position(|i| i.label().is_some_and(|l| l.starts_with("Kill")))
                .unwrap();
            let rect = a
                .context_menu_item_rects()
                .into_iter()
                .find(|(idx, _)| *idx == kill_idx)
                .map(|(_, r)| r)
                .unwrap();
            let mut h2 = false;
            a.on_click(rect.center(), &mut doc, &root, &mut h2);
            assert_eq!(
                ui::widget::controls::take_control_invocation(&mut a)
                    .map(|invocation| invocation.address),
                Some(ActionAddress::new(resource_id_for(&a, pid), "kill"))
            );
        }

        // Phase `context_menu_copy_pid_queues_canonical_resource_action`.
        // Copy PID queues the stable resource action; the broker then performs
        // the clipboard effect after resolving the current process generation.
        {
            let mut a = app();
            a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let pid = a.model.processes[3].pid;
            a.hit.rows = vec![(pid, Rect::new(100.0, 200.0, 600.0, 222.0))];
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h = false;
            a.on_right_click(Point::new(150.0, 210.0), &mut doc, &root, &mut h);
            let idx = a
                .context
                .items()
                .iter()
                .position(|i| i.label() == Some("Copy PID"))
                .unwrap();
            let rect = a
                .context_menu_item_rects()
                .into_iter()
                .find(|(i, _)| *i == idx)
                .map(|(_, r)| r)
                .unwrap();
            let mut h2 = false;
            a.on_click(rect.center(), &mut doc, &root, &mut h2);
            assert_eq!(
                ui::widget::controls::take_control_invocation(&mut a)
                    .map(|invocation| invocation.address),
                Some(ActionAddress::new(resource_id_for(&a, pid), "copy-pid"))
            );
        }

        // Phase `context_menu_show_details_pushes_detail_on_narrow`.
        // On a narrow window the context menu offers "Show details", and clicking
        // it pushes the detail pane for the target row.
        {
            let mut a = app();
            a.tree_mode = false;
            draw_at(&mut a, 520.0, 900.0);
            assert!(a.is_narrow());
            let (pid, rect) = a.hit.rows.first().copied().expect("rows");
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut hr = false;
            a.on_right_click(rect.center(), &mut doc, &root, &mut hr);
            assert!(a.context.is_open());
            let idx = a
                .context
                .items()
                .iter()
                .position(|i| i.label() == Some("Show details"))
                .expect("Show details present on narrow");
            let mrect = a
                .context_menu_item_rects()
                .into_iter()
                .find(|(i, _)| *i == idx)
                .map(|(_, r)| r)
                .unwrap();
            let mut hc = false;
            a.on_click(mrect.center(), &mut doc, &root, &mut hc);
            assert!(hc);
            assert_eq!(a.selected_pid, Some(pid));
            assert_eq!(
                a.proc_push.pane(),
                PushPane::Detail,
                "Show details pushes the detail pane"
            );
        }
    }

    // ---- responsive toolbar + summary strip (the reproduced defects) -----

    /// True when two rects share any area.
    fn overlaps(a: Rect, b: Rect) -> bool {
        a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
    }

    /// Count of distinct (rounded) row tops among a set of rects.
    fn distinct_rows(rects: &[Rect]) -> usize {
        let mut ys: Vec<i64> = rects.iter().map(|r| r.y0.round() as i64).collect();
        ys.sort_unstable();
        ys.dedup();
        ys.len()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn live_sampling_reads_real_processes_on_a_deadline() {
        // Phase `live_constructor_samples_real_processes`.
        // The live window must show REAL data, not the fabricated sample. The live
        // constructor starts empty and its first refresh pulls actual `sysinfo`
        // readings — proven by the running test process appearing in the table.
        {
            let mut a = MonitorApp::new_live();
            // Nothing fabricated before the first frame.
            assert!(
                a.model.processes.is_empty(),
                "live app starts empty, not seeded with sample rows"
            );
            assert!(a.selected_pid.is_none(), "no pre-selected fake process");
            // The first refresh samples the real system.
            a.maybe_refresh();
            assert!(
                !a.model.processes.is_empty(),
                "first refresh populated real processes"
            );
            assert!(
                a.model
                    .processes
                    .iter()
                    .any(|p| p.pid == std::process::id()),
                "the running process appears in the real table (data is live, not mock)"
            );
            assert!(
                !a.model.metrics.cores.is_empty(),
                "real per-core readings sampled"
            );
        }

        // Phase `idle_sampling_uses_a_deadline_not_continuous_frames`.
        // A live monitor asks the host for one cadence-aligned wake rather than
        // pinning the app to a 60 Hz animation loop while it is otherwise idle.
        {
            let a = MonitorApp::new_live();
            assert!(
                !a.needs_animation_frame(),
                "a static monitor is not animated"
            );
            assert!(
                a.next_wake_deadline().is_some(),
                "the first real sample is scheduled as a bounded wake"
            );
            let sample = app();
            assert!(
                sample.next_wake_deadline().is_none(),
                "deterministic screenshots never schedule live sampling"
            );
        }
    }

    // ---- input-path coverage: click / key / wheel through the real handlers --

    /// Dispatch a real `WindowEvent` through the production `handle_event`
    /// entry point (scale 1.0, no modifiers), returning whether it was handled.
    fn dispatch_ev(a: &mut MonitorApp, ev: &WindowEvent) -> bool {
        let modifiers = winit::event::Modifiers::default();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy::NodeId::new(0);
        let mut handled = false;
        let mut focus = None;
        a.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }
    fn synth_cursor(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }
    fn synth_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn synth_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }
    fn synth_wheel(dx: f32, dy: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(dx, dy),
            phase: winit::event::TouchPhase::Moved,
        }
    }
    /// A synthetic key-press event for a named key. Zeroes the winit
    /// `KeyEvent` (its `platform_specific` field is crate-private) and
    /// overwrites the fields the handlers read — same trick the `ui` widget
    /// tests use.
    fn synth_key(named: NamedKey) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: ev,
            is_synthetic: false,
        }
    }
    fn synth_text(text: &str) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(text.into());
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(text.into());
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: ev,
            is_synthetic: false,
        }
    }

    /// The sampling-interval pill and its popover (pointer, long press, semantic rows,
    /// keyboard) set the cadence locally without touching the shared doc.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn interval_control_cycles_opens_selects_and_persists_the_cadence() {
        // Phase `interval_pill_click_cycles_without_touching_the_shared_doc`.
        // A quick click (press + prompt release) on the interval pill cycles this
        // window's cadence without changing the shared document.
        {
            let mut a = app();
            a.hit.interval = Rect::new(0.0, 0.0, 80.0, 30.0);
            a.prefs.interval_ms = 1000; // land on the cycle grid
            let before = a.prefs.interval_ms;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let heads_before = doc.get_heads();
            let mut h = false;
            // Press: starts the hold timer, does NOT cycle yet.
            a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
            assert!(h, "interval pill consumed the press");
            assert_eq!(a.prefs.interval_ms, before, "a bare press does not cycle");
            // Release before the long-press threshold cycles locally.
            let cycled = a.release_interval_press(&mut doc, &root);
            assert!(cycled, "the pending press was resolved on release");
            assert_ne!(
                a.prefs.interval_ms, before,
                "a quick release cycled the cadence"
            );
            assert_eq!(
                doc.get_heads(),
                heads_before,
                "device cadence is not history"
            );
            assert!(
                !a.interval_popover.is_open(),
                "a quick click never opens the popover"
            );
        }

        // Phase `interval_long_press_opens_the_option_popover`.
        // Holding the interval pill past the long-press threshold opens the option
        // popover (instead of cycling), seeded with every cadence and the current
        // one pre-selected — the real `release_interval_press` long-press path.
        {
            let mut a = app();
            a.rect = Rect::new(0.0, 0.0, 600.0, 400.0);
            a.hit.interval = Rect::new(500.0, 8.0, 560.0, 32.0);
            a.prefs.interval_ms = 2000;
            let before = a.prefs.interval_ms;
            // Simulate a press that started well past the long-press threshold.
            a.interval_press = Some(
                ui::time::now()
                    .checked_sub(Duration::from_millis(LONG_PRESS_MS as u64 + 200))
                    .unwrap(),
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let handled = a.release_interval_press(&mut doc, &root);
            assert!(handled, "the held press was consumed");
            assert!(
                a.interval_popover.is_open(),
                "a long-press opens the popover"
            );
            assert_eq!(
                a.prefs.interval_ms, before,
                "opening the popover does not cycle"
            );
            assert_eq!(
                a.interval_popover.items().len(),
                INTERVAL_OPTIONS.len(),
                "every cadence is offered"
            );
            assert_eq!(
                a.interval_popover.selected(),
                Some(2),
                "the current cadence (2s) is pre-selected"
            );
        }

        // Phase `interval_popover_click_sets_and_persists_the_cadence`.
        // Clicking an option in the open popover applies + persists that cadence
        // and closes the menu — the production `on_click` popover-routing path.
        {
            let mut a = app();
            a.rect = Rect::new(0.0, 0.0, 600.0, 400.0);
            a.hit.interval = Rect::new(500.0, 8.0, 560.0, 32.0);
            a.prefs.interval_ms = 1000;
            a.open_interval_popover();
            assert!(a.interval_popover.is_open());
            // Resolve the body geometry, then click the "3s" row (index 3).
            // Ask the owning primitive for its actual item rect: its private
            // padding/row metrics are intentionally free to evolve without
            // silently retargeting this integration test to a neighbouring value.
            let mut tr = TextRenderer::new();
            a.interval_popover.place(&mut tr, &Theme::dark());
            let click = a
                .interval_popover
                .item_rect(3)
                .expect("the advertised 3s option has a hit rect")
                .center();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h = false;
            a.on_click(click, &mut doc, &root, &mut h);
            assert!(h, "the popover consumed the click");
            assert!(
                !a.interval_popover.is_open(),
                "picking an option closes the popover"
            );
            assert_eq!(
                a.prefs.interval_ms, INTERVAL_OPTIONS[3],
                "the picked cadence applied"
            );
            assert!(
                doc.get_heads().is_empty(),
                "interval choice is device-local"
            );
        }

        // Phase `interval_popover_semantic_rows_match_primitive_geometry`.
        // Screen-reader option bounds must be the Popover's real hit geometry,
        // not a duplicate of its private padding/row constants. This catches the
        // same kind of visual primitive change that previously retargeted the
        // pointer regression from 3s to 2s.
        {
            let mut a = app();
            a.rect = Rect::new(0.0, 0.0, 600.0, 400.0);
            a.hit.interval = Rect::new(500.0, 8.0, 560.0, 32.0);
            a.open_interval_popover();
            let mut tr = TextRenderer::new();
            a.interval_popover.place(&mut tr, &Theme::dark());
            let targets = a.accessibility_targets();
            for (index, ms) in INTERVAL_OPTIONS.iter().copied().enumerate() {
                let target = format!("monitor.interval:{ms}");
                let semantic_row = targets
                    .iter()
                    .find(|(candidate, _, _)| candidate == &target)
                    .map(|(_, rect, _)| *rect)
                    .expect("an open interval menu publishes every option");
                assert_eq!(
                    semantic_row,
                    a.interval_popover
                        .item_rect(index)
                        .expect("the item has painted geometry"),
                    "semantic row {index} is exactly the pointer row"
                );
            }
        }

        // Phase `interval_popover_keyboard_selects_and_captures_input`.
        // Keyboard nav in the open popover: ↓ moves the highlight, Enter applies
        // the highlighted device cadence, and every key is consumed so nothing
        // leaks to the table behind it.
        {
            let mut a = app();
            a.rect = Rect::new(0.0, 0.0, 600.0, 400.0);
            a.hit.interval = Rect::new(500.0, 8.0, 560.0, 32.0);
            a.prefs.interval_ms = 1000; // selected index 0
            a.open_interval_popover();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            // A stray letter must be swallowed, not fall through to a sort shortcut.
            let sort_before = a.sort_key;
            let mut h = false;
            a.on_key(
                &Key::Character("c".into()),
                Some("c"),
                false,
                &mut doc,
                &root,
                &mut h,
            );
            assert!(h, "keys are captured while the popover is open");
            assert_eq!(
                a.sort_key, sort_before,
                "the 'c' sort shortcut did not leak through"
            );

            // ↓ ↓ moves the highlight to index 2, Enter applies it.
            let mut h = false;
            a.on_key(
                &Key::Named(NamedKey::ArrowDown),
                None,
                false,
                &mut doc,
                &root,
                &mut h,
            );
            a.on_key(
                &Key::Named(NamedKey::ArrowDown),
                None,
                false,
                &mut doc,
                &root,
                &mut h,
            );
            assert_eq!(a.interval_popover.highlighted(), 2);
            let mut h = false;
            a.on_key(
                &Key::Named(NamedKey::Enter),
                None,
                false,
                &mut doc,
                &root,
                &mut h,
            );
            assert!(h);
            assert!(!a.interval_popover.is_open(), "Enter applies + closes");
            assert_eq!(
                a.prefs.interval_ms, INTERVAL_OPTIONS[2],
                "the highlighted cadence applied"
            );
            assert!(
                doc.get_heads().is_empty(),
                "keyboard view action is not history"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn toolbar_and_summary_clicks_change_local_view_state() {
        // Phase `kernel_toggle_click_is_local_and_reveals_threads`.
        // Clicking the kernel-threads pill flips the local device pref, and the
        // table now lists kernel threads — the whole `on_click` path, not a direct
        // field poke.
        {
            let mut a = app();
            assert!(a.visible_processes().iter().all(|p| !p.kernel));
            a.hit.kernel_toggle = Rect::new(0.0, 0.0, 160.0, 30.0);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h = false;
            a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
            assert!(h);
            assert!(a.prefs.show_kernel_threads, "toggled on");
            assert!(
                doc.get_heads().is_empty(),
                "kernel filter is not shared state"
            );
            assert!(
                a.visible_processes().iter().any(|p| p.kernel),
                "kernel threads now visible"
            );
        }

        // Phase `sort_header_click_sets_sort_key`.
        // Clicking a sort-header sets that column as the sort key (numeric columns
        // default descending) — the `on_click` → `set_sort` path.
        {
            let mut a = app();
            a.hit.sort_headers = vec![(SortKey::Mem, Rect::new(0.0, 0.0, 60.0, 26.0))];
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h = false;
            a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
            assert!(h);
            assert!(matches!(a.sort_key, SortKey::Mem));
            assert!(a.sort_desc, "MEM is numeric → defaults descending");
        }

        // Phase `summary_cell_click_switches_and_scrolls_to_that_stat`.
        // Clicking a summary-strip cell switches to Resources AND scrolls that
        // stat's own chart into view — each of the four cells lands on a different
        // scroll offset (not the identical "top of Resources" stub).
        {
            let mut a = app();
            // Narrow + short → the Resources dashboard page-scrolls, so the target
            // offsets are distinct and non-zero.
            draw_at(&mut a, 480.0, 520.0);
            let cells = a.hit.summary_cells.clone();
            assert_eq!(cells.len(), 4, "four summary cells");
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h0 = false;
            a.on_click(cells[0].center(), &mut doc, &root, &mut h0); // CPU
            assert!(h0);
            assert_eq!(a.tab, Tab::Resources);
            let cpu_scroll = a.view_scroll_y;

            // Back to Processes, re-lay, then click the Disk cell.
            a.tab = Tab::Processes;
            a.view_scroll_y = 0.0;
            draw_at(&mut a, 480.0, 520.0);
            let cells = a.hit.summary_cells.clone();
            let mut h3 = false;
            a.on_click(cells[3].center(), &mut doc, &root, &mut h3); // Disk
            assert!(h3);
            assert_eq!(a.tab, Tab::Resources);
            let disk_scroll = a.view_scroll_y;
            assert!(
                disk_scroll > cpu_scroll,
                "Disk cell scrolls lower than CPU: disk={disk_scroll} cpu={cpu_scroll}"
            );
            // The offset matches the production stat-scroll math for this body.
            let (bw, vh) = a.body_metrics();
            assert_eq!(disk_scroll, a.resources_scroll_for_stat(3, bw, vh));
        }

        // Phase `confirm_kills_pill_is_local_and_does_not_ungate_resource_kill`.
        // The Confirm-kills pill changes only this window's preference; the
        // published force-kill action remains host-confirmed regardless.
        {
            let mut a = app();
            assert!(a.prefs.confirm_kill, "confirm defaults on");
            a.hit.confirm_toggle = Rect::new(0.0, 0.0, 140.0, 30.0);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h = false;
            a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
            assert!(h);
            assert!(!a.prefs.confirm_kill, "pill toggled the pref off");
            assert!(
                doc.get_heads().is_empty(),
                "local safety preference is not shared"
            );
            // All visible force-kill routes still enter the host-owned contract.
            let pid = a.visible_rows()[0].pid;
            a.request_process_control(pid, "kill");
            assert_eq!(
                ui::widget::controls::take_control_invocation(&mut a)
                    .map(|invocation| invocation.address),
                Some(ActionAddress::new(resource_id_for(&a, pid), "kill"))
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn narrow_detail_push_and_return_respect_motion_preferences() {
        // Phase `narrow_selecting_row_pushes_detail_pane`.
        // On a narrow window, selecting a row through the real click path pushes
        // the detail pane (the app-level wiring of `PushNav`, not the unit).
        {
            let mut a = app();
            a.tree_mode = false;
            draw_at(&mut a, 520.0, 900.0);
            assert!(a.is_narrow());
            let (pid, rect) = a.hit.rows.first().copied().expect("rows laid out");
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h = false;
            a.on_click(rect.center(), &mut doc, &root, &mut h);
            assert!(h);
            assert_eq!(a.selected_pid, Some(pid));
            assert_eq!(
                a.proc_push.pane(),
                PushPane::Detail,
                "selecting a row pushes the detail pane"
            );
        }

        // Phase `narrow_back_chip_and_escape_return_to_table`.
        // On a narrow window the Back chip click and Esc both pop the pushed
        // detail back to the table — through the real `on_click`/`on_key` paths.
        {
            let mut a = app();
            a.rect = Rect::new(0.0, 0.0, 520.0, 900.0);
            assert!(a.is_narrow());
            a.proc_push.snap(PushPane::Detail);
            a.hit.detail_back = Rect::new(0.0, 0.0, 120.0, 32.0);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h = false;
            a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
            assert!(h);
            assert_eq!(
                a.proc_push.pane(),
                PushPane::Master,
                "Back chip returns to the table"
            );

            a.proc_push.snap(PushPane::Detail);
            let mut he = false;
            a.on_key(
                &Key::Named(NamedKey::Escape),
                None,
                false,
                &mut doc,
                &root,
                &mut he,
            );
            assert!(he);
            assert_eq!(
                a.proc_push.pane(),
                PushPane::Master,
                "Esc pops the pushed detail"
            );
        }

        // Phase `reduced_motion_snaps_a_pending_process_push`.
        {
            let mut a = app();
            a.proc_push.show_detail();
            assert!(a.proc_push.is_animating());
            Widget::set_motion_reduced(&mut a, true);
            assert_eq!(a.proc_push.slide(), 1.0);
            assert!(!a.proc_push.is_animating());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn wheel_and_scrollbars_scroll_rows_and_page() {
        // Phase `wheel_scrolls_rows_and_resources_page`.
        // A vertical wheel over the process table scrolls its rows, and a wheel on
        // the Resources tab scrolls the whole page — both through `handle_event`.
        {
            // Rows: many flat rows in a modest table so they overflow.
            let mut a = app();
            a.tree_mode = false;
            draw_at(&mut a, 1280.0, 520.0);
            assert!(
                a.hit.v_scrollbar.is_some(),
                "many rows overflow → vertical scrollbar"
            );
            a.last_cursor = a.table_viewport.center();
            let before = a.scroll_off;
            dispatch_ev(&mut a, &synth_wheel(0.0, -3.0));
            assert!(
                a.scroll_off > before,
                "vertical wheel over the table scrolls the rows"
            );

            // Page: Resources dashboard taller than a short viewport.
            let mut b = app();
            b.tab = Tab::Resources;
            b.model.metrics.cores = vec![20.0; 24];
            draw_at(&mut b, 900.0, 360.0);
            assert!(
                b.hit.page_scrollbar.is_some(),
                "Resources overflows → page scrollbar"
            );
            b.last_cursor = Point::new(450.0, 200.0);
            let pbefore = b.view_scroll_y;
            dispatch_ev(&mut b, &synth_wheel(0.0, -3.0));
            assert!(
                b.view_scroll_y > pbefore,
                "wheel scrolls the Resources page"
            );
        }

        // Phase `clicking_row_scrollbar_pages_and_does_not_select`.
        // Clicking the vertical row scrollbar pages the rows and — critically —
        // does NOT select the row beneath it (the old dead-scrollbar bug).
        {
            let mut a = app();
            a.tree_mode = false;
            draw_at(&mut a, 1280.0, 520.0);
            let sb = a.hit.v_scrollbar.expect("vertical scrollbar present");
            let before_sel = a.selected_pid;
            let before_off = a.scroll_off;
            let click = Point::new(
                sb.track.center().x,
                (sb.thumb.y1 + 4.0).min(sb.track.y1 - 1.0),
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut h = false;
            a.on_click(click, &mut doc, &root, &mut h);
            assert!(h, "scrollbar consumed the click");
            assert!(
                a.scroll_off > before_off,
                "clicking below the thumb pages down"
            );
            assert_eq!(
                a.selected_pid, before_sel,
                "the row beneath the scrollbar is NOT selected"
            );
        }

        // Phase `dragging_row_scrollbar_thumb_scrolls_rows`.
        // Dragging the vertical row scrollbar thumb scrolls the rows; release ends
        // the drag — the full press → move → release path through `handle_event`.
        {
            let mut a = app();
            a.tree_mode = false;
            draw_at(&mut a, 1280.0, 520.0);
            let sb = a.hit.v_scrollbar.expect("vertical scrollbar");
            dispatch_ev(
                &mut a,
                &synth_cursor(sb.thumb.center().x, sb.thumb.center().y),
            );
            dispatch_ev(&mut a, &synth_press());
            assert!(
                matches!(a.scroll_drag, Some(ScrollDrag::Rows { .. })),
                "press on thumb starts a row drag"
            );
            let before = a.scroll_off;
            dispatch_ev(
                &mut a,
                &synth_cursor(sb.thumb.center().x, sb.track.y1 - 2.0),
            );
            assert!(
                a.scroll_off > before,
                "dragging the thumb down scrolls rows"
            );
            dispatch_ev(&mut a, &synth_release());
            assert!(a.scroll_drag.is_none(), "release ends the drag");
        }

        // Phase `narrow_table_does_not_expose_column_scrollbar`.
        // A narrow table remains a directly scannable process list instead of
        // exposing a horizontal scrollbar for its primary columns.
        {
            let mut a = app();
            draw_at(&mut a, 430.0, 700.0);
            assert!(
                a.hit.h_scrollbar.is_none(),
                "compact process columns must fit without horizontal scrolling"
            );
        }

        // Phase `dragging_page_scrollbar_thumb_scrolls_view`.
        // Dragging the whole-view page scrollbar thumb scrolls the Resources page.
        {
            let mut a = app();
            a.tab = Tab::Resources;
            a.model.metrics.cores = vec![20.0; 24];
            draw_at(&mut a, 900.0, 360.0);
            let sb = a.hit.page_scrollbar.expect("page scrollbar");
            dispatch_ev(
                &mut a,
                &synth_cursor(sb.thumb.center().x, sb.thumb.center().y),
            );
            dispatch_ev(&mut a, &synth_press());
            assert!(matches!(a.scroll_drag, Some(ScrollDrag::Page { .. })));
            let before = a.view_scroll_y;
            dispatch_ev(
                &mut a,
                &synth_cursor(sb.thumb.center().x, sb.track.y1 - 2.0),
            );
            assert!(
                a.view_scroll_y > before,
                "dragging the page thumb down scrolls the view"
            );
            dispatch_ev(&mut a, &synth_release());
            assert!(a.scroll_drag.is_none());
        }
    }

    /// Process resources: bounded catalogue, exact generations (stale ones rejected
    /// before any signal), single-line labels, safe copy-PID, and local-only views.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn process_resources_are_bounded_generation_exact_and_local() {
        // Phase `resource_force_kill_is_typed_contract_confirmed_and_semantic_ids_are_unique`.
        {
            let mut a = app();
            draw_at(&mut a, 1_280.0, 720.0);
            let pid = a.selected_pid.expect("sample selects a process");
            let resources = ui::resources::resources(&a, &AutoCommit::new());
            let process = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == resource_id_for(&a, pid))
                .expect("selected process is a stable resource");
            let kill = process
                .actions
                .iter()
                .find(|action| action.id == "kill")
                .expect("force kill is exported");
            assert_eq!(kill.contract.confirmation, ConfirmationPolicy::Confirm);

            let targets = a.accessibility_targets();
            let unique: HashSet<&str> = targets
                .iter()
                .map(|(target, _, _)| target.as_str())
                .collect();
            assert_eq!(
                unique.len(),
                targets.len(),
                "semantic targets are stable and unique"
            );

            let started_at = a
                .model
                .processes
                .iter()
                .find(|candidate| candidate.pid == pid)
                .expect("selected sample process remains in the model")
                .started_at;

            let payload = serde_json::to_vec(&MonitorOperation::Signal {
                pid,
                started_at,
                signal: MonitorSignal::Kill,
            })
            .expect("typed monitor operation serializes");
            let mut doc = AutoCommit::new();
            assert!(
                Widget::start_operation(
                    &mut a,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        OperationKind::Effect,
                        MONITOR_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .map(|started| started.is_some())
                .expect("the action-contract-confirmed signal executes")
            );
            assert!(
                a.confirm_stop.is_none(),
                "no inaccessible local dialog is opened"
            );
            assert!(a.model.processes.iter().all(|process| process.pid != pid));
        }

        // Phase `process_resource_catalogue_is_bounded_and_omitted_generation_resolves_exactly`.
        {
            let mut a = app();
            let template = a.model.processes[0].clone();
            for offset in 0..=MAX_CONTEXT_PROCESS_RESOURCES {
                let mut process = template.clone();
                process.pid = 90_000 + u32::try_from(offset).expect("small test offset");
                process.started_at = 7_000 + u64::try_from(offset).expect("small test offset");
                process.name = format!("worker-{offset}");
                a.model.processes.push(process);
            }
            let target = a.model.processes.last().expect("added process").clone();
            let resources = ui::resources::resources(&a, &AutoCommit::new());
            let root = resources.first().expect("process root");
            assert_eq!(
                root.children.len(),
                MAX_CONTEXT_PROCESS_RESOURCES,
                "the contextual catalogue cannot grow with the OS process count"
            );
            let target_id = process_resource_id(target.pid, target.started_at);
            assert!(
                !root
                    .children
                    .iter()
                    .any(|resource| resource.id.0 == target_id),
                "a tail process is intentionally omitted from the eager page"
            );
            let resolved = ui::resources::resource(&a, &AutoCommit::new(), &target_id)
                .expect("an omitted exact PID generation stays agent-resolvable");
            assert_eq!(resolved.id.0, target_id);
            assert!(resolved.actions.iter().any(|action| action.id == "kill"));

            let page =
                ui::resources::action_directory_resource_page(&a, &AutoCommit::new(), b"", 1)
                    .expect("Monitor publishes a bounded directory page");
            assert_eq!(page.resources.len(), 1, "one root obeys max_results=1");
            assert!(page.exhausted);
            assert!(page.next_cursor.is_empty());
            let empty =
                ui::resources::action_directory_resource_page(&a, &AutoCommit::new(), b"", 0)
                    .expect("zero-size page is still a valid response");
            assert!(empty.resources.is_empty(), "max_results is a hard ceiling");
        }

        // Phase `tail_processes_are_searchable_and_resolve_to_their_current_generation`.
        {
            let mut a = app();
            let mut tail = a.model.processes[0].clone();
            tail.pid = 99_991;
            tail.started_at = 88_881;
            tail.name = "searchable-tail-worker".into();
            a.model.processes.push(tail.clone());
            a.process_search.refresh(&a.model.processes);

            let provider = ui::search::capability::search_providers(&a)
                .pop()
                .expect("process provider");
            let hit = provider
                .search_limited("tail-worker", 1)
                .pop()
                .expect("the omitted catalogue tail remains globally searchable");
            let address = hit.action_address().expect("search result has an action");
            assert_eq!(
                address.resource,
                process_resource_id(tail.pid, tail.started_at)
            );
            assert_eq!(address.action, "inspect");
            assert!(
                provider.resolve_action_resource(&address).is_some(),
                "the search address materializes the exact resource, not a PID-only fallback"
            );
        }

        // Phase `process_resource_labels_are_single_line_and_bounded`.
        {
            let mut process = app().model.processes[0].clone();
            process.name = format!(
                "line\nbreak\t{}",
                "x".repeat(MAX_PROCESS_RESOURCE_LABEL_CHARS + 8)
            );
            let label = process_resource_label(&process);
            assert!(!label.contains('\n'));
            assert!(!label.contains('\t'));
            assert!(
                label.chars().count() <= MAX_PROCESS_RESOURCE_LABEL_CHARS + 16,
                "the bounded process name plus a tiny PID suffix is safe to publish"
            );
        }

        // Phase `stale_process_generation_is_rejected_before_signal_dispatch`.
        // Process effects are bound to a PID generation, not merely a reusable
        // PID. A stale palette/agent payload must fail before it reaches the
        // sampler, leaving the currently displayed process intact.
        {
            let mut a = app();
            let process = a.model.processes[0].clone();
            let result = a.perform_operation(
                &MonitorOperation::Signal {
                    pid: process.pid,
                    started_at: process.started_at.saturating_add(1),
                    signal: MonitorSignal::Kill,
                },
                SignalConfirmation::ActionContract,
            );
            assert!(result.is_err(), "a PID-reused target is rejected");
            assert!(
                a.model
                    .processes
                    .iter()
                    .any(|candidate| candidate.pid == process.pid),
                "the current process remains untouched"
            );
        }

        // Phase `stale_semantic_process_target_cannot_queue_a_pid_only_action`.
        {
            let mut a = app();
            let process = a.model.processes[0].clone();
            let mut doc = AutoCommit::new();
            assert!(!Widget::perform_semantic_action(
                &mut a,
                &format!(
                    "{}:kill",
                    monitor_process_target(process.pid, process.started_at.saturating_add(1))
                ),
                &ui::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(ui::widget::controls::take_control_invocation(&mut a).is_none());
        }

        // Phase `copy_pid_resource_operation_is_safe_and_shows_expiring_toast`.
        // The canonical resource operation performs the copy and raises a
        // transient toast that expires on its own rather than sticking forever.
        {
            let mut a = app();
            a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let process = a.model.processes[3].clone();
            let pid = process.pid;
            let resources = ui::resources::resources(&a, &AutoCommit::new());
            let process_resource = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == resource_id_for(&a, pid))
                .expect("process resource");
            let copy = process_resource
                .actions
                .iter()
                .find(|action| action.id == "copy-pid")
                .expect("copy action");
            assert_eq!(copy.kind, OperationKind::Effect);
            assert_eq!(copy.contract.permission, PermissionRequirement::Read);
            assert_eq!(copy.contract.retry, RetryPolicy::Idempotent);
            let operation = MonitorOperation::CopyPid {
                pid,
                started_at: process.started_at,
            };
            assert!(
                Widget::start_operation(
                    &mut a,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        OperationKind::Effect,
                        MONITOR_OPERATION_TYPE,
                        &serde_json::to_vec(&operation).unwrap()
                    ),
                    &mut AutoCommit::new(),
                    &automerge::ROOT
                )
                .map(|started| started.is_some())
                .expect("copy invocation accepted")
            );
            assert_eq!(a.copied_pid, Some(pid), "copy recorded");
            assert!(a.copied_at.is_some());
            let stale = MonitorOperation::CopyPid {
                pid,
                started_at: process.started_at.saturating_add(1),
            };
            assert!(
                Widget::start_operation(
                    &mut a,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        OperationKind::Effect,
                        MONITOR_OPERATION_TYPE,
                        &serde_json::to_vec(&stale).unwrap()
                    ),
                    &mut AutoCommit::new(),
                    &automerge::ROOT
                )
                .map(|started| started.is_some())
                .is_err(),
                "a recycled PID cannot receive a stale copy action"
            );
            assert!(
                a.copy_toast_active(),
                "toast active immediately after copying"
            );
            // The toast is time-limited: an old timestamp is no longer active.
            a.copied_at = ui::time::now().checked_sub(Duration::from_millis(TOAST_MS + 100));
            assert!(
                !a.copy_toast_active(),
                "toast expires after its window (not a permanent caption)"
            );
        }

        // Phase `local_monitor_views_and_device_prefs_do_not_cross_window_or_doc_heads`.
        {
            let mut first = app();
            let mut second = app();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let heads_before = doc.get_heads();

            // Device preferences: cadence, sampler state, kernel visibility, and
            // local kill confirmation. These must remain first-window-only.
            assert!(Widget::perform_semantic_action(
                &mut first,
                "monitor.interval:3000",
                &ui::a11y::Action::Activate,
                &mut doc,
                &root,
            ));
            assert!(Widget::perform_semantic_action(
                &mut first,
                "monitor.auto-refresh",
                &ui::a11y::Action::ToggleCheck,
                &mut doc,
                &root,
            ));
            assert!(Widget::perform_semantic_action(
                &mut first,
                "monitor.kernel-threads",
                &ui::a11y::Action::ToggleCheck,
                &mut doc,
                &root,
            ));
            assert!(Widget::perform_semantic_action(
                &mut first,
                "monitor.confirm-kills",
                &ui::a11y::Action::ToggleCheck,
                &mut doc,
                &root,
            ));
            // Uncommitted widget fields (and device prefs) must not enter the
            // shared document via sync. Collaborative view prefs only move
            // through MonitorAction / present().
            first.search_input.set_text("shell");
            first.set_sort(&mut AutoCommit::new(), SortKey::Name);
            first.tree_mode = false;
            first.selected_pid = first.model.processes.last().map(|process| process.pid);

            // A shared-document sync must not import first's uncommitted fields.
            second.sync(&mut doc, &root);
            assert_eq!(second.prefs, Prefs::default());
            assert!(second.search_input.value().is_empty());
            assert_eq!(second.sort_key, SortKey::Cpu);
            assert!(second.tree_mode);
            assert_ne!(second.selected_pid, first.selected_pid);
            assert_eq!(
                doc.get_heads(),
                heads_before,
                "uncommitted widget state must not enter history"
            );
        }
    }

    /// Semantic (accessibility/automation) controls act at their drawn rects and
    /// queue the same invocations as the pointer paths.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn semantic_controls_follow_pointer_paths_at_drawn_rects() {
        // Phase `monitor_semantic_controls_are_operable_at_their_drawn_rects`.
        {
            let mut a = app();
            draw_at(&mut a, 1_280.0, 720.0);
            let node = Widget::accessibility(&a).expect("monitor is semantic");
            assert!(!node.children.is_empty());
            for (index, child) in node.children.iter().enumerate() {
                assert!(child.action_target.is_some(), "control {index} is operable");
                assert_eq!(
                    Widget::synthetic_accessibility_rect(&a, &[index]),
                    child.bounds.map(ui::a11y::AccessBounds::to_rect),
                    "semantic control {index} must activate at its drawn rect"
                );
            }
        }

        // Phase `semantic_summary_tree_and_context_controls_follow_pointer_paths`.
        {
            let mut a = app();
            draw_at(&mut a, 1_280.0, 720.0);
            let mut doc = AutoCommit::new();

            assert!(Widget::perform_semantic_action(
                &mut a,
                "monitor.summary.2",
                &ui::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(a.tab, Tab::Resources);

            a.tab = Tab::Processes;
            draw_at(&mut a, 1_280.0, 720.0);
            let (pid, _) = a.hit.twisties[0];
            let started_at = a
                .model
                .processes
                .iter()
                .find(|process| process.pid == pid)
                .expect("twisty process exists")
                .started_at;
            assert!(Widget::perform_semantic_action(
                &mut a,
                &format!(
                    "monitor.tree-toggle:{}",
                    process_resource_id(pid, started_at)
                ),
                &ui::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(a.expanded.contains(&pid));

            a.open_context_menu(pid, Point::new(20.0, 20.0));
            let copy_item = a
                .context
                .items()
                .iter()
                .position(|item| item.label() == Some("Copy PID"))
                .expect("copy item");
            assert!(Widget::perform_semantic_action(
                &mut a,
                &format!(
                    "monitor.context:{}:{copy_item}",
                    process_resource_id(pid, started_at)
                ),
                &ui::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(
                ui::widget::controls::take_control_invocation(&mut a)
                    .map(|invocation| invocation.address),
                Some(ActionAddress::new(resource_id_for(&a, pid), "copy-pid"))
            );
            assert!(!a.context.is_open());
        }

        // Phase `semantic_process_actions_queue_the_same_control_invocation`.
        {
            let mut a = app();
            draw_at(&mut a, 1_280.0, 720.0);
            let pid = a.selected_pid.expect("sample selects a process");
            let started_at = a.selected_process().unwrap().started_at;
            let mut doc = AutoCommit::new();
            assert!(Widget::perform_semantic_action(
                &mut a,
                &format!("{}:kill", monitor_process_target(pid, started_at)),
                &ui::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(
                ui::widget::controls::take_control_invocation(&mut a)
                    .map(|invocation| invocation.address),
                Some(ActionAddress::new(resource_id_for(&a, pid), "kill"))
            );
            assert!(a.model.processes.iter().any(|process| process.pid == pid));

            assert!(Widget::perform_semantic_action(
                &mut a,
                "monitor.confirm-kills",
                &ui::a11y::Action::ToggleCheck,
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(!a.prefs.confirm_kill, "semantic preference toggle persists");
        }

        // Phase `process_source_permission_state_has_an_accessible_shared_retry`.
        {
            let mut a = app();
            a.seed_shot_scenario("process-permission-error");
            draw_at(&mut a, 1_280.0, 800.0);
            assert!(
                a.hit.retry_process_source.area() > 0.0,
                "the permission recovery panel exposes a visible retry control"
            );
            assert!(a.accessibility_targets().iter().any(|(target, _, node)| {
                target == "monitor.process-source.retry"
                    && node.name.as_deref() == Some("Retry loading the process list")
            }));
            assert!(Widget::perform_semantic_action(
                &mut a,
                "monitor.process-source.retry",
                &ui::a11y::Action::Activate,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert_eq!(
                ui::widget::controls::take_control_invocation(&mut a)
                    .map(|invocation| invocation.address),
                Some(ActionAddress::new("processes", "refresh")),
                "recovery uses the root resource action, not a direct sampler call"
            );
        }
    }

    // Monitor publishes only view and effect operations on live processes, so
    // its registry is empty: the sweep checks the surface and the protocol.
    ui::action_contract_tests! {
        seed: AutoCommit::new(),
        registry: ui::tilo_actions::ActionRegistry::new(),
        resources: |doc| ui::resources::resources(&app(), doc),
        palette: ["Refresh processes", "Show process details", "Copy PID", "Stop process"],
    }
}
