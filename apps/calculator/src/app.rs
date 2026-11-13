//! The calculator's root widget: it owns the [`CalcModel`], paints the active
//! mode's screen and routes keyboard and pointer input. Durable rows are
//! presented from the resource and changed only through its actions.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::TAU;
use std::rc::Rc;
use ui::i18n::ArgValue;
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
use ui::framework::{Dispatcher, Effected, Live, Publishes, ViewCx};
use ui::interaction::InteractionEnvironment;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AppModel, Element};

use crate::actions::{self, GraphKind};
use crate::convert_page::ConvertPage;
use crate::engine;
use crate::graph2d::{Graph2dScreen, Pointer};
use crate::graph3d::Graph3dScreen;
use crate::keypad::{CalcScreen, DisplaySlot, MEM_KEYS};
use crate::mathfield::MathField;
use crate::model::{self, CalcModel, KeypadPage, Mode, PlotFn};
use crate::plot_panel::PlotPanel;
use crate::resource::CATALOG;
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
    convert: ConvertPage,
    g2: Graph2dScreen,
    g3: Graph3dScreen,

    area: Rect,
    /// The last layout's theme: the event path has none of its own.
    theme: Theme,
    environment: InteractionEnvironment,
    /// The person has panned, orbited or zoomed, so the hint is gone.
    navigated: bool,

    mouse: Point,
    /// Until a pointer has been over the window, no trace is drawn.
    pointer_seen: bool,
    dragging: bool,
    drag_last: Point,
    field_dragging: bool,
    /// Host focus selects this component's record for undo and keyboard
    /// delivery; controls remain semantic children.
    host_focused: bool,
    last_click: Option<ui::time::Instant>,
    last_click_pos: Point,

    /// The last copy, watched until it settles: a refusal must be said.
    copy: ui::CopyWatch,
    model_dirty: bool,
    /// Something visible changed since the last paint; `mark_drawn` clears it.
    /// Dirty by default, so the desktop would repaint on every pointer move.
    dirty: bool,
    /// The carets last painted into a plot's formula; they follow peer pointers,
    /// which the window does not damage for.
    drawn_remote_carets: Vec<(Point, Color)>,
    asks: Rc<RefCell<Asks>>,
    node: Option<NodeId>,

    /// The resource's door: every affordance submits its edit or verb here.
    /// `None` for a bare widget in a layout test.
    dispatch: Option<Dispatcher<Calculator>>,
    document: Option<Live<Calculator>>,
    followed: Option<(u64, u64)>,
    /// One presenter for the instance's life, so History keeps its faces.
    entries: Rc<crate::history_face::CalculatorEntries>,
    rows: Rc<Calculator>,
    select: Option<(GraphKind, Id)>,
    /// The bar as last declared: a press names one of its slots, then the window
    /// declares the bar for what it shows.
    bar: Option<ui::framework::WindowBar<Calculator>>,
}

#[derive(Default)]
struct Asks {
    effect: Option<actions::CalculatorEffect>,
    stage: Option<Staged>,
}

struct Staged(Option<String>);

impl Asks {
    /// Showing a graph for the first time adds its starter row.
    fn queue_effect(
        &mut self,
        rows: &Calculator,
        effect: actions::CalculatorEffect,
    ) -> Result<(), String> {
        if let actions::CalculatorEffect::Open { entry } = &effect
            && rows.history.get(&Id::from_raw(entry.as_str())).is_none()
        {
            return Err("Calculation no longer exists".into());
        }
        self.effect = Some(effect);
        Ok(())
    }
}

/// The calculator window as the view of [`Calculator`]: it follows the
/// document's rows, takes the effects the view answers and publishes each
/// history entry as a live row.
pub(crate) fn window(cx: &ViewCx<Calculator>) -> Element {
    let mut window = CalculatorApp::new();
    window.dispatch = Some(cx.dispatcher());
    window.bar = window.window_bar();
    window.document = Some(cx.live().clone());
    window.follow_document();
    {
        let (asks, live) = (Rc::clone(&window.asks), cx.live().clone());
        cx.effects(move |effect: actions::CalculatorEffect, _| {
            live.with(|rows| asks.borrow_mut().queue_effect(rows, effect))?;
            Ok(Effected::done())
        });
    }
    {
        let asks = Rc::clone(&window.asks);
        cx.on_stage(move |scenario| {
            asks.borrow_mut().stage = Some(Staged(scenario.map(str::to_owned)));
        });
    }
    let live = cx.live().clone();
    cx.publishing(
        Publishes::new().live_rows(move || {
            live.with(|rows| {
                rows.history
                    .iter()
                    .map(|(id, entry)| crate::resource::history_row(id, entry))
                    .collect()
            })
        }),
        Box::new(window),
    )
}

impl CalculatorApp {
    pub fn new() -> Self {
        Self::with_model(CalcModel::default())
    }

    #[cfg(test)]
    pub(crate) fn seeded() -> Self {
        Self::with_model(CalcModel::seeded())
    }

    fn with_model(model: CalcModel) -> Self {
        Self {
            model,
            copy: ui::CopyWatch::default(),
            calc: CalcScreen::default(),
            convert: ConvertPage::new(),
            g2: Graph2dScreen::default(),
            g3: Graph3dScreen::default(),
            area: Rect::ZERO,
            theme: Theme::dark(),
            environment: InteractionEnvironment::POINTER,
            navigated: false,
            mouse: Point::ZERO,
            pointer_seen: false,
            dragging: false,
            drag_last: Point::ZERO,
            field_dragging: false,
            host_focused: false,
            last_click: None,
            last_click_pos: Point::ZERO,
            model_dirty: false,
            dirty: true,
            drawn_remote_carets: Vec::new(),
            asks: Rc::default(),
            node: None,
            dispatch: None,
            document: None,
            followed: None,
            entries: Rc::default(),
            rows: Rc::default(),
            select: None,
            bar: None,
        }
    }

    fn follow_document(&mut self) {
        let Some(document) = &self.document else {
            return;
        };
        let Some(rows) = document.changed_since(&mut self.followed) else {
            return;
        };
        self.adopt_resource_state(&rows);
    }

    fn stage_asked(&mut self) {
        let stage = self.asks.borrow_mut().stage.take();
        if let Some(Staged(scenario)) = stage {
            self.stage_scenario(scenario.as_deref());
        }
    }

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
                // Not presented yet: keep waiting.
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

    /// Stages a capture's view over the rows its actions wrote: the mode, the
    /// expression, the memory register, which panel is open. Never a live
    /// window.
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
            Some("convert" | "convert-currency") => {
                self.set_mode(Mode::Convert);
                // 40 °C in °F.
                self.convert
                    .apply(&crate::convert_page::ConvertAction::Quantity(
                        crate::convert::Dimension::Temperature,
                    ));
                self.convert
                    .set_field_text(crate::model::CONVERT_DEMO_AMOUNT);
                if scenario == Some("convert-currency") {
                    self.convert
                        .apply(&crate::convert_page::ConvertAction::Currency);
                }
                self.relayout();
            }
            Some("graph3d" | "narrow-graph3d") => self.set_mode(Mode::Graph3d),
            _ => self.set_mode(Mode::Graph2d),
        }
        // A drawer docks or floats by the window it was laid out in, so lay the
        // mode out first.
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
    }

    #[cfg(test)]
    pub(crate) fn mode_for_test(&self) -> Mode {
        self.model.mode
    }

    pub(crate) fn copy_watch_mut(&mut self) -> &mut ui::CopyWatch {
        &mut self.copy
    }

    pub(crate) fn convert_page(&self) -> &ConvertPage {
        &self.convert
    }

    pub(crate) fn convert_page_mut(&mut self) -> &mut ConvertPage {
        &mut self.convert
    }

    /// Derived from the current size every call, so a resize across a
    /// breakpoint reflows.
    fn relayout(&mut self) {
        self.layout_active();
    }

    fn layout_active(&mut self) {
        match self.model.mode {
            Mode::Calc => self.calc.layout(self.area),
            Mode::Convert => self.convert.layout(self.area, &self.theme.clone()),
            Mode::Graph2d => self.g2.layout(self.area, &self.model),
            Mode::Graph3d => self.g3.layout(self.area, &self.model),
        }
    }

    /// The one way this window changes the document, shared with the palette,
    /// agents and scripts. It applies inside the event, so a formula's next
    /// keystroke builds on it, and the caller updates the model for feedback.
    ///
    /// Only for an action that really mutates the document: the affordances gate
    /// on a real change.
    fn submit(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<Calculator>,
    ) {
        if let Some(dispatch) = &self.dispatch {
            // A refusal writes nothing; the stored rows are presented again.
            let _ = dispatch.submit_now(doc, submit);
        }
    }

    fn settle_edit(&self) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.settle();
        }
    }

    /// Adds it, selects it and submits its `AddGraph`.
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

    /// Evaluates, updates the display, then dispatches
    /// [`CommitHistory`](CalculatorAction::CommitHistory) so the row lands on the
    /// shared, undoable path. A failed or empty evaluation dispatches nothing.
    fn commit_equals(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if let Some(entry) = self.model.commit_equals() {
            self.submit(
                doc,
                Calculator::COMMIT_HISTORY.with((entry.expr, entry.result)),
            );
        }
        self.model_dirty = true;
    }

    fn set_mode(&mut self, m: Mode) {
        if self.model.mode != m {
            self.model.mode = m;
            // Settle both animators, so no stale ease carries into the next
            // visit.
            self.g2.settle_zoom(self.model.view2d);
            self.g3.settle_zoom(self.model.view3d.zoom);
            self.model_dirty = true;
        }
    }

    /// A first visit to an empty graph mode adds a starter row through the same
    /// `AddGraph` action the palette offers, so it is undoable and syncs.
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

    fn set_degrees(&mut self, degrees: bool) {
        if self.model.deg != degrees {
            self.model.deg = degrees;
            self.model_dirty = true;
        }
    }

    /// A floating drawer is up, so the content beneath takes no presses.
    fn drawer_blocks(&self) -> bool {
        match self.model.mode.graph() {
            None => false,
            Some(kind) => self.panel(kind).floats_open(),
        }
    }

    /// Returns whether anything painted changed.
    fn track_pointer(&mut self, p: Point) -> bool {
        match self.model.mode.graph() {
            None => self.calc.set_pointer(p),
            Some(kind) => self.panel_mut(kind).set_pointer(p),
        }
    }

    fn plot_pointer(&self) -> Pointer {
        let over = self.pointer_seen
            && self
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
        // A press ends the edit in progress: the next keystroke is its own undo
        // step.
        self.settle_edit();

        let now = ui::time::now();
        let double = self
            .last_click
            .is_some_and(|t| now.duration_since(t).as_millis() <= 400)
            && (self.last_click_pos - p).hypot() <= 5.0;
        self.last_click = Some(now);
        self.last_click_pos = p;

        if self.model.mode == Mode::Convert {
            self.on_press_convert(p);
            return;
        }
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
            // The field has no renderer here, so the press hit-tests its cached
            // layout; it relays out on the next draw.
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
            // A press inside an open drawer must not pan or orbit what is under
            // it.
        } else if self.canvas_contains(kind, p) {
            self.dragging = true;
            self.drag_last = p;
        }
    }

    fn on_press_calc(&mut self, p: Point, doc: &mut ui::widget::Edits<'_>) {
        if let Some(i) = self.calc.key_at(p) {
            self.calc.pressed_key = Some(i);
            if let Some(key) = self.calc.keys().get(i).cloned() {
                // `=` commits through an action; every other key edits the
                // expression line.
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

    /// The same stable, tombstoned action the palette offers. The model is not
    /// spliced optimistically: the presented rows are the merged list a
    /// collaborator sees, and `adopt_resource_state` clamps the active row.
    fn delete_graph_row(&mut self, kind: GraphKind, index: usize, doc: &mut ui::widget::Edits<'_>) {
        if let Some(id) = self.rows.graph_id(kind, index).cloned() {
            match kind {
                GraphKind::Curve2d => self.submit(doc, Calculator::DELETE_FUNCTION.with(id)),
                GraphKind::Surface3d => self.submit(doc, Calculator::DELETE_SURFACE.with(id)),
            }
            self.model_dirty = true;
        }
    }

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
                self.g2.settle_zoom(self.model.view2d);
                self.model_dirty = true;
            }
            Mode::Graph3d => {
                // Within one turn, however far it orbits.
                let yaw = self.model.view3d.yaw + d.x * 0.01;
                self.model.view3d.yaw = yaw - TAU * (yaw / TAU).round();
                self.model.view3d.pitch = (self.model.view3d.pitch + d.y * 0.01).clamp(-1.5, 1.5);
                // An orbit re-projects, so settle any zoom ease.
                self.g3.settle_zoom(self.model.view3d.zoom);
                self.model_dirty = true;
            }
            Mode::Calc | Mode::Convert => {}
        }
    }

    /// The overflowing axis moves, the other stays; false with no editor there.
    fn scroll_field_under_pointer(&mut self, dx: f64, dy: f64) -> bool {
        if let Some(kind) = self.model.mode.graph()
            && let Some(i) = self.panel(kind).field_at(self.mouse)
        {
            if let Some(f) = self.graph_field(kind, i) {
                f.scroll_by(dx, dy);
            }
            return true;
        }
        if self.model.mode == Mode::Calc && self.calc.expression_rect().contains(self.mouse) {
            self.calc.field.scroll_by(dx, dy);
            return true;
        }
        false
    }

    fn on_scroll(&mut self, delta: &MouseScrollDelta, scale_factor: f64, ctrl: bool) {
        match delta {
            MouseScrollDelta::LineDelta(_, y) => self.on_wheel(f64::from(*y)),
            // A touchpad pinch arrives as a Ctrl-scroll on some platforms.
            MouseScrollDelta::PixelDelta(pos) if ctrl => self.on_wheel(pos.y / 40.0),
            MouseScrollDelta::PixelDelta(pos) => {
                self.on_pixel_scroll(Vec2::new(pos.x, pos.y) / scale_factor);
            }
        }
    }

    fn on_wheel(&mut self, step: f64) {
        if !self.scroll_field_under_pointer(-step * 24.0, -step * 24.0) {
            self.zoom_at_pointer((1.0 + step * 0.12).clamp(0.4, 2.5));
        }
    }

    /// An editor scrolls, and a graph's content follows as under a drag; zoom is
    /// the pinch's.
    fn on_pixel_scroll(&mut self, d: Vec2) {
        if self.scroll_field_under_pointer(-d.x, -d.y) {
            return;
        }
        if let Some(kind) = self.model.mode.graph()
            && self.canvas_contains(kind, self.mouse)
        {
            self.on_drag(d);
        }
    }

    fn zoom_at_pointer(&mut self, factor: f64) {
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
                // Pin the world point under the cursor for the whole ease; the
                // resample waits until it settles.
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

    fn ignores_unfocused_input(&self, event: &WindowEvent) -> bool {
        !self.host_focused
            && matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            )
    }

    /// Escape with nothing to dismiss is left to the host, whose Back navigates
    /// out of the app.
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

        if self.on_global_shortcut(key, ctrl, shift, doc) {
            return;
        }

        if let Some(kind) = self.model.mode.graph()
            && self.on_key_graph(kind, key, modifiers, doc)
        {
            return;
        }

        match key {
            WKey::Named(NamedKey::Enter) => match self.model.mode {
                // Enter evaluates the expression, committing it through an
                // action.
                Mode::Calc => {
                    self.commit_equals(doc);
                }
                // Enter appends an empty row and focuses it; the active row's
                // edit already reached the model keystroke by keystroke.
                Mode::Graph2d => self.add_graph(GraphKind::Curve2d, "", doc),
                Mode::Graph3d => self.add_graph(GraphKind::Surface3d, "", doc),
                Mode::Convert => {}
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

    fn on_global_shortcut(
        &mut self,
        key: &WKey,
        ctrl: bool,
        shift: bool,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        // A selection keeps the editor's own copy first; otherwise Ctrl+C takes
        // the result and Ctrl+Shift+C the expression.
        if ctrl
            && self.model.mode == Mode::Calc
            && let WKey::Character(s) = key
            && s.eq_ignore_ascii_case("c")
        {
            if shift {
                self.copy_display(DisplaySlot::Expression);
                return true;
            }
            self.calc.field.set_source(&self.model.expr);
            if !self.calc.field.has_selection() {
                self.copy_display(DisplaySlot::Result);
                return true;
            }
        }

        // Ctrl+digit always switches mode, from any screen.
        if ctrl && let WKey::Character(s) = key {
            match s.as_str() {
                "1" => {
                    self.switch_mode(Mode::Calc, doc);
                    return true;
                }
                "2" => {
                    self.switch_mode(Mode::Graph2d, doc);
                    return true;
                }
                "3" => {
                    self.switch_mode(Mode::Graph3d, doc);
                    return true;
                }
                "4" => {
                    self.switch_mode(Mode::Convert, doc);
                    return true;
                }
                _ => {}
            }
        }

        // Escape dismisses drawers first: an overlay must never trap a keyboard
        // user behind its scrim.
        if matches!(key, WKey::Named(NamedKey::Escape)) {
            let dismissed = match self.model.mode.graph() {
                Some(kind) if self.panel(kind).side().is_open() => {
                    self.panel_mut(kind).side_mut().close();
                    true
                }
                _ => false,
            };
            if dismissed {
                return true;
            }
        }

        false
    }

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

    /// Exactly as painted, so copying takes what the person read.
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

    /// The expression as typed, the result as shown, the answer as Ans.
    fn copy_display(&mut self, slot: DisplaySlot) {
        let Some(text) = self.display_text(slot) else {
            return;
        };
        // A clipboard that refuses is told, not confirmed.
        let copied = self.copy.watch(ui::try_clipboard_copy(&text));
        self.calc.say_copied(slot, copied);
        self.model_dirty = true;
    }

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

    /// `true` when the field consumed the key.
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
        // Only keys the field handles: mode switches fall through.
        if !field_handles(key, modifiers) {
            return false;
        }
        if field.handle_key(key, &modifiers) {
            let linear = field.current_linear();
            self.set_graph_expr(kind, active, &linear, doc);
        }
        true
    }

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

    fn graph_field(&mut self, kind: GraphKind, index: usize) -> Option<&mut MathField> {
        let panel = match kind {
            GraphKind::Curve2d => &mut self.g2.panel,
            GraphKind::Surface3d => &mut self.g3.panel,
        };
        panel.sync_fields(self.model.graphs(kind));
        panel.active_field_mut(index)
    }

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

/// The peers' cursors as `(scene point, colour)`, in the peer palette's
/// colours, for the math fields to snap onto their glyph layout.
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

/// Whether the math field acts on the key, so the app routes it there and
/// consumes it.
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

/// Intent rather than a lone glyph for the custom-painted keypad.
fn key_accessible_name(key: &model::Key) -> String {
    use model::Key;
    let message = match key {
        Key::Equals => "calculator-key-equals",
        Key::Clear => "calculator-key-clear",
        Key::Backspace => "calculator-key-backspace",
        Key::ToggleSign => "calculator-key-sign",
        Key::Percent => "calculator-key-percent",
        Key::Ans => "calculator-key-answer",
        Key::MemAdd => "calculator-key-mem-add",
        Key::MemSub => "calculator-key-mem-sub",
        Key::MemRecall => "calculator-key-mem-recall",
        Key::MemClear => "calculator-key-mem-clear",
        Key::Wrap(_, _) => "calculator-key-square",
        Key::Page(KeypadPage::Functions) => "calculator-key-functions-page",
        Key::Page(KeypadPage::Primary) => "calculator-key-numbers-page",
        Key::Ins("sin(") => "calculator-key-sin",
        Key::Ins("cos(") => "calculator-key-cos",
        Key::Ins("tan(") => "calculator-key-tan",
        Key::Ins("asin(") => "calculator-key-asin",
        Key::Ins("acos(") => "calculator-key-acos",
        Key::Ins("atan(") => "calculator-key-atan",
        Key::Ins("sqrt(") => "calculator-key-sqrt",
        Key::Ins("int(") => "calculator-key-integral",
        Key::Ins("root(") => "calculator-key-root",
        Key::Ins("abs(") => "calculator-key-abs",
        Key::Ins("sinh(") => "calculator-key-sinh",
        Key::Ins("cosh(") => "calculator-key-cosh",
        Key::Ins("tanh(") => "calculator-key-tanh",
        Key::Ins("gamma(") => "calculator-key-gamma",
        Key::Ins("ln(") => "calculator-key-ln",
        Key::Ins("log10(") => "calculator-key-log10",
        Key::Ins("exp(") => "calculator-key-exp",
        Key::Ins("1/(") => "calculator-key-reciprocal",
        Key::Ins("^") => "calculator-key-power",
        Key::Ins("*") => "calculator-key-multiply",
        Key::Ins("/") => "calculator-key-divide",
        Key::Ins("+") => "calculator-key-add",
        Key::Ins("-") => "calculator-key-subtract",
        Key::Ins("pi") => "calculator-key-pi",
        Key::Ins("e") => "calculator-key-euler",
        _ => {
            return CATALOG.t_args(
                "calculator-key-other",
                &[("key", ArgValue::Text(key.label().to_owned()))],
            );
        }
    };
    CATALOG.t(message)
}

impl CalculatorApp {
    /// Taken on the next settle: showing a graph the first time adds its starter
    /// row.
    pub(crate) fn queue_effect(&mut self, effect: actions::CalculatorEffect) -> Result<(), String> {
        self.asks.borrow_mut().queue_effect(&self.rows, effect)?;
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
            actions::CalculatorEffect::ShowConvert => Mode::Convert,
            actions::CalculatorEffect::UseDegrees => {
                self.set_degrees(true);
                return;
            }
            actions::CalculatorEffect::UseRadians => {
                self.set_degrees(false);
                return;
            }
            actions::CalculatorEffect::TogglePanel => {
                if let Some(kind) = self.model.mode.graph() {
                    self.panel_mut(kind).side_mut().toggle();
                    self.dirty = true;
                }
                return;
            }
        };
        // Presentation only, except that a first graph visit adds its starter
        // row through the resource's action.
        self.switch_mode(mode, doc);
    }
}

/// Each past calculation reads as itself; choosing one puts it back through the
/// Use calculation effect, the door the palette and agents take too.
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
        dyn ui::history_mode::capability::PresentsHistoryEntries,
        dyn ui::application_view::ApplicationViewState
    );

    fn is_focusable(&self) -> bool {
        true
    }

    /// What is typed goes to the expression or formula being edited, so the
    /// calculator takes the keyboard whenever its window does.
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

    /// A finger on a plot pans or orbits, as a mouse drag does; two pinch.
    fn touch_claim(&self, point: Point) -> ui::interaction::TouchClaim {
        match self.model.mode.graph() {
            Some(kind)
                if self.canvas_contains(kind, point) && !self.panel(kind).side().covers(point) =>
            {
                ui::interaction::TouchClaim::Drag
            }
            _ => ui::interaction::TouchClaim::Pointer,
        }
    }

    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        let content = match self.model.mode.graph() {
            None => ui::ime::InputContent::TEXT.own_keys(),
            Some(_) => ui::ime::InputContent::TEXT,
        };
        self.host_focused.then_some(content)
    }

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        self.follow_document();
        self.stage_asked();
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        let asked = self.asks.borrow().effect.is_some() || self.asks.borrow().stage.is_some();
        self.dirty |= asked;
        asked
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, _path: &automerge::ObjId) {
        let effect = self.asks.borrow_mut().effect.take();
        if let Some(effect) = effect {
            self.perform_effect(effect, edits);
        }
        if let Some(bar) = &self.bar {
            bar.take_events();
        }
        self.bar = self.window_bar();
        if let Some(bar) = &self.bar {
            bar.declare();
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        // `new_leaf` errs only at capacity: an inert node beats aborting the
        // session.
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
        let area = self.area;
        match self.model.mode {
            Mode::Calc => self.calc.prepare(tr, theme, &self.model, area),
            Mode::Convert => self.convert.layout(area, theme),
            Mode::Graph2d => self.g2.prepare(tr, theme, &self.model, area),
            Mode::Graph3d => self.g3.prepare(tr, theme, &self.model, area),
        }

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
            Mode::Convert => self.convert.draw(scene, t, tr, theme, presence),
        }
        match self.model.mode {
            Mode::Calc | Mode::Convert => {}
            Mode::Graph2d => {
                self.g2
                    .draw_panel(scene, t, tr, theme, &self.model, self.host_focused);
            }
            Mode::Graph3d => {
                self.g3
                    .draw_panel(scene, t, tr, theme, &self.model, self.host_focused);
            }
        }

        // A peer editing the same graph shows as a coloured caret inside the
        // math, not only as a pointer over the window.
        let remote = match self.model.mode {
            Mode::Calc | Mode::Convert => Vec::new(),
            Mode::Graph2d | Mode::Graph3d => remote_carets(presence),
        };
        if !remote.is_empty()
            && let Some(kind) = self.model.mode.graph()
        {
            let active = self.model.active_graph(kind);
            self.panel(kind)
                .draw_remote_carets(scene, t, active, &remote);
        }
        // Carets that moved change pixels no edit dirtied, so report them as
        // this frame's damage.
        if remote != self.drawn_remote_carets {
            self.drawn_remote_carets = remote;
            self.dirty = true;
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Keyboard and IME input belongs to the host-selected Calculator; an
        // unfocused instance leaves it unclaimed for its sibling. Pointer routing
        // stays geometry-based.
        if self.ignores_unfocused_input(event) {
            return;
        }
        self.relayout();

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = ctx.logical(position);
                self.mouse = p;
                self.pointer_seen = true;
                let hover_changed = self.track_pointer(p);
                if self.field_dragging {
                    self.on_field_drag(p);
                } else if self.dragging {
                    let d = p - self.drag_last;
                    self.on_drag(d);
                    self.drag_last = p;
                }
                // Inert motion is left unconsumed, so the host can skip the
                // frame.
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
                // A press makes the calculator the keyboard's target, as on any
                // editor: what is typed next goes where it pressed.
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
                let ctrl = ctx.modifiers.state().control_key();
                self.on_scroll(delta, ctx.scale_factor, ctrl);
                *ctx.handled = true;
            }
            WindowEvent::PinchGesture { delta, .. } => {
                self.zoom_at_pointer((1.0 + delta).clamp(0.4, 2.5));
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                *ctx.handled = true;
                if self.model.mode == Mode::Convert {
                    // The amount is the page's one editable value.
                    self.convert_key(event, ctx);
                } else {
                    *ctx.handled = !self.escape_is_idle(&key_event.logical_key);
                    self.on_key(&key_event.logical_key, *ctx.modifiers, &mut ctx.state);
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(_)) if self.model.mode == Mode::Convert => {
                self.convert_key(event, ctx);
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

        // Everything but inert motion may have changed what is drawn.
        if !matches!(event, WindowEvent::CursorMoved { .. }) || *ctx.handled || self.model_dirty {
            self.dirty = true;
        }
        // Never mirror this window's mode, selection or viewport back into the
        // document: durable changes travel through `perform_action`.
        self.model_dirty = false;
    }

    fn name(&self) -> &'static str {
        "CalculatorApp"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn needs_redraw(&self) -> bool {
        // `model_dirty` covers changes that arrive outside `handle_event`, before
        // the next event.
        self.dirty || self.model_dirty || self.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.model_dirty = false;
    }

    /// The calculator is custom-painted, so it publishes the complete retained
    /// control tree; each control below activates as its pointer counterpart
    /// does.
    #[allow(
        clippy::too_many_lines,
        reason = "the retained tree is deliberately kept beside its Widget hook so every custom-painted control is auditable together"
    )]
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};

        // The modes, the angle unit and the panel button are the bar's slots.
        let mut children = Vec::new();
        match self.model.mode {
            Mode::Calc => {
                let mut controls = Vec::new();
                let mut display = AccessNode::new(Role::TextInput)
                    .with_name(crate::resource::CATALOG.t("calculator-a11y-expression"))
                    .with_value(self.model.expr.clone())
                    .with_description(crate::resource::CATALOG.t("calculator-a11y-expression-hint"))
                    .with_action_target("calculator.expression")
                    .with_bounds(self.calc.expression_rect())
                    .with_state(AccessState::new().focusable(true))
                    .with_hashed_key("calculator.display");
                if !self.model.expr.trim().is_empty() {
                    let outcome = match engine::evaluate(&self.model.expr, &self.model.ctx()) {
                        Ok(value) => CATALOG.t_args(
                            "calculator-result",
                            &[(
                                "value",
                                ui::i18n::ArgValue::Text(engine::format_number(value)),
                            )],
                        ),
                        Err(error) => CATALOG.t_args(
                            "calculator-invalid-expression",
                            &[("detail", ui::i18n::ArgValue::Text(error))],
                        ),
                    };
                    display = display.with_children(vec![
                        AccessNode::new(Role::StaticText)
                            .with_name(outcome)
                            .with_live(Politeness::Polite)
                            .with_hashed_key("calculator.display.outcome"),
                    ]);
                }
                children.push(display);
                // Each displayed value is a copy target.
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
                        .with_name(crate::resource::CATALOG.t("calculator-a11y-controls"))
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
                        .with_name(crate::resource::CATALOG.t("calculator-a11y-keypad"))
                        .with_description(crate::resource::CATALOG.t("calculator-a11y-keypad-hint"))
                        .with_hashed_key("calculator.keypad")
                        .with_children(keys),
                );
            }
            Mode::Convert => children.push(self.convert_accessibility()),
            Mode::Graph2d => children.push(self.graph_accessibility(GraphKind::Curve2d)),
            Mode::Graph3d => children.push(self.graph_accessibility(GraphKind::Surface3d)),
        }
        Some(
            AccessNode::new(Role::Group)
                .with_name(crate::resource::CATALOG.t("calculator-title"))
                .with_value(if self.model.expr.is_empty() {
                    "Empty expression".to_string()
                } else {
                    self.model.expr.clone()
                })
                .with_description(crate::resource::CATALOG.t("calculator-a11y-workspace"))
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
        // Assistive actions bypass `handle_event`, so they may change anything.
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
            "calculator.mode:3" if matches!(action, Action::Activate) => {
                self.switch_mode(Mode::Convert, state);
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
            _ if target.starts_with("calculator.convert:")
                && matches!(action, Action::Activate) =>
            {
                let Some(convert) = ConvertPage::action_for_target(target) else {
                    return false;
                };
                self.apply_convert_action(&convert);
            }
            _ if target.starts_with("calculator.graph.") => {
                return self.perform_graph_semantic_action(target, action, state);
            }
            _ => return false,
        }
        true
    }
    /// Keeps the host pumping frames while a drawer eases open or a zoom eases
    /// toward its target; otherwise the animation freezes after one frame. Gated
    /// by the mode, so a switch cannot pump frames forever.
    fn needs_animation_frame(&self) -> bool {
        match self.model.mode {
            Mode::Calc | Mode::Convert => false,
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
    /// One self-drawn leaf, so only the first layout is a size change.
    fn is_layout_dirty(&self) -> bool {
        self.area == Rect::ZERO
    }
}

impl ui::widget::AppWidget for CalculatorApp {
    fn app_kind(&self) -> ui::widget::AppKind {
        // The glyph vocabulary has no Utility variant; an expression editor is
        // the closest kind.
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
    fn graph_accessibility(&self, kind: GraphKind) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let (title, slug) = match kind {
            GraphKind::Curve2d => (CATALOG.t("calculator-graph-functions"), "2d"),
            GraphKind::Surface3d => (CATALOG.t("calculator-graph-surfaces"), "3d"),
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
                    .with_name(CATALOG.t_args(
                        "calculator-graph-field",
                        &[
                            (
                                "kind",
                                ArgValue::Text(title.trim_end_matches('s').to_owned()),
                            ),
                            ("number", ArgValue::Text((index + 1).to_string())),
                        ],
                    ))
                    .with_value(row.expr.clone())
                    .with_action_target(format!("calculator.graph.field:{slug}:{index}"))
                    .with_state(AccessState::new().focusable(true));
                if let Some(bounds) = field_bounds {
                    field = field.with_bounds(bounds);
                }
                let mut visible = AccessNode::new(Role::ToggleButton)
                    .with_name(CATALOG.t_args(
                        "calculator-graph-show",
                        &[
                            (
                                "kind",
                                ArgValue::Text(title.trim_end_matches('s').to_owned()),
                            ),
                            ("number", ArgValue::Text((index + 1).to_string())),
                        ],
                    ))
                    .with_action_target(format!("calculator.graph.visible:{slug}:{index}"))
                    .with_state(AccessState::new().focusable(true).checked(row.visible));
                if let Some(bounds) = visible_bounds {
                    visible = visible.with_bounds(bounds);
                }
                let mut delete = AccessNode::new(Role::Button)
                    .with_name(CATALOG.t_args(
                        "calculator-graph-delete",
                        &[
                            (
                                "kind",
                                ArgValue::Text(title.trim_end_matches('s').to_owned()),
                            ),
                            ("number", ArgValue::Text((index + 1).to_string())),
                        ],
                    ))
                    .with_action_target(format!("calculator.graph.delete:{slug}:{index}"))
                    .with_description(crate::resource::CATALOG.t("calculator-a11y-remove-formula"))
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
                .with_name(CATALOG.t_args(
                    "calculator-graph-add",
                    &[(
                        "kind",
                        ArgValue::Text(title.trim_end_matches('s').to_owned()),
                    )],
                ))
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
