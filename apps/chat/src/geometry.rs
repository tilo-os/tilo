//! Pure geometry for the hand-painted chat chrome.
//!
//! The chat surface computes its rectangles by hand in `render_chat.rs`. The
//! helpers here hold the *rules* those rectangles follow — how a control band
//! is centred, how a title shares a header row with a status chip, how tall
//! the window's chrome is in a theme — as plain functions, so the draw pass,
//! the hit list and the unit tests all consume one description instead of
//! three copies of the same arithmetic.

use ui::text_render::TextStyle;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{AvatarSize, ButtonFace, ChipFace, ControlSize};

/// How many faces the call card stacks before its names count the rest.
pub(crate) const CALL_CARD_FACES: usize = 3;

/// How far each stacked face advances over the one before it, as a share of
/// its side: the faces overlap, so a crowd costs the card's names little.
pub(crate) const CALL_CARD_STACK_STEP: f64 = 0.7;

/// The chat window's chrome metrics in one theme: the header row, the
/// content gutter, the message column and the composer's height range. The
/// paint pass refreshes it from the theme it draws with; event handling and
/// the renderer-less layout reads (`messages_rect`, `composer_rect`) use the
/// last painted values, so a hit-test always matches the frame on screen.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Chrome {
    /// The window's one header row (sidebar, conversation and inspector
    /// headers share it).
    pub header_h: f64,
    /// Leading/trailing inset of the conversation column.
    pub gutter: f64,
    /// Side of a message author's avatar.
    pub avatar: f64,
    /// Gap between the avatar and the message column.
    pub avatar_gap: f64,
    /// One body line, the composer's step as its draft grows.
    pub body_line: f64,
    /// Shortest and tallest composer field.
    pub composer_min_h: f64,
    pub composer_max_h: f64,
    /// Space around the composer field.
    pub composer_pad: f64,
    /// The strip under the composer for one caption line: who is typing,
    /// an upload under way, a message not sent.
    pub composer_note_h: f64,
    /// The body text style the composer measures its draft with.
    pub body: TextStyle,
    /// The floating call window around its video: the title bar above it,
    /// the inset beside and below it, the gap between its parts, its
    /// connection line, one participant chip and its controls row.
    pub call_title_h: f64,
    pub call_inset: f64,
    pub call_gap: f64,
    pub call_status_h: f64,
    pub call_chip_h: f64,
    pub call_controls_h: f64,
    /// The call card's hero face: one of the people the call is with, small
    /// enough that the names beside the stack keep the card's width.
    pub call_card_face_h: f64,
    /// The card's hero line: the faces and the names beside them.
    pub call_card_hero_h: f64,
}

impl Chrome {
    pub(crate) fn of(theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let body = theme.text(TextRole::Body);
        let body_line = f64::from(body.line_height_px());
        let control = ButtonFace::height(ControlSize::Regular, theme);
        let composer_min_h =
            (control + 2.0 * f64::from(sp.s1)).max(body_line + 2.0 * f64::from(sp.s3));
        Self {
            header_h: ui::chrome::HeaderRowFace::height(theme),
            gutter: f64::from(sp.s6),
            avatar: AvatarSize::Regular.side(theme),
            avatar_gap: f64::from(sp.s3),
            body_line,
            composer_min_h,
            composer_max_h: composer_min_h + 6.0 * body_line,
            composer_pad: f64::from(sp.s4),
            composer_note_h: f64::from(theme.text(TextRole::Caption).line_height_px())
                + 2.0 * f64::from(sp.s1),
            body,
            call_title_h: f64::from(theme.text(TextRole::Label).line_height_px())
                + 2.0 * f64::from(sp.s3),
            call_inset: f64::from(sp.s3),
            call_gap: f64::from(sp.s2),
            call_status_h: f64::from(theme.text(TextRole::Caption).line_height_px())
                .max(f64::from(metrics.icon_sm)),
            call_chip_h: ChipFace::height(theme),
            call_controls_h: ButtonFace::height(ControlSize::Small, theme),
            call_card_face_h: AvatarSize::Regular.side(theme),
            call_card_hero_h: AvatarSize::Regular
                .side(theme)
                .max(f64::from(theme.text(TextRole::Heading).line_height_px())),
        }
    }

    /// The call card's body: its header row, the hero line that names who the
    /// call is with and how long it runs, its status line and its controls.
    /// The shell's floating layer gives the card the column's width; this is
    /// the height it asks for.
    pub(crate) fn call_card_height(&self) -> f64 {
        2.0 * self.call_inset
            + self.call_title_h
            + 3.0 * self.call_gap
            + self.call_card_hero_h
            + self.call_status_h
            + self.call_controls_h
    }

    /// Where a message's text column starts, from the conversation's leading
    /// edge: the gutter, the avatar and the gap after it. Author, body,
    /// edited marker, attachments, reactions and the thread link all share it.
    pub(crate) fn text_inset(&self) -> f64 {
        self.gutter + self.avatar + self.avatar_gap
    }
}

impl Default for Chrome {
    /// The 1× chrome before the first paint (geometry tests, the first
    /// event): the dark theme's metrics at the reference text size.
    fn default() -> Self {
        Self::of(&Theme::dark())
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests assert exact authored geometry")]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn chrome_grows_with_the_text_and_keeps_one_message_edge() {
        let base = Chrome::of(&Theme::dark());
        assert!(
            base.header_h >= 56.0,
            "the shell's header metric is a floor"
        );
        assert!(base.composer_max_h > base.composer_min_h);
        assert!(approx(
            base.text_inset(),
            base.gutter + base.avatar + base.avatar_gap
        ));
        let large = Chrome::of(&Theme::dark().with_text_scale(2.0));
        assert!(large.composer_min_h > base.composer_min_h);
    }
}
