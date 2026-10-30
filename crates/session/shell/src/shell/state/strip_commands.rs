//! Strips made, named, moved and removed in a workspace, as the desktop
//! model reduces them.

use super::{StripId, WorkspaceId, WorkspaceTree, mint_under};

impl WorkspaceTree {
    /// Create, rename, move or remove a strip of the active workspace, as a person at
    /// this machine does it: the model's own
    /// [`DesktopCommand::CreateStrip`](tilo_desktop_model::commands::DesktopCommand::CreateStrip),
    /// `RenameStrip` and `MoveStrip` reductions on the cache, then the room document and
    /// the restart snapshot. A created strip takes focus. Returns the strip
    /// the command named or made.
    ///
    /// # Errors
    ///
    /// Returns a description when there is no active workspace, the name is
    /// empty, too long or holds control characters, the strip is gone, or it
    /// belongs to another topology owner.
    pub fn commit_strip_command(
        &mut self,
        command: &tilo_desktop_model::commands::DesktopCommand,
    ) -> Result<StripId, String> {
        let active = self
            .desktop
            .active_workspace()
            .ok_or("there is no active workspace")?
            .id;
        self.commit_strip_command_in(active, command)
    }

    /// [`Self::commit_strip_command`] in the workspace `workspace`, shown
    /// or not. A created strip becomes that workspace's shown strip.
    ///
    /// # Errors
    ///
    /// As [`Self::commit_strip_command`], or the workspace is gone.
    pub fn commit_strip_command_in(
        &mut self,
        workspace: WorkspaceId,
        command: &tilo_desktop_model::commands::DesktopCommand,
    ) -> Result<StripId, String> {
        use tilo_desktop_model::commands::{DesktopCommand, apply_to_workspace};
        let renamed = match command {
            DesktopCommand::CreateStrip { .. } => None,
            DesktopCommand::RenameStrip { strip, .. }
            | DesktopCommand::MoveStrip { strip, .. }
            | DesktopCommand::RemoveStrip { strip } => {
                let strip = StripId(*strip);
                if !self.may_rename_strip(strip) {
                    return Err("target belongs to another topology owner".into());
                }
                Some(strip)
            }
            _ => return Err("not a strip command".into()),
        };
        let salt = self.local_salt();
        let made = apply_to_workspace(&mut self.desktop, workspace, command, &mut || {
            Ok(mint_under(salt))
        })?;
        if let DesktopCommand::RemoveStrip { strip } = command {
            self.strips_to_retain.remove(&StripId(*strip));
        }
        let strip = if let Some(strip) = renamed {
            strip
        } else {
            let made = made.ok_or("the strip was not created")?;
            // Made to put windows in: the others see it now, empty.
            self.strips_to_retain.insert(made);
            made
        };
        self.seed_doc_from_cache();
        self.queue_save();
        Ok(strip)
    }
}
