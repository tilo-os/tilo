//! Root widget for the calculator. A single full-window widget that owns
//! the [`CalcModel`], paints the header + the active mode's screen, and
//! routes all keyboard / pointer input. Durable rows (history, graphs) are
//! presented from the resource and changed only through its actions.

use std::collections::HashMap;
use std::rc::Rc;
#[cfg(test)]
use ui::widget::Edits;
use ui::widget::ModelPath;

#[cfg(test)]
use automerge::AutoCommit;
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey};

use ui::Widget;
use ui::framework::Dispatcher;
use ui::interaction::InteractionEnvironment;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::actions::{self, GraphKind};
use crate::engine;
use crate::graph2d::{Graph2dScreen, Pointer};
use crate::graph3d::Graph3dScreen;
use crate::keypad::{CalcScreen, DisplaySlot, MEM_KEYS};
use crate::mathfield::MathField;
use crate::model::{self, CalcModel, KeypadPage, Mode, PlotFn};
use crate::plot_panel::PlotPanel;
use crate::resource::Calculator;

use ui::tilo_resource::Id;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal flag (layout state, \
              drag state, and dirty bookkeeping); they aren't a state \
              machine, and grouping them would mean churning every field \
              access across this widget for no behavioural gain"
)]
pub struct CalculatorApp {
    model: CalcModel,
    calc: CalcScreen,
    g2: Graph2dScreen,
    g3: Graph3dScreen,

    /// The whole window.
    area: Rect,
    /// What kind of pointer drives the window, for the canvas hint's copy.
    environment: InteractionEnvironment,
    /// The person has panned, orbited or zoomed a plot, so the hint that
    /// teaches it is no longer shown.
    navigated: bool,

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

    model_dirty: bool,
    /// Something visible changed since the last paint. Cleared by
    /// `mark_drawn`; without it the default (dirty every frame) repainted the
    /// whole desktop on every pointer move while the Calculator was active.
    dirty: bool,
    /// The peers' carets last painted into a plot's formula. They follow
    /// peer pointers, which the window does not damage for its widgets.
    drawn_remote_carets: Vec<(Point, Color)>,
    /// An effect taken on the next sync, where the document is.
    pending_effect: Option<actions::CalculatorEffect>,
    node: Option<NodeId>,

    /// The resource's door: every on-screen affordance submits its field
    /// edit or verb here. `None` until the framework attaches the surface,
    /// and for a bare widget in a layout test.
    dispatch: Option<Dispatcher<Calculator>>,
    /// How this calculator's changes read in the window's History: one
    /// presenter for the instance's life, so the timeline keeps its faces.
    entries: Rc<crate::history_face::CalculatorEntries>,
    /// The durable rows last presented, with their stable ids.
    rows: Rc<Calculator>,
    /// A graph row this window just added, to select once it is presented.
    select: Option<(GraphKind, Id)>,
}

impl CalculatorApp {
    /// The live app: boots empty in Calculator mode. The resource's rows
    /// arrive with the first [`ResourceSurface::present`](ui::framework::ResourceSurface::present).
    pub fn new() -> Self {
        Self::with_model(CalcModel::default())
    }

    /// A lived-in instance (populated history / plots / surface) for layout
    /// tests of the bare widget.
    #[cfg(test)]
    pub(crate) fn seeded() -> Self {
        Self::with_model(CalcModel::seeded())
    }

    fn with_model(model: CalcModel) -> Self {
        Self {
            model,
            calc: CalcScreen::default(),
            g2: Graph2dScreen::default(),
            g3: Graph3dScreen::default(),
            area: Rect::ZERO,
            environment: InteractionEnvironment::POINTER,
            navigated: false,
            mouse: Point::ZERO,
            dragging: false,
            drag_last: Point::ZERO,
            field_dragging: false,
            host_focused: false,
            last_click: None,
            last_click_pos: Point::ZERO,
            model_dirty: false,
            dirty: true,
            drawn_remote_carets: Vec::new(),
            pending_effect: None,
            node: None,
            dispatch: None,
            entries: Rc::default(),
            rows: Rc::default(),
            select: None,
        }
    }

    /// Adopt durable history and graph rows from the framework resource state.
    pub(crate) fn adopt_resource_state(&mut self, state: &Rc<Calculator>) {
        self.model.history = state.history.values().cloned().collect();
        self.model.funcs = state.funcs.values().cloned().collect();
        self.model.surfaces = state.surfaces.values().cloned().collect();
        self.entries
            .set_curves(state.funcs.iter().map(|(id, _)| id.as_str()));
        self.rows = Rc::clone(state);
        if let Some((kind, id)) = self.select.take() {
            match state.graphs(kind).position(&id) {
                Some(index) => *self.model.active_graph_mut(kind) = index,
                // Not presented yet: keep waiting for it.
                None => self.select = Some((kind, id)),
            }
        }
        self.model.active_fn = self
            .model
            .active_fn
            .min(self.model.funcs.len().saturating_sub(1));
        self.model.active_surface = self
            .model
            .active_surface
            .min(self.model.surfaces.len().saturating_sub(1));
        self.model_dirty = true;
        self.dirty = true;
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`](ui::framework::ResourceSurface).
    pub(crate) fn attach_framework(&mut self, dispatcher: Dispatcher<Calculator>) {
        self.dispatch = Some(dispatcher);
    }

    /// Stage the view a screenshot scenario shows over the rows its actions
    /// wrote, the way a person's input would: the mode, the expression being
    /// typed, the memory register, which panel is open. Never called in a
    /// live window.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        if scenario == Some("empty") {
            return;
        }
        self.navigated = false;
        let lived_in = CalcModel::seeded();
        self.model.expr = lived_in.expr;
        self.model.ans = lived_in.ans;
        self.model.memory = lived_in.memory;
        match scenario {
            Some("calculator" | "narrow" | "narrow-functions-page") => {
                self.set_mode(Mode::Calc);
            }
            Some("error") => self.model.apply_error_demo(),
            Some("graph3d" | "narrow-graph3d") => self.set_mode(Mode::Graph3d),
            _ => self.set_mode(Mode::Graph2d),
        }
        // The drawer decides docked or floating from the window it has been
        // laid out in, so lay the mode out before staging its panel.
        self.relayout();
        if scenario == Some("narrow-functions") {
            self.g2.panel.side_mut().snap(true);
        }
        if scenario == Some("narrow-functions-page") {
            self.calc.toggle_page();
            self.relayout();
        }
        if self.model.mode == Mode::Graph2d
            && let Some(field) = self.graph_field(GraphKind::Curve2d, self.model.active_fn)
            && scenario == Some("long-formula")
        {
            field.park_caret_at_end();
        }
        self.model_dirty = true;
        self.dirty = true;
    }

    /// Lay out the active screen in the window. Derived from the current
    /// size every call, so a resize between breakpoints reflows rather than
    /// staying frozen at the launch layout.
    fn relayout(&mut self) {
        self.layout_active();
    }

    fn layout_active(&mut self) {
        match self.model.mode {
            Mode::Calc => self.calc.layout(self.area),
            Mode::Graph2d => self.g2.layout(self.area, &self.model),
            Mode::Graph3d => self.g3.layout(self.area, &self.model),
        }
    }

    /// Submit one field edit or verb through the resource's door — the one
    /// way this window changes the document, shared with the palette, agents
    /// and scripts. It applies now, inside the event, so a formula's next
    /// keystroke builds on this one; the rows it changed are presented back
    /// before the next frame. The caller updates the model for immediate
    /// feedback.
    ///
    /// Only ever call this for an action that genuinely mutates the document —
    /// the affordances gate on a real change (a non-empty history to clear, an
    /// existing row to toggle/delete).
    fn submit(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<Calculator>,
    ) {
        if let Some(dispatch) = &self.dispatch {
            // A refusal (a participant who may only view) writes nothing, and
            // the stored rows are presented again.
            let _ = dispatch.submit_now(doc, submit);
        }
    }

    /// End the formula edit the last keystrokes belonged to, so the next one
    /// is an undo step of its own.
    fn settle_edit(&self) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.settle();
        }
    }

    /// Add a graph row (empty, or a starter formula), select it, and submit
    /// its `AddGraph`.
    fn add_graph(&mut self, kind: GraphKind, expr: &str, doc: &mut ui::widget::Edits<'_>) {
        self.settle_edit();
        let id = actions::new_row_id(kind);
        self.select = Some((kind, id.clone()));
        let row = PlotFn {
            expr: expr.to_owned(),
            visible: true,
        };
        let added = kind
            .rows()
            .insert_as(&id, self.rows.graphs(kind).len(), &row);
        let rows = self.model.graphs_mut(kind);
        rows.push(row);
        let last = rows.len() - 1;
        *self.model.active_graph_mut(kind) = last;
        self.submit(doc, added);
        self.model_dirty = true;
    }

    /// Show or hide the `index`-th row of a graph list.
    fn toggle_graph_row(
        &mut self,
        kind: GraphKind,
        index: usize,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let rows = self.model.graphs_mut(kind);
        let Some(row) = rows.get_mut(index) else {
            return false;
        };
        row.visible = !row.visible;
        let shown = row.visible;
        if let Some(id) = self.rows.graph_id(kind, index) {
            let edit = kind.rows().item(id).visible.set(shown);
            self.submit(doc, edit);
        }
        self.model_dirty = true;
        true
    }

    /// Replace the formula of the `index`-th row of a graph list, as typed.
    fn set_graph_expr(
        &mut self,
        kind: GraphKind,
        index: usize,
        expr: &str,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        let rows = self.model.graphs_mut(kind);
        if let Some(row) = rows.get_mut(index) {
            expr.clone_into(&mut row.expr);
        }
        if let Some(id) = self.rows.graph_id(kind, index) {
            let edit = kind.rows().item(id).expr.replace(expr);
            self.submit(doc, edit);
        }
        self.model_dirty = true;
    }

    /// Commit the current expression (`=` / Enter in Calc mode). Evaluates and
    /// updates the presentation (Ans, result line) via the model, then dispatches
    /// the [`CommitHistory`](CalculatorAction::CommitHistory) action so the new
    /// history row lands on the shared, replayable, undoable path. A failed or
    /// empty evaluation commits nothing and dispatches nothing.
    fn commit_equals(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if let Some(entry) = self.model.commit_equals() {
            self.submit(
                doc,
                Calculator::COMMIT_HISTORY.with((entry.expr, entry.result)),
            );
        }
        self.model_dirty = true;
    }

    /// Show mode `m`: presentation only.
    fn set_mode(&mut self, m: Mode) {
        if self.model.mode != m {
            self.model.mode = m;
            // Leaving a graph mid-zoom must not carry a stale ease into the next
            // visit; settle both animators to the current model view.
            self.g2.settle_zoom(self.model.view2d);
            self.g3.settle_zoom(self.model.view3d.zoom);
            self.model_dirty = true;
        }
    }

    /// Switch to mode `m`. A first visit to an empty graph mode gives it an
    /// editable starter — an empty curve, an example surface — through the
    /// same `AddGraph` action the palette offers, so it is undoable and
    /// reaches collaborators.
    fn switch_mode(&mut self, m: Mode, doc: &mut ui::widget::Edits<'_>) {
        self.settle_edit();
        self.set_mode(m);
        match m {
            Mode::Graph2d if self.model.funcs.is_empty() => {
                self.add_graph(GraphKind::Curve2d, "", doc);
            }
            Mode::Graph3d if self.model.surfaces.is_empty() => {
                self.add_graph(GraphKind::Surface3d, model::DEFAULT_SURFACE, doc);
            }
            _ => {}
        }
    }

    /// Shared angle-unit transition for the window bar's menu and semantic
    /// activation: one state path for both.
    fn set_degrees(&mut self, degrees: bool) {
        if self.model.deg != degrees {
            self.model.deg = degrees;
            self.model_dirty = true;
        }
    }

    /// Whether a floating drawer is up over the window, so the content
    /// beneath it takes no presses.
    fn drawer_blocks(&self) -> bool {
        match self.model.mode.graph() {
            None => false,
            Some(kind) => self.panel(kind).floats_open(),
        }
    }

    /// Track the pointer over every control. Returns whether anything the
    /// window paints in response changed.
    fn track_pointer(&mut self, p: Point) -> bool {
        match self.model.mode.graph() {
            None => self.calc.set_pointer(p),
            Some(kind) => self.panel_mut(kind).set_pointer(p),
        }
    }

    /// What the plot under the pointer shows: the trace of Graph 2D, and the
    /// navigation hint until the person has navigated a plot.
    fn plot_pointer(&self) -> Pointer {
        let over = self
            .model
            .mode
            .graph()
            .is_some_and(|kind| self.canvas_contains(kind, self.mouse));
        Pointer {
            trace: (over && self.model.mode == Mode::Graph2d).then_some(self.mouse),
            hint: (over && !self.navigated).then_some(self.environment),
        }
    }

    fn on_press(&mut self, p: Point, doc: &mut ui::widget::Edits<'_>) {
        // A press ends the formula edit in progress: the next keystroke is
        // an undo step of its own.
        self.settle_edit();

        // Double-click detection (≤400 ms, within 5 px).
        let now = ui::time::now();
        let double = self
            .last_click
            .is_some_and(|t| now.duration_since(t).as_millis() <= 400)
            && (self.last_click_pos - p).hypot() <= 5.0;
        self.last_click = Some(now);
        self.last_click_pos = p;

        match self.model.mode.graph() {
            None => {
                if let Some(slot) = self.calc.copy_at(p) {
                    self.copy_display(slot);
                } else if self.calc.expression_rect().contains(p) && !self.drawer_blocks() {
                    self.calc.field.set_source(&self.model.expr);
                    self.calc.field.on_press_cached(p, double);
                    self.model.after_equals = false;
                    self.field_dragging = true;
                } else {
                    self.on_press_calc(p, doc);
                }
            }
            Some(kind) => self.on_press_graph(kind, p, double, doc),
        }
    }

    fn on_press_graph(
        &mut self,
        kind: GraphKind,
        p: Point,
        double: bool,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        // Overlay-drawer chrome takes priority over the canvas underneath.
        let panel = self.panel(kind);
        if panel.side().scrim_at(p) {
            self.panel_mut(kind).side_mut().close();
            return;
        }
        if panel.add_at(p) {
            self.add_graph(kind, "", doc);
        } else if let Some(i) = panel.delete_at(p) {
            self.delete_graph_row(kind, i, doc);
        } else if let Some(i) = panel.vis_at(p) {
            self.toggle_graph_row(kind, i, doc);
        } else if let Some(i) = panel.field_at(p) {
            // Click landed in row i's editor: activate it and place the caret.
            // The field has no text renderer here, so the press hit-tests
            // its cached layout; it relays out on the next draw.
            *self.model.active_graph_mut(kind) = i;
            if let Some(field) = self.graph_field(kind, i) {
                field.on_press_cached(p, double);
                self.field_dragging = true;
            }
            self.model_dirty = true;
        } else if let Some(i) = panel.row_at(p) {
            *self.model.active_graph_mut(kind) = i;
            self.model_dirty = true;
        } else if panel.side().covers(p) {
            // Press inside an open drawer (not a widget): consume it so it
            // doesn't fall through to the canvas as a pan or orbit.
        } else if self.canvas_contains(kind, p) {
            self.dragging = true;
            self.drag_last = p;
        }
    }

    fn on_press_calc(&mut self, p: Point, doc: &mut ui::widget::Edits<'_>) {
        if let Some(i) = self.calc.key_at(p) {
            self.calc.pressed_key = Some(i);
            if let Some(key) = self.calc.keys().get(i).cloned() {
                // `=` commits to the shared history through an action; every
                // other key is a live edit of the (presentation) expression line.
                if matches!(key, model::Key::Equals) {
                    self.commit_equals(doc);
                } else {
                    self.apply_calc_key(&key);
                    self.model_dirty = true;
                }
            }
        } else if let Some(i) = self.calc.mem_at(p)
            && let Some(key) = MEM_KEYS.get(i).cloned()
        {
            self.apply_calc_key(&key);
            self.model_dirty = true;
        }
    }

    /// Delete one graph row through the same stable, tombstoned action the
    /// palette offers. The model is deliberately not spliced optimistically:
    /// the presented rows are the exact merged list a collaborator sees, and
    /// keep active-row clamping in one place (`adopt_resource_state`).
    fn delete_graph_row(&mut self, kind: GraphKind, index: usize, doc: &mut ui::widget::Edits<'_>) {
        if let Some(id) = self.rows.graph_id(kind, index).cloned() {
            match kind {
                GraphKind::Curve2d => self.submit(doc, Calculator::DELETE_FUNCTION.with(id)),
                GraphKind::Surface3d => self.submit(doc, Calculator::DELETE_SURFACE.with(id)),
            }
            self.model_dirty = true;
        }
    }

    /// Extend a math-field drag-selection to the current pointer position.
    fn on_field_drag(&mut self, p: Point) {
        if self.model.mode == Mode::Calc {
            self.calc.field.on_drag_cached(p);
        } else if let Some(kind) = self.model.mode.graph() {
            let active = self.model.active_graph(kind);
            if let Some(field) = self.panel_mut(kind).active_field_mut(active) {
                field.on_drag_cached(p);
            }
        }
    }

    fn on_drag(&mut self, d: Vec2) {
        self.navigated = true;
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
        if let Some(kind) = self.model.mode.graph()
            && let Some(i) = self.panel(kind).field_at(self.mouse)
        {
            if let Some(f) = self.graph_field(kind, i) {
                f.scroll_by(-step * 24.0, -step * 24.0);
            }
            return;
        }

        if self.model.mode == Mode::Calc && self.calc.expression_rect().contains(self.mouse) {
            self.calc.field.scroll_by(-step * 24.0, -step * 24.0);
            return;
        }
        let factor = (1.0 + step * 0.12).clamp(0.4, 2.5);
        match self.model.mode {
            Mode::Graph2d if self.g2.canvas_contains(self.mouse) => {
                self.navigated = true;
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
                self.navigated = true;
                self.model.view3d.zoom = (self.model.view3d.zoom * factor).clamp(0.25, 8.0);
                self.g3.begin_zoom();
                self.model_dirty = true;
            }
            _ => {}
        }
    }

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
            && self.model.expr.is_empty()
    }

    fn on_key(
        &mut self,
        key: &WKey,
        modifiers: winit::event::Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        self.on_key_with_paste(key, modifiers, doc, ui::clipboard_paste);
    }

    fn on_key_with_paste(
        &mut self,
        key: &WKey,
        modifiers: winit::event::Modifiers,
        doc: &mut ui::widget::Edits<'_>,
        paste: impl FnOnce() -> Option<String>,
    ) {
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        let shift = modifiers.state().shift_key();

        // The display's values are copy targets. A selection in the
        // expression keeps the editor's own copy first; with nothing
        // selected, Ctrl+C takes the result and Ctrl+Shift+C the expression.
        if ctrl
            && self.model.mode == Mode::Calc
            && let WKey::Character(s) = key
            && s.eq_ignore_ascii_case("c")
        {
            if shift {
                self.copy_display(DisplaySlot::Expression);
                return;
            }
            self.calc.field.set_source(&self.model.expr);
            if !self.calc.field.has_selection() {
                self.copy_display(DisplaySlot::Result);
                return;
            }
        }

        // Ctrl+digit always switches mode, from any screen.
        if ctrl && let WKey::Character(s) = key {
            match s.as_str() {
                "1" => {
                    self.switch_mode(Mode::Calc, doc);
                    return;
                }
                "2" => {
                    self.switch_mode(Mode::Graph2d, doc);
                    return;
                }
                "3" => {
                    self.switch_mode(Mode::Graph3d, doc);
                    return;
                }
                _ => {}
            }
        }

        // Escape first dismisses transient drawers. Clearing an expression is
        // still available on a second Escape in Calculator mode, but an open
        // overlay must never trap a keyboard user behind its scrim.
        if matches!(key, WKey::Named(NamedKey::Escape)) {
            let dismissed = match self.model.mode.graph() {
                Some(kind) if self.panel(kind).side().is_open() => {
                    self.panel_mut(kind).side_mut().close();
                    true
                }
                _ => false,
            };
            if dismissed {
                return;
            }
        }

        // The graph editors own a real math field; route their input there.
        if let Some(kind) = self.model.mode.graph()
            && self.on_key_graph(kind, key, modifiers, doc)
        {
            return;
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
                Mode::Graph2d => self.add_graph(GraphKind::Curve2d, "", doc),
                Mode::Graph3d => self.add_graph(GraphKind::Surface3d, "", doc),
            },
            WKey::Named(NamedKey::Backspace) => {
                if self.model.mode == Mode::Calc {
                    self.edit_calc(key, modifiers);
                    self.model_dirty = true;
                }
            }
            WKey::Named(NamedKey::Escape) => {
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
                    } else if self.model.mode == Mode::Calc {
                        self.edit_calc(key, modifiers);
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
                            self.edit_calc(&WKey::Character(ch.to_string().into()), modifiers);
                        }
                    }
                    self.model_dirty = true;
                }
            }
            _ if self.model.mode == Mode::Calc && field_handles(key, modifiers) => {
                self.edit_calc(key, modifiers);
            }
            _ => {}
        }
    }

    /// Edit at the two-dimensional caret, preserving continuation after Equals.
    fn edit_calc(&mut self, key: &WKey, modifiers: winit::event::Modifiers) {
        if self.model.after_equals
            && let WKey::Character(s) = key
            && !modifiers.state().control_key()
            && !modifiers.state().super_key()
            && s.chars().next().is_some_and(|c| !"+-*/^!%)".contains(c))
        {
            self.model.expr.clear();
        }
        self.calc.field.set_source(&self.model.expr);
        if matches!(
            key,
            WKey::Named(
                NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::ArrowUp
                    | NamedKey::ArrowDown
                    | NamedKey::Home
                    | NamedKey::End
            )
        ) {
            self.model.after_equals = false;
        }
        if self.calc.field.handle_key(key, &modifiers) {
            self.model.expr = self.calc.field.current_linear();
            self.model.after_equals = false;
            self.model_dirty = true;
        }
    }

    /// What the display shows for `slot`, exactly as painted, so copying it
    /// takes what the person read (not a re-formatted approximation).
    fn display_text(&self, slot: DisplaySlot) -> Option<String> {
        match slot {
            DisplaySlot::Expression => {
                let expr = self.model.expr.trim();
                (!expr.is_empty()).then(|| expr.to_owned())
            }
            DisplaySlot::Result => {
                if self.model.expr.trim().is_empty() {
                    return None;
                }
                Some(
                    match engine::evaluate(&self.model.expr, &self.model.ctx()) {
                        Ok(value) => engine::format_number(value),
                        Err(error) => format!("Error: {error}"),
                    },
                )
            }
            DisplaySlot::Answer => Some(engine::format_number(self.model.ans)),
        }
    }

    /// Copy one display value to the system clipboard. The expression is
    /// copied as typed, the result as shown, and the answer as Ans.
    fn copy_display(&mut self, slot: DisplaySlot) {
        if let Some(text) = self.display_text(slot) {
            ui::clipboard_copy(&text);
            self.model_dirty = true;
        }
    }

    /// Pointer and semantic keypad activation use the same editor as typing.
    fn apply_calc_key(&mut self, key: &model::Key) {
        use model::Key;
        if matches!(key, Key::Page(_)) {
            self.calc.toggle_page();
            self.relayout();
            self.model_dirty = true;
            return;
        }
        let text = match key {
            Key::Digit(c) => Some(c.to_string()),
            Key::Dot => Some(".".into()),
            Key::DoubleZero => Some("00".into()),
            Key::Ins(s) => Some((*s).into()),
            Key::Ans => Some("ans".into()),
            Key::Percent => Some("%".into()),
            Key::MemRecall => Some(engine::format_number(self.model.memory)),
            Key::Backspace => {
                self.edit_calc(
                    &WKey::Named(NamedKey::Backspace),
                    winit::event::Modifiers::default(),
                );
                return;
            }
            _ => None,
        };
        if let Some(text) = text {
            for ch in text.chars() {
                self.edit_calc(
                    &WKey::Character(ch.to_string().into()),
                    winit::event::Modifiers::default(),
                );
            }
        } else {
            model::apply_key(&mut self.model, key);
        }
    }

    /// Route a key to the active row's field of a graph list. Returns
    /// whether the key was consumed by the field.
    fn on_key_graph(
        &mut self,
        kind: GraphKind,
        key: &WKey,
        modifiers: winit::event::Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let active = self.model.active_graph(kind);
        let Some(field) = self.graph_field(kind, active) else {
            return false;
        };
        // Only consume keys the field actually handles (so e.g. mode switches
        // and unhandled named keys fall through).
        if !field_handles(key, modifiers) {
            return false;
        }
        if field.handle_key(key, &modifiers) {
            let linear = field.current_linear();
            self.set_graph_expr(kind, active, &linear, doc);
        }
        true
    }

    /// The formula panel of a graph list.
    fn panel(&self, kind: GraphKind) -> &PlotPanel {
        match kind {
            GraphKind::Curve2d => &self.g2.panel,
            GraphKind::Surface3d => &self.g3.panel,
        }
    }

    fn panel_mut(&mut self, kind: GraphKind) -> &mut PlotPanel {
        match kind {
            GraphKind::Curve2d => &mut self.g2.panel,
            GraphKind::Surface3d => &mut self.g3.panel,
        }
    }

    /// Row `index`'s editor, reconciled with the model's rows first.
    fn graph_field(&mut self, kind: GraphKind, index: usize) -> Option<&mut MathField> {
        let panel = match kind {
            GraphKind::Curve2d => &mut self.g2.panel,
            GraphKind::Surface3d => &mut self.g3.panel,
        };
        panel.sync_fields(self.model.graphs(kind));
        panel.active_field_mut(index)
    }

    /// Whether `p` is on a graph's interactive plot.
    fn canvas_contains(&self, kind: GraphKind, p: Point) -> bool {
        match kind {
            GraphKind::Curve2d => self.g2.canvas_contains(p),
            GraphKind::Surface3d => self.g3.canvas_contains(p),
        }
    }
}

impl Default for CalculatorApp {
    fn default() -> Self {
        Self::new()
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
            let mut components = rc.color;
            for component in &mut components {
                *component = component.clamp(0.0, 1.0);
            }
            let col =
                vello::peniko::color::AlphaColor::<vello::peniko::color::Srgb>::new(components);
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
        Key::Page(KeypadPage::Functions) => "Function keys, second page".into(),
        Key::Page(KeypadPage::Primary) => "Number keys, first page".into(),
        Key::Ins("sin(") => "Sine".into(),
        Key::Ins("cos(") => "Cosine".into(),
        Key::Ins("tan(") => "Tangent".into(),
        Key::Ins("asin(") => "Inverse sine".into(),
        Key::Ins("acos(") => "Inverse cosine".into(),
        Key::Ins("atan(") => "Inverse tangent".into(),
        Key::Ins("sqrt(") => "Square root".into(),
        Key::Ins("int(") => "Definite integral".into(),
        Key::Ins("root(") => "Nth root".into(),
        Key::Ins("abs(") => "Absolute value".into(),
        Key::Ins("sinh(") => "Hyperbolic sine".into(),
        Key::Ins("cosh(") => "Hyperbolic cosine".into(),
        Key::Ins("tanh(") => "Hyperbolic tangent".into(),
        Key::Ins("gamma(") => "Gamma function".into(),
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
    /// Each presented history entry as its live row: recalling it.
    pub(crate) fn history_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.rows
            .history
            .iter()
            .map(|(id, entry)| crate::resource::history_row(id, entry))
            .collect()
    }

    /// Take `effect` on the next sync, where the document is: showing a graph
    /// for the first time adds its starter row.
    pub(crate) fn queue_effect(&mut self, effect: actions::CalculatorEffect) -> Result<(), String> {
        if let actions::CalculatorEffect::Open { entry } = &effect
            && self
                .rows
                .history
                .get(&Id::from_raw(entry.as_str()))
                .is_none()
        {
            return Err("Calculation no longer exists".into());
        }
        self.pending_effect = Some(effect);
        self.dirty = true;
        Ok(())
    }

    fn perform_effect(
        &mut self,
        effect: actions::CalculatorEffect,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        let mode = match effect {
            actions::CalculatorEffect::Open { entry } => {
                if let Some(past) = self.rows.history.get(&Id::from_raw(entry.as_str())) {
                    self.model.expr.clone_from(&past.expr);
                    self.model.after_equals = false;
                    self.model_dirty = true;
                    self.set_mode(Mode::Calc);
                }
                return;
            }
            actions::CalculatorEffect::Recall { expression } => {
                self.model.expr = expression;
                self.model.after_equals = false;
                self.model_dirty = true;
                self.set_mode(Mode::Calc);
                return;
            }
            actions::CalculatorEffect::ShowCalculator => Mode::Calc,
            actions::CalculatorEffect::ShowGraph2d => Mode::Graph2d,
            actions::CalculatorEffect::ShowGraph3d => Mode::Graph3d,
        };
        // Showing a mode is presentation, except that a first graph visit
        // adds its starter row through the resource's action.
        self.switch_mode(mode, doc);
    }
}

/// History is the calculator's list of past calculations: each reads as
/// itself, and choosing one puts it back into the calculator through the
/// Use calculation effect, the same door the palette and agents use.
impl ui::history_mode::capability::PresentsHistoryEntries for CalculatorApp {
    fn history_entry_presenter(&self) -> Option<Rc<dyn ui::widget::HistoryEntryPresenter>> {
        Some(Rc::clone(&self.entries) as Rc<dyn ui::widget::HistoryEntryPresenter>)
    }

    fn open_history_entry(&mut self, entry: &ui::widget::history::HistoryEntry) -> bool {
        let Some((expression, _)) = crate::history_face::calculation(entry) else {
            return false;
        };
        let recall = actions::CalculatorEffect::Recall { expression };
        match &self.dispatch {
            Some(dispatch) => dispatch.effect(&recall),
            None => {
                if self.queue_effect(recall).is_err() {
                    return false;
                }
            }
        }
        true
    }
}

impl Widget for CalculatorApp {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::history_mode::capability::PresentsHistoryEntries,
        dyn ui::application_view::ApplicationViewState
    );

    fn is_focusable(&self) -> bool {
        true
    }

    /// The calculator is one keyboard target: what is typed goes to the
    /// expression or the formula being edited, so it takes the keyboard
    /// whenever its window does without a control of its own choosing.
    fn is_default_keyboard_focus(&self) -> bool {
        true
    }

    fn set_interaction_environment(&mut self, environment: InteractionEnvironment) {
        self.dirty |= self.environment != environment;
        self.environment = environment;
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
        match self.model.mode.graph() {
            None => self.calc.expression_ime_rect(),
            Some(kind) => self.panel(kind).field_rect(self.model.active_graph(kind)),
        }
    }

    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        self.host_focused.then_some(ui::ime::InputContent::TEXT)
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &automerge::ObjId) {
        if let Some(effect) = self.pending_effect.take() {
            self.perform_effect(effect, edits);
        }
        self.apply_window_toolbar(edits, path);
        self.declare_window_toolbar();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let _ = theme;
        // `new_leaf` only errs when the tree is at capacity; degrade to an
        // inert node rather than aborting the whole session.
        let node = taffy
            .new_leaf(Style {
                size: Size::from_percent(1.0, 1.0),
                ..Default::default()
            })
            .unwrap_or_else(|_| NodeId::from(0u64));
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let area = ui::widget::layout_rect(taffy, node, origin);
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
        // The window ground is the framework's; this paints content on it.
        let area = self.area;
        match self.model.mode {
            Mode::Calc => self.calc.prepare(tr, theme, &self.model, area),
            Mode::Graph2d => self.g2.prepare(tr, theme, &self.model, area),
            Mode::Graph3d => self.g3.prepare(tr, theme, &self.model, area),
        }

        // Content, then the panel (and the scrim of a floating drawer) over
        // it.
        let pointer = self.plot_pointer();
        match self.model.mode {
            Mode::Calc => {
                self.calc.field_focused = self.host_focused;
                self.calc.draw_content(scene, t, tr, theme, &self.model);
            }
            Mode::Graph2d => self
                .g2
                .draw_content(scene, t, tr, theme, &self.model, pointer),
            Mode::Graph3d => self
                .g3
                .draw_content(scene, t, tr, theme, &self.model, pointer),
        }
        match self.model.mode {
            Mode::Calc => {}
            Mode::Graph2d => {
                self.g2
                    .draw_panel(scene, t, tr, theme, &self.model, self.host_focused);
            }
            Mode::Graph3d => {
                self.g3
                    .draw_panel(scene, t, tr, theme, &self.model, self.host_focused);
            }
        }

        // Remote collaborators' carets, mapped into the active formula field's
        // 2D glyph layout (peers editing the same graph show up as coloured
        // carets inside the math, not just as a pointer over the window).
        let remote = match self.model.mode {
            Mode::Calc => Vec::new(),
            Mode::Graph2d | Mode::Graph3d => remote_carets(presence),
        };
        if !remote.is_empty()
            && let Some(kind) = self.model.mode.graph()
        {
            let active = self.model.active_graph(kind);
            self.panel(kind)
                .draw_remote_carets(scene, t, active, &remote);
        }
        // Moved, arrived or left carets change pixels no edit dirtied: report
        // them as this frame's damage (collected after drawing).
        if remote != self.drawn_remote_carets {
            self.drawn_remote_carets = remote;
            self.dirty = true;
        }
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

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = ctx.logical(position);
                self.mouse = p;
                let hover_changed = self.track_pointer(p);
                if self.field_dragging {
                    self.on_field_drag(p);
                } else if self.dragging {
                    let d = p - self.drag_last;
                    self.on_drag(d);
                    self.drag_last = p;
                }
                // Motion that changed no hover state, traces nothing and drags
                // nothing is inert: leave it unconsumed so the host can skip
                // the frame.
                *ctx.handled = self.dragging
                    || self.field_dragging
                    || hover_changed
                    || self.plot_pointer().trace.is_some();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // A press on the calculator makes it the keyboard's target,
                // as a press on any editor does: what is typed next goes to
                // the formula or expression it pressed, whichever widget
                // the host focused at launch.
                *ctx.focus_target = Some(ctx.node_id);
                self.on_press(self.mouse, &mut ctx.state);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.dragging = false;
                self.field_dragging = false;
                self.calc.field.on_release();
                self.g2.panel.release_fields();
                self.g3.panel.release_fields();
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
                self.on_key(&key_event.logical_key, *ctx.modifiers, &mut ctx.state);
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                self.on_key(
                    &WKey::Character(text.clone().into()),
                    winit::event::Modifiers::default(),
                    &mut ctx.state,
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
        // `model_dirty` is presentation-only. Durable changes have already
        // travelled through `perform_action`; never mirror this window's
        // mode, selection or viewport back into the shared document.
        self.model_dirty = false;
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
        self.model_dirty = false;
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

        // The modes, the angle unit and the panel's show/hide button are the
        // window bar's slots, reached through the bar.
        let mut children = Vec::new();
        match self.model.mode {
            Mode::Calc => {
                let mut controls = Vec::new();
                let mut display = AccessNode::new(Role::TextInput)
                    .with_name("Expression")
                    .with_value(self.model.expr.clone())
                    .with_description("Type an expression, then activate Evaluate or press Enter.")
                    .with_action_target("calculator.expression")
                    .with_bounds(self.calc.expression_rect())
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
                // Every value the display shows is a copy target of its own.
                for (slot, name) in [
                    (DisplaySlot::Expression, "Copy expression"),
                    (DisplaySlot::Result, "Copy result"),
                    (DisplaySlot::Answer, "Copy previous answer"),
                ] {
                    let mut node = AccessNode::new(Role::Button)
                        .with_name(name)
                        .with_action_target(format!("calculator.copy:{}", slot.name()))
                        .with_state(AccessState::new().focusable(true));
                    if self.calc.copy_rect(slot) != Rect::ZERO {
                        node = node.with_bounds(self.calc.copy_rect(slot));
                    }
                    children.push(node);
                }

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
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action;
        let (state, _) = ui::doc::document(state, path);
        // Assistive actions bypass `handle_event`; they may change anything.
        self.dirty = true;
        match target {
            "calculator.mode:0" if matches!(action, Action::Activate) => {
                self.switch_mode(Mode::Calc, state);
            }
            "calculator.mode:1" if matches!(action, Action::Activate) => {
                self.switch_mode(Mode::Graph2d, state);
            }
            "calculator.mode:2" if matches!(action, Action::Activate) => {
                self.switch_mode(Mode::Graph3d, state);
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
            _ if target.starts_with("calculator.copy:") && matches!(action, Action::Activate) => {
                let slot = match target.rsplit(':').next() {
                    Some("expression") => DisplaySlot::Expression,
                    Some("result") => DisplaySlot::Result,
                    Some("previous answer") => DisplaySlot::Answer,
                    _ => return false,
                };
                self.copy_display(slot);
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
                    self.apply_calc_key(&key);
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
                self.apply_calc_key(&key);
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
            Mode::Calc => false,
            Mode::Graph2d => self.g2.zoom_active() || self.g2.panel.side().is_animating(),
            Mode::Graph3d => self.g3.zoom_active() || self.g3.panel.side().is_animating(),
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

impl ui::widget::controls::ActionControls for CalculatorApp {}

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
}

impl CalculatorApp {
    /// Retained semantics for the custom-painted graph editor panels.
    fn graph_accessibility(&self, kind: GraphKind) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let (title, slug) = match kind {
            GraphKind::Curve2d => ("Functions", "2d"),
            GraphKind::Surface3d => ("Surfaces", "3d"),
        };
        let rows = self.model.graphs(kind);
        let panel = self.panel(kind);
        let open = panel.side().is_open();
        let add_bounds = panel.add_rect();
        let mut children = Vec::new();
        if open {
            for (index, row) in rows.iter().enumerate() {
                let (field_bounds, visible_bounds, delete_bounds) = (
                    panel.field_rect(index),
                    panel.visibility_rect(index),
                    panel.delete_rect(index),
                );
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

    fn perform_graph_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        doc: &mut ui::widget::Edits<'_>,
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
            (Some("panel"), None, Action::Activate | Action::ToggleCheck) => {
                self.panel_mut(kind).side_mut().toggle();
            }
            (Some("add"), None, Action::Activate) => self.add_graph(kind, "", doc),
            (Some("visible"), Some(index), Action::Activate | Action::ToggleCheck) => {
                return self.toggle_graph_row(kind, index, doc);
            }
            (Some("delete"), Some(index), Action::Activate) => {
                if index >= self.model.graphs(kind).len() {
                    return false;
                }
                self.delete_graph_row(kind, index, doc);
            }
            (Some("field"), Some(index), Action::SetValue(expr)) => {
                let changed = self
                    .model
                    .graphs(kind)
                    .get(index)
                    .is_some_and(|row| row.expr != *expr);
                if changed {
                    // A value set by assistive technology is one edit.
                    self.settle_edit();
                    self.set_graph_expr(kind, index, expr, doc);
                    self.settle_edit();
                }
            }
            (Some("field"), Some(index), Action::Activate | Action::Focus) => {
                if index >= self.model.graphs(kind).len() {
                    return false;
                }
                *self.model.active_graph_mut(kind) = index;
            }
            _ => return false,
        }
        true
    }
}

mod local_view;
mod window_bar;

#[cfg(test)]
mod tests;
