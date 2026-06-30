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

pub struct Graph3dScreen {
    pub panel: Rect,
    pub canvas: Rect,
    expr_rect: Rect,
    /// The 2D math editor for the `z = f(x, y)` surface expression.
    field: MathField,
}

impl Default for Graph3dScreen {
    fn default() -> Self {
        Self {
            panel: Rect::ZERO,
            canvas: Rect::ZERO,
            expr_rect: Rect::ZERO,
            field: MathField::new("").with_size(20.0),
        }
    }
}

impl Graph3dScreen {
    pub fn layout(&mut self, area: Rect) {
        let panel_w = (area.width() * 0.27).clamp(260.0, 360.0);
        self.panel = Rect::new(area.x0, area.y0, area.x0 + panel_w, area.y1);
        self.canvas = Rect::new(self.panel.x1, area.y0, area.x1, area.y1);
        self.expr_rect = Rect::new(self.panel.x0 + 16.0, self.panel.y0 + 60.0, self.panel.x1 - 16.0, self.panel.y0 + 96.0);
    }

    /// Feed the surface expression into the editor (no-op when unchanged).
    pub fn sync_field(&mut self, model: &CalcModel) {
        self.field.set_source(&model.surface);
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
        self.layout(area);
        let c = &theme.tokens.colors;

        // background
        draw::fill_rect(scene, t, self.canvas, c.neutral[0]);

        let (zmin, zmax) = self.draw_surface(scene, t, theme, model);

        // ---- panel ---------------------------------------------------------
        draw::fill_rect(scene, t, self.panel, c.surfaces.base);
        draw::line(scene, t, Point::new(self.panel.x1, self.panel.y0), Point::new(self.panel.x1, self.panel.y1), c.border_standard, 1.0);
        let head = Rect::new(self.panel.x0 + 16.0, self.panel.y0 + 12.0, self.panel.x1 - 16.0, self.panel.y0 + 36.0);
        draw::text_in(scene, tr, t, head, "Surface", 16.0, c.text_primary, Align::Left, TextWeight::Semibold);
        let sub = Rect::new(self.panel.x0 + 16.0, self.panel.y0 + 34.0, self.panel.x1 - 16.0, self.panel.y0 + 54.0);
        draw::text_in(scene, tr, t, sub, "z = f(x, y)", 12.0, c.text_dim, Align::Left, TextWeight::Regular);

        // expression box (always the edit target in 3D mode): a real 2D math
        // editor for z = f(x, y).
        self.sync_field(model);
        draw::fill_rrect(scene, t, self.expr_rect, 8.0, c.neutral[1]);
        draw::stroke_rrect(scene, t, self.expr_rect, 8.0, c.accent[5], 1.0);
        let valid = engine::compile(&model.surface, model.deg, true).is_ok();
        let col = if valid { c.text_primary } else { c.semantic.danger };
        let prefix = Rect::new(self.expr_rect.x0 + 12.0, self.expr_rect.y0, self.expr_rect.x0 + 40.0, self.expr_rect.y1);
        draw::text_in(scene, tr, t, prefix, "z =", 14.0, col, Align::Left, TextWeight::Medium);
        let edit = Rect::new(self.expr_rect.x0 + 40.0, self.expr_rect.y0 + 2.0, self.expr_rect.x1 - 8.0, self.expr_rect.y1 - 2.0);
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

        draw::stroke_rrect(scene, t, self.canvas, 0.0, c.border_standard, 1.0);
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
    fn draw_surface(&self, scene: &mut Scene, t: Affine, theme: &Theme, model: &CalcModel) -> (f64, f64) {
        let comp = match engine::compile(&model.surface, model.deg, true) {
            Ok(c) => c,
            Err(_) => return (0.0, 0.0),
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
            path: BezPath,
            color: Color,
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

                // height-based base colour
                let zavg = 0.25 * (z00 + z10 + z11 + z01);
                let f = ((zavg - zmin) / zspan).clamp(0.0, 1.0);
                let base = height_color(theme, f);
                // lambert shade from the cell normal (model space)
                let nx = (x1 - x0).max(1e-6);
                let ny = (y1 - y0).max(1e-6);
                let normal = normalize3(-(z10 - z00) * ny, -(z01 - z00) * nx, nx * ny);
                let lambert = (dot3(normal, light)).abs().clamp(0.0, 1.0);
                let shade = 0.55 + 0.45 * lambert;
                cells.push(Cell {
                    depth: 0.25 * (d00 + d10 + d11 + d01),
                    path,
                    color: scale_rgb(base, shade),
                });
            }
        }
        // painter's algorithm: far (large depth) first
        cells.sort_by(|a, b| b.depth.partial_cmp(&a.depth).unwrap_or(std::cmp::Ordering::Equal));

        scene.push_layer(vello::peniko::Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &self.canvas);
        let wire = draw::with_alpha(theme.tokens.colors.neutral[0], 90);
        let wire_stroke = vello::kurbo::Stroke::new(0.6);
        for cell in &cells {
            scene.fill(vello::peniko::Fill::NonZero, t, cell.color, None, &cell.path);
            scene.stroke(&wire_stroke, t, wire, None, &cell.path);
        }
        // orientation tripod at the model origin
        self.draw_axes(scene, t, theme, &project);
        scene.pop_layer();

        (zmin, zmax)
    }

    fn draw_axes(&self, scene: &mut Scene, t: Affine, theme: &Theme, project: &impl Fn(f64, f64, f64) -> (Point, f64)) {
        let c = &theme.tokens.colors;
        let (o, _) = project(0.0, 0.0, 0.0);
        let (xa, _) = project(DOM * 1.05, 0.0, 0.0);
        let (ya, _) = project(0.0, DOM * 1.05, 0.0);
        draw::line(scene, t, o, xa, draw::with_alpha(c.semantic.danger, 180), 1.2);
        draw::line(scene, t, o, ya, draw::with_alpha(c.semantic.success, 180), 1.2);
    }

    pub fn canvas_contains(&self, p: Point) -> bool {
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
