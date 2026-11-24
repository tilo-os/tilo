//! `split`: the adaptive two-pane pattern.

use taffy::style_helpers::length;
use taffy::{Size, Style};

use super::arranged::{Arranged, Layout, Part, Resolved, lead_container, wrapper_style};
use super::units::{Axis, Direction, Length, PaneSplit, pane_split_for};
use crate::tokens::Rhythm;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Primary,
    Secondary,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Collapse {
    #[default]
    Stack,
    /// The secondary pane is closed until the person opens it, and then stands under the
    /// primary.
    Drawer,
    Navigate,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    #[default]
    Leading,
    Trailing,
}

#[derive(Clone, Debug, Default)]
pub struct Navigation {
    secondary: std::rc::Rc<std::cell::Cell<bool>>,
    collapsed: std::rc::Rc<std::cell::Cell<bool>>,
}

impl Navigation {
    #[must_use]
    pub fn shown(&self) -> Pane {
        if self.secondary.get() {
            Pane::Secondary
        } else {
            Pane::Primary
        }
    }

    pub fn show(&self, pane: Pane) {
        self.secondary.set(pane == Pane::Secondary);
    }

    #[must_use]
    pub fn collapsed(&self) -> bool {
        self.collapsed.get()
    }

    #[must_use]
    pub fn detail_alone(&self) -> bool {
        self.collapsed() && self.shown() == Pane::Secondary
    }
}

pub struct SplitLayout {
    primary_min: Length,
    secondary_min: Length,
    side: Side,
    ratio: f32,
    collapse: Collapse,
    gap: Rhythm,
    direction: Direction,
    panes: PaneSplit,
    shown: Pane,
    drawer_open: bool,
    navigation: Option<Navigation>,
}

impl SplitLayout {
    #[must_use]
    pub const fn panes(&self) -> PaneSplit {
        self.panes
    }

    #[must_use]
    pub fn collapsed(&self) -> bool {
        self.panes == PaneSplit::Collapsed
    }

    #[must_use]
    pub const fn collapse_mode(&self) -> Collapse {
        self.collapse
    }

    #[must_use]
    pub const fn shown(&self) -> Pane {
        self.shown
    }

    /// Show `pane` while navigating.
    pub const fn show(&mut self, pane: Pane) {
        self.shown = pane;
    }

    /// Open or close the drawer.
    pub const fn set_drawer_open(&mut self, open: bool) {
        self.drawer_open = open;
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

    fn container(&self, resolved: &Resolved, lead: f32) -> Style {
        let gap = length(resolved.gap(self.gap));
        lead_container(
            Style {
                display: taffy::Display::Flex,
                flex_direction: match self.panes {
                    PaneSplit::SideBySide => self.row(),
                    PaneSplit::Collapsed => taffy::FlexDirection::Column,
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
            },
            lead,
            self.leading(),
        )
    }

    fn part(&self, index: usize, _part: &Part, resolved: &Resolved) -> Style {
        let mut style = wrapper_style();
        let pane = Self::pane_of(index);
        let min = match pane {
            Pane::Primary => self.primary_min,
            Pane::Secondary => self.secondary_min,
        };
        if self.panes == PaneSplit::SideBySide {
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

    fn settles(&self) -> bool {
        true
    }

    /// The layout for the width this solve allocated, and the pane a host's navigation
    /// asks for. Whether the collapse is told to the host waits for [`Self::applied`]: a solve
    /// in a copy of the window's tree (a region measuring the split) is not where it stands.
    fn settle(&mut self, allocated: Size<f32>, resolved: &Resolved) -> bool {
        let panes = pane_split_for(
            allocated.width,
            self.primary_min.resolve(resolved.rem),
            self.secondary_min.resolve(resolved.rem),
            resolved.gap(self.gap),
        );
        let mut changed = panes != self.panes;
        self.panes = panes;
        if let Some(navigation) = &self.navigation {
            let wanted = navigation.shown();
            changed |= wanted != self.shown;
            self.shown = wanted;
        }
        changed
    }

    fn applied(&mut self) {
        if let Some(navigation) = &self.navigation {
            navigation.collapsed.set(self.panes == PaneSplit::Collapsed);
        }
    }

    fn hidden(&self, index: usize) -> bool {
        if self.panes == PaneSplit::SideBySide {
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

pub type Split = Arranged<SplitLayout>;

impl Arranged<SplitLayout> {
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
            panes: PaneSplit::SideBySide,
            shown: Pane::Primary,
            drawer_open: false,
            navigation: None,
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
    pub fn navigation(&mut self) -> Navigation {
        let navigation = self
            .layout
            .navigation
            .get_or_insert_with(Navigation::default)
            .clone();
        navigation.show(self.layout.shown);
        navigation
    }

    #[must_use]
    pub const fn direction(mut self, direction: Direction) -> Self {
        self.layout.direction = direction;
        self
    }

    /// Open or close the drawer, with the panes moving as a change of what the container holds
    /// does (the container's [`crate::motion::Feel`], reduced motion landing at once). A split
    /// that is not collapsed ignores it.
    pub fn set_drawer_open(&mut self, open: bool) {
        if self.layout.drawer_open == open {
            return;
        }
        self.note_change();
        self.layout.drawer_open = open;
        self.arguments_changed();
    }

    /// Whether the drawer is open.
    #[must_use]
    pub const fn drawer_open(&self) -> bool {
        self.layout.drawer_open
    }

    #[must_use]
    pub const fn axis() -> Axis {
        Axis::Inline
    }
}
