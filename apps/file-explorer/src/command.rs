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
use vello::kurbo::Point;

use crate::model::{SortBy, SortOrder, ViewMode};

/// A single user intent emitted by an interactive child widget.
#[derive(Clone, Debug, PartialEq)]
pub enum FileCommand {
    /// Navigate into a specific directory (sidebar / breadcrumb / row).
    Navigate(PathBuf),
    /// Open a path typed/accepted in the editable path bar: navigate into it
    /// when it is (or isn't yet) a directory, else open the file. Distinct from
    /// [`Navigate`](FileCommand::Navigate) so a typed file path opens rather
    /// than being treated as a folder.
    OpenPath(PathBuf),
    /// Go to the parent directory.
    Up,
    /// Go back / forward in the navigation history.
    Back,
    Forward,
    /// Re-read the current directory.
    Refresh,
    /// A pointer press on entry `index` with modifier state — drives
    /// selection (and, via the app's double-click detection, opening).
    Click {
        index: usize,
        ctrl: bool,
        shift: bool,
    },
    /// A right-press landed on displayed entry `Some(index)` (or empty space,
    /// `None`). Selects the target so the context menu that follows acts on
    /// what was clicked, not a stale selection — unless the target is already
    /// part of a multi-selection, which is preserved so a menu action can
    /// apply to all of it. `None` clears the selection (empty-space menu).
    ContextSelect(Option<usize>),
    /// Activate entry `index` directly (keyboard Enter / grid double-click).
    Activate(usize),
    /// A sortable list header at column `index` was clicked.
    SortColumn(usize),
    /// Switch the main view mode.
    SetView(ViewMode),
    /// Open the toolbar's sort menu (the labeled "Sort" affordance — the
    /// primary way to sort in grid mode, which has no clickable column header).
    OpenSortMenu,
    /// Open the toolbar's "View options" overflow menu (new folder / file,
    /// show-hidden toggle, open terminal — every item clearly labeled).
    OpenViewMenu,
    /// Set the sort key (name/size/type/date) directly. Reachable from the
    /// right-click menu, so it works in grid mode too (which has no header).
    SetSort(SortBy),
    /// Flip ascending/descending.
    ToggleSortOrder,
    /// Set the sort direction explicitly (from the grid "Sort by" menu, which
    /// offers Ascending / Descending as labelled choices rather than a flip).
    SetSortOrder(SortOrder),
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
    /// Request the favourite context menu for the favourite at `path` (a
    /// right-press on a sidebar favourite). The app opens a labelled menu with
    /// the normal folder actions plus "Remove from Favourites".
    OpenFavoriteMenu(PathBuf),
    /// Begin dragging a row out of the file list (a press that moved past the
    /// drag threshold). The app carries the row(s) as a floating ghost until
    /// they are dropped: onto a folder (or the parent) they move there; a single
    /// folder dropped on the sidebar is pinned to Favourites. `is_dir` lets the
    /// app gate pinning to folders without a filesystem probe.
    /// A plain press landed on a row: the app arms a drag of `path` from
    /// `at` and starts it once the held pointer travels far enough. Armed
    /// at the app level because the content pane — and the view that saw
    /// the press — is rebuilt when the click changes the selection, so
    /// state parked in the view would not survive to the first move.
    ArmDrag {
        path: PathBuf,
        is_dir: bool,
        at: Point,
    },
    BeginDrag {
        path: PathBuf,
        is_dir: bool,
    },
    /// Add a folder to the favourites / places list.
    AddFavorite(PathBuf),
    /// Remove a folder from the favourites list.
    RemoveFavorite(PathBuf),
    /// Move favourite `from` to index `to` (drag-to-reorder drop).
    ReorderFavorite {
        from: usize,
        to: usize,
    },
    /// Increase / decrease the icon/row zoom level (Ctrl +/-, Ctrl-scroll).
    ZoomIn,
    ZoomOut,
    /// Toggle showing dotfiles.
    ToggleHidden,
    /// Restore the current selection from the Trash to its original location
    /// (only meaningful while viewing the Trash).
    RestoreFromTrash,
    /// Permanently empty the Trash.
    EmptyTrash,
    /// Persist selected collaborative records on this device, or return them
    /// to automatic cache management.
    KeepOffline,
    RemoveOfflinePin,
    /// Request policy-checked asynchronous removal of locally cached bytes.
    FreeLocalSpace,
    /// Confirm the separately warned override after the daemon refused to
    /// certify an ordinary eviction as safe. Never exposed directly in menus.
    ConfirmDestructiveEviction {
        paths: Vec<PathBuf>,
    },
    /// Re-attempt hydration after a projected or failed transfer. The daemon
    /// performs the transfer asynchronously and reports progress through the
    /// availability projection.
    RetryTransfer,
    /// Open one consolidated properties surface for local and collaborative
    /// metadata without reading or decoding file content.
    ShowProperties,
    /// Open detailed residency, transfer, policy, replica and freshness state.
    ShowAvailability,
    /// Open read-only authority-backed sharing details for the selected record.
    ShowSharing,
    /// Create a view-only, 24-hour, one-redemption link and copy it.
    CreateOneTimeShareLink,
    /// Grant view access to one identity-layer-resolved human root. Names are
    /// intentionally not carried in the command or authority protocol.
    ShareWithPerson {
        person: collab_types::Ed25519PublicKey,
    },
    /// Submit one fully specified share-composer choice. The app dispatches
    /// the authority request on its background worker.
    CreateShare(crate::sharing::ShareComposerRequest),
    /// Revoke one exact authority grant. Already-downloaded bytes cannot be
    /// recalled; this stops future record and blob access.
    RevokeShare {
        grant_id: uuid::Uuid,
    },
    /// Open recent immutable versions and conflict state for the selected record.
    ShowHistory,
    /// Restore one immutable version by appending a new Restore version.
    RestoreVersion {
        version_id: collab_types::FileVersionId,
    },
    /// Resolve one open conflict using the exact immutable member selected in
    /// the history projection. The filesystem revalidates it against current
    /// signed state before committing.
    ResolveConflict {
        conflict_id: uuid::Uuid,
        strategy: collab_types::ConflictStrategy,
        version_id: collab_types::FileVersionId,
    },
    CloseCollabPanel,
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
