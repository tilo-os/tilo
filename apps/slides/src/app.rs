//! Root widget for the Slides app.
//!
//! `SlidesApp` is a single full-window widget that lays out and paints the
//! whole editor itself (toolbar, thumbnail rail, slide canvas, inspector)
//! plus a full-screen present mode. The deck lives in the shared CRDT
//! ([`crate::doc::SlidesDoc`]); this widget keeps an in-memory mirror that
//! is reloaded from the document every `sync`, so undo/redo and live
//! collaboration work without extra plumbing. View state — current slide,
//! selection, mode — is per-peer and stays local.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};

use automerge::{AutoCommit, ObjId, ReadDoc};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::icons::{Icon, draw_icon};
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_edit::{EditCore, Motion};
use ui::text_render::{TextRenderer, TextWeight};
use ui::text_selection::{
    Selection, clipboard_copy, clipboard_paste, next_word_boundary_chars, prev_word_boundary_chars,
    word_boundaries_chars,
};
use ui::theme::Theme;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    ColorPicker, DEFAULT_BREAKPOINT, FlapState, NumberInput, PageSidebar, PageSidebarEvent,
    Scrollable, SplitLayout, TextInput, Widget,
};

use crate::actions;
use crate::assets::ImageStore;
use crate::chrome;
use crate::doc::SlidesDoc;
use crate::help_overlay::HelpOverlay;
use crate::model::{
    Background, Deck, Element, ElementKind, FILL_ROLES, Id, Layout, Palette, PaletteKind, SLIDE_H,
    SLIDE_W, STROKE_ROLES, Slide, TextAlign, new_id, seed_deck, starter_deck,
};
use crate::overview_grid::{OverviewGrid, scroll_from_thumb_drag, scrollbar_rects};
use crate::presentation::{Blank, Command as PresentCommand, Presentation, PresentationFrame};
use crate::render;
use crate::strings::{tr, tr_args};
use crate::toolbar_tools::{MenuPress, ToolbarTools, ToolbarToolsLayout};
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
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(64 * 1024);
    file.take((MAX_PRESENTATION_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PRESENTATION_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "presentation file is larger than 64 MiB",
        ));
    }
    Ok(bytes)
}

fn load_presentation_from_path(path: &std::path::Path) -> Result<Deck, String> {
    let bytes = read_presentation_file(path).map_err(|error| error.to_string())?;
    if is_pptx(path) {
        crate::pptx::read_pptx(&bytes).map_err(|error| error.to_string())
    } else {
        crate::odp::read_odp(&bytes).map_err(|error| error.to_string())
    }
}

fn save_presentation_to_path(deck: &Deck, path: &std::path::Path) -> Result<(), String> {
    let (path, bytes) = if is_pptx(path) {
        (
            path.to_path_buf(),
            crate::pptx::write_pptx(deck).map_err(|error| error.to_string())?,
        )
    } else {
        (
            ensure_odp_ext(path),
            crate::odp::write_odp(deck).map_err(|error| error.to_string())?,
        )
    };
    std::fs::write(path, bytes).map_err(|error| error.to_string())
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

struct DocumentWorker {
    tx: SyncSender<DocumentJob>,
    rx: Receiver<DocumentJobResult>,
}

impl DocumentWorker {
    fn new() -> Result<Self, String> {
        let (tx, jobs) = std::sync::mpsc::sync_channel(1);
        let (results, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("slides-document-io".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    let result = match job {
                        DocumentJob::Open { generation, path } => DocumentJobResult::Open {
                            generation,
                            result: load_presentation_from_path(&path),
                        },
                        DocumentJob::Save {
                            generation,
                            path,
                            deck,
                        } => DocumentJobResult::Save {
                            generation,
                            result: save_presentation_to_path(&deck, &path),
                        },
                    };
                    if results.send(result).is_err() {
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

#[cfg(test)]
fn inject_doc_for_test(outcome: DocOutcome) -> DocPortalChannel {
    let (outcome, save) = match outcome {
        DocOutcome::Open(path) => (PortalOutcome::Opened(path), false),
        DocOutcome::Save(path) => (PortalOutcome::Opened(path), true),
        DocOutcome::Cancelled => (PortalOutcome::Cancelled, false),
        DocOutcome::Failed(error) => (PortalOutcome::Failed(error), false),
    };
    DocPortalChannel {
        rx: ui::portal::inject_for_test(outcome),
        save,
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

/// Text metrics and placement shared by caret hit-testing, selection geometry,
/// and caret painting for one text box.
#[derive(Clone, Copy)]
struct TextBoxStyle {
    rect: Rect,
    font_px: f32,
    weight: TextWeight,
    italic: bool,
    align: TextAlign,
    center_vertically: bool,
}

impl TextBoxStyle {
    fn for_element(element: &Element, rect: Rect, scale: f64) -> Self {
        Self {
            rect,
            font_px: (element.font_size * scale).max(1.0) as f32,
            weight: if element.bold {
                TextWeight::Bold
            } else {
                TextWeight::Regular
            },
            italic: element.italic,
            align: element.align,
            center_vertically: true,
        }
    }
}

/// Mutable rendering resources forwarded together through editor paint layers.
struct PaintContext<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    theme: &'a Theme,
}

/// Normalized keyboard input used by the on-canvas text editor.
struct EditingKey<'a> {
    key: &'a Key,
    text: Option<&'a str>,
    modifiers: winit::keyboard::ModifiersState,
    control: bool,
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
    deck: Deck,
    rect: Rect,
    node: Option<NodeId>,
    seeded: bool,
    bootstrapped: bool,

    mode: Mode,
    canonical_deck: Option<(ObjId, Deck)>,
    cur_slide: usize,
    sel: Option<Id>,
    editing_text: bool,
    caret: usize,
    editing_notes: bool,
    /// Selection anchor (char index) for the active on-canvas text editor.
    /// `sel_anchor == active_caret()` means "no live selection"; the
    /// pair is a [`ui::text_selection::Selection`] over the CRDT-backed text, so
    /// the on-canvas editor gets the same selection semantics as every other
    /// `ui` text field (shift+arrow, word-wise, drag-select, select-all,
    /// clipboard) without a per-app text buffer. Reset to the caret whenever an
    /// editor is (re)entered.
    sel_anchor: usize,
    /// Inline deck-title rename in progress (toolbar title clicked). Typing
    /// splices into the CRDT title; Enter/Esc/clicking away commits.
    editing_title: bool,
    title_caret: usize,
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
    last_click: Option<(Id, std::time::Instant, Point)>,
    /// A pending click that should place (or, when `true`, shift-extend) the
    /// text caret. Resolved on the next `draw`, where the `TextRenderer` is
    /// available to hit-test a point against the wrapped text layout. `bool` is
    /// "extend the current selection" (a shift-click or a live drag).
    caret_click: Option<(Point, bool)>,
    /// A live text-selection drag is in progress inside the editing text box
    /// (mouse held down after a press). While set, cursor motion extends the
    /// selection from its anchor.
    text_drag: bool,
    /// A pending double-click inside the editing text box that should select the
    /// word under the point. Resolved on the next `draw` (needs text measuring).
    select_word_click: Option<Point>,
    needs_commit: bool,

    /// Framework-owned action dispatch (undo boundary + executor + last-action
    /// record). Every structural / geometric / stylistic deck mutation goes
    /// through it as a [`SlidesAction`](crate::actions::SlidesAction); flowing
    /// text edits stay on the native CRDT splice path (see [`crate::actions`]).
    actions: ui::ActionHost,

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
    last_flap_tick: std::time::Instant,
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

    /// Complete, incremental Ctrl+K index. Cloning its provider handle is
    /// cheap; rebuilding a large deck's postings each frame is not.
    search_provider: crate::search_provider::SlideSearchProvider,

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
    /// Directly bound to the current slide's Automerge notes text while open.
    /// The shared editor owns multiline layout, selection, clipboard, scrolling
    /// and IME; `editing_notes` only owns the inspector's open state.
    notes_input: TextInput,
    /// Shared headless caret/selection/edit core for the on-canvas text box.
    /// The canvas keeps its own typography and geometry, but no longer owns
    /// the text-editing semantics.
    element_edit_core: EditCore,
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

impl Default for SlidesApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SlidesApp {
    /// A live launch opens on a single blank starter slide — the deck's real
    /// source of truth is the shared CRDT document, which persists across
    /// sessions. We deliberately do NOT auto-populate a fabricated multi-slide
    /// deck here; the rich sample deck is reserved for [`SlidesApp::demo`]
    /// (the `--shots` capture path and tests).
    pub fn new() -> Self {
        Self::build(starter_deck())
    }

    /// Root pre-loaded with the hermetic sample deck, optionally jumped to a
    /// named screen (`overview` / `present` / `presenter` / `help` / …). Used
    /// only by the `--shots` capture path and tests, so a live launch never
    /// shows fabricated content.
    pub fn demo(scenario: Option<&str>) -> Self {
        let mut app = Self::build(seed_deck());
        let env = std::env::var("SLIDES_SHOT").ok();
        if let Some(s) = scenario.or(env.as_deref()) {
            app.apply_scenario(s);
        }
        app
    }

    fn build(deck: Deck) -> Self {
        let search_provider = crate::search_provider::SlideSearchProvider::new(&deck);
        let (document_worker, document_worker_error) = match DocumentWorker::new() {
            Ok(worker) => (Some(worker), None),
            Err(error) => (None, Some(error)),
        };
        SlidesApp {
            deck,
            rect: Rect::ZERO,
            node: None,
            seeded: false,
            bootstrapped: false,
            mode: Mode::Edit,
            canonical_deck: None,
            cur_slide: 1,
            sel: None,
            editing_text: false,
            caret: 0,
            editing_notes: false,
            sel_anchor: 0,
            editing_title: false,
            title_caret: 0,
            presentation: Presentation::default(),
            presentation_target: ui::fullscreen::FullscreenTarget::Largest,
            help_overlay: HelpOverlay::default(),
            active_guides: Vec::new(),
            last_mouse: Point::ZERO,
            drag: Drag::None,
            page_sidebar: PageSidebar::new(SLIDE_H / SLIDE_W).with_panel(false),
            last_click: None,
            caret_click: None,
            text_drag: false,
            select_word_click: None,
            needs_commit: false,
            actions: ui::ActionHost::new(),
            rail_flap: FlapState::open_when_docked(),
            inspector_flap: FlapState::open_when_docked(),
            inspector_breakpoint: INSPECTOR_BREAKPOINT,
            last_flap_tick: std::time::Instant::now(),
            flap_uninitialised: true,
            text_scale: 1.0,
            breakpoint: NARROW_BREAKPOINT,
            deck_rev: 0,
            search_provider,
            images: ImageStore::new(),
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
            notes_input: TextInput::local()
                .multiline(true)
                .with_plain_chrome()
                .with_font_size(12.5)
                .with_id("slides-speaker-notes"),
            element_edit_core: EditCore::new(),
            demo_menu: None,
            demo_reorder: false,
            demo_delete_zone: false,
        }
    }

    /// Jump the (sample) app to a named screen for a headless `--shots`
    /// capture. No effect on a normal launch — only [`SlidesApp::demo`] and
    /// the `--scenario` flag call this.
    fn apply_scenario(&mut self, shot: &str) {
        self.apply_scenario_layout(shot);
        self.apply_scenario_content(shot);
        // Scenarios may deliberately swap or extend the in-memory deck before
        // the first CRDT seed. Keep Ctrl+K's cached durable index truthful to
        // the exact screen the capture (and accessibility harness) sees.
        self.search_provider.refresh(&self.deck);
    }

    /// The `--shots` scenarios that only pick a mode / viewport preference —
    /// no deck or selection change.
    fn apply_scenario_layout(&mut self, shot: &str) {
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
            // A long deck so the rail overflows its viewport — proves the rail
            // scrolls (scrolled to the end here) and the pinned Add/Dup/Del row
            // is never overlapped by thumbnails.
            "many" => self.fill_long_deck(),
            // Same long deck, but with the rail summoned open, so the scrolling
            // thumbnail list is visible at a narrow (overlay-flap) width.
            "many_rail" => {
                self.fill_long_deck();
                self.rail_flap.open();
            }
            // Overview scrolled to its end, proving the last slide is reachable.
            "overview_end" => {
                self.mode = Mode::Overview;
                self.overview.set_scroll(1.0e6);
            }
            // Narrow-layout captures (rendered at a phone-ish viewport, so the
            // panels resolve to overlay flaps from the width). We only record a
            // *preference* here and leave `flap_uninitialised` set, so the
            // first-frame snap still resolves the OTHER flap to its default —
            // otherwise it would show a one-frame half-open transient.
            "narrow" => self.rail_flap.open(),
            "narrow_closed" => self.rail_flap.close(),
            // Narrow layout with the inspector flap summoned open.
            "inspector" => self.inspector_flap.open(),
            // "edit" (and anything unknown) keeps the default editor screen.
            "empty" => {
                self.deck = starter_deck();
                self.cur_slide = 0;
                self.sel = None;
                self.bootstrapped = true;
            }
            _ => {}
        }
    }

    /// The `--shots` scenarios that seed deck content, selection or an
    /// in-progress edit/menu/drag so the capture shows something a bare mode
    /// switch can't.
    fn apply_scenario_content(&mut self, shot: &str) {
        match shot {
            // The inline deck-title rename in progress (title focused, caret in
            // the toolbar title field).
            "rename" => {
                self.cur_slide = 1;
                self.begin_title_edit();
                // Show a live selection so the capture proves the TextInput's
                // real text selection (double-click word / Shift+Arrow / drag).
                self.title_input.set_selection(0, 9);
                self.bootstrapped = true;
            }
            // The speaker-notes editor focused, with the caret shown in the
            // notes field (previously no caret was rendered at all).
            "notes_edit" => {
                self.cur_slide = 1;
                self.sel = None;
                self.editing_notes = true;
                self.bootstrapped = true;
            }
            // The docked, nothing-selected inspector (slide-level properties:
            // layout, background, and the deck-theme pills). Captures the pills
            // at the default width to prove every theme name stays readable.
            "slide_props" => {
                self.cur_slide = 1;
                self.sel = None;
                self.bootstrapped = true;
            }
            // The toolbar overflow ("More") menu, opened — proves every insert
            // tool stays pointer-reachable even when the bar can't show them all.
            "toolbar_menu" => {
                self.toolbar_tools.set_open(true);
                self.sel = None;
                self.bootstrapped = true;
            }
            // The image-insert tool now defers to the host portal chooser
            // (HIG §5) — there is no in-app picker surface to capture, so this
            // scenario just requests it (a no-op in a headless shot with no
            // portal running) and otherwise shows the plain editor.
            "picker" => self.request_image_open(),
            // A slide carrying a real decoded raster, to prove the image
            // pipeline in a headless screenshot.
            "image" => self.apply_scenario_image(),
            // The first frame after image insertion has an intentional loading
            // placeholder while decoding runs off-thread; it is a real editor
            // state, captured here without pretending a bitmap is ready.
            "image_loading" => self.apply_scenario_loading_image(),
            // This is the same concise failure toast `drain_doc_portal` shows
            // after a decoder rejects a user-selected presentation.
            "open_error" => {
                self.io_toast = Some((tr("slides-toast-open-failed"), ToastKind::Error));
            }
            // Right-click context menus (opened on the first drawn frame once
            // the layout rect is known — see `draw`).
            "element_menu" => self.apply_scenario_element_menu(),
            "slide_menu" => {
                self.cur_slide = 1;
                self.sel = None;
                self.demo_menu = Some(DemoMenu::Slide);
                self.bootstrapped = true;
            }
            // A live thumbnail reorder drag mid-flight — the grabbed slide
            // lifted out as a floating ghost with the list reflowed to open the
            // drop gap. Staged on the first drawn frame (see `draw`).
            "reorder" => {
                self.cur_slide = 0;
                self.sel = None;
                self.demo_reorder = true;
                self.bootstrapped = true;
            }
            // An on-canvas text box in edit mode with a live selection over the
            // first word — proves the shared caret / selection core.
            "edit_text" => self.apply_scenario_edit_text(),
            // A slide thumbnail dragged onto the red dashed delete zone at the
            // foot of the rail (armed) — proves the drag-to-delete affordance.
            // Staged on the first drawn frame (see `draw`).
            "delete_zone" => {
                self.cur_slide = 0;
                self.sel = None;
                self.demo_delete_zone = true;
                self.bootstrapped = true;
            }
            // The inspector with the font-size NumberInput (text selected) and
            // the fill ColorPicker popover open — the two adopted primitives.
            "inspector_widgets" => self.apply_scenario_inspector_widgets(),
            // "edit" (and anything unknown) keeps the default editor screen.
            _ => {}
        }
    }

    /// `element_menu` scenario body: select slide 2's first element and stage
    /// its context menu.
    fn apply_scenario_element_menu(&mut self) {
        self.cur_slide = 1;
        self.sel = self
            .deck
            .slides
            .get(1)
            .and_then(|s| s.elements.first().map(|e| e.id.clone()));
        self.demo_menu = Some(DemoMenu::Element);
        self.bootstrapped = true;
    }

    /// `inspector_widgets` scenario body: select slide 2's first text element
    /// and open the fill colour picker popover.
    fn apply_scenario_inspector_widgets(&mut self) {
        self.cur_slide = 1;
        self.inspector_flap.open();
        self.sel = self.deck.slides.get(1).and_then(|s| {
            s.elements
                .iter()
                .find(|e| e.kind == ElementKind::Text)
                .map(|e| e.id.clone())
        });
        self.fill_picker_open = true;
        self.fill_picker_needs_init = true;
        self.bootstrapped = true;
    }

    /// `image` scenario body: write a temp raster, decode it synchronously
    /// (the single offscreen frame has no worker-pump), then place it on a
    /// dedicated slide.
    fn apply_scenario_image(&mut self) {
        if let Some(path) = write_demo_image() {
            let Some(src) = crate::assets::import_asset_file(&path) else {
                return;
            };
            self.images.load_blocking(&src);
            let mut s = Slide::new(Layout::TitleContent);
            s.add(
                Element::text("Site photo", 96.0, 70.0, 1088.0, 90.0, 56.0).with(|e| e.bold = true),
            );
            s.add(
                Element::shape(ElementKind::Rect, 96.0, 168.0, 150.0, 8.0).with(|e| {
                    e.fill = "accent".into();
                    e.radius = 4.0;
                }),
            );
            s.add(Element::image(&src, 96.0, 200.0, 1088.0, 420.0));
            self.deck.slides.push(s);
            self.cur_slide = self.deck.slides.len() - 1;
            self.sel = None;
            // Keep this slide selected — skip the slide-2 view bootstrap.
            self.bootstrapped = true;
        }
    }

    fn apply_scenario_loading_image(&mut self) {
        let mut slide = Slide::new(Layout::TitleContent);
        slide.add(
            Element::text("Importing photo", 96.0, 70.0, 1088.0, 90.0, 56.0)
                .with(|element| element.bold = true),
        );
        slide.add(Element::image(
            "asset:blake3:0000000000000000000000000000000000000000000000000000000000000000",
            96.0,
            200.0,
            1088.0,
            420.0,
        ));
        self.deck.slides.push(slide);
        self.cur_slide = self.deck.slides.len() - 1;
        self.sel = None;
        self.bootstrapped = true;
    }

    /// `edit_text` scenario body: select the first text element on slide 2
    /// and put it in edit mode with a live selection over its first word.
    fn apply_scenario_edit_text(&mut self) {
        self.cur_slide = 1;
        self.sel = self.deck.slides.get(1).and_then(|s| {
            s.elements
                .iter()
                .find(|e| e.kind == ElementKind::Text)
                .map(|e| e.id.clone())
        });
        self.editing_text = true;
        let text = self
            .selected_element()
            .map(|e| e.text.clone())
            .unwrap_or_default();
        let (a, b) = word_boundaries_chars(&text, 0);
        self.element_edit_core.set_selection(
            Self::element_char_to_byte(&text, a),
            Self::element_char_to_byte(&text, b),
        );
        self.sync_legacy_from_element_core();
        self.bootstrapped = true;
    }

    /// Grow the sample deck to 12 slides (cloning the seed slides with fresh
    /// ids) and rest scrolled to its end, so the rail overflows its viewport.
    /// Shared by the `many` / `many_rail` capture scenarios and the rail-scroll
    /// reachability tests. Never runs on a live launch.
    fn fill_long_deck(&mut self) {
        let base = self.deck.slides.clone();
        'fill: while self.deck.slides.len() < 12 {
            for s in &base {
                if self.deck.slides.len() >= 12 {
                    break 'fill;
                }
                let mut c = s.clone();
                c.id = new_id();
                for e in &mut c.elements {
                    e.id = new_id();
                }
                self.deck.slides.push(c);
            }
        }
        self.cur_slide = self.deck.slides.len() - 1;
        // Clamped to `max_scroll` at layout → the rail rests scrolled to its
        // end (the case that used to overrun the pinned buttons).
        self.rail_scroll = 1.0e6;
        self.sel = None;
        self.bootstrapped = true;
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

    // --- CRDT mirror --------------------------------------------------------

    fn canonical_matches(&self, doc: &AutoCommit) -> bool {
        self.canonical_deck
            == doc
                .get(automerge::ROOT, "deck")
                .ok()
                .flatten()
                .and_then(|(_, id)| SlidesDoc::load(doc).map(|deck| (id, deck)))
    }

    /// Reconcile before any cached interaction or delayed file result gets authority.
    /// Local edits acknowledge their own projection through reload/dispatch.
    fn reconcile_canonical(&mut self, doc: &AutoCommit) {
        let incoming = doc
            .get(automerge::ROOT, "deck")
            .ok()
            .flatten()
            .and_then(|(_, id)| SlidesDoc::load(doc).map(|deck| (id, deck)));
        if self.canonical_deck.is_some() && incoming != self.canonical_deck {
            let selected_slide = self.slide().map(|s| s.id.clone());
            let draft = self.title_input.take_dirty();
            self.drag = Drag::None;
            self.page_sidebar.on_escape();
            self.active_guides.clear();
            self.text_drag = false;
            self.caret_click = None;
            self.select_word_click = None;
            self.ctx_menu.close();
            self.ctx_hits.clear();
            self.fill_picker_open = false;
            self.editing_text = false;
            self.editing_title = false;
            self.active_document_generation = None;
            self.portal_rx = None;
            self.doc_portal_rx = None;
            self.resource_file = None;
            if self.resource_invocation.is_some() {
                self.finish_resource_file(
                    false,
                    Some("Presentation changed while file operation was pending".into()),
                );
            }
            self.reload(doc);
            if let Some(index) = selected_slide.and_then(|id| self.deck.index_of(&id)) {
                self.cur_slide = index;
            } else {
                self.editing_notes = false;
            }
            self.title_input.set_text(&self.deck.title);
            if let Some(draft) = draft.filter(|draft| draft != &self.deck.title) {
                self.io_toast = Some((
                    format!("Presentation changed. Unapplied title draft: {draft}"),
                    ToastKind::Error,
                ));
            }
        }
        self.reload(doc);
    }

    fn reload(&mut self, doc: &AutoCommit) {
        if self.drag.active() {
            return; // cache is authoritative mid-drag
        }
        if let Some(deck) = SlidesDoc::load(doc) {
            if let Ok(Some((_, id))) = doc.get(automerge::ROOT, "deck") {
                self.canonical_deck = Some((id, deck.clone()));
            }
            if deck != self.deck {
                if !actions::deck_within_aggregate_limits(&deck) {
                    self.io_toast = Some((tr("slides-toast-shared-limit"), ToastKind::Error));
                    return;
                }
                self.deck = deck;
                self.search_provider.refresh(&self.deck);
                // Bump only for an actual local/remote/undo deck change. `sync`
                // runs per frame, so equality here keeps both the settled-slide
                // cache and the complete search catalogue cold while a talk is
                // simply being presented.
                self.bump_deck_revision();
            }
        }
        if self.deck.slides.is_empty() {
            self.cur_slide = 0;
            self.sel = None;
            return;
        }
        if self.cur_slide >= self.deck.slides.len() {
            self.cur_slide = self.deck.slides.len() - 1;
        }
        // Drop selection if the element vanished (e.g. remote delete / undo).
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

    fn mark(&mut self) {
        self.needs_commit = true;
    }

    // --- action dispatch ----------------------------------------------------

    /// Dispatch one [`SlidesAction`](crate::actions::SlidesAction) — the single
    /// CRDT-mutation path shared with the Ctrl+K palette. Delegates to the
    /// framework's [`ActionHost`](ui::ActionHost), which brackets the undo
    /// boundary and applies the action through the executor (a pinned actor + a
    /// committed change, so it replays byte-identically on peers), exactly as the
    /// palette does. The in-memory deck mirror is reconciled from the document on
    /// the next `reload` / `sync`. Callers never also call [`Self::mark`] — that
    /// would double-record the undo boundary the host already records.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &crate::actions::SlidesAction) {
        self.actions.dispatch_with_context(
            doc,
            crate::actions::SLIDES_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("org.tilo.Slides", "Slides", action),
        );
        self.canonical_deck = doc
            .get(automerge::ROOT, "deck")
            .ok()
            .flatten()
            .and_then(|(_, id)| SlidesDoc::load(doc).map(|deck| (id, deck)));
    }

    /// The context Ctrl+K needs to build the deck's resource tree: the deck
    /// title and each slide (with its elements). Derived entirely from the
    /// in-memory deck mirror, which the shared document already holds.
    pub(crate) fn slides_resource_ctx(&self) -> crate::actions::SlidesResourceCtx {
        let slides = self
            .deck
            .slides
            .iter()
            .enumerate()
            .map(|(idx, s)| crate::actions::SlideResource {
                idx,
                data: crate::actions::SlideData::from_slide(s),
                elements: s
                    .z_sorted()
                    .into_iter()
                    .map(|e| crate::actions::ElementResource {
                        id: e.id.clone(),
                        kind: e.kind,
                        text: e.text.clone(),
                        z: e.z,
                    })
                    .collect(),
            })
            .collect();
        crate::actions::SlidesResourceCtx {
            deck_title: self.deck.title.clone(),
            slides,
        }
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
    fn apply_rail_event(&mut self, doc: &mut AutoCommit, ev: PageSidebarEvent) {
        match ev {
            PageSidebarEvent::Select(i) => self.apply_hit(doc, Hit::SelectSlide(i)),
            PageSidebarEvent::Reorder { from, to } => {
                let Some(id) = self.deck.slides.get(from).map(|slide| slide.id.clone()) else {
                    return;
                };
                // A reorder is authored as one absolute rank, not as a
                // receiver-relative destination index. This keeps the same
                // encoded action deterministic if remote inserts land before
                // it is replayed elsewhere.
                let Some(rank) = SlidesDoc::rank_for_move(doc, from, to) else {
                    return;
                };
                self.perform_action(doc, &actions::set_slide_order_action(id, rank));
                self.cur_slide = to;
                self.reload(doc);
                self.scroll_rail_to_current();
            }
            PageSidebarEvent::Duplicate(i) => {
                self.cur_slide = i.min(self.deck.slides.len().saturating_sub(1));
                self.duplicate_slide(doc);
            }
            PageSidebarEvent::Delete(i) => {
                self.cur_slide = i.min(self.deck.slides.len().saturating_sub(1));
                self.delete_slide(doc);
            }
            PageSidebarEvent::Add => self.new_slide(doc),
        }
    }

    /// Drain and apply every event the shared page rail produced this gesture.
    fn apply_rail_events(&mut self, doc: &mut AutoCommit) {
        for ev in self.page_sidebar.take_events() {
            self.apply_rail_event(doc, ev);
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
            tools_x,
            tools_right,
            by,
            bh,
            toolbar,
            &|label| m.pill_w(label, false),
            &|label| m.pill_w(label, true),
            m.control_h(),
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
            r.x0 + 390.0,
            r.x0 + 620.0,
            r.y0 + 10.0,
            62.0,
            toolbar,
            &|label| self.chrome_metrics.pill_w(label, false),
            &|label| self.chrome_metrics.pill_w(label, true),
            self.chrome_metrics.control_h(),
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

    fn apply_hit(&mut self, doc: &mut AutoCommit, hit: Hit) {
        // Any action other than toggling the overflow menu itself dismisses it
        // (picking a tool from it, or any other toolbar action).
        if !matches!(hit, Hit::ToggleToolbarMenu) {
            self.toolbar_tools.dismiss();
        }
        match hit {
            Hit::ToggleToolbarMenu => self.toolbar_tools.toggle(),
            Hit::NewSlide => self.new_slide(doc),
            Hit::DuplicateSlide => self.duplicate_slide(doc),
            Hit::DeleteSlide => self.delete_slide(doc),
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
                self.perform_action(
                    doc,
                    &actions::set_deck_palette_action(self.deck.palette.next()),
                );
            }
            // Images can't be conjured from nothing — the tool opens the host
            // portal file chooser; the chosen file is inserted when it resolves.
            Hit::Insert(ElementKind::Image) => self.request_image_open(),
            Hit::Insert(kind) => self.insert_element(doc, kind),
            Hit::SelectSlide(i) => {
                self.cur_slide = i.min(self.deck.slides.len().saturating_sub(1));
                self.sel = None;
                self.editing_text = false;
                self.editing_notes = false;
                self.scroll_rail_to_current();
            }
            Hit::SetFill(role) => self.set_sel_str(doc, "fill", &role),
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
                self.set_sel_str(doc, "stroke", &role);
                // Give a freshly-stroked shape a sensible default width.
                if let Some(el) = self.selected_element()
                    && role != "none"
                    && el.stroke_width < 1.0
                {
                    self.set_sel_f64(doc, "stroke_width", 4.0);
                }
            }
            Hit::ToggleBold => {
                if let Some(el) = self.selected_element() {
                    let v = !el.bold;
                    self.set_sel_bool(doc, "bold", v);
                }
            }
            Hit::ToggleItalic => {
                if let Some(el) = self.selected_element() {
                    let v = !el.italic;
                    self.set_sel_bool(doc, "italic", v);
                }
            }
            Hit::SetAlign(a) => self.set_sel_str(doc, "align", a.as_str()),
            Hit::BringFront => self.restack(doc, true),
            Hit::SendBack => self.restack(doc, false),
            Hit::DuplicateElement => self.duplicate_element(doc),
            Hit::DeleteElement => self.delete_element(doc),
            Hit::SetLayout(l) => {
                if let Some(id) = self.slide().map(|slide| slide.id.clone()) {
                    self.perform_action(
                        doc,
                        &actions::set_slide_layout_action(self.cur_slide, id, l),
                    );
                }
            }
            Hit::SetBackground(b) => {
                if let Some(id) = self.slide().map(|slide| slide.id.clone()) {
                    self.perform_action(
                        doc,
                        &actions::set_slide_background_action(self.cur_slide, id, b),
                    );
                }
            }
            Hit::SetPalette(p) => {
                self.perform_action(doc, &actions::set_deck_palette_action(p));
            }
            Hit::EditNotes => {
                self.begin_notes_edit(doc);
            }
            Hit::ToggleRail => self.toggle_rail_flap(),
            Hit::ToggleInspector => self.toggle_inspector_flap(),
        }
        self.reload(doc);
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

    fn set_sel_str(&mut self, doc: &mut AutoCommit, key: &str, v: &str) {
        if let Some(id) = self.sel.clone() {
            self.perform_action(
                doc,
                &actions::restyle_str_action(self.cur_slide, id, key, v),
            );
        }
    }
    fn set_sel_f64(&mut self, doc: &mut AutoCommit, key: &str, v: f64) {
        if let Some(id) = self.sel.clone() {
            self.perform_action(
                doc,
                &actions::restyle_f64_action(self.cur_slide, id, key, v),
            );
        }
    }
    fn set_sel_bool(&mut self, doc: &mut AutoCommit, key: &str, v: bool) {
        if let Some(id) = self.sel.clone() {
            self.perform_action(
                doc,
                &actions::restyle_bool_action(self.cur_slide, id, key, v),
            );
        }
    }

    fn restack(&mut self, doc: &mut AutoCommit, front: bool) {
        let Some(id) = self.sel.clone() else { return };
        let Some(slide_id) = self.slide().map(|slide| slide.id.clone()) else {
            return;
        };
        let Some(rank) = actions::restack_z_for_author(doc, &slide_id, &id, front) else {
            return;
        };
        self.perform_action(doc, &actions::set_element_z_action(slide_id, id, rank));
    }

    fn new_slide(&mut self, doc: &mut AutoCommit) {
        let s = actions::new_slide_template();
        let at = self.cur_slide + 1;
        let rank = SlidesDoc::rank_for_insert(doc, at);
        self.perform_action(doc, &actions::insert_slide_with_order_action(&s, rank));
        self.cur_slide = at;
        self.sel = None;
        self.editing_text = false;
        self.scroll_rail_to_current();
    }

    fn duplicate_slide(&mut self, doc: &mut AutoCommit) {
        let Some(slide) = self.deck.slides.get(self.cur_slide).cloned() else {
            return;
        };
        let mut copy = slide;
        copy.id = new_id();
        for e in &mut copy.elements {
            e.id = new_id();
        }
        let at = self.cur_slide + 1;
        let rank = SlidesDoc::rank_for_insert(doc, at);
        self.perform_action(doc, &actions::insert_slide_with_order_action(&copy, rank));
        self.cur_slide = at;
        self.sel = None;
        self.scroll_rail_to_current();
    }

    fn delete_slide(&mut self, doc: &mut AutoCommit) {
        if self.deck.slides.len() <= 1 {
            return; // keep at least one slide
        }
        let Some(id) = self.slide().map(|slide| slide.id.clone()) else {
            return;
        };
        self.perform_action(doc, &actions::delete_slide_action(self.cur_slide, id));
        if self.cur_slide > 0 {
            self.cur_slide -= 1;
        }
        self.sel = None;
    }

    fn insert_element(&mut self, doc: &mut AutoCommit, kind: ElementKind) {
        let el = match kind {
            ElementKind::Text => Element::text("Text", 440.0, 320.0, 400.0, 90.0, 40.0),
            ElementKind::Line => Element::shape(ElementKind::Line, 440.0, 360.0, 360.0, 0.0),
            ElementKind::Image => Element::shape(ElementKind::Image, 440.0, 220.0, 400.0, 280.0),
            other => Element::shape(other, 480.0, 260.0, 320.0, 220.0),
        };
        let id = el.id.clone();
        let Some(slide_id) = self.slide().map(|slide| slide.id.clone()) else {
            return;
        };
        self.perform_action(
            doc,
            &actions::add_element_action(self.cur_slide, slide_id, &el),
        );
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
    fn drain_doc_portal(&mut self, _doc: &mut AutoCommit) {
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
        let queued = worker.tx.try_send(DocumentJob::Open { generation, path });
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
        let queued = worker.tx.try_send(DocumentJob::Save {
            generation,
            path,
            deck: self.deck.clone(),
        });
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

    fn apply_loaded_deck(&mut self, doc: &mut AutoCommit, deck: Deck) {
        SlidesDoc::seed(doc, &deck);
        self.deck = deck;
        self.bump_deck_revision();
        self.cur_slide = 0;
        self.sel = None;
        self.editing_text = false;
        self.editing_notes = false;
        self.presentation.invalidate_cache();
        self.mark();
        self.reload(doc);
    }

    fn drain_document_worker(&mut self, doc: &mut AutoCommit) {
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
                self.apply_loaded_deck(doc, deck);
                self.io_toast = Some((tr("slides-toast-opened"), ToastKind::Success));
                self.finish_resource_file(true, None);
            }
            Err(error) => {
                self.io_toast = Some((tr("slides-toast-process-failed"), ToastKind::Error));
                self.finish_resource_file(false, Some(error));
            }
        }
    }

    /// Load a `.odp` from disk, replacing the deck in the shared document so
    /// undo/redo and collaboration see the whole swap as one change.
    #[cfg(test)]
    fn load_odp(&mut self, doc: &mut AutoCommit, path: &std::path::Path) -> bool {
        self.load_presentation(doc, path)
    }

    #[cfg(test)]
    fn load_presentation(&mut self, doc: &mut AutoCommit, path: &std::path::Path) -> bool {
        let Ok(bytes) = read_presentation_file(path) else {
            return false;
        };
        let deck = if is_pptx(path) {
            crate::pptx::read_pptx(&bytes)
        } else {
            crate::odp::read_odp(&bytes)
        };
        let Ok(deck) = deck else {
            return false;
        };
        SlidesDoc::seed(doc, &deck);
        self.deck = deck;
        self.bump_deck_revision();
        self.cur_slide = 0;
        self.sel = None;
        self.editing_text = false;
        self.editing_notes = false;
        self.presentation.invalidate_cache();
        self.mark();
        self.reload(doc);
        true
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

    /// Serialise the current deck to a `.odp` at `path` (adding the extension
    /// if the host didn't).
    #[cfg(test)]
    fn save_odp(&self, path: &std::path::Path) -> bool {
        self.save_presentation(path)
    }

    #[cfg(test)]
    fn save_presentation(&self, path: &std::path::Path) -> bool {
        if is_pptx(path) {
            return match crate::pptx::write_pptx(&self.deck) {
                Ok(bytes) => std::fs::write(path, bytes).is_ok(),
                Err(_) => false,
            };
        }
        let path = ensure_odp_ext(path);
        match crate::odp::write_odp(&self.deck) {
            Ok(bytes) => std::fs::write(&path, bytes).is_ok(),
            Err(_) => false,
        }
    }

    /// Drain a completed portal request, if any. Called once per frame from
    /// [`Widget::sync`]; inserts the chosen image and clears the in-flight
    /// channel. Never blocks — a still-pending picker simply leaves the channel
    /// installed for the next frame.
    fn drain_portal(&mut self, doc: &mut AutoCommit) {
        let Some(rx) = self.portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                match outcome {
                    PortalOutcome::Opened(path) => self.insert_image_from_path(doc, &path),
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
    fn insert_image_from_path(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        let Some(src) = crate::assets::import_asset_file(path) else {
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
        self.perform_action(
            doc,
            &actions::add_element_action(self.cur_slide, slide_id, &el),
        );
        self.sel = Some(id);
        self.editing_text = false;
        self.reload(doc);
    }

    fn duplicate_element(&mut self, doc: &mut AutoCommit) {
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
        self.perform_action(
            doc,
            &actions::add_element_action(self.cur_slide, slide_id, &copy),
        );
        self.sel = Some(id);
    }

    fn delete_element(&mut self, doc: &mut AutoCommit) {
        if let Some(id) = self.sel.clone() {
            self.perform_action(doc, &actions::delete_element_action(self.cur_slide, id));
            self.sel = None;
            self.editing_text = false;
        }
    }

    fn nudge(&mut self, doc: &mut AutoCommit, dx: f64, dy: f64) {
        if let Some(el) = self.selected_element().cloned() {
            let nx = (el.x + dx).clamp(-el.w + 20.0, SLIDE_W - 20.0);
            let ny = (el.y + dy).clamp(-el.h + 20.0, SLIDE_H - 20.0);
            self.perform_action(
                doc,
                &actions::move_element_action(self.cur_slide, el.id, nx, ny),
            );
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
    /// root's [`Widget::fullscreen_intent`] each frame and, via
    /// [`Widget::fullscreen_params`] (here: the largest output, a distinct
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

    fn element_char_to_byte(text: &str, char_idx: usize) -> usize {
        text.char_indices()
            .nth(char_idx)
            .map_or(text.len(), |(byte, _)| byte)
    }

    fn sync_element_core_from_legacy(&mut self) {
        let text = self.cur_edit_text();
        if self.editing_text {
            self.element_edit_core.set_selection(
                Self::element_char_to_byte(&text, self.sel_anchor),
                Self::element_char_to_byte(&text, self.caret),
            );
            self.element_edit_core.clamp_to(&text);
        }
    }

    fn sync_legacy_from_element_core(&mut self) {
        if !self.editing_text {
            return;
        }
        let text = self.cur_edit_text();
        let selection = self.element_edit_core.selection();
        self.sel_anchor = text[..selection.anchor.min(text.len())].chars().count();
        self.caret = text[..selection.cursor.min(text.len())].chars().count();
    }

    fn begin_element_edit_at_end(&mut self) {
        self.editing_text = true;
        self.editing_notes = false;
        let text = self.cur_edit_text();
        let end = text.len();
        self.element_edit_core.set_caret(end);
        self.sel_anchor = text.chars().count();
        self.caret = self.sel_anchor;
    }

    fn set_element_selection_chars(&mut self, anchor: usize, cursor: usize) {
        let text = self.cur_edit_text();
        self.element_edit_core.set_selection(
            Self::element_char_to_byte(&text, anchor),
            Self::element_char_to_byte(&text, cursor),
        );
        self.sync_legacy_from_element_core();
    }

    fn apply_element_edit(&mut self, doc: &mut AutoCommit, edit: ui::text_edit::Edit) {
        if edit.is_noop() || !self.editing_text {
            return;
        }
        let text = self.cur_edit_text();
        let char_at = |byte: usize| text[..byte.min(text.len())].chars().count();
        SlidesDoc::splice_element_text(
            doc,
            self.cur_slide,
            self.sel.as_deref().unwrap_or_default(),
            char_at(edit.at),
            text[edit.at..edit.at.saturating_add(edit.removed)]
                .chars()
                .count(),
            &edit.inserted,
        );
        self.mark();
        self.reload(doc);
        self.sync_legacy_from_element_core();
    }

    fn insert_text(&mut self, doc: &mut AutoCommit, s: &str) {
        if self.editing_title {
            // A title is one line — drop any embedded newlines from a paste.
            let s: String = s.chars().filter(|c| *c != '\n').collect();
            if s.is_empty() {
                return;
            }
            SlidesDoc::splice_title(doc, self.title_caret, 0, &s);
            self.title_caret += s.chars().count();
            self.reload(doc);
            return;
        }
        // On-canvas editing uses the shared byte-safe core; the canvas only
        // translates its edit into Automerge's character-addressed splice.
        if self.editing_text {
            self.sync_element_core_from_legacy();
            let edit = self.element_edit_core.insert(s);
            self.apply_element_edit(doc, edit);
            return;
        }
        // Typing over a selection replaces it (delete first, then insert at the
        // collapsed caret).
        self.delete_selection(doc);
        if let Some(id) = self.sel.clone() {
            SlidesDoc::splice_element_text(doc, self.cur_slide, &id, self.caret, 0, s);
            self.caret += s.chars().count();
            self.sel_anchor = self.caret;
            self.reload(doc);
        }
    }

    fn backspace_text(&mut self, doc: &mut AutoCommit) {
        if self.editing_title {
            if self.title_caret > 0 {
                SlidesDoc::splice_title(doc, self.title_caret - 1, 1, "");
                self.title_caret -= 1;
                self.mark();
                self.reload(doc);
            }
            return;
        }
        if self.editing_text {
            self.sync_element_core_from_legacy();
            let text = self.cur_edit_text();
            let edit = self.element_edit_core.delete_backward(&text, false);
            self.apply_element_edit(doc, edit);
            return;
        }
        // A live selection is deleted whole; otherwise delete the char before.
        if self.delete_selection(doc) {
            return;
        }
        if let Some(id) = self.sel.clone()
            && self.caret > 0
        {
            SlidesDoc::splice_element_text(doc, self.cur_slide, &id, self.caret - 1, 1, "");
            self.caret -= 1;
            self.sel_anchor = self.caret;
            self.mark();
            self.reload(doc);
        }
    }

    /// Forward-delete: remove the selection if any, else the char after the
    /// caret (the `Delete` key inside a text editor).
    fn forward_delete_text(&mut self, doc: &mut AutoCommit) {
        if self.editing_title {
            let len = self.deck.title.chars().count();
            if self.title_caret < len {
                SlidesDoc::splice_title(doc, self.title_caret, 1, "");
                self.mark();
                self.reload(doc);
            }
            return;
        }
        if self.editing_text {
            self.sync_element_core_from_legacy();
            let text = self.cur_edit_text();
            let edit = self.element_edit_core.delete_forward(&text, false);
            self.apply_element_edit(doc, edit);
            return;
        }
        if self.delete_selection(doc) {
            return;
        }
        let len = self.cur_text_len();
        if let Some(id) = self.sel.clone()
            && self.caret < len
        {
            SlidesDoc::splice_element_text(doc, self.cur_slide, &id, self.caret, 1, "");
            self.mark();
            self.reload(doc);
        }
    }

    fn cur_text_len(&self) -> usize {
        if self.editing_title {
            self.deck.title.chars().count()
        } else {
            self.selected_element()
                .map_or(0, |e| e.text.chars().count())
        }
    }

    /// The text currently being edited (title / notes / selected element), used
    /// by the caret helpers for vertical navigation.
    fn cur_edit_text(&self) -> String {
        if self.editing_title {
            self.deck.title.clone()
        } else {
            self.selected_element()
                .map(|e| e.text.clone())
                .unwrap_or_default()
        }
    }

    /// Start renaming the deck title inline via the shared [`TextInput`] (caret
    /// at the end, ready for full selection editing).
    fn begin_title_edit(&mut self) {
        self.editing_title = true;
        self.editing_text = false;
        self.editing_notes = false;
        self.sel = None;
        self.title_caret = self.deck.title.chars().count();
        self.title_input.set_text(&self.deck.title);
        // `set_text` clamps but doesn't advance the caret, so park it at the end.
        self.title_input.set_cursor_pos(self.deck.title.len());
        Widget::set_focus(&mut self.title_input, true);
    }

    /// Commit the inline title rename: flush any pending edit to the CRDT and
    /// blur the field.
    fn commit_title_edit(&mut self, doc: &mut AutoCommit) {
        if let Some(t) = self.title_input.take_dirty() {
            SlidesDoc::set_title(doc, &t);
        }
        self.editing_title = false;
        Widget::set_focus(&mut self.title_input, false);
        self.mark();
        self.reload(doc);
    }

    /// Bind the shared multiline editor to the current slide's real Automerge
    /// text. Rebinding is needed only when navigation changes slides; ordinary
    /// sync preserves the live caret/selection while folding in peer edits.
    fn sync_notes_input(&mut self, doc: &mut AutoCommit, place_at_end: bool) {
        let Some(obj) = SlidesDoc::notes_text_obj(doc, self.cur_slide) else {
            self.end_notes_edit();
            return;
        };
        let rebound = self.notes_input.bound_obj() != Some(&obj);
        if rebound {
            self.notes_input.bind_text(obj);
        }
        Widget::sync(&mut self.notes_input, doc, &automerge::ROOT);
        if rebound || place_at_end {
            let end = self.notes_input.value().len();
            self.notes_input.set_cursor_pos(end);
        }
        Widget::set_focus(&mut self.notes_input, true);
    }

    fn begin_notes_edit(&mut self, doc: &mut AutoCommit) {
        self.editing_notes = true;
        self.editing_text = false;
        self.sel = None;
        self.sync_notes_input(doc, true);
    }

    fn end_notes_edit(&mut self) {
        self.editing_notes = false;
        Widget::set_focus(&mut self.notes_input, false);
        self.mark();
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
                self.commit_title_edit(routed.state);
                return true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !rect.inflate(4.0, 8.0).contains(self.last_mouse) => {
                // Click away commits, then falls through so the click still acts.
                self.commit_title_edit(routed.state);
                return false;
            }
            _ => {}
        }
        let mut h = false;
        self.title_input.handle_event(
            routed.event,
            routed.scale_factor,
            &routed.modifiers,
            routed.state,
            routed.path,
            &mut h,
            routed.focus_target,
            routed.node_id,
            routed.taffy,
        );
        // Live-splice each edit into the CRDT title (the mirror is refreshed on
        // commit; while editing, the field is the source of truth).
        if let Some(t) = self.title_input.take_dirty() {
            SlidesDoc::set_title(routed.state, &t);
            self.mark();
            self.reload(routed.state);
        }
        true
    }

    /// Route the inspector notes box through its directly-bound multiline
    /// [`TextInput`]. Escape or a press outside ends editing; all editor events
    /// otherwise stay in the shared caret/selection/IME path.
    fn route_notes_edit(&mut self, routed: &mut RoutedEvent<'_>) -> bool {
        if !self.editing_notes {
            return false;
        }
        let Some(rect) = self.compute_chrome().notes_box else {
            self.end_notes_edit();
            return false;
        };
        self.notes_input.apply_layout_external(rect);
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
        self.notes_input.handle_event(
            routed.event,
            routed.scale_factor,
            &routed.modifiers,
            routed.state,
            routed.path,
            &mut input_handled,
            routed.focus_target,
            routed.node_id,
            routed.taffy,
        );
        if self.notes_input.take_dirty().is_some() {
            self.mark();
            self.reload(routed.state);
        }
        input_handled
    }

    /// The char-index caret of whatever text is being edited.
    fn active_caret(&self) -> usize {
        if self.editing_title {
            self.title_caret
        } else {
            self.caret
        }
    }

    fn set_active_caret(&mut self, v: usize) {
        if self.editing_title {
            self.title_caret = v;
        } else {
            self.caret = v;
        }
    }

    /// The active editor's [`Selection`] (anchor + caret) over the CRDT text,
    /// clamped to the current text length. Collapsed when nothing is selected.
    fn selection(&self) -> Selection {
        let len = self.cur_text_len();
        Selection {
            anchor: self.sel_anchor.min(len),
            cursor: self.active_caret().min(len),
        }
    }

    /// Whether the active editor has a non-empty text selection.
    fn has_text_selection(&self) -> bool {
        !self.selection().is_collapsed()
    }

    /// Move the caret to `pos`; unless `extend`, collapse the selection there.
    fn place_caret(&mut self, pos: usize, extend: bool) {
        let len = self.cur_text_len();
        let pos = pos.min(len);
        self.set_active_caret(pos);
        if !extend {
            self.sel_anchor = pos;
        }
    }

    /// Collapse the selection to its caret (anchor follows the cursor).
    fn collapse_selection(&mut self) {
        self.sel_anchor = self.active_caret();
    }

    /// Delete the current selection from the CRDT, if any. Returns `true` when
    /// something was deleted (the caret is left at the deletion point).
    fn delete_selection(&mut self, doc: &mut AutoCommit) -> bool {
        if self.editing_text {
            self.sync_element_core_from_legacy();
            if !self.element_edit_core.has_selection() {
                return false;
            }
            let text = self.cur_edit_text();
            let edit = self.element_edit_core.delete_backward(&text, false);
            self.apply_element_edit(doc, edit);
            return true;
        }
        let sel = self.selection();
        if sel.is_collapsed() {
            return false;
        }
        let (a, b) = (sel.min(), sel.max());
        if let Some(id) = self.sel.clone() {
            SlidesDoc::splice_element_text(doc, self.cur_slide, &id, a, b - a, "");
            self.caret = a;
        } else {
            return false;
        }
        self.sel_anchor = a;
        self.mark();
        self.reload(doc);
        true
    }

    /// The currently-selected substring of the active editor's text.
    fn selected_text(&self) -> String {
        let sel = self.selection();
        if sel.is_collapsed() {
            return String::new();
        }
        self.cur_edit_text()
            .chars()
            .skip(sel.min())
            .take(sel.max() - sel.min())
            .collect()
    }

    /// Select the whole of the active editor's text.
    fn select_all_text(&mut self) {
        let len = self.cur_text_len();
        if self.editing_text {
            let text = self.cur_edit_text();
            self.element_edit_core.select_all(&text);
        }
        self.sel_anchor = 0;
        self.set_active_caret(len);
    }

    /// Move the active caret one char left (`dir < 0`) or right. When `extend`
    /// is false and a selection is live, collapse to the selection edge in that
    /// direction instead of moving past it (standard editor behaviour).
    fn move_caret(&mut self, dir: i32, extend: bool) {
        if self.editing_text {
            self.sync_element_core_from_legacy();
            let text = self.cur_edit_text();
            self.element_edit_core.motion(
                &text,
                if dir < 0 { Motion::Left } else { Motion::Right },
                extend,
            );
            self.sync_legacy_from_element_core();
            return;
        }
        if !extend && self.has_text_selection() {
            let sel = self.selection();
            let edge = if dir < 0 { sel.min() } else { sel.max() };
            self.place_caret(edge, false);
            return;
        }
        let len = self.cur_text_len();
        let cur = self.active_caret().min(len);
        let next = if dir < 0 {
            cur.saturating_sub(1)
        } else {
            (cur + 1).min(len)
        };
        self.place_caret(next, extend);
    }

    /// Move the active caret one word left (`dir < 0`) or right, via the shared
    /// word-boundary walker (`ctrl`+arrow).
    fn move_caret_word(&mut self, dir: i32, extend: bool) {
        if self.editing_text {
            self.sync_element_core_from_legacy();
            let text = self.cur_edit_text();
            self.element_edit_core.motion(
                &text,
                if dir < 0 {
                    Motion::WordLeft
                } else {
                    Motion::WordRight
                },
                extend,
            );
            self.sync_legacy_from_element_core();
            return;
        }
        let text = self.cur_edit_text();
        let cur = self.active_caret().min(text.chars().count());
        let next = if dir < 0 {
            prev_word_boundary_chars(&text, cur)
        } else {
            next_word_boundary_chars(&text, cur)
        };
        self.place_caret(next, extend);
    }

    /// Move the active caret up/down one explicit line, keeping the column.
    fn move_caret_vertical(&mut self, dir: i32, extend: bool) {
        let text = self.cur_edit_text();
        let next = caret_line_move(&text, self.active_caret(), dir);
        self.place_caret(next, extend);
    }

    /// Move the active caret to the start (`end=false`) or end of its line.
    fn move_caret_home_end(&mut self, end: bool, extend: bool) {
        if self.editing_text {
            self.sync_element_core_from_legacy();
            let text = self.cur_edit_text();
            self.element_edit_core.motion(
                &text,
                if end {
                    Motion::LineEnd
                } else {
                    Motion::LineStart
                },
                extend,
            );
            self.sync_legacy_from_element_core();
            return;
        }
        let text = self.cur_edit_text();
        let next = caret_line_home_end(&text, self.active_caret(), end);
        self.place_caret(next, extend);
    }

    /// Copy the selection to the clipboard (shared `ui` clipboard). Returns
    /// `true` when there was a selection to copy.
    fn copy_selection(&self) -> bool {
        let s = self.selected_text();
        if s.is_empty() {
            return false;
        }
        clipboard_copy(&s);
        true
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

        // Resolve any pending pointer interaction (click-to-place caret,
        // double-click word-select, drag-extend) now that the text layout is
        // measurable, before anything borrows the slide immutably.
        let chrome_text_scale = tr.text_scale();
        tr.set_text_scale(1.0);
        self.resolve_text_pointer(tr, chrome);

        {
            let mut paint = PaintContext {
                scene,
                transform: t,
                text: tr,
                theme,
            };
            self.draw_editor_panels_base(&mut paint, chrome, rail_overlay, insp_overlay, div);
        }
        self.draw_editor_canvas(scene, t, tr, theme, chrome, &pal);
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
            chrome::fill_rect(
                scene,
                t,
                chrome.scrim,
                render::with_alpha(Color::BLACK, (alpha * 255.0) as u8),
            );
        }

        {
            let mut paint = PaintContext {
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
        paint: &mut PaintContext<'_>,
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
        // Soft drop shadow.
        let sh = chrome.slide_dest + vello::kurbo::Vec2::new(0.0, 6.0);
        chrome::fill_rrect(
            scene,
            t,
            sh.inflate(2.0, 2.0),
            14.0,
            render::with_alpha(Color::BLACK, 70),
        );
        render::draw_slide(
            scene,
            t,
            tr,
            theme,
            pal,
            slide,
            chrome.slide_dest,
            14.0,
            &self.images,
        );
        chrome::stroke_rrect(
            scene,
            t,
            chrome.slide_dest,
            14.0,
            render::with_alpha(c.text_primary, 28),
            1.0,
        );

        self.draw_editor_selection_chrome(scene, t, tr, theme, chrome, pal);
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
        tr: &mut TextRenderer,
        theme: &Theme,
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
        chrome::stroke_rrect(
            scene,
            t,
            er.inflate(HANDLE_OFFSET, HANDLE_OFFSET),
            5.0,
            sel_col,
            1.5,
        );
        if self.editing_text {
            let mut paint = PaintContext {
                scene,
                transform: t,
                text: tr,
                theme,
            };
            self.draw_text_selection(&mut paint, pal, el, er, chrome.scale);
            self.draw_text_caret(&mut paint, pal, el, er, chrome.scale);
        } else {
            for (_, hr) in self.handles_of(chrome.slide_dest, chrome.scale, el) {
                // The returned rect is the generous grab zone (±HANDLE);
                // paint a smaller visual pip centred on it so the handle
                // clears the content edge on every side.
                let vis = hr.inflate(-4.0, -4.0);
                chrome::fill_rrect(scene, t, vis, 2.5, Color::WHITE);
                chrome::stroke_rrect(scene, t, vis, 2.5, sel_col, 1.5);
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
        paint: &mut PaintContext<'_>,
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
                render::draw_slide(scene, base, tr, theme, pal, slide, rect, 6.0, images);
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
            chrome::fill_rrect(scene, t, zone, 10.0, render::with_alpha(danger, 40));
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
        paint: &mut PaintContext<'_>,
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
                let pal = self.palette(theme);
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
        let mut paint = PaintContext {
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
        paint: &mut PaintContext<'_>,
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
            chrome::fill_rrect(paint.scene, paint.transform, rect, 8.0, pal.accent);
            on_accent(c, pal)
        } else {
            ui::surface::card(paint.scene, paint.transform, rect, 8.0, c.surfaces.base);
            c.text_primary
        };
        if !label.is_empty() {
            chrome::text_centered(
                paint.scene,
                paint.transform,
                paint.text,
                label,
                rect,
                13.0,
                fg,
                TextWeight::Semibold,
            );
        }
    }

    fn draw_button_icon_text(
        paint: &mut PaintContext<'_>,
        pal: &Palette,
        rect: Rect,
        icon: Icon,
        label: &str,
        accent: bool,
    ) {
        let c = &paint.theme.tokens.colors;
        let fg = if accent {
            chrome::fill_rrect(paint.scene, paint.transform, rect, 8.0, pal.accent);
            on_accent(c, pal)
        } else {
            ui::surface::card(paint.scene, paint.transform, rect, 8.0, c.surfaces.base);
            c.text_primary
        };
        let ir = Rect::new(rect.x0 + 11.0, rect.y0 + 8.0, rect.x0 + 29.0, rect.y1 - 8.0);
        draw_icon(paint.scene, paint.transform, ir, icon, fg, 1.8);
        chrome::text_vcenter(
            paint.scene,
            paint.transform,
            paint.text,
            label,
            rect,
            ir.x1 + 5.0,
            13.0,
            fg,
            TextWeight::Semibold,
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
            chrome::fill_rrect(scene, t, rect, 7.0, pal.accent);
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
            chrome::stroke_rrect(scene, t, rect.inflate(2.5, 2.5), 8.0, pal.accent, 2.0);
        }
    }

    fn draw_button_palette(
        paint: &mut PaintContext<'_>,
        rect: Rect,
        kind: PaletteKind,
        selected: bool,
    ) {
        let c = &paint.theme.tokens.colors;
        let col = Palette::resolve(kind, paint.theme).accent;
        ui::surface::card(paint.scene, paint.transform, rect, 8.0, c.surfaces.base);
        if selected {
            chrome::stroke_rrect(paint.scene, paint.transform, rect, 8.0, col, 2.0);
        }
        let dot = Rect::new(
            rect.x0 + 8.0,
            rect.center().y - 6.0,
            rect.x0 + 20.0,
            rect.center().y + 6.0,
        );
        chrome::fill_rrect(paint.scene, paint.transform, dot, 6.0, col);
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
                paint.scene,
                paint.transform,
                paint.text,
                kind.label(),
                rect,
                dot.x1 + 6.0,
                12.0,
                c.text_primary,
                TextWeight::Semibold,
            );
            paint.scene.pop_layer();
        }
    }

    fn draw_button_segment(
        paint: &mut PaintContext<'_>,
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
            paint.scene,
            paint.transform,
            paint.text,
            label,
            rect,
            12.5,
            fg,
            TextWeight::Semibold,
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
            chrome::fill_rrect(scene, t, rect, 8.0, pal.accent);
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

    fn draw_text_caret(
        &self,
        paint: &mut PaintContext<'_>,
        pal: &Palette,
        el: &Element,
        er: Rect,
        s: f64,
    ) {
        let style = TextBoxStyle::for_element(el, er, s);
        self.draw_caret_in_box(paint, pal.accent, &el.text, self.caret, style);
    }

    /// Paint a vertical caret over one line of text inside `er`. The caret
    /// tracks the same layout the text is painted with (line height `font_px *
    /// 1.25`, per-line `align` offset, optional vertical centering), so it lands
    /// correctly on centre- and right-aligned boxes — not just left-anchored
    /// ones. `caret` is a char index into `text`.
    fn draw_caret_in_box(
        &self,
        paint: &mut PaintContext<'_>,
        accent: Color,
        text: &str,
        caret: usize,
        style: TextBoxStyle,
    ) {
        let (top, line_h) = self.caret_geometry(paint.text, text, caret, style);
        let bar = Line::new(
            Point::new(top.x, top.y + 2.0),
            Point::new(top.x, top.y + line_h - 2.0),
        );
        paint
            .scene
            .stroke(&Stroke::new(2.0), paint.transform, accent, None, &bar);
    }

    /// Pure geometry for [`Self::draw_caret_in_box`]: the caret bar's top point
    /// and the line height, tracking the same layout the painter uses (line
    /// height `font_px * 1.25`, per-line `align` offset, optional vertical
    /// centering). Split out so the placement can be asserted directly.
    #[allow(
        clippy::unused_self,
        reason = "keeps the ergonomic self.caret_geometry(...) call site consistent with the \
                  other text-layout geometry helpers; converting to an associated function \
                  would read worse at every call site for no benefit"
    )]
    fn caret_geometry(
        &self,
        tr: &mut TextRenderer,
        text: &str,
        caret: usize,
        style: TextBoxStyle,
    ) -> (Point, f64) {
        let chars: Vec<char> = text.chars().collect();
        let caret = caret.min(chars.len());
        let starts = line_starts(&chars);
        // Explicit line the caret sits on, and that line's char span.
        let mut li = 0usize;
        for (k, &st) in starts.iter().enumerate() {
            if caret >= st {
                li = k;
            } else {
                break;
            }
        }
        let line_start = starts.get(li).copied().unwrap_or(0);
        let line_end = starts
            .get(li + 1)
            .map_or(chars.len(), |&s| s.saturating_sub(1));
        let cur_line: String = chars
            .get(line_start..line_end)
            .unwrap_or(&[])
            .iter()
            .collect();
        let prefix: String = chars.get(line_start..caret).unwrap_or(&[]).iter().collect();

        let line_h = f64::from(style.font_px) * 1.25;
        let wrap_w = (style.rect.width() + 4.0).max(1.0) as f32;
        // Total painted height (wrapped rows across explicit lines) mirrors
        // `render::draw_text_element`, so the caret shares its baselines.
        let rows: Vec<usize> = text
            .split('\n')
            .map(|l| {
                if l.is_empty() {
                    1
                } else {
                    let (_, h) = tr.measure_styled(
                        l,
                        style.font_px,
                        Some(wrap_w),
                        style.weight,
                        style.italic,
                        false,
                    );
                    ((f64::from(h) / line_h).round() as usize).max(1)
                }
            })
            .collect();
        let total_h = rows.iter().sum::<usize>() as f64 * line_h;
        let block_top = if style.center_vertically && total_h < style.rect.height() {
            style.rect.y0 + (style.rect.height() - total_h) / 2.0
        } else {
            style.rect.y0
        };
        let rows_before: usize = rows.iter().take(li).sum();
        let cy = block_top + rows_before as f64 * line_h;

        // Horizontal: offset the line by its own width per its alignment (as the
        // painter does), then advance past the prefix to the caret.
        let (lw, _) = tr.measure_styled(
            &cur_line,
            style.font_px,
            Some(wrap_w),
            style.weight,
            style.italic,
            false,
        );
        let (pw, _) = tr.measure_styled(
            &prefix,
            style.font_px,
            None,
            style.weight,
            style.italic,
            false,
        );
        let line_x = match style.align {
            TextAlign::Left | TextAlign::Justify => style.rect.x0,
            TextAlign::Center => style.rect.x0 + (style.rect.width() - f64::from(lw)) / 2.0,
            TextAlign::Right => style.rect.x1 - f64::from(lw),
        };
        let cx = line_x + f64::from(pw);
        (Point::new(cx, cy), line_h)
    }

    /// Inverse of [`Self::caret_geometry`]: the char index whose caret sits
    /// nearest `p` in the same wrapped layout, for click-to-position and
    /// drag-select. Scans candidate carets and picks the closest, weighting a
    /// matching visual line over horizontal distance.
    fn char_at_point_in_box(
        &self,
        tr: &mut TextRenderer,
        text: &str,
        style: TextBoxStyle,
        p: Point,
    ) -> usize {
        let len = text.chars().count();
        let mut best = 0usize;
        let mut best_d = f64::INFINITY;
        for i in 0..=len {
            let (pt, line_h) = self.caret_geometry(tr, text, i, style);
            let dy = if p.y < pt.y {
                pt.y - p.y
            } else if p.y > pt.y + line_h {
                p.y - (pt.y + line_h)
            } else {
                0.0
            };
            let dx = (p.x - pt.x).abs();
            // A caret on the pointer's line always beats one on another line.
            let d = dy * 100_000.0 + dx;
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    }

    /// Selection highlight rectangles for the char range `[min, max)` in `text`,
    /// tracking the same wrapped layout the caret + painter use. One rect per
    /// explicit line the selection covers.
    fn selection_rects_in_box(
        &self,
        tr: &mut TextRenderer,
        text: &str,
        min: usize,
        max: usize,
        style: TextBoxStyle,
    ) -> Vec<Rect> {
        if min >= max {
            return Vec::new();
        }
        let chars: Vec<char> = text.chars().collect();
        let starts = line_starts(&chars);
        let mut rects = Vec::new();
        for (li, &ls) in starts.iter().enumerate() {
            let le = starts
                .get(li + 1)
                .map_or(chars.len(), |&s| s.saturating_sub(1));
            // Portion of this explicit line covered by the selection.
            let a = min.max(ls);
            let b = max.min(le);
            // The line's trailing '\n' is selected when the selection spans past
            // this line into the next — draw a small trailing sliver so the wrap
            // reads as selected.
            let covers_break = li + 1 < starts.len() && max > le && min <= le;
            if a > b && !covers_break {
                continue;
            }
            let a = a.min(le);
            let b = b.max(a).min(le);
            let (pa, lh) = self.caret_geometry(tr, text, a, style);
            let (pb, _) = self.caret_geometry(tr, text, b, style);
            let x0 = pa.x.min(pb.x);
            let mut x1 = pa.x.max(pb.x);
            if covers_break {
                x1 = x1.max(x0) + f64::from(style.font_px) * 0.35;
            }
            if x1 - x0 < 1.5 {
                x1 = x0 + 1.5;
            }
            rects.push(Rect::new(x0, pa.y, x1, pa.y + lh));
        }
        rects
    }

    /// Resolve any pending pointer interaction (click-to-place caret,
    /// double-click word-select, drag-extend) against the live text layout. Runs
    /// at the top of `draw` where the `TextRenderer` is available. Selection is
    /// pure per-peer view state, so no CRDT access is needed here.
    fn resolve_text_pointer(&mut self, tr: &mut TextRenderer, chrome: &Chrome) {
        if !self.editing_text {
            // Notes / no editor: drop any element-targeted pointer intent.
            self.caret_click = None;
            self.select_word_click = None;
            return;
        }
        let Some(el) = self.selected_element().cloned() else {
            self.caret_click = None;
            self.select_word_click = None;
            return;
        };
        if el.kind != ElementKind::Text {
            return;
        }
        let er = render::element_dest(chrome.slide_dest, chrome.scale, &el);
        let style = TextBoxStyle::for_element(&el, er, chrome.scale);
        if let Some(p) = self.select_word_click.take() {
            let idx = self.char_at_point_in_box(tr, &el.text, style, p);
            let (a, b) = word_boundaries_chars(&el.text, idx);
            self.set_element_selection_chars(a, b);
        } else if let Some((p, extend)) = self.caret_click.take() {
            let idx = self.char_at_point_in_box(tr, &el.text, style, p);
            self.element_edit_core.set_selection(
                Self::element_char_to_byte(&el.text, if extend { self.sel_anchor } else { idx }),
                Self::element_char_to_byte(&el.text, idx),
            );
            self.sync_legacy_from_element_core();
        }
    }

    /// Paint the on-canvas text selection highlight for the editing element.
    fn draw_text_selection(
        &self,
        paint: &mut PaintContext<'_>,
        pal: &Palette,
        el: &Element,
        er: Rect,
        s: f64,
    ) {
        let sel = self.selection();
        if sel.is_collapsed() {
            return;
        }
        let style = TextBoxStyle::for_element(el, er, s);
        let col = render::with_alpha(pal.accent, 70);
        for r in self.selection_rects_in_box(paint.text, &el.text, sel.min(), sel.max(), style) {
            chrome::fill_rrect(paint.scene, paint.transform, r, 2.0, col);
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
            chrome::fill_rrect(scene, t, nb, 8.0, render::with_alpha(accent, 28));
            scene.stroke(
                &Stroke::new(1.0),
                t,
                accent,
                None,
                &ui::surface::edge_outline(nb, 8.0),
            );
        }
        if self.editing_notes {
            self.notes_input.apply_layout_external(nb);
            if self.notes_input.value().is_empty() {
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
            self.notes_input
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
            scene,
            t,
            tr,
            theme,
            self.rect,
            &self.deck,
            self.cur_slide,
            &palette,
            &self.images,
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

/// Line starts (char index of the first char of every `\n`-delimited line).
fn line_starts(chars: &[char]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, ch) in chars.iter().enumerate() {
        if *ch == '\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Move a char-index caret up (`dir < 0`) or down one explicit (`\n`-delimited)
/// line, keeping the column where the target line is long enough. At the first
/// line (up) or last line (down) the caret is returned unchanged.
fn caret_line_move(text: &str, caret: usize, dir: i32) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let caret = caret.min(chars.len());
    let starts = line_starts(&chars);
    // Current line = the largest start <= caret.
    let mut li = 0usize;
    for (k, &st) in starts.iter().enumerate() {
        if caret >= st {
            li = k;
        } else {
            break;
        }
    }
    let col = caret - starts.get(li).copied().unwrap_or(0);
    let target = if dir < 0 {
        if li == 0 {
            return caret;
        }
        li - 1
    } else {
        if li + 1 >= starts.len() {
            return caret;
        }
        li + 1
    };
    let tstart = starts.get(target).copied().unwrap_or(0);
    let tend = starts
        .get(target + 1)
        .map_or(chars.len(), |&s| s.saturating_sub(1)); // drop the trailing '\n'
    tstart + col.min(tend.saturating_sub(tstart))
}

/// Move a char-index caret to the start (`end=false`) or end of its explicit
/// line.
fn caret_line_home_end(text: &str, caret: usize, end: bool) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let caret = caret.min(chars.len());
    let mut ls = 0usize;
    for (i, ch) in chars.iter().enumerate().take(caret) {
        if *ch == '\n' {
            ls = i + 1;
        }
    }
    if !end {
        return ls;
    }
    let mut le = caret;
    while chars.get(le).is_some_and(|&c| c != '\n') {
        le += 1;
    }
    le
}

/// Write a small, unmistakably-photographic test raster to a temp PNG and
/// return its path. Used only by the `image` `--shots` scenario so a headless
/// screenshot can show the real decode path; never called on a live launch.
fn write_demo_image() -> Option<PathBuf> {
    use image::{Rgb, RgbImage};
    let (w, h) = (640u32, 400u32);
    let mut img = RgbImage::new(w, h);
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    for (x, y, px) in img.enumerate_pixels_mut() {
        let dx = x as f32 - cx;
        let dy = y as f32 - cy;
        let r = (dx * dx + dy * dy).sqrt();
        // Concentric warm rings over a cool field — clearly a raster picture,
        // not a flat UI gradient.
        let ring = (r * 0.06).sin() * 0.5 + 0.5;
        let red = (40.0 + 200.0 * ring) as u8;
        let green = (70.0 + 90.0 * ring) as u8;
        let blue = (120.0 + 60.0 * (1.0 - ring)) as u8;
        *px = Rgb([red, green, blue]);
    }
    let path = std::env::temp_dir().join("slides-demo-image.png");
    img.save(&path).ok()?;
    Some(path)
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

impl Widget for SlidesApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }

    fn document_action_applied(
        &mut self,
        action: &tilo_actions::EncodedAction,
        before: &[automerge::ChangeHash],
        doc: &AutoCommit,
        _path: &ObjId,
    ) {
        let prior = doc.clone().fork_at(before).ok().and_then(|prior| {
            prior
                .get(automerge::ROOT, "deck")
                .ok()
                .flatten()
                .and_then(|(_, id)| SlidesDoc::load(&prior).map(|deck| (id, deck)))
        });
        if action.type_id == crate::actions::SLIDES_ACTION_TYPE && prior == self.canonical_deck {
            self.reload(doc);
        } else {
            self.reconcile_canonical(doc);
        }
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::SLIDES_OPERATION_TYPE]
    }

    fn invoke_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        use crate::actions::SlidesOperation;
        self.reconcile_canonical(doc);

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
                        ui::fullscreen::FullscreenTarget::Primary
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
                let text_len = target.text.chars().count();
                self.mode = Mode::Edit;
                self.editing_notes = false;
                self.goto_slide(idx);
                self.sel = Some(element);
                self.editing_text = edit_text;
                if edit_text {
                    // A text search result restores an immediately useful edit
                    // state: the exact text resource is selected in full.
                    self.sel_anchor = 0;
                    self.caret = text_len;
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
        }
        Ok(true)
    }

    fn start_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        self.reconcile_canonical(doc);
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
            _ => self
                .invoke_resource_operation(kind, type_id, payload, doc, path)
                .map(|handled| {
                    handled.then(|| {
                        ui::widget::ResourceOperationStart::Completed(
                            tilo_actions::ActionOutput::default(),
                        )
                    })
                }),
        }
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

    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        self.reconcile_canonical(state);
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
        if !self.seeded {
            if SlidesDoc::is_uninitialized(state) {
                SlidesDoc::seed(state, &self.deck);
            }
            self.seeded = true;
        }
        // Fold in a completed host image-open, if the portal worker finished.
        self.drain_portal(state);
        // Fold in a completed `.odp` open/save.
        self.drain_doc_portal(state);
        self.drain_document_worker(state);
        self.reload(state);
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
            self.sync_notes_input(state, false);
        }
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
        let now = std::time::Instant::now();
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

    /// Present mode wants the surface promoted to true OS fullscreen so the
    /// slideshow fills the whole output (over the shell's strips + top bar).
    /// The host ([`ui::App`]) polls this each frame and drives
    /// `window.set_fullscreen`, which on the Tilo compositor becomes an
    /// `xdg_toplevel.set_fullscreen` request. Level-triggered: a steady
    /// `Some(true)` for the whole talk is correct and idempotent.
    fn fullscreen_intent(&self) -> Option<bool> {
        Some(self.wants_fullscreen() && !self.presentation.has_external_audience())
    }

    /// Present is a *distinct* view (the slideshow, not the editor) that wants
    /// the **largest** output — the projector / big screen — via the shared
    /// [`ui::fullscreen`] mechanism. The App enumerates the live monitors and
    /// promotes the surface onto the biggest one; on a single-output host it is
    /// simply that one screen. Same mechanism the video player uses for
    /// immersive fullscreen — only the target output + the distinct-view flag
    /// differ.
    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        ui::fullscreen::FullscreenRequest::distinct(self.presentation_target)
    }

    /// React to the *actual* fullscreen state reported by the host. A
    /// compositor-initiated exit (a global un-fullscreen keybind, or the grant
    /// being revoked) drops us out of present mode so the editor chrome comes
    /// back — without this the app would keep painting the borderless slideshow
    /// in a now-windowed surface. App-initiated exits (Esc → `exit_present`)
    /// already left present mode, so this is a no-op for them.
    fn on_fullscreen_changed(&mut self, active: bool) {
        // When a second output appears, the host deliberately removes
        // fullscreen from this *presenter* window after it has installed the
        // clean audience surface. That transition is not a revoked talk.
        if !active && self.mode == Mode::Present && !self.presentation.has_external_audience() {
            self.exit_present();
        }
    }

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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        self.reconcile_canonical(state);
        // Restore replaces Text object identities; the first event may precede sync.
        if self.editing_notes {
            self.sync_notes_input(state, false);
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
            scale_factor,
            modifiers: *modifiers,
            state,
            path,
            focus_target,
            node_id,
            taffy,
        };
        if self.route_notes_edit(&mut routed) {
            *handled = true;
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
            self.on_right_press(routed.state, self.last_mouse, handled);
            return;
        }
        // While a context menu is open it is modal: every other event goes to it.
        if self.route_context_menu(&mut routed) {
            *handled = true;
            return;
        }
        // The inline title editor owns input while renaming.
        if self.route_title_edit(&mut routed) {
            *handled = true;
            return;
        }
        // The inspector's embedded widgets (font-size field, fill picker) get
        // first refusal on events over them.
        if self.route_inspector_widgets(&mut routed) {
            *handled = true;
            return;
        }
        self.dispatch_unmodal_event(
            routed.event,
            routed.scale_factor,
            routed.modifiers,
            routed.state,
            handled,
        );
    }

    fn name(&self) -> &'static str {
        "SlidesApp"
    }

    /// Slides paints a single retained canvas, so every visible control and
    /// durable object is represented as a synthetic semantic child. Controls
    /// use the same [`Hit`] values as pointer input and stable document ids are
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
                Some(
                    AccessNode::new(role)
                        .with_name(name)
                        .with_action_target(target)
                        .with_bounds(button.rect)
                        .with_state(state),
                )
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
        state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        self.reconcile_canonical(state);
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
                self.apply_hit(state, Hit::SelectSlide(index));
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
            self.apply_hit(state, hit);
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
            return self.notes_input.cursor_shape(point);
        }
        None
    }

    fn tooltip(&self) -> Option<&str> {
        self.tooltip_text.as_deref()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.editing_notes
            .then(|| self.notes_input.ime_cursor_rect())
            .flatten()
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.editing_notes
            .then(|| self.notes_input.ime_purpose())
            .flatten()
    }

    fn needs_commit(&self) -> bool {
        self.needs_commit
    }

    /// Keep receiving pointer events for the whole of a canvas move/resize or a
    /// thumbnail reorder, even if the cursor briefly leaves the source rect.
    fn wants_pointer_capture(&self) -> bool {
        self.drag.active() || self.page_sidebar.is_pending_drag()
    }

    fn on_commit(&mut self) {
        self.needs_commit = false;
    }

    /// The deck's resource tree for the shell's one Ctrl+K palette — every
    /// document-mutating affordance exported as the same action it dispatches on
    /// screen. See [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if !self.canonical_matches(doc) {
            return Vec::new();
        }
        <Self as tilo_actions::ResourceProvider>::resources(self, doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if !self.canonical_matches(doc) {
            return None;
        }
        <Self as tilo_actions::ResourceProvider>::resource(self, doc, id)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its slides action, so it records one undo boundary for it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    /// Fold the deck's slides, elements, text and notes into the unified Ctrl+K
    /// palette. Every row addresses an exported resource `View` operation, so
    /// search uses the same brokered navigation route as agents and a11y.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
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
        state: &mut AutoCommit,
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
                } else if self.text_drag {
                    // Extend the text selection from its anchor to the pointer
                    // (resolved against the wrapped layout on the next draw).
                    self.caret_click = Some((p, true));
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
                ElementState::Pressed => self.on_press(state, self.last_mouse, handled),
                ElementState::Released => self.on_release(state, handled),
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
                    state,
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
    /// staging flags are never set outside [`SlidesApp::apply_scenario`].
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

    fn on_press(&mut self, doc: &mut AutoCommit, p: Point, handled: &mut bool) {
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
                self.apply_hit(doc, Hit::OpenSlide(index));
                *handled = true;
                return;
            }
            *handled = true;
            return;
        }
        self.on_press_editor(doc, p, handled);
    }

    /// `on_press` in `Mode::Edit`: the shared rail's own context menu and the
    /// toolbar overflow menu are modal while open (handled first, ahead of
    /// all other chrome); otherwise buttons, the title, the overlay scrim,
    /// the thumbnail rail and the canvas are tried in pointer-priority order.
    fn on_press_editor(&mut self, doc: &mut AutoCommit, p: Point, handled: &mut bool) {
        let chrome = self.compute_chrome();

        if self.on_press_editor_menus(doc, &chrome, p, handled) {
            return;
        }

        // A click anywhere but the title itself commits an in-flight rename.
        if self.editing_title && !chrome.title_rect.contains(p) {
            self.editing_title = false;
            self.mark();
        }

        // Buttons first (the hamburger toggle lives here, so it wins over the
        // scrim dismissal below).
        for b in &chrome.buttons {
            if b.rect.contains(p) {
                let hit = b.hit.clone();
                self.apply_hit(doc, hit);
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
            self.apply_rail_events(doc);
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
    fn on_press_editor_menus(
        &mut self,
        doc: &mut AutoCommit,
        chrome: &Chrome,
        p: Point,
        handled: &mut bool,
    ) -> bool {
        // The shared rail's own context menu is modal while open: the press
        // picks one of its rows or dismisses it, ahead of all other chrome.
        if self.page_sidebar.menu_open() {
            self.sync_page_rail();
            self.page_sidebar.on_press(p);
            self.apply_rail_events(doc);
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
                self.apply_hit(doc, hit);
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
            self.last_click = Some((id.clone(), std::time::Instant::now(), p));
            self.editing_notes = false;

            // Already editing this exact text box → a click inside it places
            // the caret (and arms a selection drag), never starts a box move,
            // so click-to-position and drag-select work like any text field.
            if self.editing_text && self.sel.as_deref() == Some(id.as_str()) {
                if double {
                    self.select_word_click = Some(p);
                } else {
                    self.caret_click = Some((p, false));
                    self.text_drag = true;
                }
                *handled = true;
                return;
            }

            self.sel = Some(id.clone());
            if double && is_text {
                // A deliberate double-click opens the text for editing and
                // selects the word under the point.
                self.begin_element_edit_at_end();
                self.select_word_click = Some(p);
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
    fn on_right_press(&mut self, doc: &mut AutoCommit, p: Point, handled: &mut bool) {
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
                self.apply_rail_events(doc);
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
            routed.scale_factor,
            &routed.modifiers,
            routed.state,
            routed.path,
            &mut menu_handled,
            routed.focus_target,
            routed.node_id,
            routed.taffy,
        );
        if let Some(idx) = self.ctx_menu.take_selection()
            && let Some(Some(hit)) = self.ctx_hits.get(idx).cloned()
        {
            self.apply_hit(routed.state, hit);
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
            routed.scale_factor,
            &routed.modifiers,
            routed.state,
            routed.path,
            &mut h,
            routed.focus_target,
            routed.node_id,
            routed.taffy,
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
            self.set_sel_str(routed.state, "fill", &crate::model::color_to_hex(col));
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
                    routed.scale_factor,
                    &routed.modifiers,
                    routed.state,
                    routed.path,
                    &mut h,
                    routed.focus_target,
                    routed.node_id,
                    routed.taffy,
                );
                if let Some(v) = self.font_input.take_changed() {
                    self.set_sel_f64(routed.state, "font_size", v);
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
                        routed.scale_factor,
                        &routed.modifiers,
                        routed.state,
                        routed.path,
                        &mut h,
                        routed.focus_target,
                        routed.node_id,
                        routed.taffy,
                    );
                    Widget::set_focus(&mut self.font_input, true);
                    if let Some(v) = self.font_input.take_changed() {
                        self.set_sel_f64(routed.state, "font_size", v);
                    }
                    true
                } else if self.font_input.is_focused() {
                    // Press elsewhere commits + blurs, then falls through so the
                    // press can still hit whatever it landed on.
                    Widget::set_focus(&mut self.font_input, false);
                    if let Some(v) = self.font_input.take_changed() {
                        self.set_sel_f64(routed.state, "font_size", v);
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
                self.set_sel_f64(routed.state, "font_size", v);
            }
            return true;
        }
        let mut h = false;
        self.font_input.handle_event(
            routed.event,
            routed.scale_factor,
            &routed.modifiers,
            routed.state,
            routed.path,
            &mut h,
            routed.focus_target,
            routed.node_id,
            routed.taffy,
        );
        if let Some(v) = self.font_input.take_changed() {
            self.set_sel_f64(routed.state, "font_size", v);
        }
        true
    }

    fn on_release(&mut self, doc: &mut AutoCommit, handled: &mut bool) {
        // A text-selection drag ends here (no document write — selection is
        // per-peer view state).
        if self.text_drag {
            self.text_drag = false;
            *handled = true;
            return;
        }
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
                self.apply_rail_event(doc, PageSidebarEvent::Delete(from));
                *handled = true;
                return;
            }
            let was_click = !self.page_sidebar.is_dragging();
            self.page_sidebar.on_release();
            self.apply_rail_events(doc);
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
        if let Some(id) = id {
            if let Some(el) = self
                .deck
                .slides
                .get(self.cur_slide)
                .and_then(|s| s.element(&id))
                .cloned()
            {
                // The drag preview lived only in the in-memory deck; commit the
                // element's FINAL rect once, on drop — never a write per frame.
                self.perform_action(
                    doc,
                    &actions::resize_element_action(self.cur_slide, id, el.x, el.y, el.w, el.h),
                );
                *handled = true;
            }
            self.reload(doc);
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

    fn on_key(
        &mut self,
        doc: &mut AutoCommit,
        key: &Key,
        text: Option<&str>,
        modifiers: Modifiers,
        handled: &mut bool,
    ) {
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
            self.on_key_editing_text(
                doc,
                &EditingKey {
                    key,
                    text,
                    modifiers: mods,
                    control: ctrl,
                },
                handled,
            );
            return;
        }

        // Editor command keys.
        self.on_key_editor_commands(doc, key, mods, ctrl, handled);
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

    /// Keys while the on-canvas text box, speaker notes, or the deck-title
    /// rename is focused: caret/selection navigation first, then clipboard
    /// and character insertion.
    fn on_key_editing_text(
        &mut self,
        doc: &mut AutoCommit,
        input: &EditingKey<'_>,
        handled: &mut bool,
    ) {
        let shift = input.modifiers.shift_key();
        if self.on_key_editing_nav(doc, input.key, input.control, shift, handled) {
            return;
        }
        self.on_key_editing_insert(doc, input.key, input.text, input.control, handled);
    }

    /// Caret/selection navigation while editing text: Escape commits + exits,
    /// Backspace/Delete/Enter/Space edit, and the arrow/Home/End keys move or
    /// extend the caret. Returns `true` when `key` matched one of these (so
    /// [`Self::on_key_editing_text`] should stop).
    fn on_key_editing_nav(
        &mut self,
        doc: &mut AutoCommit,
        key: &Key,
        ctrl: bool,
        shift: bool,
        handled: &mut bool,
    ) -> bool {
        match key {
            Key::Named(NamedKey::Escape) => {
                self.collapse_selection();
                self.editing_text = false;
                self.editing_notes = false;
                self.editing_title = false;
                self.mark();
                *handled = true;
            }
            Key::Named(NamedKey::Backspace) => {
                self.backspace_text(doc);
                *handled = true;
            }
            Key::Named(NamedKey::Delete) => {
                self.forward_delete_text(doc);
                *handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                // A title is a single line — Enter commits the rename rather
                // than inserting a newline.
                if self.editing_title {
                    self.editing_title = false;
                    self.mark();
                } else {
                    self.insert_text(doc, "\n");
                }
                *handled = true;
            }
            Key::Named(NamedKey::Space) => {
                self.insert_text(doc, " ");
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if ctrl {
                    self.move_caret_word(-1, shift);
                } else {
                    self.move_caret(-1, shift);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                if ctrl {
                    self.move_caret_word(1, shift);
                } else {
                    self.move_caret(1, shift);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_caret_vertical(-1, shift);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.move_caret_vertical(1, shift);
                *handled = true;
            }
            Key::Named(NamedKey::Home) => {
                if ctrl {
                    self.place_caret(0, shift);
                } else {
                    self.move_caret_home_end(false, shift);
                }
                *handled = true;
            }
            Key::Named(NamedKey::End) => {
                if ctrl {
                    let len = self.cur_text_len();
                    self.place_caret(len, shift);
                } else {
                    self.move_caret_home_end(true, shift);
                }
                *handled = true;
            }
            _ => return false,
        }
        true
    }

    /// Clipboard (Ctrl+A/C/X/V via the shared clipboard core) and plain
    /// character insertion while editing text. Reached only once
    /// [`Self::on_key_editing_nav`] didn't match.
    fn on_key_editing_insert(
        &mut self,
        doc: &mut AutoCommit,
        key: &Key,
        text: Option<&str>,
        ctrl: bool,
        handled: &mut bool,
    ) {
        match key {
            // Clipboard + select-all, via the shared clipboard core.
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => {
                self.select_all_text();
                *handled = true;
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("c") => {
                self.copy_selection();
                *handled = true;
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("x") => {
                if self.copy_selection() {
                    self.delete_selection(doc);
                }
                *handled = true;
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("v") => {
                if let Some(s) = clipboard_paste() {
                    let s = if self.editing_title {
                        s.replace('\n', " ")
                    } else {
                        s
                    };
                    if !s.is_empty() {
                        self.insert_text(doc, &s);
                    }
                }
                *handled = true;
            }
            // Ctrl+Z / Ctrl+Y (and any other ctrl-combo we don't own) fall
            // through unhandled so the toolkit's global undo / redo fires.
            Key::Character(c) if !ctrl => {
                self.insert_text(doc, c.as_str());
                *handled = true;
            }
            _ => {
                if let Some(txt) = text
                    && !txt.is_empty()
                    && !ctrl
                {
                    self.insert_text(doc, txt);
                    *handled = true;
                }
            }
        }
    }

    /// Editor command keys (nothing focused for text edit): selection nav,
    /// nudge, and letter shortcuts.
    fn on_key_editor_commands(
        &mut self,
        doc: &mut AutoCommit,
        key: &Key,
        mods: winit::keyboard::ModifiersState,
        ctrl: bool,
        handled: &mut bool,
    ) {
        if self.on_key_editor_nav(doc, key, mods, handled) {
            return;
        }
        if let Key::Character(c) = key {
            self.on_key_editor_shortcut(doc, c, ctrl, handled);
        }
    }

    /// Escape/Delete/Enter/Tab plus the nudge arrows. Returns `true` when
    /// `key` matched one of these (so [`Self::on_key_editor_commands`]
    /// should stop rather than also trying the letter shortcuts).
    fn on_key_editor_nav(
        &mut self,
        doc: &mut AutoCommit,
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
                    self.delete_element(doc);
                    self.reload(doc);
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Enter) => {
                if self
                    .selected_element()
                    .is_some_and(|e| e.kind == ElementKind::Text)
                {
                    self.begin_element_edit_at_end();
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Tab) => {
                self.cycle_selection();
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.nudge(doc, if mods.shift_key() { -1.0 } else { -12.0 }, 0.0);
                self.reload(doc);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.nudge(doc, if mods.shift_key() { 1.0 } else { 12.0 }, 0.0);
                self.reload(doc);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.nudge(doc, 0.0, if mods.shift_key() { -1.0 } else { -12.0 });
                self.reload(doc);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.nudge(doc, 0.0, if mods.shift_key() { 1.0 } else { 12.0 });
                self.reload(doc);
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
        doc: &mut AutoCommit,
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
            self.duplicate_element(doc);
            self.reload(doc);
            *handled = true;
            return;
        }
        match lc.as_str() {
            "n" => {
                self.new_slide(doc);
                self.reload(doc);
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
                self.insert_element(doc, ElementKind::Text);
                self.reload(doc);
                *handled = true;
            }
            "r" => {
                self.insert_element(doc, ElementKind::Rect);
                self.reload(doc);
                *handled = true;
            }
            "o" => {
                self.insert_element(doc, ElementKind::Ellipse);
                self.reload(doc);
                *handled = true;
            }
            "l" => {
                self.insert_element(doc, ElementKind::Line);
                self.reload(doc);
                *handled = true;
            }
            "d" => {
                self.duplicate_element(doc);
                self.reload(doc);
                *handled = true;
            }
            "]" => {
                self.restack(doc, true);
                self.reload(doc);
                *handled = true;
            }
            "[" => {
                self.restack(doc, false);
                self.reload(doc);
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

/// The inspector's "Custom · #rrggbb" fill button face.
fn custom_colour_hex(hex: &str) -> String {
    tr_args("slides-custom-colour-hex", &[("hex", ArgValue::from(hex))])
}

#[cfg(test)]
#[path = "app/r2_inspector_scroll.rs"]
mod r2_inspector_scroll;

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::model::{Deck, ElementKind, Layout, PaletteKind, Slide};

    fn wait_for_document_job(app: &mut SlidesApp, doc: &mut AutoCommit) {
        for _ in 0..100 {
            app.drain_document_worker(doc);
            if app.active_document_generation.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("background presentation operation did not settle");
    }

    fn app_with(slide: Slide) -> SlidesApp {
        let mut a = SlidesApp::new();
        a.deck = Deck {
            title: "t".into(),
            palette: PaletteKind::Indigo,
            slides: vec![slide],
        };
        a.cur_slide = 0;
        a.bootstrapped = true;
        a
    }

    #[test]
    fn snaps_element_center_to_slide_center() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(ElementKind::Rect, 0.0, 0.0, 200.0, 100.0));
        let app = app_with(s);
        let (w, h) = (200.0, 100.0);
        // Drag so the element's center-x lands 4u shy of the slide center.
        let nx = SLIDE_W / 2.0 - w / 2.0 + 4.0;
        let (sx, _sy, guides) = app.snapped_move(&id, nx, 100.0, w, h, 8.0);
        assert!(
            (sx - (SLIDE_W / 2.0 - w / 2.0)).abs() < 1e-6,
            "center should snap to slide center, got left={sx}"
        );
        assert!(guides.iter().any(|g| matches!(g, Guide::Vertical(_))));
    }

    #[test]
    fn no_snap_when_outside_threshold() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(ElementKind::Rect, 0.0, 0.0, 50.0, 50.0));
        let app = app_with(s);
        let (sx, sy, guides) = app.snapped_move(&id, 300.0, 300.0, 50.0, 50.0, 4.0);
        assert_eq!((sx, sy), (300.0, 300.0));
        assert!(guides.is_empty());
    }

    #[test]
    fn present_clock_formats_minutes_seconds() {
        let app = SlidesApp::new();
        // No presentation started → 0:00.
        assert_eq!(app.presentation.clock(), "0:00");
    }

    // --- present-mode navigation (driven through the real key handler) -----

    /// A presenting app over a 5-slide deck, starting on slide 0.
    fn presenting_app() -> SlidesApp {
        let mut a = SlidesApp::new();
        a.deck = crate::model::seed_deck();
        a.bootstrapped = true;
        a.cur_slide = 0;
        a.enter_present();
        a
    }

    /// Press a `NamedKey` through the production `on_key` dispatcher.
    fn press_named(app: &mut SlidesApp, key: NamedKey) -> bool {
        let mut doc = AutoCommit::new();
        let mods = Modifiers::default();
        let mut handled = false;
        app.on_key(&mut doc, &Key::Named(key), None, mods, &mut handled);
        handled
    }

    /// Press a character key through the production `on_key` dispatcher.
    fn press_char(app: &mut SlidesApp, ch: &str) -> bool {
        let mut doc = AutoCommit::new();
        let mods = Modifiers::default();
        let mut handled = false;
        app.on_key(
            &mut doc,
            &Key::Character(ch.into()),
            Some(ch),
            mods,
            &mut handled,
        );
        handled
    }

    #[test]
    fn inserting_a_shape_writes_through_to_the_crdt_deck() {
        // The primary edit path: a keypress mutates the shared Automerge doc and
        // the in-memory mirror reflects it — not a local-only cache.
        let mut app = SlidesApp::new();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        app.reload(&doc);
        let before = app.slide().map_or(0, |s| s.elements.len());

        let mut handled = false;
        app.on_key(
            &mut doc,
            &Key::Character("r".into()),
            Some("r"),
            Modifiers::default(),
            &mut handled,
        );
        assert!(handled, "'r' is handled as insert-rectangle");
        // Reload a *fresh* mirror straight from the document to prove the write
        // landed in the CRDT, not just the widget's cache.
        let reloaded = crate::doc::SlidesDoc::load(&doc).expect("deck round-trips from the doc");
        assert_eq!(
            reloaded.slides[app.cur_slide].elements.len(),
            before + 1,
            "the inserted shape is persisted in the document"
        );
        assert!(
            app.sel.is_some(),
            "the freshly inserted element is selected"
        );
    }

    #[test]
    fn present_advances_and_reverses_via_real_handler() {
        let mut app = presenting_app();
        assert_eq!(app.mode, Mode::Present);
        assert_eq!(app.cur_slide, 0);

        // Right advances.
        assert!(press_named(&mut app, NamedKey::ArrowRight));
        assert_eq!(app.cur_slide, 1, "ArrowRight advances one slide");
        // Space advances.
        assert!(press_named(&mut app, NamedKey::Space));
        assert_eq!(app.cur_slide, 2, "Space advances one slide");
        // Left reverses.
        assert!(press_named(&mut app, NamedKey::ArrowLeft));
        assert_eq!(app.cur_slide, 1, "ArrowLeft goes back one slide");
    }

    #[test]
    fn present_clamps_at_both_ends() {
        let mut app = presenting_app();
        // Can't reverse past the first slide.
        press_named(&mut app, NamedKey::ArrowLeft);
        assert_eq!(app.cur_slide, 0, "clamped at the first slide");
        // Walk to the very end and try to overrun.
        for _ in 0..20 {
            press_named(&mut app, NamedKey::ArrowRight);
        }
        assert_eq!(
            app.cur_slide,
            app.deck.slides.len() - 1,
            "clamped at the last slide"
        );
    }

    #[test]
    fn present_home_and_end_jump_to_first_and_last() {
        let mut app = presenting_app();
        app.cur_slide = 2;
        assert!(press_named(&mut app, NamedKey::End));
        assert_eq!(app.cur_slide, app.deck.slides.len() - 1, "End → last slide");
        assert!(press_named(&mut app, NamedKey::Home));
        assert_eq!(app.cur_slide, 0, "Home → first slide");
    }

    #[test]
    fn present_number_key_jumps_to_slide() {
        let mut app = presenting_app();
        assert!(press_char(&mut app, "3"));
        assert_eq!(app.cur_slide, 2, "'3' jumps to slide index 2");
        assert!(press_char(&mut app, "1"));
        assert_eq!(app.cur_slide, 0, "'1' jumps to slide index 0");
    }

    #[test]
    fn present_escape_exits_to_editor() {
        let mut app = presenting_app();
        app.cur_slide = 2;
        assert!(press_named(&mut app, NamedKey::Escape));
        assert_eq!(app.mode, Mode::Edit, "Esc leaves present mode");
        // Position is preserved on exit (editor opens where you stopped).
        assert_eq!(app.cur_slide, 2);
    }

    #[test]
    fn blackout_and_whiteout_toggle_and_navigation_resumes() {
        let mut app = presenting_app();
        // B blacks out; B again resumes.
        assert!(press_char(&mut app, "b"));
        assert_eq!(app.presentation.blank(), Blank::Black);
        assert!(press_char(&mut app, "b"));
        assert_eq!(app.presentation.blank(), Blank::None);
        // W whites out; pressing B switches to black (mutually exclusive).
        assert!(press_char(&mut app, "w"));
        assert_eq!(app.presentation.blank(), Blank::White);
        assert!(press_char(&mut app, "b"));
        assert_eq!(app.presentation.blank(), Blank::Black);
        // A navigation key resumes the slide *without* advancing.
        let before = app.cur_slide;
        assert!(press_named(&mut app, NamedKey::ArrowRight));
        assert_eq!(
            app.presentation.blank(),
            Blank::None,
            "navigation resumes a blanked screen"
        );
        assert_eq!(
            app.cur_slide, before,
            "the resuming keypress does not also advance"
        );
    }

    #[test]
    fn fullscreen_intent_tracks_present_mode() {
        // `Widget` is in scope via `use super::*`.
        let mut app = SlidesApp::new();
        // Editor: no fullscreen request.
        assert_eq!(Widget::fullscreen_intent(&app), Some(false));
        // Present: request true fullscreen.
        app.enter_present();
        assert_eq!(Widget::fullscreen_intent(&app), Some(true));
        // Back to editor via Esc: request windowed again.
        app.exit_present();
        assert_eq!(Widget::fullscreen_intent(&app), Some(false));
    }

    #[test]
    fn present_uses_the_shared_mechanism_targeting_the_largest_output() {
        use ui::fullscreen::{FullscreenTarget, FullscreenView};
        let app = presenting_app();
        // Present routes through the ONE shared fullscreen mechanism the video
        // player also uses: a distinct presentation view on the largest monitor
        // (the projector / big screen), which the App enumerates + selects.
        let params = Widget::fullscreen_params(&app);
        assert_eq!(
            params.target,
            FullscreenTarget::Largest,
            "Present targets the big screen"
        );
        assert_eq!(
            params.view,
            FullscreenView::Distinct,
            "Present shows the slideshow, not the editor"
        );
        assert_eq!(
            Widget::fullscreen_intent(&app),
            Some(true),
            "and it drives the shared reconcile"
        );
    }

    #[test]
    fn audience_lifecycle_keeps_dashboard_local_and_preserves_single_output_fallback() {
        use ui::fullscreen::{AudiencePresentationRequest, FullscreenTarget};

        let mut app = presenting_app();
        assert_eq!(
            Widget::audience_presentation_request(&app),
            Some(AudiencePresentationRequest::new(FullscreenTarget::Largest))
        );
        Widget::on_audience_presentation_changed(&mut app, true);
        assert!(app.presentation.has_external_audience());
        assert_eq!(
            Widget::fullscreen_intent(&app),
            Some(false),
            "the original local window must not become the audience surface"
        );
        Widget::on_fullscreen_changed(&mut app, false);
        assert_eq!(
            app.mode,
            Mode::Present,
            "the host's intentional presenter unfullscreen must not end the talk"
        );
        Widget::on_audience_presentation_changed(&mut app, false);
        assert_eq!(
            Widget::fullscreen_intent(&app),
            Some(true),
            "loss of the external output returns to the established one-output slideshow"
        );
    }

    #[test]
    fn presentation_output_resource_policy_is_local_and_selects_primary() {
        let mut app = SlidesApp::new();
        let doc = AutoCommit::new();
        let payload = serde_json::to_vec(&crate::actions::SlidesOperation::StartPresentationOn {
            output: crate::actions::PresentationOutput::PrimaryExternal,
        })
        .expect("presentation output operation serializes");
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                tilo_actions::OperationKind::View,
                crate::actions::SLIDES_OPERATION_TYPE,
                &payload,
                &doc,
                &automerge::ROOT,
            )
            .expect("presentation output operation is accepted")
        );
        assert_eq!(app.mode, Mode::Present);
        assert_eq!(
            app.presentation_target,
            ui::fullscreen::FullscreenTarget::Primary
        );
    }

    #[test]
    fn compositor_exit_of_fullscreen_leaves_present_mode() {
        let mut app = presenting_app();
        assert_eq!(app.mode, Mode::Present);
        // A compositor-driven fullscreen exit (e.g. a global un-fullscreen
        // keybind) must drop us out of present mode so the editor chrome
        // returns rather than painting a windowed borderless slideshow.
        Widget::on_fullscreen_changed(&mut app, false);
        assert_eq!(
            app.mode,
            Mode::Edit,
            "losing fullscreen leaves present mode"
        );
        // Idempotent + safe when not presenting: a redundant notification is a
        // no-op, and gaining fullscreen never forces present mode on.
        Widget::on_fullscreen_changed(&mut app, false);
        assert_eq!(app.mode, Mode::Edit);
        Widget::on_fullscreen_changed(&mut app, true);
        assert_eq!(
            app.mode,
            Mode::Edit,
            "a fullscreen grant alone doesn't start a talk"
        );
    }

    #[test]
    fn presenter_dashboard_toggles_with_p() {
        let mut app = presenting_app();
        assert!(!app.presentation.is_presenter_view());
        assert!(press_char(&mut app, "p"));
        assert!(
            app.presentation.is_presenter_view(),
            "P shows the presenter dashboard"
        );
        assert!(press_char(&mut app, "p"));
        assert!(!app.presentation.is_presenter_view(), "P hides it again");
        // Toggling the dashboard never leaves present mode.
        assert_eq!(app.mode, Mode::Present);
    }

    #[test]
    fn speaker_timer_pause_and_reset() {
        let mut app = presenting_app();
        assert!(!app.presentation.is_paused());
        // K pauses, K resumes.
        assert!(press_char(&mut app, "k"));
        assert!(app.presentation.is_paused(), "K pauses the timer");
        // Elapsed is frozen while paused.
        let frozen = app.presentation.elapsed();
        std::thread::sleep(std::time::Duration::from_millis(15));
        assert_eq!(
            app.presentation.elapsed(),
            frozen,
            "elapsed is frozen while paused"
        );
        assert!(press_char(&mut app, "k"));
        assert!(!app.presentation.is_paused(), "K resumes the timer");
        // 0 resets to zero.
        assert!(press_char(&mut app, "0"));
        assert!(
            app.presentation.elapsed().as_millis() < 50,
            "0 resets the clock"
        );
    }

    #[test]
    fn present_output_plan_pairs_audience_with_presenter() {
        // The documented multi-monitor seam: the plan always pairs the clean
        // audience surface with the presenter dashboard.
        let app = presenting_app();
        assert_eq!(
            app.present_output_plan(),
            (PresentSurface::Audience, PresentSurface::Presenter)
        );
        // And present mode signals a desire for true OS fullscreen.
        assert!(app.wants_fullscreen());
    }

    #[test]
    fn entering_present_does_not_request_fullscreen_until_presenting() {
        let app = SlidesApp::new();
        assert!(!app.wants_fullscreen(), "editor mode is not fullscreen");
    }

    // --- responsive slide rail ---------------------------------------------

    /// Build an app at a fixed allocated width and run one frame's flap sync
    /// (the real per-frame path) so `compute_chrome` resolves the rail layout
    /// and eased reveal exactly as it would live.
    pub(super) fn app_sized(w: f64) -> SlidesApp {
        let mut s = Slide::new(Layout::TitleContent);
        s.add(Element::text("hello", 96.0, 90.0, 1088.0, 100.0, 54.0));
        let mut a = app_with(s);
        a.rect = Rect::new(0.0, 0.0, w, 800.0);
        a.sync_flaps(0.0, false);
        a
    }

    /// An app whose chrome metrics were resolved against a renderer at
    /// `text_scale`, exactly as the first draw of a frame does.
    pub(super) fn measured_app(w: f64, text_scale: f32) -> (SlidesApp, TextRenderer) {
        let mut app = app_sized(w);
        let theme = Theme::dark().with_text_scale(text_scale);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(theme.text_scale);
        app.refresh_chrome_metrics(&mut tr, &theme);
        (app, tr)
    }

    /// Toolbar controls other than the rail hamburger, left to right.
    fn toolbar_controls(chrome: &Chrome) -> Vec<&UiButton> {
        let mut controls: Vec<&UiButton> = chrome
            .buttons
            .iter()
            .filter(|b| {
                !matches!(b.kind, ButtonKind::Hamburger) && b.rect.center().y < chrome.toolbar.y1
            })
            .collect();
        controls.sort_by(|a, b| a.rect.x0.total_cmp(&b.rect.x0));
        controls
    }

    /// At 480 px the insert tools all move behind the overflow button, which
    /// is then pulled left of the nominal tool slot. The title must yield to
    /// that placed control, not to the slot — otherwise the ellipsis paints
    /// under the "⋯" button.
    #[test]
    fn title_yields_to_the_leftmost_placed_toolbar_control_at_480() {
        let (app, _tr) = measured_app(480.0, 1.0);
        let chrome = app.compute_chrome();
        let first = toolbar_controls(&chrome)
            .first()
            .map(|b| b.rect.x0)
            .expect("toolbar controls");
        assert!(
            chrome.title_rect.x1 <= first + 0.01,
            "title slot ends at {:.1} but a control starts at {first:.1}",
            chrome.title_rect.x1
        );
        let title = chrome
            .labels
            .iter()
            .find(|l| l.pos == chrome.title_pos)
            .expect("title label");
        let max_w = title.max_w.expect("title is elided at paint time");
        assert!(max_w > 0.0, "480 px still shows a title fragment");
        assert!(chrome.title_pos.x + max_w <= first - app.chrome_metrics.s3 + 0.01);
    }

    /// At 1× the measured chrome reproduces the classic density exactly:
    /// 56 px toolbar, 34 px pills, 30 px rail controls, 28 px inspector pills.
    #[test]
    fn one_x_chrome_density_is_unchanged() {
        let (app, _tr) = measured_app(NARROW_BREAKPOINT + 400.0, 1.0);
        let chrome = app.compute_chrome();
        assert!((chrome.toolbar.height() - 56.0).abs() < 0.01, "toolbar");
        let present = chrome
            .buttons
            .iter()
            .find(|b| matches!(b.hit, Hit::Present))
            .expect("Present");
        assert!((present.rect.height() - 34.0).abs() < 0.01, "toolbar pill");
        let add = chrome
            .buttons
            .iter()
            .find(|b| matches!(b.hit, Hit::NewSlide) && chrome.rail.contains(b.rect.center()))
            .expect("rail Add");
        assert!((add.rect.height() - 30.0).abs() < 0.01, "rail control");
        let layout_pill = chrome
            .buttons
            .iter()
            .find(|b| matches!(b.hit, Hit::SetLayout(_)))
            .expect("layout pill");
        assert!(
            (layout_pill.rect.height() - 28.0).abs() < 0.01,
            "inspector pill"
        );
    }

    /// At 2× every toolbar pill, rail control and inspector row follows the
    /// measured text: labels fit their slots, pills share one centre line,
    /// and no section caption is painted under the control below it.
    #[test]
    fn two_x_chrome_rows_follow_measured_text() {
        let (mut app, mut tr) = measured_app(NARROW_BREAKPOINT + 400.0, 2.0);
        // Select the text element so the inspector shows every section.
        app.sel = app
            .slide()
            .and_then(|s| s.elements.first().map(|e| e.id.clone()));
        let chrome = app.compute_chrome();
        let m = app.chrome_metrics.clone();
        assert!(m.lh_control > 30.0, "2x control line box {}", m.lh_control);
        assert!(
            chrome.toolbar.height() >= m.title_block_h() + 2.0 * m.s2 - 0.01
                && chrome.toolbar.height() > 56.0,
            "toolbar grew to its two-line title block"
        );

        let controls = toolbar_controls(&chrome);
        let centre = controls[0].rect.center().y;
        for b in &controls {
            assert!((b.rect.center().y - centre).abs() < 0.01, "one centre line");
            assert!(
                b.rect.height() >= m.lh_control + m.s4 - 0.01,
                "pill fits its line box"
            );
            if let ButtonKind::Text { label, .. } | ButtonKind::IconText { label, .. } = &b.kind {
                let (w, _) = tr.measure_weighted(label, FONT_CONTROL, None, TextWeight::Semibold);
                assert!(
                    b.rect.width() >= f64::from(w) + 2.0 * m.s3 - 0.01,
                    "'{label}' needs {w:.1} + padding, pill is {:.1}",
                    b.rect.width()
                );
            }
        }
        for pair in controls.windows(2) {
            assert!(
                pair[0].rect.x1 <= pair[1].rect.x0 + 0.01,
                "toolbar controls never overlap"
            );
        }

        // Inspector: captions keep clear of the control that follows them,
        // and every text control is at least its line box tall.
        let in_inspector = |r: Rect| chrome.inspector.contains(r.center());
        for l in chrome
            .labels
            .iter()
            .filter(|l| (l.font - FONT_CAPTION).abs() < 0.01 && chrome.inspector.contains(l.pos))
        {
            let caption = Rect::new(l.pos.x, l.pos.y, l.pos.x + 1.0, l.pos.y + m.lh_caption);
            for b in chrome.buttons.iter().filter(|b| in_inspector(b.rect)) {
                assert!(
                    b.rect.y0 >= caption.y1 - 0.01 || b.rect.y1 <= caption.y0 + 0.01,
                    "'{}' caption at y {:.1}..{:.1} collides with a control at {:.1}..{:.1}",
                    l.text,
                    caption.y0,
                    caption.y1,
                    b.rect.y0,
                    b.rect.y1
                );
            }
        }
        for b in chrome.buttons.iter().filter(|b| in_inspector(b.rect)) {
            match &b.kind {
                ButtonKind::Text { label, .. } if !label.is_empty() => {
                    assert!(
                        b.rect.height() >= m.lh_control - 0.01,
                        "'{label}' row fits its text"
                    );
                }
                // Alignment segments wrap to two rows rather than clip their
                // labels once a quarter-width cell is too narrow for them.
                ButtonKind::Segment { label, .. } => {
                    let (w, _) =
                        tr.measure_weighted(label, FONT_CONTROL, None, TextWeight::Semibold);
                    assert!(
                        b.rect.width() >= f64::from(w) + 2.0 * m.s2 - 0.01,
                        "segment '{label}' needs {w:.1}, cell is {:.1}",
                        b.rect.width()
                    );
                }
                _ => {}
            }
        }
        // The font-size input reserve fits two square steppers plus a
        // three-digit value at the scaled control size, and the B/I toggles
        // never paint over it (they wrap below when the row is too narrow).
        let font_input = chrome
            .font_input_rect
            .expect("text element shows the TYPE row");
        let (w, _) = tr.measure_weighted("999 pt", FONT_CONTROL, None, TextWeight::Semibold);
        assert!(font_input.width() >= 2.0 * m.control_h() + f64::from(w) + 2.0 * m.s2 - 0.01);
        // The speaker-notes box never inverts or folds back over the sections:
        // it is at least one control tall and starts below the last control.
        let notes = chrome.notes_box.expect("notes box");
        assert!(
            notes.height() >= m.control_h() - 0.01,
            "notes box {notes:?}"
        );
        let sections_bottom = chrome
            .buttons
            .iter()
            .filter(|b| in_inspector(b.rect) && !matches!(b.hit, Hit::EditNotes))
            .map(|b| b.rect.y1)
            .fold(0.0_f64, f64::max);
        assert!(
            notes.y0 >= sections_bottom - 0.01,
            "notes box starts under the sections"
        );
        let inspector_controls: Vec<Rect> = chrome
            .buttons
            .iter()
            .filter(|b| in_inspector(b.rect))
            .map(|b| b.rect)
            .chain([font_input, notes])
            .collect();
        for (i, a) in inspector_controls.iter().enumerate() {
            for b in inspector_controls.iter().skip(i + 1) {
                let overlap = a.intersect(*b);
                assert!(
                    overlap.width() <= 0.01 || overlap.height() <= 0.01,
                    "inspector controls overlap: {a:?} vs {b:?}"
                );
            }
        }
        // Rail: the pinned action row grew with its labels.
        for b in chrome
            .buttons
            .iter()
            .filter(|b| chrome.rail.contains(b.rect.center()))
        {
            if let ButtonKind::Text { label, .. } = &b.kind {
                assert!(
                    b.rect.height() >= m.lh_control - 0.01,
                    "rail '{label}' fits"
                );
            }
        }
    }

    #[test]
    fn wide_window_docks_the_rail_beside_the_canvas() {
        let app = app_sized(NARROW_BREAKPOINT + 200.0);
        assert_eq!(app.rail_layout(), SplitLayout::Docked);
        assert!(app.rail_open(), "docked rail is open by default");
        let chrome = app.compute_chrome();
        assert_eq!(chrome.layout, SplitLayout::Docked);
        // The rail occupies the full left column and the canvas starts past it.
        assert!((chrome.rail.x0 - app.rect.x0).abs() < 1e-6);
        assert!(
            chrome.rail.width() > RAIL_W - 1.0,
            "docked rail is full width"
        );
        assert!(
            chrome.slide_dest.x0 > chrome.rail.x1 - 1.0,
            "canvas sits to the right of the docked rail"
        );
        assert_eq!(chrome.scrim.width(), 0.0, "no scrim while docked");
        // No hamburger when the rail is docked + fully open.
        assert!(
            !chrome
                .buttons
                .iter()
                .any(|b| matches!(b.kind, ButtonKind::Hamburger)),
            "no hamburger needed when the rail is docked open"
        );
    }

    #[test]
    fn narrow_window_collapses_rail_to_a_closed_overlay() {
        let app = app_sized(NARROW_BREAKPOINT - 200.0);
        assert_eq!(app.rail_layout(), SplitLayout::Overlay);
        assert!(!app.rail_open(), "overlay rail is closed by default");
        let chrome = app.compute_chrome();
        assert_eq!(chrome.layout, SplitLayout::Overlay);
        // Canvas spans the full width (rail no longer insets it).
        assert!(
            (chrome.slide_dest.x0 - app.rect.x0) < (RAIL_W * 0.5),
            "narrow canvas reclaims the rail's column"
        );
        // The closed flap is fully off the left edge, and no scrim shows.
        assert!(
            chrome.rail.x1 <= app.rect.x0 + 1.0,
            "closed flap is off-screen left"
        );
        assert_eq!(
            chrome.scrim.width(),
            0.0,
            "no scrim while the flap is closed"
        );
        // A hamburger is offered to summon it.
        assert!(
            chrome
                .buttons
                .iter()
                .any(|b| matches!(b.kind, ButtonKind::Hamburger)),
            "narrow layout offers a hamburger to summon the rail"
        );
    }

    #[test]
    fn phone_toolbar_elides_title_before_compact_controls() {
        let app = app_sized(390.0);
        let chrome = app.compute_chrome();
        let first_right_control = chrome
            .buttons
            .iter()
            .filter(|button| {
                !matches!(button.kind, ButtonKind::Hamburger)
                    && button.rect.center().y < chrome.toolbar.y1
            })
            .map(|button| button.rect.x0)
            .min_by(f64::total_cmp)
            .unwrap();
        assert!(
            chrome.title_rect.x1 <= first_right_control,
            "the two-line title must not paint under compact toolbar controls"
        );
    }

    #[test]
    fn opening_the_overlay_flap_reveals_the_rail_over_a_scrim() {
        let mut app = app_sized(NARROW_BREAKPOINT - 200.0);
        // Summon via the production toggle path, then settle the animation.
        app.rail_flap.toggle(app.rail_layout());
        app.rail_flap.snap(true);
        assert!(app.rail_open());
        let chrome = app.compute_chrome();
        assert_eq!(chrome.layout, SplitLayout::Overlay);
        // Fully revealed: the flap is flush with the left edge over a scrim.
        assert!(
            (chrome.rail.x0 - app.rect.x0).abs() < 1.0,
            "open flap is flush left"
        );
        assert!(
            chrome.scrim.width() > 0.0,
            "an open overlay flap dims the body"
        );
        assert!(
            chrome.buttons.iter().any(|button| {
                matches!(&button.kind, ButtonKind::Text { label, .. } if label == "Close")
                    && matches!(button.hit, Hit::ToggleRail)
            }),
            "the open overlay has a named Close control in its own header"
        );
    }

    #[test]
    fn hamburger_hit_toggles_the_overlay_flap() {
        let mut app = app_sized(NARROW_BREAKPOINT - 200.0);
        let mut doc = AutoCommit::new();
        assert!(!app.rail_open());
        // Drive the real action dispatcher, not a private setter.
        app.apply_hit(&mut doc, Hit::ToggleRail);
        assert!(app.rail_open(), "ToggleRail opens the collapsed flap");
        app.apply_hit(&mut doc, Hit::ToggleRail);
        assert!(!app.rail_open(), "ToggleRail closes it again");
    }

    #[test]
    fn wide_window_docks_the_inspector() {
        let app = app_sized(NARROW_BREAKPOINT + 200.0);
        assert_eq!(app.inspector_layout(), SplitLayout::Docked);
        assert!(app.inspector_open(), "docked inspector is open by default");
        let chrome = app.compute_chrome();
        assert_eq!(chrome.inspector_layout, SplitLayout::Docked);
        // Docked inspector is a full-width right column and the canvas ends at
        // its left edge.
        assert!(
            chrome.inspector.width() > INSPECTOR_W - 1.0,
            "docked inspector is full width"
        );
        assert!(
            chrome.slide_dest.x1 < chrome.inspector.x0 + 1.0,
            "canvas sits to the left of the docked inspector"
        );
    }

    #[test]
    fn large_text_chrome_keeps_hidden_tools_on_discoverable_routes() {
        let mut app = SlidesApp::demo(None);
        sized(&mut app, 1440.0, 960.0);
        app.text_scale = 2.0;
        let chrome = app.compute_chrome();
        assert!(
            chrome
                .buttons
                .iter()
                .any(|button| matches!(button.hit, Hit::ToggleToolbarMenu))
        );
        assert!(
            chrome
                .buttons
                .iter()
                .any(|button| matches!(button.hit, Hit::ToggleInspector))
        );
        assert!(
            chrome
                .buttons
                .iter()
                .any(|button| matches!(button.hit, Hit::Present))
        );
        assert!(
            chrome.slide_dest.width() > 600.0,
            "large type keeps a practical authored-slide canvas"
        );
    }

    #[test]
    fn narrow_window_collapses_inspector_to_a_closed_overlay() {
        // Below the inspector breakpoint the inspector becomes a closed flap and
        // the canvas reclaims its column (spanning to the right edge).
        let app = app_sized(INSPECTOR_BREAKPOINT - 120.0);
        assert_eq!(app.inspector_layout(), SplitLayout::Overlay);
        assert!(
            !app.inspector_open(),
            "overlay inspector is closed by default"
        );
        let chrome = app.compute_chrome();
        assert!(
            chrome.inspector.x0 >= app.rect.x1 - 1.0,
            "closed inspector is off-screen right"
        );
        // A toggle is offered to summon it.
        assert!(
            chrome
                .buttons
                .iter()
                .any(|b| matches!(b.hit, Hit::ToggleInspector)),
            "narrow layout offers an inspector toggle"
        );
    }

    #[test]
    fn inspector_toggle_hit_opens_the_overlay_flap() {
        let mut app = app_sized(INSPECTOR_BREAKPOINT - 120.0);
        let mut doc = AutoCommit::new();
        assert!(!app.inspector_open());
        // Drive the real action dispatcher, not a private setter.
        app.apply_hit(&mut doc, Hit::ToggleInspector);
        assert!(
            app.inspector_open(),
            "ToggleInspector opens the collapsed flap"
        );
        app.apply_hit(&mut doc, Hit::ToggleInspector);
        assert!(!app.inspector_open(), "ToggleInspector closes it again");
    }

    #[test]
    fn compact_editor_keeps_the_canvas_wide_with_both_panels_as_drawers() {
        let app = app_sized(820.0);
        let chrome = app.compute_chrome();
        assert_eq!(chrome.layout, SplitLayout::Overlay);
        assert_eq!(chrome.inspector_layout, SplitLayout::Overlay);
        assert!(
            chrome.slide_dest.width() > 700.0,
            "closed drawers leave a useful slide canvas at compact laptop widths"
        );
        assert!(
            chrome
                .buttons
                .iter()
                .any(|button| matches!(button.hit, Hit::ToggleRail))
                && chrome
                    .buttons
                    .iter()
                    .any(|button| matches!(button.hit, Hit::ToggleInspector)),
            "both drawers retain explicit toolbar controls"
        );
    }

    #[test]
    fn compact_drawers_are_mutually_exclusive() {
        let mut app = app_sized(820.0);
        let mut doc = AutoCommit::new();
        app.apply_hit(&mut doc, Hit::ToggleRail);
        assert!(app.rail_open());
        assert!(!app.inspector_open());
        app.apply_hit(&mut doc, Hit::ToggleInspector);
        assert!(app.inspector_open(), "properties drawer opens");
        assert!(
            !app.rail_open(),
            "opening properties closes the slide drawer immediately"
        );
    }

    #[test]
    fn resizing_across_the_breakpoint_returns_to_the_layout_default() {
        let mut app = app_sized(NARROW_BREAKPOINT - 200.0);
        // User opens the flap while narrow.
        app.rail_flap.reconcile(app.rail_layout());
        app.apply_hit(&mut AutoCommit::new(), Hit::ToggleRail);
        assert!(app.rail_open());
        // Widen past the breakpoint: the stale preference drops and the rail
        // adopts the docked default (open). Then narrow again → closed default.
        app.rect = Rect::new(0.0, 0.0, NARROW_BREAKPOINT + 200.0, 800.0);
        app.rail_flap.reconcile(app.rail_layout());
        assert_eq!(app.rail_layout(), SplitLayout::Docked);
        assert!(app.rail_open(), "docked default is open");
        app.rect = Rect::new(0.0, 0.0, NARROW_BREAKPOINT - 200.0, 800.0);
        app.rail_flap.reconcile(app.rail_layout());
        assert!(
            !app.rail_open(),
            "overlay default is closed after re-narrowing"
        );
    }

    #[test]
    fn same_instance_reflows_both_panels_wide_then_narrow() {
        // The breakpoint MUST be re-derived from the *current* width every
        // frame, not cached at construction — resize a single instance wide,
        // then narrow, and confirm both panels flip layout.
        let mut app = app_sized(1280.0);
        assert_eq!(app.rail_layout(), SplitLayout::Docked);
        assert_eq!(app.inspector_layout(), SplitLayout::Docked);
        let wide = app.compute_chrome();
        assert_eq!(wide.layout, SplitLayout::Docked);
        assert_eq!(wide.inspector_layout, SplitLayout::Docked);

        // Simulate a resize down to a phone-ish width and run the per-frame
        // flap sync (the real path) — both panels must become overlay flaps and
        // the canvas must span the full width.
        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.sync_flaps(0.0, false);
        assert_eq!(
            app.rail_layout(),
            SplitLayout::Overlay,
            "rail reflows to overlay"
        );
        assert_eq!(
            app.inspector_layout(),
            SplitLayout::Overlay,
            "inspector reflows to overlay"
        );
        let narrow = app.compute_chrome();
        assert_eq!(narrow.layout, SplitLayout::Overlay);
        assert_eq!(narrow.inspector_layout, SplitLayout::Overlay);
        assert!(
            narrow.slide_dest.x1 > 560.0 - RAIL_W,
            "canvas reclaims the collapsed panels' width when narrow"
        );
    }

    // --- scrolling ----------------------------------------------------------

    /// An edit-mode app over a deck long enough to overflow the rail.
    fn tall_editor(slide_count: usize, h: f64) -> SlidesApp {
        let mut slides = Vec::new();
        for _ in 0..slide_count {
            let mut s = Slide::new(Layout::TitleContent);
            s.add(Element::text("slide", 96.0, 90.0, 1000.0, 100.0, 40.0));
            slides.push(s);
        }
        let mut a = SlidesApp::new();
        a.deck = Deck {
            title: "t".into(),
            palette: PaletteKind::Indigo,
            slides,
        };
        a.cur_slide = 0;
        a.bootstrapped = true;
        a.rect = Rect::new(0.0, 0.0, 1280.0, h);
        a.sync_flaps(0.0, false);
        a
    }

    #[test]
    fn wheel_scrolls_the_thumbnail_rail_only_over_the_rail() {
        let mut app = tall_editor(16, 560.0);
        // The shared rail is the scroll authority: sync it to this frame, then
        // read the overflow it computed.
        app.sync_page_rail();
        let max_scroll = app.page_sidebar.max_scroll();
        assert!(max_scroll > 0.0, "a 16-slide rail overflows a 560px window");

        // Pointer over the rail → wheel scrolls it (down).
        let rail = app.rail_rect();
        app.last_mouse = Point::new(rail.center().x, rail.center().y);
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
        assert!(handled, "wheel over the rail is handled");
        assert!(app.rail_scroll > 0.0, "wheel over the rail scrolls it");

        // Overscroll clamps to the maximum, so every thumbnail stays reachable.
        for _ in 0..40 {
            let mut h = false;
            app.on_wheel(MouseScrollDelta::LineDelta(0.0, -20.0), 1.0, &mut h);
        }
        assert!(
            (app.rail_scroll - max_scroll).abs() < 1.0,
            "rail scroll clamps to max"
        );

        // Pointer over the canvas → the rail does not move.
        app.rail_scroll = 0.0;
        app.last_mouse = Point::new(app.rect.x1 - 80.0, app.rect.center().y);
        let mut h2 = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut h2);
        assert_eq!(
            app.rail_scroll, 0.0,
            "wheel over the canvas leaves the rail alone"
        );
    }

    #[test]
    fn wheel_scrolls_overview_and_last_slide_becomes_reachable() {
        let mut app = SlidesApp::new();
        app.deck = crate::model::seed_deck();
        app.bootstrapped = true;
        app.mode = Mode::Overview;
        // 800x600 is exactly the size the prior review reported cutting off slide 5.
        app.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        let (_, _, max_scroll) = app.compute_overview();
        assert!(
            max_scroll > 0.0,
            "the 5-slide deck overflows the 800x600 overview"
        );

        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
        assert!(
            handled && app.overview.scroll() > 0.0,
            "wheel scrolls the overview"
        );

        // Scroll to the very end and confirm the last slide's card is on-screen.
        for _ in 0..60 {
            let mut h = false;
            app.on_wheel(MouseScrollDelta::LineDelta(0.0, -20.0), 1.0, &mut h);
        }
        assert!(
            (app.overview.scroll() - max_scroll).abs() < 1.0,
            "overview scroll clamps to max"
        );
        let (area, thumbs, _) = app.compute_overview();
        let last = thumbs.last().expect("a slide").1;
        assert!(
            last.y0 < area.y1 && last.y1 > area.y0,
            "the last slide is inside the viewport once scrolled to the end"
        );
    }

    #[test]
    fn overview_scrollbar_thumb_drag_scrolls_the_grid() {
        let mut app = SlidesApp::new();
        app.deck = crate::model::seed_deck();
        app.bootstrapped = true;
        app.mode = Mode::Overview;
        app.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        let (area, _, max_scroll) = app.compute_overview();
        let (_, thumb) = scrollbar_rects(area, 0.0, max_scroll).expect("overflow ⇒ scrollbar");
        // Start a thumb drag (the real on_press path constructs this), then move.
        app.drag = Drag::ScrollOverview {
            start_y: thumb.center().y,
            orig_scroll: 0.0,
        };
        app.drag_move(Point::new(thumb.center().x, thumb.center().y + 120.0));
        assert!(
            app.overview.scroll() > 0.0,
            "dragging the thumb down scrolls the grid"
        );
    }

    // --- present mode rests -------------------------------------------------

    #[test]
    fn present_mode_rests_when_idle_and_wakes_for_a_transition() {
        let mut app = presenting_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        // Settle the flaps so nothing is mid-animation.
        for _ in 0..3 {
            app.sync_flaps(1.0, false);
        }
        assert!(
            !app.needs_animation_frame(),
            "a settled present mode does not request frames at vsync"
        );
        // A navigation kicks off a bounded cross-fade → frames pump again.
        app.next_slide(1);
        assert!(
            app.needs_animation_frame(),
            "a live cross-fade pumps frames"
        );
    }

    #[test]
    fn reduced_motion_settles_presentation_navigation_in_one_pass() {
        let mut app = presenting_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        app.next_slide(1);
        assert!(app.presentation.needs_animation_frame());

        Widget::set_motion_reduced(&mut app, true);

        assert!(!app.presentation.needs_animation_frame());
        assert_eq!(app.presentation.transition_alpha(true), 1.0);
        assert!(!app.needs_animation_frame());
    }

    // --- toolbar overflow menu ---------------------------------------------

    fn seeded_doc(app: &SlidesApp) -> AutoCommit {
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        doc
    }

    #[test]
    fn clicking_an_overflow_menu_row_inserts_that_element() {
        let mut app = app_sized(560.0);
        let mut doc = seeded_doc(&app);
        app.reload(&doc);
        app.toolbar_tools.set_open(true);
        let before = app.slide().unwrap().elements.len();

        let chrome = app.compute_chrome();
        let row = chrome
            .toolbar_tools
            .menu_buttons
            .iter()
            .find(|b| matches!(b.hit, Hit::Insert(ElementKind::Rect)))
            .expect("the Rectangle tool is in the overflow menu at 560px");
        let p = row.rect.center();
        let mut handled = false;
        app.last_mouse = p;
        app.on_press(&mut doc, p, &mut handled);
        assert!(handled, "clicking a menu row is handled");
        assert!(
            !app.toolbar_tools.is_open(),
            "picking a tool closes the menu"
        );
        assert_eq!(
            app.slide().unwrap().elements.len(),
            before + 1,
            "the chosen shape was actually inserted"
        );
    }

    // --- deck-theme palette pills ------------------------------------------

    #[test]
    fn deck_theme_pills_fit_their_full_labels_and_never_overlap() {
        // Docked inspector, nothing selected → the slide-level inspector shows
        // the five deck-theme pills. Each must be wide enough that its full name
        // fits (drawn from `dot.x1 + 6`), and pills on a row must not overlap —
        // this is the exact geometry `draw_button` paints, so the assertion is
        // on the real path.
        let mut app = app_sized(1280.0);
        app.sel = None;
        let chrome = app.compute_chrome();
        let pills: Vec<&UiButton> = chrome
            .buttons
            .iter()
            .filter(|b| {
                matches!(b.kind, ButtonKind::Palette { .. })
                    && chrome.inspector.contains(b.rect.center())
            })
            .collect();
        assert_eq!(
            pills.len(),
            PaletteKind::ALL.len(),
            "all five theme pills are present"
        );
        for b in &pills {
            if let ButtonKind::Palette { kind, .. } = &b.kind {
                let label_right = b.rect.x0 + 26.0 + chrome::est_width(kind.label(), 12.0);
                assert!(
                    label_right <= b.rect.x1 + 0.5,
                    "'{}' pill is wide enough for its full label (label right {:.1} <= pill right {:.1})",
                    kind.label(),
                    label_right,
                    b.rect.x1
                );
            }
        }
        for (i, a) in pills.iter().enumerate() {
            for b in &pills[i + 1..] {
                let same_row = (a.rect.y0 - b.rect.y0).abs() < 1.0;
                if same_row {
                    assert!(
                        a.rect.x1 <= b.rect.x0 + 0.5 || b.rect.x1 <= a.rect.x0 + 0.5,
                        "deck-theme pills on the same row must not overlap"
                    );
                }
            }
        }
    }

    #[test]
    fn text_alignment_segments_fill_the_shared_inspector_columns() {
        let (mut app, _doc, _id) = selected_text_app();
        sized(&mut app, 1280.0, 800.0);
        let chrome = app.compute_chrome();
        let segments: Vec<Rect> = chrome
            .buttons
            .iter()
            .filter_map(|button| {
                matches!(button.kind, ButtonKind::Segment { .. }).then_some(button.rect)
            })
            .collect();

        assert_eq!(segments.len(), TextAlign::ALL.len());
        let left = chrome.inspector.x0 + 16.0;
        let right = chrome.inspector.x1 - 16.0;
        assert_eq!(segments[0].x0, left);
        assert_eq!(segments.last().unwrap().x1, right);
        let width = segments[0].width();
        for pair in segments.windows(2) {
            if (pair[0].y0 - pair[1].y0).abs() < 0.5 {
                assert_eq!(pair[0].x1, pair[1].x0, "segments meet without gaps");
            } else {
                assert_eq!(pair[0].x1, right, "each wrapped row fills the right edge");
                assert_eq!(pair[1].x0, left, "each wrapped row starts at the left edge");
            }
            assert_eq!(pair[1].width(), width, "segments divide the row evenly");
        }
    }

    // --- selection handles clear content -----------------------------------

    #[test]
    fn selection_handles_never_cover_the_element_content() {
        // A left-anchored text box: every resize handle's visible pip must sit
        // outside the content rect, so it can't cover the first glyph (the 'G'
        // of a left-aligned title was the reported bug).
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text(
            "Goals for this phase",
            100.0,
            100.0,
            600.0,
            90.0,
            40.0,
        ));
        let mut app = app_with(s);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.sel = Some(id);
        app.sync_flaps(0.0, false);
        let chrome = app.compute_chrome();
        let el = app.selected_element().expect("a selected element").clone();
        let er = render::element_dest(chrome.slide_dest, chrome.scale, &el);
        for (h, hr) in app.handles_of(chrome.slide_dest, chrome.scale, &el) {
            // The painted pip is the grab rect inset by 4 (see `draw_editor`).
            let vis = hr.inflate(-4.0, -4.0);
            let overlaps = vis.x0 < er.x1 && vis.x1 > er.x0 && vis.y0 < er.y1 && vis.y1 > er.y0;
            assert!(
                !overlaps,
                "handle {h:?} pip {vis:?} must not overlap the content rect {er:?}"
            );
        }
    }

    // --- image insertion (real decode + real portal seam) -------------------

    fn write_png(name: &str, w: u32, h: u32) -> std::path::PathBuf {
        use image::{Rgb, RgbImage};
        let mut img = RgbImage::new(w, h);
        for (x, _y, px) in img.enumerate_pixels_mut() {
            *px = Rgb([(x % 256) as u8, 90, 140]);
        }
        let path = std::env::temp_dir().join(name);
        img.save(&path).expect("write test png");
        path
    }

    #[test]
    fn image_store_decodes_a_real_png_and_fails_gracefully() {
        use crate::assets::{ImageState, ImageStore};
        let path = write_png("slides-unit-decode.png", 8, 6);
        let key = crate::assets::import_asset_file(&path).expect("fixture imports as an asset");
        let store = ImageStore::new();
        store.load_blocking(&key);
        match store.get(&key) {
            ImageState::Ready(img) => {
                assert_eq!(
                    (img.width, img.height),
                    (8, 6),
                    "decoded dimensions match the file"
                );
            }
            _ => panic!("a real PNG must decode to Ready"),
        }
        store.load_blocking(
            "asset:blake3:0000000000000000000000000000000000000000000000000000000000000000",
        );
        assert!(
            matches!(
                store.get(
                    "asset:blake3:0000000000000000000000000000000000000000000000000000000000000000"
                ),
                ImageState::Failed
            ),
            "a missing asset resolves to Failed, not a panic"
        );
        assert!(
            matches!(store.get(&path.to_string_lossy()), ImageState::Failed),
            "a peer-supplied local path is never opened"
        );
    }

    #[test]
    fn image_tool_requests_the_host_portal_then_a_pick_inserts_a_real_image() {
        let mut app = SlidesApp::new();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        app.reload(&doc);
        let before = app.slide().map_or(0, |s| s.elements.len());

        // The Image tool asks the host portal for a file WITHOUT fabricating an
        // element and WITHOUT drawing any in-app picker (HIG §5).
        app.apply_hit(&mut doc, Hit::Insert(ElementKind::Image));
        assert!(
            app.portal_rx.is_some(),
            "the Image tool opens the host portal chooser (a real request is in flight)"
        );
        assert_eq!(
            app.slide().unwrap().elements.len(),
            before,
            "no element is inserted until the portal returns a chosen file"
        );

        // Feed the exact per-frame drain path a chosen-file outcome and confirm
        // it lands a real image element (with its src) in the CRDT.
        let path = write_png("slides-unit-insert.png", 12, 8);
        app.portal_rx = Some(ui::portal::inject_for_test(PortalOutcome::Opened(
            path.clone(),
        )));
        app.drain_portal(&mut doc);
        assert!(
            app.portal_rx.is_none(),
            "a resolved portal request clears the channel"
        );

        let reloaded = crate::doc::SlidesDoc::load(&doc).expect("deck round-trips");
        let el = reloaded.slides[app.cur_slide]
            .elements
            .last()
            .expect("an element was inserted");
        assert_eq!(
            el.kind,
            ElementKind::Image,
            "the inserted element is an image"
        );
        assert!(
            crate::assets::is_asset_id(&el.src),
            "only a portable asset id enters the CRDT"
        );
        assert_eq!(
            crate::assets::read_asset(&el.src).as_deref(),
            Some(&std::fs::read(&path).unwrap()[..])
        );
        assert_eq!(
            app.sel.as_deref(),
            Some(el.id.as_str()),
            "the new image is selected"
        );
    }

    #[test]
    fn a_cancelled_portal_pick_inserts_nothing() {
        // Dismissing the host chooser must not fabricate a placeholder element.
        let mut app = SlidesApp::new();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        app.reload(&doc);
        let before = app.slide().map_or(0, |s| s.elements.len());

        app.portal_rx = Some(ui::portal::inject_for_test(PortalOutcome::Cancelled));
        app.drain_portal(&mut doc);
        assert!(
            app.portal_rx.is_none(),
            "a cancel clears the in-flight channel"
        );
        app.reload(&doc);
        assert_eq!(
            app.slide().unwrap().elements.len(),
            before,
            "a cancelled pick leaves the slide untouched"
        );
    }

    // === selection drag + double-click-to-edit ============================

    /// The screen-space centre of the (single) selected element.
    fn sel_center(app: &SlidesApp) -> Point {
        let chrome = app.compute_chrome();
        let el = app.selected_element().unwrap().clone();
        crate::render::element_dest(chrome.slide_dest, chrome.scale, &el).center()
    }

    /// Regression for the "move-on-first-click" bug: an already-selected element
    /// must arm a move on the very first press (not swallow it into text-edit),
    /// and dragging must actually move it.
    #[test]
    fn selected_element_drags_on_first_grab() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text("Hi", 100.0, 100.0, 500.0, 160.0, 40.0));
        let (mut app, mut doc) = app_doc(s);
        sized(&mut app, 1280.0, 800.0);
        app.sel = Some(id.clone());

        let p = sel_center(&app);
        let mut handled = false;
        app.last_mouse = p;
        app.on_press(&mut doc, p, &mut handled);
        assert!(handled, "the press on the selected element is handled");
        assert!(
            app.drag.active(),
            "a single grab of a selected element arms a move"
        );
        assert!(!app.editing_text, "a single grab must NOT enter text edit");

        // Drag right and release — the element actually moves.
        app.drag_move(Point::new(p.x + 60.0, p.y));
        let mut h2 = false;
        app.on_release(&mut doc, &mut h2);
        let x = loaded(&doc).slides[0].element(&id).unwrap().x;
        assert!(x > 100.5, "the element moved right on first grab (x = {x})");
    }

    /// A deliberate double-click on a selected text box enters edit mode (the
    /// opt-in gesture, now that a single grab moves it).
    #[test]
    fn double_click_text_box_enters_edit() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text("Hi", 100.0, 100.0, 500.0, 160.0, 40.0));
        let (mut app, mut doc) = app_doc(s);
        sized(&mut app, 1280.0, 800.0);
        app.sel = Some(id);
        let p = sel_center(&app);

        // First click + release: selects, arms then releases a (zero-length) move.
        let mut h = false;
        app.last_mouse = p;
        app.on_press(&mut doc, p, &mut h);
        app.on_release(&mut doc, &mut h);
        assert!(!app.editing_text, "one click selects, does not edit");

        // Second click at the same spot, immediately after → double-click → edit.
        app.on_press(&mut doc, p, &mut h);
        assert!(
            app.editing_text,
            "double-click opens the text box for editing"
        );
        assert!(!app.drag.active(), "entering edit does not arm a drag");
    }

    // === thumbnail drag-to-reorder (shared PageSidebar) ==================

    /// A 5-slide editor seeded into a doc, sized so the whole rail fits.
    fn reorder_app() -> (SlidesApp, AutoCommit) {
        let mut app = SlidesApp::new();
        app.deck = crate::model::seed_deck();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        app.bootstrapped = true;
        app.reload(&doc);
        sized(&mut app, 1280.0, 900.0);
        (app, doc)
    }

    /// Dragging a thumbnail down the rail past the threshold reorders the deck
    /// and persists the move to the CRDT — the whole gesture on the real
    /// `on_press` → `CursorMoved` → `on_release` path.
    #[test]
    fn dragging_a_thumbnail_reorders_the_deck_in_the_crdt() {
        let (mut app, mut doc) = reorder_app();
        let first_id = loaded(&doc).slides[0].id.clone();

        // Source click coordinates from the shared rail's own slot rects (after
        // it syncs this frame's geometry).
        let thumbs = app.rail_thumb_rects();
        let start = thumbs[0].1.center();
        let target = thumbs[3].1.center();

        app.last_mouse = start;
        let mut h = false;
        app.on_press(&mut doc, start, &mut h);
        assert!(
            app.page_sidebar.is_pending_drag(),
            "pressing a thumbnail arms a reorder drag"
        );

        // Move the cursor onto slide 4's row through the live event path.
        app_dispatch(&mut app, &mut doc, &cursor_ev(target));
        assert!(
            app.page_sidebar.is_dragging(),
            "moving past the threshold promotes to a live reorder"
        );
        assert_eq!(
            app.page_sidebar.drag_insertion(),
            Some(3),
            "the gap tracks to the drop row"
        );

        let mut h2 = false;
        app.on_release(&mut doc, &mut h2);
        assert!(
            h2 && !app.page_sidebar.is_pending_drag(),
            "release commits and clears the drag"
        );

        let after = loaded(&doc);
        let new_pos = after
            .slides
            .iter()
            .position(|s| s.id == first_id)
            .expect("slide survived");
        assert_eq!(
            new_pos, 3,
            "the dragged slide landed where the gap showed, persisted to the CRDT"
        );
        assert_eq!(
            app.cur_slide, 3,
            "the dragged slide stays current at its new index"
        );
    }

    /// A press that never crosses the threshold is a plain click: it selects
    /// the slide and leaves the deck order untouched (the model is only mutated
    /// on a committed drag).
    #[test]
    fn a_plain_thumbnail_click_selects_without_reordering() {
        let (mut app, mut doc) = reorder_app();
        let order0: Vec<Id> = loaded(&doc).slides.iter().map(|s| s.id.clone()).collect();

        let (idx, rect) = app.rail_thumb_rects()[2];
        let p = rect.center();
        app.last_mouse = p;
        let mut h = false;
        app.on_press(&mut doc, p, &mut h);
        let mut h2 = false;
        app.on_release(&mut doc, &mut h2);

        assert_eq!(app.cur_slide, idx, "a plain click selects that slide");
        let order1: Vec<Id> = loaded(&doc).slides.iter().map(|s| s.id.clone()).collect();
        assert_eq!(
            order0, order1,
            "a plain click leaves the deck order unchanged"
        );
    }

    /// While a reorder drag is live the grabbed slide lifts out (its original
    /// index) and the reflow opens a gap at the drop row — the state the shared
    /// rail paints live feedback from. (The ghost-rect / per-item-shift geometry
    /// itself is now owned + unit-tested inside `ui`'s `PageSidebar`.)
    #[test]
    fn a_live_thumbnail_drag_lifts_a_ghost_and_opens_a_reflow_gap() {
        let (mut app, mut doc) = reorder_app();
        let thumbs = app.rail_thumb_rects();
        let start = thumbs[0].1.center();
        let target = thumbs[2].1.center();

        app.last_mouse = start;
        let mut h = false;
        app.on_press(&mut doc, start, &mut h);
        app_dispatch(&mut app, &mut doc, &cursor_ev(target));

        assert!(
            app.page_sidebar.is_dragging(),
            "the drag is live once past the threshold"
        );
        assert_eq!(
            app.page_sidebar.drag_from(),
            Some(0),
            "the grabbed slide is the one lifted out"
        );
        assert_eq!(
            app.page_sidebar.drag_insertion(),
            Some(2),
            "the reflow gap opens at the drop row"
        );
        // Escape aborts without mutating the deck.
        let order0: Vec<Id> = loaded(&doc).slides.iter().map(|s| s.id.clone()).collect();
        assert!(keyn(&mut app, &mut doc, NamedKey::Escape));
        assert!(
            !app.page_sidebar.is_pending_drag(),
            "Escape cancels the in-flight drag"
        );
        let order1: Vec<Id> = loaded(&doc).slides.iter().map(|s| s.id.clone()).collect();
        assert_eq!(
            order0, order1,
            "a cancelled drag never touched the deck order"
        );
    }

    // === .odp open / save through the app =================================

    #[test]
    fn app_saves_and_reloads_an_odp() {
        let mut app = SlidesApp::new();
        app.deck = crate::model::seed_deck();
        let path = std::env::temp_dir().join("slides-app-odp-roundtrip.odp");
        assert!(app.save_odp(&path), "deck saves to .odp");

        let mut app2 = SlidesApp::new();
        let mut doc2 = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc2, &app2.deck);
        app2.reload(&doc2);
        assert!(app2.load_odp(&mut doc2, &path), "the saved .odp loads back");
        assert_eq!(app2.deck, app.deck, "open(save(deck)) is the same deck");
        assert_eq!(
            loaded(&doc2).slides.len(),
            app.deck.slides.len(),
            "the load landed in the CRDT"
        );
    }

    #[test]
    fn app_routes_powerpoint_and_explains_its_fidelity_boundary() {
        let mut app = SlidesApp::new();
        app.deck = crate::model::seed_deck();
        let path = std::env::temp_dir().join("slides-app-pptx-roundtrip.pptx");
        assert!(app.save_presentation(&path), "deck saves to .pptx");

        let mut opened = SlidesApp::new();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &opened.deck);
        opened.doc_portal_rx = Some(inject_doc_for_test(DocOutcome::Open(path.clone())));
        opened.drain_doc_portal(&mut doc);
        wait_for_document_job(&mut opened, &mut doc);
        assert_eq!(opened.deck.title, app.deck.title);
        assert_eq!(opened.deck.slides.len(), app.deck.slides.len());
        assert!(matches!(
            opened.io_toast.as_ref(),
            Some((message, ToastKind::Success)) if message.contains("Presentation opened")
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn draining_an_open_outcome_loads_the_deck() {
        let deck = crate::model::seed_deck();
        let bytes = crate::odp::write_odp(&deck).unwrap();
        let path = std::env::temp_dir().join("slides-drain-open.odp");
        std::fs::write(&path, &bytes).unwrap();

        let mut app = SlidesApp::new();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        app.reload(&doc);
        app.doc_portal_rx = Some(inject_doc_for_test(DocOutcome::Open(path)));
        app.drain_doc_portal(&mut doc);
        wait_for_document_job(&mut app, &mut doc);
        assert!(
            app.doc_portal_rx.is_none(),
            "a resolved request clears the channel"
        );
        assert_eq!(
            app.deck.slides.len(),
            deck.slides.len(),
            "the opened deck replaced the starter"
        );
        assert!(matches!(
            app.io_toast.as_ref(),
            Some((_, ToastKind::Success))
        ));
    }

    #[test]
    fn failed_open_is_visible_instead_of_silent() {
        let mut app = SlidesApp::new();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        app.doc_portal_rx = Some(inject_doc_for_test(DocOutcome::Open(
            std::env::temp_dir().join(format!(
                "missing-tilo-slides-deck-{}.odp",
                std::process::id()
            )),
        )));
        app.drain_doc_portal(&mut doc);
        wait_for_document_job(&mut app, &mut doc);
        assert!(matches!(app.io_toast.as_ref(), Some((_, ToastKind::Error))));
    }

    #[test]
    fn cancelled_document_job_cannot_apply_a_stale_open_result() {
        let mut imported = crate::model::seed_deck();
        imported.title = "stale import".into();
        let path = std::env::temp_dir().join(format!("slides-stale-{}.odp", uuid::Uuid::new_v4()));
        std::fs::write(&path, crate::odp::write_odp(&imported).unwrap()).unwrap();
        let mut app = SlidesApp::new();
        let original = app.deck.title.clone();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        assert!(app.queue_document_open(path.clone()));
        // This is the same state transition `poll_resource_operation` performs
        // when a caller cancels the accepted invocation.
        app.active_document_generation = None;
        for _ in 0..100 {
            app.drain_document_worker(&mut doc);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(app.deck.title, original, "stale parser completion is inert");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn disconnected_document_worker_settles_the_accepted_resource_operation() {
        let mut app = SlidesApp::new();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        // Keep a sender for future queue attempts, but give the app a result
        // channel whose producing worker has already gone away.
        let (tx, _jobs) = std::sync::mpsc::sync_channel(1);
        let (results, rx) = std::sync::mpsc::sync_channel(1);
        drop(results);
        app.document_worker = Some(DocumentWorker { tx, rx });
        app.resource_invocation = Some(tilo_actions::InvocationId::new());
        app.active_document_generation = Some(7);

        app.drain_document_worker(&mut doc);

        assert!(app.document_worker.is_none());
        assert!(app.active_document_generation.is_none());
        assert!(app.resource_invocation.is_none());
        assert!(matches!(
            app.completed_resource_invocation,
            Some(tilo_actions::InvocationUpdate::Failed { .. })
        ));
        assert!(matches!(app.io_toast.as_ref(), Some((_, ToastKind::Error))));
    }

    #[test]
    fn ensure_odp_ext_appends_when_missing() {
        assert_eq!(
            super::ensure_odp_ext(std::path::Path::new("/x/talk"))
                .to_str()
                .unwrap(),
            "/x/talk.odp"
        );
        assert_eq!(
            super::ensure_odp_ext(std::path::Path::new("/x/talk.odp"))
                .to_str()
                .unwrap(),
            "/x/talk.odp"
        );
    }

    // === right-click context menus ========================================

    /// Right-clicking a canvas element selects it and opens the element menu
    /// whose rows map to the reorder / duplicate / delete actions.
    #[test]
    fn right_click_element_opens_menu_and_actions_apply() {
        let mut slide = Slide::new(Layout::Blank);
        let a = slide.add(Element::shape(
            ElementKind::Rect,
            100.0,
            100.0,
            300.0,
            200.0,
        ));
        let b = slide.add(Element::shape(
            ElementKind::Ellipse,
            500.0,
            300.0,
            200.0,
            200.0,
        ));
        let (mut app, mut doc) = app_doc(slide);
        sized(&mut app, 1280.0, 800.0);

        // Right-click element `a`.
        let chrome = app.compute_chrome();
        let ea = app.slide().unwrap().element(&a).unwrap().clone();
        let pt = crate::render::element_dest(chrome.slide_dest, chrome.scale, &ea).center();
        let mut h = false;
        app.last_mouse = pt;
        app.on_right_press(&mut doc, pt, &mut h);
        assert!(app.ctx_menu.is_open(), "right-click opens the element menu");
        assert_eq!(
            app.sel.as_deref(),
            Some(a.as_str()),
            "the clicked element is selected"
        );

        // Row 0 = Bring to front. Choosing it raises `a` above `b`.
        app.ctx_menu.click_item(0);
        if let Some(idx) = app.ctx_menu.take_selection()
            && let Some(Some(hit)) = app.ctx_hits.get(idx).cloned()
        {
            app.apply_hit(&mut doc, hit);
        }
        let za = loaded(&doc).slides[0].element(&a).unwrap().z;
        let zb = loaded(&doc).slides[0].element(&b).unwrap().z;
        assert!(za > zb, "Bring to front raised the element ({za} vs {zb})");
        assert!(!app.ctx_menu.is_open(), "choosing a row closes the menu");
    }

    /// Right-clicking a slide thumbnail selects that slide and opens the shared
    /// rail's own duplicate / move / delete menu; choosing Delete removes it.
    #[test]
    fn right_click_thumbnail_opens_slide_menu() {
        let mut app = SlidesApp::new();
        app.deck = crate::model::seed_deck();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        app.reload(&doc);
        sized(&mut app, 1280.0, 800.0);

        let (idx, rect) = app.rail_thumb_rects()[2];
        let p = rect.center();
        let mut h = false;
        app.last_mouse = p;
        app.on_right_press(&mut doc, p, &mut h);
        assert!(
            app.page_sidebar.menu_open(),
            "right-click on a thumbnail opens the rail menu"
        );
        assert_eq!(
            app.cur_slide, idx,
            "the right-clicked slide becomes current"
        );

        // The menu's Delete action (mapped from the widget's Delete row, unit
        // tested in `ui`) removes the slide via the production apply path.
        let before = loaded(&doc).slides.len();
        app.apply_rail_event(&mut doc, PageSidebarEvent::Delete(idx));
        assert_eq!(
            loaded(&doc).slides.len(),
            before - 1,
            "Delete slide removed one"
        );
    }

    // === embedded inspector widgets (NumberInput / ColorPicker) ===========

    fn app_dispatch(app: &mut SlidesApp, doc: &mut AutoCommit, ev: &WindowEvent) -> bool {
        app_dispatch_mods(app, doc, Modifiers::default(), ev)
    }

    fn app_dispatch_mods(
        app: &mut SlidesApp,
        doc: &mut AutoCommit,
        modifiers: Modifiers,
        ev: &WindowEvent,
    ) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        let root = automerge::ROOT;
        app.handle_event(
            ev,
            1.0,
            &modifiers,
            doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        handled
    }
    fn cursor_ev(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn left_press_ev() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn key_event(logical: Key, text: Option<&str>) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = text.map(winit::keyboard::SmolStr::new);
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }
    fn char_ev(s: &str) -> WindowEvent {
        key_event(Key::Character(s.into()), Some(s))
    }
    fn named_ev(k: NamedKey) -> WindowEvent {
        key_event(Key::Named(k), None)
    }

    /// The font-size `NumberInput`'s + stepper raises the size through the real
    /// event path (item: adopt the shared `NumberInput` for font size).
    #[test]
    fn font_size_number_input_steps_and_writes_through() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text("Hi", 100.0, 100.0, 500.0, 140.0, 40.0));
        let (mut app, mut doc) = app_doc(s);
        sized(&mut app, 1280.0, 800.0);
        app.sel = Some(id.clone());

        let chrome = app.compute_chrome();
        app.position_inspector_widgets(&chrome);
        let fr = chrome
            .font_input_rect
            .expect("a text selection shows the font-size field");
        let h = fr.height();
        let plus = Point::new(fr.x1 - h / 2.0, fr.center().y);
        app_dispatch(&mut app, &mut doc, &cursor_ev(plus));
        app_dispatch(&mut app, &mut doc, &left_press_ev());
        let fs = loaded(&doc).slides[0].element(&id).unwrap().font_size;
        assert_eq!(
            fs, 42.0,
            "the + stepper raised the font size by the step (2)"
        );
    }

    /// The embeddable `ColorPicker` writes a concrete hex fill through the real
    /// event path (item: integrate the `ColorPicker` for the inspector fill).
    #[test]
    fn fill_color_picker_writes_a_hex_fill() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(
            ElementKind::Rect,
            100.0,
            100.0,
            300.0,
            200.0,
        ));
        let (mut app, mut doc) = app_doc(s);
        sized(&mut app, 1280.0, 800.0);
        app.sel = Some(id.clone());

        app.apply_hit(&mut doc, Hit::OpenFillPicker);
        assert!(
            app.fill_picker_open,
            "the Custom colour button opens the picker"
        );
        let chrome = app.compute_chrome();
        app.position_inspector_widgets(&chrome);
        // Type a hex into the picker; one forwarded event flushes it through.
        app.fill_picker.set_hex_text_for_test("#FF8800");
        let c = app.fill_picker.get_rect().center();
        app_dispatch(&mut app, &mut doc, &cursor_ev(c));
        let fill = loaded(&doc).slides[0].element(&id).unwrap().fill.clone();
        assert_eq!(fill, "#FF8800", "the picker wrote a concrete hex fill");
    }

    // === write-through + interaction coverage =============================

    /// A one-slide deck seeded into a shared doc; `slide` becomes slide 0 and
    /// the mirror is reloaded from the document.
    fn app_doc(slide: Slide) -> (SlidesApp, AutoCommit) {
        let mut app = app_with(slide);
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        app.reload(&doc);
        (app, doc)
    }

    /// A one-slide deck with a single text element, selected, seeded to a doc.
    fn selected_text_app() -> (SlidesApp, AutoCommit, Id) {
        let mut s = Slide::new(Layout::TitleContent);
        let id = s.add(Element::text("Hi", 100.0, 100.0, 400.0, 100.0, 40.0));
        let (mut app, doc) = app_doc(s);
        app.sel = Some(id.clone());
        (app, doc, id)
    }

    fn keyn(app: &mut SlidesApp, doc: &mut AutoCommit, key: NamedKey) -> bool {
        let mut h = false;
        app.on_key(doc, &Key::Named(key), None, Modifiers::default(), &mut h);
        h
    }
    fn keyc(app: &mut SlidesApp, doc: &mut AutoCommit, ch: &str) -> bool {
        let mut h = false;
        app.on_key(
            doc,
            &Key::Character(ch.into()),
            Some(ch),
            Modifiers::default(),
            &mut h,
        );
        h
    }
    fn loaded(doc: &AutoCommit) -> Deck {
        crate::doc::SlidesDoc::load(doc).expect("deck round-trips from the document")
    }
    fn sized(app: &mut SlidesApp, w: f64, h: f64) {
        app.rect = Rect::new(0.0, 0.0, w, h);
        app.sync_flaps(0.0, false);
    }

    // --- deck title rename (was display-only: set_title had no caller) ------

    #[test]
    fn clicking_the_toolbar_title_starts_a_rename_and_typing_writes_through() {
        let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
        sized(&mut app, 1280.0, 800.0);
        let title0 = app.deck.title.clone();
        let chrome = app.compute_chrome();
        let p = chrome.title_rect.center();
        assert!(
            app.rect.contains(p),
            "the title click target is inside the window"
        );

        let mut handled = false;
        app.last_mouse = p;
        app.on_press(&mut doc, p, &mut handled);
        assert!(
            handled && app.editing_title,
            "clicking the title enters inline rename"
        );

        // Typing routes through the shared TextInput (the live handle_event
        // path) and splices into the CRDT title.
        app_dispatch(&mut app, &mut doc, &char_ev("!"));
        app_dispatch(&mut app, &mut doc, &char_ev("?"));
        assert!(
            !app.help_overlay.is_visible(),
            "'?' while renaming types into the title, not the help overlay"
        );
        // Enter commits the rename.
        app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::Enter));
        assert!(!app.editing_title, "Enter commits the rename");
        assert_eq!(
            loaded(&doc).title,
            format!("{title0}!?"),
            "the rename persisted to the CRDT"
        );
    }

    /// The inline title editor is the shared `TextInput`, so it supports real
    /// selection — Ctrl+A selects the whole title, then typing replaces it.
    #[test]
    fn title_input_supports_select_all_and_replace() {
        let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
        sized(&mut app, 1280.0, 800.0);
        app.begin_title_edit();
        // Select all + confirm the field reports a full-span selection.
        app.title_input.select_all();
        let sel = app.title_input.selection_range_public();
        assert!(
            sel.is_some(),
            "Ctrl+A selects the whole title in the TextInput"
        );
        // Typing over the selection replaces the entire title.
        app_dispatch(&mut app, &mut doc, &char_ev("X"));
        app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::Enter));
        assert_eq!(
            loaded(&doc).title,
            "X",
            "typing over the selection replaced the title"
        );
    }

    #[test]
    fn title_backspace_and_click_away_commit_persist_to_the_crdt() {
        let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
        sized(&mut app, 1280.0, 800.0);
        app.begin_title_edit();
        // Backspace deletes the last title char through the TextInput → CRDT.
        let title0 = app.deck.title.clone();
        app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::Backspace));
        let mut expect: String = title0;
        expect.pop();
        assert_eq!(
            loaded(&doc).title,
            expect,
            "backspace spliced one char out of the title"
        );

        // Add a char, then click away from the title → commits (leaves editing).
        app_dispatch(&mut app, &mut doc, &char_ev("Z"));
        let chrome = app.compute_chrome();
        let away = Point::new(chrome.slide_dest.center().x, chrome.slide_dest.center().y);
        assert!(!chrome.title_rect.contains(away));
        app_dispatch(&mut app, &mut doc, &cursor_ev(away));
        app_dispatch(&mut app, &mut doc, &left_press_ev());
        assert!(!app.editing_title, "a click away commits the rename");
        assert_eq!(
            loaded(&doc).title,
            format!("{expect}Z"),
            "the edit persisted"
        );
    }

    // --- '?' help in Present mode (was a no-op that soft-locked nav) --------

    #[test]
    fn help_overlay_is_visible_and_dismissable_in_present_mode() {
        let mut app = presenting_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        assert!(!app.help_overlay.is_visible());
        // '?' opens the overlay even while presenting.
        assert!(press_char(&mut app, "?"));
        assert!(
            app.help_overlay.is_visible(),
            "the help overlay is shown in Present mode (the exact gate `draw` uses)"
        );
        // And it actually paints over the present surface without panicking.
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let palette = app.palette(&theme);
        app.help_overlay.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            app.rect,
            &palette,
        );

        // While open it is modal (a nav key is swallowed, does not advance).
        let before = app.cur_slide;
        assert!(press_named(&mut app, NamedKey::ArrowRight));
        assert_eq!(
            app.cur_slide, before,
            "help is modal: nav is captured, not applied"
        );
        // Dismiss with '?', and navigation resumes — no soft-lock.
        assert!(press_char(&mut app, "?"));
        assert!(!app.help_overlay.is_visible(), "'?' dismisses the overlay");
        assert!(press_named(&mut app, NamedKey::ArrowRight));
        assert_eq!(
            app.cur_slide,
            before + 1,
            "navigation resumes once help is closed"
        );
    }

    // --- text caret alignment (was hard-coded to the left edge) -------------

    #[test]
    fn caret_tracks_center_and_right_alignment_not_just_left() {
        let app = app_with(Slide::new(Layout::Blank));
        let mut tr = TextRenderer::new();
        let er = Rect::new(100.0, 100.0, 700.0, 200.0);
        let text = "Hello";
        let end = text.chars().count();
        let geo = |al: TextAlign, tr: &mut TextRenderer| {
            let style = TextBoxStyle {
                rect: er,
                font_px: 40.0,
                weight: TextWeight::Regular,
                italic: false,
                align: al,
                center_vertically: true,
            };
            app.caret_geometry(tr, text, end, style).0
        };
        let left = geo(TextAlign::Left, &mut tr);
        let center = geo(TextAlign::Center, &mut tr);
        let right = geo(TextAlign::Right, &mut tr);
        // Right-aligned caret at the line end sits at the box's right edge.
        assert!(
            (right.x - er.x1).abs() < 1.0,
            "right caret at line end hits the right edge (x={})",
            right.x
        );
        // The three alignments are genuinely distinct (the old stub returned the
        // same left-anchored x for every arm).
        assert!(
            left.x < center.x && center.x < right.x,
            "l={} c={} r={}",
            left.x,
            center.x,
            right.x
        );
        assert!(
            (center.x - left.x).abs() > 1.0,
            "center caret differs from the left-anchored position"
        );
    }

    // --- caret line navigation (ArrowUp/Down did nothing) -------------------

    #[test]
    fn caret_line_move_keeps_column_and_clamps_at_edges() {
        let t = "abc\ndef\nghij";
        // Down/up keep the column.
        assert_eq!(
            caret_line_move(t, 2, 1),
            6,
            "down from line0 col2 → line1 col2"
        );
        assert_eq!(
            caret_line_move(t, 6, -1),
            2,
            "up from line1 col2 → line0 col2"
        );
        // A longer column clamps to a shorter target line's length.
        assert_eq!(
            caret_line_move(t, 12, -1),
            7,
            "up from line2 col4 clamps to line1 col3"
        );
        // Edges are no-ops.
        assert_eq!(
            caret_line_move(t, 2, -1),
            2,
            "up on the first line is unchanged"
        );
        assert_eq!(
            caret_line_move(t, 10, 1),
            10,
            "down on the last line is unchanged"
        );
    }

    #[test]
    fn caret_line_home_end_snaps_within_the_line() {
        let t = "abc\ndef";
        assert_eq!(caret_line_home_end(t, 5, false), 4, "Home → line start");
        assert_eq!(caret_line_home_end(t, 5, true), 7, "End → line end");
        assert_eq!(caret_line_home_end(t, 1, false), 0);
        assert_eq!(caret_line_home_end(t, 1, true), 3);
    }

    // --- shared multiline speaker-notes editor ------------------------------

    #[test]
    fn notes_typing_writes_through_and_up_down_move_the_caret() {
        let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
        sized(&mut app, 1280.0, 800.0);
        // Seed two lines of notes on slide 0.
        crate::doc::SlidesDoc::splice_notes(&mut doc, 0, 0, 0, "line one\nsecond");
        app.reload(&doc);
        app.apply_hit(&mut doc, Hit::EditNotes);
        assert!(app.editing_notes, "clicking notes enters the notes editor");
        let len = "line one\nsecond".len();
        assert_eq!(
            app.notes_input.cursor_pos(),
            len,
            "caret opens at the end of the notes"
        );

        // The shared multiline editor resolves vertical movement from the same
        // wrapped layout it paints, so build one deterministic inspector frame.
        let nb = app.compute_chrome().notes_box.expect("notes box");
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        app.draw_notes(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            nb,
            "line one\nsecond",
        );

        // ArrowUp jumps to the first line (keeping the column), ArrowDown back.
        app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::ArrowUp));
        assert!(
            app.notes_input.cursor_pos() < "line one".len(),
            "ArrowUp moved the notes caret onto the first shaped line"
        );
        app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::ArrowDown));
        assert_eq!(
            app.notes_input.cursor_pos(),
            len,
            "ArrowDown returns to the second line"
        );

        // Typing splices into the CRDT notes at the caret.
        app_dispatch(&mut app, &mut doc, &char_ev("!"));
        assert_eq!(
            loaded(&doc).slides[0].notes,
            "line one\nsecond!",
            "notes edit persisted to the CRDT"
        );
    }

    // --- element text editing -----------------------------------------------

    #[test]
    fn editing_element_text_types_and_deletes_through_the_crdt() {
        let (mut app, mut doc, id) = selected_text_app();
        // Enter opens the text editor on the selected box.
        assert!(keyn(&mut app, &mut doc, NamedKey::Enter));
        assert!(app.editing_text);
        assert_eq!(app.caret, "Hi".chars().count());
        assert!(keyc(&mut app, &mut doc, "!"));
        assert_eq!(
            loaded(&doc).slides[0].element(&id).unwrap().text,
            "Hi!",
            "typed char persisted"
        );
        assert!(keyn(&mut app, &mut doc, NamedKey::Backspace));
        assert_eq!(
            loaded(&doc).slides[0].element(&id).unwrap().text,
            "Hi",
            "backspace persisted"
        );
    }

    #[test]
    fn second_click_on_a_selected_text_box_enters_edit_mode() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text("Hi", 100.0, 100.0, 400.0, 120.0, 40.0));
        let (mut app, mut doc) = app_doc(s);
        sized(&mut app, 1280.0, 800.0);
        let chrome = app.compute_chrome();
        let el = app.slide().unwrap().element(&id).unwrap().clone();
        let p = render::element_dest(chrome.slide_dest, chrome.scale, &el).center();
        // First click selects.
        let mut h = false;
        app.last_mouse = p;
        app.on_press(&mut doc, p, &mut h);
        assert_eq!(app.sel.as_deref(), Some(id.as_str()));
        assert!(!app.editing_text);
        app.on_release(&mut doc, &mut h);
        // Second click on the already-selected box enters edit.
        let mut h2 = false;
        app.on_press(&mut doc, p, &mut h2);
        assert!(
            app.editing_text,
            "a second click on a selected text box edits it"
        );
    }

    // --- canvas move / resize persistence -----------------------------------

    #[test]
    fn dragging_an_element_persists_the_move_to_the_crdt() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(ElementKind::Rect, 300.0, 300.0, 160.0, 90.0));
        let (mut app, mut doc) = app_doc(s);
        sized(&mut app, 1280.0, 800.0);
        let chrome = app.compute_chrome();
        let el = app.slide().unwrap().element(&id).unwrap().clone();
        let start = render::element_dest(chrome.slide_dest, chrome.scale, &el).center();

        let mut h = false;
        app.last_mouse = start;
        app.on_press(&mut doc, start, &mut h);
        assert!(
            matches!(app.drag, Drag::Move { .. }),
            "pressing an element starts a move drag"
        );
        // Move a good distance so no guide snaps it back to the origin.
        app.drag_move(Point::new(start.x - 130.0, start.y + 70.0));
        let mut h2 = false;
        app.on_release(&mut doc, &mut h2);
        assert!(!app.drag.active(), "release ends the drag");

        let persisted = loaded(&doc).slides[0].element(&id).unwrap().clone();
        let mirror = app.slide().unwrap().element(&id).unwrap().clone();
        assert!(
            (persisted.x - mirror.x).abs() < 0.001,
            "the CRDT matches the mirror after release"
        );
        assert!(
            (persisted.x - 300.0).abs() > 5.0,
            "the move actually changed x and persisted (x={})",
            persisted.x
        );
        assert!(
            (persisted.y - 300.0).abs() > 5.0,
            "the move actually changed y and persisted (y={})",
            persisted.y
        );
    }

    #[test]
    fn resizing_via_a_selection_handle_persists_to_the_crdt() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(
            ElementKind::Rect,
            300.0,
            300.0,
            200.0,
            120.0,
        ));
        let (mut app, mut doc) = app_doc(s);
        sized(&mut app, 1280.0, 800.0);
        app.sel = Some(id.clone());
        let chrome = app.compute_chrome();
        let el = app.slide().unwrap().element(&id).unwrap().clone();
        let (_, hr) = app
            .handles_of(chrome.slide_dest, chrome.scale, &el)
            .into_iter()
            .find(|(h, _)| matches!(h, Handle::Br))
            .expect("a bottom-right resize handle");
        let start = hr.center();

        let mut h = false;
        app.last_mouse = start;
        app.on_press(&mut doc, start, &mut h);
        assert!(
            matches!(app.drag, Drag::Resize { .. }),
            "pressing a handle starts a resize"
        );
        app.drag_move(Point::new(start.x + 120.0, start.y + 90.0));
        let mut h2 = false;
        app.on_release(&mut doc, &mut h2);

        let persisted = loaded(&doc).slides[0].element(&id).unwrap().clone();
        let mirror = app.slide().unwrap().element(&id).unwrap().clone();
        assert!(
            (persisted.w - mirror.w).abs() < 0.001 && (persisted.h - mirror.h).abs() < 0.001,
            "CRDT matches mirror"
        );
        assert!(
            persisted.w > el.w + 1.0,
            "Br drag grew width and persisted ({} → {})",
            el.w,
            persisted.w
        );
        assert!(
            persisted.h > el.h + 1.0,
            "Br drag grew height and persisted ({} → {})",
            el.h,
            persisted.h
        );
    }

    // --- inspector property edits (every writer, on the real hit path) ------

    #[test]
    fn every_inspector_property_edit_writes_through_to_the_crdt() {
        let (mut app, mut doc, id) = selected_text_app();
        let el0 = app.selected_element().unwrap().clone();

        app.apply_hit(&mut doc, Hit::SetFill("accent".into()));
        assert_eq!(loaded(&doc).slides[0].element(&id).unwrap().fill, "accent");

        app.apply_hit(&mut doc, Hit::SetStroke("text".into()));
        let after_stroke = loaded(&doc).slides[0].element(&id).unwrap().clone();
        assert_eq!(after_stroke.stroke, "text");
        assert!(
            after_stroke.stroke_width >= 1.0,
            "a freshly stroked element gets a default width"
        );

        // Font size now writes through the NumberInput's commit path
        // (`set_sel_f64`), exercised directly here.
        app.set_sel_f64(&mut doc, "font_size", el0.font_size + 6.0);
        assert!(
            (loaded(&doc).slides[0].element(&id).unwrap().font_size - (el0.font_size + 6.0)).abs()
                < 0.001
        );

        app.apply_hit(&mut doc, Hit::ToggleBold);
        assert!(loaded(&doc).slides[0].element(&id).unwrap().bold);
        app.apply_hit(&mut doc, Hit::ToggleItalic);
        assert!(loaded(&doc).slides[0].element(&id).unwrap().italic);

        app.apply_hit(&mut doc, Hit::SetAlign(TextAlign::Center));
        assert_eq!(
            loaded(&doc).slides[0].element(&id).unwrap().align,
            TextAlign::Center
        );
        app.apply_hit(&mut doc, Hit::SetAlign(TextAlign::Justify));
        assert_eq!(
            loaded(&doc).slides[0].element(&id).unwrap().align,
            TextAlign::Justify
        );

        app.apply_hit(&mut doc, Hit::BringFront);
        let zf = loaded(&doc).slides[0].element(&id).unwrap().z;
        app.apply_hit(&mut doc, Hit::SendBack);
        let zb = loaded(&doc).slides[0].element(&id).unwrap().z;
        assert!(zb < zf, "SendBack lowers z below BringFront ({zf} → {zb})");

        app.apply_hit(&mut doc, Hit::SetLayout(Layout::Blank));
        assert_eq!(loaded(&doc).slides[0].layout, Layout::Blank);
        app.apply_hit(&mut doc, Hit::SetBackground(Background::Accent));
        assert_eq!(loaded(&doc).slides[0].background, Background::Accent);
        app.apply_hit(&mut doc, Hit::SetPalette(PaletteKind::Amber));
        assert_eq!(loaded(&doc).palette, PaletteKind::Amber);

        let n = loaded(&doc).slides[0].elements.len();
        app.apply_hit(&mut doc, Hit::DuplicateElement);
        assert_eq!(
            loaded(&doc).slides[0].elements.len(),
            n + 1,
            "duplicate adds an element"
        );
        app.apply_hit(&mut doc, Hit::DeleteElement);
        assert_eq!(
            loaded(&doc).slides[0].elements.len(),
            n,
            "delete removes one"
        );
    }

    // --- slide ops via the app handlers -------------------------------------

    #[test]
    fn slide_ops_via_hit_handlers_update_the_deck_and_current_slide() {
        let mut app = SlidesApp::new();
        app.deck = crate::model::seed_deck();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        app.bootstrapped = true;
        app.cur_slide = 1;
        app.reload(&doc);
        let n0 = loaded(&doc).slides.len();

        // New slide inserts after the current one and follows it.
        app.apply_hit(&mut doc, Hit::NewSlide);
        assert_eq!(loaded(&doc).slides.len(), n0 + 1);
        assert_eq!(app.cur_slide, 2, "the new slide becomes current");

        // Duplicate inserts a copy after current.
        app.apply_hit(&mut doc, Hit::DuplicateSlide);
        assert_eq!(loaded(&doc).slides.len(), n0 + 2);
        assert_eq!(app.cur_slide, 3);

        // Move up / down (the rail menu's Reorder events) reorders and tracks
        // cur_slide, on the production apply path.
        let moved_id = loaded(&doc).slides[3].id.clone();
        app.apply_rail_event(&mut doc, PageSidebarEvent::Reorder { from: 3, to: 2 });
        assert_eq!(app.cur_slide, 2);
        assert_eq!(
            loaded(&doc).slides[2].id,
            moved_id,
            "move up reordered the deck"
        );
        app.apply_rail_event(&mut doc, PageSidebarEvent::Reorder { from: 2, to: 3 });
        assert_eq!(app.cur_slide, 3);
        assert_eq!(
            loaded(&doc).slides[3].id,
            moved_id,
            "move down reordered it back"
        );

        // Delete removes the current slide.
        app.apply_hit(&mut doc, Hit::DeleteSlide);
        assert_eq!(loaded(&doc).slides.len(), n0 + 1);
    }

    #[test]
    fn delete_slide_keeps_at_least_one_slide() {
        let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
        assert_eq!(loaded(&doc).slides.len(), 1);
        app.apply_hit(&mut doc, Hit::DeleteSlide);
        assert_eq!(
            loaded(&doc).slides.len(),
            1,
            "the last slide can't be deleted"
        );
    }

    // --- edit-mode keyboard: nudge, Tab cycle, Delete -----------------------

    #[test]
    fn arrow_nudge_tab_cycle_and_delete_key_edit_the_crdt() {
        let mut s = Slide::new(Layout::Blank);
        let a = s.add(Element::shape(
            ElementKind::Rect,
            200.0,
            200.0,
            100.0,
            100.0,
        ));
        let b = s.add(Element::shape(
            ElementKind::Ellipse,
            500.0,
            400.0,
            100.0,
            100.0,
        ));
        let (mut app, mut doc) = app_doc(s);
        app.sel = Some(a.clone());

        // Arrow nudges the selected element's position (persisted).
        let x0 = loaded(&doc).slides[0].element(&a).unwrap().x;
        assert!(keyn(&mut app, &mut doc, NamedKey::ArrowRight));
        let x1 = loaded(&doc).slides[0].element(&a).unwrap().x;
        assert!(
            x1 > x0 + 1.0,
            "ArrowRight nudged and persisted x ({x0} → {x1})"
        );

        // Tab cycles selection to the next element (by z order).
        assert!(keyn(&mut app, &mut doc, NamedKey::Tab));
        assert_eq!(app.sel.as_deref(), Some(b.as_str()), "Tab cycles selection");

        // Delete removes the selected element from the CRDT.
        let n = loaded(&doc).slides[0].elements.len();
        assert!(keyn(&mut app, &mut doc, NamedKey::Delete));
        assert_eq!(
            loaded(&doc).slides[0].elements.len(),
            n - 1,
            "Delete removed the element"
        );
        assert!(app.sel.is_none(), "selection clears after delete");
    }

    // --- overview click opens the slide -------------------------------------

    #[test]
    fn clicking_an_overview_card_opens_that_slide_in_the_editor() {
        let mut app = SlidesApp::new();
        app.deck = crate::model::seed_deck();
        app.bootstrapped = true;
        app.mode = Mode::Overview;
        app.rect = Rect::new(0.0, 0.0, 900.0, 700.0);
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        app.reload(&doc);

        let (_, thumbs, _) = app.compute_overview();
        let (idx, rect) = thumbs.get(2).copied().expect("a third overview card");
        let mut handled = false;
        app.last_mouse = rect.center();
        app.on_press(&mut doc, rect.center(), &mut handled);
        assert!(handled, "clicking an overview card is handled");
        assert_eq!(app.mode, Mode::Edit, "opening a card returns to the editor");
        assert_eq!(
            app.cur_slide, idx,
            "the clicked card's slide is now current"
        );
    }

    // === shared caret / selection core (HIG §3) ===========================

    fn key_named_mods(
        app: &mut SlidesApp,
        doc: &mut AutoCommit,
        key: NamedKey,
        state: winit::keyboard::ModifiersState,
    ) -> bool {
        let mut h = false;
        let mods = Modifiers::from(state);
        app.on_key(doc, &Key::Named(key), None, mods, &mut h);
        h
    }
    fn key_char_mods(
        app: &mut SlidesApp,
        doc: &mut AutoCommit,
        ch: &str,
        state: winit::keyboard::ModifiersState,
    ) -> bool {
        let mut h = false;
        let mods = Modifiers::from(state);
        app.on_key(doc, &Key::Character(ch.into()), Some(ch), mods, &mut h);
        h
    }

    /// Shift+Arrow through the real key path builds a live selection over the
    /// CRDT-backed text (the shared `ui` selection core, not a per-app buffer).
    #[test]
    fn shift_arrow_extends_a_selection_over_the_crdt_text() {
        use winit::keyboard::ModifiersState;
        let (mut app, mut doc, _id) = selected_text_app();
        app.editing_text = true;
        app.place_caret(app.cur_text_len(), false);
        assert!(!app.has_text_selection(), "starts with no selection");
        assert!(key_named_mods(
            &mut app,
            &mut doc,
            NamedKey::ArrowLeft,
            ModifiersState::SHIFT
        ));
        assert!(key_named_mods(
            &mut app,
            &mut doc,
            NamedKey::ArrowLeft,
            ModifiersState::SHIFT
        ));
        assert!(
            app.has_text_selection(),
            "shift+arrow builds a live selection"
        );
        assert_eq!(app.selected_text(), "Hi", "the selection spans both chars");
    }

    /// Ctrl+Arrow moves word-wise via the shared word-boundary walker.
    #[test]
    fn ctrl_arrow_moves_word_wise() {
        use winit::keyboard::ModifiersState;
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text(
            "Hello world",
            100.0,
            100.0,
            600.0,
            120.0,
            40.0,
        ));
        let (mut app, mut doc) = app_doc(s);
        app.sel = Some(id);
        app.editing_text = true;
        app.place_caret(app.cur_text_len(), false);
        assert!(key_named_mods(
            &mut app,
            &mut doc,
            NamedKey::ArrowLeft,
            ModifiersState::CONTROL
        ));
        assert_eq!(
            app.active_caret(),
            6,
            "ctrl+left jumps to the start of the last word"
        );
        assert!(
            !app.has_text_selection(),
            "a plain word move collapses the selection"
        );
    }

    /// Ctrl+A then Backspace clears the whole element text in the CRDT.
    #[test]
    fn select_all_then_backspace_clears_the_element_text_in_the_crdt() {
        use winit::keyboard::ModifiersState;
        let (mut app, mut doc, id) = selected_text_app();
        app.editing_text = true;
        app.place_caret(0, false);
        assert!(key_char_mods(
            &mut app,
            &mut doc,
            "a",
            ModifiersState::CONTROL
        ));
        assert_eq!(app.selected_text(), "Hi", "Ctrl+A selects the whole text");
        assert!(keyn(&mut app, &mut doc, NamedKey::Backspace));
        assert_eq!(
            loaded(&doc).slides[0].element(&id).unwrap().text,
            "",
            "the selected text was deleted from the CRDT"
        );
    }

    /// Typing over a live selection replaces it in the CRDT.
    #[test]
    fn typing_over_a_selection_replaces_it_in_the_crdt() {
        let (mut app, mut doc, id) = selected_text_app();
        app.editing_text = true;
        app.select_all_text();
        assert!(keyc(&mut app, &mut doc, "X"), "a plain char is inserted");
        assert_eq!(
            loaded(&doc).slides[0].element(&id).unwrap().text,
            "X",
            "the selection was replaced, not appended to"
        );
    }

    /// The Delete key forward-deletes the char after the caret in the CRDT.
    #[test]
    fn forward_delete_removes_the_char_after_the_caret_in_the_crdt() {
        let (mut app, mut doc, id) = selected_text_app();
        app.editing_text = true;
        app.place_caret(0, false);
        assert!(keyn(&mut app, &mut doc, NamedKey::Delete));
        assert_eq!(
            loaded(&doc).slides[0].element(&id).unwrap().text,
            "i",
            "Delete removed the first char"
        );
    }

    /// Ctrl+X copies then removes the selection from the CRDT.
    #[test]
    fn cut_removes_the_selection_from_the_crdt() {
        use winit::keyboard::ModifiersState;
        let (mut app, mut doc, id) = selected_text_app();
        app.editing_text = true;
        app.select_all_text();
        assert!(app.copy_selection(), "there is a selection to cut");
        assert!(key_char_mods(
            &mut app,
            &mut doc,
            "x",
            ModifiersState::CONTROL
        ));
        assert_eq!(
            loaded(&doc).slides[0].element(&id).unwrap().text,
            "",
            "cut removed the selected text from the CRDT"
        );
    }

    /// Selection replacement, IME and multiline insertion all travel through
    /// the real root-widget event path into the directly-bound notes text.
    #[test]
    fn notes_selection_ime_and_multiline_edit_the_bound_crdt_text() {
        use winit::keyboard::ModifiersState;
        let mut s = Slide::new(Layout::TitleContent);
        s.notes = "abc".into();
        let (mut app, mut doc) = app_doc(s);
        sized(&mut app, 1280.0, 800.0);
        app.apply_hit(&mut doc, Hit::EditNotes);

        app_dispatch_mods(
            &mut app,
            &mut doc,
            Modifiers::from(ModifiersState::CONTROL),
            &char_ev("a"),
        );
        assert_eq!(
            app.notes_input.selection_range_public(),
            Some((0, 3)),
            "Ctrl+A selects the notes in the shared editor"
        );

        app_dispatch(
            &mut app,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Commit("東京".into())),
        );
        app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::Enter));
        app_dispatch(&mut app, &mut doc, &char_ev("agenda"));
        assert_eq!(
            loaded(&doc).slides[0].notes,
            "東京\nagenda",
            "IME replacement and newline insertion persist to the CRDT"
        );

        let handled = app_dispatch_mods(
            &mut app,
            &mut doc,
            Modifiers::from(ModifiersState::CONTROL),
            &char_ev("z"),
        );
        assert!(
            !handled,
            "Ctrl+Z falls through to the toolkit's global undo handler"
        );
    }

    /// A press inside the box currently being edited places the caret (and arms
    /// a selection drag) rather than starting a box move.
    #[test]
    fn clicking_inside_the_editing_text_arms_caret_placement_not_a_box_move() {
        let (mut app, mut doc, _id) = selected_text_app();
        sized(&mut app, 1280.0, 800.0);
        app.editing_text = true;
        let chrome = app.compute_chrome();
        let er = render::element_dest(
            chrome.slide_dest,
            chrome.scale,
            app.selected_element().unwrap(),
        );
        let p = er.center();
        app.last_mouse = p;
        let mut h = false;
        app.on_press(&mut doc, p, &mut h);
        assert!(h, "the press is handled");
        assert!(
            app.text_drag,
            "a press inside the editing box starts a selection drag"
        );
        assert!(
            app.caret_click.is_some(),
            "the click is queued to place the caret"
        );
        assert!(!app.drag.active(), "the box is not being moved");
    }

    // === drag-to-delete zone ==============================================

    /// Dropping a dragged slide thumbnail on the red dashed delete zone removes
    /// that slide from the CRDT (the whole `on_press` → drag → `on_release` path).
    #[test]
    fn dropping_a_slide_on_the_delete_zone_removes_it_from_the_crdt() {
        let (mut app, mut doc) = reorder_app();
        let victim_id = loaded(&doc).slides[1].id.clone();
        let before = loaded(&doc).slides.len();

        let start = app.rail_thumb_rects()[1].1.center();
        app.last_mouse = start;
        let mut h = false;
        app.on_press(&mut doc, start, &mut h);

        // Drag down into the delete zone pinned at the bottom of the rail.
        let drop = app.slide_delete_zone(app.rail_rect()).center();
        app_dispatch(&mut app, &mut doc, &cursor_ev(drop));
        assert!(
            app.page_sidebar.is_dragging(),
            "the drag is live past the threshold"
        );
        assert!(
            app.over_delete_zone(),
            "the pointer is over the delete drop-zone"
        );

        let mut h2 = false;
        app.on_release(&mut doc, &mut h2);
        assert!(h2, "the drop is handled");

        let after = loaded(&doc);
        assert_eq!(
            after.slides.len(),
            before - 1,
            "the dropped slide was deleted"
        );
        assert!(
            !after.slides.iter().any(|s| s.id == victim_id),
            "the exact dragged slide is gone from the CRDT"
        );
        assert!(
            !app.page_sidebar.is_pending_drag(),
            "the drag is cleared after the drop"
        );
    }

    /// The delete zone is only active while a slide is being dragged.
    #[test]
    fn delete_zone_is_inert_when_not_dragging() {
        let (mut app, _doc) = reorder_app();
        let zone = app.slide_delete_zone(app.rail_rect());
        app.last_mouse = zone.center();
        assert!(
            !app.over_delete_zone(),
            "no drag in progress → the zone is inert"
        );
    }

    #[test]
    fn accessibility_exposes_deck_and_current_slide() {
        use ui::widget::Widget as _;

        let mut app = SlidesApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let tree = app.accessibility().expect("presentation semantics");
        assert_eq!(tree.role, ui::a11y::Role::Document);
        let slides = tree
            .children
            .iter()
            .find(|node| node.role == ui::a11y::Role::List)
            .expect("semantic slide list");
        assert_eq!(slides.role, ui::a11y::Role::List);
        assert_eq!(slides.children.len(), app.deck.slides.len());
        assert!(slides.children.iter().any(|slide| slide.state.selected));
        assert!(
            slides
                .children
                .iter()
                .all(|slide| slide.action_target.is_some())
        );
    }

    #[test]
    fn accessibility_activation_uses_stable_slide_identity_and_modal_help_owns_tree() {
        use ui::widget::Widget as _;

        let mut app = SlidesApp::demo(None);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let target_id = app.deck.slides[3].id.clone();
        assert!(Widget::perform_semantic_action(
            &mut app,
            &format!("slides.slide:{target_id}"),
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
        assert_eq!(
            app.slide().map(|slide| slide.id.as_str()),
            Some(target_id.as_str())
        );

        app.help_overlay.show();
        let tree = app.accessibility().expect("modal help semantics");
        assert_eq!(tree.role, ui::a11y::Role::Dialog);
        assert!(tree.state.modal);
        assert!(Widget::perform_semantic_action(
            &mut app,
            "slides.help.close",
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
        assert!(!app.help_overlay.is_visible());
    }

    #[test]
    fn brokered_text_reveal_restores_exact_slide_element_and_selection() {
        use ui::widget::Widget as _;

        let mut app = SlidesApp::demo(None);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let slide_index = 2;
        let slide = &app.deck.slides[slide_index];
        let slide_id = slide.id.clone();
        let element = slide
            .elements
            .iter()
            .find(|element| element.kind == ElementKind::Text && !element.text.is_empty())
            .unwrap();
        let element_id = element.id.clone();
        let text_len = element.text.chars().count();
        let payload = serde_json::to_vec(&crate::actions::SlidesOperation::RevealElement {
            slide: slide_id,
            element: element_id.clone(),
            edit_text: true,
        })
        .unwrap();

        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                tilo_actions::OperationKind::View,
                crate::actions::SLIDES_OPERATION_TYPE,
                &payload,
                &doc,
                &automerge::ROOT,
            )
            .unwrap()
        );
        assert_eq!(app.cur_slide, slide_index);
        assert_eq!(app.sel.as_deref(), Some(element_id.as_str()));
        assert!(app.editing_text);
        assert_eq!(
            app.selection(),
            Selection {
                anchor: 0,
                cursor: text_len,
            }
        );
        assert_eq!(app.mode, Mode::Edit);
    }
    #[test]
    fn history_restore_rebinds_notes_before_first_input_without_intervening_sync() {
        let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
        sized(&mut app, 1280., 800.);
        SlidesDoc::splice_notes(&mut doc, 0, 0, 0, "Selected notes");
        let selected = doc.get_heads();
        SlidesDoc::splice_notes(&mut doc, 0, 0, 0, "LIVE ");
        app.reload(&doc);
        app.begin_notes_edit(&mut doc);
        let old_obj = app.notes_input.bound_obj().unwrap().clone();
        let heads = doc.get_heads();
        let plan = ui::history_mode::restore::prepare_content_restore(
            &doc,
            &crate::framework_history::SCHEMA,
            &heads,
            &selected,
        )
        .unwrap();
        ui::history_mode::restore::apply_prepared_content_restore(&mut doc, &plan).unwrap();
        app_dispatch(&mut app, &mut doc, &char_ev("!"));
        assert_eq!(
            SlidesDoc::load(&doc).unwrap().slides[0].notes,
            "Selected notes!"
        );
        assert_eq!(doc.text(old_obj).unwrap(), "LIVE Selected notes");
    }
}

#[cfg(test)]
mod history_reconcile_tests {
    use super::*;
    #[test]
    fn canonical_update_interrupts_drag_before_sync_can_write_stale_cache() {
        let mut app = SlidesApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        doc.get_heads();
        let id = app.deck.slides[0].elements[0].id.clone();
        app.drag = Drag::Move {
            id,
            start: Point::ZERO,
            orig_x: 0.,
            orig_y: 0.,
        };
        SlidesDoc::set_title(&mut doc, "Restored canonical deck");
        let heads = doc.get_heads();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.deck.title, "Restored canonical deck");
        assert!(!app.drag.active());
        assert_eq!(doc.get_heads(), heads);
    }
}

#[cfg(test)]
mod history_interruption_tests {
    use super::*;
    fn restore(doc: &mut AutoCommit, selected: &[automerge::ChangeHash]) {
        let heads = doc.get_heads();
        let plan = ui::history_mode::restore::prepare_content_restore(
            doc,
            &crate::framework_history::SCHEMA,
            &heads,
            selected,
        )
        .unwrap();
        ui::history_mode::restore::apply_prepared_content_restore(doc, &plan).unwrap();
    }
    fn assert_save_settlement(
        app: &mut SlidesApp,
        doc: &AutoCommit,
        invocation: tilo_actions::InvocationId,
        succeeds: bool,
        actual_error: Option<&str>,
    ) {
        let update = app
            .poll_resource_operation(invocation, false, doc, &automerge::ROOT)
            .unwrap();
        match update {
            tilo_actions::InvocationUpdate::Succeeded {
                invocation: settled,
                ..
            } if succeeds => {
                assert_eq!(settled, invocation);
            }
            tilo_actions::InvocationUpdate::Failed {
                invocation: settled,
                error,
            } if !succeeds => {
                assert_eq!(settled, invocation);
                assert_eq!(Some(error.message.as_str()), actual_error);
            }
            other => panic!("unexpected export settlement: {other:?}"),
        }
        assert!(
            app.poll_resource_operation(invocation, false, doc, &automerge::ROOT)
                .is_none()
        );
    }

    fn assert_export_snapshot(path: &std::path::Path, succeeds: bool) -> Option<Vec<u8>> {
        if succeeds {
            let bytes = std::fs::read(path).unwrap();
            assert_eq!(
                crate::odp::read_odp(&bytes).unwrap().title,
                "Authorized export snapshot"
            );
            Some(bytes)
        } else {
            assert!(!path.exists());
            None
        }
    }

    fn dispatched_save_after_restore(succeeds: bool) {
        let directory =
            std::env::temp_dir().join(format!("slides-save-restore-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = if succeeds {
            directory.join("export.odp")
        } else {
            directory.join("missing-parent/export.odp")
        };
        let mut deck = seed_deck();
        deck.title = "Selected restored deck".into();
        let mut doc = AutoCommit::new();
        SlidesDoc::seed(&mut doc, &deck);
        let selected = doc.get_heads();
        SlidesDoc::set_title(&mut doc, "Authorized export snapshot");
        let mut app = SlidesApp::new();
        app.sync(&mut doc, &automerge::ROOT);
        let invocation = tilo_actions::InvocationId::new();
        let payload = serde_json::to_vec(&crate::actions::SlidesOperation::SaveAs {
            path: Some(path.to_string_lossy().into_owned()),
        })
        .unwrap();
        assert!(matches!(
            app.start_resource_operation(
                invocation,
                tilo_actions::OperationKind::Effect,
                crate::actions::SLIDES_OPERATION_TYPE,
                &payload,
                &doc,
                &automerge::ROOT,
            )
            .unwrap(),
            Some(ui::widget::ResourceOperationStart::Accepted)
        ));
        // Dispatch the accepted operation without draining its completion in the same sync.
        let (save, export_path) = app.resource_file.take().unwrap();
        assert!(save);
        assert!(app.queue_document_save(export_path));
        // The actual worker performs the real export. Hold only its result at the
        // channel boundary so restoration deterministically precedes settlement.
        let completed = app
            .document_worker
            .as_ref()
            .unwrap()
            .rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let DocumentJobResult::Save { result, .. } = &completed else {
            panic!("expected a save completion");
        };
        assert_eq!(result.is_ok(), succeeds);
        let actual_error = result.as_ref().err().cloned();
        let exported_bytes = assert_export_snapshot(&path, succeeds);
        restore(&mut doc, &selected);
        let restored_heads = doc.get_heads();
        app.reconcile_canonical(&doc);
        assert!(
            app.poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                .is_none(),
            "canonical invalidation must not fabricate export settlement"
        );
        // Cancellation cannot undo a dispatched write or truthfully settle it
        // before the worker outcome. A later invocation cannot take its slot.
        assert!(
            app.poll_resource_operation(invocation, true, &doc, &automerge::ROOT)
                .is_none()
        );
        let other_invocation = tilo_actions::InvocationId::new();
        assert!(
            app.start_resource_operation(
                other_invocation,
                tilo_actions::OperationKind::Effect,
                crate::actions::SLIDES_OPERATION_TYPE,
                &payload,
                &doc,
                &automerge::ROOT,
            )
            .is_err()
        );
        let (results, rx) = std::sync::mpsc::sync_channel(1);
        results.send(completed).unwrap();
        app.document_worker.as_mut().unwrap().rx = rx;
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            app.poll_resource_operation(other_invocation, false, &doc, &automerge::ROOT)
                .is_none()
        );
        assert_save_settlement(
            &mut app,
            &doc,
            invocation,
            succeeds,
            actual_error.as_deref(),
        );
        assert_eq!(app.deck.title, "Selected restored deck");
        assert_eq!(doc.get_heads(), restored_heads);
        if let Some(bytes) = exported_bytes {
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "restore must not rerun the export"
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn successful_dispatched_save_settles_after_history_restore() {
        dispatched_save_after_restore(true);
    }

    #[test]
    fn failed_dispatched_save_reports_actual_error_after_history_restore() {
        dispatched_save_after_restore(false);
    }

    #[test]
    fn restore_rebinds_notes_by_slide_identity_and_cancels_stale_file_and_title_work() {
        let mut deck = seed_deck();
        deck.slides[0].notes = "Selected notes".into();
        let mut doc = AutoCommit::new();
        SlidesDoc::seed(&mut doc, &deck);
        let selected = doc.get_heads();
        SlidesDoc::splice_notes(&mut doc, 0, 0, 0, "LIVE ");
        doc.get_heads();
        let mut app = SlidesApp::new();
        app.sync(&mut doc, &automerge::ROOT);
        app.cur_slide = 0;
        app.begin_notes_edit(&mut doc);
        let old_obj = app.notes_input.bound_obj().unwrap().clone();
        app.active_document_generation = Some(77);
        let (jobs, _job_receiver) = std::sync::mpsc::sync_channel(1);
        let (results, result_receiver) = std::sync::mpsc::sync_channel(1);
        let mut obsolete = seed_deck();
        obsolete.title = "Obsolete worker completion".into();
        results
            .send(DocumentJobResult::Open {
                generation: 77,
                result: Ok(obsolete),
            })
            .unwrap();
        app.document_worker = Some(DocumentWorker {
            tx: jobs,
            rx: result_receiver,
        });
        app.resource_file = Some((false, "/never/open/stale.odp".into()));
        let (tx, rx) = std::sync::mpsc::channel();
        app.portal_rx = Some(rx);
        drop(tx);
        restore(&mut doc, &selected);
        let heads = doc.get_heads();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.notes_input.value(), "Selected notes");
        assert_ne!(app.notes_input.bound_obj(), Some(&old_obj));
        assert!(app.editing_notes);
        assert!(app.active_document_generation.is_none());
        assert!(app.resource_file.is_none());
        assert!(app.portal_rx.is_none());
        assert_eq!(doc.get_heads(), heads);
        // Ordinary subsequent native notes/title edits stay usable across sync.
        SlidesDoc::splice_notes(&mut doc, 0, 0, 0, "ONE ");
        app.reload(&doc);
        doc.get_heads();
        app.sync(&mut doc, &automerge::ROOT);
        SlidesDoc::splice_notes(&mut doc, 0, 0, 0, "TWO ");
        app.reload(&doc);
        doc.get_heads();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.notes_input.value(), "TWO ONE Selected notes");
        app.begin_title_edit();
        app.insert_text(&mut doc, " one");
        doc.get_heads();
        app.sync(&mut doc, &automerge::ROOT);
        app.insert_text(&mut doc, " two");
        doc.get_heads();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.editing_title);
        assert!(app.deck.title.ends_with(" one two"));
    }
    #[test]
    fn deleted_edit_target_and_stale_resource_descriptors_are_fenced_before_sync() {
        let mut doc = AutoCommit::new();
        let mut deck = seed_deck();
        deck.slides.truncate(1);
        SlidesDoc::seed(&mut doc, &deck);
        let selected = doc.get_heads();
        let extra = seed_deck().slides.remove(0);
        let id = extra.id.clone();
        deck.slides.push(extra);
        SlidesDoc::seed(&mut doc, &deck);
        doc.get_heads();
        let mut app = SlidesApp::new();
        app.sync(&mut doc, &automerge::ROOT);
        app.cur_slide = 1;
        app.begin_notes_edit(&mut doc);
        assert!(
            Widget::resources(&app, &doc)
                .iter()
                .any(|r| r.id.as_str().contains("slides"))
        );
        restore(&mut doc, &selected);
        let heads = doc.get_heads();
        assert!(
            Widget::resources(&app, &doc).is_empty(),
            "unreconciled resources cannot retain old action payloads"
        );
        app.sync(&mut doc, &automerge::ROOT);
        assert!(!app.editing_notes);
        assert!(app.deck.index_of(&id).is_none());
        assert_eq!(doc.get_heads(), heads);
        assert!(!Widget::resources(&app, &doc).is_empty());
    }
}
