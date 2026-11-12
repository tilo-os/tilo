//! Where the activity model's code is.
//!
//! A host that carries the model's code answers [`ActivityPaneCode::Ready`] with
//! [`open_activity`](tilo_desktop_surface::open_activity). The page fetches it as its own module the
//! first time someone opens Activity, so the start module does not carry it; it answers
//! [`ActivityPaneCode::Loading`] until the fetch lands and asks the surface for the frame that
//! opens it then.

use tilo_desktop_surface::ActivityPaneCode;

/// Where this host has the activity model's code.
pub trait Pane {
    /// Asked each time the model is to open.
    fn pane_code(&self) -> ActivityPaneCode;
}

/// A host that carries the model's code from the start.
pub struct Linked;

impl Pane for Linked {
    fn pane_code(&self) -> ActivityPaneCode {
        ActivityPaneCode::Ready(tilo_desktop_surface::open_activity)
    }
}
