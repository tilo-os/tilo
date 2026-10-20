//! `page`: the root of a document-like view (layout design 3).

use taffy::style_helpers::length;
use taffy::{Size, Style};

use super::arranged::{Arranged, Layout, Resolved, Slot, wrapper_style};
use super::units::{Length, WidthClass};

/// The readable width of a page, in `rem`.
pub const MEASURE_REM: f32 = 40.0;

pub struct PageLayout {
    measure: Option<Length>,
    class: Option<WidthClass>,
}

impl PageLayout {
    /// The width class of the last layout.
    #[must_use]
    pub const fn class(&self) -> Option<WidthClass> {
        self.class
    }

    fn gutter(&self, resolved: &Resolved) -> f32 {
        resolved.gap(crate::tokens::Rhythm::Group)
            * self.class.unwrap_or(WidthClass::Medium).gutter_steps()
    }
}

impl Layout for PageLayout {
    fn name(&self) -> &'static str {
        "Page"
    }

    fn container(&self, resolved: &Resolved) -> Style {
        let gutter = length(self.gutter(resolved));
        Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            align_items: Some(taffy::AlignItems::Center),
            padding: taffy::Rect {
                left: gutter,
                right: gutter,
                top: gutter,
                bottom: gutter,
            },
            size: Size {
                width: taffy::style_helpers::percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
            ..Style::default()
        }
    }

    fn slot(&self, _index: usize, _slot: &Slot, resolved: &Resolved) -> Style {
        let mut style = wrapper_style();
        style.size.width = taffy::style_helpers::percent(1.0_f32);
        if let Some(measure) = self.measure {
            style.max_size.width = length(measure.resolve(resolved.rem));
        }
        style
    }

    fn settle(&mut self, allocated: Size<f32>, resolved: &Resolved) -> bool {
        let class = WidthClass::with_history(allocated.width / resolved.rem, self.class);
        let changed = self.class != Some(class);
        self.class = Some(class);
        changed
    }
}

/// Content bounded to a readable width and centred, with a gutter that scales with the space.
pub type Page = Arranged<PageLayout>;

impl Arranged<PageLayout> {
    /// A page whose content is at most about 40 rem wide.
    #[must_use]
    pub fn new() -> Self {
        Self::with_layout(PageLayout {
            measure: Some(Length::Rem(MEASURE_REM)),
            class: None,
        })
    }

    /// `page(full)`: no bound, for canvases, tables, maps and media.
    #[must_use]
    pub fn full() -> Self {
        Self::with_layout(PageLayout {
            measure: None,
            class: None,
        })
    }

    /// Another readable width.
    #[must_use]
    pub const fn measure(mut self, measure: Length) -> Self {
        self.layout.measure = Some(measure);
        self
    }
}

impl Default for Arranged<PageLayout> {
    fn default() -> Self {
        Self::new()
    }
}
