//! The live preview pane.
//!
//! This is the one widget that legitimately *constructs* a
//! [`ui::theme::Theme`] (from the selected [`ThemeSpec`]) and renders a
//! representative slice of the qOS widget language under it — a window
//! card with a title bar, typography, buttons in every variant, a toggle
//! / checkbox / slider, a selectable list, semantic badges and two charts.
//! Everything pulls its colour, radius and spacing from the constructed
//! theme, so changing any token in the customizer is reflected here
//! instantly. It is fully self-drawn into the Vello scene, so it never
//! goes stale behind a widget cache.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::{FontChoice, TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::WindowEvent;

use crate::draw;
use crate::model::{self, ThemeSpec};

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

    /// Rebuild the live theme from the selected spec.
    pub fn set_spec(&mut self, spec: &ThemeSpec) {
        self.theme = spec.to_theme();
        self.font_choice = model::font_choice(&spec.font);
        self.spec = Some(spec.clone());
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
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                flex_basis: Dimension::length(0.0),
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
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
        if self.rect.width() < 8.0 {
            return;
        }
        let th = &self.theme;
        let sem = &th.tokens.colors.semantic;
        let radius = th.radius_standard.clamp(0.0, 18.0);

        // 1. Desktop background (surface base).
        draw::fill_rect(scene, t, self.rect, th.bg_app);

        // The "app window" card occupies the pane minus a margin.
        let margin = 22.0;
        let card = Rect::new(
            self.rect.x0 + margin,
            self.rect.y0 + margin,
            self.rect.x1 - margin,
            self.rect.y1 - margin,
        );
        draw::fill_rrect(scene, t, card, radius + 4.0, th.bg_primary);
        draw::stroke_rrect(scene, t, card, radius + 4.0, th.border_standard, 1.0);

        // 2. Title bar.
        let tb_h = 42.0;
        let title_bar = Rect::new(card.x0, card.y0, card.x1, card.y0 + tb_h);
        draw::fill_rrect(
            scene,
            t,
            title_bar,
            radius + 4.0,
            Self::lighten(th.bg_primary, 0.04),
        );
        // square off the bottom of the title bar so only the top corners round
        draw::fill_rect(
            scene,
            t,
            Rect::new(card.x0, card.y0 + tb_h - 10.0, card.x1, card.y0 + tb_h),
            Self::lighten(th.bg_primary, 0.04),
        );
        draw::line(
            scene,
            t,
            Point::new(card.x0, card.y0 + tb_h),
            Point::new(card.x1, card.y0 + tb_h),
            th.border_standard,
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
            tr,
            scene,
            t,
            &title,
            13.0,
            th.text_dim,
            card.x0,
            card.x1,
            card.y0,
            card.y0 + tb_h,
            TextWeight::Medium,
        );

        // Content region.
        let pad = f64::from(th.padding_standard).clamp(14.0, 22.0);
        let cx = card.x0 + pad;
        let cw = card.width() - pad * 2.0;
        let mut y = card.y0 + tb_h + pad;
        let gap = f64::from(th.gap_standard).clamp(10.0, 18.0);

        let section =
            |tr: &mut TextRenderer, scene: &mut Scene, y: f64, label: &str, th: &Theme| {
                draw::text_w(
                    tr,
                    scene,
                    t,
                    label,
                    11.0,
                    th.text_dim,
                    cx,
                    y,
                    TextWeight::Semibold,
                );
            };

        // 3. Typography — the specimen is shaped in the theme's chosen font
        //    family (via `font_choice`) so a font change re-renders here live.
        section(tr, scene, y, "TYPOGRAPHY", th);
        y += 18.0;
        draw::text_font(
            tr,
            scene,
            t,
            "The quick brown fox",
            24.0,
            th.text_primary,
            cx,
            y,
            TextWeight::Bold,
            &self.font_choice,
        );
        y += 32.0;
        draw::text_font(
            tr,
            scene,
            t,
            &format!("Aa Bb Cc · 0123456789 · {}", self.font()),
            13.0,
            th.text_dim,
            cx,
            y,
            TextWeight::Regular,
            &self.font_choice,
        );
        y += 22.0 + gap;

        // 4. Buttons.
        section(tr, scene, y, "BUTTONS", th);
        y += 18.0;
        let bh = 34.0;
        let bw = ((cw - 30.0) / 4.0).min(150.0);
        let labels = ["Primary", "Secondary", "Ghost", "Disabled"];
        for (i, lab) in labels.iter().enumerate() {
            let bx = cx + i as f64 * (bw + 10.0);
            let r = Rect::new(bx, y, bx + bw, y + bh);
            match i {
                0 => {
                    draw::fill_rrect(scene, t, r, radius, th.accent_primary);
                    draw::text_center(
                        tr,
                        scene,
                        t,
                        lab,
                        12.5,
                        self.on_accent(),
                        r.x0,
                        r.x1,
                        r.y0,
                        r.y1,
                        TextWeight::Semibold,
                    );
                }
                1 => {
                    draw::fill_rrect(scene, t, r, radius, th.bg_app);
                    draw::stroke_rrect(scene, t, r, radius, th.border_standard, 1.0);
                    draw::text_center(
                        tr,
                        scene,
                        t,
                        lab,
                        12.5,
                        th.text_primary,
                        r.x0,
                        r.x1,
                        r.y0,
                        r.y1,
                        TextWeight::Semibold,
                    );
                }
                2 => {
                    draw::text_center(
                        tr,
                        scene,
                        t,
                        lab,
                        12.5,
                        th.accent_primary,
                        r.x0,
                        r.x1,
                        r.y0,
                        r.y1,
                        TextWeight::Semibold,
                    );
                }
                _ => {
                    draw::fill_rrect(scene, t, r, radius, Self::alpha(th.bg_app, 0.6));
                    draw::text_center(
                        tr,
                        scene,
                        t,
                        lab,
                        12.5,
                        Self::alpha(th.text_dim, 0.6),
                        r.x0,
                        r.x1,
                        r.y0,
                        r.y1,
                        TextWeight::Semibold,
                    );
                }
            }
        }
        y += bh + gap;

        // 5. Controls (toggle, checkbox, slider).
        section(tr, scene, y, "CONTROLS", th);
        y += 18.0;
        let row_h = 26.0;
        // Toggle (on)
        let tg = Rect::new(cx, y, cx + 44.0, y + 24.0);
        draw::fill_rrect(scene, t, tg, 12.0, th.accent_primary);
        draw::circle(
            scene,
            t,
            Point::new(tg.x1 - 12.0, tg.center().y),
            9.0,
            Color::WHITE,
        );
        draw::text(tr, scene, t, "On", 12.0, th.text_dim, tg.x1 + 8.0, y + 5.0);
        // Checkbox (checked)
        let cbx = cx + 110.0;
        let cb = Rect::new(cbx, y + 2.0, cbx + 20.0, y + 22.0);
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
            cb.x1 + 8.0,
            y + 5.0,
        );
        // Slider (~62%)
        let sl_x0 = cx + 230.0;
        let sl_x1 = cx + cw.min(cw) - 0.0;
        let sl_x1 = sl_x1.min(card.x1 - pad);
        let track_y = y + row_h / 2.0 - 1.0;
        let fill_frac = 0.62;
        let knob_x = sl_x0 + (sl_x1 - sl_x0) * fill_frac;
        draw::fill_rrect(
            scene,
            t,
            Rect::new(sl_x0, track_y - 3.0, sl_x1, track_y + 3.0),
            3.0,
            th.border_standard,
        );
        draw::fill_rrect(
            scene,
            t,
            Rect::new(sl_x0, track_y - 3.0, knob_x, track_y + 3.0),
            3.0,
            th.accent_primary,
        );
        draw::circle(scene, t, Point::new(knob_x, track_y), 8.0, Color::WHITE);
        draw::stroke_circle(
            scene,
            t,
            Point::new(knob_x, track_y),
            8.0,
            th.accent_primary,
            2.0,
        );
        y += row_h + gap;

        // 6. List with a selected row.
        section(tr, scene, y, "LIST", th);
        y += 18.0;
        let rowh = 38.0;
        let people = [
            ("Ada Lovelace", "Online", sem.success, 3u32),
            ("Alan Turing", "Editing themes", th.accent_primary, 0),
            ("Grace Hopper", "Away", sem.warning, 12),
        ];
        let list_rect = Rect::new(cx, y, cx + cw, y + rowh * people.len() as f64);
        draw::fill_rrect(scene, t, list_rect, radius, th.bg_app);
        draw::stroke_rrect(scene, t, list_rect, radius, th.border_standard, 1.0);
        for (i, (name, sub, col, badge)) in people.iter().enumerate() {
            let ry0 = y + i as f64 * rowh;
            let rr = Rect::new(list_rect.x0, ry0, list_rect.x1, ry0 + rowh);
            if i == 1 {
                draw::fill_rrect(
                    scene,
                    t,
                    rr,
                    radius * 0.6,
                    Self::alpha(th.accent_primary, 0.16),
                );
                draw::fill_rect(
                    scene,
                    t,
                    Rect::new(rr.x0, rr.y0 + 6.0, rr.x0 + 3.0, rr.y1 - 6.0),
                    th.accent_primary,
                );
            }
            // avatar
            let ac = Point::new(rr.x0 + 22.0, rr.center().y);
            draw::circle(scene, t, ac, 12.0, *col);
            let initial = name.chars().next().unwrap_or('?').to_string();
            draw::text_center(
                tr,
                scene,
                t,
                &initial,
                12.0,
                model::color(model::on_color([col.r(), col.g(), col.b()])),
                ac.x - 12.0,
                ac.x + 12.0,
                ac.y - 12.0,
                ac.y + 12.0,
                TextWeight::Bold,
            );
            draw::text_w(
                tr,
                scene,
                t,
                name,
                13.0,
                th.text_primary,
                rr.x0 + 44.0,
                rr.y0 + 7.0,
                TextWeight::Medium,
            );
            draw::text(
                tr,
                scene,
                t,
                sub,
                11.0,
                th.text_dim,
                rr.x0 + 44.0,
                rr.y0 + 22.0,
            );
            // trailing badge
            if *badge > 0 {
                let bw2 = 22.0;
                let br = Rect::new(
                    rr.x1 - bw2 - 10.0,
                    rr.center().y - 9.0,
                    rr.x1 - 10.0,
                    rr.center().y + 9.0,
                );
                draw::fill_rrect(scene, t, br, 9.0, sem.danger);
                draw::text_center(
                    tr,
                    scene,
                    t,
                    &badge.to_string(),
                    10.0,
                    Color::WHITE,
                    br.x0,
                    br.x1,
                    br.y0,
                    br.y1,
                    TextWeight::Bold,
                );
            }
            if i + 1 < people.len() {
                draw::line(
                    scene,
                    t,
                    Point::new(rr.x0 + 44.0, rr.y1),
                    Point::new(rr.x1 - 10.0, rr.y1),
                    Self::alpha(th.border_standard, 0.6),
                    1.0,
                );
            }
        }
        y += rowh * people.len() as f64 + gap;

        // 7. Badges.
        section(tr, scene, y, "BADGES", th);
        y += 18.0;
        let badges = [
            ("Success", sem.success),
            ("Warning", sem.warning),
            ("Danger", sem.danger),
            ("Info", sem.info),
        ];
        let mut bx = cx;
        for (lab, col) in badges {
            let (w, _) = tr.measure(lab, 11.0, None);
            let pw = f64::from(w) + 22.0;
            let br = Rect::new(bx, y, bx + pw, y + 22.0);
            draw::fill_rrect(scene, t, br, 11.0, Self::alpha(col, 0.18));
            draw::stroke_rrect(scene, t, br, 11.0, Self::alpha(col, 0.5), 1.0);
            draw::circle(scene, t, Point::new(br.x0 + 9.0, br.center().y), 3.0, col);
            draw::text(tr, scene, t, lab, 11.0, col, br.x0 + 15.0, y + 5.5);
            bx += pw + 8.0;
        }
        // count pill + NEW
        let cr = Rect::new(bx, y, bx + 30.0, y + 22.0);
        draw::fill_rrect(scene, t, cr, 11.0, th.accent_primary);
        draw::text_center(
            tr,
            scene,
            t,
            "12",
            11.0,
            self.on_accent(),
            cr.x0,
            cr.x1,
            cr.y0,
            cr.y1,
            TextWeight::Bold,
        );
        y += 22.0 + gap;

        // 8. Charts (bar chart + sparkline) sharing a row.
        section(tr, scene, y, "DATA", th);
        y += 18.0;
        let chart_h = (card.y1 - pad - y).clamp(60.0, 130.0);
        let gapx = 14.0;
        let chart_w = (cw - gapx) / 2.0;
        let bar_rect = Rect::new(cx, y, cx + chart_w, y + chart_h);
        let spark_rect = Rect::new(cx + chart_w + gapx, y, cx + cw, y + chart_h);
        self.draw_bar_chart(scene, t, bar_rect, th);
        self.draw_sparkline(scene, t, spark_rect, th);
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

    #[allow(clippy::too_many_arguments)]
    fn handle_event(
        &mut self,
        _e: &WindowEvent,
        _s: f64,
        _m: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _p: &ObjId,
        _h: &mut bool,
        _f: &mut Option<NodeId>,
        _n: NodeId,
        _t: &TaffyTree<()>,
    ) {
    }
}

impl PreviewPane {
    fn draw_bar_chart(&self, scene: &mut Scene, t: Affine, rect: Rect, th: &Theme) {
        draw::fill_rrect(
            scene,
            t,
            rect,
            th.radius_standard.clamp(0.0, 12.0),
            th.bg_app,
        );
        draw::stroke_rrect(
            scene,
            t,
            rect,
            th.radius_standard.clamp(0.0, 12.0),
            th.border_standard,
            1.0,
        );
        let pad = 12.0;
        let inner = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x1 - pad, rect.y1 - pad);
        let data = [0.55f64, 0.8, 0.4, 0.95, 0.65, 0.5, 0.72];
        let n = data.len();
        let gap = 6.0;
        let bw = (inner.width() - gap * (n as f64 - 1.0)) / n as f64;
        // baseline grid
        draw::line(
            scene,
            t,
            Point::new(inner.x0, inner.y1),
            Point::new(inner.x1, inner.y1),
            Self::alpha(th.border_standard, 0.8),
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

    fn draw_sparkline(&self, scene: &mut Scene, t: Affine, rect: Rect, th: &Theme) {
        draw::fill_rrect(
            scene,
            t,
            rect,
            th.radius_standard.clamp(0.0, 12.0),
            th.bg_app,
        );
        draw::stroke_rrect(
            scene,
            t,
            rect,
            th.radius_standard.clamp(0.0, 12.0),
            th.border_standard,
            1.0,
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
}
