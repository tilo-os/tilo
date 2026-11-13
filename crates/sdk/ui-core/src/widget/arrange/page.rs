//! `page`: the root of a document-like view.

use taffy::style_helpers::length;
use taffy::{Size, Style};

use super::arranged::{Arranged, Layout, Resolved, Slot, wrapper_style};
use super::units::Length;

/// The readable width of a page, in `rem`.
// ratchet-allow(pixel_const): a width in rem, which the text size scales, not pixels
pub const MEASURE_REM: f32 = 40.0;

pub struct PageLayout {
    measure: Option<Length>,
    /// Take the height the page is given: a canvas (a board, a map) fills the pane it is in
    /// instead of being as tall as what it holds, and keeps its gutter above and below rather
    /// than beside it.
    fill: bool,
    /// One row of the page's content, which a virtual list stacks: the gutter at the sides
    /// only, and the row's own spacing above and below.
    row: Option<RowSpacing>,
}

#[derive(Clone, Copy, Debug)]
enum RowSpacing {
    After(crate::tokens::Rhythm),
    Foot,
}

/// The gutter a page keeps between its content and the column it is read in: the toolkit's
/// content gutter (`Rhythm::Group`), the same at every width, as the shell's columns and the
/// Rust pages keep it. A page's sides, its ends and the gap a notice has over and under it are
/// then one distance at every window size, so what stands in a page stands the same distance
/// from everything around it.
fn content_gutter(resolved: &Resolved) -> f32 {
    resolved.gap(crate::tokens::Rhythm::Group)
}

impl Layout for PageLayout {
    fn name(&self) -> &'static str {
        "Page"
    }

    fn readable_column(&self) -> Option<Length> {
        self.measure
    }

    fn container(&self, resolved: &Resolved, lead: f32) -> Style {
        let gutter = length(content_gutter(resolved));
        let side = if self.fill {
            length(lead)
        } else {
            length(content_gutter(resolved) + lead)
        };
        let (top, bottom) = match self.row {
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
                left: side,
                right: side,
                top,
                bottom,
            },
            size: Size {
                width: taffy::style_helpers::percent(1.0_f32),
                // A page that fills is as tall as the pane it is in: what it holds stands on
                // the pane's floor and scrolls inside itself (a board's lanes), instead of the
                // page growing past the pane with what it holds.
                height: if self.fill {
                    taffy::style_helpers::percent(1.0_f32)
                } else {
                    taffy::Dimension::auto()
                },
            },
            ..Style::default()
        }
    }

    fn slot(&self, _index: usize, _slot: &Slot, resolved: &Resolved) -> Style {
        let mut style = wrapper_style();
        style.size.width = taffy::style_helpers::percent(1.0_f32);
        if self.fill {
            style.size.height = taffy::style_helpers::percent(1.0_f32);
        }
        if let Some(measure) = self.measure {
            style.max_size.width = length(measure.resolve(resolved.rem));
        }
        style
    }
}

pub type Page = Arranged<PageLayout>;

impl Arranged<PageLayout> {
    /// A page whose content is at most about 40 rem wide.
    #[must_use]
    pub fn new() -> Self {
        Self::with_layout(PageLayout {
            measure: Some(Length::Rem(MEASURE_REM)),
            fill: false,
            row: None,
        })
    }

    #[must_use]
    pub fn full() -> Self {
        Self::with_layout(PageLayout {
            measure: None,
            fill: false,
            row: None,
        })
    }

    /// `page(fill)`: the height the page is given, for a canvas that fills its pane (a board's
    /// lanes, a map, a media page) instead of growing with what it holds. A page that fills its
    /// pane is a surface: it also takes the pane's whole width, and what it holds keeps its own
    /// gutter at the sides.
    #[must_use]
    pub const fn filling(mut self) -> Self {
        self.layout.fill = true;
        self
    }

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

    #[must_use]
    pub const fn measure(mut self, measure: Length) -> Self {
        self.layout.measure = Some(measure);
        self
    }

    pub const fn set_readable_measure(&mut self, measure: Option<Length>) {
        self.layout.measure = measure;
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
    first: crate::tokens::Rhythm,
}

impl TranscriptEdges {
    #[must_use]
    pub const fn new(first: crate::tokens::Rhythm) -> Self {
        Self {
            resolved: None,
            first,
        }
    }
}

impl crate::widget::virtual_list::ListEdges for TranscriptEdges {
    fn theme(&mut self, theme: &crate::theme::Theme) {
        self.resolved = Some(Resolved::of(theme));
    }

    fn edges(&mut self, _width: f64) -> (f64, f64) {
        let Some(resolved) = self.resolved else {
            return (0.0, 0.0);
        };
        let lead = (content_gutter(&resolved) - resolved.gap(self.first)).max(0.0);
        (f64::from(lead), 0.0)
    }
}
