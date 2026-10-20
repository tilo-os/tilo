//! The layout primitives (tilolang layout design): `Stack`, `Row`, `Grid`, `Split`, `Page`,
//! `Layers`, with `Scroll` and `Spacer`.
//!
//! The model apps write against is **semantic**: children are hug-sized by default, fill the
//! remaining space by weight, and are bounded in `rem`, the text size, never in pixels. Gaps are
//! [`Rhythm`](crate::tokens::Rhythm) tokens, directions are leading and trailing, and the
//! adaptive patterns (a responsive grid, a split that collapses, a readable page) are single
//! primitives with their behaviour built in. The engine is Taffy flexbox and grid; the
//! arrangement a primitive takes is decided during layout, from the width it is offered.
//!
//! Constraints flow down and the parent places: a child never reads its own final size.

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
pub use page::{MEASURE_REM, Page, PageLayout};
pub use split::{Collapse, Pane, Side, Split, SplitLayout};
pub use units::{
    Align, Arrangement, At, Axis, Direction, Distribute, Fit, Length, Orientation, Sizing, Space,
    WidthClass, arrangement_for, columns_for, page_content_width, rem_of,
};

/// A vertical scroll view around `child`: the constraint on its axis is unbounded, so the child
/// hugs.
#[must_use]
pub fn scroll(child: super::Element) -> super::Scrollable {
    super::Scrollable::new_vertical().with_child(child)
}

#[cfg(test)]
mod tests;
