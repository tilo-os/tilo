//! 3D grapher: samples `z = f(x, y)` on a grid, projects each vertex to 2D
//! with yaw/pitch orbit + orthographic projection, sorts the quads
//! back-to-front (painter's algorithm) and paints a height-shaded surface
//! with a wireframe overlay. Drag orbits, scroll zooms.

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect};
use vello::peniko::Color;

use crate::draw::{self, Align};
use crate::engine;
use crate::mathfield::MathField;
use crate::model::CalcModel;

/// Half-width of the sampled domain in x and y.
const DOM: f64 = 7.0;
/// Grid resolution (cells per axis).
const N: usize = 40;

/// Vertical padding between the surface card's edge and the math-field viewport.
const EXPR_PAD_Y: f64 = 7.0;
/// Surface card height for a short, single-line expression.
const BASE_EXPR_H: f64 = crate::mathfield::CARD_MIN_H + 2.0 * EXPR_PAD_Y;

/// Below this body width the Surface panel collapses to a toggle + overlay
/// drawer instead of docking (see [`crate::graph2d`] for the matching 2D rail).
const PANEL_DOCK_MIN_W: f64 = 720.0;
/// Overlay drawer width when the panel floats over the canvas.
const PANEL_OVERLAY_W: f64 = 300.0;

pub struct Graph3dScreen {
    pub panel: Rect,
    pub canvas: Rect,
    expr_rect: Rect,
    /// Panel is an overlay drawer (narrow) rather than docked (wide).
    overlay: bool,
    /// Whether the panel is currently shown.
    panel_open: bool,
    /// Whether the user explicitly toggled the panel this session.
    user_toggled: bool,
    /// Toggle pill / hide chip for the overlay drawer.
    toggle_rect: Rect,
    /// Dismiss scrim behind an open overlay drawer.
    scrim_rect: Rect,
    /// Grown (outer) height of the surface expression card, measured in `draw`
    /// and reused by `layout` on the event path so the box the user clicks
    /// matches what was painted. A tall stacked surface (the default
    /// `sin(√(x²+y²))`) grows the card to the max, then scrolls within it.
    expr_card_h: f64,
    /// The 2D math editor for the `z = f(x, y)` surface expression.
    field: MathField,
    /// Projected + depth-sorted surface geometry, rebuilt only when the
    /// surface expression, orbit/zoom, domain, or canvas size change — never
    /// on a theme or tab switch. Cell colour is resolved from the live theme
    /// at paint time from the cached height fraction + shade, so re-theming is
    /// a recolour, not a re-sample / re-project / re-sort.
    surface_cache: SurfaceCache,
}

impl Default for Graph3dScreen {
    fn default() -> Self {
        Self {
            panel: Rect::ZERO,
            canvas: Rect::ZERO,
            expr_rect: Rect::ZERO,
            overlay: false,
            panel_open: true,
            // Shots-only: force the panel open for the headless narrow-width
            // screenshot (no effect when unset; live default is unchanged).
            user_toggled: std::env::var_os("QOS_CALC_PANEL_OPEN").is_some(),
            toggle_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            expr_card_h: BASE_EXPR_H,
            field: MathField::new("").with_size(20.0),
            surface_cache: SurfaceCache::default(),
        }
    }
}

/// One painted quad of the surface. `height_f` is the 0..1 height fraction
/// (drives the theme gradient) and `shade` is the Lambert factor; both are
/// theme-independent, so the live colour is `scale_rgb(height_color(theme,
/// height_f), shade)` computed fresh each frame from cached geometry.
struct SurfaceCell {
    path: BezPath,
    height_f: f64,
    shade: f64,
}

/// Identity of the inputs that affect 3D surface geometry. Excludes the theme
/// so a re-theme replays the cached cells without re-sampling.
#[derive(Clone, PartialEq, Default)]
struct SurfaceKey {
    surface: String,
    deg: bool,
    yaw: u64,
    pitch: u64,
    zoom: u64,
    canvas: (u64, u64, u64, u64),
}

#[derive(Default)]
struct SurfaceCache {
    key: Option<SurfaceKey>,
    cells: Vec<SurfaceCell>,
    /// Cached orientation tripod endpoints (origin, +x tip, +y tip) so the
    /// axes redraw without re-projecting.
    axes: Option<(Point, Point, Point)>,
    zmin: f64,
    zmax: f64,
    /// Counts actual (re)samples; lets a test prove a theme/tab switch reuses
    /// the cached surface instead of re-sampling + re-projecting + re-sorting.
    rebuilds: u32,
}

fn bits(x: f64) -> u64 {
    x.to_bits()
}

impl SurfaceKey {
    fn build(model: &CalcModel, canvas: Rect) -> Self {
        SurfaceKey {
            surface: model.surface.clone(),
            deg: model.deg,
            yaw: bits(model.view3d.yaw),
            pitch: bits(model.view3d.pitch),
            zoom: bits(model.view3d.zoom),
            canvas: (bits(canvas.x0), bits(canvas.y0), bits(canvas.x1), bits(canvas.y1)),
        }
    }
}

impl Graph3dScreen {
    /// Resolve docked-vs-overlay + panel / canvas / scrim / toggle rects from
    /// the current width. Re-derived every layout pass so a resize reflows.
    fn resolve_panel(&mut self, area: Rect) {
        let can_dock = area.width() >= PANEL_DOCK_MIN_W;
        self.overlay = !can_dock;
        if !self.user_toggled {
            self.panel_open = can_dock;
        }
        if can_dock && self.panel_open {
            let panel_w = (area.width() * 0.27).clamp(260.0, 360.0);
            self.panel = Rect::new(area.x0, area.y0, area.x0 + panel_w, area.y1);
            self.canvas = Rect::new(self.panel.x1, area.y0, area.x1, area.y1);
            self.scrim_rect = Rect::ZERO;
            self.toggle_rect = Rect::ZERO;
        } else {
            self.canvas = Rect::new(area.x0, area.y0, area.x1, area.y1);
            if self.overlay && self.panel_open {
                let w = PANEL_OVERLAY_W.min(area.width() - 40.0);
                self.panel = Rect::new(area.x0, area.y0, area.x0 + w, area.y1);
                self.scrim_rect = Rect::new(self.panel.x1, area.y0, area.x1, area.y1);
                self.toggle_rect = Rect::new(self.panel.x1 - 66.0, self.panel.y0 + 14.0, self.panel.x1 - 16.0, self.panel.y0 + 42.0);
            } else {
                self.panel = Rect::ZERO;
                self.scrim_rect = Rect::ZERO;
                self.toggle_rect = Rect::new(area.x0 + 12.0, area.y0 + 12.0, area.x0 + 144.0, area.y0 + 44.0);
            }
        }
    }

    pub fn layout(&mut self, area: Rect) {
        self.resolve_panel(area);
        if self.panel == Rect::ZERO {
            self.expr_rect = Rect::ZERO;
            return;
        }
        // The surface card grows with the formula (measured in `draw`), so a
        // stacked expression is shown in a taller box instead of being clipped.
        let top = self.panel.y0 + 60.0;
        self.expr_rect = Rect::new(self.panel.x0 + 16.0, top, self.panel.x1 - 16.0, top + self.expr_card_h);
    }

    // -- overlay panel chrome ------------------------------------------------

    pub fn toggle_at(&self, p: Point) -> bool {
        self.toggle_rect != Rect::ZERO && self.toggle_rect.contains(p)
    }
    pub fn scrim_at(&self, p: Point) -> bool {
        self.overlay && self.panel_open && self.scrim_rect.contains(p)
    }
    pub fn toggle_panel(&mut self) {
        self.panel_open = !self.panel_open;
        self.user_toggled = true;
    }
    pub fn close_panel(&mut self) {
        self.panel_open = false;
        self.user_toggled = true;
    }
    pub fn overlay_captures(&self, p: Point) -> bool {
        self.overlay && self.panel_open && self.panel.contains(p)
    }
    /// Whether the Surface panel is currently shown (test/introspection).
    pub fn panel_open(&self) -> bool {
        self.panel_open
    }
    /// Center of the panel toggle, if one is placed this frame (test helper).
    pub fn toggle_center(&self) -> Option<Point> {
        (self.toggle_rect != Rect::ZERO).then(|| self.toggle_rect.center())
    }

    /// Paint remote collaborators' carets into the surface field, snapping each
    /// scene-space cursor to the field's 2D glyph layout.
    pub fn draw_remote_carets(&self, scene: &mut Scene, t: Affine, cursors: &[(Point, Color)]) {
        self.field.draw_remote_carets(scene, t, cursors);
    }

    /// Feed the surface expression into the editor (no-op when unchanged).
    pub fn sync_field(&mut self, model: &CalcModel) {
        self.field.set_source(&model.surface);
    }

    /// Measure the surface formula and grow the card to fit it up to the max.
    /// Run before `layout` in `draw` (a `TextRenderer` is available there).
    fn measure_card(&mut self, model: &CalcModel, tr: &mut TextRenderer) {
        self.sync_field(model);
        let content_h = if self.field.is_empty() {
            crate::mathfield::CARD_MIN_H
        } else {
            self.field.content_height(tr)
        };
        let field_h = crate::mathfield::card_view_height(content_h);
        self.expr_card_h = field_h + 2.0 * EXPR_PAD_Y;
    }

    /// The surface math editor.
    pub fn field_mut(&mut self) -> &mut MathField {
        &mut self.field
    }

    /// Whether `p` is inside the surface editor box.
    pub fn expr_contains(&self, p: Point) -> bool {
        self.expr_rect.contains(p)
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
        area: Rect,
    ) {
        // Measure the surface formula → grow the card, then lay out from it.
        self.measure_card(model, tr);
        self.layout(area);
        let c = &theme.tokens.colors;

        // background
        draw::fill_rect(scene, t, self.canvas, c.neutral[0]);

        let (zmin, zmax) = self.draw_surface(scene, t, theme, model);
        draw::stroke_rrect(scene, t, self.canvas, 0.0, c.border_standard, 1.0);

        // Collapsed overlay: only a floating toggle over the plot.
        if self.panel == Rect::ZERO {
            if self.overlay {
                self.draw_toggle(scene, t, tr, theme);
            }
            return;
        }
        // Open overlay drawer: dismiss scrim over the canvas first.
        if self.overlay {
            draw::fill_rect(scene, t, self.scrim_rect, draw::with_alpha(Color::BLACK, 110));
        }

        // ---- panel ---------------------------------------------------------
        let panel_bg = if self.overlay { c.surfaces.raised } else { c.surfaces.base };
        draw::fill_rect(scene, t, self.panel, panel_bg);
        draw::line(scene, t, Point::new(self.panel.x1, self.panel.y0), Point::new(self.panel.x1, self.panel.y1), c.border_standard, if self.overlay { 1.5 } else { 1.0 });
        let head = Rect::new(self.panel.x0 + 16.0, self.panel.y0 + 12.0, self.panel.x1 - 16.0, self.panel.y0 + 36.0);
        draw::text_in(scene, tr, t, head, "Surface", 16.0, c.text_primary, Align::Left, TextWeight::Semibold);
        let sub = Rect::new(self.panel.x0 + 16.0, self.panel.y0 + 34.0, self.panel.x1 - 16.0, self.panel.y0 + 54.0);
        draw::text_in(scene, tr, t, sub, "z = f(x, y)", 12.0, c.text_dim, Align::Left, TextWeight::Regular);

        // expression box (always the edit target in 3D mode): a real 2D math
        // editor for z = f(x, y). Already synced + measured by `measure_card`.
        draw::fill_rrect(scene, t, self.expr_rect, 8.0, c.neutral[1]);
        draw::stroke_rrect(scene, t, self.expr_rect, 8.0, c.accent[5], 1.0);
        let valid = engine::compile(&model.surface, model.deg, true).is_ok();
        let col = if valid { c.text_primary } else { c.semantic.danger };
        let prefix = Rect::new(self.expr_rect.x0 + 12.0, self.expr_rect.y0, self.expr_rect.x0 + 40.0, self.expr_rect.y1);
        draw::text_in(scene, tr, t, prefix, "z =", 14.0, col, Align::Left, TextWeight::Medium);
        // The field viewport fills the grown card minus its vertical padding, so
        // it grows with the card and only scrolls when content exceeds the max.
        let edit = Rect::new(self.expr_rect.x0 + 40.0, self.expr_rect.y0 + EXPR_PAD_Y, self.expr_rect.x1 - 8.0, self.expr_rect.y1 - EXPR_PAD_Y);
        self.field.set_rect(edit, tr);
        self.field.draw(scene, t, tr, theme, col, true);

        // controls readout
        let v = &model.view3d;
        let info = Rect::new(self.panel.x0 + 16.0, self.expr_rect.y1 + 16.0, self.panel.x1 - 16.0, self.expr_rect.y1 + 96.0);
        draw::fill_rrect(scene, t, info, 8.0, c.neutral[1]);
        let l1 = Rect::new(info.x0 + 12.0, info.y0 + 6.0, info.x1 - 12.0, info.y0 + 26.0);
        draw::text_in(scene, tr, t, l1, &format!("yaw  {:.0}°   pitch  {:.0}°", v.yaw.to_degrees(), v.pitch.to_degrees()), 12.0, c.text_dim, Align::Left, TextWeight::Regular);
        let l2 = Rect::new(info.x0 + 12.0, info.y0 + 28.0, info.x1 - 12.0, info.y0 + 48.0);
        draw::text_in(scene, tr, t, l2, &format!("zoom  {:.2}×   domain ±{:.0}", v.zoom, DOM), 12.0, c.text_dim, Align::Left, TextWeight::Regular);
        let l3 = Rect::new(info.x0 + 12.0, info.y0 + 50.0, info.x1 - 12.0, info.y1 - 6.0);
        draw::text_in(scene, tr, t, l3, "drag to rotate · scroll to zoom", 12.0, c.accent[6], Align::Left, TextWeight::Regular);

        // height legend
        self.draw_legend(scene, t, tr, theme, zmin, zmax, info.y1 + 16.0);

        // Overlay drawer keeps a "Hide" chip in its header.
        if self.overlay {
            self.draw_toggle(scene, t, tr, theme);
        }
    }

    /// Draw the panel toggle: a "Surface" pill over the canvas when the drawer
    /// is closed, or a "Hide" chip in the drawer header when it's open.
    fn draw_toggle(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        if self.toggle_rect == Rect::ZERO {
            return;
        }
        let c = &theme.tokens.colors;
        let r = self.toggle_rect;
        if self.panel_open {
            draw::fill_rrect(scene, t, r, 8.0, c.neutral[2]);
            draw::text_in(scene, tr, t, r, "Hide", 12.0, c.text_dim, Align::Center, TextWeight::Semibold);
        } else {
            draw::fill_rrect(scene, t, r, 8.0, draw::mix(c.surfaces.raised, c.accent[5], 0.20));
            draw::stroke_rrect(scene, t, r, 8.0, c.accent[5], 1.0);
            draw::text_in(scene, tr, t, r, "Surface", 13.0, c.accent[6], Align::Center, TextWeight::Semibold);
        }
    }

    fn draw_legend(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, zmin: f64, zmax: f64, top: f64) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(self.panel.x0 + 16.0, top, self.panel.x0 + 36.0, top + 120.0);
        let steps = 24;
        for i in 0..steps {
            let f0 = i as f64 / steps as f64;
            let seg = Rect::new(bar.x0, bar.y1 - (f0 + 1.0 / steps as f64) * bar.height(), bar.x1, bar.y1 - f0 * bar.height());
            draw::fill_rect(scene, t, seg, height_color(theme, f0));
        }
        draw::stroke_rrect(scene, t, bar, 2.0, c.border_standard, 1.0);
        let hi = Rect::new(bar.x1 + 8.0, bar.y0 - 2.0, bar.x1 + 90.0, bar.y0 + 16.0);
        draw::text_in(scene, tr, t, hi, &format!("max {}", engine::format_number(zmax)), 11.0, c.text_dim, Align::Left, TextWeight::Regular);
        let lo = Rect::new(bar.x1 + 8.0, bar.y1 - 14.0, bar.x1 + 90.0, bar.y1 + 2.0);
        draw::text_in(scene, tr, t, lo, &format!("min {}", engine::format_number(zmin)), 11.0, c.text_dim, Align::Left, TextWeight::Regular);
    }

    /// Returns (zmin, zmax) actually sampled.
    fn draw_surface(&mut self, scene: &mut Scene, t: Affine, theme: &Theme, model: &CalcModel) -> (f64, f64) {
        self.ensure_surface(model);

        scene.push_layer(vello::peniko::Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &self.canvas);
        let wire = draw::with_alpha(theme.tokens.colors.neutral[0], 90);
        let wire_stroke = vello::kurbo::Stroke::new(0.6);
        // Re-colour each cached cell from the live theme; geometry is untouched.
        for cell in &self.surface_cache.cells {
            let color = scale_rgb(height_color(theme, cell.height_f), cell.shade);
            scene.fill(vello::peniko::Fill::NonZero, t, color, None, &cell.path);
            scene.stroke(&wire_stroke, t, wire, None, &cell.path);
        }
        // orientation tripod (cached endpoints, re-coloured from the theme)
        if let Some((o, xa, ya)) = self.surface_cache.axes {
            let c = &theme.tokens.colors;
            draw::line(scene, t, o, xa, draw::with_alpha(c.semantic.danger, 180), 1.2);
            draw::line(scene, t, o, ya, draw::with_alpha(c.semantic.success, 180), 1.2);
        }
        scene.pop_layer();

        (self.surface_cache.zmin, self.surface_cache.zmax)
    }

    /// Sample `z = f(x, y)` on the grid, project + depth-sort every quad, and
    /// store the theme-independent geometry. Only runs when the surface
    /// expression, orbit/zoom, domain, or canvas size change — so a theme or
    /// tab switch never pays for the (1681-eval + project + sort) rebuild.
    fn ensure_surface(&mut self, model: &CalcModel) {
        let key = SurfaceKey::build(model, self.canvas);
        if self.surface_cache.key.as_ref() == Some(&key) {
            return;
        }

        let rebuilds = self.surface_cache.rebuilds.wrapping_add(1);
        let comp = match engine::compile(&model.surface, model.deg, true) {
            Ok(c) => c,
            Err(_) => {
                self.surface_cache = SurfaceCache {
                    key: Some(key),
                    cells: Vec::new(),
                    axes: None,
                    zmin: 0.0,
                    zmax: 0.0,
                    rebuilds,
                };
                return;
            }
        };

        // sample grid
        let mut zs = vec![[0.0f64; N + 1]; N + 1];
        let mut zmin = f64::INFINITY;
        let mut zmax = f64::NEG_INFINITY;
        for i in 0..=N {
            for j in 0..=N {
                let x = -DOM + 2.0 * DOM * i as f64 / N as f64;
                let y = -DOM + 2.0 * DOM * j as f64 / N as f64;
                let z = comp.eval2(x, y);
                zs[i][j] = z;
                if z.is_finite() {
                    zmin = zmin.min(z);
                    zmax = zmax.max(z);
                }
            }
        }
        if !zmin.is_finite() || !zmax.is_finite() {
            zmin = -1.0;
            zmax = 1.0;
        }
        let zmid = 0.5 * (zmin + zmax);
        let zspan = (zmax - zmin).max(1e-6);

        let v = &model.view3d;
        let (sa, ca) = v.yaw.sin_cos();
        let (sb, cb) = v.pitch.sin_cos();
        let s = self.canvas.height().min(self.canvas.width()) * 0.40 / (DOM * 1.7) * v.zoom;
        let cx = self.canvas.center().x;
        let cy = self.canvas.center().y;

        // map model coords → screen + depth
        let project = |x: f64, y: f64, z: f64| -> (Point, f64) {
            // normalise height to be comparable with x/y extent
            let zn = (z - zmid) / zspan * (DOM * 0.9);
            let x1 = x * ca - y * sa;
            let y1 = x * sa + y * ca;
            let z1 = zn;
            let y2 = y1 * cb - z1 * sb;
            let z2 = y1 * sb + z1 * cb;
            (Point::new(cx + x1 * s, cy - z2 * s), y2)
        };

        struct Cell {
            depth: f64,
            cell: SurfaceCell,
        }
        let mut cells: Vec<Cell> = Vec::with_capacity(N * N);
        let light = normalize3(-0.4, -0.7, 0.8);
        for i in 0..N {
            for j in 0..N {
                let z00 = zs[i][j];
                let z10 = zs[i + 1][j];
                let z11 = zs[i + 1][j + 1];
                let z01 = zs[i][j + 1];
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

                // height fraction (drives the theme gradient at paint time)
                let zavg = 0.25 * (z00 + z10 + z11 + z01);
                let height_f = ((zavg - zmin) / zspan).clamp(0.0, 1.0);
                // lambert shade from the cell normal (model space)
                let nx = (x1 - x0).max(1e-6);
                let ny = (y1 - y0).max(1e-6);
                let normal = normalize3(-(z10 - z00) * ny, -(z01 - z00) * nx, nx * ny);
                let lambert = (dot3(normal, light)).abs().clamp(0.0, 1.0);
                let shade = 0.55 + 0.45 * lambert;
                cells.push(Cell {
                    depth: 0.25 * (d00 + d10 + d11 + d01),
                    cell: SurfaceCell { path, height_f, shade },
                });
            }
        }
        // painter's algorithm: far (large depth) first
        cells.sort_by(|a, b| b.depth.partial_cmp(&a.depth).unwrap_or(std::cmp::Ordering::Equal));

        let (o, _) = project(0.0, 0.0, 0.0);
        let (xa, _) = project(DOM * 1.05, 0.0, 0.0);
        let (ya, _) = project(0.0, DOM * 1.05, 0.0);

        self.surface_cache = SurfaceCache {
            key: Some(key),
            cells: cells.into_iter().map(|c| c.cell).collect(),
            axes: Some((o, xa, ya)),
            zmin,
            zmax,
            rebuilds,
        };
    }

    pub fn canvas_contains(&self, p: Point) -> bool {
        // An open overlay drawer makes the canvas non-interactive (a press there
        // dismisses the drawer instead of orbiting).
        if self.overlay && self.panel_open {
            return false;
        }
        self.canvas.contains(p)
    }
}

/// Height gradient: low → high across two theme accent stops.
fn height_color(theme: &Theme, f: f64) -> Color {
    let c = &theme.tokens.colors;
    if f < 0.5 {
        draw::mix(c.accent[2], c.accent[5], f * 2.0)
    } else {
        draw::mix(c.accent[5], c.accent[8], (f - 0.5) * 2.0)
    }
}

fn scale_rgb(c: Color, f: f64) -> Color {
    let s = |x: u8| (x as f64 * f).round().clamp(0.0, 255.0) as u8;
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
        let mut s = Graph3dScreen::default();
        s.canvas = Rect::new(360.0, 60.0, 1280.0, 800.0);
        s
    }

    #[test]
    fn wide_docks_narrow_collapses_surface_panel() {
        let mut s = Graph3dScreen::default();
        // Wide: docked.
        s.layout(Rect::new(0.0, 60.0, 1280.0, 800.0));
        assert!(!s.overlay && s.panel_open);
        assert!(s.panel.width() > 0.0 && s.canvas.x0 >= s.panel.x1 - 0.5);
        // Narrow: collapsed overlay, canvas takes the full width.
        s.layout(Rect::new(0.0, 60.0, 560.0, 800.0));
        assert!(s.overlay && !s.panel_open);
        assert_eq!(s.panel, Rect::ZERO);
        assert!((s.canvas.width() - 560.0).abs() < 0.5);
        assert!(s.toggle_at(s.toggle_rect.center()));
        // Open it → drawer + scrim; press inside must not orbit.
        s.toggle_panel();
        s.layout(Rect::new(0.0, 60.0, 560.0, 800.0));
        assert!(s.panel_open && s.panel.width() > 0.0 && s.scrim_rect.width() > 0.0);
        assert!(!s.canvas_contains(s.panel.center()));
        // Re-widening re-docks (un-toggled path already exercised; here the user
        // opened it, so it stays open but docks).
    }

    #[test]
    fn first_draw_samples_the_surface() {
        let mut s = screen();
        let m = CalcModel::seeded(); // surface = sin(sqrt(x^2+y^2))
        s.ensure_surface(&m);
        assert_eq!(s.surface_cache.rebuilds, 1);
        assert!(!s.surface_cache.cells.is_empty(), "expected a sampled grid");
    }

    #[test]
    fn theme_or_tab_switch_does_not_rebuild_surface() {
        let mut s = screen();
        let mut m = CalcModel::seeded();
        s.ensure_surface(&m);
        assert_eq!(s.surface_cache.rebuilds, 1);
        // Re-call with an unchanged model (what a theme switch does): no rebuild.
        s.ensure_surface(&m);
        s.ensure_surface(&m);
        assert_eq!(s.surface_cache.rebuilds, 1, "theme switch re-sampled the surface");
        // Tab away and back: still identical surface/view/canvas.
        m.mode = Mode::Calc;
        s.ensure_surface(&m);
        m.mode = Mode::Graph3d;
        s.ensure_surface(&m);
        assert_eq!(s.surface_cache.rebuilds, 1, "tab switch re-sampled the surface");
    }

    #[test]
    fn editing_surface_or_orbiting_rebuilds() {
        let mut s = screen();
        let mut m = CalcModel::seeded();
        s.ensure_surface(&m);
        assert_eq!(s.surface_cache.rebuilds, 1);
        m.surface = "x*y".into();
        s.ensure_surface(&m);
        assert_eq!(s.surface_cache.rebuilds, 2);
        m.view3d.yaw += 0.2;
        s.ensure_surface(&m);
        assert_eq!(s.surface_cache.rebuilds, 3);
        m.view3d.zoom *= 1.3;
        s.ensure_surface(&m);
        assert_eq!(s.surface_cache.rebuilds, 4);
    }

    #[test]
    fn cached_cells_carry_theme_independent_shading() {
        let mut s = screen();
        let m = CalcModel::seeded();
        s.ensure_surface(&m);
        for cell in &s.surface_cache.cells {
            assert!((0.0..=1.0).contains(&cell.height_f));
            assert!(cell.shade >= 0.55 && cell.shade <= 1.0);
        }
    }

    /// The 3D default surface `sin(√(x²+y²))` is a stacked expression that used
    /// to be silently clipped in the fixed 32 px box. Its card now grows to fit
    /// it and shows it IN FULL (no clip). Mirrors `draw()`'s measure → layout
    /// sequence, then sizes the field exactly as `draw()` does.
    #[test]
    fn default_surface_card_grows_to_fit_in_full() {
        let mut tr = TextRenderer::new();
        let mut s = Graph3dScreen::default();
        let m = CalcModel::seeded(); // surface = sin(sqrt(x^2+y^2))
        let area = Rect::new(0.0, 0.0, 1000.0, 700.0);
        s.measure_card(&m, &mut tr);
        s.layout(area);
        assert!(
            s.expr_card_h > BASE_EXPR_H,
            "the 3D default surface card must grow taller than the base ({BASE_EXPR_H}), got {}",
            s.expr_card_h
        );
        assert!(
            (s.expr_rect.height() - s.expr_card_h).abs() < 1e-6,
            "the surface box must reflect the grown card height"
        );
        let edit = Rect::new(
            s.expr_rect.x0 + 40.0,
            s.expr_rect.y0 + EXPR_PAD_Y,
            s.expr_rect.x1 - 8.0,
            s.expr_rect.y1 - EXPR_PAD_Y,
        );
        // Shown in full VERTICALLY (grown to fit), not clipped.
        let content_h = s.field.content_height(&mut tr);
        assert!(
            content_h <= edit.height() + 0.5,
            "the 3D default surface must fit vertically in its grown box: content {content_h} vs field {}",
            edit.height()
        );
    }

    /// A surface taller than the generous card max grows to the max and then
    /// scrolls within the box — never clips.
    #[test]
    fn oversized_surface_card_grows_to_max_then_scrolls() {
        let mut tr = TextRenderer::new();
        let mut s = Graph3dScreen::default();
        let mut m = CalcModel::seeded();
        m.surface = "(1/x)/(1/y)".into();
        let area = Rect::new(0.0, 0.0, 1000.0, 700.0);
        s.measure_card(&m, &mut tr);
        s.layout(area);
        let expected = crate::mathfield::CARD_MAX_H + 2.0 * EXPR_PAD_Y;
        assert!(
            (s.expr_card_h - expected).abs() < 1e-6,
            "an oversized surface card must clamp to the max ({expected}), got {}",
            s.expr_card_h
        );
        let edit = Rect::new(
            s.expr_rect.x0 + 40.0,
            s.expr_rect.y0 + EXPR_PAD_Y,
            s.expr_rect.x1 - 8.0,
            s.expr_rect.y1 - EXPR_PAD_Y,
        );
        let content_h = s.field.content_height(&mut tr);
        assert!(
            content_h > edit.height() + 0.5,
            "an oversized surface must overflow its maxed box vertically (scroll, not clip): content {content_h} vs field {}",
            edit.height()
        );
        s.field.set_rect(edit, &mut tr);
        assert!(s.field.overflows(), "the maxed box must report overflow");
    }
}
