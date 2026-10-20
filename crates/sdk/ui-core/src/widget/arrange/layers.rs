//! `layers`: children drawn on top of each other, each at one of nine logical positions.

use taffy::style_helpers::{fr, length, line, minmax};
use taffy::{Size, Style};

use super::arranged::{Arranged, Layout, Resolved, Slot, wrapper_style};
use super::units::{Axis, Direction};

pub struct LayersLayout {
    direction: Direction,
}

impl Layout for LayersLayout {
    fn name(&self) -> &'static str {
        "Layers"
    }

    fn container(&self, _resolved: &Resolved) -> Style {
        Style {
            display: taffy::Display::Grid,
            grid_template_columns: vec![minmax(length(0.0_f32), fr(1.0_f32))],
            grid_template_rows: vec![taffy::style_helpers::auto()],
            min_size: Size {
                width: length(0.0_f32),
                height: taffy::Dimension::auto(),
            },
            ..Style::default()
        }
    }

    fn slot(&self, _index: usize, slot: &Slot, resolved: &Resolved) -> Style {
        let mut style = wrapper_style();
        slot.fit.apply(&mut style, Axis::Inline, resolved.rem);
        let cell = taffy::Line {
            start: line(1),
            end: taffy::GridPlacement::Auto,
        };
        style.grid_column = cell.clone();
        style.grid_row = cell;
        match slot.at {
            Some(at) => {
                let (inline, block) = at.aligns(self.direction);
                style.justify_self = Some(inline);
                style.align_self = Some(block);
            }
            None => {
                style.justify_self = Some(taffy::AlignSelf::Stretch);
                style.align_self = Some(taffy::AlignSelf::Stretch);
            }
        }
        style
    }
}

/// Children over each other; the first fills the cell, the others sit at their position.
pub type Layers = Arranged<LayersLayout>;

impl Arranged<LayersLayout> {
    #[must_use]
    pub fn layers() -> Self {
        Self::with_layout(LayersLayout {
            direction: Direction::Ltr,
        })
    }

    #[must_use]
    pub const fn direction(mut self, direction: Direction) -> Self {
        self.layout.direction = direction;
        self
    }

    /// Add a child at `at`.
    #[must_use]
    pub fn push_at(mut self, child: crate::widget::Element, at: super::units::At) -> Self {
        let mut slot = Slot::new(child);
        slot.at = Some(at);
        self.slots.push(slot);
        self
    }
}
