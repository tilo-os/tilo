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
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, ReadDoc};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_rich_text::{FieldOutcome, RichTextField, RichTextSurface, SubmitKey};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Embedded;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    AudiencePresentationProvider, ColorPicker, FullscreenProvider, NumberInput, PageSidebar,
    PageSidebarEvent, Scrollable, SidebarDrawer, SplitLayout, TextInput, Widget,
};

use crate::actions;
use crate::assets::ImageStore;
use crate::doc::SlidesDoc;
use crate::help_overlay::HelpOverlay;
use crate::model::{
    Background, Deck, Element, ElementKind, FILL_ROLES, Id, Layout, Palette, PaletteKind, SLIDE_H,
    SLIDE_W, STROKE_ROLES, Slide, TextAlign, new_id, seed_deck, starter_deck,
};
use crate::overview_grid::OverviewGrid;
use crate::presentation::{Blank, Command as PresentCommand, Presentation, PresentationFrame};
use crate::render::{self, DeckAssets};
use crate::strings::{tr, tr_args};
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

/// Half-size of a selection handle's *grab* zone (hit-test radius).
const HANDLE: f64 = 9.0;
/// How far outside the selected element's content box the resize handles sit,
/// so a handle never covers the first glyph of left-anchored text (or any
/// content at a corner). The selection ring is drawn at this same outset, so
/// the ring passes through the handle centres and the whole selection
/// decoration reads as a single frame *around* the content rather than over it.
const HANDLE_OFFSET: f64 = 8.0;

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
    /// Open the menu of the insert tools that did not fit the header row.
    ToggleToolbarMenu,
    /// Open the menu of slide layouts.
    OpenLayoutMenu,
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
/// the element's rich-text body through `SlidesAction::EditElementText`.
struct TextBoxEditor {
    slide_id: String,
    element_id: String,
    host: Embedded<RichTextSurface<RichTextField<actions::SlidesAction>>>,
}

impl TextBoxEditor {
    fn open(doc: &AutoCommit, slide_id: &str, element: &Element) -> Self {
        let (locate_slide, locate_element) = (slide_id.to_owned(), element.id.clone());
        let (edit_slide, edit_element) = (slide_id.to_owned(), element.id.clone());
        let field = RichTextField::new(
            actions::SLIDES_ACTION_TYPE,
            move |doc| SlidesDoc::element_body(doc, &locate_slide, &locate_element),
            move |edit| actions::SlidesAction::EditElementText {
                slide_id: edit_slide.clone(),
                element_id: edit_element.clone(),
                edit,
            },
        );
        let mut surface = RichTextSurface::field(field)
            .with_editor(render::text_box_editor(element))
            .with_submit(SubmitKey::ModEnter);
        surface.present_from(doc);
        surface.set_focus(true);
        surface
            .editor_mut()
            .focus_block(usize::MAX, ui::widget::CursorPlacement::End);
        Self {
            slide_id: slide_id.to_owned(),
            element_id: element.id.clone(),
            host: Embedded::new(surface),
        }
    }
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
    /// Mounted on a record another participant created: its deck is theirs
    /// to seed, so an uninitialized document is left untouched.
    joined: bool,
    bootstrapped: bool,

    mode: Mode,
    canonical_deck: Option<(ObjId, Deck)>,
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
    needs_commit: bool,

    /// Framework-owned action dispatch (undo boundary + executor + last-action
    /// record). Every structural / geometric / stylistic deck mutation goes
    /// through it as a [`SlidesAction`](crate::actions::SlidesAction); flowing
    /// text edits stay on the native CRDT splice path (see [`crate::actions`]).
    actions: ui::ActionHost,
    /// Hosted by [`crate::resource::SlidesShell`]: edits go through the
    /// framework dispatcher. Standalone / shots keep [`Self::actions`].
    framework: Option<ui::framework::Dispatcher<crate::actions::SlidesDeck>>,
    /// Test / attach override so [`PublishesResources`] can keep only the live
    /// selection beside the published deck without constructing a dispatcher.
    framework_hosted: bool,

    /// The slide sidebar: docked beside the canvas on a wide window, a
    /// floating drawer over it on a narrow one.
    rail_drawer: SidebarDrawer,
    /// The properties inspector at the trailing edge, docked or floating
    /// like the sidebar.
    inspector_drawer: SidebarDrawer,
    /// Cleared after the first layout is resolved; until then each drawer's
    /// reveal is snapped to its resolved default so the first frame at a
    /// narrow width shows the closed drawers without a one-frame slide.
    flap_uninitialised: bool,
    /// Last frame, for the rail's frame-rate-independent drag auto-scroll.
    last_tick: ui::time::Instant,

    /// Monotonic revision of the loaded deck, bumped whenever the CRDT mirror
    /// is reloaded. The presentation cache includes it in its key, so a remote
    /// edit (or undo) to the current slide invalidates the cached render.
    deck_rev: u64,

    /// Complete, incremental Ctrl+K index. Cloning its provider handle is
    /// cheap; rebuilding a large deck's postings each frame is not.
    search_provider: crate::search_provider::SlideSearchProvider,

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
    /// The open context menu is the insert tools that did not fit the
    /// header row (the More button shows as on while it is).
    toolbar_menu_open: bool,
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
    /// The speaker notes shown, read-only, while they are not being edited:
    /// the same field face the editor has.
    notes_preview: TextInput,
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
    /// `--shots`-only: open the slide sidebar or the inspector once the
    /// first layout has resolved the window's size class.
    demo_open_rail: bool,
    demo_open_inspector: bool,
}

/// One row of a context menu: what it says and the action it performs.
struct MenuRow {
    label: String,
    hit: Hit,
    checked: Option<bool>,
    destructive: bool,
}

impl MenuRow {
    fn new(label: String, hit: Hit) -> Self {
        Self {
            label,
            hit,
            checked: None,
            destructive: false,
        }
    }

    fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }
}

/// Which context menu a `--shots` scenario should pop open for its capture.
#[derive(Clone, Copy)]
enum DemoMenu {
    Element,
    Slide,
    /// The insert tools behind the header row's More button.
    Tools,
}

/// The content store an app constructed without one uses: the app's own
/// store, or an in-memory one under unit test so tests never write to the
/// user's data directory.
fn default_content() -> ContentStore {
    #[cfg(test)]
    {
        ContentStore::memory()
    }
    #[cfg(not(test))]
    {
        ContentStore::for_app()
    }
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
        let content = default_content();
        let search_provider = crate::search_provider::SlideSearchProvider::new(&deck);
        let (rail_drawer, inspector_drawer) = Self::new_drawers();
        let (document_worker, document_worker_error) = match DocumentWorker::new(content.clone()) {
            Ok(worker) => (Some(worker), None),
            Err(error) => (None, Some(error)),
        };
        SlidesApp {
            deck,
            rect: Rect::ZERO,
            node: None,
            seeded: false,
            joined: ui::launch::record_launch() == Some(ui::launch::RecordLaunch::Joined),
            bootstrapped: false,
            mode: Mode::Edit,
            canonical_deck: None,
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
            needs_commit: false,
            actions: ui::ActionHost::new(),
            framework: None,
            framework_hosted: false,
            rail_drawer,
            inspector_drawer,
            flap_uninitialised: true,
            last_tick: ui::time::now(),
            deck_rev: 0,
            search_provider,
            images: ImageStore::new(content.clone()),
            content,
            rail_scroll: 0.0,
            overview: OverviewGrid::new(),
            toolbar_menu_open: false,
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
                .compact()
                .with_id("slides-font-size"),
            // Seeded from the selected element's fill when it opens.
            fill_picker: ColorPicker::new(vello::peniko::Color::TRANSPARENT),
            fill_picker_open: false,
            fill_picker_needs_init: false,
            title_input: TextInput::local()
                .with_kind(ui::widget::TextInputKind::Name)
                .with_text_role(ui::TextRole::Heading)
                .with_id("slides-title"),
            notes_input: TextInput::local()
                .multiline(true)
                .with_text_role(ui::TextRole::Body)
                .with_placeholder(&tr("slides-notes-placeholder"))
                .with_id("slides-speaker-notes"),
            notes_preview: TextInput::local()
                .multiline(true)
                .with_text_role(ui::TextRole::Body)
                .with_placeholder(&tr("slides-notes-placeholder"))
                .with_id("slides-speaker-notes-preview"),
            demo_menu: None,
            demo_reorder: false,
            demo_delete_zone: false,
            demo_open_rail: false,
            demo_open_inspector: false,
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
                self.demo_open_rail = true;
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
            "narrow" => self.demo_open_rail = true,
            // The inspector for a selected text box: docked on a wide window,
            // the drawer summoned open on a narrow one.
            "inspector" => {
                self.demo_open_inspector = true;
                self.cur_slide = 1;
                self.sel = self.deck.slides.get(1).and_then(|s| {
                    s.elements
                        .iter()
                        .find(|e| e.kind == ElementKind::Text)
                        .map(|e| e.id.clone())
                });
                self.bootstrapped = true;
            }
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
                self.demo_menu = Some(DemoMenu::Tools);
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
        self.demo_open_inspector = true;
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
            let Some(src) = crate::assets::import_asset_file(&self.content, &path) else {
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
        self.begin_text_edit(true);
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

    /// The sidebar layout resolved from the current allocated width.
    /// Overview and present modes are full-window and never show it, but the
    /// editor (`Mode::Edit`) consults this every frame.
    fn rail_layout(&self) -> SplitLayout {
        self.rail_drawer.layout_for(self.rect.width())
    }

    /// Whether the sidebar is currently revealed (the person's choice for
    /// this size class, else its default: docked ⇒ open, drawer ⇒ closed).
    fn rail_open(&self) -> bool {
        self.rail_drawer.is_open()
    }

    /// The inspector layout resolved from the current allocated width.
    fn inspector_layout(&self) -> SplitLayout {
        self.inspector_drawer.layout_for(self.rect.width())
    }

    /// Whether the inspector is currently revealed.
    fn inspector_open(&self) -> bool {
        self.inspector_drawer.is_open()
    }

    /// Resolve both drawers against the current width and advance their
    /// reveals. On the very first resolved layout each is snapped to its
    /// default, so a narrow first frame shows the closed drawers with no
    /// one-frame slide. Called once per frame before `compute_chrome`.
    fn sync_flaps(&mut self, theme: Option<&Theme>) -> SplitLayout {
        self.resolve_drawers();
        if self.flap_uninitialised {
            // A `--shots` scenario's drawer is opened for the size class the
            // window actually resolved to.
            if std::mem::take(&mut self.demo_open_rail) {
                self.rail_drawer.open();
            }
            if std::mem::take(&mut self.demo_open_inspector) {
                self.inspector_drawer.open();
            }
            let (rail, inspector) = (self.rail_open(), self.inspector_open());
            self.rail_drawer.snap(rail);
            self.inspector_drawer.snap(inspector);
            self.flap_uninitialised = false;
        } else if let Some(theme) = theme {
            self.rail_drawer.tick(theme);
            self.inspector_drawer.tick(theme);
        }
        self.resolve_drawers();
        self.rail_drawer.layout()
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
    /// Take part in the framework: submit deck edits through its dispatcher.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::actions::SlidesDeck>,
    ) {
        self.framework = Some(dispatcher);
        self.framework_hosted = true;
    }

    /// Mark this surface as framework-hosted so
    /// [`PublishesResources`](ui::resources::PublishesResources) keeps only
    /// selection live extras beside the published deck door.
    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn set_framework_hosted(&mut self, hosted: bool) {
        self.framework_hosted = hosted;
    }

    /// Whether [`PublishesResources`](ui::resources::PublishesResources) is
    /// publishing only selection live extras beside the framework door.
    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.framework_hosted || self.framework.is_some()
    }

    /// Test-only: plant a local deck that has not been published through the
    /// framework resource yet.
    #[cfg(test)]
    pub(crate) fn seed_unpublished_deck_for_test(&mut self, deck: crate::model::Deck) {
        self.deck = deck;
        self.cur_slide = self.cur_slide.min(self.deck.slides.len().saturating_sub(1));
    }

    /// Test-only: slide ids currently held on the local deck surface.
    #[cfg(test)]
    pub(crate) fn local_slide_ids_for_test(&self) -> Vec<String> {
        self.deck
            .slides
            .iter()
            .map(|slide| slide.id.clone())
            .collect()
    }

    /// Test-only: select an element on the current slide so hosted extras bind.
    #[cfg(test)]
    pub(crate) fn select_element_for_test(&mut self, element_id: crate::model::Id) {
        self.sel = Some(element_id);
    }

    /// Show the deck the framework resource holds.
    /// An empty projection does not wipe a deck that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::actions::SlidesResourceCtx) {
        if state.slides.is_empty() && !self.deck.slides.is_empty() {
            return;
        }
        if state.slides.is_empty() {
            return;
        }
        let slides: Vec<_> = state
            .slides
            .iter()
            .map(|slide| slide.data.to_slide())
            .collect();
        if self.deck.title == state.deck_title && self.deck.slides == slides {
            return;
        }
        self.deck.title.clone_from(&state.deck_title);
        self.deck.slides = slides;
        self.search_provider.refresh(&self.deck);
        self.bump_deck_revision();
    }

    fn perform_action(&mut self, doc: &mut AutoCommit, action: &crate::actions::SlidesAction) {
        let context = tilo_actions::ActionContext::for_action("so.tilo.slides", "Slides", action);
        if let Some(dispatcher) = &self.framework {
            let _ = dispatcher.apply_now_with(doc, action, context);
        } else {
            self.actions.dispatch_with_context(
                doc,
                crate::actions::SLIDES_ACTION_TYPE,
                action,
                context,
            );
        }
        self.canonical_deck = doc
            .get(automerge::ROOT, "deck")
            .ok()
            .flatten()
            .and_then(|(_, id)| SlidesDoc::load(doc).map(|deck| (id, deck)));
    }

    /// The context Ctrl+K needs to build the deck's resource tree: the deck
    /// title, each slide (with its elements), and this peer's live selection.
    /// Deck rows come from the in-memory mirror; selection is local.
    pub(crate) fn slides_resource_ctx(&self) -> crate::actions::SlidesResourceCtx {
        let mut ctx = crate::actions::SlidesResourceCtx::from_deck(&self.deck);
        if let Some(slide) = ctx.slides.get(self.cur_slide) {
            ctx.selection = Some(crate::actions::selection_ctx_for(
                slide,
                self.sel.as_deref(),
            ));
        }
        ctx
    }

    // --- rail scroll geometry ----------------------------------------------

    /// The inspector's rect this frame (docked column or floating drawer).
    fn inspector_rect(&self) -> Rect {
        self.inspector_drawer.sidebar_rect()
    }

    /// The slide sidebar's rect this frame (docked column or floating drawer).
    fn rail_rect(&self) -> Rect {
        self.rail_drawer.sidebar_rect()
    }

    /// The thumbnail list's viewport — between the sidebar's header and its
    /// footer actions. The shared [`PageSidebar`] owns the thumbnail
    /// geometry and scroll bar inside it.
    fn rail_viewport(&self, rail: Rect) -> Rect {
        self.rail_viewport_of(rail)
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
        match hit {
            Hit::ToggleToolbarMenu => self.open_tools_menu(),
            Hit::OpenLayoutMenu => self.open_layout_menu(),
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

    /// Compact editing exposes one drawer at a time. Opening the slide
    /// sidebar dismisses a floating inspector, keeping the slide usable
    /// behind the drawer instead of sandwiching it between two panels.
    fn toggle_rail_flap(&mut self) {
        if self.rail_drawer.is_overlay() && self.inspector_drawer.is_overlay() && !self.rail_open()
        {
            self.inspector_drawer.snap(false);
        }
        self.rail_drawer.toggle();
    }

    /// Mirror [`Self::toggle_rail_flap`] for the inspector. Docked panels
    /// stay independent on roomy windows.
    fn toggle_inspector_flap(&mut self) {
        if self.inspector_drawer.is_overlay()
            && self.rail_drawer.is_overlay()
            && !self.inspector_open()
        {
            self.rail_drawer.snap(false);
        }
        self.inspector_drawer.toggle();
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
        let el = actions::inserted_element_template(kind);
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

    fn apply_loaded_deck(&mut self, doc: &mut AutoCommit, deck: Deck) {
        // Direct document write, not a deck action. Opening a file replaces
        // the title, palette, and every slide, including ids the current deck
        // already holds. `InsertSlide` rejects those ids, `DeleteSlide` refuses
        // the last slide, and imported geometry can fall outside action
        // validators. `SlideData` already carries blocks as `ClipBlock`.
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
            crate::pptx::read_pptx(&bytes, &self.content)
        } else {
            crate::odp::read_odp(&bytes, &self.content)
        };
        let Ok(deck) = deck else {
            return false;
        };
        self.apply_loaded_deck(doc, deck);
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
            return match crate::pptx::write_pptx(&self.deck, &self.content) {
                Ok(bytes) => std::fs::write(path, bytes).is_ok(),
                Err(_) => false,
            };
        }
        let path = ensure_odp_ext(path);
        match crate::odp::write_odp(&self.deck, &self.content) {
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
    fn commit_title_edit(&mut self, doc: &mut AutoCommit) {
        if let Some(t) = self.title_input.take_dirty() {
            self.perform_action(doc, &crate::actions::set_deck_title_action(t));
        }
        self.editing_title = false;
        Widget::set_focus(&mut self.title_input, false);
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

    /// The rect the inline title editor occupies in the header row: a
    /// field whose text lands exactly where the static title was.
    fn title_input_rect(&self, chrome: &Chrome) -> Rect {
        let m = &self.chrome_metrics;
        let pad = m.field_pad;
        let x1 = chrome
            .buttons
            .iter()
            .filter(|b| b.rect.y0 < chrome.toolbar.y1 && b.rect.x0 > chrome.title_pos.x)
            .map(|b| b.rect.x0 - m.s2)
            .fold(chrome.toolbar.x1 - m.s3, f64::min);
        Rect::new(
            chrome.title_pos.x - pad,
            chrome.title_rect.y0,
            x1.max(chrome.title_pos.x + m.target),
            chrome.title_rect.y1,
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
        // Live-splice each edit into the CRDT title (the mirror is refreshed on
        // commit; while editing, the field is the source of truth).
        if let Some(t) = self.title_input.take_dirty() {
            self.perform_action(
                routed.state,
                &crate::actions::SlidesAction::SetDeckTitle { title: t },
            );
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
        if self.notes_input.take_dirty().is_some() {
            self.mark();
            self.reload(routed.state);
        }
        input_handled
    }

    // --- drawing ------------------------------------------------------------

    /// The canvas slide itself: the rendered slide seated on the content
    /// ground, the selection frame and handles, and the snap guides. No-op
    /// while the deck has no current slide.
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
        let radius = f64::from(theme.tokens.radii.control);
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
            radius,
            editing,
        );
        render::seat_slide(scene, t, chrome.slide_dest, radius, theme);
        self.draw_editor_selection_chrome(scene, t, theme, chrome);
        self.draw_editor_guides(scene, t, theme, chrome);
    }

    /// How far the selection frame stands outside the selected element: a
    /// small step, never so far that it runs over a neighbouring element
    /// (a title's accent rule just under it).
    fn selection_outset(&self, chrome: &Chrome, el: &Element) -> f64 {
        let m = &self.chrome_metrics;
        let er = render::element_dest(chrome.slide_dest, chrome.scale, el);
        let grip = m.target * 0.25;
        let mut outset = m.s2;
        if let Some(slide) = self.slide() {
            for other in slide.elements.iter().filter(|other| other.id != el.id) {
                let or = render::element_dest(chrome.slide_dest, chrome.scale, other);
                if er.intersect(or).area() > 0.0 {
                    continue;
                }
                let dx = (or.x0 - er.x1).max(er.x0 - or.x1);
                let dy = (or.y0 - er.y1).max(er.y0 - or.y1);
                let gap = dx.max(dy);
                if gap > 0.0 && dx.min(dy) < 0.0 {
                    // A neighbour beside the element along one axis: keep
                    // the frame and its handles clear of it.
                    outset = outset.min(gap - grip / 2.0 - 1.0);
                }
            }
        }
        outset.max(1.0)
    }

    /// The selection frame and resize handles around the selected element.
    /// While its text is being edited the editor draws the caret and
    /// selection instead of handles. No-op with nothing selected.
    fn draw_editor_selection_chrome(
        &self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        chrome: &Chrome,
    ) {
        let Some(el) = self.selected_element() else {
            return;
        };
        let er = render::element_dest(chrome.slide_dest, chrome.scale, el);
        let you = theme.tokens.colors.intent.you;
        let outset = self.selection_outset(chrome, el);
        let line = f64::from(theme.tokens.metrics.chrome_rule_px) * 1.5;
        scene.stroke(
            &Stroke::new(line),
            t,
            you,
            None,
            &er.inflate(outset, outset),
        );
        if !self.editing_text {
            let grip = self.chrome_metrics.target * 0.25;
            for (_, hr) in self.handles_of(chrome.slide_dest, chrome.scale, el) {
                let pip = Rect::from_center_size(hr.center(), (grip, grip));
                scene.fill(
                    Fill::NonZero,
                    t,
                    theme.tokens.colors.surfaces.raised,
                    None,
                    &pip,
                );
                scene.stroke(&Stroke::new(line), t, you, None, &pip);
            }
        }
    }

    /// Live alignment guides (only while a drag is snapping).
    fn draw_editor_guides(&self, scene: &mut Scene, t: Affine, theme: &Theme, chrome: &Chrome) {
        let you = theme.tokens.colors.intent.you;
        let dest = chrome.slide_dest;
        let line = Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px));
        for g in &self.active_guides {
            let segment = match g {
                Guide::Vertical(sx) => {
                    let x = dest.x0 + sx * chrome.scale;
                    Line::new(Point::new(x, dest.y0), Point::new(x, dest.y1))
                }
                Guide::Horizontal(sy) => {
                    let y = dest.y0 + sy * chrome.scale;
                    Line::new(Point::new(dest.x0, y), Point::new(dest.x1, y))
                }
            };
            scene.stroke(&line, t, you, None, &segment);
        }
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
        let radius = f64::from(theme.tokens.radii.control);
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
                    radius,
                );
            }
        });
        self.page_sidebar = sb;
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

    /// Draw the inspector's embedded widgets: the font-size field, in the
    /// inspector's ground, and, when open, the fill colour popover floating
    /// over the window.
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
            // Clipped like the rest of the inspector body: the field scrolls
            // with its section rather than floating over the header.
            let local = self.inspector_theme(theme);
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                t,
                &chrome.inspector_body,
            );
            self.font_input.draw(scene, t, tr, &local, presence);
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
            let panel = self.fill_picker_panel_rect(rect);
            ui::widget::floating_surface::paint(scene, t, panel, theme);
            let local = theme.on_ground(ui::chrome::panel_ground(theme));
            let m = &self.chrome_metrics;
            PaintCtx::new(scene, t, tr, &local).text_role(
                &crate::strings::tr("slides-fill-colour"),
                ui::TextRole::Subheading,
                local.tokens.colors.text_primary,
                Point::new(rect.x0, panel.y0 + m.s4),
                None,
            );
            self.fill_picker.draw(scene, t, tr, &local, presence);
        }
    }

    /// Draw the inline title [`TextInput`] over the header row while renaming.
    fn draw_title_input(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        chrome: &Chrome,
    ) {
        let _ = theme;
        let rect = self.title_input_rect(chrome);
        self.title_input.apply_layout_external(rect);
        self.title_input.draw(scene, t, tr, theme, presence);
    }

    /// The speaker notes: the editing field while they are being edited,
    /// otherwise the same field showing them read-only.
    pub(super) fn draw_notes(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        nb: Rect,
    ) {
        if self.editing_notes {
            self.notes_input.apply_layout_external(nb);
            self.notes_input
                .draw(scene, t, tr, theme, &PresenceCtx::empty());
            return;
        }
        let notes = self
            .slide()
            .map(|slide| slide.notes.clone())
            .unwrap_or_default();
        if self.notes_preview.value() != notes {
            self.notes_preview.set_text(&notes);
        }
        self.notes_preview.apply_layout_external(nb);
        self.notes_preview
            .draw(scene, t, tr, theme, &PresenceCtx::empty());
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

    /// The overview's header row: the deck title, the overview toggle (on)
    /// that returns to the editor, and Present.
    fn overview_header(&self) -> (Vec<editor_chrome::UiButton>, Point, f64) {
        let m = &self.chrome_metrics;
        let header = self.overview.header(self.rect);
        let control_y = header.y0 + (header.height() - m.control_h) / 2.0;
        let present_label = tr("slides-present");
        let present_w = m.width(
            &present_label,
            editor_chrome::Measure::Button { icon: true },
        );
        let present = Rect::new(
            header.x1 - m.s6 - present_w,
            control_y,
            header.x1 - m.s6,
            control_y + m.control_h,
        );
        let toggle = self.chip(header, present.x0 - m.s2 - m.target);
        let buttons = vec![
            editor_chrome::UiButton {
                rect: toggle,
                kind: ButtonKind::Icon {
                    icon: Icon::LayoutGrid,
                    name: tr("slides-a11y-slide-overview"),
                    active: true,
                },
                hit: Hit::Overview,
            },
            editor_chrome::UiButton {
                rect: present,
                kind: ButtonKind::Command {
                    label: present_label,
                    icon: Some(Icon::Play),
                    role: ui::widget::ButtonRole::Primary,
                },
                hit: Hit::Present,
            },
        ];
        let title = Point::new(
            header.x0 + m.s6,
            header.y0 + (header.height() - m.lh_heading) / 2.0,
        );
        let title_max = (toggle.x0 - m.s4 - title.x).max(0.0);
        (buttons, title, title_max)
    }

    fn draw_overview(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let palette = self.palette(theme);
        // A slide's type is sized in slide units, whatever the reader's text
        // scale; the cards' own text is set in its roles.
        self.overview.draw(
            &mut PaintCtx::new(scene, t, tr, theme),
            self.rect,
            &self.deck,
            self.cur_slide,
            Some(self.last_mouse),
            DeckAssets {
                palette: &palette,
                images: &self.images,
            },
        );
        let (buttons, title, title_max) = self.overview_header();
        let style = theme.text(ui::TextRole::Heading);
        let shown = tr.elide_to_width_style(&self.deck.title, title_max as f32, &style);
        tr.draw_style(
            scene,
            &shown,
            theme.tokens.colors.text_primary,
            t * Affine::translate((title.x, title.y)),
            None,
            &style,
        );
        for button in &buttons {
            self.draw_button(scene, t, tr, theme, &palette, button);
        }
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
    let path = tilo_paths::temp_dir()?.join("slides-demo-image.png");
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
            Hit::OpenLayoutMenu => "slides.control.layout-menu".into(),
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
                AccessState::new().expanded(self.ctx_menu.is_open() && self.toolbar_menu_open),
            ),
            Hit::OpenLayoutMenu => (
                Role::Button,
                tr_args(
                    "slides-a11y-layout-menu",
                    &[(
                        "name",
                        ArgValue::from(self.slide().map_or("", |slide| slide.layout.label())),
                    )],
                ),
                AccessState::new(),
            ),
        }
    }

    /// The tooltip of the control under `point`: an icon-only control's
    /// name, or a swatch's colour.
    fn tooltip_at(&self, point: Point) -> Option<String> {
        if self.mode != Mode::Edit {
            return None;
        }
        let chrome = self.compute_chrome();
        chrome
            .buttons
            .iter()
            .find(|button| button.rect.contains(point))
            .and_then(|button| match &button.kind {
                ButtonKind::Icon { name, .. } => Some(name.clone()),
                ButtonKind::Swatch { role, .. } => Some(editor_chrome::role_name(role)),
                ButtonKind::Theme { kind, .. } => Some(kind.label().to_owned()),
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
        dyn ui::undo::CommittedActions,
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource,
        dyn ui::undo::UndoPoints
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
        if self.joined && SlidesDoc::is_uninitialized(state) {
            return;
        }
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
                // Direct document write, not a deck action. Every `SlidesAction`
                // loads an existing deck and refuses while that key is missing,
                // which is the only time this runs. `Resource::initialize`
                // already seeded its own `starter_deck()` ids; writing this
                // widget's `starter_deck()` over that deck would replace them.
                // Shots that set `self.deck` before sync depend on this write.
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
        self.upgrade_text(state);
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
        self.resolve_drawers();
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
        // Fold in any images that finished decoding on their worker thread. A
        // freshly-decoded image must re-shape the cached present-mode slide.
        if self.images.pump() {
            self.presentation.invalidate_cache();
        }
        // Expire a settled cross-fade so `needs_animation_frame` can rest.
        self.presentation
            .set_fade(theme.tokens.motion.durations.base());
        self.presentation.expire_transition(theme.reduce_motion);
        // Measure first, so everything this frame lays out — the drawers,
        // `compute_chrome`, and the hit-rects later events derive from it —
        // comes from the theme and renderer that are about to paint it.
        self.refresh_chrome_metrics(text_renderer, theme);
        self.sync_flaps(Some(theme));
        self.page_sidebar.begin_frame_with_theme(
            theme,
            self.rail_viewport(self.rail_rect()),
            self.deck.slides.len(),
            self.cur_slide,
        );
        let now = ui::time::now();
        let dt = (now - self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        self.sync_scroll_offsets(dt);
        self.stage_shots_demo();
        match self.mode {
            Mode::Edit => {
                let chrome = self.compute_chrome();
                // Hand the inspector's scroll model this frame's viewport and
                // content extent; it clamps the stored offset and paints its
                // bar from the same numbers `compute_chrome` shifted by.
                self.inspector_scroll
                    .set_viewport_rect(chrome.inspector_body);
                self.inspector_scroll.set_content_size(
                    chrome.inspector_body.width() as f32,
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
    }

    /// The window's top layer: the open text fields' own menus, the open
    /// context menu, and the keyboard reference sheet, which shows over
    /// every mode (including Present, where `?` must also dismiss it).
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.editing_title {
            self.title_input
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        if self.editing_notes {
            self.notes_input
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        self.ctx_menu
            .draw(scene, transform, text_renderer, theme, presence);
        self.help_overlay
            .draw(scene, transform, text_renderer, theme, self.rect);
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
            || self.rail_drawer.is_animating()
            || self.inspector_drawer.is_animating()
            || self.images.any_loading()
            // A live thumbnail reorder pumps frames so the ghost tracks the
            // cursor and the rail auto-scrolls at its edges.
            || self.page_sidebar.is_dragging()
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        if reduced {
            self.presentation.expire_transition(true);
            let (rail, inspector) = (self.rail_open(), self.inspector_open());
            self.rail_drawer.snap(rail);
            self.inspector_drawer.snap(inspector);
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
        self.reconcile_canonical(ctx.state);
        // Restore replaces Text object identities; the first event may precede sync.
        if self.editing_notes {
            self.sync_notes_input(ctx.state, false);
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
            self.on_right_press(routed.state, self.last_mouse, ctx.handled);
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
            routed.state,
            ctx.handled,
        );
    }

    fn name(&self) -> &'static str {
        "SlidesApp"
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
            let shortcuts = HelpOverlay::spoken_rows()
                .into_iter()
                .map(|shortcut| AccessNode::new(Role::StaticText).with_name(shortcut))
                .collect();
            return Some(
                AccessNode::new(Role::Dialog)
                    .with_name(tr("slides-shortcuts-title"))
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
            .chain(chrome.overflow.iter())
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
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
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
            .chain(chrome.overflow.iter())
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
        if self.editing_text
            && let Some(editor) = &self.text_editor
        {
            return editor.host.ime_cursor_rect();
        }
        self.editing_notes
            .then(|| self.notes_input.ime_cursor_rect())
            .flatten()
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.editing_notes
            .then(|| self.notes_input.ime_purpose())
            .flatten()
    }

    /// Keep receiving pointer events for the whole of a canvas move/resize or a
    /// thumbnail reorder, even if the cursor briefly leaves the source rect.
    fn wants_pointer_capture(&self) -> bool {
        self.drag.active() || self.page_sidebar.is_pending_drag()
    }
}

impl ui::undo::UndoPoints for SlidesApp {
    fn needs_commit(&self) -> bool {
        self.needs_commit
    }
    fn on_commit(&mut self) {
        self.needs_commit = false;
    }
}

impl ui::search::capability::SearchSource for SlidesApp {
    /// Fold the deck's slides, elements, text and notes into the unified Ctrl+K
    /// palette. Every row addresses an exported resource `View` operation, so
    /// search uses the same brokered navigation route as agents and a11y.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
    }
}

impl ui::resources::PublishesResources for SlidesApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns the
    /// deck door — keep only the live selection extras.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.framework_hosted || self.framework.is_some() {
            return crate::actions::live_presentation_resources(&self.slides_resource_ctx());
        }
        if !self.canonical_matches(doc) {
            return Vec::new();
        }
        Self::publish_resources(self, doc)
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if !self.canonical_matches(doc) {
            return None;
        }
        if (self.framework_hosted || self.framework.is_some()) && id == "slides.deck" {
            return None;
        }
        Self::publish_resource(self, doc, id)
    }

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

impl ui::history_mode::capability::DocumentHistory for SlidesApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

impl ui::undo::CommittedActions for SlidesApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its slides action, so it records one undo boundary for it (see
    /// `Self::perform_action`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        let typed = self.text_editor.as_mut().and_then(|editor| {
            ui::undo::CommittedActions::take_committed_action(editor.host.widget_mut())
        });
        typed.or_else(|| self.actions.take_committed())
    }
    /// Typing a word into a text box is one undo step, open until the word ends.
    fn keeps_undo_step_open(&self) -> bool {
        self.text_editor.as_ref().is_some_and(|editor| {
            ui::undo::CommittedActions::keeps_undo_step_open(editor.host.widget())
        })
    }
}

impl SlidesApp {
    fn answer_resource_operation(
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
                DemoMenu::Tools => self.open_tools_menu(),
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
            let scroll =
                self.overview
                    .scroll_from_thumb_drag(area, orig_scroll, max_scroll, p.y - start_y);
            self.overview.set_scroll(scroll);
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
            let (buttons, _, _) = self.overview_header();
            if let Some(button) = buttons.iter().find(|b| b.rect.contains(p)) {
                let hit = button.hit.clone();
                self.apply_hit(doc, hit);
                *handled = true;
                return;
            }
            let (area, _, max_scroll) = self.compute_overview();
            // Scrollbar thumb → start a scroll drag.
            if let Some((_, thumb)) = self.overview.scrollbar_rects(
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

        if self.on_press_editor_menus(doc, p, handled) {
            return;
        }

        // A click anywhere but the title itself commits an in-flight rename.
        if self.editing_title && !chrome.title_rect.contains(p) {
            self.editing_title = false;
            self.mark();
        }

        // An open floating drawer is modal over the window: a press outside
        // it, on the scrim, closes it.
        if chrome.scrim.width() > 0.0 {
            let rail_floating = chrome.layout == SplitLayout::Overlay && self.rail_open();
            let inspector_floating =
                chrome.inspector_layout == SplitLayout::Overlay && self.inspector_open();
            let in_drawer = (rail_floating && Self::in_rail(&chrome, p))
                || (inspector_floating && Self::in_inspector(&chrome, p));
            if !in_drawer && (rail_floating || inspector_floating) {
                if rail_floating {
                    self.rail_drawer.close();
                }
                if inspector_floating {
                    self.inspector_drawer.close();
                }
                *handled = true;
                return;
            }
        }
        // Buttons. Over an open floating drawer only the drawer's own count:
        // it stands over the header row.
        let over = if Self::in_rail(&chrome, p) && chrome.layout == SplitLayout::Overlay {
            Some(editor_chrome::Zone::Rail)
        } else if Self::in_inspector(&chrome, p) && chrome.inspector_layout == SplitLayout::Overlay
        {
            Some(editor_chrome::Zone::Inspector)
        } else {
            None
        };
        for (i, b) in chrome.buttons.iter().enumerate().rev() {
            if matches!(b.kind, ButtonKind::Notes)
                || over.is_some_and(|zone| chrome.button_zone(i) != zone)
            {
                continue;
            }
            if b.rect.contains(p) {
                let hit = b.hit.clone();
                self.apply_hit(doc, hit);
                *handled = true;
                return;
            }
        }
        // The notes field opens the notes for editing.
        if chrome.notes_box.is_some_and(|notes| notes.contains(p)) {
            self.apply_hit(doc, Hit::EditNotes);
            *handled = true;
            return;
        }
        // Deck title → inline rename.
        if chrome.title_rect.contains(p) && !Self::in_panel(&chrome, p) {
            self.begin_title_edit();
            *handled = true;
            return;
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

    /// The shared rail's own context menu is modal while open. Returns
    /// `true` (and sets `*handled`) when the press was consumed by it, so
    /// [`Self::on_press_editor`] should stop.
    fn on_press_editor_menus(
        &mut self,
        doc: &mut AutoCommit,
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

    /// Build the shared context menu from `rows`, mapping each row to the
    /// action it performs. `None` rows are separators.
    fn build_menu(&mut self, rows: &[Option<MenuRow>]) -> ContextMenu {
        let mut menu = ContextMenu::new();
        let mut hits: Vec<Option<Hit>> = Vec::new();
        for row in rows {
            if let Some(row) = row {
                let item = if row.destructive {
                    MenuItem::destructive_action(row.label.clone(), || {})
                } else {
                    MenuItem::action(row.label.clone(), || {})
                };
                menu.push(match row.checked {
                    Some(checked) => item.with_checked(checked),
                    None => item,
                });
                hits.push(Some(row.hit.clone()));
            } else {
                menu.push(MenuItem::separator());
                hits.push(None);
            }
        }
        self.ctx_hits = hits;
        self.toolbar_menu_open = false;
        menu
    }

    /// Open a context menu at the pointer `p`, kept inside the window.
    fn open_menu_at(&mut self, p: Point, rows: &[Option<MenuRow>]) {
        let mut menu = self.build_menu(rows);
        menu.open_at_clamped([p.x as f32, p.y as f32], self.rect);
        self.ctx_menu = menu;
    }

    /// Open a menu under the control at `anchor`.
    fn open_menu_under(&mut self, anchor: Rect, rows: &[Option<MenuRow>]) {
        let mut menu = self.build_menu(rows);
        menu.open_anchored(anchor, self.rect);
        self.ctx_menu = menu;
    }

    fn open_element_menu(&mut self, p: Point) {
        self.open_menu_at(
            p,
            &[
                Some(MenuRow::new(tr("slides-a11y-bring-front"), Hit::BringFront)),
                Some(MenuRow::new(tr("slides-a11y-send-back"), Hit::SendBack)),
                None,
                Some(MenuRow::new(tr("slides-duplicate"), Hit::DuplicateElement)),
                Some(MenuRow::new(tr("slides-delete"), Hit::DeleteElement).destructive()),
            ],
        );
    }

    /// The insert tools that did not fit the header row, under its More
    /// button.
    fn open_tools_menu(&mut self) {
        let chrome = self.compute_chrome();
        let Some(more) = chrome
            .buttons
            .iter()
            .find(|b| matches!(b.hit, Hit::ToggleToolbarMenu))
            .map(|b| b.rect)
        else {
            return;
        };
        let rows: Vec<Option<MenuRow>> = chrome
            .overflow
            .iter()
            .filter_map(|b| match &b.kind {
                ButtonKind::Command { label, .. } => {
                    Some(Some(MenuRow::new(label.clone(), b.hit.clone())))
                }
                _ => None,
            })
            .collect();
        self.open_menu_under(more, &rows);
        self.toolbar_menu_open = true;
    }

    /// The slide layouts, the current one checked, under the inspector's
    /// layout button.
    fn open_layout_menu(&mut self) {
        let chrome = self.compute_chrome();
        let Some(anchor) = chrome
            .buttons
            .iter()
            .find(|b| matches!(b.hit, Hit::OpenLayoutMenu))
            .map(|b| b.rect)
        else {
            return;
        };
        let current = self.slide().map(|s| s.layout);
        let rows: Vec<Option<MenuRow>> = Layout::ALL
            .iter()
            .map(|l| {
                Some(
                    MenuRow::new(l.label().to_owned(), Hit::SetLayout(*l))
                        .checked(current == Some(*l)),
                )
            })
            .collect();
        self.open_menu_under(anchor, &rows);
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
            self.set_sel_str(routed.state, "fill", &ui::color::to_hex(col));
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
            self.set_sel_f64(routed.state, "font_size", v);
        }
        true
    }

    fn on_release(&mut self, doc: &mut AutoCommit, handled: &mut bool) {
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
                self.rail_drawer.close();
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
        _text: Option<&str>,
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
                self.rail_drawer.close();
                dismissed = true;
            }
            if self.inspector_layout() == SplitLayout::Overlay && self.inspector_open() {
                self.inspector_drawer.close();
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

    /// Keys the focused text (a text box, the speaker notes, the deck-title
    /// rename) left alone: Escape ends editing; everything else (global
    /// undo and redo) falls through to the toolkit.
    fn on_key_editing_text(&mut self, key: &Key, handled: &mut bool) {
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.end_text_edit();
            self.editing_notes = false;
            self.editing_title = false;
            self.mark();
            *handled = true;
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
                    self.begin_text_edit(false);
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
            self.text_editor = Some(TextBoxEditor::open(doc, &slide_id, &element));
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
        self.reload(routed.state);
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
        let color = self.slide().map_or(pal.text, |slide| {
            render::text_ink(&pal.on(slide.background), slide, &element)
        });
        let Some(editor) = self.text_editor.as_mut() else {
            return;
        };
        if editor.element_id != element.id {
            return;
        }
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

    /// Rewrite a first-format deck's text as rich text, once, as an action
    /// that is not an undo step of its own.
    fn upgrade_text(&mut self, doc: &mut AutoCommit) {
        if !SlidesDoc::needs_upgrade(doc) {
            return;
        }
        self.perform_action(doc, &actions::SlidesAction::UpgradeText);
        let _ = self.actions.take_committed();
        self.reload(doc);
    }
}

/// The inspector's "Custom · #rrggbb" fill button face.
fn custom_colour_hex(hex: &str) -> String {
    tr_args("slides-custom-colour-hex", &[("hex", ArgValue::from(hex))])
}

mod editor_chrome;
#[cfg(test)]
use editor_chrome::UiButton;
use editor_chrome::{ButtonKind, Chrome, ChromeMetrics};

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

    /// Centre snapping inside/outside its threshold and the presenter clock format.
    #[test]
    fn element_snapping_and_present_clock() {
        // Case `snaps_element_center_to_slide_center`.
        {
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

        // Case `no_snap_when_outside_threshold`.
        {
            let mut s = Slide::new(Layout::Blank);
            let id = s.add(Element::shape(ElementKind::Rect, 0.0, 0.0, 50.0, 50.0));
            let app = app_with(s);
            let (sx, sy, guides) = app.snapped_move(&id, 300.0, 300.0, 50.0, 50.0, 4.0);
            assert_eq!((sx, sy), (300.0, 300.0));
            assert!(guides.is_empty());
        }

        // Case `present_clock_formats_minutes_seconds`.
        {
            let app = SlidesApp::new();
            // No presentation started → 0:00.
            assert_eq!(app.presentation.clock(), "0:00");
        }
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

    /// Shape/menu inserts and inspector property edits write through to the deck CRDT.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn insert_tools_and_inspector_edits_write_through_to_the_crdt() {
        // Case `inserting_a_shape_writes_through_to_the_crdt_deck`.
        {
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
            let reloaded =
                crate::doc::SlidesDoc::load(&doc).expect("deck round-trips from the doc");
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

        // Case `choosing_a_tool_behind_more_inserts_that_element`.
        {
            let (mut app, mut tr) = measured_app(480.0, 1.0);
            let mut doc = seeded_doc(&app);
            app.reload(&doc);
            app.refresh_chrome_metrics(&mut tr, &Theme::dark());
            let before = app.slide().unwrap().elements.len();

            let chrome = app.compute_chrome();
            let hidden = chrome
                .overflow
                .iter()
                .find_map(|b| match b.hit {
                    Hit::Insert(kind) if kind != ElementKind::Image => Some(kind),
                    _ => None,
                })
                .expect("a narrow header row puts a shape tool behind More");
            app.apply_hit(&mut doc, Hit::ToggleToolbarMenu);
            assert!(
                app.ctx_menu.is_open(),
                "More opens the menu of hidden tools"
            );
            let row = app
                .ctx_hits
                .iter()
                .position(|hit| matches!(hit, Some(Hit::Insert(kind)) if *kind == hidden))
                .expect("the menu offers the hidden tool");
            app.ctx_menu.click_item(row);
            let chosen = app.ctx_menu.take_selection().expect("the row is chosen");
            let hit = app.ctx_hits[chosen].clone().expect("a tool row");
            app.apply_hit(&mut doc, hit);
            assert!(!app.ctx_menu.is_open(), "picking a tool closes the menu");
            assert_eq!(
                app.slide().unwrap().elements.len(),
                before + 1,
                "the chosen shape was actually inserted"
            );
        }

        // Case `font_size_number_input_steps_and_writes_through`.
        // The font-size `NumberInput`'s + stepper raises the size through the real
        // event path (item: adopt the shared `NumberInput` for font size).
        {
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

        // Case `fill_color_picker_writes_a_hex_fill`.
        // The embeddable `ColorPicker` writes a concrete hex fill through the real
        // event path (item: integrate the `ColorPicker` for the inspector fill).
        {
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

        // Case `every_inspector_property_edit_writes_through_to_the_crdt`.
        {
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
                (loaded(&doc).slides[0].element(&id).unwrap().font_size - (el0.font_size + 6.0))
                    .abs()
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
    }

    /// Present-mode navigation, clamping, Home/End, number keys, Escape and black/white-out.
    #[test]
    fn present_mode_navigation_keys_and_blanking() {
        // Case `present_advances_and_reverses_via_real_handler`.
        {
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

        // Case `present_clamps_at_both_ends`.
        {
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

        // Case `present_home_and_end_jump_to_first_and_last`.
        {
            let mut app = presenting_app();
            app.cur_slide = 2;
            assert!(press_named(&mut app, NamedKey::End));
            assert_eq!(app.cur_slide, app.deck.slides.len() - 1, "End → last slide");
            assert!(press_named(&mut app, NamedKey::Home));
            assert_eq!(app.cur_slide, 0, "Home → first slide");
        }

        // Case `present_number_key_jumps_to_slide`.
        {
            let mut app = presenting_app();
            assert!(press_char(&mut app, "3"));
            assert_eq!(app.cur_slide, 2, "'3' jumps to slide index 2");
            assert!(press_char(&mut app, "1"));
            assert_eq!(app.cur_slide, 0, "'1' jumps to slide index 0");
        }

        // Case `present_escape_exits_to_editor`.
        {
            let mut app = presenting_app();
            app.cur_slide = 2;
            assert!(press_named(&mut app, NamedKey::Escape));
            assert_eq!(app.mode, Mode::Edit, "Esc leaves present mode");
            // Position is preserved on exit (editor opens where you stopped).
            assert_eq!(app.cur_slide, 2);
        }

        // Case `blackout_and_whiteout_toggle_and_navigation_resumes`.
        {
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
    }

    /// Fullscreen intent, output targeting, audience lifecycle and compositor exits.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn present_mode_outputs_fullscreen_and_audience_lifecycle() {
        // Case `fullscreen_intent_tracks_present_mode`.
        {
            // `Widget` is in scope via `use super::*`.
            let mut app = SlidesApp::new();
            // Editor: no fullscreen request.
            assert_eq!(FullscreenProvider::fullscreen_intent(&app), Some(false));
            // Present: request true fullscreen.
            app.enter_present();
            assert_eq!(FullscreenProvider::fullscreen_intent(&app), Some(true));
            // Back to editor via Esc: request windowed again.
            app.exit_present();
            assert_eq!(FullscreenProvider::fullscreen_intent(&app), Some(false));
        }

        // Case `present_uses_the_shared_mechanism_targeting_the_largest_output`.
        {
            use ui::fullscreen::{FullscreenTarget, FullscreenView};
            let app = presenting_app();
            // Present routes through the ONE shared fullscreen mechanism the video
            // player also uses: a distinct presentation view on the largest monitor
            // (the projector / big screen), which the App enumerates + selects.
            let params = FullscreenProvider::fullscreen_params(&app);
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
                FullscreenProvider::fullscreen_intent(&app),
                Some(true),
                "and it drives the shared reconcile"
            );
        }

        // Case `audience_lifecycle_keeps_dashboard_local_and_preserves_single_output_fallback`.
        {
            use ui::fullscreen::{AudiencePresentationRequest, FullscreenTarget};

            let mut app = presenting_app();
            assert_eq!(
                AudiencePresentationProvider::audience_presentation_request(&app),
                Some(AudiencePresentationRequest::new(FullscreenTarget::Largest))
            );
            AudiencePresentationProvider::on_audience_presentation_changed(&mut app, true);
            assert!(app.presentation.has_external_audience());
            assert_eq!(
                FullscreenProvider::fullscreen_intent(&app),
                Some(false),
                "the original local window must not become the audience surface"
            );
            FullscreenProvider::on_fullscreen_changed(&mut app, false);
            assert_eq!(
                app.mode,
                Mode::Present,
                "the host's intentional presenter unfullscreen must not end the talk"
            );
            AudiencePresentationProvider::on_audience_presentation_changed(&mut app, false);
            assert_eq!(
                FullscreenProvider::fullscreen_intent(&app),
                Some(true),
                "loss of the external output returns to the established one-output slideshow"
            );
        }

        // Case `presentation_output_resource_policy_is_local_and_selects_primary_external`.
        {
            let mut app = SlidesApp::new();
            let mut doc = AutoCommit::new();
            let payload =
                serde_json::to_vec(&crate::actions::SlidesOperation::StartPresentationOn {
                    output: crate::actions::PresentationOutput::PrimaryExternal,
                })
                .expect("presentation output operation serializes");
            assert!(matches!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        crate::actions::SLIDES_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .expect("presentation output operation is accepted"),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(app.mode, Mode::Present);
            assert_eq!(
                app.presentation_target,
                ui::fullscreen::FullscreenTarget::PrimaryExternal,
                "the external-display policy must not collapse to the first output, often the laptop"
            );
        }

        // Case `compositor_exit_of_fullscreen_leaves_present_mode`.
        {
            let mut app = presenting_app();
            assert_eq!(app.mode, Mode::Present);
            // A compositor-driven fullscreen exit (e.g. a global un-fullscreen
            // keybind) must drop us out of present mode so the editor chrome
            // returns rather than painting a windowed borderless slideshow.
            FullscreenProvider::on_fullscreen_changed(&mut app, false);
            assert_eq!(
                app.mode,
                Mode::Edit,
                "losing fullscreen leaves present mode"
            );
            // Idempotent + safe when not presenting: a redundant notification is a
            // no-op, and gaining fullscreen never forces present mode on.
            FullscreenProvider::on_fullscreen_changed(&mut app, false);
            assert_eq!(app.mode, Mode::Edit);
            FullscreenProvider::on_fullscreen_changed(&mut app, true);
            assert_eq!(
                app.mode,
                Mode::Edit,
                "a fullscreen grant alone doesn't start a talk"
            );
        }

        // Case `present_output_plan_pairs_audience_with_presenter`.
        {
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

        // Case `entering_present_does_not_request_fullscreen_until_presenting`.
        {
            let app = SlidesApp::new();
            assert!(!app.wants_fullscreen(), "editor mode is not fullscreen");
        }
    }

    /// The presenter dashboard, speaker timer, idle rest and reduced motion.
    #[test]
    fn presenter_dashboard_timer_and_idle_frames() {
        // Case `presenter_dashboard_toggles_with_p`.
        {
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

        // Case `speaker_timer_pause_and_reset`.
        {
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

        // Case `present_mode_rests_when_idle_and_wakes_for_a_transition`.
        {
            let mut app = presenting_app();
            app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
            // Settle the flaps so nothing is mid-animation.
            for _ in 0..3 {
                app.sync_flaps(None);
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

        // Case `reduced_motion_settles_presentation_navigation_in_one_pass`.
        {
            let mut app = presenting_app();
            app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
            app.next_slide(1);
            assert!(app.presentation.needs_animation_frame());

            Widget::set_motion_reduced(&mut app, true);

            assert!(!app.presentation.needs_animation_frame());
            assert_eq!(app.presentation.transition_alpha(true), 1.0);
            assert!(!app.needs_animation_frame());
        }
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
        a.sync_flaps(None);
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
        app.sync_flaps(None);
        (app, tr)
    }

    /// The content column's header-row controls, left to right (the
    /// sidebar toggle excluded).
    fn toolbar_controls(chrome: &Chrome) -> Vec<&UiButton> {
        let mut controls: Vec<&UiButton> = chrome
            .zone_buttons(editor_chrome::Zone::Content)
            .filter(|b| !matches!(b.hit, Hit::ToggleRail))
            .collect();
        controls.sort_by(|a, b| a.rect.x0.total_cmp(&b.rect.x0));
        controls
    }

    /// An app at `w` with its inspector drawer summoned open (it docks on a
    /// wide window anyway).
    fn inspector_app(w: f64, text_scale: f32) -> (SlidesApp, TextRenderer) {
        let (mut app, tr) = measured_app(w, text_scale);
        app.sel = app
            .slide()
            .and_then(|s| s.elements.first().map(|e| e.id.clone()));
        app.inspector_drawer.open();
        app.inspector_drawer.snap(true);
        app.sync_flaps(None);
        (app, tr)
    }

    /// The editor chrome follows measured text and the theme's metrics at 1x
    /// and 2x, on wide and phone widths.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn chrome_density_follows_measured_text_at_every_width() {
        // Case `title_yields_to_the_leftmost_placed_toolbar_control_at_480`.
        // At 480 px every insert tool is behind More; the title is elided to
        // the room before it rather than painted under it.
        {
            let (app, _tr) = measured_app(480.0, 1.0);
            let chrome = app.compute_chrome();
            let first = toolbar_controls(&chrome)
                .first()
                .map(|b| b.rect.x0)
                .expect("header controls");
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
            let max_w = title.max_w.expect("the title is elided at paint time");
            assert!(chrome.title_pos.x + max_w <= first + 0.01);
            assert!(
                chrome
                    .buttons
                    .iter()
                    .any(|b| matches!(b.hit, Hit::ToggleToolbarMenu)),
                "the hidden tools stay reachable through More"
            );
        }

        // Case `one_x_chrome_follows_the_theme_metrics`.
        {
            let (app, _tr) = measured_app(1280.0, 1.0);
            let chrome = app.compute_chrome();
            let theme = Theme::dark();
            let metrics = &theme.tokens.metrics;
            assert!(
                (chrome.toolbar.height() - f64::from(metrics.header_height)).abs() < 0.01,
                "the header row is the shell's"
            );
            let present = chrome
                .buttons
                .iter()
                .find(|b| matches!(b.hit, Hit::Present))
                .expect("Present");
            assert!(
                (present.rect.height() - ControlSizeHeight::regular()).abs() < 0.01,
                "a Regular control"
            );
            assert!(
                matches!(
                    present.kind,
                    ButtonKind::Command {
                        role: ui::widget::ButtonRole::Primary,
                        ..
                    }
                ),
                "Present is the one primary action"
            );
            let add = chrome
                .buttons
                .iter()
                .find(|b| matches!(b.hit, Hit::NewSlide) && chrome.rail.contains(b.rect.center()))
                .expect("the sidebar's new-slide action");
            assert!(
                (add.rect.height() - f64::from(metrics.min_pointer_target)).abs() < 0.01,
                "a pointer-target square"
            );
            // Exactly one primary control in the window.
            assert_eq!(
                chrome
                    .buttons
                    .iter()
                    .filter(|b| matches!(
                        b.kind,
                        ButtonKind::Command {
                            role: ui::widget::ButtonRole::Primary,
                            ..
                        }
                    ))
                    .count(),
                1
            );
        }

        // Case `two_x_chrome_rows_follow_measured_text`.
        // At 2× every header control and inspector row follows the measured
        // text: labels fit their slots, header controls share one centre line,
        // and no section label is painted under the control below it.
        {
            let (app, mut tr) = inspector_app(1440.0, 2.0);
            let theme = Theme::dark().with_text_scale(2.0);
            let chrome = app.compute_chrome();
            let m = app.chrome_metrics.clone();
            assert!(
                chrome.toolbar.height() >= m.control_h + 2.0 * m.s3 - 0.01,
                "the header row holds a 2x control"
            );

            let controls = toolbar_controls(&chrome);
            let centre = controls[0].rect.center().y;
            for b in &controls {
                assert!((b.rect.center().y - centre).abs() < 0.01, "one centre line");
            }
            for pair in controls.windows(2) {
                assert!(
                    pair[0].rect.x1 <= pair[1].rect.x0 + 0.01,
                    "header controls never overlap"
                );
            }
            for b in &chrome.buttons {
                if let ButtonKind::Command { label, icon, .. } = &b.kind {
                    let face = ui::widget::ButtonFace::new(b.rect, label);
                    let face = match icon {
                        Some(icon) => face.icon(*icon),
                        None => face,
                    };
                    let need = face.intrinsic_width(&mut tr, &theme);
                    assert!(
                        b.rect.width() >= need - 1.0,
                        "'{label}' needs {need:.1}, has {:.1}",
                        b.rect.width()
                    );
                }
            }

            // Inspector: section labels keep clear of the control that
            // follows them; segments fit their labels.
            let in_inspector = |r: Rect| chrome.inspector_body.contains(r.center());
            for l in chrome.labels.iter().filter(|l| {
                l.role == ui::TextRole::Subheading && chrome.inspector_body.contains(l.pos)
            }) {
                let label = Rect::new(l.pos.x, l.pos.y, l.pos.x + 1.0, l.pos.y + m.lh_subheading);
                for b in chrome.buttons.iter().filter(|b| in_inspector(b.rect)) {
                    assert!(
                        b.rect.y0 >= label.y1 - 0.01 || b.rect.y1 <= label.y0 + 0.01,
                        "'{}' label collides with a control at {:.1}..{:.1}",
                        l.text,
                        b.rect.y0,
                        b.rect.y1
                    );
                }
            }
            let style = ui::widget::ControlSize::Regular
                .label_style(&theme)
                .strong();
            for b in chrome.buttons.iter().filter(|b| in_inspector(b.rect)) {
                if let ButtonKind::Segment { label, .. } = &b.kind {
                    let (w, _) = tr.measure_style(label, None, &style);
                    assert!(
                        b.rect.width() >= f64::from(w) + 2.0 * m.s3 - 0.5,
                        "segment '{label}' needs {w:.1}, cell is {:.1}",
                        b.rect.width()
                    );
                }
            }
            // The notes follow the sections and never fold back over them.
            let notes = chrome.notes_box.expect("notes box");
            let sections_bottom = chrome
                .buttons
                .iter()
                .filter(|b| in_inspector(b.rect) && !matches!(b.hit, Hit::EditNotes))
                .map(|b| b.rect.y1)
                .fold(0.0_f64, f64::max);
            assert!(
                notes.y0 >= sections_bottom - 0.01,
                "notes under the sections"
            );
            let mut inspector_controls: Vec<Rect> = chrome
                .buttons
                .iter()
                .filter(|b| in_inspector(b.rect))
                .map(|b| b.rect)
                .collect();
            inspector_controls.extend(chrome.font_input_rect);
            for (i, a) in inspector_controls.iter().enumerate() {
                for b in inspector_controls.iter().skip(i + 1) {
                    let overlap = a.intersect(*b);
                    assert!(
                        overlap.width() <= 0.01 || overlap.height() <= 0.01,
                        "inspector controls overlap: {a:?} vs {b:?}"
                    );
                }
            }
        }

        // Case `phone_header_elides_the_title_before_its_controls`.
        {
            let app = app_sized(390.0);
            let chrome = app.compute_chrome();
            let first_control = toolbar_controls(&chrome)
                .first()
                .map(|b| b.rect.x0)
                .unwrap();
            assert!(
                chrome.title_rect.x1 <= first_control + 0.01,
                "the title never runs under the header controls"
            );
        }

        // Case `large_text_keeps_every_route_reachable`.
        {
            let (app, _tr) = measured_app(1440.0, 2.0);
            let chrome = app.compute_chrome();
            for hit in [Hit::ToggleInspector, Hit::ToggleRail, Hit::Present] {
                let target = app.semantic_target(&hit);
                assert!(
                    chrome
                        .buttons
                        .iter()
                        .any(|b| app.semantic_target(&b.hit) == target),
                    "{target} stays reachable at 2x"
                );
            }
            assert!(
                chrome.slide_dest.width() > 600.0,
                "large type keeps a practical authored-slide canvas"
            );
        }

        // Case `deck_theme_swatches_are_named_and_never_overlap`.
        {
            let mut app = app_sized(1280.0);
            app.sel = None;
            let chrome = app.compute_chrome();
            let swatches: Vec<&UiButton> = chrome
                .buttons
                .iter()
                .filter(|b| matches!(b.kind, ButtonKind::Theme { .. }))
                .collect();
            assert_eq!(swatches.len(), PaletteKind::ALL.len());
            for pair in swatches.windows(2) {
                assert!(
                    pair[0].rect.x1 <= pair[1].rect.x0 + 0.5,
                    "theme swatches never overlap"
                );
            }
            for b in &swatches {
                assert!(
                    app.tooltip_at(b.rect.center()).is_some(),
                    "each theme is named"
                );
            }
        }

        // Case `alignment_segments_fill_the_inspector_column`.
        {
            let (mut app, _doc, _id) = selected_text_app();
            sized(&mut app, 1280.0, 800.0);
            let chrome = app.compute_chrome();
            let segments: Vec<Rect> = chrome
                .buttons
                .iter()
                .filter_map(|button| matches!(button.hit, Hit::SetAlign(_)).then_some(button.rect))
                .collect();
            assert_eq!(segments.len(), TextAlign::ALL.len());
            let m = &app.chrome_metrics;
            let left = chrome.inspector.x0 + m.s3;
            let right = chrome.inspector.x1 - m.s3;
            assert!((segments[0].x0 - left).abs() < 0.01);
            assert!((segments.last().unwrap().x1 - right).abs() < 0.01);
            for pair in segments.windows(2) {
                if (pair[0].y0 - pair[1].y0).abs() < 0.5 {
                    assert!((pair[0].x1 - pair[1].x0).abs() < 0.01, "segments meet");
                }
            }
        }
    }

    /// A height a Regular control has under the stock theme.
    struct ControlSizeHeight;

    impl ControlSizeHeight {
        fn regular() -> f64 {
            ui::widget::ControlSize::Regular.height(&Theme::dark())
        }
    }

    /// The slide sidebar docks when wide and is a floating drawer when narrow.
    #[test]
    fn rail_docks_wide_and_overlays_narrow() {
        let base = ui::tokens::Metrics::BASE;
        // Case `wide_window_docks_the_rail_beside_the_canvas`.
        {
            let app = app_sized(1280.0);
            assert_eq!(app.rail_layout(), SplitLayout::Docked);
            assert!(app.rail_open(), "a docked sidebar is open by default");
            let chrome = app.compute_chrome();
            assert_eq!(chrome.layout, SplitLayout::Docked);
            assert!((chrome.rail.x0 - app.rect.x0).abs() < 1e-6);
            assert!(
                (chrome.rail.width() - f64::from(base.sidebar_min_width)).abs() < 0.5,
                "docked at the shell's narrow sidebar width"
            );
            assert!(
                chrome.slide_dest.x0 > chrome.rail.x1 - 1.0,
                "the canvas sits beside the docked sidebar"
            );
            assert_eq!(chrome.scrim.width(), 0.0, "no scrim while docked");
            let toggle = chrome
                .buttons
                .iter()
                .find(|b| matches!(b.hit, Hit::ToggleRail))
                .expect("the sidebar toggle");
            assert!(
                chrome.rail.contains(toggle.rect.center()),
                "the toggle sits at the sidebar's top-leading corner"
            );
        }

        // Case `narrow_window_collapses_the_rail_to_a_closed_drawer`.
        {
            let app = app_sized(600.0);
            assert_eq!(app.rail_layout(), SplitLayout::Overlay);
            assert!(!app.rail_open(), "a drawer is closed by default");
            let chrome = app.compute_chrome();
            assert_eq!(chrome.layout, SplitLayout::Overlay);
            assert_eq!(chrome.rail.width(), 0.0, "a closed drawer shows nothing");
            assert_eq!(chrome.scrim.width(), 0.0, "no scrim while it is closed");
            let toggle = chrome
                .buttons
                .iter()
                .find(|b| matches!(b.hit, Hit::ToggleRail))
                .expect("the header row offers the sidebar toggle");
            assert!(chrome.toolbar.contains(toggle.rect.center()));
        }

        // Case `opening_the_drawer_floats_the_rail_over_a_scrim`.
        {
            let mut app = app_sized(600.0);
            app.rail_drawer.toggle();
            app.rail_drawer.snap(true);
            app.sync_flaps(None);
            assert!(app.rail_open());
            let chrome = app.compute_chrome();
            assert!(
                chrome.rail.x0 > app.rect.x0,
                "the drawer floats inside the window"
            );
            assert!(chrome.scrim.width() > 0.0, "an open drawer dims the window");
            assert!(
                chrome
                    .zone_buttons(editor_chrome::Zone::Rail)
                    .any(|b| matches!(b.hit, Hit::ToggleRail)),
                "the open drawer carries its own toggle"
            );
        }

        // Case `the_toggle_hit_opens_and_closes_the_drawer`.
        {
            let mut app = app_sized(600.0);
            let mut doc = AutoCommit::new();
            assert!(!app.rail_open());
            app.apply_hit(&mut doc, Hit::ToggleRail);
            assert!(app.rail_open(), "ToggleRail opens the drawer");
            app.apply_hit(&mut doc, Hit::ToggleRail);
            assert!(!app.rail_open(), "ToggleRail closes it again");
        }
    }

    /// The inspector docks when wide and is a floating drawer when narrow.
    #[test]
    fn inspector_docks_wide_and_overlays_narrow() {
        let base = ui::tokens::Metrics::BASE;
        // Case `wide_window_docks_the_inspector`.
        {
            let app = app_sized(1280.0);
            assert_eq!(app.inspector_layout(), SplitLayout::Docked);
            assert!(app.inspector_open());
            let chrome = app.compute_chrome();
            assert_eq!(chrome.inspector_layout, SplitLayout::Docked);
            assert!(
                (chrome.inspector.width() - f64::from(base.inspector_width)).abs() < 0.5,
                "docked at the shell's inspector width"
            );
            assert!(chrome.slide_dest.x1 < chrome.inspector.x0 + 1.0);
        }

        // Case `narrow_window_collapses_the_inspector_to_a_closed_drawer`.
        {
            let app = app_sized(700.0);
            assert_eq!(app.inspector_layout(), SplitLayout::Overlay);
            assert!(!app.inspector_open());
            let chrome = app.compute_chrome();
            assert_eq!(chrome.inspector.width(), 0.0);
            assert!(
                chrome
                    .buttons
                    .iter()
                    .any(|b| matches!(b.hit, Hit::ToggleInspector)),
                "the header row offers the inspector toggle"
            );
        }

        // Case `the_toggle_hit_opens_the_drawer`.
        {
            let mut app = app_sized(700.0);
            let mut doc = AutoCommit::new();
            app.apply_hit(&mut doc, Hit::ToggleInspector);
            assert!(app.inspector_open());
            app.apply_hit(&mut doc, Hit::ToggleInspector);
            assert!(!app.inspector_open());
        }
    }

    /// Compact drawers, their exclusivity, and reflow across the breakpoint.
    #[test]
    fn compact_editor_drawers_and_breakpoint_reflow() {
        // Case `compact_editor_keeps_the_canvas_wide_with_both_panels_as_drawers`.
        {
            let app = app_sized(820.0);
            let chrome = app.compute_chrome();
            assert_eq!(chrome.layout, SplitLayout::Overlay);
            assert_eq!(chrome.inspector_layout, SplitLayout::Overlay);
            assert!(
                chrome.slide_dest.width() > 700.0,
                "closed drawers leave a useful slide canvas at compact widths"
            );
            assert!(
                chrome
                    .buttons
                    .iter()
                    .any(|b| matches!(b.hit, Hit::ToggleRail))
                    && chrome
                        .buttons
                        .iter()
                        .any(|b| matches!(b.hit, Hit::ToggleInspector)),
                "both drawers keep their toggles in the header row"
            );
        }

        // Case `compact_drawers_are_mutually_exclusive`.
        {
            let mut app = app_sized(820.0);
            let mut doc = AutoCommit::new();
            app.apply_hit(&mut doc, Hit::ToggleRail);
            assert!(app.rail_open());
            assert!(!app.inspector_open());
            app.apply_hit(&mut doc, Hit::ToggleInspector);
            assert!(app.inspector_open(), "the inspector opens");
            assert!(!app.rail_open(), "opening it closes the slide drawer");
        }

        // Case `each_size_class_keeps_its_own_choice`.
        {
            let mut app = app_sized(600.0);
            app.apply_hit(&mut AutoCommit::new(), Hit::ToggleRail);
            assert!(app.rail_open());
            // Wide: the docked sidebar is open by default.
            app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            app.sync_flaps(None);
            assert_eq!(app.rail_layout(), SplitLayout::Docked);
            assert!(app.rail_open(), "docked default is open");
            // Hide it while wide; narrow again: the drawer is as it was left.
            app.apply_hit(&mut AutoCommit::new(), Hit::ToggleRail);
            assert!(!app.rail_open());
            app.rect = Rect::new(0.0, 0.0, 600.0, 800.0);
            app.sync_flaps(None);
            assert_eq!(app.rail_layout(), SplitLayout::Overlay);
            app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            app.sync_flaps(None);
            assert!(!app.rail_open(), "the wide window remembers it was hidden");
        }

        // Case `same_instance_reflows_both_panels_wide_then_narrow`.
        {
            let mut app = app_sized(1280.0);
            assert_eq!(app.rail_layout(), SplitLayout::Docked);
            assert_eq!(app.inspector_layout(), SplitLayout::Docked);
            app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
            app.sync_flaps(None);
            assert_eq!(app.rail_layout(), SplitLayout::Overlay);
            assert_eq!(app.inspector_layout(), SplitLayout::Overlay);
            let narrow = app.compute_chrome();
            assert!(
                narrow.slide_dest.width() > 560.0 - 2.0 * 24.0 - 1.0,
                "the canvas reclaims the collapsed panels' width"
            );
        }
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
        a.sync_flaps(None);
        a
    }

    /// Wheel scrolling of rail and overview, and the overview scrollbar drag.
    #[test]
    fn wheel_and_scrollbar_scroll_rail_and_overview() {
        // Case `wheel_scrolls_the_thumbnail_rail_only_over_the_rail`.
        {
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

        // Case `wheel_scrolls_overview_and_last_slide_becomes_reachable`.
        {
            let mut app = SlidesApp::new();
            app.deck = crate::model::seed_deck();
            app.bootstrapped = true;
            app.mode = Mode::Overview;
            // A short window, so the 5-slide deck overflows the grid.
            app.rect = Rect::new(0.0, 0.0, 800.0, 400.0);
            let (_, _, max_scroll) = app.compute_overview();
            assert!(
                max_scroll > 0.0,
                "the 5-slide deck overflows the 800x400 overview"
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

        // Case `overview_scrollbar_thumb_drag_scrolls_the_grid`.
        {
            let mut app = SlidesApp::new();
            app.deck = crate::model::seed_deck();
            app.bootstrapped = true;
            app.mode = Mode::Overview;
            app.rect = Rect::new(0.0, 0.0, 800.0, 400.0);
            let (area, _, max_scroll) = app.compute_overview();
            let (_, thumb) = app
                .overview
                .scrollbar_rects(area, 0.0, max_scroll)
                .expect("an overflowing grid has a scroll bar");
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
    }

    // --- present mode rests -------------------------------------------------

    // --- toolbar overflow menu ---------------------------------------------

    fn seeded_doc(app: &SlidesApp) -> AutoCommit {
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        doc
    }

    // --- deck-theme palette pills ------------------------------------------

    // --- selection handles clear content -----------------------------------

    /// Selection handles, first-grab drags, edit-entry clicks, moves, resizes and caret placement.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn element_pointer_gestures_select_move_resize_and_edit() {
        // Case `selection_handles_never_cover_the_element_content`.
        {
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
            app.sync_flaps(None);
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

        // Case `selected_element_drags_on_first_grab`.
        // Regression for the "move-on-first-click" bug: an already-selected element
        // must arm a move on the very first press (not swallow it into text-edit),
        // and dragging must actually move it.
        {
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

        // Case `double_click_text_box_enters_edit`.
        // A deliberate double-click on a selected text box enters edit mode (the
        // opt-in gesture, now that a single grab moves it).
        {
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

        // Case `second_click_on_a_selected_text_box_enters_edit_mode`.
        {
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

        // Case `dragging_an_element_persists_the_move_to_the_crdt`.
        {
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

        // Case `resizing_via_a_selection_handle_persists_to_the_crdt`.
        {
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

        // Case `clicking_inside_the_editing_text_arms_caret_placement_not_a_box_move`.
        // A press inside the box currently being edited places the caret (and arms
        // a selection drag) rather than starting a box move.
        {
            let (mut app, mut doc, _id) = selected_text_app();
            edit_frame(&mut app, &mut doc);
            let chrome = app.compute_chrome();
            let er = render::element_dest(
                chrome.slide_dest,
                chrome.scale,
                app.selected_element().unwrap(),
            );
            app_dispatch(&mut app, &mut doc, &cursor_ev(er.center()));
            assert!(
                app_dispatch(&mut app, &mut doc, &left_press_ev()),
                "the press is handled"
            );
            assert!(
                app.text_drag,
                "a press inside the editing box starts a selection drag"
            );
            assert!(!app.drag.active(), "the box is not being moved");
            app_dispatch(&mut app, &mut doc, &release_ev());
            assert!(!app.text_drag);
            assert!(app.editing_text, "the box is still being edited");
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

    /// The image store decodes PNGs; the image tool uses the host portal and a cancel inserts nothing.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn image_insert_uses_the_host_portal_and_decodes_real_pngs() {
        // Case `image_store_decodes_a_real_png_and_fails_gracefully`.
        {
            use crate::assets::{ImageState, ImageStore};
            let path = write_png("slides-unit-decode.png", 8, 6);
            let content = ContentStore::memory();
            let key = crate::assets::import_asset_file(&content, &path)
                .expect("fixture imports as an asset");
            let store = ImageStore::new(content);
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

        // Case `image_tool_requests_the_host_portal_then_a_pick_inserts_a_real_image`.
        {
            let mut app = SlidesApp::new();
            let mut doc = AutoCommit::new();
            crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
            app.reload(&doc);
            let before = app.slide().map_or(0, |s| s.elements.len());

            // The recording host answers the chooser with a chosen file.
            let path = write_png("slides-unit-insert.png", 12, 8);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));

            // The Image tool asks the host portal for a file WITHOUT fabricating an
            // element and WITHOUT drawing any in-app picker (HIG §5).
            ui::host::scoped(&services, || {
                app.apply_hit(&mut doc, Hit::Insert(ElementKind::Image));
            });
            assert!(
                app.portal_rx.is_some(),
                "the Image tool opens the host portal chooser (a real request is in flight)"
            );
            assert_eq!(
                recorder.pick_requests().len(),
                1,
                "exactly one host chooser request"
            );
            assert_eq!(
                app.slide().unwrap().elements.len(),
                before,
                "no element is inserted until the portal returns a chosen file"
            );

            // The exact per-frame drain path takes the chosen-file outcome and
            // lands a real image element (with its src) in the CRDT.
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
                crate::assets::read_asset(&app.content, &el.src).as_deref(),
                Some(&std::fs::read(&path).unwrap()[..])
            );
            assert_eq!(
                app.sel.as_deref(),
                Some(el.id.as_str()),
                "the new image is selected"
            );
        }

        // Case `a_cancelled_portal_pick_inserts_nothing`.
        {
            // Dismissing the host chooser must not fabricate a placeholder element.
            let mut app = SlidesApp::new();
            let mut doc = AutoCommit::new();
            crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
            app.reload(&doc);
            let before = app.slide().map_or(0, |s| s.elements.len());

            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Cancelled);
            ui::host::scoped(&services, || {
                app.apply_hit(&mut doc, Hit::Insert(ElementKind::Image));
            });
            assert!(app.portal_rx.is_some(), "the Image tool asked the host");
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
    }

    // === selection drag + double-click-to-edit ============================

    /// The screen-space centre of the (single) selected element.
    fn sel_center(app: &SlidesApp) -> Point {
        let chrome = app.compute_chrome();
        let el = app.selected_element().unwrap().clone();
        crate::render::element_dest(chrome.slide_dest, chrome.scale, &el).center()
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

    /// Thumbnail drags reorder with a ghost/gap, plain clicks select, and the delete zone removes only while dragging.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn thumbnail_drag_reorders_selects_and_deletes() {
        // Case `dragging_a_thumbnail_reorders_the_deck_in_the_crdt`.
        // Dragging a thumbnail down the rail past the threshold reorders the deck
        // and persists the move to the CRDT — the whole gesture on the real
        // `on_press` → `CursorMoved` → `on_release` path.
        {
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

        // Case `a_plain_thumbnail_click_selects_without_reordering`.
        // A press that never crosses the threshold is a plain click: it selects
        // the slide and leaves the deck order untouched (the model is only mutated
        // on a committed drag).
        {
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

        // Case `a_live_thumbnail_drag_lifts_a_ghost_and_opens_a_reflow_gap`.
        // While a reorder drag is live the grabbed slide lifts out (its original
        // index) and the reflow opens a gap at the drop row — the state the shared
        // rail paints live feedback from. (The ghost-rect / per-item-shift geometry
        // itself is now owned + unit-tested inside `ui`'s `PageSidebar`.)
        {
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

        // Case `dropping_a_slide_on_the_delete_zone_removes_it_from_the_crdt`.
        // Dropping a dragged slide thumbnail on the red dashed delete zone removes
        // that slide from the CRDT (the whole `on_press` → drag → `on_release` path).
        {
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

        // Case `delete_zone_is_inert_when_not_dragging`.
        // The delete zone is only active while a slide is being dragged.
        {
            let (mut app, _doc) = reorder_app();
            let zone = app.slide_delete_zone(app.rail_rect());
            app.last_mouse = zone.center();
            assert!(
                !app.over_delete_zone(),
                "no drag in progress → the zone is inert"
            );
        }
    }

    // === .odp open / save through the app =================================

    /// ODP save/load, `PowerPoint` routing, open outcomes, failures, cancellation and worker disconnects.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn document_open_save_and_worker_outcomes() {
        // Case `app_saves_and_reloads_an_odp`.
        {
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

        // Case `app_routes_powerpoint_and_explains_its_fidelity_boundary`.
        {
            let mut app = SlidesApp::new();
            app.deck = crate::model::seed_deck();
            let path = std::env::temp_dir().join("slides-app-pptx-roundtrip.pptx");
            assert!(app.save_presentation(&path), "deck saves to .pptx");

            let mut opened = SlidesApp::new();
            let mut doc = AutoCommit::new();
            crate::doc::SlidesDoc::seed(&mut doc, &opened.deck);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
            ui::host::scoped(&services, || opened.request_odp_open());
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

        // Case `draining_an_open_outcome_loads_the_deck`.
        {
            let deck = crate::model::seed_deck();
            let bytes = crate::odp::write_odp(&deck, &ContentStore::memory()).unwrap();
            let path = std::env::temp_dir().join("slides-drain-open.odp");
            std::fs::write(&path, &bytes).unwrap();

            let mut app = SlidesApp::new();
            let mut doc = AutoCommit::new();
            crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
            app.reload(&doc);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(path));
            ui::host::scoped(&services, || app.request_odp_open());
            assert_eq!(recorder.pick_requests()[0].title(), tr("slides-open-title"));
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

        // Case `failed_open_is_visible_instead_of_silent`.
        {
            let mut app = SlidesApp::new();
            let mut doc = AutoCommit::new();
            crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(std::env::temp_dir().join(format!(
                "missing-tilo-slides-deck-{}.odp",
                std::process::id()
            ))));
            ui::host::scoped(&services, || app.request_odp_open());
            app.drain_doc_portal(&mut doc);
            wait_for_document_job(&mut app, &mut doc);
            assert!(matches!(app.io_toast.as_ref(), Some((_, ToastKind::Error))));
        }

        // Case `cancelled_document_job_cannot_apply_a_stale_open_result`.
        {
            let mut imported = crate::model::seed_deck();
            imported.title = "stale import".into();
            let path =
                std::env::temp_dir().join(format!("slides-stale-{}.odp", uuid::Uuid::new_v4()));
            std::fs::write(
                &path,
                crate::odp::write_odp(&imported, &ContentStore::memory()).unwrap(),
            )
            .unwrap();
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

        // Case `disconnected_document_worker_settles_the_accepted_resource_operation`.
        {
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

        // Case `a_finished_save_wakes_its_idle_host`: an idle host syncs only
        // when woken, so a save nobody wakes for stays Accepted forever.
        {
            let mut scope = ui::wake::WakeScope::new();
            let _entry = scope.enter();
            let (sender, wakes) = std::sync::mpsc::channel();
            scope
                .bind(move |event| {
                    let _ = sender.send(event);
                })
                .unwrap();
            let path = std::env::temp_dir()
                .join(format!("slides-idle-save-{}.pptx", uuid::Uuid::new_v4()));
            let mut app = SlidesApp::new();
            let mut doc = AutoCommit::new();
            crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
            app.resource_invocation = Some(tilo_actions::InvocationId::new());
            assert!(app.queue_document_save(path.clone()));
            tilo_test_env::recv("the finished save's wake", &wakes);
            app.drain_document_worker(&mut doc);
            assert!(
                app.dispatched_save.is_none(),
                "the woken sync settles the save"
            );
            assert!(matches!(
                app.io_toast.as_ref(),
                Some((_, ToastKind::Success))
            ));
            let _ = std::fs::remove_file(path);
        }

        // Case `ensure_odp_ext_appends_when_missing`.
        {
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
    }

    // === right-click context menus ========================================

    /// Right-click menus for canvas elements and slide thumbnails.
    #[test]
    fn context_menus_act_on_elements_and_slides() {
        // Case `right_click_element_opens_menu_and_actions_apply`.
        // Right-clicking a canvas element selects it and opens the element menu
        // whose rows map to the reorder / duplicate / delete actions.
        {
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

        // Case `right_click_thumbnail_opens_slide_menu`.
        // Right-clicking a slide thumbnail selects that slide and opens the shared
        // rail's own duplicate / move / delete menu; choosing Delete removes it.
        {
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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
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
    fn release_ev() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
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
    fn loaded(doc: &AutoCommit) -> Deck {
        crate::doc::SlidesDoc::load(doc).expect("deck round-trips from the document")
    }

    /// Open the selected text box for editing and draw one canvas frame, so
    /// its editor is open and laid out where the box is.
    fn edit_frame(app: &mut SlidesApp, doc: &mut AutoCommit) {
        if app.rect.width() <= 0.0 {
            sized(app, 1280.0, 800.0);
        }
        app.editing_text = true;
        Widget::sync(app, doc, &automerge::ROOT);
        text_frame(app);
    }

    /// Draw the text box being edited, as the canvas does each frame.
    fn text_frame(app: &mut SlidesApp) {
        let chrome = app.compute_chrome();
        let theme = Theme::dark();
        let pal = app.palette(&theme);
        let mut tr = TextRenderer::new();
        app.draw_text_editor(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &chrome,
            &pal,
        );
    }

    /// The text box editor's block editor.
    fn text_box(app: &SlidesApp) -> &ui::widget::BlockEditor {
        app.text_editor
            .as_ref()
            .expect("the text box is being edited")
            .host
            .widget()
            .editor()
    }

    /// The text selected in the text box being edited.
    fn edit_selection(app: &SlidesApp) -> String {
        let editor = text_box(app);
        if let Some(text) = editor.selected_text() {
            return text;
        }
        editor
            .blocks()
            .iter()
            .find(|block| block.line.is_focused())
            .and_then(|block| {
                let (start, end) = block.line.selection_range()?;
                block.text().get(start..end).map(str::to_owned)
            })
            .unwrap_or_default()
    }

    /// The caret's byte offset in the text box's focused paragraph.
    fn edit_caret(app: &SlidesApp) -> usize {
        text_box(app)
            .blocks()
            .iter()
            .find(|block| block.line.is_focused())
            .map_or(0, |block| block.line.cursor_pos())
    }

    /// Type `text` into the focused field through the app's event path.
    fn type_text(app: &mut SlidesApp, doc: &mut AutoCommit, text: &str) {
        for ch in text.chars() {
            app_dispatch(app, doc, &char_ev(&ch.to_string()));
        }
    }
    fn sized(app: &mut SlidesApp, w: f64, h: f64) {
        app.rect = Rect::new(0.0, 0.0, w, h);
        app.sync_flaps(None);
    }

    // --- deck title rename (was display-only: set_title had no caller) ------

    /// The inline title editor starts, selects/replaces and commits to the CRDT.
    #[test]
    fn title_rename_edits_and_commits_to_the_crdt() {
        // Case `clicking_the_toolbar_title_starts_a_rename_and_typing_writes_through`.
        {
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

        // Case `title_input_supports_select_all_and_replace`.
        // The inline title editor is the shared `TextInput`, so it supports real
        // selection — Ctrl+A selects the whole title, then typing replaces it.
        {
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

        // Case `title_backspace_and_click_away_commit_persist_to_the_crdt`.
        {
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
    }

    // --- '?' help in Present mode (was a no-op that soft-locked nav) --------

    /// Help overlay in present mode and the deck's accessibility tree and activation.
    #[test]
    fn help_overlay_and_accessibility_expose_the_deck() {
        // Case `help_overlay_is_visible_and_dismissable_in_present_mode`.
        {
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
            app.help_overlay
                .draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, app.rect);

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

        // Case `accessibility_exposes_deck_and_current_slide`.
        {
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

        // Case `accessibility_activation_uses_stable_slide_identity_and_modal_help_owns_tree`.
        {
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
    }

    // --- text caret alignment (was hard-coded to the left edge) -------------

    /// Caret placement under alignment and word-wise moves.
    #[test]
    fn caret_navigation_tracks_alignment_and_lines() {
        // Case `caret_tracks_center_and_right_alignment_not_just_left`.
        // The box's alignment is the editor's: the caret at the end of a short
        // word sits further right as the box goes from left to centre to right.
        {
            let caret_x = |align: TextAlign| {
                let mut s = Slide::new(Layout::Blank);
                let id = s.add(
                    Element::text("Hello", 100.0, 100.0, 600.0, 120.0, 40.0)
                        .with(|e| e.align = align),
                );
                let (mut app, mut doc) = app_doc(s);
                app.sel = Some(id);
                edit_frame(&mut app, &mut doc);
                assert_eq!(text_box(&app).text_align(), render::text_align(align));
                app.ime_cursor_rect().expect("the caret is shown").x0
            };
            let (left, center, right) = (
                caret_x(TextAlign::Left),
                caret_x(TextAlign::Center),
                caret_x(TextAlign::Right),
            );
            assert!(
                left < center && center < right,
                "l={left} c={center} r={right}"
            );
        }

        // Case `ctrl_arrow_moves_word_wise`.
        // Ctrl+Arrow moves word-wise via the shared word-boundary walker.
        {
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
            edit_frame(&mut app, &mut doc);
            app_dispatch_mods(
                &mut app,
                &mut doc,
                Modifiers::from(ModifiersState::CONTROL),
                &named_ev(NamedKey::ArrowLeft),
            );
            assert_eq!(
                edit_caret(&app),
                6,
                "ctrl+left jumps to the start of the last word"
            );
            assert!(
                edit_selection(&app).is_empty(),
                "a plain word move collapses the selection"
            );
        }
    }

    // --- caret line navigation (ArrowUp/Down did nothing) -------------------

    // --- shared multiline speaker-notes editor ------------------------------

    #[test]
    fn the_notes_box_reads_out_the_slides_notes() {
        fn find(node: &ui::a11y::AccessNode) -> Option<&ui::a11y::AccessNode> {
            if node.name.as_deref() == Some("Edit speaker notes") {
                return Some(node);
            }
            node.children.iter().find_map(find)
        }
        let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
        sized(&mut app, 1280.0, 800.0);
        crate::doc::SlidesDoc::splice_notes(&mut doc, 0, 0, 0, "thank the hosts");
        app.reload(&doc);
        let tree = app.accessibility().expect("editor semantics");
        let notes = find(&tree).expect("the notes box is exposed");
        assert_eq!(notes.value.as_deref(), Some("thank the hosts"));
    }

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
        app.draw_notes(&mut scene, Affine::IDENTITY, &mut tr, &theme, nb);

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

    /// Typing, selection, select-all, replace, forward delete and cut edit the
    /// CRDT text, and notes selection/IME/multiline edits the bound notes text.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn text_editing_keys_write_through_the_crdt() {
        // Case `editing_element_text_types_and_deletes_through_the_crdt`.
        {
            let (mut app, mut doc, id) = selected_text_app();
            sized(&mut app, 1280.0, 800.0);
            // Enter opens the text editor on the selected box, caret at the end.
            assert!(keyn(&mut app, &mut doc, NamedKey::Enter));
            assert!(app.editing_text);
            edit_frame(&mut app, &mut doc);
            assert_eq!(edit_caret(&app), "Hi".len());
            type_text(&mut app, &mut doc, "!");
            assert_eq!(
                loaded(&doc).slides[0].element(&id).unwrap().text,
                "Hi!",
                "typed char persisted"
            );
            app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::Backspace));
            assert_eq!(
                loaded(&doc).slides[0].element(&id).unwrap().text,
                "Hi",
                "backspace persisted"
            );
            // Enter starts a paragraph; Escape ends editing, box still selected.
            app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::Enter));
            type_text(&mut app, &mut doc, "yo");
            assert_eq!(loaded(&doc).slides[0].element(&id).unwrap().text, "Hi\nyo");
            app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::Escape));
            assert!(!app.editing_text);
            assert_eq!(app.sel.as_deref(), Some(id.as_str()));
        }

        // Case `shift_arrow_extends_a_selection_over_the_crdt_text`.
        {
            use winit::keyboard::ModifiersState;
            let (mut app, mut doc, _id) = selected_text_app();
            edit_frame(&mut app, &mut doc);
            assert!(edit_selection(&app).is_empty(), "starts with no selection");
            for _ in 0..2 {
                app_dispatch_mods(
                    &mut app,
                    &mut doc,
                    Modifiers::from(ModifiersState::SHIFT),
                    &named_ev(NamedKey::ArrowLeft),
                );
            }
            assert_eq!(edit_selection(&app), "Hi", "the selection spans both chars");
        }

        // Case `select_all_then_backspace_clears_the_element_text_in_the_crdt`.
        {
            use winit::keyboard::ModifiersState;
            let (mut app, mut doc, id) = selected_text_app();
            edit_frame(&mut app, &mut doc);
            app_dispatch_mods(
                &mut app,
                &mut doc,
                Modifiers::from(ModifiersState::CONTROL),
                &char_ev("a"),
            );
            assert_eq!(edit_selection(&app), "Hi", "Ctrl+A selects the whole text");
            app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::Backspace));
            assert_eq!(
                loaded(&doc).slides[0].element(&id).unwrap().text,
                "",
                "the selected text was deleted from the CRDT"
            );
        }

        // Case `typing_over_a_selection_replaces_it_in_the_crdt`.
        {
            let (mut app, mut doc, id) = selected_text_app();
            app.begin_text_edit(true);
            edit_frame(&mut app, &mut doc);
            type_text(&mut app, &mut doc, "X");
            assert_eq!(
                loaded(&doc).slides[0].element(&id).unwrap().text,
                "X",
                "the selection was replaced, not appended to"
            );
        }

        // Case `bold_typed_in_a_box_is_a_mark_and_undo_takes_back_a_word`.
        {
            use winit::keyboard::ModifiersState;
            let (mut app, mut doc, id) = selected_text_app();
            edit_frame(&mut app, &mut doc);
            type_text(&mut app, &mut doc, " ");
            app_dispatch_mods(
                &mut app,
                &mut doc,
                Modifiers::from(ModifiersState::CONTROL),
                &char_ev("b"),
            );
            type_text(&mut app, &mut doc, "bold");
            let element = loaded(&doc).slides[0].element(&id).unwrap().clone();
            assert_eq!(element.text, "Hi bold");
            assert!(
                element.blocks[0]
                    .marks
                    .iter()
                    .any(|span| span.mark == ui::widget::marks::Mark::Bold && span.range == (3..7)),
                "{:?}",
                element.blocks[0].marks
            );
        }

        // Case `notes_selection_ime_and_multiline_edit_the_bound_crdt_text`.
        // Selection replacement, IME and multiline insertion all travel through
        // the real root-widget event path into the directly-bound notes text.
        {
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
    }

    // --- canvas move / resize persistence -----------------------------------

    // --- inspector property edits (every writer, on the real hit path) ------

    // --- slide ops via the app handlers -------------------------------------

    /// Slide operations, the last-slide guard, keyboard nudges/delete and overview card opening.
    #[test]
    fn slide_ops_and_keyboard_edits_update_the_deck() {
        // Case `slide_ops_via_hit_handlers_update_the_deck_and_current_slide`.
        {
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

        // Case `delete_slide_keeps_at_least_one_slide`.
        {
            let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
            assert_eq!(loaded(&doc).slides.len(), 1);
            app.apply_hit(&mut doc, Hit::DeleteSlide);
            assert_eq!(
                loaded(&doc).slides.len(),
                1,
                "the last slide can't be deleted"
            );
        }

        // Case `arrow_nudge_tab_cycle_and_delete_key_edit_the_crdt`.
        {
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

        // Case `clicking_an_overview_card_opens_that_slide_in_the_editor`.
        {
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
    }

    // --- edit-mode keyboard: nudge, Tab cycle, Delete -----------------------

    // --- overview click opens the slide -------------------------------------

    // === shared caret / selection core (HIG §3) ===========================

    // === drag-to-delete zone ==============================================

    /// Brokered text reveal and history restore rebinding notes.
    #[test]
    fn brokered_reveal_and_history_restore_rebind_state() {
        // Case `brokered_text_reveal_restores_exact_slide_element_and_selection`.
        {
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

            assert!(matches!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        crate::actions::SLIDES_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(app.cur_slide, slide_index);
            assert_eq!(app.sel.as_deref(), Some(element_id.as_str()));
            assert!(app.editing_text);
            let mut doc = doc;
            edit_frame(&mut app, &mut doc);
            assert_eq!(
                edit_selection(&app).chars().count(),
                text_len,
                "the revealed text is selected in full"
            );
            assert_eq!(app.mode, Mode::Edit);
        }

        // Case `history_restore_rebinds_notes_before_first_input_without_intervening_sync`.
        {
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
                crate::odp::read_odp(&bytes, &ContentStore::memory())
                    .unwrap()
                    .title,
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
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::SLIDES_OPERATION_TYPE,
                    &payload
                ),
                &mut doc,
                &automerge::ROOT
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
            .recv_timeout(tilo_test_env::hang_bound())
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
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    other_invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::SLIDES_OPERATION_TYPE,
                    &payload
                ),
                &mut doc,
                &automerge::ROOT
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

    /// Dispatched saves settle after restore, stale work is cancelled and stale targets are fenced.
    #[test]
    fn history_interruption_settles_saves_and_fences_stale_work() {
        // Case `successful_dispatched_save_settles_after_history_restore`.
        {
            dispatched_save_after_restore(true);
        }

        // Case `failed_dispatched_save_reports_actual_error_after_history_restore`.
        {
            dispatched_save_after_restore(false);
        }

        // Case `restore_rebinds_notes_by_slide_identity_and_cancels_stale_file_and_title_work`.
        {
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
            for typed in [" one", " two"] {
                let end = app.deck.title.chars().count();
                SlidesDoc::splice_title(&mut doc, end, 0, typed);
                app.reload(&doc);
                doc.get_heads();
                app.sync(&mut doc, &automerge::ROOT);
            }
            assert!(app.editing_title);
            assert!(app.deck.title.ends_with(" one two"));
        }

        // Case `deleted_edit_target_and_stale_resource_descriptors_are_fenced_before_sync`.
        {
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
                ui::resources::resources(&app, &doc)
                    .iter()
                    .any(|r| r.id.as_str().contains("slides"))
            );
            restore(&mut doc, &selected);
            let heads = doc.get_heads();
            assert!(
                ui::resources::resources(&app, &doc).is_empty(),
                "unreconciled resources cannot retain old action payloads"
            );
            app.sync(&mut doc, &automerge::ROOT);
            assert!(!app.editing_notes);
            assert!(app.deck.index_of(&id).is_none());
            assert_eq!(doc.get_heads(), heads);
            assert!(!ui::resources::resources(&app, &doc).is_empty());
        }
    }
}
