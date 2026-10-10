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
#[cfg(test)]
use ui::widget::Edits;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, transaction::Transactable};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::collaboration::PresencePayload;
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
use crate::effect::NotesEffect;
use crate::main_panel::{BacklinkChip, MainPanel, TagChip};
use crate::model::{PageId, TagId, Workspace};
use crate::shortcuts_overlay::ShortcutsOverlay;
use crate::sidebar::{OutEvent, PageSidebar, SidebarTransfer};
use crate::status_bar::{SaveStatus, StatusBarState, StatusModel, count_words};

mod effects;

thread_local! {
    /// Whether the notebook's shell presents the window's History control.
    /// Always so in the app.
    static SHELL_PRESENTS_HISTORY: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// The test id of the sidebar header's "New note" button.
pub(crate) const NEW_NOTE_BUTTON_ID: &str = "new-note";
/// The test id of the note header's "Note actions" button.
pub(crate) const NOTE_MENU_BUTTON_ID: &str = "note-menu";

/// Draws notebook images from the notebook's asset snapshot.
fn canonical_image_store(
    assets: &Arc<std::sync::RwLock<HashMap<String, Vec<u8>>>>,
) -> ui::ImageStore {
    let assets = Arc::clone(assets);
    let fetcher: ui::Fetcher =
        Arc::new(move |source: &str| assets.read().ok()?.get(source).cloned());
    // Preserve document detail while allowing several large images to remain
    // resident together in the shared 64 MiB cache.
    ui::ImageStore::with_max_dimension(&fetcher, 2048)
}

type ImportedAssets = Vec<(String, Vec<u8>)>;
/// Document heads, pending operations, selected page and tag filter.
type CanonicalProjectionKey = (Vec<ChangeHash>, usize, Option<PageId>, Option<TagId>);

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

pub(crate) fn adopt_canonical_markdown_assets(
    markdown_path: &std::path::Path,
    markdown: &str,
) -> (String, ImportedAssets) {
    let Some(parent) = markdown_path.parent() else {
        return (markdown.to_string(), Vec::new());
    };
    let canonical_parent = crate::files::canonical(parent).unwrap_or_else(|| parent.to_path_buf());
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
        let Some(candidate) = crate::files::canonical(&parent.join(destination))
            .filter(|candidate| candidate.starts_with(&canonical_parent))
        else {
            output.push_str(segment);
            continue;
        };
        if let Some(source) = imported_paths.get(&candidate) {
            let _ = write!(output, "{indent}![{alt}]({source}){newline}");
            continue;
        }
        let Ok(bytes) = read_image_asset(&candidate) else {
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
    (output, assets.into_iter().collect())
}

/// `markdown` with every notebook image it shows carried inside it, so the
/// file stands alone wherever it is saved.
fn inline_images(markdown: &str, assets: &HashMap<String, Vec<u8>>) -> String {
    use base64::Engine as _;
    let mut output = String::with_capacity(markdown.len());
    for segment in markdown.split_inclusive('\n') {
        let (line, newline) = segment
            .strip_suffix('\n')
            .map_or((segment, ""), |line| (line, "\n"));
        let inlined = standalone_image_parts(line).and_then(|(indent, alt, source)| {
            let bytes = assets
                .get(source)
                .filter(|_| source.starts_with("asset:"))?;
            let media_type = image_media_type(bytes)?;
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
            Some(format!(
                "{indent}![{alt}](data:{media_type};base64,{data}){newline}"
            ))
        });
        output.push_str(inlined.as_deref().unwrap_or(segment));
    }
    output
}

/// `markdown` with the images it carries inside it moved into the notebook
/// as assets, and the assets: what [`inline_images`] wrote comes back as
/// the notebook's own images.
fn adopt_inline_images(markdown: &str) -> (String, ImportedAssets) {
    use base64::Engine as _;
    let mut output = String::with_capacity(markdown.len());
    let mut assets = HashMap::<String, Vec<u8>>::new();
    let mut total = 0_usize;
    for segment in markdown.split_inclusive('\n') {
        let (line, newline) = segment
            .strip_suffix('\n')
            .map_or((segment, ""), |line| (line, "\n"));
        let adopted = standalone_image_parts(line).and_then(|(indent, alt, source)| {
            let (_, data) = source.strip_prefix("data:image/")?.split_once(";base64,")?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .ok()?;
            check_image(&bytes).ok()?;
            let source = format!("asset:{}", blake3::hash(&bytes).to_hex());
            if !assets.contains_key(&source) {
                total = total
                    .checked_add(bytes.len())
                    .filter(|total| *total <= crate::notebook::MAX_ASSET_BYTES)?;
                assets.insert(source.clone(), bytes);
            }
            Some(format!("{indent}![{alt}]({source}){newline}"))
        });
        output.push_str(adopted.as_deref().unwrap_or(segment));
    }
    (output, assets.into_iter().collect())
}

fn read_image_asset(source: &std::path::Path) -> Result<Vec<u8>, String> {
    const MAX_IMAGE_BYTES: u64 = crate::notebook::MAX_ASSET_BYTES as u64;
    let bytes = crate::files::read_bounded(source, MAX_IMAGE_BYTES)
        .map_err(|error| error.to_string())?
        .ok_or("Image is larger than 1 MiB")?;
    check_image(&bytes)?;
    Ok(bytes)
}

/// `bytes` as an image the notebook keeps: as given when it fits an asset,
/// else re-encoded (JPEG, or PNG to keep transparency) and scaled down until
/// it does. A screenshot is often larger than an asset may be, and pasting it
/// must still work.
fn fit_image_asset(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    const MAX: usize = crate::notebook::MAX_ASSET_BYTES;
    image_media_type(&bytes).ok_or("Use a PNG, JPEG, or WebP image")?;
    if bytes.len() <= MAX {
        return Ok(bytes);
    }
    let mut image = image::load_from_memory(&bytes).map_err(|error| error.to_string())?;
    drop(bytes);
    let transparent = image.color().has_alpha();
    for _ in 0..8 {
        let mut encoded = std::io::Cursor::new(Vec::new());
        if transparent {
            image
                .write_to(&mut encoded, image::ImageFormat::Png)
                .map_err(|error| error.to_string())?;
        } else {
            image::DynamicImage::ImageRgb8(image.to_rgb8())
                .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
                    &mut encoded,
                    85,
                ))
                .map_err(|error| error.to_string())?;
        }
        let encoded = encoded.into_inner();
        if encoded.len() <= MAX {
            return Ok(encoded);
        }
        let (width, height) = (image.width(), image.height());
        if width < 64 || height < 64 {
            break;
        }
        image = image.resize(
            width.saturating_mul(3) / 4,
            height.saturating_mul(3) / 4,
            image::imageops::FilterType::Triangle,
        );
    }
    Err("Image is too large to keep in a note".into())
}

/// Whether `bytes` are an image a notebook keeps: a PNG, JPEG or WebP of at
/// most 1 MiB.
pub(crate) fn check_image(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > crate::notebook::MAX_ASSET_BYTES {
        return Err("Image is larger than 1 MiB".into());
    }
    if image_media_type(bytes).is_none() {
        return Err("Choose a PNG, JPEG, or WebP image".into());
    }
    Ok(())
}

/// The media type of image `bytes`, when they are one a notebook keeps.
pub(crate) fn image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
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
    /// The note's save + page/word status, surfaced on the save status
    /// surface (see `save_status`) instead of a permanent strip.
    status: StatusModel,
    /// The framework's save status surface: "Saving…" and "Saved" (with
    /// the note's length) in a transient bubble in the bottom-trailing
    /// corner, shown again on a page switch and while the pointer rests
    /// there; a failed save as an error toast with "Try again".
    save_surface: ui::SaveStatusSurface,
    /// "Try again" was chosen on a failed save; the host drains it.
    save_retry: bool,
    transfer_toasts: ui::widget::toast::ToastStack,
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
    /// The page tree had the keyboard when the shortcut sheet opened, and gets
    /// it back when the sheet closes.
    shortcuts_return_to_tree: bool,
    main_node: Option<NodeId>,
    theme: Theme,
    /// Rect covering [sidebar + main] — the upper "content row" of
    /// the column layout, sibling of the status bar. We don't expose
    /// this through taffy node ids; it's a flex sub-container whose
    /// rect we never need outside of layout. Tracked locally for
    /// completeness.
    /// Last framework-owned durability projection; querying it never touches the record.
    record_save_status: ui::record::RecordSaveStatus,
    canonical_assets: Arc<std::sync::RwLock<HashMap<String, Vec<u8>>>>,
    canonical_asset_manifest: Vec<(String, String)>,
    /// Some referenced image's bytes are still on their way from a
    /// collaborator's device; retried at most once a second.
    canonical_assets_waiting: Option<ui::time::Instant>,
    /// View/legacy callback intents that must wait for a mutable bound record admission.
    pending_events: Vec<OutEvent>,
    /// Last mouse position in logical coords. Used to hit-test
    /// link marks on mouse click without dragging the click event
    /// through every nested widget.
    last_mouse: Point,
    /// The page the `MainPanel` editor is currently bound to in the doc.
    /// `sync` rebinds whenever this drifts from the selected page.
    bound_page: Option<PageId>,
    /// The tree has been laid out once, so the shell's columns are known.
    laid_out: bool,
    /// Forces a redraw when our own chrome changes without any child widget
    /// flagging itself dirty.
    self_dirty: bool,
    paint_damage: ui::widget::paint_damage::PaintDamage,
    /// Inputs of the last canonical projection: document heads, pending
    /// operations, selected page and tag filter. `sync` re-projects the
    /// whole page tree only when one of them moved instead of every frame.
    canonical_synced: Option<CanonicalProjectionKey>,
    /// Observer-only receipt that a canonical projection reached this surface.
    projection_adopted: bool,
    /// Top-bar taffy node — present only while `built_narrow`.
    /// Rect of the narrow-mode top bar, from `apply_layout`.
    /// Hit rect of the drawer toggle (hamburger) button.
    /// Rect of the content area (between top bar and status bar). Used
    /// to place the drawer scrim in narrow mode.
    /// Where the notebook's edits go: the framework's dispatcher, once the
    /// surface is attached. A notebook shown before (or without) it is read
    /// only.
    dispatcher: Option<ui::framework::Dispatcher<crate::resource::Notebook>>,
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
    /// Validated against current text again after the editor binds.
    pending_block_range: Option<(PageId, String, String, usize, usize)>,
    /// Search/resource snapshot keyed by each block's durable Automerge Text
    /// object id, refreshed from the collaborative document on every sync.
    search_blocks: HashMap<PageId, Vec<crate::actions::NoteBlockResource>>,
    /// Local index of canonical incoming `[[Page]]` marks. It is rebuilt only
    /// when the document heads advance, alongside the full-text catalogue;
    /// drawing a selected note therefore never scans a large notebook.
    backlink_index: HashMap<PageId, Vec<(PageId, u32)>>,
    /// Durable document heads the block snapshot and backlinks were taken
    /// at. In the normal no-edit frame this avoids snapshotting every
    /// page/block just to rediscover nothing changed.
    search_heads: Option<Vec<ChangeHash>>,
    /// Last theme text scale, captured during layout. Responsive chrome uses
    /// it for width/height reservations so 200% text switches to the drawer
    /// before page labels or the title stack can collide.
    /// Effective reduced-motion setting provided by the host. Notes owns one
    /// small app-local animation (the transient save-status bubble), so it
    /// retains the value instead of relying on the default child forwarding.
    prepared_local_focus: Option<crate::local_view::PreparedFocus>,
    // The first measured width resolves the saved drawer and Sidebar intent once.
    pending_responsive_view: Option<(bool, bool)>,
    /// Whether the record's changes are on disk, as the framework reports it.
    save_status: Option<ui::LocalState<ui::record::RecordSaveStatus>>,
    /// A note to open once the notebook holds it.
    open_when_present: Option<PageId>,
}

impl NotesApp {
    /// Content-free observation of the actual editor and canonical body.
    /// Both explicit semantic inspection and native input evidence use this.
    fn editor_observation(&self, doc: &AutoCommit) -> serde_json::Value {
        let page = self.main.page_id();
        // Explicit read-only inspection and opt-in input diagnostics observe
        // both canonical text and the editor cache used to draw that frame,
        // without placing a person's note content in the diagnostic log.
        let body = page
            .filter(|id| NotesDoc::page_body(doc, *id).is_some())
            .map(|id| {
                NotesDoc::page_block_snapshots(doc, id)
                    .into_iter()
                    .map(|(_, text)| text)
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        let editor = self.main.editor();
        let rendered = editor
            .blocks()
            .iter()
            .map(|block| block.line.value())
            .collect::<Vec<_>>()
            .join("\n");
        let caret = editor.focused_index().and_then(|index| {
            editor.blocks().get(index).map(|block| {
                serde_json::json!({
                    "block_id": block.id,
                    "block_index": index,
                    "byte_offset": block.line.cursor_pos(),
                })
            })
        });
        let canonical_sync_key = self.canonical_synced.as_ref().map(|key| {
            let mut heads = blake3::Hasher::new();
            for head in &key.0 {
                heads.update(&head.0);
            }
            serde_json::json!({
                "heads_hash": heads.finalize().to_hex().to_string(),
                "heads_count": key.0.len(),
                "pending_operations": key.1,
                "selected_page": key.2,
                "active_tag": key.3,
            })
        });
        let rect = editor.get_rect();
        serde_json::json!({
            "app": "so.tilo.notes",
            "editor_id": "block_editor",
            "selected_page": self.workspace.selected(),
            "bound_page": page,
            "main_page": page,
            "editor_binding_page": self.bound_page,
            "pending_page_events": self.pending_events.len(),
            "adopted": self.projection_adopted,
            "canonical_sync_key": canonical_sync_key,
            "hash_algorithm": "blake3",
            "body_hash": body.as_ref().map(|body| blake3::hash(body.as_bytes()).to_hex().to_string()),
            "body_chars": body.as_ref().map(|body| body.chars().count()),
            "body_bytes": body.as_ref().map(String::len),
            "rendered_body_hash": blake3::hash(rendered.as_bytes()).to_hex().to_string(),
            "rendered_body_chars": rendered.chars().count(),
            "editor_focused": editor.is_focused(),
            "caret": caret,
            "editor_rect": [rect.x0, rect.y0, rect.x1, rect.y1],
        })
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
        let mut app = Self::build_widget(workspace, sidebar, main);
        if let Ok(mut assets) = app.canonical_assets.write() {
            *assets = snapshot;
        }
        app.canonical_assets_waiting = waiting.then(ui::time::now);
        app.canonical_asset_manifest = crate::notebook::asset_manifest(doc)?;
        app.main = app.main.project_canonical(doc, &app.workspace)?;
        app.bound_page = app.workspace.selected();
        app.projection_adopted = true;
        app.refresh_link_pages();
        Ok(app)
    }

    fn project_workspace(doc: &AutoCommit, selected: Option<PageId>) -> Result<Workspace, String> {
        let mut workspace = crate::notebook::project(doc, selected)?;
        if workspace.selected().is_none() {
            let first = workspace
                .pages()
                .iter()
                .filter(|page| page.parent.is_none())
                .min_by(|a, b| crate::model::sibling_order(a, b))
                .map(|page| page.id);
            workspace.set_selected(first);
        }
        Ok(workspace)
    }

    fn build_shell(columns: &Columns, header: &Rc<HeaderRequests>) -> NavShell {
        let new_note = {
            let header = Rc::clone(header);
            Button::icon_only(
                NEW_NOTE_BUTTON_ID,
                ui::icons::Icon::Plus,
                &ui::i18n!("notes-affordance-new-note"),
            )
            .on_view(move || header.new_note.set(true))
        };
        let note_menu = {
            let header = Rc::clone(header);
            Button::icon_only(
                NOTE_MENU_BUTTON_ID,
                ui::icons::Icon::MoreHorizontal,
                &ui::i18n!("notes-affordance-note-actions"),
            )
            .on_view(move || header.note_menu.set(true))
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

    fn build_widget(workspace: Workspace, sidebar: PageSidebar, main: MainPanel) -> Self {
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
            save_surface: ui::SaveStatusSurface::new(),
            save_retry: false,
            transfer_toasts: ui::widget::toast::ToastStack::new().with_max(3),
            shots_save_status: None,
            shortcuts: ShortcutsOverlay::new(),
            rect: Rect::ZERO,
            shell,
            columns,
            header,
            shell_node: None,
            sidebar_node: None,
            shortcuts_return_to_tree: false,
            main_node: None,
            theme: Theme::dark(),
            record_save_status: ui::record::RecordSaveStatus::MemoryOnly,
            canonical_assets: Arc::new(std::sync::RwLock::new(HashMap::new())),
            canonical_asset_manifest: Vec::new(),
            canonical_assets_waiting: None,
            pending_events: Vec::new(),
            last_mouse: Point::ZERO,
            bound_page: None,
            laid_out: false,
            self_dirty: false,
            paint_damage: ui::widget::paint_damage::PaintDamage::default(),
            canonical_synced: None,
            projection_adopted: false,
            dispatcher: None,
            active_tag_filter: None,
            route_history: ui::NavStack::new(initial_route),
            history_target: None,
            pending_block_reveal: None,
            pending_block_range: None,
            search_blocks: HashMap::new(),
            backlink_index: HashMap::new(),
            search_heads: None,
            prepared_local_focus: None,
            pending_responsive_view: None,
            save_status: None,
            open_when_present: None,
        };
        // Images are drawn from the notebook's assets, whoever hosts it.
        app.main
            .set_image_store(Some(canonical_image_store(&app.canonical_assets)));
        app.refresh_link_pages();
        app.main.set_tags(app.selected_tag_chips());
        app.refresh_status();
        app
    }

    /// Recompute the status bar snapshot from the workspace and the record's
    /// save state.
    /// Cheap — called from `handle_event`'s tail and after every
    /// `apply_event`. The status bar diffs internally so a no-op
    /// refresh doesn't dirty its render.
    pub(crate) fn take_save_retry_request(&mut self) -> bool {
        std::mem::take(&mut self.save_retry)
    }

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
        let save_status = self
            .shots_save_status
            .unwrap_or(match self.record_save_status {
                ui::record::RecordSaveStatus::Saved => SaveStatus::Saved,
                ui::record::RecordSaveStatus::Saving => SaveStatus::Saving,
                ui::record::RecordSaveStatus::Failed(_) => SaveStatus::Failed,
                ui::record::RecordSaveStatus::MemoryOnly => SaveStatus::SessionOnly,
            });
        let announce = self.status.state().save_status != save_status
            || self.status.state().page_path != path
            || self.status.state().page_count != self.workspace.pages().len();
        let save_problem = match &self.record_save_status {
            ui::record::RecordSaveStatus::Failed(reason) if self.shots_save_status.is_none() => {
                Some(reason.clone())
            }
            _ => None,
        };
        self.status.set_state(StatusBarState {
            save_status,
            save_problem,
            page_path: path,
            page_count: self.workspace.pages().len(),
            word_count,
        });
        // A failure stays on the surface until the note saves or it is
        // dismissed; "Saving…" and "Saved" pass. Counts stay live while the
        // bubble shows, but a new word must not reopen it.
        let now = ui::time::now();
        self.save_surface.set_detail(self.status_detail());
        self.save_surface.set(self.save_state(), now);
        if announce {
            self.save_surface.poke(now);
        }
    }

    /// The save state the surface reports.
    fn save_state(&self) -> ui::SaveState {
        match self.status.state().save_status {
            SaveStatus::Saved => ui::SaveState::Saved,
            SaveStatus::Saving => ui::SaveState::Saving,
            SaveStatus::SessionOnly => ui::SaveState::Idle,
            // What it means for the person, not the storage error behind it.
            SaveStatus::Failed => ui::SaveState::Failed(ui::i18n!("notes-save-failed-detail")),
        }
    }

    /// What the saved bubble adds: the note's length, where the window
    /// has room for it. A phone keeps the bubble to the save state.
    fn status_detail(&self) -> Option<String> {
        if self.shell.geometry().sidebar != SidebarPlacement::Docked {
            return None;
        }
        self.status.word_text()
    }

    /// `--shots`: stage the status bubble as a change just happened, so a
    /// still frame captures the floating chip.
    pub fn stage_status_bubble(&mut self) {
        self.refresh_status();
        self.save_surface.poke(ui::time::now());
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

    /// Draw the transient floating status bubble at the bottom-right of the
    /// window — the reclaimed home of the old bottom status strip. It shows
    /// on a status change or while the pointer hovers the bottom-right band,
    /// then fades. All colours come from the theme (via `draw_pill`).
    fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = ui::time::now();
        self.save_surface
            .paint(scene, transform, tr, theme, self.rect, now);
    }

    /// A press on the failed save's toast: "Try again" asks the host to
    /// submit the note again, the close button dismisses it. Returns whether
    /// the press was the toast's.
    fn handle_save_status_press(&mut self, event: &WindowEvent, handled: &mut bool) -> bool {
        let WindowEvent::MouseInput {
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        if !self.save_surface.contains(self.last_mouse) {
            return false;
        }
        if self.save_surface.click(self.last_mouse) == Some(ui::SaveAction::Retry) {
            self.save_retry = true;
        }
        self.self_dirty = true;
        *handled = true;
        true
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
        let _profile = ui::tilo_resource::profiling::Phase::start("transclusion_previews");
        self.main
            .set_transclusion_previews(self.transclusion_previews(doc));
    }

    fn transclusion_previews(
        &self,
        doc: &AutoCommit,
    ) -> HashMap<PageId, ui::widget::TransclusionPreview> {
        const EXCERPT_LIMIT: usize = 180;
        // Only a committed projection of these exact heads can replace a
        // document read. Open edits and stale projections still read the
        // actual body, including rollback followed by another edit.
        let projected = doc.pending_ops() == 0
            && self.workspace.canonical_projection_heads.as_ref() == Some(&doc.base_heads());
        let blocks = |page| -> std::borrow::Cow<'_, [actions::StoredBlock]> {
            if projected {
                std::borrow::Cow::Borrowed(
                    self.workspace
                        .bodies
                        .get(&page)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                )
            } else {
                std::borrow::Cow::Owned(NotesDoc::page_stored_blocks(doc, page))
            }
        };
        let selected = self.workspace.selected().map(&blocks).unwrap_or_default();
        let targets = selected
            .iter()
            .flat_map(|block| &block.marks)
            .filter_map(|span| match &span.mark {
                actions::InlineMark::Transclusion { page } => page.parse::<PageId>().ok(),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        targets
            .into_iter()
            .filter_map(|id| self.workspace.get(id))
            .map(|page| {
                let body = blocks(page.id);
                let raw = body
                    .iter()
                    .map(|block| block.text.as_str())
                    .find(|text| !text.trim().is_empty())
                    .unwrap_or("Empty note");
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
            .collect()
    }

    /// Keep each image pasted into the open note with the notebook and put it
    /// after the caret, as a chosen image is.
    fn drain_pasted_images(&mut self, edits: &mut ui::widget::Edits<'_>) {
        for request in self.main.take_pasted_images() {
            let Some(page) = self.main.page_id() else {
                continue;
            };
            let result = fit_image_asset(request.image.bytes).and_then(|bytes| {
                let source = format!("asset:{}", blake3::hash(&bytes).to_hex());
                // Replaces the selection the paste was made over, or goes
                // after the block the caret was in, as one action.
                let edit = self
                    .main
                    .body_editor_mut()
                    .image_paste_action(
                        edits.doc(),
                        &request.target,
                        format!("![Pasted image]({source})"),
                    )
                    .map_err(|error| error.to_string())?;
                self.dispatch(edits, &actions::add_image_asset_action(page, bytes, edit))
            });
            match result {
                Ok(()) => {
                    self.bound_page = None;
                    self.self_dirty = true;
                }
                Err(error) => self.transfer_toasts.push(
                    format!("Image paste failed: {error}"),
                    ui::widget::toast::ToastKind::Error,
                ),
            }
        }
    }

    /// Insert the image `bytes`, described as `alt`, into the open note.
    fn insert_image_bytes(&mut self, edits: &mut ui::widget::Edits<'_>, alt: &str, bytes: Vec<u8>) {
        let Some(page) = self.main.page_id() else {
            return;
        };
        let inserted = fit_image_asset(bytes)
            .and_then(|bytes| self.dispatch(edits, &self.image_insertion(page, alt, bytes)));
        match inserted {
            Ok(()) => {
                self.bound_page = None;
                self.self_dirty = true;
            }
            Err(error) => {
                tracing::warn!(%error, "notes: image not inserted");
                self.transfer_toasts.push(
                    format!("Image import failed: {error}"),
                    ui::widget::toast::ToastKind::Error,
                );
            }
        }
    }

    /// Image files dropped on the window go into the open note.
    fn handle_dropped_file(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let WindowEvent::DroppedFile(path) = event else {
            return false;
        };
        if self.main.page_id().is_none() {
            return false;
        }
        let bytes = crate::files::read_bounded(path, ui::clipboard::MAX_PASTED_IMAGE_BYTES as u64)
            .map_err(|error| error.to_string())
            .and_then(|bytes| bytes.ok_or_else(|| "Image is too large".to_owned()));
        match bytes {
            Ok(bytes) => {
                let alt = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("Image")
                    .replace([']', '[', '(', ')'], " ");
                self.insert_image_bytes(&mut ctx.edits(), alt.trim(), bytes);
            }
            Err(error) => self.transfer_toasts.push(
                format!("Image import failed: {error}"),
                ui::widget::toast::ToastKind::Error,
            ),
        }
        *ctx.handled = true;
        true
    }

    fn drain_transclusion_opens(&mut self) {
        while let Some(page) = self.main.take_transclusion_open() {
            if let Err(error) = self.open_note(page) {
                tracing::warn!(%error, %page, "notes: embedded page target went stale");
            }
        }
        while let Some(url) = self.main.take_external_link_open() {
            self.request(&NotesEffect::OpenLink { url });
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

    /// Open or close the shared NavShell sidebar drawer.
    fn set_drawer_open(&mut self, open: bool) {
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

    /// Ensure a page is selected and the `MainPanel` editor is bound to
    /// that page's `blocks` List in the shared doc, so its text syncs.
    fn ensure_editor_bound(&mut self, state: &AutoCommit) {
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
    fn bind_selected_page(&mut self, state: &AutoCommit, sel: PageId) {
        if NotesDoc::page_body(state, sel).is_none() {
            return; // page not in the shared doc yet — wait for sync
        }
        let title = self
            .workspace
            .get(sel)
            .map(|m| m.title.clone())
            .unwrap_or_default();
        match self.main.bind_canonical_page(state, sel, &title) {
            Ok(()) => {
                self.bound_page = Some(sel);
                self.refresh_link_pages();
            }
            Err(error) => tracing::warn!(%error, "notes: page bind refused"),
        }
    }

    /// Set the local user's id for authorship attribution. The same id
    /// the binary derives from `QUICK_APP_ID` and uses for presence, so
    /// authored text and the peer's cursor share a color.
    pub fn set_local_author(&mut self, author: Option<String>) {
        self.main.set_local_author(author);
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

    /// Apply one [`NotesAction`] through the framework's dispatcher, the one
    /// door every affordance, the palette and agents share: checked against
    /// the participant's permissions, applied by the replayable executor,
    /// attributed, one undo step and named in history.
    fn dispatch(
        &self,
        edits: &mut ui::widget::Edits<'_>,
        action: &NotesAction,
    ) -> Result<(), String> {
        let dispatcher = self
            .dispatcher
            .as_ref()
            .ok_or("this notebook is open to view only")?;
        dispatcher
            .apply_now_with(edits, action, actions::history_context(action))
            .map_err(|error| error.to_string())
    }

    /// Test-only: apply what this notebook submitted to `doc`, as the host
    /// applies it after a pass.
    #[cfg(test)]
    pub(crate) fn land(&self, doc: &mut AutoCommit) {
        if let Some(dispatcher) = &self.dispatcher {
            ui::framework::testing::resolve(dispatcher, doc).expect("submitted edits apply");
        }
    }

    /// Test-only: one frame as a host runs it — sync, the settle pass and
    /// what it submitted applied to `doc` — shown in the same frame.
    #[cfg(test)]
    pub(crate) fn frame(&mut self, doc: &mut AutoCommit, root: &automerge::ObjId) {
        ui::widget::Widget::settle(self, &mut Edits::lend(doc), root);
        ui::widget::Widget::sync(self, doc, root);
        ui::widget::Widget::settle(self, &mut Edits::lend(doc), root);
        self.land(doc);
        ui::widget::Widget::sync(self, doc, root);
    }

    /// [`Self::dispatch`] for an affordance that has nothing to show for a
    /// refusal but the unchanged notebook.
    fn apply_action(&self, edits: &mut ui::widget::Edits<'_>, action: &NotesAction) {
        if let Err(error) = self.dispatch(edits, action) {
            tracing::warn!(%error, "notes: action refused");
        }
    }

    /// Empty surface for the framework host; [`Self::adopt_resource_state`]
    /// installs the live workspace before the first frame.
    pub(crate) fn empty_surface(
        author: &ui::permissions::UserId,
        save_status: ui::LocalState<ui::record::RecordSaveStatus>,
    ) -> Self {
        let mut sidebar = PageSidebar::new();
        sidebar.sync_from_workspace(&Workspace::default());
        let mut main = MainPanel::new();
        main.set_local_author(Some(author.0.clone()));
        let mut app = Self::build_widget(Workspace::default(), sidebar, main);
        app.follow_save_status(save_status);
        app
    }

    /// Show whether the record's changes are on disk as `status` reports it:
    /// the framework's view of the record writer.
    pub fn follow_save_status(&mut self, status: ui::LocalState<ui::record::RecordSaveStatus>) {
        self.save_status = Some(status);
    }

    /// Open `page` as soon as the notebook holds it: the note a launch
    /// imports, whose action applies after the window is built.
    pub(crate) fn open_when_present(&mut self, page: PageId) {
        self.open_when_present = Some(page);
    }

    /// Stage a screenshot scenario: open the note it shows, with the notes
    /// above it expanded, and the save state it shows.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        let shown = crate::scenario::shown_note(scenario).and_then(|title| {
            self.workspace
                .pages()
                .iter()
                .find(|page| page.title == title)
                .map(|page| page.id)
        });
        if let Some(page) = shown {
            let mut ancestors = Vec::new();
            let mut current = page;
            while let Some(parent) = self.workspace.get(current).and_then(|meta| meta.parent) {
                ancestors.push(parent);
                current = parent;
            }
            self.sidebar
                .restore_expanded_pages(&ancestors, &self.workspace);
            self.navigate_to(page);
        }
        if matches!(scenario, Some("image" | "image_edit")) {
            self.load_images_for_capture();
        }
        match scenario {
            Some("status") => self.set_save_status_for_shots(SaveStatus::Saved),
            Some("saving") => self.set_save_status_for_shots(SaveStatus::Saving),
            Some("save_error") => self.set_save_status_for_shots(SaveStatus::Failed),
            Some("import_success") => self.transfer_toasts.push(
                "Imported Project brief",
                ui::widget::toast::ToastKind::Success,
            ),
            _ => {}
        }
    }

    /// Decode every image the notebook holds before a capture is drawn: a
    /// screenshot is one frame, which an image still decoding would show as
    /// its placeholder.
    fn load_images_for_capture(&mut self) {
        let Some(store) = self.main.image_store().cloned() else {
            return;
        };
        let sources = self
            .canonical_assets
            .read()
            .map(|assets| assets.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for source in &sources {
            let _ = store.get(source);
        }
        store.block_until_idle(std::time::Duration::from_secs(10));
        self.bound_page = None;
        self.self_dirty = true;
    }

    /// Present the framework's workspace projection.
    ///
    /// Which note is open is this person's, not the document's: it stays
    /// open, and its editor stays bound, while the note exists. Rebinding on
    /// every edit would put the caret back at the start after each keystroke.
    pub(crate) fn adopt_resource_state(&mut self, state: &std::rc::Rc<Workspace>) {
        self.projection_adopted = true;
        let selected = self
            .workspace
            .selected()
            .filter(|page| state.get(*page).is_some());
        self.workspace = (**state).clone();
        self.workspace.set_selected(selected);
        if self.bound_page != selected {
            self.bound_page = None;
        }
        if let Some(page) = self.open_when_present
            && self.workspace.get(page).is_some()
        {
            self.open_when_present = None;
            self.navigate_to(page);
        }
        self.sidebar.sync_from_workspace(&self.workspace);
        if let Some(page) = selected.and_then(|id| self.workspace.get(id)) {
            self.main.set_title(Some(page.title.clone()));
        }
        self.refresh_link_pages();
        self.self_dirty = true;
    }

    /// Attach the framework dispatcher when hosted as a surface.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Notebook>,
    ) {
        self.main.attach_notebook(dispatcher.clone());
        self.dispatcher = Some(dispatcher);
    }

    fn refresh_search_blocks(&mut self, doc: &AutoCommit) {
        let _profile = ui::tilo_resource::profiling::Phase::start("search_blocks");
        let heads = doc.base_heads();
        let committed = doc.pending_ops() == 0;
        if committed && self.search_heads.as_ref() == Some(&heads) {
            return;
        }
        let projected =
            committed && self.workspace.canonical_projection_heads.as_ref() == Some(&heads);
        let changed = projected
            .then_some(self.workspace.body_projection_refresh.as_ref())
            .flatten()
            .filter(|refresh| {
                self.search_heads.as_ref() == Some(&refresh.before) && refresh.after == heads
            })
            .map(|refresh| refresh.pages.clone());
        let _delta_profile = changed
            .as_ref()
            .map(|_| ui::tilo_resource::profiling::Phase::start("search_delta_adopted"));
        let pages = changed.unwrap_or_else(|| {
            self.search_blocks.clear();
            self.backlink_index.clear();
            self.workspace.pages().iter().map(|page| page.id).collect()
        });
        let known_pages = self
            .workspace
            .pages()
            .iter()
            .map(|page| page.id)
            .collect::<std::collections::HashSet<_>>();
        let bodies = self.workspace.bodies.clone();
        for source in pages {
            if projected {
                self.index_search_page(
                    source,
                    bodies.get(&source).map(Vec::as_slice).unwrap_or_default(),
                    &known_pages,
                );
            } else {
                // Standalone surfaces and open transactions do not carry a
                // validated projection receipt: read their actual document.
                let _read_profile =
                    ui::tilo_resource::profiling::Phase::start("search_body_decode");
                let stored = NotesDoc::page_stored_blocks(doc, source);
                self.index_search_page(source, &stored, &known_pages);
            }
        }
        self.search_heads = committed.then_some(heads);
    }

    fn index_search_page(
        &mut self,
        source: PageId,
        stored: &[crate::actions::StoredBlock],
        known_pages: &std::collections::HashSet<PageId>,
    ) {
        // Replace this source's outgoing contributions, preserving all other
        // sources and dropping targets whose last contribution disappeared.
        if self.search_blocks.contains_key(&source) {
            self.backlink_index.retain(|_, sources| {
                sources.retain(|(page, _)| *page != source);
                !sources.is_empty()
            });
        }
        let mut blocks = Vec::with_capacity(stored.len());
        for block in stored {
            blocks.push(crate::actions::NoteBlockResource {
                id: block.id.clone(),
                text: block.text.clone(),
            });
            for mark in &block.marks {
                let actions::InlineMark::Link { page } = &mark.mark else {
                    continue;
                };
                let Ok(target) = page.parse::<PageId>() else {
                    continue;
                };
                if target == source || !known_pages.contains(&target) {
                    continue;
                }
                let sources = self.backlink_index.entry(target).or_default();
                match sources.binary_search_by_key(&source, |(page, _)| *page) {
                    Ok(index) => {
                        if let Some((_, count)) = sources.get_mut(index) {
                            *count = count.saturating_add(1);
                        }
                    }
                    Err(index) => sources.insert(index, (source, 1)),
                }
            }
        }
        self.search_blocks.insert(source, blocks);
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
        let _profile = ui::tilo_resource::profiling::Phase::start("reconcile_canonical");
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

    fn apply_canonical_event(
        &mut self,
        edits: &mut ui::widget::Edits<'_>,
        event: OutEvent,
    ) -> Result<(), String> {
        self.reconcile_canonical(edits.doc())?;
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
                self.dispatch(edits, &action)?;
                self.reconcile_canonical(edits.doc())?;
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
                let action = actions::duplicate_subtree_action(edits.doc(), source)?;
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
                self.dispatch(edits, &action)?;
                self.workspace.set_selected(Some(destination));
                self.reconcile_canonical(edits.doc())?;
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
            self.dispatch(edits, &action)?;
        } else {
            self.dispatch_tree_diff(edits, &before, &after);
        }
        // Rejected optimistic metadata is always discarded, including New.
        self.reconcile_canonical(edits.doc())?;
        self.record_current_route();
        self.refresh_status();
        Ok(())
    }

    /// Apply one sidebar [`OutEvent`] to the workspace, then dispatch the
    /// matching [`NotesAction`](crate::actions) so the change lands in the shared
    /// document on the same replayable path the Ctrl+K palette uses.
    fn apply_and_dispatch(&mut self, edits: &mut ui::widget::Edits<'_>, ev: OutEvent) {
        if let Err(error) = self.apply_canonical_event(edits, ev) {
            tracing::warn!(%error, "notes: rejected page action; reconciling");
            let _ = self.reconcile_canonical(edits.doc());
            self.self_dirty = true;
        }
    }

    /// Turn a page-tree change into the matching
    /// action(s). Deletes are dispatched before upserts.
    fn dispatch_tree_diff(
        &self,
        edits: &mut ui::widget::Edits<'_>,
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
                self.apply_action(edits, &actions::delete_page_action(*id));
            }
        }
        // Creates + edits: a page new in `after`, or one whose metadata changed
        // (title / parent / order). Each is a whole-page upsert.
        let mut next = after.iter().collect::<Vec<_>>();
        next.sort_by_key(|(id, _)| **id);
        for (id, meta) in next {
            match before.get(id) {
                Some(old) if old == meta => {}
                _ => self.apply_action(edits, &actions::upsert_page_action(meta)),
            }
        }
    }

    /// Take one [`OutEvent`] from the sidebar: applied with the document on
    /// the next sync.
    fn apply_event(&mut self, ev: OutEvent) {
        self.pending_events.push(ev);
    }

    fn record_current_route(&mut self) {
        let route = self.workspace.selected();
        if self.history_target == Some(route) {
            self.history_target = None;
            return;
        }
        self.route_history.push(route);
    }

    /// Go back or forward to the nearest note that still exists. Whether
    /// that led anywhere.
    fn navigate_history(&mut self, action: ui::NavAction) -> bool {
        loop {
            let Some(route) = self.route_history.apply(action).copied() else {
                return false;
            };
            let Some(page) = route else {
                self.history_target = Some(None);
                self.workspace.set_selected(None);
                self.main.clear();
                self.record_current_route();
                self.self_dirty = true;
                return true;
            };
            if self.workspace.get(page).is_none() {
                // Deleted history entries are skipped, just as a browser skips
                // a no-longer-restorable session route instead of stranding
                // Back/Forward on an invalid destination.
                continue;
            }
            self.history_target = Some(Some(page));
            self.navigate_to(page);
            self.self_dirty = true;
            return true;
        }
    }

    fn handle_route_history_shortcut(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
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
        let changed = self.navigate_history(action);
        *handled = changed;
        changed
    }

    /// Navigate to `id`: select + load the page. Used both by the
    /// Cmd-K palette and by click-on-link events.
    pub fn navigate_to(&mut self, id: PageId) {
        self.open_page(id);
    }

    /// Open note `id` on the next sync, where its body binds.
    fn open_page(&mut self, id: PageId) {
        self.pending_events.push(OutEvent::Select(id));
    }

    // Complete lazy resolution semantics are versioned alongside the actual
    // document/owner witness retained by the SDK. No labels or callback hashes.
    fn notes_invocation_domain(
        &self,
        doc: &AutoCommit,
    ) -> ui::widget::ActionDirectoryInvocationDomain {
        let mut parts = vec![
            ui::clock::local_now()
                .format("%Y-%m-%d")
                .to_string()
                .into_bytes(),
            crate::continuation::optional_page(self.workspace.selected()),
            crate::continuation::optional_page(self.active_tag_filter),
        ];
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

    /// Complete a first-note request from the empty-state button. This runs
    /// from both the normal event path and `sync`, because an accessibility
    /// activation reaches the child widget directly and then asks the host for
    /// a sync. Either route creates the same root page and shared action.
    fn drain_empty_note_request(&mut self, edits: &mut ui::widget::Edits<'_>) -> bool {
        if !self.main.take_empty_action() {
            return false;
        }
        self.apply_and_dispatch(edits, OutEvent::CreateChild { parent: None });
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
            self.main
                .set_image_store(Some(canonical_image_store(&self.canonical_assets)));
            self.self_dirty = true;
        }
    }

    fn drain_title_rename(&mut self, edits: &mut ui::widget::Edits<'_>) {
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
                edits,
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
    fn drain_tag_open(&mut self) {
        let Some(tag) = self.main.take_tag_open() else {
            return;
        };
        let shown = self.workspace.clone();
        if let Err(error) = self.open_tag(&shown, tag) {
            tracing::warn!(%error, %tag, "notes: tag chip target went stale");
        }
    }

    /// Resolve an incoming-link chip through the same canonical open-page
    /// operation as global search and ordinary `[[Page]]` activation. The
    /// relationship row itself remains a local, derived presentation.
    fn drain_backlink_open(&mut self) {
        let Some(page) = self.main.take_backlink_open() else {
            return;
        };
        if let Err(error) = self.open_note(page) {
            tracing::warn!(%error, %page, "notes: backlink source went stale");
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
    fn drain_sidebar_events(&mut self, edits: &mut ui::widget::Edits<'_>) {
        if let Some(transfer) = self.sidebar.take_transfer() {
            self.request(&match transfer {
                SidebarTransfer::ImportMarkdown => NotesEffect::ImportMarkdown,
                SidebarTransfer::CopyLink(page) => NotesEffect::CopyLink { page },
                SidebarTransfer::ExportMarkdown(page) => NotesEffect::ExportMarkdown { page },
                SidebarTransfer::InsertImage(page) => NotesEffect::InsertImage { page },
            });
        }
        let events = self.sidebar.take_events();
        if events.is_empty() {
            return;
        }
        for ev in events {
            self.apply_and_dispatch(edits, ev);
        }
        // After mutation, push the new workspace snapshot back into
        // the sidebar so the tree shape reflects.
        self.sidebar.sync_from_workspace(&self.workspace);
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
    pub fn dispatch_event_for_test(&mut self, edits: &mut ui::widget::Edits<'_>, ev: OutEvent) {
        self.apply_and_dispatch(edits, ev);
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
        self.header_button_rect("nav-shell-sidebar")
            .map_or(Point::ZERO, |rect| rect.center())
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
}

impl ui::application_view::ApplicationViewState for NotesApp {
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
        self.pending_block_range = None;
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
}

/// The action target prefix of the save status surface's buttons.
const SAVE_STATUS_TARGET: &str = "notes.save-status";

impl NotesApp {
    fn finish_block_reveal(&mut self) {
        if self
            .pending_block_range
            .as_ref()
            .is_some_and(|(page, block, text, _, _)| {
                self.search_blocks
                    .get(page)
                    .and_then(|blocks| blocks.iter().find(|current| current.id == *block))
                    .is_none_or(|current| current.text != *text)
            })
        {
            self.pending_block_range = None;
            self.pending_block_reveal = None;
        }
        if let Some((page, block_id)) = self.pending_block_reveal.as_ref()
            && self.bound_page == Some(*page)
            && let Some(index) = self
                .search_blocks
                .get(page)
                .and_then(|blocks| blocks.iter().position(|block| block.id == *block_id))
            && self.main.reveal_block(index)
        {
            self.pending_block_reveal = None;
            if let Some((page, block, text, start, end)) = self.pending_block_range.take()
                && let Some(current) = self
                    .search_blocks
                    .get(&page)
                    .and_then(|blocks| blocks.get(index))
                && current.id == block
                && current.text == text
                && self
                    .main
                    .editor()
                    .blocks()
                    .get(index)
                    .is_some_and(|bound| bound.line.value() == text)
            {
                self.main.editor_mut().select_document_range(
                    ui::widget::DocumentPosition {
                        block: index,
                        byte: start,
                    },
                    ui::widget::DocumentPosition {
                        block: index,
                        byte: end,
                    },
                );
            }
        }
    }
}

impl Widget for NotesApp {
    ui::provides!(
        dyn ui::resources::PublishesResources,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::application_view::ApplicationViewState
    );

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.main.primary_vertical_scroll()
    }
    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        self.main.apply_primary_vertical_scroll(action)
    }

    /// What the sidebar, the title field, a paste or the empty state asked
    /// for since the last frame, applied through the notebook's actions.
    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &automerge::ObjId) {
        self.drain_title_rename(edits);
        for event in std::mem::take(&mut self.pending_events) {
            self.apply_and_dispatch(edits, event);
        }
        self.drain_sidebar_events(edits);
        self.drain_pasted_images(edits);
        self.drain_empty_note_request(edits);
        self.main.settle(edits, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        // A scheduled sync only presents when update reports a change. Mark
        // the expired chip dirty so reduced motion still removes it; paint
        // consumes its deadline, leaving an idle window asleep afterward.
        let feedback_expired = self
            .save_surface
            .next_wake_deadline()
            .is_some_and(|deadline| deadline <= ui::time::now());
        self.self_dirty |= feedback_expired;
        // The retained shell owns queued header views, responsive bindings
        // and navigation. It participates in the same lifecycle as the
        // separately projected notebook content in its column slots.
        self.shell.update(state, path) || feedback_expired
    }

    fn native_input_snapshot(
        &self,
        state: &AppModel,
        path: &ModelPath,
    ) -> Option<serde_json::Value> {
        let (doc, _) = ui::doc::document_ref(state, path);
        Some(self.editor_observation(doc))
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let _profile = ui::tilo_resource::profiling::Phase::start("surface_sync");
        self.shell.sync(state, path);
        self.drain_header_requests();
        let (state, path) = ui::doc::document_ref(state, path);
        if let Some(saved) = self.save_status.as_ref().map(ui::LocalState::get)
            && saved != self.record_save_status
        {
            self.record_save_status = saved;
            self.refresh_status();
        }
        self.refresh_canonical_assets(state);
        // Moving to another note writes nothing, so it is taken up here, in
        // the frame that asked; edits wait for the settle pass.
        let mut pending = std::mem::take(&mut self.pending_events);
        pending.retain(|event| match event {
            OutEvent::Select(id) => {
                if self.workspace.get(*id).is_some() {
                    self.workspace.set_selected(Some(*id));
                    self.record_current_route();
                }
                false
            }
            _ => true,
        });
        self.pending_events = pending;
        // Project the workspace only when its inputs moved: committed
        // heads, an open transaction (a settle pass may have written),
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
            let adopted =
                pending == 0 && self.workspace.canonical_projection_heads.as_ref() == Some(&key.0);
            let _profile = adopted.then(|| {
                ui::tilo_resource::profiling::Phase::start("canonical_projection_adopted")
            });
            if !adopted && let Err(error) = self.reconcile_canonical(state) {
                tracing::warn!(%error, "notes: canonical projection refused");
                return;
            }
            self.canonical_synced = Some(key);
            self.self_dirty = true;
        }
        self.record_current_route();
        self.ensure_editor_bound(state);
        self.drain_tag_open();
        self.drain_backlink_open();
        self.main.set_tags(self.selected_tag_chips());
        self.refresh_transclusion_previews(state);
        self.sidebar.sync(state, path);
        self.main.sync(state, path);
        self.drain_transclusion_opens();
        self.refresh_search_blocks(state);
        self.main.set_backlinks(self.selected_backlink_chips());
        self.finish_block_reveal();
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

        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[shell_node], node_map, self)
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
        self.rect = ui::widget::placed(layout, origin);
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
                self.set_drawer_open(open);
            }
            if sidebar {
                self.sidebar.set_focus(self.shell.is_sidebar_open());
            }
        }
        if let Some(n) = self.main_node {
            self.main.set_bottom_inset(
                ui::status_bubble::pill_height(&self.theme)
                    + f64::from(self.theme.tokens.spacing.s4)
                    + f64::from(self.theme.tokens.spacing.rhythm.group),
            );
            self.main
                .apply_layout(taffy, n, self.columns.content.rect().origin());
        }
        if let Some(n) = self.sidebar_node {
            self.sidebar
                .apply_layout(taffy, n, self.columns.sidebar.rect().origin());
        }
        self.note_menu.set_viewport(Some(self.rect));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let incoming = std::mem::take(scene);
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
        let main_scene = std::mem::take(scene);
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
        let outside = std::mem::take(scene);
        let mut main_damage = Vec::new();
        self.main.collect_damage(&mut main_damage);
        self.paint_damage.record(
            self.rect,
            (!self.main.has_editor_popup()).then(|| {
                ui::widget::paint_damage::PaintFrame::new(
                    vec![self.rect, self.main.get_rect()],
                    transform,
                    theme,
                    outside.clone(),
                    vec![ui::widget::paint_damage::PaintPart::new(
                        "note-main".into(),
                        self.main.get_rect(),
                        main_scene.clone(),
                        main_damage,
                    )],
                )
            }),
        );
        *scene = incoming;
        scene.append(&main_scene, None);
        scene.append(&outside, None);
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming only while the floating status bubble is lingering
        // / fading, so it hides itself without needing a stray pointer event.
        // Idle otherwise (no idle redraw spin).
        (!self.theme.reduce_motion && self.save_surface.needs_frame(ui::time::now()))
            || self.shell.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let mut earliest = self.save_surface.next_wake_deadline();
        for child in self.children() {
            if let Some(deadline) = child.next_wake_deadline() {
                earliest = Some(earliest.map_or(deadline, |old| old.min(deadline)));
            }
        }
        earliest
    }

    fn has_pending_visual_work(&self) -> bool {
        self.children()
            .iter()
            .any(|child| child.has_pending_visual_work())
    }

    fn has_transient_feedback(&self) -> bool {
        self.save_surface.next_wake_deadline().is_some()
            || self
                .children()
                .iter()
                .any(|child| child.has_transient_feedback())
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
        if self.handle_help_key(event, ctx) {
            return;
        }
        if self.handle_find_shortcut(event, *ctx.modifiers, ctx.focus_target, ctx.handled) {
            return;
        }
        if self.handle_new_child_shortcut(event, *ctx.modifiers, &mut ctx.state, ctx.handled) {
            return;
        }
        if self.handle_route_history_shortcut(event, *ctx.modifiers, ctx.handled) {
            return;
        }
        if self.handle_save_status_press(event, ctx.handled) {
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
        if self.handle_link_click(event, ctx.handled) {
            return;
        }
        if self.handle_dropped_file(event, ctx) {
            return;
        }
        self.route_to_children(event, ctx);
    }

    fn view_only_scroll(&self, event: &WindowEvent) -> bool {
        matches!(event, WindowEvent::MouseWheel { .. })
            && self.main.view_only_scroll(event)
            && !self.note_menu.is_open()
            && !self.shortcuts.is_visible()
            && !self.sidebar.menu_is_open()
            && !self.sidebar.is_renaming()
            && self.pending_events.is_empty()
            && !(self.shell.geometry().sidebar == SidebarPlacement::Drawer
                && self.shell.is_sidebar_open())
    }

    fn name(&self) -> &'static str {
        "NotesApp"
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ui::widget::ModelPath,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        match self.save_surface.perform(SAVE_STATUS_TARGET, target) {
            Some(ui::SaveAction::Retry) => self.save_retry = true,
            Some(ui::SaveAction::Dismiss) => {}
            None => return false,
        }
        self.self_dirty = true;
        true
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let mut children = vec![
            AccessNode::new(Role::StaticText)
                .with_name(self.status.save_text())
                .with_description(if self.status.needs_attention() {
                    self.status.save_detail()
                } else {
                    self.status.right_text()
                })
                .with_live(ui::a11y::Politeness::Polite)
                .with_hashed_key("notes.save-status"),
        ];
        children.extend(self.save_surface.accessibility(SAVE_STATUS_TARGET));
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

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() || self.paint_damage.is_recorded() {
            self.paint_damage.collect(self.rect, regions);
        }
    }

    fn release_scene_cache(&mut self) {
        self.paint_damage.release_scene_cache();
        for child in self.children_mut() {
            child.release_scene_cache();
        }
    }

    fn mark_drawn(&mut self) {
        self.paint_damage.mark_drawn();
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

impl ui::resources::PublishesResources for NotesApp {
    /// The notebook is the framework root's publication; the surface adds
    /// nothing beside it.
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        Vec::new()
    }
    fn action_directory_invocation_domain(
        &self,
        doc: &AutoCommit,
    ) -> ui::widget::ActionDirectoryInvocationDomain {
        self.notes_invocation_domain(doc)
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
        if !self.shortcuts.is_visible() && std::mem::take(&mut self.shortcuts_return_to_tree) {
            self.sidebar.set_focus(true);
        }
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
    fn handle_help_key(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
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
        // The sheet takes the pointer and the keyboard from the page tree
        // behind it, so no row stays hovered or ringed under the scrim.
        if let Some(node) = self.sidebar_node {
            let left = WindowEvent::CursorLeft {
                device_id: winit::event::DeviceId::dummy(),
            };
            self.sidebar
                .handle_event(&left, &mut ctx.reborrow(ctx.path, node));
        }
        self.shortcuts_return_to_tree = self.sidebar.is_focused();
        self.sidebar.set_focus(false);
        self.shortcuts.set_visible(true);
        *ctx.handled = true;
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
        edits: &mut ui::widget::Edits<'_>,
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
        self.apply_and_dispatch(edits, OutEvent::CreateChild { parent });
        self.sidebar.sync_from_workspace(&self.workspace);
        *handled = true;
        true
    }

    /// What the shell's header buttons asked for: a new root note, or the
    /// note's actions anchored to their button. A button can be pressed by
    /// pointer, keyboard or assistive technology, so this runs from `sync`
    /// as well as after input.
    fn drain_header_requests(&mut self) {
        if self.header.new_note.take() {
            self.pending_events
                .push(OutEvent::CreateChild { parent: None });
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
        self.drain_header_requests();
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
        self.drain_sidebar_events(&mut ctx.state);
        if self.shell.geometry().sidebar == SidebarPlacement::Drawer
            && self.workspace.selected() != selected
            && !self.sidebar.menu_is_open()
        {
            self.set_drawer_open(false);
        }
    }

    /// Click on a link mark navigates.
    fn handle_link_click(&mut self, event: &WindowEvent, handled: &mut bool) -> bool {
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
                self.request(&NotesEffect::OpenLink { url });
                *handled = true;
                return true;
            }
        };
        if let Err(error) = self.open_note(id) {
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
                self.apply_note_menu_selection(index, &mut ctx.state);
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

    fn apply_note_menu_selection(&mut self, index: usize, state: &mut ui::widget::Edits<'_>) {
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
        match index {
            0 => self.apply_and_dispatch(state, OutEvent::CreateChild { parent: Some(page) }),
            1 => self.main.begin_title_edit(),
            2 => self.apply_and_dispatch(state, OutEvent::Duplicate(page)),
            3 => self.request(&NotesEffect::CopyLink { page }),
            4 => self.request(&NotesEffect::ExportMarkdown { page }),
            5 => self.request(&NotesEffect::InsertImage { page }),
            7 => self.apply_and_dispatch(state, OutEvent::Delete(page)),
            _ => {}
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
            self.drain_sidebar_events(&mut ctx.edits());
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
        // A committed edit of the note's in-page title renames the current
        // page on the same replayable action path the sidebar's inline rename
        // uses, then re-syncs the sidebar so the new name shows in the tree.
        self.drain_title_rename(&mut ctx.edits());
        // A fresh workspace has no selected page, so its central first-use
        // action creates a root note. Route it through the same replayable
        // action path as the sidebar + and Cmd/Ctrl-N; a first note must be
        // just as collaborative and undoable as every later one.
        self.drain_empty_note_request(&mut ctx.edits());
        // Update the last-mouse cache so the next MouseInput knows
        // where to hit-test for link clicks.
        if let WindowEvent::CursorMoved { position, .. } = event {
            let logical = vello::kurbo::Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            );
            self.last_mouse = logical;
            // Resting on the bubble's corner pins it open; over a failed
            // save's toast its actions lift.
            let (area, theme) = (self.rect, &self.theme);
            ui::text_render::with_layout_renderer(theme.text_scale, |text| {
                self.save_surface
                    .hover_bubble(area, logical, text, theme, ui::time::now())
            });
            if self.save_surface.pointer_moved(logical) {
                self.self_dirty = true;
            }
        }
        // Status bar refresh runs at the tail of every event so word
        // count + save indicator stay current. Cheap — the StatusBar
        // diffs internally.
        self.refresh_status();
    }
}

#[cfg(test)]
mod tests {
    mod search_refresh;
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn capture_draw_preserves_import_notification_at_expiry_boundary() {
        ui::clock::freeze_for_capture();
        let (mut app, _state, _tmp, _) = app_seeded_into_doc();
        // A zero remaining lifetime deterministically exercises the expiry
        // boundary without making CI speed or a real-time sleep the oracle.
        app.transfer_toasts =
            ui::widget::toast::ToastStack::new().with_default_ttl(ui::time::Duration::ZERO);
        app.transfer_toasts.push(
            "Imported Project brief",
            ui::widget::toast::ToastKind::Success,
        );
        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        app.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::default(),
            &PresenceCtx::empty(),
        );
        let node = app
            .transfer_toasts
            .accessibility()
            .expect("the captured import notification remains announced");
        assert_eq!(node.name.as_deref(), Some("Notifications"));
        assert_eq!(node.value.as_deref(), Some("Imported Project brief"));
    }

    fn adopt_notebook_projection(app: &mut NotesApp, doc: &AutoCommit) {
        use ui::tilo_resource::Resource as _;
        let workspace = crate::resource::Notebook::read(doc).unwrap();
        app.adopt_resource_state(&Rc::new(workspace));
    }

    #[test]
    fn unchanged_long_note_preview_probe() {
        let (mut app, mut state, _tmp, _welcome) = app_seeded_into_doc();
        let source = PageId::new_v4();
        let create = actions::append_page_action(source, None, "Long note");
        tilo_actions::apply_local(&mut state.doc, &create, actions::encode(&create)).unwrap();
        let text = (0..180)
            .map(|index| {
                format!(
                    "Paragraph {index}: {}",
                    "Ordinary scrolling words. ".repeat(12)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let blocks = MainPanel::seed_blocks(&text, |_| None);
        let action = actions::write_body_action(source, &blocks);
        tilo_actions::apply_local(&mut state.doc, &action, actions::encode(&action)).unwrap();
        state.doc.commit();
        app.frame(&mut state.doc, &state.root);
        adopt_notebook_projection(&mut app, &state.doc);
        app.workspace.set_selected(Some(source));
        app.frame(&mut state.doc, &state.root);
        assert_eq!(app.workspace.selected(), Some(source));
        assert_eq!(app.workspace.bodies[&source].len(), 180);
        assert_eq!(state.doc.pending_ops(), 0);
        assert_eq!(
            app.workspace.canonical_projection_heads,
            Some(state.doc.base_heads())
        );
        let heads = state.doc.base_heads();
        let scans = tilo_doc::model::ordered_keys_scan_count();
        let began = std::time::Instant::now();
        for _ in 0..100 {
            assert!(app.transclusion_previews(&state.doc).is_empty());
        }
        let elapsed = began.elapsed().as_nanos();
        let scans = tilo_doc::model::ordered_keys_scan_count() - scans;
        assert_eq!(
            scans, 0,
            "unchanged previews must not decode the canonical body again"
        );
        assert_eq!(state.doc.base_heads(), heads);
        assert_eq!(state.doc.pending_ops(), 0);
        eprintln!(
            "notes-preview-probe {}",
            serde_json::json!({
                "architecture": std::env::consts::ARCH,
                "paragraphs": 180, "iterations": 100,
                "elapsed_ns": elapsed, "container_scans": scans,
                "includes_gpu_or_display": false
            })
        );
    }

    #[test]
    fn transclusion_previews_follow_selected_marks_remote_edits_and_deleted_targets() {
        use tilo_automerge::ApplyIncoming as _;
        let (mut app, mut state, _tmp, source) = app_seeded_into_doc();
        assert!(
            app.transclusion_previews(&state.doc).is_empty(),
            "ordinary notes must not project every other note as a preview"
        );
        let target = PageId::new_v4();
        let mut peer = state.doc.fork();
        let blocks = MainPanel::seed_blocks("Original preview", |_| None);
        let source_block = NotesDoc::page_block_snapshots(&peer, source)[0].clone();
        for action in [
            actions::append_page_action(target, None, "Target"),
            actions::write_body_action(target, &blocks),
            actions::set_mark_action(
                source,
                &source_block.0,
                0..source_block.1.len(),
                &ui::widget::Mark::Transclusion(target),
            ),
        ] {
            tilo_actions::apply_local(&mut peer, &action, actions::encode(&action)).unwrap();
        }
        peer.get_heads();
        state.doc.merge_incoming(&mut peer).unwrap();
        app.frame(&mut state.doc, &state.root);
        adopt_notebook_projection(&mut app, &state.doc);
        let previews = app.transclusion_previews(&state.doc);
        assert_eq!(previews.len(), 1);
        assert_eq!(previews[&target].title, "Target");
        assert_eq!(previews[&target].excerpt, "Original preview");

        let mut peer = state.doc.fork();
        let block = NotesDoc::page_block_snapshots(&peer, target)[0].clone();
        let action = actions::edit_body_action(
            target,
            tilo_document::RichTextAction::SpliceText {
                region: tilo_document::action::RegionKind::Body,
                block: block.0,
                at: 0,
                del: block.1.chars().count(),
                ins: "é".repeat(181),
            },
        );
        tilo_actions::apply_local(&mut peer, &action, actions::encode(&action)).unwrap();
        peer.get_heads();
        state.doc.merge_incoming(&mut peer).unwrap();
        assert_eq!(
            app.transclusion_previews(&state.doc)[&target].excerpt,
            format!("{}…", "é".repeat(180)),
            "a stale projection must read the collaborator's current body"
        );
        app.frame(&mut state.doc, &state.root);
        adopt_notebook_projection(&mut app, &state.doc);
        let scans = tilo_doc::model::ordered_keys_scan_count();
        assert_eq!(
            app.transclusion_previews(&state.doc)[&target].excerpt,
            format!("{}…", "é".repeat(180))
        );
        assert_eq!(tilo_doc::model::ordered_keys_scan_count(), scans);

        assert_open_preview_edits_and_rollback(&app, &mut state.doc, target);

        app.workspace.set_selected(Some(target));
        app.frame(&mut state.doc, &state.root);
        assert!(app.transclusion_previews(&state.doc).is_empty());
        app.workspace.set_selected(Some(source));
        app.frame(&mut state.doc, &state.root);
        assert_eq!(app.transclusion_previews(&state.doc).len(), 1);
        let action = actions::delete_page_action(target);
        tilo_actions::apply_local(&mut state.doc, &action, actions::encode(&action)).unwrap();
        state.doc.get_heads();
        app.frame(&mut state.doc, &state.root);
        assert!(app.transclusion_previews(&state.doc).is_empty());
    }

    fn assert_open_preview_edits_and_rollback(
        app: &NotesApp,
        doc: &mut AutoCommit,
        target: PageId,
    ) {
        let block = NotesDoc::page_block_snapshots(doc, target)[0].0.clone();
        let mut pending_counts = Vec::new();
        for replacement in ["one", "two"] {
            NotesDoc::edit_page_body(
                doc,
                target,
                &tilo_document::RichTextAction::SpliceText {
                    region: tilo_document::action::RegionKind::Body,
                    block: block.clone(),
                    at: 0,
                    del: 181,
                    ins: replacement.into(),
                },
            )
            .unwrap();
            assert!(doc.pending_ops() > 0);
            pending_counts.push(doc.pending_ops());
            assert_eq!(app.transclusion_previews(doc)[&target].excerpt, replacement);
            doc.rollback();
            assert_eq!(
                app.transclusion_previews(doc)[&target].excerpt,
                format!("{}…", "é".repeat(180))
            );
        }
        assert_eq!(
            pending_counts[0], pending_counts[1],
            "replacement transactions share an operation count"
        );
    }

    #[test]
    fn capture_draw_preserves_the_import_notification_lifetime() {
        ui::clock::freeze_for_capture();
        let (mut app, _state, _tmp, _) = app_seeded_into_doc();
        app.transfer_toasts.push(
            "Imported Project brief",
            ui::widget::toast::ToastKind::Success,
        );
        let lifetime = app.transfer_toasts.iter().next().unwrap().ttl_remaining;
        app.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::default(),
            &PresenceCtx::default(),
        );
        assert_eq!(
            app.transfer_toasts.iter().next().unwrap().ttl_remaining,
            lifetime,
            "the real Notes draw must preserve a capture's transient notification"
        );
        assert_eq!(
            app.transfer_toasts
                .accessibility()
                .unwrap()
                .value
                .as_deref(),
            Some("Imported Project brief")
        );
    }

    #[test]
    fn published_block_range_selects_unicode_in_the_real_editor_without_editing() {
        use tilo_document::{RichText, RichTextAction};
        use ui::tilo_resource::Invoke;
        let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
        app.call("new-page", serde_json::json!({"value": "Range fixture"}))
            .unwrap();
        let page = app.state().pages()[0].id;
        app.invoke_child::<RichText>(
            &actions::page_body_id(page),
            Invoke::apply(&RichTextAction::AppendParagraph {
                text: "café notes".into(),
            })
            .unwrap(),
        )
        .unwrap();
        app.settle();
        let block = app.read_document(|doc| {
            NotesDoc::page_block_snapshots(doc, page)
                .into_iter()
                .find(|(_, text)| text == "café notes")
                .unwrap()
                .0
        });
        let resource = actions::block_resource_id(page, &block);
        let before = app.read_document(|doc| (doc.base_heads(), doc.pending_ops()));
        for (start, end, unit) in [(3, 4, "Chars"), (3, 5, "Bytes")] {
            app.call_on(
                &resource,
                "reveal-block-range",
                serde_json::json!({
                    "start": start, "end": end, "unit": unit,
                }),
            )
            .unwrap();
            app.settle();
            assert_eq!(
                app.surface_mut::<NotesApp>()
                    .main
                    .editor()
                    .selected_text()
                    .as_deref(),
                Some("é")
            );
            assert_eq!(
                app.read_document(|doc| (doc.base_heads(), doc.pending_ops())),
                before
            );
        }
        for (start, end, unit) in [
            (4, 3, "Chars"),
            (4, 5, "Bytes"),
            (0, 100, "Chars"),
            (0, 1, "Pixels"),
        ] {
            assert!(
                app.call_on(
                    &resource,
                    "reveal-block-range",
                    serde_json::json!({
                        "start": start, "end": end, "unit": unit,
                    })
                )
                .is_err()
            );
            assert_eq!(
                app.surface_mut::<NotesApp>()
                    .main
                    .editor()
                    .selected_text()
                    .as_deref(),
                Some("é")
            );
        }
        assert_eq!(
            app.read_document(|doc| (doc.base_heads(), doc.pending_ops())),
            before
        );
    }

    #[test]
    fn a_block_edit_before_binding_discards_the_pending_range() {
        let (mut app, mut state, _tmp, page) = app_seeded_into_doc();
        let block = app.search_blocks[&page][0].id.clone();
        let shown =
            <crate::resource::Notebook as ui::tilo_resource::Resource>::read(&state.doc).unwrap();
        app.open_block_range(&shown, page, block.clone(), (0, 1, "Chars"))
            .unwrap();
        let action = actions::edit_body_action(
            page,
            tilo_document::RichTextAction::SpliceText {
                region: tilo_document::action::RegionKind::Body,
                block,
                at: 0,
                del: 0,
                ins: "changed before bind ".into(),
            },
        );
        app.apply_action(&mut Edits::lend(&mut state.doc), &action);
        app.frame(&mut state.doc, &state.root);
        assert!(app.pending_block_range.is_none());
        assert!(
            app.main.editor().selected_text().is_none(),
            "never select offsets against different text"
        );
    }

    /// The dispatcher the framework attaches to the notebook surface, for a
    /// test that drives `NotesApp` over a document of its own.
    fn framework_dispatcher() -> ui::framework::Dispatcher<crate::resource::Notebook> {
        let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
        app.surface_mut::<NotesApp>()
            .dispatcher
            .clone()
            .expect("the framework attaches its dispatcher")
    }

    /// The notebook surface over `doc`, editing through the framework's
    /// dispatcher.
    fn hosted(doc: &AutoCommit) -> NotesApp {
        let mut app = NotesApp::from_document(
            doc,
            &ui::permissions::UserId("notes-app-test-author".into()),
        )
        .unwrap();
        app.attach_framework(framework_dispatcher());
        app
    }

    /// The published notebook resource `id`, as the framework root resolves it
    /// from the notebook's state.
    fn published(doc: &AutoCommit, id: &str) -> tilo_actions::Resource {
        use ui::tilo_resource::Resource as _;
        let state = crate::resource::Notebook::read(doc).unwrap();
        if id == "notes.notebook" {
            return ui::tilo_resource::publish::<crate::resource::Notebook>(id, "Notes", &state);
        }
        crate::actions::notebook_child(&state, id).unwrap()
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn images_beside_a_markdown_file_become_notebook_assets() {
        eprintln!("phase images_beside_a_launched_file_join_the_notebook");
        {
            let source = TempDir::new().unwrap();
            let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/settings.png"));
            std::fs::write(source.path().join("photo.png"), bytes).unwrap();
            std::fs::write(source.path().join("outside.png"), bytes).unwrap();
            let markdown = "![Portable](photo.png)\n![Kept](https://example.test/a.png)\n";
            let (imported, assets) =
                adopt_canonical_markdown_assets(&source.path().join("Imported.md"), markdown);
            let asset = format!("asset:{}", blake3::hash(bytes).to_hex());
            assert_eq!(
                imported,
                format!("![Portable]({asset})\n![Kept](https://example.test/a.png)\n")
            );
            assert_eq!(assets, vec![(asset, bytes.to_vec())]);
        }

        eprintln!("phase repeated_markdown_images_only_consume_the_unique_asset_budget_once");
        {
            let tmp = TempDir::new().unwrap();
            let image = tmp.path().join("image.png");
            std::fs::write(
                &image,
                include_bytes!(concat!(env!("OUT_DIR"), "/settings.png")),
            )
            .unwrap();
            let markdown = "![Repeated](image.png)\n".repeat(10);

            let (imported, assets) =
                adopt_canonical_markdown_assets(&tmp.path().join("note.md"), &markdown);

            assert_eq!(assets.len(), 1);
            let source = &assets[0].0;
            assert_eq!(imported, format!("![Repeated]({source})\n").repeat(10));
        }
    }

    #[test]
    fn a_hosted_notebook_draws_its_images() {
        ui::i18n::I18nManager::init("en-US");
        let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
        let page = PageId::new_v4();
        let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/settings.png")).to_vec();
        let source = format!("asset:{}", blake3::hash(&bytes).to_hex());
        for action in [
            actions::new_page_action(page, None, "a".into(), "Shots"),
            actions::add_image_asset_action(
                page,
                bytes,
                tilo_document::RichTextAction::AppendParagraph {
                    text: format!("![Settings]({source})"),
                },
            ),
        ] {
            app.invoke(ui::tilo_resource::Invoke::apply(&action).unwrap())
                .unwrap();
        }
        app.settle();
        let store = app
            .surface(|notes: &mut NotesApp| notes.main.image_store().cloned())
            .unwrap()
            .expect("the hosted editor draws images from the notebook");
        let _ = store.get(&source);
        store.block_until_idle(tilo_test_env::hang_bound());
        assert!(store.get(&source).is_some(), "the image decodes");
    }

    #[test]
    fn the_empty_notebook_creates_its_first_note_from_the_empty_state() {
        eprintln!("phase accessibility_activation_creates_the_first_shared_note_on_sync");
        {
            let mut doc = AutoCommit::new();
            crate::notebook::initialize_empty(&mut doc).unwrap();
            let mut app = hosted(&doc);
            assert!(
                app.workspace.pages().is_empty(),
                "a new notebook starts empty"
            );
            assert!(app.main.perform_action(
                &ui::a11y::Action::Activate,
                &mut Edits::lend(&mut doc),
                &automerge::ROOT,
            ));

            app.frame(&mut doc, &automerge::ROOT);
            assert_eq!(app.workspace.pages().len(), 1);
            assert!(app.workspace.selected().is_some());
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn page_events_create_delete_rename_and_dispatch_canonical_actions() {
        eprintln!("phase apply_event_create_child_appends_and_selects");
        {
            let (mut app, mut state, _tmp, root) = app_seeded_into_doc();
            app.dispatch_event_for_test(
                &mut Edits::lend(&mut state.doc),
                OutEvent::CreateChild { parent: Some(root) },
            );
            let children = app.workspace.children_of(Some(root));
            assert_eq!(children.len(), 1);
            assert_eq!(app.workspace.selected(), Some(children[0].id));
        }

        eprintln!("phase delete_removes_the_page_and_resets_selection");
        {
            let (mut app, mut state, _tmp, root) = app_seeded_into_doc();
            app.dispatch_event_for_test(
                &mut Edits::lend(&mut state.doc),
                OutEvent::CreateChild { parent: Some(root) },
            );
            let child = app.workspace.selected().unwrap();
            assert!(NotesDoc::page_body(&state.doc, child).is_some());
            app.dispatch_event_for_test(&mut Edits::lend(&mut state.doc), OutEvent::Delete(child));
            app.frame(&mut state.doc, &state.root);
            assert!(NotesDoc::page_body(&state.doc, child).is_none());
            // Selection falls back to a remaining root page.
            assert_eq!(app.workspace.selected(), Some(root));
        }

        eprintln!("phase rename_updates_main_panel_title");
        {
            let (mut app, mut state, _tmp, root) = app_seeded_into_doc();
            app.dispatch_event_for_test(
                &mut Edits::lend(&mut state.doc),
                OutEvent::Rename {
                    id: root,
                    new_title: "Index".into(),
                },
            );
            assert_eq!(app.main.title(), Some("Index"));
        }

        eprintln!("phase sidebar_delete_and_palette_encode_identically");
        {
            let (mut app, mut state, _tmp, welcome) = app_seeded_into_doc();

            // Palette side: the welcome page resource's delete spec (built before we
            // delete the page, while it still exists in the tree).
            let notebook = published(&state.doc, "notes.notebook");
            let page = notebook
                .children
                .iter()
                .find(|r| r.label == "Welcome")
                .expect("a Welcome page resource");
            let delete_spec = page.actions.iter().find(|a| a.id == "delete").unwrap();
            let from_palette = match &delete_spec.input {
                tilo_actions::ActionInput::Immediate(enc) => enc.as_ref().clone(),
                tilo_actions::ActionInput::Prompted { .. }
                | tilo_actions::ActionInput::Deferred { .. } => {
                    panic!("delete is an immediate action")
                }
            };

            // Affordance side: the sidebar's Delete event through the app path,
            // as the change it recorded names it.
            let before = state.doc.get_heads();
            app.dispatch_event_for_test(
                &mut Edits::lend(&mut state.doc),
                OutEvent::Delete(welcome),
            );
            let change = state.doc.get_changes(&before).into_iter().last().unwrap();
            let context =
                tilo_actions::ActionContext::from_change_message(change.message().unwrap())
                    .unwrap();
            let from_button = crate::actions::encode(&actions::delete_page_action(welcome));
            assert_eq!(context.action_label, "Delete note");

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

            app.dispatch_event_for_test(
                &mut Edits::lend(&mut state.doc),
                OutEvent::CreateChild { parent: None },
            );

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
    }

    /// A retry must recover the actual record writer, not leave a latch for
    /// the test to consume after the production owner already took it.
    fn assert_real_save_retry(expected_target: &str) {
        use ui::tilo_resource::Resource as _;

        let profile = TempDir::new().unwrap();
        let mut host =
            ui::prelude::TestApp::<crate::NotesShell>::open_data_dir(profile.path()).unwrap();
        host.flush().unwrap();
        let directory = profile
            .path()
            .join("so.tilo.notes")
            .join("rooms")
            .join("personal")
            .join("records")
            .join(host.record().storage_key());
        assert!(directory.is_dir(), "observe the real record directory");
        let backup = profile.path().join("held-record");
        std::fs::rename(&directory, &backup).unwrap();
        // A regular file in place of this test's directory makes writes fail
        // for root as well as ordinary users; no permission assumptions.
        std::fs::write(&directory, b"deliberate test storage fault").unwrap();
        let page = uuid::Uuid::new_v4();
        host.invoke(
            ui::tilo_resource::Invoke::apply(&crate::actions::NotesAction::AppendPage {
                id: page.to_string(),
                title: "Retry receipt".into(),
                parent: String::new(),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(host.flush().is_err(), "the actual writer must fail");
        host.settle();
        let hosted_retry = host
            .surface::<NotesApp, _>(|notes| {
                assert!(matches!(
                    notes.record_save_status,
                    ui::record::RecordSaveStatus::Failed(_)
                ));
                let failure = notes
                    .accessibility()
                    .unwrap()
                    .children
                    .into_iter()
                    .find(|node| node.children.len() == 2)
                    .expect("the actual writer failure offers retry");
                failure.children[0]
                    .action_target
                    .clone()
                    .expect("live retry target")
            })
            .unwrap();
        assert_eq!(hosted_retry, expected_target);
        std::fs::remove_file(&directory).unwrap();
        std::fs::rename(&backup, &directory).unwrap();
        host.harness()
            .perform_named(&hosted_retry, &ui::a11y::Action::Activate)
            .unwrap();
        host.settle();
        host.surface::<NotesApp, _>(|notes| {
            assert_eq!(
                notes.record_save_status,
                ui::record::RecordSaveStatus::Saved,
                "the real owner must acknowledge the repaired writer"
            );
            assert!(!notes.save_retry, "the owner consumed the request");
            assert!(notes.save_surface.failure().is_none());
        })
        .unwrap();
        assert!(!host.take_save_retry().unwrap(), "no second retry remains");
        let bytes = std::fs::read(directory.join("state.automerge")).unwrap();
        let persisted = automerge::AutoCommit::load(&bytes).unwrap();
        let notebook = crate::resource::Notebook::read(&persisted).unwrap();
        assert!(
            notebook.pages().iter().any(|meta| meta.id == page),
            "the retry must make the failed edit durable on disk"
        );
    }

    #[test]
    fn status_reports_live_word_counts_persistent_failures_and_calm_motion() {
        eprintln!(
            "phase word_count_updates_without_reopening_status_and_save_failure_stays_visible"
        );
        {
            let (mut app, _state, _tmp, _) = app_seeded_into_doc();
            app.shots_save_status = Some(SaveStatus::Saved);
            app.refresh_status();
            app.main
                .editor_mut()
                .set_blocks(vec![ui::widget::Block::paragraph("One two three")]);
            app.refresh_status();
            assert_eq!(app.status.state().word_count, Some(3));
            assert_eq!(app.save_surface.failure(), None);

            app.shots_save_status = Some(SaveStatus::Failed);
            app.refresh_status();
            app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            app.draw_status_bubble(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::default(),
            );
            assert!(
                app.save_surface.failure().is_some(),
                "an unresolved save failure stays on the surface"
            );
            assert!(
                !app.save_surface.needs_frame(ui::time::now()),
                "persistent status must not cause an idle redraw loop"
            );
            let node = app
                .accessibility()
                .unwrap()
                .children
                .into_iter()
                .find(|node| node.children.len() == 2)
                .expect("the failure is announced with its actions");
            let retry = node.children[0]
                .action_target
                .clone()
                .expect("Try again is a target");
            assert_real_save_retry(&retry);

            app.shots_save_status = Some(SaveStatus::Saved);
            app.refresh_status();
            assert_eq!(
                app.save_surface.failure(),
                None,
                "the failure goes away once the note saves"
            );
        }

        eprintln!("phase reduced_motion_stops_notes_owned_status_bubble_frames");
        {
            let (mut app, state, _tmp, _) = app_seeded_into_doc();
            app.theme.reduce_motion = false;
            app.shots_save_status = Some(SaveStatus::Saved);
            app.refresh_status();
            app.save_surface.poke(ui::time::now());
            assert!(app.needs_animation_frame(), "normal fade requests frames");

            app.theme.reduce_motion = true;
            assert!(
                !app.needs_animation_frame(),
                "reduced motion must not keep the compositor awake for a decorative fade"
            );
            assert!(
                app.next_wake_deadline().is_some(),
                "the reduced-motion chip must still schedule its removal"
            );
            assert!(
                app.has_transient_feedback(),
                "scene comparisons must observe the transient chip"
            );
            assert!(
                !app.has_pending_visual_work(),
                "captures can include save feedback without waiting out its lifetime"
            );
            assert_expired_save_feedback_presents(&mut app, &state);
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

    fn assert_expired_save_feedback_presents(app: &mut NotesApp, state: &ui::State) {
        app.save_surface.poke(
            ui::time::now()
                .checked_sub(ui::time::Duration::from_secs(3))
                .expect("the test clock can age the transient chip"),
        );
        app.mark_drawn();
        assert!(!app.self_dirty);
        assert!(
            app.update(&state.doc, &state.root),
            "deadline sync must request the chip-removal frame"
        );
        assert!(app.self_dirty, "the native host must present the change");
        app.draw_status_bubble(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::default(),
        );
        assert!(
            app.save_surface.next_wake_deadline().is_none(),
            "painting the removal consumes the one-shot wake"
        );
    }

    /// A notebook holding one "Welcome" note of several paragraphs, and the
    /// notebook surface over it, editing through the framework's dispatcher.
    fn app_seeded_into_doc() -> (NotesApp, ui::State, TempDir, PageId) {
        let tmp = TempDir::new().unwrap();
        let mut state = ui::State::new();
        crate::notebook::initialize_empty(&mut state.doc).unwrap();
        let welcome = PageId::new_v4();
        let blocks = MainPanel::seed_blocks(
            "Welcome to notes.\n\nA second paragraph to read.\n\nA third one, last.",
            |_| None,
        );
        for action in [
            actions::new_page_action(welcome, None, "a".into(), "Welcome"),
            actions::write_body_action(welcome, &blocks),
        ] {
            tilo_actions::apply_local(&mut state.doc, &action, actions::encode(&action)).unwrap();
        }
        state.doc.commit();
        let mut app = hosted(&state.doc);
        app.frame(&mut state.doc, &state.root);
        assert_eq!(app.selected_for_test(), Some(welcome));
        (app, state, tmp, welcome)
    }

    fn assert_search_snapshot_matches_canonical(app: &NotesApp, doc: &AutoCommit) {
        assert_eq!(app.search_blocks.len(), app.workspace.pages().len());
        for page in app.workspace.pages() {
            let actual = app.search_blocks[&page.id]
                .iter()
                .map(|block| (block.id.clone(), block.text.clone()))
                .collect::<Vec<_>>();
            assert_eq!(actual, NotesDoc::page_block_snapshots(doc, page.id));
        }
    }

    #[test]
    fn canonical_open_refuses_missing_pages_and_blocks_before_projection_adoption() {
        use ui::tilo_resource::Resource as _;
        let (mut app, mut state, _tmp, page) = app_seeded_into_doc();
        let current = crate::resource::Notebook::read(&state.doc).unwrap();
        let pending = app.pending_events.len();
        assert_eq!(
            app.open_block(&current, page, Some("missing-block".into())),
            Err("the block no longer exists".into())
        );
        assert_eq!(app.pending_events.len(), pending);
        let delete = actions::delete_page_action(page);
        tilo_actions::apply_local(&mut state.doc, &delete, actions::encode(&delete)).unwrap();
        let current = crate::resource::Notebook::read(&state.doc).unwrap();
        assert!(current.get(page).is_none());
        assert!(
            app.workspace.get(page).is_some(),
            "the prior presentation still contains the page"
        );
        assert_eq!(
            app.open_block(&current, page, None),
            Err("the note no longer exists".into())
        );
        assert_eq!(
            app.pending_events.len(),
            pending,
            "refused Open cannot queue a selection"
        );
    }

    #[test]
    fn search_snapshot_tracks_remote_rich_edits_and_removed_link_targets() {
        use tilo_automerge::ApplyIncoming as _;
        let (mut app, mut state, _tmp, source) = app_seeded_into_doc();
        let target = PageId::new_v4();
        let absent = PageId::new_v4();
        let mut peer = state.doc.fork();
        let create = actions::new_page_action(target, None, "z".into(), "Destination");
        tilo_actions::apply_local_by(&mut peer, &create, actions::encode(&create), Some(b"peer"))
            .unwrap();
        let source_blocks = NotesDoc::page_block_snapshots(&peer, source);
        for (block, range, link) in [
            (&source_blocks[0].0, 0..4, target),
            (&source_blocks[0].0, 5..7, target),
            (&source_blocks[0].0, 8..10, source),
            (&source_blocks[1].0, 0..4, absent),
        ] {
            let action =
                actions::set_mark_action(source, block, range, &ui::widget::Mark::Link(link));
            tilo_actions::apply_local_by(
                &mut peer,
                &action,
                actions::encode(&action),
                Some(b"peer"),
            )
            .unwrap();
        }
        state.doc.merge_incoming(&mut peer).unwrap();
        let rich_before = NotesDoc::page_stored_blocks(&state.doc, source);
        app.frame(&mut state.doc, &state.root);
        assert_search_snapshot_matches_canonical(&app, &state.doc);
        assert_eq!(app.backlink_index.get(&target), Some(&vec![(source, 2)]));
        assert!(!app.backlink_index.contains_key(&source));
        assert!(!app.backlink_index.contains_key(&absent));
        assert_eq!(
            NotesDoc::page_stored_blocks(&state.doc, source),
            rich_before
        );

        let mut peer = state.doc.fork();
        let delete = actions::delete_page_action(target);
        tilo_actions::apply_local_by(&mut peer, &delete, actions::encode(&delete), Some(b"peer"))
            .unwrap();
        state.doc.merge_incoming(&mut peer).unwrap();
        app.frame(&mut state.doc, &state.root);
        assert_search_snapshot_matches_canonical(&app, &state.doc);
        assert!(!app.search_blocks.contains_key(&target));
        assert!(!app.backlink_index.contains_key(&target));
        assert_eq!(
            NotesDoc::page_stored_blocks(&state.doc, source),
            rich_before
        );
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn navigation_and_backlinks_follow_stable_page_identity() {
        eprintln!("phase canonical_links_produce_sorted_backlinks_for_the_target_note");
        {
            let (mut app, mut state, _tmp, target) = app_seeded_into_doc();
            app.dispatch_event_for_test(
                &mut Edits::lend(&mut state.doc),
                OutEvent::CreateChild { parent: None },
            );
            let source = app.selected_for_test().expect("new source page selected");
            app.dispatch_event_for_test(
                &mut Edits::lend(&mut state.doc),
                OutEvent::Rename {
                    id: source,
                    new_title: "Project index".into(),
                },
            );
            let block = NotesDoc::page_block_snapshots(&state.doc, source)[0]
                .0
                .clone();
            app.apply_action(
                &mut Edits::lend(&mut state.doc),
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
            app.apply_action(
                &mut Edits::lend(&mut state.doc),
                &actions::set_mark_action(source, &block, 0..7, &ui::widget::Mark::Link(target)),
            );
            let backlinks = actions::canonical_backlinks(
                &actions::read_bodies(&state.doc, app.workspace.pages()),
                app.workspace.pages(),
                target,
            );
            assert_eq!(backlinks.len(), 1);
            assert_eq!(backlinks[0].source, source);
            assert_eq!(backlinks[0].source_title, "Project index");
            assert_eq!(backlinks[0].target, target);
            assert_eq!(backlinks[0].excerpt, "Welcome");
        }

        eprintln!("phase native_link_marks_build_a_cached_backlink_and_open_its_canonical_source");
        {
            let (mut app, mut state, _tmp, source) = app_seeded_into_doc();

            // Create the destination through the normal page action path, then
            // attach a real native link mark to an existing source block. This is
            // intentionally not a Markdown-parser test: incoming links must be
            // derived from the same canonical mark data collaborators receive.
            app.dispatch_event_for_test(
                &mut Edits::lend(&mut state.doc),
                OutEvent::CreateChild { parent: None },
            );
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
            app.apply_action(&mut Edits::lend(&mut state.doc), &action);

            app.navigate_to(target);
            app.frame(&mut state.doc, &state.root);
            assert_eq!(
                app.selected_backlink_chips(),
                vec![BacklinkChip {
                    page: source,
                    title: "Welcome".into(),
                    references: 1,
                }],
                "the selected page projects its incoming native link once"
            );

            app.open_note(source)
                .expect("a backlink opens its source note");
            app.frame(&mut state.doc, &state.root);
            assert_eq!(app.selected_for_test(), Some(source));
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn exported_notes_carry_their_images_and_import_them_again() {
        eprintln!("phase an_exported_note_carries_its_images_and_imports_them_again");
        {
            let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/settings.png")).to_vec();
            let source = format!("asset:{}", blake3::hash(&bytes).to_hex());
            let assets = HashMap::from([(source.clone(), bytes.clone())]);
            let markdown = format!("Intro\n\n![Diagram]({source})\n![Elsewhere](asset:missing)\n");

            let exported = inline_images(&markdown, &assets);
            assert!(!exported.contains(&source), "{exported}");
            assert!(exported.contains("![Diagram](data:image/png;base64,"));
            assert!(
                exported.contains("![Elsewhere](asset:missing)"),
                "an image the notebook lacks stays as it was"
            );

            let (imported, imported_assets) = adopt_inline_images(&exported);
            assert_eq!(imported, markdown);
            assert_eq!(imported_assets, vec![(source, bytes)]);
        }

        eprintln!("phase only_images_a_notebook_keeps_are_taken_in");
        {
            let (text, assets) =
                adopt_inline_images("![Script](data:image/svg+xml;base64,PHN2Zz4=)\n");
            assert_eq!(text, "![Script](data:image/svg+xml;base64,PHN2Zz4=)\n");
            assert!(assets.is_empty());
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
            let shown =
                <crate::resource::Notebook as ui::tilo_resource::Resource>::read(&state.doc)
                    .unwrap();
            assert!(
                app.open_block(&shown, welcome, Some("no-such-block".into()))
                    .is_err(),
                "a block that is gone is not revealed"
            );
            app.open_block(&shown, welcome, Some(block_id))
                .expect("the block's note opens");
            app.frame(&mut state.doc, &state.root);

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
            app.apply_action(&mut Edits::lend(&mut state.doc), &action);

            // Simulate a remote structural refresh. The widget rebuilds plain
            // text/kinds, then Notes hydrates the native mark state back into the
            // live rich line rather than dropping formatting on rebind.
            app.bound_page = None;
            app.frame(&mut state.doc, &state.root);
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

        eprintln!("phase rebinding_the_open_note_keeps_the_caret_where_it_was");
        {
            let (mut app, mut state, _tmp, _welcome) = app_seeded_into_doc();
            let (index, block, end) = {
                let editor = app.main.editor();
                let index = editor.blocks().len().saturating_sub(1);
                let block = &editor.blocks()[index];
                (index, block.id.clone(), block.text().len())
            };
            assert!(end > 0);
            app.main
                .editor_mut()
                .focus_block(index, ui::widget::CursorPlacement::End);
            app.bound_page = None;
            app.frame(&mut state.doc, &state.root);
            let editor = app.main.editor();
            let focused = editor
                .focused_index()
                .and_then(|focused| editor.blocks().get(focused))
                .expect("the rebound editor keeps a focused block");
            assert_eq!(focused.id, block, "the caret stays in its block");
            assert_eq!(
                focused.line.cursor_pos(),
                end,
                "the next letter is typed after the last one, not at the start"
            );
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
            let before = state.doc.get_heads();
            app.frame(&mut state.doc, &state.root);
            let changes = state.doc.get_changes(&before);
            assert!(
                changes.iter().any(|change| change
                    .message()
                    .and_then(tilo_actions::ActionContext::from_change_message)
                    .is_some()),
                "the mark-only edit is an attributed, undoable Notes action"
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
            let block = original.id.clone();

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
            app.frame(&mut state.doc, &state.root);

            let resolved = published(&state.doc, &old_address.resource);
            assert_eq!(resolved.id.0, old_address.resource);
            let shown =
                <crate::resource::Notebook as ui::tilo_resource::Resource>::read(&state.doc)
                    .unwrap();
            app.open_block(&shown, welcome, Some(block))
                .expect("durable block identity still resolves");
            app.frame(&mut state.doc, &state.root);
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

impl NotesApp {
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
}
