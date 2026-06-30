//! The Monitor root widget: a two-tab activity monitor (Processes /
//! Resources) with an always-visible system rail. View-level state (sort,
//! filter, selection, active tab, scroll) lives in widget fields here;
//! durable preferences live in the CRDT via [`crate::prefs`].
//!
//! The widget owns its own geometry: `build_layout` is a single
//! full-bleed leaf, and `draw` lays out every region by hand and renders
//! it into the Vello scene. Hit rectangles computed during `draw` are
//! cached in [`Hit`] and consulted by `handle_event` on the next input —
//! the same pattern the toolkit's `DataTable` uses for its column rects.

use std::collections::HashSet;
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
use ui::widget::{SplitLayout, Widget};

use crate::charts::{self, with_alpha, Series};
use crate::layout;
use crate::model::{fmt_bytes, fmt_rate, fmt_uptime, MonitorModel, ProcessInfo};
use crate::prefs::Prefs;
use crate::responsive::{ProcPane, ProcPush, RailFlap};
use crate::sampler::{Sampler, StopKind};
use crate::tree::{FlattenOpts, Forest, Row, SumMode};

/// A row action chosen from the right-click context menu. The menu's action
/// callbacks are `Fn()` with no access to `&mut self`, so they record the
/// chosen action into a shared cell that `on_click` drains and applies after
/// dispatch. This is the consumer-side routing the self-routing context menu
/// expects (`ContextMenu::handle_event` is a no-op primitive).
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
const RAIL_W: f64 = 248.0;
const GAP: f64 = 14.0;
const ROW_H: f64 = 22.0;
const TABLE_HEADER_H: f64 = 26.0;
const DETAIL_H: f64 = 96.0;
/// Per-depth indentation of a tree row's NAME column.
const TREE_INDENT: f64 = 16.0;
/// Width of the clickable expand/collapse triangle hit target.
const TWISTY_W: f64 = 16.0;

// `ui::widget::ContextMenu` lays itself out with private constants; we mirror
// them here so the app's hit-testing + on-screen-nudge match the primitive's
// drawn geometry exactly. Keep in sync with `context_menu.rs`.
const CONTEXT_MENU_W: f64 = 220.0;
const CONTEXT_MENU_ROW_H: f64 = 28.0;
const CONTEXT_MENU_SEP_H: f64 = 9.0;
const CONTEXT_MENU_PAD: f64 = 6.0;

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
    /// Context-menu item rects, recorded while the menu is open.
    menu_items: Vec<(usize, Rect)>,
    modal_kill: Rect,
    modal_cancel: Rect,
    /// Hamburger / close affordance summoning the rail when it isn't docked.
    rail_toggle: Rect,
    /// The dimming scrim behind an open overlay rail (tap to dismiss).
    rail_scrim: Rect,
    /// Back affordance returning from the pushed detail to the table (narrow).
    detail_back: Rect,
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
            menu_items: Vec::new(),
            modal_kill: Rect::ZERO,
            modal_cancel: Rect::ZERO,
            rail_toggle: Rect::ZERO,
            rail_scrim: Rect::ZERO,
            detail_back: Rect::ZERO,
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
    filter: String,
    search_focused: bool,
    selected_pid: Option<u32>,
    scroll_off: f32,
    /// Horizontal scroll of the process table, in logical px, when its
    /// columns are wider than the viewport (clamped to the overflow).
    table_scroll_x: f32,
    /// Vertical scroll of the *whole* view, in logical px, when the laid-out
    /// content (rail card column on the Resources tab, the chart grid, etc.)
    /// is taller than the body viewport. Clamped each frame to the overflow.
    view_scroll_y: f32,
    /// Last frame's content/viewport heights, recorded during `draw` so
    /// wheel + clamp can run in `handle_event` without re-deriving layout.
    content_h: f64,
    viewport_h: f64,
    /// A pending confirmation: the process and which stop signal it will get.
    /// Set only for the `Kill` (SIGKILL) path; `Stop` (SIGTERM) never gates.
    confirm_stop: Option<(u32, StopKind)>,

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
    /// The pid most recently "Copy PID"'d — observable by tests and surfaced
    /// briefly as a toast-style caption (the OS clipboard protocol isn't wired
    /// to this app yet, so the copy is recorded here).
    copied_pid: Option<u32>,
    /// A `--shots` request to open the context menu over a pid once its row
    /// has been laid out (the anchor isn't known until `draw` computes the row
    /// rects). Consumed on the first frame that finds the row.
    context_menu_seed: Option<u32>,

    // responsive layout
    /// The system rail: docked beside content when wide, an overlay flap
    /// (closed by default, hamburger-summoned) when narrow.
    rail: RailFlap,
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
    pub fn new() -> Self {
        let model = MonitorModel::sample();
        // Pre-select the busiest process so the detail strip is populated
        // in the headless screenshot.
        let selected_pid = model
            .processes
            .iter()
            .max_by(|a, b| a.cpu.total_cmp(&b.cpu))
            .map(|p| p.pid);
        Self {
            model,
            sampler: Sampler::new(),
            prefs: Prefs::default(),
            tab: Tab::Processes,
            sort_key: SortKey::Cpu,
            sort_desc: true,
            filter: String::new(),
            search_focused: false,
            selected_pid,
            scroll_off: 0.0,
            table_scroll_x: 0.0,
            view_scroll_y: 0.0,
            content_h: 0.0,
            viewport_h: 0.0,
            confirm_stop: None,
            tree_mode: true,
            sum_mode: SumMode::SumSubtree,
            expanded: HashSet::new(),
            context: ContextMenu::new(),
            context_pid: None,
            pending_action: Arc::new(Mutex::new(None)),
            copied_pid: None,
            context_menu_seed: None,
            rail: RailFlap::default(),
            proc_push: ProcPush::default(),
            rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            // Set to "now" so the first (screenshot) frame skips the live
            // refresh and renders the seeded sample untouched.
            last_refresh: Instant::now(),
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
            self.clamp_scroll();
        }
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
        menu.push(MenuItem::action("Show details", set(ProcAction::ShowDetails(pid))));
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

    /// Apply a [`ProcAction`] the context menu recorded. Returns nothing; the
    /// CopyPid path writes to the clipboard-bridge field (a no-op stub until
    /// the OS clipboard protocol lands — recorded so it's observable in tests).
    fn apply_action(&mut self, action: ProcAction, doc: &mut AutoCommit, path: &ObjId) {
        match action {
            ProcAction::Stop(pid) => self.request_stop(pid, doc),
            ProcAction::Kill(pid) => self.request_kill(pid, doc, path),
            ProcAction::CopyPid(pid) => {
                self.copied_pid = Some(pid);
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
    /// Resolve the rail's split layout from the current allocated width.
    fn rail_layout(&self) -> SplitLayout {
        RailFlap::layout(self.rect.width())
    }

    /// `true` when the window is narrow enough for the overlay rail + the
    /// single-pane Processes push flow.
    fn is_narrow(&self) -> bool {
        crate::responsive::is_narrow(self.rect.width())
    }

    /// Whether the hamburger / close toggle is currently shown over the
    /// content — whenever the rail isn't fully docked-open, so a collapsed
    /// rail can be summoned and an open overlay rail dismissed.
    fn show_rail_toggle(&self) -> bool {
        self.rail_layout() == SplitLayout::Overlay || self.rail.reveal() < 0.999
    }

    // ---- content-height helpers (drive the whole-view vertical scroll) ---
    /// Natural height of the system rail's stacked cards (ring, memory,
    /// sparklines, info). The info card is laid out to a fixed footprint so
    /// the rail has a definite intrinsic height the page scroll can use.
    fn rail_natural_h(&self) -> f64 {
        let info_h = 112.0; // host/kernel/cores/uptime + cpu brand line
        layout::RAIL_RING_H
            + GAP
            + layout::RAIL_MEM_H
            + GAP
            + layout::rail_spark_panel_h()
            + GAP
            + info_h
    }

    /// Natural height of the Resources tab body for `n` cores at content
    /// width `w`: the per-core card (sized to its row count) + the 2×2 chart
    /// grid with a legible minimum chart height.
    fn resources_natural_h(&self, w: f64) -> f64 {
        let n = self.model.metrics.cores.len().max(1);
        let grid = layout::per_core_grid(n, w - 2.0 * layout::RAIL_CARD_PAD);
        let core_h = layout::per_core_card_h(grid);
        let min_chart_h = 150.0;
        let charts_h = 2.0 * min_chart_h + GAP;
        core_h + GAP + charts_h
    }

    /// The wider of the rail column and the active tab's body — the page's
    /// intrinsic content height, against which `view_scroll_y` is clamped.
    /// `main_w` is the content column width (right of any docked rail).
    fn content_natural_h(&self, main_w: f64) -> f64 {
        let body = match self.tab {
            // Processes is a self-contained, internally-scrolling table; its
            // body fills the viewport and never drives the page scroll.
            Tab::Processes => 0.0,
            Tab::Resources => self.resources_natural_h(main_w),
        };
        // The docked rail can be taller than a short Resources body, so the
        // page must scroll to reach the rail's info card too.
        let rail = if self.rail_layout() == SplitLayout::Docked && self.rail.reveal() > 0.5 {
            self.rail_natural_h()
        } else {
            0.0
        };
        body.max(rail)
    }

    // ---- headless screenshot scenarios -----------------------------------
    /// Seed a deterministic interaction state for a headless `--shots` frame.
    /// `--shots` renders a single frame with no event loop, so a scenario that
    /// would normally require live input (an open overlay rail, a pushed
    /// detail pane) is snapped here instead. Unknown names render the default
    /// frame — at a narrow viewport that is the closed-rail process table.
    ///
    /// Recognised scenarios:
    /// * `"rail"`   — force the overlay rail open so a narrow shot shows the
    ///   slid-in rail above its dimming scrim.
    /// * `"detail"` — snap the narrow Processes push flow to the pushed detail
    ///   pane (otherwise a narrow frame opens to the table, as a fresh window
    ///   would).
    /// * `"resources"`      — switch to the Resources tab so a shot shows the
    ///   per-core grid + 2×2 charts (the fitted per-core card).
    /// * `"resources-many"` — Resources tab seeded with a high core count so a
    ///   short/narrow shot demonstrates the per-core card growing to fit every
    ///   core and the whole view becoming vertically scrollable.
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        match scenario {
            "rail" => self.rail.snap(true),
            "detail" => self.proc_push.snap(ProcPane::Detail),
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

        // The header lays out three groups left→right: brand, centred tabs,
        // and the clock/interval block — and they must never overlap, however
        // narrow the window. We measure each group, then shrink the least
        // essential parts (the subtitle, the uptime caption, finally the tab
        // labels → icon-only pills) until they fit. The brand glyph, the tab
        // control and the clock+interval are always present.
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

        // ---- brand: glyph + title (+ subtitle when there's room) ----------
        let glyph = Rect::new(inner.x0, inner.y0 + 2.0, inner.x0 + 26.0, inner.y0 + 28.0);
        icons::draw_icon(scene, t, glyph, Icon::Monitor, c.accent[5], 1.8);
        let (titw, _) = tr.measure_weighted("Monitor", theme.tokens.typography.scale.xl, None, TextWeight::Bold);
        tr.draw_text_weighted(
            scene,
            "Monitor",
            theme.tokens.typography.scale.xl,
            c.text_primary,
            t * Affine::translate((glyph.x1 + 10.0, inner.y0 - 1.0)),
            None,
            TextWeight::Bold,
        );
        if !header_narrow {
            tr.draw_text(
                scene,
                "System Activity",
                xs,
                c.text_dim,
                t * Affine::translate((glyph.x1 + 11.0, inner.y0 + 20.0)),
                None,
            );
        }
        let brand_right = glyph.x1 + 10.0 + titw as f64;

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

    fn draw_rail(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect) {
        let c = &theme.tokens.colors;
        let m = &self.model.metrics;

        // Section 1: overall CPU ring + load average.
        let ring_h = 168.0;
        let ring_panel = Rect::new(r.x0, r.y0, r.x1, r.y0 + ring_h);
        let inner = charts::panel(scene, t, ring_panel, theme, 12.0);
        tr.draw_text_weighted(scene, "Processor", theme.tokens.typography.scale.sm, c.text_primary, t * Affine::translate((inner.x0, inner.y0)), None, TextWeight::Semibold);
        let overall = m.cpu_overall();
        let center = Point::new(inner.center().x, inner.y0 + 84.0);
        charts::ring_gauge(
            scene, t, tr, center, 44.0, 9.0, overall / 100.0,
            charts::load_color(theme, overall / 100.0), theme,
            &format!("{:.0}%", overall), "CPU LOAD",
        );
        let la = format!("load  {:.2}  {:.2}  {:.2}", m.load_avg[0], m.load_avg[1], m.load_avg[2]);
        let (lw, _) = tr.measure(&la, theme.tokens.typography.scale.xs, None);
        tr.draw_text(scene, &la, theme.tokens.typography.scale.xs, c.text_dim, t * Affine::translate((inner.center().x - lw as f64 / 2.0, inner.y1 - 12.0)), None);

        // Section 2: memory + swap.
        let mem_y = ring_panel.y1 + GAP;
        let mem_panel = Rect::new(r.x0, mem_y, r.x1, mem_y + 96.0);
        let mi = charts::panel(scene, t, mem_panel, theme, 12.0);
        tr.draw_text_weighted(scene, "Memory", theme.tokens.typography.scale.sm, c.text_primary, t * Affine::translate((mi.x0, mi.y0)), None, TextWeight::Semibold);
        let ram_label = format!("{} / {}", fmt_bytes(m.mem_used), fmt_bytes(m.mem_total));
        charts::bar_gauge(
            scene, t, tr,
            Rect::new(mi.x0, mi.y0 + 22.0, mi.x1, mi.y0 + 44.0),
            theme, m.mem_frac(), charts::load_color(theme, m.mem_frac()), "RAM", &ram_label,
        );
        let swap_label = format!("{} / {}", fmt_bytes(m.swap_used), fmt_bytes(m.swap_total));
        charts::bar_gauge(
            scene, t, tr,
            Rect::new(mi.x0, mi.y0 + 52.0, mi.x1, mi.y0 + 74.0),
            theme, m.swap_frac(), c.accent[5], "Swap", &swap_label,
        );

        // Section 3: net + disk sparklines. The card height is derived from
        // the three stacked sparkline rows (label + 32px chart each) plus the
        // card padding, so the last (Disk) row sits *inside* the card instead
        // of spilling below it. See `layout::rail_spark_panel_h`.
        let sp_y = mem_panel.y1 + GAP;
        let sp_panel = Rect::new(r.x0, sp_y, r.x1, sp_y + layout::rail_spark_panel_h());
        let si = charts::panel(scene, t, sp_panel, theme, layout::RAIL_CARD_PAD);
        let spark = |scene: &mut Scene, tr: &mut TextRenderer, y: f64, label: &str, val: &str, data: &[f32], color: Color| {
            tr.draw_text_weighted(scene, label, theme.tokens.typography.scale.xs, c.text_primary, t * Affine::translate((si.x0, y)), None, TextWeight::Medium);
            let (vw, _) = tr.measure(val, theme.tokens.typography.scale.xs, None);
            tr.draw_text(scene, val, theme.tokens.typography.scale.xs, c.text_dim, t * Affine::translate((si.x1 - vw as f64, y)), None);
            let sr = Rect::new(si.x0, y + 14.0, si.x1, y + layout::SPARK_ROW_EXTENT);
            let mx = data.iter().copied().fold(0.0_f32, f32::max).max(1.0);
            charts::sparkline(scene, t, sr, data, mx, color);
        };
        spark(scene, tr, si.y0, "Net ↓", &fmt_rate(m.net_rx), m.net_rx_hist.as_slice(), c.semantic.info);
        spark(scene, tr, si.y0 + layout::SPARK_ROW_STRIDE, "Net ↑", &fmt_rate(m.net_tx), m.net_tx_hist.as_slice(), c.accent[6]);
        spark(scene, tr, si.y0 + 2.0 * layout::SPARK_ROW_STRIDE, "Disk", &format!("{} / {}", fmt_rate(m.disk_read), fmt_rate(m.disk_write)), m.disk_read_hist.as_slice(), c.semantic.warning);

        // Section 4: system info text. Drawn to a fixed footprint (not the
        // remaining `r.y1` slack) so the rail has a definite intrinsic height
        // the whole-view scroll can reveal in full. When the rail is taller
        // than the viewport the page scroll brings this card into view.
        let info_y = sp_panel.y1 + GAP;
        let info_h = 112.0;
        let info_bottom = (info_y + info_h).min(r.y1.max(info_y + info_h));
        {
            let info_panel = Rect::new(r.x0, info_y, r.x1, info_bottom);
            let ii = charts::panel(scene, t, info_panel, theme, layout::RAIL_CARD_PAD);
            let mut y = ii.y0;
            let fs = theme.tokens.typography.scale.xs;
            let line = |scene: &mut Scene, tr: &mut TextRenderer, y: &mut f64, k: &str, v: String| {
                tr.draw_text(scene, k, fs, c.text_dim, t * Affine::translate((ii.x0, *y)), None);
                let vv = Self::truncate(tr, &v, fs, ii.width() - 52.0);
                let (vw, _) = tr.measure(&vv, fs, None);
                tr.draw_text(scene, &vv, fs, c.text_primary, t * Affine::translate((ii.x1 - vw as f64, *y)), None);
                *y += 18.0;
            };
            line(scene, tr, &mut y, "host", m.host.clone());
            line(scene, tr, &mut y, "kernel", m.kernel.clone());
            line(scene, tr, &mut y, "cores", format!("{}", m.cores.len()));
            line(scene, tr, &mut y, "uptime", fmt_uptime(m.uptime_secs));
            if y + 14.0 < ii.y1 {
                let cpu = Self::truncate(tr, &m.cpu_brand, fs, ii.width());
                tr.draw_text(scene, &cpu, fs, c.text_dim, t * Affine::translate((ii.x0, y)), None);
            }
        }
    }

    /// Position and paint the system rail for the current split layout. Docked
    /// mode draws the rail in place (the content is already inset for it).
    /// Overlay mode dims the body behind a scrim that fades with the reveal
    /// and floats the rail in from the left edge over the content, exactly
    /// like an Adwaita flap. Records the scrim hit rect for tap-to-dismiss.
    fn draw_rail_region(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rail_docked: Rect,
        layout: SplitLayout,
    ) {
        let c = &theme.tokens.colors;
        let reveal = self.rail.reveal();

        match layout {
            SplitLayout::Docked => {
                // Fully collapsed (mid hide-animation, content not yet
                // reclaimed) — nothing to paint.
                if reveal <= 0.001 {
                    return;
                }
                self.draw_rail(scene, t, tr, theme, rail_docked);
            }
            SplitLayout::Overlay => {
                if reveal <= 0.001 {
                    return; // fully collapsed overlay — nothing to paint
                }
                // Dim the whole app behind the flap; tap dismisses it.
                let scrim = with_alpha(c.surfaces.overlay, (reveal * 178.0) as u8);
                scene.fill(Fill::NonZero, t, scrim, None, &self.rect);
                self.hit.rail_scrim = self.rect;

                // Float the rail in from off-screen-left as the slide eases.
                let slide = self.rail.slide();
                let rail_w = rail_docked.width();
                let x0 = rail_docked.x0 - (rail_w + OUTER_PAD) * (1.0 - slide);
                let panel = Rect::new(x0, rail_docked.y0, x0 + rail_w, rail_docked.y1);
                // Opaque backing so the rail floats clearly over the content.
                charts::fill_rrect(scene, t, panel, theme.tokens.radii.md as f64 + 2.0, c.surfaces.raised);
                charts::stroke_rrect(scene, t, panel, theme.tokens.radii.md as f64 + 2.0, c.border_standard, 1.0);
                self.draw_rail(scene, t, tr, theme, panel);
            }
        }
    }

    /// The hamburger / close toggle in the content's top-left corner. Shown
    /// whenever the rail isn't fully docked-open, so a collapsed rail can be
    /// summoned and an open overlay rail dismissed. Records its hit rect.
    fn draw_rail_toggle(&mut self, scene: &mut Scene, t: Affine, theme: &Theme, content: Rect) {
        let c = &theme.tokens.colors;
        let size = 34.0;
        let bx = content.x0;
        let by = content.y0;
        let btn = Rect::new(bx, by, bx + size, by + size);
        charts::fill_rrect(scene, t, btn, theme.tokens.radii.md as f64, c.surfaces.raised);
        charts::stroke_rrect(scene, t, btn, theme.tokens.radii.md as f64, c.border_standard, 1.0);

        let cx = btn.x0 + size / 2.0;
        let open = self.rail.reveal() > 0.5;
        if open {
            // An "X" / close affordance when the rail is showing.
            let rr = 6.0;
            let cyc = btn.y0 + size / 2.0;
            scene.stroke(&Stroke::new(1.8), t, c.text_primary, None, &Line::new((cx - rr, cyc - rr), (cx + rr, cyc + rr)));
            scene.stroke(&Stroke::new(1.8), t, c.text_primary, None, &Line::new((cx + rr, cyc - rr), (cx - rr, cyc + rr)));
        } else {
            // Three-line hamburger.
            let line_w = 14.0;
            for i in 0..3 {
                let ly = btn.y0 + 11.0 + i as f64 * 6.0;
                scene.stroke(&Stroke::new(1.8), t, c.text_primary, None, &Line::new((cx - line_w / 2.0, ly), (cx + line_w / 2.0, ly)));
            }
        }
        self.hit.rail_toggle = btn;
    }

    /// The Processes tab. Wide: a toolbar, the process table, and the selected
    /// process's detail strip stacked top to bottom. Narrow: single-pane push
    /// — the toolbar+table OR the detail, sliding horizontally as one swaps for
    /// the other (driven by [`ProcPush`]). Selecting a row pushes the detail in
    /// with a Back affordance; Back (or Esc) slides the table back.
    fn draw_processes(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect) {
        if !self.is_narrow() {
            // Wide: toolbar, table, detail strip stacked.
            let toolbar = Rect::new(r.x0, r.y0, r.x1, r.y0 + 36.0);
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

        // Table slab: toolbar + full-height table (no detail reservation —
        // the detail is its own pushed pane on narrow).
        if slide < 0.999 {
            let toolbar = Rect::new(table_slab.x0, table_slab.y0, table_slab.x1, table_slab.y0 + 36.0);
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
        for col in columns {
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
            cell(tr, scene, &p.pid.to_string(), &columns[0], txt, TextWeight::Regular);

            // NAME column: tree indent + twisty + name, all in the NAME band.
            let name_col = &columns[1];
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

            cell(tr, scene, &p.user, &columns[2], txt, TextWeight::Regular);
            // CPU% coloured by load. A collapsed parent's summed total can
            // exceed 100% (multi-core); colour against a single core's scale
            // but clamp the hue so a hot subtree reads as fully loaded.
            let cpu_frac = (p.cpu / 100.0).clamp(0.0, 1.0);
            let cpu_col = if p.cpu >= 1.0 { charts::load_color(theme, cpu_frac) } else { txt };
            cell(tr, scene, &format!("{:.1}", p.cpu), &columns[3], cpu_col, TextWeight::Medium);
            cell(tr, scene, &fmt_bytes(p.mem_bytes), &columns[4], txt, TextWeight::Regular);
            cell(tr, scene, &p.threads.to_string(), &columns[5], txt, TextWeight::Regular);

            self.hit.rows.push((p.pid, row));
        }
        scene.pop_layer();

        // Vertical row scrollbar.
        if rows.len() > self.rows_per_page {
            let track = Rect::new(view.x1 - 5.0, view.y0, view.x1 - 1.0, view.y1);
            charts::fill_rrect(scene, t, track, 2.0, with_alpha(c.border_standard, 90));
            let frac_v = self.rows_per_page as f64 / rows.len() as f64;
            let thumb_h = (track.height() * frac_v).max(24.0);
            let max_off = (rows.len() - self.rows_per_page) as f64;
            let pos = if max_off > 0.0 { self.scroll_off as f64 / max_off } else { 0.0 };
            let ty0 = track.y0 + (track.height() - thumb_h) * pos;
            charts::fill_rrect(scene, t, Rect::new(track.x0, ty0, track.x1, ty0 + thumb_h), 2.0, with_alpha(c.text_dim, 160));
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
            charts::fill_rrect(scene, t, Rect::new(tx0, track.y0, tx0 + thumb_w, track.y1), 2.0, with_alpha(c.text_dim, 160));
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

        // Back chip: chevron + "Processes". Indented past the rail's hamburger
        // toggle (which also lives in the top-left corner) so the two don't
        // overlap.
        let label = "Processes";
        let fs = theme.tokens.typography.scale.sm;
        let (lw, lh) = tr.measure_weighted(label, fs, None, TextWeight::Semibold);
        let back_x0 = r.x0 + 34.0 + GAP;
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

    fn draw_toolbar(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect) {
        let c = &theme.tokens.colors;
        // Search box (left ~ 320px).
        let search_w = 320.0_f64.min(r.width() - 260.0).max(160.0);
        let search_r = Rect::new(r.x0, r.y0, r.x0 + search_w, r.y1);
        let radius = r.height() / 2.0;
        charts::fill_rrect(scene, t, search_r, radius, c.surfaces.base);
        let border = if self.search_focused { c.border_focus } else { c.border_standard };
        charts::stroke_rrect(scene, t, search_r, radius, border, if self.search_focused { 1.6 } else { 1.0 });
        let icon_r = Rect::new(search_r.x0 + 10.0, search_r.center().y - 8.0, search_r.x0 + 26.0, search_r.center().y + 8.0);
        icons::draw_icon(scene, t, icon_r, Icon::Search, c.text_dim, 1.6);
        let fs = theme.tokens.typography.scale.sm;
        let tx = icon_r.x1 + 8.0;
        let ty = search_r.center().y - fs as f64 / 2.0 - 1.0;
        if self.filter.is_empty() && !self.search_focused {
            tr.draw_text(scene, "Search processes…", fs, c.text_dim, t * Affine::translate((tx, ty)), None);
        } else {
            let shown = Self::truncate(tr, &self.filter, fs, search_r.x1 - tx - 12.0);
            tr.draw_text(scene, &shown, fs, c.text_primary, t * Affine::translate((tx, ty)), None);
            if self.search_focused {
                let (cw, _) = tr.measure(&shown, fs, None);
                let caret_x = tx + cw as f64 + 1.0;
                scene.stroke(&Stroke::new(1.5), t, c.accent[5], None, &Line::new((caret_x, search_r.y0 + 8.0), (caret_x, search_r.y1 - 8.0)));
            }
        }
        self.hit.search = search_r;

        // Kernel-threads toggle pill (right).
        let kt_label = "kernel threads";
        let ktw = tr.measure(kt_label, fs, None).0 as f64 + 44.0;
        let kt_r = Rect::new(r.x1 - ktw, r.y0 + 2.0, r.x1, r.y1 - 2.0);
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
        let mut next_x1 = kt_r.x0 - 10.0;
        if self.tree_mode {
            let sum_label = self.sum_mode.label();
            let sum_text = format!("Σ {sum_label}");
            let sw = tr.measure(&sum_text, fs, None).0 as f64 + 24.0;
            let sum_r = Rect::new(next_x1 - sw, r.y0 + 2.0, next_x1, r.y1 - 2.0);
            let summing = self.sum_mode == SumMode::SumSubtree;
            charts::fill_rrect(scene, t, sum_r, sum_r.height() / 2.0, if summing { with_alpha(c.accent[5], 60) } else { c.surface_white_5 });
            charts::stroke_rrect(scene, t, sum_r, sum_r.height() / 2.0, if summing { c.accent[5] } else { c.border_standard }, 1.0);
            let (stw, _) = tr.measure(&sum_text, fs, None);
            tr.draw_text(scene, &sum_text, fs, if summing { c.text_primary } else { c.text_dim }, t * Affine::translate((sum_r.center().x - stw as f64 / 2.0, sum_r.center().y - fs as f64 / 2.0 - 1.0)), None);
            self.hit.sum_toggle = sum_r;
            next_x1 = sum_r.x0 - 10.0;
        } else {
            self.hit.sum_toggle = Rect::ZERO;
        }

        // Tree/Flat view pill, left of the sum pill.
        let tree_text = if self.tree_mode { "⊟ Tree" } else { "≣ Flat" };
        let tw_ = tr.measure(tree_text, fs, None).0 as f64 + 24.0;
        let tree_r = Rect::new(next_x1 - tw_, r.y0 + 2.0, next_x1, r.y1 - 2.0);
        charts::fill_rrect(scene, t, tree_r, tree_r.height() / 2.0, if self.tree_mode { with_alpha(c.accent[5], 60) } else { c.surface_white_5 });
        charts::stroke_rrect(scene, t, tree_r, tree_r.height() / 2.0, if self.tree_mode { c.accent[5] } else { c.border_standard }, 1.0);
        let (ttw, _) = tr.measure(tree_text, fs, None);
        tr.draw_text(scene, tree_text, fs, if self.tree_mode { c.text_primary } else { c.text_dim }, t * Affine::translate((tree_r.center().x - ttw as f64 / 2.0, tree_r.center().y - fs as f64 / 2.0 - 1.0)), None);
        self.hit.tree_toggle = tree_r;

        // Count label between search and the toggle cluster.
        let total = self.model.processes.iter().filter(|p| self.prefs.show_kernel_threads || !p.kernel).count();
        let shown = self.visible_rows().len();
        let count = if let Some(pid) = self.copied_pid {
            format!("PID {pid} copied  ·  {shown} of {total} shown")
        } else {
            format!("{shown} of {total} shown")
        };
        let (cw, _) = tr.measure(&count, fs, None);
        let cluster_left = tree_r.x0;
        let cxx = (search_r.x1 + cluster_left) / 2.0 - cw as f64 / 2.0;
        // Only draw the count when it actually fits between the search box and
        // the toggle cluster, so it never overlaps a pill on a narrow toolbar.
        if cluster_left - (search_r.x1 + 10.0) > cw as f64 + 8.0 {
            tr.draw_text(scene, &count, fs, c.text_dim, t * Affine::translate((cxx.max(search_r.x1 + 10.0), r.center().y - fs as f64 / 2.0 - 1.0)), None);
        }
    }

    fn draw_detail(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect) {
        let c = &theme.tokens.colors;
        let inner = charts::panel(scene, t, r, theme, 12.0);
        self.hit.kill_btn = Rect::ZERO;
        let Some(p) = self.selected_process() else {
            tr.draw_text(scene, "Select a process to view details and actions.", theme.tokens.typography.scale.sm, c.text_dim, t * Affine::translate((inner.x0, inner.center().y - 8.0)), None);
            return;
        };
        // Name + pid.
        tr.draw_text_weighted(scene, &p.name, theme.tokens.typography.scale.lg, c.text_primary, t * Affine::translate((inner.x0, inner.y0)), None, TextWeight::Bold);
        tr.draw_text(scene, &format!("PID {}  ·  {}  ·  {}", p.pid, p.user, p.status), theme.tokens.typography.scale.xs, c.text_dim, t * Affine::translate((inner.x0, inner.y0 + 24.0)), None);

        // Stat chips.
        let chip = |scene: &mut Scene, tr: &mut TextRenderer, x: f64, label: &str, value: &str, color: Color| -> f64 {
            let fs = theme.tokens.typography.scale.xs;
            let vs = theme.tokens.typography.scale.base;
            tr.draw_text(scene, label, fs, c.text_dim, t * Affine::translate((x, inner.y1 - 34.0)), None);
            tr.draw_text_weighted(scene, value, vs, color, t * Affine::translate((x, inner.y1 - 20.0)), None, TextWeight::Bold);
            let w1 = tr.measure(label, fs, None).0 as f64;
            let w2 = tr.measure_weighted(value, vs, None, TextWeight::Bold).0 as f64;
            x + w1.max(w2) + 28.0
        };
        let mut x = inner.x0;
        x = chip(scene, tr, x, "CPU", &format!("{:.1}%", p.cpu), charts::load_color(theme, (p.cpu / 100.0).clamp(0.0, 1.0)));
        x = chip(scene, tr, x, "MEMORY", &fmt_bytes(p.mem_bytes), c.text_primary);
        x = chip(scene, tr, x, "THREADS", &p.threads.to_string(), c.text_primary);
        let _ = chip(scene, tr, x, "STATUS", &p.status, c.semantic.info);

        // Kill button (danger), bottom-right.
        let kw = 96.0;
        let kh = 32.0;
        let kr = Rect::new(inner.x1 - kw, inner.y1 - kh, inner.x1, inner.y1);
        charts::fill_rrect(scene, t, kr, theme.tokens.radii.md as f64, c.semantic.danger);
        let kl = "Kill";
        let kfs = theme.tokens.typography.scale.sm;
        let icon_r = Rect::new(kr.x0 + 16.0, kr.center().y - 8.0, kr.x0 + 32.0, kr.center().y + 8.0);
        icons::draw_icon(scene, t, icon_r, Icon::X, Color::WHITE, 1.8);
        let (klw, klh) = tr.measure_weighted(kl, kfs, None, TextWeight::Semibold);
        tr.draw_text_weighted(scene, kl, kfs, Color::WHITE, t * Affine::translate((icon_r.x1 + 6.0, kr.center().y - klh as f64 / 2.0)), None, TextWeight::Semibold);
        let _ = klw;
        self.hit.kill_btn = kr;
    }

    fn draw_resources(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect) {
        let c = &theme.tokens.colors;
        let n = self.model.metrics.cores.len().max(1);

        // Per-core grid panel. Columns fit the width; rows wrap to however
        // many the core count needs, and the card grows to hold *every* row
        // (`layout::per_core_card_h`) so the last cores never render below
        // the card background. The whole view scrolls when this pushes the
        // content past the viewport, so the card is sized to its content
        // rather than clamped to a fraction of the viewport.
        let grid_inner_w = r.width() - 2.0 * layout::RAIL_CARD_PAD;
        let grid = layout::per_core_grid(n, grid_inner_w);
        let core_panel_h = layout::per_core_card_h(grid);
        let core_panel = Rect::new(r.x0, r.y0, r.x1, r.y0 + core_panel_h);
        let ci = charts::panel(scene, t, core_panel, theme, layout::RAIL_CARD_PAD);
        tr.draw_text_weighted(scene, &format!("Per-Core Utilisation  ·  {} threads", n), theme.tokens.typography.scale.sm, c.text_primary, t * Affine::translate((ci.x0, ci.y0)), None, TextWeight::Semibold);
        let grid_top = ci.y0 + layout::CORE_TITLE_H;
        let cols = grid.cols;
        let cell_w = (ci.width() - (cols as f64 - 1.0) * layout::CORE_GAP) / cols as f64;
        let cores = self.model.metrics.cores.clone();
        for (i, &usage) in cores.iter().enumerate() {
            let col = i % cols;
            let rowi = i / cols;
            let x0 = ci.x0 + col as f64 * (cell_w + layout::CORE_GAP);
            let y0 = grid_top + rowi as f64 * layout::CORE_ROW_STRIDE;
            charts::bar_gauge(
                scene, t, tr,
                Rect::new(x0, y0, x0 + cell_w, y0 + 22.0),
                theme, usage / 100.0, charts::load_color(theme, usage / 100.0),
                &format!("C{i}"), &format!("{:.0}%", usage),
            );
        }

        // 2×2 chart grid below. Give each chart a sensible minimum height so
        // it stays legible; when the viewport is short the page scrolls.
        let charts_top = core_panel.y1 + GAP;
        let m = &self.model.metrics;
        let min_chart_h = 150.0;
        let avail_h = (r.y1 - charts_top).max(2.0 * min_chart_h + GAP);
        let area = Rect::new(r.x0, charts_top, r.x1, charts_top + avail_h);
        let cw = (area.width() - GAP) / 2.0;
        let ch = (area.height() - GAP) / 2.0;
        let cell = |row: usize, col: usize| {
            let x0 = area.x0 + col as f64 * (cw + GAP);
            let y0 = area.y0 + row as f64 * (ch + GAP);
            Rect::new(x0, y0, x0 + cw, y0 + ch)
        };

        // CPU%.
        charts::area_chart(
            scene, t, tr, cell(0, 0), theme, "CPU",
            &[Series { data: m.cpu_hist.as_slice(), color: c.accent[5], legend: format!("{:.0}%", m.cpu_overall()) }],
            100.0, "100%", "0%",
        );
        // Memory%.
        charts::area_chart(
            scene, t, tr, cell(0, 1), theme, "Memory",
            &[Series { data: m.mem_hist.as_slice(), color: c.semantic.success, legend: fmt_bytes(m.mem_used) }],
            1.0, "100%", "0%",
        );
        // Network rx/tx.
        let net_max = m.net_rx_hist.max().max(m.net_tx_hist.max()).max(1.0) * 1.2;
        charts::area_chart(
            scene, t, tr, cell(1, 0), theme, "Network",
            &[
                Series { data: m.net_rx_hist.as_slice(), color: c.semantic.info, legend: format!("↓ {}", fmt_rate(m.net_rx)) },
                Series { data: m.net_tx_hist.as_slice(), color: c.accent[6], legend: format!("↑ {}", fmt_rate(m.net_tx)) },
            ],
            net_max, &fmt_rate(net_max), "0",
        );
        // Disk read/write.
        let disk_max = m.disk_read_hist.max().max(m.disk_write_hist.max()).max(1.0) * 1.2;
        charts::area_chart(
            scene, t, tr, cell(1, 1), theme, "Disk I/O",
            &[
                Series { data: m.disk_read_hist.as_slice(), color: c.semantic.warning, legend: format!("R {}", fmt_rate(m.disk_read)) },
                Series { data: m.disk_write_hist.as_slice(), color: c.semantic.danger, legend: format!("W {}", fmt_rate(m.disk_write)) },
            ],
            disk_max, &fmt_rate(disk_max), "0",
        );
    }

    fn draw_modal(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, pid: u32, kind: StopKind) {
        let c = &theme.tokens.colors;
        // Scrim.
        scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 150), None, &self.rect);
        let name = self.model.processes.iter().find(|p| p.pid == pid).map(|p| p.name.clone()).unwrap_or_else(|| "process".into());
        let (title, signal_name) = match kind {
            StopKind::Term => ("Stop process?", "SIGTERM"),
            StopKind::Kill => ("Kill process?", "SIGKILL"),
        };

        let w = 400.0;
        let h = 168.0;
        let cx = self.rect.center().x;
        let cy = self.rect.center().y;
        let dlg = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        charts::fill_rrect(scene, t, dlg, theme.tokens.radii.md as f64 + 4.0, c.surfaces.raised);
        charts::stroke_rrect(scene, t, dlg, theme.tokens.radii.md as f64 + 4.0, c.border_standard, 1.0);
        let inner = Rect::new(dlg.x0 + 20.0, dlg.y0 + 18.0, dlg.x1 - 20.0, dlg.y1 - 18.0);

        // Danger glyph + title.
        let icon_r = Rect::new(inner.x0, inner.y0, inner.x0 + 22.0, inner.y0 + 22.0);
        icons::draw_icon(scene, t, icon_r, Icon::Power, c.semantic.danger, 1.8);
        tr.draw_text_weighted(scene, title, theme.tokens.typography.scale.lg, c.text_primary, t * Affine::translate((icon_r.x1 + 10.0, inner.y0)), None, TextWeight::Bold);
        let body = format!("{} (PID {}) will be sent {}. Unsaved work will be lost.", name, pid, signal_name);
        let body = Self::truncate(tr, &body, theme.tokens.typography.scale.sm, inner.width());
        tr.draw_text(scene, &body, theme.tokens.typography.scale.sm, c.text_dim, t * Affine::translate((inner.x0, inner.y0 + 36.0)), None);

        // Buttons.
        let bw = 110.0;
        let bh = 36.0;
        let kill_r = Rect::new(inner.x1 - bw, inner.y1 - bh, inner.x1, inner.y1);
        let cancel_r = Rect::new(kill_r.x0 - bw - 10.0, inner.y1 - bh, kill_r.x0 - 10.0, inner.y1);
        let radius = theme.tokens.radii.md as f64;
        // Cancel.
        charts::fill_rrect(scene, t, cancel_r, radius, c.surface_white_5);
        charts::stroke_rrect(scene, t, cancel_r, radius, c.border_standard, 1.0);
        Self::center_label(scene, t, tr, theme, cancel_r, "Cancel", c.text_primary);
        // Confirm (danger) — labelled for the signal being sent.
        let confirm_label = match kind {
            StopKind::Term => "Stop",
            StopKind::Kill => "Kill",
        };
        charts::fill_rrect(scene, t, kill_r, radius, c.semantic.danger);
        Self::center_label(scene, t, tr, theme, kill_r, confirm_label, Color::WHITE);
        self.hit.modal_kill = kill_r;
        self.hit.modal_cancel = cancel_r;
    }

    fn center_label(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect, label: &str, color: Color) {
        let fs = theme.tokens.typography.scale.sm;
        let (w, h) = tr.measure_weighted(label, fs, None, TextWeight::Semibold);
        tr.draw_text_weighted(scene, label, fs, color, t * Affine::translate((r.center().x - w as f64 / 2.0, r.center().y - h as f64 / 2.0)), None, TextWeight::Semibold);
    }
}

// ==========================================================================
// Event handling
// ==========================================================================
impl MonitorApp {
    fn on_click(&mut self, p: Point, doc: &mut AutoCommit, path: &ObjId, handled: &mut bool) {
        // Modal takes priority.
        if let Some((pid, kind)) = self.confirm_stop {
            if self.hit.modal_kill.contains(p) {
                self.do_stop(pid, kind, doc);
                *handled = true;
            } else if self.hit.modal_cancel.contains(p) {
                self.confirm_stop = None;
                *handled = true;
            } else {
                *handled = true; // swallow clicks behind the scrim
            }
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

        // The rail flap and its scrim float above everything else, so they
        // claim the click first. The hamburger toggle is painted on top of the
        // scrim, so test it before the scrim.
        let layout = self.rail_layout();
        if self.show_rail_toggle() && self.hit.rail_toggle.contains(p) {
            self.search_focused = false;
            self.rail.toggle(layout);
            *handled = true;
            return;
        }
        // An open overlay rail dims the content; tapping the scrim (anywhere
        // outside the floated rail) dismisses it and swallows the click.
        if layout == SplitLayout::Overlay && self.rail.is_open(layout) {
            self.search_focused = false;
            self.rail.close();
            *handled = true;
            return;
        }

        // Lose search focus unless the search box itself is clicked.
        let clicked_search = self.hit.search.contains(p);
        self.search_focused = clicked_search;

        // Back affordance in the narrow pushed detail pane → return to table.
        if self.hit.detail_back.contains(p) {
            self.proc_push.show_table();
            *handled = true;
            return;
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
            self.prefs.cycle_interval();
            self.prefs.store(doc, path);
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

    fn on_key(&mut self, key: &Key, text: Option<&str>, ctrl: bool, doc: &mut AutoCommit, path: &ObjId, handled: &mut bool) {
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

        // Context menu open: Esc dismisses it, everything else is swallowed.
        if self.context.is_open() {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.close_context_menu();
            }
            *handled = true;
            return;
        }

        // Modal.
        if let Some((pid, kind)) = self.confirm_stop {
            match key {
                Key::Named(NamedKey::Enter) => {
                    self.do_stop(pid, kind, doc);
                    *handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.confirm_stop = None;
                    *handled = true;
                }
                _ => *handled = true,
            }
            return;
        }

        // Search editing.
        if self.search_focused {
            match key {
                Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter) => {
                    self.search_focused = false;
                    *handled = true;
                }
                Key::Named(NamedKey::Backspace) => {
                    self.filter.pop();
                    self.scroll_off = 0.0;
                    *handled = true;
                }
                Key::Named(NamedKey::Space) => {
                    self.filter.push(' ');
                    *handled = true;
                }
                _ => {
                    if let Some(txt) = text {
                        let mut pushed = false;
                        for ch in txt.chars() {
                            if !ch.is_control() {
                                self.filter.push(ch);
                                pushed = true;
                            }
                        }
                        if pushed {
                            self.scroll_off = 0.0;
                            *handled = true;
                        }
                    }
                }
            }
            return;
        }

        // Global keys.
        match key {
            // Esc unwinds the responsive stack: dismiss an open overlay rail
            // first, otherwise pop a pushed detail pane back to the table.
            Key::Named(NamedKey::Escape) => {
                let layout = self.rail_layout();
                if layout == SplitLayout::Overlay && self.rail.is_open(layout) {
                    self.rail.close();
                    *handled = true;
                } else if self.is_narrow() && self.proc_push.pane() == ProcPane::Detail {
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
                        self.search_focused = true;
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

        // Resolve the rail layout from the allocated width and advance the
        // eased flaps once per frame (the redraw loop keeps pumping while
        // `needs_animation_frame` is true).
        let layout = self.rail_layout();
        self.rail.reconcile(layout);
        self.rail.tick(layout);
        self.proc_push.tick();

        let content = Rect::new(r.x0 + OUTER_PAD, r.y0 + OUTER_PAD, r.x1 - OUTER_PAD, r.y1 - OUTER_PAD);
        let header = Rect::new(content.x0, content.y0, content.x1, content.y0 + HEADER_H);
        let body_top = header.y1 + GAP;
        let body = Rect::new(content.x0, body_top, content.x1, content.y1);

        // The rail's docked footprint at full reveal, and the content column
        // beside it. Docked: the content is inset by the rail's *revealed*
        // width so it animates with the slide. Overlay: the content uses the
        // full body width and the rail floats over it.
        let rail_docked = Rect::new(body.x0, body.y0, body.x0 + RAIL_W, body.y1);
        let main_full = match layout {
            SplitLayout::Docked => {
                let inset = (RAIL_W + GAP) * self.rail.reveal();
                Rect::new(body.x0 + inset, body.y0, body.x1, body.y1)
            }
            SplitLayout::Overlay => body,
        };
        // When the hamburger toggle floats over the content's top-left corner
        // (narrow overlay), inset the content column past it so the first row
        // — the search toolbar, or the per-core card title — clears the
        // toggle instead of rendering under it. The toggle itself is still
        // painted at `main_full`'s corner.
        let main = if self.show_rail_toggle() && layout == SplitLayout::Overlay {
            Rect::new(main_full.x0 + 34.0 + GAP, main_full.y0, main_full.x1, main_full.y1)
        } else {
            main_full
        };

        self.draw_header(scene, t, text_renderer, theme, header);

        // ---- whole-view vertical scroll -------------------------------
        // The body (rail column + active tab) can be taller than its
        // viewport — a docked rail's info card, or the Resources tab's
        // per-core grid + 2×2 charts on a large machine / short window.
        // Mirror `ui::widget::Scrollable`'s model: draw the body content at
        // `(… − offset)` inside a clip layer the size of the viewport, clamp
        // the offset to `content − viewport`, and paint a scrollbar. The
        // header stays pinned above the scroll. The Processes tab manages its
        // own internal row scroll, so it reports zero page-content height and
        // never page-scrolls.
        let viewport_h = body.height();
        let content_natural_h = self.content_natural_h(main.width());
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
        let main_scrolled = Rect::new(main.x0, top, main.x1, content_bottom);
        let rail_scrolled = Rect::new(rail_docked.x0, top, rail_docked.x1, content_bottom);

        // Content first; the overlay rail (if any) paints on top of it.
        match self.tab {
            Tab::Processes => self.draw_processes(scene, t, text_renderer, theme, main_scrolled),
            Tab::Resources => self.draw_resources(scene, t, text_renderer, theme, main_scrolled),
        }

        // The system rail: docked beside the content (scrolls with the page),
        // or an overlay flap that slides in over it behind a dimming scrim
        // (pinned — overlays never page-scroll, so it uses the un-shifted
        // footprint and paints after the clip is popped).
        if layout == SplitLayout::Docked {
            self.draw_rail_region(scene, t, text_renderer, theme, rail_scrolled, layout);
        }

        if scrolling {
            scene.pop_layer();
            // Page scrollbar down the right edge of the body viewport.
            let track = Rect::new(body.x1 - 5.0, body.y0, body.x1 - 1.0, body.y1);
            let c = &theme.tokens.colors;
            charts::fill_rrect(scene, t, track, 2.0, with_alpha(c.border_standard, 90));
            let frac_v = (viewport_h / self.content_h).clamp(0.0, 1.0);
            let thumb_h = (track.height() * frac_v).max(24.0);
            let pos = if page_overflow > 0.0 { dy / page_overflow } else { 0.0 };
            let ty0 = track.y0 + (track.height() - thumb_h) * pos;
            charts::fill_rrect(scene, t, Rect::new(track.x0, ty0, track.x1, ty0 + thumb_h), 2.0, with_alpha(c.text_dim, 160));
        }

        // The overlay rail flap floats above the (now-popped) scroll clip and
        // the page scrollbar, pinned to the un-shifted body so it never
        // page-scrolls.
        if layout == SplitLayout::Overlay {
            self.draw_rail_region(scene, t, text_renderer, theme, rail_docked, layout);
        }

        // Hamburger / close affordance over the content's top-left corner,
        // shown whenever the rail isn't fully docked-open. Pinned (not
        // scrolled) so it stays reachable.
        if self.show_rail_toggle() {
            self.draw_rail_toggle(scene, t, theme, main_full);
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

        if let Some((pid, kind)) = self.confirm_stop {
            self.draw_modal(scene, t, text_renderer, theme, pid, kind);
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
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
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
                    // Anywhere else (Resources tab, the rail column, the
                    // toolbar) scrolls the whole page.
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
            WindowEvent::KeyboardInput { event: ke, .. }
                if ke.state == ElementState::Pressed =>
            {
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
        // Settle the rail flap for this width so the layout is stable.
        let layout = a.rail_layout();
        a.rail.reconcile(layout);
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        a.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
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
        // seat all six minimum columns.
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
        assert!(handled2 && a.search_focused, "bare / still focuses local search");
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
        // than the padded columns viewport.
        a.table_content_w = 528.0; // sum of the six min columns
        a.table_cols_w = 300.0; // padded band (narrow window)
        let max_x = (a.table_content_w - a.table_cols_w).max(0.0) as f32; // 228
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
        // Narrow enough that the columns overflow the table card.
        draw_at(&mut a, 520.0, 700.0);
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
                device_id: unsafe { winit::event::DeviceId::dummy() },
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
        assert!(labels.iter().any(|l| l == "Show details"), "has Show details");
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
}
