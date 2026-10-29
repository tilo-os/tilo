//! The active workspace's arrangement, as the chrome changes it: showing its
//! canvas or its strips, and where a window stands on the canvas
//! ([`tilo_desktop_model::arrangement`]).

use super::ChromeState;

impl ChromeState {
    /// Show the active workspace's windows on its canvas (`canvas`) or in its
    /// strips.
    ///
    /// # Errors
    ///
    /// The model's refusal: there is no active workspace.
    pub fn set_arrangement(&mut self, canvas: bool) -> Result<(), String> {
        let command = tilo_desktop_model::commands::DesktopCommand::SetArrangement { canvas };
        if self.tree_mut().commit_arrangement_command(&command)? {
            self.bump();
        }
        Ok(())
    }

    /// Stand window group `group` of the active workspace at `place` on its
    /// canvas.
    ///
    /// # Errors
    ///
    /// The model's refusal: the window is not in the active workspace, or
    /// the place has no size.
    pub fn place_on_canvas(
        &mut self,
        group: crate::shell::state::GroupId,
        place: tilo_desktop_model::CanvasPlace,
    ) -> Result<(), String> {
        let command = tilo_desktop_model::commands::DesktopCommand::PlaceGroup { group, place };
        if self.tree_mut().commit_arrangement_command(&command)? {
            self.bump();
        }
        Ok(())
    }
}
