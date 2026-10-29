//! What Code does that is not an edit of its project document: opening and
//! saving real files, showing a file or a location, and the editor's own
//! commands. Every one is published on the project resource, so a button, the
//! command palette, Ctrl+K, an agent and a script reach the same handler
//! (the editor's view, which answers them on [`crate::CodeApp`]).

use ui::tilo_resource::{self, Effect};

use crate::commands::Command;

/// The contract tag of an effect that acts where the caret is: the caret's
/// line, or every line of the selection when there is one. It needs an open
/// file, not a selection.
const CARET: &str = ui::tilo_actions::ContextRequirement::Caret.tag();

/// Code's effect vocabulary. File ids are the UUIDs of the project's file
/// records; paths are absolute filesystem paths.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "code_editor.effect")]
pub enum CodeEditorEffect {
    /// Open file.
    ///
    /// Offered on the file it opens: its id is the file row's, never typed.
    #[effect(row, kind = "read")]
    #[effect(icon = "file-code")]
    ShowFile {
        #[param("The file record's id")]
        file: String,
    },
    /// Show location.
    #[effect(kind = "read")]
    #[effect(icon = "locate-fixed")]
    Reveal {
        #[param("The file record's id, when the location is in a project file")]
        file: Option<String>,
        #[param("Absolute path of the file, for a location the tree has not read yet")]
        path: Option<ui::tilo_resource::AbsolutePath>,
        #[param("Zero-based line")]
        line: usize,
        #[param("Zero-based column, in characters")]
        column: usize,
        #[param("Show the project search results beside it")]
        results: bool,
    },
    /// Open a file from disk.
    ///
    /// Opens a file Code can reach: one it was handed or one in its open
    /// project. Any other file is opened from Files (its `open-with` with
    /// Code Editor), which hands the file to Code.
    #[effect(job, cancellable)]
    #[effect(icon = "folder-open")]
    OpenFile {
        #[param("Absolute path of the file; omit to choose one")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Open project folder.
    #[effect(job, cancellable)]
    #[effect(keywords = "open project, open workspace folder")]
    #[effect(icon = "folder-open")]
    OpenFolder {
        #[param("Absolute path of the folder; omit to choose one")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// New untitled file.
    #[effect(icon = "file-plus")]
    NewBuffer,
    /// Save file.
    #[effect(job, cancellable, destructive)]
    #[effect(keywords = "write file, save changes")]
    #[effect(icon = "hard-drive")]
    Save {
        #[param("The file record's id; omit for the active file")]
        file: Option<String>,
        #[param("Absolute path to write it to; omit to save in place")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
    /// Save as.
    #[effect(attended, job, cancellable)]
    #[effect(icon = "hard-drive")]
    SaveAs,
    /// Save all files.
    #[effect(icon = "hard-drive")]
    SaveAll,
    /// Format file.
    ///
    /// Reformats the active file as its language's formatter does, as one
    /// edit.
    #[effect(icon = "wand-sparkles")]
    FormatFile,
    /// Revert file to saved.
    #[effect(icon = "rotate-ccw")]
    RevertFile,
    /// Find in file.
    #[effect(kind = "read")]
    #[effect(keywords = "search in file, find text")]
    #[effect(icon = "search")]
    Find,
    /// Replace in file.
    #[effect(keywords = "find and replace, search and replace, substitute")]
    #[effect(icon = "type")]
    Replace,
    /// Search project.
    #[effect(kind = "read")]
    #[effect(keywords = "find in files, grep, search all files")]
    #[effect(icon = "search")]
    SearchProject,
    /// Go to line.
    #[effect(kind = "read")]
    #[effect(keywords = "jump to line, line number, goto")]
    #[effect(icon = "hash")]
    GoToLine {
        #[param("One-based line number; omit to ask for one")]
        value: Option<String>,
    },
    /// Duplicate line.
    #[effect(icon = "copy")]
    DuplicateLine,
    /// Move line up.
    #[effect(icon = "arrow-up")]
    MoveLineUp,
    /// Move line down.
    #[effect(icon = "arrow-down")]
    MoveLineDown,
    /// Delete line.
    #[effect(icon = "delete")]
    DeleteLine,
    /// Toggle line comment.
    #[effect(icon = "slash")]
    #[effect(tag = CARET)]
    ToggleComment,
    /// Toggle bottom panel.
    #[effect(kind = "read")]
    #[effect(icon = "panel-bottom")]
    ToggleBottomPanel,
    /// Cycle color theme.
    #[effect(kind = "read")]
    #[effect(icon = "palette")]
    NextTheme,
    /// Show problems.
    #[effect(kind = "read")]
    #[effect(keywords = "errors, warnings, diagnostics")]
    #[effect(icon = "triangle-alert")]
    GoToProblems,
    /// Show search results.
    #[effect(kind = "read")]
    #[effect(icon = "search")]
    GoToSearch,
    /// Show terminal.
    #[effect(kind = "read")]
    #[effect(keywords = "console, shell, command line")]
    #[effect(icon = "square-terminal")]
    GoToTerminal,
    /// Toggle sidebar.
    #[effect(kind = "read")]
    #[effect(icon = "panel-left")]
    ToggleSidebar,
    /// Turn language servers on or off.
    #[effect(kind = "read")]
    #[effect(icon = "code")]
    ToggleLanguageServers,
    /// Turn format on save on or off.
    #[effect(kind = "read")]
    #[effect(keywords = "auto format, format code, formatter")]
    #[effect(icon = "wand-sparkles")]
    ToggleFormatOnSave,
    /// Read file text.
    #[effect(row, kind = "read", returns, idempotent)]
    #[effect(icon = "file-text")]
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
            | Self::ShowFile { .. }
            | Self::Reveal { .. }
            | Self::OpenFile { path: Some(_) }
            | Self::OpenFolder { path: Some(_) }
            | Self::Save { file: Some(_), .. }
            | Self::Save { path: Some(_), .. }
            | Self::GoToLine { value: Some(_) }
            | Self::ToggleLanguageServers
            | Self::ToggleFormatOnSave => return None,
            Self::Save {
                file: None,
                path: None,
            } => Command::Save,
            Self::OpenFile { path: None } => Command::OpenFile,
            Self::OpenFolder { path: None } => Command::OpenFolder,
            Self::NewBuffer => Command::NewFile,
            Self::SaveAs => Command::SaveAs,
            Self::SaveAll => Command::SaveAll,
            Self::FormatFile => Command::FormatFile,
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
            Self::FormatFile => CodeEditorEffect::FormatFile,
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
            .map(|spec| spec.id.clone())
            .collect();
        for command in Command::ALL {
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
