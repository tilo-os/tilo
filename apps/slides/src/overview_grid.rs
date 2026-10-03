//! Slide-sorter overview: every slide as a media card in a scrolling grid
//! under the window's header row.

use std::cell::Cell;

use vello::kurbo::{Point, Rect};
use vello::peniko::{BlendMode, Fill};

use ui::TextRole;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    Interaction, MediaArt, MediaAspect, MediaCardFace, MediaSize, ScrollMetrics, Scrollbar,
};

use crate::model::{Deck, ElementKind, SLIDE_H, SLIDE_W, Slide};
use crate::render::{self, DeckAssets};

/// The theme's measures the renderer-free geometry lays out with, captured
/// at the last draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GridMetrics {
    /// The header row across the top of the window.
    pub(crate) header_h: f64,
    /// The content gutter round the grid.
    gutter: f64,
    column_gap: f64,
    row_gap: f64,
    /// Narrowest a card column may be.
    min_card_w: f64,
    /// Height of a card's text under its art.
    text_h: f64,
    /// Room the scroll bar keeps on the trailing edge, and its inset.
    bar_gutter: f64,
    bar_inset: f64,
}

impl GridMetrics {
    fn of(theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        let rhythm = &sp.rhythm;
        let line = |role: TextRole| f64::from(theme.text(role).line_height_px());
        Self {
            header_h: f64::from(theme.tokens.metrics.header_height)
                .max(f64::from(theme.tokens.metrics.min_pointer_target) + 2.0 * f64::from(sp.s3)),
            gutter: f64::from(sp.s6),
            column_gap: f64::from(rhythm.section),
            row_gap: f64::from(rhythm.group),
            min_card_w: MediaSize::Regular.extent(theme),
            text_h: f64::from(sp.s2) + line(TextRole::Body) + line(TextRole::Caption),
            bar_gutter: f64::from(theme.tokens.metrics.scrollbar_gutter),
            bar_inset: f64::from(sp.s1),
        }
    }
}

pub(crate) struct OverviewGrid {
    scroll: f64,
    metrics: Cell<GridMetrics>,
}

impl OverviewGrid {
    pub(crate) fn new() -> Self {
        Self {
            scroll: 0.0,
            metrics: Cell::new(GridMetrics::of(&Theme::dark())),
        }
    }

    pub(crate) fn scroll(&self) -> f64 {
        self.scroll
    }

    pub(crate) fn set_scroll(&mut self, value: f64) {
        self.scroll = value;
    }

    /// The header row across the top of the window.
    pub(crate) fn header(&self, window: Rect) -> Rect {
        Rect::new(
            window.x0,
            window.y0,
            window.x1,
            window.y0 + self.metrics.get().header_h,
        )
    }

    /// The scrolling area under the header, every card's rect (its art and
    /// text), and the furthest the area scrolls.
    pub(crate) fn geometry(&self, window: Rect, count: usize) -> (Rect, Vec<(usize, Rect)>, f64) {
        let m = self.metrics.get();
        let area = Rect::new(window.x0, window.y0 + m.header_h, window.x1, window.y1);
        let avail_w = (area.width() - 2.0 * m.gutter).max(1.0);
        let cols = ui::widget::card_columns(avail_w, m.min_card_w, m.column_gap).max(1);
        let width = (avail_w - (cols as f64 - 1.0) * m.column_gap) / cols as f64;
        let height = width * SLIDE_H / SLIDE_W + m.text_h;
        let rows = if count == 0 { 0 } else { count.div_ceil(cols) };
        let content_h = if rows == 0 {
            0.0
        } else {
            2.0 * m.gutter + rows as f64 * height + (rows as f64 - 1.0) * m.row_gap
        };
        let max_scroll = (content_h - area.height()).max(0.0);
        let scroll = self.scroll.clamp(0.0, max_scroll);
        let cards = (0..count)
            .map(|index| {
                let col = index % cols;
                let row = index / cols;
                let x = area.x0 + m.gutter + col as f64 * (width + m.column_gap);
                let y = area.y0 + m.gutter + row as f64 * (height + m.row_gap) - scroll;
                (index, Rect::new(x, y, x + width, y + height))
            })
            .collect();
        (area, cards, max_scroll)
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

    pub(crate) fn slide_at(&self, window: Rect, count: usize, point: Point) -> Option<usize> {
        self.geometry(window, count)
            .1
            .into_iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(index, _)| index)
    }

    /// The scroll bar's track and thumb over `area`, when it scrolls.
    pub(crate) fn scrollbar_rects(
        &self,
        area: Rect,
        scroll: f64,
        max: f64,
    ) -> Option<(Rect, Rect)> {
        if max <= 0.0 {
            return None;
        }
        let m = self.metrics.get();
        let track = Rect::new(
            area.x1 - m.bar_gutter + m.bar_inset,
            area.y0 + m.bar_inset,
            area.x1 - m.bar_inset,
            area.y1 - m.bar_inset,
        );
        let thumb = Scrollbar::vertical().thumb_rect(
            track,
            ScrollMetrics::new(area.height() + max, area.height(), scroll),
        )?;
        Some((track, thumb))
    }

    /// The scroll a thumb drag of `dy` from `original` asks for.
    pub(crate) fn scroll_from_thumb_drag(
        &self,
        area: Rect,
        original: f64,
        max: f64,
        dy: f64,
    ) -> f64 {
        let Some((track, thumb)) = self.scrollbar_rects(area, original, max) else {
            return 0.0;
        };
        let travel = track.height() - thumb.height();
        if travel <= 0.0 {
            0.0
        } else {
            (original + dy * max / travel).clamp(0.0, max)
        }
    }

    /// Paint the grid (the header row's controls are the app's).
    pub(crate) fn draw(
        &self,
        ctx: &mut PaintCtx<'_>,
        window: Rect,
        deck: &Deck,
        current: usize,
        hovered: Option<Point>,
        assets: DeckAssets<'_>,
    ) {
        let theme = ctx.theme;
        self.measure(ctx.text, theme, window, deck);
        let header = self.header(window);
        ui::surface::separator(
            ctx.scene,
            ctx.transform,
            header.x0,
            header.x1,
            header.y1,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        let (area, cards, max) = self.geometry(window, deck.slides.len());
        let radius = f64::from(theme.tokens.radii.control);
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &area,
        );
        for (index, rect) in cards {
            if rect.y1 < area.y0 || rect.y0 > area.y1 {
                continue;
            }
            let Some(slide) = deck.slides.get(index) else {
                continue;
            };
            let title = headline(slide);
            let number = crate::strings::tr_args(
                "slides-slide-n",
                &[("n", ui::i18n::ArgValue::from(index + 1))],
            );
            let face = MediaCardFace::new(MediaArt::Placeholder)
                .aspect(MediaAspect::Ratio(SLIDE_W / SLIDE_H))
                .title(title.as_deref().unwrap_or(number.as_str()));
            let face = if title.is_some() {
                face.meta(&number)
            } else {
                face
            };
            let face = face.selected(index == current).interaction(Interaction {
                hovered: hovered.is_some_and(|p| rect.contains(p)),
                ..Interaction::default()
            });
            face.paint(ctx, rect);
            // The card's art is the slide itself.
            let art = face.art_rect(rect);
            render::draw_slide(ctx, assets, slide, art, radius);
            render::seat_slide(ctx.scene, ctx.transform, art, radius, theme);
        }
        ctx.scene.pop_layer();
        if let Some((track, _)) = self.scrollbar_rects(area, self.scroll.clamp(0.0, max), max) {
            Scrollbar::vertical().draw(
                ctx.scene,
                ctx.transform,
                track,
                ScrollMetrics::new(
                    area.height() + max,
                    area.height(),
                    self.scroll.clamp(0.0, max),
                ),
                theme,
                false,
            );
        }
    }

    /// Capture the theme's measures and the tallest card text for the
    /// renderer-free geometry.
    fn measure(&self, text: &mut TextRenderer, theme: &Theme, window: Rect, deck: &Deck) {
        let mut m = GridMetrics::of(theme);
        self.metrics.set(m);
        let (_, cards, _) = self.geometry(window, deck.slides.len());
        let width = cards.first().map_or(m.min_card_w, |(_, r)| r.width());
        let tallest = deck
            .slides
            .iter()
            .enumerate()
            .map(|(index, slide)| {
                let number = crate::strings::tr_args(
                    "slides-slide-n",
                    &[("n", ui::i18n::ArgValue::from(index + 1))],
                );
                let title = headline(slide);
                let face = MediaCardFace::new(MediaArt::Placeholder)
                    .title(title.as_deref().unwrap_or(number.as_str()));
                let face = if title.is_some() {
                    face.meta(&number)
                } else {
                    face
                };
                face.text_height(text, theme, width)
            })
            .fold(0.0_f64, f64::max);
        if tallest > 0.0 {
            m.text_h = tallest;
        }
        self.metrics.set(m);
    }
}

/// A slide's headline: its largest text, on one line.
pub(crate) fn headline(slide: &Slide) -> Option<String> {
    slide
        .elements
        .iter()
        .filter(|e| e.kind == ElementKind::Text && !e.text.trim().is_empty())
        .max_by(|a, b| a.font_size.total_cmp(&b.font_size))
        .map(|e| e.text.split_whitespace().collect::<Vec<_>>().join(" "))
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

    #[test]
    fn a_slide_is_named_by_its_largest_text() {
        let deck = crate::model::seed_deck();
        let first = deck.slides.first().expect("a cover slide");
        assert_eq!(
            headline(first).as_deref(),
            Some("Riverside Park renewal plan")
        );
    }
}
