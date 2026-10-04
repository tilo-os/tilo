//! 3D grapher: a `z = f(x, y)` surface list (mirroring the 2D function list)
//! plus a canvas that samples every visible surface on a grid, projects each
//! vertex to 2D with yaw/pitch orbit + orthographic projection, merges the
//! quads of all surfaces and depth-sorts them together (one painter's-algorithm
//! pass so surfaces occlude each other correctly), and paints each cell in its
//! function's series colour with a Lambert shade. Drag orbits, scroll zooms.

use ui::ColorCompat;
use ui::charts::ChartPalette;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{CanvasNavigation, CanvasNavigationHint};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::Color;

use crate::engine;
use crate::graph2d::Pointer;
use crate::mathfield::MathField;
use crate::model::CalcModel;
use crate::plot_panel::{Formulas, Hover, PlotPanel, SURFACES};
use crate::zoom::ZoomAnim3d;

/// Half-width of the sampled domain in x and y.
const DOM: f64 = 7.0;
/// Grid resolution (cells per axis).
const N: usize = 40;
/// Cells per axis of the floor grid.
const FLOOR_CELLS: usize = 7;
/// How far under the lowest surface the floor sits, as a share of the
/// surfaces' height range.
const FLOOR_DROP: f64 = 0.6;
/// Opacity of the floor's grid lines and of its outline.
const FLOOR_GRID_ALPHA: u8 = 38;
const FLOOR_OUTLINE_ALPHA: u8 = 96;

pub struct Graph3dScreen {
    /// The plot, below the header row.
    pub canvas: Rect,
    /// The window area the screen was last laid out in.
    area: Rect,
    panel: PlotPanel,
    /// Projected + depth-sorted geometry for ALL visible surfaces, rebuilt only
    /// when the surfaces, orbit/zoom, domain, or canvas size change — never on a
    /// theme or tab switch. Each cell carries its function's series index; the
    /// live colour is resolved from the theme at paint time.
    surface_cache: SurfaceCache,
    /// Smooth, non-blocking wheel zoom. While active, the cached cells are
    /// replayed scaled about the canvas centre instead of re-projecting.
    zoom: ZoomAnim3d,
}

impl Default for Graph3dScreen {
    fn default() -> Self {
        Self {
            canvas: Rect::ZERO,
            area: Rect::ZERO,
            panel: PlotPanel::new(SURFACES),
            surface_cache: SurfaceCache::default(),
            zoom: ZoomAnim3d::new(crate::model::View3d::default().zoom),
        }
    }
}

/// One painted quad of a surface. `series` selects the function's theme colour;
/// `shade` is the Lambert factor. Both are theme-independent so a re-theme
/// replays the cached geometry as `scale_rgb(palette.categorical(series), shade)`.
struct SurfaceCell {
    path: BezPath,
    series: usize,
    shade: f64,
}

/// Identity of the inputs that affect 3D surface geometry. Excludes the theme
/// so a re-theme replays the cached cells without re-sampling.
#[derive(Clone, PartialEq, Default)]
struct SurfaceKey {
    /// `(expr, visible)` for every surface, in order.
    surfaces: Vec<(String, bool)>,
    deg: bool,
    yaw: u64,
    pitch: u64,
    zoom: u64,
    canvas: (u64, u64, u64, u64),
}

/// The floor under the surfaces: a square grid on the domain, drawn so the
/// plot has a scale and a ground to read heights against.
#[derive(Default)]
struct Floor {
    /// The grid's interior lines.
    grid: BezPath,
    /// The domain's outline.
    outline: BezPath,
}

#[derive(Default)]
struct SurfaceCache {
    key: Option<SurfaceKey>,
    cells: Vec<SurfaceCell>,
    floor: Floor,
    /// Counts actual (re)samples; lets a test prove a theme/tab switch reuses
    /// the cached surfaces instead of re-sampling + re-projecting + re-sorting.
    rebuilds: u32,
}

/// One surface's grid of sampled z-values (row-major, `zs[i][j]` is the
/// value at grid vertex `(i, j)`), gathered by `ensure_surfaces`'s first
/// pass before projection and depth-sorting.
struct SampledSurface {
    series: usize,
    zs: Vec<[f64; N + 1]>,
}

/// One projected quad awaiting the painter's-algorithm depth sort.
struct RankedCell {
    depth: f64,
    cell: SurfaceCell,
}

fn bits(x: f64) -> u64 {
    x.to_bits()
}

impl SurfaceKey {
    fn build(model: &CalcModel, canvas: Rect) -> Self {
        SurfaceKey {
            surfaces: model
                .surfaces
                .iter()
                .map(|f| (f.expr.clone(), f.visible))
                .collect(),
            deg: model.deg,
            yaw: bits(model.view3d.yaw),
            pitch: bits(model.view3d.pitch),
            zoom: bits(model.view3d.zoom),
            canvas: (
                bits(canvas.x0),
                bits(canvas.y0),
                bits(canvas.x1),
                bits(canvas.y1),
            ),
        }
    }
}

impl Graph3dScreen {
    /// Pure graph rendering for a frozen History model; no editing controls.
    pub(crate) fn draw_history_canvas(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        model: &CalcModel,
        area: Rect,
    ) {
        self.canvas = area;
        self.zoom.settle(model.view3d.zoom);
        self.draw_surfaces(scene, t, theme, model);
    }

    /// Adopt the theme's scale and metrics.
    pub fn scale_for(&mut self, theme: &Theme) {
        self.panel.scale_for(theme);
    }

    /// Resolve the panel, the content column and the plot for the window
    /// `area` (the header row included).
    pub fn layout(&mut self, area: Rect, model: &CalcModel) {
        self.area = area;
        self.panel.layout(area, model.surfaces.len());
        let content = self.panel.side().content();
        self.canvas = Rect::new(
            content.x0,
            (content.y0 + self.panel.side().header_height()).min(content.y1),
            content.x1,
            content.y1,
        );
    }

    /// The column beside the panel, header row included.
    pub fn content_column(&self) -> Rect {
        self.panel.side().content()
    }

    /// Reconcile the per-row editors with `model.surfaces`.
    pub fn sync_fields(&mut self, model: &CalcModel) {
        self.panel.sync_fields(&model.surfaces);
    }

    /// The math field for the active surface row, if any.
    pub fn active_field_mut(&mut self, active: usize) -> Option<&mut MathField> {
        self.panel.active_field_mut(active)
    }

    /// The row index whose editor rect contains `p`.
    pub fn field_at(&self, p: Point) -> Option<usize> {
        self.panel.field_at(p)
    }

    /// End any in-progress drag-selection in every row's field.
    pub fn release_fields(&mut self) {
        self.panel.release_fields();
    }

    /// Paint remote collaborators' carets into the active surface field.
    pub fn draw_remote_carets(
        &self,
        scene: &mut Scene,
        t: Affine,
        active: usize,
        cursors: &[(Point, Color)],
    ) {
        self.panel.draw_remote_carets(scene, t, active, cursors);
    }

    // -- panel chrome --------------------------------------------------------

    pub fn toggle_at(&self, p: Point) -> bool {
        self.panel.side().toggle_at(p)
    }
    pub fn scrim_at(&self, p: Point) -> bool {
        self.panel.side().scrim_at(p)
    }
    pub fn add_at(&self, p: Point) -> bool {
        self.panel.add_at(p)
    }
    pub fn vis_at(&self, p: Point) -> Option<usize> {
        self.panel.vis_at(p)
    }
    pub fn delete_at(&self, p: Point) -> Option<usize> {
        self.panel.delete_at(p)
    }
    pub fn row_at(&self, p: Point) -> Option<usize> {
        self.panel.row_at(p)
    }
    pub fn toggle_panel(&mut self) {
        self.panel.side_mut().toggle();
    }
    pub fn close_panel(&mut self) {
        self.panel.side_mut().close();
    }
    /// Snap the panel open or shut with no animation (captures and tests).
    pub fn snap_panel(&mut self, open: bool) {
        self.panel.side_mut().snap(open);
    }
    pub fn overlay_captures(&self, p: Point) -> bool {
        self.panel.side().covers(p)
    }
    /// Whether the Surfaces panel is currently shown.
    pub fn panel_open(&self) -> bool {
        self.panel.side().is_open()
    }
    /// Whether the panel floats over the plot.
    pub fn is_overlay(&self) -> bool {
        self.panel.side().is_overlay()
    }
    /// Whether the panel's slide is still running.
    pub fn panel_animating(&self) -> bool {
        self.panel.side().is_animating()
    }
    /// The panel's show/hide button, for the accessibility tree.
    pub fn toggle_rect(&self) -> Option<Rect> {
        Some(self.panel.side().toggle_rect()).filter(|r| *r != Rect::ZERO)
    }
    pub fn add_rect(&self) -> Option<Rect> {
        self.panel.add_rect()
    }
    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        self.panel.row_rect(i)
    }
    pub fn field_rect(&self, i: usize) -> Option<Rect> {
        self.panel.field_rect(i)
    }
    pub fn visibility_rect(&self, i: usize) -> Option<Rect> {
        self.panel.visibility_rect(i)
    }
    pub fn delete_rect(&self, i: usize) -> Option<Rect> {
        self.panel.delete_rect(i)
    }
    /// Center of the panel toggle, if one is placed this frame (test helper).
    #[doc(hidden)]
    pub fn toggle_center(&self) -> Option<Point> {
        self.toggle_rect().map(|r| r.center())
    }
    /// Center of surface row `i`'s visibility toggle, once laid out (test
    /// helper).
    #[doc(hidden)]
    pub fn vis_center(&self, i: usize) -> Option<Point> {
        self.visibility_rect(i).map(|r| r.center())
    }

    /// Track the pointer over the panel; whether anything it paints changed.
    pub fn set_pointer(&mut self, p: Point) -> bool {
        self.panel.set_pointer(p)
    }

    pub fn hover(&self) -> Hover {
        self.panel.hover()
    }

    /// Measure the formulas and lay the screen out for this frame, then
    /// advance the eased zoom. Call once per draw, before painting.
    pub fn prepare(&mut self, tr: &mut TextRenderer, theme: &Theme, model: &CalcModel, area: Rect) {
        self.panel.fit(&model.surfaces, tr, theme);
        self.layout(area, model);
        self.panel.side_mut().tick(theme);
        self.zoom.tick(model.view3d.zoom, theme.reduce_motion);
        self.layout(area, model);
    }

    /// The plot: every visible surface.
    pub fn draw_content(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
        pointer: Pointer,
    ) {
        self.draw_surfaces(scene, t, theme, model);
        if let Some(environment) = pointer.hint {
            CanvasNavigationHint::paint(
                scene,
                t,
                tr,
                theme,
                self.canvas,
                CanvasNavigation::OrbitZoom,
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
            rows: &model.surfaces,
            active: model.active_surface,
            focused,
        };
        self.panel.draw(scene, t, tr, theme, formulas);
    }

    fn draw_surfaces(&mut self, scene: &mut Scene, t: Affine, theme: &Theme, model: &CalcModel) {
        // While the zoom is animating we reuse the cached cells, scaled about
        // the canvas centre (the projection scale is proportional to zoom), so
        // no re-projection blocks the frame. When settled we re-project once.
        let tt = if self.zoom.active() {
            t * Affine::scale_about(self.zoom.scale_factor(), self.canvas.center())
        } else {
            self.ensure_surfaces(model);
            self.zoom.mark_sampled(model.view3d.zoom);
            t
        };

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &self.canvas,
        );
        // The ground shows through between cells, which reads as the mesh.
        let colors = &theme.tokens.colors;
        let wire = colors.surfaces.content;
        let palette = ChartPalette::from_theme(theme);
        // From above the floor lies behind every surface; from below it is in
        // front of them.
        let floor_behind = model.view3d.pitch >= 0.0;
        if floor_behind {
            self.draw_floor(scene, tt, theme);
        }
        for cell in &self.surface_cache.cells {
            let color = scale_rgb(palette.categorical(cell.series), cell.shade);
            scene.fill(vello::peniko::Fill::NonZero, tt, color, None, &cell.path);
            stroke_path(scene, tt, 0.6, wire, &cell.path);
        }
        if !floor_behind {
            self.draw_floor(scene, tt, theme);
        }
        scene.pop_layer();
    }

    /// The floor grid, in the rule colours the 2D plane draws its grid in.
    fn draw_floor(&self, scene: &mut Scene, t: Affine, theme: &Theme) {
        let ground = theme.tokens.colors.surfaces.content;
        let floor = &self.surface_cache.floor;
        let rule = |alpha| ui::surface::edge_on_with(ground, alpha);
        stroke_path(scene, t, 1.0, rule(FLOOR_GRID_ALPHA), &floor.grid);
        stroke_path(scene, t, 1.0, rule(FLOOR_OUTLINE_ALPHA), &floor.outline);
    }

    /// Begin a smooth zoom toward the (already-updated) model zoom.
    pub fn begin_zoom(&mut self) {
        self.zoom.begin();
    }
    /// Settle the zoom immediately (orbit / edit / mode switch, which re-project
    /// regardless).
    pub fn settle_zoom(&mut self, z: f64) {
        self.zoom.settle(z);
    }
    /// Whether a smooth zoom is animating (drives the app's frame pump).
    pub fn zoom_active(&self) -> bool {
        self.zoom.active()
    }

    /// Sample every visible `z = f(x, y)` surface on the grid, project + merge +
    /// depth-sort all their quads, and store the theme-independent geometry.
    /// Only runs when the surfaces, orbit/zoom, domain, or canvas size change.
    /// All surfaces share one z-normalisation so their relative heights are
    /// preserved (a taller surface really sits above a lower one) and one merged
    /// painter's-algorithm sort so overlapping sheets occlude each other.
    #[allow(
        clippy::similar_names,
        reason = "zmin/zmax/zmid are the standard min/max/midpoint trio for one axis's range"
    )]
    fn ensure_surfaces(&mut self, model: &CalcModel) {
        let key = SurfaceKey::build(model, self.canvas);
        if self.surface_cache.key.as_ref() == Some(&key) {
            return;
        }
        let rebuilds = self.surface_cache.rebuilds.wrapping_add(1);

        let (sampled, mut zmin, mut zmax) = Self::sample_surfaces(model);
        if sampled.is_empty() {
            self.surface_cache = SurfaceCache {
                key: Some(key),
                cells: Vec::new(),
                floor: Floor::default(),
                rebuilds,
            };
            return;
        }
        if !zmin.is_finite() || !zmax.is_finite() {
            zmin = -1.0;
            zmax = 1.0;
        }
        let zmid = 0.5 * (zmin + zmax);
        let zspan = (zmax - zmin).max(1e-6);

        let (cells, floor) = self.project_and_rank_cells(&sampled, (zmid, zspan), model);
        self.surface_cache = SurfaceCache {
            key: Some(key),
            cells,
            floor,
            rebuilds,
        };
    }

    /// First pass: sample every visible surface on the `(N+1)×(N+1)` grid,
    /// tracking the shared z-range across all of them (used afterward to
    /// normalise depth for the painter's algorithm).
    fn sample_surfaces(model: &CalcModel) -> (Vec<SampledSurface>, f64, f64) {
        let mut sampled: Vec<SampledSurface> = Vec::new();
        let mut zmin = f64::INFINITY;
        let mut zmax = f64::NEG_INFINITY;
        for (series, f) in model.surfaces.iter().enumerate() {
            if !f.visible || f.expr.trim().is_empty() {
                continue;
            }
            let Ok(comp) = engine::compile(&f.expr, model.deg, true) else {
                continue;
            };
            let mut zs = vec![[0.0f64; N + 1]; N + 1];
            for (i, row) in zs.iter_mut().enumerate() {
                for (j, cell) in row.iter_mut().enumerate() {
                    let x = -DOM + 2.0 * DOM * i as f64 / N as f64;
                    let y = -DOM + 2.0 * DOM * j as f64 / N as f64;
                    let z = comp.eval2(x, y);
                    *cell = z;
                    if z.is_finite() {
                        zmin = zmin.min(z);
                        zmax = zmax.max(z);
                    }
                }
            }
            sampled.push(SampledSurface { series, zs });
        }
        (sampled, zmin, zmax)
    }

    /// Second pass: project every sampled surface's grid cells to screen
    /// space with the shared depth normalisation (`zmid`/`zspan`), shade
    /// each with a Lambert factor, and depth-sort the merged set far-to-near
    /// for the painter's algorithm. Returns the ranked cells.
    fn project_and_rank_cells(
        &self,
        sampled: &[SampledSurface],
        z_normalize: (f64, f64),
        model: &CalcModel,
    ) -> (Vec<SurfaceCell>, Floor) {
        let (zmid, zspan) = z_normalize;
        let v = &model.view3d;
        let (sin_yaw, cos_yaw) = v.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = v.pitch.sin_cos();
        let s = self.canvas.height().min(self.canvas.width()) * 0.56 / (DOM * 1.7) * v.zoom;
        let cx = self.canvas.center().x;
        let cy = self.canvas.center().y;

        // model coords → screen + depth (shared normalisation across surfaces)
        let project = |x: f64, y: f64, z: f64| -> (Point, f64) {
            let zn = (z - zmid) / zspan * (DOM * 0.9);
            let x1 = x * cos_yaw - y * sin_yaw;
            let y1 = x * sin_yaw + y * cos_yaw;
            let z1 = zn;
            let y2 = y1 * cos_pitch - z1 * sin_pitch;
            let z2 = y1 * sin_pitch + z1 * cos_pitch;
            (Point::new(cx + x1 * s, cy - z2 * s), y2)
        };

        let mut cells: Vec<RankedCell> = Vec::with_capacity(sampled.len() * N * N);
        let light = normalize3(-0.4, -0.7, 0.8);
        for surf in sampled {
            Self::rank_surface_cells(&mut cells, surf, &project, light);
        }
        // painter's algorithm across ALL surfaces: far (large depth) first.
        cells.sort_by(|a, b| {
            b.depth
                .partial_cmp(&a.depth)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let floor = Self::project_floor(&project, zmid - zspan * FLOOR_DROP);
        (cells.into_iter().map(|c| c.cell).collect(), floor)
    }

    /// The floor grid at height `z`, projected like the surfaces above it.
    fn project_floor(project: &impl Fn(f64, f64, f64) -> (Point, f64), z: f64) -> Floor {
        let mut floor = Floor::default();
        for k in 0..=FLOOR_CELLS {
            let at = -DOM + 2.0 * DOM * k as f64 / FLOOR_CELLS as f64;
            let rule = |path: &mut BezPath| {
                path.move_to(project(at, -DOM, z).0);
                path.line_to(project(at, DOM, z).0);
                path.move_to(project(-DOM, at, z).0);
                path.line_to(project(DOM, at, z).0);
            };
            if k == 0 || k == FLOOR_CELLS {
                rule(&mut floor.outline);
            } else {
                rule(&mut floor.grid);
            }
        }
        floor
    }

    /// Project one surface's grid into ranked quads, appending them to
    /// `cells`. Skips any quad with a non-finite corner (asymptote/undefined
    /// region).
    fn rank_surface_cells(
        cells: &mut Vec<RankedCell>,
        surf: &SampledSurface,
        project: &impl Fn(f64, f64, f64) -> (Point, f64),
        light: (f64, f64, f64),
    ) {
        let zs = &surf.zs;
        for i in 0..N {
            for j in 0..N {
                let (Some(&z00), Some(&z10), Some(&z11), Some(&z01)) = (
                    zs.get(i).and_then(|row| row.get(j)),
                    zs.get(i + 1).and_then(|row| row.get(j)),
                    zs.get(i + 1).and_then(|row| row.get(j + 1)),
                    zs.get(i).and_then(|row| row.get(j + 1)),
                ) else {
                    continue;
                };
                if !(z00.is_finite() && z10.is_finite() && z11.is_finite() && z01.is_finite()) {
                    continue;
                }
                let x0 = -DOM + 2.0 * DOM * i as f64 / N as f64;
                let x1 = -DOM + 2.0 * DOM * (i + 1) as f64 / N as f64;
                let y0 = -DOM + 2.0 * DOM * j as f64 / N as f64;
                let y1 = -DOM + 2.0 * DOM * (j + 1) as f64 / N as f64;
                let (p00, d00) = project(x0, y0, z00);
                let (p10, d10) = project(x1, y0, z10);
                let (p11, d11) = project(x1, y1, z11);
                let (p01, d01) = project(x0, y1, z01);

                let mut path = BezPath::new();
                path.move_to(p00);
                path.line_to(p10);
                path.line_to(p11);
                path.line_to(p01);
                path.close_path();

                let nx = (x1 - x0).max(1e-6);
                let ny = (y1 - y0).max(1e-6);
                let normal = normalize3(-(z10 - z00) * ny, -(z01 - z00) * nx, nx * ny);
                let lambert = dot3(normal, light).abs().clamp(0.0, 1.0);
                let shade = 0.55 + 0.45 * lambert;
                cells.push(RankedCell {
                    depth: 0.25 * (d00 + d10 + d11 + d01),
                    cell: SurfaceCell {
                        path,
                        series: surf.series,
                        shade,
                    },
                });
            }
        }
    }

    pub fn canvas_contains(&self, p: Point) -> bool {
        // An open floating drawer makes the canvas non-interactive (a press
        // there dismisses the drawer instead of orbiting).
        if self.panel.side().is_overlay() && self.panel.side().is_open() {
            return false;
        }
        self.canvas.contains(p)
    }
}

/// Stroke `path` in `color` at `width`.
fn stroke_path(scene: &mut Scene, t: Affine, width: f64, color: Color, path: &BezPath) {
    scene.stroke(&Stroke::new(width), t, color, None, path);
}

fn scale_rgb(c: Color, f: f64) -> Color {
    let s = |x: u8| (f64::from(x) * f).round().clamp(0.0, 255.0) as u8;
    Color::from_rgba8(s(c.r()), s(c.g()), s(c.b()), c.a())
}

fn normalize3(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let m = (x * x + y * y + z * z).sqrt();
    if m < 1e-12 {
        (0.0, 0.0, 1.0)
    } else {
        (x / m, y / m, z / m)
    }
}

fn dot3(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    a.0 * b.0 + a.1 * b.1 + a.2 * b.2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CalcModel, Mode};

    fn screen() -> Graph3dScreen {
        Graph3dScreen {
            canvas: Rect::new(360.0, 60.0, 1280.0, 800.0),
            ..Graph3dScreen::default()
        }
    }

    /// The Surfaces panel docks when wide, floats as a drawer (button, scrim,
    /// no orbit under the drawer) when narrow, and keeps its row buttons
    /// apart. Fresh screen per phase.
    #[test]
    fn surface_panel_docks_wide_floats_narrow_and_keeps_row_targets() {
        // phase wide_docks_narrow_floats_surface_panel
        {
            let mut s = Graph3dScreen::default();
            let m = CalcModel::seeded();
            let wide = Rect::new(0.0, 0.0, 1280.0, 800.0);
            s.layout(wide, &m);
            assert!(!s.is_overlay() && s.panel_open());
            let column = s.panel.side().column();
            assert!(column.width() > 0.0 && s.canvas.x0 >= column.x1 - 0.5);
            // Narrow: collapsed drawer, the plot takes the full width.
            let narrow = Rect::new(0.0, 0.0, 560.0, 800.0);
            s.layout(narrow, &m);
            assert!(s.is_overlay() && !s.panel_open());
            assert_eq!(s.panel.side().column(), Rect::ZERO);
            assert!((s.canvas.width() - 560.0).abs() < 0.5);
            let toggle = s.toggle_rect().expect("a button opens the drawer");
            assert!(s.toggle_at(toggle.center()));
            // Open it → drawer + scrim; a press inside must not orbit.
            s.toggle_panel();
            s.snap_panel(true);
            s.layout(narrow, &m);
            let column = s.panel.side().column();
            assert!(s.panel_open() && column.width() > 0.0);
            assert!(s.scrim_at(Point::new(narrow.x1 - 10.0, narrow.center().y)));
            assert!(!s.canvas_contains(column.center()));
        }

        // phase row_buttons_have_pointer_targets_that_stay_apart
        {
            let mut s = Graph3dScreen::default();
            let m = CalcModel::seeded();
            let area = Rect::new(0.0, 0.0, 390.0, 844.0);
            s.layout(area, &m);
            s.toggle_panel();
            s.snap_panel(true);
            s.layout(area, &m);
            let target = f64::from(ui::tokens::Metrics::BASE.min_pointer_target);
            let delete = s.delete_rect(0).expect("delete target");
            let visible = s.visibility_rect(0).expect("visibility target");
            assert!(delete.width() >= target - 0.5 && delete.height() >= target - 0.5);
            assert!(visible.width() >= target - 0.5 && visible.height() >= target - 0.5);
            assert!(
                visible.x1 <= delete.x0 + 0.5,
                "row buttons must not overlap"
            );
            assert_eq!(s.delete_at(delete.center()), Some(0));
            assert_eq!(s.vis_at(visible.center()), Some(0));
            assert_eq!(
                s.delete_at(s.delete_rect(1).expect("second delete target").center()),
                Some(1),
                "a delete hit must not target the adjacent row"
            );
            let edit = s.field_rect(0).expect("editor");
            assert!(
                edit.x1 <= visible.x0 + 0.5,
                "math editing must not steal a button's target"
            );
        }
    }

    /// The sampled-surface cache: the first draw samples every visible surface
    /// with theme-independent shading; theme or tab switches do not rebuild;
    /// editing, orbiting or zooming rebuild; hiding a surface drops its cells.
    /// Fresh screen + model per phase.
    #[test]
    fn surface_cache_rebuilds_only_when_geometry_changes() {
        // phase first_draw_samples_every_visible_surface
        {
            let mut s = screen();
            let m = CalcModel::seeded(); // 3 visible surfaces
            s.ensure_surfaces(&m);
            assert_eq!(s.surface_cache.rebuilds, 1);
            assert!(!s.surface_cache.cells.is_empty(), "expected sampled grids");
            // Cells from more than one series prove multiple surfaces are drawn.
            let distinct: std::collections::BTreeSet<usize> =
                s.surface_cache.cells.iter().map(|c| c.series).collect();
            assert!(
                distinct.len() >= 2,
                "multiple surfaces must each contribute cells, got {distinct:?}"
            );
        }

        // phase theme_or_tab_switch_does_not_rebuild_surface
        {
            let mut s = screen();
            let mut m = CalcModel::seeded();
            s.ensure_surfaces(&m);
            assert_eq!(s.surface_cache.rebuilds, 1);
            s.ensure_surfaces(&m);
            s.ensure_surfaces(&m);
            assert_eq!(
                s.surface_cache.rebuilds, 1,
                "theme switch re-sampled the surfaces"
            );
            m.mode = Mode::Calc;
            s.ensure_surfaces(&m);
            m.mode = Mode::Graph3d;
            s.ensure_surfaces(&m);
            assert_eq!(
                s.surface_cache.rebuilds, 1,
                "tab switch re-sampled the surfaces"
            );
        }

        // phase editing_a_surface_or_orbiting_rebuilds
        {
            let mut s = screen();
            let mut m = CalcModel::seeded();
            s.ensure_surfaces(&m);
            assert_eq!(s.surface_cache.rebuilds, 1);
            m.surfaces[0].expr = "x*y".into();
            s.ensure_surfaces(&m);
            assert_eq!(s.surface_cache.rebuilds, 2);
            m.view3d.yaw += 0.2;
            s.ensure_surfaces(&m);
            assert_eq!(s.surface_cache.rebuilds, 3);
            m.view3d.zoom *= 1.3;
            s.ensure_surfaces(&m);
            assert_eq!(s.surface_cache.rebuilds, 4);
        }

        // phase hiding_a_surface_drops_its_cells
        {
            let mut s = screen();
            let mut m = CalcModel::seeded();
            s.ensure_surfaces(&m);
            let all_series: std::collections::BTreeSet<usize> =
                s.surface_cache.cells.iter().map(|c| c.series).collect();
            assert!(all_series.contains(&0));
            m.surfaces[0].visible = false;
            s.ensure_surfaces(&m);
            let after: std::collections::BTreeSet<usize> =
                s.surface_cache.cells.iter().map(|c| c.series).collect();
            assert!(
                !after.contains(&0),
                "a hidden surface must contribute no cells"
            );
        }

        // phase cached_cells_carry_theme_independent_shading
        {
            let mut s = screen();
            let m = CalcModel::seeded();
            s.ensure_surfaces(&m);
            for cell in &s.surface_cache.cells {
                assert!(cell.shade >= 0.55 && cell.shade <= 1.0);
            }
        }
    }
}
