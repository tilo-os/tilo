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
    #[cfg(test)]
    pub const ALL: [Self; 22] = [
        Self::NewFile,
        Self::OpenFolder,
        Self::OpenFile,
        Self::Save,
        Self::SaveAs,
        Self::SaveAll,
        Self::RevertFile,
        Self::Find,
        Self::Replace,
        Self::SearchProject,
        Self::GoToLine,
        Self::DuplicateLine,
        Self::MoveLineUp,
        Self::MoveLineDown,
        Self::DeleteLine,
        Self::ToggleComment,
        Self::ToggleBottomPanel,
        Self::NextTheme,
        Self::GoToProblems,
        Self::GoToSearch,
        Self::GoToTerminal,
        Self::ToggleSidebar,
    ];
}
