//! `PdfApp` — the root widget for the PDF reader.
//!
//! It is a single full-window widget that manages its own internal layout
//! (thumbnail rail, page canvas, comments/outline/signatures sidebar) and
//! draws each region itself; its controls are slots in the window's bar
//! (`window_bar`) and find is the window bar's search. This
//! gives pixel control over the reader chrome, mirroring the custom-draw
//! approach the toolkit uses for things like the status bar, while keeping
//! all durable markup — annotations, comments, signatures — in the shared
//! CRDT so collaboration and undo come for free.

use std::collections::{HashMap, HashSet};
use ui::chrome::{NavCount, NavRowFace};
use ui::icons::Icon;
use ui::tokens::TextRole;
use ui::widget::DrawChild;
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

use crate::actions::PdfEffect;
use std::path::Path;
use ui::Widget;
use ui::gesture::{DragSelect, Granularity};
use ui::paint::PaintCtx;
use ui::portal::{
    ContentsChannel, ContentsOutcome, DestinationOutcome, FileFilter, PickRequest, PortalChannel,
    PortalOutcome,
};

use ui::chrome::SelectionState;
use ui::presence::PresenceCtx;
use ui::surface::Region;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::page_sidebar::{PageSidebar, PageSidebarEvent};
use ui::widget::toast::{ToastKind, Transient};
use ui::widget::{
    AvatarFace, AvatarSize, Button, ButtonFace, ButtonRole, CardFace,
    ContextMenu as SharedContextMenu, ControlSize, DEFAULT_PADDING,
    DEFAULT_PAN_STEP as KEY_SCROLL_STEP, EmptyState, EmptyStateContent, ListRowFace, MenuItem,
    PAN_SLOP, RowAction, RowLeading, Segment, SegmentedFace, SegmentedStyle, SmallContentAlign,
    TextInput, ToggleGroup, ViewportState,
};
use ui::window_toolbar::ToolbarNotice;

use crate::markup::{PagePlanEntry, PdfMarkup};
use crate::model::{
    self, AnnoKind, Annotation, Document, HighlightColor, MARGIN, PAGE_H, PAGE_W, PtRect,
    SearchOptions, Signature, SignatureInk, Thread, Tool, new_id, reply_blocks,
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
pub(crate) const ZOOM_MIN: f64 = 0.25;
/// The most pages one text read answers with.
pub(crate) const MOST_READ_PAGES: usize = 50;
/// Zoomed pages are drawn from bounded tiles ([`crate::tiles`]), so the
/// ceiling is about usefulness, not bitmap size.
pub(crate) const ZOOM_MAX: f64 = 16.0;
/// Rasterise pages at this multiple of their on-screen size so they stay
/// crisp when the compositor scales the surface (and when zooming in a notch).
const RASTER_SS: f64 = tilo_page_raster::SUPERSAMPLE;
/// Max number of *new* (uncached) full-page renders a single frame may ask
/// the worker for. A real page can cost tens-to-hundreds of ms, so capping
/// this keeps the open→first-paint path to the single top page; the rest are
/// asked for as results wake later frames. Cached pages are always drawn (a
/// cache hit is ~free), so steady-state scrolling is unaffected.
const MAX_NEW_PAGE_RASTERS_PER_FRAME: usize = tilo_page_raster::Admission::FRAME.pages;

/// One frame's page imagery, keyed by 1-based page: whole-page bitmaps
/// (placeholders under tiles for zoomed pages), rail thumbnails, and the
/// tiles of zoomed pages.
type RasterFrame = (
    HashMap<usize, PenikoImage>,
    HashMap<usize, PenikoImage>,
    HashMap<usize, render::PageTiles>,
);

/// A tile the UI holds: displayed page (0-based), zoom level, column, row.
pub(crate) type UiTileKey = crate::tiles::TileKey;

#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchProjectionRevision {
    doc_id: String,
    heads: Vec<ChangeHash>,
    page_ids: Vec<String>,
    page_sources: Vec<usize>,
    search_token: String,
    search_pages: Vec<usize>,
    page_text_revision: u64,
}

/// A screenshot scenario waiting to be staged (`None` names the plain
/// capture).
#[cfg(feature = "samples")]
#[derive(Clone)]
struct StagedScenario {
    name: Option<String>,
}

fn pdf_filter() -> FileFilter {
    FileFilter::new(&crate::t("pdf-chooser-filter-pdf")).mimetype("application/pdf")
}

fn pdf_open_request() -> PickRequest {
    PickRequest::open(
        &crate::t("pdf-chooser-open"),
        &crate::t("pdf-chooser-open-accept"),
    )
    .thread_name("pdf-portal-open")
    .filter(pdf_filter())
    .all_files()
}

fn pdf_import_request() -> PickRequest {
    PickRequest::open(
        &crate::t("pdf-chooser-import"),
        &crate::t("pdf-chooser-import-accept"),
    )
    .thread_name("pdf-portal-import")
    .filter(
        FileFilter::new(&crate::t("pdf-chooser-filter-import"))
            .mimetype("application/pdf")
            .mimetype("image/png")
            .mimetype("image/jpeg"),
    )
    .all_files()
}

fn pdf_save_request(suggested_name: &str) -> PickRequest {
    PickRequest::save(
        &crate::t("pdf-bar-save"),
        &crate::t("pdf-chooser-save-accept"),
        suggested_name,
    )
    .thread_name("pdf-portal-save")
    .filter(pdf_filter())
}
/// Same budget for rail thumbnails, which are also full engine page renders (a
/// scanned page's thumbnail can cost 15-30ms). A couple per frame keeps the
/// rail filling in briskly without the open frame paying for a whole rail-full.
const MAX_NEW_THUMB_RASTERS_PER_FRAME: usize = tilo_page_raster::Admission::FRAME.thumbnails;

/// The reader's app id, the scope its permissions are decided in.
#[cfg(test)]
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

impl SidebarTab {
    /// The catalogue message naming the tab, painted on it and spoken for it.
    fn name_key(self) -> &'static str {
        match self {
            Self::Comments => "pdf-tab-comments",
            Self::Outline => "pdf-tab-contents",
            Self::Signatures => "pdf-tab-signing",
        }
    }

    /// What an action target calls the tab.
    fn tag(self) -> &'static str {
        match self {
            Self::Comments => "comments",
            Self::Outline => "contents",
            Self::Signatures => "signatures",
        }
    }
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
        widget.push(
            MenuItem::destructive_selection(crate::t("pdf-delete"))
                .with_icon(ui::icons::Icon::Trash),
        );
        widget.open_anchored(anchor, viewport);
        Self { target, widget }
    }
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
    /// The window bar's entries, each carrying the effect it runs.
    bar: ui::framework::BarCommands<PdfEffect>,
    /// The session-scoped endpoint for production `PDFium` work.  The backend is
    /// deliberately owned by its worker; this widget receives only ordinary
    /// documents, bitmaps and text layers.
    raster_handle: Option<RasterHandle>,
    raster_document: Option<RasterDocument>,
    pending_open: Option<OpenRequest>,
    /// Bounded in-flight/cached UI-safe worker results.  Keys are the
    /// zero-based display page and the worker's snapped width bucket.
    raster_images: crate::page_images::PageImages,
    raster_render_pending: HashSet<(usize, u32)>,
    /// Tiles of zoomed pages the worker has returned, per zoom level.
    raster_tiles: crate::tiles::TileCache<UiTileKey>,
    /// The tile batch last handed to the worker, most important first;
    /// entries leave it as their results arrive.
    raster_tile_batch: Vec<crate::source::TileRequest>,
    /// The page renders last handed to the worker, most important first.
    raster_render_batch: Vec<(usize, u32)>,
    /// The direction the scroll last moved in: prefetch reaches ahead.
    raster_heading: tilo_page_raster::reach::Heading,
    /// Whether the worker wakes this window when its results arrive.
    raster_wake_bound: bool,
    /// The view scrolled since the last presented frame, and nothing else
    /// asked for a repaint: what stays on screen is copied, not redrawn.
    scrolled: bool,
    /// Pages (0-based) whose bitmap or tiles arrived since the last
    /// presented frame, and whether a rail thumbnail did: repainted in
    /// place.
    delivered_pages: Vec<usize>,
    delivered_thumbs: bool,
    /// How the canvas was shown by the frame being drawn and by the frame
    /// last presented.
    shown: ui::scroll_copy::ShownFrames<CanvasShown>,
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
    /// The insert-pages effect waiting on [`Self::pending_import`].
    asked_import: Option<insert_pages::AskedImport>,
    /// False until a file has been loaded — drives the empty state.
    has_document: bool,
    /// The PDF the record's markup is made against, as last presented.
    record_source: Option<crate::markup::PinnedSource>,
    /// The record's pinned source was read from the content store and
    /// handed to the PDF owner.
    reopen_requested: bool,
    /// The record's pinned source is not on this device: where its fetch
    /// stands, and the content arrivals count it was last looked for at
    /// ([`ui::collaboration::record_content::arrivals`]). It is looked for
    /// again when that moves, never on a timer.
    source_wait: Option<(tilo_content::ContentState, u64)>,
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
    /// Screenshot-only: defer opening the annotation menu until after the
    /// initial CRDT sync has populated its target.
    shots_context_menu: bool,
    shots_panel: Option<SidebarTab>,
    /// The screenshot scenario waiting for its document to open and be read
    /// before its view state is staged.
    #[cfg(feature = "samples")]
    staged_scenario: Option<StagedScenario>,
    // CRDT-backed caches (refreshed from the shared document each frame).
    annotations: Vec<Annotation>,
    comments: Vec<Thread>,
    signatures: Vec<Signature>,

    // View state — local / per-peer, never in the shared doc.
    rect: Rect,
    /// The user's fit *intent* (page / width / a pinned custom zoom). The
    /// actual zoom + scroll live in [`self.viewport`]; `fit` only records what
    /// to re-derive when the window resizes and checks Fit in the window bar.
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
    /// The resume hint read from the record when this document was opened,
    /// taken up once the canvas has a size.
    pending_reading: Option<(String, f64, f64)>,
    /// Whether this window has already taken up the record's reading
    /// position. A peer moving later does not pull this window away.
    position_restored: bool,
    /// What this window last recorded, so a still page does not write on
    /// every frame.
    last_reading: Option<(String, f64, f64)>,
    /// Durable CRDT page-plan identities, refreshed from the markup record.
    resource_page_ids: Vec<String>,
    /// Immutable source page backing each durable plan occurrence.
    resource_page_sources: Vec<usize>,
    /// Imported content metadata parallel to the durable plan; used to build
    /// duplicate actions that retain the imported source identity.
    resource_page_imports: Vec<Option<crate::markup::ImportedPage>>,
    /// How many pages the opened document itself has, before any page plan.
    source_page_count: usize,
    /// The text of each page the engine has read, for the system's search,
    /// by the page it is of ([`Self::page_text_key`]), so every occurrence
    /// of a page in the plan finds it. Kept apart from the text layers,
    /// which are dropped as the reader moves on, so what was read stays
    /// findable.
    page_text: HashMap<String, String>,
    /// Moves whenever `page_text` does, so the search entries are rebuilt.
    page_text_revision: u64,
    /// The system's search asked for this document's text: every page is
    /// read for it, and its results are pending until they are.
    search_reads_text: bool,
    /// What the open document says, for search: rebuilt during state sync
    /// when the document or the find results change, never on a keystroke.
    search_entries: std::rc::Rc<Vec<ui::tilo_resource::SearchEntry>>,
    search_projection_revision: Option<SearchProjectionRevision>,
    #[cfg(test)]
    search_projection_refreshes: usize,
    tool: Tool,
    swatch: HighlightColor,
    /// How the highlight tool marks text: highlight, underline or strike-out.
    markup_style: model::MarkupStyle,
    sidebar_tab: SidebarTab,
    show_sidebar: bool,
    /// The review panel's place beside the pages: the shared trailing
    /// inspector, so it follows the theme, shrinks before the pages fall
    /// under their minimum, and a person can drag its edge to resize it.
    /// `show_sidebar` stays the open state; the drawer lays it out.
    review: ui::widget::SidebarDrawer,
    /// Thumbnail rail: independently resizable + collapsible, persisted in
    /// view state so it survives across the session.
    show_rail: bool,
    rail_w: f64,
    /// The shared page-preview rail (thumbnails, drag-reorder, context menu,
    /// import). Lives on the left; toggled by the window bar's sidebar toggle.
    page_sidebar: PageSidebar,
    /// Set when the page order / page set has been edited (reorder, delete,
    /// duplicate, import) since the last save — the reader shows a save hint.
    pages_dirty: bool,
    /// An in-flight XDG-portal *import* request (add a PDF/image as pages).
    portal_import_rx: Option<ContentsChannel>,
    selected_anno: Option<String>,
    /// Active inline text edit (comment body, thread reply, or signer name).
    editing: Option<Editing>,
    /// Comments and replies as written, one read-only view each, keyed by
    /// [`comment_view_key`]; measured and drawn by the Comments rail.
    comment_views: std::cell::RefCell<HashMap<String, RichTextView>>,
    /// The rich-text view's line inset, keyed by text and UI scale.
    rich_inset: std::cell::Cell<Option<((f32, f32), f64)>>,
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
    /// What the window bar's search last asked to find in the document;
    /// empty while it finds nothing. The matches below are derived from it.
    find_query: String,
    /// A find an action ran (it went to a search result) that the window's
    /// search has not yet taken over, so its field shows the query.
    own_find: Option<ui::widget::OwnFind>,
    active_match: usize,
    search_whole_word: bool,
    search_case_sensitive: bool,
    /// Search hits over the engine text layer, so search highlights land on
    /// glyph rectangles.
    raster_matches: Vec<TextMatch>,

    // Real PDF text layer — glyph geometry per visible page, refreshed each
    // draw, powering selection + search-hit rectangles. Empty when no PDF is
    // open.
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
    /// The text range `[lo, hi)` of the unit the drag anchored on (the
    /// double-clicked word / triple-clicked line). The drag keeps this unit
    /// inside the selection and grows by whole units (see [`ui::extend_span_drag`]).
    sel_anchor_range: (TextPos, TextPos),
    /// A press on page text that waits for the page's text layer.
    pending_press: Option<PendingPress>,
    /// A copy that waits for the text of pages the selection spans.
    pending_copy: bool,
    /// A selection effect that waits for the text of the pages it reads.
    pending_selection_effect: Option<PendingSelectionEffect>,
    /// The page under the pointer, whose text is read ahead of a press.
    text_hover: Option<usize>,
    /// The text layers last asked ahead of need (0-based pages).
    raster_text_batch: Vec<usize>,
    /// The frame clock of a selection drag's edge autoscroll.
    selection_autoscroll: ui::edge_autoscroll::EdgeAutoscroll,
    /// Where on the page canvas the last frame drew something that shows
    /// the pointer's hover (see `pointer_move_is_quiet`).
    canvas_hover_zones: Vec<Rect>,
    /// The glide a lifted touchpad or touchscreen gesture leaves the page in.
    glide: ui::ScrollMomentum,
    /// How the window is being used (touch or pointer) and its target size.
    interaction: ui::interaction::InteractionEnvironment,
    /// A live image-region marquee (Shift + Select drag), if any.
    region: Option<RegionSelect>,
    /// A released marquee whose crop waits for its page's render, with the
    /// document it was drawn on: copied when the render arrives.
    region_waiting: Option<(u64, RegionSelect)>,
    /// Whether Shift is currently held (drives the region-copy marquee).
    shift_down: bool,

    // Save-back.
    /// The on-disk path the current document was opened from, used to suggest a
    /// filename and folder when saving flattened markup.
    source_path: Option<std::path::PathBuf>,
    /// A transient one-line status (e.g. "Saved to …"), shown as a toast for a
    /// notice's lifetime or until the next document action.
    status_note: Transient<String>,
    /// The failure of `status_note` whose Try again asks for its work again.
    note_action: Option<(String, note_action::NoteAction)>,
    /// The last copied region, whose outcome can arrive after its note.
    region_copy: ui::CopyWatch,
    /// An in-flight XDG-portal *save* request (mirrors `portal_rx`).
    portal_save_rx: Option<ui::portal::DestinationChannel>,
    /// Where a save whose bytes the worker hands back goes (a browser page's
    /// Save dialog or download), while the worker exports.
    save_destination: Option<ui::portal::SaveDestination>,
    /// That save's write, with the name it is saved as.
    save_write_rx: Option<(String, ui::portal::SaveChannel)>,
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
    dirty: bool,
    /// Visible page/thumbnail cache misses in the last production draw.
    pending_visuals: bool,

    /// Suppresses timed fades/reveal animation while preserving immediate
    /// state changes for users who request reduced motion.
    reduce_motion: bool,
    /// The theme of the last layout pass. The pure layout helpers shared by
    /// paint, hit-testing and the accessibility tree read their tokens from
    /// it, so geometry and paint agree.
    layout_theme: Theme,
    /// The review panel's tab widths as their labels need them, measured in
    /// the draw pass (`None` until then).
    tab_widths: Option<[f64; 3]>,
    /// The empty state's "Open a PDF…" pill as last painted (see
    /// `empty_open_button_rect`).
    empty_open_button: std::cell::Cell<Rect>,
    /// The workspace's documents the empty state lists, in the order it
    /// shows them: each opens in this window by its `target`.
    workspace_documents: Vec<WorkspaceDocument>,
    /// The rows those documents took in the last draw pass, so the press and
    /// the accessibility tree hit the painted rows (`workspace_document_at`).
    empty_document_rows: std::cell::RefCell<Vec<Rect>>,
    /// File drops on the window, and the empty state they light.
    file_open: ui::widget::FileOpenSurface,

    /// An in-flight XDG-portal file-open. When the user asks to open a
    /// document (the window bar's Open, Ctrl+O, or the empty state's Open
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
    /// A document of the workspace asked to open in this window's place, by
    /// the `target` a row of the empty state names, until the host answers.
    pending_document_open: Option<(String, ui::application::ReplacementOutcome)>,

    /// When the window is too narrow to reserve space for the side panels,
    /// the rail + sidebar become closed-by-default overlays drawn over the
    /// canvas. Recomputed from the window width each layout.
    overlay_mode: bool,
    /// Tracks overlay-mode transitions so panels can auto-close on the way in
    /// and the sidebar can reopen on the way back to a wide window.
    prev_overlay: Option<bool>,

    // Region rects (computed in apply_layout).
    rail_rect: Rect,
    canvas_rect: Rect,
    sidebar_rect: Rect,

    /// The page and the zoom, said in the window's bar for a while when one
    /// of them changes (not on opening), so the canvas keeps its whole
    /// height.
    page_notice: Transient<String>,
    /// The page and zoom percent last said, so only a change is news.
    page_said: Option<(usize, u32)>,

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
    /// Whether the participant may only view the document, as the view
    /// says; absent outside a host.
    read_only_state: Option<ui::LocalState<bool>>,
    /// The document as it changes; absent outside a host.
    live: Option<ui::framework::Live<crate::markup::PdfMarkup>>,
    /// The `(revision, loads)` of the document last taken up.
    presented: Option<(u64, u64)>,
    /// What the window shares with the view it stands in.
    store: std::rc::Rc<std::cell::RefCell<shared::ReaderStore>>,
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
    /// The open document's bytes, as this window's content store keeps them.
    source_blob: Option<ui::tilo_resource::Bytes>,
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

    /// Test-only: a reader of an open document of `pages` blank US-Letter
    /// pages, for the tests of pages, geometry and markup, which need no
    /// engine; tests of text open a real PDF through the worker.
    #[cfg(test)]
    pub(crate) fn blank_document(pages: usize) -> Self {
        let mut app = Self::build(
            model::Document::blank_pages("", "", pages, PAGE_W, PAGE_H),
            default_content(),
        );
        app.has_document = true;
        app.doc_name = crate::scenarios::SAMPLE_NAME.to_string();
        app
    }

    /// A past version of the record in History: its markup, without
    /// opening a document, so previewing versions starts no PDF engine.
    pub fn past() -> Self {
        let mut app = Self::empty_with_content(ContentStore::memory());
        app.raster_handle = None;
        app
    }

    /// Stage the view state screenshot `scenario` shows, once its actions
    /// have applied: the review its document lands on, and the editor,
    /// selection, menu or panel the scenario is about. A scenario that shows
    /// a document waits for it to open through the PDF worker and for its
    /// text to be read, so the frame is staged on the real pages.
    #[cfg(feature = "samples")]
    pub(crate) fn stage(&mut self, scenario: Option<&str>) {
        self.staged_scenario = Some(StagedScenario {
            name: scenario.map(str::to_owned),
        });
        self.stage_when_ready();
    }

    /// Stage the scenario waiting for its document once that has opened, its
    /// pages are laid out and their every text has been read; at once when it shows none, or
    /// its document could not be opened.
    #[cfg(feature = "samples")]
    fn stage_when_ready(&mut self) {
        let Some(StagedScenario { name: scenario }) = self.staged_scenario.clone() else {
            return;
        };
        if crate::scenarios::shows_sample(scenario.as_deref()) && self.load_error.is_none() {
            // Laying out the record's page plan replaces the text layers
            // (and a selection on them): stage on the settled layout.
            if !self.has_document
                || self.applied_worker_plan.is_none()
                || self.pending_worker_plan.is_some()
            {
                return;
            }
            let pages = self.doc.page_count();
            let unread: Vec<usize> = (1..=pages)
                .filter(|page| !self.raster_text.contains_key(page))
                .collect();
            if !unread.is_empty() {
                for page in unread {
                    self.ensure_raster_text(page);
                }
                return;
            }
        }
        self.staged_scenario = None;
        self.apply_stage(scenario.as_deref());
    }

    #[cfg(feature = "samples")]
    pub(crate) fn apply_stage(&mut self, scenario: Option<&str>) {
        if self.has_document {
            // The review lands on the note's comment thread; zoom stays at
            // fit-page so no content is clipped.
            self.selected_anno = Some(crate::scenarios::NOTE.to_string());
            self.sidebar_tab = SidebarTab::Comments;
            self.sidebar_reveal = Some(crate::scenarios::NOTE_COMMENT.to_string());
            self.signer_name = crate::scenarios::SIGNER.to_string();
        }
        match scenario {
            Some("edit-comment") => self.begin_edit(
                EditTarget::CommentBody(crate::scenarios::NOTE_COMMENT.to_string()),
                &crate::t(crate::scenarios::NOTE_COMMENT_BODY),
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
        // A capture of a document shows its review panel, floating over the
        // pages when the window is too narrow to dock it: staging runs after
        // the first frames, past the width class's own panel state.
        if self.has_document {
            self.show_sidebar = true;
        }
        self.dirty = true;
    }

    /// Whether a document is being opened: the loading state shows.
    fn opening(&self) -> bool {
        self.pending_open.is_some() || self.staged_opening
    }

    /// Screenshot-only: arm a live page-rail reorder drag so a still frame shows
    /// the drag ghost + reflow gap. The rail is forced open.
    #[cfg(feature = "samples")]
    pub fn arm_shots_rail_drag(&mut self) {
        self.shots_rail_drag = true;
        self.show_rail = true;
    }

    /// Screenshot-only: say the page and the zoom in the window's bar, as a
    /// page change does.
    #[cfg(feature = "samples")]
    pub fn arm_shots_page_bubble(&mut self) {
        self.page_notice.show(self.page_line());
    }

    /// Screenshot-only: open the annotation action menu over the sample page
    /// so shared floating-surface styling can be reviewed in the real app.
    #[cfg(feature = "samples")]
    pub fn arm_shots_context_menu(&mut self) {
        self.shots_context_menu = true;
    }

    /// Screenshot-only: find a word in the document and make its third
    /// match current, as the window bar's search does, so a still frame
    /// shows every match with the current one told apart.
    #[cfg(feature = "samples")]
    pub fn arm_shots_search(&mut self) {
        use ui::widget::Findable as _;
        let found = self.find("godwit", ui::widget::FindOptions::default());
        self.reveal(2.min(found.len().saturating_sub(1)));
    }

    /// Screenshot-only: open the Signatures panel in Draw mode with a signature
    /// already drawn on the pad and the Sign tool armed, so a still frame shows
    /// the real signature-composer workflow.
    #[cfg(feature = "samples")]
    pub fn arm_shots_sign_compose(&mut self) {
        self.show_sidebar = true;
        self.sidebar_tab = SidebarTab::Signatures;
        self.tool = Tool::Sign;
        self.sig_mode = SigMode::Draw;
        self.sig_strokes = crate::scenarios::sample_drawn_signature();
    }

    /// Screenshot-only: select a word of page 1's text, exactly as a
    /// double-click would — via the shared word range — so a still frame
    /// shows the shared selection HUD (highlight swatches + Copy + Note)
    /// over the page.
    #[cfg(feature = "samples")]
    pub fn arm_shots_selection(&mut self) {
        self.show_sidebar = true;
        let Some(layer) = self.raster_text.get(&1).cloned() else {
            return;
        };
        let Some(byte) = layer.text.find(crate::scenarios::SELECTED_WORD) else {
            return;
        };
        let index = layer
            .text
            .get(..byte)
            .map_or(0, |prefix| prefix.chars().count());
        let (s, e) = layer.word_range(index);
        self.selection = Some(TextSelection {
            anchor: TextPos::new(1, s),
            caret: TextPos::new(1, e),
            dragging: false,
        });
        self.sel_gran = Granularity::Word;
        self.sel_anchor_range = (TextPos::new(1, s), TextPos::new(1, e));
        self.dirty = true;
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
        let mut img = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let above = (x as f32 / w as f32) > (y as f32 / h as f32);
                img.extend_from_slice(if above {
                    &[64, 132, 246, 255]
                } else {
                    &[246, 132, 64, 255]
                });
            }
        }
        let Some(png) = tilo_paths::temp_dir().map(|dir| dir.join("tilo_pdf_demo_import.png"))
        else {
            return;
        };
        // Encoded by the shared runtime's PNG encoder: `DynamicImage::save`
        // is generic over the path and picks the format at run time, so it
        // compiled every encoder the image crate has into this app.
        let written = ui::png::encode_rgba8(w, h, &img).is_ok_and(|bytes| {
            tilo_files::file_at(&png)
                .and_then(|(dir, name)| tilo_files::blocking::write(&dir, &name, &bytes))
                .is_ok()
        });
        if written {
            self.import_file(&png);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "constructor lists the complete explicit state of this custom document workspace"
    )]
    fn build(doc: Document, content: ContentStore) -> Self {
        let page_count = doc.page_count();
        let author = std::env::var("QUICK_APP_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "You".to_string());
        Self {
            doc,
            bar: ui::framework::BarCommands::new(),
            raster_handle: RasterHandle::new(content.clone()).ok(),
            raster_document: None,
            pending_open: None,
            raster_images: crate::page_images::PageImages::new(
                crate::page_images::MAX_PAGE_IMAGE_BYTES,
            ),
            raster_render_pending: HashSet::new(),
            raster_tiles: crate::tiles::TileCache::new(crate::tiles::MAX_TILE_CACHE_BYTES),
            raster_tile_batch: Vec::new(),
            raster_render_batch: Vec::new(),
            raster_heading: tilo_page_raster::reach::Heading::default(),
            raster_wake_bound: false,
            scrolled: false,
            delivered_pages: Vec::new(),
            delivered_thumbs: false,
            shown: ui::scroll_copy::ShownFrames::default(),
            raster_text_pending: HashSet::new(),
            raster_links: std::collections::HashMap::new(),
            raster_links_pending: HashSet::new(),
            pending_worker_plan: None,
            applied_worker_plan: None,
            worker_plan_blobs: Vec::new(),
            pending_save: None,
            pending_import: None,
            asked_import: None,
            has_document: false,
            record_source: None,
            reopen_requested: false,
            source_wait: None,
            staged_opening: false,
            staged_import: false,
            doc_name: String::new(),
            load_error: None,
            content,
            shots_rail_drag: false,
            shots_context_menu: false,
            shots_panel: None,
            #[cfg(feature = "samples")]
            staged_scenario: None,
            annotations: Vec::new(),
            comments: Vec::new(),
            signatures: Vec::new(),
            rect: Rect::ZERO,
            fit: FitMode::Width,
            viewport: ViewportState::new()
                .with_zoom_bounds(ZOOM_MIN, ZOOM_MAX)
                .with_padding(DEFAULT_PADDING)
                .with_alignment(SmallContentAlign::Center, SmallContentAlign::Start),
            current_page: 1,
            pending_reading: None,
            position_restored: false,
            last_reading: None,
            resource_page_ids: (1..=page_count)
                .map(|page| format!("source-{page}"))
                .collect(),
            resource_page_sources: (1..=page_count).collect(),
            resource_page_imports: vec![None; page_count],
            source_page_count: page_count,
            page_text: HashMap::new(),
            page_text_revision: 0,
            search_reads_text: false,
            search_entries: std::rc::Rc::default(),
            search_projection_revision: None,
            #[cfg(test)]
            search_projection_refreshes: 0,
            tool: Tool::Select,
            swatch: HighlightColor::Yellow,
            sidebar_tab: SidebarTab::Comments,
            show_sidebar: true,
            review: Self::review_drawer(),
            show_rail: true,
            // A preview rail, not a text sidebar: 168 px at 1.5× UI scale.
            rail_w: 112.0,
            page_sidebar: PageSidebar::new(PAGE_H / PAGE_W).with_add(crate::t("pdf-rail-import")),
            pages_dirty: false,
            portal_import_rx: None,
            selected_anno: None,
            editing: None,
            comment_views: std::cell::RefCell::new(HashMap::new()),
            rich_inset: std::cell::Cell::new(None),
            selected_sig: None,
            sidebar_scroll: 0.0,
            sidebar_reveal: None,
            // Empty until the person types one: the name shown and signed is
            // then the one the workspace knows them by (`Self::signer`).
            signer_name: String::new(),
            author,
            sig_mode: SigMode::Type,
            sig_strokes: Vec::new(),
            sig_stroke: None,
            signature: None,
            signature_revision: None,
            sig_pad_rect: Rect::ZERO,
            find_query: String::new(),
            own_find: None,
            active_match: 0,
            search_whole_word: false,
            search_case_sensitive: false,
            raster_matches: Vec::new(),
            raster_text: std::collections::HashMap::new(),
            selection: None,
            drag_select: DragSelect::new(),
            sel_gran: Granularity::Char,
            sel_anchor_range: (TextPos::new(0, 0), TextPos::new(0, 0)),
            pending_press: None,
            pending_copy: false,
            pending_selection_effect: None,
            text_hover: None,
            raster_text_batch: Vec::new(),
            selection_autoscroll: ui::edge_autoscroll::EdgeAutoscroll::default(),
            canvas_hover_zones: Vec::new(),
            glide: ui::ScrollMomentum::new(),
            interaction: ui::interaction::InteractionEnvironment::default(),
            region: None,
            region_waiting: None,
            shift_down: false,
            source_path: None,
            status_note: Transient::new(),
            note_action: None,
            region_copy: ui::CopyWatch::default(),
            portal_save_rx: None,
            save_destination: None,
            save_write_rx: None,
            portal_action: None,
            last_mouse: Point::ZERO,
            drag: None,
            pan: None,
            rail_drag: None,
            context_menu: None,
            dirty: true,
            pending_visuals: false,
            reduce_motion: false,
            layout_theme: Theme::dark(),
            tab_widths: None,
            empty_open_button: std::cell::Cell::new(Rect::ZERO),
            workspace_documents: Vec::new(),
            empty_document_rows: std::cell::RefCell::new(Vec::new()),
            file_open: ui::widget::FileOpenSurface::default(),
            portal_rx: None,
            portal_contents_rx: None,
            open_here: None,
            pending_reopen: None,
            pending_document_open: None,
            overlay_mode: false,
            prev_overlay: None,
            rail_rect: Rect::ZERO,
            canvas_rect: Rect::ZERO,
            sidebar_rect: Rect::ZERO,
            page_notice: Transient::new(),
            page_said: None,
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
            read_only_state: None,
            live: None,
            presented: None,
            store: std::rc::Rc::default(),
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
            self.status_note.show(error.to_string());
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
            // Another document is looked for afresh.
            self.source_wait = None;
            // Its recorded place is taken up once the canvas has a size;
            // this window has not restored a position for it yet.
            self.pending_reading = state.reading_position();
            self.position_restored = false;
            self.last_reading = None;
        }
        self.dirty = true;
    }

    /// Attach the resource's dispatcher when hosted in the app's view.
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
        if let Some(name) = self.known_name(id) {
            return name;
        }
        // Never a raw participant id: before the workspace names someone, say who
        // they are to the reader as plainly as the reader can tell.
        if self
            .people
            .as_ref()
            .map_or(self.author == id, |people| people.is_me(id))
        {
            crate::t("pdf-author-you")
        } else {
            crate::t("pdf-author-someone")
        }
    }

    /// The name the roster has for `id`, or `id` itself when that is a name
    /// (a record from before ids, a capture's fixture people); `None` for an
    /// opaque participant id the roster cannot name yet.
    fn known_name(&self, id: &str) -> Option<String> {
        let name = self
            .people
            .as_ref()
            .map_or_else(|| id.to_owned(), |people| people.name(id));
        (name != id || !opaque_participant_id(id)).then_some(name)
    }

    /// The name a signature is signed with: the one the person typed, else
    /// the one the workspace knows them by, else none yet (the composer then asks
    /// for it rather than signing with an id or "You").
    pub(crate) fn signer(&self) -> String {
        if self.signer_name.is_empty() {
            self.known_name(&self.author).unwrap_or_default()
        } else {
            self.signer_name.clone()
        }
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
        self.sig_strokes = crate::signature::point_lists(&kept);
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
            tracing::warn!(%error, "keeping the person's signature failed");
            self.status_note
                .show(crate::t("pdf-note-signature-not-kept"));
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
        }
    }

    /// Test-only: open find and materialise matches for `query`.
    #[cfg(test)]
    pub(crate) fn stage_find_for_test(&mut self, query: &str) {
        ui::widget::Findable::find(self, query, ui::widget::FindOptions::default());
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
            search_pages: self.raster_matches.iter().map(|hit| hit.page).collect(),
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
            page_text_revision: self.page_text_revision,
        };
        if self.search_projection_revision.as_ref() == Some(&revision) {
            return;
        }
        self.search_entries = std::rc::Rc::new(crate::search::surface_entries(
            &ctx,
            &self.displayed_page_text(),
        ));
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
        let Some(source) = self.source_blob.clone() else {
            return;
        };
        match PdfMarkup::of(state).pinned_source() {
            None if !self.read_only => {
                self.submit(crate::actions::pin_source_action(&source, &self.doc_name));
            }
            Some(pinned) if pinned.source.hash() != source.hash() => {
                self.status_note.show(crate::t("pdf-note-source-changed"));
            }
            _ => {}
        }
    }

    /// The open document's content hash, which names it the same on every
    /// device (the reference it is kept under may differ).
    fn source_identity(&self) -> Option<String> {
        self.source_blob
            .as_ref()
            .map(|source| hex::encode(source.hash()))
    }

    /// Take over the markup already in the PDF the first time the record
    /// meets these bytes (see [`crate::adopt`]): ask the PDF owner for it,
    /// once per source. Whether it was asked or is not needed.
    fn adopt_existing(&mut self, state: &AutoCommit) -> bool {
        let Some(hash) = self.source_identity() else {
            return true;
        };
        let Some(pinned) = PdfMarkup::of(state).pinned_source() else {
            // The source is pinned first; its markup is taken over after.
            return self.read_only;
        };
        if self.read_only
            || pinned.hash() != hash
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
            || self.source_identity().is_none_or(|current| current != hash)
        {
            return;
        }
        let mut known: std::collections::HashSet<String> = PdfMarkup::of(state)
            .annotations_with_hidden()
            .into_iter()
            .map(|a| a.id)
            .collect();
        for (id, comment) in PdfMarkup::of(state).comments.iter() {
            known.extend(comment.replies.ids().map(|reply| reply.as_str().to_owned()));
            known.insert(id.as_str().to_owned());
        }
        let plan = crate::adopt::plan_import(hash, existing, &known);
        self.submit(crate::actions::import_existing_action(hash, &plan));
        self.reload(state);
        self.dirty = true;
    }

    /// Page images leave the document's own markup out exactly when the
    /// record has taken it over (the reader then draws it from the records).
    fn sync_reader_markup(&mut self, state: &AutoCommit) {
        let hide = self
            .source_identity()
            .is_some_and(|hash| PdfMarkup::of(state).imported_source() == Some(hash.as_str()));
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
        self.raster_render_batch.clear();
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
        // Markup that does not read is not markup that holds nothing: the
        // framework says why in place of the view, and the annotations,
        // comments and their open views stay as they last read whole.
        let Ok(markup) = <PdfMarkup as ui::tilo_resource::Resource>::read(state) else {
            return;
        };
        self.stored_annotations = markup.list_annotations();
        self.annotations = self
            .stored_annotations
            .iter()
            .map(|a| self.page_map(a.page).annotation_to_card(a))
            .collect();
        self.adopted = markup.adopted_entries();
        self.comments = markup.list_comments();
        let shown: HashSet<String> = self
            .comments
            .iter()
            .flat_map(|comment| {
                std::iter::once(comment_view_key(&comment.id, None)).chain(
                    comment
                        .replies()
                        .map(|(reply, _)| comment_view_key(&comment.id, Some(reply))),
                )
            })
            .collect();
        self.comment_views
            .get_mut()
            .retain(|key, _| shown.contains(key));
        self.signatures = markup
            .list_signatures()
            .iter()
            .map(|s| self.page_map(s.page).signature_to_card(s))
            .collect();
        self.form_values = markup.form_values;
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

    fn review_drawer() -> ui::widget::SidebarDrawer {
        let m = ui::tokens::Metrics::BASE;
        ui::widget::SidebarDrawer::new()
            .right()
            .docked_width(f64::from(m.inspector_width))
            .overlay_width(f64::from(m.inspector_width))
            // The pages are paper drawn to the room they get, not a column
            // of text: the panel stands beside them down to their own
            // narrowest.
            .content_surface(f64::from(m.detail_min_width))
    }

    /// The strip the review panel docks in: the window's height, beside the
    /// page rail. The rail's width counts even while it is hidden, so
    /// hiding the rail never changes whether the panels dock.
    fn review_outer(&self) -> Rect {
        let r = self.rect;
        let top = r.y0;
        let (rail_min, rail_max) = self.rail_width_range();
        let x0 = (r.x0 + self.rail_w.clamp(rail_min, rail_max)).min(r.x1);
        Rect::new(x0, top.min(r.y1), r.x1, r.y1)
    }

    /// Snap panel open/closed state to the current window width. On a wide
    /// window the panels dock and reserve space; on a narrow one they collapse
    /// to closed overlays. Only the *transition* forces state, so the user's
    /// explicit toggles within a size class are respected.
    fn reconcile_overlay(&mut self) {
        // The panels dock while the page canvas keeps its minimum width
        // beside the rail and the review panel at its narrowest; below that
        // they float over the canvas.
        let outer = self.review_outer();
        self.review.resolve(outer);
        let overlay = self.review.is_overlay();
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

    /// The sidebar's drawn width: docked, the shared inspector's (theme,
    /// window and the person's resize); floating, narrower on cramped
    /// windows so it always leaves a strip of canvas to tap on to dismiss it.
    fn sidebar_width(&self) -> f64 {
        if !self.show_sidebar {
            return 0.0;
        }
        let theme = &self.layout_theme;
        let m = &theme.tokens.metrics;
        if self.overlay_mode {
            let preferred = f64::from(m.inspector_width) * f64::from(theme.text_growth());
            preferred
                .min(self.rect.width() - f64::from(m.header_height))
                .max(f64::from(m.sidebar_min_width))
        } else {
            self.review.sidebar_rect().width()
        }
    }

    fn recompute_regions(&mut self) {
        let r = self.rect;
        // The controls are the window bar's: the panels and the pages take
        // the window's whole height.
        let mid_top = r.y0;
        // No reserved bottom strip: the page and the zoom are said in the
        // window's bar, so the content spans to the window bottom.
        let mid_bottom = r.y1;
        self.review.snap(self.show_sidebar);
        self.review.resolve(self.review_outer());
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
        self.canvas_rect = Rect::new(x0, mid_top, x1, mid_bottom);
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
        let padding = f64::from(self.layout_theme.tokens.spacing.s3);
        let avail_w = r.width() - 2.0 * padding;
        let avail_h = r.height() - 2.0 * padding;
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
        // A derived fit follows the canvas size, not a focal-point gesture.
        // Keep the same document position at the viewport's leading edge;
        // centre-anchored zoom would scroll a newly opened page past its title
        // when the host applies its theme or resizes the window.
        let ratio = z / self.zoom();
        let scroll = (self.scroll_x() * ratio, self.scroll_y() * ratio);
        self.viewport.set_zoom(z);
        if !matches!(self.fit, FitMode::Custom(_)) {
            self.viewport.set_scroll(scroll.0, scroll.1);
        }
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
        self.glide.stop();
        self.viewport.set_scroll(self.viewport.scroll_x(), target_y);
        self.current_page = page;
        self.dirty = true;
    }

    /// Where the reader is now: the page occurrence under the canvas's top
    /// edge, how far down that page, and the zoom.
    fn current_reading(&self) -> Option<(String, f64, f64)> {
        if !self.has_document || self.canvas_rect.width() <= 0.0 || self.ph() <= 0.0 {
            return None;
        }
        let page_id = self.page_id_of(self.current_page)?;
        let unit = self.stack_unit();
        if unit <= 0.0 {
            return None;
        }
        let top = self
            .viewport
            .screen_to_content(Point::new(self.canvas_rect.x0, self.canvas_rect.y0))
            .y;
        let within =
            (top - (self.current_page.saturating_sub(1) as f64) * unit).clamp(0.0, self.ph());
        Some((page_id, (within / self.ph()).clamp(0.0, 1.0), self.zoom()))
    }

    /// Take up the record's recorded place, once the canvas has a size and
    /// the page plan is known. A page that is no longer in the plan leaves
    /// the reader at the top rather than guessing.
    fn restore_reading(&mut self) {
        if self.position_restored || !self.has_document || self.canvas_rect.width() <= 0.0 {
            return;
        }
        self.position_restored = true;
        // Where a document opens with no place to take up is not a place
        // the reader chose: it is written back only once they move.
        let Some((page_id, fraction, zoom)) = self.pending_reading.take() else {
            self.last_reading = self.current_reading();
            return;
        };
        let Some(index) = self.resource_page_ids.iter().position(|id| id == &page_id) else {
            self.last_reading = self.current_reading();
            return;
        };
        let page = index + 1;
        self.fit = FitMode::Custom(zoom.clamp(ZOOM_MIN, ZOOM_MAX));
        self.recompute_zoom();
        let unit = self.stack_unit();
        let target = (page.saturating_sub(1) as f64) * unit + fraction.clamp(0.0, 1.0) * self.ph();
        // Scroll is measured from the padded edge, so move by the difference
        // between where the canvas top now shows and the recorded place.
        let now_top = self
            .viewport
            .screen_to_content(Point::new(self.canvas_rect.x0, self.canvas_rect.y0))
            .y;
        let target_y = self.viewport.scroll_y() + (target - now_top) * self.viewport.zoom();
        self.viewport.set_scroll(self.viewport.scroll_x(), target_y);
        self.current_page = self.nearest_page();
        self.last_reading = Some((page_id, fraction, self.zoom()));
        self.dirty = true;
    }

    /// Keep the record's resume hint current: written when the page or the
    /// place within it settles, never for every pixel of a glide or a drag.
    fn record_reading(&mut self) {
        if self.read_only
            || !self.has_document
            || self.glide.is_active()
            || self.pan.is_some()
            || self.canvas_rect.width() <= 0.0
        {
            return;
        }
        let Some(position) = self.current_reading() else {
            return;
        };
        let settled = self.last_reading.as_ref().is_none_or(|last| {
            last.0 != position.0
                || (last.1 - position.1).abs() > 0.005
                || (last.2 - position.2).abs() > 0.001
        });
        if settled {
            self.submit(crate::actions::record_reading_position_action(
                &position.0,
                position.1,
                position.2,
            ));
            self.last_reading = Some(position);
        }
    }

    /// Carry a glide on to `now`: the page moves by what the glide covered
    /// since the last frame, and stops at either end of the document.
    fn advance_glide(&mut self, now: ui::time::Instant) {
        let dy = self.glide.advance(now);
        if dy == 0.0 {
            return;
        }
        let before = self.scroll_y();
        self.viewport.pan_by(0.0, dy);
        if self.scroll_y().to_bits() == before.to_bits() {
            self.glide.stop();
            return;
        }
        self.current_page = self.nearest_page();
        // Only the view moved, as for a wheel.
        self.scrolled = true;
        if self.selection.is_some_and(|sel| sel.dragging) {
            self.update_selection_at(self.last_mouse);
        }
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
                    let Some(action) =
                        crate::actions::reorder_page_plan_action(&self.resource_page_ids, from, to)
                    else {
                        continue;
                    };
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
            self.submit(PdfMarkup::START_PAGE_PLAN.with(self.source_page_count.max(1)));
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
                self.status_note.show(crate::t(match missing {
                    ContentState::Pending => "pdf-note-imported-arriving",
                    ContentState::Waiting => "pdf-note-imported-waiting",
                    ContentState::Resolved | ContentState::Unavailable => {
                        "pdf-note-imported-unavailable"
                    }
                }));
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
                    self.status_note
                        .show(crate::t("pdf-note-pages-unavailable"));
                }
            }
            self.doc.pages = (1..=plan.len())
                .map(|number| model::Page { number })
                .collect();
        } else {
            // Without the document's engine the plan's pages show as blank
            // cards; its occurrences are kept so a peer with the bytes, or
            // this one once they arrive, materialises them.
            if plan.iter().any(|entry| entry.imported.is_some()) {
                self.status_note.show(crate::t("pdf-note-imported-unshown"));
            } else if sources
                .iter()
                .any(|source| *source == 0 || *source > self.source_page_count)
            {
                self.status_note.show(crate::t("pdf-note-pages-missing"));
                return;
            }
            self.doc.pages = (1..=plan.len())
                .map(|number| model::Page { number })
                .collect();
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
        self.import_from(self.current_page, |handle, document, at, after| {
            handle.import_path(document, path, at, after)
        })
    }

    /// Import the pages of a file the host's chooser delivered by contents.
    fn import_picked(&mut self, file: ui::portal::PickedContents) -> bool {
        self.import_from(self.current_page, move |handle, document, at, after| {
            handle.import_contents(document, &file.name, file.bytes, at, after)
        })
    }

    /// Start importing a file's pages after the first `at` pages of the
    /// document: the one import the rail's Add, a dropped file and the
    /// insert-pages effect share.
    fn import_from(
        &mut self,
        at: usize,
        submit: impl FnOnce(
            &RasterHandle,
            u64,
            usize,
            Option<String>,
        ) -> Result<ImportRequest, crate::source::RasterSubmitError>,
    ) -> bool {
        if self.pending_import.is_some() {
            self.status_note.show(crate::t("pdf-note-import-busy"));
            self.dirty = true;
            return false;
        }
        let Some(document) = self.raster_document.as_ref() else {
            self.status_note
                .show(crate::t("pdf-note-import-unavailable"));
            self.dirty = true;
            return false;
        };
        let after = at
            .checked_sub(1)
            .and_then(|last| self.resource_page_ids.get(last))
            .cloned();
        match self
            .raster_handle
            .as_ref()
            .ok_or_else(|| "the PDF import worker is unavailable".to_string())
            .and_then(|handle| {
                submit(handle, document.id, at, after).map_err(|error| error.to_string())
            }) {
            Ok(request) => {
                self.pending_import = Some(request);
                self.status_note
                    .show_progress(crate::t("pdf-note-importing"));
                self.dirty = true;
                true
            }
            Err(error) => {
                tracing::warn!(%error, "submitting a page import failed");
                self.show_failure(
                    crate::t("pdf-note-import-failed"),
                    note_action::NoteAction::Import,
                );
                self.dirty = true;
                false
            }
        }
    }

    fn request_import_via_portal(&mut self) {
        if !self.raster_active() {
            self.status_note
                .show(crate::t("pdf-note-import-no-document"));
            self.dirty = true;
            return;
        }
        if self.portal_import_rx.is_some() || self.pending_import.is_some() {
            return;
        }
        self.portal_import_rx = Some(pdf_import_request().send_contents());
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
                    ContentsOutcome::Picked(files) => {
                        if let Some(file) = files.into_iter().next() {
                            self.import_picked(file);
                        }
                    }
                    ContentsOutcome::Cancelled => {}
                    ContentsOutcome::Failed(reason) => {
                        tracing::warn!(%reason, "the import picker failed");
                        self.status_note
                            .show(crate::t("pdf-note-import-picker-failed"));
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
        &self.find_query
    }

    /// Whether the window bar's search shows matches on the pages.
    fn finding(&self) -> bool {
        !self.find_query.is_empty()
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
                    self.status_note.show(crate::t("pdf-note-text-unavailable"));
                }
            }
            let mut hits = Vec::new();
            for (page, text) in &self.raster_text {
                let remaining = model::MAX_SEARCH_RESULTS.saturating_sub(hits.len());
                hits.extend(text.search(*page, &query, opts).into_iter().take(remaining));
                if hits.len() >= model::MAX_SEARCH_RESULTS {
                    break;
                }
            }
            hits.sort_by_key(|hit| (hit.page, hit.char_start));
            // A scanned PDF is pictures of pages: its words can be seen but
            // not found. Say so instead of a bare "0 of 0".
            if hits.is_empty() && !query.trim().is_empty() && self.document_has_no_text() {
                self.status_note.show(crate::t("pdf-note-no-text"));
            }
            self.raster_matches = hits;
            if let Some(hit) = self.raster_matches.first() {
                self.go_to_page(hit.page);
            }
        } else {
            self.raster_matches.clear();
        }
        self.dirty = true;
    }

    /// Whether every page's text layer has arrived and none holds any text:
    /// the document is images of pages (a scan), with nothing to find,
    /// select or copy.
    fn document_has_no_text(&self) -> bool {
        let pages = self.doc.page_count();
        pages > 0
            && (1..=pages).all(|page| {
                self.raster_text
                    .get(&page)
                    .is_some_and(|text| text.text.trim().is_empty())
            })
    }

    /// The number of live search hits.
    fn match_count(&self) -> usize {
        self.raster_matches.len()
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
        use ui::widget::Findable as _;
        let options = ui::widget::FindOptions {
            match_case: case_sensitive,
            whole_word,
            ..ui::widget::FindOptions::default()
        };
        self.find(query, options);
        let matched_page = self.raster_matches.get(index).map(|hit| hit.page);
        if matched_page == Some(page) {
            self.reveal(index);
            // The window's search shows the query and steps on from here.
            self.own_find = Some(ui::widget::OwnFind {
                query: query.to_owned(),
                options,
                current: index,
            });
            Self::open_search();
        } else {
            // A progressive worker may not have extracted this page yet, or a
            // source document may have changed.  Refuse to rebind a stale
            // ordinal and leave an actionable status rather than navigating to
            // a potentially unrelated match.
            self.status_note.show(crate::t("pdf-note-find-result-gone"));
        }
        self.dirty = true;
    }

    /// End the image-region marquee: crop it and copy it to the clipboard,
    /// now or, when its page is still being rendered for the crop, as soon
    /// as the render arrives. Whether it was copied now.
    fn release_region(&mut self) -> bool {
        self.region
            .take()
            .is_some_and(|region| self.copy_region(region))
    }

    /// The page bitmap a crop of `region` is cut from, and the page's size.
    fn region_source(&self, region: RegionSelect) -> ((usize, u32), (f64, f64)) {
        let page_idx = region.page.saturating_sub(1);
        let (pgw, pgh) = self
            .raster_document
            .as_ref()
            .and_then(|document| document.page_sizes.get(page_idx).copied())
            .unwrap_or_else(|| (self.pw(), self.ph()));
        let bucket = ((pgw * 2.0).round() as u32).clamp(96, 4096);
        (
            (page_idx, crate::source::RasterBackend::bucket_for(bucket)),
            (pgw, pgh),
        )
    }

    /// Crop `region` out of its page bitmap and copy it to the clipboard as
    /// an image. Raster backend only. Returns true when a region of usable
    /// size was cropped (the clipboard push itself is best-effort — it
    /// silently no-ops in a headless session). A page not rendered for the
    /// crop yet is asked for, and the copy waits for it
    /// ([`Self::copy_waiting_region`]).
    fn copy_region(&mut self, region: RegionSelect) -> bool {
        let rect = region.rect();
        if rect.w < 3.0 || rect.h < 3.0 {
            return false;
        }
        let (image_key, (pgw, pgh)) = self.region_source(region);
        let Some(image) = self.raster_images.get(&image_key) else {
            if self.raster_document.is_none() {
                return false;
            }
            if self.ask_region_render(region) {
                self.status_note
                    .show_progress(crate::t("pdf-note-copying-area"));
            } else {
                self.status_note
                    .show(crate::t("pdf-note-pages-unavailable"));
            }
            self.dirty = true;
            return false;
        };
        let Some((width, height, rgba)) = render::crop_page_region(&image, pgw, pgh, rect) else {
            return false;
        };
        let copied = self
            .region_copy
            .watch(ui::clipboard_copy_image(width, height, &rgba));
        self.status_note.show(crate::t(if copied {
            "pdf-note-copied-area"
        } else {
            "pdf-note-area-not-copied"
        }));
        self.dirty = true;
        true
    }

    /// Ask for the page render a crop of `region` is cut from, on its own,
    /// outside the frame's render batch, so a batch that moves on with the
    /// view never cancels it; the marquee waits for it. Whether it was
    /// asked for (a marquee that could not ask waits for nothing).
    fn ask_region_render(&mut self, region: RegionSelect) -> bool {
        let Some(document_id) = self.raster_document.as_ref().map(|document| document.id) else {
            self.region_waiting = None;
            return false;
        };
        let (page, target_width) = self.region_source(region).0;
        let asked = self.raster_handle.as_ref().is_some_and(|handle| {
            handle
                .request_render(RenderRequest {
                    document_id,
                    page,
                    target_width,
                })
                .is_ok()
        });
        self.region_waiting = asked.then_some((document_id, region));
        asked
    }

    /// Ask again for the render a waiting marquee needs, when the result it
    /// waited for was lost on the way (the bounded result queue let it go,
    /// or an open that then failed dropped the queued request): nothing
    /// else would ever bring it, and the copy would wait for good.
    fn ask_again_for_waiting_region(&mut self) {
        let Some((document_id, region)) = self.region_waiting else {
            return;
        };
        let current = self
            .raster_document
            .as_ref()
            .is_some_and(|document| document.id == document_id);
        if !current
            || self
                .raster_images
                .contains_key(&self.region_source(region).0)
        {
            // Settled by `copy_waiting_region` this frame.
            return;
        }
        if !self.ask_region_render(region) {
            self.status_note.show(crate::t("pdf-note-area-not-copied"));
            self.dirty = true;
        }
    }

    /// Copy a released marquee whose page render has arrived. One whose
    /// document was replaced is dropped; one whose render failed was
    /// dropped with the failure, which says so itself.
    fn copy_waiting_region(&mut self) {
        let Some((document_id, region)) = self.region_waiting else {
            return;
        };
        if self
            .raster_document
            .as_ref()
            .is_none_or(|document| document.id != document_id)
        {
            // Its progress note goes on the next sync (`keep_while`).
            self.region_waiting = None;
            self.dirty = true;
        } else if self
            .raster_images
            .contains_key(&self.region_source(region).0)
        {
            self.region_waiting = None;
            self.copy_region(region);
        }
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
            self.status_note.show(crate::t("pdf-note-save-no-document"));
            self.dirty = true;
            return;
        }
        if self.portal_save_rx.is_some()
            || self.pending_save.is_some()
            || self.save_write_rx.is_some()
        {
            return;
        }
        self.portal_save_rx =
            Some(pdf_save_request(&self.suggested_save_name()).send_save_destination());
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
        self.save_destination = None;
        self.save_write_rx = None;
        // A document asked to open in this window's place is not opened.
        if let Some((_, outcome)) = self.pending_reopen.take() {
            outcome.withdraw();
        }
        if let Some((_, outcome)) = self.pending_document_open.take() {
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
                    // A destination on this device: the worker writes the
                    // file in place, atomically.
                    DestinationOutcome::Chosen(destination) if destination.location().is_some() => {
                        let path = ensure_pdf_extension(
                            destination
                                .location()
                                .map(Path::to_path_buf)
                                .unwrap_or_default(),
                        );
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
                    // A destination the host keeps: the worker hands the
                    // exported bytes back and they are written there.
                    DestinationOutcome::Chosen(destination) => {
                        let path =
                            ensure_pdf_extension(Path::new("picked").join(destination.name()));
                        let success = self.submit_save(&path, true);
                        if success {
                            self.save_destination = Some(destination);
                        } else {
                            self.finish_portal_action(false, Some("PDF could not be saved".into()));
                        }
                    }
                    DestinationOutcome::Cancelled => self.finish_portal_action(false, None),
                    DestinationOutcome::Failed(reason) => {
                        tracing::warn!(%reason, "the save picker failed");
                        self.status_note
                            .show(crate::t("pdf-note-save-picker-failed"));
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

    /// Write a save the worker handed back to where the host chose, once
    /// written. Called once per frame from [`Widget::sync`].
    fn drain_save_write(&mut self) {
        let Some((_, rx)) = self.save_write_rx.as_ref() else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ui::portal::SaveOutcome::Failed("the save went away".into())
            }
        };
        let Some((name, _)) = self.save_write_rx.take() else {
            return;
        };
        match outcome {
            ui::portal::SaveOutcome::Saved => {
                self.status_note.show(saved_note(&name));
                self.pages_dirty = false;
                self.finish_portal_action(true, None);
            }
            ui::portal::SaveOutcome::Cancelled => self.finish_portal_action(false, None),
            ui::portal::SaveOutcome::Failed(reason) => {
                tracing::warn!(%reason, "writing the saved PDF failed");
                self.show_failure(save_failed_note(&name), note_action::NoteAction::Save);
                self.finish_portal_action(false, Some(reason));
            }
        }
        self.dirty = true;
    }

    /// Flatten the current annotations + signatures onto a fresh copy of the
    /// PDF and write it to `path`. Surfaces success or failure as a toast.
    fn save_annotated_to(&mut self, path: &std::path::Path) -> bool {
        self.submit_save(path, false)
    }

    /// Queue the flattening save named `path`: written there, or with
    /// `deliver` handed back as bytes ([`RasterEvent::Exported`]).
    fn submit_save(&mut self, path: &std::path::Path, deliver: bool) -> bool {
        if self.pending_worker_plan.is_some() {
            self.status_note
                .show(crate::t("pdf-note-save-waiting-pages"));
            self.dirty = true;
            return false;
        }
        let Some(document) = self.raster_document.as_ref() else {
            // No folder or name would help: there is no PDF to save yet.
            self.status_note.show(crate::t("pdf-note-save-no-document"));
            self.dirty = true;
            return false;
        };
        let id = match crate::source::next_worker_request_id() {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!(%error, "submitting a save failed");
                self.show_failure(
                    save_failed_note(&display_name(path)),
                    note_action::NoteAction::Save,
                );
                self.dirty = true;
                return false;
            }
        };
        let request = SaveRequest {
            id,
            document_id: document.id,
            markup: self.export_markup(),
            path: path.to_path_buf(),
            deliver,
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
                self.status_note.show_progress(crate::t_args(
                    "pdf-note-saving",
                    &[("name", display_name(path).into())],
                ));
                self.dirty = true;
                true
            }
            Err(error) => {
                tracing::warn!(%error, "submitting a save failed");
                self.show_failure(
                    save_failed_note(&display_name(path)),
                    note_action::NoteAction::Save,
                );
                self.dirty = true;
                false
            }
        }
    }

    // ---- annotation actions -----------------------------------------

    /// Preview and commit share glyph ranges, snapped lines and free-form
    /// fallback, so releasing the pointer does not change what was marked.
    fn highlight_geometry(&self, drag: &DragState) -> (Vec<PtRect>, Option<(u32, u32)>) {
        if let Some(text) = self.raster_text.get(&drag.page)
            && let Some((start, end)) = text.range_between(drag.start, drag.cur)
        {
            return (
                text.bands_for_range(start, end),
                text.source_range(start, end),
            );
        }
        let mut rect = PtRect::from_corners(drag.start.0, drag.start.1, drag.cur.0, drag.cur.1);
        rect.h = rect.h.max(3.0);
        (vec![rect], None)
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
                self.ensure_raster_text(d.page);
                let (quads, chars) = self.highlight_geometry(&d);
                let a = Annotation {
                    chars: chars.map(|(start, end)| [start, end]),
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
        let anno_id = new_id();
        let comment_id = new_id();
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
                    .find(|thread| &thread.id == id && thread.comment.text == initial)
                    .map_or_else(|| tilo_rich_text::plain_blocks(initial), Thread::blocks),
                _ => tilo_rich_text::plain_blocks(initial),
            };
            let field = self.comment_field(&blocks, matches!(target, EditTarget::Reply(_)));
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
    fn comment_field(&self, blocks: &[ui::widget::BlockContent], reply: bool) -> CommentField {
        let hint = if reply {
            crate::t("pdf-comment-reply-placeholder")
        } else {
            crate::t("pdf-viewer-comment-placeholder")
        };
        let mut surface = RichTextSurface::over(Draft::with_blocks(blocks))
            .with_editor(
                ui::widget::BlockEditor::new()
                    .with_wrap_lines(true)
                    .with_compact_paragraph_spacing(true)
                    .with_block_handles(false)
                    .with_body_size(Self::comment_body_size(&self.layout_theme)),
            )
            .with_placeholder(&hint)
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
                    .is_some_and(|c| !c.comment.replies.is_empty());
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
                // An emptied name goes back to the one the workspace knows.
                text.trim().clone_into(&mut self.signer_name);
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
                self.status_note.show(crate::t("pdf-note-item-gone"));
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
        let a = self.signature_ink_rect();
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

    /// The same ink area is used for painting and pointer normalisation.
    fn signature_ink_rect(&self) -> Rect {
        self.sig_pad_rect
            .inset(-f64::from(self.layout_theme.tokens.spacing.s3))
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
            self.status_note
                .show(crate::t("pdf-note-signature-draw-first"));
            self.dirty = true;
            return;
        };
        if self.signer().trim().is_empty() {
            // A signature names its signer: ask for the name rather than
            // stamping a nameless one.
            self.status_note
                .show(crate::t("pdf-note-signature-name-first"));
            self.begin_edit(EditTarget::Signer, "");
            self.dirty = true;
            return;
        }
        let w = 218.0;
        let h = 74.0;
        let rect = PtRect::new(
            (pt.0 - w / 2.0).clamp(MARGIN, self.pw() - MARGIN - w),
            (pt.1 - h / 2.0).clamp(MARGIN, self.ph() - MARGIN - h),
            w,
            h,
        );
        let sig_id = new_id();
        let s = Signature {
            id: sig_id.clone(),
            page,
            rect,
            signer: self.signer(),
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
            self.status_note.show(crate::t_args(
                "pdf-note-field-read-only",
                &[("field", w.name.as_str().into())],
            ));
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
                    self.status_note.show(crate::t("pdf-note-link-page-gone"));
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
                    let key = match ui::widget::try_open_external_url(uri) {
                        Ok(()) => "pdf-note-link-opening",
                        Err(_) => "pdf-note-link-failed",
                    };
                    self.status_note
                        .show(crate::t_args(key, &[("address", uri.as_str().into())]));
                } else {
                    self.status_note.show(crate::t_args(
                        "pdf-note-link-unsupported",
                        &[("address", uri.as_str().into())],
                    ));
                }
                self.dirty = true;
            }
            crate::source::LinkTarget::File(path) => {
                self.status_note.show(crate::t_args(
                    "pdf-note-link-file",
                    &[("path", path.as_str().into())],
                ));
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
                .find(|c| c.pinned() == Some(id.as_str()))
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
        const WIDTH: f64 = 180.0; // PDF points on the page, not UI spacing; allow-breakpoint: a PDF page width, not a layout breakpoint
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

    /// Route a shared-HUD action to this reader's commands.
    fn apply_hud_action(&mut self, action: ui::HudAction, state: &mut ui::widget::Edits<'_>) {
        match action {
            ui::HudAction::Copy => {
                self.copy_selection();
            }
            ui::HudAction::Highlight(i) => {
                let sw = HighlightColor::ALL
                    .get(i)
                    .copied()
                    .unwrap_or(HighlightColor::Purple);
                self.highlight_selection(state, sw);
            }
            ui::HudAction::Note => {
                self.note_from_selection(state);
            }
            ui::HudAction::SelectAll => {
                self.select_all();
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
                    .and_then(|c| c.pinned().map(str::to_owned))
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
                    let refusal = self.another_window_refusal(&name);
                    self.status_note.show(refusal.clone());
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
                    self.load_error = Some(crate::t_args(
                        "pdf-file-chooser-failed",
                        &[("reason", ui::i18n::ArgValue::Text(reason.clone()))],
                    ));
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
                            self.load_error = Some(crate::t_args(
                                "pdf-file-chooser-failed",
                                &[("reason", ui::i18n::ArgValue::Text(reason.clone()))],
                            ));
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

    /// Why a file chosen by its contents does not open in this window, and
    /// where it does: this window's record marks up its own document, and
    /// a new window opens any file.
    pub(super) fn another_window_refusal(&self, name: &str) -> String {
        let shown = self
            .record_source
            .as_ref()
            .map(|source| source.name.clone())
            .filter(|name| !name.is_empty())
            .or_else(|| {
                self.source_path
                    .as_deref()
                    .and_then(std::path::Path::file_name)
                    .map(|name| name.to_string_lossy().into_owned())
            });
        match shown {
            Some(shown) => crate::t_args(
                "pdf-note-open-in-new-window",
                &[("shown", shown.into()), ("name", name.into())],
            ),
            None => crate::t_args(
                "pdf-note-open-in-new-window-unnamed",
                &[("name", name.into())],
            ),
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
        self.status_note.show_progress(crate::t("pdf-note-opening"));
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
        let looked = ui::collaboration::record_content::arrivals();
        if self.source_wait.is_some_and(|(_, at)| at == looked) {
            return;
        }
        let name = if source.name.is_empty() {
            crate::CATALOG.t("pdf-viewer-untitled")
        } else {
            source.name.clone()
        };
        // A document shared in the workspace is fetched from a device that has
        // it: reading it asks for it, and it opens once it is here.
        let reference = source.source.reference().unwrap_or_default();
        let Some(bytes) = source.bytes(&self.content) else {
            let state = self.content.state(&reference);
            if self.source_wait.map(|(was, _)| was) != Some(state) {
                self.dirty = true;
            }
            self.source_wait = Some((state, looked));
            return;
        };
        self.reopen_requested = true;
        self.source_wait = None;
        self.queue_worker_open_contents(&name, bytes);
    }

    /// Submit a replacement to the PDF owner: filesystem reads and `PDFium`
    /// parsing never run in `sync()` or an input handler. On success the
    /// view resets to page one at fit-page and the reader switches to that
    /// document's own markup record; on failure the empty state surfaces
    /// the reason.
    fn queue_worker_open(&mut self, path: &std::path::Path) {
        let Some(handle) = self.raster_handle.as_ref() else {
            self.record_open_failure(path, &crate::t("pdf-error-worker-stopped"));
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
            self.record_open_failure(path, &crate::t("pdf-error-worker-stopped"));
            return;
        };
        match handle.open_contents(name, bytes) {
            Ok(request) => self.begin_open(request),
            Err(error) => self.record_open_failure(path, &error.to_string()),
        }
    }

    fn begin_open(&mut self, request: OpenRequest) {
        self.pending_open = Some(request);
        self.status_note.show_progress(crate::t("pdf-note-opening"));
        self.dirty = true;
    }

    fn record_open_failure(&mut self, path: &std::path::Path, error: &str) {
        let error = crate::t_args(
            "pdf-open-failed",
            &[
                ("file", ui::i18n::ArgValue::Text(path.display().to_string())),
                ("error", ui::i18n::ArgValue::Text(error.to_owned())),
            ],
        );
        if self.has_document {
            self.status_note.show(open_failed_note(path));
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
        let mut lost_region_render = false;
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
                    self.source_blob = Some(raster.source_bytes.clone());
                    self.existing_request = None;
                    self.reader_markup_hidden = false;
                    self.form_values.clear();
                    self.resource_page_rotations.clear();
                    self.raster_document = Some(raster);
                    self.raster_images.clear();
                    self.raster_render_pending.clear();
                    self.raster_tiles.clear();
                    self.raster_tile_batch.clear();
                    self.raster_render_batch.clear();
                    self.raster_text_pending.clear();
                    self.pending_worker_plan = None;
                    self.applied_worker_plan = None;
                    self.worker_plan_blobs.clear();
                    self.has_document = true;
                    self.seed_pending = true;
                    self.doc_name = name;
                    self.source_path = Some(request.path.clone());
                    self.load_error = None;
                    self.status_note.dismiss();
                    self.fit = FitMode::Width;
                    self.viewport.set_scroll(0.0, 0.0);
                    self.current_page = 1;
                    self.pending_reading = None;
                    self.position_restored = false;
                    self.last_reading = None;
                    self.resource_page_ids = (1..=self.doc.page_count())
                        .map(|page| format!("source-{page}"))
                        .collect();
                    self.resource_page_sources = (1..=self.doc.page_count()).collect();
                    self.resource_page_imports = vec![None; self.doc.page_count()];
                    self.source_page_count = self.doc.page_count();
                    self.page_text.clear();
                    self.page_text_revision += 1;
                    self.search_reads_text = false;
                    self.selected_anno = None;
                    self.selected_sig = None;
                    self.editing = None;
                    // Another document: the old one's matches go with it.
                    self.find_query.clear();
                    self.own_find = None;
                    self.raster_matches.clear();
                    self.active_match = 0;
                    self.raster_text.clear();
                    self.text_layers_replaced("another PDF opened");
                    self.raster_links.clear();
                    self.raster_links_pending.clear();
                    self.prev_overlay = None;
                    self.reconcile_overlay();
                    self.recompute_regions();
                    self.sync_viewport_frame();
                    self.rebuild_page_maps();
                    self.dirty = true;
                    if let Some(handle) = self.raster_handle.as_ref()
                        && let Err(error) = handle.activate_document(request.id)
                    {
                        tracing::warn!(%error, "activating the opened PDF failed");
                        self.status_note
                            .show(crate::t("pdf-note-pages-unavailable"));
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
                    // The open dropped the queued page work, a waiting
                    // marquee's render with it.
                    lost_region_render = true;
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
                RasterEvent::VisualWorkEvicted => {
                    // A wider canvas can complete more tiles than fit in one
                    // result burst. Cached results remain valid; only missing
                    // work must be eligible for submission again.
                    self.raster_render_pending.clear();
                    self.raster_text_pending.clear();
                    self.raster_text_batch.clear();
                    self.raster_links_pending.clear();
                    self.raster_tile_batch.clear();
                    self.raster_render_batch.clear();
                    self.text_requests_dropped();
                    lost_region_render = true;
                    self.dirty = true;
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
                        self.raster_images.insert(key, image);
                    } else if self
                        .region_waiting
                        .is_some_and(|(_, region)| self.region_source(region).0 == key)
                    {
                        // The engine could not draw the page for the crop:
                        // the marquee waiting for it is not copied, and
                        // the person is told.
                        self.region_waiting = None;
                        self.status_note.show(crate::t("pdf-note-area-not-copied"));
                        self.dirty = true;
                    }
                    if request.target_width > crate::tiles::THUMB_MAX_PX {
                        self.delivered_pages.push(request.page);
                    } else {
                        self.delivered_thumbs = true;
                    }
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
                    // A marquee waiting for this render is not copied.
                    if self
                        .region_waiting
                        .is_some_and(|(_, region)| self.region_source(region).0 == key)
                    {
                        self.region_waiting = None;
                    }
                    tracing::warn!(%error, page = request.page + 1, "rendering a page failed");
                    self.status_note.show(crate::t_args(
                        "pdf-note-page-not-shown",
                        &[("page", (request.page + 1).into())],
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
                        self.raster_tiles.insert(request.key(), image);
                    }
                    self.delivered_pages.push(request.page);
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
                    self.keep_page_text(request.page + 1, &text);
                    // An open search finds again over the grown text (its
                    // content revision moved), so the counter and the
                    // matches move together.
                    self.raster_text.insert(request.page + 1, text);
                    self.text_arrived(request.page + 1);
                    // A page's text shows only through find matches or a
                    // selection on it (a press it resolved repaints for
                    // itself); read ahead under a resting pointer, it
                    // changes nothing on screen, and a scroll goes on
                    // copying.
                    if self.finding()
                        || self
                            .selection
                            .is_some_and(|sel| sel.pages().contains(&(request.page + 1)))
                    {
                        self.dirty = true;
                    }
                }
                RasterEvent::LinksRead { request, links }
                    if self
                        .raster_document
                        .as_ref()
                        .is_some_and(|doc| doc.id == request.document_id) =>
                {
                    self.raster_links_pending.remove(&request.page);
                    // Links are only hit-tested, never drawn: their arrival
                    // repaints nothing.
                    self.raster_links.insert(request.page + 1, links);
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
                    self.raster_text_batch.retain(|page| *page != request.page);
                    let why = format!("Couldn’t read text on page {}: {error}", request.page + 1);
                    self.text_failed(request.page + 1, &why);
                    tracing::warn!(%error, page = request.page + 1, "reading a page's text failed");
                    self.status_note.show(crate::t_args(
                        "pdf-note-page-text-unread",
                        &[("page", (request.page + 1).into())],
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
                    self.raster_render_batch.clear();
                    self.raster_text.clear();
                    self.raster_links.clear();
                    self.raster_links_pending.clear();
                    self.raster_text_pending.clear();
                    let hovered = self.text_hover.is_some();
                    self.page_plan_replaced("the pages changed");
                    if let Some(document) = self.raster_document.as_mut() {
                        document.page_sizes = page_sizes;
                        document.geometries = geometries;
                        document.rotations = rotations;
                    }
                    self.rebuild_page_maps();
                    // A pointer resting on a page keeps its text read ahead:
                    // the page under it in the new layout is asked for again,
                    // or a press there would wait for a layer nobody asked.
                    if hovered {
                        self.hover_text(self.last_mouse);
                    }
                    self.submit_text_batch();
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
                    tracing::warn!(%error, "applying the shared page plan failed");
                    self.status_note
                        .show(crate::t("pdf-note-pages-not-arranged"));
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
                    self.pending_import = None;
                    self.imported(state, &request, &hash, &kind, &sizes);
                }
                RasterEvent::ImportFailed { request, error }
                    if self.pending_import.as_ref() == Some(&request) =>
                {
                    self.pending_import = None;
                    self.import_failed(&request, &error);
                    tracing::warn!(%error, "importing pages failed");
                    self.show_failure(
                        crate::t("pdf-note-import-failed"),
                        note_action::NoteAction::Import,
                    );
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
                        tracing::warn!(%error, "committing the saved PDF failed");
                        self.show_failure(
                            save_failed_note(&display_name(&request.path)),
                            note_action::NoteAction::Save,
                        );
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
                    self.status_note
                        .show(saved_note(&display_name(&request.path)));
                    self.pages_dirty = false;
                    self.dirty = true;
                    self.finish_portal_action(true, None);
                }
                RasterEvent::Exported { request, bytes }
                    if self.pending_save.as_ref() == Some(&request) =>
                {
                    self.pending_save = None;
                    match self.save_destination.take() {
                        Some(destination) => {
                            let name = destination.name().to_owned();
                            self.save_write_rx = Some((name, destination.write(bytes)));
                        }
                        None => {
                            self.finish_portal_action(false, Some("the save was withdrawn".into()));
                        }
                    }
                    self.dirty = true;
                }
                RasterEvent::SaveFailed { request, error }
                    if self.pending_save.as_ref() == Some(&request) =>
                {
                    self.save_destination = None;
                    self.pending_save = None;
                    tracing::warn!(%error, "saving the PDF failed");
                    self.show_failure(
                        save_failed_note(&display_name(&request.path)),
                        note_action::NoteAction::Save,
                    );
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
                    tracing::warn!(%error, "reading the PDF's own markup failed");
                    self.status_note
                        .show(crate::t("pdf-note-existing-markup-unread"));
                    self.dirty = true;
                }
                _ => {}
            }
        }
        if lost_region_render {
            self.ask_again_for_waiting_region();
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

/// A file's name as the person knows it, from its path.
fn display_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What the reader says once a copy with the markup is saved as `name`.
fn saved_note(name: &str) -> String {
    crate::t_args("pdf-note-saved", &[("name", name.into())])
}

/// What the reader says when the copy named `name` could not be saved.
/// Why is the save effect's answer; the person is told what to do.
fn save_failed_note(name: &str) -> String {
    crate::t_args("pdf-note-save-failed", &[("name", name.into())])
}

/// What the reader says when the file at `path` could not be opened over a
/// document it keeps showing.
fn open_failed_note(path: &std::path::Path) -> String {
    crate::t_args(
        "pdf-note-open-failed",
        &[("name", display_name(path).into())],
    )
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
include!("app_scroll_input.rs");

impl Widget for PdfApp {
    ui::provides!(
        dyn ui::widget::Findable,
        dyn ui::widget::controls::ActionControls
    );

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let mut changed = self.follow_document() | self.asked();
        for child in self.children_mut() {
            changed |= child.update(state, path);
        }
        changed
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow_document();
        self.take_up_asked();
        // A progress note stays while its copy or import is under way.
        // So does one while a document opens or a save is written.
        let working = self.pending_copy
            || self.pending_import.is_some()
            || self.region_waiting.is_some()
            || self.pending_open.is_some()
            || self.pending_reopen.is_some()
            || self.pending_document_open.is_some()
            || self.pending_save.is_some()
            || self.save_write_rx.is_some();
        if self.status_note.keep_while(working) | self.status_note.expire() {
            self.dirty = true;
        }
        if self.region_copy.failed() {
            self.status_note.show(crate::t("pdf-note-area-not-copied"));
            self.dirty = true;
        }
        // The note says so in the window's bar while it lasts, as does a
        // page or zoom change.
        self.publish_status_note();
        self.page_notice.expire();
        self.page_notice
            .publish(|line| ToolbarNotice::new(line.clone(), ToastKind::Info));
        let (state, _) = ui::doc::document_ref(state, path);
        if !self.raster_wake_bound
            && let Some(handle) = self.raster_handle.as_ref()
        {
            // Bound from the frame, where the window's wake scope is current.
            handle.set_wake(ui::wake::WakeHandle::new());
            self.raster_wake_bound = true;
        }
        // Drain the dedicated PDF owner before any portal/action work.  This
        // is non-blocking and is the sole route by which production PDFium
        // results enter widget state.
        self.drain_raster_events(state);
        self.copy_waiting_region();
        self.drain_opens();
        self.follow_cancellation();
        // A selection drag held at the canvas's edge scrolls on its own,
        // and a lifted gesture glides.
        self.autoscroll_selection(ui::time::now());
        self.advance_glide(ui::time::now());
        self.follow_reopen();
        self.follow_document_open();
        self.follow_signature();
        self.reopen_pinned_source();
        // Pull in any file the host portal picker returned since the last
        // frame (off-thread; never blocks the UI).
        self.drain_portal();
        self.drain_portal_contents();
        // And any completed save-location choice, flattening markup to it.
        self.drain_portal_save();
        self.drain_save_write();
        // And any completed import (a PDF/image chosen to add as pages).
        self.drain_portal_import();

        if self.has_document {
            if std::mem::take(&mut self.seed_pending) {
                self.seed_record(state);
            }
            self.reconcile_shared_page_plan(state);
        }
        #[cfg(feature = "samples")]
        self.stage_when_ready();
        if std::mem::take(&mut self.staged_import) {
            self.demo_import_image();
        }
        self.reload(state);
        self.sync_reader_markup(state);
        self.restore_reading();
        self.record_reading();
        self.refresh_search_entries(state);
        self.publish();
    }

    fn settle(&mut self, _edits: &mut ui::widget::Edits<'_>, _path: &automerge::ObjId) {
        self.run_window_toolbar();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Keep a user-resized rail proportional when the host changes UI scale.
        self.rail_w *= f64::from(theme.ui_scale / self.layout_theme.ui_scale);
        self.viewport =
            std::mem::take(&mut self.viewport).with_padding(f64::from(theme.tokens.spacing.s3));
        self.layout_theme = theme.clone();
        self.review.scale_for(theme);
        if let Some(Editing {
            input: EditInput::Rich(field),
            ..
        }) = &mut self.editing
        {
            field
                .widget_mut()
                .editor_mut()
                .set_body_size(Self::comment_body_size(theme));
        }
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
        if self.route_review_resize(event, ctx) {
            return;
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
        self.context_menu
            .as_ref()
            .is_some_and(|menu| menu.widget.traps_focus())
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.context_menu
            .as_ref()
            .and_then(|menu| menu.widget.modal_capture())
    }

    /// The context menu floats in the top layer, above the pages and the
    /// panels.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(menu) = self.context_menu.as_mut() {
            menu.widget
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    /// The reader paints its document chrome and pages directly, so expose the
    /// otherwise invisible structure explicitly.  The navigation controls use
    /// the same `PdfEffect` handler as the window bar and Ctrl+K resources;
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
        if !self.has_document {
            return Some(
                AccessNode::new(Role::Window)
                    .with_name(crate::t("pdf-title"))
                    .with_bounds(self.rect)
                    .with_children(self.empty_state_nodes()),
            );
        }

        // The reader's commands are the window bar's slots; what stays on
        // the pages is the review panel's divider.
        let mut controls = Vec::new();
        if !self.overlay_mode {
            controls.extend(self.review.resize_accessibility());
        }

        let marked = self.pages_with_markup();
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
                let mut node = AccessNode::new(Role::ListItem)
                    .with_name(crate::t_args(
                        "pdf-a11y-page",
                        &[("page", (index + 1).to_string().into())],
                    ))
                    .with_action_target(format!("pdf.page:{page_id}"))
                    .with_bounds(bounds)
                    .with_position(index + 1, pages)
                    .with_state(focusable().selected(index + 1 == current));
                if marked.contains(&(index + 1)) {
                    node = node.with_description(crate::t("pdf-a11y-page-markup"));
                }
                node
            })
            .collect();
        let page_list = AccessNode::new(Role::List)
            .with_name(crate::t("pdf-a11y-pages"))
            .with_hashed_key("pdf.pages")
            .with_children(page_nodes);

        let mut content_children = vec![
            AccessNode::new(Role::Heading { level: 1 }).with_name(if self.doc_name.is_empty() {
                crate::t("pdf-empty-the-document")
            } else {
                self.doc_name.clone()
            }),
            AccessNode::new(Role::Document)
                .with_name(crate::t_args(
                    "pdf-a11y-page-of",
                    &[
                        ("current", current.to_string().into()),
                        ("pages", pages.to_string().into()),
                    ],
                ))
                .with_description(crate::t("pdf-a11y-canvas"))
                .with_value(crate::t_args(
                    "pdf-a11y-zoom",
                    &[("zoom", ((self.zoom() * 100.0).round() as u32).into())],
                ))
                .with_bounds(self.canvas_rect)
                .with_state(focusable()),
        ];

        // The window bar's search holds the query and the count; the pages
        // list its matches, each one a place to go.
        if self.finding() {
            let count = self.match_count();
            let result_nodes = (0..count.min(100))
                .map(|index| {
                    let page = self.raster_matches.get(index).map_or(1, |m| m.page);
                    AccessNode::new(Role::ListItem)
                        .with_name(crate::t_args(
                            "pdf-a11y-match",
                            &[
                                ("match", (index + 1).to_string().into()),
                                ("page", page.to_string().into()),
                            ],
                        ))
                        .with_action_target(format!("pdf.match:{index}"))
                        .with_position(index + 1, count)
                        .with_state(focusable().selected(index == self.active_match))
                })
                .collect();
            content_children.push(
                AccessNode::new(Role::List)
                    .with_name(crate::t("pdf-a11y-search-results"))
                    .with_hashed_key("pdf.search.results")
                    .with_children(result_nodes),
            );
        }

        if self.show_sidebar {
            let tabs = self
                .sidebar_tab_rects()
                .into_iter()
                .map(|(tab, bounds)| {
                    AccessNode::new(Role::Tab)
                        .with_name(crate::t(tab.name_key()))
                        .with_action_target(format!("pdf.tab:{}", tab.tag()))
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
                            .with_value(crate::t_args(
                                "pdf-a11y-page",
                                &[("page", item.page.to_string().into())],
                            ))
                            .with_level(usize::from(item.level) + 1)
                            .with_action_target(format!("pdf.outline:{index}"))
                            .with_state(focusable().selected(item.page == current))
                    })
                    .collect(),
                SidebarTab::Comments => self
                    .comments
                    .iter()
                    .map(|comment| {
                        let text = &comment.comment.text;
                        AccessNode::new(Role::ListItem)
                            .with_name(if text.trim().is_empty() {
                                crate::t("pdf-comment-empty")
                            } else {
                                text.clone()
                            })
                            .with_description(crate::t_args(
                                "pdf-a11y-comment",
                                &[
                                    ("author", self.author_name(&comment.comment.author).into()),
                                    ("page", comment.page.into()),
                                    ("replies", comment.comment.replies.len().into()),
                                ],
                            ))
                            .with_action_target(format!("pdf.comment:{}.open", comment.id))
                            .with_state(
                                focusable()
                                    .selected(comment.pinned() == self.selected_anno.as_deref()),
                            )
                            .with_children(vec![
                                button(
                                    &crate::t("pdf-comment-edit"),
                                    &format!("pdf.comment:{}.edit", comment.id),
                                    Rect::ZERO,
                                ),
                                button(
                                    &crate::t("pdf-comment-reply"),
                                    &format!("pdf.comment:{}.reply", comment.id),
                                    Rect::ZERO,
                                ),
                                AccessNode::new(Role::ToggleButton)
                                    .with_name(if comment.comment.resolved {
                                        crate::t("pdf-comment-reopen")
                                    } else {
                                        crate::t("pdf-comment-resolve")
                                    })
                                    .with_action_target(format!(
                                        "pdf.comment:{}.resolve",
                                        comment.id
                                    ))
                                    .with_state(focusable().checked(comment.comment.resolved)),
                                button(
                                    &crate::t("pdf-comment-delete"),
                                    &format!("pdf.comment:{}.delete", comment.id),
                                    Rect::ZERO,
                                ),
                            ])
                    })
                    .collect(),
                SidebarTab::Signatures => {
                    let mut nodes: Vec<AccessNode> = vec![
                        AccessNode::new(Role::TextInput)
                            .with_name(crate::t("pdf-viewer-signing-as"))
                            .with_value(self.signer())
                            .with_action_target("pdf.signer")
                            .with_state(focusable()),
                        AccessNode::new(Role::RadioButton)
                            .with_name(crate::t("pdf-signature-type"))
                            .with_action_target("pdf.signature-mode:type")
                            .with_state(focusable().checked(self.sig_mode == SigMode::Type)),
                        AccessNode::new(Role::RadioButton)
                            .with_name(crate::t("pdf-signature-draw"))
                            .with_action_target("pdf.signature-mode:draw")
                            .with_state(focusable().checked(self.sig_mode == SigMode::Draw)),
                        button(
                            &crate::t("pdf-signature-place"),
                            "pdf.signature.use",
                            Rect::ZERO,
                        ),
                    ];
                    nodes.extend(self.signatures.iter().map(|signature| {
                        AccessNode::new(Role::ListItem)
                            .with_name(crate::t_args(
                                "pdf-a11y-signature-by",
                                &[("signer", signature.signer.clone().into())],
                            ))
                            .with_description(crate::t_args(
                                "pdf-a11y-signed",
                                &[
                                    ("date", crate::dates::shown(&signature.date).into()),
                                    ("page", signature.page.to_string().into()),
                                ],
                            ))
                            .with_action_target(format!("pdf.signature:{}.open", signature.id))
                            .with_state(focusable().selected(
                                self.selected_sig.as_deref() == Some(signature.id.as_str()),
                            ))
                            .with_children(vec![button(
                                &crate::t("pdf-signature-delete"),
                                &format!("pdf.signature:{}.delete", signature.id),
                                Rect::ZERO,
                            )])
                    }));
                    nodes
                }
            };
            content_children.push(
                AccessNode::new(Role::Group)
                    .with_name(crate::t("pdf-a11y-review-sidebar"))
                    .with_children(vec![
                        AccessNode::new(Role::TabBar)
                            .with_name(crate::t("pdf-a11y-tabs"))
                            .with_children(tabs),
                        AccessNode::new(Role::TabPanel)
                            .with_name(crate::t(self.sidebar_tab.name_key()))
                            .with_children(panel_children),
                    ]),
            );
        }
        if let Some(status) = self.status_note.get() {
            content_children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(status)
                    .with_live(Politeness::Polite),
            );
        }
        let mut window_children = vec![
            AccessNode::new(Role::Group)
                .with_name(crate::t("pdf-a11y-panel-resize"))
                .with_hashed_key("pdf.page-controls")
                .with_children(controls),
            page_list,
            AccessNode::new(Role::Group)
                .with_name(crate::t("pdf-a11y-document"))
                .with_hashed_key("pdf.content")
                .with_children(content_children),
        ];
        // The rail's import button: its label when the rail has the room for
        // it, its name when it stands as the glyph alone.
        if self.show_rail {
            let add = self.page_sidebar.add_button_rect();
            if add.width() > 0.0 {
                window_children.insert(1, button(&crate::t("pdf-rail-import"), "pdf.import", add));
            }
        }
        if let Some(menu) = self.context_menu.as_ref()
            && let Some(menu_node) = menu.widget.accessibility()
        {
            window_children.push(menu_node);
        }
        Some(
            AccessNode::new(Role::Window)
                .with_name(crate::t("pdf-title"))
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
        if self.perform_review_resize(target, action) {
            return true;
        }
        let (state, path) = ui::doc::document(state, path);
        if target.starts_with("context-menu:item:") {
            let Some(menu) = self.context_menu.as_mut() else {
                return false;
            };
            let handled = menu
                .widget
                .perform_semantic_action(target, action, state, path);
            let selected = menu.widget.take_selection().map(|_| menu.target.clone());
            let still_open = menu.widget.is_showing();
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
        if target == "pdf.signer" {
            match action {
                A11yAction::Activate | A11yAction::Focus => {
                    let value = self.signer();
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
            let page = self.raster_matches.get(index).map(|hit| hit.page);
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
                    self.selected_anno = comment.pinned().map(str::to_owned);
                    self.go_to_page(comment.page);
                }
                "edit" => {
                    self.begin_edit(EditTarget::CommentBody(id.into()), &comment.comment.text);
                }
                "reply" => self.begin_edit(EditTarget::Reply(id.into()), ""),
                "resolve" => {
                    self.perform_action(
                        state,
                        crate::actions::set_comment_resolved_action(id, !comment.comment.resolved),
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

        if let Some(record) = target.strip_prefix("pdf.empty.document:") {
            return activates && self.open_workspace_document(record);
        }

        if target == "pdf.import" {
            self.request_import_via_portal();
            return true;
        }

        let effect = match target {
            "pdf.open" => PdfEffect::OpenPdf { path: None },
            "pdf.signature.use" => PdfEffect::UseTool { tool: Tool::Sign },
            "pdf.previous-page" => PdfEffect::PreviousPage,
            "pdf.next-page" => PdfEffect::NextPage,
            _ => return false,
        };
        self.run_effect(&effect);
        true
    }

    fn test_id(&self) -> Option<&str> {
        Some("pdf_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The rail's import button, when it stands as its glyph alone, is named
    /// by the words the rail had no room to paint (see `ui::widget::PageSidebar`).
    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.page_sidebar.tooltip_target_at(point)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.on_rail_grip(point) {
            return Some(winit::window::CursorIcon::ColResize);
        }
        if let Some(cursor) = self.review_resize_cursor(point) {
            return Some(cursor);
        }
        self.link_under(point)
            .map(|_| winit::window::CursorIcon::Pointer)
    }

    fn has_pending_visual_work(&self) -> bool {
        let staging = {
            #[cfg(feature = "samples")]
            {
                self.staged_scenario.is_some()
            }
            #[cfg(not(feature = "samples"))]
            {
                false
            }
        };
        self.pending_open.is_some() || self.pending_visuals || staging
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.status_note
            .deadline()
            .into_iter()
            .chain(self.page_notice.deadline())
            .min()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.scrolled
            || self.delivered_thumbs
            || !self.delivered_pages.is_empty()
            || self.drag.is_some()
            || self.pan.is_some()
            || self.rail_drag.is_some()
            || self.review.is_resizing()
    }

    /// See `PdfApp::scroll_damage`.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.scroll_damage(regions);
    }

    fn needs_animation_frame(&self) -> bool {
        // Tick only while something moves on its own, so an idle window
        // settles to zero frames instead of redrawing forever.
        let owner_busy = self.portal_contents_rx.is_some()
            || self.pending_reopen.is_some()
            || self.pending_document_open.is_some()
            || self
                .raster_handle
                .as_ref()
                .is_some_and(crate::source::RasterHandle::wants_frames);
        if self.reduce_motion {
            return owner_busy;
        }
        // Also tick while the PDF owner is busy, so its results are drained
        // and painted without waiting for an input event.
        owner_busy || self.selection_autoscroll_velocity() != 0.0 || self.glide.is_active()
    }

    fn set_interaction_environment(
        &mut self,
        environment: ui::interaction::InteractionEnvironment,
    ) {
        if self.interaction != environment {
            self.interaction = environment;
        }
    }

    /// A finger on a selection grip drags that end of the selection; on
    /// the page it scrolls, as on any page.
    fn touch_claim(&self, point: Point) -> ui::interaction::TouchClaim {
        if self.tool == Tool::Select
            && self
                .selection_grips()
                .is_some_and(|grips| grips.grip_at(point).is_some())
        {
            ui::interaction::TouchClaim::Drag
        } else {
            ui::interaction::TouchClaim::Pointer
        }
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.reduce_motion = reduced;
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.scrolled = false;
        self.delivered_pages.clear();
        self.delivered_thumbs = false;
        self.shown.present();
    }
}

impl PdfApp {
    /// The search entries of the open document; none without one.
    /// The page displayed page `page` (1-based) is an occurrence of: a page
    /// of the opened document, or of an imported file.
    fn page_text_key(&self, page: usize) -> Option<String> {
        let index = page.checked_sub(1)?;
        Some(
            match self.resource_page_imports.get(index).cloned().flatten() {
                Some(imported) => format!("{}#{}", imported.hash, imported.page),
                None => format!("#{}", self.resource_page_sources.get(index)?),
            },
        )
    }

    /// Keep displayed page `page`'s text for the system's search.
    fn keep_page_text(&mut self, page: usize, text: &PageText) {
        let Some(key) = self.page_text_key(page) else {
            return;
        };
        if self.page_text.get(&key) != Some(&text.text) {
            self.page_text.insert(key, text.text.clone());
            self.page_text_revision += 1;
        }
    }

    /// The kept text of every displayed page, in order; empty for a page
    /// not read yet.
    fn displayed_page_text(&self) -> Vec<&str> {
        (1..=self.resource_page_ids.len())
            .map(|page| {
                self.page_text_key(page)
                    .and_then(|key| self.page_text.get(&key))
                    .map_or("", String::as_str)
            })
            .collect()
    }

    /// The system's search is looking: read every page's text the reader
    /// has not read yet, so the open document is found by what it says.
    pub(crate) fn read_text_for_search(&mut self) {
        if !self.raster_active() {
            return;
        }
        self.search_reads_text = true;
        for page in 1..=self.doc.page_count() {
            let read = self
                .page_text_key(page)
                .is_some_and(|key| self.page_text.contains_key(&key));
            if !read && let Some(layer) = self.ensure_raster_text(page) {
                self.keep_page_text(page, &layer);
            }
        }
    }

    /// Whether the text the system's search asked for is still being read.
    pub(crate) fn search_text_pending(&self) -> bool {
        self.search_reads_text && self.raster_active() && !self.raster_text_pending.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn shown_search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        if self.has_document {
            self.search_entries.as_ref().clone()
        } else {
            Vec::new()
        }
    }
}

impl PdfApp {
    /// The open document's live rows: its outline and the current find
    /// hits, read from the PDF and the find bar rather than the record.
    #[cfg(test)]
    pub(crate) fn document_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.pdf_resource_ctx()
            .map(|ctx| crate::actions::live_rows(&ctx))
            .unwrap_or_default()
    }

    /// The live row `id`, beyond the bounded page listed.
    #[cfg(test)]
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
            PdfEffect::Find => Self::open_search(),
            PdfEffect::UseTool { tool } => self.set_tool(*tool),
            PdfEffect::SetMarkerColour { colour } => self.set_marker_colour(*colour),
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
            PdfEffect::SelectAll => {
                self.select_all();
            }
            PdfEffect::OpenPdf { .. }
            | PdfEffect::SaveAs { .. }
            | PdfEffect::SelectText { .. }
            | PdfEffect::CopySelection
            | PdfEffect::ReadText { .. }
            | PdfEffect::ReadFormFields
            | PdfEffect::InsertPages { .. }
            | PdfEffect::LookPage { .. }
            | PdfEffect::ImportPdfContent { .. } => {
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
        path: Option<ui::tilo_resource::AbsolutePath>,
        cx: &mut ui::framework::EffectCx<crate::PdfMarkup>,
    ) -> ui::framework::EffectAnswer {
        if self.portal_action.is_some()
            || self.pending_open.is_some()
            || self.pending_reopen.is_some()
            || self.pending_document_open.is_some()
        {
            return Err("another PDF open is already in progress".into());
        }
        if let Some(path) = path {
            return self.ask_here().open(path.into_path_buf(), cx);
        }
        if tilo_paths::available(tilo_paths::Feature::NativePaths) {
            let ask = self.ask_here();
            return Ok(
                cx.choose_path(pdf_open_request(), move |(), chosen, cx| match chosen {
                    Some(path) => ask.open(path, cx),
                    None => Err("file selection cancelled".into()),
                }),
            );
        }
        self.answer_effect(&PdfEffect::OpenPdf { path: None }, || cx.reply())
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
        let error = open_error(path, &reason);
        if self.has_document {
            self.status_note.show(open_failed_note(path));
        } else {
            self.status_note.dismiss();
            self.load_error = Some(error.clone());
        }
        Err(error)
    }

    /// The PDF the record marks up, read in this process, and its name: the
    /// bytes the record pinned, from the content store, which every window
    /// of the record reads alike, else the file this window opened.
    fn reading_document(&self) -> Result<(String, tilo_pdf::Document), String> {
        // The portable engine is memory-safe and runs in process; reading
        // needs nothing only the sandboxed one draws.
        let engine = tilo_pdf::Engine::portable();
        let pinned = self
            .record_source
            .as_ref()
            .and_then(|source| Some((source, source.bytes(&self.content)?)));
        let (file, document) = match (pinned, self.source_path.as_deref()) {
            (Some((source, bytes)), _) => (source.name.clone(), engine.open_bytes(&bytes)),
            (None, Some(path)) => (path.display().to_string(), engine.open_path(path)),
            (None, None) => {
                return Err(match &self.record_source {
                    Some(source) => format!("{} is not on this device yet", source.name),
                    None => content_import::NO_PDF.to_owned(),
                });
            }
        };
        let document = document.map_err(|error| format!("the PDF cannot be read: {error}"))?;
        Ok((file, document))
    }

    /// The text of pages `from_page..=to_page` (1-based) of the PDF the
    /// record marks up ([`Self::reading_document`]), at most
    /// [`MOST_READ_PAGES`] of them.
    pub(crate) fn read_text(
        &self,
        from_page: Option<usize>,
        to_page: Option<usize>,
    ) -> Result<serde_json::Value, String> {
        let (file, document) = self.reading_document()?;
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
            let text = document.text(page - 1).map_err(|error| {
                crate::CATALOG.t_args(
                    "pdf-viewer-page-unreadable",
                    &[
                        ("page", ui::i18n::ArgValue::Number(page as f64)),
                        ("detail", ui::i18n::ArgValue::Text(error.to_string())),
                    ],
                )
            })?;
            pages.push(crate::model::PdfPageText {
                page,
                text: text.text().trim_end().to_owned(),
            });
        }
        serde_json::to_value(crate::model::PdfText {
            file,
            page_count: count,
            pages,
            next: (last < count).then_some(last + 1),
        })
        .map_err(|error| error.to_string())
    }

    /// A picture of page `page` (1-based) of the PDF the record marks up,
    /// its longest side `size` pixels, kept as content: the answer names
    /// its address, which a caller that sees is shown.
    pub(crate) fn look_page(
        &self,
        page: usize,
        size: Option<u32>,
    ) -> Result<ui::tilo_actions::ActionOutput, String> {
        let size = size.unwrap_or(1024);
        if !(64..=1536).contains(&size) {
            return Err("size must be within 64..=1536".into());
        }
        let (_, document) = self.reading_document()?;
        let count = document.page_count();
        if page == 0 || page > count {
            return Err(format!("the PDF has {count} pages"));
        }
        let page_size = document
            .page_size(page - 1)
            .ok_or_else(|| format!("page {page} cannot be read"))?;
        let longest = page_size.width.max(page_size.height);
        #[allow(clippy::cast_precision_loss, reason = "a picture side of at most 1536")]
        let options = tilo_pdf::RenderOptions {
            scale: if longest > 0.0 {
                size as f32 / longest
            } else {
                1.0
            },
            ..tilo_pdf::RenderOptions::default()
        };
        let bitmap = document
            .render(page - 1, &options)
            .map_err(|error| format!("page {page} cannot be drawn: {error}"))?;
        let png = ui::png::encode_rgba8(bitmap.width(), bitmap.height(), bitmap.pixels())
            .map_err(|error| format!("page {page} cannot be encoded: {error}"))?;
        let hash = self
            .content
            .put(&png)
            .map_err(|error| format!("the picture could not be kept: {error}"))?;
        let address = ui::tilo_actions::content_resource(&hash)
            .ok_or("the picture has no content address")?;
        Ok(ui::tilo_actions::ActionOutput {
            value: Some(serde_json::json!({
                "content": address,
                "page": page,
                "page_count": count,
                "width": bitmap.width(),
                "height": bitmap.height(),
                "format": "png",
            })),
            resources: vec![address],
        })
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
                let file = ensure_pdf_extension(file.to_path_buf());
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

/// Why the PDF at `path` could not be shown here.
fn open_error(path: &std::path::Path, reason: &str) -> String {
    format!("Could not open {}: {reason}", path.display())
}

mod content_import;
mod empty_state;
mod form_read;
mod insert_pages;
mod note_action;
mod scroll_damage;
mod selection;
mod shared;
mod window_bar;
mod workspace_documents;
use scroll_damage::CanvasShown;
use selection::{PendingPress, PendingSelectionEffect, SelectionStart, TextPos, TextSelection};
pub use workspace_documents::WorkspaceDocument;

impl ui::widget::controls::ActionControls for PdfApp {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        ui::widget::semantic_action_bindings(self.accessibility())
    }
}

/// Whether `id` is an opaque participant id (a long hex key) rather than a
/// name someone could read.
fn opaque_participant_id(id: &str) -> bool {
    id.len() >= 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

impl ui::widget::Findable for PdfApp {
    fn find(
        &mut self,
        query: &str,
        options: ui::widget::FindOptions,
    ) -> Vec<ui::widget::FindMatch> {
        self.search_case_sensitive = options.match_case;
        self.search_whole_word = options.whole_word;
        query.clone_into(&mut self.find_query);
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
        let page = self.raster_matches.get(index).map(|m| m.page);
        if let Some(page) = page {
            self.go_to_page(page);
        }
        self.dirty = true;
    }

    fn take_own_find(&mut self) -> Option<ui::widget::OwnFind> {
        self.own_find.take()
    }

    fn clear_find(&mut self) {
        self.find_query.clear();
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

#[cfg(test)]
mod empty_state_tests;
#[cfg(test)]
mod records_tests;

#[cfg(test)]
mod adopt_tests;

#[cfg(test)]
mod links_tests;

#[cfg(test)]
mod scroll_tests;
#[cfg(test)]
mod selection_tests;

#[cfg(test)]
pub(crate) mod tests;
