//! `stack` and `row`: Taffy flex containers.

use taffy::style_helpers::length;
use taffy::{Size, Style};

use super::Side;
use super::arranged::{Arranged, Layout, Resolved, Slot, wrapper_style};
use super::units::{Align, Axis, Direction, Distribute};
use crate::tokens::Rhythm;

pub struct FlexLayout {
    axis: Axis,
    gap: Rhythm,
    padding: Rhythm,
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

    fn container(&self, resolved: &Resolved, lead: f32) -> Style {
        let gap = length(resolved.gap(self.gap));
        let pad = resolved.gap(self.padding);
        let lead = if lead > 0.0 { lead } else { 0.0 };
        let (left, right) = match self.leading() {
            Side::Leading => (pad + lead, pad),
            Side::Trailing => (pad, pad + lead),
        };
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
            padding: taffy::Rect {
                left: length(left),
                right: length(right),
                top: length(pad),
                bottom: length(pad),
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

    fn leading(&self) -> Side {
        match self.direction {
            Direction::Rtl => Side::Trailing,
            Direction::Ltr => Side::Leading,
        }
    }

    fn slot(&self, index: usize, slot: &Slot, resolved: &Resolved) -> Style {
        let mut style = wrapper_style();
        slot.fit.apply(&mut style, self.axis, resolved.rem);
        let along = match self.axis {
            Axis::Inline => slot.fit.width.fill,
            Axis::Block => slot.fit.height.fill,
        };
        // A row that wraps flows its items at their own width: one that fills breaks onto a
        // line of its own when its own width does not fit beside what comes before it, and
        // then takes the room left on its line. From a zero basis it would never break, and
        // would take whatever its line left over, down to nothing (a field beside a button).
        if self.wrap && along.is_some_and(|weight| weight > 0.0) {
            style.flex_basis = taffy::Dimension::auto();
        }
        // A spacer that takes the room its siblings leave is room, not a sibling: the gap the
        // container puts between its children is not put on both sides of it as well, so a row
        // short of room lays out as if it held none (a card's title and its menu keep their
        // one gap), and a row with room sends what follows it to the far end.
        if along.is_some() && slot.element.name() == "Spacer" {
            let back = length(-resolved.gap(self.gap));
            let before = index > 0;
            match (self.axis, before == (self.direction == Direction::Ltr)) {
                (Axis::Inline, true) => style.margin.left = back,
                (Axis::Inline, false) => style.margin.right = back,
                (Axis::Block, _) if before => style.margin.top = back,
                (Axis::Block, _) => style.margin.bottom = back,
            }
        }
        style
    }
}

pub type Stack = Arranged<FlexLayout>;
pub type Row = Arranged<FlexLayout>;

impl Arranged<FlexLayout> {
    #[must_use]
    pub fn stack() -> Self {
        Self::with_layout(FlexLayout {
            axis: Axis::Block,
            gap: Rhythm::Sibling,
            padding: Rhythm::None,
            align: Align::Stretch,
            distribute: Distribute::Start,
            wrap: false,
            direction: Direction::Ltr,
            full_width: false,
            full_height: false,
        })
    }

    #[must_use]
    pub fn row() -> Self {
        Self::with_layout(FlexLayout {
            axis: Axis::Inline,
            gap: Rhythm::Related,
            padding: Rhythm::None,
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
    pub const fn padding(mut self, padding: Rhythm) -> Self {
        self.layout.padding = padding;
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

    #[must_use]
    pub const fn fill_height(mut self) -> Self {
        self.layout.full_height = true;
        self
    }

    #[must_use]
    pub const fn direction(mut self, direction: Direction) -> Self {
        self.layout.direction = direction;
        self
    }
}
