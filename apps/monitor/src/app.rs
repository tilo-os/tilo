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

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::icons::{self, Icon};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{SplitLayout, Widget};

use crate::charts::{self, with_alpha, Series};
use crate::model::{fmt_bytes, fmt_rate, fmt_uptime, MonitorModel, ProcessInfo};
use crate::prefs::Prefs;
use crate::responsive::{ProcPane, ProcPush, RailFlap};
use crate::sampler::Sampler;

#[derive(Clone, Copy, PartialEq, Eq)]
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

#[derive(Clone, Copy, PartialEq, Eq)]
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

/// Hit rectangles recorded during `draw`, consumed by `handle_event`.
struct Hit {
    tab_proc: Rect,
    tab_res: Rect,
    interval: Rect,
    search: Rect,
    kernel_toggle: Rect,
    sort_headers: Vec<(SortKey, Rect)>,
    rows: Vec<(u32, Rect)>,
    kill_btn: Rect,
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
            kill_btn: Rect::ZERO,
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
    confirm_kill: Option<u32>,

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
            confirm_kill: None,
            rail: RailFlap::default(),
            proc_push: ProcPush::default(),
            rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            // Set to "now" so the first (screenshot) frame skips the live
            // refresh and renders the seeded sample untouched.
            last_refresh: Instant::now(),
            rows_per_page: 1,
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

    fn clamp_scroll(&mut self) {
        let n = self.visible_processes().len();
        let max = n.saturating_sub(self.rows_per_page) as f32;
        self.scroll_off = self.scroll_off.clamp(0.0, max);
    }

    fn set_sort(&mut self, key: SortKey) {
        if self.sort_key == key {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort_key = key;
            self.sort_desc = key.default_desc();
        }
    }

    fn request_kill(&mut self, pid: u32, doc: &mut AutoCommit, _path: &ObjId) {
        if self.prefs.confirm_kill {
            self.confirm_kill = Some(pid);
        } else {
            self.do_kill(pid, doc);
        }
    }

    fn do_kill(&mut self, pid: u32, _doc: &mut AutoCommit) {
        // Live: dispatch SIGKILL. Sample/offscreen: just drop the row.
        let _ = self.sampler.kill(pid);
        self.model.processes.retain(|p| p.pid != pid);
        if self.selected_pid == Some(pid) {
            self.selected_pid = None;
        }
        self.confirm_kill = None;
        self.clamp_scroll();
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
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        match scenario {
            "rail" => self.rail.snap(true),
            "detail" => self.proc_push.snap(ProcPane::Detail),
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

        // Brand: monitor glyph + title.
        let glyph = Rect::new(inner.x0, inner.y0 + 2.0, inner.x0 + 26.0, inner.y0 + 28.0);
        icons::draw_icon(scene, t, glyph, Icon::Monitor, c.accent[5], 1.8);
        tr.draw_text_weighted(
            scene,
            "Monitor",
            theme.tokens.typography.scale.xl,
            c.text_primary,
            t * Affine::translate((glyph.x1 + 10.0, inner.y0 - 1.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text(
            scene,
            "System Activity",
            theme.tokens.typography.scale.xs,
            c.text_dim,
            t * Affine::translate((glyph.x1 + 11.0, inner.y0 + 20.0)),
            None,
        );

        // Centre: tab segmented control.
        let seg_w = 124.0;
        let seg_h = 30.0;
        let cy = inner.y0 + (inner.height() - seg_h) / 2.0;
        let cx = (inner.x0 + inner.x1) / 2.0 - seg_w;
        let proc_r = Rect::new(cx, cy, cx + seg_w, cy + seg_h);
        let res_r = Rect::new(cx + seg_w + 6.0, cy, cx + 2.0 * seg_w + 6.0, cy + seg_h);
        self.draw_tab_pill(scene, t, tr, theme, proc_r, Icon::Layers, "Processes", self.tab == Tab::Processes);
        self.draw_tab_pill(scene, t, tr, theme, res_r, Icon::BarChart, "Resources", self.tab == Tab::Resources);
        self.hit.tab_proc = proc_r;
        self.hit.tab_res = res_r;

        // Right: refresh-interval pill + clock.
        let clock = Self::clock();
        let cs = theme.tokens.typography.scale.lg;
        let (clw, _) = tr.measure_weighted(&clock, cs, None, TextWeight::Bold);
        tr.draw_text_weighted(
            scene,
            &clock,
            cs,
            c.text_primary,
            t * Affine::translate((inner.x1 - clw as f64, inner.y0 + 1.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text(
            scene,
            "uptime",
            theme.tokens.typography.scale.xs,
            c.text_dim,
            t * Affine::translate((inner.x1 - clw as f64, inner.y0 + 22.0)),
            None,
        );
        let up = fmt_uptime(self.model.metrics.uptime_secs);
        let (uw, _) = tr.measure(&up, theme.tokens.typography.scale.xs, None);
        tr.draw_text(
            scene,
            &up,
            theme.tokens.typography.scale.xs,
            c.text_dim,
            t * Affine::translate((inner.x1 - clw as f64 - uw as f64 - 8.0, inner.y0 + 22.0)),
            None,
        );

        // Interval pill, left of the clock block.
        let il = format!("⟳ {}", self.prefs.interval_label());
        let iw = tr.measure_weighted(&il, theme.tokens.typography.scale.sm, None, TextWeight::Medium).0 as f64 + 22.0;
        let pill_x1 = inner.x1 - clw.max(70.0) as f64 - 24.0;
        let interval_r = Rect::new(pill_x1 - iw, cy + 1.0, pill_x1, cy + seg_h - 1.0);
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

    fn draw_tab_pill(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect, icon: Icon, label: &str, active: bool) {
        let c = &theme.tokens.colors;
        let radius = r.height() / 2.0;
        if active {
            charts::fill_rrect(scene, t, r, radius, c.accent[5]);
        } else {
            charts::fill_rrect(scene, t, r, radius, c.surface_white_5);
            charts::stroke_rrect(scene, t, r, radius, c.border_standard, 1.0);
        }
        let text_color = if active { Color::WHITE } else { c.text_dim };
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

        // Section 3: net + disk sparklines.
        let sp_y = mem_panel.y1 + GAP;
        let sp_panel = Rect::new(r.x0, sp_y, r.x1, sp_y + 150.0);
        let si = charts::panel(scene, t, sp_panel, theme, 12.0);
        let spark = |scene: &mut Scene, tr: &mut TextRenderer, y: f64, label: &str, val: &str, data: &[f32], color: Color| {
            tr.draw_text_weighted(scene, label, theme.tokens.typography.scale.xs, c.text_primary, t * Affine::translate((si.x0, y)), None, TextWeight::Medium);
            let (vw, _) = tr.measure(val, theme.tokens.typography.scale.xs, None);
            tr.draw_text(scene, val, theme.tokens.typography.scale.xs, c.text_dim, t * Affine::translate((si.x1 - vw as f64, y)), None);
            let sr = Rect::new(si.x0, y + 14.0, si.x1, y + 46.0);
            let mx = data.iter().copied().fold(0.0_f32, f32::max).max(1.0);
            charts::sparkline(scene, t, sr, data, mx, color);
        };
        spark(scene, tr, si.y0, "Net ↓", &fmt_rate(m.net_rx), m.net_rx_hist.as_slice(), c.semantic.info);
        spark(scene, tr, si.y0 + 52.0, "Net ↑", &fmt_rate(m.net_tx), m.net_tx_hist.as_slice(), c.accent[6]);
        spark(scene, tr, si.y0 + 104.0, "Disk", &format!("{} / {}", fmt_rate(m.disk_read), fmt_rate(m.disk_write)), m.disk_read_hist.as_slice(), c.semantic.warning);

        // Section 4: system info text.
        let info_y = sp_panel.y1 + GAP;
        if info_y + 40.0 < r.y1 {
            let info_panel = Rect::new(r.x0, info_y, r.x1, r.y1);
            let ii = charts::panel(scene, t, info_panel, theme, 12.0);
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
        let pad = 12.0;
        let cols_x0 = ti.x0 + pad;
        let cols_x1 = ti.x1 - pad;

        // Column layout.
        let pid_w = 60.0;
        let user_w = 116.0;
        let cpu_w = 72.0;
        let mem_w = 96.0;
        let thr_w = 52.0;
        let name_w = (cols_x1 - cols_x0 - pid_w - user_w - cpu_w - mem_w - thr_w).max(80.0);
        // (key, header, x0, width, right_align)
        let columns: [(SortKey, &str, f64, f64, bool); 6] = [
            (SortKey::Pid, "PID", cols_x0, pid_w, true),
            (SortKey::Name, "NAME", cols_x0 + pid_w, name_w, false),
            (SortKey::User, "USER", cols_x0 + pid_w + name_w, user_w, false),
            (SortKey::Cpu, "CPU%", cols_x0 + pid_w + name_w + user_w, cpu_w, true),
            (SortKey::Mem, "MEM", cols_x0 + pid_w + name_w + user_w + cpu_w, mem_w, true),
            (SortKey::Threads, "THR", cols_x0 + pid_w + name_w + user_w + cpu_w + mem_w, thr_w, true),
        ];

        // Header row.
        let hdr = Rect::new(ti.x0, ti.y0, ti.x1, ti.y0 + TABLE_HEADER_H);
        charts::fill_rrect(scene, t, hdr, 0.0, c.surfaces.base);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &Line::new((hdr.x0, hdr.y1), (hdr.x1, hdr.y1)));
        let hfs = theme.tokens.typography.scale.xs;
        self.hit.sort_headers.clear();
        for (key, label, x0, w, right) in columns {
            let active = self.sort_key == key;
            let color = if active { c.text_primary } else { c.text_dim };
            let mut text = label.to_string();
            if active {
                text.push(' ');
                text.push(if self.sort_desc { '▼' } else { '▲' });
            }
            let (lw, lh) = tr.measure_weighted(&text, hfs, None, TextWeight::Semibold);
            let tx = if right { x0 + w - lw as f64 - 6.0 } else { x0 + 2.0 };
            tr.draw_text_weighted(scene, &text, hfs, color, t * Affine::translate((tx, hdr.center().y - lh as f64 / 2.0)), None, TextWeight::Semibold);
            self.hit.sort_headers.push((key, Rect::new(x0, hdr.y0, x0 + w, hdr.y1)));
        }

        // Rows region.
        let view = Rect::new(ti.x0, hdr.y1, ti.x1, ti.y1);
        self.rows_per_page = (view.height() / ROW_H).floor().max(1.0) as usize;
        let procs = self.visible_processes();
        let max_scroll = procs.len().saturating_sub(self.rows_per_page) as f32;
        self.scroll_off = self.scroll_off.clamp(0.0, max_scroll);
        let start = self.scroll_off as usize;

        // Clip rows to the view.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &view);
        self.hit.rows.clear();
        let fs = theme.tokens.typography.scale.sm;
        for i in 0..self.rows_per_page {
            let idx = start + i;
            let Some(p) = procs.get(idx) else { break };
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

            // helper to draw a cell
            let cell = |tr: &mut TextRenderer, scene: &mut Scene, s: &str, x0: f64, w: f64, right: bool, color: Color, weight: TextWeight| {
                let s = Self::truncate(tr, s, fs, w - 8.0);
                let (sw, _) = tr.measure_weighted(&s, fs, None, weight);
                let x = if right { x0 + w - sw as f64 - 6.0 } else { x0 + 2.0 };
                tr.draw_text_weighted(scene, &s, fs, color, t * Affine::translate((x, ty)), None, weight);
            };
            cell(tr, scene, &p.pid.to_string(), columns[0].2, columns[0].3, true, txt, TextWeight::Regular);
            cell(tr, scene, &p.name, columns[1].2, columns[1].3, false, name_color, TextWeight::Medium);
            cell(tr, scene, &p.user, columns[2].2, columns[2].3, false, txt, TextWeight::Regular);
            // CPU% coloured by load.
            let cpu_frac = (p.cpu / 100.0).clamp(0.0, 1.0);
            let cpu_col = if p.cpu >= 1.0 { charts::load_color(theme, cpu_frac) } else { txt };
            cell(tr, scene, &format!("{:.1}", p.cpu), columns[3].2, columns[3].3, true, cpu_col, TextWeight::Medium);
            cell(tr, scene, &fmt_bytes(p.mem_bytes), columns[4].2, columns[4].3, true, txt, TextWeight::Regular);
            cell(tr, scene, &p.threads.to_string(), columns[5].2, columns[5].3, true, txt, TextWeight::Regular);

            self.hit.rows.push((p.pid, row));
        }
        scene.pop_layer();

        // Scrollbar.
        if procs.len() > self.rows_per_page {
            let track = Rect::new(view.x1 - 5.0, view.y0, view.x1 - 1.0, view.y1);
            charts::fill_rrect(scene, t, track, 2.0, with_alpha(c.border_standard, 90));
            let frac_v = self.rows_per_page as f64 / procs.len() as f64;
            let thumb_h = (track.height() * frac_v).max(24.0);
            let max_off = (procs.len() - self.rows_per_page) as f64;
            let pos = if max_off > 0.0 { self.scroll_off as f64 / max_off } else { 0.0 };
            let ty0 = track.y0 + (track.height() - thumb_h) * pos;
            charts::fill_rrect(scene, t, Rect::new(track.x0, ty0, track.x1, ty0 + thumb_h), 2.0, with_alpha(c.text_dim, 160));
        }
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

        // Count label between search and toggle.
        let total = self.model.processes.iter().filter(|p| self.prefs.show_kernel_threads || !p.kernel).count();
        let shown = self.visible_processes().len();
        let count = format!("{shown} of {total} processes");
        let (cw, _) = tr.measure(&count, fs, None);
        let cxx = (search_r.x1 + kt_r.x0) / 2.0 - cw as f64 / 2.0;
        tr.draw_text(scene, &count, fs, c.text_dim, t * Affine::translate((cxx.max(search_r.x1 + 10.0), r.center().y - fs as f64 / 2.0 - 1.0)), None);
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

    fn draw_resources(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, r: Rect) {
        let c = &theme.tokens.colors;
        let m = &self.model.metrics;

        // Per-core grid panel.
        let n = m.cores.len().max(1);
        let cols = ((r.width() / 220.0).floor() as usize).clamp(1, 8).min(n);
        let rows_n = n.div_ceil(cols);
        let core_panel_h = (28.0 + rows_n as f64 * 30.0 + 14.0).min(r.height() * 0.42);
        let core_panel = Rect::new(r.x0, r.y0, r.x1, r.y0 + core_panel_h);
        let ci = charts::panel(scene, t, core_panel, theme, 12.0);
        tr.draw_text_weighted(scene, &format!("Per-Core Utilisation  ·  {} threads", n), theme.tokens.typography.scale.sm, c.text_primary, t * Affine::translate((ci.x0, ci.y0)), None, TextWeight::Semibold);
        let grid_top = ci.y0 + 22.0;
        let cell_w = (ci.width() - (cols as f64 - 1.0) * 14.0) / cols as f64;
        for (i, &usage) in m.cores.iter().enumerate() {
            let col = i % cols;
            let rowi = i / cols;
            let x0 = ci.x0 + col as f64 * (cell_w + 14.0);
            let y0 = grid_top + rowi as f64 * 30.0;
            charts::bar_gauge(
                scene, t, tr,
                Rect::new(x0, y0, x0 + cell_w, y0 + 22.0),
                theme, usage / 100.0, charts::load_color(theme, usage / 100.0),
                &format!("C{i}"), &format!("{:.0}%", usage),
            );
        }

        // 2×2 chart grid below.
        let charts_top = core_panel.y1 + GAP;
        let area = Rect::new(r.x0, charts_top, r.x1, r.y1);
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

    fn draw_modal(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, pid: u32) {
        let c = &theme.tokens.colors;
        // Scrim.
        scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 150), None, &self.rect);
        let name = self.model.processes.iter().find(|p| p.pid == pid).map(|p| p.name.clone()).unwrap_or_else(|| "process".into());

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
        tr.draw_text_weighted(scene, "Kill process?", theme.tokens.typography.scale.lg, c.text_primary, t * Affine::translate((icon_r.x1 + 10.0, inner.y0)), None, TextWeight::Bold);
        let body = format!("{} (PID {}) will be sent SIGKILL. Unsaved work will be lost.", name, pid);
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
        // Kill.
        charts::fill_rrect(scene, t, kill_r, radius, c.semantic.danger);
        Self::center_label(scene, t, tr, theme, kill_r, "Kill", Color::WHITE);
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
        if let Some(pid) = self.confirm_kill {
            if self.hit.modal_kill.contains(p) {
                self.do_kill(pid, doc);
                *handled = true;
            } else if self.hit.modal_cancel.contains(p) {
                self.confirm_kill = None;
                *handled = true;
            } else {
                *handled = true; // swallow clicks behind the scrim
            }
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
            self.tab = Tab::Processes;
            *handled = true;
            return;
        }
        if self.hit.tab_res.contains(p) {
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

    fn move_selection(&mut self, delta: i32) {
        let procs = self.visible_processes();
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

    fn on_key(&mut self, key: &Key, text: Option<&str>, doc: &mut AutoCommit, path: &ObjId, handled: &mut bool) {
        // Modal.
        if let Some(pid) = self.confirm_kill {
            match key {
                Key::Named(NamedKey::Enter) => {
                    self.do_kill(pid, doc);
                    *handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.confirm_kill = None;
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
        let main = match layout {
            SplitLayout::Docked => {
                let inset = (RAIL_W + GAP) * self.rail.reveal();
                Rect::new(body.x0 + inset, body.y0, body.x1, body.y1)
            }
            SplitLayout::Overlay => body,
        };

        self.draw_header(scene, t, text_renderer, theme, header);

        // Content first; the overlay rail (if any) paints on top of it.
        match self.tab {
            Tab::Processes => self.draw_processes(scene, t, text_renderer, theme, main),
            Tab::Resources => self.draw_resources(scene, t, text_renderer, theme, main),
        }

        // The system rail: docked beside the content, or an overlay flap that
        // slides in over it behind a dimming scrim.
        self.draw_rail_region(scene, t, text_renderer, theme, rail_docked, layout);

        // Hamburger / close affordance over the content's top-left corner,
        // shown whenever the rail isn't fully docked-open.
        if self.show_rail_toggle() {
            self.draw_rail_toggle(scene, t, theme, main);
        }

        if let Some(pid) = self.confirm_kill {
            self.draw_modal(scene, t, text_renderer, theme, pid);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
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
                if self.tab == Tab::Processes && self.confirm_kill.is_none() {
                    let rows = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -y * 3.0,
                        MouseScrollDelta::PixelDelta(pos) => -(pos.y as f32 / ROW_H as f32),
                    };
                    self.scroll_off = (self.scroll_off + rows).max(0.0);
                    self.clamp_scroll();
                    *handled = true;
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
            WindowEvent::KeyboardInput { event: ke, .. }
                if ke.state == ElementState::Pressed =>
            {
                let text = ke.text.as_ref().map(|s| s.as_str());
                self.on_key(&ke.logical_key, text, doc, path, handled);
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

    fn app() -> MonitorApp {
        MonitorApp::new()
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
        a.do_kill(pid, &mut doc);
        assert_eq!(a.model.processes.len(), before - 1);
        assert!(a.selected_pid.is_none());
        assert!(a.model.processes.iter().all(|p| p.pid != pid));
    }

    #[test]
    fn confirm_kill_gated_by_pref() {
        let mut a = app();
        let pid = a.selected_pid.unwrap();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        a.prefs.confirm_kill = true;
        a.request_kill(pid, &mut doc, &root);
        assert_eq!(a.confirm_kill, Some(pid));
        // Direct kill when confirmation is off.
        a.confirm_kill = None;
        a.prefs.confirm_kill = false;
        a.request_kill(pid, &mut doc, &root);
        assert!(a.confirm_kill.is_none());
        assert!(a.model.processes.iter().all(|p| p.pid != pid));
    }

    #[test]
    fn arrow_navigation_moves_selection() {
        let mut a = app();
        a.rows_per_page = 10;
        let v = a.visible_processes();
        a.selected_pid = Some(v[0].pid);
        a.move_selection(1);
        assert_eq!(a.selected_pid, Some(v[1].pid));
        a.move_selection(-1);
        assert_eq!(a.selected_pid, Some(v[0].pid));
    }
}
