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
    /// What it does, as the identity of the app action it runs (or of the
    /// field it edits): the record kind and the operation, in ids, which no
    /// composition and no language changes. `None` for a control that runs no
    /// app action — a window's own chrome, a view's own event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
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

/// One control of a window's content, as the window itself lists it: its
/// name in the person's language, what it does, and where it stands in the
/// window's own coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowControl {
    /// Its name, as its accessibility tree says it, in the person's language.
    pub name: String,
    /// What it does ([`WindowTarget::identity`]), when it runs an app action.
    pub identity: Option<String>,
    /// Where it is drawn: its left and top edge, its width and its height.
    pub rect: (f64, f64, f64, f64),
}
