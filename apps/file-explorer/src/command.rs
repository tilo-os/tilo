//! App-internal command bus.
//!
//! The toolkit's `Widget` trait has no message channel — widgets mutate
//! the CRDT or call imperative callbacks. The file explorer rebuilds its
//! whole widget subtree from `FileExplorerWidget` state each frame, so the
//! interactive widgets it builds (toolbar buttons, breadcrumb, sidebar,
//! the list/grid views) can't hold a `&mut` back to the app. Instead they
//! push [`FileCommand`]s into a shared [`CommandSink`]; the app drains the
//! sink after delivering each event and applies the commands to its own
//! state. This keeps all mutation in one place and survives the
//! rebuild-each-frame composition model.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::model::{SortBy, ViewMode};

/// A single user intent emitted by an interactive child widget.
#[derive(Clone, Debug, PartialEq)]
pub enum FileCommand {
    /// Navigate into a specific directory (sidebar / breadcrumb / row).
    Navigate(PathBuf),
    /// Go to the parent directory.
    Up,
    /// Go back / forward in the navigation history.
    Back,
    Forward,
    /// Re-read the current directory.
    Refresh,
    /// A pointer press on entry `index` with modifier state — drives
    /// selection (and, via the app's double-click detection, opening).
    Click { index: usize, ctrl: bool, shift: bool },
    /// Activate entry `index` directly (keyboard Enter / grid double-click).
    Activate(usize),
    /// A sortable list header at column `index` was clicked.
    SortColumn(usize),
    /// Switch the main view mode.
    SetView(ViewMode),
    /// Set the sort key (name/size/type/date) directly. Reachable from the
    /// right-click menu, so it works in grid mode too (which has no header).
    SetSort(SortBy),
    /// Flip ascending/descending.
    ToggleSortOrder,
    /// Select every entry.
    SelectAll,
    /// Launch a terminal in the current directory.
    OpenTerminal,
    /// Create a new folder, then drop into inline-rename so the user names
    /// it (Finder/Nautilus behaviour).
    NewFolder,
    /// Begin an inline rename of the primary selection (opens the rename bar).
    Rename,
    /// Commit the in-progress inline rename to the chosen name.
    RenameCommit,
    /// Abandon the in-progress inline rename.
    RenameCancel,
    /// Move the selection to the trash.
    Delete,
    /// Copy / cut the selection into the clipboard.
    Copy,
    Cut,
    /// Paste the clipboard into the current directory.
    Paste,
    /// Open the "New File" flyout populated from the Templates directory.
    /// Handled by the app's menu routing (opens a submenu), not applied.
    OpenTemplatesMenu,
    /// Create a new file in the current directory by copying the given
    /// template file, then drop into inline-rename.
    NewFromTemplate(PathBuf),
    /// Add a folder to the favourites / places list.
    AddFavorite(PathBuf),
    /// Remove a folder from the favourites list.
    RemoveFavorite(PathBuf),
    /// Move favourite `from` to index `to` (drag-to-reorder drop).
    ReorderFavorite { from: usize, to: usize },
    /// Increase / decrease the icon/row zoom level (Ctrl +/-, Ctrl-scroll).
    ZoomIn,
    ZoomOut,
    /// Toggle showing dotfiles.
    ToggleHidden,
}

/// Shared, cheaply-clonable queue the child widgets push into.
pub type CommandSink = Rc<RefCell<Vec<FileCommand>>>;

/// Build an empty command sink.
pub fn new_sink() -> CommandSink {
    Rc::new(RefCell::new(Vec::new()))
}

/// Convenience: push a command onto a sink clone.
pub fn push(sink: &CommandSink, cmd: FileCommand) {
    sink.borrow_mut().push(cmd);
}
