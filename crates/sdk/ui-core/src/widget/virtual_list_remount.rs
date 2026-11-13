//! The rows of a [`super::VirtualList`] laid out where they stand, and the rows a scroll brings
//! near mounted in place: each entering row's layout is built alone and the rows already there keep
//! theirs, so a row coming into view costs its own layout, not the list's
//! ([`crate::widget::Widget::remount_children`]).
use std::ops::Range;

use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, TaffyTree};

use super::{ListModel, VirtualList, px};
use crate::widget::Remount;

/// How far the view may scroll from where the rows' layout was placed
/// before they are placed again from where it stands: taffy lays out in
/// `f32`, which stays well under a hundredth of a pixel this near.
const REPLACE_AFTER_PX: f64 = 16_384.0;

impl<M: ListModel> VirtualList<M> {
    pub(super) fn mount_window(&self) -> Range<usize> {
        let near = self.rows_to_hold();
        let mounted = self.mounted_range.clone();
        if self.mounted_covers(&near) {
            let count = self.model.len();
            return mounted.start.min(count)..mounted.end.min(count);
        }
        if near.end <= mounted.start || near.start >= mounted.end {
            return near;
        }
        let overscan = self.viewport.overscan();
        let ahead = overscan + self.viewport.viewport();
        if near.start < mounted.start {
            self.viewport.range_around(ahead, overscan)
        } else {
            self.viewport.range_around(overscan, ahead)
        }
    }

    pub(super) fn mounted_covers(&self, near: &Range<usize>) -> bool {
        if near.is_empty() {
            return self.mounted_range.is_empty();
        }
        self.mounted_range.start <= near.start && near.end <= self.mounted_range.end
    }

    pub(super) fn mounted_near(&self) -> Range<usize> {
        let reach = crate::scroll_copy::EFFECT_REACH + self.give().abs();
        let height = self.rect.height();
        let start = self.mounted.partition_point(|row| {
            let Some(top) = self.viewport.item_view_top(row.index) else {
                return true;
            };
            let bottom = top + self.viewport.item_height(row.index).unwrap_or_default();
            bottom + reach <= 0.0
        });
        let end = self.mounted.partition_point(|row| {
            self.viewport
                .item_view_top(row.index)
                .is_none_or(|top| top - reach < height)
        });
        start..end.max(start)
    }

    pub(super) fn collect_row_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        let first = regions.len();
        for row in self.mounted.get(self.placed_near()).unwrap_or_default() {
            row.widget.collect_damage(regions);
        }
        // The field renaming a row in place draws over that row as part of the
        // list, and says where its own pixels changed.
        if let Some(field) = self.rename.as_ref() {
            field.input.collect_damage_for(self, regions);
        }
        if let Some(changed) = regions.get_mut(first..) {
            for rect in changed {
                *rect = rect.intersect(self.rect);
            }
        }
    }

    pub(super) fn placed_near(&self) -> Range<usize> {
        let near = self.mounted_near();
        let Some(placed) = &self.placed else {
            return near;
        };
        let start = near.start.max(placed.start);
        start..near.end.min(placed.end).max(start)
    }
}

impl<M: ListModel + 'static> VirtualList<M> {
    pub(super) fn place_rows(&mut self, taffy: &mut TaffyTree<()>) {
        self.rows_beside_bar = self.overlay_bar.is_none() && self.scroll_metrics().overflows();
        let left = length(px(self.lead));
        let right = length(px(self.trailing_inset()));
        let places = self
            .mounted
            .iter()
            .map(|row| {
                let top =
                    self.viewport.item_top(row.index).unwrap_or_default() - self.layout_offset;
                (row.index, top, row.node)
            })
            .collect::<Vec<_>>();
        self.place_bands(places.iter().map(|(index, _, _)| *index));
        for (index, top, node) in places {
            let Some(child) = node else {
                continue;
            };
            let top = top + self.band_room(index);
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

    pub(super) fn remount_rows(&mut self, remount: &mut Remount<'_>) -> bool {
        let Some(node) = self.layout_node else {
            return false;
        };
        let before: Vec<NodeId> = self
            .mounted
            .iter()
            .filter_map(super::MountedRow::laid_out_node)
            .chain(self.letting_go.iter().map(|(_, row)| row.node))
            .collect();
        self.reconcile_mounted();
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
        for row in &mut self.mounted {
            row.replacing = None;
        }
        self.letting_go.clear();
        carried
    }
}

impl<M: ListModel> VirtualList<M> {
    pub(super) fn pending_shape(&self) -> crate::widget::SkeletonShape {
        self.mounted
            .iter()
            .filter(|row| matches!(row.state, super::MountedRowState::Ready))
            .find_map(|row| {
                crate::widget::capability::capability::<dyn crate::widget::ReservedShape>(
                    row.widget.as_ref(),
                )
                .map(crate::widget::ReservedShape::reserved_shape)
            })
            .unwrap_or(crate::widget::SkeletonShape::Text { lines: 1 })
    }

    /// The mounted rows `event` may reach. Rows far from the box may stand
    /// where an earlier placement put them: a pointer meets only those near
    /// it, unless one holds it.
    pub(super) fn rows_reached(&self, event: &winit::event::WindowEvent) -> Range<usize> {
        match (&self.placed, crate::widget::layout::at_a_place(event)) {
            (Some(_), true)
                if !self
                    .mounted
                    .iter()
                    .any(|row| row.widget.wants_pointer_capture()) =>
            {
                self.placed_near()
            }
            _ => 0..self.mounted.len(),
        }
    }

    pub(super) fn draw_keep(
        &self,
        opacity: f32,
        now: crate::time::Instant,
    ) -> Option<crate::draw_cull::Keep> {
        self.shown
            .keep()
            .filter(|keep| keep.moving() && opacity >= 1.0 && self.row_motion.is_empty())
            .filter(|_| !self.arrivals.needs_redraw(now))
    }
}

/// Whether a row, unchanged, lies where the frame's copy keeps it: its
/// widget's paint and, selected, the lift the list paints in `lift`.
pub(super) fn row_kept(
    scene: &vello::Scene,
    transform: vello::kurbo::Affine,
    row: &dyn crate::widget::Widget,
    lift: Option<vello::kurbo::Rect>,
) -> bool {
    let paints = lift.map_or_else(|| row.paint_bounds(), |lift| lift.union(row.paint_bounds()));
    !row.needs_redraw()
        && !row.needs_animation_frame()
        && crate::draw_cull::kept(scene, transform, paints)
}
