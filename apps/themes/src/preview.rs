//! The live preview pane.
//!
//! This is the one widget that legitimately *constructs* a
//! [`ui::theme::Theme`] (from the selected [`ThemeSpec`]) and renders a
//! representative slice of the Tilo widget language under it — a window
//! card with a title bar, typography, buttons in every variant, a toggle
//! / checkbox / slider, a selectable list, semantic badges and two charts.
//! Everything pulls its colour, radius and spacing from the constructed
//! theme, so changing any token in the customizer is reflected here
//! instantly. It is fully self-drawn into the Vello scene, so it never
//! goes stale behind a widget cache.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::color_compat::ColorCompat;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::Color;

use crate::draw;
use crate::model::{self, ThemeSpec};

const PANE_MARGIN: f64 = 22.0;
// Text-driven heights, at 1×. Every one of these seats glyphs the shared
// renderer draws at the accessibility text scale, so `SpecimenLayout`
// multiplies them by that scale: the specimen reflows under large text
// instead of stacking its sections over one another. The charts and the
// pane margin carry no text and stay fixed.
const TITLE_BAR_H: f64 = 42.0;
const SECTION_LABEL_H: f64 = 18.0;
const TYPOGRAPHY_BODY_H: f64 = 54.0;
const BUTTONS_BODY_H: f64 = 34.0;
const CONTROLS_BODY_H: f64 = 26.0;
const LIST_ROW_H: f64 = 38.0;
const LIST_ROWS: f64 = 3.0;
const BADGES_BODY_H: f64 = 22.0;
const CHART_MIN_H: f64 = 60.0;
const CHART_MAX_H: f64 = 130.0;
const CHART_COLUMN_GAP: f64 = 14.0;

/// Panic-free leaf construction for [`Widget::build_layout`]. `new_leaf`
/// always inserts and returns `Ok`; `Widget::build_layout` returns a bare
/// `NodeId` (not a `Result`) across every implementor, so surfacing a
/// theoretical error here would mean threading a `Result` through the whole
/// toolkit for a branch that can't be taken.
#[allow(clippy::unwrap_used, reason = "new_leaf cannot fail; see doc comment")]
fn leaf_node(taffy: &mut TaffyTree<()>, style: Style) -> NodeId {
    taffy.new_leaf(style).unwrap()
}

/// Shared geometry for the specimen's content region, computed once in
/// `draw` and threaded through each section-drawing method
/// (`draw_typography_section`, `draw_buttons_section`, ...) below the title
/// bar.
struct SpecimenLayout<'t> {
    th: &'t Theme,
    /// The accessibility text scale the specimen's glyphs are drawn at; every
    /// text-driven height below is this times its 1× constant.
    scale: f64,
    /// The "app window" card the whole specimen is drawn inside.
    card: Rect,
    /// Left edge of the content region (card left edge + padding).
    cx: f64,
    /// Width of the content region (card width minus padding on both sides).
    cw: f64,
    /// Padding between the card edge and the content region.
    pad: f64,
    radius: f64,
    /// Scaled text-driven heights the section painters lay their glyphs in.
    title_bar_h: f64,
    label_h: f64,
    buttons_h: f64,
    controls_h: f64,
    list_row_h: f64,
    typography_y: f64,
    buttons_y: f64,
    controls_y: f64,
    list_y: f64,
    badges_y: f64,
    charts_y: f64,
    bar_rect: Rect,
    spark_rect: Rect,
}

impl<'t> SpecimenLayout<'t> {
    fn padding(th: &Theme) -> f64 {
        f64::from(th.padding_standard).clamp(14.0, 22.0)
    }

    fn gap(th: &Theme) -> f64 {
        f64::from(th.gap_standard).clamp(10.0, 18.0)
    }

    /// A 1× text-driven measure at the specimen's text scale.
    fn scaled(&self, at_1x: f64) -> f64 {
        at_1x * self.scale
    }

    /// The text scale the specimen's geometry follows: never below 1×, so a
    /// small-text preference keeps the 1× specimen rather than shrinking its
    /// bands under glyphs that are still legible, and never a non-finite
    /// value. `new` and `minimum_pane_height` share this rule so the pane a
    /// specimen is allocated always matches the specimen drawn in it.
    fn geometry_scale(scale: f64) -> f64 {
        if scale.is_finite() {
            scale.max(1.0)
        } else {
            1.0
        }
    }

    /// The pane height that contains the complete specimen at `scale`,
    /// including the chart floor and the card's bottom padding/margin.
    fn minimum_pane_height(th: &Theme, scale: f64) -> f64 {
        let scale = Self::geometry_scale(scale);
        let section_bodies = TYPOGRAPHY_BODY_H
            + BUTTONS_BODY_H
            + CONTROLS_BODY_H
            + LIST_ROW_H * LIST_ROWS
            + BADGES_BODY_H;
        PANE_MARGIN * 2.0
            + Self::padding(th) * 2.0
            + (TITLE_BAR_H + SECTION_LABEL_H * 6.0 + section_bodies) * scale
            + Self::gap(th) * 5.0
            + CHART_MIN_H
    }

    fn new(th: &'t Theme, pane: Rect, scale: f64) -> Self {
        let scale = Self::geometry_scale(scale);
        let radius = th.radius_standard.clamp(0.0, 18.0);
        let card = Rect::new(
            pane.x0 + PANE_MARGIN,
            pane.y0 + PANE_MARGIN,
            pane.x1 - PANE_MARGIN,
            pane.y1 - PANE_MARGIN,
        );
        let pad = Self::padding(th);
        let gap = Self::gap(th);
        let cx = card.x0 + pad;
        let cw = card.width() - pad * 2.0;

        let title_bar_h = TITLE_BAR_H * scale;
        let label_h = SECTION_LABEL_H * scale;
        let buttons_h = BUTTONS_BODY_H * scale;
        let controls_h = CONTROLS_BODY_H * scale;
        let list_row_h = LIST_ROW_H * scale;

        let typography_y = card.y0 + title_bar_h + pad;
        let buttons_y = typography_y + label_h + TYPOGRAPHY_BODY_H * scale + gap;
        let controls_y = buttons_y + label_h + buttons_h + gap;
        let list_y = controls_y + label_h + controls_h + gap;
        let badges_y = list_y + label_h + list_row_h * LIST_ROWS + gap;
        let charts_y = badges_y + label_h + BADGES_BODY_H * scale + gap;
        let chart_content_y = charts_y + label_h;
        let chart_h = (card.y1 - pad - chart_content_y).clamp(CHART_MIN_H, CHART_MAX_H);
        let chart_w = (cw - CHART_COLUMN_GAP) / 2.0;
        let bar_rect = Rect::new(cx, chart_content_y, cx + chart_w, chart_content_y + chart_h);
        let spark_rect = Rect::new(
            cx + chart_w + CHART_COLUMN_GAP,
            chart_content_y,
            cx + cw,
            chart_content_y + chart_h,
        );

        Self {
            th,
            scale,
            card,
            cx,
            cw,
            pad,
            radius,
            title_bar_h,
            label_h,
            buttons_h,
            controls_h,
            list_row_h,
            typography_y,
            buttons_y,
            controls_y,
            list_y,
            badges_y,
            charts_y,
            bar_rect,
            spark_rect,
        }
    }

    #[cfg(test)]
    fn tail_rect(&self) -> Rect {
        self.bar_rect.union(self.spark_rect)
    }
}

/// One specimen person in the "LIST" section.
#[derive(Clone, Copy)]
struct ListPerson {
    name: &'static str,
    sub: &'static str,
    color: Color,
    /// Unread count; `0` paints no badge.
    badge: u32,
}

/// Section label ("TYPOGRAPHY", "BUTTONS", ...) drawn at the top of each
/// specimen section (part of [`Widget::draw`]).
fn draw_section_label(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    cx: f64,
    y: f64,
    label: &str,
    th: &Theme,
) {
    PaintCtx::new(scene, t, tr, th).text(
        label,
        &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
        th.text_dim,
        Point::new(cx, y),
        None,
    );
}

pub struct PreviewPane {
    theme: Theme,
    spec: Option<ThemeSpec>,
    /// The typeface the specimen text is shaped in — derived from the
    /// selected spec's font so changing the font family re-renders the
    /// preview immediately, not just the font-name caption.
    font_choice: FontChoice,
    rect: Rect,
}

impl Default for PreviewPane {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewPane {
    pub fn new() -> Self {
        Self {
            theme: Theme::dark(),
            spec: None,
            font_choice: FontChoice::SansSerif,
            rect: Rect::ZERO,
        }
    }

    /// Draw the specimen into `rect` without a layout pass.
    #[cfg(test)]
    pub(crate) fn draw_snapshot(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        rect: Rect,
    ) {
        self.rect = rect;
        self.draw_specimen(scene, t, tr);
    }

    fn draw_specimen(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer) {
        if self.rect.width() < 8.0 {
            return;
        }
        let th = &self.theme;
        // The specimen's glyphs are drawn at the shared renderer's text
        // scale, so its text-driven geometry follows the same factor.
        let layout = SpecimenLayout::new(th, self.rect, f64::from(tr.text_scale()));

        // 1. Desktop background (surface base).
        draw::fill_rect(scene, t, self.rect, th.bg_app);

        // The "app window" card occupies the pane minus a margin. Its edge is
        // derived from the previewed theme's own raised surface
        // (`ui::surface::card`): this pane is the theme designer's proof that
        // a border registers under the palette they are building, so a
        // declared `border_standard` — invisible three shades from a dark
        // `raised` — would prove the wrong thing.
        ui::surface::card(scene, t, layout.card, layout.radius + 4.0, th.bg_primary);

        // 2. Title bar.
        self.draw_title_bar(scene, t, tr, layout.card, layout.radius, layout.title_bar_h);

        // 3. Typography — the specimen is shaped in the theme's chosen font
        //    family (via `font_choice`) so a font change re-renders here live.
        self.draw_typography_section(scene, t, tr, &layout, layout.typography_y);
        // 4. Buttons.
        self.draw_buttons_section(scene, t, tr, &layout, layout.buttons_y);
        // 5. Controls (toggle, checkbox, slider).
        self.draw_controls_section(scene, t, tr, &layout, layout.controls_y);
        // 6. List with a selected row.
        Self::draw_list_section(scene, t, tr, &layout, layout.list_y);
        // 7. Badges.
        self.draw_badges_section(scene, t, tr, &layout, layout.badges_y);
        // 8. Charts (bar chart + sparkline) sharing a row.
        Self::draw_charts_section(scene, t, tr, &layout, layout.charts_y);
    }

    /// Rebuild the live theme from the selected spec.
    pub fn set_spec(&mut self, spec: &ThemeSpec) {
        self.theme = spec.to_theme();
        self.font_choice = model::font_choice(&spec.font);
        self.spec = Some(spec.clone());
    }

    /// Minimum narrow allocation that contains the complete specimen at the
    /// OS theme's `text_scale`, including the chart floor and the card's
    /// bottom padding/margin. The previewed theme supplies the specimen's
    /// colours and spacing; the person's text-size preference is the OS
    /// theme's, so it is passed in rather than read from the previewed one.
    pub(crate) fn narrow_intrinsic_height(&self, text_scale: f32) -> f32 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the specimen height is a small logical-pixel value representable by f32"
        )]
        let height = SpecimenLayout::minimum_pane_height(&self.theme, f64::from(text_scale)) as f32;
        height
    }

    #[cfg(test)]
    pub(crate) fn specimen_card_rect(&self) -> Rect {
        SpecimenLayout::new(&self.theme, self.rect, 1.0).card
    }

    #[cfg(test)]
    pub(crate) fn specimen_tail_rect(&self) -> Rect {
        SpecimenLayout::new(&self.theme, self.rect, 1.0).tail_rect()
    }

    fn font(&self) -> String {
        self.theme.tokens.typography.sans_family.clone()
    }

    fn on_accent(&self) -> Color {
        let a = [
            self.theme.accent_primary.r(),
            self.theme.accent_primary.g(),
            self.theme.accent_primary.b(),
        ];
        model::color(model::on_color(a))
    }

    fn lighten(c: Color, t: f32) -> Color {
        model::color(model::mix([c.r(), c.g(), c.b()], [255, 255, 255], t))
    }
    fn alpha(c: Color, a: f32) -> Color {
        c.multiply_alpha(a)
    }
}

impl Widget for PreviewPane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = leaf_node(
            taffy,
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                flex_basis: Dimension::length(0.0),
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else { return };
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
        t: Affine,
        tr: &mut TextRenderer,
        _os_theme: &Theme,
        _p: &PresenceCtx,
    ) {
        self.draw_specimen(scene, t, tr);
    }

    fn name(&self) -> &'static str {
        "PreviewPane"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn test_id(&self) -> Option<&str> {
        Some("preview_pane")
    }
}

impl PreviewPane {
    /// The "DATA" section's bar chart (part of [`Widget::draw`], via
    /// [`Self::draw_charts_section`]). Free of `self` — it draws only from
    /// its own fixed specimen data and the passed-in theme/geometry.
    fn draw_bar_chart(scene: &mut Scene, t: Affine, rect: Rect, th: &Theme) {
        ui::surface::card(
            scene,
            t,
            rect,
            th.radius_standard.clamp(0.0, 12.0),
            th.bg_app,
        );
        let pad = 12.0;
        let inner = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x1 - pad, rect.y1 - pad);
        let data = [0.55f64, 0.8, 0.4, 0.95, 0.65, 0.5, 0.72];
        let n = data.len();
        let gap = 6.0;
        let bw = (inner.width() - gap * (n as f64 - 1.0)) / n as f64;
        // baseline grid — the same derived hairline as the panel's edge
        draw::line(
            scene,
            t,
            Point::new(inner.x0, inner.y1),
            Point::new(inner.x1, inner.y1),
            ui::surface::edge_on(th.bg_app),
            1.0,
        );
        for (i, v) in data.iter().enumerate() {
            let bx = inner.x0 + i as f64 * (bw + gap);
            let bh = inner.height() * v;
            let r = Rect::new(bx, inner.y1 - bh, bx + bw, inner.y1);
            let max = *v >= 0.94;
            let col = if max {
                th.accent_primary
            } else {
                Self::alpha(th.accent_primary, 0.55)
            };
            draw::fill_rrect(scene, t, r, 3.0, col);
        }
    }

    /// The "DATA" section's sparkline (part of [`Widget::draw`], via
    /// [`Self::draw_charts_section`]). Free of `self`, for the same reason
    /// as [`Self::draw_bar_chart`].
    fn draw_sparkline(scene: &mut Scene, t: Affine, rect: Rect, th: &Theme) {
        ui::surface::card(
            scene,
            t,
            rect,
            th.radius_standard.clamp(0.0, 12.0),
            th.bg_app,
        );
        let pad = 12.0;
        let inner = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x1 - pad, rect.y1 - pad);
        let data = [0.3f64, 0.5, 0.42, 0.66, 0.55, 0.78, 0.7, 0.9, 0.82];
        let n = data.len();
        let pts: Vec<Point> = data
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = inner.x0 + inner.width() * (i as f64 / (n as f64 - 1.0));
                let yv = inner.y1 - inner.height() * v;
                Point::new(x, yv)
            })
            .collect();
        draw::area(
            scene,
            t,
            &pts,
            inner.y1,
            Self::alpha(th.accent_primary, 0.22),
        );
        draw::polyline(scene, t, &pts, th.accent_primary, 2.0);
        if let Some(last) = pts.last() {
            draw::circle(scene, t, *last, 3.5, th.accent_primary);
        }
    }

    /// The title bar: background, window dots, and the centred theme/app
    /// name (part of [`Widget::draw`]).
    fn draw_title_bar(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        card: Rect,
        radius: f64,
        tb_h: f64,
    ) {
        let th = &self.theme;
        let sem = &th.tokens.colors.semantic;
        let title_bar = Rect::new(card.x0, card.y0, card.x1, card.y0 + tb_h);
        let bar_fill = Self::lighten(th.bg_primary, 0.04);
        draw::fill_rrect(scene, t, title_bar, radius + 4.0, bar_fill);
        // square off the bottom of the title bar so only the top corners round
        draw::fill_rect(
            scene,
            t,
            Rect::new(card.x0, card.y0 + tb_h - 10.0, card.x1, card.y0 + tb_h),
            bar_fill,
        );
        // The bar's bottom rule is a hairline derived from the bar's own fill,
        // so it separates bar from body on a dark preset as well as a light one.
        draw::line(
            scene,
            t,
            Point::new(card.x0, card.y0 + tb_h),
            Point::new(card.x1, card.y0 + tb_h),
            ui::surface::edge_on(bar_fill),
            1.0,
        );
        // window dots
        for (i, c) in [sem.danger, sem.warning, sem.success].iter().enumerate() {
            draw::circle(
                scene,
                t,
                Point::new(card.x0 + 18.0 + i as f64 * 16.0, card.y0 + tb_h / 2.0),
                5.0,
                *c,
            );
        }
        let title = self
            .spec
            .as_ref()
            .map_or_else(|| th.name.clone(), |s| s.name.clone());
        draw::text_center(
            &mut PaintCtx::new(scene, t, tr, th),
            &title,
            &TextStyle::new(13.0).with_weight(TextWeight::Medium),
            th.text_dim,
            Rect::new(card.x0, card.y0, card.x1, card.y0 + tb_h),
        );
    }

    /// The "TYPOGRAPHY" section: two lines of specimen text shaped in the
    /// theme's chosen font (part of [`Widget::draw`]).
    fn draw_typography_section(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        layout: &SpecimenLayout,
        mut y: f64,
    ) {
        let th = layout.th;
        draw_section_label(tr, scene, t, layout.cx, y, "TYPOGRAPHY", th);
        y += layout.label_h;
        PaintCtx::new(scene, t, tr, th).text(
            "The quick brown fox",
            &TextStyle::new(24.0)
                .with_weight(TextWeight::Bold)
                .with_family(self.font_choice.clone()),
            th.text_primary,
            Point::new(layout.cx, y),
            None,
        );
        y += layout.scaled(32.0);
        PaintCtx::new(scene, t, tr, th).text(
            &format!("Aa Bb Cc · 0123456789 · {}", self.font()),
            &TextStyle::new(13.0).with_family(self.font_choice.clone()),
            th.text_dim,
            Point::new(layout.cx, y),
            None,
        );
    }

    /// The "BUTTONS" section: primary / secondary / ghost / disabled
    /// specimen buttons (part of [`Widget::draw`]).
    fn draw_buttons_section(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        layout: &SpecimenLayout,
        mut y: f64,
    ) {
        let th = layout.th;
        draw_section_label(tr, scene, t, layout.cx, y, "BUTTONS", th);
        y += layout.label_h;
        let bh = layout.buttons_h;
        let bw = ((layout.cw - 30.0) / 4.0).min(150.0);
        let labels = ["Primary", "Secondary", "Ghost", "Disabled"];
        for (i, lab) in labels.iter().enumerate() {
            let bx = layout.cx + i as f64 * (bw + 10.0);
            let r = Rect::new(bx, y, bx + bw, y + bh);
            match i {
                0 => {
                    draw::fill_rrect(scene, t, r, layout.radius, th.accent_primary);
                    draw::text_center(
                        &mut PaintCtx::new(scene, t, tr, th),
                        lab,
                        &TextStyle::new(12.5).with_weight(TextWeight::Semibold),
                        self.on_accent(),
                        r,
                    );
                }
                1 => {
                    // Neutral button: rest face with its derived edge.
                    ui::surface::card(scene, t, r, layout.radius, th.bg_app);
                    draw::text_center(
                        &mut PaintCtx::new(scene, t, tr, th),
                        lab,
                        &TextStyle::new(12.5).with_weight(TextWeight::Semibold),
                        th.text_primary,
                        r,
                    );
                }
                2 => {
                    draw::text_center(
                        &mut PaintCtx::new(scene, t, tr, th),
                        lab,
                        &TextStyle::new(12.5).with_weight(TextWeight::Semibold),
                        th.accent_primary,
                        r,
                    );
                }
                _ => {
                    // Disabled: the Neutral rest face *and* its edge under the
                    // toolkit's disabled alpha (HIG §9.5: disabled is the rest
                    // face under `DISABLED_ALPHA`), so rest and disabled are
                    // two looks; the edge fades with the face by design.
                    let disabled = ui::widget::DISABLED_ALPHA;
                    draw::fill_rrect(scene, t, r, layout.radius, Self::alpha(th.bg_app, disabled));
                    draw::stroke_rrect(
                        scene,
                        t,
                        r.inset(-0.5),
                        (layout.radius - 0.5).max(0.0),
                        Self::alpha(ui::surface::edge_on(th.bg_app), disabled),
                        1.0,
                    );
                    draw::text_center(
                        &mut PaintCtx::new(scene, t, tr, th),
                        lab,
                        &TextStyle::new(12.5).with_weight(TextWeight::Semibold),
                        Self::alpha(th.text_dim, disabled),
                        r,
                    );
                }
            }
        }
    }

    /// The "CONTROLS" section: a toggle, a checkbox, and a slider, each in
    /// their "on" / checked / partial specimen state (part of
    /// [`Widget::draw`]).
    fn draw_controls_section(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        layout: &SpecimenLayout,
        mut y: f64,
    ) {
        let th = layout.th;
        let cx = layout.cx;
        let cw = layout.cw;
        let card = layout.card;
        let pad = layout.pad;
        let radius = layout.radius;
        draw_section_label(tr, scene, t, cx, y, "CONTROLS", th);
        y += layout.label_h;
        let row_h = layout.controls_h;
        // The toggle, checkbox, their labels and the slider's origin seat
        // glyphs drawn at the text scale, so every 1× measure in this row
        // follows `layout.scaled`.
        let s = |v: f64| layout.scaled(v);
        // Toggle (on). One knob colour for both toggle states (HIG §9.5):
        // ink that clears the accent *and* the off track, so the specimen
        // shows what a real toggle would, not a literal white disc. The same
        // ink serves the slider thumb below — it clears the same two grounds
        // (the accent-filled half and the lifted unfilled half).
        let colors = &th.tokens.colors;
        let off_track = colors.lift(1.0);
        let control_ink = colors.ink_on_any(
            &[off_track, th.accent_primary],
            th.text_primary,
            ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
        );
        let tg = Rect::new(cx, y, cx + s(44.0), y + s(24.0));
        draw::fill_rrect(scene, t, tg, s(12.0), th.accent_primary);
        draw::circle(
            scene,
            t,
            Point::new(tg.x1 - s(12.0), tg.center().y),
            s(9.0),
            control_ink,
        );
        draw::text(
            tr,
            scene,
            t,
            "On",
            12.0,
            th.text_dim,
            tg.x1 + s(8.0),
            y + s(5.0),
        );
        // Checkbox (checked)
        let cbx = cx + s(110.0);
        let cb = Rect::new(cbx, y + s(2.0), cbx + s(20.0), y + s(22.0));
        draw::fill_rrect(scene, t, cb, (radius * 0.4).max(3.0), th.accent_primary);
        draw::line(
            scene,
            t,
            Point::new(cb.x0 + 5.0, cb.center().y),
            Point::new(cb.x0 + 8.5, cb.y1 - 6.0),
            self.on_accent(),
            2.0,
        );
        draw::line(
            scene,
            t,
            Point::new(cb.x0 + 8.5, cb.y1 - 6.0),
            Point::new(cb.x1 - 4.5, cb.y0 + 5.0),
            self.on_accent(),
            2.0,
        );
        draw::text(
            tr,
            scene,
            t,
            "Enabled",
            12.0,
            th.text_dim,
            cb.x1 + s(8.0),
            y + s(5.0),
        );
        // Slider (~62%)
        let sl_x0 = cx + s(230.0);
        let sl_x1 = cx + cw.min(cw) - 0.0;
        let sl_x1 = sl_x1.min(card.x1 - pad);
        let track_y = y + row_h / 2.0 - 1.0;
        let fill_frac = 0.62;
        let knob_x = sl_x0 + (sl_x1 - sl_x0) * fill_frac;
        // Unfilled track: the lifted surface with its derived edge (a
        // `border_standard` fill was a bar the same three shades as the
        // ground on dark presets); filled half: the accent.
        let track = Rect::new(sl_x0, track_y - 3.0, sl_x1, track_y + 3.0);
        ui::surface::card(scene, t, track, 3.0, off_track);
        draw::fill_rrect(
            scene,
            t,
            Rect::new(sl_x0, track_y - 3.0, knob_x, track_y + 3.0),
            3.0,
            th.accent_primary,
        );
        // Thumb: the control ink (it clears both halves of the track), with
        // its own derived edge — never a literal white disc.
        draw::circle(scene, t, Point::new(knob_x, track_y), 8.0, control_ink);
        draw::stroke_circle(
            scene,
            t,
            Point::new(knob_x, track_y),
            7.5,
            ui::surface::edge_on(control_ink),
            1.0,
        );
    }

    /// One "LIST" row: selection highlight, avatar initial, name/subtitle,
    /// optional unread badge, and the row separator (part of
    /// [`Self::draw_list_section`]).
    fn draw_list_row(
        ctx: &mut PaintCtx<'_>,
        layout: &SpecimenLayout<'_>,
        rr: Rect,
        person: &ListPerson,
        selected: bool,
        rule_below: bool,
    ) {
        let th = ctx.theme;
        let sem = &th.tokens.colors.semantic;
        let (radius, rowh) = (layout.radius, layout.list_row_h);
        let ListPerson {
            name,
            sub,
            color: col,
            badge,
        } = *person;
        if selected {
            // The chosen row is a lift, not an accent wash (HIG §9.3): the
            // specimen demonstrates the selection language the OS uses, so a
            // theme designer sees how *their* palette lifts a row. The tier
            // is measured against the previewed theme's own surfaces.
            let tier = ui::chrome::selection_tier(th, ui::chrome::SelectionState::active(true));
            let row_radius = radius * 0.6;
            // One `s1` of air keeps the lifted row off the list's own edge and
            // off the rules above and below it (nothing touches).
            let sel = rr.inset(-f64::from(ui::tokens::Spacing::BASE.s1));
            draw::fill_rrect(ctx.scene, ctx.transform, sel, row_radius, tier.fill);
            if let Some(border) = tier.border {
                // The toolkit's crisp 1 px outline, half a pixel inside.
                ctx.scene.stroke(
                    &Stroke::new(1.0),
                    ctx.transform,
                    border,
                    None,
                    &ui::surface::edge_outline(sel, row_radius),
                );
            }
            if let Some(glow) = tier.inner_glow {
                draw::stroke_rrect(
                    ctx.scene,
                    ctx.transform,
                    sel.inset(-1.5),
                    (row_radius - 1.5).max(0.0),
                    glow,
                    1.0,
                );
            }
        }
        // The row's glyphs are drawn at the renderer's text scale, and the
        // row height arrives already scaled, so every 1× offset inside the
        // row follows the same factor.
        let s = rowh / LIST_ROW_H;
        // avatar
        let avatar_r = 12.0 * s;
        let ac = Point::new(rr.x0 + 22.0 * s, rr.center().y);
        draw::circle(ctx.scene, ctx.transform, ac, avatar_r, col);
        let initial = name.chars().next().unwrap_or('?').to_string();
        draw::text_center(
            ctx,
            &initial,
            &TextStyle::new(12.0).with_weight(TextWeight::Bold),
            th.on_surface(col).ink,
            Rect::new(
                ac.x - avatar_r,
                ac.y - avatar_r,
                ac.x + avatar_r,
                ac.y + avatar_r,
            ),
        );
        ctx.text(
            name,
            &TextStyle::new(13.0).with_weight(TextWeight::Medium),
            th.text_primary,
            Point::new(rr.x0 + 44.0 * s, rr.y0 + 7.0 * s),
            None,
        );
        draw::text(
            ctx.text,
            ctx.scene,
            ctx.transform,
            sub,
            11.0,
            th.text_dim,
            rr.x0 + 44.0 * s,
            rr.y0 + 22.0 * s,
        );
        // trailing badge
        if badge > 0 {
            let bw2 = 22.0 * s;
            let br = Rect::new(
                rr.x1 - bw2 - 10.0,
                rr.center().y - 9.0 * s,
                rr.x1 - 10.0,
                rr.center().y + 9.0 * s,
            );
            draw::fill_rrect(ctx.scene, ctx.transform, br, 9.0 * s, sem.danger);
            draw::text_center(
                ctx,
                &badge.to_string(),
                &TextStyle::new(10.0).with_weight(TextWeight::Bold),
                // Ink measured against the badge fill by the previewed
                // theme's own contrast policy, not a literal white.
                th.on_surface(sem.danger).ink,
                br,
            );
        }
        if rule_below {
            // Row rule: derived against the list's fill, inset from the
            // panel's corner radius on both ends (HIG §9.5, never full-bleed).
            draw::line(
                ctx.scene,
                ctx.transform,
                Point::new(rr.x0 + 44.0 * s, rr.y1),
                Point::new(rr.x1 - 10.0, rr.y1),
                ui::surface::edge_on(th.bg_app),
                1.0,
            );
        }
    }

    /// The "LIST" section: three specimen rows (one selected) with avatar,
    /// name, subtitle, and an unread-count badge on one row (part of
    /// [`Widget::draw`]).
    fn draw_list_section(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        layout: &SpecimenLayout,
        mut y: f64,
    ) {
        let th = layout.th;
        let sem = &th.tokens.colors.semantic;
        let cx = layout.cx;
        let cw = layout.cw;
        let radius = layout.radius;
        draw_section_label(tr, scene, t, cx, y, "LIST", th);
        y += layout.label_h;
        let rowh = layout.list_row_h;
        let people = [
            ListPerson {
                name: "Ada Lovelace",
                sub: "Online",
                color: sem.success,
                badge: 3,
            },
            ListPerson {
                name: "Alan Turing",
                sub: "Editing themes",
                color: th.accent_primary,
                badge: 0,
            },
            ListPerson {
                name: "Grace Hopper",
                sub: "Away",
                color: sem.warning,
                badge: 12,
            },
        ];
        let list_rect = Rect::new(cx, y, cx + cw, y + rowh * people.len() as f64);
        ui::surface::card(scene, t, list_rect, radius, th.bg_app);
        for (i, person) in people.iter().enumerate() {
            let ry0 = list_rect.y0 + i as f64 * rowh;
            Self::draw_list_row(
                &mut PaintCtx::new(scene, t, tr, th),
                layout,
                Rect::new(list_rect.x0, ry0, list_rect.x1, ry0 + rowh),
                person,
                i == 1,
                i + 1 < people.len(),
            );
        }
    }

    /// The "BADGES" section: four semantic badges plus a count pill (part
    /// of [`Widget::draw`]).
    fn draw_badges_section(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        layout: &SpecimenLayout,
        mut y: f64,
    ) {
        let th = layout.th;
        let sem = &th.tokens.colors.semantic;
        draw_section_label(tr, scene, t, layout.cx, y, "BADGES", th);
        y += layout.label_h;
        let badges = [
            ("Success", sem.success),
            ("Warning", sem.warning),
            ("Danger", sem.danger),
            ("Info", sem.info),
        ];
        // The chips seat glyphs drawn at the text scale, so their 1× padding,
        // height, radius and dot follow `layout.scaled` like the count pill.
        let mut bx = layout.cx;
        let chip_h = layout.scaled(22.0);
        for (lab, col) in badges {
            let (w, _) = tr.measure(lab, 11.0, None);
            let pw = f64::from(w) + layout.scaled(22.0);
            let br = Rect::new(bx, y, bx + pw, y + chip_h);
            draw::fill_rrect(scene, t, br, chip_h / 2.0, Self::alpha(col, 0.18));
            draw::stroke_rrect(scene, t, br, chip_h / 2.0, Self::alpha(col, 0.5), 1.0);
            draw::circle(
                scene,
                t,
                Point::new(br.x0 + layout.scaled(9.0), br.center().y),
                layout.scaled(3.0),
                col,
            );
            draw::text(
                tr,
                scene,
                t,
                lab,
                11.0,
                col,
                br.x0 + layout.scaled(15.0),
                y + layout.scaled(5.5),
            );
            bx += pw + layout.scaled(8.0);
        }
        // count pill + NEW
        let cr = Rect::new(bx, y, bx + 30.0, y + layout.scaled(BADGES_BODY_H));
        draw::fill_rrect(scene, t, cr, 11.0, th.accent_primary);
        draw::text_center(
            &mut PaintCtx::new(scene, t, tr, th),
            "12",
            &TextStyle::new(11.0).with_weight(TextWeight::Bold),
            self.on_accent(),
            cr,
        );
    }

    /// The "DATA" section: a bar chart and a sparkline sharing a row (part
    /// of [`Widget::draw`]).
    fn draw_charts_section(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        layout: &SpecimenLayout,
        y: f64,
    ) {
        let th = layout.th;
        draw_section_label(tr, scene, t, layout.cx, y, "DATA", th);
        Self::draw_bar_chart(scene, t, layout.bar_rect, th);
        Self::draw_sparkline(scene, t, layout.spark_rect, th);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::seed_custom;

    #[test]
    fn font_family_change_reshapes_the_previewed_specimen() {
        // The preview's specimen is drawn in `font_choice`; `set_spec` must
        // recompute it from the spec's font so a font-family change updates the
        // rendered typeface immediately (not just the font-name caption).
        let mut p = PreviewPane::new();
        let mut spec = seed_custom();

        spec.font = "Inter".to_string();
        p.set_spec(&spec);
        assert_eq!(p.font_choice, FontChoice::Named("Inter".to_string()));

        spec.font = "JetBrains Mono".to_string();
        p.set_spec(&spec);
        assert_eq!(
            p.font_choice,
            FontChoice::Monospace,
            "mono font re-shapes the specimen"
        );

        spec.font = "Source Serif".to_string();
        p.set_spec(&spec);
        assert_eq!(
            p.font_choice,
            FontChoice::Serif,
            "serif font re-shapes the specimen"
        );
    }

    /// The specimen is the theme designer's proof that borders register under
    /// the palette being built (HIG §7: an app's own edge is derived). Render
    /// the real `draw_specimen` for a dark preset, a light preset and the demo
    /// custom theme, then read pixels off the frame: the window card's
    /// outermost column against the card fill beside it, and the list panel's
    /// outermost column against the panel fill beside it. Both must move
    /// relative luminance by at least the toolkit's hairline floor — the
    /// declared `border_standard` this replaced failed that on every dark
    /// preset. Frames land under `target/ux1x/a3/themes/after/<theme>/` as
    /// the visual evidence.
    #[test]
    fn specimen_paints_visible_derived_edges_on_dark_and_light_palettes() {
        use ui::a11y::relative_luminance;
        use ui::offscreen::OffscreenRenderer;
        use ui::tokens::colors::HAIRLINE_MIN_DELTA;

        let catalog = model::default_catalog();
        let pick = |id: &str| {
            catalog
                .iter()
                .find(|s| s.id == id)
                .unwrap_or_else(|| panic!("catalogue ships {id}"))
                .clone()
        };
        // A dark preset, the pure-black preset (the hardest ground for a
        // hairline), a light preset and the demo custom theme.
        let specs = [
            ("dark", pick("tilo-dark")),
            ("dark", pick("oled")),
            ("light", pick("beige")),
            ("dark", model::seed_custom()),
        ];
        let (w, h) = (640_u32, 860_u32);
        let pane = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
        for (folder, spec) in specs {
            let mut p = PreviewPane::new();
            p.set_spec(&spec);
            let th = spec.to_theme();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            p.draw_snapshot(&mut scene, Affine::IDENTITY, &mut tr, pane);
            // Geometry from the same layout the painter used.
            let layout = SpecimenLayout::new(&th, pane, 1.0);
            let card = layout.card;
            let list_top = layout.list_y + layout.label_h;
            let list_x0 = layout.cx;

            let mut renderer = OffscreenRenderer::new(w, h);
            let rgba = renderer.render_rgba(&scene);
            let lum = |x: f64, y: f64| -> f64 {
                let i = ((y as usize) * (w as usize) + (x as usize)) * 4;
                relative_luminance(Color::new([
                    f32::from(rgba[i]) / 255.0,
                    f32::from(rgba[i + 1]) / 255.0,
                    f32::from(rgba[i + 2]) / 255.0,
                    1.0,
                ]))
            };
            // Window card: its left edge column, halfway down, against the
            // card fill six pixels in (the content is padded ≥ 14 px).
            let y = card.center().y;
            let card_edge = lum(card.x0, y);
            let card_fill = lum(card.x0 + 6.0, y);
            assert!(
                (card_edge - card_fill).abs() >= HAIRLINE_MIN_DELTA,
                "{}: the specimen card edge must register against its fill \
                 (edge {card_edge:.4} vs fill {card_fill:.4})",
                spec.name
            );
            // List panel: its left edge column on the first row's centre line
            // (the avatar starts 10 px in, so six pixels in is panel fill).
            let ly = list_top + layout.list_row_h * 0.5;
            let list_edge = lum(list_x0, ly);
            let list_fill = lum(list_x0 + 6.0, ly);
            assert!(
                (list_edge - list_fill).abs() >= HAIRLINE_MIN_DELTA,
                "{}: the specimen list edge must register against its fill \
                 (edge {list_edge:.4} vs fill {list_fill:.4})",
                spec.name
            );

            let slug: String = spec
                .id
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/ux1x/a3/themes/after")
                .join(folder)
                .join(format!("specimen-{slug}.png"));
            if let Some(parent) = out.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            renderer.render_png(&scene, th.bg_app, &out);
        }
    }
}
