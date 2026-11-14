//! The pill that takes a reader back to where they want to be, over a conversation they have scrolled
//! away from.

use vello::kurbo::Rect;

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{Button, ButtonFace, ButtonRole};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Jump {
    Unread,
    Present,
}

/// How many ways back a list keeps a width for ([`Jump::slot`] names each one).
pub const JUMPS: usize = 2;

/// Where a pill `width` wide and `height` tall stands in `area`: against the trailing edge, a
/// `gutter` in, half a control's height above the foot. A conversation and the list that draws its
/// rows place the same way back with this, so the pill stands in one place wherever it is drawn.
#[must_use]
pub fn pill_rect(area: Rect, gutter: f64, width: f64, height: f64) -> Rect {
    let width = width.min(area.width());
    Rect::new(
        (area.x1 - gutter - width).max(area.x0),
        (area.y1 - gutter / 2.0 - height).max(area.y0),
        (area.x1 - gutter).max(area.x0),
        (area.y1 - gutter / 2.0).max(area.y0),
    )
}

impl Jump {
    /// Both ways back, in the order [`Jump::slot`] keeps their widths.
    pub const ALL: [Self; JUMPS] = [Self::Unread, Self::Present];

    #[must_use]
    pub fn label(self) -> String {
        super::t(match self {
            Self::Unread => "conversation-jump-unread",
            Self::Present => "conversation-jump-present",
        })
    }

    #[must_use]
    pub const fn icon(self) -> Icon {
        Icon::ArrowDown
    }

    /// The width the pill needs to show this way back whole: the face's padding, its icon and the
    /// gap beside it, and the words set as the face sets them.
    #[must_use]
    pub fn width(self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        ButtonFace::new(Rect::ZERO, &self.label())
            .icon(self.icon())
            .intrinsic_width(text, theme)
    }

    /// Where this way back's width stands in what a list keeps for the pill.
    #[must_use]
    pub const fn slot(self) -> usize {
        match self {
            Self::Unread => 0,
            Self::Present => 1,
        }
    }
}

/// The pill as it is drawn: which way back, whether the pointer is on it, and where it sits.
///
/// A conversation places one at the foot of its reading area, against the trailing edge, as wide as
/// its words need: it floats over the rows it covers, so it carries a surface of its own rather
/// than cutting what it says to fit.
#[derive(Clone, Debug, PartialEq)]
pub struct JumpFace {
    jump: Jump,
    label: String,
    rect: Rect,
    hovered: bool,
    focused: bool,
}

impl JumpFace {
    #[must_use]
    pub fn new(jump: Jump, rect: Rect) -> Self {
        Self {
            jump,
            label: jump.label(),
            rect,
            hovered: false,
            focused: false,
        }
    }

    #[must_use]
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    /// The keyboard's seat, or a reader's: the ring a control wears while it is the one to press.
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    pub fn paint(&self, paint: &mut PaintCtx<'_>) {
        // The pill floats over the conversation's words, so it stands on a surface of its own: the
        // shadow that lifts it off them and the ground its own words are read against.
        let panel =
            crate::chrome::paint_panel(paint.scene, paint.transform, self.rect, paint.theme, 1.0);
        let face = ButtonFace::new(self.rect, &self.label)
            .icon(self.jump.icon())
            .role(ButtonRole::Plain)
            .hovered(self.hovered)
            .focused(self.focused);
        Button::paint(
            paint.scene,
            paint.transform,
            paint.text,
            &panel.theme,
            &face,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pill_is_as_wide_as_the_words_it_says() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut widths: Vec<f64> = Vec::new();
        for jump in [Jump::Unread, Jump::Present] {
            let label = jump.label();
            assert!(!label.is_empty(), "{jump:?} has a word");
            assert_ne!(label, "conversation-jump-unread", "{jump:?} is translated");
            assert_ne!(label, "conversation-jump-present", "{jump:?} is translated");
            let width = jump.width(&mut text, &theme);
            let style = crate::widget::ControlSize::Regular.label_style(&theme);
            let words = f64::from(text.measure_style(&label, None, &style).0);
            assert!(
                width > words,
                "{jump:?} has room for its {words} px of words and the face around them in {width} px"
            );
            widths.push(width);
        }
        assert_ne!(widths[0], widths[1], "each way back keeps its own width");
        assert_ne!(Jump::Unread.label(), Jump::Present.label());
        for (slot, jump) in Jump::ALL.into_iter().enumerate() {
            assert_eq!(jump.slot(), slot, "each width has its own slot");
        }
        assert_eq!(Jump::ALL.len(), JUMPS);
    }
}
