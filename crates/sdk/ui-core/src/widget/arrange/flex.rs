//! `stack` and `row`: Taffy flex containers (layout design 3).

use taffy::style_helpers::length;
use taffy::{Size, Style};

use super::arranged::{Arranged, Layout, Resolved, Slot, wrapper_style};
use super::units::{Align, Axis, Direction, Distribute};
use crate::tokens::Rhythm;

/// How a flex container runs.
pub struct FlexLayout {
    axis: Axis,
    gap: Rhythm,
    align: Align,
    distribute: Distribute,
    wrap: bool,
    direction: Direction,
    full_width: bool,
    full_height: bool,
}

impl Layout for FlexLayout {
    fn name(&self) -> &'static str {
        match self.axis {
            Axis::Block => "Stack",
            Axis::Inline => "Row",
        }
    }

    fn container(&self, resolved: &Resolved) -> Style {
        let gap = length(resolved.gap(self.gap));
        Style {
            display: taffy::Display::Flex,
            flex_direction: match self.axis {
                Axis::Block => taffy::FlexDirection::Column,
                Axis::Inline => self.direction.row(),
            },
            flex_wrap: if self.wrap {
                taffy::FlexWrap::Wrap
            } else {
                taffy::FlexWrap::NoWrap
            },
            align_items: Some(self.align.items()),
            justify_content: Some(self.distribute.justify()),
            gap: Size {
                width: gap,
                height: gap,
            },
            min_size: Size {
                width: length(0.0_f32),
                height: taffy::Dimension::auto(),
            },
            size: Size {
                width: if self.full_width {
                    taffy::style_helpers::percent(1.0_f32)
                } else {
                    taffy::Dimension::auto()
                },
                height: if self.full_height {
                    taffy::style_helpers::percent(1.0_f32)
                } else {
                    taffy::Dimension::auto()
                },
            },
            ..Style::default()
        }
    }

    fn slot(&self, _index: usize, slot: &Slot, resolved: &Resolved) -> Style {
        let mut style = wrapper_style();
        slot.fit.apply(&mut style, self.axis, resolved.rem);
        style
    }
}

/// Children top to bottom. `gap` defaults to `sibling`, `align` to stretch.
pub type Stack = Arranged<FlexLayout>;
/// Children along the inline direction, mirrored in right-to-left. `gap` defaults to `related`,
/// `align` to center.
pub type Row = Arranged<FlexLayout>;

impl Arranged<FlexLayout> {
    /// A [`Stack`].
    #[must_use]
    pub fn stack() -> Self {
        Self::with_layout(FlexLayout {
            axis: Axis::Block,
            gap: Rhythm::Sibling,
            align: Align::Stretch,
            distribute: Distribute::Start,
            wrap: false,
            direction: Direction::Ltr,
            full_width: false,
            full_height: false,
        })
    }

    /// A [`Row`].
    #[must_use]
    pub fn row() -> Self {
        Self::with_layout(FlexLayout {
            axis: Axis::Inline,
            gap: Rhythm::Related,
            align: Align::Center,
            distribute: Distribute::Start,
            wrap: false,
            direction: Direction::Ltr,
            full_width: false,
            full_height: false,
        })
    }

    #[must_use]
    pub const fn gap(mut self, gap: Rhythm) -> Self {
        self.layout.gap = gap;
        self
    }

    #[must_use]
    pub const fn align(mut self, align: Align) -> Self {
        self.layout.align = align;
        self
    }

    #[must_use]
    pub const fn distribute(mut self, distribute: Distribute) -> Self {
        self.layout.distribute = distribute;
        self
    }

    /// A flowing row, for chips and tags.
    #[must_use]
    pub const fn wrap(mut self) -> Self {
        self.layout.wrap = true;
        self
    }

    /// Take the whole width offered instead of hugging the content.
    #[must_use]
    pub const fn fill_width(mut self) -> Self {
        self.layout.full_width = true;
        self
    }

    /// Take the whole height offered too: the root of a window, whose children can then share
    /// the height by weight.
    #[must_use]
    pub const fn fill_height(mut self) -> Self {
        self.layout.full_height = true;
        self
    }

    /// The inline direction of a row.
    #[must_use]
    pub const fn direction(mut self, direction: Direction) -> Self {
        self.layout.direction = direction;
        self
    }
}
