//! What a window's own content shows, as the app that draws it describes it to the shell: what a
//! session finds by name and presses there
//! ([`crate::floating_surface::FloatingSurfaceClientMessage::WindowTargets`]), and what
//! the shell presses on the app
//! ([`crate::floating_surface::FloatingSurfaceHostMessage::PressTarget`]), which answers
//! through the same path an assistive technology's activation takes. A window drawn by
//! another process publishes no surface of the shell's own: the things inside it are
//! the app's, and nothing outside the app could name them without this.

use serde::{Deserialize, Serialize};

/// Most things one window describes.
pub const MAX_TARGETS: usize = 256;

/// What the shell may do to a control of a window's content.
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

/// One thing a window's content shows: a control a caller names and presses, or a thing it names
/// and reads (a map, the sentence under it), which answers no press.
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
    /// How deep it stands in the tree it is a row of, as a reader hears it: 1 at the top level.
    /// Absent for a control that is no row of one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<usize>,
    /// Whether it shows itself the chosen one now: a segment that holds the look, a swatch the
    /// accent, a switch that is on. What a caller tells a control already holding what it would
    /// press by.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub selected: bool,
}

impl WindowTarget {
    /// Whether it can be pressed at all: a thing a window shows that answers no press — a map, a
    /// sentence a person reads — is not one a caller can act on, only name.
    #[must_use]
    pub fn pressable(&self) -> bool {
        !self.actions.is_empty()
    }
}

/// One thing a window's content shows, as the window itself lists it: its name in the
/// person's language, what it does, and where it stands in the window's own coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowControl {
    pub name: String,
    pub identity: Option<String>,
    pub rect: (f64, f64, f64, f64),
    /// How deep it stands in the tree it is a row of ([`WindowTarget::level`]).
    pub level: Option<usize>,
    /// Whether it shows itself the chosen one now ([`WindowTarget::selected`]).
    pub selected: bool,
}
