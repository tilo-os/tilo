//! What Code does that is not an edit of its project document: opening and
//! saving real files, showing a file or a location, and the editor's own
//! commands. Every one is published on the project resource, so a button, the
//! command palette, Ctrl+K, an agent and a script reach the same handler
//! ([`crate::CodeApp`]'s `ResourceSurface::effect`).

use ui::tilo_resource::{self, Effect};

use crate::commands::Command;

/// Code's effect vocabulary. File ids are the UUIDs of the project's file
/// records; paths are absolute filesystem paths.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "code_editor.effect")]
pub enum CodeEditorEffect {
    /// Open file.
    #[effect(permission = "read")]
    Open {
        #[param("The file record's id")]
        file: String,
    },
    /// Show location.
    #[effect(permission = "read")]
    Reveal {
        #[param("The file record's id, when the location is in a project file")]
        file: Option<String>,
        #[param("Absolute path of the file, for a location the tree has not read yet")]
        path: Option<String>,
        #[param("Zero-based line")]
        line: usize,
        #[param("Zero-based column, in characters")]
        column: usize,
        #[param("Show the project search results beside it")]
        results: bool,
    },
    /// Open a file from disk.
    #[effect(job, cancellable, device_only)]
    OpenFile {
        #[param("Absolute path of the file; omit to choose one")]
        path: Option<String>,
    },
    /// Open project folder.
    #[effect(job, cancellable, device_only)]
    OpenFolder {
        #[param("Absolute path of the folder; omit to choose one")]
        path: Option<String>,
    },
    /// New file.
    NewBuffer,
    /// Save file.
    #[effect(job, cancellable, destructive, device_only)]
    Save {
        #[param("The file record's id; omit for the active file")]
        file: Option<String>,
        #[param("Where to write it; omit to save in place")]
        path: Option<String>,
    },
    /// Save as.
    #[effect(job, cancellable, device_only)]
    SaveAs,
    /// Save all files.
    #[effect(device_only)]
    SaveAll,
    /// Revert file to saved.
    #[effect(device_only)]
    RevertFile,
    /// Find in file.
    #[effect(permission = "read")]
    Find,
    /// Replace in file.
    Replace,
    /// Search project.
    #[effect(permission = "read")]
    SearchProject,
    /// Go to line.
    #[effect(permission = "read")]
    GoToLine {
        #[param("One-based line number; omit to ask for one")]
        value: Option<String>,
    },
    /// Duplicate line.
    DuplicateLine,
    /// Move line up.
    MoveLineUp,
    /// Move line down.
    MoveLineDown,
    /// Delete line.
    DeleteLine,
    /// Toggle line comment.
    ToggleComment,
    /// Toggle bottom panel.
    #[effect(permission = "read")]
    ToggleBottomPanel,
    /// Cycle color theme.
    #[effect(permission = "read")]
    NextTheme,
    /// Show problems.
    #[effect(permission = "read")]
    GoToProblems,
    /// Show search results.
    #[effect(permission = "read")]
    GoToSearch,
    /// Show terminal.
    #[effect(permission = "read")]
    GoToTerminal,
    /// Toggle sidebar.
    #[effect(permission = "read")]
    ToggleSidebar,
    /// Turn language servers on or off.
    #[effect(permission = "read")]
    ToggleLanguageServers,
    /// Read file text.
    #[effect(row, permission = "read", returns, idempotent)]
    ReadText {
        #[param("The file record's id")]
        file: String,
        #[param("Zero-based position in Unicode scalar values")]
        start_chars: usize,
        #[param("Maximum Unicode scalar values to read, between 1 and 4096")]
        max_chars: usize,
    },
}

impl CodeEditorEffect {
    /// The editor command this effect runs, for the effects that are one of
    /// the command palette's commands.
    pub fn command(&self) -> Option<Command> {
        Some(match self {
            Self::ReadText { .. }
            | Self::Open { .. }
            | Self::Reveal { .. }
            | Self::OpenFile { path: Some(_) }
            | Self::OpenFolder { path: Some(_) }
            | Self::Save { file: Some(_), .. }
            | Self::Save { path: Some(_), .. }
            | Self::GoToLine { value: Some(_) }
            | Self::ToggleLanguageServers => return None,
            Self::Save {
                file: None,
                path: None,
            } => Command::Save,
            Self::OpenFile { path: None } => Command::OpenFile,
            Self::OpenFolder { path: None } => Command::OpenFolder,
            Self::NewBuffer => Command::NewFile,
            Self::SaveAs => Command::SaveAs,
            Self::SaveAll => Command::SaveAll,
            Self::RevertFile => Command::RevertFile,
            Self::Find => Command::Find,
            Self::Replace => Command::Replace,
            Self::SearchProject => Command::SearchProject,
            Self::GoToLine { value: None } => Command::GoToLine,
            Self::DuplicateLine => Command::DuplicateLine,
            Self::MoveLineUp => Command::MoveLineUp,
            Self::MoveLineDown => Command::MoveLineDown,
            Self::DeleteLine => Command::DeleteLine,
            Self::ToggleComment => Command::ToggleComment,
            Self::ToggleBottomPanel => Command::ToggleBottomPanel,
            Self::NextTheme => Command::NextTheme,
            Self::GoToProblems => Command::GoToProblems,
            Self::GoToSearch => Command::GoToSearch,
            Self::GoToTerminal => Command::GoToTerminal,
            Self::ToggleSidebar => Command::ToggleSidebar,
        })
    }
}

impl Command {
    /// The published effect that runs this command.
    pub fn effect(self) -> CodeEditorEffect {
        match self {
            Self::NewFile => CodeEditorEffect::NewBuffer,
            Self::OpenFolder => CodeEditorEffect::OpenFolder { path: None },
            Self::OpenFile => CodeEditorEffect::OpenFile { path: None },
            Self::Save => CodeEditorEffect::Save {
                file: None,
                path: None,
            },
            Self::SaveAs => CodeEditorEffect::SaveAs,
            Self::SaveAll => CodeEditorEffect::SaveAll,
            Self::RevertFile => CodeEditorEffect::RevertFile,
            Self::Find => CodeEditorEffect::Find,
            Self::Replace => CodeEditorEffect::Replace,
            Self::SearchProject => CodeEditorEffect::SearchProject,
            Self::GoToLine => CodeEditorEffect::GoToLine { value: None },
            Self::DuplicateLine => CodeEditorEffect::DuplicateLine,
            Self::MoveLineUp => CodeEditorEffect::MoveLineUp,
            Self::MoveLineDown => CodeEditorEffect::MoveLineDown,
            Self::DeleteLine => CodeEditorEffect::DeleteLine,
            Self::ToggleComment => CodeEditorEffect::ToggleComment,
            Self::ToggleBottomPanel => CodeEditorEffect::ToggleBottomPanel,
            Self::NextTheme => CodeEditorEffect::NextTheme,
            Self::GoToProblems => CodeEditorEffect::GoToProblems,
            Self::GoToSearch => CodeEditorEffect::GoToSearch,
            Self::GoToTerminal => CodeEditorEffect::GoToTerminal,
            Self::ToggleSidebar => CodeEditorEffect::ToggleSidebar,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_one_published_effect_and_back() {
        let published: Vec<String> = CodeEditorEffect::specs()
            .into_iter()
            .map(|spec| spec.id)
            .collect();
        for (command, _) in Command::all() {
            let effect = command.effect();
            assert!(
                published.iter().any(|id| id == effect.id()),
                "{} is published",
                effect.id()
            );
            assert_eq!(effect.command(), Some(command), "{}", effect.id());
        }
    }
}
