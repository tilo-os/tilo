//! `split`: the adaptive two-pane pattern (layout design 3).
//!
//! Both panes always exist. The arrangement is decided *during layout* from the width the split
//! is offered: side by side while both panes' minimums fit, otherwise by the collapse mode.

use taffy::style_helpers::length;
use taffy::{Size, Style};

use super::arranged::{Arranged, Layout, Resolved, Slot, wrapper_style};
use super::units::{Arrangement, Axis, Direction, Length, arrangement_for};
use crate::tokens::Rhythm;

/// Which pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Primary,
    Secondary,
}

/// What a split does when both panes do not fit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Collapse {
    /// The secondary pane under the primary.
    #[default]
    Stack,
    /// The secondary pane is closed until the person opens it, and then stands under the
    /// primary.
    Drawer,
    /// One pane at a time; the host offers the way back.
    Navigate,
}

/// The edge the primary pane stands at when side by side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    #[default]
    Leading,
    Trailing,
}

pub struct SplitLayout {
    primary_min: Length,
    secondary_min: Length,
    side: Side,
    /// The primary pane's share of the width when side by side.
    ratio: f32,
    collapse: Collapse,
    gap: Rhythm,
    direction: Direction,
    arrangement: Arrangement,
    /// The pane `Navigate` shows, and whether the `Drawer` is open.
    shown: Pane,
    drawer_open: bool,
    dirty: bool,
}

impl SplitLayout {
    /// How the panes stand now.
    #[must_use]
    pub const fn arrangement(&self) -> Arrangement {
        self.arrangement
    }

    /// Whether the panes do not stand side by side.
    #[must_use]
    pub fn collapsed(&self) -> bool {
        self.arrangement == Arrangement::Collapsed
    }

    /// The collapse mode.
    #[must_use]
    pub const fn collapse_mode(&self) -> Collapse {
        self.collapse
    }

    /// The pane `Navigate` shows.
    #[must_use]
    pub const fn shown(&self) -> Pane {
        self.shown
    }

    /// Show `pane` while navigating.
    pub fn show(&mut self, pane: Pane) {
        if self.shown != pane {
            self.shown = pane;
            self.dirty = true;
        }
    }

    /// Open or close the drawer.
    pub fn set_drawer_open(&mut self, open: bool) {
        if self.drawer_open != open {
            self.drawer_open = open;
            self.dirty = true;
        }
    }

    #[must_use]
    pub const fn drawer_open(&self) -> bool {
        self.drawer_open
    }

    fn pane_of(index: usize) -> Pane {
        if index == 0 {
            Pane::Primary
        } else {
            Pane::Secondary
        }
    }

    fn row(&self) -> taffy::FlexDirection {
        // The primary pane is first in the source; trailing puts it at the other end.
        let reversed = self.direction.is_rtl() != (self.side == Side::Trailing);
        if reversed {
            taffy::FlexDirection::RowReverse
        } else {
            taffy::FlexDirection::Row
        }
    }
}

impl Layout for SplitLayout {
    fn name(&self) -> &'static str {
        "Split"
    }

    fn container(&self, resolved: &Resolved) -> Style {
        let gap = length(resolved.gap(self.gap));
        Style {
            display: taffy::Display::Flex,
            flex_direction: match self.arrangement {
                Arrangement::SideBySide => self.row(),
                Arrangement::Collapsed => taffy::FlexDirection::Column,
            },
            align_items: Some(taffy::AlignItems::Stretch),
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

    fn slot(&self, index: usize, _slot: &Slot, resolved: &Resolved) -> Style {
        let mut style = wrapper_style();
        let pane = Self::pane_of(index);
        let min = match pane {
            Pane::Primary => self.primary_min,
            Pane::Secondary => self.secondary_min,
        };
        if self.arrangement == Arrangement::SideBySide {
            let share = match pane {
                Pane::Primary => self.ratio,
                Pane::Secondary => 1.0 - self.ratio,
            };
            style.flex_grow = share.max(0.0);
            style.flex_shrink = 1.0;
            style.flex_basis = length(0.0_f32);
            style.min_size.width = length(min.resolve(resolved.rem));
        }
        if self.hidden(index) {
            style.display = taffy::Display::None;
        }
        style
    }

    fn settle(&mut self, allocated: Size<f32>, resolved: &Resolved) -> bool {
        let arrangement = arrangement_for(
            allocated.width,
            self.primary_min.resolve(resolved.rem),
            self.secondary_min.resolve(resolved.rem),
            resolved.gap(self.gap),
        );
        let changed = arrangement != self.arrangement || self.dirty;
        self.arrangement = arrangement;
        self.dirty = false;
        changed
    }

    fn hidden(&self, index: usize) -> bool {
        if self.arrangement == Arrangement::SideBySide {
            return false;
        }
        let pane = Self::pane_of(index);
        match self.collapse {
            Collapse::Stack => false,
            Collapse::Drawer => pane == Pane::Secondary && !self.drawer_open,
            Collapse::Navigate => pane != self.shown,
        }
    }
}

/// The adaptive two-pane pattern. Add the primary pane, then the secondary.
pub type Split = Arranged<SplitLayout>;

impl Arranged<SplitLayout> {
    /// A split whose panes need `primary_min` and `secondary_min` to stand side by side.
    #[must_use]
    pub fn new(primary_min: Length, secondary_min: Length) -> Self {
        Self::with_layout(SplitLayout {
            primary_min,
            secondary_min,
            side: Side::Leading,
            ratio: 0.4,
            collapse: Collapse::Stack,
            gap: Rhythm::Section,
            direction: Direction::Ltr,
            arrangement: Arrangement::SideBySide,
            shown: Pane::Primary,
            drawer_open: false,
            dirty: false,
        })
    }

    #[must_use]
    pub const fn side(mut self, side: Side) -> Self {
        self.layout.side = side;
        self
    }

    /// The primary pane's share of the width when side by side, between 0 and 1.
    #[must_use]
    pub const fn ratio(mut self, ratio: f32) -> Self {
        self.layout.ratio = ratio.clamp(0.05, 0.95);
        self
    }

    #[must_use]
    pub const fn collapse(mut self, collapse: Collapse) -> Self {
        self.layout.collapse = collapse;
        self
    }

    #[must_use]
    pub const fn direction(mut self, direction: Direction) -> Self {
        self.layout.direction = direction;
        self
    }

    /// The axis the panes run along when side by side.
    #[must_use]
    pub const fn axis() -> Axis {
        Axis::Inline
    }
}
