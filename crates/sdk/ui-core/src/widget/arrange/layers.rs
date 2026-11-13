//! `layers`: children drawn on top of each other, each at one of nine logical positions.

use taffy::style_helpers::{fr, length, line, minmax, percent};
use taffy::{Size, Style};

use super::Side;
use super::arranged::{Arranged, Layout, Resolved, Slot, lead_container, wrapper_style};
use super::units::{Axis, Direction};

pub struct LayersLayout {
    direction: Direction,
    straddling: Vec<usize>,
}

impl Layout for LayersLayout {
    fn name(&self) -> &'static str {
        "Layers"
    }

    fn container(&self, _resolved: &Resolved, lead: f32) -> Style {
        lead_container(
            Style {
                display: taffy::Display::Grid,
                grid_template_columns: vec![minmax(length(0.0_f32), fr(1.0_f32))],
                // As tall as the tallest layer where the layers' height is open (a card's image
                // under its badge), and no taller than a height they are given (a window's desk):
                // a layer longer than that scrolls or overflows inside it, never the cell.
                grid_template_rows: vec![minmax(length(0.0_f32), fr(1.0_f32))],
                min_size: Size {
                    width: length(0.0_f32),
                    height: taffy::Dimension::auto(),
                },
                ..Style::default()
            },
            lead,
            self.leading(),
        )
    }

    fn leading(&self) -> Side {
        match self.direction {
            Direction::Rtl => Side::Trailing,
            Direction::Ltr => Side::Leading,
        }
    }

    fn slot(&self, _index: usize, slot: &Slot, resolved: &Resolved) -> Style {
        let mut style = wrapper_style();
        slot.fit.apply(&mut style, Axis::Inline, resolved.rem);
        // A layer that fills takes the whole cell. A grid child's flex-grow says
        // nothing, so the fill is a width the cell resolves, and the layer keeps
        // its own height (a bar at the top is as tall as the bar).
        if slot.fit.width.fill.is_some() {
            style.size.width = percent(1.0_f32);
            style.flex_grow = 0.0;
            style.flex_basis = taffy::Dimension::auto();
        }
        // A layer placed at an edge keeps its own height (a bar at the top is as tall
        // as the bar); only the layer that takes the whole cell, the one with no
        // position, fills it top to bottom.
        if slot.at.is_none() && slot.fit.height.fill.is_some() {
            style.size.height = percent(1.0_f32);
        }
        let cell = taffy::Line {
            start: line(1),
            end: taffy::GridPlacement::Auto,
        };
        style.grid_column = cell.clone();
        style.grid_row = cell;
        let (inline, block) = slot.at.map_or(
            (taffy::AlignSelf::Stretch, taffy::AlignSelf::Stretch),
            |at| at.aligns(self.direction),
        );
        style.justify_self = Some(inline);
        style.align_self = Some(block);
        if self.straddling.contains(&_index) {
            style.size.height = length(0.0_f32);
            style.min_size.height = length(0.0_f32);
            style.justify_content = Some(taffy::JustifyContent::Center);
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
            straddling: Vec::new(),
        })
    }

    #[must_use]
    pub const fn direction(mut self, direction: Direction) -> Self {
        self.layout.direction = direction;
        self
    }

    #[must_use]
    pub fn push_at(mut self, child: crate::widget::Element, at: super::units::At) -> Self {
        let mut slot = Slot::new(child);
        slot.at = Some(at);
        self.slots.push(slot);
        self
    }

    /// Add a child at `at`, centred on that edge of the cell rather than inside it: half over
    /// the cell, half outside (at a top position, on the top edge).
    #[must_use]
    pub fn push_straddling(self, child: crate::widget::Element, at: super::units::At) -> Self {
        let index = self.slots.len();
        let mut layers = self.push_at(child, at);
        layers.layout.straddling.push(index);
        layers
    }
}
