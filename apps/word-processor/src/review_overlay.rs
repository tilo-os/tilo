//! Margin cards for comments and tracked changes.
//!
//! This component owns the materialized review items, view visibility, drawing,
//! and frame-local hit targets. The editor remains responsible for applying the
//! typed [`ReviewAction`] to the document.

use qdoc::snapshot::{ChangeKind, ChangeView, CommentView};
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use crate::i18n::t;
use crate::num;
use crate::paint_context::PaintContext;
use ui::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};

/// Pale-yellow band drawn behind a comment-marked body range.
pub(super) const COMMENT_HIGHLIGHT: Color = Color::from_rgba8(0xff, 0xe5, 0x7a, 0x66);
pub(super) const INSERTION_RGB: [u8; 3] = [0x1d, 0x7a, 0x3a];
pub(super) const DELETION_RGB: [u8; 3] = [0xbf, 0x29, 0x29];
/// Review colour for tracked insertions.
pub(super) const INSERTION_INK: Color =
    Color::from_rgb8(INSERTION_RGB[0], INSERTION_RGB[1], INSERTION_RGB[2]);
/// Review colour for tracked deletions.
pub(super) const DELETION_INK: Color =
    Color::from_rgb8(DELETION_RGB[0], DELETION_RGB[1], DELETION_RGB[2]);

const COMMENT_TAB: Color = Color::from_rgb8(0xe0, 0xa8, 0x10);
const COMMENT_CARD_BG: Color = Color::from_rgb8(0xff, 0xf8, 0xdc);
const COMMENT_CARD_EDGE: Color = Color::from_rgb8(0xe6, 0xcf, 0x7a);
const COMMENT_AUTHOR_INK: Color = Color::from_rgb8(0x6a, 0x52, 0x00);
const COMMENT_BODY_INK: Color = Color::from_rgb8(0x3a, 0x32, 0x14);
const COMMENT_TAB_W: f64 = 4.0;
const COMMENT_CARD_W: f64 = 150.0;
const CHANGE_CARD_BG: Color = Color::from_rgb8(0xf3, 0xf4, 0xf7);
const CHANGE_CARD_EDGE: Color = Color::from_rgb8(0xcf, 0xd2, 0xda);
const CHANGE_LABEL_INK: Color = Color::from_rgb8(0x44, 0x46, 0x50);
const REVIEW_ACCEPT_BG: Color = Color::from_rgb8(0xe2, 0xf3, 0xe6);
const REVIEW_ACCEPT_INK: Color = Color::from_rgb8(0x18, 0x6a, 0x32);
const REVIEW_REJECT_BG: Color = Color::from_rgb8(0xf7, 0xe4, 0xe4);
const REVIEW_REJECT_INK: Color = Color::from_rgb8(0xa8, 0x24, 0x24);
const REVIEW_BTN: f64 = 18.0;
const CHANGE_CARD_W: f64 = 132.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReviewAction {
    AcceptChange(String),
    RejectChange(String),
    AcceptAllChanges,
    RejectAllChanges,
    ResolveComment(String),
    /// Opens the editor-owned durable reply composer for this comment thread.
    OpenCommentThread(String),
}

pub(super) struct ReviewOverlay {
    comments: Vec<CommentView>,
    changes: Vec<ChangeView>,
    visible: bool,
    hits: Vec<(Rect, ReviewAction)>,
}

struct CommentCardLayout {
    author: String,
    body: RichTextLayout,
    replies: Option<RichTextLayout>,
    rect: Rect,
}

impl Default for ReviewOverlay {
    fn default() -> Self {
        Self {
            comments: Vec::new(),
            changes: Vec::new(),
            visible: true,
            hits: Vec::new(),
        }
    }
}

impl ReviewOverlay {
    pub(super) fn replace(&mut self, comments: Vec<CommentView>, changes: Vec<ChangeView>) {
        self.comments = comments;
        self.changes = changes;
    }

    pub(super) fn comments(&self) -> &[CommentView] {
        &self.comments
    }

    pub(super) fn changes(&self) -> &[ChangeView] {
        &self.changes
    }

    pub(super) fn is_visible(&self) -> bool {
        self.visible
    }

    pub(super) fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    pub(super) fn clear_hits(&mut self) {
        self.hits.clear();
    }

    pub(super) fn action_at(&self, point: Point) -> Option<ReviewAction> {
        self.hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, action)| action.clone())
    }

    pub(super) fn hits(&self) -> &[(Rect, ReviewAction)] {
        &self.hits
    }

    pub(super) fn draw(
        &mut self,
        context: &mut PaintContext<'_>,
        canvas: Rect,
        mut comment_markers: Vec<(String, f64, f64)>,
        mut change_markers: Vec<(String, f64, f64)>,
    ) {
        self.clear_hits();
        if !self.visible {
            return;
        }

        comment_markers.sort_by(marker_order);
        let comment_x = canvas.x1 - COMMENT_CARD_W - 6.0;
        let mut next_comment_y = f64::MIN;
        for (id, anchor_y, range_right) in comment_markers {
            next_comment_y = self.draw_comment(
                context,
                comment_x,
                next_comment_y,
                anchor_y,
                range_right,
                &id,
            );
        }

        change_markers.sort_by(marker_order);
        let change_x = canvas.x1 - COMMENT_CARD_W - CHANGE_CARD_W - 12.0;
        let mut next_change_y = f64::MIN;
        for (id, anchor_y, _) in change_markers {
            next_change_y = self.draw_change(context, change_x, next_change_y, anchor_y, &id);
        }
    }

    fn draw_comment(
        &mut self,
        context: &mut PaintContext<'_>,
        card_x: f64,
        next_card_y: f64,
        anchor_y: f64,
        range_right: f64,
        id: &str,
    ) -> f64 {
        let Some(comment) = self.comments.iter().find(|comment| comment.id == id) else {
            return next_card_y;
        };
        let card_top = anchor_y.max(next_card_y);
        let layout = Self::layout_comment(context.text, comment, card_x, card_top);
        let tab_x = (range_right + 2.0).min(card_x - COMMENT_TAB_W - 2.0);
        let tab = Rect::new(
            tab_x,
            anchor_y + 1.0,
            tab_x + COMMENT_TAB_W,
            anchor_y + 18.0,
        );
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            COMMENT_TAB,
            None,
            &vello::kurbo::RoundedRect::from_rect(tab, 1.5),
        );

        Self::draw_comment_card(context, &layout);
        self.draw_comment_reply_button(context, card_x, card_top, layout.rect.height(), id);
        self.draw_comment_resolve_button(context, card_x, card_top, layout.rect.height(), id);
        layout.rect.y1 + 6.0
    }

    fn layout_comment(
        text_renderer: &mut ui::text_render::TextRenderer,
        comment: &CommentView,
        card_x: f64,
        card_top: f64,
    ) -> CommentCardLayout {
        let author = if comment.author.is_empty() {
            t("review-comment")
        } else {
            comment.author.clone()
        };
        let body = comment
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let body_layout = RichTextLayout::shape(
            &mut text_renderer.font_system,
            &[RichSpan::new(&body)],
            false,
            11.0,
            num::f32_of_f64(COMMENT_CARD_W - 16.0),
            TextAlign::Left,
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
        );
        let replies: Vec<String> = comment
            .replies
            .iter()
            .take(2)
            .map(|reply| {
                let text = reply
                    .blocks
                    .iter()
                    .map(|block| block.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{}: {}", reply.author, text)
            })
            .collect();
        let reply_text = replies.join("\n");
        let reply_layout = (!replies.is_empty()).then(|| {
            RichTextLayout::shape(
                &mut text_renderer.font_system,
                &[RichSpan::new(&reply_text)],
                false,
                10.0,
                num::f32_of_f64(COMMENT_CARD_W - 16.0),
                TextAlign::Left,
                FontFamily::Sans,
                vec![],
                vec![],
                1.0,
            )
        });
        let reply_h = reply_layout
            .as_ref()
            .map_or(0.0, |layout| 4.0 + f64::from(layout.height()));
        let card_h = 22.0 + f64::from(body_layout.height()) + reply_h + 8.0 + REVIEW_BTN + 6.0;
        CommentCardLayout {
            author,
            body: body_layout,
            replies: reply_layout,
            rect: Rect::new(card_x, card_top, card_x + COMMENT_CARD_W, card_top + card_h),
        }
    }

    fn draw_comment_card(context: &mut PaintContext<'_>, layout: &CommentCardLayout) {
        let rounded = vello::kurbo::RoundedRect::from_rect(layout.rect, 5.0);
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            COMMENT_CARD_BG,
            None,
            &rounded,
        );
        context.scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            context.transform,
            COMMENT_CARD_EDGE,
            None,
            &rounded,
        );
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            COMMENT_TAB,
            None,
            &Rect::new(
                layout.rect.x0,
                layout.rect.y0,
                layout.rect.x0 + 3.0,
                layout.rect.y1,
            ),
        );
        context.text.draw_text(
            context.scene,
            &layout.author,
            11.5,
            COMMENT_AUTHOR_INK,
            context.transform * Affine::translate((layout.rect.x0 + 9.0, layout.rect.y0 + 5.0)),
            None,
        );
        context.text.draw_rich(
            context.scene,
            &layout.body,
            context.transform * Affine::translate((layout.rect.x0 + 9.0, layout.rect.y0 + 20.0)),
            COMMENT_BODY_INK,
        );
        if let Some(replies) = &layout.replies {
            context.text.draw_rich(
                context.scene,
                replies,
                context.transform
                    * Affine::translate((
                        layout.rect.x0 + 9.0,
                        layout.rect.y0 + 22.0 + f64::from(layout.body.height()) + 3.0,
                    )),
                COMMENT_BODY_INK,
            );
        }
    }

    fn draw_comment_reply_button(
        &mut self,
        context: &mut PaintContext<'_>,
        card_x: f64,
        card_top: f64,
        card_h: f64,
        id: &str,
    ) {
        let scene = &mut *context.scene;
        let text_renderer = &mut *context.text;
        let transform = context.transform;
        let button_y = card_top + card_h - REVIEW_BTN - 4.0;
        let button = Rect::new(card_x + 8.0, button_y, card_x + 51.0, button_y + REVIEW_BTN);
        scene.fill(
            Fill::NonZero,
            transform,
            COMMENT_CARD_BG,
            None,
            &vello::kurbo::RoundedRect::from_rect(button, 4.0),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            COMMENT_CARD_EDGE,
            None,
            &vello::kurbo::RoundedRect::from_rect(button, 4.0),
        );
        text_renderer.draw_text(
            scene,
            &t("review-reply"),
            10.0,
            COMMENT_AUTHOR_INK,
            transform * Affine::translate((button.x0 + 5.0, button.y0 + 2.5)),
            None,
        );
        self.hits
            .push((button, ReviewAction::OpenCommentThread(id.to_string())));
    }

    /// The Resolve button on a comment card: its rect, chrome, label and
    /// hit entry. Split out of [`Self::draw_comment`] to keep that under the
    /// line limit; the card's own geometry is already decided by then.
    fn draw_comment_resolve_button(
        &mut self,
        context: &mut PaintContext<'_>,
        card_x: f64,
        card_top: f64,
        card_h: f64,
        id: &str,
    ) {
        let scene = &mut *context.scene;
        let text_renderer = &mut *context.text;
        let transform = context.transform;
        let button_y = card_top + card_h - REVIEW_BTN - 4.0;
        let button = Rect::new(
            card_x + COMMENT_CARD_W - 64.0,
            button_y,
            card_x + COMMENT_CARD_W - 8.0,
            button_y + REVIEW_BTN,
        );
        let rounded_button = vello::kurbo::RoundedRect::from_rect(button, 4.0);
        scene.fill(
            Fill::NonZero,
            transform,
            REVIEW_ACCEPT_BG,
            None,
            &rounded_button,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            COMMENT_CARD_EDGE,
            None,
            &rounded_button,
        );
        text_renderer.draw_text(
            scene,
            &t("review-resolve"),
            10.5,
            REVIEW_ACCEPT_INK,
            transform * Affine::translate((button.x0 + 5.0, button.y0 + 2.5)),
            None,
        );
        self.hits
            .push((button, ReviewAction::ResolveComment(id.to_string())));
    }

    fn draw_change(
        &mut self,
        context: &mut PaintContext<'_>,
        card_x: f64,
        next_card_y: f64,
        anchor_y: f64,
        id: &str,
    ) -> f64 {
        let scene = &mut *context.scene;
        let text_renderer = &mut *context.text;
        let transform = context.transform;
        let Some(change) = self.changes.iter().find(|change| change.id == id) else {
            return next_card_y;
        };
        let (label, kind_color) = match change.kind {
            ChangeKind::Ins => (t("review-insertion"), INSERTION_INK),
            ChangeKind::Del => (t("review-deletion"), DELETION_INK),
        };
        let author = if change.author.is_empty() {
            String::new()
        } else {
            format!(" · {}", change.author)
        };
        let card_top = anchor_y.max(next_card_y);
        let card_h = 22.0 + REVIEW_BTN + 6.0;
        let card = Rect::new(card_x, card_top, card_x + CHANGE_CARD_W, card_top + card_h);
        let rounded = vello::kurbo::RoundedRect::from_rect(card, 5.0);
        scene.fill(Fill::NonZero, transform, CHANGE_CARD_BG, None, &rounded);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            CHANGE_CARD_EDGE,
            None,
            &rounded,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            kind_color,
            None,
            &Rect::new(card_x, card_top, card_x + 3.0, card_top + card_h),
        );
        text_renderer.draw_text(
            scene,
            &format!("{label}{author}"),
            10.5,
            CHANGE_LABEL_INK,
            transform * Affine::translate((card_x + 9.0, card_top + 5.0)),
            None,
        );

        let button_y = card_top + card_h - REVIEW_BTN - 4.0;
        let accept = Rect::new(
            card_x + 9.0,
            button_y,
            card_x + 9.0 + REVIEW_BTN,
            button_y + REVIEW_BTN,
        );
        let reject = Rect::new(
            accept.x1 + 6.0,
            button_y,
            accept.x1 + 6.0 + REVIEW_BTN,
            button_y + REVIEW_BTN,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            REVIEW_ACCEPT_BG,
            None,
            &vello::kurbo::RoundedRect::from_rect(accept, 4.0),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            REVIEW_REJECT_BG,
            None,
            &vello::kurbo::RoundedRect::from_rect(reject, 4.0),
        );
        text_renderer.draw_text(
            scene,
            "✓",
            12.0,
            REVIEW_ACCEPT_INK,
            transform * Affine::translate((accept.x0 + 4.0, accept.y0 + 1.5)),
            None,
        );
        text_renderer.draw_text(
            scene,
            "✗",
            12.0,
            REVIEW_REJECT_INK,
            transform * Affine::translate((reject.x0 + 4.0, reject.y0 + 1.5)),
            None,
        );
        self.hits
            .push((accept, ReviewAction::AcceptChange(id.to_string())));
        self.hits
            .push((reject, ReviewAction::RejectChange(id.to_string())));
        card_top + card_h + 6.0
    }
}

fn marker_order(a: &(String, f64, f64), b: &(String, f64, f64)) -> std::cmp::Ordering {
    a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_overlay_never_routes_stale_hits() {
        let mut overlay = ReviewOverlay::default();
        overlay.hits.push((
            Rect::new(0.0, 0.0, 20.0, 20.0),
            ReviewAction::AcceptChange("change".into()),
        ));
        overlay.toggle();
        overlay.clear_hits();

        assert!(!overlay.is_visible());
        assert_eq!(overlay.action_at(Point::new(10.0, 10.0)), None);
    }

    #[test]
    fn topmost_review_hit_wins() {
        let mut overlay = ReviewOverlay::default();
        let rect = Rect::new(0.0, 0.0, 20.0, 20.0);
        overlay
            .hits
            .push((rect, ReviewAction::AcceptChange("under".into())));
        overlay
            .hits
            .push((rect, ReviewAction::RejectChange("over".into())));

        assert_eq!(
            overlay.action_at(Point::new(10.0, 10.0)),
            Some(ReviewAction::RejectChange("over".into()))
        );
    }
}
