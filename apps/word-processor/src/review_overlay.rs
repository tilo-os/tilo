//! Margin cards for comments and tracked changes.
//!
//! This component owns the materialized review items, view visibility, drawing,
//! and frame-local hit targets. The editor remains responsible for applying the
//! typed [`ReviewAction`] to the document.

use crate::paint_context::PaintContext;
use qdoc::snapshot::{ChangeKind, ChangeView, CommentView};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use ui::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
use ui::text_render::{TextRenderer, TextWeight};

use crate::i18n::t;
use crate::num;

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
/// The comment column ends `s3` short of the canvas edge — past the 8 px
/// scrollbar lane plus its `s1` clearance — and the change column sits
/// `s2` to its left. Base-grid steps: the review cards paint unscaled.
const CARD_RIGHT_INSET: f64 = ui::tokens::Spacing::BASE.s3 as f64;
const CARD_GAP: f64 = ui::tokens::Spacing::BASE.s2 as f64;
/// Horizontal padding inside a card button around its measured label.
const BUTTON_PAD_X: f64 = ui::tokens::Spacing::BASE.s1 as f64;
/// The comment-card button labels, shaped once per frame — they are the same
/// on every card, so a document with many comments never re-measures them.
/// The two type roles (renderer input; scaled once) travel with the labels
/// they size.
struct CardButtonLabels {
    reply: String,
    reply_font: f32,
    reply_size: (f64, f64),
    resolve: String,
    resolve_font: f32,
    resolve_size: (f64, f64),
}

impl CardButtonLabels {
    fn measure(text_renderer: &mut TextRenderer) -> Self {
        let reply = t("review-reply");
        let resolve = t("review-resolve");
        let (reply_font, resolve_font) = (10.0, 10.5);
        let (rw, rh) = text_renderer.measure(&reply, reply_font, None);
        let (sw, sh) = text_renderer.measure(&resolve, resolve_font, None);
        Self {
            reply,
            reply_font,
            reply_size: (f64::from(rw), f64::from(rh)),
            resolve,
            resolve_font,
            resolve_size: (f64::from(sw), f64::from(sh)),
        }
    }
}

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
        let scene = &mut *context.scene;
        let transform = context.transform;
        let text_renderer = &mut *context.text;
        self.clear_hits();
        if !self.visible {
            return;
        }

        comment_markers.sort_by(marker_order);
        // The card column stops short of the canvas scrollbar lane, so a
        // card never runs under the thumb.
        let comment_x = canvas.x1 - COMMENT_CARD_W - CARD_RIGHT_INSET;
        let labels = CardButtonLabels::measure(text_renderer);
        let mut next_comment_y = f64::MIN;
        for (id, anchor_y, range_right) in comment_markers {
            next_comment_y = self.draw_comment(
                scene,
                text_renderer,
                transform,
                comment_x,
                next_comment_y,
                anchor_y,
                range_right,
                &id,
                &labels,
            );
        }

        change_markers.sort_by(marker_order);
        let change_x = comment_x - CARD_GAP - CHANGE_CARD_W;
        let mut next_change_y = f64::MIN;
        for (id, anchor_y, _) in change_markers {
            next_change_y = self.draw_change(
                scene,
                text_renderer,
                transform,
                change_x,
                next_change_y,
                anchor_y,
                &id,
            );
        }
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "comment painting keeps text measurement, card geometry and reply hit targets together with the render context and labels"
    )]
    fn draw_comment(
        &mut self,
        scene: &mut Scene,
        text_renderer: &mut TextRenderer,
        transform: Affine,
        card_x: f64,
        next_card_y: f64,
        anchor_y: f64,
        range_right: f64,
        id: &str,
        labels: &CardButtonLabels,
    ) -> f64 {
        let Some(comment) = self.comments.iter().find(|comment| comment.id == id) else {
            return next_card_y;
        };
        let tab_x = (range_right + 2.0).min(card_x - COMMENT_TAB_W - 2.0);
        let tab = Rect::new(
            tab_x,
            anchor_y + 1.0,
            tab_x + COMMENT_TAB_W,
            anchor_y + 18.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            COMMENT_TAB,
            None,
            &vello::kurbo::RoundedRect::from_rect(tab, 1.5),
        );

        let comment_label = t("review-comment");
        let author = if comment.author.is_empty() {
            comment_label.as_str()
        } else {
            comment.author.as_str()
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
        let card_top = anchor_y.max(next_card_y);
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
        let reply_layout = RichTextLayout::shape(
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
        );
        let reply_h = if replies.is_empty() {
            0.0
        } else {
            4.0 + f64::from(reply_layout.height())
        };
        let card_h = 22.0 + f64::from(body_layout.height()) + reply_h + 8.0 + REVIEW_BTN + 6.0;
        let card = Rect::new(card_x, card_top, card_x + COMMENT_CARD_W, card_top + card_h);
        let rounded = vello::kurbo::RoundedRect::from_rect(card, 5.0);
        scene.fill(Fill::NonZero, transform, COMMENT_CARD_BG, None, &rounded);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            COMMENT_CARD_EDGE,
            None,
            &rounded,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            COMMENT_TAB,
            None,
            &Rect::new(card_x, card_top, card_x + 3.0, card_top + card_h),
        );
        text_renderer.draw_text(
            scene,
            author,
            11.5,
            COMMENT_AUTHOR_INK,
            transform * Affine::translate((card_x + 9.0, card_top + 5.0)),
            None,
        );
        text_renderer.draw_rich(
            scene,
            &body_layout,
            transform * Affine::translate((card_x + 9.0, card_top + 20.0)),
            COMMENT_BODY_INK,
        );

        if !replies.is_empty() {
            text_renderer.draw_rich(
                scene,
                &reply_layout,
                transform
                    * Affine::translate((
                        card_x + 9.0,
                        card_top + 22.0 + f64::from(body_layout.height()) + 3.0,
                    )),
                COMMENT_BODY_INK,
            );
        }

        // Resolve is trailing-aligned and sized first; Reply takes what is
        // left of the row (less the card gap) so the two faces — and their
        // hit rects — can never overlap, whatever the locale or text scale.
        let resolve = self.draw_comment_resolve_button(
            scene,
            text_renderer,
            transform,
            card_x,
            card_top,
            card_h,
            id,
            labels,
        );
        self.draw_comment_reply_button(
            scene,
            text_renderer,
            transform,
            card_x,
            card_top,
            card_h,
            resolve.x0 - CARD_GAP,
            id,
            labels,
        );
        card_top + card_h + CARD_GAP
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "reply-button painting needs render context, card geometry, remaining row width, comment identity and measured labels"
    )]
    fn draw_comment_reply_button(
        &mut self,
        scene: &mut Scene,
        text_renderer: &mut TextRenderer,
        transform: Affine,
        card_x: f64,
        card_top: f64,
        card_h: f64,
        max_x1: f64,
        id: &str,
        labels: &CardButtonLabels,
    ) {
        let button_y = card_top + card_h - REVIEW_BTN - 4.0;
        // The button is its measured label plus padding, so a localised
        // label never overruns the face it sits on; when the card is too
        // narrow for both buttons the face stops at `max_x1` and the label
        // is elided to what fits rather than running under Resolve.
        let x0 = card_x + 8.0;
        let (label_w, label_h) = labels.reply_size;
        let width = (label_w + 2.0 * BUTTON_PAD_X + 2.0).min((max_x1 - x0).max(0.0));
        let label = text_renderer.elide_to_width_weighted(
            &labels.reply,
            labels.reply_font,
            num::f32_of_f64((width - 2.0 * BUTTON_PAD_X).max(0.0)),
            TextWeight::Regular,
        );
        let (label_w, _) = text_renderer.measure(&label, labels.reply_font, None);
        let label_w = f64::from(label_w);
        let button = Rect::new(x0, button_y, x0 + width, button_y + REVIEW_BTN);
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
            &label,
            labels.reply_font,
            COMMENT_AUTHOR_INK,
            transform
                * Affine::translate((
                    button.x0 + (width - label_w) / 2.0,
                    button.y0 + (REVIEW_BTN - label_h) / 2.0,
                )),
            None,
        );
        self.hits
            .push((button, ReviewAction::OpenCommentThread(id.to_string())));
    }

    /// The Resolve button on a comment card: its rect, chrome, label and
    /// hit entry. Split out of [`Self::draw_comment`] to keep that under the
    /// line limit; the card's own geometry is already decided by then.
    #[allow(
        clippy::too_many_arguments,
        reason = "the paint context (scene/text_renderer/transform) plus the card geometry \
                  its caller already computed; the same bundle every draw_* here carries"
    )]
    fn draw_comment_resolve_button(
        &mut self,
        scene: &mut Scene,
        text_renderer: &mut TextRenderer,
        transform: Affine,
        card_x: f64,
        card_top: f64,
        card_h: f64,
        id: &str,
        labels: &CardButtonLabels,
    ) -> Rect {
        let button_y = card_top + card_h - REVIEW_BTN - 4.0;
        // Trailing-aligned inside the card, sized to its measured label so
        // "✓ Resolve" (and its longer translations) ends inside the face.
        let (label_w, label_h) = labels.resolve_size;
        let width = label_w + 2.0 * BUTTON_PAD_X + 2.0;
        let x1 = card_x + COMMENT_CARD_W - 8.0;
        let button = Rect::new(x1 - width, button_y, x1, button_y + REVIEW_BTN);
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
            &labels.resolve,
            labels.resolve_font,
            REVIEW_ACCEPT_INK,
            transform
                * Affine::translate((
                    button.x0 + (width - label_w) / 2.0,
                    button.y0 + (REVIEW_BTN - label_h) / 2.0,
                )),
            None,
        );
        self.hits
            .push((button, ReviewAction::ResolveComment(id.to_string())));
        button
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "change-card painting combines render context, document anchor, card stack position and change identity"
    )]
    fn draw_change(
        &mut self,
        scene: &mut Scene,
        text_renderer: &mut TextRenderer,
        transform: Affine,
        card_x: f64,
        next_card_y: f64,
        anchor_y: f64,
        id: &str,
    ) -> f64 {
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
        card_top + card_h + CARD_GAP
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
