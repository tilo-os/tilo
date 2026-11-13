//! The controls of a window's own content, as the app that draws them describes them to
//! the desktop: what a session finds and presses by name
//! ([`crate::floating_surface::FloatingSurfaceClientMessage::WindowTargets`]), and what
//! the desktop presses on the app
//! ([`crate::floating_surface::FloatingSurfaceHostMessage::PressTarget`]), which answers
//! through the same path an assistive technology's activation takes. A window drawn by
//! another process publishes no surface of the desktop's own: the controls inside it are
//! the app's, and nothing outside the app could name them without this.

use serde::{Deserialize, Serialize};

/// Most controls one window describes.
pub const MAX_TARGETS: usize = 256;

/// What the desktop may do to a control of a window's content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetAction {
    Activate,
    ShowContextMenu,
}

impl TargetAction {
    /// The accessibility action an assistive technology sends to act this way on a
    /// control, which is the one path every action here takes.
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
    pub role: String,
    /// Its name, already in the person's language: what a caller finds it by.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Where it is drawn in the window's content, in logical pixels.
    pub rect: [f64; 4],
    pub actions: Vec<TargetAction>,
}

impl WindowTarget {
    /// Whether it can be pressed at all: a control that answers nothing is not one a
    /// caller can act on.
    #[must_use]
    pub fn pressable(&self) -> bool {
        !self.actions.is_empty()
    }
}

/// One control of a window's content, as the window itself lists it: its name in the
/// person's language, what it does, and where it stands in the window's own coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowControl {
    pub name: String,
    pub identity: Option<String>,
    pub rect: (f64, f64, f64, f64),
}
