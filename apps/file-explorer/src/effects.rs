//! What a Files window does, as a typed effect vocabulary.
//!
//! Showing folders, copying and pasting, opening files and sharing them
//! change the window or the world outside, not the Files document. (Creating,
//! renaming, trashing and moving files are the document's undoable actions:
//! see [`crate::file_ops`].) Every caller — a menu, a drag,
//! the palette, an agent, a script — reaches the same handler
//! ([`crate::FileExplorerWidget`]'s `ResourceSurface::effect`) with one of
//! these values. An entry is named by the id of its published row
//! (`files.entry:…`), or by its name in the folder shown. The effects marked
//! `row` are offered on the row they act on, bound to it.

use ui::collaboration::{NearbyDestinationHandle, NearbyTargetHandle, NearbyTransferHandle};
// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource;
use ui::tilo_resource::Effect;

/// What a Files window does to what it shows.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "files.effects")]
pub enum FilesEffect {
    /// Refresh folder.
    #[effect(kind = "read", idempotent)]
    Refresh,
    /// Select entries.
    ///
    /// Replaces this window's selection with the currently published rows.
    #[effect(view, returns)]
    SelectEntries {
        #[param("Published entry row ids in the folder shown; at most 256")]
        entries: Vec<String>,
    },
    /// Select all.
    #[effect(view, returns)]
    SelectAll,
    /// Clear selection.
    #[effect(view, returns)]
    ClearSelection,
    /// Copy selection.
    #[effect(kind = "read", job)]
    CopySelection,
    /// Cut selection.
    #[effect(job)]
    CutSelection,
    /// File clipboard status.
    ///
    /// Returns the workspace's current opaque offer and entry count. It does
    /// not read payloads, grant access, or change this window's selection.
    #[effect(kind = "read", job, returns)]
    ClipboardStatus,
    /// Recover interrupted clipboard transfers.
    ///
    /// Uses this receiving record's durable receipts and current file grants.
    /// Completed transfers remain in place. Interrupted Cut restores its
    /// source; Copy and remaining recovery objects appear in Trash.
    #[effect(job, cancellable, returns)]
    RecoverClipboardTransfers,
    /// Paste.
    ///
    /// Copies (or moves, after Cut) what the window holds into the folder
    /// shown.
    #[effect(job, cancellable)]
    Paste,
    /// Go to folder…
    ///
    /// Shows the folder at this absolute path; answers with its resource and
    /// entries.
    #[effect(view, returns)]
    GoTo {
        #[param("Absolute path of the folder to show")]
        path: String,
    },
    /// Open Trash.
    ///
    /// Shows this workspace's private Trash and the user's already granted
    /// home Trash. Opening it grants no filesystem access.
    #[effect(kind = "read", view, returns)]
    OpenTrash,
    /// Open folder.
    ///
    /// Shows the folder at this absolute path, as go-to does; answers with
    /// its resource and entries.
    #[effect(view, returns)]
    OpenFolder {
        #[param("Absolute path of the folder to show")]
        path: String,
    },
    /// Parent folder.
    #[effect(view)]
    Up,
    /// Back.
    #[effect(view)]
    Back,
    /// Forward.
    #[effect(view)]
    Forward,
    /// Show recent files.
    #[effect(view)]
    Recent,
    /// Toggle hidden files.
    #[effect(view)]
    ToggleHidden,
    /// Use list view.
    #[effect(view)]
    List,
    /// Use grid view.
    #[effect(view)]
    Grid,
    /// Sort by name.
    #[effect(view)]
    SortName,
    /// Sort by size.
    #[effect(view)]
    SortSize,
    /// Sort by type.
    #[effect(view)]
    SortType,
    /// Sort by modified date.
    #[effect(view)]
    SortModified,
    /// Sort ascending.
    #[effect(view)]
    SortAscending,
    /// Sort descending.
    #[effect(view)]
    SortDescending,
    /// Increase item size.
    #[effect(view)]
    ZoomIn,
    /// Decrease item size.
    #[effect(view)]
    ZoomOut,
    /// Keyboard shortcuts.
    #[effect(view)]
    KeyboardShortcuts,
    /// Open terminal here.
    #[effect(kind = "read")]
    OpenTerminal,
    /// Undo move to Trash.
    ///
    /// Puts back what this window last moved to Trash.
    UndoTrash,
    /// Empty Trash.
    ///
    /// Deletes everything in Trash for good, from the Trash folder.
    #[effect(destructive)]
    EmptyTrash,
    /// Open.
    #[effect(view, row)]
    OpenItem {
        #[param("The row: an entry, a place or a found record")]
        item: String,
    },
    /// Show in folder.
    #[effect(view, row)]
    Reveal {
        #[param("The collaborative record's id")]
        record: String,
    },
    /// Open containing folder.
    #[effect(view, row)]
    RevealRecent {
        #[param("The Recent entry's published row id")]
        entry: String,
    },
    /// Remove from Recent.
    ///
    /// Forgets this device's recent-use entry. The file remains untouched.
    #[effect(view, row)]
    ForgetRecent {
        #[param("The Recent entry's published row id")]
        entry: String,
    },
    /// New file from template.
    ///
    /// Creates a file from a template in the folder shown.
    NewFromTemplate {
        #[param("The template's row id (a files.template child)")]
        template: String,
    },
    /// Open with…
    #[effect(row, kind = "read")]
    OpenWith {
        #[param("The entry's row id")]
        entry: String,
        #[param("The application to open it with")]
        app: String,
        #[param("Always open files of this type with it")]
        always: Option<bool>,
    },
    /// Copy to folder…
    #[effect(row)]
    CopyTo {
        #[param("The entry's row id")]
        entry: String,
        #[param("Absolute path of an existing folder; the name is kept")]
        folder: String,
    },
    /// Copy.
    #[effect(row, kind = "read", job)]
    Copy {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Cut.
    #[effect(row, job)]
    Cut {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Restore.
    #[effect(row, job, cancellable)]
    Restore {
        #[param("The Trash entry's row id")]
        entry: String,
    },
    /// Delete permanently.
    #[effect(row, destructive)]
    DeletePermanently {
        #[param("The Trash entry's row id")]
        entry: String,
    },
    /// Properties.
    #[effect(view, row)]
    Properties {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Sharing and access.
    #[effect(view, row)]
    Sharing {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Version history.
    #[effect(view, row)]
    History {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Storage.
    #[effect(view, row)]
    Availability {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Keep offline.
    #[effect(row)]
    KeepOffline {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Use automatic storage.
    #[effect(row)]
    AutomaticStorage {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Free up local space.
    #[effect(row)]
    FreeLocalSpace {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Retry download.
    #[effect(row)]
    RetryDownload {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Send to a nearby device…
    #[effect(view, row)]
    SendNearby {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Send to a nearby device.
    #[effect(row, job, kind = "read")]
    SendTo {
        #[param("The entry's row id")]
        entry: String,
        #[param("The nearby device")]
        target: NearbyTargetHandle,
    },
    /// Send a file to an independently verified `AirDrop` lab receiver.
    /// The compositor, rather than this application's parameters, collects
    /// the receiver fingerprint and approves exactly one staged file.
    #[effect(row, job, privileged, kind = "read")]
    SendAirDrop {
        #[param("The entry's row id")]
        entry: String,
        #[param("The receiver's literal IP address")]
        address: String,
        #[param("The receiver's TLS port")]
        port: u16,
    },
    /// Create one-time share link.
    #[effect(row, job)]
    ShareLink {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Save an incoming transfer.
    #[effect(row, privileged)]
    AcceptNearby {
        #[param("The incoming transfer")]
        transfer: NearbyTransferHandle,
        #[param("Where to save it")]
        destination: NearbyDestinationHandle,
    },
    /// Decline an incoming transfer and discard its retained partial bytes.
    #[effect(row, destructive)]
    DeclineNearby {
        #[param("The incoming transfer")]
        transfer: NearbyTransferHandle,
    },
    /// Inspect a transfer's current phase and confirmed byte progress.
    #[effect(row, kind = "read", returns, idempotent)]
    InspectNearby {
        #[param("The visible transfer")]
        transfer: NearbyTransferHandle,
    },
    /// Retry a transfer.
    #[effect(row, kind = "read")]
    RetryNearby {
        #[param("The failed transfer")]
        transfer: NearbyTransferHandle,
    },
    /// Cancel a transfer.
    #[effect(row)]
    CancelNearby {
        #[param("The running transfer")]
        transfer: NearbyTransferHandle,
    },
    /// Open the account's files.
    #[effect(view, row)]
    OpenAccount {
        #[param("The remote account's id")]
        account: String,
    },
    /// Account details.
    #[effect(view, row)]
    AccountDetails {
        #[param("The remote account's id")]
        account: String,
    },
    /// Sign in again.
    #[effect(view, row)]
    Reauthenticate {
        #[param("The remote account's id")]
        account: String,
    },
}

/// `effect` bound to its row, published as `id` labelled `label`: the ids
/// and labels the row offered before (`open`, `create`, `retry`), which
/// searches and agents address.
pub fn row_action(
    effect: &FilesEffect,
    id: impl Into<String>,
    label: impl Into<String>,
) -> ui::tilo_actions::ActionSpec {
    let mut spec = effect.bound();
    spec.id = id.into();
    spec.label = label.into();
    spec
}
