//! Margin cards for comments and tracked changes.
//!
//! This component owns the materialized review items, view visibility, drawing,
//! and frame-local hit targets. The editor remains responsible for applying the
//! typed [`ReviewAction`] to the document.

use tilo_doc::snapshot::{ChangeKind, ChangeView, CommentView};
use vello::kurbo::{Point, Rect};
use vello::peniko::{Color, Fill};

use ui::icons::Icon;
use ui::paint::{PaintCtx, with_alpha};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::tokens::colors::NON_TEXT_MIN_CONTRAST;
use ui::widget::{Button, ButtonFace, ButtonRole, CardFace, ControlSize, Tone};

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

/// Alpha of the comment-range band on the paper (text must read through it).
const COMMENT_HIGHLIGHT_ALPHA: u8 = 0x66;

/// The paper-side review colours: the wash under a commented range and the
/// pip at its end, both in the comment author's presence colour made
/// legible on the sheet. The margin cards themselves are content on the
/// window ground and read the theme directly.
pub(super) struct ReviewColors;

impl ReviewColors {
    /// Presence colour of an author, stepped until it reads on the paper.
    pub(super) fn author_mark(theme: &Theme, paper: Color, author: &str) -> Color {
        let c = &theme.tokens.colors;
        c.ink_on(
            paper,
            Tone::of_identity(author).colour(theme),
            NON_TEXT_MIN_CONTRAST,
        )
    }

    /// The translucent band under a range `author` commented on.
    pub(super) fn comment_highlight(theme: &Theme, paper: Color, author: &str) -> Color {
        with_alpha(
            Self::author_mark(theme, paper, author),
            COMMENT_HIGHLIGHT_ALPHA,
        )
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

/// Where the margin cards go this frame: the column beside the page and the
/// paper the comment pips land on, in the markers' space.
pub(super) struct ReviewPlacement {
    /// The visible canvas (the cards never leave it).
    pub(super) canvas: Rect,
    /// The page sheet's trailing edge.
    pub(super) page_x1: f64,
    /// The sheet's colour, for the comment pips.
    pub(super) paper: Color,
}

pub(super) struct ReviewOverlay {
    comments: Vec<CommentView>,
    changes: Vec<ChangeView>,
    visible: bool,
    hits: Vec<(Rect, ReviewAction)>,
    pointer: Point,
}

impl Default for ReviewOverlay {
    fn default() -> Self {
        Self {
            comments: Vec::new(),
            changes: Vec::new(),
            visible: true,
            hits: Vec::new(),
            pointer: Point::new(f64::NAN, f64::NAN),
        }
    }
}

/// One card in the margin column; comments and changes share the column in
/// reading order.
enum MarginCard {
    Comment(CommentView, f64),
    Change(ChangeView, f64),
}

impl MarginCard {
    fn anchor(&self) -> f64 {
        match self {
            MarginCard::Comment(_, y) | MarginCard::Change(_, y) => *y,
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

    /// The author of comment `id`, for its paper-side colours.
    pub(super) fn comment_author(&self, id: &str) -> Option<&str> {
        self.comments
            .iter()
            .find(|comment| comment.id == id)
            .map(|comment| comment.author.as_str())
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

    /// Track the pointer (in the cards' space) for the buttons' hover
    /// state; returns whether the hovered button changed.
    pub(super) fn pointer_moved(&mut self, point: Point) -> bool {
        let before = self.action_at(self.pointer);
        self.pointer = point;
        before != self.action_at(point)
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

    /// Paint the margin column: one card per comment and tracked change,
    /// each at its anchor line and never above the card before it, in the
    /// margin outside the sheet when it fits there (over the sheet's trailing
    /// margin otherwise), plus a pip at the end of every commented range.
    pub(super) fn draw(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        placement: &ReviewPlacement,
        comment_markers: &[(String, f64, f64)],
        change_markers: &[(String, f64, f64)],
    ) {
        self.clear_hits();
        if !self.visible {
            return;
        }
        let theme = ctx.theme;
        let gap = f64::from(theme.tokens.spacing.s4);
        let scrollbar = f64::from(theme.tokens.metrics.scrollbar_gutter);
        let wanted = f64::from(theme.tokens.metrics.sidebar_min_width);
        let minimum = wanted * 0.75;
        let room_x0 = placement.page_x1 + gap;
        let room_x1 = placement.canvas.x1 - scrollbar - gap;
        let (x0, width) = if room_x1 - room_x0 >= minimum {
            (room_x0, (room_x1 - room_x0).min(wanted))
        } else {
            let width = wanted.min(placement.canvas.width() - 2.0 * gap).max(1.0);
            ((room_x1 - width).max(placement.canvas.x0 + gap), width)
        };

        for (id, y, right) in comment_markers {
            if let Some(author) = self.comment_author(id) {
                let mark = ReviewColors::author_mark(theme, placement.paper, author);
                draw_pip(ctx, mark, Point::new(*right, *y));
            }
        }

        let mut cards: Vec<MarginCard> = comment_markers
            .iter()
            .filter_map(|(id, y, _)| {
                self.comments
                    .iter()
                    .find(|comment| &comment.id == id)
                    .map(|comment| MarginCard::Comment(comment.clone(), *y))
            })
            .chain(change_markers.iter().filter_map(|(id, y, _)| {
                self.changes
                    .iter()
                    .find(|change| &change.id == id)
                    .map(|change| MarginCard::Change(change.clone(), *y))
            }))
            .collect();
        cards.sort_by(|a, b| {
            a.anchor()
                .partial_cmp(&b.anchor())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let seam = f64::from(theme.tokens.spacing.s2);
        let mut next_y = f64::MIN;
        for card in &cards {
            let top = card.anchor().max(next_y);
            let slot = Rect::new(x0, top, x0 + width, top);
            let height = match card {
                MarginCard::Comment(comment, _) => self.draw_comment(ctx, slot, comment),
                MarginCard::Change(change, _) => self.draw_change(ctx, slot, change),
            };
            next_y = top + height + seam;
        }
    }

    /// Paint the card for `comment` at the top of `slot` (its width is the
    /// column's) and return its height: the author as the title, the
    /// comment in `Body`, up to two replies in `Caption`, then Reply and
    /// Resolve.
    fn draw_comment(&mut self, ctx: &mut PaintCtx<'_>, slot: Rect, comment: &CommentView) -> f64 {
        let theme = ctx.theme;
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
        let replies: Vec<(String, String)> = comment
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
                (reply.author.clone(), text)
            })
            .collect();
        let face = CardFace::new()
            .title(author)
            .leading_stripe(Tone::of_identity(&comment.author));
        let probe = face.body_rect(ctx.text, theme, Rect::new(slot.x0, 0.0, slot.x1, 1.0e4));
        let wrap = num::f32_of_f64(probe.width().max(1.0));
        let related = f64::from(theme.tokens.spacing.rhythm.related);
        let tight = f64::from(theme.tokens.spacing.rhythm.tight);
        let body_style = theme.text(TextRole::Body);
        let who_style = theme.text(TextRole::Caption).strong();
        let reply_style = theme.text(TextRole::Caption);
        let body_h = f64::from(ctx.text.measure_style(&body, Some(wrap), &body_style).1);
        let mut reply_hs = Vec::new();
        for (who, text) in &replies {
            let who_h = f64::from(ctx.text.measure_style(who, Some(wrap), &who_style).1);
            let text_h = f64::from(ctx.text.measure_style(text, Some(wrap), &reply_style).1);
            reply_hs.push((who_h, text_h));
        }
        let button_h = ButtonFace::height(ControlSize::Small, theme);
        let replies_h: f64 = reply_hs
            .iter()
            .map(|(who, text)| related + who + tight + text)
            .sum();
        let content_h = body_h + replies_h + related + button_h;
        let height = face.height_for_body(ctx.text, theme, slot.width(), content_h);
        let card = Rect::new(slot.x0, slot.y0, slot.x1, slot.y0 + height);
        face.paint(ctx, card);
        let inner = face.body_rect(ctx.text, theme, card);
        let ink = theme.tokens.colors.text_primary;
        let dim = theme.tokens.colors.text_dim;
        ctx.text(
            &body,
            &body_style,
            ink,
            Point::new(inner.x0, inner.y0),
            Some(wrap),
        );
        let mut y = inner.y0 + body_h;
        for ((who, text), (who_h, text_h)) in replies.iter().zip(&reply_hs) {
            y += related;
            ctx.text(who, &who_style, ink, Point::new(inner.x0, y), Some(wrap));
            y += who_h + tight;
            ctx.text(text, &reply_style, dim, Point::new(inner.x0, y), Some(wrap));
            y += text_h;
        }
        let row = Rect::new(inner.x0, inner.y1 - button_h, inner.x1, inner.y1);
        self.draw_comment_actions(ctx, row, &comment.id);
        height
    }

    /// Reply and Resolve along the bottom of a comment card.
    fn draw_comment_actions(&mut self, ctx: &mut PaintCtx<'_>, row: Rect, id: &str) {
        let theme = ctx.theme;
        let reply = t("review-reply");
        let resolve = t("review-resolve");
        let mut x = row.x0;
        for (label, action, role) in [
            (
                &reply,
                ReviewAction::OpenCommentThread(id.to_owned()),
                ButtonRole::Secondary,
            ),
            (
                &resolve,
                ReviewAction::ResolveComment(id.to_owned()),
                ButtonRole::Plain,
            ),
        ] {
            let width = ButtonFace::new(Rect::ZERO, label)
                .control_size(ControlSize::Small)
                .intrinsic_width(ctx.text, theme)
                .min((row.x1 - x).max(0.0));
            let rect = Rect::new(x, row.y0, x + width, row.y1);
            let face = ButtonFace::new(rect, label)
                .role(role)
                .control_size(ControlSize::Small)
                .hovered(rect.contains(self.pointer));
            Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
            self.hits.push((rect, action));
            x = rect.x1 + f64::from(theme.tokens.spacing.s2);
        }
    }

    /// Paint the card for the tracked `change` at the top of `slot` and
    /// return its height: what changed and who changed it, then Accept and
    /// Reject as icon buttons.
    fn draw_change(&mut self, ctx: &mut PaintCtx<'_>, slot: Rect, change: &ChangeView) -> f64 {
        let theme = ctx.theme;
        let (label, accept, reject) = match change.kind {
            ChangeKind::Ins => (
                t("review-insertion"),
                t("review-accept-insertion"),
                t("review-reject-insertion"),
            ),
            ChangeKind::Del => (
                t("review-deletion"),
                t("review-accept-deletion"),
                t("review-reject-deletion"),
            ),
        };
        let face = CardFace::new()
            .title(&label)
            .leading_stripe(Tone::of_identity(&change.author));
        let probe = face.body_rect(ctx.text, theme, Rect::new(slot.x0, 0.0, slot.x1, 1.0e4));
        let wrap = num::f32_of_f64(probe.width().max(1.0));
        let caption = theme.text(TextRole::Caption);
        let author_h = if change.author.is_empty() {
            0.0
        } else {
            f64::from(
                ctx.text
                    .measure_style(&change.author, Some(wrap), &caption)
                    .1,
            )
        };
        let button_h = ButtonFace::height(ControlSize::Small, theme);
        let related = f64::from(theme.tokens.spacing.rhythm.related);
        let height =
            face.height_for_body(ctx.text, theme, slot.width(), author_h + related + button_h);
        let card = Rect::new(slot.x0, slot.y0, slot.x1, slot.y0 + height);
        face.paint(ctx, card);
        let inner = face.body_rect(ctx.text, theme, card);
        if !change.author.is_empty() {
            let dim = theme.tokens.colors.text_dim;
            ctx.text(
                &change.author,
                &caption,
                dim,
                Point::new(inner.x0, inner.y0),
                Some(wrap),
            );
        }
        let mut x = inner.x0;
        for (icon, name, action) in [
            (
                Icon::Check,
                &accept,
                ReviewAction::AcceptChange(change.id.clone()),
            ),
            (
                Icon::X,
                &reject,
                ReviewAction::RejectChange(change.id.clone()),
            ),
        ] {
            let rect = Rect::new(x, inner.y1 - button_h, x + button_h, inner.y1);
            let face = ButtonFace::icon_only(rect, icon, name)
                .control_size(ControlSize::Small)
                .hovered(rect.contains(self.pointer));
            Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
            self.hits.push((rect, action));
            x = rect.x1 + f64::from(theme.tokens.spacing.s1);
        }
        height
    }
}

/// The comment pip: a caret-thin mark in the author's colour standing
/// between the last commented character and the next, one body line tall.
fn draw_pip(ctx: &mut PaintCtx<'_>, mark: Color, at: Point) {
    let width = f64::from(ctx.theme.tokens.metrics.chrome_rule_px).max(1.0) * 2.0;
    let height = ctx.line_box(TextRole::Body);
    let pip = Rect::new(at.x - width / 2.0, at.y, at.x + width / 2.0, at.y + height);
    ctx.scene
        .fill(Fill::NonZero, ctx.transform, mark, None, &pip);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A comment's paper-side marks take its author's presence colour and
    /// stay legible on the sheet under every shipped theme.
    #[test]
    fn comment_marks_take_the_author_colour_and_read_on_paper() {
        use ui::a11y::contrast_ratio;
        let paper = Color::from_rgb8(0xfc, 0xfc, 0xfa);
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped theme resolves");
            let mark = ReviewColors::author_mark(&theme, paper, "Maya Chen");
            assert!(
                contrast_ratio(mark, paper) >= NON_TEXT_MIN_CONTRAST,
                "{name}"
            );
            let wash = ReviewColors::comment_highlight(&theme, paper, "Maya Chen");
            assert_eq!(wash.to_rgba8().a, COMMENT_HIGHLIGHT_ALPHA, "{name}");
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
