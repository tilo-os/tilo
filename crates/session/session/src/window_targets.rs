//! What the windows in processes of their own offer by name, and the press
//! that goes back to the app drawing the window ([`ui::window_targets`]).
//!
//! A window another process draws keeps its widget tree to itself, so the
//! session reads the controls of its content from the descriptions the app
//! sends over its `shell-ui` connection, beside the desk's own chrome, and a
//! press reaches the app as an assistive technology's activation does.

use ui::window_targets::TargetAction;

use crate::Session;

impl Session {
    /// Every control the session shows by name, each with where it stands on
    /// the desk: the desk's own chrome, and the controls the apps that draw
    /// the windows describe of their content
    /// ([`ui::window_targets`]).
    #[must_use]
    pub fn controls(&self) -> Vec<(String, (f64, f64, f64, f64))> {
        self.shared.doors.controls()
    }

    /// Act on the control `name` as `action` says, as an assistive technology
    /// does on the surface showing it.
    ///
    /// # Errors
    /// No surface shows a control by that name, or it answers nothing of
    /// `action`.
    pub fn press_action(&self, name: &str, action: TargetAction) -> Result<(), String> {
        self.shared.doors.press_action(name, action)
    }
}
