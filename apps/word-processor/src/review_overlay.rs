//! Margin cards for comments and tracked changes.
//!
//! This component owns the materialized review items, view visibility, drawing,
//! and frame-local hit targets. The editor remains responsible for applying the
//! typed [`ReviewAction`] to the document.

use crate::paint_context::PaintContext;
use qdoc::snapshot::{ChangeKind, ChangeView, CommentView};
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use ui::paint::with_alpha;
use ui::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tokens::AccentChip;
use ui::tokens::colors::{NON_TEXT_MIN_CONTRAST, ON_FILL_MIN_CONTRAST};

use crate::i18n::t;
use crate::num;

/// Revision ink painted into the document text for tracked insertions /
/// deletions — paper markup the user authored, not OS chrome.
pub(super) const INSERTION_RGB: [u8; 3] = [0x1d, 0x7a, 0x3a];
pub(super) const DELETION_RGB: [u8; 3] = [0xbf, 0x29, 0x29];
/// Review colour for tracked insertions.
pub(super) const INSERTION_INK: Color =
    Color::from_rgb8(INSERTION_RGB[0], INSERTION_RGB[1], INSERTION_RGB[2]);
/// Review colour for tracked deletions.
pub(super) const DELETION_INK: Color =
    Color::from_rgb8(DELETION_RGB[0], DELETION_RGB[1], DELETION_RGB[2]);

const COMMENT_TAB_W: f64 = 4.0;
const COMMENT_CARD_W: f64 = 150.0;
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

/// How strongly a comment card is washed with the theme's `warning` hue:
/// enough to read as "a note" beside a change card, not a second panel.
const COMMENT_WASH: f32 = 0.14;
/// Wash of the accept / reject / resolve button faces.
const BUTTON_WASH: f32 = 0.20;
/// Alpha of the comment-range band on the paper (text must read through it).
const COMMENT_HIGHLIGHT_ALPHA: u8 = 0x66;

/// The review cards' colours, derived from the OS theme. The cards sit on the
/// desk beside the page, so they are chrome: comments take the `warning` hue,
/// accepting and resolving `success`, rejecting `danger`, and every ink is
/// measured against the face it lands on.
#[derive(Clone, Copy)]
pub(super) struct ReviewColors {
    comment: AccentChip,
    comment_body_ink: Color,
    /// The comment's tab beside the range and stripe down the card.
    /// The comment's tab beside the commented range, on the paper.
    comment_tab: Color,
    /// The stripe down the comment card's leading edge.
    comment_stripe: Color,
    change_card: Color,
    change_edge: Color,
    change_label_ink: Color,
    insertion_mark: Color,
    deletion_mark: Color,
    accept: AccentChip,
    reject: AccentChip,
    /// Warning wash behind a comment-marked body range. Chrome on the paper.
    pub(super) comment_highlight: Color,
}

impl ReviewColors {
    /// `paper` is the sheet a comment's tab lands on beside its range.
    pub(super) fn for_theme(theme: &Theme, paper: Color) -> Self {
        let c = &theme.tokens.colors;
        let card = c.surfaces.raised;
        let comment =
            c.tinted_chip_on(card, c.semantic.warning, COMMENT_WASH, ON_FILL_MIN_CONTRAST);
        let comment_tab = c.ink_on(paper, c.semantic.warning, NON_TEXT_MIN_CONTRAST);
        Self {
            comment,
            comment_body_ink: theme.on_surface(comment.fill).ink,
            comment_tab,
            comment_highlight: with_alpha(comment_tab, COMMENT_HIGHLIGHT_ALPHA),
            comment_stripe: c.ink_on(comment.fill, c.semantic.warning, NON_TEXT_MIN_CONTRAST),
            change_card: card,
            change_edge: ui::surface::edge_on(card),
            change_label_ink: theme.on_surface(card).ink_dim,
            insertion_mark: c.ink_on(card, c.semantic.success, NON_TEXT_MIN_CONTRAST),
            deletion_mark: c.ink_on(card, c.semantic.danger, NON_TEXT_MIN_CONTRAST),
            accept: c.tinted_chip_on(card, c.semantic.success, BUTTON_WASH, ON_FILL_MIN_CONTRAST),
            reject: c.tinted_chip_on(card, c.semantic.danger, BUTTON_WASH, ON_FILL_MIN_CONTRAST),
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
        colors: &ReviewColors,
    ) {
        self.clear_hits();
        if !self.visible {
            return;
        }

        comment_markers.sort_by(marker_order);
        // The card column stops short of the canvas scrollbar lane, so a
        // card never runs under the thumb.
        let comment_x = canvas.x1 - COMMENT_CARD_W - CARD_RIGHT_INSET;
        let labels = CardButtonLabels::measure(context.text);
        let mut next_comment_y = f64::MIN;
        for marker in &comment_markers {
            next_comment_y = self.draw_comment(
                context,
                Point::new(comment_x, next_comment_y),
                marker,
                &labels,
                colors,
            );
        }

        change_markers.sort_by(marker_order);
        let change_x = comment_x - CARD_GAP - CHANGE_CARD_W;
        let mut next_change_y = f64::MIN;
        for marker in &change_markers {
            next_change_y =
                self.draw_change(context, Point::new(change_x, next_change_y), marker, colors);
        }
    }

    /// Paint the card for the comment `marker` anchors — `(id, anchor y,
    /// right edge of the commented range)` — no higher than `slot`, whose `x`
    /// is the card column. Returns the y below which the next card may start.
    #[allow(
        clippy::too_many_lines,
        reason = "comment painting keeps text measurement, card geometry and reply hit targets together"
    )]
    fn draw_comment(
        &mut self,
        context: &mut PaintContext<'_>,
        slot: Point,
        marker: &(String, f64, f64),
        labels: &CardButtonLabels,
        colors: &ReviewColors,
    ) -> f64 {
        let (id, anchor_y, range_right) = (marker.0.as_str(), marker.1, marker.2);
        let Point {
            x: card_x,
            y: next_card_y,
        } = slot;
        let scene = &mut *context.scene;
        let transform = context.transform;
        let text_renderer = &mut *context.text;
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
            colors.comment_tab,
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
        let body_layout = RichTextLayout::shape_styled(
            &mut text_renderer.font_system,
            &[RichSpan::new(&body)],
            &RichTextLayout::block_style(false, 11.0, FontFamily::Sans, 1.0),
            Some(num::f32_of_f64(COMMENT_CARD_W - 16.0)),
            TextAlign::Left,
            vec![],
            vec![],
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
        let reply_layout = RichTextLayout::shape_styled(
            &mut text_renderer.font_system,
            &[RichSpan::new(&reply_text)],
            &RichTextLayout::block_style(false, 10.0, FontFamily::Sans, 1.0),
            Some(num::f32_of_f64(COMMENT_CARD_W - 16.0)),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let reply_h = if replies.is_empty() {
            0.0
        } else {
            4.0 + f64::from(reply_layout.height())
        };
        let card_h = 22.0 + f64::from(body_layout.height()) + reply_h + 8.0 + REVIEW_BTN + 6.0;
        let card = Rect::new(card_x, card_top, card_x + COMMENT_CARD_W, card_top + card_h);
        let rounded = vello::kurbo::RoundedRect::from_rect(card, 5.0);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.comment.fill,
            None,
            &rounded,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.comment.border,
            None,
            &rounded,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.comment_stripe,
            None,
            &Rect::new(card_x, card_top, card_x + 3.0, card_top + card_h),
        );
        text_renderer.draw_text(
            scene,
            author,
            11.5,
            colors.comment.ink,
            transform * Affine::translate((card_x + 9.0, card_top + 5.0)),
            None,
        );
        text_renderer.draw_rich(
            scene,
            &body_layout,
            transform * Affine::translate((card_x + 9.0, card_top + 20.0)),
            colors.comment_body_ink,
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
                colors.comment_body_ink,
            );
        }

        // Resolve is trailing-aligned and sized first; Reply takes what is
        // left of the row (less the card gap) so the two faces — and their
        // hit rects — can never overlap, whatever the locale or text scale.
        let resolve = self.draw_comment_resolve_button(context, card, id, labels, colors);
        let reply_room = Rect::new(card.x0, card.y0, resolve.x0 - CARD_GAP, card.y1);
        self.draw_comment_reply_button(context, reply_room, id, labels, colors);
        card_top + card_h + CARD_GAP
    }

    /// The Reply button on a comment card, laid out along the bottom of
    /// `room`: the card, less the span Resolve took.
    fn draw_comment_reply_button(
        &mut self,
        context: &mut PaintContext<'_>,
        room: Rect,
        id: &str,
        labels: &CardButtonLabels,
        colors: &ReviewColors,
    ) {
        let scene = &mut *context.scene;
        let transform = context.transform;
        let text_renderer = &mut *context.text;
        let (card_x, max_x1) = (room.x0, room.x1);
        let button_y = room.y1 - REVIEW_BTN - 4.0;
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
            colors.comment.fill,
            None,
            &vello::kurbo::RoundedRect::from_rect(button, 4.0),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.comment.border,
            None,
            &vello::kurbo::RoundedRect::from_rect(button, 4.0),
        );
        text_renderer.draw_text(
            scene,
            &label,
            labels.reply_font,
            colors.comment.ink,
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
    fn draw_comment_resolve_button(
        &mut self,
        context: &mut PaintContext<'_>,
        card: Rect,
        id: &str,
        labels: &CardButtonLabels,
        colors: &ReviewColors,
    ) -> Rect {
        let scene = &mut *context.scene;
        let transform = context.transform;
        let text_renderer = &mut *context.text;
        let button_y = card.y1 - REVIEW_BTN - 4.0;
        // Trailing-aligned inside the card, sized to its measured label so
        // "✓ Resolve" (and its longer translations) ends inside the face.
        let (label_w, label_h) = labels.resolve_size;
        let width = label_w + 2.0 * BUTTON_PAD_X + 2.0;
        let x1 = card.x1 - 8.0;
        let button = Rect::new(x1 - width, button_y, x1, button_y + REVIEW_BTN);
        let rounded_button = vello::kurbo::RoundedRect::from_rect(button, 4.0);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accept.fill,
            None,
            &rounded_button,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.accept.border,
            None,
            &rounded_button,
        );
        text_renderer.draw_text(
            scene,
            &labels.resolve,
            labels.resolve_font,
            colors.accept.ink,
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

    /// Paint the card for the tracked change `marker` anchors, no higher than
    /// `slot`. Returns the y below which the next card may start.
    fn draw_change(
        &mut self,
        context: &mut PaintContext<'_>,
        slot: Point,
        marker: &(String, f64, f64),
        colors: &ReviewColors,
    ) -> f64 {
        let (id, anchor_y) = (marker.0.as_str(), marker.1);
        let Point {
            x: card_x,
            y: next_card_y,
        } = slot;
        let scene = &mut *context.scene;
        let transform = context.transform;
        let text_renderer = &mut *context.text;
        let Some(change) = self.changes.iter().find(|change| change.id == id) else {
            return next_card_y;
        };
        let (label, kind_color) = match change.kind {
            ChangeKind::Ins => (t("review-insertion"), colors.insertion_mark),
            ChangeKind::Del => (t("review-deletion"), colors.deletion_mark),
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
        scene.fill(Fill::NonZero, transform, colors.change_card, None, &rounded);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.change_edge,
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
            colors.change_label_ink,
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
            colors.accept.fill,
            None,
            &vello::kurbo::RoundedRect::from_rect(accept, 4.0),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.reject.fill,
            None,
            &vello::kurbo::RoundedRect::from_rect(reject, 4.0),
        );
        text_renderer.draw_text(
            scene,
            "✓",
            12.0,
            colors.accept.ink,
            transform * Affine::translate((accept.x0 + 4.0, accept.y0 + 1.5)),
            None,
        );
        text_renderer.draw_text(
            scene,
            "✗",
            12.0,
            colors.reject.ink,
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
    use ui::ColorCompat;

    /// The review cards are chrome: under every shipped theme each label
    /// clears body-text contrast on the face it is drawn on, and each mark
    /// clears the non-text floor on its ground.
    #[test]
    fn review_cards_take_the_theme_and_stay_legible_in_every_preset() {
        use ui::a11y::contrast_ratio;
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped theme resolves");
            let c = &theme.tokens.colors;
            // Canonical light paper the editor falls back to (`PAPER_DEFAULT`).
            let paper = Color::from_rgb8(0xfc, 0xfc, 0xfa);
            let rc = ReviewColors::for_theme(&theme, paper);
            for (what, ink, face, floor) in [
                (
                    "comment author",
                    rc.comment.ink,
                    rc.comment.fill,
                    ON_FILL_MIN_CONTRAST,
                ),
                (
                    "comment body",
                    rc.comment_body_ink,
                    rc.comment.fill,
                    ON_FILL_MIN_CONTRAST,
                ),
                (
                    "change label",
                    rc.change_label_ink,
                    rc.change_card,
                    ON_FILL_MIN_CONTRAST,
                ),
                (
                    "accept",
                    rc.accept.ink,
                    rc.accept.fill,
                    ON_FILL_MIN_CONTRAST,
                ),
                (
                    "reject",
                    rc.reject.ink,
                    rc.reject.fill,
                    ON_FILL_MIN_CONTRAST,
                ),
                ("comment tab", rc.comment_tab, paper, NON_TEXT_MIN_CONTRAST),
                (
                    "comment stripe",
                    rc.comment_stripe,
                    rc.comment.fill,
                    NON_TEXT_MIN_CONTRAST,
                ),
                (
                    "insertion stripe",
                    rc.insertion_mark,
                    rc.change_card,
                    NON_TEXT_MIN_CONTRAST,
                ),
                (
                    "deletion stripe",
                    rc.deletion_mark,
                    rc.change_card,
                    NON_TEXT_MIN_CONTRAST,
                ),
            ] {
                let r = contrast_ratio(ink, face);
                assert!(
                    r >= floor,
                    "{name}: {what} measures {r:.2}:1 (needs {floor})"
                );
            }
            assert_eq!(rc.change_card, c.surfaces.raised, "{name}: change card");
            assert_eq!(
                rc.comment_highlight.a(),
                COMMENT_HIGHLIGHT_ALPHA,
                "{name}: comment highlight stays a wash"
            );
            assert_eq!(
                (
                    rc.comment_highlight.r(),
                    rc.comment_highlight.g(),
                    rc.comment_highlight.b()
                ),
                (rc.comment_tab.r(), rc.comment_tab.g(), rc.comment_tab.b()),
                "{name}: comment highlight is the tab warning"
            );
            assert_eq!(
                rc.comment_tab,
                c.ink_on(paper, c.semantic.warning, NON_TEXT_MIN_CONTRAST),
                "{name}: comment tab is warning on paper"
            );
        }
    }

    /// A hidden overlay never routes stale hits and the topmost review hit wins.
    #[test]
    fn review_overlay_hits_are_topmost_and_never_stale() {
        // Phase: hidden_overlay_never_routes_stale_hits.
        {
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

        // Phase: topmost_review_hit_wins.
        {
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
}
