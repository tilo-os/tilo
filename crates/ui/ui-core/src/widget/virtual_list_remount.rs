//! The rows of a [`super::VirtualList`] laid out where they stand, and the rows a scroll brings
//! near mounted in place: each entering row's layout is built alone and the rows already there keep
//! theirs, so a row coming into view costs its own layout, not the list's
//! ([`tilo_ui_base::widget::Widget::remount_children`]).
use std::ops::Range;

use taffy::style_helpers::length;
use taffy::{Dimension, NodeId};

use super::px;
use super::{ListKey, VirtualRows};
use tilo_ui_base::widget::Remount;
use tilo_ui_base::widget::layout::Styles;

/// How far the view may scroll from where the rows' layout was placed
/// before they are placed again from where it stands: taffy lays out in
/// `f32`, which stays well under a hundredth of a pixel this near.
const REPLACE_AFTER_PX: f64 = 16_384.0;

impl VirtualRows {
    pub(super) fn mount_window(&self) -> Range<usize> {
        let near = self.rows_to_hold();
        let mounted = self.mounted_range.clone();
        if self.mounted_covers(&near) {
            let count = self.model.len();
            return mounted.start.min(count)..mounted.end.min(count);
        }
        if self.fit_rows || near.end <= mounted.start || near.start >= mounted.end {
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

    /// Whether the rows mounted hold every row `near` asks for. A list out of view (a list as tall
    /// as its rows below the fold) asks for none, and the rows it holds stay until a scroll brings
    /// it near: unmounting them would lay the window out again for nothing it shows. A model that
    /// shrank lets its gone rows go all the same ([`Self::mount_window`] clamps to its length).
    pub(super) fn mounted_covers(&self, near: &Range<usize>) -> bool {
        near.is_empty()
            || (self.mounted_range.start <= near.start && near.end <= self.mounted_range.end)
    }

    pub(super) fn mounted_near(&self) -> Range<usize> {
        let reach = tilo_ui_base::scroll_copy::EFFECT_REACH + self.give().abs();
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
            let before = regions.len();
            row.widget.collect_damage(regions);
            if regions.len() > before {
                self.collect_row_frame_damage(row, regions);
            }
        }
        // The field renaming a row in place draws over that row as part of the
        // list, and says where its own pixels changed.
        if let Some(field) = self.rename.as_ref() {
            field.input.collect_damage_for(self, regions);
        }
        if let Some(changed) = regions.get_mut(first..) {
            for rect in changed {
                *rect = rect.intersect(self.drawn());
            }
        }
    }

    /// What the list paints around a row it repaints, past the row's own box: the selection lift
    /// stands a bleed outside what the row shows, and the ring of the row holding the keyboard
    /// stands outside the row, both drawn by the list and neither covered by the row's damage.
    fn collect_row_frame_damage(
        &self,
        row: &super::MountedRow<ListKey>,
        regions: &mut Vec<vello::kurbo::Rect>,
    ) {
        let Some(row_rect) = self.row_box(row.index) else {
            return;
        };
        let mut frame = row_rect;
        if row.selection.selected && !self.delegate_selection {
            let content = crate::widget::row_content_box(row.widget.as_ref()).unwrap_or(row_rect);
            frame = frame.union(super::lift::selection_around(row_rect, content, self.lift));
        }
        if self.is_focused && self.focused_key.as_ref() == Some(&row.key) {
            frame = frame.union(row_rect.inset(-2.0));
        }
        let shown = row.widget.get_rect();
        for strip in [
            vello::kurbo::Rect::new(frame.x0, frame.y0, shown.x0, frame.y1),
            vello::kurbo::Rect::new(shown.x1, frame.y0, frame.x1, frame.y1),
            vello::kurbo::Rect::new(shown.x0, frame.y0, shown.x1, shown.y0),
            vello::kurbo::Rect::new(shown.x0, shown.y1, shown.x1, frame.y1),
        ] {
            if strip.width() > 0.0 && strip.height() > 0.0 {
                regions.push(strip.intersect(frame));
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

impl VirtualRows {
    pub(super) fn place_rows(&mut self, taffy: &mut impl Styles) {
        self.rows_beside_bar = self.keeps_bar_gutter();
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
            // The list's own offset is snapped where it places them; a row's top within it
            // stands on a device pixel too, whatever offset the rows were laid out from.
            let top = tilo_ui_base::widget::layout::snap_to_device_pixels(top + self.band_gap(index));
            let Some(current) = taffy.style_of(child) else {
                continue;
            };
            let mut style = current.clone();
            style.position = taffy::Position::Absolute;
            style.inset.left = left;
            style.inset.right = right;
            style.inset.top = length(px(top));
            style.size.width = Dimension::auto();
            if style != *current {
                let _ = taffy.set_style_of(child, style);
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
        // The node saying how tall the rows come to stays the list's, as tall as they are now.
        if let Some(natural) = self.natural {
            let style = self.natural_style();
            let _ = remount.taffy().set_style(natural, style);
            children.push(natural);
        }
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

impl VirtualRows {
    pub(super) fn pending_shape(&self) -> tilo_ui_base::widget::SkeletonShape {
        self.mounted
            .iter()
            .filter(|row| matches!(row.state, super::MountedRowState::Ready))
            .find_map(|row| {
                tilo_ui_base::widget::capability::capability::<dyn crate::widget::ReservedShape>(
                    row.widget.as_ref(),
                )
                .map(crate::widget::ReservedShape::reserved_shape)
            })
            .unwrap_or(tilo_ui_base::widget::SkeletonShape::Text { lines: 1 })
    }

    /// The mounted rows `event` may reach. Rows far from the box may stand
    /// where an earlier placement put them: a pointer meets only those near
    /// it, unless one holds it.
    pub(super) fn rows_reached(&self, event: &winit::event::WindowEvent) -> Range<usize> {
        match (&self.placed, tilo_ui_base::widget::layout::at_a_place(event)) {
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

    /// Whether `event`'s pointer stands over the list at all: clipping is an interaction boundary
    /// as well as a paint boundary, so a row scrolled past the viewport takes nothing from above
    /// or below it, where it is painted nowhere. A row holding a drag it began inside the list
    /// keeps the pointer wherever it goes.
    pub(super) fn pointer_reaches_rows(
        &self,
        event: &winit::event::WindowEvent,
        ctx: &tilo_ui_base::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::WindowEvent;
        let at = match event {
            WindowEvent::CursorMoved { position, .. } => Some(ctx.logical(position)),
            WindowEvent::Touch(touch) => Some(ctx.logical(touch.location)),
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } => self.last_cursor,
            _ => return true,
        };
        let Some(at) = at else { return true };
        self.rect.contains(at)
            || self
                .mounted
                .iter()
                .any(|row| row.widget.wants_pointer_capture())
    }

    pub(super) fn draw_keep(
        &self,
        transform: vello::kurbo::Affine,
        opacity: f32,
        now: crate::time::Instant,
    ) -> Option<tilo_ui_base::draw_cull::Keep> {
        let keep = self
            .shown
            .keep()
            .filter(|keep| keep.moving() && opacity >= 1.0 && self.row_motion.is_empty())
            .filter(|_| !self.arrivals.needs_redraw(now))?;
        // A bar beside the rows stands in its own gutter, painted over the list's ground:
        // rows show nothing there, so the pixels a copy keeps there are none of theirs.
        let content = vello::kurbo::Rect::new(
            self.rect.x0,
            self.rect.y0,
            if self.rows_beside_bar {
                self.rect.x1 - self.gutter
            } else {
                self.rect.x1
            },
            self.rect.y1,
        );
        keep.visible_within(transform.transform_rect_bbox(content))
    }
}

/// Whether a row, unchanged, lies where the frame's copy keeps it: its
/// widget's paint and, selected, the lift the list paints in `lift`.
pub(super) fn row_kept(
    scene: &vello::Scene,
    transform: vello::kurbo::Affine,
    row: &dyn tilo_ui_base::widget::Widget,
    lift: Option<vello::kurbo::Rect>,
) -> bool {
    let paints = lift.map_or_else(|| row.paint_bounds(), |lift| lift.union(row.paint_bounds()));
    !row.needs_redraw()
        && !row.needs_animation_frame()
        && tilo_ui_base::draw_cull::kept(scene, transform, paints)
}
