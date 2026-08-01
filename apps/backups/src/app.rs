//! Root widget for the Backups app.
//!
//! The whole UI is custom-drawn into the Vello scene (a dense dashboard:
//! source rail, snapshot timeline, storage gauges, retention editor, restore
//! sheet) with our own layout + hit-testing — the kind of chart/canvas
//! surface the toolkit expects apps to build on top of `Widget`. Reusable
//! ui widgets don't fit a timeline this dense, so we lay it out by hand.
//!
//! State split:
//!   * the *catalog* (snapshot listing) is held in memory ([`Catalog`]) — in
//!     the live app it comes from btrfs/snapper or `restic snapshots`;
//!   * everything the user changes (open view, selection, restore sheet,
//!     retention policy) lives in the Automerge doc (see [`BackupsDoc`]),
//!     read into a small cache on `sync` and kept in step from `handle_event`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    SegmentedFace, SidebarDrawer, Toggle, ToggleFace, ToggleGroup, Widget,
};
use ui::{card_column_width, card_columns};

use crate::actions::{self, BACKUPS_ACTION_TYPE};
use crate::doc::{BackupsDoc, PolicyField};
use crate::model::{
    Catalog, DestKind, Destination, Health, Retention, RetentionPolicy, SnapStatus, SnapType,
    Snapshot, Source, SourceKind, View,
};
use crate::paint;

const BAR_H: f64 = 60.0;
const STATUS_H: f64 = 28.0;
const PAD: f64 = 20.0;
const RAIL_W: f64 = 256.0;
const TAB_W: f64 = 132.0;
const TAB_H: f64 = 34.0;
const ROW_H: f64 = 46.0;
const DETAIL_H: f64 = 70.0;
const DAY_H: f64 = 30.0;

/// Below this window width the Timeline source rail overlays (a summoned
/// drawer) instead of docking beside the snapshot list.
const RAIL_BREAKPOINT: f64 = 820.0;
/// Below this width the top-bar chrome compacts (icon-only primary action,
/// tighter tabs) and multi-column tile/card grids drop toward a single column.
const NARROW_BREAKPOINT: f64 = 720.0;

/// Click/hover targets discovered during `draw` and consumed by
/// `handle_event`. Cleared and rebuilt every frame.
#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Tab(View),
    Source(Option<String>),
    Snap(String),
    BackupNow,
    Restore,
    Verify,
    Delete,
    RestoreConfirm,
    RestoreCancel,
    Dest(String),
    Policy(PolicyField, i64),
    FileRow(usize),
    /// The Timeline source-rail collapse / summon toggle.
    ToggleRail,
    /// The scrim behind an open overlay rail (tap to dismiss).
    CloseRail,
}

/// An in-flight "Back up now" job. Purely visual / in-memory: it animates a
/// progress bar and, on completion, materialises a fresh manual snapshot.
struct Job {
    source: String,
    started: Instant,
    dur: Duration,
}

/// Cached copy of the CRDT UI state, refreshed on every `sync` and kept in
/// step after each handled event so `draw` (which has no doc access) sees the
/// latest selection.
struct UiCache {
    view: View,
    sel_source: Option<String>,
    sel_snapshot: Option<String>,
    sel_dest: Option<String>,
    restore_open: bool,
    policy: RetentionPolicy,
}

impl Default for UiCache {
    fn default() -> Self {
        Self {
            view: View::Timeline,
            sel_source: None,
            sel_snapshot: None,
            sel_dest: Some("pool".into()),
            restore_open: false,
            policy: RetentionPolicy::default(),
        }
    }
}

pub struct BackupsApp {
    catalog: Catalog,
    ui: UiCache,
    rect: Rect,
    node: Option<NodeId>,
    hits: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
    last_mouse: Point,
    timeline_scroll: f64,
    timeline_view: Rect,
    timeline_content_h: f64,
    policy_focus: usize,
    file_sel: usize,
    job: Option<Job>,
    toast: Option<(String, Instant)>,
    seeded: bool,
    /// The Timeline source rail as the shared adaptive drawer: docked beside the
    /// snapshot list on wide windows, a summoned overlay drawer when narrow.
    drawer: SidebarDrawer,
    /// `--shots` hook: force the overlay rail open so its narrow state can be
    /// captured headlessly (`QOS_BACKUPS_RAIL_OPEN=1`).
    force_rail_open: bool,
    /// Framework-owned action dispatch: every retention-policy edit goes through
    /// here so it brackets one undo boundary, encodes under [`BACKUPS_ACTION_TYPE`],
    /// and replays byte-identically on peers — the same path Ctrl+K takes.
    actions: ui::ActionHost,
}

impl Default for BackupsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl BackupsApp {
    /// The live app: starts on an empty catalog (real first-run state) and
    /// shows onboarding until the user configures a source + destination.
    pub fn new() -> Self {
        Self::with_catalog(Catalog::empty())
    }

    /// The demo app: pre-seeded with the sample catalog. Used only by the
    /// `--shots` renderer so screenshots exercise a populated dashboard —
    /// never the live launch.
    pub fn new_demo() -> Self {
        Self::with_catalog(Catalog::sample())
    }

    fn with_catalog(catalog: Catalog) -> Self {
        Self {
            catalog,
            ui: UiCache::default(),
            rect: Rect::ZERO,
            node: None,
            hits: Vec::new(),
            hover: None,
            last_mouse: Point::ZERO,
            timeline_scroll: 0.0,
            timeline_view: Rect::ZERO,
            timeline_content_h: 0.0,
            policy_focus: 0,
            file_sel: 0,
            job: None,
            toast: None,
            seeded: false,
            drawer: SidebarDrawer::new()
                .left()
                .docked_width(RAIL_W)
                .overlay_width(RAIL_W.max(288.0))
                .breakpoint(RAIL_BREAKPOINT),
            force_rail_open: std::env::var("QOS_BACKUPS_RAIL_OPEN").as_deref() == Ok("1"),
            actions: ui::ActionHost::new(),
        }
    }

    /// Adjust one retention field by `delta`, clamped to `[0, 999]`, through the
    /// action host. This is the single production funnel both the on-screen +/-
    /// buttons and the keyboard arrow / `+` / `-` handlers call, and it dispatches
    /// the byte-identical [`BackupsAction`](crate::actions::BackupsAction) the
    /// Ctrl+K increment/decrement spec exports.
    pub(crate) fn bump_retention(
        &mut self,
        doc: &mut AutoCommit,
        root: &ObjId,
        field: PolicyField,
        delta: i64,
    ) {
        let cur = BackupsDoc::retention(doc, root, field);
        self.actions
            .dispatch(doc, BACKUPS_ACTION_TYPE, &actions::bump_retention_action(field, cur, delta));
    }

    /// The last action dispatched, for the `affordance == ActionSpec` covenant
    /// test.
    #[cfg(test)]
    pub(crate) fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    fn load_cache(&mut self, doc: &AutoCommit, root: &ObjId) {
        self.ui = UiCache {
            view: BackupsDoc::view(doc, root),
            sel_source: BackupsDoc::selected_source(doc, root),
            sel_snapshot: BackupsDoc::selected_snapshot(doc, root),
            sel_dest: BackupsDoc::selected_dest(doc, root),
            restore_open: BackupsDoc::restore_open(doc, root),
            policy: BackupsDoc::policy(doc, root),
        };
    }

    fn toast_now(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    // ---- semantic colour mapping (all sourced from the Theme) -----------

    fn retention_color(theme: &Theme, r: Retention) -> Color {
        let c = &theme.tokens.colors;
        match r {
            Retention::Hourly => c.semantic.info,
            Retention::Daily => theme.accent_primary,
            Retention::Weekly => c.semantic.success,
            Retention::Monthly => c.semantic.warning,
            Retention::Keep => c.accent[8],
        }
    }

    fn health_color(theme: &Theme, h: Health) -> Color {
        let c = &theme.tokens.colors;
        match h {
            Health::Ok => c.semantic.success,
            Health::Warning => c.semantic.warning,
            Health::Error => c.semantic.danger,
        }
    }

    fn status_color(theme: &Theme, s: SnapStatus) -> Color {
        let c = &theme.tokens.colors;
        match s {
            SnapStatus::Complete => c.semantic.success,
            SnapStatus::Verifying => c.semantic.info,
            SnapStatus::Failed => c.semantic.danger,
        }
    }

    fn job_progress(&self) -> Option<f64> {
        self.job.as_ref().map(|j| {
            (j.started.elapsed().as_secs_f64() / j.dur.as_secs_f64()).clamp(0.0, 1.0)
        })
    }

    // ---- top-level draw -------------------------------------------------

    /// Fresh install: nothing configured yet, show onboarding not a dashboard.
    fn is_empty(&self) -> bool {
        self.catalog.sources.is_empty() && self.catalog.destinations.is_empty()
    }

    fn render(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.hits.clear();
        let c = &theme.tokens.colors;

        // Window background.
        paint::fill_rect(scene, t, self.rect, c.surfaces.base);

        let content = Rect::new(
            self.rect.x0 + PAD,
            self.rect.y0 + BAR_H + 14.0,
            self.rect.x1 - PAD,
            self.rect.y1 - STATUS_H - 12.0,
        );

        // Advance + resolve the Timeline source rail. Ticking eases the reveal
        // off a real wall-clock delta; resolving re-decides docked-vs-overlay
        // from the width so the top bar's hamburger reflects the live state.
        self.drawer.tick(false);
        let _ = self.drawer.resolve(content);
        // `--shots`: force the overlay drawer open *after* the first resolve
        // (which settles the eased reveal to the width default), then re-resolve.
        if self.force_rail_open && self.drawer.is_overlay() && !self.drawer.is_open() {
            self.drawer.open();
            self.drawer.snap(true);
            let _ = self.drawer.resolve(content);
        }

        self.draw_top_bar(scene, t, tr, theme);

        if self.is_empty() {
            // Fresh install: nothing to back up yet. Show onboarding instead of
            // an empty dashboard so the app reads as ready, not broken.
            self.draw_empty_onboarding(scene, t, tr, theme, content);
        } else {
            match self.ui.view {
                View::Timeline => self.draw_timeline_view(scene, t, tr, theme, content),
                View::Sources => self.draw_sources_view(scene, t, tr, theme, content),
                View::Destinations => self.draw_destinations_view(scene, t, tr, theme, content),
                View::Schedules => self.draw_schedules_view(scene, t, tr, theme, content),
            }
        }

        self.draw_status_bar(scene, t, tr, theme);

        // The Timeline source rail draws last so an overlay drawer floats over
        // the snapshot list (with a dismiss scrim); docked, it simply sits beside.
        if !self.is_empty() && self.ui.view == View::Timeline {
            let side = self.drawer.sidebar_rect();
            let scrim = self.drawer.scrim_rect();
            if scrim.width() > 0.5 {
                self.drawer.draw_scrim(scene, t);
                self.hits.push((scrim, Hit::CloseRail));
            }
            if side.width() > 4.0 {
                self.draw_rail(scene, t, tr, theme, side);
            }
        }

        if self.ui.restore_open {
            self.draw_restore_modal(scene, t, tr, theme);
        }
        self.draw_toast(scene, t, tr, theme);
    }

    fn draw_top_bar(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + BAR_H);
        paint::fill_rect(scene, t, bar, c.neutral[0]);
        paint::hline(scene, t, bar.x0, bar.x1, bar.y1, c.border_white_10, 1.0);

        let cy = bar.y0 + BAR_H / 2.0;
        let narrow = self.rect.width() < NARROW_BREAKPOINT;
        let compact = self.rect.width() < 1000.0;

        // Left cluster. A hamburger leads it whenever the Timeline rail isn't
        // fully docked-open (overlay, or a collapsed dock), so the source rail
        // is always reachable.
        let mut lx = bar.x0 + PAD;
        let show_hamburger = self.ui.view == View::Timeline
            && !self.is_empty()
            && !(self.drawer.is_docked() && self.drawer.is_open());
        if show_hamburger {
            let hb = Rect::new(lx, cy - 17.0, lx + 34.0, cy + 17.0);
            let hov = hb.contains(self.last_mouse);
            self.drawer.draw_toggle(scene, t, theme, hb, hov);
            self.hits.push((hb, Hit::ToggleRail));
            lx = hb.x1 + 12.0;
        }
        // Brand: shield + title (+ subtitle when there's room). The wordmark is
        // dropped entirely at narrow so the tabs + primary action fit.
        self.draw_shield(scene, t, Point::new(lx + 9.0, cy), 11.0, theme.accent_primary);
        lx += 20.0;
        if !narrow {
            paint::text_bold(scene, tr, t, "Backups", 18.0, c.text_primary, lx + 8.0, cy - 9.0);
            if !compact {
                let sub = format!(
                    "{} sources · {} destinations",
                    self.catalog.sources.len(),
                    self.catalog.destinations.len()
                );
                paint::text(scene, tr, t, &sub, 11.5, c.text_dim, lx + 8.0, cy + 9.0);
            }
            let (tw, _) = tr.measure("Backups", 18.0, None);
            lx += 8.0 + tw as f64;
        }
        let brand_right = lx;

        // Right: "Back up now" — icon-only at narrow so it never crowds the tabs.
        let bw = if narrow { 40.0 } else { 152.0 };
        let btn = Rect::new(bar.x1 - PAD - bw, cy - 18.0, bar.x1 - PAD, cy + 18.0);
        let running = self.job.is_some();
        let hovered = self.hover == Some(Hit::BackupNow);
        let fill = if running {
            c.surfaces.raised
        } else if hovered {
            theme.accent_hover
        } else {
            theme.accent_primary
        };
        paint::fill_rrect(scene, t, btn, 9.0, fill);
        if let Some(p) = self.job_progress() {
            // Progress sliver along the bottom of the button.
            let pr = Rect::new(btn.x0 + 3.0, btn.y1 - 6.0, btn.x0 + 3.0 + (btn.width() - 6.0) * p, btn.y1 - 3.0);
            paint::fill_rrect(scene, t, pr, 1.5, theme.accent_primary);
        }
        if narrow {
            // Just the "+" glyph, centred.
            let gc = btn.center();
            if !running {
                paint::poly(scene, t, &[(gc.x - 5.0, gc.y), (gc.x + 5.0, gc.y)], c.text_primary, 2.0, false);
                paint::poly(scene, t, &[(gc.x, gc.y - 5.0), (gc.x, gc.y + 5.0)], c.text_primary, 2.0, false);
            }
        } else {
            let label = if running { "Backing up…" } else { "Back up now" };
            let gx = btn.x0 + 16.0;
            if !running {
                paint::poly(scene, t, &[(gx - 5.0, cy), (gx + 5.0, cy)], c.text_primary, 2.0, false);
                paint::poly(scene, t, &[(gx, cy - 5.0), (gx, cy + 5.0)], c.text_primary, 2.0, false);
            }
            paint::text_center(scene, tr, t, label, 13.5, c.text_primary, btn.x0 + 16.0, btn.x1 - 6.0, cy - 7.0);
        }
        self.hits.push((btn, Hit::BackupNow));

        // Segmented tab control, centred in the gap between the brand and the
        // button, with the per-tab width clamped so four tabs always fit.
        let band_x0 = brand_right + 14.0;
        let band_x1 = btn.x0 - 14.0;
        let n = View::ALL.len() as f64;
        let tab_w = ((band_x1 - band_x0) / n).clamp(58.0, TAB_W);
        let seg_total = tab_w * n;
        let seg_x0 = (band_x0 + ((band_x1 - band_x0) - seg_total) / 2.0).max(band_x0);
        let seg = Rect::new(seg_x0, cy - TAB_H / 2.0, seg_x0 + seg_total, cy + TAB_H / 2.0);
        paint::fill_rrect(scene, t, seg, 9.0, c.surfaces.raised);
        paint::stroke_rrect(scene, t, seg, 9.0, c.border_white_10, 1.0);
        // The active tab floats as an inset pill inside the raised track; the
        // whole strip is one shared segmented control. Abbreviate the labels
        // once the tabs get too tight so "Destinations"/"Schedules" don't
        // collide with their neighbours — the abbreviation is content the app
        // owns; the pill fill + selection palette come from the shared control.
        let narrow_tabs = tab_w < 96.0;
        let fs = if narrow_tabs { 12.0_f32 } else { 13.5 };
        let mut pill_rects: Vec<Rect> = Vec::with_capacity(View::ALL.len());
        let mut labels: Vec<&str> = Vec::with_capacity(View::ALL.len());
        for (i, v) in View::ALL.iter().enumerate() {
            let tab = Rect::new(
                seg.x0 + i as f64 * tab_w,
                seg.y0,
                seg.x0 + (i as f64 + 1.0) * tab_w,
                seg.y1,
            );
            pill_rects.push(Rect::new(tab.x0 + 4.0, tab.y0 + 4.0, tab.x1 - 4.0, tab.y1 - 4.0));
            labels.push(if narrow_tabs {
                match v {
                    View::Timeline => "Timeline",
                    View::Sources => "Sources",
                    View::Destinations => "Dests",
                    View::Schedules => "Policy",
                }
            } else {
                v.title()
            });
            self.hits.push((tab, Hit::Tab(*v)));
        }
        let selected = View::ALL.iter().position(|v| self.ui.view == *v).unwrap_or(0);
        let hovered = View::ALL.iter().position(|v| self.hover == Some(Hit::Tab(*v)));
        ToggleGroup::paint(
            scene,
            t,
            tr,
            theme,
            &SegmentedFace::new(&pill_rects, &labels, selected)
                .pill()
                .hovered(hovered)
                .radius(7.0)
                .text_size(fs),
        );
    }

    fn draw_status_bar(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(self.rect.x0, self.rect.y1 - STATUS_H, self.rect.x1, self.rect.y1);
        paint::fill_rect(scene, t, bar, c.neutral[0]);
        paint::hline(scene, t, bar.x0, bar.x1, bar.y0, c.border_white_10, 1.0);
        let cy = bar.y0 + STATUS_H / 2.0;

        let failed = self.catalog.snapshots.iter().filter(|s| s.status == SnapStatus::Failed).count();
        let (dotc, msg) = if failed > 0 {
            (c.semantic.warning, format!("{failed} backup needs attention"))
        } else {
            (c.semantic.success, "All backups healthy".to_string())
        };
        paint::dot(scene, t, Point::new(bar.x0 + PAD + 4.0, cy), 4.0, dotc);
        paint::text(scene, tr, t, &msg, 12.0, c.text_dim, bar.x0 + PAD + 16.0, cy - 6.0);

        // The snapshot/size tally on the right; the centred wordmark is dropped
        // on tight windows so the three status texts never overlap.
        let right = format!(
            "{} snapshots · {} protected",
            self.catalog.total_snapshots(),
            ui::format::bytes(self.catalog.total_protected())
        );
        paint::text_right(scene, tr, t, &right, 12.0, c.text_dim, bar.x1 - PAD, cy - 6.0);
        if self.rect.width() >= 900.0 {
            paint::text_center(scene, tr, t, "qOS Backups 1.0", 11.0, c.text_dim.with_alpha_factor(0.7), bar.x0, bar.x1, cy - 5.5);
        }
    }

    // ---- EMPTY onboarding -----------------------------------------------

    /// First-run state: no sources or destinations configured. Renders a
    /// centered prompt so the fresh app reads as ready rather than as a broken
    /// blank dashboard.
    fn draw_empty_onboarding(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, content: Rect) {
        let c = &theme.tokens.colors;
        let cy = content.y0 + content.height() * 0.36;
        paint::text_center(
            scene, tr, t,
            "No backups configured yet",
            19.0, c.text_primary, content.x0, content.x1, cy,
        );
        paint::text_center(
            scene, tr, t,
            "Add a source to protect and a destination to store snapshots.",
            13.0, c.text_dim, content.x0, content.x1, cy + 30.0,
        );
    }

    // ---- TIMELINE view --------------------------------------------------

    fn draw_timeline_view(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, content: Rect) {
        // Main column: the drawer's resolved content rect (inset by the docked
        // rail, or full width in overlay). A small gap keeps it off a docked rail.
        let docked_gap = if self.drawer.is_docked() { 18.0 } else { 0.0 };
        let cr = self.drawer.content_rect();
        let main = Rect::new(cr.x0 + docked_gap, content.y0, content.x1, content.y1);

        // Summary stat tiles — reflow from four abreast down to a 2×2 grid so
        // they never crush at narrow widths.
        let cols = card_columns(main.width(), 156.0, 12.0).clamp(1, 4);
        let rows = 4usize.div_ceil(cols);
        let tile_h = 82.0;
        let tiles_h = rows as f64 * tile_h + (rows as f64 - 1.0) * 12.0;
        let tiles = Rect::new(main.x0, main.y0, main.x1, main.y0 + tiles_h);
        self.draw_summary_tiles(scene, t, tr, theme, tiles, cols);

        // Timeline header.
        let c = &theme.tokens.colors;
        let hy = tiles.y1 + 18.0;
        let scope = match &self.ui.sel_source {
            Some(id) => self.catalog.source(id).map(|s| s.path.clone()).unwrap_or_default(),
            None => "All sources".to_string(),
        };
        paint::text_bold(scene, tr, t, "Snapshot timeline", 15.0, c.text_primary, main.x0, hy);
        let (title_w, _) = tr.measure("Snapshot timeline", 15.0, None);
        paint::text(scene, tr, t, &scope, 12.5, theme.accent_primary, main.x0 + title_w as f64 + 14.0, hy + 1.0);
        paint::text_right(scene, tr, t, "newest first", 11.5, c.text_dim, main.x1, hy + 2.0);

        let list = Rect::new(main.x0, hy + 26.0, main.x1, main.y1);
        self.draw_timeline_list(scene, t, tr, theme, list);
    }

    fn draw_rail(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rail: Rect) {
        let c = &theme.tokens.colors;
        // Clip every rail pixel so a sliding overlay reveal never spills rows
        // over the snapshot list beside it.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &rail);
        // A translucent card when docked (matches the panel), a solid surface
        // when the drawer floats over content in overlay mode.
        if self.drawer.is_overlay() {
            paint::card(scene, t, rail, 14.0, c.surfaces.raised, c.border_white_20);
        } else {
            paint::card(scene, t, rail, 14.0, c.surfaces.raised.with_alpha_factor(0.45), c.border_white_10);
        }
        let pad = 14.0;
        let mut y = rail.y0 + 14.0;
        paint::text(scene, tr, t, "SOURCES", 10.5, c.text_dim, rail.x0 + pad, y);
        // Docked-collapse affordance: a small toggle at the header's right edge
        // when the rail is docked-open (the top-bar hamburger only reappears
        // once it's collapsed).
        if self.drawer.is_docked() && self.drawer.is_open() {
            let cb = Rect::new(rail.x1 - pad - 30.0, rail.y0 + 10.0, rail.x1 - pad, rail.y0 + 40.0);
            let hov = cb.contains(self.last_mouse);
            self.drawer.draw_toggle(scene, t, theme, cb, hov);
            self.hits.push((cb, Hit::ToggleRail));
        }
        y += 20.0;

        // "All sources" pseudo-row + each source.
        let all_sel = self.ui.sel_source.is_none();
        self.draw_source_row(scene, t, tr, theme, rail, &mut y, None, all_sel);
        for i in 0..self.catalog.sources.len() {
            let sel = self.ui.sel_source.as_deref() == Some(self.catalog.sources[i].id.as_str());
            let src = self.catalog.sources[i].clone();
            self.draw_source_row_real(scene, t, tr, theme, rail, &mut y, &src, sel);
        }

        y += 10.0;
        paint::hline(scene, t, rail.x0 + pad, rail.x1 - pad, y, c.border_white_10, 1.0);
        y += 14.0;
        paint::text(scene, tr, t, "DESTINATIONS", 10.5, c.text_dim, rail.x0 + pad, y);
        y += 22.0;
        for i in 0..self.catalog.destinations.len() {
            let d = self.catalog.destinations[i].clone();
            self.draw_dest_compact(scene, t, tr, theme, rail, &mut y, &d);
        }
        scene.pop_layer();
    }

    /// Shared row highlight (selected / hovered) used by the rail rows.
    fn row_bg(&self, scene: &mut Scene, t: Affine, theme: &Theme, row: Rect, selected: bool, hovered: bool) {
        let c = &theme.tokens.colors;
        if selected {
            paint::fill_rrect(scene, t, row, 9.0, theme.accent_primary.with_alpha_factor(0.18));
            paint::stroke_rrect(scene, t, row, 9.0, theme.accent_primary.with_alpha_factor(0.6), 1.0);
        } else if hovered {
            paint::fill_rrect(scene, t, row, 9.0, c.surface_white_5);
        }
    }

    /// The "All sources" aggregate row.
    fn draw_source_row(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rail: Rect, y: &mut f64, _src: Option<&Source>, sel: bool) {
        let c = &theme.tokens.colors;
        let h = 38.0;
        let row = Rect::new(rail.x0 + 8.0, *y, rail.x1 - 8.0, *y + h);
        self.row_bg(scene, t, theme, row, sel, self.hover == Some(Hit::Source(None)));
        let cy = row.y0 + h / 2.0;
        paint::text_bold(scene, tr, t, "All sources", 13.5, c.text_primary, row.x0 + 14.0, cy - 7.0);
        let count: usize = self.catalog.total_snapshots();
        paint::text_right(scene, tr, t, &format!("{count}"), 12.0, c.text_dim, row.x1 - 14.0, cy - 7.0);
        self.hits.push((row, Hit::Source(None)));
        *y += h + 6.0;
    }

    fn draw_source_row_real(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rail: Rect, y: &mut f64, src: &Source, sel: bool) {
        let c = &theme.tokens.colors;
        let h = 50.0;
        let row = Rect::new(rail.x0 + 8.0, *y, rail.x1 - 8.0, *y + h);
        self.row_bg(scene, t, theme, row, sel, self.hover == Some(Hit::Source(Some(src.id.clone()))));
        let ix = row.x0 + 22.0;
        let cy = row.y0 + h / 2.0;
        self.draw_source_icon(scene, t, src.kind, Point::new(ix, cy), 10.0, theme.accent_primary);
        paint::text(scene, tr, t, &src.path, 13.5, c.text_primary, ix + 18.0, row.y0 + 9.0);
        let count = self.catalog.count_for(&src.id);
        let sub = format!("{} · {} snapshots", src.cadence.label(), count);
        paint::text(scene, tr, t, &sub, 11.0, c.text_dim, ix + 18.0, row.y0 + 28.0);
        // Right column: protected size + health dot.
        paint::text_right(scene, tr, t, &ui::format::bytes(self.catalog.protected_bytes(&src.id)), 11.5, c.text_dim, row.x1 - 14.0, row.y0 + 9.0);
        paint::dot(scene, t, Point::new(row.x1 - 18.0, row.y0 + 33.0), 3.5, Self::health_color(theme, src.health));
        self.hits.push((row, Hit::Source(Some(src.id.clone()))));
        *y += h + 4.0;
    }

    fn draw_dest_compact(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rail: Rect, y: &mut f64, d: &Destination) {
        let c = &theme.tokens.colors;
        let h = 48.0;
        let row = Rect::new(rail.x0 + 8.0, *y, rail.x1 - 8.0, *y + h);
        if self.hover == Some(Hit::Dest(d.id.clone())) {
            paint::fill_rrect(scene, t, row, 9.0, c.surface_white_5);
        }
        let ix = row.x0 + 18.0;
        self.draw_dest_icon(scene, t, d.kind, Point::new(ix, row.y0 + 12.0), 8.0, Self::health_color(theme, d.health));
        paint::text(scene, tr, t, &d.name, 12.5, c.text_primary, ix + 16.0, row.y0 + 5.0);
        let frac = d.used_bytes as f64 / d.total_bytes as f64;
        let bar = Rect::new(ix, row.y0 + 26.0, row.x1 - 12.0, row.y0 + 32.0);
        let barcol = if frac > 0.85 { c.semantic.warning } else { theme.accent_primary };
        paint::bar(scene, t, bar, frac, barcol, c.neutral[2]);
        let usage = format!("{} / {}", ui::format::bytes(d.used_bytes), ui::format::bytes(d.total_bytes));
        paint::text(scene, tr, t, &usage, 10.5, c.text_dim, ix, row.y0 + 35.0);
        self.hits.push((row, Hit::Dest(d.id.clone())));
        *y += h + 2.0;
    }

    fn draw_summary_tiles(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, region: Rect, cols: usize) {
        let c = &theme.tokens.colors;
        let gap = 12.0;
        let cols = cols.max(1);
        let tw = card_column_width(region.width(), cols, gap);
        let rows = 4usize.div_ceil(cols);
        let th = ((region.height() - gap * (rows as f64 - 1.0)) / rows as f64).max(64.0);
        let next_run = self
            .ui
            .sel_source
            .as_ref()
            .and_then(|id| self.catalog.source(id))
            .map(|s| s.next_run.clone())
            .unwrap_or_else(|| "In 38 min".to_string());
        let failed = self.catalog.snapshots.iter().filter(|s| s.status == SnapStatus::Failed).count();
        let last = if failed == 0 { "OK · 2h ago".to_string() } else { "1 failed".to_string() };
        let tiles: [(&str, String, Color); 4] = [
            ("Protected", ui::format::bytes(self.catalog.total_protected()), theme.accent_primary),
            ("Snapshots", format!("{}", self.catalog.total_snapshots()), c.semantic.info),
            ("Next run", next_run, c.semantic.success),
            ("Last backup", last, if failed == 0 { c.semantic.success } else { c.semantic.warning }),
        ];
        for (i, (label, value, accent)) in tiles.iter().enumerate() {
            let col = i % cols;
            let rowi = i / cols;
            let x0 = region.x0 + col as f64 * (tw + gap);
            let y0 = region.y0 + rowi as f64 * (th + gap);
            let tile = Rect::new(x0, y0, x0 + tw, y0 + th);
            paint::card(scene, t, tile, 12.0, c.surfaces.raised, c.border_white_10);
            // accent ticker on the left edge
            let tick = Rect::new(tile.x0, tile.y0 + 10.0, tile.x0 + 3.0, tile.y1 - 10.0);
            paint::fill_rrect(scene, t, tick, 1.5, *accent);
            paint::text(scene, tr, t, label, 11.5, c.text_dim, tile.x0 + 16.0, tile.y0 + 14.0);
            let vsize = if value.len() > 11 { 17.0 } else { 22.0 };
            paint::text_bold(scene, tr, t, value, vsize, c.text_primary, tile.x0 + 16.0, tile.y0 + 36.0);
        }
    }

    fn draw_timeline_list(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, list: Rect) {
        let c = &theme.tokens.colors;
        self.timeline_view = list;
        // Group snapshots by day. Clone to owned so we don't hold an
        // immutable borrow of `self.catalog` across the `&mut self` row draws.
        let snaps: Vec<Snapshot> = self
            .catalog
            .timeline(self.ui.sel_source.as_deref())
            .into_iter()
            .cloned()
            .collect();
        let show_source = self.ui.sel_source.is_none();

        // First measure total content height (so we can clamp scroll).
        let mut content_h = 0.0;
        let mut last_day: Option<u32> = None;
        for s in &snaps {
            if last_day != Some(s.day) {
                content_h += DAY_H;
                last_day = Some(s.day);
            }
            content_h += ROW_H + 4.0;
            if self.ui.sel_snapshot.as_deref() == Some(s.id.as_str()) {
                content_h += DETAIL_H;
            }
        }
        self.timeline_content_h = content_h;
        let max_scroll = (content_h - list.height()).max(0.0);
        self.timeline_scroll = self.timeline_scroll.clamp(0.0, max_scroll);

        // Clip to the list region and draw with the scroll offset.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &list);
        let mut y = list.y0 - self.timeline_scroll;
        last_day = None;
        for s in &snaps {
            if last_day != Some(s.day) {
                // Day header.
                if y + DAY_H > list.y0 && y < list.y1 {
                    paint::text_bold(scene, tr, t, &Catalog::day_label(s.day), 12.0, c.text_dim, list.x0 + 2.0, y + 8.0);
                    let label_w = 96.0;
                    paint::hline(scene, t, list.x0 + label_w, list.x1, y + 14.0, c.border_white_10, 1.0);
                }
                y += DAY_H;
                last_day = Some(s.day);
            }
            let selected = self.ui.sel_snapshot.as_deref() == Some(s.id.as_str());
            let row = Rect::new(list.x0, y, list.x1, y + ROW_H);
            let visible = row.y1 > list.y0 && row.y0 < list.y1;
            if visible {
                self.draw_snapshot_row(scene, t, tr, theme, row, s, selected, show_source);
                self.hits.push((row, Hit::Snap(s.id.clone())));
            }
            y += ROW_H + 4.0;
            if selected {
                let det = Rect::new(list.x0, y - 2.0, list.x1, y - 2.0 + DETAIL_H);
                if det.y1 > list.y0 && det.y0 < list.y1 {
                    self.draw_snapshot_detail(scene, t, tr, theme, det, s);
                }
                y += DETAIL_H;
            }
        }
        scene.pop_layer();

        // Scrollbar track indicator.
        if max_scroll > 0.0 {
            let track_h = list.height();
            let thumb_h = (track_h * (track_h / content_h)).max(28.0);
            let thumb_y = list.y0 + (track_h - thumb_h) * (self.timeline_scroll / max_scroll);
            let thumb = Rect::new(list.x1 - 4.0, thumb_y, list.x1 - 1.0, thumb_y + thumb_h);
            paint::fill_rrect(scene, t, thumb, 2.0, c.neutral[3]);
        }
    }

    fn draw_snapshot_row(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, row: Rect, s: &Snapshot, selected: bool, show_source: bool) {
        let c = &theme.tokens.colors;
        let hovered = self.hover == Some(Hit::Snap(s.id.clone()));
        if selected {
            paint::fill_rrect(scene, t, row, 10.0, theme.accent_primary.with_alpha_factor(0.16));
            paint::stroke_rrect(scene, t, row, 10.0, theme.accent_primary.with_alpha_factor(0.6), 1.0);
        } else if hovered {
            paint::fill_rrect(scene, t, row, 10.0, c.surface_white_5);
        } else {
            paint::fill_rrect(scene, t, row, 10.0, c.surfaces.raised.with_alpha_factor(0.55));
        }
        let cy = row.y0 + ROW_H / 2.0;
        // Status dot.
        paint::dot(scene, t, Point::new(row.x0 + 16.0, cy), 4.0, Self::status_color(theme, s.status));
        // Time.
        paint::text_bold(scene, tr, t, &s.time, 14.0, c.text_primary, row.x0 + 30.0, cy - 8.0);
        // Type + retention pills.
        let mut px = row.x0 + 92.0;
        let (tfg, tbg) = match s.kind {
            SnapType::Manual => (theme.accent_primary, theme.accent_primary.with_alpha_factor(0.18)),
            SnapType::Auto => (c.text_dim, c.surface_white_10),
        };
        px += paint::pill(scene, tr, t, px, cy, s.kind.label(), 11.0, tfg, tbg) + 6.0;
        let rc = Self::retention_color(theme, s.retention);
        px += paint::tag_pill(scene, tr, t, px, cy, s.retention.label(), 11.0, rc) + 8.0;
        // Source path (only in "all" mode).
        if show_source {
            if let Some(src) = self.catalog.source(&s.source_id) {
                paint::text(scene, tr, t, &src.path, 12.0, c.text_dim, px, cy - 7.0);
            }
        }
        // Right side: files changed, delta, total.
        let total_x = row.x1 - 16.0;
        paint::text_right(scene, tr, t, &ui::format::bytes(s.size_bytes), 13.0, c.text_primary, total_x, cy - 8.0);
        let meta = format!("+{} · {} files", ui::format::bytes(s.delta_bytes), s.files_changed);
        paint::text_right(scene, tr, t, &meta, 10.5, c.text_dim, total_x, cy + 7.0);
    }

    fn draw_snapshot_detail(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, det: Rect, s: &Snapshot) {
        let c = &theme.tokens.colors;
        paint::fill_rrect(scene, t, det, 10.0, c.neutral[0].with_alpha_factor(0.5));
        let dest = self.catalog.destination(&s.dest_id);
        let dest_name = dest.map(|d| d.name.clone()).unwrap_or_default();
        let dest_kind = dest.map(|d| d.kind.label()).unwrap_or("");
        paint::text(scene, tr, t, &format!("Stored on {dest_name}  ·  {dest_kind}"), 12.0, c.text_dim, det.x0 + 16.0, det.y0 + 12.0);
        let enc = if dest.map(|d| d.encrypted).unwrap_or(false) { "encrypted" } else { "plaintext" };
        paint::text(scene, tr, t, &format!("{} files · {} changed · {}", s.files_changed, ui::format::bytes(s.delta_bytes), enc), 11.5, c.text_dim, det.x0 + 16.0, det.y0 + 33.0);

        // Action buttons, right-aligned.
        let btns: [(&str, Hit, Color); 3] = [
            ("Restore…", Hit::Restore, theme.accent_primary),
            ("Verify", Hit::Verify, c.surfaces.raised),
            ("Delete", Hit::Delete, c.semantic.danger.with_alpha_factor(0.18)),
        ];
        let mut bx = det.x1 - 12.0;
        let bh = 32.0;
        let by = det.y0 + (det.height() - bh) / 2.0;
        for (label, hit, fill) in btns.iter().rev() {
            let (tw, _) = tr.measure(label, 12.5, None);
            let bw = tw as f64 + 28.0;
            let b = Rect::new(bx - bw, by, bx, by + bh);
            let hovered = self.hover.as_ref() == Some(hit);
            let f = if hovered { fill.with_alpha_factor(0.85) } else { *fill };
            paint::fill_rrect(scene, t, b, 8.0, f);
            if *hit != Hit::Restore {
                paint::stroke_rrect(scene, t, b, 8.0, c.border_white_10, 1.0);
            }
            let fg = if *hit == Hit::Delete { c.semantic.danger } else { c.text_primary };
            paint::text_center(scene, tr, t, label, 12.5, fg, b.x0, b.x1, by + (bh - 12.5) / 2.0);
            self.hits.push((b, hit.clone()));
            bx -= bw + 8.0;
        }
    }

    // ---- SOURCES view ---------------------------------------------------

    fn draw_sources_view(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, content: Rect) {
        let c = &theme.tokens.colors;
        paint::text_bold(scene, tr, t, "Backup sources", 16.0, c.text_primary, content.x0, content.y0);
        paint::text(scene, tr, t, "Folders captured on a schedule into your destinations.", 12.5, c.text_dim, content.x0, content.y0 + 24.0);

        // At narrow widths the three inline stat columns collapse to one line
        // beneath the path, so nothing is crushed against the toggle.
        let narrow = content.width() < 620.0;
        let mut y = content.y0 + 50.0;
        let card_h = if narrow { 104.0 } else { 96.0 };
        for i in 0..self.catalog.sources.len() {
            let src = self.catalog.sources[i].clone();
            let card = Rect::new(content.x0, y, content.x1, y + card_h);
            let sel = self.ui.sel_source.as_deref() == Some(src.id.as_str());
            let border = if sel { theme.accent_primary.with_alpha_factor(0.6) } else { c.border_white_10 };
            paint::card(scene, t, card, 14.0, c.surfaces.raised, border);
            let ix = card.x0 + 34.0;
            self.draw_source_icon(scene, t, src.kind, Point::new(ix, card.y0 + 30.0), 13.0, theme.accent_primary);
            paint::text_bold(scene, tr, t, &src.path, 17.0, c.text_primary, ix + 28.0, card.y0 + 16.0);
            let kind = match src.kind {
                SourceKind::Home => "Home directory",
                SourceKind::System => "System configuration",
                SourceKind::Workspace => "Collaborative workspace",
                SourceKind::Custom => "Custom path",
            };
            paint::text(scene, tr, t, kind, 12.0, c.text_dim, ix + 28.0, card.y0 + 40.0);

            if narrow {
                // Single compact stats line.
                let stats = format!(
                    "{} · {} snapshots · {}",
                    src.cadence.label(),
                    self.catalog.count_for(&src.id),
                    ui::format::bytes(self.catalog.protected_bytes(&src.id)),
                );
                paint::text(scene, tr, t, &stats, 12.0, c.text_dim, ix + 28.0, card.y0 + 60.0);
            } else {
                // Stat columns.
                let stat = |sc: &mut Scene, tr: &mut TextRenderer, x: f64, label: &str, value: &str, col: Color| {
                    paint::text(sc, tr, t, label, 10.5, c.text_dim, x, card.y0 + 16.0);
                    paint::text_bold(sc, tr, t, value, 15.0, col, x, card.y0 + 32.0);
                };
                let col0 = card.x0 + card.width() * 0.46;
                let col1 = card.x0 + card.width() * 0.62;
                let col2 = card.x0 + card.width() * 0.78;
                stat(scene, tr, col0, "CADENCE", src.cadence.label(), c.text_primary);
                stat(scene, tr, col1, "SNAPSHOTS", &format!("{}", self.catalog.count_for(&src.id)), c.semantic.info);
                stat(scene, tr, col2, "PROTECTED", &ui::format::bytes(self.catalog.protected_bytes(&src.id)), theme.accent_primary);
            }

            // Health + next-run + enable toggle.
            paint::dot(scene, t, Point::new(card.x0 + 16.0, card.y0 + card_h - 18.0), 3.5, Self::health_color(theme, src.health));
            paint::text(scene, tr, t, &format!("Next run {}", src.next_run), 11.5, c.text_dim, card.x0 + 28.0, card.y0 + card_h - 24.0);

            // Enable switch (shared Toggle look).
            let tw = 52.0;
            let tog = Rect::new(card.x1 - 22.0 - tw, card.y0 + card_h - 30.0, card.x1 - 22.0, card.y0 + card_h - 30.0 + 22.0);
            let on = src.enabled;
            Toggle::paint(scene, t, theme, &ToggleFace::new(tog, on));
            paint::text_right(scene, tr, t, if on { "Enabled" } else { "Paused" }, 10.5, c.text_dim, tog.x0 - 8.0, tog.center().y - 5.0);

            self.hits.push((card, Hit::Source(Some(src.id.clone()))));
            y += card_h + 12.0;
        }

        // Recent activity feed fills the remaining space.
        let region = Rect::new(content.x0, y + 10.0, content.x1, content.y1);
        if region.height() > 80.0 {
            self.draw_recent_activity(scene, t, tr, theme, region);
        }
    }

    fn draw_recent_activity(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, region: Rect) {
        let c = &theme.tokens.colors;
        paint::card(scene, t, region, 14.0, c.surfaces.raised.with_alpha_factor(0.45), c.border_white_10);
        paint::text_bold(scene, tr, t, "Recent activity", 13.5, c.text_primary, region.x0 + 18.0, region.y0 + 14.0);
        paint::text_right(scene, tr, t, "last 7 events", 11.0, c.text_dim, region.x1 - 18.0, region.y0 + 16.0);

        // Below this the fixed activity columns won't fit; the row folds to two
        // lines (when + size on top, the action beneath).
        let narrow = region.width() < 660.0;
        let events: Vec<Snapshot> = self.catalog.timeline(None).into_iter().take(7).cloned().collect();
        let row_h = if narrow { 46.0 } else { 38.0 };
        let y0 = region.y0 + 44.0;
        for (i, s) in events.iter().enumerate() {
            let ry = y0 + i as f64 * row_h;
            if ry + row_h > region.y1 - 8.0 {
                break;
            }
            let path = self.catalog.source(&s.source_id).map(|x| x.path.clone()).unwrap_or_default();
            let action = format!("{} snapshot of {}", s.kind.label(), path);
            let when = format!("{} {}", Catalog::day_label(s.day), s.time);
            if narrow {
                paint::dot(scene, t, Point::new(region.x0 + 22.0, ry + 12.0), 4.0, Self::status_color(theme, s.status));
                paint::text(scene, tr, t, &when, 12.5, c.text_primary, region.x0 + 36.0, ry + 4.0);
                paint::text_right(scene, tr, t, &ui::format::bytes(s.delta_bytes), 12.0, c.text_dim, region.x1 - 18.0, ry + 4.0);
                paint::text(scene, tr, t, &action, 11.5, c.text_dim, region.x0 + 36.0, ry + 24.0);
            } else {
                let cy = ry + row_h / 2.0;
                paint::dot(scene, t, Point::new(region.x0 + 22.0, cy), 4.0, Self::status_color(theme, s.status));
                paint::text(scene, tr, t, &when, 12.5, c.text_primary, region.x0 + 36.0, cy - 7.0);
                paint::text(scene, tr, t, &action, 12.0, c.text_dim, region.x0 + 168.0, cy - 7.0);
                let dest = self.catalog.destination(&s.dest_id).map(|d| d.name.clone()).unwrap_or_default();
                paint::text(scene, tr, t, &format!("→ {dest}"), 11.5, Self::dest_color(theme, &s.dest_id), region.x0 + 440.0, cy - 6.0);
                let status = match s.status {
                    SnapStatus::Complete => "completed",
                    SnapStatus::Verifying => "verifying",
                    SnapStatus::Failed => "failed",
                };
                paint::text_right(scene, tr, t, status, 11.5, Self::status_color(theme, s.status), region.x1 - 110.0, cy - 6.0);
                paint::text_right(scene, tr, t, &ui::format::bytes(s.delta_bytes), 12.0, c.text_dim, region.x1 - 18.0, cy - 7.0);
            }
            if i + 1 < events.len() && ry + row_h * 2.0 <= region.y1 - 8.0 {
                paint::hline(scene, t, region.x0 + 18.0, region.x1 - 18.0, ry + row_h, c.border_white_10, 1.0);
            }
        }
    }

    // ---- DESTINATIONS view ---------------------------------------------

    fn draw_destinations_view(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, content: Rect) {
        let c = &theme.tokens.colors;
        paint::text_bold(scene, tr, t, "Destinations", 16.0, c.text_primary, content.x0, content.y0);
        paint::text(scene, tr, t, "Where snapshots are stored. Capacity is shown per repository.", 12.5, c.text_dim, content.x0, content.y0 + 24.0);

        // Reflow the repository cards into an aligned grid — tall gauge cards
        // when several fit abreast, compact horizontal cards in a single column
        // when the window is too narrow for the ring layout.
        let n = self.catalog.destinations.len();
        let gap = 16.0;
        let cols = card_columns(content.width(), 240.0, gap).clamp(1, n.max(1));
        let cw = card_column_width(content.width(), cols, gap);
        let top = content.y0 + 52.0;
        let compact = cols == 1;
        let ch = if compact { 116.0 } else { 250.0 };
        let row_gap = 16.0;
        let mut rows_used = 1usize;
        for i in 0..n {
            let d = self.catalog.destinations[i].clone();
            let col = i % cols;
            let row = i / cols;
            rows_used = row + 1;
            let x0 = content.x0 + col as f64 * (cw + gap);
            let y0 = top + row as f64 * (ch + row_gap);
            let card = Rect::new(x0, y0, x0 + cw, y0 + ch);
            let sel = self.ui.sel_dest.as_deref() == Some(d.id.as_str());
            if compact {
                self.draw_dest_card_compact(scene, t, tr, theme, card, &d, sel);
            } else {
                self.draw_dest_card_tall(scene, t, tr, theme, card, &d, sel);
            }
            self.hits.push((card, Hit::Dest(d.id.clone())));
        }

        // Aggregate bar across the bottom.
        let used: u64 = self.catalog.destinations.iter().map(|d| d.used_bytes).sum();
        let total: u64 = self.catalog.destinations.iter().map(|d| d.total_bytes).sum();
        let ay = top + rows_used as f64 * (ch + row_gap) + 10.0;
        paint::text_bold(scene, tr, t, "Total capacity", 13.5, c.text_primary, content.x0, ay);
        let bar = Rect::new(content.x0, ay + 24.0, content.x1, ay + 38.0);
        paint::bar(scene, t, bar, used as f64 / total as f64, theme.accent_primary, c.neutral[2]);
        paint::text(scene, tr, t, &format!("{} used of {} across {} destinations", ui::format::bytes(used), ui::format::bytes(total), self.catalog.destinations.len()), 11.5, c.text_dim, content.x0, ay + 44.0);

        // Distribution: where the snapshots actually live.
        self.draw_dest_distribution(scene, t, tr, theme, content, ay + 76.0);
    }

    fn dest_color(theme: &Theme, id: &str) -> Color {
        let c = &theme.tokens.colors;
        match id {
            "pool" => theme.accent_primary,
            "usb" => c.semantic.info,
            "b2" => c.semantic.success,
            _ => c.neutral[5],
        }
    }

    /// The full repository card: a big capacity ring plus used/free figures.
    #[allow(clippy::too_many_arguments)]
    fn draw_dest_card_tall(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, card: Rect, d: &Destination, sel: bool) {
        let c = &theme.tokens.colors;
        let border = if sel { theme.accent_primary.with_alpha_factor(0.6) } else { c.border_white_10 };
        paint::card(scene, t, card, 16.0, c.surfaces.raised, border);

        self.draw_dest_icon(scene, t, d.kind, Point::new(card.x0 + 24.0, card.y0 + 26.0), 9.0, Self::health_color(theme, d.health));
        paint::text_bold(scene, tr, t, &d.name, 16.0, c.text_primary, card.x0 + 40.0, card.y0 + 18.0);
        paint::text(scene, tr, t, d.kind.label(), 11.5, c.text_dim, card.x0 + 24.0, card.y0 + 44.0);

        // Capacity ring.
        let frac = d.used_bytes as f64 / d.total_bytes as f64;
        let center = Point::new(card.center().x, card.y0 + 120.0);
        let ringcol = if frac > 0.85 { c.semantic.warning } else { theme.accent_primary };
        paint::ring(scene, t, center, 44.0, 11.0, frac, ringcol, c.neutral[2]);
        paint::text_center(scene, tr, t, &format!("{:.0}%", frac * 100.0), 20.0, c.text_primary, center.x - 40.0, center.x + 40.0, center.y - 16.0);
        paint::text_center(scene, tr, t, "used", 10.5, c.text_dim, center.x - 40.0, center.x + 40.0, center.y + 8.0);

        // Used / free figures.
        let fy = card.y0 + 184.0;
        paint::text(scene, tr, t, "Used", 11.0, c.text_dim, card.x0 + 24.0, fy);
        paint::text_right(scene, tr, t, &ui::format::bytes(d.used_bytes), 12.5, c.text_primary, card.x1 - 24.0, fy);
        paint::text(scene, tr, t, "Free", 11.0, c.text_dim, card.x0 + 24.0, fy + 18.0);
        paint::text_right(scene, tr, t, &ui::format::bytes(d.total_bytes - d.used_bytes), 12.5, c.semantic.success, card.x1 - 24.0, fy + 18.0);

        // Footer chips: encryption + last sync.
        let mut chx = card.x0 + 24.0;
        if d.encrypted {
            chx += paint::tag_pill(scene, tr, t, chx, card.y1 - 20.0, "encrypted", 10.0, c.semantic.success) + 6.0;
        }
        paint::tag_pill(scene, tr, t, chx, card.y1 - 20.0, &format!("sync {}", d.last_sync), 10.0, c.semantic.info);
    }

    /// The compact repository card for a single-column (narrow) layout: name +
    /// percent on top, a capacity bar and figures beneath — no crushed ring.
    #[allow(clippy::too_many_arguments)]
    fn draw_dest_card_compact(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, card: Rect, d: &Destination, sel: bool) {
        let c = &theme.tokens.colors;
        let border = if sel { theme.accent_primary.with_alpha_factor(0.6) } else { c.border_white_10 };
        paint::card(scene, t, card, 14.0, c.surfaces.raised, border);

        self.draw_dest_icon(scene, t, d.kind, Point::new(card.x0 + 22.0, card.y0 + 24.0), 9.0, Self::health_color(theme, d.health));
        paint::text_bold(scene, tr, t, &d.name, 15.0, c.text_primary, card.x0 + 40.0, card.y0 + 14.0);
        paint::text(scene, tr, t, d.kind.label(), 11.0, c.text_dim, card.x0 + 40.0, card.y0 + 34.0);

        let frac = d.used_bytes as f64 / d.total_bytes as f64;
        let pctcol = if frac > 0.85 { c.semantic.warning } else { theme.accent_primary };
        paint::text_right(scene, tr, t, &format!("{:.0}%", frac * 100.0), 20.0, pctcol, card.x1 - 20.0, card.y0 + 18.0);

        // Capacity bar + used/free figures.
        let bar = Rect::new(card.x0 + 22.0, card.y0 + 60.0, card.x1 - 22.0, card.y0 + 70.0);
        let barcol = if frac > 0.85 { c.semantic.warning } else { theme.accent_primary };
        paint::bar(scene, t, bar, frac, barcol, c.neutral[2]);
        paint::text(scene, tr, t, &format!("{} of {}", ui::format::bytes(d.used_bytes), ui::format::bytes(d.total_bytes)), 11.5, c.text_dim, card.x0 + 22.0, card.y0 + 80.0);
        let mut chx = card.x1 - 22.0;
        // Trailing chips, laid right-to-left.
        let sync = format!("sync {}", d.last_sync);
        let (sw, _) = tr.measure(&sync, 10.0, None);
        chx -= sw as f64 + 20.0;
        paint::tag_pill(scene, tr, t, chx, card.y0 + 86.0, &sync, 10.0, c.semantic.info);
        if d.encrypted {
            let (ew, _) = tr.measure("encrypted", 10.0, None);
            chx -= ew as f64 + 26.0;
            paint::tag_pill(scene, tr, t, chx, card.y0 + 86.0, "encrypted", 10.0, c.semantic.success);
        }
    }

    fn draw_dest_distribution(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, content: Rect, y0: f64) {
        let c = &theme.tokens.colors;
        paint::text_bold(scene, tr, t, "Snapshots stored per destination", 13.5, c.text_primary, content.x0, y0);
        let counts: Vec<(String, String, usize)> = self
            .catalog
            .destinations
            .iter()
            .map(|d| {
                let n = self.catalog.snapshots.iter().filter(|s| s.dest_id == d.id).count();
                (d.id.clone(), d.name.clone(), n)
            })
            .collect();
        let total: usize = counts.iter().map(|(_, _, n)| *n).sum::<usize>().max(1);

        // Stacked proportional bar.
        let bar = Rect::new(content.x0, y0 + 22.0, content.x1, y0 + 40.0);
        paint::fill_rrect(scene, t, bar, 9.0, c.neutral[2]);
        let mut x = bar.x0;
        for (id, _, n) in &counts {
            let w = bar.width() * (*n as f64 / total as f64);
            if w > 1.0 {
                let seg = Rect::new(x, bar.y0, (x + w).min(bar.x1), bar.y1);
                paint::fill_rrect(scene, t, seg, 9.0, Self::dest_color(theme, id));
            }
            x += w;
        }

        // Legend — wraps onto further rows so it never runs off the right edge.
        let mut lx = content.x0;
        let mut ly = y0 + 56.0;
        for (id, name, n) in &counts {
            let label = format!("{name}  ·  {n} snapshots");
            let (w, _) = tr.measure(&label, 12.0, None);
            let item_w = 16.0 + w as f64 + 28.0;
            if lx + item_w > content.x1 && lx > content.x0 + 0.5 {
                lx = content.x0;
                ly += 24.0;
            }
            paint::dot(scene, t, Point::new(lx + 5.0, ly + 6.0), 5.0, Self::dest_color(theme, id));
            paint::text(scene, tr, t, &label, 12.0, c.text_dim, lx + 16.0, ly);
            lx += item_w + 20.0;
        }
    }

    // ---- SCHEDULES view -------------------------------------------------

    fn draw_schedules_view(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, content: Rect) {
        let c = &theme.tokens.colors;
        // Below this the editor + summary stack vertically instead of a 60/40 split.
        let stacked = content.width() < 660.0;
        paint::text_bold(scene, tr, t, "Retention policy", 16.0, c.text_primary, content.x0, content.y0);
        let desc = if stacked {
            "Snapshots kept per class before pruning."
        } else {
            "How many snapshots to keep in each class before pruning. Applies to every source."
        };
        paint::text(scene, tr, t, desc, 12.5, c.text_dim, content.x0, content.y0 + 24.0);

        let panel_w = if stacked { content.width() } else { content.width() * 0.6 };
        let panel = Rect::new(content.x0, content.y0 + 52.0, content.x0 + panel_w, content.y0 + 52.0 + 4.0 * 64.0 + 16.0);
        paint::card(scene, t, panel, 16.0, c.surfaces.raised, c.border_white_10);

        let p = self.ui.policy;
        let vals = [p.hourly, p.daily, p.weekly, p.monthly];
        for (i, field) in PolicyField::ALL.iter().enumerate() {
            let ry = panel.y0 + 14.0 + i as f64 * 64.0;
            let row = Rect::new(panel.x0 + 12.0, ry, panel.x1 - 12.0, ry + 56.0);
            let focused = self.policy_focus == i;
            if focused {
                paint::fill_rrect(scene, t, row, 10.0, theme.accent_primary.with_alpha_factor(0.12));
                paint::stroke_rrect(scene, t, row, 10.0, theme.accent_primary.with_alpha_factor(0.5), 1.0);
            }
            let rc = Self::retention_color(theme, match field {
                PolicyField::Hourly => Retention::Hourly,
                PolicyField::Daily => Retention::Daily,
                PolicyField::Weekly => Retention::Weekly,
                PolicyField::Monthly => Retention::Monthly,
            });
            paint::dot(scene, t, Point::new(row.x0 + 18.0, row.center().y), 5.0, rc);
            paint::text_bold(scene, tr, t, field.label(), 14.5, c.text_primary, row.x0 + 34.0, row.y0 + 12.0);
            paint::text(scene, tr, t, &format!("keep over the {}", field.unit()), 11.0, c.text_dim, row.x0 + 34.0, row.y0 + 32.0);

            // Stepper: [-]  value  [+]
            let cy = row.center().y;
            let plus = Rect::new(row.x1 - 44.0, cy - 18.0, row.x1 - 8.0, cy + 18.0);
            let valx0 = plus.x0 - 64.0;
            let minus = Rect::new(valx0 - 44.0, cy - 18.0, valx0 - 8.0, cy + 18.0);
            self.draw_stepper(scene, t, tr, theme, minus, "–", Hit::Policy(*field, -1));
            self.draw_stepper(scene, t, tr, theme, plus, "+", Hit::Policy(*field, 1));
            paint::text_center(scene, tr, t, &format!("{}", vals[i]), 20.0, c.text_primary, valx0 - 8.0, plus.x0, cy - 12.0);
        }

        // Summary of effective retention — beside the editor when wide, stacked
        // beneath it when narrow.
        let side = if stacked {
            Rect::new(content.x0, panel.y1 + 16.0, content.x1, panel.y1 + 16.0 + 210.0)
        } else {
            Rect::new(panel.x1 + 18.0, content.y0 + 52.0, content.x1, panel.y1)
        };
        paint::card(scene, t, side, 16.0, c.surfaces.raised.with_alpha_factor(0.5), c.border_white_10);
        paint::text_bold(scene, tr, t, "Effective window", 13.5, c.text_primary, side.x0 + 16.0, side.y0 + 14.0);
        let total = p.hourly + p.daily + p.weekly + p.monthly;
        let lines = [
            format!("{} hourly  →  last {} hours", p.hourly, p.hourly),
            format!("{} daily   →  last {} days", p.daily, p.daily),
            format!("{} weekly  →  last {} weeks", p.weekly, p.weekly),
            format!("{} monthly →  last {} months", p.monthly, p.monthly),
        ];
        for (i, l) in lines.iter().enumerate() {
            paint::text(scene, tr, t, l, 12.0, c.text_dim, side.x0 + 16.0, side.y0 + 44.0 + i as f64 * 24.0);
        }
        paint::hline(scene, t, side.x0 + 16.0, side.x1 - 16.0, side.y0 + 150.0, c.border_white_10, 1.0);
        paint::text(scene, tr, t, "Snapshots retained", 11.5, c.text_dim, side.x0 + 16.0, side.y0 + 162.0);
        paint::text_bold(scene, tr, t, &format!("up to {total}"), 20.0, theme.accent_primary, side.x0 + 16.0, side.y0 + 180.0);

        // Retention ruler — one lane per class, dots = snapshots kept. Sits
        // below whichever of {editor, stacked summary} is lowest.
        let ruler_top = if stacked { side.y1 } else { panel.y1 } + 22.0;
        let ruler = Rect::new(content.x0, ruler_top, content.x1, ruler_top + 200.0);
        if ruler.y1 < content.y1 {
            self.draw_retention_ruler(scene, t, tr, theme, ruler, p);
        }
    }

    fn draw_retention_ruler(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, region: Rect, p: RetentionPolicy) {
        let c = &theme.tokens.colors;
        paint::card(scene, t, region, 16.0, c.surfaces.raised.with_alpha_factor(0.4), c.border_white_10);
        paint::text_bold(scene, tr, t, "Retention at a glance", 13.5, c.text_primary, region.x0 + 18.0, region.y0 + 14.0);
        paint::text(scene, tr, t, "Each dot is a snapshot the policy keeps; older ones are pruned to make room.", 11.5, c.text_dim, region.x0 + 18.0, region.y0 + 34.0);

        let lanes: [(Retention, &str, u32); 4] = [
            (Retention::Hourly, "Hourly", p.hourly),
            (Retention::Daily, "Daily", p.daily),
            (Retention::Weekly, "Weekly", p.weekly),
            (Retention::Monthly, "Monthly", p.monthly),
        ];
        let lane_h = 32.0;
        let top = region.y0 + 60.0;
        let dots_x0 = region.x0 + 110.0;
        let dots_x1 = region.x1 - 90.0;
        for (i, (ret, label, count)) in lanes.iter().enumerate() {
            let ly = top + i as f64 * lane_h;
            let cy = ly + lane_h / 2.0;
            let col = Self::retention_color(theme, *ret);
            paint::text(scene, tr, t, label, 12.5, c.text_primary, region.x0 + 18.0, cy - 7.0);
            // Lay out `count` dots across the lane, capped so they always fit.
            let n = (*count).max(1);
            let span = dots_x1 - dots_x0;
            let step = (span / n as f64).min(20.0);
            let r = (step * 0.32).clamp(2.5, 5.0);
            for k in 0..n {
                let dx = dots_x0 + r + k as f64 * step;
                if dx > dots_x1 {
                    break;
                }
                paint::dot(scene, t, Point::new(dx, cy), r, col.with_alpha_factor(if k == 0 { 1.0 } else { 0.55 }));
            }
            paint::text_right(scene, tr, t, &format!("keep {count}"), 12.0, col, region.x1 - 18.0, cy - 7.0);
        }
    }

    fn draw_stepper(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, b: Rect, glyph: &str, hit: Hit) {
        let c = &theme.tokens.colors;
        let hovered = self.hover.as_ref() == Some(&hit);
        let fill = if hovered { theme.accent_primary } else { c.neutral[2] };
        paint::fill_rrect(scene, t, b, 8.0, fill);
        paint::stroke_rrect(scene, t, b, 8.0, c.border_white_10, 1.0);
        paint::text_center(scene, tr, t, glyph, 18.0, c.text_primary, b.x0, b.x1, b.center().y - 11.0);
        self.hits.push((b, hit));
    }

    // ---- restore modal --------------------------------------------------

    fn draw_restore_modal(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        // Scrim.
        paint::fill_rect(scene, t, self.rect, c.surfaces.overlay);
        let snap = self.ui.sel_snapshot.as_ref().and_then(|id| self.catalog.snapshot(id)).cloned();
        let Some(snap) = snap else { return };
        let src = self.catalog.source(&snap.source_id);
        // Clamp the modal to the window so it never overflows a small screen.
        // When it's too narrow for two columns it stacks, and needs more height
        // so the options don't crowd the footer buttons.
        let mw = 760.0_f64.min(self.rect.width() - 40.0).max(300.0);
        let narrow_modal = mw < 620.0;
        let target_h: f64 = if narrow_modal { 620.0 } else { 460.0 };
        let mh = target_h.min(self.rect.height() - 40.0).max(300.0);
        let m = Rect::new(
            self.rect.center().x - mw / 2.0,
            self.rect.center().y - mh / 2.0,
            self.rect.center().x + mw / 2.0,
            self.rect.center().y + mh / 2.0,
        );
        paint::card(scene, t, m, 18.0, c.surfaces.raised, c.border_white_20);

        let title = format!(
            "Restore  ·  {}  @  {} {}",
            src.map(|s| s.path.clone()).unwrap_or_default(),
            Catalog::day_label(snap.day),
            snap.time
        );
        paint::text_bold(scene, tr, t, &title, 17.0, c.text_primary, m.x0 + 24.0, m.y0 + 20.0);
        paint::text(scene, tr, t, "Choose files to recover, then confirm. Restores never overwrite without asking.", 12.0, c.text_dim, m.x0 + 24.0, m.y0 + 46.0);

        // File browser + options: side by side when wide, stacked (tree over
        // options) when the modal is too narrow for two readable columns.
        let body_y = m.y0 + 74.0;
        let body_bottom = m.y1 - 76.0;
        let (tree, opt) = if narrow_modal {
            // Give the options column enough height for its four rows + the
            // estimate line, so nothing overlaps.
            let mid = body_y + (body_bottom - body_y) * 0.46;
            (
                Rect::new(m.x0 + 24.0, body_y, m.x1 - 24.0, mid - 8.0),
                Rect::new(m.x0 + 24.0, mid + 10.0, m.x1 - 24.0, body_bottom),
            )
        } else {
            let split = m.x0 + mw * 0.58;
            (
                Rect::new(m.x0 + 24.0, body_y, split - 12.0, body_bottom),
                Rect::new(split + 12.0, body_y, m.x1 - 24.0, body_bottom),
            )
        };
        paint::card(scene, t, tree, 12.0, c.neutral[0].with_alpha_factor(0.5), c.border_white_10);
        paint::text(scene, tr, t, "Snapshot contents", 11.0, c.text_dim, tree.x0 + 14.0, tree.y0 + 10.0);

        let files: [(usize, &str, &str, bool); 8] = [
            (0, "home/", "", true),
            (1, "Documents/", "2.1 GB", true),
            (2, "report-q2.qdoc", "4.2 MB", false),
            (1, "Pictures/", "18.4 GB", true),
            (2, "2026-trip/", "3.1 GB", true),
            (1, "Projects/", "9.8 GB", true),
            (2, "quickdesktop/", "1.2 GB", true),
            (1, ".config/", "240 MB", false),
        ];
        let row_h = 30.0;
        let ty0 = tree.y0 + 30.0;
        for (i, (depth, name, size, is_dir)) in files.iter().enumerate() {
            let ry = ty0 + i as f64 * row_h;
            // Don't spill rows past the (possibly shortened) tree card.
            if ry + row_h > tree.y1 - 4.0 {
                break;
            }
            let row = Rect::new(tree.x0 + 6.0, ry, tree.x1 - 6.0, ry + row_h - 2.0);
            let sel = self.file_sel == i;
            if sel {
                paint::fill_rrect(scene, t, row, 7.0, theme.accent_primary.with_alpha_factor(0.18));
            } else if self.hover == Some(Hit::FileRow(i)) {
                paint::fill_rrect(scene, t, row, 7.0, c.surface_white_5);
            }
            let ix = row.x0 + 10.0 + *depth as f64 * 18.0;
            let icol = if *is_dir { theme.accent_primary } else { c.text_dim };
            if *is_dir {
                // folder glyph
                let fr = Rect::new(ix, row.center().y - 5.0, ix + 13.0, row.center().y + 5.0);
                paint::fill_rrect(scene, t, fr, 2.0, icol.with_alpha_factor(0.8));
            } else {
                let fr = Rect::new(ix + 1.0, row.center().y - 6.0, ix + 11.0, row.center().y + 6.0);
                paint::stroke_rrect(scene, t, fr, 2.0, icol, 1.3);
            }
            paint::text_vc(scene, tr, t, name, 12.5, c.text_primary, ix + 20.0, row.y0, row_h - 2.0);
            if !size.is_empty() {
                paint::text_right(scene, tr, t, size, 11.0, c.text_dim, row.x1 - 10.0, row.center().y - 6.0);
            }
            self.hits.push((row, Hit::FileRow(i)));
        }

        // Options column (laid out above).
        paint::text_bold(scene, tr, t, "Restore options", 13.5, c.text_primary, opt.x0, opt.y0 + 8.0);
        let opts = [
            ("Restore to original location", true),
            ("Restore to a new folder", false),
            ("Skip files that already exist", true),
            ("Verify checksums after restore", true),
        ];
        for (i, (label, on)) in opts.iter().enumerate() {
            let oy = opt.y0 + 40.0 + i as f64 * 38.0;
            let box_r = Rect::new(opt.x0, oy, opt.x0 + 18.0, oy + 18.0);
            paint::fill_rrect(scene, t, box_r, 5.0, if *on { theme.accent_primary } else { c.neutral[2] });
            paint::stroke_rrect(scene, t, box_r, 5.0, c.border_white_10, 1.0);
            if *on {
                paint::poly(scene, t, &[(box_r.x0 + 4.0, box_r.center().y), (box_r.x0 + 7.5, box_r.y1 - 5.0), (box_r.x1 - 4.0, box_r.y0 + 5.0)], c.text_primary, 2.0, false);
            }
            paint::text(scene, tr, t, label, 12.5, c.text_primary, box_r.x1 + 12.0, oy + 2.0);
        }
        // Size summary.
        paint::hline(scene, t, opt.x0, opt.x1, opt.y1 - 40.0, c.border_white_10, 1.0);
        paint::text(scene, tr, t, "Estimated restore", 11.0, c.text_dim, opt.x0, opt.y1 - 30.0);
        paint::text_right(scene, tr, t, &ui::format::bytes(snap.size_bytes), 14.0, c.text_primary, opt.x1, opt.y1 - 32.0);

        // Footer buttons.
        let by = m.y1 - 56.0;
        let bh = 38.0;
        let confirm = Rect::new(m.x1 - 24.0 - 170.0, by, m.x1 - 24.0, by + bh);
        let hovered = self.hover == Some(Hit::RestoreConfirm);
        paint::fill_rrect(scene, t, confirm, 9.0, if hovered { theme.accent_hover } else { theme.accent_primary });
        paint::text_center(scene, tr, t, "Restore selected", 14.0, c.text_primary, confirm.x0, confirm.x1, by + (bh - 14.0) / 2.0);
        self.hits.push((confirm, Hit::RestoreConfirm));

        let cancel = Rect::new(confirm.x0 - 12.0 - 110.0, by, confirm.x0 - 12.0, by + bh);
        paint::fill_rrect(scene, t, cancel, 9.0, c.surfaces.raised);
        paint::stroke_rrect(scene, t, cancel, 9.0, c.border_white_10, 1.0);
        paint::text_center(scene, tr, t, "Cancel", 14.0, c.text_dim, cancel.x0, cancel.x1, by + (bh - 14.0) / 2.0);
        self.hits.push((cancel, Hit::RestoreCancel));
    }

    fn draw_toast(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some((msg, when)) = self.toast.clone() else { return };
        let age = when.elapsed().as_secs_f64();
        if age > 3.5 {
            self.toast = None;
            return;
        }
        let c = &theme.tokens.colors;
        let alpha = if age > 2.5 { (1.0 - (age - 2.5)) as f32 } else { 1.0 };
        let (tw, _) = tr.measure(&msg, 13.0, None);
        let w = tw as f64 + 56.0;
        let toast = Rect::new(
            self.rect.center().x - w / 2.0,
            self.rect.y1 - STATUS_H - 56.0,
            self.rect.center().x + w / 2.0,
            self.rect.y1 - STATUS_H - 20.0,
        );
        paint::fill_rrect(scene, t, toast, 10.0, c.neutral[0].with_alpha_factor(0.92 * alpha));
        paint::stroke_rrect(scene, t, toast, 10.0, theme.accent_primary.with_alpha_factor(0.6 * alpha), 1.0);
        paint::dot(scene, t, Point::new(toast.x0 + 22.0, toast.center().y), 4.0, c.semantic.success.with_alpha_factor(alpha));
        paint::text_vc(scene, tr, t, &msg, 13.0, c.text_primary.with_alpha_factor(alpha), toast.x0 + 36.0, toast.y0, toast.height());
    }

    // ---- little vector icons -------------------------------------------

    fn draw_shield(&self, scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color) {
        let (x, y) = (center.x, center.y);
        let pts = [
            (x, y - r),
            (x + r * 0.85, y - r * 0.55),
            (x + r * 0.85, y + r * 0.25),
            (x, y + r),
            (x - r * 0.85, y + r * 0.25),
            (x - r * 0.85, y - r * 0.55),
        ];
        paint::poly(scene, t, &pts, color, 2.0, true);
        paint::poly(scene, t, &[(x - r * 0.4, y), (x - r * 0.05, y + r * 0.4), (x + r * 0.45, y - r * 0.35)], color, 2.0, false);
    }

    fn draw_source_icon(&self, scene: &mut Scene, t: Affine, kind: SourceKind, center: Point, r: f64, color: Color) {
        let (x, y) = (center.x, center.y);
        match kind {
            SourceKind::Home => {
                paint::poly(scene, t, &[(x - r, y + r * 0.2), (x, y - r), (x + r, y + r * 0.2)], color, 2.0, false);
                paint::poly(scene, t, &[(x - r * 0.7, y), (x - r * 0.7, y + r), (x + r * 0.7, y + r), (x + r * 0.7, y)], color, 2.0, false);
            }
            SourceKind::System => {
                scene.stroke(&vello::kurbo::Stroke::new(2.0), t, color, None, &vello::kurbo::Circle::new(center, r * 0.55));
                for k in 0..8 {
                    let a = k as f64 * std::f64::consts::TAU / 8.0;
                    let (dx, dy) = (a.cos(), a.sin());
                    paint::poly(scene, t, &[(x + dx * r * 0.6, y + dy * r * 0.6), (x + dx * r, y + dy * r)], color, 2.0, false);
                }
            }
            SourceKind::Workspace => {
                paint::stroke_rrect(scene, t, Rect::new(x - r, y - r * 0.7, x + r, y + r * 0.7), 2.0, color, 2.0);
                paint::poly(scene, t, &[(x - r * 0.5, y + r), (x + r * 0.5, y + r)], color, 2.0, false);
            }
            SourceKind::Custom => {
                paint::stroke_rrect(scene, t, Rect::new(x - r, y - r, x + r, y + r), 3.0, color, 2.0);
            }
        }
    }

    fn draw_dest_icon(&self, scene: &mut Scene, t: Affine, kind: DestKind, center: Point, r: f64, color: Color) {
        let (x, y) = (center.x, center.y);
        match kind {
            DestKind::LocalBtrfs => {
                for k in 0..3 {
                    let yy = y - r + k as f64 * r;
                    paint::stroke_rrect(scene, t, Rect::new(x - r, yy, x + r, yy + r * 0.7), 1.5, color, 1.6);
                }
            }
            DestKind::External => {
                paint::stroke_rrect(scene, t, Rect::new(x - r, y - r * 0.6, x + r, y + r * 0.6), 2.0, color, 1.8);
                paint::dot(scene, t, Point::new(x + r * 0.5, y), 1.6, color);
            }
            DestKind::RemoteRestic => {
                scene.stroke(&vello::kurbo::Stroke::new(1.8), t, color, None, &vello::kurbo::Circle::new(center, r));
                paint::poly(scene, t, &[(x - r, y), (x + r, y)], color, 1.6, false);
                paint::poly(scene, t, &[(x, y - r), (x, y + r)], color, 1.6, false);
            }
        }
    }

    // ---- event handling -------------------------------------------------

    fn hit_at(&self, p: Point) -> Option<Hit> {
        // Last pushed wins (topmost), so iterate in reverse.
        self.hits.iter().rev().find(|(r, _)| r.contains(p)).map(|(_, h)| h.clone())
    }

    fn dispatch(&mut self, hit: Hit, doc: &mut AutoCommit, root: &ObjId) {
        match hit {
            Hit::Tab(v) => BackupsDoc::set_view(doc, root, v),
            Hit::Source(opt) => {
                BackupsDoc::set_selected_source(doc, root, opt.as_deref());
                BackupsDoc::set_selected_snapshot(doc, root, None);
                BackupsDoc::set_view(doc, root, View::Timeline);
                self.timeline_scroll = 0.0;
                // Picking a source from an overlay rail dismisses the drawer.
                if self.drawer.is_overlay() {
                    self.drawer.close();
                }
            }
            Hit::Snap(id) => {
                let toggle = self.ui.sel_snapshot.as_deref() == Some(id.as_str());
                BackupsDoc::set_selected_snapshot(doc, root, if toggle { None } else { Some(&id) });
            }
            Hit::BackupNow => self.start_job(),
            Hit::Restore => {
                if self.ui.sel_snapshot.is_some() {
                    BackupsDoc::set_restore_open(doc, root, true);
                    self.file_sel = 0;
                }
            }
            Hit::RestoreConfirm => {
                BackupsDoc::set_restore_open(doc, root, false);
                self.toast_now("Restore started in the background");
            }
            Hit::RestoreCancel => BackupsDoc::set_restore_open(doc, root, false),
            Hit::Verify => self.toast_now("Verifying snapshot integrity…"),
            Hit::Delete => {
                if let Some(id) = self.ui.sel_snapshot.clone() {
                    self.catalog.snapshots.retain(|s| s.id != id);
                    BackupsDoc::set_selected_snapshot(doc, root, None);
                    self.toast_now("Snapshot deleted");
                }
            }
            Hit::Dest(id) => {
                BackupsDoc::set_selected_dest(doc, root, &id);
                BackupsDoc::set_view(doc, root, View::Destinations);
                if self.drawer.is_overlay() {
                    self.drawer.close();
                }
            }
            Hit::Policy(field, delta) => {
                self.bump_retention(doc, root, field, delta);
                self.policy_focus = PolicyField::ALL.iter().position(|f| *f == field).unwrap_or(0);
            }
            Hit::FileRow(i) => self.file_sel = i,
            // The Timeline source-rail collapse / summon toggle.
            Hit::ToggleRail => self.drawer.toggle(),
            // A tap on the scrim behind an open overlay rail dismisses it.
            Hit::CloseRail => self.drawer.close(),
        }
        self.load_cache(doc, root);
    }

    fn start_job(&mut self) {
        if self.job.is_some() {
            return;
        }
        let source = self.ui.sel_source.clone().unwrap_or_else(|| "home".to_string());
        self.job = Some(Job { source, started: Instant::now(), dur: Duration::from_millis(2600) });
        self.toast_now("Snapshot started");
    }

    /// Materialise the just-finished job as a new manual snapshot.
    fn finish_job(&mut self) {
        let Some(job) = self.job.take() else { return };
        let size = self.catalog.protected_bytes(&job.source).max(1);
        let id = format!("manual-{}", self.catalog.snapshots.len());
        let dest = "pool".to_string();
        self.catalog.snapshots.insert(
            0,
            Snapshot {
                id: id.clone(),
                source_id: job.source.clone(),
                dest_id: dest,
                day: 0,
                time: "now".into(),
                kind: SnapType::Manual,
                retention: Retention::Keep,
                size_bytes: size,
                delta_bytes: 128 * 1024 * 1024,
                files_changed: 64,
                status: SnapStatus::Complete,
            },
        );
        self.toast_now("Snapshot complete");
    }

    fn cycle_view(&mut self, dir: i32, doc: &mut AutoCommit, root: &ObjId) {
        let idx = View::ALL.iter().position(|v| *v == self.ui.view).unwrap_or(0) as i32;
        let n = View::ALL.len() as i32;
        let next = ((idx + dir) % n + n) % n;
        BackupsDoc::set_view(doc, root, View::ALL[next as usize]);
        self.load_cache(doc, root);
    }

    fn move_snapshot_selection(&mut self, dir: i32, doc: &mut AutoCommit, root: &ObjId) {
        let snaps = self.catalog.timeline(self.ui.sel_source.as_deref());
        if snaps.is_empty() {
            return;
        }
        let cur = self
            .ui
            .sel_snapshot
            .as_deref()
            .and_then(|id| snaps.iter().position(|s| s.id == id));
        let next = match cur {
            Some(i) => ((i as i32 + dir).rem_euclid(snaps.len() as i32)) as usize,
            None => if dir >= 0 { 0 } else { snaps.len() - 1 },
        };
        let id = snaps[next].id.clone();
        BackupsDoc::set_selected_snapshot(doc, root, Some(&id));
        self.load_cache(doc, root);
    }
}

impl Widget for BackupsApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.seeded {
            BackupsDoc::seed(state, path);
            self.seeded = true;
        }
        // Finish any job whose timer has elapsed before reading state.
        if self.job_progress() == Some(1.0) {
            self.finish_job();
        }
        self.load_cache(state, path);
        // Auto-select the newest snapshot in the active filter so the detail
        // panel + actions are visible (mirrors Notes auto-selecting a page).
        if self.ui.sel_snapshot.is_none() {
            let newest = self
                .catalog
                .timeline(self.ui.sel_source.as_deref())
                .first()
                .map(|s| s.id.clone());
            if let Some(id) = newest {
                BackupsDoc::set_selected_snapshot(state, path, Some(&id));
                self.ui.sel_snapshot = Some(id);
            }
        }
        // Dev/test hook (off by default): force a starting view / open the
        // restore sheet so each screen can be captured headlessly via
        // `BACKUPS_VIEW=destinations … --shots`.
        if let Ok(v) = std::env::var("BACKUPS_VIEW") {
            match v.as_str() {
                "sources" => { BackupsDoc::set_view(state, path, View::Sources); self.ui.view = View::Sources; }
                "destinations" => { BackupsDoc::set_view(state, path, View::Destinations); self.ui.view = View::Destinations; }
                "schedules" => { BackupsDoc::set_view(state, path, View::Schedules); self.ui.view = View::Schedules; }
                "restore" => { BackupsDoc::set_restore_open(state, path, true); self.ui.restore_open = true; }
                _ => {}
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &PresenceCtx) {
        self.render(scene, transform, text_renderer, theme);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        // The root fills the window — always paint, never cull.
        self.render(scene, transform, text_renderer, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
                let new_hover = self.hit_at(self.last_mouse);
                if new_hover != self.hover {
                    self.hover = new_hover;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // A click outside the modal closes it.
                if self.ui.restore_open {
                    if let Some(hit) = self.hit_at(self.last_mouse) {
                        self.dispatch(hit, state, path);
                    } else {
                        BackupsDoc::set_restore_open(state, path, false);
                        self.load_cache(state, path);
                    }
                    *handled = true;
                    return;
                }
                if let Some(hit) = self.hit_at(self.last_mouse) {
                    self.dispatch(hit, state, path);
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.ui.view == View::Timeline && self.timeline_view.contains(self.last_mouse) {
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -(*y as f64) * 48.0,
                        MouseScrollDelta::PixelDelta(p) => -p.y,
                    };
                    let max = (self.timeline_content_h - self.timeline_view.height()).max(0.0);
                    self.timeline_scroll = (self.timeline_scroll + dy).clamp(0.0, max);
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: key_event, .. } => {
                if key_event.state != ElementState::Pressed {
                    return;
                }
                self.handle_key(&key_event.logical_key, state, path, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "BackupsApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("backups_app")
    }

    /// Publish the retention-policy resource tree to the shell's Ctrl+K palette
    /// (see [`crate::actions`]).
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its action, so it records exactly one undo boundary bracketing it — the
    /// same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        self.job.is_some() || self.toast.is_some() || self.drawer.is_animating()
    }
}

impl BackupsApp {
    fn handle_key(&mut self, key: &Key, doc: &mut AutoCommit, root: &ObjId, handled: &mut bool) {
        match key {
            Key::Named(NamedKey::Escape) => {
                if self.ui.restore_open {
                    BackupsDoc::set_restore_open(doc, root, false);
                } else {
                    BackupsDoc::set_selected_snapshot(doc, root, None);
                }
                self.load_cache(doc, root);
                *handled = true;
            }
            Key::Named(NamedKey::Tab) => {
                self.cycle_view(1, doc, root);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.ui.view == View::Schedules {
                    self.policy_focus = (self.policy_focus + PolicyField::ALL.len() - 1) % PolicyField::ALL.len();
                } else {
                    self.move_snapshot_selection(-1, doc, root);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                if self.ui.view == View::Schedules {
                    self.policy_focus = (self.policy_focus + 1) % PolicyField::ALL.len();
                } else {
                    self.move_snapshot_selection(1, doc, root);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if self.ui.view == View::Schedules {
                    let f = PolicyField::ALL[self.policy_focus];
                    self.bump_retention(doc, root, f, -1);
                    self.load_cache(doc, root);
                } else {
                    self.cycle_view(-1, doc, root);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                if self.ui.view == View::Schedules {
                    let f = PolicyField::ALL[self.policy_focus];
                    self.bump_retention(doc, root, f, 1);
                    self.load_cache(doc, root);
                } else {
                    self.cycle_view(1, doc, root);
                }
                *handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                if self.ui.view == View::Timeline && self.ui.sel_snapshot.is_some() {
                    BackupsDoc::set_restore_open(doc, root, true);
                    self.file_sel = 0;
                    self.load_cache(doc, root);
                    *handled = true;
                }
            }
            Key::Character(s) => {
                match s.as_str() {
                    "1" => { BackupsDoc::set_view(doc, root, View::Timeline); self.load_cache(doc, root); *handled = true; }
                    "2" => { BackupsDoc::set_view(doc, root, View::Sources); self.load_cache(doc, root); *handled = true; }
                    "3" => { BackupsDoc::set_view(doc, root, View::Destinations); self.load_cache(doc, root); *handled = true; }
                    "4" => { BackupsDoc::set_view(doc, root, View::Schedules); self.load_cache(doc, root); *handled = true; }
                    "n" | "N" => { self.start_job(); *handled = true; }
                    "+" | "=" => {
                        if self.ui.view == View::Schedules {
                            self.bump_retention(doc, root, PolicyField::ALL[self.policy_focus], 1);
                            self.load_cache(doc, root);
                            *handled = true;
                        }
                    }
                    "-" | "_" => {
                        if self.ui.view == View::Schedules {
                            self.bump_retention(doc, root, PolicyField::ALL[self.policy_focus], -1);
                            self.load_cache(doc, root);
                            *handled = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_constructor_starts_with_an_empty_catalog() {
        // The live app must not fabricate sources / snapshots / destinations.
        let app = BackupsApp::new();
        assert!(app.catalog.sources.is_empty(), "no fabricated sources");
        assert!(app.catalog.snapshots.is_empty(), "no fabricated snapshots");
        assert!(app.catalog.destinations.is_empty(), "no fabricated destinations");
    }

    #[test]
    fn demo_constructor_seeds_the_sample_catalog() {
        // The --shots/demo builder keeps the populated sample dashboard.
        let app = BackupsApp::new_demo();
        assert!(!app.catalog.sources.is_empty(), "demo has sources");
        assert!(!app.catalog.snapshots.is_empty(), "demo has snapshots");
        assert!(!app.catalog.destinations.is_empty(), "demo has destinations");
    }
}
