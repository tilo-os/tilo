//! Transient page-position and navigation bubble used by the reader.
//!
//! This component owns its animation, hover/reveal policy, geometry, drawing,
//! and hit-testing. The app remains responsible only for applying the emitted
//! previous/next-page command to its document-backed reading position.

use std::time::Instant;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};

use ui::text_render::TextRenderer;
use ui::theme::Theme;

const TEXT_SIZE: f32 = 12.5;
const CHEVRON_SLOT: f64 = 26.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReaderBubbleAction {
    PreviousPage,
    NextPage,
}

#[derive(Clone, Copy)]
pub(crate) struct ReaderPosition {
    pub(crate) chapter: usize,
    pub(crate) page: usize,
    pub(crate) page_count: usize,
    pub(crate) chapter_count: usize,
}

pub(crate) struct ReaderBubble {
    animation: ui::StatusBubble,
    shown_position: Option<(usize, usize)>,
    rect: Rect,
    previous_rect: Rect,
    next_rect: Rect,
    force_open: bool,
}

impl Default for ReaderBubble {
    fn default() -> Self {
        Self {
            animation: ui::StatusBubble::new(),
            shown_position: None,
            rect: Rect::ZERO,
            previous_rect: Rect::ZERO,
            next_rect: Rect::ZERO,
            force_open: false,
        }
    }
}

impl ReaderBubble {
    /// Keep the otherwise transient bubble visible in deterministic screenshots.
    pub(crate) fn arm_for_shots(&mut self) {
        self.force_open = true;
        self.animation.poke(Instant::now());
    }

    pub(crate) fn needs_frame(&self, now: Instant) -> bool {
        self.animation.needs_frame(now)
    }

    /// Resolve a click against the visible chevron slots. Hidden bubbles clear
    /// these rects during drawing, so they can never consume page content.
    pub(crate) fn hit_test(&self, point: Point) -> Option<ReaderBubbleAction> {
        if self.previous_rect.contains(point) {
            Some(ReaderBubbleAction::PreviousPage)
        } else if self.next_rect.contains(point) {
            Some(ReaderBubbleAction::NextPage)
        } else {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn action_center(&self, action: ReaderBubbleAction) -> Option<Point> {
        let rect = match action {
            ReaderBubbleAction::PreviousPage => self.previous_rect,
            ReaderBubbleAction::NextPage => self.next_rect,
        };
        (rect != Rect::ZERO).then(|| rect.center())
    }

    /// Draw the bubble and refresh the geometry consumed by [`Self::hit_test`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        cursor: Point,
        center_x0: f64,
        center_x1: f64,
        position: ReaderPosition,
    ) {
        self.rect = Rect::ZERO;
        self.previous_rect = Rect::ZERO;
        self.next_rect = Rect::ZERO;

        let now = Instant::now();
        let current = (position.chapter, position.page);
        if self
            .shown_position
            .is_some_and(|previous| previous != current)
        {
            self.animation.poke(now);
        }
        self.shown_position = Some(current);

        let label = label(text, center_x0, center_x1, position);
        let (pill_w, pill_h) = ui::status_bubble::pill_size(&label, text, TEXT_SIZE, CHEVRON_SLOT);
        let cx = f64::midpoint(center_x0, center_x1);
        let by = viewport.y1 - 16.0 - pill_h;
        let rect = Rect::new(cx - pill_w / 2.0, by, cx + pill_w / 2.0, by + pill_h);

        let reveal_zone = Rect::new(
            rect.x0 - 40.0,
            viewport.y1 - (pill_h + 32.0),
            rect.x1 + 40.0,
            viewport.y1,
        );
        self.animation.set_hover(
            self.force_open || reveal_zone.contains(cursor) || rect.contains(cursor),
            now,
        );

        let alpha = self.animation.alpha(now);
        if alpha <= 0.0 {
            return;
        }

        self.rect = rect;
        self.previous_rect = Rect::new(rect.x0, rect.y0, rect.x0 + CHEVRON_SLOT + 8.0, rect.y1);
        self.next_rect = Rect::new(rect.x1 - CHEVRON_SLOT - 8.0, rect.y0, rect.x1, rect.y1);
        self.draw_pill(scene, transform, text, theme, &label, alpha, cursor);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_pill(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        label: &str,
        alpha: f32,
        cursor: Point,
    ) {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &self.rect.inflate(12.0, 14.0),
        );
        ui::status_bubble::draw_pill(scene, transform, text, theme, self.rect, label, TEXT_SIZE);
        let cy = f64::midpoint(self.rect.y0, self.rect.y1);
        let previous_icon = Rect::new(self.rect.x0 + 8.0, cy - 8.0, self.rect.x0 + 24.0, cy + 8.0);
        let next_icon = Rect::new(self.rect.x1 - 24.0, cy - 8.0, self.rect.x1 - 8.0, cy + 8.0);
        ui::status_bubble::draw_chevron(
            scene,
            transform,
            previous_icon,
            if self.previous_rect.contains(cursor) {
                theme.text_primary
            } else {
                theme.text_dim
            },
            true,
        );
        ui::status_bubble::draw_chevron(
            scene,
            transform,
            next_icon,
            if self.next_rect.contains(cursor) {
                theme.text_primary
            } else {
                theme.text_dim
            },
            false,
        );
        scene.pop_layer();
    }
}

fn label(
    text: &mut TextRenderer,
    center_x0: f64,
    center_x1: f64,
    position: ReaderPosition,
) -> String {
    let base = format!("{} / {}", position.page + 1, position.page_count.max(1));
    let rich = format!(
        "{}   ·   Chapter {} of {}",
        base,
        position.chapter + 1,
        position.chapter_count.max(1)
    );
    let column_width = (center_x1 - center_x0).max(120.0);
    let (rich_width, _) = ui::status_bubble::pill_size(&rich, text, TEXT_SIZE, CHEVRON_SLOT);
    if position.chapter_count > 0 && rich_width <= column_width - 48.0 {
        rich
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_bubble_has_no_navigation_target() {
        let bubble = ReaderBubble::default();
        assert_eq!(bubble.hit_test(Point::ZERO), None);
        assert_eq!(bubble.action_center(ReaderBubbleAction::NextPage), None);
    }

    #[test]
    fn visible_chevrons_emit_the_matching_navigation_action() {
        let bubble = ReaderBubble {
            previous_rect: Rect::new(10.0, 10.0, 30.0, 30.0),
            next_rect: Rect::new(50.0, 10.0, 70.0, 30.0),
            ..ReaderBubble::default()
        };
        assert_eq!(
            bubble.hit_test(Point::new(20.0, 20.0)),
            Some(ReaderBubbleAction::PreviousPage)
        );
        assert_eq!(
            bubble.hit_test(Point::new(60.0, 20.0)),
            Some(ReaderBubbleAction::NextPage)
        );
        assert_eq!(bubble.hit_test(Point::new(40.0, 20.0)), None);
    }
}
