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
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::effects::SlidesEffect;
use tilo_rich_text::{FieldOutcome, RichTextField, RichTextSurface, SubmitKey};
use ui::framework::{Dispatcher, EffectAnswer, EffectCx, Effected, TextControl, ViewCx};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::portal::{FileFilter, PickRequest};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::ActionDecl;
use ui::widget::Embedded;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::{Notice, ToastAnchor, ToastKind};
use ui::widget::{
    AudiencePresentationProvider, ColorPicker, Dropdown, FullscreenProvider, NumberInput,
    PageSidebar, PageSidebarEvent, Scrollable, SidebarDrawer, SplitLayout, TextInput, Widget,
};

use crate::actions::{self, SlidesDeck};
use crate::assets::ImageStore;

use crate::doc::SlidesDoc;
use crate::help_overlay::HelpOverlay;
use crate::model::{
    Background, Deck, Element, ElementKind, FILL_ROLES, Id, Layout, Palette, PaletteKind, SLIDE_H,
    SLIDE_W, STROKE_ROLES, Slide, TextAlign, new_id, starter_deck,
};
use crate::overview_grid::OverviewGrid;
use crate::presentation::{Blank, Command as PresentCommand, Presentation, PresentationFrame};
use crate::render::{self, DeckAssets};
use crate::strings::{tr, tr_args};

use tilo_content::ContentStore;
use ui::i18n::ArgValue;

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
    parse_presentation(&bytes, is_pptx(path), content)
}

/// The deck of a presentation file's bytes: PPTX when `pptx`, else ODP.
fn parse_presentation(bytes: &[u8], pptx: bool, content: &ContentStore) -> Result<Deck, String> {
    if bytes.len() > MAX_PRESENTATION_FILE_BYTES {
        return Err("presentation file is larger than 64 MiB".into());
    }
    if pptx {
        crate::pptx::read_pptx(bytes, content).map_err(|error| error.to_string())
    } else {
        crate::odp::read_odp(bytes, content).map_err(|error| error.to_string())
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

/// The host's chooser for a presentation to open.
fn open_document_request() -> PickRequest {
    PickRequest::open(&tr("slides-open-title"), &tr("slides-open-accept"))
        .thread_name("slides-portal-open-doc")
        .filter(presentation_filter())
        .all_files()
}

/// The host's chooser for where to save the deck, suggesting `name`.
fn save_document_request(suggested_name: &str) -> PickRequest {
    PickRequest::save(
        &tr("slides-save-title"),
        &tr("slides-save-accept"),
        suggested_name,
    )
    .thread_name("slides-portal-save-doc")
    .filter(presentation_filter())
}

/// Below this allocated width (logical px) the editor switches from the
/// docked side-by-side layout to a narrow layout where the slide rail
/// collapses into an overlay flap. The slide canvas plus both side panels
/// (rail + inspector) only fit comfortably above this width.
#[cfg(test)]
const NARROW_BREAKPOINT: f64 =
    (ui::tokens::Metrics::BASE.overview_width + ui::tokens::Metrics::BASE.content_min_width) as f64;
/// Below this allocated width the inspector also collapses to a right-edge
/// overlay flap. It intentionally matches the rail breakpoint: a compact
/// window must never sacrifice the canvas to two permanent sidebars.
#[cfg(test)]
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
    /// Toggle the toolbar's insert-tool overflow ("More") menu.
    ToggleToolbarMenu,
    OpenLayoutMenu,
}

/// Framework event state shared by the small embedded-widget routers.
///
/// Keeping this boundary explicit prevents every router from mirroring the
/// full [`Widget::handle_event`] signature and makes it clear which values are
/// forwarded unchanged to child widgets.
struct RoutedEvent<'a, 'doc> {
    event: &'a WindowEvent,
    scale_factor: f64,
    modifiers: Modifiers,
    state: &'a mut ui::widget::Edits<'doc>,
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
    rail_drawer: SidebarDrawer,
    /// Responsive inspector flap. The inspector docks on the right on a wide
    /// window; below a (narrower) breakpoint it collapses to a right-edge
    /// overlay flap so the canvas reclaims the full width. Same easing kernel
    /// as the rail.
    inspector_drawer: SidebarDrawer,
    /// Width below which the inspector collapses to an overlay flap. Lower than
    /// the rail's breakpoint so a mid-width window keeps the inspector docked
    /// (rail flap + docked inspector) and only the narrowest widths collapse
    /// both panels.
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
    toolbar_menu_open: bool,
    layout_picker: Dropdown,
    demo_open_rail: bool,
    demo_open_inspector: bool,
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
    /// Visible result of a fallible open/save/image operation. Previously those
    /// paths failed silently, leaving users unsure whether a click worked.
    io_toast: Notice,

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
    notes_preview: TextInput,
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
    /// view's dispatcher, and its pictures are in the view's content store.
    pub fn new(cx: &ViewCx<SlidesDeck>) -> Self {
        let content = cx.content().unwrap_or_else(crate::assets::app_content);
        let state = cx.live().get();
        let mut app = Self::build(cx.dispatcher(), content);
        app.adopt_resource_state(&state);
        app
    }

    fn build(dispatch: Dispatcher<SlidesDeck>, content: ContentStore) -> Self {
        let deck = starter_deck();
        let notes_slide = Rc::new(RefCell::new(String::new()));
        let located = Rc::clone(&notes_slide);
        let notes = TextControl::view_only(
            TextInput::local()
                .multiline(true)
                .with_plain_chrome()
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
            rail_drawer: Self::new_drawers().0,
            inspector_drawer: Self::new_drawers().1,
            last_flap_tick: ui::time::now(),
            flap_uninitialised: true,
            text_scale: 1.0,
            deck_rev: 0,
            images: ImageStore::new(content.clone()),
            content,
            rail_scroll: 0.0,
            overview: OverviewGrid::new(),
            toolbar_menu_open: false,
            layout_picker: Self::new_layout_picker(),
            demo_open_rail: false,
            demo_open_inspector: false,
            chrome_metrics: ChromeMetrics::fallback(),
            inspector_scroll: Scrollable::new_vertical().always_visible(),
            tooltip_text: None,
            io_toast: Notice::default(),
            ctx_menu: ContextMenu::new(),
            ctx_hits: Vec::new(),
            font_input: NumberInput::new(40.0)
                .with_range(8.0, 160.0)
                .with_step(2.0)
                .with_suffix("pt")
                .with_id("slides-font-size"),
            fill_picker: ColorPicker::new(Theme::default().tokens.colors.text_primary),
            fill_picker_open: false,
            fill_picker_needs_init: false,
            title_input: TextInput::local()
                .with_kind(ui::widget::TextInputKind::Name)
                .with_plain_chrome()
                .with_id("slides-title"),
            notes_preview: TextInput::local()
                .multiline(true)
                .with_text_role(ui::TextRole::Body)
                .with_placeholder(&tr("slides-notes-placeholder"))
                .with_id("slides-speaker-notes-preview"),
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
                self.rail_drawer.open();
            }
            // Overview scrolled to its end, proving the last slide is reachable.
            "overview_end" => {
                self.mode = Mode::Overview;
                self.overview.set_scroll(1.0e6);
            }
            "narrow" => self.rail_drawer.open(),
            "narrow_closed" => self.rail_drawer.close(),
            "inspector" => self.inspector_drawer.open(),
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
                self.open_tools_menu();
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
                self.io_toast
                    .show(tr("slides-toast-open-failed"), ToastKind::Error);
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
                self.inspector_drawer.open();
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
        self.rail_drawer.layout_for(self.rect.width())
    }

    /// Whether the rail is currently revealed (resolving the user's toggle
    /// against the layout default: docked ⇒ open, overlay ⇒ closed).
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

    /// Reconcile both the rail and inspector flaps with the current width and
    /// advance their eased reveals. On the very first resolved layout each flap
    /// is snapped to its resolved default so a narrow first frame shows the
    /// (closed) flaps with no one-frame flash. Returns the rail layout. Called
    /// once per frame before `compute_chrome`.
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
                self.io_toast
                    .show(tr("slides-toast-shared-limit"), ToastKind::Error);
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
        self.inspector_drawer.sidebar_rect()
    }

    /// The rail's painted rect, resolved from the current width + flap reveal.
    /// Mirrors the computation in [`Self::compute_chrome`] so the scroll
    /// helpers and the chrome agree.
    fn rail_rect(&self) -> Rect {
        self.rail_drawer.sidebar_rect()
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

    fn handles_of(&self, dest: Rect, s: f64, el: &Element) -> Vec<(Handle, Rect)> {
        let er = render::element_dest(dest, s, el);
        // Anchor the handles on a rect outset from the content by
        // the theme's related spacing, so each handle sits *outside* the element and
        // can never overlap the content (e.g. the first glyph of a left-anchored
        // title). Resize math keys off the pointer position, not these centres,
        // so pushing them outward doesn't change resize behaviour.
        let outset = self.chrome_metrics.s2;
        let grab_radius = self.chrome_metrics.target * 0.25;
        let hr = er.inflate(outset, outset);
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
                    Rect::new(
                        p.x - grab_radius,
                        p.y - grab_radius,
                        p.x + grab_radius,
                        p.y + grab_radius,
                    ),
                )
            })
            .collect()
    }

    // --- actions ------------------------------------------------------------

    fn apply_hit(&mut self, hit: Hit) {
        // Any action other than toggling the overflow menu itself dismisses it
        // (picking a tool from it, or any other toolbar action).
        if !matches!(hit, Hit::ToggleToolbarMenu) {
            self.toolbar_menu_open = false;
            self.ctx_menu.close();
        }
        match hit {
            Hit::ToggleToolbarMenu => self.open_tools_menu(),
            Hit::OpenLayoutMenu => self.open_layout_picker(),
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
            // Images can't be conjured from nothing — the tool opens the host
            // portal file chooser; the chosen file is inserted when it resolves.
            Hit::Insert(ElementKind::Image) => {
                self.dispatch.effect(&SlidesEffect::InsertPicture);
            }
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
        if self.rail_drawer.is_overlay() && self.inspector_drawer.is_overlay() && !self.rail_open()
        {
            self.inspector_drawer.snap(false);
        }
        self.rail_drawer.toggle();
    }

    /// Mirror [`Self::toggle_rail_flap`] for the Properties drawer. Docked
    /// panels remain independent on roomy windows; mutual exclusion applies
    /// only where both are overlays.
    fn toggle_inspector_flap(&mut self) {
        if self.inspector_drawer.is_overlay()
            && self.rail_drawer.is_overlay()
            && !self.inspector_open()
        {
            self.rail_drawer.snap(false);
        }
        self.inspector_drawer.toggle();
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

    /// Ask the person for a picture with the host's chooser (the app never
    /// draws its own picker), then keep its bytes in the content store and
    /// place it on the current slide. A cancelled choice places nothing.
    fn choose_picture(cx: &mut EffectCx<SlidesDeck, Self>) -> Effected {
        cx.choose_file(image_open_request(), |app: &mut Self, chosen, _| {
            let Some(file) = chosen else {
                return Ok(Effected::done());
            };
            let Some(src) = crate::assets::store_asset(&app.content, &file.bytes) else {
                let message = tr("slides-toast-image-import-failed");
                app.io_toast.show(message.clone(), ToastKind::Error);
                return Err(message);
            };
            app.insert_picture(&src);
            Ok(Effected::done())
        })
    }

    /// Open a presentation file in place of the deck: the file at `path`,
    /// or the one the person chooses with the host's chooser. It is read off
    /// the UI thread and lands as one action, one undo step.
    fn open_presentation(
        &self,
        path: Option<String>,
        cx: &mut EffectCx<SlidesDeck, Self>,
    ) -> Effected {
        match path {
            Some(path) => {
                let content = self.content.clone();
                cx.thread(
                    move |_| load_presentation_from_path(std::path::Path::new(&path), &content),
                    |app: &mut Self, loaded, cx| app.finish_open(loaded, cx),
                )
            }
            None => cx.choose_file(open_document_request(), |app: &mut Self, chosen, cx| {
                let Some(file) = chosen else {
                    return Ok(Effected::done());
                };
                let pptx = is_pptx(std::path::Path::new(&file.name));
                if pptx {
                    app.io_toast
                        .show(tr("slides-toast-opening-pptx"), ToastKind::Info);
                }
                let content = app.content.clone();
                Ok(cx.thread(
                    move |_| parse_presentation(&file.bytes, pptx, &content),
                    |app: &mut Self, loaded, cx| app.finish_open(loaded, cx),
                ))
            }),
        }
    }

    /// Show the outcome of opening a presentation, replacing the deck when
    /// it was read.
    fn finish_open(
        &mut self,
        loaded: Result<Deck, String>,
        cx: &EffectCx<SlidesDeck, Self>,
    ) -> EffectAnswer {
        match loaded {
            Ok(deck) => {
                cx.apply(&actions::replace_deck_action(&deck));
                self.show_opened_deck();
                self.io_toast
                    .show(tr("slides-toast-opened"), ToastKind::Success);
                Ok(Effected::done())
            }
            Err(error) => {
                self.io_toast
                    .show(tr("slides-toast-process-failed"), ToastKind::Error);
                Err(error)
            }
        }
    }

    /// Save the deck as a presentation file: at `path` (PPTX by its
    /// extension, else ODP), or as ODP where the person chooses with the
    /// host's chooser. It is written off the UI thread.
    fn save_presentation(
        &mut self,
        path: Option<String>,
        cx: &mut EffectCx<SlidesDeck, Self>,
    ) -> EffectAnswer {
        if !actions::deck_within_aggregate_limits(&self.deck) {
            let message = tr("slides-toast-export-limit");
            self.io_toast.show(message.clone(), ToastKind::Error);
            return Err(message);
        }
        let deck = self.deck.clone();
        let content = self.content.clone();
        Ok(match path {
            Some(path) => cx.thread(
                move |_| save_presentation_to_path(&deck, std::path::Path::new(&path), &content),
                |app: &mut Self, saved, _| app.finish_save(saved.map(|()| true)),
            ),
            None => {
                let name = format!("{}.odp", sanitize_filename(&deck.title));
                cx.thread(
                    move |_| crate::odp::write_odp(&deck, &content).map_err(|e| e.to_string()),
                    move |app: &mut Self, bytes, cx| match bytes {
                        Ok(bytes) => Ok(cx.save_file(
                            save_document_request(&name),
                            bytes,
                            |app: &mut Self, saved, _| app.finish_save(Ok(saved)),
                        )),
                        Err(error) => app.finish_save(Err(error)),
                    },
                )
            }
        })
    }

    /// Show the outcome of saving: saved, cancelled (nothing to say) or
    /// failed.
    fn finish_save(&mut self, saved: Result<bool, String>) -> EffectAnswer {
        match saved {
            Ok(true) => {
                self.io_toast.show("Presentation saved", ToastKind::Success);
                Ok(Effected::done())
            }
            Ok(false) => Ok(Effected::done()),
            Err(error) => {
                self.io_toast
                    .show("Could not process the presentation", ToastKind::Error);
                Err(error)
            }
        }
    }

    /// Show a deck just opened from a file from its first slide, nothing
    /// selected or being edited.
    fn show_opened_deck(&mut self) {
        self.cur_slide = 0;
        self.sel = None;
        self.editing_text = false;
        self.editing_notes = false;
        self.presentation.invalidate_cache();
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

    /// Place the picture stored as `src` on the current slide and kick off
    /// its background decode so it is ready by the time it paints.
    fn insert_picture(&mut self, src: &str) {
        let src = src.to_owned();
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
    fn sync_notes(&mut self, doc: &AutoCommit) {
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
        let chrome = self.compute_chrome();
        self.inspector_scroll
            .set_viewport_rect(chrome.inspector_body);
        self.inspector_scroll.set_content_size(
            chrome.inspector_body.width() as f32,
            chrome.inspector_content_h as f32,
        );
        if let Some(notes) = chrome.notes_box {
            let overflow = (notes.y1 - chrome.inspector_body.y1).max(0.0);
            self.inspector_scroll
                .set_offset_y(self.inspector_scroll.offset_y() + overflow as f32);
        }
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
    fn route_title_edit(&mut self, routed: &mut RoutedEvent<'_, '_>) -> bool {
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
                state: routed.state.reborrow(),
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
    fn route_notes_edit(&mut self, routed: &mut RoutedEvent<'_, '_>) -> bool {
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
                state: routed.state.reborrow(),
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
            true,
        );
        self.draw_editor_selection_chrome(scene, t, theme, chrome);
        self.draw_editor_guides(scene, t, theme, chrome);
    }

    /// The selection ring + resize handles around the selected element, or
    /// (while editing) the live text selection + caret. No-op with nothing
    /// selected.
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
        let you = theme.tokens.colors.intent.you;
        let frame = self.selection_frame(chrome.slide_dest, chrome.scale, el);
        let line = f64::from(theme.tokens.metrics.chrome_rule_px) * 1.5;
        scene.stroke(&Stroke::new(line), t, you, None, &frame);
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
        match chrome.layout_picker_rect() {
            Some(rect) => {
                if !self.layout_picker.is_open() {
                    let current = self.slide().map_or(Layout::TitleContent, |s| s.layout);
                    self.layout_picker.set_selected(current.as_str());
                }
                self.layout_picker.set_viewport(self.rect);
                self.layout_picker.apply_layout_external(rect);
            }
            None => {
                if self.layout_picker.is_open() {
                    self.layout_picker.close();
                    Widget::set_focus(&mut self.layout_picker, false);
                }
                self.layout_picker.apply_layout_external(Rect::ZERO);
            }
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
        if chrome.layout_picker_rect().is_some() {
            let local = self.inspector_theme(theme);
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                t,
                &chrome.inspector_body,
            );
            self.layout_picker.draw(scene, t, tr, &local, presence);
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
        let _ = theme;
        let rect = self.title_input_rect(chrome);
        self.title_input.apply_layout_external(rect);
        self.title_input.draw(scene, t, tr, theme, presence);
    }

    pub(super) fn draw_notes(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        nb: Rect,
    ) {
        if self.editing_notes {
            self.notes.control_mut().apply_layout_external(nb);
            self.notes
                .control_mut()
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
            Hit::OpenLayoutMenu => "slides.control.layout-menu".into(),
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
            Hit::OpenLayoutMenu => (Role::Button, tr("slides-a11y-layout"), AccessState::new()),
            Hit::ToggleToolbarMenu => (
                Role::Button,
                tr("slides-a11y-more-tools"),
                AccessState::new().expanded(self.toolbar_menu_open),
            ),
        }
    }

    fn tooltip_at(&self, point: Point) -> Option<String> {
        let chrome = self.compute_chrome();
        chrome
            .buttons
            .iter()
            .chain(chrome.overflow.iter())
            .find(|button| button.rect.contains(point))
            .and_then(|button| match button.hit {
                Hit::ToggleRail => Some(tr("slides-tip-toggle-slides")),
                Hit::ToggleInspector => Some(tr("slides-tip-toggle-properties")),
                Hit::Overview => Some(tr("slides-tip-overview")),
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
        _state: &mut ui::widget::Edits<'_>,
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

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document_ref(state, path);
        self.adopt_deferred();
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
        let node = taffy.widget_leaf(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            node_map,
            self,
        );
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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
        self.presentation
            .set_fade(theme.tokens.motion.durations.base());
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
        self.sync_flaps(Some(theme));
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
        // The help overlay draws over every mode — including Present, where it
        // must appear (and be dismissable), otherwise a `?` press would set the
        // flag, suppress the overlay, and swallow all later navigation keys.
        self.help_overlay
            .draw(scene, transform, text_renderer, theme, self.rect);
        self.io_toast.paint(
            scene,
            transform,
            text_renderer,
            theme,
            self.rect,
            ToastAnchor::BottomCenter,
        );
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
            self.rail_drawer.snap(self.rail_open());
            self.inspector_drawer.snap(self.inspector_open());
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // A restore replaces the notes' text object; the first event may
        // precede the sync that follows it.
        if self.editing_notes {
            self.sync_notes(ctx.state.doc());
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
            self.io_toast.dismiss();
        }
        let mut routed = RoutedEvent {
            event,
            scale_factor: ctx.scale_factor,
            modifiers: *ctx.modifiers,
            state: &mut ctx.state,
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
        if self.route_layout_picker(&mut routed) || self.route_context_menu(&mut routed) {
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
        use ui::a11y::{AccessNode, AccessState, Role};

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
        children.extend(self.io_toast.access_node());
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
        _state: &mut ui::widget::Edits<'_>,
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
            .chain(chrome.overflow.iter())
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
    /// Answer one of the editor's effects, from a control, a key, the
    /// palette or an agent.
    pub(crate) fn answer_effect(
        &mut self,
        effect: SlidesEffect,
        cx: &mut EffectCx<SlidesDeck, Self>,
    ) -> EffectAnswer {
        match effect {
            SlidesEffect::Editor => self.mode = Mode::Edit,
            SlidesEffect::Overview => self.mode = Mode::Overview,
            SlidesEffect::Present => self.enter_present(),
            SlidesEffect::PresentLargestExternal => {
                self.presentation_target = ui::fullscreen::FullscreenTarget::Largest;
                self.enter_present();
            }
            SlidesEffect::PresentPrimaryExternal => {
                self.presentation_target = ui::fullscreen::FullscreenTarget::PrimaryExternal;
                self.enter_present();
            }
            SlidesEffect::EndPresentation => self.exit_present(),
            SlidesEffect::OpenSlide { slide } => {
                let idx = self.slide_index(&slide)?;
                self.mode = Mode::Edit;
                self.editing_notes = false;
                self.goto_slide(idx);
                self.scroll_rail_to_current();
            }
            SlidesEffect::OpenElement { slide, element } => {
                self.reveal_element(&slide, element, false)?;
            }
            SlidesEffect::EditText { slide, element } => {
                self.reveal_element(&slide, element, true)?;
            }
            SlidesEffect::OpenNotes { slide } => {
                let idx = self.slide_index(&slide)?;
                self.mode = Mode::Edit;
                self.goto_slide(idx);
                self.begin_notes_edit();
                self.scroll_rail_to_current();
            }
            SlidesEffect::InsertPicture => return Ok(Self::choose_picture(cx)),
            SlidesEffect::Open { path } => return Ok(self.open_presentation(path, cx)),
            SlidesEffect::SaveAs { path } => return self.save_presentation(path, cx),
            SlidesEffect::Read => return Ok(Effected::value(crate::outline::of_deck(&self.deck))),
        }
        Ok(Effected::done())
    }

    fn slide_index(&self, slide: &str) -> Result<usize, String> {
        self.deck
            .index_of(slide)
            .ok_or_else(|| "slide no longer exists".to_string())
    }

    /// Show element `element` of slide `slide` selected; its words selected
    /// for editing when `edit_text`.
    fn reveal_element(
        &mut self,
        slide: &str,
        element: String,
        edit_text: bool,
    ) -> Result<(), String> {
        let idx = self.slide_index(slide)?;
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
        Ok(())
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
            self.overview
                .set_scroll(self.overview.scroll_from_thumb_drag(
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
                // Keep magnetic alignment within one themed related-spacing step
                // on screen, independent of the slide canvas zoom.
                let thr = self.chrome_metrics.s2 / s.max(0.0001);
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
                self.rail_drawer.close();
                dismissed = true;
            }
            if chrome.inspector_layout == SplitLayout::Overlay && self.inspector_open() {
                self.inspector_drawer.close();
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
    fn on_press_editor_menus(&mut self, _chrome: &Chrome, p: Point, handled: &mut bool) -> bool {
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
                cid == &id
                    && t.elapsed() < ui::gesture::multi_click_interval()
                    && cp.distance(p) < 6.0
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
    fn route_context_menu(&mut self, routed: &mut RoutedEvent<'_, '_>) -> bool {
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
                state: routed.state.reborrow(),
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
    fn route_inspector_widgets(&mut self, routed: &mut RoutedEvent<'_, '_>) -> bool {
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
    fn route_fill_picker(&mut self, routed: &mut RoutedEvent<'_, '_>) -> bool {
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
                state: routed.state.reborrow(),
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
    fn route_font_input(&mut self, routed: &mut RoutedEvent<'_, '_>) -> bool {
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
                        state: routed.state.reborrow(),
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
                            state: routed.state.reborrow(),
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
    fn route_font_input_key(&mut self, routed: &mut RoutedEvent<'_, '_>) -> bool {
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
                state: routed.state.reborrow(),
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
            MouseScrollDelta::LineDelta(_, y) => f64::from(y) * self.chrome_metrics.wheel_line,
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
            self.dispatch.effect(&SlidesEffect::SaveAs { path: None });
            *handled = true;
            return;
        }
        if ctrl && lc == "o" {
            self.dispatch.effect(&SlidesEffect::Open { path: None });
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
    fn ensure_text_editor(&mut self, doc: &AutoCommit) {
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
    fn route_text_edit(&mut self, routed: &mut RoutedEvent<'_, '_>, handled: &mut bool) -> bool {
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
        editor.host.sync(routed.state.doc(), routed.path);
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

impl SlidesApp {
    fn new_layout_picker() -> Dropdown {
        let mut picker = Dropdown::from_state(Vec::new(), &ui::LocalState::new(String::new()))
            .with_accessible_name("slides-layout")
            .with_id("slides-layout");
        picker.set_value_options(
            Layout::ALL
                .iter()
                .map(|l| (l.as_str().to_owned(), l.label().to_owned()))
                .collect(),
        );
        picker
    }
}

impl SlidesApp {
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
        let rows: Vec<_> = chrome
            .overflow
            .iter()
            .filter_map(|button| match &button.kind {
                ButtonKind::Tool { label, .. } => Some(Some((label.clone(), button.hit.clone()))),
                _ => None,
            })
            .collect();
        self.open_menu_at(Point::new(more.x0, more.y1), &rows);
        self.toolbar_menu_open = true;
    }
}

mod editor_chrome;
use editor_chrome::{ButtonKind, Chrome, ChromeMetrics};

impl SlidesApp {
    fn open_layout_picker(&mut self) {
        let chrome = self.compute_chrome();
        self.position_inspector_widgets(&chrome);
        if self.layout_picker.get_rect().width() > 0.0 {
            self.layout_picker.open_menu();
            Widget::set_focus(&mut self.layout_picker, true);
        }
    }
}

impl SlidesApp {
    fn selection_frame(&self, dest: Rect, scale: f64, el: &Element) -> Rect {
        let m = &self.chrome_metrics;
        let er = render::element_dest(dest, scale, el);
        // Room a handle needs beyond the frame line, plus a clear gap.
        let reach = m.target * 0.25 / 2.0 + m.s1;
        let (mut left, mut top, mut right, mut bottom) = (m.s2, m.s2, m.s2, m.s2);
        if let Some(slide) = self.slide() {
            for other in slide.elements.iter().filter(|other| other.id != el.id) {
                let or = render::element_dest(dest, scale, other);
                if er.intersect(or).area() > 0.0 {
                    continue;
                }
                let beside_x = or.y0 < er.y1 && or.y1 > er.y0;
                let beside_y = or.x0 < er.x1 && or.x1 > er.x0;
                let room = |gap: f64| (gap - reach).max(1.0);
                if beside_y && or.y0 >= er.y1 {
                    bottom = bottom.min(room(or.y0 - er.y1));
                }
                if beside_y && or.y1 <= er.y0 {
                    top = top.min(room(er.y0 - or.y1));
                }
                if beside_x && or.x0 >= er.x1 {
                    right = right.min(room(or.x0 - er.x1));
                }
                if beside_x && or.x1 <= er.x0 {
                    left = left.min(room(er.x0 - or.x1));
                }
            }
        }
        Rect::new(er.x0 - left, er.y0 - top, er.x1 + right, er.y1 + bottom)
    }
}

impl SlidesApp {
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
        // Over the drop-to-delete zone the zone shows the drop; the lifted
        // slide would only peek out from behind it.
        let over_zone = self.over_delete_zone();
        let mut sb = std::mem::replace(&mut self.page_sidebar, PageSidebar::new(SLIDE_H / SLIDE_W));
        sb.set_ghost_hidden(over_zone);
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
}

impl SlidesApp {
    fn route_layout_picker(&mut self, routed: &mut RoutedEvent<'_, '_>) -> bool {
        if self.mode != Mode::Edit {
            return false;
        }
        let open = self.layout_picker.is_open();
        if !open && !matches!(routed.event, WindowEvent::CursorMoved { .. }) {
            return false;
        }
        let chrome = self.compute_chrome();
        self.position_inspector_widgets(&chrome);
        if open {
            self.layout_picker.sync(routed.state.doc(), routed.path);
        }
        let before = self.layout_picker.selected().to_owned();
        let focus = *routed.focus_target;
        let mut handled = false;
        self.layout_picker.handle_event(
            routed.event,
            &mut ui::widget::EventCtx {
                scale_factor: routed.scale_factor,
                modifiers: &routed.modifiers,
                state: routed.state.reborrow(),
                path: routed.path,
                handled: &mut handled,
                focus_target: routed.focus_target,
                node_id: routed.node_id,
                taffy: routed.taffy,
            },
        );
        // The picker is driven here, not through the window's focus chain.
        *routed.focus_target = focus;
        if !open {
            return false;
        }
        if !self.layout_picker.is_open() {
            Widget::set_focus(&mut self.layout_picker, false);
        }
        let chosen = self.layout_picker.selected().to_owned();
        if chosen != before
            && let Some(layout) = Layout::ALL.iter().find(|l| l.as_str() == chosen)
        {
            self.apply_hit(Hit::SetLayout(*layout));
        }
        true
    }
}

impl SlidesApp {
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
}
