//! 3D grapher: a `z = f(x, y)` surface list (mirroring the 2D function list)
//! plus a canvas that samples every visible surface on a grid, projects each
//! vertex to 2D with yaw/pitch orbit + orthographic projection, merges the
//! quads of all surfaces and depth-sorts them together (one painter's-algorithm
//! pass so surfaces occlude each other correctly), and paints each cell in its
//! function's series colour with a Lambert shade. Drag orbits, scroll zooms.

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect};
use vello::peniko::Color;

use crate::draw::{self, Align};
use crate::engine;
use crate::graph2d::series_color;
use crate::mathfield::MathField;
use crate::model::CalcModel;
use crate::zoom::ZoomAnim3d;

/// Half-width of the sampled domain in x and y.
const DOM: f64 = 7.0;
/// Grid resolution (cells per axis).
const N: usize = 40;

/// Below this body width the Surfaces panel collapses to a toggle + overlay
/// drawer instead of docking (see [`crate::graph2d`] for the matching 2D rail).
const PANEL_DOCK_MIN_W: f64 = 720.0;
/// Overlay drawer width when the panel floats over the canvas.
const PANEL_OVERLAY_W: f64 = 300.0;

/// Vertical padding between a card's edge and the math-field viewport inside it.
const CARD_PAD_Y: f64 = 7.0;
/// Gap between successive surface cards.
const ROW_GAP: f64 = 8.0;
/// Card height for a short, single-line formula.
const BASE_ROW_H: f64 = crate::mathfield::CARD_MIN_H + 2.0 * CARD_PAD_Y;

pub struct Graph3dScreen {
    pub panel: Rect,
    pub canvas: Rect,
    row_rects: Vec<Rect>,
    vis_rects: Vec<Rect>,
    edit_rects: Vec<Rect>,
    add_rect: Rect,
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
    pub hovered_row: Option<usize>,
    /// One real math editor per surface row, kept in sync with `model.surfaces`.
    fields: Vec<MathField>,
    /// Per-row card (outer) heights, grown to fit each formula up to a max.
    card_heights: Vec<f64>,
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
            panel: Rect::ZERO,
            canvas: Rect::ZERO,
            row_rects: Vec::new(),
            vis_rects: Vec::new(),
            edit_rects: Vec::new(),
            add_rect: Rect::ZERO,
            overlay: false,
            panel_open: true,
            // Shots-only: force the panel open for the headless narrow-width
            // screenshot (no effect when unset; live default is unchanged).
            user_toggled: std::env::var_os("QOS_CALC_PANEL_OPEN").is_some(),
            toggle_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            hovered_row: None,
            fields: Vec::new(),
            card_heights: Vec::new(),
            surface_cache: SurfaceCache::default(),
            zoom: ZoomAnim3d::new(crate::model::View3d::default().zoom),
        }
    }
}

/// One painted quad of a surface. `series` selects the function's theme colour;
/// `shade` is the Lambert factor. Both are theme-independent so a re-theme
/// replays the cached geometry as `scale_rgb(series_color(theme, series), shade)`.
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

#[derive(Default)]
struct SurfaceCache {
    key: Option<SurfaceKey>,
    cells: Vec<SurfaceCell>,
    /// Cached orientation tripod endpoints (origin, +x tip, +y tip).
    axes: Option<(Point, Point, Point)>,
    /// Counts actual (re)samples; lets a test prove a theme/tab switch reuses
    /// the cached surfaces instead of re-sampling + re-projecting + re-sorting.
    rebuilds: u32,
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
                self.toggle_rect = Rect::new(
                    self.panel.x1 - 66.0,
                    self.panel.y0 + 14.0,
                    self.panel.x1 - 16.0,
                    self.panel.y0 + 42.0,
                );
            } else {
                self.panel = Rect::ZERO;
                self.scrim_rect = Rect::ZERO;
                self.toggle_rect = Rect::new(
                    area.x0 + 12.0,
                    area.y0 + 12.0,
                    area.x0 + 144.0,
                    area.y0 + 44.0,
                );
            }
        }
    }

    pub fn layout(&mut self, area: Rect, model: &CalcModel) {
        self.resolve_panel(area);

        self.row_rects.clear();
        self.vis_rects.clear();
        self.edit_rects.clear();
        if self.panel == Rect::ZERO {
            self.add_rect = Rect::ZERO;
            return;
        }
        let list_top = self.panel.y0 + 64.0;
        let mut y = list_top;
        for i in 0..model.surfaces.len() {
            let card_h = self.card_heights.get(i).copied().unwrap_or(BASE_ROW_H);
            let r = Rect::new(self.panel.x0 + 12.0, y, self.panel.x1 - 12.0, y + card_h);
            let vis = Rect::new(
                r.x1 - 34.0,
                r.center().y - 11.0,
                r.x1 - 12.0,
                r.center().y + 11.0,
            );
            // "z =" label at the left, the math field fills the rest up to the dot.
            let edit = Rect::new(
                r.x0 + 60.0,
                r.y0 + CARD_PAD_Y,
                vis.x0 - 8.0,
                r.y1 - CARD_PAD_Y,
            );
            self.vis_rects.push(vis);
            self.edit_rects.push(edit);
            self.row_rects.push(r);
            y += card_h + ROW_GAP;
        }
        let add_y = y + 4.0;
        self.add_rect = Rect::new(
            self.panel.x0 + 12.0,
            add_y,
            self.panel.x1 - 12.0,
            add_y + 36.0,
        );
    }

    /// Measure every row's formula and compute its grown card height (run at the
    /// top of `draw`, where a `TextRenderer` is available).
    fn measure_cards(&mut self, model: &CalcModel, tr: &mut TextRenderer) {
        self.sync_fields(model);
        self.card_heights.clear();
        for i in 0..model.surfaces.len() {
            let content_h = if self.fields[i].is_empty() {
                crate::mathfield::CARD_MIN_H
            } else {
                self.fields[i].content_height(tr)
            };
            let field_h = crate::mathfield::card_view_height(content_h);
            self.card_heights.push(field_h + 2.0 * CARD_PAD_Y);
        }
    }

    /// Reconcile the per-row [`MathField`]s with `model.surfaces`.
    pub fn sync_fields(&mut self, model: &CalcModel) {
        if self.fields.len() == model.surfaces.len() {
            for (field, f) in self.fields.iter_mut().zip(&model.surfaces) {
                field.set_source(&f.expr);
            }
        } else {
            self.fields = model
                .surfaces
                .iter()
                .map(|f| MathField::new(&f.expr).with_size(20.0))
                .collect();
            if let Some(field) = self.fields.get_mut(model.active_surface) {
                if std::env::var_os("QOS_CALC_LONG_DEMO").is_some() {
                    field.park_caret_at_end();
                } else if std::env::var_os("QOS_CALC_SELECTION_DEMO").is_some() {
                    field.park_demo_selection();
                } else {
                    field.park_caret_in_structure();
                }
            }
        }
    }

    /// The math field for the active surface row, if any.
    pub fn active_field_mut(&mut self, active: usize) -> Option<&mut MathField> {
        self.fields.get_mut(active)
    }

    /// The row index whose editor rect contains `p`.
    pub fn field_at(&self, p: Point) -> Option<usize> {
        self.edit_rects.iter().position(|r| r.contains(p))
    }

    /// End any in-progress drag-selection in every row's field.
    pub fn release_fields(&mut self) {
        for f in &mut self.fields {
            f.on_release();
        }
    }

    /// Paint remote collaborators' carets into the active surface field.
    pub fn draw_remote_carets(
        &self,
        scene: &mut Scene,
        t: Affine,
        active: usize,
        cursors: &[(Point, Color)],
    ) {
        if let Some(f) = self.fields.get(active) {
            f.draw_remote_carets(scene, t, cursors);
        }
    }

    // -- overlay panel chrome ------------------------------------------------

    pub fn toggle_at(&self, p: Point) -> bool {
        self.toggle_rect != Rect::ZERO && self.toggle_rect.contains(p)
    }
    pub fn scrim_at(&self, p: Point) -> bool {
        self.overlay && self.panel_open && self.scrim_rect.contains(p)
    }
    pub fn add_at(&self, p: Point) -> bool {
        self.add_rect != Rect::ZERO && self.add_rect.contains(p)
    }
    pub fn vis_at(&self, p: Point) -> Option<usize> {
        self.vis_rects.iter().position(|r| r.contains(p))
    }
    pub fn row_at(&self, p: Point) -> Option<usize> {
        self.row_rects.iter().position(|r| r.contains(p))
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
    /// Whether the Surfaces panel is currently shown (test/introspection).
    pub fn panel_open(&self) -> bool {
        self.panel_open
    }
    /// Center of the panel toggle, if one is placed this frame (test helper).
    pub fn toggle_center(&self) -> Option<Point> {
        (self.toggle_rect != Rect::ZERO).then(|| self.toggle_rect.center())
    }
    /// Center of surface row `i`'s visibility dot, once laid out (test helper).
    #[doc(hidden)]
    pub fn vis_center(&self, i: usize) -> Option<Point> {
        self.vis_rects.get(i).map(ui::kurbo::Rect::center)
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
        // Measure formulas → grow cards, then lay out from them.
        self.measure_cards(model, tr);
        self.layout(area, model);
        let c = &theme.tokens.colors;

        // Advance the smooth-zoom ease one frame toward the model target.
        self.zoom.tick(model.view3d.zoom, theme.reduce_motion);

        // background + surfaces
        draw::fill_rect(scene, t, self.canvas, c.neutral[0]);
        self.draw_surfaces(scene, t, theme, model);
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
            draw::fill_rect(
                scene,
                t,
                self.scrim_rect,
                draw::with_alpha(Color::BLACK, 110),
            );
        }

        // ---- panel ---------------------------------------------------------
        let panel_bg = if self.overlay {
            c.surfaces.raised
        } else {
            c.surfaces.base
        };
        draw::fill_rect(scene, t, self.panel, panel_bg);
        draw::line(
            scene,
            t,
            Point::new(self.panel.x1, self.panel.y0),
            Point::new(self.panel.x1, self.panel.y1),
            c.border_standard,
            if self.overlay { 1.5 } else { 1.0 },
        );
        let head = Rect::new(
            self.panel.x0 + 16.0,
            self.panel.y0 + 12.0,
            self.panel.x1 - 16.0,
            self.panel.y0 + 36.0,
        );
        draw::text_in(
            scene,
            tr,
            t,
            head,
            "Surfaces",
            16.0,
            c.text_primary,
            Align::Left,
            TextWeight::Semibold,
        );
        let sub = Rect::new(
            self.panel.x0 + 16.0,
            self.panel.y0 + 36.0,
            self.panel.x1 - 16.0,
            self.panel.y0 + 56.0,
        );
        draw::text_in(
            scene,
            tr,
            t,
            sub,
            "z = f(x, y)  ·  click to edit",
            12.0,
            c.text_dim,
            Align::Left,
            TextWeight::Regular,
        );

        // Surface rows (already reconciled + measured by `measure_cards`).
        for i in 0..model.surfaces.len() {
            let f = &model.surfaces[i];
            let r = self.row_rects[i];
            let active = i == model.active_surface;
            if active {
                draw::fill_rrect(
                    scene,
                    t,
                    r,
                    8.0,
                    draw::mix(c.surfaces.raised, c.accent[5], 0.18),
                );
                draw::stroke_rrect(scene, t, r, 8.0, c.accent[5], 1.0);
            } else if self.hovered_row == Some(i) {
                draw::fill_rrect(scene, t, r, 8.0, c.surfaces.raised);
            }
            // colour swatch (this surface's series colour)
            let sw = Rect::new(
                r.x0 + 12.0,
                r.center().y - 7.0,
                r.x0 + 26.0,
                r.center().y + 7.0,
            );
            let col = series_color(theme, i);
            draw::fill_rrect(
                scene,
                t,
                sw,
                3.0,
                if f.visible {
                    col
                } else {
                    draw::with_alpha(col, 80)
                },
            );
            // "z =" prefix, then the 2D math editor.
            let txt_col = if f.visible {
                c.text_primary
            } else {
                c.text_dim
            };
            let prefix = Rect::new(r.x0 + 34.0, r.y0, r.x0 + 60.0, r.y1);
            draw::text_in(
                scene,
                tr,
                t,
                prefix,
                "z =",
                14.0,
                txt_col,
                Align::Left,
                TextWeight::Medium,
            );
            let edit = self.edit_rects[i];
            let empty = self.fields[i].is_empty();
            if empty && !active {
                let pr = Rect::new(edit.x0 + 4.0, edit.y0, edit.x1, edit.y1);
                draw::text_in(
                    scene,
                    tr,
                    t,
                    pr,
                    "f(x, y)",
                    14.0,
                    c.text_dim,
                    Align::Left,
                    TextWeight::Regular,
                );
            } else {
                self.fields[i].set_rect(edit, tr);
                let fld_col = if f.visible { txt_col } else { c.text_dim };
                self.fields[i].draw(scene, t, tr, theme, fld_col, active);
            }
            // visibility dot
            let vr = self.vis_rects[i];
            let vis_col = if f.visible { col } else { c.neutral[3] };
            draw::stroke_rrect(scene, t, vr, vr.height() / 2.0, vis_col, 2.0);
            if f.visible {
                let inner = Rect::new(vr.x0 + 5.0, vr.y0 + 5.0, vr.x1 - 5.0, vr.y1 - 5.0);
                draw::fill_rrect(scene, t, inner, inner.height() / 2.0, vis_col);
            }
        }

        // add button
        draw::fill_rrect(scene, t, self.add_rect, 8.0, c.surfaces.raised);
        draw::stroke_rrect(scene, t, self.add_rect, 8.0, c.border_standard, 1.0);
        draw::text_in(
            scene,
            tr,
            t,
            self.add_rect,
            "+ Add surface",
            13.0,
            c.accent[6],
            Align::Center,
            TextWeight::Medium,
        );

        // controls readout
        let v = &model.view3d;
        let info_y = self.add_rect.y1 + 16.0;
        let info = Rect::new(
            self.panel.x0 + 16.0,
            info_y,
            self.panel.x1 - 16.0,
            info_y + 80.0,
        );
        draw::fill_rrect(scene, t, info, 8.0, c.neutral[1]);
        let l1 = Rect::new(
            info.x0 + 12.0,
            info.y0 + 6.0,
            info.x1 - 12.0,
            info.y0 + 26.0,
        );
        draw::text_in(
            scene,
            tr,
            t,
            l1,
            &format!(
                "yaw  {:.0}°   pitch  {:.0}°",
                v.yaw.to_degrees(),
                v.pitch.to_degrees()
            ),
            12.0,
            c.text_dim,
            Align::Left,
            TextWeight::Regular,
        );
        let l2 = Rect::new(
            info.x0 + 12.0,
            info.y0 + 28.0,
            info.x1 - 12.0,
            info.y0 + 48.0,
        );
        draw::text_in(
            scene,
            tr,
            t,
            l2,
            &format!("zoom  {:.2}×   domain ±{:.0}", v.zoom, DOM),
            12.0,
            c.text_dim,
            Align::Left,
            TextWeight::Regular,
        );
        let l3 = Rect::new(
            info.x0 + 12.0,
            info.y0 + 50.0,
            info.x1 - 12.0,
            info.y1 - 6.0,
        );
        draw::text_in(
            scene,
            tr,
            t,
            l3,
            "drag to rotate · scroll to zoom",
            12.0,
            c.accent[6],
            Align::Left,
            TextWeight::Regular,
        );

        // Overlay drawer keeps a "Hide" chip in its header.
        if self.overlay {
            self.draw_toggle(scene, t, tr, theme);
        }
    }

    /// Draw the panel toggle: a "Surfaces" pill over the canvas when the drawer
    /// is closed, or a "Hide" chip in the drawer header when it's open.
    fn draw_toggle(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        if self.toggle_rect == Rect::ZERO {
            return;
        }
        let c = &theme.tokens.colors;
        let r = self.toggle_rect;
        if self.panel_open {
            draw::fill_rrect(scene, t, r, 8.0, c.neutral[2]);
            draw::text_in(
                scene,
                tr,
                t,
                r,
                "Hide",
                12.0,
                c.text_dim,
                Align::Center,
                TextWeight::Semibold,
            );
        } else {
            draw::fill_rrect(
                scene,
                t,
                r,
                8.0,
                draw::mix(c.surfaces.raised, c.accent[5], 0.20),
            );
            draw::stroke_rrect(scene, t, r, 8.0, c.accent[5], 1.0);
            draw::text_in(
                scene,
                tr,
                t,
                r,
                "Surfaces",
                13.0,
                c.accent[6],
                Align::Center,
                TextWeight::Semibold,
            );
        }
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
        let wire = draw::with_alpha(theme.tokens.colors.neutral[0], 90);
        let wire_stroke = vello::kurbo::Stroke::new(0.6);
        for cell in &self.surface_cache.cells {
            let color = scale_rgb(series_color(theme, cell.series), cell.shade);
            scene.fill(vello::peniko::Fill::NonZero, tt, color, None, &cell.path);
            scene.stroke(&wire_stroke, tt, wire, None, &cell.path);
        }
        if let Some((o, xa, ya)) = self.surface_cache.axes {
            let c = &theme.tokens.colors;
            draw::line(
                scene,
                tt,
                o,
                xa,
                draw::with_alpha(c.semantic.danger, 180),
                1.2,
            );
            draw::line(
                scene,
                tt,
                o,
                ya,
                draw::with_alpha(c.semantic.success, 180),
                1.2,
            );
        }
        scene.pop_layer();
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
    fn ensure_surfaces(&mut self, model: &CalcModel) {
        let key = SurfaceKey::build(model, self.canvas);
        if self.surface_cache.key.as_ref() == Some(&key) {
            return;
        }
        let rebuilds = self.surface_cache.rebuilds.wrapping_add(1);

        // First pass: sample each visible surface, tracking a shared z-range.
        struct Sampled {
            series: usize,
            zs: Vec<[f64; N + 1]>,
        }
        let mut sampled: Vec<Sampled> = Vec::new();
        let mut zmin = f64::INFINITY;
        let mut zmax = f64::NEG_INFINITY;
        for (series, f) in model.surfaces.iter().enumerate() {
            if !f.visible || f.expr.trim().is_empty() {
                continue;
            }
            let comp = match engine::compile(&f.expr, model.deg, true) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let mut zs = vec![[0.0f64; N + 1]; N + 1];
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
            sampled.push(Sampled { series, zs });
        }

        if sampled.is_empty() {
            self.surface_cache = SurfaceCache {
                key: Some(key),
                cells: Vec::new(),
                axes: None,
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

        let v = &model.view3d;
        let (sa, ca) = v.yaw.sin_cos();
        let (sb, cb) = v.pitch.sin_cos();
        let s = self.canvas.height().min(self.canvas.width()) * 0.40 / (DOM * 1.7) * v.zoom;
        let cx = self.canvas.center().x;
        let cy = self.canvas.center().y;

        // model coords → screen + depth (shared normalisation across surfaces)
        let project = |x: f64, y: f64, z: f64| -> (Point, f64) {
            let zn = (z - zmid) / zspan * (DOM * 0.9);
            let x1 = x * ca - y * sa;
            let y1 = x * sa + y * ca;
            let z1 = zn;
            let y2 = y1 * cb - z1 * sb;
            let z2 = y1 * sb + z1 * cb;
            (Point::new(cx + x1 * s, cy - z2 * s), y2)
        };

        struct Depth {
            depth: f64,
            cell: SurfaceCell,
        }
        let mut cells: Vec<Depth> = Vec::with_capacity(sampled.len() * N * N);
        let light = normalize3(-0.4, -0.7, 0.8);
        for surf in &sampled {
            let zs = &surf.zs;
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

                    let nx = (x1 - x0).max(1e-6);
                    let ny = (y1 - y0).max(1e-6);
                    let normal = normalize3(-(z10 - z00) * ny, -(z01 - z00) * nx, nx * ny);
                    let lambert = dot3(normal, light).abs().clamp(0.0, 1.0);
                    let shade = 0.55 + 0.45 * lambert;
                    cells.push(Depth {
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
        // painter's algorithm across ALL surfaces: far (large depth) first.
        cells.sort_by(|a, b| {
            b.depth
                .partial_cmp(&a.depth)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let (o, _) = project(0.0, 0.0, 0.0);
        let (xa, _) = project(DOM * 1.05, 0.0, 0.0);
        let (ya, _) = project(0.0, DOM * 1.05, 0.0);

        self.surface_cache = SurfaceCache {
            key: Some(key),
            cells: cells.into_iter().map(|c| c.cell).collect(),
            axes: Some((o, xa, ya)),
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
    use crate::model::{CalcModel, Mode, PlotFn};

    fn screen() -> Graph3dScreen {
        let mut s = Graph3dScreen::default();
        s.canvas = Rect::new(360.0, 60.0, 1280.0, 800.0);
        s
    }

    #[test]
    fn wide_docks_narrow_collapses_surface_panel() {
        let mut s = Graph3dScreen::default();
        let m = CalcModel::seeded();
        // Wide: docked.
        s.layout(Rect::new(0.0, 60.0, 1280.0, 800.0), &m);
        assert!(!s.overlay && s.panel_open);
        assert!(s.panel.width() > 0.0 && s.canvas.x0 >= s.panel.x1 - 0.5);
        // Narrow: collapsed overlay, canvas takes the full width.
        s.layout(Rect::new(0.0, 60.0, 560.0, 800.0), &m);
        assert!(s.overlay && !s.panel_open);
        assert_eq!(s.panel, Rect::ZERO);
        assert!((s.canvas.width() - 560.0).abs() < 0.5);
        assert!(s.toggle_at(s.toggle_rect.center()));
        // Open it → drawer + scrim; press inside must not orbit.
        s.toggle_panel();
        s.layout(Rect::new(0.0, 60.0, 560.0, 800.0), &m);
        assert!(s.panel_open && s.panel.width() > 0.0 && s.scrim_rect.width() > 0.0);
        assert!(!s.canvas_contains(s.panel.center()));
    }

    #[test]
    fn first_draw_samples_every_visible_surface() {
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

    #[test]
    fn theme_or_tab_switch_does_not_rebuild_surface() {
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

    #[test]
    fn editing_a_surface_or_orbiting_rebuilds() {
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

    #[test]
    fn hiding_a_surface_drops_its_cells() {
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

    #[test]
    fn cached_cells_carry_theme_independent_shading() {
        let mut s = screen();
        let m = CalcModel::seeded();
        s.ensure_surfaces(&m);
        for cell in &s.surface_cache.cells {
            assert!(cell.shade >= 0.55 && cell.shade <= 1.0);
        }
    }

    /// A stacked surface formula grows its card taller than the base, mirroring
    /// the 2D function rows, so the formula shows in full rather than clipping.
    #[test]
    fn fraction_surface_card_grows_to_fit() {
        let mut tr = TextRenderer::new();
        let mut s = Graph3dScreen::default();
        let mut m = CalcModel::default();
        m.surfaces = vec![PlotFn {
            expr: "(x^2+1)/(y-3)".into(),
            visible: true,
        }];
        m.active_surface = 0;
        s.measure_cards(&m, &mut tr);
        s.layout(Rect::new(0.0, 0.0, 1000.0, 700.0), &m);
        assert!(
            s.card_heights[0] > BASE_ROW_H + 8.0,
            "a fraction surface card must grow taller than the base ({BASE_ROW_H}), got {}",
            s.card_heights[0]
        );
    }

    /// A surface formula taller than even the generous card max (a
    /// fraction-over-fraction, the 3D analogue of the 2D case): the card clamps
    /// to the max and the field scrolls within it rather than clipping the
    /// formula behind the card edge. Mirrors the 2D coverage so the "grow to a
    /// max then scroll, never clip" rule is proven for surfaces too, on the real
    /// measure → layout → set-rect path.
    #[test]
    fn oversized_surface_card_grows_to_max_then_scrolls() {
        let mut tr = TextRenderer::new();
        let mut s = Graph3dScreen::default();
        let mut m = CalcModel::default();
        m.surfaces = vec![PlotFn {
            expr: "(1/x)/(1/y)".into(),
            visible: true,
        }];
        m.active_surface = 0;
        s.measure_cards(&m, &mut tr);
        s.layout(Rect::new(0.0, 0.0, 1000.0, 700.0), &m);
        let expected = crate::mathfield::CARD_MAX_H + 2.0 * CARD_PAD_Y;
        assert!(
            (s.card_heights[0] - expected).abs() < 1e-6,
            "an oversized surface card must clamp to the max ({expected}), got {}",
            s.card_heights[0]
        );
        // Taller than the maxed field → the formula scrolls (never clips).
        let content_h = s.fields[0].content_height(&mut tr);
        assert!(
            content_h > s.edit_rects[0].height() + 0.5,
            "an oversized surface must overflow its maxed field vertically (scroll, not clip): content {content_h} vs field {}",
            s.edit_rects[0].height()
        );
        s.fields[0].set_rect(s.edit_rects[0], &mut tr);
        assert!(
            s.fields[0].overflows(),
            "the maxed surface field must report overflow"
        );
    }
}
