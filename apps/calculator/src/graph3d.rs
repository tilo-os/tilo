//! 3D grapher: a `z = f(x, y)` surface list (mirroring the 2D function list)
//! plus a canvas that samples every visible surface on a grid, projects each
//! vertex to 2D with yaw/pitch orbit + orthographic projection, merges the
//! quads of all surfaces and depth-sorts them together (one painter's-algorithm
//! pass so surfaces occlude each other correctly), and paints each cell in its
//! function's series colour with a Lambert shade. Drag orbits, scroll zooms.

use ui::ColorCompat;
use ui::charts::ChartPalette;
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect};
use vello::peniko::Color;

use crate::draw::{self, Align};
use crate::engine;
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
/// Minimum independent hit/semantic target for the compact graph-row icons.
const ROW_ACTION_TARGET: f64 = 44.0;
const ROW_ACTION_GAP: f64 = 4.0;
const ROW_ACTION_ICON: f64 = 22.0;

/// Vertical padding between a card's edge and the math-field viewport inside it.
const CARD_PAD_Y: f64 = 7.0;
/// Gap between successive surface cards.
const ROW_GAP: f64 = 8.0;
/// Card height for a short, single-line formula.
const BASE_ROW_H: f64 = crate::mathfield::CARD_MIN_H + 2.0 * CARD_PAD_Y;
/// Left edge of the row prefix label inside a card (past the colour swatch).
const PREFIX_X: f64 = 34.0;
/// Gap between the measured prefix and the math field.
const PREFIX_GAP: f64 = 4.0;
/// Prefix slot before the first frame has measured it (the 1× design).
const PREFIX_W_1X: f64 = 22.0;
/// Panel header height before the first frame has measured it (the 1× design:
/// 12 top pad + 24 title line + 20 subtitle line + 8 gap).
const HEADER_H_1X: f64 = 64.0;
/// The upright prefix every row paints ahead of its math field.
const PREFIX_LABEL: &str = "z =";

pub struct Graph3dScreen {
    pub panel: Rect,
    pub canvas: Rect,
    row_rects: Vec<Rect>,
    vis_rects: Vec<Rect>,
    vis_hit_rects: Vec<Rect>,
    delete_rects: Vec<Rect>,
    delete_hit_rects: Vec<Rect>,
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
    /// Measured width of the row prefix (`y =` / `z =`) at this frame's
    /// paint scale, so the math field starts past it at every text scale.
    /// Measured during `draw`; `layout` reuses it on the event path.
    prefix_w: f64,
    /// Measured height of the panel header (title + subtitle lines) so the
    /// first card starts below them at every text scale (64 px at 1×).
    header_h: f64,
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
            vis_hit_rects: Vec::new(),
            delete_rects: Vec::new(),
            delete_hit_rects: Vec::new(),
            edit_rects: Vec::new(),
            add_rect: Rect::ZERO,
            overlay: false,
            panel_open: true,
            user_toggled: false,
            toggle_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            hovered_row: None,
            fields: Vec::new(),
            card_heights: Vec::new(),
            prefix_w: PREFIX_W_1X,
            header_h: HEADER_H_1X,
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

fn centered_square(center: Point, side: f64) -> Rect {
    let radius = side / 2.0;
    Rect::new(
        center.x - radius,
        center.y - radius,
        center.x + radius,
        center.y + radius,
    )
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
            let panel_w = (area.width() * 0.27).clamp(300.0, 360.0);
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
        self.vis_hit_rects.clear();
        self.delete_rects.clear();
        self.delete_hit_rects.clear();
        self.edit_rects.clear();
        if self.panel == Rect::ZERO {
            self.add_rect = Rect::ZERO;
            return;
        }
        let list_top = self.panel.y0 + self.header_h;
        let mut y = list_top;
        for i in 0..model.surfaces.len() {
            let card_h = self.card_heights.get(i).copied().unwrap_or(BASE_ROW_H);
            let r = Rect::new(self.panel.x0 + 12.0, y, self.panel.x1 - 12.0, y + card_h);
            let vis_hit = Rect::new(
                r.x1 - ROW_ACTION_TARGET,
                r.center().y - ROW_ACTION_TARGET / 2.0,
                r.x1,
                r.center().y + ROW_ACTION_TARGET / 2.0,
            );
            let delete_hit = Rect::new(
                vis_hit.x0 - ROW_ACTION_GAP - ROW_ACTION_TARGET,
                r.center().y - ROW_ACTION_TARGET / 2.0,
                vis_hit.x0 - ROW_ACTION_GAP,
                r.center().y + ROW_ACTION_TARGET / 2.0,
            );
            let vis = centered_square(vis_hit.center(), ROW_ACTION_ICON);
            let delete = centered_square(delete_hit.center(), ROW_ACTION_ICON);
            // "z =" label at the left, the math field fills the rest up to the dot.
            let edit = Rect::new(
                r.x0 + PREFIX_X + self.prefix_w + PREFIX_GAP,
                r.y0 + CARD_PAD_Y,
                delete_hit.x0 - 8.0,
                r.y1 - CARD_PAD_Y,
            );
            self.vis_rects.push(vis);
            self.vis_hit_rects.push(vis_hit);
            self.delete_rects.push(delete);
            self.delete_hit_rects.push(delete_hit);
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
    fn measure_cards(&mut self, model: &CalcModel, tr: &mut TextRenderer, text_scale: f32) {
        self.sync_fields(model);
        self.card_heights.clear();
        // The prefix slot and the header band follow the labels as this frame
        // paints them, so the field never starts under `y =` and the first
        // card never starts under the subtitle at any text scale.
        self.prefix_w =
            draw::measure_w(tr, PREFIX_LABEL, 14.0, TextWeight::Medium).max(PREFIX_W_1X);
        self.header_h = Self::header_band(tr).2;
        for field in &mut self.fields {
            field.set_text_scale(text_scale);
            let content_h = if field.is_empty() {
                crate::mathfield::CARD_MIN_H
            } else {
                field.content_height(tr)
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
                field.park_caret_in_structure();
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
        self.vis_hit_rects.iter().position(|r| r.contains(p))
    }
    pub fn delete_at(&self, p: Point) -> Option<usize> {
        self.delete_hit_rects.iter().position(|r| r.contains(p))
    }
    pub fn row_at(&self, p: Point) -> Option<usize> {
        self.row_rects.iter().position(|r| r.contains(p))
    }
    /// Open the drawer as if the user had, at any width.
    pub fn open_panel(&mut self) {
        self.panel_open = true;
        self.user_toggled = true;
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
    /// Geometry exported for the calculator's retained accessibility tree.
    pub fn toggle_rect(&self) -> Option<Rect> {
        (self.toggle_rect != Rect::ZERO).then_some(self.toggle_rect)
    }
    pub fn add_rect(&self) -> Option<Rect> {
        (self.add_rect != Rect::ZERO).then_some(self.add_rect)
    }
    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        self.row_rects.get(i).copied()
    }
    pub fn field_rect(&self, i: usize) -> Option<Rect> {
        self.edit_rects.get(i).copied()
    }
    pub fn visibility_rect(&self, i: usize) -> Option<Rect> {
        self.vis_hit_rects.get(i).copied()
    }
    pub fn delete_rect(&self, i: usize) -> Option<Rect> {
        self.delete_hit_rects.get(i).copied()
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
        draw::set_text_scale(draw::paint_scale(tr, theme));
        // Measure formulas → grow cards, then lay out from them.
        self.measure_cards(model, tr, draw::paint_scale(tr, theme));
        self.layout(area, model);
        let c = &theme.tokens.colors;

        // Advance the smooth-zoom ease one frame toward the model target.
        self.zoom.tick(model.view3d.zoom, theme.reduce_motion);

        // background + surfaces
        draw::fill_rect(scene, t, self.canvas, c.neutral[0]);
        self.draw_surfaces(scene, t, theme, model);
        draw::stroke_round(
            scene,
            t,
            self.canvas,
            0.0,
            ui::surface::edge_on(c.neutral[0]),
            1.0,
        );

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
                draw::with_alpha(theme.tokens.colors.surfaces.scrim, 110),
            );
        }

        self.draw_panel_header(scene, t, tr, theme);
        self.draw_surface_rows(scene, t, tr, theme, model);
        self.draw_add_button(scene, t, tr, theme);
        self.draw_info_readout(scene, t, tr, theme, model);

        // Overlay drawer keeps a "Hide" chip in its header.
        if self.overlay {
            self.draw_toggle(scene, t, tr, theme);
        }
    }

    /// The surface panel's background + "Surfaces" header.
    fn draw_panel_header(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        let panel_bg = if self.overlay {
            c.surfaces.raised
        } else {
            c.surfaces.base
        };
        ctx.fill_rect(self.panel, panel_bg);
        draw::line(
            ctx.scene,
            t,
            Point::new(self.panel.x1, self.panel.y0),
            Point::new(self.panel.x1, self.panel.y1),
            ui::surface::edge_on(panel_bg),
            1.0,
        );
        // Title and subtitle stack on their measured line boxes (24 + 20 px at
        // 1×); `layout` starts the first card below the same band.
        let (head_h, sub_h, _) = Self::header_band(ctx.text);
        let head = Rect::new(
            self.panel.x0 + 16.0,
            self.panel.y0 + 12.0,
            self.panel.x1 - 16.0,
            self.panel.y0 + 12.0 + head_h,
        );
        draw::text_in(
            &mut ctx,
            head,
            "Surfaces",
            &TextStyle::new(16.0).with_weight(TextWeight::Semibold),
            c.text_primary,
            Align::Left,
        );
        let sub = Rect::new(
            self.panel.x0 + 16.0,
            head.y1,
            self.panel.x1 - 16.0,
            head.y1 + sub_h,
        );
        draw::text_in(
            &mut ctx,
            sub,
            "z = f(x, y)  ·  click to edit",
            &TextStyle::new(12.0),
            c.text_dim,
            Align::Left,
        );
    }

    /// `(title band, subtitle band, whole header)` heights from the measured
    /// line boxes of the 16 px title and 12 px subtitle — 24 / 20 / 64 px at
    /// 1× — so the header and the card list agree at every text scale.
    fn header_band(tr: &mut TextRenderer) -> (f64, f64, f64) {
        let head_h = draw::line_box(tr, 16.0) + 4.0;
        let sub_h = draw::line_box(tr, 12.0) + 5.0;
        (head_h, sub_h, 12.0 + head_h + sub_h + 8.0)
    }

    /// One row per saved surface: background, colour swatch, "z =" prefix,
    /// the math editor (or its placeholder), visibility dot, and delete button.
    /// Rows are already reconciled + measured by `measure_cards`.
    fn draw_surface_rows(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
    ) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        let palette = ChartPalette::from_theme(theme);
        for i in 0..model.surfaces.len() {
            let (Some(f), Some(&r), Some(&edit), Some(&dr), Some(&vr)) = (
                model.surfaces.get(i),
                self.row_rects.get(i),
                self.edit_rects.get(i),
                self.delete_rects.get(i),
                self.vis_rects.get(i),
            ) else {
                continue;
            };
            let active = i == model.active_surface;
            if active {
                ctx.fill_round(r, 8.0, draw::mix(c.surfaces.raised, c.accent[5], 0.18));
                ctx.stroke_round(r, 8.0, c.accent[5], 1.0);
            } else if self.hovered_row == Some(i) {
                ui::surface::card(ctx.scene, t, r, 8.0, c.surfaces.raised);
            }
            // colour swatch (this surface's series colour)
            let sw = Rect::new(
                r.x0 + 12.0,
                r.center().y - 7.0,
                r.x0 + 26.0,
                r.center().y + 7.0,
            );
            let col = palette.categorical(i);
            ctx.fill_round(
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
            let prefix = Rect::new(r.x0 + PREFIX_X, r.y0, r.x0 + PREFIX_X + self.prefix_w, r.y1);
            draw::text_in(
                &mut ctx,
                prefix,
                "z =",
                &TextStyle::new(14.0).with_weight(TextWeight::Medium),
                txt_col,
                Align::Left,
            );
            let Some(empty) = self.fields.get(i).map(MathField::is_empty) else {
                continue;
            };
            if empty && !active {
                let pr = Rect::new(edit.x0 + 4.0, edit.y0, edit.x1, edit.y1);
                draw::text_in(
                    &mut ctx,
                    pr,
                    "f(x, y)",
                    &TextStyle::new(14.0),
                    c.text_dim,
                    Align::Left,
                );
            } else if let Some(field) = self.fields.get_mut(i) {
                field.set_rect(edit, ctx.text);
                let fld_col = if f.visible { txt_col } else { c.text_dim };
                field.draw(ctx.scene, t, ctx.text, theme, fld_col, active);
            }
            ui::surface::card(ctx.scene, t, dr, 5.0, c.neutral[2]);
            draw::text_in(
                &mut ctx,
                dr,
                "×",
                &TextStyle::new(16.0).with_weight(TextWeight::Semibold),
                c.text_dim,
                Align::Center,
            );
            // visibility dot
            let vis_col = if f.visible { col } else { c.neutral[3] };
            ctx.stroke_round(vr, vr.height() / 2.0, vis_col, 2.0);
            if f.visible {
                let inner = Rect::new(vr.x0 + 5.0, vr.y0 + 5.0, vr.x1 - 5.0, vr.y1 - 5.0);
                ctx.fill_round(inner, inner.height() / 2.0, vis_col);
            }
        }
    }

    /// The "+ Add surface" button below the row list.
    fn draw_add_button(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        ui::surface::card(ctx.scene, t, self.add_rect, 8.0, c.surfaces.raised);
        draw::text_in(
            &mut ctx,
            self.add_rect,
            "+ Add surface",
            &TextStyle::new(13.0).with_weight(TextWeight::Medium),
            c.accent[6],
            Align::Center,
        );
    }

    /// The orbit/zoom controls readout below the add button.
    fn draw_info_readout(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
    ) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        let v = &model.view3d;
        // Three 12 px lines stacked on their measured line boxes (20 px each
        // at 1×, an 80 px card), so the read-out never overprints itself.
        let line_h = draw::line_box(ctx.text, 12.0) + 5.0;
        let info_y = self.add_rect.y1 + 16.0;
        let info = Rect::new(
            self.panel.x0 + 16.0,
            info_y,
            self.panel.x1 - 16.0,
            info_y + 3.0 * line_h + 20.0,
        );
        ui::surface::card(ctx.scene, t, info, 8.0, c.neutral[1]);
        let l1 = Rect::new(
            info.x0 + 12.0,
            info.y0 + 6.0,
            info.x1 - 12.0,
            info.y0 + 6.0 + line_h,
        );
        draw::text_in(
            &mut ctx,
            l1,
            &format!(
                "yaw  {:.0}°   pitch  {:.0}°",
                v.yaw.to_degrees(),
                v.pitch.to_degrees()
            ),
            &TextStyle::new(12.0),
            c.text_dim,
            Align::Left,
        );
        let l2 = Rect::new(
            info.x0 + 12.0,
            l1.y1 + 2.0,
            info.x1 - 12.0,
            l1.y1 + 2.0 + line_h,
        );
        draw::text_in(
            &mut ctx,
            l2,
            &format!("zoom  {:.2}×   domain ±{:.0}", v.zoom, DOM),
            &TextStyle::new(12.0),
            c.text_dim,
            Align::Left,
        );
        let l3 = Rect::new(info.x0 + 12.0, l2.y1 + 2.0, info.x1 - 12.0, info.y1 - 6.0);
        draw::text_in(
            &mut ctx,
            l3,
            "drag to rotate · scroll to zoom",
            &TextStyle::new(12.0),
            c.accent[6],
            Align::Left,
        );
    }

    /// Draw the panel toggle: a "Surfaces" pill over the canvas when the drawer
    /// is closed, or a "Hide" chip in the drawer header when it's open.
    fn draw_toggle(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        if self.toggle_rect == Rect::ZERO {
            return;
        }
        let c = &theme.tokens.colors;
        let r = self.toggle_rect;
        if self.panel_open {
            ui::surface::card(ctx.scene, t, r, 8.0, c.neutral[2]);
            draw::text_in(
                &mut ctx,
                r,
                "Hide",
                &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
                c.text_dim,
                Align::Center,
            );
        } else {
            ctx.fill_round(r, 8.0, draw::mix(c.surfaces.raised, c.accent[5], 0.20));
            ctx.stroke_round(r, 8.0, c.accent[5], 1.0);
            draw::text_in(
                &mut ctx,
                r,
                "Surfaces",
                &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
                c.accent[6],
                Align::Center,
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
        let palette = ChartPalette::from_theme(theme);
        for cell in &self.surface_cache.cells {
            let color = scale_rgb(palette.categorical(cell.series), cell.shade);
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

        let (cells, axes) = self.project_and_rank_cells(&sampled, (zmid, zspan), model);
        self.surface_cache = SurfaceCache {
            key: Some(key),
            cells,
            axes: Some(axes),
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
    /// for the painter's algorithm. Returns the ranked cells and the
    /// (origin, +x tip, +y tip) axis tripod.
    fn project_and_rank_cells(
        &self,
        sampled: &[SampledSurface],
        z_normalize: (f64, f64),
        model: &CalcModel,
    ) -> (Vec<SurfaceCell>, (Point, Point, Point)) {
        let (zmid, zspan) = z_normalize;
        let v = &model.view3d;
        let (sin_yaw, cos_yaw) = v.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = v.pitch.sin_cos();
        let s = self.canvas.height().min(self.canvas.width()) * 0.40 / (DOM * 1.7) * v.zoom;
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

        let (o, _) = project(0.0, 0.0, 0.0);
        let (xa, _) = project(DOM * 1.05, 0.0, 0.0);
        let (ya, _) = project(0.0, DOM * 1.05, 0.0);

        (cells.into_iter().map(|c| c.cell).collect(), (o, xa, ya))
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
        Graph3dScreen {
            canvas: Rect::new(360.0, 60.0, 1280.0, 800.0),
            ..Graph3dScreen::default()
        }
    }

    /// The Surfaces panel docks when wide, collapses to an overlay drawer (toggle,
    /// scrim, no orbit under the drawer) when narrow, and keeps non-overlapping
    /// 44 px row-action targets. Fresh screen per phase.
    #[test]
    fn surface_panel_docks_wide_overlays_narrow_and_keeps_row_targets() {
        // phase wide_docks_narrow_collapses_surface_panel
        {
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

        // phase row_actions_have_non_overlapping_44px_targets
        {
            let mut s = Graph3dScreen::default();
            let m = CalcModel::seeded();
            let area = Rect::new(0.0, 60.0, 390.0, 844.0);
            s.layout(area, &m);
            s.toggle_panel();
            s.layout(area, &m);
            let delete = s.delete_rect(0).expect("delete target");
            let visible = s.visibility_rect(0).expect("visibility target");
            assert!(delete.width() >= 44.0 && delete.height() >= 44.0);
            assert!(visible.width() >= 44.0 && visible.height() >= 44.0);
            assert!(delete.x1 <= visible.x0, "row actions must not overlap");
            assert_eq!(s.delete_at(delete.center()), Some(0));
            assert_eq!(s.vis_at(visible.center()), Some(0));
            assert_eq!(
                s.delete_at(s.delete_rect(1).expect("second delete target").center()),
                Some(1),
                "a delete hit must not target the adjacent row"
            );
            assert!(
                s.edit_rects[0].x1 <= delete.x0,
                "math editing must not steal the delete target"
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

    /// Surface formula cards on the real measure → layout → set-rect path grow to
    /// fit a stacked formula and clamp to the max (then scroll, never clip) for an
    /// oversized one. Fresh screen + renderer per phase.
    #[test]
    fn surface_cards_grow_to_fit_then_clamp_to_max_and_scroll() {
        // phase fraction_surface_card_grows_to_fit
        // A stacked surface formula grows its card taller than the base, mirroring
        // the 2D function rows, so the formula shows in full rather than clipping.
        {
            let mut tr = TextRenderer::new();
            let mut s = Graph3dScreen::default();
            let m = CalcModel {
                surfaces: vec![PlotFn {
                    expr: "(x^2+1)/(y-3)".into(),
                    visible: true,
                }],
                active_surface: 0,
                ..CalcModel::default()
            };
            s.measure_cards(&m, &mut tr, 1.0);
            s.layout(Rect::new(0.0, 0.0, 1000.0, 700.0), &m);
            assert!(
                s.card_heights[0] > BASE_ROW_H + 8.0,
                "a fraction surface card must grow taller than the base ({BASE_ROW_H}), got {}",
                s.card_heights[0]
            );
        }

        // phase oversized_surface_card_grows_to_max_then_scrolls
        // A surface formula taller than even the generous card max (a
        // fraction-over-fraction, the 3D analogue of the 2D case): the card clamps
        // to the max and the field scrolls within it rather than clipping the
        // formula behind the card edge. Mirrors the 2D coverage so the "grow to a
        // max then scroll, never clip" rule is proven for surfaces too, on the real
        // measure → layout → set-rect path.
        {
            let mut tr = TextRenderer::new();
            let mut s = Graph3dScreen::default();
            let m = CalcModel {
                surfaces: vec![PlotFn {
                    expr: "(1/x)/(1/y)".into(),
                    visible: true,
                }],
                active_surface: 0,
                ..CalcModel::default()
            };
            s.measure_cards(&m, &mut tr, 1.0);
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
}
