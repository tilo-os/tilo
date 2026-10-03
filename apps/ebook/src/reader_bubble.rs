//! Transient page-position and navigation bubble used by the reader.
//!
//! This component owns its animation, hover/reveal policy, geometry, drawing,
//! and hit-testing. The app remains responsible only for applying the emitted
//! previous/next-page command to its document-backed reading position.

use ui::time::Instant;

use vello::kurbo::{Point, Rect};
use vello::peniko::{BlendMode, Fill};

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// The leading and trailing control slot of the bubble: one pointer target
/// for each page arrow.
fn control_slot(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.min_pointer_target)
}

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
        self.animation.poke(ui::time::now());
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

    /// Draw the bubble centred in the reading `column`, standing on its
    /// bottom edge (the slot the page's foot keeps for it), and refresh the
    /// geometry consumed by [`Self::hit_test`].
    pub(crate) fn draw(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        column: Rect,
        cursor: Point,
        position: ReaderPosition,
    ) {
        self.rect = Rect::ZERO;
        self.previous_rect = Rect::ZERO;
        self.next_rect = Rect::ZERO;

        let now = ui::time::now();
        let current = (position.chapter, position.page);
        if self
            .shown_position
            .is_some_and(|previous| previous != current)
        {
            self.animation.poke(now);
        }
        self.shown_position = Some(current);

        let theme = ctx.theme;
        let slot = control_slot(theme);
        let label = label(ctx.text, theme, column.x0, column.x1, position);
        let (pill_w, pill_h) = ui::status_bubble::pill_size(&label, ctx.text, theme, slot);
        let cx = f64::midpoint(column.x0, column.x1);
        let by = column.y1 - pill_h;
        let rect = Rect::new(cx - pill_w / 2.0, by, cx + pill_w / 2.0, by + pill_h);

        let zone = f64::from(theme.tokens.spacing.s8);
        let reveal_zone = Rect::new(
            rect.x0 - zone,
            column.y1 - (pill_h + zone),
            rect.x1 + zone,
            column.y1,
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
        self.previous_rect = Rect::new(rect.x0, rect.y0, rect.x0 + slot, rect.y1);
        self.next_rect = Rect::new(rect.x1 - slot, rect.y0, rect.x1, rect.y1);
        self.draw_pill(ctx, &label, alpha, cursor);
    }

    fn draw_pill(&self, ctx: &mut PaintCtx<'_>, label: &str, alpha: f32, cursor: Point) {
        let (transform, theme) = (ctx.transform, ctx.theme);
        let PaintCtx { scene, text, .. } = ctx;
        // The fade's layer reaches as far as the panel's shadow, so the
        // shadow is never cut into a hard edge.
        let floating = &theme.tokens.elevation.floating;
        let shadow = f64::from(floating.blur) * 2.0 + f64::from(floating.offset_y).abs();
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &self.rect.inflate(shadow, shadow),
        );
        ui::status_bubble::draw_pill(scene, transform, text, theme, self.rect, label);
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let icon = f64::from(theme.tokens.metrics.icon_md);
        for (slot, glyph) in [
            (self.previous_rect, Icon::ChevronLeft),
            (self.next_rect, Icon::ChevronRight),
        ] {
            let color = if slot.contains(cursor) {
                ink.text_primary
            } else {
                ink.text_dim
            };
            glyph.draw(
                scene,
                transform,
                Rect::from_center_size(slot.center(), (icon, icon)),
                color,
            );
        }
        scene.pop_layer();
    }
}

/// "Chapter 2, page 3 of 12" when the column has room, else "3 of 12".
fn label(
    text: &mut TextRenderer,
    theme: &Theme,
    center_x0: f64,
    center_x1: f64,
    position: ReaderPosition,
) -> String {
    let page = position.page + 1;
    let pages = position.page_count.max(1);
    let base = format!("{page} of {pages}");
    let rich = format!("Chapter {}, page {page} of {pages}", position.chapter + 1);
    let column_width = center_x1 - center_x0;
    let (rich_width, _) = ui::status_bubble::pill_size(&rich, text, theme, control_slot(theme));
    if position.chapter_count > 0 && rich_width <= column_width - 2.0 * control_slot(theme) {
        rich
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hidden bubble has no navigation target; visible chevrons emit the matching navigation action.
    #[test]
    fn reader_bubble_navigation_targets_follow_visibility() {
        // --- phase: hidden_bubble_has_no_navigation_target ---
        {
            let bubble = ReaderBubble::default();
            assert_eq!(bubble.hit_test(Point::ZERO), None);
            assert_eq!(bubble.action_center(ReaderBubbleAction::NextPage), None);
        }

        // --- phase: visible_chevrons_emit_the_matching_navigation_action ---
        {
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
}
