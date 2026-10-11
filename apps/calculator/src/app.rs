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
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey};

use ui::Widget;
use ui::framework::Dispatcher;
use ui::interaction::InteractionEnvironment;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ControlSize, SegmentedFace, SegmentedStyle, ToggleGroup};

use crate::actions::{self, CalculatorAction, GraphKind};
use crate::chrome::Header;
use crate::engine;
use crate::graph2d::{Graph2dScreen, Pointer};
use crate::graph3d::Graph3dScreen;
use crate::keypad::{CalcScreen, MEM_KEYS};
use crate::mathfield::MathField;
use crate::model::{self, CalcModel, Mode, PlotFn};
use crate::plot_panel::PlotPanel;
use crate::resource::{Calculator, CalculatorState};
use ui::chrome::HeaderRowMetrics;

/// The labels of the angle-unit strip, and the condensed forms for a narrow
/// header.
const UNIT_LABELS: [&str; 2] = ["Radians", "Degrees"];
const UNIT_SHORT: [&str; 2] = ["Rad", "Deg"];

/// Painted widths of the header strips, measured at the size they paint at
/// (see `CalculatorApp::measure_header`). Zero before the first draw.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct StripWidths {
    /// The mode strip with every full label ("Calculator", "Graph 2D", ...).
    mode_full: f64,
    /// The mode strip with the condensed labels ("Calc", "2D", "3D").
    mode_short: f64,
    /// The angle-unit strip with its full labels, and condensed.
    unit_full: f64,
    unit_short: f64,
}

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
    /// The header row's measurements in the last theme.
    header: Header,
    /// The window header row's measurements in the last theme.
    row_metrics: HeaderRowMetrics,
    /// Holds the window's History control the host offers.
    history: ui::widget::HistoryOpener,
    /// Where the History control stands in the window's trailing corner.
    history_rect: Rect,
    /// The retained History button was replaced: the layout tree is rebuilt.
    structure_dirty: bool,
    /// The header row over the content column.
    header_band: Rect,
    seg_rects: [Rect; 3],
    unit_rects: [Rect; 2],
    /// Whether the mode segments are too narrow for full labels and must use
    /// the short forms ("Calc" / "2D" / "3D"). Re-derived every `relayout`.
    seg_short: bool,
    /// Whether the unit strip condenses too ("Rad" / "Deg").
    unit_short: bool,
    /// The header strips' widths as the renderer actually paints them,
    /// refreshed each draw. `relayout` sizes the strips from these so
    /// enlarged type widens its control instead of overrunning a neighbour.
    strips: StripWidths,
    /// The strips' height.
    strip_height: f64,
    /// The segment under the pointer in the mode strip / the unit strip.
    mode_hover: Option<usize>,
    unit_hover: Option<usize>,
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

    /// The resource's door: every on-screen affordance submits its
    /// [`CalculatorAction`] here. `None` until the framework attaches the
    /// surface, and for a bare widget in a layout test.
    dispatch: Option<Dispatcher<Calculator>>,
    /// The durable rows last presented, with their stable ids.
    rows: Rc<CalculatorState>,
    /// A graph row this window just added, to select once it is presented.
    select: Option<(GraphKind, String)>,
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
            header: Header::default(),
            row_metrics: HeaderRowMetrics::default(),
            history: ui::widget::HistoryOpener::new(),
            history_rect: Rect::ZERO,
            structure_dirty: false,
            header_band: Rect::ZERO,
            seg_rects: [Rect::ZERO; 3],
            unit_rects: [Rect::ZERO; 2],
            seg_short: false,
            unit_short: false,
            strips: StripWidths::default(),
            strip_height: SegmentedFace::height(ControlSize::Regular, &Theme::dark()),
            mode_hover: None,
            unit_hover: None,
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
            rows: Rc::default(),
            select: None,
        }
    }

    /// Adopt durable history and graph rows from the framework resource state.
    pub(crate) fn adopt_resource_state(&mut self, state: &Rc<CalculatorState>) {
        self.model.history = state.history.iter().map(|(_, e)| e.clone()).collect();
        self.model.funcs = state.funcs.iter().map(|(_, f)| f.clone()).collect();
        self.model.surfaces = state.surfaces.iter().map(|(_, f)| f.clone()).collect();
        self.rows = Rc::clone(state);
        if let Some((kind, id)) = self.select.take() {
            match state.graphs(kind).iter().position(|(row, _)| *row == id) {
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
            Some(
                "calculator" | "history-open" | "history-closed" | "narrow" | "narrow-history",
            ) => {
                self.set_mode(Mode::Calc);
            }
            Some("error") => self.model.apply_error_demo(),
            Some("graph3d" | "narrow-graph3d") => self.set_mode(Mode::Graph3d),
            _ => self.set_mode(Mode::Graph2d),
        }
        // The drawer decides docked or floating from the window it has been
        // laid out in, so lay the mode out before staging its panel.
        self.relayout();
        match scenario {
            Some("history-open" | "narrow-history") => self.calc.snap_sidebar(true),
            Some("history-closed") => self.calc.snap_sidebar(false),
            Some("narrow-functions") => self.g2.panel.side_mut().snap(true),
            _ => {}
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

    /// Lay out the active screen in the window, then the header row over its
    /// content column. Derived from the current size every call, so a resize
    /// between breakpoints reflows rather than staying frozen at the launch
    /// layout.
    fn relayout(&mut self) {
        let reserve = if self.history.is_shown() {
            self.row_metrics.chip + self.row_metrics.gap
        } else {
            0.0
        };
        self.calc.set_trailing_reserve(reserve);
        self.layout_active();
        self.layout_header();
    }

    fn layout_active(&mut self) {
        match self.model.mode {
            Mode::Calc => self.calc.layout(self.area, &self.model),
            Mode::Graph2d => self.g2.layout(self.area, &self.model),
            Mode::Graph3d => self.g3.layout(self.area, &self.model),
        }
    }

    /// The column the header's mode and unit controls sit over: the one beside
    /// the active screen's panel.
    fn content_column(&self) -> Rect {
        match self.model.mode.graph() {
            None => self.calc.content_column(),
            Some(kind) => self.panel(kind).side().content(),
        }
    }

    /// Whether the panel's show/hide button sits in the content column's
    /// header (the panel is hidden), and at which end.
    fn toggle_on_content(&self) -> bool {
        let seated = |rect: Rect| rect != Rect::ZERO && self.content_column().contains_rect(rect);
        match self.model.mode.graph() {
            None => seated(self.calc.toggle_rect()),
            Some(kind) => self.panel(kind).toggle_rect().is_some_and(seated),
        }
    }

    /// Place the mode strip and the angle-unit strip in the header row of
    /// the content column. The unit strip anchors to the trailing edge, the
    /// mode strip centres in what is left and condenses to its short labels
    /// where the full ones would not fit. The panel's show/hide button takes
    /// its seat first when it sits in this header.
    fn layout_header(&mut self) {
        let content = self.content_column();
        let h = self.header;
        self.header_band = h.band(content);
        let band = self.header_band;
        let (mut left, mut right) = (band.x0 + h.inset, band.x1 - h.inset);
        let row = Rect::new(self.area.x0, band.y0, self.area.x1, band.y1);
        self.history_rect = if self.history.is_shown() {
            self.row_metrics.trailing_chip(row, 0)
        } else {
            Rect::ZERO
        };
        if let Some(button) = self.history.button_mut() {
            button.apply_layout_external(self.history_rect);
        }
        // The History control stands in the window's trailing corner, so a
        // content column that reaches it leaves the room.
        if self.history_rect != Rect::ZERO && band.x1 >= self.area.x1 - 0.5 {
            right -= self.row_metrics.chip + self.row_metrics.gap;
        }
        if self.toggle_on_content() {
            let seat = h.chip + h.gap;
            match self.model.mode {
                Mode::Calc => right -= seat,
                Mode::Graph2d | Mode::Graph3d => left += seat,
            }
        }
        let y0 = band.y0 + (band.height() - self.strip_height) / 2.0;
        let y1 = y0 + self.strip_height;

        // Full labels where they fit; the mode strip condenses first, then
        // the unit strip.
        let s = self.strips;
        let room = (right - left).max(0.0) - h.cluster_gap;
        (self.seg_short, self.unit_short) = if s.mode_full + s.unit_full <= room {
            (false, false)
        } else if s.mode_short + s.unit_full <= room {
            (true, false)
        } else {
            (true, true)
        };
        let unit_w = if self.unit_short {
            s.unit_short
        } else {
            s.unit_full
        };
        let unit_w = unit_w.min((right - left).max(0.0));
        let unit_x0 = right - unit_w;
        let half = unit_w / 2.0;
        self.unit_rects = [
            Rect::new(unit_x0, y0, unit_x0 + half, y1),
            Rect::new(unit_x0 + half, y0, right, y1),
        ];

        let region = Rect::new(left, y0, (unit_x0 - h.cluster_gap).max(left), y1);
        let wide = if self.seg_short {
            s.mode_short
        } else {
            s.mode_full
        };
        let w = wide.min(region.width());
        let centred = band.center().x - w / 2.0;
        let x0 = centred.clamp(region.x0, (region.x1 - w).max(region.x0));
        let seg_w = w / 3.0;
        for (i, rect) in self.seg_rects.iter_mut().enumerate() {
            let sx = x0 + i as f64 * seg_w;
            *rect = Rect::new(sx, y0, sx + seg_w, y1);
        }
    }

    /// Measure the strips at the size they are about to be painted. Returns
    /// `true` when a width changed (first frame, a text-scale change), so the
    /// caller re-runs `relayout` before painting this frame.
    fn measure_header(&mut self, tr: &mut TextRenderer, theme: &Theme) -> bool {
        self.header = Header::of(theme);
        self.row_metrics = HeaderRowMetrics::resolve(theme);
        let size = ControlSize::Regular;
        let modes = Mode::all();
        let full: Vec<&str> = modes.iter().map(|m| m.label()).collect();
        let short: Vec<&str> = modes.iter().map(|m| m.short_label()).collect();
        let measured = StripWidths {
            mode_full: SegmentedFace::intrinsic_width(&full, size, tr, theme),
            mode_short: SegmentedFace::intrinsic_width(&short, size, tr, theme),
            unit_full: SegmentedFace::intrinsic_width(&UNIT_LABELS, size, tr, theme),
            unit_short: SegmentedFace::intrinsic_width(&UNIT_SHORT, size, tr, theme),
        };
        let height = SegmentedFace::height(size, theme);
        let changed = measured != self.strips || (height - self.strip_height).abs() > 0.01;
        self.strips = measured;
        self.strip_height = height;
        changed
    }

    /// Submit one [`CalculatorAction`] through the resource's door — the one
    /// way this window changes the document, shared with the palette, agents
    /// and scripts. It applies now, inside the event, so a formula's next
    /// keystroke builds on this one; the rows it changed are presented back
    /// before the next frame. The caller updates the model for immediate
    /// feedback.
    ///
    /// Only ever call this for an action that genuinely mutates the document —
    /// the affordances gate on a real change (a non-empty history to clear, an
    /// existing row to toggle/delete).
    fn submit(&self, doc: &mut ui::widget::Edits<'_>, action: &CalculatorAction) {
        if let Some(dispatch) = &self.dispatch {
            // A refusal (a participant who may only view) writes nothing, and
            // the stored rows are presented again.
            let _ = dispatch.apply_now(doc, action);
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
        let action = actions::add_graph_action(kind, expr);
        if let CalculatorAction::AddGraph { id, .. } = &action {
            self.select = Some((kind, id.clone()));
        }
        let row = PlotFn {
            expr: expr.to_owned(),
            visible: true,
        };
        let rows = self.model.graphs_mut(kind);
        rows.push(row);
        let last = rows.len() - 1;
        *self.model.active_graph_mut(kind) = last;
        self.submit(doc, &action);
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
        if let Some(id) = self.rows.graph_id(kind, index) {
            let action = actions::toggle_graph_visible_action(kind, id);
            self.submit(doc, &action);
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
            let action = actions::set_graph_expr_action(kind, id, expr);
            self.submit(doc, &action);
        }
        self.model_dirty = true;
    }

    /// Empty the history.
    fn clear_history(&mut self, doc: &mut ui::widget::Edits<'_>) {
        // Only a non-empty history is a real mutation.
        if self.model.history.is_empty() {
            return;
        }
        self.model.history.clear();
        let ids = self.rows.history.iter().map(|(id, _)| id.clone()).collect();
        self.submit(doc, &actions::clear_history_ids_action(ids));
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
                &actions::commit_history_action(&entry.expr, &entry.result),
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

    /// Shared angle-unit transition for the top-bar controls. Pointer and
    /// semantic activation both use this rather than creating a second state
    /// path for assistive technology.
    fn set_degrees(&mut self, degrees: bool) {
        if self.model.deg != degrees {
            self.model.deg = degrees;
            self.model_dirty = true;
        }
    }

    /// Paint the header row over the content column: its hairline, the mode
    /// strip and the angle-unit strip.
    fn draw_header(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        ui::widget::toolbar::paint_band(scene, t, theme, self.header_band);
        let modes = Mode::all();
        let labels: Vec<&str> = modes
            .iter()
            .map(|m| {
                if self.seg_short {
                    m.short_label()
                } else {
                    m.label()
                }
            })
            .collect();
        let face = SegmentedFace::new(&self.seg_rects, &labels, self.model.mode.index())
            .hovered(self.mode_hover);
        ToggleGroup::paint(scene, t, tr, theme, &face);
        // The angle unit refines the mode, so it is the quiet strip.
        let units = if self.unit_short {
            &UNIT_SHORT
        } else {
            &UNIT_LABELS
        };
        let face = SegmentedFace::new(&self.unit_rects, units, usize::from(self.model.deg))
            .style(SegmentedStyle::Quiet)
            .hovered(self.unit_hover);
        ToggleGroup::paint(scene, t, tr, theme, &face);
    }

    // -- pointer routing -----------------------------------------------------

    /// Whether a floating drawer is up over the window, so the header and the
    /// content beneath it take no presses.
    fn drawer_blocks(&self) -> bool {
        match self.model.mode.graph() {
            None => self.calc.is_overlay() && self.calc.sidebar_open(),
            Some(kind) => self.panel(kind).floats_open(),
        }
    }

    /// Track the pointer over every control. Returns whether anything the
    /// window paints in response changed.
    fn track_pointer(&mut self, p: Point) -> bool {
        let blocked = self.drawer_blocks();
        let mode_hover = (!blocked)
            .then(|| self.seg_rects.iter().position(|r| r.contains(p)))
            .flatten();
        let unit_hover = (!blocked)
            .then(|| self.unit_rects.iter().position(|r| r.contains(p)))
            .flatten();
        let mut changed = mode_hover != self.mode_hover || unit_hover != self.unit_hover;
        self.mode_hover = mode_hover;
        self.unit_hover = unit_hover;
        changed |= match self.model.mode.graph() {
            None => self.calc.set_pointer(p),
            Some(kind) => self.panel_mut(kind).set_pointer(p),
        };
        changed
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
        // header first (a floating drawer covers it)
        if !self.drawer_blocks() {
            for (i, r) in self.seg_rects.iter().enumerate() {
                if r.contains(p) {
                    self.switch_mode(Mode::from_index(i), doc);
                    return;
                }
            }
            if let Some(i) = self.unit_rects.iter().position(|r| r.contains(p)) {
                self.set_degrees(i == 1);
                return;
            }
        }

        // Double-click detection (≤400 ms, within 5 px).
        let now = ui::time::now();
        let double = self
            .last_click
            .is_some_and(|t| now.duration_since(t).as_millis() <= 400)
            && (self.last_click_pos - p).hypot() <= 5.0;
        self.last_click = Some(now);
        self.last_click_pos = p;

        match self.model.mode.graph() {
            None => self.on_press_calc(p, doc),
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
        if panel.side().toggle_at(p) {
            self.panel_mut(kind).side_mut().toggle();
            return;
        }
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
        // History sidebar chrome takes priority over the keypad underneath.
        if self.calc.toggle_at(p) {
            self.calc.toggle_sidebar();
            return;
        }
        if self.calc.scrim_at(p) {
            // Tapping outside an open overlay drawer dismisses it.
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
            self.clear_history(doc);
        } else if let Some(hi) = self.calc.history_at(p)
            && let Some(e) = self.model.history.get(hi).cloned()
        {
            self.model.expr = e.expr;
            self.model.after_equals = false;
            self.model_dirty = true;
        }
    }

    /// Delete one graph row through the same stable, tombstoned action the
    /// palette offers. The model is deliberately not spliced optimistically:
    /// the presented rows are the exact merged list a collaborator sees, and
    /// keep active-row clamping in one place (`adopt_resource_state`).
    fn delete_graph_row(&mut self, kind: GraphKind, index: usize, doc: &mut ui::widget::Edits<'_>) {
        if let Some(id) = self.rows.graph_id(kind, index) {
            let action = actions::delete_graph_action(kind, id);
            self.submit(doc, &action);
            self.model_dirty = true;
        }
    }

    /// Extend a math-field drag-selection to the current pointer position.
    fn on_field_drag(&mut self, p: Point) {
        if let Some(kind) = self.model.mode.graph() {
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

        let factor = (1.0 + step * 0.12).clamp(0.4, 2.5);
        match self.model.mode {
            // Scroll the history list when the pointer is over it. This is
            // ephemeral view state (like hover), so it lives on the screen
            // cache rather than the CRDT.
            Mode::Calc if self.calc.history_viewport().contains(self.mouse) => {
                self.calc.history_scroll = (self.calc.history_scroll - step * 48.0).max(0.0);
            }
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
                None if self.calc.sidebar_open() => {
                    self.calc.close_sidebar();
                    true
                }
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
    /// Take `effect` on the next sync, where the document is: showing a graph
    /// for the first time adds its starter row.
    pub(crate) fn queue_effect(&mut self, effect: actions::CalculatorEffect) -> Result<(), String> {
        if let actions::CalculatorEffect::Open { entry } = &effect
            && !self.rows.history.iter().any(|(row, _)| row == entry)
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
                if let Some((_, past)) = self.rows.history.iter().find(|(row, _)| *row == entry) {
                    self.model.expr.clone_from(&past.expr);
                    self.model.after_equals = false;
                    self.model_dirty = true;
                    self.set_mode(Mode::Calc);
                }
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

impl Widget for CalculatorApp {
    ui::provides!(
        dyn ui::widget::WindowHeader,
        dyn ui::widget::controls::ActionControls
    );

    fn is_focusable(&self) -> bool {
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

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.host_focused
            .then_some(winit::window::ImePurpose::Normal)
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, _path: &automerge::ObjId) {
        if let Some(effect) = self.pending_effect.take() {
            self.perform_effect(effect, edits);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // A History button the header replaced may be dropped now: this pass
        // rebuilds the tree without it. The button is seated by hand in the
        // header row, so its taffy geometry is unused; the node exists so the
        // button is registered for a11y, focus and events.
        self.history.tree_rebuilt();
        let children: Vec<NodeId> = self
            .history
            .button_mut()
            .into_iter()
            .map(|button| button.build_layout(taffy, theme, node_map))
            .collect();
        // `new_with_children` only errs when the tree is at capacity or a
        // child belongs to another tree; degrade to an inert node rather than
        // aborting the whole session.
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &children,
            )
            .unwrap_or_else(|_| NodeId::from(0u64));
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.history
            .button()
            .into_iter()
            .map(|button| button as &dyn Widget)
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.history
            .button_mut()
            .into_iter()
            .map(|button| button as &mut dyn Widget)
            .collect()
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
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
        // Size the header strips from the labels as this frame paints them.
        self.measure_header(tr, theme);
        let area = self.area;
        match self.model.mode {
            Mode::Calc => self.calc.prepare(tr, theme, &self.model, area),
            Mode::Graph2d => self.g2.prepare(tr, theme, &self.model, area),
            Mode::Graph3d => self.g3.prepare(tr, theme, &self.model, area),
        }
        self.layout_header();

        // Content, then the header row over its column, then the panel (and
        // the scrim of a floating drawer) over both.
        let pointer = self.plot_pointer();
        match self.model.mode {
            Mode::Calc => self.calc.draw_content(scene, t, tr, theme, &self.model),
            Mode::Graph2d => self
                .g2
                .draw_content(scene, t, tr, theme, &self.model, pointer),
            Mode::Graph3d => self
                .g3
                .draw_content(scene, t, tr, theme, &self.model, pointer),
        }
        self.draw_header(scene, t, tr, theme);
        if let Some(button) = self.history.button_mut() {
            button.draw(scene, t, tr, theme, presence);
        }
        match self.model.mode {
            Mode::Calc => self.calc.draw_panel(scene, t, tr, theme, &self.model),
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

        // The window's History control is a retained button seated in the
        // header row; the layout tree may lag it by a frame when it was just
        // offered, so one with no node yet is addressed by the window's own.
        if !self.drawer_blocks() {
            let node = ctx
                .taffy
                .children(ctx.node_id)
                .ok()
                .and_then(|nodes| nodes.first().copied())
                .unwrap_or(ctx.node_id);
            if let Some(button) = self.history.button_mut() {
                button.handle_event(event, &mut ctx.reborrow(ctx.path, node));
                if *ctx.handled {
                    self.dirty = true;
                    return;
                }
            }
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                    .with_bounds(self.unit_rects.first().copied().unwrap_or(Rect::ZERO))
                    .with_state(AccessState::new().focusable(true).checked(!self.model.deg)),
                AccessNode::new(Role::ToggleButton)
                    .with_name("Degrees")
                    .with_action_target("calculator.angle:degrees")
                    .with_bounds(self.unit_rects.get(1).copied().unwrap_or(Rect::ZERO))
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
            "calculator.history.toggle"
                if matches!(action, Action::Activate | Action::ToggleCheck) =>
            {
                self.calc.toggle_sidebar();
            }
            "calculator.history.clear"
                if matches!(action, Action::Activate) && !self.model.history.is_empty() =>
            {
                self.clear_history(state);
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

/// The header row is the window's: it holds the History control the host
/// offers, so the window draws no History bar above the calculator.
impl ui::widget::WindowHeader for CalculatorApp {
    fn set_history_control(&mut self, control: Option<ui::widget::HistoryControl>) {
        if self.history.control() != control {
            self.history.set_control(control);
            self.structure_dirty |= self.history.take_rebuilt();
            self.dirty = true;
        }
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

    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        None
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
        let (toggle_bounds, add_bounds) = (panel.toggle_rect(), panel.add_rect());
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

    #[allow(
        clippy::too_many_lines,
        reason = "one parser keeps every retained graph control on the same action-dispatch path"
    )]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{CalculatorShell, history_entry_id};
    use ui::prelude::TestApp;
    use ui::tilo_resource::Invoke;

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

    /// A hosted calculator: the framework's live app with its resource.
    fn hosted() -> TestApp<CalculatorShell> {
        let mut app = TestApp::<CalculatorShell>::new().unwrap();
        app.harness().resize((1280, 800)).unwrap();
        app
    }

    /// The calculator's retained node in the inspection tree: its path, and
    /// the semantic tree it publishes.
    fn calculator(app: &mut TestApp<CalculatorShell>) -> (String, ui::a11y::AccessNode) {
        fn walk(node: &ui::headless::InspectionNode) -> Option<(String, ui::a11y::AccessNode)> {
            match &node.semantics {
                Some(semantics)
                    if semantics.description.as_deref()
                        == Some("Calculator and graphing workspace") =>
                {
                    Some((node.path.clone(), semantics.clone()))
                }
                _ => node.children.iter().find_map(walk),
            }
        }
        walk(&app.harness().inspection().root).expect("the calculator is laid out")
    }

    /// The semantic node the calculator publishes for `target`.
    fn node(app: &mut TestApp<CalculatorShell>, target: &str) -> ui::a11y::AccessNode {
        fn find(node: &ui::a11y::AccessNode, target: &str) -> Option<ui::a11y::AccessNode> {
            if node.action_target.as_deref() == Some(target) {
                return Some(node.clone());
            }
            node.children.iter().find_map(|child| find(child, target))
        }
        find(&calculator(app).1, target).unwrap_or_else(|| panic!("no semantic node {target}"))
    }

    /// Click the painted control `target`, as a pointer does.
    fn click(app: &mut TestApp<CalculatorShell>, target: &str) {
        let bounds = node(app, target)
            .bounds
            .unwrap_or_else(|| panic!("{target} is laid out"));
        app.harness().click_at(
            f64::midpoint(bounds.x0, bounds.x1),
            f64::midpoint(bounds.y0, bounds.y1),
        );
        app.settle();
    }

    /// A capture stages its view over the scenario's rows: the mode, an
    /// open drawer, a collaborator's pointer over the first formula.
    #[test]
    fn captures_stage_mode_drawers_and_collaborators() {
        let mut calc = TestApp::<CalculatorShell>::capture(Some("history-open")).unwrap();
        assert!(node(&mut calc, "calculator.mode:0").state.selected);
        assert_eq!(
            node(&mut calc, "calculator.history.toggle").name.as_deref(),
            Some("Hide history"),
            "the history drawer is staged open"
        );

        let mut narrow = TestApp::<CalculatorShell>::capture(Some("narrow-functions")).unwrap();
        assert_eq!(narrow.harness().inspection().viewport, [480, 800]);
        assert_eq!(
            node(&mut narrow, "calculator.graph.panel:2d").state.checked,
            Some(true),
            "the narrow functions drawer is staged open"
        );

        let mut remote = TestApp::<CalculatorShell>::capture(Some("remote-cursor")).unwrap();
        let field = node(&mut remote, "calculator.graph.field:2d:0")
            .bounds
            .expect("the first formula is laid out");
        let (x, y) = crate::resource::REMOTE_POINTER;
        let (x, y) = (f64::from(x), f64::from(y));
        assert!(
            field.x0 <= x && x <= field.x1 && field.y0 <= y && y <= field.y1,
            "the collaborator's pointer is over the first formula: {field:?}"
        );
    }

    #[test]
    fn keyboard_and_ime_only_mutate_the_host_focused_calculator() {
        let mut app = CalculatorApp::new();
        let mut state = ui::State::new();

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
        assert!(caret.height() > 0.0 && app.area.contains(caret.center()));
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
            app.set_focus(true);
            app.on_key_with_paste(
                &WKey::Character("v".into()),
                primary.into(),
                &mut Edits::lend(&mut state.doc),
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

    /// A history row's published `open` recalls exactly that row by its
    /// stable identity — surviving index shifts, refusing a deleted row —
    /// and is presentation only, open to a participant who may only view.
    #[test]
    fn history_recall_by_published_action_uses_stable_identity() {
        // A window someone uses, where effects run, over the scenario's book.
        let mut app = TestApp::<CalculatorShell>::new().unwrap();
        for invoke in <CalculatorShell as ui::framework::ResourceApp>::scenario(Some("graph3d")) {
            app.invoke(invoke).unwrap();
        }
        app.settle();
        let (id, expression) = app.state().history[1].clone();
        let expression = expression.expr;
        // A new calculation moves every positional index.
        app.invoke(Invoke::apply(&actions::commit_history_action("999+1", "1000")).unwrap())
            .unwrap();
        let heads = app.read_document(|doc| doc.clone().get_heads());
        app.call_on(&history_entry_id(&id), "open", serde_json::json!({}))
            .unwrap();
        app.settle();
        assert_eq!(
            app.read_document(|doc| doc.clone().get_heads()),
            heads,
            "recall is presentation only"
        );
        assert_eq!(
            calculator(&mut app).1.value.as_deref(),
            Some(expression.as_str()),
            "the recalled calculation is the expression"
        );
        assert!(
            node(&mut app, "calculator.mode:0").state.selected,
            "recall shows the calculator"
        );

        app.invoke_confirmed(Invoke::apply(&actions::delete_history_entry_action(&id)).unwrap())
            .unwrap();
        assert!(
            app.call_on(&history_entry_id(&id), "open", serde_json::json!({}))
                .is_err(),
            "a deleted row is no longer offered"
        );
    }

    /// Launch and mode-switch state: the live app boots empty in Calc, and
    /// entering an empty graph mode adds an editable starter row through
    /// the resource's action.
    #[test]
    fn launch_state_and_mode_switch_seeding() {
        let app = CalculatorApp::new();
        assert_eq!(app.model.mode, Mode::Calc, "live launch: Calc mode");
        assert!(app.model.expr.is_empty(), "live launch: empty expression");
        assert!(app.model.history.is_empty(), "live launch: no history");
        assert!(app.model.funcs.is_empty(), "live launch: nothing plotted");
        assert_eq!(app.model.memory, 0.0, "live launch: empty memory");
        assert_eq!(app.model.ans, 0.0, "live launch: ans is 0");

        let mut app = hosted();
        assert_eq!(app.state(), CalculatorState::default());
        click(&mut app, "calculator.mode:1");
        let state = app.state();
        assert_eq!(state.funcs.len(), 1, "Graph 2D adds one empty row");
        assert!(state.funcs[0].1.expr.is_empty());
        click(&mut app, "calculator.mode:2");
        let state = app.state();
        assert_eq!(state.surfaces.len(), 1, "Graph 3D adds one starter surface");
        assert_eq!(state.surfaces[0].1.expr, model::DEFAULT_SURFACE);
        // A second visit adds nothing.
        click(&mut app, "calculator.mode:1");
        assert_eq!(app.state().funcs.len(), 1);
        assert!(app.undo(), "the starter is an undo step");
        assert_eq!(app.state().surfaces.len(), 0);
    }

    /// The semantic tree exposes the header, keypad, expression and graph
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

        // phase accessibility_exposes_and_activates_header_controls
        {
            let mut app = CalculatorApp::new();
            let tree = app.accessibility().expect("calculator semantics");
            assert_eq!(
                tree.name.as_deref(),
                Some("Calculator"),
                "header: root name"
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
                "header: Graph 2D mode control exposed"
            );

            let mut doc = AutoCommit::new();
            assert!(
                app.perform_semantic_action(
                    "calculator.mode:1",
                    &Action::Activate,
                    &mut Edits::lend(&mut doc),
                    &automerge::ROOT,
                ),
                "header: mode activate handled"
            );
            assert_eq!(app.model.mode, Mode::Graph2d, "header: mode switched");
            assert!(
                app.perform_semantic_action(
                    "calculator.angle:degrees",
                    &Action::ToggleCheck,
                    &mut Edits::lend(&mut doc),
                    &automerge::ROOT,
                ),
                "header: angle toggle handled"
            );
            assert!(app.model.deg, "header: degrees on");
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
                    &mut Edits::lend(&mut doc),
                    &automerge::ROOT,
                ),
                "keypad: key 7 handled"
            );
            assert_eq!(app.model.expr, "7", "keypad: key 7 appended");
            assert!(
                app.perform_semantic_action(
                    expression.action_target.as_deref().unwrap_or_default(),
                    &Action::SetValue("sqrt(9)".into()),
                    &mut Edits::lend(&mut doc),
                    &automerge::ROOT,
                ),
                "keypad: expression SetValue handled"
            );
            assert_eq!(app.model.expr, "sqrt(9)", "keypad: expression set");

            app.set_mode(Mode::Graph2d);
            app.model.funcs = vec![PlotFn {
                expr: String::new(),
                visible: true,
            }];
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
                    &mut Edits::lend(&mut doc),
                    &automerge::ROOT,
                ),
                "graph: formula SetValue handled"
            );
            assert_eq!(app.model.funcs[0].expr, "2*x", "graph: formula set");
        }
    }

    impl CalculatorApp {
        /// Screen-space center of the Calc keypad key labelled `label`, after
        /// the current layout. `None` if the key is absent or not laid out.
        fn key_center(&self, label: &str) -> Option<Point> {
            let i = self.calc.key_index_for_label(label)?;
            self.calc.key_center(i)
        }
    }

    /// A seeded app sized `w` by 720 with its header strips measured, as the
    /// first draw leaves it.
    fn laid_out(w: f64, theme: &Theme) -> CalculatorApp {
        let mut app = CalculatorApp::seeded();
        app.area = Rect::new(0.0, 0.0, w, 720.0);
        app.measure_header(&mut TextRenderer::new(), theme);
        app.relayout();
        app
    }

    /// The window's History control stands in the trailing corner of the header
    /// row, and nothing else on the row reaches under it.
    #[test]
    fn history_control_seats_in_the_trailing_corner_clear_of_the_row() {
        use ui::widget::{HistoryControl, WindowHeader};
        for w in [1280.0, 520.0] {
            let theme = Theme::dark();
            let mut app = laid_out(w, &theme);
            assert_eq!(app.history_rect, Rect::ZERO, "no control, no seat");
            app.set_history_control(Some(HistoryControl::Closed));
            app.relayout();
            let seat = app.history_rect;
            assert!(
                seat.width() > 0.0 && (seat.x1 - w).abs() < 16.0,
                "the control stands in the corner at {w}: {seat:?}"
            );
            assert!(
                app.unit_rects[1].x1 <= seat.x0 + 0.5,
                "the unit strip clears the control at {w}: {:?} vs {seat:?}",
                app.unit_rects[1]
            );
            let toggle = app.calc.toggle_rect();
            assert!(
                toggle == Rect::ZERO || toggle.x1 <= seat.x0 + 0.5,
                "the panel button clears the control at {w}: {toggle:?} vs {seat:?}"
            );
        }
    }

    /// Header geometry: the mode strip and the angle-unit strip never clip or
    /// collide down to 400 px, reflow between
    /// breakpoints on resize, and widen to seat their measured labels at a
    /// large text scale (1× unchanged).
    #[test]
    fn header_layout_reflows_and_seats_labels_without_overlap() {
        // phase header_never_overlaps_or_clips_down_to_400
        // The strips must reflow so they neither clip past the window edge nor
        // collide, at every width down to 400 px.
        for w in [400.0, 460.0, 520.0, 560.0, 620.0, 680.0, 800.0, 1280.0] {
            let app = laid_out(w, &Theme::dark());
            for (i, r) in app.seg_rects.iter().enumerate() {
                assert!(
                    r.x0 >= app.area.x0 - 0.5 && r.x1 <= app.area.x1 + 0.5,
                    "mode segment {i} clipped the window edge at w={w}: {r:?}"
                );
            }
            assert!(
                app.seg_rects[2].x1 <= app.unit_rects[0].x0 + 0.5,
                "mode strip overlaps the angle-unit strip at w={w} \
                 (seg end {} vs unit start {})",
                app.seg_rects[2].x1,
                app.unit_rects[0].x0
            );
            assert!(
                app.unit_rects[1].x1 <= app.area.x1 + 0.5,
                "angle-unit strip clipped the window edge at w={w}"
            );
        }

        // phase header_strips_widen_to_seat_their_labels_at_large_text
        {
            // At an accessibility text scale the strips widen to seat the
            // labels the renderer actually paints — measured on the production
            // draw path — instead of keeping their 1× widths and letting the
            // labels run into each other.
            let one = laid_out(1280.0, &Theme::dark());
            let theme = Theme::dark().with_text_scale(2.0);
            let mut app = laid_out(1280.0, &theme);
            let mut tr = TextRenderer::new();
            app.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::empty(),
            );
            assert!(
                app.strips.mode_full > one.strips.mode_full
                    && app.strips.unit_full > one.strips.unit_full,
                "strips widen with the text: {:?} vs {:?}",
                app.strips,
                one.strips
            );
            assert!(
                app.seg_rects[2].x1 <= app.unit_rects[0].x0 + 0.5,
                "the widened mode strip still clears the unit strip: {} vs {}",
                app.seg_rects[2].x1,
                app.unit_rects[0].x0
            );
            // 1× keeps its geometry: drawing measures the same widths.
            let mut base = laid_out(1280.0, &Theme::dark());
            let before = (base.seg_rects, base.unit_rects);
            base.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            assert_eq!(
                (base.seg_rects, base.unit_rects),
                before,
                "1× header geometry is unchanged by drawing"
            );
        }

        // phase header_reflows_on_resize_between_breakpoints
        {
            // Re-derive the layout from the CURRENT width every relayout: shrinking a
            // laid-out instance must switch to condensed mode labels, and
            // re-widening must restore them (not freeze at the first frame).
            let mut app = laid_out(1280.0, &Theme::dark());
            assert!(!app.seg_short, "wide: full mode labels");
            app.area = Rect::new(0.0, 0.0, 430.0, 720.0);
            app.relayout();
            assert!(app.seg_short, "narrow: condensed mode labels");
            app.area = Rect::new(0.0, 0.0, 1280.0, 720.0);
            app.relayout();
            assert!(!app.seg_short, "re-widening must restore the full labels");
        }
    }

    /// The canvas hint teaches navigation only while the pointer is over a
    /// plot the person has not navigated yet; the trace follows the pointer
    /// over Graph 2D only.
    #[test]
    fn navigation_hint_shows_over_an_unnavigated_plot_only() {
        let mut app = wide(CalculatorApp::seeded()); // Graph 2D
        let over_plot = app.g2.canvas.center();
        app.mouse = over_plot;
        let pointer = app.plot_pointer();
        assert!(pointer.hint.is_some(), "hint over a fresh plot");
        assert_eq!(
            pointer.trace,
            Some(over_plot),
            "Graph 2D traces the pointer"
        );

        app.mouse = Point::new(10.0, app.g2.canvas.y0 + 10.0);
        let pointer = app.plot_pointer();
        assert!(
            pointer.hint.is_none() && pointer.trace.is_none(),
            "no hint over the panel"
        );

        app.mouse = over_plot;
        app.on_drag(Vec2::new(5.0, 0.0));
        assert!(
            app.plot_pointer().hint.is_none(),
            "no hint once the person has panned"
        );

        let mut app = wide(CalculatorApp::seeded());
        app.set_mode(Mode::Graph3d);
        app.relayout();
        app.mouse = app.g3.canvas.center();
        let pointer = app.plot_pointer();
        assert!(pointer.hint.is_some(), "hint over a fresh surface plot");
        assert!(pointer.trace.is_none(), "only Graph 2D traces");

        app.set_mode(Mode::Calc);
        app.relayout();
        assert!(app.plot_pointer().hint.is_none(), "no plot in Calculator");
    }

    /// A scenario stages the view a capture shows: the mode, the expression
    /// being typed, and the panel as an open floating drawer when it asks for
    /// one. A plain capture shows the lived-in 2D graph.
    #[test]
    fn scenarios_stage_the_view_they_name() {
        let stage = |name: Option<&str>, w: f64| {
            let mut app = CalculatorApp::seeded();
            app.area = Rect::new(0.0, 0.0, w, 800.0);
            app.stage_scenario(name);
            app
        };
        assert_eq!(stage(None, 1280.0).model.mode, Mode::Graph2d);
        assert_eq!(stage(Some("graph2d"), 1280.0).model.mode, Mode::Graph2d);
        assert_eq!(stage(Some("graph3d"), 1280.0).model.mode, Mode::Graph3d);
        let calc = stage(Some("calculator"), 1280.0);
        assert_eq!(calc.model.mode, Mode::Calc);
        assert_eq!(calc.model.expr, "sin(0.5)+cos(0.5)");
        let error = stage(Some("error"), 1280.0);
        assert!(engine::evaluate(&error.model.expr, &error.model.ctx()).is_err());
        let drawer = stage(Some("narrow-functions"), 480.0);
        assert!(drawer.g2.panel.side().is_overlay() && drawer.g2.panel.side().is_open());
        let drawer = stage(Some("narrow-history"), 480.0);
        assert!(drawer.calc.is_overlay() && drawer.calc.sidebar_open());
    }

    /// The real key path (`on_key`): Ctrl+digit switches mode, typing and
    /// Backspace edit the active 2D function / 3D surface, and Enter appends and
    /// focuses a fresh row. Fresh app + state per phase.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn keyboard_path_switches_mode_and_edits_graph_rows() {
        let none = winit::event::Modifiers::default();

        // phase ctrl_digit_switches_mode
        {
            let mut app = CalculatorApp::new();
            let mut st = ui::State::new();
            let ctrl = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
            app.on_key(
                &WKey::Character("3".into()),
                ctrl,
                &mut Edits::lend(&mut st.doc),
            );
            assert_eq!(app.model.mode, Mode::Graph3d, "ctrl+3 switches to Graph 3D");
            assert_eq!(app.model.surfaces.len(), 1, "with its starter surface");
        }

        // phase escape_clears_then_leaves_an_empty_calculator_to_the_host
        {
            let mut app = CalculatorApp::new();
            let mut st = ui::State::new();
            let escape = WKey::Named(NamedKey::Escape);
            app.calc.snap_sidebar(true);
            assert!(!app.escape_is_idle(&escape), "Escape dismisses the drawer");
            app.on_key(&escape, none, &mut Edits::lend(&mut st.doc));
            assert!(!app.calc.sidebar_open());
            app.on_key(
                &WKey::Character("7".into()),
                none,
                &mut Edits::lend(&mut st.doc),
            );
            assert!(!app.escape_is_idle(&escape), "Escape clears the expression");
            app.on_key(&escape, none, &mut Edits::lend(&mut st.doc));
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
                app.on_key(
                    &WKey::Character(ch.into()),
                    none,
                    &mut Edits::lend(&mut st.doc),
                );
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
            // on_key_graph resyncs the active field to the surface before editing.
            app.on_key(
                &WKey::Named(NamedKey::Backspace),
                none,
                &mut Edits::lend(&mut st.doc),
            );
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
                app.on_key(
                    &WKey::Character(ch.into()),
                    none,
                    &mut Edits::lend(&mut st.doc),
                );
            }
            assert_eq!(app.model.funcs[0].expr, "sin(x)", "enter 2D: typed row");
            app.on_key(
                &WKey::Named(NamedKey::Enter),
                none,
                &mut Edits::lend(&mut st.doc),
            );
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
            app.switch_mode(Mode::Graph3d, &mut Edits::lend(&mut st.doc)); // adds one starter surface
            let start = app.model.surfaces.len();
            assert_eq!(start, 1);
            app.on_key(
                &WKey::Named(NamedKey::Enter),
                none,
                &mut Edits::lend(&mut st.doc),
            );
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

    /// Typing a formula writes the document keystroke by keystroke, and the
    /// keystrokes of one row are one undo step; a collaborator's edit to the
    /// formula is presented.
    #[test]
    fn formula_keystrokes_are_one_undo_step_and_peer_edits_are_presented() {
        let mut app = hosted();
        app.invoke(Invoke::apply(&actions::add_graph_action(GraphKind::Curve2d, "")).unwrap())
            .unwrap();
        click(&mut app, "calculator.mode:1");
        click(&mut app, "calculator.graph.field:2d:0");
        // Keyboard input goes to the focused calculator: Tab to it.
        for _ in 0..32 {
            if calculator(&mut app).1.state.focused {
                break;
            }
            app.harness().tab(false);
        }
        assert!(
            calculator(&mut app).1.state.focused,
            "Tab reaches the calculator"
        );
        app.harness().type_text("2*x");
        app.settle();
        assert_eq!(
            app.state().funcs[0].1.expr,
            "2*x",
            "every keystroke is written"
        );

        // Enter starts the next row: the typed formula is settled.
        app.harness().key("Enter");
        app.settle();
        assert_eq!(app.state().funcs.len(), 2, "Enter adds the next row");
        assert!(app.undo(), "undo the new row");
        assert_eq!(app.state().funcs.len(), 1);
        assert!(app.undo(), "undo the typing");
        assert_eq!(
            app.state().funcs[0].1.expr,
            "",
            "one undo takes back the whole typed formula"
        );
        assert!(app.redo());
        assert_eq!(app.state().funcs[0].1.expr, "2*x");

        let id = app.state().funcs[0].0.clone();
        app.merge_peer_edit(|doc| {
            use tilo_actions::Action as _;
            actions::set_graph_expr_action(GraphKind::Curve2d, id.as_str(), "x*x+1")
                .apply(doc)
                .unwrap();
        });
        app.settle();
        assert_eq!(
            node(&mut app, "calculator.graph.field:2d:0")
                .value
                .as_deref(),
            Some("x*x+1"),
            "the collaborator's formula is shown"
        );
    }

    // -- pointer-path harness: drive the REAL production handle_event route -----
    //
    // These exercise the live pointer pipeline end-to-end — hit-testing against
    // the same rects the last layout painted, then the wired handler — rather
    // than calling the model-level `apply_key` / view mutators directly. Node /
    // taffy are unused by the (single-leaf) calculator, so a throwaway leaf is
    // fine.

    fn send(app: &mut CalculatorApp, st: &mut ui::State, ev: &WindowEvent) -> bool {
        ui::testing::Dispatch::new()
            .doc(&mut st.doc)
            .path(&st.root)
            .send(app, ev)
            .handled
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

    /// An app at the wide 1280×800 area used by the pointer tests, after one
    /// real frame: the controls are measured at the size they paint at.
    fn wide(mut app: CalculatorApp) -> CalculatorApp {
        app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        app.relayout();
        app
    }

    /// Calculator-mode pointer path: keycaps append, a history row recalls,
    /// Clear empties history, memory keys accumulate/clear, and the angle-unit
    /// toggle changes trig output. Fresh app + state per phase.
    #[test]
    fn calc_pointer_clicks_drive_keypad_history_memory_and_angle() {
        // phase calc_keypad_click_appends_to_expression
        {
            // handle_event → on_press → on_press_calc → key_at → apply_key on the
            // real keycap rects (not a model-level apply_key shortcut).
            let mut app = wide(CalculatorApp::new()); // Calc mode, empty
            let mut st = ui::State::new();
            let seven = app.key_center("7").expect("7 keycap laid out");
            tap(&mut app, &mut st, seven);
            assert_eq!(app.model.expr, "7", "tapping a digit keycap must append it");
            let plus = app.key_center("+").expect("+ keycap laid out");
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
            // on_press → unit_rects → model.deg, and the change must flow
            // through model.ctx() so trig actually evaluates differently.
            use crate::engine;
            let mut app = CalculatorApp::new();
            let mut st = ui::State::new();
            app.area = Rect::new(0.0, 0.0, 1280.0, 800.0);
            app.measure_header(&mut TextRenderer::new(), &Theme::dark());
            app.relayout();
            assert!(!app.model.deg, "default is radians");
            let rad_val = engine::evaluate("sin(90)", &app.model.ctx()).unwrap();
            assert!(
                (rad_val - 1.0).abs() > 0.01,
                "RAD sin(90) must not be 1 (got {rad_val})"
            );
            let deg_pt = app.unit_rects[1].center();
            tap(&mut app, &mut st, deg_pt);
            assert!(app.model.deg, "clicking DEG must switch to degrees");
            let deg_val = engine::evaluate("sin(90)", &app.model.ctx()).unwrap();
            assert!(
                (deg_val - 1.0).abs() < 1e-9,
                "DEG sin(90) must be 1 (got {deg_val})"
            );
            let rad_pt = app.unit_rects[0].center();
            tap(&mut app, &mut st, rad_pt);
            assert!(!app.model.deg, "clicking RAD must switch back to radians");
        }
    }

    /// Graph-mode pointer path: drag pans / orbits and the wheel zooms the 2D and
    /// 3D views, the narrow Functions drawer opens from its toggle, and the row
    /// visibility toggle and delete button act on the tapped row. Fresh app + state
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
            // Drive the REAL production event path (handle_event → on_press) at a
            // narrow width and assert the collapsed Functions drawer opens.
            let mut app = CalculatorApp::new();
            app.set_mode(Mode::Graph2d);
            app.area = Rect::new(0.0, 0.0, 560.0, 720.0);
            app.relayout();
            app.layout_active();
            assert!(
                !app.g2.panel.side().is_open(),
                "narrow default must start collapsed"
            );
            let toggle = app
                .g2
                .panel
                .toggle_center()
                .expect("a collapsed toggle must be present");

            let mut st = ui::State::new();
            let dev = winit::event::DeviceId::dummy();
            let moved = ui::testing::Dispatch::new()
                .doc(&mut st.doc)
                .path(&st.root)
                .send(
                    &mut app,
                    &WindowEvent::CursorMoved {
                        device_id: dev,
                        position: winit::dpi::PhysicalPosition::new(toggle.x, toggle.y),
                    },
                );
            ui::testing::Dispatch::new()
                .doc(&mut st.doc)
                .path(&st.root)
                .handled(moved.handled)
                .focus_target(moved.focus_target)
                .send(
                    &mut app,
                    &WindowEvent::MouseInput {
                        device_id: dev,
                        state: ElementState::Pressed,
                        button: MouseButton::Left,
                    },
                );
            assert!(
                app.g2.panel.side().is_open(),
                "tapping the toggle must open the Functions drawer"
            );
        }

        // phase vis_dot_click_toggles_function_visibility
        {
            // on_press_graph → vis_at → funcs[i].visible flips, via the real toggle rect.
            let mut app = CalculatorApp::seeded(); // Graph2d, 4 visible funcs
            app.set_mode(Mode::Graph2d);
            let mut st = ui::State::new();
            let mut app = wide(app); // docked panel
            assert!(app.model.funcs[0].visible, "vis toggle: starts visible");
            let dot = app
                .g2
                .panel
                .vis_center(0)
                .expect("row 0 visibility toggle laid out");
            tap(&mut app, &mut st, dot);
            assert!(
                !app.model.funcs[0].visible,
                "tapping the toggle must hide the function"
            );
            tap(&mut app, &mut st, dot);
            assert!(app.model.funcs[0].visible, "tapping again must show it");
        }

        // phase graph_row_delete_button_tombstones_the_stable_resource
        {
            // The visible delete button uses the same keyed action as the
            // palette, so it cannot rebind after a peer changes list order.
            let mut app = TestApp::<CalculatorShell>::capture(None).unwrap();
            let before = app.state().funcs;
            let delete = node(&mut app, "calculator.graph.delete:2d:0")
                .bounds
                .expect("first formula has a visible delete control");
            app.harness().click_at(
                f64::midpoint(delete.x0, delete.x1),
                f64::midpoint(delete.y0, delete.y1),
            );
            app.settle();
            let after = app.state().funcs;
            assert_eq!(after.len(), before.len() - 1, "one row removed");
            assert!(
                after.iter().all(|(id, _)| *id != before[0].0),
                "the clicked stable row is tombstoned rather than a neighbouring index"
            );
        }
    }
}
