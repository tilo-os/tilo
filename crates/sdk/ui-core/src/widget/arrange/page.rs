//! `page`: the root of a document-like view (layout design 3).

use taffy::style_helpers::length;
use taffy::{Size, Style};

use super::arranged::{Arranged, Layout, Resolved, Slot, wrapper_style};
use super::units::{Length, WidthClass};

/// The readable width of a page, in `rem`.
// ratchet-allow(pixel_const): a width in rem, which the text size scales, not pixels
pub const MEASURE_REM: f32 = 40.0;

pub struct PageLayout {
    measure: Option<Length>,
    class: Option<WidthClass>,
    /// One row of the page's content, which a virtual list stacks: the gutter at the sides
    /// only, and the row's own spacing above and below.
    row: Option<RowSpacing>,
}

/// The space a row of a page's content ([`Page::row`]) has above and below it.
#[derive(Clone, Copy, Debug)]
enum RowSpacing {
    /// Half the sibling rhythm above and below: rows a sibling apart.
    Halves,
    /// The gap to the row before it above, nothing below: a transcript's message, the next
    /// line of a run closer than the next run.
    After(crate::tokens::Rhythm),
    /// A section's gap above, the gutter below: the bar at a column's foot, close under what
    /// it follows.
    Foot,
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
        let (top, bottom) = match self.row {
            Some(RowSpacing::Halves) => {
                let half = length(resolved.gap(crate::tokens::Rhythm::Sibling) / 2.0);
                (half, half)
            }
            Some(RowSpacing::After(gap)) => (length(resolved.gap(gap)), length(0.0_f32)),
            Some(RowSpacing::Foot) => {
                (length(resolved.gap(crate::tokens::Rhythm::Section)), gutter)
            }
            None => (gutter, gutter),
        };
        Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            align_items: Some(taffy::AlignItems::Center),
            padding: taffy::Rect {
                left: gutter,
                right: gutter,
                top,
                bottom,
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
            row: None,
        })
    }

    /// `page(full)`: no bound, for canvases, tables, maps and media.
    #[must_use]
    pub fn full() -> Self {
        Self::with_layout(PageLayout {
            measure: None,
            class: None,
            row: None,
        })
    }

    /// One row of a page's content, for a virtual list that shows the page's rows one by one
    /// (a transcript): the page's gutter at its sides, and half the sibling rhythm above and
    /// below, so the rows it stacks sit a sibling apart in the page's column as a page's
    /// children do.
    #[must_use]
    pub const fn row(mut self) -> Self {
        self.layout.row = Some(RowSpacing::Halves);
        self
    }

    /// One row of a page's content that stands `gap` below the row before it and has nothing
    /// below it: a transcript's message, whose run's next line sits closer than the next run.
    /// Its list leaves [`TranscriptEdges`] around the rows.
    #[must_use]
    pub const fn row_after(mut self, gap: crate::tokens::Rhythm) -> Self {
        self.layout.row = Some(RowSpacing::After(gap));
        self
    }

    /// The bar at the foot of a page's column (a conversation's composer): the page's gutter
    /// at its sides and below it, and a section's gap above, so it sits close under the content
    /// it follows.
    #[must_use]
    pub const fn foot(mut self) -> Self {
        self.layout.row = Some(RowSpacing::Foot);
        self
    }

    /// Another readable width.
    #[must_use]
    pub const fn measure(mut self, measure: Length) -> Self {
        self.layout.measure = Some(measure);
        self
    }
}

/// The space a list of page rows ([`Page::row`]) leaves above its first row and below its last
/// ([`crate::widget::VirtualList::with_edges`]): the page's gutter, less the half sibling a row
/// has there, so the first row's content stands where a page's first child does, under the
/// page's top spacing.
#[derive(Debug, Default)]
pub struct PageRowEdges {
    resolved: Option<Resolved>,
    /// The width class of the rows, with the history a page keeps.
    class: Option<WidthClass>,
}

impl PageRowEdges {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            resolved: None,
            class: None,
        }
    }
}

impl crate::widget::virtual_list::ListEdges for PageRowEdges {
    fn theme(&mut self, theme: &crate::theme::Theme) {
        self.resolved = Some(Resolved::of(theme));
    }

    fn edges(&mut self, width: f64) -> (f64, f64) {
        let Some(resolved) = self.resolved else {
            return (0.0, 0.0);
        };
        let width = crate::num::f32_of_f64(width);
        let class = WidthClass::with_history(width / resolved.rem, self.class);
        self.class = Some(class);
        let layout = PageLayout {
            measure: None,
            class: Some(class),
            row: Some(RowSpacing::Halves),
        };
        let edge = (layout.gutter(&resolved) - resolved.gap(crate::tokens::Rhythm::Sibling) / 2.0)
            .max(0.0);
        (f64::from(edge), f64::from(edge))
    }
}

impl Default for Arranged<PageLayout> {
    fn default() -> Self {
        Self::new()
    }
}

/// The space a transcript's list of page rows ([`Page::row_after`]) leaves above its first row
/// and below its last: above, the page's gutter less the gap the first row has over it, so the
/// first message stands where a page's first child does; below, nothing, since what follows a
/// transcript (its composer) brings its own spacing, and the newest message sits right above
/// it.
#[derive(Debug)]
pub struct TranscriptEdges {
    resolved: Option<Resolved>,
    class: Option<WidthClass>,
    /// The gap the first row has above it.
    first: crate::tokens::Rhythm,
}

impl TranscriptEdges {
    /// Edges for rows whose first stands `first` below the row before it.
    #[must_use]
    pub const fn new(first: crate::tokens::Rhythm) -> Self {
        Self {
            resolved: None,
            class: None,
            first,
        }
    }
}

impl crate::widget::virtual_list::ListEdges for TranscriptEdges {
    fn theme(&mut self, theme: &crate::theme::Theme) {
        self.resolved = Some(Resolved::of(theme));
    }

    fn edges(&mut self, width: f64) -> (f64, f64) {
        let Some(resolved) = self.resolved else {
            return (0.0, 0.0);
        };
        let width = crate::num::f32_of_f64(width);
        let class = WidthClass::with_history(width / resolved.rem, self.class);
        self.class = Some(class);
        let layout = PageLayout {
            measure: None,
            class: Some(class),
            row: Some(RowSpacing::After(self.first)),
        };
        let lead = (layout.gutter(&resolved) - resolved.gap(self.first)).max(0.0);
        (f64::from(lead), 0.0)
    }
}
