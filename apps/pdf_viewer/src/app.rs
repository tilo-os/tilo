//! `PdfApp` — the root widget for the PDF reader.
//!
//! It is a single full-window widget that manages its own internal layout
//! (toolbar, thumbnail rail, page canvas, comments/outline/signatures
//! sidebar, status bar, search strip) and draws each region itself. This
//! gives pixel control over the reader chrome, mirroring the custom-draw
//! approach the toolkit uses for things like the status bar, while keeping
//! all durable markup — annotations, comments, signatures — in the shared
//! CRDT so collaboration and undo come for free.

use std::collections::HashSet;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill, ImageData as PenikoImage};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use std::time::Duration;

use ui::presence::PresenceCtx;
use ui::permissions::PermissionContext;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::tooltip::Tooltip;
use ui::widget::page_sidebar::{PageSidebar, PageSidebarEvent};
use ui::widget::{SmallContentAlign, ViewportState};
use ui::Widget;

use crate::doc::PdfDoc;
use crate::model::{
    self, AnnoKind, Annotation, Comment, Document, Match, PtRect, Reply, SearchOptions, Signature,
    Swatch, Tool, MARGIN, PAGE_H, PAGE_W,
};
use crate::render;
use crate::text_layer::{PageText, TextMatch};
use std::sync::Arc;

const TOOLBAR_H: f64 = 56.0;
const STATUS_H: f64 = 28.0;
/// Default width of the thumbnail rail; the user can drag-resize it within
/// [`RAIL_W_MIN`, `RAIL_W_MAX`] and fully collapse it.
const RAIL_W: f64 = 144.0;
const RAIL_W_MIN: f64 = 96.0;
const RAIL_W_MAX: f64 = 280.0;
/// Width of the invisible drag strip on the rail's right edge.
const RAIL_GRIP: f64 = 6.0;
const SIDEBAR_W: f64 = 324.0;
const SEARCH_H: f64 = 46.0;
/// Screen-px padding the viewport reserves around the page stack (top / sides
/// alike). Handed to [`ViewportState::with_padding`] so the shared kernel and
/// the reader agree on where the content origin sits.
const VIEW_PAD: f64 = 28.0;
/// Gap between stacked pages, in **content (page-point) space** — it scales
/// with zoom, exactly like the page it separates, because the whole stack is a
/// single [`ViewportState`] content rect.
const PAGE_GAP: f64 = 22.0;
const ZOOM_MIN: f64 = 0.25;
const ZOOM_MAX: f64 = 6.0;
/// A drag shorter than this (in screen px) is treated as a click, not a pan.
const PAN_SLOP: f64 = 4.0;
/// Below this window width the rail + sidebar become closed-by-default
/// overlays drawn over a full-width canvas instead of reserving columns.
const OVERLAY_BREAKPOINT: f64 = 900.0;
/// Rasterise pages at this multiple of their on-screen size so they stay
/// crisp when the compositor scales the surface (and when zooming in a notch).
const RASTER_SS: f64 = 2.0;
/// Max number of *new* (uncached) full-page rasterisations a single frame may
/// perform. A real page can cost tens-to-hundreds of ms, so capping this keeps
/// the open→first-paint frame to the single top page and every frame cheap;
/// skipped pages fill in over the next frames (`pending_raster`). Cached pages
/// are always drawn (a cache hit is ~free), so steady-state scrolling is
/// unaffected.
const MAX_NEW_PAGE_RASTERS_PER_FRAME: usize = 1;
/// Same budget for rail thumbnails, which are also full pdfium page renders (a
/// scanned page's thumbnail can cost 15-30ms). A couple per frame keeps the
/// rail filling in briskly without the open frame paying for a whole rail-full.
const MAX_NEW_THUMB_RASTERS_PER_FRAME: usize = 2;
/// How close (page points) a highlight drag must come to a text line before
/// it snaps to text instead of falling back to a free-form rectangle.
const SNAP_NEAR: f64 = 18.0;
/// Screen-px a single arrow-key press scrolls the document.
const KEY_SCROLL_STEP: f64 = 90.0;
/// Fixed date stamped on the bundled demonstration signature. It is only
/// ever read by [`PdfApp::seed`], which runs on the screenshot / test path,
/// so the shots stay byte-for-byte deterministic. Signatures placed at
/// runtime read the real system clock via [`today_ymd`] instead.
const SEED_DATE: &str = "2026-06-30";

/// The current UTC date as `YYYY-MM-DD`, read from the system clock. This is
/// the real signing date stamped on a signature the user places at runtime.
fn today_ymd() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    ymd_from_unix_secs(secs)
}

/// Convert a Unix timestamp (seconds) into a UTC `YYYY-MM-DD` string using
/// the civil-from-days algorithm (Howard Hinnant, public domain). Pure and
/// dependency-free so the date logic is unit-testable without a clock.
fn ymd_from_unix_secs(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    // Shift onto a calendar whose era begins 0000-03-01 (day 0 == that date).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day-of-era, [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year, [0, 365]
    let mp = (5 * doy + 2) / 153; // month shifted so March == 0, [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", year, m, d)
}

/// Deterministically pick one of the four bundled handwriting styles for a
/// signer by hashing their name, so each distinct signer gets a stable,
/// distinct stroke and the signer-card preview matches the signature that
/// will actually be placed. An empty name falls back to style 0.
fn signature_style_for(name: &str) -> u8 {
    let name = name.trim();
    if name.is_empty() {
        return 0;
    }
    // FNV-1a over the name bytes — stable across runs and across peers, so a
    // collaborator sees the same style for the same signer.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h % 4) as u8
}

#[derive(Clone, Copy, PartialEq, Eq)]
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

/// A text selection over the real pdfium text layer (raster backend only). The
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

#[derive(Clone)]
struct ContextMenu {
    at: Point,
    target: DeleteTarget,
    /// Captured during draw so the click handler knows the menu-item rect.
    item_rect: Rect,
}

#[derive(Clone)]
enum ToolbarHit {
    Tool(Tool),
    Swatch(Swatch),
    ZoomIn,
    ZoomOut,
    Fit,
    Search,
    ToggleSidebar,
    ToggleRail,
    Open,
    Save,
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
}

#[derive(Clone)]
enum SearchHit {
    ToggleWholeWord,
    ToggleCaseSensitive,
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
}

/// A live inline text edit. The buffer is local until committed so a burst
/// of keystrokes is a single undo step rather than one per character.
#[derive(Clone)]
struct Editing {
    target: EditTarget,
    buffer: String,
    /// Caret position as a byte offset into `buffer`.
    caret: usize,
}

pub struct PdfApp {
    doc: Document,
    /// Real page rasteriser (pdfium). `None` means the text backend is in use
    /// (or no document is open).
    raster: Option<crate::source::Rasterizer>,
    /// False until a real file has been loaded — drives the empty state.
    has_document: bool,
    /// The opened file's display name, shown in the chrome.
    doc_name: String,
    /// A human-readable reason the last open failed, if any.
    load_error: Option<String>,
    /// Seed demonstration markup on first sync. Only ever set on the
    /// screenshot / test path, never for a real document.
    seed_sample: bool,
    /// Screenshot-only: drop the caret into the first comment on the first
    /// sync so a still frame can show the inline editor. Never set at runtime.
    shots_edit_first_comment: bool,
    /// Screenshot-only: arm a live page-rail reorder drag each frame so a still
    /// frame captures the drag ghost + reflow gap. Never set at runtime.
    shots_rail_drag: bool,

    // CRDT-backed caches (refreshed from the shared document each frame).
    annotations: Vec<Annotation>,
    comments: Vec<Comment>,
    signatures: Vec<Signature>,
    seeded: bool,

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
    tool: Tool,
    swatch: Swatch,
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
    portal_import_rx: Option<crate::portal::PortalChannel>,
    selected_anno: Option<String>,
    /// Active inline text edit (comment body, thread reply, or signer name).
    editing: Option<Editing>,
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

    // Search.
    search_active: bool,
    query: String,
    matches: Vec<Match>,
    active_match: usize,
    search_whole_word: bool,
    search_case_sensitive: bool,
    /// Raster-backend search hits over the real pdfium text layer. Used instead
    /// of `matches` (which is the text backend's block model) when a real PDF
    /// is open, so search highlights land on genuine glyph rectangles.
    raster_matches: Vec<TextMatch>,

    // Real PDF text layer (raster backend) — glyph geometry per visible page,
    // refreshed each draw, powering selection + search-hit rectangles. Empty in
    // the text backend, where the block model handles both.
    raster_text: std::collections::HashMap<usize, Arc<PageText>>,
    /// A live text selection over the raster text layer, if any.
    selection: Option<TextSelection>,
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
    /// An in-flight XDG-portal *save* request (mirrors `portal_rx`).
    portal_save_rx: Option<crate::portal::PortalChannel>,

    // Interaction transients.
    last_mouse: Point,
    drag: Option<DragState>,
    pan: Option<PanState>,
    /// Some(pointer-x-at-press) while dragging the rail's resize grip.
    rail_drag: Option<f64>,
    context_menu: Option<ContextMenu>,
    created_counter: i64,
    commit_pending: bool,
    dirty: bool,
    /// Set by `rasterize_visible` when it hit the per-frame new-raster budget
    /// and still has uncached visible pages to render. Keeps `needs_redraw`
    /// true so those pages fill in over the next frame(s) instead of stalling
    /// the current one — the open→first-paint path renders only the top page.
    pending_raster: bool,

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

    /// An in-flight XDG-portal file-open. When the user asks to open a
    /// document (toolbar folder button, Ctrl+O, or the empty state's Open
    /// button) the portal round-trip runs on a worker thread and hands the
    /// chosen path back over this channel, which is drained once per frame in
    /// [`Widget::sync`]. `None` when no picker is in flight — the app never
    /// draws its own picker (HIG §5).
    portal_rx: Option<crate::portal::PortalChannel>,

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
    status_rect: Rect,

    // Sidebar + search content hit-targets, captured during draw.
    sidebar_hits: Vec<(SidebarHit, Rect)>,
    search_hits: Vec<(SearchHit, Rect)>,
    /// Screen rect of the active inline editor, captured during draw so a
    /// click inside it keeps focus while a click outside commits.
    edit_rect: Rect,

    /// The shared reader selection HUD, rebuilt each frame while a text
    /// selection is live and hit-tested on the next press. The identical bar
    /// the ebook reader draws — Copy / highlight swatches / Note.
    sel_hud: Option<ui::SelectionHud>,
    /// An in-progress drag of a sticky-note marker (reposition), if any.
    marker_drag: Option<ui::MarkerDrag>,
}

impl PdfApp {
    /// Open a real PDF from `path`, rasterising via pdfium when available and
    /// falling back to pure-Rust text extraction. A failure lands in the empty
    /// state with the error surfaced, rather than crashing.
    pub fn open(path: impl AsRef<std::path::Path>) -> Self {
        let path = path.as_ref();
        match crate::source::open(path) {
            Ok(loaded) => {
                let mut app = Self::build(loaded.doc, false);
                app.raster = loaded.raster;
                app.has_document = true;
                app.doc_name = loaded.name;
                app.source_path = Some(path.to_path_buf());
                app
            }
            Err(e) => {
                let mut app = Self::empty();
                app.load_error = Some(format!("Could not open {}: {e}", path.display()));
                app
            }
        }
    }

    /// The no-document state: a clean "open a PDF" surface, no fabricated
    /// content.
    pub fn empty() -> Self {
        Self::build(model::Document::blank_pages("", "", 1, PAGE_W, PAGE_H), false)
    }

    /// The bundled demonstration document with example markup seeded on first
    /// sync. Used only by the screenshot harness and tests.
    pub fn sample() -> Self {
        let mut app = Self::build(model::sample_document(), true);
        app.has_document = true;
        app.doc_name = "qOS Technical Overview.pdf".to_string();
        app.signer_name = "Alice Daskju".to_string();
        app
    }

    /// Screenshot-only: the sample document with the first comment already in
    /// edit mode, so a still frame captures the inline editor (caret + focus
    /// ring). Only reachable from the shots harness.
    pub fn sample_editing() -> Self {
        let mut app = Self::sample();
        app.shots_edit_first_comment = true;
        app
    }

    /// Back-compat constructor: the empty state.
    pub fn new() -> Self {
        Self::empty()
    }

    /// Screenshot-only: arm a live page-rail reorder drag so a still frame shows
    /// the drag ghost + reflow gap. The rail is forced open.
    pub fn arm_shots_rail_drag(&mut self) {
        self.shots_rail_drag = true;
        self.show_rail = true;
    }

    /// Screenshot-only: stage a settled text selection over page 1 (a synthetic
    /// glyph layer so it needs no native pdfium) so a still frame shows the
    /// shared selection HUD — highlight swatches + Copy + Note — over the page.
    pub fn arm_shots_selection(&mut self) {
        let text = "the keeper climbed the iron stair and set the great wick alight";
        let cols = 30usize;
        let glyphs: Vec<(char, PtRect)> = text
            .chars()
            .enumerate()
            .map(|(i, c)| {
                let (row, col) = (i / cols, i % cols);
                (c, PtRect::new(70.0 + col as f64 * 12.5, 150.0 + row as f64 * 22.0, 10.5, 15.0))
            })
            .collect();
        let n = glyphs.len();
        self.raster_text.insert(1, std::sync::Arc::new(PageText::from_glyphs(glyphs)));
        self.selection = Some(TextSelection { page: 1, anchor: 0, caret: n.saturating_sub(1), dragging: false });
        self.show_sidebar = true;
    }

    /// Screenshot / demo helper: import a distinctive generated image as a new
    /// page after the current one, so a still frame shows an imported image page
    /// in the rail + canvas. No-op on the text backend.
    pub fn demo_import_image(&mut self) {
        self.show_rail = true;
        if self.raster.is_none() {
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
        let png = std::env::temp_dir().join("qos_pdf_demo_import.png");
        if image::DynamicImage::ImageRgba8(img).save(&png).is_ok() {
            self.import_file(&png);
        }
    }

    fn build(doc: Document, seed_sample: bool) -> Self {
        let author = std::env::var("QUICK_APP_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "You".to_string());
        Self {
            doc,
            raster: None,
            has_document: false,
            doc_name: String::new(),
            load_error: None,
            seed_sample,
            shots_edit_first_comment: false,
            shots_rail_drag: false,
            annotations: Vec::new(),
            comments: Vec::new(),
            signatures: Vec::new(),
            seeded: false,
            rect: Rect::ZERO,
            fit: FitMode::Page,
            viewport: ViewportState::new()
                .with_zoom_bounds(ZOOM_MIN, ZOOM_MAX)
                .with_padding(VIEW_PAD)
                .with_alignment(SmallContentAlign::Center, SmallContentAlign::Start),
            current_page: 1,
            tool: Tool::Select,
            swatch: Swatch::Yellow,
            sidebar_tab: SidebarTab::Comments,
            show_sidebar: true,
            show_rail: true,
            rail_w: RAIL_W,
            page_sidebar: PageSidebar::new(PAGE_H / PAGE_W)
                .with_header("PAGES")
                .with_add("Import…"),
            pages_dirty: false,
            portal_import_rx: None,
            selected_anno: None,
            editing: None,
            selected_sig: None,
            sidebar_scroll: 0.0,
            sidebar_reveal: None,
            signer_name: author.clone(),
            author,
            search_active: false,
            query: String::new(),
            matches: Vec::new(),
            active_match: 0,
            search_whole_word: false,
            search_case_sensitive: false,
            raster_matches: Vec::new(),
            raster_text: std::collections::HashMap::new(),
            selection: None,
            region: None,
            shift_down: false,
            source_path: None,
            status_note: None,
            portal_save_rx: None,
            last_mouse: Point::ZERO,
            drag: None,
            pan: None,
            rail_drag: None,
            context_menu: None,
            created_counter: 100,
            commit_pending: false,
            dirty: true,
            pending_raster: false,
            text_lines: std::collections::HashMap::new(),
            tooltip: Tooltip::new("").with_delay(Duration::from_millis(350)),
            tooltip_anchor: Rect::ZERO,
            tooltip_pending: false,
            portal_rx: None,
            overlay_mode: false,
            prev_overlay: None,
            toolbar_rect: Rect::ZERO,
            search_rect: Rect::ZERO,
            rail_rect: Rect::ZERO,
            canvas_rect: Rect::ZERO,
            sidebar_rect: Rect::ZERO,
            status_rect: Rect::ZERO,
            sidebar_hits: Vec::new(),
            search_hits: Vec::new(),
            edit_rect: Rect::ZERO,
            sel_hud: None,
            marker_drag: None,
        }
    }

    // ---- seeding + cache refresh ------------------------------------

    fn seed(&mut self, state: &mut AutoCommit) {
        PdfDoc::set_meta(state, &self.doc.title, &self.doc.author);

        // Two highlights over the abstract, a sticky note, and a placed review
        // signature — all on page 1 so the default screenshot shows real
        // review markup anchored to real text (no free-floating scribble over
        // empty whitespace).
        let highlight = Annotation {
            id: Annotation::new_id(),
            page: 1,
            kind: AnnoKind::Highlight { rect: PtRect::new(64.0, 196.0, 420.0, 14.0) },
            color: Swatch::Yellow,
            author: "Bob".into(),
        };
        let highlight2 = Annotation {
            id: Annotation::new_id(),
            page: 1,
            kind: AnnoKind::Highlight { rect: PtRect::new(64.0, 210.0, 300.0, 14.0) },
            color: Swatch::Yellow,
            author: "Bob".into(),
        };
        let note = Annotation {
            id: Annotation::new_id(),
            page: 1,
            kind: AnnoKind::Note { x: 506.0, y: 300.0 },
            color: Swatch::Blue,
            author: "Alice".into(),
        };
        let sig = Signature {
            id: Signature::new_id(),
            page: 1,
            rect: PtRect::new(318.0, 612.0, 218.0, 74.0),
            signer: "Alice Daskju".into(),
            date: SEED_DATE.into(),
            style: signature_style_for("Alice Daskju"),
        };

        let note_id = note.id.clone();
        let hl_id = highlight.id.clone();
        for a in [&highlight, &highlight2, &note] {
            PdfDoc::add_annotation(state, a);
        }
        PdfDoc::add_signature(state, &sig);

        // This comment stands in for the one the user "just placed": the
        // demo view-state below selects its note and reveals it in the rail.
        let focus_comment = Comment::new_id();
        PdfDoc::add_comment(
            state,
            &Comment {
                id: focus_comment.clone(),
                anno_id: Some(note_id.clone()),
                page: 1,
                author: "Alice".into(),
                body: "Should we spell out the relay fallback path here? mDNS only covers the local segment.".into(),
                created: 1,
                resolved: false,
                replies: vec![
                    Reply { author: "Bob".into(), body: "Good catch — folded it into section 1.2.".into() },
                    Reply { author: "Alice".into(), body: "Perfect, thanks.".into() },
                ],
            },
        );
        PdfDoc::add_comment(
            state,
            &Comment {
                id: Comment::new_id(),
                anno_id: Some(hl_id),
                page: 1,
                author: "Bob".into(),
                body: "This sentence is the thesis of the whole paper — let's keep it verbatim.".into(),
                created: 2,
                resolved: false,
                replies: vec![],
            },
        );
        PdfDoc::add_comment(
            state,
            &Comment {
                id: Comment::new_id(),
                anno_id: None,
                page: 1,
                author: "Carol".into(),
                body: "Typography reads cleanly at fit-page zoom. Approved.".into(),
                created: 3,
                resolved: true,
                replies: vec![],
            },
        );

        // Land the demonstration view on the note's comment thread so the
        // screenshot shows the review workflow; zoom stays at fit-page so no
        // content is clipped.
        self.selected_anno = Some(note_id);
        self.sidebar_tab = SidebarTab::Comments;
        self.sidebar_reveal = Some(focus_comment);
    }

    fn reload(&mut self, state: &AutoCommit) {
        self.annotations = PdfDoc::list_annotations(state);
        self.comments = PdfDoc::list_comments(state);
        self.signatures = PdfDoc::list_signatures(state);
    }

    // ---- geometry ---------------------------------------------------

    fn rail_width(&self) -> f64 {
        if self.show_rail { self.rail_w.clamp(RAIL_W_MIN, RAIL_W_MAX) } else { 0.0 }
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
        let overlay = self.rect.width() < OVERLAY_BREAKPOINT;
        self.overlay_mode = overlay;
        if self.prev_overlay != Some(overlay) {
            self.show_sidebar = !overlay;
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
        if self.overlay_mode {
            SIDEBAR_W.min((self.rect.width() - 56.0).max(200.0))
        } else {
            SIDEBAR_W
        }
    }

    fn recompute_regions(&mut self) {
        let r = self.rect;
        self.toolbar_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOOLBAR_H);
        self.status_rect = Rect::new(r.x0, r.y1 - STATUS_H, r.x1, r.y1);
        let mut mid_top = r.y0 + TOOLBAR_H;
        if self.search_active {
            self.search_rect = Rect::new(r.x0, mid_top, r.x1, mid_top + SEARCH_H);
            mid_top += SEARCH_H;
        } else {
            self.search_rect = Rect::ZERO;
        }
        let mid_bottom = r.y1 - STATUS_H;
        let rail_w = self.rail_width();
        let sidebar_w = self.sidebar_width();
        self.rail_rect = Rect::new(r.x0, mid_top, r.x0 + rail_w, mid_bottom);
        self.sidebar_rect = Rect::new(r.x1 - sidebar_w, mid_top, r.x1, mid_bottom);
        if self.overlay_mode {
            // Panels float; the canvas spans the whole mid strip beneath them.
            self.canvas_rect = Rect::new(r.x0, mid_top, r.x1, mid_bottom);
        } else {
            self.canvas_rect = Rect::new(r.x0 + rail_w, mid_top, r.x1 - sidebar_w, mid_bottom);
        }
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
        self.viewport.set_content_size(self.pw(), self.content_stack_h());
        self.recompute_zoom();
    }

    /// The zoom that fits one whole page inside the canvas (both axes), the
    /// Fit-page target. Mirrors `ViewportState::fit_zoom` but against a single
    /// page rather than the whole stack.
    fn fit_page_zoom(&self) -> f64 {
        let r = self.viewport.rect();
        let avail_w = r.width() - 2.0 * VIEW_PAD;
        let avail_h = r.height() - 2.0 * VIEW_PAD;
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
            .map(|i| self.viewport.content_point_to_screen(Point::new(0.0, i as f64 * unit)))
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
    fn apply_sidebar_events(&mut self, _state: &mut AutoCommit) {
        for ev in self.page_sidebar.take_events() {
            match ev {
                PageSidebarEvent::Select(i) => self.go_to_page(i + 1),
                PageSidebarEvent::Reorder { from, to } => self.reorder_pages(from, to),
                PageSidebarEvent::Duplicate(i) => self.duplicate_page_at(i),
                PageSidebarEvent::Delete(i) => self.delete_page_at(i),
                PageSidebarEvent::Add => self.request_import_via_portal(),
            }
        }
    }

    /// Rebuild the page list to match the (possibly edited) backend page set and
    /// flag that a save is needed. In raster mode the real page identities live
    /// in the rasteriser's plan and `doc.pages` are size placeholders; in text
    /// mode `doc.pages` already carries the reordered content and only needs
    /// renumbering.
    fn after_page_edit(&mut self) {
        if let Some(rz) = self.raster.as_ref() {
            let n = rz.page_count().max(1);
            self.doc.pages = (1..=n)
                .map(|num| model::Page { number: num, blocks: Vec::new() })
                .collect();
        } else {
            for (i, p) in self.doc.pages.iter_mut().enumerate() {
                p.number = i + 1;
            }
        }
        self.pages_dirty = true;
        self.dirty = true;
    }

    /// Whether the document's pages have been edited (reorder / delete /
    /// duplicate / import) since it was last saved.
    pub fn pages_dirty(&self) -> bool {
        self.pages_dirty
    }

    /// Move displayed page `from` to final index `to`.
    pub fn reorder_pages(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        if let Some(rz) = self.raster.as_mut() {
            rz.reorder_page(from, to);
        } else {
            ui::widget::page_sidebar::move_item(&mut self.doc.pages, from, to);
        }
        self.after_page_edit();
        self.go_to_page(to + 1);
        self.status_note = Some("Page moved — Save to keep the new order".to_string());
    }

    /// Duplicate displayed page `i`, inserting the copy right after it.
    pub fn duplicate_page_at(&mut self, i: usize) {
        if let Some(rz) = self.raster.as_mut() {
            rz.duplicate_page(i);
        } else if i < self.doc.pages.len() {
            ui::widget::page_sidebar::duplicate_after(&mut self.doc.pages, i);
        }
        self.after_page_edit();
        self.go_to_page(i + 2);
        self.status_note = Some("Page duplicated — Save to keep it".to_string());
    }

    /// Remove displayed page `i` (never the last remaining page).
    pub fn delete_page_at(&mut self, i: usize) {
        if self.doc.page_count() <= 1 {
            return;
        }
        if let Some(rz) = self.raster.as_mut() {
            rz.delete_page(i);
        } else if i < self.doc.pages.len() {
            self.doc.pages.remove(i);
        }
        self.after_page_edit();
        self.go_to_page((i + 1).min(self.doc.page_count()));
        self.status_note = Some("Page removed — Save to keep the change".to_string());
    }

    /// Import a PDF or image file as new page(s) after the current page (raster
    /// backend only). Returns true on success.
    pub fn import_file(&mut self, path: &std::path::Path) -> bool {
        if self.raster.is_none() {
            self.status_note = Some("Importing pages needs the pdfium backend".to_string());
            self.dirty = true;
            return false;
        }
        let at = self.current_page; // 0-based index of the slot after the current page
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                self.status_note = Some(format!("Couldn't read {}: {e}", path.display()));
                self.dirty = true;
                return false;
            }
        };
        let is_pdf = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| e.eq_ignore_ascii_case("pdf"))
            .unwrap_or(false)
            || bytes.starts_with(b"%PDF");
        let rz = self.raster.as_mut().unwrap();
        let result = if is_pdf {
            rz.import_pdf(bytes, at).map(|n| format!("Imported {n} page(s) — Save to keep them"))
        } else {
            rz.import_image(&bytes, at)
                .map(|_| "Imported image as a page — Save to keep it".to_string())
        };
        match result {
            Ok(note) => {
                self.after_page_edit();
                self.go_to_page(at + 1);
                self.status_note = Some(note);
                true
            }
            Err(e) => {
                self.status_note = Some(format!("Import failed: {e}"));
                self.dirty = true;
                false
            }
        }
    }

    fn request_import_via_portal(&mut self) {
        if self.raster.is_none() {
            self.status_note = Some("Importing pages needs the pdfium backend".to_string());
            self.dirty = true;
            return;
        }
        if self.portal_import_rx.is_some() {
            return;
        }
        self.portal_import_rx = Some(crate::portal::request_import());
        self.dirty = true;
    }

    fn drain_portal_import(&mut self) {
        let Some(rx) = self.portal_import_rx.as_ref() else { return };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_import_rx = None;
                match outcome {
                    crate::portal::PortalOutcome::Opened(path) => {
                        self.import_file(&path);
                    }
                    crate::portal::PortalOutcome::Cancelled => {}
                    crate::portal::PortalOutcome::Failed(reason) => {
                        self.status_note = Some(format!("Couldn't open the import dialog: {reason}"));
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

    fn recompute_matches(&mut self) {
        let opts = self.search_options();
        self.active_match = 0;
        if self.raster.is_some() {
            // Raster backend: search the real pdfium text layer of every page.
            self.matches.clear();
            let mut hits = Vec::new();
            let n = self.doc.page_count();
            for i in 0..n {
                let pt = self.raster.as_mut().unwrap().page_text(i);
                hits.extend(pt.search(i + 1, &self.query, opts));
            }
            self.raster_matches = hits;
            if let Some(m) = self.raster_matches.first() {
                self.go_to_page(m.page);
            }
        } else {
            // Text backend: search the reflowed block model.
            self.raster_matches.clear();
            self.matches = model::search(&self.doc, &self.query, opts);
            if let Some(m) = self.matches.first() {
                self.go_to_page(m.page);
            }
        }
        self.dirty = true;
    }

    /// The number of live search hits, across whichever backend is active.
    fn match_count(&self) -> usize {
        if self.raster.is_some() {
            self.raster_matches.len()
        } else {
            self.matches.len()
        }
    }

    fn step_match(&mut self, delta: isize) {
        let n = self.match_count();
        if n == 0 {
            return;
        }
        let n = n as isize;
        let mut idx = self.active_match as isize + delta;
        idx = ((idx % n) + n) % n;
        self.active_match = idx as usize;
        let page = if self.raster.is_some() {
            self.raster_matches[self.active_match].page
        } else {
            self.matches[self.active_match].page
        };
        self.go_to_page(page);
    }

    fn matched_blocks_for(&self, page: usize) -> (HashSet<usize>, Option<usize>) {
        let mut set = HashSet::new();
        let mut active = None;
        for (i, m) in self.matches.iter().enumerate() {
            if m.page == page {
                set.insert(m.block);
                if i == self.active_match {
                    active = Some(m.block);
                }
            }
        }
        (set, active)
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
    fn begin_selection(&mut self, page: usize, pt: (f64, f64)) -> bool {
        let Some(layer) = self.ensure_raster_text(page) else { return false };
        let Some(idx) = layer.char_at(pt.0, pt.1) else { return false };
        self.selection = Some(TextSelection { page, anchor: idx, caret: idx, dragging: true });
        self.dirty = true;
        true
    }

    /// The pdfium text layer for 1-based `page`, extracting it on demand and
    /// caching it into `raster_text` if the (deferred) per-frame refresh hasn't
    /// built it yet. This lets a text selection start on a freshly-opened or
    /// just-scrolled-to page without the reader having eagerly paid the
    /// extraction cost on every prior frame. `None` when no raster backend.
    fn ensure_raster_text(&mut self, page: usize) -> Option<Arc<PageText>> {
        if let Some(layer) = self.raster_text.get(&page) {
            return Some(layer.clone());
        }
        let layer = self.raster.as_mut()?.page_text(page - 1);
        self.raster_text.insert(page, layer.clone());
        Some(layer)
    }

    /// Extend the live selection to the pointer at page-point `pt`. Only tracks
    /// motion on the selection's own page (selection stays single-page).
    fn update_selection(&mut self, page: usize, pt: (f64, f64)) {
        let Some(sel) = self.selection else { return };
        if !sel.dragging || sel.page != page {
            return;
        }
        let Some(layer) = self.raster_text.get(&page) else { return };
        if let Some(idx) = layer.char_at(pt.0, pt.1).or_else(|| layer.nearest_char(pt.0, pt.1)) {
            if let Some(s) = self.selection.as_mut() {
                s.caret = idx;
            }
            self.dirty = true;
        }
    }

    /// End the drag. A selection that never grew past its anchor (a bare click)
    /// is dropped so a single click doesn't leave a one-glyph highlight behind.
    fn finish_selection(&mut self) {
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
            self.status_note = Some(format!("Copied {n} character{}", if n == 1 { "" } else { "s" }));
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
        let Some(region) = self.region else { return false };
        let rect = region.rect();
        if rect.w < 3.0 || rect.h < 3.0 {
            return false;
        }
        let page_idx = region.page.saturating_sub(1);
        let (pgw, pgh) = self
            .raster
            .as_ref()
            .map(|r| r.page_size(page_idx))
            .unwrap_or((self.pw(), self.ph()));
        // Render (or reuse) the page at ~2x so the crop is crisp.
        let bucket = ((pgw * 2.0).round() as u32).clamp(96, 4096);
        let Some(img) = self.raster.as_mut().and_then(|r| r.render(page_idx, bucket)) else {
            return false;
        };
        let Some((w, h, rgba)) = render::crop_page_region(&img, pgw, pgh, rect) else {
            return false;
        };
        let copied = ui::clipboard_copy_image(w, h, &rgba);
        self.status_note = Some(if copied {
            format!("Copied a {w}×{h} image region")
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
        // Saving flattens via pdfium; the text backend has no library loaded.
        if self.raster.is_none() {
            self.status_note = Some("Saving annotations needs the pdfium backend".to_string());
            self.dirty = true;
            return;
        }
        if self.portal_save_rx.is_some() {
            return;
        }
        self.portal_save_rx = Some(crate::portal::request_save(self.suggested_save_name()));
        self.dirty = true;
    }

    /// Drain a completed portal *save* request, if any, and flatten the current
    /// markup to the chosen path. Called once per frame from [`Widget::sync`].
    fn drain_portal_save(&mut self) {
        let Some(rx) = self.portal_save_rx.as_ref() else { return };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_save_rx = None;
                match outcome {
                    crate::portal::PortalOutcome::Opened(path) => {
                        let path = ensure_pdf_extension(path);
                        self.save_annotated_to(&path);
                    }
                    crate::portal::PortalOutcome::Cancelled => {}
                    crate::portal::PortalOutcome::Failed(reason) => {
                        self.status_note = Some(format!("Couldn't open the save dialog: {reason}"));
                        self.dirty = true;
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
        if self.raster.is_none() {
            self.status_note = Some("Saving annotations needs the pdfium backend".to_string());
            self.dirty = true;
            return false;
        }
        // Scope the (immutable) raster borrow tightly so the toast update below
        // can take its mutable borrow of `self`.
        let result = self.raster.as_ref().unwrap().save_annotated(
            &self.annotations,
            &self.comments,
            &self.signatures,
            path,
        );
        match result {
            Ok(()) => {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("the file")
                    .to_string();
                self.status_note = Some(format!("Saved annotated PDF to {name}"));
                self.pages_dirty = false;
                self.dirty = true;
                true
            }
            Err(e) => {
                self.status_note = Some(format!("Save failed: {e}"));
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
        let Some(lines) = self.text_lines.get(&page) else { return Vec::new() };
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
            // Horizontal overlap with the line's glyph extent.
            let x0 = drag.x.max(lr.x0);
            let x1 = drag_right.min(lr.x1);
            // Near-horizontal proximity still counts so a slightly-off drag
            // snaps to the whole line rather than dropping it.
            let h_overlap = x1 - x0;
            let h_gap = (lr.x0 - drag_right).max(drag.x - lr.x1).max(0.0);
            if h_overlap <= 1.0 && h_gap > SNAP_NEAR {
                continue;
            }
            let (bx0, bx1) = if h_overlap > 1.0 { (x0, x1) } else { (lr.x0, lr.x1) };
            bands.push(PtRect::new(bx0, line.y, (bx1 - bx0).max(2.0), line.h));
        }
        bands
    }

    fn commit_drag(&mut self, state: &mut AutoCommit) {
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
                let snapped = self.snap_highlights(d.page, drag_rect);
                let rects = if snapped.is_empty() { vec![drag_rect] } else { snapped };
                let mut last_id = None;
                for r in rects {
                    let a = Annotation {
                        id: Annotation::new_id(),
                        page: d.page,
                        kind: AnnoKind::Highlight { rect: r },
                        color: self.swatch,
                        author: self.author.clone(),
                    };
                    last_id = Some(a.id.clone());
                    PdfDoc::add_annotation(state, &a);
                }
                self.selected_anno = last_id;
                self.commit_pending = true;
            }
            Tool::Ink => {
                if d.points.len() >= 2 {
                    let a = Annotation {
                        id: Annotation::new_id(),
                        page: d.page,
                        kind: AnnoKind::Ink { points: d.points.clone() },
                        color: self.swatch,
                        author: self.author.clone(),
                    };
                    PdfDoc::add_annotation(state, &a);
                    self.commit_pending = true;
                }
            }
            _ => {}
        }
        self.reload(state);
        self.dirty = true;
    }

    fn create_note(&mut self, state: &mut AutoCommit, page: usize, pt: (f64, f64)) {
        let a = Annotation {
            id: Annotation::new_id(),
            page,
            kind: AnnoKind::Note { x: pt.0, y: pt.1 },
            color: self.swatch,
            author: self.author.clone(),
        };
        let anno_id = a.id.clone();
        PdfDoc::add_annotation(state, &a);
        self.created_counter += 1;
        let comment_id = Comment::new_id();
        PdfDoc::add_comment(
            state,
            &Comment {
                id: comment_id.clone(),
                anno_id: Some(anno_id.clone()),
                page,
                author: self.author.clone(),
                body: String::new(),
                created: self.created_counter,
                resolved: false,
                replies: vec![],
            },
        );
        self.selected_anno = Some(anno_id);
        self.selected_sig = None;
        // Open the Comments rail, scroll the brand-new thread into view, and
        // drop the caret straight into it so the user just types.
        self.focus_sidebar(SidebarTab::Comments, comment_id.clone());
        self.commit_pending = true;
        self.reload(state);
        self.begin_edit(EditTarget::CommentBody(comment_id), String::new());
        self.dirty = true;
    }

    // ---- inline text editing ----------------------------------------

    /// Start editing `target`, seeding the buffer with its current text and
    /// dropping the caret at the end. Any in-flight edit is dropped without
    /// committing — callers that need to preserve it commit first.
    fn begin_edit(&mut self, target: EditTarget, initial: String) {
        let caret = initial.len();
        self.editing = Some(Editing { target, buffer: initial, caret });
        self.dirty = true;
    }

    /// Whether `target` is the edit currently in progress.
    fn is_editing(&self, target: &EditTarget) -> bool {
        self.editing.as_ref().map(|e| &e.target == target).unwrap_or(false)
    }

    fn edit_insert(&mut self, s: &str) {
        if let Some(e) = self.editing.as_mut() {
            let filtered: String = s.chars().filter(|c| !c.is_control()).collect();
            if filtered.is_empty() {
                return;
            }
            e.buffer.insert_str(e.caret, &filtered);
            e.caret += filtered.len();
            self.dirty = true;
        }
    }

    fn edit_backspace(&mut self) {
        if let Some(e) = self.editing.as_mut() {
            if e.caret == 0 {
                return;
            }
            // Remove the char boundary immediately before the caret.
            let prev = e.buffer[..e.caret]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            e.buffer.replace_range(prev..e.caret, "");
            e.caret = prev;
            self.dirty = true;
        }
    }

    fn edit_move(&mut self, dir: isize) {
        if let Some(e) = self.editing.as_mut() {
            if dir < 0 {
                e.caret = e.buffer[..e.caret].char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
            } else if e.caret < e.buffer.len() {
                let next = e.buffer[e.caret..].char_indices().nth(1).map(|(i, _)| e.caret + i).unwrap_or(e.buffer.len());
                e.caret = next;
            }
            self.dirty = true;
        }
    }

    fn edit_home_end(&mut self, end: bool) {
        if let Some(e) = self.editing.as_mut() {
            e.caret = if end { e.buffer.len() } else { 0 };
            self.dirty = true;
        }
    }

    /// Copy the whole field to the system clipboard. The inline editor has no
    /// selection model, so the copyable unit is the entire buffer. Backed by
    /// the shared `ui::clipboard_copy` (a silent no-op when the platform
    /// clipboard is unavailable, exactly like the toolkit's own text input).
    fn edit_clipboard_copy(&self) {
        if let Some(e) = self.editing.as_ref() {
            if !e.buffer.is_empty() {
                ui::clipboard_copy(&e.buffer);
            }
        }
    }

    /// Cut: copy the whole field to the clipboard, then clear it.
    fn edit_clipboard_cut(&mut self) {
        self.edit_clipboard_copy();
        if let Some(e) = self.editing.as_mut() {
            if !e.buffer.is_empty() {
                e.buffer.clear();
                e.caret = 0;
                self.dirty = true;
            }
        }
    }

    /// Paste the clipboard's text at the caret. Reuses [`Self::edit_insert`],
    /// so control characters (newlines, etc.) are stripped to keep the field
    /// single-line, matching typed input.
    fn edit_clipboard_paste(&mut self) {
        if self.editing.is_none() {
            return;
        }
        if let Some(text) = ui::clipboard_paste() {
            self.edit_insert(&text);
        }
    }

    /// Write the active edit's buffer to its durable home and clear the edit.
    /// An emptied comment body (with no replies) removes the note entirely so
    /// an abandoned marker never litters the page.
    fn commit_edit(&mut self, state: &mut AutoCommit) {
        let Some(e) = self.editing.take() else { return };
        match e.target {
            EditTarget::CommentBody(id) => {
                let trimmed = e.buffer.trim();
                let has_replies = self
                    .comments
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| !c.replies.is_empty())
                    .unwrap_or(false);
                if trimmed.is_empty() && !has_replies {
                    self.delete_target(&DeleteTarget::Comment(id), state);
                    return;
                }
                PdfDoc::set_comment_body(state, &id, e.buffer.trim_end());
                self.commit_pending = true;
                self.reload(state);
            }
            EditTarget::Reply(id) => {
                let body = e.buffer.trim();
                if !body.is_empty() {
                    PdfDoc::add_reply(
                        state,
                        &id,
                        &Reply { author: self.author.clone(), body: body.to_string() },
                    );
                    self.commit_pending = true;
                    self.reload(state);
                }
            }
            EditTarget::Signer => {
                let name = e.buffer.trim();
                self.signer_name = if name.is_empty() { self.author.clone() } else { name.to_string() };
            }
        }
        self.dirty = true;
    }

    fn cancel_edit(&mut self) {
        self.editing = None;
        self.dirty = true;
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

    fn place_signature(&mut self, state: &mut AutoCommit, page: usize, pt: (f64, f64)) {
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
            style: signature_style_for(&self.signer_name),
        };
        PdfDoc::add_signature(state, &s);
        self.selected_sig = Some(sig_id.clone());
        self.selected_anno = None;
        // Land the user on the Signatures rail, scrolled to the new field.
        self.focus_sidebar(SidebarTab::Signatures, sig_id);
        self.commit_pending = true;
        self.reload(state);
        self.dirty = true;
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
                AnnoKind::Highlight { rect } => {
                    if rect.contains(pt.0, pt.1) && hit.is_none() {
                        hit = Some(a.id.clone());
                    }
                }
                _ => {}
            }
        }
        self.selected_anno = hit.clone();
        self.selected_sig = None;
        if let Some(id) = hit {
            // Reveal the linked comment.
            if let Some(c) = self.comments.iter().find(|c| c.anno_id.as_deref() == Some(id.as_str())) {
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
        self.annotations.iter().filter(|a| a.page == page).find_map(|a| match &a.kind {
            AnnoKind::Note { x, y } if (pt.0 - x).abs() < 24.0 && (pt.1 - y).abs() < 24.0 => {
                Some((a.id.clone(), *x, *y))
            }
            _ => None,
        })
    }

    /// The page an annotation (by id) lives on.
    fn anno_page(&self, id: &str) -> Option<usize> {
        self.annotations.iter().find(|a| a.id == id).map(|a| a.page)
    }

    /// The current page-point position of a note (by id).
    fn note_pos(&self, id: &str) -> Option<(f64, f64)> {
        self.annotations.iter().find(|a| a.id == id).and_then(|a| match &a.kind {
            AnnoKind::Note { x, y } => Some((*x, *y)),
            _ => None,
        })
    }

    /// Move a note's *local* cached anchor for live drag feedback (the CRDT is
    /// only written once, on release, so a drag is a single undo step).
    fn move_note_local(&mut self, id: &str, x: f64, y: f64) {
        if let Some(a) = self.annotations.iter_mut().find(|a| a.id == id) {
            if let AnnoKind::Note { x: nx, y: ny } = &mut a.kind {
                *nx = x;
                *ny = y;
            }
        }
        self.dirty = true;
    }

    /// The live selection's page-point bands (real pdfium glyph boxes), for
    /// creating highlights from a text selection.
    fn selection_page_bands(&self) -> Vec<PtRect> {
        let Some(sel) = self.selection else { return Vec::new() };
        if !sel.is_active() {
            return Vec::new();
        }
        let Some(layer) = self.raster_text.get(&sel.page) else { return Vec::new() };
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
    fn highlight_selection(&mut self, state: &mut AutoCommit, swatch: Swatch) -> bool {
        let Some(sel) = self.selection else { return false };
        let bands = self.selection_page_bands();
        if bands.is_empty() {
            return false;
        }
        let mut last = None;
        for r in bands {
            let a = Annotation {
                id: Annotation::new_id(),
                page: sel.page,
                kind: AnnoKind::Highlight { rect: r },
                color: swatch,
                author: self.author.clone(),
            };
            last = Some(a.id.clone());
            PdfDoc::add_annotation(state, &a);
        }
        self.swatch = swatch;
        self.selected_anno = last;
        self.commit_pending = true;
        self.clear_selection();
        self.reload(state);
        self.dirty = true;
        true
    }

    /// Attach a sticky note to the start of the live selection and open its
    /// editor (mirrors the ebook reader's note-from-selection flow).
    fn note_from_selection(&mut self, state: &mut AutoCommit) -> bool {
        let bands = self.selection_page_bands();
        let Some(first) = bands.first().copied() else { return false };
        let Some(sel) = self.selection else { return false };
        let page = sel.page;
        self.clear_selection();
        self.create_note(state, page, (first.x, first.y));
        true
    }

    /// Route a shared-HUD action to this reader's commands.
    fn apply_hud_action(&mut self, action: ui::HudAction, state: &mut AutoCommit) {
        match action {
            ui::HudAction::Copy => {
                self.copy_selection();
            }
            ui::HudAction::Highlight(i) => {
                let sw = Swatch::ALL[i.min(Swatch::ALL.len() - 1)];
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
    fn delete_target(&mut self, target: &DeleteTarget, state: &mut AutoCommit) {
        match target {
            DeleteTarget::Annotation(id) => {
                PdfDoc::delete_annotation(state, id);
                if self.selected_anno.as_deref() == Some(id.as_str()) {
                    self.selected_anno = None;
                }
            }
            DeleteTarget::Comment(id) => {
                // Deleting a comment also removes the annotation it pins, so
                // the orphaned note marker doesn't linger on the page.
                if let Some(anno) = self
                    .comments
                    .iter()
                    .find(|c| &c.id == id)
                    .and_then(|c| c.anno_id.clone())
                {
                    PdfDoc::delete_annotation(state, &anno);
                    if self.selected_anno.as_deref() == Some(anno.as_str()) {
                        self.selected_anno = None;
                    }
                }
                PdfDoc::delete_comment(state, id);
            }
            DeleteTarget::Signature(id) => {
                PdfDoc::delete_signature(state, id);
                if self.selected_sig.as_deref() == Some(id.as_str()) {
                    self.selected_sig = None;
                }
            }
        }
        self.context_menu = None;
        self.commit_pending = true;
        self.reload(state);
        self.dirty = true;
    }

    /// Delete whichever markup the user currently has selected (Delete key).
    fn delete_selected(&mut self, state: &mut AutoCommit) -> bool {
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
                AnnoKind::Highlight { rect } => {
                    if rect.contains(pt.0, pt.1) {
                        return Some(DeleteTarget::Annotation(a.id.clone()));
                    }
                }
                _ => {}
            }
        }
        None
    }

    // ---- host-native file open (XDG portal) -------------------------

    /// Ask the host desktop's file chooser to open a PDF. Per HIG §5 the app
    /// never draws its own picker: the portal round-trip runs on a worker
    /// thread ([`crate::portal::request_open`]) and the chosen path comes back
    /// over a channel drained each frame in [`Self::drain_portal`]. A second
    /// request while one is in flight is ignored, so a double-click on the
    /// Open button can't stack two host dialogs.
    fn request_open_via_portal(&mut self) {
        if self.portal_rx.is_some() {
            return;
        }
        self.portal_rx = Some(crate::portal::request_open());
        self.dirty = true;
    }

    /// Drain a completed portal request, if any. Called once per frame from
    /// [`Widget::sync`]; loads the chosen document, surfaces a failure on the
    /// empty state, and clears the in-flight channel. Never blocks — a still
    /// pending picker simply leaves the channel installed for the next frame.
    fn drain_portal(&mut self, state: &mut AutoCommit) {
        let Some(rx) = self.portal_rx.as_ref() else { return };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                match outcome {
                    crate::portal::PortalOutcome::Opened(path) => self.load_path(&path, state),
                    // A cancel is not an error — leave the current view intact.
                    crate::portal::PortalOutcome::Cancelled => {}
                    crate::portal::PortalOutcome::Failed(reason) => {
                        if !self.has_document {
                            self.load_error = Some(format!("Couldn't open the file chooser: {reason}"));
                        }
                        self.dirty = true;
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

    /// Load a real PDF from `path` into the running reader, replacing whatever
    /// was open. On success the view resets to page one at fit-page and the
    /// prior document's markup is cleared; on failure the empty state surfaces
    /// the reason.
    fn load_path(&mut self, path: &std::path::Path, state: &mut AutoCommit) {
        match crate::source::open(path) {
            Ok(loaded) => {
                self.doc = loaded.doc;
                self.raster = loaded.raster;
                self.has_document = true;
                self.doc_name = loaded.name;
                self.source_path = Some(path.to_path_buf());
                self.load_error = None;
                self.status_note = None;
                PdfDoc::clear_all(state);
                self.reload(state);
                self.commit_pending = true;
                // Reset per-peer view state for the fresh document.
                self.fit = FitMode::Page;
                self.viewport.set_scroll(0.0, 0.0);
                self.current_page = 1;
                self.selected_anno = None;
                self.selected_sig = None;
                self.editing = None;
                self.selection = None;
                self.close_search();
                self.query.clear();
                self.matches.clear();
                self.raster_matches.clear();
                self.raster_text.clear();
                self.text_lines.clear();
                self.prev_overlay = None; // re-derive panel docking for the new doc
                self.reconcile_overlay();
                self.recompute_regions();
                self.sync_viewport_frame();
            }
            Err(e) => {
                self.has_document = false;
                self.load_error = Some(format!("Could not open {}: {e}", path.display()));
            }
        }
        self.dirty = true;
    }

    /// The centred "Open a PDF…" button drawn on the empty state. Pure so the
    /// draw and the click handler agree on its rect.
    fn empty_open_button_rect(&self) -> Rect {
        let canvas = self.canvas_rect;
        let cx = (canvas.x0 + canvas.x1) / 2.0;
        let cy = (canvas.y0 + canvas.y1) / 2.0;
        let w = 150.0;
        let h = 38.0;
        Rect::new(cx - w / 2.0, cy + 78.0, cx + w / 2.0, cy + 78.0 + h)
    }
}

impl Default for PdfApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Ensure a chosen save path ends in `.pdf` (some portals return a name
/// without the extension when the user typed it bare).
fn ensure_pdf_extension(path: std::path::PathBuf) -> std::path::PathBuf {
    let is_pdf = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false);
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
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        // Pull in any file the host portal picker returned since the last
        // frame (off-thread; never blocks the UI).
        self.drain_portal(state);
        // And any completed save-location choice, flattening markup to it.
        self.drain_portal_save();
        // And any completed import (a PDF/image chosen to add as pages).
        self.drain_portal_import();

        if !self.seeded {
            // Demonstration markup is seeded only on the screenshot / test
            // path; a real document opens with whatever markup the shared
            // CRDT already holds and never fabricates content.
            if self.seed_sample && !PdfDoc::is_seeded(state) {
                self.seed(state);
            }
            self.seeded = true;
        }
        self.reload(state);

        // Screenshot hook (never set at runtime): focus the first comment.
        if self.shots_edit_first_comment {
            if let Some(c) = self.comments.first() {
                let (id, body) = (c.id.clone(), c.body.clone());
                self.begin_edit(EditTarget::CommentBody(id), body);
            }
            self.shots_edit_first_comment = false;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        self.on_event(event, scale_factor, modifiers, state, handled);
    }

    fn name(&self) -> &str {
        "PdfApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("pdf_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_commit(&self) -> bool {
        self.commit_pending
    }

    fn on_commit(&mut self) {
        self.commit_pending = false;
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.pending_raster
            || self.drag.is_some()
            || self.pan.is_some()
            || self.rail_drag.is_some()
    }

    fn needs_animation_frame(&self) -> bool {
        // Tick only while a hover is genuinely counting toward the tooltip's
        // reveal delay. Once the tooltip is shown (or the pointer isn't over a
        // button at all) no further frames are needed, so an idle window
        // settles to zero frames instead of redrawing forever. The pointer
        // leaving a button arrives as a real event, which repaints and clears
        // the popover on its own.
        //
        // Also tick while a page rasterisation was deferred off this frame's
        // budget (`pending_raster`): the framework's animation pump re-draws at
        // ~60 Hz so the remaining visible pages fill in over the next frame(s),
        // then `pending_raster` clears and the window settles back to idle.
        self.tooltip_pending || self.pending_raster
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
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
        // Root fills the whole window — never cull.
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn app_with_state() -> (PdfApp, ui::State) {
        let mut app = PdfApp::sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        // Give it a viewport so geometry is meaningful.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        (app, st)
    }

    /// Run one full `ui::App`-equivalent frame against `app`: build the taffy
    /// tree, compute layout at `w`×`h`, apply it, then draw. Mirrors
    /// `ui::App`'s RedrawRequested sequence so a driven test exercises the
    /// *live* path, not a shortcut.
    ///
    /// A PNG is written to `out` **only when `PDF_SHOTS=1`** in the
    /// environment. The default `cargo test` run therefore never touches the
    /// GPU (the offscreen `wgpu` renderer serialises on a process-wide context
    /// and is slow/contended under the parallel suite) — the behavioural
    /// assertions still run every time. Regenerate the shots deliberately with
    /// `PDF_SHOTS=1 … cargo test drive_review -- --test-threads=1`.
    fn full_frame(app: &mut PdfApp, st: &mut ui::State, w: f64, h: f64, out: Option<&str>) {
        use std::collections::HashMap;
        use ui::offscreen::OffscreenRenderer;
        use ui::permissions::PermissionContext;
        use ui::presence::PresenceCtx;
        use ui::text_render::TextRenderer;
        use ui::theme::Theme;

        let want_png = std::env::var("PDF_SHOTS").as_deref() == Ok("1");
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        app.sync(&mut st.doc, &st.root);
        let child = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style { size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) }, ..Default::default() },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let trm = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size { width: taffy::AvailableSpace::Definite(w as f32), height: taffy::AvailableSpace::Definite(h as f32) },
                    |known, avail, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, avail, trm) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        app.apply_layout(&taffy, child, Point::ORIGIN);
        // Draw until the per-frame raster budget's backlog drains, so a single
        // still frame is the *settled* image (the live app pumps these
        // follow-up frames via `needs_animation_frame`; a one-shot capture must
        // do it itself or the rail/second page would be half-rasterised).
        let mut scene = Scene::new();
        for _ in 0..32 {
            scene = Scene::new();
            app.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &PresenceCtx::empty(), &PermissionContext::permissive());
            if !app.pending_raster {
                break;
            }
        }
        if let (Some(path), true) = (out, want_png) {
            if let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let mut r = OffscreenRenderer::new(w as u32, h as u32);
            r.render_png(&scene, vello::peniko::Color::from_rgb8(10, 12, 16), std::path::Path::new(path));
        }
    }

    /// Set the zoom to an exact factor via the viewport kernel + fit intent,
    /// then run a live frame and (optionally) shoot it. Returns the page-card
    /// width on screen so a test can assert the scale really changed.
    fn set_zoom_and_frame(app: &mut PdfApp, st: &mut ui::State, z: f64, out: Option<&str>) -> f64 {
        app.fit = FitMode::Custom(z);
        app.viewport.set_zoom(z);
        full_frame(app, st, 1280.0, 800.0, out);
        app.pw() * app.zoom()
    }

    /// Drive-review on the real bundled `sample.pdf`: exact 50 / 100 / 200 %
    /// zoom, a panned view, and a placed annotation — each run through the live
    /// frame path and shot to `/tmp/pdf_repro`. Proves zoom actually changes
    /// the on-screen page size and that horizontal pan reaches new content.
    #[test]
    #[serial(pdfium)]
    fn drive_review_zoom_pan_annotate_shots() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.pdf");
        let mut app = PdfApp::open(path);
        let mut st = ui::State::new();
        assert!(app.has_document, "sample.pdf must load");
        eprintln!("DRIVE backend: {}", if app.raster.is_some() { "RASTER (pdfium)" } else { "TEXT" });

        // --- exact 50 / 100 / 200 % zoom, each a live frame + shot. ---
        let w50 = set_zoom_and_frame(&mut app, &mut st, 0.5, Some("/tmp/pdf_repro/zoom_50.png"));
        let w100 = set_zoom_and_frame(&mut app, &mut st, 1.0, Some("/tmp/pdf_repro/zoom_100.png"));
        let w200 = set_zoom_and_frame(&mut app, &mut st, 2.0, Some("/tmp/pdf_repro/zoom_200.png"));
        eprintln!("DRIVE page-card width px: 50%={w50:.1}  100%={w100:.1}  200%={w200:.1}");
        assert!((w100 - 2.0 * w50).abs() < 1.0, "100% page must be twice the 50% page");
        assert!((w200 - 2.0 * w100).abs() < 1.0, "200% page must be twice the 100% page");

        // --- Ctrl+wheel zoom changes scale (the wheel handler's reducer). ---
        let before = app.zoom();
        let c = app.canvas_rect.center();
        app.zoom_at(1.0 + 56.0 / 240.0, Point::new(c.x, c.y));
        assert!(app.zoom() > before + 1e-3, "ctrl+wheel-equivalent zoom must change scale");

        // --- horizontal pan at 200%: the page overflows, so panning right must
        //     move the content and reach new offsets. ---
        app.fit = FitMode::Custom(2.0);
        app.viewport.set_zoom(2.0);
        full_frame(&mut app, &mut st, 1280.0, 800.0, None);
        assert!(app.is_overflowing_x(), "200% must overflow the viewport horizontally");
        let sx0 = app.scroll_x();
        app.viewport.pan_by(240.0, 0.0); // arrow/drag-equivalent rightward pan
        let sx1 = app.scroll_x();
        assert!(sx1 > sx0 + 1.0, "panning right must increase scroll_x ({sx0} -> {sx1})");
        full_frame(&mut app, &mut st, 1280.0, 800.0, Some("/tmp/pdf_repro/panned_200.png"));
        eprintln!("DRIVE pan: scroll_x {sx0:.1} -> {sx1:.1} (max {:.1})", app.viewport.max_scroll_x());

        // --- place an annotation on the real document and shoot it. ---
        app.fit = FitMode::Page;
        app.viewport.set_zoom(app.fit_page_zoom());
        full_frame(&mut app, &mut st, 1280.0, 800.0, None);
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
        app.commit_drag(&mut st.doc);
        assert!(app.annotations.len() > a0, "a highlight drag adds an annotation");
        app.go_to_page(1); // bring the freshly-placed highlight into view
        eprintln!("DRIVE annotate scroll_y after go_to_page(1) = {:.1}", app.scroll_y());
        full_frame(&mut app, &mut st, 1280.0, 800.0, Some("/tmp/pdf_repro/annotated.png"));
        eprintln!("DRIVE annotate: {} -> {} annotations; scroll_y after frame = {:.1}", a0, app.annotations.len(), app.scroll_y());
    }

    /// Drive-review the review features on the seeded sample document:
    /// annotations render anchored to real text (no free-floating ellipse over
    /// whitespace), search highlights matches, and the Contents outline draws.
    /// Shots to `/tmp/pdf_repro`.
    #[test]
    fn drive_review_features_shots() {
        let mut app = PdfApp::sample();
        let mut st = ui::State::new();

        // Default sample view — seeded highlights + note + signature on page 1.
        full_frame(&mut app, &mut st, 1280.0, 800.0, Some("/tmp/pdf_repro/sample_markup.png"));
        // The seeded markup must contain NO free-form ink stroke (the removed
        // "emphasis loop" that circled empty whitespace).
        assert!(
            !app.annotations.iter().any(|a| matches!(a.kind, AnnoKind::Ink { .. })),
            "no seeded ink 'emphasis loop' should remain"
        );
        assert!(
            app.annotations.iter().any(|a| matches!(a.kind, AnnoKind::Highlight { .. })),
            "seeded highlights remain"
        );

        // Search: whole-word + case toggles narrow results.
        app.open_search();
        app.query = "sandbox".into();
        app.recompute_matches();
        let loose = app.matches.len();
        assert!(loose > 0, "search finds 'sandbox' in the sample");
        full_frame(&mut app, &mut st, 1280.0, 800.0, Some("/tmp/pdf_repro/search_results.png"));
        app.search_whole_word = true;
        app.recompute_matches();
        assert!(app.matches.len() <= loose, "whole-word never yields more matches");
        app.close_search();

        // Contents (outline) tab renders the document's section list.
        app.sidebar_tab = SidebarTab::Outline;
        assert!(!app.doc.outline.is_empty(), "sample has an outline");
        full_frame(&mut app, &mut st, 1280.0, 800.0, Some("/tmp/pdf_repro/outline.png"));
        eprintln!("DRIVE features: {} annotations, {} search hits, {} outline items", app.annotations.len(), loose, app.doc.outline.len());
    }

    #[test]
    fn sync_seeds_markup_once() {
        let (app, _st) = app_with_state();
        assert!(app.seeded);
        assert!(!app.annotations.is_empty(), "seed should add annotations");
        assert!(!app.comments.is_empty(), "seed should add comments");
        assert!(!app.signatures.is_empty(), "seed should add a signature");
        // The note annotation has a linked comment thread.
        let note = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Note { .. }))
            .expect("a sticky note");
        assert!(app
            .comments
            .iter()
            .any(|c| c.anno_id.as_deref() == Some(note.id.as_str())));
    }

    #[test]
    fn second_sync_does_not_duplicate() {
        let (mut app, mut st) = app_with_state();
        let n = app.annotations.len();
        app.sync(&mut st.doc, &st.root);
        assert_eq!(app.annotations.len(), n, "re-sync must not re-seed");
    }

    /// A synthetic single-line text layer over page 1 + a settled selection,
    /// so the shared-HUD tests need no native pdfium.
    fn seed_selection(app: &mut PdfApp, chars: &str, upto: usize) {
        let glyphs: Vec<(char, PtRect)> = chars
            .chars()
            .enumerate()
            .map(|(i, c)| (c, PtRect::new(72.0 + i as f64 * 9.0, 120.0, 8.0, 12.0)))
            .collect();
        app.raster_text.insert(1, std::sync::Arc::new(PageText::from_glyphs(glyphs)));
        app.selection = Some(TextSelection { page: 1, anchor: 0, caret: upto, dragging: false });
    }

    #[test]
    fn hud_highlight_action_highlights_the_text_selection() {
        let (mut app, mut st) = app_with_state();
        seed_selection(&mut app, "Hello", 4);
        let hl = |st: &ui::State| {
            PdfDoc::list_annotations(&st.doc)
                .into_iter()
                .filter(|a| matches!(a.kind, AnnoKind::Highlight { .. }))
                .count()
        };
        let before = hl(&st);
        // The shared HUD's swatch action highlights the run in that colour.
        app.apply_hud_action(ui::HudAction::Highlight(1), &mut st.doc);
        assert!(hl(&st) > before, "highlighting the selection adds a highlight annotation");
        assert!(app.selection.is_none(), "the selection is consumed by the highlight");
        assert_eq!(app.swatch, Swatch::ALL[1], "the armed swatch follows the HUD choice");
    }

    #[test]
    fn hud_note_action_attaches_a_note_to_the_selection() {
        let (mut app, mut st) = app_with_state();
        seed_selection(&mut app, "AB", 1);
        let notes = |st: &ui::State| {
            PdfDoc::list_annotations(&st.doc)
                .into_iter()
                .filter(|a| matches!(a.kind, AnnoKind::Note { .. }))
                .count()
        };
        let before = notes(&st);
        app.apply_hud_action(ui::HudAction::Note, &mut st.doc);
        assert_eq!(notes(&st), before + 1, "a note is attached to the selection");
        assert!(app.editing.is_some(), "the note's comment editor opens for typing");
    }

    #[test]
    fn hud_copy_action_reports_success_on_a_selection() {
        let (mut app, mut st) = app_with_state();
        seed_selection(&mut app, "Copyme", 5);
        assert!(app.selection_text().is_some(), "the synthetic run yields text");
        app.apply_hud_action(ui::HudAction::Copy, &mut st.doc);
        assert!(
            app.status_note.as_deref().unwrap_or("").contains("Copied"),
            "the HUD Copy action copies + toasts ({:?})",
            app.status_note
        );
    }

    #[test]
    fn dragging_a_note_marker_repositions_and_persists() {
        let (mut app, mut st) = app_with_state();
        // A fresh note at a known page-point.
        app.create_note(&mut st.doc, 1, (100.0, 100.0));
        app.reload(&st.doc);
        let id = app.selected_anno.clone().expect("the new note is selected");
        // Its marker is grabbable at its anchor (same target as clicking it).
        assert!(app.note_marker_at(1, (100.0, 100.0)).is_some(), "the marker is grabbable");
        // Begin → move → the local cache tracks the pointer for live feedback.
        let mut mdrag = ui::MarkerDrag::begin(id.clone(), (100.0, 100.0), (100.0, 100.0));
        let (nx, ny) = mdrag.update((180.0, 140.0));
        assert!(mdrag.moved(), "a real drag latches moved");
        app.move_note_local(&id, nx, ny);
        assert_eq!(app.note_pos(&id), Some((nx, ny)), "the marker follows the pointer live");
        // Release persists the new anchor to the shared document.
        PdfDoc::set_annotation_position(&mut st.doc, &id, nx, ny);
        app.reload(&st.doc);
        let (x, y) = app.note_pos(&id).expect("the note survives the reposition");
        assert!((x - nx).abs() < 1e-6 && (y - ny).abs() < 1e-6, "the reposition persisted to the CRDT");
        assert!((x - 100.0).abs() > 1.0, "the note actually moved from its origin");
    }

    #[test]
    fn search_finds_and_jumps() {
        let (mut app, _st) = app_with_state();
        app.query = "filesystem".into();
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

    #[test]
    fn create_note_adds_annotation_and_comment() {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        let c0 = app.comments.len();
        app.create_note(&mut st.doc, 2, (120.0, 200.0));
        assert_eq!(app.annotations.len(), a0 + 1);
        assert_eq!(app.comments.len(), c0 + 1);
        assert!(app.commit_pending);
        assert_eq!(app.sidebar_tab as u8, SidebarTab::Comments as u8);
    }

    #[test]
    fn highlight_drag_commits_annotation() {
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
        app.commit_drag(&mut st.doc);
        assert_eq!(app.annotations.len(), a0 + 1, "drag should add one highlight");
    }

    #[test]
    fn tiny_highlight_drag_is_ignored() {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (64.0, 100.0),
            cur: (66.0, 101.0),
            points: vec![],
        });
        app.commit_drag(&mut st.doc);
        assert_eq!(app.annotations.len(), a0, "sub-threshold drag adds nothing");
    }

    #[test]
    fn place_signature_clamps_into_page() {
        let (mut app, mut st) = app_with_state();
        let s0 = app.signatures.len();
        app.signer_name = "Tester".into();
        app.place_signature(&mut st.doc, 3, (10.0, 10.0)); // near corner
        assert_eq!(app.signatures.len(), s0 + 1);
        let last = app.signatures.iter().find(|s| s.page == 3).unwrap();
        assert!(last.rect.x >= MARGIN - 0.001);
        assert!(last.rect.y >= MARGIN - 0.001);
    }

    #[test]
    fn page_navigation_clamps() {
        let (mut app, _st) = app_with_state();
        app.go_to_page(999);
        assert_eq!(app.current_page, app.doc.page_count());
        app.go_to_page(0);
        assert_eq!(app.current_page, 1);
    }

    #[test]
    fn zoom_switches_to_custom() {
        let (mut app, _st) = app_with_state();
        app.zoom_by(1.25);
        assert!(matches!(app.fit, FitMode::Custom(_)));
    }

    #[test]
    fn zoom_at_keeps_focal_point_stationary() {
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
        assert!((before.1 .0 - after.1 .0).abs() < 2.0, "x focal drift {} too large", (before.1 .0 - after.1 .0).abs());
        assert!((before.1 .1 - after.1 .1).abs() < 2.0, "y focal drift {} too large", (before.1 .1 - after.1 .1).abs());
    }

    #[test]
    fn zoom_respects_min_max_bounds() {
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

    #[test]
    fn horizontal_pan_reaches_full_width_when_zoomed() {
        let (mut app, _st) = app_with_state();
        app.fit = FitMode::Custom(3.0);
        app.recompute_zoom();
        app.clamp_scroll();
        assert!(app.is_overflowing_x(), "3x zoom must overflow the viewport");
        let sy = app.scroll_y();
        app.viewport.set_scroll(f64::MAX, sy);
        // The full reachable width is content_width - canvas width.
        let expected = (app.viewport.scaled_content_width() - app.canvas_rect.width()).max(0.0);
        assert!((app.scroll_x() - expected).abs() < 1.0, "pan should reach the full zoomed width");
    }

    #[test]
    fn highlight_snaps_to_text_when_lines_present() {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        // Seed a few text-line bands spanning two lines and drag across both.
        app.text_lines.insert(
            1,
            vec![
                render::TextLine { block: 5, x: 64.0, x_end: 480.0, y: 196.0, h: 13.0 },
                render::TextLine { block: 5, x: 64.0, x_end: 420.0, y: 210.0, h: 13.0 },
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
        app.commit_drag(&mut st.doc);
        // Two covered lines -> two text-shaped bands (not one free rectangle).
        assert_eq!(app.annotations.len(), a0 + 2, "drag over two lines snaps to two bands");
    }

    #[test]
    fn highlight_falls_back_to_freeform_without_text() {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        // No text-line cache near the drag -> free-form rectangle (one anno).
        app.text_lines.insert(
            1,
            vec![render::TextLine { block: 5, x: 64.0, x_end: 480.0, y: 196.0, h: 13.0 }],
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
        app.commit_drag(&mut st.doc);
        assert_eq!(app.annotations.len(), a0 + 1, "no nearby text -> single free-form band");
    }

    #[test]
    fn delete_selected_annotation_removes_it() {
        let (mut app, mut st) = app_with_state();
        let hl = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Highlight { .. }))
            .unwrap()
            .clone();
        let n = app.annotations.len();
        app.selected_anno = Some(hl.id.clone());
        assert!(app.delete_selected(&mut st.doc));
        assert_eq!(app.annotations.len(), n - 1);
        assert!(!app.annotations.iter().any(|a| a.id == hl.id));
        assert!(app.selected_anno.is_none());
    }

    #[test]
    fn delete_signature_target_removes_it() {
        let (mut app, mut st) = app_with_state();
        let sig = app.signatures[0].clone();
        let n = app.signatures.len();
        app.delete_target(&DeleteTarget::Signature(sig.id.clone()), &mut st.doc);
        assert_eq!(app.signatures.len(), n - 1);
        assert!(!app.signatures.iter().any(|s| s.id == sig.id));
    }

    #[test]
    fn delete_comment_target_also_drops_its_annotation() {
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
        app.delete_target(&DeleteTarget::Comment(comment.id.clone()), &mut st.doc);
        assert!(!app.comments.iter().any(|c| c.id == comment.id));
        assert!(!app.annotations.iter().any(|a| a.id == note.id), "orphan note marker removed");
    }

    #[test]
    fn placing_signature_focuses_signatures_sidebar() {
        let (mut app, mut st) = app_with_state();
        app.sidebar_tab = SidebarTab::Comments;
        app.place_signature(&mut st.doc, 2, (200.0, 300.0));
        assert_eq!(app.sidebar_tab as u8, SidebarTab::Signatures as u8);
        assert!(app.show_sidebar);
        assert!(app.selected_sig.is_some(), "new signature is selected");
        assert!(app.sidebar_reveal.is_some(), "reveal request queued for the new entry");
    }

    #[test]
    fn creating_note_focuses_comments_sidebar_on_new_thread() {
        let (mut app, mut st) = app_with_state();
        app.sidebar_tab = SidebarTab::Signatures;
        app.create_note(&mut st.doc, 2, (200.0, 300.0));
        assert_eq!(app.sidebar_tab as u8, SidebarTab::Comments as u8);
        assert!(app.show_sidebar);
        assert!(app.sidebar_reveal.is_some(), "new comment queued to scroll into view");
    }

    #[test]
    fn rail_toggle_and_resize_reclaim_space() {
        let (mut app, _st) = app_with_state();
        let canvas_with_rail = app.canvas_rect.width();
        app.toggle_rail();
        assert!(!app.show_rail);
        assert!(app.canvas_rect.width() > canvas_with_rail, "hiding the rail reclaims space");
        app.toggle_rail();
        assert!(app.show_rail);
        // Resize within bounds is honoured.
        app.rail_w = 999.0;
        app.recompute_regions();
        assert!((app.rail_width() - RAIL_W_MAX).abs() < 1e-6, "rail width clamps to max");
    }

    #[test]
    fn responsive_mode_reflows_on_resize_not_cached() {
        // Prove the breakpoint is re-derived from the *current* width on every
        // layout pass: the same instance must reflow when resized between size
        // classes, not stay frozen at whatever it launched as.
        let (mut app, _st) = app_with_state(); // starts wide (1280)
        app.reconcile_overlay();
        app.recompute_regions();
        assert!(!app.overlay_mode, "wide window docks panels");
        assert!(app.show_sidebar && app.show_rail, "docked panels are visible when wide");
        // Wide: the canvas leaves room for the rail + sidebar columns.
        assert!(app.canvas_rect.width() < app.rect.width() - 100.0, "docked panels reserve columns");

        // Shrink below the overlay breakpoint — panels must collapse to closed
        // overlays and the canvas must span the whole (smaller) window.
        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.reconcile_overlay();
        app.recompute_regions();
        assert!(app.overlay_mode, "narrow window switches to overlay mode");
        assert!(!app.show_sidebar && !app.show_rail, "narrow window auto-closes the panels");
        assert!((app.canvas_rect.width() - app.rect.width()).abs() < 1.0, "canvas spans the full width in overlay mode");

        // Grow back to wide — panels re-dock.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.reconcile_overlay();
        app.recompute_regions();
        assert!(!app.overlay_mode, "returning to wide re-docks");
        assert!(app.show_sidebar && app.show_rail, "panels come back when wide again");
    }

    #[test]
    fn search_toggles_change_match_logic() {
        let (mut app, _st) = app_with_state();
        app.query = "the".into();
        app.search_case_sensitive = false;
        app.search_whole_word = false;
        app.recompute_matches();
        let loose = app.matches.len();
        app.search_whole_word = true;
        app.recompute_matches();
        let strict = app.matches.len();
        assert!(strict <= loose, "whole-word never yields more matches than substring");
    }

    #[test]
    fn ymd_from_unix_secs_matches_known_dates() {
        // Fixed reference points, so the pure date conversion is proven
        // correct independently of the wall clock.
        assert_eq!(ymd_from_unix_secs(0), "1970-01-01"); // Unix epoch
        assert_eq!(ymd_from_unix_secs(946_684_800), "2000-01-01");
        assert_eq!(ymd_from_unix_secs(1_582_977_600), "2020-02-29"); // a leap day
        assert_eq!(ymd_from_unix_secs(1_798_761_599), "2026-12-31"); // year boundary
        // `today_ymd` is well-formed `YYYY-MM-DD`.
        let t = today_ymd();
        assert_eq!(t.len(), 10, "today is YYYY-MM-DD: {t}");
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[7..8], "-");
    }

    #[test]
    fn placed_signature_stamps_the_real_date_not_a_constant() {
        let (mut app, mut st) = app_with_state();
        app.signer_name = "Grace Hopper".into();
        app.place_signature(&mut st.doc, 2, (200.0, 300.0));
        let s = app.signatures.iter().find(|s| s.page == 2).unwrap().clone();
        // The stamp is the live system date, not the frozen demo/seed date.
        assert_eq!(s.date, today_ymd(), "runtime signature carries today's date");
        assert_ne!(s.date, SEED_DATE, "runtime date is not the hardcoded demo constant");
        // And the real date is what durably lands in the shared CRDT.
        let stored = PdfDoc::list_signatures(&st.doc);
        assert!(stored.iter().any(|x| x.id == s.id && x.date == today_ymd()));
    }

    #[test]
    fn signature_style_is_per_signer_and_reaches_every_style() {
        use std::collections::HashSet;
        // Distinct signers must not collapse to one canned squiggle: across a
        // spread of names all four bundled styles are reachable.
        let mut seen = HashSet::new();
        for i in 0..256 {
            let s = signature_style_for(&format!("Signer {i}"));
            assert!(s < 4, "style stays in range");
            seen.insert(s);
        }
        assert_eq!(seen.len(), 4, "all four handwriting styles are reachable across signers");
        // Stable for a given name; empty falls back to a fixed style.
        assert_eq!(signature_style_for("Ada Lovelace"), signature_style_for("Ada Lovelace"));
        assert_eq!(signature_style_for("   "), 0);
    }

    #[test]
    fn placed_signature_style_follows_the_signer_and_matches_the_preview() {
        let (mut app, mut st) = app_with_state();
        app.signer_name = "Grace Hopper".into();
        // The signer-card preview (drawn when not editing) uses
        // signature_style_for(signer_name); placement stamps the same input, so
        // the card can never preview a stroke different from what is stamped.
        let preview_style = signature_style_for(&app.signer_name);
        app.place_signature(&mut st.doc, 3, (200.0, 300.0));
        let placed = app.signatures.iter().find(|s| s.page == 3).unwrap();
        assert_eq!(placed.style, preview_style, "placed style follows the signer, matching the preview");
    }

    #[test]
    fn ink_drag_commits_a_polyline_annotation() {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        app.tool = Tool::Ink;
        app.drag = Some(DragState {
            tool: Tool::Ink,
            page: 1,
            start: (80.0, 300.0),
            cur: (240.0, 360.0),
            points: vec![(80.0, 300.0), (140.0, 330.0), (200.0, 320.0), (240.0, 360.0)],
        });
        app.commit_drag(&mut st.doc);
        assert_eq!(app.annotations.len(), a0 + 1, "an ink drag adds one annotation");
        let ink = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Ink { .. }))
            .expect("an ink stroke was created");
        match &ink.kind {
            AnnoKind::Ink { points } => assert!(points.len() >= 2, "the polyline retains its points"),
            _ => unreachable!(),
        }
        assert!(app.commit_pending, "ink is a committed CRDT action");
        assert!(
            PdfDoc::list_annotations(&st.doc).iter().any(|a| matches!(a.kind, AnnoKind::Ink { .. })),
            "and the ink stroke landed in the shared document"
        );
    }

    #[test]
    fn ink_drag_with_a_single_point_is_discarded() {
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
        app.commit_drag(&mut st.doc);
        assert_eq!(app.annotations.len(), a0, "a degenerate single-point ink stroke adds nothing");
    }

    #[test]
    fn signer_name_inline_edit_commits_and_empty_falls_back_to_author() {
        let (mut app, mut st) = app_with_state();
        // Type a real name into the signer identity field and commit it.
        app.begin_edit(EditTarget::Signer, app.signer_name.clone());
        app.editing.as_mut().unwrap().buffer = "Grace Hopper".into();
        app.commit_edit(&mut st.doc);
        assert!(app.editing.is_none(), "committing closes the editor");
        assert_eq!(app.signer_name, "Grace Hopper", "signer name updates from the editor");
        // A subsequent signature signs under the new identity.
        app.place_signature(&mut st.doc, 4, (200.0, 300.0));
        assert_eq!(app.signatures.iter().find(|s| s.page == 4).unwrap().signer, "Grace Hopper");
        // Committing an empty name falls back to the app author identity.
        app.begin_edit(EditTarget::Signer, "whatever".into());
        app.editing.as_mut().unwrap().buffer = "   ".into();
        app.commit_edit(&mut st.doc);
        assert_eq!(app.signer_name, app.author, "an empty signer name falls back to the author");
    }

    #[test]
    #[serial(pdfium)]
    fn loading_a_second_document_clears_the_prior_documents_markup() {
        let (mut app, mut st) = app_with_state(); // seeded sample, has markup
        assert!(!app.annotations.is_empty(), "sample starts with seeded markup");
        assert!(!app.signatures.is_empty());
        assert!(!app.comments.is_empty());
        let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.pdf"));
        app.load_path(&path, &mut st.doc);
        assert!(app.has_document, "the second document loads");
        assert_eq!(app.doc_name, "sample.pdf");
        // The prior document's markup must not bleed onto the new one.
        assert!(app.annotations.is_empty(), "prior annotations cleared");
        assert!(app.comments.is_empty(), "prior comments cleared");
        assert!(app.signatures.is_empty(), "prior signatures cleared");
        assert!(PdfDoc::list_annotations(&st.doc).is_empty(), "cleared in the shared document too");
        assert!(PdfDoc::list_signatures(&st.doc).is_empty());
        // View state resets for the fresh document.
        assert_eq!(app.current_page, 1);
        assert!(app.selected_anno.is_none() && app.selected_sig.is_none());
    }

    // ----------------------------------------------------------------
    // Real-PDF (pdfium raster) tests: text layer + save-back.
    //
    // These exercise the pdfium backend directly, so they require a usable
    // `libpdfium.so`. `crate::source::find_pdfium` already probes the well-known
    // paths, `~/.local/lib/pdfium`, and `QOS_PDFIUM_LIB`; if none is present the
    // test fails loudly with an actionable message rather than silently passing,
    // because the feature under test *is* the pdfium backend.
    // ----------------------------------------------------------------

    const SAMPLE_PDF: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.pdf");

    /// Open the bundled sample and require the raster backend, or fail with an
    /// operator-facing message.
    fn open_raster_sample() -> PdfApp {
        let app = PdfApp::open(SAMPLE_PDF);
        assert!(
            app.raster.is_some(),
            "sample.pdf must open with the pdfium raster backend — install libpdfium.so \
             (e.g. ~/.local/lib/pdfium/libpdfium.so) or set QOS_PDFIUM_LIB"
        );
        app
    }

    /// Measure the open path so regressions surface as numbers, not vibes.
    /// Times: document open (load + page sizes, no rendering), first text-layer
    /// extraction, a cold first-page rasterise, and a cache-hit rasterise
    /// (the scroll-frame cost). Prints under `--nocapture`; the assertions are
    /// loose sanity bounds, not perf gates.
    #[test]
    #[serial(pdfium)]
    fn measure_open_and_render_perf() {
        use std::time::Instant;

        let t = Instant::now();
        let mut app = PdfApp::open(SAMPLE_PDF);
        let open_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(app.raster.is_some(), "pdfium raster backend required for the perf measurement");
        let pages = app.doc.page_count();

        let t = Instant::now();
        let layer = app.raster.as_mut().unwrap().page_text(0);
        let text_ms = t.elapsed().as_secs_f64() * 1000.0;
        let glyphs = layer.chars.len();

        // A fit-page render is ~US-Letter width (612pt) times the 2x supersample.
        let bucket = 1224u32;
        let t = Instant::now();
        let img = app.raster.as_mut().unwrap().render(0, bucket);
        let render_cold_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(img.is_some(), "page 0 rasterises");

        let t = Instant::now();
        let _ = app.raster.as_mut().unwrap().render(0, bucket);
        let render_cached_ms = t.elapsed().as_secs_f64() * 1000.0;

        eprintln!(
            "PERF pages={pages} glyphs={glyphs} | open={open_ms:.1}ms \
             text_extract={text_ms:.1}ms render_cold={render_cold_ms:.1}ms \
             render_cached(scroll-frame)={render_cached_ms:.3}ms"
        );
        assert!(open_ms < 5000.0, "open should not take seconds (was {open_ms:.0}ms)");
        assert!(render_cached_ms < render_cold_ms.max(1.0), "cached render is faster than cold");
    }

    /// Opening a document and painting its first frame must not pay the pdfium
    /// text-extraction cost (hundreds of ms per page): the selectable text layer
    /// only feeds selection + search overlays, so it stays unbuilt until one of
    /// those is actually live. This guards the deferral — a regression that
    /// re-adds eager per-frame extraction reintroduces the multi-hundred-ms
    /// open/scroll hitch this reader used to have. Prints the real frame times.
    #[test]
    #[serial(pdfium)]
    fn first_frame_defers_text_extraction() {
        use std::collections::HashMap;
        use std::time::Instant;
        use ui::permissions::PermissionContext;
        use ui::presence::PresenceCtx;
        use ui::text_render::TextRenderer;
        use ui::theme::Theme;

        let mut app = open_raster_sample();
        let mut st = ui::State::new();
        let theme = Theme::dark();
        let (w, h) = (1280.0f64, 800.0f64);

        // Reuse ONE TextRenderer across frames, exactly as the live app does, so
        // the timing reflects real per-frame cost and not the one-time font-atlas
        // init the shot harness pays on every `full_frame` call.
        let mut tr = TextRenderer::new();
        // Drive one full 5-pass frame against the shared renderer.
        let mut frame = |app: &mut PdfApp, st: &mut ui::State, tr: &mut TextRenderer| {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            app.sync(&mut st.doc, &st.root);
            let child = app.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy
                .new_with_children(
                    Style { size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) }, ..Default::default() },
                    &[child],
                )
                .unwrap();
            let measure_map = node_map.clone();
            let trm = &mut *tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size { width: taffy::AvailableSpace::Definite(w as f32), height: taffy::AvailableSpace::Definite(h as f32) },
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
            app.draw_with_presence(&mut scene, Affine::IDENTITY, tr, &theme, &PresenceCtx::empty(), &PermissionContext::permissive());
        };

        // Warm the shared renderer's font atlas so it is not charged to frame 1.
        frame(&mut app, &mut st, &mut tr);
        app.raster_text.clear();

        // An idle document frame: no selection, no search — no text extraction.
        let t = Instant::now();
        frame(&mut app, &mut st, &mut tr);
        let idle_open_ms = t.elapsed().as_secs_f64() * 1000.0;
        let idle_text_pages = app.raster_text.len();

        let t = Instant::now();
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

    /// Opening a document must not rasterise a screen-full of pages on the very
    /// first frame: a real page (and, for a scanned PDF, even its rail
    /// thumbnail) costs tens-to-hundreds of ms, so the open→first-paint frame
    /// pays for only the single top page and defers the rest to follow-up
    /// frames. This guards that budget — and that the backlog drains and the
    /// frame pump settles (no permanent redraw spin).
    #[test]
    #[serial(pdfium)]
    fn open_rasterises_only_the_budgeted_pages_on_the_first_frame() {
        use std::collections::HashMap;
        use ui::permissions::PermissionContext;
        use ui::presence::PresenceCtx;
        use ui::text_render::TextRenderer;
        use ui::theme::Theme;

        let mut app = open_raster_sample();
        let mut st = ui::State::new();
        let theme = Theme::dark();
        let (w, h) = (1280.0f64, 800.0f64);
        let mut tr = TextRenderer::new();

        let mut frame = |app: &mut PdfApp, st: &mut ui::State, tr: &mut TextRenderer| {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            app.sync(&mut st.doc, &st.root);
            let child = app.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy
                .new_with_children(
                    Style { size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) }, ..Default::default() },
                    &[child],
                )
                .unwrap();
            let measure_map = node_map.clone();
            let trm = &mut *tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size { width: taffy::AvailableSpace::Definite(w as f32), height: taffy::AvailableSpace::Definite(h as f32) },
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
            app.draw_with_presence(&mut scene, Affine::IDENTITY, tr, &theme, &PresenceCtx::empty(), &PermissionContext::permissive());
        };

        // The very first painted frame after open.
        frame(&mut app, &mut st, &mut tr);
        let full_pages_first = app.raster.as_ref().unwrap().cached_full_page_count();
        assert!(
            full_pages_first <= MAX_NEW_PAGE_RASTERS_PER_FRAME,
            "first paint rasterises at most {MAX_NEW_PAGE_RASTERS_PER_FRAME} full page(s), not a screen-full — got {full_pages_first}"
        );

        // The deferred work drains over the next frames and the pump settles:
        // `pending_raster` (which drives the animation-frame follow-up) clears.
        for _ in 0..20 {
            if !app.pending_raster {
                break;
            }
            frame(&mut app, &mut st, &mut tr);
        }
        assert!(
            !app.pending_raster,
            "the deferred-raster backlog drains and the frame pump settles back to idle"
        );
    }

    /// End-to-end text selection over the real pdfium glyph layer: press on a
    /// glyph, drag across a run, copy. Drives the same `begin_selection` /
    /// `update_selection` / `copy_selection` path the pointer handlers use, so a
    /// regression in on-demand text-layer extraction or in the selection→clipboard
    /// wiring fails here. Serialised on the clipboard so it never races the other
    /// arboard tests.
    #[test]
    #[serial(clipboard)]
    #[serial(pdfium)]
    fn text_selection_drag_copies_the_real_run() {
        let mut app = open_raster_sample();

        // Two glyphs with real boxes on page 1, ordered left-to-right on a line,
        // give a non-empty run to sweep between.
        let layer = app.raster.as_mut().unwrap().page_text(0);
        let mut printable: Vec<_> = layer
            .chars
            .iter()
            .filter(|c| c.rect.w > 1.0 && c.rect.h > 1.0)
            .cloned()
            .collect();
        assert!(printable.len() >= 4, "page 1 has a run of printable glyphs");
        printable.sort_by(|a, b| (a.rect.y, a.rect.x).partial_cmp(&(b.rect.y, b.rect.x)).unwrap());
        let start = printable[0].rect.center();
        let end = printable[printable.len().min(6) - 1].rect.center();

        // Begin the selection from a cold state — no prior frame populated the
        // text layer, so this exercises on-demand extraction inside the press.
        assert!(app.raster_text.is_empty(), "no eager text layer before a selection");
        assert!(
            app.begin_selection(1, start),
            "pressing on a glyph starts a selection (extracting the layer on demand)"
        );
        app.update_selection(1, end);
        let sel = app.selection.expect("a selection is armed");
        assert!(sel.is_active(), "the drag grew the selection past its anchor");

        let text = app.selection_text().expect("the selected run yields text");
        assert!(!text.trim().is_empty(), "the selected run is real page text: {text:?}");
        // The copy path reports success and stamps the toast.
        assert!(app.copy_selection(), "copy_selection succeeds on a live selection");
        assert!(
            app.status_note.as_deref().unwrap_or("").contains("Copied"),
            "a copy toast is shown ({:?})",
            app.status_note
        );
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

    #[test]
    #[serial(pdfium)]
    fn raster_text_layer_hit_tests_real_glyphs_and_search_finds_them() {
        let mut app = open_raster_sample();

        // The text layer comes straight from pdfium's glyph geometry.
        let layer = app.raster.as_mut().unwrap().page_text(0);
        assert!(!layer.is_empty(), "page 1 exposes real extracted glyphs");

        // Hit-testing a glyph's own centre returns a glyph whose box contains
        // that point — proving the selectable layer is aligned to the render.
        let probe = layer
            .chars
            .iter()
            .find(|c| c.rect.w > 1.0 && c.rect.h > 1.0)
            .cloned()
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
        app.query = word.clone();
        app.recompute_matches();
        assert!(
            !app.raster_matches.is_empty(),
            "search finds the real word {word:?} through the pdfium text layer"
        );
        let m = app.raster_matches[0];
        let hit_layer = app.raster.as_mut().unwrap().page_text(m.page - 1);
        let got = hit_layer.text_for_range(m.char_start, m.char_start + m.char_len);
        assert_eq!(got.to_lowercase(), word.to_lowercase(), "the hit is the searched word");
        let bands = hit_layer.bands_for_range(m.char_start, m.char_start + m.char_len);
        assert!(!bands.is_empty(), "the hit yields at least one on-page band");
        let pw = app.pw();
        for b in &bands {
            assert!(b.x >= -1.0 && b.x + b.w <= pw + 1.0, "band {b:?} sits within the page width {pw}");
            assert!(b.w > 0.0 && b.h > 0.0, "band {b:?} is non-degenerate");
        }
    }

    #[test]
    #[serial(pdfium)]
    fn save_back_flattens_markup_into_a_reopenable_pdf() {
        let mut app = open_raster_sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();

        let before = app
            .raster
            .as_ref()
            .unwrap()
            .page_object_count(0)
            .expect("page 1 object count");

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
        app.commit_drag(&mut st.doc);
        app.create_note(&mut st.doc, 1, (120.0, 300.0));
        app.edit_insert("Check this claim");
        app.commit_edit(&mut st.doc);
        app.signer_name = "Grace Hopper".into();
        app.place_signature(&mut st.doc, 1, (360.0, 520.0));
        assert!(app.annotations.iter().any(|a| matches!(a.kind, AnnoKind::Highlight { .. })));
        assert!(app.signatures.iter().any(|s| s.signer == "Grace Hopper"));

        // Flatten + write a real .pdf to disk.
        let out = std::env::temp_dir().join("qos_pdf_saveback").join("annotated.pdf");
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&out);
        assert!(
            app.save_annotated_to(&out),
            "save_annotated_to reports success (note: {:?})",
            app.status_note
        );
        assert!(out.exists(), "the annotated .pdf exists on disk");
        assert!(std::fs::metadata(&out).unwrap().len() > 0, "and is non-empty");

        // Reopen the saved file with a fresh pdfium document and prove the
        // markup is really in the file: page 1 gained content objects, and the
        // signer name + note body are extractable text (so any viewer sees them).
        let reopened = crate::source::open(&out).expect("the saved PDF reopens");
        let mut rz = reopened.raster.expect("the saved PDF reopens via pdfium");
        let after = rz.page_object_count(0).expect("reopened page 1 object count");
        assert!(after > before, "flattening added page content ({before} -> {after})");
        let text = rz.page_text(0).text.clone();
        assert!(
            text.contains("Grace Hopper"),
            "the signer name is baked into the saved page text: {text:?}"
        );
        assert!(
            text.contains("Check this claim"),
            "the note body is baked into the saved page text"
        );
    }

    /// Prepare an opened raster sample for page-editing tests: sync + lay out so
    /// `current_page` and region rects are valid.
    fn open_laid_out_sample() -> (PdfApp, ui::State) {
        let mut app = open_raster_sample();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        (app, st)
    }

    fn saved_reopen(app: &mut PdfApp, name: &str) -> crate::source::Rasterizer {
        let out = std::env::temp_dir().join("qos_pdf_edit").join(name);
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&out);
        assert!(app.save_annotated_to(&out), "save: {:?}", app.status_note);
        assert!(!app.pages_dirty(), "a successful save clears the page-dirty flag");
        crate::source::open(&out)
            .expect("the edited PDF reopens")
            .raster
            .expect("the edited PDF reopens via pdfium")
    }

    #[test]
    #[serial(pdfium)]
    fn reorder_persists_across_save_and_reopen() {
        let (mut app, _st) = open_laid_out_sample();
        let n = app.doc.page_count();
        assert!(n >= 2, "the fixture must have at least two pages to test reordering");
        let first = app.raster.as_mut().unwrap().page_text(0).text.trim().to_string();
        let last = app.raster.as_mut().unwrap().page_text(n - 1).text.trim().to_string();
        assert_ne!(first, last, "the two pages carry distinct text so a swap is observable");

        // Move the first page to the end — the shared rail's Reorder event maps
        // straight onto this production method.
        app.reorder_pages(0, n - 1);
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

    #[test]
    #[serial(pdfium)]
    fn delete_page_persists_across_save_and_reopen() {
        let (mut app, _st) = open_laid_out_sample();
        let n = app.doc.page_count();
        assert!(n >= 2);
        let second = app.raster.as_mut().unwrap().page_text(1).text.trim().to_string();

        app.delete_page_at(0);
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

    #[test]
    #[serial(pdfium)]
    fn duplicate_page_persists_across_save_and_reopen() {
        let (mut app, _st) = open_laid_out_sample();
        let n = app.doc.page_count();
        let first = app.raster.as_mut().unwrap().page_text(0).text.trim().to_string();

        app.duplicate_page_at(0);
        assert_eq!(app.doc.page_count(), n + 1, "duplicate adds one page");

        let mut rz = saved_reopen(&mut app, "duplicated.pdf");
        assert_eq!(rz.page_count(), n + 1, "the duplicate persisted");
        assert_eq!(rz.page_text(0).text.trim(), first);
        assert_eq!(rz.page_text(1).text.trim(), first, "page 2 is a copy of page 1");
    }

    #[test]
    #[serial(pdfium)]
    fn import_image_adds_a_page_that_survives_save() {
        let (mut app, _st) = open_laid_out_sample();
        let n = app.doc.page_count();

        // A genuine PNG on disk, imported through the real code path.
        let img = image::RgbaImage::from_pixel(96, 64, image::Rgba([210, 60, 60, 255]));
        let png = std::env::temp_dir().join("qos_pdf_edit").join("swatch.png");
        std::fs::create_dir_all(png.parent().unwrap()).unwrap();
        image::DynamicImage::ImageRgba8(img).save(&png).expect("write the test PNG");

        assert!(app.import_file(&png), "import: {:?}", app.status_note);
        assert_eq!(app.doc.page_count(), n + 1, "the image import added a page");
        assert!(app.pages_dirty());

        let rz = saved_reopen(&mut app, "with_image.pdf");
        assert_eq!(rz.page_count(), n + 1, "the imported image page persisted");
    }

    #[test]
    #[serial(pdfium)]
    fn import_pdf_appends_all_its_pages() {
        let (mut app, _st) = open_laid_out_sample();
        let n = app.doc.page_count();
        // Import the sample into itself: page count should exactly double.
        assert!(app.import_file(std::path::Path::new(SAMPLE_PDF)), "import: {:?}", app.status_note);
        assert_eq!(app.doc.page_count(), n * 2, "every page of the imported PDF was added");

        let rz = saved_reopen(&mut app, "merged.pdf");
        assert_eq!(rz.page_count(), n * 2, "the merged page set persisted");
    }

    #[test]
    #[serial(pdfium)]
    fn copy_image_region_crops_the_page_bitmap() {
        let (mut app, _st) = open_laid_out_sample();
        let (pw, ph) = app.raster.as_ref().unwrap().page_size(0);

        // Marquee the upper-left portion of page 1 and copy it.
        app.region = Some(RegionSelect { page: 1, start: (40.0, 40.0), cur: (pw * 0.6, ph * 0.5) });
        assert!(app.copy_region(), "a usable region is cropped: {:?}", app.status_note);

        // The pure crop path yields a correctly-sized sub-image of the page.
        let bucket = ((pw * 2.0).round() as u32).clamp(96, 4096);
        let img = app.raster.as_mut().unwrap().render(0, bucket).expect("page 1 renders");
        let rect = PtRect::from_corners(40.0, 40.0, pw * 0.6, ph * 0.5);
        let (w, h, rgba) = render::crop_page_region(&img, pw, ph, rect).expect("crop succeeds");
        assert!(w > 0 && h > 0);
        assert_eq!(rgba.len(), w * h * 4, "RGBA buffer matches the cropped dimensions");
        assert!(
            w < img.width as usize && h < img.height as usize,
            "the crop is a proper sub-region of the full page bitmap"
        );
    }

    /// The note/comment card header must never overlap — the author name is
    /// bounded and truncated so it can't run under the Resolve chip or the ×
    /// button, whatever its length. Asserts on the production layout helper.
    #[test]
    fn comment_header_never_overlaps_even_with_a_long_name() {
        let app = PdfApp::sample();
        // A representative inner card rect for the 324px sidebar.
        let card = Rect::new(0.0, 0.0, SIDEBAR_W - 24.0, 120.0);
        let hdr = app.comment_header_layout(card);

        assert!(
            hdr.name_x + hdr.name_max as f64 <= hdr.chip.x0,
            "the author name stops before the resolve chip (name ends {}, chip at {})",
            hdr.name_x + hdr.name_max as f64,
            hdr.chip.x0
        );
        assert!(hdr.chip.x1 <= hdr.del.x0, "the resolve chip stops before the delete button");
        assert!(hdr.del.x1 <= card.x1, "the delete button stays inside the card");
        assert!(
            hdr.body_top >= hdr.avatar_c.y + 9.0,
            "the body starts below the avatar, not overlapping it"
        );

        // A very long name/email truncates to fit the bounded width.
        let mut tr = ui::text_render::TextRenderer::new();
        let long = "quinten.van.der.berg.reviewer@really-long-domain.example.com";
        let shown = truncate_to_width(&mut tr, long, 11.5, hdr.name_max);
        let (w, _) = tr.measure(&shown, 11.5, None);
        assert!(
            w as f32 <= hdr.name_max + 0.5,
            "the truncated name ({shown:?}, {w}px) fits within the bounded width {}px",
            hdr.name_max
        );
        assert!(shown.chars().count() < long.chars().count(), "the long name was actually truncated");
    }

    /// Drive the real fixture PDF through the live frame path to produce the
    /// review shots: a search hit over genuine glyphs, a placed highlight +
    /// signature, and the *reopened saved* file rendered by pdfium with the
    /// flattened markup baked in. PNGs are written only under `PDF_SHOTS=1`
    /// (to `$PDF_SHOT_DIR`, default a temp dir); the assertions always run.
    #[test]
    #[serial(pdfium)]
    fn drive_real_pdf_search_highlight_save_shots() {
        let shot_dir = std::env::var("PDF_SHOT_DIR").unwrap_or_else(|_| {
            std::env::temp_dir().join("qos_pdf_review").to_string_lossy().into_owned()
        });
        let shot = |name: &str| format!("{shot_dir}/{name}");

        let mut app = open_raster_sample();
        let mut st = ui::State::new();
        full_frame(&mut app, &mut st, 1280.0, 800.0, None);

        // --- search a real word and shoot the hit over genuine glyphs. ---
        let word = {
            let layer = app.raster.as_mut().unwrap().page_text(0);
            first_word(&layer.text, 4).expect("a searchable word")
        };
        app.open_search();
        app.query = word.clone();
        app.recompute_matches();
        assert!(!app.raster_matches.is_empty(), "search finds {word:?} on the real PDF");
        let p_search = shot("real_search_hit.png");
        full_frame(&mut app, &mut st, 1280.0, 800.0, Some(p_search.as_str()));
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
        app.commit_drag(&mut st.doc);
        app.signer_name = "Grace Hopper".into();
        app.place_signature(&mut st.doc, 1, (360.0, 520.0));
        app.go_to_page(1);
        let p_anno = shot("real_annotated.png");
        full_frame(&mut app, &mut st, 1280.0, 800.0, Some(p_anno.as_str()));

        // --- save, reopen the saved file, and shoot pdfium's own render of the
        //     flattened markup — proof the annotations are in the .pdf. ---
        let out = std::env::temp_dir().join("qos_pdf_review").join("annotated.pdf");
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        assert!(app.save_annotated_to(&out), "save succeeds: {:?}", app.status_note);

        let mut app2 = PdfApp::open(&out);
        assert!(app2.raster.is_some(), "the saved file reopens via pdfium");
        let mut st2 = ui::State::new();
        let p_reopen = shot("real_reopened_saved.png");
        full_frame(&mut app2, &mut st2, 1280.0, 800.0, Some(p_reopen.as_str()));
        let baked = app2.raster.as_mut().unwrap().page_text(0).text.clone();
        assert!(baked.contains("Grace Hopper"), "reopened saved file carries the signature text");
        eprintln!(
            "DRIVE real-pdf: word={word:?}  hits={}  saved={}  reopened-has-signature={}",
            app.raster_matches.len(),
            out.display(),
            baked.contains("Grace Hopper")
        );
    }
}
