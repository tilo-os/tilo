//! `FileExplorerWidget` — the Tilo file explorer's `AppWidget`.
//!
//! The widget owns all app state (current dir, entries, multi-selection,
//! sort, view mode, navigation history) and rebuilds its child widget tree
//! from that state. Interactive children can't borrow the app back, so they
//! push [`FileCommand`]s into a shared [`CommandSink`]; `handle_event`
//! drains the sink after each event and applies the commands, marking the
//! tree structurally dirty so the App rebuilds the composition on the next
//! frame. See [`crate::command`].

use std::fmt::Write as _;

use crate::command::{CommandSink, FileCommand, new_sink};
use crate::history_panel::HistoryPanel;
use crate::i18n::{arg, t, t_args};
use crate::metadata_refresh;
use crate::model::{Entry, SortBy, SortOrder, ViewMode, initial_path, read_dir_sync, sort_entries};
use crate::places_sidebar::{self, FavStore, PlacesSidebar};
use crate::views::file_view::{FileView, ThumbCtx};
use crate::{config, favorites, fileops, templates};
use host_fs::attributes as xattr;
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use thumbnail::{ThumbKey, ThumbnailService};
use ui::paint::PaintCtx;
use ui::tilo_actions::{
    ActionArgs, ActionContract, ActionParam, ActionSpec, Availability, ConfirmationPolicy,
    EncodedAction, ExecutionMode, OperationKind, ParamType, PermissionRequirement, Resource,
    ResultSpec, RetryPolicy, RiskLevel,
};
use ui::time::{Duration, Instant, UNIX_EPOCH};
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Button, Collaborative, Column, EmptyState, EmptyStateContent, NavShell, ResourceOperation,
    ScrollAxis, Scrollable, ShellMode, Spacer, Spinner, Text, TextInput, Widget,
};
use ui::{AppKind, AppWidget, IconDescriptor, SelectionModel};
use vello::kurbo::Point;
use vello::peniko::ImageData as PenikoImage;

/// Width (logical px) at/above which the places list docks beside the file
/// view; below it the places list becomes an overlay drawer *over* the file
/// view, summoned by the hamburger chip. The file list is the content either
/// way — the places list is navigation, so it never takes the window from it
/// (`crates/sdk/ui-core/HIG.md` §1a).
const BODY_BREAKPOINT_PX: f64 = 640.0;
/// Space kept clear at the bottom of the file viewport for the transient
/// item/status pill. The pill includes its hover shadow, so this is larger
/// than its measured text capsule: no file label or target may sit beneath it.
pub const STATUS_BUBBLE_SAFE_INSET_PX: f32 = 64.0;
/// Long absolute paths clip at narrow widths and obscure the recovery action.
/// Preserve the recognisable tail here; the full location stays in the path bar
/// and the root accessibility description.
const STATE_SURFACE_LOCATION_CHARS: usize = 36;
/// The resource catalogue is shared with search, activity, automation, and
/// assistive clients. Keep its eagerly enumerated current-folder projection
/// deliberately small; an exact item is materialised from the live snapshot
/// by [`FileExplorerWidget::resource`] when a caller addresses it directly.
const MAX_EAGER_ENTRY_RESOURCES: usize = 128;
/// The most entries one `move` takes.
const MAX_MOVE_ENTRIES: usize = 256;
/// The most entry names showing a folder answers with.
const MAX_ANSWERED_ENTRIES: usize = 64;

/// A `move` of several of the current folder's entries into `folder`.
#[derive(serde::Serialize, serde::Deserialize)]
struct MoveRequest {
    entries: Vec<String>,
    folder: String,
    create: bool,
}
type MergePreflightResult = Arc<
    std::sync::Mutex<
        Option<(
            uuid::Uuid,
            collab_types::FileVersionId,
            Result<collab_types::MergePreflightProjectionV1, String>,
        )>,
    >,
>;
/// A remote account's folder opens while the cloud service has it mounted.
/// A disabled or not-yet-mounted account says why instead.
fn remote_folder_availability(account: &crate::remote_accounts::RemoteAccount) -> Availability {
    if account.health == crate::remote_accounts::RemoteAccountHealth::Disabled {
        return Availability::Unavailable {
            code: "account_disabled".into(),
            reason: account.health.unavailable_reason().into(),
        };
    }
    if !account.mount.as_deref().is_some_and(Path::is_dir) {
        return Availability::Unavailable {
            code: "account_not_mounted".into(),
            reason: "This account's folder is not mounted yet".into(),
        };
    }
    Availability::Available
}

/// Build the file manager's thumbnail service. The content-addressed cache
/// lives under the app's persistence dir (a tempdir in tests, so they stay
/// isolated) so previews survive across launches; it falls back to a temp dir
/// when there is no persistence dir. `None` when neither location is
/// writable — previews then fall back to type badges (see [`ThumbCtx`]).
fn build_thumb_service(config_dir: Option<&Path>) -> Option<ThumbnailService> {
    #[cfg(test)]
    crate::model::fixture_io_guard::ambient_io("build_thumb_service");
    let dir = config_dir.map_or_else(
        || std::env::temp_dir().join("tilo-file-explorer-thumbnails"),
        |d| d.join("thumbnails"),
    );
    ThumbnailService::open(&dir, 4).ok().or_else(|| {
        let fallback = std::env::temp_dir().join("tilo-file-explorer-thumbnails");
        ThumbnailService::open(&fallback, 4).ok()
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CollabPanel {
    title: String,
    summary: String,
    lines: Vec<String>,
    action: Option<CollabPanelAction>,
    /// History has its own selectable timeline and detail pane instead of
    /// being flattened into generic bullet lines.
    history: Option<HistoryPanel>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CollabPanelAction {
    DestructiveEviction(Vec<PathBuf>),
    PermanentDelete(Vec<PathBuf>),
    EmptyTrash,
    ApplySafeMerge {
        conflict_id: uuid::Uuid,
        version_id: collab_types::FileVersionId,
    },
    ChooseNearbyDestination {
        handle: ui::collaboration::NearbyTransferHandle,
    },
    RetryNearby {
        handle: ui::collaboration::NearbyTransferHandle,
    },
}

/// Lower-case hex of the first `count` bytes (all of them when shorter).
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

/// Opaque, bounded resource identity for a local Templates entry.
///
/// A resource tree is searchable and may be retained in activity/history, so
/// it must not turn an absolute local template path into an identifier. The
/// full path remains an invocation payload only and is revalidated against the
/// current template catalogue before it is read.
pub(crate) fn template_resource_id(path: &str) -> String {
    // Resource addresses cross the action door and history, so a collision
    // must be cryptographically infeasible rather than merely improbable. The
    // 128-bit SHA-256 prefix is stable, opaque, and far beyond the collision
    // bound of the small, local Templates catalogue; SHA-2 is already part of
    // the Tilo shared cryptographic stack.
    let digest = Sha256::digest(path.as_bytes());
    format!("files.template:{}", hex_prefix(&digest, 16))
}

/// Folder Views/Effects for [`crate::resource::FileExplorer::local_operations`].
///
/// Hosted surfaces stop republishing a second `files.folder` root, so these
/// must ride `tilo_resource::publish` — otherwise the palette loses refresh,
/// new-folder, paste, go-to, and the presentation Views.
#[allow(
    clippy::expect_used,
    reason = "folder name and path payloads are locally constructed and serializable"
)]
pub(crate) fn folder_palette_operations() -> Vec<ActionSpec> {
    let mut ops = Vec::with_capacity(24);
    if let Ok(spec) = ActionSpec::operation(
        "refresh",
        "Refresh folder",
        OperationKind::Effect,
        "files.refresh",
        &"",
    ) {
        ops.push(spec.with_contract(ActionContract {
            permission: PermissionRequirement::Read,
            retry: RetryPolicy::Idempotent,
            ..ActionContract::default()
        }));
    }
    ops.push(
        ActionSpec::prompted_operation(
            "new-folder",
            "New folder",
            "Folder name",
            "New Folder",
            OperationKind::Effect,
            "files.new-folder",
            |name| {
                EncodedAction::encode_operation("files.new-folder", &name)
                    .expect("folder name is serializable")
            },
        )
        .with_contract(ActionContract {
            permission: PermissionRequirement::Write,
            reversible: false,
            ..ActionContract::default()
        }),
    );
    if let Ok(spec) =
        ActionSpec::operation("paste", "Paste", OperationKind::Effect, "files.paste", &"")
    {
        ops.push(spec.with_contract(ActionContract {
            permission: PermissionRequirement::Write,
            risk: RiskLevel::Low,
            confirmation: ConfirmationPolicy::None,
            retry: RetryPolicy::AtMostOnce,
            reversible: false,
            ..ActionContract::default()
        }));
    }
    ops.push(go_to_folder_action());
    for (id, label, verb, kind) in folder_view_commands() {
        if let Ok(spec) = ActionSpec::operation(id, label, kind, "files.folder-command", &verb) {
            ops.push(spec.with_contract(if kind == OperationKind::View {
                ActionContract::view()
            } else {
                ActionContract::default()
            }));
        }
    }
    for (id, label, verb) in [
        ("add-favorite", "Add to Favourites", "add-favorite"),
        (
            "remove-favorite",
            "Remove from Favourites",
            "remove-favorite",
        ),
    ] {
        if let Ok(spec) = ActionSpec::operation(
            id,
            label,
            OperationKind::Effect,
            "files.folder-command",
            &verb,
        ) {
            ops.push(spec.with_contract(ActionContract {
                permission: PermissionRequirement::Write,
                retry: RetryPolicy::AtMostOnce,
                ..ActionContract::default()
            }));
        }
    }
    ops
}

fn folder_view_commands() -> [(&'static str, &'static str, &'static str, OperationKind); 17] {
    [
        ("up", "Parent folder", "up", OperationKind::View),
        ("back", "Back", "back", OperationKind::View),
        ("forward", "Forward", "forward", OperationKind::View),
        ("recent", "Show recent files", "recent", OperationKind::View),
        (
            "open-terminal",
            "Open terminal here",
            "terminal",
            OperationKind::Effect,
        ),
        (
            "toggle-hidden",
            "Toggle hidden files",
            "hidden",
            OperationKind::View,
        ),
        ("list", "Use list view", "list", OperationKind::View),
        ("grid", "Use grid view", "grid", OperationKind::View),
        (
            "sort-name",
            "Sort by name",
            "sort-name",
            OperationKind::View,
        ),
        (
            "sort-size",
            "Sort by size",
            "sort-size",
            OperationKind::View,
        ),
        (
            "sort-type",
            "Sort by type",
            "sort-type",
            OperationKind::View,
        ),
        (
            "sort-modified",
            "Sort by modified date",
            "sort-modified",
            OperationKind::View,
        ),
        (
            "sort-ascending",
            "Sort ascending",
            "sort-ascending",
            OperationKind::View,
        ),
        (
            "sort-descending",
            "Sort descending",
            "sort-descending",
            OperationKind::View,
        ),
        (
            "zoom-in",
            "Increase item size",
            "zoom-in",
            OperationKind::View,
        ),
        (
            "zoom-out",
            "Decrease item size",
            "zoom-out",
            OperationKind::View,
        ),
        (
            "keyboard-shortcuts",
            "Keyboard shortcuts",
            "shortcuts",
            OperationKind::View,
        ),
    ]
}

#[allow(
    clippy::expect_used,
    reason = "a path string payload is always serializable"
)]
fn go_to_folder_action() -> ActionSpec {
    ActionSpec::typed_operation(
        "go-to",
        "Go to folder…",
        "absolute folder path, e.g. /data/in",
        OperationKind::View,
        "files.go-to",
        vec![ActionParam::required(
            "path",
            ParamType::Text,
            "Absolute path of the folder to show",
        )],
        |args: &ActionArgs| {
            EncodedAction::encode_operation("files.go-to", &args.text("path").unwrap_or(""))
                .expect("folder path is serializable")
        },
    )
}

/// Places for [`crate::resource::FileExplorer::children`]: Open is a View.
/// This catalogue is still loaded from disk. Hosted pin and unpin edits are
/// document actions on the sink, not a second action tree beside publish.
pub(crate) fn published_place_resources(favorites: &[favorites::Favorite]) -> Vec<Resource> {
    favorites
        .iter()
        .filter_map(|favorite| {
            let resource_id = crate::search_provider::place_resource_id(&favorite.path);
            let open = ActionSpec::operation(
                crate::search_provider::OPEN_ACTION,
                "Open",
                OperationKind::View,
                "files.open",
                &resource_id,
            )
            .ok()?
            .with_contract(ActionContract::view());
            Some(
                Resource::new(resource_id, "files.place", favorite.label.clone()).with_action(open),
            )
        })
        .collect()
}

/// Template create Effects for [`crate::resource::FileExplorer::children`].
/// The catalogue is the user's Templates directory, not the live folder listing.
pub(crate) fn published_template_resources() -> Vec<Resource> {
    let Some(template_dir) = templates::templates_dir() else {
        return Vec::new();
    };
    templates::list_templates(&template_dir)
        .into_iter()
        .filter_map(|template| {
            let template_path = template.to_string_lossy().into_owned();
            let template_id = template_resource_id(&template_path);
            let create = ActionSpec::operation(
                "create",
                templates::menu_label(&template),
                OperationKind::Effect,
                "files.new-from-template",
                &template_id,
            )
            .ok()?
            .with_contract(ActionContract {
                permission: PermissionRequirement::Write,
                retry: RetryPolicy::AtMostOnce,
                ..ActionContract::default()
            });
            Some(
                Resource::new(
                    template_id,
                    "files.template",
                    templates::menu_label(&template),
                )
                .with_action(create),
            )
        })
        .collect()
}

/// Favourites snapshot for Resource publish when the widget is not the door.
pub(crate) fn load_published_places() -> Vec<favorites::Favorite> {
    let Some(home) = tilo_paths::home() else {
        return Vec::new();
    };
    let dirs = favorites::UserDirs::from_system();
    match config::config_dir() {
        Some(dir) => favorites::load(&dir, &home, &dirs),
        None => favorites::defaults(&home, &dirs),
    }
}

fn short_fingerprint(key: &[u8; 32]) -> String {
    hex_prefix(key, 6)
}

fn short_uuid(id: uuid::Uuid) -> String {
    let simple = id.simple().to_string();
    simple.get(..8).unwrap_or(&simple).to_string()
}

fn format_timestamp(timestamp_ms: u64) -> String {
    i64::try_from(timestamp_ms)
        .ok()
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map_or_else(
            || t("files-time-unknown"),
            |timestamp| crate::i18n::format_datetime(timestamp.with_timezone(&chrono::Local)),
        )
}

fn certified_device_detail(device: &crate::identity_directory::TrustedDevice) -> String {
    let caps = &device.capabilities;
    let mut capabilities = Vec::new();
    if caps.can_own_workspaces {
        capabilities.push(t("files-device-cap-own"));
    }
    if caps.can_admit_devices {
        capabilities.push(t("files-device-cap-admit"));
    }
    if caps.can_act_as_host {
        capabilities.push(t("files-device-cap-host"));
    }
    if caps.can_read_all_workspaces {
        capabilities.push(t("files-device-cap-read-all"));
    }
    if capabilities.is_empty() {
        capabilities.push(t("files-device-cap-member"));
    }
    let expiry = device.expires_at_ns.map_or_else(
        || t("files-no-expiry"),
        |deadline| {
            t_args(
                "files-expires",
                &[arg("time", format_timestamp(deadline / 1_000_000))],
            )
        },
    );
    t_args(
        "files-certified-device-detail",
        &[
            arg("device", device.label.clone()),
            arg("fingerprint", short_fingerprint(device.device.as_bytes())),
            arg("capabilities", capabilities.join(", ")),
            arg("issued", format_timestamp(device.issued_at_ns / 1_000_000)),
            arg("expiry", expiry),
        ],
    )
}

fn fluent_count(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn transfer_phase_label(phase: crate::model::TransferPhase) -> String {
    t(match phase {
        crate::model::TransferPhase::Idle => "files-transfer-idle",
        crate::model::TransferPhase::Queued => "files-transfer-queued",
        crate::model::TransferPhase::Downloading => "files-transfer-downloading",
        crate::model::TransferPhase::Uploading => "files-transfer-uploading",
        crate::model::TransferPhase::Verifying => "files-transfer-verifying",
    })
}

fn grant_status_label(status: crate::model::ShareGrantStatus) -> String {
    t(match status {
        crate::model::ShareGrantStatus::Active => "files-grant-active",
        crate::model::ShareGrantStatus::Expired => "files-grant-expired",
        crate::model::ShareGrantStatus::Revoked => "files-grant-revoked",
        crate::model::ShareGrantStatus::Exhausted => "files-grant-exhausted",
    })
}

fn conflict_strategy_label(strategy: collab_types::ConflictStrategy) -> String {
    t(match strategy {
        collab_types::ConflictStrategy::KeepCurrent => "files-strategy-keep-current",
        collab_types::ConflictStrategy::UseOther => "files-strategy-use-other",
        collab_types::ConflictStrategy::KeepBoth => "files-strategy-keep-both",
        collab_types::ConflictStrategy::Merge => "files-strategy-merge",
    })
}

fn share_permission_label(permission: collab_types::FilePermission) -> String {
    match permission {
        collab_types::FilePermission::View => t("files-share-choice-view"),
        collab_types::FilePermission::Edit => t("files-share-choice-edit"),
    }
}

fn share_expiry_label(expiry: crate::sharing::ShareExpiryChoice) -> String {
    t(match expiry {
        crate::sharing::ShareExpiryChoice::Day => "files-share-choice-day",
        crate::sharing::ShareExpiryChoice::Week => "files-share-choice-week",
        crate::sharing::ShareExpiryChoice::Month => "files-share-choice-month",
        crate::sharing::ShareExpiryChoice::Never => "files-share-choice-never",
    })
}

fn named_delivery_status_key(delivery: collab_types::NamedShareDeliveryStateV1) -> &'static str {
    match delivery {
        collab_types::NamedShareDeliveryStateV1::Pending => "files-share-created-pending",
        collab_types::NamedShareDeliveryStateV1::Delivered => "files-share-created-delivered",
    }
}

/// Explicit initial presentation data for an embedded Files view.
/// Paths here are data, not authorization; the host must separately enforce its grants.
/// Injected views never discover ambient accounts, identities or workspace roots.
#[derive(Clone)]
pub struct StartupInputs {
    pub initial_dir: PathBuf,
    pub workspace_roots: Vec<PathBuf>,
    pub record_roots: Vec<PathBuf>,
    pub remote_accounts: Vec<crate::remote_accounts::RemoteAccount>,
    pub trusted_humans: crate::identity_directory::TrustedHumanDirectory,
}

/// Exact document-open route supplied by a hosting shell.
#[derive(Clone)]
pub enum DocumentOpenRoute {
    /// This embedding deliberately provides no document owner.
    Unavailable,
    /// Legacy synchronous host route, retained for standalone embeddings.
    Immediate(Rc<RefCell<DocumentOpener>>),
    /// The shell drains a capability-bound request after the tile releases its
    /// record loan. A different app cannot construct the shell's matching
    /// capability.
    Deferred(ui::widget::DocumentOpenCapability),
}

impl HostedFileServices {
    /// Create the one deferred route whose capability the hosting shell will
    /// retain and validate before it reads or publishes a document.
    #[must_use]
    pub fn deferred_document_open() -> Self {
        Self {
            thumbnails: None,
            document_opener: DocumentOpenRoute::Deferred(
                ui::widget::DocumentOpenCapability::issue(),
            ),
            open_with: None,
        }
    }

    #[must_use]
    pub fn accepts_document_open_capability(
        &self,
        capability: &ui::widget::DocumentOpenCapability,
    ) -> bool {
        matches!(
            &self.document_opener,
            DocumentOpenRoute::Deferred(expected) if expected == capability
        )
    }
}

/// Optional services supplied by an embedding host. Absence never selects
/// the standalone thumbnail cache or global document opener.
#[derive(Clone)]
pub struct HostedFileServices {
    pub thumbnails: Option<Arc<ThumbnailService>>,
    pub document_opener: DocumentOpenRoute,
    /// Lists and launches "Open With" applications; absent means the
    /// submenu offers nothing rather than guessing.
    pub open_with: Option<crate::open_with::SharedOpenWithRoute>,
}

enum FileServices {
    Legacy,
    Hosted(HostedFileServices),
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each boolean is independent named UI state for loading, clipboard, rename, layout, or visibility"
)]
pub struct FileExplorerWidget {
    framework_history: ui::history_mode::checked_files::FileHistorySource,
    framework_history_binding: Option<(PathBuf, collab_types::FileRecordId)>,
    services: FileServices,
    startup_accounts: Option<Vec<crate::remote_accounts::RemoteAccount>>,
    current_path: PathBuf,
    home_path: Option<PathBuf>,
    entries: Vec<Entry>,
    /// Multi-selection over the displayed entry list.
    selection: SelectionModel,
    error: Option<String>,
    is_loading: bool,
    last_click: Option<(usize, Instant)>,
    sort_by: SortBy,
    sort_order: SortOrder,
    view_mode: ViewMode,
    /// Back/forward navigation history (paths only).
    history: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// Clipboard for cut/copy → paste. `true` = cut (move on paste).
    clipboard: Vec<PathBuf>,
    clipboard_cut: bool,
    /// The exact Trash entries created by the last local delete, captured only
    /// when the host exposed an unambiguous before/after snapshot. It is
    /// process-local because a filesystem mutation cannot be honestly replayed
    /// through collaborative history.
    last_trash_undo: Option<Vec<PathBuf>>,
    /// Transient status line (operation results), shown in the footer.
    status: Option<String>,
    /// Filesystem/xattr changes have no winit event of their own. The widget
    /// asks the host for this low-frequency wake so sync/availability/share/
    /// history badges update without burning an animation frame loop.
    next_metadata_refresh: Instant,
    /// When `Some(i)`, an inline rename of displayed entry `i` is in
    /// progress: the in-place editor is drawn over row/cell `i`. `None` = not
    /// renaming.
    renaming: Option<usize>,
    /// Editable name held only for the lifetime of the rename interaction;
    /// `commit_rename` renames the file to this value. Rename text is device
    /// local UI state, not collaborative document data or file metadata.
    rename_buffer: String,
    /// Set when a rename just began so the next `sync` seeds the local field
    /// with `rename_buffer` before it receives user input.
    rename_seed_pending: bool,
    /// The in-place rename editor: a persistent, process-local `TextInput`
    /// (so its caret/focus survive the per-frame rebuild) drawn **over the
    /// selected row/cell** — not in a bottom bar. Filesystem observations and
    /// a transient proposed name must never be written to the shared UI doc.
    rename_input: TextInput,
    /// Where the file view wants the rename editor drawn: the on-screen rect of
    /// the renaming row's name (list) or label (grid), published by `FileView`
    /// each paint. `None` when not renaming or the row is scrolled off-screen.
    rename_rect: Rc<Cell<Option<vello::kurbo::Rect>>>,
    /// Set on `begin_rename` so the first `sync` after the seed selects the
    /// whole name (typing then replaces it, Finder-style).
    rename_select_pending: bool,
    rect: vello::kurbo::Rect,
    /// The responsive body: `[ places | file view ]`. Held persistently (NOT
    /// rebuilt inside `build_layout`) so its docked/push state and eased
    /// reveal survive the per-structure-change rebuild of the panes — the two
    /// panes are swapped in via `NavShell::set_sidebar` / `set_content` each
    /// rebuild. Wide docks the places list beside the file view; narrow shows
    /// one at a time (places → tap a place → the file view pushes in with a
    /// Back arrow). See the module note on live child swap in `ui::NavShell`.
    nav: NavShell,
    /// The single top bar: nav cluster + the editable path bar + view/sort/menu
    /// controls. Held **persistently** (not rebuilt each frame) because the path
    /// bar carries live edit state — the in-progress typed path and its
    /// completion popover — that must survive the per-structure-change rebuild.
    toolbar: crate::toolbar::FileToolbar,
    /// Transient floating status bubble (bottom-right) — the item count /
    /// operation status + selection summary, surfaced on a real change or on
    /// hover then faded, in place of a permanent bottom footer strip so the
    /// file list spans to the window bottom.
    status_bubble: ui::StatusBubble,
    /// Last label the bubble showed, so it is re-poked only on a real change.
    last_status_label: String,
    /// `--shots` forces the bubble fully visible so a still frame captures it.
    shots_status: bool,
    /// Shared indeterminate progress indicator for the loading state. Its
    /// reduced-motion behaviour and frame scheduling come from `Spinner`.
    state_spinner: Spinner,
    /// Retained shared control positioned beneath the painted failure state.
    /// It publishes the same local Refresh operation as other retry routes.
    state_retry: Button,
    /// Set once `build_layout` has composed the panes into `nav`. Used to
    /// detect "hosted in a `TabBar` that never ran `build_layout` for us yet" so
    /// paint can flag a structure rebuild (replaces the old `chrome_below`
    /// None-sentinel).
    composed_once: bool,
    /// Command queue the interactive children push into.
    cmds: CommandSink,
    /// Set when applying a command changed structure; consumed by
    /// `take_structure_dirty` so the App rebuilds the composition.
    structure_dirty: bool,
    /// Framework dispatcher when hosted; local action host otherwise.
    /// Standalone, `--shots`, and unit tests leave `framework` empty.
    actions: crate::resource::ActionSink,
    /// Document edits queued until `sync` can submit them through the sink.
    pending_shared: Vec<crate::resource::FilesDocAction>,
    /// Set while adopting a presented document so that adoption does not
    /// queue another edit.
    adopting_shared: bool,
    /// Bookmark paths last copied in from the shared document. Only these
    /// are removed when a later document drops them; disk defaults stay.
    adopted_bookmarks: Vec<String>,
    /// Right-click menu, owned here (not in the rebuilt composition) so it
    /// stays open across frames. `menu_commands[i]` is the command to run
    /// for menu item `i` (None for separators).
    context_menu: ContextMenu,
    menu_commands: Vec<Option<FileCommand>>,
    /// The "New File ▸" flyout submenu, populated from the Templates dir when
    /// the main menu opens; empty (and closed) when there are no templates.
    submenu: ContextMenu,
    submenu_commands: Vec<Option<FileCommand>>,
    /// Set while draining commands from a right-press when the press landed on
    /// a sidebar favourite, so the right-press handler opens the favourite menu
    /// (not the file context menu) at the cursor. Consumed each right-press.
    pending_fav_menu: Option<PathBuf>,
    /// Last cursor position in logical coords (for menu placement + the
    /// context-menu hit-test).
    last_cursor: Point,
    /// Shared, persistent favourites + drag-reorder state (see
    /// [`crate::places_sidebar`]). Cloned into the sidebar each rebuild.
    favs: FavStore,
    /// Per-user config dir for persisting favourites + zoom (`None` when the
    /// environment has no config dir — the UI still works, just doesn't persist).
    config_dir: Option<PathBuf>,
    /// Icon/row zoom step (see [`crate::config`]); `0` = default.
    zoom_level: i32,
    /// Whether dotfiles are shown.
    show_hidden: bool,
    /// The user's real, localized XDG user directories (Documents, Downloads,
    /// …). Drives the standard favourites so a German desktop shows
    /// *Dokumente / Bilder / Musik*, not hard-coded English names.
    user_dirs: favorites::UserDirs,
    /// The Tilo collaborative-workspace roots the FUSE driver mounts, surfaced as
    /// first-class System locations in the sidebar. Resolved once at startup.
    workspace_roots: Vec<PathBuf>,
    /// Durable global-search projections. These are retained for the widget's
    /// lifetime: opening Ctrl+K clones cheap handles and never rescans Files'
    /// sources. The monotonically increasing source revision is advanced only
    /// by an explicit Files refresh/mutation.
    places_search_revision: u64,
    collaborative_search_revision: Cell<u64>,
    remote_accounts_search_revision: u64,
    places_search: crate::search_provider::PlacesSearchProvider,
    collaborative_search: crate::search_provider::CollaborativeRecordSearchProvider,
    /// One bounded mount page is admitted at a time. `RefCell` lets opening
    /// global search advance a continuation without making its query path
    /// rescan or materialise the whole workspace.
    collaborative_record_pager: RefCell<crate::model::WorkspaceRecordIndexPager>,
    remote_accounts_search: crate::search_provider::RemoteAccountSearchProvider,
    /// The row a drag started on. `Some(path)` while a drag is live; a floating
    /// ghost tracks the cursor. On release the app resolves the drop: onto a
    /// folder row (or the Up target) moves [`Self::drag_paths`] there; over the
    /// sidebar pins a folder to Favourites. `None` when no drag is in flight.
    pin_drag: Option<PathBuf>,
    /// All paths carried by the live drag — the pressed row, or the whole
    /// multi-selection when the drag started on a row that was part of it.
    drag_paths: Vec<PathBuf>,
    /// A press on a row waiting to become a drag: `(press point, path,
    /// is_dir)`. Cleared by the release, or by the first move past the
    /// threshold, which starts the drag.
    armed_drag: Option<(Point, PathBuf, bool)>,
    /// The multi-selection captured at the moment of a press, before the press's
    /// click collapses the selection to the single row. If a drag then starts
    /// from a row that was in this set, the drag carries the whole set (so a
    /// user can drag several selected items at once). Empty otherwise.
    drag_seed: Vec<PathBuf>,
    /// Whether the row the live drag started on is a folder (from the drag-begin
    /// signal, not a filesystem probe). Only a single dragged folder can be
    /// pinned to the sidebar.
    drag_origin_is_dir: bool,
    /// The folder currently under the cursor during a drag (a valid drop
    /// target), so it can be highlighted. `None` when over no target.
    drag_hover: Option<PathBuf>,
    nearby_send: crate::nearby_send::NearbySendState,
    nearby_send_path: Option<PathBuf>,
    shortcut_menu: crate::shortcut_menu::ShortcutMenuState,
    /// Read-only sharing/history sheet sourced entirely from bounded FUSE
    /// projections. `None` means no sheet is open.
    collab_panel: Option<CollabPanel>,
    collab_panel_focused: bool,
    /// Single background share request, run on the app runtime. It never
    /// touches widget state; update/paint/event polling adopts its bounded
    /// result on the UI thread.
    share_result: ui::LatestRequest<Result<crate::sharing::ShareActionResult, String>>,
    share_pending_path: Option<PathBuf>,
    /// Admission of a received bearer link, run on the app runtime.
    received_share: ui::LatestRequest<Result<collab_types::RecipientSubscriptionStatusV1, String>>,
    /// Host-projected names keyed by certified human roots. Labels affect
    /// presentation only; every command and request continues to carry the
    /// root key.
    trusted_humans: crate::identity_directory::TrustedHumanDirectory,
    history_result: Arc<std::sync::Mutex<Option<Result<(), String>>>>,
    history_pending_path: Option<PathBuf>,
    history_pending_success: Option<String>,

    merge_preflight_result: MergePreflightResult,
    merge_preflight_pending: Option<(uuid::Uuid, collab_types::FileVersionId)>,
    resource_invocation: Option<tilo_actions::InvocationId>,
    /// The value the operation being answered returns, when it returns one.
    operation_result: Option<serde_json::Value>,
    resource_result: Option<tilo_actions::InvocationUpdate>,
    /// Settings' config root remote accounts are read from; `None` reads
    /// none (embedded and test views), so no ambient store is touched.
    accounts_root: Option<PathBuf>,
    /// The accounts as last read from `accounts_root`.
    accounts_cache: Vec<crate::remote_accounts::RemoteAccount>,
    host_intent: Option<ui::widget::AppHostIntent>,
    /// A one-shot shell-owned document admission. The reply is strong only
    /// while this Files widget still owns the request; the shell gets a weak
    /// witness and cannot act after this widget has gone away.
    pending_document_open: Option<(u64, Rc<RefCell<ui::widget::DocumentOpenReply>>)>,
    /// A file handed to the desktop's default handler, until it answers.
    pending_launch: Option<(PathBuf, ui::portal::LaunchChannel)>,
    next_document_open_token: u64,
    /// Folder rows the file view publishes each layout, so a drag's drop point
    /// can be resolved to the folder it lands on (the view never sees the
    /// release — the app captures the pointer for the duration of the drag).
    drop_targets: crate::views::file_view::DropTargets,
    /// The content pane's live scroll position, owned here so it survives the
    /// per-frame rebuild. Each rebuilt list/grid `Scrollable` binds to this
    /// cell (see [`ui::widget::Scrollable::with_persisted_offset`]); without
    /// it, selecting a row, typing in search, or changing sort — all of which
    /// rebuild the composition — would snap the list back to the top. Reset to
    /// `(0, 0)` whenever the directory changes (a new folder starts at top).
    scroll_offset: Rc<Cell<(f32, f32)>>,
    /// The shared, off-thread thumbnail service (content-addressed cache +
    /// worker pool). Cloned into each rebuilt file view so a folder of images /
    /// PDFs / ebooks shows real previews (and workspace files show the host's
    /// gossiped preview) without janking on the UI thread. `None` when no
    /// writable cache directory could be found — the file view then falls
    /// back to type badges.
    thumbs: Option<Arc<ThumbnailService>>,
    /// App-lived memo turning a decoded thumbnail into a `peniko` image once, so
    /// a scrolling grid never rebuilds the GPU blob per frame.
    /// Recently opened or changed files, newest first.
    recent: crate::recent::RecentFiles,
    /// "Open With" candidates per file type, asked of the host once per
    /// listing rather than once per row.
    open_with_cache: RefCell<HashMap<String, crate::open_with::OpenWithCandidates>>,
    /// The shared searchable picker behind "Other Application…"; its rows are
    /// the published open-with actions of [`Self::app_chooser_target`].
    app_chooser: ui::widget::command_palette::CommandPalette,
    app_chooser_target: Option<PathBuf>,
    thumb_imgs: Rc<RefCell<HashMap<ThumbKey, PenikoImage>>>,
}

impl Default for FileExplorerWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl FileExplorerWidget {
    pub fn new() -> Self {
        // The same home the start directory is resolved against
        // (`crate::model::default_start_dir`), and for the same reason.
        // Under a sandbox this process's `HOME` is its own per-package
        // directory — `/run/tilo/app` — while the directory actually
        // opened is the user's real home, bound by a grant and named by
        // `TILO_USER_HOME`. Reading `tilo_paths::home()` here made the two
        // disagree, and the breadcrumb is where a disagreement shows:
        // `format_path` abbreviates a path to `~` only when it lies
        // under the home it was handed, so a home it does not recognise
        // renders as an absolute trail from the filesystem root.
        let mut widget = Self::with_persistence(config::config_dir(), crate::model::user_home());
        widget.accounts_root = accounts::config_root();
        widget.refresh_external_search_catalogs();
        widget
    }

    /// Keep the framework dispatcher. Hosted document edits then go through
    /// [`Dispatcher::apply_now_with`](ui::framework::Dispatcher::apply_now_with).
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::FileExplorer>,
    ) {
        self.actions.attach(dispatcher);
    }

    /// Mark this explorer as a framework-hosted surface so
    /// [`Widget::resources`] defers to `tilo_resource::publish`.
    ///
    /// Tests use this without a live dispatcher. The sink then records the
    /// edit on its local action host. Production [`Self::attach_framework`]
    /// stores the dispatcher and submits with `apply_now_with`.
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn set_framework_hosted(&mut self, hosted: bool) {
        self.actions.set_hosted(hosted);
    }

    #[allow(
        dead_code,
        reason = "called from this crate's tests; dependents compile the library without cfg(test)"
    )]
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[cfg(test)]
    pub(crate) fn last_shared_action(&self) -> Option<&ui::tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    fn share_document_edit(&mut self, action: crate::resource::FilesDocAction) {
        if self.adopting_shared || !self.actions.is_hosted() {
            return;
        }
        self.pending_shared.push(action);
    }

    fn share_sort(&mut self) {
        self.share_document_edit(crate::resource::FilesDocAction::SetSort {
            key: crate::doc::sort_key_token(self.sort_by).to_owned(),
            order: crate::doc::sort_order_token(self.sort_order).to_owned(),
        });
    }

    fn share_bookmarks(&mut self) {
        self.share_document_edit(crate::resource::FilesDocAction::SetBookmarks {
            paths: self.bookmark_paths(),
        });
    }

    fn bookmark_paths(&self) -> Vec<String> {
        self.favs
            .borrow()
            .favorites
            .iter()
            .map(|favorite| favorite.path.display().to_string())
            .collect()
    }

    fn flush_shared_edits(&mut self, doc: &mut automerge::AutoCommit) {
        let pending = std::mem::take(&mut self.pending_shared);
        if !self.actions.is_hosted() {
            return;
        }
        for action in pending {
            self.actions.dispatch(doc, &action);
        }
    }

    /// Copy shared view preferences and bookmarks onto the widget.
    ///
    /// An empty document (`shared == false`) leaves the local sidebar and
    /// view alone. Listing rows, the current path, and selection are not
    /// touched.
    pub(crate) fn adopt_shared_state(&mut self, state: &crate::resource::FilesState) {
        if !state.shared {
            return;
        }
        self.adopting_shared = true;
        self.set_view_mode(crate::doc::parse_view_mode(&state.view_mode));
        self.set_sort_by(crate::doc::parse_sort_key(&state.sort_key));
        if self.sort_order != crate::doc::parse_sort_order(&state.sort_order) {
            self.sort_order = crate::doc::parse_sort_order(&state.sort_order);
            sort_entries(self.sort_by, self.sort_order, &mut self.entries);
        }
        self.set_zoom(state.zoom);
        if self.show_hidden != state.show_hidden {
            self.show_hidden = state.show_hidden;
            self.refresh();
        }
        self.adopt_bookmarks(&state.bookmarks);
        self.adopting_shared = false;
    }

    fn adopt_bookmarks(&mut self, paths: &[String]) {
        let gone: Vec<std::path::PathBuf> = self
            .adopted_bookmarks
            .iter()
            .filter(|path| !paths.iter().any(|kept| kept == *path))
            .map(std::path::PathBuf::from)
            .collect();
        let home = self
            .home_path
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        let mut changed = false;
        for path in &gone {
            let mut favs = self.favs.borrow_mut();
            changed |= favorites::remove(&mut favs.favorites, path);
        }
        for path in paths {
            let mut favs = self.favs.borrow_mut();
            changed |= favorites::add(
                &mut favs.favorites,
                std::path::PathBuf::from(path),
                &home,
                &self.user_dirs,
            );
        }
        if changed {
            self.save_favorites();
            self.refresh_places_search();
        }
        self.adopted_bookmarks = paths.to_vec();
    }

    /// Surface a bounded result from startup portals such as received-file
    /// admission without coupling those clients to widget internals.
    pub fn set_external_status(&mut self, status: impl AsRef<str>) {
        const MAX_STATUS_CHARS: usize = 512;
        self.status = Some(status.as_ref().chars().take(MAX_STATUS_CHARS).collect());
        self.structure_dirty = true;
    }

    /// Deterministic visual-test seam for the remote-account details sheet.
    /// Production paths always re-read the canonical store immediately before
    /// opening this surface; the seam never persists a sample account.
    #[doc(hidden)]
    pub fn debug_show_remote_account(
        &mut self,
        account: &crate::remote_accounts::RemoteAccount,
        reauthenticate: bool,
    ) {
        self.show_remote_account_panel(account, reauthenticate);
    }

    fn show_remote_account_panel(
        &mut self,
        account: &crate::remote_accounts::RemoteAccount,
        reauthenticate: bool,
    ) {
        let title = if reauthenticate {
            format!("Sign in again: {}", account.label)
        } else {
            format!("{} files", account.label)
        };
        let mut lines = vec![
            format!("{} · {}", account.kind.label(), account.host),
            format!("Status: {}", account.health.label()),
            account.health.unavailable_reason().to_string(),
        ];
        if let Some(mount) = &account.mount {
            lines.push(format!("Folder: {}", mount.display()));
        }
        if reauthenticate {
            lines.push(
                "Open Settings → External Accounts and choose Sign in again. Credentials stay in the system vault and are never shown to Files."
                    .into(),
            );
        }
        self.show_collab_panel(CollabPanel {
            history: None,
            title,
            summary: "Remote account details".into(),
            lines,
            action: None,
        });
    }

    /// Construct with an explicit persistence directory + home, resolving the
    /// XDG user dirs from the real environment. `new` uses the real per-user
    /// config dir; tests pass a `tempdir` (or `None`) so they never read or
    /// write the user's real favourites/zoom state.
    #[doc(hidden)]
    pub fn with_persistence(config_dir: Option<PathBuf>, home: Option<PathBuf>) -> Self {
        Self::with_persistence_dirs(config_dir, home, favorites::UserDirs::from_system())
    }

    /// Construct with an explicit persistence directory, home, and injected
    /// XDG user directories. Tests inject a known (e.g. German-localized) set
    /// so the standard favourites are deterministic without depending on the
    /// build machine's locale.
    #[doc(hidden)]
    #[allow(
        clippy::too_many_lines,
        reason = "the constructor declares the complete stable widget state in one auditable place"
    )]
    pub fn with_persistence_dirs(
        config_dir: Option<PathBuf>,
        home: Option<PathBuf>,
        user_dirs: favorites::UserDirs,
    ) -> Self {
        Self::with_inputs(config_dir, home, user_dirs, None, FileServices::Legacy)
    }

    /// Build with host-supplied startup data and legacy thumbnail/opener services.
    /// Use [`Self::with_startup_inputs_and_services`] to supply services explicitly,
    /// including their absence.
    pub fn with_startup_inputs(inputs: StartupInputs) -> Self {
        Self::with_inputs(
            None,
            None,
            favorites::UserDirs::default(),
            Some(inputs),
            FileServices::Legacy,
        )
    }

    /// Build with exact host services; absent services remain unavailable.
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

    #[allow(
        clippy::too_many_lines,
        reason = "Declare the complete stable widget state in one auditable constructor."
    )]
    fn with_inputs(
        config_dir: Option<PathBuf>,
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

        let zoom_level = config_dir.as_deref().map_or(0, config::load_zoom);
        // Favourites: persisted list when we have a home + config dir, else the
        // built-in defaults so the sidebar is never empty. Standard entries
        // follow the user's real, localized XDG directories.
        let favorites = match (home.as_deref(), config_dir.as_deref()) {
            (Some(h), Some(dir)) => favorites::load(dir, h, &user_dirs),
            (Some(h), None) => favorites::defaults(h, &user_dirs),
            (None, _) => Vec::new(),
        };
        let show_hidden = false;
        let workspace_roots = inputs.workspace_roots;
        // Construct each durable namespace once. Subsequent refreshes are
        // revision-gated by `refresh_external_search_catalogs`, never by palette input.
        let places_search = crate::search_provider::PlacesSearchProvider::new(&favorites);
        let mut collaborative_record_pager =
            crate::model::WorkspaceRecordIndexPager::new(&inputs.record_roots);
        let initial_collaborative_page = collaborative_record_pager.next_page();
        let collaborative_search = crate::search_provider::CollaborativeRecordSearchProvider::new(
            &initial_collaborative_page.records,
        );
        let remote_accounts = inputs.remote_accounts;
        let remote_accounts_search =
            crate::search_provider::RemoteAccountSearchProvider::new(&remote_accounts);
        let mut entries = if injected {
            crate::model::read_dir_with_workspace_roots(&cwd, &workspace_roots)
        } else {
            read_dir_sync(&cwd)
        }
        .unwrap_or_default();
        if !show_hidden {
            entries.retain(|e| !e.name.starts_with('.'));
        }
        sort_entries(SortBy::Name, SortOrder::Ascending, &mut entries);
        let cmds = new_sink();
        let thumbs = match &services {
            FileServices::Legacy => build_thumb_service(config_dir.as_deref()).map(Arc::new),
            FileServices::Hosted(hosted) => hosted.thumbnails.clone(),
        };
        let trusted_humans = inputs.trusted_humans;
        let startup_accounts = injected.then_some(remote_accounts);
        let config_dir_recent = config_dir
            .as_deref()
            .map(crate::recent::RecentFiles::load)
            .unwrap_or_default();
        Self {
            services,
            startup_accounts,
            current_path: cwd,
            home_path: home,
            entries,
            selection: SelectionModel::new(),
            error: None,
            is_loading: false,
            last_click: None,
            sort_by: SortBy::Name,
            sort_order: SortOrder::Ascending,
            view_mode: ViewMode::List,
            history: Vec::new(),
            forward: Vec::new(),
            clipboard: Vec::new(),
            clipboard_cut: false,
            last_trash_undo: None,
            status: None,
            next_metadata_refresh: ui::time::now() + metadata_refresh::LIVE_INTERVAL,
            renaming: None,
            rename_buffer: String::new(),
            rename_seed_pending: false,
            rename_input: TextInput::local()
                .with_kind(ui::widget::TextInputKind::Name)
                .with_id("files.rename_input")
                .with_placeholder(&t("files-new-name-placeholder")),
            rename_rect: Rc::new(Cell::new(None)),
            rename_select_pending: false,
            rect: vello::kurbo::Rect::ZERO,
            // Placeholder panes; the real places list + file view are swapped
            // in on the first `build_layout`. The places list is the shell's
            // *sidebar*, not a content overview: docked beside the file view
            // when wide, an overlay drawer over it when narrow. Sidebar width
            // matches the historical fixed sidebar so the docked layout is
            // pixel-stable.
            nav: NavShell::sidebar_over_content(Box::new(Text::new("")), Box::new(Text::new("")))
                .with_sidebar_width(places_sidebar::SIDEBAR_WIDTH)
                // A narrow drawer should read as navigation, not leave a
                // sliver of obscured list columns competing for attention.
                // Keep the compact 200px desktop dock, but give phone-width
                // places enough room for labels and touch targets.
                .with_narrow_sidebar_width(320.0)
                .with_breakpoint(BODY_BREAKPOINT_PX)
                // The docked places list is permanent — no in-sidebar collapse chip;
                // the hamburger still summons the drawer when narrow.
                .hide_toggle_when_docked(),
            toolbar: crate::toolbar::FileToolbar::new(cmds.clone()),
            status_bubble: ui::StatusBubble::new(),
            last_status_label: String::new(),
            shots_status: false,
            state_spinner: Spinner::with_size(28.0),
            state_retry: Button::new("files.retry", &t("files-try-again"), None).on_effect({
                let sink = cmds.clone();
                move || crate::command::push(&sink, FileCommand::Refresh)
            }),
            composed_once: false,
            cmds,
            structure_dirty: false,
            actions: crate::resource::ActionSink::default(),
            pending_shared: Vec::new(),
            adopting_shared: false,
            adopted_bookmarks: Vec::new(),
            context_menu: ContextMenu::new(),
            menu_commands: Vec::new(),
            submenu: ContextMenu::new(),
            submenu_commands: Vec::new(),
            pending_fav_menu: None,
            last_cursor: Point::ZERO,
            favs: places_sidebar::new_store(favorites),
            config_dir,
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
            drag_origin_is_dir: false,
            drag_hover: None,
            nearby_send: crate::nearby_send::NearbySendState::default(),
            nearby_send_path: None,
            shortcut_menu: crate::shortcut_menu::ShortcutMenuState::default(),
            collab_panel: None,
            collab_panel_focused: false,
            share_result: ui::LatestRequest::new(),
            share_pending_path: None,
            received_share: ui::LatestRequest::new(),
            trusted_humans,
            framework_history: ui::history_mode::checked_files::FileHistorySource::default(),
            framework_history_binding: None,
            history_result: Arc::new(std::sync::Mutex::new(None)),
            history_pending_path: None,
            history_pending_success: None,

            merge_preflight_result: Arc::new(std::sync::Mutex::new(None)),
            merge_preflight_pending: None,
            resource_invocation: None,
            operation_result: None,
            resource_result: None,
            accounts_root: None,
            accounts_cache: Vec::new(),
            host_intent: None,
            pending_document_open: None,
            pending_launch: None,
            next_document_open_token: 0,
            drop_targets: Rc::new(std::cell::RefCell::new(Vec::new())),
            scroll_offset: Rc::new(Cell::new((0.0, 0.0))),
            thumbs,
            recent: config_dir_recent,
            open_with_cache: RefCell::new(HashMap::new()),
            app_chooser: ui::widget::command_palette::CommandPalette::commands(),
            app_chooser_target: None,
            thumb_imgs: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// The thumbnail context (service + peniko memo) handed to each rebuilt file
    /// view. `None` when no thumbnail service could be opened — the view then
    /// falls back to type badges.
    fn thumb_ctx(&self) -> Option<ThumbCtx> {
        Some(ThumbCtx {
            service: self.thumbs.clone()?,
            imgs: self.thumb_imgs.clone(),
        })
    }

    /// The current zoom scale multiplier (icon/row size).
    fn zoom_scale(&self) -> f64 {
        config::zoom_scale(self.zoom_level)
    }

    /// Persist the favourites list (best-effort).
    fn save_favorites(&self) {
        if let Some(dir) = self.config_dir.as_deref() {
            favorites::save(dir, &self.favs.borrow().favorites);
        }
    }

    /// Refresh an already-loaded private Places projection. This is deliberately
    /// called only by the favourite mutation paths, never by a palette query or
    /// normal CRDT `sync`.
    fn refresh_places_search(&mut self) {
        self.places_search_revision = self.places_search_revision.wrapping_add(1);
        let favorites = self.favs.borrow().favorites.clone();
        self.places_search
            .refresh(self.places_search_revision, &favorites);
    }

    /// Admit the next bounded collaborative mount page. Search state is reset
    /// only when the source authority generation changes; otherwise the
    /// retained `SearchCatalog` gets exactly the newly fetched rows.
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

    /// Refresh external metadata after an explicit directory refresh/navigation
    /// or completed remote operation. The mount portion consumes a single
    /// bounded continuation page, never its entire authority.
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
            self.status = Some("Collaborative search needs a refresh to continue.".to_string());
        }
        if let Some(root) = self.accounts_root.as_deref() {
            self.accounts_cache = crate::remote_accounts::load_in(root, self.home_path.as_deref());
        }
        self.remote_accounts_search_revision = self.remote_accounts_search_revision.wrapping_add(1);
        let accounts = self.remote_account_snapshot();
        self.remote_accounts_search
            .refresh(self.remote_accounts_search_revision, &accounts);
    }

    /// The entries currently shown. (There is no in-app search field — search
    /// is OS-owned per `crates/sdk/ui-core/HIG.md` §5 — so this is the sorted directory
    /// listing as-is.)
    pub fn displayed_entries(&self) -> Vec<Entry> {
        self.entries.clone()
    }

    /// The sole bridge from a durable/agent-visible resource id to a host
    /// path. Paths stay in the live presentation snapshot: action payloads,
    /// history and search identities carry only opaque ids and are refused
    /// when the row disappeared or was replaced.
    fn entry_resource_id(entry: &Entry) -> String {
        entry.record_id.map_or_else(
            || crate::search_provider::entry_resource_id(&entry.path),
            |id| crate::search_provider::record_resource_id(uuid::Uuid::from_bytes(id)),
        )
    }

    /// The resource identities (kind and id) of the single selected entry,
    /// most specific first: its collaborative record, then its path.
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
        self.entries
            .iter()
            .find(|entry| Self::entry_resource_id(entry) == resource_id)
            .map(|entry| entry.path.clone())
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

    /// Block until every in-flight thumbnail generation has settled (or the
    /// timeout elapses). Requests are enqueued during `draw`, so a caller draws
    /// once, calls this, then draws the frame it keeps — used by the `--shots`
    /// pre-warm so a headless grid captures the real previews, never the
    /// placeholders. Never call on a live UI thread.
    pub fn prewarm_thumbnails(&self, timeout: std::time::Duration) {
        if let Some(thumbs) = self.thumbs.as_deref() {
            thumbs.block_until_idle(timeout);
        }
    }

    /// Refresh the persistent top bar with the current navigation context
    /// (path, history availability, view mode) before it is laid out.
    fn update_toolbar(&mut self) {
        let nav = crate::toolbar::NavAvailability {
            back: !self.history.is_empty(),
            forward: !self.forward.is_empty(),
            up: self.current_path.parent().is_some() && !self.is_recent_view(),
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

    /// The **places** pane (the responsive body's master / overview): the
    /// modern Favourites / Places list. A single widget that owns
    /// drag-to-reorder for the favourites (via the shared, persistent
    /// `FavStore`) so a reorder survives the per-frame rebuild.
    fn build_places_pane(&self) -> Box<dyn Widget> {
        Box::new(
            PlacesSidebar::new(
                self.favs.clone(),
                self.current_path.clone(),
                self.cmds.clone(),
            )
            .with_cursor(self.last_cursor)
            .with_workspaces(self.workspace_roots.clone())
            .with_remote_accounts(
                self.remote_account_snapshot()
                    .into_iter()
                    .filter(|account| {
                        matches!(remote_folder_availability(account), Availability::Available)
                    })
                    .filter_map(|account| {
                        let badge = account.health.badge().map(str::to_owned);
                        account.mount.map(|mount| (account.label, mount, badge))
                    })
                    .collect(),
            ),
        )
    }

    /// The **content** pane (the responsive body's detail): the file list or
    /// grid (plus any error affordance), scrolling under a symmetric gutter.
    fn build_content_pane(&self, theme: &ui::Theme) -> Box<dyn Widget> {
        let sp = &theme.tokens.spacing;
        // Loading and failure are painted by the shared centered state surface
        // in `draw_state_surface`. Keep this structural slot empty: it avoids
        // a stale list beneath a failure while the surface owns the visual,
        // semantic, and recovery treatment consistently at every width.
        if self.is_loading || self.error.is_some() {
            return Box::new(Spacer::new());
        }
        let displayed = self.displayed_entries();
        let scale = self.zoom_scale();
        let text_scale = theme.text_scale;
        // `renaming` marks the item edited in place, publishing its label rect
        // into `rename_rect` for the host to draw the shared editor over. The
        // cursor seeds the view's pointer position so a press hit-tests
        // correctly on the frame it is rebuilt (see `FileView::with_cursor`).
        let view = FileView::new(&displayed, &self.selection, &self.cmds)
            .with_scale(scale)
            .with_text_scale(text_scale)
            .with_rename(self.renaming, self.rename_rect.clone())
            .with_cursor(self.last_cursor)
            .with_drop_targets(self.drop_targets.clone())
            .with_thumbs(self.thumb_ctx());
        let view_child: Box<dyn Widget> = match self.view_mode {
            ViewMode::List => {
                crate::views::list_view::build_list(view, self.sort_by, self.sort_order)
            }
            ViewMode::Grid => crate::views::grid_view::build_grid(view),
        };
        // The hamburger chip that summons the places drawer floats over this
        // pane's top-left in the narrow layout. `NavShell` reserves that band
        // itself, in the pass that positions the chip — reserving it here
        // instead would mean asking the shell how wide it was on the *previous*
        // frame, which is how the list used to be built 44px lower than it was
        // laid out and then jump the first time anything rebuilt it.
        // A plain (percent-height) column: this pane is the root of `NavShell`'s
        // inner layout, sized to the content rect, so it must FILL that rect
        // (`percent(1.0)` height). `with_fill_remaining_height` (flex-grow from
        // zero) needs a flex parent to distribute space and would collapse to
        // its content height here — leaving the `Scrollable` zero-height. Inside
        // this bounded column the `Scrollable` grows to take the space under the
        // optional error row.
        let mut view_col = Column::new()
            .with_vertical_padding(sp.s1)
            .with_horizontal_padding(sp.s3, sp.s3);
        // The list/grid scrolls: pin the scroll viewport to the *remaining*
        // height of the content column (grow-from-zero) rather than letting it
        // claim `percent(1.0)`. `fill_parent_height` takes exactly the space
        // left under the optional error row so a long directory overflows the
        // viewport and the wheel + scrollbar thumb move the list.
        view_col = view_col.push(Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_persisted_offset(self.scroll_offset.clone())
                .with_cursor(self.last_cursor)
                // The status pill is intentionally outside the scrollable's
                // interactive viewport. Besides making the count easy to
                // scan, that means it cannot obscure a file name, target, or
                // keyboard focus ring at the bottom of a compact grid.
                .with_bottom_content_inset(STATUS_BUBBLE_SAFE_INSET_PX)
                .with_child(view_child),
        ));
        Box::new(view_col)
    }

    /// The label for the floating status bubble: an operation status, else the
    /// item count, plus the selection summary when something is selected. The
    /// current path is shown ONCE — in the path bar at the top — so it is
    /// deliberately not repeated here. Empty string means nothing to surface.
    fn footer_label(&self) -> String {
        let count = self.entries.len();
        let left = self
            .status
            .clone()
            .unwrap_or_else(|| t_args("files-item-count", &[arg("count", fluent_count(count))]));
        let sel = self.selection.selected().len();
        if sel > 0 {
            t_args(
                "files-selected-count",
                &[arg("base", left), arg("count", fluent_count(sel))],
            )
        } else {
            left
        }
    }

    fn empty_state_message(&self) -> String {
        if self.is_recent_view() {
            t("files-empty-recent")
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

    fn bounded_state_prefix(value: &str, max_chars: usize) -> String {
        let count = value.chars().count();
        if count <= max_chars {
            return value.to_string();
        }
        let keep = max_chars.saturating_sub(1);
        format!("{}…", value.chars().take(keep).collect::<String>())
    }

    /// Concise, readable copy for the center state. The visual treatment has a
    /// deliberately finite line length, while the full path remains discoverable
    /// in the location bar and the root accessibility description.
    fn state_surface_copy(&self) -> Option<(String, String, bool)> {
        let location = Self::bounded_state_text(
            &self.current_path.display().to_string(),
            STATE_SURFACE_LOCATION_CHARS,
        );
        if self.is_loading {
            return Some((
                t("files-loading-title"),
                t_args("files-loading-detail", &[arg("path", location)]),
                false,
            ));
        }
        self.error.as_ref().map(|error| {
            // `read_current_dir` already wraps the OS failure in the localized
            // “Could not open this folder:” sentence. The state title carries
            // that context, so retain only the cause in the narrow visual line.
            let cause = error
                .rsplit_once(": ")
                .map_or(error.as_str(), |(_, cause)| cause);
            let location = Self::bounded_state_text(&location, 22);
            let error = Self::bounded_state_prefix(cause, 26);
            (
                t("files-load-error-title"),
                t_args(
                    "files-load-error-detail",
                    &[arg("path", location), arg("error", error)],
                ),
                true,
            )
        })
    }

    /// Draw the shared center-state primitive above the otherwise empty pane.
    /// Loading adds the toolkit spinner (which stops requesting frames in
    /// reduced-motion mode); failure uses the shared `EmptyState` visual plus a
    /// retained shared Button for the same typed Refresh command every input
    /// route uses.
    fn draw_state_surface(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let Some((title, detail, can_retry)) = self.state_surface_copy() else {
            return;
        };
        let rect = self.nav.overview_content_rect();
        if rect.width() <= 1.0 || rect.height() <= 1.0 {
            return;
        }
        if can_retry {
            // The shared empty state lays out the action as part of its
            // centred stack (measured title, detail and label, so the pill
            // clears the detail line at every text scale) and returns that
            // rect; the retained shared Button — which owns hover, focus,
            // keyboard activation and the typed Refresh command — is then
            // painted over it, so the geometry has one author and the
            // behaviour keeps the production control. Hover is suppressed on
            // the underlay because the Button draws its own.
            let action_rect = EmptyState::paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
                &EmptyStateContent {
                    icon: Some(ui::icons::Icon::FolderOpen),
                    title: &title,
                    message: Some(&detail),
                    action_label: Some(&t("files-try-again")),
                },
                Point::new(-1.0, -1.0),
            );
            let button_rect = action_rect.map_or(vello::kurbo::Rect::ZERO, |r| {
                // Never narrower than four minimum pointer targets, so a short
                // localized label still gets a comfortable retry button; widen
                // symmetrically so the Button still covers the underlay pill.
                let min_w = 4.0 * f64::from(theme.tokens.metrics.min_pointer_target);
                let w = r.width().max(min_w);
                vello::kurbo::Rect::from_center_size(r.center(), (w, r.height()))
            });
            self.state_retry.apply_layout_external(button_rect);
            self.state_retry
                .draw(scene, transform, text_renderer, theme, presence);
        } else {
            EmptyState::paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
                &EmptyStateContent {
                    icon: None,
                    title: &title,
                    message: Some(&detail),
                    action_label: None,
                },
                self.last_cursor,
            );
            let center = rect.center();
            let spinner_rect = vello::kurbo::Rect::from_center_size(
                Point::new(center.x, center.y - 62.0),
                (28.0, 28.0),
            );
            self.state_spinner.set_rect(spinner_rect);
            self.state_spinner
                .draw(scene, transform, text_renderer, theme, presence);
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

    /// Surface the complete built-in keyboard vocabulary in the same modal
    /// shell used for Properties, sharing and recovery.  The rows describe
    /// semantic primary-modifier bindings (Ctrl on most desktops, Command on
    /// macOS), rather than hard-coding a physical keyboard layout.
    fn show_keyboard_shortcuts_panel(&mut self) {
        self.show_collab_panel(CollabPanel {
            title: t("files-shortcuts-title"),
            summary: t("files-shortcuts-summary"),
            lines: [
                "files-shortcuts-primary",
                "files-shortcuts-navigate",
                "files-shortcuts-grid",
                "files-shortcuts-open",
                "files-shortcuts-location",
                "files-shortcuts-new-folder",
                "files-shortcuts-edit",
                "files-shortcuts-hidden",
                "files-shortcuts-size",
                "files-shortcuts-context",
                "files-shortcuts-close",
            ]
            .into_iter()
            .map(t)
            .collect(),
            action: None,
            history: None,
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
            CollabPanelAction::ApplySafeMerge {
                conflict_id,
                version_id,
            } => Some(FileCommand::ResolveConflict {
                conflict_id: *conflict_id,
                strategy: collab_types::ConflictStrategy::Merge,
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
            CollabPanelAction::ApplySafeMerge { .. } => Some(t("files-merge-apply-safe-detail")),
            CollabPanelAction::ChooseNearbyDestination { .. } => {
                Some(t("files-nearby-consent-detail"))
            }
            CollabPanelAction::RetryNearby { .. } => Some(t("files-nearby-retry-detail")),
        }
    }

    /// Spoken state for custom-painted surfaces which otherwise have no
    /// backing child widget. The stable accessible name on Files' root turns
    /// value changes into live announcements without stealing keyboard focus.
    fn accessibility_status(&self) -> String {
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

    /// `--shots`: force the status bubble fully visible so a single headless
    /// frame captures the floating chip.
    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
        self.status_bubble.poke(ui::time::now());
    }

    /// Draw the transient floating status bubble at the window's bottom-right —
    /// the reclaimed home of the old footer strip. It surfaces on a real change
    /// (dir load, selection, an operation result) or while the pointer hovers
    /// the bottom-right band, then fades. Colours come from the theme.
    fn draw_status_bubble(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        tr: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
    ) {
        let now = ui::time::now();
        let label = self.footer_label();
        if label != self.last_status_label {
            label.clone_into(&mut self.last_status_label);
            self.status_bubble.poke(now);
        }
        if label.is_empty() {
            return;
        }
        // `pill_size` / `draw_pill` hand the size to a scaling renderer, so
        // unscale the theme token once or the bubble paints at 4× at 200 %.
        let text_size = theme.tokens.typography.scale.sm / tr.text_scale();
        let (w, h) = ui::status_bubble::pill_size(&label, tr, text_size, 0.0);
        let margin = f64::from(theme.tokens.spacing.s4);
        let rect = vello::kurbo::Rect::new(
            self.rect.x1 - margin - w,
            self.rect.y1 - margin - h,
            self.rect.x1 - margin,
            self.rect.y1 - margin,
        );
        let zone =
            vello::kurbo::Rect::new(rect.x0 - 40.0, rect.y0 - 24.0, self.rect.x1, self.rect.y1);
        // Archive browsing is deliberately read-only. Unlike transient file
        // operation feedback, that fact remains visible for the whole visit.
        let archive_read_only = crate::archive::location(&self.current_path).is_some();
        let over = self.shots_status || archive_read_only || zone.contains(self.last_cursor);
        self.status_bubble.set_hover(over, now);
        // A status change is still useful with reduced motion; only its
        // opacity animation is removed. The next normal paint will settle it
        // to hidden after its linger instead of asking the host for a fade
        // frame loop.
        let alpha = if theme.reduce_motion {
            if self.status_bubble.is_visible(now) {
                1.0
            } else {
                0.0
            }
        } else {
            self.status_bubble.alpha(now)
        };
        if alpha <= 0.0 {
            return;
        }
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            alpha,
            transform,
            &rect.inflate(12.0, 14.0),
        );
        ui::status_bubble::draw_pill(scene, transform, tr, theme, rect, &label, text_size);
        scene.pop_layer();
    }

    /// An open Places drawer is modal. Replace the translucent right-hand
    /// scrim with an opaque app-surface continuation so enlarged filenames
    /// cannot protrude as clipped fragments at the drawer boundary. The
    /// drawer itself remains fully visible and the same rect remains the
    /// click-to-dismiss target owned by `NavShell`.
    fn draw_places_modal_backdrop(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        theme: &ui::Theme,
    ) {
        if self.nav.mode() != ui::widget::ShellMode::Narrow || !self.nav.is_sidebar_open() {
            return;
        }
        let drawer = self.nav.sidebar_rect();
        let content = self.nav.overview_rect();
        if drawer.x1 < content.x1 {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.bg_primary,
                None,
                &vello::kurbo::Rect::new(drawer.x1, content.y0, content.x1, content.y1),
            );
        }
    }

    /// A narrow places drawer is a modal flap: the file view and every one of
    /// its transient affordances live behind its scrim. The bubble is drawn by
    /// this app (rather than by `NavShell`), so make that ownership boundary
    /// explicit instead of accidentally painting it over the flap.
    fn status_bubble_is_occluded_by_places_drawer(&self) -> bool {
        self.nav.mode() == ShellMode::Narrow && self.nav.is_sidebar_open()
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
    /// All currently-selected displayed-entry indices.
    pub fn selected_indices(&self) -> Vec<usize> {
        self.selection.selected()
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn is_loading(&self) -> bool {
        self.is_loading
    }

    /// Deterministic offscreen-shot seam for the busy state. Production
    /// navigation owns this transition synchronously; exposing it only for the
    /// headless renderer lets visual CI cover the semantic/loading composition
    /// without adding a fake delayed filesystem implementation.
    #[doc(hidden)]
    pub fn debug_show_loading_state(&mut self) {
        self.entries.clear();
        self.error = None;
        self.is_loading = true;
        self.status = None;
    }

    /// Deterministic offscreen-shot seam for the actionable error state.
    #[doc(hidden)]
    pub fn debug_show_error_state(&mut self) {
        self.entries.clear();
        self.is_loading = false;
        self.error = Some(t_args(
            "files-directory-read-failed",
            &[arg("error", "Permission denied")],
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

    /// Navigate to `path`, pushing the current path onto the back-history.
    pub fn navigate(&mut self, path: PathBuf) {
        if path != self.current_path {
            self.history.push(self.current_path.clone());
            self.forward.clear();
        }
        self.load(path);
    }

    fn read_directory(&self, path: &Path) -> Result<Vec<Entry>, String> {
        if crate::recent::is_location(path) {
            return Ok(self.recent_entries());
        }
        if self.startup_accounts.is_some() {
            crate::model::read_dir_with_workspace_roots(path, &self.workspace_roots)
        } else {
            read_dir_sync(path)
        }
    }

    /// Re-read `path` into the view without touching history (shared by
    /// navigate/back/forward/refresh).
    fn load(&mut self, path: PathBuf) {
        self.is_loading = true;
        self.selection.clear();
        self.error = None;
        self.last_click = None;
        self.status = None;
        // A new directory starts at the top; the persisted scroll offset only
        // survives *structural* rebuilds within the same directory.
        self.scroll_offset.set((0.0, 0.0));
        self.invalidate_open_with();
        self.close_app_chooser();
        // Opening the Trash for the first time (nothing deleted yet) should show
        // an empty folder, not a "no such directory" error, so materialise it.
        if matches!(self.services, FileServices::Legacy) && fileops::is_trash_dir(&path) {
            let _ = fileops::ensure_trash_dir();
        }
        let recent = crate::recent::is_location(&path);
        let loaded = if let Some((archive_path, prefix)) = crate::archive::location(&path) {
            crate::archive::list_directory(&archive_path, &prefix)
                .map(|archive_entries| {
                    archive_entries
                        .into_iter()
                        .map(|entry| {
                            let name = entry
                                .path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned();
                            let synthetic_path = archive_path.join(&entry.path);
                            Entry {
                                name,
                                path: synthetic_path.clone(),
                                is_dir: entry.is_dir,
                                size: entry.size,
                                modified: None,
                                mime: if entry.is_dir {
                                    "inode/directory".into()
                                } else {
                                    mime_guess::from_path(&synthetic_path)
                                        .first_or_octet_stream()
                                        .to_string()
                                },
                                sync_state: None,
                                content_hash: None,
                                record_id: None,
                                availability: None,
                                share: None,
                                history: None,
                                rich_metadata: None,
                            }
                        })
                        .collect()
                })
                .map_err(|error| error.to_string())
        } else {
            self.read_directory(&path)
        };
        match loaded {
            Ok(mut entries) => {
                if !self.show_hidden {
                    entries.retain(|e| !e.name.starts_with('.'));
                }
                // Recent is ordered by when you last used each item.
                if !recent {
                    sort_entries(self.sort_by, self.sort_order, &mut entries);
                }
                self.entries = entries;
            }
            Err(e) => {
                // Drop stale entries so the error view isn't backed by the old
                // directory's contents.
                self.entries.clear();
                self.error = Some(t_args("files-directory-read-failed", &[arg("error", e)]));
            }
        }
        // Always record the path we attempted, even on error, so a Refresh
        // ("Try Again") retries the same target rather than reloading the
        // previous valid directory and silently clearing the error.
        self.current_path = path;
        if self.error.is_none() && crate::archive::location(&self.current_path).is_some() {
            self.status = Some(t("files-archive-read-only"));
        }
        self.is_loading = false;
        self.refresh_external_search_catalogs();
    }

    /// Go to the parent directory (history-aware).
    pub fn back(&mut self) {
        if self.is_recent_view() {
            return;
        }
        if let Some(parent) = self.current_path.parent() {
            self.navigate(parent.to_path_buf());
        }
    }

    /// Go up to the parent directory (alias of `back` kept explicit for
    /// the toolbar's distinct Up affordance).
    pub fn go_up(&mut self) {
        self.back();
    }

    /// Pop the back-history.
    pub fn history_back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.forward.push(self.current_path.clone());
            self.load(prev);
        }
    }

    /// Pop the forward-history.
    pub fn history_forward(&mut self) {
        if let Some(next) = self.forward.pop() {
            self.history.push(self.current_path.clone());
            self.load(next);
        }
    }

    pub fn refresh(&mut self) {
        self.load(self.current_path.clone());
    }

    /// Poll the visible directory without treating it as navigation. Stable
    /// record identity preserves selection across remote rename/move metadata;
    /// ordinary files fall back to their path. Scroll and transient operation
    /// status remain untouched.
    fn refresh_metadata_if_due(&mut self, now: Instant) -> bool {
        if now < self.next_metadata_refresh {
            return false;
        }
        // Set the next deadline before doing any I/O.  A transient read error
        // must not accidentally turn a very large directory into a busy loop.
        self.next_metadata_refresh = now + metadata_refresh::interval(self.entries.len());
        let catalog_changed = self
            .collaborative_record_pager
            .borrow_mut()
            .poll_generation();
        // `sync` admits the one bounded page for this turn. Keep polling and
        // admission separate so an authority change cannot consume two pages
        // in one UI frame.
        let identities_changed = self.refresh_trusted_identities(None);
        if self.renaming.is_some() || self.pin_drag.is_some() {
            return identities_changed || catalog_changed;
        }
        self.refresh_metadata_preserving_view() || identities_changed || catalog_changed
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
        true
    }

    /// Re-read the current directory while preserving stable-record
    /// selection, scroll, and operation status. Collaborative controls use
    /// this path because their xattr response changes row metadata but is not
    /// navigation.
    fn refresh_metadata_preserving_view(&mut self) -> bool {
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
        let Ok(mut entries) = self.read_directory(&self.current_path) else {
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
        self.structure_dirty = true;
        true
    }

    /// Single/double click handler retained for tests + the command path.
    /// A second click on the same row within the session's multi-click window
    /// returns its path.
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
        self.last_click = idx.map(|i| (i, now));
        None
    }

    pub fn set_sort_by(&mut self, s: SortBy) {
        let changed = self.sort_by != s;
        self.sort_by = s;
        sort_entries(self.sort_by, self.sort_order, &mut self.entries);
        if changed {
            self.share_sort();
        }
    }
    pub fn toggle_sort_order(&mut self) {
        self.sort_order = match self.sort_order {
            SortOrder::Ascending => SortOrder::Descending,
            SortOrder::Descending => SortOrder::Ascending,
        };
        sort_entries(self.sort_by, self.sort_order, &mut self.entries);
        self.share_sort();
    }
    pub fn set_view_mode(&mut self, m: ViewMode) {
        if self.view_mode == m {
            return;
        }
        self.view_mode = m;
        self.share_document_edit(crate::resource::FilesDocAction::SetView {
            mode: crate::doc::view_mode_token(m).to_owned(),
        });
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

    // --- Command application ------------------------------------------

    /// Apply one queued command, returning whether it changed the widget
    /// Apply one semantic Files command through the same path used by menus,
    /// keyboard actions and accessibility activation.
    ///
    /// This is public so shell hosts and full-stack mount tests can drive the
    /// application without synthesizing layout-dependent pointer coordinates.
    // Central semantic dispatcher: every menu, keyboard, accessibility, and
    // integration-test command must share the same state transitions.
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
                    | FileCommand::CompressSelection
                    | FileCommand::OpenTerminal
                    | FileCommand::ArmDrag { .. }
                    | FileCommand::BeginDrag { .. }
            )
        {
            self.status = Some(t("files-archive-read-only"));
            return true;
        }
        // Recent is not a folder: nothing can be created in, pasted into or
        // run from it. Items keep working (open, copy, rename, drag).
        if self.is_recent_view()
            && matches!(
                &cmd,
                FileCommand::NewFolder
                    | FileCommand::NewFromTemplate(_)
                    | FileCommand::Paste
                    | FileCommand::OpenTerminal
                    | FileCommand::ExtractHere
                    | FileCommand::CompressSelection
            )
        {
            self.status = Some(t("files-recent-not-a-folder"));
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
            FileCommand::MoveSelection { index, extend } => {
                if index < self.displayed_entries().len() {
                    self.selection.move_to(index, extend);
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
                // Pin policy is a collaborative-FS control, not a generic file
                // xattr.  A mixed selection may contain ordinary files, and an
                // inherited pin can only be changed on its parent.  Restrict
                // writes to records for which this exact action is meaningful.
                let displayed = self.displayed_entries();
                let paths: Vec<_> = self
                    .selection
                    .selected()
                    .into_iter()
                    .filter_map(|index| displayed.get(index))
                    .filter(|entry| {
                        entry.availability.as_ref().is_some_and(|availability| {
                            if keep_offline {
                                availability.policy != crate::model::AvailabilityPolicy::Pinned
                            } else {
                                availability.policy == crate::model::AvailabilityPolicy::Pinned
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
                    if let Err(error) = xattr::set(path, crate::model::PIN_XATTR, value) {
                        failures.push(format!("{}: {error}", path.display()));
                    }
                }
                if failures.is_empty() {
                    self.status = Some(if value == b"pinned" {
                        t_args(
                            "files-keeping-offline",
                            &[arg("count", fluent_count(paths.len()))],
                        )
                    } else {
                        t_args(
                            "files-using-automatic",
                            &[arg("count", fluent_count(paths.len()))],
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
                                availability.policy == crate::model::AvailabilityPolicy::Automatic
                                    && !availability.policy_inherited
                                    && availability.transfer.phase
                                        == crate::model::TransferPhase::Idle
                                    && matches!(
                                        availability.residency,
                                        crate::model::Residency::Partial { .. }
                                            | crate::model::Residency::Resident { .. }
                                    )
                            })
                    })
                    .map(|entry| entry.path.clone())
                    .collect();
                let mut failures = Vec::new();
                let mut safety_refusals = Vec::new();
                for path in &paths {
                    if let Err(error) =
                        xattr::set(path, crate::model::AVAILABILITY_CONTROL_XATTR, b"evict")
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
                        &[arg("count", fluent_count(paths.len()))],
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
                        history: None,
                        title: t("files-destructive-eviction-title"),
                        summary: t_args(
                            "files-destructive-eviction-summary",
                            &[arg("count", fluent_count(safety_refusals.len()))],
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
                        crate::model::AVAILABILITY_CONTROL_XATTR,
                        b"evict-destructive",
                    ) {
                        failures.push(format!("{}: {error}", path.display()));
                    }
                }
                self.status = Some(if failures.is_empty() {
                    t_args(
                        "files-destructive-eviction-started",
                        &[arg("count", fluent_count(paths.len()))],
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
                                crate::model::Residency::Resident { .. }
                            ) && availability.transfer.phase == crate::model::TransferPhase::Idle
                        })
                    })
                    .map(|entry| entry.path.clone())
                    .collect();
                let mut failures = Vec::new();
                for path in &paths {
                    if let Err(error) =
                        xattr::set(path, crate::model::AVAILABILITY_CONTROL_XATTR, b"retry")
                    {
                        failures.push(format!("{}: {error}", path.display()));
                    }
                }
                self.status = Some(if paths.is_empty() {
                    t("files-no-retryable-transfer")
                } else if failures.is_empty() {
                    t_args(
                        "files-retrying-transfers",
                        &[arg("count", fluent_count(paths.len()))],
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
                    if let Some(record_id) = entry.record_id {
                        lines.push(t_args(
                            "files-property-collaborative-record",
                            &[arg("record", uuid::Uuid::from_bytes(record_id).to_string())],
                        ));
                    }
                    if let Some(content_hash) = entry.content_hash {
                        let hash = hex_prefix(&content_hash, 8);
                        lines.push(t_args("files-property-content", &[arg("hash", hash)]));
                    }
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
                            crate::model::Residency::NotResident => {
                                t("files-residency-metadata-only")
                            }
                            crate::model::Residency::Partial { resident_bytes } => t_args(
                                "files-residency-partly-local",
                                &[arg(
                                    "bytes",
                                    i64::try_from(resident_bytes).unwrap_or(i64::MAX),
                                )],
                            ),
                            crate::model::Residency::Resident { bytes } => t_args(
                                "files-residency-local",
                                &[arg("bytes", i64::try_from(bytes).unwrap_or(i64::MAX))],
                            ),
                        };
                        lines.push(t_args(
                            "files-property-on-device",
                            &[arg("state", residency)],
                        ));
                    }
                    if let Some(share) = &entry.share {
                        lines.push(t_args(
                            "files-property-sharing",
                            &[arg("count", fluent_count(share.active_grants()))],
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
                                arg("conflicts", fluent_count(history.open_conflicts().count())),
                            ],
                        ));
                    }
                    self.show_collab_panel(CollabPanel {
                        history: None,
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
                        crate::model::Residency::NotResident => t("files-not-stored"),
                        crate::model::Residency::Partial { resident_bytes } => t_args(
                            "files-partly-stored",
                            &[arg(
                                "bytes",
                                i64::try_from(resident_bytes).unwrap_or(i64::MAX),
                            )],
                        ),
                        crate::model::Residency::Resident { bytes } => t_args(
                            "files-stored",
                            &[arg("bytes", i64::try_from(bytes).unwrap_or(i64::MAX))],
                        ),
                    };
                    let policy = match (availability.policy, availability.policy_inherited) {
                        (crate::model::AvailabilityPolicy::Pinned, true) => {
                            t("files-kept-offline-parent")
                        }
                        (crate::model::AvailabilityPolicy::Pinned, false) => {
                            t("files-kept-offline")
                        }
                        (crate::model::AvailabilityPolicy::Automatic, _) => {
                            t("files-automatic-storage")
                        }
                    };
                    let replicas = match availability.replicas {
                        crate::model::ReplicaEvidence::Unknown => t("files-replica-unverified"),
                        crate::model::ReplicaEvidence::Known { reachable, durable } => t_args(
                            "files-replica-counts",
                            &[
                                arg("reachable", i64::from(reachable)),
                                arg("durable", i64::from(durable)),
                            ],
                        ),
                    };
                    let transfer = availability.transfer.last_error.clone().unwrap_or_else(|| {
                        t_args(
                            "files-transfer-state",
                            &[arg(
                                "state",
                                transfer_phase_label(availability.transfer.phase),
                            )],
                        )
                    });
                    self.show_collab_panel(CollabPanel {
                        history: None,
                        title: t_args("files-availability-title", &[arg("name", entry.name)]),
                        summary: residency,
                        lines: vec![policy, replicas, transfer],
                        action: None,
                    });
                }
                true
            }
            FileCommand::CreateOneTimeShareLink => {
                if self.share_pending_path.is_some() {
                    self.status = Some(t("files-share-link-pending"));
                    return true;
                }
                let entry = self
                    .selection
                    .primary()
                    .and_then(|index| self.displayed_entries().get(index).cloned());
                let Some(entry) = entry else {
                    self.status = Some(t("files-select-collaborative-file"));
                    return true;
                };
                let (Some(record_id), Some(socket)) = (
                    entry.record_id,
                    crate::model::share_control_socket(&entry.path, &self.workspace_roots),
                ) else {
                    self.status = Some(t("files-sharing-unavailable"));
                    return true;
                };
                self.share_pending_path = Some(entry.path);
                self.status = Some(t("files-creating-one-use-link"));
                self.spawn_share(async move {
                    crate::sharing::create_one_use_view_link(
                        &socket,
                        collab_types::FileRecordId(uuid::Uuid::from_bytes(record_id)),
                    )
                    .await
                    .map(crate::sharing::ShareActionResult::LinkCreated)
                });
                true
            }
            FileCommand::ShareWithPerson { person } => {
                if self.share_pending_path.is_some() {
                    self.status = Some(t("files-share-change-pending"));
                    return true;
                }
                let entry = self
                    .selection
                    .primary()
                    .and_then(|index| self.displayed_entries().get(index).cloned());
                let Some(entry) = entry else {
                    self.status = Some(t("files-select-collaborative-file"));
                    return true;
                };
                let (Some(record_id), Some(socket)) = (
                    entry.record_id,
                    crate::model::share_control_socket(&entry.path, &self.workspace_roots),
                ) else {
                    self.status = Some(t("files-sharing-unavailable"));
                    return true;
                };
                let display = self.trusted_humans.display_name_or_fingerprint(&person);
                let recipient_devices: Vec<collab_types::Ed25519PublicKey> = self
                    .trusted_humans
                    .human(&person)
                    .map(|human| human.devices.iter().map(|device| device.device).collect())
                    .unwrap_or_default();
                if recipient_devices.is_empty() {
                    self.status = Some(t("files-share-recipient-unreachable"));
                    return true;
                }
                self.share_pending_path = Some(entry.path);
                self.status = Some(t_args(
                    "files-share-with-progress",
                    &[arg("person", display)],
                ));
                self.spawn_share(async move {
                    crate::sharing::create_named_share(
                        &socket,
                        collab_types::FileRecordId(uuid::Uuid::from_bytes(record_id)),
                        person,
                        recipient_devices,
                        collab_types::FilePermission::View,
                        None,
                    )
                    .await
                    .map(crate::sharing::ShareActionResult::NamedCreated)
                });
                true
            }
            FileCommand::CreateShare(request) => {
                if self.share_pending_path.is_some() {
                    self.status = Some(t("files-share-change-pending"));
                    return true;
                }
                let entry = self
                    .selection
                    .primary()
                    .and_then(|index| self.displayed_entries().get(index).cloned());
                let Some(entry) = entry else {
                    self.status = Some(t("files-select-collaborative-file"));
                    return true;
                };
                let (Some(record_id), Some(socket)) = (
                    entry.record_id,
                    crate::model::share_control_socket(&entry.path, &self.workspace_roots),
                ) else {
                    self.status = Some(t("files-sharing-unavailable"));
                    return true;
                };
                self.share_pending_path = Some(entry.path);
                self.status = Some(t("files-share-creating"));
                self.spawn_share(async move {
                    crate::sharing::create_share(
                        &socket,
                        collab_types::FileRecordId(uuid::Uuid::from_bytes(record_id)),
                        request,
                    )
                    .await
                });
                true
            }
            FileCommand::RevokeShare { grant_id } => {
                if self.share_pending_path.is_some() {
                    self.status = Some(t("files-share-change-pending"));
                    return true;
                }
                let entry = self
                    .selection
                    .primary()
                    .and_then(|index| self.displayed_entries().get(index).cloned());
                let Some(entry) = entry else {
                    self.status = Some(t("files-select-shared-file"));
                    return true;
                };
                let Some(socket) =
                    crate::model::share_control_socket(&entry.path, &self.workspace_roots)
                else {
                    self.status = Some(t("files-sharing-unavailable"));
                    return true;
                };
                self.share_pending_path = Some(entry.path);
                self.status = Some(t("files-revoking-access"));
                self.spawn_share(
                    async move { crate::sharing::revoke_grant(&socket, grant_id).await },
                );
                true
            }
            FileCommand::ShowSharing => {
                let entry = self
                    .selection
                    .primary()
                    .and_then(|index| self.displayed_entries().get(index).cloned());
                if let Some(entry) = entry
                    && let Some(share) = entry.share
                {
                    let active = share.active_grants();
                    let lines = if share.grants.is_empty() {
                        vec![t("files-not-shared")]
                    } else {
                        let mut lines = Vec::new();
                        for grant in &share.grants {
                            let (subject, devices) = match grant.subject {
                                crate::model::ShareSubject::Person { person } => {
                                    let person = collab_types::Ed25519PublicKey::from_bytes(person);
                                    self.trusted_humans.human(&person).map_or_else(
                                        || {
                                            (
                                                crate::identity_directory::fingerprint(
                                                    person.as_bytes(),
                                                ),
                                                Vec::new(),
                                            )
                                        },
                                        |human| {
                                            let subject = if human.devices.is_empty() {
                                                human.display_name.clone()
                                            } else {
                                                t_args(
                                                    "files-certified-devices",
                                                    &[
                                                        arg("person", human.display_name.clone()),
                                                        arg(
                                                            "devices",
                                                            human
                                                                .devices
                                                                .iter()
                                                                .map(|device| device.label.as_str())
                                                                .collect::<Vec<_>>()
                                                                .join(", "),
                                                        ),
                                                    ],
                                                )
                                            };
                                            let devices = human
                                                .devices
                                                .iter()
                                                .map(certified_device_detail)
                                                .collect();
                                            (subject, devices)
                                        },
                                    )
                                }
                                crate::model::ShareSubject::BearerLink => {
                                    (t("files-anyone-link"), Vec::new())
                                }
                            };
                            let permission = match grant.permission {
                                crate::model::FilePermission::View => t("files-can-view"),
                                crate::model::FilePermission::Edit => t("files-can-edit"),
                            };
                            let status = grant_status_label(grant.status);
                            let expiry = grant.expires_at_ms.map_or_else(
                                || t("files-no-expiry"),
                                |deadline| {
                                    t_args(
                                        "files-expires",
                                        &[arg("time", format_timestamp(deadline))],
                                    )
                                },
                            );
                            lines.push(t_args(
                                "files-share-grant-line",
                                &[
                                    arg("subject", subject),
                                    arg("permission", permission),
                                    arg("status", status),
                                    arg("expiry", expiry),
                                ],
                            ));
                            lines.extend(devices);
                        }
                        lines
                    };
                    self.show_collab_panel(CollabPanel {
                        history: None,
                        title: t_args("files-sharing-title", &[arg("name", entry.name)]),
                        summary: t_args(
                            "files-sharing-summary",
                            &[arg("count", fluent_count(active))],
                        ),
                        lines,
                        action: None,
                    });
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
                    if history.record_id != record_id {
                        return None;
                    }
                    history
                        .versions
                        .iter()
                        .find(|version| version.id == version_id.0.to_string())
                        .cloned()
                        .map(|version| (version, entry.path.clone()))
                });
                let entry = selected.map(|(version, path)| {
                    let person = collab_types::Ed25519PublicKey::from_bytes(version.actor.person);
                    let person_label = self.trusted_humans.display_name_or_fingerprint(&person);
                    let identity = short_fingerprint(&version.actor.person);
                    let action = t(version.provenance_key());
                    let resource_label = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("File")
                        .to_string();
                    let context = ui::tilo_actions::ActionContext::for_resource(
                        action.clone(),
                        record_id.clone(),
                        "files.entry",
                        resource_label.clone(),
                    )
                    .with_app("so.tilo.file-explorer", "Files")
                    .with_facet(ui::tilo_actions::ActionFacet::new(
                        "version",
                        version.id.clone(),
                    ));
                    ui::widget::HistoryEntry {
                        id: ui::widget::HistoryEntryId(*version_id.0.as_bytes()),
                        author: ui::widget::HistoryAuthor::new(
                            person_label,
                            ui::history_source::stable_hue(&identity),
                            identity,
                        ),
                        // This is a signed immutable checkpoint, not a shell
                        // CRDT operation the generic Revert affordance can undo.
                        op_class: collab_types::OpClass::Compaction,
                        artifact_kind: collab_types::ArtifactKind::File,
                        timestamp: UNIX_EPOCH
                            .checked_add(Duration::from_millis(version.timestamp_ms))
                            .unwrap_or(UNIX_EPOCH),
                        summary: format!("{action} · {resource_label}"),
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
                // This legacy menu intent now selects the immutable version in
                // shared History. Only its reviewed confirmation can write.
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
                // The review remains non-committing while the daemon reads the
                // immutable members. It repeats this preflight immediately
                // before publishing, so reopening an old menu can never commit
                // a merge whose members or base have changed.
                self.show_collab_panel(CollabPanel {
                    history: None,
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
                    collab_types::ConflictStrategy::KeepCurrent => t("files-keeping-current"),
                    collab_types::ConflictStrategy::UseOther => t("files-applying-other"),
                    collab_types::ConflictStrategy::KeepBoth => {
                        conflict_strategy_label(collab_types::ConflictStrategy::KeepBoth)
                    }
                    collab_types::ConflictStrategy::Merge => t("files-merging-versions"),
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
                true
            }
            FileCommand::OpenTerminal => {
                let _ = self.open_terminal(&mut SystemRunner);
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
            FileCommand::UndoLastTrash => self.apply_undo_last_trash(),
            FileCommand::RestoreFromTrash => self.apply_restore_from_trash(),
            FileCommand::EmptyTrash => self.request_empty_trash_confirmation(),
            FileCommand::Copy => self.apply_copy(),
            FileCommand::Cut => self.apply_cut(),
            FileCommand::Paste => {
                self.paste();
                true
            }
            FileCommand::ExtractHere => self.apply_extract_here(),
            FileCommand::ExtractCurrentArchive => self.apply_extract_current_archive(),
            FileCommand::CompressSelection => self.apply_compress_selection(),
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
            FileCommand::OpenWith {
                path,
                app_id,
                always,
            } => self.apply_open_with(&path, &app_id, always),
            FileCommand::OpenWithOther => {
                self.open_app_chooser();
                true
            }
            FileCommand::RevealInFolder(path) => self.apply_reveal_in_folder(&path),
            FileCommand::ForgetRecent(path) => self.apply_forget_recent(&path),
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
                self.share_document_edit(crate::resource::FilesDocAction::SetHidden {
                    shown: self.show_hidden,
                });
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
            FileCommand::OpenFavoriteMenu(path) => self.apply_open_favorite_menu(path),
        }
    }

    /// `FileCommand::Click` — drives selection and, via the app's
    /// double-click detection, opening.
    fn apply_click(&mut self, index: usize, ctrl: bool, shift: bool) -> bool {
        if ctrl || shift {
            self.selection.apply_click(index, ctrl, shift);
            self.last_click = None;
        } else {
            // Capture a multi-selection the press lands inside *before*
            // the click collapses it, so a drag that starts here carries
            // the whole set (drag a multi-selection). A press outside the
            // multi-selection seeds nothing.
            let sel = self.selection.selected();
            self.drag_seed = if sel.len() > 1 && sel.contains(&index) {
                self.selected_paths()
            } else {
                Vec::new()
            };
            if let Some(path) = self.select(Some(index)) {
                // Double-click → open / enter.
                self.open_path(path);
            }
        }
        true
    }

    /// `FileCommand::ContextSelect` — selects the right-press target so the
    /// context menu that follows acts on what was clicked.
    fn apply_context_select(&mut self, target: Option<usize>) -> bool {
        match target {
            // A right-press on a row that isn't already part of the
            // selection selects just it; one inside an existing
            // multi-selection leaves that selection intact so the menu
            // action applies to every highlighted item.
            Some(i) if !self.selection.is_selected(i) => self.selection.select_only(i),
            Some(_) => {}
            // Right-press on empty space clears the selection.
            None => self.selection.clear(),
        }
        self.last_click = None;
        true
    }

    /// `FileCommand::NewFolder` — create, refresh, then drop into
    /// inline-rename so the user names it right away.
    fn apply_new_folder(&mut self) -> bool {
        match fileops::new_folder(&self.current_path, &t("files-new-folder-default-name")) {
            Ok(path) => {
                self.refresh();
                if let Some(i) = self.displayed_entries().iter().position(|e| e.path == path) {
                    self.selection.select_only(i);
                    self.begin_rename();
                }
                self.status = Some(t("files-created-folder"));
            }
            Err(e) => self.status = Some(t_args("files-new-folder-failed", &[arg("error", e)])),
        }
        true
    }

    /// `FileCommand::Delete` — trash the selection, or (already in the
    /// Trash) first ask before permanently purging it.
    fn apply_delete(&mut self) -> bool {
        let paths = self.selected_paths();
        if !paths.is_empty() {
            if self.is_trash_view() {
                // Already in the Trash — a second delete is irreversible.
                // Hold the exact paths in the modal action so a changed
                // selection cannot silently change what gets destroyed.
                self.show_collab_panel(CollabPanel {
                    history: None,
                    title: t_args(
                        "files-permanent-delete-title",
                        &[arg("count", fluent_count(paths.len()))],
                    ),
                    summary: t_args(
                        "files-permanent-delete-summary",
                        &[arg("count", fluent_count(paths.len()))],
                    ),
                    lines: vec![t("files-permanent-delete-loss")],
                    action: Some(CollabPanelAction::PermanentDelete(paths)),
                });
            } else {
                // Trash implementations may choose a mount-local store or
                // rename on collision. Snapshot the concrete entries before
                // and after so Undo is offered only for an exact set Files can
                // still revalidate; never guess a path from the source name.
                let before = fileops::trash_entries_snapshot();
                match fileops::delete_to_trash(&paths) {
                    Ok(()) => {
                        self.last_trash_undo = before.and_then(|before| {
                            let after = fileops::trash_entries_snapshot()?;
                            let created: Vec<_> = after
                                .into_iter()
                                .filter(|entry| !before.contains(entry))
                                .collect();
                            (created.len() == paths.len()).then_some(created)
                        });
                        self.refresh();
                        self.status = Some(t_args(
                            "files-moved-to-trash",
                            &[arg("count", fluent_count(paths.len()))],
                        ));
                    }
                    Err(e) => {
                        self.status = Some(t_args("files-delete-failed", &[arg("error", e)]));
                    }
                }
            }
        }
        true
    }

    /// Restore the last precisely observed local Trash operation. Recheck that
    /// every captured entry still exists before asking the platform to restore:
    /// another process may have emptied Trash or changed an entry since Files
    /// offered the affordance.
    fn apply_undo_last_trash(&mut self) -> bool {
        let Some(entries) = self.last_trash_undo.clone() else {
            self.status = Some(t("files-undo-trash-unavailable"));
            return true;
        };
        let still_present = fileops::trash_entries_snapshot()
            .is_some_and(|current| entries.iter().all(|entry| current.contains(entry)));
        if !still_present {
            self.last_trash_undo = None;
            self.status = Some(t("files-undo-trash-unavailable"));
            return true;
        }
        match fileops::restore_entries(&entries) {
            Ok(restored) if restored == entries.len() => {
                self.last_trash_undo = None;
                self.refresh();
                self.status = Some(t_args(
                    "files-undo-trash-restored",
                    &[arg("count", fluent_count(restored))],
                ));
            }
            Ok(_) => {
                self.last_trash_undo = None;
                self.status = Some(t("files-undo-trash-unavailable"));
            }
            Err(error) => {
                self.status = Some(t_args("files-undo-trash-failed", &[arg("error", error)]));
            }
        }
        true
    }

    /// Carry out a previously confirmed permanent deletion from Trash.
    fn apply_permanent_delete(&mut self, paths: &[PathBuf]) -> bool {
        self.last_trash_undo = None;
        match fileops::purge_entries(paths) {
            Ok(n) => {
                self.refresh();
                self.status = Some(t_args(
                    "files-permanently-deleted",
                    &[arg("count", fluent_count(n))],
                ));
            }
            Err(e) => {
                self.status = Some(t_args("files-delete-failed", &[arg("error", e)]));
            }
        }
        true
    }

    /// `FileCommand::RestoreFromTrash` — restore the selection to its
    /// original location.
    fn apply_restore_from_trash(&mut self) -> bool {
        self.last_trash_undo = None;
        let paths = self.selected_paths();
        if !paths.is_empty() {
            match fileops::restore_entries(&paths) {
                Ok(n) => {
                    self.refresh();
                    self.status = Some(t_args("files-restored", &[arg("count", fluent_count(n))]));
                }
                Err(e) => {
                    self.status = Some(t_args("files-restore-failed", &[arg("error", e)]));
                }
            }
        }
        true
    }

    /// `FileCommand::EmptyTrash` — ask before permanently emptying a
    /// non-empty Trash. An already-empty Trash is safe feedback, not a
    /// confirmation flow.
    fn request_empty_trash_confirmation(&mut self) -> bool {
        if self.entries.is_empty() {
            self.status = Some(t("files-trash-already-empty"));
            return true;
        }
        self.show_collab_panel(CollabPanel {
            history: None,
            title: t("files-empty-trash-title"),
            summary: t_args(
                "files-empty-trash-summary",
                &[arg("count", fluent_count(self.entries.len()))],
            ),
            lines: vec![t("files-permanent-delete-loss")],
            action: Some(CollabPanelAction::EmptyTrash),
        });
        true
    }

    /// Carry out a previously confirmed request to empty the Trash.
    fn perform_empty_trash(&mut self) -> bool {
        self.last_trash_undo = None;
        match fileops::empty_trash() {
            Ok(n) => {
                self.refresh();
                self.status = Some(if n == 0 {
                    t("files-trash-already-empty")
                } else {
                    t_args("files-emptied-trash", &[arg("count", fluent_count(n))])
                });
            }
            Err(e) => {
                self.status = Some(t_args("files-empty-trash-failed", &[arg("error", e)]));
            }
        }
        true
    }

    /// `FileCommand::NewFromTemplate` — copy `template`, refresh, then drop
    /// into inline-rename.
    fn apply_new_from_template(&mut self, template: &Path) -> bool {
        match templates::create_from_template(template, &self.current_path) {
            Ok(path) => {
                self.note_recent(&path, crate::recent::RecentUse::Changed);
                self.refresh();
                if let Some(i) = self.displayed_entries().iter().position(|e| e.path == path) {
                    self.selection.select_only(i);
                    self.begin_rename();
                }
                self.status = Some(t("files-created-file"));
            }
            Err(e) => self.status = Some(t_args("files-new-file-failed", &[arg("error", e)])),
        }
        true
    }

    /// `FileCommand::BeginDrag` — start carrying the row(s) as a floating
    /// ghost, and hand the gesture to the host or the toolkit's own
    /// client-side drag.
    fn apply_begin_drag(&mut self, path: PathBuf, is_dir: bool) -> bool {
        if self.pin_drag.is_some() {
            // Already carrying — the view and the app both arm the
            // same press, and whichever fires second is redundant.
            return true;
        }
        self.armed_drag = None;
        tracing::debug!(target: "file_explorer::drag", ?path, "drag begins");
        // Start carrying the row(s); the ghost + the drop are owned by
        // `handle_event`. Drag the whole multi-selection when the press
        // landed inside it (captured in `drag_seed` before the click
        // collapsed it, Finder/Nautilus behaviour), else just this row.
        let seed = std::mem::take(&mut self.drag_seed);
        self.drag_paths = if seed.contains(&path) {
            seed
        } else {
            vec![path.clone()]
        };
        self.drag_origin_is_dir = is_dir;
        self.pin_drag = Some(path);
        self.drag_hover = None;
        // Tell the host what is being carried: if the pointer
        // reaches another application's window, the host turns
        // this into a real drag-and-drop for that client.
        //
        // That is the path when Files is a widget inside the
        // compositor. When it is a packaged app — its own
        // process, its own sandbox, a Wayland client like any
        // other — there is no host holding the gesture, so it
        // opens the drag itself. Only one of the two can be
        // true in a given process, and each reports whether it
        // applied, so both are offered the gesture and at most
        // one takes it.
        crate::external_drag::begin(self.drag_paths.clone());
        if ui::dnd::start_file_drag(&self.drag_paths) {
            tracing::debug!(
                target: "file_explorer::drag",
                paths = ?self.drag_paths,
                "carrying the drag out of this app as a client drag"
            );
            // The gesture now belongs to the compositor's
            // drag grab, and this window will not see the rest
            // of it: no more motion, and no release. Anything
            // kept here would be state waiting for an end that
            // never arrives — a ghost pinned to the last place
            // the pointer was, and a drop this app would try to
            // resolve against its own folders. So it is let go
            // of here, and the picture from this point on is
            // the compositor's to draw.
            self.pin_drag = None;
            self.drag_paths.clear();
            self.drag_hover = None;
            self.armed_drag = None;
            crate::external_drag::end();
        }
        true
    }

    /// `FileCommand::AddFavorite` — pin `path` to the places list.
    fn apply_add_favorite(&mut self, path: PathBuf) -> bool {
        let added = {
            let home = self.home_path.clone().unwrap_or_else(|| PathBuf::from("/"));
            let mut favs = self.favs.borrow_mut();
            favorites::add(&mut favs.favorites, path, &home, &self.user_dirs)
        };
        if added {
            self.save_favorites();
            self.refresh_places_search();
            self.status = Some(t("files-added-favourite"));
            self.share_bookmarks();
        }
        true
    }

    /// `FileCommand::RemoveFavorite` — unpin `path` from the places list.
    fn apply_remove_favorite(&mut self, path: &Path) -> bool {
        let removed = {
            let mut favs = self.favs.borrow_mut();
            favorites::remove(&mut favs.favorites, path)
        };
        if removed {
            self.save_favorites();
            self.refresh_places_search();
            self.status = Some(t("files-removed-favourite"));
            self.share_bookmarks();
        }
        true
    }

    /// `FileCommand::ReorderFavorite` — move favourite `from` to index `to`
    /// (drag-to-reorder drop).
    fn apply_reorder_favorite(&mut self, from: usize, to: usize) -> bool {
        let before = self.bookmark_paths();
        {
            let mut favs = self.favs.borrow_mut();
            favorites::reorder(&mut favs.favorites, from, to);
        }
        self.save_favorites();
        self.refresh_places_search();
        let after = self.bookmark_paths();
        if before != after {
            self.share_document_edit(crate::resource::FilesDocAction::SetBookmarks {
                paths: after,
            });
        }
        true
    }

    /// `FileCommand::Activate` — activate entry `index` directly (keyboard
    /// Enter / grid double-click).
    fn apply_activate(&mut self, index: usize) -> bool {
        let disp = self.displayed_entries();
        if let Some(e) = disp.get(index) {
            self.open_path(e.path.clone());
        }
        true
    }

    /// `FileCommand::SortColumn` — a sortable list header at `col` was
    /// clicked; toggles order on a repeat click of the active column.
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

    /// `FileCommand::Copy` — copy the selection into the clipboard.
    fn apply_copy(&mut self) -> bool {
        self.clipboard = self.selected_paths();
        self.clipboard_cut = false;
        self.status = Some(t_args(
            "files-copied",
            &[arg("count", fluent_count(self.clipboard.len()))],
        ));
        true
    }

    /// `FileCommand::Cut` — cut the selection into the clipboard.
    fn apply_cut(&mut self) -> bool {
        self.clipboard = self.selected_paths();
        self.clipboard_cut = true;
        self.status = Some(t_args(
            "files-cut",
            &[arg("count", fluent_count(self.clipboard.len()))],
        ));
        true
    }

    fn apply_extract_here(&mut self) -> bool {
        let paths: Vec<_> = self
            .selected_paths()
            .into_iter()
            .filter(|path| crate::archive::is_supported(path))
            .collect();
        if paths.is_empty() {
            self.status = Some(t("files-archive-select"));
            return true;
        }
        let mut count = 0usize;
        let mut failures = Vec::new();
        for path in paths {
            let stem = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Archive")
                .trim_end_matches(".gz")
                .trim_end_matches(".tar")
                .trim_end_matches(".zip");
            let parent = path.parent().unwrap_or(&self.current_path);
            let mut destination = parent.join(stem);
            let mut suffix = 2u32;
            while destination.exists() {
                destination = parent.join(format!("{stem} {suffix}"));
                suffix = suffix.saturating_add(1);
            }
            match crate::archive::extract(&path, &destination) {
                Ok(entries) => count = count.saturating_add(entries.len()),
                Err(error) => failures.push(format!("{}: {error}", path.display())),
            }
        }
        self.refresh();
        self.status = Some(if failures.is_empty() {
            t_args(
                "files-archive-extracted",
                &[arg("count", fluent_count(count))],
            )
        } else {
            t_args(
                "files-archive-extracted-partial",
                &[
                    arg("count", fluent_count(count)),
                    arg("failures", failures.join("; ")),
                ],
            )
        });
        true
    }

    fn apply_extract_current_archive(&mut self) -> bool {
        let Some((archive, _)) = crate::archive::location(&self.current_path) else {
            return false;
        };
        self.selection.clear();
        if let Some(index) = self.entries.iter().position(|entry| entry.path == archive) {
            self.selection.select_only(index);
            return self.apply_extract_here();
        }
        let parent = archive.parent().unwrap_or(&self.current_path).to_path_buf();
        let stem = archive
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Archive")
            .trim_end_matches(".gz")
            .trim_end_matches(".tar")
            .trim_end_matches(".zip");
        let mut destination = parent.join(stem);
        let mut suffix = 2u32;
        while destination.exists() {
            destination = parent.join(format!("{stem} {suffix}"));
            suffix = suffix.saturating_add(1);
        }
        self.status = Some(match crate::archive::extract(&archive, &destination) {
            Ok(entries) => t_args(
                "files-archive-extracted",
                &[arg("count", fluent_count(entries.len()))],
            ),
            Err(error) => t_args(
                "files-archive-extract-failed",
                &[arg("error", error.to_string())],
            ),
        });
        true
    }

    fn apply_compress_selection(&mut self) -> bool {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.status = Some(t("files-archive-select-compress"));
            return true;
        }
        let base = if paths.len() == 1 {
            paths
                .first()
                .and_then(|path| path.file_stem())
                .and_then(|value| value.to_str())
                .unwrap_or("Archive")
        } else {
            "Archive"
        };
        let mut output = self.current_path.join(format!("{base}.zip"));
        let mut suffix = 2u32;
        while output.exists() {
            output = self.current_path.join(format!("{base} {suffix}.zip"));
            suffix = suffix.saturating_add(1);
        }
        self.status = Some(match crate::archive::create_zip(&paths, &output) {
            Ok(()) => {
                self.refresh();
                t_args(
                    "files-archive-created",
                    &[arg(
                        "name",
                        output.file_name().unwrap_or_default().to_string_lossy(),
                    )],
                )
            }
            Err(error) => t_args(
                "files-archive-create-failed",
                &[arg("error", error.to_string())],
            ),
        });
        true
    }

    /// `FileCommand::OpenTemplatesMenu` — opened from the menu-routing path
    /// with real geometry; if it arrives via the command queue just open it
    /// at the cursor.
    fn apply_open_templates_menu(&mut self) -> bool {
        self.open_templates_submenu(self.last_cursor);
        true
    }

    /// `FileCommand::ArmDrag` — a plain press landed on a row: arm a drag of
    /// `path` from `at`, started once the held pointer travels far enough.
    fn apply_arm_drag(&mut self, path: PathBuf, is_dir: bool, at: Point) -> bool {
        tracing::debug!(target: "file_explorer::drag", ?path, ?at, "drag armed");
        self.armed_drag = Some((at, path, is_dir));
        true
    }

    /// `FileCommand::OpenPath` — a typed/accepted path from the editable
    /// path bar: open a file, navigate into a directory (or a
    /// not-yet-existing path, so the error affordance surfaces rather than
    /// silently doing nothing).
    fn apply_open_path(&mut self, p: PathBuf) -> bool {
        if p.is_file() {
            self.open_path(p);
        } else {
            self.navigate(p);
        }
        true
    }

    /// `FileCommand::SetSortOrder` — set the sort direction explicitly
    /// (from the grid "Sort by" menu).
    fn apply_set_sort_order(&mut self, order: SortOrder) -> bool {
        if self.sort_order != order {
            self.toggle_sort_order();
        }
        true
    }

    /// `FileCommand::OpenFavoriteMenu` — remember the target; the
    /// right-press handler opens the menu at the cursor once this event's
    /// commands are drained.
    fn apply_open_favorite_menu(&mut self, path: PathBuf) -> bool {
        self.pending_fav_menu = Some(path);
        false
    }

    /// Set the zoom level (clamped) and persist it.
    fn set_zoom(&mut self, level: i32) {
        let level = config::clamp_zoom(level);
        if level != self.zoom_level {
            self.zoom_level = level;
            if !self.adopting_shared
                && let Some(dir) = self.config_dir.as_deref()
            {
                config::save_zoom(dir, level);
            }
            self.share_document_edit(crate::resource::FilesDocAction::SetZoom { level });
        }
    }

    /// The current zoom level (for tests).
    pub fn zoom_level(&self) -> i32 {
        self.zoom_level
    }

    /// Whether hidden files are shown (for tests).
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Borrow the favourites list (for tests).
    pub fn favorites(&self) -> Vec<crate::favorites::Favorite> {
        self.favs.borrow().favorites.clone()
    }

    /// Shared favourites store (tests / the headless shots demo).
    #[doc(hidden)]
    pub fn favorites_store(&self) -> FavStore {
        self.favs.clone()
    }

    /// Open the right-click context menu at `at` (headless shots demo).
    #[doc(hidden)]
    pub fn debug_open_context_menu(&mut self, at: Point) {
        self.open_context_menu(at);
    }

    /// Open the shared toolbar overflow menu (headless shots demo).
    #[doc(hidden)]
    pub fn debug_open_view_menu(&mut self) {
        self.open_view_menu();
    }

    /// Seed the local-only Trash Undo affordance for an offscreen review. The
    /// sentinel is deliberately not a real path, so headless rendering never
    /// mutates a user's Trash.
    #[doc(hidden)]
    pub fn debug_arm_trash_undo(&mut self) {
        self.last_trash_undo = Some(vec![PathBuf::from("/shot-only-trash-entry")]);
    }

    /// Open the context menu *and* its "New File" flyout (headless shots demo).
    #[doc(hidden)]
    pub fn debug_show_templates_menu(&mut self, at: Point) {
        // `--scenario context-menu` uses this exact pointer location. Keeping
        // it in the widget makes the screenshot's menu position truthful to a
        // real right-click rather than visually competing with the toolbar ⋯.
        self.last_cursor = at;
        self.open_context_menu(at);
        let anchor = Point::new(self.context_menu.get_rect().x1 - 6.0, at.y + 66.0);
        self.open_templates_submenu(anchor);
    }

    /// Open the toolbar's actual ⋯ menu for an offscreen visual review. This
    /// is intentionally separate from the pointer-context fixture above: the
    /// two surfaces have different anchors and should never be conflated.
    #[doc(hidden)]
    pub fn debug_show_view_menu(&mut self) {
        self.open_view_menu();
    }

    /// Begin an active favourite drag from `from` toward `to`, for a mid-drag
    /// screenshot. Requires the sidebar to have been laid out first (so the
    /// favourites band geometry is published).
    #[doc(hidden)]
    pub fn debug_begin_favorite_drag(&mut self, from: usize, to: usize) -> bool {
        crate::places_sidebar::debug_begin_drag(&self.favs, from, to)
    }

    /// Open the places drawer over the file view (the narrow layout's
    /// hamburger action), settled deterministically for a headless
    /// screenshot. In the wide layout the places list is already docked.
    #[doc(hidden)]
    pub fn debug_open_places_drawer(&mut self) {
        self.nav.open_sidebar();
        self.nav.snap();
    }

    /// Whether the places drawer / docked sidebar is currently revealed.
    #[doc(hidden)]
    pub fn debug_places_open(&self) -> bool {
        self.nav.is_sidebar_open()
    }

    /// The file-view pane's resolved rect — what a test asserts against to
    /// prove the file list is actually on screen.
    #[doc(hidden)]
    pub fn debug_content_rect(&self) -> vello::kurbo::Rect {
        self.nav.overview_rect()
    }

    /// The docked sidebar's resolved rect — the drag-to-pin drop target.
    #[doc(hidden)]
    pub fn debug_sidebar_rect(&self) -> vello::kurbo::Rect {
        self.nav.sidebar_rect()
    }

    /// Whether a drag-to-pin is currently in flight (a folder is being carried
    /// toward the sidebar).
    #[doc(hidden)]
    pub fn debug_pin_dragging(&self) -> bool {
        self.pin_drag.is_some()
    }

    /// Start a drag-to-pin ghost carrying `path`, with the cursor at `at` — for
    /// a mid-drag screenshot.
    #[doc(hidden)]
    pub fn debug_begin_pin_drag(&mut self, path: PathBuf, at: Point) {
        self.pin_drag = Some(path.clone());
        self.drag_paths = vec![path];
        self.drag_origin_is_dir = true;
        self.last_cursor = at;
        crate::external_drag::begin(self.drag_paths.clone());
    }

    /// Start a drag carrying `srcs` hovering the folder `over` — for a shot of a
    /// drag-to-move in progress (ghost + drop-target highlight). Requires the
    /// file view to have published its folder rows (a layout must have run);
    /// returns `false` if `over` isn't a laid-out folder row.
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
        // Cursor inside the target row so the ghost sits over it.
        self.last_cursor = Point::new(rect.x0 + 48.0, f64::midpoint(rect.y0, rect.y1));
        true
    }

    /// The folder rows the file view currently publishes as drop targets — for
    /// tests / the shots demo to pick a real target.
    #[doc(hidden)]
    pub fn debug_folder_targets(&self) -> Vec<(PathBuf, vello::kurbo::Rect)> {
        self.drop_targets
            .borrow()
            .iter()
            .map(|t| (t.path.clone(), t.rect))
            .collect()
    }

    /// The Up nav button's rect — the drop target that moves carried items into
    /// the parent directory. `None` at the root (no Up button).
    #[doc(hidden)]
    pub fn debug_up_button_rect(&self) -> Option<vello::kurbo::Rect> {
        self.toolbar.up_button_rect()
    }

    /// The responsive body's current mode (docked-wide vs push-narrow) — for
    /// tests asserting the sidebar reflows with the window width.
    #[doc(hidden)]
    pub fn debug_body_mode(&self) -> ShellMode {
        self.nav.mode()
    }

    /// Select the given displayed-entry indices (headless shots demo).
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
        self.structure_dirty = true;
    }

    /// Ask the owning framework host to open the selected real file history.
    /// A standalone widget without a managed host cannot render History.
    #[doc(hidden)]
    pub fn debug_open_history_demo(&mut self) {
        self.refresh_framework_history_target();
        self.framework_history.open(None);
    }

    fn refresh_framework_history_target(&mut self) {
        let entry = self
            .selection
            .primary()
            .and_then(|index| self.displayed_entries().get(index).cloned());
        let selected = entry.and_then(|entry| {
            let id = entry
                .history
                .as_ref()
                .and_then(|h| uuid::Uuid::parse_str(&h.record_id).ok())?;
            Some((entry.path, collab_types::FileRecordId(id), entry.name))
        });
        let identity = selected
            .as_ref()
            .map(|(path, record, _)| (path.clone(), *record));
        if identity == self.framework_history_binding {
            return;
        }
        let target = selected.and_then(|(path, record, label)| {
            crate::framework_history::target(&path, record, label).ok()
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

    // --- drag & drop --------------------------------------------------

    /// The folder a drag released at `pt` would move into: a folder row under
    /// the cursor (never one of the dragged items), else the parent directory
    /// when the cursor is over the Up affordance. `None` over anything else.
    fn folder_target_at(&self, pt: Point) -> Option<PathBuf> {
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
        None
    }

    /// Resolve and apply a drag's drop: move the carried row(s) into a folder
    /// (a row, or the parent via the Up target), else pin a single folder to
    /// Favourites when released over the sidebar. Clears the drag state.
    fn finish_drag(&mut self) {
        crate::external_drag::end();
        let target = self.folder_target_at(self.last_cursor);
        let over_sidebar =
            self.nav.is_sidebar_open() && self.nav.sidebar_rect().contains(self.last_cursor);
        let paths = std::mem::take(&mut self.drag_paths);
        let origin = self.pin_drag.take();
        self.drag_hover = None;
        if let Some(dir) = target {
            self.move_paths_into(&paths, &dir);
        } else if over_sidebar {
            // Only a single folder can be a favourite — files and multi-drags
            // are not pinnable. `is_dir` comes from the dragged row, so this
            // holds even for a folder that isn't physically on disk (tests).
            if paths.len() == 1
                && self.drag_origin_is_dir
                && let Some(path) = origin
            {
                self.apply_command(FileCommand::AddFavorite(path));
            }
        }
        self.drag_origin_is_dir = false;
    }

    /// Drop the drag without acting on it: the release belonged to someone
    /// else (the host delivered it to another application), or the gesture
    /// was abandoned.
    fn cancel_drag(&mut self) {
        crate::external_drag::end();
        self.drag_paths.clear();
        self.pin_drag = None;
        self.drag_hover = None;
        self.drag_origin_is_dir = false;
    }

    /// Move each of `paths` into directory `dir` as a real filesystem move
    /// (an honest side effect, not a CRDT action). No-ops and impossible moves
    /// — into itself, into its own subtree, or into the folder it already lives
    /// in — are skipped rather than erroring. Refreshes and reports the result.
    fn move_paths_into(&mut self, paths: &[PathBuf], dir: &std::path::Path) {
        let mut moved = 0usize;
        let mut errors = 0usize;
        for src in paths {
            let already_here = src.parent() == Some(dir);
            let into_self = src.as_path() == dir;
            let into_descendant = dir.starts_with(src);
            if already_here || into_self || into_descendant {
                continue;
            }
            match fileops::move_into(src, dir) {
                Ok(_) => moved += 1,
                Err(_) => errors += 1,
            }
        }
        if moved > 0 {
            self.refresh();
        }
        let dest = dir.file_name().map_or_else(
            || dir.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        self.status = Some(if errors > 0 {
            t_args(
                "files-moved-with-failures",
                &[
                    arg("moved", fluent_count(moved)),
                    arg("destination", dest),
                    arg("failed", fluent_count(errors)),
                ],
            )
        } else if moved > 0 {
            t_args(
                "files-moved-to-folder",
                &[arg("count", fluent_count(moved)), arg("destination", dest)],
            )
        } else {
            t("files-nothing-to-move")
        });
    }

    fn paste(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        let dest = self.current_path.clone();
        let cut = self.clipboard_cut;
        let items = self.clipboard.clone();
        let mut errors = 0usize;
        // A cut paste is allowed to complete only part of a multi-selection:
        // one source may have been removed or become unavailable between Cut
        // and Paste. Keep precisely those failed sources on the clipboard so
        // retrying neither repeats successful moves nor loses the unfinished
        // work. (Copies intentionally remain on the clipboard.)
        let mut remaining_cut_sources = Vec::new();
        for src in &items {
            let res = if cut {
                fileops::move_into(src, &dest)
            } else {
                fileops::copy_into(src, &dest)
            };
            if res.is_err() {
                errors += 1;
                if cut {
                    remaining_cut_sources.push(src.clone());
                }
            }
        }
        if cut {
            self.clipboard = remaining_cut_sources;
        }
        self.refresh();
        self.status = Some(if errors == 0 {
            t_args("files-pasted", &[arg("count", fluent_count(items.len()))])
        } else {
            t_args(
                "files-pasted-with-failures",
                &[
                    arg("count", fluent_count(items.len() - errors)),
                    arg("failed", fluent_count(errors)),
                ],
            )
        });
    }

    fn request_deferred_document_open(
        &mut self,
        path: PathBuf,
        capability: ui::widget::DocumentOpenCapability,
    ) -> Result<(), String> {
        if self.pending_document_open.is_some() {
            // One request already owns the only reply slot. Keep that
            // truthful pending state instead of replacing it with a second
            // status that the first successful reply could not clear.
            return Ok(());
        }
        let token = self
            .next_document_open_token
            .checked_add(1)
            .ok_or_else(|| "Document opening is unavailable".to_string())?;
        self.next_document_open_token = token;
        let reply = Rc::new(RefCell::new(ui::widget::DocumentOpenReply::pending(token)));
        self.host_intent = Some(ui::widget::AppHostIntent::OpenDocument {
            path,
            capability,
            token,
            reply: Rc::downgrade(&reply),
        });
        self.pending_document_open = Some((token, reply));
        self.set_external_status(t("files-opening-document"));
        Ok(())
    }

    fn poll_deferred_document_open(&mut self) {
        let Some((token, reply)) = &self.pending_document_open else {
            return;
        };
        let Some(result) = reply
            .try_borrow_mut()
            .ok()
            .and_then(|mut reply| reply.take_result(*token))
        else {
            return;
        };
        self.pending_document_open = None;
        match result {
            ui::widget::DocumentOpenOutcome::Opened => {
                if self.status.as_deref() == Some(t("files-opening-document").as_str()) {
                    self.status = None;
                    self.structure_dirty = true;
                }
            }
            ui::widget::DocumentOpenOutcome::Refused(reason) => self.set_external_status(reason),
            ui::widget::DocumentOpenOutcome::Cancelled => {
                self.set_external_status(t("files-document-opening-cancelled"));
            }
        }
    }

    /// Open a path: navigate into directories, else launch its app.
    fn open_path(&mut self, path: PathBuf) {
        let mut runner = SystemRunner;
        let _ = self.open_target(path, &mut runner);
    }

    /// Open `path`: navigate into directories, and hand a file to the
    /// desktop to open in whichever app owns it.
    ///
    /// The explorer picks no app of its own. It asks the host
    /// ([`set_document_opener`]), which resolves the owner, grants the
    /// read, focuses the document if it is already open, and launches —
    /// all in one place, shared with every other surface that can open a
    /// file. Only when no host installed a route (a standalone run, off a
    /// Tilo session, or a sandboxed Files) does this fall back to the
    /// desktop's handler (its launch request, via
    /// `CommandRunner::open_default`), which in a sandbox asks the session to
    /// open the file in the app that owns it.
    ///
    /// A refusal from the host is shown in the status bar naming the
    /// file; activation never silently does nothing.
    ///
    /// Returns whether the path was handed off (`false` when it navigated
    /// a dir).
    fn open_target<R: CommandRunner>(
        &mut self,
        path: PathBuf,
        runner: &mut R,
    ) -> std::io::Result<bool> {
        if let Some((archive, prefix)) = crate::archive::location(&path) {
            if prefix.as_os_str().is_empty() {
                self.navigate(path);
                return Ok(false);
            }
            let member = crate::archive::list(&archive)?
                .into_iter()
                .find(|entry| entry.path == prefix);
            let is_implicit_directory = member.is_none()
                && crate::archive::list(&archive)?
                    .iter()
                    .any(|entry| entry.path.starts_with(&prefix));
            if member.as_ref().is_some_and(|entry| entry.is_dir) || is_implicit_directory {
                self.navigate(path);
            } else {
                self.status = Some(t("files-archive-open-after-extract"));
            }
            return Ok(false);
        }
        if path.is_dir() {
            self.navigate(path);
            return Ok(false);
        }
        let hosted_route = match &self.services {
            FileServices::Legacy => None,
            FileServices::Hosted(hosted) => Some(hosted.document_opener.clone()),
        };
        let result = match hosted_route {
            None => ask_host_to_open(&path),
            Some(DocumentOpenRoute::Unavailable) => {
                Some(Err("Document opening is unavailable".into()))
            }
            Some(DocumentOpenRoute::Immediate(open)) => Some(open.borrow_mut()(&path)),
            Some(DocumentOpenRoute::Deferred(capability)) => {
                Some(self.request_deferred_document_open(path.clone(), capability))
            }
        };
        match result {
            Some(Ok(())) => self.note_recent(&path, crate::recent::RecentUse::Opened),
            Some(Err(reason)) => self.status = Some(reason),
            None => {
                let launch = runner.open_default(&path);
                self.pending_launch = Some((path, launch));
                self.poll_pending_launch();
            }
        }
        Ok(true)
    }

    /// Report the desktop's answer to a file handed to its default handler.
    fn poll_pending_launch(&mut self) {
        let Some((path, launch)) = &self.pending_launch else {
            return;
        };
        let outcome = match launch.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ui::portal::LaunchOutcome::Failed("the desktop did not answer".into())
            }
        };
        let path = path.clone();
        self.pending_launch = None;
        match outcome {
            ui::portal::LaunchOutcome::Opened => {
                self.note_recent(&path, crate::recent::RecentUse::Opened);
            }
            ui::portal::LaunchOutcome::Failed(reason) => self.set_external_status(reason),
        }
    }

    // --- inline rename -------------------------------------------------

    /// The transient status line shown in the footer — an operation
    /// result, or the reason an open was refused. Readable so a host (or
    /// a test standing in for one) can confirm the explorer told the user
    /// something rather than swallowing it.
    pub fn status_line(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Whether an inline rename is currently in progress.
    pub fn is_renaming(&self) -> bool {
        self.renaming.is_some()
    }

    /// The current editable rename text (the field's live contents).
    pub fn rename_buffer(&self) -> &str {
        &self.rename_buffer
    }

    /// Replace the editable rename text. The next `sync` seeds the local
    /// editor; this test seam deliberately has no document side effect.
    pub fn set_rename_buffer(&mut self, s: String) {
        self.rename_buffer = s;
        self.rename_seed_pending = true;
    }

    /// Begin an inline rename of the primary selection: open the in-place editor
    /// over the row, seeded with the entry's current name. No-op without a
    /// selection.
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

    /// Tear down the in-place editor: drop focus and clear its published rect so
    /// no stale field is drawn after a rename ends.
    fn end_rename_editor(&mut self) {
        self.renaming = None;
        self.rename_seed_pending = false;
        self.rename_select_pending = false;
        self.rename_input.set_focus(false);
        self.rename_rect.set(None);
    }

    /// Abandon the in-progress rename, leaving the file untouched.
    pub fn cancel_rename(&mut self) {
        self.end_rename_editor();
    }

    /// Paint the app background and lazily build the composition if it is
    /// missing (a tab activated after boot never ran `build_layout`). Shared
    /// by [`Widget::draw`] and [`Widget::draw_with_presence`].
    fn paint_background(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        theme: &ui::Theme,
    ) {
        // Paint the app background so the explorer is self-contained (light
        // text reads on it, and it looks right over any compositor surface).
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        // The chrome + body panes are normally built inside `build_layout`.
        // When this widget is hosted inside a shell's TabBar (e.g. the Files
        // tab on a Tilo shell that boots Notes-active), `build_layout` doesn't
        // run for inactive tabs — so switching to Files later produces a widget
        // with no chrome built and nothing visible renders even though
        // `self.rect` is correct. Flag structure-dirty here so the host runs a
        // full rebuild (which populates the chrome + swaps the real panes into
        // the persistent `nav`) on the next frame.
        let _ = theme;
        if !self.composed_once && self.rect.width() > 0.0 && self.rect.height() > 0.0 {
            self.structure_dirty = true;
        }
    }

    /// Draw a centred empty-state in the file area when the current directory
    /// has no entries (and isn't showing an error) — so an empty folder, and
    /// especially an empty Trash, reads as intentionally empty rather than
    /// broken. A muted icon over a short, context-aware line.
    fn draw_empty_state(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
    ) {
        if !self.entries.is_empty() || self.error.is_some() || self.is_loading {
            return;
        }
        let rect = self.nav.overview_rect();
        if rect.width() <= 1.0 || rect.height() <= 1.0 {
            return;
        }
        let (icon, msg) = if self.is_recent_view() {
            (ui::icons::Icon::History, self.empty_state_message())
        } else if self.is_trash_view() {
            (ui::icons::Icon::Trash2, self.empty_state_message())
        } else {
            (ui::icons::Icon::Folder, self.empty_state_message())
        };
        let tokens = &theme.tokens;
        let dim = tokens.colors.text_dim;
        let icon_px = 40.0_f64;
        // The theme token is already text-scaled and the renderer scales
        // again, so hand it the nominal size (one scale rule, see
        // `crate::text_layout`).
        let text_scale = text_renderer.text_scale();
        let size = tokens.typography.scale.base / text_scale;
        let (tw, _) = text_renderer.measure(&msg, size, None);
        let cx = rect.x0 + rect.width() / 2.0;
        let cy = rect.y0 + rect.height() / 2.0;
        // Icon centred above the line.
        let ix = cx - icon_px / 2.0;
        let iy = cy - icon_px - 10.0;
        ui::icons::draw_icon(
            scene,
            transform,
            vello::kurbo::Rect::new(ix, iy, ix + icon_px, iy + icon_px),
            icon,
            dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let ty = crate::text_layout::centered_text_y(
            cy,
            f64::from(size) * f64::from(text_scale),
            size,
            text_scale,
        );
        text_renderer.draw_text(
            scene,
            &msg,
            size,
            dim,
            transform * vello::kurbo::Affine::translate((cx - f64::from(tw) / 2.0, ty)),
            None,
        );
    }

    fn collab_panel_rect(&self) -> vello::kurbo::Rect {
        let line_count = self
            .collab_panel
            .as_ref()
            .map_or(1, |panel| panel.lines.len().clamp(1, 14));
        let width = self.rect.width().clamp(320.0, 680.0) - 32.0;
        let action_height = if self
            .collab_panel
            .as_ref()
            .is_some_and(|panel| panel.action.is_some())
        {
            58.0
        } else {
            0.0
        };
        let height = (116.0 + line_count as f64 * 27.0 + action_height)
            .min(self.rect.height() - 32.0)
            .max(180.0);
        let x0 = self.rect.x0 + (self.rect.width() - width) / 2.0;
        let y0 = self.rect.y0 + (self.rect.height() - height) / 2.0;
        vello::kurbo::Rect::new(x0, y0, x0 + width, y0 + height)
    }

    fn collab_panel_action_rect(&self) -> Option<vello::kurbo::Rect> {
        self.collab_panel.as_ref()?.action.as_ref()?;
        let panel = self.collab_panel_rect();
        Some(vello::kurbo::Rect::new(
            panel.x0 + 24.0,
            panel.y1 - 50.0,
            panel.x1 - 24.0,
            panel.y1 - 14.0,
        ))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "modal painting keeps scene, transform, text and theme together while deriving geometry and hit targets in one pass"
    )]
    // Drawing the modal as one routine keeps geometry, focus, and action hit
    // targets derived from the same bounded panel state.
    #[allow(
        clippy::too_many_lines,
        reason = "modal painting derives geometry, focus, and hit targets from one bounded panel state"
    )]
    fn draw_collab_panel(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
    ) {
        use ui::ColorCompat;
        use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
        use vello::peniko::Fill;
        let Some(panel) = &self.collab_panel else {
            return;
        };
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base.with_alpha_factor(0.72),
            None,
            &self.rect,
        );
        let rect = self.collab_panel_rect();
        // Panel role (HIG §9.1/§9.5): the sheet is the same material as every
        // other floating Panel — `panel_ground` under the derived Panel
        // border, with its elevation — not a raised slot stroked with a
        // declared border that vanishes on the dark preset.
        let radius = ui::chrome::ChromeRole::Panel.radius(theme, rect.height());
        let shape = RoundedRect::from_rect(rect, radius);
        ui::chrome::panel_shadow(scene, transform, rect, radius, theme, 1.0);
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::panel_ground(theme),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::chrome::panel_border(theme),
            None,
            &ui::surface::edge_outline(rect, radius),
        );
        let x = rect.x0 + 24.0;
        text_renderer.draw_text(
            scene,
            &panel.title,
            theme.tokens.typography.scale.lg,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x, rect.y0 + 23.0)),
            Some((rect.width() - 80.0) as f32),
        );
        text_renderer.draw_text(
            scene,
            &panel.summary,
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((x, rect.y0 + 53.0)),
            Some((rect.width() - 48.0) as f32),
        );

        let accent = theme.tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX];
        let mut y = rect.y0 + 88.0;
        for line in panel.lines.iter().take(14) {
            scene.fill(
                Fill::NonZero,
                transform,
                accent,
                None,
                &Rect::new(x, y + 5.0, x + 5.0, y + 10.0),
            );
            text_renderer.draw_text(
                scene,
                line,
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((x + 14.0, y)),
                Some((rect.width() - 62.0) as f32),
            );
            y += 27.0;
            if y > rect.y1 - 26.0 {
                break;
            }
        }
        if let Some(action_rect) = self.collab_panel_action_rect()
            && let Some(label) = self.collab_panel_action_label()
        {
            // The shared button face (HIG §8): the outlined destructive style
            // for the irreversible actions, the primary for a safe merge.
            let face = ui::widget::ButtonFace::new(action_rect, &label)
                .text_size(theme.tokens.typography.scale.sm)
                .wrap_label();
            let face = match panel.action.as_ref() {
                Some(
                    CollabPanelAction::ApplySafeMerge { .. }
                    | CollabPanelAction::ChooseNearbyDestination { .. }
                    | CollabPanelAction::RetryNearby { .. },
                ) => face.primary(),
                _ => face.danger_outline(),
            };
            ui::widget::Button::paint(scene, transform, text_renderer, theme, &face);
        }
        let close_rect = Rect::new(
            rect.x1 - 42.0,
            rect.y0 + 12.0,
            rect.x1 - 14.0,
            rect.y0 + 40.0,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            close_rect,
            ui::icons::Icon::X,
            theme.tokens.colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    /// Draw the in-place rename editor over the row/cell being renamed. The
    /// file view publishes the target rect (in on-screen coords) during its own
    /// paint and omits that row's label, so the field slots exactly where the
    /// name was. Nothing is drawn when not renaming or when the row is scrolled
    /// out of view (the published rect is then `None`).
    fn draw_rename_editor(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if self.renaming.is_none() {
            return;
        }
        if let Some(rect) = self.rename_rect.get() {
            self.rename_input.apply_layout_external(rect);
            self.rename_input
                .draw(scene, transform, text_renderer, theme, presence);
        }
    }

    /// Draw the accent highlight over the folder (or Up affordance) a live drag
    /// is hovering, so the drop target is unmistakable. Drawn under the ghost.
    fn draw_drop_highlight(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        theme: &ui::Theme,
    ) {
        use ui::ColorCompat;
        use vello::kurbo::{Rect, RoundedRect, Stroke};
        use vello::peniko::Fill;
        let Some(hover) = &self.drag_hover else {
            return;
        };
        // A row target carries its published rect; the parent (Up) target uses
        // the Up button's rect.
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
            });
        let Some(rect) = rect else { return };
        // A live drop target is state the user did not choose and must notice
        // (HIG §9.4), so the accent is the right ink here. The face matches
        // the row highlight's insets so the two never disagree by a pixel.
        let accent = theme.tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX];
        let (ix, iy) = (
            f64::from(theme.tokens.spacing.s2),
            f64::from(theme.tokens.spacing.s1) / 2.0,
        );
        let inset = Rect::new(rect.x0 + ix, rect.y0 + iy, rect.x1 - ix, rect.y1 - iy);
        let shape = RoundedRect::from_rect(inset, f64::from(theme.tokens.radii.md));
        scene.fill(
            Fill::NonZero,
            transform,
            accent.with_alpha_factor(0.25),
            None,
            &shape,
        );
        scene.stroke(&Stroke::new(2.0), transform, accent, None, &shape);
    }

    /// Draw the floating ghost that tracks the cursor during a drag, so it reads
    /// as carrying the row(s). Shows the dragged item's own icon/name, or an
    /// item count when several are carried at once.
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
            t_args("files-drag-items", &[arg("count", fluent_count(count))])
        } else {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        };
        let tokens = &theme.tokens;
        let text_scale = text_renderer.text_scale();
        let size = tokens.typography.scale.base / text_scale;
        let (tw, th) = text_renderer.measure(&label, size, None);
        let (pad, icon, gap) = (10.0_f64, 16.0_f64, 8.0_f64);
        // The pill grows with its measured label so enlarged text stays inside it.
        let h = (f64::from(th) + 2.0 * f64::from(tokens.spacing.s1)).max(30.0);
        let w = pad + icon + gap + f64::from(tw) + pad;
        // Offset from the cursor so the ghost sits beside, not under, the pointer.
        let x0 = self.last_cursor.x + 14.0;
        let y0 = self.last_cursor.y + 6.0;
        let rect = Rect::new(x0, y0, x0 + w, y0 + h);
        // The ghost is genuinely off the surface (HIG §9.3): it casts the
        // Panel shadow and its edge derives from its own raised face.
        let radius = f64::from(tokens.radii.md);
        ui::chrome::panel_shadow(scene, transform, rect, radius, theme, 1.0);
        ui::surface::card(
            scene,
            transform,
            rect,
            radius,
            tokens.colors.surfaces.raised,
        );
        let accent = tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX];
        let iy = y0 + (h - icon) / 2.0;
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(x0 + pad, iy, x0 + pad + icon, iy + icon),
            glyph,
            accent,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let ty = crate::text_layout::centered_text_y(y0, h, size, text_scale);
        text_renderer.draw_text(
            scene,
            &label,
            size,
            tokens.colors.text_primary,
            transform * Affine::translate((x0 + pad + icon + gap, ty)),
            None,
        );
    }

    /// Commit the in-progress rename: rename the target file to
    /// `rename_buffer` (trimmed). Returns whether the file was renamed. A
    /// blank or unchanged name just closes the bar; an fs error keeps the
    /// bar open with a status message so the user can retry.
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
        match fileops::rename(&path, &new_name) {
            Ok(renamed) => {
                self.recent_moved(&path, &renamed);
                self.end_rename_editor();
                self.refresh();
                self.status = Some(t("files-renamed"));
                true
            }
            Err(e) => {
                // Keep the bar open so the user can fix the name.
                self.status = Some(t_args("files-rename-failed", &[arg("error", e)]));
                false
            }
        }
    }
}

/// Resolve a standalone [`ContextMenu`]'s rect from its anchor (it isn't part
/// of the App's taffy tree, so drive a throwaway layout pass). Shared by the
/// main menu and the "New File" flyout.
fn layout_menu_widget(menu: &mut ContextMenu) {
    if !menu.is_open() {
        return;
    }
    let mut taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
    let mut node_map: std::collections::HashMap<taffy::NodeId, *mut dyn Widget> =
        std::collections::HashMap::new();
    let theme = ui::Theme::dark();
    let node = menu.build_layout(&mut taffy, &theme, &mut node_map);
    let _ = taffy.compute_layout(
        node,
        taffy::Size {
            width: taffy::AvailableSpace::Definite(400.0),
            height: taffy::AvailableSpace::Definite(800.0),
        },
    );
    menu.apply_layout(&taffy, node, Point::ZERO);
}

/// Thin mockable wrapper around process launch + the desktop "open with
/// default app" handler. Mocked in tests so the open/launch paths are
/// verifiable without actually spawning anything.
pub trait CommandRunner {
    /// Spawn `program` in `cwd`.
    ///
    /// # Errors
    ///
    /// Returns the OS error if the process cannot be spawned — `program`
    /// is not on `PATH`, or the current user lacks permission to run it.
    fn spawn(&mut self, program: &str, cwd: &std::path::Path) -> std::io::Result<()>;
    /// Spawn `program` in `cwd` with `arg` as its sole argument.
    ///
    /// # Errors
    ///
    /// Returns the OS error if the process cannot be spawned — `program`
    /// is not on `PATH`, or the current user lacks permission to run it.
    fn spawn_arg(
        &mut self,
        program: &str,
        arg: &std::path::Path,
        cwd: &std::path::Path,
    ) -> std::io::Result<()>;
    /// Open `path` with the system default handler. This is the fallback for
    /// every file type Tilo has no registered app for (images, PDFs, video,
    /// audio, plain text, …) so double-click/Enter/Open never silently do
    /// nothing.
    ///
    /// The desktop's answer arrives on the returned channel: a sandboxed
    /// explorer asks the session (its portal, or the shell) to open the file
    /// in the app that owns it.
    fn open_default(&mut self, path: &std::path::Path) -> ui::portal::LaunchChannel;
}

pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn spawn(&mut self, program: &str, cwd: &std::path::Path) -> std::io::Result<()> {
        #[cfg(test)]
        crate::model::fixture_io_guard::ambient_io("SystemRunner process");
        std::process::Command::new(program)
            .current_dir(cwd)
            .spawn()
            .map(|_c| ())
    }
    fn spawn_arg(
        &mut self,
        program: &str,
        arg: &std::path::Path,
        cwd: &std::path::Path,
    ) -> std::io::Result<()> {
        #[cfg(test)]
        crate::model::fixture_io_guard::ambient_io("SystemRunner process");
        std::process::Command::new(program)
            .arg(arg)
            .current_dir(cwd)
            .spawn()
            .map(|_c| ())
    }
    fn open_default(&mut self, path: &std::path::Path) -> ui::portal::LaunchChannel {
        #[cfg(test)]
        crate::model::fixture_io_guard::ambient_io("SystemRunner::open_default");
        ui::portal::LaunchRequest::open(path).send()
    }
}

/// Whether `path` is an editable document, per qdoc's codec registry.
///
/// A presentation fact only — it drives the document glyph in the icon
/// column. It deliberately says nothing about *which* app opens the file:
/// that is the desktop's answer, from the manifests of what is installed,
/// and the explorer asks for it rather than deciding it (see
/// [`set_document_opener`]).
pub fn is_document(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(tilo_doc_codecs::is_supported_ext)
}

/// Presentation-side lower bound for the daemon's fail-closed merge codecs.
/// The daemon still validates exact bytes and may reject malformed qdoc,
/// invalid UTF-8, overlapping edits, or a stale signed conflict.
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

/// How the explorer hands a document to the application that owns it.
///
/// `Ok(())` means the host took it. `Err(reason)` is a failure the user
/// must be told about, in words naming the file — the explorer shows it
/// in its status bar.
pub type DocumentOpener = Box<dyn FnMut(&std::path::Path) -> Result<(), String>>;

thread_local! {
    static DOCUMENT_OPENER: std::cell::RefCell<Option<DocumentOpener>> =
        const { std::cell::RefCell::new(None) };
}

/// Install the route by which this explorer opens documents.
///
/// The explorer does not decide which app opens a `.odt`, and does not
/// launch one. That decision belongs to the desktop — which app declares
/// the type, which sandbox it runs in, which read capability it needs,
/// and whether the document is already open somewhere — and it lives in
/// one place, the compositor's `shell::shell::doc_open`. An explorer
/// hosted by the compositor is wired straight to it here; nothing about
/// opening a document is reimplemented on this side.
///
/// Thread-local rather than a global because what it points at is the
/// compositor's chrome, which is thread-affine, and because it then
/// reaches *every* explorer instance the host creates without threading a
/// parameter through each widget constructor.
///
/// With nothing installed — the explorer run standalone, with no
/// compositor to ask — activation falls back to the desktop's own
/// handler. That is the honest answer off a Tilo session, not a second
/// implementation of the decision.
pub fn set_document_opener<F>(opener: F)
where
    F: FnMut(&std::path::Path) -> Result<(), String> + 'static,
{
    DOCUMENT_OPENER.with(|slot| *slot.borrow_mut() = Some(Box::new(opener)));
}

/// Remove the installed route, restoring the standalone fallback. Used by
/// tests, which share the thread-local with each other.
pub fn clear_document_opener() {
    DOCUMENT_OPENER.with(|slot| *slot.borrow_mut() = None);
}

/// Ask the host to open `path`. `None` when no host installed a route.
fn ask_host_to_open(path: &std::path::Path) -> Option<Result<(), String>> {
    #[cfg(test)]
    crate::model::fixture_io_guard::ambient_io("ask_host_to_open");
    DOCUMENT_OPENER.with(|slot| slot.borrow_mut().as_mut().map(|open| open(path)))
}

impl FileExplorerWidget {
    /// Launch a terminal rooted at the current directory.
    ///
    /// # Errors
    ///
    /// Returns the OS error if the terminal process cannot be spawned.
    pub fn open_terminal<R: CommandRunner>(&self, runner: &mut R) -> std::io::Result<()> {
        runner.spawn("terminal", &self.current_path)
    }

    /// Activate entry `idx` (by displayed index): navigate into a directory
    /// or launch the associated app.
    ///
    /// # Errors
    ///
    /// Returns the OS error if launching the entry's associated app fails.
    pub fn open_entry<R: CommandRunner>(
        &mut self,
        idx: usize,
        runner: &mut R,
    ) -> std::io::Result<bool> {
        let disp = self.displayed_entries();
        let Some(entry) = disp.get(idx) else {
            return Ok(false);
        };
        let path = entry.path.clone();
        self.open_target(path, runner)
    }

    /// Build + open the right-click menu at `at`, with items appropriate to
    /// the current selection and clipboard.
    /// The template files currently available for the "New File ▸" submenu.
    pub fn available_templates(&self) -> Vec<PathBuf> {
        templates::templates_dir()
            .map(|d| templates::list_templates(&d))
            .unwrap_or_default()
    }

    /// The menu item specs for the current selection / clipboard / state, given
    /// the available `templates`. Returns `(label, command)` pairs; a `None`
    /// command with an empty label is a separator. Pure (templates injected) so
    /// the menu contents — including the "New File ▸" gating — are unit-testable.
    // This declarative menu state machine intentionally keeps visibility and
    // command gating adjacent for auditability and accessibility parity.
    #[allow(
        clippy::too_many_lines,
        reason = "one declarative menu specification keeps visibility, enablement, and command identity auditable"
    )]
    fn context_menu_spec(&self, templates: &[PathBuf]) -> Vec<(String, Option<FileCommand>)> {
        let has_sel = !self.selection.is_empty();
        let can_paste = !self.clipboard.is_empty();
        let has_templates = !templates.is_empty();
        let in_trash = self.is_trash_view();
        let mut spec: Vec<(String, Option<FileCommand>)> = Vec::new();

        // Recent gathers files from many folders: it is for finding and
        // reopening, not for editing in place.
        if self.is_recent_view() {
            return self.recent_menu_spec();
        }

        // An archive is a read-only folder view. Synthetic member paths must
        // never leak into filesystem mutation commands such as rename, trash,
        // drag, paste, sharing, or terminal launch.
        if crate::archive::location(&self.current_path).is_some() {
            if has_sel {
                spec.push((
                    t("files-menu-open"),
                    self.selection.primary().map(FileCommand::Activate),
                ));
                spec.push((String::new(), None));
            }
            spec.push((
                t("files-archive-extract"),
                Some(FileCommand::ExtractCurrentArchive),
            ));
            return spec;
        }

        // The Trash is a restore/purge surface, not an editing one: offer Open
        // plus Restore / permanent-delete / Empty, and nothing that would edit
        // in place.
        if in_trash {
            if has_sel {
                spec.push((
                    t("files-menu-open"),
                    self.selection.primary().map(FileCommand::Activate),
                ));
                spec.push((String::new(), None));
                spec.push((t("files-menu-restore"), Some(FileCommand::RestoreFromTrash)));
                spec.push((
                    t("files-menu-delete-permanently"),
                    Some(FileCommand::Delete),
                ));
            }
            spec.push((String::new(), None));
            spec.push((t("files-menu-empty-trash"), Some(FileCommand::EmptyTrash)));
            return spec;
        }

        if has_sel {
            spec.push((
                t("files-menu-open"),
                self.selection.primary().map(FileCommand::Activate),
            ));
            if self.open_with_available() {
                spec.push((t("files-menu-open-with"), Some(FileCommand::OpenWithMenu)));
            }
            spec.push((t("files-menu-rename"), Some(FileCommand::Rename)));
            spec.push((String::new(), None));
            spec.push((t("files-menu-cut"), Some(FileCommand::Cut)));
            spec.push((t("files-menu-copy"), Some(FileCommand::Copy)));
            if self.selection.selected().len() == 1 {
                spec.push((
                    t("files-menu-send-nearby"),
                    Some(FileCommand::OpenNearbySend),
                ));
                if !self
                    .shortcut_menu
                    .offered_on(&self.selected_entry_identities())
                    .is_empty()
                {
                    spec.push((
                        t("files-menu-run-shortcut"),
                        Some(FileCommand::OpenShortcutsMenu),
                    ));
                }
            }
            let displayed = self.displayed_entries();
            if self.selection.selected().iter().any(|index| {
                displayed
                    .get(*index)
                    .is_some_and(|entry| crate::archive::is_supported(&entry.path))
            }) {
                spec.push((
                    t("files-archive-extract-here"),
                    Some(FileCommand::ExtractHere),
                ));
            }
            spec.push((
                t("files-archive-compress"),
                Some(FileCommand::CompressSelection),
            ));
            let collaborative: Vec<_> = self
                .selection
                .selected()
                .iter()
                .filter_map(|index| displayed.get(*index))
                .filter(|entry| entry.availability.is_some())
                .collect();
            if !collaborative.is_empty() {
                let any_automatic = collaborative.iter().any(|entry| {
                    entry.availability.as_ref().is_some_and(|availability| {
                        availability.policy == crate::model::AvailabilityPolicy::Automatic
                    })
                });
                let any_directly_pinned = collaborative.iter().any(|entry| {
                    entry.availability.as_ref().is_some_and(|availability| {
                        availability.policy == crate::model::AvailabilityPolicy::Pinned
                            && !availability.policy_inherited
                    })
                });
                spec.push((String::new(), None));
                let any_inherited = collaborative.iter().any(|entry| {
                    entry
                        .availability
                        .as_ref()
                        .is_some_and(|availability| availability.policy_inherited)
                });
                if any_automatic {
                    spec.push((t("files-keep-offline"), Some(FileCommand::KeepOffline)));
                }
                if any_directly_pinned {
                    spec.push((
                        t("files-use-automatic-storage"),
                        Some(FileCommand::RemoveOfflinePin),
                    ));
                }
                if any_inherited && !any_directly_pinned {
                    spec.push((t("files-pinned-by-parent"), None));
                }
                let any_releasable = collaborative.iter().any(|entry| {
                    !entry.is_dir
                        && entry.availability.as_ref().is_some_and(|availability| {
                            availability.policy == crate::model::AvailabilityPolicy::Automatic
                                && !availability.policy_inherited
                                && availability.transfer.phase == crate::model::TransferPhase::Idle
                                && matches!(
                                    availability.residency,
                                    crate::model::Residency::Partial { .. }
                                        | crate::model::Residency::Resident { .. }
                                )
                        })
                });
                if any_releasable {
                    spec.push((
                        t("files-free-local-space"),
                        Some(FileCommand::FreeLocalSpace),
                    ));
                }
                let any_retryable = collaborative.iter().any(|entry| {
                    entry.availability.as_ref().is_some_and(|availability| {
                        !matches!(
                            availability.residency,
                            crate::model::Residency::Resident { .. }
                        ) && availability.transfer.phase == crate::model::TransferPhase::Idle
                    })
                });
                if any_retryable {
                    spec.push((t("files-retry-download"), Some(FileCommand::RetryTransfer)));
                }
            }
            if self.selection.selected().len() == 1
                && let Some(entry) = self
                    .selection
                    .primary()
                    .and_then(|index| displayed.get(index))
            {
                spec.push((
                    t("files-menu-properties"),
                    Some(FileCommand::ShowProperties),
                ));
                if !entry.is_dir
                    && entry.record_id.is_some()
                    && crate::model::share_control_socket(&entry.path, &self.workspace_roots)
                        .is_some()
                {
                    for human in self
                        .trusted_humans
                        .humans()
                        .iter()
                        .filter(|human| !human.devices.is_empty())
                        .take(16)
                    {
                        // Keep the concise default, then expose every named
                        // permission/expiry choice without ever placing the
                        // display label into the authority command.
                        spec.push((
                            t_args(
                                "files-share-with",
                                &[arg("person", human.display_name.clone())],
                            ),
                            Some(FileCommand::ShareWithPerson {
                                person: human.person,
                            }),
                        ));
                        for permission in [
                            collab_types::FilePermission::View,
                            collab_types::FilePermission::Edit,
                        ] {
                            for expiry in [
                                crate::sharing::ShareExpiryChoice::Day,
                                crate::sharing::ShareExpiryChoice::Week,
                                crate::sharing::ShareExpiryChoice::Month,
                                crate::sharing::ShareExpiryChoice::Never,
                            ] {
                                spec.push((
                                    t_args(
                                        "files-share-composer-named",
                                        &[
                                            arg("person", human.display_name.clone()),
                                            arg("permission", share_permission_label(permission)),
                                            arg("expiry", share_expiry_label(expiry)),
                                        ],
                                    ),
                                    Some(FileCommand::CreateShare(
                                        crate::sharing::ShareComposerRequest {
                                            subject: crate::sharing::ShareSubjectChoice::Named(
                                                human.person,
                                            ),
                                            permission,
                                            expiry,
                                            link_uses: None,
                                            recipient_devices: human
                                                .devices
                                                .iter()
                                                .map(|device| device.device)
                                                .collect(),
                                        },
                                    )),
                                ));
                            }
                        }
                    }
                    spec.push((
                        t("files-copy-one-time-link"),
                        Some(FileCommand::CreateOneTimeShareLink),
                    ));
                    for permission in [
                        collab_types::FilePermission::View,
                        collab_types::FilePermission::Edit,
                    ] {
                        for expiry in [
                            crate::sharing::ShareExpiryChoice::Day,
                            crate::sharing::ShareExpiryChoice::Week,
                            crate::sharing::ShareExpiryChoice::Month,
                        ] {
                            for link_uses in [
                                crate::sharing::LinkUseChoice::OneUse,
                                crate::sharing::LinkUseChoice::Bounded(10),
                            ] {
                                let uses = match link_uses {
                                    crate::sharing::LinkUseChoice::OneUse => {
                                        t("files-share-choice-one-use")
                                    }
                                    crate::sharing::LinkUseChoice::Bounded(count) => t_args(
                                        "files-share-choice-bounded-uses",
                                        &[arg("count", i64::from(count))],
                                    ),
                                };
                                spec.push((
                                    t_args(
                                        "files-share-composer-link",
                                        &[
                                            arg("permission", share_permission_label(permission)),
                                            arg("expiry", share_expiry_label(expiry)),
                                            arg("uses", uses),
                                        ],
                                    ),
                                    Some(FileCommand::CreateShare(
                                        crate::sharing::ShareComposerRequest {
                                            subject: crate::sharing::ShareSubjectChoice::Link,
                                            permission,
                                            expiry,
                                            link_uses: Some(link_uses),
                                            recipient_devices: Vec::new(),
                                        },
                                    )),
                                ));
                            }
                        }
                    }
                }
                if entry.share.is_some() {
                    spec.push((t("files-sharing-access"), Some(FileCommand::ShowSharing)));
                }
                if entry.availability.is_some() {
                    spec.push((
                        t("files-availability-title-short"),
                        Some(FileCommand::ShowAvailability),
                    ));
                }
                if crate::model::share_control_socket(&entry.path, &self.workspace_roots).is_some()
                    && let Some(share) = &entry.share
                {
                    for grant in share
                        .grants
                        .iter()
                        .filter(|grant| grant.status == crate::model::ShareGrantStatus::Active)
                    {
                        let Ok(grant_id) = uuid::Uuid::parse_str(&grant.grant_id) else {
                            continue;
                        };
                        let subject = match grant.subject {
                            crate::model::ShareSubject::Person { person } => {
                                let person = collab_types::Ed25519PublicKey::from_bytes(person);
                                self.trusted_humans.display_name_or_fingerprint(&person)
                            }
                            crate::model::ShareSubject::BearerLink => t("files-shared-link"),
                        };
                        spec.push((
                            t_args("files-revoke-subject", &[arg("subject", subject)]),
                            Some(FileCommand::RevokeShare { grant_id }),
                        ));
                    }
                }
                if entry.history.is_some() {
                    spec.push((t("files-version-history"), Some(FileCommand::ShowHistory)));
                    if let Some(version_id) = entry.history.as_ref().and_then(|history| {
                        history.versions.iter().rev().find_map(|version| {
                            (!history.heads.contains(&version.id) && version.content_hash.is_some())
                                .then(|| uuid::Uuid::parse_str(&version.id).ok())
                                .flatten()
                                .map(collab_types::FileVersionId)
                        })
                    }) {
                        spec.push((
                            t("files-restore-previous"),
                            Some(FileCommand::RestoreVersion { version_id }),
                        ));
                    }
                    if let (Some(history), Some(current_hash)) =
                        (entry.history.as_ref(), entry.content_hash)
                    {
                        for conflict in history.open_conflicts() {
                            let members = conflict
                                .versions
                                .iter()
                                .filter_map(|id| {
                                    history.versions.iter().find(|version| {
                                        uuid::Uuid::parse_str(&version.id).ok() == Some(id.0)
                                    })
                                })
                                .collect::<Vec<_>>();
                            let current = members
                                .iter()
                                .copied()
                                .filter(|version| version.content_hash == Some(current_hash))
                                .collect::<Vec<_>>();
                            // An exact unique hash match is the strongest fact
                            // this bounded V1 projection exposes. The daemon
                            // additionally CAS-checks its signed revision, so a
                            // stale menu action fails instead of changing meaning.
                            if current.len() != 1 {
                                continue;
                            }
                            let Some(current_version) = current.first() else {
                                continue;
                            };
                            let current_projection_id = current_version.id.as_str();
                            let Ok(current_id) = uuid::Uuid::parse_str(current_projection_id)
                            else {
                                continue;
                            };
                            spec.push((String::new(), None));
                            spec.push((
                                t_args(
                                    "files-resolve-keep-current",
                                    &[arg("conflict", short_uuid(conflict.id))],
                                ),
                                Some(FileCommand::ResolveConflict {
                                    conflict_id: conflict.id,
                                    strategy: collab_types::ConflictStrategy::KeepCurrent,
                                    version_id: collab_types::FileVersionId(current_id),
                                }),
                            ));
                            for other in members
                                .into_iter()
                                .filter(|version| version.id != current_projection_id)
                            {
                                let Ok(other_id) = uuid::Uuid::parse_str(&other.id) else {
                                    continue;
                                };
                                spec.push((
                                    t_args(
                                        "files-resolve-use-other",
                                        &[
                                            arg("conflict", short_uuid(conflict.id)),
                                            arg("device", short_fingerprint(&other.actor.device)),
                                        ],
                                    ),
                                    Some(FileCommand::ResolveConflict {
                                        conflict_id: conflict.id,
                                        strategy: collab_types::ConflictStrategy::UseOther,
                                        version_id: collab_types::FileVersionId(other_id),
                                    }),
                                ));
                                if merge_supported_path(&entry.path) {
                                    spec.push((
                                        format!(
                                            "{} · {}",
                                            t("files-merge-review-menu"),
                                            short_fingerprint(&other.actor.device),
                                        ),
                                        Some(FileCommand::ReviewConflictMerge {
                                            conflict_id: conflict.id,
                                            version_id: collab_types::FileVersionId(other_id),
                                            other_device: other.actor.device,
                                        }),
                                    ));
                                }
                                spec.push((
                                    format!(
                                        "{} · {}",
                                        conflict_strategy_label(
                                            collab_types::ConflictStrategy::KeepBoth,
                                        ),
                                        short_fingerprint(&other.actor.device),
                                    ),
                                    Some(FileCommand::ResolveConflict {
                                        conflict_id: conflict.id,
                                        strategy: collab_types::ConflictStrategy::KeepBoth,
                                        version_id: collab_types::FileVersionId(other_id),
                                    }),
                                ));
                            }
                        }
                    }
                }
            }
        }
        if can_paste {
            spec.push((t("files-menu-paste"), Some(FileCommand::Paste)));
        }
        spec.push((String::new(), None));
        spec.push((t("files-menu-new-folder"), Some(FileCommand::NewFolder)));
        // Only offer "New File ▸" when the Templates dir actually has files.
        if has_templates {
            spec.push((
                t("files-menu-new-file"),
                Some(FileCommand::OpenTemplatesMenu),
            ));
        }
        if has_sel {
            spec.push((String::new(), None));
            spec.push((t("files-menu-move-trash"), Some(FileCommand::Delete)));
        }

        // Favourites: add the single selected folder, or remove the current
        // directory when it is a (non-home) favourite.
        if let Some(dir) = self.single_selected_dir() {
            spec.push((String::new(), None));
            spec.push((
                t("files-menu-add-favourite"),
                Some(FileCommand::AddFavorite(dir)),
            ));
        } else if self.current_is_removable_favourite() {
            spec.push((String::new(), None));
            spec.push((
                t("files-menu-remove-favourite"),
                Some(FileCommand::RemoveFavorite(self.current_path.clone())),
            ));
        }

        // View section: show/hide hidden files.
        spec.push((String::new(), None));
        spec.push((
            if self.show_hidden {
                t("files-menu-hide-hidden")
            } else {
                t("files-menu-show-hidden")
            },
            Some(FileCommand::ToggleHidden),
        ));

        // Sort section — the only place the sort *key* can be changed in grid
        // mode (which has no clickable column header) and the only way to
        // reach "Sort by Type" at all.
        spec.push((String::new(), None));
        spec.push((
            t("files-menu-sort-name"),
            Some(FileCommand::SetSort(SortBy::Name)),
        ));
        spec.push((
            t("files-menu-sort-size"),
            Some(FileCommand::SetSort(SortBy::Size)),
        ));
        spec.push((
            t("files-menu-sort-type"),
            Some(FileCommand::SetSort(SortBy::Type)),
        ));
        spec.push((
            t("files-menu-sort-date"),
            Some(FileCommand::SetSort(SortBy::Modified)),
        ));
        spec.push((String::new(), None));
        spec.push((
            t("files-menu-open-terminal"),
            Some(FileCommand::OpenTerminal),
        ));
        spec
    }

    /// Whether the current directory is the Trash, which swaps the delete
    /// affordances for Restore / Empty / permanent-delete.
    fn is_trash_view(&self) -> bool {
        fileops::is_trash_dir(&self.current_path)
    }

    /// The path of the single selected entry when it is a directory (drives the
    /// "Add to Favourites" action).
    fn single_selected_dir(&self) -> Option<PathBuf> {
        let disp = self.displayed_entries();
        let sel = self.selection.selected();
        if sel.len() != 1 {
            return None;
        }
        let idx = *sel.first()?;
        disp.get(idx).filter(|e| e.is_dir).map(|e| e.path.clone())
    }

    /// Whether the current directory is a favourite that may be removed (i.e.
    /// present in the list and not the pinned Home entry).
    fn current_is_removable_favourite(&self) -> bool {
        self.favs
            .borrow()
            .favorites
            .iter()
            .any(|f| !f.is_home && f.path == self.current_path)
    }

    fn open_context_menu(&mut self, at: Point) {
        self.submenu.close();
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
                menu.push(MenuItem::action(label, || {}));
                cmds.push(cmd);
            }
        }
        menu.open_at([at.x as f32, at.y as f32]);
        self.context_menu = menu;
        self.menu_commands = cmds;
        layout_menu_widget(&mut self.context_menu);
    }

    /// Open the "New File" flyout listing one "New <name>" entry per template.
    fn open_templates_submenu(&mut self, at: Point) {
        let tpls = self.available_templates();
        // The catalogue can change after its parent menu was opened.  Do not
        // leave a focusable, visually empty flyout behind when its last
        // template vanishes (and keep headless action-state shots truthful).
        if tpls.is_empty() {
            self.submenu.close();
            self.submenu_commands.clear();
            return;
        }
        let mut menu = self.new_overlay_menu();
        let mut cmds = Vec::with_capacity(tpls.len());
        for t in tpls {
            menu.push(MenuItem::action(templates::menu_label(&t), || {}));
            cmds.push(Some(FileCommand::NewFromTemplate(t)));
        }
        menu.open_at([at.x as f32, at.y as f32]);
        self.submenu = menu;
        self.submenu_commands = cmds;
        layout_menu_widget(&mut self.submenu);
    }

    /// Open the "Run shortcut" flyout: one row per shortcut that runs on the
    /// selected entry.
    fn open_shortcuts_submenu(&mut self, at: Point) {
        let identities = self.selected_entry_identities();
        let offered: Vec<(String, String)> = self
            .shortcut_menu
            .offered_on(&identities)
            .into_iter()
            .map(|offer| (offer.label().to_string(), offer.id().to_string()))
            .collect();
        if offered.is_empty() {
            self.submenu.close();
            self.submenu_commands.clear();
            return;
        }
        let mut menu = self.new_overlay_menu();
        let mut cmds = Vec::with_capacity(offered.len());
        for (label, shortcut) in offered {
            menu.push(MenuItem::action(label, || {}));
            cmds.push(Some(FileCommand::RunShortcut { shortcut }));
        }
        menu.open_at([at.x as f32, at.y as f32]);
        self.submenu = menu;
        self.submenu_commands = cmds;
        layout_menu_widget(&mut self.submenu);
    }

    /// Say on the status line what became of a shortcut run from the menu,
    /// when `event` says. Returns whether the status line changed.
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

    fn open_nearby_device_picker(&mut self, targets: &[ui::collaboration::NearbyTransferTarget]) {
        let online = targets
            .iter()
            .filter(|target| target.online)
            .collect::<Vec<_>>();
        if online.is_empty() {
            self.status = Some(t("files-nearby-no-devices"));
            return;
        }
        self.submenu.close();
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
        self.context_menu = menu;
        self.menu_commands = commands;
        layout_menu_widget(&mut self.context_menu);
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
        self.submenu.close();
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
        self.context_menu = menu;
        self.menu_commands = commands;
        layout_menu_widget(&mut self.context_menu);
        self.status = Some(t("files-nearby-choose-destination"));
    }

    /// Open `self.context_menu` from a `(label, command)` spec, right-aligned so
    /// its right edge sits at `anchor` (a toolbar control's bottom-right). Used
    /// by the labelled View / Sort menus, whose triggers hug the window's right
    /// edge where a left-anchored menu would spill off-screen. A `None` command
    /// with an empty label is a separator.
    fn open_menu_right_aligned(
        &mut self,
        spec: Vec<(String, Option<FileCommand>, Option<bool>)>,
        anchor: Point,
        flip_y: f64,
    ) {
        self.submenu.close();
        let mut menu = self.new_overlay_menu();
        let mut cmds = Vec::with_capacity(spec.len());
        for (label, cmd, checked) in spec {
            if cmd.is_none() && label.is_empty() {
                menu.push(MenuItem::separator());
                cmds.push(None);
            } else {
                menu.push(match checked {
                    Some(checked) => MenuItem::checked_action(label, checked, || {}),
                    None => MenuItem::action(label, || {}),
                });
                cmds.push(cmd);
            }
        }
        // Right-align: lay out once to learn the menu size, then re-anchor so
        // the right edge lands on `anchor.x`. A tall menu flips above the
        // trigger rather than slipping below the window; both axes clamp as a
        // final guard for unusually tiny viewports.
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
        self.context_menu = menu;
        self.menu_commands = cmds;
        layout_menu_widget(&mut self.context_menu);
    }

    /// Construct a menu constrained to Files' live window rect. Overlay menus
    /// are outside the app's normal layout tree, so this is what keeps a
    /// 200%-text menu fully reachable rather than letting it grow off a right
    /// or bottom edge after its trigger was measured at normal text size.
    fn new_overlay_menu(&self) -> ContextMenu {
        let mut menu = ContextMenu::new();
        menu.set_viewport(Some(self.rect));
        menu
    }

    /// The labelled "View options" overflow menu (the ⋯ button): new folder /
    /// file, a clearly-labelled show-hidden toggle with a check, and a terminal.
    /// Everything here is spelled out in words — no mystery symbolic controls.
    fn open_view_menu(&mut self) {
        let anchor = self.toolbar.view_menu_anchor();
        let flip_y = self
            .toolbar
            .view_menu_rect()
            .map_or(anchor.y, |rect| rect.y0 - 2.0);
        let mut spec: Vec<(String, Option<FileCommand>, Option<bool>)> = Vec::new();
        // At phone width the path remains useful while direct view / sort
        // controls move here. The same labelled actions remain keyboard and
        // accessibility reachable without squeezing desktop chrome into one row.
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
            spec.push((t("files-sort-by"), Some(FileCommand::OpenSortMenu), None));
            spec.push((String::new(), None, None));
        }
        if crate::archive::location(&self.current_path).is_some() {
            spec.push((
                t("files-archive-extract"),
                Some(FileCommand::ExtractCurrentArchive),
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
        if self.last_trash_undo.is_some() {
            spec.push((String::new(), None, None));
            spec.push((
                t("files-undo-move-trash"),
                Some(FileCommand::UndoLastTrash),
                None,
            ));
        }
        spec.push((String::new(), None, None));
        spec.push((
            t("files-menu-show-hidden"),
            Some(FileCommand::ToggleHidden),
            Some(self.show_hidden),
        ));
        if self.is_trash_view() {
            spec.push((String::new(), None, None));
            spec.push((
                t("files-menu-empty-trash"),
                Some(FileCommand::EmptyTrash),
                None,
            ));
        }
        spec.push((String::new(), None, None));
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

    /// The labelled "Sort by" menu (the grid-mode Sort button): the sort key,
    /// then the direction, each with a check on the active choice. This is the
    /// grid's equivalent of clicking a list column header.
    fn open_sort_menu(&mut self) {
        let anchor = self.toolbar.sort_menu_anchor();
        let flip_y = self
            .toolbar
            .sort_menu_rect()
            .map_or(anchor.y, |rect| rect.y0 - 2.0);
        let mut spec: Vec<(String, Option<FileCommand>, Option<bool>)> = Vec::new();
        for (label, by) in [
            (t("files-sort-name"), SortBy::Name),
            (t("files-sort-size"), SortBy::Size),
            (t("files-sort-type"), SortBy::Type),
            (t("files-sort-date"), SortBy::Modified),
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
        self.open_menu_right_aligned(spec, anchor, flip_y);
    }

    /// The right-click menu for a sidebar favourite `path`: open it, plus the
    /// standard "Remove from Favourites" (suppressed for the pinned Home entry,
    /// which cannot be removed). Anchored at the cursor like any context menu.
    fn open_favorite_menu(&mut self, path: PathBuf, at: Point) {
        let is_home = self
            .favs
            .borrow()
            .favorites
            .iter()
            .any(|f| f.is_home && f.path == path);
        let mut spec: Vec<(String, Option<FileCommand>)> = Vec::new();
        spec.push((
            t("files-menu-open"),
            Some(FileCommand::Navigate(path.clone())),
        ));
        if !is_home {
            spec.push((String::new(), None));
            spec.push((
                t("files-menu-remove-favourite"),
                Some(FileCommand::RemoveFavorite(path)),
            ));
        }
        // A favourite menu opens at the cursor (left-anchored is fine — the
        // sidebar sits at the window's left edge).
        self.submenu.close();
        let mut menu = self.new_overlay_menu();
        let mut cmds = Vec::with_capacity(spec.len());
        for (label, cmd) in spec {
            if cmd.is_none() && label.is_empty() {
                menu.push(MenuItem::separator());
                cmds.push(None);
            } else {
                menu.push(MenuItem::action(label, || {}));
                cmds.push(cmd);
            }
        }
        menu.open_at([at.x as f32, at.y as f32]);
        self.context_menu = menu;
        self.menu_commands = cmds;
        layout_menu_widget(&mut self.context_menu);
    }

    /// If either menu is open, route a left-press to it. Returns true when the
    /// press was consumed (item fired, submenu opened, or a menu dismissed).
    fn route_menu_click(&mut self, p: Point) -> bool {
        // The submenu takes priority while it is up.
        if self.submenu.is_open() {
            if let Some(idx) = self.submenu.item_at([p.x, p.y]) {
                let cmd = self.submenu_commands.get(idx).cloned().flatten();
                self.submenu.close();
                self.context_menu.close();
                if let Some(cmd) = cmd {
                    self.apply_command(cmd);
                }
                return true;
            }
            // A click outside the submenu closes both menus.
            self.submenu.close();
            self.context_menu.close();
            return true;
        }
        if !self.context_menu.is_open() {
            return false;
        }
        if let Some(idx) = self.context_menu.item_at([p.x, p.y]) {
            let cmd = self.menu_commands.get(idx).cloned().flatten();
            if let Some(FileCommand::OpenTemplatesMenu | FileCommand::OpenShortcutsMenu) = cmd {
                // Open the flyout to the right of the clicked row; keep the
                // main menu open behind it.
                let anchor = Point::new(self.context_menu.get_rect().x1 - 6.0, p.y - 10.0);
                if cmd == Some(FileCommand::OpenShortcutsMenu) {
                    self.open_shortcuts_submenu(anchor);
                } else {
                    self.open_templates_submenu(anchor);
                }
                return true;
            }
            if let Some(FileCommand::OpenWithMenu) = cmd {
                let anchor = Point::new(self.context_menu.get_rect().x1 - 6.0, p.y - 10.0);
                self.open_open_with_submenu(anchor);
                return true;
            }
            self.context_menu.close();
            if let Some(cmd) = cmd {
                self.apply_command(cmd);
            }
            return true;
        }
        // Click outside the open menu dismisses it.
        self.context_menu.close();
        true
    }

    /// Route keyboard input through a standalone context menu and translate
    /// its selected row back into the app command bus. The menu is painted as
    /// an overlay rather than installed in the taffy tree, so normal child
    /// event propagation cannot reach it.
    fn route_menu_keyboard(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let submenu = self.submenu.is_open();
        let mut menu_handled = false;
        let selected = {
            let menu = if submenu {
                &mut self.submenu
            } else {
                &mut self.context_menu
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
            self.submenu_commands.get(index).cloned().flatten()
        } else {
            self.menu_commands.get(index).cloned().flatten()
        };
        if let Some(FileCommand::OpenTemplatesMenu) = command {
            let rect = self.context_menu.get_rect();
            self.open_templates_submenu(Point::new(rect.x1 - 6.0, rect.y0 + 28.0));
        } else if let Some(FileCommand::OpenShortcutsMenu) = command {
            let rect = self.context_menu.get_rect();
            self.open_shortcuts_submenu(Point::new(rect.x1 - 6.0, rect.y0 + 28.0));
        } else if let Some(FileCommand::OpenWithMenu) = command {
            let rect = self.context_menu.get_rect();
            let row = self
                .context_menu
                .item_rect(index)
                .map_or(rect.y0 + 28.0, |row| row.y0);
            self.open_open_with_submenu(Point::new(rect.x1 - 6.0, row));
        } else {
            self.submenu.close();
            self.context_menu.close();
            if let Some(command) = command {
                self.apply_command(command);
            }
        }
        true
    }

    /// Put a keyboard-opened context menu beside the selected row. Falling
    /// back to the content area's upper-left keeps the action available when
    /// the row is virtualized or the composition has not painted yet.
    fn keyboard_context_anchor(&self) -> Point {
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
            .and_then(|name| find_named_rect(&self.nav, name))
        {
            return Point::new(rect.x0 + 24.0, rect.y0 + 24.0);
        }
        Point::new(self.rect.x0 + 24.0, self.rect.y0 + 72.0)
    }

    /// Drain and apply all queued commands. Returns whether any command
    /// changed the structure.
    #[allow(
        clippy::needless_collect,
        reason = "collecting drops the RefCell borrow before the loop; apply_command can itself queue follow-up commands into the same self.cmds sink, and iterating the drain directly would hold the borrow_mut across that and panic"
    )]
    fn drain_commands(&mut self) -> bool {
        let queued: Vec<FileCommand> = self.cmds.borrow_mut().drain(..).collect();
        let mut dirty = false;
        for cmd in queued {
            dirty |= self.apply_command(cmd);
        }
        dirty
    }

    /// Admit a received bearer link through the recipient service. The call
    /// runs on the app runtime; when it finishes, Files opens the received
    /// mount or reports why it could not.
    pub fn admit_received_share(&mut self, uri: String) {
        if !self
            .received_share
            .spawn(crate::received_shares::admit_link(uri))
        {
            self.set_external_status(t("files-received-unavailable"));
        }
    }

    fn poll_received_share(&mut self) -> bool {
        let Some(result) = self.received_share.try_take() else {
            return false;
        };
        match result {
            Ok(status) => {
                if let Some(mount) = status.mount {
                    self.navigate(PathBuf::from(mount.path));
                    self.set_external_status(if mount.running {
                        t("files-received-file")
                    } else {
                        t("files-received-mount-starting")
                    });
                } else {
                    self.set_external_status(t("files-received-admitted"));
                }
            }
            Err(error) => self.set_external_status(error),
        }
        true
    }

    /// Run a share-service call on the app runtime; its outcome wakes the
    /// event loop and is applied by [`Self::poll_share_result`].
    fn spawn_share(
        &mut self,
        call: impl std::future::Future<Output = Result<crate::sharing::ShareActionResult, String>>
        + Send
        + 'static,
    ) {
        if !self.share_result.spawn(call) {
            // The window is closing; no outcome could be delivered.
            self.share_pending_path = None;
            self.status = Some(t("files-sharing-unavailable"));
        }
    }

    fn poll_share_result(&mut self) -> bool {
        let Some(result) = self.share_result.try_take() else {
            return false;
        };
        let lifecycle = self
            .resource_invocation
            .take()
            .map(|invocation| match &result {
                Ok(_) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(message) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "files.share_failed".into(),
                        message: message.clone(),
                        details: None,
                        retryable: false,
                    },
                },
            });
        let path = self.share_pending_path.take();
        match result {
            Ok(crate::sharing::ShareActionResult::LinkCreated(uri)) => {
                ui::clipboard_copy(&uri);
                self.status = Some(t("files-link-copied"));
                if let Some(path) = path {
                    let projection = crate::model::read_share(&path);
                    if let Some(entry) = self.entries.iter_mut().find(|entry| entry.path == path) {
                        entry.share = projection;
                    }
                }
            }
            Ok(crate::sharing::ShareActionResult::NamedCreated(metadata)) => {
                let display = self
                    .trusted_humans
                    .display_name_or_fingerprint(&metadata.person);
                self.status = Some(t_args(
                    named_delivery_status_key(metadata.delivery),
                    &[arg("person", display)],
                ));
                if let Some(path) = path {
                    let projection = crate::model::read_share(&path);
                    if let Some(entry) = self.entries.iter_mut().find(|entry| entry.path == path) {
                        entry.share = projection;
                    }
                }
            }
            Ok(crate::sharing::ShareActionResult::Revoked { revoked, .. }) => {
                self.status = Some(if revoked {
                    t("files-access-revoked")
                } else {
                    t("files-share-inactive")
                });
                if let Some(path) = path {
                    let projection = crate::model::read_share(&path);
                    if let Some(entry) = self.entries.iter_mut().find(|entry| entry.path == path) {
                        entry.share = projection;
                    }
                }
            }
            Err(error) => self.status = Some(error),
        }
        self.structure_dirty = true;
        if lifecycle.is_some() {
            self.resource_result = lifecycle;
        }
        true
    }

    fn request_merge_preflight(
        &mut self,
        path: PathBuf,
        conflict_id: uuid::Uuid,
        version_id: collab_types::FileVersionId,
    ) {
        self.merge_preflight_pending = Some((conflict_id, version_id));
        let result = Arc::clone(&self.merge_preflight_result);
        std::thread::spawn(move || {
            let outcome = crate::model::read_merge_preflight(&path, conflict_id)
                .ok_or_else(|| t("files-merge-review-unavailable"));
            *result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some((conflict_id, version_id, outcome));
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
        let lifecycle = self
            .resource_invocation
            .take()
            .map(|invocation| match &result {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(message) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "files.history_failed".into(),
                        message: message.clone(),
                        details: None,
                        retryable: false,
                    },
                },
            });
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
                self.status = Some(error);
            }
        }
        self.close_collab_panel();
        self.structure_dirty = true;
        if lifecycle.is_some() {
            self.resource_result = lifecycle;
        }
        true
    }

    /// Forward an event to the three body children in visual order — chrome
    /// above, the responsive `nav` body, chrome below — stopping at the first
    /// that consumes it. `node_id`'s taffy children are `[above, nav, below]`
    /// (see `build_layout`).
    fn forward_to_body(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let Ok(children) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        let (Some(&above_n), Some(&nav_n)) = (children.first(), children.get(1)) else {
            return;
        };
        if !*ctx.handled {
            self.toolbar
                .handle_event(event, &mut ctx.reborrow(ctx.path, above_n));
        }
        if !*ctx.handled {
            self.nav
                .handle_event(event, &mut ctx.reborrow(ctx.path, nav_n));
        }
    }

    /// App-level keyboard navigation, applied when a key isn't consumed by
    /// a focused child (search field, etc.). Returns whether it acted.
    #[allow(
        clippy::too_many_lines,
        reason = "the complete keyboard command map stays auditable in one match so every shortcut has a single routing point"
    )]
    fn handle_app_key(
        &mut self,
        ke: &winit::event::KeyEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if ke.state != winit::event::ElementState::Pressed {
            return false;
        }
        // While renaming, the rename field owns the keyboard (Enter/Escape are
        // handled upstream in `handle_event`); don't let list nav / Delete /
        // clipboard shortcuts fire underneath the editor.
        if self.renaming.is_some() {
            return false;
        }
        let shift = modifiers.state().shift_key();
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        let len = self.displayed_entries().len();
        match &ke.logical_key {
            // Location is a first-class, keyboard-accessible control. Keep
            // this at the application layer so it works when focus is in the
            // file view, places list, or breadcrumb; a path editor itself
            // consumes keys before this method is reached.
            Key::Character(s) if (s == "l" || s == "L") && ctrl => {
                self.toolbar.begin_path_edit();
                true
            }
            // Help is intentionally a regular view command, not an invisible
            // one-off keypath: the same reference is reachable from View
            // options and the current-folder action resource.
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
                self.open_context_menu(self.keyboard_context_anchor());
                true
            }
            // F2 renames the current selection in place.
            Key::Named(NamedKey::F2) => {
                self.begin_rename();
                true
            }
            Key::Named(NamedKey::ArrowDown) if len > 0 => {
                let next = self.selection.primary().map_or(0, |i| (i + 1).min(len - 1));
                self.selection.move_to(next, shift);
                true
            }
            Key::Named(NamedKey::ArrowUp) if len > 0 => {
                let next = self.selection.primary().map_or(0, |i| i.saturating_sub(1));
                self.selection.move_to(next, shift);
                true
            }
            Key::Named(NamedKey::Home) if len > 0 => {
                self.selection.move_to(0, shift);
                true
            }
            Key::Named(NamedKey::End) if len > 0 => {
                self.selection.move_to(len - 1, shift);
                true
            }
            // Escape clears the selection (when there is one); otherwise it is
            // left for the host so it never swallows a no-op.
            Key::Named(NamedKey::Escape) if !self.selection.is_empty() => {
                self.selection.clear();
                self.last_click = None;
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(p) = self.primary_path() {
                    self.open_path(p);
                }
                true
            }
            Key::Named(NamedKey::Backspace) => {
                self.go_up();
                true
            }
            Key::Named(NamedKey::Delete) => {
                self.apply_command(FileCommand::Delete);
                true
            }
            Key::Character(s) if (s == "a" || s == "A") && ctrl => {
                self.selection.select_all(len);
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
            Key::Character(s) if (s == "z" || s == "Z") && ctrl => {
                self.apply_command(FileCommand::UndoLastTrash);
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
            _ => false,
        }
    }

    /// A live drag owns the pointer: the ghost follows the cursor, the
    /// hovered folder is highlighted, and the release resolves the drop —
    /// moving the carried row(s) into a folder (or the parent), or pinning a
    /// folder to the sidebar. Returns whether the event was consumed (the
    /// caller should stop routing it further).
    fn handle_live_drag_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        if self.pin_drag.is_none() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                // Once the host has carried the drag to another
                // application, no folder here is a target any more.
                self.drag_hover = if crate::external_drag::is_handed_off() {
                    None
                } else {
                    self.folder_target_at(self.last_cursor)
                };
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
                    // The drop landed in another application; the host
                    // delivered it. Nothing moves here.
                    self.cancel_drag();
                } else {
                    self.finish_drag();
                }
                *ctx.handled = true;
                self.structure_dirty = true;
                true
            }
            _ => false,
        }
    }

    /// Route an open compact Places drawer as a modal surface. The uncovered
    /// strip is deliberately a dismiss backdrop, never a transparent route to
    /// the file view: right-clicks, wheel input, and selection gestures must
    /// not reach the list behind the scrim.
    fn handle_places_drawer_modal_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.nav.mode() != ShellMode::Narrow || !self.nav.is_sidebar_open() {
            return false;
        }
        let Ok(children) = ctx.taffy.children(ctx.node_id) else {
            return false;
        };
        let Some(&nav_node) = children.get(1) else {
            return false;
        };
        let path_before = self.current_path.clone();
        self.nav
            .handle_event(event, &mut ctx.reborrow(ctx.path, nav_node));
        if self.drain_commands() {
            self.structure_dirty = true;
        }
        if self.current_path != path_before {
            self.nav.close_sidebar();
        }
        // NavShell handles a left scrim press by closing; other events such as
        // a right-click over the scrim are still swallowed here so they cannot
        // construct a file-view context menu through the modal backdrop.
        *ctx.handled = true;
        true
    }

    /// The open context menu (or its flyout) gets first dibs on clicks +
    /// Escape. Returns whether the event was consumed.
    fn handle_open_menu_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        if !(self.context_menu.is_open() || self.submenu.is_open()) {
            return false;
        }
        match event {
            WindowEvent::KeyboardInput { .. } => {
                if self.route_menu_keyboard(event, ctx) {
                    *ctx.handled = true;
                    self.structure_dirty = true;
                    let _ = self.drain_commands();
                    return true;
                }
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.route_menu_click(self.last_cursor) {
                    *ctx.handled = true;
                    self.structure_dirty = true;
                    let _ = self.drain_commands();
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Right-click opens the context menu. First deliver the press to the
    /// body so the content pane selects the row under the cursor (it pushes
    /// `ContextSelect`); draining that before building the menu is what
    /// makes the menu's actions target what was actually clicked rather
    /// than a stale selection from an earlier click. Returns whether the
    /// event was consumed.
    fn handle_right_click_open_menu(
        &mut self,
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
        self.forward_to_body(
            event,
            &mut ui::widget::EventCtx {
                handled: &mut child_handled,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        self.drain_commands();
        // A right-press over a sidebar favourite pushes `OpenFavoriteMenu`,
        // which the drain records here; open the favourite's own menu at the
        // cursor rather than the file-view context menu.
        if let Some(fav) = self.pending_fav_menu.take() {
            self.open_favorite_menu(fav, self.last_cursor);
        } else {
            self.open_context_menu(self.last_cursor);
        }
        *ctx.handled = true;
        self.structure_dirty = true;
        true
    }

    /// Ctrl + mouse-wheel zooms the icon/row size (before the wheel reaches
    /// the Scrollable, which would otherwise consume it for scrolling).
    /// Returns whether the event was consumed.
    fn handle_zoom_wheel(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let winit::event::WindowEvent::MouseWheel { delta, .. } = event else {
            return false;
        };
        if !(ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key()) {
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

    /// While an inline rename is active the in-place editor (drawn over the
    /// row, not in a bottom bar) owns input: Enter commits, Escape cancels,
    /// every other keystroke edits the field, and a press outside the field
    /// commits. Handled before the event reaches app-level nav / the file
    /// view so typing never leaks into list navigation or Enter-opens a
    /// row. Returns whether the event was consumed.
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
                            // A local field owns this transient proposal. Read
                            // it directly so Enter commits the final keystroke
                            // without serialising a UI-only value into CRDT.
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
                // Typing / editing keys go to the field (it splices the CRDT).
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
                    // Reposition the caret within the field.
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

    /// Deliver to the body first (toolbar/breadcrumb, places list, file
    /// view), stopping at the first that consumes it; then app-level
    /// keyboard nav, queued commands, and the narrow-drawer dismissal that
    /// follows a place navigation.
    fn finish_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let path_before = self.current_path.clone();
        self.forward_to_body(event, ctx);
        if !*ctx.handled
            && let winit::event::WindowEvent::KeyboardInput { event: ke, .. } = event
            && self.handle_app_key(ke, *ctx.modifiers)
        {
            *ctx.handled = true;
            self.structure_dirty = true;
        }
        if self.drain_commands() {
            self.structure_dirty = true;
        }
        if self.current_path != path_before && self.nav.mode() == ShellMode::Narrow {
            self.nav.close_sidebar();
        }
    }
}

impl FileExplorerWidget {
    /// The small, stable root exposed to the broker directory. A folder listing
    /// may be huge, so entry rows stay bounded. Session favorites come first.
    /// A place [`tilo_resource::publish`] already listed stays when this session
    /// does not already have that place.
    fn published_folder(&self, doc: &automerge::AutoCommit) -> Resource {
        let live = self.action_directory_folder_root();
        let state = <crate::resource::FileExplorer as ui::tilo_resource::Resource>::read(doc)
            .unwrap_or_default();
        let mut folder = ui::tilo_resource::publish::<crate::resource::FileExplorer>(
            &crate::search_provider::folder_resource_id(&self.current_path),
            &self.current_path.display().to_string(),
            &state,
        );
        for action in live.actions {
            if let Some(existing) = folder
                .actions
                .iter_mut()
                .find(|existing| existing.id == action.id)
            {
                *existing = action;
            } else {
                folder.actions.push(action);
            }
        }
        let published = std::mem::take(&mut folder.children);
        let mut templates = Vec::new();
        let mut published_places = Vec::new();
        for child in published {
            if child.kind == "files.template" {
                templates.push(child);
            } else if child.kind == "files.place" {
                published_places.push(child);
            }
        }
        folder.children = live.children;
        if self.error.is_some() {
            folder
                .children
                .extend(ui::resources::PublishesResources::resources(
                    &self.state_retry,
                    doc,
                ));
        }
        if !self.is_trash_view() {
            let can_create = self.current_path.is_dir();
            for template in &mut templates {
                if let Some(action) = template.actions.first_mut() {
                    action.contract.availability = if can_create {
                        Availability::Available
                    } else {
                        Availability::Unavailable {
                            code: "location_unavailable".into(),
                            reason: "The current location cannot contain a new file".into(),
                        }
                    };
                }
            }
            folder.children.extend(templates);
        }
        folder
            .children
            .extend(published_place_resources(&self.favs.borrow().favorites));
        let listed: std::collections::HashSet<String> = folder
            .children
            .iter()
            .map(|child| child.id.as_str().to_string())
            .collect();
        for place in published_places {
            if !listed.contains(place.id.as_str()) {
                folder.children.push(place);
            }
        }
        folder.children.extend(self.live_remote_account_resources());
        folder.children.extend(self.live_entry_resources());
        folder
    }

    fn action_directory_folder_root(&self) -> Resource {
        let mut folder = self.folder_resource_base();
        Self::append_navigation_actions(&mut folder);
        folder.actions.push(Self::go_to_action());
        folder.actions.push(Self::open_folder_action());
        folder.actions.push(self.move_action());
        self.append_favorite_action(&mut folder);
        self.append_creation_actions(&mut folder);
        self.append_nearby_receiver_resources(&mut folder);
        folder
    }

    /// The folder as a caller browsing into it resolves it: the root plus
    /// its first entries, each with its complete actions.
    #[allow(dead_code, reason = "framework migration test helper")]
    fn folder_with_entries(&self) -> Resource {
        let mut folder = self.action_directory_folder_root();
        folder.children.extend(
            self.entries
                .iter()
                .take(MAX_EAGER_ENTRY_RESOURCES)
                .map(|entry| self.entry_resource(entry)),
        );
        folder
    }

    fn go_to_action() -> ActionSpec {
        Self::show_folder_action("go-to", "Go to folder…")
    }

    /// `open` with a path, the convention an agent opens an app at a file
    /// or folder by: Files shows that folder, as `go-to` does.
    fn open_folder_action() -> ActionSpec {
        Self::show_folder_action("open", "Open folder")
    }

    #[allow(
        clippy::expect_used,
        reason = "a path string payload is always serializable"
    )]
    fn show_folder_action(id: &'static str, label: &'static str) -> ActionSpec {
        ActionSpec::typed_operation(
            id,
            label,
            "absolute folder path, e.g. /data/in",
            OperationKind::View,
            "files.go-to",
            vec![ActionParam::required(
                "path",
                ParamType::Text,
                "Absolute path of the folder to show",
            )],
            |args: &ActionArgs| {
                EncodedAction::encode_operation("files.go-to", &args.text("path").unwrap_or(""))
                    .expect("folder path is serializable")
            },
        )
        .with_description(
            "Show the folder at this absolute path; answers with its resource (this folder's id changes) and entries",
        )
        .with_contract(ActionContract {
            result: ResultSpec::Json {
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["resource", "path", "entries"],
                    "properties": {
                        "resource": {"type": "string"},
                        "path": {"type": "string"},
                        "entries": {"type": "array", "items": {"type": "string"}},
                        "more": {"type": "integer"}
                    }
                }),
            },
            ..ActionContract::view()
        })
    }

    /// What showing a folder answers: its resource id, its path and its
    /// first entries (a folder's name ends in `/`), so a caller acts on
    /// the folder without querying it again.
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
            "resource": crate::search_provider::folder_resource_id(&self.current_path),
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

    /// `move`: put several of this folder's entries into another folder in
    /// one step, creating that folder unless told not to. Answers with what
    /// moved where.
    #[allow(
        clippy::expect_used,
        reason = "a move request of strings and a flag is serializable"
    )]
    fn move_action(&self) -> ActionSpec {
        ActionSpec::typed_operation(
            "move",
            "Move entries to folder",
            "entry names, absolute folder path, e.g. /data/out/images",
            OperationKind::Effect,
            "files.move",
            vec![
                ActionParam::required(
                    "entries",
                    ParamType::Array,
                    "Names of entries in this folder, e.g. [\"a.png\",\"b.jpg\"]",
                )
                .with_schema(serde_json::json!({
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_MOVE_ENTRIES,
                    "items": {"type": "string", "minLength": 1, "maxLength": 4096}
                })),
                ActionParam::required(
                    "folder",
                    ParamType::Text,
                    "Absolute path of the destination folder; names are kept",
                ),
                ActionParam::optional(
                    "create",
                    ParamType::Boolean,
                    "Create the folder (and its parents) when missing; default true",
                ),
            ],
            |args: &ActionArgs| {
                let request = MoveRequest {
                    entries: args
                        .array("entries")
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|entry| entry.as_str().map(str::to_owned))
                        .collect(),
                    folder: args.text("folder").unwrap_or_default().to_owned(),
                    create: args.boolean("create").unwrap_or(true),
                };
                EncodedAction::encode_operation("files.move", &request)
                    .expect("move request is serializable")
            },
        )
        .with_description(
            "Move several entries of this folder into another folder in one call, creating it when missing",
        )
        .with_contract(ActionContract {
            availability: if self.current_path.is_dir() {
                Availability::Available
            } else {
                Availability::Unavailable {
                    code: "location_unavailable".into(),
                    reason: "The current location lists no entries to move".into(),
                }
            },
            permission: PermissionRequirement::Write,
            retry: RetryPolicy::AtMostOnce,
            reversible: false,
            result: ResultSpec::Json {
                schema: serde_json::json!({"type": "string"}),
            },
            ..ActionContract::default()
        })
    }

    /// Carry out a [`MoveRequest`]: every entry is found before anything
    /// moves, so a misspelt name moves nothing.
    fn move_entries(&mut self, request: &MoveRequest) -> Result<String, String> {
        let folder = PathBuf::from(&request.folder);
        if !folder.is_absolute() {
            return Err(format!(
                "{} is not an absolute path; pass the full destination, e.g. {}",
                folder.display(),
                self.current_path.join(&folder).display()
            ));
        }
        let mut sources = Vec::with_capacity(request.entries.len());
        let mut unknown = Vec::new();
        for name in &request.entries {
            let wanted = Path::new(name);
            match self.entries.iter().find(|entry| {
                entry.path == wanted || (entry.name == *name && !wanted.is_absolute())
            }) {
                Some(entry) => sources.push((entry.name.clone(), entry.path.clone())),
                None => unknown.push(name.as_str()),
            }
        }
        if !unknown.is_empty() {
            let names: Vec<&str> = self
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect();
            return Err(format!(
                "not in {}: {}; nothing moved. Its entries: {}",
                self.current_path.display(),
                unknown.join(", "),
                names.join(", ")
            ));
        }
        let created = !folder.exists();
        if created {
            if !request.create {
                return Err(format!(
                    "{} does not exist; nothing moved. Call move again with create:true to make it",
                    folder.display()
                ));
            }
            std::fs::create_dir_all(&folder)
                .map_err(|error| format!("cannot create {}: {error}", folder.display()))?;
        } else if !folder.is_dir() {
            return Err(format!(
                "{} is a file, not a folder; nothing moved",
                folder.display()
            ));
        }
        let mut moved = Vec::new();
        let mut failed = Vec::new();
        for (name, source) in sources {
            match fileops::move_into(&source, &folder) {
                Ok(_) => moved.push(name),
                Err(error) => failed.push(format!("{name}: {error}")),
            }
        }
        self.refresh();
        let into = format!(
            "{}{}",
            folder.display(),
            if created { " (created)" } else { "" }
        );
        let summary = format!("moved {} into {into}: {}", moved.len(), moved.join(", "));
        if failed.is_empty() {
            Ok(summary)
        } else {
            Err(format!("{summary}; not moved: {}", failed.join("; ")))
        }
    }

    /// `move-to` and `copy-to` for one entry: put it into another folder,
    /// keeping its name.
    #[allow(
        clippy::expect_used,
        reason = "the verb, resource id and folder path payload is serializable"
    )]
    fn transfer_actions(resource_id: &str) -> [ActionSpec; 2] {
        ["move-to", "copy-to"].map(|verb| {
            let resource_id = resource_id.to_owned();
            ActionSpec::typed_operation(
                verb,
                if verb == "move-to" {
                    "Move to folder…"
                } else {
                    "Copy to folder…"
                },
                "absolute folder path, e.g. /data/out",
                OperationKind::Effect,
                "files.transfer",
                vec![ActionParam::required(
                    "folder",
                    ParamType::Text,
                    "Absolute path of an existing folder; the name is kept",
                )],
                move |args: &ActionArgs| {
                    EncodedAction::encode_operation(
                        "files.transfer",
                        &[
                            verb,
                            resource_id.as_str(),
                            args.text("folder").unwrap_or(""),
                        ],
                    )
                    .expect("transfer payload is serializable")
                },
            )
            .with_contract(ActionContract {
                permission: PermissionRequirement::Write,
                retry: RetryPolicy::AtMostOnce,
                reversible: false,
                ..ActionContract::default()
            })
        })
    }

    fn append_nearby_receiver_resources(&self, folder: &mut Resource) {
        for transfer in self.nearby_send.transfers() {
            if let Some(resource) = self.nearby_receiver_resource(transfer) {
                folder.children.push(resource);
            }
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "nearby payloads contain only fixed tuples of serializable opaque handles"
    )]
    fn nearby_receiver_resource(
        &self,
        transfer: &ui::collaboration::NearbyTransferStatus,
    ) -> Option<Resource> {
        use ui::collaboration::NearbyTransferPhase;

        let mut resource = Resource::new(
            format!(
                "nearby-transfer:{}",
                nearby_action_locator("transfer", transfer.handle.as_bytes())
            ),
            "files.nearby-transfer",
            transfer.name.clone(),
        );
        self.append_nearby_decision_actions(&mut resource, transfer);
        if transfer.phase == NearbyTransferPhase::Failed {
            resource.actions.push(
                ActionSpec::operation(
                    "retry",
                    t("files-nearby-retry"),
                    OperationKind::Effect,
                    "files.nearby-retry",
                    &transfer.handle,
                )
                .expect("nearby retry payload is serializable")
                .with_contract(ActionContract {
                    permission: PermissionRequirement::Read,
                    retry: RetryPolicy::AtMostOnce,
                    ..ActionContract::default()
                }),
            );
        }
        if matches!(
            transfer.phase,
            NearbyTransferPhase::Queued | NearbyTransferPhase::Transferring
        ) {
            resource.actions.push(
                ActionSpec::operation(
                    "cancel",
                    t("files-nearby-cancelled"),
                    OperationKind::Effect,
                    "files.nearby-cancel",
                    &transfer.handle,
                )
                .expect("nearby cancel payload is serializable")
                .with_contract(ActionContract {
                    permission: PermissionRequirement::Write,
                    retry: RetryPolicy::AtMostOnce,
                    ..ActionContract::default()
                }),
            );
        }
        (!resource.actions.is_empty()).then_some(resource)
    }

    #[allow(
        clippy::expect_used,
        reason = "nearby payloads contain only fixed tuples of serializable opaque handles"
    )]
    fn append_nearby_decision_actions(
        &self,
        resource: &mut Resource,
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
            resource.actions.push(
                ActionSpec::operation(
                    format!(
                        "accept-{}",
                        nearby_action_locator("destination", destination.handle.as_bytes())
                    ),
                    t_args(
                        "files-nearby-save-to-destination",
                        &[arg("destination", destination.label.clone())],
                    ),
                    OperationKind::Effect,
                    "files.nearby-decide",
                    &(transfer.handle, Some(destination.handle), false),
                )
                .expect("nearby accept payload is serializable")
                .with_contract(ActionContract {
                    permission: PermissionRequirement::Write,
                    risk: RiskLevel::Privileged,
                    confirmation: ConfirmationPolicy::Confirm,
                    retry: RetryPolicy::AtMostOnce,
                    ..ActionContract::default()
                }),
            );
        }
        resource.actions.push(
            ActionSpec::operation(
                "decline",
                t("files-nearby-declined"),
                OperationKind::Effect,
                "files.nearby-decide",
                &(
                    transfer.handle,
                    None::<ui::collaboration::NearbyDestinationHandle>,
                    false,
                ),
            )
            .expect("nearby decline payload is serializable")
            .with_contract(ActionContract {
                permission: PermissionRequirement::Write,
                retry: RetryPolicy::AtMostOnce,
                ..ActionContract::default()
            }),
        );
    }

    fn folder_resource_base(&self) -> Resource {
        let mut folder = Resource::new(
            crate::search_provider::folder_resource_id(&self.current_path),
            "files.folder",
            self.current_path.display().to_string(),
        );
        if let Ok(spec) = ActionSpec::operation(
            "refresh",
            "Refresh folder",
            OperationKind::Effect,
            "files.refresh",
            &"",
        ) {
            folder.actions.push(spec.with_contract(ActionContract {
                availability: if self.current_path.exists() || self.is_recent_view() {
                    Availability::Available
                } else {
                    Availability::Unavailable {
                        code: "location_missing".into(),
                        reason: "The current folder no longer exists".into(),
                    }
                },
                permission: PermissionRequirement::Read,
                retry: RetryPolicy::Idempotent,
                ..ActionContract::default()
            }));
        }
        folder
    }

    #[allow(
        clippy::expect_used,
        reason = "these fixed command payloads are locally constructed and serializable"
    )]
    fn append_navigation_actions(folder: &mut Resource) {
        for (id, label, verb, kind) in folder_view_commands() {
            folder.actions.push(
                ActionSpec::operation(id, label, kind, "files.folder-command", &verb)
                    .expect("folder command is serializable")
                    .with_contract(if kind == OperationKind::View {
                        ActionContract::view()
                    } else {
                        ActionContract::default()
                    }),
            );
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "the selected fixed folder-command payload is serializable"
    )]
    fn append_favorite_action(&self, folder: &mut Resource) {
        let current_is_favorite = self
            .favs
            .borrow()
            .favorites
            .iter()
            .any(|favorite| favorite.path == self.current_path);
        let (id, label, verb) = if current_is_favorite {
            (
                "remove-favorite",
                "Remove from Favourites",
                "remove-favorite",
            )
        } else {
            ("add-favorite", "Add to Favourites", "add-favorite")
        };
        folder.actions.push(
            ActionSpec::operation(
                id,
                label,
                OperationKind::Effect,
                "files.folder-command",
                &verb,
            )
            .expect("favourite command is serializable")
            .with_contract(ActionContract {
                availability: if self.current_path.is_dir() {
                    Availability::Available
                } else {
                    Availability::Unavailable {
                        code: "location_unavailable".into(),
                        reason: "The current location cannot be a favourite".into(),
                    }
                },
                permission: PermissionRequirement::Write,
                retry: RetryPolicy::AtMostOnce,
                ..ActionContract::default()
            }),
        );
    }

    #[allow(
        clippy::expect_used,
        reason = "new-folder, paste, and empty-trash payloads are fixed local values"
    )]
    fn append_creation_actions(&self, folder: &mut Resource) {
        folder.actions.push(
            ActionSpec::prompted_operation(
                "new-folder",
                "New folder",
                "Folder name",
                "New Folder",
                OperationKind::Effect,
                "files.new-folder",
                |name| {
                    EncodedAction::encode_operation("files.new-folder", &name)
                        .expect("folder name is serializable")
                },
            )
            .with_contract(ActionContract {
                availability: if self.current_path.is_dir() {
                    Availability::Available
                } else {
                    Availability::Unavailable {
                        code: "location_unavailable".into(),
                        reason: "The current location cannot contain a new folder".into(),
                    }
                },
                permission: PermissionRequirement::Write,
                reversible: false,
                ..ActionContract::default()
            }),
        );
        folder.actions.push(
            ActionSpec::operation("paste", "Paste", OperationKind::Effect, "files.paste", &"")
                .expect("file operation payload is serializable")
                .with_contract(ActionContract {
                    availability: if self.clipboard.is_empty() {
                        Availability::Unavailable {
                            code: "clipboard_empty".into(),
                            reason: "Copy or cut files before pasting".into(),
                        }
                    } else {
                        Availability::Available
                    },
                    permission: PermissionRequirement::Write,
                    risk: RiskLevel::Low,
                    confirmation: ConfirmationPolicy::None,
                    retry: RetryPolicy::AtMostOnce,
                    reversible: false,
                    ..ActionContract::default()
                }),
        );
        if self.last_trash_undo.is_some() {
            folder.actions.push(
                ActionSpec::operation(
                    "undo-trash",
                    t("files-undo-move-trash"),
                    OperationKind::Effect,
                    "files.folder-command",
                    &"undo-trash",
                )
                .expect("undo-trash payload is serializable")
                .with_contract(ActionContract {
                    permission: PermissionRequirement::Write,
                    retry: RetryPolicy::AtMostOnce,
                    ..ActionContract::default()
                }),
            );
        }
        if self.is_trash_view() {
            folder.actions.push(
                ActionSpec::operation(
                    "empty-trash",
                    t("files-menu-empty-trash"),
                    OperationKind::Effect,
                    "files.folder-command",
                    &"empty-trash",
                )
                .expect("empty-trash command is serializable")
                .destructive()
                .with_contract(ActionContract {
                    permission: PermissionRequirement::Write,
                    risk: RiskLevel::Destructive,
                    confirmation: ConfirmationPolicy::Confirm,
                    retry: RetryPolicy::AtMostOnce,
                    ..ActionContract::default()
                }),
            );
        }
    }

    /// Construct the complete action contract for one already-projected row.
    /// This is intentionally separate from `resources`: a folder can have far
    /// more rows than an action directory should enumerate, while an exact
    /// search/history/agent address must still resolve to the same canonical
    /// actions as a visible context menu.
    #[allow(
        clippy::expect_used,
        reason = "ActionSpec serializes only owned, validated in-memory values"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "an entry's complete canonical action contract stays together so lazy and eager resource resolution cannot diverge by dispatch route"
    )]
    fn entry_resource(&self, entry: &Entry) -> Resource {
        let resource_id = Self::entry_resource_id(entry);
        let record_id = entry.record_id.map(uuid::Uuid::from_bytes);
        let mut resource = Resource::new(
            resource_id.clone(),
            if record_id.is_some() {
                "files.record"
            } else {
                "files.entry"
            },
            entry.name.clone(),
        );
        if let Some(record_id) = record_id {
            resource.actions.push(
                ActionSpec::operation(
                    crate::search_provider::REVEAL_ACTION,
                    t("files-action-reveal"),
                    OperationKind::View,
                    "files.reveal-record",
                    &record_id.to_string(),
                )
                .expect("record id is serializable")
                .with_contract(ActionContract::view()),
            );
        }
        if let Ok(spec) = ActionSpec::operation(
            "open",
            t("files-menu-open"),
            OperationKind::View,
            "files.open",
            &resource_id,
        ) {
            resource
                .actions
                .push(spec.with_contract(ActionContract::view()));
        }
        self.append_open_with_actions(entry, &resource_id, &mut resource);
        if !self.is_trash_view() {
            let rename_resource_id = resource_id.clone();
            resource.actions.push(
                ActionSpec::prompted_operation(
                    "rename",
                    t("files-menu-rename"),
                    t("files-action-rename-prompt"),
                    &entry.name,
                    OperationKind::Effect,
                    "files.rename",
                    move |name| {
                        EncodedAction::encode_operation(
                            "files.rename",
                            &vec![rename_resource_id.as_str(), name],
                        )
                        .expect("rename payload is serializable")
                    },
                )
                .with_contract(ActionContract {
                    permission: PermissionRequirement::Write,
                    retry: RetryPolicy::AtMostOnce,
                    // The filesystem owns durable history, so never advertise
                    // an action-history inverse that Files cannot honour.
                    ..ActionContract::default()
                }),
            );
            resource
                .actions
                .extend(Self::transfer_actions(&resource_id));
            resource.actions.push(
                ActionSpec::operation(
                    "send-nearby",
                    t("files-menu-send-nearby"),
                    OperationKind::View,
                    "files.entry-command",
                    &vec!["send-nearby".to_string(), resource_id.clone()],
                )
                .expect("nearby picker payload is serializable")
                .with_contract(ActionContract::view()),
            );
            for target in self
                .nearby_send
                .targets()
                .iter()
                .filter(|target| target.online)
            {
                let action_id = format!("send-nearby-{}", hex_prefix(target.handle.as_bytes(), 6));
                resource.actions.push(
                    ActionSpec::operation(
                        action_id,
                        t_args(
                            "files-action-send-nearby-device",
                            &[arg("device", target.label.clone())],
                        ),
                        OperationKind::Effect,
                        "files.nearby-send",
                        &(resource_id.clone(), target.handle),
                    )
                    .expect("exact nearby target payload is serializable")
                    .with_contract(ActionContract {
                        permission: PermissionRequirement::Read,
                        retry: RetryPolicy::AtMostOnce,
                        ..ActionContract::default()
                    })
                    .execution(ExecutionMode::Background { cancellable: true })
                    .returns(ResultSpec::Job),
                );
            }
        }
        let entry_commands: Vec<(&str, String, &str, bool)> = if self.is_trash_view() {
            vec![
                ("restore", t("files-menu-restore"), "restore", false),
                (
                    "delete-permanently",
                    t("files-menu-delete-permanently"),
                    "purge",
                    true,
                ),
                (
                    "properties",
                    t("files-menu-properties"),
                    "properties",
                    false,
                ),
            ]
        } else {
            vec![
                ("copy", t("files-menu-copy"), "copy", false),
                ("cut", t("files-menu-cut"), "cut", false),
                ("trash", t("files-menu-move-trash"), "trash", true),
                (
                    "properties",
                    t("files-menu-properties"),
                    "properties",
                    false,
                ),
                ("sharing", t("files-sharing-access"), "sharing", false),
                ("history", t("files-version-history"), "history", false),
            ]
        };
        for (id, label, verb, destructive) in entry_commands {
            let mut spec = ActionSpec::operation(
                id,
                label,
                if matches!(verb, "properties" | "sharing" | "history") {
                    OperationKind::View
                } else {
                    OperationKind::Effect
                },
                "files.entry-command",
                &vec![verb.to_string(), resource_id.clone()],
            )
            .expect("entry command is serializable");
            if destructive {
                spec = spec.destructive();
            }
            resource.actions.push(spec);
        }
        if let Some(availability) = entry.availability.as_ref() {
            resource.actions.push(
                ActionSpec::operation(
                    "availability",
                    t("files-availability-title-short"),
                    OperationKind::View,
                    "files.entry-command",
                    &vec!["availability".to_string(), resource_id.clone()],
                )
                .expect("availability payload is serializable")
                .with_contract(ActionContract::view()),
            );
            let mut availability_command =
                |id: &str, label: String, verb: &str, destructive: bool| {
                    let mut spec = ActionSpec::operation(
                        id,
                        label,
                        OperationKind::Effect,
                        "files.entry-command",
                        &vec![verb.to_string(), resource_id.clone()],
                    )
                    .expect("availability command payload is serializable")
                    .with_contract(ActionContract {
                        permission: PermissionRequirement::Write,
                        retry: RetryPolicy::AtMostOnce,
                        ..ActionContract::default()
                    });
                    if destructive {
                        spec = spec.destructive();
                    }
                    resource.actions.push(spec);
                };
            if availability.policy == crate::model::AvailabilityPolicy::Automatic {
                availability_command(
                    "keep-offline",
                    t("files-keep-offline"),
                    "keep-offline",
                    false,
                );
            }
            if availability.policy == crate::model::AvailabilityPolicy::Pinned
                && !availability.policy_inherited
            {
                availability_command(
                    "automatic-storage",
                    t("files-use-automatic-storage"),
                    "automatic-storage",
                    false,
                );
            }
            let releasable = !entry.is_dir
                && availability.policy == crate::model::AvailabilityPolicy::Automatic
                && !availability.policy_inherited
                && availability.transfer.phase == crate::model::TransferPhase::Idle
                && matches!(
                    availability.residency,
                    crate::model::Residency::Partial { .. }
                        | crate::model::Residency::Resident { .. }
                );
            if releasable {
                availability_command(
                    "free-local-space",
                    t("files-free-local-space"),
                    "free-local-space",
                    false,
                );
            }
            let retryable = !matches!(
                availability.residency,
                crate::model::Residency::Resident { .. }
            ) && availability.transfer.phase == crate::model::TransferPhase::Idle;
            if retryable {
                availability_command(
                    "retry-download",
                    t("files-retry-download"),
                    "retry-transfer",
                    false,
                );
            }
        }
        if entry.record_id.is_some() {
            resource.actions.push(
                ActionSpec::operation(
                    "share-link",
                    "Create one-time share link",
                    OperationKind::Effect,
                    "files.share-link",
                    &resource_id,
                )
                .expect("share path is serializable")
                .execution(ExecutionMode::Background { cancellable: false })
                .returns(ResultSpec::Job),
            );
        }
        resource
    }

    /// Live extras beside `ResourceRoot` when framework-hosted: listing rows,
    /// remote accounts, nearby transfers, and the error retry control. Not a
    /// second `files.folder` root and not places/templates (those ride publish).
    fn live_filesystem_extras(&self, doc: &automerge::AutoCommit) -> Vec<Resource> {
        let mut extras = self.live_entry_resources();
        extras.extend(self.live_remote_account_resources());
        let mut nearby_root = Resource::new("files.nearby", "files.nearby", "Nearby");
        self.append_nearby_receiver_resources(&mut nearby_root);
        extras.extend(nearby_root.children);
        if self.error.is_some() {
            extras.extend(ui::resources::PublishesResources::resources(
                &self.state_retry,
                doc,
            ));
        }
        extras
    }

    fn live_entry_resources(&self) -> Vec<Resource> {
        self.entries
            .iter()
            .take(MAX_EAGER_ENTRY_RESOURCES)
            .map(|entry| self.entry_resource(entry))
            .collect()
    }

    #[allow(
        clippy::expect_used,
        reason = "remote account payloads are fixed local strings"
    )]
    fn live_remote_account_resources(&self) -> Vec<Resource> {
        // External file accounts are the folders the storage daemon's cloud
        // service mounts under `~/Remote`. Files never talks to the server or
        // opens the vault: it opens the folder like any other place and shows
        // the account's reported health as a badge.
        self.remote_account_snapshot()
            .into_iter()
            .map(|account| {
                let resource_id = crate::remote_accounts::resource_id(&account.id);
                let availability = remote_folder_availability(&account);
                let mut resource =
                    Resource::new(resource_id, "files.remote-account", account.label.clone());
                resource.presentation = Some(tilo_actions::ResourcePresentation {
                    detail: Some(format!("{} · {}", account.kind.label(), account.host)),
                    badge: account.health.badge().map(str::to_owned),
                    preview: None,
                });
                let open_payload = vec!["open".to_string(), account.id.to_string()];
                resource.actions.push(
                    ActionSpec::operation(
                        crate::search_provider::REMOTE_ACCOUNT_OPEN_ACTION,
                        "Open",
                        OperationKind::View,
                        "files.remote-account",
                        &open_payload,
                    )
                    .expect("remote account open payload is serializable")
                    .with_contract(ActionContract {
                        availability,
                        ..ActionContract::view()
                    }),
                );
                let details_payload = vec!["details".to_string(), account.id.to_string()];
                resource.actions.push(
                    ActionSpec::operation(
                        "details",
                        "Show account status",
                        OperationKind::View,
                        "files.remote-account",
                        &details_payload,
                    )
                    .expect("remote account detail payload is serializable")
                    .with_contract(ActionContract::view()),
                );
                let reauth_payload = vec!["reauthenticate".to_string(), account.id.to_string()];
                resource.actions.push(
                    ActionSpec::operation(
                        "reauthenticate",
                        "How to sign in again",
                        OperationKind::View,
                        "files.remote-account",
                        &reauth_payload,
                    )
                    .expect("remote account reauthentication payload is serializable")
                    .with_contract(ActionContract::view()),
                );
                resource
            })
            .collect()
    }
}

#[path = "native_observer.rs"]
mod native_observer;

impl FileExplorerWidget {
    fn history_registration_impl(
        &self,
    ) -> Result<
        Option<ui::history_mode::HistoryRegistration>,
        ui::history_mode::HistoryRegistrationError,
    > {
        ui::history_mode::HistoryRegistration::new(
            &crate::framework_history::HISTORY,
            vec![ui::history_mode::HistorySourceRegistration::CheckedFiles {
                scope: "selected-file",
                source: self.framework_history.clone(),
            }],
        )
        .map(Some)
    }

    fn has_action_control_bindings_impl(&self) -> bool {
        // Own accessibility actions are semantic; preserve the same modal child
        // exposure as the complete binding inventory.
        let mut found = false;
        self.for_each_child(&mut |child| {
            if !found {
                found = ui::widget::controls::has_action_control_bindings(child);
            }
        });
        found
    }

    fn native_input_snapshot_impl(
        &self,
        _doc: &automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> serde_json::Value {
        native_observer::snapshot(self)
    }
}

impl Collaborative for FileExplorerWidget {
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        let now = ui::time::now();
        self.nearby_send.poll_if_due(now);
        self.shortcut_menu.poll_if_due(now);
        let mut requests = self.nearby_send.take_requests();
        requests.extend(self.shortcut_menu.take_requests());
        requests
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        let mut changed = self.handle_shortcut_event(event);
        for effect in self.nearby_send.handle_event(event) {
            match effect {
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
                        history: None,
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
                        history: None,
                    });
                }
                crate::nearby_send::NearbyUiEffect::Status(message) => {
                    if self.status.as_deref() != Some(message.as_str()) {
                        self.status = Some(message);
                        changed = true;
                    }
                }
                crate::nearby_send::NearbyUiEffect::SendResult(result) => {
                    let Some(invocation) = self.resource_invocation.take() else {
                        continue;
                    };
                    changed = true;
                    self.resource_result = Some(match result {
                        Ok(_) => tilo_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: tilo_actions::ActionOutput::default(),
                        },
                        Err(message) => tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "files.nearby_send_failed".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        },
                    });
                }
            }
        }
        if changed {
            self.structure_dirty = true;
        }
    }
}

impl Widget for FileExplorerWidget {
    ui::provides!(
        dyn ui::widget::Collaborative,
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn native_input_snapshot(&self, doc: &AppModel, path: &ModelPath) -> Option<serde_json::Value> {
        let (doc, path) = ui::doc::document_ref(doc, path);
        Some(self.native_input_snapshot_impl(doc, path))
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            "files.refresh",
            "files.new-folder",
            "files.paste",
            "files.go-to",
            "files.move",
            "files.transfer",
            "files.new-from-template",
            "files.open",
            "files.open-with",
            "files.reveal-record",
            "files.folder-command",
            "files.entry-command",
            "files.nearby-send",
            "files.nearby-decide",
            "files.nearby-retry",
            "files.nearby-cancel",
            "files.rename",
            "files.share-link",
            "files.remote-account",
            // The hand-positioned recovery Button still publishes through the
            // normal action door; this handler delegates its stable control
            // resource back to the retained widget.
            "tilo.ui.control.invoke",
        ]
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the resource dispatcher keeps every typed Files operation at one auditable authorization boundary"
    )]
    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if request.type_id == "tilo.ui.control.invoke" {
            // A shared Button publishes its own canonical resource. This root
            // also publishes the contained toolbar/menu controls, so route the
            // exact payload through the retained tree (including the
            // separately-painted retry control) just as a pointer/keyboard
            // activation does.
            if self.state_retry.start_operation(op, doc, path)?.is_none()
                && ui::widget::start_in_children(self, op, doc, path)?.is_none()
            {
                return Ok(None);
            }
            // Control handlers queue the same commands their painted
            // counterparts do. Apply them at this resource boundary,
            // never through a document write or widget-local click flag.
            self.drain_commands();
            return Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )));
        }
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.poll_share_result();
        self.poll_history_result();
        if self
            .resource_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            self.resource_result.take()
        } else {
            None
        }
    }

    fn update(&mut self, _state: &mut AppModel, _path: &ModelPath) -> bool {
        // Service calls finishing on the app runtime wake the event loop.
        let received = self.poll_received_share();
        self.poll_share_result() | received
    }

    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.flush_shared_edits(doc);
        }
        self.poll_deferred_document_open();
        self.poll_pending_launch();
        self.refresh_framework_history_target();
        self.refresh_metadata_if_due(ui::time::now());
        // One bounded catalogue page per idle/sync turn. The palette only
        // reads its retained SearchCatalog; it never performs mount I/O.
        if self.collaborative_record_pager.borrow().has_more() {
            let page = self.advance_collaborative_search_page();
            if page.truncated {
                self.status = Some("Collaborative search needs a refresh to continue.".to_string());
            }
        }
        // A rename proposal is process-local. Seeding it must not mutate the
        // shared document: neither a filesystem observation nor an unfinished
        // filename belongs in collaborative UI history.
        if self.rename_seed_pending {
            self.rename_input.set_text(&self.rename_buffer);
            self.rename_seed_pending = false;
        }

        // Keep the persistent in-place rename editor alive through rebuilds;
        // select the whole seed on its first frame (Finder-style).
        self.rename_input.sync(state, path);
        self.state_retry.sync(state, path);
        if self.rename_select_pending {
            self.rename_input.select_all();
            self.rename_select_pending = false;
        }

        self.toolbar.sync(state, path);
        self.nav.sync(state, path);

        // `TextInput` also accepts accessibility SetValue; absorb any local
        // edit after sync so every input route feeds the same command.
        if self.renaming.is_some()
            && let Some(value) = self.rename_input.take_dirty()
        {
            self.rename_buffer = value;
        }
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let document_open_reply = self
            .pending_document_open
            .as_ref()
            .and_then(|(token, reply)| {
                reply
                    .try_borrow()
                    .ok()
                    .and_then(|reply| (!reply.is_pending(*token)).then(Instant::now))
            });
        Some(self.next_metadata_refresh.min(self.nearby_send.next_poll()))
            .into_iter()
            .chain(document_open_reply)
            .chain(self.nav.next_wake_deadline())
            .chain(self.toolbar.next_wake_deadline())
            .min()
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // Refresh the persistent top bar, then rebuild the footer + swap the
        // freshly-built body panes into the persistent `nav`. `nav` and the
        // `toolbar` keep their state across the swap (docked/push state; the
        // path bar's in-progress edit), so a rebuild triggered by navigation /
        // sort doesn't snap the body or the path bar back mid-interaction.
        self.update_toolbar();
        // At enlarged text sizes, a permanently docked 200 px Places column
        // turns useful labels into a wall of truncation and steals the width
        // needed by file metadata. Promote it to the same labelled hamburger
        // drawer used on narrow screens. The threshold grows only above 1×,
        // preserving the normal desktop/tablet breakpoint exactly.
        self.nav.set_breakpoint(
            BODY_BREAKPOINT_PX + f64::from((theme.text_scale - 1.0).max(0.0)) * 900.0,
        );
        self.composed_once = true;
        self.nav.set_sidebar(Some(self.build_places_pane()));
        self.nav.set_content(self.build_content_pane(theme));

        let above_node = self.toolbar.build_layout(taffy, theme, node_map);
        let nav_node = self.nav.build_layout(taffy, theme, node_map);
        // No permanent footer band: the item count / selection status floats
        // in a reserved bottom lane inside the body (see
        // `draw_status_bubble`), leaving the rest of the body available to
        // the file viewport.

        // The body fills the height left below the top bar (the toolbar uses
        // `with_intrinsic_height`).
        // `NavShell::build_layout` hard-codes a `percent(1.0)` height leaf; in
        // a column of three that would claim the full height, so override it to
        // grow into the remaining space instead.
        // `set_style` only fails when the node id is unknown to this tree;
        // `nav_node` was just built in it above.
        #[allow(
            clippy::unwrap_used,
            reason = "nav_node was just created in this same tree"
        )]
        taffy
            .set_style(
                nav_node,
                taffy::Style {
                    flex_grow: 1.0,
                    flex_shrink: 1.0,
                    flex_basis: taffy::Dimension::length(0.0),
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    min_size: taffy::Size {
                        width: taffy::Dimension::auto(),
                        height: taffy::Dimension::length(0.0),
                    },
                    ..Default::default()
                },
            )
            .unwrap();

        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        // `new_with_children` only fails when a child belongs to a
        // different tree; both children were just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "above_node and nav_node were just created in this same tree"
        )]
        let node = taffy
            .new_with_children(style, &[above_node, nav_node])
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        // `node` was created by `build_layout` in this same tree, so a
        // lookup miss would mean the frame cycle called these out of order.
        #[allow(
            clippy::unwrap_used,
            reason = "node was built in this tree by build_layout; a miss means the frame cycle called these out of order"
        )]
        let l = taffy.layout(node).unwrap();
        self.rect = vello::kurbo::Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        let base = vello::kurbo::Point::new(self.rect.x0, self.rect.y0);
        if let Ok(children) = taffy.children(node)
            && let (Some(&above_n), Some(&nav_n)) = (children.first(), children.get(1))
        {
            self.toolbar.apply_layout(taffy, above_n, base);
            self.nav.apply_layout(taffy, nav_n, base);
        }
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.poll_share_result();
        self.poll_history_result();
        self.paint_background(scene, transform, theme);
        self.nav
            .draw(scene, transform, text_renderer, theme, presence);
        self.draw_places_modal_backdrop(scene, transform, theme);
        self.draw_state_surface(scene, transform, text_renderer, theme, presence);
        // The toolbar is fixed app chrome. Paint it above the body so an
        // error/empty surface can never obscure its Back, Up, or location
        // routes at a compact width.
        self.toolbar
            .draw(scene, transform, text_renderer, theme, presence);
        self.draw_empty_state(scene, transform, text_renderer, theme);
        self.draw_rename_editor(scene, transform, text_renderer, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.paint_background(scene, transform, theme);
        // Recurse with `draw_with_presence` (not the plain `draw`) so the
        // inner `Scrollable` pushes its viewport rect and the file list culls
        // rows outside it. The default trait impl would call our own `draw`,
        // which breaks that chain — a long directory would then emit every
        // row's glyphs every frame regardless of what's visible.
        self.nav.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
        self.draw_places_modal_backdrop(scene, transform, theme);
        self.draw_state_surface(scene, transform, text_renderer, theme, presence);
        self.toolbar.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
        self.draw_empty_state(scene, transform, text_renderer, theme);
        self.draw_rename_editor(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        // Child overlays (the path bar's completion popover, dropdowns) first,
        // then our top-layer context menu (and its flyout) so they composite
        // above everything.
        self.toolbar
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        self.nav
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        // Floating status bubble (item count / status + selection), above the
        // content but below any open menus / drag ghost. A narrow drawer is a
        // modal flap and its scrim must occlude every content affordance.
        if !self.status_bubble_is_occluded_by_places_drawer() {
            self.draw_status_bubble(scene, transform, text_renderer, theme);
        }
        self.draw_collab_panel(scene, transform, text_renderer, theme);
        if self.context_menu.is_open() {
            layout_menu_widget(&mut self.context_menu);
            self.context_menu
                .draw(scene, transform, text_renderer, theme, presence);
        }
        if self.submenu.is_open() {
            layout_menu_widget(&mut self.submenu);
            self.submenu
                .draw(scene, transform, text_renderer, theme, presence);
        }
        // The drop-target highlight sits on the target folder; the drag ghost
        // floats above everything else.
        self.draw_drop_highlight(scene, transform, theme);
        self.draw_pin_ghost(scene, transform, text_renderer, theme);
        self.draw_app_chooser(scene, transform, text_renderer, theme, presence);
    }

    // Primary widget event dispatcher; pointer, keyboard, drag, menu, and
    // accessibility paths converge here before entering semantic commands.
    #[allow(
        clippy::too_many_lines,
        reason = "root event dispatcher orders pointer, keyboard, drag, menu, and retained-child routing"
    )]
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        let state = &mut *ctx.state;
        let path = ctx.path;
        self.poll_share_result();
        self.poll_history_result();
        if self.app_chooser_open() {
            self.route_app_chooser_event(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                state,
                path,
                ctx.focus_target,
            );
            *ctx.handled = true;
            return;
        }
        let panel_was_open = self.collab_panel.is_some();
        // Track the cursor for menu placement + hit-testing.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            // An armed press that has travelled far enough becomes a drag.
            if let Some((start, path, is_dir)) = self.armed_drag.clone()
                && self.pin_drag.is_none()
                && self.last_cursor.distance(start) > crate::views::file_view::DRAG_THRESHOLD
            {
                self.armed_drag = None;
                self.apply_command(FileCommand::BeginDrag { path, is_dir });
                self.structure_dirty = true;
            }
        }
        // The shared EmptyState visual and the retained Button share one
        // recovery route. The button's `on_effect` publishes a typed local
        // control action; draining it below enters the ordinary Refresh path.
        if self.error.is_some() {
            self.state_retry.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: ctx.modifiers,
                    state,
                    path,
                    handled: ctx.handled,
                    focus_target: ctx.focus_target,
                    node_id: ctx.node_id,
                    taffy: ctx.taffy,
                },
            );
            if *ctx.handled {
                self.structure_dirty |= self.drain_commands();
                return;
            }
            if matches!(
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
            ) {
                self.structure_dirty |= self.apply_command(FileCommand::Refresh);
                *ctx.handled = true;
                return;
            }
        }
        if self.collab_panel.is_some() {
            use winit::keyboard::{Key, NamedKey};
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
                } if self
                    .collab_panel_action_rect()
                    .is_some_and(|rect| rect.contains(self.last_cursor)) =>
                {
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
                } => {
                    let rect = self.collab_panel_rect();
                    let close_rect = vello::kurbo::Rect::new(
                        rect.x1 - 48.0,
                        rect.y0 + 6.0,
                        rect.x1 - 8.0,
                        rect.y0 + 46.0,
                    );
                    !rect.contains(self.last_cursor) || close_rect.contains(self.last_cursor)
                }
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
            // A release without travel was a click, not a drag.
            self.armed_drag = None;
        }

        // A live drag, the open context menu, a right-click, a zoom wheel,
        // and an in-place rename all get first dibs on the event, in that
        // priority order; the first that consumes it stops routing.
        if self.handle_live_drag_event(event, ctx)
            || self.handle_places_drawer_modal_event(event, ctx)
            || self.handle_open_menu_event(event, ctx)
            || self.handle_right_click_open_menu(event, ctx)
            || self.handle_zoom_wheel(event, ctx)
            || self.handle_rename_mode_event(event, ctx)
        {
            return;
        }

        self.finish_event(event, ctx);
        if !panel_was_open && self.collab_panel.is_some() {
            // The dialog replaced the semantic subtree during this event.
            // Focus its real backing widget so AccessKit never points at a
            // now-hidden toolbar/list node.
            *ctx.focus_target = Some(ctx.node_id);
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut d = std::mem::take(&mut self.structure_dirty);
        d |= self.toolbar.take_structure_dirty();
        d |= self.nav.take_structure_dirty();
        d
    }

    fn needs_animation_frame(&self) -> bool {
        // The body's docked→push / reveal slide must keep the host pumping
        // frames while it settles; the floating status bubble likewise while it
        // lingers / fades.
        self.nav.needs_animation_frame()
            || self.toolbar.needs_animation_frame()
            || (self.is_loading && self.state_spinner.needs_animation_frame())
            || self.status_bubble.needs_frame(ui::time::now())
    }

    fn needs_idle_sync(&self) -> bool {
        self.collaborative_record_pager.borrow().has_more()
            || self.nav.needs_idle_sync()
            || self.toolbar.needs_idle_sync()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.collab_panel.is_some() {
            return Vec::new();
        }
        let v: Vec<&dyn Widget> = vec![
            &self.toolbar,
            &self.nav,
            // Standalone overlays are not taffy children, but they are semantic
            // children: expose their menu/item nodes while open (closed nodes carry
            // `hidden`, so assistive technology drops them).
            &self.context_menu,
            &self.submenu,
        ];
        v
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.collab_panel.is_some() {
            return Vec::new();
        }
        let v: Vec<&mut (dyn Widget + '_)> = vec![
            &mut self.toolbar,
            &mut self.nav,
            &mut self.context_menu,
            &mut self.submenu,
        ];
        v
    }

    fn name(&self) -> &'static str {
        "FileExplorerWidget"
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    fn traps_focus(&self) -> bool {
        self.collab_panel.is_some() || self.app_chooser_open()
    }

    fn is_focusable(&self) -> bool {
        self.collab_panel.is_some()
    }

    fn set_focus(&mut self, focused: bool) {
        self.collab_panel_focused = focused && self.collab_panel.is_some();
    }

    fn is_focused(&self) -> bool {
        self.collab_panel.is_some() && self.collab_panel_focused
    }

    #[allow(
        clippy::too_many_lines,
        reason = "custom-painted file, collaboration, and empty-state surfaces require one matching semantic tree"
    )]
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        if self.app_chooser_open() {
            return self.app_chooser.accessibility();
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
            return Some(
                AccessNode::new(Role::Dialog)
                    .with_name(panel.title.clone())
                    .with_description(t_args(
                        "files-a11y-dialog-close",
                        &[arg("summary", panel.summary.clone())],
                    ))
                    .with_value(panel.lines.join(". "))
                    .with_live(Politeness::Polite)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.collab_panel_focused,
                        modal: true,
                        ..Default::default()
                    })
                    .with_children(children),
            );
        }

        let mut root = AccessNode::new(Role::Group)
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
            .with_state(AccessState {
                busy: self.is_loading,
                ..Default::default()
            });
        if let Some((title, detail, can_retry)) = self.state_surface_copy() {
            let mut state = AccessNode::new(Role::Group)
                .with_name(title)
                .with_description(detail)
                .with_live(if can_retry {
                    Politeness::Assertive
                } else {
                    Politeness::Polite
                })
                .with_state(AccessState {
                    busy: self.is_loading,
                    ..Default::default()
                });
            if can_retry {
                state = state.with_children(vec![
                    AccessNode::new(Role::Button)
                        .with_name(t("files-try-again"))
                        .with_action_target("files-retry-folder"),
                ]);
            }
            root = root.with_children(vec![state]);
        }
        Some(root)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action;
        if target == "files-retry-folder" && matches!(action, Action::Activate) {
            let changed = self.apply_command(FileCommand::Refresh);
            self.structure_dirty |= changed;
            return changed;
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
                    version_id: collab_types::FileVersionId(version),
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

impl ui::search::capability::SearchSource for FileExplorerWidget {
    /// Keep ordinary Places search and add a separately bounded snapshot of
    /// collaborative records from the live mount projection. Collaborative
    /// actions carry stable record UUIDs; their current paths are provenance,
    /// never identity.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![
            Box::new(self.places_search.clone()),
            Box::new(self.collaborative_search.clone()),
            Box::new(self.remote_accounts_search.clone()),
        ]
    }
}

impl ui::resources::PublishesResources for FileExplorerWidget {
    #[allow(
        clippy::expect_used,
        reason = "JSON serialization of owned strings and static command payloads is infallible"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "the resource catalogue mirrors the folder/entry protocol in one auditable tree; splitting it would separate action contracts from their resources"
    )]
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<Resource> {
        // Hosted: ResourceRoot already published folder Views/Effects, places,
        // and templates via tilo_resource::publish. Keep only live filesystem
        // listing rows (and other live extras) — never a second files.folder.
        if self.is_framework_hosted() {
            return self.live_filesystem_extras(doc);
        }
        vec![self.published_folder(doc)]
    }
    fn action_directory_resource_page(
        &self,
        doc: &automerge::AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui::widget::ActionDirectoryResourcePage> {
        if self.is_framework_hosted() {
            // Folder root is ResourceRoot's publish, not a second page.
            return Some(ui::widget::ActionDirectoryResourcePage {
                resources: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: true,
            });
        }
        Some(ui::widget::ActionDirectoryResourcePage {
            resources: (cursor.is_empty() && max_results > 0)
                .then(|| self.published_folder(doc))
                .into_iter()
                .collect(),
            next_cursor: Vec::new(),
            exhausted: true,
        })
    }
    /// Materialise an exact search-addressed row only when the action host is
    /// about to resolve it. This keeps the open resource tree bounded to the
    /// actual folder while preserving canonical action equivalence for Places,
    /// remote accounts, and late collaborative records.
    fn resource(&self, doc: &automerge::AutoCommit, id: &str) -> Option<Resource> {
        fn find(resource: &Resource, id: &str) -> Option<Resource> {
            if resource.id.as_str() == id {
                return Some(resource.clone());
            }
            for child in &resource.children {
                if let Some(found) = find(child, id) {
                    return Some(found);
                }
            }
            None
        }
        let folder = self.published_folder(doc);
        if let Some(found) = find(&folder, id) {
            if self.is_framework_hosted() && found.id == folder.id {
                return None;
            }
            return Some(found);
        }
        if !self.is_framework_hosted()
            && id == crate::search_provider::folder_resource_id(&self.current_path)
        {
            return Some(self.published_folder(doc));
        }
        self.entries
            .iter()
            .find(|entry| Self::entry_resource_id(entry) == id)
            .map(|entry| self.entry_resource(entry))
            .or_else(|| {
                self.places_search
                    .resource(id)
                    .or_else(|| self.collaborative_search.resource(id))
                    .or_else(|| self.remote_accounts_search.resource(id))
            })
            .or_else(|| {
                self.error.is_some().then(|| {
                    self.state_retry
                        .resources(doc)
                        .into_iter()
                        .find(|resource| resource.id.as_str() == id)
                })?
            })
    }
}

impl ui::widget::controls::ActionControls for FileExplorerWidget {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if let Some(invocation) = self.app_chooser.take_control_invocation() {
            return Some(invocation);
        }
        self.state_retry.take_control_invocation().or_else(|| {
            self.children_mut()
                .into_iter()
                .find_map(ui::widget::controls::take_control_invocation)
        })
    }
    fn has_action_control_bindings(&self) -> bool {
        self.has_action_control_bindings_impl()
    }
}

impl ui::history_mode::capability::DocumentHistory for FileExplorerWidget {
    fn history_registration(
        &self,
    ) -> Result<
        Option<ui::history_mode::HistoryRegistration>,
        ui::history_mode::HistoryRegistrationError,
    > {
        self.history_registration_impl()
    }
}

impl FileExplorerWidget {
    #[allow(
        clippy::too_many_lines,
        reason = "this is the single protocol dispatcher for the declared local file operations; keeping the match together makes handler coverage auditable"
    )]
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> Result<bool, String> {
        match (kind, type_id) {
            (OperationKind::Effect, "files.refresh") => self.refresh(),
            (OperationKind::Effect, "files.new-folder") => {
                let name: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                fileops::new_folder(&self.current_path, &name)?;
                self.refresh();
            }
            (OperationKind::Effect, "files.new-from-template") => {
                let template_id: String =
                    serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let Some(template_dir) = templates::templates_dir() else {
                    return Err("the Templates directory is unavailable".into());
                };
                let target = templates::list_templates(&template_dir)
                    .iter()
                    .find(|candidate| {
                        template_resource_id(&candidate.to_string_lossy()) == template_id
                    })
                    .cloned()
                    .ok_or_else(|| "the template is no longer available".to_string())?;
                templates::create_from_template(&target, &self.current_path)?;
                self.refresh();
            }
            (OperationKind::Effect, "files.paste") => {
                if self.clipboard.is_empty() {
                    return Err("The clipboard is empty".into());
                }
                let destination = self.current_path.clone();
                let sources = self.clipboard.clone();
                let cut = self.clipboard_cut;
                let mut failures = Vec::new();
                let mut remaining_cut_sources = Vec::new();
                for source in &sources {
                    let result = if cut {
                        fileops::move_into(source, &destination)
                    } else {
                        fileops::copy_into(source, &destination)
                    };
                    if let Err(error) = result {
                        if cut {
                            remaining_cut_sources.push(source.clone());
                        }
                        failures.push(format!("{}: {error}", source.display()));
                    }
                }
                if cut {
                    self.clipboard = remaining_cut_sources;
                }
                self.refresh();
                if !failures.is_empty() {
                    return Err(format!("paste failed: {}", failures.join("; ")));
                }
            }
            (OperationKind::View, "files.go-to") => {
                let path: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let path = PathBuf::from(path);
                if !path.is_absolute() {
                    return Err(format!("{} is not an absolute path", path.display()));
                }
                if !path.is_dir() {
                    return Err(format!("{} is not a folder Files can open", path.display()));
                }
                self.navigate(path);
                self.operation_result = Some(self.folder_answer());
            }
            (OperationKind::Effect, "files.move") => {
                let request: MoveRequest =
                    serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                if request.entries.is_empty() || request.entries.len() > MAX_MOVE_ENTRIES {
                    return Err(format!("move takes 1 to {MAX_MOVE_ENTRIES} entries"));
                }
                self.operation_result = Some(self.move_entries(&request)?.into());
            }
            (OperationKind::Effect, "files.transfer") => {
                let [verb, resource_id, folder]: [String; 3] =
                    serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let source = self
                    .visible_entry_path(&resource_id)
                    .ok_or_else(|| "file entry is no longer visible".to_string())?;
                let folder = PathBuf::from(folder);
                if !folder.is_absolute() || !folder.is_dir() {
                    let name = source
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let mut error = format!("{} is not an existing folder.", folder.display());
                    if verb == "move-to" {
                        let _ = write!(
                            error,
                            r#" To create it and move there, call move on the folder with {{"entries":[{}],"folder":{},"create":true}}"#,
                            serde_json::Value::from(name),
                            serde_json::Value::from(folder.to_string_lossy().into_owned())
                        );
                    }
                    return Err(error);
                }
                match verb.as_str() {
                    "move-to" => fileops::move_into(&source, &folder)?,
                    "copy-to" => fileops::copy_into(&source, &folder)?,
                    _ => return Ok(false),
                };
                self.refresh();
            }
            (OperationKind::View, "files.open") => {
                let resource_id: String =
                    serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                // `open` is an action on an already-published resource, not a
                // general ambient-path launcher. Resolve its opaque id only
                // against the live presentation snapshot so deleted, renamed,
                // or replayed rows cannot regain authority or disclose paths.
                let target = self
                    .visible_open_path(&resource_id)
                    .ok_or_else(|| "file location is no longer available".to_string())?;
                self.open_path(target);
            }
            (OperationKind::Effect, "files.open-with") => {
                self.invoke_open_with(payload)?;
            }
            (OperationKind::View, "files.reveal-record") => {
                let value: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let record_id = uuid::Uuid::parse_str(&value)
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
                }
            }
            (kind, "files.folder-command") => {
                let verb: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let expected_kind = match verb.as_str() {
                    "up" | "back" | "forward" | "recent" | "list" | "grid" | "sort-name"
                    | "sort-size" | "sort-type" | "sort-modified" | "sort-ascending"
                    | "sort-descending" | "zoom-in" | "zoom-out" | "hidden" | "shortcuts" => {
                        OperationKind::View
                    }
                    "add-favorite" | "remove-favorite" | "empty-trash" | "terminal"
                    | "undo-trash" => OperationKind::Effect,
                    _ => return Ok(false),
                };
                if kind != expected_kind {
                    return Err("file command has an invalid operation kind".into());
                }
                match verb.as_str() {
                    "up" => self.go_up(),
                    "back" => self.history_back(),
                    "forward" => self.history_forward(),
                    "recent" => self.navigate(crate::recent::location()),
                    "list" => self.set_view_mode(ViewMode::List),
                    "grid" => self.set_view_mode(ViewMode::Grid),
                    "sort-name" => self.set_sort_by(SortBy::Name),
                    "sort-size" => self.set_sort_by(SortBy::Size),
                    "sort-type" => self.set_sort_by(SortBy::Type),
                    "sort-modified" => self.set_sort_by(SortBy::Modified),
                    "sort-ascending" => {
                        self.apply_set_sort_order(SortOrder::Ascending);
                    }
                    "sort-descending" => {
                        self.apply_set_sort_order(SortOrder::Descending);
                    }
                    "zoom-in" => self.set_zoom(self.zoom_level.saturating_add(1)),
                    "zoom-out" => self.set_zoom(self.zoom_level.saturating_sub(1)),
                    "shortcuts" => self.show_keyboard_shortcuts_panel(),
                    "hidden" => {
                        self.apply_command(FileCommand::ToggleHidden);
                    }
                    "add-favorite" => {
                        if !self.current_path.is_dir() {
                            return Err("the current location cannot be a favourite".into());
                        }
                        self.apply_command(FileCommand::AddFavorite(self.current_path.clone()));
                    }
                    "remove-favorite" => {
                        self.apply_command(FileCommand::RemoveFavorite(self.current_path.clone()));
                    }
                    "empty-trash" if self.is_trash_view() => {
                        // The action contract was confirmation-gated by the
                        // resource broker; do not make an agent traverse an
                        // inaccessible second UI-only confirmation sheet.
                        self.perform_empty_trash();
                    }
                    "undo-trash" => {
                        self.apply_undo_last_trash();
                    }
                    "terminal" => self
                        .open_terminal(&mut SystemRunner)
                        .map_err(|e| format!("could not open terminal: {e}"))?,
                    _ => return Ok(false),
                }
            }
            (kind, "files.entry-command") => {
                let args: Vec<String> =
                    serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let verb = args
                    .first()
                    .ok_or_else(|| "missing file command".to_string())?;
                let resource_id = args
                    .get(1)
                    .ok_or_else(|| "missing file resource".to_string())?;
                let target = self
                    .visible_entry_path(resource_id)
                    .ok_or_else(|| "file entry is no longer visible".to_string())?;
                let expected_kind = match verb.as_str() {
                    "properties" | "sharing" | "history" | "availability" | "send-nearby" => {
                        OperationKind::View
                    }
                    "copy" | "cut" | "restore" | "purge" | "trash" | "keep-offline"
                    | "automatic-storage" | "free-local-space" | "retry-transfer" => {
                        OperationKind::Effect
                    }
                    _ => return Ok(false),
                };
                if kind != expected_kind {
                    return Err("file command has an invalid operation kind".into());
                }
                let index = self
                    .entries
                    .iter()
                    .position(|entry| entry.path == target)
                    .ok_or_else(|| "file entry is no longer visible".to_string())?;
                self.selection.select_only(index);
                if verb == "trash" {
                    if self.is_trash_view() {
                        return Err("Use the separately confirmed permanent-delete action for items already in Trash".into());
                    }
                    self.apply_delete();
                    self.structure_dirty = true;
                    return Ok(true);
                }
                let command = match verb.as_str() {
                    "copy" => FileCommand::Copy,
                    "cut" => FileCommand::Cut,
                    "extract" => FileCommand::ExtractHere,
                    "compress" => FileCommand::CompressSelection,
                    "restore" if self.is_trash_view() => FileCommand::RestoreFromTrash,
                    "purge" if self.is_trash_view() => FileCommand::ConfirmPermanentDelete {
                        paths: vec![target],
                    },
                    "properties" => FileCommand::ShowProperties,
                    "sharing" => FileCommand::ShowSharing,
                    "history" => FileCommand::ShowHistory,
                    "availability" => FileCommand::ShowAvailability,
                    "keep-offline" => FileCommand::KeepOffline,
                    "automatic-storage" => FileCommand::RemoveOfflinePin,
                    "free-local-space" => FileCommand::FreeLocalSpace,
                    "retry-transfer" => FileCommand::RetryTransfer,
                    "send-nearby" => FileCommand::OpenNearbySend,
                    _ => return Ok(false),
                };
                if !self.apply_command(command) {
                    return Err("file operation could not be applied".into());
                }
            }
            (OperationKind::Effect, "files.nearby-send") => {
                let (resource_id, handle): (String, ui::collaboration::NearbyTargetHandle) =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                let target = self
                    .visible_entry_path(&resource_id)
                    .ok_or_else(|| "file entry is no longer visible".to_string())?;
                if !self
                    .nearby_send
                    .targets()
                    .iter()
                    .any(|target| target.handle == handle && target.online)
                {
                    return Err("nearby device is no longer available".into());
                }
                self.nearby_send.send(&target, handle)?;
                self.nearby_send_path = Some(target.clone());
                self.status = Some(t("files-nearby-queueing"));
            }
            (OperationKind::Effect, "files.nearby-decide") => {
                let (handle, destination, remember_for_session): (
                    ui::collaboration::NearbyTransferHandle,
                    Option<ui::collaboration::NearbyDestinationHandle>,
                    bool,
                ) = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                let transfer = self
                    .nearby_send
                    .transfers()
                    .iter()
                    .find(|transfer| transfer.handle == handle)
                    .ok_or_else(|| "nearby transfer is no longer available".to_string())?;
                if transfer.direction != ui::collaboration::NearbyTransferDirection::Receiving
                    || !matches!(
                        transfer.phase,
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
                self.nearby_send
                    .decide(handle, destination, remember_for_session);
                self.status = Some(if destination.is_some() {
                    t("files-nearby-accepted")
                } else {
                    t("files-nearby-declined")
                });
            }
            (OperationKind::Effect, "files.nearby-retry") => {
                let handle: ui::collaboration::NearbyTransferHandle =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                if !self.nearby_send.transfers().iter().any(|transfer| {
                    transfer.handle == handle
                        && transfer.phase == ui::collaboration::NearbyTransferPhase::Failed
                }) {
                    return Err("nearby transfer is not retryable".into());
                }
                self.nearby_send.retry(handle);
                self.status = Some(t("files-nearby-retrying"));
            }
            (OperationKind::Effect, "files.nearby-cancel") => {
                let handle: ui::collaboration::NearbyTransferHandle =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                if !self.nearby_send.transfers().iter().any(|transfer| {
                    transfer.handle == handle
                        && matches!(
                            transfer.phase,
                            ui::collaboration::NearbyTransferPhase::Queued
                                | ui::collaboration::NearbyTransferPhase::Transferring
                        )
                }) {
                    return Err("nearby transfer is not cancellable".into());
                }
                self.nearby_send.cancel(handle);
                self.status = Some(t("files-nearby-cancelled"));
            }
            (OperationKind::Effect, "files.rename") => {
                let args: Vec<String> =
                    serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let resource_id = args
                    .first()
                    .ok_or_else(|| "missing file resource".to_string())?;
                let name = args
                    .get(1)
                    .ok_or_else(|| "missing new file name".to_string())?;
                let target = self
                    .visible_entry_path(resource_id)
                    .ok_or_else(|| "file entry is no longer visible".to_string())?;
                fileops::rename(&target, name)?;
                self.refresh();
            }
            (OperationKind::Effect, "files.share-link") => {
                let resource_id: String =
                    serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let target = self
                    .visible_entry_path(&resource_id)
                    .ok_or_else(|| "file entry is no longer visible".to_string())?;
                let index = self
                    .displayed_entries()
                    .iter()
                    .position(|entry| entry.path == target)
                    .ok_or_else(|| "file entry is no longer visible".to_string())?;
                self.selection.select_only(index);
                if !self.apply_command(FileCommand::CreateOneTimeShareLink)
                    || self.share_pending_path.is_none()
                {
                    return Err(self
                        .status
                        .clone()
                        .unwrap_or_else(|| "sharing is unavailable for this file".into()));
                }
            }
            (OperationKind::View, "files.remote-account") => {
                let args: Vec<String> =
                    serde_json::from_slice(payload).map_err(|error| error.to_string())?;
                let operation = args
                    .first()
                    .ok_or_else(|| "missing remote account operation".to_string())?;
                let id = args
                    .get(1)
                    .ok_or_else(|| "missing remote account id".to_string())?;
                let account = self
                    .remote_account(id)
                    .ok_or_else(|| "remote account is no longer available".to_string())?;
                match operation.as_str() {
                    "open" => match (&account.mount, remote_folder_availability(&account)) {
                        (Some(mount), Availability::Available) => self.open_path(mount.clone()),
                        _ => self.show_remote_account_panel(&account, false),
                    },
                    "details" => self.show_remote_account_panel(&account, false),
                    "reauthenticate" => self.show_remote_account_panel(&account, true),
                    _ => return Ok(false),
                }
            }
            _ => return Ok(false),
        }
        self.structure_dirty = true;
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if self.resource_invocation.is_some() {
            return Err("another file operation is still running".into());
        }
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        if matches!(type_id, "files.share-link" | "files.nearby-send") {
            self.resource_invocation = Some(invocation);
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput {
                    value: self.operation_result.take(),
                    ..tilo_actions::ActionOutput::default()
                },
            )))
        }
    }
}

impl AppWidget for FileExplorerWidget {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        static TITLE: OnceLock<String> = OnceLock::new();
        TITLE.get_or_init(|| t("files-app-title")).as_str()
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰉋")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Files)
    }
    fn take_host_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        self.host_intent.take()
    }
}

#[path = "files_app/open_with_recent.rs"]
mod open_with_recent;

#[cfg(test)]
#[path = "files_app/tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "framework_history_tests.rs"]
mod framework_history_tests;

#[cfg(test)]
mod merge_support_tests {
    use super::merge_supported_path;

    #[test]
    fn merge_is_never_offered_for_binary_or_unsupported_names() {
        assert!(merge_supported_path(std::path::Path::new("notes.txt")));
        assert!(merge_supported_path(std::path::Path::new("draft.QDOC")));
        assert!(!merge_supported_path(std::path::Path::new("photo.png")));
        assert!(!merge_supported_path(std::path::Path::new("unknown")));
    }
}
