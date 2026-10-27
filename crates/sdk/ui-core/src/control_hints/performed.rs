//! What the person just did with the desktop's own controls, as the
//! widgets that carried it out report it.
//!
//! The control that performs a gesture says so once, where it commits
//! (a window's bar let go at its new place, an edge let go at its new
//! width, the canvas let go after a drag): every host then reads the same
//! account, whichever of them drew the control. Only what the person did
//! by hand is noted, never a sync, a script or a shortcut's result.

use std::cell::RefCell;

/// A control the person used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Performed {
    /// Opened search.
    OpenedSearch,
    /// Opened a window (an app) from search or a launcher.
    OpenedWindow,
    /// Carried a window to a new place by its bar.
    MovedWindow,
    /// Rested the pointer on a window's bar, where it can be taken.
    HoveredWindowBar,
    /// Resized a window by its edge.
    ResizedWindow,
    /// Moved the view across the canvas by hand.
    PannedView,
}

thread_local! {
    static PERFORMED: RefCell<Vec<Performed>> = const { RefCell::new(Vec::new()) };
}

/// The person just did `what`.
pub fn note(what: Performed) {
    PERFORMED.with(|log| {
        let mut log = log.borrow_mut();
        // A host that reads rarely still hears each kind once.
        if !log.contains(&what) {
            log.push(what);
        }
    });
}

/// What the person did since the last call, oldest first.
#[must_use]
pub fn take() -> Vec<Performed> {
    PERFORMED.with(|log| std::mem::take(&mut *log.borrow_mut()))
}
