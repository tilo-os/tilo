//! Slide-sorter overview grid.

use std::cell::Cell;

use vello::kurbo::{Affine, Rect};
use vello::peniko::{BlendMode, Fill};

use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::chrome;
use crate::model::{Deck, SLIDE_H, SLIDE_W, Slide};
use crate::render::{self, DeckAssets};

/// Classic 1× header (title line, subtitle line, divider, `s1` seam); the
/// floor of the measured header and the value used before the first draw.
const HEADER_H_1X: f64 = 64.0;
/// Least advance from the title's top to the subtitle's (1×), and from the
/// subtitle's top to the divider.
const HEADER_LINE_ADV: f64 = 22.0;
/// Side inset of the header text and the thumbnail grid.
const PAD: f64 = 36.0;

pub(crate) struct OverviewGrid {
    scroll: f64,
    /// Header height measured at the last draw — title and subtitle line
    /// boxes plus their seams — so `geometry` (hit-testing, wheel, clamp)
    /// starts the grid where the painter drew its divider at every text
    /// scale. `HEADER_H_1X` until the first draw.
    header_h: Cell<f64>,
}

impl OverviewGrid {
    pub(crate) fn new() -> Self {
        Self {
            scroll: 0.0,
            header_h: Cell::new(HEADER_H_1X),
        }
    }

    /// Resolve the header rows for this frame's renderer and remember the
    /// resulting height for `geometry`. Returns `(title_top, subtitle_top,
    /// divider_y)` in window space.
    fn header_rows(&self, text: &mut TextRenderer, theme: &Theme, window: Rect) -> (f64, f64, f64) {
        let sp = &theme.tokens.spacing;
        let (_, lh_title) = text.measure_weighted("Ag", 18.0, None, TextWeight::Bold);
        let (_, lh_sub) = text.measure_weighted("Ag", 12.0, None, TextWeight::Regular);
        let title_top = window.y0 + f64::from(sp.s4);
        let sub_top = title_top + f64::from(lh_title).max(HEADER_LINE_ADV);
        let divider_y = sub_top + (f64::from(lh_sub) + f64::from(sp.s1)).max(HEADER_LINE_ADV);
        self.header_h
            .set((divider_y + f64::from(sp.s1) - window.y0).max(HEADER_H_1X));
        (title_top, sub_top, divider_y)
    }

    pub(crate) fn scroll(&self) -> f64 {
        self.scroll
    }

    pub(crate) fn set_scroll(&mut self, value: f64) {
        self.scroll = value;
    }

    pub(crate) fn geometry(&self, window: Rect, count: usize) -> (Rect, Vec<(usize, Rect)>, f64) {
        let area = Rect::new(
            window.x0,
            window.y0 + self.header_h.get(),
            window.x1,
            window.y1,
        );
        let pad = PAD;
        let gap = 26.0;
        let avail_w = (area.width() - 2.0 * pad).max(200.0);
        let cols = (((avail_w + gap) / 286.0).floor().max(1.0)) as usize;
        let width = (avail_w - (cols as f64 - 1.0) * gap) / cols as f64;
        let height = width * SLIDE_H / SLIDE_W;
        let cell_h = height + 30.0;
        let rows = if count == 0 { 0 } else { count.div_ceil(cols) };
        let content_h = if rows == 0 {
            0.0
        } else {
            pad + rows as f64 * cell_h + (rows as f64 - 1.0) * gap + pad
        };
        let max_scroll = (content_h - area.height()).max(0.0);
        let scroll = self.scroll.clamp(0.0, max_scroll);
        let thumbs = (0..count)
            .map(|index| {
                let col = index % cols;
                let row = index / cols;
                let x = area.x0 + pad + col as f64 * (width + gap);
                let y = area.y0 + pad + row as f64 * (cell_h + gap) - scroll;
                (index, Rect::new(x, y, x + width, y + height))
            })
            .collect();
        (area, thumbs, max_scroll)
    }

    pub(crate) fn clamp(&mut self, window: Rect, count: usize) {
        let (_, _, max) = self.geometry(window, count);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    pub(crate) fn wheel(&mut self, window: Rect, count: usize, delta: f64) -> bool {
        let (_, _, max) = self.geometry(window, count);
        if max <= 0.0 {
            return false;
        }
        self.scroll = (self.scroll - delta).clamp(0.0, max);
        true
    }

    pub(crate) fn slide_at(
        &self,
        window: Rect,
        count: usize,
        point: vello::kurbo::Point,
    ) -> Option<usize> {
        self.geometry(window, count)
            .1
            .into_iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(index, _)| index)
    }

    pub(crate) fn draw(
        &self,
        ctx: &mut PaintCtx<'_>,
        window: Rect,
        deck: &Deck,
        current: usize,
        assets: DeckAssets<'_>,
    ) {
        let (scene, transform, text, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let colors = &theme.tokens.colors;
        chrome::fill_rect(scene, transform, window, colors.surfaces.base);
        // Header rows follow the measured title and subtitle line boxes, so a
        // 2× title never paints over the hint beneath it; the title is elided
        // to the window rather than running under its right edge.
        let (title_top, sub_top, divider_y) = self.header_rows(text, theme, window);
        let text_w = (window.width() - 2.0 * PAD).max(0.0) as f32;
        let title = text.elide_to_width_weighted(&deck.title, 18.0, text_w, TextWeight::Bold);
        text.draw_text_weighted(
            scene,
            &title,
            18.0,
            colors.text_primary,
            transform * Affine::translate((window.x0 + PAD, title_top)),
            None,
            TextWeight::Bold,
        );
        let hint = crate::strings::tr_args(
            "slides-overview-hint",
            &[("count", ui::i18n::ArgValue::from(deck.slides.len() as i64))],
        );
        let hint = text.elide_to_width_weighted(&hint, 12.0, text_w, TextWeight::Regular);
        text.draw_text_weighted(
            scene,
            &hint,
            12.0,
            colors.text_dim,
            transform * Affine::translate((window.x0 + PAD, sub_top)),
            None,
            TextWeight::Regular,
        );
        // The header rule is the window ground's derived edge on one crisp
        // pixel row.
        chrome::hairline_h(
            scene,
            transform,
            window.x0,
            window.x1,
            divider_y.floor(),
            ui::surface::edge_on(colors.surfaces.base),
        );
        let (area, thumbs, max) = self.geometry(window, deck.slides.len());
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &area);
        for (index, rect) in thumbs {
            let Some(slide) = deck.slides.get(index) else {
                continue;
            };
            Self::draw_thumb(ctx, assets, (index, slide), rect, index == current);
        }
        let scene = &mut *ctx.scene;
        scene.pop_layer();
        if let Some((track, thumb)) = scrollbar_rects(area, self.scroll.clamp(0.0, max), max) {
            chrome::fill_round(
                scene,
                transform,
                track,
                3.0,
                render::with_alpha(colors.text_primary, 18),
            );
            chrome::fill_round(
                scene,
                transform,
                thumb,
                3.0,
                render::with_alpha(colors.text_primary, 90),
            );
        }
    }

    /// One overview-grid thumbnail: drop shadow, the slide render, the
    /// current-slide accent border, and its index/layout-label caption.
    /// Part of [`Self::draw`].
    fn draw_thumb(
        ctx: &mut PaintCtx<'_>,
        assets: DeckAssets<'_>,
        (index, slide): (usize, &Slide),
        rect: Rect,
        is_current: bool,
    ) {
        let (transform, colors, palette) =
            (ctx.transform, &ctx.theme.tokens.colors, assets.palette);
        ui::surface::shadow(
            ctx.scene,
            transform,
            rect,
            8.0,
            &ctx.theme.tokens.elevation.e2,
        );
        render::draw_slide(ctx, assets, slide, rect, 8.0);
        let (scene, text) = (&mut *ctx.scene, &mut *ctx.text);
        // The thumbnail frame is the window ground's derived edge; the
        // current slide keeps its accent ring.
        chrome::stroke_round(
            scene,
            transform,
            rect,
            8.0,
            if is_current {
                palette.accent
            } else {
                ui::surface::edge_on(colors.surfaces.base)
            },
            if is_current { 2.5 } else { 1.0 },
        );
        let number = format!("{}", index + 1);
        let y = rect.y1 + 7.0;
        text.draw_text_weighted(
            scene,
            &number,
            12.0,
            if is_current {
                palette.accent
            } else {
                colors.text_primary
            },
            transform * Affine::translate((rect.x0 + 2.0, y)),
            None,
            TextWeight::Bold,
        );
        let (number_w, _) = text.measure_weighted(&number, 12.0, None, TextWeight::Bold);
        text.draw_text_weighted(
            scene,
            slide.layout.label(),
            11.5,
            colors.text_dim,
            transform * Affine::translate((rect.x0 + f64::from(number_w) + 12.0, y + 0.5)),
            None,
            TextWeight::Regular,
        );
    }
}

pub(crate) fn scrollbar_rects(viewport: Rect, scroll: f64, max: f64) -> Option<(Rect, Rect)> {
    if max <= 0.0 {
        return None;
    }
    let track = Rect::new(
        viewport.x1 - 10.0,
        viewport.y0 + 8.0,
        viewport.x1 - 4.0,
        viewport.y1 - 8.0,
    );
    let content = viewport.height() + max;
    let thumb_h = (track.height() * viewport.height() / content)
        .max(32.0)
        .min(track.height());
    let travel = track.height() - thumb_h;
    let y = track.y0 + travel * (scroll / max).clamp(0.0, 1.0);
    Some((track, Rect::new(track.x0, y, track.x1, y + thumb_h)))
}

pub(crate) fn scroll_from_thumb_drag(viewport: Rect, original: f64, max: f64, dy: f64) -> f64 {
    let Some((track, thumb)) = scrollbar_rects(viewport, original, max) else {
        return 0.0;
    };
    let travel = track.height() - thumb.height();
    if travel <= 0.0 {
        0.0
    } else {
        (original + dy * max / travel).clamp(0.0, max)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    #[test]
    fn geometry_reflows_and_scroll_clamps() {
        let mut grid = OverviewGrid::new();
        grid.set_scroll(1.0e6);
        grid.clamp(Rect::new(0.0, 0.0, 700.0, 500.0), 20);
        let (_, cards, max) = grid.geometry(Rect::new(0.0, 0.0, 700.0, 500.0), 20);
        assert_eq!(cards.len(), 20);
        assert_eq!(grid.scroll(), max);
    }
}
