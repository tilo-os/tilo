//! Slide-sorter overview grid.

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{BlendMode, Color, Fill};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::assets::ImageStore;
use crate::chrome;
use crate::model::{Deck, Palette, SLIDE_H, SLIDE_W};
use crate::render;

pub(crate) struct OverviewGrid {
    scroll: f64,
}

impl OverviewGrid {
    pub(crate) fn new() -> Self {
        Self { scroll: 0.0 }
    }

    pub(crate) fn scroll(&self) -> f64 {
        self.scroll
    }

    pub(crate) fn set_scroll(&mut self, value: f64) {
        self.scroll = value;
    }

    pub(crate) fn geometry(&self, window: Rect, count: usize) -> (Rect, Vec<(usize, Rect)>, f64) {
        let area = Rect::new(window.x0, window.y0 + 64.0, window.x1, window.y1);
        let pad = 36.0;
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
        deck: &Deck,
        current: usize,
        palette: &Palette,
        images: &ImageStore,
    ) {
        let colors = &theme.tokens.colors;
        chrome::fill_rect(scene, transform, window, colors.surfaces.base);
        text.draw_text_weighted(
            scene,
            &deck.title,
            18.0,
            colors.text_primary,
            transform * Affine::translate((window.x0 + 36.0, window.y0 + 16.0)),
            None,
            TextWeight::Bold,
        );
        text.draw_text_weighted(
            scene,
            &format!(
                "{} slides  ·  click to open  ·  Esc / G to return",
                deck.slides.len()
            ),
            12.0,
            colors.text_dim,
            transform * Affine::translate((window.x0 + 36.0, window.y0 + 38.0)),
            None,
            TextWeight::Regular,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &vello::kurbo::Line::new((window.x0, window.y0 + 60.0), (window.x1, window.y0 + 60.0)),
        );
        let (area, thumbs, max) = self.geometry(window, deck.slides.len());
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &area);
        for (index, rect) in thumbs {
            let Some(slide) = deck.slides.get(index) else {
                continue;
            };
            chrome::fill_rrect(
                scene,
                transform,
                (rect + vello::kurbo::Vec2::new(0.0, 4.0)).inflate(1.0, 1.0),
                8.0,
                render::with_alpha(Color::BLACK, 60),
            );
            render::draw_slide(
                scene, transform, text, theme, palette, slide, rect, 8.0, images,
            );
            chrome::stroke_rrect(
                scene,
                transform,
                rect,
                8.0,
                if index == current {
                    palette.accent
                } else {
                    render::with_alpha(colors.text_primary, 30)
                },
                if index == current { 2.5 } else { 1.0 },
            );
            let number = format!("{}", index + 1);
            let y = rect.y1 + 7.0;
            text.draw_text_weighted(
                scene,
                &number,
                12.0,
                if index == current {
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
        scene.pop_layer();
        if let Some((track, thumb)) = scrollbar_rects(area, self.scroll.clamp(0.0, max), max) {
            chrome::fill_rrect(
                scene,
                transform,
                track,
                3.0,
                render::with_alpha(colors.text_primary, 18),
            );
            chrome::fill_rrect(
                scene,
                transform,
                thumb,
                3.0,
                render::with_alpha(colors.text_primary, 90),
            );
        }
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
