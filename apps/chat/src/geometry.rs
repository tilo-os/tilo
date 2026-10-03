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
use ui::widget::{AvatarSize, ButtonFace, ControlSize};
use vello::kurbo::Rect;

/// A band of `content_h + 2 * pad` centred vertically inside `container`,
/// spanning the container's full width. When the padded content is taller
/// than the container the band is the container itself (the content owner
/// scrolls or grows; it never leaks past the box).
///
/// The composer uses this so a one-line draft sits on the same centre line as
/// the attach / emoji / send buttons instead of hugging the box's top edge.
pub(crate) fn centered_band(container: Rect, content_h: f64, pad: f64) -> Rect {
    let wanted = (content_h + 2.0 * pad).max(0.0);
    let h = wanted.min(container.height());
    let y0 = container.y0 + (container.height() - h) / 2.0;
    Rect::new(container.x0, y0, container.x1, y0 + h)
}

/// How a column header shares its row between a title and an optional
/// trailing chip (the sidebar's "3 online"). The chip yields first: it is
/// shown only while the whole title still fits beside it, so the workspace
/// name never elides to make room for a status chip. Returns the width the
/// title may take and whether the chip shows.
pub(crate) fn title_beside_chip(avail: f64, title_w: f64, chip_w: f64, gap: f64) -> (f64, bool) {
    let avail = avail.max(0.0);
    if chip_w > 0.0 && title_w + gap + chip_w <= avail {
        (avail - gap - chip_w, true)
    } else {
        (avail, false)
    }
}

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
    /// Width of the workspace rail inside the navigation drawer.
    pub rail_w: f64,
    /// The body text style the composer measures its draft with.
    pub body: TextStyle,
}

impl Chrome {
    pub(crate) fn of(theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let heading_line = f64::from(theme.text(TextRole::Heading).line_height_px());
        let body = theme.text(TextRole::Body);
        let body_line = f64::from(body.line_height_px());
        let control = ButtonFace::height(ControlSize::Regular, theme);
        let composer_min_h =
            (control + 2.0 * f64::from(sp.s1)).max(body_line + 2.0 * f64::from(sp.s3));
        Self {
            header_h: f64::from(metrics.header_height).max(heading_line + 2.0 * f64::from(sp.s3)),
            gutter: f64::from(sp.s6),
            avatar: AvatarSize::Regular.side(theme),
            avatar_gap: f64::from(sp.s3),
            body_line,
            composer_min_h,
            composer_max_h: composer_min_h + 6.0 * body_line,
            composer_pad: f64::from(sp.s4),
            rail_w: f64::from(metrics.rail_width),
            body,
        }
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
    fn centered_band_and_line_top_centre_inside_their_container() {
        // Phase: centered_band_shares_the_container_centre.
        {
            let box_ = Rect::new(0.0, 100.0, 400.0, 176.0);
            let band = centered_band(box_, 19.0, 8.0);
            assert!(approx(band.height(), 35.0));
            assert!(approx(band.center().y, box_.center().y));
            assert_eq!((band.x0, band.x1), (box_.x0, box_.x1));
        }
        // Phase: centered_band_never_exceeds_its_container.
        {
            let box_ = Rect::new(0.0, 0.0, 400.0, 60.0);
            let band = centered_band(box_, 200.0, 8.0);
            assert_eq!(band, box_);
        }
    }

    #[test]
    fn the_chip_yields_before_the_title_elides() {
        // Room for both: the title keeps its width beside the chip.
        assert_eq!(title_beside_chip(200.0, 100.0, 60.0, 8.0), (132.0, true));
        // Too tight for both: the chip goes and the title takes the row.
        assert_eq!(title_beside_chip(150.0, 100.0, 60.0, 8.0), (150.0, false));
        // No chip at all.
        assert_eq!(title_beside_chip(150.0, 100.0, 0.0, 8.0), (150.0, false));
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
