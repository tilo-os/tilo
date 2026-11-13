//! 3D grapher: a `z = f(x, y)` surface list and a canvas that samples every
//! visible surface, projects each vertex with the orbit and orthographic
//! projection, and depth-sorts all their quads in one painter's-algorithm pass,
//! so sheets occlude each other. Drag orbits, the wheel zooms.

use ui::charts::ChartPalette;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{CanvasNavigation, CanvasNavigationHint};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::Color;

use crate::engine;
use crate::graph2d::Pointer;
use crate::model::CalcModel;
use crate::plot_panel::{Formulas, PlotPanel, SURFACES};
use crate::zoom::ZoomAnim3d;

/// Half-width of the sampled domain, in x and y.
const DOM: f64 = 7.0;
/// Grid cells per axis.
const N: usize = 40;
const FLOOR_CELLS: usize = 7;
/// How far under the lowest surface the floor sits, as a share of their height
/// range.
const FLOOR_DROP: f64 = 0.6;
const FLOOR_GRID_ALPHA: u8 = 38;
const FLOOR_OUTLINE_ALPHA: u8 = 96;

pub struct Graph3dScreen {
    pub canvas: Rect,
    area: Rect,
    pub panel: PlotPanel,
    /// Projected and depth-sorted cells for every visible surface, rebuilt only
    /// when the surfaces, the orbit or zoom, the domain or the canvas changes;
    /// the colour is resolved from the theme at paint time.
    surface_cache: SurfaceCache,
    /// While active, the cached cells are replayed scaled about the canvas
    /// centre instead of re-projecting.
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

/// One painted quad: the function's series index and its Lambert shade, both
/// theme-independent, so a re-theme replays the cached geometry.
struct SurfaceCell {
    path: BezPath,
    series: usize,
    shade: f64,
}

/// The inputs that affect surface geometry, minus the theme, so a re-theme
/// replays the cached cells.
#[derive(Clone, PartialEq, Default)]
struct SurfaceKey {
    surfaces: Vec<(String, bool)>,
    yaw: u64,
    pitch: u64,
    zoom: u64,
    canvas: (u64, u64, u64, u64),
}

/// A square grid on the domain, so the plot has a scale to read heights
/// against.
#[derive(Default)]
struct Floor {
    grid: BezPath,
    outline: BezPath,
}

#[derive(Default)]
struct SurfaceCache {
    key: Option<SurfaceKey>,
    cells: Vec<SurfaceCell>,
    floor: Floor,
    /// World-space heights survive camera and viewport changes, so orbiting
    /// only re-projects them.
    sampled: Vec<SampledSurface>,
    z_range: (f64, f64),
    sample_rebuilds: u32,
    /// Lets a test prove a theme switch reuses the cached surfaces.
    rebuilds: u32,
}

/// One surface's grid of sampled z-values, row-major.
struct SampledSurface {
    series: usize,
    zs: Vec<[f64; N + 1]>,
}

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
    /// The plot fills the content column: its controls are the window bar's.
    pub fn layout(&mut self, area: Rect, model: &CalcModel) {
        self.area = area;
        self.panel.layout(area, model.surfaces.len());
        self.canvas = self.panel.side().content();
    }

    /// Call once per draw, before painting.
    pub fn prepare(&mut self, tr: &mut TextRenderer, theme: &Theme, model: &CalcModel, area: Rect) {
        self.panel.fit(&model.surfaces, tr, theme);
        self.layout(area, model);
        self.panel.side_mut().tick(theme);
        self.zoom.tick(model.view3d.zoom, theme.reduce_motion);
        self.layout(area, model);
    }

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
        // The projection scale is proportional to zoom, so the cached cells
        // replay scaled about the centre; one re-projection when settled.
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
        // The ground shows through between cells as the mesh.
        let colors = &theme.tokens.colors;
        let wire = colors.surfaces.content;
        let palette = ChartPalette::from_theme(theme);
        // From above the floor is behind every surface; from below, in front.
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

    /// In the rule colours the 2D plane draws its grid in.
    fn draw_floor(&self, scene: &mut Scene, t: Affine, theme: &Theme) {
        let ground = theme.tokens.colors.surfaces.content;
        let floor = &self.surface_cache.floor;
        let rule = |alpha| ui::surface::edge_on_with(ground, alpha);
        stroke_path(scene, t, 1.0, rule(FLOOR_GRID_ALPHA), &floor.grid);
        stroke_path(scene, t, 1.0, rule(FLOOR_OUTLINE_ALPHA), &floor.outline);
    }

    pub fn begin_zoom(&mut self) {
        self.zoom.begin();
    }
    /// Orbit, edit and mode switch re-project anyway.
    pub fn settle_zoom(&mut self, z: f64) {
        self.zoom.settle(z);
    }
    /// Drives the app's frame pump.
    pub fn zoom_active(&self) -> bool {
        self.zoom.active()
    }

    /// Samples every visible surface and stores the theme-independent geometry.
    /// One z-normalisation across surfaces keeps their relative heights, and one
    /// merged sort makes overlapping sheets occlude each other.
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

        let resample = self
            .surface_cache
            .key
            .as_ref()
            .is_none_or(|previous| previous.surfaces != key.surfaces);
        let sample_rebuilds = self.surface_cache.sample_rebuilds + u32::from(resample);
        let (sampled, mut zmin, mut zmax) = if resample {
            Self::sample_surfaces(model)
        } else {
            let (min, max) = self.surface_cache.z_range;
            (std::mem::take(&mut self.surface_cache.sampled), min, max)
        };
        if sampled.is_empty() {
            self.surface_cache = SurfaceCache {
                key: Some(key),
                cells: Vec::new(),
                floor: Floor::default(),
                sampled,
                z_range: (zmin, zmax),
                sample_rebuilds,
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
            sampled,
            z_range: (zmin, zmax),
            sample_rebuilds,
            rebuilds,
        };
    }

    /// First pass: the shared z-range across all surfaces normalises depth for
    /// the sort.
    fn sample_surfaces(model: &CalcModel) -> (Vec<SampledSurface>, f64, f64) {
        let mut sampled: Vec<SampledSurface> = Vec::new();
        let mut zmin = f64::INFINITY;
        let mut zmax = f64::NEG_INFINITY;
        for (series, f) in model.surfaces.iter().enumerate() {
            if !f.visible || f.expr.trim().is_empty() {
                continue;
            }
            let Ok(comp) = engine::compile(&f.expr, engine::PLOT_IN_DEGREES, true) else {
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

    /// Second pass: project each cell, shade it, and sort the merged set far to
    /// near.
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

        // Screen point and depth, normalised across surfaces.
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
        // Far first, across all surfaces.
        cells.sort_by(|a, b| {
            b.depth
                .partial_cmp(&a.depth)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let floor = Self::project_floor(&project, zmid - zspan * FLOOR_DROP);
        (cells.into_iter().map(|c| c.cell).collect(), floor)
    }

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

    /// A quad with a non-finite corner is skipped: an asymptote has no cell.
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
        // An open drawer makes the canvas non-interactive: a press dismisses
        // it instead of orbiting.
        !self.panel.floats_open() && self.canvas.contains(p)
    }
}

fn stroke_path(scene: &mut Scene, t: Affine, width: f64, color: Color, path: &BezPath) {
    scene.stroke(&Stroke::new(width), t, color, None, path);
}

fn scale_rgb(color: Color, factor: f64) -> Color {
    let [r, g, b, a] = color.components;
    let factor = factor as f32;
    vello::peniko::color::AlphaColor::<vello::peniko::color::Srgb>::new([
        (r * factor).clamp(0.0, 1.0),
        (g * factor).clamp(0.0, 1.0),
        (b * factor).clamp(0.0, 1.0),
        a,
    ])
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

    /// Docks when wide, floats as a drawer when narrow, and keeps its row
    /// buttons apart.
    #[test]
    fn surface_panel_docks_wide_floats_narrow_and_keeps_row_targets() {
        // phase wide_docks_narrow_floats_surface_panel
        {
            let mut s = Graph3dScreen::default();
            let m = CalcModel::seeded();
            let wide = Rect::new(0.0, 0.0, 1280.0, 800.0);
            s.layout(wide, &m);
            assert!(!s.panel.side().is_overlay() && s.panel.side().is_open());
            let column = s.panel.side().column();
            assert!(column.width() > 0.0 && s.canvas.x0 >= column.x1 - 0.5);
            let narrow = Rect::new(0.0, 0.0, 560.0, 800.0);
            s.layout(narrow, &m);
            assert!(s.panel.side().is_overlay() && !s.panel.side().is_open());
            assert_eq!(s.panel.side().column(), Rect::ZERO);
            assert!((s.canvas.width() - 560.0).abs() < 0.5);
            // Open it → drawer + scrim; a press inside must not orbit.
            s.panel.side_mut().toggle();
            s.panel.side_mut().snap(true);
            s.layout(narrow, &m);
            let column = s.panel.side().column();
            assert!(s.panel.side().is_open() && column.width() > 0.0);
            assert!(
                s.panel
                    .side()
                    .scrim_at(Point::new(narrow.x1 - 10.0, narrow.center().y))
            );
            assert!(!s.canvas_contains(column.center()));
        }

        {
            let mut s = Graph3dScreen::default();
            let m = CalcModel::seeded();
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

    #[test]
    fn orbit_and_resize_reproject_without_resampling_equations() {
        let mut screen = screen();
        let mut model = CalcModel::seeded();
        screen.ensure_surfaces(&model);
        let first = screen.surface_cache.cells.first().unwrap().path.clone();
        model.view3d.yaw += 0.4;
        model.view3d.pitch += 0.2;
        screen.ensure_surfaces(&model);
        assert_eq!(screen.surface_cache.sample_rebuilds, 1);
        assert_ne!(screen.surface_cache.cells.first().unwrap().path, first);
        screen.canvas.x1 += 100.0;
        model.view3d.zoom *= 1.2;
        screen.ensure_surfaces(&model);
        assert_eq!(screen.surface_cache.sample_rebuilds, 1);
        model.surfaces.first_mut().unwrap().expr = "x*y".into();
        screen.ensure_surfaces(&model);
        assert_eq!(screen.surface_cache.sample_rebuilds, 2);
        // The angle unit is for calculations; a plot's axes stay numbers.
        model.deg = true;
        screen.ensure_surfaces(&model);
        assert_eq!(screen.surface_cache.sample_rebuilds, 2);
    }

    /// A surface reads x and y as plain numbers whatever the angle unit, so
    /// `sin(x)` waves through its range instead of lying flat.
    #[test]
    fn surfaces_plot_in_radians_whatever_the_angle_unit() {
        let mut model = CalcModel::seeded();
        for surface in &mut model.surfaces {
            surface.visible = false;
        }
        let first = model.surfaces.first_mut().unwrap();
        first.expr = "sin(x)".into();
        first.visible = true;
        model.deg = true;
        let (_, zmin, zmax) = Graph3dScreen::sample_surfaces(&model);
        assert!(zmin < -0.9 && zmax > 0.9, "sin(x) spans {zmin}..{zmax}");
    }

    /// A theme switch does not rebuild; editing, orbiting or zooming does;
    /// hiding a surface drops its cells.
    #[test]
    fn surface_cache_rebuilds_only_when_geometry_changes() {
        // phase first_draw_samples_every_visible_surface
        {
            let mut s = screen();
            let m = CalcModel::seeded(); // 3 visible surfaces
            s.ensure_surfaces(&m);
            assert_eq!(s.surface_cache.rebuilds, 1);
            assert!(!s.surface_cache.cells.is_empty(), "expected sampled grids");
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
