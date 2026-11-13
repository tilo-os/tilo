//! Files' window: the folder it shows, its listing and selection, and the
//! changes it queues in the resource. Interactive children push
//! [`FileCommand`]s into a [`CommandSink`], which the window applies after
//! each event.

use ui::num::i64_of_usize;
use ui::tilo_resource::word;

use crate::command::{CommandSink, FileCommand, new_sink};
use crate::i18n::{arg, t, t_args};
use crate::metadata_refresh;
use crate::model::{Entry, SortBy, SortOrder, ViewMode, initial_path, sort_entries};
use crate::places_sidebar::{self, FavStore, Places};
use crate::views::file_view::{FileView, ThumbCtx};
use crate::{config, favorites, fileops, templates};
use host_fs::attributes as xattr;
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use thumbnail::{ThumbKey, ThumbnailService};
use ui::SelectionModel;
use ui::lock::MutexRecover as _;
#[cfg(test)]
use ui::tilo_actions::{ActionKind, ConfirmationPolicy, ExecutionMode, OperationKind, ResultSpec};
use ui::tilo_actions::{Availability, ResourceSpec};
use ui::time::{Duration, Instant, UNIX_EPOCH};
use ui::widget::DrawChild;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Button, Column, EmptyState, NavShell, ScrollAxis, Scrollable, Stack, TextInput, Widget,
};
use vello::kurbo::Point;
use vello::peniko::ImageData as PenikoImage;

mod applications;
mod clipboard_client;
mod cloud_browse;
mod drives;
mod entry_rows;
mod mounted_cloud;
mod window;
mod window_bar;
pub use clipboard_client::Receipt as ClipboardReceipt;
pub(crate) use clipboard_client::UndoRequest;
pub use window::FileExplorerWidget;
use window::{Chrome, MenuChange};
mod archives;
#[cfg(test)]
use archives::archive_member_copy;
use archives::{ArchiveMembers, archive_member_cached};
mod clash;
mod context_menu;
mod notice;
mod preview;
use preview::Preview;
mod read_text;
use clash::{ClashChoice, PendingClash};
mod selection_actions;
use notice::{Notice, NoticeAction};
mod folder_read;
use folder_read::{Purpose, Then};
mod shown_selection;

const TRANSFER_SHOWN_AFTER: Duration = Duration::from_millis(600);
/// How long a drag rests on a folder before the folder opens.
const SPRING_OPEN: Duration = Duration::from_millis(700);
const CLASH_TARGET: &str = "files-clash-choice:";
const COLLAB_PANEL_MAX_LINES: usize = 20;
fn paste_conflict(
    name: Option<&str>,
) -> Result<ui::collaboration::file_clipboard::Conflict, String> {
    use ui::collaboration::file_clipboard::Conflict;
    Ok(match name {
        None | Some("keep-both") => Conflict::KeepBoth,
        Some("skip") => Conflict::Skip,
        Some("replace") => Conflict::Replace,
        Some("ask") => Conflict::Ask,
        Some(other) => {
            return Err(format!(
                "conflict must be keep-both, skip, replace or ask, not {other:?}"
            ));
        }
    })
}

fn command_shortcut(command: &FileCommand) -> Option<ui::platform::Shortcut> {
    use ui::platform::Shortcut;
    Some(match command {
        FileCommand::Activate(_) => Shortcut::key("Enter"),
        FileCommand::Rename => Shortcut::key("F2"),
        FileCommand::Delete | FileCommand::Uninstall => Shortcut::key("Delete"),
        FileCommand::Copy => Shortcut::command("C"),
        FileCommand::Cut => Shortcut::command("X"),
        FileCommand::Paste => Shortcut::command("V"),
        FileCommand::SelectAll => Shortcut::command("A"),
        FileCommand::NewFolder => Shortcut::command("N").shift(),
        FileCommand::ToggleHidden => Shortcut::command("H"),
        FileCommand::Refresh => Shortcut::key("F5"),
        FileCommand::Undo => Shortcut::command("Z"),
        FileCommand::ShowKeyboardShortcuts => Shortcut::key("F1"),
        _ => return None,
    })
}

/// Where the part of a name the rename editor selects ends, or `None` for all of it.
fn rename_stem_end(name: &str, is_dir: bool) -> Option<usize> {
    if is_dir {
        return None;
    }
    name.rfind('.').filter(|&dot| dot > 0)
}

/// A separator divides two groups only; a group of one joins its neighbour.
fn tidy_menu(spec: Vec<(String, Option<FileCommand>)>) -> Vec<(String, Option<FileCommand>)> {
    tidy_groups(
        spec,
        |item| item.0.is_empty() && item.1.is_none(),
        || (String::new(), None),
    )
}

/// [`tidy_menu`] for any item type.
fn tidy_groups<T>(
    items: Vec<T>,
    is_separator: impl Fn(&T) -> bool,
    separator: impl Fn() -> T,
) -> Vec<T> {
    let mut groups: Vec<Vec<T>> = Vec::new();
    let mut current = Vec::new();
    for item in items {
        if is_separator(&item) {
            if !current.is_empty() {
                groups.push(std::mem::take(&mut current));
            }
        } else {
            current.push(item);
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }
    let mut merged: Vec<Vec<T>> = Vec::new();
    let mut carry: Vec<T> = Vec::new();
    for group in groups {
        let mut joined = std::mem::take(&mut carry);
        joined.extend(group);
        if joined.len() == 1 {
            carry = joined;
        } else {
            merged.push(joined);
        }
    }
    if !carry.is_empty() {
        match merged.last_mut() {
            Some(last) => last.extend(carry),
            None => merged.push(carry),
        }
    }
    let mut out = Vec::new();
    for (index, group) in merged.into_iter().enumerate() {
        if index > 0 {
            out.push(separator());
        }
        out.extend(group);
    }
    out
}

/// Long paths clip at narrow widths: keep their tail readable, and the whole in the path bar.
const STATE_SURFACE_LOCATION_CHARS: usize = 36;
/// Eager rows are shared with search and agents, so keep them few; any other entry is read from the live listing.
const MAX_EAGER_ENTRY_RESOURCES: usize = 128;
const MAX_ANSWERED_ENTRIES: usize = 64;

enum OpFollowUp {
    NewFolder,
    Rename,
    Trash { count: usize },
    Move { destination: String },
    Replaced,
    Paste { count: usize, cut: bool },
    Extract { name: String },
    Compress,
}

pub(crate) enum Ran {
    Done(Option<serde_json::Value>),
    Pending,
}

type MergePreflightResult = Arc<
    std::sync::Mutex<
        Option<(
            uuid::Uuid,
            fs_protocol::FileVersionId,
            Result<fs_protocol::MergePreflightProjectionV1, String>,
        )>,
    >,
>;
fn entry_answer(entry: &Entry) -> serde_json::Value {
    serde_json::json!({
        "name": entry.name,
        "path": entry.path.display().to_string(),
        "folder": entry.is_dir,
        "type": entry.mime,
        "size": entry.app.as_ref().map_or(Some(entry.size), |app| app.size),
        "modified": entry.modified.map(|modified| modified.to_rfc3339()),
        "app": entry.app.as_ref().map(|app| crate::applications::kind_label(app.kind)),
        "content_here": entry.content_is_local(),
        "conflict": entry.is_conflict(),
    })
}

fn remote_folder_availability(account: &crate::remote_accounts::RemoteAccount) -> Availability {
    if account.health == crate::remote_accounts::RemoteAccountHealth::Disabled {
        return Availability::Unavailable {
            code: "account_disabled".into(),
            reason: account.health.unavailable_reason().into(),
        };
    }
    if account.is_import() {
        return Availability::Available;
    }
    if !account.mount.as_deref().is_some_and(Path::is_dir) {
        return Availability::Unavailable {
            code: "account_not_mounted".into(),
            reason: "This account's folder is not mounted yet".into(),
        };
    }
    Availability::Available
}

fn build_thumb_service(config_dir: Option<&Path>) -> Option<ThumbnailService> {
    #[cfg(test)]
    crate::model::fixture_io_guard::ambient_io("build_thumb_service");
    enable_pdf_previews();
    let dir = config_dir.map_or_else(
        || std::env::temp_dir().join("tilo-file-explorer-thumbnails"),
        |d| d.join("thumbnails"),
    );
    ThumbnailService::open(&dir, 4).ok().or_else(|| {
        let fallback = std::env::temp_dir().join("tilo-file-explorer-thumbnails");
        ThumbnailService::open(&fallback, 4).ok()
    })
}

pub(crate) fn enable_pdf_previews() {
    thumbnail::set_pdf_rasteriser(tilo_pdf::first_page_preview);
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CollabPanel {
    title: String,
    summary: String,
    lines: Vec<String>,
    action: Option<CollabPanelAction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CollabPanelAction {
    RecheckAccount(String),
    DestructiveEviction(Vec<PathBuf>),
    PermanentDelete(Vec<PathBuf>),
    EmptyTrash,
    ApplySafeMerge {
        conflict_id: uuid::Uuid,
        version_id: fs_protocol::FileVersionId,
    },
    ChooseNearbyDestination {
        handle: ui::collaboration::NearbyTransferHandle,
    },
    RetryNearby {
        handle: ui::collaboration::NearbyTransferHandle,
    },
}

fn hex_prefix(bytes: &[u8], count: usize) -> String {
    hex::encode(bytes.get(..count).unwrap_or(bytes))
}

fn nearby_action_locator(kind: &str, bytes: &[u8; 16]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"so.tilo.files.nearby-action-locator.v1\0");
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(bytes);
    hex_prefix(&hasher.finalize(), 16)
}

/// An opaque resource id for a Templates entry: its path is not published.
pub(crate) fn template_resource_id(path: &str) -> String {
    let digest = Sha256::digest(path.as_bytes());
    format!("files.template:{}", hex_prefix(&digest, 16))
}

const RETRY_ROW: &str = "files.retry";

fn retry_row(may_pass: bool) -> ResourceSpec {
    let (effect, label) = if may_pass {
        (crate::effects::FilesEffect::Refresh, t("files-try-again"))
    } else {
        (crate::effects::FilesEffect::Up, t("files-open-enclosing"))
    };
    ResourceSpec::new(RETRY_ROW, RETRY_ROW, label.clone())
        .with_action(crate::effects::row_action(&effect, "invoke", label))
}

fn failure_state(title: String, detail: String, may_pass: bool) -> EmptyState {
    let row = retry_row(may_pass);
    let state = EmptyState::error(title, detail)
        .with_icon(ui::icons::Icon::FolderOpen)
        .with_action(row.label.clone())
        .with_action_id(RETRY_ROW);
    match row.actions.first().and_then(|action| {
        ui::tilo_actions::ActionBinding::static_json(&row, action, serde_json::Map::new()).ok()
    }) {
        Some(binding) => state.on_action(binding),
        None => state.with_enabled(false),
    }
}

pub(crate) fn published_place_resources(favorites: &[favorites::Favorite]) -> Vec<ResourceSpec> {
    favorites
        .iter()
        .map(|favorite| {
            let resource_id = crate::search_provider::place_resource_id(&favorite.path);
            let open = crate::effects::row_action(
                &crate::effects::FilesEffect::OpenItem {
                    item: resource_id.clone(),
                },
                crate::search_provider::OPEN_ACTION,
                crate::i18n::CATALOG.t("files-shortcuts-open"),
            );
            ResourceSpec::new(resource_id, "files.place", favorite.label.clone()).with_action(open)
        })
        .collect()
}

pub(crate) fn published_template_resources(templates: Vec<PathBuf>) -> Vec<ResourceSpec> {
    templates
        .into_iter()
        .map(|template| {
            let template_path = template.to_string_lossy().into_owned();
            let template_id = template_resource_id(&template_path);
            let create = crate::effects::row_action(
                &crate::effects::FilesEffect::NewFromTemplate {
                    template: template_id.clone(),
                },
                "create",
                templates::menu_label(&template),
            );
            ResourceSpec::new(
                template_id,
                "files.template",
                templates::menu_label(&template),
            )
            .with_action(create)
        })
        .collect()
}

pub(crate) fn resolve_places(
    places: &[String],
    home: Option<&Path>,
    dirs: &favorites::UserDirs,
) -> Vec<favorites::Favorite> {
    places
        .iter()
        .filter_map(|place| crate::places::path_of(place, home, dirs))
        .map(|path| favorites::favorite(path, home.unwrap_or_else(|| Path::new("/")), dirs))
        .collect()
}

fn short_fingerprint(key: &[u8; 32]) -> String {
    hex_prefix(key, 6)
}

fn short_uuid(id: uuid::Uuid) -> String {
    let simple = id.simple().to_string();
    simple.get(..8).unwrap_or(&simple).to_string()
}

fn transfer_phase_label(phase: fs_protocol::TransferPhase) -> String {
    t(match phase {
        fs_protocol::TransferPhase::Idle => "files-transfer-idle",
        fs_protocol::TransferPhase::Queued => "files-transfer-queued",
        fs_protocol::TransferPhase::Downloading => "files-transfer-downloading",
        fs_protocol::TransferPhase::Uploading => "files-transfer-uploading",
        fs_protocol::TransferPhase::Verifying => "files-transfer-verifying",
    })
}

fn conflict_strategy_label(strategy: fs_protocol::ConflictStrategy) -> String {
    t(match strategy {
        fs_protocol::ConflictStrategy::KeepCurrent => "files-strategy-keep-current",
        fs_protocol::ConflictStrategy::UseOther => "files-strategy-use-other",
        fs_protocol::ConflictStrategy::KeepBoth => "files-strategy-keep-both",
        fs_protocol::ConflictStrategy::Merge => "files-strategy-merge",
    })
}

#[derive(Clone)]
pub struct StartupInputs {
    pub initial_dir: PathBuf,
    pub workspace_roots: Vec<PathBuf>,
    pub record_roots: Vec<PathBuf>,
    pub remote_accounts: Vec<crate::remote_accounts::RemoteAccount>,
    pub trusted_humans: crate::identity_directory::TrustedHumanDirectory,
}

#[derive(Clone)]
pub struct HostedFileServices {
    pub thumbnails: Option<Arc<ThumbnailService>>,
    /// The host supplies the native clipboard lane; this is availability, not a grant.
    pub local_clipboard: bool,
}

enum FileServices {
    Legacy,
    Hosted(HostedFileServices),
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each boolean is independent named UI state for loading, clipboard, rename, layout, or visibility"
)]
pub struct FilesState {
    framework_history: ui::history_mode::checked_files::FileHistorySource,
    framework_history_binding: Option<(PathBuf, fs_protocol::FileRecordId)>,
    services: FileServices,
    startup_accounts: Option<Vec<crate::remote_accounts::RemoteAccount>>,
    current_path: PathBuf,
    home_path: Option<PathBuf>,
    entries: Vec<Entry>,
    find: String,
    selection: SelectionModel,
    shown_selection: shown_selection::ShownSelection,
    shown_folder: Option<Arc<host_fs::RetainedObject>>,
    shown_is_folder: bool,
    folder_reads: folder_read::FolderReads,
    local_clipboard: clipboard_client::Client,
    error: Option<String>,
    is_loading: bool,
    archive_listing: crate::archive::Listing,
    staged_transfer: Option<crate::file_ops::Running>,
    archive_members: ArchiveMembers,
    extract_chooser: Option<(Vec<PathBuf>, ui::portal::PortalChannel)>,
    last_click: Option<(usize, Instant)>,
    sort_by: SortBy,
    sort_order: SortOrder,
    view_mode: ViewMode,
    history: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    clipboard: Vec<PathBuf>,
    clipboard_cut: bool,
    notice: Notice,
    clash: Option<PendingClash>,
    preview: Option<Preview>,
    incoming: Vec<PathBuf>,
    undo: ui::UndoAccess,
    typing: ui::type_ahead::TypeAhead,
    /// The device whose filesystem this window changes; `None` outside a framework host.
    device: Option<String>,
    ops_drain: Option<ui::framework::Projection>,
    awaiting_ops: Vec<(crate::file_ops::FileOp, OpFollowUp)>,
    op_replies: Vec<(crate::file_ops::FileOp, ui::framework::EffectReply)>,
    answering_op: Option<crate::file_ops::FileOp>,
    settled_ops: std::collections::HashSet<String>,
    ops_presented: bool,
    local_outcomes: HashMap<String, crate::file_ops::Outcome>,
    status: Option<String>,
    next_metadata_refresh: Option<Instant>,
    directory_watch: crate::directory_watch::DirectoryWatch,
    folder_counts: Arc<crate::folder_counts::FolderCounts>,
    identity_watch: crate::directory_watch::AwaitedDirectoryWatch,
    catalog_watch: crate::directory_watch::RootsWatch,
    directory_changed: bool,
    last_directory_read: Instant,
    renaming: Option<usize>,
    rename_buffer: String,
    rename_seed_pending: bool,
    rename_input: TextInput,
    rename_rect: Rc<Cell<Option<vello::kurbo::Rect>>>,
    rename_paint: ui::own_paint::OwnPaint,
    rename_select_pending: bool,
    rect: vello::kurbo::Rect,
    toolbar: crate::toolbar::ToolbarHandle,
    header_in_bar: bool,
    bar_query: Option<String>,
    declared_bar: Option<(Option<u64>, window_bar::BarKey)>,
    bar_entries: window_bar::BarEntries,
    status_notice: StatusText,
    needs: ui::attention::Presses,
    status_said: Option<String>,
    archive_said: Option<PathBuf>,
    staged_hover: Option<usize>,
    staged_move_drag: bool,
    templates: templates::Catalog,
    composed_once: bool,
    cmds: CommandSink,
    opened: Rc<RefCell<Vec<String>>>,
    structure_dirty: bool,
    dispatcher: Option<ui::framework::Dispatcher<crate::resource::FileExplorer>>,
    preferences: Option<ui::framework::Setting<crate::resource::FilesPreferences>>,
    shown_preferences: Option<crate::resource::FilesPreferences>,
    menus: Vec<MenuChange>,
    staged: Option<StagedScenario>,
    last_cursor: Point,
    favs: FavStore,
    places_shown: Option<(Vec<ui::widget::NavSection>, Option<String>)>,
    places_choice: Rc<RefCell<Option<String>>>,
    places_context: Rc<RefCell<Option<(PathBuf, Point)>>>,
    stage_favorite_drag: bool,
    zoom_level: i32,
    show_hidden: bool,
    user_dirs: favorites::UserDirs,
    workspace_roots: Vec<PathBuf>,
    places_search_revision: u64,
    collaborative_search_revision: Cell<u64>,
    remote_accounts_search_revision: u64,
    places_search: crate::search_provider::PlacesSearch,
    collaborative_search: crate::search_provider::CollaborativeRecordSearch,
    collaborative_record_pager: RefCell<crate::model::WorkspaceRecordIndexPager>,
    remote_accounts_search: crate::search_provider::RemoteAccountSearch,
    pin_drag: Option<PathBuf>,
    drag_paths: Vec<PathBuf>,
    armed_drag: Option<(Point, PathBuf, bool)>,
    drag_seed: Vec<PathBuf>,
    collapse_on_release: Option<usize>,
    spring: Option<(PathBuf, Instant)>,
    drag_origin_is_dir: bool,
    drag_hover: Option<PathBuf>,
    nearby_send: crate::nearby_send::NearbySendState,
    nearby_send_path: Option<PathBuf>,
    shortcut_menu: crate::shortcut_menu::ShortcutMenuState,
    collab_panel: Option<CollabPanel>,
    collab_panel_focused: bool,
    collab_layout: Option<ui::widget::ModalLayout>,
    share_invite: crate::share_invite::ShareInvite,
    trusted_humans: crate::identity_directory::TrustedHumanDirectory,
    history_result: Arc<std::sync::Mutex<Option<Result<(), String>>>>,
    history_pending_path: Option<PathBuf>,
    history_pending_success: Option<String>,

    merge_preflight_result: MergePreflightResult,
    merge_preflight_pending: Option<(uuid::Uuid, fs_protocol::FileVersionId)>,
    effect_reply: Option<ui::framework::EffectReply>,
    accounts_root: Option<PathBuf>,
    accounts_cache: Vec<crate::remote_accounts::RemoteAccount>,
    cloud: cloud_browse::CloudView,
    #[cfg(test)]
    cloud_fixture_consent: bool,
    mounted_cloud: mounted_cloud::MountedCloud,
    drives: drives::Drives,
    applications: applications::Applications,
    host_intent: Option<ui::widget::AppHostIntent>,
    drop_targets: crate::views::file_view::DropTargets,
    item_rects: crate::views::file_view::ItemRects,
    marquee: Option<(Point, Point, Vec<usize>)>,
    scroll_offset: Rc<Cell<(f32, f32)>>,
    list_slot: crate::views::file_list::ListSlot,
    widths_at_press: Option<std::collections::BTreeMap<String, u32>>,
    thumbs: Option<Arc<ThumbnailService>>,
    recent: crate::recent::RecentFiles,
    recent_record: Option<ui::framework::PersonRecord<crate::recent::RecentRecord>>,
    open_request: crate::open_with::OpenRequest,
    document_candidates: crate::open_with::CandidateList,
    open_replies: crate::open_with::OpenReplies,
    open_with_cache: RefCell<HashMap<String, Vec<runtime_portal::tools::DocumentApp>>>,
    entry_ids: RefCell<EntryIds>,
    entry_rows: RefCell<entry_rows::EntryRows>,
    app_chooser: ui::search::SearchOpener,
    app_chooser_target: Option<PathBuf>,
    thumb_imgs: Rc<RefCell<HashMap<ThumbKey, PenikoImage>>>,
}

struct StagedScenario(Option<String>);

/// One test at a time: the waiting stack and the ledger are the process's,
/// so two tests running at once would see each other's rows.
#[cfg(test)]
pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    use ui::lock::MutexRecover as _;
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock_recover()
}

fn files_owner() -> ui::attention::Owner {
    ui::attention::Owner::new("so.tilo.files", "Files").with_icon(ui::icons::Icon::Folder)
}

#[derive(Debug, Default)]
struct StatusText(Option<String>);

impl StatusText {
    fn show(&mut self, text: impl Into<String>) {
        self.0 = Some(text.into());
    }

    fn dismiss(&mut self) {
        self.0 = None;
    }

    fn take(&mut self) -> Option<String> {
        self.0.take()
    }

    #[cfg(test)]
    fn is_shown(&self) -> bool {
        self.0.is_some()
    }

    #[cfg(test)]
    fn get(&self) -> Option<&String> {
        self.0.as_ref()
    }
}

impl FileExplorerWidget {
    pub fn new() -> Self {
        let widget =
            Self::with_persistence(config::config_dir().as_deref(), crate::model::user_home());
        {
            let mut state = widget.state();
            state.accounts_root = accounts::config_root();
            state.refresh_external_search_catalogs();
        }
        widget
    }

    #[doc(hidden)]
    pub fn with_persistence(config_dir: Option<&Path>, home: Option<PathBuf>) -> Self {
        Self::with_persistence_dirs(config_dir, home, favorites::UserDirs::from_system())
    }

    #[doc(hidden)]
    pub fn with_persistence_dirs(
        config_dir: Option<&Path>,
        home: Option<PathBuf>,
        user_dirs: favorites::UserDirs,
    ) -> Self {
        Self::with_inputs(config_dir, home, user_dirs, None, FileServices::Legacy)
    }

    pub fn with_startup_inputs(inputs: StartupInputs) -> Self {
        Self::with_inputs(
            None,
            None,
            favorites::UserDirs::default(),
            Some(inputs),
            FileServices::Legacy,
        )
    }

    pub fn with_startup_inputs_and_services(
        inputs: StartupInputs,
        services: HostedFileServices,
    ) -> Self {
        Self::with_inputs(
            None,
            None,
            favorites::UserDirs::default(),
            Some(inputs),
            FileServices::Hosted(services),
        )
    }

    #[cfg(feature = "samples")]
    pub fn for_scene(scene: &crate::scenarios::Scene) -> Self {
        enable_pdf_previews();
        let thumbnails = ThumbnailService::open(&scene.thumbnails, 4)
            .ok()
            .map(Arc::new);
        let widget = Self::with_inputs(
            None,
            Some(scene.home.clone()),
            favorites::UserDirs::default(),
            Some(StartupInputs {
                initial_dir: scene.home.clone(),
                workspace_roots: scene.workspace_roots.clone(),
                record_roots: Vec::new(),
                remote_accounts: Vec::new(),
                trusted_humans: crate::identity_directory::TrustedHumanDirectory::default(),
            }),
            FileServices::Hosted(HostedFileServices {
                thumbnails,
                local_clipboard: false,
            }),
        );
        {
            let mut state = widget.state();
            state.templates =
                templates::Catalog::new(Some(scene.home.join("Templates")), ui::WakeHandle::new());
            if scene.start != scene.home {
                state.navigate(scene.start.clone());
            }
        }
        widget
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Declare the complete stable widget state in one auditable constructor."
    )]
    fn with_inputs(
        config_dir: Option<&Path>,
        home: Option<PathBuf>,
        user_dirs: favorites::UserDirs,
        inputs: Option<StartupInputs>,
        services: FileServices,
    ) -> Self {
        let injected = inputs.is_some();
        let inputs = inputs.unwrap_or_else(|| StartupInputs {
            initial_dir: initial_path(),
            workspace_roots: crate::model::workspace_roots(),
            record_roots: crate::model::workspace_record_authority_roots(),
            remote_accounts: Vec::new(),
            trusted_humans: crate::identity_directory::TrustedHumanDirectory::load_default()
                .unwrap_or_default(),
        });
        let cwd = inputs.initial_dir;

        let zoom_level = 0;
        let favorites = home
            .as_deref()
            .map(|home| favorites::defaults(home, &user_dirs))
            .unwrap_or_default();
        let show_hidden = false;
        let workspace_roots = inputs.workspace_roots;
        let places_search = crate::search_provider::PlacesSearch::new(&favorites);
        let mut collaborative_record_pager =
            crate::model::WorkspaceRecordIndexPager::new(&inputs.record_roots);
        let initial_collaborative_page = collaborative_record_pager.next_page();
        let collaborative_search = crate::search_provider::CollaborativeRecordSearch::new(
            &initial_collaborative_page.records,
        );
        let remote_accounts = inputs.remote_accounts;
        let remote_accounts_search =
            crate::search_provider::RemoteAccountSearch::new(&remote_accounts);
        let cmds = new_sink();
        let (path_widget, actions_widget, toolbar) = crate::toolbar::FileToolbar::new(cmds.clone());
        // The header the bar takes over once it hosts the window.
        let nav = ui::window_toolbar::exempt!(Fallback, {
            NavShell::new()
                .content_kind(ui::widget::ContentKind::List)
                .with_id("files.shell")
                .content_header("", vec![Box::new(actions_widget)])
                .content_header_leading(Box::new(path_widget))
        });
        let thumbs = match &services {
            FileServices::Legacy => build_thumb_service(config_dir).map(Arc::new),
            FileServices::Hosted(hosted) => hosted.thumbnails.clone(),
        };
        let trusted_humans = inputs.trusted_humans;
        let startup_accounts = injected.then_some(remote_accounts);
        let mut state = FilesState {
            services,
            startup_accounts,
            current_path: cwd.clone(),
            home_path: home,
            entries: Vec::new(),
            find: String::new(),
            selection: SelectionModel::new(),
            shown_selection: shown_selection::ShownSelection::default(),
            shown_folder: None,
            shown_is_folder: false,
            folder_reads: folder_read::FolderReads::new(ui::WakeHandle::new()),
            error: None,
            is_loading: false,
            archive_listing: {
                let wake = ui::WakeHandle::new();
                crate::archive::Listing::new(move || wake.wake())
            },
            extract_chooser: None,
            staged_transfer: None,
            archive_members: {
                let wake = ui::WakeHandle::new();
                ArchiveMembers::new(move || wake.wake())
            },
            last_click: None,
            sort_by: SortBy::Name,
            sort_order: SortOrder::Ascending,
            view_mode: ViewMode::List,
            history: Vec::new(),
            forward: Vec::new(),
            clipboard: Vec::new(),
            clipboard_cut: false,
            notice: Notice::default(),
            clash: None,
            preview: None,
            incoming: Vec::new(),
            undo: ui::UndoAccess::default(),
            typing: ui::type_ahead::TypeAhead::default(),
            device: None,
            ops_drain: None,
            awaiting_ops: Vec::new(),
            op_replies: Vec::new(),
            answering_op: None,
            settled_ops: std::collections::HashSet::new(),
            ops_presented: false,
            local_outcomes: HashMap::new(),
            status: None,
            next_metadata_refresh: None,
            directory_watch: crate::directory_watch::DirectoryWatch::new(ui::WakeHandle::new()),
            folder_counts: {
                let wake = ui::WakeHandle::new();
                Arc::new(crate::folder_counts::FolderCounts::new(move || wake.wake()))
            },
            identity_watch: crate::directory_watch::AwaitedDirectoryWatch::new(
                ui::WakeHandle::new(),
            ),
            catalog_watch: crate::directory_watch::RootsWatch::new(ui::WakeHandle::new()),
            directory_changed: false,
            last_directory_read: ui::time::now(),
            renaming: None,
            rename_buffer: String::new(),
            rename_seed_pending: false,
            rename_input: TextInput::local()
                .with_kind(ui::widget::TextInputKind::Name)
                .with_id("files.rename_input")
                .with_placeholder(t("files-new-name-placeholder")),
            rename_rect: Rc::new(Cell::new(None)),
            rename_paint: ui::own_paint::OwnPaint::default(),
            rename_select_pending: false,
            rect: vello::kurbo::Rect::ZERO,
            toolbar,
            header_in_bar: false,
            bar_query: None,
            declared_bar: None,
            bar_entries: window_bar::BarEntries::new(),
            status_notice: StatusText::default(),
            needs: ui::attention::Presses::new(),
            status_said: None,
            archive_said: None,
            staged_hover: None,
            staged_move_drag: false,
            templates: crate::templates::Catalog::new(
                crate::templates::templates_dir(),
                ui::WakeHandle::new(),
            ),

            composed_once: false,
            cmds,
            opened: Rc::default(),
            structure_dirty: false,
            dispatcher: None,
            preferences: None,
            shown_preferences: None,
            menus: Vec::new(),
            staged: None,
            last_cursor: Point::ZERO,
            favs: places_sidebar::new_store(favorites),
            places_shown: None,
            places_choice: Rc::new(RefCell::new(None)),
            places_context: Rc::default(),
            stage_favorite_drag: false,
            zoom_level,
            show_hidden,
            user_dirs,
            workspace_roots,
            places_search_revision: 0,
            collaborative_search_revision: Cell::new(0),
            remote_accounts_search_revision: 0,
            places_search,
            collaborative_search,
            collaborative_record_pager: RefCell::new(collaborative_record_pager),
            remote_accounts_search,
            pin_drag: None,
            drag_paths: Vec::new(),
            armed_drag: None,
            drag_seed: Vec::new(),
            collapse_on_release: None,
            spring: None,
            drag_origin_is_dir: false,
            drag_hover: None,
            nearby_send: crate::nearby_send::NearbySendState::default(),
            nearby_send_path: None,
            shortcut_menu: crate::shortcut_menu::ShortcutMenuState::default(),
            collab_panel: None,
            collab_panel_focused: false,
            collab_layout: None,
            share_invite: crate::share_invite::ShareInvite::default(),
            trusted_humans,
            framework_history: ui::history_mode::checked_files::FileHistorySource::default(),
            framework_history_binding: None,
            history_result: Arc::new(std::sync::Mutex::new(None)),
            history_pending_path: None,
            history_pending_success: None,

            merge_preflight_result: Arc::new(std::sync::Mutex::new(None)),
            merge_preflight_pending: None,
            effect_reply: None,
            local_clipboard: clipboard_client::Client::new(),
            accounts_root: None,
            accounts_cache: Vec::new(),
            cloud: cloud_browse::CloudView::default(),
            #[cfg(test)]
            cloud_fixture_consent: false,
            mounted_cloud: mounted_cloud::MountedCloud::default(),
            drives: drives::Drives::default(),
            applications: applications::Applications::default(),
            host_intent: None,
            open_request: crate::open_with::request,
            document_candidates: crate::open_with::candidates,
            open_replies: Arc::default(),
            drop_targets: Rc::new(std::cell::RefCell::new(Vec::new())),
            item_rects: Rc::new(std::cell::RefCell::new(Vec::new())),
            marquee: None,
            scroll_offset: Rc::new(Cell::new((0.0, 0.0))),
            list_slot: crate::views::file_list::ListSlot::default(),
            widths_at_press: None,
            thumbs,
            recent: crate::recent::RecentFiles::default(),
            recent_record: None,
            open_with_cache: RefCell::new(HashMap::new()),
            entry_ids: RefCell::default(),
            entry_rows: RefCell::default(),
            app_chooser: ui::search::SearchOpener::new(),
            app_chooser_target: None,
            thumb_imgs: Rc::new(RefCell::new(HashMap::new())),
        };
        state.is_loading = true;
        state.read_shown_folder(cwd, Then::Nothing);
        Self {
            state: Rc::new(RefCell::new(state)),
            chrome: Chrome::new(nav),
            live: None,
            presented: None,
        }
    }
}

impl FilesState {
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::FileExplorer>,
        preferences: ui::framework::Setting<crate::resource::FilesPreferences>,
        recent: Option<ui::framework::PersonRecord<crate::recent::RecentRecord>>,
        device: Option<String>,
        ops_drain: Option<ui::framework::Projection>,
    ) {
        self.dispatcher = Some(dispatcher);
        self.preferences = Some(preferences);
        self.recent_record = recent;
        self.device = device;
        self.ops_drain = ops_drain;
        self.adopt_preferences();
    }

    fn queue(
        &mut self,
        op: crate::file_ops::FileOp,
        follow: OpFollowUp,
    ) -> Option<crate::file_ops::Outcome> {
        self.retire_stale_undo_notice();
        if let crate::file_ops::FileOp::CloudImport { job_id, .. } = &op {
            crate::cloud_jobs::register(job_id);
            self.cloud.job_id = Some(job_id.clone());
        }
        if let (Some(dispatcher), Some(device)) = (&self.dispatcher, &self.device)
            && let Some(queueing) = crate::resource::FileExplorer::queueing(device, op.clone())
        {
            dispatcher.submit(queueing);
            self.awaiting_ops.push((op, follow));
            return None;
        }
        let id = uuid::Uuid::new_v4().to_string();
        if matches!(op, crate::file_ops::FileOp::CloudImport { .. }) {
            let (tx, rx) = std::sync::mpsc::channel();
            // Issue and consume on the checked host thread, not in the worker.
            // An unhosted widget alone supplies no authority to read cloud bytes.
            let device = self.device.as_deref().unwrap_or("unhosted");
            let consent = crate::cloud_jobs::approve(device, &op, &id)
                .and_then(|()| crate::cloud_jobs::consume(device, &op, &id));
            #[cfg(test)]
            let consent = if self.cloud_fixture_consent {
                Ok(crate::cloud_jobs::fixture_consent(&op))
            } else {
                consent
            };
            self.cloud.transfer = Some((id, rx));
            std::thread::spawn(move || {
                let outcome = match consent {
                    Ok(consent) => crate::cloud_jobs::run(&op, consent),
                    Err(reason) => ui::tilo_resource::Settled::Failed {
                        reason,
                        partial: vec![],
                    },
                };
                let _ = tx.send(outcome);
            });
            return None;
        }
        let reverting = match &op {
            crate::file_ops::FileOp::Revert { op } => self
                .local_outcomes
                .get(op)
                .map(|outcome| outcome.result().as_slice()),
            _ => None,
        };
        let outcome = crate::file_ops::execute(&op, reverting);
        self.local_outcomes.insert(id, outcome.clone());
        self.settle_op(follow, &outcome);
        Some(outcome)
    }

    pub(crate) fn follow_ops(
        &mut self,
        ops: &ui::tilo_resource::Outbox<crate::resource::FileTask, Vec<crate::file_ops::Change>>,
    ) {
        let first = !self.ops_presented;
        self.ops_presented = true;
        let mut changed = false;
        let mut clipboard_status = None;
        for (id, entry) in ops.entries() {
            let Some(outcome) = &entry.settled else {
                continue;
            };
            if !self.settled_ops.insert(id.as_str().to_owned()) || first {
                continue;
            }
            if let Some(at) = self
                .awaiting_ops
                .iter()
                .position(|(op, _)| *op == entry.value.op)
            {
                let (op, follow) = self.awaiting_ops.remove(at);
                if matches!(op, crate::file_ops::FileOp::CloudImport { .. }) {
                    self.cloud_finished(outcome);
                }
                self.settle_op(follow, outcome);
                self.answer_op(&op, outcome);
            } else {
                for change in outcome.result() {
                    match change {
                        crate::file_ops::Change::ClipboardUndo { .. } => {
                            clipboard_status = Some("files-clipboard-undone");
                        }
                        crate::file_ops::Change::ClipboardTransfer { .. } => {
                            clipboard_status = Some("files-clipboard-redone");
                        }
                        _ => {}
                    }
                }
                changed = true;
            }
        }
        if changed {
            self.refresh();
            self.structure_dirty = true;
        }
        if let Some(key) = clipboard_status {
            self.status = Some(t(key));
        }
    }

    fn settle_op(&mut self, follow: OpFollowUp, outcome: &crate::file_ops::Outcome) {
        use crate::file_ops::Change;
        self.refresh();
        self.structure_dirty = true;
        let error = outcome.reason().map(str::to_owned);
        match follow {
            OpFollowUp::NewFolder => match error {
                None => {
                    let created = outcome.result().iter().find_map(|change| match change {
                        Change::Created { path } => Some(PathBuf::from(path)),
                        _ => None,
                    });
                    if let Some(i) = created.and_then(|path| {
                        self.displayed_entries().iter().position(|e| e.path == path)
                    }) {
                        self.selection.select_only(i);
                        self.retain_shown_selection();
                        self.begin_rename();
                    }
                }
                Some(error) => {
                    self.notify_failure(t("files-new-folder-failed-title"), &error, None);
                }
            },
            OpFollowUp::Rename => match error {
                None => {
                    for change in outcome.result() {
                        if let Change::Moved { from, to } = change {
                            self.recent_moved(Path::new(from), Path::new(to));
                        }
                    }
                    if self.renaming.is_some() {
                        self.end_rename_editor();
                    }
                }
                Some(error) => {
                    self.notify_failure(t("files-rename-failed-title"), &error, None);
                }
            },
            OpFollowUp::Trash { count } => match error {
                None => {
                    let undoable = outcome
                        .result()
                        .iter()
                        .all(|change| matches!(change, Change::Trashed { entry: Some(_), .. }));
                    self.notify_done(
                        t_args("files-moved-to-trash", &[arg("count", i64_of_usize(count))]),
                        undoable,
                    );
                }
                Some(error) => {
                    self.notify_failure(t("files-trash-failed"), &error, None);
                }
            },
            OpFollowUp::Move { destination } => {
                let moved = outcome
                    .result()
                    .iter()
                    .filter(|change| matches!(change, Change::Moved { .. }))
                    .count();
                match error {
                    None => self.notify_done(
                        t_args(
                            "files-moved-to-folder",
                            &[
                                arg("count", i64_of_usize(moved)),
                                arg("destination", destination),
                            ],
                        ),
                        true,
                    ),
                    Some(error) => self.notify_failure(
                        t_args("files-move-failed", &[arg("destination", destination)]),
                        &error,
                        None,
                    ),
                }
            }
            OpFollowUp::Paste { count, cut } => self.settle_paste(count, cut, outcome),
            OpFollowUp::Extract { name } => match error {
                None => self.select_written(outcome),
                Some(error) => self.notify_failure(
                    t_args("files-archive-extract-failed-named", &[arg("name", name)]),
                    &error,
                    None,
                ),
            },
            OpFollowUp::Compress => match error {
                None => self.select_written(outcome),
                Some(error) => {
                    self.notify_failure(t("files-archive-create-failed-title"), &error, None);
                }
            },
            OpFollowUp::Replaced => {
                if let Some(error) = error {
                    self.notify_failure(t("files-trash-failed"), &error, None);
                }
            }
        }
    }

    fn select_written(&mut self, outcome: &crate::file_ops::Outcome) {
        let written = outcome.result().iter().find_map(|change| match change {
            crate::file_ops::Change::Wrote { path } => Some(PathBuf::from(path)),
            _ => None,
        });
        if let Some(index) =
            written.and_then(|path| self.displayed_entries().iter().position(|e| e.path == path))
        {
            self.selection.select_only(index);
            self.retain_shown_selection();
        }
    }

    fn notify_done(&mut self, title: String, undoable: bool) {
        self.status = None;
        let undo = undoable && self.undo.can_undo();
        self.notice
            .show(title, "", undo.then_some(NoticeAction::Undo));
        self.structure_dirty = true;
    }

    fn notify_failure(&mut self, title: String, reason: &str, action: Option<NoticeAction>) {
        if reason == crate::file_ops::CANCELLED {
            self.notify_done(t("files-transfer-stopped"), true);
            return;
        }
        self.status = None;
        self.notice
            .show(title, crate::fileops::readable_error(reason), action);
        self.structure_dirty = true;
    }

    fn retire_stale_undo_notice(&mut self) {
        if self.notice.offers_undo() {
            self.notice.dismiss();
            self.structure_dirty = true;
        }
    }

    fn act_on_notice(&mut self, action: NoticeAction) {
        match action {
            NoticeAction::Undo => {
                self.undo.request(ui::UndoRequest::Undo);
            }
            NoticeAction::CancelTransfer => crate::file_ops::cancel_transfers(),
            NoticeAction::OpenWith(path) => {
                if let Some(i) = self.displayed_entries().iter().position(|e| e.path == path) {
                    self.selection.select_only(i);
                    self.retain_shown_selection();
                    self.open_app_chooser();
                }
            }
            NoticeAction::OpenHistory(path) => {
                if let Some(i) = self.displayed_entries().iter().position(|e| e.path == path) {
                    self.selection.select_only(i);
                    self.retain_shown_selection();
                    self.apply_command(FileCommand::ShowHistory);
                }
            }
        }
        self.structure_dirty = true;
    }

    fn settle_paste(&mut self, count: usize, cut: bool, outcome: &crate::file_ops::Outcome) {
        use crate::file_ops::Change;
        let error = outcome.reason();
        let done: Vec<&str> = outcome
            .result()
            .iter()
            .filter_map(|change| match change {
                Change::Moved { from, .. } | Change::Copied { from, .. } => Some(from.as_str()),
                _ => None,
            })
            .collect();
        if cut {
            self.clipboard
                .retain(|path| !done.iter().any(|from| path.as_path() == Path::new(from)));
        }
        if let Some(error) = error {
            let title = t_args(
                "files-paste-failed",
                &[
                    arg("count", i64_of_usize(count - done.len())),
                    arg("total", i64_of_usize(count)),
                ],
            );
            self.notify_failure(title, error, None);
        }
    }

    pub(crate) fn selection_named(&self) -> Option<(usize, String)> {
        let selected = self.selected_paths();
        let first = selected.first()?;
        let name = first.file_name().map_or_else(
            || first.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        Some((selected.len(), name))
    }

    pub(crate) fn selection_label(key: &str, (count, name): &(usize, String)) -> String {
        crate::i18n::t_args(
            key,
            &[
                crate::i18n::arg("count", *count),
                crate::i18n::arg("name", name.as_str()),
            ],
        )
    }

    pub(crate) fn selection_declarations(
        &self,
    ) -> Vec<ui::tilo_resource::ActionDecl<crate::resource::FileExplorer>> {
        use crate::resource::{FileExplorer, file_explorer};
        const SELECTION: &str = ui::tilo_actions::ContextRequirement::Selection.tag();
        let Some(device) = self.device.clone() else {
            return Vec::new();
        };
        if self.is_trash_view() {
            return Vec::new();
        }
        let Some((count, name)) = self.selection_named() else {
            return Vec::new();
        };
        let paths: Vec<String> = self
            .selected_paths()
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        let mut declarations = Vec::new();
        if let [path] = paths.as_slice() {
            declarations.extend(
                FileExplorer::RENAME
                    .bind(file_explorer::Rename::paths().device, device.clone())
                    .and(file_explorer::Rename::paths().folder, String::new())
                    .and(file_explorer::Rename::paths().entry, path.clone())
                    .declaration(
                        "rename-selected",
                        word!("files-folder-rename-selected", name = name.clone()),
                    ),
            );
        }
        declarations.extend(
            FileExplorer::TRASH
                .bind(file_explorer::Trash::paths().device, device)
                .and(file_explorer::Trash::paths().folder, String::new())
                .and(file_explorer::Trash::paths().entries, paths)
                .declaration(
                    "trash-selected",
                    word!("files-folder-trash-selected", name = name, count = count),
                )
                .map(|mut trash| {
                    trash.destructive = true;
                    trash
                }),
        );
        declarations
            .into_iter()
            .map(|decl| decl.tagged(SELECTION))
            .collect()
    }

    pub(crate) fn entry_declarations(
        &self,
        row: &str,
    ) -> Vec<ui::tilo_resource::ActionDecl<crate::resource::FileExplorer>> {
        use crate::resource::{FileExplorer, file_explorer};
        let Some(device) = self.device.clone() else {
            return Vec::new();
        };
        if self.is_trash_view() {
            return Vec::new();
        }
        let Some(entry) = self.entry_by_resource_id(row) else {
            return Vec::new();
        };
        let path = entry.path.display().to_string();
        let rename = FileExplorer::RENAME
            .bind(file_explorer::Rename::paths().device, device.clone())
            .and(file_explorer::Rename::paths().folder, String::new())
            .and(file_explorer::Rename::paths().entry, path.clone())
            .declaration("rename", word!("files-menu-rename"));
        let move_to = FileExplorer::MOVE_ENTRIES
            .bind(file_explorer::MoveEntries::paths().device, device.clone())
            .and(file_explorer::MoveEntries::paths().from, String::new())
            .and(
                file_explorer::MoveEntries::paths().entries,
                vec![path.clone()],
            )
            .declaration("move-to", word!("files-folder-move-to"));
        let trash = FileExplorer::TRASH
            .bind(file_explorer::Trash::paths().device, device)
            .and(file_explorer::Trash::paths().folder, String::new())
            .and(file_explorer::Trash::paths().entries, vec![path])
            .declaration("trash", word!("files-menu-move-trash"))
            .map(|mut trash| {
                trash.destructive = true;
                trash
            });
        [rename, move_to, trash].into_iter().flatten().collect()
    }

    pub(crate) fn file_op_declarations(
        &self,
    ) -> Vec<ui::tilo_resource::ActionDecl<crate::resource::FileExplorer>> {
        use crate::resource::{FileExplorer, file_explorer};
        let Some(device) = self.device.clone() else {
            return Vec::new();
        };
        if self.is_trash_view() || !self.shown_is_folder {
            return Vec::new();
        }
        let Ok(here) = ui::tilo_resource::AbsolutePath::new(&self.current_path) else {
            return Vec::new();
        };
        let folder = here.to_string_lossy();
        let names: Vec<serde_json::Value> = self
            .entries
            .iter()
            .flat_map(|entry| {
                [
                    serde_json::Value::String(entry.name.clone()),
                    serde_json::Value::String(entry.path.display().to_string()),
                ]
            })
            .collect();
        // An entry the folder does not hold is refused at the door, so a
        // misspelt name changes nothing.
        let shown = |mut decl: ui::tilo_resource::ActionDecl<FileExplorer>| {
            for param in &mut decl.params {
                let schema = match &*param.name {
                    "entry" => serde_json::json!({ "enum": names }),
                    "entries" => serde_json::json!({
                        "items": { "enum": names },
                        "minItems": 1,
                        "maxItems": crate::resource::MAX_MOVE_ENTRIES,
                    }),
                    _ => continue,
                };
                param.schema = Some(std::sync::Arc::new(schema));
            }
            decl
        };
        let new_folder = FileExplorer::NEW_FOLDER
            .bind(file_explorer::NewFolder::paths().device, device.clone())
            .and(file_explorer::NewFolder::paths().parent, here.clone())
            .declaration("new-folder-here", word!("files-menu-new-folder"));
        let rename = FileExplorer::RENAME
            .bind(file_explorer::Rename::paths().device, device.clone())
            .and(file_explorer::Rename::paths().folder, folder.clone())
            .declaration("rename-here", word!("files-folder-rename-here"));
        let trash = FileExplorer::TRASH
            .bind(file_explorer::Trash::paths().device, device.clone())
            .and(file_explorer::Trash::paths().folder, folder.clone())
            .declaration("trash-here", word!("files-folder-trash-here"));
        let moving = FileExplorer::MOVE_ENTRIES
            .bind(file_explorer::MoveEntries::paths().device, device.clone())
            .and(file_explorer::MoveEntries::paths().from, folder.clone())
            .declaration("move-here", word!("files-folder-move-here"));
        let copying = FileExplorer::COPY_INTO
            .bind(file_explorer::CopyInto::paths().device, device.clone())
            .and(file_explorer::CopyInto::paths().from, folder)
            .declaration("copy-here", word!("files-folder-copy-here"));
        let writing = FileExplorer::WRITE_FILE
            .bind(file_explorer::WriteFile::paths().device, device.clone())
            .and(file_explorer::WriteFile::paths().folder, here.clone())
            .declaration("write-file-here", word!("files-folder-write-file-here"));
        let compressing = FileExplorer::COMPRESS
            .bind(file_explorer::Compress::paths().device, device.clone())
            .and(file_explorer::Compress::paths().from, here.clone())
            .declaration("compress-here", word!("files-folder-compress-here"));
        let extracting = FileExplorer::EXTRACT
            .bind(file_explorer::Extract::paths().device, device)
            .and(file_explorer::Extract::paths().to, Some(here))
            .declaration("extract-here", word!("files-folder-extract-here"));
        let for_agents = |decl: ui::tilo_resource::ActionDecl<FileExplorer>| {
            decl.for_audience(ui::tilo_actions::Audience::Agents)
        };
        let by_entry = [
            rename,
            trash,
            moving,
            copying,
            writing,
            compressing,
            extracting,
        ]
        .into_iter()
        .flatten()
        .map(for_agents);
        new_folder.into_iter().chain(by_entry).map(shown).collect()
    }

    pub(crate) fn present_places(&mut self, places: &[String]) {
        let favorites = resolve_places(places, self.home_path.as_deref(), &self.user_dirs);
        if self.favs.borrow().favorites == favorites {
            return;
        }
        self.favs.borrow_mut().favorites = favorites;
        self.refresh_places_search();
        self.structure_dirty = true;
    }

    fn place_for(&self, path: &Path) -> String {
        crate::places::place_of(path, self.home_path.as_deref(), &self.user_dirs)
    }

    fn submit(&mut self, invoke: ui::tilo_resource::Invoke<crate::resource::FileExplorer>) {
        self.retire_stale_undo_notice();
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(invoke);
        }
    }

    fn current_preferences(&self) -> crate::resource::FilesPreferences {
        crate::resource::FilesPreferences {
            view: crate::resource::view_mode_token(self.view_mode).to_owned(),
            sort: crate::resource::sort_key_token(self.sort_by).to_owned(),
            order: crate::resource::sort_order_token(self.sort_order).to_owned(),
            zoom: self.zoom_level,
            hidden: self.show_hidden,
            widths: self.list_slot.widths(),
        }
    }

    fn store_widths(&self) {
        let widths = self.list_slot.widths();
        let stored = self.preferences.as_ref().map(ui::framework::Setting::get);
        if stored.as_ref().is_some_and(|prefs| prefs.widths == widths) {
            return;
        }
        if let Some(preferences) = &self.preferences
            && let Err(error) = preferences.update(|prefs| prefs.widths = widths)
        {
            tracing::warn!(%error, "file-explorer: column widths not stored");
        }
    }

    fn store_preferences(&mut self) {
        let current = self.current_preferences();
        if self.shown_preferences.as_ref() == Some(&current) {
            return;
        }
        if let Some(preferences) = &self.preferences
            && let Err(error) = preferences.set(current.clone())
        {
            tracing::warn!(%error, "file-explorer: view preferences not stored");
        }
        self.shown_preferences = Some(current);
    }

    fn adopt_preferences(&mut self) {
        let Some(stored) = self.preferences.as_ref().map(ui::framework::Setting::get) else {
            return;
        };
        if self.shown_preferences.as_ref() == Some(&stored) {
            return;
        }
        self.shown_preferences = Some(stored.clone());
        self.list_slot.set_widths(&stored.widths);
        self.view_mode = crate::resource::parse_view_mode(&stored.view);
        let sort_by = crate::resource::parse_sort_key(&stored.sort);
        let sort_order = crate::resource::parse_sort_order(&stored.order);
        if (sort_by, sort_order) != (self.sort_by, self.sort_order) {
            self.sort_by = sort_by;
            self.sort_order = sort_order;
            sort_entries(self.sort_by, self.sort_order, &mut self.entries);
        }
        self.zoom_level = config::clamp_zoom(stored.zoom);
        if self.show_hidden != stored.hidden {
            self.show_hidden = stored.hidden;
            self.refresh();
        }
        self.structure_dirty = true;
    }

    pub fn set_external_status(&mut self, status: impl AsRef<str>) {
        const MAX_STATUS_CHARS: usize = 512;
        self.status = Some(status.as_ref().chars().take(MAX_STATUS_CHARS).collect());
        self.structure_dirty = true;
    }

    #[doc(hidden)]
    pub fn debug_show_remote_account(
        &mut self,
        account: &crate::remote_accounts::RemoteAccount,
        reauthenticate: bool,
    ) {
        self.startup_accounts = Some(vec![account.clone()]);
        self.places_shown = None;
        self.show_remote_account_panel(account, reauthenticate);
    }

    fn show_remote_account_panel(
        &mut self,
        account: &crate::remote_accounts::RemoteAccount,
        reauthenticate: bool,
    ) {
        self.leave_cloud_browse();
        let service = account.kind.label().to_string();
        let (title, lines, action) = if reauthenticate {
            (
                t_args(
                    "files-account-sign-in-title",
                    &[arg("account", account.label.clone())],
                ),
                vec![
                    t_args(
                        "files-account-sign-in-needed",
                        &[arg("service", service), arg("host", account.host.clone())],
                    ),
                    t("files-account-sign-in-where"),
                ],
                Some(CollabPanelAction::RecheckAccount(
                    account.id.as_str().to_owned(),
                )),
            )
        } else {
            let mut lines = vec![
                t_args(
                    "files-account-service-at-host",
                    &[arg("service", service), arg("host", account.host.clone())],
                ),
                account.health.unavailable_reason().to_string(),
            ];
            if let Some(mount) = &account.mount {
                lines.push(t_args(
                    "files-account-folder",
                    &[arg("path", mount.display().to_string())],
                ));
            }
            (account.label.clone(), lines, None)
        };
        self.show_collab_panel(CollabPanel {
            title,
            summary: String::new(),
            lines,
            action,
        });
    }

    fn show_remote_account(&mut self, id: &str) -> bool {
        self.refresh_external_search_catalogs();
        let Some(account) = self.remote_account(id) else {
            return false;
        };
        let reauthenticate =
            account.health == crate::remote_accounts::RemoteAccountHealth::NeedsAuthentication;
        if account.is_import() {
            if let Err(error) = self.browse_cloud(id.to_owned(), None, 0) {
                self.set_external_status(error);
            }
            return true;
        }
        match (&account.mount, remote_folder_availability(&account)) {
            (Some(mount), Availability::Available) => {
                self.close_collab_panel();
                self.navigate(mount.clone());
            }
            _ => self.show_remote_account_panel(&account, reauthenticate),
        }
        true
    }

    fn thumb_ctx(&self) -> Option<ThumbCtx> {
        Some(ThumbCtx {
            service: self.thumbs.clone()?,
            imgs: self.thumb_imgs.clone(),
        })
    }

    fn zoom_scale(&self) -> f64 {
        config::zoom_scale(self.zoom_level)
    }

    fn refresh_places_search(&mut self) {
        self.places_search_revision = self.places_search_revision.wrapping_add(1);
        let favorites = self.favs.borrow().favorites.clone();
        self.places_search
            .refresh(self.places_search_revision, &favorites);
    }

    fn advance_collaborative_search_page(&self) -> crate::model::WorkspaceRecordIndexPage {
        let page = self.collaborative_record_pager.borrow_mut().next_page();
        let revision = self.collaborative_search_revision.get();
        if page.reset {
            let next = revision.wrapping_add(1);
            self.collaborative_search_revision.set(next);
            self.collaborative_search.refresh(next, &page.records);
        } else {
            self.collaborative_search
                .append_page(revision, &page.records);
        }
        page
    }

    fn remote_account_snapshot(&self) -> Vec<crate::remote_accounts::RemoteAccount> {
        self.startup_accounts
            .clone()
            .unwrap_or_else(|| self.accounts_cache.clone())
    }

    fn remote_account(&self, id: &str) -> Option<crate::remote_accounts::RemoteAccount> {
        self.remote_account_snapshot()
            .into_iter()
            .find(|account| account.id.as_str() == id)
    }

    fn refresh_external_search_catalogs(&mut self) {
        let page = self.advance_collaborative_search_page();
        if page.truncated {
            tracing::debug!("collaborative search catalogue truncated; continues on refresh");
        }
        if let Some(root) = self.accounts_root.as_deref() {
            self.accounts_cache = crate::remote_accounts::load_in(root, self.home_path.as_deref());
        }
        self.remote_accounts_search_revision = self.remote_accounts_search_revision.wrapping_add(1);
        let accounts = self.remote_account_snapshot();
        self.remote_accounts_search
            .refresh(self.remote_accounts_search_revision, &accounts);
    }

    pub fn displayed_entries(&self) -> Vec<Entry> {
        let find = self.find.trim().to_lowercase();
        if find.is_empty() {
            return self.entries.clone();
        }
        self.entries
            .iter()
            .filter(|entry| entry.name.to_lowercase().contains(&find))
            .cloned()
            .collect()
    }

    fn find_count(&self) -> ui::widget::FilterCount {
        let shown = self.displayed_entries().len();
        ui::widget::FilterCount::new(shown, self.entries.len())
    }

    fn entry_resource_id(entry: &Entry) -> String {
        entry.record_id.map_or_else(
            || crate::search_provider::entry_resource_id(&entry.path),
            |id| crate::search_provider::record_resource_id(uuid::Uuid::from_bytes(id)),
        )
    }

    fn entry_by_resource_id(&self, id: &str) -> Option<&Entry> {
        self.entry_ids.borrow_mut().find(&self.entries, id)
    }

    fn selected_entry_identities(&self) -> Vec<(String, String)> {
        if self.selection.selected().len() != 1 {
            return Vec::new();
        }
        let displayed = self.displayed_entries();
        let Some(entry) = self
            .selection
            .primary()
            .and_then(|index| displayed.get(index))
        else {
            return Vec::new();
        };
        let mut identities = Vec::with_capacity(2);
        if let Some(record) = entry.record_id {
            identities.push((
                "files.record".to_string(),
                crate::search_provider::record_resource_id(uuid::Uuid::from_bytes(record)),
            ));
        }
        identities.push((
            "files.entry".to_string(),
            crate::search_provider::entry_resource_id(&entry.path),
        ));
        identities
    }

    fn visible_entry_path(&self, resource_id: &str) -> Option<PathBuf> {
        self.entry_by_resource_id(resource_id)
            .map(|entry| entry.path.clone())
    }

    fn recent_entry_path(&self, resource_id: &str) -> Result<PathBuf, String> {
        if !self.is_recent_view() {
            return Err(t("files-recent-action-unavailable"));
        }
        self.visible_entry_path(resource_id)
            .filter(|path| path.exists())
            .ok_or_else(|| t("files-recent-action-unavailable"))
    }

    pub(crate) fn run_effect(
        &mut self,
        effect: crate::effects::FilesEffect,
    ) -> Result<Ran, String> {
        self.run_effect_for(effect, None)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one match over the effect vocabulary keeps every caller's door in one auditable place"
    )]
    pub(crate) fn run_effect_for(
        &mut self,
        effect: crate::effects::FilesEffect,
        invocation: Option<tilo_actions::InvocationId>,
    ) -> Result<Ran, String> {
        use crate::effects::FilesEffect;
        self.answering_op = None;
        let answer = match effect {
            FilesEffect::ClipboardStatus => return self.snapshot_clipboard(invocation),
            FilesEffect::RecoverClipboardTransfers => return self.recover_clipboard(invocation),
            FilesEffect::Refresh => {
                self.refresh();
                None
            }
            FilesEffect::SelectEntries { entries } => {
                self.select_published_entries(&entries)?;
                Some(self.selection_answer())
            }
            FilesEffect::SelectAll => {
                self.apply_command(FileCommand::SelectAll);
                self.last_click = None;
                Some(self.selection_answer())
            }
            FilesEffect::SelectedEntries => Some(self.selected_entries_answer()),
            FilesEffect::ReadText { file, from } => Some(self.read_text_answer(&file, from)?),
            FilesEffect::HandOver { file } => {
                let hash = self.hand_over(&file)?;
                Some(serde_json::json!({ "content": tilo_actions::content_resource(&hash) }))
            }
            FilesEffect::ClearSelection => {
                self.selection.clear();
                self.retain_shown_selection();
                self.last_click = None;
                Some(self.selection_answer())
            }
            FilesEffect::CopySelection | FilesEffect::CutSelection => {
                if matches!(self.services, FileServices::Hosted(_)) || self.hosted_clipboard() {
                    return self.publish_clipboard(
                        invocation,
                        matches!(effect, FilesEffect::CutSelection),
                    );
                }
                if self.selected_paths().is_empty() {
                    return Err(t("files-selection-empty"));
                }
                self.apply_command(if matches!(effect, FilesEffect::CutSelection) {
                    FileCommand::Cut
                } else {
                    FileCommand::Copy
                });
                None
            }
            FilesEffect::Paste { conflict } => {
                if matches!(self.services, FileServices::Hosted(_)) || self.hosted_clipboard() {
                    return self.paste_clipboard(invocation, paste_conflict(conflict.as_deref())?);
                }
                let queued = self.paste()?;
                return Ok(self.answer_when_settled(queued));
            }
            FilesEffect::GoTo { path } | FilesEffect::OpenFolder { path } => {
                let path = path.into_path_buf();
                if !path.is_dir() {
                    return Err(crate::i18n::CATALOG.t_args(
                        "files-not-a-folder",
                        &[("path", ui::i18n::ArgValue::Text(path.display().to_string()))],
                    ));
                }
                self.navigate(path);
                Some(self.folder_answer())
            }
            FilesEffect::OpenTrash => {
                let path = fileops::trash_dir().ok_or_else(|| t("files-trash-unavailable"))?;
                self.navigate(path);
                if let Some(error) = &self.error {
                    return Err(error.clone());
                }
                if self.hosted_clipboard() && host_fs::trash::user_trash_dir().is_none() {
                    self.status = Some(t("files-user-trash-unavailable"));
                }
                Some(self.folder_answer())
            }
            FilesEffect::RemoveFavorite { path } => {
                self.apply_command(FileCommand::RemoveFavorite(path.into()));
                None
            }
            FilesEffect::MoveFavorite {
                source,
                target,
                before,
            } => {
                let moved = crate::favorites::move_for(
                    &self.favs.borrow().favorites,
                    source.as_path(),
                    target.as_path(),
                    before,
                );
                if let Some((from, to)) = moved {
                    self.apply_command(FileCommand::ReorderFavorite { from, to });
                }
                None
            }
            FilesEffect::Up => {
                self.go_up();
                None
            }
            FilesEffect::Back => {
                self.history_back();
                None
            }
            FilesEffect::Forward => {
                self.history_forward();
                None
            }
            FilesEffect::Recent => {
                self.navigate(crate::recent::location());
                None
            }
            FilesEffect::ToggleHidden => {
                self.apply_command(FileCommand::ToggleHidden);
                None
            }
            FilesEffect::List => {
                self.set_view_mode(ViewMode::List);
                None
            }
            FilesEffect::Grid => {
                self.set_view_mode(ViewMode::Grid);
                None
            }
            FilesEffect::SortName => {
                self.set_sort_by(SortBy::Name);
                None
            }
            FilesEffect::SortSize => {
                self.set_sort_by(SortBy::Size);
                None
            }
            FilesEffect::SortType => {
                self.set_sort_by(SortBy::Type);
                None
            }
            FilesEffect::SortModified => {
                self.set_sort_by(SortBy::Modified);
                None
            }
            FilesEffect::SortAscending => {
                self.apply_set_sort_order(SortOrder::Ascending);
                None
            }
            FilesEffect::SortDescending => {
                self.apply_set_sort_order(SortOrder::Descending);
                None
            }
            FilesEffect::ZoomIn => {
                self.set_zoom(self.zoom_level.saturating_add(1));
                None
            }
            FilesEffect::ZoomOut => {
                self.set_zoom(self.zoom_level.saturating_sub(1));
                None
            }
            FilesEffect::KeyboardShortcuts => {
                self.show_keyboard_shortcuts_panel();
                None
            }
            FilesEffect::OpenTerminal => {
                self.open_terminal().map_err(|e| {
                    crate::i18n::CATALOG.t_args(
                        "files-terminal-failed",
                        &[("detail", ui::i18n::ArgValue::Text(e))],
                    )
                })?;
                None
            }
            FilesEffect::EmptyTrash => {
                if !self.is_trash_view() {
                    return Err("Empty Trash from the Trash folder".into());
                }
                if self.has_native_trash_entries() {
                    return Err(t("files-native-trash-delete-unavailable"));
                }
                self.perform_empty_trash();
                None
            }
            FilesEffect::OpenItem { item } => {
                let target = self
                    .visible_open_path(&item)
                    .ok_or_else(|| crate::i18n::CATALOG.t("files-location-gone"))?;
                self.open_path(target);
                None
            }
            FilesEffect::ReadEntry { entry } => {
                let entry = self
                    .entry_by_resource_id(&entry)
                    .ok_or_else(|| crate::i18n::CATALOG.t("files-location-gone"))?;
                Some(entry_answer(entry))
            }
            FilesEffect::Reveal { record } => {
                self.reveal_record(&record)?;
                None
            }
            FilesEffect::RevealRecent { entry } => {
                let path = self.recent_entry_path(&entry)?;
                self.apply_command(FileCommand::RevealInFolder(path));
                None
            }
            FilesEffect::ForgetRecent { entry } => {
                let path = self.recent_entry_path(&entry)?;
                self.apply_command(FileCommand::ForgetRecent(path));
                None
            }
            FilesEffect::NewFromTemplate { template } => {
                let target = self
                    .available_templates()
                    .into_iter()
                    .find(|candidate| {
                        template_resource_id(&candidate.to_string_lossy()) == template
                    })
                    .ok_or_else(|| crate::i18n::CATALOG.t("files-template-gone"))?;
                self.create_from_template(&target)?;
                None
            }
            FilesEffect::OpenWith { entry, app } => {
                self.invoke_open_with(&entry, &app)?;
                None
            }
            effect @ (FilesEffect::BrowseCloud { .. }
            | FilesEffect::MoreCloud
            | FilesEffect::SelectCloud { .. }
            | FilesEffect::ImportCloud { .. }
            | FilesEffect::CloudProgress
            | FilesEffect::CancelCloud
            | FilesEffect::RetryCloud
            | FilesEffect::RevealCloud) => self.cloud_effect(effect)?,
            FilesEffect::ImportConverted { entry } => {
                self.choose_mounted_import(entry)?;
                None
            }
            FilesEffect::ImportOnce { entry, folder } => {
                let queued = self.import_once(&entry, &folder)?;
                return Ok(self.answer_when_settled(queued));
            }
            FilesEffect::CopyTo { entry, folder } => {
                let queued = self.copy_into_folder(&entry, &folder)?;
                return Ok(self.answer_when_settled(queued));
            }
            FilesEffect::Copy { entry } => {
                if matches!(self.services, FileServices::Hosted(_)) || self.hosted_clipboard() {
                    let target = self
                        .visible_entry_path(&entry)
                        .ok_or_else(|| t("files-clipboard-source-unavailable"))?;
                    return self.publish_clipboard_row(invocation, false, &target);
                }
                self.entry_command(&entry, |_, _| Some(FileCommand::Copy))?
            }
            FilesEffect::Cut { entry } => {
                if matches!(self.services, FileServices::Hosted(_)) || self.hosted_clipboard() {
                    let target = self
                        .visible_entry_path(&entry)
                        .ok_or_else(|| t("files-clipboard-source-unavailable"))?;
                    return self.publish_clipboard_row(invocation, true, &target);
                }
                self.entry_command(&entry, |_, _| Some(FileCommand::Cut))?
            }
            FilesEffect::Restore { entry } => {
                let target = self
                    .visible_entry_path(&entry)
                    .ok_or_else(|| t("files-clipboard-source-unavailable"))?;
                if self.is_native_trash_entry(&target) {
                    return self.restore_clipboard_entry(invocation, &target);
                }
                self.entry_command(&entry, |files, _| {
                    files
                        .is_trash_view()
                        .then_some(FileCommand::RestoreFromTrash)
                })?
            }
            FilesEffect::DeletePermanently { entry } => {
                let target = self
                    .visible_entry_path(&entry)
                    .ok_or_else(|| t("files-clipboard-source-unavailable"))?;
                if self.is_native_trash_entry(&target) {
                    return Err(t("files-native-trash-delete-unavailable"));
                }
                self.entry_command(&entry, |files, target| {
                    files
                        .is_trash_view()
                        .then(|| FileCommand::ConfirmPermanentDelete {
                            paths: vec![target.to_path_buf()],
                        })
                })?
            }
            FilesEffect::Properties { entry } => {
                self.entry_command(&entry, |_, _| Some(FileCommand::ShowProperties))?
            }
            FilesEffect::History { entry } => {
                self.entry_command(&entry, |_, _| Some(FileCommand::ShowHistory))?
            }
            FilesEffect::Availability { entry } => {
                self.entry_command(&entry, |_, _| Some(FileCommand::ShowAvailability))?
            }
            FilesEffect::KeepOffline { entry } => {
                self.entry_command(&entry, |_, _| Some(FileCommand::KeepOffline))?
            }
            FilesEffect::AutomaticStorage { entry } => {
                self.entry_command(&entry, |_, _| Some(FileCommand::RemoveOfflinePin))?
            }
            FilesEffect::FreeLocalSpace { entry } => {
                self.entry_command(&entry, |_, _| Some(FileCommand::FreeLocalSpace))?
            }
            FilesEffect::RetryDownload { entry } => {
                self.entry_command(&entry, |_, _| Some(FileCommand::RetryTransfer))?
            }
            FilesEffect::SendNearby { entry } => {
                self.entry_command(&entry, |_, _| Some(FileCommand::OpenNearbySend))?
            }
            FilesEffect::SendTo { entry, target } => {
                self.refuse_while_answering()?;
                let path = self
                    .visible_entry_path(&entry)
                    .ok_or_else(|| crate::i18n::CATALOG.t("files-entry-gone"))?;
                if !self
                    .nearby_send
                    .targets()
                    .iter()
                    .any(|candidate| candidate.handle == target && candidate.online)
                {
                    return Err("nearby device is no longer available".into());
                }
                self.nearby_send.send(&path, target)?;
                self.nearby_send_path = Some(path);
                self.status = Some(t("files-nearby-queueing"));
                self.structure_dirty = true;
                return Ok(Ran::Pending);
            }
            FilesEffect::SendAirDrop {
                entry,
                address,
                port,
            } => {
                self.refuse_while_answering()?;
                let path = self
                    .visible_entry_path(&entry)
                    .ok_or_else(|| t("files-airdrop-source-unavailable"))?;
                if !path.is_file() {
                    return Err(t("files-airdrop-file-only"));
                }
                self.nearby_send.send_airdrop(&path, address, port)?;
                self.status = Some(t("files-airdrop-verifying"));
                self.structure_dirty = true;
                return Ok(Ran::Pending);
            }
            FilesEffect::Share { entry } => {
                self.refuse_while_answering()?;
                let target = self
                    .visible_entry_path(&entry)
                    .ok_or_else(|| crate::i18n::CATALOG.t("files-entry-gone"))?;
                self.select_entry(&target)?;
                if !self.apply_command(FileCommand::Share) || !self.share_invite.asking() {
                    return Err(self
                        .status
                        .clone()
                        .unwrap_or_else(|| crate::i18n::CATALOG.t("files-sharing-unavailable")));
                }
                self.structure_dirty = true;
                return Ok(Ran::Pending);
            }
            FilesEffect::AcceptNearby {
                transfer,
                destination,
            } => {
                self.decide_nearby(transfer, Some(destination))?;
                None
            }
            FilesEffect::DeclineNearby { transfer } => {
                self.decide_nearby(transfer, None)?;
                None
            }
            FilesEffect::InspectNearby { transfer } => {
                let status = self
                    .nearby_send
                    .transfers()
                    .iter()
                    .find(|candidate| candidate.handle == transfer)
                    .ok_or_else(|| t("files-nearby-transfer-unavailable"))?;
                Some(serde_json::json!({"transfer": status}))
            }
            FilesEffect::RetryNearby { transfer } => {
                if !self.nearby_send.transfers().iter().any(|candidate| {
                    candidate.handle == transfer
                        && candidate.phase == ui::collaboration::NearbyTransferPhase::Failed
                }) {
                    return Err("nearby transfer is not retryable".into());
                }
                self.nearby_send.retry(transfer);
                self.status = Some(t("files-nearby-retrying"));
                None
            }
            FilesEffect::CancelNearby { transfer } => {
                if !self.nearby_send.transfers().iter().any(|candidate| {
                    candidate.handle == transfer
                        && matches!(
                            candidate.phase,
                            ui::collaboration::NearbyTransferPhase::Queued
                                | ui::collaboration::NearbyTransferPhase::Transferring
                        )
                }) {
                    return Err("nearby transfer is not cancellable".into());
                }
                self.nearby_send.cancel(transfer);
                self.status = Some(t("files-nearby-cancelled"));
                None
            }
            FilesEffect::OpenAccount { account } => {
                let account = self.account_named(&account)?;
                if account.is_import() {
                    self.browse_cloud(account.id.to_string(), None, 0)?;
                    return Ok(Ran::Done(None));
                }
                match (&account.mount, remote_folder_availability(&account)) {
                    (Some(mount), Availability::Available) => self.open_path(mount.clone()),
                    _ => self.show_remote_account_panel(&account, false),
                }
                None
            }
            FilesEffect::AccountDetails { account } => {
                let account = self.account_named(&account)?;
                self.show_remote_account_panel(&account, false);
                None
            }
            FilesEffect::OpenDrive { drive } => {
                self.open_drive(&drive)?;
                None
            }
            FilesEffect::UninstallApp { app } => {
                let path = self
                    .visible_entry_path(&app)
                    .filter(|path| crate::applications::is_app(path))
                    .ok_or_else(|| t("files-applications-gone"))?;
                self.uninstall_apps(&[path]);
                None
            }
            FilesEffect::Reauthenticate { account } => {
                let account = self.account_named(&account)?;
                self.show_remote_account_panel(&account, true);
                None
            }
        };
        self.structure_dirty = true;
        Ok(Ran::Done(answer))
    }

    fn perform(&mut self, effect: crate::effects::FilesEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(&effect);
            return;
        }
        if let Err(error) = self.run_effect(effect) {
            self.status = Some(error);
            self.structure_dirty = true;
        }
    }

    fn refuse_while_answering(&mut self) -> Result<(), String> {
        self.retire_cancelled_effect();
        if self.effect_reply.is_some() {
            return Err("another file operation is still running".into());
        }
        Ok(())
    }

    fn retire_cancelled_effect(&mut self) {
        if self
            .effect_reply
            .as_ref()
            .is_some_and(ui::framework::EffectReply::is_cancelled)
        {
            self.effect_reply.take();
            self.local_clipboard.abandon();
            self.status = Some(t("files-clipboard-cancelled"));
            self.structure_dirty = true;
        }
    }

    pub(crate) fn await_effect(&mut self, reply: ui::framework::EffectReply) {
        match self.answering_op.take() {
            Some(op) => {
                if let crate::file_ops::FileOp::Copy { paths, folder }
                | crate::file_ops::FileOp::ImportMounted { paths, folder }
                | crate::file_ops::FileOp::Move { paths, folder, .. } = &op
                {
                    let shown = Path::new(folder).file_name().map_or_else(
                        || folder.clone(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                    let phase = t_args(
                        if matches!(op, crate::file_ops::FileOp::Move { .. }) {
                            "files-moving"
                        } else {
                            "files-copying"
                        },
                        &[
                            arg("count", i64_of_usize(paths.len())),
                            arg("folder", shown),
                        ],
                    );
                    reply.progress().report(phase, None, None);
                }
                self.op_replies.push((op, reply));
            }
            None => self.effect_reply = Some(reply),
        }
    }

    fn answer_op(&mut self, op: &crate::file_ops::FileOp, outcome: &crate::file_ops::Outcome) {
        self.op_replies.retain(|(_, reply)| !reply.is_cancelled());
        let Some(at) = self.op_replies.iter().position(|(queued, _)| queued == op) else {
            return;
        };
        let (_, reply) = self.op_replies.remove(at);
        reply.finish(match outcome.reason() {
            None => Ok(tilo_actions::ActionOutput::default()),
            Some(error) => Err(error.to_owned()),
        });
    }

    fn answer_effect(
        &mut self,
        result: Result<tilo_actions::ActionOutput, tilo_actions::ActionFailure>,
    ) {
        if let Some(reply) = self.effect_reply.take() {
            reply.answer(result);
        }
    }

    fn select_entry(&mut self, target: &Path) -> Result<(), String> {
        let index = self
            .displayed_entries()
            .iter()
            .position(|entry| entry.path == target)
            .ok_or_else(|| "file entry is no longer visible".to_string())?;
        self.selection.select_only(index);
        self.retain_shown_selection();
        Ok(())
    }

    fn entry_command(
        &mut self,
        entry: &str,
        command: impl FnOnce(&Self, &Path) -> Option<FileCommand>,
    ) -> Result<Option<serde_json::Value>, String> {
        let target = self
            .visible_entry_path(entry)
            .ok_or_else(|| "file entry is no longer visible".to_string())?;
        let command =
            command(self, &target).ok_or_else(|| "that command does not apply here".to_string())?;
        let index = self
            .entries
            .iter()
            .position(|candidate| candidate.path == target)
            .ok_or_else(|| "file entry is no longer visible".to_string())?;
        self.selection.select_only(index);
        self.retain_shown_selection();
        if !self.apply_command(command) {
            return Err("file operation could not be applied".into());
        }
        Ok(None)
    }

    fn account_named(&self, id: &str) -> Result<crate::remote_accounts::RemoteAccount, String> {
        self.remote_account(id)
            .ok_or_else(|| "remote account is no longer available".to_string())
    }

    fn importable_cloud_entry(&self, path: &Path) -> bool {
        entry_rows::importable_under(path, &self.import_mounts())
    }

    fn import_mounts(&self) -> Vec<PathBuf> {
        self.remote_account_snapshot()
            .into_iter()
            .filter(|account| {
                matches!(
                    account.kind,
                    crate::remote_accounts::RemoteAccountKind::GoogleDrive
                        | crate::remote_accounts::RemoteAccountKind::Dropbox
                ) && !account.is_import()
                    && account.health != crate::remote_accounts::RemoteAccountHealth::Disabled
            })
            .filter_map(|account| account.mount)
            .collect()
    }

    fn import_once(
        &mut self,
        entry: &str,
        folder: &Path,
    ) -> Result<Option<crate::file_ops::FileOp>, String> {
        let source = self
            .visible_entry_path(entry)
            .ok_or_else(|| "file entry is no longer visible".to_string())?;
        if !self.importable_cloud_entry(&source) {
            return Err(t("files-import-source-unavailable"));
        }
        if !folder.is_dir() {
            return Err(crate::i18n::CATALOG.t_args(
                "files-folder-missing",
                &[(
                    "path",
                    ui::i18n::ArgValue::Text(folder.display().to_string()),
                )],
            ));
        }
        let destination = std::fs::canonicalize(folder).map_err(|e| e.to_string())?;
        if self
            .remote_account_snapshot()
            .iter()
            .filter_map(|account| account.mount.as_ref())
            .any(|mount| {
                std::fs::canonicalize(mount).is_ok_and(|mount| destination.starts_with(mount))
            })
        {
            return Err(t("files-import-local-destination"));
        }
        self.queue_copy(crate::file_ops::FileOp::ImportMounted {
            paths: vec![source.display().to_string()],
            folder: destination.display().to_string(),
        })
    }

    fn copy_into_folder(
        &mut self,
        entry: &str,
        folder: &Path,
    ) -> Result<Option<crate::file_ops::FileOp>, String> {
        let source = self
            .visible_entry_path(entry)
            .ok_or_else(|| "file entry is no longer visible".to_string())?;
        if !folder.is_dir() {
            return Err(crate::i18n::CATALOG.t_args(
                "files-folder-missing",
                &[(
                    "path",
                    ui::i18n::ArgValue::Text(folder.display().to_string()),
                )],
            ));
        }
        self.queue_copy(crate::file_ops::FileOp::Copy {
            paths: vec![source.display().to_string()],
            folder: folder.display().to_string(),
        })
    }

    fn queue_copy(
        &mut self,
        op: crate::file_ops::FileOp,
    ) -> Result<Option<crate::file_ops::FileOp>, String> {
        let follow = OpFollowUp::Paste {
            count: 1,
            cut: false,
        };
        match self.queue(op.clone(), follow) {
            Some(outcome) => outcome.reason().map_or(Ok(None), |error| {
                Err(crate::i18n::CATALOG.t_args(
                    "files-copy-failed",
                    &[("detail", ui::i18n::ArgValue::Text(error.to_owned()))],
                ))
            }),
            None => Ok(Some(op)),
        }
    }

    fn answer_when_settled(&mut self, queued: Option<crate::file_ops::FileOp>) -> Ran {
        match queued {
            Some(op) => {
                self.answering_op = Some(op);
                Ran::Pending
            }
            None => Ran::Done(None),
        }
    }

    fn reveal_record(&mut self, record: &str) -> Result<(), String> {
        let record_id = uuid::Uuid::parse_str(record)
            .map_err(|_| "invalid collaborative record id".to_string())?;
        let record = self
            .collaborative_search
            .record(record_id)
            .ok_or_else(|| "collaborative record is not indexed yet".to_string())?;
        if crate::model::read_record_id(&record.path) != Some(*record_id.as_bytes()) {
            return Err("collaborative record is no longer available".into());
        }
        if record.path.parent() != Some(self.current_path.as_path())
            && let Some(parent) = record.path.parent().map(Path::to_path_buf)
        {
            self.navigate(parent);
        }
        if let Some(index) = self.entries.iter().position(|entry| {
            entry.record_id == Some(*record_id.as_bytes()) || entry.path == record.path
        }) {
            self.selection.select_only(index);
            self.retain_shown_selection();
        }
        Ok(())
    }

    fn decide_nearby(
        &mut self,
        transfer: ui::collaboration::NearbyTransferHandle,
        destination: Option<ui::collaboration::NearbyDestinationHandle>,
    ) -> Result<(), String> {
        let status = self
            .nearby_send
            .transfers()
            .iter()
            .find(|candidate| candidate.handle == transfer)
            .ok_or_else(|| "nearby transfer is no longer available".to_string())?;
        if status.direction != ui::collaboration::NearbyTransferDirection::Receiving
            || !matches!(
                status.phase,
                ui::collaboration::NearbyTransferPhase::AwaitingConsent
                    | ui::collaboration::NearbyTransferPhase::AwaitingDestination
            )
        {
            return Err("nearby transfer no longer awaits a receiver decision".into());
        }
        if let Some(destination) = destination
            && !self
                .nearby_send
                .destinations()
                .iter()
                .any(|candidate| candidate.handle == destination)
        {
            return Err("nearby destination is no longer available".into());
        }
        self.nearby_send.decide(transfer, destination, false);
        self.status = Some(if destination.is_some() {
            t("files-nearby-accepted")
        } else {
            t("files-nearby-declined")
        });
        Ok(())
    }

    fn visible_open_path(&self, resource_id: &str) -> Option<PathBuf> {
        self.visible_entry_path(resource_id)
            .or_else(|| {
                self.favs
                    .borrow()
                    .favorites
                    .iter()
                    .find(|favorite| {
                        crate::search_provider::place_resource_id(&favorite.path) == resource_id
                    })
                    .map(|favorite| favorite.path.clone())
            })
            .or_else(|| {
                self.remote_account_snapshot()
                    .into_iter()
                    .filter(|account| {
                        matches!(remote_folder_availability(account), Availability::Available)
                    })
                    .filter_map(|account| account.mount)
                    .find(|mount| crate::search_provider::place_resource_id(mount) == resource_id)
            })
    }

    fn update_toolbar(&self) {
        let nav = crate::toolbar::NavAvailability {
            back: !self.history.is_empty(),
            forward: !self.forward.is_empty(),
            up: self.current_path.parent().is_some()
                && !self.is_recent_view()
                && !self.is_applications_view(),
        };
        self.toolbar.set_context(
            nav,
            self.view_mode,
            crate::archive::location(&self.current_path).is_some(),
        );
        self.toolbar.set_path(
            self.current_path.clone(),
            self.home_path.clone(),
            self.workspace_roots.clone(),
        );
    }

    fn current_places(&self) -> Places {
        Places::new(&self.favs, self.current_path.clone())
            .with_active_account(self.cloud.active.then(|| self.cloud.account.clone()))
            .with_workspaces(self.workspace_roots.clone())
            .with_remote_accounts(
                self.remote_account_snapshot()
                    .into_iter()
                    .map(|account| {
                        let available = matches!(
                            remote_folder_availability(&account),
                            Availability::Available
                        );
                        places_sidebar::AccountPlace {
                            status: account.badge(),
                            id: account.id.as_str().to_owned(),
                            label: account.label,
                            mount: account.mount.filter(|_| available),
                        }
                    })
                    .collect(),
            )
            .with_drives(self.drives.shown.iter().map(drives::drive_place).collect())
    }

    fn refresh_places_pane(&mut self, chrome: &mut Chrome) {
        let places = self.current_places();
        let sections = places.sections();
        let selected = places.selected_id();
        if let Some((shown, chosen)) = &self.places_shown
            && *shown == sections
            && (*chosen == selected || *self.places_choice.borrow() == selected)
        {
            self.places_shown = Some((sections, selected));
            return;
        }
        let drag = std::mem::take(&mut self.stage_favorite_drag)
            .then(|| places.favorite_drag())
            .flatten();
        let context = Rc::clone(&self.places_context);
        let mut list = places
            .nav_list(&self.cmds, &self.places_choice)
            .on_context_menu(move |id, _, at| {
                if let Some(path) = id.strip_prefix("fav:") {
                    *context.borrow_mut() = Some((PathBuf::from(path), at));
                }
            });
        if let Some((dragged, before)) = drag {
            list.stage_drag(&dragged, &before);
        }
        chrome.nav.set_sidebar(Some(list.into_widget()));
        *self.places_choice.borrow_mut() = None;
        self.places_shown = Some((sections, selected));
    }

    fn build_content_pane(&self, theme: &ui::Theme) -> Box<dyn Widget> {
        if self.cloud.active {
            return self.build_cloud_pane(theme);
        }
        let sp = &theme.tokens.spacing;
        if let Some((title, detail, can_retry)) = self.state_surface_copy() {
            return Box::new(if can_retry {
                failure_state(
                    title,
                    detail,
                    self.folder_recovery_command() == FileCommand::Refresh,
                )
            } else {
                EmptyState::new(title).with_message(detail).busy()
            });
        }
        let displayed = self.displayed_entries();
        let scale = self.zoom_scale();
        let view = FileView::new(&displayed, &self.selection, &self.cmds)
            .with_scale(scale)
            .with_workspace_roots(Rc::from(self.workspace_roots.clone()))
            .with_hover(self.staged_hover)
            .with_theme(theme)
            .with_rename(self.renaming, self.rename_rect.clone())
            .with_cursor(self.last_cursor)
            .with_drop_targets(self.drop_targets.clone())
            .with_item_rects(self.item_rects.clone())
            .with_thumbs(self.thumb_ctx())
            .with_folder_counts(Some(Arc::clone(&self.folder_counts)), self.show_hidden)
            .with_list(self.list_slot.clone());
        let mut view_col = Column::new().with_vertical_padding(sp.s1);
        match self.view_mode {
            ViewMode::List => {
                view_col = view_col.push(crate::views::list_view::build_list(
                    view,
                    self.sort_by,
                    self.sort_order,
                ));
            }
            ViewMode::Grid => {
                view_col = view_col.push(
                    Scrollable::new(ScrollAxis::Vertical)
                        .fill_parent_height()
                        .with_persisted_offset(self.scroll_offset.clone())
                        .with_cursor(self.last_cursor)
                        .with_child(crate::views::grid_view::build_grid(view))
                        .into_widget(),
                );
            }
        }
        if !self.entries.is_empty() {
            return view_col.into_widget();
        }
        Box::new(Stack::new().push(view_col.into_widget()).push(Box::new(
            EmptyState::new(self.empty_state_message()).with_icon(self.empty_state_icon()),
        )))
    }

    fn footer_label(&self) -> String {
        if self.cloud.active {
            return t_args(
                "files-item-count",
                &[arg("count", i64_of_usize(self.cloud.entries.len()))],
            );
        }
        if self.error.is_some() || self.is_loading {
            return String::new();
        }
        if let Some(description) = self
            .selection
            .primary()
            .and_then(|index| {
                self.displayed_entries()
                    .get(index)
                    .map(|entry| entry.path.clone())
            })
            .and_then(|path| self.converted_description(&path))
        {
            return description;
        }
        let count = self.entries.len();
        let left = self
            .status
            .clone()
            .unwrap_or_else(|| t_args("files-item-count", &[arg("count", i64_of_usize(count))]));
        let sel = self.selection.selected().len();
        if sel > 0 {
            t_args(
                "files-selected-count",
                &[arg("base", left), arg("count", i64_of_usize(sel))],
            )
        } else {
            left
        }
    }

    fn empty_state_message(&self) -> String {
        if self.is_recent_view() {
            t("files-empty-recent")
        } else if self.is_applications_view() {
            t("files-empty-applications")
        } else if self.is_trash_view() {
            t("files-empty-trash")
        } else {
            t("files-empty-folder")
        }
    }

    fn bounded_state_text(value: &str, max_chars: usize) -> String {
        let count = value.chars().count();
        if count <= max_chars {
            return value.to_string();
        }
        let keep = max_chars.saturating_sub(1);
        format!(
            "…{}",
            value
                .chars()
                .skip(count.saturating_sub(keep))
                .collect::<String>()
        )
    }

    fn state_surface_copy(&self) -> Option<(String, String, bool)> {
        if self.cloud.active {
            return None;
        }
        let location = Self::bounded_state_text(
            &crate::model::format_path(&self.current_path, self.home_path.as_deref()),
            STATE_SURFACE_LOCATION_CHARS,
        );
        if self.is_loading {
            return Some((
                t("files-loading-title"),
                t_args("files-loading-detail", &[arg("path", location)]),
                false,
            ));
        }
        let (failure, cause) = self.folder_failure()?;
        let name = self.current_path.file_name().map_or_else(
            || location.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        let detail = match failure {
            crate::fileops::FolderFailure::Permission => t("files-folder-error-permission"),
            crate::fileops::FolderFailure::Missing => t("files-folder-error-missing"),
            crate::fileops::FolderFailure::Other => crate::fileops::readable_error(cause),
        };
        Some((
            t_args("files-load-error-title", &[arg("name", name)]),
            detail,
            true,
        ))
    }

    fn folder_failure(&self) -> Option<(crate::fileops::FolderFailure, &str)> {
        let error = self.error.as_deref()?;
        let cause = error.rsplit_once(": ").map_or(error, |(_, cause)| cause);
        Some((crate::fileops::FolderFailure::of(cause), cause))
    }

    fn folder_recovery_command(&self) -> FileCommand {
        match self.folder_failure() {
            Some((failure, _)) if !failure.may_pass() => FileCommand::Up,
            _ => FileCommand::Refresh,
        }
    }

    fn show_collab_panel(&mut self, panel: CollabPanel) {
        self.merge_preflight_pending = None;
        self.collab_panel = Some(panel);
        self.collab_panel_focused = true;
    }

    fn close_collab_panel(&mut self) {
        self.merge_preflight_pending = None;
        self.collab_panel = None;
        self.collab_panel_focused = false;
    }

    fn show_keyboard_shortcuts_panel(&mut self) {
        use ui::platform::{HostShortcut, PlatformConventions, Shortcut};
        let conventions = PlatformConventions::default();
        let keys = |shortcuts: &[Shortcut]| {
            shortcuts
                .iter()
                .map(|shortcut| shortcut.label(conventions))
                .collect::<Vec<_>>()
                .join(" / ")
        };
        let rows: [(&str, Vec<Shortcut>); 14] = [
            ("files-shortcuts-open", vec![Shortcut::key("Enter")]),
            ("files-shortcuts-preview", vec![Shortcut::key("Space")]),
            (
                "files-shortcuts-back-forward",
                vec![Shortcut::key("←").alt(), Shortcut::key("→").alt()],
            ),
            ("files-shortcuts-up", vec![Shortcut::key("↑").alt()]),
            ("files-shortcuts-location", vec![Shortcut::command("L")]),
            ("files-shortcuts-rename", vec![Shortcut::key("F2")]),
            (
                "files-shortcuts-new-folder",
                vec![Shortcut::command("N").shift()],
            ),
            ("files-shortcuts-trash", vec![Shortcut::key("Delete")]),
            (
                "files-shortcuts-copy-cut-paste",
                vec![
                    Shortcut::command("C"),
                    Shortcut::command("X"),
                    Shortcut::command("V"),
                ],
            ),
            (
                "files-shortcuts-undo-redo",
                vec![
                    HostShortcut::Undo.shortcut(conventions),
                    HostShortcut::Redo.shortcut(conventions),
                ],
            ),
            ("files-shortcuts-select-all", vec![Shortcut::command("A")]),
            ("files-shortcuts-hidden", vec![Shortcut::command("H")]),
            (
                "files-shortcuts-size",
                vec![Shortcut::command("+"), Shortcut::command("−")],
            ),
            ("files-shortcuts-refresh", vec![Shortcut::key("F5")]),
        ];
        self.show_collab_panel(CollabPanel {
            title: t("files-shortcuts-title"),
            summary: String::new(),
            lines: rows
                .iter()
                .map(|(action, shortcuts)| {
                    t_args(
                        "files-shortcuts-row",
                        &[arg("action", t(action)), arg("keys", keys(shortcuts))],
                    )
                })
                .collect(),
            action: None,
        });
    }

    fn collab_panel_action_command(&self) -> Option<FileCommand> {
        match self.collab_panel.as_ref()?.action.as_ref()? {
            CollabPanelAction::DestructiveEviction(paths) => {
                Some(FileCommand::ConfirmDestructiveEviction {
                    paths: paths.clone(),
                })
            }
            CollabPanelAction::PermanentDelete(paths) => {
                Some(FileCommand::ConfirmPermanentDelete {
                    paths: paths.clone(),
                })
            }
            CollabPanelAction::EmptyTrash => Some(FileCommand::ConfirmEmptyTrash),
            CollabPanelAction::RecheckAccount(id) => {
                Some(FileCommand::RecheckRemoteAccount(id.clone()))
            }
            CollabPanelAction::ApplySafeMerge {
                conflict_id,
                version_id,
            } => Some(FileCommand::ResolveConflict {
                conflict_id: *conflict_id,
                strategy: fs_protocol::ConflictStrategy::Merge,
                version_id: *version_id,
            }),
            CollabPanelAction::ChooseNearbyDestination { handle } => {
                Some(FileCommand::ChooseNearbyDestination { handle: *handle })
            }
            CollabPanelAction::RetryNearby { handle } => {
                Some(FileCommand::RetryNearby { handle: *handle })
            }
        }
    }

    fn collab_panel_action_label(&self) -> Option<String> {
        match self.collab_panel.as_ref()?.action.as_ref()? {
            CollabPanelAction::DestructiveEviction(_) => {
                Some(t("files-destructive-eviction-confirm"))
            }
            CollabPanelAction::PermanentDelete(_) => Some(t("files-permanent-delete-confirm")),
            CollabPanelAction::EmptyTrash => Some(t("files-empty-trash-confirm")),
            CollabPanelAction::RecheckAccount(_) => Some(t("files-account-check-again")),
            CollabPanelAction::ApplySafeMerge { .. } => Some(t("files-merge-apply-safe")),
            CollabPanelAction::ChooseNearbyDestination { .. } => {
                Some(t("files-nearby-choose-destination"))
            }
            CollabPanelAction::RetryNearby { .. } => Some(t("files-nearby-retry")),
        }
    }

    fn collab_panel_action_description(&self) -> Option<String> {
        match self.collab_panel.as_ref()?.action.as_ref()? {
            CollabPanelAction::DestructiveEviction(_) => Some(t("files-destructive-eviction-loss")),
            CollabPanelAction::PermanentDelete(_) | CollabPanelAction::EmptyTrash => {
                Some(t("files-permanent-delete-loss"))
            }
            CollabPanelAction::RecheckAccount(_) => Some(t("files-account-check-again-detail")),
            CollabPanelAction::ApplySafeMerge { .. } => Some(t("files-merge-apply-safe-detail")),
            CollabPanelAction::ChooseNearbyDestination { .. } => {
                Some(t("files-nearby-consent-detail"))
            }
            CollabPanelAction::RetryNearby { .. } => Some(t("files-nearby-retry-detail")),
        }
    }

    fn accessibility_status(&self) -> String {
        if self.cloud.active {
            return self.footer_label();
        }
        if let Some(panel) = &self.collab_panel {
            let details = panel.lines.join(". ");
            return t_args(
                "files-a11y-panel-details",
                &[
                    arg("title", panel.title.clone()),
                    arg("summary", panel.summary.clone()),
                    arg("details", details),
                ],
            );
        }
        if let Some(error) = &self.error {
            return t_args("files-a11y-load-error", &[arg("error", error.clone())]);
        }
        if self.is_loading {
            return t_args(
                "files-loading-folder",
                &[arg("path", self.current_path.display().to_string())],
            );
        }
        if self.entries.is_empty() {
            return self.empty_state_message();
        }
        self.footer_label()
    }

    fn publish_status(&mut self) {
        if self.status != self.status_said {
            self.status_said.clone_from(&self.status);
            match self.status.clone() {
                Some(status) => self.status_notice.show(status),
                None => {
                    self.status_notice.dismiss();
                }
            }
        }
        let archive = crate::archive::location(&self.current_path)
            .is_some()
            .then(|| self.current_path.clone());
        if archive != self.archive_said {
            if archive.is_some() && self.status.is_none() {
                self.status_notice.show(t("files-archive-read-only"));
            }
            self.archive_said = archive;
        }
        if let Some(text) = self.status_notice.take() {
            ui::attention::publish(ui::attention::Notice::did(files_owner(), text));
        }
    }

    fn sync_toolbar_compact(&self, chrome: &Chrome) {
        let column = chrome.nav.geometry().content_column;
        self.toolbar.set_content_width(column.width());
    }

    pub fn current_path(&self) -> PathBuf {
        self.current_path.clone()
    }
    pub fn home_path(&self) -> Option<&std::path::Path> {
        self.home_path.as_deref()
    }
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub fn selected_index(&self) -> Option<usize> {
        self.selection
            .primary()
            .filter(|&i| self.selection.is_selected(i))
    }
    pub fn selected_indices(&self) -> Vec<usize> {
        self.selection.selected()
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn is_loading(&self) -> bool {
        self.is_loading
    }

    #[doc(hidden)]
    pub fn debug_show_loading_state(&mut self) {
        self.entries.clear();
        self.error = None;
        self.is_loading = true;
        self.status = None;
    }

    #[doc(hidden)]
    pub fn debug_show_error_state(&mut self) {
        self.entries.clear();
        self.is_loading = false;
        let denied = std::io::Error::from_raw_os_error(13).to_string();
        self.error = Some(t_args(
            "files-directory-read-failed",
            &[arg("error", denied)],
        ));
        self.status = None;
    }
    pub fn sort_by(&self) -> SortBy {
        self.sort_by
    }
    pub fn sort_order(&self) -> SortOrder {
        self.sort_order
    }
    pub fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    pub fn navigate(&mut self, path: PathBuf) {
        if path != self.current_path {
            self.history.push(self.current_path.clone());
            self.forward.clear();
        }
        self.load(path);
    }

    pub fn back(&mut self) {
        if self.is_recent_view() || self.is_applications_view() {
            return;
        }
        if let Some(parent) = self.current_path.parent() {
            self.navigate(parent.to_path_buf());
        }
    }

    pub fn go_up(&mut self) {
        self.back();
    }

    pub fn history_back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.forward.push(self.current_path.clone());
            self.load(prev);
        }
    }

    pub fn history_forward(&mut self) {
        if let Some(next) = self.forward.pop() {
            self.history.push(self.current_path.clone());
            self.load(next);
        }
    }

    pub fn refresh(&mut self) {
        if !self.cloud.active {
            self.load(self.current_path.clone());
        }
    }

    fn refresh_metadata_if_due(&mut self, now: Instant) -> bool {
        if !self.folder_reads.is_pending() {
            let watched = self.watched_directory(&self.current_path);
            if self.directory_watch.point_at(watched.as_deref()) {
                self.directory_changed = true;
            }
        }
        let identities_watched = self.startup_accounts.is_none()
            && self.identity_watch.point_at(
                crate::identity_directory::TrustedHumanDirectory::default_path()
                    .parent()
                    .filter(|dir| !dir.as_os_str().is_empty()),
            );
        if !self.directory_changed && self.directory_watch.take_changes() {
            self.directory_changed = true;
        }
        let identity_signal = identities_watched || self.identity_watch.take_changes();
        let catalog_watched = self.catalog_watch.point_at(&self.catalog_roots_to_watch());
        let catalog_signal = catalog_watched || self.catalog_watch.take_changes();
        let poll_due = self.next_metadata_refresh.is_some_and(|due| now >= due);
        if poll_due || self.next_metadata_refresh.is_none() {
            self.next_metadata_refresh = self.next_poll_interval().map(|interval| now + interval);
        }
        let catalog_changed = (catalog_signal
            || (poll_due
                && self.catalog_watch.coverage() != crate::directory_watch::Coverage::Complete))
            && self
                .collaborative_record_pager
                .borrow_mut()
                .poll_generation();
        let identities_changed = (identity_signal
            || (poll_due
                && self.identity_watch.coverage() != crate::directory_watch::Coverage::Complete))
            && self.refresh_trusted_identities(None);
        let directory_polled = poll_due
            && self.directory_watch.coverage() != crate::directory_watch::Coverage::Complete;
        let change_readable = self.directory_changed
            && now
                >= self.last_directory_read + metadata_refresh::change_read_gap(self.entries.len());
        let changed = identities_changed || catalog_changed;
        if !(directory_polled || change_readable) || self.directory_read_deferred() {
            return changed;
        }
        let _ = self.directory_watch.take_changes();
        self.directory_changed = false;
        self.last_directory_read = now;
        self.refresh_metadata_preserving_view() || changed
    }

    fn catalog_roots_to_watch(&self) -> Vec<PathBuf> {
        let roots = self.collaborative_record_pager.borrow().generation_roots();
        if roots.len() > crate::model::WorkspaceRecordIndexPager::GENERATION_ROOTS_PER_POLL {
            return Vec::new();
        }
        roots
    }

    fn directory_read_deferred(&self) -> bool {
        self.renaming.is_some() || self.pin_drag.is_some() || self.folder_reads.is_pending()
    }

    fn watched_directory(&self, path: &Path) -> Option<PathBuf> {
        let virtual_listing = crate::recent::is_location(path)
            || crate::applications::is_location(path)
            || crate::archive::location(path).is_some()
            || (self.hosted_clipboard() && fileops::is_trash_dir(path));
        (!virtual_listing).then(|| path.to_path_buf())
    }

    fn next_poll_interval(&self) -> Option<Duration> {
        use crate::directory_watch::Coverage;
        let entries = self.entries.len();
        let directory = match self.directory_watch.coverage() {
            Coverage::Complete => None,
            Coverage::Partial => Some(metadata_refresh::interval(entries)),
            Coverage::None => Some(metadata_refresh::unwatched_interval(entries)),
        };
        let catalog = (self.collaborative_record_pager.borrow().has_roots()
            && self.catalog_watch.coverage() != Coverage::Complete)
            .then_some(metadata_refresh::BACKGROUND_INTERVAL);
        let identities = (self.startup_accounts.is_none()
            && self.identity_watch.coverage() != Coverage::Complete)
            .then_some(metadata_refresh::BACKGROUND_INTERVAL);
        [directory, catalog, identities].into_iter().flatten().min()
    }

    fn refresh_trusted_identities(&mut self, path: Option<&Path>) -> bool {
        if self.startup_accounts.is_some() && path.is_none() {
            return false;
        }
        let next = path.map_or_else(
            crate::identity_directory::TrustedHumanDirectory::load_default,
            crate::identity_directory::TrustedHumanDirectory::load,
        );
        // A malformed or missing live authority snapshot is not permission to
        // retain stale recipients. Fail closed to the empty directory.
        let next = next.unwrap_or_default();
        if next == self.trusted_humans {
            return false;
        }
        self.trusted_humans = next;
        self.structure_dirty = true;
        self.framework_history_binding = None;
        true
    }

    fn refresh_metadata_preserving_view(&mut self) -> bool {
        if self.folder_reads.is_pending() {
            return false;
        }
        let job = self.folder_job(self.current_path.clone(), true);
        self.folder_reads
            .start(job, Purpose::Update)
            .is_some_and(|(purpose, read)| self.apply_read(purpose, read))
    }

    fn update_from(&mut self, listing: Result<Vec<Entry>, String>) -> bool {
        let selected = self
            .selection
            .selected()
            .into_iter()
            .filter_map(|index| self.entries.get(index))
            .map(|entry| (entry.record_id, entry.path.clone()))
            .collect::<Vec<_>>();
        let primary = self
            .selection
            .primary()
            .and_then(|index| self.entries.get(index))
            .map(|entry| (entry.record_id, entry.path.clone()));
        let Ok(mut entries) = listing else {
            // A transient unmount/reconnect must not replace a still-useful
            // view with an error page. Explicit Refresh remains the path that
            // surfaces directory errors to the user.
            return false;
        };
        if !self.show_hidden {
            entries.retain(|entry| !entry.name.starts_with('.'));
        }
        if !self.is_recent_view() {
            sort_entries(self.sort_by, self.sort_order, &mut entries);
        }
        if entries == self.entries {
            return false;
        }
        let matches_key = |entry: &Entry, key: &(Option<[u8; 16]>, PathBuf)| {
            key.0.map_or_else(
                || entry.path == key.1,
                |record| entry.record_id == Some(record),
            )
        };
        let selected_indices = entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                selected
                    .iter()
                    .any(|key| matches_key(entry, key))
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let primary_index = primary
            .as_ref()
            .and_then(|key| entries.iter().position(|entry| matches_key(entry, key)));
        self.entries = entries;
        self.selection
            .replace_indices(selected_indices, primary_index);
        self.retain_shown_selection();
        self.structure_dirty = true;
        true
    }

    pub fn select(&mut self, idx: Option<usize>) -> Option<PathBuf> {
        let now = ui::time::now();
        if let (Some(i), Some((pi, pt))) = (idx, self.last_click)
            && i == pi
            && now.duration_since(pt) < ui::gesture::multi_click_interval()
        {
            let disp = self.displayed_entries();
            if let Some(e) = disp.get(i) {
                let path = e.path.clone();
                self.last_click = None;
                return Some(path);
            }
        }
        match idx {
            Some(i) => self.selection.select_only(i),
            None => self.selection.clear(),
        }
        self.retain_shown_selection();
        self.last_click = idx.map(|i| (i, now));
        None
    }

    pub fn set_sort_by(&mut self, s: SortBy) {
        let changed = self.sort_by != s;
        self.sort_by = s;
        self.sort_preserving_selection();
        if changed {
            self.store_preferences();
            self.store_widths();
        }
    }
    pub fn toggle_sort_order(&mut self) {
        self.sort_order = match self.sort_order {
            SortOrder::Ascending => SortOrder::Descending,
            SortOrder::Descending => SortOrder::Ascending,
        };
        self.sort_preserving_selection();
        self.store_preferences();
        self.store_widths();
    }

    fn sort_preserving_selection(&mut self) {
        let positions: HashMap<_, _> = self
            .selection
            .selected_set()
            .iter()
            .copied()
            .chain(self.selection.primary())
            .chain(self.selection.anchor())
            .chain(self.renaming)
            .filter_map(|index| {
                self.entries
                    .get(index)
                    .map(|entry| (entry.path.clone(), index))
            })
            .collect();
        sort_entries(self.sort_by, self.sort_order, &mut self.entries);
        let remapped: HashMap<_, _> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(new, entry)| positions.get(&entry.path).map(|old| (*old, new)))
            .collect();
        self.selection
            .remap_indices(|old| remapped.get(&old).copied());
        self.retain_shown_selection();
        self.renaming = self.renaming.and_then(|old| remapped.get(&old).copied());
        self.rename_rect.set(None);
        self.last_click = None;
    }
    pub fn set_view_mode(&mut self, m: ViewMode) {
        if self.view_mode == m {
            return;
        }
        self.view_mode = m;
        self.store_preferences();
        self.store_widths();
    }

    pub fn entries_snapshot(&self) -> &[Entry] {
        &self.entries
    }

    pub fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn footer_text(&self) -> (usize, String) {
        (
            self.entries.len(),
            crate::model::format_path(&self.current_path, self.home_path.as_deref()),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "central semantic command dispatcher keeps every input source on one state transition path"
    )]
    pub fn apply_command(&mut self, cmd: FileCommand) -> bool {
        if crate::archive::location(&self.current_path).is_some()
            && matches!(
                &cmd,
                FileCommand::NewFolder
                    | FileCommand::NewFromTemplate(_)
                    | FileCommand::Rename
                    | FileCommand::RenameCommit
                    | FileCommand::Delete
                    | FileCommand::Copy
                    | FileCommand::Cut
                    | FileCommand::Paste
                    | FileCommand::ExtractHere
                    | FileCommand::CompressSelection(_)
                    | FileCommand::OpenTerminal
                    | FileCommand::ArmDrag { .. }
                    | FileCommand::BeginDrag { .. }
            )
        {
            self.status = Some(t("files-archive-read-only"));
            return true;
        }
        if self.is_recent_view()
            && matches!(
                &cmd,
                FileCommand::NewFolder
                    | FileCommand::NewFromTemplate(_)
                    | FileCommand::Paste
                    | FileCommand::OpenTerminal
                    | FileCommand::ExtractHere
                    | FileCommand::ExtractTo
                    | FileCommand::CompressSelection(_)
            )
        {
            self.status = Some(t("files-recent-not-a-folder"));
            return true;
        }
        if self.is_applications_view() && self.refuses_in_applications(&cmd) {
            return true;
        }
        match cmd {
            FileCommand::Navigate(p) => {
                self.navigate(p);
                true
            }
            FileCommand::Up => {
                self.go_up();
                true
            }
            FileCommand::Back => {
                self.history_back();
                true
            }
            FileCommand::Forward => {
                self.history_forward();
                true
            }
            FileCommand::Refresh => {
                self.refresh();
                true
            }
            FileCommand::Click { index, ctrl, shift } => self.apply_click(index, ctrl, shift),
            FileCommand::ContextSelect(target) => self.apply_context_select(target),
            FileCommand::Activate(index) => self.apply_activate(index),
            FileCommand::ContextMenuAt { index, at } => {
                self.apply_context_select(Some(index));
                self.last_cursor = at;
                self.open_context_menu(at);
                true
            }
            FileCommand::MoveSelection { index, extend } => {
                if index < self.displayed_entries().len() {
                    self.selection.move_to(index, extend);
                    self.retain_shown_selection();
                    true
                } else {
                    false
                }
            }
            FileCommand::SortColumn(col) => self.apply_sort_column(col),
            policy @ (FileCommand::KeepOffline | FileCommand::RemoveOfflinePin) => {
                let keep_offline = matches!(policy, FileCommand::KeepOffline);
                let value: &[u8] = if keep_offline {
                    b"pinned"
                } else {
                    b"automatic"
                };
                let displayed = self.displayed_entries();
                let paths: Vec<_> = self
                    .selection
                    .selected()
                    .into_iter()
                    .filter_map(|index| displayed.get(index))
                    .filter(|entry| {
                        entry.availability.as_ref().is_some_and(|availability| {
                            if keep_offline {
                                availability.policy != fs_protocol::AvailabilityPolicy::Pinned
                            } else {
                                availability.policy == fs_protocol::AvailabilityPolicy::Pinned
                                    && !availability.policy_inherited
                            }
                        })
                    })
                    .map(|entry| entry.path.clone())
                    .collect();
                if paths.is_empty() {
                    self.status = Some(if value == b"pinned" {
                        t("files-no-pin-needed")
                    } else {
                        t("files-no-direct-pin")
                    });
                    return true;
                }
                let mut failures = Vec::new();
                for path in &paths {
                    if let Err(error) = xattr::set(path, fs_protocol::PIN_XATTR, value) {
                        failures.push(format!("{}: {error}", path.display()));
                    }
                }
                if failures.is_empty() {
                    self.status = Some(if value == b"pinned" {
                        t_args(
                            "files-keeping-offline",
                            &[arg("count", i64_of_usize(paths.len()))],
                        )
                    } else {
                        t_args(
                            "files-using-automatic",
                            &[arg("count", i64_of_usize(paths.len()))],
                        )
                    });
                } else {
                    self.status = Some(t_args(
                        "files-offline-policy-failed",
                        &[arg("errors", failures.join("; "))],
                    ));
                }
                self.refresh_metadata_preserving_view();
                true
            }
            FileCommand::FreeLocalSpace => {
                let displayed = self.displayed_entries();
                let paths: Vec<_> = self
                    .selection
                    .selected()
                    .into_iter()
                    .filter_map(|index| displayed.get(index))
                    .filter(|entry| {
                        !entry.is_dir
                            && entry.availability.as_ref().is_some_and(|availability| {
                                availability.policy == fs_protocol::AvailabilityPolicy::Automatic
                                    && !availability.policy_inherited
                                    && availability.transfer.phase
                                        == fs_protocol::TransferPhase::Idle
                                    && matches!(
                                        availability.residency,
                                        fs_protocol::Residency::Partial { .. }
                                            | fs_protocol::Residency::Resident { .. }
                                    )
                            })
                    })
                    .map(|entry| entry.path.clone())
                    .collect();
                let mut failures = Vec::new();
                let mut safety_refusals = Vec::new();
                for path in &paths {
                    if let Err(error) =
                        xattr::set(path, fs_protocol::AVAILABILITY_CONTROL_XATTR, b"evict")
                    {
                        if xattr::ErrorCode::of(&error) == Some(xattr::ErrorCode::Busy) {
                            safety_refusals.push(path.clone());
                        } else {
                            failures.push(format!("{}: {error}", path.display()));
                        }
                    }
                }
                self.status = Some(if paths.is_empty() {
                    t("files-no-local-space-releasable")
                } else if failures.is_empty() && safety_refusals.is_empty() {
                    t_args(
                        "files-freeing-local-space",
                        &[arg("count", i64_of_usize(paths.len()))],
                    )
                } else if failures.is_empty() {
                    t("files-eviction-needs-confirmation")
                } else {
                    t_args(
                        "files-free-local-space-failed",
                        &[arg("errors", failures.join("; "))],
                    )
                });
                if !safety_refusals.is_empty() {
                    self.show_collab_panel(CollabPanel {
                        title: t("files-destructive-eviction-title"),
                        summary: t_args(
                            "files-destructive-eviction-summary",
                            &[arg("count", i64_of_usize(safety_refusals.len()))],
                        ),
                        lines: vec![
                            t("files-destructive-eviction-no-replica"),
                            t("files-destructive-eviction-loss"),
                            t("files-destructive-eviction-protected"),
                        ],
                        action: Some(CollabPanelAction::DestructiveEviction(safety_refusals)),
                    });
                }
                self.refresh_metadata_preserving_view();
                true
            }
            FileCommand::ConfirmDestructiveEviction { paths } => {
                let mut failures = Vec::new();
                for path in &paths {
                    if let Err(error) = xattr::set(
                        path,
                        fs_protocol::AVAILABILITY_CONTROL_XATTR,
                        b"evict-destructive",
                    ) {
                        failures.push(format!("{}: {error}", path.display()));
                    }
                }
                self.status = Some(if failures.is_empty() {
                    t_args(
                        "files-destructive-eviction-started",
                        &[arg("count", i64_of_usize(paths.len()))],
                    )
                } else {
                    t_args(
                        "files-free-local-space-failed",
                        &[arg("errors", failures.join("; "))],
                    )
                });
                self.refresh_metadata_preserving_view();
                true
            }
            FileCommand::ConfirmPermanentDelete { paths } => self.apply_permanent_delete(&paths),
            FileCommand::ConfirmEmptyTrash => self.perform_empty_trash(),
            FileCommand::RetryTransfer => {
                let displayed = self.displayed_entries();
                let paths: Vec<_> = self
                    .selection
                    .selected()
                    .into_iter()
                    .filter_map(|index| displayed.get(index))
                    .filter(|entry| {
                        entry.availability.as_ref().is_some_and(|availability| {
                            !matches!(
                                availability.residency,
                                fs_protocol::Residency::Resident { .. }
                            ) && availability.transfer.phase == fs_protocol::TransferPhase::Idle
                        })
                    })
                    .map(|entry| entry.path.clone())
                    .collect();
                let mut failures = Vec::new();
                for path in &paths {
                    if let Err(error) =
                        xattr::set(path, fs_protocol::AVAILABILITY_CONTROL_XATTR, b"retry")
                    {
                        failures.push(format!("{}: {error}", path.display()));
                    }
                }
                self.status = Some(if paths.is_empty() {
                    t("files-no-retryable-transfer")
                } else if failures.is_empty() {
                    t_args(
                        "files-retrying-transfers",
                        &[arg("count", i64_of_usize(paths.len()))],
                    )
                } else {
                    t_args("files-retry-failed", &[arg("errors", failures.join("; "))])
                });
                self.refresh_metadata_preserving_view();
                true
            }
            FileCommand::ShowProperties => {
                let entry = self
                    .selection
                    .primary()
                    .and_then(|index| self.displayed_entries().get(index).cloned());
                if let Some(entry) = entry {
                    let mut lines = vec![
                        t_args(
                            "files-property-type",
                            &[arg(
                                "type",
                                if entry.is_dir {
                                    t("files-a11y-folder")
                                } else {
                                    entry.mime.clone()
                                },
                            )],
                        ),
                        t_args(
                            "files-property-location",
                            &[arg(
                                "location",
                                entry
                                    .path
                                    .parent()
                                    .unwrap_or_else(|| Path::new("/"))
                                    .display()
                                    .to_string(),
                            )],
                        ),
                        t_args(
                            "files-property-size",
                            &[arg("size", crate::model::human_size(&entry))],
                        ),
                        t_args(
                            "files-property-modified",
                            &[arg(
                                "time",
                                entry.modified.map_or_else(
                                    || t("files-property-unknown"),
                                    |modified| crate::model::format_modified(Some(modified)),
                                ),
                            )],
                        ),
                    ];
                    if let Some(metadata) = &entry.rich_metadata {
                        let properties = &metadata.properties;
                        if let (Some(width), Some(height)) =
                            (properties.image_width, properties.image_height)
                        {
                            lines.push(t_args(
                                "files-property-dimensions",
                                &[
                                    arg("width", i64::from(width)),
                                    arg("height", i64::from(height)),
                                ],
                            ));
                        }
                        if let Some(duration_ms) = properties.duration_ms {
                            lines.push(t_args(
                                "files-property-duration",
                                &[arg("duration", ui::format::timecode(duration_ms / 1_000))],
                            ));
                        }
                        if let Some(page_count) = properties.page_count {
                            lines.push(t_args(
                                "files-property-pages",
                                &[arg("count", i64::from(page_count))],
                            ));
                        }
                        if let Some(codec) = &properties.codec {
                            lines.push(t_args(
                                "files-property-codec",
                                &[arg("codec", codec.clone())],
                            ));
                        }
                        if let Some(container) = &properties.container {
                            lines.push(t_args(
                                "files-property-container",
                                &[arg("container", container.clone())],
                            ));
                        }
                    }
                    if let Some(availability) = &entry.availability {
                        let residency = match availability.residency {
                            fs_protocol::Residency::NotResident => {
                                t("files-residency-metadata-only")
                            }
                            fs_protocol::Residency::Partial { resident_bytes } => t_args(
                                "files-residency-partly-local",
                                &[arg(
                                    "bytes",
                                    i64::try_from(resident_bytes).unwrap_or(i64::MAX),
                                )],
                            ),
                            fs_protocol::Residency::Resident { bytes } => t_args(
                                "files-residency-local",
                                &[arg("bytes", i64::try_from(bytes).unwrap_or(i64::MAX))],
                            ),
                        };
                        lines.push(t_args(
                            "files-property-on-device",
                            &[arg("state", residency)],
                        ));
                    }
                    if let Some(history) = &entry.history {
                        lines.push(t_args(
                            "files-property-history",
                            &[
                                arg(
                                    "versions",
                                    i64::try_from(history.total_versions).unwrap_or(i64::MAX),
                                ),
                                arg("conflicts", i64_of_usize(history.open_conflicts().count())),
                            ],
                        ));
                    }
                    self.show_collab_panel(CollabPanel {
                        title: t_args("files-properties-title", &[arg("name", entry.name)]),
                        summary: if entry.record_id.is_some() {
                            t("files-properties-collab-summary")
                        } else {
                            t("files-properties-summary")
                        },
                        lines,
                        action: None,
                    });
                }
                true
            }
            FileCommand::ShowAvailability => {
                let entry = self
                    .selection
                    .primary()
                    .and_then(|index| self.displayed_entries().get(index).cloned());
                if let Some(entry) = entry
                    && let Some(availability) = entry.availability
                {
                    let residency = match availability.residency {
                        fs_protocol::Residency::NotResident => t("files-not-stored"),
                        fs_protocol::Residency::Partial { resident_bytes } => t_args(
                            "files-partly-stored",
                            &[arg(
                                "bytes",
                                i64::try_from(resident_bytes).unwrap_or(i64::MAX),
                            )],
                        ),
                        fs_protocol::Residency::Resident { bytes } => t_args(
                            "files-stored",
                            &[arg("bytes", i64::try_from(bytes).unwrap_or(i64::MAX))],
                        ),
                    };
                    let policy = match (availability.policy, availability.policy_inherited) {
                        (fs_protocol::AvailabilityPolicy::Pinned, true) => {
                            t("files-kept-offline-parent")
                        }
                        (fs_protocol::AvailabilityPolicy::Pinned, false) => t("files-kept-offline"),
                        (fs_protocol::AvailabilityPolicy::Automatic, _) => {
                            t("files-automatic-storage")
                        }
                    };
                    let replicas = match availability.replicas {
                        fs_protocol::ProjectedReplicaEvidence::Unknown => {
                            t("files-replica-unverified")
                        }
                        fs_protocol::ProjectedReplicaEvidence::Known { reachable, durable } => {
                            t_args(
                                "files-replica-counts",
                                &[
                                    arg("reachable", i64::from(reachable)),
                                    arg("durable", i64::from(durable)),
                                ],
                            )
                        }
                    };
                    let transfer = if availability.unavailable() {
                        Some(t("files-unavailable-offline"))
                    } else {
                        availability.transfer.last_error.clone()
                    };
                    let transfer = transfer.unwrap_or_else(|| {
                        t_args(
                            "files-transfer-state",
                            &[arg(
                                "state",
                                transfer_phase_label(availability.transfer.phase),
                            )],
                        )
                    });
                    self.show_collab_panel(CollabPanel {
                        title: t_args("files-availability-title", &[arg("name", entry.name)]),
                        summary: residency,
                        lines: vec![policy, replicas, transfer],
                        action: None,
                    });
                }
                true
            }
            FileCommand::Share => {
                let entry = self
                    .selection
                    .primary()
                    .and_then(|index| self.displayed_entries().get(index).cloned());
                let Some(entry) = entry else {
                    self.status = Some(t("files-select-collaborative-file"));
                    return true;
                };
                if crate::model::workspace_root_for(&entry.path, &self.workspace_roots).is_none() {
                    self.status = Some(t("files-share-needs-workspace"));
                    return true;
                }
                match self.share_invite.ask() {
                    Ok(()) => {
                        self.status = Some(t("files-share-opening"));
                        self.structure_dirty = true;
                    }
                    Err(reason) => self.status = Some(reason),
                }
                true
            }
            FileCommand::ShowHistory | FileCommand::ToggleHistoryCompare => {
                self.refresh_framework_history_target();
                self.close_collab_panel();
                self.framework_history.open(None);
                true
            }
            FileCommand::SelectHistoryVersion { version_id } => {
                self.refresh_framework_history_target();
                self.close_collab_panel();
                self.framework_history.open(Some(version_id));
                true
            }
            FileCommand::ShowInWorkspaceActivity {
                record_id,
                version_id,
            } => {
                let selected = self.displayed_entries().iter().find_map(|entry| {
                    let history = entry.history.as_ref()?;
                    if history.record_id.0.to_string() != record_id {
                        return None;
                    }
                    history
                        .versions
                        .iter()
                        .find(|version| version.id == version_id)
                        .cloned()
                        .map(|version| (version, entry.path.clone()))
                });
                let authors = self.trusted_humans.history_authors();
                let entry = selected.map(|(version, path)| {
                    let person = version.actor.person;
                    let identity = short_fingerprint(&version.actor.person.0);
                    let attribution =
                        crate::framework_history::version_attribution(&path, version.id);
                    let author = authors
                        .resolve_attributed_file_actor(
                            &version.actor.person.0,
                            &version.actor.device.0,
                            attribution.as_ref(),
                        )
                        .unwrap_or_else(|| {
                            ui::widget::HistoryAuthor::new(
                                self.trusted_humans.display_name_or_fingerprint(&person),
                                ui::history_source::stable_hue(&identity),
                                identity.clone(),
                            )
                        });
                    let action = t(crate::model::provenance_key(&version.provenance));
                    let resource_label = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("File")
                        .to_string();
                    let context = ui::tilo_actions::ActionContext::for_resource(
                        record_id.clone(),
                        "files.entry",
                        resource_label.clone(),
                    )
                    .with_app("so.tilo.file-explorer", "Files")
                    .with_facet(ui::tilo_actions::ActionFacet::new(
                        "version",
                        version.id.0.to_string(),
                    ));
                    ui::widget::HistoryEntry {
                        id: ui::widget::HistoryEntryId(*version_id.0.as_bytes()),
                        author,
                        // This is a signed immutable checkpoint, not a shell
                        // CRDT operation the generic Revert affordance can undo.
                        entry_kind: ui::widget::EntryKind::Compaction,
                        artifact_kind: ui::widget::ArtifactKind::File,
                        timestamp: UNIX_EPOCH
                            .checked_add(Duration::from_millis(version.timestamp_ms))
                            .unwrap_or(UNIX_EPOCH),
                        summary: format!("{action}: {resource_label}"),
                        text_delta: None,
                        diff_payload: None,
                        redaction: None,
                        context: Some(context),
                    }
                });
                self.host_intent = Some(ui::widget::AppHostIntent::ShowWorkspaceActivity {
                    resource_id: record_id,
                    version_id: Some(*version_id.0.as_bytes()),
                    entry,
                });
                self.close_collab_panel();
                true
            }
            FileCommand::RestoreVersion { version_id } => {
                // This intent only selects the version; the reviewed confirmation writes.
                self.refresh_framework_history_target();
                self.close_collab_panel();
                self.framework_history.open(Some(version_id));
                true
            }
            FileCommand::ReviewConflictMerge {
                conflict_id,
                version_id,
                other_device,
            } => {
                let entry = self
                    .selection
                    .primary()
                    .and_then(|index| self.displayed_entries().get(index).cloned());
                let Some(entry) = entry.filter(|entry| entry.history.is_some()) else {
                    self.status = Some(t("files-select-file-with-history"));
                    return true;
                };
                self.show_collab_panel(CollabPanel {
                    title: t_args("files-merge-review-title", &[arg("name", entry.name)]),
                    summary: t("files-merge-review-checking"),
                    lines: vec![
                        t_args(
                            "files-merge-review-incoming",
                            &[arg("device", short_fingerprint(&other_device))],
                        ),
                        t("files-merge-review-checking-detail"),
                    ],
                    action: None,
                });
                self.request_merge_preflight(entry.path, conflict_id, version_id);
                true
            }
            FileCommand::ResolveConflict {
                conflict_id,
                strategy,
                version_id,
            } => {
                if self.history_pending_path.is_some() {
                    self.status = Some(t("files-history-change-pending"));
                    return true;
                }
                let entry = self
                    .selection
                    .primary()
                    .and_then(|index| self.displayed_entries().get(index).cloned());
                let Some(entry) = entry.filter(|entry| entry.history.is_some()) else {
                    self.status = Some(t("files-select-file-with-history"));
                    return true;
                };
                let verb = match strategy {
                    fs_protocol::ConflictStrategy::KeepCurrent => t("files-keeping-current"),
                    fs_protocol::ConflictStrategy::UseOther => t("files-applying-other"),
                    fs_protocol::ConflictStrategy::KeepBoth => {
                        conflict_strategy_label(fs_protocol::ConflictStrategy::KeepBoth)
                    }
                    fs_protocol::ConflictStrategy::Merge => t("files-merging-versions"),
                };
                self.history_pending_path = Some(entry.path.clone());
                self.history_pending_success = Some(t("files-conflict-resolved-signed"));
                self.status = Some(verb);
                let result = Arc::clone(&self.history_result);
                std::thread::spawn(move || {
                    let outcome = crate::history_control::resolve_conflict(
                        &entry.path,
                        conflict_id,
                        strategy,
                        version_id,
                    );
                    if let Ok(mut slot) = result.lock() {
                        *slot = Some(outcome);
                    }
                });
                true
            }
            FileCommand::CloseCollabPanel => {
                if let Some(CollabPanelAction::ChooseNearbyDestination { handle }) = self
                    .collab_panel
                    .as_ref()
                    .and_then(|panel| panel.action.as_ref())
                {
                    self.nearby_send.decline(*handle);
                }
                self.close_collab_panel();
                true
            }
            FileCommand::SetView(m) => {
                self.set_view_mode(m);
                true
            }
            FileCommand::SetSort(by) => {
                self.set_sort_by(by);
                true
            }
            FileCommand::ToggleSortOrder => {
                self.toggle_sort_order();
                true
            }
            FileCommand::ShowKeyboardShortcuts => {
                self.show_keyboard_shortcuts_panel();
                true
            }
            FileCommand::SelectAll => {
                self.selection.select_all(self.displayed_entries().len());
                self.retain_shown_selection();
                self.last_click = None;
                true
            }
            FileCommand::OpenTerminal => {
                if let Err(error) = self.open_terminal() {
                    self.status =
                        Some(t_args("files-open-terminal-failed", &[arg("error", error)]));
                    return true;
                }
                false
            }
            FileCommand::NewFolder => self.apply_new_folder(),
            FileCommand::Rename => {
                self.begin_rename();
                true
            }
            FileCommand::RenameCommit => {
                self.commit_rename();
                true
            }
            FileCommand::RenameCancel => {
                self.cancel_rename();
                true
            }
            FileCommand::Delete => self.apply_delete(),
            FileCommand::ShowPathMenu { folders, at } => {
                self.open_path_menu(folders, at);
                true
            }
            FileCommand::Undo => {
                self.undo.request(ui::UndoRequest::Undo);
                true
            }
            FileCommand::RestoreFromTrash => self.request_restore_from_trash(),
            FileCommand::EmptyTrash => self.request_empty_trash_confirmation(),
            FileCommand::Cloud(effect) => {
                self.perform(effect);
                true
            }
            FileCommand::Copy => self.apply_copy(),
            FileCommand::Cut => self.apply_cut(),
            FileCommand::Paste => {
                if self.can_paste_clipboard() {
                    self.perform(crate::effects::FilesEffect::Paste {
                        conflict: Some("ask".into()),
                    });
                }
                true
            }
            FileCommand::RecoverClipboardTransfers => {
                self.perform(crate::effects::FilesEffect::RecoverClipboardTransfers);
                true
            }
            FileCommand::ExtractHere => self.apply_extract_here(),
            FileCommand::ExtractCurrentArchive => self.apply_extract_current_archive(),
            FileCommand::ExtractTo => self.apply_extract_to(),
            FileCommand::CompressSelection(packing) => self.apply_compress_selection(packing),
            FileCommand::OpenNearbySend => {
                let selected = self.selected_paths();
                if selected.len() != 1 {
                    self.status = Some(t("files-nearby-select-one"));
                    return true;
                }
                self.nearby_send_path = selected.into_iter().next();
                self.status = Some(t("files-nearby-loading-devices"));
                self.nearby_send.open_picker();
                true
            }
            FileCommand::SendNearby { target } => {
                let Some(path) = self.nearby_send_path.clone().or_else(|| {
                    let mut selected = self.selected_paths().into_iter();
                    let only = selected.next()?;
                    selected.next().is_none().then_some(only)
                }) else {
                    self.status = Some(t("files-nearby-select-one"));
                    return true;
                };
                self.status = Some(match self.nearby_send.send(&path, target) {
                    Ok(()) => t("files-nearby-queueing"),
                    Err(error) => error,
                });
                true
            }
            FileCommand::ChooseNearbyDestination { handle } => {
                self.open_nearby_destination_picker(handle);
                true
            }
            FileCommand::DecideNearby {
                handle,
                destination,
                remember_for_session,
            } => {
                self.nearby_send
                    .decide(handle, Some(destination), remember_for_session);
                self.close_collab_panel();
                self.status = Some(t("files-nearby-accepted"));
                true
            }
            FileCommand::RetryNearby { handle } => {
                self.nearby_send.retry(handle);
                self.close_collab_panel();
                self.status = Some(t("files-nearby-retrying"));
                true
            }
            FileCommand::CancelNearby { handle } => {
                self.nearby_send.cancel(handle);
                self.status = Some(t("files-nearby-cancelled"));
                true
            }
            FileCommand::OpenTemplatesMenu => self.apply_open_templates_menu(),
            FileCommand::OpenShortcutsMenu => {
                self.open_shortcuts_submenu(self.last_cursor);
                true
            }
            FileCommand::RunShortcut { shortcut } => {
                let identities = self.selected_entry_identities();
                self.status = Some(match self.shortcut_menu.run(&shortcut, &identities) {
                    Ok(label) => t_args("files-shortcut-running", &[arg("shortcut", label)]),
                    Err(reason) => reason,
                });
                true
            }
            FileCommand::OpenWithMenu => {
                self.open_open_with_submenu(self.last_cursor);
                true
            }
            FileCommand::OpenWith { path, app_id } => self.apply_open_with(&path, &app_id),
            FileCommand::OpenWithOther => {
                self.open_app_chooser();
                true
            }
            FileCommand::RevealInFolder(path) => self.apply_reveal_in_folder(&path),
            FileCommand::ForgetRecent(path) => self.apply_forget_recent(&path),
            FileCommand::ShowRemoteAccount(id) | FileCommand::RecheckRemoteAccount(id) => {
                self.show_remote_account(&id)
            }
            FileCommand::Uninstall => self.uninstall_selected_apps(),
            FileCommand::OpenDrive(id) => {
                if let Err(error) = self.open_drive(&id) {
                    self.status = Some(t_args("files-drive-open-failed", &[arg("error", error)]));
                }
                true
            }
            FileCommand::NewFromTemplate(template) => self.apply_new_from_template(&template),
            FileCommand::ArmDrag { path, is_dir, at } => self.apply_arm_drag(path, is_dir, at),
            FileCommand::BeginDrag { path, is_dir } => self.apply_begin_drag(path, is_dir),
            FileCommand::AddFavorite(path) => self.apply_add_favorite(path),
            FileCommand::RemoveFavorite(path) => self.apply_remove_favorite(&path),
            FileCommand::ReorderFavorite { from, to } => self.apply_reorder_favorite(from, to),
            FileCommand::ZoomIn => {
                self.set_zoom(self.zoom_level + 1);
                true
            }
            FileCommand::ZoomOut => {
                self.set_zoom(self.zoom_level - 1);
                true
            }
            FileCommand::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                self.store_preferences();
                self.store_widths();
                self.refresh();
                true
            }
            FileCommand::OpenPath(p) => self.apply_open_path(p),
            FileCommand::OpenViewMenu => {
                self.open_view_menu();
                true
            }
            FileCommand::OpenSortMenu => {
                self.open_sort_menu();
                true
            }
            FileCommand::SetSortOrder(order) => self.apply_set_sort_order(order),
        }
    }

    fn apply_click(&mut self, index: usize, ctrl: bool, shift: bool) -> bool {
        if ctrl || shift {
            self.selection.apply_click(index, ctrl, shift);
            self.retain_shown_selection();
            self.last_click = None;
        } else {
            let sel = self.selection.selected();
            if sel.len() > 1 && sel.contains(&index) {
                self.drag_seed = self.selected_paths();
                self.collapse_on_release = Some(index);
                return true;
            }
            self.drag_seed = Vec::new();
            if let Some(path) = self.select(Some(index)) {
                self.open_path(path);
            }
        }
        true
    }

    fn release_click(&mut self) {
        if let Some(index) = self.collapse_on_release.take() {
            self.drag_seed.clear();
            if let Some(path) = self.select(Some(index)) {
                self.open_path(path);
            }
            self.structure_dirty = true;
        }
    }

    fn apply_context_select(&mut self, target: Option<usize>) -> bool {
        match target {
            Some(i) if !self.selection.is_selected(i) => self.selection.select_only(i),
            Some(_) => {}
            None => self.selection.clear(),
        }
        self.retain_shown_selection();
        self.last_click = None;
        true
    }

    fn apply_new_folder(&mut self) -> bool {
        self.queue(
            crate::file_ops::FileOp::NewFolder {
                parent: self.current_path.display().to_string(),
                name: t("files-new-folder-default-name"),
            },
            OpFollowUp::NewFolder,
        );
        true
    }

    fn apply_delete(&mut self) -> bool {
        let paths = self.selected_paths();
        if paths.iter().any(|path| self.is_native_trash_entry(path)) {
            self.status = Some(t("files-native-trash-delete-unavailable"));
            return true;
        }
        if self.is_applications_view() && paths.iter().any(|path| crate::applications::is_app(path))
        {
            // An app leaves through Uninstall, which asks first.
            return self.uninstall_selected_apps();
        }
        if self.is_recent_view() {
            for path in &paths {
                self.apply_forget_recent(path);
            }
            return true;
        }
        if !paths.is_empty() {
            if self.is_trash_view() {
                self.show_collab_panel(CollabPanel {
                    title: t_args(
                        "files-permanent-delete-title",
                        &[arg("count", i64_of_usize(paths.len()))],
                    ),
                    summary: t_args(
                        "files-permanent-delete-summary",
                        &[arg("count", i64_of_usize(paths.len()))],
                    ),
                    lines: vec![t("files-permanent-delete-loss")],
                    action: Some(CollabPanelAction::PermanentDelete(paths)),
                });
            } else {
                let count = paths.len();
                self.queue(
                    crate::file_ops::FileOp::Trash {
                        paths: paths
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect(),
                    },
                    OpFollowUp::Trash { count },
                );
            }
        }
        true
    }

    fn apply_permanent_delete(&mut self, paths: &[PathBuf]) -> bool {
        if paths.iter().any(|path| self.is_native_trash_entry(path)) {
            self.status = Some(t("files-native-trash-delete-unavailable"));
            return true;
        }
        match fileops::purge_entries(paths) {
            Ok(n) => {
                self.refresh();
                self.status = Some(t_args(
                    "files-permanently-deleted",
                    &[arg("count", i64_of_usize(n))],
                ));
            }
            Err(e) => {
                self.status = Some(t_args("files-delete-failed", &[arg("error", e)]));
            }
        }
        true
    }

    fn is_native_trash_entry(&self, path: &Path) -> bool {
        entry_rows::in_native_trash(path, self.native_trash_dir().as_deref())
    }

    fn native_trash_dir(&self) -> Option<PathBuf> {
        self.hosted_clipboard()
            .then(host_fs::trash::user_trash_dir)
            .flatten()
    }

    fn request_restore_from_trash(&mut self) -> bool {
        let paths = self.selected_paths();
        if paths.iter().any(|path| self.is_native_trash_entry(path)) {
            let [path] = paths.as_slice() else {
                self.status = Some(t("files-native-restore-select-one"));
                return true;
            };
            self.perform(crate::effects::FilesEffect::Restore {
                entry: crate::search_provider::entry_resource_id(path),
            });
            return true;
        }
        self.apply_restore_from_trash()
    }

    fn apply_restore_from_trash(&mut self) -> bool {
        let paths = self.selected_paths();
        if !paths.is_empty() {
            match fileops::restore_entries(&paths) {
                Ok(n) => {
                    self.refresh();
                    self.status = Some(t_args("files-restored", &[arg("count", i64_of_usize(n))]));
                }
                Err(e) => {
                    self.status = Some(t_args("files-restore-failed", &[arg("error", e)]));
                }
            }
        }
        true
    }

    fn request_empty_trash_confirmation(&mut self) -> bool {
        if self.has_native_trash_entries() {
            self.status = Some(t("files-native-trash-delete-unavailable"));
            return true;
        }
        if self.entries.is_empty() {
            self.status = Some(t("files-trash-already-empty"));
            return true;
        }
        self.show_collab_panel(CollabPanel {
            title: t("files-empty-trash-title"),
            summary: t_args(
                "files-empty-trash-summary",
                &[arg("count", i64_of_usize(self.entries.len()))],
            ),
            lines: vec![t("files-permanent-delete-loss")],
            action: Some(CollabPanelAction::EmptyTrash),
        });
        true
    }

    fn perform_empty_trash(&mut self) -> bool {
        if self.has_native_trash_entries() {
            self.status = Some(t("files-native-trash-delete-unavailable"));
            return true;
        }
        match fileops::empty_trash() {
            Ok(n) => {
                self.refresh();
                self.status = Some(if n == 0 {
                    t("files-trash-already-empty")
                } else {
                    t_args("files-emptied-trash", &[arg("count", i64_of_usize(n))])
                });
            }
            Err(e) => {
                self.status = Some(t_args("files-empty-trash-failed", &[arg("error", e)]));
            }
        }
        true
    }

    fn apply_new_from_template(&mut self, template: &Path) -> bool {
        self.perform(crate::effects::FilesEffect::NewFromTemplate {
            template: template_resource_id(&template.to_string_lossy()),
        });
        true
    }

    fn create_from_template(&mut self, template: &Path) -> Result<(), String> {
        match templates::create_from_template(template, &self.current_path) {
            Ok(path) => {
                self.note_recent(&path, crate::recent::RecentUse::Changed);
                if !self.cloud.active {
                    self.load_then(self.current_path.clone(), Then::Rename(path));
                }
                self.status = Some(t("files-created-file"));
                Ok(())
            }
            Err(error) => {
                self.status = Some(t_args(
                    "files-new-file-failed",
                    &[arg("error", error.clone())],
                ));
                Err(error)
            }
        }
    }

    fn apply_begin_drag(&mut self, path: PathBuf, is_dir: bool) -> bool {
        self.collapse_on_release = None;
        if self.pin_drag.is_some() {
            return true;
        }
        self.armed_drag = None;
        tracing::debug!(target: "file_explorer::drag", ?path, "drag begins");
        let seed = std::mem::take(&mut self.drag_seed);
        self.drag_paths = if seed.contains(&path) {
            seed
        } else {
            vec![path.clone()]
        };
        self.drag_origin_is_dir = is_dir;
        self.pin_drag = Some(path);
        self.drag_hover = None;
        if self
            .drag_paths
            .iter()
            .any(|path| crate::applications::is_app(path))
        {
            return true;
        }
        crate::external_drag::begin(self.drag_paths.clone());
        if ui::dnd::start_file_drag(&self.drag_paths) {
            tracing::debug!(
                target: "file_explorer::drag",
                paths = ?self.drag_paths,
                "carrying the drag out of this app as a client drag"
            );
            self.pin_drag = None;
            self.drag_paths.clear();
            self.drag_hover = None;
            self.armed_drag = None;
            crate::external_drag::end();
        }
        true
    }

    fn apply_add_favorite(&mut self, path: PathBuf) -> bool {
        let place = self.place_for(&path);
        let added = {
            let home = self.home_path.clone().unwrap_or_else(|| PathBuf::from("/"));
            let mut favs = self.favs.borrow_mut();
            favorites::add(&mut favs.favorites, path, &home, &self.user_dirs)
        };
        if added {
            self.refresh_places_search();
            self.status = Some(t("files-added-favourite"));
            // A place is a path or a stored place's name, never blank.
            if let Some(place) = ui::tilo_resource::NonBlank::parse(&place) {
                self.submit(
                    crate::resource::FileExplorer::ADD_BOOKMARK
                        .with(crate::resource::file_explorer::AddBookmark { place, at: None }),
                );
            }
        }
        true
    }

    fn apply_remove_favorite(&mut self, path: &Path) -> bool {
        let removed = {
            let mut favs = self.favs.borrow_mut();
            favorites::remove(&mut favs.favorites, path)
        };
        if removed {
            self.refresh_places_search();
            self.status = Some(t("files-removed-favourite"));
            self.submit(crate::resource::FileExplorer::REMOVE_BOOKMARK.with(self.place_for(path)));
        }
        true
    }

    fn apply_reorder_favorite(&mut self, from: usize, to: usize) -> bool {
        let moved = self
            .favs
            .borrow()
            .favorites
            .get(from)
            .filter(|_| from != to)
            .map(|favorite| favorite.path.clone());
        let Some(moved) = moved else {
            return true;
        };
        {
            let mut favs = self.favs.borrow_mut();
            favorites::reorder(&mut favs.favorites, from, to);
        }
        self.refresh_places_search();
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(
                crate::resource::FileExplorer::paths()
                    .places
                    .move_to(&crate::resource::bookmark_id(&self.place_for(&moved)), to),
            );
        }
        true
    }

    fn apply_activate(&mut self, index: usize) -> bool {
        let disp = self.displayed_entries();
        if let Some(e) = disp.get(index) {
            self.open_path(e.path.clone());
        }
        true
    }

    fn apply_sort_column(&mut self, col: usize) -> bool {
        let by = match col {
            1 => SortBy::Name,
            2 => SortBy::Size,
            3 => SortBy::Modified,
            _ => return false,
        };
        if self.sort_by == by {
            self.toggle_sort_order();
        } else {
            self.set_sort_by(by);
        }
        true
    }

    fn apply_copy(&mut self) -> bool {
        if matches!(self.services, FileServices::Hosted(_)) || self.hosted_clipboard() {
            self.perform(crate::effects::FilesEffect::CopySelection);
            return true;
        }
        self.clipboard = self.selected_paths();
        self.clipboard_cut = false;
        true
    }

    fn apply_cut(&mut self) -> bool {
        if matches!(self.services, FileServices::Hosted(_)) || self.hosted_clipboard() {
            self.perform(crate::effects::FilesEffect::CutSelection);
            return true;
        }
        self.clipboard = self.selected_paths();
        self.clipboard_cut = true;
        true
    }

    fn apply_open_templates_menu(&mut self) -> bool {
        self.open_templates_submenu(self.last_cursor);
        true
    }

    fn apply_arm_drag(&mut self, path: PathBuf, is_dir: bool, at: Point) -> bool {
        tracing::debug!(target: "file_explorer::drag", ?path, ?at, "drag armed");
        self.armed_drag = Some((at, path, is_dir));
        true
    }

    fn apply_open_path(&mut self, p: PathBuf) -> bool {
        if p.is_file() {
            self.open_path(p);
        } else {
            self.navigate(p);
        }
        true
    }

    fn apply_set_sort_order(&mut self, order: SortOrder) -> bool {
        if self.sort_order != order {
            self.toggle_sort_order();
        }
        true
    }

    fn set_zoom(&mut self, level: i32) {
        let level = config::clamp_zoom(level);
        if level != self.zoom_level {
            self.zoom_level = level;
            self.store_preferences();
            self.store_widths();
        }
    }

    pub fn zoom_level(&self) -> i32 {
        self.zoom_level
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub fn favorites(&self) -> Vec<crate::favorites::Favorite> {
        self.favs.borrow().favorites.clone()
    }

    #[cfg(test)]
    pub(crate) fn favorites_store(&self) -> FavStore {
        self.favs.clone()
    }

    #[doc(hidden)]
    pub fn debug_show_preview(&mut self) {
        let text = |entry: &Entry| {
            entry.path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("txt")
            })
        };
        if let Some(i) = self.displayed_entries().iter().position(text) {
            self.select(Some(i));
            self.show_preview();
        }
    }

    #[doc(hidden)]
    pub fn debug_show_clash(&mut self) {
        let files: Vec<PathBuf> = self
            .displayed_entries()
            .iter()
            .filter(|entry| !entry.is_dir)
            .take(3)
            .map(|entry| entry.path.clone())
            .collect();
        self.clash = Some(PendingClash::new(
            files.clone(),
            files,
            self.current_path.join("Documents"),
            true,
        ));
    }

    #[doc(hidden)]
    pub fn debug_show_trash_notice(&mut self) {
        self.undo.publish(&ui::UndoStatus {
            can_undo: true,
            ..ui::UndoStatus::default()
        });
        self.notify_done(t_args("files-moved-to-trash", &[arg("count", 2)]), true);
    }

    #[doc(hidden)]
    pub fn debug_show_templates_menu(&mut self, at: Point) {
        self.last_cursor = at;
        self.open_context_menu(at);
        let anchor = Point::new(self.opened_menu_rect().x1 - 6.0, at.y + 66.0);
        self.open_templates_submenu(anchor);
    }

    fn opened_menu(&mut self) -> Option<&mut ContextMenu> {
        self.menus.iter_mut().rev().find_map(|change| match change {
            MenuChange::Main(menu, _) => Some(menu),
            _ => None,
        })
    }

    fn opened_menu_rect(&mut self) -> vello::kurbo::Rect {
        self.opened_menu()
            .map_or(vello::kurbo::Rect::ZERO, |menu| menu.get_rect())
    }

    #[doc(hidden)]
    pub fn debug_pin_dragging(&self) -> bool {
        self.pin_drag.is_some()
    }

    #[doc(hidden)]
    pub fn debug_begin_pin_drag(&mut self, path: PathBuf, at: Point) {
        self.pin_drag = Some(path.clone());
        self.drag_paths = vec![path];
        self.drag_origin_is_dir = true;
        self.last_cursor = at;
        crate::external_drag::begin(self.drag_paths.clone());
    }

    #[doc(hidden)]
    pub fn debug_begin_move_drag(&mut self, srcs: Vec<PathBuf>, over: PathBuf) -> bool {
        let rect = self
            .drop_targets
            .borrow()
            .iter()
            .find(|t| t.path == over)
            .map(|t| t.rect);
        let Some(rect) = rect else { return false };
        self.pin_drag = srcs.first().cloned();
        self.drag_paths = srcs;
        self.drag_hover = Some(over);
        self.last_cursor = Point::new(rect.x0 + 48.0, f64::midpoint(rect.y0, rect.y1));
        true
    }

    #[doc(hidden)]
    pub fn debug_folder_targets(&self) -> Vec<(PathBuf, vello::kurbo::Rect)> {
        self.drop_targets
            .borrow()
            .iter()
            .map(|t| (t.path.clone(), t.rect))
            .collect()
    }

    #[doc(hidden)]
    pub fn debug_up_button_rect(&self) -> Option<vello::kurbo::Rect> {
        self.toolbar.up_button_rect()
    }

    #[doc(hidden)]
    pub fn debug_select(&mut self, indices: &[usize]) {
        self.selection.clear();
        for (n, &i) in indices.iter().enumerate() {
            if n == 0 {
                self.selection.select_only(i);
            } else {
                self.selection.toggle(i);
            }
        }
        self.retain_shown_selection();
        self.structure_dirty = true;
    }

    #[doc(hidden)]
    pub fn debug_open_history_demo(&mut self) {
        self.refresh_framework_history_target();
        self.framework_history.open(None);
    }

    pub(crate) fn show_file_history_in(
        &mut self,
        source: ui::history_mode::checked_files::FileHistorySource,
    ) {
        self.framework_history = source;
        self.framework_history_binding = None;
        self.refresh_framework_history_target();
    }

    pub fn open_launch_argument(&mut self, argument: &str) {
        match std::path::absolute(argument) {
            Ok(path) if path.is_file() => {
                self.apply_reveal_in_folder(&path);
            }
            Ok(path) => self.navigate(path),
            Err(error) => self.set_external_status(error.to_string()),
        }
    }

    pub(crate) fn stage_on_next_sync(&mut self, scenario: Option<&str>) {
        self.staged = Some(StagedScenario(scenario.map(str::to_owned)));
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one table of the screenshot scenarios, an arm each"
    )]
    pub(crate) fn stage_scenario(&mut self, chrome: &mut Chrome, scenario: Option<&str>) {
        let rect = self.rect;
        self.wait_for_archive();
        match scenario {
            Some("archive-file-menu") => {
                if let Some(index) = self
                    .displayed_entries()
                    .iter()
                    .position(|entry| crate::archive::is_supported(&entry.path))
                {
                    self.debug_select(&[index]);
                }
                let at = Point::new(
                    rect.x0 + rect.width() * 0.45,
                    rect.y0 + rect.height() * 0.30,
                );
                self.last_cursor = at;
                self.open_context_menu(at);
                if let Some(menu) = self.opened_menu() {
                    menu.focus_first_action();
                }
            }
            Some("extract-progress") => self.debug_show_extract_progress(),
            Some("selection-actions") => self.stage_published_selection(),
            Some(name @ ("selection-sort" | "selection-sorted" | "selection-reversed")) => {
                self.stage_sort_selection(name);
            }
            Some(name @ ("recent" | "recent-menu" | "recent-revealed" | "recent-forgotten")) => {
                let home = self.current_path.clone();
                let target = home.join("Quarterly report.pdf");
                self.note_recent(&home.join("Notes.txt"), crate::recent::RecentUse::Opened);
                self.note_recent(&target, crate::recent::RecentUse::Opened);
                self.navigate(crate::recent::location());
                let row = crate::search_provider::entry_resource_id(&target);
                match name {
                    "recent-menu" => {
                        self.debug_select(&[0]);
                        self.open_context_menu(Point::new(
                            rect.x0 + rect.width() * 0.45,
                            rect.y0 + rect.height() * 0.34,
                        ));
                        if let Some(menu) = self.opened_menu() {
                            menu.focus_first_action();
                        }
                    }
                    "recent-revealed" => {
                        if let Err(error) = self
                            .run_effect(crate::effects::FilesEffect::RevealRecent { entry: row })
                        {
                            self.set_external_status(error);
                        }
                    }
                    "recent-forgotten" => {
                        if let Err(error) = self
                            .run_effect(crate::effects::FilesEffect::ForgetRecent { entry: row })
                        {
                            self.set_external_status(error);
                        }
                    }
                    _ => {}
                }
            }
            Some("grid") => self.set_view_mode(ViewMode::Grid),
            Some("thumbnails" | "thumbnails-fs") => {
                self.set_view_mode(ViewMode::Grid);
                self.prefetch_thumbnails();
            }
            Some("select") => {
                self.debug_select(&[9, 10]);
                self.staged_hover = Some(12);
                self.arm_status_for_shots();
            }
            Some("status") => self.arm_status_for_shots(),
            Some("rename") => {
                self.debug_select(&[11]);
                self.begin_rename();
            }
            Some("places") => chrome.show_places(),
            Some("remote-account") => {
                let account = crate::remote_accounts::RemoteAccount {
                    id: accounts::AccountId::from("shot-remote-account"),
                    kind: crate::remote_accounts::RemoteAccountKind::Nextcloud,
                    label: "Personal cloud".into(),
                    host: "cloud.example".into(),
                    health: crate::remote_accounts::RemoteAccountHealth::NeedsAuthentication,
                    files_mode: accounts::cloud_types::FilesMode::Sync,
                    mount: None,
                };
                self.show_remote_account_panel(&account, true);
            }
            Some("cloud-folder") => self.debug_show_cloud_folder(),
            Some("drives") => self.debug_show_sample_drives(),
            Some(name @ ("applications" | "applications-menu")) => {
                self.debug_show_sample_applications();
                if name == "applications-menu" {
                    if let Some(index) = self
                        .displayed_entries()
                        .iter()
                        .position(|entry| entry.name == "Chess")
                    {
                        self.debug_select(&[index]);
                    }
                    let at = Point::new(
                        rect.x0 + rect.width() * 0.45,
                        rect.y0 + rect.height() * 0.30,
                    );
                    self.last_cursor = at;
                    self.open_context_menu(at);
                    if let Some(menu) = self.opened_menu() {
                        menu.focus_first_action();
                    }
                }
            }
            Some("loading") => self.debug_show_loading_state(),
            Some("error") => self.debug_show_error_state(),
            Some("menu" | "archive-menu") => self.open_view_menu(),
            Some("undo-trash") => self.debug_show_trash_notice(),
            Some("clash") => self.debug_show_clash(),
            Some("preview") => self.debug_show_preview(),
            Some("context-menu") => {
                let at = Point::new(
                    rect.x0 + rect.width() * 0.45,
                    rect.y0 + rect.height() * 0.34,
                );
                self.debug_show_templates_menu(at);
                if let Some(menu) = self.opened_menu() {
                    menu.focus_first_action();
                }
            }
            Some("reorder") => {
                self.debug_begin_favorite_drag(chrome, 4, 1);
            }
            Some("pindrag") => {
                let folder = self.current_path.join("Development");
                self.debug_begin_pin_drag(folder, Point::new(rect.x0 + 150.0, rect.y0 + 300.0));
            }
            Some("dropmove") => self.staged_move_drag = true,
            Some("history") => {
                self.debug_select(&[0]);
                self.debug_open_history_demo();
            }
            _ => {}
        }
        let folders: Vec<_> = self
            .displayed_entries()
            .iter()
            .filter(|entry| entry.is_dir)
            .map(|entry| (entry.path.clone(), entry.modified))
            .collect();
        self.folder_counts.settle(&folders, self.show_hidden);
    }

    fn stage_sort_selection(&mut self, name: &str) {
        self.set_view_mode(ViewMode::List);
        self.set_sort_by(SortBy::Name);
        self.apply_set_sort_order(SortOrder::Ascending);
        for filename in ["Notes.txt", "Recipes.md"] {
            if let Some(index) = self.entries.iter().position(|entry| entry.name == filename) {
                self.apply_command(FileCommand::Click {
                    index,
                    ctrl: true,
                    shift: false,
                });
            }
        }
        if name != "selection-sort" {
            self.set_sort_by(SortBy::Size);
        }
        if name == "selection-reversed" {
            self.apply_set_sort_order(SortOrder::Descending);
        }
    }

    fn stage_published_selection(&mut self) {
        use crate::effects::FilesEffect;
        let entries = self
            .displayed_entries()
            .iter()
            .filter(|entry| matches!(entry.name.as_str(), "Notes.txt" | "Recipes.md"))
            .map(Self::entry_resource_id)
            .collect();
        for effect in [
            FilesEffect::List,
            FilesEffect::SortName,
            FilesEffect::SortAscending,
            FilesEffect::SelectEntries { entries },
            FilesEffect::SortSize,
        ] {
            if let Err(error) = self.run_effect(effect) {
                self.set_external_status(error);
                return;
            }
        }
    }

    pub(crate) fn effect_statuses(&self) -> Vec<ui::framework::EffectStatus> {
        use crate::effects::FilesEffect;
        let mut statuses = Vec::new();
        if !self.hosted_clipboard() {
            statuses.push(Self::clipboard_unavailable(FilesEffect::CLIPBOARD_STATUS));
            statuses.push(Self::clipboard_unavailable(
                FilesEffect::RECOVER_CLIPBOARD_TRANSFERS,
            ));
            if matches!(self.services, FileServices::Hosted(_)) {
                statuses.extend([
                    Self::clipboard_unavailable(FilesEffect::COPY_SELECTION),
                    Self::clipboard_unavailable(FilesEffect::CUT_SELECTION),
                    Self::clipboard_unavailable(FilesEffect::COPY),
                    Self::clipboard_unavailable(FilesEffect::CUT),
                ]);
            }
        }
        if let Some(named) = self.selection_named() {
            statuses.push(ui::framework::EffectStatus::labelled(
                FilesEffect::COPY_SELECTION,
                Self::selection_label("files-copy-selected", &named),
            ));
            statuses.push(ui::framework::EffectStatus::labelled(
                FilesEffect::CUT_SELECTION,
                Self::selection_label("files-cut-selected", &named),
            ));
        } else {
            for id in [FilesEffect::COPY_SELECTION, FilesEffect::CUT_SELECTION] {
                statuses.push(ui::framework::EffectStatus::unavailable(
                    id,
                    "selection_empty",
                    t("files-selection-empty"),
                ));
            }
        }
        if self.selected_paths().is_empty() {
            statuses.push(ui::framework::EffectStatus::unavailable(
                FilesEffect::CLEAR_SELECTION,
                "selection_empty",
                t("files-selection-empty"),
            ));
        }
        let hosted_paste =
            matches!(self.services, FileServices::Hosted(_)) || self.hosted_clipboard();
        if !hosted_paste && self.clipboard.is_empty() {
            statuses.push(ui::framework::EffectStatus::unavailable(
                FilesEffect::PASTE,
                "clipboard_empty",
                "The clipboard is empty",
            ));
        }
        if self.history.is_empty() {
            statuses.push(ui::framework::EffectStatus::unavailable(
                FilesEffect::BACK,
                "no_previous_folder",
                "No folder before this one",
            ));
        }
        if self.forward.is_empty() {
            statuses.push(ui::framework::EffectStatus::unavailable(
                FilesEffect::FORWARD,
                "no_next_folder",
                "No folder after this one",
            ));
        }
        if self.current_path.parent().is_none() {
            statuses.push(ui::framework::EffectStatus::unavailable(
                FilesEffect::UP,
                "at_root",
                "This folder has no parent",
            ));
        }
        statuses.extend(self.cloud_statuses());
        if !self.is_trash_view() {
            statuses.push(ui::framework::EffectStatus::unavailable(
                FilesEffect::EMPTY_TRASH,
                "not_in_trash",
                "Empty Trash from the Trash folder",
            ));
        }
        statuses
    }

    fn prefetch_thumbnails(&self) {
        if let Some(ctx) = self.thumb_ctx() {
            crate::views::file_view::FileView::prefetch_thumbnails(
                &ctx,
                &self.displayed_entries(),
                &self.workspace_roots,
            );
        }
    }

    fn resolve_staged_move_drag(&mut self) {
        if !self.staged_move_drag {
            return;
        }
        let target = self.drop_targets.borrow().first().map(|t| t.path.clone());
        let source = self
            .entries
            .iter()
            .find(|entry| !entry.is_dir)
            .map(|entry| entry.path.clone());
        if let (Some(target), Some(source)) = (target, source) {
            self.staged_move_drag = false;
            self.debug_begin_move_drag(vec![source], target);
        }
    }

    fn refresh_framework_history_target(&mut self) {
        let entry = self
            .selection
            .primary()
            .and_then(|index| self.displayed_entries().get(index).cloned());
        let selected = entry.and_then(|entry| {
            let id = entry.history.as_ref()?.record_id;
            Some((entry.path, id, entry.name))
        });
        let identity = selected
            .as_ref()
            .map(|(path, record, _)| (path.clone(), *record));
        if identity == self.framework_history_binding {
            return;
        }
        let target = selected.and_then(|(path, record, label)| {
            crate::framework_history::target(
                &path,
                record,
                label,
                self.trusted_humans.history_authors(),
            )
            .ok()
        });
        self.framework_history_binding = if target.is_some() { identity } else { None };
        self.framework_history.set_target(target);
    }

    fn primary_path(&self) -> Option<PathBuf> {
        let disp = self.displayed_entries();
        self.selection
            .primary()
            .and_then(|i| disp.get(i).map(|e| e.path.clone()))
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        let disp = self.displayed_entries();
        self.selection
            .selected()
            .into_iter()
            .filter_map(|i| disp.get(i).map(|e| e.path.clone()))
            .collect()
    }

    fn folder_target_at(&self, chrome: &Chrome, pt: Point) -> Option<PathBuf> {
        for t in self.drop_targets.borrow().iter() {
            if t.rect.contains(pt) && !self.drag_paths.contains(&t.path) {
                return Some(t.path.clone());
            }
        }
        if let Some(rect) = self.toolbar.up_button_rect()
            && rect.contains(pt)
            && let Some(parent) = self.current_path.parent()
        {
            return Some(parent.to_path_buf());
        }
        if let Some((path, _)) = self.toolbar.crumb_at(pt)
            && path != self.current_path
            && !self.drag_paths.contains(&path)
        {
            return Some(path);
        }
        self.place_target_at(chrome, pt).map(|(path, _)| path)
    }

    fn follow_transfers(&mut self, now: Instant) -> bool {
        let running = crate::file_ops::active_transfers();
        let Some(transfer) = running.iter().find(|transfer| {
            transfer.started.is_some_and(|started| {
                now.saturating_duration_since(started) >= TRANSFER_SHOWN_AFTER
            })
        }) else {
            if running.is_empty() && self.notice.shows_progress() {
                self.notice.dismiss();
                self.structure_dirty = true;
                return true;
            }
            return false;
        };
        self.show_transfer(transfer);
        true
    }

    fn show_transfer(&mut self, transfer: &crate::file_ops::LocalTransfer) {
        use std::sync::atomic::Ordering;
        let folder = Path::new(&transfer.folder).file_name().map_or_else(
            || transfer.folder.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        let title = t_args(
            match transfer.kind {
                crate::file_ops::TransferKind::Copy => "files-copying",
                crate::file_ops::TransferKind::Move => "files-moving",
                crate::file_ops::TransferKind::Extract => "files-extracting",
                crate::file_ops::TransferKind::Compress => "files-compressing",
            },
            &[
                arg("count", i64_of_usize(transfer.count)),
                arg("folder", folder),
            ],
        );
        let body = t_args(
            "files-transfer-progress",
            &[
                arg(
                    "done",
                    crate::i18n::format_bytes(transfer.done.load(Ordering::Relaxed)),
                ),
                arg(
                    "total",
                    crate::i18n::format_bytes(transfer.total.load(Ordering::Relaxed)),
                ),
            ],
        );
        self.notice.show_progress(title, body);
        self.structure_dirty = true;
    }

    fn spring_open_if_due(&mut self, now: Instant) -> bool {
        let due = self.pin_drag.is_some()
            && self
                .spring
                .as_ref()
                .is_some_and(|(_, since)| now.saturating_duration_since(*since) >= SPRING_OPEN);
        if !due {
            return false;
        }
        let Some((folder, _)) = self.spring.take() else {
            return false;
        };
        self.drag_hover = None;
        self.navigate(folder);
        self.structure_dirty = true;
        true
    }

    fn sheet_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        if let Some(preview) = &self.preview {
            return Some(
                AccessNode::new(Role::Dialog)
                    .with_name(preview.title().to_owned())
                    .with_description(preview.detail().to_owned())
                    .with_live(Politeness::Polite)
                    .with_state(AccessState::new().focusable(true).focused(true).modal(true)),
            );
        }
        if let Some(pending) = &self.clash {
            let buttons = PendingClash::choice_labels()
                .into_iter()
                .enumerate()
                .map(|(i, label)| {
                    AccessNode::new(Role::Button)
                        .with_name(label)
                        .with_action_target(format!("{CLASH_TARGET}{i}"))
                })
                .collect();
            return Some(
                AccessNode::new(Role::Dialog)
                    .with_name(pending.title())
                    .with_description(PendingClash::detail())
                    .with_live(Politeness::Assertive)
                    .with_state(AccessState::new().focusable(true).focused(true).modal(true))
                    .with_children(buttons),
            );
        }
        None
    }

    fn open_path_menu(&mut self, folders: Vec<(String, PathBuf)>, at: Point) {
        if folders.is_empty() {
            return;
        }
        let mut menu = self.new_overlay_menu();
        let mut commands = Vec::with_capacity(folders.len());
        for (label, path) in folders {
            menu.push(MenuItem::action(label, || {}));
            commands.push(Some(FileCommand::Navigate(path)));
        }
        menu.open_at([at.x as f32, at.y as f32]);
        layout_menu_widget(&mut menu);
        self.menus.push(MenuChange::Main(menu, commands));
        self.structure_dirty = true;
    }

    fn route_incoming_drop(&mut self, chrome: &Chrome, event: &winit::event::WindowEvent) -> bool {
        use winit::event::WindowEvent;
        match event {
            WindowEvent::HoveredFile(_) => {
                self.drag_hover = self.folder_target_at(chrome, self.last_cursor);
                self.structure_dirty = true;
                true
            }
            WindowEvent::HoveredFileCancelled => {
                self.drag_hover = None;
                self.structure_dirty = true;
                true
            }
            WindowEvent::DroppedFile(path) => {
                self.incoming.push(path.clone());
                self.structure_dirty = true;
                true
            }
            _ => false,
        }
    }

    fn take_incoming_drop(&mut self) {
        if self.incoming.is_empty() {
            return;
        }
        let paths = std::mem::take(&mut self.incoming);
        let target = self.drag_hover.take().or_else(|| {
            (!self.is_recent_view()
                && !self.is_applications_view()
                && !self.is_trash_view()
                && crate::archive::location(&self.current_path).is_none())
            .then(|| self.current_path.clone())
        });
        match target {
            Some(folder) => self.transfer(paths, &folder, true),
            None => self.status = Some(t("files-drop-nowhere")),
        }
    }

    fn show_preview(&mut self) {
        let Some(entry) = self
            .selection
            .primary()
            .and_then(|i| self.displayed_entries().get(i).cloned())
        else {
            self.preview = None;
            return;
        };
        self.preview = Some(Preview::of(&entry, entry.content_is_local()));
        self.structure_dirty = true;
    }

    fn route_preview_event(
        &mut self,
        event: &winit::event::WindowEvent,
        modifiers: winit::event::Modifiers,
    ) {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        use winit::keyboard::{Key, NamedKey};
        match event {
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                match &key.logical_key {
                    Key::Named(NamedKey::Space | NamedKey::Escape) => self.preview = None,
                    Key::Named(
                        NamedKey::ArrowUp
                        | NamedKey::ArrowDown
                        | NamedKey::ArrowLeft
                        | NamedKey::ArrowRight,
                    ) => {
                        let len = self.displayed_entries().len();
                        let forward = matches!(
                            key.logical_key,
                            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight)
                        );
                        if len > 0 {
                            let next = self.selection.primary().map_or(0, |i| {
                                if forward {
                                    (i + 1).min(len - 1)
                                } else {
                                    i.saturating_sub(1)
                                }
                            });
                            self.selection.move_to(next, modifiers.state().shift_key());
                            self.retain_shown_selection();
                            self.show_preview();
                        }
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.preview = None;
                        self.open_selected();
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self
                .preview
                .as_ref()
                .is_some_and(|preview| preview.closes_at(self.last_cursor)) =>
            {
                self.preview = None;
            }
            _ => {}
        }
        self.structure_dirty = true;
    }

    fn maybe_start_marquee(
        &mut self,
        chrome: &Chrome,
        event: &winit::event::WindowEvent,
        ctx: &ui::widget::EventCtx<'_>,
    ) {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return;
        };
        let at = self.last_cursor;
        let on_item = self
            .item_rects
            .borrow()
            .iter()
            .any(|(_, rect)| rect.contains(at));
        if *ctx.handled
            || on_item
            || self.pin_drag.is_some()
            || !chrome.content_body_rect().contains(at)
        {
            return;
        }
        let keep = ctx.modifiers.state().control_key() || ctx.modifiers.state().shift_key();
        let base = if keep {
            self.selection.selected()
        } else {
            Vec::new()
        };
        if !keep {
            self.selection.clear();
            self.retain_shown_selection();
        }
        self.marquee = Some((at, at, base));
        self.structure_dirty = true;
    }

    fn route_marquee_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        let Some((start, _, base)) = self.marquee.clone() else {
            return false;
        };
        match event {
            WindowEvent::CursorMoved { .. } => {
                let now = self.last_cursor;
                let area = vello::kurbo::Rect::from_points(start, now);
                let mut chosen: std::collections::BTreeSet<usize> = base.into_iter().collect();
                chosen.extend(
                    self.item_rects
                        .borrow()
                        .iter()
                        .filter(|(_, rect)| rect.intersect(area).area() > 0.0)
                        .map(|(index, _)| *index),
                );
                let primary = chosen.iter().next().copied();
                self.selection.replace_indices(chosen, primary);
                self.retain_shown_selection();
                if let Some(marquee) = self.marquee.as_mut() {
                    marquee.1 = now;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.marquee = None;
            }
            _ => return false,
        }
        *ctx.handled = true;
        self.structure_dirty = true;
        true
    }

    fn draw_marquee(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        theme: &ui::Theme,
    ) {
        let Some((start, now, _)) = &self.marquee else {
            return;
        };
        let area = vello::kurbo::Rect::from_points(*start, *now);
        let ink = theme.tokens.colors.border_focus;
        ui::paint::fill_rect(scene, transform, area, ui::paint::with_alpha(ink, 31));
        ui::paint::stroke_rect(scene, transform, area, ink, 1.0);
    }

    fn place_target_at(&self, chrome: &Chrome, pt: Point) -> Option<(PathBuf, vello::kurbo::Rect)> {
        if !chrome.in_places_list(pt) {
            return None;
        }
        let list = chrome.nav.sidebar_ref::<ui::widget::NavList>()?;
        list.visible_item_ids().into_iter().find_map(|id| {
            let row = list.row_rect(&id)?;
            let band = row.height() / 4.0;
            let inner = vello::kurbo::Rect::new(row.x0, row.y0 + band, row.x1, row.y1 - band);
            let path = places_sidebar::path_of_id(&id)?;
            let carrying_apps = self
                .drag_paths
                .iter()
                .any(|path| crate::applications::is_app(path));
            let takes = if carrying_apps {
                crate::fileops::is_trash_dir(&path)
            } else {
                path.is_dir()
                    && !self.drag_paths.contains(&path)
                    && !crate::fileops::is_trash_dir(&path)
            };
            (inner.contains(pt) && takes).then_some((path, row))
        })
    }

    fn finish_drag(&mut self, chrome: &Chrome, copy: bool) {
        crate::external_drag::end();
        let target = self.folder_target_at(chrome, self.last_cursor);
        let over_sidebar = chrome.nav.is_sidebar_open()
            && chrome
                .nav
                .geometry()
                .sidebar_column
                .contains(self.last_cursor);
        let paths = std::mem::take(&mut self.drag_paths);
        let origin = self.pin_drag.take();
        self.drag_hover = None;
        self.spring = None;
        if paths.iter().any(|path| crate::applications::is_app(path)) {
            if target.as_deref().is_some_and(crate::fileops::is_trash_dir) {
                self.uninstall_apps(&paths);
            }
        } else if let Some(dir) = target {
            self.drop_paths_into(&paths, &dir, copy);
        } else if over_sidebar {
            // Only one folder is pinnable: files and multi-selections are not.
            if paths.len() == 1
                && self.drag_origin_is_dir
                && let Some(path) = origin
            {
                self.apply_command(FileCommand::AddFavorite(path));
            }
        }
        self.drag_origin_is_dir = false;
    }

    fn cancel_drag(&mut self) {
        crate::external_drag::end();
        self.drag_paths.clear();
        self.pin_drag = None;
        self.drag_hover = None;
        self.spring = None;
        self.drag_origin_is_dir = false;
    }

    /// Move each of `paths` into `dir`, copying instead when the two are on different filesystems.
    fn drop_paths_into(&mut self, paths: &[PathBuf], dir: &std::path::Path, copy: bool) {
        let copy = copy || paths.iter().any(|src| !host_fs::same_filesystem(src, dir));
        let entries: Vec<PathBuf> = paths
            .iter()
            .filter(|src| {
                let already_here = src.parent() == Some(dir);
                let into_self = src.as_path() == dir;
                let into_descendant = dir.starts_with(src);
                !((already_here && !copy) || into_self || into_descendant)
            })
            .cloned()
            .collect();
        if entries.is_empty() {
            self.status = Some(t("files-nothing-to-move"));
            return;
        }
        self.transfer(entries, dir, copy);
    }

    fn transfer(&mut self, paths: Vec<PathBuf>, dir: &Path, copy: bool) {
        let clashing = clash::clashing(&paths, dir, copy);
        if clashing.is_empty() {
            self.run_transfer(&paths, dir, copy);
        } else {
            self.clash = Some(PendingClash::new(paths, clashing, dir.to_path_buf(), copy));
            self.structure_dirty = true;
        }
    }

    fn run_transfer(&mut self, paths: &[PathBuf], dir: &Path, copy: bool) {
        if paths.is_empty() {
            return;
        }
        let count = paths.len();
        let paths: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let folder = dir.display().to_string();
        if copy {
            self.queue(
                crate::file_ops::FileOp::Copy { paths, folder },
                OpFollowUp::Paste { count, cut: false },
            );
        } else {
            let destination = dir.file_name().map_or_else(
                || dir.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            self.queue(
                crate::file_ops::FileOp::Move {
                    paths,
                    folder,
                    create: false,
                },
                OpFollowUp::Move { destination },
            );
        }
    }

    fn settle_clash(&mut self, choice: Option<ClashChoice>) {
        let Some(pending) = self.clash.take() else {
            return;
        };
        self.structure_dirty = true;
        if pending.clipboard.is_some() {
            use ui::collaboration::file_clipboard::Conflict;
            let conflict = choice.map(|choice| match choice {
                ClashChoice::Skip => Conflict::Skip,
                ClashChoice::KeepBoth => Conflict::KeepBoth,
                ClashChoice::Replace => Conflict::Replace,
            });
            if let Some(result) = self.local_clipboard.decide(conflict) {
                self.complete_clipboard_publication(result);
            }
            return;
        }
        if pending.extracting.is_some() {
            use crate::file_ops::IfExists;
            let if_exists = match choice {
                None => return,
                Some(ClashChoice::Skip) => IfExists::Skip,
                Some(ClashChoice::KeepBoth) => IfExists::KeepBoth,
                Some(ClashChoice::Replace) => IfExists::Replace,
            };
            self.queue_extractions(&pending.sources, &pending.folder, if_exists);
            return;
        }
        match choice {
            None => {}
            Some(ClashChoice::KeepBoth) => {
                self.run_transfer(&pending.sources, &pending.folder, pending.copy);
            }
            Some(ClashChoice::Skip) => {
                self.run_transfer(&pending.unclashing(), &pending.folder, pending.copy);
            }
            Some(ClashChoice::Replace) => {
                let group = self
                    .dispatcher
                    .as_ref()
                    .map(ui::framework::Dispatcher::begin_group);
                self.queue(
                    crate::file_ops::FileOp::Trash {
                        paths: pending
                            .replaced()
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect(),
                    },
                    OpFollowUp::Replaced,
                );
                self.run_transfer(&pending.sources, &pending.folder, pending.copy);
                drop(group);
            }
        }
    }

    fn route_clash_event(&mut self, event: &winit::event::WindowEvent) {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        use winit::keyboard::{Key, NamedKey};
        let Some(pending) = self.clash.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CursorMoved { .. } => {
                self.structure_dirty |= pending.pointer_moved(self.last_cursor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => match pending.click(self.last_cursor) {
                Some(clash::ClashClick::Choose(choice)) => self.settle_clash(Some(choice)),
                Some(clash::ClashClick::Cancel) => self.settle_clash(None),
                None => {}
            },
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) => self.settle_clash(None),
                    Key::Named(NamedKey::Enter) => self.settle_clash(Some(ClashChoice::KeepBoth)),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn paste(&mut self) -> Result<Option<crate::file_ops::FileOp>, String> {
        if self.clipboard.is_empty() {
            return Err("The clipboard is empty".into());
        }
        let cut = self.clipboard_cut;
        let paths: Vec<String> = self
            .clipboard
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        let folder = self.current_path.display().to_string();
        let count = paths.len();
        let op = if cut {
            crate::file_ops::FileOp::Move {
                paths,
                folder,
                create: false,
            }
        } else {
            crate::file_ops::FileOp::Copy { paths, folder }
        };
        match self.queue(op.clone(), OpFollowUp::Paste { count, cut }) {
            Some(outcome) => outcome
                .reason()
                .map_or(Ok(None), |error| Err(format!("paste failed: {error}"))),
            None => Ok(Some(op)),
        }
    }

    fn open_selected(&mut self) {
        let paths = self.selected_paths();
        let files: Vec<PathBuf> = paths.iter().filter(|p| !p.is_dir()).cloned().collect();
        if paths.len() <= 1 || files.is_empty() {
            if let Some(p) = self.primary_path() {
                self.open_path(p);
            }
            return;
        }
        for file in files {
            self.open_path(file);
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        let _ = self.open_target(path);
    }

    fn open_target(&mut self, path: PathBuf) -> std::io::Result<bool> {
        if let Some(app) = crate::applications::app_id(&path) {
            self.open_app(app.to_owned());
            return Ok(true);
        }
        if let Some((archive, prefix)) = crate::archive::location(&path) {
            if prefix.as_os_str().is_empty() {
                self.navigate(path);
                return Ok(false);
            }
            let is_folder = crate::archive::cached(&archive).is_none_or(|entries| {
                !entries.iter().any(|entry| {
                    entry.path == prefix && entry.kind != crate::archive::Kind::Directory
                })
            });
            if is_folder {
                self.navigate(path);
                return Ok(false);
            }
            if let Some(copy) = archive_member_cached(&archive, &prefix) {
                return self.open_target(copy);
            }
            self.archive_members.start(archive, prefix);
            return Ok(true);
        }
        if path.is_dir() {
            self.navigate(path);
            return Ok(false);
        }
        if tilo_files::native::retained_home::contains(&path) {
            match Self::bring_into_workspace(&path) {
                Ok(copy) => self.ask_open(&copy, None),
                Err(reason) => self.notify_open_failed(&path, &reason),
            }
            return Ok(true);
        }
        self.ask_open(&path, None);
        Ok(true)
    }

    fn bring_into_workspace(path: &Path) -> Result<PathBuf, String> {
        let home = crate::model::user_home().ok_or_else(|| t("files-open-with-no-workspace"))?;
        fileops::import_mounted_into(path, &home)
    }

    fn ask_open(&mut self, path: &Path, app: Option<String>) {
        let request = self.open_request;
        let replies = Arc::clone(&self.open_replies);
        let shown = path.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name("files-open".into())
            .spawn(move || {
                let answer = request(&shown, app.as_deref());
                if let Ok(mut replies) = replies.lock() {
                    replies.push((shown, answer));
                }
            });
        if spawned.is_err() {
            self.notify_open_failed(path, "the window could not ask the session to open it");
        }
    }

    fn take_open_replies(&mut self) -> bool {
        let answered = self
            .open_replies
            .lock()
            .map(|mut replies| std::mem::take(&mut *replies))
            .unwrap_or_default();
        let changed = !answered.is_empty();
        for (path, answer) in answered {
            match answer {
                Ok(()) => self.note_recent(&path, crate::recent::RecentUse::Opened),
                Err(reason) => self.notify_open_failed(&path, &reason),
            }
        }
        changed
    }

    fn notify_open_failed(&mut self, path: &Path, reason: &str) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let other_app = self.displayed_entries().iter().any(|e| e.path == path)
            && !self.open_with_candidates(path).is_empty();
        self.notify_failure(
            t_args("files-open-failed", &[arg("name", name)]),
            reason,
            other_app.then(|| NoticeAction::OpenWith(path.to_path_buf())),
        );
    }

    pub fn status_line(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn notice_line(&self) -> Option<String> {
        self.notice.line()
    }

    pub fn is_renaming(&self) -> bool {
        self.renaming.is_some()
    }

    pub fn rename_buffer(&self) -> &str {
        &self.rename_buffer
    }

    #[cfg(test)]
    pub(crate) fn set_rename_buffer(&mut self, s: String) {
        self.rename_buffer = s;
        self.rename_seed_pending = true;
    }

    pub fn begin_rename(&mut self) {
        let disp = self.displayed_entries();
        if let Some(i) = self.selection.primary()
            && let Some(e) = disp.get(i)
        {
            self.renaming = Some(i);
            e.name.clone_into(&mut self.rename_buffer);
            self.rename_seed_pending = true;
            self.rename_select_pending = true;
            self.rename_input.set_focus(true);
            self.status = None;
        }
    }

    fn end_rename_editor(&mut self) {
        self.renaming = None;
        self.rename_seed_pending = false;
        self.rename_select_pending = false;
        self.rename_input.set_focus(false);
        self.rename_rect.set(None);
    }

    pub fn cancel_rename(&mut self) {
        self.end_rename_editor();
    }

    fn paint_background(&mut self, theme: &ui::Theme) {
        let _ = theme;
        if !self.composed_once && self.rect.width() > 0.0 && self.rect.height() > 0.0 {
            self.structure_dirty = true;
        }
    }

    fn empty_state_icon(&self) -> ui::icons::Icon {
        if self.is_recent_view() {
            ui::icons::Icon::History
        } else if self.is_applications_view() {
            ui::icons::Icon::LayoutGrid
        } else if self.is_trash_view() {
            ui::icons::Icon::Trash2
        } else {
            ui::icons::Icon::Folder
        }
    }

    fn collab_panel_spec<'a>(
        title: &'a str,
        actions: &'a [ui::widget::ModalAction<'a>],
        body_height: f64,
    ) -> ui::widget::ModalSpec<'a> {
        ui::widget::ModalSpec::new(title)
            .actions(actions)
            .body_height(body_height)
    }

    fn collab_panel_body_height(
        panel: &CollabPanel,
        tr: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        width: f32,
    ) -> f64 {
        let rhythm = &theme.tokens.spacing.rhythm;
        let style = theme.text(ui::TextRole::Body);
        let mut height = 0.0;
        let mut lines = panel.lines.iter().take(COLLAB_PANEL_MAX_LINES).peekable();
        if !panel.summary.is_empty() {
            let (_, summary) = tr.measure_style(&panel.summary, Some(width), &style);
            height += f64::from(summary);
            if lines.peek().is_some() {
                height += f64::from(rhythm.group);
            }
        }
        for (index, line) in lines.enumerate() {
            if index > 0 {
                height += f64::from(rhythm.related);
            }
            let (_, h) = tr.measure_style(line, Some(width), &style);
            height += f64::from(h);
        }
        height
    }

    fn draw_collab_panel(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
    ) {
        use ui::widget::modal_dialog;
        let Some(panel) = &self.collab_panel else {
            self.collab_layout = None;
            return;
        };
        let action_label = self.collab_panel_action_label();
        let hovered = |rect: vello::kurbo::Rect| rect.contains(self.last_cursor);
        let emphasis_safe = matches!(
            panel.action.as_ref(),
            Some(
                CollabPanelAction::ApplySafeMerge { .. }
                    | CollabPanelAction::ChooseNearbyDestination { .. }
                    | CollabPanelAction::RecheckAccount(_)
                    | CollabPanelAction::RetryNearby { .. }
            )
        );
        let mut actions = Vec::new();
        if let Some(label) = action_label.as_deref() {
            actions.push(if emphasis_safe {
                modal_dialog::ModalAction::primary(label)
            } else {
                modal_dialog::ModalAction::danger(label)
            });
        }
        let body_width = modal_dialog::body_width(self.rect, modal_dialog::MODAL_MAX_WIDTH, theme);
        let body_height =
            Self::collab_panel_body_height(panel, text_renderer, theme, body_width as f32);
        let spec = Self::collab_panel_spec(&panel.title, &actions, body_height);
        let layout = modal_dialog::layout(self.rect, &spec, text_renderer, theme);
        if let (Some(action), Some(rect)) = (actions.first_mut(), layout.actions.first()) {
            action.hovered = hovered(*rect);
        }
        let spec = Self::collab_panel_spec(&panel.title, &actions, body_height)
            .close_hovered(layout.close.is_some_and(hovered));
        modal_dialog::paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);

        let (ink, dim) = modal_dialog::ink(theme);
        let body = layout.body;
        let width = body.width() as f32;
        let text_style = theme.text(ui::TextRole::Body);
        let mut y = body.y0;
        if !panel.summary.is_empty() {
            let (_, summary_h) =
                text_renderer.measure_style(&panel.summary, Some(width), &text_style);
            text_renderer.draw_style(
                scene,
                &panel.summary,
                dim,
                transform * vello::kurbo::Affine::translate((body.x0, y)),
                Some(width),
                &text_style,
            );
            y += f64::from(summary_h) + f64::from(theme.tokens.spacing.rhythm.group);
        }
        for line in panel.lines.iter().take(COLLAB_PANEL_MAX_LINES) {
            let (_, line_h) = text_renderer.measure_style(line, Some(width), &text_style);
            if y + f64::from(line_h) > body.y1 + 0.5 {
                break;
            }
            text_renderer.draw_style(
                scene,
                line,
                ink,
                transform * vello::kurbo::Affine::translate((body.x0, y)),
                Some(width),
                &text_style,
            );
            y += f64::from(line_h) + f64::from(theme.tokens.spacing.rhythm.related);
        }
        modal_dialog::paint_actions(scene, transform, text_renderer, theme, &layout, &spec);
        self.collab_layout = Some(layout);
    }

    fn draw_rename_editor(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let Some(rect) = self.rename_rect.get().filter(|_| self.renaming.is_some()) else {
            self.rename_paint.skip();
            return;
        };
        self.rename_input.apply_layout_external(rect);
        let input = &mut self.rename_input;
        self.rename_paint.record(scene, transform, |scene| {
            input.draw_child(scene, transform, text_renderer, theme, presence);
            let reach = f64::from(theme.tokens.spacing.s2);
            let ring = input.get_rect().inflate(reach, reach);
            input.overhang().map_or(ring, |chip| ring.union(chip))
        });
    }

    fn draw_drop_highlight(
        &self,
        chrome: &Chrome,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        theme: &ui::Theme,
    ) {
        let Some(hover) = &self.drag_hover else {
            return;
        };
        let rect = self
            .drop_targets
            .borrow()
            .iter()
            .find(|t| &t.path == hover)
            .map(|t| t.rect)
            .or_else(|| {
                (Some(hover.as_path()) == self.current_path.parent())
                    .then(|| self.toolbar.up_button_rect())
                    .flatten()
            })
            .or_else(|| {
                self.toolbar
                    .crumb_at(self.last_cursor)
                    .filter(|(path, _)| path == hover)
                    .map(|(_, rect)| rect)
            })
            .or_else(|| {
                self.place_target_at(chrome, self.last_cursor)
                    .filter(|(path, _)| path == hover)
                    .map(|(_, row)| row)
            });
        let Some(rect) = rect else { return };
        let (ix, iy) = (
            f64::from(theme.tokens.spacing.s2),
            f64::from(theme.tokens.spacing.s1) / 2.0,
        );
        let face = vello::kurbo::Rect::new(rect.x0 + ix, rect.y0 + iy, rect.x1 - ix, rect.y1 - iy);
        ui::widget::paint_focus_ring(
            scene,
            transform,
            face,
            f64::from(theme.tokens.radii.item),
            theme,
        );
    }

    fn draw_pin_ghost(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
    ) {
        use vello::kurbo::{Affine, Rect};
        let Some(path) = &self.pin_drag else { return };
        let count = self.drag_paths.len().max(1);
        let glyph = ui::icons::icon_for_path(path, path.is_dir());
        let label = if count > 1 {
            t_args("files-drag-items", &[arg("count", i64_of_usize(count))])
        } else {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        };
        let tokens = &theme.tokens;
        let style = theme.text(ui::TextRole::Label);
        let (tw, th) = text_renderer.measure_style(&label, None, &style);
        let pad = f64::from(tokens.spacing.s3);
        let icon = f64::from(tokens.metrics.icon_sm);
        let gap = f64::from(tokens.spacing.s2);
        let h = (f64::from(th) + 2.0 * f64::from(tokens.spacing.s2))
            .max(ui::widget::ControlSize::Small.height(theme));
        let w = pad + icon + gap + f64::from(tw) + pad;
        let x0 = self.last_cursor.x + f64::from(tokens.spacing.s3);
        let y0 = self.last_cursor.y + f64::from(tokens.spacing.s2);
        let rect = Rect::new(x0, y0, x0 + w, y0 + h);
        ui::widget::floating_surface::paint(scene, transform, rect, theme);
        let iy = y0 + (h - icon) / 2.0;
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(x0 + pad, iy, x0 + pad + icon, iy + icon),
            glyph,
            tokens.colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let ty = ui::text_metrics::centered_text_top(y0, h, f64::from(th));
        text_renderer.draw_style(
            scene,
            &label,
            tokens.colors.text_primary,
            transform * Affine::translate((x0 + pad + icon + gap, ty)),
            None,
            &style,
        );
    }

    pub fn commit_rename(&mut self) -> bool {
        let Some(idx) = self.renaming else {
            return false;
        };
        let disp = self.displayed_entries();
        let Some(entry) = disp.get(idx) else {
            self.end_rename_editor();
            return false;
        };
        let path = entry.path.clone();
        let new_name = self.rename_buffer.trim().to_string();
        let unchanged = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|cur| cur == new_name);
        if new_name.is_empty() || unchanged {
            self.end_rename_editor();
            return false;
        }
        self.queue(
            crate::file_ops::FileOp::Rename {
                path: path.display().to_string(),
                name: new_name,
            },
            OpFollowUp::Rename,
        );
        true
    }
}

fn layout_menu_widget(menu: &mut ContextMenu) {
    if menu.is_open() {
        menu.set_text_scale(ui::Theme::dark().text_scale);
        menu.layout_at_position();
    }
}

fn merge_supported_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "txt" | "qdoc" | "tilo-doc"
            )
        })
}

impl FilesState {
    pub fn terminal_intent(&self) -> runtime_portal::tools::ToolIntent {
        use runtime_portal::tools::ToolIntent;
        let directory = self
            .current_path
            .to_str()
            .map(str::to_owned)
            .filter(|directory| {
                ToolIntent::Terminal {
                    directory: Some(directory.clone()),
                }
                .validate()
                .is_ok()
            });
        ToolIntent::Terminal { directory }
    }

    pub fn open_terminal(&self) -> Result<(), String> {
        #[cfg(test)]
        crate::model::fixture_io_guard::ambient_io("open_terminal");
        let on_behalf_of = ui::invocation_scope::delivered_invocation().map(|id| id.0.into_bytes());
        runtime_portal::tools::open_tool(self.terminal_intent(), on_behalf_of)
    }

    pub fn open_entry(&mut self, idx: usize) -> std::io::Result<bool> {
        let disp = self.displayed_entries();
        let Some(entry) = disp.get(idx) else {
            return Ok(false);
        };
        let path = entry.path.clone();
        self.open_target(path)
    }

    pub fn available_templates(&self) -> Vec<PathBuf> {
        self.templates.templates()
    }

    fn has_native_trash_entries(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| self.is_native_trash_entry(&entry.path))
    }

    fn is_trash_view(&self) -> bool {
        fileops::is_trash_dir(&self.current_path)
            || (self.hosted_clipboard()
                && host_fs::trash::user_trash_dir().as_ref() == Some(&self.current_path))
    }

    fn single_selected_dir(&self) -> Option<PathBuf> {
        let disp = self.displayed_entries();
        let sel = self.selection.selected();
        if sel.len() != 1 {
            return None;
        }
        let idx = *sel.first()?;
        disp.get(idx).filter(|e| e.is_dir).map(|e| e.path.clone())
    }

    pub(crate) fn current_place(&self) -> Option<String> {
        (self.shown_is_folder && !self.is_trash_view()).then(|| self.place_for(&self.current_path))
    }

    pub(crate) fn current_is_favourite(&self) -> bool {
        self.favs
            .borrow()
            .favorites
            .iter()
            .any(|f| f.path == self.current_path)
    }

    fn current_is_removable_favourite(&self) -> bool {
        self.favs
            .borrow()
            .favorites
            .iter()
            .any(|f| !f.is_home && f.path == self.current_path)
    }

    fn command_checked(&self, command: Option<&FileCommand>) -> Option<bool> {
        match command? {
            FileCommand::SetSort(by) => Some(self.sort_by == *by),
            FileCommand::ToggleHidden => Some(self.show_hidden),
            _ => None,
        }
    }

    fn open_context_menu(&mut self, at: Point) {
        self.menus.push(MenuChange::CloseSub);
        let spec = self.context_menu_spec(&self.available_templates());
        let mut menu = self.new_overlay_menu();
        let mut cmds = Vec::with_capacity(spec.len());
        for (label, cmd) in spec {
            if cmd.is_none() && label.is_empty() {
                menu.push(MenuItem::separator());
                cmds.push(None);
            } else {
                // No-op action — the app maps the clicked index to a command
                // itself (the sink is !Send, so it can't live in the action).
                let mut item = MenuItem::action(label, || {});
                if let Some(keys) = cmd.as_ref().and_then(command_shortcut) {
                    item = item.with_shortcut(keys);
                }
                menu.push(match self.command_checked(cmd.as_ref()) {
                    Some(checked) => item.with_checked(checked),
                    None => item,
                });
                cmds.push(cmd);
            }
        }
        menu.set_about(self.shared_selection());
        menu.open_at([at.x as f32, at.y as f32]);
        layout_menu_widget(&mut menu);
        self.menus.push(MenuChange::Main(menu, cmds));
    }

    fn open_templates_submenu(&mut self, at: Point) {
        let tpls = self.available_templates();
        // The catalogue can change after its parent menu was opened.  Do not
        // leave a focusable, visually empty flyout behind when its last
        // template vanishes (and keep headless action-state shots truthful).
        if tpls.is_empty() {
            self.menus.push(MenuChange::CloseSub);
            return;
        }
        let mut menu = self.new_overlay_menu();
        let mut cmds = Vec::with_capacity(tpls.len());
        for t in tpls {
            menu.push(MenuItem::action(templates::menu_label(&t), || {}));
            cmds.push(Some(FileCommand::NewFromTemplate(t)));
        }
        menu.open_at([at.x as f32, at.y as f32]);
        layout_menu_widget(&mut menu);
        self.menus.push(MenuChange::Sub(menu, cmds));
    }

    fn open_shortcuts_submenu(&mut self, at: Point) {
        let identities = self.selected_entry_identities();
        let offered: Vec<(String, String)> = self
            .shortcut_menu
            .offered_on(&identities)
            .into_iter()
            .map(|offer| (offer.label().to_string(), offer.id().to_string()))
            .collect();
        if offered.is_empty() {
            self.menus.push(MenuChange::CloseSub);
            return;
        }
        let mut menu = self.new_overlay_menu();
        let mut cmds = Vec::with_capacity(offered.len());
        for (label, shortcut) in offered {
            menu.push(MenuItem::action(label, || {}));
            cmds.push(Some(FileCommand::RunShortcut { shortcut }));
        }
        menu.open_at([at.x as f32, at.y as f32]);
        layout_menu_widget(&mut menu);
        self.menus.push(MenuChange::Sub(menu, cmds));
    }

    fn handle_shortcut_event(&mut self, event: &ui::collaboration::CollabEvent) -> bool {
        let status = match self.shortcut_menu.handle_event(event) {
            Some(crate::shortcut_menu::ShortcutOutcome::Started { label }) => {
                Some(t_args("files-shortcut-started", &[arg("shortcut", label)]))
            }
            Some(crate::shortcut_menu::ShortcutOutcome::Refused { label, reason }) => Some(t_args(
                "files-shortcut-failed",
                &[arg("shortcut", label), arg("reason", reason)],
            )),
            None => return false,
        };
        if self.status == status {
            return false;
        }
        self.status = status;
        true
    }

    fn handle_share_event(&mut self, event: &ui::collaboration::CollabEvent) -> bool {
        let Some(answer) = self.share_invite.handle_event(event) else {
            return false;
        };
        let status = match &answer {
            Ok(()) => {
                self.answer_effect(Ok(tilo_actions::ActionOutput::default()));
                None
            }
            Err(reason) => {
                self.answer_effect(Err(tilo_actions::ActionFailure {
                    code: "files.share_failed".into(),
                    message: reason.clone(),
                    details: None,
                    retryable: false,
                }));
                Some(t_args(
                    "files-share-failed",
                    &[arg("reason", reason.clone())],
                ))
            }
        };
        self.status = status;
        true
    }

    fn offers_sharing(&self, entry: &crate::model::Entry) -> bool {
        crate::model::workspace_root_for(&entry.path, &self.workspace_roots).is_some()
    }

    fn open_nearby_device_picker(&mut self, targets: &[ui::collaboration::NearbyTransferTarget]) {
        let online = targets
            .iter()
            .filter(|target| target.online)
            .collect::<Vec<_>>();
        if online.is_empty() {
            self.status = Some(t("files-nearby-no-devices"));
            return;
        }
        self.menus.push(MenuChange::CloseSub);
        let mut menu = self.new_overlay_menu();
        let mut commands = Vec::with_capacity(online.len());
        for target in online {
            menu.push(MenuItem::action(
                t_args(
                    "files-nearby-send-to-device",
                    &[arg("device", target.label.clone())],
                ),
                || {},
            ));
            commands.push(Some(FileCommand::SendNearby {
                target: target.handle,
            }));
        }
        menu.open_at([self.last_cursor.x as f32, self.last_cursor.y as f32]);
        layout_menu_widget(&mut menu);
        self.menus.push(MenuChange::Main(menu, commands));
        self.status = Some(t("files-nearby-choose-device"));
    }

    fn open_nearby_destination_picker(
        &mut self,
        transfer: ui::collaboration::NearbyTransferHandle,
    ) {
        let destinations = self.nearby_send.destinations().to_vec();
        if destinations.is_empty() {
            self.status = Some(t("files-nearby-no-destinations"));
            return;
        }
        self.menus.push(MenuChange::CloseSub);
        let mut menu = self.new_overlay_menu();
        let mut commands = Vec::with_capacity(destinations.len().saturating_mul(2));
        for destination in destinations {
            menu.push(MenuItem::action(
                t_args(
                    "files-nearby-save-to-destination",
                    &[arg("destination", destination.label.clone())],
                ),
                || {},
            ));
            commands.push(Some(FileCommand::DecideNearby {
                handle: transfer,
                destination: destination.handle,
                remember_for_session: false,
            }));
            if !destination.remembered_for_session {
                menu.push(MenuItem::action(
                    t_args(
                        "files-nearby-save-and-remember",
                        &[arg("destination", destination.label)],
                    ),
                    || {},
                ));
                commands.push(Some(FileCommand::DecideNearby {
                    handle: transfer,
                    destination: destination.handle,
                    remember_for_session: true,
                }));
            }
        }
        menu.open_at([self.last_cursor.x as f32, self.last_cursor.y as f32]);
        layout_menu_widget(&mut menu);
        self.menus.push(MenuChange::Main(menu, commands));
        self.status = Some(t("files-nearby-choose-destination"));
    }

    fn open_menu_right_aligned(
        &mut self,
        spec: Vec<(String, Option<FileCommand>, Option<bool>)>,
        anchor: Point,
        flip_y: f64,
    ) {
        self.menus.push(MenuChange::CloseSub);
        let spec = tidy_groups(
            spec,
            |item| item.0.is_empty() && item.1.is_none(),
            || (String::new(), None, None),
        );
        let mut menu = self.new_overlay_menu();
        let mut cmds = Vec::with_capacity(spec.len());
        for (label, cmd, checked) in spec {
            if cmd.is_none() && label.is_empty() {
                menu.push(MenuItem::separator());
                cmds.push(None);
            } else {
                let mut item = MenuItem::action(label, || {});
                if let Some(keys) = cmd.as_ref().and_then(command_shortcut) {
                    item = item.with_shortcut(keys);
                }
                menu.push(match checked {
                    Some(checked) => item.with_checked(checked),
                    None => item,
                });
                cmds.push(cmd);
            }
        }
        menu.open_at([anchor.x as f32, anchor.y as f32]);
        layout_menu_widget(&mut menu);
        let menu_rect = menu.get_rect();
        let w = menu_rect.width();
        let h = menu_rect.height();
        let max_x = (self.rect.x1 - w).max(self.rect.x0);
        let x = (anchor.x - w).clamp(self.rect.x0, max_x);
        let y = if anchor.y + h <= self.rect.y1 {
            anchor.y
        } else {
            (flip_y - h).max(self.rect.y0)
        };
        menu.open_at([x as f32, y as f32]);
        layout_menu_widget(&mut menu);
        self.menus.push(MenuChange::Main(menu, cmds));
    }

    fn new_overlay_menu(&self) -> ContextMenu {
        let mut menu = ContextMenu::new();
        menu.set_viewport(Some(self.rect));
        menu
    }

    fn open_view_menu(&mut self) {
        let anchor = self.toolbar.view_menu_anchor();
        let flip_y = self
            .toolbar
            .view_menu_rect()
            .map_or(anchor.y, |rect| rect.y0 - 2.0);
        let mut spec: Vec<(String, Option<FileCommand>, Option<bool>)> = Vec::new();
        if self.toolbar.is_compact() {
            if self.toolbar.can_go_forward() {
                spec.push((t("files-forward"), Some(FileCommand::Forward), None));
                spec.push((String::new(), None, None));
            }
            spec.push((
                t("files-view-list"),
                Some(FileCommand::SetView(ViewMode::List)),
                Some(self.view_mode == ViewMode::List),
            ));
            spec.push((
                t("files-view-grid"),
                Some(FileCommand::SetView(ViewMode::Grid)),
                Some(self.view_mode == ViewMode::Grid),
            ));
            spec.push((String::new(), None, None));
            spec.extend(self.sort_menu_spec(true));
            spec.push((String::new(), None, None));
        }
        if crate::archive::location(&self.current_path).is_some() {
            spec.push((
                t("files-archive-extract-here"),
                Some(FileCommand::ExtractCurrentArchive),
                None,
            ));
            spec.push((
                t("files-archive-extract-to"),
                Some(FileCommand::ExtractTo),
                None,
            ));
            self.open_menu_right_aligned(spec, anchor, flip_y);
            return;
        }
        spec.push((
            t("files-menu-new-folder"),
            Some(FileCommand::NewFolder),
            None,
        ));
        if !self.available_templates().is_empty() {
            spec.push((
                t("files-menu-new-file"),
                Some(FileCommand::OpenTemplatesMenu),
                None,
            ));
        }
        if self.undo.can_undo() {
            spec.push((t("files-menu-undo"), Some(FileCommand::Undo), None));
        }
        spec.push((String::new(), None, None));
        spec.push((
            t("files-menu-show-hidden"),
            Some(FileCommand::ToggleHidden),
            Some(self.show_hidden),
        ));
        if self.is_trash_view() && !self.has_native_trash_entries() {
            spec.push((
                t("files-menu-empty-trash"),
                Some(FileCommand::EmptyTrash),
                None,
            ));
        }
        spec.push((
            t("files-menu-open-terminal"),
            Some(FileCommand::OpenTerminal),
            None,
        ));
        spec.push((String::new(), None, None));
        spec.push((
            t("files-shortcuts-title"),
            Some(FileCommand::ShowKeyboardShortcuts),
            None,
        ));
        self.open_menu_right_aligned(spec, anchor, flip_y);
    }

    fn open_sort_menu(&mut self) {
        let anchor = self.toolbar.sort_menu_anchor();
        let flip_y = self
            .toolbar
            .sort_menu_rect()
            .map_or(anchor.y, |rect| rect.y0 - 2.0);
        let spec = self.sort_menu_spec(false);
        self.open_menu_right_aligned(spec, anchor, flip_y);
    }

    fn sort_menu_spec(&self, inline: bool) -> Vec<(String, Option<FileCommand>, Option<bool>)> {
        let mut spec: Vec<(String, Option<FileCommand>, Option<bool>)> = Vec::new();
        let key = |bare: &str, inline_label: &str| t(if inline { inline_label } else { bare });
        for (label, by) in [
            (key("files-sort-name", "files-menu-sort-name"), SortBy::Name),
            (key("files-sort-size", "files-menu-sort-size"), SortBy::Size),
            (key("files-sort-type", "files-menu-sort-type"), SortBy::Type),
            (
                key("files-sort-date", "files-menu-sort-date"),
                SortBy::Modified,
            ),
        ] {
            spec.push((
                label,
                Some(FileCommand::SetSort(by)),
                Some(self.sort_by == by),
            ));
        }
        spec.push((String::new(), None, None));
        let asc = self.sort_order == SortOrder::Ascending;
        spec.push((
            t("files-sort-ascending"),
            Some(FileCommand::SetSortOrder(SortOrder::Ascending)),
            Some(asc),
        ));
        spec.push((
            t("files-sort-descending"),
            Some(FileCommand::SetSortOrder(SortOrder::Descending)),
            Some(!asc),
        ));
        spec
    }

    fn route_menu_event(
        &mut self,
        chrome: &mut Chrome,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let submenu = chrome.submenu.is_open();
        if submenu
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && !chrome
                .submenu
                .contains([self.last_cursor.x, self.last_cursor.y])
        {
            chrome.submenu.close();
            chrome.context_menu.close();
            return true;
        }
        let mut menu_handled = false;
        let selected = {
            let menu = if submenu {
                &mut chrome.submenu
            } else {
                &mut chrome.context_menu
            };
            menu.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut menu_handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            menu.take_selection()
        };
        if !menu_handled {
            return false;
        }
        let Some(index) = selected else {
            return true;
        };
        let command = if submenu {
            chrome.submenu_commands.get(index).cloned().flatten()
        } else {
            chrome.menu_commands.get(index).cloned().flatten()
        };
        let rect = chrome.context_menu.get_rect();
        let row = chrome
            .context_menu
            .item_rect(index)
            .map_or(rect.y0 + 28.0, |row| row.y0);
        let beside = Point::new(rect.x1 - 6.0, row);
        match command {
            Some(FileCommand::OpenTemplatesMenu) => self.open_templates_submenu(beside),
            Some(FileCommand::OpenShortcutsMenu) => self.open_shortcuts_submenu(beside),
            Some(FileCommand::OpenWithMenu) => self.open_open_with_submenu(beside),
            command => {
                chrome.submenu.close();
                chrome.context_menu.close();
                if let Some(command) = command {
                    self.apply_command(command);
                }
            }
        }
        true
    }

    fn keyboard_context_anchor(&self, chrome: &Chrome) -> Point {
        fn find_named_rect(widget: &dyn Widget, name: &str) -> Option<vello::kurbo::Rect> {
            if let Some((_, rect)) = widget
                .test_rects()
                .into_iter()
                .find(|(candidate, _)| candidate == name)
            {
                return Some(rect);
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| find_named_rect(child, name))
        }

        let row_name = self.selection.primary().and_then(|index| {
            self.displayed_entries()
                .get(index)
                .map(|entry| format!("file_row:{}", entry.name))
        });
        if let Some(rect) = row_name
            .as_deref()
            .and_then(|name| find_named_rect(&chrome.nav, name))
        {
            return Point::new(rect.x0 + 24.0, rect.y0 + 24.0);
        }
        Point::new(self.rect.x0 + 24.0, self.rect.y0 + 72.0)
    }

    pub(crate) fn opener(&self) -> impl FnMut(&str) + 'static {
        let opened = Rc::clone(&self.opened);
        move |anchor| opened.borrow_mut().push(anchor.to_owned())
    }

    pub(crate) fn open_anchor(&mut self, anchor: &str) -> Result<(), String> {
        use crate::effects::FilesEffect;
        let effect = if anchor.starts_with("files.place:") {
            FilesEffect::OpenItem {
                item: anchor.to_owned(),
            }
        } else if let Some(record) = anchor.strip_prefix("files.record:") {
            FilesEffect::Reveal {
                record: record.to_owned(),
            }
        } else if let Some(account) = crate::remote_accounts::account_of_resource(anchor) {
            FilesEffect::AccountDetails { account }
        } else {
            return Err(format!("Files opens no `{anchor}`"));
        };
        self.run_effect(effect).map(drop)
    }

    fn drain_opened(&mut self) -> bool {
        let opened: Vec<String> = self.opened.borrow_mut().drain(..).collect();
        for anchor in &opened {
            if let Err(error) = self.open_anchor(anchor) {
                self.set_external_status(error);
            }
        }
        !opened.is_empty()
    }

    fn drain_commands(&mut self) -> bool {
        let queued: Vec<FileCommand> = self.cmds.borrow_mut().drain(..).collect();
        let mut dirty = false;
        for cmd in queued {
            dirty |= self.apply_command(cmd);
        }
        dirty
    }

    fn request_merge_preflight(
        &mut self,
        path: PathBuf,
        conflict_id: uuid::Uuid,
        version_id: fs_protocol::FileVersionId,
    ) {
        self.merge_preflight_pending = Some((conflict_id, version_id));
        let result = Arc::clone(&self.merge_preflight_result);
        std::thread::spawn(move || {
            let outcome = crate::model::read_merge_preflight(&path, conflict_id)
                .ok_or_else(|| t("files-merge-review-unavailable"));
            *result.lock_recover() = Some((conflict_id, version_id, outcome));
        });
    }

    fn poll_merge_preflight(&mut self) -> bool {
        let result = self
            .merge_preflight_result
            .lock()
            .ok()
            .and_then(|mut result| result.take());
        let Some((conflict_id, version_id, result)) = result else {
            return false;
        };
        if self.merge_preflight_pending != Some((conflict_id, version_id)) {
            return false;
        }
        self.merge_preflight_pending = None;
        let Some(panel) = self.collab_panel.as_mut() else {
            return false;
        };
        match result {
            Ok(preflight) if preflight.safe => {
                panel.summary = t("files-merge-review-safe");
                panel.lines = vec![
                    preflight.explanation,
                    preflight.diff_summary,
                    t("files-merge-review-undo"),
                ];
                panel.action = Some(CollabPanelAction::ApplySafeMerge {
                    conflict_id,
                    version_id,
                });
            }
            Ok(preflight) => {
                panel.summary = t("files-merge-review-needs-review");
                panel.lines = vec![
                    preflight.explanation,
                    preflight.diff_summary,
                    t("files-merge-review-no-apply"),
                ];
                panel.action = None;
            }
            Err(error) => {
                panel.summary = t("files-merge-review-needs-review");
                panel.lines = vec![error, t("files-merge-review-no-apply")];
                panel.action = None;
            }
        }
        self.structure_dirty = true;
        true
    }

    fn poll_history_result(&mut self) -> bool {
        let preview_changed = self.poll_merge_preflight();
        let result = self
            .history_result
            .lock()
            .ok()
            .and_then(|mut result| result.take());
        let Some(result) = result else {
            return preview_changed;
        };
        let path = self.history_pending_path.take();
        match result {
            Ok(()) => {
                self.status = self
                    .history_pending_success
                    .take()
                    .or_else(|| Some(t("files-history-change-saved")));
                if let Some(path) = path {
                    let history = crate::model::read_history(&path);
                    if let Some(entry) = self.entries.iter_mut().find(|entry| entry.path == path) {
                        entry.history = history;
                    }
                }
            }
            Err(error) => {
                self.history_pending_success = None;
                self.status = None;
                let action = path.map(NoticeAction::OpenHistory);
                self.notice.show(error, "", action);
            }
        }
        self.close_collab_panel();
        self.structure_dirty = true;
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete keyboard command map stays auditable in one match so every shortcut has a single routing point"
    )]
    fn handle_app_key(
        &mut self,
        chrome: &Chrome,
        ke: &winit::event::KeyEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if ke.state != winit::event::ElementState::Pressed {
            return false;
        }
        if self.renaming.is_some() {
            return false;
        }
        let shift = modifiers.state().shift_key();
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        let alt = modifiers.state().alt_key();
        let len = self.displayed_entries().len();
        match &ke.logical_key {
            Key::Named(NamedKey::ArrowLeft) if alt => {
                self.history_back();
                true
            }
            Key::Named(NamedKey::ArrowRight) if alt => {
                self.history_forward();
                true
            }
            Key::Named(NamedKey::ArrowUp) if alt => {
                self.go_up();
                true
            }
            Key::Character(s) if (s == "l" || s == "L") && ctrl => {
                if self.header_in_bar {
                    ui::window_toolbar::edit_location();
                } else {
                    self.toolbar.begin_path_edit();
                }
                true
            }
            Key::Named(NamedKey::F1) => {
                self.apply_command(FileCommand::ShowKeyboardShortcuts);
                true
            }
            Key::Character(s) if s == "?" => {
                self.apply_command(FileCommand::ShowKeyboardShortcuts);
                true
            }
            Key::Named(NamedKey::F5) => {
                self.apply_command(FileCommand::Refresh);
                true
            }
            Key::Named(NamedKey::ContextMenu) if !self.selection.is_empty() => {
                self.open_context_menu(self.keyboard_context_anchor(chrome));
                true
            }
            Key::Named(NamedKey::F2) => {
                self.begin_rename();
                true
            }
            Key::Named(NamedKey::ArrowDown) if len > 0 => {
                let next = self.selection.primary().map_or(0, |i| (i + 1).min(len - 1));
                self.selection.move_to(next, shift);
                self.retain_shown_selection();
                true
            }
            Key::Named(NamedKey::ArrowUp) if len > 0 => {
                let next = self.selection.primary().map_or(0, |i| i.saturating_sub(1));
                self.selection.move_to(next, shift);
                self.retain_shown_selection();
                true
            }
            Key::Named(NamedKey::Home) if len > 0 => {
                self.selection.move_to(0, shift);
                self.retain_shown_selection();
                true
            }
            Key::Named(NamedKey::End) if len > 0 => {
                self.selection.move_to(len - 1, shift);
                self.retain_shown_selection();
                true
            }
            Key::Named(NamedKey::Escape) if !self.selection.is_empty() => {
                self.selection.clear();
                self.retain_shown_selection();
                self.last_click = None;
                true
            }
            Key::Named(NamedKey::Enter) => {
                self.open_selected();
                true
            }
            Key::Named(NamedKey::Backspace) => {
                self.go_up();
                true
            }
            Key::Named(NamedKey::Space) if !self.selection.is_empty() => {
                self.show_preview();
                true
            }
            Key::Named(NamedKey::Delete) => {
                self.apply_command(FileCommand::Delete);
                true
            }
            Key::Character(s) if (s == "a" || s == "A") && ctrl => {
                self.selection.select_all(len);
                self.retain_shown_selection();
                true
            }
            Key::Character(s) if (s == "c" || s == "C") && ctrl => {
                self.apply_command(FileCommand::Copy);
                true
            }
            Key::Character(s) if (s == "x" || s == "X") && ctrl => {
                self.apply_command(FileCommand::Cut);
                true
            }
            Key::Character(s) if (s == "v" || s == "V") && ctrl => {
                self.apply_command(FileCommand::Paste);
                true
            }
            Key::Character(s) if (s == "n" || s == "N") && ctrl && shift => {
                self.apply_command(FileCommand::NewFolder);
                true
            }
            Key::Character(s) if (s == "h" || s == "H") && ctrl => {
                self.apply_command(FileCommand::ToggleHidden);
                true
            }
            Key::Character(s) if s == "0" && ctrl => {
                self.set_zoom(0);
                true
            }
            // Ctrl + / Ctrl - zoom the icon/row size (both "+" and "=" cover
            // keyboards where "+" is the shifted "=").
            Key::Character(s) if ctrl && (s == "+" || s == "=") => {
                self.set_zoom(self.zoom_level + 1);
                true
            }
            Key::Character(s) if ctrl && (s == "-" || s == "_") => {
                self.set_zoom(self.zoom_level - 1);
                true
            }
            Key::Character(s)
                if !ctrl
                    && !alt
                    && len > 0
                    && !s.trim().is_empty()
                    && !s.chars().any(char::is_control) =>
            {
                self.typing.type_text(s, ui::time::now());
                let names: Vec<String> = self
                    .displayed_entries()
                    .iter()
                    .map(|e| e.name.clone())
                    .collect();
                if let Some(i) = self
                    .typing
                    .target(names.iter().map(String::as_str), self.selection.primary())
                {
                    self.selection.select_only(i);
                    self.retain_shown_selection();
                }
                true
            }
            _ => false,
        }
    }

    fn handle_live_drag_event(
        &mut self,
        chrome: &Chrome,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        if self.pin_drag.is_none() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                self.drag_hover = if crate::external_drag::is_handed_off() {
                    None
                } else {
                    self.folder_target_at(chrome, self.last_cursor)
                };
                if self.spring.as_ref().map(|(path, _)| path) != self.drag_hover.as_ref() {
                    self.spring = self.drag_hover.clone().map(|path| (path, ui::time::now()));
                }
                *ctx.handled = true;
                self.structure_dirty = true;
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if crate::external_drag::is_handed_off() {
                    self.cancel_drag();
                } else {
                    self.finish_drag(chrome, ctx.modifiers.state().control_key());
                }
                *ctx.handled = true;
                self.structure_dirty = true;
                true
            }
            _ => false,
        }
    }

    fn handle_open_menu_event(
        &mut self,
        chrome: &mut Chrome,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::{MouseButton, WindowEvent};
        if !(chrome.context_menu.is_open() || chrome.submenu.is_open())
            || !matches!(
                event,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::Ime(_)
                    | WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::MouseInput {
                        button: MouseButton::Left,
                        ..
                    }
            )
            || !self.route_menu_event(chrome, event, ctx)
        {
            return false;
        }
        *ctx.handled = true;
        self.structure_dirty = true;
        let _ = self.drain_commands();
        true
    }

    fn handle_right_click_open_menu(
        &mut self,
        chrome: &mut Chrome,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } = event
        else {
            return false;
        };
        let mut child_handled = false;
        chrome.forward_to_body(
            event,
            &mut ui::widget::EventCtx {
                handled: &mut child_handled,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        self.drain_commands();
        let place = self.places_context.borrow_mut().take();
        if let Some((path, at)) = place {
            let removable = self
                .favs
                .borrow()
                .favorites
                .iter()
                .any(|favorite| favorite.path == path && !favorite.is_home);
            let mut spec = vec![(
                t("files-menu-open"),
                Some(FileCommand::Navigate(path.clone())),
                None,
            )];
            if removable {
                spec.push((
                    t("files-menu-remove-favourite"),
                    Some(FileCommand::RemoveFavorite(path)),
                    None,
                ));
            }
            self.open_menu_right_aligned(spec, at, at.y);
        } else if !chrome.in_places_list(self.last_cursor) {
            self.open_context_menu(self.last_cursor);
        }
        *ctx.handled = true;
        self.structure_dirty = true;
        true
    }

    fn handle_zoom_wheel(
        &mut self,
        chrome: &Chrome,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let winit::event::WindowEvent::MouseWheel { delta, .. } = event else {
            return false;
        };
        if !(ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key()) {
            return false;
        }
        if Widget::modal_capture(&chrome.nav).is_some() {
            return false;
        }
        let dy = match delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
            winit::event::MouseScrollDelta::PixelDelta(p) => p.y,
        };
        if dy > 0.0 {
            self.set_zoom(self.zoom_level + 1);
        } else if dy < 0.0 {
            self.set_zoom(self.zoom_level - 1);
        }
        *ctx.handled = true;
        self.structure_dirty = true;
        true
    }

    fn handle_rename_mode_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        if self.renaming.is_none() {
            return false;
        }
        if let Some(rect) = self.rename_rect.get() {
            self.rename_input.apply_layout_external(rect);
        }
        match event {
            WindowEvent::KeyboardInput { event: ke, .. } => {
                use winit::keyboard::{Key, NamedKey};
                if ke.state == ElementState::Pressed {
                    match &ke.logical_key {
                        Key::Named(NamedKey::Enter) => {
                            self.rename_buffer = self.rename_input.value().to_string();
                            self.commit_rename();
                            *ctx.handled = true;
                            self.structure_dirty = true;
                            return true;
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.cancel_rename();
                            *ctx.handled = true;
                            self.structure_dirty = true;
                            return true;
                        }
                        _ => {}
                    }
                }
                self.rename_input.handle_event(event, ctx);
                if let Some(value) = self.rename_input.take_dirty() {
                    self.rename_buffer = value;
                }
                *ctx.handled = true;
                self.structure_dirty = true;
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let inside = self
                    .rename_rect
                    .get()
                    .is_some_and(|r| r.contains(self.last_cursor));
                if inside {
                    self.rename_input.handle_event(event, ctx);
                    if let Some(value) = self.rename_input.take_dirty() {
                        self.rename_buffer = value;
                    }
                } else {
                    // A click away commits the rename (then this click is
                    // consumed; a second click acts on its target).
                    self.commit_rename();
                }
                *ctx.handled = true;
                self.structure_dirty = true;
                true
            }
            _ => false,
        }
    }

    fn finish_event(
        &mut self,
        chrome: &mut Chrome,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let path_before = self.current_path.clone();
        if self.route_marquee_event(event, ctx) {
            return;
        }
        chrome.forward_to_body(event, ctx);
        self.maybe_start_marquee(chrome, event, ctx);
        if !*ctx.handled
            && let winit::event::WindowEvent::KeyboardInput { event: ke, .. } = event
            && self.handle_app_key(chrome, ke, *ctx.modifiers)
        {
            *ctx.handled = true;
            self.structure_dirty = true;
        }
        if self.drain_commands() {
            self.structure_dirty = true;
        }
        if self.current_path != path_before
            && chrome.nav.front_pane() == ui::widget::NavPane::Sidebar
            && !chrome.nav.geometry().stack.is_empty()
        {
            chrome.nav.show(ui::widget::NavPane::Content);
        }
    }
}

impl FilesState {
    fn folder_answer(&self) -> serde_json::Value {
        let entries: Vec<String> = self
            .entries
            .iter()
            .take(MAX_ANSWERED_ENTRIES)
            .map(|entry| {
                if entry.is_dir {
                    format!("{}/", entry.name)
                } else {
                    entry.name.clone()
                }
            })
            .collect();
        let mut answer = serde_json::json!({
            "resource": <crate::resource::FileExplorer as ui::tilo_resource::Resource>::KIND,
            "path": self.current_path.display().to_string(),
            "entries": entries,
        });
        let more = self.entries.len().saturating_sub(MAX_ANSWERED_ENTRIES);
        if more > 0
            && let Some(fields) = answer.as_object_mut()
        {
            fields.insert("more".into(), more.into());
        }
        answer
    }

    fn append_nearby_receiver_resources(&self, folder: &mut ResourceSpec) {
        for transfer in self.nearby_send.transfers() {
            folder
                .children
                .push(self.nearby_receiver_resource(transfer));
        }
    }

    fn nearby_receiver_resource(
        &self,
        transfer: &ui::collaboration::NearbyTransferStatus,
    ) -> ResourceSpec {
        use ui::collaboration::NearbyTransferPhase;

        let mut resource = ResourceSpec::new(
            format!(
                "nearby-transfer:{}",
                nearby_action_locator("transfer", transfer.handle.as_bytes())
            ),
            "files.nearby-transfer",
            transfer.name.clone(),
        );
        let mut inspect = crate::effects::row_action(
            &crate::effects::FilesEffect::InspectNearby {
                transfer: transfer.handle,
            },
            "status",
            t("files-nearby-status"),
        );
        inspect.type_mut().description = Some(t("files-nearby-status-detail"));
        resource.actions.push(inspect);
        self.append_nearby_decision_actions(&mut resource, transfer);
        if transfer.phase == NearbyTransferPhase::Failed {
            resource.actions.push(crate::effects::row_action(
                &crate::effects::FilesEffect::RetryNearby {
                    transfer: transfer.handle,
                },
                "retry",
                t("files-nearby-retry"),
            ));
        }
        if matches!(
            transfer.phase,
            NearbyTransferPhase::Queued | NearbyTransferPhase::Transferring
        ) {
            resource.actions.push(crate::effects::row_action(
                &crate::effects::FilesEffect::CancelNearby {
                    transfer: transfer.handle,
                },
                "cancel",
                t("files-nearby-cancelled"),
            ));
        }
        resource
    }

    fn append_nearby_decision_actions(
        &self,
        resource: &mut ResourceSpec,
        transfer: &ui::collaboration::NearbyTransferStatus,
    ) {
        use ui::collaboration::{NearbyTransferDirection, NearbyTransferPhase};

        if transfer.direction != NearbyTransferDirection::Receiving
            || !matches!(
                transfer.phase,
                NearbyTransferPhase::AwaitingConsent | NearbyTransferPhase::AwaitingDestination
            )
        {
            return;
        }
        for destination in self.nearby_send.destinations() {
            resource.actions.push(crate::effects::row_action(
                &crate::effects::FilesEffect::AcceptNearby {
                    transfer: transfer.handle,
                    destination: destination.handle,
                },
                format!(
                    "accept-{}",
                    nearby_action_locator("destination", destination.handle.as_bytes())
                ),
                t_args(
                    "files-nearby-save-to-destination",
                    &[arg("destination", destination.label.clone())],
                ),
            ));
        }
        resource.actions.push(crate::effects::row_action(
            &crate::effects::FilesEffect::DeclineNearby {
                transfer: transfer.handle,
            },
            "decline",
            t("files-nearby-declined"),
        ));
    }

    fn live_filesystem_extras(&self) -> Vec<ResourceSpec> {
        self.published_live_rows()
            .into_iter()
            .map(ui::framework::LiveRow::into_resource)
            .collect()
    }

    pub(crate) fn published_live_rows(&self) -> Vec<ui::framework::LiveRow> {
        use ui::framework::LiveRow;
        let mut rows: Vec<LiveRow> = published_place_resources(&self.favorites())
            .into_iter()
            .chain(published_template_resources(self.available_templates()))
            .map(LiveRow::from_resource)
            .collect();
        rows.extend(self.live_entry_rows());
        rows.extend(
            self.live_other_resources()
                .into_iter()
                .map(LiveRow::from_resource),
        );
        rows
    }

    fn live_other_resources(&self) -> Vec<ResourceSpec> {
        let mut extras = self.live_remote_account_resources();
        extras.extend(self.live_drive_resources());
        if ui::bound_dispatch::host_person_invocation_permitted() {
            extras.extend(self.cloud_resources());
        }
        let mut nearby_root = ResourceSpec::new(
            "files.nearby",
            "files.nearby",
            crate::i18n::CATALOG.t("files-resource-nearby"),
        );
        self.append_nearby_receiver_resources(&mut nearby_root);
        extras.extend(nearby_root.children);
        if let Some((failure, _)) = self.folder_failure() {
            extras.push(retry_row(failure.may_pass()));
        }
        extras
    }

    fn live_remote_account_resources(&self) -> Vec<ResourceSpec> {
        // Account discovery belongs to the host person, not to whoever owns
        // the workspace. Checked resource reads install this trusted scope too.
        if !ui::bound_dispatch::host_person_invocation_permitted() {
            return Vec::new();
        }
        self.remote_account_snapshot()
            .into_iter()
            .map(|account| {
                let resource_id = crate::remote_accounts::resource_id(&account.id);
                let availability = remote_folder_availability(&account);
                let mut resource =
                    ResourceSpec::new(resource_id, "files.remote-account", account.label.clone());
                resource.presentation = Some(tilo_actions::ResourcePresentation {
                    detail: Some(format!("{} at {}", account.kind.label(), account.host)),
                    badge: account.badge(),
                    preview: None,
                });
                let account_id = account.id.to_string();
                let mut open = crate::effects::row_action(
                    &crate::effects::FilesEffect::OpenAccount {
                        account: account_id.clone(),
                    },
                    crate::search_provider::REMOTE_ACCOUNT_OPEN_ACTION,
                    crate::i18n::CATALOG.t("files-shortcuts-open"),
                );
                open.type_mut().contract.availability = availability;
                resource.actions.push(open);
                resource.actions.push(crate::effects::row_action(
                    &crate::effects::FilesEffect::AccountDetails {
                        account: account_id.clone(),
                    },
                    "details",
                    crate::i18n::CATALOG.t("files-account-status"),
                ));
                resource.actions.push(crate::effects::row_action(
                    &crate::effects::FilesEffect::Reauthenticate {
                        account: account_id,
                    },
                    "reauthenticate",
                    crate::i18n::CATALOG.t("files-account-reauthenticate"),
                ));
                resource
            })
            .collect()
    }
}

#[derive(Default)]
struct EntryIds {
    made_of: Vec<(PathBuf, Option<[u8; 16]>)>,
    by_id: HashMap<String, usize>,
}

impl EntryIds {
    fn find<'a>(&mut self, entries: &'a [Entry], id: &str) -> Option<&'a Entry> {
        if let Some(entry) = self.lookup(entries, id) {
            return Some(entry);
        }
        let current = self.made_of.len() == entries.len()
            && self
                .made_of
                .iter()
                .zip(entries)
                .all(|((path, record), entry)| *path == entry.path && *record == entry.record_id);
        if current {
            return None;
        }
        self.made_of = entries
            .iter()
            .map(|entry| (entry.path.clone(), entry.record_id))
            .collect();
        self.by_id = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (FilesState::entry_resource_id(entry), index))
            .collect();
        self.lookup(entries, id)
    }

    fn lookup<'a>(&self, entries: &'a [Entry], id: &str) -> Option<&'a Entry> {
        let index = *self.by_id.get(id)?;
        let (path, record) = self.made_of.get(index)?;
        entries
            .get(index)
            .filter(|entry| entry.path == *path && entry.record_id == *record)
    }
}

impl FilesState {
    fn listing_deadline(&self) -> Option<Instant> {
        let changed_directory =
            (self.directory_changed && !self.directory_read_deferred()).then(|| {
                self.last_directory_read + metadata_refresh::change_read_gap(self.entries.len())
            });
        [self.next_metadata_refresh, changed_directory]
            .into_iter()
            .flatten()
            .min()
    }

    fn other_deadline(&self, chrome: &Chrome) -> Option<Instant> {
        self.nearby_send
            .has_requests()
            .then(Instant::now)
            .into_iter()
            .chain(self.local_clipboard.next_wake())
            .chain(
                self.open_replies
                    .lock()
                    .is_ok_and(|replies| !replies.is_empty())
                    .then(Instant::now),
            )
            .chain(self.drives.next_wake())
            .chain(self.applications.next_wake())
            .chain((!self.incoming.is_empty()).then(Instant::now))
            .chain(
                self.preview
                    .as_ref()
                    .filter(|preview| preview.is_loading())
                    .map(|_| Instant::now() + Duration::from_millis(50)),
            )
            .chain(
                crate::file_ops::active_transfers()
                    .first()
                    .and_then(|transfer| transfer.started)
                    .map(|started| {
                        (started + TRANSFER_SHOWN_AFTER)
                            .max(Instant::now() + Duration::from_millis(250))
                    }),
            )
            .chain(self.spring.as_ref().map(|(_, since)| *since + SPRING_OPEN))
            .chain(chrome.nav.next_wake_deadline())
            .min()
    }
}

#[path = "native_observer.rs"]
mod native_observer;

impl FilesState {
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        self.retire_cancelled_effect();
        let now = ui::time::now();
        if let Some(result) = self.local_clipboard.timeout(now) {
            self.complete_clipboard_publication(result);
        }
        self.shortcut_menu.poll_if_due(now);
        let mut requests = self.nearby_send.take_requests();
        requests.extend(self.shortcut_menu.take_requests());
        requests.extend(self.share_invite.take_requests());
        requests.extend(self.local_clipboard.take_request());
        requests
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        self.retire_cancelled_effect();
        if let Some(result) = self.local_clipboard.event(event) {
            self.complete_clipboard_publication(result);
        }
        self.drives.event(event);
        self.applications.event(event);
        let changed = self.handle_shortcut_event(event) || self.handle_share_event(event);
        let nearby_changed = self.handle_nearby_event(event);
        if changed || nearby_changed {
            self.structure_dirty = true;
        }
    }
}

impl FilesState {
    fn handle_nearby_event(&mut self, event: &ui::collaboration::CollabEvent) -> bool {
        let mut changed = false;
        for effect in self.nearby_send.handle_event(event) {
            match effect {
                crate::nearby_send::NearbyUiEffect::AvailabilityChanged => {
                    self.structure_dirty = true;
                }
                crate::nearby_send::NearbyUiEffect::OpenPicker(targets) => {
                    self.open_nearby_device_picker(&targets);
                    changed = true;
                }
                crate::nearby_send::NearbyUiEffect::Offer {
                    transfer,
                    destinations,
                } => {
                    changed = true;
                    self.show_collab_panel(CollabPanel {
                        title: t_args(
                            "files-nearby-offer-title",
                            &[arg("device", transfer.device_label)],
                        ),
                        summary: t_args(
                            "files-nearby-offer-summary",
                            &[arg("name", transfer.name)],
                        ),
                        lines: vec![
                            t("files-nearby-consent-detail"),
                            if destinations.is_empty() {
                                t("files-nearby-no-destinations")
                            } else {
                                t_args(
                                    "files-nearby-destination-count",
                                    &[arg(
                                        "count",
                                        i64::try_from(destinations.len()).unwrap_or(i64::MAX),
                                    )],
                                )
                            },
                            t_args(
                                "files-nearby-offer-size",
                                &[arg(
                                    "bytes",
                                    i64::try_from(transfer.total_bytes).unwrap_or(i64::MAX),
                                )],
                            ),
                        ],
                        action: (!destinations.is_empty()).then_some(
                            CollabPanelAction::ChooseNearbyDestination {
                                handle: transfer.handle,
                            },
                        ),
                    });
                }
                crate::nearby_send::NearbyUiEffect::Failed(transfer) => {
                    changed = true;
                    self.show_collab_panel(CollabPanel {
                        title: t("files-nearby-failed-title"),
                        summary: transfer.name,
                        lines: vec![
                            t_args(
                                "files-nearby-failed-device",
                                &[arg("device", transfer.device_label)],
                            ),
                            t("files-nearby-retry-detail"),
                        ],
                        action: Some(CollabPanelAction::RetryNearby {
                            handle: transfer.handle,
                        }),
                    });
                }
                crate::nearby_send::NearbyUiEffect::Status(message) => {
                    if self.status.as_deref() != Some(message.as_str()) {
                        self.status = Some(message);
                        changed = true;
                    }
                }
                crate::nearby_send::NearbyUiEffect::SendResult(result) => {
                    if self.effect_reply.is_none() {
                        continue;
                    }
                    changed = true;
                    self.answer_effect(
                        result
                            .map(|_| tilo_actions::ActionOutput::default())
                            .map_err(|message| tilo_actions::ActionFailure {
                                code: "files.nearby_send_failed".into(),
                                message,
                                details: None,
                                retryable: true,
                            }),
                    );
                }
            }
        }
        changed
    }
}

impl FilesState {
    fn shared_selection(&self) -> Option<ui::shared_selection::SharedSelection> {
        let displayed = self.displayed_entries();
        let picked: Vec<&Entry> = self
            .selection
            .selected()
            .into_iter()
            .filter_map(|index| displayed.get(index))
            .collect();
        if picked.is_empty() {
            return None;
        }
        Some(
            ui::shared_selection::SharedSelection::of_items(
                picked
                    .iter()
                    .map(|entry| (Self::entry_resource_id(entry), entry.name.as_str())),
            )
            .in_kind(<crate::resource::FileExplorer as ui::tilo_resource::Resource>::KIND),
        )
    }
}

impl FilesState {
    fn set_filter(&mut self, query: &str) {
        if self.find == query {
            return;
        }
        query.clone_into(&mut self.find);
        self.structure_dirty = true;
    }
}

impl FilesState {
    fn poll_frame(&mut self) -> bool {
        let (opened, menu) = self.list_slot.take_row_requests();
        let mut restored = false;
        if let Some(index) = opened {
            restored |= self.apply_command(FileCommand::Activate(index));
        }
        if let Some((index, at)) = menu {
            restored |= self.apply_command(FileCommand::ContextMenuAt { index, at });
        }
        let transfers = self.follow_transfers(ui::time::now());
        let previewed = self.preview.as_mut().is_some_and(Preview::poll);
        let sprang = self.spring_open_if_due(ui::time::now());
        let counted = self.folder_counts.take_arrived();
        let listed = self.take_folder_read()
            | self.take_archive_listing()
            | self.poll_extract_chooser()
            | self.open_read_members();
        self.structure_dirty |= counted;
        self.take_open_replies()
            | self.poll_history_result()
            | restored
            | transfers
            | previewed
            | sprang
            | counted
            | listed
            | self.poll_cloud()
            | self.poll_mounted_cloud()
            | self.poll_drives()
            | self.poll_applications(ui::time::now())
    }

    fn sync_frame(&mut self, chrome: &mut Chrome, state: &AppModel, path: &ModelPath) {
        self.adopt_preferences();
        if self.drain_commands() | self.drain_opened() {
            self.structure_dirty = true;
        }
        if self.sync_window_toolbar(chrome) {
            self.structure_dirty = true;
        }
        self.poll_cloud();
        self.poll_mounted_cloud();
        self.take_incoming_drop();
        self.refresh_framework_history_target();
        self.refresh_metadata_if_due(ui::time::now());
        if self.collaborative_record_pager.borrow().has_more() {
            let page = self.advance_collaborative_search_page();
            if page.truncated {
                tracing::debug!("collaborative search catalogue truncated; continues on refresh");
            }
        }
        // A rename proposal is process-local. Seeding it must not mutate the
        // shared document: neither a filesystem observation nor an unfinished
        // filename belongs in collaborative UI history.
        if self.rename_seed_pending {
            self.rename_input.set_text(&self.rename_buffer);
            self.rename_seed_pending = false;
        }

        self.rename_input.sync(state, path);
        if self.rename_select_pending {
            let is_dir = self
                .renaming
                .and_then(|i| self.displayed_entries().get(i).map(|e| e.is_dir))
                .unwrap_or(true);
            match rename_stem_end(&self.rename_buffer, is_dir) {
                Some(stem) => self.rename_input.set_selection(0, stem),
                None => self.rename_input.select_all(),
            }
            self.rename_select_pending = false;
        }

        chrome.nav.sync(state, path);

        if self.renaming.is_some()
            && let Some(value) = self.rename_input.take_dirty()
        {
            self.rename_buffer = value;
        }
        for press in self.needs.take() {
            let Some(acted) = ui::attention::apply(press) else {
                continue;
            };
            if let Some(action) = self.notice.pressed(&acted) {
                self.act_on_notice(action);
                self.structure_dirty = true;
            }
        }
        self.publish_status();
        self.notice.publish();
    }

    fn wake_deadline(&self, chrome: &Chrome) -> Option<Instant> {
        [self.listing_deadline(), self.other_deadline(chrome)]
            .into_iter()
            .flatten()
            .min()
    }

    fn draw_overlay_frame(
        &mut self,
        chrome: &mut Chrome,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        chrome
            .nav
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        self.draw_collab_panel(scene, transform, text_renderer, theme);
        let window = self.rect;
        if let Some(preview) = self.preview.as_mut() {
            preview.paint(scene, transform, text_renderer, theme, window);
        }
        if let Some(pending) = self.clash.as_mut() {
            pending.paint(scene, transform, text_renderer, theme, window);
        }
        if chrome.context_menu.is_open() {
            layout_menu_widget(&mut chrome.context_menu);
            chrome
                .context_menu
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        if chrome.submenu.is_open() {
            layout_menu_widget(&mut chrome.submenu);
            chrome
                .submenu
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        self.resolve_staged_move_drag();
        self.draw_drop_highlight(chrome, scene, transform, theme);
        self.draw_marquee(scene, transform, theme);
        self.draw_pin_ghost(scene, transform, text_renderer, theme);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "root event dispatcher orders pointer, keyboard, drag, menu, and retained-child routing"
    )]
    fn handle_window_event(
        &mut self,
        chrome: &mut Chrome,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        self.poll_history_result();
        let panel_was_open = self.collab_panel.is_some();
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) {
            self.widths_at_press = Some(self.list_slot.widths());
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = ctx.logical(position);
            if let Some((start, path, is_dir)) = self.armed_drag.clone()
                && self.pin_drag.is_none()
                && self.last_cursor.distance(start) > crate::views::file_view::drag_threshold()
            {
                self.armed_drag = None;
                self.apply_command(FileCommand::BeginDrag { path, is_dir });
                self.structure_dirty = true;
            }
        }
        if self.clash.is_some() {
            self.route_clash_event(event);
            *ctx.handled = true;
            return;
        }
        if self.preview.is_some() {
            self.route_preview_event(event, *ctx.modifiers);
            *ctx.handled = true;
            return;
        }
        if self.route_incoming_drop(chrome, event) {
            *ctx.handled = true;
            return;
        }
        if self.error.is_some()
            && matches!(
                event,
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed
                        && matches!(
                            event.logical_key,
                            winit::keyboard::Key::Named(
                                winit::keyboard::NamedKey::Enter
                                    | winit::keyboard::NamedKey::Space
                            )
                        )
            )
        {
            self.structure_dirty |= self.apply_command(self.folder_recovery_command());
            *ctx.handled = true;
            return;
        }
        if self.collab_panel.is_some() {
            use winit::keyboard::{Key, NamedKey};
            let hit = self
                .collab_layout
                .as_ref()
                .and_then(|layout| layout.hit(self.last_cursor));
            let activate = match event {
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed
                        && event.logical_key == Key::Named(NamedKey::Enter) =>
                {
                    self.collab_panel_action_command()
                }
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                } if hit == Some(ui::widget::ModalHit::Action(0)) => {
                    self.collab_panel_action_command()
                }
                _ => None,
            };
            if let Some(command) = activate {
                self.close_collab_panel();
                self.apply_command(command);
                *ctx.handled = true;
                self.structure_dirty = true;
                return;
            }
            let close = match event {
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed
                        && event.logical_key == Key::Named(NamedKey::Escape) =>
                {
                    true
                }
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                } => self
                    .collab_layout
                    .as_ref()
                    .is_none_or(|layout| layout.dismisses(self.last_cursor)),
                _ => false,
            };
            if close {
                self.close_collab_panel();
            }
            *ctx.handled = true;
            self.structure_dirty = true;
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } = event
        {
            self.armed_drag = None;
            self.release_click();
        }

        if self.handle_live_drag_event(chrome, event, ctx)
            || self.handle_open_menu_event(chrome, event, ctx)
            || self.handle_right_click_open_menu(chrome, event, ctx)
            || self.handle_zoom_wheel(chrome, event, ctx)
            || self.handle_rename_mode_event(event, ctx)
        {
            return;
        }

        self.finish_event(chrome, event, ctx);
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            }
        ) && self.widths_at_press.take() != Some(self.list_slot.widths())
        {
            self.store_widths();
        }
        if !panel_was_open && self.collab_panel.is_some() {
            // The dialog replaced the semantic subtree during this event.
            // Focus its real backing widget so AccessKit never points at a
            // now-hidden toolbar/list node.
            *ctx.focus_target = Some(ctx.node_id);
        }
    }

    fn window_accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        if let Some(sheet) = self.sheet_accessibility() {
            return sheet;
        }
        if let Some(panel) = &self.collab_panel {
            let mut children = Vec::with_capacity(panel.lines.len() + 2);
            children
                .push(AccessNode::new(Role::Heading { level: 2 }).with_name(panel.title.clone()));
            children.push(AccessNode::new(Role::Paragraph).with_name(panel.summary.clone()));

            children.extend(
                panel
                    .lines
                    .iter()
                    .map(|line| AccessNode::new(Role::StaticText).with_name(line.clone())),
            );
            if panel.action.is_some() {
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name(self.collab_panel_action_label().unwrap_or_default())
                        .with_description(
                            self.collab_panel_action_description().unwrap_or_default(),
                        ),
                );
            }
            return AccessNode::new(Role::Dialog)
                .with_name(panel.title.clone())
                .with_description(t_args(
                    "files-a11y-dialog-close",
                    &[arg("summary", panel.summary.clone())],
                ))
                .with_value(panel.lines.join(". "))
                .with_live(Politeness::Polite)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.collab_panel_focused)
                        .modal(true),
                )
                .with_children(children);
        }

        AccessNode::new(Role::Group)
            .with_name(t("files-a11y-activity"))
            .with_description(t_args(
                "files-a11y-current-folder",
                &[arg("path", self.current_path.display().to_string())],
            ))
            .with_value(self.accessibility_status())
            .with_live(if self.error.is_some() {
                Politeness::Assertive
            } else {
                Politeness::Polite
            })
            .with_state(AccessState::new().busy(self.is_loading))
    }

    fn perform_window_action(&mut self, target: &str, action: &ui::a11y::Action) -> bool {
        use ui::a11y::Action;
        if let Some(index) = target.strip_prefix(CLASH_TARGET)
            && matches!(action, Action::Activate)
        {
            let choice = index.parse().ok().and_then(PendingClash::choice_at);
            if choice.is_some() {
                self.settle_clash(choice);
            }
            return choice.is_some();
        }
        if let Some(version) = target.strip_prefix("files-history-version:") {
            let Ok(version) = uuid::Uuid::parse_str(version) else {
                return false;
            };
            if matches!(
                action,
                Action::Focus | Action::Activate | Action::ScrollIntoView
            ) {
                return self.apply_command(FileCommand::SelectHistoryVersion {
                    version_id: fs_protocol::FileVersionId(version),
                });
            }
            return false;
        }
        if !matches!(action, Action::Activate) {
            return false;
        }
        false
    }
}

impl FilesState {
    pub(crate) fn search_entries_shown(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        let mut entries = self.places_search.entries();
        entries.extend(self.collaborative_search.entries());
        if ui::bound_dispatch::host_person_invocation_permitted() {
            entries.extend(self.remote_accounts_search.entries());
        }
        entries
    }
}

impl FilesState {
    #[cfg(test)]
    pub(crate) fn published_rows(&self) -> Vec<ResourceSpec> {
        self.live_filesystem_extras()
    }

    pub(crate) fn published_row(&self, id: &str) -> Option<ResourceSpec> {
        fn find(resource: &ResourceSpec, id: &str) -> Option<ResourceSpec> {
            if resource.id.as_str() == id {
                return Some(resource.clone());
            }
            resource.children.iter().find_map(|child| find(child, id))
        }
        if let Some(found) = self
            .live_filesystem_extras()
            .iter()
            .find_map(|resource| find(resource, id))
        {
            return Some(found);
        }
        self.entry_by_resource_id(id)
            .map(|entry| self.entry_resource(entry))
            .or_else(|| {
                self.places_search
                    .resource(id)
                    .or_else(|| self.collaborative_search.resource(id))
                    .or_else(|| {
                        ui::bound_dispatch::host_person_invocation_permitted()
                            .then(|| self.remote_accounts_search.resource(id))
                            .flatten()
                    })
            })
            .or_else(|| {
                self.folder_failure()
                    .filter(|_| id == RETRY_ROW)
                    .map(|(failure, _)| retry_row(failure.may_pass()))
            })
    }
}

impl FilesState {
    pub(crate) fn take_host_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        self.host_intent.take()
    }
}

#[path = "files_app/open_with_recent.rs"]
mod open_with_recent;

#[cfg(test)]
#[path = "files_app/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "files_app/notice_action_tests.rs"]
mod notice_action_tests;

#[cfg(test)]
#[path = "files_app/launch_argument_tests.rs"]
mod launch_argument_tests;

#[cfg(test)]
#[path = "files_app/sidebar_button_tests.rs"]
mod sidebar_button_tests;

#[cfg(test)]
pub(crate) mod published {
    use super::FileExplorerWidget;
    use crate::effects::FilesEffect;
    use crate::resource::FileExplorer;
    use ui::framework::HostDelivery as _;
    use ui::tilo_actions::ResourceSpec;
    use ui::tilo_resource::{Effect as _, Resource as _};

    pub(crate) fn tree(
        files: &FileExplorerWidget,
        doc: &automerge::AutoCommit,
    ) -> Vec<ResourceSpec> {
        let mut root = ui::tilo_resource::publish::<FileExplorer>(
            FileExplorer::KIND,
            "Files",
            &FileExplorer::default(),
        );
        for decl in files.state().bound_declarations() {
            root = ui::tilo_resource::bind_declaration(root, &decl);
        }
        root.actions.extend(FilesEffect::specs());
        let _ = doc;
        root.children.extend(files.state().published_rows());
        vec![root]
    }

    pub(crate) fn invoke(
        files: &mut FileExplorerWidget,
        doc: &mut automerge::AutoCommit,
        resource: &str,
        action: &str,
        args: &serde_json::Value,
    ) -> Result<Option<Started>, String> {
        let tree = tree(files, doc);
        let address = ui::tilo_actions::door::published(&tree, resource, action)
            .ok_or_else(|| format!("{resource} offers no {action}"))?;
        let spec = ui::tilo_actions::door::resolve(&tree, &address)
            .ok_or_else(|| format!("{resource} offers no {action}"))?;
        let kind = spec.kind;
        let empty = serde_json::Map::new();
        let encoded = ui::tilo_actions::door::encode_json(
            &tree,
            &address,
            args.as_object().unwrap_or(&empty),
        )
        .map_err(|error| error.to_string())?;
        start(
            files,
            kind,
            &encoded.type_id,
            &encoded.payload,
            doc,
            &automerge::ROOT,
        )
    }

    pub(crate) type Started = ui::framework::Effected;

    pub(crate) fn start(
        files: &mut FileExplorerWidget,
        kind: ui::tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Result<Option<Started>, String> {
        if type_id == FilesEffect::OPERATION {
            return Ok(Some(
                match files.state().run_effect(
                    FilesEffect::from_payload(payload).map_err(|error| error.to_string())?,
                )? {
                    super::Ran::Done(value) => Started::Done(ui::tilo_actions::ActionOutput {
                        value,
                        ..ui::tilo_actions::ActionOutput::default()
                    }),
                    super::Ran::Pending => Started::Pending,
                },
            ));
        }
        files.start_encoded(kind, type_id, payload, doc, path)
    }
}

#[cfg(test)]
#[cfg(unix)]
#[path = "framework_history_tests.rs"]
mod framework_history_tests;

impl FilesState {
    pub(crate) fn debug_begin_favorite_drag(
        &self,
        chrome: &mut Chrome,
        from: usize,
        to: usize,
    ) -> bool {
        let (source, before) = {
            let favs = self.favs.borrow();
            let Some(source) = favs.favorites.get(from) else {
                return false;
            };
            let Some(before) = favs.favorites.get(to) else {
                return false;
            };
            if source.is_home || before.is_home {
                return false;
            }
            (
                format!("fav:{}", source.path.display()),
                format!("fav:{}", before.path.display()),
            )
        };
        let Some(list) = chrome.nav.sidebar_mut::<ui::widget::NavList>() else {
            return false;
        };
        list.stage_drag(&source, &before);
        true
    }

    pub fn arm_status_for_shots(&mut self) {
        let label = self.footer_label();
        self.status_notice.show(label);
    }
}
