//! The Monitor root widget: a two-tab activity monitor (Processes /
//! Resources). The system stats are treated as **content**, not navigation —
//! there is no sidebar. The Processes tab is a full-width process table under
//! a compact horizontal summary strip; the Resources tab is a reflowing
//! dashboard of gauges and charts. View-level state (sort, filter, selection,
//! active tab, scroll) lives in widget fields here; durable preferences live
//! in the CRDT via [`crate::prefs`].
//!
//! The widget owns its own geometry: `build_layout` is a single
//! full-bleed leaf, and `draw` lays out every region by hand and renders
//! it into the Vello scene. Hit rectangles computed during `draw` are
//! cached in [`Hit`] and consulted by `handle_event` on the next input —
//! the same pattern the toolkit's `DataTable` uses for its column rects.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::icons::{self, Icon};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::popover::Popover;
use ui::widget::{Button, ButtonFace, ButtonStyle, ModalDialog, SearchField, Text, Widget};
use std::cell::Cell;
use std::rc::Rc;

use crate::charts::{self, with_alpha, Series};
use crate::layout;
use crate::model::{fmt_bytes, fmt_rate, fmt_uptime, MonitorModel, ProcessInfo, Ring};
use crate::prefs::Prefs;
use crate::responsive::{ProcPane, ProcPush};
use crate::sampler::{Sampler, StopKind};
use crate::tree::{FlattenOpts, Forest, Row, SumMode};

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Processes,
    Resources,
}

impl Tab {
    fn other(self) -> Tab {
        match self {
            Tab::Processes => Tab::Resources,
            Tab::Resources => Tab::Processes,
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
}

// ---- layout constants ----------------------------------------------------
const OUTER_PAD: f64 = 14.0;
const HEADER_H: f64 = 56.0;
const GAP: f64 = 14.0;
const ROW_H: f64 = 22.0;
const TABLE_HEADER_H: f64 = 26.0;
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

// `ui::widget::ContextMenu` lays itself out with private constants; we mirror
// them here so the app's hit-testing + on-screen-nudge match the primitive's
// drawn geometry exactly. Keep in sync with `context_menu.rs`.
const CONTEXT_MENU_W: f64 = 220.0;
const CONTEXT_MENU_ROW_H: f64 = 28.0;
const CONTEXT_MENU_SEP_H: f64 = 9.0;
const CONTEXT_MENU_PAD: f64 = 6.0;

/// How long the "PID copied" toast stays up after a Copy PID action before it
/// expires on its own. The redraw loop keeps ticking (`needs_animation_frame`
/// is always true), so the toast clears itself without any further input.
const TOAST_MS: u64 = 2200;

/// Refresh cadences offered by the interval control, smallest → largest. A
/// quick click on the pill cycles through them; a long-press opens a popover
/// listing them all. Kept in one place so the pill label, the cycle order and
/// the popover options stay in lock-step.
const INTERVAL_OPTIONS: [u64; 5] = [1000, 1500, 2000, 3000, 5000];

/// Press-and-hold duration on the interval pill that opens the option popover
/// instead of cycling. A release before this fires the plain click-to-cycle.
const LONG_PRESS_MS: u128 = 350;

/// Sample depth of the per-process CPU sparkline shown in the detail pane —
/// a shorter window than the whole-system charts so the mini-line stays punchy.
const PROC_SPARK_CAP: usize = 48;

/// Height of the pinned action bar at the bottom of the detail pane. The
/// Kill/Stop buttons live here and nowhere else, so process content above can
/// never collide with them however narrow the pane gets.
const DETAIL_ACTION_H: f64 = 46.0;

/// Human label for a refresh cadence in ms: `1s`, `1.5s`, `2s`. Whole seconds
/// drop the decimal. Mirrors [`crate::prefs::Prefs::interval_label`] so the
/// pill and the popover options read identically.
fn interval_label(ms: u64) -> String {
    if ms % 1000 == 0 {
        format!("{}s", ms / 1000)
    } else {
        format!("{:.1}s", ms as f32 / 1000.0)
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
    /// Context-menu item rects, recorded while the menu is open.
    menu_items: Vec<(usize, Rect)>,
    /// Summary-strip stat cells (Processes tab). Clicking one jumps to the
    /// Resources dashboard, where that stat's full gauge/chart lives.
    summary_cells: Vec<Rect>,
    /// Back affordance returning from the pushed detail to the table (narrow).
    detail_back: Rect,
    /// The "Confirm kills" preference toggle pill in the toolbar — the only UI
    /// that reaches the persisted `confirm_kill` preference.
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

pub struct MonitorApp {
    model: MonitorModel,
    sampler: Sampler,
    prefs: Prefs,

    // view state
    tab: Tab,
    sort_key: SortKey,
    sort_desc: bool,
    /// Lower-cased process filter, mirrored from `search` each time its query
    /// changes; the row filter reads this.
    filter: String,
    /// The shared search field (magnifier pill; real caret / selection /
    /// clipboard / clear button). Source of truth for the filter text + focus.
    search: SearchField,
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
    confirm_stop: Option<(u32, StopKind)>,
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
    /// Rolling per-process CPU history, keyed by pid, feeding the detail pane's
    /// sparkline. Pushed each live refresh and pruned to the live process set.
    proc_cpu_hist: HashMap<u32, Ring>,

    // responsive layout
    /// The Processes master-detail push flow used only when narrow.
    proc_push: ProcPush,

    // frame caches
    rect: Rect,
    last_cursor: Point,
    last_refresh: Instant,
    rows_per_page: usize,
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
    hit: Hit,
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
        // Pre-select the busiest process so the detail strip is populated
        // in the headless screenshot.
        let selected_pid = model
            .processes
            .iter()
            .max_by(|a, b| a.cpu.total_cmp(&b.cpu))
            .map(|p| p.pid);
        // "now" ⇒ the first (screenshot) frame skips the live refresh and
        // renders the seeded sample untouched.
        let mut app = Self::build(model, selected_pid, Instant::now());
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
        let stale = Instant::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        Self::build(MonitorModel::empty(), None, stale)
    }

    fn build(model: MonitorModel, selected_pid: Option<u32>, last_refresh: Instant) -> Self {
        Self {
            model,
            sampler: Sampler::new(),
            prefs: Prefs::default(),
            tab: Tab::Processes,
            sort_key: SortKey::Cpu,
            sort_desc: true,
            filter: String::new(),
            search: SearchField::new()
                .standalone()
                .with_id("monitor_search")
                .with_placeholder("Search processes…"),
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
            copied_pid: None,
            copied_at: None,
            scroll_drag: None,
            context_menu_seed: None,
            interval_popover: Popover::new().with_label("Refresh interval"),
            interval_press: None,
            interval_hold_opened: false,
            interval_popover_seed: false,
            proc_cpu_hist: HashMap::new(),
            proc_push: ProcPush::default(),
            rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            last_refresh,
            rows_per_page: 1,
            table_viewport: Rect::ZERO,
            table_content_w: 0.0,
            table_cols_w: 0.0,
            hit: Hit::new(),
        }
    }

    /// Refresh from sysinfo at most once per preference interval. The very
    /// first draw never trips this (elapsed ≈ 0), so `--shots` shows the
    /// sample snapshot; the live loop updates a beat later.
    fn maybe_refresh(&mut self) {
        if self.last_refresh.elapsed() >= Duration::from_millis(self.prefs.interval_ms) {
            self.sampler.refresh(&mut self.model);
            self.last_refresh = Instant::now();
            self.record_proc_cpu();
            self.clamp_scroll();
        }
    }

    /// Fold this refresh's per-process CPU into the sparkline histories, then
    /// prune rings for processes that have since exited so the map stays bound
    /// to the live process set.
    fn record_proc_cpu(&mut self) {
        for p in &self.model.processes {
            self.proc_cpu_hist
                .entry(p.pid)
                .or_insert_with(|| Ring::new(PROC_SPARK_CAP))
                .push(p.cpu);
        }
        let live: HashSet<u32> = self.model.processes.iter().map(|p| p.pid).collect();
        self.proc_cpu_hist.retain(|pid, _| live.contains(pid));
    }

    /// Seed the currently-selected process's CPU ring with a synthetic wave
    /// around its present load. Used only by the hermetic `--shots`/sample
    /// constructor so the detail sparkline has data before any live refresh.
    fn seed_selected_spark(&mut self) {
        let Some(pid) = self.selected_pid else { return };
        let Some(p) = self.model.processes.iter().find(|p| p.pid == pid) else { return };
        let base = p.cpu;
        let ring = Ring::from_iter(
            PROC_SPARK_CAP,
            (0..PROC_SPARK_CAP).map(|i| {
                let t = i as f32;
                (base + 9.0 * (t * 0.31).sin() + 4.0 * (t * 0.73).cos()).clamp(0.0, base.max(4.0) * 1.6)
            }),
        );
        self.proc_cpu_hist.insert(pid, ring);
    }

    /// The filtered + sorted process list currently displayed.
    fn visible_processes(&self) -> Vec<ProcessInfo> {
        let f = self.filter.to_lowercase();
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
            if self.sort_desc {
                o.reverse()
            } else {
                o
            }
        });
        v
    }

    fn selected_process(&self) -> Option<ProcessInfo> {
        let pid = self.selected_pid?;
        self.model.processes.iter().find(|p| p.pid == pid).cloned()
    }

    /// Build the rows the process table renders this frame. In tree mode this
    /// is the flattened parent→child forest (honoring collapse state, the
    /// sum/per-process mode, sort, kernel pref and filter); in flat mode it's
    /// the sorted list rendered at depth 0 with no twisties. Both paths share
    /// the same [`Row`] shape so `draw_process_table` is mode-agnostic.
    fn visible_rows(&self) -> Vec<Row> {
        if self.tree_mode {
            let forest = Forest::build(&self.model.processes);
            let filter = self.filter.to_lowercase();
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
        self.clamp_scroll();
    }

    /// Whether `pid` has at least one child in the current process set —
    /// honoring the kernel-thread pref so a parent whose only children are
    /// hidden kernel threads doesn't claim a twisty.
    fn row_has_children(&self, pid: u32) -> bool {
        self.model.processes.iter().any(|c| {
            c.ppid == Some(pid)
                && c.pid != pid
                && (self.prefs.show_kernel_threads || !c.kernel)
        })
    }

    fn set_sort(&mut self, key: SortKey) {
        if self.sort_key == key {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort_key = key;
            self.sort_desc = key.default_desc();
        }
    }

    /// Request a hard **Kill** (SIGKILL). Gated by the confirm-kill pref,
    /// since SIGKILL is un-catchable and loses unsaved work.
    fn request_kill(&mut self, pid: u32, doc: &mut AutoCommit, _path: &ObjId) {
        if self.prefs.confirm_kill {
            self.confirm_stop = Some((pid, StopKind::Kill));
        } else {
            self.do_stop(pid, StopKind::Kill, doc);
        }
    }

    /// **Stop** a process (SIGTERM) — the polite, catchable termination
    /// request. Never gated by a confirm dialog: it lets the process clean
    /// up, so it's safe as a one-click action.
    fn request_stop(&mut self, pid: u32, doc: &mut AutoCommit) {
        self.do_stop(pid, StopKind::Term, doc);
    }

    /// Dispatch the real signal and reflect it in the local model. Live: the
    /// sampler sends the actual SIGTERM/SIGKILL via the process handle.
    /// Sample/offscreen: the signal call is a no-op (the pid isn't a real
    /// process), but we still drop the row so the UI responds — the next live
    /// refresh would do the same once the process actually exits.
    fn do_stop(&mut self, pid: u32, kind: StopKind, _doc: &mut AutoCommit) {
        let _ = self.sampler.signal(pid, kind);
        // SIGKILL removes the row immediately (it cannot survive); SIGTERM is
        // a request, so on a *live* system we'd wait for the next refresh —
        // but in the seeded/offscreen model there's no live refresh, so we
        // drop it here too to keep the action observable.
        self.model.processes.retain(|p| p.pid != pid);
        if self.selected_pid == Some(pid) {
            self.selected_pid = None;
        }
        self.confirm_stop = None;
        self.confirm_dialog = None;
        self.clamp_scroll();
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
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "process".into());

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
            menu.push(MenuItem::action("Show details", set(ProcAction::ShowDetails(pid))));
        }
        menu.push(MenuItem::action("Copy PID", set(ProcAction::CopyPid(pid))));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action(format!("Stop {name}  (SIGTERM)"), set(ProcAction::Stop(pid))));
        menu.push(MenuItem::action(format!("Kill {name}  (SIGKILL)"), set(ProcAction::Kill(pid))));

        // Anchor so the menu stays on-screen: nudge left/up if it would spill
        // past the app's right/bottom edge.
        let mh = Self::context_menu_height(&menu);
        let mw = CONTEXT_MENU_W;
        let mut x = pos.x;
        let mut y = pos.y;
        if x + mw > self.rect.x1 {
            x = (self.rect.x1 - mw).max(self.rect.x0);
        }
        if y + mh > self.rect.y1 {
            y = (self.rect.y1 - mh).max(self.rect.y0);
        }
        menu.open_at([x as f32, y as f32]);
        self.context = menu;
        self.context_pid = Some(pid);
        self.selected_pid = Some(pid);
        self.layout_context_menu();
    }

    /// Run the `ContextMenu` primitive's own `build_layout`/`apply_layout`
    /// passes against a throwaway one-node taffy tree so its internal `rect`
    /// matches the anchored position. The menu is a hand-drawn child of this
    /// app (it isn't in the host's widget tree), so we drive its layout
    /// contract here — honoring the primitive rather than poking its private
    /// rect — before drawing it or hit-testing its rows.
    fn layout_context_menu(&mut self) {
        use std::collections::HashMap;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = self.context.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let _ = taffy.compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(CONTEXT_MENU_W as f32),
                height: taffy::AvailableSpace::MaxContent,
            },
        );
        // `apply_layout` prefers the open menu's stored anchor over the taffy
        // origin, so the origin we pass is irrelevant when open.
        self.context.apply_layout(&taffy, node, Point::ZERO);
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
        let labels: Vec<String> = INTERVAL_OPTIONS.iter().map(|ms| interval_label(*ms)).collect();
        let selected = INTERVAL_OPTIONS.iter().position(|ms| *ms == self.prefs.interval_ms);
        self.interval_popover.set_items(labels);
        self.interval_popover.set_viewport(self.rect);
        // The pill's hit rect is known after `draw_header`; fall back to a
        // sensible header-corner anchor if the popover is opened outside a draw
        // (headless tests that never paint a frame).
        let anchor = if self.hit.interval == Rect::ZERO {
            Rect::new(self.rect.x1 - 140.0, self.rect.y0 + 20.0, self.rect.x1 - 40.0, self.rect.y0 + 48.0)
        } else {
            self.hit.interval
        };
        self.interval_popover.open_at(anchor, selected);
    }

    /// Apply the interval option the popover just yielded (drained via
    /// `take_selection`) to the prefs + CRDT.
    fn apply_interval_selection(&mut self, doc: &mut AutoCommit, path: &ObjId) {
        if let Some(sel) = self.interval_popover.take_selection() {
            self.prefs.interval_ms = INTERVAL_OPTIONS[sel];
            self.prefs.store(doc, path);
        }
    }

    /// Resolve a completed press on the interval pill. A release *after* the
    /// long-press threshold has already opened the popover (via the hold check
    /// in `draw`) leaves it up; a release *before* the threshold cycles the
    /// cadence — the plain click. A release that crosses the threshold with no
    /// paint in between (headless) opens the popover now. Returns whether a
    /// pending press was consumed.
    fn release_interval_press(&mut self, doc: &mut AutoCommit, path: &ObjId) -> bool {
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
            self.prefs.store(doc, path);
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

    /// Apply a [`ProcAction`] the context menu recorded. The CopyPid path
    /// writes the pid to the real OS clipboard via [`ui::clipboard_copy`] (a
    /// silent no-op in a headless/locked-down session) and records it so the
    /// transient toast and tests can observe the action.
    fn apply_action(&mut self, action: ProcAction, doc: &mut AutoCommit, path: &ObjId) {
        match action {
            ProcAction::Stop(pid) => self.request_stop(pid, doc),
            ProcAction::Kill(pid) => self.request_kill(pid, doc, path),
            ProcAction::CopyPid(pid) => {
                ui::clipboard_copy(&pid.to_string());
                self.copied_pid = Some(pid);
                self.copied_at = Some(Instant::now());
                self.selected_pid = Some(pid);
            }
            ProcAction::ShowDetails(pid) => {
                self.selected_pid = Some(pid);
                if self.is_narrow() {
                    self.proc_push.show_detail();
                }
            }
        }
    }

    /// Per-item rects of the open context menu, mirroring the primitive's own
    /// fixed geometry (`ROW_HEIGHT`/`SEPARATOR_HEIGHT`/`PADDING`) so a click
    /// maps to the row under the cursor. Returns `(item_index, rect)` for
    /// action rows only (separators aren't clickable).
    fn context_menu_item_rects(&self) -> Vec<(usize, Rect)> {
        let r = self.context.get_rect();
        let mut out = Vec::new();
        let mut y = r.y0 + CONTEXT_MENU_PAD;
        for (idx, item) in self.context.items().iter().enumerate() {
            match item {
                MenuItem::Action { .. } => {
                    out.push((idx, Rect::new(r.x0, y, r.x1, y + CONTEXT_MENU_ROW_H)));
                    y += CONTEXT_MENU_ROW_H;
                }
                MenuItem::Separator => y += CONTEXT_MENU_SEP_H,
            }
        }
        out
    }

    /// Total height the menu occupies — mirrors the primitive's `build_layout`
    /// height math so the on-screen-nudge in `open_context_menu` is accurate.
    fn context_menu_height(menu: &ContextMenu) -> f64 {
        let items: f64 = menu
            .items()
            .iter()
            .map(|i| match i {
                MenuItem::Action { .. } => CONTEXT_MENU_ROW_H,
                MenuItem::Separator => CONTEXT_MENU_SEP_H,
            })
            .sum::<f64>()
            .max(CONTEXT_MENU_ROW_H);
        CONTEXT_MENU_PAD * 2.0 + items
    }

    // ---- text helpers ----------------------------------------------------
    fn truncate(tr: &mut TextRenderer, s: &str, size: f32, max_w: f64) -> String {
        if max_w <= 0.0 {
            return String::new();
        }
        if tr.measure(s, size, None).0 as f64 <= max_w {
            return s.to_string();
        }
        let mut out = s.to_string();
        while !out.is_empty() {
            out.pop();
            let cand = format!("{out}…");
            if tr.measure(&cand, size, None).0 as f64 <= max_w {
                return cand;
            }
        }
        "…".to_string()
    }

    fn clock() -> String {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
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
        let core_h = layout::per_core_card_h(grid);
        let rows = layout::charts_rows(w);
        let min_chart_h = 150.0;
        let charts_h = rows as f64 * min_chart_h + (rows as f64 - 1.0) * GAP;
        layout::overview_h(w) + GAP + core_h + GAP + charts_h
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
        let core_h = layout::per_core_card_h(grid);
        let charts_top = layout::overview_h(body_w) + GAP + core_h + GAP;
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
        let viewport_h = (self.rect.height() - 2.0 * OUTER_PAD - HEADER_H - GAP).max(1.0);
        (body_w, viewport_h)
    }

    /// Whether the "PID copied" toast should currently show — true for a short
    /// window after a Copy PID action, then it expires on its own.
    fn copy_toast_active(&self) -> bool {
        self.copied_at
            .map(|t| t.elapsed() < Duration::from_millis(TOAST_MS))
            .unwrap_or(false)
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
            "detail" => self.proc_push.snap(ProcPane::Detail),
            // Open the refresh-interval option popover (anchored under the pill,
            // which isn't positioned until `draw_header` runs, so defer it).
            "interval-popover" => self.interval_popover_seed = true,
            "resources" => self.tab = Tab::Resources,
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
                self.context_menu_seed =
                    self.busiest_pid_named("rustc").or_else(|| self.busiest_visible_pid());
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
                if let Some(pid) = self
                    .selected_pid
                    .or_else(|| self.model.processes.first().map(|p| p.pid))
                {
                    self.confirm_stop = Some((pid, StopKind::Kill));
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
    fn draw_header(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect) {
        let c = &theme.tokens.colors;
        let inner = charts::panel(scene, t, r, theme, 12.0);
        let xs = theme.tokens.typography.scale.xs;
        let seg_h = 30.0;
        let cy = inner.y0 + (inner.height() - seg_h) / 2.0;

        // The header lays out two groups: centred tabs and the clock/interval
        // block — and they must never overlap, however narrow the window. We
        // measure each, then shrink the least essential parts (the uptime
        // caption, finally the tab labels → icon-only pills) until they fit. The
        // tab control and the clock+interval are always present.
        let header_narrow = inner.width() < 560.0;

        // ---- right block: clock (top) + uptime caption (bottom) + pill ----
        let clock = Self::clock();
        let cs = theme.tokens.typography.scale.lg;
        let (clw, _) = tr.measure_weighted(&clock, cs, None, TextWeight::Bold);
        let up = fmt_uptime(self.model.metrics.uptime_secs);
        let up_caption = format!("uptime {up}");
        let (upw, _) = tr.measure(&up_caption, xs, None);
        let right_block_w = (clw as f64).max(upw as f64);
        let right_block_x0 = inner.x1 - right_block_w;

        let il = format!("⟳ {}", self.prefs.interval_label());
        let iw = tr.measure_weighted(&il, xs, None, TextWeight::Medium).0 as f64 + 22.0;
        let pill_x1 = right_block_x0 - 16.0;
        let pill_x0 = pill_x1 - iw;

        // The OS owns window chrome (app-name/logo/subtitle) — the header draws
        // only its functional controls (tabs, clock, interval pill), so the tab
        // control gets the full width from the left edge.
        let brand_right = inner.x0;

        // ---- centre tab segmented control ---------------------------------
        // Tabs want a comfortable width; clamp them into the gap between the
        // brand and the interval pill, and fall back to icon-only pills when
        // even the clamped width can't seat the labels.
        let gap_l = brand_right + 16.0;
        let gap_r = pill_x0 - 16.0;
        let avail_tabs = (gap_r - gap_l).max(0.0);
        let want_seg_w = 124.0_f64;
        let compact = avail_tabs < 2.0 * 96.0 + 6.0;
        let seg_w = if compact {
            ((avail_tabs - 6.0) / 2.0).clamp(40.0, 64.0)
        } else {
            want_seg_w.min((avail_tabs - 6.0) / 2.0)
        };
        let pair_w = 2.0 * seg_w + 6.0;
        // Centre the pair in the gap, but never let it cross either edge.
        let centre = (inner.x0 + inner.x1) / 2.0;
        let cx = (centre - pair_w / 2.0).clamp(gap_l, (gap_r - pair_w).max(gap_l));
        let proc_r = Rect::new(cx, cy, cx + seg_w, cy + seg_h);
        let res_r = Rect::new(cx + seg_w + 6.0, cy, cx + 2.0 * seg_w + 6.0, cy + seg_h);
        self.draw_tab_pill_ex(scene, t, tr, theme, proc_r, Icon::Layers, "Processes", self.tab == Tab::Processes, compact);
        self.draw_tab_pill_ex(scene, t, tr, theme, res_r, Icon::BarChart, "Resources", self.tab == Tab::Resources, compact);
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
            t * Affine::translate((inner.x1 - clw as f64, inner.y0 + 1.0)),
            None,
            TextWeight::Bold,
        );
        // Uptime caption (second line) — dropped when space is tight.
        if show_uptime {
            tr.draw_text(
                scene,
                &up_caption,
                xs,
                c.text_dim,
                t * Affine::translate((inner.x1 - upw as f64, inner.y0 + 22.0)),
                None,
            );
        }

        // Interval pill, left of the right block.
        let interval_r = Rect::new(pill_x0, cy + 1.0, pill_x1, cy + seg_h - 1.0);
        charts::fill_rrect(scene, t, interval_r, seg_h / 2.0, c.surface_white_5);
        charts::stroke_rrect(scene, t, interval_r, seg_h / 2.0, c.border_standard, 1.0);
        let (tw, th) = tr.measure_weighted(&il, theme.tokens.typography.scale.sm, None, TextWeight::Medium);
        tr.draw_text_weighted(
            scene,
            &il,
            theme.tokens.typography.scale.sm,
            c.text_primary,
            t * Affine::translate((interval_r.x0 + (interval_r.width() - tw as f64) / 2.0, interval_r.y0 + (interval_r.height() - th as f64) / 2.0)),
            None,
            TextWeight::Medium,
        );
        self.hit.interval = interval_r;
    }

    /// Draw a tab pill. `compact` draws an icon-only pill (the label is
    /// dropped) so the tab control still fits when the header is narrow.
    #[allow(clippy::too_many_arguments)]
    fn draw_tab_pill_ex(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect, icon: Icon, label: &str, active: bool, compact: bool) {
        let c = &theme.tokens.colors;
        let radius = r.height() / 2.0;
        if active {
            charts::fill_rrect(scene, t, r, radius, c.accent[5]);
        } else {
            charts::fill_rrect(scene, t, r, radius, c.surface_white_5);
            charts::stroke_rrect(scene, t, r, radius, c.border_standard, 1.0);
        }
        let text_color = if active { Color::WHITE } else { c.text_dim };
        if compact {
            // Centre the icon; no label.
            let icon_r = Rect::new(r.center().x - 8.0, r.center().y - 8.0, r.center().x + 8.0, r.center().y + 8.0);
            icons::draw_icon(scene, t, icon_r, icon, text_color, 1.6);
            return;
        }
        let icon_r = Rect::new(r.x0 + 14.0, r.center().y - 8.0, r.x0 + 30.0, r.center().y + 8.0);
        icons::draw_icon(scene, t, icon_r, icon, text_color, 1.6);
        let fs = theme.tokens.typography.scale.sm;
        let (_, th) = tr.measure_weighted(label, fs, None, TextWeight::Semibold);
        tr.draw_text_weighted(
            scene,
            label,
            fs,
            text_color,
            t * Affine::translate((icon_r.x1 + 8.0, r.center().y - th as f64 / 2.0)),
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
    /// Clicking a cell jumps to the
    /// Resources dashboard where that stat's full gauge/chart lives; the cell
    /// hit rects are recorded for `on_click`.
    fn draw_summary_strip(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect, g: layout::StripGrid) {
        let c = &theme.tokens.colors;

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
        let cells = {
            let m = &self.model.metrics;
            let overall = m.cpu_overall();
            let net_max = m.net_rx_hist.max().max(m.net_tx_hist.max()).max(1.0);
            let disk_max = m.disk_read_hist.max().max(m.disk_write_hist.max()).max(1.0);
            [
                Cell { label: "CPU", value: format!("{:.0}%", overall), vcolor: charts::load_color(theme, overall / 100.0), data: m.cpu_hist.as_slice().to_vec(), max: 100.0, spark: c.accent[5] },
                Cell { label: "Memory", value: fmt_bytes(m.mem_used), vcolor: c.text_primary, data: m.mem_hist.as_slice().to_vec(), max: 1.0, spark: c.semantic.success },
                Cell { label: "Network", value: fmt_rate(m.net_rx + m.net_tx), vcolor: c.text_primary, data: m.net_rx_hist.as_slice().to_vec(), max: net_max, spark: c.semantic.info },
                Cell { label: "Disk", value: fmt_rate(m.disk_read + m.disk_write), vcolor: c.text_primary, data: m.disk_read_hist.as_slice().to_vec(), max: disk_max, spark: c.semantic.warning },
            ]
        };

        self.hit.summary_cells.clear();
        let cols = g.cols;
        let cell_w = (r.width() - (cols as f64 - 1.0) * GAP) / cols as f64;
        let xs = theme.tokens.typography.scale.xs;
        let sm = theme.tokens.typography.scale.sm;
        for (i, cell) in cells.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let x0 = r.x0 + col as f64 * (cell_w + GAP);
            let y0 = r.y0 + row as f64 * (layout::STRIP_CELL_H + GAP);
            let rect = Rect::new(x0, y0, x0 + cell_w, y0 + layout::STRIP_CELL_H);
            let inner = charts::panel(scene, t, rect, theme, layout::STRIP_CELL_PAD);
            // Label (dim, left). The single-row strip keeps a constant height so
            // the table below never loses height; the cell instead adapts
            // horizontally — value beside the label when it fits, else stacked
            // beneath it — so a narrow cell never overlaps its own text.
            tr.draw_text(scene, cell.label, xs, c.text_dim, t * Affine::translate((inner.x0, inner.y0)), None);
            let (lw, _) = tr.measure(cell.label, xs, None);
            let (vw, _) = tr.measure_weighted(&cell.value, sm, None, TextWeight::Bold);
            let spark_top = if lw as f64 + 8.0 + vw as f64 <= inner.width() {
                // Roomy: headline value right-aligned on the label's line.
                tr.draw_text_weighted(scene, &cell.value, sm, cell.vcolor, t * Affine::translate((inner.x1 - vw as f64, inner.y0 - 1.0)), None, TextWeight::Bold);
                inner.y0 + 22.0
            } else {
                // Tight: value drops onto its own line under the label so the
                // two never collide, and the sparkline shifts down to clear it.
                tr.draw_text_weighted(scene, &cell.value, sm, cell.vcolor, t * Affine::translate((inner.x0, inner.y0 + 14.0)), None, TextWeight::Bold);
                inner.y0 + 34.0
            };
            // Sparkline fills the remaining lower portion of the cell.
            let sr = Rect::new(inner.x0, spark_top.min(inner.y1), inner.x1, inner.y1);
            charts::sparkline(scene, t, sr, &cell.data, cell.max, cell.spark);
            self.hit.summary_cells.push(rect);
        }
    }

    /// The Resources overview **Processor** card: a CPU-load ring gauge, plus
    /// (when the card is wide enough) a compact stats column — load average,
    /// process and thread counts. This is the old rail's top section as
    /// dashboard content.
    fn draw_processor_card(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect) {
        let c = &theme.tokens.colors;
        let m = &self.model.metrics;
        let inner = charts::panel(scene, t, rect, theme, 12.0);
        tr.draw_text_weighted(scene, "Processor", theme.tokens.typography.scale.sm, c.text_primary, t * Affine::translate((inner.x0, inner.y0)), None, TextWeight::Semibold);
        let overall = m.cpu_overall();
        let xs = theme.tokens.typography.scale.xs;
        let sm = theme.tokens.typography.scale.sm;
        if inner.width() >= 300.0 {
            // Ring on the left, stats column on the right.
            let center = Point::new(inner.x0 + 62.0, inner.center().y + 8.0);
            charts::ring_gauge(scene, t, tr, center, 44.0, 9.0, overall / 100.0, charts::load_color(theme, overall / 100.0), theme, &format!("{:.0}%", overall), "CPU LOAD");
            let sx = inner.x0 + 138.0;
            let procs = self.model.processes.len();
            let threads: usize = self.model.processes.iter().map(|p| p.threads).sum();
            let stats = [
                ("Load average", format!("{:.2}  {:.2}  {:.2}", m.load_avg[0], m.load_avg[1], m.load_avg[2])),
                ("Processes", format!("{procs}")),
                ("Threads", format!("{threads}")),
            ];
            let mut sy = inner.y0 + 30.0;
            for (label, value) in stats {
                tr.draw_text(scene, label, xs, c.text_dim, t * Affine::translate((sx, sy)), None);
                let value = Self::truncate(tr, &value, sm, inner.x1 - sx);
                tr.draw_text_weighted(scene, &value, sm, c.text_primary, t * Affine::translate((sx, sy + 15.0)), None, TextWeight::Semibold);
                sy += 40.0;
            }
        } else {
            // Narrow/stacked: centred ring with the load average beneath it.
            let center = Point::new(inner.center().x, inner.y0 + 84.0);
            charts::ring_gauge(scene, t, tr, center, 44.0, 9.0, overall / 100.0, charts::load_color(theme, overall / 100.0), theme, &format!("{:.0}%", overall), "CPU LOAD");
            let la = format!("load  {:.2}  {:.2}  {:.2}", m.load_avg[0], m.load_avg[1], m.load_avg[2]);
            let (lw, _) = tr.measure(&la, xs, None);
            tr.draw_text(scene, &la, xs, c.text_dim, t * Affine::translate((inner.center().x - lw as f64 / 2.0, inner.y1 - 12.0)), None);
        }
    }

    /// The Resources overview **Memory** card: RAM + swap bar gauges, plus a
    /// memory-history sparkline when the card is tall enough.
    fn draw_memory_card(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect) {
        let c = &theme.tokens.colors;
        let m = &self.model.metrics;
        let mi = charts::panel(scene, t, rect, theme, 12.0);
        tr.draw_text_weighted(scene, "Memory", theme.tokens.typography.scale.sm, c.text_primary, t * Affine::translate((mi.x0, mi.y0)), None, TextWeight::Semibold);
        let ram_label = format!("{} / {}", fmt_bytes(m.mem_used), fmt_bytes(m.mem_total));
        charts::bar_gauge(scene, t, tr, Rect::new(mi.x0, mi.y0 + 24.0, mi.x1, mi.y0 + 46.0), theme, m.mem_frac(), charts::load_color(theme, m.mem_frac()), "RAM", &ram_label);
        let swap_label = format!("{} / {}", fmt_bytes(m.swap_used), fmt_bytes(m.swap_total));
        charts::bar_gauge(scene, t, tr, Rect::new(mi.x0, mi.y0 + 56.0, mi.x1, mi.y0 + 78.0), theme, m.swap_frac(), c.accent[5], "Swap", &swap_label);
        // A memory sparkline fills the remaining height on the taller (abreast
        // or fixed-height) card.
        if mi.height() >= 108.0 {
            let sr = Rect::new(mi.x0, mi.y0 + 92.0, mi.x1, mi.y1);
            charts::sparkline(scene, t, sr, m.mem_hist.as_slice(), 1.0, c.semantic.success);
        }
    }

    /// The Resources overview **System** card: host / kernel / cores / uptime
    /// and the CPU brand — the old rail info card, now dashboard content.
    fn draw_system_card(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect) {
        let c = &theme.tokens.colors;
        let m = &self.model.metrics;
        let ii = charts::panel(scene, t, rect, theme, 12.0);
        tr.draw_text_weighted(scene, "System", theme.tokens.typography.scale.sm, c.text_primary, t * Affine::translate((ii.x0, ii.y0)), None, TextWeight::Semibold);
        let fs = theme.tokens.typography.scale.xs;
        let mut y = ii.y0 + 24.0;
        let line = |scene: &mut Scene, tr: &mut TextRenderer, k: &str, v: String, y: &mut f64| {
            tr.draw_text(scene, k, fs, c.text_dim, t * Affine::translate((ii.x0, *y)), None);
            let vv = Self::truncate(tr, &v, fs, ii.width() - 60.0);
            let (vw, _) = tr.measure(&vv, fs, None);
            tr.draw_text(scene, &vv, fs, c.text_primary, t * Affine::translate((ii.x1 - vw as f64, *y)), None);
            *y += 18.0;
        };
        line(scene, tr, "host", m.host.clone(), &mut y);
        line(scene, tr, "kernel", m.kernel.clone(), &mut y);
        line(scene, tr, "cores", format!("{}", m.cores.len()), &mut y);
        line(scene, tr, "uptime", fmt_uptime(m.uptime_secs), &mut y);
        if y + 14.0 <= ii.y1 {
            let cpu = Self::truncate(tr, &m.cpu_brand, fs, ii.width());
            tr.draw_text(scene, &cpu, fs, c.text_dim, t * Affine::translate((ii.x0, y)), None);
        }
    }

    /// The Resources **per-core utilisation** card: one bar gauge per core in a
    /// grid that wraps to as many rows as the core count needs, sized so the
    /// last row renders inside the card (`layout::per_core_card_h`).
    fn draw_per_core(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, core_panel: Rect, grid: layout::CoreGrid) {
        let c = &theme.tokens.colors;
        let n = self.model.metrics.cores.len().max(1);
        let ci = charts::panel(scene, t, core_panel, theme, layout::RAIL_CARD_PAD);
        tr.draw_text_weighted(scene, &format!("Per-Core Utilisation  ·  {} threads", n), theme.tokens.typography.scale.sm, c.text_primary, t * Affine::translate((ci.x0, ci.y0)), None, TextWeight::Semibold);
        let grid_top = ci.y0 + layout::CORE_TITLE_H;
        let cols = grid.cols;
        let cell_w = (ci.width() - (cols as f64 - 1.0) * layout::CORE_GAP) / cols as f64;
        for (i, &usage) in self.model.metrics.cores.iter().enumerate() {
            let col = i % cols;
            let rowi = i / cols;
            let x0 = ci.x0 + col as f64 * (cell_w + layout::CORE_GAP);
            let y0 = grid_top + rowi as f64 * layout::CORE_ROW_STRIDE;
            charts::bar_gauge(scene, t, tr, Rect::new(x0, y0, x0 + cell_w, y0 + 22.0), theme, usage / 100.0, charts::load_color(theme, usage / 100.0), &format!("C{i}"), &format!("{:.0}%", usage));
        }
    }

    /// The Processes tab: a compact summary strip, a search/sort toolbar, and
    /// a **full-width** process table — the table is the content. Wide: the
    /// selected process's detail strip stacks below the table. Narrow:
    /// single-pane push — the strip+toolbar+table OR the detail, sliding
    /// horizontally as one swaps for the other (driven by [`ProcPush`]).
    /// Selecting a row pushes the detail in with a Back affordance; Back (or
    /// Esc) slides the table back.
    fn draw_processes(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect) {
        let strip_g = layout::summary_grid(r.width());
        let strip_h = layout::summary_strip_h(strip_g);

        if !self.is_narrow() {
            // Wide: summary strip, toolbar, table, detail strip stacked.
            let strip = Rect::new(r.x0, r.y0, r.x1, r.y0 + strip_h);
            self.draw_summary_strip(scene, t, tr, theme, strip, strip_g);
            let toolbar_h = self.toolbar_height(tr, theme, r.width());
            let toolbar = Rect::new(r.x0, strip.y1 + GAP, r.x1, strip.y1 + GAP + toolbar_h);
            self.draw_toolbar(scene, t, tr, theme, toolbar);
            let table_top = toolbar.y1 + GAP;
            let table_bottom = r.y1 - DETAIL_H - GAP;
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
            let strip = Rect::new(table_slab.x0, table_slab.y0, table_slab.x1, table_slab.y0 + strip_h);
            self.draw_summary_strip(scene, t, tr, theme, strip, strip_g);
            let toolbar_h = self.toolbar_height(tr, theme, table_slab.width());
            let toolbar = Rect::new(table_slab.x0, strip.y1 + GAP, table_slab.x1, strip.y1 + GAP + toolbar_h);
            self.draw_toolbar(scene, t, tr, theme, toolbar);
            let table_panel = Rect::new(table_slab.x0, toolbar.y1 + GAP, table_slab.x1, table_slab.y1);
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
    fn draw_process_table(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, table_panel: Rect) {
        let c = &theme.tokens.colors;
        let ti = charts::panel(scene, t, table_panel, theme, 0.0);
        let pad = layout::TABLE_PAD;
        // The columns' content viewport: the card inner box minus left/right
        // padding. Columns are laid out in *content space* (x from 0) and the
        // whole strip is shifted left by `table_scroll_x` when it overflows.
        let cols_left = ti.x0 + pad;
        let cols_right = ti.x1 - pad;
        let avail_w = (cols_right - cols_left).max(1.0);

        let columns = layout::process_columns(avail_w);
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
        let sx = self.table_scroll_x as f64;
        // Absolute x of a content-space column edge after the horizontal scroll.
        let col_abs = |x: f64| cols_left + x - sx;

        // The horizontal clip: header text and row cells are clipped to the
        // padded column band so an overflowing column (e.g. THR when narrow)
        // is hidden past the right edge instead of overlapping the border.
        let band = Rect::new(cols_left, ti.y0, cols_right, ti.y1);

        // Header row.
        let hdr = Rect::new(ti.x0, ti.y0, ti.x1, ti.y0 + TABLE_HEADER_H);
        charts::fill_rrect(scene, t, hdr, 0.0, c.surfaces.base);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &Line::new((hdr.x0, hdr.y1), (hdr.x1, hdr.y1)));
        let hfs = theme.tokens.typography.scale.xs;
        self.hit.sort_headers.clear();
        let hdr_clip = Rect::new(band.x0, hdr.y0, band.x1, hdr.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &hdr_clip);
        for col in &columns {
            let x0 = col_abs(col.x0);
            let active = self.sort_key == col.key;
            let color = if active { c.text_primary } else { c.text_dim };
            let mut text = col.header.to_string();
            if active {
                text.push(' ');
                text.push(if self.sort_desc { '▼' } else { '▲' });
            }
            let (lw, lh) = tr.measure_weighted(&text, hfs, None, TextWeight::Semibold);
            let tx = if col.right { x0 + col.w - lw as f64 - 6.0 } else { x0 + 2.0 };
            tr.draw_text_weighted(scene, &text, hfs, color, t * Affine::translate((tx, hdr.center().y - lh as f64 / 2.0)), None, TextWeight::Semibold);
            // Hit rect clamped to the visible band so a click maps to the
            // column under the cursor, not its off-screen extent.
            let hx0 = x0.max(band.x0);
            let hx1 = (x0 + col.w).min(band.x1);
            if hx1 > hx0 {
                self.hit.sort_headers.push((col.key, Rect::new(hx0, hdr.y0, hx1, hdr.y1)));
            }
        }
        scene.pop_layer();

        // Rows region (above any horizontal scrollbar).
        let view = Rect::new(ti.x0, hdr.y1, ti.x1, ti.y1 - hbar_h);
        self.rows_per_page = (view.height() / ROW_H).floor().max(1.0) as usize;
        let rows = self.visible_rows();
        let max_scroll = rows.len().saturating_sub(self.rows_per_page) as f32;
        self.scroll_off = self.scroll_off.clamp(0.0, max_scroll);
        let start = self.scroll_off as usize;
        // Record the viewport so wheel routing can tell table-scroll from
        // page-scroll.
        self.table_viewport = view;

        // Clip rows to the view (both axes — vertical for row scroll, and the
        // padded band horizontally so overflowing columns stay inside).
        let rows_clip = Rect::new(band.x0, view.y0, band.x1, view.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &rows_clip);
        self.hit.rows.clear();
        self.hit.twisties.clear();
        let fs = theme.tokens.typography.scale.sm;
        for i in 0..self.rows_per_page {
            let idx = start + i;
            let Some(p) = rows.get(idx) else { break };
            let y0 = view.y0 + i as f64 * ROW_H;
            let row = Rect::new(view.x0, y0, view.x1, y0 + ROW_H);
            let selected = self.selected_pid == Some(p.pid);
            if selected {
                charts::fill_rrect(scene, t, Rect::new(row.x0, row.y0, row.x1, row.y1), 0.0, with_alpha(c.accent[5], 60));
                let bar = Rect::new(row.x0, row.y0, row.x0 + 3.0, row.y1);
                scene.fill(Fill::NonZero, t, c.accent[5], None, &bar);
            } else if idx % 2 == 1 {
                charts::fill_rrect(scene, t, row, 0.0, c.neutral[2]);
            }

            let ty = row.center().y - fs as f64 / 2.0 - 1.0;
            let txt = if selected { c.text_primary } else { c.text_dim };
            let name_color = c.text_primary;

            // helper to draw a cell at a content-space column.
            let cell = |tr: &mut TextRenderer, scene: &mut Scene, s: &str, col: &layout::Column, color: Color, weight: TextWeight| {
                let x0 = col_abs(col.x0);
                let s = Self::truncate(tr, s, fs, col.w - 8.0);
                let (sw, _) = tr.measure_weighted(&s, fs, None, weight);
                let x = if col.right { x0 + col.w - sw as f64 - 6.0 } else { x0 + 2.0 };
                tr.draw_text_weighted(scene, &s, fs, color, t * Affine::translate((x, ty)), None, weight);
            };
            // Look a column up by key — the set is now width-dependent (USER is
            // dropped at narrow widths), so cells are addressed by identity, not
            // a fixed index.
            let column = |k: SortKey| columns.iter().find(|c| c.key == k);
            if let Some(col) = column(SortKey::Pid) {
                cell(tr, scene, &p.pid.to_string(), col, txt, TextWeight::Regular);
            }

            // NAME column: tree indent + twisty + name, all in the NAME band.
            let name_col = column(SortKey::Name).expect("NAME column is always shown");
            let indent = p.depth as f64 * TREE_INDENT;
            let name_x0 = col_abs(name_col.x0) + 2.0 + indent;
            if self.tree_mode && p.has_children {
                // Expand/collapse triangle (▸ collapsed, ▾ expanded).
                let tw = Rect::new(name_x0, row.y0, name_x0 + TWISTY_W, row.y1);
                Self::draw_twisty(scene, t, tw, p.expanded, if selected { c.text_primary } else { c.text_dim });
                // Hit rect for the twisty (clamped to the visible band).
                let hx0 = tw.x0.max(band.x0);
                let hx1 = tw.x1.min(band.x1);
                if hx1 > hx0 {
                    self.hit.twisties.push((p.pid, Rect::new(hx0, row.y0, hx1, row.y1)));
                }
            }
            let text_x0 = name_x0 + if self.tree_mode { TWISTY_W } else { 0.0 };
            // Width left for the name text after the indent + twisty.
            let name_avail = (col_abs(name_col.x0) + name_col.w - text_x0 - 4.0).max(8.0);
            let label = Self::truncate(tr, &p.name, fs, name_avail);
            tr.draw_text_weighted(scene, &label, fs, name_color, t * Affine::translate((text_x0, ty)), None, TextWeight::Medium);

            if let Some(col) = column(SortKey::User) {
                cell(tr, scene, &p.user, col, txt, TextWeight::Regular);
            }
            // CPU% coloured by load. A collapsed parent's summed total can
            // exceed 100% (multi-core); colour against a single core's scale
            // but clamp the hue so a hot subtree reads as fully loaded.
            let cpu_frac = (p.cpu / 100.0).clamp(0.0, 1.0);
            let cpu_col = if p.cpu >= 1.0 { charts::load_color(theme, cpu_frac) } else { txt };
            if let Some(col) = column(SortKey::Cpu) {
                cell(tr, scene, &format!("{:.1}", p.cpu), col, cpu_col, TextWeight::Medium);
            }
            if let Some(col) = column(SortKey::Mem) {
                cell(tr, scene, &fmt_bytes(p.mem_bytes), col, txt, TextWeight::Regular);
            }
            if let Some(col) = column(SortKey::Threads) {
                cell(tr, scene, &p.threads.to_string(), col, txt, TextWeight::Regular);
            }

            self.hit.rows.push((p.pid, row));
        }
        scene.pop_layer();

        // Vertical row scrollbar — a draggable thumb over a page-able track.
        if rows.len() > self.rows_per_page {
            let track = Rect::new(view.x1 - 8.0, view.y0, view.x1 - 1.0, view.y1);
            charts::fill_rrect(scene, t, track, 2.0, with_alpha(c.border_standard, 90));
            let frac_v = self.rows_per_page as f64 / rows.len() as f64;
            let thumb_h = (track.height() * frac_v).max(24.0);
            let max_off = (rows.len() - self.rows_per_page) as f64;
            let pos = if max_off > 0.0 { self.scroll_off as f64 / max_off } else { 0.0 };
            let ty0 = track.y0 + (track.height() - thumb_h) * pos;
            let thumb = Rect::new(track.x0, ty0, track.x1, ty0 + thumb_h);
            charts::fill_rrect(scene, t, thumb, 2.0, with_alpha(c.text_dim, 160));
            self.hit.v_scrollbar = Some(ScrollBarHit { track, thumb, max_value: max_off });
        } else {
            self.hit.v_scrollbar = None;
        }

        // Horizontal scrollbar — appears only when the columns overflow the
        // viewport (a narrow window), so THR no longer overlaps the border:
        // the user scrolls right to reach it instead.
        if h_overflow > 0.5 {
            let track = Rect::new(band.x0, ti.y1 - hbar_h + 1.0, band.x1, ti.y1 - 1.0);
            charts::fill_rrect(scene, t, track, 2.0, with_alpha(c.border_standard, 90));
            let frac_h = (avail_w / content_w).clamp(0.0, 1.0);
            let thumb_w = (track.width() * frac_h).max(24.0);
            let pos = (sx / h_overflow).clamp(0.0, 1.0);
            let tx0 = track.x0 + (track.width() - thumb_w) * pos;
            let thumb = Rect::new(tx0, track.y0, tx0 + thumb_w, track.y1);
            charts::fill_rrect(scene, t, thumb, 2.0, with_alpha(c.text_dim, 160));
            self.hit.h_scrollbar = Some(ScrollBarHit { track, thumb, max_value: h_overflow });
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
    fn draw_detail_pane(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect) {
        let c = &theme.tokens.colors;
        let bar_h = 36.0;

        // Back chip: chevron + "Processes", top-left of the pushed detail pane.
        let label = "Processes";
        let fs = theme.tokens.typography.scale.sm;
        let (lw, lh) = tr.measure_weighted(label, fs, None, TextWeight::Semibold);
        let back_x0 = r.x0;
        let back = Rect::new(back_x0, r.y0, back_x0 + 34.0 + lw as f64 + 14.0, r.y0 + bar_h - 4.0);
        charts::fill_rrect(scene, t, back, theme.tokens.radii.md as f64, c.surfaces.raised);
        charts::stroke_rrect(scene, t, back, theme.tokens.radii.md as f64, c.border_standard, 1.0);
        let chev = Rect::new(back.x0 + 8.0, back.center().y - 8.0, back.x0 + 24.0, back.center().y + 8.0);
        icons::draw_icon(scene, t, chev, Icon::ChevronLeft, c.text_primary, 1.8);
        tr.draw_text_weighted(scene, label, fs, c.text_primary, t * Affine::translate((chev.x1 + 4.0, back.center().y - lh as f64 / 2.0)), None, TextWeight::Semibold);
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
        let fs = theme.tokens.typography.scale.sm;
        // Kernel-threads toggle (switch knob + label).
        let mut w = tr.measure("kernel threads", fs, None).0 as f64 + 44.0;
        // Sum-mode pill exists only in tree mode.
        if self.tree_mode {
            let sum_text = format!("Σ {}", self.sum_mode.label());
            w += TOOLBAR_PILL_GAP + tr.measure(&sum_text, fs, None).0 as f64 + 24.0;
        }
        // Tree/Flat pill.
        let tree_text = if self.tree_mode { "⊟ Tree" } else { "≣ Flat" };
        w += TOOLBAR_PILL_GAP + tr.measure(tree_text, fs, None).0 as f64 + 24.0;
        // Confirm-kills pill (reserved so the toolbar stacks before the pill
        // cluster crowds the search box; dropped only at extreme-narrow widths).
        w += TOOLBAR_PILL_GAP + tr.measure("Confirm kills", fs, None).0 as f64 + 24.0;
        w
    }

    /// The toolbar stacks the search box onto its own row above the filter
    /// pills once a single row can no longer seat a minimum-width search box
    /// beside the pill cluster. Re-derived from the CURRENT width each frame,
    /// so resizing between one and two rows reflows immediately.
    fn toolbar_stacked(&self, tr: &mut TextRenderer, theme: &Theme, avail_w: f64) -> bool {
        avail_w - self.pills_cluster_width(tr, theme) - GAP < SEARCH_MIN_W
    }

    /// Height the toolbar occupies at `avail_w`: one row when the search box
    /// and pills fit side by side, two rows (search above pills) when narrow.
    fn toolbar_height(&self, tr: &mut TextRenderer, theme: &Theme, avail_w: f64) -> f64 {
        if self.toolbar_stacked(tr, theme, avail_w) {
            2.0 * TOOLBAR_ROW_H + TOOLBAR_ROW_GAP
        } else {
            TOOLBAR_ROW_H
        }
    }

    fn draw_toolbar(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect) {
        let c = &theme.tokens.colors;
        let fs = theme.tokens.typography.scale.sm;

        // One row (search beside pills) or two (search above pills). Deriving
        // this from `r.width()` — the width the host allocated this frame —
        // keeps the toolbar reflowing on every resize rather than freezing to
        // whatever the first frame chose.
        let stacked = self.toolbar_stacked(tr, theme, r.width());
        let pills_row = if stacked {
            Rect::new(r.x0, r.y1 - TOOLBAR_ROW_H, r.x1, r.y1)
        } else {
            Rect::new(r.x0, r.y0, r.x1, r.y0 + TOOLBAR_ROW_H)
        };
        let search_row = if stacked {
            Rect::new(r.x0, r.y0, r.x1, r.y0 + TOOLBAR_ROW_H)
        } else {
            pills_row
        };

        // ---- filter pills, laid right→left in `pills_row` -----------------
        // Kernel-threads toggle (rightmost).
        let kt_label = "kernel threads";
        let ktw = tr.measure(kt_label, fs, None).0 as f64 + 44.0;
        let kt_r = Rect::new(pills_row.x1 - ktw, pills_row.y0 + 2.0, pills_row.x1, pills_row.y1 - 2.0);
        let on = self.prefs.show_kernel_threads;
        charts::fill_rrect(scene, t, kt_r, kt_r.height() / 2.0, if on { with_alpha(c.accent[5], 60) } else { c.surface_white_5 });
        charts::stroke_rrect(scene, t, kt_r, kt_r.height() / 2.0, if on { c.accent[5] } else { c.border_standard }, 1.0);
        // little switch knob
        let knob_track = Rect::new(kt_r.x0 + 10.0, kt_r.center().y - 6.0, kt_r.x0 + 30.0, kt_r.center().y + 6.0);
        charts::fill_rrect(scene, t, knob_track, 6.0, if on { c.accent[5] } else { with_alpha(c.text_dim, 120) });
        let knob_cx = if on { knob_track.x1 - 6.0 } else { knob_track.x0 + 6.0 };
        scene.fill(Fill::NonZero, t, Color::WHITE, None, &vello::kurbo::Circle::new(Point::new(knob_cx, knob_track.center().y), 5.0));
        tr.draw_text(scene, kt_label, fs, if on { c.text_primary } else { c.text_dim }, t * Affine::translate((knob_track.x1 + 6.0, kt_r.center().y - fs as f64 / 2.0 - 1.0)), None);
        self.hit.kernel_toggle = kt_r;

        // Sum-mode pill, left of the kernel toggle: "Σ Sum subtree" /
        // "Σ Per-process". Active (Sum) is accent-filled. Hidden in flat
        // mode (there's nothing to sum into when there's no tree).
        let mut next_x1 = kt_r.x0 - TOOLBAR_PILL_GAP;
        if self.tree_mode {
            let sum_label = self.sum_mode.label();
            let sum_text = format!("Σ {sum_label}");
            let sw = tr.measure(&sum_text, fs, None).0 as f64 + 24.0;
            let sum_r = Rect::new(next_x1 - sw, pills_row.y0 + 2.0, next_x1, pills_row.y1 - 2.0);
            let summing = self.sum_mode == SumMode::SumSubtree;
            charts::fill_rrect(scene, t, sum_r, sum_r.height() / 2.0, if summing { with_alpha(c.accent[5], 60) } else { c.surface_white_5 });
            charts::stroke_rrect(scene, t, sum_r, sum_r.height() / 2.0, if summing { c.accent[5] } else { c.border_standard }, 1.0);
            let (stw, _) = tr.measure(&sum_text, fs, None);
            tr.draw_text(scene, &sum_text, fs, if summing { c.text_primary } else { c.text_dim }, t * Affine::translate((sum_r.center().x - stw as f64 / 2.0, sum_r.center().y - fs as f64 / 2.0 - 1.0)), None);
            self.hit.sum_toggle = sum_r;
            next_x1 = sum_r.x0 - TOOLBAR_PILL_GAP;
        } else {
            self.hit.sum_toggle = Rect::ZERO;
        }

        // Tree/Flat view pill, left of the sum pill.
        let tree_text = if self.tree_mode { "⊟ Tree" } else { "≣ Flat" };
        let tw_ = tr.measure(tree_text, fs, None).0 as f64 + 24.0;
        let tree_r = Rect::new(next_x1 - tw_, pills_row.y0 + 2.0, next_x1, pills_row.y1 - 2.0);
        charts::fill_rrect(scene, t, tree_r, tree_r.height() / 2.0, if self.tree_mode { with_alpha(c.accent[5], 60) } else { c.surface_white_5 });
        charts::stroke_rrect(scene, t, tree_r, tree_r.height() / 2.0, if self.tree_mode { c.accent[5] } else { c.border_standard }, 1.0);
        let (ttw, _) = tr.measure(tree_text, fs, None);
        tr.draw_text(scene, tree_text, fs, if self.tree_mode { c.text_primary } else { c.text_dim }, t * Affine::translate((tree_r.center().x - ttw as f64 / 2.0, tree_r.center().y - fs as f64 / 2.0 - 1.0)), None);
        self.hit.tree_toggle = tree_r;

        // Confirm-kills toggle pill, left of the tree pill. Accent-filled when
        // on. This is the only affordance that reaches the persisted
        // `confirm_kill` preference. Drawn whenever it fits within the pills
        // row; at extreme-narrow widths (where the toolbar is already stacked)
        // it's dropped rather than clipped off the left edge.
        let confirm_text = "Confirm kills";
        let cfw = tr.measure(confirm_text, fs, None).0 as f64 + 24.0;
        let confirm_x1 = tree_r.x0 - TOOLBAR_PILL_GAP;
        let confirm_x0 = confirm_x1 - cfw;
        // Left edge of the pill cluster — the search box stops before this so
        // the box and the pills can never overlap however narrow the window.
        let cluster_left = if confirm_x0 >= pills_row.x0 {
            let confirm_r = Rect::new(confirm_x0, pills_row.y0 + 2.0, confirm_x1, pills_row.y1 - 2.0);
            let ck = self.prefs.confirm_kill;
            charts::fill_rrect(scene, t, confirm_r, confirm_r.height() / 2.0, if ck { with_alpha(c.accent[5], 60) } else { c.surface_white_5 });
            charts::stroke_rrect(scene, t, confirm_r, confirm_r.height() / 2.0, if ck { c.accent[5] } else { c.border_standard }, 1.0);
            let (cftw, _) = tr.measure(confirm_text, fs, None);
            tr.draw_text(scene, confirm_text, fs, if ck { c.text_primary } else { c.text_dim }, t * Affine::translate((confirm_r.center().x - cftw as f64 / 2.0, confirm_r.center().y - fs as f64 / 2.0 - 1.0)), None);
            self.hit.confirm_toggle = confirm_r;
            confirm_r.x0
        } else {
            self.hit.confirm_toggle = Rect::ZERO;
            tree_r.x0
        };

        // ---- search box in `search_row` -----------------------------------
        // Stacked: full width on its own row. Side-by-side: fill from the left
        // up to the pill cluster (capped at a comfortable preferred width), so
        // the box and the pills can never overlap however narrow the window.
        let search_r = if stacked {
            Rect::new(search_row.x0, search_row.y0, search_row.x1, search_row.y1)
        } else {
            let max_right = cluster_left - GAP;
            let right = (search_row.x0 + SEARCH_PREF_W).min(max_right);
            Rect::new(search_row.x0, search_row.y0, right, search_row.y1)
        };
        // The shared SearchField owns the pill chrome, magnifier, placeholder,
        // caret/selection/clipboard, and clear button. Monitor's search is not
        // a collaborative surface, so it draws with empty presence (matching the
        // app's own presence-free draw).
        let presence = PresenceCtx::empty();
        self.search.apply_layout_external(search_r);
        self.search.draw(scene, t, tr, theme, &presence);
        self.hit.search = search_r;

        // Count label centred in the slack between the search box and the pill
        // cluster — only on a single row, and only when it genuinely fits, so
        // it never overlaps a pill.
        if !stacked {
            let total = self.model.processes.iter().filter(|p| self.prefs.show_kernel_threads || !p.kernel).count();
            let shown = self.visible_rows().len();
            let count = format!("{shown} of {total} shown");
            let (cw, _) = tr.measure(&count, fs, None);
            let cxx = (search_r.x1 + cluster_left) / 2.0 - cw as f64 / 2.0;
            if cluster_left - (search_r.x1 + 10.0) > cw as f64 + 8.0 {
                tr.draw_text(scene, &count, fs, c.text_dim, t * Affine::translate((cxx.max(search_r.x1 + 10.0), pills_row.center().y - fs as f64 / 2.0 - 1.0)), None);
            }
        }
    }

    /// The per-process detail pane. Reserves a pinned action bar (Stop + Kill)
    /// at the bottom so process content can never collide with the buttons.
    /// When the pane is tall (the narrow pushed detail), fills the space with
    /// the command line, identity, a live CPU sparkline and a two-column stat
    /// grid; when it's the short wide strip, keeps a compact chip row clear of
    /// the reserved buttons.
    fn draw_detail(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect) {
        let c = &theme.tokens.colors;
        let inner = charts::panel(scene, t, r, theme, 12.0);
        self.hit.kill_btn = Rect::ZERO;
        self.hit.stop_btn = Rect::ZERO;
        let Some(p) = self.selected_process() else {
            tr.draw_text(scene, "Select a process to view details and actions.", theme.tokens.typography.scale.sm, c.text_dim, t * Affine::translate((inner.x0, inner.center().y - 8.0)), None);
            return;
        };

        let xs = theme.tokens.typography.scale.xs;
        let sm = theme.tokens.typography.scale.sm;
        let base = theme.tokens.typography.scale.base;
        let lg = theme.tokens.typography.scale.lg;
        let ppid = p.ppid.map(|x| x.to_string()).unwrap_or_else(|| "—".into());
        let cpu_col = charts::load_color(theme, (p.cpu / 100.0).clamp(0.0, 1.0));

        let tall = inner.height() > 150.0;

        if tall {
            // ---- full detail: reserve a bottom action bar, fill above it ----
            let bar = Rect::new(inner.x0, inner.y1 - DETAIL_ACTION_H, inner.x1, inner.y1);
            let content = Rect::new(inner.x0, inner.y0, inner.x1, bar.y0 - 12.0);
            let mut y = content.y0;

            // Header: name + identity line.
            tr.draw_text_weighted(scene, &p.name, lg, c.text_primary, t * Affine::translate((content.x0, y)), None, TextWeight::Bold);
            y += lg as f64 + 8.0;
            let ident = format!("PID {}   ·   PPID {}   ·   {}", p.pid, ppid, p.user);
            tr.draw_text(scene, &ident, xs, c.text_dim, t * Affine::translate((content.x0, y)), None);
            y += xs as f64 + 14.0;

            // Command line.
            tr.draw_text_weighted(scene, "COMMAND", xs, c.text_dim, t * Affine::translate((content.x0, y)), None, TextWeight::Semibold);
            y += xs as f64 + 4.0;
            let cmd = Self::truncate(tr, &p.cmd, sm, content.width());
            tr.draw_text(scene, &cmd, sm, c.text_primary, t * Affine::translate((content.x0, y)), None);
            y += sm as f64 + 14.0;

            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &Line::new((content.x0, y), (content.x1, y)));
            y += 14.0;

            // CPU sparkline block: label + current value over a mini area line.
            tr.draw_text_weighted(scene, "CPU", xs, c.text_dim, t * Affine::translate((content.x0, y)), None, TextWeight::Semibold);
            let cpu_val = format!("{:.1}%", p.cpu);
            let (cvw, _) = tr.measure_weighted(&cpu_val, base, None, TextWeight::Bold);
            tr.draw_text_weighted(scene, &cpu_val, base, cpu_col, t * Affine::translate((content.x1 - cvw as f64, y - 2.0)), None, TextWeight::Bold);
            y += base as f64 + 6.0;
            // The stat grid gets a fixed, comfortable stride; the sparkline
            // absorbs whatever vertical slack remains so a tall pane fills
            // top-to-bottom cleanly instead of leaving gaps between the rows.
            let stats: [(&str, String, Color); 6] = [
                ("MEMORY (RSS)", fmt_bytes(p.mem_bytes), c.text_primary),
                ("VIRTUAL", fmt_bytes(p.virt_bytes), c.text_primary),
                ("THREADS", p.threads.to_string(), c.text_primary),
                ("OPEN FILES", p.open_files.map(|n| n.to_string()).unwrap_or_else(|| "—".into()), c.text_primary),
                ("STATUS", p.status.clone(), c.semantic.info),
                ("RUNNING FOR", fmt_uptime(p.run_secs), c.text_primary),
            ];
            let cols = 2usize;
            let grid_rows = stats.len().div_ceil(cols);
            let stat_row_h = 48.0;
            let grid_h = grid_rows as f64 * stat_row_h;
            // Sparkline fills the space between the CPU value line and the grid
            // (minus the two 14px divider gaps around it), floored so it stays
            // legible on a short pane.
            let spark_h = (content.y1 - y - grid_h - 28.0).max(46.0);
            let spark = Rect::new(content.x0, y, content.x1, y + spark_h);
            charts::fill_rrect(scene, t, spark, 6.0, with_alpha(c.border_standard, 40));
            if let Some(ring) = self.proc_cpu_hist.get(&p.pid) {
                let smax = ring.max().max(100.0);
                charts::sparkline(scene, t, layout::inner(spark, 5.0), ring.as_slice(), smax, cpu_col);
            }
            y += spark_h + 14.0;

            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &Line::new((content.x0, y), (content.x1, y)));
            y += 14.0;

            let col_gap = 18.0;
            let cell_w = (content.width() - (cols as f64 - 1.0) * col_gap) / cols as f64;
            for (i, (label, value, color)) in stats.iter().enumerate() {
                let col = i % cols;
                let row = i / cols;
                let cx = content.x0 + col as f64 * (cell_w + col_gap);
                let cy = y + row as f64 * stat_row_h;
                tr.draw_text(scene, label, xs, c.text_dim, t * Affine::translate((cx, cy)), None);
                let v = Self::truncate(tr, value, base, cell_w);
                tr.draw_text_weighted(scene, &v, base, *color, t * Affine::translate((cx, cy + xs as f64 + 5.0)), None, TextWeight::Semibold);
            }

            // Reserved action bar: hairline top, right-aligned Stop + Kill.
            scene.stroke(&Stroke::new(1.0), t, with_alpha(c.border_standard, 120), None, &Line::new((bar.x0, bar.y0), (bar.x1, bar.y0)));
            self.draw_detail_actions(scene, t, tr, theme, bar);
        } else {
            // ---- compact strip: chips clamped left of a button cluster ------
            let cluster_w = 92.0 + 8.0 + 92.0;
            let bar = Rect::new(inner.x1 - cluster_w, inner.y1 - 32.0, inner.x1, inner.y1);
            let chips_right = bar.x0 - 14.0;

            tr.draw_text_weighted(scene, &p.name, lg, c.text_primary, t * Affine::translate((inner.x0, inner.y0)), None, TextWeight::Bold);
            let ident = format!("PID {}  ·  PPID {}  ·  {}  ·  {}", p.pid, ppid, p.user, p.status);
            let ident = Self::truncate(tr, &ident, xs, chips_right - inner.x0);
            tr.draw_text(scene, &ident, xs, c.text_dim, t * Affine::translate((inner.x0, inner.y0 + lg as f64 + 4.0)), None);

            // Chip row along the bottom, stopping before the button cluster.
            let chip = |scene: &mut Scene, tr: &mut TextRenderer, x: f64, label: &str, value: &str, color: Color| -> f64 {
                if x > chips_right {
                    return x;
                }
                tr.draw_text(scene, label, xs, c.text_dim, t * Affine::translate((x, inner.y1 - 30.0)), None);
                tr.draw_text_weighted(scene, value, base, color, t * Affine::translate((x, inner.y1 - 16.0)), None, TextWeight::Bold);
                let w1 = tr.measure(label, xs, None).0 as f64;
                let w2 = tr.measure_weighted(value, base, None, TextWeight::Bold).0 as f64;
                x + w1.max(w2) + 24.0
            };
            let mut x = inner.x0;
            x = chip(scene, tr, x, "CPU", &format!("{:.1}%", p.cpu), cpu_col);
            x = chip(scene, tr, x, "RSS", &fmt_bytes(p.mem_bytes), c.text_primary);
            x = chip(scene, tr, x, "VIRT", &fmt_bytes(p.virt_bytes), c.text_primary);
            x = chip(scene, tr, x, "THREADS", &p.threads.to_string(), c.text_primary);
            let _ = chip(scene, tr, x, "FILES", &p.open_files.map(|n| n.to_string()).unwrap_or_else(|| "—".into()), c.text_primary);

            self.draw_detail_actions(scene, t, tr, theme, bar);
        }
    }

    /// Draw the detail pane's Stop (SIGTERM) + Kill (SIGKILL) buttons,
    /// right-aligned inside `bar`, and record their hit rects. Both fit within
    /// `bar` so they occupy only the reserved area.
    fn draw_detail_actions(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, bar: Rect) {
        let c = &theme.tokens.colors;
        let bh = (bar.height() - 8.0).clamp(26.0, 34.0);
        let by0 = bar.center().y - bh / 2.0;
        let bw = 92.0_f64.min((bar.width() - 8.0) / 2.0);
        let kill_r = Rect::new(bar.x1 - bw, by0, bar.x1, by0 + bh);
        let stop_r = Rect::new(kill_r.x0 - 8.0 - bw, by0, kill_r.x0 - 8.0, by0 + bh);
        let cur = self.last_cursor;
        Self::draw_action_button(scene, t, tr, theme, stop_r, "Stop", Icon::Power, ButtonStyle::Neutral, stop_r.contains(cur));
        Self::draw_action_button(scene, t, tr, theme, kill_r, "Kill", Icon::X, ButtonStyle::Danger, kill_r.contains(cur));
        self.hit.stop_btn = stop_r;
        self.hit.kill_btn = kill_r;
    }

    /// One rounded action button (the shared Button face): a centred
    /// icon-then-label pair. `Neutral` for Stop, `Danger` for the destructive
    /// Kill; hover/press feedback comes from the themed face.
    fn draw_action_button(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, label: &str, icon: Icon, style: ButtonStyle, hovered: bool) {
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(rect, label)
                .radius(theme.tokens.radii.md as f64)
                .text_size(theme.tokens.typography.scale.sm)
                .weight(TextWeight::Semibold)
                .icon(icon)
                .style(style)
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
    fn draw_resources(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect) {
        let c = &theme.tokens.colors;

        // ---- overview row: Processor / Memory / System --------------------
        let mut y = r.y0;
        if layout::overview_cols(r.width()) == 3 {
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
            self.draw_processor_card(scene, t, tr, theme, Rect::new(r.x0, y, r.x1, y + layout::OVERVIEW_RING_H));
            y += layout::OVERVIEW_RING_H + GAP;
            self.draw_memory_card(scene, t, tr, theme, Rect::new(r.x0, y, r.x1, y + layout::OVERVIEW_MEM_H));
            y += layout::OVERVIEW_MEM_H + GAP;
            self.draw_system_card(scene, t, tr, theme, Rect::new(r.x0, y, r.x1, y + layout::OVERVIEW_SYS_H));
            y += layout::OVERVIEW_SYS_H + GAP;
        }

        // ---- per-core utilisation grid (full width) -----------------------
        let n = self.model.metrics.cores.len().max(1);
        let grid = layout::per_core_grid(n, r.width() - 2.0 * layout::RAIL_CARD_PAD);
        let core_h = layout::per_core_card_h(grid);
        self.draw_per_core(scene, t, tr, theme, Rect::new(r.x0, y, r.x1, y + core_h), grid);
        y += core_h + GAP;

        // ---- chart grid: CPU / Memory / Network / Disk --------------------
        // Two-up when wide, one column when narrow. Each cell keeps a legible
        // minimum height; when the viewport is short the page scrolls.
        let cols = layout::charts_cols(r.width());
        let rows = layout::charts_rows(r.width());
        let min_chart_h = 150.0;
        let charts_top = y;
        let avail_h = (r.y1 - charts_top).max(rows as f64 * min_chart_h + (rows as f64 - 1.0) * GAP);
        let cw = (r.width() - (cols as f64 - 1.0) * GAP) / cols as f64;
        let ch = (avail_h - (rows as f64 - 1.0) * GAP) / rows as f64;
        let cell = |i: usize| {
            let col = i % cols;
            let row = i / cols;
            let x0 = r.x0 + col as f64 * (cw + GAP);
            let y0 = charts_top + row as f64 * (ch + GAP);
            Rect::new(x0, y0, x0 + cw, y0 + ch)
        };

        let m = &self.model.metrics;
        // CPU%.
        charts::area_chart(
            scene, t, tr, cell(0), theme, "CPU",
            &[Series { data: m.cpu_hist.as_slice(), color: c.accent[5], legend: format!("{:.0}%", m.cpu_overall()) }],
            100.0, "100%", "0%",
        );
        // Memory%.
        charts::area_chart(
            scene, t, tr, cell(1), theme, "Memory",
            &[Series { data: m.mem_hist.as_slice(), color: c.semantic.success, legend: fmt_bytes(m.mem_used) }],
            1.0, "100%", "0%",
        );
        // Network rx/tx.
        let net_max = m.net_rx_hist.max().max(m.net_tx_hist.max()).max(1.0) * 1.2;
        charts::area_chart(
            scene, t, tr, cell(2), theme, "Network",
            &[
                Series { data: m.net_rx_hist.as_slice(), color: c.semantic.info, legend: format!("↓ {}", fmt_rate(m.net_rx)) },
                Series { data: m.net_tx_hist.as_slice(), color: c.accent[6], legend: format!("↑ {}", fmt_rate(m.net_tx)) },
            ],
            net_max, &fmt_rate(net_max), "0",
        );
        // Disk read/write.
        let disk_max = m.disk_read_hist.max().max(m.disk_write_hist.max()).max(1.0) * 1.2;
        charts::area_chart(
            scene, t, tr, cell(3), theme, "Disk I/O",
            &[
                Series { data: m.disk_read_hist.as_slice(), color: c.semantic.warning, legend: format!("R {}", fmt_rate(m.disk_read)) },
                Series { data: m.disk_write_hist.as_slice(), color: c.semantic.danger, legend: format!("W {}", fmt_rate(m.disk_write)) },
            ],
            disk_max, &fmt_rate(disk_max), "0",
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
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "process".into());
        let (title, signal_name, confirm_label) = match kind {
            StopKind::Term => ("Stop process?", "SIGTERM", "Stop"),
            StopKind::Kill => ("Kill process?", "SIGKILL", "Kill"),
        };
        let body = format!("{name} (PID {pid}) will be sent {signal_name}. Unsaved work will be lost.");
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
        ConfirmDialog { dialog, confirm, cancel }
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
        let Some(cd) = self.confirm_dialog.as_mut() else { return };
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = cd.dialog.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(self.rect.width() as f32),
                        height: Dimension::length(self.rect.height() as f32),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        let _ = taffy.compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(self.rect.width() as f32),
                height: taffy::AvailableSpace::Definite(self.rect.height() as f32),
            },
        );
        cd.dialog
            .apply_layout(&taffy, node, Point::new(self.rect.x0, self.rect.y0));
    }

    /// Route an event into the open confirm dialog and act on the flags its
    /// callbacks set. Enter is a submit convenience; the dialog itself owns
    /// Escape, the backdrop click, and the footer buttons.
    fn route_confirm_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        let Some((pid, kind)) = self.confirm_stop else { return };
        if let WindowEvent::KeyboardInput { event: ke, .. } = event {
            if ke.state == ElementState::Pressed
                && matches!(ke.logical_key, Key::Named(NamedKey::Enter))
            {
                self.do_stop(pid, kind, doc);
                *handled = true;
                return;
            }
        }
        self.ensure_confirm_dialog(pid, kind);
        self.layout_confirm_dialog();
        let (confirm, cancel) = {
            let Some(cd) = self.confirm_dialog.as_mut() else { return };
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut focus_target = None;
            cd.dialog.handle_event(
                event, scale_factor, modifiers, doc, path, handled,
                &mut focus_target, node, &taffy,
            );
            (cd.confirm.replace(false), cd.cancel.replace(false))
        };
        if confirm {
            self.do_stop(pid, kind, doc);
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
        let c = &theme.tokens.colors;
        let label = format!("PID {pid} copied to clipboard");
        let fs = theme.tokens.typography.scale.sm;
        let (lw, lh) = tr.measure_weighted(&label, fs, None, TextWeight::Medium);
        let pad = 16.0;
        let icon_w = 22.0;
        let w = lw as f64 + pad * 2.0 + icon_w;
        let h = 34.0;
        let cx = self.rect.center().x;
        let x0 = cx - w / 2.0;
        let y1 = self.rect.y1 - OUTER_PAD - 6.0;
        let y0 = y1 - h;
        let toast = Rect::new(x0, y0, x0 + w, y1);
        charts::fill_rrect(scene, t, toast, h / 2.0, c.surfaces.raised);
        charts::stroke_rrect(scene, t, toast, h / 2.0, c.border_standard, 1.0);
        let icon_r = Rect::new(toast.x0 + pad, toast.center().y - 8.0, toast.x0 + pad + 16.0, toast.center().y + 8.0);
        icons::draw_icon(scene, t, icon_r, Icon::Check, c.semantic.success, 1.8);
        tr.draw_text_weighted(scene, &label, fs, c.text_primary, t * Affine::translate((icon_r.x1 + 8.0, toast.center().y - lh as f64 / 2.0)), None, TextWeight::Medium);
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
        if let Some(sb) = self.hit.page_scrollbar {
            if sb.track.contains(p) {
                if sb.thumb.contains(p) {
                    self.scroll_drag = Some(ScrollDrag::Page { grab: p.y - sb.thumb.y0 });
                } else {
                    let dir = if p.y < sb.thumb.y0 { -1.0 } else { 1.0 };
                    let page = (self.viewport_h * 0.9) as f32;
                    let max_y = sb.max_value as f32;
                    self.view_scroll_y = (self.view_scroll_y + dir * page).clamp(0.0, max_y);
                }
                return true;
            }
        }
        // Vertical row bar. Present only on the Processes tab.
        if let Some(sb) = self.hit.v_scrollbar {
            if sb.track.contains(p) {
                if sb.thumb.contains(p) {
                    self.scroll_drag = Some(ScrollDrag::Rows { grab: p.y - sb.thumb.y0 });
                } else {
                    let dir = if p.y < sb.thumb.y0 { -1.0 } else { 1.0 };
                    self.scroll_off = (self.scroll_off + dir * self.rows_per_page as f32).max(0.0);
                    self.clamp_scroll();
                }
                return true;
            }
        }
        // Horizontal column bar.
        if let Some(sb) = self.hit.h_scrollbar {
            if sb.track.contains(p) {
                if sb.thumb.contains(p) {
                    self.scroll_drag = Some(ScrollDrag::Cols { grab: p.x - sb.thumb.x0 });
                } else {
                    let dir = if p.x < sb.thumb.x0 { -1.0 } else { 1.0 };
                    let max_x = sb.max_value as f32;
                    self.table_scroll_x = (self.table_scroll_x + dir * self.table_cols_w as f32).clamp(0.0, max_x);
                }
                return true;
            }
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

        // An open interval popover captures clicks: a press on an option row
        // selects that cadence, a press anywhere else dismisses. Either way the
        // click is consumed so it doesn't fall through to the content.
        if self.interval_popover.is_open() {
            if let Some(idx) = self.interval_popover.item_at(p) {
                self.interval_popover.select(idx);
                self.apply_interval_selection(doc, path);
            } else {
                self.interval_popover.close();
            }
            *handled = true;
            return;
        }

        // An open context menu floats above the rest of the content. A click
        // on one of its action rows dispatches the action (and the action's
        // callback records a `ProcAction` we then drain + apply); a click
        // anywhere else dismisses it. Either way the click is consumed.
        if self.context.is_open() {
            let mut hit_idx = None;
            for (idx, rect) in self.context_menu_item_rects() {
                if rect.contains(p) {
                    hit_idx = Some(idx);
                    break;
                }
            }
            if let Some(idx) = hit_idx {
                self.context.click_item(idx); // fires the callback → pending_action
                let action = self.pending_action.lock().ok().and_then(|mut g| g.take());
                if let Some(action) = action {
                    self.apply_action(action, doc, path);
                }
            }
            self.close_context_menu();
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

        // Lose search focus unless the search box itself is clicked (clicks
        // inside the box are routed to the SearchField in `handle_event`, which
        // focuses it and positions the caret).
        let clicked_search = self.hit.search.contains(p);
        self.search.set_focused(clicked_search);

        // Back affordance in the narrow pushed detail pane → return to table.
        if self.hit.detail_back.contains(p) {
            self.proc_push.show_table();
            *handled = true;
            return;
        }

        // A click on a summary-strip cell (Processes) jumps to the Resources
        // dashboard AND scrolls that stat's own chart into view — so the four
        // cells land on four different charts rather than all dumping the user
        // at the top.
        for (i, cell) in self.hit.summary_cells.clone().into_iter().enumerate() {
            if cell.contains(p) {
                self.tab = Tab::Resources;
                let (body_w, viewport_h) = self.body_metrics();
                self.view_scroll_y = self.resources_scroll_for_stat(i, body_w, viewport_h);
                *handled = true;
                return;
            }
        }

        if self.hit.tab_proc.contains(p) {
            if self.tab != Tab::Processes {
                self.view_scroll_y = 0.0;
            }
            self.tab = Tab::Processes;
            *handled = true;
            return;
        }
        if self.hit.tab_res.contains(p) {
            if self.tab != Tab::Resources {
                self.view_scroll_y = 0.0;
            }
            self.tab = Tab::Resources;
            *handled = true;
            return;
        }
        if self.hit.interval.contains(p) {
            // The interval pill distinguishes a quick click (cycle) from a
            // press-and-hold (open the option popover). Start the hold timer on
            // press; `draw` opens the popover once it crosses the threshold, and
            // `release_interval_press` cycles on an early release.
            self.interval_press = Some(Instant::now());
            self.interval_hold_opened = false;
            *handled = true;
            return;
        }

        if self.tab == Tab::Processes {
            if clicked_search {
                *handled = true;
                return;
            }
            if self.hit.kernel_toggle.contains(p) {
                self.prefs.show_kernel_threads = !self.prefs.show_kernel_threads;
                self.prefs.store(doc, path);
                self.clamp_scroll();
                *handled = true;
                return;
            }
            // Confirm-kills preference toggle (persisted).
            if self.hit.confirm_toggle.contains(p) {
                self.prefs.confirm_kill = !self.prefs.confirm_kill;
                self.prefs.store(doc, path);
                *handled = true;
                return;
            }
            // Tree/Flat view toggle.
            if self.hit.tree_toggle.contains(p) {
                self.tree_mode = !self.tree_mode;
                self.scroll_off = 0.0;
                self.clamp_scroll();
                *handled = true;
                return;
            }
            // Sum subtree / Per-process toggle (tree mode only).
            if self.tree_mode && self.hit.sum_toggle.contains(p) {
                self.sum_mode = self.sum_mode.toggled();
                *handled = true;
                return;
            }
            for (key, rect) in self.hit.sort_headers.clone() {
                if rect.contains(p) {
                    self.set_sort(key);
                    *handled = true;
                    return;
                }
            }
            if self.hit.kill_btn.contains(p) {
                if let Some(pid) = self.selected_pid {
                    self.request_kill(pid, doc, path);
                }
                *handled = true;
                return;
            }
            if self.hit.stop_btn.contains(p) {
                if let Some(pid) = self.selected_pid {
                    self.request_stop(pid, doc);
                }
                *handled = true;
                return;
            }
            // Expand/collapse triangles take precedence over the row body, so
            // clicking a twisty toggles the subtree without selecting/pushing.
            for (pid, rect) in self.hit.twisties.clone() {
                if rect.contains(p) {
                    self.toggle_expand(pid);
                    *handled = true;
                    return;
                }
            }
            for (pid, rect) in self.hit.rows.clone() {
                if rect.contains(p) {
                    self.selected_pid = Some(pid);
                    // On narrow windows the detail is its own pushed pane;
                    // selecting a row slides it in.
                    if self.is_narrow() {
                        self.proc_push.show_detail();
                    }
                    *handled = true;
                    return;
                }
            }
        }
    }

    /// A right-click (secondary button) over a process row opens the action
    /// context menu anchored at the cursor. Right-clicking empty space (or
    /// off the Processes tab) is ignored. Returns whether it was consumed.
    fn on_right_click(&mut self, p: Point, _doc: &mut AutoCommit, _path: &ObjId, handled: &mut bool) {
        // A right-click anywhere first dismisses an open menu.
        if self.context.is_open() {
            self.close_context_menu();
        }
        if self.tab != Tab::Processes {
            return;
        }
        for (pid, rect) in self.hit.rows.clone() {
            if rect.contains(p) {
                self.open_context_menu(pid, p);
                *handled = true;
                return;
            }
        }
    }

    fn move_selection(&mut self, delta: i32) {
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
        self.selected_pid = Some(procs[next].pid);
        // Keep the selection in view.
        let next_f = next as f32;
        if next_f < self.scroll_off {
            self.scroll_off = next_f;
        } else if next_f >= self.scroll_off + self.rows_per_page as f32 {
            self.scroll_off = next_f - self.rows_per_page as f32 + 1.0;
        }
        self.clamp_scroll();
    }

    fn on_key(&mut self, key: &Key, _text: Option<&str>, ctrl: bool, doc: &mut AutoCommit, path: &ObjId, handled: &mut bool) {
        // Global-search seam: leave Ctrl+K (and Ctrl+/) untouched so the
        // forthcoming shared global-search framework can claim it. We do NOT
        // build a bespoke search here — just refuse to swallow the chord so
        // it bubbles up unhandled. (Bare "/" still focuses the local process
        // filter below; only the Ctrl-modified chord is reserved.)
        if ctrl {
            if let Key::Character(s) = key {
                if matches!(s.as_str(), "k" | "K" | "/") {
                    // `handled` stays false → event propagates to the host.
                    return;
                }
            }
        }

        // Interval popover open: full keyboard nav (↑/↓/Home/End move, Enter
        // selects, Esc closes). Every key is swallowed so none leaks to the
        // table behind the overlay — the primitive's focus-capture contract.
        if self.interval_popover.is_open() {
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
            *handled = true;
            return;
        }

        // Context menu open: Esc dismisses it, everything else is swallowed.
        if self.context.is_open() {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.close_context_menu();
            }
            *handled = true;
            return;
        }

        // The stop-confirmation modal is handled in `handle_event` before
        // dispatch reaches here, so `on_key` never runs with a confirm pending.

        // Search editing while the field is focused is handled in
        // `handle_event`, which routes keystrokes to the shared SearchField
        // (and never reaches `on_key` for those). So here we only handle the
        // global keys below.

        // Global keys.
        match key {
            // Esc pops a pushed detail pane back to the table (narrow).
            Key::Named(NamedKey::Escape) => {
                if self.is_narrow() && self.proc_push.pane() == ProcPane::Detail {
                    self.proc_push.show_table();
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Tab) => {
                self.tab = self.tab.other();
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) if self.tab == Tab::Processes => {
                self.move_selection(-1);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) if self.tab == Tab::Processes => {
                self.move_selection(1);
                *handled = true;
            }
            // Right/Left expand/collapse the selected tree node (when it has
            // children); a no-op in flat mode.
            Key::Named(NamedKey::ArrowRight)
                if self.tab == Tab::Processes && self.tree_mode =>
            {
                if let Some(pid) = self.selected_pid {
                    if self.row_has_children(pid) && self.expanded.insert(pid) {
                        self.clamp_scroll();
                    }
                    *handled = true;
                }
            }
            Key::Named(NamedKey::ArrowLeft)
                if self.tab == Tab::Processes && self.tree_mode =>
            {
                if let Some(pid) = self.selected_pid {
                    if self.expanded.remove(&pid) {
                        self.clamp_scroll();
                    }
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Enter) if self.tab == Tab::Processes => {
                if let Some(pid) = self.selected_pid {
                    self.request_kill(pid, doc, path);
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Delete) if self.tab == Tab::Processes => {
                if let Some(pid) = self.selected_pid {
                    self.request_kill(pid, doc, path);
                    *handled = true;
                }
            }
            Key::Character(s) => {
                match s.as_str() {
                    "1" => self.tab = Tab::Processes,
                    "2" => self.tab = Tab::Resources,
                    "/" => {
                        self.tab = Tab::Processes;
                        self.search.set_focused(true);
                    }
                    "c" | "C" => self.set_sort(SortKey::Cpu),
                    "m" | "M" => self.set_sort(SortKey::Mem),
                    "p" | "P" => self.set_sort(SortKey::Pid),
                    "n" | "N" => self.set_sort(SortKey::Name),
                    // "s" sends SIGTERM (Stop) to the selection; "g" toggles
                    // the subtree-sum mode; "t" toggles tree/flat.
                    "s" | "S" => {
                        if self.tab == Tab::Processes {
                            if let Some(pid) = self.selected_pid {
                                self.request_stop(pid, doc);
                            }
                        }
                    }
                    "g" | "G" => {
                        if self.tree_mode {
                            self.sum_mode = self.sum_mode.toggled();
                        }
                    }
                    "t" | "T" => {
                        self.tree_mode = !self.tree_mode;
                        self.scroll_off = 0.0;
                        self.clamp_scroll();
                    }
                    _ => return,
                }
                *handled = true;
            }
            _ => {}
        }
    }
}

impl Widget for MonitorApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        // Pull durable preferences out of the CRDT each sync.
        self.prefs = Prefs::load(state, path);
    }

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
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.maybe_refresh();
        let r = self.rect;
        if r.width() <= 4.0 || r.height() <= 4.0 {
            return;
        }
        let t = transform;
        self.hit = Hit::new();

        // App background.
        scene.fill(Fill::NonZero, t, theme.tokens.colors.surfaces.base, None, &r);

        // Advance the eased push flap once per frame (the redraw loop keeps
        // pumping while `needs_animation_frame` is true).
        self.proc_push.tick();

        let content = Rect::new(r.x0 + OUTER_PAD, r.y0 + OUTER_PAD, r.x1 - OUTER_PAD, r.y1 - OUTER_PAD);
        let header = Rect::new(content.x0, content.y0, content.x1, content.y0 + HEADER_H);
        let body_top = header.y1 + GAP;
        let body = Rect::new(content.x0, body_top, content.x1, content.y1);

        self.draw_header(scene, t, text_renderer, theme, header);

        // A press-and-hold on the interval pill opens the option popover while
        // still held (the pill's hit rect is now known from `draw_header`). The
        // redraw loop keeps pumping — `needs_animation_frame` is always true —
        // so this check fires mid-hold without waiting for another event.
        if let Some(started) = self.interval_press {
            if !self.interval_hold_opened
                && !self.interval_popover.is_open()
                && started.elapsed().as_millis() >= LONG_PRESS_MS
            {
                self.open_interval_popover();
                self.interval_hold_opened = true;
            }
        }
        // Headless `--shots` seed: open the popover deterministically once the
        // pill is positioned.
        if std::mem::take(&mut self.interval_popover_seed) {
            self.open_interval_popover();
        }

        // ---- whole-view vertical scroll -------------------------------
        // The Resources dashboard can be taller than its viewport — the
        // overview row + per-core grid + chart grid on a large machine or a
        // short/narrow window. Mirror `ui::widget::Scrollable`'s model: draw
        // the body content at `(… − offset)` inside a clip layer the size of
        // the viewport, clamp the offset to `content − viewport`, and paint a
        // scrollbar. The header stays pinned above the scroll. The Processes
        // tab manages its own internal row scroll, so it reports zero
        // page-content height and never page-scrolls.
        let viewport_h = body.height();
        let content_natural_h = self.content_natural_h(body.width());
        let page_overflow = (content_natural_h - viewport_h).max(0.0);
        self.content_h = content_natural_h.max(viewport_h);
        self.viewport_h = viewport_h;
        self.view_scroll_y = self.view_scroll_y.clamp(0.0, page_overflow as f32);
        let dy = self.view_scroll_y as f64;

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
            // Page scrollbar down the right edge of the body viewport — a
            // draggable thumb over a page-able track.
            let track = Rect::new(body.x1 - 8.0, body.y0, body.x1 - 1.0, body.y1);
            let c = &theme.tokens.colors;
            charts::fill_rrect(scene, t, track, 2.0, with_alpha(c.border_standard, 90));
            let frac_v = (viewport_h / self.content_h).clamp(0.0, 1.0);
            let thumb_h = (track.height() * frac_v).max(24.0);
            let pos = if page_overflow > 0.0 { dy / page_overflow } else { 0.0 };
            let ty0 = track.y0 + (track.height() - thumb_h) * pos;
            let thumb = Rect::new(track.x0, ty0, track.x1, ty0 + thumb_h);
            charts::fill_rrect(scene, t, thumb, 2.0, with_alpha(c.text_dim, 160));
            self.hit.page_scrollbar = Some(ScrollBarHit { track, thumb, max_value: page_overflow });
        }

        // A `--shots` request to open the context menu over a seeded pid: now
        // that the table rows are laid out (`hit.rows` is populated), anchor
        // the menu at that row and clear the seed. If the seeded pid isn't a
        // visible row this frame, fall back to the first laid-out row so the
        // shot still demonstrates the menu.
        if let Some(seed) = self.context_menu_seed.take() {
            let target = self
                .hit
                .rows
                .iter()
                .find(|(rp, _)| *rp == seed)
                .or_else(|| self.hit.rows.first())
                .map(|(rp, rect)| (*rp, *rect));
            if let Some((pid, rect)) = target {
                let anchor = Point::new(rect.x0 + 60.0, rect.y0 + ROW_H);
                self.last_cursor = Point::new(anchor.x + 20.0, anchor.y + CONTEXT_MENU_ROW_H * 3.5);
                self.open_context_menu(pid, anchor);
            }
        }

        // The right-click action menu floats above the content (but below the
        // confirm modal). The primitive renders itself at its stored anchor;
        // we draw it directly here, the same hand-drawn way as the rest of the
        // app, then record its item hit rects for `on_click` routing.
        if self.context.is_open() {
            self.context.draw(scene, t, text_renderer, theme, _presence);
            self.hit.menu_items = self.context_menu_item_rects();
            // Highlight the row under the cursor for affordance.
            let cur = self.last_cursor;
            let c = &theme.tokens.colors;
            for (_idx, rect) in &self.hit.menu_items {
                if rect.contains(cur) {
                    charts::fill_rrect(scene, t, *rect, 4.0, with_alpha(c.accent[5], 70));
                }
            }
            // Re-draw labels over the highlight so they stay legible.
            self.context.draw(scene, t, text_renderer, theme, _presence);
        } else {
            self.hit.menu_items.clear();
        }

        // The refresh-interval option popover floats above the content (below
        // the confirm modal). The reusable primitive positions + paints itself
        // against the pill anchor; the app routes clicks/keys into it.
        if self.interval_popover.is_open() {
            self.interval_popover.draw(scene, t, text_renderer, theme, _presence);
        }

        // The "PID copied" toast floats above the content, below the modal.
        self.draw_toast(scene, t, text_renderer, theme);

        if let Some((pid, kind)) = self.confirm_stop {
            self.ensure_confirm_dialog(pid, kind);
            self.layout_confirm_dialog();
            if let Some(cd) = self.confirm_dialog.as_mut() {
                cd.dialog.draw(scene, t, text_renderer, theme, _presence);
            }
        } else {
            self.confirm_dialog = None;
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // A pending stop confirmation is modal: while it's up the shared
        // dialog captures every event (typing, clicks, Esc) and nothing leaks
        // to the process table behind the scrim.
        if self.confirm_stop.is_some() {
            self.route_confirm_event(event, scale_factor, modifiers, doc, path, handled);
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                // Let the search field track hover (so a subsequent press can
                // focus it and place the caret) and follow a drag-select.
                let mut sink = None;
                let mut h = false;
                self.search.handle_event(event, scale_factor, modifiers, doc, path, &mut h, &mut sink, node_id, taffy);
                if let Some(q) = self.search.take_dirty() {
                    self.filter = q;
                    self.scroll_off = 0.0;
                }
                // A live scrollbar thumb drag follows the cursor.
                if self.scroll_drag.is_some() {
                    self.apply_scroll_drag(self.last_cursor);
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.confirm_stop.is_some() {
                    return;
                }
                // Pixels of scroll this notch represents (≈60px/line desktop
                // convention; touchpad pixel deltas pass through 1×).
                let (px_x, px_y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (-x * 60.0, -y * 60.0),
                    MouseScrollDelta::PixelDelta(pos) => (-(pos.x as f32), -(pos.y as f32)),
                };
                let shift = modifiers.state().shift_key();
                let over_table = self.tab == Tab::Processes
                    && self.proc_push.slide() < 0.5
                    && self.table_viewport.contains(self.last_cursor);
                if over_table {
                    // Shift-wheel (or a horizontal touchpad delta) scrolls the
                    // table sideways through its overflowing columns; a plain
                    // vertical wheel scrolls the rows.
                    if shift || px_x.abs() > px_y.abs() {
                        let dx = if shift && px_x == 0.0 { px_y } else { px_x };
                        // Clamp against the *columns* viewport (`table_cols_w`,
                        // the padded band), NOT the card's full inner width, so
                        // the final column + its right padding scroll into view.
                        let max_x = (self.table_content_w - self.table_cols_w).max(0.0) as f32;
                        self.table_scroll_x = (self.table_scroll_x + dx).clamp(0.0, max_x);
                    } else {
                        let rows = px_y / ROW_H as f32;
                        self.scroll_off = (self.scroll_off + rows).max(0.0);
                        self.clamp_scroll();
                    }
                    *handled = true;
                } else {
                    // Anywhere else (the Resources dashboard, the summary
                    // strip, the toolbar) scrolls the whole page.
                    let max_y = (self.content_h - self.viewport_h).max(0.0) as f32;
                    let before = self.view_scroll_y;
                    self.view_scroll_y = (self.view_scroll_y + px_y).clamp(0.0, max_y);
                    if (self.view_scroll_y - before).abs() > f32::EPSILON {
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_cursor;
                // A click inside the search pill goes to the shared field
                // (caret placement, clear button, focus).
                if self.hit.search.contains(p) {
                    let mut sink = None;
                    self.search.handle_event(event, scale_factor, modifiers, doc, path, handled, &mut sink, node_id, taffy);
                    // Immediate-mode hosts drive focus explicitly (the field
                    // only requests focus via the sunk focus_target).
                    self.search.set_focused(true);
                    if let Some(q) = self.search.take_dirty() {
                        self.filter = q;
                        self.scroll_off = 0.0;
                    }
                    *handled = true;
                } else {
                    self.on_click(p, doc, path, handled);
                }
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
                // Let the search field end any drag-select it started (a plain
                // click is a press+release, so the field must see the release).
                let mut sink = None;
                let mut h = false;
                self.search.handle_event(event, scale_factor, modifiers, doc, path, &mut h, &mut sink, node_id, taffy);
                // End any scrollbar thumb drag, otherwise resolve a pending
                // interval-pill press (quick release cycles; a held release
                // that already opened the popover leaves it up).
                if self.scroll_drag.take().is_some() {
                    *handled = true;
                } else if self.release_interval_press(doc, path) {
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: ke, .. }
                if ke.state == ElementState::Pressed =>
            {
                // While the search field holds focus, keystrokes edit it: text,
                // caret motion, selection, and Ctrl+C/V/X all go to the shared
                // SearchField; Enter/Escape blur it. Nothing reaches on_key.
                if self.search.is_focused() {
                    match &ke.logical_key {
                        Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter) => {
                            self.search.set_focused(false);
                            *handled = true;
                        }
                        _ => {
                            let mut sink = None;
                            self.search.handle_event(event, scale_factor, modifiers, doc, path, handled, &mut sink, node_id, taffy);
                            if let Some(q) = self.search.take_dirty() {
                                self.filter = q;
                                self.scroll_off = 0.0;
                            }
                        }
                    }
                    return;
                }
                let text = ke.text.as_ref().map(|s| s.as_str());
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

    fn name(&self) -> &str {
        "MonitorApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("monitor_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A live monitor keeps the redraw loop pumping so the throttled
    /// sysinfo refresh and the chart histories keep advancing.
    fn needs_animation_frame(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::text_render::TextRenderer;

    fn app() -> MonitorApp {
        MonitorApp::new()
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

    #[test]
    fn starts_on_processes_with_cpu_sort_and_a_selection() {
        let a = app();
        assert!(matches!(a.tab, Tab::Processes));
        assert!(matches!(a.sort_key, SortKey::Cpu));
        assert!(a.sort_desc);
        assert!(a.selected_pid.is_some());
    }

    #[test]
    fn cpu_sort_descending_orders_busiest_first() {
        let a = app();
        let v = a.visible_processes();
        assert!(v.len() >= 2);
        assert!(v[0].cpu >= v[1].cpu, "first row must be the busiest");
    }

    #[test]
    fn toggling_sort_key_flips_direction_on_repeat() {
        let mut a = app();
        a.set_sort(SortKey::Name); // text → ascending
        assert!(!a.sort_desc);
        a.set_sort(SortKey::Name); // repeat → flip
        assert!(a.sort_desc);
    }

    #[test]
    fn filter_narrows_the_list() {
        let mut a = app();
        let all = a.visible_processes().len();
        a.filter = "browser".into();
        let some = a.visible_processes().len();
        assert!(some >= 1 && some < all);
        assert!(a.visible_processes().iter().all(|p| {
            p.name.to_lowercase().contains("browser")
                || p.user.to_lowercase().contains("browser")
                || p.pid.to_string().contains("browser")
        }));
    }

    #[test]
    fn kernel_threads_hidden_by_default() {
        let mut a = app();
        assert!(a.visible_processes().iter().all(|p| !p.kernel));
        a.prefs.show_kernel_threads = true;
        assert!(a.visible_processes().iter().any(|p| p.kernel));
    }

    #[test]
    fn kill_removes_the_row_and_clears_selection() {
        let mut a = app();
        let pid = a.selected_pid.unwrap();
        let before = a.model.processes.len();
        let mut doc = AutoCommit::new();
        a.do_stop(pid, StopKind::Kill, &mut doc);
        assert_eq!(a.model.processes.len(), before - 1);
        assert!(a.selected_pid.is_none());
        assert!(a.model.processes.iter().all(|p| p.pid != pid));
    }

    /// Stop (SIGTERM) and Kill (SIGKILL) are distinct actions: Stop is never
    /// gated by a confirm, while Kill is gated by the confirm-kill pref.
    #[test]
    fn stop_is_ungated_kill_is_gated() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        a.prefs.confirm_kill = true;
        let pid = a.visible_rows()[0].pid;

        // Stop (SIGTERM) goes through immediately, no modal.
        a.selected_pid = Some(pid);
        a.request_stop(pid, &mut doc);
        assert!(a.confirm_stop.is_none(), "Stop must not pop a confirm");
        assert!(a.model.processes.iter().all(|p| p.pid != pid), "Stop dropped the row");

        // Kill (SIGKILL) pops the confirm dialog while the pref is on.
        let pid2 = a.visible_rows()[0].pid;
        a.request_kill(pid2, &mut doc, &root);
        assert_eq!(a.confirm_stop, Some((pid2, StopKind::Kill)), "Kill is gated");
    }

    #[test]
    fn confirm_kill_gated_by_pref() {
        let mut a = app();
        let pid = a.selected_pid.unwrap();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        a.prefs.confirm_kill = true;
        a.request_kill(pid, &mut doc, &root);
        assert_eq!(a.confirm_stop, Some((pid, StopKind::Kill)));
        // Direct kill when confirmation is off.
        a.confirm_stop = None;
        a.prefs.confirm_kill = false;
        a.request_kill(pid, &mut doc, &root);
        assert!(a.confirm_stop.is_none());
        assert!(a.model.processes.iter().all(|p| p.pid != pid));
    }

    #[test]
    fn arrow_navigation_moves_selection() {
        let mut a = app();
        a.tree_mode = false; // flat list so visible_rows order == sorted list
        a.rows_per_page = 10;
        let v = a.visible_rows();
        a.selected_pid = Some(v[0].pid);
        a.move_selection(1);
        assert_eq!(a.selected_pid, Some(v[1].pid));
        a.move_selection(-1);
        assert_eq!(a.selected_pid, Some(v[0].pid));
    }

    // ---- responsive layout fixes (issues 1-5) ---------------------------

    /// Issue 4: the per-core card the renderer sizes (`resources_natural_h`
    /// uses `per_core_card_h`) must hold a cell for *every* core in the
    /// model, for a range of core counts — not just the default 8. Assert on
    /// the production grid the draw path computes, so the last cores can
    /// never land below the card background.
    #[test]
    fn per_core_grid_fits_every_core_for_varied_counts() {
        for n in [1usize, 4, 8, 16, 32, 48] {
            let mut a = app();
            a.model.metrics.cores = (0..n).map(|i| (i as f32 * 3.0) % 100.0).collect();
            // The grid the renderer lays out (same width math as draw_resources).
            let w = 900.0;
            let grid = layout::per_core_grid(a.model.metrics.cores.len(), w - 2.0 * layout::RAIL_CARD_PAD);
            assert!(
                grid.cells() >= n,
                "grid must hold every core: n={n} cells={}",
                grid.cells()
            );
            // The card the renderer sizes holds the last grid row inside it.
            let card_h = layout::per_core_card_h(grid);
            let last_row_bottom = layout::CORE_TITLE_H + grid.rows as f64 * layout::CORE_ROW_STRIDE;
            assert!(
                last_row_bottom <= card_h + 0.001,
                "n={n}: last core row {last_row_bottom} spills past card {card_h}"
            );
        }
    }

    /// Issue 4 + 5: more cores ⇒ a taller Resources body. The natural height
    /// the page-scroll clamps against must grow with the core count, proving
    /// the grid drives the page height rather than being clipped.
    #[test]
    fn resources_height_grows_with_core_count() {
        let mut a8 = app();
        a8.model.metrics.cores = vec![10.0; 8];
        let mut a64 = app();
        a64.model.metrics.cores = vec![10.0; 64];
        assert!(
            a64.resources_natural_h(900.0) > a8.resources_natural_h(900.0),
            "64-core Resources body must be taller than 8-core"
        );
    }

    /// Issue 5: on a short viewport the Resources content exceeds the body,
    /// so `content_natural_h` reports more than the viewport height — the
    /// signal the page scrollbar + wheel routing key off.
    #[test]
    fn short_viewport_makes_resources_page_scrollable() {
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

    /// Issue 2: a narrow table viewport makes the columns overflow, so the
    /// horizontal scroll has a non-zero range and the THR column lives in
    /// scrollable space rather than overlapping the border.
    #[test]
    fn narrow_table_has_horizontal_scroll_range() {
        // The content area inside the card, minus padding, is what the
        // renderer hands to `process_columns`. A ~360px content width can't
        // seat even the five essential columns (USER is already dropped), so
        // the table still overflows and scrolls.
        let avail = 360.0 - 2.0 * layout::TABLE_PAD;
        let cols = layout::process_columns(avail);
        let total = layout::process_columns_total_w(&cols);
        assert!(
            total > avail,
            "narrow table columns ({total}) must overflow the viewport ({avail}) \
             so a horizontal scrollbar appears"
        );
    }

    /// Issue 5: page-scroll offset clamps to the overflow — it can't scroll
    /// past the content or above the top. Drives the same clamp the wheel
    /// handler uses.
    #[test]
    fn page_scroll_clamps_to_overflow() {
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

    /// Issue 6: Ctrl+K is left for the global-search framework — `on_key`
    /// must NOT consume it (so it bubbles up), while a bare "/" still focuses
    /// the local process filter.
    #[test]
    fn ctrl_k_is_left_unhandled_for_global_search() {
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
        assert!(handled2 && a.search.is_focused(), "bare / still focuses local search");
    }

    /// Switching tabs via the production click path resets the page scroll so
    /// the new tab opens at the top rather than inheriting a stale offset.
    #[test]
    fn clicking_a_tab_resets_page_scroll() {
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
        assert_eq!(a.view_scroll_y, 0.0, "tab switch must reset the page scroll");
    }

    // ---- new features: h-scroll, tree, sum, context menu ----------------

    /// Bug fix: the horizontal scroll must reach the FULL table width so the
    /// final column (plus its right padding) scrolls into view. The wheel
    /// handler clamps against `table_content_w − table_cols_w` (the padded
    /// columns viewport), so at max scroll the last column's right edge lands
    /// exactly at the right of the visible band — not `2·TABLE_PAD` short, the
    /// bug. This asserts on the exact clamp the production wheel path uses.
    #[test]
    fn h_scroll_reaches_the_last_column() {
        let mut a = app();
        // Simulate a narrow table the renderer would record: content wider
        // than the padded columns viewport. Derive the content width from the
        // real column solver at this band width (300px is below the essential
        // minimum, so the columns overflow) rather than hard-coding a total.
        a.table_cols_w = 300.0; // padded band (narrow window)
        a.table_content_w =
            layout::process_columns_total_w(&layout::process_columns(a.table_cols_w));
        assert!(
            a.table_content_w > a.table_cols_w,
            "columns must overflow at 300px so there is a scroll range"
        );
        let max_x = (a.table_content_w - a.table_cols_w).max(0.0) as f32;
        // The OLD (buggy) clamp used the un-padded viewport width, which is
        // 2·TABLE_PAD wider, leaving the last 24px unreachable. Prove the new
        // max exceeds that buggy bound by exactly 2·TABLE_PAD.
        let buggy_max = (a.table_content_w - (a.table_cols_w + 2.0 * layout::TABLE_PAD)).max(0.0) as f32;
        assert!(max_x > buggy_max, "fixed clamp {max_x} must exceed buggy {buggy_max}");
        assert!((max_x - buggy_max - (2.0 * layout::TABLE_PAD) as f32).abs() < 0.01);
        // Over-scroll clamps to exactly max_x — the last column reachable.
        a.table_scroll_x = (a.table_scroll_x + 9999.0).clamp(0.0, max_x);
        assert_eq!(a.table_scroll_x, max_x);
        // At max scroll, the last column's content-space right edge maps to
        // the right of the columns band (within rounding): full width shown.
        let cols = layout::process_columns(a.table_cols_w);
        let content_w = layout::process_columns_total_w(&cols);
        let last_right_in_view = content_w - a.table_scroll_x as f64; // relative to band left
        assert!(
            (last_right_in_view - a.table_cols_w).abs() < 1.0,
            "last column right edge {last_right_in_view} must align to band width {}",
            a.table_cols_w
        );
    }

    /// End-to-end h-scroll: lay the app out narrow (so the six columns
    /// overflow), draw a real frame, then drive the production wheel handler
    /// with a big shift-scroll. The table must scroll until the final column's
    /// right edge reaches the visible band's right edge — the whole table
    /// width, including the last column, becomes reachable.
    #[test]
    fn h_scroll_drive_reaches_last_column_end_to_end() {
        let mut a = app();
        // Narrow enough that even the five essential columns overflow the table
        // card (below the essential minimum, once USER is already dropped) — so
        // the h-scroll path is genuinely exercised. Wider windows now fit the
        // essentials (THR reads without scrolling), which is the whole point of
        // the column-priority fix; this test targets the still-overflowing tail.
        draw_at(&mut a, 430.0, 700.0);
        assert!(
            a.table_content_w > a.table_cols_w + 1.0,
            "columns must overflow at this width ({} vs {})",
            a.table_content_w,
            a.table_cols_w
        );
        // Park the cursor over the table so the wheel routes to h-scroll.
        a.last_cursor = a.table_viewport.center();
        let modifiers = winit::event::Modifiers::default();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy::NodeId::new(0);
        // A horizontal wheel (x delta dominant) drives the column scroll
        // through the real `handle_event` path. Drive it hard enough to pin
        // the clamp ceiling.
        for _ in 0..40 {
            let ev = WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(-50.0, 0.0),
                phase: winit::event::TouchPhase::Moved,
            };
            let mut handled = false;
            let mut focus = None;
            a.handle_event(&ev, 1.0, &modifiers, &mut doc, &root, &mut handled, &mut focus, node, &taffy);
        }
        // The real clamp ceiling from this frame's metrics:
        let max_x = (a.table_content_w - a.table_cols_w).max(0.0) as f32;
        assert_eq!(a.table_scroll_x, max_x, "wheel drove scroll to its ceiling");
        // At the ceiling, the last column's content-space right edge maps to
        // the right of the columns band (within 1px rounding).
        let cols = layout::process_columns(a.table_cols_w);
        let content_w = layout::process_columns_total_w(&cols);
        let last_right_in_band = content_w - a.table_scroll_x as f64;
        assert!(
            (last_right_in_band - a.table_cols_w).abs() < 1.0,
            "last column right edge {last_right_in_band} reaches band width {}",
            a.table_cols_w
        );
    }

    /// The tree defaults on, Sum subtree by default, and collapsed — so the
    /// first frame shows parents carrying their full subtree totals.
    #[test]
    fn tree_defaults_collapsed_summed() {
        let a = app();
        assert!(a.tree_mode, "tree on by default");
        assert_eq!(a.sum_mode, SumMode::SumSubtree, "Sum is the default");
        assert!(a.expanded.is_empty(), "collapsed by default");
        // A collapsed parent (shell) shows MORE than its own CPU because it
        // sums its subtree.
        let rows = a.visible_rows();
        let shell = rows.iter().find(|r| r.name == "shell").unwrap();
        let own = a.model.processes.iter().find(|p| p.name == "shell").unwrap().cpu;
        assert!(shell.cpu > own + 1.0, "collapsed shell {} sums subtree (own {own})", shell.cpu);
        assert!(shell.has_children && !shell.expanded);
    }

    /// Clicking a twisty (via the production `on_click` path) expands the node
    /// and its children appear in the next `visible_rows`.
    #[test]
    fn clicking_twisty_expands_and_children_appear() {
        let mut a = app();
        let shell_pid = a.model.processes.iter().find(|p| p.name == "shell").unwrap().pid;
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

    /// Toggling Sum→Per-process changes a collapsed parent's displayed total
    /// (its summed subtree usage drops to its own usage). Drives the exact
    /// production toggle path `on_click` runs for the sum pill.
    #[test]
    fn toggling_sum_changes_parent_total() {
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
        let shell_own = a.visible_rows().iter().find(|r| r.name == "shell").unwrap().cpu;
        assert!(
            shell_summed > shell_own + 1.0,
            "summed total {shell_summed} must exceed per-process {shell_own}"
        );
        let real_own = a.model.processes.iter().find(|p| p.name == "shell").unwrap().cpu;
        assert!((shell_own - real_own).abs() < 1e-3, "per-process == own usage");
    }

    /// A right-click over a process row opens the action context menu anchored
    /// at the cursor, targeting that row's pid.
    #[test]
    fn right_click_opens_context_menu_for_row() {
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
            .filter_map(|i| i.label().map(|s| s.to_string()))
            .collect();
        assert!(labels.iter().any(|l| l.starts_with("Stop")), "has Stop: {labels:?}");
        assert!(labels.iter().any(|l| l.starts_with("Kill")), "has Kill: {labels:?}");
        assert!(labels.iter().any(|l| l == "Copy PID"), "has Copy PID");
        // On a WIDE window the detail strip is always visible and the
        // right-click already selected the row, so "Show details" is a no-op
        // and is deliberately omitted (it would otherwise be a dead action).
        assert!(
            !labels.iter().any(|l| l == "Show details"),
            "Show details is omitted on wide (it would be a no-op): {labels:?}"
        );
    }

    /// Clicking the "Stop" item in the open menu dispatches a SIGTERM via the
    /// production action path (the menu callback records the action, on_click
    /// drains + applies it). Stop is ungated, so the row drops with no modal.
    #[test]
    fn context_menu_stop_dispatches_sigterm() {
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
            .position(|i| i.label().map(|l| l.starts_with("Stop")).unwrap_or(false))
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
        assert!(a.confirm_stop.is_none(), "Stop (SIGTERM) is ungated");
        assert!(a.model.processes.iter().all(|p| p.pid != pid), "process stopped");
    }

    /// Clicking "Kill" in the menu routes through the gated path: with the
    /// confirm pref on, it pops the SIGKILL confirm modal.
    #[test]
    fn context_menu_kill_gated_by_confirm() {
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
            .position(|i| i.label().map(|l| l.starts_with("Kill")).unwrap_or(false))
            .unwrap();
        let rect = a
            .context_menu_item_rects()
            .into_iter()
            .find(|(idx, _)| *idx == kill_idx)
            .map(|(_, r)| r)
            .unwrap();
        let mut h2 = false;
        a.on_click(rect.center(), &mut doc, &root, &mut h2);
        assert_eq!(a.confirm_stop, Some((pid, StopKind::Kill)), "Kill popped the confirm");
        // The process is NOT yet gone — the modal must be confirmed first.
        assert!(a.model.processes.iter().any(|p| p.pid == pid), "not killed until confirmed");
    }

    /// Copy PID records the pid (clipboard bridge) and is observable.
    #[test]
    fn context_menu_copy_pid_records_pid() {
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
        assert_eq!(a.copied_pid, Some(pid), "Copy PID recorded");
    }

    /// Search/filter still works over the tree: filtering to a deep child
    /// keeps its ancestors visible (so the match is reachable) and prunes
    /// unrelated branches.
    #[test]
    fn filter_works_over_the_tree() {
        let mut a = app();
        a.filter = "rustc".into();
        let rows = a.visible_rows();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"rustc"), "match present: {names:?}");
        // rustc's ancestor chain (cargo → code → shell) is force-shown.
        assert!(names.contains(&"cargo"), "ancestor cargo kept: {names:?}");
        assert!(names.contains(&"code"), "ancestor code kept: {names:?}");
        assert!(names.contains(&"shell"), "ancestor shell kept: {names:?}");
        // An unrelated branch (systemd's services) is pruned.
        assert!(!names.contains(&"chronyd"), "unrelated branch pruned: {names:?}");
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

    /// Defect 1: at no width — down to well below the narrow breakpoint — may
    /// the search box overlap any filter pill. Overlapping hit rects would make
    /// a pill unclickable (a click in the shared area is consumed as search),
    /// so this asserts the real hit rectangles the click path uses are
    /// pairwise disjoint across a width sweep, on the production draw path.
    #[test]
    fn toolbar_search_never_overlaps_pills() {
        for w in [1280.0, 1024.0, 900.0, 760.0, 700.0, 640.0, 560.0, 480.0, 420.0] {
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

    /// Defect 1: once the search box can no longer sit beside the pills, the
    /// toolbar stacks — the search box moves to its own row *above* the pills,
    /// and the toolbar grows to two rows. Proven on the real draw path.
    #[test]
    fn toolbar_stacks_on_very_narrow_widths() {
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

    /// The summary strip's cards grow with the window width but stay on a
    /// **single row** at every width — so narrowing the window can never steal
    /// vertical height from the process table below. Critically the width must
    /// keep re-deriving on resize of the SAME instance (not freeze at the first
    /// frame). Drives the real draw path, reading back the card hit rects it
    /// records, and asserts the table keeps its height across the narrowing.
    #[test]
    fn summary_strip_stays_one_row_and_table_keeps_height() {
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
        let filled = wide.iter().map(|r| r.width()).sum::<f64>() + 3.0 * GAP;
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
        assert_eq!(distinct_rows(&narrow), 1, "strip stays a single row when narrow");
        assert!(
            a.table_viewport.height() >= wide_table_h - 1.0,
            "the table must keep its height at narrow widths (narrow {} vs wide {})",
            a.table_viewport.height(),
            wide_table_h
        );

        // Widening straight back keeps one row — no frozen mode — and the card
        // width returns to its wide value.
        draw_at(&mut a, 1280.0, 720.0);
        assert_eq!(distinct_rows(&a.hit.summary_cells), 1, "one row at every width");
        assert!(
            (a.hit.summary_cells[0].width() - wide_card_w).abs() < 1.0,
            "card width returns to its wide value after resize round-trip"
        );
    }

    /// The live window must show REAL data, not the fabricated sample. The live
    /// constructor starts empty and its first refresh pulls actual `sysinfo`
    /// readings — proven by the running test process appearing in the table.
    #[test]
    fn live_constructor_samples_real_processes() {
        let mut a = MonitorApp::new_live();
        // Nothing fabricated before the first frame.
        assert!(a.model.processes.is_empty(), "live app starts empty, not seeded with sample rows");
        assert!(a.selected_pid.is_none(), "no pre-selected fake process");
        // The first refresh samples the real system.
        a.maybe_refresh();
        assert!(!a.model.processes.is_empty(), "first refresh populated real processes");
        assert!(
            a.model.processes.iter().any(|p| p.pid == std::process::id()),
            "the running process appears in the real table (data is live, not mock)"
        );
        assert!(!a.model.metrics.cores.is_empty(), "real per-core readings sampled");
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
        a.handle_event(ev, 1.0, &modifiers, &mut doc, &root, &mut handled, &mut focus, node, &taffy);
        handled
    }
    fn synth_cursor(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }
    /// A synthetic printable keypress carrying `text` (what a focused field
    /// actually inserts).
    fn synth_char(s: &str) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(s.into());
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(s.into());
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: ev,
            is_synthetic: false,
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
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
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

    /// A quick click (press + prompt release) on the interval pill cycles the
    /// cadence AND persists it to the CRDT — the full `on_click` press →
    /// `release_interval_press` → `cycle_interval` → `store` production path. A
    /// press alone must NOT cycle (that would double-fire on a long-press).
    #[test]
    fn interval_pill_click_cycles_and_persists() {
        let mut a = app();
        a.hit.interval = Rect::new(0.0, 0.0, 80.0, 30.0);
        a.prefs.interval_ms = 1000; // land on the cycle grid
        let before = a.prefs.interval_ms;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut h = false;
        // Press: starts the hold timer, does NOT cycle yet.
        a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
        assert!(h, "interval pill consumed the press");
        assert_eq!(a.prefs.interval_ms, before, "a bare press does not cycle");
        // Release before the long-press threshold cycles + persists.
        let cycled = a.release_interval_press(&mut doc, &root);
        assert!(cycled, "the pending press was resolved on release");
        assert_ne!(a.prefs.interval_ms, before, "a quick release cycled the cadence");
        assert_eq!(
            Prefs::load(&doc, &root).interval_ms,
            a.prefs.interval_ms,
            "new cadence persisted to the CRDT"
        );
        assert!(!a.interval_popover.is_open(), "a quick click never opens the popover");
    }

    /// Holding the interval pill past the long-press threshold opens the option
    /// popover (instead of cycling), seeded with every cadence and the current
    /// one pre-selected — the real `release_interval_press` long-press path.
    #[test]
    fn interval_long_press_opens_the_option_popover() {
        let mut a = app();
        a.rect = Rect::new(0.0, 0.0, 600.0, 400.0);
        a.hit.interval = Rect::new(500.0, 8.0, 560.0, 32.0);
        a.prefs.interval_ms = 2000;
        let before = a.prefs.interval_ms;
        // Simulate a press that started well past the long-press threshold.
        a.interval_press = Some(
            Instant::now()
                .checked_sub(Duration::from_millis(LONG_PRESS_MS as u64 + 200))
                .unwrap(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let handled = a.release_interval_press(&mut doc, &root);
        assert!(handled, "the held press was consumed");
        assert!(a.interval_popover.is_open(), "a long-press opens the popover");
        assert_eq!(a.prefs.interval_ms, before, "opening the popover does not cycle");
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

    /// Clicking an option in the open popover applies + persists that cadence
    /// and closes the menu — the production `on_click` popover-routing path.
    #[test]
    fn interval_popover_click_sets_and_persists_the_cadence() {
        let mut a = app();
        a.rect = Rect::new(0.0, 0.0, 600.0, 400.0);
        a.hit.interval = Rect::new(500.0, 8.0, 560.0, 32.0);
        a.prefs.interval_ms = 1000;
        a.open_interval_popover();
        assert!(a.interval_popover.is_open());
        // Resolve the body geometry, then click the "3s" row (index 3).
        let mut tr = TextRenderer::new();
        a.interval_popover.place(&mut tr, &Theme::dark());
        let row = a.interval_popover.rect();
        // Row 3 centre: y0 + PAD_Y(6) + 3.5 * ROW_H(30).
        let click = Point::new(row.center().x, row.y0 + 6.0 + 3.5 * 30.0);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut h = false;
        a.on_click(click, &mut doc, &root, &mut h);
        assert!(h, "the popover consumed the click");
        assert!(!a.interval_popover.is_open(), "picking an option closes the popover");
        assert_eq!(a.prefs.interval_ms, INTERVAL_OPTIONS[3], "the picked cadence applied");
        assert_eq!(
            Prefs::load(&doc, &root).interval_ms,
            INTERVAL_OPTIONS[3],
            "the picked cadence persisted to the CRDT"
        );
    }

    /// Keyboard nav in the open popover: ↓ moves the highlight, Enter applies
    /// the highlighted cadence + persists, and every key is consumed so nothing
    /// leaks to the table behind it.
    #[test]
    fn interval_popover_keyboard_selects_and_captures_input() {
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
        a.on_key(&Key::Character("c".into()), Some("c"), false, &mut doc, &root, &mut h);
        assert!(h, "keys are captured while the popover is open");
        assert_eq!(a.sort_key, sort_before, "the 'c' sort shortcut did not leak through");

        // ↓ ↓ moves the highlight to index 2, Enter applies it.
        let mut h = false;
        a.on_key(&Key::Named(NamedKey::ArrowDown), None, false, &mut doc, &root, &mut h);
        a.on_key(&Key::Named(NamedKey::ArrowDown), None, false, &mut doc, &root, &mut h);
        assert_eq!(a.interval_popover.highlighted(), 2);
        let mut h = false;
        a.on_key(&Key::Named(NamedKey::Enter), None, false, &mut doc, &root, &mut h);
        assert!(h);
        assert!(!a.interval_popover.is_open(), "Enter applies + closes");
        assert_eq!(a.prefs.interval_ms, INTERVAL_OPTIONS[2], "the highlighted cadence applied");
        assert_eq!(Prefs::load(&doc, &root).interval_ms, INTERVAL_OPTIONS[2], "persisted");
    }

    /// The narrow detail pane reserves a bottom action bar so the Kill button
    /// can never overlap the stat content — the exact collision the rework
    /// fixes. Asserted against the real drawn hit rects at a narrow, tall
    /// viewport with the detail pane pushed in: the buttons sit inside the
    /// reserved band pinned to the pane bottom, and don't overlap each other.
    #[test]
    fn detail_kill_button_sits_in_reserved_action_bar() {
        let mut a = app();
        a.proc_push.snap(ProcPane::Detail);
        draw_at(&mut a, 480.0, 820.0);
        let kill = a.hit.kill_btn;
        let stop = a.hit.stop_btn;
        assert!(kill.area() > 0.0, "the Kill button is drawn in the pushed detail pane");
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

    /// Clicking the kernel-threads pill flips the pref, persists it, and the
    /// table now lists kernel threads — the whole `on_click` path, not a direct
    /// field poke.
    #[test]
    fn kernel_toggle_click_flips_persists_and_reveals_threads() {
        let mut a = app();
        assert!(a.visible_processes().iter().all(|p| !p.kernel));
        a.hit.kernel_toggle = Rect::new(0.0, 0.0, 160.0, 30.0);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut h = false;
        a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
        assert!(h);
        assert!(a.prefs.show_kernel_threads, "toggled on");
        assert!(Prefs::load(&doc, &root).show_kernel_threads, "persisted to the CRDT");
        assert!(a.visible_processes().iter().any(|p| p.kernel), "kernel threads now visible");
    }

    /// Clicking the Tree/Flat pill toggles the view; flat rows are all depth 0.
    #[test]
    fn tree_flat_pill_click_toggles_mode() {
        let mut a = app();
        assert!(a.tree_mode);
        a.hit.tree_toggle = Rect::new(0.0, 0.0, 80.0, 30.0);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut h = false;
        a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
        assert!(h);
        assert!(!a.tree_mode, "toggled to flat");
        assert!(a.visible_rows().iter().all(|r| r.depth == 0), "flat rows carry no tree depth");
    }

    /// Clicking a sort-header sets that column as the sort key (numeric columns
    /// default descending) — the `on_click` → `set_sort` path.
    #[test]
    fn sort_header_click_sets_sort_key() {
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

    /// Clicking the detail-strip Kill button routes through the gated kill
    /// path (with confirm on it pops the modal) — via `on_click`, not a direct
    /// `request_kill`.
    #[test]
    fn kill_button_click_requests_gated_kill() {
        let mut a = app();
        a.prefs.confirm_kill = true;
        let pid = a.selected_pid.unwrap();
        a.hit.kill_btn = Rect::new(0.0, 0.0, 96.0, 32.0);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut h = false;
        a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
        assert!(h);
        assert_eq!(a.confirm_stop, Some((pid, StopKind::Kill)), "Kill button gated → confirm modal");
    }

    /// The shared confirm dialog's destructive primary dispatches the signal;
    /// its secondary (Cancel) dismisses without killing — both driven through
    /// the real `handle_event` → `ModalDialog` path (footer button rects
    /// resolved by a live layout pass, then a click at their centre).
    #[test]
    fn confirm_modal_buttons_confirm_and_cancel() {
        let mut a = app();
        let pid = a.visible_rows()[0].pid;
        a.selected_pid = Some(pid);
        a.confirm_stop = Some((pid, StopKind::Kill));
        // A real frame builds + lays out the shared dialog so its footer button
        // rects resolve exactly as the live renderer would.
        draw_at(&mut a, 900.0, 600.0);
        let primary = a.confirm_dialog.as_ref().unwrap().dialog.primary_rect();
        dispatch_ev(&mut a, &synth_cursor(primary.center().x, primary.center().y));
        let h = dispatch_ev(&mut a, &synth_press());
        assert!(h);
        assert!(a.confirm_stop.is_none(), "modal dismissed after confirm");
        assert!(a.model.processes.iter().all(|p| p.pid != pid), "process killed on confirm");

        let mut b = app();
        let pid2 = b.visible_rows()[0].pid;
        b.confirm_stop = Some((pid2, StopKind::Kill));
        draw_at(&mut b, 900.0, 600.0);
        let secondary = b.confirm_dialog.as_ref().unwrap().dialog.secondary_rect();
        dispatch_ev(&mut b, &synth_cursor(secondary.center().x, secondary.center().y));
        let h2 = dispatch_ev(&mut b, &synth_press());
        assert!(h2);
        assert!(b.confirm_stop.is_none(), "Cancel dismissed the modal");
        assert!(b.model.processes.iter().any(|p| p.pid == pid2), "Cancel did not kill");
    }

    /// The confirm dialog's keyboard: Enter confirms the kill, Escape cancels —
    /// both through the real `handle_event` → `ModalDialog` routing.
    #[test]
    fn confirm_modal_keyboard_enter_confirms_escape_cancels() {
        let mut a = app();
        let pid = a.visible_rows()[0].pid;
        a.confirm_stop = Some((pid, StopKind::Kill));
        let h = dispatch_ev(&mut a, &synth_key(NamedKey::Enter));
        assert!(h);
        assert!(a.confirm_stop.is_none());
        assert!(a.model.processes.iter().all(|p| p.pid != pid), "Enter confirmed the kill");

        let mut b = app();
        let pid2 = b.visible_rows()[0].pid;
        b.confirm_stop = Some((pid2, StopKind::Kill));
        let h2 = dispatch_ev(&mut b, &synth_key(NamedKey::Escape));
        assert!(h2);
        assert!(b.confirm_stop.is_none());
        assert!(b.model.processes.iter().any(|p| p.pid == pid2), "Escape cancelled the kill");
    }

    /// Clicking the search box focuses the shared SearchField, and the real
    /// keyboard path (chars, Backspace, Escape) drives the filter through it.
    #[test]
    fn search_click_focus_and_keyboard_editing_filters() {
        let mut a = app();
        // A real draw lays out the search pill + the embedded field's rect.
        draw_at(&mut a, 1280.0, 800.0);
        let sc = a.hit.search.center();
        // Click inside the pill → routed to the field, which focuses.
        dispatch_ev(&mut a, &synth_cursor(sc.x, sc.y));
        dispatch_ev(&mut a, &synth_press());
        assert!(a.search.is_focused(), "clicking the search box focuses the shared field");
        // Type through the production keyboard path — each key edits the field.
        for ch in "brow".chars() {
            dispatch_ev(&mut a, &synth_char(&ch.to_string()));
        }
        assert_eq!(a.search.value(), "brow", "typed text lands in the shared field");
        assert_eq!(a.filter, "brow", "the filter mirror tracks the field value");
        // Backspace erases through the field's real caret.
        dispatch_ev(&mut a, &synth_key(NamedKey::Backspace));
        assert_eq!(a.search.value(), "bro");
        assert_eq!(a.filter, "bro");
        assert!(
            a.visible_processes().iter().all(|p| p.name.to_lowercase().contains("bro")
                || p.user.to_lowercase().contains("bro")
                || p.pid.to_string().contains("bro")),
            "typed filter actually narrows the list"
        );
        // Escape blurs the field.
        dispatch_ev(&mut a, &synth_key(NamedKey::Escape));
        assert!(!a.search.is_focused(), "Escape leaves the search box");
    }

    /// On a narrow window, selecting a row through the real click path pushes
    /// the detail pane (the app-level wiring of `ProcPush`, not the unit).
    #[test]
    fn narrow_selecting_row_pushes_detail_pane() {
        let mut a = app();
        a.tree_mode = false;
        draw_at(&mut a, 520.0, 900.0);
        assert!(a.is_narrow());
        let (pid, rect) = a.hit.rows.first().cloned().expect("rows laid out");
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut h = false;
        a.on_click(rect.center(), &mut doc, &root, &mut h);
        assert!(h);
        assert_eq!(a.selected_pid, Some(pid));
        assert_eq!(a.proc_push.pane(), ProcPane::Detail, "selecting a row pushes the detail pane");
    }

    /// On a narrow window the Back chip click and Esc both pop the pushed
    /// detail back to the table — through the real `on_click`/`on_key` paths.
    #[test]
    fn narrow_back_chip_and_escape_return_to_table() {
        let mut a = app();
        a.rect = Rect::new(0.0, 0.0, 520.0, 900.0);
        assert!(a.is_narrow());
        a.proc_push.snap(ProcPane::Detail);
        a.hit.detail_back = Rect::new(0.0, 0.0, 120.0, 32.0);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut h = false;
        a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
        assert!(h);
        assert_eq!(a.proc_push.pane(), ProcPane::Table, "Back chip returns to the table");

        a.proc_push.snap(ProcPane::Detail);
        let mut he = false;
        a.on_key(&Key::Named(NamedKey::Escape), None, false, &mut doc, &root, &mut he);
        assert!(he);
        assert_eq!(a.proc_push.pane(), ProcPane::Table, "Esc pops the pushed detail");
    }

    /// A vertical wheel over the process table scrolls its rows, and a wheel on
    /// the Resources tab scrolls the whole page — both through `handle_event`.
    #[test]
    fn wheel_scrolls_rows_and_resources_page() {
        // Rows: many flat rows in a modest table so they overflow.
        let mut a = app();
        a.tree_mode = false;
        draw_at(&mut a, 1280.0, 520.0);
        assert!(a.hit.v_scrollbar.is_some(), "many rows overflow → vertical scrollbar");
        a.last_cursor = a.table_viewport.center();
        let before = a.scroll_off;
        dispatch_ev(&mut a, &synth_wheel(0.0, -3.0));
        assert!(a.scroll_off > before, "vertical wheel over the table scrolls the rows");

        // Page: Resources dashboard taller than a short viewport.
        let mut b = app();
        b.tab = Tab::Resources;
        b.model.metrics.cores = vec![20.0; 24];
        draw_at(&mut b, 900.0, 360.0);
        assert!(b.hit.page_scrollbar.is_some(), "Resources overflows → page scrollbar");
        b.last_cursor = Point::new(450.0, 200.0);
        let pbefore = b.view_scroll_y;
        dispatch_ev(&mut b, &synth_wheel(0.0, -3.0));
        assert!(b.view_scroll_y > pbefore, "wheel scrolls the Resources page");
    }

    /// On a narrow window the context menu offers "Show details", and clicking
    /// it pushes the detail pane for the target row.
    #[test]
    fn context_menu_show_details_pushes_detail_on_narrow() {
        let mut a = app();
        a.tree_mode = false;
        draw_at(&mut a, 520.0, 900.0);
        assert!(a.is_narrow());
        let (pid, rect) = a.hit.rows.first().cloned().expect("rows");
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
        assert_eq!(a.proc_push.pane(), ProcPane::Detail, "Show details pushes the detail pane");
    }

    /// Clicking a summary-strip cell switches to Resources AND scrolls that
    /// stat's own chart into view — each of the four cells lands on a different
    /// scroll offset (not the identical "top of Resources" stub).
    #[test]
    fn summary_cell_click_switches_and_scrolls_to_that_stat() {
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

    /// Clicking the vertical row scrollbar pages the rows and — critically —
    /// does NOT select the row beneath it (the old dead-scrollbar bug).
    #[test]
    fn clicking_row_scrollbar_pages_and_does_not_select() {
        let mut a = app();
        a.tree_mode = false;
        draw_at(&mut a, 1280.0, 520.0);
        let sb = a.hit.v_scrollbar.expect("vertical scrollbar present");
        let before_sel = a.selected_pid;
        let before_off = a.scroll_off;
        let click = Point::new(sb.track.center().x, (sb.thumb.y1 + 4.0).min(sb.track.y1 - 1.0));
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut h = false;
        a.on_click(click, &mut doc, &root, &mut h);
        assert!(h, "scrollbar consumed the click");
        assert!(a.scroll_off > before_off, "clicking below the thumb pages down");
        assert_eq!(a.selected_pid, before_sel, "the row beneath the scrollbar is NOT selected");
    }

    /// Dragging the vertical row scrollbar thumb scrolls the rows; release ends
    /// the drag — the full press → move → release path through `handle_event`.
    #[test]
    fn dragging_row_scrollbar_thumb_scrolls_rows() {
        let mut a = app();
        a.tree_mode = false;
        draw_at(&mut a, 1280.0, 520.0);
        let sb = a.hit.v_scrollbar.expect("vertical scrollbar");
        dispatch_ev(&mut a, &synth_cursor(sb.thumb.center().x, sb.thumb.center().y));
        dispatch_ev(&mut a, &synth_press());
        assert!(matches!(a.scroll_drag, Some(ScrollDrag::Rows { .. })), "press on thumb starts a row drag");
        let before = a.scroll_off;
        dispatch_ev(&mut a, &synth_cursor(sb.thumb.center().x, sb.track.y1 - 2.0));
        assert!(a.scroll_off > before, "dragging the thumb down scrolls rows");
        dispatch_ev(&mut a, &synth_release());
        assert!(a.scroll_drag.is_none(), "release ends the drag");
    }

    /// Dragging the horizontal column scrollbar thumb scrolls the columns.
    #[test]
    fn dragging_column_scrollbar_thumb_scrolls_columns() {
        let mut a = app();
        // Below the essential-column minimum (USER already dropped) so the five
        // essentials still overflow and the horizontal scrollbar appears. Wider
        // windows now fit the essentials — THR reads without scrolling — which
        // is the column-priority fix; this test targets the overflowing tail.
        draw_at(&mut a, 430.0, 700.0);
        let sb = a.hit.h_scrollbar.expect("horizontal scrollbar present when columns overflow");
        dispatch_ev(&mut a, &synth_cursor(sb.thumb.center().x, sb.thumb.center().y));
        dispatch_ev(&mut a, &synth_press());
        assert!(matches!(a.scroll_drag, Some(ScrollDrag::Cols { .. })));
        let before = a.table_scroll_x;
        dispatch_ev(&mut a, &synth_cursor(sb.track.x1 - 2.0, sb.thumb.center().y));
        assert!(a.table_scroll_x > before, "dragging the thumb right scrolls columns");
        dispatch_ev(&mut a, &synth_release());
        assert!(a.scroll_drag.is_none());
    }

    /// Dragging the whole-view page scrollbar thumb scrolls the Resources page.
    #[test]
    fn dragging_page_scrollbar_thumb_scrolls_view() {
        let mut a = app();
        a.tab = Tab::Resources;
        a.model.metrics.cores = vec![20.0; 24];
        draw_at(&mut a, 900.0, 360.0);
        let sb = a.hit.page_scrollbar.expect("page scrollbar");
        dispatch_ev(&mut a, &synth_cursor(sb.thumb.center().x, sb.thumb.center().y));
        dispatch_ev(&mut a, &synth_press());
        assert!(matches!(a.scroll_drag, Some(ScrollDrag::Page { .. })));
        let before = a.view_scroll_y;
        dispatch_ev(&mut a, &synth_cursor(sb.thumb.center().x, sb.track.y1 - 2.0));
        assert!(a.view_scroll_y > before, "dragging the page thumb down scrolls the view");
        dispatch_ev(&mut a, &synth_release());
        assert!(a.scroll_drag.is_none());
    }

    /// Copy PID records the pid (also pushed to the OS clipboard) and raises a
    /// transient toast that expires on its own rather than sticking forever.
    #[test]
    fn copy_pid_records_and_shows_expiring_toast() {
        let mut a = app();
        a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let pid = a.model.processes[3].pid;
        a.apply_action(ProcAction::CopyPid(pid), &mut AutoCommit::new(), &automerge::ROOT);
        assert_eq!(a.copied_pid, Some(pid), "copy recorded");
        assert!(a.copied_at.is_some());
        assert!(a.copy_toast_active(), "toast active immediately after copying");
        // The toast is time-limited: an old timestamp is no longer active.
        a.copied_at = Instant::now().checked_sub(Duration::from_millis(TOAST_MS + 100));
        assert!(!a.copy_toast_active(), "toast expires after its window (not a permanent caption)");
    }

    /// The Confirm-kills pill reaches the persisted preference: clicking it
    /// flips + stores `confirm_kill`, after which a kill is un-gated.
    #[test]
    fn confirm_kills_pill_toggles_persists_and_ungates_kill() {
        let mut a = app();
        assert!(a.prefs.confirm_kill, "confirm defaults on");
        a.hit.confirm_toggle = Rect::new(0.0, 0.0, 140.0, 30.0);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut h = false;
        a.on_click(Point::new(10.0, 10.0), &mut doc, &root, &mut h);
        assert!(h);
        assert!(!a.prefs.confirm_kill, "pill toggled the pref off");
        assert!(!Prefs::load(&doc, &root).confirm_kill, "persisted to the CRDT");
        // With confirm off, a kill goes straight through — proving the pill
        // actually reaches the gating logic.
        let pid = a.visible_rows()[0].pid;
        a.request_kill(pid, &mut doc, &root);
        assert!(a.confirm_stop.is_none(), "confirm off → kill is immediate");
        assert!(a.model.processes.iter().all(|p| p.pid != pid));
    }

    /// The confirm pill is actually laid out on the real draw path at a normal
    /// width (not just a hand-seated rect).
    #[test]
    fn confirm_pill_is_laid_out_on_wide_toolbar() {
        let mut a = app();
        draw_at(&mut a, 1280.0, 720.0);
        assert_ne!(a.hit.confirm_toggle, Rect::ZERO, "confirm pill drawn at a normal width");
        assert!(!overlaps(a.hit.confirm_toggle, a.hit.search), "confirm pill clears the search box");
        assert!(!overlaps(a.hit.confirm_toggle, a.hit.tree_toggle), "confirm pill clears the tree pill");
    }
}
