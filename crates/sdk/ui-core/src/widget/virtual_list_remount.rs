//! The rows of a [`super::VirtualList`] laid out where they stand, and the
//! rows a scroll brings near mounted in place: each entering row's layout is
//! built alone and the rows already there keep theirs, so a row coming into
//! view costs its own layout, not the list's
//! ([`crate::widget::Widget::remount_children`]).

use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, TaffyTree};

use super::{ListModel, VirtualList, px};
use crate::widget::Remount;

/// How far the view may scroll from where the rows' layout was placed
/// before they are placed again from where it stands: taffy lays out in
/// `f32`, which stays well under a hundredth of a pixel this near.
const REPLACE_AFTER_PX: f64 = 16_384.0;

impl<M: ListModel + 'static> VirtualList<M> {
    /// Stand each mounted row one bleed in from the list's edges (so its
    /// lift fills the list; beside the bar it leaves the bar's gutter), at
    /// its top in the list less [`Self::layout_offset`]. Only a row whose
    /// place changed is touched, so taffy keeps every other row's layout.
    pub(super) fn place_rows(&mut self, taffy: &mut TaffyTree<()>) {
        self.rows_beside_bar = self.scroll_metrics().overflows();
        let left = length(px(self.bleed));
        let right = length(px(self.bleed
            + if self.rows_beside_bar {
                self.gutter
            } else {
                0.0
            }));
        for row in &self.mounted {
            let Some(child) = row.node else {
                continue;
            };
            let top = self.viewport.item_top(row.index).unwrap_or_default() - self.layout_offset;
            let Ok(current) = taffy.style(child) else {
                continue;
            };
            let mut style = current.clone();
            style.position = taffy::Position::Absolute;
            style.inset.left = left;
            style.inset.right = right;
            style.inset.top = length(px(top));
            style.size.width = Dimension::auto();
            if style != *current {
                let _ = taffy.set_style(child, style);
            }
        }
    }

    /// [`crate::widget::Widget::remount_children`]: mount the rows now near the view and
    /// let go of the others, building only the new rows' layouts, and
    /// place the rows; the rows that stay carry out their own changes.
    /// `false` when the list has no node of the last build to mount under.
    pub(super) fn remount_rows(&mut self, remount: &mut Remount<'_>) -> bool {
        let Some(node) = self.layout_node else {
            return false;
        };
        let before: Vec<NodeId> = self.mounted.iter().filter_map(|row| row.node).collect();
        self.reconcile_mounted();
        // The rows it built are the ones laid out here.
        self.structure_dirty = false;
        self.remount = false;
        let mut carried = true;
        let mut children = Vec::with_capacity(self.mounted.len());
        for row in &mut self.mounted {
            let child = match row.node {
                Some(child) => {
                    carried &= row.widget.remount_children(remount);
                    child
                }
                None => remount.mount(node, row.widget.as_mut()),
            };
            row.node = Some(child);
            children.push(child);
        }
        if (self.viewport.offset() - self.layout_offset).abs() > REPLACE_AFTER_PX {
            self.layout_offset = self.viewport.offset();
        }
        self.place_rows(remount.taffy());
        if remount.set_children(node, &children).is_err() {
            return false;
        }
        for gone in before.into_iter().filter(|gone| !children.contains(gone)) {
            remount.unmount(gone);
        }
        carried
    }
}
