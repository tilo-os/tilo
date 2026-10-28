//! `grid(min:)` and `grid(columns:)`: the responsive grid (layout design 3).

use taffy::style_helpers::{fr, length, minmax, repeat};
use taffy::{Size, Style};

use super::arranged::{Arranged, Layout, Resolved, Slot, wrapper_style};
use super::units::{Axis, Length, columns_for};
use crate::tokens::Rhythm;

/// How many columns a grid has.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Columns {
    /// As many equal columns of at least this width as fit.
    AtLeast(Length),
    /// This many.
    Fixed(u16),
}

pub struct GridLayout {
    columns: Columns,
    gap: Rhythm,
    /// The count the last allocation settled on.
    count: u16,
    /// How many items the grid holds: a row never has more columns than items, so a few cards
    /// share the whole width instead of leaving empty columns beside them.
    items: usize,
    /// Each item as tall as its content rather than its row: a dashboard's cards of different
    /// lengths, which would otherwise stand with empty space under the shorter ones.
    hug: bool,
}

impl GridLayout {
    /// The columns the grid has at `width` logical pixels.
    #[must_use]
    pub fn count_for(&self, width: f32, resolved: &Resolved) -> u16 {
        match self.columns {
            Columns::Fixed(count) => count.max(1),
            Columns::AtLeast(min) => {
                let found = columns_for(width, min.resolve(resolved.rem), resolved.gap(self.gap));
                let kept = found.max(1).min(self.items.max(1));
                u16::try_from(kept).unwrap_or(u16::MAX)
            }
        }
    }

    /// The column count of the last layout.
    #[must_use]
    pub const fn columns(&self) -> u16 {
        self.count
    }
}

impl Layout for GridLayout {
    fn name(&self) -> &'static str {
        "Grid"
    }

    fn items(&mut self, count: usize) {
        self.items = count;
    }

    fn container(&self, resolved: &Resolved) -> Style {
        let gap = length(resolved.gap(self.gap));
        Style {
            display: taffy::Display::Grid,
            grid_template_columns: vec![repeat(
                self.count.max(1),
                vec![minmax(length(0.0_f32), fr(1.0_f32))],
            )],
            grid_auto_rows: vec![taffy::style_helpers::auto()],
            align_items: Some(if self.hug {
                taffy::AlignItems::Start
            } else {
                taffy::AlignItems::Stretch
            }),
            gap: Size {
                width: gap,
                height: gap,
            },
            min_size: Size {
                width: length(0.0_f32),
                height: taffy::Dimension::auto(),
            },
            size: Size {
                width: taffy::style_helpers::percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
            ..Style::default()
        }
    }

    fn slot(&self, _index: usize, slot: &Slot, resolved: &Resolved) -> Style {
        let mut style = wrapper_style();
        slot.fit.apply(&mut style, Axis::Inline, resolved.rem);
        let span = slot.span.clamp(1, self.count.max(1));
        style.grid_column = taffy::Line {
            start: taffy::GridPlacement::Span(span),
            end: taffy::GridPlacement::Auto,
        };
        style
    }

    fn settle(&mut self, allocated: Size<f32>, resolved: &Resolved) -> bool {
        let count = self.count_for(allocated.width, resolved);
        let changed = count != self.count;
        self.count = count;
        changed
    }
}

/// A responsive grid. Items fill the columns; an item may span several.
pub type Grid = Arranged<GridLayout>;

impl Arranged<GridLayout> {
    /// As many equal columns of at least `min` as fit: one column on a phone, five on a monitor,
    /// with no breakpoints.
    #[must_use]
    pub fn auto(min: Length) -> Self {
        Self::with_layout(GridLayout {
            columns: Columns::AtLeast(min),
            gap: Rhythm::Sibling,
            count: 1,
            items: usize::MAX,
            hug: false,
        })
    }

    /// A fixed number of columns.
    #[must_use]
    pub fn columns(count: u16) -> Self {
        Self::with_layout(GridLayout {
            columns: Columns::Fixed(count),
            gap: Rhythm::Sibling,
            count: count.max(1),
            items: usize::MAX,
            hug: false,
        })
    }

    /// Items as tall as their content, each at the top of its row.
    #[must_use]
    pub const fn hugging(mut self) -> Self {
        self.layout.hug = true;
        self
    }

    #[must_use]
    pub const fn gap(mut self, gap: Rhythm) -> Self {
        self.layout.gap = gap;
        self
    }

    /// Add a child spanning `span` columns.
    #[must_use]
    pub fn push_span(mut self, child: crate::widget::Element, span: u16) -> Self {
        let mut slot = Slot::new(child);
        slot.span = span.max(1);
        self.slots.push(slot);
        self
    }
}
