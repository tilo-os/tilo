//! The commands interactive children push to the app, which applies them after each event.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use vello::kurbo::Point;

use crate::model::{SortBy, SortOrder, ViewMode};

#[derive(Clone, Debug, PartialEq)]
pub enum FileCommand {
    Navigate(PathBuf),
    OpenPath(PathBuf),
    Up,
    Back,
    Forward,
    Refresh,
    Click {
        index: usize,
        ctrl: bool,
        shift: bool,
    },
    ContextSelect(Option<usize>),
    Activate(usize),
    ContextMenuAt {
        index: usize,
        at: Point,
    },
    MoveSelection {
        index: usize,
        extend: bool,
    },
    SortColumn(usize),
    SetView(ViewMode),
    OpenSortMenu,
    OpenViewMenu,
    SetSort(SortBy),
    ToggleSortOrder,
    ShowKeyboardShortcuts,
    SetSortOrder(SortOrder),
    SelectAll,
    OpenTerminal,
    NewFolder,
    Rename,
    RenameCommit,
    RenameCancel,
    Delete,
    Undo,
    ShowPathMenu {
        folders: Vec<(String, PathBuf)>,
        at: vello::kurbo::Point,
    },
    Copy,
    Cloud(crate::effects::FilesEffect),
    Cut,
    Paste,
    RecoverClipboardTransfers,
    ExtractHere,
    ExtractCurrentArchive,
    ExtractTo,
    CompressSelection(crate::archive::Packing),
    OpenNearbySend,
    SendNearby {
        target: ui::collaboration::NearbyTargetHandle,
    },
    ChooseNearbyDestination {
        handle: ui::collaboration::NearbyTransferHandle,
    },
    DecideNearby {
        handle: ui::collaboration::NearbyTransferHandle,
        destination: ui::collaboration::NearbyDestinationHandle,
        remember_for_session: bool,
    },
    RetryNearby {
        handle: ui::collaboration::NearbyTransferHandle,
    },
    CancelNearby {
        handle: ui::collaboration::NearbyTransferHandle,
    },
    OpenWithMenu,
    OpenWith {
        path: PathBuf,
        app_id: String,
    },
    OpenWithOther,
    RevealInFolder(PathBuf),
    ForgetRecent(PathBuf),
    ShowRemoteAccount(String),
    OpenDrive(String),
    Uninstall,
    RecheckRemoteAccount(String),
    OpenTemplatesMenu,
    OpenShortcutsMenu,
    RunShortcut {
        shortcut: String,
    },
    NewFromTemplate(PathBuf),
    ArmDrag {
        path: PathBuf,
        is_dir: bool,
        at: Point,
    },
    BeginDrag {
        path: PathBuf,
        is_dir: bool,
    },
    AddFavorite(PathBuf),
    RemoveFavorite(PathBuf),
    ReorderFavorite {
        from: usize,
        to: usize,
    },
    ZoomIn,
    ZoomOut,
    ToggleHidden,
    RestoreFromTrash,
    EmptyTrash,
    ConfirmPermanentDelete {
        paths: Vec<PathBuf>,
    },
    ConfirmEmptyTrash,
    KeepOffline,
    RemoveOfflinePin,
    FreeLocalSpace,
    ConfirmDestructiveEviction {
        paths: Vec<PathBuf>,
    },
    RetryTransfer,
    ShowProperties,
    ShowAvailability,
    Share,
    ShowHistory,
    SelectHistoryVersion {
        version_id: fs_protocol::FileVersionId,
    },
    ToggleHistoryCompare,
    ShowInWorkspaceActivity {
        record_id: String,
        version_id: fs_protocol::FileVersionId,
    },
    RestoreVersion {
        version_id: fs_protocol::FileVersionId,
    },
    ResolveConflict {
        conflict_id: uuid::Uuid,
        strategy: fs_protocol::ConflictStrategy,
        version_id: fs_protocol::FileVersionId,
    },
    ReviewConflictMerge {
        conflict_id: uuid::Uuid,
        version_id: fs_protocol::FileVersionId,
        other_device: [u8; 32],
    },
    CloseCollabPanel,
}

pub type CommandSink = Rc<RefCell<Vec<FileCommand>>>;

pub fn new_sink() -> CommandSink {
    Rc::new(RefCell::new(Vec::new()))
}

pub fn push(sink: &CommandSink, cmd: FileCommand) {
    sink.borrow_mut().push(cmd);
}
