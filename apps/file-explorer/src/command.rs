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

use crate::model::ViewMode;

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
    /// Flip ascending/descending.
    ToggleSortOrder,
    /// Select every entry.
    SelectAll,
    /// Launch a terminal in the current directory.
    OpenTerminal,
    /// Create a new folder (prompts for a name).
    NewFolder,
    /// Rename the primary selection.
    Rename,
    /// Move the selection to the trash.
    Delete,
    /// Copy / cut the selection into the clipboard.
    Copy,
    Cut,
    /// Paste the clipboard into the current directory.
    Paste,
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
