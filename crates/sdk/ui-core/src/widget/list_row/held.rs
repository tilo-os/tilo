//! What the list holding a row says of it: that the row stands in an outline, and the box the
//! list lifts the row in when it is chosen.

use vello::kurbo::Rect;

use super::{ListRow, ListRowFace};

impl ListRow {
    /// Stand as a row of an outline ([`ListRowFace::outline`]), its content after the
    /// `chevron_column` the tree draws its chevrons in: a tree's list says it of every row it
    /// mounts, so each of its trees reads alike however the app built the rows.
    pub fn set_outline(&mut self, chevron_column: f64) {
        let chevron_column = chevron_column.max(0.0);
        if !self.outline || (self.chevron_column - chevron_column).abs() > f64::EPSILON {
            self.outline = true;
            self.chevron_column = chevron_column;
            self.dirty = true;
        }
    }

    /// Paint the row's hover and selection over `lift_box`, the box the list that holds it lifts
    /// a chosen row in: one box whether the list or the row draws the state, so a chosen row
    /// under the pointer never shows a second, smaller box inside its own.
    pub fn set_lift_box(&mut self, lift_box: Option<Rect>) {
        if self.lift_box != lift_box {
            self.lift_box = lift_box;
            self.dirty = true;
        }
    }
}

impl ListRowFace<'_> {
    /// Show the row as a row of an outline: a sidebar's tree reads many rows at once, so its
    /// title takes the label's size and the row the tree's own height
    /// ([`crate::widget::TREE_ROW_HEIGHT`]) unless it says another.
    #[must_use]
    pub fn outline(mut self) -> Self {
        self.outline = true;
        self.row_height
            .get_or_insert(f64::from(crate::widget::TREE_ROW_HEIGHT));
        self
    }
}
