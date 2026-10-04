//! Root widget for the Slides app.
//!
//! `SlidesApp` is a single full-window widget that lays out and paints the
//! whole editor itself (toolbar, thumbnail rail, slide canvas, inspector)
//! plus a full-screen present mode. The deck lives in the shared CRDT
//! ([`crate::doc::SlidesDoc`]); this widget keeps an in-memory mirror that
//! is reloaded from the document every `sync`, so undo/redo and live
//! collaboration work without extra plumbing. View state — current slide,
//! selection, mode — is per-peer and stays local.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_rich_text::{FieldOutcome, RichTextField, RichTextSurface, SubmitKey};
use ui::ColorCompat;
use ui::framework::{Dispatcher, TextControl, ViewCx};
use ui::icons::{Icon, draw_icon};
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tilo_resource::ActionDecl;
use ui::widget::Embedded;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    AudiencePresentationProvider, ColorPicker, DEFAULT_BREAKPOINT, FlapState, FullscreenProvider,
    NumberInput, PageSidebar, PageSidebarEvent, Scrollable, SplitLayout, TextInput, Widget,
};

use crate::actions::{self, SlidesDeck};
use crate::assets::ImageStore;
use crate::chrome;
use crate::doc::SlidesDoc;
use crate::help_overlay::HelpOverlay;
use crate::model::{
    Background, Deck, Element, ElementKind, FILL_ROLES, Id, Layout, Palette, PaletteKind, SLIDE_H,
    SLIDE_W, STROKE_ROLES, Slide, TextAlign, new_id, starter_deck,
};
use crate::overview_grid::{OverviewGrid, scroll_from_thumb_drag, scrollbar_rects};
use crate::presentation::{Blank, Command as PresentCommand, Presentation, PresentationFrame};
use crate::render::{self, DeckAssets};
use crate::strings::{tr, tr_args};
use crate::toolbar_tools::{MenuPress, ToolMetrics, ToolbarTools, ToolbarToolsLayout};
use tilo_content::ContentStore;
use ui::i18n::ArgValue;

/// Logical pixels scrolled per mouse-wheel line notch.
const WHEEL_LINE_PX: f64 = 48.0;
/// Compressed presentation bytes are bounded before opening the ZIP.  Archive
/// member and aggregate expansion limits live beside the ODP/PPTX readers.
const MAX_PRESENTATION_FILE_BYTES: usize = 64 * 1024 * 1024;
/// Maximum slide rows materialized into one accessibility frame.
const A11Y_SLIDE_WINDOW: usize = 64;
/// Maximum current-slide objects materialized into one accessibility frame.
const A11Y_ELEMENT_WINDOW: usize = 128;

fn image_open_request() -> PickRequest {
    PickRequest::open(
        &tr("slides-pick-image-title"),
        &tr("slides-pick-image-accept"),
    )
    .thread_name("slides-portal-open")
    .filter(
        FileFilter::new(&tr("slides-pick-images-filter"))
            .glob("*.png")
            .glob("*.jpg")
            .glob("*.jpeg")
            .glob("*.gif")
            .glob("*.webp")
            .glob("*.bmp")
            .glob("*.svg"),
    )
    .all_files()
}

fn presentation_filter() -> FileFilter {
    FileFilter::new(&tr("slides-pick-presentations-filter"))
        .glob("*.odp")
        .glob("*.pptx")
}

fn read_presentation_file(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    crate::assets::read_bounded(path, MAX_PRESENTATION_FILE_BYTES)?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "presentation file is larger than 64 MiB",
        )
    })
}

fn load_presentation_from_path(
    path: &std::path::Path,
    content: &ContentStore,
) -> Result<Deck, String> {
    let bytes = read_presentation_file(path).map_err(|error| error.to_string())?;
    if is_pptx(path) {
        crate::pptx::read_pptx(&bytes, content).map_err(|error| error.to_string())
    } else {
        crate::odp::read_odp(&bytes, content).map_err(|error| error.to_string())
    }
}

fn save_presentation_to_path(
    deck: &Deck,
    path: &std::path::Path,
    content: &ContentStore,
) -> Result<(), String> {
    let (path, bytes) = if is_pptx(path) {
        (
            path.to_path_buf(),
            crate::pptx::write_pptx(deck, content).map_err(|error| error.to_string())?,
        )
    } else {
        (
            ensure_odp_ext(path),
            crate::odp::write_odp(deck, content).map_err(|error| error.to_string())?,
        )
    };
    tilo_files::file_at(&path)
        .and_then(|(dir, name)| tilo_files::blocking::save_document(&dir, &name, &bytes))
        .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DocOutcome {
    Open(PathBuf),
    Save(PathBuf),
    Cancelled,
    Failed(String),
}

/// The archive parser and ZIP writer never run on a compositor/sync frame.
/// Generation tags fence stale imports. Dispatched exports retain separate
/// settlement custody because the worker cannot cancel an external file write.
enum DocumentJob {
    Open {
        generation: u64,
        path: PathBuf,
    },
    Save {
        generation: u64,
        path: PathBuf,
        deck: Deck,
    },
}

enum DocumentJobResult {
    Open {
        generation: u64,
        result: Result<Deck, String>,
    },
    Save {
        generation: u64,
        result: Result<(), String>,
    },
}

struct DispatchedSave {
    generation: u64,
    invocation: Option<tilo_actions::InvocationId>,
}

/// Each job carries a wake for the app that queued it: an idle host syncs,
/// and so drains the result, only when woken.
struct DocumentWorker {
    tx: SyncSender<(DocumentJob, ui::wake::WakeHandle)>,
    rx: Receiver<DocumentJobResult>,
}

impl DocumentWorker {
    fn new(content: ContentStore) -> Result<Self, String> {
        let (tx, jobs) = std::sync::mpsc::sync_channel::<(DocumentJob, ui::wake::WakeHandle)>(1);
        let (results, rx) = std::sync::mpsc::sync_channel(1);
        ui::rt::spawn_thread("slides-document-io", move || {
            while let Ok((job, wake)) = jobs.recv() {
                let result = match job {
                    DocumentJob::Open { generation, path } => DocumentJobResult::Open {
                        generation,
                        result: load_presentation_from_path(&path, &content),
                    },
                    DocumentJob::Save {
                        generation,
                        path,
                        deck,
                    } => DocumentJobResult::Save {
                        generation,
                        result: save_presentation_to_path(&deck, &path, &content),
                    },
                };
                let sent = results.send(result);
                wake.wake();
                if sent.is_err() {
                    break;
                }
            }
        })
        .map(|_| Self { tx, rx })
        .map_err(|error| format!("could not start presentation I/O worker: {error}"))
    }
}

struct DocPortalChannel {
    rx: PortalChannel,
    save: bool,
}

impl DocPortalChannel {
    fn try_recv(&self) -> Result<DocOutcome, TryRecvError> {
        self.rx.try_recv().map(|outcome| match outcome {
            PortalOutcome::Opened(path) if self.save => DocOutcome::Save(path),
            PortalOutcome::Opened(path) => DocOutcome::Open(path),
            PortalOutcome::Cancelled => DocOutcome::Cancelled,
            PortalOutcome::Failed(error) => DocOutcome::Failed(error),
        })
    }
}

fn open_document_request() -> DocPortalChannel {
    DocPortalChannel {
        rx: PickRequest::open(&tr("slides-open-title"), &tr("slides-open-accept"))
            .thread_name("slides-portal-open-doc")
            .filter(presentation_filter())
            .all_files()
            .send(),
        save: false,
    }
}

fn save_document_request(suggested_name: &str) -> DocPortalChannel {
    DocPortalChannel {
        rx: PickRequest::save(
            &tr("slides-save-title"),
            &tr("slides-save-accept"),
            suggested_name,
        )
        .thread_name("slides-portal-save-doc")
        .filter(presentation_filter())
        .send(),
        save: true,
    }
}

/// 1× floors for the editor chrome. Every row is derived from the measured
/// line box of the text it carries plus spacing tokens (see
/// [`ChromeMetrics`]); these floors reproduce the classic density exactly at
/// 1× and only give way when larger text needs the room.
const TOOLBAR_MIN_H: f64 = 56.0;
const TOOLBAR_PILL_MIN_H: f64 = 34.0;
const CONTROL_MIN_H: f64 = 30.0;
const SMALL_PILL_MIN_H: f64 = 28.0;
/// Colour swatches carry no text; they keep one fixed height.
const SWATCH_H: f64 = 26.0;
/// The speaker-notes box never shrinks below this, however tall the
/// property sections above it grow.
const NOTES_MIN_H: f64 = 188.0;
/// Width reserved for the font-size `NumberInput` in the TYPE row.
const FONT_INPUT_W: f64 = 136.0;
/// Deliberately off-scale: the two lines of the toolbar title block sit this
/// close so they read as one label with a caption, not two rows.
const TITLE_LINE_GAP: f64 = 2.0;
/// Base (unscaled) chrome font sizes; the renderer applies the accessibility
/// scale once when it paints them.
const FONT_CAPTION: f32 = 11.0;
const FONT_SUBTITLE: f32 = 11.5;
const FONT_PILL: f32 = 12.0;
const FONT_CONTROL: f32 = 13.0;
const FONT_HEADING: f32 = 16.0;
const FONT_TITLE: f32 = 17.0;
const RAIL_W: f64 = 212.0;
const INSPECTOR_W: f64 = 296.0;
/// Inset of the inspector's content column from the panel edges (top, sides
/// and, below the last section, the bottom of its scrollable content).
const INSPECTOR_PAD: f64 = 16.0;
/// Half-size of a selection handle's *grab* zone (hit-test radius).
const HANDLE: f64 = 9.0;
/// How far outside the selected element's content box the resize handles sit,
/// so a handle never covers the first glyph of left-anchored text (or any
/// content at a corner). The selection ring is drawn at this same outset, so
/// the ring passes through the handle centres and the whole selection
/// decoration reads as a single frame *around* the content rather than over it.
const HANDLE_OFFSET: f64 = 8.0;

/// Below this allocated width (logical px) the editor switches from the
/// docked side-by-side layout to a narrow layout where the slide rail
/// collapses into an overlay flap. The slide canvas plus both side panels
/// (rail + inspector) only fit comfortably above this width.
const NARROW_BREAKPOINT: f64 = DEFAULT_BREAKPOINT;
/// Below this allocated width the inspector also collapses to a right-edge
/// overlay flap. It intentionally matches the rail breakpoint: a compact
/// window must never sacrifice the canvas to two permanent sidebars.
const INSPECTOR_BREAKPOINT: f64 = NARROW_BREAKPOINT;
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Edit,
    /// Slide-sorter: a full-window grid of every slide thumbnail.
    Overview,
    Present,
}

/// One of the two surfaces a multi-output present produces. Returned by
/// [`SlidesApp::present_output_plan`] so a compositor integration knows
/// which view to paint on which output. See the multi-monitor seam doc on
/// `present_output_plan`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresentSurface {
    /// The clean, letterboxed slide shown to the room (larger output).
    Audience,
    /// The presenter dashboard: current + next + notes + timer + position.
    Presenter,
}

/// An on-canvas alignment guide produced by the snapping logic while a
/// drag is live. Coordinates are in slide space; the renderer maps them
/// to the destination rect.
#[derive(Clone, Copy)]
enum Guide {
    Vertical(f64),
    Horizontal(f64),
}

/// Snap threshold in *pixels*; converted to slide units per-drag.
const SNAP_PX: f64 = 7.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Handle {
    Tl,
    T,
    Tr,
    R,
    Br,
    B,
    Bl,
    L,
}

impl Handle {
    const ALL: [Handle; 8] = [
        Handle::Tl,
        Handle::T,
        Handle::Tr,
        Handle::R,
        Handle::Br,
        Handle::B,
        Handle::Bl,
        Handle::L,
    ];
}

#[derive(Clone)]
enum Drag {
    None,
    Move {
        id: Id,
        start: Point,
        orig_x: f64,
        orig_y: f64,
    },
    Resize {
        id: Id,
        handle: Handle,
        start: Point,
        orig: (f64, f64, f64, f64),
    },
    /// Dragging the overview grid scrollbar thumb.
    ScrollOverview {
        start_y: f64,
        orig_scroll: f64,
    },
}

impl Drag {
    fn active(&self) -> bool {
        !matches!(self, Drag::None)
    }
}

#[derive(Clone)]
pub(super) enum Hit {
    NewSlide,
    DuplicateSlide,
    DeleteSlide,
    Present,
    Overview,
    OpenSlide(usize),
    CyclePalette,
    Insert(ElementKind),
    SelectSlide(usize),
    SetFill(String),
    SetStroke(String),
    /// Open the embeddable colour picker popover for the selected element's fill.
    OpenFillPicker,
    ToggleBold,
    ToggleItalic,
    SetAlign(TextAlign),
    BringFront,
    SendBack,
    DuplicateElement,
    DeleteElement,
    SetLayout(Layout),
    SetBackground(Background),
    SetPalette(PaletteKind),
    EditNotes,
    /// Toggle the collapsible slide rail (narrow-window hamburger).
    ToggleRail,
    /// Toggle the collapsible inspector (narrow-window properties flap).
    ToggleInspector,
    /// Toggle the toolbar's insert-tool overflow ("More") menu.
    ToggleToolbarMenu,
}

#[derive(Clone, Copy)]
enum LabelColor {
    Primary,
    Dim,
}

#[derive(Clone)]
struct Label {
    pos: Point,
    text: String,
    font: f32,
    color: LabelColor,
    weight: TextWeight,
    /// When set, the painter elides `text` with the renderer's own metrics to
    /// fit this many final pixels, so layout only has to reserve the slot.
    max_w: Option<f64>,
}

/// Chrome text metrics for the editor's toolbar, rail and inspector, resolved
/// once per frame from the renderer that paints them.
///
/// Slides hands the renderer base font sizes and lets it apply the
/// accessibility scale once, so every value here is in *final* pixels: the
/// measured line box of each chrome font and the measured width of each label
/// the chrome reserves a slot for. `compute_chrome` is renderer-free (event
/// handlers call it too), so it reads these instead of estimating; every row
/// height and label slot it produces therefore matches what is painted and
/// what is hit-tested. `fallback()` stands in until the first draw, whose
/// `refresh_chrome_metrics` replaces it before that frame's chrome is built.
#[derive(Clone, Debug, PartialEq)]
struct ChromeMetrics {
    /// Renderer text scale the widths and line boxes were measured under.
    scale: f64,
    /// Measured line boxes (final px) of the chrome fonts.
    lh_caption: f64,
    lh_subtitle: f64,
    lh_pill: f64,
    lh_control: f64,
    lh_heading: f64,
    lh_title: f64,
    /// Measured label widths keyed by `(text, font × 10, weight)`.
    labels: HashMap<(String, u16, u16), f64>,
    /// Theme spacing tokens.
    s1: f64,
    s2: f64,
    s3: f64,
    s4: f64,
}

impl ToolMetrics for ChromeMetrics {
    fn pill_w(&self, label: &str, icon: bool) -> f64 {
        Self::pill_w(self, label, icon)
    }

    fn menu_row_h(&self) -> f64 {
        self.control_h()
    }
}

impl ChromeMetrics {
    fn key(text: &str, font: f32, weight: TextWeight) -> (String, u16, u16) {
        (
            text.to_owned(),
            (font * 10.0).round() as u16,
            weight.to_u16(),
        )
    }

    /// Resolve every chrome metric against `tr`; `labels` is the set of
    /// `(text, font, weight)` the chrome reserves a slot for.
    fn resolve<'a>(
        tr: &mut TextRenderer,
        theme: &Theme,
        labels: impl IntoIterator<Item = (&'a str, f32, TextWeight)>,
    ) -> Self {
        let line_h = |tr: &mut TextRenderer, font: f32| f64::from(tr.measure("Ag", font, None).1);
        let lh_caption = line_h(tr, FONT_CAPTION);
        let lh_subtitle = line_h(tr, FONT_SUBTITLE);
        let lh_pill = line_h(tr, FONT_PILL);
        let lh_control = line_h(tr, FONT_CONTROL);
        let lh_heading = line_h(tr, FONT_HEADING);
        let lh_title = line_h(tr, FONT_TITLE);
        let labels = labels
            .into_iter()
            .map(|(text, font, weight)| {
                let (w, _) = tr.measure_weighted(text, font, None, weight);
                (Self::key(text, font, weight), f64::from(w))
            })
            .collect();
        let sp = &theme.tokens.spacing;
        Self {
            scale: f64::from(tr.text_scale()),
            lh_caption,
            lh_subtitle,
            lh_pill,
            lh_control,
            lh_heading,
            lh_title,
            labels,
            s1: f64::from(sp.s1),
            s2: f64::from(sp.s2),
            s3: f64::from(sp.s3),
            s4: f64::from(sp.s4),
        }
    }

    /// The 1× metrics used before the first draw has a renderer: every row
    /// lands on its classic floor and label slots fall back to an estimate.
    fn fallback() -> Self {
        let line_h = |font: f32| f64::from(font) * 1.25;
        Self {
            scale: 1.0,
            lh_caption: line_h(FONT_CAPTION),
            lh_subtitle: line_h(FONT_SUBTITLE),
            lh_pill: line_h(FONT_PILL),
            lh_control: line_h(FONT_CONTROL),
            lh_heading: line_h(FONT_HEADING),
            lh_title: line_h(FONT_TITLE),
            labels: HashMap::new(),
            s1: 4.0,
            s2: 8.0,
            s3: 12.0,
            s4: 16.0,
        }
    }

    /// Final-pixel width of `text` at `font`/`weight`: measured when the
    /// renderer has seen it, otherwise a generous per-glyph estimate.
    fn label_w(&self, text: &str, font: f32, weight: TextWeight) -> f64 {
        self.labels
            .get(&Self::key(text, font, weight))
            .copied()
            .unwrap_or_else(|| chrome::est_width(text, f64::from(font)) * self.scale)
    }

    /// A toolbar pill: label plus `s3` each side, plus an icon slot when it
    /// carries one.
    fn pill_w(&self, label: &str, icon: bool) -> f64 {
        let icon_w = if icon { TOOLBAR_ICON + self.s2 } else { 0.0 };
        icon_w + self.label_w(label, FONT_CONTROL, TextWeight::Semibold) + 2.0 * self.s3
    }

    /// Toolbar pills: the control line box plus `s4`, never below 34 px.
    fn pill_h(&self) -> f64 {
        (self.lh_control + self.s4).max(TOOLBAR_PILL_MIN_H)
    }

    /// Toolbar: the two-line title block plus `s2` above and below, never
    /// below 56 px.
    fn toolbar_h(&self) -> f64 {
        (self.title_block_h() + 2.0 * self.s2).max(TOOLBAR_MIN_H)
    }

    /// Title line box, the fixed 2 px seam, and the subtitle line box.
    fn title_block_h(&self) -> f64 {
        self.lh_title + TITLE_LINE_GAP + self.lh_subtitle
    }

    /// Inspector / rail buttons: the control line box plus `s3`, never below
    /// 30 px.
    fn control_h(&self) -> f64 {
        (self.lh_control + self.s3).max(CONTROL_MIN_H)
    }

    /// Layout / background / theme pills: the pill line box plus `s3`, never
    /// below 28 px.
    fn small_pill_h(&self) -> f64 {
        (self.lh_pill + self.s3).max(SMALL_PILL_MIN_H)
    }

    /// Vertical advance of a "SECTION" caption: its line box plus `s2`.
    fn caption_adv(&self) -> f64 {
        self.lh_caption + self.s2
    }

    /// Vertical advance of a panel heading ("TEXT", "SLIDE 3"): its line box
    /// plus `s3`.
    fn heading_adv(&self) -> f64 {
        self.lh_heading + self.s3
    }

    /// The rail header row (the "SLIDES" caption and the overlay's Close
    /// button): a control plus `s1`, never below the classic 34 px.
    fn rail_header_h(&self) -> f64 {
        (self.control_h() + self.s1).max(TOOLBAR_PILL_MIN_H)
    }
}

/// Icon slot inside an icon+label toolbar pill.
const TOOLBAR_ICON: f64 = 18.0;
/// Leading slot of a palette pill: `s2` inset, the 12 px colour dot and a
/// 6 px gap before the label (the dot is painted at `x0+8 .. x0+20`).
const PALETTE_DOT_SLOT: f64 = 26.0;

#[derive(Clone)]
pub(super) enum ButtonKind {
    Text {
        label: String,
        active: bool,
        danger: bool,
    },
    IconText {
        icon: Icon,
        label: String,
        accent: bool,
    },
    IconOnly {
        icon: Icon,
        active: bool,
    },
    Swatch {
        role: String,
        selected: bool,
    },
    Palette {
        kind: PaletteKind,
        selected: bool,
    },
    /// Three-line "hamburger" that summons the collapsed slide rail.
    Hamburger,
    /// One cell of a connected segmented control (e.g. text alignment). The
    /// `pos` decides which outer corners round so the group reads as a single
    /// pill split into cells.
    Segment {
        label: String,
        active: bool,
        pos: SegPos,
    },
}

/// Where a [`ButtonKind::Segment`] sits in its group, controlling corner
/// rounding so only the outer ends are rounded.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SegPos {
    First,
    Mid,
    Last,
}

#[derive(Clone)]
pub(super) struct UiButton {
    pub(super) rect: Rect,
    pub(super) kind: ButtonKind,
    pub(super) hit: Hit,
}

/// Rects the inspector reserves for widgets it doesn't draw as `UiButton`s
/// (the font-size [`NumberInput`], the speaker-notes box).
struct InspectorRects {
    notes: Option<Rect>,
    font_input: Option<Rect>,
}

/// Framework event state shared by the small embedded-widget routers.
///
/// Keeping this boundary explicit prevents every router from mirroring the
/// full [`Widget::handle_event`] signature and makes it clear which values are
/// forwarded unchanged to child widgets.
struct RoutedEvent<'a> {
    event: &'a WindowEvent,
    scale_factor: f64,
    modifiers: Modifiers,
    state: &'a mut AutoCommit,
    path: &'a ObjId,
    focus_target: &'a mut Option<NodeId>,
    node_id: NodeId,
    taffy: &'a TaffyTree<()>,
}

/// The words of one text box being edited on the canvas: the SDK editor,
/// embedded where the box is drawn and laid out at the slide's size, editing
/// the element's rich-text body as the deck's child resource, each edit a
/// `SlidesAction::EditElementText`.
struct TextBoxEditor {
    slide_id: String,
    element_id: String,
    host: Embedded<RichTextSurface<RichTextField>>,
}

impl TextBoxEditor {
    fn open(
        doc: &AutoCommit,
        slide_id: &str,
        element: &Element,
        deck: &Dispatcher<SlidesDeck>,
    ) -> Self {
        let (locate_slide, locate_element) = (slide_id.to_owned(), element.id.clone());
        let field = RichTextField::view_only(move |doc| {
            SlidesDoc::element_body(doc, &locate_slide, &locate_element)
        });
        let surface = RichTextSurface::field(field)
            .with_editor(render::text_box_editor(element))
            .with_submit(SubmitKey::ModEnter);
        let mut editor = Self {
            slide_id: slide_id.to_owned(),
            element_id: element.id.clone(),
            host: Embedded::new(surface),
        };
        editor.attach(deck);
        let surface = editor.host.widget_mut();
        surface.present_from(doc);
        surface.set_focus(true);
        surface
            .editor_mut()
            .focus_block(usize::MAX, ui::widget::CursorPlacement::End);
        editor
    }

    /// Edit through the deck's dispatcher.
    fn attach(&mut self, deck: &Dispatcher<SlidesDeck>) {
        let (id, wrap) = actions::element_text_child(&self.slide_id, &self.element_id);
        self.host
            .widget_mut()
            .source_mut()
            .attach(deck.child::<tilo_document::RichText>(&id, wrap));
    }
}

/// Default size of the fill colour-picker popover, in logical px.
const FILL_PICKER_W: f64 = 260.0;
const FILL_PICKER_H: f64 = 220.0;

struct Chrome {
    toolbar: Rect,
    /// Clickable region of the deck title in the toolbar — a press here starts
    /// the inline title rename. Its top-left is [`Chrome::title_pos`].
    title_rect: Rect,
    /// Baseline top-left the title label is drawn at (used to place the rename
    /// caret so it tracks the real glyphs).
    title_pos: Point,
    /// The rail's painted rect. When the rail is an overlay flap this slides
    /// in from the left edge (off-screen when closed); when docked it is the
    /// permanent left column.
    rail: Rect,
    inspector: Rect,
    /// Unscrolled height of the inspector's content (sections + notes) from
    /// the panel top, so `draw` can size the panel's scroll model.
    inspector_content_h: f64,
    slide_dest: Rect,
    scale: f64,
    /// Thumbnail slot rects `(index, rect)` for this frame, read back from the
    /// shared [`PageSidebar`] so hit-testing / auto-scroll / the `--shots` demo
    /// agree with what it paints.
    thumbs: Vec<(usize, Rect)>,
    buttons: Vec<UiButton>,
    labels: Vec<Label>,
    /// The scrolling viewport for the thumbnail list (between the "SLIDES"
    /// header and the pinned Add/Dup/Del row) — the rect handed to the shared
    /// rail's `begin_frame`.
    rail_viewport: Rect,
    notes_box: Option<Rect>,
    /// Where the font-size [`NumberInput`] sits (only when a text element is
    /// selected). Positioned + drawn by the widget itself, not as a `UiButton`.
    font_input_rect: Option<Rect>,
    /// Where the fill [`ColorPicker`] popover sits (only while it's open).
    fill_picker_rect: Option<Rect>,
    /// Responsive insert-tool layout, including the open overflow popover.
    toolbar_tools: ToolbarToolsLayout,
    /// How the rail resolves at the current width (docked beside content, or
    /// an overlay flap over it).
    layout: SplitLayout,
    /// How the inspector resolves at the current width.
    inspector_layout: SplitLayout,
    /// Dimming scrim behind an open overlay flap (empty when docked/closed).
    scrim: Rect,
    /// Eased reveal `0..=1` of the rail, for the scrim opacity.
    reveal: f64,
    /// Eased reveal `0..=1` of the inspector, for the scrim opacity.
    inspector_reveal: f64,
}

/// Rail + inspector column geometry resolved from the current width and
/// flap reveal, plus the scrim that dims the body while either is an open
/// overlay flap. The intermediate result [`SlidesApp::compute_panels`]
/// builds and [`SlidesApp::compute_chrome`] assembles into a full [`Chrome`].
struct PanelGeometry {
    rail: Rect,
    inspector: Rect,
    canvas_area: Rect,
    layout: SplitLayout,
    reveal: f64,
    insp_layout: SplitLayout,
    insp_reveal: f64,
    scrim: Rect,
    body_top: f64,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across several unrelated concerns (document lifecycle, editor mode/selection state, the presentation flow, and --shots-only demo staging); grouping them would need a real design pass (e.g. per-concern sub-structs), left for a follow-up rather than forced here"
)]
pub struct SlidesApp {
    /// The deck as last presented, with any gesture in flight previewed on
    /// it (a drag moves its element here until the drop dispatches).
    deck: Deck,
    /// The slides' order keys, in deck order, as last presented.
    ranks: Vec<i64>,
    /// The slide to show once the deck holds it: one this editor just added
    /// or moved.
    focus_slide: Option<Id>,
    /// A deck presented while a gesture was in flight, shown once it ends.
    deferred: Option<std::rc::Rc<crate::actions::SlidesResourceCtx>>,
    rect: Rect,
    node: Option<NodeId>,
    bootstrapped: bool,

    mode: Mode,
    cur_slide: usize,
    sel: Option<Id>,
    editing_text: bool,
    editing_notes: bool,
    /// Inline deck-title rename in progress (toolbar title clicked). Typing
    /// splices into the CRDT title; Enter/Esc/clicking away commits.
    editing_title: bool,
    /// Transient state and input policy for a running talk.
    presentation: Presentation,
    /// Local routing policy selected from the presentation resource actions.
    /// Never sync this through the deck: collaborators do not share hardware.
    presentation_target: ui::fullscreen::FullscreenTarget,

    /// Modal keyboard-shortcuts overlay. Owns its visibility, drawing, and
    /// pointer/keyboard capture policy in every editor and presentation mode.
    help_overlay: HelpOverlay,
    /// Snap-to-guides toggle (G in edit mode would clash with overview, so
    /// snapping is always on, but the active guide lines computed during the
    /// current drag live here so `draw` can paint them).
    active_guides: Vec<Guide>,
    last_mouse: Point,
    drag: Drag,
    /// The slide-thumbnail rail — the shared [`PageSidebar`] widget. It owns the
    /// list geometry, the scroll offset, drag-to-reorder (live reflow gap +
    /// floating ghost), the index numbers, its scrollbar and its own right-click
    /// menu. Slides keeps drawing the panel background, the "SLIDES" header and
    /// the pinned Add/Dup/Del action row around it (`with_panel(false)`). Pointer
    /// input is fed to it from the rail branches of the event handlers and the
    /// events it emits are applied to the deck in [`SlidesApp::apply_rail_event`].
    page_sidebar: PageSidebar,
    /// Last left-press on a canvas element: `(id, when, where)`. A second press
    /// on the same element within the double-click window enters text edit; a
    /// single press just selects and arms a move, so a selected element drags on
    /// the very first grab.
    last_click: Option<(Id, ui::time::Instant, Point)>,
    /// A press inside the text box being edited is held: its drag selects
    /// text in the box, not the element.
    text_drag: bool,
    /// The words of the text box being edited, in the SDK editor; opened when
    /// editing starts and closed when it ends.
    text_editor: Option<TextBoxEditor>,
    /// Select every word once the editor opens (a search result revealed).
    text_edit_select_all: bool,
    /// Where the text box being edited is drawn, in window coordinates.
    text_edit_box: Rect,

    /// Every edit of the deck goes through the resource's dispatcher as a
    /// [`SlidesAction`](crate::actions::SlidesAction).
    dispatch: Dispatcher<SlidesDeck>,

    /// Responsive slide-rail flap. On a wide window the rail is docked beside
    /// the canvas; on a narrow window it collapses to an overlay flap behind a
    /// scrim, closed by default and summoned by the toolbar hamburger. This is
    /// the shared toolkit easing kernel ([`FlapState`]).
    rail_flap: FlapState,
    /// Responsive inspector flap. The inspector docks on the right on a wide
    /// window; below a (narrower) breakpoint it collapses to a right-edge
    /// overlay flap so the canvas reclaims the full width. Same easing kernel
    /// as the rail.
    inspector_flap: FlapState,
    /// Width below which the inspector collapses to an overlay flap. Lower than
    /// the rail's breakpoint so a mid-width window keeps the inspector docked
    /// (rail flap + docked inspector) and only the narrowest widths collapse
    /// both panels.
    inspector_breakpoint: f64,
    /// Last instant the flap animation advanced, for frame-rate-independent
    /// easing across `draw` calls.
    last_flap_tick: ui::time::Instant,
    /// Cleared after the first layout is resolved; until then the flap's eased
    /// reveal is snapped to the resolved default so the very first frame at a
    /// narrow width shows the (closed) flap without a one-frame flash.
    flap_uninitialised: bool,
    /// Last applied theme text scale. Chrome construction is also used for
    /// pointer hit testing and accessibility bounds, so large-text mode is a
    /// layout state rather than a draw-only override.
    text_scale: f32,
    /// Width breakpoint between the docked and overlay rail layouts. A field
    /// (not a constant) so `--shots` can force a narrow layout deterministically.
    breakpoint: f64,

    /// Monotonic revision of the loaded deck, bumped whenever the CRDT mirror
    /// is reloaded. The presentation cache includes it in its key, so a remote
    /// edit (or undo) to the current slide invalidates the cached render.
    deck_rev: u64,

    /// Where image elements' bytes live, addressed by the hash in their `src`.
    /// Created once with the app and shared with its image and document workers.
    content: ContentStore,
    /// Path-keyed decode cache for image elements. Decodes off the UI thread;
    /// the slide shows a light loading state until the pixels land.
    images: ImageStore,
    /// Vertical scroll offset (px) of the edit-mode thumbnail rail. Clamped to
    /// the overflow at every use, so all thumbnails stay reachable however many
    /// slides the deck has.
    rail_scroll: f64,
    /// Vertical scroll offset (px) of the slide-sorter overview grid.
    overview: OverviewGrid,
    /// Responsive insert toolbar. Owns overflow state, layout, drawing, and
    /// pointer policy; document actions returned by it are applied here.
    toolbar_tools: ToolbarTools,
    /// Measured chrome text metrics `compute_chrome` lays out from (see
    /// [`ChromeMetrics`]); refreshed at the top of every draw.
    chrome_metrics: ChromeMetrics,
    /// Scroll model + overlay bar for the inspector column. The panel's
    /// sections are laid out imperatively (buttons + labels), so the shared
    /// [`Scrollable`] is driven by hand: `compute_chrome` shifts every
    /// inspector item by its offset, `draw` syncs its viewport / content size
    /// and paints its bar, and the wheel over the panel scrolls it. Without
    /// this a 2× inspector in a short window simply ran past the viewport.
    inspector_scroll: Scrollable,
    /// Tooltip for the control under the pointer, refreshed on every pointer
    /// move. Cached because [`Widget::tooltip`] hands out a borrow and the
    /// text is a fluent lookup, not a `'static` literal.
    tooltip_text: Option<String>,
    /// An in-flight host image-open request. `Some(rx)` means the XDG portal's
    /// file chooser is open on a worker thread; the chosen path arrives over
    /// this channel and is drained each frame in [`SlidesApp::drain_portal`].
    /// `None` when idle. Per-peer/transient (not CRDT).
    portal_rx: Option<PortalChannel>,
    /// An in-flight host file open/save for `.odp` documents. Same
    /// worker-thread/channel contract as [`Self::portal_rx`]; `save` carries
    /// the deck bytes to write once the user names a destination.
    doc_portal_rx: Option<DocPortalChannel>,
    /// Bounded parser/serializer worker. A missing worker is an explicit
    /// visible failure rather than a synchronous fallback on the UI thread.
    document_worker: Option<DocumentWorker>,
    document_worker_error: Option<String>,
    next_document_generation: u64,
    active_document_generation: Option<u64>,
    dispatched_save: Option<DispatchedSave>,
    resource_file: Option<(bool, std::path::PathBuf)>,
    resource_invocation: Option<tilo_actions::InvocationId>,
    completed_resource_invocation: Option<tilo_actions::InvocationUpdate>,
    /// Visible result of a fallible open/save/image operation. Previously those
    /// paths failed silently, leaving users unsure whether a click worked.
    io_toast: Option<(String, ToastKind)>,

    /// Right-click context menu (element / slide actions) — the shared
    /// [`ui::widget::ContextMenu`] primitive, driven standalone. `ctx_hits`
    /// maps each menu row index (separators included, as `None`) to the action
    /// it performs when chosen.
    ctx_menu: ContextMenu,
    ctx_hits: Vec<Option<Hit>>,
    /// Font-size field for the selected text element — the shared
    /// [`ui::widget::NumberInput`] (click-to-type + steppers), positioned into
    /// the inspector's TYPE row and driven directly.
    font_input: NumberInput,
    /// Embeddable fill colour picker ([`ui::widget::ColorPicker`]), shown as a
    /// popover over the inspector when the user picks a custom fill.
    fill_picker: ColorPicker,
    fill_picker_open: bool,
    /// Inline deck-title editor — the shared single-line [`ui::widget::TextInput`]
    /// (full selection: double-click word, Shift+Arrow, drag, Ctrl+A/C/X/V). Live
    /// while [`Self::editing_title`]; its edits diff-splice into the CRDT title.
    title_input: TextInput,
    /// The current slide's speaker notes, edited as the deck's child text
    /// (each edit a [`SlidesAction::EditNotes`](crate::actions::SlidesAction::EditNotes)).
    /// The shared editor owns multiline layout, selection, clipboard,
    /// scrolling and IME; `editing_notes` only owns the inspector's open
    /// state.
    notes: TextControl<TextInput>,
    /// The slide whose notes [`Self::notes`] edits.
    notes_slide: Rc<RefCell<String>>,
    /// Put the notes caret at their end when the editor next shows them.
    notes_caret_to_end: bool,
    /// Set when the popover opens so the next `draw` (which has the theme, hence
    /// can resolve a colour *role* to a concrete colour) seeds the picker with
    /// the selected element's current fill.
    fill_picker_needs_init: bool,
    /// `--shots`-only: open a context menu on the first drawn frame (once the
    /// layout rect is known) so a headless capture can show it. Never set on a
    /// live launch.
    demo_menu: Option<DemoMenu>,
    /// `--shots`-only: stage a live thumbnail reorder drag on the first drawn
    /// frame (once the rail geometry is known) so a headless capture shows the
    /// floating ghost + reflow gap. Never set on a live launch.
    demo_reorder: bool,
    /// `--shots`-only: stage a thumbnail drag dropped over the red dashed
    /// delete zone on the first drawn frame, so a headless capture shows the
    /// armed "Drop to delete" target. Never set on a live launch.
    demo_delete_zone: bool,
}

/// Which context menu a `--shots` scenario should pop open for its capture.
#[derive(Clone, Copy)]
enum DemoMenu {
    Element,
    Slide,
}

impl SlidesApp {
    /// The editor of the deck the view `cx` shows: its edits go through the
    /// view's dispatcher, its pictures are in the view's content store, and
    /// presentation files are read and written only in a window someone
    /// uses.
    pub fn new(cx: &ViewCx<SlidesDeck>) -> Self {
        let content = cx.content().unwrap_or_else(crate::assets::app_content);
        let state = cx.live().get();
        let mut app = Self::build(cx.dispatcher(), content, cx.mode().is_live());
        app.adopt_resource_state(&state);
        app
    }

    fn build(dispatch: Dispatcher<SlidesDeck>, content: ContentStore, live: bool) -> Self {
        let deck = starter_deck();
        let (document_worker, document_worker_error) = if live {
            match DocumentWorker::new(content.clone()) {
                Ok(worker) => (Some(worker), None),
                Err(error) => (None, Some(error)),
            }
        } else {
            (
                None,
                Some("presentation files open only in a live window".into()),
            )
        };
        let notes_slide = Rc::new(RefCell::new(String::new()));
        let located = Rc::clone(&notes_slide);
        let notes = TextControl::view_only(
            TextInput::local()
                .multiline(true)
                .with_plain_chrome()
                .with_font_size(12.5)
                .with_id("slides-speaker-notes"),
            move |doc| SlidesDoc::notes_text(doc, &located.borrow()),
        );
        SlidesApp {
            ranks: Vec::new(),
            focus_slide: None,
            deferred: None,
            deck,
            rect: Rect::ZERO,
            node: None,
            bootstrapped: false,
            mode: Mode::Edit,
            cur_slide: 1,
            sel: None,
            editing_text: false,
            editing_notes: false,
            editing_title: false,
            presentation: Presentation::default(),
            presentation_target: ui::fullscreen::FullscreenTarget::Largest,
            help_overlay: HelpOverlay::default(),
            active_guides: Vec::new(),
            last_mouse: Point::ZERO,
            drag: Drag::None,
            page_sidebar: PageSidebar::new(SLIDE_H / SLIDE_W).with_panel(false),
            last_click: None,
            text_drag: false,
            text_editor: None,
            text_edit_select_all: false,
            text_edit_box: Rect::ZERO,
            dispatch,
            rail_flap: FlapState::open_when_docked(),
            inspector_flap: FlapState::open_when_docked(),
            inspector_breakpoint: INSPECTOR_BREAKPOINT,
            last_flap_tick: ui::time::now(),
            flap_uninitialised: true,
            text_scale: 1.0,
            breakpoint: NARROW_BREAKPOINT,
            deck_rev: 0,
            images: ImageStore::new(content.clone()),
            content,
            rail_scroll: 0.0,
            overview: OverviewGrid::new(),
            toolbar_tools: ToolbarTools::default(),
            chrome_metrics: ChromeMetrics::fallback(),
            inspector_scroll: Scrollable::new_vertical().always_visible(),
            tooltip_text: None,
            portal_rx: None,
            doc_portal_rx: None,
            document_worker,
            document_worker_error,
            next_document_generation: 0,
            active_document_generation: None,
            dispatched_save: None,
            resource_file: None,
            resource_invocation: None,
            completed_resource_invocation: None,
            io_toast: None,
            ctx_menu: ContextMenu::new(),
            ctx_hits: Vec::new(),
            font_input: NumberInput::new(40.0)
                .with_range(8.0, 160.0)
                .with_step(2.0)
                .with_suffix("pt")
                .with_id("slides-font-size"),
            fill_picker: ColorPicker::new(vello::peniko::Color::from_rgba8(90, 96, 240, 255)),
            fill_picker_open: false,
            fill_picker_needs_init: false,
            title_input: TextInput::local()
                .with_kind(ui::widget::TextInputKind::Name)
                .with_plain_chrome()
                .with_font_size(17.0)
                .with_id("slides-title"),
            notes,
            notes_slide,
            notes_caret_to_end: false,
            demo_menu: None,
            demo_reorder: false,
            demo_delete_zone: false,
        }
    }

    /// Stage the view state of a screenshot scenario over the deck its
    /// actions opened ([`crate::resource::SlidesShell::scenario`]): a mode, a
    /// selection, an open menu, an edit or a drag in flight. Flaps take
    /// their staged state at once rather than sliding into it.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        let Some(shot) = scenario else {
            return;
        };
        self.flap_uninitialised = true;
        self.stage_layout(shot);
        self.stage_content(shot);
    }

    /// The scenarios that pick a mode or a panel.
    fn stage_layout(&mut self, shot: &str) {
        match shot {
            "overview" => self.mode = Mode::Overview,
            "present" => {
                self.mode = Mode::Present;
                self.presentation.start();
            }
            "present_notes" => {
                self.mode = Mode::Present;
                self.presentation.start();
                self.presentation.set_show_notes(true);
            }
            // Full presenter dashboard (current + next + notes + timer +
            // position) as it appears on the presenter's own monitor.
            "presenter" => {
                self.mode = Mode::Present;
                self.presentation.start();
                self.presentation.set_presenter_view(true);
            }
            // Present-mode white-out (`W`) — the projector becomes a
            // bright "whiteboard"/attention surface.
            "present_white" => {
                self.mode = Mode::Present;
                self.presentation.start();
                self.presentation.set_blank(Blank::White);
            }
            "help" => self.help_overlay.show(),
            // The long deck rests on its last slide with the rail scrolled to
            // its end: the rail scrolls, and the pinned Add/Dup/Del row is
            // never overlapped by thumbnails.
            "many" => self.rest_on_last_slide(),
            // The same, with the rail summoned open at a narrow width.
            "many_rail" => {
                self.rest_on_last_slide();
                self.rail_flap.open();
            }
            // Overview scrolled to its end, proving the last slide is reachable.
            "overview_end" => {
                self.mode = Mode::Overview;
                self.overview.set_scroll(1.0e6);
            }
            "narrow" => self.rail_flap.open(),
            "narrow_closed" => self.rail_flap.close(),
            "inspector" => self.inspector_flap.open(),
            "empty" => {
                self.cur_slide = 0;
                self.sel = None;
                self.bootstrapped = true;
            }
            _ => {}
        }
    }

    /// The scenarios that select something or show an edit, a menu or a
    /// drag in progress.
    fn stage_content(&mut self, shot: &str) {
        match shot {
            // The inline deck-title rename in progress, with a live
            // selection in the title field.
            "rename" => {
                self.cur_slide = 1;
                self.begin_title_edit();
                self.title_input.set_selection(0, 9);
                self.bootstrapped = true;
            }
            // The speaker-notes editor focused, its caret shown.
            "notes_edit" => {
                self.cur_slide = 1;
                self.sel = None;
                self.editing_notes = true;
                self.bootstrapped = true;
            }
            // The nothing-selected inspector: layout, background and every
            // deck theme pill readable at the default width.
            "slide_props" => {
                self.cur_slide = 1;
                self.sel = None;
                self.bootstrapped = true;
            }
            // The toolbar overflow ("More") menu, opened: every insert tool
            // stays reachable even when the bar can't show them all.
            "toolbar_menu" => {
                self.toolbar_tools.set_open(true);
                self.sel = None;
                self.bootstrapped = true;
            }
            // The picture on the last slide, decoded.
            "image" => {
                self.rest_on_picture();
                for src in self.picture_sources() {
                    self.images.load_blocking(&src);
                }
            }
            // The first frames after a picture is placed, while its bytes
            // are still on their way.
            "image_loading" => {
                self.rest_on_picture();
                for src in self.picture_sources() {
                    self.images.hold_loading(&src);
                }
            }
            // The failure toast a presentation the decoder rejects shows.
            "open_error" => {
                self.io_toast = Some((tr("slides-toast-open-failed"), ToastKind::Error));
            }
            // Context menus open once the layout is known (see `draw`).
            "element_menu" => {
                self.cur_slide = 1;
                self.sel = self
                    .deck
                    .slides
                    .get(1)
                    .and_then(|s| s.elements.first().map(|e| e.id.clone()));
                self.demo_menu = Some(DemoMenu::Element);
                self.bootstrapped = true;
            }
            "slide_menu" => {
                self.cur_slide = 1;
                self.sel = None;
                self.demo_menu = Some(DemoMenu::Slide);
                self.bootstrapped = true;
            }
            // A thumbnail reorder drag mid-flight: the grabbed slide lifted
            // out as a floating ghost, the list reflowed to open the gap.
            "reorder" => {
                self.cur_slide = 0;
                self.sel = None;
                self.demo_reorder = true;
                self.bootstrapped = true;
            }
            // An on-canvas text box in edit mode with its first word
            // selected.
            "edit_text" => {
                self.select_first_text_on_second_slide();
                self.begin_text_edit(true);
                self.bootstrapped = true;
            }
            // A thumbnail dragged onto the armed delete zone.
            "delete_zone" => {
                self.cur_slide = 0;
                self.sel = None;
                self.demo_delete_zone = true;
                self.bootstrapped = true;
            }
            // The font-size field (text selected) and the fill colour
            // picker popover open.
            "inspector_widgets" => {
                self.select_first_text_on_second_slide();
                self.inspector_flap.open();
                self.fill_picker_open = true;
                self.fill_picker_needs_init = true;
                self.bootstrapped = true;
            }
            _ => {}
        }
    }

    fn select_first_text_on_second_slide(&mut self) {
        self.cur_slide = 1;
        self.sel = self.deck.slides.get(1).and_then(|s| {
            s.elements
                .iter()
                .find(|e| e.kind == ElementKind::Text)
                .map(|e| e.id.clone())
        });
    }

    /// Show the last slide with the rail scrolled to its end (clamped to
    /// the overflow at layout).
    fn rest_on_last_slide(&mut self) {
        self.cur_slide = self.deck.slides.len().saturating_sub(1);
        self.rail_scroll = 1.0e6;
        self.sel = None;
        self.bootstrapped = true;
    }

    fn rest_on_picture(&mut self) {
        self.cur_slide = self.deck.slides.len().saturating_sub(1);
        self.sel = None;
        self.bootstrapped = true;
    }

    /// The pictures on the current slide.
    fn picture_sources(&self) -> Vec<String> {
        self.slide()
            .map(|slide| {
                slide
                    .elements
                    .iter()
                    .filter(|element| element.kind == ElementKind::Image)
                    .map(|element| element.src.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The rail layout resolved from the current allocated width. Overview and
    /// present modes are full-window and never show the rail, but the editor
    /// (`Mode::Edit`) consults this every frame.
    fn rail_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), self.breakpoint)
    }

    /// Whether the rail is currently revealed (resolving the user's toggle
    /// against the layout default: docked ⇒ open, overlay ⇒ closed).
    fn rail_open(&self) -> bool {
        self.rail_flap.is_open(self.rail_layout())
    }

    /// The inspector layout resolved from the current allocated width.
    fn inspector_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), self.inspector_breakpoint)
    }

    /// Whether the inspector is currently revealed.
    fn inspector_open(&self) -> bool {
        self.inspector_flap.is_open(self.inspector_layout())
    }

    /// Reconcile both the rail and inspector flaps with the current width and
    /// advance their eased reveals. On the very first resolved layout each flap
    /// is snapped to its resolved default so a narrow first frame shows the
    /// (closed) flaps with no one-frame flash. Returns the rail layout. Called
    /// once per frame before `compute_chrome`.
    fn sync_flaps(&mut self, dt: f64, reduce_motion: bool) -> SplitLayout {
        let rail = self.rail_layout();
        let insp = self.inspector_layout();
        self.rail_flap.reconcile(rail);
        self.inspector_flap.reconcile(insp);
        if self.flap_uninitialised {
            self.rail_flap.snap(self.rail_flap.is_open(rail));
            self.inspector_flap.snap(self.inspector_flap.is_open(insp));
            // `snap` records a user preference; clear it so width changes still
            // fall back to the layout default. Re-resolving via reconcile after
            // a fresh open_when_docked would re-snap, so instead just mark done.
            self.flap_uninitialised = false;
        } else if reduce_motion {
            self.rail_flap.snap(self.rail_flap.is_open(rail));
            self.inspector_flap.snap(self.inspector_flap.is_open(insp));
        } else {
            self.rail_flap.tick(rail, dt);
            self.inspector_flap.tick(insp, dt);
        }
        rail
    }

    // --- the presented deck -------------------------------------------------

    /// Show the deck the resource holds now: an edit of this participant's,
    /// a collaborator's, an undo or a restore. While a gesture is in flight
    /// its preview stays on screen, and the deck is shown once it ends.
    pub(crate) fn adopt_resource_state(&mut self, state: &Rc<crate::actions::SlidesResourceCtx>) {
        if self.gesture_in_flight() {
            self.deferred = Some(Rc::clone(state));
            return;
        }
        self.deferred = None;
        self.ranks.clone_from(&state.ranks);
        let deck = state.deck();
        if deck != self.deck {
            if !actions::deck_within_aggregate_limits(&deck) {
                self.io_toast = Some((tr("slides-toast-shared-limit"), ToastKind::Error));
                return;
            }
            // The slide shown stays shown wherever the change moved it; a
            // slide this editor just added or moved is shown once it lands.
            let shown = self
                .focus_slide
                .clone()
                .or_else(|| self.slide().map(|slide| slide.id.clone()));
            self.deck = deck;
            if let Some(index) = shown.and_then(|id| self.deck.index_of(&id)) {
                self.cur_slide = index;
                self.focus_slide = None;
            }
            self.bump_deck_revision();
        }
        if self.deck.slides.is_empty() {
            self.cur_slide = 0;
            self.sel = None;
            return;
        }
        if self.cur_slide >= self.deck.slides.len() {
            self.cur_slide = self.deck.slides.len() - 1;
        }
        // Notes whose slide went away are no longer being edited: the next
        // keystroke must not land in another slide's notes.
        if self.editing_notes {
            let slide = self.notes_slide.borrow().clone();
            if !slide.is_empty() && self.deck.index_of(&slide).is_none() {
                self.end_notes_edit();
            }
        }
        // Drop the selection when its element went away (a collaborator
        // deleted it, an undo).
        if let Some(id) = self.sel.clone() {
            let alive = self
                .deck
                .slides
                .get(self.cur_slide)
                .is_some_and(|s| s.element(&id).is_some());
            if !alive {
                self.sel = None;
                self.editing_text = false;
            }
        }
    }

    /// Whether a drag the deck previews is under way.
    fn gesture_in_flight(&self) -> bool {
        self.drag.active() || self.page_sidebar.is_dragging()
    }

    /// Show a deck presented during a gesture, once the gesture is over.
    fn adopt_deferred(&mut self) {
        if !self.gesture_in_flight()
            && let Some(state) = self.deferred.take()
        {
            self.adopt_resource_state(&state);
        }
    }

    fn slide(&self) -> Option<&Slide> {
        self.deck.slides.get(self.cur_slide)
    }

    fn selected_element(&self) -> Option<&Element> {
        let id = self.sel.as_ref()?;
        self.slide()?.element(id)
    }

    fn palette(&self, theme: &Theme) -> Palette {
        Palette::resolve(self.deck.palette, theme)
    }

    // --- action dispatch ----------------------------------------------------

    /// Submit one edit of the deck through the resource's dispatcher, as
    /// every control, the palette and an agent do: it is checked against
    /// the participant's permission, applied, recorded as one undo step and
    /// shared. The deck shows it when it is presented back.
    fn submit(&self, action: &crate::actions::SlidesAction) {
        self.dispatch.apply(action);
    }

    /// The edits bound to the selected element, published on the deck
    /// beside its declared actions: delete it, bring it to the front, send
    /// it to the back.
    pub(crate) fn selection_declarations(&self) -> Vec<ActionDecl<SlidesDeck>> {
        if self.mode != Mode::Edit {
            return Vec::new();
        }
        let (Some(slide), Some(element)) = (self.slide(), self.selected_element()) else {
            return Vec::new();
        };
        let (index, slide_id, element_id) = (self.cur_slide, slide.id.clone(), element.id.clone());
        let label = crate::actions::element_label(element.kind, &element.text);
        let mut declarations = vec![{
            let element_id = element_id.clone();
            ActionDecl::new(
                "delete-selection",
                format!("Delete {label}"),
                Vec::new(),
                move |_, _| Ok(actions::delete_element_action(index, element_id.clone())),
            )
            .describe("Delete the element selected in the editor.")
            .destructive()
        }];
        for (id, front, verb) in [
            ("bring-selection-front", true, "Bring to front"),
            ("send-selection-back", false, "Send to back"),
        ] {
            let Some(z) = actions::restack_z_for_author(&self.deck, &slide_id, &element_id, front)
            else {
                continue;
            };
            let (slide_id, element_id) = (slide_id.clone(), element_id.clone());
            declarations.push(
                ActionDecl::new(id, format!("{verb}: {label}"), Vec::new(), move |_, _| {
                    Ok(actions::set_element_z_action(
                        slide_id.clone(),
                        element_id.clone(),
                        z,
                    ))
                })
                .describe("Restack the element selected in the editor."),
            );
        }
        declarations
    }

    // --- rail scroll geometry ----------------------------------------------

    /// The inspector column's painted rect this frame (the same geometry
    /// `compute_chrome` uses), for pointer tests that need only the panel.
    fn inspector_rect(&self) -> Rect {
        let r = self.rect;
        let toolbar = Rect::new(r.x0, r.y0, r.x1, r.y0 + self.chrome_metrics.toolbar_h());
        self.compute_panels(r, toolbar).inspector
    }

    /// The rail's painted rect, resolved from the current width + flap reveal.
    /// Mirrors the computation in [`Self::compute_chrome`] so the scroll
    /// helpers and the chrome agree.
    fn rail_rect(&self) -> Rect {
        let r = self.rect;
        let body_top = r.y0 + self.chrome_metrics.toolbar_h();
        match self.rail_layout() {
            SplitLayout::Docked => {
                let inset = RAIL_W * self.rail_flap.reveal();
                Rect::new(r.x0, body_top, r.x0 + inset, r.y1)
            }
            SplitLayout::Overlay => {
                let slide = self.rail_flap.slide();
                let x0 = r.x0 - RAIL_W * (1.0 - slide);
                Rect::new(x0, body_top, x0 + RAIL_W, r.y1)
            }
        }
    }

    /// The scrolling viewport for the thumbnail list — the region between the
    /// fixed "SLIDES" header and the pinned Add/Dup/Del action row. This is the
    /// rect handed to the shared [`PageSidebar`]'s `begin_frame`; the widget
    /// owns the thumbnail geometry + scrollbar inside it.
    #[allow(
        clippy::unused_self,
        reason = "keeps the ergonomic self.rail_viewport(...) call site consistent with the \
                  other rail geometry helpers; converting to an associated function would read \
                  worse at every call site for no benefit"
    )]
    fn rail_viewport(&self, rail: Rect) -> Rect {
        let m = &self.chrome_metrics;
        // Header row (caption / Close) above, pinned action row below, each
        // separated from the list by `s2`.
        let list_top = rail.y0 + m.s2 + m.rail_header_h() + m.s2;
        let list_bottom = (rail.y1 - m.s4 - m.control_h() - m.s2).max(list_top);
        Rect::new(rail.x0, list_top, rail.x1, list_bottom)
    }

    /// The red dashed "drop to delete" zone, pinned to the bottom of the rail
    /// over the (drag-irrelevant) action row. Only shown — and only live — while
    /// a slide thumbnail is being dragged. Dropping a slide here removes it
    /// instead of reordering it, matching the app-wide drag-to-delete gesture.
    #[allow(
        clippy::unused_self,
        reason = "keeps the ergonomic self.slide_delete_zone(...) call site consistent with the \
                  other rail geometry helpers; converting to an associated function would read \
                  worse at every call site for no benefit"
    )]
    fn slide_delete_zone(&self, rail: Rect) -> Rect {
        // Covers the pinned action row it replaces (same `s4` bottom pad and
        // control height), inset `s2` from the rail edges.
        let m = &self.chrome_metrics;
        Rect::new(
            rail.x0 + m.s2,
            rail.y1 - m.s4 - m.control_h() - m.s2,
            rail.x1 - m.s2,
            rail.y1 - m.s2,
        )
    }

    /// Whether a slide drag is currently hovering the delete zone (for the
    /// zone's armed highlight).
    fn over_delete_zone(&self) -> bool {
        self.page_sidebar.is_dragging()
            && self
                .slide_delete_zone(self.rail_rect())
                .contains(self.last_mouse)
    }

    /// Sync the shared page rail to this frame's geometry + scroll before it is
    /// drawn or hit-tested. Feeds it the live viewport, the slide count and the
    /// current slide (which clamps the scroll and re-pins any held drag gap),
    /// then mirrors the clamped scroll back into `rail_scroll` so the widget
    /// stays the single source of truth for the list geometry.
    fn sync_page_rail(&mut self) {
        let viewport = self.rail_viewport(self.rail_rect());
        self.page_sidebar.set_aspect(SLIDE_H / SLIDE_W);
        self.page_sidebar.set_scroll(self.rail_scroll);
        self.page_sidebar
            .begin_frame(viewport, self.deck.slides.len(), self.cur_slide);
        self.rail_scroll = self.page_sidebar.scroll();
    }

    /// Adjust the rail scroll so the current slide's thumbnail is fully in view
    /// (called after keyboard navigation / selection changes). Delegates to the
    /// shared rail's `scroll_to`, then mirrors the offset back.
    fn scroll_rail_to_current(&mut self) {
        self.sync_page_rail();
        self.page_sidebar.scroll_to(self.cur_slide);
        self.rail_scroll = self.page_sidebar.scroll();
    }

    /// Run the shared rail's frame sync and return its thumbnail slot rects —
    /// the entry point tests + the `--shots` demo use to source click
    /// coordinates, mirroring how `compute_chrome` reads them back.
    fn rail_thumb_rects(&mut self) -> Vec<(usize, Rect)> {
        self.sync_page_rail();
        self.page_sidebar.thumb_rects()
    }

    /// Apply one event emitted by the shared page rail to the deck (and CRDT).
    /// The single mapping used by both the live event path and the tests.
    fn apply_rail_event(&mut self, ev: PageSidebarEvent) {
        match ev {
            PageSidebarEvent::Select(i) => self.apply_hit(Hit::SelectSlide(i)),
            PageSidebarEvent::Reorder { from, to } => {
                let Some(id) = self.deck.slides.get(from).map(|slide| slide.id.clone()) else {
                    return;
                };
                // A reorder is authored as one absolute rank, not as a
                // receiver-relative destination index. This keeps the same
                // encoded action deterministic if remote inserts land before
                // it is replayed elsewhere.
                let Some(rank) = crate::doc::rank_for_move(&self.ranks, from, to) else {
                    return;
                };
                self.submit(&actions::set_slide_order_action(id.clone(), rank));
                self.focus_slide = Some(id);
                self.cur_slide = to;
                self.scroll_rail_to_current();
            }
            PageSidebarEvent::Duplicate(i) => {
                self.cur_slide = i.min(self.deck.slides.len().saturating_sub(1));
                self.duplicate_slide();
            }
            PageSidebarEvent::Delete(i) => {
                self.cur_slide = i.min(self.deck.slides.len().saturating_sub(1));
                self.delete_slide();
            }
            PageSidebarEvent::Add => self.new_slide(),
        }
    }

    /// Drain and apply every event the shared page rail produced this gesture.
    fn apply_rail_events(&mut self) {
        for ev in self.page_sidebar.take_events() {
            self.apply_rail_event(ev);
        }
    }

    // --- geometry -----------------------------------------------------------

    /// Every `(text, font, weight)` the chrome reserves a slot for, so
    /// [`ChromeMetrics::resolve`] can measure each with the style it is
    /// painted in. Titles are elided at paint time and need no entry.
    fn chrome_label_set(&self) -> Vec<(String, f32, TextWeight)> {
        let control = |s: &str| (s.to_owned(), FONT_CONTROL, TextWeight::Semibold);
        let mut set: Vec<(String, f32, TextWeight)> = vec![
            control(&tr("slides-present")),
            control(&tr("slides-close")),
            control(&tr("slides-add")),
            control(&tr("slides-dup")),
            control(&tr("slides-del")),
            control(&tr("slides-bring-front")),
            control(&tr("slides-send-back")),
            control(&tr("slides-duplicate")),
            control(&tr("slides-delete")),
            control(&tr("slides-custom-colour")),
            control("B"),
            control("I"),
            // The TYPE row reserves its NumberInput for a three-digit value.
            control("999 pt"),
        ];
        set.extend(
            crate::toolbar_tools::bar_labels()
                .iter()
                .map(|s| control(s)),
        );
        set.extend(
            crate::toolbar_tools::menu_labels()
                .iter()
                .map(|s| control(s)),
        );
        set.extend(TextAlign::ALL.iter().map(|a| control(a.glyph())));
        set.extend(Layout::ALL.iter().map(|l| control(l.label())));
        set.extend(Background::ALL.iter().map(|b| control(b.label())));
        set.extend(
            PaletteKind::ALL
                .iter()
                .map(|p| (p.label().to_owned(), FONT_PILL, TextWeight::Semibold)),
        );
        if let Some(el) = self.selected_element()
            && el.fill.starts_with('#')
        {
            set.push(control(&custom_colour_hex(&el.fill)));
        }
        set
    }

    /// Re-resolve the chrome metrics against the renderer about to paint, so
    /// this frame's chrome — and the hit-rects later events derive from the
    /// same `compute_chrome` — is laid out from measured text.
    fn refresh_chrome_metrics(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let set = self.chrome_label_set();
        // Nothing is gated on a change here — `compute_chrome` re-derives the
        // chrome from these on every use — so the fresh set simply replaces
        // the old one.
        self.chrome_metrics = ChromeMetrics::resolve(
            tr,
            theme,
            set.iter()
                .map(|(text, font, weight)| (text.as_str(), *font, *weight)),
        );
    }

    fn compute_chrome(&self) -> Chrome {
        if self.text_scale >= 1.5 {
            return self.compute_large_text_chrome();
        }
        let r = self.rect;
        let m = &self.chrome_metrics;
        let toolbar = Rect::new(r.x0, r.y0, r.x1, r.y0 + m.toolbar_h());
        let panels = self.compute_panels(r, toolbar);
        let (slide_dest, s) = Self::fit_slide(panels.canvas_area);

        let mut buttons = Vec::new();
        let mut labels = Vec::new();

        // Toolbar -----------------------------------------------------------
        let title_x =
            Self::toolbar_hamburger(toolbar, m, panels.layout, panels.reveal, &mut buttons);
        let bh = m.pill_h();
        let by = toolbar.y0 + (toolbar.height() - bh) / 2.0;

        // Right-anchored group first (Present · palette · overview grid · the
        // inspector toggle) so we know the boundary the centre tool group and
        // title must not cross.
        let grid_rect = self.toolbar_right_group(toolbar, by, bh, &mut buttons);
        let right_edge = self.toolbar_inspector_toggle(
            panels.insp_layout,
            panels.insp_reveal,
            grid_rect,
            by,
            bh,
            &mut buttons,
        );

        // Centre tool group. Every insert tool must stay pointer-reachable at
        // any width: the ones that fit sit on the toolbar; the rest move behind
        // a "More" (⋯) overflow button that opens a dropdown menu. Nothing is
        // ever silently dropped.
        let tools_x = (title_x + 196.0).max(toolbar.x0 + 320.0_f64.min(toolbar.width() * 0.34));
        let tools_right = right_edge - m.s3;
        let tools_first = buttons.len();
        let toolbar_tools = self.toolbar_tools.layout(
            &mut buttons,
            Rect::new(tools_x, by, tools_right, by + bh),
            toolbar,
            m,
        );
        // The overflow button may sit left of the nominal tools slot when
        // nothing else fits, so the title yields to the leftmost control that
        // was actually placed, not to the slot.
        let leftmost_tool = buttons
            .get(tools_first..)
            .unwrap_or_default()
            .iter()
            .filter(|b| b.rect.center().y < toolbar.y1)
            .map(|b| b.rect.x0)
            .fold(f64::INFINITY, f64::min);

        // Title block, elided at paint time so it never runs under the tools
        // or the right group.
        let title_right = tools_x.min(right_edge - m.s3).min(leftmost_tool);
        let (title_rect, title_pos) =
            self.toolbar_title_block(toolbar, title_x, title_right, &mut labels);

        // Thumbnail rail ----------------------------------------------------
        let rail_viewport = self.build_rail_chrome(panels.rail, &mut buttons, &mut labels);
        let thumbs = self.page_sidebar.thumb_rects();

        // Inspector ---------------------------------------------------------
        let insp_first_button = buttons.len();
        let insp_first_label = labels.len();
        let insp_rects = self.build_inspector(panels.inspector, &mut buttons, &mut labels);
        let mut notes_box = insp_rects.notes;
        let mut font_input_rect = insp_rects.font_input;
        // The panel scrolls once its sections outgrow it (large text in a
        // short window). Everything it owns is shifted by the offset the
        // shared scroll model holds — clamped here against this frame's
        // content so a shrunken panel never shows a stale gap — and whatever
        // then lies outside the viewport is dropped, so the painter, the
        // pointer and the accessibility tree all see the same controls.
        let inspector = panels.inspector;
        let content_bottom = buttons
            .get(insp_first_button..)
            .unwrap_or_default()
            .iter()
            .map(|b| b.rect.y1)
            .chain(
                labels
                    .get(insp_first_label..)
                    .unwrap_or_default()
                    .iter()
                    .map(|l| l.pos.y + m.lh_caption),
            )
            .chain(notes_box.map(|nb| nb.y1))
            .fold(inspector.y0, f64::max);
        let inspector_content_h = content_bottom - inspector.y0 + INSPECTOR_PAD;
        let max_offset = (inspector_content_h - inspector.height()).max(0.0);
        let offset = f64::from(self.inspector_scroll.offset_y()).clamp(0.0, max_offset);
        if offset > 0.0 {
            let shift = vello::kurbo::Vec2::new(0.0, -offset);
            for b in buttons.get_mut(insp_first_button..).unwrap_or_default() {
                b.rect = b.rect + shift;
            }
            for l in labels.get_mut(insp_first_label..).unwrap_or_default() {
                l.pos += shift;
            }
            notes_box = notes_box.map(|nb| nb + shift);
            font_input_rect = font_input_rect.map(|fi| fi + shift);
        }
        // Cull by centre: what remains has its centre inside the viewport, so
        // `in_inspector` (a centre test) keeps classifying it as panel content
        // and nothing scrolled off the top is ever painted over the toolbar.
        let inside = |y: f64| y >= inspector.y0 && y <= inspector.y1;
        let kept: Vec<UiButton> = buttons
            .drain(insp_first_button..)
            .filter(|b| inside(b.rect.center().y))
            .collect();
        buttons.extend(kept);
        let kept: Vec<Label> = labels
            .drain(insp_first_label..)
            .filter(|l| inside(l.pos.y + m.lh_caption / 2.0))
            .collect();
        labels.extend(kept);
        // The embedded font-size field follows the same rule: once its TYPE
        // row scrolls out it is not a hit target this frame (otherwise a
        // field shifted into the toolbar band would still swallow the press
        // meant for a toolbar control above it). The notes box is last in
        // the content, so it can only ever lie below the viewport — where it
        // meets no pointer — and keeps its (shifted, clipped) rect so the
        // sections above it can still be checked against it.
        font_input_rect = font_input_rect.filter(|fi| inside(fi.center().y));
        let fill_picker_rect = self.fill_picker_popover_rect(inspector, r, panels.body_top);

        Chrome {
            toolbar,
            title_rect,
            title_pos,
            rail: panels.rail,
            inspector,
            inspector_content_h,
            slide_dest,
            scale: s,
            thumbs,
            buttons,
            labels,
            rail_viewport,
            notes_box,
            font_input_rect,
            fill_picker_rect,
            toolbar_tools,
            layout: panels.layout,
            inspector_layout: panels.insp_layout,
            scrim: panels.scrim,
            reveal: panels.reveal,
            inspector_reveal: panels.insp_reveal,
        }
    }

    /// An intrinsically spaced editor shell for 200% type. Author content is
    /// intentionally rendered at its authored size (a deck's typography is
    /// not desktop chrome); all application controls use the active type scale
    /// and the exact same rectangles feed draw, pointer routing, and a11y.
    #[allow(
        clippy::too_many_lines,
        reason = "large-text chrome is one shared geometry snapshot for drawing, pointer routing, and accessibility; keeping its complete rectangle set together prevents those paths diverging"
    )]
    fn compute_large_text_chrome(&self) -> Chrome {
        let r = self.rect;
        let toolbar = Rect::new(r.x0, r.y0, r.x1, r.y0 + 82.0);
        let inspector_open = self.inspector_open();
        let inspector = if inspector_open {
            Rect::new(r.x1 - 470.0, toolbar.y1, r.x1, r.y1)
        } else {
            Rect::ZERO
        };
        let canvas = Rect::new(r.x0, toolbar.y1, inspector.x0.max(r.x0), r.y1);
        let (slide_dest, scale) = Self::fit_slide(canvas);
        let mut buttons = vec![
            UiButton {
                rect: Rect::new(r.x1 - 300.0, r.y0 + 10.0, r.x1 - 174.0, r.y0 + 72.0),
                kind: ButtonKind::IconOnly {
                    icon: Icon::Settings2,
                    active: inspector_open,
                },
                hit: Hit::ToggleInspector,
            },
            UiButton {
                rect: Rect::new(r.x1 - 154.0, r.y0 + 10.0, r.x1 - 20.0, r.y0 + 72.0),
                kind: ButtonKind::IconText {
                    icon: Icon::Monitor,
                    label: "Present".into(),
                    accent: true,
                },
                hit: Hit::Present,
            },
        ];
        let toolbar_tools = self.toolbar_tools.layout(
            &mut buttons,
            Rect::new(r.x0 + 390.0, r.y0 + 10.0, r.x0 + 620.0, r.y0 + 72.0),
            toolbar,
            &self.chrome_metrics,
        );
        let mut labels = vec![Label {
            pos: Point::new(r.x0 + 650.0, r.y0 + 22.0),
            text: self.deck.title.clone(),
            max_w: Some(220.0),
            font: 17.0,
            color: LabelColor::Primary,
            weight: TextWeight::Bold,
        }];
        if inspector_open {
            labels.push(Label {
                pos: Point::new(inspector.x0 + 28.0, inspector.y0 + 28.0),
                text: "PROPERTIES".into(),
                max_w: None,
                font: 14.0,
                color: LabelColor::Primary,
                weight: TextWeight::Bold,
            });
            if self.selected_element().is_some() {
                buttons.extend([
                    UiButton {
                        rect: Rect::new(
                            inspector.x0 + 28.0,
                            inspector.y0 + 82.0,
                            inspector.x0 + 218.0,
                            inspector.y0 + 140.0,
                        ),
                        kind: ButtonKind::Text {
                            label: "Bold".into(),
                            active: false,
                            danger: false,
                        },
                        hit: Hit::ToggleBold,
                    },
                    UiButton {
                        rect: Rect::new(
                            inspector.x0 + 238.0,
                            inspector.y0 + 82.0,
                            inspector.x1 - 28.0,
                            inspector.y0 + 140.0,
                        ),
                        kind: ButtonKind::Text {
                            label: "Italic".into(),
                            active: false,
                            danger: false,
                        },
                        hit: Hit::ToggleItalic,
                    },
                    UiButton {
                        rect: Rect::new(
                            inspector.x0 + 28.0,
                            inspector.y0 + 162.0,
                            inspector.x1 - 28.0,
                            inspector.y0 + 220.0,
                        ),
                        kind: ButtonKind::Text {
                            label: "Delete object".into(),
                            active: false,
                            danger: true,
                        },
                        hit: Hit::DeleteElement,
                    },
                ]);
            } else {
                labels.push(Label {
                    pos: Point::new(inspector.x0 + 28.0, inspector.y0 + 86.0),
                    text: "Select an object to edit its properties.".into(),
                    max_w: Some(inspector.width() - 56.0),
                    font: 13.0,
                    color: LabelColor::Dim,
                    weight: TextWeight::Regular,
                });
            }
        }
        Chrome {
            toolbar,
            title_rect: Rect::new(r.x0 + 640.0, r.y0, r.x1 - 310.0, toolbar.y1),
            title_pos: Point::new(r.x0 + 650.0, r.y0 + 22.0),
            rail: Rect::ZERO,
            inspector,
            inspector_content_h: inspector.height(),
            slide_dest,
            scale,
            thumbs: Vec::new(),
            buttons,
            labels,
            rail_viewport: Rect::ZERO,
            notes_box: None,
            font_input_rect: None,
            fill_picker_rect: None,
            toolbar_tools,
            layout: SplitLayout::Overlay,
            inspector_layout: SplitLayout::Docked,
            scrim: Rect::ZERO,
            reveal: 0.0,
            inspector_reveal: 1.0,
        }
    }

    /// Resolve the rail + inspector column geometry from the current width
    /// and flap reveal, plus the scrim that dims the body while either is an
    /// open overlay flap. Shared by [`Self::compute_chrome`].
    fn compute_panels(&self, r: Rect, toolbar: Rect) -> PanelGeometry {
        // Resolve both responsive flaps from the allocated width. Docked ⇒ the
        // panel is a permanent column that insets the canvas; narrow ⇒ it
        // collapses to an overlay flap that slides over the canvas behind a
        // scrim, and the canvas reclaims that column's width.
        let layout = self.rail_layout();
        let reveal = self.rail_flap.reveal();
        let insp_layout = self.inspector_layout();
        let insp_reveal = self.inspector_flap.reveal();
        let body_top = toolbar.y1;

        // Rail column (left).
        let (rail, canvas_left) = match layout {
            SplitLayout::Docked => {
                // Eased inset so the rail reveals/collapses smoothly when
                // toggled even while docked.
                let inset = RAIL_W * reveal;
                (Rect::new(r.x0, body_top, r.x0 + inset, r.y1), r.x0 + inset)
            }
            SplitLayout::Overlay => {
                // Slide in from the left edge; closed ⇒ fully off-screen.
                let slide = self.rail_flap.slide();
                let x0 = r.x0 - RAIL_W * (1.0 - slide);
                (Rect::new(x0, body_top, x0 + RAIL_W, r.y1), r.x0)
            }
        };

        // Inspector column (right) — mirror of the rail.
        let (inspector, canvas_right) = match insp_layout {
            SplitLayout::Docked => {
                let inset = INSPECTOR_W * insp_reveal;
                (Rect::new(r.x1 - inset, body_top, r.x1, r.y1), r.x1 - inset)
            }
            SplitLayout::Overlay => {
                // Slide in from the right edge; closed ⇒ fully off-screen.
                let slide = self.inspector_flap.slide();
                let x0 = r.x1 - INSPECTOR_W * slide;
                (Rect::new(x0, body_top, x0 + INSPECTOR_W, r.y1), r.x1)
            }
        };

        // A single scrim dims the body whenever either overlay flap is revealed.
        let rail_scrim = layout == SplitLayout::Overlay && reveal > 0.001;
        let insp_scrim = insp_layout == SplitLayout::Overlay && insp_reveal > 0.001;
        let scrim = if rail_scrim || insp_scrim {
            Rect::new(r.x0, body_top, r.x1, r.y1)
        } else {
            Rect::ZERO
        };
        let canvas_area = Rect::new(canvas_left, body_top, canvas_right, r.y1);

        PanelGeometry {
            rail,
            inspector,
            canvas_area,
            layout,
            reveal,
            insp_layout,
            insp_reveal,
            scrim,
            body_top,
        }
    }

    /// Fit the 16:9 slide inside `canvas_area` with padding + caption room.
    /// Returns the destination rect and the slide→pixel scale factor.
    fn fit_slide(canvas_area: Rect) -> (Rect, f64) {
        let pad = 30.0;
        let cap = 34.0;
        let avail_w = (canvas_area.width() - 2.0 * pad).max(40.0);
        let avail_h = (canvas_area.height() - 2.0 * pad - cap).max(40.0);
        let s = (avail_w / SLIDE_W).min(avail_h / SLIDE_H);
        let sw = SLIDE_W * s;
        let sh = SLIDE_H * s;
        let sx = canvas_area.x0 + (canvas_area.width() - sw) / 2.0;
        let sy = canvas_area.y0 + pad + (avail_h - sh) / 2.0;
        (Rect::new(sx, sy, sx + sw, sy + sh), s)
    }

    /// The rail hamburger (offered whenever the rail can collapse — it's an
    /// overlay flap, or docked but closed). Sits at the far left toolbar edge;
    /// returns the x the title block should start at, past it.
    fn toolbar_hamburger(
        toolbar: Rect,
        m: &ChromeMetrics,
        layout: SplitLayout,
        reveal: f64,
        buttons: &mut Vec<UiButton>,
    ) -> f64 {
        let rail_collapsible = layout == SplitLayout::Overlay || reveal < 0.999;
        if rail_collapsible {
            let size = m.pill_h();
            let hy = toolbar.y0 + (toolbar.height() - size) / 2.0;
            let hb = Rect::new(toolbar.x0 + m.s4, hy, toolbar.x0 + m.s4 + size, hy + size);
            buttons.push(UiButton {
                rect: hb,
                kind: ButtonKind::Hamburger,
                hit: Hit::ToggleRail,
            });
            hb.x1 + m.s4
        } else {
            toolbar.x0 + m.s4
        }
    }

    /// Right-anchored toolbar group: Present · palette · overview grid.
    /// Returns the grid button's rect, which the inspector toggle and the
    /// centre tool group both measure from.
    fn toolbar_right_group(
        &self,
        toolbar: Rect,
        by: f64,
        bh: f64,
        buttons: &mut Vec<UiButton>,
    ) -> Rect {
        let m = &self.chrome_metrics;
        let present_w = m.pill_w(&tr("slides-present"), true);
        let present_rect = Rect::new(
            toolbar.x1 - m.s4 - present_w,
            by,
            toolbar.x1 - m.s4,
            by + bh,
        );
        // On a narrow toolbar the palette pill collapses to just its colour dot
        // (the swatch button already hides its label below 40px wide).
        let pal_w = if toolbar.width() > 640.0 {
            Self::palette_pill_width(m, self.deck.palette.label())
        } else {
            bh
        };
        let pal_rect = Rect::new(
            present_rect.x0 - m.s2 - pal_w,
            by,
            present_rect.x0 - m.s2,
            by + bh,
        );
        let grid_rect = Rect::new(pal_rect.x0 - m.s2 - bh, by, pal_rect.x0 - m.s2, by + bh);
        buttons.push(UiButton {
            rect: present_rect,
            kind: ButtonKind::IconText {
                icon: Icon::Monitor,
                label: tr("slides-present"),
                accent: true,
            },
            hit: Hit::Present,
        });
        buttons.push(UiButton {
            rect: pal_rect,
            kind: ButtonKind::Palette {
                kind: self.deck.palette,
                selected: false,
            },
            hit: Hit::CyclePalette,
        });
        buttons.push(UiButton {
            rect: grid_rect,
            kind: ButtonKind::IconOnly {
                icon: Icon::LayoutGrid,
                active: self.mode == Mode::Overview,
            },
            hit: Hit::Overview,
        });
        grid_rect
    }

    /// Inspector toggle — offered whenever the inspector can collapse (it's
    /// an overlay flap, or docked but not fully open). Mirrors the rail
    /// hamburger on the opposite edge. Returns the left edge the centre tool
    /// group must stay clear of.
    fn toolbar_inspector_toggle(
        &self,
        insp_layout: SplitLayout,
        insp_reveal: f64,
        grid_rect: Rect,
        by: f64,
        bh: f64,
        buttons: &mut Vec<UiButton>,
    ) -> f64 {
        let inspector_collapsible = insp_layout == SplitLayout::Overlay || insp_reveal < 0.999;
        if inspector_collapsible {
            let gap = self.chrome_metrics.s2;
            let ib = Rect::new(grid_rect.x0 - gap - bh, by, grid_rect.x0 - gap, by + bh);
            buttons.push(UiButton {
                rect: ib,
                kind: ButtonKind::IconOnly {
                    icon: Icon::Settings2,
                    active: self.inspector_open(),
                },
                hit: Hit::ToggleInspector,
            });
            ib.x0
        } else {
            grid_rect.x0
        }
    }

    /// The deck-title label(s) in the toolbar, elided so they never run under
    /// the tools or right group. Returns the rename click-target rect and the
    /// label's origin (used to place the rename caret).
    fn toolbar_title_block(
        &self,
        toolbar: Rect,
        title_x: f64,
        title_right: f64,
        labels: &mut Vec<Label>,
    ) -> (Rect, Point) {
        // A 390px phone toolbar has room for the rail toggle, the one unified
        // insert overflow and the always-available presentation controls, but
        // not a useful title fragment. Leave that slot empty rather than paint
        // ellipsis under a focusable control.
        let m = &self.chrome_metrics;
        let title_max = if toolbar.width() < 480.0 {
            0.0
        } else {
            (title_right - m.s3 - title_x).max(0.0)
        };
        // The two-line block (title, 2 px seam, subtitle) is centred on the
        // toolbar, so its lines keep their spacing at every text scale.
        let block_top = toolbar.y0 + (toolbar.height() - m.title_block_h()) / 2.0;
        let title_pos = Point::new(title_x, block_top);
        // While renaming, the inline TextInput draws the title (with selection +
        // caret), so the static label is blanked; otherwise the painter elides
        // it with the renderer's own metrics to `title_max`.
        let title_text = if self.editing_title || title_max < 1.0 {
            String::new()
        } else {
            self.deck.title.clone()
        };
        labels.push(Label {
            pos: title_pos,
            text: title_text,
            font: FONT_TITLE,
            color: LabelColor::Primary,
            weight: TextWeight::Bold,
            max_w: Some(title_max),
        });
        // The whole two-line title block is the click target for renaming.
        let title_rect = Rect::new(
            title_x - m.s1,
            block_top - m.s1,
            title_x + title_max,
            block_top + m.title_block_h() + m.s1,
        );
        if title_max >= 1.0 {
            labels.push(Label {
                pos: Point::new(title_x, block_top + m.lh_title + TITLE_LINE_GAP),
                text: tr_args(
                    "slides-rail-subtitle",
                    &[
                        ("current", ArgValue::from(self.cur_slide + 1)),
                        ("total", ArgValue::from(self.deck.slides.len())),
                        ("palette", ArgValue::from(self.deck.palette.label())),
                    ],
                ),
                font: FONT_SUBTITLE,
                color: LabelColor::Dim,
                weight: TextWeight::Regular,
                max_w: Some(title_max),
            });
        }
        (title_rect, title_pos)
    }

    /// Thumbnail rail chrome: the "SLIDES" header and the pinned Add/Dup/Del
    /// row. The shared [`PageSidebar`] owns the list geometry, scroll and
    /// reorder drag inside the returned viewport (between this header and the
    /// pinned action row) itself.
    fn build_rail_chrome(
        &self,
        rail: Rect,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
    ) -> Rect {
        let m = &self.chrome_metrics;
        let pad = m.s4;
        let rail_viewport = self.rail_viewport(rail);
        // Header row: the caption and (for an overlay) its Close button share
        // one centre line.
        let header = Rect::new(
            rail.x0,
            rail.y0 + m.s2,
            rail.x1,
            rail.y0 + m.s2 + m.rail_header_h(),
        );
        labels.push(Label {
            pos: Point::new(
                rail.x0 + pad,
                header.y0 + (header.height() - m.lh_caption) / 2.0,
            ),
            text: tr("slides-slides"),
            font: FONT_CAPTION,
            color: LabelColor::Dim,
            weight: TextWeight::Semibold,
            max_w: None,
        });
        // The toolbar hamburger remains a toggle, but an open overlay must
        // also advertise its local escape route where the user's attention is:
        // the flap header. This keeps the narrow panel dismissible without a
        // hunt back to the global toolbar (and gives AT a named button).
        if self.rail_layout() == SplitLayout::Overlay && self.rail_flap.reveal() > 0.001 {
            let close_w =
                m.label_w(&tr("slides-close"), FONT_CONTROL, TextWeight::Semibold) + 2.0 * m.s3;
            let close_h = m.control_h();
            let close_y = header.y0 + (header.height() - close_h) / 2.0;
            let close = Rect::new(
                rail.x1 - pad - close_w,
                close_y,
                rail.x1 - pad,
                close_y + close_h,
            );
            buttons.push(UiButton {
                rect: close,
                kind: ButtonKind::Text {
                    label: tr("slides-close"),
                    active: false,
                    danger: false,
                },
                hit: Hit::ToggleRail,
            });
        }
        // Rail action buttons at the bottom: three equal controls, `s2` apart.
        let rb_h = m.control_h();
        let rb_y = rail.y1 - pad - rb_h;
        let rb_w = (rail.width() - 2.0 * pad - 2.0 * m.s2) / 3.0;
        let mk = |idx: f64, label: &str, danger: bool, hit: Hit| -> UiButton {
            let x = rail.x0 + pad + idx * (rb_w + m.s2);
            UiButton {
                rect: Rect::new(x, rb_y, x + rb_w, rb_y + rb_h),
                kind: ButtonKind::Text {
                    label: label.to_string(),
                    active: false,
                    danger,
                },
                hit,
            }
        };
        buttons.push(mk(0.0, &tr("slides-add"), false, Hit::NewSlide));
        buttons.push(mk(1.0, &tr("slides-dup"), false, Hit::DuplicateSlide));
        buttons.push(mk(2.0, &tr("slides-del"), true, Hit::DeleteSlide));
        rail_viewport
    }

    /// Fill picker popover, anchored inside the inspector, clamped on-screen.
    /// `None` while the picker isn't open.
    fn fill_picker_popover_rect(&self, inspector: Rect, r: Rect, body_top: f64) -> Option<Rect> {
        if !self.fill_picker_open {
            return None;
        }
        // The shared picker spans its sliders across the rect it is given,
        // so the reserve's width grows with the text scale; its height does
        // not (the picker's vertical layout is fixed). Its hex / rgb readouts
        // still overrun the rect at 2× — a `ColorPicker` defect, recorded for
        // its owner in the A7 round-two review.
        let m = &self.chrome_metrics;
        let scale = m.scale;
        let (w, h) = (FILL_PICKER_W * scale, FILL_PICKER_H);
        // Leave ~64px of slack on the right: the picker's hex field extends
        // past its preview swatch, so a flush-right anchor would clip it.
        let x = (inspector.x0 + INSPECTOR_PAD)
            .min(r.x1 - w - 64.0 * scale)
            .max(r.x0 + m.s2);
        // Sit below the top property controls, above the notes box, and
        // keep the whole popover panel (caption row above, `s3` below) at
        // least `s1` inside the body so it never paints over the toolbar.
        let (top_inset, bottom_inset) = self.fill_picker_panel_insets();
        let y = (inspector.y0 + 300.0)
            .min(r.y1 - h - bottom_inset - m.s1)
            .max(body_top + top_inset + m.s1);
        Some(Rect::new(x, y, x + w, y + h))
    }

    /// How far the fill-picker popover panel extends beyond the picker's own
    /// rect above (its caption row: `s2` + the caption line box + `s2`) and
    /// below (`s3`); the sides are `s3`.
    fn fill_picker_panel_insets(&self) -> (f64, f64) {
        let m = &self.chrome_metrics;
        (m.s2 + m.lh_caption + m.s2, m.s3)
    }

    /// The popover panel painted around the fill picker at `picker` — the
    /// same rect `draw_inspector_widgets` paints and the pointer dismisses
    /// against, so a click on the caption row never closes the popover.
    fn fill_picker_panel_rect(&self, picker: Rect) -> Rect {
        let (top, bottom) = self.fill_picker_panel_insets();
        let side = self.chrome_metrics.s3;
        Rect::new(
            picker.x0 - side,
            picker.y0 - top,
            picker.x1 + side,
            picker.y1 + bottom,
        )
    }

    /// Lay out the inspector contents; returns the speaker-notes box rect.
    fn build_inspector(
        &self,
        inspector: Rect,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
    ) -> InspectorRects {
        let pad = INSPECTOR_PAD;
        let x0 = inspector.x0 + pad;
        let x1 = inspector.x1 - pad;
        let inner_w = x1 - x0;
        let y = inspector.y0 + pad;

        let (y, font_input_rect) = if let Some(el) = self.selected_element() {
            self.build_inspector_element(el, x0, x1, inner_w, y, buttons, labels)
        } else {
            (
                self.build_inspector_slide_props(x0, x1, y, buttons, labels),
                None,
            )
        };

        let nb = self.build_inspector_notes(inspector, x0, x1, pad, y, labels, buttons);

        InspectorRects {
            notes: Some(nb),
            font_input: font_input_rect,
        }
    }

    /// Push a small dim "SECTION" header label at `x0, *y` and advance `*y`
    /// past it. Shared by every inspector section.
    fn push_inspector_header(
        m: &ChromeMetrics,
        labels: &mut Vec<Label>,
        x0: f64,
        y: &mut f64,
        t: &str,
    ) {
        labels.push(Label {
            pos: Point::new(x0, *y),
            text: t.to_string(),
            font: FONT_CAPTION,
            color: LabelColor::Dim,
            weight: TextWeight::Semibold,
            max_w: None,
        });
        *y += m.caption_adv();
    }

    /// Inspector contents for a selected element: type label, fill/stroke
    /// swatches, (text elements only) the TYPE row, and the arrange actions.
    /// Returns the y just past the arrange row and the font-size input's
    /// rect (`Some` only for a selected text element).
    fn build_inspector_element(
        &self,
        el: &Element,
        x0: f64,
        x1: f64,
        inner_w: f64,
        y: f64,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
    ) -> (f64, Option<Rect>) {
        let m = &self.chrome_metrics;
        let mut y = y;
        labels.push(Label {
            pos: Point::new(x0, y),
            text: el.kind.label().to_uppercase(),
            font: FONT_HEADING,
            color: LabelColor::Primary,
            weight: TextWeight::Bold,
            max_w: None,
        });
        y += m.heading_adv();

        // Rhythm: rows inside a section are `s2` apart; sections are `s4`
        // apart. Fill swatches (theme presets) plus a Custom… button that
        // opens the embeddable colour picker for an arbitrary fill.
        Self::push_inspector_header(m, labels, x0, &mut y, &tr("slides-fill"));
        y = self.swatch_grid(buttons, x0, inner_w, y, &FILL_ROLES, &el.fill, true);
        y += m.s2;
        let custom = Rect::new(x0, y, x1, y + m.control_h());
        buttons.push(UiButton {
            rect: custom,
            kind: ButtonKind::Text {
                // Show the active colour role / hex on the button.
                label: if el.fill.starts_with('#') {
                    custom_colour_hex(&el.fill)
                } else {
                    tr("slides-custom-colour")
                },
                active: el.fill.starts_with('#'),
                danger: false,
            },
            hit: Hit::OpenFillPicker,
        });
        y += m.control_h() + m.s4;

        // Stroke swatches.
        Self::push_inspector_header(m, labels, x0, &mut y, &tr("slides-stroke"));
        y = self.swatch_grid(buttons, x0, inner_w, y, &STROKE_ROLES, &el.stroke, false);
        y += m.s4;

        let font_input_rect = if el.kind == ElementKind::Text {
            let (new_y, rect) =
                Self::build_inspector_type_row(m, el, x0, x1, inner_w, y, buttons, labels);
            y = new_y;
            rect
        } else {
            None
        };

        y = Self::build_inspector_arrange_row(m, x0, x1, inner_w, y, buttons, labels);

        (y, font_input_rect)
    }

    /// The ARRANGE row for a selected element: bring-front/send-back and
    /// duplicate/delete, each pair a half-width button. Returns the y just
    /// past it.
    fn build_inspector_arrange_row(
        m: &ChromeMetrics,
        x0: f64,
        x1: f64,
        inner_w: f64,
        y: f64,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
    ) -> f64 {
        let mut y = y;
        Self::push_inspector_header(m, labels, x0, &mut y, &tr("slides-arrange"));
        let bh = m.control_h();
        let half = (inner_w - m.s2) / 2.0;
        let front = Rect::new(x0, y, x0 + half, y + bh);
        let back = Rect::new(x0 + half + m.s2, y, x1, y + bh);
        buttons.push(UiButton {
            rect: front,
            kind: ButtonKind::Text {
                label: tr("slides-bring-front"),
                active: false,
                danger: false,
            },
            hit: Hit::BringFront,
        });
        buttons.push(UiButton {
            rect: back,
            kind: ButtonKind::Text {
                label: tr("slides-send-back"),
                active: false,
                danger: false,
            },
            hit: Hit::SendBack,
        });
        y += bh + m.s2;
        let dup = Rect::new(x0, y, x0 + half, y + bh);
        let del = Rect::new(x0 + half + m.s2, y, x1, y + bh);
        buttons.push(UiButton {
            rect: dup,
            kind: ButtonKind::Text {
                label: tr("slides-duplicate"),
                active: false,
                danger: false,
            },
            hit: Hit::DuplicateElement,
        });
        buttons.push(UiButton {
            rect: del,
            kind: ButtonKind::Text {
                label: tr("slides-delete"),
                active: false,
                danger: true,
            },
            hit: Hit::DeleteElement,
        });
        y + bh + m.s4
    }

    /// The TYPE row for a selected text element: the font-size input's
    /// reserved rect, bold/italic toggles, and an alignment segmented
    /// control. Returns the y just past it and the font-size input's rect.
    fn build_inspector_type_row(
        m: &ChromeMetrics,
        el: &Element,
        x0: f64,
        x1: f64,
        inner_w: f64,
        y: f64,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
    ) -> (f64, Option<Rect>) {
        let mut y = y;
        Self::push_inspector_header(m, labels, x0, &mut y, &tr("slides-type"));
        let bh = m.control_h();
        // The font size is a click-to-type NumberInput (with ± steppers)
        // — the widget draws itself, so we only reserve its rect here: two
        // square steppers around a three-digit "pt" value, never narrower than
        // the classic 136 px.
        let font_input_w =
            (2.0 * bh + m.label_w("999 pt", FONT_CONTROL, TextWeight::Semibold) + 2.0 * m.s2)
                .max(FONT_INPUT_W);
        let font_input_rect = Some(Rect::new(x0, y, x0 + font_input_w, y + bh));
        // Bold / italic toggles: square-ish controls sized to their glyph,
        // right-anchored beside the input while both fit, otherwise on their
        // own row `s2` below it so neither ever paints over the other.
        let toggle_w = (m.label_w("B", FONT_CONTROL, TextWeight::Semibold) + 2.0 * m.s3).max(bh);
        let toggles_w = 2.0 * toggle_w + m.s2;
        let toggles_y = if font_input_w + m.s2 + toggles_w <= inner_w + 0.5 {
            y
        } else {
            y + bh + m.s2
        };
        let bold = Rect::new(x1 - toggle_w, toggles_y, x1, toggles_y + bh);
        buttons.push(UiButton {
            rect: bold,
            kind: ButtonKind::Text {
                label: "B".into(),
                active: el.bold,
                danger: false,
            },
            hit: Hit::ToggleBold,
        });
        let italic = Rect::new(
            x1 - toggle_w - m.s2 - toggle_w,
            toggles_y,
            x1 - toggle_w - m.s2,
            toggles_y + bh,
        );
        buttons.push(UiButton {
            rect: italic,
            kind: ButtonKind::Text {
                label: "I".into(),
                active: el.italic,
                danger: false,
            },
            hit: Hit::ToggleItalic,
        });
        y = toggles_y + bh + m.s4;
        Self::push_inspector_header(m, labels, x0, &mut y, &tr("slides-align"));
        // A proper segmented control: left · center · right · justify,
        // the current alignment highlighted. Replaces the old single
        // button that only cycled through the options blind. When the widest
        // label (plus `s2` each side) no longer fits an equal share of the
        // row, the group wraps into two connected rows of two, `s2` apart.
        let n = TextAlign::ALL.len();
        let widest = TextAlign::ALL
            .iter()
            .map(|a| m.label_w(a.glyph(), FONT_CONTROL, TextWeight::Semibold))
            .fold(0.0_f64, f64::max);
        let cols = if widest + 2.0 * m.s2 <= inner_w / n as f64 {
            n
        } else {
            2
        };
        let seg_w = inner_w / cols as f64;
        for (i, a) in TextAlign::ALL.iter().enumerate() {
            let col = i % cols;
            let row = (i / cols) as f64;
            let sx = x0 + col as f64 * seg_w;
            let ex = if col + 1 == cols {
                x1
            } else {
                x0 + (col as f64 + 1.0) * seg_w
            };
            let pos = if col == 0 {
                SegPos::First
            } else if col + 1 == cols {
                SegPos::Last
            } else {
                SegPos::Mid
            };
            let sy = y + row * (bh + m.s2);
            buttons.push(UiButton {
                rect: Rect::new(sx, sy, ex, sy + bh),
                kind: ButtonKind::Segment {
                    label: a.glyph().to_string(),
                    active: *a == el.align,
                    pos,
                },
                hit: Hit::SetAlign(*a),
            });
        }
        let rows = n.div_ceil(cols) as f64;
        y += rows * bh + (rows - 1.0) * m.s2 + m.s4;
        (y, font_input_rect)
    }

    /// Inspector contents when nothing is selected: the slide-level layout /
    /// background pills and the deck-theme pills. Returns the y just past
    /// them.
    fn build_inspector_slide_props(
        &self,
        x0: f64,
        x1: f64,
        y: f64,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
    ) -> f64 {
        let m = &self.chrome_metrics;
        let mut y = y;
        labels.push(Label {
            pos: Point::new(x0, y),
            text: tr_args(
                "slides-slide-n",
                &[("n", ArgValue::from(self.cur_slide + 1))],
            ),
            font: FONT_HEADING,
            color: LabelColor::Primary,
            weight: TextWeight::Bold,
            max_w: None,
        });
        y += m.heading_adv();

        Self::push_inspector_header(m, labels, x0, &mut y, &tr("slides-layout"));
        let cur_layout = self.slide().map_or(Layout::TitleContent, |s| s.layout);
        y = self.wrap_pills(
            buttons,
            x0,
            x1,
            y,
            Layout::ALL
                .iter()
                .map(|l| (l.label().to_string(), *l == cur_layout, Hit::SetLayout(*l))),
        );
        y += m.s4;

        Self::push_inspector_header(m, labels, x0, &mut y, &tr("slides-background"));
        let cur_bg = self.slide().map_or(Background::Surface, |s| s.background);
        y = self.wrap_pills(
            buttons,
            x0,
            x1,
            y,
            Background::ALL
                .iter()
                .map(|b| (b.label().to_string(), *b == cur_bg, Hit::SetBackground(*b))),
        );
        y += m.s4;

        Self::push_inspector_header(m, labels, x0, &mut y, &tr("slides-deck-theme"));
        y = self.wrap_palette_pills(buttons, x0, x1, y);
        y += m.s4;

        y
    }

    /// Speaker notes box (always shown at the bottom of the inspector).
    /// Returns its rect.
    fn build_inspector_notes(
        &self,
        inspector: Rect,
        x0: f64,
        x1: f64,
        pad: f64,
        y: f64,
        labels: &mut Vec<Label>,
        buttons: &mut Vec<UiButton>,
    ) -> Rect {
        let m = &self.chrome_metrics;
        // The notes block sits under the property sections, pulled up to leave
        // it `NOTES_MIN_H` when there is room. When the sections outgrow the
        // panel (large text in a short window) it flows on below them rather
        // than folding back over them or inverting: the box is always at
        // least one control tall and its top is always below the sections.
        let notes_top = y.max(inspector.y1 - NOTES_MIN_H);
        labels.push(Label {
            pos: Point::new(x0, notes_top),
            text: if self.editing_notes {
                tr("slides-speaker-notes-editing")
            } else {
                tr("slides-speaker-notes")
            },
            font: FONT_CAPTION,
            color: LabelColor::Dim,
            weight: TextWeight::Semibold,
            max_w: Some(x1 - x0),
        });
        let nb_y0 = notes_top + m.caption_adv();
        let nb = Rect::new(
            x0,
            nb_y0,
            x1,
            (inspector.y1 - pad).max(nb_y0 + m.control_h()),
        );
        buttons.push(UiButton {
            rect: nb,
            kind: ButtonKind::Text {
                label: String::new(),
                active: false,
                danger: false,
            },
            hit: Hit::EditNotes,
        });
        nb
    }

    #[allow(
        clippy::unused_self,
        reason = "keeps the ergonomic self.swatch_grid(...) call site consistent with the \
                  other inspector layout helpers; converting to an associated function would \
                  read worse at every call site for no benefit"
    )]
    fn swatch_grid(
        &self,
        buttons: &mut Vec<UiButton>,
        x0: f64,
        inner_w: f64,
        mut y: f64,
        roles: &[&str],
        current: &str,
        is_fill: bool,
    ) -> f64 {
        let cols = 4.0;
        let gap = self.chrome_metrics.s2;
        let sw = (inner_w - (cols - 1.0) * gap) / cols;
        let sh = SWATCH_H;
        for (i, role) in roles.iter().enumerate() {
            let col = (i as f64) % cols;
            let row = ((i as f64) / cols).floor();
            let x = x0 + col * (sw + gap);
            let yy = y + row * (sh + gap);
            buttons.push(UiButton {
                rect: Rect::new(x, yy, x + sw, yy + sh),
                kind: ButtonKind::Swatch {
                    role: role.to_string(),
                    selected: *role == current,
                },
                hit: if is_fill {
                    Hit::SetFill(role.to_string())
                } else {
                    Hit::SetStroke(role.to_string())
                },
            });
        }
        let rows = ((roles.len() as f64) / cols).ceil();
        y += rows * (sh + gap) - gap;
        y
    }

    /// Width a palette pill needs to show its full label without truncation:
    /// the colour dot (drawn at `x0+8 .. x0+20`), a 6px gap, the measured
    /// label, and a trailing `s3` pad.
    fn palette_pill_width(m: &ChromeMetrics, label: &str) -> f64 {
        PALETTE_DOT_SLOT + m.label_w(label, FONT_PILL, TextWeight::Semibold) + m.s3
    }

    /// Lay out the five deck-theme pills, each sized to fit its full name and
    /// wrapping onto a new row when the next pill would overflow `x1`. This is
    /// what keeps every theme name ("Indigo", "Emerald", …) fully readable in
    /// the docked inspector at any width, instead of packing five fixed-width
    /// pills that clip their labels. Returns the y just past the last row.
    fn wrap_palette_pills(&self, buttons: &mut Vec<UiButton>, x0: f64, x1: f64, mut y: f64) -> f64 {
        let m = &self.chrome_metrics;
        let bh = m.small_pill_h();
        let gap = m.s2;
        let mut x = x0;
        for p in &PaletteKind::ALL {
            let w = Self::palette_pill_width(m, p.label()).min(x1 - x0);
            if x > x0 && x + w > x1 + 0.5 {
                x = x0;
                y += bh + gap;
            }
            buttons.push(UiButton {
                rect: Rect::new(x, y, x + w, y + bh),
                kind: ButtonKind::Palette {
                    kind: *p,
                    selected: *p == self.deck.palette,
                },
                hit: Hit::SetPalette(*p),
            });
            x += w + gap;
        }
        y + bh
    }

    #[allow(
        clippy::unused_self,
        reason = "keeps the ergonomic self.wrap_pills(...) call site consistent with the other \
                  inspector layout helpers; converting to an associated function would read \
                  worse at every call site for no benefit"
    )]
    fn wrap_pills(
        &self,
        buttons: &mut Vec<UiButton>,
        x0: f64,
        x1: f64,
        mut y: f64,
        items: impl Iterator<Item = (String, bool, Hit)>,
    ) -> f64 {
        let m = &self.chrome_metrics;
        let bh = m.small_pill_h();
        let gap = m.s2;
        let mut x = x0;
        for (label, active, hit) in items {
            // Pills paint as `Text` buttons at the control font, so the slot is
            // measured with that same style.
            let w = m.label_w(&label, FONT_CONTROL, TextWeight::Semibold) + 2.0 * m.s3;
            if x > x0 && x + w > x1 {
                x = x0;
                y += bh + gap;
            }
            buttons.push(UiButton {
                rect: Rect::new(x, y, x + w, y + bh),
                kind: ButtonKind::Text {
                    label,
                    active,
                    danger: false,
                },
                hit,
            });
            x += w + gap;
        }
        y + bh
    }

    // --- mouse → slide coords ----------------------------------------------

    #[allow(
        clippy::unused_self,
        reason = "keeps the ergonomic self.to_slide(...) call site consistent with the other \
                  slide-space geometry helpers; converting to an associated function would read \
                  worse at every call site for no benefit"
    )]
    fn to_slide(&self, dest: Rect, s: f64, p: Point) -> Point {
        Point::new((p.x - dest.x0) / s, (p.y - dest.y0) / s)
    }

    #[allow(
        clippy::unused_self,
        reason = "keeps the ergonomic self.handles_of(...) call site consistent with the other \
                  slide-space geometry helpers; converting to an associated function would read \
                  worse at every call site for no benefit"
    )]
    fn handles_of(&self, dest: Rect, s: f64, el: &Element) -> Vec<(Handle, Rect)> {
        let er = render::element_dest(dest, s, el);
        // Anchor the handles on a rect outset from the content by
        // `HANDLE_OFFSET`, so each handle sits just *outside* the element and
        // can never overlap the content (e.g. the first glyph of a left-anchored
        // title). Resize math keys off the pointer position, not these centres,
        // so pushing them outward doesn't change resize behaviour.
        let hr = er.inflate(HANDLE_OFFSET, HANDLE_OFFSET);
        let cx = hr.center().x;
        let cy = hr.center().y;
        let pos = |h: Handle| -> Point {
            match h {
                Handle::Tl => Point::new(hr.x0, hr.y0),
                Handle::T => Point::new(cx, hr.y0),
                Handle::Tr => Point::new(hr.x1, hr.y0),
                Handle::R => Point::new(hr.x1, cy),
                Handle::Br => Point::new(hr.x1, hr.y1),
                Handle::B => Point::new(cx, hr.y1),
                Handle::Bl => Point::new(hr.x0, hr.y1),
                Handle::L => Point::new(hr.x0, cy),
            }
        };
        Handle::ALL
            .iter()
            .map(|h| {
                let p = pos(*h);
                (
                    *h,
                    Rect::new(p.x - HANDLE, p.y - HANDLE, p.x + HANDLE, p.y + HANDLE),
                )
            })
            .collect()
    }

    // --- actions ------------------------------------------------------------

    fn apply_hit(&mut self, hit: Hit) {
        // Any action other than toggling the overflow menu itself dismisses it
        // (picking a tool from it, or any other toolbar action).
        if !matches!(hit, Hit::ToggleToolbarMenu) {
            self.toolbar_tools.dismiss();
        }
        match hit {
            Hit::ToggleToolbarMenu => self.toolbar_tools.toggle(),
            Hit::NewSlide => self.new_slide(),
            Hit::DuplicateSlide => self.duplicate_slide(),
            Hit::DeleteSlide => self.delete_slide(),
            Hit::Present => self.enter_present(),
            Hit::Overview => {
                self.mode = if self.mode == Mode::Overview {
                    Mode::Edit
                } else {
                    Mode::Overview
                };
                self.editing_text = false;
                self.editing_notes = false;
            }
            Hit::OpenSlide(i) => {
                self.cur_slide = i.min(self.deck.slides.len().saturating_sub(1));
                self.sel = None;
                self.mode = Mode::Edit;
            }
            Hit::CyclePalette => {
                self.submit(&actions::set_deck_palette_action(self.deck.palette.next()));
            }
            // Images can't be conjured from nothing — the tool opens the host
            // portal file chooser; the chosen file is inserted when it resolves.
            Hit::Insert(ElementKind::Image) => self.request_image_open(),
            Hit::Insert(kind) => self.insert_element(kind),
            Hit::SelectSlide(i) => {
                self.cur_slide = i.min(self.deck.slides.len().saturating_sub(1));
                self.sel = None;
                self.editing_text = false;
                self.editing_notes = false;
                self.scroll_rail_to_current();
            }
            Hit::SetFill(role) => self.set_sel_str("fill", &role),
            Hit::OpenFillPicker => {
                // Only meaningful with an element selected. The next `draw`
                // seeds the picker with the element's current fill (it has the
                // theme to resolve a colour role).
                if self.sel.is_some() {
                    self.fill_picker_open = true;
                    self.fill_picker_needs_init = true;
                }
            }
            Hit::SetStroke(role) => {
                self.set_sel_str("stroke", &role);
                // Give a freshly-stroked shape a sensible default width.
                if let Some(el) = self.selected_element()
                    && role != "none"
                    && el.stroke_width < 1.0
                {
                    self.set_sel_f64("stroke_width", 4.0);
                }
            }
            Hit::ToggleBold => {
                if let Some(el) = self.selected_element() {
                    let v = !el.bold;
                    self.set_sel_bool("bold", v);
                }
            }
            Hit::ToggleItalic => {
                if let Some(el) = self.selected_element() {
                    let v = !el.italic;
                    self.set_sel_bool("italic", v);
                }
            }
            Hit::SetAlign(a) => self.set_sel_str("align", a.as_str()),
            Hit::BringFront => self.restack(true),
            Hit::SendBack => self.restack(false),
            Hit::DuplicateElement => self.duplicate_element(),
            Hit::DeleteElement => self.delete_element(),
            Hit::SetLayout(l) => {
                if let Some(id) = self.slide().map(|slide| slide.id.clone()) {
                    self.submit(&actions::set_slide_layout_action(self.cur_slide, id, l));
                }
            }
            Hit::SetBackground(b) => {
                if let Some(id) = self.slide().map(|slide| slide.id.clone()) {
                    self.submit(&actions::set_slide_background_action(self.cur_slide, id, b));
                }
            }
            Hit::SetPalette(p) => {
                self.submit(&actions::set_deck_palette_action(p));
            }
            Hit::EditNotes => self.begin_notes_edit(),
            Hit::ToggleRail => self.toggle_rail_flap(),
            Hit::ToggleInspector => self.toggle_inspector_flap(),
        }
    }

    /// Compact editing exposes one drawer at a time. Opening the slide rail
    /// immediately dismisses an overlay inspector, keeping the slide usable
    /// behind the drawer instead of sandwiching it between two panels.
    fn toggle_rail_flap(&mut self) {
        let layout = self.rail_layout();
        if layout == SplitLayout::Overlay
            && self.inspector_layout() == SplitLayout::Overlay
            && !self.rail_open()
        {
            self.inspector_flap.snap(false);
        }
        self.rail_flap.toggle(layout);
    }

    /// Mirror [`Self::toggle_rail_flap`] for the Properties drawer. Docked
    /// panels remain independent on roomy windows; mutual exclusion applies
    /// only where both are overlays.
    fn toggle_inspector_flap(&mut self) {
        let layout = self.inspector_layout();
        if layout == SplitLayout::Overlay
            && self.rail_layout() == SplitLayout::Overlay
            && !self.inspector_open()
        {
            self.rail_flap.snap(false);
        }
        self.inspector_flap.toggle(layout);
    }

    fn set_sel_str(&self, key: &str, v: &str) {
        if let Some(id) = self.sel.clone() {
            self.submit(&actions::restyle_str_action(self.cur_slide, id, key, v));
        }
    }
    fn set_sel_f64(&self, key: &str, v: f64) {
        if let Some(id) = self.sel.clone() {
            self.submit(&actions::restyle_f64_action(self.cur_slide, id, key, v));
        }
    }
    fn set_sel_bool(&self, key: &str, v: bool) {
        if let Some(id) = self.sel.clone() {
            self.submit(&actions::restyle_bool_action(self.cur_slide, id, key, v));
        }
    }

    fn restack(&self, front: bool) {
        let Some(id) = self.sel.clone() else { return };
        let Some(slide_id) = self.slide().map(|slide| slide.id.clone()) else {
            return;
        };
        let Some(rank) = actions::restack_z_for_author(&self.deck, &slide_id, &id, front) else {
            return;
        };
        self.submit(&actions::set_element_z_action(slide_id, id, rank));
    }

    fn new_slide(&mut self) {
        let s = actions::new_slide_template();
        let at = self.cur_slide + 1;
        let rank = crate::doc::rank_for_insert(&self.ranks, at);
        self.submit(&actions::insert_slide_with_order_action(&s, rank));
        self.focus_slide = Some(s.id);
        self.cur_slide = at;
        self.sel = None;
        self.editing_text = false;
        self.scroll_rail_to_current();
    }

    fn duplicate_slide(&mut self) {
        let Some(slide) = self.deck.slides.get(self.cur_slide).cloned() else {
            return;
        };
        let mut copy = slide;
        copy.id = new_id();
        for e in &mut copy.elements {
            e.id = new_id();
        }
        let at = self.cur_slide + 1;
        let rank = crate::doc::rank_for_insert(&self.ranks, at);
        self.submit(&actions::insert_slide_with_order_action(&copy, rank));
        self.focus_slide = Some(copy.id);
        self.cur_slide = at;
        self.sel = None;
        self.scroll_rail_to_current();
    }

    fn delete_slide(&mut self) {
        if self.deck.slides.len() <= 1 {
            return; // keep at least one slide
        }
        let Some(id) = self.slide().map(|slide| slide.id.clone()) else {
            return;
        };
        self.submit(&actions::delete_slide_action(self.cur_slide, id));
        if self.cur_slide > 0 {
            self.cur_slide -= 1;
        }
        self.sel = None;
    }

    fn insert_element(&mut self, kind: ElementKind) {
        let el = actions::inserted_element_template(kind);
        let id = el.id.clone();
        let Some(slide_id) = self.slide().map(|slide| slide.id.clone()) else {
            return;
        };
        self.submit(&actions::add_element_action(self.cur_slide, slide_id, &el));
        self.sel = Some(id);
        self.editing_text = false;
    }

    /// Ask the host desktop's file chooser to pick an image, via the XDG
    /// Desktop Portal. Per HIG §5 the app never draws its own picker: the
    /// portal round-trip runs on a worker thread ([`ui::portal::PickRequest`]) and
    /// the chosen path comes back over a channel drained each frame in
    /// [`Self::drain_portal`]. A second request while one is in flight is
    /// ignored, so a double-click can't stack two host dialogs.
    fn request_image_open(&mut self) {
        if self.portal_rx.is_some() {
            return;
        }
        self.portal_rx = Some(image_open_request().send());
    }

    /// Ask the host to pick a `.odp` presentation to open (Ctrl+O). The chosen
    /// file is read and decoded when the request drains in
    /// [`Self::drain_doc_portal`]. One request at a time.
    fn request_odp_open(&mut self) {
        if self.doc_portal_rx.is_some() {
            return;
        }
        self.doc_portal_rx = Some(open_document_request());
    }

    /// Ask the host where to save the current deck as a `.odp` (Ctrl+S).
    fn request_odp_save(&mut self) {
        if self.doc_portal_rx.is_some() {
            return;
        }
        let name = format!("{}.odp", sanitize_filename(&self.deck.title));
        self.doc_portal_rx = Some(save_document_request(&name));
    }

    /// Drain a completed `.odp` open/save request, loading or writing the deck.
    /// Called once per frame from [`Widget::sync`].
    fn drain_doc_portal(&mut self) {
        let Some(rx) = self.doc_portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.doc_portal_rx = None;
                match outcome {
                    DocOutcome::Open(path) => {
                        let power_point = is_pptx(&path);
                        if !self.queue_document_open(path) {
                            self.io_toast =
                                Some((tr("slides-toast-import-failed"), ToastKind::Error));
                            self.finish_resource_file(
                                false,
                                Some("presentation import could not be started".into()),
                            );
                        } else if power_point {
                            self.io_toast =
                                Some((tr("slides-toast-opening-pptx"), ToastKind::Info));
                        }
                    }
                    DocOutcome::Save(path) => {
                        if !self.queue_document_save(path) {
                            self.io_toast =
                                Some((tr("slides-toast-export-failed"), ToastKind::Error));
                            self.finish_resource_file(
                                false,
                                Some("presentation export could not be started".into()),
                            );
                        }
                    }
                    DocOutcome::Cancelled => self.finish_resource_file(false, None),
                    DocOutcome::Failed(reason) => {
                        eprintln!("slides: file chooser failed: {reason}");
                        self.io_toast = Some((tr("slides-toast-chooser-failed"), ToastKind::Error));
                        self.finish_resource_file(false, Some(reason));
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.doc_portal_rx = None;
            }
        }
    }

    fn finish_resource_file(&mut self, success: bool, error: Option<String>) {
        let Some(invocation) = self.resource_invocation.take() else {
            return;
        };
        self.finish_file_invocation(invocation, success, error);
    }

    fn finish_file_invocation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        success: bool,
        error: Option<String>,
    ) {
        self.completed_resource_invocation = Some(if success {
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            }
        } else if let Some(message) = error {
            tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "slides.file_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            }
        } else {
            tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("file selection cancelled".into()),
            }
        });
    }

    fn next_document_generation(&mut self) -> Option<u64> {
        let next = self.next_document_generation.checked_add(1)?;
        self.next_document_generation = next;
        Some(next)
    }

    fn queue_document_open(&mut self, path: PathBuf) -> bool {
        if self.dispatched_save.is_some() {
            return false;
        }
        let Some(generation) = self.next_document_generation() else {
            return false;
        };
        let Some(worker) = self.document_worker.as_ref() else {
            self.io_toast = Some((
                self.document_worker_error
                    .clone()
                    .unwrap_or_else(|| "presentation I/O is unavailable".into()),
                ToastKind::Error,
            ));
            return false;
        };
        let queued = worker.tx.try_send((
            DocumentJob::Open { generation, path },
            ui::wake::WakeHandle::new(),
        ));
        match queued {
            Ok(()) => {
                self.active_document_generation = Some(generation);
                true
            }
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => {
                self.document_worker = None;
                self.document_worker_error =
                    Some("presentation I/O worker stopped unexpectedly".into());
                false
            }
        }
    }

    fn queue_document_save(&mut self, path: PathBuf) -> bool {
        if self.dispatched_save.is_some() {
            return false;
        }
        if !actions::deck_within_aggregate_limits(&self.deck) {
            self.io_toast = Some((tr("slides-toast-export-limit"), ToastKind::Error));
            return false;
        }
        let Some(generation) = self.next_document_generation() else {
            return false;
        };
        let Some(worker) = self.document_worker.as_ref() else {
            self.io_toast = Some((
                self.document_worker_error
                    .clone()
                    .unwrap_or_else(|| "presentation I/O is unavailable".into()),
                ToastKind::Error,
            ));
            return false;
        };
        let queued = worker.tx.try_send((
            DocumentJob::Save {
                generation,
                path,
                deck: self.deck.clone(),
            },
            ui::wake::WakeHandle::new(),
        ));
        match queued {
            Ok(()) => {
                self.active_document_generation = Some(generation);
                self.dispatched_save = Some(DispatchedSave {
                    generation,
                    invocation: self.resource_invocation.take(),
                });
                true
            }
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => {
                self.document_worker = None;
                self.document_worker_error =
                    Some("presentation I/O worker stopped unexpectedly".into());
                false
            }
        }
    }

    /// Open a presentation file's deck in place of this one: one edit, so
    /// one undo puts the previous deck back.
    fn apply_loaded_deck(&mut self, deck: &Deck) {
        self.submit(&actions::replace_deck_action(deck));
        self.cur_slide = 0;
        self.sel = None;
        self.editing_text = false;
        self.editing_notes = false;
        self.presentation.invalidate_cache();
    }

    fn drain_document_worker(&mut self) {
        let Some(received) = self
            .document_worker
            .as_ref()
            .map(|worker| worker.rx.try_recv())
        else {
            return;
        };
        let result = match received {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.document_worker = None;
                let error = "presentation I/O worker stopped unexpectedly".to_string();
                self.document_worker_error = Some(error.clone());
                self.active_document_generation = None;
                self.io_toast = Some((error.clone(), ToastKind::Error));
                if let Some(save) = self.dispatched_save.take()
                    && let Some(invocation) = save.invocation
                {
                    self.finish_file_invocation(invocation, false, Some(error));
                } else {
                    self.finish_resource_file(false, Some(error));
                }
                return;
            }
        };
        if let DocumentJobResult::Save { generation, result } = result {
            let Some(save) = self
                .dispatched_save
                .take_if(|save| save.generation == generation)
            else {
                return;
            };
            if self.active_document_generation == Some(generation) {
                self.active_document_generation = None;
            }
            self.io_toast = Some(if result.is_ok() {
                ("Presentation saved".into(), ToastKind::Success)
            } else {
                (
                    "Could not process the presentation".into(),
                    ToastKind::Error,
                )
            });
            if let Some(invocation) = save.invocation {
                self.finish_file_invocation(invocation, result.is_ok(), result.err());
            }
            return;
        }
        let DocumentJobResult::Open { generation, result } = result else {
            return;
        };
        if self.active_document_generation != Some(generation) {
            return;
        }
        self.active_document_generation = None;
        match result {
            Ok(deck) => {
                self.apply_loaded_deck(&deck);
                self.io_toast = Some((tr("slides-toast-opened"), ToastKind::Success));
                self.finish_resource_file(true, None);
            }
            Err(error) => {
                self.io_toast = Some((tr("slides-toast-process-failed"), ToastKind::Error));
                self.finish_resource_file(false, Some(error));
            }
        }
    }

    /// Advance the render-cache generation without letting a wrapping integer
    /// accidentally make a newly loaded deck look like an old cached frame.
    /// The practically unreachable boundary still has an explicit, safe
    /// outcome: drop caches before beginning a fresh generation.
    fn bump_deck_revision(&mut self) {
        if let Some(next) = self.deck_rev.checked_add(1) {
            self.deck_rev = next;
        } else {
            self.deck_rev = 0;
            self.presentation.invalidate_cache();
        }
    }

    /// Drain a completed portal request, if any. Called once per frame from
    /// [`Widget::sync`]; inserts the chosen image and clears the in-flight
    /// channel. Never blocks — a still-pending picker simply leaves the channel
    /// installed for the next frame.
    fn drain_portal(&mut self) {
        let Some(rx) = self.portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                match outcome {
                    PortalOutcome::Opened(path) => self.insert_image_from_path(&path),
                    // A cancel or a portal failure is not a bug — leave the deck
                    // untouched (we never fabricate a placeholder element).
                    PortalOutcome::Cancelled => {}
                    PortalOutcome::Failed(reason) => {
                        eprintln!("slides: image chooser failed: {reason}");
                        self.io_toast =
                            Some((tr("slides-toast-image-chooser-failed"), ToastKind::Error));
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

    /// Place the picked image on the current slide and kick off its background
    /// decode so the picture is ready by the time it paints.
    fn insert_image_from_path(&mut self, path: &std::path::Path) {
        let Some(src) = crate::assets::import_asset_file(&self.content, path) else {
            self.io_toast = Some((tr("slides-toast-image-import-failed"), ToastKind::Error));
            return;
        };
        // Start decoding now (off the UI thread) so the first paint already has
        // pixels, or a graceful loading/failed state if it can't be read.
        let _ = self.images.get(&src);
        // A comfortable default frame, centred on the slide; the user resizes.
        let el = Element::image(&src, 340.0, 168.0, 600.0, 384.0);
        let id = el.id.clone();
        let Some(slide_id) = self.slide().map(|slide| slide.id.clone()) else {
            return;
        };
        self.submit(&actions::add_element_action(self.cur_slide, slide_id, &el));
        self.sel = Some(id);
        self.editing_text = false;
    }

    fn duplicate_element(&mut self) {
        let Some(el) = self.selected_element().cloned() else {
            return;
        };
        let mut copy = el;
        copy.id = new_id();
        copy.x += 24.0;
        copy.y += 24.0;
        let id = copy.id.clone();
        let Some(slide_id) = self.slide().map(|slide| slide.id.clone()) else {
            return;
        };
        self.submit(&actions::add_element_action(
            self.cur_slide,
            slide_id,
            &copy,
        ));
        self.sel = Some(id);
    }

    fn delete_element(&mut self) {
        if let Some(id) = self.sel.clone() {
            self.submit(&actions::delete_element_action(self.cur_slide, id));
            self.sel = None;
            self.editing_text = false;
        }
    }

    fn nudge(&self, dx: f64, dy: f64) {
        if let Some(el) = self.selected_element().cloned() {
            let nx = (el.x + dx).clamp(-el.w + 20.0, SLIDE_W - 20.0);
            let ny = (el.y + dy).clamp(-el.h + 20.0, SLIDE_H - 20.0);
            self.submit(&actions::move_element_action(self.cur_slide, el.id, nx, ny));
        }
    }

    fn next_slide(&mut self, delta: isize) {
        let n = self.deck.slides.len() as isize;
        if n == 0 {
            return;
        }
        let mut i = self.cur_slide as isize + delta;
        i = i.clamp(0, n - 1);
        let from = self.cur_slide;
        if i as usize != from {
            // Cross-fade only in present mode; edit/overview jumps are instant.
            if self.mode == Mode::Present {
                self.presentation.start_transition(from);
            }
            self.cur_slide = i as usize;
        }
        self.sel = None;
        self.editing_text = false;
    }

    /// Jump straight to a slide by index (clamped). In present mode this is
    /// the Home/End and overview-click path; it still cross-fades.
    fn goto_slide(&mut self, idx: usize) {
        let n = self.deck.slides.len();
        if n == 0 {
            return;
        }
        let target = idx.min(n - 1);
        let from = self.cur_slide;
        if target != from {
            if self.mode == Mode::Present {
                self.presentation.start_transition(from);
            }
            self.cur_slide = target;
        }
        self.sel = None;
        self.editing_text = false;
    }

    /// Enter full-screen present mode, (re)starting the speaker timer.
    fn enter_present(&mut self) {
        self.mode = Mode::Present;
        self.editing_text = false;
        self.editing_notes = false;
        self.presentation.start();
    }

    /// Leave present mode back to the editor, clearing transient present
    /// state (blank, presenter dashboard) but keeping the elapsed timer so a
    /// re-entry via `P` resumes the same talk unless explicitly reset.
    fn exit_present(&mut self) {
        self.mode = Mode::Edit;
        self.presentation.leave();
    }

    // --- fullscreen + multi-monitor seam -----------------------------------

    /// Whether the app currently wants the window promoted to a true
    /// *borderless OS fullscreen* surface — i.e. present mode.
    ///
    /// This drives the shared [`ui::fullscreen`] mechanism: `ui::App` polls the
    /// root's [`FullscreenProvider::fullscreen_intent`] each frame and, via
    /// [`FullscreenProvider::fullscreen_params`] (here: the largest output, a distinct
    /// presentation view), enumerates the live monitors and `set_fullscreen`s
    /// onto the chosen one — the same path the video player uses for immersive
    /// fullscreen. On the Tilo compositor (which spans one combined scene) that
    /// becomes a full-output `xdg_toplevel.set_fullscreen`; the present view
    /// itself is the mode-`Present` draw (no toolbar/rail/inspector, black
    /// backdrop, letterboxed slide).
    ///
    /// Exposed publicly so either consumer can read it without reaching into
    /// the widget's private state.
    pub fn wants_fullscreen(&self) -> bool {
        self.mode == Mode::Present
    }

    /// The *audience* surface description for a multi-output present: the
    /// clean slide (no presenter chrome), letterboxed on a black field.
    ///
    /// Multi-monitor presenter mode (the design intent):
    ///   * The compositor enumerates connected outputs. With two outputs, it
    ///     fullscreens the clean **audience** presentation on the **larger**
    ///     output, and paints the presentation's **presenter dashboard** on
    ///     the other.
    ///   * Both surfaces share one CRDT deck + one `cur_slide`, so advancing
    ///     on either output is reflected on both instantly (the model is
    ///     already per-deck, with `cur_slide` per-peer view state).
    ///
    /// `ui::App` turns this into a real second winit toplevel when the session
    /// exposes a monitor distinct from this window. The primary window then
    /// becomes this dashboard; a one-output session keeps the regular
    /// fullscreen audience fallback. Both views are pure projections of the
    /// same deck + `cur_slide` and therefore stay in lock-step.
    pub fn present_output_plan(&self) -> (PresentSurface, PresentSurface) {
        (PresentSurface::Audience, PresentSurface::Presenter)
    }

    // --- text editing -------------------------------------------------------

    /// Start renaming the deck title inline via the shared [`TextInput`] (caret
    /// at the end, ready for full selection editing).
    fn begin_title_edit(&mut self) {
        self.editing_title = true;
        self.editing_text = false;
        self.editing_notes = false;
        self.sel = None;
        self.title_input.set_text(&self.deck.title);
        // `set_text` clamps but doesn't advance the caret, so park it at the end.
        self.title_input.set_cursor_pos(self.deck.title.len());
        Widget::set_focus(&mut self.title_input, true);
    }

    /// Commit the inline title rename: flush any pending edit to the CRDT and
    /// blur the field.
    fn commit_title_edit(&mut self) {
        if let Some(t) = self.title_input.take_dirty() {
            self.submit(&crate::actions::set_deck_title_action(t));
        }
        self.editing_title = false;
        Widget::set_focus(&mut self.title_input, false);
    }

    /// Show the current slide's speaker notes in the notes editor, edited
    /// through the deck's dispatcher as that slide's notes. Moving to
    /// another slide rebinds it; otherwise a collaborator's edit folds in
    /// under the live caret.
    fn sync_notes(&mut self, doc: &mut AutoCommit) {
        let Some(slide_id) = self.slide().map(|slide| slide.id.clone()) else {
            self.end_notes_edit();
            return;
        };
        let rebound = *self.notes_slide.borrow() != slide_id;
        if rebound {
            let (id, wrap) = actions::notes_child(&slide_id);
            *self.notes_slide.borrow_mut() = slide_id;
            self.notes.attach(
                self.dispatch
                    .child::<ui::tilo_resource::PlainText>(&id, wrap),
            );
        }
        self.notes.pull(doc);
        if std::mem::take(&mut self.notes_caret_to_end) || rebound {
            let control = self.notes.control_mut();
            let end = control.value().len();
            control.set_cursor_pos(end);
        }
        Widget::set_focus(self.notes.control_mut(), true);
    }

    fn begin_notes_edit(&mut self) {
        self.editing_notes = true;
        self.editing_text = false;
        self.sel = None;
        self.notes_caret_to_end = true;
    }

    fn end_notes_edit(&mut self) {
        self.editing_notes = false;
        Widget::set_focus(self.notes.control_mut(), false);
    }

    /// The rect the inline title editor occupies in the toolbar.
    #[allow(
        clippy::unused_self,
        reason = "keeps the ergonomic self.title_input_rect(...) call site consistent with the \
                  other chrome geometry helpers; converting to an associated function would \
                  read worse at every call site for no benefit"
    )]
    fn title_input_rect(&self, chrome: &Chrome) -> Rect {
        // The editor wraps the title's measured line box with `s1` above and
        // below, so its glyphs land exactly where the static label was.
        let m = &self.chrome_metrics;
        Rect::new(
            chrome.title_pos.x,
            chrome.title_pos.y - m.s1,
            chrome.title_rect.x1,
            chrome.title_pos.y + m.lh_title + m.s1,
        )
    }

    /// Route an event to the inline title [`TextInput`] while renaming. Returns
    /// `true` if the event was consumed. Enter / Esc commit; a press outside the
    /// field commits and falls through so the click still acts.
    fn route_title_edit(&mut self, routed: &mut RoutedEvent<'_>) -> bool {
        if !self.editing_title {
            return false;
        }
        let chrome = self.compute_chrome();
        let rect = self.title_input_rect(&chrome);
        self.title_input.apply_layout_external(rect);

        match routed.event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(
                    position.x / routed.scale_factor,
                    position.y / routed.scale_factor,
                );
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Enter | NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.commit_title_edit();
                return true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !rect.inflate(4.0, 8.0).contains(self.last_mouse) => {
                // Click away commits, then falls through so the click still acts.
                self.commit_title_edit();
                return false;
            }
            _ => {}
        }
        let mut h = false;
        self.title_input.handle_event(
            routed.event,
            &mut ui::widget::EventCtx {
                scale_factor: routed.scale_factor,
                modifiers: &routed.modifiers,
                state: routed.state,
                path: routed.path,
                handled: &mut h,
                focus_target: routed.focus_target,
                node_id: routed.node_id,
                taffy: routed.taffy,
            },
        );
        // The rename is one edit, submitted when it is committed.
        true
    }

    /// Route the inspector notes box through its notes editor, whose every
    /// edit is a splice of the slide's notes through the deck's dispatcher.
    /// Escape or a press outside ends editing; all editor events otherwise
    /// stay in the shared caret/selection/IME path.
    fn route_notes_edit(&mut self, routed: &mut RoutedEvent<'_>) -> bool {
        if !self.editing_notes {
            return false;
        }
        let Some(rect) = self.compute_chrome().notes_box else {
            self.end_notes_edit();
            return false;
        };
        self.notes.control_mut().apply_layout_external(rect);
        match routed.event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(
                    position.x / routed.scale_factor,
                    position.y / routed.scale_factor,
                );
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.end_notes_edit();
                return true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left | MouseButton::Right,
                ..
            } if !rect.contains(self.last_mouse) => {
                self.end_notes_edit();
                return false;
            }
            _ => {}
        }
        let mut input_handled = false;
        self.notes.handle_event(
            routed.event,
            &mut ui::widget::EventCtx {
                scale_factor: routed.scale_factor,
                modifiers: &routed.modifiers,
                state: routed.state,
                path: routed.path,
                handled: &mut input_handled,
                focus_target: routed.focus_target,
                node_id: routed.node_id,
                taffy: routed.taffy,
            },
        );
        input_handled
    }

    // --- drawing ------------------------------------------------------------

    /// Whether a chrome point (a button/label centre) falls inside the
    /// rail panel — geometric: below the toolbar, within the panel's
    /// (possibly off-screen / sliding) x-span.
    fn in_rail(chrome: &Chrome, center: Point) -> bool {
        center.y >= chrome.rail.y0
            && center.x >= chrome.rail.x0 - 0.5
            && center.x <= chrome.rail.x1 + 0.5
    }

    /// Same as [`Self::in_rail`], for the inspector panel.
    fn in_inspector(chrome: &Chrome, center: Point) -> bool {
        center.y >= chrome.inspector.y0
            && center.x >= chrome.inspector.x0 - 0.5
            && center.x <= chrome.inspector.x1 + 0.5
    }

    /// Whether a chrome point falls inside either side panel — content
    /// here is deferred to a final pass so an open overlay flap paints on
    /// top of the dimming scrim.
    fn in_panel(chrome: &Chrome, center: Point) -> bool {
        Self::in_rail(chrome, center) || Self::in_inspector(chrome, center)
    }

    fn draw_editor(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
    ) {
        let pal = self.palette(theme);
        let c = &theme.tokens.colors;
        let rail_overlay = chrome.layout == SplitLayout::Overlay;
        let insp_overlay = chrome.inspector_layout == SplitLayout::Overlay;
        // Panel dividers are the raised panels' own derived edge (HIG §9.5);
        // the declared `border_standard` is within a shade of dark `raised`.
        let div = ui::surface::edge_on(c.surfaces.raised);

        let chrome_text_scale = tr.text_scale();
        tr.set_text_scale(1.0);

        {
            let mut paint = PaintCtx {
                scene,
                transform: t,
                text: tr,
                theme,
            };
            self.draw_editor_panels_base(&mut paint, chrome, rail_overlay, insp_overlay, div);
        }
        self.draw_editor_canvas(scene, t, tr, theme, chrome, &pal);
        self.draw_text_editor(scene, t, tr, theme, chrome, &pal);
        tr.set_text_scale(chrome_text_scale);
        self.draw_editor_base_chrome(scene, t, tr, theme, chrome, &pal);

        // Inline title rename: an accent underline reads the toolbar title as an
        // editable field; the shared TextInput (drawn in `Widget::draw`) paints
        // the text, caret and selection.
        if self.editing_title {
            let underline_y = chrome.title_pos.y + 22.0;
            stroke_line(
                scene,
                t,
                chrome.title_pos.x,
                underline_y,
                chrome.title_rect.x1,
                underline_y,
                pal.accent,
            );
        }

        // Scrim behind an open overlay flap, dimming the editor body.
        if chrome.scrim.width() > 0.0 {
            let reveal = chrome.reveal.max(chrome.inspector_reveal);
            let alpha = (reveal * 0.5) as f32;
            ui::chrome::fill_scrim(scene, t, chrome.scrim, alpha, theme);
        }

        {
            let mut paint = PaintCtx {
                scene,
                transform: t,
                text: tr,
                theme,
            };
            self.draw_editor_rail(&mut paint, chrome, &pal, rail_overlay, div);
            self.draw_editor_inspector_layer(&mut paint, chrome, &pal, insp_overlay, div);
        }

        // Toolbar overflow menu — painted last so it floats above all chrome.
        ToolbarTools::draw(scene, t, theme, &chrome.toolbar_tools, |scene, button| {
            self.draw_button(scene, t, tr, theme, &pal, button);
        });
    }

    /// Base-layer panel fills + divider lines (toolbar, rail, inspector).
    /// A docked panel is part of this base layer; an overlay flap is
    /// painted later, on top of the dimming scrim.
    fn draw_editor_panels_base(
        &self,
        paint: &mut PaintCtx<'_>,
        chrome: &Chrome,
        rail_overlay: bool,
        insp_overlay: bool,
        div: Color,
    ) {
        let c = &paint.theme.tokens.colors;
        // Panels (base layer). A docked panel is part of the base; an overlay
        // flap is painted later, on top of the dimming scrim.
        chrome::fill_rect(paint.scene, paint.transform, self.rect, c.surfaces.base);
        if !rail_overlay {
            chrome::fill_rect(paint.scene, paint.transform, chrome.rail, c.surfaces.raised);
        }
        if !insp_overlay {
            chrome::fill_rect(
                paint.scene,
                paint.transform,
                chrome.inspector,
                c.surfaces.raised,
            );
        }
        let toolbar_fill = mix_panel(c.surfaces.raised, c.surfaces.base);
        chrome::fill_rect(paint.scene, paint.transform, chrome.toolbar, toolbar_fill);
        // Divider lines: each panel's derived edge on its own last pixel
        // row / column, crisp (a stroke centred on the boundary blurred
        // across two pixels).
        chrome::hairline_h(
            paint.scene,
            paint.transform,
            chrome.toolbar.x0,
            chrome.toolbar.x1,
            chrome.toolbar.y1 - 1.0,
            ui::surface::edge_on(toolbar_fill),
        );
        if !rail_overlay {
            chrome::hairline_v(
                paint.scene,
                paint.transform,
                chrome.rail.x1 - 1.0,
                chrome.rail.y0,
                chrome.rail.y1,
                div,
            );
        }
        if !insp_overlay {
            chrome::hairline_v(
                paint.scene,
                paint.transform,
                chrome.inspector.x0,
                chrome.inspector.y0,
                chrome.inspector.y1,
                div,
            );
        }
    }

    /// The canvas slide itself: drop shadow, the rendered slide, its
    /// border, the selection ring/handles or live text caret+selection, the
    /// snap guides, and the caption underneath. No-op while the deck has no
    /// current slide.
    fn draw_editor_canvas(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
        pal: &Palette,
    ) {
        let Some(slide) = self.slide() else { return };
        let c = &theme.tokens.colors;
        // Drop shadow from the theme elevation ladder.
        ui::surface::shadow(
            scene,
            t,
            chrome.slide_dest,
            14.0,
            &theme.tokens.elevation.e3,
        );
        // The text box being edited draws its words through its editor.
        let editing = self
            .text_editor
            .as_ref()
            .filter(|_| self.editing_text)
            .map(|editor| editor.element_id.as_str());
        render::draw_slide_except(
            &mut PaintCtx::new(scene, t, tr, theme),
            DeckAssets {
                palette: pal,
                images: &self.images,
            },
            slide,
            chrome.slide_dest,
            14.0,
            editing,
        );
        chrome::stroke_round(
            scene,
            t,
            chrome.slide_dest,
            14.0,
            render::with_alpha(c.text_primary, 28),
            1.0,
        );

        self.draw_editor_selection_chrome(scene, t, chrome, pal);
        self.draw_editor_guides(scene, t, chrome, pal);

        // Caption under the slide.
        let cap = format!("{}  ·  {}", slide.layout.label(), slide.background.label());
        tr.draw_text_weighted(
            scene,
            &cap,
            12.0,
            c.text_dim,
            t * Affine::translate((chrome.slide_dest.x0, chrome.slide_dest.y1 + 10.0)),
            None,
            TextWeight::Regular,
        );
    }

    /// The selection ring + resize handles around the selected element, or
    /// (while editing) the live text selection + caret. No-op with nothing
    /// selected.
    fn draw_editor_selection_chrome(
        &self,
        scene: &mut Scene,
        t: Affine,
        chrome: &Chrome,
        pal: &Palette,
    ) {
        let Some(el) = self.selected_element() else {
            return;
        };
        let er = render::element_dest(chrome.slide_dest, chrome.scale, el);
        let sel_col = pal.accent;
        // Ring is drawn at the handle outset so it passes through the
        // handle centres — the whole selection frame sits *around* the
        // content, never over the first glyph.
        chrome::stroke_round(
            scene,
            t,
            er.inflate(HANDLE_OFFSET, HANDLE_OFFSET),
            5.0,
            sel_col,
            1.5,
        );
        // The text box being edited draws its own caret and selection.
        if !self.editing_text {
            for (_, hr) in self.handles_of(chrome.slide_dest, chrome.scale, el) {
                // The returned rect is the generous grab zone (±HANDLE);
                // paint a smaller visual pip centred on it so the handle
                // clears the content edge on every side.
                let vis = hr.inflate(-4.0, -4.0);
                chrome::fill_round(scene, t, vis, 2.5, Color::WHITE);
                chrome::stroke_round(scene, t, vis, 2.5, sel_col, 1.5);
            }
        }
    }

    /// Live alignment guides (only while a drag is snapping).
    fn draw_editor_guides(&self, scene: &mut Scene, t: Affine, chrome: &Chrome, pal: &Palette) {
        for g in &self.active_guides {
            let gc = render::with_alpha(pal.accent, 220);
            match g {
                Guide::Vertical(sx) => {
                    let x = chrome.slide_dest.x0 + sx * chrome.scale;
                    stroke_line(
                        scene,
                        t,
                        x,
                        chrome.slide_dest.y0,
                        x,
                        chrome.slide_dest.y1,
                        gc,
                    );
                }
                Guide::Horizontal(sy) => {
                    let y = chrome.slide_dest.y0 + sy * chrome.scale;
                    stroke_line(
                        scene,
                        t,
                        chrome.slide_dest.x0,
                        y,
                        chrome.slide_dest.x1,
                        y,
                        gc,
                    );
                }
            }
        }
    }

    /// Base-layer buttons + labels: everything NOT inside the rail or
    /// inspector panel (their content is deferred to its own layer so an
    /// open overlay flap paints on top of the dimming scrim).
    fn draw_editor_base_chrome(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
        pal: &Palette,
    ) {
        // Content that lives inside a side panel is deferred to a final pass so
        // an open overlay flap paints on top of the dimming scrim. Membership is
        // geometric: below the toolbar, within the panel's (possibly off-screen
        // / sliding) x-span.
        // Base-layer buttons + labels (toolbar chrome only; panel content is
        // deferred below).
        for b in &chrome.buttons {
            if !Self::in_panel(chrome, b.rect.center()) {
                self.draw_button(scene, t, tr, theme, pal, b);
            }
        }
        for l in &chrome.labels {
            if !Self::in_panel(chrome, Point::new(l.pos.x, l.pos.y)) {
                self.draw_label(scene, t, tr, theme, pal, l);
            }
        }
    }

    /// Rail group on top: panel fill (overlay only), then the fixed header
    /// and action buttons, then the scrolling thumbnail list clipped to its
    /// viewport so it can never bleed over the pinned Add/Dup/Del row.
    fn draw_editor_rail(
        &mut self,
        paint: &mut PaintCtx<'_>,
        chrome: &Chrome,
        pal: &Palette,
        rail_overlay: bool,
        div: Color,
    ) {
        let c = &paint.theme.tokens.colors;
        // Rail group on top: panel fill (overlay only), then the fixed header
        // + action buttons, then the scrolling thumbnail list clipped to its
        // viewport so it can never bleed over the pinned Add/Dup/Del row.
        if rail_overlay && chrome.reveal > 0.001 {
            chrome::fill_rect(paint.scene, paint.transform, chrome.rail, c.surfaces.raised);
            chrome::hairline_v(
                paint.scene,
                paint.transform,
                chrome.rail.x1 - 1.0,
                chrome.rail.y0,
                chrome.rail.y1,
                div,
            );
        }
        // Fixed rail chrome (SLIDES header + Add/Dup/Del) — never scrolls. While
        // a thumbnail is being dragged, the pinned action row is replaced by the
        // red "drop to delete" zone drawn below, so suppress the buttons it
        // covers (otherwise they bleed through the zone's translucent fill).
        let drag_zone = self
            .page_sidebar
            .is_dragging()
            .then(|| self.slide_delete_zone(chrome.rail));
        for b in &chrome.buttons {
            if Self::in_rail(chrome, b.rect.center())
                && !drag_zone.is_some_and(|z| z.contains(b.rect.center()))
            {
                self.draw_button(
                    paint.scene,
                    paint.transform,
                    paint.text,
                    paint.theme,
                    pal,
                    b,
                );
            }
        }
        for l in &chrome.labels {
            if Self::in_rail(chrome, Point::new(l.pos.x, l.pos.y)) {
                self.draw_label(
                    paint.scene,
                    paint.transform,
                    paint.text,
                    paint.theme,
                    pal,
                    l,
                );
            }
        }
        self.draw_editor_rail_thumbnails(
            paint.scene,
            paint.transform,
            paint.text,
            paint.theme,
            chrome,
            pal,
        );
    }

    /// Scrolling thumbnail list — the shared `PageSidebar` paints the
    /// thumbnails, selection ring, index numbers, the live reorder ghost +
    /// reflow gap, its scrollbar and its own right-click menu, all clipped to
    /// the rail viewport it was given in `begin_frame`. It renders each slide
    /// through the closure; slides keeps the panel chrome (fill + SLIDES
    /// header + Add/Dup/Del) around it. Move the rail out of `self` so the
    /// closure can borrow the deck + image cache alongside it. No-op below a
    /// 1px-wide rail (fully collapsed).
    fn draw_editor_rail_thumbnails(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
        pal: &Palette,
    ) {
        if chrome.rail.width() <= 1.0 {
            return;
        }
        let mut sb = std::mem::replace(&mut self.page_sidebar, PageSidebar::new(SLIDE_H / SLIDE_W));
        let deck = &self.deck;
        let images = &self.images;
        sb.render(scene, t, tr, theme, |scene, tr, base, i, rect| {
            if let Some(slide) = deck.slides.get(i) {
                render::draw_slide(
                    &mut PaintCtx::new(scene, base, tr, theme),
                    DeckAssets {
                        palette: pal,
                        images,
                    },
                    slide,
                    rect,
                    6.0,
                );
            }
        });
        self.page_sidebar = sb;

        if self.page_sidebar.is_dragging() {
            self.draw_editor_delete_zone(scene, t, tr, theme, chrome);
        }
    }

    /// Drop-to-delete zone: a red dashed target pinned to the bottom of the
    /// rail, revealed only while a slide is being dragged. Dropping a
    /// thumbnail here removes that slide.
    fn draw_editor_delete_zone(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
    ) {
        let zone = self.slide_delete_zone(chrome.rail);
        let armed = self.over_delete_zone();
        let danger = theme.tokens.colors.semantic.danger;
        if armed {
            chrome::fill_round(scene, t, zone, 10.0, render::with_alpha(danger, 40));
        }
        scene.stroke(
            &Stroke::new(if armed { 2.0 } else { 1.5 }).with_dashes(0.0, [6.0, 4.0]),
            t,
            danger,
            None,
            &vello::kurbo::RoundedRect::from_rect(zone, 10.0),
        );
        // Icon + label cluster centred on the zone, both axes measured with
        // the style the label is painted in.
        let m = &self.chrome_metrics;
        let icon_sz = TOOLBAR_ICON;
        let label = crate::strings::tr("slides-drop-to-delete");
        let font = 12.5;
        let (lw, lh) = tr.measure_weighted(&label, font, None, TextWeight::Medium);
        let total = icon_sz + m.s2 + f64::from(lw);
        let cx = zone.x0 + (zone.width() - total) / 2.0;
        let cy = zone.y0 + zone.height() / 2.0;
        draw_icon(
            scene,
            t,
            Rect::new(cx, cy - icon_sz / 2.0, cx + icon_sz, cy + icon_sz / 2.0),
            Icon::Trash,
            danger,
            1.8,
        );
        tr.draw_text_weighted(
            scene,
            &label,
            font,
            danger,
            t * Affine::translate((cx + icon_sz + m.s2, cy - f64::from(lh) / 2.0)),
            None,
            TextWeight::Medium,
        );
    }

    /// Inspector group on top: panel fill (overlay only), buttons, notes,
    /// labels.
    fn draw_editor_inspector_layer(
        &mut self,
        paint: &mut PaintCtx<'_>,
        chrome: &Chrome,
        pal: &Palette,
        insp_overlay: bool,
        div: Color,
    ) {
        let c = &paint.theme.tokens.colors;
        // Inspector group on top: panel fill (overlay only), buttons, notes, labels.
        if insp_overlay && chrome.inspector_reveal > 0.001 {
            chrome::fill_rect(
                paint.scene,
                paint.transform,
                chrome.inspector,
                c.surfaces.raised,
            );
            chrome::hairline_v(
                paint.scene,
                paint.transform,
                chrome.inspector.x0,
                chrome.inspector.y0,
                chrome.inspector.y1,
                div,
            );
        }
        // The content is clipped to the panel: a scrolled panel's first and
        // last rows are cut at its edges instead of bleeding into the toolbar
        // or under the window edge.
        paint.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            paint.transform,
            &chrome.inspector,
        );
        for b in &chrome.buttons {
            if Self::in_inspector(chrome, b.rect.center()) {
                self.draw_button(
                    paint.scene,
                    paint.transform,
                    paint.text,
                    paint.theme,
                    pal,
                    b,
                );
            }
        }
        if let (Some(nb), Some(notes)) = (
            chrome.notes_box,
            self.slide().map(|slide| slide.notes.clone()),
        ) {
            self.draw_notes(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                nb,
                &notes,
            );
        }
        for l in &chrome.labels {
            if Self::in_inspector(chrome, Point::new(l.pos.x, l.pos.y)) {
                self.draw_label(
                    paint.scene,
                    paint.transform,
                    paint.text,
                    paint.theme,
                    pal,
                    l,
                );
            }
        }
        paint.scene.pop_layer();
        if chrome.inspector.width() > 1.0 {
            self.inspector_scroll
                .draw_bar_overlay(paint.scene, paint.transform, paint.theme);
        }
    }

    /// Position the inspector's embedded `ui` widgets (font-size field, fill
    /// picker) from `chrome` so both `draw` and `handle_event` hit-test them.
    fn position_inspector_widgets(&mut self, chrome: &Chrome) {
        if let Some(rect) = chrome.font_input_rect {
            // Reflect the element's size unless the user is mid-edit (so typing
            // isn't clobbered by the CRDT mirror).
            if !self.font_input.is_editing() {
                let fs = self.selected_element().map_or(40.0, |e| e.font_size);
                self.font_input.set_value(fs);
            }
            self.font_input.apply_layout_external(rect);
        } else {
            // No slot this frame (nothing selected, or the TYPE row scrolled
            // out of the panel): park the field so its last rect cannot keep
            // catching presses meant for whatever is painted there now.
            self.font_input.apply_layout_external(Rect::ZERO);
        }
        if let Some(rect) = chrome.fill_picker_rect {
            self.fill_picker.apply_layout_external(rect);
        }
    }

    /// Draw the inspector's embedded widgets on top of the editor chrome: the
    /// font-size [`NumberInput`] and, when open, the fill [`ColorPicker`]
    /// popover (with its own panel + shadow).
    fn draw_inspector_widgets(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        chrome: &Chrome,
    ) {
        self.position_inspector_widgets(chrome);
        if chrome.font_input_rect.is_some() {
            // Clipped like the rest of the panel content: the field scrolls
            // with its TYPE row rather than floating over the toolbar.
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                t,
                &chrome.inspector,
            );
            self.font_input.draw(scene, t, tr, theme, presence);
            scene.pop_layer();
        }
        if let Some(rect) = chrome.fill_picker_rect {
            // Seed the picker with the element's current fill on the first frame
            // it's open (now that we hold the theme to resolve a colour role).
            if self.fill_picker_needs_init {
                let mut pal = self.palette(theme);
                if let Some(slide) = self.slide() {
                    pal = pal.on(slide.background);
                }
                let col = self
                    .selected_element()
                    .and_then(|e| pal.resolve_role(&e.fill))
                    .unwrap_or(pal.accent);
                self.fill_picker.set_color(col);
                self.fill_picker_needs_init = false;
            }
            let c = &theme.tokens.colors;
            let panel = self.fill_picker_panel_rect(rect);
            chrome::panel(scene, t, theme, panel, f64::from(theme.tokens.radii.lg));
            tr.draw_text_weighted(
                scene,
                &crate::strings::tr("slides-fill-colour"),
                FONT_CAPTION,
                c.text_dim,
                t * Affine::translate((rect.x0, panel.y0 + self.chrome_metrics.s2)),
                None,
                TextWeight::Semibold,
            );
            self.fill_picker.draw(scene, t, tr, theme, presence);
        }
    }

    /// Draw the inline title [`TextInput`] over the toolbar while renaming.
    fn draw_title_input(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        chrome: &Chrome,
    ) {
        let rect = self.title_input_rect(chrome);
        self.title_input.apply_layout_external(rect);
        self.title_input.draw(scene, t, tr, theme, presence);
    }

    fn draw_label(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pal: &Palette,
        l: &Label,
    ) {
        let col = self.label_color(l.color, theme, pal);
        // Elide with the renderer's own metrics, so the ellipsis lands where
        // the glyphs really end rather than where a per-glyph estimate did.
        let text = match l.max_w {
            Some(max_w) if max_w <= 0.0 => String::new(),
            Some(max_w) => tr.elide_to_width_weighted(&l.text, l.font, max_w as f32, l.weight),
            None => l.text.clone(),
        };
        tr.draw_text_weighted(
            scene,
            &text,
            l.font,
            col,
            t * Affine::translate((l.pos.x, l.pos.y)),
            None,
            l.weight,
        );
    }

    #[allow(
        clippy::unused_self,
        reason = "keeps the ergonomic self.label_color(...) call site consistent with the \
                  other chrome draw helpers; converting to an associated function would read \
                  worse at every call site for no benefit"
    )]
    fn label_color(&self, lc: LabelColor, theme: &Theme, _pal: &Palette) -> Color {
        match lc {
            LabelColor::Primary => theme.tokens.colors.text_primary,
            LabelColor::Dim => theme.tokens.colors.text_dim,
        }
    }

    fn draw_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pal: &Palette,
        b: &UiButton,
    ) {
        let c = &theme.tokens.colors;
        let mut paint = PaintCtx {
            scene,
            transform: t,
            text: tr,
            theme,
        };
        match &b.kind {
            ButtonKind::Text {
                label,
                active,
                danger,
            } => Self::draw_button_text(&mut paint, pal, b.rect, label, *active, *danger),
            ButtonKind::IconText {
                icon,
                label,
                accent,
            } => Self::draw_button_icon_text(&mut paint, pal, b.rect, *icon, label, *accent),
            ButtonKind::IconOnly { icon, active } => {
                Self::draw_button_icon_only(
                    paint.scene,
                    paint.transform,
                    c,
                    pal,
                    b.rect,
                    *icon,
                    *active,
                );
            }
            ButtonKind::Swatch { role, selected } => {
                Self::draw_button_swatch(
                    paint.scene,
                    paint.transform,
                    c,
                    pal,
                    b.rect,
                    role,
                    *selected,
                );
            }
            ButtonKind::Palette { kind, selected } => {
                Self::draw_button_palette(&mut paint, b.rect, *kind, *selected);
            }
            ButtonKind::Segment { label, active, pos } => {
                Self::draw_button_segment(&mut paint, pal, b.rect, label, *active, *pos);
            }
            ButtonKind::Hamburger => {
                self.draw_button_hamburger(paint.scene, paint.transform, c, pal, b.rect);
            }
        }
    }

    fn draw_button_text(
        paint: &mut PaintCtx<'_>,
        pal: &Palette,
        rect: Rect,
        label: &str,
        active: bool,
        danger: bool,
    ) {
        let c = &paint.theme.tokens.colors;
        // Rest: the base face with its derived edge (HIG §9.5 "Button
        // Neutral"); active: the accent fill alone; danger: the tint
        // composited over the face first, so its edge and ink are measured
        // against what is on screen.
        let fg = if danger {
            let chip = c.tinted_chip_on(
                c.surfaces.base,
                c.semantic.danger,
                DANGER_WASH,
                ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
            );
            ui::surface::card(paint.scene, paint.transform, rect, 8.0, chip.fill);
            chip.ink
        } else if active {
            chrome::fill_round(paint.scene, paint.transform, rect, 8.0, pal.accent);
            on_accent(c, pal)
        } else {
            ui::surface::card(paint.scene, paint.transform, rect, 8.0, c.surfaces.base);
            c.text_primary
        };
        if !label.is_empty() {
            chrome::text_centered(
                paint,
                label,
                rect,
                &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
                fg,
            );
        }
    }

    fn draw_button_icon_text(
        paint: &mut PaintCtx<'_>,
        pal: &Palette,
        rect: Rect,
        icon: Icon,
        label: &str,
        accent: bool,
    ) {
        let c = &paint.theme.tokens.colors;
        let fg = if accent {
            chrome::fill_round(paint.scene, paint.transform, rect, 8.0, pal.accent);
            on_accent(c, pal)
        } else {
            ui::surface::card(paint.scene, paint.transform, rect, 8.0, c.surfaces.base);
            c.text_primary
        };
        let ir = Rect::new(rect.x0 + 11.0, rect.y0 + 8.0, rect.x0 + 29.0, rect.y1 - 8.0);
        draw_icon(paint.scene, paint.transform, ir, icon, fg, 1.8);
        chrome::text_vcenter(
            paint,
            label,
            Rect {
                x0: ir.x1 + 5.0,
                ..rect
            },
            &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
            fg,
        );
    }

    fn draw_button_icon_only(
        scene: &mut Scene,
        t: Affine,
        c: &ui::tokens::Colors,
        pal: &Palette,
        rect: Rect,
        icon: Icon,
        active: bool,
    ) {
        let fg = if active {
            chrome::fill_round(scene, t, rect, 7.0, pal.accent);
            on_accent(c, pal)
        } else {
            ui::surface::card(scene, t, rect, 7.0, c.surfaces.base);
            c.text_primary
        };
        let ir = rect.inflate(-6.0, -6.0);
        draw_icon(scene, t, ir, icon, fg, 1.8);
    }

    fn draw_button_swatch(
        scene: &mut Scene,
        t: Affine,
        c: &ui::tokens::Colors,
        pal: &Palette,
        rect: Rect,
        role: &str,
        selected: bool,
    ) {
        if let Some(col) = pal.resolve_role(role) {
            // The swatch's edge is derived from the swatch itself, so a
            // near-black and a near-white role both keep a visible frame.
            ui::surface::card(scene, t, rect, 6.0, col);
        } else {
            // "none" → hatched empty box
            ui::surface::card(scene, t, rect, 6.0, c.surfaces.base);
            stroke_line(
                scene,
                t,
                rect.x0 + 4.0,
                rect.y1 - 4.0,
                rect.x1 - 4.0,
                rect.y0 + 4.0,
                c.semantic.danger,
            );
        }
        if selected {
            chrome::stroke_round(scene, t, rect.inflate(2.5, 2.5), 8.0, pal.accent, 2.0);
        }
    }

    fn draw_button_palette(
        paint: &mut PaintCtx<'_>,
        rect: Rect,
        kind: PaletteKind,
        selected: bool,
    ) {
        let c = &paint.theme.tokens.colors;
        let col = Palette::resolve(kind, paint.theme).accent;
        ui::surface::card(paint.scene, paint.transform, rect, 8.0, c.surfaces.base);
        if selected {
            chrome::stroke_round(paint.scene, paint.transform, rect, 8.0, col, 2.0);
        }
        let dot = Rect::new(
            rect.x0 + 8.0,
            rect.center().y - 6.0,
            rect.x0 + 20.0,
            rect.center().y + 6.0,
        );
        chrome::fill_round(paint.scene, paint.transform, dot, 6.0, col);
        if rect.width() > 40.0 {
            // Clip the name to the pill so a slight text-metric over-run can
            // never bleed onto the next pill. Pills are sized (see
            // `palette_pill_width`) so the full name fits, making this a
            // belt-and-suspenders guard, not a truncation.
            paint.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                paint.transform,
                &rect,
            );
            chrome::text_vcenter(
                paint,
                kind.label(),
                Rect {
                    x0: dot.x1 + 6.0,
                    ..rect
                },
                &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
                c.text_primary,
            );
            paint.scene.pop_layer();
        }
    }

    fn draw_button_segment(
        paint: &mut PaintCtx<'_>,
        pal: &Palette,
        rect: Rect,
        label: &str,
        active: bool,
        pos: SegPos,
    ) {
        let c = &paint.theme.tokens.colors;
        let (bg, fg) = if active {
            (pal.accent, on_accent(c, pal))
        } else {
            (c.surfaces.base, c.text_primary)
        };
        // Round only the outer corners so the row reads as one pill.
        let r = 8.0;
        let shape_of = |rect: Rect, r: f64| match pos {
            SegPos::First => {
                vello::kurbo::RoundedRect::new(rect.x0, rect.y0, rect.x1, rect.y1, (r, 0.0, 0.0, r))
            }
            SegPos::Last => {
                vello::kurbo::RoundedRect::new(rect.x0, rect.y0, rect.x1, rect.y1, (0.0, r, r, 0.0))
            }
            SegPos::Mid => vello::kurbo::RoundedRect::from_rect(rect, 0.0),
        };
        paint
            .scene
            .fill(Fill::NonZero, paint.transform, bg, None, &shape_of(rect, r));
        // Each cell's edge derives from its own fill and sits half a pixel
        // inside it (the `ui::surface::card` geometry, per-corner radii).
        paint.scene.stroke(
            &Stroke::new(1.0),
            paint.transform,
            ui::surface::edge_on(bg),
            None,
            &shape_of(rect.inset(-0.5), r - 0.5),
        );
        chrome::text_centered(
            paint,
            label,
            rect,
            &TextStyle::new(12.5).with_weight(TextWeight::Semibold),
            fg,
        );
    }

    fn draw_button_hamburger(
        &self,
        scene: &mut Scene,
        t: Affine,
        c: &ui::tokens::Colors,
        pal: &Palette,
        rect: Rect,
    ) {
        let active = self.rail_open();
        let fg = if active {
            chrome::fill_round(scene, t, rect, 8.0, pal.accent);
            on_accent(c, pal)
        } else {
            ui::surface::card(scene, t, rect, 8.0, c.surfaces.base);
            c.text_primary
        };
        // Three evenly-spaced horizontal lines.
        let cx0 = rect.x0 + 9.0;
        let cx1 = rect.x1 - 9.0;
        let cy = rect.center().y;
        for dy in [-5.0, 0.0, 5.0] {
            stroke_line(scene, t, cx0, cy + dy, cx1, cy + dy, fg);
        }
    }

    fn draw_notes(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        nb: Rect,
        notes: &str,
    ) {
        let c = &theme.tokens.colors;
        // A text field: the base face with its derived rest edge; editing
        // lays the accent wash and ring over it (focus state, not an edge).
        ui::surface::card(scene, t, nb, 8.0, c.surfaces.base);
        if self.editing_notes {
            let accent = self.palette(theme).accent;
            chrome::fill_round(scene, t, nb, 8.0, render::with_alpha(accent, 28));
            scene.stroke(
                &Stroke::new(1.0),
                t,
                accent,
                None,
                &ui::surface::edge_outline(nb, 8.0),
            );
        }
        if self.editing_notes {
            self.notes.control_mut().apply_layout_external(nb);
            if self.notes.control().value().is_empty() {
                tr.draw_text_weighted(
                    scene,
                    &crate::strings::tr("slides-notes-placeholder"),
                    12.5,
                    c.text_dim,
                    t * Affine::translate((nb.x0 + 10.0, nb.y0 + 9.0)),
                    Some((nb.width() - 20.0) as f32),
                    TextWeight::Regular,
                );
            }
            self.notes
                .control_mut()
                .draw(scene, t, tr, theme, &PresenceCtx::empty());
        } else {
            let placeholder = crate::strings::tr("slides-notes-placeholder");
            let (display, col) = if notes.is_empty() {
                (placeholder.as_str(), c.text_dim)
            } else {
                (notes, c.text_primary)
            };
            tr.draw_text_weighted(
                scene,
                display,
                12.5,
                col,
                t * Affine::translate((nb.x0 + 10.0, nb.y0 + 9.0)),
                Some((nb.width() - 20.0) as f32),
                TextWeight::Regular,
            );
        }
    }

    fn draw_present(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let frame = PresentationFrame::new(
            self.rect,
            &self.deck,
            self.cur_slide,
            self.deck_rev,
            &self.images,
        );
        self.presentation.draw(scene, t, tr, theme, &frame);
    }

    // --- overview (slide-sorter) -------------------------------------------

    fn compute_overview(&self) -> (Rect, Vec<(usize, Rect)>, f64) {
        self.overview.geometry(self.rect, self.deck.slides.len())
    }

    fn draw_overview(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let palette = self.palette(theme);
        self.overview.draw(
            &mut PaintCtx::new(scene, t, tr, theme),
            self.rect,
            &self.deck,
            self.cur_slide,
            DeckAssets {
                palette: &palette,
                images: &self.images,
            },
        );
    }
}

/// Make `title` safe as a filename stem: keep alphanumerics, dash and
/// underscore, fold everything else to `_`, and never return empty.
fn sanitize_filename(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() {
        "presentation".into()
    } else {
        s
    }
}

/// Ensure `path` ends in `.odp` (the host Save dialog may drop the extension).
fn ensure_odp_ext(path: &std::path::Path) -> PathBuf {
    match path.extension() {
        Some(ext) if ext.eq_ignore_ascii_case("odp") => path.to_path_buf(),
        _ => {
            let mut p = path.as_os_str().to_os_string();
            p.push(".odp");
            PathBuf::from(p)
        }
    }
}

fn is_pptx(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pptx"))
}

fn mix_panel(a: Color, b: Color) -> Color {
    Color::from_rgba8(
        u16::midpoint(u16::from(a.r()), u16::from(b.r())) as u8,
        u16::midpoint(u16::from(a.g()), u16::from(b.g())) as u8,
        u16::midpoint(u16::from(a.b()), u16::from(b.b())) as u8,
        255,
    )
}

fn stroke_line(scene: &mut Scene, t: Affine, x0: f64, y0: f64, x1: f64, y1: f64, color: Color) {
    let line = Line::new(Point::new(x0, y0), Point::new(x1, y1));
    scene.stroke(&Stroke::new(1.0), t, color, None, &line);
}

/// Wash strength of a danger-tinted Neutral button (the Delete actions):
/// a faint tint over the button face, matched to the classic `danger/38`.
const DANGER_WASH: f32 = 0.15;

/// Ink on the deck palette's accent fill (active buttons): white where it
/// reads, stepped toward the contrasting extreme on a bright accent — one
/// answer per palette, so a row of active pills never mixes polarities.
fn on_accent(c: &ui::tokens::Colors, pal: &Palette) -> Color {
    c.ink_on_any(
        &[pal.accent],
        Color::WHITE,
        ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
    )
}

impl SlidesApp {
    fn semantic_target(&self, hit: &Hit) -> String {
        match hit {
            Hit::NewSlide => "slides.control.new-slide".into(),
            Hit::DuplicateSlide => "slides.control.duplicate-slide".into(),
            Hit::DeleteSlide => "slides.control.delete-slide".into(),
            Hit::Present => "slides.control.present".into(),
            Hit::Overview => "slides.control.overview".into(),
            Hit::OpenSlide(index) | Hit::SelectSlide(index) => {
                self.deck.slides.get(*index).map_or_else(
                    || format!("slides.slide-index:{index}"),
                    |slide| format!("slides.slide:{}", slide.id),
                )
            }
            Hit::CyclePalette => "slides.control.cycle-palette".into(),
            Hit::Insert(kind) => format!("slides.control.insert:{}", kind.as_str()),
            Hit::SetFill(role) => format!("slides.control.fill:{role}"),
            Hit::SetStroke(role) => format!("slides.control.stroke:{role}"),
            Hit::OpenFillPicker => "slides.control.custom-fill".into(),
            Hit::ToggleBold => "slides.control.bold".into(),
            Hit::ToggleItalic => "slides.control.italic".into(),
            Hit::SetAlign(align) => format!("slides.control.align:{}", align.as_str()),
            Hit::BringFront => "slides.control.bring-front".into(),
            Hit::SendBack => "slides.control.send-back".into(),
            Hit::DuplicateElement => "slides.control.duplicate-element".into(),
            Hit::DeleteElement => "slides.control.delete-element".into(),
            Hit::SetLayout(layout) => format!("slides.control.layout:{}", layout.as_str()),
            Hit::SetBackground(background) => {
                format!("slides.control.background:{}", background.as_str())
            }
            Hit::SetPalette(palette) => format!("slides.control.palette:{}", palette.as_str()),
            Hit::EditNotes => "slides.control.edit-notes".into(),
            Hit::ToggleRail => "slides.control.toggle-slides".into(),
            Hit::ToggleInspector => "slides.control.toggle-properties".into(),
            Hit::ToggleToolbarMenu => "slides.control.more-tools".into(),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive semantic label/state mapping must stay visibly parallel to the Hit enum"
    )]
    fn semantic_copy(&self, hit: &Hit) -> (ui::a11y::Role, String, ui::a11y::AccessState) {
        use ui::a11y::{AccessState, Role};
        let selected = self.selected_element();
        match hit {
            Hit::NewSlide => (
                Role::Button,
                tr("slides-a11y-new-slide"),
                AccessState::new(),
            ),
            Hit::DuplicateSlide => (
                Role::Button,
                tr("slides-a11y-duplicate-slide"),
                AccessState::new(),
            ),
            Hit::DeleteSlide => (
                Role::Button,
                tr("slides-a11y-delete-slide"),
                AccessState::new().disabled(self.deck.slides.len() <= 1),
            ),
            Hit::Present => (
                Role::Button,
                tr("slides-a11y-start-presentation"),
                AccessState::new(),
            ),
            Hit::Overview => (
                Role::ToggleButton,
                tr("slides-a11y-slide-overview"),
                AccessState::new().checked(self.mode == Mode::Overview),
            ),
            Hit::OpenSlide(index) | Hit::SelectSlide(index) => (
                Role::Button,
                tr_args(
                    "slides-a11y-open-slide",
                    &[("n", ArgValue::from(*index + 1))],
                ),
                AccessState::new().selected(*index == self.cur_slide),
            ),
            Hit::CyclePalette => (
                Role::Button,
                tr_args(
                    "slides-a11y-change-theme",
                    &[("name", ArgValue::from(self.deck.palette.label()))],
                ),
                AccessState::new(),
            ),
            Hit::Insert(kind) => (
                Role::Button,
                tr_args(
                    "slides-a11y-insert",
                    &[("kind", ArgValue::from(kind.label().to_lowercase()))],
                ),
                AccessState::new(),
            ),
            Hit::SetFill(role) => (
                Role::RadioButton,
                tr_args(
                    "slides-a11y-fill",
                    &[("role", ArgValue::from(role.as_str()))],
                ),
                AccessState::new().checked(selected.is_some_and(|element| element.fill == *role)),
            ),
            Hit::SetStroke(role) => (
                Role::RadioButton,
                tr_args(
                    "slides-a11y-stroke",
                    &[("role", ArgValue::from(role.as_str()))],
                ),
                AccessState::new().checked(selected.is_some_and(|element| element.stroke == *role)),
            ),
            Hit::OpenFillPicker => (
                Role::Button,
                tr("slides-a11y-custom-fill"),
                AccessState::new(),
            ),
            Hit::ToggleBold => (
                Role::ToggleButton,
                tr("slides-a11y-bold"),
                AccessState::new().checked(selected.is_some_and(|element| element.bold)),
            ),
            Hit::ToggleItalic => (
                Role::ToggleButton,
                tr("slides-a11y-italic"),
                AccessState::new().checked(selected.is_some_and(|element| element.italic)),
            ),
            Hit::SetAlign(align) => (
                Role::RadioButton,
                tr_args(
                    "slides-a11y-align",
                    &[("align", ArgValue::from(align.as_str()))],
                ),
                AccessState::new().checked(selected.is_some_and(|element| element.align == *align)),
            ),
            Hit::BringFront => (
                Role::Button,
                tr("slides-a11y-bring-front"),
                AccessState::new(),
            ),
            Hit::SendBack => (
                Role::Button,
                tr("slides-a11y-send-back"),
                AccessState::new(),
            ),
            Hit::DuplicateElement => (
                Role::Button,
                tr("slides-a11y-duplicate-element"),
                AccessState::new(),
            ),
            Hit::DeleteElement => (
                Role::Button,
                tr("slides-a11y-delete-element"),
                AccessState::new(),
            ),
            Hit::SetLayout(layout) => (
                Role::RadioButton,
                tr_args(
                    "slides-a11y-layout",
                    &[("name", ArgValue::from(layout.label()))],
                ),
                AccessState::new()
                    .checked(self.slide().is_some_and(|slide| slide.layout == *layout)),
            ),
            Hit::SetBackground(background) => (
                Role::RadioButton,
                tr_args(
                    "slides-a11y-background",
                    &[("name", ArgValue::from(background.label()))],
                ),
                AccessState::new().checked(
                    self.slide()
                        .is_some_and(|slide| slide.background == *background),
                ),
            ),
            Hit::SetPalette(palette) => (
                Role::RadioButton,
                tr_args(
                    "slides-a11y-deck-theme",
                    &[("name", ArgValue::from(palette.label()))],
                ),
                AccessState::new().checked(self.deck.palette == *palette),
            ),
            Hit::EditNotes => (
                Role::Button,
                tr("slides-a11y-edit-notes"),
                AccessState::new(),
            ),
            Hit::ToggleRail => (
                Role::ToggleButton,
                tr("slides-a11y-slides-panel"),
                AccessState::new().checked(self.rail_open()),
            ),
            Hit::ToggleInspector => (
                Role::ToggleButton,
                tr("slides-a11y-properties-panel"),
                AccessState::new().checked(self.inspector_open()),
            ),
            Hit::ToggleToolbarMenu => (
                Role::Button,
                tr("slides-a11y-more-tools"),
                AccessState::new().expanded(self.toolbar_tools.is_open()),
            ),
        }
    }

    fn tooltip_at(&self, point: Point) -> Option<String> {
        let chrome = self.compute_chrome();
        chrome
            .buttons
            .iter()
            .chain(chrome.toolbar_tools.menu_buttons.iter())
            .find(|button| button.rect.contains(point))
            .and_then(|button| match button.hit {
                Hit::ToggleRail => Some(tr("slides-tip-toggle-slides")),
                Hit::ToggleInspector => Some(tr("slides-tip-toggle-properties")),
                Hit::Overview => Some(tr("slides-tip-overview")),
                Hit::CyclePalette => Some(tr("slides-tip-theme")),
                Hit::ToggleToolbarMenu => Some(tr("slides-tip-more-tools")),
                _ => None,
            })
    }
}

impl FullscreenProvider for SlidesApp {
    fn fullscreen_intent(&self) -> Option<bool> {
        Some(self.wants_fullscreen() && !self.presentation.has_external_audience())
    }

    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        ui::fullscreen::FullscreenRequest::distinct(self.presentation_target)
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        // Installing a real audience surface intentionally unfullscreens the
        // presenter; only an ordinary compositor exit ends the talk.
        if !active && self.mode == Mode::Present && !self.presentation.has_external_audience() {
            self.exit_present();
        }
    }
}

impl AudiencePresentationProvider for SlidesApp {
    fn audience_presentation_request(&self) -> Option<ui::fullscreen::AudiencePresentationRequest> {
        (self.mode == Mode::Present)
            .then(|| ui::fullscreen::AudiencePresentationRequest::new(self.presentation_target))
    }

    fn on_audience_presentation_changed(&mut self, active: bool) {
        self.presentation.set_external_audience(active);
    }

    fn draw_audience_presentation(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) -> bool {
        if self.mode != Mode::Present {
            return false;
        }
        let frame = PresentationFrame::new(
            rect,
            &self.deck,
            self.cur_slide,
            self.deck_rev,
            &self.images,
        );
        self.presentation
            .draw_audience(scene, transform, text_renderer, theme, &frame);
        true
    }

    fn handle_audience_presentation_event(
        &mut self,
        event: &WindowEvent,
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        if self.mode != Mode::Present {
            return false;
        }
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        ..
                    },
                ..
            } => {
                let mut handled = false;
                self.on_key_present(logical_key, &mut handled);
                handled
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.next_slide(1);
                true
            }
            _ => false,
        }
    }

    fn on_audience_presentation_closed(&mut self) {
        self.exit_present();
    }
}

impl Widget for SlidesApp {
    ui::provides!(
        dyn ui::widget::FullscreenProvider,
        dyn ui::widget::AudiencePresentationProvider,
        dyn ui::resources::PublishesResources
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::SLIDES_OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if cancellation_requested && self.resource_invocation == Some(invocation) {
            self.resource_invocation = None;
            self.resource_file = None;
            self.doc_portal_rx = None;
            // Imports can still finish after cancellation; their stale result
            // is inert. Dispatched saves have separate invocation custody and
            // settle only with the worker's actual outcome.
            self.active_document_generation = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("presentation file operation cancelled".into()),
            });
        }
        if self
            .completed_resource_invocation
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return self.completed_resource_invocation.take();
        }
        None
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        self.adopt_deferred();
        if let Some((save, file)) = self.resource_file.take() {
            let queued = if save {
                self.queue_document_save(file)
            } else {
                self.queue_document_open(file)
            };
            if !queued {
                self.finish_resource_file(
                    false,
                    Some("presentation file operation could not be started".into()),
                );
            }
        }
        // Fold in a completed host image-open, if the portal worker finished.
        self.drain_portal();
        // Fold in a completed `.odp` open/save.
        self.drain_doc_portal();
        self.drain_document_worker();
        // One-time view bootstrap: open on the second slide with its title
        // selected so the editor demonstrates selection + the element
        // inspector immediately (also what `--shots` captures).
        if !self.bootstrapped && self.deck.slides.len() > 1 {
            self.cur_slide = 1;
            if let Some(slide) = self.deck.slides.get(1) {
                self.sel = slide
                    .elements
                    .iter()
                    .find(|e| e.kind == ElementKind::Text)
                    .map(|e| e.id.clone());
            }
            self.bootstrapped = true;
        }
        if self.editing_notes {
            self.sync_notes(state);
        }
        self.ensure_text_editor(state);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `Widget::build_layout` returns a bare `NodeId`, not a `Result` — a
        // framework-wide contract this one widget can't change — so a
        // `new_leaf` failure (which a freshly-created tree with one valid
        // leaf style cannot produce) has no propagation path here.
        #[allow(
            clippy::unwrap_used,
            reason = "new_leaf on a fresh tree with a valid Style cannot fail; Widget::build_layout's Result-free signature is shared by every implementor and out of this crate's scope to change"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    // `taffy.layout(...)` only errors for a `NodeId` from a different tree,
    // which never happens here — every id it's called with is one this
    // widget received from its own `build_layout`. `Widget::apply_layout`
    // returns `()` across every implementor, so surfacing a theoretical
    // error here would mean threading a `Result` through the whole toolkit
    // for a branch that can't be taken.
    #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.rect.width() < 2.0 {
            return;
        }
        self.text_scale = theme.text_scale;
        // Fold in any images that finished decoding on their worker thread. A
        // freshly-decoded image must re-shape the cached present-mode slide.
        if self.images.pump() {
            self.presentation.invalidate_cache();
        }
        // Expire a settled cross-fade so `needs_animation_frame` can rest.
        self.presentation.expire_transition(theme.reduce_motion);
        // Measure first, so everything this frame lays out — the flap and rail
        // sync below, `compute_chrome`, and the hit-rects later events derive
        // from it — comes from the renderer that is about to paint it.
        self.refresh_chrome_metrics(text_renderer, theme);
        // Reconcile the rail flap with the current width (so resizing across
        // the breakpoint returns to the layout default) and advance its eased
        // reveal using real elapsed time, frame-rate independent.
        let now = ui::time::now();
        let dt = (now - self.last_flap_tick).as_secs_f64().min(0.1);
        self.last_flap_tick = now;
        self.sync_flaps(dt, theme.reduce_motion);
        self.sync_scroll_offsets(dt);
        self.stage_shots_demo();
        match self.mode {
            Mode::Edit => {
                let chrome = self.compute_chrome();
                // Hand the inspector's scroll model this frame's viewport and
                // content extent; it clamps the stored offset and paints its
                // bar from the same numbers `compute_chrome` shifted by.
                self.inspector_scroll.set_viewport_rect(chrome.inspector);
                self.inspector_scroll.set_content_size(
                    chrome.inspector.width() as f32,
                    chrome.inspector_content_h as f32,
                );
                self.draw_editor(scene, transform, text_renderer, theme, &chrome);
                self.draw_inspector_widgets(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                    &chrome,
                );
                if self.editing_title {
                    self.draw_title_input(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        presence,
                        &chrome,
                    );
                }
            }
            Mode::Overview => self.draw_overview(scene, transform, text_renderer, theme),
            Mode::Present => self.draw_present(scene, transform, text_renderer, theme),
        }
        // The help overlay draws over every mode — including Present, where it
        // must appear (and be dismissable), otherwise a `?` press would set the
        // flag, suppress the overlay, and swallow all later navigation keys.
        let palette = self.palette(theme);
        self.help_overlay
            .draw(scene, transform, text_renderer, theme, self.rect, &palette);
        if let Some((message, kind)) = self.io_toast.as_ref() {
            paint_transient(
                scene,
                transform,
                text_renderer,
                theme,
                self.rect,
                ToastAnchor::BottomCenter,
                &ToastCard::new(message, *kind),
            );
        }
        // Right-click context menu floats above everything else.
        self.ctx_menu
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn needs_animation_frame(&self) -> bool {
        // Only ask for another frame when something is actually moving:
        //   * a slide cross-fade is mid-flight (bounded ~240ms),
        //   * a rail/inspector flap is sliding open/closed (bounded), or
        //   * an image is still decoding on its worker thread.
        // Present mode is deliberately NOT an unconditional "true": the settled
        // slide is static and the speaker clock reads wall-time on each paint,
        // so it re-renders on navigation/input rather than spinning at vsync
        // for a once-a-second clock. See the note on the redraw cadence below.
        self.presentation.needs_animation_frame()
            || self.rail_flap.is_animating(self.rail_layout())
            || self.inspector_flap.is_animating(self.inspector_layout())
            || self.images.any_loading()
            // A live thumbnail reorder pumps frames so the ghost tracks the
            // cursor and the rail auto-scrolls at its edges.
            || self.page_sidebar.is_dragging()
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        if reduced {
            self.presentation.expire_transition(true);
            let rail = self.rail_layout();
            let inspector = self.inspector_layout();
            self.rail_flap.snap(self.rail_flap.is_open(rail));
            self.inspector_flap
                .snap(self.inspector_flap.is_open(inspector));
        }
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
        self.draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // A restore replaces the notes' text object; the first event may
        // precede the sync that follows it.
        if self.editing_notes {
            self.sync_notes(ctx.state);
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } | WindowEvent::KeyboardInput {
                event: KeyEvent {
                    state: ElementState::Pressed,
                    ..
                },
                ..
            }
        ) {
            self.io_toast = None;
        }
        let mut routed = RoutedEvent {
            event,
            scale_factor: ctx.scale_factor,
            modifiers: *ctx.modifiers,
            state: ctx.state,
            path: ctx.path,
            focus_target: ctx.focus_target,
            node_id: ctx.node_id,
            taffy: ctx.taffy,
        };
        if self.route_notes_edit(&mut routed) {
            *ctx.handled = true;
            return;
        }
        // A right-press (re)opens a context menu — handled before the modal
        // routing so it works even while another menu is already up.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } = routed.event
        {
            self.on_right_press(self.last_mouse, ctx.handled);
            return;
        }
        // While a context menu is open it is modal: every other event goes to it.
        if self.route_context_menu(&mut routed) {
            *ctx.handled = true;
            return;
        }
        // The inline title editor owns input while renaming.
        if self.route_title_edit(&mut routed) {
            *ctx.handled = true;
            return;
        }
        // The inspector's embedded widgets (font-size field, fill picker) get
        // first refusal on events over them.
        if self.route_inspector_widgets(&mut routed) {
            *ctx.handled = true;
            return;
        }
        // The text box being edited owns the keyboard and its own pointer.
        if self.route_text_edit(&mut routed, ctx.handled) {
            return;
        }
        self.dispatch_unmodal_event(
            routed.event,
            routed.scale_factor,
            routed.modifiers,
            ctx.handled,
        );
    }

    fn name(&self) -> &'static str {
        "SlidesApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("slides-editor")
    }

    /// Slides paints a single retained canvas, so every visible control and
    /// durable object is represented as a synthetic semantic child. Controls
    /// use the same `Hit` values as pointer input and stable document ids are
    /// used for slides/elements, keeping AT activation from drifting after a
    /// reorder or remote edit.
    #[allow(
        clippy::too_many_lines,
        reason = "the custom-painted editor has editor, presentation, and modal semantic projections in one Widget hook"
    )]
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};

        if self.help_overlay.is_visible() {
            let shortcuts = [
                "N: New slide",
                "P: Start presentation",
                "G: Slide overview",
                "T, R, O, L: Insert text, rectangle, ellipse, or line",
                "Tab: Cycle objects on the slide",
                "Enter: Edit selected text",
                "Arrow keys: Move the selected object",
                "Control Z and Control Y: Undo and redo",
                "Question mark or Escape: Close keyboard shortcuts",
            ]
            .into_iter()
            .map(|shortcut| AccessNode::new(Role::StaticText).with_name(shortcut))
            .collect();
            return Some(
                AccessNode::new(Role::Dialog)
                    .with_name("Keyboard shortcuts")
                    .with_description("Slides keyboard reference")
                    .with_state(AccessState::new().modal(true))
                    .with_children(vec![
                        AccessNode::new(Role::List)
                            .with_name("Shortcuts")
                            .with_children(shortcuts),
                        AccessNode::new(Role::Button)
                            .with_name("Close keyboard shortcuts")
                            .with_action_target("slides.help.close")
                            .with_state(AccessState::new().focusable(true)),
                    ]),
            );
        }

        let count = self.deck.slides.len();
        if self.mode == Mode::Present {
            let title = self
                .slide()
                .and_then(|slide| {
                    slide.elements.iter().find_map(|element| {
                        (element.kind == ElementKind::Text && !element.text.trim().is_empty())
                            .then(|| element.text.lines().next().unwrap_or("").to_string())
                    })
                })
                .unwrap_or_else(|| format!("Slide {}", self.cur_slide + 1));
            let controls = [
                ("Previous slide", "slides.present.previous"),
                ("Next slide", "slides.present.next"),
                ("First slide", "slides.present.first"),
                ("Last slide", "slides.present.last"),
                ("Toggle speaker notes", "slides.present.notes"),
                ("Toggle presenter view", "slides.present.presenter"),
                ("End presentation", "slides.present.exit"),
            ]
            .into_iter()
            .map(|(name, target)| {
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_action_target(target)
                    .with_state(AccessState::new().focusable(true))
            })
            .collect();
            return Some(
                AccessNode::new(Role::Document)
                    .with_name(format!("Presenting {}", self.deck.title))
                    .with_value(format!("Slide {} of {count}: {title}", self.cur_slide + 1))
                    .with_description(format!("Elapsed {}", self.presentation.clock()))
                    .with_state(AccessState::new().focusable(true))
                    .with_children(vec![
                        AccessNode::new(Role::Toolbar)
                            .with_name("Presentation controls")
                            .with_children(controls),
                    ]),
            );
        }

        // The semantic rail is windowed around the current slide. Global
        // search and `resource(id)` resolve the entire durable deck lazily; an
        // accessibility frame remains bounded even for an imported 1,000-slide
        // presentation.
        let start = self
            .cur_slide
            .saturating_sub(A11Y_SLIDE_WINDOW / 2)
            .min(count.saturating_sub(A11Y_SLIDE_WINDOW));
        let slides = self
            .deck
            .slides
            .iter()
            .enumerate()
            .skip(start)
            .take(A11Y_SLIDE_WINDOW)
            .map(|(index, slide)| {
                let title = slide
                    .elements
                    .iter()
                    .find(|element| element.kind == ElementKind::Text)
                    .map(|element| element.text.clone())
                    .filter(|text| !text.trim().is_empty())
                    .unwrap_or_else(|| format!("Slide {}", index + 1));
                AccessNode::new(Role::Button)
                    .with_name(title)
                    .with_description(format!("Slide {} of {count}", index + 1))
                    .with_action_target(format!("slides.slide:{}", slide.id))
                    .with_position(index + 1, count)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(index == self.cur_slide),
                    )
            })
            .collect();

        let chrome = self.compute_chrome();
        let mut seen = std::collections::HashSet::new();
        let controls = chrome
            .buttons
            .iter()
            .chain(chrome.toolbar_tools.menu_buttons.iter())
            .filter(|button| self.rect.contains(button.rect.center()))
            .filter_map(|button| {
                let target = self.semantic_target(&button.hit);
                if !seen.insert(target.clone()) {
                    return None;
                }
                let (role, name, mut state) = self.semantic_copy(&button.hit);
                state.focusable = !state.disabled;
                let node = AccessNode::new(role)
                    .with_name(name)
                    .with_action_target(target)
                    .with_bounds(button.rect)
                    .with_state(state);
                // The notes box reads out what it holds, as a field would.
                Some(match (&button.hit, self.slide()) {
                    (Hit::EditNotes, Some(slide)) if !slide.notes.is_empty() => {
                        node.with_value(slide.notes.clone())
                    }
                    _ => node,
                })
            })
            .collect();

        let elements = self.slide().map_or_else(Vec::new, |slide| {
            slide
                .z_sorted()
                .into_iter()
                .take(A11Y_ELEMENT_WINDOW)
                .map(|element| {
                    let name =
                        if element.kind == ElementKind::Text && !element.text.trim().is_empty() {
                            element.text.lines().next().unwrap_or("Text").to_string()
                        } else {
                            element.kind.label().to_string()
                        };
                    AccessNode::new(if element.kind == ElementKind::Image {
                        Role::Image
                    } else {
                        Role::Button
                    })
                    .with_name(name)
                    .with_description(element.kind.label())
                    .with_action_target(format!("slides.element:{}", element.id))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.sel.as_deref() == Some(element.id.as_str())),
                    )
                })
                .collect()
        });

        let mut children = vec![
            AccessNode::new(Role::Toolbar)
                .with_name("Slides controls")
                .with_children(controls),
            AccessNode::new(Role::List)
                .with_name("Slides")
                .with_description(format!(
                    "Showing slides {} through {} of {count}",
                    start + 1,
                    (start + A11Y_SLIDE_WINDOW).min(count)
                ))
                .with_children(slides),
            AccessNode::new(Role::Canvas)
                .with_name(format!("Slide {} canvas", self.cur_slide + 1))
                .with_children(elements),
        ];
        if let Some((message, kind)) = &self.io_toast {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(message)
                    .with_live(if *kind == ToastKind::Error {
                        Politeness::Assertive
                    } else {
                        Politeness::Polite
                    }),
            );
        }
        Some(
            AccessNode::new(Role::Document)
                .with_name(self.deck.title.clone())
                .with_value(format!("Slide {} of {count}", self.cur_slide + 1))
                .with_description("Presentation editor with slide canvas and notes")
                .with_state(AccessState::new().focusable(true))
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        if target == "slides.help.close" {
            self.help_overlay.hide();
            return true;
        }
        let present_key = match target {
            "slides.present.previous" => Some(Key::Named(NamedKey::ArrowLeft)),
            "slides.present.next" => Some(Key::Named(NamedKey::ArrowRight)),
            "slides.present.first" => Some(Key::Named(NamedKey::Home)),
            "slides.present.last" => Some(Key::Named(NamedKey::End)),
            "slides.present.notes" => Some(Key::Character("s".into())),
            "slides.present.presenter" => Some(Key::Character("p".into())),
            "slides.present.exit" => Some(Key::Named(NamedKey::Escape)),
            _ => None,
        };
        if let Some(key) = present_key {
            if self.mode != Mode::Present {
                return false;
            }
            let mut handled = false;
            self.on_key_present(&key, &mut handled);
            return handled;
        }
        if let Some(slide_id) = target.strip_prefix("slides.slide:") {
            if let Some(index) = self
                .deck
                .slides
                .iter()
                .position(|slide| slide.id == slide_id)
            {
                self.apply_hit(Hit::SelectSlide(index));
                return true;
            }
            return false;
        }
        if let Some(element_id) = target.strip_prefix("slides.element:") {
            if self.slide().is_some_and(|slide| {
                slide
                    .elements
                    .iter()
                    .any(|element| element.id == element_id)
            }) {
                self.sel = Some(element_id.to_string());
                self.editing_text = false;
                self.editing_notes = false;
                return true;
            }
            return false;
        }
        let chrome = self.compute_chrome();
        let hit = chrome
            .buttons
            .iter()
            .chain(chrome.toolbar_tools.menu_buttons.iter())
            .find(|button| self.semantic_target(&button.hit) == target)
            .map(|button| button.hit.clone());
        if let Some(hit) = hit {
            self.apply_hit(hit);
            true
        } else {
            false
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.editing_notes {
            return self.notes.control().cursor_shape(point);
        }
        None
    }

    fn tooltip(&self) -> Option<&str> {
        self.tooltip_text.as_deref()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.editing_text
            && let Some(editor) = &self.text_editor
        {
            return editor.host.ime_cursor_rect();
        }
        self.editing_notes
            .then(|| self.notes.control().ime_cursor_rect())
            .flatten()
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.editing_notes
            .then(|| self.notes.control().ime_purpose())
            .flatten()
    }

    /// Keep receiving pointer events for the whole of a canvas move/resize or a
    /// thumbnail reorder, even if the cursor briefly leaves the source rect.
    fn wants_pointer_capture(&self) -> bool {
        self.drag.active() || self.page_sidebar.is_pending_drag()
    }
}

impl ui::resources::PublishesResources for SlidesApp {
    /// The deck's edits answer their caller with the deck's outline, so an
    /// agent sees what it changed without reading the deck again. The deck
    /// itself is published by its resource; the editor publishes nothing
    /// beside it.
    fn document_action_result(
        &self,
        doc: &AutoCommit,
        address: &tilo_actions::ActionAddress,
        _changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        address
            .resource
            .as_str()
            .starts_with("slides.")
            .then(|| crate::outline::of_document(doc).map(serde_json::Value::String))
            .flatten()
    }
}

impl SlidesApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        use crate::actions::SlidesOperation;

        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        // Keep the registered wire type on the canonical dispatch path.  This
        // is both a cheap rejection of foreign operation payloads and makes
        // the runtime registry auditable independently from the decoder.
        if type_id != crate::actions::SLIDES_OPERATION_TYPE {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        match operation {
            SlidesOperation::ShowEditor => self.mode = Mode::Edit,
            SlidesOperation::ShowOverview => self.mode = Mode::Overview,
            SlidesOperation::StartPresentation => self.enter_present(),
            SlidesOperation::StartPresentationOn { output } => {
                self.presentation_target = match output {
                    crate::actions::PresentationOutput::LargestExternal => {
                        ui::fullscreen::FullscreenTarget::Largest
                    }
                    crate::actions::PresentationOutput::PrimaryExternal => {
                        ui::fullscreen::FullscreenTarget::PrimaryExternal
                    }
                };
                self.enter_present();
            }
            SlidesOperation::EndPresentation => self.exit_present(),
            SlidesOperation::RevealSlide { slide } => {
                let Some(idx) = self.deck.index_of(&slide) else {
                    return Err("slide no longer exists".into());
                };
                self.mode = Mode::Edit;
                self.editing_notes = false;
                self.goto_slide(idx);
                self.scroll_rail_to_current();
            }
            SlidesOperation::RevealElement {
                slide,
                element,
                edit_text,
            } => {
                let Some(idx) = self.deck.index_of(&slide) else {
                    return Err("slide no longer exists".into());
                };
                let Some(target) = self
                    .deck
                    .slides
                    .get(idx)
                    .and_then(|candidate| candidate.element(&element))
                else {
                    return Err("element no longer exists".into());
                };
                if edit_text && target.kind != ElementKind::Text {
                    return Err("element is not editable text".into());
                }
                self.mode = Mode::Edit;
                self.editing_notes = false;
                self.goto_slide(idx);
                self.sel = Some(element);
                self.editing_text = edit_text;
                if edit_text {
                    // A text search result restores an immediately useful edit
                    // state: the exact text resource is selected in full.
                    self.begin_text_edit(true);
                }
                self.scroll_rail_to_current();
            }
            SlidesOperation::RevealNotes { slide } => {
                let Some(idx) = self.deck.index_of(&slide) else {
                    return Err("slide no longer exists".into());
                };
                self.mode = Mode::Edit;
                self.goto_slide(idx);
                self.editing_notes = true;
                self.editing_text = false;
                self.sel = None;
                self.scroll_rail_to_current();
            }
            SlidesOperation::Open { .. } | SlidesOperation::SaveAs { .. } => {
                return Err(
                    "presentation file operations require the lifecycle-capable host".into(),
                );
            }
            SlidesOperation::Read => return Ok(false),
        }
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if type_id != crate::actions::SLIDES_OPERATION_TYPE {
            return Ok(None);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        let save_operation = matches!(&operation, crate::actions::SlidesOperation::SaveAs { .. });
        match operation {
            crate::actions::SlidesOperation::Open { path: file }
            | crate::actions::SlidesOperation::SaveAs { path: file } => {
                if self.resource_invocation.is_some() || self.dispatched_save.is_some() {
                    return Err("another presentation file operation is running".into());
                }
                let save = save_operation;
                self.resource_invocation = Some(invocation);
                let direct = file.is_some();
                if let Some(file) = file {
                    self.resource_file = Some((save, file.into()));
                } else if save {
                    self.request_odp_save();
                } else {
                    self.request_odp_open();
                }
                if !direct && self.doc_portal_rx.is_none() {
                    self.resource_invocation = None;
                    return Err("presentation picker could not be started".into());
                }
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            }
            crate::actions::SlidesOperation::Read => Ok(Some(
                ui::widget::ResourceOperationStart::Completed(tilo_actions::ActionOutput {
                    value: Some(
                        crate::outline::of_document(doc)
                            .map_or(serde_json::Value::Null, serde_json::Value::String),
                    ),
                    ..tilo_actions::ActionOutput::default()
                }),
            )),
            _ => self
                .answer_resource_operation(kind, type_id, payload, doc, path)
                .map(|handled| {
                    handled.then(|| {
                        ui::widget::ResourceOperationStart::Completed(
                            tilo_actions::ActionOutput::default(),
                        )
                    })
                }),
        }
    }
}

impl SlidesApp {
    /// The non-modal event dispatch: cursor motion (rail drag / text-selection
    /// drag / element drag), the left mouse button, the wheel, and key presses.
    /// Reached only once none of the modal routers (context menu, title edit,
    /// inspector widgets) claimed the event.
    fn dispatch_unmodal_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        handled: &mut bool,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                self.tooltip_text = self.tooltip_at(p);
                if self.page_sidebar.is_pending_drag() || self.page_sidebar.menu_open() {
                    // Feed the shared rail's reorder controller / menu hover; the
                    // gap + hover track the cursor. (The rail's list geometry is
                    // refreshed each frame in `draw` and on press.)
                    self.page_sidebar.on_motion(p);
                    *handled = true;
                } else if self.drag.active() {
                    self.drag_move(p);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: btn_state,
                button: MouseButton::Left,
                ..
            } => match btn_state {
                ElementState::Pressed => self.on_press(self.last_mouse, handled),
                ElementState::Released => self.on_release(handled),
            },
            WindowEvent::MouseWheel { delta, .. } => {
                self.on_wheel(*delta, scale_factor, handled);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        text,
                        ..
                    },
                ..
            } => {
                self.on_key(
                    logical_key,
                    text.as_ref().map(winit::keyboard::SmolStr::as_str),
                    modifiers,
                    handled,
                );
            }
            _ => {}
        }
    }

    /// Re-clamp the persisted scroll offsets to the current size/content, so
    /// a window resize (or a fresh over-large offset) self-corrects rather
    /// than sticking past the end of the list. Called once per `draw`.
    fn sync_scroll_offsets(&mut self, dt: f64) {
        match self.mode {
            Mode::Edit => {
                // Auto-scroll the rail while a thumbnail reorder drag hovers near
                // the viewport edges, so a slide can be dropped at a position
                // currently scrolled off-screen.
                if self.page_sidebar.is_dragging() {
                    let vp = self.rail_viewport(self.rail_rect());
                    let edge = 40.0;
                    let y = self.last_mouse.y;
                    let v = if y < vp.y0 + edge {
                        -((vp.y0 + edge - y) / edge).clamp(0.0, 1.0)
                    } else if y > vp.y1 - edge {
                        ((y - (vp.y1 - edge)) / edge).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    if v != 0.0 {
                        self.rail_scroll += v * 900.0 * dt;
                    }
                }
                // Sync the shared rail to this frame's geometry + scroll, which
                // clamps the offset and re-pins the insertion gap to the held
                // ghost against the freshest (possibly just-scrolled) geometry.
                self.sync_page_rail();
            }
            Mode::Overview => {
                self.overview.clamp(self.rect, self.deck.slides.len());
            }
            Mode::Present => {}
        }
    }

    /// Stage the three `--shots`-only demo interactions (a popped context
    /// menu, a live reorder drag, a thumbnail dropped on the delete zone)
    /// now that the layout rect is known. No-op on a live launch — the
    /// staging flags are never set outside [`SlidesApp::stage_scenario`].
    fn stage_shots_demo(&mut self) {
        // `--shots`-only: pop a context menu now that the layout rect is known.
        if let Some(kind) = self.demo_menu.take() {
            let chrome = self.compute_chrome();
            match kind {
                DemoMenu::Element => {
                    let p = self.selected_element().map_or_else(
                        || chrome.slide_dest.center(),
                        |el| render::element_dest(chrome.slide_dest, chrome.scale, el).center(),
                    );
                    self.open_element_menu(p);
                }
                DemoMenu::Slide => {
                    // Open the shared rail's own duplicate / move / delete menu on
                    // the current slide's thumbnail.
                    let p = chrome
                        .thumbs
                        .iter()
                        .find(|(i, _)| *i == self.cur_slide)
                        .map_or_else(|| Point::new(120.0, 220.0), |(_, r)| r.center());
                    self.page_sidebar.on_right_press(p);
                    let _ = self.page_sidebar.take_events();
                }
            }
        }
        // `--shots`-only: stage a live reorder drag so the capture shows the
        // floating ghost + the reflowed drop gap.
        if std::mem::take(&mut self.demo_reorder) {
            let thumbs = self.rail_thumb_rects();
            if let (Some(first), Some(third)) = (thumbs.first(), thumbs.get(2)) {
                let start = first.1.center();
                // Drop it two slots down: the ghost floats at the cursor and the
                // list reflows to open the gap at that slot.
                let target = third.1.center();
                self.last_mouse = target;
                self.page_sidebar.on_press(start);
                self.page_sidebar.on_motion(target);
                let _ = self.page_sidebar.take_events();
            }
        }
        // `--shots`-only: stage a thumbnail dragged onto the delete zone so the
        // capture shows the armed red dashed "Drop to delete" target.
        if std::mem::take(&mut self.demo_delete_zone) {
            let thumbs = self.rail_thumb_rects();
            if let Some(second) = thumbs.get(1) {
                let start = second.1.center();
                let drop = self.slide_delete_zone(self.rail_rect()).center();
                self.last_mouse = drop;
                self.page_sidebar.on_press(start);
                self.page_sidebar.on_motion(drop);
                let _ = self.page_sidebar.take_events();
            }
        }
    }

    /// Whether the picture `src` is decoded and ready to paint.
    #[cfg(test)]
    pub(crate) fn picture_ready(&self, src: &str) -> bool {
        matches!(self.images.get(src), crate::assets::ImageState::Ready(_))
    }

    /// The index of the slide currently shown. Lets a test observe where a
    /// Ctrl+K activation navigated.
    pub fn current_slide(&self) -> usize {
        self.cur_slide
    }

    /// Snap a moving element to the slide's center/edge guides and to the
    /// anchors (left/center/right · top/middle/bottom) of its siblings.
    /// Returns the adjusted top-left plus the guide lines that fired.
    fn snapped_move(
        &self,
        id: &str,
        nx: f64,
        ny: f64,
        w: f64,
        h: f64,
        thr: f64,
    ) -> (f64, f64, Vec<Guide>) {
        // Candidate target lines in slide space.
        let mut vx: Vec<f64> = vec![0.0, SLIDE_W / 2.0, SLIDE_W, 96.0, SLIDE_W - 96.0];
        let mut hy: Vec<f64> = vec![0.0, SLIDE_H / 2.0, SLIDE_H, 70.0, SLIDE_H - 60.0];
        if let Some(slide) = self.slide() {
            for e in &slide.elements {
                if e.id == id {
                    continue;
                }
                vx.push(e.x);
                vx.push(e.x + e.w / 2.0);
                vx.push(e.x + e.w);
                hy.push(e.y);
                hy.push(e.y + e.h / 2.0);
                hy.push(e.y + e.h);
            }
        }
        let mut guides = Vec::new();

        // X axis: try left, center, right anchors; keep the closest snap.
        let anchors_x = [(nx, 0.0), (nx + w / 2.0, w / 2.0), (nx + w, w)];
        let mut best_x: Option<(f64, f64, f64)> = None; // (dist, new_left, guide_line)
        for (anchor, off) in anchors_x {
            for &target in &vx {
                let d = (anchor - target).abs();
                if d <= thr && best_x.is_none_or(|b| d < b.0) {
                    best_x = Some((d, target - off, target));
                }
            }
        }
        let out_x = if let Some((_, left, line)) = best_x {
            guides.push(Guide::Vertical(line));
            left
        } else {
            nx
        };

        let anchors_y = [(ny, 0.0), (ny + h / 2.0, h / 2.0), (ny + h, h)];
        let mut best_y: Option<(f64, f64, f64)> = None;
        for (anchor, off) in anchors_y {
            for &target in &hy {
                let d = (anchor - target).abs();
                if d <= thr && best_y.is_none_or(|b| d < b.0) {
                    best_y = Some((d, target - off, target));
                }
            }
        }
        let out_y = if let Some((_, top, line)) = best_y {
            guides.push(Guide::Horizontal(line));
            top
        } else {
            ny
        };

        (out_x, out_y, guides)
    }

    fn drag_move(&mut self, p: Point) {
        // Scrollbar-thumb drags don't need slide-space mapping — resolve them
        // first (they can be live in overview mode, where there is no canvas).
        if let Drag::ScrollOverview {
            start_y,
            orig_scroll,
        } = self.drag.clone()
        {
            let (area, _, max_scroll) = self.compute_overview();
            self.overview.set_scroll(scroll_from_thumb_drag(
                area,
                orig_scroll,
                max_scroll,
                p.y - start_y,
            ));
            return;
        }
        let chrome = self.compute_chrome();
        let dest = chrome.slide_dest;
        let s = chrome.scale;
        let sp = self.to_slide(dest, s, p);
        match self.drag.clone() {
            Drag::Move {
                id,
                start,
                orig_x,
                orig_y,
            } => {
                let dx = sp.x - start.x;
                let dy = sp.y - start.y;
                let (w, h) = self.selected_element().map_or((0.0, 0.0), |e| (e.w, e.h));
                let thr = SNAP_PX / s.max(0.0001);
                let (nx, ny, guides) = self.snapped_move(&id, orig_x + dx, orig_y + dy, w, h, thr);
                self.active_guides = guides;
                if let Some(slide) = self.deck.slides.get_mut(self.cur_slide)
                    && let Some(el) = slide.element_mut(&id)
                {
                    el.x = nx;
                    el.y = ny;
                }
            }
            Drag::Resize {
                id,
                handle,
                start,
                orig,
            } => {
                let dx = sp.x - start.x;
                let dy = sp.y - start.y;
                let (mut rx, mut ry, mut rw, mut rh) = orig;
                let left = matches!(handle, Handle::Tl | Handle::L | Handle::Bl);
                let right = matches!(handle, Handle::Tr | Handle::R | Handle::Br);
                let top = matches!(handle, Handle::Tl | Handle::T | Handle::Tr);
                let bottom = matches!(handle, Handle::Bl | Handle::B | Handle::Br);
                if left {
                    rx = orig.0 + dx;
                    rw = orig.2 - dx;
                }
                if right {
                    rw = orig.2 + dx;
                }
                if top {
                    ry = orig.1 + dy;
                    rh = orig.3 - dy;
                }
                if bottom {
                    rh = orig.3 + dy;
                }
                let min = 16.0;
                if rw < min {
                    if left {
                        rx = orig.0 + orig.2 - min;
                    }
                    rw = min;
                }
                if rh < min {
                    if top {
                        ry = orig.1 + orig.3 - min;
                    }
                    rh = min;
                }
                if let Some(slide) = self.deck.slides.get_mut(self.cur_slide)
                    && let Some(el) = slide.element_mut(&id)
                {
                    el.x = rx;
                    el.y = ry;
                    el.w = rw;
                    el.h = rh;
                }
            }
            // `ScrollOverview` is resolved above; everything else is a no-op here.
            _ => {}
        }
    }

    fn on_press(&mut self, p: Point, handled: &mut bool) {
        // The help overlay swallows the next click (to dismiss).
        if self.help_overlay.on_pointer_press(p, self.rect) {
            *handled = true;
            return;
        }
        if self.mode == Mode::Present {
            if self.presentation.pointer_command() == PresentCommand::Advance {
                self.next_slide(1);
            }
            *handled = true;
            return;
        }
        if self.mode == Mode::Overview {
            let (area, _, max_scroll) = self.compute_overview();
            // Scrollbar thumb → start a scroll drag.
            if let Some((_, thumb)) = scrollbar_rects(
                area,
                self.overview.scroll().clamp(0.0, max_scroll),
                max_scroll,
            ) && thumb.contains(p)
            {
                self.drag = Drag::ScrollOverview {
                    start_y: p.y,
                    orig_scroll: self.overview.scroll().clamp(0.0, max_scroll),
                };
                *handled = true;
                return;
            }
            if let Some(index) = self.overview.slide_at(self.rect, self.deck.slides.len(), p) {
                self.apply_hit(Hit::OpenSlide(index));
                *handled = true;
                return;
            }
            *handled = true;
            return;
        }
        self.on_press_editor(p, handled);
    }

    /// `on_press` in `Mode::Edit`: the shared rail's own context menu and the
    /// toolbar overflow menu are modal while open (handled first, ahead of
    /// all other chrome); otherwise buttons, the title, the overlay scrim,
    /// the thumbnail rail and the canvas are tried in pointer-priority order.
    fn on_press_editor(&mut self, p: Point, handled: &mut bool) {
        let chrome = self.compute_chrome();

        if self.on_press_editor_menus(&chrome, p, handled) {
            return;
        }

        // A click anywhere but the title itself commits an in-flight rename.
        if self.editing_title && !chrome.title_rect.contains(p) {
            self.editing_title = false;
        }

        // Buttons first (the hamburger toggle lives here, so it wins over the
        // scrim dismissal below).
        for b in &chrome.buttons {
            if b.rect.contains(p) {
                let hit = b.hit.clone();
                self.apply_hit(hit);
                *handled = true;
                return;
            }
        }
        // Deck title → inline rename (the title sits between the hamburger and
        // the centre tool group, so it never overlaps a button).
        if chrome.title_rect.contains(p) {
            self.begin_title_edit();
            *handled = true;
            return;
        }
        // Scrim dismissal: with an overlay flap open, a click in the dimmed body
        // (outside both panels) closes whichever flap is open — like tapping the
        // scrim in Adwaita's flap.
        if chrome.scrim.contains(p) && !chrome.rail.contains(p) && !chrome.inspector.contains(p) {
            let mut dismissed = false;
            if chrome.layout == SplitLayout::Overlay && self.rail_open() {
                self.rail_flap.close();
                dismissed = true;
            }
            if chrome.inspector_layout == SplitLayout::Overlay && self.inspector_open() {
                self.inspector_flap.close();
                dismissed = true;
            }
            if dismissed {
                *handled = true;
                return;
            }
        }
        // The slide-thumbnail rail. The shared PageSidebar owns selection and
        // the drag-to-reorder gesture: a press selects the slide and arms a
        // drag; if the pointer clears the threshold it becomes a live reorder
        // (ghost + reflow, committed on release), while a plain click that never
        // moves stays a select and — on a narrow window — reveals the chosen
        // slide by closing the overlay flap (deferred to `on_release` so a drag
        // doesn't close it). Sync the rail's geometry to this frame first.
        if chrome.rail_viewport.contains(p) {
            self.sync_page_rail();
            self.page_sidebar.on_press(p);
            self.apply_rail_events();
            self.rail_scroll = self.page_sidebar.scroll();
            *handled = true;
            return;
        }
        // Canvas: handle selection / drag.
        if chrome.slide_dest.inflate(12.0, 12.0).contains(p) {
            self.press_canvas(p, &chrome, handled);
            return;
        }
        // Clicked empty chrome → drop selection / exit text edit.
        if self.sel.is_some() || self.editing_text || self.editing_notes {
            self.sel = None;
            self.editing_text = false;
            self.editing_notes = false;
            *handled = true;
        }
    }

    /// The two modal-while-open menus a press can land on: the shared rail's
    /// own context menu, and the toolbar overflow dropdown. Returns `true`
    /// (and sets `*handled` as appropriate) when the press was consumed by
    /// either, so [`Self::on_press_editor`] should stop.
    fn on_press_editor_menus(&mut self, chrome: &Chrome, p: Point, handled: &mut bool) -> bool {
        // The shared rail's own context menu is modal while open: the press
        // picks one of its rows or dismisses it, ahead of all other chrome.
        if self.page_sidebar.menu_open() {
            self.sync_page_rail();
            self.page_sidebar.on_press(p);
            self.apply_rail_events();
            self.rail_scroll = self.page_sidebar.scroll();
            *handled = true;
            return true;
        }

        match self
            .toolbar_tools
            .on_press(&chrome.toolbar_tools, &chrome.buttons, p)
        {
            MenuPress::PassThrough => {}
            MenuPress::Consumed => {
                *handled = true;
                return true;
            }
            MenuPress::Action(hit) => {
                self.apply_hit(hit);
                *handled = true;
                return true;
            }
        }
        false
    }

    fn press_canvas(&mut self, p: Point, chrome: &Chrome, handled: &mut bool) {
        let dest = chrome.slide_dest;
        let s = chrome.scale;
        // Resize handle of the current selection?
        if let Some(el) = self.selected_element().cloned()
            && !self.editing_text
        {
            for (h, hr) in self.handles_of(dest, s, &el) {
                if hr.contains(p) {
                    self.drag = Drag::Resize {
                        id: el.id.clone(),
                        handle: h,
                        start: self.to_slide(dest, s, p),
                        orig: (el.x, el.y, el.w, el.h),
                    };
                    *handled = true;
                    return;
                }
            }
        }
        // Hit-test elements front-to-back.
        let mut picked: Option<Id> = None;
        if let Some(slide) = self.slide() {
            let mut z: Vec<&Element> = slide.elements.iter().collect();
            z.sort_by(|a, b| b.z.cmp(&a.z).then_with(|| b.id.cmp(&a.id)));
            for el in z {
                let er = render::element_dest(dest, s, el);
                if er.inflate(2.0, 2.0).contains(p) {
                    picked = Some(el.id.clone());
                    break;
                }
            }
        }
        if let Some(id) = picked {
            let is_text = self
                .slide()
                .and_then(|sl| sl.element(&id))
                .is_some_and(|e| e.kind == ElementKind::Text);
            // Double-click = same element, pressed again quickly and near
            // the first press.
            let double = self.last_click.as_ref().is_some_and(|(cid, t, cp)| {
                cid == &id && t.elapsed().as_millis() < 400 && cp.distance(p) < 6.0
            });
            self.last_click = Some((id.clone(), ui::time::now(), p));
            self.editing_notes = false;

            // Already editing this exact text box → a click inside it places
            // the caret (and arms a selection drag), never starts a box move,
            // so click-to-position and drag-select work like any text field.
            if self.editing_text && self.sel.as_deref() == Some(id.as_str()) {
                *handled = true;
                return;
            }

            self.sel = Some(id.clone());
            if double && is_text {
                // A deliberate double-click opens the text for editing and
                // selects the word under the point.
                self.begin_text_edit(false);
            } else {
                // Single press: select AND arm a move so a selected element
                // can be dragged on the very first grab (the reported bug was
                // that a second click on a selected box entered edit instead
                // of letting you move it).
                self.editing_text = false;
                // The element was just selected above, so this should always
                // resolve; if a concurrent edit removed it in between, simply
                // skip arming the drag (the selection itself already stuck).
                if let Some(el) = self.selected_element().cloned() {
                    self.drag = Drag::Move {
                        id,
                        start: self.to_slide(dest, s, p),
                        orig_x: el.x,
                        orig_y: el.y,
                    };
                }
            }
            *handled = true;
        } else {
            self.sel = None;
            self.editing_text = false;
            self.last_click = None;
            *handled = true;
        }
    }

    /// Hit-test the canvas elements front-to-back at screen point `p`.
    fn element_at(&self, chrome: &Chrome, p: Point) -> Option<Id> {
        let dest = chrome.slide_dest;
        let s = chrome.scale;
        let slide = self.slide()?;
        let mut z: Vec<&Element> = slide.elements.iter().collect();
        z.sort_by(|a, b| b.z.cmp(&a.z).then_with(|| b.id.cmp(&a.id)));
        for el in z {
            if render::element_dest(dest, s, el)
                .inflate(2.0, 2.0)
                .contains(p)
            {
                return Some(el.id.clone());
            }
        }
        None
    }

    /// Right-click opens a context menu — on a canvas element (front / back /
    /// duplicate / delete) or a slide thumbnail (new / duplicate / move /
    /// delete). The target is selected first so the menu's actions (which act
    /// on the current selection / slide) hit the intended object.
    fn on_right_press(&mut self, p: Point, handled: &mut bool) {
        *handled = true;
        self.ctx_menu.close();
        self.ctx_hits.clear();
        if self.mode != Mode::Edit {
            return;
        }
        let chrome = self.compute_chrome();
        // A slide thumbnail → the shared rail's own duplicate / move / delete
        // menu (only when it's actually in the visible viewport). The widget
        // selects the slide under the cursor and opens its menu; we apply the
        // emitted selection so the rest of the editor follows.
        if chrome.rail_viewport.contains(p) {
            self.sync_page_rail();
            if self.page_sidebar.on_right_press(p) {
                self.apply_rail_events();
                self.rail_scroll = self.page_sidebar.scroll();
                return;
            }
        }
        // A canvas element.
        if chrome.slide_dest.contains(p) {
            // Close any open rail menu first so the two never stack.
            self.page_sidebar.on_escape();
            if let Some(id) = self.element_at(&chrome, p) {
                self.sel = Some(id);
                self.editing_text = false;
                self.editing_notes = false;
                self.open_element_menu(p);
            }
        }
    }

    /// Build + open the context menu at `p`, mapping each row to the action it
    /// performs. `None` rows are separators. Clamps the 220px-wide menu so it
    /// stays fully on-screen.
    fn open_menu_at(&mut self, p: Point, rows: &[Option<(String, Hit)>]) {
        let mut menu = ContextMenu::new();
        let mut hits: Vec<Option<Hit>> = Vec::new();
        for row in rows {
            if let Some((label, hit)) = row {
                menu.push(MenuItem::action(label.clone(), || {}));
                hits.push(Some(hit.clone()));
            } else {
                menu.push(MenuItem::separator());
                hits.push(None);
            }
        }
        let x = (p.x.min(self.rect.x1 - 228.0)).max(self.rect.x0 + 4.0);
        menu.open_at([x as f32, p.y as f32]);
        let h = menu.get_rect().height();
        let y = (p.y.min(self.rect.y1 - 6.0 - h)).max(self.rect.y0 + 4.0);
        menu.open_at([x as f32, y as f32]);
        self.ctx_menu = menu;
        self.ctx_hits = hits;
    }

    fn open_element_menu(&mut self, p: Point) {
        self.open_menu_at(
            p,
            &[
                Some((tr("slides-a11y-bring-front"), Hit::BringFront)),
                Some((tr("slides-a11y-send-back"), Hit::SendBack)),
                None,
                Some((tr("slides-duplicate"), Hit::DuplicateElement)),
                Some((tr("slides-delete"), Hit::DeleteElement)),
            ],
        );
    }

    /// Forward an event to the open context menu and apply any chosen action.
    /// Returns `true` if a menu was open (and thus consumed the event).
    fn route_context_menu(&mut self, routed: &mut RoutedEvent<'_>) -> bool {
        if !self.ctx_menu.is_open() {
            return false;
        }
        if let WindowEvent::CursorMoved { position, .. } = routed.event {
            self.last_mouse = Point::new(
                position.x / routed.scale_factor,
                position.y / routed.scale_factor,
            );
        }
        let mut menu_handled = false;
        self.ctx_menu.handle_event(
            routed.event,
            &mut ui::widget::EventCtx {
                scale_factor: routed.scale_factor,
                modifiers: &routed.modifiers,
                state: routed.state,
                path: routed.path,
                handled: &mut menu_handled,
                focus_target: routed.focus_target,
                node_id: routed.node_id,
                taffy: routed.taffy,
            },
        );
        if let Some(idx) = self.ctx_menu.take_selection()
            && let Some(Some(hit)) = self.ctx_hits.get(idx).cloned()
        {
            self.apply_hit(hit);
        }
        true
    }

    /// Forward events to the inspector's embedded `ui` widgets (font-size
    /// field and fill picker) and fold any change into the CRDT. Returns
    /// `true` when the event was consumed (so `handle_event` should stop).
    /// The picker popover is modal while open; the font field only claims
    /// presses on its own rect and keystrokes while focused.
    fn route_inspector_widgets(&mut self, routed: &mut RoutedEvent<'_>) -> bool {
        if self.mode != Mode::Edit || self.drag.active() {
            return false;
        }
        let text_selected = self
            .selected_element()
            .is_some_and(|e| e.kind == ElementKind::Text);
        if !self.fill_picker_open && !text_selected {
            return false;
        }
        let chrome = self.compute_chrome();
        self.position_inspector_widgets(&chrome);

        if self.fill_picker_open {
            return self.route_fill_picker(routed);
        }

        if chrome.font_input_rect.is_none() {
            return false;
        }
        self.route_font_input(routed)
    }

    /// The fill [`ColorPicker`] popover: modal while open, so a routed event
    /// is always consumed. A press outside its panel closes it instead of
    /// reaching the field.
    fn route_fill_picker(&mut self, routed: &mut RoutedEvent<'_>) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = routed.event {
            self.last_mouse = Point::new(
                position.x / routed.scale_factor,
                position.y / routed.scale_factor,
            );
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = routed.event
        {
            let panel = self.fill_picker_panel_rect(self.fill_picker.get_rect());
            if !panel.contains(self.last_mouse) {
                Widget::set_focus(&mut self.fill_picker, false);
                self.fill_picker_open = false;
                return true;
            }
        }
        let mut h = false;
        self.fill_picker.handle_event(
            routed.event,
            &mut ui::widget::EventCtx {
                scale_factor: routed.scale_factor,
                modifiers: &routed.modifiers,
                state: routed.state,
                path: routed.path,
                handled: &mut h,
                focus_target: routed.focus_target,
                node_id: routed.node_id,
                taffy: routed.taffy,
            },
        );
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = routed.event
            && self.fill_picker.get_rect().contains(self.last_mouse)
        {
            Widget::set_focus(&mut self.fill_picker, true);
        }
        if let Some(col) = self.fill_picker.take_changed() {
            self.set_sel_str("fill", &ui::color::to_hex(col));
        }
        true
    }

    /// The font-size [`NumberInput`]: cursor motion is forwarded but never
    /// consumed (canvas hover still wants it), a press on the field focuses
    /// and forwards it, a press elsewhere commits + blurs, and keystrokes
    /// while focused edit it (Esc commits + blurs).
    fn route_font_input(&mut self, routed: &mut RoutedEvent<'_>) -> bool {
        match routed.event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(
                    position.x / routed.scale_factor,
                    position.y / routed.scale_factor,
                );
                let mut h = false;
                self.font_input.handle_event(
                    routed.event,
                    &mut ui::widget::EventCtx {
                        scale_factor: routed.scale_factor,
                        modifiers: &routed.modifiers,
                        state: routed.state,
                        path: routed.path,
                        handled: &mut h,
                        focus_target: routed.focus_target,
                        node_id: routed.node_id,
                        taffy: routed.taffy,
                    },
                );
                if let Some(v) = self.font_input.take_changed() {
                    self.set_sel_f64("font_size", v);
                }
                // Don't consume — canvas hover still wants the move.
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.font_input.get_rect().contains(self.last_mouse) {
                    let mut h = false;
                    self.font_input.handle_event(
                        routed.event,
                        &mut ui::widget::EventCtx {
                            scale_factor: routed.scale_factor,
                            modifiers: &routed.modifiers,
                            state: routed.state,
                            path: routed.path,
                            handled: &mut h,
                            focus_target: routed.focus_target,
                            node_id: routed.node_id,
                            taffy: routed.taffy,
                        },
                    );
                    Widget::set_focus(&mut self.font_input, true);
                    if let Some(v) = self.font_input.take_changed() {
                        self.set_sel_f64("font_size", v);
                    }
                    true
                } else if self.font_input.is_focused() {
                    // Press elsewhere commits + blurs, then falls through so the
                    // press can still hit whatever it landed on.
                    Widget::set_focus(&mut self.font_input, false);
                    if let Some(v) = self.font_input.take_changed() {
                        self.set_sel_f64("font_size", v);
                    }
                    false
                } else {
                    false
                }
            }
            WindowEvent::KeyboardInput { .. } if self.font_input.is_focused() => {
                self.route_font_input_key(routed)
            }
            _ => false,
        }
    }

    /// A keystroke while the font-size field is focused: Esc blurs it
    /// (committing any change); everything else edits it.
    fn route_font_input_key(&mut self, routed: &mut RoutedEvent<'_>) -> bool {
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = routed.event
        {
            Widget::set_focus(&mut self.font_input, false);
            if let Some(v) = self.font_input.take_changed() {
                self.set_sel_f64("font_size", v);
            }
            return true;
        }
        let mut h = false;
        self.font_input.handle_event(
            routed.event,
            &mut ui::widget::EventCtx {
                scale_factor: routed.scale_factor,
                modifiers: &routed.modifiers,
                state: routed.state,
                path: routed.path,
                handled: &mut h,
                focus_target: routed.focus_target,
                node_id: routed.node_id,
                taffy: routed.taffy,
            },
        );
        if let Some(v) = self.font_input.take_changed() {
            self.set_sel_f64("font_size", v);
        }
        true
    }

    fn on_release(&mut self, handled: &mut bool) {
        // A thumbnail drag-to-reorder: the shared rail commits the move (emitting
        // a Reorder we apply to the CRDT) or — if the press never crossed the
        // threshold into a real drag — nothing, in which case it stays a plain
        // select and, on a narrow window, reveals the chosen slide by closing
        // the overlay flap.
        if self.page_sidebar.is_pending_drag() {
            // Drop onto the red delete zone removes the dragged slide instead of
            // reordering it (guarded so the deck never drops to zero slides).
            if self.page_sidebar.is_dragging()
                && self
                    .slide_delete_zone(self.rail_rect())
                    .contains(self.last_mouse)
                && let Some(from) = self.page_sidebar.drag_from()
            {
                self.page_sidebar.on_escape(); // cancel the reorder cleanly
                self.page_sidebar.take_events();
                self.rail_scroll = self.page_sidebar.scroll();
                self.apply_rail_event(PageSidebarEvent::Delete(from));
                *handled = true;
                return;
            }
            let was_click = !self.page_sidebar.is_dragging();
            self.page_sidebar.on_release();
            self.apply_rail_events();
            self.rail_scroll = self.page_sidebar.scroll();
            if was_click && self.rail_layout() == SplitLayout::Overlay {
                self.rail_flap.close();
            }
            *handled = true;
            return;
        }
        // A scrollbar-thumb drag ends with no document write, in any mode.
        if matches!(self.drag, Drag::ScrollOverview { .. }) {
            self.drag = Drag::None;
            *handled = true;
            return;
        }
        if self.mode != Mode::Edit {
            return;
        }
        self.active_guides.clear();
        let drag = std::mem::replace(&mut self.drag, Drag::None);
        let id = match drag {
            Drag::Move { id, .. } | Drag::Resize { id, .. } => Some(id),
            // Scroll drags were already resolved above; `None` writes nothing.
            _ => None,
        };
        if let Some(id) = id
            && let Some(el) = self
                .deck
                .slides
                .get(self.cur_slide)
                .and_then(|s| s.element(&id))
                .cloned()
        {
            // The drag preview lived only in the in-memory deck; submit the
            // element's final rect once, on drop — never an edit per frame.
            self.submit(&actions::resize_element_action(
                self.cur_slide,
                id,
                el.x,
                el.y,
                el.w,
                el.h,
            ));
            *handled = true;
        }
    }

    /// Vertical mouse-wheel scrolling for the two scrollable surfaces: the
    /// edit-mode thumbnail rail (only while the pointer is over it) and the
    /// slide-sorter overview grid.
    fn on_wheel(&mut self, delta: MouseScrollDelta, scale_factor: f64, handled: &mut bool) {
        // Positive wheel deltas scroll the content up (toward the start), so we
        // subtract from the offset.
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y) * WHEEL_LINE_PX,
            MouseScrollDelta::PixelDelta(pos) => pos.y / scale_factor,
        };
        if dy == 0.0 {
            return;
        }
        match self.mode {
            Mode::Edit => {
                if self.rail_rect().contains(self.last_mouse) {
                    // The shared rail owns the scroll: sync its geometry, feed it
                    // the wheel (its `on_wheel` takes +down), and mirror back.
                    self.sync_page_rail();
                    if self.page_sidebar.on_wheel(-dy) {
                        self.rail_scroll = self.page_sidebar.scroll();
                        *handled = true;
                    }
                } else if self.inspector_open() && self.inspector_rect().contains(self.last_mouse) {
                    // The inspector's scroll model clamps to the content
                    // extent the last frame handed it; a wheel at either end
                    // changes nothing and is left for whoever is below.
                    let before = self.inspector_scroll.offset_y();
                    self.inspector_scroll.scroll_by(0.0, -dy as f32);
                    if self.inspector_scroll.offset_y().to_bits() != before.to_bits() {
                        *handled = true;
                    }
                }
            }
            Mode::Overview => {
                if self.overview.wheel(self.rect, self.deck.slides.len(), dy) {
                    *handled = true;
                }
            }
            Mode::Present => {}
        }
    }

    fn on_key(&mut self, key: &Key, _text: Option<&str>, modifiers: Modifiers, handled: &mut bool) {
        let mods = modifiers.state();
        let ctrl = mods.control_key() || mods.super_key();

        // Escape aborts an in-flight thumbnail reorder drag (restoring the
        // original order — the model is only mutated on a committed drop) or
        // closes the shared rail's open context menu.
        if matches!(key, Key::Named(NamedKey::Escape))
            && (self.page_sidebar.is_pending_drag() || self.page_sidebar.menu_open())
        {
            self.page_sidebar.on_escape();
            self.rail_scroll = self.page_sidebar.scroll();
            *handled = true;
            return;
        }

        // The help overlay owns every key while visible. It can be opened from
        // any non-text context; active editors reserve `?` for typing.
        let allow_help = !self.editing_text && !self.editing_notes && !self.editing_title;
        if self.help_overlay.on_key(key, allow_help) {
            *handled = true;
            return;
        }

        // Esc dismisses an open overlay flap first (when not editing text).
        if self.mode == Mode::Edit
            && !self.editing_text
            && !self.editing_notes
            && matches!(key, Key::Named(NamedKey::Escape))
        {
            let mut dismissed = false;
            if self.rail_layout() == SplitLayout::Overlay && self.rail_open() {
                self.rail_flap.close();
                dismissed = true;
            }
            if self.inspector_layout() == SplitLayout::Overlay && self.inspector_open() {
                self.inspector_flap.close();
                dismissed = true;
            }
            if dismissed {
                *handled = true;
                return;
            }
        }

        // Overview (slide-sorter) navigation.
        if self.mode == Mode::Overview {
            self.on_key_overview(key, handled);
            return;
        }

        // Present-mode keys.
        if self.mode == Mode::Present {
            self.on_key_present(key, handled);
            return;
        }

        // Text editing intercepts most keys. Speaker notes and title rename are
        // consumed earlier by their shared `TextInput`s; they reach here only
        // for shortcuts the fields intentionally leave to the toolkit (notably
        // global undo/redo).
        if self.editing_text || self.editing_notes || self.editing_title {
            self.on_key_editing_text(key, handled);
            return;
        }

        // Editor command keys.
        self.on_key_editor_commands(key, mods, ctrl, handled);
    }

    /// Overview (slide-sorter) key navigation.
    fn on_key_overview(&mut self, key: &Key, handled: &mut bool) {
        // Escape / Enter / "g" all return to the editor — one condition
        // instead of two identical match arms.
        let exits_overview = matches!(key, Key::Named(NamedKey::Escape | NamedKey::Enter))
            || matches!(key, Key::Character(c) if c.eq_ignore_ascii_case("g"));
        if exits_overview {
            self.mode = Mode::Edit;
            *handled = true;
            return;
        }
        match key {
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) => {
                if self.cur_slide + 1 < self.deck.slides.len() {
                    self.cur_slide += 1;
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) => {
                self.cur_slide = self.cur_slide.saturating_sub(1);
                *handled = true;
            }
            Key::Named(NamedKey::Home) => {
                self.cur_slide = 0;
                *handled = true;
            }
            Key::Named(NamedKey::End) => {
                self.cur_slide = self.deck.slides.len().saturating_sub(1);
                *handled = true;
            }
            // Present straight from the highlighted slide.
            Key::Character(c) if c.eq_ignore_ascii_case("p") => {
                self.enter_present();
                *handled = true;
            }
            _ => {}
        }
    }

    /// Present-mode keys, via the shared [`Presentation`] command mapping
    /// plus the digit-jump shortcut it doesn't own.
    fn on_key_present(&mut self, key: &Key, handled: &mut bool) {
        if let Some(command) = self.presentation.handle_key(key) {
            match command {
                PresentCommand::None => {}
                PresentCommand::Advance => self.next_slide(1),
                PresentCommand::Reverse => self.next_slide(-1),
                PresentCommand::First => self.goto_slide(0),
                PresentCommand::Last => {
                    self.goto_slide(self.deck.slides.len().saturating_sub(1));
                }
                PresentCommand::Exit => self.exit_present(),
            }
            *handled = true;
        } else if let Key::Character(c) = key
            && let Some(d) = c.chars().next().and_then(|ch| ch.to_digit(10))
            && d >= 1
        {
            self.goto_slide((d as usize) - 1);
            *handled = true;
        }
    }

    /// Keys the focused text (a text box, the speaker notes, the deck-title
    /// rename) left alone: Escape ends editing; everything else (global
    /// undo and redo) falls through to the toolkit.
    fn on_key_editing_text(&mut self, key: &Key, handled: &mut bool) {
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.end_text_edit();
            self.editing_notes = false;
            self.editing_title = false;
            *handled = true;
        }
    }

    /// Editor command keys (nothing focused for text edit): selection nav,
    /// nudge, and letter shortcuts.
    fn on_key_editor_commands(
        &mut self,
        key: &Key,
        mods: winit::keyboard::ModifiersState,
        ctrl: bool,
        handled: &mut bool,
    ) {
        if self.on_key_editor_nav(key, mods, handled) {
            return;
        }
        if let Key::Character(c) = key {
            self.on_key_editor_shortcut(c, ctrl, handled);
        }
    }

    /// Escape/Delete/Enter/Tab plus the nudge arrows. Returns `true` when
    /// `key` matched one of these (so [`Self::on_key_editor_commands`]
    /// should stop rather than also trying the letter shortcuts).
    fn on_key_editor_nav(
        &mut self,
        key: &Key,
        mods: winit::keyboard::ModifiersState,
        handled: &mut bool,
    ) -> bool {
        match key {
            Key::Named(NamedKey::Escape) => {
                if self.sel.is_some() {
                    self.sel = None;
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                if self.sel.is_some() {
                    self.delete_element();
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Enter) => {
                if self
                    .selected_element()
                    .is_some_and(|e| e.kind == ElementKind::Text)
                {
                    self.begin_text_edit(false);
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Tab) => {
                self.cycle_selection();
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.nudge(if mods.shift_key() { -1.0 } else { -12.0 }, 0.0);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.nudge(if mods.shift_key() { 1.0 } else { 12.0 }, 0.0);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.nudge(0.0, if mods.shift_key() { -1.0 } else { -12.0 });
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.nudge(0.0, if mods.shift_key() { 1.0 } else { 12.0 });
                *handled = true;
            }
            _ => return false,
        }
        true
    }

    /// Single-letter (and Ctrl+letter) editor shortcuts: new slide, present,
    /// overview, insert tools, duplicate, restack, and Ctrl+S/O document
    /// open/save.
    fn on_key_editor_shortcut(
        &mut self,
        c: &winit::keyboard::SmolStr,
        ctrl: bool,
        handled: &mut bool,
    ) {
        let lc = c.to_lowercase();
        // Ctrl+S / Ctrl+O — save / open a LibreOffice `.odp` via the
        // host file chooser.
        if ctrl && lc == "s" {
            self.request_odp_save();
            *handled = true;
            return;
        }
        if ctrl && lc == "o" {
            self.request_odp_open();
            *handled = true;
            return;
        }
        if ctrl && lc == "d" {
            self.duplicate_element();
            *handled = true;
            return;
        }
        match lc.as_str() {
            "n" => {
                self.new_slide();
                *handled = true;
            }
            "p" => {
                self.enter_present();
                *handled = true;
            }
            "g" => {
                self.mode = Mode::Overview;
                self.sel = None;
                self.editing_text = false;
                *handled = true;
            }
            "t" => {
                self.insert_element(ElementKind::Text);
                *handled = true;
            }
            "r" => {
                self.insert_element(ElementKind::Rect);
                *handled = true;
            }
            "o" => {
                self.insert_element(ElementKind::Ellipse);
                *handled = true;
            }
            "l" => {
                self.insert_element(ElementKind::Line);
                *handled = true;
            }
            "d" => {
                self.duplicate_element();
                *handled = true;
            }
            "]" => {
                self.restack(true);
                *handled = true;
            }
            "[" => {
                self.restack(false);
                *handled = true;
            }
            _ => {}
        }
    }

    fn cycle_selection(&mut self) {
        let Some(slide) = self.slide() else { return };
        if slide.elements.is_empty() {
            return;
        }
        let mut z: Vec<&Element> = slide.elements.iter().collect();
        z.sort_by(|a, b| a.z.cmp(&b.z).then_with(|| a.id.cmp(&b.id)));
        let ids: Vec<Id> = z.iter().map(|e| e.id.clone()).collect();
        let Some(first) = ids.first() else { return };
        let next = match &self.sel {
            Some(cur) => {
                let i = ids.iter().position(|x| x == cur).unwrap_or(usize::MAX);
                let next_i = i.checked_add(1).map_or(0, |next| next % ids.len());
                ids.get(next_i).unwrap_or(first).clone()
            }
            None => first.clone(),
        };
        self.sel = Some(next);
        self.editing_text = false;
    }
}

impl SlidesApp {
    /// Start editing the selected text box's words; `select_all` selects
    /// them all once the editor opens.
    fn begin_text_edit(&mut self, select_all: bool) {
        self.editing_text = true;
        self.editing_notes = false;
        self.text_edit_select_all = select_all;
    }

    /// Stop editing the text box: its editor closes on the next sync.
    fn end_text_edit(&mut self) {
        self.editing_text = false;
        self.text_drag = false;
        self.text_editor = None;
    }

    /// The text box being edited and its slide, when there is one.
    fn editing_text_box(&self) -> Option<(String, Element)> {
        if !self.editing_text || self.mode != Mode::Edit {
            return None;
        }
        let slide = self.slide()?;
        let element = slide.element(self.sel.as_deref()?)?;
        (element.kind == ElementKind::Text).then(|| (slide.id.clone(), element.clone()))
    }

    /// Open the editor for the text box being edited, close it when editing
    /// ended, and bring it up to date with the document.
    fn ensure_text_editor(&mut self, doc: &mut AutoCommit) {
        let Some((slide_id, element)) = self.editing_text_box() else {
            self.text_editor = None;
            self.text_drag = false;
            return;
        };
        let current = self
            .text_editor
            .as_ref()
            .is_some_and(|editor| editor.slide_id == slide_id && editor.element_id == element.id);
        if !current {
            self.text_editor = Some(TextBoxEditor::open(
                doc,
                &slide_id,
                &element,
                &self.dispatch,
            ));
        }
        let select_all = std::mem::take(&mut self.text_edit_select_all);
        if let Some(editor) = self.text_editor.as_mut() {
            editor.host.sync(doc, &automerge::ROOT);
            if select_all {
                editor.host.widget_mut().editor_mut().select_all();
            }
        }
    }

    /// Give the text box being edited its keyboard and pointer events.
    /// Returns whether the event was the box's.
    fn route_text_edit(&mut self, routed: &mut RoutedEvent<'_>, handled: &mut bool) -> bool {
        if self.editing_text_box().is_none() {
            return false;
        }
        let keyboard = matches!(
            routed.event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        );
        let pointer = match routed.event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.text_edit_box.contains(self.last_mouse),
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.text_drag,
            _ => false,
        };
        // The pointer's position always reaches the box, so a press lands
        // where it was last seen.
        let moved = matches!(routed.event, WindowEvent::CursorMoved { .. });
        if !keyboard && !pointer && !moved {
            return false;
        }
        self.ensure_text_editor(routed.state);
        let Some(editor) = self.text_editor.as_mut() else {
            return false;
        };
        let mut consumed = false;
        editor.host.handle_event(
            routed.event,
            routed.scale_factor,
            &routed.modifiers,
            routed.state,
            routed.path,
            &mut consumed,
        );
        editor.host.sync(routed.state, routed.path);
        let outcome = editor.host.widget_mut().take_outcome();
        match routed.event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => self.text_drag = true,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => self.text_drag = false,
            _ => {}
        }
        if outcome.is_some() {
            // Escape or Ctrl+Enter: done with the words, the box stays selected.
            debug_assert!(matches!(
                outcome,
                Some(FieldOutcome::Submit | FieldOutcome::Cancel)
            ));
            self.end_text_edit();
            consumed = true;
        }
        if keyboard || pointer {
            // Keys the editor left alone (global undo) fall through to the host.
            *handled |= consumed || pointer;
            return true;
        }
        false
    }

    /// Paint the text box being edited: its editor, laid out at the slide's
    /// size and drawn where the box is, in the box's colour.
    fn draw_text_editor(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
        pal: &Palette,
    ) {
        let Some((_, element)) = self.editing_text_box() else {
            return;
        };
        let background = self.slide().map(|slide| slide.background);
        let Some(editor) = self.text_editor.as_mut() else {
            return;
        };
        if editor.element_id != element.id {
            return;
        }
        let palette = background.map_or(*pal, |background| pal.on(background));
        let color = palette.resolve_role(&element.fill).unwrap_or(palette.text);
        let surface = editor.host.widget_mut();
        surface.editor_mut().set_body_size(element.font_size as f32);
        surface
            .editor_mut()
            .set_text_align(render::text_align(element.align));
        editor.host.set_text_color(Some(color));
        let content_h = editor
            .host
            .layout(render::text_box_rect(&element), theme, tr);
        editor.host.set_transform(render::text_box_transform(
            chrome.slide_dest,
            chrome.scale,
            &element,
            content_h,
        ));
        editor.host.draw(scene, t, tr, theme, &PresenceCtx::empty());
        self.text_edit_box = render::element_dest(chrome.slide_dest, chrome.scale, &element);
    }
}

/// The inspector's "Custom · #rrggbb" fill button face.
fn custom_colour_hex(hex: &str) -> String {
    tr_args("slides-custom-colour-hex", &[("hex", ArgValue::from(hex))])
}

#[cfg(test)]
#[path = "app/bench.rs"]
mod bench;

#[cfg(test)]
#[path = "app/tests_ops.rs"]
mod tests_ops;

#[cfg(test)]
#[path = "app/tests_present.rs"]
mod tests_present;

#[cfg(test)]
#[path = "app/tests_layout.rs"]
mod tests_layout;

#[cfg(test)]
#[path = "app/tests_pointer.rs"]
mod tests_pointer;

#[cfg(test)]
#[path = "app/tests_text.rs"]
mod tests_text;

#[cfg(test)]
#[path = "app/tests_files.rs"]
mod tests_files;
