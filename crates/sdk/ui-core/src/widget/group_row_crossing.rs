//! A window crossing to another of the person's devices by its bar: the bar's resource goes on the
//! drag bus, so the pointer carries the window there as the strip rearranges here, and a window
//! another of the person's devices carried here lands on the window it is let go over.

use super::{GroupDrag, GroupRow};
use crate::widget::Widget as _;

/// The key this row takes a dropped window under, on the drag bus: a window's bar drag is known by
/// the window it lifted, never by the row.
const DROP: &str = "strip";

/// What letting go of a window's bar came to.
pub(super) enum LetGo {
    /// The drag is this screen's to finish: nothing took it, and the strip rearranges it.
    Here,
    /// The flight left with the pointer for another of the person's devices: nothing here moves.
    CarriedAway,
    /// Let go over another window of this strip after a press on a tab or a lone window's title:
    /// its place joins that window as a tab, and the strip does not also rearrange the window.
    Joined,
}

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
    ///
    /// Let go over another window of this strip, a window carried from a tab or a lone window's
    /// title lands as a place of that window: the same drop another of the person's devices'
    /// windows lands by ([`Self::take_window_drop`]), so a window tabbed in by the pointer and one
    /// carried between devices join the same way. A window carried by its bar's empty area
    /// rearranges the strip instead.
    pub(super) fn let_the_window_go(&mut self, id: u64, land: bool) -> LetGo {
        if !self.carried.published {
            return LetGo::Here;
        }
        self.carried.published = false;
        if crate::widget::drag_bus::current().is_none() {
            crate::widget::drag_bus::end(None);
            self.put_back_drag(id);
            return LetGo::CarriedAway;
        }
        // A window stands where it is let go: on the canvas a place keeps its point, and a float
        // keeps its own drag, so only a window among the strip's own joins one beside it, and only
        // one carried from a tab or a lone window's title: the empty bar rearranges.
        let onto = land
            && self.canvas.is_none()
            && self.float_drag_from.is_none()
            && self
                .groups
                .iter()
                .any(|group| group.id != id && group.get_rect().contains(self.last_cursor));
        let carries = self
            .groups
            .iter()
            .find(|group| group.id == id)
            .and_then(|group| group.carries())
            .map(str::to_owned);
        if let Some(carries) = carries.filter(|_| onto) {
            crate::widget::drag_bus::end(Some(crate::widget::drag_bus::Landed {
                source: format!("window:{id}"),
                payload: crate::dnd::DragPayload::Custom {
                    kind: crate::widget::drag_source::RESOURCE_PAYLOAD.to_owned(),
                    blob: carries.into_bytes(),
                },
                pointer: self.last_cursor,
            }));
            return LetGo::Joined;
        }
        crate::widget::drag_bus::end(None);
        LetGo::Here
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
