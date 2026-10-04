//! The editor's commands: what the platform palette lists beside the project's
//! files, and what the keyboard shortcuts run.
//!
//! The editor draws no palette of its own. Each command is published as a
//! resource action (see `actions.rs`), so the platform search finds it, an
//! agent can call it, and a shortcut in `app.rs` runs the same action.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    NewFile,
    OpenFolder,
    OpenFile,
    Save,
    SaveAs,
    SaveAll,
    RevertFile,
    CloseTab,
    Find,
    Replace,
    SearchProject,
    GoToLine,
    DuplicateLine,
    MoveLineUp,
    MoveLineDown,
    DeleteLine,
    ToggleComment,
    ToggleBottomPanel,
    GoToProblems,
    GoToTerminal,
    ToggleSidebar,
}

impl Command {
    pub const fn id(self) -> &'static str {
        match self {
            Self::NewFile => "new-file",
            Self::OpenFolder => "open-folder",
            Self::OpenFile => "open-file",
            Self::Save => "save",
            Self::SaveAs => "save-as",
            Self::SaveAll => "save-all",
            Self::RevertFile => "revert-file",
            Self::CloseTab => "close-tab",
            Self::Find => "find",
            Self::Replace => "replace",
            Self::SearchProject => "search-project",
            Self::GoToLine => "go-to-line",
            Self::DuplicateLine => "duplicate-line",
            Self::MoveLineUp => "move-line-up",
            Self::MoveLineDown => "move-line-down",
            Self::DeleteLine => "delete-line",
            Self::ToggleComment => "toggle-comment",
            Self::ToggleBottomPanel => "toggle-bottom-panel",
            Self::GoToProblems => "go-to-problems",
            Self::GoToTerminal => "go-to-terminal",
            Self::ToggleSidebar => "toggle-sidebar",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::catalogue()
            .into_iter()
            .find_map(|(command, _)| (command.id() == id).then_some(command))
    }

    /// Every command and the name the palette lists it under.
    pub fn catalogue() -> Vec<(Command, &'static str)> {
        vec![
            (Command::OpenFolder, "Open project folder"),
            (Command::OpenFile, "Open file"),
            (Command::NewFile, "New file"),
            (Command::Save, "Save"),
            (Command::SaveAs, "Save as"),
            (Command::SaveAll, "Save all"),
            (Command::RevertFile, "Revert to saved"),
            (Command::CloseTab, "Close tab"),
            (Command::Find, "Find in file"),
            (Command::Replace, "Replace in file"),
            (Command::SearchProject, "Search in project"),
            (Command::GoToLine, "Go to line"),
            (Command::DuplicateLine, "Duplicate line"),
            (Command::MoveLineUp, "Move line up"),
            (Command::MoveLineDown, "Move line down"),
            (Command::DeleteLine, "Delete line"),
            (Command::ToggleComment, "Toggle line comment"),
            (Command::ToggleBottomPanel, "Show or hide the panel"),
            (Command::ToggleSidebar, "Show or hide the sidebar"),
            (Command::GoToProblems, "Show problems"),
            (Command::GoToTerminal, "Show terminal"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_listed_once_and_found_by_its_id() {
        let catalogue = Command::catalogue();
        for (command, label) in &catalogue {
            assert_eq!(Command::from_id(command.id()), Some(*command));
            assert!(!label.is_empty());
            assert!(
                label.chars().next().is_some_and(char::is_uppercase)
                    && !label.contains(": ")
                    && !label.chars().skip(1).any(char::is_uppercase),
                "{label} is in sentence case"
            );
        }
        let mut ids: Vec<_> = catalogue.iter().map(|(command, _)| command.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), catalogue.len());
        assert_eq!(Command::from_id("cycle-theme"), None);
    }
}
