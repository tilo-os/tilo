//! Root widget for the calculator. A single full-window widget that owns
//! the [`CalcModel`], paints the header + the active mode's screen, and
//! routes all keyboard / pointer input. State is flushed into the shared
//! Automerge document on every change (see [`crate::doc`]).

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey};

use ui::Widget;
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tilo_resource::Resource;

use crate::actions::{self, CalculatorAction, GraphKind};
use crate::draw::{self, Align};
use crate::engine;
use crate::graph2d::Graph2dScreen;
use crate::graph3d::Graph3dScreen;
use crate::keypad::{CalcScreen, MEM_KEYS};
use crate::model::{self, CalcModel, Mode, PlotFn};

const TOPBAR_H: f64 = 60.0;
/// Gap between the segments of the mode switch and of the RAD/DEG toggle:
/// one `s1`, so no two segments touch inside their shared track (HIG §7).
const SEG_GAP: f64 = ui::tokens::spacing::Spacing::BASE.s1 as f64;

/// Painted widths of the topbar's widest labels (see
/// `CalculatorApp::measure_topbar_labels`). Zero before the first draw, where
/// `relayout` falls back to the control's ordinary minimum widths.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct TopbarLabels {
    /// Widest full mode label ("Calculator").
    full: f64,
    /// Widest condensed mode label ("Calc").
    short: f64,
    /// Wider of "RAD" / "DEG".
    toggle: f64,
}

impl TopbarLabels {
    /// Base font sizes the topbar paints its labels at (scaled at paint time).
    const SEG_FONT: f32 = 14.0;
    const TOGGLE_FONT: f32 = 13.0;
    /// Horizontal breathing room either side of a label inside its control.
    const SEG_PAD_X: f64 = 16.0;
    const TOGGLE_PAD_X: f64 = 10.0;
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal flag (layout state, \
              drag state, and seed/dirty bookkeeping); they aren't a state \
              machine, and grouping them would mean churning every field \
              access across this widget for no behavioural gain"
)]
pub struct CalculatorApp {
    model: CalcModel,
    calc: CalcScreen,
    g2: Graph2dScreen,
    g3: Graph3dScreen,

    area: Rect,
    topbar: Rect,
    body: Rect,
    seg_rects: [Rect; 3],
    rad_rect: Rect,
    deg_rect: Rect,
    /// Whether the mode segments are too narrow for full labels and must use
    /// the short forms ("Calc" / "2D" / "3D"). Re-derived every `relayout`.
    seg_short: bool,
    /// Widest topbar labels as the renderer actually paints them, refreshed
    /// each draw. `relayout` sizes the mode segments and the RAD/DEG halves
    /// from these so enlarged type widens its control instead of overrunning
    /// a fixed-width neighbour.
    topbar_labels: TopbarLabels,

    mouse: Point,
    dragging: bool,
    drag_last: Point,
    /// A drag-select is in progress inside a math field.
    field_dragging: bool,
    /// Host focus selects this component's record for undo/redo and keyboard
    /// delivery. Individual custom-painted controls remain semantic children.
    host_focused: bool,
    /// Time + position of the last press, for double-click detection.
    last_click: Option<ui::time::Instant>,
    last_click_pos: Point,

    seeded: bool,
    model_dirty: bool,
    /// Something visible changed since the last paint. Cleared by
    /// `mark_drawn`; without it the default (dirty every frame) repainted the
    /// whole desktop on every pointer move while the Calculator was active.
    dirty: bool,
    /// Document heads and pending-operation count at the last model reload,
    /// so `sync` reloads history, functions and surfaces only when the
    /// document actually moved rather than on every frame.
    synced: Option<(Vec<automerge::ChangeHash>, usize)>,
    /// When `Some`, an edit landed at this time and has not yet been sealed into
    /// an undo checkpoint. Drives [`Widget::needs_commit`] so a burst of typing
    /// coalesces into a single Ctrl+Z step once editing pauses.
    last_edit_time: Option<ui::time::Instant>,
    node: Option<NodeId>,

    /// Framework-owned action dispatch. Each on-screen affordance dispatches its
    /// [`CalculatorAction`] through this: hosted via [`crate::resource::ActionSink`]
    /// (framework [`Dispatcher`](ui::framework::Dispatcher)), standalone via the
    /// local action host for tests and benches.
    actions: crate::resource::ActionSink,
}

/// How long editing must pause before a burst of keystrokes is sealed into one
/// undo checkpoint (see [`CalculatorApp::needs_commit`]). Mirrors `TextInput`'s
/// time-based coalescing so undo granularity is per-pause, not per-keystroke.
const COMMIT_COALESCE: std::time::Duration = std::time::Duration::from_millis(600);

impl CalculatorApp {
    /// The live app: boots empty in Calculator mode. Persisted state (if any)
    /// is adopted from the CRDT on the first [`Widget::sync`].
    pub fn new() -> Self {
        Self::with_model(CalcModel::default())
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Project an already admitted Calculator record without authoring schema
    /// or replacing malformed content. The checked component initializes New
    /// records before calling this constructor; Existing records stay read-only.
    ///
    /// # Errors
    ///
    /// Returns the schema error when the record is not a valid Calculator
    /// document.
    pub fn from_document(doc: &AutoCommit) -> Result<Self, String> {
        let mut app = Self::with_model(crate::doc::load_existing(doc)?);
        app.seeded = true;
        Ok(app)
    }

    /// A lived-in demo instance (populated history / plots / surface) used by
    /// the headless `--shots` renderer and tests. Never used by the live loop.
    pub fn seeded() -> Self {
        Self::with_model(CalcModel::seeded())
    }

    /// Shots/demo only: force the initial mode (e.g. a 3D-plot screenshot).
    pub fn shots_set_mode(&mut self, m: Mode) {
        self.set_mode(m);
    }

    /// Shots/demo only: force the history sidebar open/closed with no
    /// animation, so a headless frame deterministically captures the docked /
    /// collapsed / overlay states.
    pub fn shots_set_history_open(&mut self, open: bool) {
        self.calc.snap_sidebar(open);
    }

    fn with_model(model: CalcModel) -> Self {
        Self {
            model,
            calc: CalcScreen::default(),
            g2: Graph2dScreen::default(),
            g3: Graph3dScreen::default(),
            area: Rect::ZERO,
            topbar: Rect::ZERO,
            body: Rect::ZERO,
            seg_rects: [Rect::ZERO; 3],
            rad_rect: Rect::ZERO,
            deg_rect: Rect::ZERO,
            seg_short: false,
            topbar_labels: TopbarLabels::default(),
            mouse: Point::ZERO,
            dragging: false,
            drag_last: Point::ZERO,
            field_dragging: false,
            host_focused: false,
            last_click: None,
            last_click_pos: Point::ZERO,
            seeded: false,
            model_dirty: false,
            dirty: true,
            synced: None,
            last_edit_time: None,
            node: None,
            actions: crate::resource::ActionSink::default(),
        }
    }

    /// Adopt durable history and graph rows from the framework resource state.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::CalculatorState) {
        self.model.history = state.history.iter().map(|(_, e)| e.clone()).collect();
        self.model.funcs = state.funcs.iter().map(|(_, f)| f.clone()).collect();
        self.model.surfaces = state.surfaces.iter().map(|(_, f)| f.clone()).collect();
        self.model.active_fn = self
            .model
            .active_fn
            .min(self.model.funcs.len().saturating_sub(1));
        self.model.active_surface = self
            .model
            .active_surface
            .min(self.model.surfaces.len().saturating_sub(1));
        self.seeded = true;
        self.model_dirty = true;
        self.dirty = true;
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Calculator>,
    ) {
        self.actions.attach(dispatcher);
        self.seeded = true;
    }

    /// Recompute the header geometry from `self.area`. The mode switcher and the
    /// RAD/DEG toggle are packed so they never overlap or clip, reflowing down to
    /// a ~400 px window: the wordmark drops out first, then the mode labels
    /// condense to short forms and the toggle shrinks. All of this is derived
    /// from the *current* width every call, so a resize between breakpoints
    /// reflows rather than staying frozen at the launch layout.
    fn relayout(&mut self) {
        self.topbar = Rect::new(
            self.area.x0,
            self.area.y0,
            self.area.x1,
            self.area.y0 + TOPBAR_H,
        );
        self.body = Rect::new(
            self.area.x0,
            self.area.y0 + TOPBAR_H,
            self.area.x1,
            self.area.y1,
        );

        let w = self.area.width();
        let seg_h = 36.0;
        let y0 = self.area.y0 + (TOPBAR_H - seg_h) / 2.0;

        // The OS draws the window title/app-name — the toolbar reserves only a
        // small left margin, then hands the rest to the functional controls.
        let brand_right = self.area.x0 + 12.0;

        // RAD/DEG anchored to the right edge; compact form when tight. Each
        // half must seat its measured label, so enlarged type widens the
        // toggle rather than spilling out of it.
        let compact_toggle = w < 620.0;
        let toggle_min =
            2.0 * (self.topbar_labels.toggle + 2.0 * TopbarLabels::TOGGLE_PAD_X) + SEG_GAP;
        let toggle_base: f64 = if compact_toggle { 100.0 } else { 124.0 };
        let toggle_w = toggle_base.max(toggle_min);
        let right_margin = if compact_toggle { 12.0 } else { 24.0 };
        let ax1 = self.area.x1 - right_margin;
        let ax0 = ax1 - toggle_w;
        let half = (toggle_w - SEG_GAP) / 2.0;
        self.rad_rect = Rect::new(ax0, y0, ax0 + half, y0 + seg_h);
        self.deg_rect = Rect::new(ax0 + half + SEG_GAP, y0, ax1, y0 + seg_h);

        // The mode switcher fills the gap between the brand and the toggle,
        // centred, with each segment clamped to a tappable width. The clamps
        // grow with the measured labels: a segment is never narrower than its
        // short label and may widen to seat the full one; the switch to short
        // labels happens where the full label would no longer fit.
        let full_w = self.topbar_labels.full + 2.0 * TopbarLabels::SEG_PAD_X;
        let short_w = self.topbar_labels.short + 2.0 * TopbarLabels::SEG_PAD_X;
        let seg_min = 46.0_f64.max(short_w);
        let seg_max = 128.0_f64.max(full_w);
        let seg_area_left = brand_right + 12.0;
        let seg_area_right = ax0 - 12.0;
        let avail = (seg_area_right - seg_area_left).max(3.0 * seg_min + 2.0 * SEG_GAP);
        let seg_w = ((avail - 2.0 * SEG_GAP) / 3.0).clamp(seg_min, seg_max);
        self.seg_short = seg_w < 104.0_f64.max(full_w);
        let seg_total = seg_w * 3.0 + 2.0 * SEG_GAP;
        let seg_start =
            seg_area_left + ((seg_area_right - seg_area_left) - seg_total).max(0.0) / 2.0;
        for (i, rect) in self.seg_rects.iter_mut().enumerate() {
            let sx = seg_start + i as f64 * (seg_w + SEG_GAP);
            *rect = Rect::new(sx, y0, sx + seg_w, y0 + seg_h);
        }
    }

    /// Measure the topbar's labels at the size they are about to be painted.
    /// Returns `true` when a width changed (first frame, a text-scale change),
    /// so the caller re-runs `relayout` before painting this frame.
    fn measure_topbar_labels(&mut self, tr: &mut TextRenderer) -> bool {
        let widest = |tr: &mut TextRenderer, labels: &[&str], size: f32| {
            labels
                .iter()
                .map(|label| draw::measure_w(tr, label, size, TextWeight::Semibold))
                .fold(0.0_f64, f64::max)
        };
        let modes = Mode::all();
        let full: Vec<&str> = modes.iter().map(|m| m.label()).collect();
        let short: Vec<&str> = modes.iter().map(|m| m.short_label()).collect();
        let measured = TopbarLabels {
            full: widest(tr, &full, TopbarLabels::SEG_FONT),
            short: widest(tr, &short, TopbarLabels::SEG_FONT),
            toggle: widest(tr, &["RAD", "DEG"], TopbarLabels::TOGGLE_FONT),
        };
        let changed = measured != self.topbar_labels;
        self.topbar_labels = measured;
        changed
    }

    fn layout_active(&mut self) {
        match self.model.mode {
            Mode::Calc => self.calc.layout(self.body, &self.model),
            Mode::Graph2d => self.g2.layout(self.body, &self.model),
            Mode::Graph3d => self.g3.layout(self.body, &self.model),
        }
    }

    /// The context Ctrl+K needs to build the calculator's resource tree: the
    /// history + saved graphs the in-memory model mirrors from the shared
    /// document. Read-only snapshot; nothing is fabricated.
    pub(crate) fn calc_resource_ctx(&self) -> crate::actions::CalcResourceCtx {
        crate::actions::CalcResourceCtx {
            mode: self.model.mode,
            history: self.model.history.clone(),
            funcs: self.model.funcs.clone(),
            surfaces: self.model.surfaces.clone(),
        }
    }

    /// Dispatch one [`CalculatorAction`] — the single CRDT-mutation path shared
    /// with the Ctrl+K palette. Applied through the Tilo action executor (a pinned
    /// actor + committed change, so it replays byte-identically on peers). The
    /// document heads captured just before the first action of the current event
    /// are stashed so the host records **one** undo boundary bracketing it
    /// ([`CommittedActions::take_committed_action`](ui::undo::CommittedActions::take_committed_action)). The in-memory model is mutated by the
    /// caller for immediate feedback; `store` then reconciles it (a no-op for the
    /// list the action already wrote), and `sync` re-adopts the document on
    /// undo/redo or a merged peer patch.
    ///
    /// Only ever call this for an action that genuinely mutates the document —
    /// the affordances gate on a real change (a non-empty history to clear, an
    /// existing row to toggle/delete) — because the executor commits even a
    /// no-op action, and an empty change wedges the next dispatch. This app
    /// supplies only its domain: the action and its type.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &CalculatorAction) {
        // Unit/headless callers can invoke controls before the first widget
        // sync. Bootstrap their current shared artifacts once, then continue
        // through the same canonical action path as a live window. Hosted
        // surfaces never write the document outside Action::apply — the
        // framework already initialized the record.
        if !self.actions.is_hosted() && !crate::doc::is_seeded(doc) {
            crate::doc::store(doc, &self.model);
        }
        self.actions.dispatch(doc, action);
        // Re-adopt only durable resources. This makes a keyed action visible
        // immediately without ever copying another window's presentation.
        let _ = crate::doc::load_into(doc, &mut self.model);
    }

    /// Commit the current expression (`=` / Enter in Calc mode). Evaluates and
    /// updates the presentation (Ans, result line) via the model, then dispatches
    /// the [`CommitHistory`](CalculatorAction::CommitHistory) action so the new
    /// history row lands on the shared, replayable, undoable path. A failed or
    /// empty evaluation commits nothing and dispatches nothing.
    fn commit_equals(&mut self, doc: &mut AutoCommit) {
        if let Some(entry) = self.model.commit_equals() {
            self.perform_action(
                doc,
                &actions::commit_history_action(&entry.expr, &entry.result),
            );
        }
        self.model_dirty = true;
    }

    fn set_mode(&mut self, m: Mode) {
        if self.model.mode != m {
            self.model.mode = m;
            // Leaving a graph mid-zoom must not carry a stale ease into the next
            // visit; settle both animators to the current model view.
            self.g2.settle_zoom(self.model.view2d);
            self.g3.settle_zoom(self.model.view3d.zoom);
            // Give tests and the screenshot-only constructor a useful starter
            // without needing a live document. Production mode changes go
            // through `set_mode_in_document`, which publishes this same row as
            // a canonical AddGraph action before `store` reconciles the model.
            match m {
                Mode::Graph2d if self.model.funcs.is_empty() => {
                    self.model.funcs.push(PlotFn {
                        expr: String::new(),
                        visible: true,
                    });
                    self.model.active_fn = 0;
                }
                Mode::Graph3d if self.model.surfaces.is_empty() => {
                    self.model.surfaces.push(PlotFn {
                        expr: model::DEFAULT_SURFACE.to_string(),
                        visible: true,
                    });
                    self.model.active_surface = 0;
                }
                _ => {}
            }
            self.model_dirty = true;
        }
    }

    /// Switch modes on a live document.  A first visit to either graph mode
    /// creates its starter row through the exact `AddGraph` action published to
    /// Ctrl+K, rather than smuggling it into the CRDT in a later wholesale
    /// store. This matters for undo, activity history, and collaborators.
    fn set_mode_in_document(&mut self, m: Mode, doc: &mut AutoCommit) {
        if !self.actions.is_hosted() && !crate::doc::is_seeded(doc) {
            crate::doc::store(doc, &self.model);
        }
        let funcs_before = self.model.funcs.len();
        let surfaces_before = self.model.surfaces.len();
        self.set_mode(m);
        if self.model.funcs.len() > funcs_before {
            self.perform_action(doc, &actions::add_graph_action(GraphKind::Curve2d, ""));
        }
        if self.model.surfaces.len() > surfaces_before {
            self.perform_action(
                doc,
                &actions::add_graph_action(GraphKind::Surface3d, model::DEFAULT_SURFACE),
            );
        }
    }

    /// Shared angle-unit transition for the top-bar controls. Pointer and
    /// semantic activation both use this rather than creating a second state
    /// path for assistive technology.
    fn set_degrees(&mut self, degrees: bool) {
        if self.model.deg != degrees {
            self.model.deg = degrees;
            self.model_dirty = true;
        }
    }

    fn draw_topbar(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        ctx.fill_rect(self.topbar, c.surfaces.raised);
        draw::line(
            ctx.scene,
            t,
            Point::new(self.topbar.x0, self.topbar.y1),
            Point::new(self.topbar.x1, self.topbar.y1),
            ui::chrome::chrome_hairline(theme),
            1.0,
        );

        // The OS owns window chrome (title/app-name/logo) — the app draws only
        // its functional toolbar controls here.

        // segmented mode control
        let outer = Rect::new(
            self.seg_rects[0].x0 - 4.0,
            self.seg_rects[0].y0 - 4.0,
            self.seg_rects[2].x1 + 4.0,
            self.seg_rects[2].y1 + 4.0,
        );
        ui::surface::card(ctx.scene, t, outer, 10.0, c.neutral[2]);
        let sel = self.model.mode.index();
        let on_accent = c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST);
        for (i, (m, &r)) in Mode::all().iter().zip(self.seg_rects.iter()).enumerate() {
            if i == sel {
                ctx.fill_round(r, 8.0, c.accent[5]);
            }
            let col = if i == sel { on_accent } else { c.text_dim };
            let label = if self.seg_short {
                m.short_label()
            } else {
                m.label()
            };
            draw::text_in(
                &mut ctx,
                r,
                label,
                &TextStyle::new(TopbarLabels::SEG_FONT).with_weight(TextWeight::Semibold),
                col,
                Align::Center,
            );
        }

        // DEG / RAD toggle
        let outer2 = Rect::new(
            self.rad_rect.x0 - 4.0,
            self.rad_rect.y0 - 4.0,
            self.deg_rect.x1 + 4.0,
            self.deg_rect.y1 + 4.0,
        );
        ui::surface::card(ctx.scene, t, outer2, 10.0, c.neutral[2]);
        if self.model.deg {
            ctx.fill_round(self.deg_rect, 8.0, c.accent[5]);
        } else {
            ctx.fill_round(self.rad_rect, 8.0, c.accent[5]);
        }
        let rad_col = if self.model.deg {
            c.text_dim
        } else {
            on_accent
        };
        let deg_col = if self.model.deg {
            on_accent
        } else {
            c.text_dim
        };
        draw::text_in(
            &mut ctx,
            self.rad_rect,
            "RAD",
            &TextStyle::new(TopbarLabels::TOGGLE_FONT).with_weight(TextWeight::Semibold),
            rad_col,
            Align::Center,
        );
        draw::text_in(
            &mut ctx,
            self.deg_rect,
            "DEG",
            &TextStyle::new(TopbarLabels::TOGGLE_FONT).with_weight(TextWeight::Semibold),
            deg_col,
            Align::Center,
        );
    }

    // -- pointer routing -----------------------------------------------------

    fn on_press(&mut self, p: Point, doc: &mut AutoCommit) {
        // header first
        for (i, r) in self.seg_rects.iter().enumerate() {
            if r.contains(p) {
                self.set_mode_in_document(Mode::from_index(i), doc);
                return;
            }
        }
        if self.rad_rect.contains(p) {
            self.set_degrees(false);
            return;
        }
        if self.deg_rect.contains(p) {
            self.set_degrees(true);
            return;
        }

        // Double-click detection (≤400 ms, within 5 px).
        let now = ui::time::now();
        let double = self
            .last_click
            .is_some_and(|t| now.duration_since(t).as_millis() <= 400)
            && (self.last_click_pos - p).hypot() <= 5.0;
        self.last_click = Some(now);
        self.last_click_pos = p;

        match self.model.mode {
            Mode::Calc => self.on_press_calc(p, doc),
            Mode::Graph2d => self.on_press_g2(p, double, doc),
            Mode::Graph3d => self.on_press_g3(p, double, doc),
        }
    }

    fn on_press_g3(&mut self, p: Point, double: bool, doc: &mut AutoCommit) {
        // Overlay-drawer chrome takes priority over the canvas underneath.
        if self.g3.toggle_at(p) {
            self.g3.toggle_panel();
            return;
        }
        if self.g3.scrim_at(p) {
            self.g3.close_panel();
            return;
        }
        if self.g3.add_at(p) {
            self.perform_action(doc, &actions::add_graph_action(GraphKind::Surface3d, ""));
            self.model.active_surface = self.model.surfaces.len().saturating_sub(1);
            self.model_dirty = true;
        } else if let Some(i) = self.g3.delete_at(p) {
            self.delete_graph_row(GraphKind::Surface3d, i, doc);
        } else if let Some(i) = self.g3.vis_at(p) {
            if let Some(f) = self.model.surfaces.get_mut(i) {
                f.visible = !f.visible;
                self.perform_action(
                    doc,
                    &crate::doc::row_id_at(doc, crate::doc::SURFACES, i).map_or_else(
                        || actions::toggle_graph_visible_action(GraphKind::Surface3d, i),
                        |id| {
                            actions::toggle_graph_visible_action(GraphKind::Surface3d, id.as_str())
                        },
                    ),
                );
                self.model_dirty = true;
            }
        } else if let Some(i) = self.g3.field_at(p) {
            self.model.active_surface = i;
            self.g3.sync_fields(&self.model);
            if let Some(field) = self.g3.active_field_mut(i) {
                field.on_press_cached(p, double);
                self.field_dragging = true;
            }
            self.model_dirty = true;
        } else if let Some(i) = self.g3.row_at(p) {
            self.model.active_surface = i;
            self.model_dirty = true;
        } else if self.g3.overlay_captures(p) {
            // Press inside an open drawer (not a widget): consume it so it
            // doesn't fall through to the canvas as an orbit.
        } else if self.g3.canvas_contains(p) {
            self.dragging = true;
            self.drag_last = p;
        }
    }

    fn on_press_calc(&mut self, p: Point, doc: &mut AutoCommit) {
        // History sidebar chrome takes priority over the keypad underneath.
        if self.calc.toggle_at(p) {
            self.calc.toggle_sidebar();
            return;
        }
        if self.calc.scrim_at(p) {
            // Tapping outside an open overlay drawer dismisses it (Adwaita).
            self.calc.close_sidebar();
            return;
        }
        if let Some(i) = self.calc.key_at(p) {
            self.calc.pressed_key = Some(i);
            if let Some(key) = self.calc.keys().get(i).cloned() {
                // `=` commits to the shared history through an action; every
                // other key is a live edit of the (presentation) expression line.
                if matches!(key, model::Key::Equals) {
                    self.commit_equals(doc);
                } else {
                    model::apply_key(&mut self.model, &key);
                    self.model_dirty = true;
                }
            }
        } else if let Some(i) = self.calc.mem_at(p) {
            if let Some(key) = MEM_KEYS.get(i).cloned() {
                model::apply_key(&mut self.model, &key);
                self.model_dirty = true;
            }
        } else if self.calc.clear_history_at(p) {
            // Only a non-empty history is a real mutation; clearing an empty
            // list would be a no-op action (which wedges the executor).
            if !self.model.history.is_empty() {
                self.model.history.clear();
                self.perform_action(
                    doc,
                    &actions::clear_history_ids_action(crate::doc::visible_ids(
                        doc,
                        crate::doc::HISTORY,
                    )),
                );
                self.model_dirty = true;
            }
        } else if let Some(hi) = self.calc.history_at(p)
            && let Some(e) = self.model.history.get(hi).cloned()
        {
            self.model.expr = e.expr;
            self.model.after_equals = false;
            self.model_dirty = true;
        }
    }

    fn on_press_g2(&mut self, p: Point, double: bool, doc: &mut AutoCommit) {
        // Overlay-drawer chrome takes priority over the canvas underneath.
        if self.g2.toggle_at(p) {
            self.g2.toggle_panel();
            return;
        }
        if self.g2.scrim_at(p) {
            self.g2.close_panel();
            return;
        }
        if self.g2.add_at(p) {
            self.perform_action(doc, &actions::add_graph_action(GraphKind::Curve2d, ""));
            self.model.active_fn = self.model.funcs.len().saturating_sub(1);
            self.model_dirty = true;
        } else if let Some(i) = self.g2.delete_at(p) {
            self.delete_graph_row(GraphKind::Curve2d, i, doc);
        } else if let Some(i) = self.g2.vis_at(p) {
            if let Some(f) = self.model.funcs.get_mut(i) {
                f.visible = !f.visible;
                self.perform_action(
                    doc,
                    &crate::doc::row_id_at(doc, crate::doc::FUNCS, i).map_or_else(
                        || actions::toggle_graph_visible_action(GraphKind::Curve2d, i),
                        |id| actions::toggle_graph_visible_action(GraphKind::Curve2d, id.as_str()),
                    ),
                );
                self.model_dirty = true;
            }
        } else if let Some(i) = self.g2.field_at(p) {
            // Click landed in row i's editor: activate it and place the caret.
            self.model.active_fn = i;
            self.g2.sync_fields(&self.model);
            self.field_press_g2(i, p, double);
            self.model_dirty = true;
        } else if let Some(i) = self.g2.row_at(p) {
            self.model.active_fn = i;
            self.model_dirty = true;
        } else if self.g2.overlay_captures(p) {
            // Press inside an open drawer (not a widget): consume it so it
            // doesn't fall through to the canvas as a pan.
        } else if self.g2.canvas_contains(p) {
            self.dragging = true;
            self.drag_last = p;
        }
    }

    fn field_press_g2(&mut self, i: usize, p: Point, double: bool) {
        // Borrow the renderer-free press path: MathField needs a TextRenderer
        // to lay out for hit-testing. We don't have one here, so the field
        // relays out lazily on the next draw; press uses the cached layout.
        if let Some(field) = self.g2.active_field_mut(i) {
            field.on_press_cached(p, double);
            self.field_dragging = true;
        }
    }

    /// Delete one graph row through the same stable, tombstoned action used by
    /// Ctrl+K. The model is deliberately not spliced optimistically: the action
    /// reload gives this window the exact merged resource list a collaborator
    /// sees, and keeps active-row clamping in one place (`doc::load_into`).
    fn delete_graph_row(&mut self, kind: GraphKind, index: usize, doc: &mut AutoCommit) {
        let collection = match kind {
            GraphKind::Curve2d => crate::doc::FUNCS,
            GraphKind::Surface3d => crate::doc::SURFACES,
        };
        let action = crate::doc::row_id_at(doc, collection, index).map_or_else(
            || actions::delete_graph_action(kind, index),
            |id| actions::delete_graph_action(kind, id.as_str()),
        );
        self.perform_action(doc, &action);
        self.model_dirty = true;
    }

    /// Extend a math-field drag-selection to the current pointer position.
    fn on_field_drag(&mut self, p: Point) {
        match self.model.mode {
            Mode::Graph2d => {
                let a = self.model.active_fn;
                if let Some(field) = self.g2.active_field_mut(a) {
                    field.on_drag_cached(p);
                }
            }
            Mode::Graph3d => {
                let a = self.model.active_surface;
                if let Some(field) = self.g3.active_field_mut(a) {
                    field.on_drag_cached(p);
                }
            }
            Mode::Calc => {}
        }
    }

    fn on_drag(&mut self, d: Vec2) {
        match self.model.mode {
            Mode::Graph2d => {
                let s = self.model.view2d.scale;
                self.model.view2d.cx -= d.x / s;
                self.model.view2d.cy += d.y / s;
                // A pan is immediate (not eased) and resamples in place.
                self.g2.settle_zoom(self.model.view2d);
                self.model_dirty = true;
            }
            Mode::Graph3d => {
                self.model.view3d.yaw += d.x * 0.01;
                self.model.view3d.pitch = (self.model.view3d.pitch + d.y * 0.01).clamp(-1.5, 1.5);
                // An orbit re-projects regardless, so settle any zoom ease.
                self.g3.settle_zoom(self.model.view3d.zoom);
                self.model_dirty = true;
            }
            Mode::Calc => {}
        }
    }

    fn on_wheel(&mut self, step: f64) {
        // Scrolling over a math editor scrolls its (clipped) content — the axis
        // that actually overflows moves; the other is clamped to a no-op.
        match self.model.mode {
            Mode::Graph2d => {
                if let Some(i) = self.g2.field_at(self.mouse) {
                    self.g2.sync_fields(&self.model);
                    if let Some(f) = self.g2.active_field_mut(i) {
                        f.scroll_by(-step * 24.0, -step * 24.0);
                    }
                    return;
                }
            }
            Mode::Graph3d => {
                if let Some(i) = self.g3.field_at(self.mouse) {
                    self.g3.sync_fields(&self.model);
                    if let Some(f) = self.g3.active_field_mut(i) {
                        f.scroll_by(-step * 24.0, -step * 24.0);
                    }
                    return;
                }
            }
            Mode::Calc => {}
        }

        let factor = (1.0 + step * 0.12).clamp(0.4, 2.5);
        match self.model.mode {
            // Scroll the history list when the pointer is over it. This is
            // ephemeral view state (like hover), so it lives on the screen
            // cache rather than the CRDT.
            Mode::Calc if self.calc.history_viewport().contains(self.mouse) => {
                self.calc.history_scroll = (self.calc.history_scroll - step * 48.0).max(0.0);
            }
            Mode::Graph2d if self.g2.canvas_contains(self.mouse) => {
                let v0 = self.model.view2d;
                let before = self.g2.to_world(self.mouse, &v0);
                let mut v = v0;
                v.scale = (v.scale * factor).clamp(2.0, 8000.0);
                let after = self.g2.to_world(self.mouse, &v);
                v.cx += before.x - after.x;
                v.cy += before.y - after.y;
                self.model.view2d = v;
                // Ease toward this new scale while pinning the world point under
                // the cursor (`before`) for the whole animation; the resample is
                // deferred until the ease settles.
                self.g2.begin_zoom(before, self.mouse);
                self.model_dirty = true;
            }
            Mode::Graph3d if self.g3.canvas_contains(self.mouse) => {
                self.model.view3d.zoom = (self.model.view3d.zoom * factor).clamp(0.25, 8.0);
                self.g3.begin_zoom();
                self.model_dirty = true;
            }
            _ => {}
        }
    }

    // -- keyboard ------------------------------------------------------------

    /// Keyboard and IME input an unfocused instance must leave unclaimed.
    fn ignores_unfocused_input(&self, event: &WindowEvent) -> bool {
        !self.host_focused
            && matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            )
    }

    /// Escape with no drawer to dismiss and no expression to clear is left to
    /// the host, whose Back then navigates out of the app.
    fn escape_is_idle(&self, key: &WKey) -> bool {
        matches!(key, WKey::Named(NamedKey::Escape))
            && self.model.mode == Mode::Calc
            && !self.calc.sidebar_open()
            && self.model.expr.is_empty()
    }

    fn on_key(&mut self, key: &WKey, modifiers: winit::event::Modifiers, doc: &mut AutoCommit) {
        self.on_key_with_paste(key, modifiers, doc, ui::clipboard_paste);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one flat per-key dispatch table for every Calculator mode"
    )]
    fn on_key_with_paste(
        &mut self,
        key: &WKey,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
        paste: impl FnOnce() -> Option<String>,
    ) {
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();

        // Ctrl+digit always switches mode, from any screen.
        if ctrl && let WKey::Character(s) = key {
            match s.as_str() {
                "1" => {
                    self.set_mode_in_document(Mode::Calc, doc);
                    return;
                }
                "2" => {
                    self.set_mode_in_document(Mode::Graph2d, doc);
                    return;
                }
                "3" => {
                    self.set_mode_in_document(Mode::Graph3d, doc);
                    return;
                }
                _ => {}
            }
        }

        // Escape first dismisses transient drawers. Clearing an expression is
        // still available on a second Escape in Calculator mode, but an open
        // overlay must never trap a keyboard user behind its scrim.
        if matches!(key, WKey::Named(NamedKey::Escape)) {
            let dismissed = match self.model.mode {
                Mode::Calc if self.calc.sidebar_open() => {
                    self.calc.close_sidebar();
                    true
                }
                Mode::Graph2d if self.g2.panel_open() => {
                    self.g2.close_panel();
                    true
                }
                Mode::Graph3d if self.g3.panel_open() => {
                    self.g3.close_panel();
                    true
                }
                _ => false,
            };
            if dismissed {
                return;
            }
        }

        // The graph editors own a real math field; route their input there.
        match self.model.mode {
            Mode::Graph2d => {
                if self.on_key_g2(key, modifiers, doc) {
                    return;
                }
            }
            Mode::Graph3d => {
                if self.on_key_g3(key, modifiers, doc) {
                    return;
                }
            }
            Mode::Calc => {}
        }

        match key {
            WKey::Named(NamedKey::Enter) => match self.model.mode {
                // In Calc mode Enter evaluates the expression (`=`), committing
                // the result to the shared history via an action.
                Mode::Calc => {
                    self.commit_equals(doc);
                }
                // In a graph mode Enter commits the current row and starts a new
                // empty function/surface, focusing it — the Desmos-style "next
                // expression" gesture. The active-row field already round-trips
                // its edit into the model on each keystroke, so the row is
                // committed; here we just append (an action) and focus it.
                Mode::Graph2d => {
                    self.perform_action(doc, &actions::add_graph_action(GraphKind::Curve2d, ""));
                    self.model.active_fn = self.model.funcs.len().saturating_sub(1);
                    self.model_dirty = true;
                }
                Mode::Graph3d => {
                    self.perform_action(doc, &actions::add_graph_action(GraphKind::Surface3d, ""));
                    self.model.active_surface = self.model.surfaces.len().saturating_sub(1);
                    self.model_dirty = true;
                }
            },
            WKey::Named(NamedKey::Backspace) => {
                if self.model.mode == Mode::Calc {
                    model::apply_key(&mut self.model, &model::Key::Backspace);
                    self.model_dirty = true;
                }
            }
            WKey::Named(NamedKey::Escape | NamedKey::Delete) => {
                if self.model.mode == Mode::Calc {
                    model::apply_key(&mut self.model, &model::Key::Clear);
                    self.model_dirty = true;
                }
            }
            WKey::Character(s) => {
                if ctrl {
                    if self.model.mode == Mode::Calc
                        && matches!(s.as_str(), "v" | "V")
                        && let Some(paste) = paste()
                    {
                        self.on_key(
                            &WKey::Character(paste.into()),
                            winit::event::Modifiers::default(),
                            doc,
                        );
                    }
                    return;
                }
                if self.model.mode == Mode::Calc {
                    for ch in s.chars() {
                        // `=` commits to history via an action; everything else
                        // is a live edit of the presentation expression line.
                        if ch == '=' {
                            self.commit_equals(doc);
                        } else {
                            model::type_char(&mut self.model, ch);
                        }
                    }
                    self.model_dirty = true;
                }
            }
            _ => {}
        }
    }

    /// Route a key to the active Graph 2D function field. Returns whether the
    /// key was consumed by the field.
    fn on_key_g2(
        &mut self,
        key: &WKey,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
    ) -> bool {
        self.g2.sync_fields(&self.model);
        let a = self.model.active_fn;
        let Some(field) = self.g2.active_field_mut(a) else {
            return false;
        };
        // Only consume keys the field actually handles (so e.g. mode switches
        // and unhandled named keys fall through).
        if !field_handles(key, modifiers) {
            return false;
        }
        let changed = field.handle_key(key, &modifiers);
        if changed {
            let linear = field.current_linear();
            if let Some(f) = self.model.funcs.get_mut(a) {
                f.expr = linear;
            }
            if let Some(id) = crate::doc::row_id_at(doc, crate::doc::FUNCS, a) {
                let action = actions::set_graph_expr_action(
                    GraphKind::Curve2d,
                    id.as_str(),
                    self.model.funcs.get(a).map_or("", |f| f.expr.as_str()),
                );
                self.perform_action(doc, &action);
            }
            self.model_dirty = true;
        }
        true
    }

    /// Route a key to the active Graph 3D surface field. Returns whether the
    /// key was consumed by the field.
    fn on_key_g3(
        &mut self,
        key: &WKey,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
    ) -> bool {
        self.g3.sync_fields(&self.model);
        let a = self.model.active_surface;
        let Some(field) = self.g3.active_field_mut(a) else {
            return false;
        };
        if !field_handles(key, modifiers) {
            return false;
        }
        let changed = field.handle_key(key, &modifiers);
        if changed {
            let linear = field.current_linear();
            if let Some(f) = self.model.surfaces.get_mut(a) {
                f.expr = linear;
            }
            if let Some(id) = crate::doc::row_id_at(doc, crate::doc::SURFACES, a) {
                let action = actions::set_graph_expr_action(
                    GraphKind::Surface3d,
                    id.as_str(),
                    self.model.surfaces.get(a).map_or("", |f| f.expr.as_str()),
                );
                self.perform_action(doc, &action);
            }
            self.model_dirty = true;
        }
        true
    }
}

impl Default for CalculatorApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Bench/test accessors used by the headless interaction bench (`--bench-clicks`)
/// and regression tests. Not used by the live app.
impl CalculatorApp {
    #[doc(hidden)]
    pub fn bench_seg_center(&self, i: usize) -> Point {
        self.seg_rects.get(i).map_or(Point::ZERO, Rect::center)
    }

    /// Screen-space center of the Calc keypad key labelled `label`, after the
    /// current layout. `None` if the key is absent or the pad isn't laid out.
    #[doc(hidden)]
    pub fn bench_calc_key_center(&self, label: &str) -> Option<Point> {
        let i = self.calc.key_index_for_label(label)?;
        self.calc.key_center(i)
    }

    /// Current calculator expression length, in characters.
    #[doc(hidden)]
    pub fn bench_expr_len(&self) -> usize {
        self.model.expr.chars().count()
    }

    #[doc(hidden)]
    pub fn bench_mode_is_calc(&self) -> bool {
        self.model.mode == Mode::Calc
    }
}

/// Flatten a [`PresenceCtx`]'s remote cursors into `(scene point, colour)`
/// pairs the math fields can snap onto their glyph layout. Colours come from
/// the peer palette already resolved by the collab layer.
fn remote_carets(presence: &PresenceCtx) -> Vec<(Point, Color)> {
    presence
        .remote_cursors
        .iter()
        .map(|rc| {
            let p = Point::new(f64::from(rc.position[0]), f64::from(rc.position[1]));
            let to_u8 = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
            let col = Color::from_rgba8(
                to_u8(rc.color[0]),
                to_u8(rc.color[1]),
                to_u8(rc.color[2]),
                to_u8(rc.color[3]),
            );
            (p, col)
        })
        .collect()
}

/// Whether a key is one the math field acts on (so the app knows to route it
/// there and consume it, vs. letting it fall through to mode-level handling).
fn field_handles(key: &WKey, modifiers: winit::event::Modifiers) -> bool {
    let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
    match key {
        WKey::Named(
            NamedKey::ArrowLeft
            | NamedKey::ArrowRight
            | NamedKey::ArrowUp
            | NamedKey::ArrowDown
            | NamedKey::Home
            | NamedKey::End
            | NamedKey::Backspace
            | NamedKey::Delete
            | NamedKey::Space,
        ) => true,
        WKey::Character(s) if ctrl => {
            matches!(s.as_str(), "a" | "A" | "c" | "C" | "x" | "X" | "v" | "V")
        }
        WKey::Character(s) => s.chars().any(field_accepts),
        _ => false,
    }
}

fn field_accepts(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || "+-*/^(),. %!·π√".contains(ch)
        || ch == '×'
        || ch == '÷'
        || ch == '−'
}

/// Speak intent rather than a lone glyph for the custom-painted keypad.
fn key_accessible_name(key: &model::Key) -> String {
    use model::Key;
    match key {
        Key::Equals => "Evaluate expression".into(),
        Key::Clear => "Clear expression".into(),
        Key::Backspace => "Backspace".into(),
        Key::ToggleSign => "Toggle sign".into(),
        Key::Percent => "Percent".into(),
        Key::Ans => "Insert previous answer".into(),
        Key::MemAdd => "Add expression to memory".into(),
        Key::MemSub => "Subtract expression from memory".into(),
        Key::MemRecall => "Recall memory".into(),
        Key::MemClear => "Clear memory".into(),
        Key::Wrap(_, _) => "Square expression".into(),
        Key::Ins("sin(") => "Sine".into(),
        Key::Ins("cos(") => "Cosine".into(),
        Key::Ins("tan(") => "Tangent".into(),
        Key::Ins("asin(") => "Inverse sine".into(),
        Key::Ins("acos(") => "Inverse cosine".into(),
        Key::Ins("atan(") => "Inverse tangent".into(),
        Key::Ins("sqrt(") => "Square root".into(),
        Key::Ins("ln(") => "Natural logarithm".into(),
        Key::Ins("log10(") => "Base 10 logarithm".into(),
        Key::Ins("exp(") => "Exponential".into(),
        Key::Ins("1/(") => "Reciprocal".into(),
        Key::Ins("^") => "Power".into(),
        Key::Ins("*") => "Multiply".into(),
        Key::Ins("/") => "Divide".into(),
        Key::Ins("+") => "Add".into(),
        Key::Ins("-") => "Subtract".into(),
        Key::Ins("pi") => "Pi".into(),
        Key::Ins("e") => "Euler's number".into(),
        _ => format!("Calculator key {}", key.label()),
    }
}

impl CalculatorApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        if kind != tilo_actions::OperationKind::View {
            return Err("Mode changes require a View invocation".into());
        }
        if let actions::CalculatorOperation::RecallHistory { id } = &operation {
            let index = crate::doc::visible_ids(doc, crate::doc::HISTORY)
                .iter()
                .position(|visible| visible == id)
                .ok_or("Calculation no longer exists")?;
            let document = crate::doc::load(doc).ok_or("Calculation history is unavailable")?;
            let entry = document
                .history
                .get(index)
                .ok_or("Calculation no longer exists")?;
            self.model.expr.clone_from(&entry.expr);
            self.model.after_equals = false;
            self.model_dirty = true;
            self.set_mode(Mode::Calc);
            return Ok(true);
        }
        // Resource-open operations are presentation-only except for the first
        // graph visit, whose starter must still be a replayable action. The
        // handler receives an immutable document reference, so stage the
        // visible mode here; the regular interaction path will materialize a
        // starter before editing. (A palette add action is always available.)
        self.set_mode(match operation {
            actions::CalculatorOperation::ShowCalculator => Mode::Calc,
            actions::CalculatorOperation::ShowGraph2d => Mode::Graph2d,
            actions::CalculatorOperation::ShowGraph3d => Mode::Graph3d,
            actions::CalculatorOperation::RecallHistory { .. } => {
                return Ok(false);
            }
        });
        Ok(true)
    }
}

impl Widget for CalculatorApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::resources::PublishesResources,
        dyn ui::undo::UndoPoints
    );

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.dirty |= self.host_focused != focused;
        self.host_focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.host_focused
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        if !self.host_focused {
            return None;
        }
        match self.model.mode {
            Mode::Calc => self.calc.expression_ime_rect(),
            Mode::Graph2d => self.g2.field_rect(self.model.active_fn),
            Mode::Graph3d => self.g3.field_rect(self.model.active_surface),
        }
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.host_focused
            .then_some(winit::window::ImePurpose::Normal)
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[actions::CALCULATOR_OPERATION_TYPE]
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

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        use automerge::transaction::Transactable as _;
        let (state, _) = ui::doc::document(state, path);
        if !self.seeded {
            if crate::doc::is_seeded(state) {
                let _ = crate::doc::load_into(state, &mut self.model);
            } else if self.actions.is_hosted() {
                // Framework hosts initialize via Resource::initialize; never
                // write the shared document from the surface.
            } else {
                // Keep standalone native and checked component roots on the
                // same schema initializer. The explicit model preserves the
                // separate screenshot/demo fixture path.
                let _ = crate::doc::initialize(state, &self.model);
            }
            self.seeded = true;
            return;
        }
        // After the first frame the CRDT holds the source of truth for all
        // persisted state. If the document changed underneath us — Ctrl+Z/Y
        // reloaded a snapshot through `ui::State`, or a collaborator's patch
        // merged — adopt it so the restored formula / history / view is what we
        // render and edit next. Without this, undo/redo would revert the CRDT
        // but leave the on-screen model stale (the actual bug).
        //
        // Our own in-frame edits round-trip byte-identically through
        // store→load, so this reloads *only* on a genuine external change.
        // `after_equals` is ephemeral (never persisted), so carry it across the
        // comparison to avoid a spurious reload after an `=`.
        //
        // Reload only when the document moved: committed heads differ, or a
        // transaction is or was open. Reloading every frame re-read every
        // history row, function and surface on each pointer move.
        let heads = state.base_heads();
        let pending = state.pending_ops();
        let unchanged = self
            .synced
            .as_ref()
            .is_some_and(|(synced_heads, synced_pending)| {
                *synced_heads == heads && *synced_pending == 0 && pending == 0
            });
        if !unchanged {
            let _ = crate::doc::load_into(state, &mut self.model);
            self.synced = Some((heads, pending));
            self.dirty = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` only errs when the tree is at capacity; degrade to an
        // inert node rather than aborting the whole session.
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap_or_else(|_| NodeId::from(0u64));
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        let area = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        self.dirty |= area != self.area;
        self.area = area;
        self.relayout();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, self.area, c.surfaces.base);
        // Size the topbar controls from the labels as this frame paints them
        // (the screens below set the same paint scale before their own text).
        draw::set_text_scale(draw::paint_scale(tr, theme));
        if self.measure_topbar_labels(tr) {
            self.relayout();
        }
        let body = self.body;
        match self.model.mode {
            Mode::Calc => self.calc.draw(scene, t, tr, theme, &self.model, body),
            Mode::Graph2d => {
                let trace = if self.g2.canvas_contains(self.mouse) {
                    Some(self.mouse)
                } else {
                    None
                };
                self.g2.draw(scene, t, tr, theme, &self.model, body, trace);
            }
            Mode::Graph3d => self.g3.draw(scene, t, tr, theme, &self.model, body),
        }

        // Remote collaborators' carets, mapped into the active formula field's
        // 2D glyph layout (peers editing the same graph show up as coloured
        // carets inside the math, not just as a pointer over the window).
        let remote = remote_carets(presence);
        if !remote.is_empty() {
            match self.model.mode {
                Mode::Graph2d => {
                    self.g2
                        .draw_remote_carets(scene, t, self.model.active_fn, &remote);
                }
                Mode::Graph3d => {
                    self.g3
                        .draw_remote_carets(scene, t, self.model.active_surface, &remote);
                }
                Mode::Calc => {}
            }
        }

        self.draw_topbar(scene, t, tr, theme);
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Window groups offer an event to visible apps in presentation order.
        // Keyboard and IME input belongs to the host-selected Calculator only;
        // an unfocused instance must leave it unclaimed so that selected sibling
        // can receive the same event. Pointer routing remains geometry-based.
        if self.ignores_unfocused_input(event) {
            return;
        }
        self.relayout();
        self.layout_active();
        let hover_before = (
            self.calc.hovered_key,
            self.calc.hovered_mem,
            self.g2.hovered_row,
            self.g3.hovered_row,
        );

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.mouse = p;
                match self.model.mode {
                    Mode::Calc => {
                        self.calc.hovered_key = self.calc.key_at(p);
                        self.calc.hovered_mem = self.calc.mem_at(p);
                    }
                    Mode::Graph2d => {
                        self.g2.hovered_row = self.g2.row_at(p);
                    }
                    Mode::Graph3d => {
                        self.g3.hovered_row = self.g3.row_at(p);
                    }
                }
                if self.field_dragging {
                    self.on_field_drag(p);
                } else if self.dragging {
                    let d = p - self.drag_last;
                    self.on_drag(d);
                    self.drag_last = p;
                }
                // Motion that changed no hover state and drags nothing is
                // inert: leave it unconsumed so the host can skip the frame.
                let hover_now = (
                    self.calc.hovered_key,
                    self.calc.hovered_mem,
                    self.g2.hovered_row,
                    self.g3.hovered_row,
                );
                *ctx.handled = self.dragging || self.field_dragging || hover_now != hover_before;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_press(self.mouse, ctx.state);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.dragging = false;
                self.field_dragging = false;
                self.g2.release_fields();
                self.g3.release_fields();
                self.calc.pressed_key = None;
                *ctx.handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let step = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(pos) => pos.y / 40.0,
                };
                self.on_wheel(step);
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                *ctx.handled = !self.escape_is_idle(&key_event.logical_key);
                self.on_key(&key_event.logical_key, *ctx.modifiers, ctx.state);
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                self.on_key(
                    &WKey::Character(text.clone().into()),
                    winit::event::Modifiers::default(),
                    ctx.state,
                );
                *ctx.handled = true;
            }
            WindowEvent::Ime(_) => {
                *ctx.handled = true;
            }
            _ => {}
        }

        // Everything but inert pointer motion may have changed what is drawn.
        if !matches!(event, WindowEvent::CursorMoved { .. }) || *ctx.handled || self.model_dirty {
            self.dirty = true;
        }
        if self.model_dirty {
            // `model_dirty` is presentation-only. Durable changes have already
            // travelled through `perform_action`; never mirror this window's
            // mode, selection or viewport back into the shared document.
            self.model_dirty = false;
            // Mark an uncommitted edit so `needs_commit` can seal it into an
            // undo checkpoint once editing pauses (character-key edits are not
            // committed by the App's per-event path, only coalesced here).
            self.last_edit_time = Some(ui::time::now());
        }
    }

    fn name(&self) -> &'static str {
        "CalculatorApp"
    }

    fn needs_redraw(&self) -> bool {
        // `model_dirty` covers mutations that arrive outside `handle_event`
        // (resource operations, assistive actions) before the next event.
        self.dirty || self.model_dirty || self.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// The calculator is custom-painted, so publish the complete retained
    /// control tree rather than leaving screen-reader users with only its
    /// toolbar. Every painted control below has the same activation path as
    /// its pointer counterpart.
    #[allow(
        clippy::too_many_lines,
        reason = "the retained tree is deliberately kept beside its Widget hook so every custom-painted control is auditable together"
    )]
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};

        let modes = Mode::all()
            .iter()
            .enumerate()
            .map(|(index, mode)| {
                AccessNode::new(Role::Tab)
                    .with_name(mode.label())
                    .with_action_target(format!("calculator.mode:{index}"))
                    .with_bounds(self.seg_rects.get(index).copied().unwrap_or(Rect::ZERO))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.model.mode == *mode),
                    )
            })
            .collect();
        let top_bar = AccessNode::new(Role::Toolbar)
            .with_name("Calculator controls")
            // Its siblings include the keyed Evaluate control; key this
            // structural synthetic node too so AT identities never mix
            // ordinal and durable addressing at the same level.
            .with_hashed_key("calculator.controls")
            .with_children(vec![
                AccessNode::new(Role::TabBar)
                    .with_name("Mode")
                    // The tabs themselves carry action-derived keys.
                    .with_hashed_key("calculator.mode-tabs")
                    .with_children(modes),
                AccessNode::new(Role::ToggleButton)
                    .with_name("Radians")
                    .with_action_target("calculator.angle:radians")
                    .with_bounds(self.rad_rect)
                    .with_state(AccessState::new().focusable(true).checked(!self.model.deg)),
                AccessNode::new(Role::ToggleButton)
                    .with_name("Degrees")
                    .with_action_target("calculator.angle:degrees")
                    .with_bounds(self.deg_rect)
                    .with_state(AccessState::new().focusable(true).checked(self.model.deg)),
            ]);
        let mut children = vec![top_bar];
        match self.model.mode {
            Mode::Calc => {
                let mut controls = Vec::new();
                let mut display = AccessNode::new(Role::TextInput)
                    .with_name("Expression")
                    .with_value(self.model.expr.clone())
                    .with_description("Type an expression, then activate Evaluate or press Enter.")
                    .with_action_target("calculator.expression")
                    .with_state(AccessState::new().focusable(true))
                    .with_hashed_key("calculator.display");
                if !self.model.expr.trim().is_empty() {
                    let outcome = match engine::evaluate(&self.model.expr, &self.model.ctx()) {
                        Ok(value) => format!("Result {}", engine::format_number(value)),
                        Err(error) => format!("Invalid expression: {error}"),
                    };
                    display = display.with_children(vec![
                        AccessNode::new(Role::StaticText)
                            .with_name(outcome)
                            .with_live(Politeness::Polite)
                            .with_hashed_key("calculator.display.outcome"),
                    ]);
                }
                children.push(display);

                controls.push(
                    AccessNode::new(Role::Button)
                        .with_name(if self.calc.sidebar_open() {
                            "Hide history"
                        } else {
                            "Show history"
                        })
                        .with_action_target("calculator.history.toggle")
                        .with_bounds(self.calc.toggle_rect()),
                );
                for (index, key) in MEM_KEYS.iter().enumerate() {
                    let mut node = AccessNode::new(Role::Button)
                        .with_name(key_accessible_name(key))
                        .with_action_target(format!("calculator.memory:{index}"))
                        .with_state(AccessState::new().focusable(true));
                    if let Some(bounds) = self.calc.mem_rect(index) {
                        node = node.with_bounds(bounds);
                    }
                    controls.push(node);
                }
                children.push(
                    AccessNode::new(Role::Toolbar)
                        .with_name("Calculator controls")
                        .with_hashed_key("calculator.keypad-controls")
                        .with_children(controls),
                );

                let keys = self
                    .calc
                    .keys()
                    .iter()
                    .enumerate()
                    .map(|(index, key)| {
                        let mut node = AccessNode::new(Role::Button)
                            .with_name(key_accessible_name(key))
                            .with_action_target(format!("calculator.key:{index}"))
                            .with_state(AccessState::new().focusable(true));
                        if let Some(bounds) = self.calc.key_rect(index) {
                            node = node.with_bounds(bounds);
                        }
                        node
                    })
                    .collect();
                children.push(
                    AccessNode::new(Role::Group)
                        .with_name("Scientific keypad")
                        .with_description(
                            "Use the calculator keys, or type an expression directly.",
                        )
                        .with_hashed_key("calculator.keypad")
                        .with_children(keys),
                );

                if self.calc.sidebar_open() {
                    let mut history = Vec::new();
                    if !self.model.history.is_empty() {
                        history.push(
                            AccessNode::new(Role::Button)
                                .with_name("Clear calculation history")
                                .with_action_target("calculator.history.clear")
                                .with_state(AccessState::new().focusable(true))
                                .with_bounds(self.calc.clear_history_rect()),
                        );
                    }
                    for (index, entry) in self.model.history.iter().enumerate() {
                        let mut item = AccessNode::new(Role::ListItem)
                            .with_name(format!("{} equals {}", entry.expr, entry.result))
                            .with_action_target(format!("calculator.history.recall:{index}"))
                            .with_position(index + 1, self.model.history.len())
                            .with_state(AccessState::new().focusable(true));
                        if let Some(bounds) = self.calc.history_row_rect(index) {
                            item = item.with_bounds(bounds);
                        }
                        history.push(item);
                    }
                    children.push(
                        AccessNode::new(Role::List)
                            .with_name("Calculation history")
                            .with_hashed_key("calculator.history")
                            .with_children(history),
                    );
                }
            }
            Mode::Graph2d => children.push(self.graph_accessibility(GraphKind::Curve2d)),
            Mode::Graph3d => children.push(self.graph_accessibility(GraphKind::Surface3d)),
        }
        Some(
            AccessNode::new(Role::Group)
                .with_name("Calculator")
                .with_value(if self.model.expr.is_empty() {
                    "Empty expression".to_string()
                } else {
                    self.model.expr.clone()
                })
                .with_description("Calculator and graphing workspace")
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.host_focused),
                )
                .with_children(children),
        )
    }
    #[allow(
        clippy::too_many_lines,
        reason = "all custom-painted Calculator controls deliberately share one semantic activation router, preventing assistive technology from taking a different mutation path"
    )]
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action;
        let (state, _) = ui::doc::document(state, path);
        // Assistive actions bypass `handle_event`; they may change anything.
        self.dirty = true;
        match target {
            "calculator.mode:0" if matches!(action, Action::Activate) => {
                self.set_mode_in_document(Mode::Calc, state);
            }
            "calculator.mode:1" if matches!(action, Action::Activate) => {
                self.set_mode_in_document(Mode::Graph2d, state);
            }
            "calculator.mode:2" if matches!(action, Action::Activate) => {
                self.set_mode_in_document(Mode::Graph3d, state);
            }
            "calculator.angle:radians"
                if matches!(action, Action::Activate | Action::ToggleCheck) =>
            {
                self.set_degrees(false);
            }
            "calculator.angle:degrees"
                if matches!(action, Action::Activate | Action::ToggleCheck) =>
            {
                self.set_degrees(true);
            }
            "calculator.history.toggle"
                if matches!(action, Action::Activate | Action::ToggleCheck) =>
            {
                self.calc.toggle_sidebar();
            }
            "calculator.history.clear"
                if matches!(action, Action::Activate) && !self.model.history.is_empty() =>
            {
                self.model.history.clear();
                self.perform_action(
                    state,
                    &actions::clear_history_ids_action(crate::doc::visible_ids(
                        state,
                        crate::doc::HISTORY,
                    )),
                );
                self.model_dirty = true;
            }
            "calculator.expression" if matches!(action, Action::SetValue(_)) => {
                let Action::SetValue(expr) = action else {
                    return false;
                };
                self.model.expr.clone_from(expr);
                self.model.after_equals = false;
                self.model_dirty = true;
            }
            "calculator.evaluate"
                if matches!(action, Action::Activate) && self.model.mode == Mode::Calc =>
            {
                self.commit_equals(state);
            }
            _ if target.starts_with("calculator.key:") && matches!(action, Action::Activate) => {
                let Some(index) = target
                    .rsplit(':')
                    .next()
                    .and_then(|i| i.parse::<usize>().ok())
                else {
                    return false;
                };
                let Some(key) = self.calc.keys().get(index).cloned() else {
                    return false;
                };
                if matches!(key, model::Key::Equals) {
                    self.commit_equals(state);
                } else {
                    model::apply_key(&mut self.model, &key);
                    self.model_dirty = true;
                }
            }
            _ if target.starts_with("calculator.memory:") && matches!(action, Action::Activate) => {
                let Some(index) = target
                    .rsplit(':')
                    .next()
                    .and_then(|i| i.parse::<usize>().ok())
                else {
                    return false;
                };
                let Some(key) = MEM_KEYS.get(index).cloned() else {
                    return false;
                };
                model::apply_key(&mut self.model, &key);
                self.model_dirty = true;
            }
            _ if target.starts_with("calculator.history.recall:")
                && matches!(action, Action::Activate) =>
            {
                let Some(index) = target
                    .rsplit(':')
                    .next()
                    .and_then(|i| i.parse::<usize>().ok())
                else {
                    return false;
                };
                let Some(entry) = self.model.history.get(index) else {
                    return false;
                };
                self.model.expr = entry.expr.clone();
                self.model.after_equals = false;
                self.model_dirty = true;
            }
            _ if target.starts_with("calculator.graph.") => {
                return self.perform_graph_semantic_action(target, action, state);
            }
            _ => return false,
        }
        true
    }
    /// Keep the host pumping frames while any frame-driven animation is in
    /// flight: the history drawer's eased reveal, or a smooth graph zoom easing
    /// toward its target. Without this the animation would paint one frame and
    /// then freeze until the next unrelated event. Gated by the active mode so a
    /// transition left mid-flight by a mode switch can't pump frames forever
    /// (only the visible screen's `draw` advances its own animation).
    fn needs_animation_frame(&self) -> bool {
        match self.model.mode {
            Mode::Calc => self.calc.drawer_animating(),
            Mode::Graph2d => self.g2.zoom_active(),
            Mode::Graph3d => self.g3.zoom_active(),
        }
    }

    fn test_id(&self) -> Option<&str> {
        Some("calculator_app")
    }

    fn get_rect(&self) -> Rect {
        self.area
    }
    /// One self-drawn taffy leaf filling its tile; controls are positioned
    /// inside `self.area` by `relayout`, so only the first layout is a size
    /// change.
    fn is_layout_dirty(&self) -> bool {
        self.area == Rect::ZERO
    }
}

impl ui::undo::UndoPoints for CalculatorApp {
    /// Seal a burst of edits into one undo checkpoint after a short pause, so
    /// Ctrl+Z steps back per token/pause rather than per keystroke. The App
    /// calls [`Widget::needs_commit`] → `commit_action` (a `ui::State` snapshot)
    /// → [`Widget::on_commit`]; undo/redo is entirely `ui::State`'s, not a
    /// bespoke stack.
    fn needs_commit(&self) -> bool {
        self.last_edit_time
            .is_some_and(|t| t.elapsed() >= COMMIT_COALESCE)
    }
    fn on_commit(&mut self) {
        self.last_edit_time = None;
    }
}

impl ui::resources::PublishesResources for CalculatorApp {
    /// Publish the calculator's Ctrl+K resource tree (history entries + saved
    /// graph formulas), via the [`ResourceProvider`](tilo_actions::ResourceProvider)
    /// impl in [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        // The framework root publishes this document. The widget tree would
        // be a second door.
        if self.actions.is_hosted() {
            return Vec::new();
        }
        let state = crate::resource::Calculator::read(doc).unwrap_or_default();
        vec![ui::tilo_resource::publish::<crate::resource::Calculator>(
            crate::resource::Calculator::KIND,
            "Calculator",
            &state,
        )]
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        fn find(resources: &[tilo_actions::Resource], id: &str) -> Option<tilo_actions::Resource> {
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
        if self.actions.is_hosted() && id == crate::resource::Calculator::KIND {
            return None;
        }
        let state = crate::resource::Calculator::read(doc).unwrap_or_default();
        let root = ui::tilo_resource::publish::<crate::resource::Calculator>(
            crate::resource::Calculator::KIND,
            "Calculator",
            &state,
        );
        find(&[root], id)
    }
}

impl ui::history_mode::capability::DocumentHistory for CalculatorApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

impl ui::undo::CommittedActions for CalculatorApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its history / graph action, so it records one undo boundary for the whole
    /// gesture (see `Self::perform_action`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl ui::widget::AppWidget for CalculatorApp {
    fn app_kind(&self) -> ui::widget::AppKind {
        // The hosted glyph vocabulary has no Utility variant. Calculator is
        // an interactive expression editor, which is the closest existing kind.
        ui::widget::AppKind::Editor
    }

    fn app_title(&self) -> &'static str {
        "Calculator"
    }

    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Named("calculator".into())
    }

    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        None
    }
}

impl CalculatorApp {
    /// Retained semantics for the custom-painted graph editor panels.
    fn graph_accessibility(&self, kind: GraphKind) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let (title, slug, rows, open, toggle_bounds, add_bounds) = match kind {
            GraphKind::Curve2d => (
                "Functions",
                "2d",
                &self.model.funcs,
                self.g2.panel_open(),
                self.g2.toggle_rect(),
                self.g2.add_rect(),
            ),
            GraphKind::Surface3d => (
                "Surfaces",
                "3d",
                &self.model.surfaces,
                self.g3.panel_open(),
                self.g3.toggle_rect(),
                self.g3.add_rect(),
            ),
        };
        let mut children = Vec::new();
        if let Some(bounds) = toggle_bounds {
            children.push(
                AccessNode::new(Role::ToggleButton)
                    .with_name(format!("Show {title}"))
                    .with_action_target(format!("calculator.graph.panel:{slug}"))
                    .with_state(AccessState::new().focusable(true).checked(open))
                    .with_bounds(bounds),
            );
        }
        if open {
            for (index, row) in rows.iter().enumerate() {
                let (field_bounds, visible_bounds, delete_bounds) = match kind {
                    GraphKind::Curve2d => (
                        self.g2.field_rect(index),
                        self.g2.visibility_rect(index),
                        self.g2.delete_rect(index),
                    ),
                    GraphKind::Surface3d => (
                        self.g3.field_rect(index),
                        self.g3.visibility_rect(index),
                        self.g3.delete_rect(index),
                    ),
                };
                let mut field = AccessNode::new(Role::TextInput)
                    .with_name(format!(
                        "{} {} formula",
                        title.trim_end_matches('s'),
                        index + 1
                    ))
                    .with_value(row.expr.clone())
                    .with_action_target(format!("calculator.graph.field:{slug}:{index}"))
                    .with_state(AccessState::new().focusable(true));
                if let Some(bounds) = field_bounds {
                    field = field.with_bounds(bounds);
                }
                let mut visible = AccessNode::new(Role::ToggleButton)
                    .with_name(format!(
                        "Show {} {}",
                        title.trim_end_matches('s'),
                        index + 1
                    ))
                    .with_action_target(format!("calculator.graph.visible:{slug}:{index}"))
                    .with_state(AccessState::new().focusable(true).checked(row.visible));
                if let Some(bounds) = visible_bounds {
                    visible = visible.with_bounds(bounds);
                }
                let mut delete = AccessNode::new(Role::Button)
                    .with_name(format!(
                        "Delete {} {}",
                        title.trim_end_matches('s'),
                        index + 1
                    ))
                    .with_action_target(format!("calculator.graph.delete:{slug}:{index}"))
                    .with_description("Permanently removes this saved formula.")
                    .with_state(AccessState::new().focusable(true));
                if let Some(bounds) = delete_bounds {
                    delete = delete.with_bounds(bounds);
                }
                children.push(
                    AccessNode::new(Role::ListItem)
                        .with_name(format!("{} {}", title.trim_end_matches('s'), index + 1))
                        .with_hashed_key(format!("calculator.graph.row:{slug}:{index}"))
                        .with_position(index + 1, rows.len())
                        .with_children(vec![field, visible, delete]),
                );
            }
            let mut add = AccessNode::new(Role::Button)
                .with_name(format!("Add {}", title.trim_end_matches('s')))
                .with_action_target(format!("calculator.graph.add:{slug}"))
                .with_state(AccessState::new().focusable(true));
            if let Some(bounds) = add_bounds {
                add = add.with_bounds(bounds);
            }
            children.push(add);
        }
        AccessNode::new(Role::TabPanel)
            .with_name(title)
            .with_hashed_key(format!("calculator.graph:{slug}"))
            .with_children(children)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one parser keeps every retained graph control on the same action-dispatch path"
    )]
    fn perform_graph_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        doc: &mut AutoCommit,
    ) -> bool {
        use ui::a11y::Action;
        let Some(rest) = target.strip_prefix("calculator.graph.") else {
            return false;
        };
        let parts: Vec<_> = rest.split(':').collect();
        let Some(kind) = parts.get(1).and_then(|slug| match *slug {
            "2d" => Some(GraphKind::Curve2d),
            "3d" => Some(GraphKind::Surface3d),
            _ => None,
        }) else {
            return false;
        };
        match (
            parts.first().copied(),
            parts.get(2).and_then(|s| s.parse::<usize>().ok()),
            action,
        ) {
            (Some("panel"), None, Action::Activate | Action::ToggleCheck) => match kind {
                GraphKind::Curve2d => self.g2.toggle_panel(),
                GraphKind::Surface3d => self.g3.toggle_panel(),
            },
            (Some("add"), None, Action::Activate) => {
                match kind {
                    GraphKind::Curve2d => {
                        self.model.funcs.push(PlotFn {
                            expr: String::new(),
                            visible: true,
                        });
                        self.model.active_fn = self.model.funcs.len() - 1;
                    }
                    GraphKind::Surface3d => {
                        self.model.surfaces.push(PlotFn {
                            expr: String::new(),
                            visible: true,
                        });
                        self.model.active_surface = self.model.surfaces.len() - 1;
                    }
                }
                self.perform_action(doc, &actions::add_graph_action(kind, ""));
                self.model_dirty = true;
            }
            (Some("visible"), Some(index), Action::Activate | Action::ToggleCheck) => {
                let changed = match kind {
                    GraphKind::Curve2d => self
                        .model
                        .funcs
                        .get_mut(index)
                        .map(|row| row.visible = !row.visible),
                    GraphKind::Surface3d => self
                        .model
                        .surfaces
                        .get_mut(index)
                        .map(|row| row.visible = !row.visible),
                };
                if changed.is_none() {
                    return false;
                }
                let key = match kind {
                    GraphKind::Curve2d => crate::doc::FUNCS,
                    GraphKind::Surface3d => crate::doc::SURFACES,
                };
                let action = crate::doc::row_id_at(doc, key, index).map_or_else(
                    || actions::toggle_graph_visible_action(kind, index),
                    |id| actions::toggle_graph_visible_action(kind, id.as_str()),
                );
                self.perform_action(doc, &action);
                self.model_dirty = true;
            }
            (Some("delete"), Some(index), Action::Activate) => {
                let exists = match kind {
                    GraphKind::Curve2d => index < self.model.funcs.len(),
                    GraphKind::Surface3d => index < self.model.surfaces.len(),
                };
                if !exists {
                    return false;
                }
                self.delete_graph_row(kind, index, doc);
            }
            (Some("field"), Some(index), Action::SetValue(expr)) => {
                let changed = match kind {
                    GraphKind::Curve2d => self
                        .model
                        .funcs
                        .get(index)
                        .is_some_and(|row| row.expr != *expr),
                    GraphKind::Surface3d => self
                        .model
                        .surfaces
                        .get(index)
                        .is_some_and(|row| row.expr != *expr),
                };
                if !changed {
                    return true;
                }
                match kind {
                    GraphKind::Curve2d => {
                        if let Some(row) = self.model.funcs.get_mut(index) {
                            row.expr.clone_from(expr);
                        }
                    }
                    GraphKind::Surface3d => {
                        if let Some(row) = self.model.surfaces.get_mut(index) {
                            row.expr.clone_from(expr);
                        }
                    }
                }
                let key = match kind {
                    GraphKind::Curve2d => crate::doc::FUNCS,
                    GraphKind::Surface3d => crate::doc::SURFACES,
                };
                let action = crate::doc::row_id_at(doc, key, index).map_or_else(
                    || actions::set_graph_expr_action(kind, index, expr),
                    |id| actions::set_graph_expr_action(kind, id.as_str(), expr),
                );
                self.perform_action(doc, &action);
                self.model_dirty = true;
            }
            (Some("field"), Some(index), Action::Activate | Action::Focus) => match kind {
                GraphKind::Curve2d if index < self.model.funcs.len() => {
                    self.model.active_fn = index;
                }
                GraphKind::Surface3d if index < self.model.surfaces.len() => {
                    self.model.active_surface = index;
                }
                _ => return false,
            },
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyboard(key: WKey) -> WindowEvent {
        let text = if let WKey::Character(text) = &key {
            Some(text.clone())
        } else {
            None
        };
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: winit::event::KeyEvent::synthetic(
                winit::event::RawKeyEvent {
                    physical_key: winit::keyboard::PhysicalKey::Unidentified(
                        winit::keyboard::NativeKeyCode::Unidentified,
                    ),
                    state: ElementState::Pressed,
                },
                key.clone(),
                text.clone(),
                winit::keyboard::KeyLocation::Standard,
                false,
                text,
                key,
            ),
            is_synthetic: false,
        }
    }

    #[test]
    fn keyboard_and_ime_only_mutate_the_host_focused_calculator() {
        let mut app = CalculatorApp::new();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);

        assert!(
            !send(&mut app, &mut state, &keyboard(WKey::Character("2".into())),),
            "an unfocused Calculator must leave keyboard input for the focused sibling"
        );
        assert!(app.model.expr.is_empty());
        assert!(
            !send(
                &mut app,
                &mut state,
                &WindowEvent::Ime(winit::event::Ime::Commit("3".into())),
            ),
            "an unfocused Calculator must leave IME commits for the focused sibling"
        );
        assert!(app.model.expr.is_empty());
        assert!(
            !send(
                &mut app,
                &mut state,
                &WindowEvent::Ime(winit::event::Ime::Preedit("3".into(), Some((1, 1)))),
            ),
            "an unfocused Calculator must not capture an IME preedit"
        );

        app.set_focus(true);
        assert!(send(
            &mut app,
            &mut state,
            &keyboard(WKey::Character("2".into())),
        ));
        assert_eq!(app.model.expr, "2");

        app.set_mode(Mode::Graph2d);
        app.model.funcs = vec![PlotFn {
            expr: String::new(),
            visible: true,
        }];
        app.model.active_fn = 0;
        assert!(send(
            &mut app,
            &mut state,
            &keyboard(WKey::Character("x".into())),
        ));
        assert_eq!(app.model.funcs[0].expr, "x");

        app.set_mode(Mode::Graph3d);
        app.model.surfaces = vec![PlotFn {
            expr: "x+y".into(),
            visible: true,
        }];
        app.model.active_surface = 0;
        assert!(send(
            &mut app,
            &mut state,
            &keyboard(WKey::Named(NamedKey::Backspace)),
        ));
        assert_eq!(app.model.surfaces[0].expr, "x+");

        app.set_focus(false);
        let graph_expression = app.model.surfaces[0].expr.clone();
        assert!(
            !send(&mut app, &mut state, &keyboard(WKey::Character("y".into())),),
            "an unfocused Calculator must not consume graph keyboard edits"
        );
        assert_eq!(app.model.surfaces[0].expr, graph_expression);
    }

    #[test]
    fn power_input_physical_keys_evaluates_once() {
        let mut app = CalculatorApp::new();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        app.set_focus(true);
        for text in ["2", "^", "2"] {
            send(
                &mut app,
                &mut state,
                &keyboard(WKey::Character(text.into())),
            );
        }
        assert_eq!(app.model.expr, "2^2");
        send(
            &mut app,
            &mut state,
            &keyboard(WKey::Named(NamedKey::Enter)),
        );
        assert_eq!(app.model.ans, 4.0);
        assert_eq!(app.model.history.len(), 1);
    }

    #[test]
    fn power_input_dead_key_commit_preserves_caret() {
        use winit::event::Ime;
        let mut app = CalculatorApp::new();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        app.set_focus(true);
        send(&mut app, &mut state, &keyboard(WKey::Character("2".into())));
        send(&mut app, &mut state, &keyboard(WKey::Dead(Some('^'))));
        send(
            &mut app,
            &mut state,
            &WindowEvent::Ime(Ime::Preedit("^".into(), Some((1, 1)))),
        );
        assert_eq!(app.model.expr, "2", "preedit must not insert an operator");
        send(
            &mut app,
            &mut state,
            &WindowEvent::Ime(Ime::Commit("^".into())),
        );
        send(&mut app, &mut state, &keyboard(WKey::Character("2".into())));
        assert_eq!(app.model.expr, "2^2");
        send(
            &mut app,
            &mut state,
            &keyboard(WKey::Named(NamedKey::Enter)),
        );
        assert_eq!(app.model.ans, 4.0);
        assert_eq!(app.model.history.len(), 1);
    }

    #[test]
    fn power_input_browser_commits_evaluate_once() {
        let mut app = CalculatorApp::new();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        app.set_focus(true);
        for text in ["2", "^", "2"] {
            send(
                &mut app,
                &mut state,
                &WindowEvent::Ime(winit::event::Ime::Commit(text.into())),
            );
        }
        assert_eq!(app.model.expr, "2^2");
        send(
            &mut app,
            &mut state,
            &keyboard(WKey::Named(NamedKey::Enter)),
        );
        assert_eq!(app.model.ans, 4.0);
        assert_eq!(app.model.history.len(), 1);
    }

    #[test]
    fn power_input_focused_calculator_enables_browser_text_editor() {
        let mut app = CalculatorApp::new();
        app.area = Rect::new(0.0, 0.0, 800.0, 600.0);
        app.relayout();
        app.layout_active();
        assert!(app.ime_cursor_rect().is_none());
        app.set_focus(true);
        let caret = app
            .ime_cursor_rect()
            .expect("text entry must enable the browser editor and platform IME");
        assert!(caret.height() > 0.0 && app.body.contains(caret.center()));
        assert_eq!(app.ime_purpose(), Some(winit::window::ImePurpose::Normal));
        app.set_focus(false);
        assert!(app.ime_cursor_rect().is_none());
        assert_eq!(app.ime_purpose(), None);
    }

    #[test]
    fn power_input_paste_shortcuts_evaluate_once() {
        for primary in [
            winit::keyboard::ModifiersState::CONTROL,
            winit::keyboard::ModifiersState::SUPER,
        ] {
            let mut app = CalculatorApp::new();
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root);
            app.set_focus(true);
            app.on_key_with_paste(
                &WKey::Character("v".into()),
                primary.into(),
                &mut state.doc,
                || Some("2^2".into()),
            );
            assert_eq!(app.model.expr, "2^2");
            send(
                &mut app,
                &mut state,
                &keyboard(WKey::Named(NamedKey::Enter)),
            );
            assert_eq!(app.model.ans, 4.0);
            assert_eq!(app.model.history.len(), 1);
        }
    }

    /// A history row's published `open` action recalls exactly that row: via
    /// the headless host for a local viewer, and via the resource operation by
    /// stable identity (surviving index shifts, refusing deleted rows). Recall
    /// is presentation only. Each phase builds a fresh seeded app + document.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn history_recall_by_published_action_uses_stable_identity() {
        // phase history_recall_through_headless_host_is_available_to_local_viewer
        {
            let mut app = CalculatorApp::seeded();
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root);
            let id = crate::doc::row_id_at(&state.doc, crate::doc::HISTORY, 1).unwrap();
            let expression = crate::doc::load(&state.doc).unwrap().history[1]
                .expr
                .clone();
            let resources = ui::resources::resources(&app, &state.doc);
            let row = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == format!("calculator.history_entry:{id}"))
                .unwrap();
            let binding = tilo_actions::ActionBinding::unit_for(row, "open").unwrap();
            app.set_mode(Mode::Graph3d);
            let before = state.doc.get_heads();
            // Exercise the same published binding and host admission used by a
            // search result, rather than calling the app's handler directly.
            let root = ui::widget::Column::new().push(Box::new(app)).push(Box::new(
                ui::widget::Button::new("recall", "Recall", None).on_action(binding),
            ));
            let mut harness = ui::headless::FrameHarness::new(state, Box::new(root), (800, 600));
            harness.set_permission_ctx(ui::permissions::PermissionContext::from_capabilities(
                ui::permissions::Actor::person(
                    Some(ui::permissions::Role::Viewer),
                    ui::permissions::LOCAL_USER,
                ),
                vec![],
            ));
            harness
                .perform_named("recall", &ui::a11y::Action::Activate)
                .unwrap();
            harness
                .expect(
                    &ui::headless::SemanticExpectation::named("calculator_app").value(expression),
                )
                .unwrap();
            assert_eq!(
                harness.state_mut().doc.get_heads(),
                before,
                "headless recall: document unchanged"
            );
        }

        // phase search_recall_uses_stable_history_identity_and_refuses_deleted_entries
        {
            let mut app = CalculatorApp::seeded();
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root);
            let id = crate::doc::row_id_at(&state.doc, crate::doc::HISTORY, 1).unwrap();
            let expression = crate::doc::load(&state.doc).unwrap().history[1]
                .expr
                .clone();
            let resources = ui::resources::resources(&app, &state.doc);
            let row = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == format!("calculator.history_entry:{id}"))
                .unwrap();
            let open = row.actions.iter().find(|spec| spec.id == "open").unwrap();
            assert_eq!(
                open.kind,
                tilo_actions::OperationKind::View,
                "search recall: open is a View"
            );
            let tilo_actions::ActionInput::Immediate(encoded) = &open.input else {
                panic!("recall needs no prompt");
            };
            // A new calculation moves every old positional index. The saved search
            // result must still recall exactly the document row it advertised.
            app.perform_action(
                &mut state.doc,
                &actions::commit_history_action("999+1", "1000"),
            );
            let heads = state.doc.get_heads();
            app.set_mode(Mode::Graph3d);
            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        open.kind,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut state.doc,
                    &state.root
                )
                .map(|started| started.is_some())
                .unwrap(),
                "search recall: first invoke handled"
            );
            assert_eq!(app.model.mode, Mode::Calc, "search recall: back in Calc");
            assert_eq!(app.model.expr, expression, "search recall: expression");
            assert_eq!(state.doc.get_heads(), heads, "recall is presentation only");
            app.model_dirty = false;
            app.model.expr = "uncommitted input".into();
            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        open.kind,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut state.doc,
                    &state.root
                )
                .map(|started| started.is_some())
                .unwrap(),
                "search recall: second invoke handled"
            );
            assert_eq!(
                app.model.expr, expression,
                "search recall: overwrites uncommitted input"
            );
            assert!(
                app.model_dirty,
                "recalling while already in Calc still repaints"
            );
            app.perform_action(&mut state.doc, &actions::delete_history_entry_action(&id));
            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        open.kind,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut state.doc,
                    &state.root
                )
                .map(|started| started.is_some())
                .is_err(),
                "search recall: deleted row is refused"
            );
            assert_eq!(
                app.model.expr, expression,
                "stale search must not recall a different row"
            );
        }
    }

    /// Launch and mode-switch state: the live app boots empty in Calc, the
    /// seeded constructor is populated, `set_mode` marks dirty, and entering an
    /// empty graph mode seeds an editable starter. Fresh app per phase.
    #[test]
    fn launch_state_and_mode_switch_seeding() {
        // phase live_launch_is_empty_calc_mode
        {
            // The live app must boot to a blank slate in Calculator mode — no
            // fabricated plots / history / memory / expression.
            let app = CalculatorApp::new();
            assert_eq!(app.model.mode, Mode::Calc, "live launch: Calc mode");
            assert!(
                app.model.expr.is_empty(),
                "live launch must have an empty expression"
            );
            assert!(
                app.model.history.is_empty(),
                "live launch must have no history"
            );
            assert!(
                app.model.funcs.is_empty(),
                "live launch must have no pre-plotted functions"
            );
            assert_eq!(app.model.memory, 0.0, "live launch must have empty memory");
            assert_eq!(app.model.ans, 0.0, "live launch: ans is 0");
        }
        // phase seeded_launch_has_lived_in_content
        {
            // The demo/shots constructor keeps the populated content.
            let app = CalculatorApp::seeded();
            assert!(!app.model.funcs.is_empty(), "seeded: funcs present");
            assert!(!app.model.history.is_empty(), "seeded: history present");
        }
        // phase set_mode_marks_dirty
        {
            let mut app = CalculatorApp::new();
            app.set_mode(Mode::Graph2d);
            assert_eq!(app.model.mode, Mode::Graph2d, "set_mode: mode");
            assert!(app.model_dirty, "set_mode: marks dirty");
        }
        // phase entering_empty_graph_modes_seeds_a_starter
        {
            // Switching an empty live app into a graph mode gives it an editable
            // starter (empty 2D row / example surface), not a blank void.
            let mut app = CalculatorApp::new();
            app.set_mode(Mode::Graph2d);
            assert_eq!(app.model.funcs.len(), 1, "Graph 2D seeds one empty row");
            assert!(
                app.model.funcs[0].expr.is_empty(),
                "Graph 2D starter row is empty"
            );
            app.set_mode(Mode::Graph3d);
            assert_eq!(
                app.model.surfaces.len(),
                1,
                "Graph 3D seeds one starter surface"
            );
            assert!(
                !app.model.surfaces[0].expr.trim().is_empty(),
                "Graph 3D starter is a non-empty example surface"
            );
        }
    }

    /// The semantic tree exposes the topbar, keypad, expression and graph
    /// formula controls, and activating/setting them through
    /// `perform_semantic_action` drives the model. Fresh app per phase.
    #[test]
    #[allow(
        clippy::items_after_statements,
        reason = "the local sibling-key assertion reads most clearly next to the tree it validates"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn accessibility_exposes_and_activates_calculator_controls() {
        use ui::a11y::Action;
        use ui::widget::Widget as _;

        // phase accessibility_exposes_and_activates_topbar_controls
        {
            let mut app = CalculatorApp::new();
            let tree = app.accessibility().expect("calculator semantics");
            assert_eq!(
                tree.name.as_deref(),
                Some("Calculator"),
                "topbar: root name"
            );
            let mode_bar = &tree.children[0].children[0];
            fn siblings_are_consistently_keyed(nodes: &[ui::a11y::AccessNode]) -> bool {
                nodes.is_empty()
                    || nodes.iter().all(|node| node.key.is_some())
                    || nodes.iter().all(|node| node.key.is_none())
            }
            assert!(
                siblings_are_consistently_keyed(&tree.children)
                    && siblings_are_consistently_keyed(&tree.children[0].children)
                    && siblings_are_consistently_keyed(&mode_bar.children),
                "synthetic Calculator siblings must not mix stable and ordinal keys"
            );
            assert!(
                mode_bar.children.iter().any(|node| {
                    node.action_target.as_deref() == Some("calculator.mode:1")
                        && node.name.as_deref() == Some("Graph 2D")
                }),
                "topbar: Graph 2D mode control exposed"
            );

            let mut doc = AutoCommit::new();
            assert!(
                app.perform_semantic_action(
                    "calculator.mode:1",
                    &Action::Activate,
                    &mut doc,
                    &automerge::ROOT,
                ),
                "topbar: mode activate handled"
            );
            assert_eq!(app.model.mode, Mode::Graph2d, "topbar: mode switched");
            assert!(
                app.perform_semantic_action(
                    "calculator.angle:degrees",
                    &Action::ToggleCheck,
                    &mut doc,
                    &automerge::ROOT,
                ),
                "topbar: angle toggle handled"
            );
            assert!(app.model.deg, "topbar: degrees on");
        }

        // phase accessibility_exposes_keypad_and_graph_formula_controls
        {
            let mut app = CalculatorApp::new();
            app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
            app.relayout();
            app.layout_active();
            let tree = app.accessibility().expect("calculator semantics");
            let keypad = tree
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Scientific keypad"))
                .expect("all painted keypad keys are exposed");
            let expression = tree
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Expression"))
                .expect("expression is a semantic text input");
            assert_eq!(
                expression.role,
                ui::a11y::Role::TextInput,
                "keypad: expression role"
            );
            let seven = keypad
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Calculator key 7"))
                .expect("digit key is actionable");
            let mut doc = AutoCommit::new();
            assert!(
                app.perform_semantic_action(
                    seven.action_target.as_deref().unwrap_or_default(),
                    &Action::Activate,
                    &mut doc,
                    &automerge::ROOT,
                ),
                "keypad: key 7 handled"
            );
            assert_eq!(app.model.expr, "7", "keypad: key 7 appended");
            assert!(
                app.perform_semantic_action(
                    expression.action_target.as_deref().unwrap_or_default(),
                    &Action::SetValue("sqrt(9)".into()),
                    &mut doc,
                    &automerge::ROOT,
                ),
                "keypad: expression SetValue handled"
            );
            assert_eq!(app.model.expr, "sqrt(9)", "keypad: expression set");

            app.set_mode(Mode::Graph2d);
            crate::doc::store(&mut doc, &app.model);
            app.layout_active();
            let graph = app.accessibility().expect("graph semantics");
            let panel = graph
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Functions"))
                .expect("functions panel is exposed");
            let formula = panel
                .children
                .iter()
                .flat_map(|node| node.children.iter())
                .find(|node| node.role == ui::a11y::Role::TextInput)
                .expect("formula editor is a text input");
            assert!(
                app.perform_semantic_action(
                    formula.action_target.as_deref().unwrap_or_default(),
                    &Action::SetValue("2*x".into()),
                    &mut doc,
                    &automerge::ROOT,
                ),
                "graph: formula SetValue handled"
            );
            assert_eq!(app.model.funcs[0].expr, "2*x", "graph: formula set");
        }
    }

    /// Topbar geometry: the mode switcher and RAD/DEG toggle never clip or
    /// collide down to 400 px, reflow between breakpoints on resize, and widen
    /// to seat their measured labels at a large text scale (1× unchanged).
    #[test]
    fn topbar_layout_reflows_and_seats_labels_without_overlap() {
        // phase topbar_never_overlaps_or_clips_down_to_400
        // The mode switcher and RAD/DEG toggle must reflow so they neither clip
        // past the window edge nor collide, at every width down to 400 px.
        for w in [400.0, 460.0, 520.0, 560.0, 620.0, 680.0, 800.0, 1280.0] {
            let mut app = CalculatorApp::seeded();
            app.area = Rect::new(0.0, 0.0, w, 720.0);
            app.relayout();
            for (i, r) in app.seg_rects.iter().enumerate() {
                assert!(
                    r.x0 >= app.area.x0 - 0.5 && r.x1 <= app.area.x1 + 0.5,
                    "mode segment {i} clipped the window edge at w={w}: {r:?}"
                );
            }
            assert!(
                app.seg_rects[2].x1 <= app.rad_rect.x0 + 0.5,
                "mode switcher overlaps the RAD/DEG toggle at w={w} \
                 (seg end {} vs toggle start {})",
                app.seg_rects[2].x1,
                app.rad_rect.x0
            );
            assert!(
                app.deg_rect.x1 <= app.area.x1 + 0.5,
                "RAD/DEG toggle clipped the window edge at w={w}"
            );
        }

        // phase topbar_controls_seat_their_measured_labels_at_large_text
        {
            // At an accessibility text scale the topbar's segments and RAD/DEG halves
            // widen to seat the labels the renderer actually paints — measured on the
            // production draw path — instead of keeping their 1× widths and letting
            // the labels run into each other.
            let mut app = CalculatorApp::seeded();
            app.area = Rect::new(0.0, 0.0, 1280.0, 720.0);
            app.relayout();
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let mut scene = Scene::new();
            app.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::empty(),
            );
            let labels = app.topbar_labels;
            assert!(
                labels.full > 0.0 && labels.toggle > 0.0,
                "labels measured: {labels:?}"
            );
            let seg_label = if app.seg_short {
                labels.short
            } else {
                labels.full
            };
            for (i, r) in app.seg_rects.iter().enumerate() {
                assert!(
                    r.width() + 0.5 >= seg_label + 2.0 * TopbarLabels::SEG_PAD_X,
                    "segment {i} {r:?} must seat its label ({seg_label}px) with padding"
                );
            }
            for (name, r) in [("RAD", app.rad_rect), ("DEG", app.deg_rect)] {
                assert!(
                    r.width() + 0.5 >= labels.toggle + 2.0 * TopbarLabels::TOGGLE_PAD_X,
                    "{name} half {r:?} must seat its label ({}px)",
                    labels.toggle
                );
            }
            assert!(
                app.seg_rects[2].x1 <= app.rad_rect.x0 + 0.5,
                "the widened switcher still clears the toggle: {} vs {}",
                app.seg_rects[2].x1,
                app.rad_rect.x0
            );
            // 1× keeps its authored geometry: the measured labels fit the ordinary
            // clamps, so nothing widens.
            let mut base = CalculatorApp::seeded();
            base.area = Rect::new(0.0, 0.0, 1280.0, 720.0);
            base.relayout();
            let before = (base.seg_rects, base.rad_rect, base.deg_rect);
            let mut tr1 = TextRenderer::new();
            base.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut tr1,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            assert_eq!(
                (base.seg_rects, base.rad_rect, base.deg_rect),
                before,
                "1× topbar geometry is unchanged by measurement"
            );
        }

        // phase topbar_reflows_on_resize_between_breakpoints
        {
            // Re-derive the layout from the CURRENT width every relayout: shrinking a
            // laid-out instance must switch to condensed mode labels, and
            // re-widening must restore them (not freeze at the first frame).
            let mut app = CalculatorApp::seeded();
            app.area = Rect::new(0.0, 0.0, 1280.0, 720.0);
            app.relayout();
            assert!(!app.seg_short, "wide: full mode labels");
            app.area = Rect::new(0.0, 0.0, 430.0, 720.0);
            app.relayout();
            assert!(app.seg_short, "narrow: condensed mode labels");
            app.area = Rect::new(0.0, 0.0, 1280.0, 720.0);
            app.relayout();
            assert!(!app.seg_short, "re-widening must restore the full labels");
        }
    }

    /// The real key path (`on_key`): Ctrl+digit switches mode, typing and
    /// Backspace edit the active 2D function / 3D surface, and Enter appends and
    /// focuses a fresh row. Fresh app + state per phase.
    #[test]
    fn keyboard_path_switches_mode_and_edits_graph_rows() {
        let none = winit::event::Modifiers::default();

        // phase ctrl_digit_switches_mode
        {
            let mut app = CalculatorApp::new();
            let mut st = ui::State::new();
            let ctrl = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
            app.on_key(&WKey::Character("3".into()), ctrl, &mut st.doc);
            assert_eq!(app.model.mode, Mode::Graph3d, "ctrl+3 switches to Graph 3D");
        }

        // phase escape_clears_then_leaves_an_empty_calculator_to_the_host
        {
            let mut app = CalculatorApp::new();
            let mut st = ui::State::new();
            let escape = WKey::Named(NamedKey::Escape);
            app.calc.snap_sidebar(true);
            assert!(!app.escape_is_idle(&escape), "Escape dismisses the drawer");
            app.on_key(&escape, none, &mut st.doc);
            assert!(!app.calc.sidebar_open());
            app.on_key(&WKey::Character("7".into()), none, &mut st.doc);
            assert!(!app.escape_is_idle(&escape), "Escape clears the expression");
            app.on_key(&escape, none, &mut st.doc);
            assert!(app.model.expr.is_empty());
            assert!(app.escape_is_idle(&escape), "nothing left for Escape to do");
            assert!(!app.escape_is_idle(&WKey::Named(NamedKey::Backspace)));
        }

        // phase typing_into_graph2d_field_updates_active_func
        {
            let mut app = CalculatorApp::new();
            let mut st = ui::State::new();
            app.set_mode(Mode::Graph2d);
            // start from a clean active row
            app.model.funcs = vec![crate::model::PlotFn {
                expr: String::new(),
                visible: true,
            }];
            app.model.active_fn = 0;
            for ch in ["2", "*", "x"] {
                app.on_key(&WKey::Character(ch.into()), none, &mut st.doc);
            }
            assert_eq!(app.model.funcs[0].expr, "2*x", "typing into 2D field");
        }

        // phase backspace_in_graph3d_field_edits_surface
        {
            let mut app = CalculatorApp::new();
            let mut st = ui::State::new();
            app.set_mode(Mode::Graph3d);
            app.model.surfaces = vec![PlotFn {
                expr: "x+y".into(),
                visible: true,
            }];
            app.model.active_surface = 0;
            // on_key_g3 resyncs the active field to the surface before editing.
            app.on_key(&WKey::Named(NamedKey::Backspace), none, &mut st.doc);
            assert_eq!(app.model.surfaces[0].expr, "x+", "backspace in 3D field");
        }

        // phase enter_in_graph2d_adds_and_focuses_a_new_function
        {
            // Real key path: after editing a function, Enter appends a fresh empty
            // row and focuses it (the "next expression" gesture).
            let mut app = CalculatorApp::new();
            let mut st = ui::State::new();
            app.set_mode(Mode::Graph2d);
            app.model.funcs = vec![PlotFn {
                expr: String::new(),
                visible: true,
            }];
            app.model.active_fn = 0;
            for ch in ["s", "i", "n", "(", "x", ")"] {
                app.on_key(&WKey::Character(ch.into()), none, &mut st.doc);
            }
            assert_eq!(app.model.funcs[0].expr, "sin(x)", "enter 2D: typed row");
            app.on_key(&WKey::Named(NamedKey::Enter), none, &mut st.doc);
            assert_eq!(
                app.model.funcs.len(),
                2,
                "Enter must add a new function row"
            );
            assert_eq!(app.model.active_fn, 1, "the new row must be focused");
            assert!(
                app.model.funcs[1].expr.is_empty(),
                "the new row starts empty"
            );
            assert_eq!(
                app.model.funcs[0].expr, "sin(x)",
                "the edited row is preserved"
            );
        }

        // phase enter_in_graph3d_adds_and_focuses_a_new_surface
        {
            let mut app = CalculatorApp::new();
            let mut st = ui::State::new();
            app.set_mode(Mode::Graph3d); // seeds one starter surface
            let start = app.model.surfaces.len();
            app.on_key(&WKey::Named(NamedKey::Enter), none, &mut st.doc);
            assert_eq!(
                app.model.surfaces.len(),
                start + 1,
                "Enter must add a new surface row"
            );
            assert_eq!(
                app.model.active_surface,
                app.model.surfaces.len() - 1,
                "the new surface is focused"
            );
            assert!(
                app.model.surfaces.last().unwrap().expr.is_empty(),
                "the new surface starts empty"
            );
        }
    }

    /// Formula state is CRDT-backed and undoable through the framework: undo/redo
    /// round-trips through `ui::State`'s snapshot stack, an external CRDT edit is
    /// adopted on sync, and an event-path edit arms and seals the coalesced undo
    /// commit. Fresh app + state per phase.
    #[test]
    fn formula_edits_sync_through_crdt_and_framework_undo() {
        use ui::undo::needs_commit;
        // phase undo_redo_of_formula_edit_routes_through_ui_state
        {
            // Undo/redo of a formula edit round-trips through `ui::State`'s snapshot
            // stack (not a bespoke calculator stack): typing checkpoints via
            // `commit_action`, Ctrl+Z restores the previous CRDT snapshot, and `sync`
            // adopts the restored document back into the on-screen model.
            use crate::doc;
            let mut app = CalculatorApp::new();
            let mut state = ui::State::new();

            // Seed the CRDT from the initial model, then checkpoint that baseline.
            app.sync(&mut state.doc, &state.root);
            state.commit_action(None, None);

            // Start from a single, empty 2D function; checkpoint it.
            app.set_mode(Mode::Graph2d);
            app.model.funcs = vec![PlotFn {
                expr: String::new(),
                visible: true,
            }];
            app.model.active_fn = 0;
            doc::store(&mut state.doc, &app.model);
            state.commit_action(None, None);

            // Type "2*x" (character edits) and seal one checkpoint — exactly what the
            // App's needs_commit → commit_action coalescing does after a pause.
            let none = winit::event::Modifiers::default();
            for ch in ["2", "*", "x"] {
                app.on_key(&WKey::Character(ch.into()), none, &mut state.doc);
            }
            assert_eq!(app.model.funcs[0].expr, "2*x", "undo: typed formula");
            let before = ui::undo::take_committed_action(&mut app)
                .expect("formula actions expose one undo bracket");
            assert!(
                state.record_committed_action(before, None, None),
                "undo: committed action recorded"
            );

            // Ctrl+Z: ui::State restores the prior snapshot; sync adopts it.
            assert!(
                state.undo(None, None).is_some(),
                "undo must pop a checkpoint"
            );
            app.sync(&mut state.doc, &state.root);
            assert_eq!(
                app.model.funcs[0].expr, "",
                "Ctrl+Z must restore the pre-edit formula"
            );

            // Ctrl+Y: redo re-applies; sync adopts it.
            assert!(
                state.redo(None, None).is_some(),
                "redo must pop a checkpoint"
            );
            app.sync(&mut state.doc, &state.root);
            assert_eq!(
                app.model.funcs[0].expr, "2*x",
                "Ctrl+Y must re-apply the edit"
            );
        }

        // phase external_crdt_edit_is_adopted_on_sync
        {
            // A change made directly in the shared CRDT (as a collaborator's merged
            // patch would appear) is adopted by `sync` — proving the field buffer is
            // genuinely CRDT-backed, not a private String the app owns.
            let mut app = CalculatorApp::new();
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root);

            // Mutate the document out-of-band, then re-sync.
            app.model.mode = Mode::Graph3d; // (only to prove sync overwrites it)
            let mut edited = crate::doc::load(&state.doc).expect("seeded");
            edited.surfaces = vec![PlotFn {
                expr: "x*y+1".into(),
                visible: true,
            }];
            crate::doc::store(&mut state.doc, &edited);
            app.sync(&mut state.doc, &state.root);
            assert_eq!(
                app.model.surfaces.first().map(|f| f.expr.as_str()),
                Some("x*y+1"),
                "sync must adopt the external CRDT surface edit"
            );
        }

        // phase edit_through_event_path_arms_and_seals_undo_commit
        {
            // The calculator's half of the framework Ctrl+Z: a real edit through the
            // event path arms the coalescing timer, `needs_commit` seals a checkpoint
            // only after the pause, and `on_commit` clears it. (The Ctrl+Z keybinding →
            // snapshot → sync loop itself lives in `ui::App`; the snapshot round-trip is
            // proven by the undo_redo_of_formula_edit_routes_through_ui_state phase above.)
            let mut app = CalculatorApp::new();
            let mut st = ui::State::new();
            app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
            app.relayout();
            app.layout_active();
            assert!(!needs_commit(&app), "no edit yet → nothing to commit");
            let five = app.bench_calc_key_center("5").expect("5 keycap laid out");
            tap(&mut app, &mut st, five);
            assert_eq!(app.model.expr, "5", "seal: tapped 5");
            assert!(
                app.last_edit_time.is_some(),
                "an edit must arm the coalescing timer"
            );
            assert!(
                !needs_commit(&app),
                "a just-made edit must not seal a checkpoint mid-burst"
            );
            // Once the coalesce window has elapsed the widget asks to checkpoint.
            app.last_edit_time = Some(
                ui::clock::Instant::now()
                    .checked_sub(COMMIT_COALESCE)
                    .unwrap(),
            );
            assert!(
                needs_commit(&app),
                "after the pause the edit must seal into a checkpoint"
            );
            // The App calls on_commit after snapshotting; that clears the pending edit.
            ui::undo::on_commit(&mut app);
            assert!(!needs_commit(&app), "seal: on_commit clears needs_commit");
            assert!(
                app.last_edit_time.is_none(),
                "seal: on_commit clears the timer"
            );
        }
    }

    // -- pointer-path harness: drive the REAL production handle_event route -----
    //
    // These exercise the live pointer pipeline end-to-end — hit-testing against
    // the same rects the last layout painted, then the wired handler — rather
    // than calling the model-level `apply_key` / view mutators directly. Node /
    // taffy are unused by the (single-leaf) calculator, so a throwaway leaf is
    // fine.

    fn send(app: &mut CalculatorApp, st: &mut ui::State, ev: &WindowEvent) -> bool {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut st.doc,
                path: &st.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    fn cursor(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn btn(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        }
    }
    fn wheel(y: f64) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, y as f32),
            phase: winit::event::TouchPhase::Moved,
        }
    }

    /// A full click (move → press → release) at scene point `p`.
    fn tap(app: &mut CalculatorApp, st: &mut ui::State, p: Point) {
        send(app, st, &cursor(p));
        send(app, st, &btn(ElementState::Pressed));
        send(app, st, &btn(ElementState::Released));
    }

    /// A press-drag: press at `a`, move to `b` with the button held, release.
    fn drag(app: &mut CalculatorApp, st: &mut ui::State, a: Point, b: Point) {
        send(app, st, &cursor(a));
        send(app, st, &btn(ElementState::Pressed));
        send(app, st, &cursor(b));
        send(app, st, &btn(ElementState::Released));
    }

    /// A laid-out app at the wide 1280×800 area used by the pointer tests.
    fn wide(mut app: CalculatorApp) -> CalculatorApp {
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        app.layout_active();
        app
    }

    /// Calculator-mode pointer path: keycaps append, a history row recalls,
    /// Clear empties history, memory keys accumulate/clear, and the RAD/DEG
    /// toggle changes trig output. Fresh app + state per phase.
    #[test]
    fn calc_pointer_clicks_drive_keypad_history_memory_and_angle() {
        // phase calc_keypad_click_appends_to_expression
        {
            // handle_event → on_press → on_press_calc → key_at → apply_key on the
            // real keycap rects (not a model-level apply_key shortcut).
            let mut app = wide(CalculatorApp::new()); // Calc mode, empty
            let mut st = ui::State::new();
            let seven = app.bench_calc_key_center("7").expect("7 keycap laid out");
            tap(&mut app, &mut st, seven);
            assert_eq!(app.model.expr, "7", "tapping a digit keycap must append it");
            let plus = app.bench_calc_key_center("+").expect("+ keycap laid out");
            tap(&mut app, &mut st, plus);
            assert_eq!(
                app.model.expr, "7+",
                "tapping an operator keycap must append it"
            );
        }

        // phase history_row_click_recalls_expression
        {
            // on_press_calc → history_at → model.expr loads the tapped entry.
            let mut app = CalculatorApp::seeded();
            app.set_mode(Mode::Calc);
            let mut st = ui::State::new();
            let mut app = wide(app); // wide → history docks open
            let want = app.model.history[0].expr.clone();
            assert!(!want.is_empty(), "history recall: fixture non-empty");
            let row = app
                .calc
                .history_row_center(0)
                .expect("history row 0 laid out");
            app.model.expr = "999".into(); // something distinct to overwrite
            app.model.after_equals = true;
            tap(&mut app, &mut st, row);
            assert_eq!(
                app.model.expr, want,
                "tapping a history row must load its expression"
            );
            assert!(
                !app.model.after_equals,
                "recall must clear the after-equals latch"
            );
        }

        // phase clear_history_button_click_empties_history
        {
            // on_press_calc → clear_history_at → model.history.clear().
            let mut app = CalculatorApp::seeded();
            app.set_mode(Mode::Calc);
            let mut st = ui::State::new();
            let mut app = wide(app);
            assert!(
                !app.model.history.is_empty(),
                "clear history: fixture non-empty"
            );
            let clear = app
                .calc
                .clear_history_center()
                .expect("Clear button laid out");
            tap(&mut app, &mut st, clear);
            assert!(
                app.model.history.is_empty(),
                "tapping Clear must empty the history"
            );
        }

        // phase memory_button_click_accumulates_via_pointer
        {
            // on_press_calc → mem_at → apply_key(MemAdd/MemClear) via the mem rects.
            let mut app = CalculatorApp::new();
            let mut st = ui::State::new();
            app.model.expr = "10".into();
            let mut app = wide(app);
            // MEM_KEYS = [MC, MR, M+, M-]; index 2 is M+, index 0 is MC.
            let m_add = app.calc.mem_center(2).expect("M+ button laid out");
            tap(&mut app, &mut st, m_add);
            assert_eq!(
                app.model.memory, 10.0,
                "M+ must add the current value to memory"
            );
            tap(&mut app, &mut st, m_add);
            assert_eq!(app.model.memory, 20.0, "M+ again accumulates");
            let m_clr = app.calc.mem_center(0).expect("MC button laid out");
            tap(&mut app, &mut st, m_clr);
            assert_eq!(app.model.memory, 0.0, "MC must clear memory");
        }

        // phase deg_toggle_click_changes_angle_mode_and_trig_output
        {
            // on_press → deg_rect / rad_rect → model.deg, and the change must flow
            // through model.ctx() so trig actually evaluates differently.
            use crate::engine;
            let mut app = CalculatorApp::new();
            let mut st = ui::State::new();
            app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
            app.relayout();
            assert!(!app.model.deg, "default is radians");
            let rad_val = engine::evaluate("sin(90)", &app.model.ctx()).unwrap();
            assert!(
                (rad_val - 1.0).abs() > 0.01,
                "RAD sin(90) must not be 1 (got {rad_val})"
            );
            let deg_pt = app.deg_rect.center();
            tap(&mut app, &mut st, deg_pt);
            assert!(app.model.deg, "clicking DEG must switch to degrees");
            let deg_val = engine::evaluate("sin(90)", &app.model.ctx()).unwrap();
            assert!(
                (deg_val - 1.0).abs() < 1e-9,
                "DEG sin(90) must be 1 (got {deg_val})"
            );
            let rad_pt = app.rad_rect.center();
            tap(&mut app, &mut st, rad_pt);
            assert!(!app.model.deg, "clicking RAD must switch back to radians");
        }
    }

    /// Graph-mode pointer path: drag pans / orbits and the wheel zooms the 2D and
    /// 3D views, the narrow Functions drawer opens from its toggle, and the row
    /// visibility dot and delete button act on the tapped row. Fresh app + state
    /// per phase.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn graph_pointer_path_drives_views_drawer_and_row_controls() {
        // phase graph2d_drag_pans_the_view
        {
            // handle_event drag → on_drag → view2d center shifts.
            let mut app = CalculatorApp::new();
            app.set_mode(Mode::Graph2d);
            let mut st = ui::State::new();
            let mut app = wide(app); // docked panel → live canvas
            assert_eq!(app.model.view2d.cx, 0.0, "pan 2D: starts at cx 0");
            assert_eq!(app.model.view2d.cy, 0.0, "pan 2D: starts at cy 0");
            let c = app.g2.canvas.center();
            drag(&mut app, &mut st, c, Point::new(c.x + 40.0, c.y + 30.0));
            assert!(app.model.view2d.cx != 0.0, "horizontal drag must pan cx");
            assert!(app.model.view2d.cy != 0.0, "vertical drag must pan cy");
        }

        // phase graph2d_wheel_zooms_the_view
        {
            // handle_event wheel over the canvas → on_wheel → view2d.scale grows.
            let mut app = CalculatorApp::new();
            app.set_mode(Mode::Graph2d);
            let mut st = ui::State::new();
            let mut app = wide(app);
            let before = app.model.view2d.scale;
            let c = app.g2.canvas.center();
            send(&mut app, &mut st, &cursor(c));
            send(&mut app, &mut st, &wheel(1.0));
            assert!(
                app.model.view2d.scale > before,
                "scrolling up over the canvas must zoom in (scale {before} → {})",
                app.model.view2d.scale
            );
        }

        // phase graph3d_drag_orbits_the_view
        {
            // handle_event drag → on_drag → view3d yaw/pitch change.
            let mut app = CalculatorApp::new();
            app.set_mode(Mode::Graph3d);
            let mut st = ui::State::new();
            let mut app = wide(app);
            let (yaw0, pitch0) = (app.model.view3d.yaw, app.model.view3d.pitch);
            let c = app.g3.canvas.center();
            drag(&mut app, &mut st, c, Point::new(c.x + 50.0, c.y + 20.0));
            assert!(
                (app.model.view3d.yaw - yaw0).abs() >= f64::EPSILON,
                "horizontal drag must change yaw"
            );
            assert!(
                (app.model.view3d.pitch - pitch0).abs() >= f64::EPSILON,
                "vertical drag must change pitch"
            );
        }

        // phase graph3d_wheel_zooms_the_view
        {
            // handle_event wheel over the surface canvas → on_wheel → view3d.zoom.
            let mut app = CalculatorApp::new();
            app.set_mode(Mode::Graph3d);
            let mut st = ui::State::new();
            let mut app = wide(app);
            let before = app.model.view3d.zoom;
            let c = app.g3.canvas.center();
            send(&mut app, &mut st, &cursor(c));
            send(&mut app, &mut st, &wheel(1.0));
            assert!(
                app.model.view3d.zoom > before,
                "scrolling up over the surface must zoom in"
            );
        }

        // phase narrow_graph2d_toggle_opens_drawer_via_handle_event
        {
            use taffy::{Style, TaffyTree};
            // Drive the REAL production event path (handle_event → on_press) at a
            // narrow width and assert the collapsed Functions drawer opens.
            let mut app = CalculatorApp::new();
            app.set_mode(Mode::Graph2d);
            app.area = Rect::new(0.0, 0.0, 560.0, 720.0);
            app.relayout();
            app.layout_active();
            assert!(!app.g2.panel_open(), "narrow default must start collapsed");
            let toggle = app
                .g2
                .toggle_center()
                .expect("a collapsed toggle must be present");

            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut st = ui::State::new();
            let mods = winit::event::Modifiers::default();
            let mut handled = false;
            let mut focus = None;
            let dev = winit::event::DeviceId::dummy();
            app.handle_event(
                &WindowEvent::CursorMoved {
                    device_id: dev,
                    position: winit::dpi::PhysicalPosition::new(toggle.x, toggle.y),
                },
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut st.doc,
                    path: &st.root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            app.handle_event(
                &WindowEvent::MouseInput {
                    device_id: dev,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut st.doc,
                    path: &st.root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert!(
                app.g2.panel_open(),
                "tapping the toggle must open the Functions drawer"
            );
        }

        // phase vis_dot_click_toggles_function_visibility
        {
            // on_press_g2 → vis_at → funcs[i].visible flips, via the real dot rect.
            let mut app = CalculatorApp::seeded(); // Graph2d, 4 visible funcs
            app.set_mode(Mode::Graph2d);
            let mut st = ui::State::new();
            // Mirror the live frame loop, which syncs the model into the document
            // before routing a pointer event. Without it the taps dispatch their
            // toggle actions against an empty document: the first toggle finds no
            // `funcs[0]` to flip, so it commits an empty change, and (per
            // `perform_action`'s contract) an empty change wedges the next dispatch
            // — the second tap then panics inside Automerge's transaction open.
            app.sync(&mut st.doc, &st.root);
            let mut app = wide(app); // docked panel
            assert!(app.model.funcs[0].visible, "vis dot: starts visible");
            let dot = app.g2.vis_center(0).expect("row 0 visibility dot laid out");
            tap(&mut app, &mut st, dot);
            assert!(
                !app.model.funcs[0].visible,
                "tapping the dot must hide the function"
            );
            tap(&mut app, &mut st, dot);
            assert!(app.model.funcs[0].visible, "tapping again must show it");
        }

        // phase graph_row_delete_button_tombstones_the_stable_resource
        {
            // A directly visible delete button must use the same keyed action as
            // the palette, so it cannot accidentally rebind after a peer changes
            // list order.
            let mut app = CalculatorApp::seeded();
            let mut st = ui::State::new();
            app.sync(&mut st.doc, &st.root);
            let mut app = wide(app);
            let id = crate::doc::row_id_at(&st.doc, crate::doc::FUNCS, 0)
                .expect("first formula has a durable id");
            let delete = app
                .g2
                .delete_rect(0)
                .expect("first formula has a visible delete control")
                .center();
            let before = app.model.funcs.len();
            tap(&mut app, &mut st, delete);
            assert_eq!(
                app.model.funcs.len(),
                before - 1,
                "delete button: one row removed"
            );
            assert!(
                !crate::doc::visible_ids(&st.doc, crate::doc::FUNCS).contains(&id),
                "the clicked stable row is tombstoned rather than a neighbouring index"
            );
        }
    }
}
