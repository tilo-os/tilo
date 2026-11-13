//! The PDF reader's root widget: one full-window widget that lays out and draws the
//! page rail, the canvas and the sidebar itself, with its controls as slots in the
//! window's bar. Durable markup lives in the shared document.

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

/// The gap between stacked pages, in content space, so it scales with zoom.
const PAGE_GAP: f64 = 22.0; // page points between pages, not UI spacing
pub(crate) const ZOOM_MIN: f64 = 0.25;
pub(crate) const MOST_READ_PAGES: usize = 50;
pub(crate) const ZOOM_MAX: f64 = 16.0;
const RASTER_SS: f64 = tilo_page_raster::SUPERSAMPLE;
const MAX_NEW_PAGE_RASTERS_PER_FRAME: usize = tilo_page_raster::Admission::FRAME.pages;

type RasterFrame = (
    HashMap<usize, PenikoImage>,
    HashMap<usize, PenikoImage>,
    HashMap<usize, render::PageTiles>,
);

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
const MAX_NEW_THUMB_RASTERS_PER_FRAME: usize = tilo_page_raster::Admission::FRAME.thumbnails;

#[cfg(test)]
const APP_ID: &str = "so.tilo.pdf-viewer";

fn now_ms() -> i64 {
    i64::try_from(ui::time::unix_now_ms()).unwrap_or(0)
}

fn today_ymd() -> String {
    ui::clock::today().format("%Y-%m-%d").to_string()
}

pub(crate) fn ymd_from_unix_ms(ms: i64) -> String {
    ymd_from_unix_secs(ms.div_euclid(1000))
}

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
    fn name_key(self) -> &'static str {
        match self {
            Self::Comments => "pdf-tab-comments",
            Self::Outline => "pdf-tab-contents",
            Self::Signatures => "pdf-tab-signing",
        }
    }

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

#[derive(Clone, Copy)]
struct PanState {
    start: Point,
    scroll_x0: f64,
    scroll_y0: f64,
    moved: bool,
}

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
    SigMode(SigMode),
    SigClear,
}

#[derive(Clone, PartialEq)]
enum EditTarget {
    CommentBody(String),
    Reply(String),
    Signer,
    FormField(String),
    FreeText(String),
}

#[derive(Clone, Debug)]
pub(crate) struct FieldWidgetHit {
    pub(crate) name: String,
    pub(crate) kind: tilo_pdf::FieldKind,
    pub(crate) rect: PtRect,
    pub(crate) export: String,
    pub(crate) options: Vec<String>,
    pub(crate) read_only: bool,
    pub(crate) pdf_value: String,
    pub(crate) pdf_checked: bool,
}

struct Editing {
    target: EditTarget,
    input: EditInput,
}

enum EditInput {
    Plain(Box<TextInput>),
    Rich(Box<CommentField>),
}

fn comment_view_key(comment: &str, reply: Option<&str>) -> String {
    match reply {
        Some(reply) => format!("{comment}\u{1f}{reply}"),
        None => comment.to_owned(),
    }
}

pub(crate) type CommentField = ui::widget::Embedded<RichTextSurface<Draft>>;

impl Editing {
    fn text(&self) -> String {
        match &self.input {
            EditInput::Plain(input) => input.value().to_owned(),
            EditInput::Rich(field) => field.widget().source().plain_text(),
        }
    }

    fn is_dragging(&self) -> bool {
        match &self.input {
            EditInput::Plain(input) => input.is_dragging(),
            EditInput::Rich(_) => false,
        }
    }

    fn take_submit(&mut self) -> bool {
        match &mut self.input {
            EditInput::Plain(input) => input.take_submit(),
            EditInput::Rich(field) => {
                field.widget_mut().take_outcome() == Some(FieldOutcome::Submit)
            }
        }
    }

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
    bar: ui::framework::BarCommands<PdfEffect>,
    raster_handle: Option<RasterHandle>,
    raster_document: Option<RasterDocument>,
    pending_open: Option<OpenRequest>,
    raster_images: crate::page_images::PageImages,
    raster_render_pending: HashSet<(usize, u32)>,
    raster_tiles: crate::tiles::TileCache<UiTileKey>,
    raster_tile_batch: Vec<crate::source::TileRequest>,
    raster_render_batch: Vec<(usize, u32)>,
    raster_heading: tilo_page_raster::reach::Heading,
    raster_wake_bound: bool,
    scrolled: bool,
    delivered_pages: Vec<usize>,
    delivered_thumbs: bool,
    shown: ui::scroll_copy::ShownFrames<CanvasShown>,
    raster_text_pending: HashSet<usize>,
    raster_links: std::collections::HashMap<usize, Arc<Vec<crate::source::PageLink>>>,
    raster_links_pending: HashSet<usize>,
    pending_worker_plan: Option<Vec<PagePlanEntry>>,
    applied_worker_plan: Option<Vec<PagePlanEntry>>,
    worker_plan_blobs: Vec<ContentState>,
    pending_save: Option<SaveRequest>,
    pending_import: Option<ImportRequest>,
    asked_import: Option<insert_pages::AskedImport>,
    has_document: bool,
    record_source: Option<crate::markup::PinnedSource>,
    reopen_requested: bool,
    /// Where the record's pinned source is being fetched from, and the arrivals count
    /// last checked; looked for again when that moves, never on a timer.
    source_wait: Option<(tilo_content::ContentState, u64)>,
    staged_opening: bool,
    staged_import: bool,
    doc_name: String,
    load_error: Option<String>,
    content: ContentStore,
    shots_rail_drag: bool,
    shots_context_menu: bool,
    shots_panel: Option<SidebarTab>,
    #[cfg(feature = "samples")]
    staged_scenario: Option<StagedScenario>,
    annotations: Vec<Annotation>,
    comments: Vec<Thread>,
    signatures: Vec<Signature>,

    // View state: local, never in the shared document.
    rect: Rect,
    fit: FitMode,
    viewport: ViewportState,
    current_page: usize,
    pending_reading: Option<(String, f64, f64)>,
    position_restored: bool,
    last_reading: Option<(String, f64, f64)>,
    resource_page_ids: Vec<String>,
    resource_page_sources: Vec<usize>,
    resource_page_imports: Vec<Option<crate::markup::ImportedPage>>,
    source_page_count: usize,
    page_text: HashMap<String, String>,
    page_text_revision: u64,
    search_reads_text: bool,
    /// The open document's text for search, rebuilt on state sync when the document or
    /// the find results change, never on a keystroke.
    search_entries: std::rc::Rc<Vec<ui::tilo_resource::SearchEntry>>,
    search_projection_revision: Option<SearchProjectionRevision>,
    #[cfg(test)]
    search_projection_refreshes: usize,
    tool: Tool,
    swatch: HighlightColor,
    markup_style: model::MarkupStyle,
    sidebar_tab: SidebarTab,
    show_sidebar: bool,
    review: ui::widget::SidebarDrawer,
    show_rail: bool,
    rail_w: f64,
    page_sidebar: PageSidebar,
    pages_dirty: bool,
    portal_import_rx: Option<ContentsChannel>,
    selected_anno: Option<String>,
    editing: Option<Editing>,
    comment_views: std::cell::RefCell<HashMap<String, RichTextView>>,
    rich_inset: std::cell::Cell<Option<((f32, f32), f64)>>,
    selected_sig: Option<String>,
    sidebar_scroll: f64,
    sidebar_reveal: Option<String>,
    signer_name: String,
    author: String,

    sig_mode: SigMode,
    sig_strokes: Vec<Vec<(f64, f64)>>,
    sig_stroke: Option<Vec<(f64, f64)>>,
    /// The person's own signature record: it follows them to their other devices and
    /// never reaches a collaborator. Absent where the host keeps no person records.
    signature: Option<ui::framework::PersonRecord<crate::signature::SignatureProfile>>,
    signature_revision: Option<u64>,
    sig_pad_rect: Rect,

    find_query: String,
    own_find: Option<ui::widget::OwnFind>,
    active_match: usize,
    search_whole_word: bool,
    search_case_sensitive: bool,
    raster_matches: Vec<TextMatch>,

    raster_text: std::collections::HashMap<usize, Arc<PageText>>,
    selection: Option<TextSelection>,
    drag_select: DragSelect,
    sel_gran: Granularity,
    sel_anchor_range: (TextPos, TextPos),
    pending_press: Option<PendingPress>,
    pending_copy: bool,
    pending_selection_effect: Option<PendingSelectionEffect>,
    text_hover: Option<usize>,
    raster_text_batch: Vec<usize>,
    selection_autoscroll: ui::edge_autoscroll::EdgeAutoscroll,
    canvas_hover_zones: Vec<Rect>,
    glide: ui::ScrollMomentum,
    interaction: ui::interaction::InteractionEnvironment,
    region: Option<RegionSelect>,
    region_waiting: Option<(u64, RegionSelect)>,
    shift_down: bool,

    source_path: Option<std::path::PathBuf>,
    status_note: Transient<String>,
    note_action: Option<(String, note_action::NoteAction)>,
    region_copy: ui::CopyWatch,
    portal_save_rx: Option<ui::portal::DestinationChannel>,
    save_destination: Option<ui::portal::SaveDestination>,
    save_write_rx: Option<(String, ui::portal::SaveChannel)>,
    portal_action: Option<ui::framework::EffectReply>,

    last_mouse: Point,
    drag: Option<DragState>,
    pan: Option<PanState>,
    rail_drag: Option<f64>,
    context_menu: Option<ContextMenu>,
    dirty: bool,
    pending_visuals: bool,

    reduce_motion: bool,
    layout_theme: Theme,
    tab_widths: Option<[f64; 3]>,
    empty_open_button: std::cell::Cell<Rect>,
    workspace_documents: Vec<WorkspaceDocument>,
    empty_document_rows: std::cell::RefCell<Vec<Rect>>,
    file_open: ui::widget::FileOpenSurface,

    /// The host's file picker while it is open: the chosen path comes back here and is
    /// drained each sync. The app never draws a picker of its own.
    portal_rx: Option<PortalChannel>,
    portal_contents_rx: Option<ContentsChannel>,
    open_here: Option<ui::framework::OpenHere>,
    pending_reopen: Option<(std::path::PathBuf, ui::application::ReplacementOutcome)>,
    pending_document_open: Option<(String, ui::application::ReplacementOutcome)>,

    overlay_mode: bool,
    prev_overlay: Option<bool>,

    rail_rect: Rect,
    canvas_rect: Rect,
    sidebar_rect: Rect,

    page_notice: Transient<String>,
    page_said: Option<(usize, u32)>,

    sidebar_hits: Vec<(SidebarHit, Rect)>,
    edit_rect: Rect,

    sel_hud: Option<ui::SelectionHud>,
    marker_drag: Option<ui::MarkerDrag>,

    people: Option<ui::framework::People>,
    dispatcher: Option<ui::framework::Dispatcher<crate::markup::PdfMarkup>>,
    read_only: bool,
    read_only_state: Option<ui::LocalState<bool>>,
    live: Option<ui::framework::Live<crate::markup::PdfMarkup>>,
    presented: Option<(u64, u64)>,
    store: std::rc::Rc<std::cell::RefCell<shared::ReaderStore>>,
    page_maps: Vec<model::PageMap>,
    resource_page_rotations: Vec<i32>,
    form_fields: Vec<tilo_pdf::FormField>,
    form_values: std::collections::BTreeMap<String, model::FormEntry>,
    source_blob: Option<ui::tilo_resource::Bytes>,
    stored_annotations: Vec<Annotation>,
    adopted: Vec<crate::adopt::AdoptedEntry>,
    existing_request: Option<String>,
    opens: std::rc::Rc<std::cell::RefCell<Vec<ui::tilo_resource::Id>>>,
    seed_pending: bool,
    reader_markup_hidden: bool,
}

pub(crate) fn default_content() -> ContentStore {
    ContentStore::memory()
}

impl PdfApp {
    pub fn open(path: impl AsRef<std::path::Path>) -> Self {
        Self::open_with_content(path, default_content())
    }

    pub fn open_with_content(path: impl AsRef<std::path::Path>, content: ContentStore) -> Self {
        let mut app = Self::empty_with_content(content);
        app.queue_worker_open(path.as_ref());
        app
    }

    pub fn empty() -> Self {
        Self::empty_with_content(default_content())
    }

    pub fn empty_with_content(content: ContentStore) -> Self {
        Self::build(
            model::Document::blank_pages("", "", 1, PAGE_W, PAGE_H),
            content,
        )
    }

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

    pub fn past() -> Self {
        let mut app = Self::empty_with_content(ContentStore::memory());
        app.raster_handle = None;
        app
    }

    #[cfg(feature = "samples")]
    pub(crate) fn stage(&mut self, scenario: Option<&str>) {
        self.staged_scenario = Some(StagedScenario {
            name: scenario.map(str::to_owned),
        });
        self.stage_when_ready();
    }

    #[cfg(feature = "samples")]
    fn stage_when_ready(&mut self) {
        let Some(StagedScenario { name: scenario }) = self.staged_scenario.clone() else {
            return;
        };
        if crate::scenarios::shows_sample(scenario.as_deref()) && self.load_error.is_none() {
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
        if self.has_document {
            self.show_sidebar = true;
        }
        self.dirty = true;
    }

    fn opening(&self) -> bool {
        self.pending_open.is_some() || self.staged_opening
    }

    #[cfg(feature = "samples")]
    pub fn arm_shots_rail_drag(&mut self) {
        self.shots_rail_drag = true;
        self.show_rail = true;
    }

    #[cfg(feature = "samples")]
    pub fn arm_shots_page_bubble(&mut self) {
        self.page_notice.show(self.page_line());
    }

    #[cfg(feature = "samples")]
    pub fn arm_shots_context_menu(&mut self) {
        self.shots_context_menu = true;
    }

    #[cfg(feature = "samples")]
    pub fn arm_shots_search(&mut self) {
        use ui::widget::Findable as _;
        let found = self.find("godwit", ui::widget::FindOptions::default());
        self.reveal(2.min(found.len().saturating_sub(1)));
    }

    #[cfg(feature = "samples")]
    pub fn arm_shots_sign_compose(&mut self) {
        self.show_sidebar = true;
        self.sidebar_tab = SidebarTab::Signatures;
        self.tool = Tool::Sign;
        self.sig_mode = SigMode::Draw;
        self.sig_strokes = crate::scenarios::sample_drawn_signature();
    }

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

    fn demo_import_image(&mut self) {
        self.show_rail = true;
        if !self.raster_active() {
            return;
        }
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
            // A preview rail, 168 px at 1.5× UI scale.
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

    /// Submit a field edit through the dispatcher, from a pass that only reads the
    /// document; a refusal is reported by the dispatcher.
    pub(crate) fn submit(&self, invoke: ui::tilo_resource::Invoke<PdfMarkup>) {
        if let Some(dispatcher) = self.dispatcher.as_ref() {
            dispatcher.submit(invoke);
        }
    }

    /// Apply a field edit now, from an input event, and say why when it is refused.
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

    pub(crate) fn adopt_resource_state(&mut self, state: &crate::markup::PdfMarkup) {
        let source = state.pinned_source();
        if self.record_source != source {
            self.record_source = source;
            self.source_wait = None;
            self.pending_reading = state.reading_position();
            self.position_restored = false;
            self.last_reading = None;
        }
        self.dirty = true;
    }

    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::markup::PdfMarkup>,
    ) {
        self.read_only = dispatcher.read_only();
        if let Some(me) = dispatcher.me() {
            me.clone_into(&mut self.author);
        }
        self.dispatcher = Some(dispatcher);
    }

    pub(crate) fn attach_open_here(&mut self, open_here: Option<ui::framework::OpenHere>) {
        self.open_here = open_here;
    }

    pub(crate) fn attach_people(&mut self, people: ui::framework::People) {
        self.people = Some(people);
        self.dirty = true;
    }

    pub(crate) fn author_name(&self, id: &str) -> String {
        if let Some(name) = self.known_name(id) {
            return name;
        }
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

    fn known_name(&self, id: &str) -> Option<String> {
        let name = self
            .people
            .as_ref()
            .map_or_else(|| id.to_owned(), |people| people.name(id));
        (name != id || !opaque_participant_id(id)).then_some(name)
    }

    pub(crate) fn signer(&self) -> String {
        if self.signer_name.is_empty() {
            self.known_name(&self.author).unwrap_or_default()
        } else {
            self.signer_name.clone()
        }
    }

    pub(crate) fn attach_signature(
        &mut self,
        signature: ui::framework::PersonRecord<crate::signature::SignatureProfile>,
    ) {
        self.signature = Some(signature);
        self.signature_revision = None;
    }

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

    fn keep_strokes(&mut self) {
        let invoke = crate::signature::SignatureProfile::SET_STROKES.with(self.sig_strokes.clone());
        self.keep_signature(&invoke);
    }

    pub(crate) fn set_read_only(&mut self, read_only: bool) {
        if self.read_only != read_only {
            self.read_only = read_only;
            self.seed_pending |= !read_only;
        }
    }

    #[cfg(test)]
    pub(crate) fn stage_find_for_test(&mut self, query: &str) {
        ui::widget::Findable::find(self, query, ui::widget::FindOptions::default());
    }

    #[cfg(test)]
    pub(crate) fn find_match_count_for_test(&self) -> usize {
        self.match_count()
    }

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

    /// Rebuild the search entries only when the document or the page ids changed;
    /// searching never traverses text or markup.
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

    fn page_map(&self, page: usize) -> model::PageMap {
        page.checked_sub(1)
            .and_then(|i| self.page_maps.get(i).copied())
            .unwrap_or_else(|| model::PageMap::identity(self.pw(), self.ph()))
    }

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

    fn page_id_of(&self, page: usize) -> Option<String> {
        page.checked_sub(1)
            .and_then(|i| self.resource_page_ids.get(i))
            .cloned()
    }

    fn text_for_card(&self, page: usize, text: Arc<PageText>) -> Arc<PageText> {
        let map = self.page_map(page);
        if map.is_identity() {
            text
        } else {
            Arc::new(text.with_map(map))
        }
    }

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

    fn source_identity(&self) -> Option<String> {
        self.source_blob
            .as_ref()
            .map(|source| hex::encode(source.hash()))
    }

    fn adopt_existing(&mut self, state: &AutoCommit) -> bool {
        let Some(hash) = self.source_identity() else {
            return true;
        };
        let Some(pinned) = PdfMarkup::of(state).pinned_source() else {
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
        self.raster_images.clear();
        self.raster_render_pending.clear();
        self.raster_tiles.clear();
        self.raster_tile_batch.clear();
        self.raster_render_batch.clear();
        self.dirty = true;
    }

    fn export_markup(&self) -> crate::source::ExportMarkup {
        crate::source::ExportMarkup {
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

    fn reload(&mut self, state: &AutoCommit) {
        if self.page_maps.len() != self.doc.page_count() {
            self.rebuild_page_maps();
        }
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
            .content_surface(f64::from(m.detail_min_width))
    }

    fn review_outer(&self) -> Rect {
        let r = self.rect;
        let top = r.y0;
        let (rail_min, rail_max) = self.rail_width_range();
        let x0 = (r.x0 + self.rail_w.clamp(rail_min, rail_max)).min(r.x1);
        Rect::new(x0, top.min(r.y1), r.x1, r.y1)
    }

    fn reconcile_overlay(&mut self) {
        let outer = self.review_outer();
        self.review.resolve(outer);
        let overlay = self.review.is_overlay();
        self.overlay_mode = overlay;
        if self.prev_overlay != Some(overlay) {
            self.show_sidebar = !overlay || self.shots_panel.is_some();
            self.show_rail = !overlay;
            self.prev_overlay = Some(overlay);
        }
        if !self.has_document {
            self.show_sidebar = false;
            self.show_rail = false;
        }
    }

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
        let mid_top = r.y0;
        let mid_bottom = r.y1;
        self.review.snap(self.show_sidebar);
        self.review.resolve(self.review_outer());
        let rail_w = self.rail_width();
        let sidebar_w = self.sidebar_width();
        self.rail_rect = Rect::new(r.x0, mid_top, r.x0 + rail_w, mid_bottom);
        self.sidebar_rect = Rect::new(r.x1 - sidebar_w, mid_top, r.x1, mid_bottom);
        let (x0, x1) = if self.overlay_mode {
            (r.x0, r.x1)
        } else {
            (r.x0 + rail_w, r.x1 - sidebar_w)
        };
        self.canvas_rect = Rect::new(x0, mid_top, x1, mid_bottom);
    }

    /// The zoom: page points to screen px.
    fn zoom(&self) -> f64 {
        self.viewport.zoom()
    }

    fn scroll_x(&self) -> f64 {
        self.viewport.scroll_x()
    }
    fn scroll_y(&self) -> f64 {
        self.viewport.scroll_y()
    }

    fn stack_unit(&self) -> f64 {
        self.ph() + PAGE_GAP
    }

    fn content_stack_h(&self) -> f64 {
        let n = self.doc.page_count() as f64;
        n * self.ph() + (n - 1.0).max(0.0) * PAGE_GAP
    }

    fn sync_viewport_frame(&mut self) {
        self.viewport.set_rect(self.canvas_rect);
        self.viewport
            .set_content_size(self.pw(), self.content_stack_h());
        self.recompute_zoom();
    }

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

    fn recompute_zoom(&mut self) {
        let z = match self.fit {
            FitMode::Page => self.fit_page_zoom(),
            FitMode::Width => self.viewport.fit_width_zoom(),
            FitMode::Custom(z) => z.clamp(ZOOM_MIN, ZOOM_MAX),
        };
        let ratio = z / self.zoom();
        let scroll = (self.scroll_x() * ratio, self.scroll_y() * ratio);
        self.viewport.set_zoom(z);
        if !matches!(self.fit, FitMode::Custom(_)) {
            self.viewport.set_scroll(scroll.0, scroll.1);
        }
    }

    fn is_overflowing_x(&self) -> bool {
        self.viewport.overflows_x()
    }

    fn clamp_scroll(&mut self) {
        self.viewport.clamp_scroll();
    }

    fn page_origins(&self) -> Vec<Point> {
        let unit = self.stack_unit();
        (0..self.doc.page_count())
            .map(|i| {
                self.viewport
                    .content_point_to_screen(Point::new(0.0, i as f64 * unit))
            })
            .collect()
    }

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

    fn nearest_page(&self) -> usize {
        let n = self.doc.page_count().max(1);
        let unit = self.stack_unit();
        if unit <= 0.0 {
            return 1;
        }
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

    fn restore_reading(&mut self) {
        if self.position_restored || !self.has_document || self.canvas_rect.width() <= 0.0 {
            return;
        }
        self.position_restored = true;
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

    /// Keep the resume hint current: written when the page settles, never per pixel.
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
        self.scrolled = true;
        if self.selection.is_some_and(|sel| sel.dragging) {
            self.update_selection_at(self.last_mouse);
        }
    }

    fn scroll_by(&mut self, dx: f64, dy: f64) {
        self.viewport.pan_by(dx, dy);
        self.current_page = self.nearest_page();
        self.dirty = true;
    }

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

    fn seed_record(&mut self, state: &AutoCommit) {
        if self.read_only {
            self.seed_pending = true;
            return;
        }
        if !PdfMarkup::of(state).page_plan_seeded {
            self.submit(PdfMarkup::START_PAGE_PLAN.with(self.source_page_count.max(1)));
        }
        self.pin_source(state);
        self.seed_pending = !self.adopt_existing(state);
    }

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
        self.pages_dirty |= plan_changed;
        self.current_page = self.current_page.clamp(1, self.doc.page_count().max(1));
    }

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

    fn import_picked(&mut self, file: ui::portal::PickedContents) -> bool {
        self.import_from(self.current_page, move |handle, document, at, after| {
            handle.import_contents(document, &file.name, file.bytes, at, after)
        })
    }

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

    fn search_options(&self) -> SearchOptions {
        SearchOptions {
            whole_word: self.search_whole_word,
            case_sensitive: self.search_case_sensitive,
        }
    }

    fn query(&self) -> &str {
        &self.find_query
    }

    fn finding(&self) -> bool {
        !self.find_query.is_empty()
    }

    fn recompute_matches_for(&mut self, query: &str) {
        let opts = self.search_options();
        let query = query.to_string();
        self.active_match = 0;
        if let Some(document) = self.raster_document.as_ref() {
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

    fn document_has_no_text(&self) -> bool {
        let pages = self.doc.page_count();
        pages > 0
            && (1..=pages).all(|page| {
                self.raster_text
                    .get(&page)
                    .is_some_and(|text| text.text.trim().is_empty())
            })
    }

    fn match_count(&self) -> usize {
        self.raster_matches.len()
    }

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
            self.own_find = Some(ui::widget::OwnFind {
                query: query.to_owned(),
                options,
                current: index,
            });
            Self::open_search();
        } else {
            self.status_note.show(crate::t("pdf-note-find-result-gone"));
        }
        self.dirty = true;
    }

    fn release_region(&mut self) -> bool {
        self.region
            .take()
            .is_some_and(|region| self.copy_region(region))
    }

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
            return;
        }
        if !self.ask_region_render(region) {
            self.status_note.show(crate::t("pdf-note-area-not-copied"));
            self.dirty = true;
        }
    }

    fn copy_waiting_region(&mut self) {
        let Some((document_id, region)) = self.region_waiting else {
            return;
        };
        if self
            .raster_document
            .as_ref()
            .is_none_or(|document| document.id != document_id)
        {
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

    fn drain_portal_save(&mut self) {
        let Some(rx) = self.portal_save_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_save_rx = None;
                match outcome {
                    DestinationOutcome::Chosen(destination) if destination.location().is_some() => {
                        let path = ensure_pdf_extension(
                            destination
                                .location()
                                .map(Path::to_path_buf)
                                .unwrap_or_default(),
                        );
                        let success = self.save_annotated_to(&path);
                        if self.pending_save.is_none() {
                            self.finish_portal_action(
                                success,
                                (!success).then(|| "PDF could not be saved".into()),
                            );
                        }
                    }
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

    fn save_annotated_to(&mut self, path: &std::path::Path) -> bool {
        self.submit_save(path, false)
    }

    fn submit_save(&mut self, path: &std::path::Path, deliver: bool) -> bool {
        if self.pending_worker_plan.is_some() {
            self.status_note
                .show(crate::t("pdf-note-save-waiting-pages"));
            self.dirty = true;
            return false;
        }
        let Some(document) = self.raster_document.as_ref() else {
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
                    self.reload(state);
                    self.dirty = true;
                    return;
                }
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
        self.focus_sidebar(SidebarTab::Comments, comment_id.clone());
        self.reload(state);
        self.begin_edit(EditTarget::CommentBody(comment_id), "");
        self.dirty = true;
    }

    fn begin_edit(&mut self, target: EditTarget, initial: &str) {
        if let EditTarget::CommentBody(id) | EditTarget::Reply(id) = &target {
            let blocks = match &target {
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
                    self.delete_target(&DeleteTarget::Annotation(id), state);
                    return;
                }
                if let Some(action) =
                    crate::actions::set_free_text_action(&PdfMarkup::of(state), &id, text)
                {
                    self.perform_action(state, action);
                }
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

    fn focus_sidebar(&mut self, tab: SidebarTab, id: String) {
        self.sidebar_tab = tab;
        if !self.show_sidebar {
            self.show_sidebar = true;
            self.reflow();
        }
        self.sidebar_reveal = Some(id);
        self.dirty = true;
    }

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

    fn signature_ink_rect(&self) -> Rect {
        self.sig_pad_rect
            .inset(-f64::from(self.layout_theme.tokens.spacing.s3))
    }

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
        self.focus_sidebar(SidebarTab::Signatures, sig_id);
        self.reload(state);
        self.dirty = true;
    }

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

    pub(crate) fn field_at(&self, page: usize, pt: (f64, f64)) -> Option<FieldWidgetHit> {
        self.field_widgets_on(page)
            .into_iter()
            .find(|w| w.rect.contains(pt.0, pt.1))
    }

    pub(crate) fn field_text(&self, w: &FieldWidgetHit) -> String {
        match self.form_values.get(&w.name) {
            Some(model::FormEntry::Text(t) | model::FormEntry::Choice(t)) => t.clone(),
            _ => w.pdf_value.clone(),
        }
    }

    pub(crate) fn field_checked(&self, w: &FieldWidgetHit) -> bool {
        match self.form_values.get(&w.name) {
            Some(model::FormEntry::Checked { on, export }) => match (w.kind, export) {
                (tilo_pdf::FieldKind::RadioButton, Some(e)) => *on && *e == w.export,
                _ => *on,
            },
            _ => w.pdf_checked,
        }
    }

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
            self.raster_links_pending.remove(&(page - 1));
        }
    }

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

    fn note_marker_at(&self, page: usize, pt: (f64, f64)) -> Option<(String, f64, f64)> {
        let on_page = || self.annotations.iter().filter(|a| a.page == page);
        on_page()
            .find_map(|a| match &a.kind {
                AnnoKind::Note { x, y } if (pt.0 - x).abs() < 24.0 && (pt.1 - y).abs() < 24.0 => {
                    Some((a.id.clone(), *x, *y))
                }
                _ => None,
            })
            .or_else(|| {
                on_page().find_map(|a| match &a.kind {
                    AnnoKind::FreeText { rect, .. } if rect.contains(pt.0, pt.1) => {
                        Some((a.id.clone(), rect.x, rect.y))
                    }
                    _ => None,
                })
            })
    }

    fn is_free_text(&self, id: &str) -> bool {
        self.annotations
            .iter()
            .any(|a| a.id == id && matches!(a.kind, AnnoKind::FreeText { .. }))
    }

    fn anno_page(&self, id: &str) -> Option<usize> {
        self.annotations.iter().find(|a| a.id == id).map(|a| a.page)
    }

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

    fn zoom_by(&mut self, factor: f64) {
        self.viewport.zoom_by(factor);
        self.fit = FitMode::Custom(self.viewport.zoom());
        self.current_page = self.nearest_page();
        self.dirty = true;
    }

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

    fn delete_target(&mut self, target: &DeleteTarget, state: &mut ui::widget::Edits<'_>) {
        match target {
            DeleteTarget::Annotation(id) => {
                self.perform_action(state, crate::actions::delete_annotation_action(id));
                if self.selected_anno.as_deref() == Some(id.as_str()) {
                    self.selected_anno = None;
                }
            }
            DeleteTarget::Comment(id) => {
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

    fn delete_target_at(&self, page: usize, pt: (f64, f64)) -> Option<DeleteTarget> {
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
                        if self.pending_open.is_none() {
                            self.finish_portal_action(self.has_document, self.load_error.clone());
                        }
                    }
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
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
            }
        }
    }

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

    fn holds_document(&self) -> bool {
        self.has_document || self.pending_open.is_some() || self.record_source.is_some()
    }

    fn open_chosen(&mut self, path: &std::path::Path) {
        if self.source_path.as_deref() == Some(path) {
            self.finish_portal_action(true, None);
            return;
        }
        if !self.ask_to_open_here(path) {
            self.queue_worker_open(path);
        }
    }

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
                    self.pending_open = None;
                    self.dirty = true;
                }
                RasterEvent::Opened { request, .. } => {
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
                    self.raster_text.insert(request.page + 1, text);
                    self.text_arrived(request.page + 1);
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

    fn empty_open_button_rect(&self) -> Rect {
        self.empty_open_button.get()
    }
}

impl Default for PdfApp {
    fn default() -> Self {
        Self::empty()
    }
}

fn display_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn saved_note(name: &str) -> String {
    crate::t_args("pdf-note-saved", &[("name", name.into())])
}

fn save_failed_note(name: &str) -> String {
    crate::t_args("pdf-note-save-failed", &[("name", name.into())])
}

fn open_failed_note(path: &std::path::Path) -> String {
    crate::t_args(
        "pdf-note-open-failed",
        &[("name", display_name(path).into())],
    )
}

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
        self.publish_status_note();
        self.page_notice.expire();
        self.page_notice
            .publish(|line| ToolbarNotice::new(line.clone(), ToastKind::Info));
        let (state, _) = ui::doc::document_ref(state, path);
        if !self.raster_wake_bound
            && let Some(handle) = self.raster_handle.as_ref()
        {
            handle.set_wake(ui::wake::WakeHandle::new());
            self.raster_wake_bound = true;
        }
        self.drain_raster_events(state);
        self.copy_waiting_region();
        self.drain_opens();
        self.follow_cancellation();
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
        self.drain_portal_save();
        self.drain_save_write();
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

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.scroll_damage(regions);
    }

    fn needs_animation_frame(&self) -> bool {
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
    fn page_text_key(&self, page: usize) -> Option<String> {
        let index = page.checked_sub(1)?;
        Some(
            match self.resource_page_imports.get(index).cloned().flatten() {
                Some(imported) => format!("{}#{}", imported.hash, imported.page),
                None => format!("#{}", self.resource_page_sources.get(index)?),
            },
        )
    }

    fn keep_page_text(&mut self, page: usize, text: &PageText) {
        let Some(key) = self.page_text_key(page) else {
            return;
        };
        if self.page_text.get(&key) != Some(&text.text) {
            self.page_text.insert(key, text.text.clone());
            self.page_text_revision += 1;
        }
    }

    fn displayed_page_text(&self) -> Vec<&str> {
        (1..=self.resource_page_ids.len())
            .map(|page| {
                self.page_text_key(page)
                    .and_then(|key| self.page_text.get(&key))
                    .map_or("", String::as_str)
            })
            .collect()
    }

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
    #[cfg(test)]
    pub(crate) fn document_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.pdf_resource_ctx()
            .map(|ctx| crate::actions::live_rows(&ctx))
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn document_row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        self.pdf_resource_ctx()
            .and_then(|ctx| crate::actions::live_row(&ctx, id))
    }

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

    fn reading_document(&self) -> Result<(String, tilo_pdf::Document), String> {
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
