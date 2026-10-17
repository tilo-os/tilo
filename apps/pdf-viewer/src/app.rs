//! `PdfApp` — the root widget for the PDF reader.
//!
//! It is a single full-window widget that manages its own internal layout
//! (toolbar, thumbnail rail, page canvas, comments/outline/signatures
//! sidebar, status bar, search strip) and draws each region itself. This
//! gives pixel control over the reader chrome, mirroring the custom-draw
//! approach the toolkit uses for things like the status bar, while keeping
//! all durable markup — annotations, comments, signatures — in the shared
//! CRDT so collaboration and undo come for free.

use std::collections::{HashMap, HashSet};
use ui::chrome::{NavCount, NavRowFace};
use ui::icons::Icon;
use ui::tokens::TextRole;
#[cfg(test)]
use ui::widget::Edits;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill, ImageData as PenikoImage};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use std::time::Duration;

use crate::actions::PdfEffect;
use ui::Widget;
use ui::gesture::{DragSelect, Granularity};
use ui::paint::PaintCtx;
use ui::portal::{
    ContentsChannel, ContentsOutcome, FileFilter, PickRequest, PortalChannel, PortalOutcome,
};

use ui::chrome::SelectionState;
use ui::presence::PresenceCtx;
use ui::surface::Region;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::page_sidebar::{PageSidebar, PageSidebarEvent};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::tooltip::Tooltip;
use ui::widget::{
    AvatarFace, AvatarSize, Button, ButtonFace, ButtonRole, CardFace, ColorSwatch,
    ColorSwatchPicker, ContextMenu as SharedContextMenu, ControlSize, DEFAULT_PADDING,
    DEFAULT_PAN_STEP as KEY_SCROLL_STEP, EmptyState, EmptyStateContent, ListRowFace, MenuItem,
    PAN_SLOP, RowAction, RowLeading, Segment, SegmentedFace, SegmentedStyle, SmallContentAlign,
    TextInput, ToggleGroup, ViewportState,
};

use crate::markup::{PagePlanEntry, PdfMarkup};
use crate::model::{
    self, AnnoKind, Annotation, Comment, Document, MARGIN, Match, PAGE_H, PAGE_W, PtRect,
    SearchOptions, Signature, SignatureInk, Swatch, Tool,
};
use crate::render;
use crate::source::{
    ImportRequest, OpenRequest, RasterDocument, RasterEvent, RasterHandle, ReconcileRequest,
    RenderRequest, SaveRequest, TextRequest,
};
use crate::text_layer::{PageText, TextMatch};
use std::sync::Arc;
use tilo_content::{ContentState, ContentStore};
use tilo_rich_text::{Draft, FieldOutcome, RichTextSurface, RichTextView, SubmitKey};

/// Gap between stacked pages, in **content (page-point) space** — it scales
/// with zoom, exactly like the page it separates, because the whole stack is a
/// single [`ViewportState`] content rect.
const PAGE_GAP: f64 = 22.0; // page points between pages, not UI spacing
const ZOOM_MIN: f64 = 0.25;
/// The most pages one text read answers with.
pub(crate) const MOST_READ_PAGES: usize = 50;
/// Zoomed pages are drawn from bounded tiles ([`crate::tiles`]), so the
/// ceiling is about usefulness, not bitmap size.
const ZOOM_MAX: f64 = 16.0;
/// Rasterise pages at this multiple of their on-screen size so they stay
/// crisp when the compositor scales the surface (and when zooming in a notch).
const RASTER_SS: f64 = 2.0;
/// Max number of *new* (uncached) full-page renders a single frame may ask
/// the worker for. A real page can cost tens-to-hundreds of ms, so capping
/// this keeps the open→first-paint path to the single top page; the rest are
/// asked for as results wake later frames. Cached pages are always drawn (a
/// cache hit is ~free), so steady-state scrolling is unaffected.
const MAX_NEW_PAGE_RASTERS_PER_FRAME: usize = 1;
/// A second, UI-side bound after the owner worker's result bound.  This keeps
/// a rapid scroll/zoom sequence from retaining an unbounded history of owned
/// bitmap results in one viewer window.
const MAX_UI_RASTER_IMAGES: usize = 12;

/// One frame's page imagery, keyed by 1-based page: whole-page bitmaps
/// (placeholders under tiles for zoomed pages), rail thumbnails, and the
/// tiles of zoomed pages.
type RasterFrame = (
    HashMap<usize, PenikoImage>,
    HashMap<usize, PenikoImage>,
    HashMap<usize, render::PageTiles>,
);

/// A tile the UI holds: displayed page (0-based), zoom level, column, row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct UiTileKey {
    page: usize,
    level: crate::tiles::Level,
    col: u32,
    row: u32,
}

impl UiTileKey {
    fn of(request: &crate::source::TileRequest) -> Self {
        Self {
            page: request.page,
            level: request.level,
            col: request.col,
            row: request.row,
        }
    }
}

impl crate::tiles::LevelKey for UiTileKey {
    fn level(&self) -> crate::tiles::Level {
        self.level
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchProjectionRevision {
    doc_id: String,
    heads: Vec<ChangeHash>,
    page_ids: Vec<String>,
    page_sources: Vec<usize>,
    search_token: String,
    search_pages: Vec<usize>,
}

fn pdf_filter() -> FileFilter {
    FileFilter::new("PDF documents").mimetype("application/pdf")
}

fn pdf_open_request() -> PickRequest {
    PickRequest::open("Open a PDF", "Open")
        .thread_name("pdf-portal-open")
        .filter(pdf_filter())
        .all_files()
}

fn pdf_import_request() -> PickRequest {
    PickRequest::open("Import pages from a PDF or image", "Import")
        .thread_name("pdf-portal-import")
        .filter(
            FileFilter::new("PDF or image")
                .mimetype("application/pdf")
                .mimetype("image/png")
                .mimetype("image/jpeg"),
        )
        .all_files()
}

fn pdf_save_request(suggested_name: &str) -> PickRequest {
    PickRequest::save("Save annotated PDF", "Save", suggested_name)
        .thread_name("pdf-portal-save")
        .filter(pdf_filter())
}
/// Same budget for rail thumbnails, which are also full engine page renders (a
/// scanned page's thumbnail can cost 15-30ms). A couple per frame keeps the
/// rail filling in briskly without the open frame paying for a whole rail-full.
const MAX_NEW_THUMB_RASTERS_PER_FRAME: usize = 2;
/// How close (page points) a highlight drag must come to a text line before
/// it snaps to text instead of falling back to a free-form rectangle.
const SNAP_NEAR: f64 = 18.0;
/// Screen-px a single arrow-key press scrolls the document.
/// The current UTC date as `YYYY-MM-DD`, read from the system clock. This is
/// the real signing date stamped on a signature the user places at runtime.
#[cfg(test)]
/// The reader's app id, the scope its permissions are decided in.
const APP_ID: &str = "so.tilo.pdf-viewer";

/// Wall-clock unix milliseconds, for stamping authored markup.
fn now_ms() -> i64 {
    i64::try_from(ui::time::unix_now_ms()).unwrap_or(0)
}

/// Today's local date as `YYYY-MM-DD`, through the toolkit clock: the
/// signing date stamped on a signature placed now (pinned in captures).
fn today_ymd() -> String {
    ui::clock::today().format("%Y-%m-%d").to_string()
}

/// The UTC date of a Unix time in milliseconds, as `YYYY-MM-DD`.
pub(crate) fn ymd_from_unix_ms(ms: i64) -> String {
    ymd_from_unix_secs(ms.div_euclid(1000))
}

/// Convert a Unix timestamp (seconds) into a UTC `YYYY-MM-DD` string. Pure,
/// so the date logic is unit-testable without a clock.
fn ymd_from_unix_secs(secs: i64) -> String {
    let (year, m, d) = ui::date::civil_from_days(secs.div_euclid(86_400));
    format!("{year:04}-{m:02}-{d:02}")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarTab {
    Comments,
    Outline,
    Signatures,
}

#[derive(Clone, Copy, PartialEq)]
enum FitMode {
    Page,
    Width,
    Custom(f64),
}

#[derive(Clone)]
struct DragState {
    tool: Tool,
    page: usize,
    start: (f64, f64),
    cur: (f64, f64),
    points: Vec<(f64, f64)>,
}

/// An in-progress canvas pan (drag-to-pan with Select/empty space). Records
/// the screen point and scroll offsets at press so motion maps 1:1.
#[derive(Clone, Copy)]
struct PanState {
    start: Point,
    scroll_x0: f64,
    scroll_y0: f64,
    moved: bool,
}

/// A text selection over the real engine text layer (raster backend only). The
/// anchor is where the drag began; the caret follows the pointer. Both are
/// glyph indices into that page's [`PageText`]; selection stays within one page.
#[derive(Clone, Copy)]
struct TextSelection {
    page: usize,
    anchor: usize,
    caret: usize,
    /// True while the pointer button is held and the drag is live.
    dragging: bool,
}

impl TextSelection {
    /// The selected glyph run as a half-open `[start, end)` index range.
    fn range(&self) -> (usize, usize) {
        let lo = self.anchor.min(self.caret);
        let hi = self.anchor.max(self.caret) + 1;
        (lo, hi)
    }

    /// Whether the selection spans at least one glyph (a bare click selects
    /// nothing).
    fn is_active(&self) -> bool {
        self.anchor != self.caret
    }
}

/// A rubber-band region selection over a page (Shift + drag with the Select
/// tool). On release its pixels are cropped from the page bitmap and copied to
/// the clipboard as an image. Both corners are in page-point space.
#[derive(Clone, Copy)]
struct RegionSelect {
    page: usize,
    start: (f64, f64),
    cur: (f64, f64),
}

impl RegionSelect {
    fn rect(&self) -> PtRect {
        PtRect::from_corners(self.start.0, self.start.1, self.cur.0, self.cur.1)
    }
}

/// The target of a right-click "Delete" context menu.
#[derive(Clone)]
enum DeleteTarget {
    Annotation(String),
    Comment(String),
    Signature(String),
}

struct ContextMenu {
    target: DeleteTarget,
    widget: SharedContextMenu,
}

impl ContextMenu {
    /// A delete menu for `target`, opened against `anchor` (what it acts on,
    /// on screen) inside `viewport`.
    fn delete(target: DeleteTarget, anchor: Rect, viewport: Rect) -> Self {
        let mut widget = SharedContextMenu::new();
        widget.push(MenuItem::destructive_selection("Delete").with_icon(ui::icons::Icon::Trash));
        widget.open_anchored(anchor, viewport);
        Self { target, widget }
    }
}

#[derive(Clone)]
enum ToolbarHit {
    /// A press on the bar that does nothing (a disabled control, a gap).
    Inert,
    /// The zoom stepper's value, edited in place.
    ZoomValue,
    Tool(Tool),
    /// Open the marker-colour popover from its well.
    MarkerColour,
    ZoomIn,
    ZoomOut,
    Fit,
    Search,
    ToggleSidebar,
    ToggleRail,
    Open,
    Save,
}

/// How the user is composing their signature in the Signatures panel: typing
/// their name (rendered as a script), or drawing it by hand on the pad.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SigMode {
    Type,
    Draw,
}

#[derive(Clone)]
enum SidebarHit {
    Tab(SidebarTab),
    Outline(usize),
    Comment(String),
    EditComment(String),
    ReplyField(String),
    Resolve(String),
    DeleteComment(String),
    AddSignature,
    EditSigner,
    SigJump(usize),
    DeleteSignature(String),
    /// Switch the signature composer between typing and drawing.
    SigMode(SigMode),
    /// Clear the drawn signature strokes.
    SigClear,
}

/// What the inline text editor is currently writing into. All three write
/// durable text: a comment/note body and a thread reply land in the shared
/// CRDT; the signer name is the local per-peer identity used for new
/// signatures.
#[derive(Clone, PartialEq)]
enum EditTarget {
    CommentBody(String),
    Reply(String),
    Signer,
    /// A text form field, by fully qualified name.
    FormField(String),
    /// The text of a free text box, by annotation id.
    FreeText(String),
}

/// A form field widget on a displayed page, in card space.
#[derive(Clone, Debug)]
pub(crate) struct FieldWidgetHit {
    pub(crate) name: String,
    pub(crate) kind: tilo_pdf::FieldKind,
    pub(crate) rect: PtRect,
    pub(crate) export: String,
    pub(crate) options: Vec<String>,
    pub(crate) read_only: bool,
    /// The PDF's own value and check state for this widget.
    pub(crate) pdf_value: String,
    pub(crate) pdf_checked: bool,
}

/// A live inline text edit. The text is local until committed, so a burst
/// of keystrokes is a single undo step. `Enter` commits, `Shift+Enter`
/// breaks the line.
struct Editing {
    target: EditTarget,
    input: EditInput,
}

/// What an edit is typed into.
enum EditInput {
    /// A single run of text (a signer's name, a form field, a free text box),
    /// in the shared multiline [`TextInput`].
    Plain(Box<TextInput>),
    /// A comment or reply, written as rich text in the SDK editor.
    Rich(Box<CommentField>),
}

/// The key a comment's (or one of its replies') view is kept under.
fn comment_view_key(comment: &str, reply: Option<&str>) -> String {
    match reply {
        Some(reply) => format!("{comment}\u{1f}{reply}"),
        None => comment.to_owned(),
    }
}

/// The SDK editor a comment or reply is written in.
pub(crate) type CommentField = ui::widget::Embedded<RichTextSurface<Draft>>;

impl Editing {
    /// The current edited text, as plain text.
    fn text(&self) -> String {
        match &self.input {
            EditInput::Plain(input) => input.value().to_owned(),
            EditInput::Rich(field) => field.widget().source().plain_text(),
        }
    }

    /// Whether a press-drag is extending the edit's selection.
    fn is_dragging(&self) -> bool {
        match &self.input {
            EditInput::Plain(input) => input.is_dragging(),
            EditInput::Rich(_) => false,
        }
    }

    /// Whether the person asked to commit (Enter) since the last call.
    fn take_submit(&mut self) -> bool {
        match &mut self.input {
            EditInput::Plain(input) => input.take_submit(),
            EditInput::Rich(field) => {
                field.widget_mut().take_outcome() == Some(FieldOutcome::Submit)
            }
        }
    }

    /// Replace what the edit holds, as if typed (for tests).
    #[cfg(test)]
    fn set_text(&mut self, text: &str) {
        match &mut self.input {
            EditInput::Plain(input) => input.set_text(text),
            EditInput::Rich(field) => {
                field
                    .widget_mut()
                    .source_mut()
                    .reset(&tilo_rich_text::plain_blocks(text));
                field.widget_mut().present_now();
            }
        }
    }

    /// The paragraphs written in a comment or reply, trailing blank ones
    /// dropped.
    fn clips(&self) -> Vec<tilo_document::ClipBlock> {
        let EditInput::Rich(field) = &self.input else {
            return tilo_rich_text::plain_clips(self.text().trim_end());
        };
        let mut clips = field.widget().source().clip_blocks();
        while clips.last().is_some_and(|clip| clip.text.trim().is_empty()) {
            clips.pop();
        }
        clips
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (screenshot/demo-only flags, panel visibility, search options, interaction transients); grouping them would need a real design pass, left for a follow-up rather than forced here"
)]
pub struct PdfApp {
    doc: Document,
    /// The session-scoped endpoint for production `PDFium` work.  The backend is
    /// deliberately owned by its worker; this widget receives only ordinary
    /// documents, bitmaps and text layers.
    raster_handle: Option<RasterHandle>,
    raster_document: Option<RasterDocument>,
    pending_open: Option<OpenRequest>,
    /// Bounded in-flight/cached UI-safe worker results.  Keys are the
    /// zero-based display page and the worker's snapped width bucket.
    raster_images: HashMap<(usize, u32), PenikoImage>,
    raster_render_pending: HashSet<(usize, u32)>,
    /// Tiles of zoomed pages the worker has returned, per zoom level.
    raster_tiles: crate::tiles::TileCache<UiTileKey>,
    /// The tile batch last handed to the worker, most important first;
    /// entries leave it as their results arrive.
    raster_tile_batch: Vec<crate::source::TileRequest>,
    raster_text_pending: HashSet<usize>,
    /// Link areas per displayed page (1-based), in the page's display space;
    /// mapped into card space when hit-tested so they follow the current
    /// page map.
    raster_links: std::collections::HashMap<usize, Arc<Vec<crate::source::PageLink>>>,
    /// Pages (0-based) whose links have been requested from the owner worker.
    raster_links_pending: HashSet<usize>,
    pending_worker_plan: Option<Vec<PagePlanEntry>>,
    applied_worker_plan: Option<Vec<PagePlanEntry>>,
    worker_plan_blobs: Vec<ContentState>,
    pending_save: Option<SaveRequest>,
    pending_import: Option<ImportRequest>,
    /// False until a file has been loaded — drives the empty state.
    has_document: bool,
    /// The PDF the record's markup is made against, as last presented.
    record_source: Option<crate::markup::PinnedSource>,
    /// The record's pinned source was asked for from the content store.
    reopen_requested: bool,
    /// A capture shows the reader opening a document whose open never ends.
    staged_opening: bool,
    /// A capture imports a page on the next sync, where the document is.
    staged_import: bool,
    /// The opened file's display name, shown in the chrome.
    doc_name: String,
    /// A human-readable reason the last open failed, if any.
    load_error: Option<String>,
    /// Imported pages' bytes, referenced from the shared page plan by hash.
    /// Created once with the app and shared with its PDF worker session.
    content: ContentStore,
    /// Screenshot-only: arm a live page-rail reorder drag each frame so a still
    /// frame captures the drag ghost + reflow gap. Never set at runtime.
    shots_rail_drag: bool,
    /// Screenshot-only: pin the floating page bubble open so a still frame shows
    /// the transient page-info overlay. Never set at runtime.
    shots_page_bubble: bool,
    /// Screenshot-only: defer opening the annotation menu until after the
    /// initial CRDT sync has populated its target.
    shots_context_menu: bool,
    shots_panel: Option<SidebarTab>,
    shots_colours: bool,
    shots_selection: bool,
    // CRDT-backed caches (refreshed from the shared document each frame).
    annotations: Vec<Annotation>,
    comments: Vec<Comment>,
    signatures: Vec<Signature>,

    // View state — local / per-peer, never in the shared doc.
    rect: Rect,
    /// The user's fit *intent* (page / width / a pinned custom zoom). The
    /// actual zoom + scroll live in [`self.viewport`]; `fit` only records what
    /// to re-derive when the window resizes and drives the toolbar label.
    fit: FitMode,
    /// Shared zoom/pan kernel (`ui::ViewportState`). Owns the zoom factor,
    /// the scroll offsets, the content↔screen transform and every pan/zoom
    /// reducer, so the reader's zoom behaves exactly like the other document
    /// apps (typst / ebook / maps) instead of a hand-rolled scale. The
    /// multi-page stack is projected into its single content rect: content
    /// width is one page, content height is the whole page stack including
    /// inter-page gaps (see [`Self::content_stack_h`]).
    viewport: ViewportState,
    current_page: usize,
    /// Durable CRDT page-plan identities, refreshed from the markup record.
    resource_page_ids: Vec<String>,
    /// Immutable source page backing each durable plan occurrence.
    resource_page_sources: Vec<usize>,
    /// Imported content metadata parallel to the durable plan; used to build
    /// duplicate actions that retain the imported source identity.
    resource_page_imports: Vec<Option<crate::markup::ImportedPage>>,
    /// Original text-backend pages, used to materialise a remote plan.
    source_pages: Vec<model::Page>,
    /// What the open document says, for search: rebuilt during state sync
    /// when the document or the find results change, never on a keystroke.
    search_entries: Vec<ui::tilo_resource::SearchEntry>,
    search_projection_revision: Option<SearchProjectionRevision>,
    #[cfg(test)]
    search_projection_refreshes: usize,
    tool: Tool,
    swatch: Swatch,
    /// How the highlight tool marks text: highlight, underline or strike-out.
    markup_style: model::MarkupStyle,
    sidebar_tab: SidebarTab,
    show_sidebar: bool,
    /// Thumbnail rail: independently resizable + collapsible, persisted in
    /// view state so it survives across the session.
    show_rail: bool,
    rail_w: f64,
    /// The shared page-preview rail (thumbnails, drag-reorder, context menu,
    /// import). Lives on the left; toggled by the left-most toolbar button.
    page_sidebar: PageSidebar,
    /// Set when the page order / page set has been edited (reorder, delete,
    /// duplicate, import) since the last save — the reader shows a save hint.
    pages_dirty: bool,
    /// An in-flight XDG-portal *import* request (add a PDF/image as pages).
    portal_import_rx: Option<PortalChannel>,
    selected_anno: Option<String>,
    /// Active inline text edit (comment body, thread reply, or signer name).
    editing: Option<Editing>,
    /// Comments and replies as written, one read-only view each, keyed by
    /// [`comment_view_key`]; measured and drawn by the Comments rail.
    comment_views: std::cell::RefCell<HashMap<String, RichTextView>>,
    /// The rich-text view's line inset, measured at a text scale.
    rich_inset: std::cell::Cell<Option<(f32, f64)>>,
    /// The signature whose sidebar row is highlighted/selected (so placing a
    /// signature lands the user on the new entry).
    selected_sig: Option<String>,
    /// Vertical scroll offset of the active sidebar list, so a freshly
    /// created comment/signature can be scrolled into view.
    sidebar_scroll: f64,
    /// When set, the next sidebar draw scrolls this comment/sig id into view
    /// and clears the request.
    sidebar_reveal: Option<String>,
    signer_name: String,
    author: String,

    // Signature composer — the user prepares a real signature (typed name or a
    // hand-drawn one) before stamping it onto a page with the Sign tool.
    /// Whether the composer is in type or draw mode.
    sig_mode: SigMode,
    /// The composed hand-drawn signature: one or more polylines in normalised
    /// `[0,1]²` pad space. Empty until the user draws.
    sig_strokes: Vec<Vec<(f64, f64)>>,
    /// The stroke currently being drawn on the pad (pointer held), if any.
    sig_stroke: Option<Vec<(f64, f64)>>,
    /// The person's own signature record the composer reads and keeps the
    /// signer and drawn strokes in, so they follow the person to their
    /// other devices and never reach a collaborator. Absent where the host
    /// keeps no person records (a capture, a past version).
    signature: Option<ui::framework::PersonRecord<crate::signature::SignatureProfile>>,
    /// The signature record's revision the composer last read.
    signature_revision: Option<u64>,
    /// The draw pad's screen rect, captured during the sidebar draw so a press
    /// inside it starts drawing rather than hitting a button.
    sig_pad_rect: Rect,

    // Search.
    search_active: bool,
    /// The framework find band: the query, its options, the counter and
    /// previous/next. Matches remain derived view state below.
    find_bar: ui::widget::FindBar,
    matches: Vec<Match>,
    active_match: usize,
    search_whole_word: bool,
    search_case_sensitive: bool,
    /// Raster-backend search hits over the engine text layer. Used instead
    /// of `matches` (which searches the bundled sample's block model) when a PDF
    /// is open, so search highlights land on glyph rectangles.
    raster_matches: Vec<TextMatch>,

    // Real PDF text layer (raster backend) — glyph geometry per visible page,
    // refreshed each draw, powering selection + search-hit rectangles. Empty when
    // no PDF is open, where the block model handles both.
    raster_text: std::collections::HashMap<usize, Arc<PageText>>,
    /// A live text selection over the raster text layer, if any.
    selection: Option<TextSelection>,
    /// The shared toolkit selection-gesture core: it owns the multi-click ladder
    /// (single → double-word → triple-line), the timing/anchor rules, and reports
    /// the drag granularity — so the reader never hand-rolls double-click
    /// detection and page text selects exactly like a text field and the ebook.
    drag_select: DragSelect,
    /// The unit the in-flight text-selection drag snaps to (from the click
    /// ladder): character, whole word, or whole visual line.
    sel_gran: Granularity,
    /// The glyph range `[lo, hi)` of the unit the drag anchored on (the
    /// double-clicked word / triple-clicked line). The drag keeps this unit
    /// inside the selection and grows by whole units (see [`ui::extend_span_drag`]).
    sel_anchor_range: (usize, usize),
    /// A live image-region marquee (Shift + Select drag), if any.
    region: Option<RegionSelect>,
    /// Whether Shift is currently held (drives the region-copy marquee).
    shift_down: bool,

    // Save-back.
    /// The on-disk path the current document was opened from, used to suggest a
    /// filename and folder when saving flattened markup.
    source_path: Option<std::path::PathBuf>,
    /// A transient one-line status (e.g. "Saved to …"), shown as a toast until
    /// the next document action.
    status_note: Option<String>,
    /// The last copied region, whose outcome can arrive after its note.
    region_copy: ui::CopyWatch,
    /// An in-flight XDG-portal *save* request (mirrors `portal_rx`).
    portal_save_rx: Option<PortalChannel>,
    /// The open or save effect waiting on a picker, the PDF owner or the
    /// host, answered when it finishes.
    portal_action: Option<ui::framework::EffectReply>,

    // Interaction transients.
    last_mouse: Point,
    drag: Option<DragState>,
    pan: Option<PanState>,
    /// Some(pointer-x-at-press) while dragging the rail's resize grip.
    rail_drag: Option<f64>,
    context_menu: Option<ContextMenu>,
    /// The marker-colour popover, while it is open over the toolbar.
    marker_colours: Option<ui::widget::Popover<ColorSwatchPicker>>,
    /// The window's command row between the pages toggle and the zoom
    /// cluster: Open and Save, the annotation tools, the marker colour and
    /// Fit. What does not fit goes to its More menu.
    toolbar: ui::widget::Toolbar,
    /// The band the toolbar is laid out in (see `recompute_regions`).
    toolbar_band: Rect,
    /// The reader state last carried onto the toolbar (see `sync_toolbar`).
    toolbar_synced: String,
    created_counter: i64,
    dirty: bool,
    /// Visible page/thumbnail cache misses in the last production draw.
    pending_visuals: bool,

    // Per-page text-line geometry (page-point space), cached during draw so
    // the smart highlight tool can snap to text without a TextRenderer.
    text_lines: std::collections::HashMap<usize, Vec<render::TextLine>>,

    // Toolbar tooltip — a single hover-revealed popover; which button it is
    // anchored to is tracked alongside it.
    tooltip: Tooltip,
    tooltip_anchor: Rect,
    /// True only while the pointer sits on a toolbar button and the reveal
    /// timer is still counting down. Drives `needs_animation_frame`, so an
    /// idle window (no hover in progress) settles to zero frames instead of
    /// spinning forever.
    tooltip_pending: bool,
    /// Suppresses timed fades/reveal animation while preserving immediate
    /// state changes for users who request reduced motion.
    reduce_motion: bool,
    /// The theme of the last layout pass. The pure layout helpers shared by
    /// paint, hit-testing and the accessibility tree read their tokens from
    /// it, so geometry and paint agree.
    layout_theme: Theme,
    /// The zoom stepper's width, measured in the draw pass (see
    /// `measure_toolbar`).
    zoom_w: f64,
    /// The review panel's tab widths as their labels need them, full and
    /// short, measured with the toolbar (`None` until then).
    tab_widths: Option<[[f64; 3]; 2]>,
    /// The toolbar's zoom stepper, and whether it was built enabled.
    zoom_input: ui::widget::NumberInput,
    zoom_input_enabled: bool,
    /// The empty state's "Open a PDF…" pill as last painted (see
    /// `empty_open_button_rect`).
    empty_open_button: std::cell::Cell<Rect>,

    /// An in-flight XDG-portal file-open. When the user asks to open a
    /// document (toolbar folder button, Ctrl+O, or the empty state's Open
    /// button) the portal round-trip runs on a worker thread and hands the
    /// chosen path back over this channel, which is drained once per frame in
    /// [`Widget::sync`]. `None` when no picker is in flight — the app never
    /// draws its own picker (HIG §5).
    portal_rx: Option<PortalChannel>,
    /// The same request on a host without paths (a browser page), which
    /// answers with the file's bytes.
    portal_contents_rx: Option<ContentsChannel>,
    /// Opens another document in this window, on that document's own
    /// markup record, in place of the one shown. Absent outside a host.
    open_here: Option<ui::framework::OpenHere>,
    /// The document asked to open in this window's place, until the host
    /// answers.
    pending_reopen: Option<(std::path::PathBuf, ui::application::ReplacementOutcome)>,

    /// When the window is too narrow to reserve space for the side panels,
    /// the rail + sidebar become closed-by-default overlays drawn over the
    /// canvas. Recomputed from the window width each layout.
    overlay_mode: bool,
    /// Tracks overlay-mode transitions so panels can auto-close on the way in
    /// and the sidebar can reopen on the way back to a wide window.
    prev_overlay: Option<bool>,

    // Region rects (computed in apply_layout).
    toolbar_rect: Rect,
    search_rect: Rect,
    rail_rect: Rect,
    canvas_rect: Rect,
    sidebar_rect: Rect,

    /// The floating page-info bubble — a transient overlay (the shared
    /// [`ui::StatusBubble`]) that appears on a page change or while the pointer
    /// hovers the bottom-centre of the canvas, then fades. It replaces the old
    /// always-on bottom status strip so the canvas keeps that vertical space.
    page_bubble: ui::StatusBubble,
    /// The page the bubble last announced, so a page change re-pokes it.
    bubble_shown_page: usize,
    /// The bubble's pill + chevron rects, captured during draw for hover
    /// detection and click hit-testing.
    bubble_rect: Rect,
    bubble_prev: Rect,
    bubble_next: Rect,

    // Sidebar content hit-targets, captured during draw.
    sidebar_hits: Vec<(SidebarHit, Rect)>,
    /// Screen rect of the active inline editor, captured during draw so a
    /// click inside it keeps focus while a click outside commits.
    edit_rect: Rect,

    /// The shared reader selection HUD, rebuilt each frame while a text
    /// selection is live and hit-tested on the next press. The identical bar
    /// the ebook reader draws — Copy / highlight swatches / Note.
    sel_hud: Option<ui::SelectionHud>,
    /// An in-progress drag of a sticky-note marker (reposition), if any.
    marker_drag: Option<ui::MarkerDrag>,

    /// The names of the participants, as the host's roster has them; the
    /// markup records ids. Absent outside a host.
    people: Option<ui::framework::People>,
    /// Where edits go: the resource's dispatcher, attached by the host.
    dispatcher: Option<ui::framework::Dispatcher<crate::markup::PdfMarkup>>,
    /// The participant may only view the document.
    read_only: bool,
    /// How each displayed page maps between the engine's page space, PDF
    /// user space and its card in the stack (rotation and fit included).
    page_maps: Vec<model::PageMap>,
    /// Recorded rotation of each displayed page, parallel to
    /// `resource_page_ids`.
    resource_page_rotations: Vec<i32>,
    /// The open document's form fields (widgets in engine page space).
    form_fields: Vec<tilo_pdf::FormField>,
    /// Form values recorded in the document's record, by field name.
    form_values: std::collections::BTreeMap<String, model::FormEntry>,
    /// Content hash and length of the open document's bytes.
    source_blob: Option<(String, u64)>,
    /// The record's annotations as stored (user space), for export.
    stored_annotations: Vec<Annotation>,
    /// Where the record's taken-over markup came from in the source.
    adopted: Vec<crate::adopt::AdoptedEntry>,
    /// The source hash whose existing markup was asked for, so it is
    /// asked for once.
    existing_request: Option<String>,
    /// The pages, markup and comments asked to be opened (a search hit, an
    /// agent's `open`) since the last frame.
    opens: std::rc::Rc<std::cell::RefCell<Vec<ui::tilo_resource::Id>>>,
    /// The loaded document's record still needs what opening it adds: a
    /// page plan, the pinned source, the markup taken over from the PDF.
    seed_pending: bool,
    /// Whether page images currently leave the document's own markup out.
    reader_markup_hidden: bool,
}

/// The content store an app constructed without one uses, where no host
/// lent the window one: kept in memory.
pub(crate) fn default_content() -> ContentStore {
    ContentStore::memory()
}

impl PdfApp {
    /// Open a real PDF from `path`, rendered by the PDF engine. A failure —
    /// including a host without the engine — lands in the empty state
    /// with the error surfaced, rather than crashing.
    pub fn open(path: impl AsRef<std::path::Path>) -> Self {
        Self::open_with_content(path, default_content())
    }

    /// [`PdfApp::open`], keeping imported pages in `content`.
    pub fn open_with_content(path: impl AsRef<std::path::Path>, content: ContentStore) -> Self {
        // Launch-with-a-file is still a UI entry point.  Queue it through
        // the same session owner as the portal rather than parsing before
        // the first frame; `sync()` installs the matching completion.
        let mut app = Self::empty_with_content(content);
        app.queue_worker_open(path.as_ref());
        app
    }

    /// The no-document state: a clean "open a PDF" surface, no fabricated
    /// content. A record that already marks up a document reopens it.
    pub fn empty() -> Self {
        Self::empty_with_content(default_content())
    }

    /// [`PdfApp::empty`], reading documents from and keeping imported pages
    /// in `content`.
    pub fn empty_with_content(content: ContentStore) -> Self {
        Self::build(
            model::Document::blank_pages("", "", 1, PAGE_W, PAGE_H),
            content,
        )
    }

    /// A past version of the record in History: its markup, without
    /// opening a document, so previewing versions starts no PDF engine.
    pub fn past() -> Self {
        let mut app = Self::empty_with_content(ContentStore::memory());
        app.raster_handle = None;
        app
    }

    /// The bundled demonstration document. Used only by screenshot
    /// scenarios and tests; its markup is the scenario's actions.
    pub fn sample() -> Self {
        Self::sample_with_content(default_content())
    }

    /// [`PdfApp::sample`], keeping imported pages in `content`.
    pub fn sample_with_content(content: ContentStore) -> Self {
        let mut app = Self::build(model::sample_document(), content);
        app.has_document = true;
        app.doc_name = "Tilo Technical Overview.pdf".to_string();
        app.signer_name = crate::scenarios::SIGNER.to_string();
        app
    }

    /// Stage the view state screenshot `scenario` shows, once its actions
    /// have applied: the review the demonstration document lands on, and
    /// the editor, selection, menu or panel the scenario is about.
    pub(crate) fn stage(&mut self, scenario: Option<&str>) {
        if self.has_document {
            // The review lands on the note's comment thread; zoom stays at
            // fit-page so no content is clipped.
            self.selected_anno = Some(crate::scenarios::NOTE.to_string());
            self.sidebar_tab = SidebarTab::Comments;
            self.sidebar_reveal = Some(crate::scenarios::NOTE_COMMENT.to_string());
        }
        match scenario {
            Some("edit-comment") => self.begin_edit(
                EditTarget::CommentBody(crate::scenarios::NOTE_COMMENT.to_string()),
                crate::scenarios::NOTE_COMMENT_BODY,
            ),
            Some("selection-hud") => self.arm_shots_selection(),
            Some("page-bubble") => self.arm_shots_page_bubble(),
            Some("context-menu") => self.arm_shots_context_menu(),
            Some("search") => self.arm_shots_search(),
            Some("sign") => self.arm_shots_sign_compose(),
            Some("reorder") => self.arm_shots_rail_drag(),
            Some("import") => {
                self.show_rail = true;
                self.staged_import = true;
            }
            Some("loading") => self.staged_opening = true,
            Some("error") => {
                self.load_error = Some(
                    "Could not open inaccessible.pdf: the file is unavailable or is not a readable PDF"
                        .to_string(),
                );
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Whether a document is being opened: the loading state shows.
    fn opening(&self) -> bool {
        self.pending_open.is_some() || self.staged_opening
    }

    /// Screenshot-only: arm a live page-rail reorder drag so a still frame shows
    /// the drag ghost + reflow gap. The rail is forced open.
    pub fn arm_shots_rail_drag(&mut self) {
        self.shots_rail_drag = true;
        self.show_rail = true;
    }

    /// Screenshot-only: pin the floating page bubble open so a still frame shows
    /// the transient page-info overlay (instead of the old bottom strip).
    pub fn arm_shots_page_bubble(&mut self) {
        self.shots_page_bubble = true;
        self.page_bubble.poke(ui::time::now());
    }

    /// Screenshot-only: open the annotation action menu over the sample page
    /// so shared floating-surface styling can be reviewed in the real app.
    pub fn arm_shots_context_menu(&mut self) {
        self.shots_context_menu = true;
    }

    /// Screenshot-only: open the find bar with a query and live matches so a
    /// still frame shows the (non-overlapping) find bar.
    pub fn arm_shots_search(&mut self) {
        self.search_active = true;
        self.find_bar.set_query("compositor");
        ui::widget::Widget::set_focus(&mut self.find_bar, true);
        self.drive_find_bar();
        self.find_bar.next();
        self.find_bar.next();
        self.drive_find_bar();
    }

    /// Screenshot-only: open the Signatures panel in Draw mode with a signature
    /// already drawn on the pad and the Sign tool armed, so a still frame shows
    /// the real signature-composer workflow.
    pub fn arm_shots_sign_compose(&mut self) {
        self.show_sidebar = true;
        self.sidebar_tab = SidebarTab::Signatures;
        self.tool = Tool::Sign;
        self.sig_mode = SigMode::Draw;
        self.sig_strokes = crate::scenarios::sample_drawn_signature();
    }

    /// Screenshot-only: stage a settled text selection over a word of page
    /// 1's first paragraph, so a still frame shows the shared selection HUD —
    /// highlight swatches + Copy + Note — over the page. The glyph layer is
    /// laid out from the page itself in the first draw (see
    /// `stage_shots_selection`), so the selection sits on the printed word.
    pub fn arm_shots_selection(&mut self) {
        self.shots_selection = true;
        self.show_sidebar = true;
    }

    /// Lay out the glyphs of page 1's first body line and select one of its
    /// words, exactly as a double-click would — via the shared word range.
    fn stage_shots_selection(&mut self, tr: &mut TextRenderer) {
        self.shots_selection = false;
        let Some(page) = self.doc.page(1) else {
            return;
        };
        // A word of the closing quote: the selection bar floats over the
        // paragraph above it, clear of the section heading.
        let glyphs = render::line_glyphs(tr, page, "collaborative case");
        let text: String = glyphs.iter().map(|(c, _)| *c).collect();
        let layer = std::sync::Arc::new(PageText::from_glyphs(glyphs));
        self.raster_text.insert(1, layer.clone());
        let Some(byte) = text.find("collaborative") else {
            return;
        };
        let idx = text.get(..byte).map_or(0, |prefix| prefix.chars().count()) + 2;
        let (s, e) = layer.word_range(idx);
        self.selection = Some(TextSelection {
            page: 1,
            anchor: s,
            caret: e.saturating_sub(1),
            dragging: false,
        });
        self.sel_gran = Granularity::Word;
        self.sel_anchor_range = (s, e);
    }

    /// Screenshot helper: import a distinctive generated image as a new page
    /// after the current one, so a still frame shows an imported image page
    /// in the rail + canvas. No-op when no PDF is open.
    fn demo_import_image(&mut self) {
        self.show_rail = true;
        if !self.raster_active() {
            return;
        }
        // A recognisable swatch (diagonal split, bright colours) written to a
        // temp PNG and imported through the real code path.
        let (w, h) = (480u32, 360u32);
        let mut img = image::RgbaImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let above = (x as f32 / w as f32) > (y as f32 / h as f32);
            *px = if above {
                image::Rgba([64, 132, 246, 255])
            } else {
                image::Rgba([246, 132, 64, 255])
            };
        }
        let Some(png) = tilo_paths::temp_dir().map(|dir| dir.join("tilo_pdf_demo_import.png"))
        else {
            return;
        };
        if image::DynamicImage::ImageRgba8(img).save(&png).is_ok() {
            self.import_file(&png);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "constructor lists the complete explicit state of this custom document workspace"
    )]
    fn build(doc: Document, content: ContentStore) -> Self {
        let page_count = doc.page_count();
        let source_pages = doc.pages.clone();
        let author = std::env::var("QUICK_APP_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "You".to_string());
        Self {
            doc,
            raster_handle: RasterHandle::new(content.clone()).ok(),
            raster_document: None,
            pending_open: None,
            raster_images: HashMap::new(),
            raster_render_pending: HashSet::new(),
            raster_tiles: crate::tiles::TileCache::new(crate::tiles::MAX_TILE_CACHE_BYTES),
            raster_tile_batch: Vec::new(),
            raster_text_pending: HashSet::new(),
            raster_links: std::collections::HashMap::new(),
            raster_links_pending: HashSet::new(),
            pending_worker_plan: None,
            applied_worker_plan: None,
            worker_plan_blobs: Vec::new(),
            pending_save: None,
            pending_import: None,
            has_document: false,
            record_source: None,
            reopen_requested: false,
            staged_opening: false,
            staged_import: false,
            doc_name: String::new(),
            load_error: None,
            content,
            shots_rail_drag: false,
            shots_page_bubble: false,
            shots_context_menu: false,
            shots_panel: None,
            shots_colours: false,
            shots_selection: false,
            annotations: Vec::new(),
            comments: Vec::new(),
            signatures: Vec::new(),
            rect: Rect::ZERO,
            fit: FitMode::Page,
            viewport: ViewportState::new()
                .with_zoom_bounds(ZOOM_MIN, ZOOM_MAX)
                .with_padding(DEFAULT_PADDING)
                .with_alignment(SmallContentAlign::Center, SmallContentAlign::Start),
            current_page: 1,
            resource_page_ids: (1..=page_count)
                .map(|page| format!("source-{page}"))
                .collect(),
            resource_page_sources: (1..=page_count).collect(),
            resource_page_imports: vec![None; page_count],
            source_pages,
            search_entries: Vec::new(),
            search_projection_revision: None,
            #[cfg(test)]
            search_projection_refreshes: 0,
            tool: Tool::Select,
            swatch: Swatch::Yellow,
            sidebar_tab: SidebarTab::Comments,
            show_sidebar: true,
            show_rail: true,
            rail_w: f64::from(ui::tokens::Metrics::BASE.sidebar_min_width),
            page_sidebar: PageSidebar::new(PAGE_H / PAGE_W).with_add("Import…"),
            pages_dirty: false,
            portal_import_rx: None,
            selected_anno: None,
            editing: None,
            comment_views: std::cell::RefCell::new(HashMap::new()),
            rich_inset: std::cell::Cell::new(None),
            selected_sig: None,
            sidebar_scroll: 0.0,
            sidebar_reveal: None,
            signer_name: author.clone(),
            author,
            sig_mode: SigMode::Type,
            sig_strokes: Vec::new(),
            sig_stroke: None,
            signature: None,
            signature_revision: None,
            sig_pad_rect: Rect::ZERO,
            search_active: false,
            find_bar: ui::widget::FindBar::new(),
            matches: Vec::new(),
            active_match: 0,
            search_whole_word: false,
            search_case_sensitive: false,
            raster_matches: Vec::new(),
            raster_text: std::collections::HashMap::new(),
            selection: None,
            drag_select: DragSelect::new(),
            sel_gran: Granularity::Char,
            sel_anchor_range: (0, 0),
            region: None,
            shift_down: false,
            source_path: None,
            status_note: None,
            region_copy: ui::CopyWatch::default(),
            portal_save_rx: None,
            portal_action: None,
            last_mouse: Point::ZERO,
            drag: None,
            pan: None,
            rail_drag: None,
            context_menu: None,
            marker_colours: None,
            toolbar: build_toolbar(),
            toolbar_band: Rect::ZERO,
            toolbar_synced: String::new(),
            created_counter: 100,
            dirty: true,
            pending_visuals: false,
            text_lines: std::collections::HashMap::new(),
            tooltip: Tooltip::new(""),
            tooltip_anchor: Rect::ZERO,
            tooltip_pending: false,
            reduce_motion: false,
            layout_theme: Theme::dark(),
            zoom_w: 0.0,
            tab_widths: None,
            zoom_input: zoom_stepper(100.0, false),
            zoom_input_enabled: false,
            empty_open_button: std::cell::Cell::new(Rect::ZERO),
            portal_rx: None,
            portal_contents_rx: None,
            open_here: None,
            pending_reopen: None,
            overlay_mode: false,
            prev_overlay: None,
            toolbar_rect: Rect::ZERO,
            search_rect: Rect::ZERO,
            rail_rect: Rect::ZERO,
            canvas_rect: Rect::ZERO,
            sidebar_rect: Rect::ZERO,
            page_bubble: ui::StatusBubble::new(),
            bubble_shown_page: 0,
            bubble_rect: Rect::ZERO,
            bubble_prev: Rect::ZERO,
            bubble_next: Rect::ZERO,
            sidebar_hits: Vec::new(),
            edit_rect: Rect::ZERO,
            sel_hud: None,
            marker_drag: None,
            // Unit tests drive the reader without a host: its edits go
            // through the same executor a hosted reader's do.
            #[cfg(test)]
            dispatcher: Some(ui::framework::testing::dispatcher(APP_ID)),
            #[cfg(not(test))]
            dispatcher: None,
            read_only: false,
            people: None,
            page_maps: Vec::new(),
            resource_page_rotations: Vec::new(),
            form_fields: Vec::new(),
            form_values: std::collections::BTreeMap::new(),
            source_blob: None,
            stored_annotations: Vec::new(),
            adopted: Vec::new(),
            existing_request: None,
            opens: std::rc::Rc::default(),
            seed_pending: true,
            reader_markup_hidden: false,
            markup_style: model::MarkupStyle::Highlight,
        }
    }

    /// Test-only: one frame's sync, then what it submitted applied to `doc`
    /// as the host applies it, and shown in the same frame.
    #[cfg(test)]
    pub(crate) fn sync_settled(&mut self, doc: &mut AutoCommit, root: &automerge::ObjId) {
        Widget::sync(self, doc, root);
        let Some(dispatcher) = self.dispatcher.clone() else {
            return;
        };
        if !ui::framework::testing::resolve(&dispatcher, doc)
            .expect("submitted edits apply")
            .is_empty()
        {
            Widget::sync(self, doc, root);
        }
    }

    /// Submit a field edit or verb through the resource's dispatcher to be
    /// applied after this pass, from a pass that only reads the document (a
    /// worker's answer). A refusal is reported by the dispatcher.
    pub(crate) fn submit(&self, invoke: ui::tilo_resource::Invoke<PdfMarkup>) {
        if let Some(dispatcher) = self.dispatcher.as_ref() {
            dispatcher.submit(invoke);
        }
    }

    /// Apply a field edit or verb now, from an input event, as the person's
    /// change; say why when it is refused.
    pub(crate) fn perform_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        invoke: ui::tilo_resource::Invoke<PdfMarkup>,
    ) {
        let Some(dispatcher) = self.dispatcher.as_ref() else {
            return;
        };
        if let Err(error) = dispatcher.submit_now(state, invoke) {
            self.status_note = Some(error.to_string());
            self.dirty = true;
        }
    }

    /// Follow the record's state: which document its markup is made
    /// against. The markup itself is read from the document every sync
    /// receives, the live one or a past version.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::markup::PdfMarkup) {
        let source = state.pinned_source();
        if self.record_source != source {
            self.record_source = source;
        }
        self.dirty = true;
    }

    /// Attach the resource's dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::markup::PdfMarkup>,
    ) {
        self.read_only = dispatcher.read_only();
        // What this participant authors is recorded under their id, and
        // shown under the name the roster has for it.
        if let Some(me) = dispatcher.me() {
            me.clone_into(&mut self.author);
        }
        self.dispatcher = Some(dispatcher);
    }

    /// Open other documents through `open_here`.
    pub(crate) fn attach_open_here(&mut self, open_here: Option<ui::framework::OpenHere>) {
        self.open_here = open_here;
    }

    /// Show the participants' names from `people`.
    pub(crate) fn attach_people(&mut self, people: ui::framework::People) {
        self.people = Some(people);
        self.dirty = true;
    }

    /// The name to show for the participant `id` an annotation, comment or
    /// reply records.
    pub(crate) fn author_name(&self, id: &str) -> String {
        self.people
            .as_ref()
            .map_or_else(|| id.to_owned(), |people| people.name(id))
    }

    /// Compose with the person's own signature record.
    pub(crate) fn attach_signature(
        &mut self,
        signature: ui::framework::PersonRecord<crate::signature::SignatureProfile>,
    ) {
        self.signature = Some(signature);
        self.signature_revision = None;
    }

    /// Read the person's signature when it changed: composed here, in
    /// another window or on another of their devices.
    fn follow_signature(&mut self) {
        let Some(record) = self.signature.as_ref() else {
            return;
        };
        let revision = record.revision();
        if self.signature_revision == Some(revision) {
            return;
        }
        self.signature_revision = Some(revision);
        let kept = record.get();
        if !kept.signer.is_empty() {
            self.signer_name.clone_from(&kept.signer);
        }
        self.sig_strokes.clone_from(&kept.strokes.0);
        self.dirty = true;
    }

    /// Keep an edit of the person's signature in their own record.
    fn keep_signature(
        &mut self,
        invoke: &ui::tilo_resource::Invoke<crate::signature::SignatureProfile>,
    ) {
        let Some(record) = self.signature.as_ref() else {
            return;
        };
        if let Err(error) = record.invoke(invoke) {
            self.status_note = Some(format!("Your signature was not kept: {error}"));
            self.dirty = true;
        }
    }

    /// Keep the drawn strokes as the person's signature.
    fn keep_strokes(&mut self) {
        let invoke = crate::signature::SignatureProfile::SET_STROKES.with(self.sig_strokes.clone());
        self.keep_signature(&invoke);
    }

    /// The participant's permission to edit changed.
    pub(crate) fn set_read_only(&mut self, read_only: bool) {
        if self.read_only != read_only {
            self.read_only = read_only;
            // Allowed to write now, the record gets what opening it adds.
            self.seed_pending |= !read_only;
            self.dirty = true;
        }
    }

    /// Test-only: open find and materialise matches for `query`.
    #[cfg(test)]
    pub(crate) fn stage_find_for_test(&mut self, query: &str) {
        self.open_search();
        self.find_bar.set_query(query);
        self.recompute_matches();
    }

    /// Test-only: live find hit count after [`Self::stage_find_for_test`].
    #[cfg(test)]
    pub(crate) fn find_match_count_for_test(&self) -> usize {
        self.match_count()
    }

    /// The presentation context Ctrl+K needs to build the document-level action
    /// (add a note at the current page). `None` when no document is open.
    pub(crate) fn pdf_resource_ctx(&self) -> Option<crate::actions::PdfResourceCtx> {
        if !self.has_document {
            return None;
        }
        let doc_id = "document".to_string();
        let title = if self.doc.title.is_empty() {
            doc_id.clone()
        } else {
            self.doc.title.clone()
        };
        Some(crate::actions::PdfResourceCtx {
            doc_id,
            title,
            page_ids: self.resource_page_ids.clone(),
            page_sources: self.resource_page_sources.clone(),
            search_query: self.query().to_string(),
            search_whole_word: self.search_whole_word,
            search_case_sensitive: self.search_case_sensitive,
            search_token: crate::actions::search_result_token(
                self.query(),
                self.search_whole_word,
                self.search_case_sensitive,
            ),
            search_pages: if self.raster_active() {
                self.raster_matches.iter().map(|hit| hit.page).collect()
            } else {
                self.matches.iter().map(|hit| hit.page).collect()
            },
            outline: self
                .doc
                .outline
                .iter()
                .map(|item| (item.title.clone(), item.page, item.level))
                .collect(),
        })
    }

    /// Rebuild the search entries only when the shared document or durable
    /// page occurrence projection changed. Searching never traverses
    /// document text or markup.
    fn refresh_search_entries(&mut self, state: &AutoCommit) {
        let Some(ctx) = self.pdf_resource_ctx() else {
            return;
        };
        let revision = SearchProjectionRevision {
            doc_id: ctx.doc_id.clone(),
            heads: state.committed_heads(),
            page_ids: ctx.page_ids.clone(),
            page_sources: ctx.page_sources.clone(),
            search_token: ctx.search_token.clone(),
            search_pages: ctx.search_pages.clone(),
        };
        if self.search_projection_revision.as_ref() == Some(&revision) {
            return;
        }
        self.search_entries = crate::search::surface_entries(&ctx, &self.source_pages);
        self.search_projection_revision = Some(revision);
        #[cfg(test)]
        {
            self.search_projection_refreshes += 1;
        }
    }

    // ---- page maps ---------------------------------------------------

    /// The map for 1-based displayed `page`.
    fn page_map(&self, page: usize) -> model::PageMap {
        page.checked_sub(1)
            .and_then(|i| self.page_maps.get(i).copied())
            .unwrap_or_else(|| model::PageMap::identity(self.pw(), self.ph()))
    }

    /// Recompute every page's map from the renderer's geometry and the
    /// recorded rotations.
    fn rebuild_page_maps(&mut self) {
        let (card_w, card_h) = (self.pw(), self.ph());
        let (geometries, rotations): (Vec<Option<tilo_pdf::PageGeometry>>, Vec<i32>) = {
            self.raster_document
                .as_ref()
                .map(|d| (d.geometries.clone(), d.rotations.clone()))
                .unwrap_or_default()
        };
        self.page_maps = (0..self.doc.page_count())
            .map(|i| match geometries.get(i).copied().flatten() {
                Some(own) => {
                    model::PageMap::new(own, rotations.get(i).copied().unwrap_or(0), card_w, card_h)
                }
                None => model::PageMap::identity(card_w, card_h),
            })
            .collect();
    }

    /// The durable occurrence id of 1-based displayed `page`.
    fn page_id_of(&self, page: usize) -> Option<String> {
        page.checked_sub(1)
            .and_then(|i| self.resource_page_ids.get(i))
            .cloned()
    }

    /// A page's text layer shown in its card space.
    fn text_for_card(&self, page: usize, text: Arc<PageText>) -> Arc<PageText> {
        let map = self.page_map(page);
        if map.is_identity() {
            text
        } else {
            Arc::new(text.with_map(map))
        }
    }

    /// Author a new annotation drawn in card space: store its geometry in
    /// the page's PDF user space, anchored to the page occurrence.
    fn author_annotation(&mut self, state: &mut ui::widget::Edits<'_>, a: &Annotation) {
        let user = Annotation {
            created_ms: now_ms(),
            ..self.page_map(a.page).annotation_to_user(a)
        };
        let Some(page_id) = self.page_id_of(a.page) else {
            return;
        };
        self.perform_action(state, crate::actions::add_annotation_on(&user, &page_id));
    }

    /// Pin the open document's bytes in its record the first time the
    /// record meets them; say so when the file no longer matches.
    fn pin_source(&mut self, state: &AutoCommit) {
        let Some((hash, len)) = self.source_blob.clone() else {
            return;
        };
        match PdfMarkup::of(state).pinned_source() {
            None if !self.read_only => {
                self.submit(crate::actions::pin_source_action(
                    &hash,
                    len,
                    &self.doc_name,
                ));
            }
            Some(pinned) if pinned.hash != hash => {
                self.status_note = Some(
                    "This file has changed since its markup was made; annotations may not line up"
                        .to_string(),
                );
            }
            _ => {}
        }
    }

    /// Take over the markup already in the PDF the first time the record
    /// meets these bytes (see [`crate::adopt`]): ask the PDF owner for it,
    /// once per source. Whether it was asked or is not needed.
    fn adopt_existing(&mut self, state: &AutoCommit) -> bool {
        let Some((hash, _)) = self.source_blob.clone() else {
            return true;
        };
        let Some(pinned) = PdfMarkup::of(state).pinned_source() else {
            // The source is pinned first; its markup is taken over after.
            return self.read_only;
        };
        if self.read_only
            || pinned.hash != hash
            || PdfMarkup::of(state)
                .imported_source()
                .map(str::to_owned)
                .as_deref()
                == Some(hash.as_str())
            || self.existing_request.as_deref() == Some(hash.as_str())
        {
            return true;
        }
        self.existing_request = Some(hash);
        let requested = match (self.raster_handle.as_ref(), self.raster_document.as_ref()) {
            (Some(handle), Some(document)) => handle.read_existing(document.id),
            _ => Err(crate::source::RasterSubmitError::Stopped),
        };
        if let Err(error) = requested {
            self.existing_request = None;
            tracing::debug!(%error, "asking for the PDF's own markup failed");
            return false;
        }
        true
    }

    /// Record the markup `existing` (read from the bytes hashed `hash`).
    fn finish_adoption(
        &mut self,
        state: &AutoCommit,
        hash: &str,
        existing: &crate::adopt::Existing,
    ) {
        if PdfMarkup::of(state)
            .imported_source()
            .map(str::to_owned)
            .as_deref()
            == Some(hash)
            || self
                .source_blob
                .as_ref()
                .is_none_or(|(current, _)| current != hash)
        {
            return;
        }
        let mut known: std::collections::HashSet<String> = PdfMarkup::of(state)
            .annotations_with_hidden()
            .into_iter()
            .map(|a| a.id)
            .collect();
        for comment in PdfMarkup::of(state).list_comments() {
            known.extend(comment.replies.into_iter().map(|r| r.id));
            known.insert(comment.id);
        }
        let plan = crate::adopt::plan_import(hash, existing, &known);
        self.submit(crate::actions::import_existing_action(hash, &plan));
        self.reload(state);
        self.dirty = true;
    }

    /// Page images leave the document's own markup out exactly when the
    /// record has taken it over (the reader then draws it from the records).
    fn sync_reader_markup(&mut self, state: &AutoCommit) {
        let hide = self.source_blob.as_ref().is_some_and(|(hash, _)| {
            PdfMarkup::of(state)
                .imported_source()
                .map(str::to_owned)
                .as_deref()
                == Some(hash)
        });
        if hide == self.reader_markup_hidden {
            return;
        }
        self.reader_markup_hidden = hide;
        if let (Some(handle), Some(document)) =
            (self.raster_handle.as_ref(), self.raster_document.as_ref())
            && let Err(error) = handle.set_reader_markup(document.id, !hide)
        {
            tracing::debug!(%error, "updating the page images' markup failed");
            self.reader_markup_hidden = !hide;
            return;
        }
        // Images already delivered show the other choice.
        self.raster_images.clear();
        self.raster_render_pending.clear();
        self.raster_tiles.clear();
        self.raster_tile_batch.clear();
        self.dirty = true;
    }

    /// The markup to export, in PDF user space.
    fn export_markup(&self) -> crate::source::ExportMarkup {
        crate::source::ExportMarkup {
            // As stored: taken-over markup keeps its exact user-space
            // geometry, so an unchanged thread is recognised as unchanged.
            annotations: self
                .stored_annotations
                .iter()
                .map(|a| self.page_map(a.page).annotation_to_user(a))
                .collect(),
            comments: self.comments.clone(),
            signatures: self
                .signatures
                .iter()
                .map(|s| self.page_map(s.page).signature_to_user(s))
                .collect(),
            form_values: self
                .form_values
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            page_ids: self.resource_page_ids.clone(),
            adopted: self.adopted.clone(),
        }
    }

    // ---- cache refresh ----------------------------------------------

    fn reload(&mut self, state: &AutoCommit) {
        if self.page_maps.len() != self.doc.page_count() {
            self.rebuild_page_maps();
        }
        self.stored_annotations = PdfMarkup::of(state).list_annotations();
        self.annotations = self
            .stored_annotations
            .iter()
            .map(|a| self.page_map(a.page).annotation_to_card(a))
            .collect();
        self.adopted = PdfMarkup::of(state).adopted_entries();
        self.comments = PdfMarkup::of(state).list_comments();
        let shown: HashSet<String> = self
            .comments
            .iter()
            .flat_map(|comment| {
                std::iter::once(comment_view_key(&comment.id, None)).chain(
                    comment
                        .replies
                        .iter()
                        .map(|reply| comment_view_key(&comment.id, Some(&reply.id))),
                )
            })
            .collect();
        self.comment_views
            .get_mut()
            .retain(|key, _| shown.contains(key));
        self.signatures = PdfMarkup::of(state)
            .list_signatures()
            .iter()
            .map(|s| self.page_map(s.page).signature_to_card(s))
            .collect();
        self.form_values = PdfMarkup::of(state).form_values;
    }

    // ---- geometry ---------------------------------------------------

    /// The range a person may drag the page rail's width within.
    fn rail_width_range(&self) -> (f64, f64) {
        let m = &self.layout_theme.tokens.metrics;
        (f64::from(m.rail_width), f64::from(m.sidebar_max_width))
    }

    fn rail_width(&self) -> f64 {
        if self.show_rail {
            let (lo, hi) = self.rail_width_range();
            self.rail_w.clamp(lo, hi)
        } else {
            0.0
        }
    }

    /// The document's uniform page size, in PDF points.
    fn pw(&self) -> f64 {
        self.doc.page_w
    }
    fn ph(&self) -> f64 {
        self.doc.page_h
    }

    /// Snap panel open/closed state to the current window width. On a wide
    /// window the panels dock and reserve space; on a narrow one they collapse
    /// to closed overlays. Only the *transition* forces state, so the user's
    /// explicit toggles within a size class are respected.
    fn reconcile_overlay(&mut self) {
        // The panels dock while the page canvas keeps its minimum width
        // beside both of them; below that they float over the canvas.
        let m = &self.layout_theme.tokens.metrics;
        let docked = (f64::from(m.sidebar_min_width)
            + f64::from(m.inspector_width)
            + f64::from(m.content_min_width))
            * self.layout_theme.responsive_scale();
        let overlay = self.rect.width() < docked;
        self.overlay_mode = overlay;
        if self.prev_overlay != Some(overlay) {
            // A screenshot that stages the review panel keeps it on screen,
            // floating over a narrow window.
            self.show_sidebar = !overlay || self.shots_panel.is_some();
            self.show_rail = !overlay;
            self.prev_overlay = Some(overlay);
        }
        // The empty state has nothing to fill the panels with.
        if !self.has_document {
            self.show_sidebar = false;
            self.show_rail = false;
        }
    }

    /// The sidebar's drawn width — narrower on cramped overlay windows so it
    /// always leaves a strip of canvas to tap on to dismiss it.
    fn sidebar_width(&self) -> f64 {
        if !self.show_sidebar {
            return 0.0;
        }
        // A docked review rail gets a modest width increase with enlarged
        // type. This preserves real line length and tab labels instead of
        // forcing every comment into a tall, hard-to-scan single word column.
        let theme = &self.layout_theme;
        let m = &theme.tokens.metrics;
        let preferred = f64::from(m.inspector_width) * theme.responsive_scale();
        if self.overlay_mode {
            preferred
                .min(self.rect.width() - f64::from(m.header_height))
                .max(f64::from(m.sidebar_min_width))
        } else {
            preferred
        }
    }

    fn recompute_regions(&mut self) {
        let r = self.rect;
        // The toolbar is always one row: commands that do not fit go to its
        // More menu.
        let toolbar_h = f64::from(self.layout_theme.tokens.metrics.header_height);
        self.toolbar_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + toolbar_h);
        self.lay_out_toolbar();
        let mid_top = r.y0 + toolbar_h;
        // No reserved bottom strip: the page-info bubble floats over the canvas
        // (see `draw_page_bubble`), so the content spans to the window bottom.
        let mid_bottom = r.y1;
        let rail_w = self.rail_width();
        let sidebar_w = self.sidebar_width();
        self.rail_rect = Rect::new(r.x0, mid_top, r.x0 + rail_w, mid_bottom);
        self.sidebar_rect = Rect::new(r.x1 - sidebar_w, mid_top, r.x1, mid_bottom);
        let (x0, x1) = if self.overlay_mode {
            // Panels float; the canvas spans the whole mid strip beneath them.
            (r.x0, r.x1)
        } else {
            (r.x0 + rail_w, r.x1 - sidebar_w)
        };
        // Find is a band at the top of the page canvas.
        let mut canvas_top = mid_top;
        if self.search_active {
            let find_h = self.find_bar.height(&self.layout_theme);
            self.search_rect = Rect::new(x0, mid_top, x1, mid_top + find_h);
            canvas_top += find_h;
        } else {
            self.search_rect = Rect::ZERO;
        }
        self.canvas_rect = Rect::new(x0, canvas_top, x1, mid_bottom);
    }

    // ---- viewport (zoom + pan) --------------------------------------
    //
    // The zoom factor and scroll offsets live in `self.viewport`
    // (`ui::ViewportState`) — the same shared kernel typst / ebook / maps
    // use. These accessors + projection helpers adapt its single content rect
    // to the reader's vertical multi-page stack.

    /// Current uniform zoom (page-points → screen px).
    fn zoom(&self) -> f64 {
        self.viewport.zoom()
    }
    fn scroll_x(&self) -> f64 {
        self.viewport.scroll_x()
    }
    fn scroll_y(&self) -> f64 {
        self.viewport.scroll_y()
    }

    /// Content-space pitch from one page's top to the next (page height plus
    /// the inter-page gap). Because the gap lives in content space it scales
    /// with zoom, exactly like the pages.
    fn stack_unit(&self) -> f64 {
        self.ph() + PAGE_GAP
    }

    /// Total content-space height of the page stack (all pages + the gaps
    /// between them). This is the viewport's content height.
    fn content_stack_h(&self) -> f64 {
        let n = self.doc.page_count() as f64;
        n * self.ph() + (n - 1.0).max(0.0) * PAGE_GAP
    }

    /// Push the current canvas rect + content size into the viewport, then
    /// re-apply the fit intent. Called every layout pass so a resize reflows
    /// the zoom and re-clamps the scroll through the shared kernel.
    fn sync_viewport_frame(&mut self) {
        self.viewport.set_rect(self.canvas_rect);
        self.viewport
            .set_content_size(self.pw(), self.content_stack_h());
        self.recompute_zoom();
    }

    /// The zoom that fits one whole page inside the canvas (both axes), the
    /// Fit-page target. Mirrors `ViewportState::fit_zoom` but against a single
    /// page rather than the whole stack.
    fn fit_page_zoom(&self) -> f64 {
        let r = self.viewport.rect();
        let avail_w = r.width() - 2.0 * DEFAULT_PADDING;
        let avail_h = r.height() - 2.0 * DEFAULT_PADDING;
        if self.pw() <= 0.0 || self.ph() <= 0.0 || avail_w <= 0.0 || avail_h <= 0.0 {
            return self.viewport.zoom();
        }
        (avail_w / self.pw())
            .min(avail_h / self.ph())
            .clamp(ZOOM_MIN, ZOOM_MAX)
    }

    /// Re-apply the current fit intent to the viewport. Fit-page / fit-width
    /// derive a fresh zoom from the canvas size; a pinned custom zoom is left
    /// to the kernel (idempotent — `set_zoom` to the value it already holds is
    /// a no-op).
    fn recompute_zoom(&mut self) {
        let z = match self.fit {
            FitMode::Page => self.fit_page_zoom(),
            FitMode::Width => self.viewport.fit_width_zoom(),
            FitMode::Custom(z) => z.clamp(ZOOM_MIN, ZOOM_MAX),
        };
        self.viewport.set_zoom(z);
    }

    /// Whether the zoomed page is wider than the viewport — drives whether
    /// the arrow keys pan horizontally and whether the h-scrollbar shows.
    fn is_overflowing_x(&self) -> bool {
        self.viewport.overflows_x()
    }

    fn clamp_scroll(&mut self) {
        self.viewport.clamp_scroll();
    }

    /// Screen-space top-left origin of each page card, projected through the
    /// viewport's content→screen transform. Page `i` sits at content
    /// `(0, i * stack_unit)`, so horizontal centring, scroll and zoom all fall
    /// out of the shared kernel.
    fn page_origins(&self) -> Vec<Point> {
        let unit = self.stack_unit();
        (0..self.doc.page_count())
            .map(|i| {
                self.viewport
                    .content_point_to_screen(Point::new(0.0, i as f64 * unit))
            })
            .collect()
    }

    /// Map a screen point to (page-number, page-point) if it lands on a page.
    fn hit_page(&self, p: Point) -> Option<(usize, (f64, f64))> {
        let cp = self.viewport.screen_to_content(p);
        if cp.x < 0.0 || cp.x > self.pw() {
            return None;
        }
        let unit = self.stack_unit();
        if cp.y < 0.0 {
            return None;
        }
        let idx = (cp.y / unit).floor();
        let i = idx as usize;
        if i >= self.doc.page_count() {
            return None;
        }
        let py = cp.y - idx * unit;
        if py < 0.0 || py > self.ph() {
            return None;
        }
        Some((i + 1, (cp.x, py)))
    }

    /// The page whose top edge currently sits nearest the top of the canvas.
    fn nearest_page(&self) -> usize {
        let n = self.doc.page_count().max(1);
        let unit = self.stack_unit();
        if unit <= 0.0 {
            return 1;
        }
        // Content-space y at the very top of the canvas viewport.
        let top_content = self
            .viewport
            .screen_to_content(Point::new(self.canvas_rect.x0, self.canvas_rect.y0))
            .y;
        let p = (top_content / unit).round() as i64 + 1;
        p.clamp(1, n as i64) as usize
    }

    fn go_to_page(&mut self, page: usize) {
        let page = page.clamp(1, self.doc.page_count());
        let target_y = (page - 1) as f64 * self.stack_unit() * self.viewport.zoom();
        self.viewport.set_scroll(self.viewport.scroll_x(), target_y);
        self.current_page = page;
        self.dirty = true;
    }

    /// Scroll the document by a screen-space delta (arrow-key / wheel nudge),
    /// clamped by the shared viewport kernel, keeping the current-page readout
    /// in sync.
    fn scroll_by(&mut self, dx: f64, dy: f64) {
        self.viewport.pan_by(dx, dy);
        self.current_page = self.nearest_page();
        self.dirty = true;
    }

    // ---- page editing (driven by the shared page rail) --------------

    /// Apply the events the shared page rail produced this gesture: selection
    /// jumps, drag-reorder, context-menu duplicate/move/delete, and the import
    /// affordance.
    fn apply_sidebar_events(&mut self, state: &mut ui::widget::Edits<'_>) {
        for ev in self.page_sidebar.take_events() {
            match ev {
                PageSidebarEvent::Select(i) => self.go_to_page(i + 1),
                PageSidebarEvent::Reorder { from, to } => {
                    let Some(id) = self.resource_page_ids.get(from).cloned() else {
                        continue;
                    };
                    let after = if to == 0 {
                        None
                    } else {
                        self.resource_page_ids.get(to - 1).map(String::as_str)
                    };
                    let action = crate::actions::move_page_plan_action(&id, after);
                    self.perform_action(state, action);
                }
                PageSidebarEvent::Duplicate(i) => {
                    let Some(id) = self.resource_page_ids.get(i) else {
                        continue;
                    };
                    let source_page = self.resource_page_sources.get(i).copied().unwrap_or(i + 1);
                    let action =
                        crate::actions::duplicate_page_plan_action(&crate::markup::PagePlanEntry {
                            id: id.clone(),
                            source_page,
                            imported: self.resource_page_imports.get(i).cloned().flatten(),
                            rotation: 0,
                        });
                    self.perform_action(state, action);
                }
                PageSidebarEvent::Delete(i) => {
                    let Some(id) = self.resource_page_ids.get(i) else {
                        continue;
                    };
                    let action = crate::actions::delete_page_plan_action(id);
                    self.perform_action(state, action);
                }
                PageSidebarEvent::Add => self.request_import_via_portal(),
            }
        }
    }

    /// Add what opening the loaded document adds to its record, once per
    /// document: its page plan, the source it pins, the markup taken over
    /// from the PDF. A reader that may not write adds it once it may.
    fn seed_record(&mut self, state: &AutoCommit) {
        if self.read_only {
            self.seed_pending = true;
            return;
        }
        if !PdfMarkup::of(state).page_plan_seeded {
            self.submit(PdfMarkup::START_PAGE_PLAN.with(self.source_pages.len().max(1)));
        }
        self.pin_source(state);
        // Asked again on the next frame while the PDF owner cannot be asked.
        self.seed_pending = !self.adopt_existing(state);
    }

    /// Adopt the durable, collaboration-replayed page plan into whichever
    /// local renderer is available. A peer without source bytes still keeps
    /// the shared intent in its record; a renderer with the original source
    /// can immediately materialise duplicates, deletes and order changes.
    fn reconcile_shared_page_plan(&mut self, state: &AutoCommit) {
        let plan = PdfMarkup::of(state).page_plan();
        if plan.is_empty() {
            return;
        }
        let plan_ids: Vec<String> = plan.iter().map(|entry| entry.id.clone()).collect();
        let sources: Vec<usize> = plan.iter().map(|entry| entry.source_page).collect();
        let rotations: Vec<i32> = plan.iter().map(|entry| entry.rotation).collect();
        let plan_changed = self.resource_page_ids != plan_ids
            || self.resource_page_sources != sources
            || (self.resource_page_rotations != rotations
                && !(self.resource_page_rotations.is_empty() && rotations.iter().all(|r| *r == 0)));
        self.resource_page_rotations = rotations;
        if let Some(document) = self.raster_document.as_ref() {
            // Page-plan edits are durable action state, but materialising
            // them mutates PDFium's local source graph.  Submit that work to
            // the owner and retain honest page-shaped placeholders until its
            // matching completion arrives.
            // A collaborator's imported bytes can arrive without changing the
            // CRDT plan. Re-materialise when that availability changes too.
            let blobs: Vec<_> = plan
                .iter()
                .filter_map(|entry| entry.imported.as_ref())
                .map(|imported| self.content.state(&imported.hash))
                .collect();
            let needs_reconcile = self.pending_worker_plan.as_ref().map_or_else(
                || {
                    self.applied_worker_plan.as_ref() != Some(&plan)
                        || self.worker_plan_blobs != blobs
                },
                |pending| pending != &plan,
            );
            // Imported bytes that have not arrived cannot be materialised:
            // asking the owner anyway only answers with a refusal that reads
            // like a broken plan. Say what is missing and ask again once
            // their availability changes.
            let missing = blobs
                .iter()
                .find(|state| **state != ContentState::Resolved)
                .copied();
            if needs_reconcile && let Some(missing) = missing {
                self.status_note = Some(
                    if missing == ContentState::Pending {
                        "Imported page content is arriving from collaborators"
                    } else {
                        "Imported page content is unavailable on this device"
                    }
                    .into(),
                );
                self.worker_plan_blobs = blobs;
            } else if needs_reconcile {
                let request = ReconcileRequest {
                    document_id: document.id,
                    plan: plan.clone(),
                };
                if self
                    .raster_handle
                    .as_ref()
                    .is_some_and(|handle| handle.reconcile_page_plan(request).is_ok())
                {
                    self.pending_worker_plan = Some(plan.clone());
                    self.worker_plan_blobs = blobs;
                } else {
                    self.status_note = Some("PDF page-plan worker is unavailable".to_string());
                }
            }
            self.doc.pages = (1..=plan.len())
                .map(|number| model::Page {
                    number,
                    blocks: Vec::new(),
                })
                .collect();
        } else if plan.iter().any(|entry| entry.imported.is_some()) {
            // Text backend cannot materialise imported PDF/image bytes, but it
            // must retain their occurrences so another peer can hydrate them.
            self.doc.pages = (1..=plan.len())
                .map(|number| model::Page {
                    number,
                    blocks: Vec::new(),
                })
                .collect();
            self.status_note =
                Some("Imported pages are unavailable until a PDF renderer is available".into());
        } else {
            let Some(mut pages) = sources
                .iter()
                .map(|source| self.source_pages.get(source - 1).cloned())
                .collect::<Option<Vec<_>>>()
            else {
                self.status_note =
                    Some("Shared page plan needs source pages unavailable on this device".into());
                return;
            };
            for (index, page) in pages.iter_mut().enumerate() {
                page.number = index + 1;
            }
            self.doc.pages = pages;
        }
        self.resource_page_ids = plan_ids;
        self.resource_page_sources = sources;
        self.resource_page_imports = plan.iter().map(|entry| entry.imported.clone()).collect();
        // A CRDT page-plan mutation changes the binary document that Save
        // writes, even when it arrived from a collaborator rather than a local
        // rail gesture. The original source plan compares equal and therefore
        // does not make a freshly opened document appear dirty.
        self.pages_dirty |= plan_changed;
        self.current_page = self.current_page.clamp(1, self.doc.page_count().max(1));
    }

    /// Whether the document's pages have been edited (reorder / delete /
    /// duplicate / import) since it was last saved.
    pub fn pages_dirty(&self) -> bool {
        self.pages_dirty
    }

    /// Import content-addressed PDF/image bytes as a shared, replayable page
    /// plan action. The bytes live in the app's content store, never in Automerge.
    pub fn import_file(&mut self, path: &std::path::Path) -> bool {
        if self.pending_import.is_some() {
            self.status_note = Some("Another page import is already in progress".to_string());
            self.dirty = true;
            return false;
        }
        let Some(document) = self.raster_document.as_ref() else {
            self.status_note = Some("Importing pages needs the PDF worker".to_string());
            self.dirty = true;
            return false;
        };
        let at = self.current_page;
        let after = self.resource_page_ids.get(at.saturating_sub(1)).cloned();
        match self
            .raster_handle
            .as_ref()
            .ok_or_else(|| "the PDF import worker is unavailable".to_string())
            .and_then(|handle| {
                handle
                    .import_path(document.id, path, at, after)
                    .map_err(|error| error.to_string())
            }) {
            Ok(request) => {
                self.pending_import = Some(request);
                self.status_note = Some("Preparing pages for shared import…".to_string());
                self.dirty = true;
                true
            }
            Err(error) => {
                self.status_note = Some(format!("Import failed: {error}"));
                self.dirty = true;
                false
            }
        }
    }

    fn request_import_via_portal(&mut self) {
        if !self.raster_active() {
            self.status_note = Some("Importing pages needs an open PDF".to_string());
            self.dirty = true;
            return;
        }
        if self.portal_import_rx.is_some() || self.pending_import.is_some() {
            return;
        }
        self.portal_import_rx = Some(pdf_import_request().send());
        self.dirty = true;
    }

    fn drain_portal_import(&mut self) {
        let Some(rx) = self.portal_import_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_import_rx = None;
                match outcome {
                    PortalOutcome::Opened(path) => {
                        self.import_file(&path);
                    }
                    PortalOutcome::Cancelled => {}
                    PortalOutcome::Failed(reason) => {
                        self.status_note =
                            Some(format!("Couldn't open the import dialog: {reason}"));
                        self.dirty = true;
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_import_rx = None;
            }
        }
    }

    // ---- search -----------------------------------------------------

    fn search_options(&self) -> SearchOptions {
        SearchOptions {
            whole_word: self.search_whole_word,
            case_sensitive: self.search_case_sensitive,
        }
    }

    fn query(&self) -> &str {
        self.find_bar.query()
    }

    fn recompute_matches(&mut self) {
        let query = self.query().to_string();
        self.recompute_matches_for(&query);
    }

    /// Find `query` in the open document with the current options.
    fn recompute_matches_for(&mut self, query: &str) {
        let opts = self.search_options();
        let query = query.to_string();
        self.active_match = 0;
        if let Some(document) = self.raster_document.as_ref() {
            // Searching a worker-backed document is progressive: queue the
            // bounded text layers and search only layers that have arrived.
            // No keystroke ever extracts PDFium text on the UI thread.
            for page in 0..self.doc.page_count() {
                if !self.raster_text.contains_key(&(page + 1))
                    && self.raster_text_pending.insert(page)
                    && !self.raster_handle.as_ref().is_some_and(|handle| {
                        handle
                            .request_text(TextRequest {
                                document_id: document.id,
                                page,
                            })
                            .is_ok()
                    })
                {
                    self.raster_text_pending.remove(&page);
                    self.status_note = Some("PDF text worker is unavailable".to_string());
                }
            }
            self.matches.clear();
            let mut hits = Vec::new();
            for (page, text) in &self.raster_text {
                let remaining = model::MAX_SEARCH_RESULTS.saturating_sub(hits.len());
                hits.extend(text.search(*page, &query, opts).into_iter().take(remaining));
                if hits.len() >= model::MAX_SEARCH_RESULTS {
                    break;
                }
            }
            hits.sort_by_key(|hit| (hit.page, hit.char_start));
            self.raster_matches = hits;
            if let Some(hit) = self.raster_matches.first() {
                self.go_to_page(hit.page);
            }
        } else {
            // Text backend: search the reflowed block model.
            self.raster_matches.clear();
            self.matches = model::search(&self.doc, &query, opts);
            if let Some(m) = self.matches.first() {
                self.go_to_page(m.page);
            }
        }
        self.dirty = true;
    }

    /// The number of live search hits, across whichever backend is active.
    fn match_count(&self) -> usize {
        if self.raster_active() {
            self.raster_matches.len()
        } else {
            self.matches.len()
        }
    }

    /// Step the find bar's current match by `delta` (wrapping), as its
    /// previous / next buttons do.
    fn step_match(&mut self, delta: isize) {
        for _ in 0..delta.unsigned_abs() {
            if delta < 0 {
                self.find_bar.previous();
            } else {
                self.find_bar.next();
            }
            self.drive_find_bar();
        }
    }

    /// Set the find bar's Whole word and Match case options.
    fn set_find_options(&mut self, whole_word: bool, case_sensitive: bool) {
        let mut options = self.find_bar.options();
        options.whole_word = whole_word;
        options.match_case = case_sensitive;
        self.find_bar.set_options(options);
    }

    /// Restore and navigate a canonical search-result resource.  An ordinal is
    /// meaningful only for the query/options that produced it, so this always
    /// rebuilds that exact bounded result set before accepting the target.
    fn go_to_search_result(
        &mut self,
        query: &str,
        whole_word: bool,
        case_sensitive: bool,
        index: usize,
        page: usize,
    ) {
        self.open_search();
        self.set_find_options(whole_word, case_sensitive);
        self.find_bar.set_query(query);
        self.drive_find_bar();
        let matched_page = if self.raster_active() {
            self.raster_matches.get(index).map(|hit| hit.page)
        } else {
            self.matches.get(index).map(|hit| hit.page)
        };
        if matched_page == Some(page) {
            // Step the bar to the result so its counter and the emphasised
            // match agree.
            self.step_match(isize::try_from(index).unwrap_or(0));
            self.go_to_page(page);
        } else {
            // A progressive worker may not have extracted this page yet, or a
            // source document may have changed.  Refuse to rebind a stale
            // ordinal and leave an actionable status rather than navigating to
            // a potentially unrelated match.
            self.status_note = Some("Find result is no longer available".into());
        }
        self.dirty = true;
    }

    // ---- text selection (raster backend) ----------------------------
    //
    // A real PDF's glyph geometry lives in `raster_text` (filled each draw for
    // the visible pages). Selection maps pointer positions to glyph indices via
    // that layer, exactly the geometry the raster page bitmap is drawn from, so
    // a highlighted run lines up with the printed text.

    /// Try to begin a text selection at page-point `pt` on `page`. Returns
    /// `true` when the point lands on a glyph (selection armed); `false` when
    /// there is no text there, so the caller can fall back to panning.
    /// Begin a text selection at page-point `pt` on `page`. Routed through the
    /// shared [`ui::gesture::DragSelect`] click ladder, so a single click places
    /// the caret, a double-click selects the word under it, and a triple-click
    /// selects the whole visual line — behaving identically to every text field
    /// and the ebook reader. `press` is the screen point (for the multi-click
    /// distance test). Returns whether a selection was armed (i.e. the press
    /// landed on real glyph text); when it did not, the caller falls back to a
    /// pan so empty page area still scrolls.
    fn begin_selection(&mut self, page: usize, pt: (f64, f64), press: Point) -> bool {
        let Some(layer) = self.ensure_raster_text(page) else {
            return false;
        };
        let Some(idx) = layer.char_at(pt.0, pt.1) else {
            return false;
        };
        let intent = self.drag_select.press(press, false);
        match intent.granularity {
            Granularity::Word | Granularity::Line => {
                let (s, e) = if intent.granularity == Granularity::Word {
                    layer.word_range(idx)
                } else {
                    layer.row_range(idx)
                };
                if e > s {
                    self.selection = Some(TextSelection {
                        page,
                        anchor: s,
                        caret: e - 1,
                        dragging: true,
                    });
                    self.sel_gran = intent.granularity;
                    self.sel_anchor_range = (s, e);
                } else {
                    self.arm_char_selection(page, idx);
                }
            }
            Granularity::Char => self.arm_char_selection(page, idx),
        }
        self.dirty = true;
        true
    }

    /// Arm a fresh character-granularity selection collapsed at glyph `idx`.
    fn arm_char_selection(&mut self, page: usize, idx: usize) {
        self.selection = Some(TextSelection {
            page,
            anchor: idx,
            caret: idx,
            dragging: true,
        });
        self.sel_gran = Granularity::Char;
        self.sel_anchor_range = (idx, idx + 1);
    }

    /// The engine text layer for 1-based `page`, extracting it on demand and
    /// caching it into `raster_text` if the (deferred) per-frame refresh hasn't
    /// built it yet. This lets a text selection start on a freshly-opened or
    /// just-scrolled-to page without the reader having eagerly paid the
    /// extraction cost on every prior frame. `None` when no raster backend.
    fn ensure_raster_text(&mut self, page: usize) -> Option<Arc<PageText>> {
        if let Some(layer) = self.raster_text.get(&page) {
            return Some(layer.clone());
        }
        if let Some(document) = self.raster_document.as_ref()
            && page > 0
            && self.raster_text_pending.insert(page - 1)
        {
            if !self.raster_handle.as_ref().is_some_and(|handle| {
                handle
                    .request_text(TextRequest {
                        document_id: document.id,
                        page: page - 1,
                    })
                    .is_ok()
            }) {
                self.raster_text_pending.remove(&(page - 1));
                self.status_note = Some("PDF text worker is unavailable".to_string());
            }
            self.dirty = true;
        }
        None
    }

    /// Extend the live selection to the pointer at page-point `pt`. Only tracks
    /// motion on the selection's own page (selection stays single-page). A
    /// character drag walks the caret glyph-by-glyph; a word/line drag (after a
    /// double/triple click) keeps the anchored unit inside the selection and
    /// grows by whole units through the shared [`ui::extend_span_drag`] rule.
    fn update_selection(&mut self, page: usize, pt: (f64, f64)) {
        let Some(sel) = self.selection else { return };
        if !sel.dragging || sel.page != page {
            return;
        }
        let Some(layer) = self.raster_text.get(&page) else {
            return;
        };
        let Some(idx) = layer
            .char_at(pt.0, pt.1)
            .or_else(|| layer.nearest_char(pt.0, pt.1))
        else {
            return;
        };
        let (anchor, caret) = match self.sel_gran {
            Granularity::Char => (sel.anchor, idx),
            Granularity::Word | Granularity::Line => {
                let (unit_lo, unit_hi) = if self.sel_gran == Granularity::Word {
                    layer.word_range(idx)
                } else {
                    layer.row_range(idx)
                };
                let (a_lo, a_hi) = self.sel_anchor_range;
                let (a, c) = ui::extend_span_drag(a_lo, a_hi, idx, unit_lo, unit_hi);
                let lo = a.min(c);
                let hi = a.max(c).max(lo + 1);
                (lo, hi - 1)
            }
        };
        if let Some(s) = self.selection.as_mut() {
            s.anchor = anchor;
            s.caret = caret;
        }
        self.dirty = true;
    }

    /// End the drag. A selection that never grew past its anchor (a bare click)
    /// is dropped so a single click doesn't leave a one-glyph highlight behind.
    fn finish_selection(&mut self) {
        self.drag_select.release();
        if let Some(s) = self.selection.as_mut() {
            s.dragging = false;
            if !s.is_active() {
                self.selection = None;
            }
        }
        self.dirty = true;
    }

    fn clear_selection(&mut self) {
        if self.selection.take().is_some() {
            self.dirty = true;
        }
    }

    /// The selected text, if a run is selected and its page's glyph layer is
    /// available.
    fn selection_text(&self) -> Option<String> {
        let sel = self.selection?;
        if !sel.is_active() {
            return None;
        }
        let layer = self.raster_text.get(&sel.page)?;
        let (a, b) = sel.range();
        let s = layer.text_for_range(a, b);
        (!s.trim().is_empty()).then_some(s)
    }

    /// Copy the current selection to the system clipboard, surfacing a toast.
    fn copy_selection(&mut self) -> bool {
        if let Some(text) = self.selection_text() {
            ui::clipboard_copy(&text);
            let n = text.chars().count();
            self.status_note = Some(format!(
                "Copied {n} character{}",
                if n == 1 { "" } else { "s" }
            ));
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// Crop the active image-region marquee out of its page bitmap and copy it
    /// to the clipboard as an image. Raster backend only. Returns true when a
    /// region of usable size was cropped (the clipboard push itself is
    /// best-effort — it silently no-ops in a headless session).
    fn copy_region(&mut self) -> bool {
        let Some(region) = self.region else {
            return false;
        };
        let rect = region.rect();
        if rect.w < 3.0 || rect.h < 3.0 {
            return false;
        }
        let page_idx = region.page.saturating_sub(1);
        let (pgw, pgh) = self
            .raster_document
            .as_ref()
            .and_then(|document| document.page_sizes.get(page_idx).copied())
            .unwrap_or_else(|| (self.pw(), self.ph()));
        let bucket = ((pgw * 2.0).round() as u32).clamp(96, 4096);
        let image_key = (page_idx, crate::source::RasterBackend::bucket_for(bucket));
        let Some(image) = self.raster_images.get(&image_key).cloned() else {
            if let Some(document) = self.raster_document.as_ref()
                && self.raster_render_pending.insert(image_key)
                && !self.raster_handle.as_ref().is_some_and(|handle| {
                    handle
                        .request_render(RenderRequest {
                            document_id: document.id,
                            page: page_idx,
                            target_width: bucket,
                        })
                        .is_ok()
                })
            {
                self.raster_render_pending.remove(&image_key);
            }
            self.status_note = Some("Rendering selected region…".to_string());
            self.dirty = true;
            return false;
        };
        let Some((width, height, rgba)) = render::crop_page_region(&image, pgw, pgh, rect) else {
            return false;
        };
        let copied = self
            .region_copy
            .watch(ui::clipboard_copy_image(width, height, &rgba));
        self.status_note = Some(if copied {
            format!("Copied a {width}×{height} image region")
        } else {
            "Selected a region (clipboard unavailable)".to_string()
        });
        self.dirty = true;
        true
    }

    // ---- save-back (flatten markup onto a real .pdf) ----------------

    /// A suggested filename for the saved copy: `<stem>-annotated.pdf`.
    fn suggested_save_name(&self) -> String {
        let stem = self
            .source_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("document");
        format!("{stem}-annotated.pdf")
    }

    /// Ask the host portal for a save location (HIG §5 — never an in-app
    /// picker). A second request while one is in flight is ignored.
    fn request_save_via_portal(&mut self) {
        // Saving exports through the PDF engine; without an open PDF there is nothing to save.
        if !self.raster_active() {
            self.status_note = Some("Saving annotations needs an open PDF".to_string());
            self.dirty = true;
            return;
        }
        if self.portal_save_rx.is_some() || self.pending_save.is_some() {
            return;
        }
        self.portal_save_rx = Some(pdf_save_request(&self.suggested_save_name()).send());
        self.dirty = true;
    }

    fn finish_portal_action(&mut self, success: bool, error: Option<String>) {
        let Some(reply) = self.portal_action.take() else {
            return;
        };
        reply.finish(if success {
            Ok(tilo_actions::ActionOutput::default())
        } else {
            Err(error.unwrap_or_else(|| "file selection cancelled".into()))
        });
    }

    /// The open or save effect waiting here was cancelled by its caller:
    /// stop the picker, the PDF owner's work and the host's open.
    fn follow_cancellation(&mut self) {
        if !self
            .portal_action
            .as_ref()
            .is_some_and(ui::framework::EffectReply::is_cancelled)
        {
            return;
        }
        self.portal_action = None;
        self.portal_rx = None;
        self.portal_contents_rx = None;
        self.portal_save_rx = None;
        // A document asked to open in this window's place is not opened.
        if let Some((_, outcome)) = self.pending_reopen.take() {
            outcome.withdraw();
        }
        self.cancel_pending_open();
        if let Some(request) = self.pending_save.take() {
            let _ = self
                .raster_handle
                .as_ref()
                .and_then(|handle| handle.cancel_save(request).ok());
        }
        self.dirty = true;
    }

    /// Drain a completed portal *save* request, if any, and flatten the current
    /// markup to the chosen path. Called once per frame from [`Widget::sync`].
    fn drain_portal_save(&mut self) {
        let Some(rx) = self.portal_save_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_save_rx = None;
                match outcome {
                    PortalOutcome::Opened(path) => {
                        let path = ensure_pdf_extension(path);
                        let success = self.save_annotated_to(&path);
                        // A worker save remains Accepted through prepare and
                        // atomic publication; its exact terminal update is
                        // emitted from `drain_raster_events`.
                        if self.pending_save.is_none() {
                            self.finish_portal_action(
                                success,
                                (!success).then(|| "PDF could not be saved".into()),
                            );
                        }
                    }
                    PortalOutcome::Cancelled => self.finish_portal_action(false, None),
                    PortalOutcome::Failed(reason) => {
                        self.status_note = Some(format!("Couldn't open the save dialog: {reason}"));
                        self.dirty = true;
                        self.finish_portal_action(false, Some(reason));
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_save_rx = None;
            }
        }
    }

    /// Flatten the current annotations + signatures onto a fresh copy of the
    /// PDF and write it to `path`. Surfaces success or failure as a toast.
    fn save_annotated_to(&mut self, path: &std::path::Path) -> bool {
        if self.pending_worker_plan.is_some() {
            self.status_note = Some("Waiting for the shared page plan before saving".to_string());
            self.dirty = true;
            return false;
        }
        let Some(document) = self.raster_document.as_ref() else {
            self.status_note = Some("Saving annotations needs the PDF worker".to_string());
            self.dirty = true;
            return false;
        };
        let id = match crate::source::next_worker_request_id() {
            Ok(id) => id,
            Err(error) => {
                self.status_note = Some(format!("Save failed: {error}"));
                self.dirty = true;
                return false;
            }
        };
        let request = SaveRequest {
            id,
            document_id: document.id,
            markup: self.export_markup(),
            path: path.to_path_buf(),
        };
        match self
            .raster_handle
            .as_ref()
            .ok_or_else(|| "the PDF save worker is unavailable".to_string())
            .and_then(|handle| {
                handle
                    .save_annotated(request.clone())
                    .map_err(|error| error.to_string())
            }) {
            Ok(()) => {
                self.pending_save = Some(request);
                self.status_note = Some("Saving annotated PDF…".to_string());
                self.dirty = true;
                true
            }
            Err(error) => {
                self.status_note = Some(format!("Save failed: {error}"));
                self.dirty = true;
                false
            }
        }
    }

    // ---- annotation actions -----------------------------------------

    /// Smart text marker: snap a highlight drag onto the text lines under it.
    ///
    /// Returns one [`PtRect`] per text line the drag covers, each clipped to
    /// the line's glyph extent so the band looks like a real text selection.
    /// Returns an empty vec when the drag isn't near any text — the caller
    /// then falls back to a free-form rectangle. The decision is purely a
    /// proximity test, so the user never toggles a mode.
    fn snap_highlights(&self, page: usize, drag: PtRect) -> Vec<PtRect> {
        let Some(lines) = self.text_lines.get(&page) else {
            return Vec::new();
        };
        let drag_bottom = drag.y + drag.h;
        let drag_right = drag.x + drag.w;
        let mut bands = Vec::new();
        for line in lines {
            let lr = line.rect();
            // Vertical proximity: the line's band overlaps (or is within
            // SNAP_NEAR of) the drag's vertical span.
            let v_gap = (lr.y0 - drag_bottom).max(drag.y - lr.y1).max(0.0);
            if v_gap > SNAP_NEAR {
                continue;
            }
            // Horizontal overlap with the line's glyph extent, widened to
            // the words the drag touches so a marker never cuts a word.
            let (x0, x1) = line.snap_to_words(drag.x.max(lr.x0), drag_right.min(lr.x1));
            // Near-horizontal proximity still counts so a slightly-off drag
            // snaps to the whole line rather than dropping it.
            let h_overlap = x1 - x0;
            let h_gap = (lr.x0 - drag_right).max(drag.x - lr.x1).max(0.0);
            if h_overlap <= 1.0 && h_gap > SNAP_NEAR {
                continue;
            }
            let (bx0, bx1) = if h_overlap > 1.0 {
                (x0, x1)
            } else {
                (lr.x0, lr.x1)
            };
            bands.push(PtRect::new(bx0, line.y, (bx1 - bx0).max(2.0), line.h));
        }
        bands
    }

    fn commit_drag(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(d) = self.drag.take() else { return };
        match d.tool {
            Tool::Highlight => {
                let drag_rect = PtRect::from_corners(d.start.0, d.start.1, d.cur.0, d.cur.1);
                if drag_rect.w <= 6.0 && drag_rect.h <= 4.0 {
                    // Sub-threshold: nothing meaningful was dragged.
                    self.reload(state);
                    self.dirty = true;
                    return;
                }
                // Default: stick to text. Fall back to free-form only when
                // the drag isn't near any text line.
                // On a page with a text layer the drag selects text: the
                // highlight covers exactly those characters and remembers
                // their range. Otherwise it snaps to laid-out lines, or is a
                // free-form band.
                let text = self.ensure_raster_text(d.page);
                let anchored = text.as_ref().and_then(|t| t.range_between(d.start, d.cur));
                let (quads, chars) = if let (Some((s, e)), Some(t)) = (anchored, text.as_ref()) {
                    (t.bands_for_range(s, e), t.source_range(s, e))
                } else {
                    let snapped = self.snap_highlights(d.page, drag_rect);
                    let quads = if snapped.is_empty() {
                        vec![drag_rect]
                    } else {
                        snapped
                    };
                    (quads, None)
                };
                let a = Annotation {
                    chars,
                    ..Annotation::new(
                        d.page,
                        AnnoKind::Markup {
                            style: self.markup_style,
                            quads,
                        },
                        self.swatch,
                        self.author.clone(),
                    )
                };
                self.selected_anno = Some(a.id.clone());
                self.author_annotation(state, &a);
            }
            Tool::Ink if d.points.len() >= 2 => {
                let a = Annotation::new(
                    d.page,
                    AnnoKind::ink(d.points),
                    self.swatch,
                    self.author.clone(),
                );
                self.author_annotation(state, &a);
            }
            _ => {}
        }
        self.reload(state);
        self.dirty = true;
    }

    fn create_note(&mut self, state: &mut ui::widget::Edits<'_>, page: usize, pt: (f64, f64)) {
        let anno_id = Annotation::new_id();
        self.created_counter += 1;
        let comment_id = Comment::new_id();
        // One action creates the note marker *and* its (empty) comment thread —
        // one undo boundary, replayed atomically on peers.
        let note = crate::actions::NoteDraft {
            anno_id: &anno_id,
            comment_id: &comment_id,
            color: self.swatch,
            body: "",
        };
        let Some(page_id) = self.page_id_of(page) else {
            return;
        };
        let action = crate::actions::create_note_on_action(
            &note,
            &page_id,
            self.page_map(page).card_to_user(pt),
        );
        self.perform_action(state, action);
        self.selected_anno = Some(anno_id);
        self.selected_sig = None;
        // Open the Comments rail, scroll the brand-new thread into view, and
        // drop the caret straight into it so the user just types.
        self.focus_sidebar(SidebarTab::Comments, comment_id.clone());
        self.reload(state);
        self.begin_edit(EditTarget::CommentBody(comment_id), "");
        self.dirty = true;
    }

    // ---- inline text editing ----------------------------------------

    /// Start editing `target`, seeding a shared [`TextInput`] with its current
    /// text and dropping the caret at the end. Any in-flight edit is dropped
    /// without committing — callers that need to preserve it commit first.
    fn begin_edit(&mut self, target: EditTarget, initial: &str) {
        if let EditTarget::CommentBody(id) | EditTarget::Reply(id) = &target {
            let blocks = match &target {
                // The comment as written, unless the edit starts from other text.
                EditTarget::CommentBody(_) => self
                    .comments
                    .iter()
                    .find(|comment| &comment.id == id && comment.body == initial)
                    .map_or_else(
                        || tilo_rich_text::plain_blocks(initial),
                        |comment| comment.blocks.clone(),
                    ),
                _ => tilo_rich_text::plain_blocks(initial),
            };
            let field = Self::comment_field(&blocks, matches!(target, EditTarget::Reply(_)));
            self.editing = Some(Editing {
                target,
                input: EditInput::Rich(Box::new(field)),
            });
            self.dirty = true;
            return;
        }
        // Typed in the body role the panel sets these texts in.
        let mut input = TextInput::local()
            .multiline(true)
            .with_enter_submits(true)
            .with_text_role(ui::TextRole::Body);
        input.set_plain_chrome(true);
        input.set_text(initial);
        input.set_cursor_pos(initial.len());
        input.set_focus(true);
        self.editing = Some(Editing {
            target,
            input: EditInput::Plain(Box::new(input)),
        });
        self.dirty = true;
    }

    /// A comment or reply field holding `blocks`, focused at its end: Enter
    /// sends, Shift+Enter breaks the line.
    fn comment_field(blocks: &[ui::widget::BlockContent], reply: bool) -> CommentField {
        let hint = if reply {
            "Reply…"
        } else {
            "Write a comment…"
        };
        let mut surface = RichTextSurface::over(Draft::with_blocks(blocks))
            .with_editor(
                ui::widget::BlockEditor::new()
                    .with_wrap_lines(true)
                    .with_compact_paragraph_spacing(true)
                    .with_block_handles(false),
            )
            .with_placeholder(hint)
            .with_submit(SubmitKey::Enter);
        surface.present_now();
        surface.set_focus(true);
        surface
            .editor_mut()
            .focus_block(usize::MAX, ui::widget::CursorPlacement::End);
        ui::widget::Embedded::new(surface)
    }

    /// Whether `target` is the edit currently in progress.
    fn is_editing(&self, target: &EditTarget) -> bool {
        self.editing.as_ref().is_some_and(|e| &e.target == target)
    }

    /// Write the active edit's text to its durable home and clear the edit.
    /// An emptied comment body (with no replies) removes the note entirely so
    /// an abandoned marker never litters the page.
    fn commit_edit(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(e) = self.editing.take() else { return };
        let text = e.text();
        let clips = e.clips();
        match e.target {
            EditTarget::CommentBody(id) => {
                let trimmed = text.trim();
                let has_replies = self
                    .comments
                    .iter()
                    .find(|c| c.id == id)
                    .is_some_and(|c| !c.replies.is_empty());
                if trimmed.is_empty() && !has_replies {
                    self.delete_target(&DeleteTarget::Comment(id), state);
                    return;
                }
                self.perform_action(state, crate::actions::set_comment_text_action(&id, clips));
                self.reload(state);
            }
            EditTarget::Reply(id) => {
                if !text.trim().is_empty() {
                    let action = crate::actions::add_reply_text_action(&id, clips);
                    self.perform_action(state, action);
                    self.reload(state);
                }
            }
            EditTarget::Signer => {
                let name = text.trim();
                self.signer_name = if name.is_empty() {
                    self.author_name(&self.author)
                } else {
                    name.to_string()
                };
                let invoke =
                    crate::signature::SignatureProfile::SET_SIGNER.with(self.signer_name.clone());
                self.keep_signature(&invoke);
            }
            EditTarget::FormField(name) => {
                let value = model::FormEntry::Text(text.trim_end().to_string());
                self.perform_action(state, crate::actions::set_form_value_action(&name, &value));
                self.reload(state);
            }
            EditTarget::FreeText(id) => {
                let text = text.trim_end();
                if text.trim().is_empty() {
                    // An emptied box is removed rather than left invisible.
                    self.delete_target(&DeleteTarget::Annotation(id), state);
                    return;
                }
                if let Some(action) =
                    crate::actions::set_free_text_action(&PdfMarkup::of(state), &id, text)
                {
                    self.perform_action(state, action);
                }
                // Grow the box to hold what was typed.
                let grown = self
                    .annotations
                    .iter_mut()
                    .find(|a| a.id == id)
                    .is_some_and(|a| {
                        if let AnnoKind::FreeText { rect, size, .. } = &mut a.kind {
                            let needed = model::free_text_height(text, *size, rect.w);
                            if needed > rect.h + 0.5 {
                                rect.h = needed;
                                return true;
                            }
                        }
                        false
                    });
                if grown {
                    self.persist_free_text_box(state, &id);
                }
                self.reload(state);
            }
        }
        self.dirty = true;
    }

    fn cancel_edit(&mut self) {
        self.editing = None;
        self.dirty = true;
    }

    /// What opens a page, a piece of markup or a comment thread of the
    /// record when asked from outside the window (a search hit, an agent).
    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&ui::tilo_resource::Id) -> Result<(), ui::tilo_resource::Refusal> + 'static
    {
        let opens = std::rc::Rc::clone(&self.opens);
        move |id| {
            opens.borrow_mut().push(id.clone());
            Ok(())
        }
    }

    /// Show what was asked to be opened since the last frame: go to the
    /// page, select the markup on it, reveal the comment thread.
    fn drain_opens(&mut self) {
        let opens = std::mem::take(&mut *self.opens.borrow_mut());
        for id in opens {
            let id = id.as_str();
            if let Some(index) = self.resource_page_ids.iter().position(|page| page == id) {
                self.go_to_page(index + 1);
            } else if let Some(page) = self.annotations.iter().find(|a| a.id == id).map(|a| a.page)
            {
                self.go_to_page(page);
                self.selected_anno = Some(id.to_owned());
                self.selected_sig = None;
            } else if let Some(page) = self.comments.iter().find(|c| c.id == id).map(|c| c.page) {
                self.go_to_page(page);
                self.focus_sidebar(SidebarTab::Comments, id.to_owned());
            } else if let Some(page) = self.signatures.iter().find(|s| s.id == id).map(|s| s.page) {
                self.go_to_page(page);
                self.selected_sig = Some(id.to_owned());
                self.selected_anno = None;
                self.focus_sidebar(SidebarTab::Signatures, id.to_owned());
            } else {
                self.status_note = Some("That is no longer in this document".to_owned());
            }
            self.dirty = true;
        }
    }

    /// Reveal a freshly-created item: ensure the sidebar is open on `tab` and
    /// request the next draw scroll `id` into view + select it.
    fn focus_sidebar(&mut self, tab: SidebarTab, id: String) {
        self.sidebar_tab = tab;
        if !self.show_sidebar {
            self.show_sidebar = true;
            self.reflow();
        }
        self.sidebar_reveal = Some(id);
        self.dirty = true;
    }

    /// Normalise a screen point into the signature draw pad's `[0,1]²` space
    /// (clamped), against the same inset the pad renders into so a drawn stroke
    /// lands where the pointer went.
    fn sig_pad_norm(&self, p: Point) -> (f64, f64) {
        let a = self.sig_pad_rect.inflate(-10.0, -10.0);
        let x = if a.width() > 0.0 {
            ((p.x - a.x0) / a.width()).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let y = if a.height() > 0.0 {
            ((p.y - a.y0) / a.height()).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (x, y)
    }

    /// The signature the composer currently holds: the drawn strokes when in
    /// draw mode (and strokes have been drawn), otherwise the typed
    /// name. `None` when draw mode is selected but the pad is empty — there is
    /// no signature to place yet.
    fn composed_ink(&self) -> Option<SignatureInk> {
        match self.sig_mode {
            SigMode::Draw => {
                let ink = SignatureInk::Drawn {
                    strokes: self.sig_strokes.clone(),
                };
                (!ink.is_empty()).then_some(ink)
            }
            SigMode::Type => Some(SignatureInk::Typed),
        }
    }

    fn place_signature(&mut self, state: &mut ui::widget::Edits<'_>, page: usize, pt: (f64, f64)) {
        // Refuse to stamp an empty drawn signature — the user must draw one
        // first (or switch to a typed signature).
        let Some(ink) = self.composed_ink() else {
            self.status_note = Some("Draw your signature first, then click a page".to_string());
            self.dirty = true;
            return;
        };
        let w = 218.0;
        let h = 74.0;
        let rect = PtRect::new(
            (pt.0 - w / 2.0).clamp(MARGIN, self.pw() - MARGIN - w),
            (pt.1 - h / 2.0).clamp(MARGIN, self.ph() - MARGIN - h),
            w,
            h,
        );
        let sig_id = Signature::new_id();
        let s = Signature {
            id: sig_id.clone(),
            page,
            rect,
            signer: self.signer_name.clone(),
            date: today_ymd(),
            ink,
            space: crate::model::GeomSpace::Page,
        };
        let user = self.page_map(page).signature_to_user(&s);
        let Some(page_id) = self.page_id_of(page) else {
            return;
        };
        self.perform_action(state, crate::actions::add_signature_on(&user, &page_id));
        self.selected_sig = Some(sig_id.clone());
        self.selected_anno = None;
        // Land the user on the Signatures rail, scrolled to the new field.
        self.focus_sidebar(SidebarTab::Signatures, sig_id);
        self.reload(state);
        self.dirty = true;
    }

    // ---- form fields ------------------------------------------------

    /// The form widgets shown on 1-based displayed `page`. Fields belong to
    /// the originally opened document, so only its pages carry them.
    pub(crate) fn field_widgets_on(&self, page: usize) -> Vec<FieldWidgetHit> {
        let Some(index) = page.checked_sub(1) else {
            return Vec::new();
        };
        if self
            .resource_page_imports
            .get(index)
            .is_some_and(Option::is_some)
        {
            return Vec::new();
        }
        let Some(source) = self.resource_page_sources.get(index).copied() else {
            return Vec::new();
        };
        let map = self.page_map(page);
        let mut out = Vec::new();
        for field in &self.form_fields {
            for widget in field.widgets.iter().filter(|w| w.page + 1 == source) {
                let r = &widget.rect;
                let rect = map.layer_rect_to_card(&PtRect::new(
                    f64::from(r.x0),
                    f64::from(r.y0),
                    f64::from(r.width()),
                    f64::from(r.height()),
                ));
                out.push(FieldWidgetHit {
                    name: field.name.clone(),
                    kind: field.kind,
                    rect,
                    export: widget.export_value.clone(),
                    options: field.options.clone(),
                    read_only: field.read_only,
                    pdf_value: field.value.clone(),
                    pdf_checked: widget.checked,
                });
            }
        }
        out
    }

    /// The widget under card point `pt` on `page`.
    pub(crate) fn field_at(&self, page: usize, pt: (f64, f64)) -> Option<FieldWidgetHit> {
        self.field_widgets_on(page)
            .into_iter()
            .find(|w| w.rect.contains(pt.0, pt.1))
    }

    /// The text a widget currently shows: the record's value when there is
    /// one, else the document's own.
    pub(crate) fn field_text(&self, w: &FieldWidgetHit) -> String {
        match self.form_values.get(&w.name) {
            Some(model::FormEntry::Text(t) | model::FormEntry::Choice(t)) => t.clone(),
            _ => w.pdf_value.clone(),
        }
    }

    /// Whether a check box or radio widget is on, record first.
    pub(crate) fn field_checked(&self, w: &FieldWidgetHit) -> bool {
        match self.form_values.get(&w.name) {
            Some(model::FormEntry::Checked { on, export }) => match (w.kind, export) {
                (tilo_pdf::FieldKind::RadioButton, Some(e)) => *on && *e == w.export,
                _ => *on,
            },
            _ => w.pdf_checked,
        }
    }

    /// Act on a click on a form widget: edit text, toggle a check box, pick
    /// a radio button, or step a choice field to its next option.
    fn activate_field(&mut self, state: &mut ui::widget::Edits<'_>, w: &FieldWidgetHit) {
        if w.read_only {
            self.status_note = Some(format!("“{}” is read-only", w.name));
            self.dirty = true;
            return;
        }
        let value = match w.kind {
            tilo_pdf::FieldKind::Text => {
                let current = self.field_text(w);
                self.begin_edit(EditTarget::FormField(w.name.clone()), &current);
                return;
            }
            tilo_pdf::FieldKind::CheckBox => model::FormEntry::Checked {
                on: !self.field_checked(w),
                export: None,
            },
            tilo_pdf::FieldKind::RadioButton => model::FormEntry::Checked {
                on: true,
                export: Some(w.export.clone()),
            },
            tilo_pdf::FieldKind::ComboBox | tilo_pdf::FieldKind::ListBox => {
                if w.options.is_empty() {
                    return;
                }
                let current = self.field_text(w);
                let next = w
                    .options
                    .iter()
                    .position(|o| *o == current)
                    .map_or(0, |i| (i + 1) % w.options.len());
                model::FormEntry::Choice(w.options.get(next).cloned().unwrap_or_default())
            }
            _ => return,
        };
        self.perform_action(
            state,
            crate::actions::set_form_value_action(&w.name, &value),
        );
        self.reload(state);
        self.dirty = true;
    }

    /// A click on a page with the select tool: form widgets first, then
    /// markup.
    pub(crate) fn click_page(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        page: usize,
        pt: (f64, f64),
    ) {
        if let Some(widget) = self.field_at(page, pt) {
            self.activate_field(state, &widget);
            return;
        }
        // Markup the reader drew over a link keeps its own click (select and
        // reveal its thread); a link underneath is reached where no markup is.
        if self.annotation_at(page, pt).is_none()
            && let Some(link) = self.link_at(page, pt)
        {
            self.selected_anno = None;
            self.selected_sig = None;
            self.activate_link(page, &link);
            return;
        }
        self.select_at(page, pt);
    }

    // ---- links -------------------------------------------------------

    /// The link under screen point `p`, when a click there would follow it:
    /// the Select tool, on the canvas, with no markup or form field on top.
    pub(crate) fn link_under(&self, p: Point) -> Option<crate::source::PageLink> {
        if self.tool != Tool::Select || !self.canvas_rect.contains(p) {
            return None;
        }
        let (page, pt) = self.hit_page(p)?;
        if self.field_at(page, pt).is_some() || self.annotation_at(page, pt).is_some() {
            return None;
        }
        self.link_at(page, pt)
    }

    /// Make sure displayed page `page`'s link areas are loaded or requested
    /// from the owner worker.
    pub(crate) fn ensure_page_links(&mut self, page: usize) {
        if page == 0 || self.raster_links.contains_key(&page) {
            return;
        }
        let Some(document) = self.raster_document.as_ref() else {
            return;
        };
        if self.raster_links_pending.insert(page - 1)
            && !self.raster_handle.as_ref().is_some_and(|handle| {
                handle
                    .request_links(crate::source::LinksRequest {
                        document_id: document.id,
                        page: page - 1,
                    })
                    .is_ok()
            })
        {
            // A full queue is retried on a later frame.
            self.raster_links_pending.remove(&(page - 1));
        }
    }

    /// The link whose area (mapped into card space) contains card point
    /// `pt` on displayed page `page`. The smallest area wins, so a link
    /// nested in a larger one stays reachable.
    pub(crate) fn link_at(&self, page: usize, pt: (f64, f64)) -> Option<crate::source::PageLink> {
        let links = self.raster_links.get(&page)?;
        let map = self.page_map(page);
        links
            .iter()
            .map(|link| (map.layer_rect_to_card(&link.rect), link))
            .filter(|(rect, _)| rect.contains(pt.0, pt.1))
            .min_by(|(a, _), (b, _)| (a.w * a.h).total_cmp(&(b.w * b.h)))
            .map(|(_, link)| link.clone())
    }

    /// The id of the markup or note under card point `pt`, using the same
    /// hit rules as [`Self::select_at`].
    fn annotation_at(&self, page: usize, pt: (f64, f64)) -> Option<String> {
        self.annotations
            .iter()
            .filter(|a| a.page == page)
            .find(|a| match &a.kind {
                AnnoKind::Note { x, y } => (pt.0 - x).abs() < 24.0 && (pt.1 - y).abs() < 24.0,
                kind @ AnnoKind::Markup { .. } => kind.markup_contains(pt.0, pt.1),
                AnnoKind::Ink { .. } => false,
                kind @ AnnoKind::FreeText { .. } => kind.free_text_contains(pt.0, pt.1),
            })
            .map(|a| a.id.clone())
    }

    /// Follow a link clicked on displayed page `from`.
    fn activate_link(&mut self, from: usize, link: &crate::source::PageLink) {
        match &link.target {
            crate::source::LinkTarget::Page { page, at } => {
                let Some(target) = self.link_destination_page(from, *page) else {
                    self.status_note =
                        Some("The page this link points to is no longer in the document".into());
                    self.dirty = true;
                    return;
                };
                self.go_to_page(target);
                if let Some(at) = *at {
                    // Put the destination point at the top of the view.
                    let (_, y) = self.page_map(target).layer_to_card(at);
                    let top = (target - 1) as f64 * self.stack_unit() + y.max(0.0);
                    self.viewport
                        .set_scroll(self.viewport.scroll_x(), top * self.viewport.zoom());
                    self.current_page = self.nearest_page();
                }
                self.dirty = true;
            }
            crate::source::LinkTarget::Uri(uri) => {
                let scheme = uri
                    .split_once(':')
                    .map(|(scheme, _)| scheme.to_ascii_lowercase());
                if matches!(scheme.as_deref(), Some("http" | "https" | "mailto")) {
                    self.status_note = match ui::widget::try_open_external_url(uri) {
                        Ok(()) => Some(format!("Opening {uri}")),
                        Err(error) => Some(format!("Couldn't open {uri}: {error}")),
                    };
                } else {
                    self.status_note = Some(format!("Links like {uri} are not opened"));
                }
                self.dirty = true;
            }
            crate::source::LinkTarget::File(path) => {
                self.status_note = Some(format!("Links to other files are not opened ({path})"));
                self.dirty = true;
            }
        }
    }

    /// The displayed page (1-based) an internal link on displayed page
    /// `from` leads to: the first occurrence, in the current page plan, of
    /// page `page` (0-based) of the document `from` comes from. `None` when
    /// that page has been deleted.
    fn link_destination_page(&self, from: usize, page: usize) -> Option<usize> {
        let origin = |i: usize| {
            self.resource_page_imports
                .get(i)
                .cloned()
                .flatten()
                .map(|imported| (imported.hash, imported.kind))
        };
        let from_origin = origin(from.checked_sub(1)?);
        (0..self.doc.page_count())
            .find(|&i| {
                self.resource_page_sources.get(i).copied() == Some(page + 1)
                    && origin(i) == from_origin
            })
            .map(|i| i + 1)
    }

    fn select_at(&mut self, page: usize, pt: (f64, f64)) {
        // Front-to-back: notes first (small targets), then highlights.
        let mut hit: Option<String> = None;
        for a in self.annotations.iter().filter(|a| a.page == page) {
            match &a.kind {
                AnnoKind::Note { x, y } => {
                    if (pt.0 - x).abs() < 24.0 && (pt.1 - y).abs() < 24.0 {
                        hit = Some(a.id.clone());
                    }
                }
                kind @ AnnoKind::Markup { .. }
                    if kind.markup_contains(pt.0, pt.1) && hit.is_none() =>
                {
                    hit = Some(a.id.clone());
                }
                _ => {}
            }
        }
        self.selected_anno.clone_from(&hit);
        self.selected_sig = None;
        if let Some(id) = hit {
            // Reveal the linked comment.
            if let Some(c) = self
                .comments
                .iter()
                .find(|c| c.anno_id.as_deref() == Some(id.as_str()))
            {
                let cid = c.id.clone();
                self.focus_sidebar(SidebarTab::Comments, cid);
            }
        }
        self.dirty = true;
    }

    // ---- draggable annotation markers + selection HUD ---------------

    /// The `(id, x, y)` of a sticky-note marker under page-point `pt`, if one is
    /// there. Same hit radius as [`Self::select_at`] so grabbing a marker to
    /// drag it lines up with clicking it to select.
    fn note_marker_at(&self, page: usize, pt: (f64, f64)) -> Option<(String, f64, f64)> {
        let on_page = || self.annotations.iter().filter(|a| a.page == page);
        on_page()
            .find_map(|a| match &a.kind {
                AnnoKind::Note { x, y } if (pt.0 - x).abs() < 24.0 && (pt.1 - y).abs() < 24.0 => {
                    Some((a.id.clone(), *x, *y))
                }
                _ => None,
            })
            // A free text box moves like a marker, by its top-left corner.
            .or_else(|| {
                on_page().find_map(|a| match &a.kind {
                    AnnoKind::FreeText { rect, .. } if rect.contains(pt.0, pt.1) => {
                        Some((a.id.clone(), rect.x, rect.y))
                    }
                    _ => None,
                })
            })
    }

    /// Whether annotation `id` is a free text box.
    fn is_free_text(&self, id: &str) -> bool {
        self.annotations
            .iter()
            .any(|a| a.id == id && matches!(a.kind, AnnoKind::FreeText { .. }))
    }

    /// The page an annotation (by id) lives on.
    fn anno_page(&self, id: &str) -> Option<usize> {
        self.annotations.iter().find(|a| a.id == id).map(|a| a.page)
    }

    /// The current page-point position of a note (by id).
    fn note_pos(&self, id: &str) -> Option<(f64, f64)> {
        self.annotations
            .iter()
            .find(|a| a.id == id)
            .and_then(|a| match &a.kind {
                AnnoKind::Note { x, y } => Some((*x, *y)),
                AnnoKind::FreeText { rect, .. } => Some((rect.x, rect.y)),
                _ => None,
            })
    }

    /// Move a note's *local* cached anchor for live drag feedback (the CRDT is
    /// only written once, on release, so a drag is a single undo step).
    fn move_note_local(&mut self, id: &str, x: f64, y: f64) {
        if let Some(a) = self.annotations.iter_mut().find(|a| a.id == id) {
            match &mut a.kind {
                AnnoKind::Note { x: nx, y: ny } => {
                    *nx = x;
                    *ny = y;
                }
                AnnoKind::FreeText { rect, .. } => {
                    rect.x = x;
                    rect.y = y;
                }
                _ => {}
            }
        }
        self.dirty = true;
    }

    /// Record a free text box's current (card-space) box and size in its
    /// page's user space.
    fn persist_free_text_box(&mut self, state: &mut ui::widget::Edits<'_>, id: &str) {
        let Some(a) = self.annotations.iter().find(|a| a.id == id).cloned() else {
            return;
        };
        if let AnnoKind::FreeText { rect, size, .. } =
            self.page_map(a.page).annotation_to_user(&a).kind
            && let Some(action) = crate::actions::set_free_text_box_user_action(
                &PdfMarkup::of(state),
                id,
                &rect,
                size,
            )
        {
            self.perform_action(state, action);
        }
    }

    /// Place a new free text box with its top-left corner at `pt` (card
    /// space) and start typing into it.
    fn create_free_text(&mut self, state: &mut ui::widget::Edits<'_>, page: usize, pt: (f64, f64)) {
        const WIDTH: f64 = 180.0; // PDF points on the page, not UI spacing
        let size = model::FREE_TEXT_SIZE;
        let height = size * 1.2 * 2.0 + 4.0;
        let a = Annotation {
            created_ms: now_ms(),
            ..Annotation::new(
                page,
                AnnoKind::FreeText {
                    rect: PtRect::new(pt.0, pt.1, WIDTH, height),
                    text: String::new(),
                    size,
                },
                self.swatch,
                self.author.clone(),
            )
        };
        let id = a.id.clone();
        self.author_annotation(state, &a);
        self.reload(state);
        self.selected_anno = Some(id.clone());
        self.selected_sig = None;
        self.begin_edit(EditTarget::FreeText(id), "");
        self.dirty = true;
    }

    /// Start editing the text of free text box `id`.
    fn edit_free_text(&mut self, id: &str) {
        let text = self
            .annotations
            .iter()
            .find(|a| a.id == id)
            .and_then(|a| match &a.kind {
                AnnoKind::FreeText { text, .. } => Some(text.clone()),
                _ => None,
            });
        if let Some(text) = text {
            self.begin_edit(EditTarget::FreeText(id.to_string()), &text);
        }
    }

    /// Step a free text box's text size by `delta` points.
    fn resize_free_text(&mut self, state: &mut ui::widget::Edits<'_>, id: &str, delta: f64) {
        if let Some(a) = self.annotations.iter_mut().find(|a| a.id == id)
            && let AnnoKind::FreeText { size, .. } = &mut a.kind
        {
            *size = (*size + delta).clamp(4.0, 144.0);
        } else {
            return;
        }
        self.persist_free_text_box(state, id);
        self.reload(state);
        self.dirty = true;
    }

    /// The live selection's page-point bands (real engine glyph boxes), for
    /// creating highlights from a text selection.
    fn selection_page_bands(&self) -> Vec<PtRect> {
        let Some(sel) = self.selection else {
            return Vec::new();
        };
        if !sel.is_active() {
            return Vec::new();
        }
        let Some(layer) = self.raster_text.get(&sel.page) else {
            return Vec::new();
        };
        let (a, b) = sel.range();
        layer.bands_for_range(a, b)
    }

    /// The live selection's bands mapped to screen space, for anchoring the HUD.
    fn selection_screen_bounds(&self) -> Option<Rect> {
        let sel = self.selection?;
        if !sel.is_active() {
            return None;
        }
        let origins = self.page_origins();
        let origin = origins.get(sel.page.saturating_sub(1))?;
        let z = self.zoom();
        let mut acc: Option<Rect> = None;
        for r in self.selection_page_bands() {
            let sr = Rect::new(
                origin.x + r.x * z,
                origin.y + r.y * z,
                origin.x + (r.x + r.w) * z,
                origin.y + (r.y + r.h) * z,
            );
            acc = Some(match acc {
                Some(a) => a.union(sr),
                None => sr,
            });
        }
        acc
    }

    /// Highlight the live text selection in `swatch` — one highlight annotation
    /// per selection band — then clear the selection.
    fn highlight_selection(&mut self, state: &mut ui::widget::Edits<'_>, swatch: Swatch) -> bool {
        let Some(sel) = self.selection else {
            return false;
        };
        let bands = self.selection_page_bands();
        if bands.is_empty() {
            return false;
        }
        let (start, end) = sel.range();
        let chars = self
            .raster_text
            .get(&sel.page)
            .and_then(|t| t.source_range(start, end));
        let a = Annotation {
            chars,
            ..Annotation::new(
                sel.page,
                AnnoKind::Markup {
                    style: self.markup_style,
                    quads: bands,
                },
                swatch,
                self.author.clone(),
            )
        };
        self.swatch = swatch;
        self.selected_anno = Some(a.id.clone());
        self.author_annotation(state, &a);
        self.clear_selection();
        self.reload(state);
        self.dirty = true;
        true
    }

    /// Attach a sticky note to the start of the live selection and open its
    /// editor (mirrors the ebook reader's note-from-selection flow).
    fn note_from_selection(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
        let bands = self.selection_page_bands();
        let Some(first) = bands.first().copied() else {
            return false;
        };
        let Some(sel) = self.selection else {
            return false;
        };
        let page = sel.page;
        self.clear_selection();
        self.create_note(state, page, (first.x, first.y));
        true
    }

    /// Route a shared-HUD action to this reader's commands.
    fn apply_hud_action(&mut self, action: ui::HudAction, state: &mut ui::widget::Edits<'_>) {
        match action {
            ui::HudAction::Copy => {
                self.copy_selection();
            }
            ui::HudAction::Highlight(i) => {
                let sw = Swatch::ALL.get(i).copied().unwrap_or(Swatch::Purple);
                self.highlight_selection(state, sw);
            }
            ui::HudAction::Note => {
                self.note_from_selection(state);
            }
            ui::HudAction::Delete => {}
        }
    }

    /// Zoom keeping the viewport centre fixed (used by the +/- buttons and
    /// Ctrl +/-). Pins the fit intent to the resulting custom zoom.
    fn zoom_by(&mut self, factor: f64) {
        self.viewport.zoom_by(factor);
        self.fit = FitMode::Custom(self.viewport.zoom());
        self.current_page = self.nearest_page();
        self.dirty = true;
    }

    /// Pin the zoom to `zoom` (a committed value from the zoom stepper).
    fn set_custom_zoom(&mut self, zoom: f64) {
        let current = self.viewport.zoom();
        if current > 0.0 {
            self.zoom_by(zoom.clamp(ZOOM_MIN, ZOOM_MAX) / current);
        }
    }

    /// Zoom by `factor` while holding the document point under `focal`
    /// (a screen-space point) stationary, so Ctrl+wheel zooms toward the
    /// cursor and the content under it doesn't drift. The focal-tracking math
    /// and scroll clamping are the shared kernel's.
    fn zoom_at(&mut self, factor: f64, focal: Point) {
        self.viewport.zoom_at(factor, focal);
        self.fit = FitMode::Custom(self.viewport.zoom());
        self.current_page = self.nearest_page();
        self.dirty = true;
    }

    fn reset_zoom(&mut self) {
        self.fit = FitMode::Page;
        let z = self.fit_page_zoom();
        self.viewport.set_zoom(z);
        self.viewport.set_scroll(0.0, 0.0);
        self.current_page = self.nearest_page();
        self.dirty = true;
    }

    // ---- deletion ---------------------------------------------------

    /// Remove a markup item by id from the shared CRDT, drop any local
    /// selection that pointed at it, and force a redraw. Used by the Delete
    /// key, the sidebar item buttons, and the right-click context menu — so
    /// every delete affordance lands on the same model mutation.
    fn delete_target(&mut self, target: &DeleteTarget, state: &mut ui::widget::Edits<'_>) {
        match target {
            DeleteTarget::Annotation(id) => {
                self.perform_action(state, crate::actions::delete_annotation_action(id));
                if self.selected_anno.as_deref() == Some(id.as_str()) {
                    self.selected_anno = None;
                }
            }
            DeleteTarget::Comment(id) => {
                // Deleting a comment also removes the annotation it pins, so the
                // orphaned note marker doesn't linger on the page — the
                // `DeleteComment` action carries that cascade. Clear the local
                // selection pointing at the pinned note.
                if let Some(anno) = self
                    .comments
                    .iter()
                    .find(|c| &c.id == id)
                    .and_then(|c| c.anno_id.clone())
                    && self.selected_anno.as_deref() == Some(anno.as_str())
                {
                    self.selected_anno = None;
                }
                self.perform_action(state, crate::actions::delete_comment_action(id));
            }
            DeleteTarget::Signature(id) => {
                self.perform_action(state, crate::actions::delete_signature_action(id));
                if self.selected_sig.as_deref() == Some(id.as_str()) {
                    self.selected_sig = None;
                }
            }
        }
        self.context_menu = None;
        self.reload(state);
        self.dirty = true;
    }

    /// Delete whichever markup the user currently has selected (Delete key).
    fn delete_selected(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
        if let Some(id) = self.selected_anno.clone() {
            self.delete_target(&DeleteTarget::Annotation(id), state);
            true
        } else if let Some(id) = self.selected_sig.clone() {
            self.delete_target(&DeleteTarget::Signature(id), state);
            true
        } else {
            false
        }
    }

    /// Pick a deletable markup item under a right-click on the canvas.
    fn delete_target_at(&self, page: usize, pt: (f64, f64)) -> Option<DeleteTarget> {
        // Signatures first (largest, but explicit), then notes, highlights.
        for s in self.signatures.iter().filter(|s| s.page == page) {
            if s.rect.contains(pt.0, pt.1) {
                return Some(DeleteTarget::Signature(s.id.clone()));
            }
        }
        for a in self.annotations.iter().filter(|a| a.page == page) {
            match &a.kind {
                AnnoKind::Note { x, y } => {
                    if (pt.0 - x).abs() < 24.0 && (pt.1 - y).abs() < 24.0 {
                        return Some(DeleteTarget::Annotation(a.id.clone()));
                    }
                }
                kind @ AnnoKind::Markup { .. } if kind.markup_contains(pt.0, pt.1) => {
                    return Some(DeleteTarget::Annotation(a.id.clone()));
                }
                kind @ AnnoKind::FreeText { .. } if kind.free_text_contains(pt.0, pt.1) => {
                    return Some(DeleteTarget::Annotation(a.id.clone()));
                }
                _ => {}
            }
        }
        None
    }

    // ---- host-native file open (XDG portal) -------------------------

    /// Ask the host desktop's file chooser to open a PDF. Per HIG §5 the app
    /// never draws its own picker: the portal round-trip runs on a worker
    /// thread ([`ui::portal::PickRequest`]) and the chosen path comes back
    /// over a channel drained each frame in [`Self::drain_portal`]. A second
    /// request while one is in flight is ignored, so a double-click on the
    /// Open button can't stack two host dialogs.
    fn request_open_via_portal(&mut self) {
        if self.portal_rx.is_some()
            || self.portal_contents_rx.is_some()
            || self.pending_open.is_some()
        {
            return;
        }
        if tilo_paths::available(tilo_paths::Feature::NativePaths) {
            self.portal_rx = Some(pdf_open_request().send());
        } else {
            self.portal_contents_rx = Some(pdf_open_request().send_contents());
        }
        self.dirty = true;
    }

    /// Drain a contents answer to the open request: queue the bytes on the
    /// PDF owner as [`Self::drain_portal`] queues a path.
    fn drain_portal_contents(&mut self) {
        let Some(rx) = self.portal_contents_rx.as_ref() else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ContentsOutcome::Failed("the file chooser went away".into())
            }
        };
        self.portal_contents_rx = None;
        match outcome {
            ContentsOutcome::Picked(files) => {
                let Some(file) = files.into_iter().next() else {
                    self.finish_portal_action(false, None);
                    return;
                };
                let name = file.name.clone();
                // A file chosen by its contents has no file identity to name
                // a record of its own, so it opens only in a window that
                // marks up nothing yet.
                if self.holds_document() {
                    let refusal = format!(
                        "{name} keeps its markup in a window of its own; open it from Files"
                    );
                    self.status_note = Some(refusal.clone());
                    self.dirty = true;
                    self.finish_portal_action(false, Some(refusal));
                    return;
                }
                self.queue_worker_open_contents(&name, file.bytes);
                if self.pending_open.is_none() {
                    self.finish_portal_action(self.has_document, self.load_error.clone());
                }
                self.dirty = true;
            }
            ContentsOutcome::Cancelled => self.finish_portal_action(false, None),
            ContentsOutcome::Failed(reason) => {
                if !self.has_document {
                    self.load_error = Some(format!("Couldn't open the file chooser: {reason}"));
                }
                self.dirty = true;
                self.finish_portal_action(false, Some(reason));
            }
        }
    }

    /// Drain a completed portal request, if any. Called once per frame from
    /// [`Widget::sync`]; loads the chosen document, surfaces a failure on the
    /// empty state, and clears the in-flight channel. Never blocks — a still
    /// pending picker simply leaves the channel installed for the next frame.
    fn drain_portal(&mut self) {
        let Some(rx) = self.portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                match outcome {
                    PortalOutcome::Opened(path) => {
                        self.open_chosen(&path);
                        // The accepted open invocation settles only when its
                        // matching owner-worker completion arrives.  Treating
                        // “queued” as a failed open used to strand action
                        // history and announce a false error.
                        if self.pending_open.is_none() {
                            self.finish_portal_action(self.has_document, self.load_error.clone());
                        }
                    }
                    // A cancel is not an error — leave the current view intact.
                    PortalOutcome::Cancelled => self.finish_portal_action(false, None),
                    PortalOutcome::Failed(reason) => {
                        if !self.has_document {
                            self.load_error =
                                Some(format!("Couldn't open the file chooser: {reason}"));
                        }
                        self.dirty = true;
                        self.finish_portal_action(false, Some(reason));
                    }
                }
            }
            // Still waiting on the host dialog, or the worker vanished.
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
            }
        }
    }

    /// Whether this window's record marks up a document already: it shows
    /// one, is opening one, or has one pinned.
    fn holds_document(&self) -> bool {
        self.has_document || self.pending_open.is_some() || self.record_source.is_some()
    }

    /// Open the PDF the user chose at `path` on its own markup record, in
    /// this window in place of the document shown: the record a launch on
    /// the file opens, so its markup is the same however it was opened.
    /// Outside a host (the reader on its own, in a test) it opens here.
    fn open_chosen(&mut self, path: &std::path::Path) {
        if self.source_path.as_deref() == Some(path) {
            self.finish_portal_action(true, None);
            return;
        }
        if !self.ask_to_open_here(path) {
            self.queue_worker_open(path);
        }
    }

    /// Ask the host to show the PDF at `path` on its own markup record in
    /// this window; the effect that asked is answered with the host's
    /// answer ([`Self::follow_reopen`]). Whether a host can.
    fn ask_to_open_here(&mut self, path: &std::path::Path) -> bool {
        let Some(open_here) = self.open_here.clone() else {
            return false;
        };
        let outcome = open_here.open(&[path.display().to_string()]);
        self.pending_reopen = Some((path.to_path_buf(), outcome));
        self.status_note = Some("Opening PDF…".to_string());
        self.dirty = true;
        true
    }

    /// Answer the open this window asked of its host: shown (this reader
    /// closes next), or refused, which the person is told and the
    /// operation that asked fails with.
    fn follow_reopen(&mut self) {
        let Some(answer) = self
            .pending_reopen
            .as_ref()
            .and_then(|(_, outcome)| outcome.get())
        else {
            return;
        };
        let Some((path, _)) = self.pending_reopen.take() else {
            return;
        };
        match self.opened_here(&path, answer) {
            Ok(()) => self.finish_portal_action(true, None),
            Err(error) => self.finish_portal_action(false, Some(error)),
        }
    }

    /// Reopen the document the record's markup is made against from the
    /// content store, when the window was not launched on a file: the same
    /// document every window of this record shows.
    fn reopen_pinned_source(&mut self) {
        if self.reopen_requested || self.has_document || self.pending_open.is_some() {
            return;
        }
        let Some(source) = self.record_source.clone() else {
            return;
        };
        self.reopen_requested = true;
        let name = if source.name.is_empty() {
            "Document.pdf".to_string()
        } else {
            source.name.clone()
        };
        let Some(bytes) = self.content.read(&source.hash) else {
            self.load_error = Some(format!("{name} is not on this device yet"));
            self.dirty = true;
            return;
        };
        self.queue_worker_open_contents(&name, bytes);
    }

    /// Submit a replacement to the PDF owner: filesystem reads and `PDFium`
    /// parsing never run in `sync()` or an input handler. On success the
    /// view resets to page one at fit-page and the reader switches to that
    /// document's own markup record; on failure the empty state surfaces
    /// the reason.
    fn queue_worker_open(&mut self, path: &std::path::Path) {
        let Some(handle) = self.raster_handle.as_ref() else {
            self.record_open_failure(path, "the PDF worker could not be started");
            return;
        };
        match handle.open_path(path) {
            Ok(request) => self.begin_open(request),
            Err(error) => self.record_open_failure(path, &error.to_string()),
        }
    }

    /// Submit a file chosen or stored by its contents, named `name`.
    fn queue_worker_open_contents(&mut self, name: &str, bytes: Vec<u8>) {
        let path = std::path::Path::new(name);
        let Some(handle) = self.raster_handle.as_ref() else {
            self.record_open_failure(path, "the PDF worker could not be started");
            return;
        };
        match handle.open_contents(name, bytes) {
            Ok(request) => self.begin_open(request),
            Err(error) => self.record_open_failure(path, &error.to_string()),
        }
    }

    fn begin_open(&mut self, request: OpenRequest) {
        self.pending_open = Some(request);
        self.status_note = Some("Opening PDF…".to_string());
        self.dirty = true;
    }

    fn record_open_failure(&mut self, path: &std::path::Path, error: &str) {
        let error = format!("Could not open {}: {error}", path.display());
        if self.has_document {
            self.status_note = Some(error);
        } else {
            self.load_error = Some(error);
        }
        self.dirty = true;
    }

    /// Consume owner-worker completions at the frame boundary.  Request ids
    /// are authority tokens: an A result delivered after the user chose B is
    /// inert, including failures.  This keeps the readable old document on
    /// screen through both stale and failed replacements.
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive worker event boundary keeps stale-result filtering auditable"
    )]
    fn drain_raster_events(&mut self, state: &AutoCommit) {
        let events = self
            .raster_handle
            .as_ref()
            .map(RasterHandle::drain_events)
            .unwrap_or_default();
        for event in events {
            match event {
                RasterEvent::Opened {
                    request,
                    document,
                    name,
                    raster,
                    ..
                } if self
                    .pending_open
                    .as_ref()
                    .is_some_and(|pending| pending.id == request.id) =>
                {
                    self.pending_open = None;
                    self.doc = document;
                    self.form_fields.clone_from(&raster.form_fields);
                    self.source_blob = Some((raster.source_hash.clone(), raster.source_len));
                    self.existing_request = None;
                    self.reader_markup_hidden = false;
                    self.form_values.clear();
                    self.resource_page_rotations.clear();
                    self.raster_document = Some(raster);
                    self.raster_images.clear();
                    self.raster_render_pending.clear();
                    self.raster_tiles.clear();
                    self.raster_tile_batch.clear();
                    self.raster_text_pending.clear();
                    self.pending_worker_plan = None;
                    self.applied_worker_plan = None;
                    self.worker_plan_blobs.clear();
                    self.has_document = true;
                    self.seed_pending = true;
                    self.doc_name = name;
                    self.source_path = Some(request.path.clone());
                    self.load_error = None;
                    self.status_note = None;
                    self.fit = FitMode::Page;
                    self.viewport.set_scroll(0.0, 0.0);
                    self.current_page = 1;
                    self.resource_page_ids = (1..=self.doc.page_count())
                        .map(|page| format!("source-{page}"))
                        .collect();
                    self.resource_page_sources = (1..=self.doc.page_count()).collect();
                    self.resource_page_imports = vec![None; self.doc.page_count()];
                    self.source_pages = self.doc.pages.clone();
                    self.selected_anno = None;
                    self.selected_sig = None;
                    self.editing = None;
                    self.selection = None;
                    self.close_search();
                    self.find_bar = ui::widget::FindBar::new();
                    self.matches.clear();
                    self.raster_matches.clear();
                    self.raster_text.clear();
                    self.raster_links.clear();
                    self.raster_links_pending.clear();
                    self.text_lines.clear();
                    self.prev_overlay = None;
                    self.reconcile_overlay();
                    self.recompute_regions();
                    self.sync_viewport_frame();
                    self.rebuild_page_maps();
                    self.dirty = true;
                    if let Some(handle) = self.raster_handle.as_ref()
                        && let Err(error) = handle.activate_document(request.id)
                    {
                        self.status_note = Some(format!("PDF source activation failed: {error}"));
                    }
                    self.finish_portal_action(true, None);
                }
                RasterEvent::OpenFailed { request, error }
                    if self
                        .pending_open
                        .as_ref()
                        .is_some_and(|pending| pending.id == request.id) =>
                {
                    self.pending_open = None;
                    self.record_open_failure(&request.path, &error);
                    self.finish_portal_action(false, Some(error));
                }
                RasterEvent::OpenSuperseded { request }
                    if self
                        .pending_open
                        .as_ref()
                        .is_some_and(|pending| pending.id == request.id) =>
                {
                    // A newer accepted request owns the loading indicator.
                    self.pending_open = None;
                    self.dirty = true;
                }
                RasterEvent::Opened { request, .. } => {
                    // The worker may have completed a cancelled or superseded
                    // parse. Discard it explicitly; the accepted document's
                    // backend remains alive until a later activation.
                    let _ = self
                        .raster_handle
                        .as_ref()
                        .and_then(|handle| handle.discard_document(request.id).ok());
                }
                RasterEvent::Rendered { request, image }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|doc| doc.id == request.document_id) =>
                {
                    let key = (
                        request.page,
                        crate::source::RasterBackend::bucket_for(request.target_width),
                    );
                    self.raster_render_pending.remove(&key);
                    if let Some(image) = image {
                        if !self.raster_images.contains_key(&key)
                            && self.raster_images.len() >= MAX_UI_RASTER_IMAGES
                            && let Some(evicted) = self.raster_images.keys().next().copied()
                        {
                            self.raster_images.remove(&evicted);
                        }
                        self.raster_images.insert(key, image);
                    }
                    self.dirty = true;
                }
                RasterEvent::RenderFailed { request, error }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|doc| doc.id == request.document_id) =>
                {
                    let key = (
                        request.page,
                        crate::source::RasterBackend::bucket_for(request.target_width),
                    );
                    self.raster_render_pending.remove(&key);
                    self.status_note = Some(format!(
                        "Couldn’t render page {}: {error}",
                        request.page + 1
                    ));
                    self.dirty = true;
                }
                RasterEvent::TileRendered { request, image }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|doc| doc.id == request.document_id) =>
                {
                    self.raster_tile_batch.retain(|queued| *queued != request);
                    if let Some(image) = image {
                        self.raster_tiles.insert(UiTileKey::of(&request), image);
                    }
                    self.dirty = true;
                }
                RasterEvent::TileFailed { request, .. }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|doc| doc.id == request.document_id) =>
                {
                    // The placeholder keeps showing; the tile is asked for
                    // again only by a later batch.
                    self.raster_tile_batch.retain(|queued| *queued != request);
                    self.dirty = true;
                }
                RasterEvent::TextExtracted { request, text }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|doc| doc.id == request.document_id) =>
                {
                    self.raster_text_pending.remove(&request.page);
                    let text = self.text_for_card(request.page + 1, text);
                    self.raster_text.insert(request.page + 1, text);
                    if self.search_active {
                        // The bar finds again over the grown text, so the
                        // counter and the matches move together.
                        self.drive_find_bar();
                    }
                    self.dirty = true;
                }
                RasterEvent::LinksRead { request, links }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|doc| doc.id == request.document_id) =>
                {
                    self.raster_links_pending.remove(&request.page);
                    self.raster_links.insert(request.page + 1, links);
                    self.dirty = true;
                }
                RasterEvent::LinksFailed { request, .. }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|doc| doc.id == request.document_id) =>
                {
                    // A page whose links cannot be read simply has none.
                    self.raster_links_pending.remove(&request.page);
                    self.raster_links.insert(request.page + 1, Arc::default());
                }
                RasterEvent::TextFailed { request, error }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|doc| doc.id == request.document_id) =>
                {
                    self.raster_text_pending.remove(&request.page);
                    self.status_note = Some(format!(
                        "Couldn’t read text on page {}: {error}",
                        request.page + 1
                    ));
                    self.dirty = true;
                }
                RasterEvent::Reconciled {
                    request,
                    page_sizes,
                    geometries,
                    rotations,
                } if self
                    .raster_document
                    .as_ref()
                    .is_some_and(|doc| doc.id == request.document_id)
                    && self.pending_worker_plan.as_ref() == Some(&request.plan) =>
                {
                    self.pending_worker_plan = None;
                    self.applied_worker_plan = Some(request.plan);
                    self.raster_images.clear();
                    self.raster_render_pending.clear();
                    self.raster_tiles.clear();
                    self.raster_tile_batch.clear();
                    self.raster_text.clear();
                    self.raster_links.clear();
                    self.raster_links_pending.clear();
                    self.raster_text_pending.clear();
                    if let Some(document) = self.raster_document.as_mut() {
                        document.page_sizes = page_sizes;
                        document.geometries = geometries;
                        document.rotations = rotations;
                    }
                    self.rebuild_page_maps();
                    self.dirty = true;
                }
                RasterEvent::ReconcileFailed { request, error }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|doc| doc.id == request.document_id)
                        && self.pending_worker_plan.as_ref() == Some(&request.plan) =>
                {
                    self.pending_worker_plan = None;
                    self.status_note = Some(format!("Couldn’t apply shared page plan: {error}"));
                    self.dirty = true;
                }
                RasterEvent::Imported {
                    request,
                    hash,
                    kind,
                    sizes,
                } if self
                    .raster_document
                    .as_ref()
                    .is_some_and(|doc| doc.id == request.document_id)
                    && self.pending_import.as_ref() == Some(&request) =>
                {
                    let pages_before = self.doc.page_count();
                    let pages = sizes
                        .into_iter()
                        .enumerate()
                        .map(|(index, (width, height))| PagePlanEntry {
                            id: format!("import-{}", uuid::Uuid::new_v4()),
                            source_page: index + 1,
                            imported: Some(crate::markup::ImportedPage {
                                hash: hash.clone(),
                                kind: kind.clone(),
                                page: index + 1,
                                width_milli: (width * 1000.0).round() as u32,
                                height_milli: (height * 1000.0).round() as u32,
                            }),
                            rotation: 0,
                        })
                        .collect();
                    self.pending_import = None;
                    self.submit(crate::actions::import_pages_action(
                        request.after.as_deref(),
                        pages,
                    ));
                    self.reconcile_shared_page_plan(state);
                    self.go_to_page(request.at + 1);
                    self.status_note = Some(
                        "Imported pages are shared with collaborators; Save exports a flattened PDF"
                            .into(),
                    );
                    self.pages_dirty = self.doc.page_count() != pages_before;
                    self.dirty = true;
                }
                RasterEvent::ImportFailed { request, error }
                    if self.pending_import.as_ref() == Some(&request) =>
                {
                    self.pending_import = None;
                    self.status_note = Some(format!("Import failed: {error}"));
                    self.dirty = true;
                }
                RasterEvent::SavePrepared { request, temporary }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|doc| doc.id == request.document_id)
                        && self.pending_save.as_ref() == Some(&request) =>
                {
                    if let Err(error) = self
                        .raster_handle
                        .as_ref()
                        .ok_or_else(|| "the PDF save worker is unavailable".to_string())
                        .and_then(|handle| {
                            handle
                                .commit_save(request.clone(), temporary)
                                .map_err(|error| error.to_string())
                        })
                    {
                        self.pending_save = None;
                        self.status_note = Some(format!("Save failed: {error}"));
                        self.dirty = true;
                        self.finish_portal_action(false, Some(error));
                    }
                }
                RasterEvent::Saved { request }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|doc| doc.id == request.document_id)
                        && self.pending_save.as_ref() == Some(&request) =>
                {
                    self.pending_save = None;
                    let name = request
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("the file");
                    self.status_note = Some(format!("Saved annotated PDF to {name}"));
                    self.pages_dirty = false;
                    self.dirty = true;
                    self.finish_portal_action(true, None);
                }
                RasterEvent::SaveFailed { request, error }
                    if self.pending_save.as_ref() == Some(&request) =>
                {
                    self.pending_save = None;
                    self.status_note = Some(format!("Save failed: {error}"));
                    self.dirty = true;
                    self.finish_portal_action(false, Some(error));
                }
                RasterEvent::ExistingRead {
                    document_id,
                    existing,
                } if self
                    .raster_document
                    .as_ref()
                    .is_some_and(|document| document.id == document_id) =>
                {
                    if let Some(hash) = self.existing_request.clone() {
                        self.finish_adoption(state, &hash, &existing);
                    }
                }
                RasterEvent::ExistingFailed { document_id, error }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|document| document.id == document_id) =>
                {
                    self.status_note = Some(format!("Could not read the PDF's markup: {error}"));
                    self.dirty = true;
                }
                _ => {}
            }
        }
    }

    fn raster_active(&self) -> bool {
        self.raster_document.is_some()
    }

    fn cancel_pending_open(&mut self) {
        if let Some(request) = self.pending_open.take() {
            let _ = self
                .raster_handle
                .as_ref()
                .and_then(|handle| handle.discard_document(request.id).ok());
        }
        self.dirty = true;
    }

    /// The centred "Open a PDF…" button drawn on the empty state, as the last
    /// draw pass laid it out from its measured label (`draw_empty_state`), so
    /// the click handler hits exactly the painted pill. Empty until the first
    /// frame and while an open is pending (nothing to click).
    fn empty_open_button_rect(&self) -> Rect {
        self.empty_open_button.get()
    }
}

impl Default for PdfApp {
    fn default() -> Self {
        Self::empty()
    }
}

/// Ensure a chosen save path ends in `.pdf` (some portals return a name
/// without the extension when the user typed it bare).
fn ensure_pdf_extension(path: std::path::PathBuf) -> std::path::PathBuf {
    let is_pdf = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
    if is_pdf {
        path
    } else {
        let mut s = path.into_os_string();
        s.push(".pdf");
        std::path::PathBuf::from(s)
    }
}

include!("app_draw.rs");
include!("app_events.rs");

impl Widget for PdfApp {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if self.region_copy.failed() {
            self.status_note = Some("Selected a region (clipboard unavailable)".to_string());
            self.dirty = true;
        }
        let (state, _) = ui::doc::document_ref(state, path);
        // Drain the dedicated PDF owner before any portal/action work.  This
        // is non-blocking and is the sole route by which production PDFium
        // results enter widget state.
        self.drain_raster_events(state);
        self.drain_opens();
        self.follow_cancellation();
        self.follow_reopen();
        self.follow_signature();
        self.reopen_pinned_source();
        // Pull in any file the host portal picker returned since the last
        // frame (off-thread; never blocks the UI).
        self.drain_portal();
        self.drain_portal_contents();
        // And any completed save-location choice, flattening markup to it.
        self.drain_portal_save();
        // And any completed import (a PDF/image chosen to add as pages).
        self.drain_portal_import();

        if self.has_document {
            if std::mem::take(&mut self.seed_pending) {
                self.seed_record(state);
            }
            self.reconcile_shared_page_plan(state);
        }
        if std::mem::take(&mut self.staged_import) {
            self.demo_import_image();
        }
        self.reload(state);
        self.sync_reader_markup(state);
        self.refresh_search_entries(state);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        self.reconcile_overlay();
        self.recompute_regions();
        self.sync_viewport_frame();
        self.current_page = self.nearest_page();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.draw_all(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.context_menu.is_some() {
            let (selected_target, still_open) = {
                match self.context_menu.as_mut() {
                    Some(menu) => {
                        menu.widget.handle_event(event, ctx);
                        let selected = menu.widget.take_selection().map(|_| menu.target.clone());
                        (selected, menu.widget.is_open())
                    }
                    None => (None, false),
                }
            };
            if let Some(target) = selected_target {
                self.delete_target(&target, &mut ctx.state);
                self.context_menu = None;
                *ctx.handled = true;
                return;
            }
            if !still_open {
                self.context_menu = None;
            }
            let modal_event = matches!(
                event,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::Ime(_)
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::MouseInput {
                        button: MouseButton::Left,
                        ..
                    }
            );
            if *ctx.handled || modal_event {
                *ctx.handled = true;
                return;
            }
        }
        self.on_event(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            &mut ctx.state,
            ctx.handled,
        );
    }

    fn name(&self) -> &'static str {
        "PdfApp"
    }

    fn traps_focus(&self) -> bool {
        self.toolbar.shown_menu().is_some()
            || self
                .marker_colours
                .as_ref()
                .is_some_and(Widget::traps_focus)
            || self
                .context_menu
                .as_ref()
                .is_some_and(|menu| menu.widget.traps_focus())
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.toolbar
            .modal_capture()
            .or_else(|| self.marker_colours.as_ref().and_then(Widget::modal_capture))
            .or_else(|| {
                self.context_menu
                    .as_ref()
                    .and_then(|menu| menu.widget.modal_capture())
            })
    }

    /// The marker-colour popover and the context menu float in the top
    /// layer, above the pages and the panels.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.toolbar
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        if let Some(popover) = self.marker_colours.as_mut() {
            popover.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        if let Some(menu) = self.context_menu.as_mut() {
            menu.widget
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    /// The reader paints its document chrome and pages directly, so expose the
    /// otherwise invisible structure explicitly.  The navigation controls use
    /// the same `PdfEffect` handler as the toolbar and Ctrl+K resources;
    /// assistive technology never takes a separate, untested shortcut.
    #[allow(
        clippy::too_many_lines,
        reason = "the custom-painted workspace needs one complete semantic mirror of every visible region"
    )]
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        let pages = self.doc.page_count();
        let current = self.current_page.clamp(1, pages.max(1));
        let focusable = || AccessState::new().focusable(true);
        let button = |name: &str, target: &str, bounds: Rect| {
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_action_target(target)
                .with_bounds(bounds)
                .with_state(focusable())
        };
        let opening = self.opening();
        if !self.has_document {
            let mut children = vec![
                AccessNode::new(Role::Heading { level: 1 })
                    .with_name(if opening { "Opening PDF" } else { "Open a PDF" })
                    .with_hashed_key("pdf.empty.heading"),
            ];
            if opening {
                children.push(
                    AccessNode::new(Role::StaticText)
                        .with_name("Opening PDF")
                        .with_hashed_key("pdf.empty.loading")
                        .with_live(Politeness::Polite),
                );
            } else {
                children.push(button("Open PDF", "pdf.open", self.canvas_rect));
            }
            if let Some(error) = &self.load_error {
                children.push(
                    AccessNode::new(Role::StaticText)
                        .with_name(error)
                        .with_hashed_key("pdf.empty.error")
                        .with_live(Politeness::Assertive),
                );
            }
            return Some(
                AccessNode::new(Role::Window)
                    .with_name("PDF Viewer")
                    .with_bounds(self.rect)
                    .with_children(children),
            );
        }

        let layout = self.toolbar_layout();
        let mut controls = vec![
            button("Open PDF", "pdf.open", layout.open),
            button("Save annotated PDF", "pdf.save", layout.save),
            button("Previous page", "pdf.previous-page", self.bubble_prev)
                .with_state(focusable().disabled(current <= 1)),
            button("Next page", "pdf.next-page", self.bubble_next)
                .with_state(focusable().disabled(current >= pages)),
            button("Zoom out", "pdf.zoom-out", layout.zoom_out),
            button("Zoom in", "pdf.zoom-in", layout.zoom_in),
            button("Fit page or width", "pdf.fit", layout.fit),
            button("Find in document", "pdf.search.open", layout.search),
            AccessNode::new(Role::ToggleButton)
                .with_name("Page thumbnails")
                .with_action_target("pdf.toggle-rail")
                .with_bounds(layout.toggle_rail)
                .with_state(focusable().checked(self.show_rail)),
            AccessNode::new(Role::ToggleButton)
                .with_name("Review sidebar")
                .with_action_target("pdf.toggle-sidebar")
                .with_bounds(layout.toggle_sidebar)
                .with_state(focusable().checked(self.show_sidebar)),
        ];
        // Commands that went to the More menu keep their nodes (zero bounds)
        // and actions; the More button, and its menu while open, come from
        // the toolbar itself.
        if let Some(bar) = ui::widget::Widget::accessibility(&self.toolbar) {
            controls.extend(bar.children.into_iter().filter(|child| {
                child.role == Role::Menu
                    || child.action_target.as_deref() == Some("toolbar.overflow")
            }));
        }
        controls.extend(layout.tools.iter().map(|(tool, bounds)| {
            AccessNode::new(Role::ToggleButton)
                .with_name(tool.label())
                .with_description(format!("Annotation tool; shortcut {}", tool.hotkey()))
                .with_action_target(format!("pdf.tool:{}", tool.hotkey()))
                .with_bounds(*bounds)
                .with_state(focusable().checked(self.tool == *tool))
        }));
        // The marker colours are a radio group behind the well: each colour
        // is chosen directly, whether or not the popover is showing.
        if let Some(well) = layout.colour {
            controls.push(
                AccessNode::new(Role::Group)
                    .with_name("Marker colour")
                    .with_hashed_key("pdf.marker-colour")
                    .with_bounds(well)
                    .with_children(
                        Swatch::ALL
                            .iter()
                            .map(|swatch| {
                                AccessNode::new(Role::RadioButton)
                                    .with_name(format!(
                                        "{} annotation colour",
                                        swatch_label(*swatch)
                                    ))
                                    .with_action_target(format!("pdf.swatch:{}", swatch.key()))
                                    .with_bounds(well)
                                    .with_state(focusable().checked(self.swatch == *swatch))
                            })
                            .collect(),
                    ),
            );
        }

        let page_nodes = self
            .page_sidebar
            .thumb_rects()
            .into_iter()
            .map(|(index, bounds)| {
                let page_id = self
                    .resource_page_ids
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("page-{}", index + 1));
                AccessNode::new(Role::ListItem)
                    .with_name(format!("Page {}", index + 1))
                    .with_description(if self.pages_with_markup().contains(&(index + 1)) {
                        "Contains review markup"
                    } else {
                        "No review markup"
                    })
                    .with_action_target(format!("pdf.page:{page_id}"))
                    .with_bounds(bounds)
                    .with_position(index + 1, pages)
                    .with_state(focusable().selected(index + 1 == current))
            })
            .collect();
        let page_list = AccessNode::new(Role::List)
            .with_name("Pages")
            .with_hashed_key("pdf.pages")
            .with_children(page_nodes);

        let mut content_children = vec![
            AccessNode::new(Role::Heading { level: 1 }).with_name(if self.doc_name.is_empty() {
                "PDF document".to_string()
            } else {
                self.doc_name.clone()
            }),
            AccessNode::new(Role::Document)
                .with_name(format!("Page {current} of {pages}"))
                .with_description(
                    "PDF page canvas. Arrow keys scroll; Page Up and Page Down change pages.",
                )
                .with_value(format!("Zoom {:.0}%", self.zoom() * 100.0))
                .with_bounds(self.canvas_rect)
                .with_state(focusable()),
        ];

        if self.search_active {
            let count = self.match_count();
            let result_nodes = (0..count.min(100))
                .map(|index| {
                    let page = if self.raster_active() {
                        self.raster_matches.get(index).map_or(1, |m| m.page)
                    } else {
                        self.matches.get(index).map_or(1, |m| m.page)
                    };
                    AccessNode::new(Role::ListItem)
                        .with_name(format!("Match {} on page {page}", index + 1))
                        .with_action_target(format!("pdf.match:{index}"))
                        .with_position(index + 1, count)
                        .with_state(focusable().selected(index == self.active_match))
                })
                .collect();
            let mut find = vec![
                AccessNode::new(Role::TextInput)
                    .with_name("Find in document")
                    .with_value(self.query())
                    .with_action_target("pdf.search.input")
                    .with_bounds(self.search_rect)
                    .with_state(
                        focusable().focused(ui::widget::Widget::is_focused(&self.find_bar)),
                    ),
            ];
            find.extend(ui::widget::Widget::accessibility(&self.find_bar));
            content_children.push(
                AccessNode::new(Role::Group)
                    .with_name("Find in document")
                    .with_children(vec![
                        AccessNode::new(Role::Group)
                            .with_name("Find")
                            .with_children(find),
                        AccessNode::new(Role::StaticText)
                            .with_name(format!("{count} matches"))
                            .with_hashed_key("pdf.search.count")
                            .with_live(Politeness::Polite),
                        AccessNode::new(Role::List)
                            .with_name("Search results")
                            .with_hashed_key("pdf.search.results")
                            .with_children(result_nodes),
                    ]),
            );
        }

        if self.show_sidebar {
            let tabs = self
                .sidebar_tab_rects()
                .into_iter()
                .map(|(tab, bounds)| {
                    let (name, tag) = match tab {
                        SidebarTab::Comments => ("Comments", "comments"),
                        SidebarTab::Outline => ("Contents", "contents"),
                        SidebarTab::Signatures => ("Signatures", "signatures"),
                    };
                    AccessNode::new(Role::Tab)
                        .with_name(name)
                        .with_action_target(format!("pdf.tab:{tag}"))
                        .with_bounds(bounds)
                        .with_state(focusable().selected(tab == self.sidebar_tab))
                })
                .collect();
            let panel_children = match self.sidebar_tab {
                SidebarTab::Outline => self
                    .doc
                    .outline
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        AccessNode::new(Role::TreeItem)
                            .with_name(item.title.clone())
                            .with_value(format!("Page {}", item.page))
                            .with_level(usize::from(item.level) + 1)
                            .with_action_target(format!("pdf.outline:{index}"))
                            .with_state(focusable().selected(item.page == current))
                    })
                    .collect(),
                SidebarTab::Comments => self
                    .comments
                    .iter()
                    .map(|comment| {
                        AccessNode::new(Role::ListItem)
                            .with_name(if comment.body.trim().is_empty() {
                                "Empty comment".into()
                            } else {
                                comment.body.clone()
                            })
                            .with_description(format!(
                                "{} on page {}; {} replies",
                                self.author_name(&comment.author),
                                comment.page,
                                comment.replies.len()
                            ))
                            .with_action_target(format!("pdf.comment:{}.open", comment.id))
                            .with_state(focusable().selected(
                                comment.anno_id.as_deref() == self.selected_anno.as_deref(),
                            ))
                            .with_children(vec![
                                button(
                                    "Edit comment",
                                    &format!("pdf.comment:{}.edit", comment.id),
                                    Rect::ZERO,
                                ),
                                button(
                                    "Reply",
                                    &format!("pdf.comment:{}.reply", comment.id),
                                    Rect::ZERO,
                                ),
                                AccessNode::new(Role::ToggleButton)
                                    .with_name(if comment.resolved {
                                        "Reopen comment"
                                    } else {
                                        "Resolve comment"
                                    })
                                    .with_action_target(format!(
                                        "pdf.comment:{}.resolve",
                                        comment.id
                                    ))
                                    .with_state(focusable().checked(comment.resolved)),
                                button(
                                    "Delete comment",
                                    &format!("pdf.comment:{}.delete", comment.id),
                                    Rect::ZERO,
                                ),
                            ])
                    })
                    .collect(),
                SidebarTab::Signatures => {
                    let mut nodes: Vec<AccessNode> = vec![
                        AccessNode::new(Role::TextInput)
                            .with_name("Signer name")
                            .with_value(self.signer_name.clone())
                            .with_action_target("pdf.signer")
                            .with_state(focusable()),
                        AccessNode::new(Role::RadioButton)
                            .with_name("Type signature")
                            .with_action_target("pdf.signature-mode:type")
                            .with_state(focusable().checked(self.sig_mode == SigMode::Type)),
                        AccessNode::new(Role::RadioButton)
                            .with_name("Draw signature")
                            .with_action_target("pdf.signature-mode:draw")
                            .with_state(focusable().checked(self.sig_mode == SigMode::Draw)),
                        button("Use signature on page", "pdf.signature.use", Rect::ZERO),
                    ];
                    nodes.extend(self.signatures.iter().map(|signature| {
                        AccessNode::new(Role::ListItem)
                            .with_name(format!("Signature by {}", signature.signer))
                            .with_description(format!(
                                "Signed {} on page {}",
                                signature.date, signature.page
                            ))
                            .with_action_target(format!("pdf.signature:{}.open", signature.id))
                            .with_state(focusable().selected(
                                self.selected_sig.as_deref() == Some(signature.id.as_str()),
                            ))
                            .with_children(vec![button(
                                "Delete signature",
                                &format!("pdf.signature:{}.delete", signature.id),
                                Rect::ZERO,
                            )])
                    }));
                    nodes
                }
            };
            content_children.push(
                AccessNode::new(Role::Group)
                    .with_name("Review sidebar")
                    .with_children(vec![
                        AccessNode::new(Role::TabBar)
                            .with_name("Review panels")
                            .with_children(tabs),
                        AccessNode::new(Role::TabPanel)
                            .with_name(match self.sidebar_tab {
                                SidebarTab::Comments => "Comments",
                                SidebarTab::Outline => "Contents",
                                SidebarTab::Signatures => "Signatures",
                            })
                            .with_children(panel_children),
                    ]),
            );
        }
        if let Some(status) = &self.status_note {
            content_children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(status)
                    .with_live(Politeness::Polite),
            );
        }
        let mut window_children = vec![
            AccessNode::new(Role::Toolbar)
                .with_name("Document tools")
                .with_hashed_key("pdf.toolbar")
                .with_children(controls),
            page_list,
            AccessNode::new(Role::Group)
                .with_name("Document")
                .with_hashed_key("pdf.content")
                .with_children(content_children),
        ];
        if let Some(menu) = self.context_menu.as_ref()
            && let Some(menu_node) = menu.widget.accessibility()
        {
            window_children.push(menu_node);
        }
        Some(
            AccessNode::new(Role::Window)
                .with_name("PDF Viewer")
                .with_bounds(self.rect)
                .with_children(window_children),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one target router mirrors the single synthetic accessibility tree"
    )]
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action as A11yAction;
        let (state, path) = ui::doc::document(state, path);
        if target.starts_with("context-menu:item:") {
            let Some(menu) = self.context_menu.as_mut() else {
                return false;
            };
            let handled = menu
                .widget
                .perform_semantic_action(target, action, state, path);
            let selected = menu.widget.take_selection().map(|_| menu.target.clone());
            let still_open = menu.widget.is_open();
            if let Some(target) = selected {
                self.delete_target(&target, state);
                self.context_menu = None;
                return true;
            }
            if !still_open {
                self.context_menu = None;
            }
            return handled;
        }
        if target.starts_with("toolbar.") {
            let handled = ui::widget::Widget::perform_semantic_action(
                &mut self.toolbar,
                target,
                action,
                state,
                path,
            );
            self.drain_toolbar();
            return handled;
        }
        if target.starts_with("find-") {
            let handled = ui::widget::Widget::perform_semantic_action(
                &mut self.find_bar,
                target,
                action,
                state,
                path,
            );
            self.drive_find_bar();
            return handled;
        }
        if target == "pdf.search.input" {
            match action {
                A11yAction::Activate | A11yAction::Focus => self.open_search(),
                A11yAction::SetValue(value) => {
                    self.open_search();
                    self.find_bar.set_query(value);
                    self.drive_find_bar();
                }
                _ => return false,
            }
            return true;
        }
        if target == "pdf.signer" {
            match action {
                A11yAction::Activate | A11yAction::Focus => {
                    let value = self.signer_name.clone();
                    self.begin_edit(EditTarget::Signer, &value);
                }
                A11yAction::SetValue(value) => {
                    self.signer_name.clone_from(value);
                    self.begin_edit(EditTarget::Signer, value);
                }
                _ => return false,
            }
            return true;
        }
        let activates = matches!(action, A11yAction::Activate | A11yAction::ToggleCheck);
        if !activates && !matches!(action, A11yAction::ScrollIntoView | A11yAction::Focus) {
            return false;
        }

        if let Some(page_id) = target.strip_prefix("pdf.page:") {
            let Some(page) = self
                .resource_page_ids
                .iter()
                .position(|id| id == page_id)
                .map(|index| index + 1)
            else {
                return false;
            };
            self.go_to_page(page);
            return true;
        }
        if let Some(index) = target
            .strip_prefix("pdf.match:")
            .and_then(|v| v.parse::<usize>().ok())
        {
            if index >= self.match_count() {
                return false;
            }
            self.active_match = index;
            let page = if self.raster_active() {
                self.raster_matches.get(index).map(|hit| hit.page)
            } else {
                self.matches.get(index).map(|hit| hit.page)
            };
            if let Some(page) = page {
                self.go_to_page(page);
            }
            return true;
        }
        if let Some(index) = target
            .strip_prefix("pdf.outline:")
            .and_then(|v| v.parse::<usize>().ok())
        {
            if let Some(page) = self.doc.outline.get(index).map(|item| item.page) {
                self.go_to_page(page);
                return true;
            }
            return false;
        }
        if let Some(tag) = target.strip_prefix("pdf.tool:") {
            let tool = match tag {
                "1" => Tool::Select,
                "2" => Tool::Highlight,
                "3" => Tool::Ink,
                "4" => Tool::Note,
                "5" => Tool::Sign,
                "6" => Tool::Text,
                _ => return false,
            };
            self.set_tool(tool);
            return true;
        }
        if let Some(name) = target.strip_prefix("pdf.swatch:") {
            self.swatch = Swatch::parse(name);
            if let Some(id) = self.selected_anno.clone() {
                self.perform_action(
                    state,
                    crate::actions::set_annotation_color_action(&id, self.swatch),
                );
                self.reload(state);
            }
            self.dirty = true;
            return true;
        }
        if let Some(tab) = target.strip_prefix("pdf.tab:") {
            let next = match tab {
                "comments" => SidebarTab::Comments,
                "contents" => SidebarTab::Outline,
                "signatures" => SidebarTab::Signatures,
                _ => return false,
            };
            if self.sidebar_tab != next {
                self.sidebar_scroll = 0.0;
                self.sidebar_reveal = None;
            }
            self.sidebar_tab = next;
            self.show_sidebar = true;
            self.reflow();
            return true;
        }
        if let Some(mode) = target.strip_prefix("pdf.signature-mode:") {
            self.sig_mode = match mode {
                "type" => SigMode::Type,
                "draw" => SigMode::Draw,
                _ => return false,
            };
            self.sig_stroke = None;
            self.dirty = true;
            return true;
        }
        if let Some(rest) = target.strip_prefix("pdf.comment:") {
            let Some((id, verb)) = rest.rsplit_once('.') else {
                return false;
            };
            let Some(comment) = self
                .comments
                .iter()
                .find(|comment| comment.id == id)
                .cloned()
            else {
                return false;
            };
            match verb {
                "open" => {
                    self.selected_anno = comment.anno_id;
                    self.go_to_page(comment.page);
                }
                "edit" => {
                    self.begin_edit(EditTarget::CommentBody(id.into()), &comment.body);
                }
                "reply" => self.begin_edit(EditTarget::Reply(id.into()), ""),
                "resolve" => {
                    self.perform_action(
                        state,
                        crate::actions::set_comment_resolved_action(id, !comment.resolved),
                    );
                    self.reload(state);
                }
                "delete" => self.delete_target(&DeleteTarget::Comment(id.into()), state),
                _ => return false,
            }
            return true;
        }
        if let Some(rest) = target.strip_prefix("pdf.signature:") {
            let Some((id, verb)) = rest.rsplit_once('.') else {
                return false;
            };
            let Some(signature) = self
                .signatures
                .iter()
                .find(|signature| signature.id == id)
                .cloned()
            else {
                return false;
            };
            match verb {
                "open" => {
                    self.selected_sig = Some(id.into());
                    self.selected_anno = None;
                    self.go_to_page(signature.page);
                }
                "delete" => self.delete_target(&DeleteTarget::Signature(id.into()), state),
                _ => return false,
            }
            return true;
        }

        match target {
            "pdf.open" => {
                self.request_open_via_portal();
                return true;
            }
            "pdf.save" => {
                self.request_save_via_portal();
                return true;
            }
            "pdf.toggle-rail" => {
                self.toggle_rail();
                return true;
            }
            "pdf.search.open" => {
                self.open_search();
                return true;
            }
            "pdf.search.whole-word" | "pdf.search.case-sensitive" => {
                let part = if target == "pdf.search.whole-word" {
                    "find-WholeWord"
                } else {
                    "find-MatchCase"
                };
                self.open_search();
                let toggled = ui::widget::Widget::perform_semantic_action(
                    &mut self.find_bar,
                    part,
                    &A11yAction::Activate,
                    state,
                    path,
                );
                self.drive_find_bar();
                return toggled;
            }
            "pdf.fit" => {
                if self.fit == FitMode::Width {
                    self.fit_page();
                } else {
                    self.fit_width();
                }
                return true;
            }
            "pdf.signature.use" => {
                self.set_tool(Tool::Sign);
                return true;
            }
            _ => {}
        }
        let effect = match target {
            "pdf.previous-page" => PdfEffect::PreviousPage,
            "pdf.next-page" => PdfEffect::NextPage,
            "pdf.zoom-in" => PdfEffect::ZoomIn,
            "pdf.zoom-out" => PdfEffect::ZoomOut,
            "pdf.toggle-sidebar" => PdfEffect::ToggleSidebar,
            _ => return false,
        };
        self.show(&effect)
    }

    fn test_id(&self) -> Option<&str> {
        Some("pdf_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.search_active
            && self.search_rect.contains(point)
            && let Some(cursor) = ui::widget::Widget::cursor_shape(&self.find_bar, point)
        {
            return Some(cursor);
        }
        if self.zoom_rect_contains(point) {
            return Some(winit::window::CursorIcon::Pointer);
        }
        if self.on_rail_grip(point) {
            return Some(winit::window::CursorIcon::ColResize);
        }
        self.link_under(point)
            .map(|_| winit::window::CursorIcon::Pointer)
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.search_active
            .then(|| ui::widget::Widget::ime_cursor_rect(&self.find_bar))
            .flatten()
    }

    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        self.search_active
            .then(|| ui::widget::Widget::ime_content(&self.find_bar))
            .flatten()
    }

    fn has_pending_visual_work(&self) -> bool {
        self.pending_open.is_some() || self.pending_visuals
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.drag.is_some() || self.pan.is_some() || self.rail_drag.is_some()
    }

    fn needs_animation_frame(&self) -> bool {
        // Tick only while a hover is counting toward the tooltip's
        // reveal delay. Once the tooltip is shown (or the pointer isn't over a
        // button at all) no further frames are needed, so an idle window
        // settles to zero frames instead of redrawing forever. The pointer
        // leaving a button arrives as a real event, which repaints and clears
        // the popover on its own.
        let owner_busy = self.portal_contents_rx.is_some()
            || self.pending_reopen.is_some()
            || self
                .raster_handle
                .as_ref()
                .is_some_and(crate::source::RasterHandle::wants_frames);
        if self.reduce_motion {
            return owner_busy;
        }
        // Also tick while the PDF owner is busy, so its results are drained
        // and painted without waiting for an input event. Also keep frames
        // coming while the floating page bubble is lingering /
        // fading, so it hides itself without needing a stray pointer event.
        self.tooltip_pending || owner_busy || self.page_bubble.needs_frame(ui::time::now())
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.reduce_motion = reduced;
        if reduced {
            self.tooltip_pending = false;
            self.page_bubble = ui::StatusBubble::new().with_linger(Duration::ZERO);
        } else {
            self.page_bubble = ui::StatusBubble::new();
        }
        self.dirty = true;
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

impl PdfApp {
    /// The search entries of the open document; none without one.
    pub(crate) fn shown_search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        if self.has_document {
            self.search_entries.clone()
        } else {
            Vec::new()
        }
    }
}

impl PdfApp {
    /// The open document's live rows: its outline and the current find
    /// hits, read from the PDF and the find bar rather than the record.
    pub(crate) fn document_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.pdf_resource_ctx()
            .map(|ctx| crate::actions::live_rows(&ctx))
            .unwrap_or_default()
    }

    /// The live row `id`, beyond the bounded page listed.
    pub(crate) fn document_row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        self.pdf_resource_ctx()
            .and_then(|ctx| crate::actions::live_row(&ctx, id))
    }

    /// Answer a view effect: move through the document or change how it is
    /// shown. Whether `effect` is one.
    pub(crate) fn show(&mut self, effect: &PdfEffect) -> bool {
        match effect {
            PdfEffect::PreviousPage => self.prev_page(),
            PdfEffect::NextPage => self.next_page(),
            PdfEffect::ZoomIn => self.zoom_by(1.15),
            PdfEffect::ZoomOut => self.zoom_by(1.0 / 1.15),
            PdfEffect::FitPage => self.fit_page(),
            PdfEffect::FitWidth => self.fit_width(),
            PdfEffect::ToggleSidebar => self.toggle_sidebar(),
            PdfEffect::TogglePages => self.toggle_rail(),
            PdfEffect::Find => self.open_search(),
            PdfEffect::GoTo { page } => self.go_to_page(*page),
            PdfEffect::MarkHighlight | PdfEffect::MarkUnderline | PdfEffect::MarkStrikeout => {
                self.markup_style = match effect {
                    PdfEffect::MarkUnderline => model::MarkupStyle::Underline,
                    PdfEffect::MarkStrikeout => model::MarkupStyle::StrikeOut,
                    _ => model::MarkupStyle::Highlight,
                };
                self.tool = Tool::Highlight;
                self.dirty = true;
            }
            PdfEffect::GoToMatch {
                query,
                whole_word,
                case_sensitive,
                index,
                page,
            } => self.go_to_search_result(query, *whole_word, *case_sensitive, *index, *page),
            PdfEffect::OpenPdf { .. } | PdfEffect::SaveAs { .. } | PdfEffect::ReadText { .. } => {
                return false;
            }
        }
        true
    }

    /// Answer `effect`, invoked by a button, the palette or an agent. Opening
    /// and saving wait on a picker, the PDF owner or the host and answer
    /// through `reply` when they finish.
    ///
    /// # Errors
    /// Returns why the effect cannot run now.
    /// Open the PDF at `path`, or the one the person chooses, in this
    /// window: on its own markup record, which the host shows in place of
    /// this one, answered once it does or why it could not. Where files
    /// have no paths the chosen bytes open here, in a window that marks up
    /// nothing yet.
    pub(crate) fn open_effect(
        &mut self,
        path: Option<String>,
        cx: &mut ui::framework::EffectCx<crate::PdfMarkup, Self>,
    ) -> ui::framework::EffectAnswer {
        if self.portal_action.is_some()
            || self.pending_open.is_some()
            || self.pending_reopen.is_some()
        {
            return Err("another PDF open is already in progress".into());
        }
        if let Some(path) = path {
            return self.open_here_effect(path.into(), cx);
        }
        if tilo_paths::available(tilo_paths::Feature::NativePaths) {
            return Ok(
                cx.choose_path(pdf_open_request(), |app, chosen, cx| match chosen {
                    Some(path) => app.open_here_effect(path, cx),
                    None => Err("file selection cancelled".into()),
                }),
            );
        }
        self.answer_effect(&PdfEffect::OpenPdf { path: None }, || cx.reply())
    }

    /// Ask the host to show the PDF at `path` on its own markup record in
    /// this window, and answer with whether it does.
    fn open_here_effect(
        &mut self,
        path: std::path::PathBuf,
        cx: &mut ui::framework::EffectCx<crate::PdfMarkup, Self>,
    ) -> ui::framework::EffectAnswer {
        use ui::framework::Effected;
        if self.source_path.as_deref() == Some(path.as_path()) {
            return Ok(Effected::done());
        }
        let Some(open_here) = self.open_here.clone() else {
            return Err("this window shows the document it was opened on".into());
        };
        self.status_note = Some("Opening PDF…".to_string());
        self.dirty = true;
        let arguments = [path.display().to_string()];
        Ok(cx.open_here(&open_here, &arguments, move |app, answer, _| {
            app.opened_here(&path, answer).map(|()| Effected::done())
        }))
    }

    /// Take up the host's answer to showing the PDF at `path` here: the
    /// person is told why it could not, and the open fails with it.
    fn opened_here(
        &mut self,
        path: &std::path::Path,
        answer: Result<(), String>,
    ) -> Result<(), String> {
        self.dirty = true;
        let Err(reason) = answer else {
            return Ok(());
        };
        let error = format!("Could not open {}: {reason}", path.display());
        if self.has_document {
            self.status_note = Some(error.clone());
        } else {
            self.status_note = None;
            self.load_error = Some(error.clone());
        }
        Err(error)
    }

    /// The text of pages `from_page..=to_page` (1-based) of the PDF as its
    /// file holds them, at most [`MOST_READ_PAGES`] of them.
    pub(crate) fn read_text(
        &self,
        from_page: Option<usize>,
        to_page: Option<usize>,
    ) -> Result<serde_json::Value, String> {
        let path = self
            .source_path
            .as_deref()
            .ok_or("this PDF has no file to read its text from yet; save it first")?;
        // The portable engine is memory-safe and runs in process; reading
        // text needs nothing only the sandboxed one draws.
        let document = tilo_pdf::Engine::portable()
            .open_path(path)
            .map_err(|error| format!("the PDF cannot be read: {error}"))?;
        let count = document.page_count();
        let first = from_page.unwrap_or(1).max(1);
        if first > count {
            return Err(format!("the PDF has {count} pages"));
        }
        let last = to_page
            .unwrap_or(usize::MAX)
            .min(first.saturating_add(MOST_READ_PAGES - 1))
            .min(count);
        let mut pages = Vec::new();
        for page in first..=last {
            let text = document
                .text(page - 1)
                .map_err(|error| format!("page {page} cannot be read: {error}"))?;
            pages.push(serde_json::json!({ "page": page, "text": text.text().trim_end() }));
        }
        Ok(serde_json::json!({
            "file": path.display().to_string(),
            "page_count": count,
            "pages": pages,
            "next": (last < count).then_some(last + 1),
        }))
    }

    pub(crate) fn answer_effect(
        &mut self,
        effect: &PdfEffect,
        reply: impl FnOnce() -> ui::framework::EffectReply,
    ) -> ui::framework::EffectAnswer {
        use ui::framework::Effected;
        if self.show(effect) {
            return Ok(Effected::done());
        }
        let opening = matches!(effect, PdfEffect::OpenPdf { .. });
        if opening && (self.portal_action.is_some() || self.pending_open.is_some()) {
            return Err("another PDF open is already in progress".into());
        }
        if !opening && (self.portal_action.is_some() || self.pending_save.is_some()) {
            return Err("another PDF save is already in progress".into());
        }
        match effect {
            PdfEffect::SaveAs { path: Some(file) } => {
                let file = ensure_pdf_extension(file.into());
                if !self.save_annotated_to(&file) {
                    return Err("PDF could not be saved".into());
                }
                if self.pending_save.is_none() {
                    return Ok(Effected::done());
                }
            }
            PdfEffect::OpenPdf { path: None } => {
                self.request_open_via_portal();
                if self.portal_contents_rx.is_none() {
                    return Err("PDF picker could not be started".into());
                }
            }
            PdfEffect::SaveAs { path: None } => {
                self.request_save_via_portal();
                if self.portal_save_rx.is_none() {
                    return Err("PDF save picker could not be started".into());
                }
            }
            _ => return Ok(Effected::done()),
        }
        self.portal_action = Some(reply());
        Ok(Effected::Pending)
    }
}

#[cfg(test)]
mod records_tests;

#[cfg(test)]
mod adopt_tests;

#[cfg(test)]
mod links_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::Size;
    use ui::search::SearchProvider as _;

    /// The app's search source as the framework builds it from what the open
    /// document says.
    fn searched(app: &PdfApp) -> impl ui::search::SearchProvider + use<> {
        ui::framework::testing::search_source::<crate::PdfMarkup>(app.shown_search_entries())
    }
    use crate::model::Reply;
    use serial_test::serial;
    use ui::headless::{Framed, viewport};

    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: Duration = Duration::from_secs(30);
    use tilo_automerge::ApplyIncoming;

    /// Two real PDFs on disk, from the bundled fixture. "Open a second
    /// document" has to be a second *file* — the bug is about one document's
    /// markup reaching another, and the same path twice is one record.
    fn two_documents() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let a = dir.path().join("a.pdf");
        let b = dir.path().join("b.pdf");
        let fixture = concat!(env!("OUT_DIR"), "/sample.pdf");
        std::fs::copy(fixture, &a).expect("copy fixture to a.pdf");
        std::fs::copy(fixture, &b).expect("copy fixture to b.pdf");
        (dir, a, b)
    }

    /// One frame of the reader over its record.
    fn sync_fixture(app: &mut PdfApp, st: &mut ui::State) {
        app.sync_settled(&mut st.doc, &st.root);
    }

    /// The reader launched on `path` on a fresh profile, as
    /// `pdf-viewer PATH` opens it.
    fn launched(path: &std::path::Path) -> ui::framework::testing::TestApp<crate::PdfViewerShell> {
        settled(
            ui::framework::testing::TestApp::launched(&[path.to_str().expect("utf-8 path")])
                .expect("the reader launches on the file"),
        )
    }

    /// Run `app`'s frames until the document it was launched on is shown and
    /// its record holds what opening it adds, as the window settles after a
    /// launch while the PDF worker answers.
    fn settled(
        mut app: ui::framework::testing::TestApp<crate::PdfViewerShell>,
    ) -> ui::framework::testing::TestApp<crate::PdfViewerShell> {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let shown = app
                .with_surface::<PdfApp, _>(|pdf| {
                    pdf.has_document && pdf.pending_open.is_none() && !pdf.seed_pending
                })
                .unwrap_or(false);
            if shown && app.state().imported_source().is_some() {
                return app;
            }
            assert!(
                ui::time::now() < deadline,
                "the launched document never opened"
            );
            std::thread::sleep(Duration::from_millis(5));
            app.settle();
        }
    }

    /// A caller with no screen reads the open PDF's text page by page, and
    /// a page past its end is refused.
    #[test]
    fn the_open_pdf_reads_as_its_pages_text() {
        let (_dir, path, _) = two_documents();
        let mut app = launched(&path);
        let read = app
            .effect(&PdfEffect::ReadText {
                from_page: None,
                to_page: Some(1),
            })
            .expect("the text reads")
            .value
            .expect("a read answers");
        assert_eq!(read["pages"][0]["page"], 1);
        assert!(
            !read["pages"][0]["text"].as_str().unwrap().trim().is_empty(),
            "{read}"
        );
        let count = read["page_count"].as_u64().unwrap();
        assert_eq!(
            read["next"],
            if count > 1 {
                serde_json::json!(2)
            } else {
                serde_json::Value::Null
            }
        );
        assert!(
            app.effect(&PdfEffect::ReadText {
                from_page: Some(usize::try_from(count).unwrap() + 1),
                to_page: None,
            })
            .is_err()
        );
    }

    /// A highlight on the first page of the open document, as the palette
    /// or an agent adds one.
    fn highlight_on_first_page() -> ui::tilo_resource::Invoke<crate::PdfMarkup> {
        crate::actions::add_annotation_on(
            &Annotation {
                id: "launched-highlight".into(),
                page: 1,
                kind: AnnoKind::highlight(PtRect::new(70.0, 150.0, 290.0, 15.0)),
                color: Swatch::Yellow,
                author: "Ada".into(),
                chars: None,
                created_ms: 0,
                space: crate::model::GeomSpace::Page,
            },
            "source-1",
        )
    }

    #[serial(pdfium)]
    #[test]
    fn each_document_keeps_its_markup_in_a_record_of_its_own() {
        // Opening a second document must not destroy or show the first
        // document's markup: each file is launched on its own record.
        eprintln!("phase launching_on_another_document_shows_none_of_the_first");
        {
            let (_dir, a, b) = two_documents();
            let mut app = launched(&a);
            app.invoke(highlight_on_first_page())
                .expect("the owner annotates");
            assert_eq!(app.state().annotations.len(), 1);
            let a_record = app.record().clone();
            assert_eq!(
                app.state().pinned_source().map(|source| source.name),
                Some("a.pdf".to_string()),
                "the record pins the document it marks up"
            );

            let other = settled(app.relaunch(&[b.to_str().unwrap()]).unwrap());
            assert_ne!(other.record(), &a_record, "b.pdf is a record of its own");
            assert!(
                other.state().annotations.is_empty(),
                "a.pdf's markup is not b.pdf's"
            );

            let again = settled(other.relaunch(&[a.to_str().unwrap()]).unwrap());
            assert_eq!(again.record(), &a_record);
            assert_eq!(again.state().annotations.len(), 1, "a.pdf keeps its markup");

            // Launched bare, the window reopens the record it showed last,
            // and that record's document from the content store.
            let mut bare = again.reopen().unwrap();
            assert_eq!(bare.record(), &a_record);
            assert!(
                bare.harness()
                    .wait_until(tilo_test_env::hang_bound(), |harness| {
                        harness.a11y_node_named("Page 1 of 3").is_some()
                    }),
                "the pinned document reopens"
            );
        }

        eprintln!("phase the_apps_open_shows_the_chosen_document_on_its_own_record");
        {
            let open = |path: &std::path::Path| PdfEffect::OpenPdf {
                path: Some(path.display().to_string()),
            };
            let (_dir, a, b) = two_documents();
            let mut app = launched(&a);
            app.invoke(highlight_on_first_page())
                .expect("the owner annotates");
            let a_record = app.record().clone();

            app.effect_confirmed(&open(&b))
                .expect("the app opens another document");
            app.harness().relayout();
            assert_ne!(
                app.record(),
                &a_record,
                "b.pdf shows on a record of its own"
            );
            assert!(app.state().annotations.is_empty(), "without a.pdf's markup");
            assert_eq!(
                app.state().pinned_source().map(|source| source.name),
                Some("b.pdf".to_string())
            );
            let b_record = app.record().clone();

            app.effect_confirmed(&open(&a)).expect("and back");
            app.harness().relayout();
            assert_eq!(app.record(), &a_record);
            assert_eq!(app.state().annotations.len(), 1, "a.pdf kept its markup");

            // A document that cannot be opened leaves the window as it is:
            // the person is told why, and the operation fails with it.
            let missing = a.with_file_name("gone.pdf");
            let failure = app
                .effect_confirmed(&open(&missing))
                .expect_err("a missing document fails the open");
            assert!(
                failure.to_string().contains("gone.pdf"),
                "the operation fails with the reason: {failure}"
            );
            app.harness().relayout();
            assert_eq!(app.record(), &a_record, "the window keeps its record");
            let note = app
                .with_surface::<PdfApp, _>(|pdf| pdf.status_note.clone())
                .flatten()
                .expect("the person is told");
            assert!(note.contains("gone.pdf"), "{note}");

            // A bare window opening a file marks it up on the file's own
            // record, the one a launch on the file opens.
            let mut bare = app.relaunch(&[]).expect("a bare window");
            assert!(
                bare.harness()
                    .wait_until(tilo_test_env::hang_bound(), |harness| {
                        harness.a11y_node_named("Page 1 of 3").is_some()
                    }),
                "the bare window finishes reopening its pinned document"
            );
            bare.effect_confirmed(&open(&b))
                .expect("the bare window opens b.pdf");
            bare.harness().relayout();
            assert_eq!(bare.record(), &b_record);
        }

        eprintln!("phase the_reader_on_its_own_opens_the_chosen_document_itself");
        {
            let (_dir, a, _b) = two_documents();
            let mut app = PdfApp::empty();
            let mut st = ui::State::new();
            app.open_chosen(&a);
            settle_record(&mut app, &mut st);
            assert!(app.has_document);
            assert_eq!(
                PdfMarkup::of(&st.doc)
                    .pinned_source()
                    .map(|source| source.hash),
                app.source_blob.as_ref().map(|(hash, _)| hash.clone()),
                "the window's record now marks up the chosen document"
            );
        }
    }

    #[serial(pdfium)]
    #[test]
    fn owner_worker_opens_settle_and_ignore_stale_results() {
        eprintln!("phase owner_worker_failure_settles_the_matching_open_without_blocking");
        {
            let dir = tempfile::tempdir().expect("temporary PDFs");
            let corrupt = dir.path().join("corrupt.pdf");
            std::fs::write(&corrupt, b"%PDF-1.7\nnot a readable document")
                .expect("write corrupt fixture");
            for path in [dir.path().join("missing.pdf"), corrupt] {
                let mut app = PdfApp::empty();
                let state = AutoCommit::new();
                app.queue_worker_open(&path);
                assert!(app.pending_open.is_some(), "accepted request is loading");
                let deadline = tilo_test_env::hang_deadline();
                while app.pending_open.is_some() && ui::time::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                    app.drain_raster_events(&state);
                }
                assert!(app.pending_open.is_none(), "worker failure settles loading");
                assert!(!app.has_document);
                assert!(app.load_error.as_deref().is_some_and(|error| {
                    error.contains("Could not open") && error.contains(&path.display().to_string())
                }));
                let tree = app.accessibility().expect("failed-open accessibility");
                assert!(any_access_node(&tree, &|node| node.name == app.load_error));
            }
        }

        eprintln!("phase owner_worker_stale_open_result_cannot_replace_the_latest_request");
        {
            let mut app = PdfApp::empty();
            let state = AutoCommit::new();
            let first = std::path::Path::new("/definitely/not/a/tilo-pdf-a.pdf");
            let latest = std::path::Path::new("/definitely/not/a/tilo-pdf-b.pdf");
            app.queue_worker_open(first);
            app.queue_worker_open(latest);
            let deadline = ui::time::now() + HANG_BOUND;
            while ui::time::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
                app.drain_raster_events(&state);
                if app.pending_open.is_none() {
                    break;
                }
            }
            assert!(app.pending_open.is_none());
            assert!(
                app.load_error
                    .as_deref()
                    .is_some_and(|error| error.contains("tilo-pdf-b.pdf"))
            );
        }
    }

    #[serial(pdfium)]
    #[test]
    fn markup_follows_the_file_through_rename_and_move_but_not_a_copy() {
        // Markup follows the file, not its path or its bytes: renaming and
        // moving the PDF reopens the same annotations; a copy starts clean.
        let dir = tempfile::tempdir().expect("temp dir");
        let original = dir.path().join("paper.pdf");
        let fixture = concat!(env!("OUT_DIR"), "/latin.pdf");
        std::fs::copy(fixture, &original).expect("copy the Typst fixture");

        let mut app = launched(&original);
        app.invoke(highlight_on_first_page())
            .expect("the owner annotates");
        let markup = app.record().clone();

        let moved_dir = dir.path().join("reviewed");
        std::fs::create_dir(&moved_dir).unwrap();
        let moved = moved_dir.join("paper (final).pdf");
        std::fs::rename(&original, &moved).unwrap();
        let reopened = settled(app.relaunch(&[moved.to_str().unwrap()]).unwrap());
        assert_eq!(reopened.record(), &markup);
        assert_eq!(
            reopened.state().annotations.len(),
            1,
            "the renamed file shows its annotations"
        );

        let copy = dir.path().join("paper copy.pdf");
        std::fs::copy(&moved, &copy).unwrap();
        let copied = settled(reopened.relaunch(&[copy.to_str().unwrap()]).unwrap());
        assert_ne!(copied.record(), &markup);
        assert!(
            copied.state().annotations.is_empty(),
            "a copy of the file starts with no markup"
        );
    }

    /// The resource `id` as the palette, an agent or a search hit reaches it:
    /// the record's published tree, a child it resolves beyond its listed
    /// page, or a live row the reader adds beside it.
    fn resolve(app: &PdfApp, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        fn find(resource: &tilo_actions::Resource, id: &str) -> Option<tilo_actions::Resource> {
            if resource.id.as_str() == id {
                return Some(resource.clone());
            }
            resource.children.iter().find_map(|child| find(child, id))
        }
        let state = <crate::PdfMarkup as ui::tilo_resource::Resource>::read(doc).ok()?;
        let published =
            ui::tilo_resource::publish::<crate::PdfMarkup>("pdf.document", "PDF", &state);
        find(&published, id)
            .or_else(|| <crate::PdfMarkup as ui::tilo_resource::Resource>::child(&state, id))
            .or_else(|| {
                app.document_row(id)
                    .map(ui::framework::LiveRow::into_resource)
            })
    }

    /// The empty reader staged as screenshot `scenario` shows it.
    fn staged_empty(scenario: &str) -> PdfApp {
        let mut app = PdfApp::empty();
        app.stage(Some(scenario));
        app
    }

    fn app_with_state() -> (PdfApp, ui::State) {
        let mut app = crate::scenarios::sample_reader();
        let mut st = crate::scenarios::sample_state();
        sync_fixture(&mut app, &mut st);
        // Give it a viewport so geometry is meaningful.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        (app, st)
    }

    /// Opening a document adds its page plan once; later frames leave the
    /// record alone.
    #[test]
    fn opening_seeds_the_record_once_not_every_frame() {
        let mut app = crate::scenarios::sample_reader();
        let mut st = ui::State::new();
        sync_fixture(&mut app, &mut st);
        assert!(
            PdfMarkup::of(&st.doc).page_plan_seeded,
            "opening adds the page plan"
        );
        let seeded = st.doc.get_heads();
        for _ in 0..3 {
            sync_fixture(&mut app, &mut st);
        }
        assert_eq!(st.doc.get_heads(), seeded, "frames write nothing");

        app.set_read_only(true);
        app.set_read_only(false);
        sync_fixture(&mut app, &mut st);
        assert_eq!(
            st.doc.get_heads(),
            seeded,
            "a record that has its plan gets nothing more when writing is allowed again"
        );
    }

    fn any_access_node(
        node: &ui::a11y::AccessNode,
        predicate: &impl Fn(&ui::a11y::AccessNode) -> bool,
    ) -> bool {
        predicate(node)
            || node
                .children
                .iter()
                .any(|child| any_access_node(child, predicate))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "this lifecycle test intentionally verifies canonical page, bookmark, markup, reply, and find-result addresses together"
    )]
    #[test]
    fn semantic_targets_and_global_search_address_stable_pages() {
        eprintln!("phase accessibility_exposes_reader_state_and_routes_toolbar_operations");
        {
            use ui::a11y::Action;
            use ui::widget::Widget as _;

            let (mut app, mut state) = app_with_state();
            let tree = app.accessibility().expect("reader semantics");
            assert_eq!(tree.role, ui::a11y::Role::Window);
            assert!(any_access_node(&tree, &|node| node
                .name
                .as_deref()
                .is_some_and(|value| value.contains("Page"))));
            assert!(any_access_node(&tree, &|node| node
                .action_target
                .as_deref()
                == Some("pdf.zoom-in")));

            let sidebar_before = app.show_sidebar;
            assert!(app.perform_semantic_action(
                "pdf.toggle-sidebar",
                &Action::Activate,
                &mut Edits::lend(&mut state.doc),
                &state.root,
            ));
            assert_ne!(app.show_sidebar, sidebar_before);
        }

        eprintln!("phase stable_page_semantic_target_follows_a_reordered_page");
        {
            use ui::a11y::Action;

            let (mut app, mut state) = app_with_state();
            let target_id = app.resource_page_ids[0].clone();
            let last = app.resource_page_ids[2].clone();
            app.perform_action(
                &mut Edits::lend(&mut state.doc),
                crate::actions::move_page_plan_action(&target_id, Some(&last)),
            );
            app.sync_settled(&mut state.doc, &state.root);
            assert_eq!(app.resource_page_ids[2], target_id);

            assert!(app.perform_semantic_action(
                &format!("pdf.page:{target_id}"),
                &Action::Activate,
                &mut Edits::lend(&mut state.doc),
                &state.root,
            ));
            assert_eq!(app.current_page, 3, "the addressed page, not ordinal 1");
            assert!(
                !app.perform_semantic_action(
                    "pdf.page:deleted-page",
                    &Action::Activate,
                    &mut Edits::lend(&mut state.doc),
                    &state.root,
                ),
                "unknown targets fail closed"
            );
        }

        eprintln!("phase global_search_pages_canonically_and_stale_page_hits_fail_closed");
        {
            let (mut app, mut state) = app_with_state();
            let provider = searched(&app);
            assert!(
                provider
                    .search_page(&ui::search::SearchQuery::new(""), None, 0, 1)
                    .results
                    .is_empty()
            );
            let first = provider.search_page(&ui::search::SearchQuery::new(""), None, 1, 1);
            assert_eq!(first.inspected, 1);
            let stale_cursor = first.next.expect("catalogue paging stays bounded");
            let item = |field: &str, id: &str| {
                ui::tilo_resource::items::item_resource_id::<PdfMarkup>(
                    field,
                    &ui::tilo_resource::Id::from_raw(id),
                )
            };
            let page_prefix = item("pages", "");
            let hit = provider
                .search("overview")
                .into_iter()
                .map(|result| result.anchor())
                .find(|anchor| anchor.starts_with(&page_prefix))
                .expect("page text is globally searchable at its page");
            assert!(resolve(&app, &state.doc, &hit).is_some());

            // The markup people made is searched through the resource, each
            // entry opening the published child it names.
            let markup = <crate::PdfMarkup as ui::tilo_resource::Resource>::read(&state.doc)
                .expect("the record reads");
            let entries = <crate::PdfMarkup as ui::tilo_resource::Resource>::search(&markup);
            let markup_hit = |needle: &str| {
                entries
                    .iter()
                    .find(|entry| entry.title.contains(needle) || entry.text.contains(needle))
                    .and_then(|entry| entry.anchor.clone())
                    .unwrap_or_else(|| panic!("{needle:?} should produce a search hit at its item"))
            };
            for (needle, delete) in [
                ("highlight", "delete-annotation"),
                ("Iroh relay", "delete-comment"),
            ] {
                let anchor = markup_hit(needle);
                let resource = resolve(&app, &state.doc, &anchor)
                    .expect("search door resolves the anchored annotation/comment item");
                assert!(resource.actions.iter().any(|action| action.id == delete));
            }
            let bookmark_address = provider
                .search("security model")
                .into_iter()
                .filter_map(|result| result.action_address())
                .find(|address| address.resource.starts_with("pdf.outline:"))
                .expect("PDF bookmarks are globally searchable by their canonical action");
            let bookmark_resource = resolve(&app, &state.doc, &bookmark_address.resource)
                .expect("bookmark search result resolves to the current outline resource");
            assert!(
                bookmark_resource
                    .actions
                    .iter()
                    .any(|action| action.id == bookmark_address.action)
            );
            let reply_anchor = markup_hit("folded it");
            assert!(
                reply_anchor.starts_with(&item("comments", "")),
                "a reply is found in its thread"
            );
            assert!(
                resolve(&app, &state.doc, &reply_anchor).is_some(),
                "a reply search hit resolves to the durable thread"
            );

            app.open_search();
            app.find_bar.set_query("sandbox");
            app.recompute_matches();
            app.sync_settled(&mut state.doc, &state.root);
            let find_provider = searched(&app);
            let find_address = find_provider
                .search("match 1")
                .into_iter()
                .filter_map(|result| result.action_address())
                .find(|address| address.resource.starts_with("pdf.search-result:"))
                .expect("bounded find result has a canonical global-search action");
            let find_resource = resolve(&app, &state.doc, &find_address.resource)
                .expect("current find-result address resolves");
            assert!(
                find_resource
                    .actions
                    .iter()
                    .any(|action| action.id == "go-to-match")
            );
            app.find_bar.set_query("filesystem");
            app.recompute_matches();
            assert!(
                resolve(&app, &state.doc, &find_address.resource).is_none(),
                "a stale find ordinal cannot be rebound to the next query"
            );
            // The changed find projection refreshes once; its settled successor
            // must remain retained without re-walking the corpus every frame.
            app.sync_settled(&mut state.doc, &state.root);
            let refreshes = app.search_projection_refreshes;
            app.sync_settled(&mut state.doc, &state.root);
            assert_eq!(
                app.search_projection_refreshes, refreshes,
                "unchanged sync avoids corpus traversal"
            );

            let stale_resource = item("pages", "source-1");
            let action = crate::actions::delete_page_plan_action("source-1");
            app.perform_action(&mut Edits::lend(&mut state.doc), action);
            app.sync_settled(&mut state.doc, &state.root);
            let restarted = searched(&app).search_page(
                &ui::search::SearchQuery::new(""),
                Some(&stale_cursor),
                1,
                1,
            );
            assert!(
                restarted.results.len() <= 1,
                "a stale cursor restarts against the new generation"
            );
            assert!(resolve(&app, &state.doc, &stale_resource).is_none());
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "this integration test intentionally keeps replay, convergence, resource, and undo assertions in one lifecycle"
    )]
    #[test]
    fn shared_page_plan_keeps_markup_and_addresses_through_edits() {
        eprintln!("phase shared_page_plan_replays_merges_undoes_and_keeps_addresses_stable");
        {
            use crate::actions::{delete_page_plan_action, move_page_plan_action};
            use ui::tilo_resource::{Id, Invoke, Replica};

            // One peer authors an action, the other replays its envelope.
            let author_and_replay =
                |a: &mut AutoCommit, b: &mut AutoCommit, invoke: Invoke<PdfMarkup>| {
                    let mut author = Replica::<PdfMarkup>::from_doc(std::mem::take(a));
                    let mut peer = Replica::<PdfMarkup>::from_doc(std::mem::take(b));
                    let envelope = author.apply(invoke).expect("author").clone();
                    peer.replay(&envelope).expect("replay");
                    *a = std::mem::take(author.doc_mut());
                    *b = std::mem::take(peer.doc_mut());
                };
            let duplicate = |id: &str, copy: &str| {
                PdfMarkup::DUPLICATE_PAGE.with((Id::from_raw(id), Some(copy.to_owned())))
            };

            let (mut a, mut state_a) = app_with_state();
            state_a.doc.commit();
            let base = state_a.doc.save();
            let mut state_b = ui::State::new();
            state_b.doc = AutoCommit::load(&base).expect("clone shared base");
            let (mut b, _) = app_with_state();

            // One peer authors an ordered duplicate/reorder sequence. The envelope
            // carries the generated occurrence id, so the other replica need not
            // (and must not) mint one while replaying it.
            author_and_replay(
                &mut state_a.doc,
                &mut state_b.doc,
                duplicate("source-2", "copy-of-source-2"),
            );
            author_and_replay(
                &mut state_a.doc,
                &mut state_b.doc,
                move_page_plan_action("source-1", Some("copy-of-source-2")),
            );
            assert_eq!(
                PdfMarkup::of(&state_a.doc).page_plan(),
                PdfMarkup::of(&state_b.doc).page_plan(),
                "the exact same action envelopes reproduce durable ids and order"
            );

            // Materialising the same shared plan gives both renderers the same
            // address space, independent of their local source/raster instances.
            a.sync_settled(&mut state_a.doc, &state_a.root);
            b.sync_settled(&mut state_b.doc, &state_b.root);
            assert_eq!(a.resource_page_ids, b.resource_page_ids);
            let source_two_hits: Vec<String> = searched(&a)
                .search("Per-cluster sandboxing")
                .into_iter()
                .map(|result| result.anchor())
                .collect();
            assert!(
                source_two_hits
                    .iter()
                    .any(|anchor| anchor.ends_with("/source-2"))
                    && source_two_hits
                        .iter()
                        .any(|anchor| anchor.ends_with("/copy-of-source-2")),
                "duplicate occurrences retain the immutable source page's searchable text"
            );
            let page_id = "source-1";
            let resource_id = ui::tilo_resource::items::item_resource_id::<PdfMarkup>(
                "pages",
                &Id::from_raw(page_id),
            );
            assert!(
                resolve(&a, &state_a.doc, &resource_id).is_some(),
                "the durable page id resolves through the provider"
            );
            assert_eq!(
                a.resource_page_ids
                    .iter()
                    .position(|id| id == page_id)
                    .map(|index| index + 1),
                Some(3),
                "the stable page id moved while retaining its address"
            );

            // A stale delete cannot target a newly-created or reordered occurrence:
            // after it wins, lazy resolution fails closed instead of falling back to
            // an ordinal page address.
            author_and_replay(
                &mut state_a.doc,
                &mut state_b.doc,
                delete_page_plan_action(page_id),
            );
            a.sync_settled(&mut state_a.doc, &state_a.root);
            b.sync_settled(&mut state_b.doc, &state_b.root);
            assert!(resolve(&a, &state_a.doc, &resource_id).is_none());
            assert!(resolve(&b, &state_b.doc, &resource_id).is_none());

            // Concurrent page-plan actions merge like the other markup actions.
            let mut left = AutoCommit::load(&base).expect("left replica");
            let mut right = AutoCommit::load(&base).expect("right replica");
            PdfMarkup::apply_to(&mut left, duplicate("source-1", "copy-left"));
            PdfMarkup::apply_to(&mut right, duplicate("source-3", "copy-right"));
            left.merge_incoming(&mut right)
                .expect("merge concurrent page edits");
            let merged_ids: Vec<String> = PdfMarkup::of(&left)
                .page_plan()
                .into_iter()
                .map(|entry| entry.id)
                .collect();
            assert!(merged_ids.contains(&"copy-left".to_string()));
            assert!(merged_ids.contains(&"copy-right".to_string()));

            // A page-plan edit is one undo step of its author's: undo rolls
            // back the action rather than silently mutating a local raster plan.
            let mut undo_app = ui::framework::testing::TestApp::<crate::PdfViewerShell>::new()
                .expect("the reader");
            undo_app
                .invoke(PdfMarkup::START_PAGE_PLAN.with(3))
                .expect("the plan");
            let before = undo_app.state().page_plan();
            undo_app
                .invoke(duplicate("source-1", "copy-undo"))
                .expect("the owner duplicates a page");
            assert_eq!(undo_app.state().page_plan().len(), before.len() + 1);
            assert!(undo_app.undo(), "shared page action is an undo step");
            assert_eq!(undo_app.state().page_plan(), before);
        }

        eprintln!("phase imported_occurrence_keeps_markup_through_move_duplicate_and_delete");
        {
            use crate::actions::{add_annotation_on, move_page_plan_action};
            use crate::markup::{ImportedPage, PagePlanEntry};

            let (mut app, mut state) = app_with_state();
            let imported = ImportedPage {
                hash: "c".repeat(64),
                kind: "image".into(),
                page: 1,
                width_milli: 612_000,
                height_milli: 408_000,
            };
            let page = PagePlanEntry {
                id: "imported-occurrence".into(),
                source_page: 1,
                imported: Some(imported.clone()),
                rotation: 0,
            };
            app.perform_action(
                &mut Edits::lend(&mut state.doc),
                crate::actions::import_pages_action(Some("source-1"), vec![page.clone()]),
            );
            app.sync_settled(&mut state.doc, &state.root);
            assert_eq!(app.resource_page_ids[1], "imported-occurrence");
            assert!(
                app.status_note
                    .as_deref()
                    .is_some_and(|note| note.contains("unavailable"))
            );

            let annotation = Annotation {
                id: "annotation-on-import".into(),
                page: 2,
                kind: AnnoKind::Note { x: 12.0, y: 24.0 },
                color: Swatch::Yellow,
                author: "You".into(),
                chars: None,
                created_ms: 0,
                space: crate::model::GeomSpace::Page,
            };
            app.perform_action(
                &mut Edits::lend(&mut state.doc),
                add_annotation_on(&annotation, "imported-occurrence"),
            );
            app.perform_action(
                &mut Edits::lend(&mut state.doc),
                move_page_plan_action("imported-occurrence", Some("source-3")),
            );
            app.sync_settled(&mut state.doc, &state.root);
            assert_eq!(
                app.annotations
                    .iter()
                    .find(|annotation| annotation.id == "annotation-on-import")
                    .map(|annotation| annotation.page),
                Some(4),
                "anchor follows the imported occurrence"
            );

            app.perform_action(
                &mut Edits::lend(&mut state.doc),
                PdfMarkup::DUPLICATE_PAGE.with((
                    ui::tilo_resource::Id::from_raw(page.id),
                    Some("imported-copy".to_owned()),
                )),
            );
            assert_eq!(
                PdfMarkup::of(&state.doc)
                    .page_plan()
                    .into_iter()
                    .find(|entry| entry.id == "imported-copy")
                    .and_then(|entry| entry.imported),
                Some(imported),
                "duplicate retains content hash and source page"
            );
            app.perform_action(
                &mut Edits::lend(&mut state.doc),
                crate::actions::delete_page_plan_action("imported-occurrence"),
            );
            app.sync_settled(&mut state.doc, &state.root);
            assert!(
                app.annotations
                    .iter()
                    .all(|annotation| annotation.id != "annotation-on-import"),
                "deleting one occurrence removes only its markup"
            );
            assert!(app.resource_page_ids.iter().any(|id| id == "imported-copy"));
        }
    }

    #[serial(pdfium)]
    #[test]
    fn worker_reader_says_when_imported_page_bytes_are_missing() {
        let Some(mut app) = open_raster_sample() else {
            return;
        };
        let mut state = ui::State::new();
        settle_record(&mut app, &mut state);
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            crate::actions::import_pages_action(
                Some("source-1"),
                vec![crate::markup::PagePlanEntry {
                    id: "missing-import".into(),
                    source_page: 1,
                    imported: Some(crate::markup::ImportedPage {
                        hash: "d".repeat(64),
                        kind: "image".into(),
                        page: 1,
                        width_milli: 612_000,
                        height_milli: 408_000,
                    }),
                    rotation: 0,
                }],
            ),
        );
        app.sync_settled(&mut state.doc, &state.root);
        assert!(
            app.status_note
                .as_deref()
                .is_some_and(|note| note.starts_with("Imported page content is")),
            "{:?}",
            app.status_note
        );
        assert!(
            app.pending_worker_plan.is_none(),
            "a plan whose bytes are missing is not sent to the owner"
        );
    }

    #[serial(pdfium)]
    #[test]
    fn shot_drivers_render_every_review_and_product_state() {
        // Draw the truthful page-shaped state a collaborator without a PDF
        // renderer sees for an imported page. Kept as a screenshot driver as
        // well as an assertion so release review can catch a misleading
        // fallback render.
        eprintln!("phase drive_import_unavailable_shot");
        {
            let mut app = crate::scenarios::sample_reader();
            let mut state = ui::State::new();
            app.sync_settled(&mut state.doc, &state.root);
            app.perform_action(
                &mut Edits::lend(&mut state.doc),
                crate::actions::import_pages_action(
                    Some("source-1"),
                    vec![crate::markup::PagePlanEntry {
                        id: "review-unavailable-import".into(),
                        source_page: 1,
                        imported: Some(crate::markup::ImportedPage {
                            hash: "d".repeat(64),
                            kind: "image".into(),
                            page: 1,
                            width_milli: 612_000,
                            height_milli: 408_000,
                        }),
                        rotation: 0,
                    }],
                ),
            );
            let app = framed(
                app,
                state,
                1280.0,
                800.0,
                Some("/tmp/pdf_viewer_release_shots/import_unavailable.png"),
            );
            assert!(
                app.status_note
                    .as_deref()
                    .is_some_and(|note| note.contains("unavailable")),
                "a missing imported blob is never rendered as a source page"
            );
        }

        // Drive-review on the real bundled `sample.pdf`: exact 50 / 100 / 200 %
        // zoom, a panned view, and a placed annotation — each run through the live
        // frame path and shot to `/tmp/pdf_repro`. Checks that zoom changes
        // the on-screen page size and that horizontal pan reaches new content.
        eprintln!("phase drive_review_zoom_pan_annotate_shots");
        'phase: {
            let Some(app) = open_raster_sample() else {
                break 'phase;
            };

            // --- exact 50 / 100 / 200 % zoom, each a live frame + shot. ---
            let mut app = framed_at_zoom(
                app,
                ui::State::new(),
                0.5,
                Some("/tmp/pdf_repro/zoom_50.png"),
            );
            let w50 = app.pw() * app.zoom();
            let w100 = set_zoom_and_frame(&mut app, 1.0, Some("/tmp/pdf_repro/zoom_100.png"));
            let w200 = set_zoom_and_frame(&mut app, 2.0, Some("/tmp/pdf_repro/zoom_200.png"));
            eprintln!("DRIVE page-card width px: 50%={w50:.1}  100%={w100:.1}  200%={w200:.1}");
            assert!(
                (w100 - 2.0 * w50).abs() < 1.0,
                "100% page must be twice the 50% page"
            );
            assert!(
                (w200 - 2.0 * w100).abs() < 1.0,
                "200% page must be twice the 100% page"
            );
            // A deliberate page-top view makes the high-zoom crop reviewable:
            // zoom retains the reader's focal scroll position, while page
            // navigation remains an immediate, keyboard-equivalent way to return
            // to the start of readable content.
            app.go_to_page(1);
            let scroll_y = app.scroll_y();
            app.viewport.set_scroll(0.0, scroll_y);
            full_frame(
                &mut app,
                1280.0,
                800.0,
                Some("/tmp/pdf_repro/zoom_200_page_top.png"),
            );
            assert_eq!(app.nearest_page(), 1, "page-top zoom view remains usable");
            assert!(
                app.scroll_x().abs() < f64::EPSILON,
                "review frame starts at the leading column"
            );

            // --- Ctrl+wheel zoom changes scale (the wheel handler's reducer). ---
            let before = app.zoom();
            let c = app.canvas_rect.center();
            app.zoom_at(1.0 + 56.0 / 240.0, Point::new(c.x, c.y));
            assert!(
                app.zoom() > before + 1e-3,
                "ctrl+wheel-equivalent zoom must change scale"
            );

            // --- horizontal pan at 200%: the page overflows, so panning right must
            //     move the content and reach new offsets. ---
            app.fit = FitMode::Custom(2.0);
            app.viewport.set_zoom(2.0);
            full_frame(&mut app, 1280.0, 800.0, None);
            assert!(
                app.is_overflowing_x(),
                "200% must overflow the viewport horizontally"
            );
            let sx0 = app.scroll_x();
            app.viewport.pan_by(240.0, 0.0); // arrow/drag-equivalent rightward pan
            let sx1 = app.scroll_x();
            assert!(
                sx1 > sx0 + 1.0,
                "panning right must increase scroll_x ({sx0} -> {sx1})"
            );
            full_frame(
                &mut app,
                1280.0,
                800.0,
                Some("/tmp/pdf_repro/panned_200.png"),
            );
            eprintln!(
                "DRIVE pan: scroll_x {sx0:.1} -> {sx1:.1} (max {:.1})",
                app.viewport.max_scroll_x()
            );

            // --- place an annotation on the document and shoot it. ---
            app.fit = FitMode::Page;
            let fit_page = app.fit_page_zoom();
            app.viewport.set_zoom(fit_page);
            full_frame(&mut app, 1280.0, 800.0, None);
            let a0 = app.annotations.len();
            app.tool = Tool::Highlight;
            // Drag across a text line on page 1 (page-point coords).
            app.drag = Some(DragState {
                tool: Tool::Highlight,
                page: 1,
                start: (70.0, 150.0),
                cur: (360.0, 165.0),
                points: vec![],
            });
            let (reader, st) = Framed::split(&mut app);
            reader.commit_drag(&mut Edits::lend(&mut st.doc));
            assert!(
                app.annotations.len() > a0,
                "a highlight drag adds an annotation"
            );
            app.go_to_page(1); // bring the freshly-placed highlight into view
            eprintln!(
                "DRIVE annotate scroll_y after go_to_page(1) = {:.1}",
                app.scroll_y()
            );
            full_frame(
                &mut app,
                1280.0,
                800.0,
                Some("/tmp/pdf_repro/annotated.png"),
            );
            eprintln!(
                "DRIVE annotate: {} -> {} annotations; scroll_y after frame = {:.1}",
                a0,
                app.annotations.len(),
                app.scroll_y()
            );
        }

        // Drive-review the review features on the seeded sample document:
        // annotations render anchored to real text (no free-floating ellipse over
        // whitespace), search highlights matches, and the Contents outline draws.
        // Shots to `/tmp/pdf_repro`.
        eprintln!("phase drive_review_features_shots");
        {
            let app = crate::scenarios::sample_reader();
            let st = crate::scenarios::sample_state();

            // Default sample view — seeded highlights + note + signature on page 1.
            let mut app = framed(
                app,
                st,
                1280.0,
                800.0,
                Some("/tmp/pdf_repro/sample_markup.png"),
            );
            // The seeded markup must contain NO free-form ink stroke (the removed
            // "emphasis loop" that circled empty whitespace).
            assert!(
                !app.annotations
                    .iter()
                    .any(|a| matches!(a.kind, AnnoKind::Ink { .. })),
                "no seeded ink 'emphasis loop' should remain"
            );
            assert!(
                app.annotations
                    .iter()
                    .any(|a| matches!(a.kind, AnnoKind::Markup { .. })),
                "seeded highlights remain"
            );

            // Search: whole-word + case toggles narrow results.
            app.open_search();
            app.find_bar.set_query("sandbox");
            app.recompute_matches();
            let loose = app.matches.len();
            assert!(loose > 0, "search finds 'sandbox' in the sample");
            full_frame(
                &mut app,
                1280.0,
                800.0,
                Some("/tmp/pdf_repro/search_results.png"),
            );
            app.search_whole_word = true;
            app.recompute_matches();
            assert!(
                app.matches.len() <= loose,
                "whole-word never yields more matches"
            );
            app.close_search();

            // Contents (outline) tab renders the document's section list.
            app.sidebar_tab = SidebarTab::Outline;
            assert!(!app.doc.outline.is_empty(), "sample has an outline");
            full_frame(&mut app, 1280.0, 800.0, Some("/tmp/pdf_repro/outline.png"));
            eprintln!(
                "DRIVE features: {} annotations, {} search hits, {} outline items",
                app.annotations.len(),
                loose,
                app.doc.outline.len()
            );
        }

        // A compact visual state matrix for release review.  It deliberately
        // covers the simple open/error flow and the power-user reader flow at a
        // phone-width viewport, then repeats the latter under high contrast and
        // reduced motion.  Set `PDF_SHOTS=1` to retain the PNGs.
        eprintln!("phase drive_product_state_matrix_shots");
        {
            let shot_dir = std::env::var("PDF_SHOT_DIR")
                .unwrap_or_else(|_| "/tmp/tilo_pdf_product_states".to_string());
            let shot = |name: &str| format!("{shot_dir}/{name}");

            let narrow = crate::scenarios::sample_reader();
            let narrow_state = crate::scenarios::sample_state();
            let mut narrow = framed(
                narrow,
                narrow_state,
                390.0,
                844.0,
                Some(&shot("narrow-document.png")),
            );
            assert!(narrow.overlay_mode, "compact viewport uses overlay panels");

            narrow.open_search();
            narrow.find_bar.set_query("sandbox");
            narrow.recompute_matches();
            full_frame(&mut narrow, 390.0, 844.0, Some(&shot("narrow-search.png")));
            assert!(narrow.match_count() > 0, "find remains usable when compact");

            let loading = staged_empty("loading");
            let loading_state = ui::State::new();
            let loading = framed(
                loading,
                loading_state,
                1280.0,
                800.0,
                Some(&shot("loading.png")),
            );
            let loading_a11y = loading.accessibility().expect("loading semantics");
            assert!(
                !any_access_node(&loading_a11y, &|node| {
                    node.action_target.as_deref() == Some("pdf.open")
                }),
                "a pending open is progress, not a duplicate Open action"
            );
            let error = staged_empty("error");
            let error_state = ui::State::new();
            let error = framed(error, error_state, 1280.0, 800.0, Some(&shot("error.png")));
            assert!(
                error.canvas_rect.height() > 700.0,
                "the persistent error keeps the full empty-state canvas"
            );
            let narrow_error = staged_empty("error");
            let narrow_error_state = ui::State::new();
            framed(
                narrow_error,
                narrow_error_state,
                390.0,
                844.0,
                Some(&shot("narrow-error.png")),
            );

            let contrast = crate::scenarios::sample_reader();
            let contrast_state = crate::scenarios::sample_state();
            framed_with_theme(
                contrast,
                contrast_state,
                1280.0,
                800.0,
                Some(&shot("high-contrast.png")),
                ui::theme::Theme::high_contrast(),
            );

            let mut reduced = crate::scenarios::sample_reader();
            let reduced_state = crate::scenarios::sample_state();
            reduced.arm_shots_page_bubble();
            reduced.set_motion_reduced(true);
            let reduced = framed(
                reduced,
                reduced_state,
                390.0,
                844.0,
                Some(&shot("reduced-motion.png")),
            );
            assert!(
                !reduced.needs_animation_frame(),
                "reduced motion settles the page bubble without a timer loop"
            );
        }

        // Drive the real fixture PDF through the live frame path to produce the
        // review shots: a search hit over genuine glyphs, a placed highlight +
        // signature, and the *reopened saved* file rendered by pdfium with the
        // flattened markup baked in. PNGs are written only under `PDF_SHOTS=1`
        // (to `$PDF_SHOT_DIR`, default a temp dir); the assertions always run.
        eprintln!("phase drive_real_pdf_search_highlight_save_shots");
        'phase: {
            let shot_dir = std::env::var("PDF_SHOT_DIR").unwrap_or_else(|_| {
                std::env::temp_dir()
                    .join("tilo_pdf_review")
                    .to_string_lossy()
                    .into_owned()
            });
            let shot = |name: &str| format!("{shot_dir}/{name}");

            let Some(app) = open_raster_sample() else {
                break 'phase;
            };
            let mut app = framed(app, ui::State::new(), 1280.0, 800.0, None);

            // --- search a word and shoot the hit over glyphs. ---
            let word = {
                let (reader, st) = Framed::split(&mut app);
                let layer = text_layer(reader, st, 1);
                first_word(&layer.text, 4).expect("a searchable word")
            };
            app.open_search();
            app.find_bar.set_query(&word);
            app.recompute_matches();
            {
                let (reader, st) = Framed::split(&mut app);
                settle_until(reader, st, |app| app.raster_text_pending.is_empty());
            }
            assert!(
                !app.raster_matches.is_empty(),
                "search finds {word:?} on the real PDF"
            );
            let p_search = shot("real_search_hit.png");
            full_frame(&mut app, 1280.0, 800.0, Some(p_search.as_str()));
            app.close_search();

            // --- place a highlight + a signature and shoot the annotated page. ---
            app.tool = Tool::Highlight;
            app.drag = Some(DragState {
                tool: Tool::Highlight,
                page: 1,
                start: (70.0, 150.0),
                cur: (360.0, 165.0),
                points: vec![],
            });
            let (reader, st) = Framed::split(&mut app);
            reader.commit_drag(&mut Edits::lend(&mut st.doc));
            reader.signer_name = "Grace Hopper".into();
            reader.place_signature(&mut Edits::lend(&mut st.doc), 1, (360.0, 520.0));
            app.go_to_page(1);
            let p_anno = shot("real_annotated.png");
            full_frame(&mut app, 1280.0, 800.0, Some(p_anno.as_str()));

            // --- save, reopen the saved file, and shoot pdfium's own render of the
            //     flattened markup — proof the annotations are in the .pdf. ---
            let out = std::env::temp_dir()
                .join("tilo_pdf_review")
                .join("annotated.pdf");
            std::fs::create_dir_all(out.parent().unwrap()).unwrap();
            let (reader, _) = Framed::split(&mut app);
            assert!(
                save_settled(reader, &out),
                "save succeeds: {:?}",
                reader.status_note
            );

            let app2 = open_settled(&out).expect("the saved file reopens via pdfium");
            let p_reopen = shot("real_reopened_saved.png");
            framed(
                app2,
                ui::State::new(),
                1280.0,
                800.0,
                Some(p_reopen.as_str()),
            );
            let signed = crate::source::open(&out, &ContentStore::memory())
                .expect("the saved file reopens")
                .raster
                .page_annotations(0)
                .iter()
                .any(|a| {
                    a.kind == tilo_pdf::AnnotationKind::Stamp && a.contents.contains("Grace Hopper")
                });
            assert!(signed, "reopened saved file carries the signature stamp");
            eprintln!(
                "DRIVE real-pdf: word={word:?}  hits={}  saved={}  reopened-has-signature={signed}",
                app.raster_matches.len(),
                out.display(),
            );
        }
    }

    /// Mount `app` over `st` in the production frame host at `w`×`h` and
    /// run its first frame (see [`full_frame`]).
    fn framed(app: PdfApp, st: ui::State, w: f64, h: f64, out: Option<&str>) -> Framed<PdfApp> {
        framed_with_theme(app, st, w, h, out, Theme::dark())
    }

    /// Variant used by the visual-state matrix to exercise real layout and
    /// paint under high-contrast text/tokens as well as the default theme.
    fn framed_with_theme(
        app: PdfApp,
        st: ui::State,
        w: f64,
        h: f64,
        out: Option<&str>,
        theme: Theme,
    ) -> Framed<PdfApp> {
        let mut app = Framed::new(st, app, viewport((w, h)), theme);
        settle_and_shoot(&mut app, viewport((w, h)), out);
        app
    }

    /// Set the zoom to an exact factor via the viewport kernel + fit intent
    /// before [`framed`]'s first frame at 1280×800.
    fn framed_at_zoom(mut app: PdfApp, st: ui::State, z: f64, out: Option<&str>) -> Framed<PdfApp> {
        app.fit = FitMode::Custom(z);
        app.viewport.set_zoom(z);
        framed(app, st, 1280.0, 800.0, out)
    }

    /// Run one full `ui::App`-equivalent frame against `app` at `w`×`h`
    /// through the production frame host (update → sync → layout → draw), so
    /// a driven test exercises the *live* path, not a shortcut.
    ///
    /// A PNG is written to `out` **only when `PDF_SHOTS=1`** in the
    /// environment. The default `cargo test` run therefore never touches the
    /// GPU (the offscreen `wgpu` renderer serialises on a process-wide context
    /// and is slow/contended under the parallel suite) — the behavioural
    /// assertions still run every time. Regenerate the shots deliberately with
    /// `PDF_SHOTS=1 … cargo test drive_review -- --test-threads=1`.
    fn full_frame(app: &mut Framed<PdfApp>, w: f64, h: f64, out: Option<&str>) {
        Framed::harness_mut(app)
            .relayout_at(viewport((w, h)))
            .expect("pdf frame lays out");
        settle_and_shoot(app, viewport((w, h)), out);
    }

    /// Frame until the worker has delivered every visible page, so a single
    /// still frame is the *settled* image (the live app is woken by the
    /// worker's results; a one-shot capture must wait for them itself or the
    /// rail/second page would be missing), then write the PNG `out` names
    /// when `PDF_SHOTS=1`.
    fn settle_and_shoot(app: &mut Framed<PdfApp>, (w, h): (u32, u32), out: Option<&str>) {
        // What the frame submitted lands, as a host applies it.
        {
            let (reader, st) = Framed::split(app);
            reader.sync_settled(&mut st.doc, &st.root);
        }
        let deadline = tilo_test_env::hang_deadline();
        while app.pending_visuals && ui::time::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            Framed::harness_mut(app).relayout();
        }
        let want_png = std::env::var("PDF_SHOTS").as_deref() == Ok("1");
        if let (Some(path), true) = (out, want_png) {
            if let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent).ok();
            }
            ui::offscreen::OffscreenRenderer::new(w, h).render_png(
                Framed::harness(app).scene(),
                vello::peniko::Color::from_rgb8(10, 12, 16),
                std::path::Path::new(path),
            );
        }
    }

    /// Set the zoom to an exact factor via the viewport kernel + fit intent,
    /// then run a live frame and (optionally) shoot it. Returns the page-card
    /// width on screen so a test can assert the scale really changed.
    fn set_zoom_and_frame(app: &mut Framed<PdfApp>, z: f64, out: Option<&str>) -> f64 {
        app.fit = FitMode::Custom(z);
        app.viewport.set_zoom(z);
        full_frame(app, 1280.0, 800.0, out);
        app.pw() * app.zoom()
    }

    #[test]
    fn review_markup_reads_back_once() {
        eprintln!("phase the_review_reads_back");
        {
            let (app, _st) = app_with_state();
            assert!(!app.annotations.is_empty(), "seed should add annotations");
            assert!(!app.comments.is_empty(), "seed should add comments");
            assert!(!app.signatures.is_empty(), "seed should add a signature");
            // The note annotation has a linked comment thread.
            let note = app
                .annotations
                .iter()
                .find(|a| matches!(a.kind, AnnoKind::Note { .. }))
                .expect("a sticky note");
            assert!(
                app.comments
                    .iter()
                    .any(|c| c.anno_id.as_deref() == Some(note.id.as_str()))
            );
        }

        eprintln!("phase second_sync_does_not_duplicate");
        {
            let (mut app, mut st) = app_with_state();
            let n = app.annotations.len();
            sync_fixture(&mut app, &mut st);
            assert_eq!(
                app.annotations.len(),
                n,
                "re-sync must not duplicate markup"
            );
        }
    }

    /// A synthetic single-line text layer over page 1 + a settled selection,
    /// so the shared-HUD tests need no native pdfium.
    fn seed_selection(app: &mut PdfApp, chars: &str, upto: usize) {
        let glyphs: Vec<(char, PtRect)> = chars
            .chars()
            .enumerate()
            .map(|(i, c)| (c, PtRect::new(72.0 + i as f64 * 9.0, 120.0, 8.0, 12.0)))
            .collect();
        app.raster_text
            .insert(1, std::sync::Arc::new(PageText::from_glyphs(glyphs)));
        app.selection = Some(TextSelection {
            page: 1,
            anchor: 0,
            caret: upto,
            dragging: false,
        });
    }

    #[test]
    fn selection_hud_actions_highlight_note_and_copy() {
        eprintln!("phase hud_highlight_action_highlights_the_text_selection");
        {
            let (mut app, mut st) = app_with_state();
            seed_selection(&mut app, "Hello", 4);
            let hl = |st: &ui::State| {
                PdfMarkup::of(&st.doc)
                    .list_annotations()
                    .into_iter()
                    .filter(|a| matches!(a.kind, AnnoKind::Markup { .. }))
                    .count()
            };
            let before = hl(&st);
            // The shared HUD's swatch action highlights the run in that colour.
            app.apply_hud_action(ui::HudAction::Highlight(1), &mut Edits::lend(&mut st.doc));
            assert!(
                hl(&st) > before,
                "highlighting the selection adds a highlight annotation"
            );
            assert!(
                app.selection.is_none(),
                "the selection is consumed by the highlight"
            );
            assert_eq!(
                app.swatch,
                Swatch::ALL[1],
                "the armed swatch follows the HUD choice"
            );
        }

        eprintln!("phase hud_note_action_attaches_a_note_to_the_selection");
        {
            let (mut app, mut st) = app_with_state();
            seed_selection(&mut app, "AB", 1);
            let notes = |st: &ui::State| {
                PdfMarkup::of(&st.doc)
                    .list_annotations()
                    .into_iter()
                    .filter(|a| matches!(a.kind, AnnoKind::Note { .. }))
                    .count()
            };
            let before = notes(&st);
            app.apply_hud_action(ui::HudAction::Note, &mut Edits::lend(&mut st.doc));
            assert_eq!(
                notes(&st),
                before + 1,
                "a note is attached to the selection"
            );
            assert!(
                app.editing.is_some(),
                "the note's comment editor opens for typing"
            );
        }

        eprintln!("phase hud_copy_action_reports_success_on_a_selection");
        {
            let (mut app, mut st) = app_with_state();
            seed_selection(&mut app, "Copyme", 5);
            assert!(
                app.selection_text().is_some(),
                "the synthetic run yields text"
            );
            app.apply_hud_action(ui::HudAction::Copy, &mut Edits::lend(&mut st.doc));
            assert!(
                app.status_note.as_deref().unwrap_or("").contains("Copied"),
                "the HUD Copy action copies + toasts ({:?})",
                app.status_note
            );
        }
    }

    /// Seed a synthetic multi-word glyph layer on page 1 ("hello world"),
    /// glyphs on one row with real boxes, so the selection-gesture tests need no
    /// native pdfium. Returns the page-point of the centre of glyph `i`.
    fn seed_words(app: &mut PdfApp, text: &str) {
        let glyphs: Vec<(char, PtRect)> = text
            .chars()
            .enumerate()
            .map(|(i, c)| (c, PtRect::new(72.0 + i as f64 * 9.0, 120.0, 8.0, 12.0)))
            .collect();
        app.raster_text
            .insert(1, std::sync::Arc::new(PageText::from_glyphs(glyphs)));
    }
    fn glyph_pt(i: usize) -> (f64, f64) {
        (72.0 + i as f64 * 9.0 + 4.0, 126.0)
    }

    #[test]
    fn multi_click_selection_snaps_to_words_and_lines() {
        eprintln!("phase double_click_selects_the_word_through_the_selection_path");
        {
            let (mut app, _st) = app_with_state();
            seed_words(&mut app, "hello world");
            let pt = glyph_pt(1); // over the 'e' in "hello"
            let scr = Point::new(pt.0, pt.1);

            // First press → release: a single click places a collapsed caret only.
            assert!(app.begin_selection(1, pt, scr));
            assert!(
                !app.selection.unwrap().is_active(),
                "one click selects no text"
            );
            app.finish_selection();

            // Second press at the same spot within the multi-click window is a
            // double-click → the whole word under the pointer is selected.
            assert!(app.begin_selection(1, pt, scr));
            let sel = app.selection.expect("the double-click armed a selection");
            let (lo, hi) = sel.range();
            assert_eq!(
                app.raster_text[&1].text_for_range(lo, hi),
                "hello",
                "double-click selects the whole word, not one glyph"
            );
            assert!(sel.is_active(), "the word selection is active (HUD shows)");
        }

        // A word-granularity drag (after the double-click) keeps the anchored word
        // inside the selection and grows by whole words — the shared
        // `extend_span_drag` rule, so it never collapses back to the click.
        eprintln!("phase double_click_then_drag_extends_by_whole_words");
        {
            let (mut app, _st) = app_with_state();
            seed_words(&mut app, "hello world again");
            let pt = glyph_pt(1);
            let scr = Point::new(pt.0, pt.1);
            app.begin_selection(1, pt, scr);
            app.finish_selection();
            app.begin_selection(1, pt, scr); // double-click "hello"
            // Drag into "world" (glyph 7) — the selection grows to whole words.
            app.update_selection(1, glyph_pt(7));
            let sel = app.selection.expect("selection live");
            let (lo, hi) = sel.range();
            assert_eq!(
                app.raster_text[&1].text_for_range(lo, hi),
                "hello world",
                "the word drag spans whole words and keeps the first word in"
            );
        }

        // Triple-click selects the whole visual line.
        eprintln!("phase triple_click_selects_the_line");
        {
            let (mut app, _st) = app_with_state();
            seed_words(&mut app, "hello world");
            let pt = glyph_pt(1);
            let scr = Point::new(pt.0, pt.1);
            app.begin_selection(1, pt, scr);
            app.finish_selection();
            app.begin_selection(1, pt, scr);
            app.finish_selection();
            app.begin_selection(1, pt, scr); // third click
            let sel = app.selection.expect("selection live");
            let (lo, hi) = sel.range();
            assert_eq!(
                app.raster_text[&1].text_for_range(lo, hi),
                "hello world",
                "triple-click selects the whole line"
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn chrome_layout_reflows_find_bar_contents_bubble_and_rail() {
        // Find is a band at the top of the page canvas: between the rail and
        // the review panel, above the pages, at the bar's own height.
        eprintln!("phase find_band_tops_the_canvas_column");
        {
            let (mut app, _st) = app_with_state();
            app.open_search();
            let band = app.search_rect;
            let theme = app.layout_theme.clone();
            assert!(
                (band.height() - app.find_bar.height(&theme)).abs() < 1e-9,
                "the band is the bar's height"
            );
            assert!(
                (band.x0 - app.canvas_rect.x0).abs() < 1e-9
                    && (band.x1 - app.canvas_rect.x1).abs() < 1e-9,
                "the band spans the canvas column"
            );
            assert!(
                (band.y1 - app.canvas_rect.y0).abs() < 1e-9,
                "the pages start under the band"
            );
            assert!(
                band.x0 >= app.rail_rect.x1 - 0.01 && band.x1 <= app.sidebar_rect.x0 + 0.01,
                "the band leaves the rail and the review panel alone"
            );
            app.close_search();
            assert_eq!(app.search_rect, Rect::ZERO, "closing removes the band");
        }

        // A long bookmark tree must remain navigable rather than being silently
        // clipped after the first sidebar viewport.  This exercises the same
        // measured list used by paint and the wheel handler, at maximum text size
        // where fixed-height rows used to collide.
        eprintln!("phase contents_sidebar_scrolls_and_scales_all_outline_entries");
        {
            let (mut app, _st) = app_with_state();
            app.sidebar_tab = SidebarTab::Outline;
            app.layout_theme = Theme::dark().with_text_scale(2.0);
            app.doc.outline = (0..80)
                .map(|index| model::OutlineItem {
                    title: format!("Section {index}: an intentionally descriptive heading"),
                    page: index % app.doc.page_count() + 1,
                    level: u8::from(index % 3 != 0),
                })
                .collect();
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);

            let (items, content_h) = app.sidebar_item_layout(&mut tr);
            assert_eq!(items.len(), 80, "every bookmark participates in scrolling");
            assert!(
                items[1].1 - items[0].1 > ui::chrome::nav_row_height(&Theme::dark()),
                "outline rows grow with the text"
            );

            app.sidebar_scroll = f64::MAX;
            app.prepare_sidebar_scroll(&mut tr);
            let body = app.sidebar_body();
            let origin = app.outline_top(body);
            let max_scroll = (content_h - (body.y1 - origin)).max(0.0);
            assert!(max_scroll > 0.0, "the test outline exceeds the viewport");
            assert!(
                (app.sidebar_scroll - max_scroll).abs() < 0.1,
                "wheel state clamps to the final bookmark instead of resetting"
            );
            let (_, last_top, last_h) = items.last().expect("last bookmark");
            assert!(
                last_top + last_h - app.sidebar_scroll <= body.y1 + 0.1,
                "the final bookmark can be brought fully into view"
            );
        }

        // The floating page bubble draws (and captures its nav hit-rects) only
        // while visible, and clears them once it fades — proving it is a transient
        // overlay, not a permanent strip. Drives the real `draw_page_bubble`.
        eprintln!("phase page_bubble_overlay_shows_then_hides");
        {
            let (mut app, _st) = app_with_state();
            app.has_document = true;
            app.canvas_rect = Rect::new(0.0, 100.0, 800.0, 700.0);
            let mut scene = Scene::new();
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();

            // Hidden by default (no page change, pointer away from the bottom): the
            // overlay draws nothing and captures no clickable rect.
            app.last_mouse = Point::new(10.0, 110.0);
            app.draw_page_bubble(&mut scene, Affine::IDENTITY, &mut tr, &theme);
            assert_eq!(
                app.bubble_rect,
                Rect::ZERO,
                "a hidden bubble captures no hit-rect"
            );

            // A page change pokes it → it draws and captures its chevron rects.
            app.page_bubble.poke(ui::time::now());
            app.draw_page_bubble(&mut scene, Affine::IDENTITY, &mut tr, &theme);
            assert_ne!(
                app.bubble_rect,
                Rect::ZERO,
                "a visible bubble captures its rect"
            );
            assert!(
                app.bubble_rect.y1 <= app.canvas_rect.y1 + 0.01,
                "it floats within the canvas"
            );
            assert!(
                app.bubble_rect.y0 > app.canvas_rect.y0,
                "it sits near the bottom, not filling the canvas"
            );
            // The right chevron steps the page (a click is consumed).
            let before = app.current_page;
            assert!(
                app.apply_page_bubble(app.bubble_next.center()),
                "a click on the bubble is consumed"
            );
            assert!(
                app.current_page >= before,
                "the next chevron does not go backwards"
            );

            // After its linger, with the pointer away, it hides again.
            app.page_bubble =
                ui::StatusBubble::new().with_linger(std::time::Duration::from_millis(0));
            app.page_bubble.poke(
                ui::time::now()
                    .checked_sub(std::time::Duration::from_millis(10))
                    .unwrap(),
            );
            app.last_mouse = Point::new(10.0, 110.0);
            app.draw_page_bubble(&mut scene, Affine::IDENTITY, &mut tr, &theme);
            assert_eq!(
                app.bubble_rect,
                Rect::ZERO,
                "the bubble hides after its linger"
            );
        }

        eprintln!("phase rail_toggle_and_resize_reclaim_space");
        {
            let (mut app, _st) = app_with_state();
            let canvas_with_rail = app.canvas_rect.width();
            app.toggle_rail();
            assert!(!app.show_rail);
            assert!(
                app.canvas_rect.width() > canvas_with_rail,
                "hiding the rail reclaims space"
            );
            app.toggle_rail();
            assert!(app.show_rail);
            // Resize within bounds is honoured.
            app.rail_w = 999.0;
            app.recompute_regions();
            assert!(
                (app.rail_width() - app.rail_width_range().1).abs() < 1e-6,
                "rail width clamps to max"
            );
        }

        eprintln!("phase responsive_mode_reflows_on_resize_not_cached");
        {
            let (mut app, _st) = app_with_state(); // starts wide (1280)
            app.reconcile_overlay();
            app.recompute_regions();
            assert!(!app.overlay_mode, "wide window docks panels");
            assert!(
                app.show_sidebar && app.show_rail,
                "docked panels are visible when wide"
            );
            // Wide: the canvas leaves room for the rail + sidebar columns.
            assert!(
                app.canvas_rect.width() < app.rect.width() - 100.0,
                "docked panels reserve columns"
            );

            // Shrink below the overlay breakpoint — panels must collapse to closed
            // overlays and the canvas must span the whole (smaller) window.
            app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
            app.reconcile_overlay();
            app.recompute_regions();
            assert!(app.overlay_mode, "narrow window switches to overlay mode");
            assert!(
                !app.show_sidebar && !app.show_rail,
                "narrow window auto-closes the panels"
            );
            assert!(
                (app.canvas_rect.width() - app.rect.width()).abs() < 1.0,
                "canvas spans the full width in overlay mode"
            );

            // Grow back to wide — panels re-dock.
            app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            app.reconcile_overlay();
            app.recompute_regions();
            assert!(!app.overlay_mode, "returning to wide re-docks");
            assert!(
                app.show_sidebar && app.show_rail,
                "panels come back when wide again"
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn markup_tools_commit_notes_highlights_and_ink() {
        eprintln!("phase create_note_adds_annotation_and_comment");
        {
            let (mut app, mut st) = app_with_state();
            let a0 = app.annotations.len();
            let c0 = app.comments.len();
            app.create_note(&mut Edits::lend(&mut st.doc), 2, (120.0, 200.0));
            assert_eq!(app.annotations.len(), a0 + 1);
            assert_eq!(app.comments.len(), c0 + 1);
            assert_eq!(
                PdfMarkup::of(&st.doc).list_comments().len(),
                c0 + 1,
                "creating a note landed in the shared document"
            );
            assert_eq!(app.sidebar_tab as u8, SidebarTab::Comments as u8);
        }

        eprintln!("phase dragging_a_note_marker_repositions_and_persists");
        {
            let (mut app, mut st) = app_with_state();
            // A fresh note at a known page-point.
            app.create_note(&mut Edits::lend(&mut st.doc), 1, (100.0, 100.0));
            app.reload(&st.doc);
            let id = app.selected_anno.clone().expect("the new note is selected");
            // Its marker is grabbable at its anchor (same target as clicking it).
            assert!(
                app.note_marker_at(1, (100.0, 100.0)).is_some(),
                "the marker is grabbable"
            );
            // Begin → move → the local cache tracks the pointer for live feedback.
            let mut mdrag = ui::MarkerDrag::begin(id.clone(), (100.0, 100.0), (100.0, 100.0));
            let (nx, ny) = mdrag.update((180.0, 140.0));
            assert!(mdrag.moved(), "a real drag latches moved");
            app.move_note_local(&id, nx, ny);
            assert_eq!(
                app.note_pos(&id),
                Some((nx, ny)),
                "the marker follows the pointer live"
            );
            // Release persists the new anchor to the shared document.
            let note = PdfMarkup::paths()
                .annotations
                .item(&ui::tilo_resource::Id::from_raw(id.clone()));
            PdfMarkup::apply_to(
                &mut st.doc,
                ui::tilo_resource::Invoke::group([
                    ui::tilo_resource::Invoke::from(note.mark.set(AnnoKind::Note { x: nx, y: ny })),
                    ui::tilo_resource::Invoke::from(note.space.set(crate::model::GeomSpace::Page)),
                ]),
            );
            app.reload(&st.doc);
            let (x, y) = app.note_pos(&id).expect("the note survives the reposition");
            assert!(
                (x - nx).abs() < 1e-6 && (y - ny).abs() < 1e-6,
                "the reposition persisted to the CRDT"
            );
            assert!(
                (x - 100.0).abs() > 1.0,
                "the note actually moved from its origin"
            );
        }

        eprintln!("phase highlight_drag_commits_annotation");
        {
            let (mut app, mut st) = app_with_state();
            let a0 = app.annotations.len();
            app.tool = Tool::Highlight;
            app.drag = Some(DragState {
                tool: Tool::Highlight,
                page: 1,
                start: (64.0, 100.0),
                cur: (300.0, 116.0),
                points: vec![],
            });
            app.commit_drag(&mut Edits::lend(&mut st.doc));
            assert_eq!(
                app.annotations.len(),
                a0 + 1,
                "drag should add one highlight"
            );
        }

        eprintln!("phase tiny_highlight_drag_is_ignored");
        {
            let (mut app, mut st) = app_with_state();
            let a0 = app.annotations.len();
            app.drag = Some(DragState {
                tool: Tool::Highlight,
                page: 1,
                start: (64.0, 100.0),
                cur: (66.0, 101.0),
                points: vec![],
            });
            app.commit_drag(&mut Edits::lend(&mut st.doc));
            assert_eq!(app.annotations.len(), a0, "sub-threshold drag adds nothing");
        }

        eprintln!("phase highlight_snaps_to_text_when_lines_present");
        {
            let (mut app, mut st) = app_with_state();
            let a0 = app.annotations.len();
            // Seed a few text-line bands spanning two lines and drag across both.
            app.text_lines.insert(
                1,
                vec![
                    render::TextLine {
                        block: 5,
                        x: 64.0,
                        x_end: 480.0,
                        y: 196.0,
                        h: 13.0,
                        words: Vec::new(),
                    },
                    render::TextLine {
                        block: 5,
                        x: 64.0,
                        x_end: 420.0,
                        y: 210.0,
                        h: 13.0,
                        words: Vec::new(),
                    },
                ],
            );
            app.tool = Tool::Highlight;
            app.drag = Some(DragState {
                tool: Tool::Highlight,
                page: 1,
                start: (80.0, 198.0),
                cur: (300.0, 220.0),
                points: vec![],
            });
            app.commit_drag(&mut Edits::lend(&mut st.doc));
            // Two covered lines -> one highlight with two text-shaped bands (not
            // one free rectangle).
            assert_eq!(app.annotations.len(), a0 + 1, "one highlight");
            let added = app.annotations.last().unwrap();
            assert!(
                matches!(&added.kind, AnnoKind::Markup { quads, .. } if quads.len() == 2),
                "drag over two lines snaps to two bands: {:?}",
                added.kind
            );
        }

        eprintln!("phase highlight_falls_back_to_freeform_without_text");
        {
            let (mut app, mut st) = app_with_state();
            let a0 = app.annotations.len();
            // No text-line cache near the drag -> free-form rectangle (one anno).
            app.text_lines.insert(
                1,
                vec![render::TextLine {
                    block: 5,
                    x: 64.0,
                    x_end: 480.0,
                    y: 196.0,
                    h: 13.0,
                    words: Vec::new(),
                }],
            );
            app.tool = Tool::Highlight;
            // Drag far below any seeded line.
            app.drag = Some(DragState {
                tool: Tool::Highlight,
                page: 1,
                start: (80.0, 500.0),
                cur: (300.0, 540.0),
                points: vec![],
            });
            app.commit_drag(&mut Edits::lend(&mut st.doc));
            assert_eq!(
                app.annotations.len(),
                a0 + 1,
                "no nearby text -> single free-form band"
            );
        }

        eprintln!("phase ink_drag_commits_a_polyline_annotation");
        {
            let (mut app, mut st) = app_with_state();
            let a0 = app.annotations.len();
            app.tool = Tool::Ink;
            app.drag = Some(DragState {
                tool: Tool::Ink,
                page: 1,
                start: (80.0, 300.0),
                cur: (240.0, 360.0),
                points: vec![
                    (80.0, 300.0),
                    (140.0, 330.0),
                    (200.0, 320.0),
                    (240.0, 360.0),
                ],
            });
            app.commit_drag(&mut Edits::lend(&mut st.doc));
            assert_eq!(
                app.annotations.len(),
                a0 + 1,
                "an ink drag adds one annotation"
            );
            let ink = app
                .annotations
                .iter()
                .find(|a| matches!(a.kind, AnnoKind::Ink { .. }))
                .expect("an ink stroke was created");
            match &ink.kind {
                AnnoKind::Ink { strokes } => {
                    assert!(strokes[0].len() >= 2, "the polyline retains its points");
                }
                _ => unreachable!(),
            }
            assert!(
                PdfMarkup::of(&st.doc)
                    .list_annotations()
                    .iter()
                    .any(|a| matches!(a.kind, AnnoKind::Ink { .. })),
                "and the ink stroke landed in the shared document"
            );
        }

        eprintln!("phase ink_drag_with_a_single_point_is_discarded");
        {
            let (mut app, mut st) = app_with_state();
            let a0 = app.annotations.len();
            app.tool = Tool::Ink;
            app.drag = Some(DragState {
                tool: Tool::Ink,
                page: 1,
                start: (80.0, 300.0),
                cur: (80.0, 300.0),
                points: vec![(80.0, 300.0)],
            });
            app.commit_drag(&mut Edits::lend(&mut st.doc));
            assert_eq!(
                app.annotations.len(),
                a0,
                "a degenerate single-point ink stroke adds nothing"
            );
        }
    }

    #[test]
    fn find_jumps_to_matches_and_toggles_change_match_logic() {
        eprintln!("phase search_finds_and_jumps");
        {
            let (mut app, _st) = app_with_state();
            app.find_bar.set_query("filesystem");
            app.recompute_matches();
            assert!(!app.matches.is_empty());
            // Active match drives the current page.
            let p = app.matches[0].page;
            assert_eq!(app.current_page, p);
            // Stepping wraps around.
            let first = app.active_match;
            app.step_match(1);
            if app.matches.len() > 1 {
                assert_ne!(app.active_match, first);
            }
        }

        eprintln!("phase search_toggles_change_match_logic");
        {
            let (mut app, _st) = app_with_state();
            app.find_bar.set_query("the");
            app.search_case_sensitive = false;
            app.search_whole_word = false;
            app.recompute_matches();
            let loose = app.matches.len();
            app.search_whole_word = true;
            app.recompute_matches();
            let strict = app.matches.len();
            assert!(
                strict <= loose,
                "whole-word never yields more matches than substring"
            );
        }
    }

    #[test]
    fn placing_markup_clamps_into_the_page_and_focuses_its_sidebar() {
        eprintln!("phase place_signature_clamps_into_page");
        {
            let (mut app, mut st) = app_with_state();
            let s0 = app.signatures.len();
            app.signer_name = "Tester".into();
            app.place_signature(&mut Edits::lend(&mut st.doc), 3, (10.0, 10.0)); // near corner
            assert_eq!(app.signatures.len(), s0 + 1);
            let last = app.signatures.iter().find(|s| s.page == 3).unwrap();
            assert!(last.rect.x >= MARGIN - 0.001);
            assert!(last.rect.y >= MARGIN - 0.001);
        }

        eprintln!("phase placing_signature_focuses_signatures_sidebar");
        {
            let (mut app, mut st) = app_with_state();
            app.sidebar_tab = SidebarTab::Comments;
            app.place_signature(&mut Edits::lend(&mut st.doc), 2, (200.0, 300.0));
            assert_eq!(app.sidebar_tab as u8, SidebarTab::Signatures as u8);
            assert!(app.show_sidebar);
            assert!(app.selected_sig.is_some(), "new signature is selected");
            assert!(
                app.sidebar_reveal.is_some(),
                "reveal request queued for the new entry"
            );
        }

        eprintln!("phase creating_note_focuses_comments_sidebar_on_new_thread");
        {
            let (mut app, mut st) = app_with_state();
            app.sidebar_tab = SidebarTab::Signatures;
            app.create_note(&mut Edits::lend(&mut st.doc), 2, (200.0, 300.0));
            assert_eq!(app.sidebar_tab as u8, SidebarTab::Comments as u8);
            assert!(app.show_sidebar);
            assert!(
                app.sidebar_reveal.is_some(),
                "new comment queued to scroll into view"
            );
        }
    }

    #[test]
    fn navigation_and_zoom_clamp_and_keep_the_focal_point() {
        eprintln!("phase page_navigation_clamps");
        {
            let (mut app, _st) = app_with_state();
            app.go_to_page(999);
            assert_eq!(app.current_page, app.doc.page_count());
            app.go_to_page(0);
            assert_eq!(app.current_page, 1);
        }

        eprintln!("phase zoom_switches_to_custom");
        {
            let (mut app, _st) = app_with_state();
            app.zoom_by(1.25);
            assert!(matches!(app.fit, FitMode::Custom(_)));
        }

        eprintln!("phase zoom_at_keeps_focal_point_stationary");
        {
            let (mut app, _st) = app_with_state();
            // Start already overflowing and mid-scrolled so neither axis clamps
            // during the zoom — the focal point can then stay exactly put.
            app.fit = FitMode::Custom(2.0);
            app.recompute_zoom();
            app.go_to_page(1);
            let (half_x, sy) = (app.viewport.max_scroll_x() / 2.0, app.scroll_y());
            app.viewport.set_scroll(half_x, sy);
            // Build a focal point that demonstrably lands inside page 1's card so
            // neither axis is at a scroll clamp.
            let origin = app.page_origins()[0];
            let focal = Point::new(origin.x + 300.0 * app.zoom(), origin.y + 200.0 * app.zoom());
            let before = app.hit_page(focal).expect("focal lands on a page");
            app.zoom_at(1.3, focal);
            let after = app.hit_page(focal).expect("focal still on a page");
            assert_eq!(before.0, after.0, "same page stays under the cursor");
            assert!(
                (before.1.0 - after.1.0).abs() < 2.0,
                "x focal drift {} too large",
                (before.1.0 - after.1.0).abs()
            );
            assert!(
                (before.1.1 - after.1.1).abs() < 2.0,
                "y focal drift {} too large",
                (before.1.1 - after.1.1).abs()
            );
        }

        eprintln!("phase zoom_respects_min_max_bounds");
        {
            let (mut app, _st) = app_with_state();
            for _ in 0..40 {
                app.zoom_by(1.5);
            }
            assert!(app.zoom() <= ZOOM_MAX + 1e-6);
            for _ in 0..40 {
                app.zoom_by(0.5);
            }
            assert!(app.zoom() >= ZOOM_MIN - 1e-6);
        }

        eprintln!("phase horizontal_pan_reaches_full_width_when_zoomed");
        {
            let (mut app, _st) = app_with_state();
            app.fit = FitMode::Custom(3.0);
            app.recompute_zoom();
            app.clamp_scroll();
            assert!(app.is_overflowing_x(), "3x zoom must overflow the viewport");
            let sy = app.scroll_y();
            app.viewport.set_scroll(f64::MAX, sy);
            // The full reachable width is content_width - canvas width.
            let expected = (app.viewport.scaled_content_width() - app.canvas_rect.width()).max(0.0);
            assert!(
                (app.scroll_x() - expected).abs() < 1.0,
                "pan should reach the full zoomed width"
            );
        }
    }

    #[test]
    fn delete_targets_remove_markup_and_anchored_annotations() {
        eprintln!("phase delete_selected_annotation_removes_it");
        {
            let (mut app, mut st) = app_with_state();
            let hl = app
                .annotations
                .iter()
                .find(|a| matches!(a.kind, AnnoKind::Markup { .. }))
                .unwrap()
                .clone();
            let n = app.annotations.len();
            app.selected_anno = Some(hl.id.clone());
            assert!(app.delete_selected(&mut Edits::lend(&mut st.doc)));
            assert_eq!(app.annotations.len(), n - 1);
            assert!(!app.annotations.iter().any(|a| a.id == hl.id));
            assert!(app.selected_anno.is_none());
        }

        eprintln!("phase delete_signature_target_removes_it");
        {
            let (mut app, mut st) = app_with_state();
            let sig = app.signatures[0].clone();
            let n = app.signatures.len();
            app.delete_target(
                &DeleteTarget::Signature(sig.id.clone()),
                &mut Edits::lend(&mut st.doc),
            );
            assert_eq!(app.signatures.len(), n - 1);
            assert!(!app.signatures.iter().any(|s| s.id == sig.id));
        }

        eprintln!("phase delete_comment_target_also_drops_its_annotation");
        {
            let (mut app, mut st) = app_with_state();
            // The note's linked comment: deleting it must drop the note marker too.
            let note = app
                .annotations
                .iter()
                .find(|a| matches!(a.kind, AnnoKind::Note { .. }))
                .unwrap()
                .clone();
            let comment = app
                .comments
                .iter()
                .find(|c| c.anno_id.as_deref() == Some(note.id.as_str()))
                .unwrap()
                .clone();
            app.delete_target(
                &DeleteTarget::Comment(comment.id.clone()),
                &mut Edits::lend(&mut st.doc),
            );
            assert!(!app.comments.iter().any(|c| c.id == comment.id));
            assert!(
                !app.annotations.iter().any(|a| a.id == note.id),
                "orphan note marker removed"
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn signatures_stamp_real_dates_and_store_typed_drawn_and_edited_signers() {
        eprintln!("phase today_is_the_local_date");
        {
            // `today_ymd` is well-formed `YYYY-MM-DD`.
            let t = today_ymd();
            assert_eq!(t.len(), 10, "today is YYYY-MM-DD: {t}");
            assert_eq!(&t[4..5], "-");
            assert_eq!(&t[7..8], "-");
        }

        eprintln!("phase placed_signature_stamps_the_real_date_not_a_constant");
        {
            let (mut app, mut st) = app_with_state();
            app.signer_name = "Grace Hopper".into();
            app.place_signature(&mut Edits::lend(&mut st.doc), 2, (200.0, 300.0));
            let s = app.signatures.iter().find(|s| s.page == 2).unwrap().clone();
            // The stamp is the live system date, not the frozen demo/seed date.
            assert_eq!(
                s.date,
                today_ymd(),
                "runtime signature carries today's date"
            );
            assert_ne!(
                s.date,
                crate::scenarios::SIGNED_ON,
                "runtime date is not the hardcoded demo constant"
            );
            // And the real date is what durably lands in the shared CRDT.
            let stored = PdfMarkup::of(&st.doc).list_signatures();
            assert!(stored.iter().any(|x| x.id == s.id && x.date == today_ymd()));
        }

        // A typed signature stamps the signer's name as its content — real input,
        // no canned squiggle.
        eprintln!("phase placing_a_typed_signature_stores_the_typed_name");
        {
            let (mut app, mut st) = app_with_state();
            app.sig_mode = SigMode::Type;
            app.signer_name = "Grace Hopper".into();
            app.place_signature(&mut Edits::lend(&mut st.doc), 3, (200.0, 300.0));
            let placed = app
                .signatures
                .iter()
                .find(|s| s.page == 3)
                .expect("a signature landed");
            assert_eq!(placed.signer, "Grace Hopper");
            assert_eq!(
                placed.ink,
                SignatureInk::Typed,
                "a typed signature carries typed ink"
            );
            // And it durably reaches the shared CRDT as typed ink.
            let stored = PdfMarkup::of(&st.doc).list_signatures();
            assert!(
                stored
                    .iter()
                    .any(|x| x.id == placed.id && x.ink == SignatureInk::Typed)
            );
        }

        // A drawn signature stores the exact strokes the user drew, and placement
        // writes them into the document; an *empty* drawn signature is refused so
        // no blank "signed" field is ever stamped.
        eprintln!("phase drawn_signature_places_the_strokes_and_empty_is_refused");
        {
            let (mut app, mut st) = app_with_state();
            app.sig_mode = SigMode::Draw;

            // Empty pad → nothing is placed, and the user is told to draw first.
            let before = app.signatures.len();
            app.place_signature(&mut Edits::lend(&mut st.doc), 2, (200.0, 300.0));
            assert_eq!(
                app.signatures.len(),
                before,
                "an empty drawn signature is not placed"
            );
            assert!(
                app.status_note
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains("draw")
            );

            // Draw a stroke, then place: the strokes are what land in the document.
            app.sig_strokes = vec![vec![(0.1, 0.5), (0.4, 0.2), (0.7, 0.6), (0.9, 0.4)]];
            app.place_signature(&mut Edits::lend(&mut st.doc), 2, (200.0, 300.0));
            let placed = app
                .signatures
                .iter()
                .find(|s| s.page == 2)
                .expect("a drawn signature landed");
            match &placed.ink {
                SignatureInk::Drawn { strokes } => {
                    assert_eq!(strokes.len(), 1);
                    assert_eq!(strokes[0].len(), 4, "the drawn polyline is preserved");
                }
                SignatureInk::Typed => panic!("expected drawn ink, got Typed"),
            }
            let stored = PdfMarkup::of(&st.doc).list_signatures();
            assert!(
                stored.iter().any(
                    |x| matches!(&x.ink, SignatureInk::Drawn { strokes } if strokes.len() == 1)
                )
            );
        }

        eprintln!("phase signer_name_inline_edit_commits_and_empty_falls_back_to_author");
        {
            let (mut app, mut st) = app_with_state();
            // Type a real name into the signer identity field and commit it.
            app.begin_edit(EditTarget::Signer, &app.signer_name.clone());
            app.editing.as_mut().unwrap().set_text("Grace Hopper");
            app.commit_edit(&mut Edits::lend(&mut st.doc));
            assert!(app.editing.is_none(), "committing closes the editor");
            assert_eq!(
                app.signer_name, "Grace Hopper",
                "signer name updates from the editor"
            );
            // A subsequent signature signs under the new identity.
            app.place_signature(&mut Edits::lend(&mut st.doc), 4, (200.0, 300.0));
            assert_eq!(
                app.signatures.iter().find(|s| s.page == 4).unwrap().signer,
                "Grace Hopper"
            );
            // Committing an empty name falls back to the app author identity.
            app.begin_edit(EditTarget::Signer, "whatever");
            app.editing.as_mut().unwrap().set_text("   ");
            app.commit_edit(&mut Edits::lend(&mut st.doc));
            assert_eq!(
                app.signer_name, app.author,
                "an empty signer name falls back to the author"
            );
        }

        eprintln!("phase comment_authors_read_as_the_rosters_names");
        {
            let (mut app, mut st) = app_with_state();
            // The test dispatcher acts as `test-author`.
            app.author = "test-author".into();
            app.attach_people(ui::framework::People::named(
                Some("test-author"),
                &[("test-author", "Tess Author"), ("peer-id", "Bob Builder")],
            ));
            app.create_note(&mut Edits::lend(&mut st.doc), 2, (120.0, 200.0));
            let thread = PdfMarkup::of(&st.doc)
                .list_comments()
                .into_iter()
                .find(|comment| comment.page == 2)
                .expect("the new note's thread");
            assert_eq!(thread.author, "test-author", "the record keeps the id");
            assert_eq!(
                app.author_name(&thread.author),
                "Tess Author",
                "your own note is signed with your name, not \"You\""
            );
            assert_eq!(app.author_name("peer-id"), "Bob Builder");
            assert_eq!(
                app.author_name("Carol"),
                "Carol",
                "an unknown id reads as itself"
            );
            let described = format!("{:?}", Widget::accessibility(&app));
            assert!(
                described.contains("Tess Author on page 2"),
                "assistive technology hears the name, not the id"
            );
        }

        eprintln!("phase the_signature_is_the_persons_own_and_follows_them");
        {
            let mut host = ui::framework::testing::TestApp::<crate::PdfViewerShell>::for_person(
                &"ad".repeat(32),
            )
            .expect("the reader for a person");
            let record = host
                .person::<crate::signature::SignatureProfile>()
                .expect("the person's signature record");
            let (mut app, mut st) = app_with_state();
            app.attach_signature(record.clone());
            app.begin_edit(EditTarget::Signer, "");
            app.editing.as_mut().unwrap().set_text("Ada Lovelace");
            app.commit_edit(&mut Edits::lend(&mut st.doc));
            app.sig_strokes = vec![vec![(0.1, 0.5), (0.9, 0.4)]];
            app.keep_strokes();
            assert_eq!(record.get().signer, "Ada Lovelace", "the signer is kept");
            assert_eq!(
                record.get().strokes.0,
                app.sig_strokes,
                "the strokes are kept"
            );
            assert!(
                PdfMarkup::of(&st.doc).list_signatures().is_empty()
                    || PdfMarkup::of(&st.doc)
                        .list_signatures()
                        .iter()
                        .all(|signature| signature.signer != "Ada Lovelace"),
                "composing a signature writes nothing into the shared document"
            );

            // Another window of the same person composes with the same signature.
            let (mut other, mut other_st) = app_with_state();
            other.attach_signature(record);
            sync_fixture(&mut other, &mut other_st);
            assert_eq!(other.signer_name, "Ada Lovelace");
            assert_eq!(other.sig_strokes, vec![vec![(0.1, 0.5), (0.9, 0.4)]]);
        }
    }

    // ----------------------------------------------------------------
    // Real-PDF (pdfium raster) tests: text layer + save-back.
    //
    // These exercise the pdfium backend directly, so they require a usable
    // `libpdfium.so`. `crate::source::find_pdfium` already probes the well-known
    // paths, `~/.local/lib/pdfium`, and `TILO_PDFIUM_LIB`.
    //
    // pdfium is not packaged for the distribution the CI image is built from
    // — Fedora ships `deepin-pdfium`, which is a different library behind a
    // different ABI, not the one this loads — so on that machine there is no
    // amount of installing that makes these run. They used to fail there,
    // which is thirteen red tests that say nothing except "this is not a
    // developer's workstation", every run, for as long as that stays true.
    //
    // So they say what they could not look at and stop, rather than either
    // failing on it or passing as though they had checked. `TILO_REQUIRE_PDFIUM`
    // turns the absence back into a failure, for anywhere the backend is
    // supposed to be present and its absence is the bug — the image build is
    // the case that wants it.
    // ----------------------------------------------------------------

    const SAMPLE_PDF: &str = concat!(env!("OUT_DIR"), "/sample.pdf");

    /// Why these tests cannot run, in the words the operator needs.
    const NO_PDFIUM: &str = "no usable libpdfium.so — install one (e.g. \
         ~/.local/lib/pdfium/libpdfium.so) or set TILO_PDFIUM_LIB";

    /// Open `path` through the PDF worker, as the reader opens any file, and
    /// wait for the open to settle — or say plainly that the backend is not
    /// here and let the caller stand down.
    ///
    /// `None` is never "it worked": every caller returns on it, so the test
    /// reports as passed having announced on stderr exactly which property
    /// went unchecked. Set `TILO_REQUIRE_PDFIUM` to make that a failure.
    pub(super) fn open_settled(path: impl AsRef<std::path::Path>) -> Option<PdfApp> {
        let path = path.as_ref();
        let mut app = PdfApp::open(path);
        drain_until(&mut app, |app| app.pending_open.is_none());
        match pdfium_gate(app.raster_active(), require_pdfium()) {
            PdfiumGate::Run => Some(app),
            PdfiumGate::Fail => panic!(
                "TILO_REQUIRE_PDFIUM is set and there is {NO_PDFIUM} ({:?})",
                app.load_error
            ),
            PdfiumGate::Skip => {
                eprintln!(
                    "SKIPPING a pdfium-backend test on {}: {NO_PDFIUM}. The \
                     raster text layer, save-back and page-editing paths went \
                     unverified.",
                    path.display()
                );
                None
            }
        }
    }

    /// The bundled sample, opened through the PDF worker (see [`open_settled`]).
    fn open_raster_sample() -> Option<PdfApp> {
        open_settled(SAMPLE_PDF)
    }

    /// Run the reader's frames over `st` until `done` holds, as a window's
    /// frames run while its PDF worker answers.
    pub(super) fn settle_until(
        app: &mut PdfApp,
        st: &mut ui::State,
        mut done: impl FnMut(&PdfApp) -> bool,
    ) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            app.sync_settled(&mut st.doc, &st.root);
            if done(app) {
                return;
            }
            assert!(
                ui::time::now() < deadline,
                "the PDF worker never answered ({:?})",
                app.status_note
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Run frames until the record holds what opening the document adds: its
    /// page plan, pinned source and the markup taken over from the file.
    pub(super) fn settle_record(app: &mut PdfApp, st: &mut ui::State) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            app.sync_settled(&mut st.doc, &st.root);
            let adopted = app.source_blob.as_ref().is_some_and(|(hash, _)| {
                PdfMarkup::of(&st.doc).imported_source() == Some(hash.as_str())
            });
            if !app.seed_pending && adopted {
                return;
            }
            assert!(
                ui::time::now() < deadline,
                "the record never took the document over ({:?})",
                app.status_note
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Page `page`'s (1-based) engine text layer, as the worker delivers it
    /// for a selection or search.
    pub(super) fn text_layer(app: &mut PdfApp, st: &mut ui::State, page: usize) -> Arc<PageText> {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(layer) = app.ensure_raster_text(page) {
                return layer;
            }
            assert!(
                ui::time::now() < deadline,
                "the PDF worker never answered ({:?})",
                app.status_note
            );
            // A reconcile answered in the same frame as the text drops both
            // the layer and the request; ask again, as the next drawn frame does.
            settle_until(app, st, |app| {
                app.raster_text.contains_key(&page)
                    || !app.raster_text_pending.contains(&(page - 1))
            });
        }
    }

    /// Take the PDF worker's results until `done` holds, for results that
    /// leave the record alone (an open, a save).
    pub(super) fn drain_until(app: &mut PdfApp, mut done: impl FnMut(&PdfApp) -> bool) {
        let state = AutoCommit::new();
        let deadline = tilo_test_env::hang_deadline();
        while !done(app) {
            assert!(
                ui::time::now() < deadline,
                "the PDF worker never answered ({:?})",
                app.status_note
            );
            std::thread::sleep(Duration::from_millis(5));
            app.drain_raster_events(&state);
        }
    }

    /// Save the markup onto a copy of the PDF at `out` and wait for the
    /// worker to write it; whether it did.
    pub(super) fn save_settled(app: &mut PdfApp, out: &std::path::Path) -> bool {
        if !app.save_annotated_to(out) {
            return false;
        }
        drain_until(app, |app| app.pending_save.is_none());
        app.status_note
            .as_deref()
            .is_some_and(|note| note.starts_with("Saved"))
    }

    /// What a pdfium-backed test should do about the backend's absence.
    #[derive(Debug, PartialEq, Eq)]
    enum PdfiumGate {
        /// The backend is here; run the test.
        Run,
        /// It is not, and nobody said it had to be: say so and stand down.
        Skip,
        /// It is not, and someone said it had to be. That is the bug.
        Fail,
    }

    fn pdfium_gate(has_raster: bool, required: bool) -> PdfiumGate {
        match (has_raster, required) {
            (true, _) => PdfiumGate::Run,
            (false, true) => PdfiumGate::Fail,
            (false, false) => PdfiumGate::Skip,
        }
    }

    fn require_pdfium() -> bool {
        std::env::var_os("TILO_REQUIRE_PDFIUM").is_some()
    }

    #[serial(pdfium)]
    #[test]
    fn raster_open_is_gated_measured_and_budgeted_on_the_first_frame() {
        // The escape hatch is the whole reason skipping is defensible, so it
        // is checked rather than assumed.
        //
        // Written against the decision and not against the environment on
        // purpose: there is no supported way to make `find_pdfium` fail on a
        // machine that has the library, so a test that tried to arrange the
        // absence would be testing its own mock. What can be pinned is that
        // a missing backend is a skip only while nobody has asked for it, and
        // a failure the moment somebody has.
        eprintln!("phase a_missing_pdfium_skips_unless_it_was_required");
        {
            assert_eq!(pdfium_gate(true, false), PdfiumGate::Run);
            assert_eq!(pdfium_gate(true, true), PdfiumGate::Run);
            assert_eq!(
                pdfium_gate(false, false),
                PdfiumGate::Skip,
                "with no backend and no demand for one, say so and stand down"
            );
            assert_eq!(
                pdfium_gate(false, true),
                PdfiumGate::Fail,
                "TILO_REQUIRE_PDFIUM has to turn the absence back into a failure, \
                 or the skip has no floor under it"
            );
        }

        // Measure the open path so regressions surface as numbers, not vibes.
        // Times: document open (load + page sizes, no rendering), first text-layer
        // extraction, a cold first-page rasterise, and a cache-hit rasterise
        // (the scroll-frame cost). Prints under `--nocapture`; the assertions are
        // loose sanity bounds, not perf gates.
        eprintln!("phase measure_open_and_render_perf");
        'phase: {
            let t = ui::time::now();
            let loaded =
                crate::source::open(std::path::Path::new(SAMPLE_PDF), &ContentStore::memory());
            let open_ms = t.elapsed().as_secs_f64() * 1000.0;
            match pdfium_gate(loaded.is_ok(), require_pdfium()) {
                PdfiumGate::Run => {}
                PdfiumGate::Fail => panic!("TILO_REQUIRE_PDFIUM is set and there is {NO_PDFIUM}"),
                PdfiumGate::Skip => {
                    eprintln!("SKIPPING the pdfium open/render measurement: {NO_PDFIUM}.");
                    break 'phase;
                }
            }
            let mut raster = loaded.expect("the sample opens").raster;
            let pages = raster.page_count();

            let t = ui::time::now();
            let layer = raster.page_text(0);
            let text_ms = t.elapsed().as_secs_f64() * 1000.0;
            let glyphs = layer.chars.len();

            // A fit-page render is ~US-Letter width (612pt) times the 2x supersample.
            let bucket = 1224u32;
            let t = ui::time::now();
            let img = raster.render(0, bucket);
            let render_cold_ms = t.elapsed().as_secs_f64() * 1000.0;
            assert!(img.is_some(), "page 0 rasterises");

            let t = ui::time::now();
            let _ = raster.render(0, bucket);
            let render_cached_ms = t.elapsed().as_secs_f64() * 1000.0;

            eprintln!(
                "PERF pages={pages} glyphs={glyphs} | open={open_ms:.1}ms \
                 text_extract={text_ms:.1}ms render_cold={render_cold_ms:.1}ms \
                 render_cached(scroll-frame)={render_cached_ms:.3}ms"
            );
            assert!(
                render_cached_ms < render_cold_ms.max(1.0),
                "cached render is faster than cold"
            );
        }

        // Opening a document and painting its first frame must not pay the pdfium
        // text-extraction cost (hundreds of ms per page): the selectable text layer
        // only feeds selection + search overlays, so it stays unbuilt until one of
        // those is actually live. This guards the deferral — a regression that
        // re-adds eager per-frame extraction reintroduces the multi-hundred-ms
        // open/scroll hitch this reader used to have. Prints the real frame times.
        eprintln!("phase first_frame_defers_text_extraction");
        'phase: {
            use std::collections::HashMap;
            use ui::presence::PresenceCtx;
            use ui::text_render::TextRenderer;
            use ui::theme::Theme;

            let Some(mut app) = open_raster_sample() else {
                break 'phase;
            };
            let mut st = ui::State::new();
            let theme = Theme::dark();
            let (w, h) = (1280.0f64, 800.0f64);

            // Reuse ONE TextRenderer across frames, exactly as the live app does, so
            // the timing reflects real per-frame cost and not the one-time font-atlas
            // init the shot harness pays on every `full_frame` call.
            let mut tr = TextRenderer::new();
            // Drive one full 5-pass frame against the shared renderer.
            let frame = |app: &mut PdfApp, st: &mut ui::State, tr: &mut TextRenderer| {
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                sync_fixture(app, st);
                let child = app.build_layout(&mut taffy, &theme, &mut node_map);
                let root = taffy
                    .new_with_children(
                        Style {
                            size: Size::from_lengths(w as f32, h as f32),
                            ..Default::default()
                        },
                        &[child],
                    )
                    .unwrap();
                let measure_map = node_map.clone();
                let trm = &mut *tr;
                taffy
                    .compute_layout_with_measure(
                        root,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(w as f32),
                            height: taffy::AvailableSpace::Definite(h as f32),
                        },
                        |known, avail, node_id, _, _| {
                            if let Some(&ptr) = measure_map.get(&node_id) {
                                unsafe { (*ptr).measure(known, avail, trm) }
                            } else {
                                taffy::Size::ZERO
                            }
                        },
                    )
                    .unwrap();
                app.apply_layout(&taffy, child, Point::ORIGIN);
                let mut scene = Scene::new();
                app.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    tr,
                    &theme,
                    &PresenceCtx::empty(),
                );
            };

            // Warm the shared renderer's font atlas so it is not charged to frame 1.
            frame(&mut app, &mut st, &mut tr);
            app.raster_text.clear();

            // An idle document frame: no selection, no search — no text extraction.
            let t = ui::time::now();
            frame(&mut app, &mut st, &mut tr);
            let idle_open_ms = t.elapsed().as_secs_f64() * 1000.0;
            let idle_text_pages = app.raster_text.len() + app.raster_text_pending.len();

            let t = ui::time::now();
            frame(&mut app, &mut st, &mut tr);
            let idle_scroll_ms = t.elapsed().as_secs_f64() * 1000.0;

            eprintln!(
                "PERF-FRAME idle_open={idle_open_ms:.2}ms idle_scroll={idle_scroll_ms:.2}ms \
                 idle_text_pages={idle_text_pages}"
            );
            assert_eq!(
                idle_text_pages, 0,
                "an idle frame must defer pdfium text extraction (only selection/search needs it)"
            );
        }

        // Opening a document must not ask for a screen-full of pages on the very
        // first frame: a real page (and, for a scanned PDF, even its rail
        // thumbnail) costs tens-to-hundreds of ms, so the open→first-paint frame
        // asks the worker for only the single top page and the rest on
        // follow-up frames. This guards that budget — and that the backlog
        // drains and the reader settles.
        eprintln!("phase open_rasterises_only_the_budgeted_pages_on_the_first_frame");
        'phase: {
            use std::collections::HashMap;
            use ui::presence::PresenceCtx;
            use ui::text_render::TextRenderer;
            use ui::theme::Theme;

            let Some(mut app) = open_raster_sample() else {
                break 'phase;
            };
            let mut st = ui::State::new();
            let theme = Theme::dark();
            let (w, h) = (1280.0f64, 800.0f64);
            let mut tr = TextRenderer::new();

            let frame = |app: &mut PdfApp, st: &mut ui::State, tr: &mut TextRenderer| {
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                sync_fixture(app, st);
                let child = app.build_layout(&mut taffy, &theme, &mut node_map);
                let root = taffy
                    .new_with_children(
                        Style {
                            size: Size::from_lengths(w as f32, h as f32),
                            ..Default::default()
                        },
                        &[child],
                    )
                    .unwrap();
                let measure_map = node_map.clone();
                let trm = &mut *tr;
                taffy
                    .compute_layout_with_measure(
                        root,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(w as f32),
                            height: taffy::AvailableSpace::Definite(h as f32),
                        },
                        |known, avail, node_id, _, _| {
                            if let Some(&ptr) = measure_map.get(&node_id) {
                                unsafe { (*ptr).measure(known, avail, trm) }
                            } else {
                                taffy::Size::ZERO
                            }
                        },
                    )
                    .unwrap();
                app.apply_layout(&taffy, child, Point::ORIGIN);
                let mut scene = Scene::new();
                app.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    tr,
                    &theme,
                    &PresenceCtx::empty(),
                );
            };

            // The very first painted frame after open. Rail thumbnails are at
            // most 600 px wide; anything wider is a full page.
            frame(&mut app, &mut st, &mut tr);
            let full_pages_first = app
                .raster_render_pending
                .iter()
                .filter(|(_, bucket)| *bucket > 600)
                .count();
            assert!(
                full_pages_first <= MAX_NEW_PAGE_RASTERS_PER_FRAME,
                "first paint asks for at most {MAX_NEW_PAGE_RASTERS_PER_FRAME} full page(s), not a screen-full — got {full_pages_first}"
            );

            // The deferred work drains over the next frames and the reader settles.
            let deadline = tilo_test_env::hang_deadline();
            while app.pending_visuals && ui::time::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
                frame(&mut app, &mut st, &mut tr);
            }
            assert!(
                !app.pending_visuals && app.raster_render_pending.is_empty(),
                "the deferred page backlog drains and the reader settles back to idle"
            );
        }
    }

    /// The Typst-typeset text fixture shared with the engine's tests.
    const LATIN_PDF: &str = concat!(env!("OUT_DIR"), "/latin.pdf");

    fn open_latin_raster() -> Option<PdfApp> {
        open_settled(LATIN_PDF)
    }

    /// Lay the reader out at 100 %, then switch to `zoom` without drawing, so
    /// the next `rasterize_visible` is the first frame at that zoom.
    fn zoom_without_frame(app: PdfApp, zoom: f64) -> Framed<PdfApp> {
        let mut app = framed_at_zoom(app, ui::State::new(), 1.0, None);
        rezoom_without_frame(&mut app, zoom);
        app
    }

    /// Switch an already laid-out reader to `zoom` without drawing.
    fn rezoom_without_frame(app: &mut PdfApp, zoom: f64) {
        app.fit = FitMode::Custom(zoom);
        app.viewport.set_zoom(zoom);
    }

    /// Run frames until every visible tile has arrived from the worker.
    fn settle_tiles(app: &mut Framed<PdfApp>) -> RasterFrame {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let frame = app.rasterize_visible();
            if !app.pending_visuals {
                return frame;
            }
            assert!(
                ui::time::now() < deadline,
                "the viewport's tiles never all arrived"
            );
            std::thread::sleep(Duration::from_millis(5));
            let (reader, st) = Framed::split(app);
            reader.sync_settled(&mut st.doc, &st.root);
        }
    }

    /// The zoom levels the reader holds worker tiles for.
    fn tile_levels(app: &PdfApp) -> Vec<crate::tiles::Level> {
        let mut levels: Vec<_> = app.raster_tiles.keys().map(|key| key.level).collect();
        levels.sort_unstable();
        levels.dedup();
        levels
    }

    /// The screen area of `canvas ∩ page` the tiles of `tiles` cover, and
    /// the area of `canvas ∩ page` itself.
    fn tile_coverage(app: &PdfApp, tiles: &render::PageTiles) -> (f64, f64) {
        let canvas = app.canvas_rect;
        let p = tiles.placement;
        let (pw, ph) = crate::tiles::page_pixels(p.size, p.level);
        let (px0, py0, px1, py1) = p.screen_rect(tilo_pdf::PixelRect {
            x: 0,
            y: 0,
            width: pw,
            height: ph,
        });
        let target = Rect::new(px0, py0, px1, py1).intersect(canvas);
        let covered: f64 = tiles
            .tiles
            .iter()
            .map(|(region, _)| {
                let (x0, y0, x1, y1) = p.screen_rect(*region);
                let r = Rect::new(x0, y0, x1, y1).intersect(target);
                r.width().max(0.0) * r.height().max(0.0)
            })
            .sum();
        (covered, target.width().max(0.0) * target.height().max(0.0))
    }

    #[serial(pdfium)]
    #[test]
    fn zoomed_pages_draw_from_reusable_worker_tiles_matching_full_renders() {
        // At 800 % a page is far wider than the old 4096 px whole-page cap; it
        // is drawn from tiles at a level sharper than that cap allowed,
        // covering the viewport exactly.
        eprintln!("phase zoomed_pages_are_drawn_from_tiles_past_the_old_bitmap_cap");
        'phase: {
            let Some(app) = open_latin_raster() else {
                break 'phase;
            };
            let mut app = zoom_without_frame(app, 8.0);
            let (_, _, settled) = settle_tiles(&mut app);
            let tiles = settled.values().next().expect("the zoomed page is tiled");
            let (pw, _) = crate::tiles::page_pixels(tiles.placement.size, tiles.placement.level);
            assert!(
                pw > 4096,
                "the page is rendered {pw} px wide, past the whole-page cap"
            );
            let (covered, target) = tile_coverage(&app, tiles);
            assert!(target > 0.0);
            assert!(
                (covered - target).abs() / target < 1e-6,
                "visible tiles cover the viewport exactly: {covered} of {target}"
            );
            assert!(tiles.tiles.iter().all(|(r, image)| {
                image.width == r.width && image.height == r.height && r.width <= 512
            }));
            assert_eq!(tile_levels(&app), vec![tiles.placement.level]);
        }

        // Tiles are kept per zoom level: zooming out and back finds the first
        // level's tiles still there, so the viewport is complete on the very
        // first frame back.
        eprintln!("phase returning_to_a_zoom_level_reuses_its_tiles");
        'phase: {
            let Some(app) = open_latin_raster() else {
                break 'phase;
            };
            let mut app = zoom_without_frame(app, 8.0);
            let (_, _, at_eight) = settle_tiles(&mut app);
            let count: usize = at_eight.values().map(|t| t.tiles.len()).sum();
            assert!(count > 0, "the zoomed viewport is drawn from tiles");
            let eight = at_eight.values().next().unwrap().placement.level;
            let scroll = (app.scroll_x(), app.scroll_y());

            rezoom_without_frame(&mut app, 5.0);
            let (_, _, at_five) = settle_tiles(&mut app);
            let five = at_five
                .values()
                .next()
                .expect("5x is tiled")
                .placement
                .level;
            assert_ne!(eight, five);
            assert_eq!(tile_levels(&app), {
                let mut levels = vec![eight, five];
                levels.sort_unstable();
                levels
            });

            rezoom_without_frame(&mut app, 8.0);
            app.viewport.set_scroll(scroll.0, scroll.1);
            let (_, _, back) = app.rasterize_visible();
            assert!(!app.pending_visuals, "every tile was already here");
            assert_eq!(back.values().map(|t| t.tiles.len()).sum::<usize>(), count);
        }

        // The production path: the owner worker renders the placeholder and the
        // tile batch the draw pass hands it, and the viewport fills from tiles.
        eprintln!("phase the_owner_worker_fills_a_zoomed_viewport_with_tiles");
        'phase: {
            let mut app = PdfApp::empty();
            let mut st = ui::State::new();
            app.queue_worker_open(std::path::Path::new(LATIN_PDF));
            let deadline = tilo_test_env::hang_deadline();
            while app.pending_open.is_some() && ui::time::now() < deadline {
                app.sync_settled(&mut st.doc, &st.root);
                std::thread::sleep(Duration::from_millis(5));
            }
            if !app.has_document {
                assert!(!require_pdfium(), "fixture must open: {:?}", app.load_error);
                eprintln!("SKIPPING the worker tiled-zoom test: {NO_PDFIUM}");
                break 'phase;
            }
            // The page at 100 % arrives first; it is what shows under the tiles.
            let mut app = framed_at_zoom(app, st, 1.0, None);
            while app.raster_images.is_empty() && ui::time::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
                full_frame(&mut app, 1280.0, 800.0, None);
            }
            app.fit = FitMode::Custom(8.0);
            app.viewport.set_zoom(8.0);

            let mut saw_placeholder_while_pending = false;
            let placement = loop {
                let (placeholders, _, tiles) = app.rasterize_visible();
                let page = tiles.values().next().expect("the zoomed page is tiled");
                let (covered, target) = tile_coverage(&app, page);
                if app.pending_visuals && placeholders.contains_key(&(page.placement.page + 1)) {
                    saw_placeholder_while_pending = true;
                }
                if !app.pending_visuals && (covered - target).abs() / target < 1e-6 {
                    break Some(page.placement);
                }
                if ui::time::now() >= deadline {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(5));
                let (reader, st) = Framed::split(&mut app);
                reader.sync_settled(&mut st.doc, &st.root);
            };
            let placement = placement.expect("the worker filled the viewport with tiles");
            assert!(
                saw_placeholder_while_pending,
                "the placeholder showed while tiles were on their way"
            );
            // Whatever is still queued is prefetch margin, never a visible tile.
            let canvas = app.canvas_rect;
            let visible: Vec<_> = crate::tiles::wanted_tiles(
                &[placement],
                (canvas.x0, canvas.y0, canvas.x1, canvas.y1),
            )
            .into_iter()
            .filter(|t| t.visible)
            .map(|t| (t.col, t.row))
            .collect();
            assert!(
                app.raster_tile_batch
                    .iter()
                    .all(|queued| !visible.contains(&(queued.col, queued.row)))
            );
        }

        // A tile's pixels are the same region of the page rendered whole at the
        // same scale (the recorded rotation applied in both).
        eprintln!("phase a_tile_matches_the_same_region_of_a_whole_page_render");
        'phase: {
            let loaded =
                crate::source::open(std::path::Path::new(LATIN_PDF), &ContentStore::memory());
            match pdfium_gate(loaded.is_ok(), require_pdfium()) {
                PdfiumGate::Run => {}
                PdfiumGate::Fail => panic!("TILO_REQUIRE_PDFIUM is set and there is {NO_PDFIUM}"),
                PdfiumGate::Skip => {
                    eprintln!("SKIPPING the tile-versus-page render check: {NO_PDFIUM}");
                    break 'phase;
                }
            }
            let mut rz = loaded.expect("the fixture opens").raster;
            let plan: Vec<_> = (1..=rz.page_count())
                .map(|page| crate::markup::PagePlanEntry {
                    id: format!("source-{page}"),
                    source_page: page,
                    imported: None,
                    rotation: if page == 1 { 90 } else { 0 },
                })
                .collect();
            assert!(rz.reconcile_page_plan(&plan));
            let level = 8; // 4 px per point.
            let tile = rz.render_tile(0, level, 1, 1).expect("tile");
            let engine = tilo_pdf::Engine::shared().expect("engine");
            let doc = engine
                .open_path(std::path::Path::new(LATIN_PDF))
                .expect("open fixture");
            let whole = doc
                .render(
                    0,
                    &tilo_pdf::RenderOptions {
                        scale: crate::tiles::level_scale(level),
                        rotation: tilo_pdf::Rotation::from_degrees(90).unwrap(),
                        ..tilo_pdf::RenderOptions::default()
                    },
                )
                .expect("whole page");
            let stride = whole.width() as usize * 4;
            let pixels = tile.data.data();
            let mut diff = 0u64;
            for y in 0..tile.height as usize {
                for x in 0..tile.width as usize * 4 {
                    let a = pixels[y * tile.width as usize * 4 + x];
                    let b = whole.pixels()[(512 + y) * stride + 512 * 4 + x];
                    diff += u64::from(a.abs_diff(b));
                }
            }
            let mean = diff as f64 / f64::from(tile.width * tile.height * 4);
            assert!(mean < 1.0, "tile differs from the page region by {mean}");
            // The tile is not blank paper.
            assert!(pixels.chunks_exact(4).any(|px| px[0] < 128));
        }
    }

    #[serial(clipboard)]
    #[serial(pdfium)]
    #[test]
    fn raster_text_layer_selects_copies_and_searches_real_glyphs() {
        // End-to-end text selection over the real pdfium glyph layer: press on a
        // glyph, drag across a run, copy. Drives the same `begin_selection` /
        // `update_selection` / `copy_selection` path the pointer handlers use, so a
        // regression in on-demand text-layer extraction or in the selection→clipboard
        // wiring fails here. Serialised on the clipboard so it never races the other
        // arboard tests.
        eprintln!("phase text_selection_drag_copies_the_real_run");
        'phase: {
            let Some(mut app) = open_raster_sample() else {
                break 'phase;
            };
            let mut st = ui::State::new();

            // A press before the page's text layer is here asks the worker
            // for it rather than extracting it on the UI thread.
            assert!(
                app.raster_text.is_empty(),
                "no eager text layer before a selection"
            );
            assert!(
                !app.begin_selection(1, (100.0, 100.0), Point::new(100.0, 100.0)),
                "nothing to select until the layer arrives"
            );
            assert!(
                app.raster_text_pending.contains(&0),
                "the press asked for it"
            );

            // Two glyphs with real boxes on page 1, ordered left-to-right on a line,
            // give a non-empty run to sweep between.
            let layer = text_layer(&mut app, &mut st, 1);
            let mut printable: Vec<_> = layer
                .chars
                .iter()
                .filter(|c| c.rect.w > 1.0 && c.rect.h > 1.0)
                .copied()
                .collect();
            assert!(printable.len() >= 4, "page 1 has a run of printable glyphs");
            printable.sort_by(|a, b| {
                (a.rect.y, a.rect.x)
                    .partial_cmp(&(b.rect.y, b.rect.x))
                    .unwrap()
            });
            let start = printable[0].rect.center();
            let end = printable[printable.len().min(6) - 1].rect.center();

            assert!(
                app.begin_selection(1, start, Point::new(start.0, start.1)),
                "pressing on a glyph starts a selection"
            );
            app.update_selection(1, end);
            let sel = app.selection.expect("a selection is armed");
            assert!(
                sel.is_active(),
                "the drag grew the selection past its anchor"
            );

            let text = app.selection_text().expect("the selected run yields text");
            assert!(
                !text.trim().is_empty(),
                "the selected run is real page text: {text:?}"
            );
            // The copy path reports success and stamps the toast.
            assert!(
                app.copy_selection(),
                "copy_selection succeeds on a live selection"
            );
            assert!(
                app.status_note.as_deref().unwrap_or("").contains("Copied"),
                "a copy toast is shown ({:?})",
                app.status_note
            );
        }

        eprintln!("phase raster_text_layer_hit_tests_real_glyphs_and_search_finds_them");
        'phase: {
            let Some(mut app) = open_raster_sample() else {
                break 'phase;
            };
            let mut st = ui::State::new();

            // The text layer comes straight from pdfium's glyph geometry.
            let layer = text_layer(&mut app, &mut st, 1);
            assert!(!layer.is_empty(), "page 1 exposes real extracted glyphs");

            // Hit-testing a glyph's own centre returns a glyph whose box contains
            // that point — proving the selectable layer is aligned to the render.
            let probe = layer
                .chars
                .iter()
                .find(|c| c.rect.w > 1.0 && c.rect.h > 1.0)
                .copied()
                .expect("a printable glyph with a real box");
            let (cx, cy) = probe.rect.center();
            let hit = layer.char_at(cx, cy).expect("hit-test lands on a glyph");
            assert!(
                layer.chars[hit].rect.contains(cx, cy),
                "the hit glyph's box contains the probed point"
            );

            // A real word from the page must be found by search, and the hit must
            // resolve back to on-page bands that read as that word.
            let word = first_word(&layer.text, 4).expect("a searchable word on page 1");
            app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            app.recompute_regions();
            app.sync_viewport_frame();
            app.open_search();
            app.find_bar.set_query(&word);
            app.recompute_matches();
            settle_until(&mut app, &mut st, |app| app.raster_text_pending.is_empty());
            assert!(
                !app.raster_matches.is_empty(),
                "search finds the real word {word:?} through the pdfium text layer"
            );
            let m = app.raster_matches[0];
            let hit_layer = text_layer(&mut app, &mut st, m.page);
            let got = hit_layer.text_for_range(m.char_start, m.char_start + m.char_len);
            assert_eq!(
                got.to_lowercase(),
                word.to_lowercase(),
                "the hit is the searched word"
            );
            let bands = hit_layer.bands_for_range(m.char_start, m.char_start + m.char_len);
            assert!(
                !bands.is_empty(),
                "the hit yields at least one on-page band"
            );
            let pw = app.pw();
            for b in &bands {
                assert!(
                    b.x >= -1.0 && b.x + b.w <= pw + 1.0,
                    "band {b:?} sits within the page width {pw}"
                );
                assert!(b.w > 0.0 && b.h > 0.0, "band {b:?} is non-degenerate");
            }
        }
    }

    /// The first run of `min_len`+ alphabetic characters in `text`.
    fn first_word(text: &str, min_len: usize) -> Option<String> {
        let mut cur = String::new();
        for ch in text.chars() {
            if ch.is_alphabetic() {
                cur.push(ch);
            } else {
                if cur.chars().count() >= min_len {
                    return Some(cur);
                }
                cur.clear();
            }
        }
        (cur.chars().count() >= min_len).then_some(cur)
    }

    #[serial(pdfium)]
    #[test]
    fn markup_and_page_edits_persist_across_save_and_reopen() {
        eprintln!("phase save_back_writes_markup_as_real_pdf_annotations");
        'phase: {
            let Some((mut app, mut st)) = open_laid_out_sample() else {
                break 'phase;
            };
            let before =
                crate::source::open(std::path::Path::new(SAMPLE_PDF), &ContentStore::memory())
                    .expect("the sample opens")
                    .raster
                    .page_annotations(0)
                    .len();

            // Place a highlight, a sticky note with a body, and a signature on page
            // 1 — all through the reader's real mutation methods.
            app.tool = Tool::Highlight;
            app.drag = Some(DragState {
                tool: Tool::Highlight,
                page: 1,
                start: (70.0, 150.0),
                cur: (360.0, 165.0),
                points: vec![],
            });
            app.commit_drag(&mut Edits::lend(&mut st.doc));
            app.create_note(&mut Edits::lend(&mut st.doc), 1, (120.0, 300.0));
            app.editing.as_mut().unwrap().set_text("Check this claim");
            app.commit_edit(&mut Edits::lend(&mut st.doc));
            app.signer_name = "Grace Hopper".into();
            app.place_signature(&mut Edits::lend(&mut st.doc), 1, (360.0, 520.0));
            assert!(
                app.annotations
                    .iter()
                    .any(|a| matches!(a.kind, AnnoKind::Markup { .. }))
            );
            assert!(app.signatures.iter().any(|s| s.signer == "Grace Hopper"));

            let out = std::env::temp_dir()
                .join("tilo_pdf_saveback")
                .join("annotated.pdf");
            std::fs::create_dir_all(out.parent().unwrap()).unwrap();
            let _ = std::fs::remove_file(&out);
            assert!(
                save_settled(&mut app, &out),
                "the save reports success (note: {:?})",
                app.status_note
            );
            assert!(out.exists(), "the annotated .pdf exists on disk");

            // Reopen the saved file: page 1 carries the markup as standard PDF
            // annotations (a highlight, a note holding the comment text, and a
            // stamp for the signature) that any viewer reads back.
            let reopened =
                crate::source::open(&out, &ContentStore::memory()).expect("the saved PDF reopens");
            let annotations = reopened.raster.page_annotations(0);
            assert!(
                annotations.len() >= before + 3,
                "markup became annotations ({before} -> {})",
                annotations.len()
            );
            assert!(
                annotations
                    .iter()
                    .any(|a| a.kind == tilo_pdf::AnnotationKind::Highlight)
            );
            assert!(annotations.iter().any(|a| {
                a.kind == tilo_pdf::AnnotationKind::Text && a.contents.contains("Check this claim")
            }));
            assert!(annotations.iter().any(|a| {
                a.kind == tilo_pdf::AnnotationKind::Stamp && a.contents.contains("Grace Hopper")
            }));
        }

        eprintln!("phase reorder_persists_across_save_and_reopen");
        'phase: {
            let Some((mut app, mut st)) = open_laid_out_sample() else {
                break 'phase;
            };
            let n = app.doc.page_count();
            assert!(
                n >= 2,
                "the fixture must have at least two pages to test reordering"
            );
            let first = text_layer(&mut app, &mut st, 1).text.trim().to_string();
            let last = text_layer(&mut app, &mut st, n).text.trim().to_string();
            assert_ne!(
                first, last,
                "the two pages carry distinct text so a swap is observable"
            );

            // Move the first page to the end, as the shared rail's Reorder does.
            let ids = app.resource_page_ids.clone();
            edit_pages(
                &mut app,
                &mut st,
                crate::actions::move_page_plan_action(&ids[0], ids.last().map(String::as_str)),
            );
            assert!(app.pages_dirty(), "a reorder flags that a save is needed");
            assert_eq!(app.doc.page_count(), n, "reorder keeps the page count");

            let mut rz = saved_reopen(&mut app, "reordered.pdf");
            assert_eq!(rz.page_count(), n, "the page count persisted");
            assert_eq!(
                rz.page_text(n - 1).text.trim(),
                first,
                "the old first page is now last in the saved-and-reopened file"
            );
        }

        eprintln!("phase delete_page_persists_across_save_and_reopen");
        'phase: {
            let Some((mut app, mut st)) = open_laid_out_sample() else {
                break 'phase;
            };
            let n = app.doc.page_count();
            assert!(n >= 2);
            let second = text_layer(&mut app, &mut st, 2).text.trim().to_string();

            let first_id = app.resource_page_ids[0].clone();
            edit_pages(
                &mut app,
                &mut st,
                crate::actions::delete_page_plan_action(&first_id),
            );
            assert_eq!(app.doc.page_count(), n - 1, "delete drops one page");
            assert!(app.pages_dirty());

            let mut rz = saved_reopen(&mut app, "deleted.pdf");
            assert_eq!(rz.page_count(), n - 1, "the deletion persisted");
            assert_eq!(
                rz.page_text(0).text.trim(),
                second,
                "the former second page is now first"
            );
        }

        eprintln!("phase duplicate_page_persists_across_save_and_reopen");
        'phase: {
            let Some((mut app, mut st)) = open_laid_out_sample() else {
                break 'phase;
            };
            let n = app.doc.page_count();
            let first = text_layer(&mut app, &mut st, 1).text.trim().to_string();

            let entry = PdfMarkup::of(&st.doc).page_plan()[0].clone();
            edit_pages(
                &mut app,
                &mut st,
                crate::actions::duplicate_page_plan_action(&entry),
            );
            assert_eq!(app.doc.page_count(), n + 1, "duplicate adds one page");

            let mut rz = saved_reopen(&mut app, "duplicated.pdf");
            assert_eq!(rz.page_count(), n + 1, "the duplicate persisted");
            assert_eq!(rz.page_text(0).text.trim(), first);
            assert_eq!(
                rz.page_text(1).text.trim(),
                first,
                "page 2 is a copy of page 1"
            );
        }
    }

    /// Prepare an opened raster sample for page-editing tests: its record
    /// seeded and laid out so `current_page` and region rects are valid.
    fn open_laid_out_sample() -> Option<(PdfApp, ui::State)> {
        let mut app = open_raster_sample()?;
        let mut st = ui::State::new();
        settle_record(&mut app, &mut st);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        Some((app, st))
    }

    /// Edit the record's page plan with `invoke` and wait for the worker to
    /// materialise it.
    fn edit_pages(
        app: &mut PdfApp,
        st: &mut ui::State,
        invoke: ui::tilo_resource::Invoke<PdfMarkup>,
    ) {
        app.perform_action(&mut Edits::lend(&mut st.doc), invoke);
        settle_until(app, st, |app| app.pending_worker_plan.is_none());
    }

    fn saved_reopen(app: &mut PdfApp, name: &str) -> crate::source::RasterBackend {
        let out = std::env::temp_dir().join("tilo_pdf_edit").join(name);
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&out);
        assert!(save_settled(app, &out), "save: {:?}", app.status_note);
        assert!(
            !app.pages_dirty(),
            "a successful save clears the page-dirty flag"
        );
        crate::source::open(&out, &ContentStore::memory())
            .expect("the edited PDF reopens")
            .raster
    }

    /// Wait for an accepted import to reach the record and the worker's
    /// page plan.
    fn settle_import(app: &mut PdfApp, st: &mut ui::State) {
        settle_until(app, st, |app| {
            app.pending_import.is_none() && app.pending_worker_plan.is_none()
        });
    }

    #[serial(pdfium)]
    #[test]
    fn imported_images_and_pdfs_add_pages_and_regions_crop_the_bitmap() {
        eprintln!("phase import_image_adds_a_page_that_survives_save");
        'phase: {
            let Some((mut app, mut st)) = open_laid_out_sample() else {
                break 'phase;
            };
            let n = app.doc.page_count();

            // A PNG on disk, imported through the normal code path.
            let img = image::RgbaImage::from_pixel(96, 64, image::Rgba([210, 60, 60, 255]));
            let png = std::env::temp_dir()
                .join("tilo_pdf_edit")
                .join("swatch.png");
            std::fs::create_dir_all(png.parent().unwrap()).unwrap();
            image::DynamicImage::ImageRgba8(img)
                .save(&png)
                .expect("write the test PNG");

            assert!(app.import_file(&png), "import: {:?}", app.status_note);
            settle_import(&mut app, &mut st);
            assert_eq!(app.doc.page_count(), n + 1, "the image import added a page");
            assert!(app.pages_dirty());

            let rz = saved_reopen(&mut app, "with_image.pdf");
            assert_eq!(rz.page_count(), n + 1, "the imported image page persisted");
        }

        eprintln!("phase import_pdf_appends_all_its_pages");
        'phase: {
            let Some((mut app, mut st)) = open_laid_out_sample() else {
                break 'phase;
            };
            let n = app.doc.page_count();
            // Import the sample into itself: page count should exactly double.
            assert!(
                app.import_file(std::path::Path::new(SAMPLE_PDF)),
                "import: {:?}",
                app.status_note
            );
            settle_import(&mut app, &mut st);
            assert_eq!(
                app.doc.page_count(),
                n * 2,
                "every page of the imported PDF was added"
            );

            let rz = saved_reopen(&mut app, "merged.pdf");
            assert_eq!(rz.page_count(), n * 2, "the merged page set persisted");
        }

        eprintln!("phase copy_image_region_crops_the_page_bitmap");
        'phase: {
            let Some((mut app, mut st)) = open_laid_out_sample() else {
                break 'phase;
            };
            let (pw, ph) = app.raster_document.as_ref().unwrap().page_sizes[0];

            // Marquee the upper-left portion of page 1 and copy it.
            app.region = Some(RegionSelect {
                page: 1,
                start: (40.0, 40.0),
                cur: (pw * 0.6, ph * 0.5),
            });
            // The page is rendered for the crop first; the copy follows once
            // the worker delivers it.
            assert!(!app.copy_region(), "the crop waits for its page render");
            let bucket = ((pw * 2.0).round() as u32).clamp(96, 4096);
            let key = (0, crate::source::RasterBackend::bucket_for(bucket));
            settle_until(&mut app, &mut st, |app| {
                app.raster_images.contains_key(&key)
            });
            assert!(
                app.copy_region(),
                "a usable region is cropped: {:?}",
                app.status_note
            );

            // The pure crop path yields a correctly-sized sub-image of the page.
            let img = app.raster_images[&key].clone();
            let rect = PtRect::from_corners(40.0, 40.0, pw * 0.6, ph * 0.5);
            let (w, h, rgba) = render::crop_page_region(&img, pw, ph, rect).expect("crop succeeds");
            assert!(w > 0 && h > 0);
            assert_eq!(
                rgba.len(),
                w * h * 4,
                "RGBA buffer matches the cropped dimensions"
            );
            assert!(
                w < img.width as usize && h < img.height as usize,
                "the crop is a proper sub-region of the full page bitmap"
            );
        }
    }

    #[test]
    fn comment_text_starts_on_the_author_name_edge_at_any_scale() {
        // The rich text of a comment sets its first glyph where the author's
        // name starts, at every text scale.
        for scale in [1.0_f32, 2.0] {
            let mut app = PdfApp::sample();
            app.layout_theme = Theme::dark().with_text_scale(scale);
            let theme = app.layout_theme.clone();
            let mut tr = ui::text_render::TextRenderer::new();
            let card_w = f64::from(theme.tokens.metrics.inspector_width)
                - 2.0 * f64::from(theme.tokens.spacing.s3);
            let body =
                CardFace::new().body_rect(&mut tr, &theme, Rect::new(0.0, 0.0, card_w, 400.0));
            let hdr = PdfApp::comment_header_layout(
                body,
                PdfApp::resolve_button_w(&mut tr, &theme),
                &theme,
            );
            let rect = app.rich_text_rect(&mut tr, hdr.text_x, hdr.body_top, 200.0);
            let mut view = RichTextView::new();
            view.show(&[ui::widget::BlockContent::paragraph(
                "a",
                "Should we spell it out?",
            )]);
            view.layout(rect, &theme, &mut tr);
            let caret = view
                .editor_mut()
                .blocks()
                .first()
                .map(|b| b.line.caret_rect(&theme, 0).x0)
                .unwrap();
            assert!(
                (caret - hdr.text_x).abs() < 0.5,
                "body text at {caret} starts on the name edge {} at {scale}×",
                hdr.text_x
            );
        }
    }

    #[test]
    fn comment_cards_centre_wrap_and_never_overlap_at_any_scale() {
        // The comment card's header never overlaps: the author name is bounded
        // and elides before the Resolve and delete buttons, whatever its
        // length, at every text scale, and every piece shares one centre line.
        eprintln!("phase comment_header_never_overlaps_even_with_a_long_name");
        let long = "quinten.van.der.berg.reviewer@really-long-domain.example.com";
        for scale in [1.0_f32, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut tr = ui::text_render::TextRenderer::new();
            let card_w = f64::from(theme.tokens.metrics.inspector_width)
                - 2.0 * f64::from(theme.tokens.spacing.s3);
            let card = Rect::new(0.0, 0.0, card_w, 400.0);
            let body = CardFace::new().body_rect(&mut tr, &theme, card);
            let resolve_w = PdfApp::resolve_button_w(&mut tr, &theme);
            let hdr = PdfApp::comment_header_layout(body, resolve_w, &theme);
            assert!(
                hdr.avatar.x1 <= hdr.name.x0,
                "avatar before the name at {scale}×"
            );
            assert!(
                hdr.name.x1 <= hdr.resolve.x0,
                "name before Resolve at {scale}×"
            );
            assert!(
                hdr.resolve.x1 <= hdr.delete.x0,
                "Resolve before delete at {scale}×"
            );
            assert!(
                hdr.delete.x1 <= body.x1 + 0.01,
                "delete inside the card at {scale}×"
            );
            for r in [hdr.avatar, hdr.resolve, hdr.delete] {
                assert!(
                    (r.center().y - hdr.name.center().y).abs() < 0.5,
                    "{r:?} shares the header's centre line at {scale}×"
                );
            }
            assert!(
                hdr.body_top >= hdr.delete.y1,
                "the body starts under the header"
            );
            let style = theme.text(TextRole::Label).strong();
            let shown = tr.elide_to_width_style(long, hdr.name.width() as f32, &style);
            let (w, _) = tr.measure_style(&shown, None, &style);
            assert!(
                f64::from(w) <= hdr.name.width() + 0.5,
                "the elided name ({shown:?}, {w}px) fits its {}px at {scale}×",
                hdr.name.width()
            );
            assert!(shown.chars().count() < long.chars().count());
        }

        // A measured card holds its header, body, replies and reply button:
        // the running sequence the draw pass follows ends inside the card.
        eprintln!("phase comment_card_measures_what_it_paints");
        {
            let app = crate::scenarios::sample_reader();
            let theme = ui::Theme::dark().with_text_scale(2.0);
            let mut tr = ui::text_render::TextRenderer::new();
            let cm = Comment {
                id: "high-text-thread".into(),
                anno_id: None,
                page: 1,
                author: "An exceptionally long review author name".into(),
                body: "A deliberately long comment body that wraps across the narrow review panel at two-times text size.".into(),
                created: 1,
                resolved: false,
                replies: vec![Reply {
                    id: "high-text-reply-one".into(),
                    author: "Bob".into(),
                    body: "A long reply that also wraps, proving the thread is allotted its own measured space.".into(),
                    blocks: tilo_rich_text::plain_blocks("A long reply that also wraps, proving the thread is allotted its own measured space."),
                }],
                blocks: tilo_rich_text::plain_blocks("A deliberately long comment body that wraps across the narrow review panel at two-times text size."),
            };
            let card_w = 276.0;
            let m = app.comment_metrics(&mut tr, &theme, &cm, card_w);
            let card = Rect::new(0.0, 0.0, card_w, m.card_h);
            let body = CardFace::new().body_rect(&mut tr, &theme, card);
            let hdr = PdfApp::comment_header_layout(
                body,
                PdfApp::resolve_button_w(&mut tr, &theme),
                &theme,
            );
            let rhythm = &theme.tokens.spacing.rhythm;
            let label = f64::from(theme.text(TextRole::Label).line_height_px());
            let mut y = hdr.body_top + m.body_h;
            for h in &m.reply_heights {
                y += f64::from(rhythm.sibling) + label + f64::from(rhythm.tight) + h;
            }
            y += m.compose_h;
            assert!(
                y <= body.y1 + 0.5,
                "the reply button ({y}) stays inside the measured card body ({})",
                body.y1
            );
        }
    }
}

impl ui::widget::Findable for PdfApp {
    fn find(
        &mut self,
        query: &str,
        options: ui::widget::FindOptions,
    ) -> Vec<ui::widget::FindMatch> {
        self.search_case_sensitive = options.match_case;
        self.search_whole_word = options.whole_word;
        // Finding again over grown text keeps the emphasised match; a new
        // query reveals its first one.
        let current = self.active_match;
        self.recompute_matches_for(query);
        if current < self.match_count() {
            self.active_match = current;
        }
        (0..self.match_count())
            .map(|i| ui::widget::FindMatch::new(i, i + 1))
            .collect()
    }

    fn reveal(&mut self, index: usize) {
        self.active_match = index;
        let page = if self.raster_active() {
            self.raster_matches.get(index).map(|m| m.page)
        } else {
            self.matches.get(index).map(|m| m.page)
        };
        if let Some(page) = page {
            self.go_to_page(page);
        }
        self.dirty = true;
    }

    fn clear_find(&mut self) {
        self.matches.clear();
        self.raster_matches.clear();
        self.active_match = 0;
        self.dirty = true;
    }

    /// The searchable text grows as page text layers arrive from the worker;
    /// the bar finds again, so its counter follows.
    fn content_revision(&self) -> u64 {
        let document = self.raster_document.as_ref().map_or(0, |doc| doc.id);
        let pages = u64::try_from(self.raster_text.len()).unwrap_or(u64::MAX);
        document.rotate_left(32) ^ pages
    }
}
