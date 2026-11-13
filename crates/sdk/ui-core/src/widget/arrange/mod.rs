//! The layout primitives: `Stack`, `Row`, `Grid`, `Split`, `Page`, `Layers`, with `Scroll` and
//! `Spacer`.

mod arranged;
mod flex;
mod grid;
mod layers;
mod page;
mod split;
mod units;

pub use arranged::{Arranged, Layout, Resolved, Slot};
pub use flex::{FlexLayout, Row, Stack};
pub use grid::{Columns, Grid, GridLayout};
pub use layers::{Layers, LayersLayout};
pub use page::{MEASURE_REM, Page, PageLayout, TranscriptEdges};
pub use split::{Collapse, Navigation, Pane, Side, Split, SplitLayout};
pub use units::{
    Align, Arrangement, At, Axis, Direction, Distribute, Fit, Length, Orientation, Sizing, Space,
    WidthClass, arrangement_for, class_min_width, column_min_width, columns_for,
    page_content_width, rem_of,
};

/// A vertical scroll view around `child`: the constraint on its axis is unbounded, so the child
/// hugs.
#[must_use]
pub fn scroll(child: super::Element) -> super::Scrollable {
    super::Scrollable::new_vertical().with_child(child)
}

#[cfg(test)]
mod tests;
