//! The controls of a window's own content, as the app that draws them
//! describes them to the desktop: what a session finds and presses by name
//! ([`crate::floating_surface::FloatingSurfaceClientMessage::WindowTargets`]),
//! and what the desktop presses on the app
//! ([`crate::floating_surface::FloatingSurfaceHostMessage::PressTarget`]),
//! which answers through the same path an assistive technology's activation
//! takes.
//!
//! A window drawn by another process publishes no surface of the desktop's
//! own: the controls inside it are the app's, and nothing outside the app
//! could name them without this. Everything is in the window's logical
//! coordinates, rectangles as `[x0, y0, x1, y1]`.

use serde::{Deserialize, Serialize};

/// Most controls one window describes. A window with more is described by
/// its first ones in reading order; the session lists and presses those.
pub const MAX_TARGETS: usize = 256;

/// What the desktop may do to a control of a window's content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetAction {
    /// Press it: its `activate` verb, or a click at its centre when the app
    /// gives it none.
    Activate,
    /// Open its context menu.
    ShowContextMenu,
}

impl TargetAction {
    /// The accessibility action an assistive technology sends to act this
    /// way on a control, which is the one path every action here takes.
    #[must_use]
    pub fn accesskit(self) -> crate::accesskit::Action {
        match self {
            Self::Activate => crate::accesskit::Action::Click,
            Self::ShowContextMenu => crate::accesskit::Action::ShowContextMenu,
        }
    }
}

/// One control of a window's content that a caller can name and press.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowTarget {
    /// Its role as assistive technology names it (`Button`, `ListItem`).
    pub role: String,
    /// Its name, already in the person's language: what a caller finds it by.
    pub name: String,
    /// Where it is drawn in the window's content, in logical pixels.
    pub rect: [f64; 4],
    /// What the app answers on it, in the order the app offers them.
    pub actions: Vec<TargetAction>,
}

impl WindowTarget {
    /// Whether it can be pressed at all: a control that answers nothing is
    /// not one a caller can act on.
    #[must_use]
    pub fn pressable(&self) -> bool {
        !self.actions.is_empty()
    }
}
