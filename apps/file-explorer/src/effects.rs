//! What a Files window does to what it shows, as a typed effect vocabulary.

use ui::collaboration::{NearbyDestinationHandle, NearbyTargetHandle, NearbyTransferHandle};
use ui::tilo_resource;
use ui::tilo_resource::{AbsolutePath, Effect};

const SELECTION: &str = ui::tilo_actions::ContextRequirement::Selection.tag();

/// What a Files window does to what it shows.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "files.effects")]
pub enum FilesEffect {
    #[effect(kind = "read", idempotent)]
    #[effect(keywords = "reload, update folder")]
    #[effect(icon = "refresh")]
    #[effect(label = "files-shortcuts-refresh")]
    Refresh,
    /// Select entries.
    #[effect(view, returns)]
    #[effect(icon = "list-checks")]
    #[effect(audience = "agents")]
    SelectEntries {
        #[param("Published entry row ids in the folder shown; at most 256")]
        entries: Vec<String>,
    },
    #[effect(view, returns)]
    #[effect(icon = "list-checks")]
    #[effect(label = "files-shortcuts-select-all")]
    SelectAll,
    /// Clear selection.
    #[effect(view, returns)]
    #[effect(icon = "x")]
    ClearSelection,
    /// Selected entries.
    #[effect(view, kind = "read", returns, idempotent)]
    #[effect(icon = "list")]
    #[effect(audience = "agents")]
    SelectedEntries,
    /// Hand over a file.
    #[effect(content, kind = "read", idempotent)]
    #[effect(icon = "send")]
    #[effect(audience = "agents")]
    HandOver {
        #[param("The file: an absolute path, a name in the folder shown, or its entry row id")]
        file: String,
    },
    /// Read a file's text.
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
    #[effect(kind = "read", job, returns)]
    #[effect(icon = "clipboard-paste")]
    #[effect(audience = "agents")]
    ClipboardStatus,
    /// Recover interrupted clipboard transfers.
    #[effect(job, cancellable, returns)]
    #[effect(icon = "rotate-ccw")]
    RecoverClipboardTransfers,
    #[effect(job, cancellable)]
    #[effect(icon = "clipboard-paste")]
    #[effect(label = "files-menu-paste")]
    Paste {
        #[param(
            "For names the folder already has: keep both (the default), skip them, replace them (the existing items go to Trash), or ask the person in the window",
            choices = ["keep-both", "skip", "replace", "ask"]
        )]
        conflict: Option<String>,
    },
    /// Go to folder…
    #[effect(view, returns)]
    #[effect(keywords = "open path, go to path, location, address bar")]
    #[effect(icon = "navigation")]
    GoTo {
        #[param("Absolute path of the folder to show")]
        path: AbsolutePath,
    },
    /// Open Trash.
    #[effect(kind = "read", view, returns)]
    #[effect(keywords = "recycle bin, bin, deleted files, wastebasket")]
    #[effect(icon = "trash")]
    OpenTrash,
    /// Open folder.
    #[effect(view, returns)]
    #[effect(icon = "folder-open")]
    #[effect(audience = "agents")]
    OpenFolder {
        #[param("Absolute path of the folder to show")]
        path: AbsolutePath,
    },
    /// Remove from favorites.
    #[effect(view, idempotent)]
    #[effect(icon = "x")]
    RemoveFavorite {
        #[param("Absolute path of the favorite to unpin")]
        path: AbsolutePath,
    },
    /// Move favorite.
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
    #[effect(view)]
    #[effect(icon = "arrow-left")]
    #[effect(label = "files-back")]
    Back,
    #[effect(view)]
    #[effect(icon = "arrow-right")]
    #[effect(label = "files-forward")]
    Forward,
    /// Show recent files.
    #[effect(view)]
    #[effect(keywords = "recent files, recently opened, last opened")]
    #[effect(icon = "clock")]
    Recent,
    #[effect(view)]
    #[effect(keywords = "show hidden files, hidden files, dotfiles")]
    #[effect(icon = "eye")]
    #[effect(label = "files-shortcuts-hidden")]
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
    #[effect(view)]
    #[effect(icon = "arrow-up-down")]
    #[effect(label = "files-menu-sort-name")]
    SortName,
    #[effect(view)]
    #[effect(icon = "arrow-up-down")]
    #[effect(label = "files-menu-sort-size")]
    SortSize,
    #[effect(view)]
    #[effect(icon = "arrow-up-down")]
    #[effect(label = "files-menu-sort-type")]
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
    #[effect(view)]
    #[effect(icon = "keyboard")]
    #[effect(label = "files-shortcuts-title")]
    KeyboardShortcuts,
    #[effect(kind = "read")]
    #[effect(keywords = "command line, shell, console")]
    #[effect(icon = "square-terminal")]
    #[effect(label = "files-menu-open-terminal")]
    OpenTerminal,
    #[effect(destructive)]
    #[effect(keywords = "empty bin, empty recycle bin, clear trash")]
    #[effect(icon = "trash-2")]
    #[effect(label = "files-empty-trash-confirm")]
    EmptyTrash,
    #[effect(view, row)]
    #[effect(icon = "external-link")]
    #[effect(label = "files-shortcuts-open")]
    OpenItem {
        #[param("The row: an entry, a place or a found record")]
        item: String,
    },
    /// Read entry
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
    #[effect(view, row)]
    #[effect(label = "files-recent-reveal")]
    RevealRecent {
        #[param("The Recent entry's published row id")]
        entry: String,
    },
    #[effect(view, row)]
    #[effect(label = "files-recent-forget")]
    ForgetRecent {
        #[param("The Recent entry's published row id")]
        entry: String,
    },
    /// New file from template.
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
    },
    /// Browse a provider folder without mounting it.
    #[effect(view)]
    #[effect(icon = "cloud")]
    #[effect(audience = "agents")]
    BrowseCloud {
        account: String,
        folder: Option<String>,
    },
    /// Load the next page of cloud entries.
    #[effect(view)]
    #[effect(icon = "more-horizontal")]
    MoreCloud,
    /// Toggle a cloud entry in the selection.
    #[effect(view)]
    #[effect(icon = "check")]
    #[effect(audience = "agents")]
    SelectCloud { entry: String },
    /// Import selected files and folders to an explicit folder, or choose one.
    #[effect(view)]
    #[effect(icon = "cloud-download")]
    ImportCloud { folder: Option<String> },
    /// Observe enumeration, export and copy progress.
    #[effect(kind = "read", returns)]
    #[effect(icon = "hourglass")]
    #[effect(audience = "agents")]
    CloudProgress,
    #[effect(label = "files-cloud-cancel", view)]
    #[effect(icon = "x")]
    CancelCloud,
    #[effect(label = "files-cloud-retry", view)]
    #[effect(icon = "refresh")]
    RetryCloud,
    /// Show the destination folder.
    #[effect(view)]
    #[effect(icon = "folder-open")]
    RevealCloud,
    /// Create an independent editable copy of a converted native document.
    #[effect(row)]
    ImportConverted {
        #[param("The converted entry's published row id")]
        entry: String,
    },
    #[effect(row, job)]
    #[effect(label = "files-import-once")]
    ImportOnce {
        #[param("The cloud entry's published row id")]
        entry: String,
        #[param("Absolute path of an existing local destination folder")]
        folder: AbsolutePath,
    },
    #[effect(row, job)]
    #[effect(icon = "copy")]
    #[effect(label = "files-action-copy-to-folder")]
    CopyTo {
        #[param("The entry's row id")]
        entry: String,
        #[param("Absolute path of an existing folder; the name is kept")]
        folder: AbsolutePath,
    },
    #[effect(row, kind = "read", job)]
    #[effect(icon = "copy")]
    #[effect(label = "files-menu-copy")]
    Copy {
        #[param("The entry's row id")]
        entry: String,
    },
    #[effect(row, job)]
    #[effect(icon = "scissors")]
    #[effect(label = "files-menu-cut")]
    Cut {
        #[param("The entry's row id")]
        entry: String,
    },
    #[effect(row, job, cancellable)]
    #[effect(icon = "rotate-ccw")]
    #[effect(label = "files-menu-restore")]
    Restore {
        #[param("The Trash entry's row id")]
        entry: String,
    },
    #[effect(row, destructive)]
    #[effect(keywords = "delete forever, remove permanently, shred")]
    #[effect(icon = "trash-2")]
    #[effect(label = "files-permanent-delete-confirm")]
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
    #[effect(view, row)]
    #[effect(icon = "history")]
    #[effect(label = "files-version-history")]
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
    #[effect(row)]
    #[effect(label = "files-keep-offline")]
    KeepOffline {
        #[param("The entry's row id")]
        entry: String,
    },
    #[effect(row)]
    #[effect(label = "files-use-automatic-storage")]
    AutomaticStorage {
        #[param("The entry's row id")]
        entry: String,
    },
    #[effect(row)]
    #[effect(label = "files-free-local-space")]
    FreeLocalSpace {
        #[param("The entry's row id")]
        entry: String,
    },
    #[effect(row)]
    #[effect(label = "files-retry-download")]
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
    #[effect(row, job, privileged, kind = "read")]
    SendAirDrop {
        #[param("The entry's row id")]
        entry: String,
        #[param("The receiver's literal IP address")]
        address: String,
        #[param("The receiver's TLS port")]
        port: u16,
    },
    /// Open the desk's invite sheet on the workspace the entry is in, where a
    #[effect(row, job)]
    #[effect(keywords = "share, invite, send link, share file")]
    #[effect(icon = "user-plus")]
    Share {
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
    #[effect(view, row)]
    #[effect(icon = "hard-drive")]
    #[effect(label = "files-drive-open-action")]
    OpenDrive {
        #[param("The drive's id, as its row names it")]
        drive: String,
    },
    #[effect(row)]
    #[effect(label = "files-menu-uninstall")]
    UninstallApp {
        #[param("The app's row in the Applications place")]
        app: String,
    },
}

pub fn row_action(
    effect: &FilesEffect,
    id: impl Into<String>,
    label: impl Into<String>,
) -> ui::tilo_actions::ActionSpec {
    let mut spec = effect.bound();
    spec.type_mut().id = id.into();
    spec.type_mut().label = label.into();
    spec.type_mut()
        .contract
        .set_audience(ui::tilo_actions::Audience::Everyone);
    spec
}
