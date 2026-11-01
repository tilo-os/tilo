//! What is being dragged, across widgets in one window.
//!
//! A [`DragRun`](super::DragRun) reorders its own items by itself. A drag
//! that leaves the run — a dock item dragged onto a bar's strip, a search
//! result dragged onto a dock — needs every other run in the window to know
//! it is there: which one it came from, its label, where the pointer is, and
//! where it was let go.
//!
//! This is that one place, per thread (a window's widgets all run on its own
//! thread). A run publishes when an item lifts past the drag threshold and
//! clears when it is let go; every run reads it while it is up; the release
//! records the drop, and the run whose extent holds the pointer takes it in
//! its next update, as one action of the owner's.
//!
//! It holds no model: the payload is opaque bytes the owner put there, and
//! the drop names the run it landed in so the owner can route it.

use std::cell::RefCell;

use vello::kurbo::{Point, Rect};

/// What a drag in flight is.
#[derive(Clone, Debug)]
pub struct InFlight {
    /// The run the drag came from.
    pub source: String,
    /// What is dragged, as its owner encoded it.
    pub payload: crate::dnd::DragPayload,
    /// What the ghost shows.
    pub label: String,
    /// Where the pointer is now.
    pub pointer: Point,
    /// The extent of every run in this window, by its key, as each last laid
    /// out: where a drop can land, and what it lands before.
    pub runs: Vec<(String, Rect)>,
}

/// A drag that was let go outside its own run, waiting for the run it landed
/// in to take it.
#[derive(Clone, Debug)]
pub struct Landed {
    pub source: String,
    pub payload: crate::dnd::DragPayload,
    pub pointer: Point,
}

thread_local! {
    static IN_FLIGHT: RefCell<Option<InFlight>> = const { RefCell::new(None) };
    static LANDED: RefCell<Option<Landed>> = const { RefCell::new(None) };
}

/// `run` lifted an item: everything else in the window may take it now.
pub fn publish(in_flight: InFlight) {
    IN_FLIGHT.with(|slot| *slot.borrow_mut() = Some(in_flight));
}

/// The pointer moved while something is in flight: what every run needs to
/// show where it would land.
pub fn moved(pointer: Point, run: (String, Rect)) {
    IN_FLIGHT.with(|slot| {
        if let Some(in_flight) = slot.borrow_mut().as_mut() {
            in_flight.pointer = pointer;
            match in_flight.runs.iter_mut().find(|(key, _)| *key == run.0) {
                Some(entry) => entry.1 = run.1,
                None => in_flight.runs.push(run),
            }
        }
    });
}

/// What is in flight now, if anything.
#[must_use]
pub fn current() -> Option<InFlight> {
    IN_FLIGHT.with(|slot| slot.borrow().clone())
}

/// The drag ended: `None` for a cancel, or where it was let go for a drop
/// another run takes.
pub fn end(landed: Option<Landed>) {
    IN_FLIGHT.with(|slot| *slot.borrow_mut() = None);
    LANDED.with(|slot| *slot.borrow_mut() = landed);
}

/// Take the drop `run` takes, if the last drag ended inside its `extent` and
/// came from somewhere else. One run takes it; the rest see nothing.
pub fn take(run: &str, extent: Rect) -> Option<Landed> {
    LANDED.with(|slot| {
        let mut slot = slot.borrow_mut();
        let landed = slot.as_ref()?;
        if landed.source == run || !extent.contains(landed.pointer) {
            return None;
        }
        slot.take()
    })
}
