//! Root widget for the notes binary. Owns the in-memory [`Workspace`],
//! the storage layer, and the two panes (the [`PageSidebar`] page tree +
//! [`MainPanel`]). Coordinates the data flow:
//!
//! - sidebar emits [`OutEvent`]s when the user clicks / renames /
//!   creates / deletes,
//! - the app applies them to the workspace,
//! - the workspace is then pushed back into the sidebar (so the new
//!   tree shape reflects), the main panel (title update), and the
//!   storage saver (debounced disk persistence).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId, transaction::Transactable};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::collaboration::PresencePayload;
use ui::invocation_scope::{ResourceEffectScope, ResourceInvocationScope};
use ui::permissions::PermissionContext;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::link_popover::PageEntry;
use ui::widget::{Button, NavShell, SidebarPlacement, TaffyTreeExt};

use crate::actions::{self, NotesAction};
use crate::columns::{Columns, HeaderRequests};
use crate::doc::NotesDoc;
use crate::main_panel::{BacklinkChip, MainPanel, TagChip};
use crate::model::{PageId, TagAssignment, TagId, Workspace};
use crate::shortcuts_overlay::ShortcutsOverlay;
use crate::sidebar::{OutEvent, PageSidebar, SidebarTransfer};
use crate::status_bar::{StatusBarState, StatusModel, count_words};
use crate::storage::{self, PageSaver, SaveStatus, Saver};

thread_local! {
    /// Whether the notebook's shell presents the window's History control.
    /// Always so in the app.
    static SHELL_PRESENTS_HISTORY: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Test seam: keep the shell out of the tree the window offers its History
/// control to, so the window draws its own History bar instead. A headless
/// component host opens History from that bar; it does not serve a click on
/// the shell's glyph.
#[doc(hidden)]
pub fn present_history_in_shell_for_test(present: bool) {
    SHELL_PRESENTS_HISTORY.set(present);
}

/// The resource the window's History control acts on.
const HISTORY_RESOURCE: &str = "tilo.host:history";

/// The test id of the sidebar header's "New note" button.
pub(crate) const NEW_NOTE_BUTTON_ID: &str = "new-note";
/// The test id of the note header's "Note actions" button.
pub(crate) const NOTE_MENU_BUTTON_ID: &str = "note-menu";

fn workspace_image_fetcher(root: &std::path::Path) -> ui::Fetcher {
    const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
    let base = root.join(storage::PAGES_DIR);
    let canonical_root = crate::files::canonical(root).unwrap_or_else(|| root.to_path_buf());
    Arc::new(move |source: &str| {
        if source.contains("://") || source.starts_with('/') {
            return None;
        }
        let source = source.split(['?', '#']).next().unwrap_or(source);
        let candidate = crate::files::canonical(&base.join(source))?;
        if !candidate.starts_with(&canonical_root) {
            return None;
        }
        let metadata = crate::files::metadata(&candidate).ok()?;
        if !crate::files::is_file(&metadata) || metadata.len > MAX_IMAGE_BYTES {
            return None;
        }
        crate::files::read(&candidate).ok()
    })
}

fn workspace_image_store(root: &std::path::Path) -> ui::ImageStore {
    let fetcher = workspace_image_fetcher(root);
    // Preserve document detail while allowing several large attachments to
    // remain resident together in the shared 64 MiB cache.
    ui::ImageStore::with_max_dimension(&fetcher, 2048)
}
fn canonical_image_fetcher(
    assets: Arc<std::sync::RwLock<HashMap<String, Vec<u8>>>>,
    legacy: Option<ui::Fetcher>,
) -> ui::Fetcher {
    Arc::new(move |source: &str| {
        let embedded = assets.read().ok()?.get(source).cloned();
        if embedded.is_some() || source.starts_with("asset:") {
            return embedded;
        }
        legacy.as_ref().and_then(|fetch| fetch(source))
    })
}

/// Draws notebook images from the canonical snapshot, falling back to a legacy
/// workspace's page files for relative (non-`asset:`) sources.
fn canonical_image_store(
    assets: &Arc<std::sync::RwLock<HashMap<String, Vec<u8>>>>,
    legacy_root: Option<&std::path::Path>,
) -> ui::ImageStore {
    let fetcher =
        canonical_image_fetcher(Arc::clone(assets), legacy_root.map(workspace_image_fetcher));
    ui::ImageStore::with_max_dimension(&fetcher, 2048)
}

/// Debounce window for the background save thread. Long enough that
/// a burst of keystrokes during inline rename coalesces, short enough
/// that the user's next "is this saved yet" sanity check doesn't have
/// to wait.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
/// How long a freshly-started instance waits before seeding its own
/// welcome page into the shared document. This gives an already-running
/// peer's full-state reply (the late-join handshake) time to arrive, so
/// a second instance adopts the first's workspace instead of creating a
/// duplicate. A lone instance simply seeds after the window elapses.
const SEED_GRACE: Duration = Duration::from_millis(1500);
/// Keeps one import plus its CRDT/action overhead below the collaboration
/// transport's 3 MiB mutation frame.
const MAX_MARKDOWN_IMPORT_BYTES: u64 = 1024 * 1024;
const MAX_MARKDOWN_IMPORT_BLOCKS: usize = 10_000;

enum MarkdownTransfer {
    Import,
    Export {
        markdown: Vec<u8>,
        asset_root: Option<PathBuf>,
        canonical_assets: HashMap<String, Vec<u8>>,
    },
    InsertImage {
        page: PageId,
    },
}

/// The original operation owns every continuation until its one terminal outcome.
struct PendingOperation {
    invocation: tilo_actions::InvocationId,
    scope: Option<ResourceInvocationScope>,
}
impl PendingOperation {
    fn local(invocation: tilo_actions::InvocationId) -> Self {
        Self {
            invocation,
            scope: None,
        }
    }
    fn scoped(scope: &ResourceInvocationScope) -> Self {
        Self {
            invocation: scope.invocation(),
            scope: Some(scope.clone()),
        }
    }
}
impl Drop for PendingOperation {
    fn drop(&mut self) {
        if let Some(scope) = &self.scope {
            scope.cancel();
        }
    }
}
struct ExternalLinkJob {
    receiver: std::sync::mpsc::Receiver<Result<(), String>>,
    operation: Option<PendingOperation>,
}
type ImportedAssets = Vec<(String, Vec<u8>)>;
type LoadedImage = (String, Option<Vec<u8>>);
/// Document heads, pending operations, selected page and tag filter.
type CanonicalProjectionKey = (Vec<ChangeHash>, usize, Option<PageId>, Option<TagId>);
struct PendingMarkdownImport {
    path: PathBuf,
    body: String,
    assets: Option<ImportedAssets>,
}
enum MarkdownJobResult {
    Imported {
        path: PathBuf,
        result: Result<(String, ImportedAssets), String>,
    },
    Exported {
        path: PathBuf,
        result: Result<(), String>,
    },
    Image {
        page: PageId,
        path: PathBuf,
        result: Result<LoadedImage, String>,
    },
}

fn file_effect<R>(
    scope: Option<&ResourceEffectScope>,
    effect: impl FnOnce() -> R,
) -> Result<R, String> {
    match scope {
        Some(scope) => scope.with_effect(|| Ok(effect())),
        None => Ok(effect()),
    }
}

/// Start a host file choice, retaining the admitting invocation's custody
/// through the chooser when the request came from a scoped resource operation.
fn send_markdown_pick(
    request: ui::portal::PickRequest,
    scope: Option<ResourceEffectScope>,
) -> ui::portal::PortalChannel {
    match scope {
        Some(scope) => request.send_scoped(scope),
        None => request.send(),
    }
}

fn standalone_image_parts(line: &str) -> Option<(&str, &str, &str)> {
    let trimmed = line.trim();
    let body = trimmed.strip_prefix("![")?;
    let (alt, destination) = body.split_once("](")?;
    let destination = destination.strip_suffix(')')?.trim();
    (!destination.is_empty()).then_some((
        line.get(..line.len().saturating_sub(line.trim_start().len()))
            .unwrap_or(""),
        alt,
        destination,
    ))
}

fn adopt_portable_markdown_assets(
    root: &std::path::Path,
    markdown_path: &std::path::Path,
    markdown: &str,
    scope: Option<&ResourceEffectScope>,
) -> Result<String, String> {
    let Some(parent) = markdown_path.parent() else {
        return Ok(markdown.to_string());
    };
    let canonical_parent = file_effect(scope, || crate::files::canonical(parent))?
        .unwrap_or_else(|| parent.to_path_buf());
    let mut output = String::with_capacity(markdown.len());
    for segment in markdown.split_inclusive('\n') {
        let (line, newline) = segment
            .strip_suffix('\n')
            .map_or((segment, ""), |line| (line, "\n"));
        let Some((indent, alt, destination)) = standalone_image_parts(line) else {
            output.push_str(segment);
            continue;
        };
        if destination.contains("://") || std::path::Path::new(destination).is_absolute() {
            output.push_str(segment);
            continue;
        }
        let Some(candidate) =
            file_effect(scope, || crate::files::canonical(&parent.join(destination)))?
                .filter(|candidate| candidate.starts_with(&canonical_parent))
        else {
            output.push_str(segment);
            continue;
        };
        // Preserve best-effort asset errors, but never swallow a retired scope.
        if let Some(scope) = scope {
            scope.with_effect(|| Ok(()))?;
        }
        let managed = import_image_asset_scoped(root, &candidate, scope);
        if let Some(scope) = scope {
            scope.with_effect(|| Ok(()))?;
        }
        let Ok(managed) = managed else {
            output.push_str(segment);
            continue;
        };
        let _ = write!(output, "{indent}![{alt}]({managed}){newline}");
    }
    Ok(output)
}

fn adopt_canonical_markdown_assets(
    markdown_path: &std::path::Path,
    markdown: &str,
    scope: Option<&ResourceEffectScope>,
) -> Result<(String, ImportedAssets), String> {
    let Some(parent) = markdown_path.parent() else {
        return Ok((markdown.to_string(), Vec::new()));
    };
    let canonical_parent = file_effect(scope, || crate::files::canonical(parent))?
        .unwrap_or_else(|| parent.to_path_buf());
    let mut output = String::with_capacity(markdown.len());
    let mut assets = HashMap::<String, Vec<u8>>::new();
    let mut imported_paths = HashMap::<PathBuf, String>::new();
    let mut asset_bytes = 0_usize;
    for segment in markdown.split_inclusive('\n') {
        let (line, newline) = segment
            .strip_suffix('\n')
            .map_or((segment, ""), |line| (line, "\n"));
        let Some((indent, alt, destination)) = standalone_image_parts(line) else {
            output.push_str(segment);
            continue;
        };
        if destination.contains("://") || std::path::Path::new(destination).is_absolute() {
            output.push_str(segment);
            continue;
        }
        let Some(candidate) =
            file_effect(scope, || crate::files::canonical(&parent.join(destination)))?
                .filter(|candidate| candidate.starts_with(&canonical_parent))
        else {
            output.push_str(segment);
            continue;
        };
        if let Some(source) = imported_paths.get(&candidate) {
            let _ = write!(output, "{indent}![{alt}]({source}){newline}");
            continue;
        }
        let Ok((bytes, _)) = file_effect(scope, || read_image_asset(&candidate))? else {
            output.push_str(segment);
            continue;
        };
        let digest = blake3::hash(&bytes).to_hex().to_string();
        let source = format!("asset:{digest}");
        if !assets.contains_key(&source) {
            let Some(next_total) = asset_bytes.checked_add(bytes.len()) else {
                output.push_str(segment);
                continue;
            };
            if next_total > crate::notebook::MAX_ASSET_BYTES {
                output.push_str(segment);
                continue;
            }
            asset_bytes = next_total;
            assets.insert(source.clone(), bytes);
        }
        imported_paths.insert(candidate, source.clone());
        let _ = write!(output, "{indent}![{alt}]({source}){newline}");
    }
    Ok((output, assets.into_iter().collect()))
}

fn available_export_asset_name(
    folder: &std::path::Path,
    name: &str,
    bytes: &[u8],
    scope: Option<&ResourceEffectScope>,
) -> Result<String, String> {
    let candidate = folder.join(name);
    if !file_effect(scope, || candidate.exists())?
        || file_effect(scope, || crate::files::read(&candidate))?
            .is_ok_and(|existing| existing == bytes)
    {
        return Ok(name.to_string());
    }
    let path = std::path::Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("image");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("png");
    for suffix in 2_u32..=u32::MAX {
        let name = format!("{stem}-{suffix}.{extension}");
        if !file_effect(scope, || folder.join(&name).exists())? {
            return Ok(name);
        }
    }
    Ok(format!("{}.{extension}", uuid::Uuid::new_v4()))
}

fn managed_asset_file_name(source: &str) -> Option<&str> {
    let mut components = std::path::Path::new(source).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(name)), None) if name.to_str() == Some(source) => {
            Some(source)
        }
        _ => None,
    }
}

#[cfg(test)]
fn write_portable_markdown_export(
    root: Option<&std::path::Path>,
    canonical_assets: &HashMap<String, Vec<u8>>,
    destination: &std::path::Path,
    markdown: &[u8],
) -> Result<(), String> {
    write_portable_markdown_export_scoped(root, canonical_assets, destination, markdown, None)
}

fn write_portable_markdown_export_scoped(
    root: Option<&std::path::Path>,
    canonical_assets: &HashMap<String, Vec<u8>>,
    destination: &std::path::Path,
    markdown: &[u8],
    scope: Option<&ResourceEffectScope>,
) -> Result<(), String> {
    if root.is_none() && canonical_assets.is_empty() {
        return file_effect(scope, || {
            tilo_files::native::atomic::write(destination, markdown)
        })?
        .map_err(|error| error.to_string());
    }
    let source = std::str::from_utf8(markdown).map_err(|_| "export is not UTF-8".to_string())?;
    let stem = destination
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("note")
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let folder_name = format!("{}-assets", stem.trim_matches('-'));
    let folder = destination.with_file_name(&folder_name);
    let mut copied = HashMap::<String, String>::new();
    let mut output = String::with_capacity(source.len());
    for segment in source.split_inclusive('\n') {
        let (line, newline) = segment
            .strip_suffix('\n')
            .map_or((segment, ""), |line| (line, "\n"));
        let Some((indent, alt, source)) = standalone_image_parts(line) else {
            output.push_str(segment);
            continue;
        };
        let (source_name, bytes) = if let Some(source_name) = source
            .strip_prefix("../assets/")
            .and_then(managed_asset_file_name)
        {
            let Some(root) = root else {
                output.push_str(segment);
                continue;
            };
            let bytes =
                file_effect(scope, || workspace_image_fetcher(root)(source))?.ok_or_else(|| {
                    format!("image {source_name} is unavailable or outside its storage bounds")
                })?;
            (source_name.to_owned(), bytes)
        } else if source.starts_with("asset:") {
            let Some(bytes) = canonical_assets.get(source).cloned() else {
                output.push_str(segment);
                continue;
            };
            let extension = if bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
                "png"
            } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
                "jpg"
            } else {
                "webp"
            };
            let digest = source.strip_prefix("asset:").unwrap_or("image");
            (format!("{digest}.{extension}"), bytes)
        } else {
            output.push_str(segment);
            continue;
        };
        let exported_name = if let Some(name) = copied.get(&source_name) {
            name.clone()
        } else {
            file_effect(scope, || crate::files::create_dir_all(&folder))?
                .map_err(|error| error.to_string())?;
            let name = available_export_asset_name(&folder, &source_name, &bytes, scope)?;
            file_effect(scope, || {
                tilo_files::native::atomic::write(&folder.join(&name), &bytes)
            })?
            .map_err(|error| error.to_string())?;
            copied.insert(source_name, name.clone());
            name
        };
        let _ = write!(
            output,
            "{indent}![{alt}]({folder_name}/{exported_name}){newline}"
        );
    }
    file_effect(scope, || {
        tilo_files::native::atomic::write(destination, output.as_bytes())
    })?
    .map_err(|error| error.to_string())
}

fn read_image_asset(source: &std::path::Path) -> Result<(Vec<u8>, &'static str), String> {
    const MAX_IMAGE_BYTES: u64 = crate::notebook::MAX_ASSET_BYTES as u64;
    let bytes = crate::files::read_bounded(source, MAX_IMAGE_BYTES)
        .map_err(|error| error.to_string())?
        .ok_or("Image is larger than 1 MiB")?;
    let extension = if bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
        "png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpg"
    } else if bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "webp"
    } else {
        return Err("Choose a PNG, JPEG, or WebP image".into());
    };
    Ok((bytes, extension))
}

#[cfg(test)]
fn import_image_asset(root: &std::path::Path, source: &std::path::Path) -> Result<String, String> {
    import_image_asset_scoped(root, source, None)
}
fn import_image_asset_scoped(
    root: &std::path::Path,
    source: &std::path::Path,
    scope: Option<&ResourceEffectScope>,
) -> Result<String, String> {
    let (bytes, extension) = file_effect(scope, || read_image_asset(source))??;
    let assets = root.join("assets");
    file_effect(scope, || crate::files::create_dir_all(&assets))?
        .map_err(|error| error.to_string())?;
    let name = format!("{}.{}", uuid::Uuid::new_v4(), extension);
    file_effect(scope, || {
        tilo_files::native::atomic::write(&assets.join(&name), &bytes)
    })?
    .map_err(|error| error.to_string())?;
    Ok(format!("../assets/{name}"))
}
fn load_image(
    root: Option<&std::path::Path>,
    path: &std::path::Path,
    scope: Option<&ResourceEffectScope>,
) -> Result<LoadedImage, String> {
    if let Some(root) = root {
        import_image_asset_scoped(root, path, scope).map(|destination| (destination, None))
    } else {
        file_effect(scope, || read_image_asset(path))?.map(|(bytes, _)| {
            let digest = blake3::hash(&bytes).to_hex().to_string();
            (format!("asset:{digest}"), Some(bytes))
        })
    }
}

fn read_bounded_markdown(path: &std::path::Path) -> Result<String, String> {
    let bytes = crate::files::read_bounded(path, MAX_MARKDOWN_IMPORT_BYTES)
        .map_err(|error| error.to_string())?
        .ok_or("Markdown file is larger than 1 MiB")?;
    let body = String::from_utf8(bytes).map_err(|_| "Markdown file is not UTF-8".to_string())?;
    if crate::markdown::parse(&body).len() > MAX_MARKDOWN_IMPORT_BLOCKS {
        return Err("Markdown file contains more than 10,000 blocks".into());
    }
    Ok(body)
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independently-toggled flag from a distinct concern (shots override, collab sync-state tracking, responsive layout mode, dirty tracking) with no shared sub-concept; grouping them would only relocate the count, not reduce it"
)]
pub struct NotesApp {
    workspace: Workspace,
    sidebar: PageSidebar,
    note_menu: ContextMenu,
    note_menu_target: Option<PageId>,
    note_menu_editor: bool,
    note_menu_table: Option<(String, usize, usize)>,
    note_menu_table_source: Option<String>,
    main: MainPanel,
    /// The note's save + page/word status, surfaced as a transient
    /// floating bubble (see `status_bubble`) instead of a permanent strip.
    status: StatusModel,
    /// Transient floating status bubble: poked on a status change (a save
    /// flip, a page switch, a word-count tick), and shown while the pointer
    /// hovers the bottom-right corner. Replaces the old bottom status bar so
    /// the editor spans to the window bottom.
    status_bubble: ui::StatusBubble,
    transfer_toasts: ui::widget::toast::ToastStack,
    last_toast_tick: ui::time::Instant,
    /// Shot-only override for the real save progress/failure presentation.
    /// Notes opens its workspace synchronously, so this does not alter live
    /// loading behavior or introduce a second production state source.
    shots_save_status: Option<SaveStatus>,
    shortcuts: ShortcutsOverlay,
    /// Root container rect, populated by `apply_layout`.
    rect: Rect,
    /// The window shell: the header row, the sidebar toggle, the regions and
    /// the drawer. The page tree and the note panel are laid out in the slots
    /// it places (see [`crate::columns`]).
    shell: NavShell,
    /// Where the shell put the two columns, written by its slots.
    columns: Columns,
    /// What the shell's header buttons asked for.
    header: Rc<HeaderRequests>,
    /// Cached taffy node IDs. Filled in `build_layout`, used by
    /// `apply_layout` and `handle_event`. The columns' nodes exist only while
    /// the shell shows them (a closed drawer shows no page tree).
    shell_node: Option<NodeId>,
    sidebar_node: Option<NodeId>,
    main_node: Option<NodeId>,
    /// The theme of the last layout, for what the app measures between frames.
    theme: Theme,
    /// Present only for the legacy standalone/fixture adapter until its host cutover.
    legacy_fixture_storage: Option<LegacyFixtureStorage>,
    /// Workspace-scoped image attachments; independent of notebook persistence.
    /// Canonical notebooks keep their document/history in the bound RecordStore.
    workspace_asset_root: Option<PathBuf>,
    /// Preserve a loaded draft when its first durable bind cannot read disk.
    /// Do not save or navigate away until that read can be retried safely.
    legacy_read_blocked: Option<PageId>,
    /// Last framework-owned durability projection; querying it never touches the record.
    record_save_status: ui::record::RecordSaveStatus,
    pub(crate) workspace_lock: Option<advisory_lock::FileLock>,
    /// A host-portal file choice in flight. File access never escapes the
    /// workspace sandbox without an explicit portal grant.
    markdown_transfer: Option<(MarkdownTransfer, ui::portal::PortalChannel)>,
    markdown_job: ui::LatestRequest<MarkdownJobResult>,
    markdown_invocation: Option<PendingOperation>,
    markdown_results: Vec<tilo_actions::InvocationUpdate>,
    external_link_opens: Vec<ExternalLinkJob>,
    canonical_assets: Arc<std::sync::RwLock<HashMap<String, Vec<u8>>>>,
    canonical_asset_manifest: Vec<(String, String)>,
    /// Some referenced image's bytes are still on their way from a
    /// collaborator's device; retried at most once a second.
    canonical_assets_waiting: Option<ui::time::Instant>,
    /// Whether this instance already offered to move inline images into the
    /// content store.
    inline_assets_offered: bool,
    /// Explicit local launch/picker input, held until the notebook has seeded/adopted.
    /// Never imported before then: its first page could replace the old tree.
    pending_markdown_source: Option<PendingMarkdownImport>,
    // Local pending-job identity, separate from authority and future page IDs.
    pending_import_generation: u64,
    import_control_emitted: bool,
    /// View/legacy callback intents that must wait for a mutable bound record admission.
    pending_events: Vec<OutEvent>,
    /// Last mouse position in logical coords. Used to hit-test
    /// link marks on mouse click without dragging the click event
    /// through every nested widget.
    last_mouse: Point,
    /// Start of the seed-grace window. Set to the time of the FIRST
    /// `sync` (when collaboration is live), not spawn — renderer init
    /// takes seconds, and measuring from spawn would let a late joiner's
    /// grace lapse before its first sync, making it seed its own welcome
    /// instead of adopting an existing peer's.
    started: ui::time::Instant,
    /// Whether the first `sync` has run (which (re)bases `started`).
    synced_once: bool,
    /// True once we've taken on shared document state — either by
    /// adopting a peer's or by seeding our own. Gates the one-time
    /// discard-local-welcome-on-adopt.
    adopted: bool,
    /// True once our page tree has been mirrored into the shared doc.
    seeded: bool,
    /// Pages deleted locally this session that still need to be removed from
    /// the shared doc on the next `sync` (`apply_event` has no CRDT access,
    /// and the sync pull-loop would otherwise resurrect them).
    pending_doc_removals: Vec<PageId>,
    /// The page the `MainPanel` editor is currently bound to in the doc.
    /// `sync` rebinds whenever this drifts from the selected page.
    bound_page: Option<PageId>,
    /// The tree has been laid out once, so the shell's columns are known.
    laid_out: bool,
    /// Forces a redraw when our own chrome changes without any child widget
    /// flagging itself dirty.
    self_dirty: bool,
    /// Inputs of the last canonical projection: document heads, pending
    /// operations, selected page and tag filter. `sync` re-projects the
    /// whole page tree only when one of them moved instead of every frame.
    canonical_synced: Option<CanonicalProjectionKey>,
    /// Framework-owned action dispatch. Hosted via
    /// [`crate::resource::ActionSink`]; standalone tests use the local host.
    actions: crate::resource::ActionSink,
    /// Page ids that have appeared in the shared document. Steady-state `sync`
    /// uses this to tell a never-yet-mirrored local page (push it up) from a page
    /// the document dropped — a Ctrl+Z undo of a create, or a remote delete —
    /// which must be removed locally too. Without it the unconditional local→doc
    /// mirror would resurrect an undone page every frame.
    known_in_doc: std::collections::HashSet<PageId>,
    /// Equivalent provenance tracking for the keyed tag map. It prevents an
    /// undone/deleted tag from being silently pushed back by a stale local
    /// snapshot on the next collaboration sync.
    known_tags_in_doc: std::collections::HashSet<TagId>,
    /// Local-only current tag browse/filter. It is deliberately not persisted
    /// or replicated: collaborators may inspect different tag slices.
    active_tag_filter: Option<TagId>,
    /// App-agnostic toolkit navigation history over Notes' local page route.
    /// The route itself remains presentation state; opening a note never
    /// becomes a collaborative document mutation.
    route_history: ui::NavStack<Option<PageId>>,
    /// A route selected by Back/Forward. The ordinary open path still performs
    /// stale-resource validation, while this marker prevents that replay from
    /// being pushed as a brand-new route and destroying the forward branch.
    #[allow(
        clippy::option_option,
        reason = "outer None means no history replay; inner None is the notebook overview route"
    )]
    history_target: Option<Option<PageId>>,
    /// A block reveal requested by a brokered view operation. Re-applied after
    /// the selected page binds to its collaborative block list, because binding
    /// rebuilds the editor and otherwise resets focus to block zero.
    pending_block_reveal: Option<(PageId, String)>,
    /// Search/resource snapshot keyed by each block's durable Automerge Text
    /// object id, refreshed from the collaborative document on every sync.
    search_blocks: HashMap<PageId, Vec<crate::actions::NoteBlockResource>>,
    /// Local index of canonical incoming `[[Page]]` marks. It is rebuilt only
    /// when the document heads advance, alongside the full-text catalogue;
    /// drawing a selected note therefore never scans a large notebook.
    backlink_index: HashMap<PageId, Vec<(PageId, u32)>>,
    /// Durable document heads used as the retained search catalogue revision.
    /// In the normal no-edit frame this avoids snapshotting every page/block
    /// just to rediscover the catalogue has not changed.
    search_heads: Option<Vec<ChangeHash>>,
    /// Retained complete catalogue; Ctrl+K receives a cheap handle instead of
    /// rebuilding/cloning every page and block at open time.
    search_provider: crate::search_provider::NoteSearchProvider,
    prepared_local_focus: Option<crate::local_view::PreparedFocus>,
    // The first measured width resolves the saved drawer and Sidebar intent once.
    pending_responsive_view: Option<(bool, bool)>,
}

/// Compatibility storage for the old standalone entry point and disk fixtures.
/// Canonical components never construct this adapter or start either worker.
struct LegacyFixtureStorage {
    root: PathBuf,
    saver: Saver,
    page_saver: PageSaver,
    // Declared last: both saver Drop implementations join before unlocking.
    _lock: advisory_lock::FileLock,
}

impl Drop for NotesApp {
    fn drop(&mut self) {
        // Retire queued effects before any widget fields or worker inboxes drop.
        if let Some(scope) = self
            .markdown_invocation
            .as_ref()
            .and_then(|job| job.scope.as_ref())
        {
            scope.cancel();
        }
        for job in &self.external_link_opens {
            if let Some(scope) = job.operation.as_ref().and_then(|job| job.scope.as_ref()) {
                scope.cancel();
            }
        }
    }
}

impl NotesApp {
    /// Load the workspace under `root` and wire up the debounced background
    /// saver. Returns a fully-initialised widget ready to be handed to
    /// `ui::App`.
    ///
    /// A brand-new workspace starts **empty** — no fabricated pages. The
    /// sidebar's "+" (and its empty-area "New note" menu) is the affordance to
    /// create the first page. Tests and headless captures that need a starting
    /// page use [`Self::spawn_with_starter_page`].
    ///
    /// # Errors
    ///
    /// Returns `Err` if the workspace directory can't be created, its saved
    /// state fails to load or decode, or the background saver threads fail
    /// to spawn.
    pub fn spawn(root: PathBuf) -> Result<Self, String> {
        Self::spawn_inner(root, false)
    }

    /// Like [`Self::spawn`] but seeds a "Welcome" starter page into an empty
    /// workspace. Used by tests and dev fixtures that need content to exercise;
    /// the live binary always uses [`Self::spawn`] / [`Self::spawn_default`],
    /// which start empty.
    ///
    /// # Errors
    ///
    /// Same failure modes as [`Self::spawn`].
    pub fn spawn_with_starter_page(root: PathBuf) -> Result<Self, String> {
        Self::spawn_inner(root, true)
    }

    fn spawn_inner(root: PathBuf, seed_starter: bool) -> Result<Self, String> {
        let lock = storage::lock_workspace(&root)?;
        if root.join(".notes-canonical-v1").exists() {
            return Err("this notebook has moved to canonical storage; open it with the current Notes launcher".into());
        }
        storage::ensure_layout(&root).map_err(|e| e.to_string())?;
        let mut workspace = storage::load_workspace(&root)?.unwrap_or_default();
        if seed_starter && workspace.pages().is_empty() {
            seed_welcome(&root, &mut workspace);
            storage::save_workspace(&root, &workspace).map_err(|e| e.to_string())?;
        }

        let mut sidebar = PageSidebar::new();
        sidebar.sync_from_workspace(&workspace);
        let mut main = MainPanel::new();
        main.set_image_store(Some(workspace_image_store(&root)));
        // Build a title→id map for any [[Title]] link marks parsed
        // out of the initial page body.
        let by_title: std::collections::HashMap<String, PageId> = workspace
            .pages()
            .iter()
            .map(|p| (p.title.clone(), p.id))
            .collect();
        if let Some(sel) = workspace.selected() {
            if let Some(meta) = workspace.get(sel) {
                let body = storage::read_page_body(&root, sel)
                    .map_err(|error| format!("Could not read note {}: {error}", meta.title))?;
                main.load_page_with_link_resolver(sel, &meta.title, &body, |t| {
                    by_title.get(t).copied()
                });
            }
        } else {
            let first = workspace
                .children_of(None)
                .first()
                .map(|p| (p.id, p.title.clone()));
            if let Some((id, title)) = first {
                workspace.set_selected(Some(id));
                let body = storage::read_page_body(&root, id)
                    .map_err(|error| format!("Could not read note {title}: {error}"))?;
                main.load_page_with_link_resolver(id, &title, &body, |t| by_title.get(t).copied());
                sidebar.sync_from_workspace(&workspace);
            }
        }

        let saver = Saver::spawn(root.clone(), SAVE_DEBOUNCE).map_err(|e| e.to_string())?;
        let page_saver =
            PageSaver::spawn(root.clone(), SAVE_DEBOUNCE).map_err(|e| e.to_string())?;
        Ok(Self::build_widget(
            workspace,
            sidebar,
            main,
            Some(LegacyFixtureStorage {
                root,
                saver,
                page_saver,
                _lock: lock,
            }),
        ))
    }

    /// Construct the real notebook presentation from an existing canonical document.
    /// This does not initialize schema, read Markdown, or own persistence workers.
    ///
    /// # Errors
    /// Rejects an invalid canonical notebook or a missing selected page's block list.
    pub fn from_document(
        doc: &AutoCommit,
        author: &ui::permissions::UserId,
    ) -> Result<Self, String> {
        let workspace = Self::project_workspace(doc, None)?;
        let mut sidebar = PageSidebar::new();
        sidebar.sync_from_workspace(&workspace);
        let mut main = MainPanel::new();
        main.set_local_author(Some(author.0.clone()));
        let snapshot = crate::notebook::asset_snapshot(doc)?;
        let waiting = !crate::notebook::assets_complete(doc, &snapshot)?;
        let canonical_assets = Arc::new(std::sync::RwLock::new(snapshot));
        main.set_image_store(Some(canonical_image_store(&canonical_assets, None)));
        let mut app = Self::build_widget(workspace, sidebar, main, None);
        app.canonical_assets = canonical_assets;
        app.canonical_assets_waiting = waiting.then(ui::time::now);
        app.canonical_asset_manifest = crate::notebook::asset_manifest(doc)?;
        app.main = app.main.project_canonical(doc, &app.workspace)?;
        app.bound_page = app.workspace.selected();
        app.refresh_link_pages();
        Ok(app)
    }

    /// Configure local attachments without creating a legacy document writer.
    pub(crate) fn configure_workspace_assets(
        &mut self,
        root: &std::path::Path,
    ) -> Result<(), String> {
        storage::ensure_layout(root).map_err(|error| error.to_string())?;
        self.main.set_image_store(Some(canonical_image_store(
            &self.canonical_assets,
            Some(root),
        )));
        self.workspace_asset_root = Some(root.to_path_buf());
        Ok(())
    }

    fn project_workspace(doc: &AutoCommit, selected: Option<PageId>) -> Result<Workspace, String> {
        let mut workspace = crate::notebook::project(doc, selected)?;
        if workspace.selected().is_none() {
            let first = workspace
                .pages()
                .iter()
                .filter(|page| page.parent.is_none())
                .min_by_key(|page| (page.order, page.id))
                .map(|page| page.id);
            workspace.set_selected(first);
        }
        Ok(workspace)
    }

    /// The window shell around the page tree and the note: "Notes" with its
    /// new-note button over the page tree, and the note's actions and search
    /// in the content header (the framework adds History). The columns
    /// themselves are the slots the app lays its page tree and note panel into.
    fn build_shell(columns: &Columns, header: &Rc<HeaderRequests>) -> NavShell {
        let new_note = {
            let header = Rc::clone(header);
            Button::icon_only(
                NEW_NOTE_BUTTON_ID,
                ui::icons::Icon::Plus,
                &ui::i18n!("notes-affordance-new-note"),
            )
            .on_click(move || header.new_note.set(true))
        };
        let note_menu = {
            let header = Rc::clone(header);
            Button::icon_only(
                NOTE_MENU_BUTTON_ID,
                ui::icons::Icon::MoreHorizontal,
                &ui::i18n!("notes-affordance-note-actions"),
            )
            .on_click(move || header.note_menu.set(true))
        };
        let actions: Vec<ui::widget::Element> = vec![
            Box::new(note_menu),
            Box::new(ui::search::search_button(
                ui::search::SearchRequest::this_app(),
            )),
        ];
        NavShell::new()
            .with_id("notes-shell")
            .sidebar(Box::new(columns.sidebar.slot()))
            .content(Box::new(columns.content.slot()))
            .sidebar_header(ui::i18n!("notes-title"), vec![Box::new(new_note)])
            .content_header("", actions)
    }

    fn build_widget(
        workspace: Workspace,
        sidebar: PageSidebar,
        main: MainPanel,
        legacy_fixture_storage: Option<LegacyFixtureStorage>,
    ) -> Self {
        let canonical = legacy_fixture_storage.is_none();
        let workspace_asset_root = legacy_fixture_storage
            .as_ref()
            .map(|legacy| legacy.root.clone());
        let search_provider = crate::search_provider::NoteSearchProvider::new(&workspace);
        let initial_route = workspace.selected();
        let columns = Columns::default();
        let header = Rc::new(HeaderRequests::default());
        let shell = Self::build_shell(&columns, &header);

        let mut app = Self {
            workspace,
            note_menu: ContextMenu::new(),
            note_menu_target: None,
            note_menu_editor: false,
            note_menu_table: None,
            note_menu_table_source: None,
            sidebar,
            main,
            status: StatusModel::new(),
            status_bubble: ui::StatusBubble::new(),
            transfer_toasts: ui::widget::toast::ToastStack::new().with_max(3),
            last_toast_tick: ui::time::now(),
            shots_save_status: None,
            shortcuts: ShortcutsOverlay::new(),
            rect: Rect::ZERO,
            shell,
            columns,
            header,
            shell_node: None,
            sidebar_node: None,
            main_node: None,
            theme: Theme::dark(),
            legacy_fixture_storage,
            workspace_asset_root,
            legacy_read_blocked: None,
            record_save_status: ui::record::RecordSaveStatus::MemoryOnly,
            workspace_lock: None,
            markdown_transfer: None,
            markdown_job: ui::LatestRequest::new(),
            markdown_invocation: None,
            markdown_results: Vec::new(),
            external_link_opens: Vec::new(),
            canonical_assets: Arc::new(std::sync::RwLock::new(HashMap::new())),
            canonical_asset_manifest: Vec::new(),
            canonical_assets_waiting: None,
            inline_assets_offered: false,
            pending_markdown_source: None,
            pending_import_generation: 0,
            import_control_emitted: false,
            pending_events: Vec::new(),
            last_mouse: Point::ZERO,
            started: ui::time::now(),
            synced_once: false,
            adopted: canonical,
            seeded: canonical,
            pending_doc_removals: Vec::new(),
            bound_page: None,
            laid_out: false,
            self_dirty: false,
            canonical_synced: None,
            actions: crate::resource::ActionSink::default(),
            known_in_doc: std::collections::HashSet::new(),
            known_tags_in_doc: std::collections::HashSet::new(),
            active_tag_filter: None,
            route_history: ui::NavStack::new(initial_route),
            history_target: None,
            pending_block_reveal: None,
            search_blocks: HashMap::new(),
            backlink_index: HashMap::new(),
            search_heads: None,
            search_provider,
            prepared_local_focus: None,
            pending_responsive_view: None,
        };
        app.refresh_link_pages();
        app.main.set_tags(app.selected_tag_chips());
        app.refresh_status();
        app
    }

    /// Recompute the status bar snapshot from the workspace + savers.
    /// Cheap — called from `handle_event`'s tail and after every
    /// `apply_event`. The status bar diffs internally so a no-op
    /// refresh doesn't dirty its render.
    fn refresh_status(&mut self) {
        let path = match self.workspace.selected() {
            Some(id) => self.breadcrumb_for(id),
            None => String::new(),
        };
        let parent_path = path
            .rsplit_once(" / ")
            .map_or_else(String::new, |(parents, _)| parents.to_string());
        self.main.set_parent_path(parent_path);
        // Count words from the live editor (not the on-disk body) so
        // the indicator stays in sync with what the user is typing.
        // We count over each block's plain text body — not the
        // serialised markdown — so chrome like `- `, `**`, `` ` ``
        // doesn't inflate the total.
        let word_count = if self.workspace.selected().is_some() {
            let mut total = 0usize;
            for b in self.main.editor().blocks() {
                total += count_words(b.text());
            }
            Some(total)
        } else {
            None
        };
        // Combined save status: worst-of(workspace, page bodies). If
        // either is in-flight the user sees "Saving…"; either failure
        // surfaces "Save failed".
        let save_status = self.shots_save_status.unwrap_or_else(|| {
            if self.legacy_read_blocked.is_some() {
                return SaveStatus::Failed;
            }
            if let Some(legacy) = &self.legacy_fixture_storage {
                combine_save_status(legacy.saver.status(), legacy.page_saver.status())
            } else {
                match self.record_save_status {
                    ui::record::RecordSaveStatus::Saved => SaveStatus::Saved,
                    ui::record::RecordSaveStatus::Saving => SaveStatus::Saving,
                    ui::record::RecordSaveStatus::Failed(_) => SaveStatus::Failed,
                    ui::record::RecordSaveStatus::MemoryOnly => SaveStatus::SessionOnly,
                }
            }
        });
        // A failure is not a passing remark: it stays under the title until
        // the note saves, and the bubble does not announce it a second time.
        self.main.set_save_failed(save_status == SaveStatus::Failed);
        let announce = save_status != SaveStatus::Failed
            && self.status.state().save_status != save_status
            || self.status.state().page_path != path
            || self.status.state().page_count != self.workspace.pages().len();
        self.status.set_state(StatusBarState {
            save_status,
            page_path: path,
            page_count: self.workspace.pages().len(),
            word_count,
        });
        // Counts stay live when revealed, but each new word must not restart
        // a decorative animation while the user is concentrating on writing.
        if announce {
            self.status_bubble.poke(ui::time::now());
        }
    }

    /// What the status bubble says. A phone keeps the transient edge
    /// affordance to the actionable save state; wider windows add the
    /// note's length.
    fn status_label(&self) -> String {
        if self.shell.geometry().sidebar == SidebarPlacement::Docked {
            self.status.bubble_text()
        } else {
            self.status.save_text().to_string()
        }
    }

    /// `--shots`: stage the status bubble as a change just happened with the
    /// pointer resting on it, so a still frame captures the floating chip.
    pub fn stage_status_bubble(&mut self) {
        let now = ui::time::now();
        let label = self.status_label();
        self.status_bubble.show(&label, now);
        self.status_bubble.set_hover(true, now);
    }

    /// `--shots`: capture an actual save-state presentation deterministically,
    /// without depending on a timing-sensitive filesystem failure.
    pub fn set_save_status_for_shots(&mut self, status: SaveStatus) {
        self.shots_save_status = Some(status);
        let mut state = self.status.state().clone();
        state.save_status = status;
        let _ = self.status.set_state(state);
        self.stage_status_bubble();
    }

    #[doc(hidden)]
    pub fn show_transfer_toast_for_shots(&mut self) {
        self.transfer_toasts.push(
            "Imported Project brief",
            ui::widget::toast::ToastKind::Success,
        );
    }

    /// Paint the transient floating status bubble at the bottom-right of the
    /// window. It shows on a status change or while the pointer rests on its
    /// corner, then fades; a failed save stays until it is resolved.
    fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.status.state().save_status == SaveStatus::Failed {
            return;
        }
        let now = ui::time::now();
        let label = self.status_label();
        self.status_bubble.show(&label, now);
        self.status_bubble
            .paint(scene, transform, tr, theme, self.status_area(theme), now);
    }

    /// The window less the note's scroll bar: where the status bubble sits, so
    /// it never lands on the bar.
    fn status_area(&self, theme: &Theme) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            (self.rect.x1 - f64::from(theme.tokens.metrics.scrollbar_gutter)).max(self.rect.x0),
            self.rect.y1,
        )
    }

    /// Resolve a page's breadcrumb path by walking parent pointers.
    /// Format: `Root / Mid / Leaf`. Caps recursion at 16 for safety.
    fn breadcrumb_for(&self, id: PageId) -> String {
        let mut chain: Vec<String> = Vec::new();
        let mut cur = Some(id);
        let mut hops = 0;
        while let Some(pid) = cur {
            if hops > 16 {
                break;
            }
            match self.workspace.get(pid) {
                Some(meta) => {
                    chain.push(meta.title.clone());
                    cur = meta.parent;
                }
                None => break,
            }
            hops += 1;
        }
        chain.reverse();
        chain.join(" / ")
    }

    /// Sync pages into the editor's `[[link]]` popover. Global search and
    /// actions consume the resource and search providers in `ui::App`.
    fn refresh_link_pages(&mut self) {
        let page_entries: Vec<PageEntry> = self
            .workspace
            .pages()
            .iter()
            .map(|p| PageEntry {
                id: p.id,
                title: p.title.clone(),
            })
            .collect();
        self.main.editor_mut().set_link_pages(page_entries);
    }

    /// Metadata projection for the selected note header. The page/tag ids stay
    /// attached to their labels so the header can invoke a stable canonical
    /// tag resource even after a collaborator renames a tag.
    fn selected_tag_chips(&self) -> Vec<TagChip> {
        self.workspace
            .selected()
            .map(|page| {
                self.workspace
                    .tags_for_page(page)
                    .into_iter()
                    .map(|tag| TagChip {
                        id: tag.id,
                        name: tag.name.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Present only the selected page's incoming-link row. The complete index
    /// is built from canonical native marks in `refresh_search_blocks`; this
    /// projection is local presentation, while the page references and open
    /// operation remain canonical resources/actions.
    fn selected_backlink_chips(&self) -> Vec<BacklinkChip> {
        let Some(page) = self.workspace.selected() else {
            return Vec::new();
        };
        let mut chips = self
            .backlink_index
            .get(&page)
            .into_iter()
            .flatten()
            .filter_map(|(source, references)| {
                self.workspace.get(*source).map(|meta| BacklinkChip {
                    page: *source,
                    title: meta.title.clone(),
                    references: *references,
                })
            })
            .collect::<Vec<_>>();
        chips.sort_by(|left, right| {
            left.title
                .to_lowercase()
                .cmp(&right.title.to_lowercase())
                .then(left.page.cmp(&right.page))
        });
        chips
    }

    fn refresh_transclusion_previews(&mut self, doc: &AutoCommit) {
        const EXCERPT_LIMIT: usize = 180;
        let previews = self
            .workspace
            .pages()
            .iter()
            .map(|page| {
                let canonical = NotesDoc::page_block_snapshots(doc, page.id)
                    .into_iter()
                    .map(|(_, text)| text)
                    .find(|text| !text.trim().is_empty());
                let legacy = || {
                    let root = &self.legacy_fixture_storage.as_ref()?.root;
                    let body = storage::read_page_body(root, page.id).ok()?;
                    crate::markdown::specs_to_blocks(&crate::markdown::parse(&body))
                        .into_iter()
                        .map(|block| block.text().to_string())
                        .find(|text| !text.trim().is_empty())
                };
                let raw = canonical
                    .or_else(legacy)
                    .unwrap_or_else(|| "Empty note".to_string());
                let mut excerpt = raw.chars().take(EXCERPT_LIMIT).collect::<String>();
                if raw.chars().nth(EXCERPT_LIMIT).is_some() {
                    excerpt.push('…');
                }
                (
                    page.id,
                    ui::widget::TransclusionPreview {
                        title: page.title.clone(),
                        excerpt,
                    },
                )
            })
            .collect();
        self.main.set_transclusion_previews(previews);
    }

    fn drain_transclusion_opens(&mut self, doc: &AutoCommit) {
        while let Some(page) = self.main.take_transclusion_open() {
            if let Err(error) = self.open_notes_operation(
                actions::NotesOperation::OpenPage {
                    page,
                    block_id: None,
                },
                doc,
            ) {
                tracing::warn!(%error, %page, "notes: embedded page target went stale");
            }
        }
        while let Some(url) = self.main.take_external_link_open() {
            if let Err(error) =
                self.open_notes_operation(actions::NotesOperation::OpenExternalLink { url }, doc)
            {
                tracing::warn!(%error, "notes: semantic external link refused");
            }
        }
    }

    /// Test accessor: the currently selected page id.
    #[doc(hidden)]
    pub fn selected_for_test(&self) -> Option<PageId> {
        self.workspace.selected()
    }

    /// Test accessor: handle to the shortcuts overlay.
    #[doc(hidden)]
    pub fn shortcuts_mut(&mut self) -> &mut ShortcutsOverlay {
        &mut self.shortcuts
    }

    /// Hit-test the cached last-mouse-position against every link
    /// span in every block. Returns the page id of the first link
    /// covering that pixel, or `None` if the click missed.
    fn link_under_last_mouse(&self) -> Option<ui::widget::block_editor::LinkTarget> {
        self.main.editor().link_at_point(self.last_mouse)
    }

    fn open_external_link(&mut self, url: String) {
        if let Err(error) = self.start_external_link(url, None) {
            self.transfer_toasts
                .push(error, ui::widget::toast::ToastKind::Error);
        }
    }
    fn start_external_link(
        &mut self,
        url: String,
        operation: Option<PendingOperation>,
    ) -> Result<(), String> {
        if !crate::markdown::safe_external_url(&url) {
            return Err("This link is not safe to open".into());
        }
        let receiver = if let Some(scope) = operation.as_ref().and_then(|job| job.scope.as_ref()) {
            ui::portal::open_uri_scoped(url, scope.effect_scope()?)
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            let failed = tx.clone();
            let spawned = ui::rt::spawn_thread("notes-open-link", move || {
                let result =
                    ui::widget::try_open_external_url(&url).map_err(|error| error.to_string());
                let _ = tx.send(result);
            });
            if let Err(error) = spawned {
                let _ = failed.send(Err(format!("could not open the link: {error}")));
            }
            rx
        };
        self.external_link_opens.push(ExternalLinkJob {
            receiver,
            operation,
        });
        self.transfer_toasts
            .push("Opening link…", ui::widget::toast::ToastKind::Info);
        Ok(())
    }
    fn drain_external_link_opens(&mut self) {
        for mut job in std::mem::take(&mut self.external_link_opens) {
            let expired = job
                .operation
                .as_ref()
                .and_then(|operation| operation.scope.as_ref())
                .and_then(|scope| scope.check().err());
            let result = if let Some(error) = expired {
                Some(Err(error))
            } else {
                match job.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Some(Err("Link portal disconnected".into()))
                    }
                }
            };
            let Some(result) = result else {
                self.external_link_opens.push(job);
                continue;
            };
            if let Err(error) = &result {
                tracing::warn!(%error, "notes: external link portal failed");
                self.transfer_toasts
                    .push("Could not open link", ui::widget::toast::ToastKind::Error);
            }
            if let Some(operation) = job.operation.take() {
                self.complete_operation(
                    operation,
                    result.map(|()| tilo_actions::ActionOutput::default()),
                );
            }
        }
    }

    #[cfg(test)]
    fn backlinks_for(
        &self,
        doc: &AutoCommit,
        target: PageId,
    ) -> Vec<actions::NoteBacklinkResource> {
        if self.legacy_fixture_storage.is_none() {
            return actions::canonical_backlinks(doc, self.workspace.pages(), target);
        }
        let title_ids = self
            .workspace
            .pages()
            .iter()
            .map(|page| (page.title.clone(), page.id))
            .collect::<HashMap<_, _>>();
        let mut backlinks = self
            .workspace
            .pages()
            .iter()
            .filter(|page| page.id != target)
            .filter_map(|page| {
                let legacy = self.legacy_fixture_storage.as_ref()?;
                let body = storage::read_page_body(&legacy.root, page.id).unwrap_or_default();
                let mut specs = crate::markdown::parse(&body);
                crate::markdown::resolve_link_ids(&mut specs, |title| {
                    title_ids.get(title).copied()
                });
                let block = crate::markdown::specs_to_blocks(&specs)
                    .into_iter()
                    .find(|block| {
                        block.line.marks().iter().any(|span| {
                            matches!(span.mark, ui::widget::Mark::Link(page) if page == target)
                        })
                    })?;
                Some(actions::NoteBacklinkResource {
                    target,
                    source: page.id,
                    block_id: String::new(),
                    source_title: page.title.clone(),
                    excerpt: block.text().to_string(),
                })
            })
            .collect::<Vec<_>>();
        backlinks.sort_by(|a, b| {
            a.source_title
                .to_lowercase()
                .cmp(&b.source_title.to_lowercase())
                .then(a.source.cmp(&b.source))
        });
        backlinks
    }

    /// Show or hide the page tree, without the slide: where the window is too
    /// narrow to dock it, it is a drawer the shell opens over the note.
    fn set_sidebar_open(&mut self, open: bool) {
        if self.shell.is_sidebar_open() != open {
            if open {
                self.shell.open_sidebar();
            } else {
                self.shell.close_sidebar();
            }
            self.shell.snap();
            self.self_dirty = true;
        }
    }

    /// Default constructor — resolves the workspace root via `tilo-paths`,
    /// falls back to `./notes` if the home dir is unavailable.
    ///
    /// # Errors
    ///
    /// Same failure modes as [`Self::spawn`].
    pub fn spawn_default() -> Result<Self, String> {
        let root = storage::default_root()?.unwrap_or_else(|| PathBuf::from("./notes"));
        Self::spawn(root)
    }

    /// Import an explicitly supplied local Markdown launch target as a new
    /// notebook page. Like the file-picker import, this never edits the source.
    ///
    /// # Errors
    /// Rejects a second pending target, non-local URLs, unreadable files, and
    /// input exceeding the same UTF-8/size/block limits as picker imports.
    pub fn queue_launch_markdown(&mut self, target: &str) -> Result<(), String> {
        if self.pending_markdown_source.is_some()
            || self.markdown_transfer.is_some()
            || self.markdown_invocation.is_some()
        {
            return Err("A file transfer is already pending".into());
        }
        let path = if target.to_ascii_lowercase().starts_with("file:") {
            let url =
                url::Url::parse(target).map_err(|error| format!("Invalid file URL: {error}"))?;
            ui::portal::to_local_path(&url).ok_or_else(|| {
                match tilo_files::availability(tilo_files::Feature::NativePaths) {
                    tilo_files::Availability::Unsupported(reason) => {
                        format!("Markdown launch does not support local file URLs here: {reason}")
                    }
                    _ => "Markdown launch requires a local file URL".to_string(),
                }
            })?
        } else if target.contains("://") {
            return Err("Markdown launch requires a local file".into());
        } else {
            PathBuf::from(target)
        };
        let body = read_bounded_markdown(&path)?;
        self.stage_pending_import(PendingMarkdownImport {
            path,
            body,
            assets: None,
        })?;
        self.import_control_emitted = false;
        self.self_dirty = true;
        Ok(())
    }

    /// Reconcile our in-memory page tree with the shared Automerge
    /// document. Seeds the doc from our welcome page when we're the
    /// origin, adopts a peer's tree when we join late, and keeps both in
    /// step thereafter. Runs every frame from [`sync`].
    fn sync_workspace_tree(&mut self, state: &mut AutoCommit) {
        // Base the seed-grace window on the first sync (collab is live by
        // now), not spawn — see `started`'s doc comment.
        if !self.synced_once {
            self.started = ui::time::now();
            self.synced_once = true;
        }
        // Apply local deletions through their canonical action BEFORE reading
        // the document, so the pull-loop below doesn't resurrect a just-
        // deleted page. `apply_event_for_test` uses this reconciliation path
        // too, so it must not write straight to Automerge.
        for id in std::mem::take(&mut self.pending_doc_removals) {
            self.perform_action(state, &actions::delete_page_action(id));
        }
        let doc_pages = NotesDoc::list_pages(state);
        if !doc_pages.is_empty() {
            if self.adopted {
                // Already sharing. The document is authoritative for the page
                // tree: every local tree mutation is dispatched as an action
                // that writes it (see `dispatch_tree_diff`), and remote peers'
                // mutations arrive by merge. Reconcile the workspace to match —
                // pull new pages, fold changed metadata back in, and drop pages
                // the document no longer holds (a Ctrl+Z undo of a create, or a
                // remote delete). A page the document has never held yet (a
                // freshly seeded welcome, or a test-created page) is pushed up.
                let doc_ids: std::collections::HashSet<PageId> =
                    doc_pages.iter().map(|p| p.id).collect();
                for dp in &doc_pages {
                    self.known_in_doc.insert(dp.id);
                }
                let mut changed = false;
                for dp in &doc_pages {
                    if let Some(existing) = self.workspace.get_mut(dp.id) {
                        if existing != dp {
                            *existing = dp.clone();
                            changed = true;
                        }
                    } else {
                        self.workspace.data.pages.push(dp.clone());
                        changed = true;
                    }
                }
                // Drop pages the document dropped. Only pages it once held — a
                // never-mirrored local page stays and is pushed up below.
                let to_drop: Vec<PageId> = self
                    .workspace
                    .pages()
                    .iter()
                    .filter(|p| !doc_ids.contains(&p.id) && self.known_in_doc.contains(&p.id))
                    .map(|p| p.id)
                    .collect();
                if !to_drop.is_empty() {
                    self.workspace
                        .data
                        .pages
                        .retain(|p| !to_drop.contains(&p.id));
                    changed = true;
                    if let Some(s) = self.workspace.selected()
                        && self.workspace.get(s).is_none()
                    {
                        self.workspace.set_selected(None);
                    }
                }
                // Push any local page the document has never held yet (seed
                // carry-over or a test-created page), recording it as known.
                for p in self.workspace.pages().to_vec() {
                    if !doc_ids.contains(&p.id) {
                        self.perform_action(state, &actions::upsert_page_action(&p));
                        self.seed_legacy_page_body(state, p.id);
                        self.known_in_doc.insert(p.id);
                    }
                }
                if changed {
                    self.sidebar.sync_from_workspace(&self.workspace);
                }
            } else {
                // First shared state we've seen — adopt it wholesale,
                // discarding our locally-seeded welcome so we don't push
                // a duplicate page tree.
                tracing::debug!(
                    target: "tilo.state",
                    event = "notes_state_adopted",
                    pages = doc_pages.len(),
                    "adopting initial shared Notes state"
                );
                let ids: Vec<PageId> = doc_pages.iter().map(|p| p.id).collect();
                self.workspace.data.pages = doc_pages;
                self.known_in_doc.extend(ids);
                if let Some(s) = self.workspace.selected()
                    && self.workspace.get(s).is_none()
                {
                    self.workspace.set_selected(None);
                }
                self.bound_page = None;
                self.main.clear();
                self.adopted = true;
                self.seeded = true;
                self.sidebar.sync_from_workspace(&self.workspace);
            }
        } else if !self.seeded && self.started.elapsed() > SEED_GRACE {
            // Doc still empty after the grace window — we're the origin.
            tracing::debug!(
                target: "tilo.state",
                event = "notes_state_seeded",
                pages = self.workspace.pages().len(),
                empty_for_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
                "seeding initial local Notes state"
            );
            for p in self.workspace.pages().to_vec() {
                self.perform_action(state, &actions::upsert_page_action(&p));
                self.seed_legacy_page_body(state, p.id);
                self.known_in_doc.insert(p.id);
            }
            self.seeded = true;
            self.adopted = true;
        }
    }

    /// Reconcile tags after the page tree. Tags and membership edges are
    /// independent CRDT keys, but an edge is only meaningful when both its
    /// endpoints survive; filtering here makes delete-wins deterministic even
    /// when a stale peer replays an assignment after a tombstone arrived.
    fn sync_tags(&mut self, state: &mut AutoCommit) {
        if !self.adopted {
            return;
        }
        let before_tags = self.workspace.data.tags.clone();
        let before_assignments = self.workspace.data.tag_assignments.clone();
        let doc_tags = NotesDoc::list_tags(state);
        let doc_tag_ids = doc_tags
            .iter()
            .map(|tag| tag.id)
            .collect::<std::collections::HashSet<_>>();
        let doc_page_ids = self
            .workspace
            .pages()
            .iter()
            .map(|page| page.id)
            .collect::<std::collections::HashSet<_>>();

        // Authoritative pull for tags the document has seen. A newly-created
        // local tag is dispatched immediately, so it is either visible here or
        // remains an explicitly unknown local value to push below.
        self.workspace.data.tags.retain(|tag| {
            doc_tag_ids.contains(&tag.id) || !self.known_tags_in_doc.contains(&tag.id)
        });
        for tag in &doc_tags {
            self.known_tags_in_doc.insert(tag.id);
            match self
                .workspace
                .data
                .tags
                .iter_mut()
                .find(|candidate| candidate.id == tag.id)
            {
                Some(local) => *local = tag.clone(),
                None => self.workspace.data.tags.push(tag.clone()),
            }
        }
        for tag in self.workspace.tags().to_vec() {
            if !doc_tag_ids.contains(&tag.id) {
                self.perform_action(state, &actions::upsert_tag_action(&tag));
                self.known_tags_in_doc.insert(tag.id);
            }
        }
        self.workspace.data.tag_assignments = NotesDoc::list_tag_assignments(state)
            .into_iter()
            .filter(|(page, tag)| doc_page_ids.contains(page) && doc_tag_ids.contains(tag))
            .map(|(page, tag)| TagAssignment { page, tag })
            .collect();
        if self
            .active_tag_filter
            .is_some_and(|tag| !doc_tag_ids.contains(&tag))
        {
            self.active_tag_filter = None;
        }
        if self.workspace.data.tags != before_tags
            || self.workspace.data.tag_assignments != before_assignments
        {
            self.refresh_link_pages();
        }
    }

    /// Ensure a page is selected and the `MainPanel` editor is bound to
    /// that page's `blocks` List in the shared doc, so its text syncs.
    fn ensure_editor_bound(&mut self, state: &mut AutoCommit) {
        // Don't touch the doc until we've seeded or adopted the shared
        // workspace. Binding earlier would `upsert` a page into the doc,
        // creating an independent `pages` Map before the late-join
        // handshake establishes a shared base — which makes two peers'
        // page trees conflict on merge and never converge.
        if !self.adopted {
            return;
        }
        if self.workspace.selected().is_none()
            && let Some(first) = self.workspace.children_of(None).first().map(|p| p.id)
        {
            self.workspace.set_selected(Some(first));
        }
        let Some(sel) = self.workspace.selected() else {
            if self.main.page_id().is_some() {
                self.main.clear();
            }
            self.bound_page = None;
            return;
        };
        if self.bound_page != Some(sel) {
            self.bind_selected_page(state, sel);
            if self.sidebar.is_focused() {
                // Opening a page from the page tree (a row, New note) keeps
                // keyboard focus in the tree. The freshly bound editor keeps
                // its caret position but must not also report focus: two
                // focused siblings leave the host with no keyboard owner, so
                // Ctrl+Z and browser text input would reach nothing.
                self.main.editor_mut().set_focus(false);
            }
        }
    }

    /// Bind the editor to `sel`'s blocks in the shared doc. The page must
    /// already exist in the doc (placed there by seeding or adoption) —
    /// we deliberately do NOT create it here, so binding can never fork
    /// the `pages` Map ahead of the handshake.
    fn bind_selected_page(&mut self, state: &mut AutoCommit, sel: PageId) {
        if NotesDoc::page_body(state, sel).is_none() {
            return; // page not in the shared doc yet — wait for sync
        }
        let title = self
            .workspace
            .get(sel)
            .map(|m| m.title.clone())
            .unwrap_or_default();
        if let Some(legacy) = &self.legacy_fixture_storage {
            let body = match storage::read_page_body(&legacy.root, sel) {
                Ok(body) => body,
                Err(error) => {
                    // Never turn an unreadable durable note into an editable
                    // empty one: the saver would replace the original bytes.
                    tracing::warn!(%error, page = %sel, "notes: page bind refused");
                    if self.legacy_read_blocked != Some(sel) {
                        self.report_page_read_failure(&error);
                    }
                    self.legacy_read_blocked = Some(sel);
                    self.bound_page = None;
                    return;
                }
            };
            let body = if self.legacy_read_blocked.take() == Some(sel)
                && self.main.page_id() == Some(sel)
                && self.main.has_unsaved_body()
            {
                // The user may already have typed during the initial seed
                // grace. Recover that draft rather than the disk copy.
                self.main.take_body_snapshot()
            } else {
                body
            };
            // A page's first blocks are a durable mutation too. Seed them
            // through the same action host that owns every later edit.
            self.seed_page_body(state, sel, &body);
        }
        match self.main.bind_canonical_page(state, sel, &title) {
            Ok(()) => {
                self.bound_page = Some(sel);
                self.refresh_link_pages();
            }
            Err(error) => tracing::warn!(%error, "notes: page bind refused"),
        }
    }

    /// Adopt a legacy Markdown body only when the explicit disk adapter exists.
    /// Canonical notebooks already own their bodies in the shared document.
    fn seed_legacy_page_body(&mut self, doc: &mut AutoCommit, page: PageId) {
        let Some(legacy) = &self.legacy_fixture_storage else {
            return;
        };
        let body = match storage::read_page_body(&legacy.root, page) {
            Ok(body) => body,
            Err(error) => {
                tracing::warn!(%error, %page, "notes: unreadable page was not seeded");
                return;
            }
        };
        self.seed_page_body(doc, page, &body);
    }

    /// Seed an otherwise-empty durable page through the canonical action
    /// executor. This is intentionally called for every new page rather than
    /// only the page currently visible in the editor, so a collaborator gets
    /// complete duplicate/imported content without the originating client
    /// having to open every descendant first.
    fn seed_page_body(&mut self, doc: &mut AutoCommit, page: PageId, body: &str) {
        if NotesDoc::page_body(doc, page).is_none() {
            tracing::warn!(%page, "notes: refused to seed a missing page");
            return;
        }
        let empty = NotesDoc::page_block_snapshots(doc, page)
            .iter()
            .all(|(_, text)| text.is_empty());
        if !empty || NotesDoc::page_blocks(doc, page).len() > 1 {
            return;
        }
        let titles = self
            .workspace
            .pages()
            .iter()
            .map(|meta| (meta.title.clone(), meta.id))
            .collect::<HashMap<_, _>>();
        let blocks = MainPanel::seed_blocks(body, |title| titles.get(title).copied());
        if blocks.iter().all(|block| block.text.is_empty()) && blocks.len() <= 1 {
            return;
        }
        let action = actions::write_body_action(page, &blocks);
        if let Err(error) = self.actions.try_dispatch_with_context(
            doc,
            actions::NOTES_ACTION_TYPE,
            &action,
            actions::history_context(&action),
        ) {
            tracing::error!(%error, %page, "notes: could not seed page blocks");
        }
    }

    /// Set the local user's id for authorship attribution. The same id
    /// the binary derives from `QUICK_APP_ID` and uses for presence, so
    /// authored text and the peer's cursor share a color.
    pub fn set_local_author(&mut self, author: Option<String>) {
        self.main.set_local_author(author);
    }

    /// Test hook: collapse the seed grace window so the next `sync`
    /// seeds immediately (tests can't wait 800 ms of wall-clock).
    #[doc(hidden)]
    pub fn bypass_seed_grace_for_test(&mut self) {
        self.started = ui::time::now()
            .checked_sub(SEED_GRACE + Duration::from_secs(1))
            .unwrap_or_else(ui::time::now);
        // Mark first-sync as done so `sync` doesn't re-base `started`
        // back to "now" and undo the bypass.
        self.synced_once = true;
    }

    /// Test hook: the editor's block text joined by newlines.
    #[doc(hidden)]
    pub fn editor_text_for_test(&self) -> String {
        self.main
            .editor()
            .blocks()
            .iter()
            .map(ui::widget::Block::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    // --- action dispatch -------------------------------------------------

    /// Dispatch one [`NotesAction`] — the single CRDT-mutation path shared with
    /// the Ctrl+K palette. Applied through the Tilo action executor (a pinned
    /// actor + committed change, so it replays byte-identically on peers). The
    /// document heads captured just before the first action of the current event
    /// are stashed so the host records **one** undo boundary bracketing it
    /// ([`CommittedActions::take_committed_action`](ui::undo::CommittedActions::take_committed_action)). The in-memory workspace is updated by
    /// the caller for immediate UI feedback and reconciled from the document on
    /// the next `sync`. This app supplies only its domain: the action and its type.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &NotesAction) {
        self.actions.dispatch(doc, action);
    }

    /// Empty surface for the framework host; [`Self::adopt_resource_state`]
    /// installs the live workspace before the first frame.
    pub(crate) fn empty_surface(author: &ui::permissions::UserId) -> Self {
        let mut sidebar = PageSidebar::new();
        sidebar.sync_from_workspace(&Workspace::default());
        let mut main = MainPanel::new();
        main.set_local_author(Some(author.0.clone()));
        let mut app = Self::build_widget(Workspace::default(), sidebar, main, None);
        app.seeded = true;
        app.adopted = true;
        app
    }

    /// Present the framework's workspace projection.
    pub(crate) fn adopt_resource_state(&mut self, state: &std::rc::Rc<Workspace>) {
        self.workspace = (**state).clone();
        self.sidebar.sync_from_workspace(&self.workspace);
        self.bound_page = self.workspace.selected();
        self.refresh_link_pages();
        self.self_dirty = true;
    }

    /// Attach the framework dispatcher when hosted as a surface.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Notebook>,
    ) {
        self.actions.attach(dispatcher);
        self.seeded = true;
        self.adopted = true;
    }

    /// The context Ctrl+K needs to build the notebook's resource tree: the page
    /// tree the workspace holds (which mirrors the shared `pages` map).
    pub(crate) fn notes_resource_ctx(&self) -> crate::actions::NotesResourceCtx {
        crate::actions::NotesResourceCtx {
            pages: self.workspace.pages().to_vec(),
            // The palette snapshot does not eagerly materialize block
            // resources.::resource resolves a requested
            // stable block id from the durable document, after stale checks.
            blocks: HashMap::new(),
            tags: self.workspace.tags().to_vec(),
            tag_assignments: self
                .workspace
                .data
                .tag_assignments
                .iter()
                .map(|edge| (edge.page, edge.tag))
                .collect(),
            can_go_back: self.route_history.can_go_back(),
            can_go_forward: self.route_history.can_go_forward(),
        }
    }

    pub(crate) fn notes_resource_ctx_for(
        &self,
        doc: &AutoCommit,
    ) -> Option<crate::actions::NotesResourceCtx> {
        if self.legacy_fixture_storage.is_some() {
            return Some(self.notes_resource_ctx());
        }
        let workspace = crate::notebook::project(doc, None).ok()?;
        Some(crate::actions::NotesResourceCtx {
            pages: workspace.pages().to_vec(),
            blocks: HashMap::new(),
            tags: workspace.tags().to_vec(),
            tag_assignments: workspace
                .data
                .tag_assignments
                .iter()
                .map(|edge| (edge.page, edge.tag))
                .collect(),
            can_go_back: self.route_history.can_go_back(),
            can_go_forward: self.route_history.can_go_forward(),
        })
    }

    fn refresh_search_blocks(&mut self, doc: &AutoCommit) {
        let heads = doc.base_heads();
        if automerge::transaction::Transactable::pending_ops(doc) == 0
            && self.search_heads.as_ref() == Some(&heads)
        {
            return;
        }
        self.search_blocks = self
            .workspace
            .pages()
            .iter()
            .map(|page| {
                let blocks = NotesDoc::page_block_snapshots(doc, page.id)
                    .into_iter()
                    .map(|(id, text)| crate::actions::NoteBlockResource { id, text })
                    .collect();
                (page.id, blocks)
            })
            .collect();
        let known_pages = self
            .workspace
            .pages()
            .iter()
            .map(|page| page.id)
            .collect::<std::collections::HashSet<_>>();
        let mut backlinks = HashMap::<PageId, HashMap<PageId, u32>>::new();
        for source in self.workspace.pages().iter().map(|page| page.id) {
            for (_, marks) in NotesDoc::page_block_marks(doc, source) {
                for mark in marks {
                    let actions::InlineMark::Link { page } = mark.mark else {
                        continue;
                    };
                    let Ok(target) = page.parse::<PageId>() else {
                        continue;
                    };
                    if target != source && known_pages.contains(&target) {
                        let count = backlinks
                            .entry(target)
                            .or_default()
                            .entry(source)
                            .or_default();
                        *count = count.saturating_add(1);
                    }
                }
            }
        }
        self.backlink_index = backlinks
            .into_iter()
            .map(|(target, sources)| {
                let mut sources = sources.into_iter().collect::<Vec<_>>();
                sources.sort_by_key(|(source, _)| *source);
                (target, sources)
            })
            .collect();
        self.search_provider
            .update(&self.workspace, &self.search_blocks);
        self.search_heads = Some(heads);
    }

    /// Snapshot every page's metadata by id, for the pre/post diff that turns an
    /// applied [`OutEvent`] into the matching [`NotesAction`](crate::actions).
    fn meta_map(&self) -> HashMap<PageId, crate::model::PageMeta> {
        self.workspace
            .pages()
            .iter()
            .map(|p| (p.id, p.clone()))
            .collect()
    }

    /// Canonical synchronization is a projection, never an attempt to replay
    /// local rows that are absent from the document.
    fn reconcile_canonical(&mut self, doc: &AutoCommit) -> Result<(), String> {
        self.workspace =
            crate::notebook::project_without_asset_rehash(doc, self.workspace.selected())?;
        if self
            .active_tag_filter
            .is_some_and(|tag| self.workspace.get_tag(tag).is_none())
        {
            self.active_tag_filter = None;
        }
        self.sidebar.sync_from_workspace(&self.workspace);
        if let Some(page) = self
            .workspace
            .selected()
            .and_then(|id| self.workspace.get(id))
        {
            self.main.set_title(Some(page.title.clone()));
        }
        self.refresh_link_pages();
        Ok(())
    }

    fn dispatch_canonical_action(
        &mut self,
        doc: &mut AutoCommit,
        action: &NotesAction,
    ) -> Result<(), String> {
        self.actions
            .try_dispatch_with_context(
                doc,
                actions::NOTES_ACTION_TYPE,
                action,
                actions::history_context(action),
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one flat dispatch over every canonical Notes event kind"
    )]
    fn apply_canonical_event(
        &mut self,
        doc: &mut AutoCommit,
        event: OutEvent,
    ) -> Result<(), String> {
        self.reconcile_canonical(doc)?;
        // Direct title/create affordances use the current published contract,
        // just like named actions. Check before any optimistic metadata edit.
        let resource_action = match &event {
            OutEvent::Rename { id, .. } => Some((actions::page_resource_id(*id), "rename")),
            OutEvent::CreateChild { parent: Some(id) } => {
                Some((actions::page_resource_id(*id), "new-child"))
            }
            OutEvent::CreateChild { parent: None } => Some(("notes.notebook".into(), "new-page")),
            OutEvent::Delete(id) => Some((actions::page_resource_id(*id), "delete")),
            OutEvent::Duplicate(id) => Some((actions::page_resource_id(*id), "duplicate")),
            OutEvent::Reparent {
                id,
                new_parent,
                before,
            } => {
                let published_action = (|| {
                    let current = self.workspace.get(*id)?;
                    if current.parent != *new_parent {
                        let siblings = self.workspace.children_of(current.parent);
                        let index = siblings.iter().position(|page| page.id == *id)?;
                        if index
                            .checked_sub(1)
                            .and_then(|previous| siblings.get(previous))
                            .is_some_and(|page| Some(page.id) == *new_parent)
                            && before.is_none()
                        {
                            return Some("indent");
                        }
                        let parent = self.workspace.get(current.parent?)?;
                        let parent_siblings = self.workspace.children_of(parent.parent);
                        let expected_before = parent_siblings
                            .iter()
                            .skip_while(|page| page.id != parent.id)
                            .nth(1)
                            .map(|page| page.id);
                        if *new_parent == parent.parent && *before == expected_before {
                            return Some("outdent");
                        }
                        return None;
                    }

                    let siblings = self.workspace.children_of(current.parent);
                    let old_index = siblings.iter().position(|page| page.id == *id)?;
                    let without_current = siblings
                        .into_iter()
                        .filter(|page| page.id != *id)
                        .collect::<Vec<_>>();
                    let target_index = before
                        .and_then(|before| {
                            without_current.iter().position(|page| page.id == before)
                        })
                        .unwrap_or(without_current.len());
                    if target_index.checked_add(1) == Some(old_index) {
                        Some("move-up")
                    } else if target_index == old_index.saturating_add(1) {
                        Some("move-down")
                    } else {
                        None
                    }
                })()
                // Arbitrary drag reparenting has no published named contract.
                // Unbound legacy hosts allow it; bound custody fails closed.
                .unwrap_or("direct-reparent");
                Some((actions::page_resource_id(*id), published_action))
            }
            _ => None,
        };
        if let Some((resource_id, action_id)) = resource_action {
            let resource = ui::resources::resource(self, doc, &resource_id)
                .ok_or("page action resource no longer exists")?;
            ui::ActionHost::authorize_resource_action(doc, &resource, action_id)?;
        }
        let before = self.meta_map();
        let tree_move = matches!(event, OutEvent::Reparent { .. });
        match event {
            OutEvent::Select(id) => {
                if self.workspace.get(id).is_none() {
                    return Err("page no longer exists".into());
                }
                self.workspace.set_selected(Some(id));
            }
            OutEvent::CreateChild { parent } => {
                if parent.is_some_and(|id| self.workspace.get(id).is_none()) {
                    return Err("parent page no longer exists".into());
                }
                let id = self.workspace.create_page(parent);
                self.workspace.set_selected(Some(id));
            }
            OutEvent::Rename { id, new_title } => {
                if self.workspace.get(id).is_none() {
                    return Err("page no longer exists".into());
                }
                self.workspace.rename(id, &new_title);
            }
            OutEvent::Delete(id) => {
                if self.workspace.get(id).is_none() {
                    return Err("page no longer exists".into());
                }
                let action = actions::delete_page_action(id);
                self.dispatch_canonical_action(doc, &action)?;
                self.reconcile_canonical(doc)?;
                self.record_current_route();
                return Ok(());
            }
            OutEvent::Reparent {
                id,
                new_parent,
                before,
            } => {
                if self.workspace.get(id).is_none()
                    || new_parent.is_some_and(|id| self.workspace.get(id).is_none())
                {
                    return Err("page no longer exists".into());
                }
                self.workspace.move_to(id, new_parent, before);
            }
            OutEvent::Duplicate(source) => {
                let action = actions::duplicate_subtree_action(doc, source)?;
                let NotesAction::DuplicateSubtree { pages, .. } = &action else {
                    return Err("invalid duplicate action".into());
                };
                let destination = pages
                    .iter()
                    .find(|page| page.source == source.to_string())
                    .ok_or("duplicate root is missing")?
                    .destination
                    .parse::<PageId>()
                    .map_err(|e| e.to_string())?;
                self.dispatch_canonical_action(doc, &action)?;
                self.workspace.set_selected(Some(destination));
                self.reconcile_canonical(doc)?;
                self.record_current_route();
                return Ok(());
            }
        }
        let after = self.meta_map();
        if tree_move && before != after {
            let mut before = before
                .values()
                .map(actions::PageData::from_meta)
                .collect::<Vec<_>>();
            let mut after = after
                .values()
                .map(actions::PageData::from_meta)
                .collect::<Vec<_>>();
            before.sort_by(|a, b| a.id.cmp(&b.id));
            after.sort_by(|a, b| a.id.cmp(&b.id));
            let action = NotesAction::UpdatePageTree { before, after };
            self.dispatch_canonical_action(doc, &action)?;
        } else {
            self.dispatch_tree_diff(doc, &before, &after);
        }
        // Rejected optimistic metadata is always discarded, including New.
        self.reconcile_canonical(doc)?;
        self.record_current_route();
        self.refresh_status();
        Ok(())
    }

    /// Apply one sidebar [`OutEvent`] to the workspace, then dispatch the
    /// matching [`NotesAction`](crate::actions) so the change lands in the shared
    /// document on the same replayable path the Ctrl+K palette uses.
    fn apply_and_dispatch(&mut self, doc: &mut AutoCommit, ev: OutEvent) {
        if self.legacy_fixture_storage.is_none() {
            if let Err(error) = self.apply_canonical_event(doc, ev) {
                tracing::warn!(%error, "notes: rejected page action; reconciling");
                let _ = self.reconcile_canonical(doc);
                self.self_dirty = true;
            }
            return;
        }
        let before = self.meta_map();
        self.apply_event(ev);
        let after = self.meta_map();
        self.dispatch_tree_diff(doc, &before, &after);
        // The action path removed any deleted page from the document itself, so
        // drop the removals `apply_event` queued for the next `sync` — replaying
        // them would re-delete a page a Ctrl+Z undo has just restored.
        self.pending_doc_removals.clear();
    }

    /// Turn the page-tree change [`apply_event`] just made into the matching
    /// action(s). Deletes are dispatched before upserts so a delete's own sibling
    /// renumber (which `apply` recomputes in the document) doesn't double-write.
    fn dispatch_tree_diff(
        &mut self,
        doc: &mut AutoCommit,
        before: &HashMap<PageId, crate::model::PageMeta>,
        after: &HashMap<PageId, crate::model::PageMeta>,
    ) {
        // Deletions: a page present before and gone after. Dispatch one
        // DeletePage per *deletion root* (a removed page whose parent wasn't
        // also removed) — `apply` deletes the whole subtree, so descendants need
        // no separate action.
        let mut previous = before.iter().collect::<Vec<_>>();
        previous.sort_by_key(|(id, _)| **id);
        for (id, meta) in previous {
            if after.contains_key(id) {
                continue;
            }
            let parent_also_removed = meta
                .parent
                .is_some_and(|p| before.contains_key(&p) && !after.contains_key(&p));
            if !parent_also_removed {
                self.perform_action(doc, &actions::delete_page_action(*id));
            }
        }
        // Creates + edits: a page new in `after`, or one whose metadata changed
        // (title / parent / order). Each is a whole-page upsert.
        let mut next = after.iter().collect::<Vec<_>>();
        next.sort_by_key(|(id, _)| **id);
        for (id, meta) in next {
            match before.get(id) {
                Some(old) if old == meta => {}
                _ => self.perform_action(doc, &actions::upsert_page_action(meta)),
            }
        }
        // Page metadata alone is not a usable collaborative note. Seed every
        // newly-created page's persisted body now (including every descendant
        // of a sidebar Duplicate), rather than waiting until each one is
        // selected and accidentally making peer content view-dependent.
        for id in after.keys().filter(|id| !before.contains_key(*id)) {
            self.seed_legacy_page_body(doc, *id);
        }
    }

    /// Apply one [`OutEvent`] from the sidebar to the workspace + disk.
    fn apply_event(&mut self, ev: OutEvent) {
        if self.legacy_read_blocked.is_some() {
            self.report_page_read_failure(
                "The current note could not be read; its unsaved draft is still open",
            );
            return;
        }
        if self.legacy_fixture_storage.is_none() {
            self.pending_events.push(ev);
            return;
        }
        self.apply_legacy_event(ev);
    }

    fn apply_legacy_event(&mut self, ev: OutEvent) {
        let Some(legacy) = &self.legacy_fixture_storage else {
            return;
        };
        let root = legacy.root.clone();
        match ev {
            OutEvent::Select(id) => {
                // Flush any pending edits to the previously-selected
                // page before swapping. Otherwise a quick click from
                // page A to page B could lose A's most recent edit
                // if the debounce window hadn't elapsed yet.
                self.navigate_to(id);
            }
            OutEvent::Rename { id, new_title } => {
                self.workspace.rename(id, &new_title);
                if self.workspace.selected() == Some(id) {
                    self.main.set_title(Some(new_title));
                }
            }
            OutEvent::Delete(id) => {
                let removed = self.workspace.delete_page(id);
                // Propagate the deletion to the shared doc (done in `sync`,
                // which has CRDT access) so it isn't resurrected next frame.
                self.pending_doc_removals.extend(removed.iter().copied());
                for rid in &removed {
                    if let Err(e) = storage::delete_page_file(&root, *rid) {
                        tracing::error!(error = %e, page = %rid, "notes: delete page file failed");
                    }
                }
                // If the selection was nuked, pick a reasonable fallback.
                if self.workspace.selected().is_none() {
                    let first = self.workspace.children_of(None).first().map(|p| p.id);
                    if let Some(id) = first {
                        self.main.clear();
                        self.navigate_to(id);
                    } else {
                        self.main.clear();
                    }
                }
            }
            OutEvent::CreateChild { parent } => {
                self.flush_main_panel_edit();
                let id = self.workspace.create_page(parent);
                self.workspace.set_selected(Some(id));
                if let Err(e) = storage::write_page_body(&root, id, "") {
                    tracing::error!(error = %e, page = %id, "notes: write empty page failed");
                }
                self.main.load_page(id, "Untitled", "");
            }
            OutEvent::Reparent {
                id,
                new_parent,
                before,
            } => {
                // Drag-drop reorder / re-parent. The new `parent` + `order`
                // propagate to the shared doc on the next `sync` and to
                // disk via `persist()` below. `move_to` rejects cycles.
                self.workspace.move_to(id, new_parent, before);
            }
            OutEvent::Duplicate(id) => {
                self.flush_main_panel_edit();
                let mut duplicate = self.workspace.clone();
                if let Some((new_root, mapping)) = duplicate.duplicate_page(id) {
                    // Read the entire subtree before publishing any copied
                    // metadata. One failed source must not produce blank copies.
                    let bodies = mapping
                        .iter()
                        .map(|(old, new)| {
                            storage::read_page_body(&root, *old).map(|body| (*new, body))
                        })
                        .collect::<Result<Vec<_>, _>>();
                    let bodies = match bodies {
                        Ok(bodies) => bodies,
                        Err(error) => {
                            self.report_page_read_failure(&error);
                            return;
                        }
                    };
                    // Copy each source page's body onto its fresh copy id so
                    // the duplicated subtree has independent content on disk.
                    for (new, body) in &bodies {
                        if let Err(e) = storage::write_page_body(&root, *new, body) {
                            tracing::error!(error = %e, page = %new, "notes: duplicate body write failed");
                            self.transfer_toasts.push(
                                format!("Could not duplicate note: {e}"),
                                ui::widget::toast::ToastKind::Error,
                            );
                            self.self_dirty = true;
                            return;
                        }
                    }
                    self.workspace = duplicate;
                    self.workspace.set_selected(Some(new_root));
                    if let Some(meta) = self.workspace.get(new_root) {
                        let title = meta.title.clone();
                        if let Some((_, body)) = bodies.iter().find(|(id, _)| *id == new_root) {
                            self.load_page_with_resolver(new_root, &title, body);
                        }
                    }
                }
            }
        }
        self.record_current_route();
        // After every event refresh the link pages so the popovers
        // see fresh titles.
        self.refresh_link_pages();
        self.persist();
        self.refresh_status();
    }

    /// Load a page through `MainPanel::load_page_with_link_resolver`,
    /// resolving `[[Title]]` link marks against the current
    /// workspace's page list.
    fn load_page_with_resolver(&mut self, id: PageId, title: &str, body: &str) {
        // Snapshot the title→id map ahead of borrowing main mutably.
        let by_title: std::collections::HashMap<String, PageId> = self
            .workspace
            .pages()
            .iter()
            .map(|p| (p.title.clone(), p.id))
            .collect();
        self.main
            .load_page_with_link_resolver(id, title, body, |t| by_title.get(t).copied());
        self.main.set_tags(self.selected_tag_chips());
        self.main.set_backlinks(self.selected_backlink_chips());
    }

    fn record_current_route(&mut self) {
        let route = self.workspace.selected();
        if self.history_target == Some(route) {
            self.history_target = None;
            return;
        }
        self.route_history.push(route);
    }

    fn navigate_history(
        &mut self,
        action: ui::NavAction,
        doc: &AutoCommit,
    ) -> Result<bool, String> {
        loop {
            let Some(route) = self.route_history.apply(action).copied() else {
                return Ok(false);
            };
            let Some(page) = route else {
                self.history_target = Some(None);
                self.workspace.set_selected(None);
                self.main.clear();
                self.record_current_route();
                self.self_dirty = true;
                return Ok(true);
            };
            let page_exists = if self.legacy_fixture_storage.is_some() {
                self.workspace.get(page).is_some()
            } else {
                NotesDoc::page_body(doc, page).is_some()
            };
            if !page_exists {
                // Deleted history entries are skipped, just as a browser skips
                // a no-longer-restorable session route instead of stranding
                // Back/Forward on an invalid destination.
                continue;
            }
            self.history_target = Some(Some(page));
            self.navigate_to(page);
            self.self_dirty = true;
            return Ok(true);
        }
    }

    fn handle_route_history_shortcut(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        doc: &AutoCommit,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(key),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let mods = modifiers.state();
        let action = match key {
            NamedKey::ArrowLeft if mods.alt_key() => ui::NavAction::Back,
            NamedKey::ArrowRight if mods.alt_key() => ui::NavAction::Forward,
            _ => return false,
        };
        match self.navigate_history(action, doc) {
            Ok(changed) => {
                *handled = changed;
                changed
            }
            Err(error) => {
                tracing::warn!(%error, "notes: navigation history target went stale");
                false
            }
        }
    }

    /// Navigate to `id`: select + load the page. Used both by the
    /// Cmd-K palette and by click-on-link events.
    pub fn navigate_to(&mut self, id: PageId) {
        if let Err(error) = self.try_navigate_to(id) {
            self.report_page_read_failure(&error);
        }
    }

    fn report_page_read_failure<E>(&mut self, error: &E)
    where
        E: std::fmt::Display + ?Sized,
    {
        tracing::warn!(%error, "notes: page read refused");
        self.transfer_toasts.push(
            format!("Could not read note: {error}"),
            ui::widget::toast::ToastKind::Error,
        );
        self.self_dirty = true;
    }

    fn try_navigate_to(&mut self, id: PageId) -> Result<(), String> {
        if self.legacy_read_blocked.is_some() {
            return Err(
                "The current note could not be read; its unsaved draft is still open".into(),
            );
        }
        if self.legacy_fixture_storage.is_none() {
            self.pending_events.push(OutEvent::Select(id));
            return Ok(());
        }
        let Some(legacy) = &self.legacy_fixture_storage else {
            return Ok(());
        };
        let root = legacy.root.clone();
        self.flush_main_panel_edit();
        if let Some(meta) = self.workspace.get(id) {
            let title = meta.title.clone();
            let body = storage::read_page_body(&root, id).map_err(|error| error.to_string())?;
            self.workspace.set_selected(Some(id));
            self.record_current_route();
            self.load_page_with_resolver(id, &title, &body);
            // `load_page_with_resolver` creates a fresh, unbound editor. This
            // matters even when opening the page already selected (for example
            // a brokered block reveal): force the next sync to bind it back to
            // the canonical UUID-addressed block list instead of leaving a
            // markdown-only editor whose random block ids cannot be revealed.
            self.bound_page = None;
            self.sidebar.sync_from_workspace(&self.workspace);
            self.persist();
        }
        Ok(())
    }

    fn request_markdown_import(&mut self) -> Result<(), String> {
        self.request_markdown_import_scoped(None)
    }
    fn request_markdown_import_scoped(
        &mut self,
        scope: Option<ResourceEffectScope>,
    ) -> Result<(), String> {
        if self.markdown_transfer.is_some()
            || self.markdown_job.is_pending()
            || self.markdown_invocation.is_some()
            || self.pending_markdown_source.is_some()
        {
            return Err("a file transfer is already pending".into());
        }
        let request = ui::portal::PickRequest::open("Import Markdown", "Import")
            .filter(ui::portal::FileFilter::new("Markdown").glob("*.md"))
            .thread_name("notes-import-markdown");
        let channel = send_markdown_pick(request, scope);
        self.markdown_transfer = Some((MarkdownTransfer::Import, channel));
        self.self_dirty = true;
        Ok(())
    }

    fn request_image_insert(&mut self, page: PageId) -> Result<(), String> {
        self.request_image_insert_scoped(page, None)
    }
    fn request_image_insert_scoped(
        &mut self,
        page: PageId,
        scope: Option<ResourceEffectScope>,
    ) -> Result<(), String> {
        if self.markdown_transfer.is_some()
            || self.markdown_job.is_pending()
            || self.markdown_invocation.is_some()
            || self.pending_markdown_source.is_some()
        {
            return Err("a file chooser is already open".into());
        }
        let request = ui::portal::PickRequest::open("Insert image", "Insert")
            .filter(
                ui::portal::FileFilter::new("Images")
                    .glob("*.png")
                    .glob("*.jpg")
                    .glob("*.jpeg")
                    .glob("*.webp"),
            )
            .thread_name("notes-insert-image");
        let channel = send_markdown_pick(request, scope);
        self.markdown_transfer = Some((MarkdownTransfer::InsertImage { page }, channel));
        self.self_dirty = true;
        Ok(())
    }

    fn request_markdown_export(&mut self, doc: &AutoCommit, page: PageId) -> Result<(), String> {
        self.request_markdown_export_scoped(doc, page, None)
    }
    fn request_markdown_export_scoped(
        &mut self,
        doc: &AutoCommit,
        page: PageId,
        scope: Option<ResourceEffectScope>,
    ) -> Result<(), String> {
        if self.markdown_transfer.is_some()
            || self.markdown_job.is_pending()
            || self.markdown_invocation.is_some()
            || self.pending_markdown_source.is_some()
        {
            return Err("a file chooser is already open".into());
        }
        let mut workspace = self.workspace.clone();
        workspace.set_selected(Some(page));
        let panel = self.main.project_canonical(doc, &workspace)?;
        let markdown =
            crate::markdown::serialize(&crate::markdown::blocks_to_specs(panel.editor().blocks()));
        let title = workspace.get(page).map_or("note", |meta| meta.title.trim());
        let safe = title
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                    ch
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let suggested = format!("{}.md", safe.trim_matches('-').to_lowercase());
        let request = ui::portal::PickRequest::save(
            "Export Markdown",
            "Export",
            if suggested == ".md" {
                "note.md"
            } else {
                &suggested
            },
        )
        .filter(ui::portal::FileFilter::new("Markdown").glob("*.md"))
        .thread_name("notes-export-markdown");
        let channel = send_markdown_pick(request, scope);
        self.markdown_transfer = Some((
            MarkdownTransfer::Export {
                markdown: markdown.into_bytes(),
                // Workspace-scoped attachments stay exportable beside embedded
                // canonical assets, which are read from the authoritative document.
                asset_root: self.workspace_asset_root.clone(),
                canonical_assets: if self.legacy_fixture_storage.is_none() {
                    crate::notebook::asset_snapshot(doc)?
                } else {
                    HashMap::new()
                },
            },
            channel,
        ));
        self.self_dirty = true;
        Ok(())
    }

    fn complete_markdown_operation(&mut self, outcome: Result<tilo_actions::ActionOutput, String>) {
        let Some(operation) = self.markdown_invocation.take() else {
            return;
        };
        self.complete_operation(operation, outcome);
    }
    fn complete_operation(
        &mut self,
        mut operation: PendingOperation,
        outcome: Result<tilo_actions::ActionOutput, String>,
    ) {
        let invocation = operation.invocation;
        let outcome = match (&operation.scope, outcome) {
            (Some(scope), Ok(output)) => scope.check().map(|()| output),
            (_, outcome) => outcome,
        };
        if outcome.is_ok() {
            // The SDK still owns this same scope until it delivers the terminal
            // receipt. Success releases our clone without retiring that custody.
            // Failed or abandoned jobs keep Drop's irreversible cancellation.
            drop(operation.scope.take());
        }
        self.markdown_results.push(match outcome {
            Ok(output) => tilo_actions::InvocationUpdate::Succeeded { invocation, output },
            Err(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "notes_transfer_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        });
    }

    #[cfg(test)]
    fn finish_image_insert(
        &mut self,
        doc: &mut AutoCommit,
        page: PageId,
        path: &std::path::Path,
    ) -> Result<(), String> {
        let imported = load_image(
            self.legacy_fixture_storage
                .as_ref()
                .map(|legacy| legacy.root.as_path()),
            path,
            None,
        );
        self.insert_loaded_image(doc, page, path, imported)
    }
    fn insert_loaded_image(
        &mut self,
        doc: &mut AutoCommit,
        page: PageId,
        path: &std::path::Path,
        imported: Result<LoadedImage, String>,
    ) -> Result<(), String> {
        let (destination, canonical_bytes) = match imported {
            Ok(imported) => imported,
            Err(error) => {
                self.transfer_toasts.push(
                    format!("Image import failed: {error}"),
                    ui::widget::toast::ToastKind::Error,
                );
                return Err(error);
            }
        };
        let alt = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("Image")
            .replace([']', '[', '(', ')'], " ");
        let after = (self.main.page_id() == Some(page))
            .then(|| {
                self.main
                    .editor()
                    .focused_index()
                    .and_then(|index| self.main.editor().blocks().get(index))
                    .or_else(|| self.main.editor().blocks().last())
                    .map(|block| block.id.clone())
            })
            .flatten();
        let text = format!("![{}]({destination})", alt.trim());
        let edit = match after {
            Some(after) => tilo_document::RichTextAction::InsertParagraph { after, text },
            None => tilo_document::RichTextAction::AppendParagraph { text },
        };
        let action = if let Some(bytes) = canonical_bytes {
            actions::add_image_asset_action(page, bytes, edit)
        } else {
            actions::edit_body_action(page, edit)
        };
        let scope = self.markdown_scope();
        let result = self.dispatch_transfer_action(doc, &action, scope.as_ref());
        match result {
            Ok(()) => {
                self.bound_page = None;
                self.self_dirty = true;
                self.transfer_toasts
                    .push("Image inserted", ui::widget::toast::ToastKind::Success);
                Ok(())
            }
            Err(error) => {
                self.transfer_toasts.push(
                    format!("Image insert failed: {error}"),
                    ui::widget::toast::ToastKind::Error,
                );
                Err(error)
            }
        }
    }

    fn markdown_scope(&self) -> Option<ResourceInvocationScope> {
        self.markdown_invocation
            .as_ref()
            .and_then(|job| job.scope.clone())
    }
    fn markdown_effect_scope(&self) -> Result<Option<ResourceEffectScope>, String> {
        self.markdown_scope()
            .map(|scope| scope.effect_scope())
            .transpose()
    }
    fn dispatch_transfer_action(
        &mut self,
        doc: &mut AutoCommit,
        action: &NotesAction,
        scope: Option<&ResourceInvocationScope>,
    ) -> Result<(), String> {
        let mut dispatch = |doc: &mut AutoCommit| {
            self.actions
                .try_dispatch_with_context(
                    doc,
                    actions::NOTES_ACTION_TYPE,
                    action,
                    actions::history_context(action),
                )
                .map_err(|error| error.to_string())
        };
        match scope {
            Some(scope) => scope.with_document_mutation(doc, dispatch),
            None => dispatch(doc),
        }
    }
    fn retire_markdown_operation(&mut self, error: String) {
        self.markdown_transfer = None;
        self.markdown_job.cancel();
        self.pending_markdown_source = None;
        self.import_control_emitted = false;
        self.complete_markdown_operation(Err(error));
        self.self_dirty = true;
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one transfer inbox preserves original job custody through picker, file worker and canonical continuation"
    )]
    fn drain_markdown_transfer(&mut self, doc: &mut AutoCommit) {
        if let Some(error) = self.markdown_scope().and_then(|scope| scope.check().err()) {
            self.retire_markdown_operation(error);
            return;
        }
        if let Some(job) = self.markdown_job.try_take() {
            match job {
                MarkdownJobResult::Imported { path, result } => match result {
                    Ok((body, assets)) if self.pending_markdown_source.is_none() => {
                        match self.stage_pending_import(PendingMarkdownImport {
                            path,
                            body,
                            assets: Some(assets),
                        }) {
                            Ok(()) => {
                                self.import_control_emitted = false;
                                self.self_dirty = true;
                            }
                            Err(error) => self.complete_markdown_operation(Err(error)),
                        }
                    }
                    Ok(_) => self.complete_markdown_operation(Err(
                        "Another Markdown import is pending".into(),
                    )),
                    Err(error) => {
                        tracing::warn!(%error, path = %path.display(), "notes: Markdown import failed");
                        self.transfer_toasts.push(
                            format!("Import failed: {error}"),
                            ui::widget::toast::ToastKind::Error,
                        );
                        self.complete_markdown_operation(Err(error));
                    }
                },
                MarkdownJobResult::Exported { path, result } => {
                    match &result {
                        Ok(()) => self.transfer_toasts.push(
                            format!("Exported {}", path.display()),
                            ui::widget::toast::ToastKind::Success,
                        ),
                        Err(error) => self.transfer_toasts.push(
                            format!("Export failed: {error}"),
                            ui::widget::toast::ToastKind::Error,
                        ),
                    }
                    self.complete_markdown_operation(
                        result.map(|()| tilo_actions::ActionOutput::default()),
                    );
                }
                MarkdownJobResult::Image { page, path, result } => {
                    let outcome = self
                        .insert_loaded_image(doc, page, &path, result)
                        .map(|()| tilo_actions::ActionOutput::default());
                    self.complete_markdown_operation(outcome);
                }
            }
            return;
        }
        let outcome =
            self.markdown_transfer
                .as_ref()
                .and_then(|(_, channel)| match channel.try_recv() {
                    Ok(outcome) => Some(outcome),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Some(ui::portal::PortalOutcome::Failed(
                            "File chooser disconnected before returning a result".into(),
                        ))
                    }
                });
        let Some(outcome) = outcome else {
            return;
        };
        let Some((transfer, _)) = self.markdown_transfer.take() else {
            return;
        };
        let scope = match self.markdown_effect_scope() {
            Ok(scope) => scope,
            Err(error) => {
                self.retire_markdown_operation(error);
                return;
            }
        };
        match (transfer, outcome) {
            (
                MarkdownTransfer::Export {
                    markdown,
                    asset_root,
                    canonical_assets,
                },
                ui::portal::PortalOutcome::Opened(path),
            ) => {
                let completion = self.markdown_job.begin();
                let target = path.clone();
                let spawned = ui::rt::spawn_thread("notes-markdown-export", move || {
                    let result = write_portable_markdown_export_scoped(
                        asset_root.as_deref(),
                        &canonical_assets,
                        &path,
                        &markdown,
                        scope.as_ref(),
                    );
                    completion.complete(MarkdownJobResult::Exported { path, result });
                });
                if let Err(error) = spawned {
                    self.markdown_job
                        .begin()
                        .complete(MarkdownJobResult::Exported {
                            path: target,
                            result: Err(format!("could not start the export: {error}")),
                        });
                }
            }
            (MarkdownTransfer::Import, ui::portal::PortalOutcome::Opened(path)) => {
                let completion = self.markdown_job.begin();
                let root = self
                    .legacy_fixture_storage
                    .as_ref()
                    .map(|legacy| legacy.root.clone());
                let source = path.clone();
                let spawned = ui::rt::spawn_thread("notes-markdown-import", move || {
                    let result = (|| {
                        let body = file_effect(scope.as_ref(), || read_bounded_markdown(&path))??;
                        if let Some(root) = root {
                            adopt_portable_markdown_assets(&root, &path, &body, scope.as_ref())
                                .map(|body| (body, Vec::new()))
                        } else {
                            adopt_canonical_markdown_assets(&path, &body, scope.as_ref())
                        }
                    })();
                    completion.complete(MarkdownJobResult::Imported { path, result });
                });
                if let Err(error) = spawned {
                    self.markdown_job
                        .begin()
                        .complete(MarkdownJobResult::Imported {
                            path: source,
                            result: Err(format!("could not start the import: {error}")),
                        });
                }
            }
            (MarkdownTransfer::InsertImage { page }, ui::portal::PortalOutcome::Opened(path)) => {
                let completion = self.markdown_job.begin();
                let root = self
                    .legacy_fixture_storage
                    .as_ref()
                    .map(|legacy| legacy.root.clone());
                let source = path.clone();
                let spawned = ui::rt::spawn_thread("notes-insert-image", move || {
                    let result = load_image(root.as_deref(), &path, scope.as_ref());
                    completion.complete(MarkdownJobResult::Image { page, path, result });
                });
                if let Err(error) = spawned {
                    self.markdown_job
                        .begin()
                        .complete(MarkdownJobResult::Image {
                            page,
                            path: source,
                            result: Err(format!("could not start the image import: {error}")),
                        });
                }
            }
            (_, ui::portal::PortalOutcome::Cancelled) => {
                if let Some(operation) = self.markdown_invocation.take() {
                    self.markdown_results
                        .push(tilo_actions::InvocationUpdate::Cancelled {
                            invocation: operation.invocation,
                            message: None,
                        });
                }
            }
            (_, ui::portal::PortalOutcome::Failed(error)) => {
                self.transfer_toasts.push(
                    format!("File chooser unavailable: {error}"),
                    ui::widget::toast::ToastKind::Error,
                );
                self.complete_markdown_operation(Err(error));
            }
        }
    }
    fn finish_pending_markdown_import(
        &mut self,
        doc: &mut AutoCommit,
        fallback: Option<&ResourceInvocationScope>,
    ) -> Result<(), String> {
        let pending = self
            .pending_markdown_source
            .take()
            .ok_or("No local file import is pending")?;
        self.import_control_emitted = false;
        let scope = self.markdown_scope().or_else(|| fallback.cloned());
        self.finish_markdown_import(
            doc,
            &pending.path,
            &pending.body,
            pending.assets,
            scope.as_ref(),
        )
    }

    fn finish_markdown_import(
        &mut self,
        doc: &mut AutoCommit,
        path: &std::path::Path,
        body: &str,
        prepared_assets: Option<ImportedAssets>,
        scope: Option<&ResourceInvocationScope>,
    ) -> Result<(), String> {
        if self.legacy_read_blocked.is_some() {
            return Err(
                "The current note could not be read; its unsaved draft is still open".into(),
            );
        }
        let effect = scope
            .map(ResourceInvocationScope::effect_scope)
            .transpose()?;
        let (body, assets) = if let Some(assets) = prepared_assets {
            (body.to_string(), assets)
        } else if let Some(legacy) = self.legacy_fixture_storage.as_ref() {
            (
                adopt_portable_markdown_assets(&legacy.root, path, body, effect.as_ref())?,
                Vec::new(),
            )
        } else {
            adopt_canonical_markdown_assets(path, body, effect.as_ref())?
        };
        let mut workspace = if self.legacy_fixture_storage.is_some() {
            let mut workspace = self.workspace.clone();
            workspace.data.pages = NotesDoc::list_pages(doc);
            workspace
        } else {
            Self::project_workspace(doc, self.workspace.selected())?
        };
        let page = workspace.create_page(None);
        let title = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or("Imported note");
        workspace.rename(page, title);
        workspace.set_selected(Some(page));
        let meta = workspace.get(page).ok_or("Imported page is unavailable")?;
        let blocks = MainPanel::seed_blocks(&body, |title| {
            workspace
                .pages()
                .iter()
                .find(|page| page.title == title)
                .map(|page| page.id)
        });
        let action = NotesAction::ImportPage {
            page: actions::PageData::from_meta(meta),
            blocks: blocks.iter().map(actions::PageBlock::of).collect(),
            assets,
        };
        self.dispatch_transfer_action(doc, &action, scope)?;
        self.workspace = workspace;
        self.sidebar.sync_from_workspace(&self.workspace);
        if let Some(legacy) = &self.legacy_fixture_storage {
            legacy.page_saver.request(page, body);
            self.persist();
        }
        self.bound_page = None;
        self.self_dirty = true;
        self.transfer_toasts.push(
            format!("Imported {title}"),
            ui::widget::toast::ToastKind::Success,
        );
        Ok(())
    }

    fn finish_direct_markdown_import(
        &mut self,
        doc: &mut AutoCommit,
        title: &str,
        body: &str,
    ) -> Result<PageId, String> {
        let title = title.trim();
        let title = if title.is_empty() {
            "Imported note"
        } else {
            title
        };
        let body = crate::markdown::strip_matching_title_heading(body, title);
        let mut workspace = Self::project_workspace(doc, self.workspace.selected())?;
        let page = workspace.create_page(None);
        workspace.rename(page, title);
        workspace.set_selected(Some(page));
        let meta = workspace.get(page).ok_or("Imported page is unavailable")?;
        let blocks = MainPanel::seed_blocks(&body, |target| {
            workspace
                .pages()
                .iter()
                .find(|candidate| candidate.title == target)
                .map(|candidate| candidate.id)
        });
        let action = NotesAction::ImportPage {
            page: actions::PageData::from_meta(meta),
            blocks: blocks.iter().map(actions::PageBlock::of).collect(),
            assets: Vec::new(),
        };
        self.dispatch_canonical_action(doc, &action)?;
        self.workspace = workspace;
        self.sidebar.sync_from_workspace(&self.workspace);
        self.bound_page = None;
        self.self_dirty = true;
        self.transfer_toasts.push(
            format!("Imported {title}"),
            ui::widget::toast::ToastKind::Success,
        );
        Ok(page)
    }

    fn stage_pending_import(&mut self, pending: PendingMarkdownImport) -> Result<(), String> {
        if self.pending_markdown_source.is_some() {
            return Err("Another Markdown import is pending".into());
        }
        let generation = self
            .pending_import_generation
            .checked_add(1)
            .ok_or("Markdown import generation exhausted")?;
        self.pending_import_generation = generation;
        self.pending_markdown_source = Some(pending);
        Ok(())
    }

    // Complete lazy resolution semantics are versioned alongside the actual
    // document/owner witness retained by the SDK. No labels or callback hashes.
    fn notes_invocation_domain(
        &self,
        doc: &AutoCommit,
    ) -> ui::widget::ActionDirectoryInvocationDomain {
        if self.legacy_fixture_storage.is_some() || self.legacy_read_blocked.is_some() {
            return ui::widget::ActionDirectoryInvocationDomain::Unsupported;
        }
        let mut parts = vec![
            ui::clock::local_now()
                .format("%Y-%m-%d")
                .to_string()
                .into_bytes(),
            crate::continuation::optional_page(self.workspace.selected()),
            crate::continuation::optional_page(self.active_tag_filter),
            vec![u8::from(self.adopted)],
            self.pending_import_generation.to_be_bytes().to_vec(),
            vec![u8::from(self.pending_markdown_source.is_some())],
        ];
        match &self.markdown_invocation {
            Some(job) => {
                parts.push(vec![1]);
                parts.push(job.invocation.0.as_bytes().to_vec());
            }
            None => parts.push(vec![0]),
        }
        // A different route with the same Back/Forward availability is a
        // different relative navigation domain. Deleted targets are checked
        // against the current document by the unchanged navigation handler.
        for routes in [
            self.route_history.past(),
            std::slice::from_ref(self.route_history.current()),
            self.route_history.future(),
        ] {
            parts.push((routes.len() as u64).to_be_bytes().to_vec());
            parts.extend(
                routes
                    .iter()
                    .copied()
                    .map(crate::continuation::optional_page),
            );
        }
        let heads = doc.base_heads();
        parts.push((heads.len() as u64).to_be_bytes().to_vec());
        parts.extend(heads.iter().map(|head| head.0.to_vec()));
        parts.push(
            (automerge::transaction::Transactable::pending_ops(doc) as u64)
                .to_be_bytes()
                .to_vec(),
        );
        crate::continuation::provider(
            "notes-canonical-page-block-tag-backlink-local-dispatch/v1",
            parts,
        )
    }

    fn pending_import_resource(&self) -> Option<tilo_actions::Resource> {
        if self.pending_markdown_source.is_none() || !self.adopted {
            return None;
        }
        let action = tilo_actions::ActionSpec::operation(
            "finalize",
            "Import Markdown",
            tilo_actions::OperationKind::Effect,
            actions::NOTES_OPERATION_TYPE,
            &actions::NotesOperation::FinalizeImport,
        )
        .ok()?;
        Some(
            tilo_actions::Resource::new("notes.pending-import", "notes.import", "Import Markdown")
                .with_action(action),
        )
    }

    /// Execute Notes' one typed view operation locally. Search, agents and
    /// collaboration invoke this through [`Widget::start_operation`];
    /// the private jump palette and in-document links construct the identical
    /// operation and arrive here too, so every open path validates the same
    /// durable page/block resource before changing presentation state.
    #[allow(
        clippy::too_many_lines,
        reason = "keeps the exhaustive typed Notes operation dispatch in one validation boundary"
    )]
    fn open_notes_operation(
        &mut self,
        operation: actions::NotesOperation,
        doc: &AutoCommit,
    ) -> Result<(), String> {
        match operation {
            actions::NotesOperation::NavigateBack => self
                .navigate_history(ui::NavAction::Back, doc)
                .and_then(|changed| {
                    changed
                        .then_some(())
                        .ok_or_else(|| "no previous note".into())
                }),
            actions::NotesOperation::NavigateForward => self
                .navigate_history(ui::NavAction::Forward, doc)
                .and_then(|changed| changed.then_some(()).ok_or_else(|| "no next note".into())),
            actions::NotesOperation::FinalizeImport => {
                Err("Import completion requires mutable host admission".into())
            }
            actions::NotesOperation::ImportMarkdown => self.request_markdown_import(),
            actions::NotesOperation::ImportMarkdownContent { title, body } => {
                if body.len() > usize::try_from(MAX_MARKDOWN_IMPORT_BYTES).unwrap_or(usize::MAX) {
                    return Err("Markdown content is larger than 1 MiB".into());
                }
                if crate::markdown::parse(&body).len() > MAX_MARKDOWN_IMPORT_BLOCKS {
                    return Err("Markdown content contains more than 10,000 blocks".into());
                }
                let _ = (title, body);
                Err("Markdown content import requires mutable host admission".into())
            }
            actions::NotesOperation::InsertImage { page } => {
                if NotesDoc::page_body(doc, page).is_none() {
                    return Err("page no longer exists".into());
                }
                self.request_image_insert(page)
            }
            actions::NotesOperation::ExportMarkdown { page } => {
                if NotesDoc::page_body(doc, page).is_none() {
                    return Err("page no longer exists".into());
                }
                self.request_markdown_export(doc, page)
            }
            actions::NotesOperation::CopyPageLink { page } => {
                let title = NotesDoc::list_pages(doc)
                    .into_iter()
                    .find(|meta| meta.id == page)
                    .map(|meta| meta.title)
                    .ok_or_else(|| "page no longer exists".to_string())?;
                let label = if title.trim().is_empty() {
                    "Untitled"
                } else {
                    title.trim()
                };
                if !ui::try_clipboard_copy(&format!("[[{label}]]")) {
                    return Err("clipboard is unavailable".into());
                }
                self.transfer_toasts.push(
                    format!("Copied link to {label}"),
                    ui::widget::toast::ToastKind::Success,
                );
                Ok(())
            }
            actions::NotesOperation::OpenExternalLink { url } => {
                if !crate::markdown::safe_external_url(&url) {
                    return Err("external link target is invalid".into());
                }
                self.open_external_link(url);
                Ok(())
            }
            actions::NotesOperation::OpenPage { page, block_id } => {
                if NotesDoc::page_body(doc, page).is_none() {
                    return Err("page no longer exists".into());
                }
                if let Some(block_id) = block_id.as_ref()
                    && !NotesDoc::page_block_snapshots(doc, page)
                        .iter()
                        .any(|(id, _)| id == block_id)
                {
                    return Err("block no longer exists".into());
                }
                self.try_navigate_to(page)?;
                if let Some(block_id) = block_id {
                    self.pending_block_reveal = Some((page, block_id));
                }
                Ok(())
            }
            actions::NotesOperation::OpenTag { tag } => {
                if !NotesDoc::list_tags(doc)
                    .iter()
                    .any(|candidate| candidate.id == tag)
                {
                    return Err("tag no longer exists".into());
                }
                self.active_tag_filter = Some(tag);
                // A tag is a navigable collection. Preserve the current note
                // when it already belongs to the collection; otherwise land
                // on a deterministic first tagged note (page UUID order).
                let pages = if self.legacy_fixture_storage.is_none() {
                    crate::notebook::project(doc, self.workspace.selected())?.pages_for_tag(tag)
                } else {
                    self.workspace.pages_for_tag(tag)
                };
                if let Some(first) = pages.first().copied()
                    && !self
                        .workspace
                        .selected()
                        .is_some_and(|page| pages.contains(&page))
                {
                    self.navigate_to(first);
                }
                self.self_dirty = true;
                Ok(())
            }
        }
    }

    /// If the editor has pending edits for the current page, snapshot
    /// the body and request a save before swapping pages.
    fn flush_main_panel_edit(&mut self) {
        if self.legacy_fixture_storage.is_none() || self.legacy_read_blocked.is_some() {
            return;
        }
        if let (Some(id), true) = (self.main.page_id(), self.main.editor_is_dirty()) {
            let body = self.main.take_body_snapshot();
            if let Some(legacy) = &self.legacy_fixture_storage {
                let saver = &legacy.page_saver;
                saver.request(id, body);
                // Force the worker to flush immediately so the next
                // load_page (for a different page) doesn't race with
                // the still-queued save of this page.
                saver.flush();
            }
        }
    }

    /// Poll-style: called once per tick after a `handle_event` to
    /// detect editor edits and request a debounced save. Triggers a
    /// workspace mtime bump so the sidebar can highlight "modified"
    /// state if we ever add that affordance.
    fn drain_editor_edits(&mut self) {
        if !self.main.editor_is_dirty() {
            return;
        }
        self.save_current_editor_snapshot();
    }

    /// Serialize and queue the already-known-dirty current editor. Kept
    /// separate from [`Self::drain_editor_edits`] so the event path can first
    /// dispatch rich marks, then perform exactly one Markdown snapshot for the
    /// save worker without scanning a long note again on pointer-only events.
    fn save_current_editor_snapshot(&mut self) {
        if self.legacy_fixture_storage.is_none() || self.legacy_read_blocked.is_some() {
            return;
        }
        let Some(id) = self.main.page_id() else {
            return;
        };
        let body = self.main.take_body_snapshot();
        if let Some(legacy) = &self.legacy_fixture_storage {
            let saver = &legacy.page_saver;
            saver.request(id, body);
        }
    }

    /// Complete a first-note request from the empty-state button. This runs
    /// from both the normal event path and `sync`, because an accessibility
    /// activation reaches the child widget directly and then asks the host for
    /// a sync. Either route creates the same root page and shared action.
    fn drain_empty_note_request(&mut self, state: &mut AutoCommit) -> bool {
        if !self.main.take_empty_action() {
            return false;
        }
        self.apply_and_dispatch(state, OutEvent::CreateChild { parent: None });
        self.sidebar.sync_from_workspace(&self.workspace);
        true
    }

    /// Commit either a keyboard/pointer title edit or an accessibility value
    /// change through the one page-tree action route. Kept callable from sync
    /// because semantic actions target `MainPanel` directly and naturally
    /// request a subsequent sync rather than travelling through `handle_event`.
    /// Keep the image snapshot in step with the notebook's asset references,
    /// and pick up referenced bytes that arrive from a collaborator's device
    /// after the reference did.
    fn refresh_canonical_assets(&mut self, state: &AutoCommit) {
        let Ok(manifest) = crate::notebook::asset_manifest(state) else {
            return;
        };
        let retry = self
            .canonical_assets_waiting
            .is_some_and(|since| since.elapsed() >= std::time::Duration::from_secs(1));
        if manifest == self.canonical_asset_manifest && !retry {
            return;
        }
        let Ok(snapshot) = crate::notebook::asset_snapshot(state) else {
            return;
        };
        let waiting = !crate::notebook::assets_complete(state, &snapshot).unwrap_or(true);
        let arrived = self
            .canonical_assets
            .read()
            .is_ok_and(|assets| snapshot.keys().any(|key| !assets.contains_key(key)));
        if let Ok(mut assets) = self.canonical_assets.write() {
            *assets = snapshot;
        }
        self.canonical_asset_manifest = manifest;
        self.canonical_assets_waiting = waiting.then(ui::time::now);
        if arrived && retry {
            // The image store remembers a failed load; a fresh store draws the
            // images that were placeholders while their bytes were on the way.
            self.main.set_image_store(Some(canonical_image_store(
                &self.canonical_assets,
                self.workspace_asset_root.as_deref(),
            )));
            self.self_dirty = true;
        }
    }

    /// Notebooks written before images moved into the content store carry
    /// them inline. Once this process has a content store, move them out
    /// through the ordinary action path so the change is attributed and
    /// syncs like any other; the bytes stay in the notebook until the store
    /// has them.
    fn offer_inline_asset_move(&mut self, state: &mut AutoCommit) {
        if self.inline_assets_offered || !crate::notebook::content().shared() {
            return;
        }
        self.inline_assets_offered = true;
        let digests = match crate::notebook::inline_asset_digests(state) {
            Ok(digests) if !digests.is_empty() => digests,
            _ => return,
        };
        let action = NotesAction::MoveAssetsToContentStore { digests };
        if let Err(error) = self.actions.try_dispatch_with_context(
            state,
            actions::NOTES_ACTION_TYPE,
            &action,
            actions::history_context(&action),
        ) {
            tracing::warn!(%error, "notes: inline images stay in the notebook for now");
        }
    }

    fn drain_title_rename(&mut self, state: &mut AutoCommit) {
        let Some(new_title) = self.main.take_title_rename() else {
            return;
        };
        let Some(id) = self.workspace.selected() else {
            return;
        };
        let trimmed = new_title.trim();
        let changed = self
            .workspace
            .get(id)
            .is_some_and(|meta| meta.title != trimmed);
        if !trimmed.is_empty() && changed {
            self.apply_and_dispatch(
                state,
                OutEvent::Rename {
                    id,
                    new_title: trimmed.to_string(),
                },
            );
            self.sidebar.sync_from_workspace(&self.workspace);
        }
    }

    /// Route a note-header tag activation through the durable resource
    /// operation. It is intentionally a view operation (each collaborator may
    /// browse a different tag) but still validates the keyed tag record in the
    /// canonical document before selecting a collection.
    fn drain_tag_open(&mut self, state: &AutoCommit) {
        let Some(tag) = self.main.take_tag_open() else {
            return;
        };
        if let Err(error) =
            self.open_notes_operation(actions::NotesOperation::OpenTag { tag }, state)
        {
            tracing::warn!(%error, %tag, "notes: tag chip target went stale");
        }
    }

    /// Resolve an incoming-link chip through the same canonical open-page
    /// operation as global search and ordinary `[[Page]]` activation. The
    /// relationship row itself remains a local, derived presentation.
    fn drain_backlink_open(&mut self, state: &AutoCommit) {
        let Some(page) = self.main.take_backlink_open() else {
            return;
        };
        if let Err(error) = self.open_notes_operation(
            actions::NotesOperation::OpenPage {
                page,
                block_id: None,
            },
            state,
        ) {
            tracing::warn!(%error, %page, "notes: backlink source went stale");
        }
    }

    /// Schedule a debounced save of the current workspace.
    fn persist(&self) {
        if let Some(legacy) = &self.legacy_fixture_storage {
            legacy.saver.request(self.workspace.clone());
        }
    }

    /// A press the page tree claimed focus for gives it keyboard focus at
    /// once, so a key that follows in the same frame reaches its commands.
    fn claim_sidebar_focus(&mut self, focus_target: Option<NodeId>) {
        if focus_target.is_some() && focus_target == self.sidebar_node {
            self.sidebar.set_focus(true);
        }
    }

    /// Drain the sidebar's outbound event queue and apply each, dispatching the
    /// matching page-tree action against the shared document. Called after every
    /// `handle_event` call.
    fn drain_sidebar_events(&mut self, doc: &mut AutoCommit) {
        if let Some(transfer) = self.sidebar.take_transfer() {
            let result = match transfer {
                SidebarTransfer::ImportMarkdown => self.request_markdown_import(),
                SidebarTransfer::CopyLink(page) => {
                    self.open_notes_operation(actions::NotesOperation::CopyPageLink { page }, doc)
                }
                SidebarTransfer::ExportMarkdown(page) => self.request_markdown_export(doc, page),
                SidebarTransfer::InsertImage(page) => self.request_image_insert(page),
            };
            if let Err(error) = result {
                tracing::warn!(%error, "notes: could not start Markdown transfer");
            }
        }
        let events = self.sidebar.take_events();
        if events.is_empty() {
            return;
        }
        for ev in events {
            self.apply_and_dispatch(doc, ev);
        }
        // After mutation, push the new workspace snapshot back into
        // the sidebar so the tree shape reflects.
        self.sidebar.sync_from_workspace(&self.workspace);
    }

    /// Force-flush the background saver. Called from main on
    /// shutdown so the user's last edits never get lost in the
    /// debounce window.
    pub fn flush_saves(&mut self) {
        // Drain any pending body before we ask the savers to flush.
        self.flush_main_panel_edit();
        if let Some(legacy) = &self.legacy_fixture_storage {
            legacy.saver.flush();
        }
        if let Some(legacy) = &self.legacy_fixture_storage {
            legacy.page_saver.flush();
        }
    }

    /// Test accessor: snapshot of the current page list. Kept out of
    /// the public surface so external callers can't poke at internal
    /// model state — `cfg(any)` since both the integration tests in
    /// `tests/` and the unit tests need it.
    #[doc(hidden)]
    pub fn workspace_pages_for_test(&self) -> Vec<crate::model::PageMeta> {
        self.workspace.pages().to_vec()
    }

    /// Test hook: mutable access to the main panel. Lets integration
    /// tests drive the editor programmatically without dispatching
    /// fake winit events.
    #[doc(hidden)]
    pub fn main_panel_mut(&mut self) -> &mut MainPanel {
        &mut self.main
    }

    /// Test hook: centre of the in-page title band, for click targeting.
    #[doc(hidden)]
    pub fn main_title_center_for_test(&self) -> Point {
        self.main.title_rect_for_test().center()
    }

    /// Test hook: whether the in-page title is currently being edited.
    #[doc(hidden)]
    pub fn main_title_editing_for_test(&self) -> bool {
        self.main.title_is_editing_for_test()
    }

    /// Test hook: run one tick's worth of the edit-drain logic so
    /// tests don't have to actually go through `handle_event`. After
    /// this returns, any dirty edit is queued on the page saver.
    #[doc(hidden)]
    pub fn tick_for_test(&mut self) {
        self.drain_editor_edits();
        self.refresh_status();
    }

    /// Test hook: read-only access to the status bar so integration
    /// tests can assert on save state / page count / word count
    /// without going through pixel-level inspection.
    #[doc(hidden)]
    pub fn status_bar_for_test(&self) -> &crate::status_bar::StatusModel {
        &self.status
    }

    /// Test hook: re-run the real `refresh_status` merge WITHOUT
    /// draining editor edits (so polling for an async saver result
    /// doesn't re-queue the save and starve its debounce).
    #[doc(hidden)]
    pub fn refresh_status_for_test(&mut self) {
        self.refresh_status();
    }

    /// Test hook: apply one [`OutEvent`] through the same code path
    /// the sidebar would drive it. Lets integration tests simulate
    /// "user clicked Create Child in the sidebar" without forging
    /// fake winit events.
    #[doc(hidden)]
    pub fn apply_event_for_test(&mut self, ev: OutEvent) {
        self.apply_event(ev);
    }

    /// Test hook: apply one [`OutEvent`] and dispatch its page-tree action against
    /// `doc`, as the live sidebar-event path does.
    #[doc(hidden)]
    pub fn dispatch_event_for_test(&mut self, doc: &mut AutoCommit, ev: OutEvent) {
        self.apply_and_dispatch(doc, ev);
    }

    /// Test hook: the last action an affordance dispatched through
    /// [`Self::perform_action`], so a test can compare it to a palette spec.
    #[doc(hidden)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub fn last_dispatched_for_test(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Test hook: whether the page tree is a drawer over the note rather
    /// than a docked column.
    #[doc(hidden)]
    pub fn built_narrow_for_test(&self) -> bool {
        self.shell.geometry().sidebar == SidebarPlacement::Drawer
    }

    /// Test hook: whether the page tree shows (a docked column, or an open
    /// drawer).
    #[doc(hidden)]
    pub fn drawer_open_for_test(&self) -> bool {
        self.shell.is_sidebar_open()
    }

    /// Test hook: centre of the shell's sidebar toggle.
    #[doc(hidden)]
    pub fn menu_btn_center_for_test(&self) -> Point {
        self.shell.geometry().toggle.center()
    }

    /// Test hook: centre of sidebar row `idx`, for click targeting.
    #[doc(hidden)]
    pub fn sidebar_row_center_for_test(&self, idx: usize) -> Option<Point> {
        self.sidebar.row_center_for_test(idx)
    }

    /// Test hook: x of the disclosure triangle for sidebar row `idx`.
    #[doc(hidden)]
    pub fn sidebar_disclosure_x_for_test(&self, idx: usize) -> Option<f64> {
        self.sidebar.row_disclosure_x_for_test(idx)
    }

    /// Test hook: the tree depth of visible sidebar row `idx`.
    #[doc(hidden)]
    pub fn sidebar_row_depth_for_test(&self, idx: usize) -> Option<usize> {
        self.sidebar.row_depth_for_test(idx)
    }

    /// Test hook: the title shown on sidebar row `idx`.
    #[doc(hidden)]
    pub fn sidebar_row_title_for_test(&self, idx: usize) -> Option<&str> {
        self.sidebar.row_title_for_test(idx)
    }

    /// Test hook: whether visible sidebar row `idx` has children.
    #[doc(hidden)]
    pub fn sidebar_row_has_children_for_test(&self, idx: usize) -> Option<bool> {
        self.sidebar.row_has_children_for_test(idx)
    }

    /// Test hook: whether the active inline rename field is plain-chrome
    /// (styled like the row, not a boxed form field).
    #[doc(hidden)]
    pub fn sidebar_rename_is_plain_for_test(&self) -> Option<bool> {
        self.sidebar.rename_is_plain_for_test()
    }

    /// Test hook: centre of the sidebar header's "New note" button, for
    /// click targeting through the real event path.
    #[doc(hidden)]
    pub fn sidebar_new_page_btn_center_for_test(&self) -> Point {
        self.header_button_rect(NEW_NOTE_BUTTON_ID)
            .map_or(Point::ZERO, |rect| rect.center())
    }

    /// Test hook: the sidebar column's rect (docked column or overlay
    /// drawer), so a test can prove the drawer is clamped to the window.
    #[doc(hidden)]
    pub fn sidebar_rect_for_test(&self) -> Rect {
        self.shell.geometry().sidebar_column
    }

    /// The laid-out rect of the shell's control with test id `id`.
    fn header_button_rect(&self, id: &str) -> Option<Rect> {
        fn find(widget: &dyn Widget, id: &str) -> Option<Rect> {
            if widget.test_id() == Some(id) {
                return Some(widget.get_rect());
            }
            let mut found = None;
            widget.for_each_child(&mut |child| {
                if found.is_none() {
                    found = find(child, id);
                }
            });
            found
        }
        find(&self.shell, id)
    }

    /// Test hook: whether a live page-tree drag is in flight.
    #[doc(hidden)]
    pub fn sidebar_drag_active_for_test(&self) -> bool {
        self.sidebar.drag_active_for_test()
    }

    /// Test hook: the drop indicator the page tree paints for the drag in
    /// flight — an insertion line, or the outline of the page it would nest
    /// into.
    #[doc(hidden)]
    pub fn sidebar_drop_indicator_for_test(&self) -> Option<Rect> {
        self.sidebar.drop_indicator_for_test()
    }

    /// Whether a menu on the open note currently captures input.
    #[doc(hidden)]
    pub fn note_menu_open_for_test(&self) -> bool {
        self.note_menu.is_open()
    }

    /// Test hook: the open note menu's laid-out rect, and the rect of the
    /// button that opens it.
    #[doc(hidden)]
    pub fn note_menu_rects_for_test(&self) -> Option<(Rect, Rect)> {
        let button = self.header_button_rect(NOTE_MENU_BUTTON_ID)?;
        let first = self.note_menu.item_rect(0)?;
        let last = self
            .note_menu
            .items()
            .len()
            .checked_sub(1)
            .and_then(|index| self.note_menu.item_rect(index))?;
        Some((first.union(last), button))
    }

    /// Test hook: centre of the note header's "Note actions" button.
    #[doc(hidden)]
    pub fn note_menu_button_center_for_test(&self) -> Point {
        self.header_button_rect(NOTE_MENU_BUTTON_ID)
            .map_or(Point::ZERO, |rect| rect.center())
    }

    /// Test hook: whether the sidebar's right-click context menu is open.
    #[doc(hidden)]
    pub fn sidebar_menu_open_for_test(&self) -> bool {
        self.sidebar.context_menu_open_for_test()
    }

    /// Test hook: whether the editor's `/` block menu is visible.
    #[doc(hidden)]
    pub fn editor_slash_menu_visible_for_test(&self) -> bool {
        self.main.editor().slash_menu_visible()
    }

    /// Test hook: drop the editor's keyboard focus so global single-key
    /// shortcuts (like `?`) fire instead of typing into a note. Mirrors
    /// the state when the page tree — not the editor — is focused.
    #[doc(hidden)]
    pub fn blur_editor_for_test(&mut self) {
        self.main.editor_mut().set_focus(false);
    }

    /// Test hook: whether the shortcuts overlay is currently visible.
    #[doc(hidden)]
    pub fn shortcuts_visible_for_test(&self) -> bool {
        self.shortcuts.is_visible()
    }

    /// Test hook: whether the sidebar currently has an inline rename
    /// editor open (e.g. after F2 / double-click).
    #[doc(hidden)]
    pub fn sidebar_is_renaming_for_test(&self) -> bool {
        self.sidebar.is_renaming()
    }

    /// Test hook: mirror the workspace into the sidebar's row list, the
    /// same refresh `drain_sidebar_events` does after applying an event.
    #[doc(hidden)]
    pub fn sync_sidebar_for_test(&mut self) {
        self.sidebar.sync_from_workspace(&self.workspace);
    }

    /// Snapshot hook: open the page tree so a headless shot can show the
    /// drawer. Where the tree is docked it is already open.
    #[doc(hidden)]
    pub fn open_drawer_for_shots(&mut self) {
        self.set_sidebar_open(true);
    }
}

/// First-launch seed: a single starter note the user can edit or
/// delete. It doubles as a quick reference for the editing syntax, so
/// it deliberately uses each block kind at least once. The body must
/// round-trip through `notes::markdown::{parse,serialize}` byte-stably
/// — `welcome_page_renders_all_block_kinds` pins that.
pub const WELCOME_BODY: &str = "\
# Welcome

This is your first note. Edit it, or add a note from the left.

Type here. **Bold**, *italic*, `code`, and [[Welcome]] links work inline.

## Formatting

Start a line with `#` for a heading, or `- ` for a bullet:

- Coffee

- Tea

- Hot chocolate

  - With marshmallows

Add `[ ]` after the dash to track to-dos:

- [ ] Something to finish

- [x] Something already done

Triple backticks make a code block:

```rust
fn hello() {}
```

Start with `> ` to call out a quotation:

> Notes should feel calm, fast, and connected.

Or number them when order matters:

1. First, add a note.

2. Then write something.

3. Finally, link it with `[[`.

Three dashes make a divider:

---

## Shortcuts

- `Cmd-K` jumps to any note by name

- `Cmd-N` adds a note under the selection

- `/` opens the block menu; `[[` links a note

- `?` opens the full shortcuts list

Double-click a title in the sidebar to rename it.
";

/// Combine the workspace saver's status with the page-body saver's into
/// the single indicator the status bar renders. Worst-of wins so the
/// most actionable signal surfaces: any `Failed` shows "Save failed";
/// otherwise any `Saving` shows "Saving…"; only when both are clean do
/// we report `Saved`. Free function so the merge is unit-testable
/// without spinning up the background saver threads.
fn combine_save_status(workspace: SaveStatus, page: SaveStatus) -> SaveStatus {
    match (workspace, page) {
        (SaveStatus::Failed, _) | (_, SaveStatus::Failed) => SaveStatus::Failed,
        (SaveStatus::Saving, _) | (_, SaveStatus::Saving) => SaveStatus::Saving,
        _ => SaveStatus::Saved,
    }
}

fn seed_welcome(root: &std::path::Path, workspace: &mut Workspace) {
    let id = workspace.create_page(None);
    workspace.rename(id, "Welcome");
    workspace.set_selected(Some(id));
    if let Err(e) = storage::write_page_body(root, id, WELCOME_BODY) {
        tracing::error!(error = %e, "notes: write Welcome page failed");
    }
}

impl ui::widget::AppWidget for NotesApp {
    fn capture_application_view(
        &self,
    ) -> Result<Option<ui::application_view::ApplicationLocalView>, ui::view::ThawError> {
        use crate::local_view::{BlockPosition, NotesFocus, NotesLocalViewV1};
        let editor = self.main.editor();
        // Only blocks the note stores have a stable position; a placeholder
        // in an empty note does not.
        let position = |index: usize, byte_offset| {
            editor
                .blocks()
                .get(index)
                .filter(|block| self.main.stores_block(&block.id))
                .map(|block| BlockPosition {
                    block_id: block.id.clone(),
                    byte_offset,
                })
        };
        let (anchor, caret) = if let Some(selection) = editor.document_selection() {
            (
                position(selection.anchor.block, selection.anchor.byte),
                position(selection.cursor.block, selection.cursor.byte),
            )
        } else if let Some(index) = editor.focused_index() {
            let block = editor.blocks().get(index);
            let caret = block.and_then(|block| position(index, block.line.cursor_pos()));
            let anchor = block.and_then(|block| {
                block.line.selection_range().and_then(|(start, end)| {
                    position(
                        index,
                        if block.line.cursor_pos() == start {
                            end
                        } else {
                            start
                        },
                    )
                })
            });
            (anchor, caret)
        } else {
            (None, None)
        };
        let focus = if self
            .pending_responsive_view
            .is_some_and(|(_, sidebar)| sidebar)
            || self.sidebar.is_focused()
        {
            NotesFocus::Sidebar
        } else if self.main.title_focused() {
            NotesFocus::Title
        } else if editor.is_focused() && self.main.page_id().is_some() {
            NotesFocus::Editor
        } else {
            NotesFocus::None
        };
        let local = NotesLocalViewV1 {
            selected_page: self.workspace.selected(),
            active_tag: self.active_tag_filter,
            drawer_open: self
                .pending_responsive_view
                .map_or_else(|| self.shell.is_sidebar_open(), |(open, _)| open),
            expanded_pages: self.sidebar.expanded_pages(),
            editor_scroll: self.main.local_scroll(),
            focus,
            anchor,
            caret,
        };
        local.encode().map(Some)
    }

    fn restore_application_view(
        &mut self,
        local: &ui::application_view::ApplicationLocalView,
        doc: &AutoCommit,
    ) -> Result<ui::application_view::ApplicationFocusRestore, ui::view::ThawError> {
        use crate::local_view::{NotesLocalViewV1, PreparedFocus};
        let local = NotesLocalViewV1::decode(local)?;
        let workspace = Self::project_workspace(doc, local.selected_page)
            .map_err(ui::view::ThawError::State)?;
        let mut main = self
            .main
            .project_canonical(doc, &workspace)
            .map_err(ui::view::ThawError::State)?;
        let stored = workspace
            .selected()
            .map(|page| {
                NotesDoc::page_block_snapshots(doc, page)
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect::<std::collections::HashSet<_>>()
            })
            .unwrap_or_default();
        let focus = PreparedFocus::resolve(&local, main.editor(), &|id| stored.contains(id));
        main.restore_scroll(local.editor_scroll);
        let mut sidebar = PageSidebar::new();
        sidebar.sync_from_workspace(&workspace);
        sidebar.restore_expanded_pages(&local.expanded_pages, &workspace);
        let active_tag = local
            .active_tag
            .filter(|id| workspace.tags().iter().any(|tag| tag.id == *id));
        // All validation/projection above precedes the first saved local change.
        self.main = main;
        self.workspace = workspace;
        self.sidebar = sidebar;
        self.active_tag_filter = active_tag;
        self.bound_page = self.workspace.selected();
        self.pending_events.clear();
        self.pending_block_reveal = None;
        self.pending_responsive_view = Some((local.drawer_open, false));
        self.prepared_local_focus = Some(focus);
        self.refresh_link_pages();
        self.refresh_status();
        self.self_dirty = true;
        Ok(ui::application_view::ApplicationFocusRestore::Application)
    }

    fn finish_application_view_restore(&mut self) -> Result<(), ui::view::ThawError> {
        use crate::local_view::NotesFocus;
        let Some(plan) = self.prepared_local_focus.take() else {
            return Ok(());
        };
        self.sidebar.set_focus(false);
        // Restore editor presentation even while another local control holds focus.
        if let Some(caret) = plan.caret {
            if let Some(anchor) = plan.anchor {
                self.main.editor_mut().select_document_range(anchor, caret);
            } else {
                self.main
                    .editor_mut()
                    .focus_block(caret.block, ui::widget::CursorPlacement::Byte(caret.byte));
            }
        }
        self.main.finish_local_focus(plan.focus);
        if plan.focus == NotesFocus::Sidebar {
            if self.rect.width() > 0.0 {
                self.sidebar.set_focus(self.shell.is_sidebar_open());
            } else if let Some((_, sidebar)) = self.pending_responsive_view.as_mut() {
                *sidebar = true;
                self.sidebar.set_focus(true);
            }
        }
        Ok(())
    }

    fn app_kind(&self) -> ui::widget::AppKind {
        ui::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Notes"
    }
    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Named("notes".into())
    }
    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        None
    }
    fn record_save_status(&mut self, status: ui::record::RecordSaveStatus) {
        self.record_save_status = status;
        self.refresh_status();
    }
}

impl NotesApp {
    /// A local operation keeps the order the retained host gave it before
    /// the single entry: document writes, then file transfers and links,
    /// then the children, then this app's own view of the notebook.
    fn start_local_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        let kind = request.kind;
        let operation = actions::decode_operation(request.type_id, request.payload)?;
        let effect = kind == tilo_actions::OperationKind::Effect;
        let started = match operation {
            Some(actions::NotesOperation::FinalizeImport) => {
                if !effect {
                    return Err("Import completion requires an Effect invocation".into());
                }
                self.finish_pending_markdown_import(doc, None)?;
                ui::widget::ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
            }
            Some(actions::NotesOperation::ImportMarkdownContent { title, body }) => {
                if !effect {
                    return Err("Markdown content import requires an Effect invocation".into());
                }
                if body.len() > usize::try_from(MAX_MARKDOWN_IMPORT_BYTES).unwrap_or(usize::MAX) {
                    return Err("Markdown content is larger than 1 MiB".into());
                }
                if crate::markdown::parse(&body).len() > MAX_MARKDOWN_IMPORT_BLOCKS {
                    return Err("Markdown content contains more than 10,000 blocks".into());
                }
                let page = self.finish_direct_markdown_import(doc, &title, &body)?;
                let mut output = tilo_actions::ActionOutput::default();
                output.resources.push(actions::page_resource_id(page));
                ui::widget::ResourceOperationStart::Completed(output)
            }
            Some(
                operation @ (actions::NotesOperation::ImportMarkdown
                | actions::NotesOperation::ExportMarkdown { .. }
                | actions::NotesOperation::InsertImage { .. }),
            ) => {
                if !effect {
                    return Err("File transfers require an Effect invocation".into());
                }
                self.open_notes_operation(operation, doc)?;
                self.markdown_invocation = Some(PendingOperation::local(request.invocation));
                ui::widget::ResourceOperationStart::Accepted
            }
            Some(actions::NotesOperation::OpenExternalLink { url }) => {
                if !effect {
                    return Err("Opening a link requires an Effect invocation".into());
                }
                self.start_external_link(url, Some(PendingOperation::local(request.invocation)))?;
                ui::widget::ResourceOperationStart::Accepted
            }
            operation => {
                if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
                    return Ok(Some(started));
                }
                let Some(operation) = operation else {
                    return Ok(None);
                };
                if kind == tilo_actions::OperationKind::Document {
                    return Ok(None);
                }
                self.open_notes_operation(operation, doc)?;
                ui::widget::ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
            }
        };
        Ok(Some(started))
    }
}

impl Widget for NotesApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::component_lifecycle::ComponentLifecycle
    );

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let (request, Some(scope)) = op.with_custody() else {
            return self.start_local_operation(op, doc, path);
        };
        scope.check()?;
        let (kind, type_id, payload) = (request.kind, request.type_id, request.payload);
        if self.legacy_fixture_storage.is_some() {
            return Err("Scoped operations require the canonical Notes record owner".into());
        }
        let Some(operation) = actions::decode_operation(type_id, payload)? else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if matches!(
            operation,
            actions::NotesOperation::ImportMarkdown
                | actions::NotesOperation::ExportMarkdown { .. }
                | actions::NotesOperation::InsertImage { .. }
                | actions::NotesOperation::OpenExternalLink { .. }
                | actions::NotesOperation::ImportMarkdownContent { .. }
                | actions::NotesOperation::FinalizeImport
        ) && kind != tilo_actions::OperationKind::Effect
        {
            return Err("This operation requires an Effect invocation".into());
        }
        let started = match operation {
            actions::NotesOperation::ImportMarkdown => {
                let pending = PendingOperation::scoped(scope);
                self.request_markdown_import_scoped(Some(scope.effect_scope()?))?;
                self.markdown_invocation = Some(pending);
                ui::widget::ResourceOperationStart::Accepted
            }
            actions::NotesOperation::ExportMarkdown { page } => {
                if NotesDoc::page_body(doc, page).is_none() {
                    return Err("page no longer exists".into());
                }
                let pending = PendingOperation::scoped(scope);
                self.request_markdown_export_scoped(doc, page, Some(scope.effect_scope()?))?;
                self.markdown_invocation = Some(pending);
                ui::widget::ResourceOperationStart::Accepted
            }
            actions::NotesOperation::InsertImage { page } => {
                if NotesDoc::page_body(doc, page).is_none() {
                    return Err("page no longer exists".into());
                }
                let pending = PendingOperation::scoped(scope);
                self.request_image_insert_scoped(page, Some(scope.effect_scope()?))?;
                self.markdown_invocation = Some(pending);
                ui::widget::ResourceOperationStart::Accepted
            }
            actions::NotesOperation::OpenExternalLink { url } => {
                self.start_external_link(url, Some(PendingOperation::scoped(scope)))?;
                ui::widget::ResourceOperationStart::Accepted
            }
            actions::NotesOperation::FinalizeImport => {
                self.finish_pending_markdown_import(doc, Some(scope))?;
                ui::widget::ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
            }
            actions::NotesOperation::ImportMarkdownContent { title, body } => {
                if body.len() > usize::try_from(MAX_MARKDOWN_IMPORT_BYTES).unwrap_or(usize::MAX) {
                    return Err("Markdown content is larger than 1 MiB".into());
                }
                if crate::markdown::parse(&body).len() > MAX_MARKDOWN_IMPORT_BLOCKS {
                    return Err("Markdown content contains more than 10,000 blocks".into());
                }
                let page = scope.with_document_mutation(doc, |doc| {
                    self.finish_direct_markdown_import(doc, &title, &body)
                })?;
                let mut output = tilo_actions::ActionOutput::default();
                output.resources.push(actions::page_resource_id(page));
                ui::widget::ResourceOperationStart::Completed(output)
            }
            operation => {
                scope.with_effect(|| self.open_notes_operation(operation, doc))?;
                ui::widget::ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
            }
        };
        Ok(Some(started))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        // These portal operations are declared non-cancellable. User dismissal
        // still reports Cancelled through the portal's terminal outcome.
        if let Some(index) = self
            .markdown_results
            .iter()
            .position(|update| update.invocation() == invocation)
        {
            return Some(self.markdown_results.remove(index));
        }
        self.children_mut().into_iter().find_map(|child| {
            child.poll_resource_operation(invocation, cancellation_requested, doc, path)
        })
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.main.primary_vertical_scroll()
    }
    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        self.main.apply_primary_vertical_scroll(action)
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::NOTES_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        self.refresh_canonical_assets(state);
        if self.legacy_fixture_storage.is_none() {
            self.offer_inline_asset_move(state);
        }
        self.drain_markdown_transfer(state);
        self.drain_external_link_opens();
        if self.legacy_fixture_storage.is_none() {
            self.drain_title_rename(state);
            for event in std::mem::take(&mut self.pending_events) {
                self.apply_and_dispatch(state, event);
            }
            self.drain_header_requests(state);
            self.drain_sidebar_events(state);
            self.drain_empty_note_request(state);
            // Project the workspace only when its inputs moved: committed
            // heads, an open transaction (the drains above may have written),
            // the selected page or the tag filter. Re-projecting every frame
            // re-listed every page and tag on each pointer move.
            let heads = state.base_heads();
            let pending = state.pending_ops();
            let key = (
                heads,
                pending,
                self.workspace.selected(),
                self.active_tag_filter,
            );
            let unchanged = self.canonical_synced.as_ref().is_some_and(|synced| {
                synced.0 == key.0
                    && synced.1 == 0
                    && key.1 == 0
                    && synced.2 == key.2
                    && synced.3 == key.3
            });
            if !unchanged {
                if let Err(error) = self.reconcile_canonical(state) {
                    tracing::warn!(%error, "notes: canonical projection refused");
                    return;
                }
                self.canonical_synced = Some(key);
                self.self_dirty = true;
            }
        } else {
            self.drain_empty_note_request(state);
            // Bridge the in-memory workspace to the shared CRDT document:
            // seed/adopt the page tree, then bind the editor to the selected
            // page so its text syncs between instances.
            self.sync_workspace_tree(state);
            self.sync_tags(state);
            self.drain_title_rename(state);
        }
        self.record_current_route();
        self.ensure_editor_bound(state);
        if self.legacy_read_blocked.is_some() {
            return;
        }
        self.drain_tag_open(state);
        self.drain_backlink_open(state);
        self.main.set_tags(self.selected_tag_chips());
        self.refresh_transclusion_previews(state);
        self.sidebar.sync(state, path);
        self.main.sync(state, path);
        self.shell.sync(state, path);
        self.drain_transclusion_opens(state);
        self.refresh_search_blocks(state);
        self.main.set_backlinks(self.selected_backlink_chips());
        if let Some((page, block_id)) = self.pending_block_reveal.as_ref()
            && self.bound_page == Some(*page)
            && let Some(index) = self
                .search_blocks
                .get(page)
                .and_then(|blocks| blocks.iter().position(|block| block.id == *block_id))
            && self.main.reveal_block(index)
        {
            self.pending_block_reveal = None;
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Forward the main panel's editor structure-dirty flag so the App
        // rebuilds the taffy tree when the block count changes (local
        // split/merge OR a remote peer adding a block via sync). Without
        // this the new block gets no taffy node and isn't laid out. The
        // shell's flag too: a drawer opening or the window crossing the
        // breakpoint changes which columns exist.
        let editor = self.main.take_structure_dirty();
        let shell = self.shell.take_structure_dirty();
        editor || shell
    }

    fn needs_idle_sync(&self) -> bool {
        // Until we've seeded or adopted the shared document, keep syncing
        // on idle frames: seeding happens in `sync` after a grace window,
        // and a first peer with no input/events would otherwise never run
        // `sync` again to perform it (leaving its doc empty, so a joiner
        // gets nothing and seeds a duplicate).
        !self.adopted || self.shell.needs_idle_sync()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        self.columns.sidebar.reset();
        self.columns.content.reset();
        let shell_node = self.shell.build_layout(taffy, theme, node_map);
        self.shell_node = Some(shell_node);

        // The page tree and the note panel hang under the slots the shell
        // placed for their columns, so its own placement sizes them in the
        // same pass. A column the shell does not show (a closed drawer) has
        // no slot, and its widget is not part of the tree.
        self.main_node = self.columns.content.node().map(|slot| {
            let node = self.main.build_layout(taffy, theme, node_map);
            Self::fill_slot(taffy, slot, node);
            node
        });
        self.sidebar_node = self.columns.sidebar.node().map(|slot| {
            let node = self.sidebar.build_layout(taffy, theme, node_map);
            Self::fill_slot(taffy, slot, node);
            node
        });

        let root = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &[shell_node],
        );
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    // `taffy.layout(...)` only errors for a `NodeId` from a different tree,
    // which never happens here — every id it's called with is one this
    // widget (or a child it just laid out) received from its own
    // `build_layout`. `Widget::apply_layout` returns `()` across every
    // implementor, so surfacing a theoretical error here would mean
    // threading a `Result` through the whole toolkit for a branch that
    // can't be taken (see `ui::widget::TaffyTreeExt`).
    #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // The shell lays its columns, header row and toggle out first; the
        // page tree and the note panel are children of the columns' slots,
        // whose own origin is the column's.
        if let Some(shell_node) = self.shell_node {
            self.shell
                .apply_layout(taffy, shell_node, Point::new(self.rect.x0, self.rect.y0));
        }
        self.laid_out = true;
        // The first measured width resolves the saved sidebar intent once.
        if self.rect.width() > 0.0
            && let Some((open, sidebar)) = self.pending_responsive_view.take()
        {
            // Only a drawer remembers being open or closed; a docked page
            // tree stays where the window put it.
            if self.shell.geometry().sidebar == SidebarPlacement::Drawer {
                self.set_sidebar_open(open);
            }
            if sidebar {
                self.sidebar.set_focus(self.shell.is_sidebar_open());
            }
        }
        if let Some(n) = self.main_node {
            self.main
                .apply_layout(taffy, n, self.columns.content.rect().origin());
        }
        if let Some(n) = self.sidebar_node {
            self.sidebar
                .apply_layout(taffy, n, self.columns.sidebar.rect().origin());
        }
        self.note_menu.set_viewport(Some(self.rect));
        self.shortcuts.set_viewport(self.rect, &self.theme);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // No ground of our own: the window paints `surfaces.content`, and the
        // shell paints the sidebar's region.
        let drawer_open = self.shell.geometry().sidebar == SidebarPlacement::Drawer
            && self.shell.is_sidebar_open();
        let covered =
            self.shortcuts.is_visible() || self.note_menu.is_open() || self.sidebar.menu_is_open();
        let main_covered = covered || drawer_open;
        self.main.suppress_caret_paint(main_covered);
        self.main.suppress_tooltips(main_covered);
        self.sidebar.suppress_tooltips(covered);
        // The note first; the shell over it (its drawer and scrim cover the
        // note), then the page tree on the sidebar the shell painted.
        self.main
            .draw(scene, transform, text_renderer, theme, presence);
        self.shell
            .draw(scene, transform, text_renderer, theme, presence);
        if self.sidebar_node.is_some() {
            let geometry = self.shell.geometry();
            let sidebar_theme = if geometry.sidebar == SidebarPlacement::Drawer {
                theme.on_ground(ui::chrome::panel_ground(theme))
            } else {
                theme.in_region(ui::surface::Region::Sidebar)
            };
            // Unclipped: the page tree's context menu is a floating surface
            // that reaches past its column.
            self.sidebar
                .draw(scene, transform, text_renderer, &sidebar_theme, presence);
        }
        // Transient floating status bubble (bottom-right).
        self.draw_status_bubble(scene, transform, text_renderer, theme);
        let now = ui::time::now();
        self.transfer_toasts
            .tick(now.saturating_duration_since(self.last_toast_tick));
        self.last_toast_tick = now;
        // Toasts stand above the status bubble's corner.
        let gutter = f64::from(theme.tokens.spacing.s4);
        let lift = ui::status_bubble::pill_height(theme) + gutter;
        let toast_area = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            (self.rect.y1 - lift).max(self.rect.y0),
        );
        self.transfer_toasts.place_bottom_right(toast_area, gutter);
        self.transfer_toasts
            .draw(scene, transform, text_renderer, theme, presence);
        self.note_menu
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming only while the floating status bubble is lingering
        // / fading, so it hides itself without needing a stray pointer event.
        // Idle otherwise (no idle redraw spin).
        (!self.theme.reduce_motion && self.status_bubble.needs_frame(ui::time::now()))
            || self.shell.needs_animation_frame()
            || !self.transfer_toasts.is_empty()
            || (self.pending_markdown_source.is_some() && !self.import_control_emitted)
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // An open note menu owns input before shortcuts and the narrow drawer,
        // including the click that dismisses it.
        if self.note_menu.is_open() {
            self.handle_note_context_menu(event, ctx);
            return;
        }
        if self.handle_shortcuts_overlay(event, ctx) {
            return;
        }
        if self.handle_help_key(event, ctx.handled) {
            return;
        }
        if self.handle_find_shortcut(event, *ctx.modifiers, ctx.focus_target, ctx.handled) {
            return;
        }
        if self.handle_new_child_shortcut(event, *ctx.modifiers, ctx.state, ctx.handled) {
            return;
        }
        if self.handle_route_history_shortcut(event, *ctx.modifiers, ctx.state, ctx.handled) {
            return;
        }
        self.transfer_toasts.handle_event(event, ctx);
        if *ctx.handled {
            return;
        }
        if self.handle_shell(event, ctx) {
            return;
        }
        if self.handle_note_context_menu(event, ctx) {
            return;
        }
        if self.handle_link_click(event, ctx.state, ctx.handled) {
            return;
        }
        self.route_to_children(event, ctx);
    }

    fn name(&self) -> &'static str {
        "NotesApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let children = vec![
            AccessNode::new(Role::StaticText)
                .with_name(self.status.save_text())
                .with_description(self.status.right_text())
                .with_live(ui::a11y::Politeness::Polite)
                .with_hashed_key("notes.save-status"),
        ];
        Some(
            AccessNode::new(Role::Group)
                .with_name("Notes")
                .with_description("Workspace notes and note editor")
                .with_children(children),
        )
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::with_capacity(6);
        if SHELL_PRESENTS_HISTORY.get() {
            children.push(&self.shell);
        }
        if self.sidebar_shown() {
            children.push(self.sidebar.tree());
        }
        if self.note_menu.is_open() {
            children.push(&self.note_menu);
        }
        children.push(&self.main);
        children.push(&self.shortcuts);
        children.push(&self.transfer_toasts);
        children
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        let show_sidebar = self.sidebar_shown();
        let mut children: Vec<&mut dyn Widget> = Vec::with_capacity(6);
        if SHELL_PRESENTS_HISTORY.get() {
            children.push(&mut self.shell);
        }
        if show_sidebar {
            children.push(self.sidebar.tree_mut());
        }
        if self.note_menu.is_open() {
            children.push(&mut self.note_menu);
        }
        children.push(&mut self.main);
        children.push(&mut self.shortcuts);
        children.push(&mut self.transfer_toasts);
        children
    }

    fn is_focusable(&self) -> bool {
        false
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.children().into_iter().find_map(Widget::modal_capture)
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        None
    }

    fn needs_redraw(&self) -> bool {
        self.self_dirty
            || self.shell.needs_redraw()
            || self.sidebar.needs_redraw()
            || self.main.needs_redraw()
            || self.shortcuts.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.self_dirty = false;
        self.shell.mark_drawn();
        self.sidebar.mark_drawn();
        self.main.mark_drawn();
        self.shortcuts.mark_drawn();
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        // The root rect is the whole window — always paint, never cull.
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

impl ui::component_lifecycle::ComponentLifecycle for NotesApp {
    fn try_flush_owned_persistence(&mut self) -> Option<Result<(), String>> {
        self.legacy_fixture_storage.as_ref()?;
        if self.legacy_read_blocked.is_some() {
            return Some(Err(
                "Current note is unreadable; its draft has not been saved".into(),
            ));
        }
        self.flush_main_panel_edit();
        let legacy = self.legacy_fixture_storage.as_ref()?;
        let bodies = legacy.page_saver.try_flush();
        let metadata = legacy.saver.try_flush();
        Some(bodies.and(metadata))
    }
}

impl ui::presence::capability::PublishesPresence for NotesApp {
    fn presence_payload(&self) -> Option<(PresenceKey, PresencePayload)> {
        // Broadcast which page we're viewing as a Tab location so peers
        // can render "indicative mist" on the sidebar row that leads here.
        // This is per-peer (never in the shared doc), which is what lets
        // two instances view different pages.
        let sel = self.workspace.selected()?;
        Some((
            PresenceKey::Tab {
                id: sel.to_string(),
            },
            PresencePayload::Arrived,
        ))
    }
}

impl ui::search::capability::SearchSource for NotesApp {
    /// Fold the notebook into the unified Ctrl+K palette: a snapshot
    /// [`NoteSearchProvider`](crate::search_provider::NoteSearchProvider) makes
    /// every page findable there by title, alongside the actions the app
    /// exports.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
    }
}

impl ui::resources::PublishesResources for NotesApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.actions.is_hosted() {
            return self.pending_import_resource().into_iter().collect();
        }
        let mut resources = self.publish_resources(doc);
        resources.extend(self.pending_import_resource());
        resources
    }
    /// Publish the open notebook + its pages (nested by the tree) so the host's
    /// Ctrl+K action palette scopes to them. Delegates to the
    /// [`ResourceProvider`](tilo_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn action_directory_invocation_domain(
        &self,
        doc: &AutoCommit,
    ) -> ui::widget::ActionDirectoryInvocationDomain {
        self.notes_invocation_domain(doc)
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if id == "notes.pending-import" {
            return self.pending_import_resource();
        }
        // The framework root already owns the notebook door.
        if self.actions.is_hosted() && id == "notes.notebook" {
            return None;
        }
        self.publish_resource(doc, id)
    }
}

impl ui::widget::controls::ActionControls for NotesApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if !self.import_control_emitted
            && let Some(resource) = self.pending_import_resource()
            && let Some(spec) = resource.actions.first()
            && let Ok(binding) =
                tilo_actions::ActionBinding::new(&resource, spec, |()| serde_json::Map::new())
            && let Ok(intent) = binding.invoke(())
        {
            self.import_control_emitted = true;
            return Some(intent);
        }
        self.children_mut()
            .into_iter()
            .find_map(ui::widget::controls::take_control_invocation)
    }
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<ui::widget::ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        if address.resource == "notes.pending-import" && address.action == "finalize" {
            match result {
                Err(error) => {
                    self.pending_markdown_source = None;
                    self.import_control_emitted = false;
                    self.transfer_toasts.push(
                        format!("Import failed: {error}"),
                        ui::widget::toast::ToastKind::Error,
                    );
                    self.complete_markdown_operation(Err(error.to_string()));
                    self.self_dirty = true;
                }
                Ok(ui::widget::ResourceOperationStart::Completed(output)) => {
                    self.complete_markdown_operation(Ok(output.clone()));
                }
                Ok(ui::widget::ResourceOperationStart::Accepted) => {}
            }
        }
        for child in self.children_mut() {
            ui::widget::controls::on_control_invocation_result(child, address, result);
        }
    }
    /// The window hosts History, as it hosts search: its command is not one
    /// of the notebook's own actions, so the availability the host hands the
    /// notebook's controls does not list it, and the History control the
    /// shell draws would stay disabled. Offer the shell its own History
    /// command.
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        let mut allowed = allowed.clone();
        allowed.extend(
            ui::widget::controls::control_action_bindings(&self.shell)
                .into_iter()
                .filter_map(|binding| match binding.kind {
                    ui::widget::ControlBindingKind::Action(address)
                        if address.resource == HISTORY_RESOURCE =>
                    {
                        Some(address)
                    }
                    _ => None,
                }),
        );
        for child in self.children_mut() {
            ui::widget::controls::refresh_control_action_availability(child, &allowed);
        }
    }

    fn has_pending_control_invocation(&self) -> bool {
        !self.adopted
            || self.pending_markdown_source.is_some()
            || self
                .children()
                .iter()
                .any(|child| ui::widget::controls::has_pending_control_invocation(*child))
    }
}

impl ui::undo::CommittedActions for NotesApp {
    /// Hand the host the heads captured before a sidebar affordance dispatched
    /// its page-tree action, so it records one undo boundary for it (see
    /// `Self::perform_action`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        // A body edit dispatches through the page's editor; everything else
        // through the app's own host. One event does one or the other.
        let body = ui::undo::CommittedActions::take_committed_action(self.main.body_editor_mut());
        let notes = self.actions.take_committed();
        body.or(notes)
    }
    /// Typing a word in a note is one undo step.
    fn keeps_undo_step_open(&self) -> bool {
        self.main.keeps_undo_step_open()
    }
}

impl NotesApp {
    /// Whether the page tree is part of the window: the shell shows its
    /// column (a docked sidebar, an open drawer), or nothing has been laid
    /// out yet.
    fn sidebar_shown(&self) -> bool {
        self.sidebar_node.is_some() || !self.laid_out
    }

    /// Hang a column's widget under the slot the shell placed for it: the
    /// column fills the slot, whatever size the shell gave it.
    fn fill_slot(taffy: &mut TaffyTree<()>, slot: NodeId, node: NodeId) {
        if let Ok(existing) = taffy.style(node).cloned() {
            let _ = taffy.set_style(
                node,
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    flex_grow: 0.0,
                    flex_shrink: 0.0,
                    flex_basis: Dimension::auto(),
                    ..existing
                },
            );
        }
        let _ = taffy.add_child(slot, node);
    }

    /// The shortcuts overlay is fully blocking when visible: it either
    /// consumes the key itself, or every other event is swallowed so
    /// nothing reaches the editor underneath.
    fn handle_shortcuts_overlay(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.shortcuts.is_visible() {
            return false;
        }
        self.shortcuts.handle_event(event, ctx);
        self.self_dirty = true;
        self.refresh_status();
        *ctx.handled = true;
        true
    }

    /// `?` opens the shortcuts overlay — but ONLY when a note isn't being
    /// typed into. A notes app is a text editor; if `?` were a global
    /// shortcut the character could never be typed into a note. So it's
    /// gated on the editor NOT holding focus. When the editor is focused the
    /// key falls through to the block editor and types a literal `?`; when
    /// the page tree / nothing is focused, `?` opens help. This is the
    /// standard "help key only outside inputs" behaviour.
    fn handle_help_key(&mut self, event: &WindowEvent, handled: &mut bool) -> bool {
        if self.main.editor().is_focused() {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        if c.as_str() != "?" {
            return false;
        }
        self.shortcuts.set_visible(true);
        *handled = true;
        self.refresh_status();
        true
    }

    /// Window-level Find always targets the current note body. The searchable
    /// editor and overlay live in `ui::BlockEditor`; Notes only routes the
    /// conventional command when a sibling such as the title owns focus.
    fn handle_find_shortcut(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        focus_target: &mut Option<NodeId>,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(key),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let mods = modifiers.state();
        if !(mods.control_key() || mods.super_key()) || !key.eq_ignore_ascii_case("f") {
            return false;
        }
        if !self.main.open_find(focus_target) {
            return false;
        }
        *handled = true;
        true
    }

    /// Cmd-N / Ctrl-N creates a child of the selected page. Handled at the
    /// app level — not just in the sidebar — so it works no matter which
    /// pane holds focus. The editor takes focus on load, so a
    /// sidebar-only handler (gated on the tree being focused) would
    /// silently no-op, making the welcome copy "Cmd-N adds a page under
    /// the selection" a lie. `N` is modifier-qualified, so it never
    /// conflicts with typing into a note.
    fn handle_new_child_shortcut(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let mods = modifiers.state();
        if !(mods.control_key() || mods.super_key()) || c.to_lowercase().as_str() != "n" {
            return false;
        }
        let parent = self.workspace.selected();
        self.apply_and_dispatch(state, OutEvent::CreateChild { parent });
        self.sidebar.sync_from_workspace(&self.workspace);
        *handled = true;
        true
    }

    /// What the shell's header buttons asked for: a new root note, or the
    /// note's actions anchored to their button. A button can be pressed by
    /// pointer, keyboard or assistive technology, so this runs from `sync`
    /// as well as after input.
    fn drain_header_requests(&mut self, doc: &mut AutoCommit) {
        if self.header.new_note.take() {
            self.apply_and_dispatch(doc, OutEvent::CreateChild { parent: None });
            self.sidebar.sync_from_workspace(&self.workspace);
            self.self_dirty = true;
        }
        if self.header.note_menu.take() {
            self.open_note_menu_from_button();
        }
    }

    /// The window shell: its header buttons, its sidebar toggle and shortcut,
    /// and the drawer where the page tree is too wide to dock. A drawer is
    /// modal while it is open: input belongs to the page tree, and choosing a
    /// note closes it.
    fn handle_shell(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        // A page-tree context menu may extend past the drawer. Give it the
        // dismissing click or Escape before the shell reads either as input
        // to the drawer.
        if self.sidebar_node.is_some() && self.sidebar.menu_is_open() {
            self.route_to_sidebar(event, ctx);
            *ctx.handled = true;
            return true;
        }
        if let Some(node) = self.shell_node {
            self.shell
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
        }
        self.drain_header_requests(ctx.state);
        if *ctx.handled {
            self.self_dirty = true;
            return true;
        }
        let drawer_open = self.shell.geometry().sidebar == SidebarPlacement::Drawer
            && self.shell.is_sidebar_open();
        if !(drawer_open && self.sidebar_node.is_some()) {
            return false;
        }
        self.route_to_sidebar(event, ctx);
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        self.refresh_status();
        *ctx.handled = true;
        true
    }

    /// Route `event` to the page tree and apply what it asked for. Choosing a
    /// note in a drawer closes the drawer.
    fn route_to_sidebar(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let Some(node) = self.sidebar_node else {
            return;
        };
        let selected = self.workspace.selected();
        self.sidebar
            .handle_event(event, &mut ctx.reborrow(ctx.path, node));
        self.claim_sidebar_focus(*ctx.focus_target);
        self.drain_sidebar_events(ctx.state);
        if self.shell.geometry().sidebar == SidebarPlacement::Drawer
            && self.workspace.selected() != selected
            && !self.sidebar.menu_is_open()
        {
            self.set_sidebar_open(false);
        }
    }

    /// Click on a link mark navigates.
    fn handle_link_click(
        &mut self,
        event: &WindowEvent,
        state: &AutoCommit,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        let target = self.link_under_last_mouse();
        let Some(target) = target else { return false };
        let id = match target {
            ui::widget::block_editor::LinkTarget::Page(id) => id,
            ui::widget::block_editor::LinkTarget::External(url) => {
                if let Err(error) = self
                    .open_notes_operation(actions::NotesOperation::OpenExternalLink { url }, state)
                {
                    tracing::warn!(%error, "notes: external link refused");
                    return false;
                }
                *handled = true;
                return true;
            }
        };
        if let Err(error) = self.open_notes_operation(
            actions::NotesOperation::OpenPage {
                page: id,
                block_id: None,
            },
            state,
        ) {
            tracing::warn!(%error, page = %id, "notes: link target went stale");
            return false;
        }
        *handled = true;
        true
    }

    /// Route note commands before pointer or keyboard events reach the editor.
    #[allow(
        clippy::too_many_lines,
        reason = "routes the complete context-menu event lifecycle through one stateful boundary"
    )]
    fn handle_note_context_menu(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.note_menu.is_open() {
            self.note_menu.handle_event(event, ctx);
            if let Some(index) = self.note_menu.take_selection() {
                self.apply_note_menu_selection(index, ctx.state);
            }
            self.self_dirty = true;
            *ctx.handled = true;
            return true;
        }
        let pointer_open = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            }
        ) && self.main.get_rect().contains(self.last_mouse);
        let keyboard_open = matches!(event, WindowEvent::KeyboardInput { event: KeyEvent {
            state: ElementState::Pressed, logical_key, ..
        }, .. } if matches!(logical_key, Key::Named(NamedKey::ContextMenu))
            || (ctx.modifiers.state().shift_key() && matches!(logical_key, Key::Named(NamedKey::F10))))
            && !self.sidebar.is_focused()
            && (self.main.editor().is_focused() || self.main.title_focused());
        if !(pointer_open || keyboard_open) {
            return false;
        }
        // An active title input keeps the shared text field's native menu.
        if self.main.title_focused()
            && (keyboard_open || !self.main.editor().get_rect().contains(self.last_mouse))
        {
            return false;
        }
        let Some(page) = self.workspace.selected() else {
            return false;
        };
        // A pointer opens it where it clicked; the keyboard opens it on the
        // note's actions button, as the menu of that trigger.
        let anchor = if pointer_open {
            Rect::from_origin_size(self.last_mouse, (0.0, 0.0))
        } else {
            self.header_button_rect(NOTE_MENU_BUTTON_ID)
                .unwrap_or_else(|| {
                    Rect::from_origin_size(self.main.get_rect().origin(), (0.0, 0.0))
                })
        };
        self.note_menu_editor = if pointer_open {
            self.main.editor().get_rect().contains(self.last_mouse)
        } else {
            self.main.editor().is_focused()
        };
        if self.note_menu_editor && pointer_open {
            // Right-click transfers ownership without changing the retained
            // caret or selection, unlike synthesizing a left click here.
            self.sidebar.set_focus(false);
            if !self.main.title_focused() {
                self.main.editor_mut().set_focus(true);
            }
        }
        self.note_menu = ContextMenu::new();
        self.note_menu_table = self
            .note_menu_editor
            .then(|| self.main.editor().focused_table_cell())
            .flatten();
        self.note_menu_table_source = self.note_menu_table.as_ref().and_then(|(id, _, _)| {
            self.main
                .editor()
                .blocks()
                .iter()
                .find(|block| &block.id == id)
                .map(|block| block.text().to_owned())
        });
        if self.note_menu_editor {
            for label in ["Cut", "Copy", "Paste", "Select all"] {
                self.note_menu.push(MenuItem::selection(label));
            }
            self.note_menu.push(MenuItem::separator());
            if self.note_menu_table.is_some() {
                for key in [
                    "notes-table-add-row",
                    "notes-table-add-column",
                    "notes-table-delete-row",
                    "notes-table-delete-column",
                ] {
                    self.note_menu
                        .push(MenuItem::selection(ui::i18n::I18nManager::lookup(
                            key, None,
                        )));
                }
                self.note_menu.push(MenuItem::separator());
            }
        }
        for item in crate::sidebar::note_context_menu().items() {
            self.note_menu.push(item.clone());
        }
        self.note_menu.set_text_scale(self.theme.text_scale);
        self.note_menu.set_viewport(Some(self.rect));
        self.note_menu.open_anchored(anchor, self.rect);
        self.note_menu_target = Some(page);
        self.self_dirty = true;
        *ctx.handled = true;
        true
    }

    /// Open the note's actions from the header button: the page's own
    /// commands, anchored to the button that opened them.
    fn open_note_menu_from_button(&mut self) {
        let Some(page) = self.workspace.selected() else {
            return;
        };
        let Some(anchor) = self.header_button_rect(NOTE_MENU_BUTTON_ID) else {
            return;
        };
        self.note_menu_editor = false;
        self.note_menu_table = None;
        self.note_menu_table_source = None;
        self.note_menu = ContextMenu::new();
        for item in crate::sidebar::note_context_menu().items() {
            self.note_menu.push(item.clone());
        }
        self.note_menu.set_text_scale(self.theme.text_scale);
        self.note_menu.set_viewport(Some(self.rect));
        self.note_menu.open_anchored(anchor, self.rect);
        self.note_menu_target = Some(page);
        self.self_dirty = true;
    }

    fn apply_note_menu_selection(&mut self, index: usize, state: &mut AutoCommit) {
        let Some(page) = self.note_menu_target.take() else {
            return;
        };
        if self.workspace.get(page).is_none() {
            return;
        }
        let index = if self.note_menu_editor {
            let table_command = self
                .note_menu_table
                .as_ref()
                .and_then(|_| index.checked_sub(5))
                .and_then(|slot| {
                    [
                        "table-add-row",
                        "table-add-column",
                        "table-delete-row",
                        "table-delete-column",
                    ]
                    .get(slot)
                    .copied()
                });
            let command = ["cut", "copy", "paste", "select-all"]
                .get(index)
                .copied()
                .or(table_command);
            if let Some(command) = command {
                // Preserve the selection captured before opening the menu. A
                // remote note switch must not apply this command to a new note.
                if self.workspace.selected() != Some(page) {
                    return;
                }
                if self.note_menu_table.is_some()
                    && (self.main.editor().focused_table_cell() != self.note_menu_table
                        || self.note_menu_table.as_ref().and_then(|(id, _, _)| {
                            self.main
                                .editor()
                                .blocks()
                                .iter()
                                .find(|block| &block.id == id)
                                .map(ui::widget::Block::text)
                        }) != self.note_menu_table_source.as_deref())
                {
                    return;
                }
                self.main.editor_mut().perform_semantic_action(
                    &format!("notes.editor:{command}"),
                    &ui::a11y::Action::Activate,
                    state,
                    &automerge::ROOT,
                );
                if self.main.editor_is_dirty() {
                    self.save_current_editor_snapshot();
                }
                self.refresh_status();
                return;
            }
            let Some(index) = index.checked_sub(if self.note_menu_table.is_some() {
                10
            } else {
                5
            }) else {
                return;
            };
            index
        } else {
            index
        };
        let result = match index {
            0 => {
                self.apply_and_dispatch(state, OutEvent::CreateChild { parent: Some(page) });
                Ok(())
            }
            1 => {
                self.main.begin_title_edit();
                Ok(())
            }
            2 => {
                self.apply_and_dispatch(state, OutEvent::Duplicate(page));
                Ok(())
            }
            3 => self.open_notes_operation(actions::NotesOperation::CopyPageLink { page }, state),
            4 => self.request_markdown_export(state, page),
            5 => self.request_image_insert(page),
            7 => {
                self.apply_and_dispatch(state, OutEvent::Delete(page));
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            tracing::warn!(%error, "notes: note command failed");
        }
        self.sidebar.sync_from_workspace(&self.workspace);
        self.refresh_status();
    }

    fn route_to_children(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Modal focus capture: while the editor's `/` block menu (or the
        // `[[` link popover) is open, keyboard nav belongs to it — not the
        // page list behind it; symmetrically, while the sidebar's
        // right-click menu is open, keyboard nav belongs to the menu, not
        // the editor. Mouse events still reach both panes (a click only
        // ever hits one pane's rect), so we only gate keyboard routing.
        let is_keyboard = matches!(event, WindowEvent::KeyboardInput { .. });
        let editor_menu_open = self.main.editor().slash_menu_visible()
            || self.main.editor().link_popover().is_visible();
        let sidebar_menu_open = self.sidebar.menu_is_open();

        // Route to the page tree only when it's on screen. A drawer is
        // modal and gets its input from `handle_shell`.
        if self.shell.geometry().sidebar != SidebarPlacement::Drawer
            && (!editor_menu_open || !is_keyboard)
        {
            if let Some(n) = self.sidebar_node {
                self.sidebar
                    .handle_event(event, &mut ctx.reborrow(ctx.path, n));
                self.claim_sidebar_focus(*ctx.focus_target);
            }
            // Drain any commands the sidebar produced.
            self.drain_sidebar_events(ctx.state);
        }
        // The editor never sees keystrokes the sidebar is capturing — its
        // right-click menu OR an in-progress inline rename — so a rename
        // keystroke doesn't also type into the note behind the field.
        let sidebar_capturing_kbd = sidebar_menu_open || self.sidebar.is_renaming();
        if !(sidebar_capturing_kbd && is_keyboard)
            && let Some(n) = self.main_node
        {
            self.main
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
        // The body editor has applied the person's edit as a Notes action.
        // `editor_is_dirty` drains the widget's edit edge, so a pointer move
        // or caret blink never snapshots a long note for its Markdown file.
        if self.main.editor_is_dirty() {
            self.save_current_editor_snapshot();
        }
        // A committed edit of the note's in-page title renames the current
        // page on the same replayable action path the sidebar's inline rename
        // uses, then re-syncs the sidebar so the new name shows in the tree.
        self.drain_title_rename(ctx.state);
        // A fresh workspace has no selected page, so its central first-use
        // action creates a root note. Route it through the same replayable
        // action path as the sidebar + and Cmd/Ctrl-N; a first note must be
        // just as collaborative and undoable as every later one.
        self.drain_empty_note_request(ctx.state);
        // Update the last-mouse cache so the next MouseInput knows
        // where to hit-test for link clicks.
        if let WindowEvent::CursorMoved { position, .. } = event {
            let logical = vello::kurbo::Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            );
            self.last_mouse = logical;
            // Resting on the bubble's corner pins it open.
            let (area, theme) = (self.status_area(&self.theme), &self.theme);
            ui::text_render::with_layout_renderer(theme.text_scale, |text| {
                self.status_bubble
                    .pointer_moved(area, logical, text, theme, ui::time::now())
            });
        }
        // Status bar refresh runs at the tail of every event so word
        // count + save indicator stay current. Cheap — the StatusBar
        // diffs internally.
        self.refresh_status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn settle_markdown_io(app: &mut NotesApp, doc: &mut AutoCommit) {
        tilo_test_env::wait_until("Markdown background I/O", || {
            app.drain_markdown_transfer(doc);
            !app.markdown_job.is_pending() && app.markdown_transfer.is_none()
        });
    }

    fn admit_pending_import(app: &mut NotesApp, state: &mut ui::State) {
        let intent =
            ui::widget::controls::take_control_invocation(app).expect("import emits a host intent");
        let resource = ui::resources::resource(app, &state.doc, &intent.address.resource).unwrap();
        let spec = resource
            .actions
            .iter()
            .find(|spec| spec.id == intent.address.action)
            .unwrap();
        assert_eq!(
            spec.contract.permission,
            tilo_actions::PermissionRequirement::Write
        );
        assert!(spec.params().is_empty());
        let tilo_actions::ActionInput::Immediate(encoded) = &spec.input else {
            panic!("zero-argument import")
        };
        assert_eq!(
            actions::decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(actions::NotesOperation::FinalizeImport)
        );
        let started = app
            .start_operation(
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    spec.kind,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut state.doc,
                &state.root,
            )
            .unwrap()
            .unwrap();
        assert!(matches!(
            started,
            ui::widget::ResourceOperationStart::Completed(_)
        ));
        ui::widget::controls::on_control_invocation_result(app, &intent.address, &Ok(started));
        app.sync(&mut state.doc, &state.root);
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn launch_markdown_adopts_valid_sources_once_with_host_admission() {
        eprintln!("phase launch_markdown_preserves_existing_notebook_source_and_restart");
        {
            for as_url in [false, true] {
                let tmp = TempDir::new().unwrap();
                let workspace_root = tmp.path().join("notebook");
                let mut workspace = Workspace::default();
                let existing = workspace.create_page(None);
                workspace.rename(existing, "Existing note");
                workspace.set_selected(Some(existing));
                storage::save_workspace(&workspace_root, &workspace).unwrap();
                storage::write_page_body(&workspace_root, existing, "Existing body").unwrap();
                let source = tmp.path().join("Überblick notes.markdown");
                let original = "# Imported heading\n\n**Keep these bytes.**\n";
                std::fs::write(&source, original).unwrap();
                let target = if as_url {
                    url::Url::from_file_path(&source).unwrap().to_string()
                } else {
                    source.to_str().unwrap().to_owned()
                };
                let mut app = NotesApp::spawn(workspace_root.clone()).unwrap();
                app.queue_launch_markdown(&target).unwrap();
                assert!(app.needs_animation_frame());
                let mut state = ui::State::new();
                app.sync(&mut state.doc, &state.root);
                assert_eq!(app.workspace.pages().len(), 1, "wait for normal seed/adopt");
                assert!(app.pending_markdown_source.is_some());
                app.bypass_seed_grace_for_test();
                app.sync(&mut state.doc, &state.root);
                admit_pending_import(&mut app, &mut state);
                assert_eq!(app.workspace.pages().len(), 2);
                assert_eq!(
                    NotesDoc::page_block_snapshots(&state.doc, existing)[0].1,
                    "Existing body"
                );
                let imported = app.workspace.selected().unwrap();
                assert_ne!(imported, existing);
                assert_eq!(
                    app.workspace.get(imported).unwrap().title,
                    "Überblick notes"
                );
                assert_eq!(
                    NotesDoc::page_block_snapshots(&state.doc, imported)[0].1,
                    "Imported heading"
                );
                assert!(
                    ui::undo::take_committed_action(&mut app).is_some(),
                    "import uses canonical action/history path"
                );
                app.flush_saves();
                drop(app);
                let restored = NotesApp::spawn(workspace_root.clone()).unwrap();
                assert_eq!(restored.workspace.pages().len(), 2);
                assert_eq!(restored.workspace.selected(), Some(imported));
                assert!(
                    restored
                        .editor_text_for_test()
                        .contains("Keep these bytes.")
                );
                assert_eq!(
                    storage::read_page_body(&workspace_root, existing).unwrap(),
                    "Existing body"
                );
                assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
            }
        }

        eprintln!("phase launch_markdown_rejects_invalid_sources_without_changing_notebook");
        {
            let tmp = TempDir::new().unwrap();
            let mut app = NotesApp::spawn(tmp.path().join("notebook")).unwrap();
            let source = tmp.path().join("invalid.md");
            std::fs::write(&source, [0xff]).unwrap();
            for target in [
                "https://example.com/note.md",
                "file:///%ZZ",
                source.to_str().unwrap(),
            ] {
                assert!(app.queue_launch_markdown(target).is_err());
                assert!(app.pending_markdown_source.is_none());
                assert!(app.workspace.pages().is_empty());
            }
            std::fs::write(&source, "Valid").unwrap();
            app.queue_launch_markdown(source.to_str().unwrap()).unwrap();
            assert!(app.queue_launch_markdown(source.to_str().unwrap()).is_err());
        }

        eprintln!("phase launch_markdown_host_admission_refuses_readonly_and_revoked_write");
        {
            for initially_writable in [false, true] {
                let (mut app, mut state, tmp, existing) = app_seeded_into_doc();
                let source = tmp.path().join("Private source.md");
                let original = "Do not publish without Write";
                std::fs::write(&source, original).unwrap();
                app.queue_launch_markdown(source.to_str().unwrap()).unwrap();
                state.doc.commit();
                let before = state.doc.get_heads();
                let mut harness = ui::headless::FrameHarness::new(state, Box::new(app), (800, 600));
                if initially_writable {
                    assert!(
                        harness
                            .resources()
                            .iter()
                            .any(|resource| resource.id.0 == "notes.pending-import")
                    );
                }
                harness.set_permission_ctx(PermissionContext::from_capabilities(
                    ui::permissions::Actor::person(Some(ui::permissions::Role::Viewer), "reader"),
                    vec![],
                ));
                harness.send(&WindowEvent::RedrawRequested);
                assert_eq!(harness.state_mut().doc.get_heads(), before);
                assert_eq!(
                    NotesDoc::list_pages(&harness.state_mut().doc)
                        .iter()
                        .map(|page| page.id)
                        .collect::<Vec<_>>(),
                    vec![existing]
                );
                assert!(
                    !harness
                        .resources()
                        .iter()
                        .any(|resource| resource.id.0 == "notes.pending-import"),
                    "refused intent clears pending import instead of retrying forever"
                );
                harness.set_permission_ctx(PermissionContext::permissive());
                harness.send(&WindowEvent::RedrawRequested);
                assert_eq!(
                    harness.state_mut().doc.get_heads(),
                    before,
                    "restoring Write must not silently retry a refused import"
                );
                assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
            }
        }

        eprintln!("phase launch_markdown_host_admission_commits_complete_page_once");
        {
            let (mut app, mut state, tmp, _) = app_seeded_into_doc();
            let source = tmp.path().join("Allowed import.md");
            std::fs::write(&source, "# Complete page\n\nBody").unwrap();
            app.queue_launch_markdown(source.to_str().unwrap()).unwrap();
            state.doc.commit();
            let mut harness = ui::headless::FrameHarness::new(state, Box::new(app), (800, 600));
            harness.send(&WindowEvent::RedrawRequested);
            let pages = NotesDoc::list_pages(&harness.state_mut().doc);
            assert_eq!(pages.len(), 2);
            let imported = pages
                .iter()
                .find(|page| page.title == "Allowed import")
                .unwrap();
            assert_eq!(
                NotesDoc::page_block_snapshots(&harness.state_mut().doc, imported.id)
                    .iter()
                    .map(|(_, text)| text.as_str())
                    .collect::<Vec<_>>(),
                vec!["Complete page", "Body"]
            );
            harness.send(&WindowEvent::RedrawRequested);
            assert_eq!(NotesDoc::list_pages(&harness.state_mut().doc).len(), 2);
        }

        eprintln!("phase launch_markdown_waits_for_peer_adoption_before_adding_its_page");
        {
            let tmp = TempDir::new().unwrap();
            let mut app = NotesApp::spawn_with_starter_page(tmp.path().join("notebook")).unwrap();
            let local_page = app.workspace.selected().unwrap();
            let source = tmp.path().join("Launch.md");
            std::fs::write(&source, "Explicit import").unwrap();
            app.queue_launch_markdown(source.to_str().unwrap()).unwrap();
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root);
            assert!(NotesDoc::list_pages(&state.doc).is_empty());

            let mut peer = Workspace::default();
            let peer_page = peer.create_page(None);
            peer.rename(peer_page, "Peer note");
            NotesDoc::upsert_page_meta(&mut state.doc, peer.get(peer_page).unwrap());
            NotesDoc::fill_page_body(
                &mut state.doc,
                peer_page,
                &MainPanel::seed_blocks("Peer contents", |_| None),
            )
            .unwrap();
            app.sync(&mut state.doc, &state.root);
            admit_pending_import(&mut app, &mut state);
            assert_eq!(NotesDoc::list_pages(&state.doc).len(), 2);
            assert!(
                app.workspace.get(local_page).is_none(),
                "never publish pre-adoption local pages"
            );
            assert_eq!(
                NotesDoc::page_block_snapshots(&state.doc, peer_page)[0].1,
                "Peer contents"
            );
            let imported = app.workspace.selected().unwrap();
            assert_ne!(imported, peer_page);
            assert_eq!(
                NotesDoc::page_block_snapshots(&state.doc, imported)[0].1,
                "Explicit import"
            );
            assert!(app.pending_markdown_source.is_none());
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn unreadable_notes_refuse_replacement_until_repaired() {
        eprintln!("phase unreadable_note_refuses_startup_without_replacing_original_bytes");
        {
            for selected in [true, false] {
                let tmp = TempDir::new().unwrap();
                let mut workspace = Workspace::default();
                let page = workspace.create_page(None);
                workspace.rename(page, "Unrecoverable UTF-8");
                workspace.set_selected(selected.then_some(page));
                storage::save_workspace(tmp.path(), &workspace).unwrap();
                let path = storage::page_file_path(tmp.path(), page);
                let original = [0xff, 0xfe, 0x41];
                std::fs::write(&path, original).unwrap();

                let Err(error) = NotesApp::spawn(tmp.path().into()) else {
                    panic!("an unreadable note must not become an empty editor");
                };
                assert!(error.contains("Could not read note"));
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }
        }

        eprintln!("phase unreadable_note_refuses_navigation_and_subtree_duplicate");
        {
            let (mut app, mut state, tmp, welcome) = app_seeded_into_doc();
            let child = app.workspace.create_page(Some(welcome));
            storage::write_page_body(tmp.path(), child, "Child contents").unwrap();
            app.perform_action(
                &mut state.doc,
                &actions::upsert_page_action(app.workspace.get(child).unwrap()),
            );
            app.seed_legacy_page_body(&mut state.doc, child);
            let path = storage::page_file_path(tmp.path(), child);
            let original = [0xff, 0xfe, 0x41];
            std::fs::write(&path, original).unwrap();

            let operation = actions::NotesOperation::OpenPage {
                page: child,
                block_id: None,
            };
            assert!(app.open_notes_operation(operation, &state.doc).is_err());
            assert_eq!(app.workspace.selected(), Some(welcome));
            assert_eq!(app.main.page_id(), Some(welcome));
            app.apply_event(OutEvent::Select(child));
            assert_eq!(app.workspace.selected(), Some(welcome));
            let before = app.workspace.pages().to_vec();
            app.apply_and_dispatch(&mut state.doc, OutEvent::Duplicate(welcome));
            assert_eq!(app.workspace.pages(), before);
            assert_eq!(NotesDoc::list_pages(&state.doc).len(), 2);
            app.flush_saves();
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert!(
                app.transfer_toasts
                    .iter()
                    .any(|toast| toast.kind == ui::widget::toast::ToastKind::Error)
            );
        }

        eprintln!("phase unreadable_note_is_not_seeded_or_bound_as_empty_and_can_retry");
        {
            let tmp = TempDir::new().unwrap();
            let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
            let page = app.workspace.selected().unwrap();
            app.main.editor_mut().blocks_mut()[0]
                .line
                .set_text("Unsaved draft");
            app.main.editor_mut().force_dirty_for_test();
            let path = storage::page_file_path(tmp.path(), page);
            let original = [0xff, 0xfe, 0x41];
            std::fs::write(&path, original).unwrap();
            app.bypass_seed_grace_for_test();
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root);
            assert!(
                NotesDoc::page_block_snapshots(&state.doc, page)
                    .iter()
                    .all(|(_, text)| text.is_empty())
            );
            assert_eq!(app.bound_page, None);
            assert_eq!(app.main.page_id(), Some(page));
            assert_eq!(app.main.editor().blocks()[0].text(), "Unsaved draft");
            assert!(app.try_navigate_to(page).is_err());
            app.flush_saves();
            assert_eq!(std::fs::read(&path).unwrap(), original);

            storage::write_page_body(tmp.path(), page, "Recovered note").unwrap();
            app.sync(&mut state.doc, &state.root);
            assert_eq!(app.bound_page, Some(page));
            assert_eq!(
                NotesDoc::page_block_snapshots(&state.doc, page)[0].1,
                "Unsaved draft"
            );
        }

        eprintln!("phase unreadable_note_recovery_uses_repaired_file_when_draft_is_unchanged");
        {
            let tmp = TempDir::new().unwrap();
            let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
            let page = app.workspace.selected().unwrap();
            assert!(!app.main.has_unsaved_body());
            let path = storage::page_file_path(tmp.path(), page);
            std::fs::write(&path, [0xff]).unwrap();
            app.bypass_seed_grace_for_test();
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root);
            app.flush_saves();
            assert_eq!(std::fs::read(&path).unwrap(), [0xff]);
            storage::write_page_body(tmp.path(), page, "Recovered external contents").unwrap();
            app.sync(&mut state.doc, &state.root);
            assert_eq!(
                NotesDoc::page_block_snapshots(&state.doc, page)[0].1,
                "Recovered external contents"
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn workspace_images_decode_in_root_and_import_bounded_portable_assets() {
        eprintln!("phase workspace_image_loader_decodes_only_in_root");
        {
            let tmp = TempDir::new().unwrap();
            storage::ensure_layout(tmp.path()).unwrap();
            let fixture = include_bytes!("../../settings/shots/settings.png");
            std::fs::write(
                tmp.path().join(storage::PAGES_DIR).join("image.png"),
                fixture,
            )
            .unwrap();
            let store = workspace_image_store(tmp.path());
            assert!(store.get("image.png").is_none());
            store.block_until_idle(tilo_test_env::hang_bound());
            let image = store.get("image.png").expect("workspace image must decode");
            assert_eq!(
                (image.width, image.height),
                (1200, 760),
                "document images retain readable source detail"
            );
            assert!(store.get("../../etc/passwd").is_none());
            store.block_until_idle(tilo_test_env::hang_bound());
            assert!(store.get("../../etc/passwd").is_none());
        }

        eprintln!("phase canonical_notebook_keeps_image_insert_and_portable_markdown_assets");
        {
            let workspace = TempDir::new().unwrap();
            let source = TempDir::new().unwrap();
            let image = source.path().join("photo.png");
            std::fs::write(&image, include_bytes!("../../settings/shots/settings.png")).unwrap();
            let markdown = source.path().join("Imported.md");
            let body = "![Portable](photo.png)\n";
            std::fs::write(&markdown, body).unwrap();
            let mut doc = AutoCommit::new();
            crate::notebook::initialize_empty(&mut doc).unwrap();
            let author = ui::permissions::UserId("canonical-assets-test".into());
            let mut app = NotesApp::from_document(&doc, &author).unwrap();
            app.configure_workspace_assets(workspace.path()).unwrap();
            let legacy_reference = import_image_asset(workspace.path(), &image).unwrap();
            let legacy_path = workspace
                .path()
                .join(storage::PAGES_DIR)
                .join(&legacy_reference);
            let legacy_bytes = std::fs::read(&legacy_path).unwrap();
            app.finish_markdown_import(&mut doc, &markdown, body, None, None)
                .unwrap();
            let page = app.workspace.selected().unwrap();
            let imported = NotesDoc::page_block_snapshots(&doc, page);
            assert!(imported.iter().any(|(_, text)| text.contains("asset:")));
            app.finish_image_insert(&mut doc, page, &image).unwrap();
            let images = NotesDoc::page_block_snapshots(&doc, page);
            assert_eq!(
                images
                    .iter()
                    .filter(|(_, text)| text.contains("asset:"))
                    .count(),
                2
            );
            // Retain a pre-existing scoped attachment beside the new embedded
            // images; compatibility custody must not replace the canonical map.
            let action = actions::edit_body_action(
                page,
                tilo_document::RichTextAction::InsertParagraph {
                    after: images.last().map(|(id, _)| id.clone()).unwrap(),
                    text: format!("![Existing]({legacy_reference})"),
                },
            );
            app.actions
                .try_dispatch_with_context(
                    &mut doc,
                    actions::NOTES_ACTION_TYPE,
                    &action,
                    actions::history_context(&action),
                )
                .unwrap();
            app.sync(&mut doc, &automerge::ROOT);
            let embedded = format!("asset:{}", blake3::hash(&legacy_bytes).to_hex());
            let store = canonical_image_store(&app.canonical_assets, Some(workspace.path()));
            assert!(store.get(&embedded).is_none());
            assert!(store.get(&legacy_reference).is_none());
            store.block_until_idle(tilo_test_env::hang_bound());
            assert!(store.get(&embedded).is_some());
            assert!(store.get(&legacy_reference).is_some());
            assert_eq!(std::fs::read(&legacy_path).unwrap(), legacy_bytes);
            let destination = source.path().join("Exported.md");
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(destination.clone()));
            app.request_markdown_export(&doc, page).unwrap();
            settle_markdown_io(&mut app, &mut doc);
            let exported = std::fs::read_to_string(&destination).unwrap();
            assert!(!exported.contains("../assets/"));
            let references: Vec<_> = exported
                .lines()
                .filter_map(standalone_image_parts)
                .collect();
            assert_eq!(references.len(), 3);
            for (_, _, reference) in references {
                assert_eq!(
                    std::fs::read(source.path().join(reference)).unwrap(),
                    std::fs::read(&image).unwrap()
                );
            }
            assert!(app.legacy_fixture_storage.is_none());
            assert!(!storage::workspace_file_path(workspace.path()).exists());
            crate::notebook::validate(&doc).unwrap();
        }

        eprintln!("phase image_attachment_import_is_bounded_and_workspace_relative");
        {
            let tmp = TempDir::new().unwrap();
            storage::ensure_layout(tmp.path()).unwrap();
            let source = tmp.path().join("picked.png");
            std::fs::write(&source, include_bytes!("../../settings/shots/settings.png")).unwrap();
            let markdown_path = import_image_asset(tmp.path(), &source).unwrap();
            assert!(markdown_path.starts_with("../assets/"));
            assert!(
                tmp.path()
                    .join(storage::PAGES_DIR)
                    .join(markdown_path)
                    .exists()
            );

            let invalid = tmp.path().join("not-image.png");
            std::fs::write(&invalid, b"not an image").unwrap();
            assert!(import_image_asset(tmp.path(), &invalid).is_err());
        }

        eprintln!("phase repeated_markdown_images_only_consume_the_unique_asset_budget_once");
        {
            let tmp = TempDir::new().unwrap();
            let image = tmp.path().join("image.png");
            std::fs::write(&image, include_bytes!("../../settings/shots/settings.png")).unwrap();
            let markdown = "![Repeated](image.png)\n".repeat(10);

            let (imported, assets) =
                adopt_canonical_markdown_assets(&tmp.path().join("note.md"), &markdown, None)
                    .unwrap();

            assert_eq!(assets.len(), 1);
            let source = &assets[0].0;
            assert_eq!(imported, format!("![Repeated]({source})\n").repeat(10));
        }
    }

    #[test]
    fn canonical_image_keys_cannot_fall_back_to_scoped_files() {
        let bytes = include_bytes!("../../settings/shots/settings.png").to_vec();
        let key = format!("asset:{}", blake3::hash(&bytes).to_hex());
        let missing = format!("asset:{}", "0".repeat(64));
        let mut assets = HashMap::new();
        assets.insert(key.clone(), bytes.clone());
        // Deliberately offer bytes for every scoped key. Even this fallback
        // cannot shadow an embedded key or supply a missing asset: key.
        let fallback: ui::Fetcher = Arc::new(|_| Some(vec![1, 2, 3]));
        let fetch =
            canonical_image_fetcher(Arc::new(std::sync::RwLock::new(assets)), Some(fallback));
        assert_eq!(fetch(&key), Some(bytes));
        assert_eq!(fetch(&missing), None);
        assert_eq!(fetch("../assets/old.png"), Some(vec![1, 2, 3]));
    }

    #[test]
    fn scoped_image_export_refuses_oversized_assets() {
        let root = TempDir::new().unwrap();
        storage::ensure_layout(root.path()).unwrap();
        std::fs::create_dir_all(root.path().join("assets")).unwrap();
        let file = std::fs::File::create(root.path().join("assets/oversized.png")).unwrap();
        file.set_len(20 * 1024 * 1024 + 1).unwrap();
        let destination = root.path().join("export.md");
        assert!(
            write_portable_markdown_export(
                Some(root.path()),
                &HashMap::new(),
                &destination,
                b"![oversized](../assets/oversized.png)\n",
            )
            .is_err()
        );
        assert!(!destination.exists());
    }

    #[cfg(all(test, unix))]
    #[test]
    fn scoped_image_export_refuses_a_symlink_outside_custody() {
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        storage::ensure_layout(root.path()).unwrap();
        std::fs::create_dir_all(root.path().join("assets")).unwrap();
        let image = outside.path().join("private.png");
        std::fs::write(&image, include_bytes!("../../settings/shots/settings.png")).unwrap();
        std::os::unix::fs::symlink(&image, root.path().join("assets/escape.png")).unwrap();
        let destination = root.path().join("export.md");
        assert!(
            write_portable_markdown_export(
                Some(root.path()),
                &HashMap::new(),
                &destination,
                b"![outside](../assets/escape.png)\n",
            )
            .is_err()
        );
        assert!(!destination.exists());
    }

    #[test]
    fn spawn_starts_empty_unless_a_starter_page_or_first_note_is_requested() {
        eprintln!("phase spawn_starts_empty_by_default");
        {
            // The live default (`spawn` / `spawn_default`) must NOT fabricate a
            // page: a fresh workspace opens empty onto the "New note" affordance.
            let tmp = TempDir::new().unwrap();
            let app = NotesApp::spawn(tmp.path().into()).unwrap();
            assert!(
                app.workspace.pages().is_empty(),
                "a fresh launch seeds no pages"
            );
            assert!(
                app.workspace.selected().is_none(),
                "nothing selected on an empty workspace"
            );
            assert!(app.main.title().is_none(), "no page bound in the editor");
        }

        eprintln!("phase spawn_with_starter_page_seeds_welcome");
        {
            // The explicit test/dev fixture constructor still seeds a starter page.
            let tmp = TempDir::new().unwrap();
            let app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
            assert_eq!(app.workspace.pages().len(), 1);
            assert_eq!(app.workspace.pages()[0].title, "Welcome");
            // Welcome page file exists on disk.
            let id = app.workspace.pages()[0].id;
            assert!(storage::page_file_path(tmp.path(), id).exists());
        }

        eprintln!("phase accessibility_activation_creates_the_first_shared_note_on_sync");
        {
            let tmp = TempDir::new().unwrap();
            let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
            let mut doc = AutoCommit::new();
            assert!(
                app.main
                    .perform_action(&ui::a11y::Action::Activate, &mut doc, &ObjId::Root,)
            );

            app.sync(&mut doc, &ObjId::Root);
            assert_eq!(app.workspace.pages().len(), 1);
            assert!(app.workspace.selected().is_some());
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn page_events_create_delete_rename_and_dispatch_canonical_actions() {
        eprintln!("phase apply_event_create_child_appends_and_selects");
        {
            let tmp = TempDir::new().unwrap();
            let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
            let root = app.workspace.pages()[0].id;
            app.apply_event(OutEvent::CreateChild { parent: Some(root) });
            let children = app.workspace.children_of(Some(root));
            assert_eq!(children.len(), 1);
            assert_eq!(app.workspace.selected(), Some(children[0].id));
        }

        eprintln!("phase apply_event_delete_removes_file_and_resets_selection");
        {
            let tmp = TempDir::new().unwrap();
            let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
            let root = app.workspace.pages()[0].id;
            app.apply_event(OutEvent::CreateChild { parent: Some(root) });
            let child = app.workspace.selected().unwrap();
            let child_path = storage::page_file_path(tmp.path(), child);
            assert!(child_path.exists());
            app.apply_event(OutEvent::Delete(child));
            assert!(!child_path.exists());
            // Selection falls back to a remaining root page.
            assert_eq!(app.workspace.selected(), Some(root));
        }

        eprintln!("phase rename_updates_main_panel_title");
        {
            let tmp = TempDir::new().unwrap();
            let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
            let root = app.workspace.pages()[0].id;
            app.apply_event(OutEvent::Rename {
                id: root,
                new_title: "Index".into(),
            });
            assert_eq!(app.main.title(), Some("Index"));
        }

        eprintln!("phase sidebar_delete_and_palette_encode_identically");
        {
            let (mut app, mut state, _tmp, welcome) = app_seeded_into_doc();

            // Palette side: the welcome page resource's delete spec (built before we
            // delete the page, while it still exists in the tree).
            let resources = ui::resources::resources(&app, &state.doc);
            let page = resources[0]
                .children
                .iter()
                .find(|r| r.label == "Welcome")
                .expect("a Welcome page resource");
            let delete_spec = page.actions.iter().find(|a| a.id == "delete").unwrap();
            let from_palette = match &delete_spec.input {
                tilo_actions::ActionInput::Immediate(enc) => enc.clone(),
                tilo_actions::ActionInput::Prompted { .. }
                | tilo_actions::ActionInput::Deferred { .. } => {
                    panic!("delete is an immediate action")
                }
            };

            // Affordance side: the sidebar's Delete event through the app path.
            app.dispatch_event_for_test(&mut state.doc, OutEvent::Delete(welcome));
            let from_button = app
                .last_dispatched_for_test()
                .expect("the sidebar delete dispatched an action")
                .clone();

            assert_eq!(from_palette.type_id, from_button.type_id);
            assert_eq!(
                from_palette.payload, from_button.payload,
                "the sidebar affordance and the palette build the same EncodedAction"
            );
        }

        eprintln!("phase creating_a_page_dispatches_an_upsert_into_the_doc");
        {
            let (mut app, mut state, _tmp, _welcome) = app_seeded_into_doc();
            let before = NotesDoc::list_pages(&state.doc).len();

            app.dispatch_event_for_test(&mut state.doc, OutEvent::CreateChild { parent: None });

            assert_eq!(
                NotesDoc::list_pages(&state.doc).len(),
                before + 1,
                "the create wrote a new page into the shared document"
            );
            let new_page = app
                .workspace
                .selected()
                .expect("new page becomes the active page");
            assert!(
                !NotesDoc::page_blocks(&state.doc, new_page).is_empty(),
                "the new page body also crossed the typed action executor"
            );
        }

        eprintln!("phase page_delete_records_one_undo_boundary_and_reverts_in_the_app");
        {
            let (mut app, mut state, _tmp, welcome) = app_seeded_into_doc();
            assert!(
                NotesDoc::list_pages(&state.doc)
                    .iter()
                    .any(|p| p.id == welcome)
            );

            // Delete through the real sidebar-event path.
            app.dispatch_event_for_test(&mut state.doc, OutEvent::Delete(welcome));

            // The host's own recorder drains the captured heads and records the
            // boundary. Going through it rather than pushing an entry by hand is
            // the point: attribution happens in there, and undo is author-scoped.
            // The node map is empty because the app widget is the root here.
            let local_author = ui::permissions::UserId("notes-app-test-author".into());
            let root: *mut dyn Widget = &mut app;
            assert!(
                ui::undo::record_dispatched_action(
                    &mut state,
                    root,
                    &std::collections::HashMap::new(),
                    &local_author,
                ),
                "the host recorded an undo boundary for the dispatched delete"
            );

            assert!(
                !NotesDoc::list_pages(&state.doc)
                    .iter()
                    .any(|p| p.id == welcome),
                "the delete removed the page from the document"
            );
            let entry = state
                .peek_last_undo()
                .expect("exactly one undo boundary recorded");
            assert_eq!(
                entry.author.as_ref(),
                Some(&local_author),
                "attributed to the local user, so their Ctrl+Z can reach it"
            );
            assert!(
                ui::undo::take_committed_action(&mut app).is_none(),
                "the boundary is rising-edge — cleared on read"
            );

            // Ctrl+Z reverts the document; the next `sync` reconciles the workspace.
            state
                .undo_as(Some(&local_author), None, None)
                .expect("an undo entry to pop");
            assert!(
                NotesDoc::list_pages(&state.doc)
                    .iter()
                    .any(|p| p.id == welcome),
                "undo restored the page in the document"
            );
            app.sync(&mut state.doc, &state.root);
            assert!(
                app.workspace_pages_for_test()
                    .iter()
                    .any(|p| p.id == welcome),
                "the app shows the page again after undo"
            );
        }
    }

    #[test]
    fn status_reports_worst_save_state_live_word_counts_and_calm_motion() {
        eprintln!("phase combine_save_status_is_worst_of");
        {
            use SaveStatus::*;
            // Failed dominates every combination.
            assert_eq!(combine_save_status(Failed, Saved), Failed);
            assert_eq!(combine_save_status(Saved, Failed), Failed);
            assert_eq!(combine_save_status(Failed, Saving), Failed);
            assert_eq!(combine_save_status(Saving, Failed), Failed);
            assert_eq!(combine_save_status(Failed, Failed), Failed);
            // Saving wins over Saved when nothing failed.
            assert_eq!(combine_save_status(Saving, Saved), Saving);
            assert_eq!(combine_save_status(Saved, Saving), Saving);
            assert_eq!(combine_save_status(Saving, Saving), Saving);
            // Only both-clean reports Saved.
            assert_eq!(combine_save_status(Saved, Saved), Saved);
        }

        eprintln!(
            "phase word_count_updates_without_reopening_status_and_save_failure_stays_visible"
        );
        {
            let tmp = TempDir::new().unwrap();
            let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
            app.shots_save_status = Some(SaveStatus::Saved);
            app.refresh_status();
            app.status_bubble = ui::StatusBubble::new();
            app.main
                .editor_mut()
                .set_blocks(vec![ui::widget::Block::paragraph("One two three")]);
            app.refresh_status();
            assert_eq!(app.status.state().word_count, Some(3));
            assert!(!app.status_bubble.is_visible(ui::time::now()));

            app.shots_save_status = Some(SaveStatus::Failed);
            app.refresh_status();
            app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            app.last_mouse = Point::ZERO;
            app.draw_status_bubble(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::default(),
            );
            assert!(
                app.main.save_failure_shown_for_test(),
                "an unresolved save failure stays in view under the title"
            );
            assert!(
                !app.status_bubble
                    .is_visible(ui::time::now() + std::time::Duration::from_secs(60)),
                "the bubble does not announce a failure the banner already shows"
            );
            assert!(
                !app.status_bubble.needs_frame(ui::time::now()),
                "persistent status must not cause an idle redraw loop"
            );

            app.shots_save_status = Some(SaveStatus::Saved);
            app.refresh_status();
            assert!(
                !app.main.save_failure_shown_for_test(),
                "the failure goes away once the note saves"
            );
        }

        eprintln!("phase reduced_motion_stops_notes_owned_status_bubble_frames");
        {
            let tmp = TempDir::new().unwrap();
            let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
            app.status_bubble.poke(ui::clock::Instant::now());
            assert!(app.needs_animation_frame(), "normal fade requests frames");

            app.theme.reduce_motion = true;
            assert!(
                !app.needs_animation_frame(),
                "reduced motion must not keep the compositor awake for a decorative fade"
            );
        }

        eprintln!("phase unchanged_document_heads_skip_full_search_snapshot_work");
        {
            let (mut app, mut state, _tmp, _welcome) = app_seeded_into_doc();
            app.search_blocks.clear();
            app.search_heads = Some(state.doc.get_heads());
            app.refresh_search_blocks(&state.doc);
            assert!(app.search_blocks.is_empty());
        }
    }

    /// A fresh app whose seeded "Welcome" page is mirrored into `state.doc`, with
    /// the seed-grace window collapsed so the first `sync` seeds immediately.
    fn app_seeded_into_doc() -> (NotesApp, ui::State, TempDir, PageId) {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        app.bypass_seed_grace_for_test();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let welcome = app
            .selected_for_test()
            .expect("welcome page seeded + selected");
        // `sync` seeded the welcome page's initial blocks through the action
        // host. The real window host drains that edge at the end of the frame;
        // mirror that lifecycle here so subsequent tests bracket only the
        // user action they dispatch.
        let _ = ui::undo::take_committed_action(&mut app);
        (app, state, tmp, welcome)
    }

    #[test]
    fn copy_page_link_operation_uses_the_current_title_and_stable_target() {
        const CHILD: &str = "TILO_NOTES_COPY_LINK_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let temp = TempDir::new().unwrap();
            let stdout = temp.path().join("stdout.log");
            let stderr = temp.path().join("stderr.log");
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "app::tests::copy_page_link_operation_uses_the_current_title_and_stable_target",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .current_dir(temp.path())
                .stdout(std::fs::File::create(&stdout).unwrap())
                .stderr(std::fs::File::create(&stderr).unwrap());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            let mut child = command.spawn().unwrap();
            let deadline = tilo_test_env::hang_deadline();
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if ui::time::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("isolated clipboard regression timed out");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            let stdout = std::fs::read_to_string(stdout).unwrap();
            assert!(
                status.success(),
                "{stdout}\n{}",
                std::fs::read_to_string(stderr).unwrap()
            );
            // libtest exits 0 when `--exact` selects nothing; require the child ran.
            assert!(
                stdout.contains("test result: ok. 1 passed"),
                "the child ran no test: {stdout}"
            );
            return;
        }
        // The default host is process-global. Install a recording one only in
        // this isolated child so neither this assertion nor concurrent widget
        // tests touch the user's real clipboard or replace one another's host.
        ui::host::install_default(ui::host::HostServices::recording().0);
        let (mut app, state, _tmp, welcome) = app_seeded_into_doc();
        app.open_notes_operation(
            actions::NotesOperation::CopyPageLink { page: welcome },
            &state.doc,
        )
        .unwrap();
        let copied = (0..10).find_map(|_| {
            let value = ui::clipboard_paste();
            if value.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            value
        });
        assert_eq!(copied.as_deref(), Some("[[Welcome]]"));
        assert!(
            app.open_notes_operation(
                actions::NotesOperation::CopyPageLink {
                    page: PageId::new_v4(),
                },
                &state.doc,
            )
            .is_err(),
            "a stale resource must not copy a nearby page's link"
        );
        assert_eq!(ui::clipboard_paste().as_deref(), Some("[[Welcome]]"));
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn navigation_and_backlinks_follow_stable_page_identity() {
        eprintln!("phase note_navigation_history_is_browser_style_and_action_addressable");
        {
            let (mut app, mut state, _tmp, welcome) = app_seeded_into_doc();
            app.dispatch_event_for_test(&mut state.doc, OutEvent::CreateChild { parent: None });
            let second = app.selected_for_test().expect("second note selected");
            app.dispatch_event_for_test(&mut state.doc, OutEvent::CreateChild { parent: None });
            let third = app.selected_for_test().expect("third note selected");

            app.open_notes_operation(actions::NotesOperation::NavigateBack, &state.doc)
                .expect("Back is available");
            app.sync(&mut state.doc, &state.root);
            assert_eq!(app.selected_for_test(), Some(second));

            app.open_notes_operation(actions::NotesOperation::NavigateBack, &state.doc)
                .expect("a second Back reaches the original note");
            app.sync(&mut state.doc, &state.root);
            assert_eq!(app.selected_for_test(), Some(welcome));

            app.open_notes_operation(actions::NotesOperation::NavigateForward, &state.doc)
                .expect("Forward replays the abandoned route");
            app.sync(&mut state.doc, &state.root);
            assert_eq!(app.selected_for_test(), Some(second));

            app.dispatch_event_for_test(&mut state.doc, OutEvent::Select(third));
            assert!(
                app.open_notes_operation(actions::NotesOperation::NavigateForward, &state.doc)
                    .is_err(),
                "a fresh navigation clears the old forward branch"
            );

            let notebook = ui::resources::resources(&app, &state.doc)
                .into_iter()
                .find(|resource| resource.id.as_str() == "notes.notebook")
                .expect("notebook resource");
            assert!(notebook.actions.iter().any(|action| action.id == "back"));
        }

        eprintln!("phase canonical_links_produce_sorted_backlinks_for_the_target_note");
        {
            let (mut app, mut state, _tmp, target) = app_seeded_into_doc();
            app.dispatch_event_for_test(&mut state.doc, OutEvent::CreateChild { parent: None });
            let source = app.selected_for_test().expect("new source page selected");
            app.dispatch_event_for_test(
                &mut state.doc,
                OutEvent::Rename {
                    id: source,
                    new_title: "Project index".into(),
                },
            );
            let block = NotesDoc::page_block_snapshots(&state.doc, source)[0]
                .0
                .clone();
            app.perform_action(
                &mut state.doc,
                &actions::edit_body_action(
                    source,
                    tilo_document::RichTextAction::SpliceText {
                        region: tilo_document::action::RegionKind::Body,
                        block: block.clone(),
                        at: 0,
                        del: 0,
                        ins: "Welcome".into(),
                    },
                ),
            );
            app.perform_action(
                &mut state.doc,
                &actions::set_mark_action(source, &block, 0..7, &ui::widget::Mark::Link(target)),
            );
            // Exercise the canonical reader rather than the legacy Markdown
            // fallback used by this fixture's launch path.
            app.legacy_fixture_storage = None;
            let backlinks = app.backlinks_for(&state.doc, target);
            assert_eq!(backlinks.len(), 1);
            assert_eq!(backlinks[0].source, source);
            assert_eq!(backlinks[0].source_title, "Project index");
            assert_eq!(backlinks[0].target, target);
            assert_eq!(backlinks[0].excerpt, "Welcome");
        }

        eprintln!("phase external_link_outcomes_are_drained_and_failures_are_visible");
        {
            for (outcome, failed) in [
                (Some(Ok(())), false),
                (Some(Err("host refused the link".to_string())), true),
                (None, true),
            ] {
                let (mut app, _state, _tmp, _) = app_seeded_into_doc();
                let (sender, receiver) = std::sync::mpsc::channel();
                app.external_link_opens.push(ExternalLinkJob {
                    receiver,
                    operation: None,
                });
                app.drain_external_link_opens();
                assert_eq!(app.external_link_opens.len(), 1);
                if let Some(outcome) = outcome {
                    sender.send(outcome).unwrap();
                }
                drop(sender);
                app.drain_external_link_opens();
                assert!(app.external_link_opens.is_empty());
                assert_eq!(
                    app.transfer_toasts
                        .iter()
                        .any(|toast| { toast.kind == ui::widget::toast::ToastKind::Error }),
                    failed,
                );
            }
        }

        eprintln!("phase native_link_marks_build_a_cached_backlink_and_open_its_canonical_source");
        {
            let (mut app, mut state, _tmp, source) = app_seeded_into_doc();

            // Create the destination through the normal page action path, then
            // attach a real native link mark to an existing source block. This is
            // intentionally not a Markdown-parser test: incoming links must be
            // derived from the same canonical mark data collaborators receive.
            app.dispatch_event_for_test(&mut state.doc, OutEvent::CreateChild { parent: None });
            let target = app
                .selected_for_test()
                .expect("new page becomes the backlink destination");
            assert_ne!(source, target);
            let source_block = app.search_blocks[&source][0].id.clone();
            let action = crate::actions::set_mark_action(
                source,
                &source_block,
                0..4,
                &ui::widget::Mark::Link(target),
            );
            app.perform_action(&mut state.doc, &action);

            app.navigate_to(target);
            app.sync(&mut state.doc, &state.root);
            assert_eq!(
                app.selected_backlink_chips(),
                vec![BacklinkChip {
                    page: source,
                    title: "Welcome".into(),
                    references: 1,
                }],
                "the selected page projects its incoming native link once"
            );

            app.open_notes_operation(
                crate::actions::NotesOperation::OpenPage {
                    page: source,
                    block_id: None,
                },
                &state.doc,
            )
            .expect("backlink uses the exported canonical page operation");
            assert_eq!(app.selected_for_test(), Some(source));
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn portable_export_copies_assets_safely_and_round_trips_images() {
        eprintln!("phase portal_markdown_export_writes_the_snapshotted_bytes");
        {
            let (mut app, mut state, tmp, page) = app_seeded_into_doc();
            let destination = tmp.path().join("exported.md");
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(destination.clone()));
            app.request_markdown_export(&state.doc, page).unwrap();
            assert_eq!(recorder.pick_requests()[0].title(), "Export Markdown");
            let Some((MarkdownTransfer::Export { markdown, .. }, _)) = &app.markdown_transfer
            else {
                panic!("export must snapshot the page before the chooser answers");
            };
            let snapshot = markdown.clone();
            assert!(!snapshot.is_empty());
            settle_markdown_io(&mut app, &mut state.doc);
            assert_eq!(std::fs::read(destination).unwrap(), snapshot);
            assert!(
                app.transfer_toasts
                    .iter()
                    .any(|toast| toast.kind == ui::widget::toast::ToastKind::Success)
            );
        }

        eprintln!("phase portable_export_copies_managed_images_and_rewrites_markdown");
        {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path().join("workspace");
            storage::ensure_layout(&root).unwrap();
            std::fs::create_dir_all(root.join("assets")).unwrap();
            std::fs::write(root.join("assets/picture.png"), b"png bytes").unwrap();
            let destination = tmp.path().join("Project brief.md");
            write_portable_markdown_export(
                Some(&root),
                &HashMap::new(),
                &destination,
                b"# Brief\n\n![Diagram](../assets/picture.png)\n",
            )
            .unwrap();
            assert_eq!(
                std::fs::read_to_string(&destination).unwrap(),
                "# Brief\n\n![Diagram](Project-brief-assets/picture.png)\n"
            );
            assert_eq!(
                std::fs::read(tmp.path().join("Project-brief-assets/picture.png")).unwrap(),
                b"png bytes"
            );
        }

        eprintln!("phase portable_export_does_not_clobber_conflicting_assets");
        {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path().join("workspace");
            storage::ensure_layout(&root).unwrap();
            std::fs::create_dir_all(root.join("assets")).unwrap();
            std::fs::write(root.join("assets/picture.png"), b"new").unwrap();
            let folder = tmp.path().join("note-assets");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("picture.png"), b"existing").unwrap();
            let destination = tmp.path().join("note.md");
            write_portable_markdown_export(
                Some(&root),
                &HashMap::new(),
                &destination,
                b"![Diagram](../assets/picture.png)\n",
            )
            .unwrap();
            assert_eq!(
                std::fs::read(folder.join("picture.png")).unwrap(),
                b"existing"
            );
            assert!(std::fs::read(folder.join("picture-2.png")).is_ok());
            assert_eq!(
                std::fs::read_to_string(destination).unwrap(),
                "![Diagram](note-assets/picture-2.png)\n"
            );
        }

        eprintln!("phase portable_export_does_not_follow_asset_path_traversal");
        {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path().join("workspace");
            std::fs::create_dir_all(root.join("assets")).unwrap();
            std::fs::write(root.join("secret.png"), b"private").unwrap();
            let destination = tmp.path().join("note.md");

            write_portable_markdown_export(
                Some(&root),
                &HashMap::new(),
                &destination,
                b"![Diagram](../assets/../secret.png)\n",
            )
            .unwrap();

            assert_eq!(
                std::fs::read_to_string(destination).unwrap(),
                "![Diagram](../assets/../secret.png)\n"
            );
            assert!(!tmp.path().join("note-assets").exists());
        }

        eprintln!("phase portable_image_export_import_round_trip_adopts_assets");
        {
            let tmp = TempDir::new().unwrap();
            let first = tmp.path().join("first");
            storage::ensure_layout(&first).unwrap();
            std::fs::create_dir_all(first.join("assets")).unwrap();
            std::fs::write(
                first.join("assets/picture.png"),
                include_bytes!("../../settings/shots/settings.png"),
            )
            .unwrap();
            let exported = tmp.path().join("portable.md");
            write_portable_markdown_export(
                Some(&first),
                &HashMap::new(),
                &exported,
                b"![Diagram](../assets/picture.png)\n",
            )
            .unwrap();

            let second = tmp.path().join("second");
            storage::ensure_layout(&second).unwrap();
            let imported = adopt_portable_markdown_assets(
                &second,
                &exported,
                &std::fs::read_to_string(&exported).unwrap(),
                None,
            )
            .unwrap();
            let (_, managed) = imported.trim().split_once("](").unwrap();
            let managed = managed.strip_suffix(')').unwrap();
            assert!(managed.starts_with("../assets/"));
            assert!(second.join(storage::PAGES_DIR).join(managed).exists());
        }

        eprintln!("phase canonical_image_export_import_round_trip_preserves_the_bitmap");
        {
            let tmp = TempDir::new().unwrap();
            let bytes = include_bytes!("../../settings/shots/settings.png").to_vec();
            let digest = blake3::hash(&bytes).to_hex().to_string();
            let source = format!("asset:{digest}");
            let mut assets = HashMap::new();
            assets.insert(source.clone(), bytes.clone());
            let destination = tmp.path().join("portable.md");
            write_portable_markdown_export(
                None,
                &assets,
                &destination,
                format!("![Diagram]({source})\n").as_bytes(),
            )
            .unwrap();

            let exported = std::fs::read_to_string(&destination).unwrap();
            assert_eq!(
                exported,
                format!("![Diagram](portable-assets/{digest}.png)\n")
            );
            let (imported, imported_assets) =
                adopt_canonical_markdown_assets(&destination, &exported, None).unwrap();
            assert_eq!(imported, format!("![Diagram]({source})\n"));
            assert_eq!(imported_assets, vec![(source, bytes)]);
        }
    }

    #[test]
    fn markdown_import_creates_bounded_titled_pages() {
        eprintln!("phase portal_markdown_import_creates_a_titled_native_page");
        {
            let (mut app, mut state, tmp, _) = app_seeded_into_doc();
            let source = tmp.path().join("Project brief.md");
            std::fs::write(&source, "# Scope\n\n- Fast\n").unwrap();
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(source));
            app.request_markdown_import().unwrap();
            assert_eq!(recorder.pick_requests()[0].title(), "Import Markdown");
            settle_markdown_io(&mut app, &mut state.doc);
            app.sync(&mut state.doc, &state.root);
            admit_pending_import(&mut app, &mut state);
            assert!(app.transfer_toasts.iter().any(|toast| {
                toast.kind == ui::widget::toast::ToastKind::Success
                    && toast.text.contains("Project brief")
            }));
            let page = app.workspace.selected().expect("import selected its page");
            assert_eq!(app.workspace.get(page).unwrap().title, "Project brief");
            let blocks = NotesDoc::page_block_snapshots(&state.doc, page);
            assert_eq!(
                blocks
                    .iter()
                    .map(|(_, text)| text.as_str())
                    .collect::<Vec<_>>(),
                vec!["Scope", "Fast"]
            );
        }

        eprintln!("phase markdown_import_refuses_oversized_and_non_utf8_files");
        {
            let tmp = TempDir::new().unwrap();
            let oversized = tmp.path().join("oversized.md");
            let file = std::fs::File::create(&oversized).unwrap();
            file.set_len(MAX_MARKDOWN_IMPORT_BYTES + 1).unwrap();
            assert!(read_bounded_markdown(&oversized).is_err());

            let binary = tmp.path().join("binary.md");
            std::fs::write(&binary, [0xff, 0xfe]).unwrap();
            assert!(read_bounded_markdown(&binary).is_err());
        }
    }

    fn start_markdown_receipt(
        app: &mut NotesApp,
        state: &mut ui::State,
        page: Option<PageId>,
        outcome: ui::portal::PortalOutcome,
    ) -> tilo_actions::InvocationId {
        let (resource_id, action_id) = page.map_or_else(
            || ("notes.notebook".into(), "import-markdown"),
            |page| (format!("notes.page:{page}"), "export-markdown"),
        );
        let resource = ui::resources::resource(app, &state.doc, &resource_id).unwrap();
        let spec = resource
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .unwrap();
        let encoded = spec.build_json(&serde_json::Map::new()).unwrap();
        let invocation = tilo_actions::InvocationId::new();
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        recorder.respond_to_pick(outcome);
        assert_eq!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    spec.kind,
                    &encoded.type_id,
                    &encoded.payload
                ),
                &mut state.doc,
                &state.root
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Accepted)
        );
        assert!(
            app.poll_resource_operation(invocation, false, &state.doc, &state.root)
                .is_none(),
            "chooser admission is not success"
        );
        assert_eq!(recorder.pick_requests().len(), 1);
        invocation
    }

    fn take_markdown_receipt(
        app: &mut NotesApp,
        state: &ui::State,
        invocation: tilo_actions::InvocationId,
    ) -> tilo_actions::InvocationUpdate {
        let update = app
            .poll_resource_operation(invocation, false, &state.doc, &state.root)
            .expect("terminal receipt");
        assert!(update.is_terminal());
        assert!(
            app.poll_resource_operation(invocation, false, &state.doc, &state.root)
                .is_none(),
            "terminal result is delivered exactly once"
        );
        update
    }

    #[test]
    fn link_receipts_keep_each_original_job_and_actual_backend_outcome() {
        let (mut app, state, _temp, _) = app_seeded_into_doc();
        let first = tilo_actions::InvocationId::new();
        let second = tilo_actions::InvocationId::new();
        let (first_tx, first_rx) = std::sync::mpsc::channel();
        let (second_tx, second_rx) = std::sync::mpsc::channel();
        app.external_link_opens.push(ExternalLinkJob {
            receiver: first_rx,
            operation: Some(PendingOperation::local(first)),
        });
        app.external_link_opens.push(ExternalLinkJob {
            receiver: second_rx,
            operation: Some(PendingOperation::local(second)),
        });
        app.drain_external_link_opens();
        assert!(
            app.poll_resource_operation(first, false, &state.doc, &state.root)
                .is_none()
        );
        second_tx
            .send(Err("actual backend refused".into()))
            .unwrap();
        app.drain_external_link_opens();
        assert!(
            matches!(take_markdown_receipt(&mut app, &state, second), tilo_actions::InvocationUpdate::Failed { invocation, .. } if invocation == second)
        );
        assert!(
            app.poll_resource_operation(first, false, &state.doc, &state.root)
                .is_none()
        );
        first_tx.send(Ok(())).unwrap();
        app.drain_external_link_opens();
        assert!(
            matches!(take_markdown_receipt(&mut app, &state, first), tilo_actions::InvocationUpdate::Succeeded { invocation, .. } if invocation == first)
        );
        assert!(app.external_link_opens.is_empty());
    }

    #[test]
    fn image_receipt_waits_for_actual_document_edit_and_reports_read_failure() {
        for succeeds in [true, false] {
            let (mut app, mut state, temp, page) = app_seeded_into_doc();
            let source = temp.path().join("insert.png");
            if succeeds {
                std::fs::write(&source, include_bytes!("../../settings/shots/settings.png"))
                    .unwrap();
            }
            let resource =
                ui::resources::resource(&app, &state.doc, &format!("notes.page:{page}")).unwrap();
            let spec = resource
                .actions
                .iter()
                .find(|action| action.id == "insert-image")
                .unwrap();
            let encoded = spec.build_json(&serde_json::Map::new()).unwrap();
            let invocation = tilo_actions::InvocationId::new();
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(source));
            let before = state.doc.save();
            let block_count = NotesDoc::page_block_snapshots(&state.doc, page).len();
            assert_eq!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        spec.kind,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut state.doc,
                    &state.root
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            );
            assert!(
                app.poll_resource_operation(invocation, false, &state.doc, &state.root)
                    .is_none()
            );
            settle_markdown_io(&mut app, &mut state.doc);
            let receipt = take_markdown_receipt(&mut app, &state, invocation);
            assert_eq!(receipt.invocation(), invocation);
            if succeeds {
                assert!(matches!(
                    receipt,
                    tilo_actions::InvocationUpdate::Succeeded { .. }
                ));
                assert_eq!(
                    NotesDoc::page_block_snapshots(&state.doc, page).len(),
                    block_count + 1
                );
            } else {
                assert!(matches!(
                    receipt,
                    tilo_actions::InvocationUpdate::Failed { .. }
                ));
                assert_eq!(state.doc.save(), before);
            }
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn markdown_receipts_report_real_outcomes_and_terminate_exactly_once() {
        eprintln!("phase markdown_receipt_import_succeeds_only_after_canonical_commit");
        {
            let (mut app, mut state, temp, _) = app_seeded_into_doc();
            let source = temp.path().join("Receipt.md");
            std::fs::write(&source, "# Receipt body").unwrap();
            let invocation = start_markdown_receipt(
                &mut app,
                &mut state,
                None,
                ui::portal::PortalOutcome::Opened(source.clone()),
            );
            app.sync(&mut state.doc, &state.root);
            settle_markdown_io(&mut app, &mut state.doc);
            app.sync(&mut state.doc, &state.root);
            assert!(
                app.poll_resource_operation(invocation, false, &state.doc, &state.root)
                    .is_none(),
                "reading a file is not a committed import"
            );
            assert!(
                app.request_markdown_import().is_err(),
                "cannot start another chooser while finalization is pending"
            );
            admit_pending_import(&mut app, &mut state);
            assert!(matches!(
                take_markdown_receipt(&mut app, &state, invocation),
                tilo_actions::InvocationUpdate::Succeeded { .. }
            ));
            let imported = NotesDoc::list_pages(&state.doc)
                .into_iter()
                .find(|page| page.title == "Receipt")
                .unwrap();
            assert_eq!(
                NotesDoc::page_block_snapshots(&state.doc, imported.id)[0].1,
                "Receipt body"
            );
            assert_eq!(std::fs::read_to_string(source).unwrap(), "# Receipt body");
        }

        eprintln!("phase markdown_receipt_launch_and_picker_cannot_overlap");
        {
            let (mut app, mut state, temp, page) = app_seeded_into_doc();
            let source = temp.path().join("Launch.md");
            std::fs::write(&source, "Launch body").unwrap();
            app.queue_launch_markdown(source.to_str().unwrap()).unwrap();
            // A recording host keeps any accidental chooser start hermetic.
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            assert!(app.request_markdown_export(&state.doc, page).is_err());
            assert!(app.request_image_insert(page).is_err());
            assert!(recorder.pick_requests().is_empty());
            assert!(app.markdown_invocation.is_none());
            assert!(app.markdown_transfer.is_none());
            admit_pending_import(&mut app, &mut state);
            assert!(
                app.markdown_results.is_empty(),
                "CLI import must not complete a different invocation"
            );
            let invocation = start_markdown_receipt(
                &mut app,
                &mut state,
                Some(page),
                ui::portal::PortalOutcome::Cancelled,
            );
            assert!(app.queue_launch_markdown(source.to_str().unwrap()).is_err());
            app.sync(&mut state.doc, &state.root);
            assert!(matches!(
                take_markdown_receipt(&mut app, &state, invocation),
                tilo_actions::InvocationUpdate::Cancelled { .. }
            ));
        }

        eprintln!("phase markdown_receipt_export_reports_actual_write_result");
        {
            for succeeds in [true, false] {
                let (mut app, mut state, temp, page) = app_seeded_into_doc();
                let destination = if succeeds {
                    temp.path().join("export.md")
                } else {
                    temp.path().to_path_buf()
                };
                let invocation = start_markdown_receipt(
                    &mut app,
                    &mut state,
                    Some(page),
                    ui::portal::PortalOutcome::Opened(destination.clone()),
                );
                app.sync(&mut state.doc, &state.root);
                settle_markdown_io(&mut app, &mut state.doc);
                app.sync(&mut state.doc, &state.root);
                let update = take_markdown_receipt(&mut app, &state, invocation);
                if succeeds {
                    assert!(matches!(
                        update,
                        tilo_actions::InvocationUpdate::Succeeded { .. }
                    ));
                    assert!(!std::fs::read_to_string(destination).unwrap().is_empty());
                } else {
                    assert!(matches!(
                        update,
                        tilo_actions::InvocationUpdate::Failed { .. }
                    ));
                }
            }
        }

        eprintln!("phase markdown_receipt_finalize_refusal_fails_original_invocation");
        {
            let (mut app, mut state, temp, _) = app_seeded_into_doc();
            let source = temp.path().join("Refused.md");
            std::fs::write(&source, "Private source").unwrap();
            state.doc.commit();
            let before = state.doc.get_heads();
            let invocation = start_markdown_receipt(
                &mut app,
                &mut state,
                None,
                ui::portal::PortalOutcome::Opened(source.clone()),
            );
            app.sync(&mut state.doc, &state.root);
            settle_markdown_io(&mut app, &mut state.doc);
            app.sync(&mut state.doc, &state.root);
            let intent = ui::widget::controls::take_control_invocation(&mut app).unwrap();
            // The separate FrameHarness revocation regression proves this host
            // refusal route. Here assert its original asynchronous receipt too.
            ui::widget::controls::on_control_invocation_result(
                &mut app,
                &intent.address,
                &Err(tilo_actions::DoorError::Unavailable {
                    code: "permission_denied".into(),
                    reason: "Write was revoked".into(),
                }),
            );
            assert!(matches!(
                take_markdown_receipt(&mut app, &state, invocation),
                tilo_actions::InvocationUpdate::Failed { .. }
            ));
            assert!(app.pending_markdown_source.is_none());
            assert!(ui::widget::controls::take_control_invocation(&mut app).is_none());
            assert_eq!(state.doc.get_heads(), before);
            assert_eq!(std::fs::read_to_string(source).unwrap(), "Private source");
        }

        eprintln!("phase markdown_receipt_cancel_failure_and_disconnect_are_terminal");
        {
            for mode in ["cancel", "failure", "disconnect", "invalid-source"] {
                let (mut app, mut state, temp, _) = app_seeded_into_doc();
                state.doc.commit();
                let before = state.doc.get_heads();
                let outcome = match mode {
                    "cancel" | "disconnect" => ui::portal::PortalOutcome::Cancelled,
                    "invalid-source" => {
                        ui::portal::PortalOutcome::Opened(temp.path().join("missing.md"))
                    }
                    _ => ui::portal::PortalOutcome::Failed("Chooser failed".into()),
                };
                let invocation = start_markdown_receipt(&mut app, &mut state, None, outcome);
                if mode == "disconnect" {
                    let (sender, receiver) = std::sync::mpsc::channel();
                    drop(sender);
                    app.markdown_transfer = Some((MarkdownTransfer::Import, receiver));
                }
                app.sync(&mut state.doc, &state.root);
                settle_markdown_io(&mut app, &mut state.doc);
                app.sync(&mut state.doc, &state.root);
                let update = take_markdown_receipt(&mut app, &state, invocation);
                assert_eq!(
                    matches!(update, tilo_actions::InvocationUpdate::Cancelled { .. }),
                    mode == "cancel"
                );
                if mode != "cancel" {
                    assert!(matches!(
                        update,
                        tilo_actions::InvocationUpdate::Failed { .. }
                    ));
                }
                assert_eq!(state.doc.get_heads(), before);
                assert!(app.markdown_invocation.is_none());
                assert!(app.markdown_transfer.is_none());
                assert!(app.pending_markdown_source.is_none());
            }
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn collaborative_rebinds_keep_marks_targets_and_reveal_focus() {
        eprintln!("phase brokered_block_reveal_restores_exact_page_and_editor_focus");
        {
            let (mut app, mut state, _tmp, welcome) = app_seeded_into_doc();
            assert!(
                app.main.editor().blocks().len() > 1,
                "welcome has several blocks"
            );
            let block_id = app.search_blocks[&welcome][1].id.clone();
            let operation = crate::actions::NotesOperation::OpenPage {
                page: welcome,
                block_id: Some(block_id),
            };
            let payload = serde_json::to_vec(&operation).unwrap();

            assert!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        crate::actions::NOTES_OPERATION_TYPE,
                        &payload,
                    ),
                    &mut state.doc,
                    &state.root,
                )
                .expect("broker accepts the exported Notes view operation")
                .is_some()
            );
            app.sync(&mut state.doc, &state.root);

            assert_eq!(app.selected_for_test(), Some(welcome));
            assert_eq!(
                app.main.editor().focused_index(),
                Some(1),
                "the canonical block action reveals the exact editor block"
            );
        }

        eprintln!("phase canonical_inline_marks_rehydrate_after_a_collaborative_rebind");
        {
            let (mut app, mut state, _tmp, welcome) = app_seeded_into_doc();
            let block = app.search_blocks[&welcome][0].id.clone();
            let original = app
                .main
                .editor()
                .blocks()
                .iter()
                .find(|candidate| candidate.id == block)
                .expect("search block belongs to the bound editor")
                .text()
                .to_string();
            assert!(original.len() >= 4);
            let marks = vec![crate::actions::InlineMarkSpan {
                start: 0,
                end: 4,
                mark: crate::actions::InlineMark::Underline,
            }];
            let action = crate::actions::set_mark_action(
                welcome,
                &block,
                0..4,
                &ui::widget::marks::Mark::Underline,
            );
            app.perform_action(&mut state.doc, &action);

            // Simulate a remote structural refresh. The widget rebuilds plain
            // text/kinds, then Notes hydrates the native mark state back into the
            // live rich line rather than dropping formatting on rebind.
            app.bound_page = None;
            app.sync(&mut state.doc, &state.root);
            let restored = app
                .main
                .editor()
                .blocks()
                .iter()
                .find(|candidate| candidate.id == block)
                .expect("same durable block rebinds")
                .line
                .marks()
                .iter()
                .map(crate::actions::InlineMarkSpan::from_ui)
                .collect::<Vec<_>>();
            assert_eq!(restored, marks);
        }

        eprintln!("phase mark_only_dirty_edge_dispatches_the_canonical_action");
        {
            let (mut app, mut state, _tmp, welcome) = app_seeded_into_doc();
            let (block, text_len) = {
                let first = &app.main.editor().blocks()[0];
                (first.id.clone(), first.text().len())
            };
            assert!(text_len >= 4);
            app.main.editor_mut().blocks_mut()[0].line.set_marks(vec![
                ui::widget::marks::MarkSpan::new(0..4, ui::widget::marks::Mark::Underline),
            ]);
            // Formatting changes no plain text; the editor still writes it as
            // the page's own edit.
            app.sync(&mut state.doc, &state.root);
            assert!(
                ui::undo::take_committed_action(&mut app).is_some(),
                "the mark-only edit is an undoable Notes action"
            );
            assert_eq!(
                NotesDoc::page_block_marks(&state.doc, welcome)
                    .into_iter()
                    .find(|(id, _)| id == &block)
                    .map(|(_, marks)| marks),
                Some(vec![crate::actions::InlineMarkSpan {
                    start: 0,
                    end: 4,
                    mark: crate::actions::InlineMark::Underline,
                }])
            );
        }

        eprintln!("phase collaborative_identical_insert_preserves_the_original_block_target");
        {
            use tilo_actions::Action as _;

            let (mut app, mut state, _tmp, welcome) = app_seeded_into_doc();
            let original = app.search_blocks[&welcome][1].clone();
            let old_address = crate::actions::open_block_address(welcome, &original.id);
            let operation = crate::actions::NotesOperation::OpenPage {
                page: welcome,
                block_id: Some(original.id),
            };
            let payload = serde_json::to_vec(&operation).unwrap();

            // A collaborator inserts an identical-text block before the target.
            // Content and index cannot distinguish them; block identity can.
            let last = app.search_blocks[&welcome].last().unwrap().id.clone();
            crate::actions::edit_body_action(
                welcome,
                tilo_document::RichTextAction::InsertParagraph {
                    after: last.clone(),
                    text: original.text,
                },
            )
            .apply(&mut state.doc)
            .unwrap();
            let inserted = NotesDoc::page_block_snapshots(&state.doc, welcome)
                .into_iter()
                .skip_while(|(id, _)| *id != last)
                .nth(1)
                .unwrap()
                .0;
            crate::actions::edit_body_action(
                welcome,
                tilo_document::RichTextAction::MoveBlocks {
                    blocks: vec![inserted],
                    after: None,
                },
            )
            .apply(&mut state.doc)
            .unwrap();
            app.sync(&mut state.doc, &state.root);

            let resolved = ui::resources::resource(&app, &state.doc, &old_address.resource)
                .expect("the original block's resource resolves lazily");
            assert_eq!(resolved.id.0, old_address.resource);
            assert!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        crate::actions::NOTES_OPERATION_TYPE,
                        &payload,
                    ),
                    &mut state.doc,
                    &state.root,
                )
                .expect("durable block identity still resolves")
                .is_some()
            );
            app.sync(&mut state.doc, &state.root);
            assert_eq!(
                app.main.editor().focused_index(),
                Some(2),
                "the original block moved to index 2; the identical insertion at 0 is not revealed"
            );
        }
    }
}

#[cfg(test)]
#[path = "app/continuation_tests.rs"]
mod continuation_tests;
