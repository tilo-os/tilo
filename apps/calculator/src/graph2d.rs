//! 2D grapher: a `y = f(x)` function list plus a custom Cartesian canvas
//! that samples each function per pixel-column and draws axes, gridlines,
//! numeric labels and discontinuity-aware polylines straight into the
//! Vello scene.

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect};
use vello::peniko::Color;

use crate::draw::{self, Align};
use crate::engine;
use crate::mathfield::MathField;
use crate::model::{CalcModel, View2d};
use crate::zoom::ZoomAnim2d;

/// Distinct, theme-derived colour for plot series `i`.
pub fn series_color(theme: &Theme, i: usize) -> Color {
    let c = &theme.tokens.colors;
    match i % 6 {
        0 => c.accent[5],
        1 => c.semantic.success,
        2 => c.semantic.warning,
        3 => c.semantic.info,
        4 => c.semantic.danger,
        _ => c.accent[7],
    }
}

/// Below this body width the Functions panel can't dock without eating half the
/// plot, so it collapses to a toggle + overlay drawer (matching Calc's history
/// sidebar and the OS Adwaita overlay-sidebar pattern).
const PANEL_DOCK_MIN_W: f64 = 720.0;
/// Overlay drawer width when the panel floats over the canvas.
const PANEL_OVERLAY_W: f64 = 300.0;

pub struct Graph2dScreen {
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
            edit_rects: Vec::new(),
            add_rect: Rect::ZERO,
            overlay: false,
            panel_open: true,
            // Shots-only: force the panel open so the headless narrow-width
            // screenshot can show the overlay drawer. No effect when unset, so
            // live behaviour (breakpoint-driven default) is unchanged.
            user_toggled: std::env::var_os("QOS_CALC_PANEL_OPEN").is_some(),
            toggle_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            hovered_row: None,
            fields: Vec::new(),
            card_heights: Vec::new(),
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

/// Vertical padding between a card's edge and the math-field viewport inside it.
const CARD_PAD_Y: f64 = 7.0;
/// Gap between successive function cards.
const ROW_GAP: f64 = 8.0;
/// Card height for a short, single-line formula — `CARD_MIN_H` viewport plus the
/// top/bottom padding. Keeps the common case identical to the old fixed rows.
const BASE_ROW_H: f64 = crate::mathfield::CARD_MIN_H + 2.0 * CARD_PAD_Y;

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
        self.edit_rects.clear();
        if self.panel == Rect::ZERO {
            // Panel hidden (collapsed overlay): no rows to place.
            self.add_rect = Rect::ZERO;
            return;
        }
        let list_top = self.panel.y0 + 64.0;
        // Cards flow top-to-bottom with per-row heights (a fraction's card is
        // taller than a plain line's), so the list reflows to accommodate them.
        let mut y = list_top;
        for i in 0..model.funcs.len() {
            let card_h = self.card_heights.get(i).copied().unwrap_or(BASE_ROW_H);
            let r = Rect::new(self.panel.x0 + 12.0, y, self.panel.x1 - 12.0, y + card_h);
            let vis = Rect::new(
                r.x1 - 34.0,
                r.center().y - 11.0,
                r.x1 - 12.0,
                r.center().y + 11.0,
            );
            // Editor area: a "y =" label sits at the left, the math field fills
            // the rest up to the visibility dot. Its height is the grown card
            // minus the padding, so the field viewport grows with the card.
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

    /// Measure every row's formula and compute its grown card height. Run at the
    /// top of `draw` (a `TextRenderer` is available there) so `layout` — which
    /// also runs on the event path without a renderer — can place variable-height
    /// cards from the cached measurements. A formula taller than the card max
    /// clamps to the max height and then scrolls within its field.
    fn measure_cards(&mut self, model: &CalcModel, tr: &mut TextRenderer) {
        self.sync_fields(model);
        self.card_heights.clear();
        for field in &mut self.fields {
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
            // `QOS_CALC_SELECTION_DEMO` instead seeds a selection band, for the
            // drag-selection screenshot.
            if let Some(field) = self.fields.get_mut(model.active_fn) {
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

    #[allow(clippy::too_many_arguments)]
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
        // Measure formulas → grow cards, then place the (now variable-height)
        // rows before painting anything that depends on their rects.
        self.measure_cards(model, tr);
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
                draw::with_alpha(Color::BLACK, 110),
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
        draw::stroke_rrect(scene, t, self.canvas, 0.0, c.border_standard, 1.0);
    }

    /// The function panel's background + "Functions" header.
    fn draw_panel_header(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
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
            "Functions",
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
            "y = f(x)  ·  click to edit",
            12.0,
            c.text_dim,
            Align::Left,
            TextWeight::Regular,
        );
    }

    /// One row per saved function: background, colour swatch, "y =" prefix,
    /// the math editor (or its placeholder), and the visibility dot.
    /// Editors are already reconciled + measured by `measure_cards`.
    fn draw_function_rows(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        model: &CalcModel,
    ) {
        let c = &theme.tokens.colors;
        for i in 0..model.funcs.len() {
            let (Some(f), Some(&r), Some(&edit), Some(&vr)) = (
                model.funcs.get(i),
                self.row_rects.get(i),
                self.edit_rects.get(i),
                self.vis_rects.get(i),
            ) else {
                continue;
            };
            let active = i == model.active_fn;
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
            // colour swatch
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
            // "y =" prefix (upright label), then the 2D math editor.
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
                "y =",
                14.0,
                txt_col,
                Align::Left,
                TextWeight::Medium,
            );
            let visible = f.visible;
            let Some(empty) = self.fields.get(i).map(MathField::is_empty) else {
                continue;
            };
            if empty && !active {
                let pr = Rect::new(edit.x0 + 4.0, edit.y0, edit.x1, edit.y1);
                draw::text_in(
                    scene,
                    tr,
                    t,
                    pr,
                    "f(x)",
                    14.0,
                    c.text_dim,
                    Align::Left,
                    TextWeight::Regular,
                );
            } else if let Some(field) = self.fields.get_mut(i) {
                field.set_rect(edit, tr);
                let fld_col = if visible { txt_col } else { c.text_dim };
                field.draw(scene, t, tr, theme, fld_col, active);
            }
            // visibility dot
            let vis_col = if f.visible { col } else { c.neutral[3] };
            draw::stroke_rrect(scene, t, vr, vr.height() / 2.0, vis_col, 2.0);
            if f.visible {
                let inner = Rect::new(vr.x0 + 5.0, vr.y0 + 5.0, vr.x1 - 5.0, vr.y1 - 5.0);
                draw::fill_rrect(scene, t, inner, inner.height() / 2.0, vis_col);
            }
        }
    }

    /// The "+ Add function" button below the row list.
    fn draw_add_button(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        draw::fill_rrect(scene, t, self.add_rect, 8.0, c.surfaces.raised);
        draw::stroke_rrect(scene, t, self.add_rect, 8.0, c.border_standard, 1.0);
        draw::text_in(
            scene,
            tr,
            t,
            self.add_rect,
            "+ Add function",
            13.0,
            c.accent[6],
            Align::Center,
            TextWeight::Medium,
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
        let c = &theme.tokens.colors;
        let info_y = self.add_rect.y1 + 16.0;
        let info = Rect::new(
            self.panel.x0 + 16.0,
            info_y,
            self.panel.x1 - 16.0,
            info_y + 80.0,
        );
        draw::fill_rrect(scene, t, info, 8.0, c.neutral[1]);
        let view = &model.view2d;
        let l1 = Rect::new(
            info.x0 + 12.0,
            info.y0 + 8.0,
            info.x1 - 12.0,
            info.y0 + 28.0,
        );
        draw::text_in(
            scene,
            tr,
            t,
            l1,
            &format!("center  ({:.2}, {:.2})", view.cx, view.cy),
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
            &format!("zoom  {:.1} px/unit", view.scale),
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
        if let Some(p) = trace {
            let wx = self.to_world(p, view).x;
            if let Some(expr) = model.active_expr()
                && let Ok(comp) = engine::compile(expr, model.deg, false)
            {
                let wy = comp.eval1(wx);
                draw::text_in(
                    scene,
                    tr,
                    t,
                    l3,
                    &format!(
                        "f({}) = {}",
                        engine::format_number(wx),
                        engine::format_number(wy)
                    ),
                    13.0,
                    c.accent[6],
                    Align::Left,
                    TextWeight::Semibold,
                );
            }
        } else {
            draw::text_in(
                scene,
                tr,
                t,
                l3,
                "drag to pan · scroll to zoom",
                12.0,
                c.text_dim,
                Align::Left,
                TextWeight::Regular,
            );
        }
    }

    /// Draw the panel toggle: a floating "Functions" pill over the canvas when
    /// the drawer is closed, or a compact "Hide" chip in the drawer header when
    /// it's open.
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
                "ƒ  Functions",
                13.0,
                c.accent[6],
                Align::Center,
                TextWeight::Semibold,
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
        self.vis_rects.get(i).copied()
    }
    /// Center of function row `i`'s visibility dot, once laid out (test helper).
    #[doc(hidden)]
    pub fn vis_center(&self, i: usize) -> Option<Point> {
        self.vis_rects.get(i).map(ui::kurbo::Rect::center)
    }

    // ----- canvas internals -------------------------------------------------

    fn to_screen(&self, wx: f64, wy: f64, v: &View2d) -> Point {
        let cx = self.canvas.center().x;
        let cy = self.canvas.center().y;
        Point::new(cx + (wx - v.cx) * v.scale, cy - (wy - v.cy) * v.scale)
    }
    pub fn to_world(&self, p: Point, v: &View2d) -> Point {
        let cx = self.canvas.center().x;
        let cy = self.canvas.center().y;
        Point::new(v.cx + (p.x - cx) / v.scale, v.cy - (p.y - cy) / v.scale)
    }

    fn draw_grid(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        v: &View2d,
    ) {
        let c = &theme.tokens.colors;
        let step = nice_step(72.0 / v.scale);
        let minor = step / 5.0;
        let left = self.to_world(Point::new(self.canvas.x0, 0.0), v).x;
        let right = self.to_world(Point::new(self.canvas.x1, 0.0), v).x;
        let top = self.to_world(Point::new(0.0, self.canvas.y0), v).y;
        let bottom = self.to_world(Point::new(0.0, self.canvas.y1), v).y;
        let bounds = (left, right, bottom, top);

        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &self.canvas,
        );

        self.draw_grid_lines(
            scene,
            t,
            v,
            draw::with_alpha(c.neutral[2], 120),
            bounds,
            minor,
        );
        self.draw_grid_lines(scene, t, v, c.neutral[3], bounds, step);
        let origin = self.draw_grid_axes(scene, t, v, c.neutral[5]);
        self.draw_grid_labels(scene, t, tr, (v, c.text_dim), bounds, (step, origin));

        scene.pop_layer();
    }

    /// One set of evenly-spaced grid lines (minor or major, depending on
    /// `spacing`) across the world-space `bounds` (`left, right, bottom,
    /// top`).
    fn draw_grid_lines(
        &self,
        scene: &mut Scene,
        t: Affine,
        v: &View2d,
        color: Color,
        bounds: (f64, f64, f64, f64),
        spacing: f64,
    ) {
        let (left, right, bottom, top) = bounds;
        for k in line_indices(left, right, spacing) {
            let wx = k as f64 * spacing;
            let px = self.to_screen(wx, 0.0, v).x;
            draw::line(
                scene,
                t,
                Point::new(px, self.canvas.y0),
                Point::new(px, self.canvas.y1),
                color,
                1.0,
            );
        }
        for k in line_indices(bottom, top, spacing) {
            let wy = k as f64 * spacing;
            let py = self.to_screen(0.0, wy, v).y;
            draw::line(
                scene,
                t,
                Point::new(self.canvas.x0, py),
                Point::new(self.canvas.x1, py),
                color,
                1.0,
            );
        }
    }

    /// The x/y axis lines (only drawn where the origin falls inside the
    /// canvas). Returns the origin in screen space, for the label pass.
    fn draw_grid_axes(&self, scene: &mut Scene, t: Affine, v: &View2d, color: Color) -> Point {
        let origin = self.to_screen(0.0, 0.0, v);
        if origin.y >= self.canvas.y0 && origin.y <= self.canvas.y1 {
            draw::line(
                scene,
                t,
                Point::new(self.canvas.x0, origin.y),
                Point::new(self.canvas.x1, origin.y),
                color,
                1.6,
            );
        }
        if origin.x >= self.canvas.x0 && origin.x <= self.canvas.x1 {
            draw::line(
                scene,
                t,
                Point::new(origin.x, self.canvas.y0),
                Point::new(origin.x, self.canvas.y1),
                color,
                1.6,
            );
        }
        origin
    }

    /// Numeric tick labels along both axes at the major grid step.
    fn draw_grid_labels(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        view_style: (&View2d, Color),
        bounds: (f64, f64, f64, f64),
        grid_state: (f64, Point),
    ) {
        let (v, color) = view_style;
        let (left, right, bottom, top) = bounds;
        let (step, origin) = grid_state;

        let label_y = (origin.y + 4.0).clamp(self.canvas.y0 + 2.0, self.canvas.y1 - 18.0);
        for k in line_indices(left, right, step) {
            if k == 0 {
                continue;
            }
            let wx = k as f64 * step;
            let px = self.to_screen(wx, 0.0, v).x;
            let r = Rect::new(px + 3.0, label_y, px + 60.0, label_y + 16.0);
            draw::text_in(
                scene,
                tr,
                t,
                r,
                &fmt_tick(wx, step),
                11.0,
                color,
                Align::Left,
                TextWeight::Regular,
            );
        }
        let label_x = (origin.x + 6.0).clamp(self.canvas.x0 + 2.0, self.canvas.x1 - 50.0);
        for k in line_indices(bottom, top, step) {
            if k == 0 {
                continue;
            }
            let wy = k as f64 * step;
            let py = self.to_screen(0.0, wy, v).y;
            let r = Rect::new(label_x, py - 16.0, label_x + 60.0, py);
            draw::text_in(
                scene,
                tr,
                t,
                r,
                &fmt_tick(wy, step),
                11.0,
                color,
                Align::Left,
                TextWeight::Regular,
            );
        }
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
        for cp in &self.curve_cache.paths {
            let col = series_color(theme, cp.series);
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
            scene,
            t,
            Point::new(sx, self.canvas.y0),
            Point::new(sx, self.canvas.y1),
            draw::with_alpha(c.text_dim, 120),
            1.0,
        );
        if wy.is_finite() {
            let dot = self.to_screen(wx, wy, v);
            if dot.y >= self.canvas.y0 && dot.y <= self.canvas.y1 {
                let col = series_color(theme, model.active_fn);
                let dot_r = Rect::new(dot.x - 5.0, dot.y - 5.0, dot.x + 5.0, dot.y + 5.0);
                draw::fill_rrect(scene, t, dot_r, 5.0, col);
                draw::stroke_rrect(scene, t, dot_r, 5.0, Color::WHITE, 1.5);
            }
            // readout chip near the top
            let label = format!(
                "({}, {})",
                engine::format_number(wx),
                engine::format_number(wy)
            );
            let (label_w, _h) = tr.measure_weighted(&label, 13.0, None, TextWeight::Semibold);
            let chip = Rect::new(
                sx + 8.0,
                self.canvas.y0 + 10.0,
                sx + 8.0 + f64::from(label_w) + 20.0,
                self.canvas.y0 + 36.0,
            );
            let chip = clamp_rect_x(chip, self.canvas);
            draw::fill_rrect(scene, t, chip, 6.0, c.surfaces.raised);
            draw::stroke_rrect(scene, t, chip, 6.0, c.border_standard, 1.0);
            draw::text_in(
                scene,
                tr,
                t,
                chip,
                &label,
                13.0,
                c.text_primary,
                Align::Center,
                TextWeight::Semibold,
            );
        }
    }

    // ----- hit-testing ------------------------------------------------------

    pub fn row_at(&self, p: Point) -> Option<usize> {
        self.row_rects.iter().position(|r| r.contains(p))
    }
    pub fn vis_at(&self, p: Point) -> Option<usize> {
        self.vis_rects.iter().position(|r| r.contains(p))
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

/// Inclusive integer multipliers `k` such that `k*step` spans `[lo, hi]`.
fn line_indices(lo: f64, hi: f64, step: f64) -> impl Iterator<Item = i64> {
    let kmin = (lo / step).floor() as i64 - 1;
    let kmax = (hi / step).ceil() as i64 + 1;
    let span = (kmax - kmin).clamp(0, 100_000);
    (0..=span).map(move |i| kmin + i)
}

/// 1-2-5 "nice" step at least covering `raw` units.
fn nice_step(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 1.0;
    }
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    let nice = if norm < 1.5 {
        1.0
    } else if norm < 3.0 {
        2.0
    } else if norm < 7.0 {
        5.0
    } else {
        10.0
    };
    nice * mag
}

fn fmt_tick(v: f64, step: f64) -> String {
    if v.abs() < step * 1e-6 {
        return "0".to_string();
    }
    let decimals = if step >= 1.0 {
        0
    } else {
        (-step.log10().floor()) as usize
    };
    let s = format!("{v:.decimals$}");
    if decimals == 0 {
        s
    } else {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
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

    #[test]
    fn wide_docks_the_functions_panel() {
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

    #[test]
    fn narrow_collapses_panel_to_overlay() {
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

    #[test]
    fn narrow_overlay_opens_with_scrim_and_dismisses() {
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

    #[test]
    fn narrow_panel_reflows_back_to_docked_when_widened() {
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

    #[test]
    fn first_draw_samples_visible_functions() {
        let (mut s, _) = screen_with_canvas();
        let m = CalcModel::seeded();
        s.ensure_curves(&m);
        // seeded model has 4 visible functions → 4 sampled polylines.
        let visible = m.funcs.iter().filter(|f| f.visible).count();
        assert_eq!(s.curve_cache.paths.len(), visible);
        assert_eq!(s.curve_cache.rebuilds, 1);
    }

    #[test]
    fn theme_or_tab_switch_does_not_resample() {
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

    #[test]
    fn editing_a_function_resamples() {
        let (mut s, _) = screen_with_canvas();
        let mut m = CalcModel::seeded();
        s.ensure_curves(&m);
        assert_eq!(s.curve_cache.rebuilds, 1);
        m.funcs[0].expr = "x^3".into();
        s.ensure_curves(&m);
        assert_eq!(s.curve_cache.rebuilds, 2);
    }

    #[test]
    fn panning_the_view_resamples() {
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

    #[test]
    fn toggling_visibility_off_drops_the_curve() {
        let (mut s, _) = screen_with_canvas();
        let mut m = CalcModel::seeded();
        s.ensure_curves(&m);
        let before = s.curve_cache.paths.len();
        m.funcs[0].visible = false;
        s.ensure_curves(&m);
        assert_eq!(s.curve_cache.paths.len(), before - 1);
    }

    /// A fraction row's card grows taller than the base to show the fraction IN
    /// FULL (no clip, no scroll) — mirrors `draw()`'s measure → layout → set-rect
    /// sequence on the real path.
    #[test]
    fn fraction_card_grows_to_show_in_full() {
        let mut tr = TextRenderer::new();
        let mut s = Graph2dScreen::default();
        let mut m = CalcModel::seeded();
        m.funcs = vec![crate::model::PlotFn {
            expr: "(x^2+1)/(x-3)".into(),
            visible: true,
        }];
        m.active_fn = 0;
        let area = Rect::new(0.0, 0.0, 1000.0, 700.0);
        s.measure_cards(&m, &mut tr);
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

    /// A fraction-over-fraction exceeds even the generous max: the card grows to
    /// the max and its field scrolls (never clips).
    #[test]
    fn oversized_formula_card_grows_to_max_then_scrolls() {
        let mut tr = TextRenderer::new();
        let mut s = Graph2dScreen::default();
        let mut m = CalcModel::seeded();
        m.funcs = vec![crate::model::PlotFn {
            expr: "(1/x)/(1/y)".into(),
            visible: true,
        }];
        m.active_fn = 0;
        let area = Rect::new(0.0, 0.0, 1000.0, 700.0);
        s.measure_cards(&m, &mut tr);
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

    /// A short one-line formula keeps the base card height and does not scroll,
    /// so the common case is unchanged.
    #[test]
    fn short_formula_card_keeps_base_height() {
        let mut tr = TextRenderer::new();
        let mut s = Graph2dScreen::default();
        let mut m = CalcModel::seeded();
        m.funcs = vec![crate::model::PlotFn {
            expr: "sin(x)".into(),
            visible: true,
        }];
        m.active_fn = 0;
        s.measure_cards(&m, &mut tr);
        s.layout(Rect::new(0.0, 0.0, 1000.0, 700.0), &m);
        assert!(
            (s.card_heights[0] - BASE_ROW_H).abs() < 1e-6,
            "a short one-line formula keeps the base card height, got {}",
            s.card_heights[0]
        );
        s.fields[0].set_rect(s.edit_rects[0], &mut tr);
        assert!(!s.fields[0].overflows(), "a short formula must not scroll");
    }

    /// Cards reflow: the second card starts below the (grown) first card plus the
    /// gap, so a tall formula pushes later rows down instead of overlapping them.
    #[test]
    fn cards_reflow_to_accommodate_taller_rows() {
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
        s.measure_cards(&m, &mut tr);
        s.layout(Rect::new(0.0, 0.0, 1000.0, 700.0), &m);
        assert!(
            s.row_rects[1].y0 >= s.row_rects[0].y1 - 0.01,
            "row 1 (y0={}) must start at/after the bottom of the taller row 0 (y1={})",
            s.row_rects[1].y0,
            s.row_rects[0].y1
        );
    }
}
