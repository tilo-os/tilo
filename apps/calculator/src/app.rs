//! Root widget for the calculator. A single full-window widget that owns
//! the [`CalcModel`], paints the header + the active mode's screen, and
//! routes all keyboard / pointer input. State is flushed into the shared
//! Automerge document on every change (see [`crate::doc`]).

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey};

use ui::Widget;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::actions::{self, CalculatorAction, GraphKind};
use crate::draw::{self, Align};
use crate::graph2d::Graph2dScreen;
use crate::graph3d::Graph3dScreen;
use crate::keypad::{CalcScreen, MEM_KEYS};
use crate::model::{self, CalcModel, Mode, PlotFn};

const TOPBAR_H: f64 = 60.0;

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

    mouse: Point,
    dragging: bool,
    drag_last: Point,
    /// A drag-select is in progress inside a math field.
    field_dragging: bool,
    /// Time + position of the last press, for double-click detection.
    last_click: Option<std::time::Instant>,
    last_click_pos: Point,

    seeded: bool,
    model_dirty: bool,
    /// When `Some`, an edit landed at this time and has not yet been sealed into
    /// an undo checkpoint. Drives [`Widget::needs_commit`] so a burst of typing
    /// coalesces into a single Ctrl+Z step once editing pauses.
    last_edit_time: Option<std::time::Instant>,
    node: Option<NodeId>,

    /// Framework-owned action dispatch. Each on-screen affordance dispatches its
    /// [`CalculatorAction`] through this: the host brackets the undo boundary,
    /// applies the action through the executor (so it replays byte-identically on
    /// peers, exactly as the Ctrl+K palette does), and records the last one so a
    /// test can prove an affordance and its [`ActionSpec`](qos_actions::ActionSpec)
    /// build the identical action.
    actions: ui::ActionHost,
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
            mouse: Point::ZERO,
            dragging: false,
            drag_last: Point::ZERO,
            field_dragging: false,
            last_click: None,
            last_click_pos: Point::ZERO,
            seeded: false,
            model_dirty: false,
            last_edit_time: None,
            node: None,
            actions: ui::ActionHost::new(),
        }
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

        // RAD/DEG anchored to the right edge; compact form when tight.
        let compact_toggle = w < 620.0;
        let toggle_w = if compact_toggle { 100.0 } else { 124.0 };
        let right_margin = if compact_toggle { 12.0 } else { 24.0 };
        let ax1 = self.area.x1 - right_margin;
        let ax0 = ax1 - toggle_w;
        let half = toggle_w / 2.0;
        self.rad_rect = Rect::new(ax0, y0, ax0 + half, y0 + seg_h);
        self.deg_rect = Rect::new(ax0 + half, y0, ax1, y0 + seg_h);

        // The mode switcher fills the gap between the brand and the toggle,
        // centred, with each segment clamped to a tappable width.
        let seg_area_left = brand_right + 12.0;
        let seg_area_right = ax0 - 12.0;
        let avail = (seg_area_right - seg_area_left).max(3.0 * 46.0);
        let seg_w = (avail / 3.0).clamp(46.0, 128.0);
        self.seg_short = seg_w < 104.0;
        let seg_total = seg_w * 3.0;
        let seg_start =
            seg_area_left + ((seg_area_right - seg_area_left) - seg_total).max(0.0) / 2.0;
        for (i, rect) in self.seg_rects.iter_mut().enumerate() {
            let sx = seg_start + i as f64 * seg_w;
            *rect = Rect::new(sx, y0, sx + seg_w, y0 + seg_h);
        }
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
    /// with the Ctrl+K palette. Applied through the qOS action executor (a pinned
    /// actor + committed change, so it replays byte-identically on peers). The
    /// document heads captured just before the first action of the current event
    /// are stashed so the host records **one** undo boundary bracketing it
    /// ([`Widget::take_committed_action`]). The in-memory model is mutated by the
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
        self.actions.dispatch_with_context(
            doc,
            actions::CALCULATOR_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action("org.qos.Calculator", "Calculator", action),
        );
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
            // Give a freshly-entered graph mode something to edit rather than a
            // blank void: an empty `y =` row for 2D, an example `z =` surface
            // for 3D. These are editor affordances, not restored data.
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
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, self.topbar, c.surfaces.raised);
        draw::line(
            scene,
            t,
            Point::new(self.topbar.x0, self.topbar.y1),
            Point::new(self.topbar.x1, self.topbar.y1),
            c.border_standard,
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
        draw::fill_rrect(scene, t, outer, 10.0, c.neutral[2]);
        let sel = self.model.mode.index();
        for (i, (m, &r)) in Mode::all().iter().zip(self.seg_rects.iter()).enumerate() {
            if i == sel {
                draw::fill_rrect(scene, t, r, 8.0, c.accent[5]);
            }
            let col = if i == sel { Color::WHITE } else { c.text_dim };
            let label = if self.seg_short {
                m.short_label()
            } else {
                m.label()
            };
            draw::text_in(
                scene,
                tr,
                t,
                r,
                label,
                14.0,
                col,
                Align::Center,
                TextWeight::Semibold,
            );
        }

        // DEG / RAD toggle
        let outer2 = Rect::new(
            self.rad_rect.x0 - 4.0,
            self.rad_rect.y0 - 4.0,
            self.deg_rect.x1 + 4.0,
            self.deg_rect.y1 + 4.0,
        );
        draw::fill_rrect(scene, t, outer2, 10.0, c.neutral[2]);
        if self.model.deg {
            draw::fill_rrect(scene, t, self.deg_rect, 8.0, c.accent[5]);
        } else {
            draw::fill_rrect(scene, t, self.rad_rect, 8.0, c.accent[5]);
        }
        let rad_col = if self.model.deg {
            c.text_dim
        } else {
            Color::WHITE
        };
        let deg_col = if self.model.deg {
            Color::WHITE
        } else {
            c.text_dim
        };
        draw::text_in(
            scene,
            tr,
            t,
            self.rad_rect,
            "RAD",
            13.0,
            rad_col,
            Align::Center,
            TextWeight::Semibold,
        );
        draw::text_in(
            scene,
            tr,
            t,
            self.deg_rect,
            "DEG",
            13.0,
            deg_col,
            Align::Center,
            TextWeight::Semibold,
        );
    }

    // -- pointer routing -----------------------------------------------------

    fn on_press(&mut self, p: Point, doc: &mut AutoCommit) {
        // header first
        for (i, r) in self.seg_rects.iter().enumerate() {
            if r.contains(p) {
                self.set_mode(Mode::from_index(i));
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
        let now = std::time::Instant::now();
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
            self.model.surfaces.push(PlotFn {
                expr: String::new(),
                visible: true,
            });
            self.model.active_surface = self.model.surfaces.len() - 1;
            self.perform_action(doc, &actions::add_graph_action(GraphKind::Surface3d, ""));
            self.model_dirty = true;
        } else if let Some(i) = self.g3.vis_at(p) {
            if let Some(f) = self.model.surfaces.get_mut(i) {
                f.visible = !f.visible;
                self.perform_action(
                    doc,
                    &actions::toggle_graph_visible_action(GraphKind::Surface3d, i),
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
                self.perform_action(doc, &actions::clear_history_action());
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
            self.model.funcs.push(PlotFn {
                expr: String::new(),
                visible: true,
            });
            self.model.active_fn = self.model.funcs.len() - 1;
            self.perform_action(doc, &actions::add_graph_action(GraphKind::Curve2d, ""));
            self.model_dirty = true;
        } else if let Some(i) = self.g2.vis_at(p) {
            if let Some(f) = self.model.funcs.get_mut(i) {
                f.visible = !f.visible;
                self.perform_action(
                    doc,
                    &actions::toggle_graph_visible_action(GraphKind::Curve2d, i),
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

    fn on_key(&mut self, key: &WKey, modifiers: winit::event::Modifiers, doc: &mut AutoCommit) {
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();

        // Ctrl+digit always switches mode, from any screen.
        if ctrl && let WKey::Character(s) = key {
            match s.as_str() {
                "1" => {
                    self.set_mode(Mode::Calc);
                    return;
                }
                "2" => {
                    self.set_mode(Mode::Graph2d);
                    return;
                }
                "3" => {
                    self.set_mode(Mode::Graph3d);
                    return;
                }
                _ => {}
            }
        }

        // The graph editors own a real math field; route their input there.
        match self.model.mode {
            Mode::Graph2d => {
                if self.on_key_g2(key, modifiers) {
                    return;
                }
            }
            Mode::Graph3d => {
                if self.on_key_g3(key, modifiers) {
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
                    self.model.funcs.push(PlotFn {
                        expr: String::new(),
                        visible: true,
                    });
                    self.model.active_fn = self.model.funcs.len() - 1;
                    self.perform_action(doc, &actions::add_graph_action(GraphKind::Curve2d, ""));
                    self.model_dirty = true;
                }
                Mode::Graph3d => {
                    self.model.surfaces.push(PlotFn {
                        expr: String::new(),
                        visible: true,
                    });
                    self.model.active_surface = self.model.surfaces.len() - 1;
                    self.perform_action(doc, &actions::add_graph_action(GraphKind::Surface3d, ""));
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
    fn on_key_g2(&mut self, key: &WKey, modifiers: winit::event::Modifiers) -> bool {
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
            self.model_dirty = true;
        }
        true
    }

    /// Route a key to the active Graph 3D surface field. Returns whether the
    /// key was consumed by the field.
    fn on_key_g3(&mut self, key: &WKey, modifiers: winit::event::Modifiers) -> bool {
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

impl Widget for CalculatorApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[actions::CALCULATOR_OPERATION_TYPE]
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == qos_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        self.set_mode(match operation {
            actions::CalculatorOperation::ShowCalculator => Mode::Calc,
            actions::CalculatorOperation::ShowGraph2d => Mode::Graph2d,
            actions::CalculatorOperation::ShowGraph3d => Mode::Graph3d,
        });
        Ok(true)
    }

    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            if crate::doc::is_seeded(state) {
                if let Some(m) = crate::doc::load(state) {
                    self.model = m;
                }
            } else {
                crate::doc::store(state, &self.model);
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
        if let Some(mut loaded) = crate::doc::load(state) {
            loaded.after_equals = self.model.after_equals;
            if loaded != self.model {
                self.model = loaded;
            }
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
        self.area = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        self.relayout();
        self.layout_active();

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
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
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_press(self.mouse, state);
                *handled = true;
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
                *handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let step = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(pos) => pos.y / 40.0,
                };
                self.on_wheel(step);
                *handled = true;
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                self.on_key(&key_event.logical_key, *modifiers, state);
                *handled = true;
            }
            _ => {}
        }

        if self.model_dirty {
            crate::doc::store(state, &self.model);
            self.model_dirty = false;
            // Mark an uncommitted edit so `needs_commit` can seal it into an
            // undo checkpoint once editing pauses (character-key edits are not
            // committed by the App's per-event path, only coalesced here).
            self.last_edit_time = Some(std::time::Instant::now());
        }
    }

    fn name(&self) -> &'static str {
        "CalculatorApp"
    }

    /// The calculator canvas has no native control subtree. Export the
    /// top-bar's persistent state and its primary evaluate affordance; graph
    /// fields and keypad input retain their existing keyboard/text semantics.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let modes = Mode::all()
            .iter()
            .enumerate()
            .map(|(index, mode)| {
                AccessNode::new(Role::Tab)
                    .with_name(mode.label())
                    .with_action_target(format!("calculator.mode:{index}"))
                    .with_bounds(self.seg_rects[index])
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
        if self.model.mode == Mode::Calc {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("Evaluate expression")
                    .with_action_target("calculator.evaluate")
                    .with_state(AccessState::new().focusable(true)),
            );
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
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        use ui::a11y::Action;
        if !matches!(action, Action::Activate | Action::ToggleCheck) {
            return false;
        }
        match target {
            "calculator.mode:0" => self.set_mode(Mode::Calc),
            "calculator.mode:1" => self.set_mode(Mode::Graph2d),
            "calculator.mode:2" => self.set_mode(Mode::Graph3d),
            "calculator.angle:radians" => self.set_degrees(false),
            "calculator.angle:degrees" => self.set_degrees(true),
            "calculator.evaluate" if self.model.mode == Mode::Calc => self.commit_equals(state),
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

    /// Publish the calculator's Ctrl+K resource tree (history entries + saved
    /// graph formulas), via the [`ResourceProvider`](qos_actions::ResourceProvider)
    /// impl in [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its history / graph action, so it records one undo boundary for the whole
    /// gesture (see [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn test_id(&self) -> Option<&str> {
        Some("calculator_app")
    }

    fn get_rect(&self) -> Rect {
        self.area
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_launch_is_empty_calc_mode() {
        // The live app must boot to a blank slate in Calculator mode — no
        // fabricated plots / history / memory / expression.
        let app = CalculatorApp::new();
        assert_eq!(app.model.mode, Mode::Calc);
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
        assert_eq!(app.model.ans, 0.0);
    }

    #[test]
    fn seeded_launch_has_lived_in_content() {
        // The demo/shots constructor keeps the populated content.
        let app = CalculatorApp::seeded();
        assert!(!app.model.funcs.is_empty());
        assert!(!app.model.history.is_empty());
    }

    #[test]
    fn set_mode_marks_dirty() {
        let mut app = CalculatorApp::new();
        app.set_mode(Mode::Graph2d);
        assert_eq!(app.model.mode, Mode::Graph2d);
        assert!(app.model_dirty);
    }

    #[test]
    fn accessibility_exposes_and_activates_topbar_controls() {
        use ui::a11y::Action;
        use ui::widget::Widget as _;

        let mut app = CalculatorApp::new();
        let tree = app.accessibility().expect("calculator semantics");
        assert_eq!(tree.name.as_deref(), Some("Calculator"));
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
        assert!(mode_bar.children.iter().any(|node| {
            node.action_target.as_deref() == Some("calculator.mode:1")
                && node.name.as_deref() == Some("Graph 2D")
        }));

        let mut doc = AutoCommit::new();
        assert!(app.perform_semantic_action(
            "calculator.mode:1",
            &Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
        assert_eq!(app.model.mode, Mode::Graph2d);
        assert!(app.perform_semantic_action(
            "calculator.angle:degrees",
            &Action::ToggleCheck,
            &mut doc,
            &automerge::ROOT,
        ));
        assert!(app.model.deg);
    }

    #[test]
    fn entering_empty_graph_modes_seeds_a_starter() {
        // Switching an empty live app into a graph mode gives it an editable
        // starter (empty 2D row / example surface), not a blank void.
        let mut app = CalculatorApp::new();
        app.set_mode(Mode::Graph2d);
        assert_eq!(app.model.funcs.len(), 1, "Graph 2D seeds one empty row");
        assert!(app.model.funcs[0].expr.is_empty());
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

    #[test]
    fn topbar_never_overlaps_or_clips_down_to_400() {
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
    }

    #[test]
    fn topbar_reflows_on_resize_between_breakpoints() {
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

    #[test]
    fn narrow_graph2d_toggle_opens_drawer_via_handle_event() {
        // Drive the REAL production event path (handle_event → on_press) at a
        // narrow width and assert the collapsed Functions drawer opens.
        use taffy::{Style, TaffyTree};
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
            1.0,
            &mods,
            &mut st.doc,
            &st.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        app.handle_event(
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
            &mods,
            &mut st.doc,
            &st.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        assert!(
            app.g2.panel_open(),
            "tapping the toggle must open the Functions drawer"
        );
    }

    #[test]
    fn ctrl_digit_switches_mode() {
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        let ctrl = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
        app.on_key(&WKey::Character("3".into()), ctrl, &mut st.doc);
        assert_eq!(app.model.mode, Mode::Graph3d);
    }

    #[test]
    fn typing_into_graph2d_field_updates_active_func() {
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        app.set_mode(Mode::Graph2d);
        // start from a clean active row
        app.model.funcs = vec![crate::model::PlotFn {
            expr: String::new(),
            visible: true,
        }];
        app.model.active_fn = 0;
        let none = winit::event::Modifiers::default();
        for ch in ["2", "*", "x"] {
            app.on_key(&WKey::Character(ch.into()), none, &mut st.doc);
        }
        assert_eq!(app.model.funcs[0].expr, "2*x");
    }

    #[test]
    fn backspace_in_graph3d_field_edits_surface() {
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        app.set_mode(Mode::Graph3d);
        app.model.surfaces = vec![PlotFn {
            expr: "x+y".into(),
            visible: true,
        }];
        app.model.active_surface = 0;
        // on_key_g3 resyncs the active field to the surface before editing.
        let none = winit::event::Modifiers::default();
        app.on_key(&WKey::Named(NamedKey::Backspace), none, &mut st.doc);
        assert_eq!(app.model.surfaces[0].expr, "x+");
    }

    #[test]
    fn enter_in_graph2d_adds_and_focuses_a_new_function() {
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
        let none = winit::event::Modifiers::default();
        for ch in ["s", "i", "n", "(", "x", ")"] {
            app.on_key(&WKey::Character(ch.into()), none, &mut st.doc);
        }
        assert_eq!(app.model.funcs[0].expr, "sin(x)");
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

    #[test]
    fn enter_in_graph3d_adds_and_focuses_a_new_surface() {
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        app.set_mode(Mode::Graph3d); // seeds one starter surface
        let start = app.model.surfaces.len();
        let none = winit::event::Modifiers::default();
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

    /// Undo/redo of a formula edit round-trips through `ui::State`'s snapshot
    /// stack (not a bespoke calculator stack): typing checkpoints via
    /// `commit_action`, Ctrl+Z restores the previous CRDT snapshot, and `sync`
    /// adopts the restored document back into the on-screen model.
    #[test]
    fn undo_redo_of_formula_edit_routes_through_ui_state() {
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
        assert_eq!(app.model.funcs[0].expr, "2*x");
        doc::store(&mut state.doc, &app.model);
        state.commit_action(None, None);

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

    /// A change made directly in the shared CRDT (as a collaborator's merged
    /// patch would appear) is adopted by `sync` — proving the field buffer is
    /// genuinely CRDT-backed, not a private String the app owns.
    #[test]
    fn external_crdt_edit_is_adopted_on_sync() {
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

    // -- pointer-path harness: drive the REAL production handle_event route -----
    //
    // These exercise the live pointer pipeline end-to-end — hit-testing against
    // the same rects the last layout painted, then the wired handler — rather
    // than calling the model-level `apply_key` / view mutators directly. Node /
    // taffy are unused by the (single-leaf) calculator, so a throwaway leaf is
    // fine.

    fn send(app: &mut CalculatorApp, st: &mut ui::State, ev: &WindowEvent) {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        let mods = winit::event::Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            1.0,
            &mods,
            &mut st.doc,
            &st.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
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

    #[test]
    fn calc_keypad_click_appends_to_expression() {
        // handle_event → on_press → on_press_calc → key_at → apply_key on the
        // real keycap rects (not a model-level apply_key shortcut).
        let mut app = CalculatorApp::new(); // Calc mode, empty
        let mut st = ui::State::new();
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        app.layout_active();
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

    #[test]
    fn history_row_click_recalls_expression() {
        // on_press_calc → history_at → model.expr loads the tapped entry.
        let mut app = CalculatorApp::seeded();
        app.set_mode(Mode::Calc);
        let mut st = ui::State::new();
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0); // wide → history docks open
        app.relayout();
        app.layout_active();
        let want = app.model.history[0].expr.clone();
        assert!(!want.is_empty());
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

    #[test]
    fn clear_history_button_click_empties_history() {
        // on_press_calc → clear_history_at → model.history.clear().
        let mut app = CalculatorApp::seeded();
        app.set_mode(Mode::Calc);
        let mut st = ui::State::new();
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        app.layout_active();
        assert!(!app.model.history.is_empty());
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

    #[test]
    fn memory_button_click_accumulates_via_pointer() {
        // on_press_calc → mem_at → apply_key(MemAdd/MemClear) via the mem rects.
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        app.model.expr = "10".into();
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        app.layout_active();
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

    #[test]
    fn deg_toggle_click_changes_angle_mode_and_trig_output() {
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

    #[test]
    fn graph2d_drag_pans_the_view() {
        // handle_event drag → on_drag → view2d center shifts.
        let mut app = CalculatorApp::new();
        app.set_mode(Mode::Graph2d);
        let mut st = ui::State::new();
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0); // docked panel → live canvas
        app.relayout();
        app.layout_active();
        assert_eq!(app.model.view2d.cx, 0.0);
        assert_eq!(app.model.view2d.cy, 0.0);
        let c = app.g2.canvas.center();
        drag(&mut app, &mut st, c, Point::new(c.x + 40.0, c.y + 30.0));
        assert!(app.model.view2d.cx != 0.0, "horizontal drag must pan cx");
        assert!(app.model.view2d.cy != 0.0, "vertical drag must pan cy");
    }

    #[test]
    fn graph2d_wheel_zooms_the_view() {
        // handle_event wheel over the canvas → on_wheel → view2d.scale grows.
        let mut app = CalculatorApp::new();
        app.set_mode(Mode::Graph2d);
        let mut st = ui::State::new();
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        app.layout_active();
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

    #[test]
    fn graph3d_drag_orbits_the_view() {
        // handle_event drag → on_drag → view3d yaw/pitch change.
        let mut app = CalculatorApp::new();
        app.set_mode(Mode::Graph3d);
        let mut st = ui::State::new();
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        app.layout_active();
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

    #[test]
    fn graph3d_wheel_zooms_the_view() {
        // handle_event wheel over the surface canvas → on_wheel → view3d.zoom.
        let mut app = CalculatorApp::new();
        app.set_mode(Mode::Graph3d);
        let mut st = ui::State::new();
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        app.layout_active();
        let before = app.model.view3d.zoom;
        let c = app.g3.canvas.center();
        send(&mut app, &mut st, &cursor(c));
        send(&mut app, &mut st, &wheel(1.0));
        assert!(
            app.model.view3d.zoom > before,
            "scrolling up over the surface must zoom in"
        );
    }

    #[test]
    fn vis_dot_click_toggles_function_visibility() {
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
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0); // docked panel
        app.relayout();
        app.layout_active();
        assert!(app.model.funcs[0].visible);
        let dot = app.g2.vis_center(0).expect("row 0 visibility dot laid out");
        tap(&mut app, &mut st, dot);
        assert!(
            !app.model.funcs[0].visible,
            "tapping the dot must hide the function"
        );
        tap(&mut app, &mut st, dot);
        assert!(app.model.funcs[0].visible, "tapping again must show it");
    }

    /// The calculator's half of the framework Ctrl+Z: a real edit through the
    /// event path arms the coalescing timer, `needs_commit` seals a checkpoint
    /// only after the pause, and `on_commit` clears it. (The Ctrl+Z keybinding →
    /// snapshot → sync loop itself lives in `ui::App`; the snapshot round-trip is
    /// proven by `undo_redo_of_formula_edit_routes_through_ui_state`.)
    #[test]
    fn edit_through_event_path_arms_and_seals_undo_commit() {
        let mut app = CalculatorApp::new();
        let mut st = ui::State::new();
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        app.layout_active();
        assert!(!app.needs_commit(), "no edit yet → nothing to commit");
        let five = app.bench_calc_key_center("5").expect("5 keycap laid out");
        tap(&mut app, &mut st, five);
        assert_eq!(app.model.expr, "5");
        assert!(
            app.last_edit_time.is_some(),
            "an edit must arm the coalescing timer"
        );
        assert!(
            !app.needs_commit(),
            "a just-made edit must not seal a checkpoint mid-burst"
        );
        // Once the coalesce window has elapsed the widget asks to checkpoint.
        app.last_edit_time = Some(
            std::time::Instant::now()
                .checked_sub(COMMIT_COALESCE)
                .unwrap(),
        );
        assert!(
            app.needs_commit(),
            "after the pause the edit must seal into a checkpoint"
        );
        // The App calls on_commit after snapshotting; that clears the pending edit.
        app.on_commit();
        assert!(!app.needs_commit());
        assert!(app.last_edit_time.is_none());
    }
}
