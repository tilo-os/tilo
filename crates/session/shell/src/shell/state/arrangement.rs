//! The active workspace's arrangement: whether it shows its canvas or its
//! strips, and where each window stands on the canvas.

use super::WorkspaceTree;

impl WorkspaceTree {
    /// Commit an arrangement change of the active workspace (showing its
    /// canvas or its strips, standing a window somewhere on the canvas) as
    /// this device's topology owner: the shared reduction on the cache,
    /// mirrored into the desktop document so it is history and undoable,
    /// and into the restart snapshot. The room's layout record carries it
    /// to peers on its next capture. `Ok(false)` when nothing changed.
    ///
    /// # Errors
    ///
    /// The model's refusal: no active workspace, a window that is not in
    /// it, a place with no size; or a command that is not an arrangement.
    pub fn commit_arrangement_command(
        &mut self,
        command: &tilo_desktop_model::commands::DesktopCommand,
    ) -> Result<bool, String> {
        use tilo_desktop_model::commands::{DesktopCommand, apply_command};
        if !matches!(
            command,
            DesktopCommand::SetArrangement { .. } | DesktopCommand::PlaceGroup { .. }
        ) {
            return Err("not an arrangement command".into());
        }
        let active = self.desktop.active;
        let workspace = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == active)
            .ok_or("there is no active workspace")?;
        let before = workspace.arrangement.clone();
        apply_command(
            workspace,
            command,
            &mut || Err("an arrangement mints no identity".into()),
            &mut |_, _| Err("an arrangement creates no record".into()),
        )?;
        if workspace.arrangement == before {
            return Ok(false);
        }
        self.seed_doc_from_cache();
        self.queue_save();
        Ok(true)
    }
}
