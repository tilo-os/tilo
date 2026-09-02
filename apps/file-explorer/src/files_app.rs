//! `FileExplorerWidget` — the qOS file explorer's `AppWidget`.
//!
//! The widget owns all app state (current dir, entries, multi-selection,
//! sort, view mode, navigation history) and rebuilds its child widget tree
//! from that state. Interactive children can't borrow the app back, so they
//! push [`FileCommand`]s into a shared [`CommandSink`]; `handle_event`
//! drains the sink after each event and applies the commands, marking the
//! tree structurally dirty so the App rebuilds the composition on the next
//! frame. See [`crate::command`].

use crate::command::{CommandSink, FileCommand, new_sink};
use crate::i18n::{arg, t, t_args};
use crate::model::{Entry, SortBy, SortOrder, ViewMode, initial_path, read_dir_sync, sort_entries};
use crate::places_sidebar::{self, FavStore, PlacesSidebar};
use crate::views::file_view::ThumbCtx;
use crate::{config, favorites, fileops, templates};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use thumbnail::{ThumbKey, ThumbnailService};
use ui::qos_actions::{
    ActionContract, ActionSpec, Availability, ConfirmationPolicy, EncodedAction, ExecutionMode,
    OperationKind, PermissionRequirement, Resource, ResultSpec, RetryPolicy, RiskLevel,
};
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{
    Column, NavShell, Row, ScrollAxis, Scrollable, ShellMode, Spacer, Text, TextInput, Widget,
};
use ui::{AppKind, AppWidget, IconDescriptor, SelectionModel};
use vello::kurbo::Point;
use vello::peniko::ImageData as PenikoImage;

/// CRDT map key holding the inline-rename field's `Text` object. Seeded with
/// the current name when a rename begins; the in-place editor's `TextInput`
/// edits it and `commit_rename` reads the chosen name back out.
const RENAME_QUERY_PATH: &str = "files.rename_query";

/// Width (logical px) at/above which the places list docks beside the file
/// view; below it the places list becomes an overlay drawer *over* the file
/// view, summoned by the hamburger chip. The file list is the content either
/// way — the places list is navigation, so it never takes the window from it
/// (`ui/HIG.md` §1a).
const BODY_BREAKPOINT_PX: f64 = 640.0;
const LIVE_METADATA_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(750);

fn format_duration(duration_ms: u64) -> String {
    let seconds = duration_ms / 1_000;
    let hours = seconds / 3_600;
    let minutes = (seconds / 60) % 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Ensure a `Text` object exists at `key` on `root`, returning its id. The
/// toolkit's `TextInput` binds to (and writes into) an existing `Text`
/// object but never creates one, so the app must seed it — otherwise
/// keystrokes mutate only the input's local string and never reach the CRDT.
/// `None` if `root` is not a live object in `state` (seeding then simply
/// does not happen for this sync pass).
fn ensure_text_obj(
    state: &mut automerge::AutoCommit,
    root: &automerge::ObjId,
    key: &str,
) -> Option<automerge::ObjId> {
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ReadDoc, Value};
    if let Ok(Some((Value::Object(ObjType::Text), id))) = state.get(root, key) {
        return Some(id);
    }
    state.put_object(root, key, ObjType::Text).ok()
}

/// Build the file manager's thumbnail service. The content-addressed cache
/// lives under the app's persistence dir (a tempdir in tests, so they stay
/// isolated) so previews survive across launches; it falls back to a temp dir
/// when there is no persistence dir. `None` when neither location is
/// writable — previews then fall back to type badges (see [`ThumbCtx`]).
fn build_thumb_service(config_dir: Option<&Path>) -> Option<ThumbnailService> {
    let dir = config_dir.map_or_else(
        || std::env::temp_dir().join("qos-file-explorer-thumbnails"),
        |d| d.join("thumbnails"),
    );
    ThumbnailService::open(&dir, 4).ok().or_else(|| {
        let fallback = std::env::temp_dir().join("qos-file-explorer-thumbnails");
        ThumbnailService::open(&fallback, 4).ok()
    })
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
    DestructiveEviction(Vec<PathBuf>),
    PermanentDelete(Vec<PathBuf>),
    EmptyTrash,
}

fn hex_prefix(bytes: &[u8], count: usize) -> String {
    let mut output = String::with_capacity(count.saturating_mul(2));
    for byte in bytes.iter().take(count) {
        let _ = write!(output, "{byte:02x}");
    }
    output
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "each boolean is independent named UI state for loading, clipboard, rename, layout, or visibility"
)]
pub struct FileExplorerWidget {
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
    /// Transient status line (operation results), shown in the footer.
    status: Option<String>,
    /// Filesystem/xattr changes have no winit event of their own. The widget
    /// asks the host for this low-frequency wake so sync/availability/share/
    /// history badges update without burning an animation frame loop.
    next_metadata_refresh: Instant,
    /// When `Some(i)`, an inline rename of displayed entry `i` is in
    /// progress: the in-place editor is drawn over row/cell `i` and its
    /// `TextInput` edits [`RENAME_QUERY_PATH`]. `None` = not renaming.
    renaming: Option<usize>,
    /// Editable name mirrored from the rename field's `Text` object each
    /// `sync` while renaming; `commit_rename` renames the file to this.
    rename_buffer: String,
    /// Set when a rename just began so the next `sync` seeds the CRDT
    /// `Text` object with `rename_buffer` (the current name) before the
    /// input reads it back.
    rename_seed_pending: bool,
    /// The in-place rename editor: a persistent, CRDT-bound `TextInput` the
    /// app owns (so its caret/focus survive the per-frame rebuild) and draws
    /// **over the selected row/cell** — not in a bottom bar. It binds to
    /// [`RENAME_QUERY_PATH`], so keystrokes splice into the same `Text` object
    /// `rename_buffer` mirrors.
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
    /// The qOS collaborative-workspace roots the FUSE driver mounts, surfaced as
    /// first-class System locations in the sidebar. Resolved once at startup.
    workspace_roots: Vec<PathBuf>,
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
    /// Read-only sharing/history sheet sourced entirely from bounded FUSE
    /// projections. `None` means no sheet is open.
    collab_panel: Option<CollabPanel>,
    collab_panel_focused: bool,
    /// Single background share request. The worker never touches widget state;
    /// paint/event polling adopts its bounded result on the UI thread.
    share_result: Arc<std::sync::Mutex<Option<Result<crate::sharing::ShareActionResult, String>>>>,
    share_pending_path: Option<PathBuf>,
    /// Host-projected names keyed by certified human roots. Labels affect
    /// presentation only; every command and request continues to carry the
    /// root key.
    trusted_humans: crate::identity_directory::TrustedHumanDirectory,
    history_result: Arc<std::sync::Mutex<Option<Result<(), String>>>>,
    history_pending_path: Option<PathBuf>,
    history_pending_success: Option<String>,
    resource_invocation: Option<qos_actions::InvocationId>,
    resource_result: Option<qos_actions::InvocationUpdate>,
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
        // directory — `/run/qos/app` — while the directory actually
        // opened is the user's real home, bound by a grant and named by
        // `QOS_USER_HOME`. Reading `dirs::home_dir()` here made the two
        // disagree, and the breadcrumb is where a disagreement shows:
        // `format_path` abbreviates a path to `~` only when it lies
        // under the home it was handed, so a home it does not recognise
        // renders as an absolute trail from the filesystem root.
        Self::with_persistence(config::config_dir(), crate::model::user_home())
    }

    /// Surface a bounded result from startup portals such as received-file
    /// admission without coupling those clients to widget internals.
    pub fn set_external_status(&mut self, status: impl AsRef<str>) {
        const MAX_STATUS_CHARS: usize = 512;
        self.status = Some(status.as_ref().chars().take(MAX_STATUS_CHARS).collect());
        self.structure_dirty = true;
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
    pub fn with_persistence_dirs(
        config_dir: Option<PathBuf>,
        home: Option<PathBuf>,
        user_dirs: favorites::UserDirs,
    ) -> Self {
        let cwd = initial_path();
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
        let mut entries = read_dir_sync(&cwd).unwrap_or_default();
        if !show_hidden {
            entries.retain(|e| !e.name.starts_with('.'));
        }
        sort_entries(SortBy::Name, SortOrder::Ascending, &mut entries);
        let cmds = new_sink();
        let thumbs = build_thumb_service(config_dir.as_deref()).map(Arc::new);
        let trusted_humans =
            crate::identity_directory::TrustedHumanDirectory::load_default().unwrap_or_default();
        Self {
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
            status: None,
            next_metadata_refresh: Instant::now() + LIVE_METADATA_REFRESH_INTERVAL,
            renaming: None,
            rename_buffer: String::new(),
            rename_seed_pending: false,
            rename_input: TextInput::new(vec![])
                .with_kind(ui::widget::TextInputKind::Name)
                .with_id("files.rename_input")
                .with_on_change_path(RENAME_QUERY_PATH)
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
                .with_breakpoint(BODY_BREAKPOINT_PX)
                // The docked places list is permanent — no in-sidebar collapse chip;
                // the hamburger still summons the drawer when narrow.
                .hide_toggle_when_docked(),
            toolbar: crate::toolbar::FileToolbar::new(cmds.clone()),
            status_bubble: ui::StatusBubble::new(),
            last_status_label: String::new(),
            shots_status: false,
            composed_once: false,
            cmds,
            structure_dirty: false,
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
            workspace_roots: crate::model::workspace_roots(),
            pin_drag: None,
            drag_paths: Vec::new(),
            armed_drag: None,
            drag_seed: Vec::new(),
            drag_origin_is_dir: false,
            drag_hover: None,
            collab_panel: None,
            collab_panel_focused: false,
            share_result: Arc::new(std::sync::Mutex::new(None)),
            share_pending_path: None,
            trusted_humans,
            history_result: Arc::new(std::sync::Mutex::new(None)),
            history_pending_path: None,
            history_pending_success: None,
            resource_invocation: None,
            resource_result: None,
            drop_targets: Rc::new(std::cell::RefCell::new(Vec::new())),
            scroll_offset: Rc::new(Cell::new((0.0, 0.0))),
            thumbs,
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

    /// The entries currently shown. (There is no in-app search field — search
    /// is OS-owned per `ui/HIG.md` §5 — so this is the sorted directory
    /// listing as-is.)
    pub fn displayed_entries(&self) -> Vec<Entry> {
        self.entries.clone()
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
        let can_back = !self.history.is_empty();
        let can_forward = !self.forward.is_empty();
        let can_up = self.current_path.parent().is_some();
        self.toolbar.set_context(
            self.current_path.clone(),
            self.home_path.clone(),
            self.workspace_roots.clone(),
            can_back,
            can_forward,
            can_up,
            self.view_mode,
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
            .with_workspaces(self.workspace_roots.clone()),
        )
    }

    /// The **content** pane (the responsive body's detail): the file list or
    /// grid (plus any error affordance), scrolling under a symmetric gutter.
    fn build_content_pane(&self, theme: &ui::Theme) -> Box<dyn Widget> {
        let displayed = self.displayed_entries();
        let scale = self.zoom_scale();
        let view_child: Box<dyn Widget> = match self.view_mode {
            ViewMode::List => crate::views::list_view::build_list(
                &displayed,
                &self.selection,
                &self.cmds,
                scale,
                self.sort_by,
                self.sort_order,
                self.renaming,
                self.rename_rect.clone(),
                self.last_cursor,
                self.drop_targets.clone(),
                self.thumb_ctx(),
            ),
            ViewMode::Grid => crate::views::grid_view::build_grid(
                &displayed,
                &self.selection,
                &self.cmds,
                scale,
                self.renaming,
                self.rename_rect.clone(),
                self.last_cursor,
                self.drop_targets.clone(),
                self.thumb_ctx(),
            ),
        };
        let sp = &theme.tokens.spacing;
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
        if self.is_loading {
            view_col = view_col.push(Box::new(Text::new(&t("files-loading"))));
        } else if let Some(err) = self.error.as_deref() {
            // Real error affordance: the message plus a "Try Again" button
            // wired to re-read the directory (Refresh).
            let sink = self.cmds.clone();
            let retry = ui::widget::Button::new("files.retry", &t("files-try-again"), None)
                .on_effect(move || crate::command::push(&sink, FileCommand::Refresh));
            view_col = view_col.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(err)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(retry)),
            ));
        }
        // The list/grid scrolls: pin the scroll viewport to the *remaining*
        // height of the content column (grow-from-zero) rather than letting it
        // claim `percent(1.0)`. `fill_parent_height` takes exactly the space
        // left under the optional error row so a long directory overflows the
        // viewport and the wheel + scrollbar thumb move the list.
        view_col = view_col.push(Box::new(
            Scrollable::new(ScrollAxis::Vertical)
                .fill_parent_height()
                .with_persisted_offset(self.scroll_offset.clone())
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
        if self.is_trash_view() {
            t("files-empty-trash")
        } else {
            t("files-empty-folder")
        }
    }

    fn show_collab_panel(&mut self, panel: CollabPanel) {
        self.collab_panel = Some(panel);
        self.collab_panel_focused = true;
    }

    fn close_collab_panel(&mut self) {
        self.collab_panel = None;
        self.collab_panel_focused = false;
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
        }
    }

    fn collab_panel_action_label(&self) -> Option<String> {
        match self.collab_panel.as_ref()?.action.as_ref()? {
            CollabPanelAction::DestructiveEviction(_) => {
                Some(t("files-destructive-eviction-confirm"))
            }
            CollabPanelAction::PermanentDelete(_) => Some(t("files-permanent-delete-confirm")),
            CollabPanelAction::EmptyTrash => Some(t("files-empty-trash-confirm")),
        }
    }

    fn collab_panel_action_description(&self) -> Option<String> {
        match self.collab_panel.as_ref()?.action.as_ref()? {
            CollabPanelAction::DestructiveEviction(_) => Some(t("files-destructive-eviction-loss")),
            CollabPanelAction::PermanentDelete(_) | CollabPanelAction::EmptyTrash => {
                Some(t("files-permanent-delete-loss"))
            }
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
        self.status_bubble.poke(std::time::Instant::now());
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
        let now = std::time::Instant::now();
        let label = self.footer_label();
        if label != self.last_status_label {
            label.clone_into(&mut self.last_status_label);
            self.status_bubble.poke(now);
        }
        if label.is_empty() {
            return;
        }
        let text_size = 12.0f32;
        let (w, h) = ui::status_bubble::pill_size(&label, tr, text_size, 0.0);
        let margin = 14.0;
        let rect = vello::kurbo::Rect::new(
            self.rect.x1 - margin - w,
            self.rect.y1 - margin - h,
            self.rect.x1 - margin,
            self.rect.y1 - margin,
        );
        let zone =
            vello::kurbo::Rect::new(rect.x0 - 40.0, rect.y0 - 24.0, self.rect.x1, self.rect.y1);
        let over = self.shots_status || zone.contains(self.last_cursor);
        self.status_bubble.set_hover(over, now);
        let alpha = self.status_bubble.alpha(now);
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
        // Opening the Trash for the first time (nothing deleted yet) should show
        // an empty folder, not a "no such directory" error, so materialise it.
        if fileops::is_trash_dir(&path) {
            let _ = fileops::ensure_trash_dir();
        }
        match read_dir_sync(&path) {
            Ok(mut entries) => {
                if !self.show_hidden {
                    entries.retain(|e| !e.name.starts_with('.'));
                }
                sort_entries(self.sort_by, self.sort_order, &mut entries);
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
        self.is_loading = false;
    }

    /// Go to the parent directory (history-aware).
    pub fn back(&mut self) {
        if let Some(parent) = self.current_path.parent() {
            self.navigate(parent.to_path_buf());
        }
    }

    /// Go up to the parent directory (alias of [`back`] kept explicit for
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
        self.next_metadata_refresh = now + LIVE_METADATA_REFRESH_INTERVAL;
        let identities_changed = self.refresh_trusted_identities(None);
        if self.renaming.is_some() || self.pin_drag.is_some() {
            return identities_changed;
        }
        self.refresh_metadata_preserving_view() || identities_changed
    }

    fn refresh_trusted_identities(&mut self, path: Option<&Path>) -> bool {
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
        let Ok(mut entries) = read_dir_sync(&self.current_path) else {
            // A transient unmount/reconnect must not replace a still-useful
            // view with an error page. Explicit Refresh remains the path that
            // surfaces directory errors to the user.
            return false;
        };
        if !self.show_hidden {
            entries.retain(|entry| !entry.name.starts_with('.'));
        }
        sort_entries(self.sort_by, self.sort_order, &mut entries);
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
        let now = Instant::now();
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
        self.sort_by = s;
        sort_entries(self.sort_by, self.sort_order, &mut self.entries);
    }
    pub fn toggle_sort_order(&mut self) {
        self.sort_order = match self.sort_order {
            SortOrder::Ascending => SortOrder::Descending,
            SortOrder::Descending => SortOrder::Ascending,
        };
        sort_entries(self.sort_by, self.sort_order, &mut self.entries);
    }
    pub fn set_view_mode(&mut self, m: ViewMode) {
        self.view_mode = m;
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
    #[allow(clippy::too_many_lines)]
    pub fn apply_command(&mut self, cmd: FileCommand) -> bool {
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
                        if error.raw_os_error() == Some(libc::EBUSY) {
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
                                &[arg("duration", format_duration(duration_ms))],
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
                let result = Arc::clone(&self.share_result);
                std::thread::spawn(move || {
                    let outcome = crate::sharing::create_one_use_view_link(
                        &socket,
                        collab_types::FileRecordId(uuid::Uuid::from_bytes(record_id)),
                    )
                    .map(crate::sharing::ShareActionResult::LinkCreated);
                    if let Ok(mut slot) = result.lock() {
                        *slot = Some(outcome);
                    }
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
                let result = Arc::clone(&self.share_result);
                std::thread::spawn(move || {
                    let outcome = crate::sharing::create_named_share(
                        &socket,
                        collab_types::FileRecordId(uuid::Uuid::from_bytes(record_id)),
                        person,
                        recipient_devices,
                        collab_types::FilePermission::View,
                        None,
                    )
                    .map(crate::sharing::ShareActionResult::NamedCreated);
                    if let Ok(mut slot) = result.lock() {
                        *slot = Some(outcome);
                    }
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
                let result = Arc::clone(&self.share_result);
                std::thread::spawn(move || {
                    let outcome = crate::sharing::create_share(
                        &socket,
                        collab_types::FileRecordId(uuid::Uuid::from_bytes(record_id)),
                        request,
                    );
                    if let Ok(mut slot) = result.lock() {
                        *slot = Some(outcome);
                    }
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
                let result = Arc::clone(&self.share_result);
                std::thread::spawn(move || {
                    let outcome = crate::sharing::revoke_grant(&socket, grant_id);
                    if let Ok(mut slot) = result.lock() {
                        *slot = Some(outcome);
                    }
                });
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
            FileCommand::ShowHistory => {
                let entry = self
                    .selection
                    .primary()
                    .and_then(|index| self.displayed_entries().get(index).cloned());
                if let Some(entry) = entry
                    && let Some(history) = entry.history
                {
                    let mut lines = history
                        .versions
                        .iter()
                        .rev()
                        .map(|version| {
                            let person =
                                collab_types::Ed25519PublicKey::from_bytes(version.actor.person);
                            t_args(
                                "files-history-version-line",
                                &[
                                    arg("action", t(version.provenance_key())),
                                    arg("time", format_timestamp(version.timestamp_ms)),
                                    arg(
                                        "person",
                                        self.trusted_humans.display_name_or_fingerprint(&person),
                                    ),
                                    arg("device", short_fingerprint(&version.actor.device)),
                                ],
                            )
                        })
                        .collect::<Vec<_>>();
                    if history.versions_truncated {
                        lines.push(t("files-history-older"));
                    }
                    for conflict in &history.conflicts {
                        let state = match conflict.status {
                            collab_types::ConflictStatus::Open => t("files-conflict-needs-review"),
                            collab_types::ConflictStatus::Resolved => {
                                conflict.resolution.as_ref().map_or_else(
                                    || t("files-conflict-resolved"),
                                    |resolution| {
                                        t_args(
                                            "files-conflict-resolved-strategy",
                                            &[arg(
                                                "strategy",
                                                conflict_strategy_label(resolution.strategy),
                                            )],
                                        )
                                    },
                                )
                            }
                        };
                        lines.push(t_args(
                            "files-conflict-line",
                            &[
                                arg("conflict", short_uuid(conflict.id)),
                                arg("count", fluent_count(conflict.versions.len())),
                                arg("state", state),
                            ],
                        ));
                        if conflict.status == collab_types::ConflictStatus::Open {
                            lines.push(t("files-conflict-resolve-hint"));
                            lines.push(t("files-conflict-unsupported-hint"));
                        }
                    }
                    self.show_collab_panel(CollabPanel {
                        title: t_args("files-history-title", &[arg("name", entry.name)]),
                        summary: t_args(
                            "files-history-summary",
                            &[
                                arg(
                                    "versions",
                                    i64::try_from(history.total_versions).unwrap_or(i64::MAX),
                                ),
                                arg("heads", fluent_count(history.heads.len())),
                            ],
                        ),
                        lines,
                        action: None,
                    });
                }
                true
            }
            FileCommand::RestoreVersion { version_id } => {
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
                self.history_pending_path = Some(entry.path.clone());
                self.history_pending_success = Some(t("files-version-restored"));
                self.status = Some(t("files-restoring-version"));
                let result = Arc::clone(&self.history_result);
                std::thread::spawn(move || {
                    let outcome = crate::history_control::restore_version(&entry.path, version_id);
                    if let Ok(mut slot) = result.lock() {
                        *slot = Some(outcome);
                    }
                });
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
            FileCommand::RestoreFromTrash => self.apply_restore_from_trash(),
            FileCommand::EmptyTrash => self.request_empty_trash_confirmation(),
            FileCommand::Copy => self.apply_copy(),
            FileCommand::Cut => self.apply_cut(),
            FileCommand::Paste => {
                self.paste();
                true
            }
            FileCommand::OpenTemplatesMenu => self.apply_open_templates_menu(),
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
                match fileops::delete_to_trash(&paths) {
                    Ok(()) => {
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

    /// Carry out a previously confirmed permanent deletion from Trash.
    fn apply_permanent_delete(&mut self, paths: &[PathBuf]) -> bool {
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
            self.status = Some(t("files-added-favourite"));
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
            self.status = Some(t("files-removed-favourite"));
        }
        true
    }

    /// `FileCommand::ReorderFavorite` — move favourite `from` to index `to`
    /// (drag-to-reorder drop).
    fn apply_reorder_favorite(&self, from: usize, to: usize) -> bool {
        {
            let mut favs = self.favs.borrow_mut();
            favorites::reorder(&mut favs.favorites, from, to);
        }
        self.save_favorites();
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
            if let Some(dir) = self.config_dir.as_deref() {
                config::save_zoom(dir, level);
            }
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

    /// Open the context menu *and* its "New File" flyout (headless shots demo).
    #[doc(hidden)]
    pub fn debug_show_templates_menu(&mut self, at: Point) {
        self.open_context_menu(at);
        let anchor = Point::new(self.context_menu.get_rect().x1 - 6.0, at.y + 66.0);
        self.open_templates_submenu(anchor);
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
        for src in &items {
            let res = if cut {
                fileops::move_into(src, &dest)
            } else {
                fileops::copy_into(src, &dest)
            };
            if res.is_err() {
                errors += 1;
            }
        }
        if cut {
            self.clipboard.clear();
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
    /// qOS session) does this fall back to the desktop default handler
    /// (`open::that`, via `CommandRunner::open_default`).
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
        if path.is_dir() {
            self.navigate(path);
            return Ok(false);
        }
        match ask_host_to_open(&path) {
            Some(Ok(())) => {}
            Some(Err(reason)) => self.status = Some(reason),
            None => runner.open_default(&path)?,
        }
        Ok(true)
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

    /// Replace the editable rename text. The next `sync` seeds this into the
    /// rename field's CRDT `Text` object; used by tests to drive the flow
    /// without a keystroke round-trip through the widget tree.
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
        // tab on a qOS shell that boots Notes-active), `build_layout` doesn't
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
        let (icon, msg) = if self.is_trash_view() {
            (ui::icons::Icon::Trash2, self.empty_state_message())
        } else {
            (ui::icons::Icon::Folder, self.empty_state_message())
        };
        let tokens = &theme.tokens;
        let dim = tokens.colors.text_dim;
        let icon_px = 40.0_f64;
        let size = tokens.typography.scale.base;
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
        let ty = crate::text_layout::centered_text_y(cy, f64::from(size), size);
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

    // Drawing the modal as one routine keeps geometry, focus, and action hit
    // targets derived from the same bounded panel state.
    #[allow(clippy::too_many_lines)]
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
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &shape,
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
            let danger = theme.tokens.colors.semantic.danger;
            let action_shape = RoundedRect::from_rect(action_rect, 8.0);
            scene.fill(
                Fill::NonZero,
                transform,
                danger.with_alpha_factor(0.14),
                None,
                &action_shape,
            );
            scene.stroke(&Stroke::new(1.5), transform, danger, None, &action_shape);
            let (label_width, _) = text_renderer.measure(
                &label,
                theme.tokens.typography.scale.sm,
                Some((action_rect.width() - 24.0) as f32),
            );
            text_renderer.draw_text(
                scene,
                &label,
                theme.tokens.typography.scale.sm,
                danger,
                transform
                    * Affine::translate((
                        action_rect.x0 + (action_rect.width() - f64::from(label_width)) / 2.0,
                        action_rect.y0 + 9.0,
                    )),
                Some((action_rect.width() - 24.0) as f32),
            );
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
        let accent = theme.tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX];
        let inset = Rect::new(rect.x0 + 6.0, rect.y0 + 2.0, rect.x1 - 6.0, rect.y1 - 2.0);
        let shape = RoundedRect::from_rect(inset, 8.0);
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
        use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
        use vello::peniko::Fill;
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
        let size = tokens.typography.scale.base;
        let (tw, _) = text_renderer.measure(&label, size, None);
        let (pad, icon, gap, h) = (10.0_f64, 16.0_f64, 8.0_f64, 30.0_f64);
        let w = pad + icon + gap + f64::from(tw) + pad;
        // Offset from the cursor so the ghost sits beside, not under, the pointer.
        let x0 = self.last_cursor.x + 14.0;
        let y0 = self.last_cursor.y + 6.0;
        let rect = Rect::new(x0, y0, x0 + w, y0 + h);
        let shape = RoundedRect::from_rect(rect, f64::from(tokens.radii.md));
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_standard,
            None,
            &shape,
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
        let ty = crate::text_layout::centered_text_y(y0, h, size);
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
            Ok(_) => {
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
    /// every file type qOS has no registered app for (images, PDFs, video,
    /// audio, plain text, …) so double-click/Enter/Open never silently do
    /// nothing.
    ///
    /// # Errors
    ///
    /// Returns the OS error if the desktop has no default handler
    /// registered for `path`, or fails to launch it.
    fn open_default(&mut self, path: &std::path::Path) -> std::io::Result<()>;
}

pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn spawn(&mut self, program: &str, cwd: &std::path::Path) -> std::io::Result<()> {
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
        std::process::Command::new(program)
            .arg(arg)
            .current_dir(cwd)
            .spawn()
            .map(|_c| ())
    }
    fn open_default(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        open::that(path)
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
        .is_some_and(qdoc::is_supported_ext)
}

/// Presentation-side lower bound for the daemon's fail-closed merge codecs.
/// The daemon still validates exact bytes and may reject malformed qdoc,
/// invalid UTF-8, overlapping edits, or a stale signed conflict.
fn merge_supported_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "txt" | "qdoc"))
}

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
/// handler. That is the honest answer off a qOS session, not a second
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
    DOCUMENT_OPENER.with(|slot| slot.borrow_mut().as_mut().map(|open| open(path)))
}

/// The full `Widget::handle_event` parameter set, bundled so the
/// event-routing helpers `handle_event` delegates to stay under clippy's
/// argument-count limit instead of threading nine parameters through each.
struct EventCtx<'a> {
    event: &'a winit::event::WindowEvent,
    scale_factor: f64,
    modifiers: winit::event::Modifiers,
    state: &'a mut automerge::AutoCommit,
    path: &'a automerge::ObjId,
    handled: &'a mut bool,
    focus_target: &'a mut Option<taffy::NodeId>,
    node_id: taffy::NodeId,
    taffy: &'a taffy::TaffyTree<()>,
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
    #[allow(clippy::too_many_lines)]
    fn context_menu_spec(&self, templates: &[PathBuf]) -> Vec<(String, Option<FileCommand>)> {
        let has_sel = !self.selection.is_empty();
        let can_paste = !self.clipboard.is_empty();
        let has_templates = !templates.is_empty();
        let in_trash = self.is_trash_view();
        let mut spec: Vec<(String, Option<FileCommand>)> = Vec::new();

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
            spec.push((t("files-menu-rename"), Some(FileCommand::Rename)));
            spec.push((String::new(), None));
            spec.push((t("files-menu-cut"), Some(FileCommand::Cut)));
            spec.push((t("files-menu-copy"), Some(FileCommand::Copy)));
            let displayed = self.displayed_entries();
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
                                            conflict_strategy_label(
                                                collab_types::ConflictStrategy::Merge,
                                            ),
                                            short_fingerprint(&other.actor.device),
                                        ),
                                        Some(FileCommand::ResolveConflict {
                                            conflict_id: conflict.id,
                                            strategy: collab_types::ConflictStrategy::Merge,
                                            version_id: collab_types::FileVersionId(other_id),
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
        let mut menu = ContextMenu::new();
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
        let mut menu = ContextMenu::new();
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

    /// Open `self.context_menu` from a `(label, command)` spec, right-aligned so
    /// its right edge sits at `anchor` (a toolbar control's bottom-right). Used
    /// by the labelled View / Sort menus, whose triggers hug the window's right
    /// edge where a left-anchored menu would spill off-screen. A `None` command
    /// with an empty label is a separator.
    fn open_menu_right_aligned(&mut self, spec: Vec<(String, Option<FileCommand>)>, anchor: Point) {
        self.submenu.close();
        let mut menu = ContextMenu::new();
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
        // Right-align: lay out once to learn the menu width, then re-anchor so
        // the right edge lands on `anchor.x`, clamped to the window's left edge.
        menu.open_at([anchor.x as f32, anchor.y as f32]);
        layout_menu_widget(&mut menu);
        let w = menu.get_rect().width();
        let x = (anchor.x - w).max(0.0);
        menu.open_at([x as f32, anchor.y as f32]);
        self.context_menu = menu;
        self.menu_commands = cmds;
        layout_menu_widget(&mut self.context_menu);
    }

    /// The labelled "View options" overflow menu (the ⋯ button): new folder /
    /// file, a clearly-labelled show-hidden toggle with a check, and a terminal.
    /// Everything here is spelled out in words — no mystery symbolic controls.
    fn open_view_menu(&mut self) {
        let anchor = self.toolbar.view_menu_anchor();
        let mut spec: Vec<(String, Option<FileCommand>)> = Vec::new();
        // At phone width the path remains useful while direct view / sort
        // controls move here. The same labelled actions remain keyboard and
        // accessibility reachable without squeezing desktop chrome into one row.
        if self.toolbar.is_compact() {
            if self.toolbar.can_go_forward() {
                spec.push((t("files-forward"), Some(FileCommand::Forward)));
                spec.push((String::new(), None));
            }
            let checked = |on: bool, label: String| {
                t_args(
                    if on {
                        "files-menu-checked"
                    } else {
                        "files-menu-unchecked"
                    },
                    &[arg("label", label)],
                )
            };
            spec.push((
                checked(self.view_mode == ViewMode::List, t("files-view-list")),
                Some(FileCommand::SetView(ViewMode::List)),
            ));
            spec.push((
                checked(self.view_mode == ViewMode::Grid, t("files-view-grid")),
                Some(FileCommand::SetView(ViewMode::Grid)),
            ));
            spec.push((String::new(), None));
            spec.push((t("files-sort-by"), Some(FileCommand::OpenSortMenu)));
            spec.push((String::new(), None));
        }
        spec.push((t("files-menu-new-folder"), Some(FileCommand::NewFolder)));
        if !self.available_templates().is_empty() {
            spec.push((
                t("files-menu-new-file"),
                Some(FileCommand::OpenTemplatesMenu),
            ));
        }
        spec.push((String::new(), None));
        spec.push((
            if self.show_hidden {
                t("files-menu-show-hidden-checked")
            } else {
                t("files-menu-show-hidden")
            },
            Some(FileCommand::ToggleHidden),
        ));
        if self.is_trash_view() {
            spec.push((String::new(), None));
            spec.push((t("files-menu-empty-trash"), Some(FileCommand::EmptyTrash)));
        }
        spec.push((String::new(), None));
        spec.push((
            t("files-menu-open-terminal"),
            Some(FileCommand::OpenTerminal),
        ));
        self.open_menu_right_aligned(spec, anchor);
    }

    /// The labelled "Sort by" menu (the grid-mode Sort button): the sort key,
    /// then the direction, each with a check on the active choice. This is the
    /// grid's equivalent of clicking a list column header.
    fn open_sort_menu(&mut self) {
        let anchor = self.toolbar.sort_menu_anchor();
        let checked = |on: bool, label: String| {
            t_args(
                if on {
                    "files-menu-checked"
                } else {
                    "files-menu-unchecked"
                },
                &[arg("label", label)],
            )
        };
        let mut spec: Vec<(String, Option<FileCommand>)> = Vec::new();
        for (label, by) in [
            (t("files-sort-name"), SortBy::Name),
            (t("files-sort-size"), SortBy::Size),
            (t("files-sort-type"), SortBy::Type),
            (t("files-sort-date"), SortBy::Modified),
        ] {
            spec.push((
                checked(self.sort_by == by, label),
                Some(FileCommand::SetSort(by)),
            ));
        }
        spec.push((String::new(), None));
        let asc = self.sort_order == SortOrder::Ascending;
        spec.push((
            checked(asc, t("files-sort-ascending")),
            Some(FileCommand::SetSortOrder(SortOrder::Ascending)),
        ));
        spec.push((
            checked(!asc, t("files-sort-descending")),
            Some(FileCommand::SetSortOrder(SortOrder::Descending)),
        ));
        self.open_menu_right_aligned(spec, anchor);
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
        let mut menu = ContextMenu::new();
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
            if let Some(FileCommand::OpenTemplatesMenu) = cmd {
                // Open the flyout to the right of the clicked row; keep the
                // main menu open behind it.
                let anchor = Point::new(self.context_menu.get_rect().x1 - 6.0, p.y - 10.0);
                self.open_templates_submenu(anchor);
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
    #[allow(clippy::too_many_arguments)]
    fn route_menu_keyboard(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        focus_target: &mut Option<taffy::NodeId>,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
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
                scale_factor,
                &modifiers,
                state,
                path,
                &mut menu_handled,
                focus_target,
                node_id,
                taffy,
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

    fn poll_share_result(&mut self) -> bool {
        let result = self
            .share_result
            .lock()
            .ok()
            .and_then(|mut result| result.take());
        let Some(result) = result else {
            return false;
        };
        let lifecycle = self
            .resource_invocation
            .take()
            .map(|invocation| match &result {
                Ok(_) => qos_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: qos_actions::ActionOutput::default(),
                },
                Err(message) => qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
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

    fn poll_history_result(&mut self) -> bool {
        let result = self
            .history_result
            .lock()
            .ok()
            .and_then(|mut result| result.take());
        let Some(result) = result else {
            return false;
        };
        let lifecycle = self
            .resource_invocation
            .take()
            .map(|invocation| match &result {
                Ok(()) => qos_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: qos_actions::ActionOutput::default(),
                },
                Err(message) => qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
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
    #[allow(clippy::too_many_arguments)]
    fn forward_to_body(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        let Ok(children) = taffy.children(node_id) else {
            return;
        };
        let (Some(&above_n), Some(&nav_n)) = (children.first(), children.get(1)) else {
            return;
        };
        if !*handled {
            self.toolbar.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                above_n,
                taffy,
            );
        }
        if !*handled {
            self.nav.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                nav_n,
                taffy,
            );
        }
    }

    /// App-level keyboard navigation, applied when a key isn't consumed by
    /// a focused child (search field, etc.). Returns whether it acted.
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
    fn handle_live_drag_event(&mut self, ctx: &mut EventCtx) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        if self.pin_drag.is_none() {
            return false;
        }
        match ctx.event {
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

    /// The open context menu (or its flyout) gets first dibs on clicks +
    /// Escape. Returns whether the event was consumed.
    fn handle_open_menu_event(&mut self, ctx: &mut EventCtx) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        if !(self.context_menu.is_open() || self.submenu.is_open()) {
            return false;
        }
        match ctx.event {
            WindowEvent::KeyboardInput { .. } => {
                if self.route_menu_keyboard(
                    ctx.event,
                    ctx.scale_factor,
                    ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                    ctx.node_id,
                    ctx.taffy,
                ) {
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
    fn handle_right_click_open_menu(&mut self, ctx: &mut EventCtx) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } = ctx.event
        else {
            return false;
        };
        let mut child_handled = false;
        self.forward_to_body(
            ctx.event,
            ctx.scale_factor,
            ctx.modifiers,
            ctx.state,
            ctx.path,
            &mut child_handled,
            ctx.focus_target,
            ctx.node_id,
            ctx.taffy,
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
    fn handle_zoom_wheel(&mut self, ctx: &mut EventCtx) -> bool {
        let winit::event::WindowEvent::MouseWheel { delta, .. } = ctx.event else {
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
    fn handle_rename_mode_event(&mut self, ctx: &mut EventCtx) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        if self.renaming.is_none() {
            return false;
        }
        if let Some(rect) = self.rename_rect.get() {
            self.rename_input.apply_layout_external(rect);
        }
        match ctx.event {
            WindowEvent::KeyboardInput { event: ke, .. } => {
                use winit::keyboard::{Key, NamedKey};
                if ke.state == ElementState::Pressed {
                    match &ke.logical_key {
                        Key::Named(NamedKey::Enter) => {
                            // Pull the freshest field text from the CRDT, then
                            // commit — the last keystroke may not have synced.
                            let rename_obj =
                                ensure_text_obj(ctx.state, ctx.path, RENAME_QUERY_PATH);
                            if let Some(obj) = rename_obj.as_ref()
                                && let Ok(text) = {
                                    use automerge::ReadDoc;
                                    ctx.state.text(obj)
                                }
                            {
                                self.rename_buffer = text;
                            }
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
                self.rename_input.handle_event(
                    ctx.event,
                    ctx.scale_factor,
                    &ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.handled,
                    ctx.focus_target,
                    ctx.node_id,
                    ctx.taffy,
                );
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
                    self.rename_input.handle_event(
                        ctx.event,
                        ctx.scale_factor,
                        &ctx.modifiers,
                        ctx.state,
                        ctx.path,
                        ctx.handled,
                        ctx.focus_target,
                        ctx.node_id,
                        ctx.taffy,
                    );
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
    fn finish_event(&mut self, ctx: &mut EventCtx) {
        let path_before = self.current_path.clone();
        self.forward_to_body(
            ctx.event,
            ctx.scale_factor,
            ctx.modifiers,
            ctx.state,
            ctx.path,
            ctx.handled,
            ctx.focus_target,
            ctx.node_id,
            ctx.taffy,
        );
        if !*ctx.handled
            && let winit::event::WindowEvent::KeyboardInput { event: ke, .. } = ctx.event
            && self.handle_app_key(ke, ctx.modifiers)
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

impl Widget for FileExplorerWidget {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            "files.refresh",
            "files.new-folder",
            "files.paste",
            "files.navigate",
            "files.open",
            "files.folder-command",
            "files.entry-command",
            "files.share-link",
        ]
    }

    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<Resource> {
        let mut folder = Resource::new(
            format!("folder:{}", self.current_path.display()),
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
                availability: if self.current_path.exists() {
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
        folder.actions.push(
            ActionSpec::prompted_operation(
                "navigate",
                "Open location",
                "Folder path",
                "/path/to/folder",
                OperationKind::View,
                "files.navigate",
                |path| {
                    EncodedAction::encode_operation("files.navigate", &path)
                        .expect("string payload")
                },
            )
            .with_contract(ActionContract::view()),
        );
        for (id, label, verb, kind) in [
            ("up", "Parent folder", "up", OperationKind::View),
            ("back", "Back", "back", OperationKind::View),
            ("forward", "Forward", "forward", OperationKind::View),
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
        ] {
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
        let new_folder = ActionSpec::prompted_operation(
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
            reversible: true,
            ..ActionContract::default()
        });
        folder.actions.push(new_folder);
        for (id, label, type_id, available) in
            [("paste", "Paste", "files.paste", !self.clipboard.is_empty())]
        {
            let spec = ActionSpec::operation(id, label, OperationKind::Effect, type_id, &"")
                .expect("file operation payload is serializable")
                .with_contract(ActionContract {
                    availability: if available {
                        Availability::Available
                    } else {
                        Availability::Unavailable {
                            code: if id == "paste" {
                                "clipboard_empty"
                            } else {
                                "location_unavailable"
                            }
                            .into(),
                            reason: if id == "paste" {
                                "Copy or cut files before pasting"
                            } else {
                                "The current location cannot contain a new folder"
                            }
                            .into(),
                        }
                    },
                    permission: PermissionRequirement::Write,
                    risk: RiskLevel::Low,
                    confirmation: ConfirmationPolicy::None,
                    retry: RetryPolicy::AtMostOnce,
                    reversible: false,
                    ..ActionContract::default()
                });
            folder.actions.push(spec);
        }
        for entry in &self.entries {
            let path = entry.path.to_string_lossy().into_owned();
            let mut resource =
                Resource::new(format!("entry:{path}"), "files.entry", entry.name.clone());
            if let Ok(spec) =
                ActionSpec::operation("open", "Open", OperationKind::View, "files.open", &path)
            {
                resource
                    .actions
                    .push(spec.with_contract(ActionContract::view()));
            }
            for (id, label, verb, destructive) in [
                ("copy", "Copy", "copy", false),
                ("cut", "Cut", "cut", false),
                ("trash", "Move to Trash", "trash", true),
                ("properties", "Properties", "properties", false),
                ("sharing", "Sharing and access", "sharing", false),
                ("history", "Version history", "history", false),
            ] {
                let mut spec = ActionSpec::operation(
                    id,
                    label,
                    if matches!(verb, "properties" | "sharing" | "history") {
                        OperationKind::View
                    } else {
                        OperationKind::Effect
                    },
                    "files.entry-command",
                    &vec![verb.to_string(), path.clone()],
                )
                .expect("entry command is serializable");
                if destructive {
                    spec = spec.destructive();
                }
                resource.actions.push(spec);
            }
            if entry.record_id.is_some() {
                resource.actions.push(
                    ActionSpec::operation(
                        "share-link",
                        "Create one-time share link",
                        OperationKind::Effect,
                        "files.share-link",
                        &path,
                    )
                    .expect("share path is serializable")
                    .execution(ExecutionMode::Background { cancellable: false })
                    .returns(ResultSpec::Job),
                );
            }
            folder.children.push(resource);
        }
        vec![folder]
    }

    fn invoke_resource_operation(
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
            (OperationKind::Effect, "files.paste") => {
                if self.clipboard.is_empty() {
                    return Err("The clipboard is empty".into());
                }
                let destination = self.current_path.clone();
                let sources = self.clipboard.clone();
                let cut = self.clipboard_cut;
                let mut failures = Vec::new();
                for source in &sources {
                    let result = if cut {
                        fileops::move_into(source, &destination)
                    } else {
                        fileops::copy_into(source, &destination)
                    };
                    if let Err(error) = result {
                        failures.push(format!("{}: {error}", source.display()));
                    }
                }
                if cut && failures.is_empty() {
                    self.clipboard.clear();
                }
                self.refresh();
                if !failures.is_empty() {
                    return Err(format!("paste failed: {}", failures.join("; ")));
                }
            }
            (OperationKind::View, "files.navigate") | (OperationKind::View, "files.open") => {
                let value: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                self.open_path(PathBuf::from(value));
            }
            (_, "files.folder-command") => {
                let verb: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                match verb.as_str() {
                    "up" => self.go_up(),
                    "back" => self.history_back(),
                    "forward" => self.history_forward(),
                    "hidden" => {
                        self.apply_command(FileCommand::ToggleHidden);
                    }
                    "terminal" => self
                        .open_terminal(&mut SystemRunner)
                        .map_err(|e| format!("could not open terminal: {e}"))?,
                    _ => return Ok(false),
                }
            }
            (_, "files.entry-command") => {
                let args: Vec<String> =
                    serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let verb = args
                    .first()
                    .ok_or_else(|| "missing file command".to_string())?;
                let path = args.get(1).ok_or_else(|| "missing file path".to_string())?;
                let target = PathBuf::from(path);
                let index = self
                    .displayed_entries()
                    .iter()
                    .position(|entry| entry.path == target)
                    .ok_or_else(|| "file entry is no longer visible".to_string())?;
                self.selection.select_only(index);
                if verb == "trash" {
                    if self.is_trash_view() {
                        return Err("Use the separately confirmed permanent-delete action for items already in Trash".into());
                    }
                    fileops::delete_to_trash(&[target])?;
                    self.refresh();
                    self.structure_dirty = true;
                    return Ok(true);
                }
                let command = match verb.as_str() {
                    "copy" => FileCommand::Copy,
                    "cut" => FileCommand::Cut,
                    "properties" => FileCommand::ShowProperties,
                    "sharing" => FileCommand::ShowSharing,
                    "history" => FileCommand::ShowHistory,
                    _ => return Ok(false),
                };
                if !self.apply_command(command) {
                    return Err("file operation could not be applied".into());
                }
            }
            (OperationKind::Effect, "files.share-link") => {
                let path: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let target = PathBuf::from(path);
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
            _ => return Ok(false),
        }
        self.structure_dirty = true;
        Ok(true)
    }

    fn start_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if self.resource_invocation.is_some() {
            return Err("another file operation is still running".into());
        }
        if !self.invoke_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        if type_id == "files.share-link" {
            self.resource_invocation = Some(invocation);
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                qos_actions::ActionOutput::default(),
            )))
        }
    }

    fn poll_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
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

    fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        use automerge::ReadDoc;
        self.refresh_metadata_if_due(Instant::now());
        // Seed the inline-rename field's backing Text object *before* the
        // children sync so the rename input resolves a real object to write
        // into (the toolkit's TextInput binds to an existing Text but never
        // lazily creates one).
        let rename_obj = ensure_text_obj(state, path, RENAME_QUERY_PATH);

        // A just-started rename seeds the field with the entry's current name.
        if let Some(obj) = rename_obj.as_ref()
            && self.rename_seed_pending
        {
            ui::doc::splice_text_diffed(state, obj, &self.rename_buffer);
            self.rename_seed_pending = false;
        }

        // Keep the persistent in-place rename editor reading from the seeded
        // object; select the whole name on the first sync after a rename begins
        // so the first keystroke replaces it (Finder-style).
        self.rename_input.sync(state, path);
        if self.rename_select_pending {
            self.rename_input.select_all();
            self.rename_select_pending = false;
        }

        self.toolbar.sync(state, path);
        self.nav.sync(state, path);

        // While renaming, mirror the field's live text into our buffer so a
        // commit renames to what the user actually typed.
        if self.renaming.is_some()
            && let Some(obj) = rename_obj.as_ref()
            && let Ok(text) = state.text(obj)
        {
            self.rename_buffer = text;
        }
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        Some(self.next_metadata_refresh)
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
        self.composed_once = true;
        self.nav.set_sidebar(Some(self.build_places_pane()));
        self.nav.set_content(self.build_content_pane(theme));

        let above_node = self.toolbar.build_layout(taffy, theme, node_map);
        let nav_node = self.nav.build_layout(taffy, theme, node_map);
        // No footer band: the item count / selection status floats as a
        // transient bubble (see `draw_status_bubble`), so the body spans to the
        // window bottom.

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
        self.toolbar
            .draw(scene, transform, text_renderer, theme, presence);
        self.nav
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
        self.toolbar.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
        self.nav.draw_with_presence(
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
        // content but below any open menus / drag ghost.
        self.draw_status_bubble(scene, transform, text_renderer, theme);
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
    }

    // Primary widget event dispatcher; pointer, keyboard, drag, menu, and
    // accessibility paths converge here before entering semantic commands.
    #[allow(clippy::too_many_lines)]
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        self.poll_share_result();
        self.poll_history_result();
        let panel_was_open = self.collab_panel.is_some();
        // Track the cursor for menu placement + hit-testing.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
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
                *handled = true;
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
            *handled = true;
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

        let mut ctx = EventCtx {
            event,
            scale_factor,
            modifiers: *modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        };

        // A live drag, the open context menu, a right-click, a zoom wheel,
        // and an in-place rename all get first dibs on the event, in that
        // priority order; the first that consumes it stops routing.
        if self.handle_live_drag_event(&mut ctx)
            || self.handle_open_menu_event(&mut ctx)
            || self.handle_right_click_open_menu(&mut ctx)
            || self.handle_zoom_wheel(&mut ctx)
            || self.handle_rename_mode_event(&mut ctx)
        {
            return;
        }

        self.finish_event(&mut ctx);
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

    /// Keep ordinary Places search and add a separately bounded snapshot of
    /// collaborative records from the live mount projection. Collaborative
    /// actions carry stable record UUIDs; their current paths are provenance,
    /// never identity.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        let collaborative = crate::model::read_workspace_record_index(&self.workspace_roots);
        vec![
            Box::new(crate::search_provider::PlacesSearchProvider::new(
                self.favs.borrow().favorites.clone(),
            )),
            Box::new(
                crate::search_provider::CollaborativeRecordSearchProvider::new(
                    &collaborative.records,
                ),
            ),
        ]
    }

    /// Navigate to the place a chosen palette row identifies (its destination
    /// path is the opaque id). Navigation is app-local presentation.
    fn activate_search_result(
        &mut self,
        action_id: &str,
        _doc: &mut automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) {
        if let Some(dest) = crate::search_provider::PlacesSearchProvider::parse_action_id(action_id)
        {
            self.navigate(dest);
        } else if let Some(record_id) =
            crate::search_provider::CollaborativeRecordSearchProvider::parse_action_id(action_id)
            // Resolve mutable provenance again at activation time. A remote
            // rename or move may have landed since the palette was opened.
            && let Some(path) = crate::model::read_workspace_record_index(&self.workspace_roots)
                .records
                .into_iter()
                .find(|record| record.record_id == record_id)
                .map(|record| record.path)
        {
            if path.parent() != Some(self.current_path.as_path())
                && let Some(parent) = path.parent().map(Path::to_path_buf)
            {
                self.navigate(parent);
            }
            if let Some(index) = self.entries.iter().position(|entry| {
                entry.record_id == Some(*record_id.as_bytes()) || entry.path == path
            }) {
                self.selection.select_only(index);
                self.structure_dirty = true;
            }
        }
    }

    fn needs_animation_frame(&self) -> bool {
        // The body's docked→push / reveal slide must keep the host pumping
        // frames while it settles; the floating status bubble likewise while it
        // lingers / fades.
        self.nav.needs_animation_frame()
            || self.toolbar.needs_animation_frame()
            || self.status_bubble.needs_frame(std::time::Instant::now())
    }

    fn needs_idle_sync(&self) -> bool {
        self.nav.needs_idle_sync() || self.toolbar.needs_idle_sync()
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
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    fn traps_focus(&self) -> bool {
        self.collab_panel.is_some()
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

        Some(
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
                .with_state(AccessState {
                    busy: self.is_loading,
                    ..Default::default()
                }),
        )
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
}

#[cfg(test)]
mod open_with_tests {
    use super::*;
    use crate::model::Entry;
    use std::path::{Path, PathBuf};

    #[derive(Default)]
    struct RecRunner {
        calls: Vec<(String, Option<PathBuf>)>,
        /// Paths handed to the system default handler (non-document opens).
        opened: Vec<PathBuf>,
    }
    impl CommandRunner for RecRunner {
        fn spawn(&mut self, program: &str, _cwd: &Path) -> std::io::Result<()> {
            self.calls.push((program.to_string(), None));
            Ok(())
        }
        fn spawn_arg(&mut self, program: &str, arg: &Path, _cwd: &Path) -> std::io::Result<()> {
            self.calls
                .push((program.to_string(), Some(arg.to_path_buf())));
            Ok(())
        }
        fn open_default(&mut self, path: &Path) -> std::io::Result<()> {
            self.opened.push(path.to_path_buf());
            Ok(())
        }
    }

    fn file_entry(name: &str) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from("/docs").join(name),
            is_dir: false,
            size: 0,
            modified: None,
            mime: String::new(),
            sync_state: None,
            content_hash: None,
            record_id: None,
            availability: None,
            share: None,
            history: None,
            rich_metadata: None,
        }
    }

    #[test]
    fn document_extensions_are_recognised_as_documents() {
        for f in ["report.odt", "memo.docx", "notes.md", "readme.markdown"] {
            assert!(is_document(Path::new(f)), "{f}");
        }
        assert!(!is_document(Path::new("photo.png")));
        assert!(!is_document(Path::new("main.rs")));
    }

    /// Every activated file goes to the host, whatever its type. The
    /// explorer must not sort files into "documents" and "everything
    /// else" — which app owns a `.pdf` or a `.png` is the desktop's
    /// answer, from what is installed, and a table here would be wrong
    /// the moment anybody installs anything.
    #[test]
    fn activating_a_file_hands_the_path_to_the_host() {
        let asked: std::rc::Rc<std::cell::RefCell<Vec<PathBuf>>> = std::rc::Rc::default();
        let sink = asked.clone();
        set_document_opener(move |p| {
            sink.borrow_mut().push(p.to_path_buf());
            Ok(())
        });

        let mut w = FileExplorerWidget::new();
        w.entries = ["report.odt", "photo.png", "slides.pdf", "main.rs"]
            .iter()
            .map(|f| file_entry(f))
            .collect();
        let mut runner = RecRunner::default();
        for i in 0..4 {
            assert!(w.open_entry(i, &mut runner).unwrap(), "entry {i} opens");
        }

        assert_eq!(
            *asked.borrow(),
            vec![
                PathBuf::from("/docs/report.odt"),
                PathBuf::from("/docs/photo.png"),
                PathBuf::from("/docs/slides.pdf"),
                PathBuf::from("/docs/main.rs"),
            ],
            "the host is asked for every file, with its own path"
        );
        assert!(
            runner.calls.is_empty(),
            "the explorer launches nothing itself: {:?}",
            runner.calls
        );
        assert!(
            runner.opened.is_empty(),
            "and does not go behind the host's back to xdg-open"
        );
        assert_eq!(w.status, None, "a successful open reports nothing");
        clear_document_opener();
    }

    /// A host that cannot open the file says why, and the explorer shows
    /// it. Activation is never silent.
    #[test]
    fn a_refused_open_is_shown_to_the_user() {
        set_document_opener(|_| Err("no installed app opens .zzz files (thing.zzz)".into()));
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("thing.zzz")];
        let mut runner = RecRunner::default();
        assert!(w.open_entry(0, &mut runner).unwrap());
        let status = w
            .status
            .clone()
            .expect("the refusal reaches the status bar");
        assert!(status.contains("thing.zzz"), "must name the file: {status}");
        assert!(runner.opened.is_empty(), "a refusal is not a fallback");
        clear_document_opener();
    }

    /// With no host installed — the explorer run outside a qOS session —
    /// activation still opens the file, via the desktop's own handler,
    /// rather than doing nothing.
    #[test]
    fn without_a_host_every_type_falls_back_to_the_desktop_handler() {
        clear_document_opener();
        for f in [
            "pic.jpg",
            "movie.mp4",
            "song.flac",
            "doc.pdf",
            "main.rs",
            "report.odt",
        ] {
            let mut w = FileExplorerWidget::new();
            w.entries = vec![file_entry(f)];
            let mut runner = RecRunner::default();
            assert!(w.open_entry(0, &mut runner).unwrap(), "{f} should open");
            assert!(runner.calls.is_empty(), "{f} must not spawn a bare binary");
            assert_eq!(runner.opened, vec![PathBuf::from("/docs").join(f)], "{f}");
        }
    }

    #[test]
    fn context_menu_opens_with_selection_aware_items() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("a.txt"), file_entry("b.txt")];
        w.selection.select_only(0);
        w.open_context_menu(Point::new(40.0, 40.0));
        assert!(w.context_menu.is_open());
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(std::string::ToString::to_string))
            .collect();
        // Selection-aware actions are present.
        for expected in [
            "Open",
            "Rename",
            "Cut",
            "Copy",
            "Move to Trash",
            "New Folder",
        ] {
            assert!(
                labels.contains(&expected.to_string()),
                "menu missing {expected}: {labels:?}"
            );
        }
        // No clipboard yet → no Paste.
        assert!(!labels.contains(&"Paste".to_string()));
    }

    #[test]
    fn empty_selection_menu_omits_item_actions() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("a.txt")];
        // No selection.
        w.open_context_menu(Point::new(10.0, 10.0));
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(std::string::ToString::to_string))
            .collect();
        assert!(!labels.contains(&"Move to Trash".to_string()));
        assert!(labels.contains(&"New Folder".to_string()));
        assert!(labels.contains(&"Open Terminal Here".to_string()));
    }

    #[test]
    fn copy_then_paste_menu_includes_paste() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![file_entry("a.txt")];
        w.selection.select_only(0);
        // Simulate a copy populating the clipboard.
        w.apply_command(crate::command::FileCommand::Copy);
        assert_eq!(w.clipboard.len(), 1);
        w.open_context_menu(Point::new(10.0, 10.0));
        assert!(
            w.context_menu
                .items()
                .iter()
                .filter_map(|i| i.label().map(std::string::ToString::to_string))
                .any(|label| label == "Paste")
        );
    }
}

/// Tests for the real command-application paths — the things a user actually
/// triggers from the toolbar / context menu / keyboard, exercised through
/// `apply_command`, `sync`, and the fs mutations (never through a tautological
/// helper). These are the paths the review flagged as untested.
#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod command_path_tests {
    use super::*;
    use crate::model::Entry;
    use std::path::PathBuf;

    /// In-memory entry with a controllable name/mime (no fs touch).
    fn ent(name: &str, is_dir: bool, mime: &str) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from(name),
            is_dir,
            size: 0,
            modified: None,
            mime: mime.into(),
            sync_state: None,
            content_hash: None,
            record_id: None,
            availability: None,
            share: None,
            history: None,
            rich_metadata: None,
        }
    }

    #[test]
    fn set_sort_command_selects_the_key_including_type() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![
            ent("b.png", false, "image/png"),
            ent("a.txt", false, "text/plain"),
        ];
        // SortBy::Type is ONLY reachable via this command (no column/button),
        // so exercising it proves the previously-unreachable key now works.
        assert!(w.apply_command(FileCommand::SetSort(SortBy::Type)));
        assert_eq!(w.sort_by(), SortBy::Type);
        assert_eq!(
            w.entries_snapshot()[0].name,
            "b.png",
            "image/png < text/plain"
        );

        assert!(w.apply_command(FileCommand::SetSort(SortBy::Name)));
        assert_eq!(w.sort_by(), SortBy::Name);
        assert_eq!(w.entries_snapshot()[0].name, "a.txt");
    }

    #[test]
    fn trash_view_menu_offers_restore_and_empty_not_move_to_trash() {
        let Some(trash) = crate::fileops::trash_dir() else {
            return;
        };
        let mut w = FileExplorerWidget::new();
        // Pretend we are viewing the Trash (without touching the real trash).
        w.current_path = trash;
        w.entries = vec![ent("deleted.txt", false, "text/plain")];
        w.selection.select_only(0);
        w.open_context_menu(Point::new(5.0, 5.0));
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(str::to_string))
            .collect();
        assert!(
            labels.contains(&"Restore".to_string()),
            "trash menu offers Restore: {labels:?}"
        );
        assert!(
            labels.contains(&"Empty Trash".to_string()),
            "and Empty Trash: {labels:?}"
        );
        assert!(
            labels.contains(&"Delete Permanently".to_string()),
            "and a permanent delete"
        );
        assert!(
            !labels.contains(&"Move to Trash".to_string()),
            "not re-trash (already trashed)"
        );
        assert!(
            !labels.contains(&"Rename".to_string()),
            "no in-place editing in the trash"
        );
    }

    #[test]
    fn trash_actions_require_an_explicit_irrevocable_confirmation() {
        let Some(trash) = crate::fileops::trash_dir() else {
            return;
        };
        let mut w = FileExplorerWidget::new();
        // Keep this entirely in-memory: the assertion is the interaction
        // boundary, before Files ever calls the OS trash implementation.
        w.current_path = trash;
        w.entries = vec![ent("deleted.txt", false, "text/plain")];
        w.selection.select_only(0);

        assert!(w.apply_command(FileCommand::Delete));
        let panel = w.collab_panel.as_ref().expect("permanent delete warning");
        assert!(matches!(
            panel.action.as_ref(),
            Some(CollabPanelAction::PermanentDelete(_))
        ));
        assert_eq!(
            w.collab_panel_action_command(),
            Some(FileCommand::ConfirmPermanentDelete {
                paths: vec![PathBuf::from("deleted.txt")],
            }),
            "only the named warning action may invoke the purge"
        );
        let dialog = Widget::accessibility(&w).expect("warning has semantics");
        assert_eq!(dialog.role, ui::a11y::Role::Dialog);
        assert_eq!(
            dialog.children.last().and_then(|node| node.name.as_deref()),
            Some("Delete Permanently")
        );

        w.close_collab_panel();
        assert!(w.apply_command(FileCommand::EmptyTrash));
        assert!(matches!(
            w.collab_panel
                .as_ref()
                .and_then(|panel| panel.action.as_ref()),
            Some(CollabPanelAction::EmptyTrash)
        ));
        assert_eq!(
            w.collab_panel_action_command(),
            Some(FileCommand::ConfirmEmptyTrash),
            "emptying Trash must use a separate explicit action"
        );
    }

    #[test]
    fn context_menu_exposes_the_sort_section_including_type() {
        let mut w = FileExplorerWidget::new();
        w.entries = vec![ent("a.txt", false, "text/plain")];
        w.open_context_menu(Point::new(5.0, 5.0));
        let labels: Vec<String> = w
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(str::to_string))
            .collect();
        for s in [
            "Sort by Name",
            "Sort by Size",
            "Sort by Type",
            "Sort by Date",
        ] {
            assert!(
                labels.contains(&s.to_string()),
                "menu missing {s}: {labels:?}"
            );
        }
    }

    #[test]
    fn error_state_composition_adds_exactly_the_retry_button() {
        use std::collections::HashMap;
        use taffy::{NodeId, TaffyTree};
        use ui::theme::Theme;

        // Build the real content pane (the file-view detail, where the retry
        // affordance lives) and count its buttons. The error flag is the only
        // structural difference, so the delta is precisely the "Try Again"
        // button `build_content_pane` adds.
        fn button_count(err: bool) -> usize {
            let mut w = FileExplorerWidget::new();
            if err {
                w.error = Some("could not read directory".into());
            }
            let theme = Theme::dark();
            let mut pane = w.build_content_pane(&theme);
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            pane.build_layout(&mut taffy, &theme, &mut nm);
            nm.values()
                .filter(|&&p| unsafe { (*p).name() } == "Button")
                .count()
        }
        assert_eq!(
            button_count(true),
            button_count(false) + 1,
            "error view must add the retry button"
        );
    }

    #[test]
    fn retry_refresh_recovers_from_error_once_the_dir_exists() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("appears-later");
        let mut w = FileExplorerWidget::new();
        w.navigate(target.clone());
        assert!(w.error_message().is_some(), "missing dir sets an error");
        // The "Try Again" button pushes Refresh — still errors while missing.
        assert!(w.apply_command(FileCommand::Refresh));
        assert!(w.error_message().is_some());
        // Once the directory exists, the same retry clears the error.
        std::fs::create_dir(&target).unwrap();
        assert!(w.apply_command(FileCommand::Refresh));
        assert!(
            w.error_message().is_none(),
            "retry succeeds once the dir exists"
        );
    }

    #[test]
    fn rename_flow_renames_the_selected_file_on_disk() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("old.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "old.txt")
            .unwrap();
        w.select(Some(idx));

        // Begin the rename via the real command; the bar seeds the name.
        assert!(w.apply_command(FileCommand::Rename));
        assert!(w.is_renaming());
        assert_eq!(w.rename_buffer(), "old.txt");

        // Type a new name + commit through the command path.
        w.set_rename_buffer("new.txt".into());
        assert!(w.apply_command(FileCommand::RenameCommit));
        assert!(!w.is_renaming(), "commit closes the rename bar");
        assert!(d.path().join("new.txt").exists(), "file renamed on disk");
        assert!(!d.path().join("old.txt").exists());
    }

    #[test]
    fn reseeding_the_rename_field_splices_only_the_changed_span() {
        use automerge::ReadDoc;
        // Every rename re-seeds the *same* backing Text object with the new
        // entry's name. Clearing and refilling it tombstones the whole name
        // each time, so a session that renames a few dozen files pays for all
        // of them forever.
        let d = tempfile::tempdir().unwrap();
        for i in 0..40 {
            std::fs::write(d.path().join(format!("report-{i:03}.txt")), b"x").unwrap();
        }
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());

        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        // First seed creates the object; measure growth from there.
        w.set_rename_buffer("report-000.txt".into());
        w.sync(&mut doc, &root);
        let baseline = doc.save().len();

        for i in 1..40 {
            w.set_rename_buffer(format!("report-{i:03}.txt"));
            w.sync(&mut doc, &root);
        }
        let grown = doc.save().len() - baseline;

        let obj = ensure_text_obj(&mut doc, &root, RENAME_QUERY_PATH).unwrap();
        assert_eq!(
            doc.text(&obj).unwrap(),
            "report-039.txt",
            "the field must hold the last seeded name"
        );
        assert!(
            grown < 39 * "report-000.txt".len(),
            "39 rename re-seeds grew the document by {grown} bytes — the seed is \
             rewriting the whole field instead of splicing the diff"
        );
    }

    #[test]
    fn rename_commit_is_a_noop_for_blank_or_unchanged_names() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("keep.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "keep.txt")
            .unwrap();

        w.select(Some(idx));
        w.begin_rename();
        w.set_rename_buffer("   ".into());
        assert!(!w.commit_rename(), "blank name does not rename");
        assert!(!w.is_renaming());
        assert!(d.path().join("keep.txt").exists());

        w.select(Some(idx));
        w.begin_rename();
        w.set_rename_buffer("keep.txt".into());
        assert!(!w.commit_rename(), "unchanged name does not rename");
        assert!(d.path().join("keep.txt").exists());
    }

    #[test]
    fn rename_cancel_leaves_the_file_untouched() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("stay.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "stay.txt")
            .unwrap();
        w.select(Some(idx));
        w.apply_command(FileCommand::Rename);
        w.set_rename_buffer("other.txt".into());
        assert!(w.apply_command(FileCommand::RenameCancel));
        assert!(!w.is_renaming());
        assert!(d.path().join("stay.txt").exists());
        assert!(!d.path().join("other.txt").exists());
    }

    #[test]
    fn new_folder_creates_the_folder_and_opens_inline_rename() {
        let d = tempfile::tempdir().unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        assert!(w.apply_command(FileCommand::NewFolder));
        assert!(
            d.path().join("New Folder").is_dir(),
            "folder actually created"
        );
        assert!(w.is_renaming(), "new folder drops straight into rename");
        assert_eq!(w.rename_buffer(), "New Folder");
    }

    #[test]
    fn external_new_folder_uses_the_typed_name_without_rename_ui() {
        let d = tempfile::tempdir().unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let doc = automerge::AutoCommit::new();
        assert!(
            Widget::invoke_resource_operation(
                &mut w,
                OperationKind::Effect,
                "files.new-folder",
                &serde_json::to_vec("Agent Created").unwrap(),
                &doc,
                &automerge::ROOT,
            )
            .unwrap()
        );
        assert!(d.path().join("Agent Created").is_dir());
        assert!(!w.is_renaming());
    }

    #[test]
    fn exported_background_operations_have_valid_job_contracts() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("shared.txt"), b"x").unwrap();
        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        let resources = Widget::resources(&w, &automerge::AutoCommit::new());
        for action in resources.iter().flat_map(|resource| {
            resource.actions.iter().chain(
                resource
                    .children
                    .iter()
                    .flat_map(|child| child.actions.iter()),
            )
        }) {
            if matches!(action.contract.execution, ExecutionMode::Background { .. }) {
                assert_eq!(action.contract.result, ResultSpec::Job, "{}", action.id);
                assert!(
                    action.contract.validate_declaration().is_empty(),
                    "{}",
                    action.id
                );
            }
        }
    }

    #[test]
    fn copy_then_paste_duplicates_the_file_into_the_current_dir() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        let dest = d.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(src.clone());
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "a.txt")
            .unwrap();
        w.select(Some(idx));
        assert!(w.apply_command(FileCommand::Copy));
        w.navigate(dest.clone());
        assert!(w.apply_command(FileCommand::Paste));
        assert!(dest.join("a.txt").exists(), "copied into dest");
        assert!(src.join("a.txt").exists(), "original kept (copy, not move)");
    }

    #[test]
    fn cut_then_paste_moves_the_file_into_the_current_dir() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("m.txt"), b"x").unwrap();
        let dest = d.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(src.clone());
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "m.txt")
            .unwrap();
        w.select(Some(idx));
        assert!(w.apply_command(FileCommand::Cut));
        w.navigate(dest.clone());
        assert!(w.apply_command(FileCommand::Paste));
        assert!(dest.join("m.txt").exists(), "moved into dest");
        assert!(!src.join("m.txt").exists(), "original removed (move)");
    }

    #[test]
    fn history_back_and_forward_traverse_visited_dirs() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        std::fs::create_dir(&a).unwrap();
        let b = d.path().join("b");
        std::fs::create_dir(&b).unwrap();

        let mut w = FileExplorerWidget::new();
        w.navigate(d.path().to_path_buf());
        w.navigate(a.clone());
        w.navigate(b.clone());
        assert_eq!(w.current_path(), b);
        w.history_back();
        assert_eq!(w.current_path(), a, "back to the previously-visited dir");
        w.history_back();
        assert_eq!(w.current_path(), d.path());
        w.history_forward();
        assert_eq!(w.current_path(), a, "forward re-visits");
    }

    /// A hermetic widget whose favourites/zoom persist to a throwaway dir, so
    /// tests never touch the user's real `~/.config`.
    fn hermetic() -> (FileExplorerWidget, tempfile::TempDir) {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        (w, cfg)
    }

    #[test]
    fn double_click_click_command_navigates_into_a_directory() {
        // Drives the real pointer path: a `Click` command, then a second within
        // the double-click window → the app opens (navigates into) the folder.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let (mut w, _cfg) = hermetic();
        w.navigate(d.path().to_path_buf());
        let idx = w
            .displayed_entries()
            .iter()
            .position(|e| e.name == "sub")
            .unwrap();

        w.apply_command(FileCommand::Click {
            index: idx,
            ctrl: false,
            shift: false,
        });
        assert_eq!(w.current_path(), d.path(), "first click only selects");
        assert_eq!(w.selected_index(), Some(idx));

        w.apply_command(FileCommand::Click {
            index: idx,
            ctrl: false,
            shift: false,
        });
        assert_eq!(
            w.current_path(),
            d.path().join("sub"),
            "double-click opens the directory"
        );
    }

    #[test]
    fn zoom_commands_step_the_item_size_and_clamp() {
        let (mut w, _cfg) = hermetic();
        let base = w.zoom_level();
        w.apply_command(FileCommand::ZoomIn);
        assert_eq!(w.zoom_level(), base + 1, "zoom in raises the step");
        w.apply_command(FileCommand::ZoomOut);
        w.apply_command(FileCommand::ZoomOut);
        assert_eq!(w.zoom_level(), base - 1, "zoom out lowers the step");
        // Clamps at the extremes.
        for _ in 0..50 {
            w.apply_command(FileCommand::ZoomOut);
        }
        assert_eq!(
            w.zoom_level(),
            config::ZOOM_MIN,
            "zoom clamps at the minimum"
        );
    }

    #[test]
    fn zoom_level_persists_across_reconstruction() {
        let cfg = tempfile::tempdir().unwrap();
        {
            let mut w = FileExplorerWidget::with_persistence(
                Some(cfg.path().to_path_buf()),
                Some(PathBuf::from("/home/tester")),
            );
            w.apply_command(FileCommand::ZoomIn);
            w.apply_command(FileCommand::ZoomIn);
            assert_eq!(w.zoom_level(), 2);
        }
        // A fresh widget reads the persisted level back.
        let w2 = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        assert_eq!(w2.zoom_level(), 2, "zoom level survives a restart");
    }

    #[test]
    fn favorite_commands_add_reorder_and_remove_and_persist() {
        let cfg = tempfile::tempdir().unwrap();
        let make = || {
            FileExplorerWidget::with_persistence(
                Some(cfg.path().to_path_buf()),
                Some(PathBuf::from("/home/tester")),
            )
        };
        let sub = PathBuf::from("/home/tester/Projects/qos");
        let mut w = make();
        let before = w.favorites().len();
        w.apply_command(FileCommand::AddFavorite(sub.clone()));
        assert_eq!(w.favorites().len(), before + 1, "added one favourite");
        assert_eq!(w.favorites().last().unwrap().path, sub);

        // Reorder: drag the new favourite (last) up to index 1.
        let last = w.favorites().len() - 1;
        w.apply_command(FileCommand::ReorderFavorite { from: last, to: 1 });
        assert_eq!(w.favorites()[1].path, sub, "reorder moved it to slot 1");

        // The new order + membership persists to a fresh widget.
        let w2 = make();
        assert_eq!(w2.favorites()[1].path, sub, "reordered favourite persists");

        // Remove drops it (and persists).
        let mut w3 = make();
        w3.apply_command(FileCommand::RemoveFavorite(sub.clone()));
        assert!(!w3.favorites().iter().any(|f| f.path == sub));
        let w4 = make();
        assert!(
            !w4.favorites().iter().any(|f| f.path == sub),
            "removal persists"
        );
    }

    #[test]
    fn favourites_follow_localized_xdg_user_dirs() {
        use crate::favorites::{FavKind, UserDirs};
        let cfg = tempfile::tempdir().unwrap();
        let home = PathBuf::from("/home/tester");
        // A German desktop: the XDG user dirs resolve to localized folder names.
        let dirs = UserDirs {
            home: Some(home.clone()),
            documents: Some(home.join("Dokumente")),
            downloads: Some(home.join("Downloads")),
            pictures: Some(home.join("Bilder")),
            music: Some(home.join("Musik")),
            videos: Some(home.join("Videos")),
            ..Default::default()
        };
        let w = FileExplorerWidget::with_persistence_dirs(
            Some(cfg.path().to_path_buf()),
            Some(home.clone()),
            dirs,
        );
        let labels: Vec<String> = w.favorites().iter().map(|f| f.label.clone()).collect();
        // The sidebar shows the user's real localized names, not hard-coded
        // English ones.
        assert!(
            labels.contains(&"Dokumente".to_string()),
            "labels: {labels:?}"
        );
        assert!(labels.contains(&"Bilder".to_string()), "labels: {labels:?}");
        assert!(labels.contains(&"Musik".to_string()), "labels: {labels:?}");
        assert!(
            !labels.contains(&"Documents".to_string()),
            "must not show hard-coded English Documents"
        );
        // The Documents favourite points at the actual localized directory (so
        // clicking it opens ~/Dokumente, where the files really are) and keeps
        // its evocative glyph despite the localized label.
        let docs = w
            .favorites()
            .into_iter()
            .find(|f| f.kind == FavKind::Documents)
            .unwrap();
        assert_eq!(docs.path, home.join("Dokumente"));
        assert_eq!(docs.icon(), ui::icons::Icon::FileText);
    }

    #[test]
    fn new_from_template_creates_the_file_and_opens_rename() {
        let d = tempfile::tempdir().unwrap();
        let tmpl = d.path().join("Doc.md");
        std::fs::write(&tmpl, b"template body").unwrap();
        let work = d.path().join("work");
        std::fs::create_dir(&work).unwrap();

        let (mut w, _cfg) = hermetic();
        w.navigate(work.clone());
        w.apply_command(FileCommand::NewFromTemplate(tmpl));
        assert!(
            work.join("Doc.md").exists(),
            "template copied into the current dir"
        );
        assert_eq!(
            std::fs::read(work.join("Doc.md")).unwrap(),
            b"template body"
        );
        assert!(
            w.is_renaming(),
            "new file drops straight into inline-rename"
        );
    }

    #[test]
    fn toggle_hidden_reveals_and_hides_dotfiles() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".secret"), b"x").unwrap();
        std::fs::write(d.path().join("visible.txt"), b"x").unwrap();
        let (mut w, _cfg) = hermetic();
        w.navigate(d.path().to_path_buf());
        assert!(
            !w.entries_snapshot().iter().any(|e| e.name == ".secret"),
            "dotfiles hidden by default"
        );
        w.apply_command(FileCommand::ToggleHidden);
        assert!(w.show_hidden());
        assert!(
            w.entries_snapshot().iter().any(|e| e.name == ".secret"),
            "dotfiles shown after toggle"
        );
        w.apply_command(FileCommand::ToggleHidden);
        assert!(
            !w.entries_snapshot().iter().any(|e| e.name == ".secret"),
            "toggled back off"
        );
    }

    /// A file manager pointed at a directory that has files in it lists
    /// those files.
    ///
    /// Asserts the *exact* set rather than "contains one of them". The
    /// frame this guards showed "This folder is empty" over a directory
    /// that held five entries, and an `.iter().any(|e| e.name == …)`
    /// probe for a single name — the shape every other listing test in
    /// this file uses — would still have passed on a listing that had
    /// lost the other four.
    ///
    /// The empty-state banner is a draw-only side effect with no
    /// accessor, so the assertions are on the two conditions that gate
    /// it (a non-empty `entries`, no error) plus the item count the same
    /// view renders in its footer.
    #[test]
    fn a_populated_directory_lists_exactly_its_contents() {
        let d = tempfile::tempdir().unwrap();
        let names = [
            "alpha.txt",
            "beta.md",
            "delta.rs",
            "epsilon.png",
            "gamma.odt",
        ];
        for n in names {
            std::fs::write(d.path().join(n), b"x").unwrap();
        }

        let (mut w, _cfg) = hermetic();
        w.navigate(d.path().to_path_buf());

        assert_eq!(
            w.error_message(),
            None,
            "a directory that reads cleanly reports no error",
        );

        let mut listed: Vec<String> = w
            .entries_snapshot()
            .iter()
            .map(|e| e.name.clone())
            .collect();
        listed.sort();
        let mut expected: Vec<String> = names.iter().map(|s| (*s).to_string()).collect();
        expected.sort();
        assert_eq!(
            listed, expected,
            "every file in the directory is listed, and nothing else is",
        );

        let (count, _path) = w.footer_text();
        assert_eq!(
            count,
            names.len(),
            "the footer counts the same entries the list shows",
        );
    }

    #[test]
    fn scroll_position_survives_a_structural_rebuild_but_resets_on_navigation() {
        // The real fix: selecting a row / re-sorting rebuilds the whole
        // composition (a fresh Scrollable), which used to snap the list back to
        // the top. The persisted offset cell — the exact one the rebuilt
        // Scrollable binds to via `with_persisted_offset` — must carry the
        // position across those rebuilds, and only reset when the directory
        // actually changes.
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        std::fs::create_dir(&a).unwrap();
        for i in 0..6 {
            std::fs::write(a.join(format!("f{i}.txt")), b"x").unwrap();
        }
        let b = d.path().join("b");
        std::fs::create_dir(&b).unwrap();

        let (mut w, _cfg) = hermetic();
        w.navigate(a);
        // Simulate the list having been scrolled down 240px.
        w.scroll_offset.set((0.0, 240.0));

        // Selecting a row is a structural rebuild — scroll must hold.
        w.apply_command(FileCommand::Click {
            index: 0,
            ctrl: false,
            shift: false,
        });
        assert!(
            (w.scroll_offset.get().1 - 240.0).abs() <= f32::EPSILON,
            "selecting a row keeps the scroll"
        );
        // Re-sorting is a structural rebuild — scroll must hold.
        w.apply_command(FileCommand::SetSort(SortBy::Size));
        assert!(
            (w.scroll_offset.get().1 - 240.0).abs() <= f32::EPSILON,
            "re-sorting keeps the scroll"
        );

        // Navigating to a different directory starts at the top.
        w.navigate(b);
        assert_eq!(
            w.scroll_offset.get(),
            (0.0, 0.0),
            "a new directory scrolls to the top"
        );
    }

    #[test]
    fn context_select_targets_the_clicked_row_not_a_stale_selection() {
        // Guards the dangerous bug: a right-click's menu must act on the row it
        // landed on, not whatever happened to be selected. `ContextSelect` is
        // the command the content pane pushes on a right-press.
        let (mut w, _cfg) = hermetic();
        w.entries = vec![
            ent("a.txt", false, "text/plain"),
            ent("b.txt", false, "text/plain"),
            ent("c.txt", false, "text/plain"),
        ];
        // Stale selection on row 0.
        w.selection.select_only(0);
        // Right-press on row 2 moves the selection there.
        assert!(w.apply_command(FileCommand::ContextSelect(Some(2))));
        assert_eq!(
            w.selected_indices(),
            vec![2],
            "context select targets the clicked row"
        );

        // A right-press inside an existing multi-selection preserves all of it
        // (so a menu action applies to every highlighted item).
        w.selection.select_only(1);
        w.selection.toggle(2);
        w.apply_command(FileCommand::ContextSelect(Some(1)));
        let mut got = w.selected_indices();
        got.sort_unstable();
        assert_eq!(
            got,
            vec![1, 2],
            "right-click within a multi-selection keeps it"
        );

        // A right-press on empty space clears the selection.
        w.apply_command(FileCommand::ContextSelect(None));
        assert!(
            w.selected_indices().is_empty(),
            "empty-space right-click clears selection"
        );
    }

    #[test]
    fn context_menu_gates_new_file_on_templates_existing() {
        // The "New File ▸" entry appears iff the Templates dir has files, and
        // "New Folder" is always present. Templates are injected so the check is
        // hermetic (no reliance on the ambient Templates dir).
        let (w, _cfg) = hermetic();
        let has_new_file = |spec: &[(String, Option<FileCommand>)]| {
            spec.iter().any(|(label, _)| label.starts_with("New File"))
        };
        let has_new_folder = |spec: &[(String, Option<FileCommand>)]| {
            spec.iter().any(|(label, _)| label == "New Folder")
        };

        // Empty Templates dir → no "New File", but "New Folder" stays.
        let none = w.context_menu_spec(&[]);
        assert!(!has_new_file(&none), "no templates → submenu hidden");
        assert!(has_new_folder(&none), "New Folder is always offered");

        // Two templates → the "New File ▸" entry appears.
        let two = w.context_menu_spec(&[PathBuf::from("Doc.md"), PathBuf::from("Sheet.csv")]);
        assert!(
            has_new_file(&two),
            "templates present → New File entry shown"
        );
        assert!(has_new_folder(&two));
    }

    #[test]
    fn collaborative_selection_offers_and_applies_keep_offline() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("shared.txt");
        std::fs::write(&file, b"shared").unwrap();
        let availability = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
            "current_version": null,
            "residency": {"state": "not_resident"},
            "transfer": {"phase": "idle", "progress": null, "last_error": null,
                "last_error_at_ms": null, "error_truncated": false},
            "policy": "automatic",
            "replicas": {"state": "unknown"},
            "freshness": {"metadata_observed_at_ms": null, "local_verified_at_ms": null,
                "replicas_observed_at_ms": null}
        }))
        .unwrap();
        xattr::set(&file, crate::model::AVAILABILITY_XATTR, &availability).unwrap();

        let mut w = FileExplorerWidget::new();
        w.current_path = dir.path().to_path_buf();
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.path = file.clone();
        entry.availability = crate::model::read_availability(&file);
        w.entries = vec![entry];
        w.selection.select_only(0);
        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Keep Offline" && *command == Some(FileCommand::KeepOffline)
        }));
        assert!(w.apply_command(FileCommand::KeepOffline));
        assert_eq!(
            xattr::get(&file, crate::model::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"pinned".as_slice())
        );
    }

    #[test]
    fn free_local_space_is_offered_and_sent_only_for_eligible_local_files() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local.txt");
        let remote = dir.path().join("remote.txt");
        std::fs::write(&local, b"local").unwrap();
        std::fs::write(&remote, b"remote").unwrap();
        let availability = |state: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": uuid::Uuid::new_v4(),
                "current_version": null,
                "residency": state,
                "transfer": {"phase": "idle", "progress": null, "last_error": null,
                    "last_error_at_ms": null, "error_truncated": false},
                "policy": "automatic",
                "policy_inherited": false,
                "replicas": {"state": "unknown"},
                "freshness": {"metadata_observed_at_ms": null, "local_verified_at_ms": null,
                    "replicas_observed_at_ms": null}
            }))
            .unwrap()
        };
        xattr::set(
            &local,
            crate::model::AVAILABILITY_XATTR,
            &availability(serde_json::json!({"state": "resident", "bytes": 5})),
        )
        .unwrap();
        xattr::set(
            &remote,
            crate::model::AVAILABILITY_XATTR,
            &availability(serde_json::json!({"state": "not_resident"})),
        )
        .unwrap();

        let mut widget = FileExplorerWidget::new();
        widget.current_path = dir.path().to_path_buf();
        widget.entries = [("local.txt", &local), ("remote.txt", &remote)]
            .into_iter()
            .map(|(name, path)| {
                let mut entry = ent(name, false, "text/plain");
                entry.path = path.clone();
                entry.availability = crate::model::read_availability(path);
                entry
            })
            .collect();
        widget.selection.select_all(2);

        assert!(
            widget
                .context_menu_spec(&[])
                .iter()
                .any(|(label, command)| {
                    label == "Free Local Space" && *command == Some(FileCommand::FreeLocalSpace)
                })
        );
        assert!(widget.apply_command(FileCommand::FreeLocalSpace));
        assert_eq!(
            xattr::get(&local, crate::model::AVAILABILITY_CONTROL_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"evict".as_slice())
        );
        assert_eq!(
            xattr::get(&remote, crate::model::AVAILABILITY_CONTROL_XATTR).unwrap(),
            None
        );
    }

    #[test]
    fn scheduled_metadata_refresh_preserves_record_selection_and_scroll() {
        let dir = tempfile::tempdir().unwrap();
        // Production discovers this from the mounted workspace root. Mark the
        // isolated fixture as a collaborative root so the listing reads the
        // record/sync xattrs whose refresh behavior this test exercises.
        let previous_roots = std::env::var_os("QOS_WORKSPACE_ROOTS");
        unsafe { std::env::set_var("QOS_WORKSPACE_ROOTS", dir.path()) };
        let before = dir.path().join("before.txt");
        let after = dir.path().join("after.txt");
        std::fs::write(&before, b"shared").unwrap();
        let record = uuid::Uuid::from_u128(0x5151);
        xattr::set(&before, crate::model::RECORD_ID_XATTR, record.as_bytes()).unwrap();
        xattr::set(&before, crate::model::SYNC_XATTR, b"projected").unwrap();

        let mut widget = FileExplorerWidget::new();
        widget.load(dir.path().to_path_buf());
        widget.selection.select_only(0);
        widget.scroll_offset.set((0.0, 137.0));
        std::fs::rename(&before, &after).unwrap();
        xattr::set(&after, crate::model::SYNC_XATTR, b"local").unwrap();
        widget.next_metadata_refresh = Instant::now();

        assert!(widget.refresh_metadata_if_due(Instant::now()));
        assert_eq!(widget.entries[0].path, after);
        assert_eq!(
            widget.entries[0].sync_state,
            Some(crate::model::SyncState::Local)
        );
        assert_eq!(widget.selection.selected(), vec![0]);
        assert_eq!(widget.selection.primary(), Some(0));
        assert_eq!(widget.scroll_offset.get(), (0.0, 137.0));
        if let Some(previous_roots) = previous_roots {
            unsafe { std::env::set_var("QOS_WORKSPACE_ROOTS", previous_roots) };
        } else {
            unsafe { std::env::remove_var("QOS_WORKSPACE_ROOTS") };
        }
    }

    #[test]
    fn mixed_selection_exposes_both_storage_policies_and_writes_only_eligible_records() {
        let dir = tempfile::tempdir().unwrap();
        let automatic = dir.path().join("automatic.txt");
        let pinned = dir.path().join("pinned.txt");
        let ordinary = dir.path().join("ordinary.txt");
        for path in [&automatic, &pinned, &ordinary] {
            std::fs::write(path, b"data").unwrap();
        }
        let availability = |policy: &str, inherited: bool| {
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "current_version": null,
                "residency": {"state": "resident", "bytes": 4},
                "transfer": {"phase": "idle", "progress": null, "last_error": null,
                    "last_error_at_ms": null, "error_truncated": false},
                "policy": policy,
                "policy_inherited": inherited,
                "replicas": {"state": "unknown"},
                "freshness": {"metadata_observed_at_ms": null, "local_verified_at_ms": null,
                    "replicas_observed_at_ms": null}
            }))
            .unwrap()
        };
        xattr::set(
            &automatic,
            crate::model::AVAILABILITY_XATTR,
            &availability("automatic", false),
        )
        .unwrap();
        xattr::set(
            &pinned,
            crate::model::AVAILABILITY_XATTR,
            &availability("pinned", false),
        )
        .unwrap();

        let mut w = FileExplorerWidget::new();
        w.current_path = dir.path().to_path_buf();
        w.entries = [
            ("automatic.txt", &automatic),
            ("pinned.txt", &pinned),
            ("ordinary.txt", &ordinary),
        ]
        .into_iter()
        .map(|(name, path)| {
            let mut entry = ent(name, false, "text/plain");
            entry.path = path.clone();
            entry.availability = crate::model::read_availability(path);
            entry
        })
        .collect();
        w.selection.select_all(3);

        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Keep Offline" && *command == Some(FileCommand::KeepOffline)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Use Automatic Storage" && *command == Some(FileCommand::RemoveOfflinePin)
        }));

        w.apply_command(FileCommand::KeepOffline);
        assert_eq!(
            xattr::get(&automatic, crate::model::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"pinned".as_slice())
        );
        assert_eq!(xattr::get(&pinned, crate::model::PIN_XATTR).unwrap(), None);
        assert_eq!(
            xattr::get(&ordinary, crate::model::PIN_XATTR).unwrap(),
            None
        );

        // `refresh` deliberately treats this temp directory as an ordinary
        // non-workspace path. Restore the directly-read projections so the
        // second half can exercise the opposite command independently.
        for entry in &mut w.entries {
            entry.availability = crate::model::read_availability(&entry.path);
        }
        w.selection.select_all(w.entries.len());
        w.apply_command(FileCommand::RemoveOfflinePin);
        assert_eq!(
            xattr::get(&pinned, crate::model::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"automatic".as_slice())
        );
        assert_eq!(
            xattr::get(&automatic, crate::model::PIN_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"pinned".as_slice()),
            "the opposite policy action must not rewrite an automatic record"
        );
        assert_eq!(
            xattr::get(&ordinary, crate::model::PIN_XATTR).unwrap(),
            None
        );
    }

    #[test]
    fn inherited_pin_is_explained_but_not_offered_as_a_local_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("inherited.txt");
        std::fs::write(&file, b"data").unwrap();
        let availability = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
            "current_version": null,
            "residency": {"state": "resident", "bytes": 4},
            "transfer": {"phase": "idle", "progress": null, "last_error": null,
                "last_error_at_ms": null, "error_truncated": false},
            "policy": "pinned",
            "policy_inherited": true,
            "replicas": {"state": "unknown"},
            "freshness": {"metadata_observed_at_ms": null, "local_verified_at_ms": null,
                "replicas_observed_at_ms": null}
        }))
        .unwrap();
        xattr::set(&file, crate::model::AVAILABILITY_XATTR, &availability).unwrap();

        let mut w = FileExplorerWidget::new();
        w.current_path = dir.path().to_path_buf();
        let mut entry = ent("inherited.txt", false, "text/plain");
        entry.path = file.clone();
        entry.availability = crate::model::read_availability(&file);
        w.entries = vec![entry];
        w.selection.select_only(0);

        let spec = w.context_menu_spec(&[]);
        assert!(
            spec.iter().any(|(label, command)| {
                label == "Pinned by Parent Folder" && command.is_none()
            })
        );
        assert!(
            !spec
                .iter()
                .any(|(_, command)| { *command == Some(FileCommand::RemoveOfflinePin) })
        );
        w.apply_command(FileCommand::RemoveOfflinePin);
        assert_eq!(xattr::get(&file, crate::model::PIN_XATTR).unwrap(), None);
    }

    #[test]
    fn failed_projected_file_offers_retry_and_availability_details() {
        let mut entry = ent("offline.txt", false, "text/plain");
        entry.availability = crate::model::AvailabilityProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "current_version": null,
                "residency": {"state": "not_resident"},
                "transfer": {"phase": "idle", "progress": null,
                    "last_error": "No available replica could provide this file",
                    "last_error_at_ms": 12, "error_truncated": false},
                "policy": "automatic",
                "replicas": {"state": "known", "reachable": 0, "durable": 1},
                "freshness": {"metadata_observed_at_ms": 10,
                    "local_verified_at_ms": null, "replicas_observed_at_ms": 11}
            }))
            .unwrap(),
        );
        let mut w = FileExplorerWidget::new();
        w.entries = vec![entry];
        w.selection.select_only(0);

        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Retry Download" && *command == Some(FileCommand::RetryTransfer)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Availability…" && *command == Some(FileCommand::ShowAvailability)
        }));

        w.apply_command(FileCommand::ShowAvailability);
        let panel = w.collab_panel.as_ref().expect("availability panel");
        assert!(panel.summary.contains("Not stored"));
        assert!(panel.lines.iter().any(|line| line.contains("durable")));
        assert!(
            panel
                .lines
                .iter()
                .any(|line| line.contains("No available replica"))
        );
    }

    #[test]
    fn authority_sharing_and_history_open_real_detail_panels() {
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.record_id =
            Some(*uuid::Uuid::from_u128(0xb0d4_e23f_c7cb_4ded_a427_b32c_f9da_9341).as_bytes());
        entry.rich_metadata = crate::model::MetadataProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "properties": {"schema_version": 1, "image_width": 1920,
                    "image_height": 1080, "duration_ms": 65000,
                    "page_count": 4, "codec": "h264", "container": "mp4"}
            }))
            .unwrap(),
        );
        entry.share = crate::model::ShareProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "observed_at_ms": 10,
                "grants": [{
                    "grant_id": "c0d4e23f-c7cb-4ded-a427-b32cf9da9343",
                    "subject": {"kind": "bearer_link"},
                    "permission": "View",
                    "expires_at_ms": null,
                    "revoked": false,
                    "expired": false,
                    "used": false,
                    "exhausted": false,
                    "status": "active",
                    "redemptions": 0,
                    "max_redemptions": 1
                }]
            }))
            .unwrap(),
        );
        entry.history = crate::model::HistoryProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "total_versions": 1,
                "versions_truncated": false,
                "heads": ["a0d4e23f-c7cb-4ded-a427-b32cf9da9342"],
                "versions": [{
                    "id": "a0d4e23f-c7cb-4ded-a427-b32cf9da9342",
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [],
                    "actor": {"person": vec![1; 32], "device": vec![2; 32]},
                    "timestamp_ms": 9,
                    "content_hash": null,
                    "metadata_hash": vec![3; 32],
                    "provenance": "Create"
                }],
                "conflicts_truncated": false,
                "conflicts": []
            }))
            .unwrap(),
        );
        let mut w = FileExplorerWidget::new();
        w.entries = vec![entry];
        w.selection.select_only(0);
        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Sharing & Access…" && *command == Some(FileCommand::ShowSharing)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Version History…" && *command == Some(FileCommand::ShowHistory)
        }));
        assert!(spec.iter().any(|(label, command)| {
            label == "Properties…" && *command == Some(FileCommand::ShowProperties)
        }));

        w.apply_command(FileCommand::ShowProperties);
        let panel = w.collab_panel.as_ref().unwrap();
        assert!(panel.title.contains("Properties"));
        assert!(
            panel
                .lines
                .iter()
                .any(|line| line.contains("Collaborative record"))
        );
        assert!(
            panel
                .lines
                .iter()
                .any(|line| line.contains("1 active grant"))
        );
        assert!(panel.lines.iter().any(|line| line.contains("1 version")));
        assert!(panel.lines.iter().any(|line| line.contains("1920 × 1080")));
        assert!(panel.lines.iter().any(|line| line.contains("1:05")));
        assert!(panel.lines.iter().any(|line| line.contains("Pages: 4")));
        w.apply_command(FileCommand::ShowSharing);
        let panel = w.collab_panel.as_ref().unwrap();
        assert!(panel.title.contains("Sharing"));
        assert!(panel.lines[0].contains("Anyone with the link"));
        w.apply_command(FileCommand::ShowHistory);
        let panel = w.collab_panel.as_ref().unwrap();
        assert!(panel.title.contains("Version History"));
        assert!(panel.lines[0].contains("Created"));
        assert!(
            panel.lines[0].contains("by"),
            "file history attributes the human before the secondary device"
        );
        w.apply_command(FileCommand::CloseCollabPanel);
        assert!(w.collab_panel.is_none());
    }

    // This scenario proves that governed presentation labels remain separate
    // from root-key command identity across the complete share-menu flow.
    #[allow(clippy::too_many_lines)]
    #[test]
    fn named_recipient_menu_uses_identity_label_but_commands_the_root_key() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.txt");
        std::fs::write(&file, b"report").unwrap();
        xattr::set(
            dir.path(),
            crate::model::SHARE_CONTROL_XATTR,
            b"/run/qos/app/qos-fs-share.sock",
        )
        .unwrap();
        let root_key = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
        let person_bytes = root_key.verifying_key().to_bytes();
        let person = collab_types::Ed25519PublicKey::from_bytes(person_bytes);
        let device_key = ed25519_dalek::SigningKey::from_bytes(&[82; 32]);
        let device =
            collab_types::Ed25519PublicKey::from_bytes(device_key.verifying_key().to_bytes());
        let mut certificate = qos_user_identity::DeviceCert {
            user_public_key: person_bytes,
            device_public_key: device_key.verifying_key().to_bytes(),
            device_label: "Ada's laptop".into(),
            capabilities: qos_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        qos_user_identity::sign_device_cert(&mut certificate, &root_key).unwrap();
        let mut w = FileExplorerWidget::new();
        w.workspace_roots = vec![dir.path().to_path_buf()];
        w.trusted_humans = crate::identity_directory::TrustedHumanDirectory::from_projection_v2(
            qos_user_identity::TrustedIdentityProjectionV2 {
                schema_version: qos_user_identity::TRUSTED_IDENTITY_PROJECTION_VERSION,
                humans: vec![qos_user_identity::GovernedHumanIdentityV2 {
                    root_public_key: person_bytes,
                    display_name: "Ada Lovelace".to_string(),
                    devices: vec![qos_user_identity::GovernedDeviceIdentityV2 { certificate }],
                }],
            },
        )
        .unwrap();
        let mut entry = ent("report.txt", false, "text/plain");
        entry.path = file;
        entry.record_id = Some(*uuid::Uuid::from_u128(82).as_bytes());
        w.entries = vec![entry];
        w.selection.select_only(0);

        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Share with Ada Lovelace"
                && *command == Some(FileCommand::ShareWithPerson { person })
        }));
        assert!(spec.iter().any(|(label, command)| {
            label.contains("Ada Lovelace")
                && label.contains("Edit")
                && label.contains("no expiry")
                && *command
                    == Some(FileCommand::CreateShare(
                        crate::sharing::ShareComposerRequest {
                            subject: crate::sharing::ShareSubjectChoice::Named(person),
                            permission: collab_types::FilePermission::Edit,
                            expiry: crate::sharing::ShareExpiryChoice::Never,
                            link_uses: None,
                            recipient_devices: vec![device],
                        },
                    ))
        }));
        assert!(spec.iter().any(|(label, command)| {
            label.contains("Copy link")
                && label.contains("Edit")
                && label.contains("30 days")
                && label.contains("10 uses")
                && *command
                    == Some(FileCommand::CreateShare(
                        crate::sharing::ShareComposerRequest {
                            subject: crate::sharing::ShareSubjectChoice::Link,
                            permission: collab_types::FilePermission::Edit,
                            expiry: crate::sharing::ShareExpiryChoice::Month,
                            link_uses: Some(crate::sharing::LinkUseChoice::Bounded(10)),
                            recipient_devices: Vec::new(),
                        },
                    ))
        }));

        w.entries[0].share = Some(crate::model::ShareProjection {
            schema_version: 1,
            record_id: uuid::Uuid::from_u128(82).to_string(),
            observed_at_ms: 1,
            grants: vec![crate::model::ShareGrantProjection {
                grant_id: uuid::Uuid::from_u128(83).to_string(),
                subject: crate::model::ShareSubject::Person {
                    person: *person.as_bytes(),
                },
                permission: crate::model::FilePermission::Edit,
                expires_at_ms: None,
                revoked: false,
                expired: false,
                used: false,
                exhausted: false,
                status: crate::model::ShareGrantStatus::Active,
                redemptions: 0,
                max_redemptions: None,
            }],
        });
        w.apply_command(FileCommand::ShowSharing);
        let lines = &w.collab_panel.as_ref().unwrap().lines;
        assert!(lines.iter().any(|line| line.contains("Ada's laptop")));
        assert!(lines.iter().any(|line| line.contains("manages workspaces")));
        assert!(
            lines.iter().any(|line| line.contains("key ")),
            "the on-demand Sharing panel exposes certificate identity, not labels alone"
        );
    }

    #[test]
    fn running_files_reloads_new_renamed_removed_and_expired_governed_devices() {
        fn projection(
            root: &ed25519_dalek::SigningKey,
            name: &str,
            devices: &[(u8, &str, Option<u64>)],
        ) -> qos_user_identity::TrustedIdentityProjectionV2 {
            let devices = devices
                .iter()
                .map(|(seed, label, expires_at_ns)| {
                    let device = ed25519_dalek::SigningKey::from_bytes(&[*seed; 32]);
                    let mut certificate = qos_user_identity::DeviceCert {
                        user_public_key: root.verifying_key().to_bytes(),
                        device_public_key: device.verifying_key().to_bytes(),
                        device_label: (*label).into(),
                        capabilities: qos_user_identity::DeviceCapabilities::member(),
                        issued_at_ns: 1,
                        expires_at_ns: *expires_at_ns,
                        signature: [0; 64],
                    };
                    qos_user_identity::sign_device_cert(&mut certificate, root).unwrap();
                    qos_user_identity::GovernedDeviceIdentityV2 { certificate }
                })
                .collect();
            qos_user_identity::TrustedIdentityProjectionV2 {
                schema_version: qos_user_identity::TRUSTED_IDENTITY_PROJECTION_VERSION,
                humans: vec![qos_user_identity::GovernedHumanIdentityV2 {
                    root_public_key: root.verifying_key().to_bytes(),
                    display_name: name.into(),
                    devices,
                }],
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted-identities.json");
        let root = ed25519_dalek::SigningKey::from_bytes(&[93; 32]);
        let person = collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes());
        let mut widget = FileExplorerWidget::new();

        std::fs::write(
            &path,
            serde_json::to_vec(&projection(&root, "Ada", &[(94, "Laptop", None)])).unwrap(),
        )
        .unwrap();
        assert!(widget.refresh_trusted_identities(Some(&path)));
        assert_eq!(
            widget.trusted_humans.human(&person).unwrap().devices.len(),
            1
        );

        std::fs::write(
            &path,
            serde_json::to_vec(&projection(
                &root,
                "Ada Renamed",
                &[(94, "Laptop", None), (95, "Phone", None)],
            ))
            .unwrap(),
        )
        .unwrap();
        assert!(widget.refresh_trusted_identities(Some(&path)));
        let human = widget.trusted_humans.human(&person).unwrap();
        assert_eq!(human.display_name, "Ada Renamed");
        assert_eq!(human.devices.len(), 2);

        std::fs::write(
            &path,
            serde_json::to_vec(&projection(&root, "Ada Renamed", &[(95, "Phone", Some(1))]))
                .unwrap(),
        )
        .unwrap();
        assert!(widget.refresh_trusted_identities(Some(&path)));
        assert!(
            widget
                .trusted_humans
                .human(&person)
                .unwrap()
                .devices
                .is_empty(),
            "removed and expired devices cannot remain usable after reload"
        );

        std::fs::write(
            &path,
            serde_json::to_vec(&qos_user_identity::TrustedIdentityProjectionV2::empty()).unwrap(),
        )
        .unwrap();
        assert!(widget.refresh_trusted_identities(Some(&path)));
        assert!(widget.trusted_humans.humans().is_empty());
    }

    #[test]
    fn collaborative_search_activation_resolves_the_latest_path_by_record_id() {
        use ui::widget::Widget;

        let dir = tempfile::tempdir().unwrap();
        let record_id = uuid::Uuid::from_u128(0xabc);
        let nested = dir.path().join("nested");
        let file = nested.join("renamed.txt");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(&file, b"hello").unwrap();
        xattr::set(&file, crate::model::RECORD_ID_XATTR, record_id.as_bytes()).unwrap();
        let projection = serde_json::json!({
            "schema_version": 1,
            "truncated": false,
            "records": [{
                "record_id": record_id,
                "path": "/nested/renamed.txt",
                "is_dir": false
            }]
        });
        xattr::set(
            dir.path(),
            crate::model::RECORD_INDEX_XATTR,
            &serde_json::to_vec(&projection).unwrap(),
        )
        .unwrap();
        let mut widget = FileExplorerWidget::new();
        widget.current_path = dir.path().to_path_buf();
        widget.entries.clear();
        widget.workspace_roots = vec![dir.path().to_path_buf()];

        let providers = widget.search_providers();
        assert_eq!(providers.len(), 2, "Places remains a separate provider");
        let hit = providers[1]
            .search("renamed")
            .pop()
            .expect("collaborative record is searchable");
        let mut doc = automerge::AutoCommit::new();
        widget.activate_search_result(&hit.action_id, &mut doc, &automerge::ROOT);
        assert_eq!(widget.current_path, nested);
        assert_eq!(widget.selection.primary(), Some(0));
        assert_eq!(widget.entries[0].path, file);
    }

    #[test]
    fn share_composer_authority_round_trip_never_blocks_the_ui_command() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.txt");
        let socket = dir.path().join("share.sock");
        std::fs::write(&file, b"report").unwrap();
        xattr::set(
            dir.path(),
            crate::model::SHARE_CONTROL_XATTR,
            socket.as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut length = [0_u8; 4];
            stream.read_exact(&mut length).unwrap();
            let mut request = vec![0; u32::from_be_bytes(length) as usize];
            stream.read_exact(&mut request).unwrap();
            seen_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            let response = collab_types::ShareControlResponseV1 {
                protocol_version: collab_types::SHARE_CONTROL_VERSION,
                result: collab_types::ShareControlResultV1::BearerCreatedV2 {
                    uri: "qos-file://bounded".into(),
                },
            };
            let response = collab_types::canonical_bytes(&response).unwrap();
            stream
                .write_all(&(response.len() as u32).to_be_bytes())
                .unwrap();
            stream.write_all(&response).unwrap();
        });

        let mut widget = FileExplorerWidget::new();
        widget.workspace_roots = vec![dir.path().to_path_buf()];
        let mut entry = ent("report.txt", false, "text/plain");
        entry.path = file;
        entry.record_id = Some(*uuid::Uuid::from_u128(0xdef).as_bytes());
        widget.entries = vec![entry];
        widget.selection.select_only(0);
        assert!(widget.apply_command(FileCommand::CreateShare(
            crate::sharing::ShareComposerRequest {
                subject: crate::sharing::ShareSubjectChoice::Link,
                permission: collab_types::FilePermission::Edit,
                expiry: crate::sharing::ShareExpiryChoice::Month,
                link_uses: Some(crate::sharing::LinkUseChoice::Bounded(10)),
                recipient_devices: Vec::new(),
            },
        )));
        // The daemon has the request but deliberately cannot reply yet. Reaching
        // this receive proves the UI command returned independently of I/O.
        seen_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(widget.share_pending_path.is_some());
        release_tx.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn named_share_status_distinguishes_pending_from_delivered() {
        let pending = t_args(
            named_delivery_status_key(collab_types::NamedShareDeliveryStateV1::Pending),
            &[arg("person", "Ada")],
        );
        let delivered = t_args(
            named_delivery_status_key(collab_types::NamedShareDeliveryStateV1::Delivered),
            &[arg("person", "Ada")],
        );
        assert!(pending.contains("pending"));
        assert!(delivered.contains("completed"));
        assert_ne!(pending, delivered);
    }

    #[test]
    fn previous_immutable_version_is_an_explicit_restore_command() {
        let previous = "a0d4e23f-c7cb-4ded-a427-b32cf9da9342";
        let current = "a0d4e23f-c7cb-4ded-a427-b32cf9da9344";
        let mut entry = ent("history.txt", false, "text/plain");
        entry.history = crate::model::HistoryProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "total_versions": 2,
                "versions_truncated": false,
                "heads": [current],
                "versions": [{
                    "id": previous,
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [],
                    "actor": {"person": vec![1; 32], "device": vec![2; 32]},
                    "timestamp_ms": 9,
                    "content_hash": vec![3; 32],
                    "metadata_hash": vec![4; 32],
                    "provenance": "Create"
                }, {
                    "id": current,
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [previous],
                    "actor": {"person": vec![1; 32], "device": vec![2; 32]},
                    "timestamp_ms": 10,
                    "content_hash": vec![5; 32],
                    "metadata_hash": vec![6; 32],
                    "provenance": "Edit"
                }],
                "conflicts_truncated": false,
                "conflicts": []
            }))
            .unwrap(),
        );
        let mut widget = FileExplorerWidget::new();
        widget.entries = vec![entry];
        widget.selection.select_only(0);
        let expected = collab_types::FileVersionId(uuid::Uuid::parse_str(previous).unwrap());
        assert!(
            widget
                .context_menu_spec(&[])
                .iter()
                .any(|(label, command)| {
                    label == "Restore Previous Version"
                        && *command
                            == Some(FileCommand::RestoreVersion {
                                version_id: expected,
                            })
                })
        );
    }

    #[test]
    fn open_text_conflict_menu_offers_exact_safe_actions_including_merge() {
        let current = "a0d4e23f-c7cb-4ded-a427-b32cf9da9344";
        let other = "a0d4e23f-c7cb-4ded-a427-b32cf9da9342";
        let conflict = uuid::Uuid::parse_str("c0d4e23f-c7cb-4ded-a427-b32cf9da9343").unwrap();
        let mut entry = ent("conflicted.qdoc", false, "application/x-qdoc");
        entry.content_hash = Some([5; 32]);
        entry.history = crate::model::HistoryProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "total_versions": 2,
                "versions_truncated": false,
                "heads": [current, other],
                "versions": [{
                    "id": other,
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [],
                    "actor": {"person": vec![1; 32], "device": vec![2; 32]},
                    "timestamp_ms": 9,
                    "content_hash": vec![3; 32],
                    "metadata_hash": vec![4; 32],
                    "provenance": "Edit"
                }, {
                    "id": current,
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [other],
                    "actor": {"person": vec![6; 32], "device": vec![7; 32]},
                    "timestamp_ms": 10,
                    "content_hash": vec![5; 32],
                    "metadata_hash": vec![8; 32],
                    "provenance": "Edit"
                }],
                "conflicts_truncated": false,
                "conflicts": [{
                    "id": conflict,
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "versions": [other, current],
                    "status": "Open",
                    "resolution": null
                }]
            }))
            .unwrap(),
        );
        let mut widget = FileExplorerWidget::new();
        widget.entries = vec![entry];
        widget.selection.select_only(0);
        let menu = widget.context_menu_spec(&[]);
        let current_id = collab_types::FileVersionId(uuid::Uuid::parse_str(current).unwrap());
        let other_id = collab_types::FileVersionId(uuid::Uuid::parse_str(other).unwrap());
        assert!(menu.iter().any(|(_, command)| {
            *command
                == Some(FileCommand::ResolveConflict {
                    conflict_id: conflict,
                    strategy: collab_types::ConflictStrategy::KeepCurrent,
                    version_id: current_id,
                })
        }));
        assert!(menu.iter().any(|(_, command)| {
            *command
                == Some(FileCommand::ResolveConflict {
                    conflict_id: conflict,
                    strategy: collab_types::ConflictStrategy::UseOther,
                    version_id: other_id,
                })
        }));
        assert!(menu.iter().any(|(_, command)| {
            *command
                == Some(FileCommand::ResolveConflict {
                    conflict_id: conflict,
                    strategy: collab_types::ConflictStrategy::KeepBoth,
                    version_id: other_id,
                })
        }));
        assert!(menu.iter().any(|(_, command)| {
            *command
                == Some(FileCommand::ResolveConflict {
                    conflict_id: conflict,
                    strategy: collab_types::ConflictStrategy::Merge,
                    version_id: other_id,
                })
        }));
    }

    #[test]
    fn owner_context_menu_revokes_each_exact_active_grant() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("shared.txt");
        std::fs::write(&file, b"shared").unwrap();
        let socket = root.path().join("share-control.sock");
        xattr::set(
            root.path(),
            crate::model::SHARE_CONTROL_XATTR,
            socket.as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        let grant = uuid::Uuid::parse_str("c0d4e23f-c7cb-4ded-a427-b32cf9da9343").unwrap();
        let mut entry = ent("shared.txt", false, "text/plain");
        entry.path = file;
        entry.share = crate::model::ShareProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "observed_at_ms": 10,
                "grants": [{
                    "grant_id": grant.to_string(),
                    "subject": {"kind": "bearer_link"},
                    "permission": "View",
                    "expires_at_ms": null,
                    "revoked": false,
                    "expired": false,
                    "used": false,
                    "exhausted": false,
                    "status": "active",
                    "redemptions": 0,
                    "max_redemptions": 2
                }]
            }))
            .unwrap(),
        );
        let mut w = FileExplorerWidget::new();
        w.workspace_roots = vec![root.path().to_path_buf()];
        w.entries = vec![entry];
        w.selection.select_only(0);
        let spec = w.context_menu_spec(&[]);
        assert!(spec.iter().any(|(label, command)| {
            label == "Revoke shared link"
                && *command == Some(FileCommand::RevokeShare { grant_id: grant })
        }));
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod responsive_body_tests {
    //! The places list is a *navigation* sidebar over the file view, on the
    //! shared `NavShell`: a comfortably-wide window docks it beside the files;
    //! a narrow window turns it into an overlay drawer over them. The file
    //! list is the content at every width — it is never pushed off the window
    //! to make room for the list of places. Drives the real production path
    //! (`build_layout` → `apply_layout` at a size, then `handle_event`).
    use super::*;
    use std::collections::HashMap;
    use taffy::prelude::*;
    use ui::theme::Theme;

    struct H {
        w: FileExplorerWidget,
        taffy: TaffyTree<()>,
        node: NodeId,
        state: ui::State,
        tr: ui::text_render::TextRenderer,
    }

    impl H {
        fn new(dir: Option<&std::path::Path>, width: f32, height: f32) -> Self {
            let mut w = FileExplorerWidget::with_persistence(None, None);
            if let Some(d) = dir {
                w.navigate(d.to_path_buf());
            }
            Self::from_widget(w, width, height)
        }

        /// Lay out a pre-built widget (e.g. one with persisted favourites, which
        /// `new`'s home-less widget lacks).
        fn from_widget(w: FileExplorerWidget, width: f32, height: f32) -> Self {
            let mut h = H {
                w,
                taffy: TaffyTree::new(),
                node: NodeId::new(0),
                state: ui::State::new(),
                tr: ui::text_render::TextRenderer::new(),
            };
            h.lay_out(width, height);
            // Match the live host: a responsive child discovers its width
            // during the first layout and asks for one immediate structural
            // reflow when it crosses a breakpoint.
            if h.w.take_structure_dirty() {
                h.lay_out(width, height);
            }
            h
        }

        fn lay_out(&mut self, width: f32, height: f32) {
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            self.w.sync(&mut self.state.doc, &self.state.root);
            let node = self.w.build_layout(&mut taffy, &theme, &mut nm);
            let measure_map = nm.clone();
            let trm = &mut self.tr;
            taffy
                .compute_layout_with_measure(
                    node,
                    Size {
                        width: AvailableSpace::Definite(width),
                        height: AvailableSpace::Definite(height),
                    },
                    |k, a, id, _, _| {
                        if let Some(&p) = measure_map.get(&id) {
                            unsafe { (*p).measure(k, a, trm) }
                        } else {
                            Size::ZERO
                        }
                    },
                )
                .unwrap();
            self.w.apply_layout(&taffy, node, Point::ORIGIN);
            self.taffy = taffy;
            self.node = node;
        }

        fn dispatch(&mut self, ev: &winit::event::WindowEvent) {
            let mut handled = false;
            let mut focus = None;
            self.w.handle_event(
                ev,
                1.0,
                &winit::event::Modifiers::default(),
                &mut self.state.doc,
                &self.state.root,
                &mut handled,
                &mut focus,
                self.node,
                &self.taffy,
            );
        }

        fn cursor(&mut self, x: f64, y: f64) {
            self.dispatch(&winit::event::WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(x, y),
            });
        }

        fn right_press(&mut self) {
            self.dispatch(&winit::event::WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Right,
            });
        }

        fn left_release(&mut self) {
            self.dispatch(&winit::event::WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
            });
        }

        fn left_press(&mut self) {
            self.dispatch(&winit::event::WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            });
        }

        /// The labels of the currently-open context/favourite menu, if any.
        fn open_menu_labels(&self) -> Vec<String> {
            if !self.w.context_menu.is_open() {
                return Vec::new();
            }
            self.w
                .context_menu
                .items()
                .iter()
                .filter_map(|i| i.label().map(str::to_string))
                .collect()
        }

        /// The laid-out rect of a named widget in the live tree. Asserting on
        /// the *file view's* own rect is what makes the narrow test about the
        /// files rather than about whichever pane happens to be full-width.
        fn rect_of(&self, name: &str) -> Option<vello::kurbo::Rect> {
            fn find(w: &dyn Widget, name: &str) -> Option<vello::kurbo::Rect> {
                if w.name() == name {
                    return Some(w.get_rect());
                }
                for c in w.children() {
                    if let Some(r) = find(c, name) {
                        return Some(r);
                    }
                }
                None
            }
            find(&self.w, name)
        }

        /// Run a real draw pass so immediate-mode children (the file view)
        /// publish their per-frame geometry — e.g. the in-place rename field's
        /// rect.
        fn draw(&mut self) {
            let theme = Theme::dark();
            let presence = ui::presence::PresenceCtx::empty();
            let permissions = ui::permissions::PermissionContext::permissive();
            let mut scene = vello::Scene::new();
            self.w.draw_with_presence(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut self.tr,
                &theme,
                &presence,
                &permissions,
            );
        }

        fn press_named(&mut self, key: winit::keyboard::NamedKey) {
            let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = winit::keyboard::Key::Named(key);
            ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = winit::event::ElementState::Pressed;
            ev.repeat = false;
            ev.text = None;
            self.dispatch(&winit::event::WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            });
        }

        fn type_char(&mut self, s: &str) {
            let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = winit::keyboard::Key::Character(winit::keyboard::SmolStr::new(s));
            ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = winit::event::ElementState::Pressed;
            ev.repeat = false;
            ev.text = Some(winit::keyboard::SmolStr::new(s));
            self.dispatch(&winit::event::WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            });
        }
    }

    #[test]
    fn body_docks_wide_and_reflows_to_push_when_narrowed() {
        let mut h = H::new(None, 1200.0, 720.0);
        assert_eq!(
            h.w.debug_body_mode(),
            ShellMode::Wide,
            "a wide window docks the places list beside the file view"
        );
        // Resize DOWN on the same instance (a real window resize re-runs
        // apply_layout) — the body must reflow to a single pane, not freeze.
        h.lay_out(520.0, 720.0);
        assert_eq!(
            h.w.debug_body_mode(),
            ShellMode::Narrow,
            "a narrow window collapses the body to a single pane"
        );
        // ...and back up again proves it isn't one-way.
        h.lay_out(1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Wide);
    }

    #[test]
    fn toolbar_reprioritizes_before_desktop_controls_can_overlap_on_phone_width() {
        let mut h = H::new(None, 390.0, 760.0);
        assert!(
            h.w.toolbar.is_compact(),
            "the first narrow frame keeps the path and navigation clear"
        );
        h.w.forward.push(PathBuf::from("/later"));
        h.lay_out(390.0, 760.0);
        h.w.open_view_menu();
        assert!(
            h.open_menu_labels().iter().any(|label| label == "Forward"),
            "compact overflow retains history navigation rather than removing it"
        );

        // The width is discovered during layout; one follow-up composition is
        // how the live app promotes wide controls after a real resize.
        h.lay_out(1200.0, 720.0);
        assert!(
            h.w.take_structure_dirty(),
            "crossing the breakpoint requests a structural toolbar rebuild"
        );
        h.lay_out(1200.0, 720.0);
        assert!(
            !h.w.toolbar.is_compact(),
            "wide windows restore direct view and sort controls"
        );
    }

    /// The regression: at ~600 px the window showed the breadcrumb and the
    /// FAVOURITES / SYSTEM list and **no files at all**, while the footer
    /// underneath read "27 items". The places list had been declared a content
    /// overview, so the narrow layout promoted it to the full-screen page and
    /// pushed the file view off the window.
    #[test]
    fn narrow_shows_the_files_not_the_places_list() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), b"x").unwrap();
        }
        let h = H::new(Some(dir.path()), 600.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Narrow);

        assert!(
            !h.w.debug_places_open(),
            "the places drawer starts closed, so the files are what you see",
        );
        // The file view's OWN rect, not "whichever pane is full-width": under
        // the original bug the places list was the full-width pane and the file
        // view was laid out off the right edge of the window.
        let files = h
            .rect_of("FileView")
            .expect("the file view must be in the tree at a narrow width");
        assert!(
            files.width() > 0.0 && files.height() > 0.0,
            "the file view must be laid out at a real size; got {files:?}",
        );
        assert!(
            files.x0 >= 0.0 && files.x1 <= 600.0,
            "and inside the window rather than pushed off it: {files:?}",
        );
        assert!(
            (h.w.debug_content_rect().width() - 600.0).abs() <= f64::EPSILON,
            "the content pane spans the whole narrow window",
        );
    }

    #[test]
    fn keyboard_context_menu_reaches_selection_actions_and_executes_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("report.txt"), b"draft").unwrap();
        let mut h = H::new(Some(dir.path()), 900.0, 720.0);

        // Arrow navigation selects the first row. The platform Menu key must
        // open the same selection-aware surface as right-click.
        h.press_named(winit::keyboard::NamedKey::ArrowDown);
        h.press_named(winit::keyboard::NamedKey::ContextMenu);
        assert!(h.w.context_menu.is_open());
        let labels = h.open_menu_labels();
        assert!(labels.iter().any(|label| label == "Open"));
        assert!(labels.iter().any(|label| label == "Rename"));

        // Menu keyboard handling skips separators and maps the selected row
        // back onto the exact FileCommand. Down selects Open, Down selects
        // Rename, and Enter executes it without any pointer event.
        h.press_named(winit::keyboard::NamedKey::ArrowDown);
        h.press_named(winit::keyboard::NamedKey::ArrowDown);
        h.press_named(winit::keyboard::NamedKey::Enter);
        assert!(!h.w.context_menu.is_open());
        assert!(h.w.renaming.is_some(), "keyboard menu executes Rename");
    }

    /// The drawer draws *over* the files rather than displacing them, and the
    /// files stay full-width behind it (HIG §1a).
    #[test]
    fn narrow_places_drawer_overlays_the_files_without_displacing_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut h = H::new(Some(dir.path()), 600.0, 720.0);
        let before = h.w.debug_content_rect();

        h.w.debug_open_places_drawer();
        h.lay_out(600.0, 720.0);
        h.w.debug_open_places_drawer();
        assert!(h.w.debug_places_open(), "the drawer is revealed");
        assert_eq!(
            h.w.debug_content_rect(),
            before,
            "the file view must not move or shrink when the drawer opens",
        );
    }

    /// Picking a destination in the narrow drawer dismisses it, leaving the
    /// user on the files they navigated to.
    #[test]
    fn narrow_navigation_dismisses_the_drawer_and_leaves_the_files_showing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut h = H::new(Some(dir.path()), 520.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Narrow);
        h.w.debug_open_places_drawer();
        assert!(h.w.debug_places_open());

        // A place-navigation (what tapping a place in the list emits). Inject
        // the command the places list pushes, then pump an event so
        // `handle_event` drains it and applies the dismissal wiring.
        crate::command::push(&h.w.cmds, FileCommand::Navigate(dir.path().join("sub")));
        h.cursor(10.0, 10.0);
        assert_eq!(
            h.w.current_path(),
            dir.path().join("sub"),
            "the navigation applied"
        );
        assert!(
            !h.w.debug_places_open(),
            "navigating dismisses the drawer so the files are what remains",
        );
        assert!(
            (h.w.debug_content_rect().width() - 520.0).abs() <= f64::EPSILON,
            "and the file view still owns the whole window",
        );
    }

    /// An inline rename is edited *in place* — a field placed over the row's
    /// name inside the file view — not in a bottom bar. The file view publishes
    /// the field rect during paint; it must sit within the file rows.
    #[test]
    fn rename_is_edited_in_place_over_the_row() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), b"x").unwrap();
        let mut h = H::new(Some(dir.path()), 1000.0, 720.0);
        let idx =
            h.w.displayed_entries()
                .iter()
                .position(|e| e.name == "old.txt")
                .unwrap();
        h.w.select(Some(idx));
        assert!(h.w.apply_command(FileCommand::Rename));
        assert!(h.w.is_renaming());

        // Rebuild with the rename active, then paint so the file view publishes
        // the editor's on-screen rect.
        h.lay_out(1000.0, 720.0);
        h.draw();

        let field =
            h.w.rename_rect
                .get()
                .expect("the in-place editor is placed over the row");
        let files = h.rect_of("FileView").expect("the file view is present");
        assert!(
            field.y0 >= files.y0 - 1.0 && field.y1 <= files.y1 + 1.0,
            "the rename field sits within the file rows (in place), not a bottom bar: \
             field={field:?} files={files:?}",
        );
        assert!(
            field.width() > 40.0,
            "the field spans the name column: {field:?}"
        );

        // Cancelling clears the published rect (nothing left drawn).
        h.w.cancel_rename();
        h.lay_out(1000.0, 720.0);
        h.draw();
        assert!(
            h.w.rename_rect.get().is_none(),
            "cancel removes the in-place editor"
        );
    }

    /// Typing into the in-place editor and pressing Enter renames the file on
    /// disk — driving the real keystroke path through the app into the shared
    /// `TextInput` and its CRDT-backed field. F2 opens the editor.
    #[test]
    fn typing_in_the_in_place_editor_renames_on_enter() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut h = H::new(Some(dir.path()), 1000.0, 720.0);
        let idx =
            h.w.displayed_entries()
                .iter()
                .position(|e| e.name == "a.txt")
                .unwrap();
        h.w.select(Some(idx));

        // F2 begins the rename; rebuild + paint places + seeds the editor.
        h.press_named(winit::keyboard::NamedKey::F2);
        assert!(h.w.is_renaming(), "F2 starts an inline rename");
        h.lay_out(1000.0, 720.0);
        h.draw();

        // The seed selected the whole name, so typing replaces it.
        for ch in ["b", ".", "m", "d"] {
            h.type_char(ch);
        }
        h.press_named(winit::keyboard::NamedKey::Enter);

        assert!(!h.w.is_renaming(), "Enter commits and closes the editor");
        assert!(
            dir.path().join("b.md").exists(),
            "the typed name renamed the file on disk"
        );
        assert!(!dir.path().join("a.txt").exists(), "the old name is gone");
    }

    /// Dragging a folder out of the list and dropping it on the docked sidebar
    /// pins it to Favourites; a release off the sidebar pins nothing.
    #[test]
    fn dropping_a_folder_on_the_sidebar_pins_it() {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        let mut h = H::from_widget(w, 1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Wide);
        let before = h.w.favorites().len();
        let folder = PathBuf::from("/home/tester/Projects/qos");

        // Begin the drag (what the file view emits once a folder press travels).
        assert!(h.w.apply_command(FileCommand::BeginDrag {
            path: folder.clone(),
            is_dir: true
        }));
        assert!(h.w.debug_pin_dragging(), "the drag is in flight");

        // Move over the sidebar, then release → pin.
        let sb = h.w.debug_sidebar_rect();
        h.cursor(sb.center().x, sb.center().y);
        h.left_release();
        assert!(!h.w.debug_pin_dragging(), "the drop ends the drag");
        assert_eq!(h.w.favorites().len(), before + 1, "the folder was pinned");
        assert!(
            h.w.favorites().iter().any(|f| f.path == folder),
            "and it is the dropped folder"
        );
    }

    #[test]
    fn releasing_a_pin_drag_off_the_sidebar_pins_nothing() {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        let mut h = H::from_widget(w, 1200.0, 720.0);
        let before = h.w.favorites().len();
        h.w.apply_command(FileCommand::BeginDrag {
            path: PathBuf::from("/home/tester/Downloads/tmp"),
            is_dir: true,
        });
        // Release over the content pane (not the sidebar).
        let content = h.w.debug_content_rect();
        h.cursor(content.center().x, content.center().y);
        h.left_release();
        assert!(!h.w.debug_pin_dragging());
        assert_eq!(
            h.w.favorites().len(),
            before,
            "a drop off the sidebar pins nothing"
        );
    }

    /// A press on a row, a frame (the click changed the selection, so the
    /// content pane — and the view that saw the press — is rebuilt), then
    /// a move past the threshold: the drag must start anyway. This is the
    /// real mouse sequence; state parked only in the view was lost between
    /// press and move, and no drag ever began from a fresh click.
    #[test]
    fn a_row_press_still_becomes_a_drag_after_the_view_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("photo.png"), b"png").unwrap();
        let mut h = H::new(Some(dir.path()), 1200.0, 720.0);
        let view = h.rect_of("FileView").expect("the file view is laid out");
        // First list row: below the header band.
        let row = (view.x0 + 60.0, view.y0 + 30.0 + 17.0);
        h.cursor(row.0, row.1);
        h.left_press();
        assert!(!h.w.debug_pin_dragging(), "a press alone is not a drag");
        // The frame after the click rebuilds the content pane.
        h.lay_out(1200.0, 720.0);
        h.cursor(row.0 + 40.0, row.1 + 20.0);
        assert!(
            h.w.debug_pin_dragging(),
            "the armed press must start the drag after the rebuild"
        );
        assert_eq!(
            crate::external_drag::unclaimed().map(|d| d.paths),
            Some(vec![dir.path().join("photo.png")]),
            "the host is told what is being carried"
        );
        h.left_release();
        assert!(!h.w.debug_pin_dragging());
        assert_eq!(crate::external_drag::unclaimed(), None);
    }

    /// A right-click on a sidebar favourite opens *its* menu (Open + Remove from
    /// Favourites), not the file-view context menu. The pinned Home entry offers
    /// no Remove. Drives the real right-press path end to end.
    #[test]
    fn right_clicking_a_favourite_opens_its_menu() {
        let cfg = tempfile::tempdir().unwrap();
        let w = FileExplorerWidget::with_persistence(
            Some(cfg.path().to_path_buf()),
            Some(PathBuf::from("/home/tester")),
        );
        let mut h = H::from_widget(w, 1200.0, 720.0);
        assert_eq!(
            h.w.debug_body_mode(),
            ShellMode::Wide,
            "wide docks the sidebar"
        );

        let band =
            h.w.favorites_store()
                .borrow()
                .band
                .expect("sidebar laid out");
        // Row 1 is a removable favourite (row 0 is the pinned Home).
        let (x, y) = (band.center_x(), band.row_center_y(1));
        h.cursor(x, y);
        h.right_press();
        let labels = h.open_menu_labels();
        assert!(
            labels.iter().any(|l| l == "Remove from Favourites"),
            "the favourite's own menu is shown: {labels:?}",
        );
        assert!(
            labels.iter().any(|l| l == "Open"),
            "with the Open folder action: {labels:?}"
        );
        // It is NOT the file-view context menu (which would offer New Folder).
        assert!(
            !labels.iter().any(|l| l == "New Folder"),
            "not the file menu: {labels:?}"
        );

        // The pinned Home favourite (row 0) has no Remove.
        let (hx, hy) = (band.center_x(), band.row_center_y(0));
        h.cursor(hx, hy);
        h.right_press();
        let home = h.open_menu_labels();
        assert!(
            !home.iter().any(|l| l == "Remove from Favourites"),
            "Home is pinned and cannot be removed: {home:?}",
        );
    }

    #[test]
    fn wide_docks_the_places_list_beside_the_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut h = H::new(Some(dir.path()), 1200.0, 720.0);
        assert_eq!(h.w.debug_body_mode(), ShellMode::Wide);
        assert!(h.w.debug_places_open(), "wide docks the places list open");

        let content = h.w.debug_content_rect();
        assert!(
            (content.x1 - 1200.0).abs() <= f64::EPSILON,
            "the file view runs to the window's right edge",
        );
        assert!(
            content.x0 > 0.0,
            "and starts after the docked places list: {content:?}",
        );

        crate::command::push(&h.w.cmds, FileCommand::Navigate(dir.path().join("sub")));
        h.cursor(10.0, 10.0);
        assert!(
            h.w.debug_places_open(),
            "wide navigation leaves the docked list in place — nothing to dismiss",
        );
    }
}

#[cfg(test)]
mod accessibility_tests {
    use super::*;
    use ui::a11y::{Politeness, Role};

    fn empty_widget() -> (tempfile::TempDir, FileExplorerWidget) {
        let home = tempfile::tempdir().unwrap();
        let mut widget =
            FileExplorerWidget::with_persistence(None, Some(home.path().to_path_buf()));
        widget.entries.clear();
        widget.error = None;
        widget.is_loading = false;
        (home, widget)
    }

    #[test]
    fn custom_empty_loading_and_error_states_are_live_semantics() {
        let (_home, mut widget) = empty_widget();
        let empty = Widget::accessibility(&widget).expect("Files has a live root");
        assert_eq!(empty.role, Role::Group);
        assert_eq!(empty.name.as_deref(), Some("Files activity"));
        assert_eq!(empty.live, Some(Politeness::Polite));
        assert_eq!(empty.value.as_deref(), Some("This folder is empty"));
        assert!(!empty.state.busy);

        widget.is_loading = true;
        let loading = Widget::accessibility(&widget).unwrap();
        assert!(loading.state.busy);
        assert!(loading.value.as_deref().unwrap().starts_with("Loading "));

        widget.is_loading = false;
        widget.error = Some("permission denied".into());
        let error = Widget::accessibility(&widget).unwrap();
        assert_eq!(error.live, Some(Politeness::Assertive));
        assert!(
            error
                .value
                .as_deref()
                .unwrap()
                .contains("permission denied")
        );
        assert!(error.value.as_deref().unwrap().contains("Try Again"));
    }

    #[test]
    fn collaboration_sheet_is_a_focused_modal_semantic_subtree() {
        let (_home, mut widget) = empty_widget();
        widget.show_collab_panel(CollabPanel {
            title: "Sharing — report.pdf".into(),
            summary: "Shared with 2 people".into(),
            lines: vec!["Alice can view".into(), "Bob can edit".into()],
            action: None,
        });

        let dialog = Widget::accessibility(&widget).expect("open sheet has semantics");
        assert_eq!(dialog.role, Role::Dialog);
        assert_eq!(dialog.name.as_deref(), Some("Sharing — report.pdf"));
        assert!(dialog.state.modal && dialog.state.focusable && dialog.state.focused);
        assert_eq!(dialog.children[0].role, Role::Heading { level: 2 });
        assert_eq!(dialog.children[1].role, Role::Paragraph);
        assert!(dialog.value.as_deref().unwrap().contains("Bob can edit"));
        assert!(Widget::traps_focus(&widget));
        assert!(
            Widget::children(&widget).is_empty(),
            "background toolbar/sidebar/list leave the semantic tree while modal"
        );

        widget.close_collab_panel();
        assert!(!Widget::traps_focus(&widget));
        assert!(!Widget::children(&widget).is_empty());
        assert_eq!(Widget::accessibility(&widget).unwrap().role, Role::Group);
    }

    #[test]
    fn destructive_eviction_requires_a_named_modal_action_before_writing_the_override() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("only-copy.txt");
        std::fs::write(&file, b"only local copy").unwrap();
        let (_home, mut widget) = empty_widget();
        widget.show_collab_panel(CollabPanel {
            title: t("files-destructive-eviction-title"),
            summary: t_args("files-destructive-eviction-summary", &[arg("count", 1_i64)]),
            lines: vec![t("files-destructive-eviction-loss")],
            action: Some(CollabPanelAction::DestructiveEviction(vec![file.clone()])),
        });

        let dialog = Widget::accessibility(&widget).expect("warning dialog has semantics");
        let action = dialog.children.last().expect("dialog action");
        assert_eq!(action.role, Role::Button);
        assert_eq!(
            action.name.as_deref(),
            Some(t("files-destructive-eviction-confirm").as_str())
        );
        assert_eq!(
            xattr::get(&file, crate::model::AVAILABILITY_CONTROL_XATTR).unwrap(),
            None,
            "opening the warning must not perform the destructive operation"
        );

        let command = widget
            .collab_panel_action_command()
            .expect("the explicit action is keyboard/click routable");
        widget.close_collab_panel();
        assert!(widget.apply_command(command));
        assert_eq!(
            xattr::get(&file, crate::model::AVAILABILITY_CONTROL_XATTR)
                .unwrap()
                .as_deref(),
            Some(b"evict-destructive".as_slice())
        );
    }
}

#[cfg(test)]
mod breadcrumb_home_tests {
    use super::*;

    /// The window opens on the user's real home, and the breadcrumb has
    /// to be measured against the same one.
    ///
    /// Under a sandbox this process's `HOME` is its own per-package
    /// directory (`/run/qos/app`) while the directory actually opened is
    /// the user's real home, bound by a grant and named by
    /// `QOS_USER_HOME`. `new` resolved the start directory from the
    /// second and the breadcrumb's home from the first, so
    /// [`crate::model::format_path`] was measuring against a directory
    /// nothing is under — and the trail rendered as an absolute path
    /// from the filesystem root instead of a `~`.
    #[test]
    fn breadcrumb_home_is_the_granted_user_home_not_the_process_home() {
        let granted = tempfile::tempdir().expect("tempdir");
        let granted_path = granted.path().to_path_buf();
        // Only `QOS_USER_HOME` is touched: `HOME` is left as the machine's,
        // which is precisely the disagreement under test — the override has
        // to win over whatever `dirs::home_dir()` says, and it does not say
        // this temp dir.
        // SAFETY: the value is read on the two lines below and by nothing
        // else in this crate's tests.
        unsafe { std::env::set_var("QOS_USER_HOME", &granted_path) };

        let resolved = crate::model::user_home();
        let widget_home = FileExplorerWidget::new()
            .home_path()
            .map(std::path::Path::to_path_buf);

        unsafe { std::env::remove_var("QOS_USER_HOME") };

        assert_eq!(
            resolved.as_deref(),
            Some(granted_path.as_path()),
            "the granted home wins over the process's own HOME",
        );
        assert_ne!(
            dirs::home_dir().as_deref(),
            Some(granted_path.as_path()),
            "this test only means something while the two disagree",
        );
        assert_eq!(
            widget_home.as_deref(),
            Some(granted_path.as_path()),
            "the widget measures its breadcrumb against the home it opens on",
        );

        // Which is what makes the trail read as somebody's home rather
        // than as a path from the filesystem root. This is the function
        // that rendered the bad one: handed a home the path is not
        // under, it falls through to the `/`-rooted branch and emits a
        // component per directory from the filesystem root.
        let deep = granted_path.join("Documents").join("Investors");
        let labels: Vec<String> =
            crate::breadcrumbs::breadcrumb_components(&deep, widget_home.as_deref(), &[])
                .into_iter()
                .map(|(label, _)| label)
                .collect();
        assert_eq!(
            labels,
            vec!["Home", "Documents", "Investors"],
            "the trail is rooted at Home, not at /",
        );

        assert_eq!(
            crate::model::format_path(&granted_path.join("Documents"), widget_home.as_deref()),
            "~/Documents",
        );
        assert_eq!(
            crate::model::format_path(&granted_path, widget_home.as_deref()),
            "~",
        );
    }
}
