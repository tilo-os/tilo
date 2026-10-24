//! Motion that survives a rebuild: a widget built again from fresh data
//! (a view that makes its banner or its table anew on every change) picks
//! up where the widget it replaces was, on its way or at rest.
//!
//! A widget that moves leaves its motion state when it is dropped
//! ([`keep`]), under its identity (its id) and where it was laid out. The
//! widget a rebuild puts in its place takes it up at its first layout
//! ([`take_kept`] by identity, [`take_kept_at`] by place) instead of
//! starting over: a banner that was already shown stays shown at rest, one
//! still opening goes on opening, a sorted table's rows go on moving. What
//! nothing takes up by the end of the next frame is let go, so the store
//! only ever holds what one rebuild dropped.
//!
//! A widget that finds nothing arrived: it moves in, unless its window is
//! laying out its first frame ([`window_has_shown`]), which shows at rest.

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};

use vello::kurbo::Rect;

/// What a dropped widget left.
struct Kept {
    kind: TypeId,
    id: String,
    /// Where it was laid out or drawn.
    at: Option<Rect>,
    value: Box<dyn Any>,
    /// A frame ended since it was left: the next frame's end lets it go.
    stale: bool,
}

thread_local! {
    static KEPT: RefCell<Vec<Kept>> = const { RefCell::new(Vec::new()) };
    /// Whether the window being laid out has shown a frame before.
    static WINDOW_SHOWN: Cell<bool> = const { Cell::new(false) };
}

/// Leave `value`, the motion state of a widget being dropped, for the
/// widget a rebuild puts in its place: the one with the same `id`, or else
/// one laid out where it was (`at`).
pub fn keep<T: 'static>(id: impl Into<String>, at: Option<Rect>, value: T) {
    let id = id.into();
    let kind = TypeId::of::<T>();
    // A widget dropped as its thread ends has no rebuild to wait for.
    let _ = KEPT.try_with(|kept| {
        let Ok(mut kept) = kept.try_borrow_mut() else {
            return;
        };
        kept.retain(|entry| !(entry.kind == kind && entry.id == id));
        kept.push(Kept {
            kind,
            id,
            at,
            value: Box::new(value),
            stale: false,
        });
    });
}

/// What a widget of the same kind with this `id` left when a rebuild
/// dropped it, if anything.
#[must_use]
pub fn take_kept<T: 'static>(id: &str) -> Option<T> {
    take_where::<T>(|entry| entry.id == id)
}

/// What a widget of the same kind left when a rebuild dropped it from
/// where this one is laid out (`at`: the same top left corner and width),
/// if anything.
#[must_use]
pub fn take_kept_at<T: 'static>(at: Rect) -> Option<T> {
    let near = |a: f64, b: f64| (a - b).abs() < 0.5;
    take_where::<T>(|entry| {
        entry.at.is_some_and(|was| {
            near(was.x0, at.x0) && near(was.y0, at.y0) && near(was.width(), at.width())
        })
    })
}

fn take_where<T: 'static>(matches: impl Fn(&Kept) -> bool) -> Option<T> {
    let kind = TypeId::of::<T>();
    let entry = KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        let index = kept
            .iter()
            .position(|entry| entry.kind == kind && matches(entry))?;
        Some(kept.swap_remove(index))
    })?;
    entry.value.downcast::<T>().ok().map(|value| *value)
}

/// A frame's layout and drawing ended: what was left before the frame
/// that just ended and nothing took up is let go.
pub(crate) fn frame_ended() {
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        if kept.is_empty() {
            return;
        }
        kept.retain(|entry| !entry.stale);
        for entry in kept.iter_mut() {
            entry.stale = true;
        }
    });
}

/// Whether the window being laid out has shown a frame before: a widget
/// in its first frame shows at rest (first paint shows the resting state;
/// motion explains changes).
#[must_use]
pub fn window_has_shown() -> bool {
    WINDOW_SHOWN.with(Cell::get)
}

/// While the guard lives, the window being laid out has (`shown`) or has
/// not shown a frame before ([`window_has_shown`]).
pub(crate) fn laying_out_window(shown: bool) -> LayingOutWindow {
    LayingOutWindow(WINDOW_SHOWN.with(|cell| cell.replace(shown)))
}

/// Restores what [`laying_out_window`] replaced.
pub(crate) struct LayingOutWindow(bool);

impl Drop for LayingOutWindow {
    fn drop(&mut self) {
        WINDOW_SHOWN.with(|cell| cell.set(self.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rebuilt_widget_takes_up_what_its_predecessor_left_by_id_or_by_place() {
        keep("sync", Some(Rect::new(0.0, 10.0, 100.0, 40.0)), 7_u32);
        assert_eq!(take_kept::<u32>("other"), None);
        assert_eq!(take_kept::<u8>("sync"), None, "only a widget of its kind");
        assert_eq!(take_kept::<u32>("sync"), Some(7));
        assert_eq!(take_kept::<u32>("sync"), None, "taken once");

        keep("old title", Some(Rect::new(0.0, 10.0, 100.0, 40.0)), 8_u32);
        assert_eq!(take_kept_at::<u32>(Rect::new(0.0, 50.0, 100.0, 90.0)), None);
        assert_eq!(
            take_kept_at::<u32>(Rect::new(0.0, 10.0, 100.0, 70.0)),
            Some(8),
            "a widget in the same place, whatever it now says"
        );
    }

    #[test]
    fn what_nothing_takes_up_is_let_go_after_the_next_frame() {
        keep("sync", None, 1_u32);
        frame_ended();
        assert_eq!(
            take_kept::<u32>("sync"),
            Some(1),
            "a rebuild in the next frame"
        );
        keep("sync", None, 1_u32);
        frame_ended();
        frame_ended();
        assert_eq!(take_kept::<u32>("sync"), None);
        KEPT.with(|kept| assert!(kept.borrow().is_empty()));
    }
}
