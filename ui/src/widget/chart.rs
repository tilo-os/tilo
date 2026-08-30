//! Time-series charts — [`Chart`] (a full line/area plot with grid lines,
//! axis ticks and a legend) and [`Sparkline`] (a compact, chromeless
//! variant for inline trend cells).
//!
//! Both render one or more [`Series`] of `f32` samples. A series is mapped
//! to pixels with the standard time-series projection: the samples are
//! evenly spaced across the plot width and scaled against a shared `max`
//! so multiple series share a vertical axis. The top edge of each series
//! is a stroked polyline; with [`Chart::with_area`] an optional translucent
//! area fill is dropped from the line down to the baseline.
//!
//! Colours come entirely from the [`Theme`]: grid lines from the standard
//! border colour, axis tick labels from the dim text colour, and each
//! series carries its own [`Series::color`] (callers typically pull these
//! from the theme accent / semantic ramps). Nothing is hard-coded.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Alpha (0–255) of the translucent area fill under a series line. Matches
/// the concept's `bg-<color>/18` area treatment.
const AREA_FILL_ALPHA: u8 = 46;
/// Alpha of the sparkline area fill — a touch heavier since sparklines are
/// small and benefit from a more legible fill.
const SPARK_FILL_ALPHA: u8 = 54;
/// Alpha of the grid lines against the border colour.
const GRID_ALPHA: u8 = 90;
/// Number of horizontal grid divisions (so `DIVISIONS + 1` rules).
const GRID_DIVISIONS: usize = 4;
/// Stroke width of a series line.
const LINE_WIDTH: f64 = 1.8;
/// Stroke width of a sparkline.
const SPARK_LINE_WIDTH: f64 = 1.5;
/// Height reserved at the top of the plot for the title / legend strip.
const HEADER_H: f64 = 22.0;

/// Replace a colour's alpha channel with `a` (0–255), keeping RGB. The
/// toolkit's stand-in for a vertical gradient fade — a flat translucent
/// tint reads cleanly against the dark surfaces and avoids a gradient
/// resource per frame.
fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

/// One labelled data series. `data` is sampled left-to-right across the
/// plot; `color` is the line/area tint; `legend` is the label shown in the
/// chart's legend strip (empty string = omit from the legend).
#[derive(Clone)]
pub struct Series {
    pub data: Vec<f32>,
    pub color: Color,
    pub legend: String,
}

impl Series {
    pub fn new(data: Vec<f32>, color: Color) -> Self {
        Self {
            data,
            color,
            legend: String::new(),
        }
    }

    pub fn with_legend(mut self, legend: impl Into<String>) -> Self {
        self.legend = legend.into();
        self
    }
}

/// Map sample `i` of a series (value `v`) into a point inside `plot`,
/// scaling against `ceil` (the shared series max, floored away from zero).
/// `n` is the series length. Shared by [`Chart`] and [`Sparkline`].
fn map_point(plot: Rect, i: usize, v: f32, n: usize, ceil: f32) -> Point {
    let dx = if n > 1 {
        plot.width() / (n as f64 - 1.0)
    } else {
        0.0
    };
    let x = plot.x0 + dx * i as f64;
    let frac = f64::from((v / ceil).clamp(0.0, 1.0));
    // y-down: the baseline is `plot.y1`, larger values rise toward `y0`.
    let y = plot.y1 - frac * plot.height();
    Point::new(x, y)
}

/// Build the closed area path (line down to the baseline) for `data`.
fn area_path(plot: Rect, data: &[f32], ceil: f32) -> BezPath {
    let n = data.len();
    let mut path = BezPath::new();
    path.move_to(Point::new(plot.x0, plot.y1));
    for (i, &v) in data.iter().enumerate() {
        path.line_to(map_point(plot, i, v, n, ceil));
    }
    path.line_to(Point::new(plot.x1, plot.y1));
    path.close_path();
    path
}

/// Build the open top-line path for `data`.
fn line_path(plot: Rect, data: &[f32], ceil: f32) -> BezPath {
    let n = data.len();
    let mut path = BezPath::new();
    for (i, &v) in data.iter().enumerate() {
        let p = map_point(plot, i, v, n, ceil);
        if i == 0 {
            path.move_to(p);
        } else {
            path.line_to(p);
        }
    }
    path
}

/// Full time-series line/area chart with grid, axis ticks and a legend.
pub struct Chart {
    series: Vec<Series>,
    /// Explicit vertical-axis maximum. `None` ⇒ derived from the data.
    max: Option<f32>,
    area: bool,
    title: String,
    /// `(hi, lo)` axis tick labels drawn at the top-right / bottom-right.
    axis_hi: String,
    axis_lo: String,
    height: f32,
    rect: Rect,
}

impl Chart {
    /// Construct a chart from a set of series. Defaults: no area fill, a
    /// data-derived max, an intrinsic height of 160 px.
    pub fn new(series: Vec<Series>) -> Self {
        Self {
            series,
            max: None,
            area: false,
            title: String::new(),
            axis_hi: String::new(),
            axis_lo: String::new(),
            height: 160.0,
            rect: Rect::ZERO,
        }
    }

    /// Builder: drop a translucent area fill under each series line.
    pub fn with_area(mut self, on: bool) -> Self {
        self.area = on;
        self
    }

    /// Builder: pin the vertical-axis maximum (so multiple charts share a
    /// scale). Without this the max is derived from the largest sample.
    pub fn with_max(mut self, max: f32) -> Self {
        self.max = Some(max);
        self
    }

    /// Builder: a title drawn at the top-left of the header strip.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Builder: the high / low axis tick labels (drawn right-aligned at the
    /// top and bottom of the plot, e.g. `"100%"` / `"0%"`).
    pub fn with_axis_labels(mut self, hi: impl Into<String>, lo: impl Into<String>) -> Self {
        self.axis_hi = hi.into();
        self.axis_lo = lo.into();
        self
    }

    /// Builder: intrinsic layout height in logical pixels.
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    pub fn set_series(&mut self, series: Vec<Series>) {
        self.series = series;
    }

    /// The resolved vertical-axis ceiling, floored away from zero so the
    /// projection never divides by ~0.
    fn ceiling(&self) -> f32 {
        let derived = self
            .series
            .iter()
            .flat_map(|s| s.data.iter().copied())
            .fold(0.0_f32, f32::max);
        self.max.unwrap_or(derived).max(1e-3)
    }

    /// Immediate-mode draw: render a full chart into `scene` at `rect`,
    /// reading colours from `theme`. Lets a host that lays out its own
    /// dashboard cards paint a chart at a known rect without a widget.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_at(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        rect: Rect,
        theme: &Theme,
        series: &[Series],
        max: f32,
        area: bool,
        title: &str,
        axis_hi: &str,
        axis_lo: &str,
    ) {
        if rect.width() <= 0.0 || rect.height() <= HEADER_H {
            return;
        }
        let tokens = &theme.tokens;
        let plot = Rect::new(rect.x0, rect.y0 + HEADER_H, rect.x1, rect.y1);
        let ceil = max.max(1e-3);

        // Title (top-left of header).
        if !title.is_empty() {
            text_renderer.draw_text(
                scene,
                title,
                tokens.typography.scale.sm,
                tokens.colors.text_primary,
                transform * Affine::translate((rect.x0, rect.y0 + 2.0)),
                None,
            );
        }

        // Legend (right-aligned, painted right-to-left).
        let legend_size = tokens.typography.scale.xs;
        let mut lx = rect.x1;
        for s in series.iter().rev() {
            if s.legend.is_empty() {
                continue;
            }
            let (tw, _) = text_renderer.measure(&s.legend, legend_size, None);
            lx -= f64::from(tw);
            text_renderer.draw_text(
                scene,
                &s.legend,
                legend_size,
                tokens.colors.text_dim,
                transform * Affine::translate((lx, rect.y0 + 4.0)),
                None,
            );
            lx -= 8.0;
            let dot = Circle::new(
                Point::new(lx, rect.y0 + 4.0 + f64::from(legend_size) / 2.0),
                3.0,
            );
            scene.fill(Fill::NonZero, transform, s.color, None, &dot);
            lx -= 12.0;
        }

        // Horizontal grid rules.
        let grid = with_alpha(tokens.colors.border_standard, GRID_ALPHA);
        for i in 0..=GRID_DIVISIONS {
            let y = plot.y0 + plot.height() * (i as f64 / GRID_DIVISIONS as f64);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                grid,
                None,
                &Line::new((plot.x0, y), (plot.x1, y)),
            );
        }

        // Axis ticks (right-aligned just inside the plot).
        if !axis_hi.is_empty() {
            let (tw, _) = text_renderer.measure(axis_hi, legend_size, None);
            text_renderer.draw_text(
                scene,
                axis_hi,
                legend_size,
                tokens.colors.text_dim,
                transform * Affine::translate((plot.x1 - f64::from(tw), plot.y0 - 1.0)),
                None,
            );
        }
        if !axis_lo.is_empty() {
            let (tw, _) = text_renderer.measure(axis_lo, legend_size, None);
            text_renderer.draw_text(
                scene,
                axis_lo,
                legend_size,
                tokens.colors.text_dim,
                transform * Affine::translate((plot.x1 - f64::from(tw), plot.y1 - 12.0)),
                None,
            );
        }

        // Series: area fill (optional) then top line.
        for s in series {
            if s.data.len() < 2 {
                continue;
            }
            if area {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    with_alpha(s.color, AREA_FILL_ALPHA),
                    None,
                    &area_path(plot, &s.data, ceil),
                );
            }
            scene.stroke(
                &Stroke::new(LINE_WIDTH),
                transform,
                s.color,
                None,
                &line_path(plot, &s.data, ceil),
            );
        }
    }
}

impl Widget for Chart {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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
        Chart::draw_at(
            scene,
            transform,
            text_renderer,
            self.rect,
            theme,
            &self.series,
            self.ceiling(),
            self.area,
            &self.title,
            &self.axis_hi,
            &self.axis_lo,
        );
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &'static str {
        "Chart"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

/// Compact, chromeless trend line — no axis, grid, title or legend. Just
/// the area fill + line, sized to fill its rect. Drop one into a table cell
/// or a stat card to show a recent trend.
pub struct Sparkline {
    data: Vec<f32>,
    color: Color,
    /// Explicit max; `None` ⇒ derived from the data.
    max: Option<f32>,
    width: f32,
    height: f32,
    rect: Rect,
}

impl Sparkline {
    /// Construct a sparkline of `data` in `color`. Defaults to a 64×20
    /// intrinsic size and a data-derived max.
    pub fn new(data: Vec<f32>, color: Color) -> Self {
        Self {
            data,
            color,
            max: None,
            width: 64.0,
            height: 20.0,
            rect: Rect::ZERO,
        }
    }

    /// Builder: pin the vertical max.
    pub fn with_max(mut self, max: f32) -> Self {
        self.max = Some(max);
        self
    }

    /// Builder: intrinsic size in logical pixels.
    pub fn with_size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    pub fn set_data(&mut self, data: Vec<f32>) {
        self.data = data;
    }

    fn ceiling(&self) -> f32 {
        let derived = self.data.iter().copied().fold(0.0_f32, f32::max);
        self.max.unwrap_or(derived).max(1e-3)
    }

    /// Immediate-mode draw of a sparkline into `scene` at `rect`.
    pub fn draw_at(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        data: &[f32],
        max: f32,
        color: Color,
    ) {
        if data.len() < 2 || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let ceil = max.max(1e-3);
        scene.fill(
            Fill::NonZero,
            transform,
            with_alpha(color, SPARK_FILL_ALPHA),
            None,
            &area_path(rect, data, ceil),
        );
        scene.stroke(
            &Stroke::new(SPARK_LINE_WIDTH),
            transform,
            color,
            None,
            &line_path(rect, data, ceil),
        );
    }
}

impl Widget for Sparkline {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(self.width),
                height: Dimension::length(self.height),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        Sparkline::draw_at(
            scene,
            transform,
            self.rect,
            &self.data,
            self.ceiling(),
            self.color,
        );
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &'static str {
        "Sparkline"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello_encoding::DrawTag;

    fn count_color_tags(scene: &Scene) -> usize {
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    /// The point projection scales against `ceil` and flips y: the max
    /// value lands at the plot top, zero at the baseline, the midpoint in
    /// the middle.
    #[test]
    fn map_point_projects_value_to_pixels() {
        let plot = Rect::new(0.0, 0.0, 100.0, 50.0);
        // n = 3 samples, ceil = 10.
        let top = map_point(plot, 0, 10.0, 3, 10.0);
        let bottom = map_point(plot, 2, 0.0, 3, 10.0);
        let mid = map_point(plot, 1, 5.0, 3, 10.0);
        assert!((top.y - plot.y0).abs() < 1e-6, "max sits at plot top");
        assert!(
            (bottom.y - plot.y1).abs() < 1e-6,
            "zero sits at the baseline"
        );
        assert!((mid.y - 25.0).abs() < 1e-6, "half value lands mid-plot");
        // x marches evenly: dx = 100/(3-1) = 50.
        assert!((top.x - 0.0).abs() < 1e-6);
        assert!((mid.x - 50.0).abs() < 1e-6);
        assert!((bottom.x - 100.0).abs() < 1e-6);
    }

    /// Values above `ceil` clamp to the plot top rather than escaping it.
    #[test]
    fn map_point_clamps_overshoot() {
        let plot = Rect::new(0.0, 0.0, 10.0, 10.0);
        let p = map_point(plot, 0, 999.0, 2, 1.0);
        assert!((p.y - plot.y0).abs() < 1e-6, "overshoot clamps to the top");
    }

    /// A chart with an area fill emits strictly more COLOR fills than the
    /// same chart without it (the area path is the extra fill).
    #[test]
    fn area_fill_adds_a_color_tag() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let data = vec![1.0_f32, 3.0, 2.0, 5.0, 4.0];
        let series = vec![Series::new(data, theme.accent_primary)];

        let mut plain = Scene::new();
        Chart::draw_at(
            &mut plain,
            Affine::IDENTITY,
            &mut tr,
            Rect::new(0.0, 0.0, 200.0, 120.0),
            &theme,
            &series,
            5.0,
            false,
            "",
            "",
            "",
        );
        let mut filled = Scene::new();
        Chart::draw_at(
            &mut filled,
            Affine::IDENTITY,
            &mut tr,
            Rect::new(0.0, 0.0, 200.0, 120.0),
            &theme,
            &series,
            5.0,
            true,
            "",
            "",
            "",
        );
        assert!(
            count_color_tags(&filled) > count_color_tags(&plain),
            "area fill must add at least one COLOR fill (plain={}, filled={})",
            count_color_tags(&plain),
            count_color_tags(&filled),
        );
    }

    /// The grid emits exactly `GRID_DIVISIONS + 1` stroke rules, so a chart
    /// with no series still paints its grid (proves grid is independent of
    /// the data path). We assert the chart paints *something* and a
    /// data-bearing chart paints strictly more.
    #[test]
    fn chart_paints_grid_then_series() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let empty: Vec<Series> = vec![];
        let mut grid_only = Scene::new();
        Chart::draw_at(
            &mut grid_only,
            Affine::IDENTITY,
            &mut tr,
            Rect::new(0.0, 0.0, 200.0, 120.0),
            &theme,
            &empty,
            1.0,
            false,
            "",
            "",
            "",
        );
        let grid_tags = count_color_tags(&grid_only);
        assert!(grid_tags > GRID_DIVISIONS, "grid rules must paint");

        let series = vec![Series::new(vec![1.0, 2.0, 3.0], theme.accent_primary)];
        let mut with_line = Scene::new();
        Chart::draw_at(
            &mut with_line,
            Affine::IDENTITY,
            &mut tr,
            Rect::new(0.0, 0.0, 200.0, 120.0),
            &theme,
            &series,
            3.0,
            false,
            "",
            "",
            "",
        );
        assert!(
            count_color_tags(&with_line) > grid_tags,
            "series line adds fills"
        );
    }

    /// A single-point series is skipped (no line drawn) — a sparkline or
    /// chart line needs at least two points to interpolate.
    #[test]
    fn sparkline_needs_two_points() {
        let theme = Theme::dark();
        let mut one = Scene::new();
        Sparkline::draw_at(
            &mut one,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 64.0, 20.0),
            &[5.0],
            5.0,
            theme.accent_primary,
        );
        assert_eq!(count_color_tags(&one), 0, "single point draws nothing");

        let mut many = Scene::new();
        Sparkline::draw_at(
            &mut many,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 64.0, 20.0),
            &[5.0, 3.0, 7.0],
            7.0,
            theme.accent_primary,
        );
        assert!(count_color_tags(&many) >= 2, "area + line both paint");
    }

    /// `ceiling` floors away from zero so an all-zero series never divides
    /// by ~0 in the projection.
    #[test]
    fn chart_ceiling_floors_away_from_zero() {
        let chart = Chart::new(vec![Series::new(vec![0.0, 0.0], Color::WHITE)]);
        assert!(chart.ceiling() >= 1e-3);
        let pinned = Chart::new(vec![]).with_max(42.0);
        assert_eq!(pinned.ceiling(), 42.0);
    }
}
