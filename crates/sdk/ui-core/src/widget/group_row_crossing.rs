//! A window crossing to another of the person's devices by its bar: the bar's resource goes on the
//! drag bus, so the pointer carries the window there as the strip rearranges here, and a window
//! another of the person's devices carried here lands on the window it is let go over.

use super::{GroupDrag, GroupRow};
use crate::widget::Widget as _;

/// The key this row takes a dropped window under, on the drag bus: a window's bar drag is known by
/// the window it lifted, never by the row.
const DROP: &str = "strip";

/// What a bar drag carries, and what it took from the bus.
#[derive(Clone, Debug, Default)]
pub(super) struct Carried {
    /// The window's resource went on the drag bus this drag.
    published: bool,
    /// A window another of the person's devices carried here and let go over window `into`, and
    /// the resource it shows: the shell moves the place showing it there.
    landed: Option<(u64, String)>,
}

impl GroupRow {
    /// Put the window the drag carries on the drag bus, or move the flight along it: the one drag
    /// model, so the strip rearranges under the pointer here and the window crosses to the device
    /// beside this one when the pointer leaves the screen. A window whose bar carries nothing (a
    /// foreign window's) stays a strip drag alone.
    pub(super) fn carry_window(&mut self, drag: &GroupDrag) {
        let Some(group) = self.groups.iter().find(|group| group.id == drag.group_id) else {
            return;
        };
        let Some(carries) = group.carries().map(str::to_owned) else {
            return;
        };
        let key = format!("window:{}", drag.group_id);
        let rect = group.get_rect();
        let name = group.name();
        if self.carried.published {
            crate::widget::drag_bus::moved(self.last_cursor, (key, rect));
        } else {
            crate::widget::drag_bus::carry(key, carries.into_bytes(), name, self.last_cursor, rect);
            self.carried.published = true;
        }
    }

    /// Take the window the drag carried off the bus: whether the drag is still this screen's to
    /// finish. A flight taken off it already left with the pointer for another of the person's
    /// devices, so nothing here is rearranged and no window floats.
    pub(super) fn let_the_window_go(&mut self, id: u64) -> bool {
        if !self.carried.published {
            return true;
        }
        let here = crate::widget::drag_bus::current().is_some();
        crate::widget::drag_bus::end(None);
        self.carried.published = false;
        if !here {
            self.put_back_drag(id);
        }
        here
    }

    /// A window carried here by another of the person's devices was let go: the window of this row
    /// under the pointer takes it, as the window whose bar it was let go over takes a window dragged
    /// on one device. The row hands it to its shell, which moves the place showing that resource.
    pub(super) fn take_window_drop(&mut self) {
        if self.group_drag.is_some() {
            return;
        }
        let Some(landed) = crate::widget::drag_bus::take(DROP, self.rect) else {
            return;
        };
        let crate::dnd::DragPayload::Custom { kind, blob } = landed.payload else {
            return;
        };
        if kind != crate::widget::drag_source::RESOURCE_PAYLOAD {
            return;
        }
        let Ok(resource) = String::from_utf8(blob) else {
            return;
        };
        let Some(group) = self
            .groups
            .iter()
            .rev()
            .find(|group| group.get_rect().contains(landed.pointer))
        else {
            return;
        };
        self.carried.landed = Some((group.id, resource));
    }

    /// The window another of the person's devices carried here and let go over one of this row's
    /// windows, when a pass took it: that window, and the resource the carried window shows.
    pub fn take_pending_window_drop(&mut self) -> Option<(u64, String)> {
        self.carried.landed.take()
    }

    /// Put the window `id` back where the drag took it from: its canvas place or its floating one,
    /// which followed the pointer for a window carried off this screen.
    fn put_back_drag(&mut self, id: u64) {
        if self.canvas.is_some() {
            self.put_down_on_canvas(id, false);
        }
        if self.float_drag_from.is_some() {
            self.put_down_float(id, false);
        }
        self.group_reorder_target = None;
        self.group_reorder_section = None;
    }
}
