//! 2D grapher: a `y = f(x)` function list plus a pannable, zoomable canvas
//! that samples each function per pixel-column into discontinuity-aware
//! polylines, painted over the toolkit's [`FunctionPlane`] grid.

use ui::charts::{ChartPalette, FunctionPlane};
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect};
use vello::peniko::Color;

use crate::draw::{self, Align};
use crate::engine;
use crate::mathfield::MathField;
use crate::model::{CalcModel, View2d};
use crate::zoom::ZoomAnim2d;

/// Below this body width the Functions panel can't dock without eating half the
/// plot, so it collapses to a toggle + overlay drawer (matching Calc's history
/// sidebar and the OS Adwaita overlay-sidebar pattern).
const PANEL_DOCK_MIN_W: f64 = 720.0;
/// Overlay drawer width when the panel floats over the canvas.
const PANEL_OVERLAY_W: f64 = 300.0;
/// Every graph-row action gets a finger-sized semantic/hit target, while the
/// painted icon inside remains compact and unobtrusive.
const ROW_ACTION_TARGET: f64 = 44.0;
const ROW_ACTION_GAP: f64 = 4.0;
const ROW_ACTION_ICON: f64 = 22.0;

pub struct Graph2dScreen {
    pub panel: Rect,
    pub canvas: Rect,
    row_rects: Vec<Rect>,
    /// Compact visibility marks painted in the row.
    vis_rects: Vec<Rect>,
    /// Minimum 44×44 targets for visibility actions and a11y bounds.
    vis_hit_rects: Vec<Rect>,
    /// Direct delete affordance for each saved formula. Keeping this alongside
    /// the visibility dot means graph management is not hidden in Ctrl+K.
    delete_rects: Vec<Rect>,
    /// Minimum 44×44 targets for delete actions and a11y bounds.
    delete_hit_rects: Vec<Rect>,
    edit_rects: Vec<Rect>,
    add_rect: Rect,
    /// Panel is an overlay drawer (narrow) rather than docked (wide).
    overlay: bool,
    /// Whether the panel is currently shown.
    panel_open: bool,
    /// Whether the user explicitly toggled the panel this session; until they
    /// do, open/closed tracks the breakpoint so a resize feels natural.
    user_toggled: bool,
    /// Toggle pill (only present in overlay mode, to open the drawer).
    toggle_rect: Rect,
    /// Dismiss scrim behind an open overlay drawer.
    scrim_rect: Rect,
    pub hovered_row: Option<usize>,
    /// One real math editor per function row, kept in sync with `model.funcs`.
    fields: Vec<MathField>,
    /// Per-row card (outer) heights, grown to fit each formula up to a max.
    /// Measured during `draw` (where a `TextRenderer` is available) and reused
    /// by `layout` on the pointer/event path so hit-testing matches what was
    /// painted. Empty / short entries fall back to [`BASE_ROW_H`].
    card_heights: Vec<f64>,
    /// Measured width of the row prefix (`y =` / `z =`) at this frame's
    /// paint scale, so the math field starts past it at every text scale.
    /// Measured during `draw`; `layout` reuses it on the event path.
    prefix_w: f64,
    /// Measured height of the panel header (title + subtitle lines) so the
    /// first card starts below them at every text scale (64 px at 1×).
    header_h: f64,
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

fn centered_square(center: Point, side: f64) -> Rect {
    let radius = side / 2.0;
    Rect::new(
        center.x - radius,
        center.y - radius,
        center.x + radius,
        center.y + radius,
    )
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

/// Vertical padding between a card's edge and the math-field viewport inside it.
const CARD_PAD_Y: f64 = 7.0;
/// Gap between successive function cards.
const ROW_GAP: f64 = 8.0;
/// Card height for a short, single-line formula — `CARD_MIN_H` viewport plus the
/// top/bottom padding. Keeps the common case identical to the old fixed rows.
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
const PREFIX_LABEL: &str = "y =";

impl Graph2dScreen {
    /// Resolve docked-vs-overlay and the panel / canvas / scrim / toggle rects
    /// from the current width. Re-derived every layout pass so a resize reflows.
    fn resolve_panel(&mut self, area: Rect) {
        let can_dock = area.width() >= PANEL_DOCK_MIN_W;
        self.overlay = !can_dock;
        if !self.user_toggled {
            self.panel_open = can_dock;
        }

        if can_dock && self.panel_open {
            // The 300 px minimum leaves formula text enough room even after
            // reserving two adjacent 44 px graph actions.
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
                // "Hide" chip at the drawer header's right edge.
                self.toggle_rect = Rect::new(
                    self.panel.x1 - 66.0,
                    self.panel.y0 + 14.0,
                    self.panel.x1 - 16.0,
                    self.panel.y0 + 42.0,
                );
            } else {
                self.panel = Rect::ZERO;
                self.scrim_rect = Rect::ZERO;
                // Closed overlay: a toggle pill floats at the canvas top-left.
                self.toggle_rect = Rect::new(
                    area.x0 + 12.0,
                    area.y0 + 12.0,
                    area.x0 + 152.0,
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
            // Panel hidden (collapsed overlay): no rows to place.
            self.add_rect = Rect::ZERO;
            return;
        }
        let list_top = self.panel.y0 + self.header_h;
        // Cards flow top-to-bottom with per-row heights (a fraction's card is
        // taller than a plain line's), so the list reflows to accommodate them.
        let mut y = list_top;
        for i in 0..model.funcs.len() {
            let card_h = self.card_heights.get(i).copied().unwrap_or(BASE_ROW_H);
            let r = Rect::new(self.panel.x0 + 12.0, y, self.panel.x1 - 12.0, y + card_h);
            // Keep the two durable actions separate for touch, mouse, and AT.
            // The compact marks are centered inside their 44 px target boxes.
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
            // Editor area: a "y =" label sits at the left, the math field fills
            // the rest up to the visibility dot. Its height is the grown card
            // minus the padding, so the field viewport grows with the card.
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

    /// Measure every row's formula and compute its grown card height. Run at the
    /// top of `draw` (a `TextRenderer` is available there) so `layout` — which
    /// also runs on the event path without a renderer — can place variable-height
    /// cards from the cached measurements. A formula taller than the card max
    /// clamps to the max height and then scrolls within its field.
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

    /// Reconcile the per-row [`MathField`]s with `model.funcs`: build one
    /// field per row and feed it the row's current expression. Local edits are
    /// preserved because `set_source` is a no-op when the string is unchanged.
    pub fn sync_fields(&mut self, model: &CalcModel) {
        if self.fields.len() == model.funcs.len() {
            for (field, f) in self.fields.iter_mut().zip(&model.funcs) {
                field.set_source(&f.expr);
            }
        } else {
            self.fields = model
                .funcs
                .iter()
                .map(|f| MathField::new(&f.expr).with_size(20.0))
                .collect();
            // Seed the active row's caret inside a structure so a fresh frame
            // (incl. the headless screenshot) shows a real 2D caret in place.
            if let Some(field) = self.fields.get_mut(model.active_fn) {
                field.park_caret_in_structure();
            }
        }
    }

    /// The math field for the active function row, if any.
    pub fn active_field_mut(&mut self, active: usize) -> Option<&mut MathField> {
        self.fields.get_mut(active)
    }

    /// The row index whose editor rect contains `p`.
    pub fn field_at(&mut self, p: Point) -> Option<usize> {
        self.edit_rects.iter().position(|r| r.contains(p))
    }

    /// End any in-progress drag-selection in every row's field.
    pub fn release_fields(&mut self) {
        for f in &mut self.fields {
            f.on_release();
        }
    }

    /// Paint remote collaborators' carets into the active function field,
    /// snapping each scene-space cursor to the field's 2D glyph layout.
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

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
        area: Rect,
        trace: Option<Point>,
    ) {
        draw::set_text_scale(draw::paint_scale(tr, theme));
        // Measure formulas → grow cards, then place the (now variable-height)
        // rows before painting anything that depends on their rects.
        self.measure_cards(model, tr, draw::paint_scale(tr, theme));
        self.layout(area, model);

        // Advance the smooth-zoom ease one frame toward the model target. The
        // eased `view` (== model when settled) drives the grid + trace so they
        // move in lock-step with the cheaply-transformed curves.
        self.zoom.tick(model.view2d, theme.reduce_motion);
        let view = self.zoom.display();

        self.draw_canvas(scene, t, tr, theme, model, (trace, &view));

        // Collapsed overlay: nothing but a floating toggle over the plot.
        if self.panel == Rect::ZERO {
            if self.overlay {
                self.draw_toggle(scene, t, tr, theme);
            }
            return;
        }
        // Open overlay drawer: lay a dismiss scrim over the canvas first.
        if self.overlay {
            draw::fill_rect(
                scene,
                t,
                self.scrim_rect,
                draw::with_alpha(theme.tokens.colors.surfaces.scrim, 110),
            );
        }

        self.draw_panel_header(scene, t, tr, theme);
        self.draw_function_rows(scene, t, tr, theme, model);
        self.draw_add_button(scene, t, tr, theme);
        self.draw_info_readout(scene, t, tr, theme, model, trace);

        // Overlay drawer keeps a "Hide" chip in its header so it can be
        // dismissed without hunting for the scrim.
        if self.overlay {
            self.draw_toggle(scene, t, tr, theme);
        }
    }

    /// The plot canvas: background, grid, curves, the trace crosshair (if
    /// any), and the frame stroke.
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
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, self.canvas, c.neutral[0]);
        self.draw_grid(scene, t, tr, theme, view);
        self.draw_curves(scene, t, theme, model);
        if let Some(p) = trace {
            self.draw_trace(scene, t, tr, theme, model, (p, view));
        }
        draw::stroke_round(
            scene,
            t,
            self.canvas,
            0.0,
            ui::surface::edge_on(c.neutral[0]),
            1.0,
        );
    }

    /// The function panel's background + "Functions" header.
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
            "Functions",
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
            "y = f(x)  ·  click to edit",
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

    /// One row per saved function: background, colour swatch, "y =" prefix,
    /// the math editor (or its placeholder), visibility dot, and delete button.
    /// Editors are already reconciled + measured by `measure_cards`.
    fn draw_function_rows(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
    ) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        for i in 0..model.funcs.len() {
            let (Some(f), Some(&r), Some(&edit), Some(&dr), Some(&vr)) = (
                model.funcs.get(i),
                self.row_rects.get(i),
                self.edit_rects.get(i),
                self.delete_rects.get(i),
                self.vis_rects.get(i),
            ) else {
                continue;
            };
            let active = i == model.active_fn;
            if active {
                ctx.fill_round(r, 8.0, draw::mix(c.surfaces.raised, c.accent[5], 0.18));
                ctx.stroke_round(r, 8.0, c.accent[5], 1.0);
            } else if self.hovered_row == Some(i) {
                ui::surface::card(ctx.scene, t, r, 8.0, c.surfaces.raised);
            }
            // colour swatch
            let sw = Rect::new(
                r.x0 + 12.0,
                r.center().y - 7.0,
                r.x0 + 26.0,
                r.center().y + 7.0,
            );
            let col = ChartPalette::from_theme(theme).categorical(i);
            ctx.fill_round(
                sw,
                3.0,
                if f.visible {
                    col
                } else {
                    draw::with_alpha(col, 80)
                },
            );
            // "y =" prefix (upright label), then the 2D math editor.
            let txt_col = if f.visible {
                c.text_primary
            } else {
                c.text_dim
            };
            let prefix = Rect::new(r.x0 + PREFIX_X, r.y0, r.x0 + PREFIX_X + self.prefix_w, r.y1);
            draw::text_in(
                &mut ctx,
                prefix,
                "y =",
                &TextStyle::new(14.0).with_weight(TextWeight::Medium),
                txt_col,
                Align::Left,
            );
            let visible = f.visible;
            let Some(empty) = self.fields.get(i).map(MathField::is_empty) else {
                continue;
            };
            if empty && !active {
                let pr = Rect::new(edit.x0 + 4.0, edit.y0, edit.x1, edit.y1);
                draw::text_in(
                    &mut ctx,
                    pr,
                    "f(x)",
                    &TextStyle::new(14.0),
                    c.text_dim,
                    Align::Left,
                );
            } else if let Some(field) = self.fields.get_mut(i) {
                field.set_rect(edit, ctx.text);
                let fld_col = if visible { txt_col } else { c.text_dim };
                field.draw(ctx.scene, t, ctx.text, theme, fld_col, active);
            }
            // Direct deletion remains available beside every formula; Ctrl+K
            // offers the same durable DeleteGraph action to agents and search.
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

    /// The "+ Add function" button below the row list.
    fn draw_add_button(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
        ui::surface::card(ctx.scene, t, self.add_rect, 8.0, c.surfaces.raised);
        draw::text_in(
            &mut ctx,
            self.add_rect,
            "+ Add function",
            &TextStyle::new(13.0).with_weight(TextWeight::Medium),
            c.accent[6],
            Align::Center,
        );
    }

    /// The view/trace readout card below the add button: pan center, zoom,
    /// and either the traced `f(x)` value or a hint line.
    fn draw_info_readout(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
        trace: Option<Point>,
    ) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let c = &theme.tokens.colors;
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
        let view = &model.view2d;
        let l1 = Rect::new(
            info.x0 + 12.0,
            info.y0 + 8.0,
            info.x1 - 12.0,
            info.y0 + 8.0 + line_h,
        );
        draw::text_in(
            &mut ctx,
            l1,
            &format!("center  ({:.2}, {:.2})", view.cx, view.cy),
            &TextStyle::new(12.0),
            c.text_dim,
            Align::Left,
        );
        let l2 = Rect::new(info.x0 + 12.0, l1.y1, info.x1 - 12.0, l1.y1 + line_h);
        draw::text_in(
            &mut ctx,
            l2,
            &format!("zoom  {:.1} px/unit", view.scale),
            &TextStyle::new(12.0),
            c.text_dim,
            Align::Left,
        );
        let l3 = Rect::new(info.x0 + 12.0, l2.y1 + 2.0, info.x1 - 12.0, info.y1 - 6.0);
        if let Some(p) = trace {
            let wx = self.to_world(p, view).x;
            if let Some(expr) = model.active_expr()
                && let Ok(comp) = engine::compile(expr, model.deg, false)
            {
                let wy = comp.eval1(wx);
                draw::text_in(
                    &mut ctx,
                    l3,
                    &format!(
                        "f({}) = {}",
                        engine::format_number(wx),
                        engine::format_number(wy)
                    ),
                    &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
                    c.accent[6],
                    Align::Left,
                );
            }
        } else {
            draw::text_in(
                &mut ctx,
                l3,
                "drag to pan · scroll to zoom",
                &TextStyle::new(12.0),
                c.text_dim,
                Align::Left,
            );
        }
    }

    /// Draw the panel toggle: a floating "Functions" pill over the canvas when
    /// the drawer is closed, or a compact "Hide" chip in the drawer header when
    /// it's open.
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
                "ƒ  Functions",
                &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
                c.accent[6],
                Align::Center,
            );
        }
    }

    // -- overlay panel chrome ------------------------------------------------

    /// Whether `p` hit the panel toggle (open/close the Functions drawer).
    pub fn toggle_at(&self, p: Point) -> bool {
        self.toggle_rect != Rect::ZERO && self.toggle_rect.contains(p)
    }
    /// Whether `p` landed on the dismiss scrim of an open overlay drawer.
    pub fn scrim_at(&self, p: Point) -> bool {
        self.overlay && self.panel_open && self.scrim_rect.contains(p)
    }
    /// Flip the panel open/closed and remember the user's explicit choice.
    /// Open the drawer as if the user had, at any width.
    pub fn open_panel(&mut self) {
        self.panel_open = true;
        self.user_toggled = true;
    }
    pub fn toggle_panel(&mut self) {
        self.panel_open = !self.panel_open;
        self.user_toggled = true;
    }
    /// Force the overlay drawer shut (e.g. a scrim tap).
    pub fn close_panel(&mut self) {
        self.panel_open = false;
        self.user_toggled = true;
    }
    /// Whether an open overlay drawer is covering `p` (so a click there must not
    /// fall through to the canvas as a pan).
    pub fn overlay_captures(&self, p: Point) -> bool {
        self.overlay && self.panel_open && self.panel.contains(p)
    }
    /// Whether the Functions panel is currently shown (test/introspection).
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
    /// Geometry for the direct formula-delete button.
    pub fn delete_rect(&self, i: usize) -> Option<Rect> {
        self.delete_hit_rects.get(i).copied()
    }
    /// Center of function row `i`'s visibility dot, once laid out (test helper).
    #[doc(hidden)]
    pub fn vis_center(&self, i: usize) -> Option<Point> {
        self.vis_rects.get(i).map(ui::kurbo::Rect::center)
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
        // The grid derives against the canvas ground `draw_canvas` fills;
        // its tick labels match the canvas's other 11 px captions.
        self.plane(v).paint_grid(
            scene,
            t,
            tr,
            theme,
            theme.tokens.colors.neutral[0],
            draw::type_size(11.0),
        );
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
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &self.canvas,
        );
        let stroke = vello::kurbo::Stroke::new(2.0);
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

    fn draw_trace(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
        trace_at: (Point, &View2d),
    ) {
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        let (p, v) = trace_at;
        if !self.canvas.contains(p) {
            return;
        }
        let c = &theme.tokens.colors;
        let wx = self.to_world(p, v).x;
        let Some(expr) = model.active_expr() else {
            return;
        };
        let Ok(comp) = engine::compile(expr, model.deg, false) else {
            return;
        };
        let wy = comp.eval1(wx);
        let sx = self.to_screen(wx, 0.0, v).x;
        // vertical guide
        draw::line(
            ctx.scene,
            t,
            Point::new(sx, self.canvas.y0),
            Point::new(sx, self.canvas.y1),
            draw::with_alpha(c.text_dim, 120),
            1.0,
        );
        if wy.is_finite() {
            let dot = self.to_screen(wx, wy, v);
            if dot.y >= self.canvas.y0 && dot.y <= self.canvas.y1 {
                let col = ChartPalette::from_theme(theme).categorical(model.active_fn);
                let dot_r = Rect::new(dot.x - 5.0, dot.y - 5.0, dot.x + 5.0, dot.y + 5.0);
                ctx.fill_round(dot_r, 5.0, col);
                ctx.stroke_round(
                    dot_r,
                    5.0,
                    ui::tokens::Colors::hairline_on(c.neutral[0], 255),
                    1.5,
                );
            }
            // readout chip near the top
            let label = format!(
                "({}, {})",
                engine::format_number(wx),
                engine::format_number(wy)
            );
            let (label_w, _h) = ctx
                .text
                .measure_weighted(&label, 13.0, None, TextWeight::Semibold);
            let chip = Rect::new(
                sx + 8.0,
                self.canvas.y0 + 10.0,
                sx + 8.0 + f64::from(label_w) + 20.0,
                self.canvas.y0 + 36.0,
            );
            let chip = clamp_rect_x(chip, self.canvas);
            ui::surface::card(ctx.scene, t, chip, 6.0, c.surfaces.raised);
            draw::text_in(
                &mut ctx,
                chip,
                &label,
                &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
                c.text_primary,
                Align::Center,
            );
        }
    }

    // ----- hit-testing ------------------------------------------------------

    pub fn row_at(&self, p: Point) -> Option<usize> {
        self.row_rects.iter().position(|r| r.contains(p))
    }
    pub fn vis_at(&self, p: Point) -> Option<usize> {
        self.vis_hit_rects.iter().position(|r| r.contains(p))
    }
    /// The formula row whose direct delete button contains `p`.
    pub fn delete_at(&self, p: Point) -> Option<usize> {
        self.delete_hit_rects.iter().position(|r| r.contains(p))
    }
    pub fn add_at(&self, p: Point) -> bool {
        self.add_rect.contains(p)
    }
    pub fn canvas_contains(&self, p: Point) -> bool {
        // While an overlay drawer (and its scrim) is up, the canvas is not
        // directly interactive — a press there dismisses the drawer instead of
        // panning/tracing.
        if self.overlay && self.panel_open {
            return false;
        }
        self.canvas.contains(p)
    }
}

fn clamp_rect_x(r: Rect, bounds: Rect) -> Rect {
    let w = r.width();
    let mut x0 = r.x0;
    if x0 + w > bounds.x1 - 6.0 {
        x0 = bounds.x1 - 6.0 - w;
    }
    if x0 < bounds.x0 + 6.0 {
        x0 = bounds.x0 + 6.0;
    }
    Rect::new(x0, r.y0, x0 + w, r.y1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CalcModel, Mode};

    fn screen_with_canvas() -> (Graph2dScreen, Rect) {
        let mut s = Graph2dScreen::default();
        let canvas = Rect::new(360.0, 60.0, 1280.0, 800.0);
        s.canvas = canvas;
        (s, canvas)
    }

    /// The Functions panel layout is re-derived from the current width: it docks
    /// when wide, collapses to an overlay drawer (toggle, scrim, dismiss) when
    /// narrow, re-docks when widened, and keeps non-overlapping 44 px row-action
    /// targets. Fresh screen per phase.
    #[test]
    fn functions_panel_docks_wide_overlays_narrow_and_keeps_row_targets() {
        // phase wide_docks_the_functions_panel
        {
            let mut s = Graph2dScreen::default();
            let m = CalcModel::seeded();
            let area = Rect::new(0.0, 60.0, 1280.0, 800.0);
            s.layout(area, &m);
            assert!(!s.overlay, "wide window docks the panel");
            assert!(s.panel_open);
            assert!(
                s.panel.width() > 0.0 && (s.panel.x0 - area.x0).abs() < f64::EPSILON,
                "docked panel on the left"
            );
            assert!(
                s.canvas.x0 >= s.panel.x1 - 0.5,
                "canvas starts after the docked panel"
            );
        }

        // phase narrow_collapses_panel_to_overlay
        {
            let mut s = Graph2dScreen::default();
            let m = CalcModel::seeded();
            let area = Rect::new(0.0, 60.0, 560.0, 800.0);
            s.layout(area, &m);
            assert!(s.overlay, "narrow window uses the overlay drawer");
            assert!(
                !s.panel_open,
                "narrow default is collapsed so the plot keeps the width"
            );
            assert_eq!(
                s.panel,
                Rect::ZERO,
                "collapsed → no docked panel eating the canvas"
            );
            assert!(
                (s.canvas.width() - area.width()).abs() < 0.5,
                "canvas gets the full width when collapsed"
            );
            assert!(
                s.toggle_at(s.toggle_rect.center()),
                "a floating toggle must be present to open it"
            );
        }

        // phase narrow_overlay_opens_with_scrim_and_dismisses
        {
            let mut s = Graph2dScreen::default();
            let m = CalcModel::seeded();
            let area = Rect::new(0.0, 60.0, 560.0, 800.0);
            s.layout(area, &m);
            // Open via the toggle.
            s.toggle_panel();
            s.layout(area, &m);
            assert!(s.panel_open && s.overlay);
            assert!(s.panel.width() > 0.0, "open overlay draws a drawer");
            assert!(
                s.scrim_rect.width() > 0.0,
                "open overlay lays down a dismiss scrim"
            );
            // A press inside the drawer must not pan the canvas.
            assert!(!s.canvas_contains(s.panel.center()));
            // Scrim tap dismisses.
            let scrim_pt = Point::new(s.scrim_rect.x0 + 10.0, s.scrim_rect.center().y);
            assert!(s.scrim_at(scrim_pt));
            s.close_panel();
            s.layout(area, &m);
            assert!(!s.panel_open);
        }

        // phase narrow_panel_reflows_back_to_docked_when_widened
        {
            // Re-derive from the current width each pass: a fresh (un-toggled) screen
            // must dock when wide and overlay when narrow, proving no first-frame cache.
            let mut s = Graph2dScreen::default();
            let m = CalcModel::seeded();
            s.layout(Rect::new(0.0, 60.0, 560.0, 800.0), &m);
            assert!(s.overlay && !s.panel_open);
            s.layout(Rect::new(0.0, 60.0, 1280.0, 800.0), &m);
            assert!(
                !s.overlay && s.panel_open,
                "widening must re-dock the panel"
            );
        }

        // phase row_actions_have_non_overlapping_44px_targets
        {
            let mut s = Graph2dScreen::default();
            let m = CalcModel::seeded();
            // 390-wide overlay is the tightest production presentation.
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

    /// The sampled-curve cache: the first draw samples every visible function;
    /// theme or tab switches do not resample; editing a function, panning or
    /// zooming resample; hiding a function drops its curve. Fresh screen + model
    /// per phase.
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

    /// Formula cards on the real measure → layout → set-rect path: a fraction
    /// grows to show in full, an oversized formula clamps to the max and scrolls,
    /// a short formula keeps the base height, and taller rows push later rows
    /// down. Fresh screen + renderer per phase.
    #[test]
    fn formula_cards_grow_to_fit_clamp_to_max_and_reflow() {
        // phase fraction_card_grows_to_show_in_full
        // A fraction row's card grows taller than the base to show the fraction IN
        // FULL (no clip, no scroll) — mirrors `draw()`'s measure → layout → set-rect
        // sequence on the real path.
        {
            let mut tr = TextRenderer::new();
            let mut s = Graph2dScreen::default();
            let mut m = CalcModel::seeded();
            m.funcs = vec![crate::model::PlotFn {
                expr: "(x^2+1)/(x-3)".into(),
                visible: true,
            }];
            m.active_fn = 0;
            let area = Rect::new(0.0, 0.0, 1000.0, 700.0);
            s.measure_cards(&m, &mut tr, 1.0);
            s.layout(area, &m);
            assert!(
                s.card_heights[0] > BASE_ROW_H + 8.0,
                "a fraction card must grow visibly taller than the base ({BASE_ROW_H}), got {}",
                s.card_heights[0]
            );
            assert!(
                (s.row_rects[0].height() - s.card_heights[0]).abs() < 1e-6,
                "the row rect must reflect the grown card height"
            );
            // The fraction fits VERTICALLY in its grown field (no vertical scroll /
            // clip); horizontal scrolling for very wide formulas is a separate axis.
            let content_h = s.fields[0].content_height(&mut tr);
            assert!(
                content_h <= s.edit_rects[0].height() + 0.5,
                "a fraction under the card max must fit vertically in full: content {content_h} vs field {}",
                s.edit_rects[0].height()
            );
        }

        // phase oversized_formula_card_grows_to_max_then_scrolls
        // A fraction-over-fraction exceeds even the generous max: the card grows to
        // the max and its field scrolls (never clips).
        {
            let mut tr = TextRenderer::new();
            let mut s = Graph2dScreen::default();
            let mut m = CalcModel::seeded();
            m.funcs = vec![crate::model::PlotFn {
                expr: "(1/x)/(1/y)".into(),
                visible: true,
            }];
            m.active_fn = 0;
            let area = Rect::new(0.0, 0.0, 1000.0, 700.0);
            s.measure_cards(&m, &mut tr, 1.0);
            s.layout(area, &m);
            let expected = crate::mathfield::CARD_MAX_H + 2.0 * CARD_PAD_Y;
            assert!(
                (s.card_heights[0] - expected).abs() < 1e-6,
                "an oversized formula card must clamp to the max ({expected}), got {}",
                s.card_heights[0]
            );
            // The formula is taller than the (maxed) field → it scrolls vertically.
            let content_h = s.fields[0].content_height(&mut tr);
            assert!(
                content_h > s.edit_rects[0].height() + 0.5,
                "an oversized formula must overflow its maxed field vertically (scroll, not clip): content {content_h} vs field {}",
                s.edit_rects[0].height()
            );
            s.fields[0].set_rect(s.edit_rects[0], &mut tr);
            assert!(
                s.fields[0].overflows(),
                "the maxed field must report overflow"
            );
        }

        // phase short_formula_card_keeps_base_height
        // A short one-line formula keeps the base card height and does not scroll,
        // so the common case is unchanged.
        {
            let mut tr = TextRenderer::new();
            let mut s = Graph2dScreen::default();
            let mut m = CalcModel::seeded();
            m.funcs = vec![crate::model::PlotFn {
                expr: "sin(x)".into(),
                visible: true,
            }];
            m.active_fn = 0;
            s.measure_cards(&m, &mut tr, 1.0);
            s.layout(Rect::new(0.0, 0.0, 1000.0, 700.0), &m);
            assert!(
                (s.card_heights[0] - BASE_ROW_H).abs() < 1e-6,
                "a short one-line formula keeps the base card height, got {}",
                s.card_heights[0]
            );
            s.fields[0].set_rect(s.edit_rects[0], &mut tr);
            assert!(!s.fields[0].overflows(), "a short formula must not scroll");
        }

        // phase cards_reflow_to_accommodate_taller_rows
        // Cards reflow: the second card starts below the (grown) first card plus the
        // gap, so a tall formula pushes later rows down instead of overlapping them.
        {
            let mut tr = TextRenderer::new();
            let mut s = Graph2dScreen::default();
            let mut m = CalcModel::seeded();
            m.funcs = vec![
                crate::model::PlotFn {
                    expr: "(x^2+1)/(x-3)".into(),
                    visible: true,
                },
                crate::model::PlotFn {
                    expr: "sin(x)".into(),
                    visible: true,
                },
            ];
            s.measure_cards(&m, &mut tr, 1.0);
            s.layout(Rect::new(0.0, 0.0, 1000.0, 700.0), &m);
            assert!(
                s.row_rects[1].y0 >= s.row_rects[0].y1 - 0.01,
                "row 1 (y0={}) must start at/after the bottom of the taller row 0 (y1={})",
                s.row_rects[1].y0,
                s.row_rects[0].y1
            );
        }
    }
}
