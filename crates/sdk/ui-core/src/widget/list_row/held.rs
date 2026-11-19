//! What the list holding a row says of it: that the row stands in an outline, and the box the
//! list lifts the row in when it is chosen.

use vello::kurbo::Rect;

use super::ListRow;

impl ListRow {
    /// Stand as a row of an outline ([`super::ListRowFace::outline`]): a tree's list says it of every
    /// row it mounts, so each of its trees reads alike however the app built the rows.
    pub fn set_outline(&mut self) {
        if !self.outline {
            self.outline = true;
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
