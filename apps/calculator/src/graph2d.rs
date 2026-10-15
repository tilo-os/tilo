//! 2D grapher: a `y = f(x)` function list plus a pannable, zoomable canvas
//! that samples each function per pixel-column into discontinuity-aware
//! polylines, painted over the toolkit's [`FunctionPlane`] grid.

use ui::charts::{ChartPalette, FunctionPlane};
use ui::interaction::InteractionEnvironment;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{CanvasNavigation, CanvasNavigationHint};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Fill};

use crate::engine;
use crate::model::{CalcModel, View2d};
use crate::plot_panel::{CURVES, Formulas, PlotPanel};
use crate::zoom::ZoomAnim2d;

pub struct Graph2dScreen {
    /// The plot, below the header row.
    pub canvas: Rect,
    /// The window area the screen was last laid out in.
    area: Rect,
    /// The formula list beside (or floating over) the plot.
    pub panel: PlotPanel,
    /// Sampled curve geometry, rebuilt only when the functions, view, domain,
    /// or canvas size change — never on a theme or tab switch (colour is
    /// resolved from the theme at paint time from the cached series index).
    curve_cache: CurveCache,
    /// Smooth, non-blocking wheel zoom. While active, the cached curves are
    /// replayed under a cheap affine instead of re-sampling every frame.
    zoom: ZoomAnim2d,
}

impl Default for Graph2dScreen {
    fn default() -> Self {
        Self {
            canvas: Rect::ZERO,
            area: Rect::ZERO,
            panel: PlotPanel::new(CURVES),
            curve_cache: CurveCache::default(),
            zoom: ZoomAnim2d::new(View2d::default()),
        }
    }
}

/// A single sampled curve: the polyline (already in screen space) plus the
/// series index so the colour can be re-resolved from the live theme without
/// touching the geometry.
struct CurvePath {
    series: usize,
    path: BezPath,
}

/// Identity of the inputs that affect 2D plot geometry. When this is
/// unchanged between frames the cached polylines are replayed as-is; a theme
/// or tab switch does NOT change it, so neither triggers a re-sample.
#[derive(Clone, PartialEq, Default)]
struct CurveKey {
    /// `(expr, visible)` for every function, in order.
    funcs: Vec<(String, bool)>,
    deg: bool,
    cx: u64,
    cy: u64,
    scale: u64,
    canvas: (u64, u64, u64, u64),
}

#[derive(Default)]
struct CurveCache {
    key: Option<CurveKey>,
    paths: Vec<CurvePath>,
    /// Counts actual (re)samples; lets a test prove a theme/tab switch reuses
    /// the cache instead of re-evaluating every function.
    rebuilds: u32,
}

fn bits(x: f64) -> u64 {
    x.to_bits()
}

impl CurveKey {
    fn build(model: &CalcModel, canvas: Rect) -> Self {
        CurveKey {
            funcs: model
                .funcs
                .iter()
                .map(|f| (f.expr.clone(), f.visible))
                .collect(),
            deg: model.deg,
            cx: bits(model.view2d.cx),
            cy: bits(model.view2d.cy),
            scale: bits(model.view2d.scale),
            canvas: (
                bits(canvas.x0),
                bits(canvas.y0),
                bits(canvas.x1),
                bits(canvas.y1),
            ),
        }
    }
}

impl Graph2dScreen {
    /// Resolve the panel, the content column and the plot for the window
    /// `area` (the header row included).
    pub fn layout(&mut self, area: Rect, model: &CalcModel) {
        self.area = area;
        self.panel.layout(area, model.funcs.len());
        let content = self.panel.side().content();
        self.canvas = Rect::new(
            content.x0,
            (content.y0 + self.panel.side().header_height()).min(content.y1),
            content.x1,
            content.y1,
        );
    }

    /// Measure the formulas and lay the screen out for this frame, then
    /// advance the eased zoom. Call once per draw, before painting.
    pub fn prepare(&mut self, tr: &mut TextRenderer, theme: &Theme, model: &CalcModel, area: Rect) {
        self.panel.fit(&model.funcs, tr, theme);
        self.layout(area, model);
        self.panel.side_mut().tick(theme);
        // Advance the smooth-zoom ease one frame toward the model target.
        self.zoom.tick(model.view2d, theme.reduce_motion);
        self.layout(area, model);
    }

    /// The plot: grid, curves and the trace under the pointer.
    pub fn draw_content(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
        pointer: Pointer,
    ) {
        // The eased `view` (== model when settled) drives the grid + trace so
        // they move in lock-step with the cheaply-transformed curves.
        let view = self.zoom.display();
        self.draw_canvas(scene, t, tr, theme, model, (pointer.trace, &view));
        if let Some(environment) = pointer.hint {
            CanvasNavigationHint::paint(
                scene,
                t,
                tr,
                theme,
                self.canvas,
                CanvasNavigation::PanZoom,
                environment,
            );
        }
    }

    /// The formula panel, over the plot.
    pub fn draw_panel(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
        focused: bool,
    ) {
        let formulas = Formulas {
            rows: &model.funcs,
            active: model.active_fn,
            focused,
        };
        self.panel.draw(scene, t, tr, theme, formulas);
    }

    /// The plot canvas: grid, curves and the trace crosshair (if any).
    fn draw_canvas(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
        trace_view: (Option<Point>, &View2d),
    ) {
        let (trace, view) = trace_view;
        self.draw_grid(scene, t, tr, theme, view);
        self.draw_curves(scene, t, theme, model);
        if let Some(p) = trace {
            self.draw_trace(scene, t, tr, theme, model, (p, view));
        }
    }

    // ----- canvas internals -------------------------------------------------

    fn plane(&self, v: &View2d) -> FunctionPlane {
        FunctionPlane::new(self.canvas, Point::new(v.cx, v.cy), v.scale)
    }
    fn to_screen(&self, wx: f64, wy: f64, v: &View2d) -> Point {
        self.plane(v).to_screen(Point::new(wx, wy))
    }
    pub fn to_world(&self, p: Point, v: &View2d) -> Point {
        self.plane(v).to_world(p)
    }

    fn draw_grid(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        v: &View2d,
    ) {
        // The grid derives against the window ground the plot stands on.
        self.plane(v)
            .paint_grid(scene, t, tr, theme, theme.tokens.colors.surfaces.content);
    }

    fn draw_curves(&mut self, scene: &mut Scene, t: Affine, theme: &Theme, model: &CalcModel) {
        // While the zoom is animating we reuse the last-sampled polylines under
        // a cheap affine (a resample would block the frame). When settled we do
        // the one real resample and record the view it represents.
        let aff = if self.zoom.active() {
            self.zoom.curve_affine(self.canvas.center())
        } else {
            self.ensure_curves(model);
            self.zoom.mark_sampled(model.view2d);
            Affine::IDENTITY
        };
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &self.canvas);
        let stroke = Stroke::new(2.0 * f64::from(theme.ui_scale));
        let palette = ChartPalette::from_theme(theme);
        for cp in &self.curve_cache.paths {
            let col = palette.categorical(cp.series);
            scene.stroke(&stroke, t * aff, col, None, &cp.path);
        }
        scene.pop_layer();
    }

    /// Begin a smooth zoom toward the (already-updated) model view, pinning the
    /// world point `anchor_world` under the cursor `screen` for the whole ease.
    pub fn begin_zoom(&mut self, anchor_world: Point, screen: Point) {
        self.zoom.begin(anchor_world, screen, self.canvas.center());
    }
    /// Settle the zoom immediately (pan / edit / reset / mode switch).
    pub fn settle_zoom(&mut self, v: View2d) {
        self.zoom.settle(v);
    }
    /// Whether a smooth zoom is animating (drives the app's frame pump).
    pub fn zoom_active(&self) -> bool {
        self.zoom.active()
    }

    /// Re-sample every visible function into screen-space polylines, but only
    /// when the cache key (functions / view / domain / canvas) has changed.
    /// This is the heavy per-pixel-column sampling; gating it here is what
    /// keeps a theme switch, tab switch, or hover from re-evaluating the
    /// expressions. Colour is deliberately NOT part of the cached geometry.
    fn ensure_curves(&mut self, model: &CalcModel) {
        let key = CurveKey::build(model, self.canvas);
        if self.curve_cache.key.as_ref() == Some(&key) {
            return;
        }
        let v = &model.view2d;
        let world_h = self.canvas.height() / v.scale;
        let break_thresh = world_h * 3.0;
        let mut paths = Vec::new();
        for (i, f) in model.funcs.iter().enumerate() {
            if !f.visible || f.expr.trim().is_empty() {
                continue;
            }
            let Ok(comp) = engine::compile(&f.expr, model.deg, false) else {
                continue;
            };
            let mut path = BezPath::new();
            let mut pen_down = false;
            let mut prev_wy = f64::NAN;
            // Sample every 1.5px column. Iterating an integer step count
            // (rather than a `while sx <= x1` float loop) keeps the last
            // column reachable regardless of float rounding.
            let steps = ((self.canvas.x1 - self.canvas.x0) / 1.5).floor().max(0.0) as usize;
            for step in 0..=steps {
                let sx = self.canvas.x0 + step as f64 * 1.5;
                let wx = self.to_world(Point::new(sx, 0.0), v).x;
                let wy = comp.eval1(wx);
                if !wy.is_finite() {
                    pen_down = false;
                    prev_wy = f64::NAN;
                    continue;
                }
                // asymptote break: large jump with a sign flip
                if pen_down
                    && prev_wy.is_finite()
                    && (wy - prev_wy).abs() > break_thresh
                    && wy.signum() != prev_wy.signum()
                {
                    pen_down = false;
                }
                let p = self.to_screen(wx, wy, v);
                if pen_down {
                    path.line_to(p);
                } else {
                    path.move_to(p);
                    pen_down = true;
                }
                prev_wy = wy;
            }
            paths.push(CurvePath { series: i, path });
        }
        self.curve_cache.key = Some(key);
        self.curve_cache.paths = paths;
        self.curve_cache.rebuilds = self.curve_cache.rebuilds.wrapping_add(1);
    }

    /// The trace under the pointer: a guide, a dot on the active curve and a
    /// readout of the point it marks, in a floating chip over the plot.
    fn draw_trace(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
        trace_at: (Point, &View2d),
    ) {
        let (p, v) = trace_at;
        if !self.canvas.contains(p) {
            return;
        }
        let c = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.spacing.s1) + 1.0;
        let Some((world, intersection)) = self.trace_point(model, p, v, 3.0 * radius) else {
            return;
        };
        let (wx, wy) = (world.x, world.y);
        let sx = self.to_screen(wx, 0.0, v).x;
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        // vertical guide
        ctx.fill_rect(
            Rect::new(sx - 0.5, self.canvas.y0, sx + 0.5, self.canvas.y1),
            c.text_tertiary,
        );
        if !wy.is_finite() {
            return;
        }
        let dot = self.to_screen(wx, wy, v);
        if dot.y < self.canvas.y0 || dot.y > self.canvas.y1 {
            return;
        }
        let col = ChartPalette::from_theme(theme).categorical(model.active_fn);
        let marker = Rect::from_center_size(dot, (2.0 * radius, 2.0 * radius));
        ctx.fill_round(marker, radius, col);
        ctx.stroke_round(
            marker,
            radius,
            c.surfaces.content,
            f64::from(theme.ui_scale),
        );
        if intersection {
            let ring = marker.inflate(radius * 0.5, radius * 0.5);
            ctx.stroke_round(ring, radius * 1.5, col, f64::from(theme.ui_scale));
        }
        // Bounded precision keeps the readout compact while moving.
        let label = format!("({wx:.4}, {wy:.4})");
        let size = ctx.measure_role(&label, TextRole::Label, None);
        let pad = f64::from(theme.tokens.spacing.s2);
        let chip = Rect::from_origin_size(
            Point::new(sx + pad, self.canvas.y0 + pad),
            (size.width + 2.0 * pad, size.height + pad),
        );
        let chip = clamp_rect_x(chip, self.canvas, pad);
        ui::surface::panel(
            ctx.scene,
            t,
            chip,
            f64::from(theme.tokens.radii.control),
            theme,
        );
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        ctx.text_role_in_band(
            &label,
            TextRole::Label,
            ink.tokens.colors.text_primary,
            chip.x0 + pad,
            chip,
            None,
        );
    }

    /// Snap to a nearby crossing of the active curve and another visible
    /// curve. A bounded bracket and a residual check reject asymptotes.
    fn trace_point(
        &self,
        model: &CalcModel,
        pointer: Point,
        view: &View2d,
        radius: f64,
    ) -> Option<(Point, bool)> {
        let active = model.funcs.get(model.active_fn)?;
        if !active.visible {
            return None;
        }
        let curve = engine::compile(&active.expr, model.deg, false).ok()?;
        let x = self.to_world(pointer, view).x;
        let mut point = Point::new(x, curve.eval1(x));
        let mut snapped = false;
        let mut nearest = radius;
        for (index, row) in model.funcs.iter().enumerate() {
            if index == model.active_fn || !row.visible {
                continue;
            }
            let Ok(other) = engine::compile(&row.expr, model.deg, false) else {
                continue;
            };
            if let Some(crossing) = crossing_near(&curve, &other, x, radius / view.scale) {
                let distance = (self.to_screen(crossing.x, crossing.y, view) - pointer).hypot();
                if distance < nearest {
                    nearest = distance;
                    point = crossing;
                    snapped = true;
                }
            }
        }
        point.y.is_finite().then_some((point, snapped))
    }

    // ----- hit-testing ------------------------------------------------------

    pub fn canvas_contains(&self, p: Point) -> bool {
        // While a floating drawer (and its scrim) is up, the canvas is not
        // directly interactive — a press there dismisses the drawer instead of
        // panning/tracing.
        !self.panel.floats_open() && self.canvas.contains(p)
    }
}

/// What the pointer is doing over the plot, for painting.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pointer {
    /// The point to trace, while the pointer is over the plot.
    pub trace: Option<Point>,
    /// Show the navigation hint, for this kind of pointer.
    pub hint: Option<InteractionEnvironment>,
}

fn crossing_near(a: &engine::Compiled, b: &engine::Compiled, x: f64, radius: f64) -> Option<Point> {
    let difference = |x| a.eval1(x) - b.eval1(x);
    let (mut lo, mut hi) = (x - radius, x + radius);
    let mut left = difference(lo);
    let right = difference(hi);
    if !left.is_finite() || !right.is_finite() || left.signum() == right.signum() {
        return None;
    }
    for _ in 0..40 {
        let mid = f64::midpoint(lo, hi);
        let value = difference(mid);
        if !value.is_finite() {
            return None;
        }
        if value.signum() == left.signum() {
            lo = mid;
            left = value;
        } else {
            hi = mid;
        }
    }
    let x = f64::midpoint(lo, hi);
    let y = a.eval1(x);
    (difference(x).abs() < 1e-7 && y.is_finite()).then_some(Point::new(x, y))
}

fn clamp_rect_x(r: Rect, bounds: Rect, margin: f64) -> Rect {
    let w = r.width();
    let mut x0 = r.x0;
    if x0 + w > bounds.x1 - margin {
        x0 = bounds.x1 - margin - w;
    }
    if x0 < bounds.x0 + margin {
        x0 = bounds.x0 + margin;
    }
    Rect::new(x0, r.y0, x0 + w, r.y1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CalcModel, Mode};

    #[test]
    fn trace_snaps_to_visible_crossings_but_not_poles_or_hidden_curves() {
        let (screen, _) = screen_with_canvas();
        let mut model = CalcModel {
            funcs: vec![
                crate::model::PlotFn {
                    expr: "x".into(),
                    visible: true,
                },
                crate::model::PlotFn {
                    expr: "-x".into(),
                    visible: true,
                },
            ],
            ..CalcModel::default()
        };
        let pointer = screen.to_screen(0.05, 0.03, &model.view2d);
        let (point, snapped) = screen
            .trace_point(&model, pointer, &model.view2d, 12.0)
            .unwrap();
        assert!(snapped);
        assert!(point.x.abs() < 1e-8 && point.y.abs() < 1e-8);
        model.funcs[1].visible = false;
        assert!(
            !screen
                .trace_point(&model, pointer, &model.view2d, 12.0)
                .unwrap()
                .1
        );
        model.funcs[0].visible = false;
        assert!(
            screen
                .trace_point(&model, pointer, &model.view2d, 12.0)
                .is_none()
        );
        let pole = engine::compile("1/x", false, false).unwrap();
        let zero = engine::compile("0", false, false).unwrap();
        assert!(crossing_near(&pole, &zero, 0.0, 0.1).is_none());
    }

    fn screen_with_canvas() -> (Graph2dScreen, Rect) {
        let mut s = Graph2dScreen::default();
        let canvas = Rect::new(360.0, 60.0, 1280.0, 800.0);
        s.canvas = canvas;
        (s, canvas)
    }

    /// The Functions panel is re-derived from the current width: it docks when
    /// wide, becomes a floating drawer (button, scrim, no pan beneath it)
    /// when narrow, re-docks when widened, and keeps its row buttons apart.
    /// Fresh screen per phase.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn functions_panel_docks_wide_floats_narrow_and_keeps_row_targets() {
        // phase wide_docks_the_functions_panel
        {
            let mut s = Graph2dScreen::default();
            let m = CalcModel::seeded();
            let area = Rect::new(0.0, 0.0, 1280.0, 800.0);
            s.layout(area, &m);
            assert!(!s.panel.side().is_overlay(), "wide window docks the panel");
            assert!(s.panel.side().is_open());
            let column = s.panel.side().column();
            assert!(
                column.width() > 0.0 && (column.x0 - area.x0).abs() < f64::EPSILON,
                "docked panel on the left"
            );
            assert!(
                s.canvas.x0 >= column.x1 - 0.5,
                "canvas starts after the docked panel"
            );
            assert!(
                s.canvas.y0 >= area.y0 + s.panel.side().header_height() - 0.5,
                "the plot stands below the header row"
            );
        }

        // phase narrow_floats_the_panel_as_a_drawer
        {
            let mut s = Graph2dScreen::default();
            let m = CalcModel::seeded();
            let area = Rect::new(0.0, 0.0, 560.0, 800.0);
            s.layout(area, &m);
            assert!(
                s.panel.side().is_overlay(),
                "narrow window floats the panel"
            );
            assert!(
                !s.panel.side().is_open(),
                "narrow default is collapsed so the plot keeps the width"
            );
            assert_eq!(
                s.panel.side().column(),
                Rect::ZERO,
                "no panel eating the plot"
            );
            assert!(
                (s.canvas.width() - area.width()).abs() < 0.5,
                "the plot gets the full width when collapsed"
            );
            let toggle = s
                .panel
                .toggle_rect()
                .expect("a button must be present to open it");
            assert!(
                toggle.x0 < area.x0 + s.panel.side().header_height(),
                "the button sits at the leading edge of the content header"
            );
            assert!(s.panel.side().toggle_at(toggle.center()));
        }

        // phase narrow_drawer_opens_with_scrim_and_dismisses
        {
            let mut s = Graph2dScreen::default();
            let m = CalcModel::seeded();
            let area = Rect::new(0.0, 0.0, 560.0, 800.0);
            s.layout(area, &m);
            s.panel.side_mut().toggle();
            s.panel.side_mut().snap(true);
            s.layout(area, &m);
            assert!(s.panel.side().is_open() && s.panel.side().is_overlay());
            let column = s.panel.side().column();
            assert!(column.width() > 0.0, "an open drawer is drawn");
            assert!(
                s.panel
                    .toggle_rect()
                    .is_some_and(|t| column.contains_rect(t)),
                "the drawer carries its own show/hide button"
            );
            // A press inside the drawer must not pan the plot.
            assert!(!s.canvas_contains(column.center()));
            assert!(s.panel.side().covers(column.center()));
            // A press on the scrim, beside the drawer, dismisses it.
            let beside = Point::new(area.x1 - 10.0, area.center().y);
            assert!(s.panel.side().scrim_at(beside));
            s.panel.side_mut().close();
            s.panel.side_mut().snap(false);
            s.layout(area, &m);
            assert!(!s.panel.side().is_open());
        }

        // phase panel_reflows_back_to_docked_when_widened
        {
            // Re-derive from the current width each pass: a fresh screen must
            // dock when wide and float when narrow, with no first-frame cache.
            let mut s = Graph2dScreen::default();
            let m = CalcModel::seeded();
            s.layout(Rect::new(0.0, 0.0, 560.0, 800.0), &m);
            assert!(s.panel.side().is_overlay() && !s.panel.side().is_open());
            s.layout(Rect::new(0.0, 0.0, 1280.0, 800.0), &m);
            assert!(
                !s.panel.side().is_overlay() && s.panel.side().is_open(),
                "widening must re-dock the panel"
            );
        }

        // phase row_buttons_have_pointer_targets_that_stay_apart
        {
            let mut s = Graph2dScreen::default();
            let m = CalcModel::seeded();
            // 390 wide is the tightest production presentation.
            let area = Rect::new(0.0, 0.0, 390.0, 844.0);
            s.layout(area, &m);
            s.panel.side_mut().toggle();
            s.panel.side_mut().snap(true);
            s.layout(area, &m);
            let target = f64::from(ui::tokens::Metrics::BASE.min_pointer_target);
            let delete = s.panel.delete_rect(0).expect("delete target");
            let visible = s.panel.visibility_rect(0).expect("visibility target");
            assert!(delete.width() >= target - 0.5 && delete.height() >= target - 0.5);
            assert!(visible.width() >= target - 0.5 && visible.height() >= target - 0.5);
            assert!(
                visible.x1 <= delete.x0 + 0.5,
                "row buttons must not overlap"
            );
            assert_eq!(s.panel.delete_at(delete.center()), Some(0));
            assert_eq!(s.panel.vis_at(visible.center()), Some(0));
            assert_eq!(
                s.panel.delete_at(
                    s.panel
                        .delete_rect(1)
                        .expect("second delete target")
                        .center()
                ),
                Some(1),
                "a delete hit must not target the adjacent row"
            );
            let edit = s.panel.field_rect(0).expect("editor");
            assert!(
                edit.x1 <= visible.x0 + 0.5,
                "math editing must not steal a button's target"
            );
        }
    }

    /// The sampled-curve cache: the first draw samples every visible function;
    /// theme or tab switches do not resample; editing a function, panning or
    /// zooming resample; hiding a function drops its curve. Fresh screen +
    /// model per phase.
    #[test]
    fn curve_cache_resamples_only_when_geometry_changes() {
        // phase first_draw_samples_visible_functions
        {
            let (mut s, _) = screen_with_canvas();
            let m = CalcModel::seeded();
            s.ensure_curves(&m);
            // seeded model has 4 visible functions → 4 sampled polylines.
            let visible = m.funcs.iter().filter(|f| f.visible).count();
            assert_eq!(s.curve_cache.paths.len(), visible);
            assert_eq!(s.curve_cache.rebuilds, 1);
        }

        // phase theme_or_tab_switch_does_not_resample
        {
            let (mut s, _) = screen_with_canvas();
            let mut m = CalcModel::seeded();
            s.ensure_curves(&m);
            assert_eq!(s.curve_cache.rebuilds, 1);
            // A theme switch is invisible to geometry: nothing about the model that
            // ensure_curves keys on changes, so re-calling it must NOT resample.
            s.ensure_curves(&m);
            s.ensure_curves(&m);
            assert_eq!(
                s.curve_cache.rebuilds, 1,
                "re-sampled with no geometry change"
            );
            // Switching tabs and back leaves funcs/view/canvas identical.
            m.mode = Mode::Calc;
            s.ensure_curves(&m);
            m.mode = Mode::Graph2d;
            s.ensure_curves(&m);
            assert_eq!(s.curve_cache.rebuilds, 1, "tab switch triggered a resample");
        }

        // phase editing_a_function_resamples
        {
            let (mut s, _) = screen_with_canvas();
            let mut m = CalcModel::seeded();
            s.ensure_curves(&m);
            assert_eq!(s.curve_cache.rebuilds, 1);
            m.funcs[0].expr = "x^3".into();
            s.ensure_curves(&m);
            assert_eq!(s.curve_cache.rebuilds, 2);
        }

        // phase panning_the_view_resamples
        {
            let (mut s, _) = screen_with_canvas();
            let mut m = CalcModel::seeded();
            s.ensure_curves(&m);
            m.view2d.cx += 1.5;
            s.ensure_curves(&m);
            assert_eq!(s.curve_cache.rebuilds, 2);
            m.view2d.scale *= 1.2;
            s.ensure_curves(&m);
            assert_eq!(s.curve_cache.rebuilds, 3);
        }

        // phase toggling_visibility_off_drops_the_curve
        {
            let (mut s, _) = screen_with_canvas();
            let mut m = CalcModel::seeded();
            s.ensure_curves(&m);
            let before = s.curve_cache.paths.len();
            m.funcs[0].visible = false;
            s.ensure_curves(&m);
            assert_eq!(s.curve_cache.paths.len(), before - 1);
        }
    }

    /// Every curve of the lived-in plot gets its own colour: no two of the
    /// first four series repeat.
    #[test]
    fn curves_take_distinct_colours() {
        let palette = ChartPalette::from_theme(&Theme::dark());
        for a in 0..4 {
            for b in (a + 1)..4 {
                assert_ne!(
                    palette.categorical(a),
                    palette.categorical(b),
                    "series {a} and {b} share a colour"
                );
            }
        }
    }
}
