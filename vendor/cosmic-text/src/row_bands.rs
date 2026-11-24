// SPDX-License-Identifier: MIT OR Apache-2.0

//! Rows narrowed beside a floating picture: the vertical bands a buffer lays its rows in, and how
//! far each band is inset from either edge.

/// A band in a buffer's own vertical coordinates: rows whose top stands above `until` are laid out
/// between `left` and the buffer's width less `right`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RowBand {
    /// The band's bottom, in the buffer's coordinates from the first row's top.
    pub until: f32,
    /// How far the rows start from the left edge of the shaped width.
    pub left: f32,
    /// How far the rows end from the right edge of the shaped width.
    pub right: f32,
}

impl RowBand {
    /// A band reaching down to `until`, with `left` and `right` narrowed off either side.
    #[must_use]
    pub const fn new(until: f32, left: f32, right: f32) -> Self {
        Self { until, left, right }
    }
}

/// The bands beside a buffer's rows, sorted by how far down each reaches. A row belongs to the
/// band it stands in: the last band whose bottom is below the row's top. Bands empty, every row
/// has the buffer's whole width and a row of no band starts at the buffer's left edge.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowBands {
    bands: Vec<RowBand>,
}

impl RowBands {
    /// The bands `bands` describes, in whatever order they come: each row takes the one reaching
    /// furthest down that is still below it.
    #[must_use]
    pub fn new(mut bands: Vec<RowBand>) -> Self {
        bands.sort_by(|a, b| a.until.total_cmp(&b.until));
        Self { bands }
    }

    /// Whether any row is narrowed: the answer that lets a buffer keep its one-width layout.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }

    /// The insets `(left, right)` a row whose top stands at `y` is laid out between.
    #[must_use]
    pub fn insets_at(&self, y: f32) -> (f32, f32) {
        let mut insets = (0.0, 0.0);
        for band in &self.bands {
            if y < band.until {
                insets = (band.left, band.right);
            } else {
                break;
            }
        }
        insets
    }

    /// The width a row whose top stands at `y` is wrapped to, when the buffer has a width.
    #[must_use]
    pub fn width_at(&self, width: Option<f32>, y: f32) -> Option<f32> {
        let width = width?;
        let (left, right) = self.insets_at(y);
        if left == 0.0 && right == 0.0 {
            // A row no band narrows keeps the width it always had, to the bit.
            return Some(width);
        }
        Some((width - left - right).max(1.0))
    }
}
