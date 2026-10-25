//! `FileExplorerWidget` — the Tilo file explorer's `AppWidget`.
//!
//! The widget owns all app state (current dir, entries, multi-selection,
//! sort, view mode, navigation history) and rebuilds its child widget tree
//! from that state. Interactive children can't borrow the app back, so they
//! push [`FileCommand`]s into a shared [`CommandSink`]; `handle_event`
//! drains the sink after each event and applies the commands, marking the
//! tree structurally dirty so the App rebuilds the composition on the next
//! frame. See [`crate::command`].

use ui::num::i64_of_usize;
use ui::widget::TaffyTreeExt;

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
use std::sync::{Arc, OnceLock};
use thumbnail::{ThumbKey, ThumbnailService};
use ui::lock::MutexRecover as _;
use ui::paint::PaintCtx;
#[cfg(test)]
use ui::tilo_actions::{ActionKind, ConfirmationPolicy, ExecutionMode, OperationKind, ResultSpec};
use ui::tilo_actions::{Availability, Resource};
use ui::time::{Duration, Instant, UNIX_EPOCH};
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Button, Collaborative, Column, EmptyState, EmptyStateContent, NavShell, ScrollAxis, Scrollable,
    SidebarPlacement, Spacer, Spinner, TextInput, Widget,
};
use ui::{AppKind, AppWidget, IconDescriptor, SelectionModel};
use vello::kurbo::Point;
use vello::peniko::ImageData as PenikoImage;

mod applications;
mod clipboard_client;
mod cloud_browse;
mod drives;
mod entry_rows;
mod mounted_cloud;
mod native_open_with;
mod selected_open;
mod window_bar;
pub use clipboard_client::Receipt as ClipboardReceipt;
pub(crate) use clipboard_client::UndoRequest;
mod archives;
#[cfg(test)]
use archives::archive_member_copy;
use archives::{ArchiveMembers, archive_member_cached};
mod clash;
mod notice;
mod preview;
use preview::Preview;
mod read_text;
use clash::{ClashChoice, PendingClash};
mod selection_actions;
use notice::{Notice, NoticeAction, NoticeClick};
mod folder_read;
use folder_read::{Purpose, Then};
mod shown_selection;

/// A copy or move shows its progress once it has run this long.
const TRANSFER_SHOWN_AFTER: Duration = Duration::from_millis(600);
/// How long a drag rests on a folder before the folder opens.
const SPRING_OPEN: Duration = Duration::from_millis(700);
/// The accessibility target prefix of the clash sheet's choices.
const CLASH_TARGET: &str = "files-clash-choice:";
/// The accessibility targets of the notice's action and close buttons.
const NOTICE_ACTION_TARGET: &str = "files-notice-action";
const NOTICE_DISMISS_TARGET: &str = "files-notice-dismiss";
/// The most lines a detail sheet lists.
const COLLAB_PANEL_MAX_LINES: usize = 20;
/// A paste's policy for names the folder already has, as callers name it.
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

/// The keys that run `command` in the listing, shown beside it in menus so
/// the menu teaches the keyboard. Only commands with a key of their own.
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

/// Where the part of a file name to select for renaming ends: before its
/// last extension (`report` of `report.final.pdf`). `None` selects the whole
/// name: a folder, a dotfile, or a name with no extension.
fn rename_stem_end(name: &str, is_dir: bool) -> Option<usize> {
    if is_dir {
        return None;
    }
    name.rfind('.').filter(|&dot| dot > 0)
}

pub fn status_bubble_safe_inset(theme: &ui::Theme) -> f32 {
    theme.tokens.spacing.s16
}
/// Close up a menu spec: a separator stands only between two groups, and a
/// group of one is not a group, so it joins its neighbour rather than being
/// fenced off by a hairline. (A separator is an item with no label and no
/// command.)
fn tidy_menu(spec: Vec<(String, Option<FileCommand>)>) -> Vec<(String, Option<FileCommand>)> {
    tidy_groups(
        spec,
        |item| item.0.is_empty() && item.1.is_none(),
        || (String::new(), None),
    )
}

/// [`tidy_menu`] for any item type: `is_separator` says which items divide
/// groups and `separator` makes one.
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
/// The most entry names showing a folder answers with.
const MAX_ANSWERED_ENTRIES: usize = 64;

/// What a window shows when an operation it queued settles.
enum OpFollowUp {
    /// Select the new folder and start naming it.
    NewFolder,
    /// Close the rename editor (or keep it open on a failure).
    Rename,
    /// Offer to undo the move of `count` entries to Trash.
    Trash { count: usize },
    /// Offer to undo the move into `destination`.
    Move { destination: String },
    /// Items a Replace moved to Trash: said only if that failed.
    Replaced,
    /// Say how many of `count` were pasted; after Cut, take the moved ones
    /// off the clipboard (the ones that failed stay, to retry).
    Paste { count: usize, cut: bool },
    /// Select what extracting the archive `name` made, or say why not.
    Extract { name: String },
    /// Select the new archive, or say why it was not made.
    Compress,
}

/// What doing an effect came to: its answer now, or a job that answers later.
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
/// A remote account's folder opens while the cloud service has it mounted.
/// A disabled or not-yet-mounted account says why instead.
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

/// Build the file manager's thumbnail service. The content-addressed cache
/// lives under the app's persistence dir (a tempdir in tests, so they stay
/// isolated) so previews survive across launches; it falls back to a temp dir
/// when there is no persistence dir. `None` when neither location is
/// writable — previews then fall back to type badges (see [`ThumbCtx`]).
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

/// PDFs get first-page previews: Files links the PDF engine, which the
/// thumbnailer every app shares leaves out (`thumbnail::set_pdf_rasteriser`).
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
    /// Look at the cloud account with this id again.
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

/// The row a failed listing offers its retry on.
const RETRY_ROW: &str = "files.retry";

/// A failed listing's way on, as a row: Try again when that may get past
/// the failure, else going to the enclosing folder.
fn retry_row(may_pass: bool) -> Resource {
    let (effect, label) = if may_pass {
        (crate::effects::FilesEffect::Refresh, t("files-try-again"))
    } else {
        (crate::effects::FilesEffect::Up, t("files-open-enclosing"))
    };
    Resource::new(RETRY_ROW, RETRY_ROW, label.clone())
        .with_action(crate::effects::row_action(&effect, "invoke", label))
}

/// The retained button under a failed listing, bound to [`retry_row`].
fn recovery_button(may_pass: bool) -> Button {
    let row = retry_row(may_pass);
    let button = Button::new(RETRY_ROW, &row.label);
    match row.actions.first().and_then(|action| {
        ui::tilo_actions::ActionBinding::static_json(&row, action, serde_json::Map::new()).ok()
    }) {
        Some(binding) => button.on_action(binding),
        None => button.with_enabled(false),
    }
}

/// The bookmarked places, as [`crate::resource::FileExplorer::children`]
/// publishes them: each opens in the window.
pub(crate) fn published_place_resources(favorites: &[favorites::Favorite]) -> Vec<Resource> {
    favorites
        .iter()
        .map(|favorite| {
            let resource_id = crate::search_provider::place_resource_id(&favorite.path);
            let open = crate::effects::row_action(
                &crate::effects::FilesEffect::OpenItem {
                    item: resource_id.clone(),
                },
                crate::search_provider::OPEN_ACTION,
                "Open",
            );
            Resource::new(resource_id, "files.place", favorite.label.clone()).with_action(open)
        })
        .collect()
}

/// Template create Effects, rows of the window.
/// The catalogue is the user's Templates directory, not the live folder listing.
pub(crate) fn published_template_resources(templates: Vec<PathBuf>) -> Vec<Resource> {
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
            Resource::new(
                template_id,
                "files.template",
                templates::menu_label(&template),
            )
            .with_action(create)
        })
        .collect()
}

/// The favourites `places` name on this device, in order; places this device
/// cannot resolve (a home it does not have) are left out.
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

fn transfer_phase_label(phase: fs_protocol::TransferPhase) -> String {
    t(match phase {
        fs_protocol::TransferPhase::Idle => "files-transfer-idle",
        fs_protocol::TransferPhase::Queued => "files-transfer-queued",
        fs_protocol::TransferPhase::Downloading => "files-transfer-downloading",
        fs_protocol::TransferPhase::Uploading => "files-transfer-uploading",
        fs_protocol::TransferPhase::Verifying => "files-transfer-verifying",
    })
}

fn grant_status_label(status: fs_protocol::ShareGrantStatus) -> String {
    t(match status {
        fs_protocol::ShareGrantStatus::Active => "files-grant-active",
        fs_protocol::ShareGrantStatus::Expired => "files-grant-expired",
        fs_protocol::ShareGrantStatus::Revoked => "files-grant-revoked",
        fs_protocol::ShareGrantStatus::Exhausted => "files-grant-exhausted",
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

fn share_permission_label(permission: fs_protocol::FilePermission) -> String {
    match permission {
        fs_protocol::FilePermission::View => t("files-share-choice-view"),
        fs_protocol::FilePermission::Edit => t("files-share-choice-edit"),
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

fn named_delivery_status_key(delivery: fs_protocol::NamedShareDeliveryStateV1) -> &'static str {
    match delivery {
        fs_protocol::NamedShareDeliveryStateV1::Pending => "files-share-created-pending",
        fs_protocol::NamedShareDeliveryStateV1::Delivered => "files-share-created-delivered",
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
            local_clipboard: true,
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
    /// Lists and launches "Open with" applications; absent means the
    /// submenu offers nothing rather than guessing.
    pub open_with: Option<crate::open_with::SharedOpenWithRoute>,
    /// The host supplies the existing native Files clipboard lane. This is
    /// availability, never a filesystem grant or an invocation authority.
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
pub struct FileExplorerWidget {
    framework_history: ui::history_mode::checked_files::FileHistorySource,
    framework_history_binding: Option<(PathBuf, fs_protocol::FileRecordId)>,
    services: FileServices,
    startup_accounts: Option<Vec<crate::remote_accounts::RemoteAccount>>,
    current_path: PathBuf,
    home_path: Option<PathBuf>,
    entries: Vec<Entry>,
    /// Multi-selection over the displayed entry list.
    selection: SelectionModel,
    shown_selection: shown_selection::ShownSelection,
    shown_folder: Option<Arc<host_fs::RetainedObject>>,
    /// Whether the folder shown is a folder on the disk, as its last read
    /// found: not Recent, not inside an archive, not a failed read.
    shown_is_folder: bool,
    /// The read of the folder shown still outstanding, if any.
    folder_reads: folder_read::FolderReads,
    local_clipboard: clipboard_client::Client,
    selected_open: selected_open::Client,
    error: Option<String>,
    is_loading: bool,
    /// Reads the archive being browsed off the window's thread.
    archive_listing: crate::archive::Listing,
    /// Files being read out of archives to be opened.
    archive_members: ArchiveMembers,
    /// Archives waiting for the folder Extract to… asks for.
    extract_chooser: Option<(Vec<PathBuf>, ui::portal::PortalChannel)>,
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
    /// The card over the file view for a change to take back or a failure
    /// to act on ([`notice`]).
    notice: Notice,
    /// A copy or move waiting for the answer to a name clash.
    clash: Option<PendingClash>,
    /// The selected file shown large (Space).
    preview: Option<Preview>,
    /// Files another app is dropping here, gathered until the drop ends.
    incoming: Vec<PathBuf>,
    /// The window's undo, as the host publishes it: what the Undo in a
    /// notice and in the menu asks for, the same one Ctrl+Z does.
    undo: ui::UndoAccess,
    /// The start of a name being typed into the listing.
    typing: ui::type_ahead::TypeAhead,
    /// The device this window changes the filesystem of, which carries out
    /// the operations it queues; `None` outside a framework host.
    device: Option<String>,
    /// Carries out the operations queued for this device.
    ops_drain: Option<ui::framework::Projection>,
    /// Operations this window queued, and what it shows when they settle.
    awaiting_ops: Vec<(crate::file_ops::FileOp, OpFollowUp)>,
    /// The callers of queued operations, each answered once its operation
    /// settles: a Paste is done when its files are, not when it is queued.
    op_replies: Vec<(crate::file_ops::FileOp, ui::framework::EffectReply)>,
    /// The operation the effect just started queued, for the reply
    /// [`Self::await_effect`] is handed next.
    answering_op: Option<crate::file_ops::FileOp>,
    /// Operations already seen carried out, so each settles once here.
    settled_ops: std::collections::HashSet<String>,
    ops_presented: bool,
    /// What operations came to when this window carried them out itself
    /// (no framework hosts it), for their reverts.
    local_outcomes: HashMap<String, crate::file_ops::Outcome>,
    /// Transient status line (operation results), shown in the footer.
    status: Option<String>,
    /// The next poll of what no change signal covers (see
    /// [`metadata_refresh`]): a directory its watch does not fully cover,
    /// the workspace record catalogue, an unwatched identity projection.
    /// `None` when every source has a change signal, so an idle window with
    /// an unchanged directory has no deadline at all.
    next_metadata_refresh: Option<Instant>,
    /// Reports changes to the open directory's entries and wakes the window.
    directory_watch: crate::directory_watch::DirectoryWatch,
    /// How many items each listed folder holds, counted off the UI thread;
    /// an arrival wakes the window.
    folder_counts: Arc<crate::folder_counts::FolderCounts>,
    /// Reports changes to the trusted-identity projection's directory, or,
    /// while it does not exist, its creation.
    identity_watch: crate::directory_watch::AwaitedDirectoryWatch,
    /// The directory watch reported a change that has not been read yet
    /// (held while a rename or pin drag defers the read).
    directory_changed: bool,
    /// When the open directory was last read, for [`metadata_refresh::change_read_gap`].
    last_directory_read: Instant,
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
    /// The window shell: `[ places | file view ]` under one header row.
    /// Held persistently (NOT rebuilt inside `build_layout`) so its docked
    /// state and eased reveal survive the per-structure-change rebuild of the
    /// panes — the two panes are swapped in via `NavShell::set_sidebar` /
    /// `set_content` each rebuild. A narrow window turns the places list into
    /// a drawer the shell summons.
    nav: NavShell,
    /// The app's side of the toolbar the shell's content header owns: the nav
    /// cluster, the editable path bar and the view / sort controls. The toolbar
    /// itself lives for the window's lifetime (the path bar carries live edit
    /// state — the in-progress typed path and its completion popover), and
    /// this handle feeds it the context and reads its control rects back.
    toolbar: crate::toolbar::ToolbarHandle,
    /// The content header is in the window's bar ([`ui::window_toolbar`])
    /// rather than in the window.
    header_in_bar: bool,
    /// What the person is typing into the location in the window's bar, as
    /// the bar last said; `None` while it is not edited there.
    bar_query: Option<String>,
    /// What the bar's slots were last declared from.
    declared_bar: Option<window_bar::BarKey>,
    /// Transient floating status bubble (bottom-right) — the item count /
    /// operation status + selection summary, surfaced on a real change or on
    /// hover then faded, in place of a permanent bottom footer strip so the
    /// file list spans to the window bottom.
    status_bubble: ui::StatusBubble,
    shots_status: bool,
    staged_hover: Option<usize>,
    staged_move_drag: bool,
    /// The user's template files, for the "New File" menus and the
    /// published rows; read from memory on the frame loop.
    templates: templates::Catalog,
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
    /// Rows a search hit or an agent's `open` asked to show, by their ids.
    opened: Rc<RefCell<Vec<String>>>,
    /// Set when applying a command changed structure; consumed by
    /// `take_structure_dirty` so the App rebuilds the composition.
    structure_dirty: bool,
    /// Where bookmark edits go: the resource's dispatcher. `None` for a
    /// widget no framework hosts (a shell's embedded tile, a unit test),
    /// whose bookmark edits stay in the window.
    dispatcher: Option<ui::framework::Dispatcher<crate::resource::FileExplorer>>,
    /// The person's view preferences; `None` outside a framework host, where
    /// they stay in the window.
    preferences: Option<ui::framework::Setting<crate::resource::FilesPreferences>>,
    /// The preferences last shown, so a change from another window or device
    /// is adopted once.
    shown_preferences: Option<crate::resource::FilesPreferences>,
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
    /// Last cursor position in logical coords (for menu placement + the
    /// context-menu hit-test).
    last_cursor: Point,
    /// Shared, persistent favourites + drag-reorder state (see
    /// [`crate::places_sidebar`]). Cloned into the sidebar each rebuild.
    favs: FavStore,
    /// The places the shell's list was last built from, and the row chosen in
    /// it.
    places_shown: Option<(Vec<ui::widget::NavSection>, Option<String>)>,
    /// The row the person last chose in the places list itself, so the
    /// navigation that follows does not rebuild a list that already shows it.
    places_choice: Rc<RefCell<Option<String>>>,
    /// A shared places row asked for its own context menu.
    places_context: Rc<RefCell<Option<(PathBuf, Point)>>>,
    /// A capture shows a favourite being dragged in the places list.
    stage_favorite_drag: bool,
    /// Per-user config dir for persisting favourites + zoom (`None` when the
    /// environment has no config dir — the UI still works, just doesn't persist).
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
    places_search: crate::search_provider::PlacesSearch,
    collaborative_search: crate::search_provider::CollaborativeRecordSearch,
    /// One bounded mount page is admitted at a time. `RefCell` lets opening
    /// global search advance a continuation without making its query path
    /// rescan or materialise the whole workspace.
    collaborative_record_pager: RefCell<crate::model::WorkspaceRecordIndexPager>,
    remote_accounts_search: crate::search_provider::RemoteAccountSearch,
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
    /// A plain press inside a multi-selection: the row it collapses the
    /// selection to on release, unless a drag of the whole set starts first.
    collapse_on_release: Option<usize>,
    /// The folder a drag has rested on, and since when: held long enough,
    /// it opens, so a drop can reach a folder deeper down.
    spring: Option<(PathBuf, Instant)>,
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
    /// Where the sheet's parts landed at the last paint.
    collab_layout: Option<ui::widget::ModalLayout>,
    /// Single background share request, run on the app runtime. It never
    /// touches widget state; update/paint/event polling adopts its bounded
    /// result on the UI thread.
    share_result: ui::LatestRequest<Result<crate::sharing::ShareActionResult, String>>,
    share_pending_path: Option<PathBuf>,
    /// Admission of a received bearer link, run on the app runtime.
    received_share: ui::LatestRequest<Result<fs_protocol::RecipientSubscriptionStatusV1, String>>,
    /// Host-projected names keyed by certified human roots. Labels affect
    /// presentation only; every command and request continues to carry the
    /// root key.
    trusted_humans: crate::identity_directory::TrustedHumanDirectory,
    history_result: Arc<std::sync::Mutex<Option<Result<(), String>>>>,
    history_pending_path: Option<PathBuf>,
    history_pending_success: Option<String>,

    merge_preflight_result: MergePreflightResult,
    merge_preflight_pending: Option<(uuid::Uuid, fs_protocol::FileVersionId)>,
    /// The job of an effect still running (a share link, a nearby send),
    /// answered when its work settles.
    effect_reply: Option<ui::framework::EffectReply>,
    /// Settings' config root remote accounts are read from; `None` reads
    /// none (embedded and test views), so no ambient store is touched.
    accounts_root: Option<PathBuf>,
    /// The accounts as last read from `accounts_root`.
    accounts_cache: Vec<crate::remote_accounts::RemoteAccount>,
    cloud: cloud_browse::CloudView,
    /// Explicit consent injection for protocol fixtures, never ambient authority.
    #[cfg(test)]
    cloud_fixture_consent: bool,
    mounted_cloud: mounted_cloud::MountedCloud,
    /// NTFS drives the shell can open, for the places list.
    drives: drives::Drives,
    /// The person's apps, for the Applications place.
    applications: applications::Applications,
    host_intent: Option<ui::widget::AppHostIntent>,
    /// A one-shot shell-owned document admission. The reply is strong only
    /// while this Files widget still owns the request; the shell gets a weak
    /// witness and cannot act after this widget has gone away.
    pending_document_open: Option<(u64, Rc<RefCell<ui::widget::DocumentOpenReply>>)>,
    /// Files waiting to be opened while the host answers an earlier one.
    open_queue: std::collections::VecDeque<PathBuf>,
    /// The file [`Self::pending_document_open`] is opening, to name it if
    /// the host refuses.
    document_open_path: Option<PathBuf>,
    /// A file handed to the desktop's default handler, until it answers.
    pending_launch: Option<(PathBuf, ui::portal::LaunchChannel)>,
    next_document_open_token: u64,
    /// Folder rows the file view publishes each layout, so a drag's drop point
    /// can be resolved to the folder it lands on (the view never sees the
    /// release — the app captures the pointer for the duration of the drag).
    drop_targets: crate::views::file_view::DropTargets,
    /// Every visible item's rect, for a box selection.
    item_rects: crate::views::file_view::ItemRects,
    /// A box selection being drawn: where it started, where the pointer
    /// is, and what was selected before it (kept with Ctrl).
    marquee: Option<(Point, Point, Vec<usize>)>,
    /// The content pane's live scroll position, owned here so it survives the
    /// per-frame rebuild. Each rebuilt list/grid `Scrollable` binds to this
    /// cell (see [`ui::widget::Scrollable::with_persisted_offset`]); without
    /// it, selecting a row, typing in search, or changing sort — all of which
    /// rebuild the composition — would snap the list back to the top. Reset to
    /// `(0, 0)` whenever the directory changes (a new folder starts at top).
    scroll_offset: Rc<Cell<(f32, f32)>>,
    /// The file list's table, kept across rebuilds with its scroll position,
    /// column widths and scroll anchor (the list scrolls itself; only the grid
    /// scrolls under `scroll_offset`).
    list_slot: crate::views::file_list::ListSlot,
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
    /// The Recent list of a window no framework hosts.
    recent: crate::recent::RecentFiles,
    /// This device's Recent list, in a hosted window.
    recent_setting: Option<ui::framework::Setting<crate::recent::RecentFiles>>,
    /// "Open With" candidates per file type, asked of the host once per
    /// listing rather than once per row.
    open_with_cache: RefCell<HashMap<String, crate::open_with::OpenWithCandidates>>,
    /// The listing's entries by resource id, made once per listing.
    entry_ids: RefCell<EntryIds>,
    /// The entry rows last published, kept while what each shows holds.
    entry_rows: RefCell<entry_rows::EntryRows>,
    /// The shared searchable picker behind "Other application…"; its rows are
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
        let mut widget =
            Self::with_persistence(config::config_dir().as_deref(), crate::model::user_home());
        widget.accounts_root = accounts::config_root();
        widget.refresh_external_search_catalogs();
        widget
    }

    /// Take part in the resource: bookmark edits go through `dispatcher`,
    /// and the view follows the person's `preferences`.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::FileExplorer>,
        preferences: ui::framework::Setting<crate::resource::FilesPreferences>,
        recent: ui::framework::Setting<crate::recent::RecentFiles>,
        device: Option<String>,
        ops_drain: Option<ui::framework::Projection>,
    ) {
        self.dispatcher = Some(dispatcher);
        self.preferences = Some(preferences);
        self.recent_setting = Some(recent);
        self.device = device;
        self.ops_drain = ops_drain;
        self.adopt_preferences();
    }

    /// Change the filesystem: queue `op` in the Files document, where it is
    /// in history and undoable, for this device to carry out; `follow` is
    /// what the window shows when it settles. A window no framework hosts
    /// carries it out at once.
    ///
    /// What it came to when carried out at once; `None` when queued.
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

    /// Show what the operations carried out since the last presentation
    /// came to: the ones this window queued as it asked, any other (an
    /// agent's, another window's, an undo) as a refreshed listing.
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
                // The editor stays open so the name can be fixed.
                Some(error) => {
                    self.notify_failure(t("files-rename-failed-title"), &error, None);
                }
            },
            OpFollowUp::Trash { count } => match error {
                None => {
                    // Undo puts back only what Files could tell apart in
                    // Trash; anything else is offered no Undo it can't keep.
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

    /// Select what an operation wrote, when it is in the folder shown.
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

    /// Give the notice the pointer events that are its: hover, and a press
    /// and release on the card. Whether the event was the notice's.
    fn route_notice_event(&mut self, event: &winit::event::WindowEvent) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        match event {
            WindowEvent::CursorMoved { .. } => {
                self.structure_dirty |= self.notice.pointer_moved(self.last_cursor);
                self.notice.contains(self.last_cursor)
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.notice.press(self.last_cursor),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => match self.notice.release(self.last_cursor) {
                Some(click) => {
                    if let NoticeClick::Action(action) = click {
                        self.act_on_notice(action);
                    }
                    self.structure_dirty = true;
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    /// Say that a change is done, with Undo when it can be taken back. Only
    /// a change worth taking back is announced: one the listing alone would
    /// not show, like files leaving the folder.
    fn notify_done(&mut self, title: String, undoable: bool) {
        self.status = None;
        // Undo is the window's: offered only when the window has it to give
        // (a window no framework hosts keeps no history).
        let undo = undoable && self.undo.can_undo();
        self.notice.show(
            title,
            "",
            ui::widget::toast::ToastKind::Success,
            undo.then_some(NoticeAction::Undo),
        );
        self.structure_dirty = true;
    }

    /// Say that something failed: `title` says what, `reason` why, and
    /// `action` is the way on, when there is one.
    fn notify_failure(&mut self, title: String, reason: &str, action: Option<NoticeAction>) {
        if reason == crate::file_ops::CANCELLED {
            // The person stopped it: what got through can be taken back.
            self.notify_done(t("files-transfer-stopped"), true);
            return;
        }
        self.status = None;
        self.notice.show(
            title,
            crate::fileops::readable_error(reason),
            ui::widget::toast::ToastKind::Error,
            action,
        );
        self.structure_dirty = true;
    }

    /// A new change makes an Undo notice stale: its Undo would take back
    /// the newer change instead of the one it names.
    fn retire_stale_undo_notice(&mut self) {
        if self.notice.offers_undo() {
            self.notice.dismiss();
            self.structure_dirty = true;
        }
    }

    /// Act on a notice's button.
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
        }
        self.structure_dirty = true;
    }

    /// Say why any of `count` weren't pasted; after Cut, take the moved ones
    /// off the clipboard (the ones that failed stay, to retry).
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
        // What was pasted shows up in the listing; only a failure is news.
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

    /// Filesystem changes bound to the entry row `row`: rename, move to a
    /// folder, move to Trash — the resource's queued, undoable verbs with
    /// this device and the entry bound, for an agent acting on the row.
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
            .declaration("rename", t("files-menu-rename"));
        let move_to = FileExplorer::MOVE_ENTRIES
            .bind(file_explorer::MoveEntries::paths().device, device.clone())
            .and(file_explorer::MoveEntries::paths().from, String::new())
            .and(
                file_explorer::MoveEntries::paths().entries,
                vec![path.clone()],
            )
            .declaration("move-to", "Move to folder…");
        let trash = FileExplorer::TRASH
            .bind(file_explorer::Trash::paths().device, device)
            .and(file_explorer::Trash::paths().folder, String::new())
            .and(file_explorer::Trash::paths().entries, vec![path])
            .declaration("trash", t("files-menu-move-trash"))
            .map(|mut trash| {
                trash.destructive = true;
                trash
            });
        [rename, move_to, trash].into_iter().flatten().collect()
    }

    /// Filesystem changes an agent or a script asks for in the folder shown:
    /// the resource's queued, undoable verbs with this device and the folder
    /// bound, naming only entries the folder holds.
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
        let folder = self.current_path.display().to_string();
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
            .and(file_explorer::NewFolder::paths().parent, folder.clone())
            .declaration("new-folder-here", "New folder here");
        let rename = FileExplorer::RENAME
            .bind(file_explorer::Rename::paths().device, device.clone())
            .and(file_explorer::Rename::paths().folder, folder.clone())
            .declaration("rename-here", "Rename an entry here");
        let trash = FileExplorer::TRASH
            .bind(file_explorer::Trash::paths().device, device.clone())
            .and(file_explorer::Trash::paths().folder, folder.clone())
            .declaration("trash-here", "Move entries here to Trash");
        let moving = FileExplorer::MOVE_ENTRIES
            .bind(file_explorer::MoveEntries::paths().device, device.clone())
            .and(file_explorer::MoveEntries::paths().from, folder.clone())
            .declaration("move-here", "Move entries from here to a folder");
        let copying = FileExplorer::COPY_INTO
            .bind(file_explorer::CopyInto::paths().device, device.clone())
            .and(file_explorer::CopyInto::paths().from, folder.clone())
            .declaration("copy-here", "Copy entries from here to a folder");
        let writing = FileExplorer::WRITE_FILE
            .bind(file_explorer::WriteFile::paths().device, device.clone())
            .and(file_explorer::WriteFile::paths().folder, folder.clone())
            .declaration("write-file-here", "Write a file here");
        let compressing = FileExplorer::COMPRESS
            .bind(file_explorer::Compress::paths().device, device.clone())
            .and(file_explorer::Compress::paths().from, folder.clone())
            .declaration(
                "compress-here",
                "Compress entries here to a zip or tar.zst archive",
            );
        let extracting = FileExplorer::EXTRACT
            .bind(file_explorer::Extract::paths().device, device)
            .and(file_explorer::Extract::paths().to, Some(folder))
            .declaration("extract-here", "Extract an archive into this folder");
        [
            new_folder,
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
        .map(shown)
        .collect()
    }

    /// Show the bookmarked `places`, resolved on this device.
    pub(crate) fn present_places(&mut self, places: &[String]) {
        let favorites = resolve_places(places, self.home_path.as_deref(), &self.user_dirs);
        if self.favs.borrow().favorites == favorites {
            return;
        }
        self.favs.borrow_mut().favorites = favorites;
        self.refresh_places_search();
        self.structure_dirty = true;
    }

    /// The stored place for folder `path` on this device.
    fn place_for(&self, path: &Path) -> String {
        crate::places::place_of(path, self.home_path.as_deref(), &self.user_dirs)
    }

    /// Submit a bookmark verb. The sidebar already shows it; the stored
    /// places are presented back once it applies (or again unchanged if it
    /// is refused).
    fn submit(&mut self, invoke: ui::tilo_resource::Invoke<crate::resource::FileExplorer>) {
        self.retire_stale_undo_notice();
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(invoke);
        }
    }

    /// The view preferences this window shows.
    fn current_preferences(&self) -> crate::resource::FilesPreferences {
        crate::resource::FilesPreferences {
            view: crate::resource::view_mode_token(self.view_mode).to_owned(),
            sort: crate::resource::sort_key_token(self.sort_by).to_owned(),
            order: crate::resource::sort_order_token(self.sort_order).to_owned(),
            zoom: self.zoom_level,
            hidden: self.show_hidden,
        }
    }

    /// Keep what the window now shows as the person's preferences.
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

    /// Show the person's preferences when they changed: in another window,
    /// on another device, in Settings.
    fn adopt_preferences(&mut self) {
        let Some(stored) = self.preferences.as_ref().map(ui::framework::Setting::get) else {
            return;
        };
        if self.shown_preferences.as_ref() == Some(&stored) {
            return;
        }
        self.shown_preferences = Some(stored.clone());
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
        // The places list shows the account too, as it would from the store.
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
            // The one thing to do, said once; Settings is where people sign in.
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

    /// Choose the cloud account row `id` in the places list: open its folder
    /// when it has one now, else say what is wrong with it.
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

    /// Construct with an explicit persistence directory + home, resolving the
    /// XDG user dirs from the real environment. `new` uses the real per-user
    /// config dir; tests pass a `tempdir` (or `None`) so they never read or
    /// write the user's real preview cache.
    #[doc(hidden)]
    pub fn with_persistence(config_dir: Option<&Path>, home: Option<PathBuf>) -> Self {
        Self::with_persistence_dirs(config_dir, home, favorites::UserDirs::from_system())
    }

    /// Construct with an explicit persistence directory, home, and injected
    /// XDG user directories. Tests inject a known (e.g. German-localized) set
    /// so the standard favourites are deterministic without depending on the
    /// build machine's locale.
    #[doc(hidden)]
    pub fn with_persistence_dirs(
        config_dir: Option<&Path>,
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

    /// The window a screenshot shows: the scene's made-up home and folders,
    /// previews cached in the scene, and nothing read from the machine the
    /// capture is taken on.
    pub fn for_scene(scene: &crate::scenarios::Scene) -> Self {
        enable_pdf_previews();
        let thumbnails = ThumbnailService::open(&scene.thumbnails, 4)
            .ok()
            .map(Arc::new);
        let mut widget = Self::with_inputs(
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
                document_opener: DocumentOpenRoute::Unavailable,
                open_with: None,
                local_clipboard: false,
            }),
        );
        widget.templates =
            templates::Catalog::new(Some(scene.home.join("Templates")), ui::WakeHandle::new());
        if scene.start != scene.home {
            widget.navigate(scene.start.clone());
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
        // The standard places until the resource presents the stored ones;
        // a widget no framework hosts keeps them.
        let favorites = home
            .as_deref()
            .map(|home| favorites::defaults(home, &user_dirs))
            .unwrap_or_default();
        let show_hidden = false;
        let workspace_roots = inputs.workspace_roots;
        // Construct each durable namespace once. Subsequent refreshes are
        // revision-gated by `refresh_external_search_catalogs`, never by palette input.
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
        // The places list and the file view are swapped in on the first
        // `build_layout`; the toolbar is the content header's one action and
        // stays for the window's lifetime.
        let (path_widget, actions_widget, toolbar) = crate::toolbar::FileToolbar::new(cmds.clone());
        // window-bar: fallback, the header the bar takes over once it hosts the window
        let nav = NavShell::new()
            .content_kind(ui::widget::ContentKind::List)
            .with_id("files.shell")
            .content_header("", vec![Box::new(actions_widget)])
            .content_header_leading(Box::new(path_widget));
        let thumbs = match &services {
            FileServices::Legacy => build_thumb_service(config_dir).map(Arc::new),
            FileServices::Hosted(hosted) => hosted.thumbnails.clone(),
        };
        let trusted_humans = inputs.trusted_humans;
        let startup_accounts = injected.then_some(remote_accounts);
        let state_retry = recovery_button(true);
        let mut widget = Self {
            services,
            startup_accounts,
            current_path: cwd.clone(),
            home_path: home,
            // Shown once the first read of `cwd` is (see below).
            entries: Vec::new(),
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
            // Scheduled by the first sync, once the watches exist.
            next_metadata_refresh: None,
            directory_watch: crate::directory_watch::DirectoryWatch::new(ui::WakeHandle::new()),
            folder_counts: {
                let wake = ui::WakeHandle::new();
                Arc::new(crate::folder_counts::FolderCounts::new(move || wake.wake()))
            },
            identity_watch: crate::directory_watch::AwaitedDirectoryWatch::new(
                ui::WakeHandle::new(),
            ),
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
            rename_select_pending: false,
            rect: vello::kurbo::Rect::ZERO,
            // Placeholder panes; the real places list + file view are swapped
            // in on the first `build_layout`. The places list is the shell's
            // *sidebar*, not a content overview: docked beside the file view
            // when wide, an overlay drawer over it when narrow. Sidebar width
            // matches the historical fixed sidebar so the docked layout is
            // pixel-stable.
            nav,
            toolbar,
            header_in_bar: false,
            bar_query: None,
            declared_bar: None,
            status_bubble: ui::StatusBubble::new(),
            state_spinner: Spinner::new(),
            state_retry,
            shots_status: false,
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
            context_menu: ContextMenu::new(),
            menu_commands: Vec::new(),
            submenu: ContextMenu::new(),
            submenu_commands: Vec::new(),
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
            effect_reply: None,
            local_clipboard: clipboard_client::Client::new(),
            selected_open: selected_open::Client::default(),
            accounts_root: None,
            accounts_cache: Vec::new(),
            cloud: cloud_browse::CloudView::default(),
            #[cfg(test)]
            cloud_fixture_consent: false,
            mounted_cloud: mounted_cloud::MountedCloud::default(),
            drives: drives::Drives::default(),
            applications: applications::Applications::default(),
            host_intent: None,
            pending_document_open: None,
            open_queue: std::collections::VecDeque::new(),
            document_open_path: None,
            pending_launch: None,
            next_document_open_token: 0,
            drop_targets: Rc::new(std::cell::RefCell::new(Vec::new())),
            item_rects: Rc::new(std::cell::RefCell::new(Vec::new())),
            marquee: None,
            scroll_offset: Rc::new(Cell::new((0.0, 0.0))),
            list_slot: crate::views::file_list::ListSlot::default(),
            thumbs,
            recent: crate::recent::RecentFiles::default(),
            recent_setting: None,
            open_with_cache: RefCell::new(HashMap::new()),
            entry_ids: RefCell::default(),
            entry_rows: RefCell::default(),
            // window-bar: picker, the Open with chooser
            app_chooser: ui::widget::command_palette::CommandPalette::commands(),
            app_chooser_target: None,
            thumb_imgs: Rc::new(RefCell::new(HashMap::new())),
        };
        // Nothing is shown yet: until the first read is, that is the
        // loading state.
        widget.is_loading = true;
        widget.read_shown_folder(cwd, Then::Nothing);
        widget
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

    /// The listed entry whose resource id is `id`
    /// ([`Self::entry_resource_id`]), through ids made once per listing:
    /// finding a row's entry hashed the path of every entry before it, and
    /// a publication asks it of every row.
    fn entry_by_resource_id(&self, id: &str) -> Option<&Entry> {
        self.entry_ids.borrow_mut().find(&self.entries, id)
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
        self.entry_by_resource_id(resource_id)
            .map(|entry| entry.path.clone())
    }

    /// Recent actions name only a currently presented, still-existing row.
    /// A retained address cannot navigate or edit another location's history.
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

    /// Do `effect`: show a folder, copy or paste, open or share a file. A job (sending to a nearby device, creating a share link)
    /// answers later through the reply [`Self::await_effect`] keeps.
    ///
    /// # Errors
    /// Why nothing (or not everything) was done.
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
                let path = PathBuf::from(path);
                if !path.is_absolute() {
                    return Err(format!("{} is not an absolute path", path.display()));
                }
                if !path.is_dir() {
                    return Err(format!("{} is not a folder Files can open", path.display()));
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
                self.open_terminal()
                    .map_err(|e| format!("could not open terminal: {e}"))?;
                None
            }
            FilesEffect::EmptyTrash => {
                if !self.is_trash_view() {
                    return Err("Empty Trash from the Trash folder".into());
                }
                if self.has_native_trash_entries() {
                    return Err(t("files-native-trash-delete-unavailable"));
                }
                // The effect is confirmed by its contract; no second sheet.
                self.perform_empty_trash();
                None
            }
            FilesEffect::OpenItem { item } => {
                // Resolved only against what the window shows now, so a
                // deleted, renamed or replayed row cannot regain authority
                // or disclose a path.
                let target = self
                    .visible_open_path(&item)
                    .ok_or_else(|| "file location is no longer available".to_string())?;
                self.open_path(target);
                None
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
                    .ok_or_else(|| "the template is no longer available".to_string())?;
                self.create_from_template(&target)?;
                None
            }
            FilesEffect::OpenWith {
                entry,
                app,
                always,
                access,
            } => {
                if let Some(access) = access {
                    if !self.retained_selected_open_available() && !self.native_selected_discovery()
                    {
                        return Err(t("files-open-with-launch-unavailable"));
                    }
                    let access = match access.as_str() {
                        "read-only" => {
                            ui::collaboration::open_selected::v2::SelectedAccess::ReadOnly
                        }
                        "edit-in-parent" => {
                            ui::collaboration::open_selected::v2::SelectedAccess::EditInParent
                        }
                        _ => return Err(t("files-open-with-access-unavailable")),
                    };
                    return self.begin_selected_open_v2(
                        &entry,
                        &app,
                        invocation,
                        always.unwrap_or(false),
                        access,
                    );
                }
                if self.retained_selected_open_available()
                    && self
                        .visible_entry_path(&entry)
                        .is_some_and(|path| tilo_files::native::retained_home::contains(&path))
                {
                    return self.begin_selected_open(
                        &entry,
                        &app,
                        invocation,
                        always.unwrap_or(false),
                    );
                }
                self.invoke_open_with(&entry, &app, always.unwrap_or(false))?;
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
            FilesEffect::Sharing { entry } => {
                self.entry_command(&entry, |_, _| Some(FileCommand::ShowSharing))?
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
                    .ok_or_else(|| "file entry is no longer visible".to_string())?;
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
            FilesEffect::ShareLink { entry } => {
                self.refuse_while_answering()?;
                let target = self
                    .visible_entry_path(&entry)
                    .ok_or_else(|| "file entry is no longer visible".to_string())?;
                self.select_entry(&target)?;
                if !self.apply_command(FileCommand::CreateOneTimeShareLink)
                    || self.share_pending_path.is_none()
                {
                    return Err(self
                        .status
                        .clone()
                        .unwrap_or_else(|| "sharing is unavailable for this file".into()));
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

    /// Do `effect` through the door every caller uses: submitted to the
    /// framework host, which answers it with [`Self::run_effect`] like a
    /// palette's or an agent's call. A widget no framework hosts (a shell's
    /// embedded tile) does it at once.
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

    /// A job answers its caller later; one runs at a time.
    fn refuse_while_answering(&mut self) -> Result<(), String> {
        self.retire_cancelled_effect();
        if self.effect_reply.is_some() {
            return Err("another file operation is still running".into());
        }
        Ok(())
    }

    /// A cancelled caller cannot acknowledge a transfer. Retire its local
    /// polling state while the host retains the durable recovery receipt.
    fn retire_cancelled_effect(&mut self) {
        if self
            .effect_reply
            .as_ref()
            .is_some_and(ui::framework::EffectReply::is_cancelled)
        {
            self.effect_reply.take();
            self.local_clipboard.abandon();
            self.selected_open.abandon();
            self.status = Some(t("files-clipboard-cancelled"));
            self.structure_dirty = true;
        }
    }

    /// Keep `reply` until the job [`Self::run_effect`] started settles.
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

    /// Answer the caller waiting on `op`, now that it settled as `outcome`.
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

    /// Answer the job awaiting its result, if one is.
    fn answer_effect(&mut self, result: Result<(), tilo_actions::ActionFailure>) {
        if let Some(reply) = self.effect_reply.take() {
            reply.answer(result.map(|()| tilo_actions::ActionOutput::default()));
        }
    }

    /// Select the entry at `target`, as acting on its row does.
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

    /// Run the menu command `command` builds for the entry `entry` names,
    /// with that entry selected, as its context menu does.
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

    /// The remote account `id` names.
    fn account_named(&self, id: &str) -> Result<crate::remote_accounts::RemoteAccount, String> {
        self.remote_account(id)
            .ok_or_else(|| "remote account is no longer available".to_string())
    }

    fn importable_cloud_entry(&self, path: &Path) -> bool {
        entry_rows::importable_under(path, &self.import_mounts())
    }

    /// Where the accounts a converted file can be imported once from are
    /// mounted: Google Drive and Dropbox accounts, not themselves imports,
    /// not disabled.
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

    /// Import uses the ordinary queued copy path: kernel reads hydrate the
    /// mounted source, while the destination contains independent local bytes.
    /// No credential, mount, pin or sync relationship is created here.
    fn import_once(
        &mut self,
        entry: &str,
        folder: &str,
    ) -> Result<Option<crate::file_ops::FileOp>, String> {
        let source = self
            .visible_entry_path(entry)
            .ok_or_else(|| "file entry is no longer visible".to_string())?;
        if !self.importable_cloud_entry(&source) {
            return Err(t("files-import-source-unavailable"));
        }
        let folder = Path::new(folder);
        // Validate the submitted spelling before canonicalization makes even a
        // relative path absolute. Retain the validated target, not its alias,
        // when the document outbox executes the copy on a later frame.
        if !folder.is_absolute() || !folder.is_dir() {
            return Err(format!("{} is not an existing folder.", folder.display()));
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

    /// Copy the entry `entry` names into `folder`, keeping its name: a
    /// queued, undoable operation.
    fn copy_into_folder(
        &mut self,
        entry: &str,
        folder: &str,
    ) -> Result<Option<crate::file_ops::FileOp>, String> {
        let source = self
            .visible_entry_path(entry)
            .ok_or_else(|| "file entry is no longer visible".to_string())?;
        let folder = PathBuf::from(folder);
        if !folder.is_absolute() || !folder.is_dir() {
            return Err(format!("{} is not an existing folder.", folder.display()));
        }
        self.queue_copy(crate::file_ops::FileOp::Copy {
            paths: vec![source.display().to_string()],
            folder: folder.display().to_string(),
        })
    }

    /// Queue the copy of one entry `op`: the operation queued, `None` when
    /// it was carried out at once.
    fn queue_copy(
        &mut self,
        op: crate::file_ops::FileOp,
    ) -> Result<Option<crate::file_ops::FileOp>, String> {
        let follow = OpFollowUp::Paste {
            count: 1,
            cut: false,
        };
        match self.queue(op.clone(), follow) {
            Some(outcome) => outcome
                .reason()
                .map_or(Ok(None), |error| Err(format!("copy failed: {error}"))),
            None => Ok(Some(op)),
        }
    }

    /// What an effect that changed files came to: done when `queued` is
    /// `None` (carried out at once), else a job answered once the device
    /// carried the queued operation out, so a caller that hears it finished
    /// finds the files there.
    fn answer_when_settled(&mut self, queued: Option<crate::file_ops::FileOp>) -> Ran {
        match queued {
            Some(op) => {
                self.answering_op = Some(op);
                Ran::Pending
            }
            None => Ran::Done(None),
        }
    }

    /// Show the collaborative record `record` in its folder, selected.
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

    /// Accept (into `destination`) or decline an incoming nearby transfer.
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

    /// Refresh the persistent top bar with the current navigation context
    /// (path, history availability, view mode) before it is laid out.
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

    /// What the places list shows now: the Favorites and the locations.
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
                            // A folder that cannot be opened has no place to go to.
                            mount: account.mount.filter(|_| available),
                        }
                    })
                    .collect(),
            )
            .with_drives(self.drives.shown.iter().map(drives::drive_place).collect())
    }

    /// Put the **places** pane (the window's sidebar) in the shell. The list
    /// keeps its own keyboard focus and scroll, so it is replaced only when
    /// what it shows changes: its places, or the chosen one when the choice
    /// did not come from the list itself.
    fn refresh_places_pane(&mut self) {
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
        self.nav.set_sidebar(Some(list.into_widget()));
        *self.places_choice.borrow_mut() = None;
        self.places_shown = Some((sections, selected));
    }

    /// The **content** pane (the responsive body's detail): the file list or
    /// grid (plus any error affordance), scrolling under a symmetric gutter.
    fn build_content_pane(&self, theme: &ui::Theme) -> Box<dyn Widget> {
        if self.cloud.active {
            return self.build_cloud_pane(theme);
        }
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
        // `renaming` marks the item edited in place, publishing its label rect
        // into `rename_rect` for the host to draw the shared editor over. The
        // cursor seeds the view's pointer position so a press hit-tests
        // correctly on the frame it is rebuilt (see `FileView::with_cursor`).
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
            .with_list(self.list_slot.clone())
            .with_bottom_inset(f64::from(status_bubble_safe_inset(theme)));
        // A plain (percent-height) column: this pane is the content column of
        // the shell, so it must FILL that rect (`percent(1.0)` height).
        let mut view_col = Column::new().with_vertical_padding(sp.s1);
        match self.view_mode {
            // The list is a table that scrolls itself and keeps the status
            // lane clear below its rows.
            ViewMode::List => {
                view_col = view_col.push(crate::views::list_view::build_list(
                    view,
                    self.sort_by,
                    self.sort_order,
                ));
            }
            // The grid scrolls: pin the scroll viewport to the *remaining*
            // height of the content column (grow-from-zero) rather than
            // letting it claim `percent(1.0)`, so a long directory overflows
            // the viewport and the wheel + scrollbar thumb move the grid.
            ViewMode::Grid => {
                view_col = view_col.push(
                    Scrollable::new(ScrollAxis::Vertical)
                        .fill_parent_height()
                        .with_persisted_offset(self.scroll_offset.clone())
                        .with_cursor(self.last_cursor)
                        // The status pill is intentionally outside the
                        // scrollable's interactive viewport. Besides making the
                        // count easy to scan, that means it cannot obscure a
                        // file name, target, or keyboard focus ring at the
                        // bottom of a compact grid.
                        .with_bottom_content_inset(status_bubble_safe_inset(theme))
                        .with_child(crate::views::grid_view::build_grid(view))
                        .into_widget(),
                );
            }
        }
        view_col.into_widget()
    }

    /// The label for the floating status bubble: an operation status, else the
    /// item count, plus the selection summary when something is selected. The
    /// current path is shown ONCE — in the path bar at the top — so it is
    /// deliberately not repeated here. Empty string means nothing to surface.
    fn footer_label(&self) -> String {
        if self.cloud.active {
            return t_args(
                "files-item-count",
                &[arg("count", i64_of_usize(self.cloud.entries.len()))],
            );
        }
        // A folder that did not open, or is still opening, has no count to
        // report: the state surface says what is happening.
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

    /// Concise, readable copy for the center state. The visual treatment has a
    /// deliberately finite line length, while the full path remains discoverable
    /// in the location bar and the root accessibility description.
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

    /// Why the folder shown couldn't be listed, and the system's words for
    /// it, when it couldn't.
    fn folder_failure(&self) -> Option<(crate::fileops::FolderFailure, &str)> {
        let error = self.error.as_deref()?;
        // The stored error is the localized "couldn't open" sentence around
        // the system's message; the cause is what follows its colon.
        let cause = error.rsplit_once(": ").map_or(error, |(_, cause)| cause);
        Some((crate::fileops::FolderFailure::of(cause), cause))
    }

    /// What a failed listing's button says.
    fn folder_recovery_label(&self) -> String {
        retry_row(self.folder_recovery_command() == FileCommand::Refresh).label
    }

    /// The command a failed listing's button runs.
    fn folder_recovery_command(&self) -> FileCommand {
        match self.folder_failure() {
            Some((failure, _)) if !failure.may_pass() => FileCommand::Up,
            _ => FileCommand::Refresh,
        }
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
        let rect = self.content_body_rect();
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
                    action_label: Some(&self.folder_recovery_label()),
                    error: true,
                    ..EmptyStateContent::new(&title)
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
                    title: &title,
                    message: Some(&detail),
                    ..EmptyStateContent::new(&title)
                },
                self.last_cursor,
            );
            let center = rect.center();
            let side = f64::from(theme.tokens.metrics.icon_lg) + f64::from(theme.tokens.spacing.s1);
            let spinner_rect = vello::kurbo::Rect::from_center_size(
                Point::new(
                    center.x,
                    center.y - f64::from(theme.tokens.spacing.s16)
                        + f64::from(theme.tokens.spacing.s1),
                ),
                (side, side),
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
        use ui::platform::{HostShortcut, PlatformConventions, Shortcut};
        let conventions = PlatformConventions::default();
        let keys = |shortcuts: &[Shortcut]| {
            shortcuts
                .iter()
                .map(|shortcut| shortcut.label(conventions))
                .collect::<Vec<_>>()
                .join(" / ")
        };
        // In the order a person works: get somewhere, change things, see
        // them differently.
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

    /// Spoken state for custom-painted surfaces which otherwise have no
    /// backing child widget. The stable accessible name on Files' root turns
    /// value changes into live announcements without stealing keyboard focus.
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

    /// `--shots`: stage the status bubble the way a change would, resting on
    /// it, so a single headless frame captures the floating chip.
    pub fn debug_show_status_bubble(&mut self) {
        let now = ui::time::now();
        let label = self.footer_label();
        self.status_bubble.show(&label, now);
        self.status_bubble.set_hover(true, now);
    }

    /// Paint the transient floating status bubble at the window's
    /// bottom-right, the reclaimed home of the old footer strip. It surfaces
    /// when there is news (a status) or while the pointer rests on its
    /// corner, then fades.
    fn draw_status_bubble(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        tr: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
    ) {
        let now = ui::time::now();
        // Only news pops the bubble. The item and selection counts follow
        // along and show when the pointer rests on the corner; selecting a
        // file never flashes a bubble.
        match &self.status {
            Some(status) => {
                self.status_bubble.show(status, now);
            }
            None => self.status_bubble.set_text(&self.footer_label()),
        }
        // Archive browsing is deliberately read-only. Unlike transient file
        // operation feedback, that fact remains visible for the whole visit.
        if self.shots_status || crate::archive::location(&self.current_path).is_some() {
            self.status_bubble.set_hover(true, now);
        } else {
            self.status_bubble
                .pointer_moved(self.rect, self.last_cursor, tr, theme, now);
        }
        self.status_bubble
            .paint(scene, transform, tr, theme, self.rect, now);
    }

    /// Whether the content column is narrower than the direct toolbar
    /// controls need; they move into the overflow when it is.
    fn sync_toolbar_compact(&self) {
        let column = self.nav.geometry().content_column;
        self.toolbar.set_content_width(column.width());
    }

    /// Whether `point` is over the places list (docked, or the places
    /// screen of a narrow window).
    fn in_places_list(&self, point: Point) -> bool {
        self.nav.is_sidebar_open() && self.nav.geometry().sidebar_column.contains(point)
    }

    /// The content column below the shell's header row: where the file view
    /// and the empty, loading and error surfaces stand.
    fn content_body_rect(&self) -> vello::kurbo::Rect {
        let geometry = self.nav.geometry();
        let column = geometry.content_column;
        vello::kurbo::Rect::new(
            column.x0,
            column.y0 + geometry.header_height,
            column.x1,
            column.y1,
        )
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
        let denied = std::io::Error::from_raw_os_error(13).to_string();
        self.state_retry = recovery_button(false);
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

    /// Navigate to `path`, pushing the current path onto the back-history.
    pub fn navigate(&mut self, path: PathBuf) {
        if path != self.current_path {
            self.history.push(self.current_path.clone());
            self.forward.clear();
        }
        self.load(path);
    }

    /// Go to the parent directory (history-aware).
    pub fn back(&mut self) {
        if self.is_recent_view() || self.is_applications_view() {
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
        // Settling a local copy must not navigate away from its source view.
        if !self.cloud.active {
            self.load(self.current_path.clone());
        }
    }

    /// Re-read the visible directory without treating it as navigation, when
    /// its watch reported a change or, where no watch covers it, when its
    /// poll is due. Stable record identity preserves selection across remote
    /// rename/move metadata; ordinary files fall back to their path. Scroll
    /// and transient operation status remain untouched.
    fn refresh_metadata_if_due(&mut self, now: Instant) -> bool {
        // The first sync after construction starts the watches; changes made
        // before a watch started are covered by one more read. A folder read starts the watch of the folder it reads; this
        // starts it only for a folder shown without one.
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
        // A directory already known to have changed is not drained again
        // until it is read: its events would only wake the window again.
        if !self.directory_changed && self.directory_watch.take_changes() {
            self.directory_changed = true;
        }
        let identity_signal = identities_watched || self.identity_watch.take_changes();
        let poll_due = self.next_metadata_refresh.is_some_and(|due| now >= due);
        if poll_due || self.next_metadata_refresh.is_none() {
            // Set the next deadline before doing any I/O.  A transient read
            // error must not accidentally turn a very large directory into a
            // busy loop.
            self.next_metadata_refresh = self.next_poll_interval().map(|interval| now + interval);
        }
        let catalog_changed = poll_due
            && self
                .collaborative_record_pager
                .borrow_mut()
                .poll_generation();
        // `sync` admits the one bounded page for this turn. Keep polling and
        // admission separate so an authority change cannot consume two pages
        // in one UI frame.
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
        // Drain before reading: a change after this point wakes the window
        // again and is read next time.
        let _ = self.directory_watch.take_changes();
        self.directory_changed = false;
        self.last_directory_read = now;
        self.refresh_metadata_preserving_view() || changed
    }

    /// A rename or pin drag defers reading the listing, and a read under way
    /// covers every change before it started.
    fn directory_read_deferred(&self) -> bool {
        self.renaming.is_some() || self.pin_drag.is_some() || self.folder_reads.is_pending()
    }

    /// The directory to watch for `path`'s listing: `None` for listings that
    /// are not one directory's entries (Recent, an archive, the hosted Trash
    /// that merges two directories), which are polled instead.
    fn watched_directory(&self, path: &Path) -> Option<PathBuf> {
        let virtual_listing = crate::recent::is_location(path)
            || crate::applications::is_location(path)
            || crate::archive::location(path).is_some()
            || (self.hosted_clipboard() && fileops::is_trash_dir(path));
        (!virtual_listing).then(|| path.to_path_buf())
    }

    /// How often to poll what no change signal covers, or `None` when
    /// nothing needs polling.
    fn next_poll_interval(&self) -> Option<Duration> {
        use crate::directory_watch::Coverage;
        let entries = self.entries.len();
        let directory = match self.directory_watch.coverage() {
            Coverage::Complete => None,
            // Local edits arrive as events; remote ones (tilo-fs gossip, a
            // network share) only by reading, as promptly as before.
            Coverage::Partial => Some(metadata_refresh::interval(entries)),
            Coverage::None => Some(metadata_refresh::unwatched_interval(entries)),
        };
        let catalog = self
            .collaborative_record_pager
            .borrow()
            .has_roots()
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
        // File history names its authors from this directory: rebind it.
        self.framework_history_binding = None;
        true
    }

    /// Re-read the current directory while preserving stable-record
    /// selection, scroll, and operation status. Collaborative controls use
    /// this path because their xattr response changes row metadata but is not
    /// navigation.
    fn refresh_metadata_preserving_view(&mut self) -> bool {
        // A read still outstanding is newer than what is shown.
        if self.folder_reads.is_pending() {
            return false;
        }
        let job = self.folder_job(self.current_path.clone(), true);
        self.folder_reads
            .start(job, Purpose::Update)
            .is_some_and(|(purpose, read)| self.apply_read(purpose, read))
    }

    /// Show `listing`, a new read of the folder shown, keeping the
    /// selection (by stable record identity, else by path), the scroll and
    /// the status.
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
        }
    }
    pub fn toggle_sort_order(&mut self) {
        self.sort_order = match self.sort_order {
            SortOrder::Ascending => SortOrder::Descending,
            SortOrder::Descending => SortOrder::Ascending,
        };
        self.sort_preserving_selection();
        self.store_preferences();
    }

    fn sort_preserving_selection(&mut self) {
        // Sorting only permutes this snapshot; paths identify its unchanged
        // entries without cloning file metadata or reading/hydrating content.
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
        // A previous click's row cannot be used for double-click detection
        // after reordering: that row may now contain a different file.
        self.last_click = None;
    }
    pub fn set_view_mode(&mut self, m: ViewMode) {
        if self.view_mode == m {
            return;
        }
        self.view_mode = m;
        self.store_preferences();
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
                    | FileCommand::CompressSelection(_)
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
                    // Record ids and content hashes are the system's
                    // bookkeeping; agents read them through the actions.
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
                    if let Some(share) = &entry.share {
                        lines.push(t_args(
                            "files-property-sharing",
                            &[arg("count", i64_of_usize(share.active_grants()))],
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
                        fs_protocol::FileRecordId(uuid::Uuid::from_bytes(record_id)),
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
                let recipient_devices: Vec<fs_protocol::Ed25519PublicKey> = self
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
                        fs_protocol::FileRecordId(uuid::Uuid::from_bytes(record_id)),
                        person,
                        recipient_devices,
                        fs_protocol::FilePermission::View,
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
                        fs_protocol::FileRecordId(uuid::Uuid::from_bytes(record_id)),
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
                                fs_protocol::ShareSubjectProjection::Person { person } => {
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
                                fs_protocol::ShareSubjectProjection::BearerLink => {
                                    (t("files-anyone-link"), Vec::new())
                                }
                            };
                            let permission = match grant.permission {
                                fs_protocol::FilePermission::View => t("files-can-view"),
                                fs_protocol::FilePermission::Edit => t("files-can-edit"),
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
                        title: t_args("files-sharing-title", &[arg("name", entry.name)]),
                        summary: t_args(
                            "files-sharing-summary",
                            &[arg("count", i64_of_usize(active))],
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
                    // The device that signed the version names who wrote it,
                    // as the file's History does: an agent's own device reads
                    // as the agent, on behalf of its person.
                    let author = authors
                        .resolve_file_actor(&version.actor.person.0, &version.actor.device.0)
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
                    // The person pasting is here to answer: clashing names
                    // are asked about once.
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

    /// `FileCommand::Click` — drives selection and, via the app's
    /// double-click detection, opening.
    fn apply_click(&mut self, index: usize, ctrl: bool, shift: bool) -> bool {
        if ctrl || shift {
            self.selection.apply_click(index, ctrl, shift);
            self.retain_shown_selection();
            self.last_click = None;
        } else {
            // Capture a multi-selection the press lands inside *before*
            // the click collapses it, so a drag that starts here carries
            // the whole set (drag a multi-selection). A press outside the
            // multi-selection seeds nothing.
            let sel = self.selection.selected();
            if sel.len() > 1 && sel.contains(&index) {
                // Keep the set while the press may still become its drag;
                // the release collapses it (`release_click`).
                self.drag_seed = self.selected_paths();
                self.collapse_on_release = Some(index);
                return true;
            }
            self.drag_seed = Vec::new();
            if let Some(path) = self.select(Some(index)) {
                // Double-click → open / enter.
                self.open_path(path);
            }
        }
        true
    }

    /// The release of a plain press inside a multi-selection that didn't
    /// become a drag: now it was a click, and selects just that row.
    fn release_click(&mut self) {
        if let Some(index) = self.collapse_on_release.take() {
            self.drag_seed.clear();
            if let Some(path) = self.select(Some(index)) {
                self.open_path(path);
            }
            self.structure_dirty = true;
        }
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
        self.retain_shown_selection();
        self.last_click = None;
        true
    }

    /// `FileCommand::NewFolder` — create, refresh, then drop into
    /// inline-rename so the user names it right away.
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

    /// `FileCommand::Delete` — trash the selection, or (already in the
    /// Trash) first ask before permanently purging it.
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
            // Recent lists uses of files from many folders: Delete forgets
            // the use, as the row's menu does, and never trashes the file.
            for path in &paths {
                self.apply_forget_recent(path);
            }
            return true;
        }
        if !paths.is_empty() {
            if self.is_trash_view() {
                // Already in the Trash — a second delete is irreversible.
                // Hold the exact paths in the modal action so a changed
                // selection cannot silently change what gets destroyed.
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

    /// Carry out a previously confirmed permanent deletion from Trash.
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

    /// `FileCommand::RestoreFromTrash` — restore the selection to its
    /// original location.
    fn is_native_trash_entry(&self, path: &Path) -> bool {
        entry_rows::in_native_trash(path, self.native_trash_dir().as_deref())
    }

    /// The host's Trash, when this window uses its native Trash.
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

    /// `FileCommand::EmptyTrash` — ask before permanently emptying a
    /// non-empty Trash. An already-empty Trash is safe feedback, not a
    /// confirmation flow.
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

    /// Carry out a previously confirmed request to empty the Trash.
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

    /// `FileCommand::NewFromTemplate` — copy `template`, refresh, then drop
    /// into inline-rename.
    fn apply_new_from_template(&mut self, template: &Path) -> bool {
        self.perform(crate::effects::FilesEffect::NewFromTemplate {
            template: template_resource_id(&template.to_string_lossy()),
        });
        true
    }

    /// Create a file from `template` in the folder shown and start naming it.
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

    /// `FileCommand::BeginDrag` — start carrying the row(s) as a floating
    /// ghost, and hand the gesture to the host or the toolkit's own
    /// client-side drag.
    fn apply_begin_drag(&mut self, path: PathBuf, is_dir: bool) -> bool {
        // The press carries the whole selection; it was never a click.
        self.collapse_on_release = None;
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
        if self
            .drag_paths
            .iter()
            .any(|path| crate::applications::is_app(path))
        {
            // An app is not a file another app could take: it is carried
            // only inside this window, where the Trash uninstalls it.
            return true;
        }
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
        let place = self.place_for(&path);
        let added = {
            let home = self.home_path.clone().unwrap_or_else(|| PathBuf::from("/"));
            let mut favs = self.favs.borrow_mut();
            favorites::add(&mut favs.favorites, path, &home, &self.user_dirs)
        };
        if added {
            self.refresh_places_search();
            self.status = Some(t("files-added-favourite"));
            self.submit(
                crate::resource::FileExplorer::ADD_BOOKMARK
                    .with(crate::resource::file_explorer::AddBookmark { place, at: None }),
            );
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
            self.refresh_places_search();
            self.status = Some(t("files-removed-favourite"));
            self.submit(crate::resource::FileExplorer::REMOVE_BOOKMARK.with(self.place_for(path)));
        }
        true
    }

    /// `FileCommand::ReorderFavorite` — move favourite `from` to index `to`
    /// (drag-to-reorder drop).
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
        if matches!(self.services, FileServices::Hosted(_)) || self.hosted_clipboard() {
            self.perform(crate::effects::FilesEffect::CopySelection);
            return true;
        }
        self.clipboard = self.selected_paths();
        self.clipboard_cut = false;
        true
    }

    /// `FileCommand::Cut` — cut the selection into the clipboard.
    fn apply_cut(&mut self) -> bool {
        if matches!(self.services, FileServices::Hosted(_)) || self.hosted_clipboard() {
            self.perform(crate::effects::FilesEffect::CutSelection);
            return true;
        }
        self.clipboard = self.selected_paths();
        self.clipboard_cut = true;
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

    /// Set the zoom level (clamped) and persist it.
    fn set_zoom(&mut self, level: i32) {
        let level = config::clamp_zoom(level);
        if level != self.zoom_level {
            self.zoom_level = level;
            self.store_preferences();
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

    /// Shared favourites store.
    #[cfg(test)]
    pub(crate) fn favorites_store(&self) -> FavStore {
        self.favs.clone()
    }

    /// Stage the preview of the folder's first text file, for an offscreen
    /// review.
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

    /// Stage the name-clash question, for an offscreen review. Nothing is
    /// copied: the sheet is only shown.
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

    /// Stage the Undo notice a move to Trash shows, for an offscreen review.
    /// Nothing is moved: headless rendering never touches a person's Trash.
    #[doc(hidden)]
    pub fn debug_show_trash_notice(&mut self) {
        self.undo.publish(&ui::UndoStatus {
            can_undo: true,
            ..ui::UndoStatus::default()
        });
        self.notify_done(t_args("files-moved-to-trash", &[arg("count", 2)]), true);
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

    /// Bring the places list in front (a narrow window's places screen),
    /// settled deterministically for a headless screenshot. In the wide
    /// layout the places list is already docked.
    #[doc(hidden)]
    pub fn debug_show_places(&mut self) {
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
        self.nav.geometry().content_column
    }

    /// The docked sidebar's resolved rect — the drag-to-pin drop target.
    #[doc(hidden)]
    pub fn debug_sidebar_rect(&self) -> vello::kurbo::Rect {
        self.nav.geometry().sidebar_column
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

    /// Where the places list is (docked beside the file view, or a drawer
    /// over it) — for tests asserting the sidebar reflows with the window
    /// width.
    #[doc(hidden)]
    pub fn debug_body_mode(&self) -> SidebarPlacement {
        self.nav.geometry().sidebar
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
        self.retain_shown_selection();
        self.structure_dirty = true;
    }

    /// Ask the owning framework host to open the selected real file history.
    /// A standalone widget without a managed host cannot render History.
    #[doc(hidden)]
    pub fn debug_open_history_demo(&mut self) {
        self.refresh_framework_history_target();
        self.framework_history.open(None);
    }

    /// Show the history of the file named by `source`: the selected file's,
    /// as the host's History offers it.
    pub(crate) fn show_file_history_in(
        &mut self,
        source: ui::history_mode::checked_files::FileHistorySource,
    ) {
        self.framework_history = source;
        self.framework_history_binding = None;
        self.refresh_framework_history_target();
    }

    /// Open what a launch names: a received share link (`tilo-file://…`) is
    /// admitted through the recipient service, anything else is a folder,
    /// resolved against the launcher's working directory. A folder that is
    /// missing or unreadable shows the ordinary navigation error.
    pub fn open_launch_argument(&mut self, argument: &str) {
        if argument.starts_with(crate::received_shares::LINK_SCHEME) {
            self.admit_received_share(argument.to_owned());
            return;
        }
        match std::path::absolute(argument) {
            Ok(path) => self.navigate(path),
            Err(error) => self.set_external_status(error.to_string()),
        }
    }

    /// Stage the view state screenshot `scenario` shows, once the window is
    /// laid out: a selection, an open menu, a drag in flight.
    #[expect(
        clippy::too_many_lines,
        reason = "one table of the screenshot scenarios, an arm each"
    )]
    pub fn stage_scenario(&mut self, scenario: Option<&str>) {
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
                self.context_menu.focus_first_action();
            }
            Some("extract-progress") => self.debug_show_extract_progress(),
            Some("selection-actions") => self.stage_published_selection(),
            Some(name @ ("selection-sort" | "selection-sorted" | "selection-reversed")) => {
                self.stage_sort_selection(name);
            }
            Some(name @ ("recent" | "recent-menu" | "recent-revealed" | "recent-forgotten")) => {
                // Capture-only files from the prepared scratch home. Use the
                // same Recent setting and commands as the live window.
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
                        self.context_menu.focus_first_action();
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
                self.arm_status_bubble_for_shots();
            }
            Some("status") => self.arm_status_bubble_for_shots(),
            Some("rename") => {
                self.debug_select(&[11]);
                self.begin_rename();
            }
            Some("places") => self.debug_show_places(),
            Some("remote-account") => {
                // A made-up account that needs signing in again: no real
                // account store or vault is read.
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
                    self.context_menu.focus_first_action();
                }
            }
            Some("loading") => self.debug_show_loading_state(),
            Some("error") => self.debug_show_error_state(),
            Some("menu" | "archive-menu") => self.open_view_menu(),
            Some("undo-trash") => self.debug_show_trash_notice(),
            Some("clash") => self.debug_show_clash(),
            Some("preview") => self.debug_show_preview(),
            Some("context-menu") => {
                // A right-click in the content pane, away from the toolbar's
                // menu, with its first row armed as the keyboard would.
                let at = Point::new(
                    rect.x0 + rect.width() * 0.45,
                    rect.y0 + rect.height() * 0.34,
                );
                self.debug_show_templates_menu(at);
                self.context_menu.focus_first_action();
            }
            Some("reorder") => {
                self.debug_begin_favorite_drag(4, 1);
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
        // A capture is one frame: it shows the folders' item counts as a
        // settled window does, counted here rather than on the worker.
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

    /// The filesystem work that cannot be done now, as this window's state
    /// says.
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
        if self.selected_paths().is_empty() {
            for id in [FilesEffect::COPY_SELECTION, FilesEffect::CUT_SELECTION] {
                statuses.push(ui::framework::EffectStatus::unavailable(
                    id,
                    "selection_empty",
                    t("files-selection-empty"),
                ));
            }
        }
        if !self.is_trash_view() {
            statuses.push(ui::framework::EffectStatus::unavailable(
                FilesEffect::EMPTY_TRASH,
                "not_in_trash",
                "Empty Trash from the Trash folder",
            ));
        }
        statuses
    }

    /// Ask for the previews of every file shown, so a capture waits for them.
    fn prefetch_thumbnails(&self) {
        if let Some(ctx) = self.thumb_ctx() {
            crate::views::file_view::FileView::prefetch_thumbnails(
                &ctx,
                &self.displayed_entries(),
                &self.workspace_roots,
            );
        }
    }

    /// Carry a file onto the first folder the view laid out, for a capture
    /// of a move in progress. The view publishes its folder rows as it draws.
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
        if let Some((path, _)) = self.toolbar.crumb_at(pt)
            && path != self.current_path
            && !self.drag_paths.contains(&path)
        {
            return Some(path);
        }
        self.place_target_at(pt).map(|(path, _)| path)
    }

    /// Show the copy or move under way once it has run long enough to be
    /// worth watching, with Cancel; whether the card changed.
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

    /// The card for a transfer under way: what it does, and how far it is.
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
                // `folder` is the archive being extracted.
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
        // A total not yet known (none counted) draws no bar rather than an
        // empty one that never fills.
        let done = transfer.done.load(Ordering::Relaxed);
        let total = transfer.total.load(Ordering::Relaxed);
        let progress = (total > 0).then(|| {
            ui::num::f32_of_f64((ui::num::f64_of_u64(done) / ui::num::f64_of_u64(total)).min(1.0))
        });
        self.notice.show_progress(title, body, progress);
        self.structure_dirty = true;
    }

    /// Open the folder a drag has rested on long enough; whether it did.
    /// The drag goes on in the opened folder.
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

    /// The preview or clash sheet, while one is up: it is the window's
    /// whole semantic tree, as a modal is.
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

    /// The folders a long path bar hides, as a menu: each opens its folder.
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
        self.context_menu = menu;
        self.menu_commands = commands;
        layout_menu_widget(&mut self.context_menu);
        self.structure_dirty = true;
    }

    /// Files dragged in from another app: the folder under the pointer (or
    /// the one shown) lights up, and the drop copies them there. Each file
    /// arrives as its own event; they are gathered and copied as one.
    fn route_incoming_drop(&mut self, event: &winit::event::WindowEvent) -> bool {
        use winit::event::WindowEvent;
        match event {
            WindowEvent::HoveredFile(_) => {
                self.drag_hover = self.folder_target_at(self.last_cursor);
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

    /// Copy what another app dropped, once its files have all arrived.
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

    /// Show the primary selected item large.
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

    /// The preview sheet takes every event while it is up: Space or Escape
    /// close it, the arrow keys move through the folder with it open.
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

    /// A press on empty space in the file view starts a box selection.
    fn maybe_start_marquee(
        &mut self,
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
            || !self.content_body_rect().contains(at)
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

    /// A box selection under way takes the pointer: it grows with the
    /// pointer, selecting what it touches, and ends on release.
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

    /// Paint the box selection being drawn.
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

    /// The sidebar place a drop at `pt` lands in, with its row: the middle
    /// of a place's row, where a drop moves into it. The band between rows
    /// stays the gap a folder is pinned into.
    fn place_target_at(&self, pt: Point) -> Option<(PathBuf, vello::kurbo::Rect)> {
        if !self.in_places_list(pt) {
            return None;
        }
        let list = self.nav.sidebar_ref::<ui::widget::NavList>()?;
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

    /// Resolve and apply a drag's drop: move the carried row(s) into a folder
    /// (a row, or the parent via the Up target), else pin a single folder to
    /// Favourites when released over the sidebar. Clears the drag state.
    fn finish_drag(&mut self, copy: bool) {
        crate::external_drag::end();
        let target = self.folder_target_at(self.last_cursor);
        let over_sidebar = self.nav.is_sidebar_open()
            && self
                .nav
                .geometry()
                .sidebar_column
                .contains(self.last_cursor);
        let paths = std::mem::take(&mut self.drag_paths);
        let origin = self.pin_drag.take();
        self.drag_hover = None;
        self.spring = None;
        if paths.iter().any(|path| crate::applications::is_app(path)) {
            // Apps go nowhere but the Trash, where they are uninstalled.
            if target.as_deref().is_some_and(crate::fileops::is_trash_dir) {
                self.uninstall_apps(&paths);
            }
        } else if let Some(dir) = target {
            self.drop_paths_into(&paths, &dir, copy);
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
        self.spring = None;
        self.drag_origin_is_dir = false;
    }

    /// Move (or with `copy`, copy) each of `paths` into directory `dir`. A
    /// drop onto another filesystem copies, as a move there would be a copy
    /// and a delete. No-ops and impossible moves — into itself, into its own
    /// subtree, or into the folder it already lives in — are skipped rather
    /// than erroring.
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

    /// Copy or move `paths` into `dir`, first asking once what to do about
    /// names `dir` already has.
    fn transfer(&mut self, paths: Vec<PathBuf>, dir: &Path, copy: bool) {
        let clashing = clash::clashing(&paths, dir, copy);
        if clashing.is_empty() {
            self.run_transfer(&paths, dir, copy);
        } else {
            self.clash = Some(PendingClash::new(paths, clashing, dir.to_path_buf(), copy));
            self.structure_dirty = true;
        }
    }

    /// Queue the copy or move of `paths` into `dir`; a taken name gets a
    /// free one.
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

    /// Settle the clash question: `None` leaves everything as it was.
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
                // One step to undo: the replaced items come back from Trash
                // and the new ones go.
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

    /// The clash sheet takes every event while it is up.
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
                    // Enter takes the choice that loses nothing.
                    Key::Named(NamedKey::Enter) => self.settle_clash(Some(ClashChoice::KeepBoth)),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Copy (or, after Cut, move) the clipboard into the folder shown.
    ///
    /// The operation queued for this device to carry out, `None` when it
    /// was carried out at once.
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

    fn request_deferred_document_open(
        &mut self,
        path: PathBuf,
        capability: ui::widget::DocumentOpenCapability,
    ) -> Result<(), String> {
        if self.pending_document_open.is_some() {
            // One request owns the only reply slot; this one waits for it.
            self.open_queue.push_back(path);
            return Ok(());
        }
        let token = self
            .next_document_open_token
            .checked_add(1)
            .ok_or_else(|| t("files-open-unavailable"))?;
        self.next_document_open_token = token;
        let reply = Rc::new(RefCell::new(ui::widget::DocumentOpenReply::pending(token)));
        let path_shown = path.clone();
        self.host_intent = Some(ui::widget::AppHostIntent::OpenDocument {
            path,
            capability,
            token,
            reply: Rc::downgrade(&reply),
        });
        self.pending_document_open = Some((token, reply));
        self.document_open_path = Some(path_shown);
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
        let path = self.document_open_path.take();
        match result {
            ui::widget::DocumentOpenOutcome::Opened => {
                if self.status.as_deref() == Some(t("files-opening-document").as_str()) {
                    self.status = None;
                    self.structure_dirty = true;
                }
            }
            ui::widget::DocumentOpenOutcome::Refused(reason) => match path {
                Some(path) => self.notify_open_failed(&path, &reason),
                None => self.set_external_status(reason),
            },
            ui::widget::DocumentOpenOutcome::Cancelled => {
                self.set_external_status(t("files-document-opening-cancelled"));
            }
        }
    }

    /// Enter: open every selected file, each in its app; a single folder
    /// (or a selection of only folders) opens the primary one here.
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

    /// Hand the next waiting file to the host once it has answered the last.
    fn open_next_queued(&mut self) {
        while self.pending_document_open.is_none() && self.pending_launch.is_none() {
            let Some(next) = self.open_queue.pop_front() else {
                return;
            };
            self.open_path(next);
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
        if let Some(app) = crate::applications::app_id(&path) {
            self.open_app(app.to_owned());
            return Ok(true);
        }
        if let Some((archive, prefix)) = crate::archive::location(&path) {
            if prefix.as_os_str().is_empty() {
                self.navigate(path);
                return Ok(false);
            }
            // The rows shown came from the archive's entries, read already;
            // a path typed into an archive not yet read opens as a folder,
            // which reads it.
            let is_folder = crate::archive::cached(&archive).is_none_or(|entries| {
                !entries.iter().any(|entry| {
                    entry.path == prefix && entry.kind != crate::archive::Kind::Directory
                })
            });
            if is_folder {
                self.navigate(path);
                return Ok(false);
            }
            // A file inside an archive opens as a read-only copy of it, read
            // off the window's thread: reaching it can mean decoding most of
            // a compressed tar.
            if let Some(copy) = archive_member_cached(&archive, &prefix) {
                return self.open_target(copy, runner);
            }
            self.archive_members.start(archive, prefix);
            return Ok(true);
        }
        if path.is_dir() {
            self.navigate(path);
            return Ok(false);
        }
        if self.pending_document_open.is_some() || self.pending_launch.is_some() {
            // The host answers one open at a time; the rest wait their turn
            // (Enter on several selected files opens each).
            self.open_queue.push_back(path);
            return Ok(true);
        }
        let hosted_route = match &self.services {
            FileServices::Legacy => None,
            FileServices::Hosted(hosted) => Some(hosted.document_opener.clone()),
        };
        let result = match hosted_route {
            None => ask_host_to_open(&path),
            Some(DocumentOpenRoute::Unavailable) => Some(Err(t("files-open-unavailable"))),
            Some(DocumentOpenRoute::Deferred(capability)) => {
                Some(self.request_deferred_document_open(path.clone(), capability))
            }
        };
        match result {
            Some(Ok(())) => self.note_recent(&path, crate::recent::RecentUse::Opened),
            Some(Err(reason)) => self.notify_open_failed(&path, &reason),
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
                ui::portal::LaunchOutcome::Failed(t("files-open-no-answer"))
            }
        };
        let path = path.clone();
        self.pending_launch = None;
        match outcome {
            ui::portal::LaunchOutcome::Opened => {
                self.note_recent(&path, crate::recent::RecentUse::Opened);
            }
            ui::portal::LaunchOutcome::Failed(reason) => self.notify_open_failed(&path, &reason),
        }
    }

    /// Say that `path` didn't open and why, offering another app when one
    /// claims the file: a double-click never just does nothing.
    fn notify_open_failed(&mut self, path: &Path, reason: &str) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let other_app = self.displayed_entries().iter().any(|e| e.path == path)
            && !self.open_with_candidates(path).apps.is_empty();
        self.notify_failure(
            t_args("files-open-failed", &[arg("name", name)]),
            reason,
            other_app.then(|| NoticeAction::OpenWith(path.to_path_buf())),
        );
    }

    // --- inline rename -------------------------------------------------

    /// The transient status line shown in the footer — an operation
    /// result, or the reason an open was refused. Readable so a host (or
    /// a test standing in for one) can confirm the explorer told the user
    /// something rather than swallowing it.
    pub fn status_line(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// The notice over the file view (a change to take back, a failure to
    /// act on) as one line, title then reason; readable so a host or a test
    /// can confirm what the person was told.
    pub fn notice_line(&self) -> Option<String> {
        self.notice.line()
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
    #[cfg(test)]
    pub(crate) fn set_rename_buffer(&mut self, s: String) {
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
    /// missing (a tab activated after boot never ran `build_layout`).
    fn paint_background(&mut self, theme: &ui::Theme) {
        // The window ground is the framework's; this only notices that the
        // chrome + body panes are normally built inside `build_layout`.
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

    /// Draw the shared empty state in the file area when the current
    /// directory has no entries (and isn't showing an error) — so an empty
    /// folder, and especially an empty Trash, reads as intentionally empty
    /// rather than broken. A muted icon over one context-aware sentence.
    fn draw_empty_state(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
    ) {
        if self.cloud.active || !self.entries.is_empty() || self.error.is_some() || self.is_loading
        {
            return;
        }
        let rect = self.content_body_rect();
        if rect.width() <= 1.0 || rect.height() <= 1.0 {
            return;
        }
        let icon = if self.is_recent_view() {
            ui::icons::Icon::History
        } else if self.is_applications_view() {
            ui::icons::Icon::LayoutGrid
        } else if self.is_trash_view() {
            ui::icons::Icon::Trash2
        } else {
            ui::icons::Icon::Folder
        };
        let message = self.empty_state_message();
        EmptyState::paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            rect,
            &EmptyStateContent {
                icon: Some(icon),
                ..EmptyStateContent::new(&message)
            },
            Point::new(-1.0, -1.0),
        );
    }

    /// The sheet's spec: the title, a close control and, when the panel
    /// proposes something, its one action.
    fn collab_panel_spec<'a>(
        title: &'a str,
        actions: &'a [ui::widget::ModalAction<'a>],
        body_height: f64,
    ) -> ui::widget::ModalSpec<'a> {
        ui::widget::ModalSpec::new(title)
            .actions(actions)
            .body_height(body_height)
    }

    /// Height the sheet's body wants at `width`: the summary, then each line
    /// in its own wrapped row.
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

    /// Draw the sheet: the shared modal over its scrim, the panel's summary
    /// and lines as its body, and the one action in its footer. Records the
    /// sheet's layout so the event path hit-tests what was painted.
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
        // Hover on the footer action and the close control follows the pointer.
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

    /// Mark the folder (or Up affordance) a live drag is hovering, so the drop
    /// target is unmistakable: your colour ringed around its row. Drawn under
    /// the ghost.
    fn draw_drop_highlight(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        theme: &ui::Theme,
    ) {
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
            })
            .or_else(|| {
                self.toolbar
                    .crumb_at(self.last_cursor)
                    .filter(|(path, _)| path == hover)
                    .map(|(_, rect)| rect)
            })
            .or_else(|| {
                self.place_target_at(self.last_cursor)
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
        // The pill grows with its measured label so enlarged text stays inside it.
        let h = (f64::from(th) + 2.0 * f64::from(tokens.spacing.s2))
            .max(ui::widget::ControlSize::Small.height(theme));
        let w = pad + icon + gap + f64::from(tw) + pad;
        // Offset from the cursor so the ghost sits beside, not under, the pointer.
        let x0 = self.last_cursor.x + f64::from(tokens.spacing.s3);
        let y0 = self.last_cursor.y + f64::from(tokens.spacing.s2);
        let rect = Rect::new(x0, y0, x0 + w, y0 + h);
        // The ghost is genuinely off the surface: it floats, with the
        // floating shadow and material.
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

/// Resolve a standalone [`ContextMenu`]'s rect through its shared anchored
/// layout. Both the main menu and the "New File" flyout own their geometry;
/// neither needs an app-local instantiation of the Taffy layout solver.
fn layout_menu_widget(menu: &mut ContextMenu) {
    if menu.is_open() {
        menu.set_text_scale(ui::Theme::dark().text_scale);
        menu.layout_at_position();
    }
}

/// Thin mockable wrapper around the desktop "open with default app"
/// handler. Mocked in tests so the open paths are verifiable without
/// asking a real desktop.
pub trait CommandRunner {
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
    fn open_default(&mut self, path: &std::path::Path) -> ui::portal::LaunchChannel {
        #[cfg(test)]
        crate::model::fixture_io_guard::ambient_io("SystemRunner::open_default");
        ui::portal::LaunchRequest::open(path).send()
    }
}

/// Presentation-side lower bound for the daemon's fail-closed merge codecs.
/// The daemon still validates exact bytes and may reject a malformed serialized document,
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
    /// What "Open Terminal Here" asks the shell for: Terminal, started in
    /// the current directory when that is a path the request can carry.
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

    /// Ask the shell to open Terminal in the current directory.
    ///
    /// # Errors
    ///
    /// Returns the shell's reason when Terminal could not be opened, or that
    /// there is no Tilo session to ask.
    pub fn open_terminal(&self) -> Result<(), String> {
        #[cfg(test)]
        crate::model::fixture_io_guard::ambient_io("open_terminal");
        let on_behalf_of = ui::invocation_scope::delivered_invocation().map(|id| id.0.into_bytes());
        runtime_portal::tools::open_tool(self.terminal_intent(), on_behalf_of)
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
        self.templates.templates()
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
        let can_paste = self.can_paste_clipboard();
        let has_templates = !templates.is_empty();
        let in_trash = self.is_trash_view();
        let mut spec: Vec<(String, Option<FileCommand>)> = Vec::new();

        // Recent gathers files from many folders: it is for finding and
        // reopening, not for editing in place.
        if self.is_recent_view() {
            return self.recent_menu_spec();
        }
        if let Some(spec) = self.applications_menu_spec() {
            return spec;
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
                t("files-archive-extract-here"),
                Some(FileCommand::ExtractCurrentArchive),
            ));
            spec.push((t("files-archive-extract-to"), Some(FileCommand::ExtractTo)));
            return tidy_menu(spec);
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
                if !self
                    .selected_paths()
                    .iter()
                    .any(|path| self.is_native_trash_entry(path))
                {
                    spec.push((
                        t("files-menu-delete-permanently"),
                        Some(FileCommand::Delete),
                    ));
                }
            }
            spec.push((String::new(), None));
            if !self.has_native_trash_entries() {
                spec.push((t("files-menu-empty-trash"), Some(FileCommand::EmptyTrash)));
            }
            return tidy_menu(spec);
        }

        // A file's menu is about the file: open it, change it, organise it,
        // collaborate on it. What belongs to the folder (new items, sorting,
        // hidden files, a terminal) is the menu of the folder's empty space.
        if has_sel {
            let single = self.selection.selected().len() == 1;
            spec.push((
                t("files-menu-open"),
                self.selection.primary().map(FileCommand::Activate),
            ));
            if self.open_with_available() {
                spec.push((t("files-menu-open-with"), Some(FileCommand::OpenWithMenu)));
            }
            if single {
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
                // Only offered while one of the person's devices is there
                // to receive.
                if self
                    .nearby_send
                    .targets()
                    .iter()
                    .any(|target| target.online)
                {
                    spec.push((
                        t("files-menu-send-nearby"),
                        Some(FileCommand::OpenNearbySend),
                    ));
                }
            }
            spec.push((String::new(), None));
            spec.push((t("files-menu-cut"), Some(FileCommand::Cut)));
            spec.push((t("files-menu-copy"), Some(FileCommand::Copy)));
            if can_paste {
                spec.push((t("files-menu-paste"), Some(FileCommand::Paste)));
            }
            spec.push((t("files-menu-rename"), Some(FileCommand::Rename)));
            spec.push((t("files-menu-move-trash"), Some(FileCommand::Delete)));
            spec.push((String::new(), None));
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
                spec.push((t("files-archive-extract-to"), Some(FileCommand::ExtractTo)));
            }
            spec.push((
                t("files-archive-compress-zip"),
                Some(FileCommand::CompressSelection(crate::archive::Packing::Zip)),
            ));
            spec.push((
                t("files-archive-compress-tar-zst"),
                Some(FileCommand::CompressSelection(
                    crate::archive::Packing::TarZst,
                )),
            ));
            // Favourites: add the single selected folder.
            if let Some(dir) = self.single_selected_dir() {
                spec.push((
                    t("files-menu-add-favourite"),
                    Some(FileCommand::AddFavorite(dir)),
                ));
            }
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
                        availability.policy == fs_protocol::AvailabilityPolicy::Automatic
                    })
                });
                let any_directly_pinned = collaborative.iter().any(|entry| {
                    entry.availability.as_ref().is_some_and(|availability| {
                        availability.policy == fs_protocol::AvailabilityPolicy::Pinned
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
                            availability.policy == fs_protocol::AvailabilityPolicy::Automatic
                                && !availability.policy_inherited
                                && availability.transfer.phase == fs_protocol::TransferPhase::Idle
                                && matches!(
                                    availability.residency,
                                    fs_protocol::Residency::Partial { .. }
                                        | fs_protocol::Residency::Resident { .. }
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
                            fs_protocol::Residency::Resident { .. }
                        ) && availability.transfer.phase == fs_protocol::TransferPhase::Idle
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
                            fs_protocol::FilePermission::View,
                            fs_protocol::FilePermission::Edit,
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
                        fs_protocol::FilePermission::View,
                        fs_protocol::FilePermission::Edit,
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
                        .filter(|grant| grant.status == fs_protocol::ShareGrantStatus::Active)
                    {
                        let grant_id = grant.grant_id;
                        let subject = match grant.subject {
                            fs_protocol::ShareSubjectProjection::Person { person } => {
                                self.trusted_humans.display_name_or_fingerprint(&person)
                            }
                            fs_protocol::ShareSubjectProjection::BearerLink => {
                                t("files-shared-link")
                            }
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
                                .then_some(version.id)
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
                                    history.versions.iter().find(|version| version.id == *id)
                                })
                                .collect::<Vec<_>>();
                            let current = members
                                .iter()
                                .copied()
                                .filter(|version| {
                                    version.content_hash.map(|hash| hash.0) == Some(current_hash)
                                })
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
                            let current_id = current_version.id;
                            spec.push((String::new(), None));
                            spec.push((
                                t_args(
                                    "files-resolve-keep-current",
                                    &[arg("conflict", short_uuid(conflict.id))],
                                ),
                                Some(FileCommand::ResolveConflict {
                                    conflict_id: conflict.id,
                                    strategy: fs_protocol::ConflictStrategy::KeepCurrent,
                                    version_id: current_id,
                                }),
                            ));
                            for other in members
                                .into_iter()
                                .filter(|version| version.id != current_id)
                            {
                                let other_id = other.id;
                                spec.push((
                                    t_args(
                                        "files-resolve-use-other",
                                        &[
                                            arg("conflict", short_uuid(conflict.id)),
                                            arg("device", short_fingerprint(&other.actor.device.0)),
                                        ],
                                    ),
                                    Some(FileCommand::ResolveConflict {
                                        conflict_id: conflict.id,
                                        strategy: fs_protocol::ConflictStrategy::UseOther,
                                        version_id: other_id,
                                    }),
                                ));
                                if merge_supported_path(&entry.path) {
                                    spec.push((
                                        format!(
                                            "{} ({})",
                                            t("files-merge-review-menu"),
                                            short_fingerprint(&other.actor.device.0),
                                        ),
                                        Some(FileCommand::ReviewConflictMerge {
                                            conflict_id: conflict.id,
                                            version_id: other_id,
                                            other_device: other.actor.device.0,
                                        }),
                                    ));
                                }
                                spec.push((
                                    format!(
                                        "{} ({})",
                                        conflict_strategy_label(
                                            fs_protocol::ConflictStrategy::KeepBoth,
                                        ),
                                        short_fingerprint(&other.actor.device.0),
                                    ),
                                    Some(FileCommand::ResolveConflict {
                                        conflict_id: conflict.id,
                                        strategy: fs_protocol::ConflictStrategy::KeepBoth,
                                        version_id: other_id,
                                    }),
                                ));
                            }
                        }
                    }
                }
            }
            return tidy_menu(spec);
        }

        // The folder's own menu (nothing is selected).
        spec.push((t("files-menu-new-folder"), Some(FileCommand::NewFolder)));
        // Only offer "New file…" when the Templates dir actually has files.
        if has_templates {
            spec.push((
                t("files-menu-new-file"),
                Some(FileCommand::OpenTemplatesMenu),
            ));
        }
        if can_paste {
            spec.push((t("files-menu-paste"), Some(FileCommand::Paste)));
        }
        if self.hosted_clipboard() {
            spec.push((
                t("files-menu-recover-transfers"),
                Some(FileCommand::RecoverClipboardTransfers),
            ));
        }
        spec.push((
            t("files-menu-open-terminal"),
            Some(FileCommand::OpenTerminal),
        ));
        // Remove the current directory from the favourites when it is one.
        if self.current_is_removable_favourite() {
            spec.push((
                t("files-menu-remove-favourite"),
                Some(FileCommand::RemoveFavorite(self.current_path.clone())),
            ));
        }
        // How the folder is shown. Sorting lives here too: a grid has no
        // column header, and "Sort by type" is reached only from this menu.
        spec.push((String::new(), None));
        for (label, by) in [
            ("files-menu-sort-name", SortBy::Name),
            ("files-menu-sort-size", SortBy::Size),
            ("files-menu-sort-type", SortBy::Type),
            ("files-menu-sort-date", SortBy::Modified),
        ] {
            spec.push((t(label), Some(FileCommand::SetSort(by))));
        }
        spec.push((
            if self.show_hidden {
                t("files-menu-hide-hidden")
            } else {
                t("files-menu-show-hidden")
            },
            Some(FileCommand::ToggleHidden),
        ));
        tidy_menu(spec)
    }

    /// Host entries cannot be purged through private-data Trash controls.
    fn has_native_trash_entries(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| self.is_native_trash_entry(&entry.path))
    }

    /// Whether this view offers Trash operations rather than editing.
    fn is_trash_view(&self) -> bool {
        fileops::is_trash_dir(&self.current_path)
            || (self.hosted_clipboard()
                && host_fs::trash::user_trash_dir().as_ref() == Some(&self.current_path))
    }

    /// The path of the single selected entry when it is a directory (drives the
    /// "Add to Favorites" action).
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
    /// The stored place of the folder shown, when it is a folder a person
    /// can pin (not Recent, Trash or an archive).
    pub(crate) fn current_place(&self) -> Option<String> {
        (self.shown_is_folder && !self.is_trash_view()).then(|| self.place_for(&self.current_path))
    }

    /// Whether the folder shown is one of the Favourites.
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

    /// The check a menu row for `command` shows: the sort that is in force,
    /// and whether hidden files are shown. Other commands are not state.
    fn command_checked(&self, command: Option<&FileCommand>) -> Option<bool> {
        match command? {
            FileCommand::SetSort(by) => Some(self.sort_by == *by),
            FileCommand::ToggleHidden => Some(self.show_hidden),
            _ => None,
        }
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
            // The sort choices sit in this menu itself, each with a check,
            // rather than behind one row that opens another menu.
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

    /// The labelled "Sort by" menu (the grid-mode Sort button): the sort key,
    /// then the direction, each with a check on the active choice. This is the
    /// grid's equivalent of clicking a list column header.
    fn open_sort_menu(&mut self) {
        let anchor = self.toolbar.sort_menu_anchor();
        let flip_y = self
            .toolbar
            .sort_menu_rect()
            .map_or(anchor.y, |rect| rect.y0 - 2.0);
        let spec = self.sort_menu_spec(false);
        self.open_menu_right_aligned(spec, anchor, flip_y);
    }

    /// The sort key, then the direction, each with a check on the active
    /// choice. Within another menu (`inline`) each key says what it sorts
    /// ("Sort by name"); under the "Sort by" button the bare key is enough.
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
            if cmd == Some(FileCommand::OpenWithMenu) {
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
        if command == Some(FileCommand::OpenTemplatesMenu) {
            let rect = self.context_menu.get_rect();
            self.open_templates_submenu(Point::new(rect.x1 - 6.0, rect.y0 + 28.0));
        } else if command == Some(FileCommand::OpenShortcutsMenu) {
            let rect = self.context_menu.get_rect();
            self.open_shortcuts_submenu(Point::new(rect.x1 - 6.0, rect.y0 + 28.0));
        } else if command == Some(FileCommand::OpenWithMenu) {
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
    /// What shows the row a search hit or an agent's `open` names: queued,
    /// and opened before the next frame is laid out.
    pub(crate) fn opener(&self) -> impl FnMut(&str) + 'static {
        let opened = Rc::clone(&self.opened);
        move |anchor| opened.borrow_mut().push(anchor.to_owned())
    }

    /// Show the row `anchor` names as its own open action does: a place
    /// opens, a collaborative record is revealed, a remote account shows its
    /// connection details.
    ///
    /// # Errors
    /// The anchor names no row Files opens, or opening it failed.
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
        self.answer_effect(match &result {
            Ok(_) => Ok(()),
            Err(message) => Err(tilo_actions::ActionFailure {
                code: "files.share_failed".into(),
                message: message.clone(),
                details: None,
                retryable: false,
            }),
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
        true
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
                self.status = Some(error);
            }
        }
        self.close_collab_panel();
        self.structure_dirty = true;
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
        let Some(&nav_n) = children.first() else {
            return;
        };
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
        let alt = modifiers.state().alt_key();
        let len = self.displayed_entries().len();
        match &ke.logical_key {
            // Back, forward and up, as in every file manager and browser.
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
            // Location is a first-class, keyboard-accessible control. Keep
            // this at the application layer so it works when focus is in the
            // file view, places list, or breadcrumb; a path editor itself
            // consumes keys before this method is reached.
            Key::Character(s) if (s == "l" || s == "L") && ctrl => {
                if self.header_in_bar {
                    // The location is the window bar's: it edits there.
                    ui::window_toolbar::edit_location();
                } else {
                    self.toolbar.begin_path_edit();
                }
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
            // Escape clears the selection (when there is one); otherwise it is
            // left for the host so it never swallows a no-op.
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
            // Space shows the selected file large, as in every file manager.
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
            // Typing the start of a name moves to it.
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
                    // The drop landed in another application; the host
                    // delivered it. Nothing moves here.
                    self.cancel_drag();
                } else {
                    // Ctrl copies, as on every desktop; so does a drop onto
                    // another disk, where a move would be a copy and delete.
                    self.finish_drag(ctx.modifiers.state().control_key());
                }
                *ctx.handled = true;
                self.structure_dirty = true;
                true
            }
            _ => false,
        }
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
            // Use the same shared, viewport-constrained menu as the toolbar.
            self.open_menu_right_aligned(spec, at, at.y);
        } else if !self.in_places_list(self.last_cursor) {
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
        // An open popup of the window (a dropdown's list, a menu) takes
        // every wheel over the window, held keys or not.
        if Widget::modal_capture(&self.nav).is_some() {
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
        if self.route_marquee_event(event, ctx) {
            return;
        }
        self.forward_to_body(event, ctx);
        self.maybe_start_marquee(event, ctx);
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
        // Going somewhere from the places screen of a narrow window shows
        // the folder.
        if self.current_path != path_before
            && self.nav.front_pane() == ui::widget::NavPane::Sidebar
            && !self.nav.geometry().stack.is_empty()
        {
            self.nav.show(ui::widget::NavPane::Content);
        }
    }
}

impl FileExplorerWidget {
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

    fn append_nearby_receiver_resources(&self, folder: &mut Resource) {
        for transfer in self.nearby_send.transfers() {
            folder
                .children
                .push(self.nearby_receiver_resource(transfer));
        }
    }

    fn nearby_receiver_resource(
        &self,
        transfer: &ui::collaboration::NearbyTransferStatus,
    ) -> Resource {
        use ui::collaboration::NearbyTransferPhase;

        let mut resource = Resource::new(
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
        inspect.description = Some(t("files-nearby-status-detail"));
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

    /// The live rows the window shows beneath the resource: listing entries,
    /// remote accounts, nearby transfers, and the retry of a failed listing.
    /// The folder, its places and templates are the resource's publication.
    fn live_filesystem_extras(&self) -> Vec<Resource> {
        self.published_live_rows()
            .into_iter()
            .map(ui::framework::LiveRow::into_resource)
            .collect()
    }

    /// [`Self::live_filesystem_extras`] as the rows a publication shares:
    /// an entry's row is the one last published while what it shows holds
    /// ([`Self::live_entry_rows`]).
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

    /// The live rows besides places, templates and entries: remote
    /// accounts, drives, cloud entries, nearby transfers and a failed
    /// listing's retry.
    fn live_other_resources(&self) -> Vec<Resource> {
        let mut extras = self.live_remote_account_resources();
        extras.extend(self.live_drive_resources());
        if ui::bound_dispatch::host_person_invocation_permitted() {
            extras.extend(self.cloud_resources());
        }
        let mut nearby_root = Resource::new("files.nearby", "files.nearby", "Nearby");
        self.append_nearby_receiver_resources(&mut nearby_root);
        extras.extend(nearby_root.children);
        if let Some((failure, _)) = self.folder_failure() {
            extras.push(retry_row(failure.may_pass()));
        }
        extras
    }

    fn live_remote_account_resources(&self) -> Vec<Resource> {
        // Account discovery belongs to the host person, not to whoever owns
        // the workspace. Checked resource reads install this trusted scope too.
        if !ui::bound_dispatch::host_person_invocation_permitted() {
            return Vec::new();
        }
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
                    "Open",
                );
                open.contract.availability = availability;
                resource.actions.push(open);
                resource.actions.push(crate::effects::row_action(
                    &crate::effects::FilesEffect::AccountDetails {
                        account: account_id.clone(),
                    },
                    "details",
                    "Show account status",
                ));
                resource.actions.push(crate::effects::row_action(
                    &crate::effects::FilesEffect::Reauthenticate {
                        account: account_id,
                    },
                    "reauthenticate",
                    "How to sign in again",
                ));
                resource
            })
            .collect()
    }
}

/// The listing's entries by resource id ([`FileExplorerWidget::entry_resource_id`]).
/// An id is a pure function of an entry's path and record, so an id found
/// here names the entry at its index whenever that entry still has the
/// path and record it had when the ids were made; anything else (another
/// listing, an id not found) makes them again from the listing as it is.
#[derive(Default)]
struct EntryIds {
    /// Each entry's path and record, in listing order, when the ids were made.
    made_of: Vec<(PathBuf, Option<[u8; 16]>)>,
    /// The index of each id's entry in that listing.
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
            .map(|(index, entry)| (FileExplorerWidget::entry_resource_id(entry), index))
            .collect();
        self.lookup(entries, id)
    }

    /// The entry `id` names, when its index still holds the entry it was
    /// made from.
    fn lookup<'a>(&self, entries: &'a [Entry], id: &str) -> Option<&'a Entry> {
        let index = *self.by_id.get(id)?;
        let (path, record) = self.made_of.get(index)?;
        entries
            .get(index)
            .filter(|entry| entry.path == *path && entry.record_id == *record)
    }
}

impl FileExplorerWidget {
    /// When the listing is next read: its poll, or the read a reported
    /// change asked for once its gap has passed
    /// ([`Self::refresh_metadata_if_due`]).
    fn listing_deadline(&self) -> Option<Instant> {
        // A deferred read ends on input of its own or when the read under way
        // wakes the window; a due deadline here would only spin until then.
        let changed_directory =
            (self.directory_changed && !self.directory_read_deferred()).then(|| {
                self.last_directory_read + metadata_refresh::change_read_gap(self.entries.len())
            });
        [self.next_metadata_refresh, changed_directory]
            .into_iter()
            .flatten()
            .min()
    }

    /// Every deadline besides the listing's, which the sync serves.
    fn other_deadline(&self) -> Option<Instant> {
        let document_open_reply = self
            .pending_document_open
            .as_ref()
            .and_then(|(token, reply)| {
                reply
                    .try_borrow()
                    .ok()
                    .and_then(|reply| (!reply.is_pending(*token)).then(Instant::now))
            });
        // Nearby Send asks only when the shell announces a change; the asks
        // that announcement queued go out on the next pump, at once.
        self.nearby_send
            .has_requests()
            .then(Instant::now)
            .into_iter()
            .chain(document_open_reply)
            .chain(
                (!self.open_queue.is_empty()
                    && self.pending_document_open.is_none()
                    && self.pending_launch.is_none())
                .then(Instant::now),
            )
            .chain(self.local_clipboard.next_wake())
            .chain(self.drives.next_wake())
            .chain(self.applications.next_wake())
            .chain(self.notice.deadline())
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
            .chain(self.nav.next_wake_deadline())
            .min()
    }
}

#[path = "native_observer.rs"]
mod native_observer;

impl FileExplorerWidget {
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
        self.retire_cancelled_effect();
        let now = ui::time::now();
        if let Some(result) = self.local_clipboard.timeout(now) {
            self.complete_clipboard_publication(result);
        }
        self.shortcut_menu.poll_if_due(now);
        let mut requests = self.nearby_send.take_requests();
        requests.extend(self.shortcut_menu.take_requests());
        self.poll_selected_open();
        requests.extend(self.local_clipboard.take_request());
        requests.extend(self.selected_open.take_request());
        requests.extend(self.selected_open.discovery.take_request());
        requests
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        self.retire_cancelled_effect();
        if let Some(result) = self.local_clipboard.event(event) {
            self.complete_clipboard_publication(result);
        }
        if self.selected_open.discovery.event(event) {
            self.invalidate_open_with();
            self.structure_dirty = true;
        }
        self.selected_open.event(event);
        self.poll_selected_open();
        self.drives.event(event);
        self.applications.event(event);
        let changed = self.handle_shortcut_event(event);
        let nearby_changed = self.handle_nearby_event(event);
        if changed || nearby_changed {
            self.structure_dirty = true;
        }
    }
}

impl FileExplorerWidget {
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
                    self.answer_effect(result.map(|_| ()).map_err(|message| {
                        tilo_actions::ActionFailure {
                            code: "files.nearby_send_failed".into(),
                            message,
                            details: None,
                            retryable: true,
                        }
                    }));
                }
            }
        }
        changed
    }
}

/// The selected files and folders, for asking an agent about them: each
/// row's published resource as a whole item, in the order shown, and their
/// names listed.
impl ui::presence::capability::PublishesPresence for FileExplorerWidget {
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

/// The window's undo, which the notice's and the menu's Undo ask for.
impl ui::undo::UndoControl for FileExplorerWidget {
    fn set_undo_status(&mut self, status: &mut dyn FnMut() -> ui::UndoStatus) {
        self.undo.publish(&status());
    }

    fn take_undo_request(&mut self) -> Option<ui::UndoRequest> {
        self.undo.take_request()
    }
}

impl Widget for FileExplorerWidget {
    ui::provides!(
        dyn ui::undo::UndoControl,
        dyn ui::widget::Collaborative,
        dyn ui::widget::controls::ActionControls,
        dyn ui::presence::capability::PublishesPresence
    );

    fn native_input_snapshot(&self, doc: &AppModel, path: &ModelPath) -> Option<serde_json::Value> {
        let (doc, path) = ui::doc::document_ref(doc, path);
        Some(self.native_input_snapshot_impl(doc, path))
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        // Service calls finishing on the app runtime wake the event loop.
        let received = self.poll_received_share();
        let transfers = self.follow_transfers(ui::time::now());
        let previewed = self.preview.as_mut().is_some_and(Preview::poll);
        let notice_expired = self.notice.expire(ui::time::now());
        let sprang = self.spring_open_if_due(ui::time::now());
        // A folder's item count came in: the list shows it.
        let counted = self.folder_counts.take_arrived();
        let listed = self.take_folder_read()
            | self.take_archive_listing()
            | self.poll_extract_chooser()
            | self.open_read_members();
        self.structure_dirty |= notice_expired | counted;
        self.poll_share_result()
            | received
            | notice_expired
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

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.adopt_preferences();
        // A control a caller invoked through the door (a toolbar button)
        // queued its command, and a search hit or an agent's `open` queued
        // the row it names; apply them before this frame is laid out.
        if self.drain_commands() | self.drain_opened() {
            self.structure_dirty = true;
        }
        // The window's bar shows the toolbar's slots, and what the person did
        // with them runs here, before this frame is laid out.
        if self.sync_window_toolbar() {
            self.structure_dirty = true;
        }
        self.poll_cloud();
        self.poll_mounted_cloud();
        self.poll_deferred_document_open();
        self.poll_pending_launch();
        self.open_next_queued();
        self.take_incoming_drop();
        self.refresh_framework_history_target();
        self.refresh_metadata_if_due(ui::time::now());
        // One bounded catalogue page per idle/sync turn. The palette only
        // reads its retained SearchCatalog; it never performs mount I/O.
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

        // Keep the persistent in-place rename editor alive through rebuilds.
        // On its first frame select the name but not a file's extension, so
        // typing replaces the name and keeps the type (a folder's whole name).
        self.rename_input.sync(state, path);
        self.state_retry.sync(state, path);
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
        [self.listing_deadline(), self.other_deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    // The listing's poll and change reads are served here: the directory is
    // read again and compared, and only a listing that changed (or trusted
    // identities, or the collaborative catalogue) asks for a sync. Any other
    // due deadline is the sync's.
    fn serve_deadline(&mut self, now: Instant) -> bool {
        if self.other_deadline().is_some_and(|due| due <= now) {
            return true;
        }
        self.refresh_metadata_if_due(now)
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // Refresh the toolbar's context, then swap the freshly built body
        // panes into the persistent `nav`. `nav` and the toolbar it owns keep
        // their state across the swap (docked state; the path bar's
        // in-progress edit), so a rebuild triggered by navigation / sort
        // doesn't snap the body or the path bar back mid-interaction.
        self.update_toolbar();
        self.composed_once = true;
        self.refresh_places_pane();
        self.refresh_cloud_header();
        self.nav.set_content(self.build_content_pane(theme));

        // The shell fills the window; the item count / selection status floats
        // in a reserved bottom lane inside the body (see `draw_status_bubble`),
        // leaving the rest of the body available to the file viewport.
        let nav_node = self.nav.build_layout(taffy, theme, node_map);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        // `new_with_children` only fails when a child belongs to a
        // different tree; the shell was just built in this one above.
        taffy.widget_node(style, &[nav_node], node_map, self)
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let base = vello::kurbo::Point::new(self.rect.x0, self.rect.y0);
        if let Ok(children) = taffy.children(node)
            && let Some(&nav_n) = children.first()
        {
            self.nav.apply_layout(taffy, nav_n, base);
            self.sync_toolbar_compact();
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
        self.paint_background(theme);
        self.nav
            .draw(scene, transform, text_renderer, theme, presence);
        self.draw_state_surface(scene, transform, text_renderer, theme, presence);
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
        self.nav
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        // Floating status bubble (item count / status + selection), above the
        // content but below any open menus / drag ghost, and only while the
        // file view is on screen (not while a narrow window shows the places
        // screen). A notice takes the bubble's place while it is up: one
        // message at a time at the bottom of the file view.
        if self.nav.geometry().content_column.width() > 0.0 {
            if self.notice.is_shown() {
                let area = self.content_body_rect();
                self.notice
                    .paint(scene, transform, text_renderer, theme, area);
            } else {
                self.draw_status_bubble(scene, transform, text_renderer, theme);
            }
        }
        self.draw_collab_panel(scene, transform, text_renderer, theme);
        let window = self.rect;
        if let Some(preview) = self.preview.as_mut() {
            preview.paint(scene, transform, text_renderer, theme, window);
        }
        if let Some(pending) = self.clash.as_mut() {
            pending.paint(scene, transform, text_renderer, theme, window);
        }
        if self.context_menu.is_open() {
            layout_menu_widget(&mut self.context_menu);
            self.context_menu
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        if self.submenu.is_open() {
            layout_menu_widget(&mut self.submenu);
            self.submenu
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        // The drop-target highlight sits on the target folder; the drag ghost
        // floats above everything else.
        self.resolve_staged_move_drag();
        self.draw_drop_highlight(scene, transform, theme);
        self.draw_marquee(scene, transform, theme);
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
        let state = &mut ctx.state;
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
            self.last_cursor = ctx.logical(position);
            // An armed press that has travelled far enough becomes a drag.
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
        if self.route_incoming_drop(event) {
            *ctx.handled = true;
            return;
        }
        if self.notice.is_shown() && self.route_notice_event(event) {
            *ctx.handled = true;
            return;
        }
        // The shared EmptyState visual and the retained Button share one
        // recovery route. The button's `on_effect` publishes a typed local
        // control action; draining it below enters the ordinary Refresh path.
        if self.error.is_some() {
            self.state_retry
                .handle_event(event, &mut ctx.reborrow(path, ctx.node_id));
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
                self.structure_dirty |= self.apply_command(self.folder_recovery_command());
                *ctx.handled = true;
                return;
            }
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
            // A release without travel was a click, not a drag.
            self.armed_drag = None;
            self.release_click();
        }

        // A live drag, the open context menu, a right-click, a zoom wheel,
        // and an in-place rename all get first dibs on the event, in that
        // priority order; the first that consumes it stops routing.
        if self.handle_live_drag_event(event, ctx)
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

    fn has_pending_visual_work(&self) -> bool {
        let mut pending = self
            .thumbs
            .as_deref()
            .is_some_and(|thumbs| !thumbs.is_idle());
        self.for_each_child(&mut |child| pending |= child.has_pending_visual_work());
        pending
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut d = std::mem::take(&mut self.structure_dirty);
        d |= self.nav.take_structure_dirty();
        d
    }

    fn needs_animation_frame(&self) -> bool {
        // The body's docked→push / reveal slide must keep the host pumping
        // frames while it settles; the floating status bubble likewise while it
        // lingers / fades.
        self.cloud.pending.is_some()
            || self.mounted_cloud.is_pending()
            || self.cloud.chooser.is_some()
            || self.cloud_import_pending()
            || self.nav.needs_animation_frame()
            || (self.is_loading && self.state_spinner.needs_animation_frame())
            || self.status_bubble.needs_frame(ui::time::now())
    }

    fn needs_idle_sync(&self) -> bool {
        self.collaborative_record_pager.borrow().has_more() || self.nav.needs_idle_sync()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.collab_panel.is_some() {
            return Vec::new();
        }
        let v: Vec<&dyn Widget> = vec![
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
        let v: Vec<&mut (dyn Widget + '_)> =
            vec![&mut self.nav, &mut self.context_menu, &mut self.submenu];
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

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        if self.app_chooser_open() {
            return self.app_chooser.accessibility();
        }
        if let Some(sheet) = self.sheet_accessibility() {
            return Some(sheet);
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
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.collab_panel_focused)
                            .modal(true),
                    )
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
            .with_state(AccessState::new().busy(self.is_loading));
        if let Some((title, detail, can_retry)) = self.state_surface_copy() {
            let mut state = AccessNode::new(Role::Group)
                .with_name(title)
                .with_description(detail)
                .with_live(if can_retry {
                    Politeness::Assertive
                } else {
                    Politeness::Polite
                })
                .with_state(AccessState::new().busy(self.is_loading));
            if can_retry {
                state = state.with_children(vec![
                    AccessNode::new(Role::Button)
                        .with_name(self.folder_recovery_label())
                        .with_action_target("files-retry-folder"),
                ]);
            }
            root = root.with_children(vec![state]);
        }
        if let Some(mut notice) = self.notice.access_node() {
            let mut buttons = Vec::new();
            if let Some(action) = self.notice.action() {
                let label = match action {
                    NoticeAction::Undo => t("files-notice-undo"),
                    NoticeAction::OpenWith(_) => t("files-notice-open-with"),
                    NoticeAction::CancelTransfer => t("files-notice-cancel"),
                };
                buttons.push(
                    AccessNode::new(Role::Button)
                        .with_name(label)
                        .with_action_target(NOTICE_ACTION_TARGET),
                );
            }
            buttons.push(
                AccessNode::new(Role::Button)
                    .with_name(t("files-notice-dismiss"))
                    .with_action_target(NOTICE_DISMISS_TARGET),
            );
            notice = notice.with_children(buttons);
            root.children.push(notice);
        }
        Some(root)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
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
        if matches!(action, Action::Activate)
            && (target == NOTICE_ACTION_TARGET || target == NOTICE_DISMISS_TARGET)
        {
            let click = if target == NOTICE_DISMISS_TARGET {
                NoticeClick::Dismissed
            } else {
                match self.notice.action().cloned() {
                    Some(action) => NoticeClick::Action(action),
                    None => return false,
                }
            };
            if let NoticeClick::Action(action) = self.notice.activate(click) {
                self.act_on_notice(action);
            }
            self.structure_dirty = true;
            return true;
        }
        if target == "files-retry-folder" && matches!(action, Action::Activate) {
            let changed = self.apply_command(self.folder_recovery_command());
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

impl FileExplorerWidget {
    /// The places, collaborative records and remote accounts as search
    /// entries. Collaborative entries carry stable record UUIDs; their
    /// current paths are provenance, never identity.
    pub(crate) fn search_entries_shown(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        let mut entries = self.places_search.entries();
        entries.extend(self.collaborative_search.entries());
        if ui::bound_dispatch::host_person_invocation_permitted() {
            entries.extend(self.remote_accounts_search.entries());
        }
        entries
    }
}

impl FileExplorerWidget {
    /// The live rows the window shows beneath the resource: listing entries,
    /// remote accounts, nearby transfers and the retry of a failed listing.
    /// The folder, its places and templates and its folder-wide work are the
    /// resource's publication.
    #[cfg(test)]
    pub(crate) fn published_rows(&self) -> Vec<Resource> {
        self.live_filesystem_extras()
    }

    /// The row `id`, when [`Self::published_rows`] leaves it out: an entry
    /// past the eager page, a place, a late collaborative record, a remote
    /// account, found by a search and resolved only when acted on.
    pub(crate) fn published_row(&self, id: &str) -> Option<Resource> {
        fn find(resource: &Resource, id: &str) -> Option<Resource> {
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

impl ui::widget::controls::ActionControls for FileExplorerWidget {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if let Some(invocation) = self.selected_open.control.take() {
            return Some(invocation);
        }
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
        // Own accessibility actions are semantic; only children emit actions.
        ui::widget::controls::children_have_action_control_bindings(self)
    }
    // A control a caller invoked through the door (a toolbar button) queued
    // its command; apply it now, as the click it stands for would.
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &ui::widget::controls::ControlResult,
    ) {
        for child in self.children_mut() {
            ui::widget::controls::on_control_invocation_result(child, address, result);
        }
        if self.drain_commands() {
            self.structure_dirty = true;
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

/// What the palette, agents and scripts reach of a Files window, for the
/// tests that drive the widget without a framework host: the resource's
/// publication with the window's effects beside it and its live rows
/// beneath, and the routing the framework's surface host does.
#[cfg(test)]
pub(crate) mod published {
    use super::FileExplorerWidget;
    use crate::effects::FilesEffect;
    use crate::resource::FileExplorer;
    use ui::framework::HostDelivery as _;
    use ui::tilo_actions::Resource;
    use ui::tilo_resource::{Effect as _, Resource as _};

    /// The published tree: one root, as the framework publishes it, with the
    /// places the window shows.
    pub(crate) fn tree(files: &FileExplorerWidget, doc: &automerge::AutoCommit) -> Vec<Resource> {
        let mut root = ui::tilo_resource::publish::<FileExplorer>(
            FileExplorer::KIND,
            "Files",
            &FileExplorer::default(),
        );
        root.actions.extend(FilesEffect::specs());
        let _ = doc;
        root.children.extend(files.published_rows());
        vec![root]
    }

    /// Invoke the action `action` of the published resource `resource` with
    /// `args`, as the palette or an agent does through the door.
    ///
    /// # Errors
    /// The door's refusal, or why the operation failed.
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

    /// How an operation started: done at once with its output, or pending
    /// to finish later.
    pub(crate) type Started = ui::framework::Effected;

    /// Start the encoded operation on the window as the surface host does:
    /// the effect vocabulary to the surface's handler, anything else to the
    /// toolkit's widgets inside it.
    ///
    /// # Errors
    /// Why the operation failed or was refused.
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
                match files.run_effect(FilesEffect::from_payload(payload)?)? {
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

impl FileExplorerWidget {
    pub fn debug_begin_favorite_drag(&mut self, from: usize, to: usize) -> bool {
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
        let Some(list) = self.nav.sidebar_mut::<ui::widget::NavList>() else {
            return false;
        };
        list.stage_drag(&source, &before);
        true
    }

    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
        self.debug_show_status_bubble();
    }
}
