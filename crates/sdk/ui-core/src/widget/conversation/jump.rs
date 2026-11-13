//! The pill that takes a reader back to where they want to be, over a conversation they have scrolled
//! away from.

use vello::kurbo::{Point, Rect};

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::theme::Theme;
use crate::widget::{Button, ButtonFace, ControlSize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Jump {
    Unread,
    Present,
}

impl Jump {
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
}

/// The pill as it is drawn: which way back, whether the pointer is on it, and where it sits.
///
/// A conversation places one at the foot of its reading area, against the trailing edge:
/// [`JumpFace::rect_in`] gives the rectangle for a reading area, and it never leaves the
/// conversation's own column.
#[derive(Clone, Debug, PartialEq)]
pub struct JumpFace {
    jump: Jump,
    label: String,
    rect: Rect,
    hovered: bool,
}

impl JumpFace {
    #[must_use]
    pub fn new(jump: Jump, rect: Rect) -> Self {
        Self {
            jump,
            label: jump.label(),
            rect,
            hovered: false,
        }
    }

    /// Where the pill sits in the reading area `area`: against the trailing edge, a gutter in,
    /// half a control's height above the foot, so it clears the composer and the scroll bar.
    #[must_use]
    pub fn rect_in(area: Rect, gutter: f64, width: f64, theme: &Theme) -> Rect {
        let height = ButtonFace::height(ControlSize::Regular, theme);
        let inset = gutter.max(0.0);
        Rect::new(
            (area.x1 - inset - width).max(area.x0),
            (area.y1 - inset / 2.0 - height).max(area.y0),
            (area.x1 - inset).max(area.x0),
            (area.y1 - inset / 2.0).max(area.y0),
        )
    }

    #[must_use]
    pub fn width(&self, text: &mut crate::text_render::TextRenderer, theme: &Theme) -> f64 {
        ButtonFace::new(Rect::ZERO, &self.label)
            .icon(self.jump.icon())
            .intrinsic_width(text, theme)
    }

    #[must_use]
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    #[must_use]
    pub fn jump(&self) -> Jump {
        self.jump
    }

    #[must_use]
    pub fn spoken(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn contains(&self, point: Point) -> bool {
        self.rect.contains(point)
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn paint(&self, paint: &mut PaintCtx<'_>) {
        let face = ButtonFace::new(self.rect, &self.label)
            .icon(self.jump.icon())
            .hovered(self.hovered);
        Button::paint(paint.scene, paint.transform, paint.text, paint.theme, &face);
    }
}

/// The space a conversation leaves at its foot for a pill over it, so the pill never covers
/// the last line: one control's height and the half-gutter it stands from the foot.
#[must_use]
pub fn pill_reserve(theme: &Theme, gutter: f64) -> f64 {
    ButtonFace::height(ControlSize::Regular, theme) + gutter / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pill_says_which_way_back_and_sits_clear_of_the_foot() {
        let theme = Theme::dark();
        let area = Rect::new(0.0, 0.0, 800.0, 600.0);
        let width = 140.0;
        let rect = JumpFace::rect_in(area, 16.0, width, &theme);
        assert!((rect.x1 - (area.x1 - 16.0)).abs() < f64::EPSILON);
        assert!((rect.x0 - (rect.x1 - width)).abs() < f64::EPSILON);
        let height = ButtonFace::height(ControlSize::Regular, &theme);
        assert!(
            (rect.height() - height).abs() < f64::EPSILON,
            "one control tall"
        );
        assert!(
            rect.y1 < area.y1,
            "the pill stands above the foot, clear of the composer"
        );
        // Both words are the catalogue's, never an app's.
        for jump in [Jump::Unread, Jump::Present] {
            let label = JumpFace::new(jump, rect).spoken().to_owned();
            assert!(!label.is_empty(), "{jump:?} has a word");
            assert_ne!(label, "conversation-jump-unread", "{jump:?} is translated");
            assert_ne!(label, "conversation-jump-present", "{jump:?} is translated");
        }
        assert_ne!(Jump::Unread.label(), Jump::Present.label());
        assert!(pill_reserve(&theme, 16.0) > height);
    }
}
