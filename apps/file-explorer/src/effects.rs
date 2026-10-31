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
use ui::tilo_resource::{AbsolutePath, Effect};

/// The contract tag of an effect that acts on the entries the person
/// selected: an app's search lists it before the other actions.
const SELECTION: &str = ui::tilo_actions::ContextRequirement::Selection.tag();

/// What a Files window does to what it shows.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "files.effects")]
pub enum FilesEffect {
    /// Refresh folder.
    #[effect(kind = "read", idempotent)]
    #[effect(keywords = "reload, update folder")]
    #[effect(icon = "refresh")]
    Refresh,
    /// Select entries.
    ///
    /// Replaces this window's selection with the currently published rows.
    #[effect(view, returns)]
    #[effect(icon = "list-checks")]
    #[effect(audience = "agents")]
    SelectEntries {
        #[param("Published entry row ids in the folder shown; at most 256")]
        entries: Vec<String>,
    },
    /// Select all.
    #[effect(view, returns)]
    #[effect(icon = "list-checks")]
    SelectAll,
    /// Clear selection.
    #[effect(view, returns)]
    #[effect(icon = "x")]
    ClearSelection,
    /// Selected entries.
    ///
    /// Answers which entries are selected in the folder shown, with their
    /// absolute paths, changing nothing: what "these files" means when a
    /// request is about the selection.
    #[effect(view, kind = "read", returns, idempotent)]
    #[effect(icon = "list")]
    #[effect(audience = "agents")]
    SelectedEntries,
    /// Hand over a file.
    ///
    /// Gives a file's bytes to the next step, by their content address: an
    /// attachment to a message, a picture to place, a file to post. Reads
    /// the file and changes nothing.
    #[effect(content, kind = "read", idempotent)]
    #[effect(icon = "send")]
    #[effect(audience = "agents")]
    HandOver {
        #[param("The file: an absolute path, a name in the folder shown, or its entry row id")]
        file: String,
    },
    /// Read a file's text.
    ///
    /// Answers with what a file says: each page's text for a PDF, the text
    /// itself for Markdown and any other text file (a Word, `OpenDocument`
    /// or RTF document is Writer's `read-file`). A long file answers in
    /// parts: read on from the `next` an answer gives.
    #[effect(kind = "read", returns, idempotent)]
    #[effect(icon = "file-text")]
    #[effect(audience = "agents")]
    ReadText {
        #[param("The file: an absolute path, a name in the folder shown, or its entry row id")]
        file: String,
        #[param("The character to read from; the start when none")]
        from: Option<usize>,
    },
    /// Copy selection.
    #[effect(kind = "read", job)]
    #[effect(icon = "copy")]
    #[effect(tag = SELECTION)]
    CopySelection,
    /// Cut selection.
    #[effect(job)]
    #[effect(icon = "scissors")]
    #[effect(tag = SELECTION)]
    CutSelection,
    /// File clipboard status.
    ///
    /// Returns the workspace's current opaque offer and entry count. It does
    /// not read payloads, grant access, or change this window's selection.
    #[effect(kind = "read", job, returns)]
    #[effect(icon = "clipboard-paste")]
    #[effect(audience = "agents")]
    ClipboardStatus,
    /// Recover interrupted clipboard transfers.
    ///
    /// Uses this receiving record's durable receipts and current file grants.
    /// Completed transfers remain in place. Interrupted Cut restores its
    /// source; Copy and remaining recovery objects appear in Trash.
    #[effect(job, cancellable, returns)]
    #[effect(icon = "rotate-ccw")]
    RecoverClipboardTransfers,
    /// Paste.
    ///
    /// Copies (or moves, after Cut) what the window holds into the folder
    /// shown, and finishes once the files are there.
    #[effect(job, cancellable)]
    #[effect(icon = "clipboard-paste")]
    Paste {
        #[param(
            "For names the folder already has: keep both (the default), skip them, replace them (the existing items go to Trash), or ask the person in the window",
            choices = ["keep-both", "skip", "replace", "ask"]
        )]
        conflict: Option<String>,
    },
    /// Go to folder…
    ///
    /// Shows the folder at this absolute path; answers with its resource and
    /// entries.
    #[effect(view, returns)]
    #[effect(keywords = "open path, go to path, location, address bar")]
    #[effect(icon = "navigation")]
    GoTo {
        #[param("Absolute path of the folder to show")]
        path: AbsolutePath,
    },
    /// Open Trash.
    ///
    /// Shows this workspace's private Trash and the user's already granted
    /// home Trash. Opening it grants no filesystem access.
    #[effect(kind = "read", view, returns)]
    #[effect(keywords = "recycle bin, bin, deleted files, wastebasket")]
    #[effect(icon = "trash")]
    OpenTrash,
    /// Open folder.
    ///
    /// Shows the folder at this absolute path, as go-to does; answers with
    /// its resource and entries.
    #[effect(view, returns)]
    #[effect(icon = "folder-open")]
    #[effect(audience = "agents")]
    OpenFolder {
        #[param("Absolute path of the folder to show")]
        path: AbsolutePath,
    },
    /// Remove from favorites.
    ///
    /// Unpins a favorite from the places list of this window. The folder
    /// stays where it is; nothing on disk changes.
    #[effect(view, idempotent)]
    #[effect(icon = "x")]
    RemoveFavorite {
        #[param("Absolute path of the favorite to unpin")]
        path: AbsolutePath,
    },
    /// Move favorite.
    ///
    /// Puts one favorite before or after another in the places list, in this
    /// window's own order; nothing on disk changes.
    #[effect(view, idempotent)]
    #[effect(icon = "list")]
    MoveFavorite {
        #[param("Absolute path of the favorite to move")]
        source: AbsolutePath,
        #[param("Absolute path of the favorite it lands beside")]
        target: AbsolutePath,
        #[param("Whether it lands before the target, and after it otherwise")]
        before: bool,
    },
    /// Parent folder.
    #[effect(view)]
    #[effect(keywords = "go up, parent directory, up one level")]
    #[effect(icon = "arrow-up")]
    Up,
    /// Back.
    #[effect(view)]
    #[effect(icon = "arrow-left")]
    Back,
    /// Forward.
    #[effect(view)]
    #[effect(icon = "arrow-right")]
    Forward,
    /// Show recent files.
    #[effect(view)]
    #[effect(keywords = "recent files, recently opened, last opened")]
    #[effect(icon = "clock")]
    Recent,
    /// Toggle hidden files.
    #[effect(view)]
    #[effect(keywords = "show hidden files, hidden files, dotfiles")]
    #[effect(icon = "eye")]
    ToggleHidden,
    /// Use list view.
    #[effect(view)]
    #[effect(keywords = "details view, list view")]
    #[effect(icon = "list")]
    List,
    /// Use grid view.
    #[effect(view)]
    #[effect(keywords = "icon view, thumbnails")]
    #[effect(icon = "layout-grid")]
    Grid,
    /// Sort by name.
    #[effect(view)]
    #[effect(icon = "arrow-up-down")]
    SortName,
    /// Sort by size.
    #[effect(view)]
    #[effect(icon = "arrow-up-down")]
    SortSize,
    /// Sort by type.
    #[effect(view)]
    #[effect(icon = "arrow-up-down")]
    SortType,
    /// Sort by modified date.
    #[effect(view)]
    #[effect(icon = "arrow-up-down")]
    SortModified,
    /// Sort ascending.
    #[effect(view)]
    #[effect(icon = "arrow-up")]
    SortAscending,
    /// Sort descending.
    #[effect(view)]
    #[effect(icon = "arrow-down")]
    SortDescending,
    /// Increase item size.
    #[effect(view)]
    #[effect(icon = "maximize-2")]
    ZoomIn,
    /// Decrease item size.
    #[effect(view)]
    #[effect(icon = "minimize-2")]
    ZoomOut,
    /// Keyboard shortcuts.
    #[effect(view)]
    #[effect(icon = "keyboard")]
    KeyboardShortcuts,
    /// Open terminal here.
    #[effect(kind = "read")]
    #[effect(keywords = "command line, shell, console")]
    #[effect(icon = "square-terminal")]
    OpenTerminal,
    /// Empty Trash.
    ///
    /// Deletes everything in Trash for good, from the Trash folder.
    #[effect(destructive)]
    #[effect(keywords = "empty bin, empty recycle bin, clear trash")]
    #[effect(icon = "trash-2")]
    EmptyTrash,
    /// Open.
    #[effect(view, row)]
    #[effect(icon = "external-link")]
    OpenItem {
        #[param("The row: an entry, a place or a found record")]
        item: String,
    },
    /// Read entry
    ///
    /// Answers what a file, folder or app the window shows is: its name and
    /// absolute path, whether it is a folder, its type, size and when it
    /// was last changed, whether its content is on this device and whether
    /// it has an open conflict. A file's text is Read a file's text. It is
    /// the entry's read: what discloses its row, so copying, sending,
    /// sharing, renaming and its other actions are offered to whoever may
    /// read it, an agent too.
    #[effect(view, row, kind = "read", returns, idempotent)]
    #[effect(icon = "info")]
    ReadEntry {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Show in folder.
    #[effect(view, row)]
    #[effect(icon = "folder-open")]
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
    #[effect(row)]
    #[effect(keywords = "new file, create file, empty file")]
    #[effect(icon = "file-plus")]
    NewFromTemplate {
        #[param("The template's row id (a files.template child)")]
        template: String,
    },
    /// Open with…
    #[effect(row, kind = "read")]
    #[effect(icon = "app-window")]
    OpenWith {
        #[param("The entry's row id")]
        entry: String,
        #[param("The application to open it with")]
        app: String,
        #[param("Always open files of this type with it")]
        always: Option<bool>,
        #[param("Access to the original file", choices = ["read-only", "edit-in-parent"])]
        access: Option<String>,
    },
    /// Browse a provider folder without mounting it.
    #[effect(label = "Browse account", view)]
    #[effect(icon = "cloud")]
    #[effect(audience = "agents")]
    BrowseCloud {
        account: String,
        folder: Option<String>,
    },
    /// Load the next page of cloud entries.
    #[effect(label = "Show more", view)]
    #[effect(icon = "more-horizontal")]
    MoreCloud,
    /// Toggle a cloud entry in the selection.
    #[effect(view)]
    #[effect(icon = "check")]
    #[effect(audience = "agents")]
    SelectCloud { entry: String },
    /// Import selected files and folders to an explicit folder, or choose one.
    #[effect(label = "Import selection", view)]
    #[effect(icon = "cloud-download")]
    ImportCloud { folder: Option<String> },
    /// Observe enumeration, export and copy progress.
    #[effect(label = "Import progress", kind = "read", returns)]
    #[effect(icon = "hourglass")]
    #[effect(audience = "agents")]
    CloudProgress,
    /// Cancel the current transfer, retaining completed independent copies.
    #[effect(label = "Cancel import", view)]
    #[effect(icon = "x")]
    CancelCloud,
    /// Retry only unsuccessful top-level entries.
    #[effect(label = "Retry failed", view)]
    #[effect(icon = "refresh")]
    RetryCloud,
    /// Show the destination folder.
    #[effect(label = "Show imported files", view)]
    #[effect(icon = "folder-open")]
    RevealCloud,
    /// Create an independent editable copy of a converted native document.
    #[effect(label = "Make editable copy", row)]
    ImportConverted {
        #[param("The converted entry's published row id")]
        entry: String,
    },
    /// Import once from cloud…
    ///
    /// Copies the mounted entry's bytes into a local folder. Supported native
    /// documents are converted; the copy has no sync relationship.
    #[effect(row, job)]
    ImportOnce {
        #[param("The cloud entry's published row id")]
        entry: String,
        #[param("Absolute path of an existing local destination folder")]
        folder: AbsolutePath,
    },
    /// Copy to folder…
    #[effect(row, job)]
    #[effect(icon = "copy")]
    CopyTo {
        #[param("The entry's row id")]
        entry: String,
        #[param("Absolute path of an existing folder; the name is kept")]
        folder: AbsolutePath,
    },
    /// Copy.
    #[effect(row, kind = "read", job)]
    #[effect(icon = "copy")]
    Copy {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Cut.
    #[effect(row, job)]
    #[effect(icon = "scissors")]
    Cut {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Restore.
    #[effect(row, job, cancellable)]
    #[effect(icon = "rotate-ccw")]
    Restore {
        #[param("The Trash entry's row id")]
        entry: String,
    },
    /// Delete permanently.
    #[effect(row, destructive)]
    #[effect(keywords = "delete forever, remove permanently, shred")]
    #[effect(icon = "trash-2")]
    DeletePermanently {
        #[param("The Trash entry's row id")]
        entry: String,
    },
    /// Properties.
    #[effect(view, row)]
    #[effect(keywords = "get info, details, file info, file size")]
    #[effect(icon = "info")]
    Properties {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Sharing and access.
    #[effect(view, row)]
    #[effect(icon = "users")]
    Sharing {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Version history.
    #[effect(view, row)]
    #[effect(icon = "history")]
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
    #[effect(icon = "send")]
    SendNearby {
        #[param("The entry's row id")]
        entry: String,
    },
    /// Send to a nearby device.
    #[effect(row, job, kind = "read")]
    #[effect(icon = "send")]
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
    #[effect(keywords = "share, send link, share file")]
    #[effect(icon = "link")]
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
    #[effect(icon = "cloud")]
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
    /// Open a drive.
    ///
    /// Opens an NTFS drive (a Windows partition, an external drive) in
    /// Files. The shell asks the person first, in its own sheet, unless
    /// they chose to remember the drive.
    #[effect(view, row)]
    #[effect(icon = "hard-drive")]
    OpenDrive {
        #[param("The drive's id, as its row names it")]
        drive: String,
    },
    /// Uninstall.
    ///
    /// Uninstalls an app listed in the Applications place, or, for a part of
    /// the system, removes it from the person's apps. The shell asks the
    /// person first, in its own sheet, and does nothing unless they agree.
    #[effect(row)]
    UninstallApp {
        #[param("The app's row in the Applications place")]
        app: String,
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
    spec.type_mut().id = id.into();
    spec.type_mut().label = label.into();
    // A row's copy names what it acts on, so a person takes it from the row
    // even where the effect itself, its target typed out by id, is listed
    // for agents only (browsing a cloud folder, selecting a cloud entry).
    spec.type_mut()
        .contract
        .set_audience(ui::tilo_actions::Audience::Everyone);
    spec
}
