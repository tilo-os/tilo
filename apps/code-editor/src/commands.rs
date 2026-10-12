//! The editor's commands: each one is an effect the project publishes, so
//! the host palette (Ctrl+K, and Ctrl+P / Ctrl+Shift+P from the editor),
//! agents and the editor's own chords reach the same work.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    NewFile,
    OpenFolder,
    OpenFile,
    Save,
    SaveAs,
    SaveAll,
    RevertFile,
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
    NextTheme,
    GoToProblems,
    GoToSearch,
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
            Self::NextTheme => "next-theme",
            Self::GoToProblems => "go-to-problems",
            Self::GoToSearch => "go-to-search",
            Self::GoToTerminal => "go-to-terminal",
            Self::ToggleSidebar => "toggle-sidebar",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::catalogue()
            .into_iter()
            .find_map(|(command, _, _)| (command.id() == id).then_some(command))
    }

    pub fn all() -> Vec<(Command, &'static str)> {
        Self::catalogue()
            .into_iter()
            .map(|(command, label, _)| (command, label))
            .collect()
    }

    pub fn catalogue() -> Vec<(Command, &'static str, &'static str)> {
        vec![
            (
                Command::OpenFolder,
                "File: Open Folder\u{2026}",
                "Ctrl+Shift+O",
            ),
            (Command::OpenFile, "File: Open File\u{2026}", "Ctrl+O"),
            (Command::NewFile, "File: New File", ""),
            (Command::Save, "File: Save", "Ctrl+S"),
            (Command::SaveAs, "File: Save As\u{2026}", "Ctrl+Shift+S"),
            (Command::SaveAll, "File: Save All", "Ctrl+Alt+S"),
            (Command::RevertFile, "File: Revert to Saved", ""),
            (Command::Find, "Edit: Find in File", "Ctrl+F"),
            (Command::Replace, "Edit: Replace in File", "Ctrl+H"),
            (
                Command::SearchProject,
                "Edit: Search Project",
                "Ctrl+Shift+F",
            ),
            (Command::GoToLine, "Go: Go to Line\u{2026}", "Ctrl+G"),
            (
                Command::DuplicateLine,
                "Selection: Duplicate Line",
                "Shift+Alt+Down",
            ),
            (Command::MoveLineUp, "Selection: Move Line Up", "Alt+Up"),
            (
                Command::MoveLineDown,
                "Selection: Move Line Down",
                "Alt+Down",
            ),
            (
                Command::DeleteLine,
                "Selection: Delete Line",
                "Ctrl+Shift+K",
            ),
            (
                Command::ToggleComment,
                "Edit: Toggle Line Comment",
                "Ctrl+/",
            ),
            (
                Command::ToggleBottomPanel,
                "View: Toggle Bottom Panel",
                "Ctrl+J",
            ),
            (Command::ToggleSidebar, "View: Toggle Sidebar", "Ctrl+B"),
            (Command::GoToProblems, "View: Focus Problems", ""),
            (Command::GoToSearch, "View: Focus Search Results", ""),
            (Command::GoToTerminal, "View: Focus Terminal", ""),
        ]
    }
}
