//! The Slides editor: one full-window widget that lays out and paints the
//! toolbar, the slide rail, the canvas, the inspector and present mode. It shows
//! the framework's mirror of the [`crate::deck::Deck`] record, so undo and
//! collaboration need no extra plumbing; the current slide, the selection and the
//! mode are the window's own.

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
use crate::strings::CATALOG;
use tilo_rich_text::{FieldOutcome, RichTextField, RichTextSurface, SubmitKey};
use ui::framework::{Dispatcher, Live, TextControl, ViewCx};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::portal::{FileFilter, PickRequest};
use ui::presence::PresenceCtx;
use ui::snap::{Guide, snap_rect};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Embedded;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::{Notice, ToastKind};
use ui::widget::{
    AudiencePresentationProvider, ColorPicker, Dropdown, FullscreenProvider, NumberInput,
    PageSidebar, PageSidebarEvent, Scrollable, SidebarDrawer, SplitLayout, TextInput, Widget,
};

use crate::actions;
use crate::assets::ImageStore;
use crate::deck::Deck as DeckRecord;

use crate::help_overlay::HelpOverlay;
use crate::model::{
    Background, Deck, DeckLook, Element, ElementKind, FILL_ROLES, Id, Layout, Palette, PaletteKind,
    SLIDE_H, SLIDE_W, STROKE_ROLES, Slide, TextAlign, new_id, starter_deck,
};
use crate::overview_grid::OverviewGrid;
use crate::presentation::{Blank, Command as PresentCommand, Presentation, PresentationFrame};
use crate::render::{self, DeckAssets};
use crate::strings::{tr, tr_args};

use tilo_content::ContentStore;
use ui::i18n::ArgValue;
use ui::widget::DrawChild;

/// Bounded before opening the ZIP; the readers bound members and expansion.
const MAX_PRESENTATION_FILE_BYTES: usize = 64 * 1024 * 1024;
const A11Y_SLIDE_WINDOW: usize = 64;
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

fn open_document_request() -> PickRequest {
    PickRequest::open(&tr("slides-open-title"), &tr("slides-open-accept"))
        .thread_name("slides-portal-open-doc")
        .filter(presentation_filter())
        .all_files()
}

fn save_document_request(suggested_name: &str) -> PickRequest {
    PickRequest::save(
        &tr("slides-save-title"),
        &tr("slides-save-accept"),
        suggested_name,
    )
    .thread_name("slides-portal-save-doc")
    .filter(presentation_filter())
}

/// Below this width the slide rail collapses into an overlay flap: the canvas
/// and both panels only fit above it.
#[cfg(test)]
const NARROW_BREAKPOINT: f64 =
    (ui::tokens::Metrics::BASE.overview_width + ui::tokens::Metrics::BASE.content_min_width) as f64;
/// Below this width the inspector collapses too: a compact window never gives
/// the canvas away to two sidebars.
#[cfg(test)]
const INSPECTOR_BREAKPOINT: f64 = NARROW_BREAKPOINT;
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Edit,
    Overview,
    Present,
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
    OpenSlide(usize),
    Insert(ElementKind),
    SelectSlide(usize),
    SetFill(String),
    SetStroke(String),
    OpenFillPicker,
    ToggleBold,
    ToggleItalic,
    ToggleList(bool),
    SetAlign(TextAlign),
    BringFront,
    SendBack,
    DuplicateElement,
    DeleteElement,
    SetLayout(Layout),
    SetBackground(Background),
    SetPalette(PaletteKind),
    SetLook(DeckLook),
    EditNotes,
    OpenLayoutMenu,
}

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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum SlideZoom {
    #[default]
    Fit,
    /// Screen px per slide unit; 1.0 is the design size.
    Pinned(f64),
}

impl SlideZoom {
    pub(crate) const STEPS: [f64; 8] = [0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];

    pub(crate) fn label(self) -> String {
        match self {
            Self::Fit => tr("slides-zoom-fit"),
            Self::Pinned(scale) => format!("{:.0}%", scale * 100.0),
        }
    }

    pub(crate) fn step(current: f64, forward: bool) -> Self {
        let steps = Self::STEPS;
        let found = if forward {
            steps
                .iter()
                .copied()
                .find(|step| *step > current + 0.001)
                .or_else(|| steps.last().copied())
                .unwrap_or(current)
        } else {
            steps
                .iter()
                .rev()
                .copied()
                .find(|step| *step < current - 0.001)
                .or_else(|| steps.first().copied())
                .unwrap_or(current)
        };
        Self::Pinned(found)
    }

    pub(crate) fn is_fit(self) -> bool {
        matches!(self, Self::Fit)
    }
}

impl RoutedEvent<'_, '_> {
    fn event_ctx<'b>(&'b mut self, handled: &'b mut bool) -> ui::widget::EventCtx<'b> {
        ui::widget::EventCtx {
            scale_factor: self.scale_factor,
            modifiers: &self.modifiers,
            state: self.state.reborrow(),
            path: self.path,
            handled,
            focus_target: self.focus_target,
            node_id: self.node_id,
            taffy: self.taffy,
        }
    }
}

fn notes_control(deck: &Dispatcher<DeckRecord>, slide: &str) -> TextControl<TextInput, DeckRecord> {
    TextControl::at(
        TextInput::local()
            .multiline(true)
            .with_plain_chrome()
            .with_id("slides-speaker-notes"),
        deck.clone(),
        actions::slide_paths(slide).notes,
    )
}

#[derive(Default)]
struct Opened {
    known: std::collections::HashSet<String>,
    requests: Vec<String>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across several unrelated concerns (document lifecycle, editor mode/selection state, the presentation flow, and --shots-only demo staging); grouping them would need a real design pass (e.g. per-concern sub-structs), left for a follow-up rather than forced here"
)]
pub struct SlidesApp {
    /// The deck as last presented, with a gesture in flight previewed on it.
    deck: Deck,
    focus_slide: Option<Id>,
    deferred: Option<std::rc::Rc<DeckRecord>>,
    opened: Rc<RefCell<Opened>>,
    store: store::Shared,
    live: Option<Live<DeckRecord>>,
    presented: Option<(u64, u64)>,
    rect: Rect,
    node: Option<NodeId>,
    bootstrapped: bool,

    mode: Mode,
    cur_slide: usize,
    sel: Option<Id>,
    editing_text: bool,
    editing_notes: bool,
    presentation: Presentation,
    /// Local routing policy; never sync it: collaborators do not share
    /// hardware.
    presentation_target: ui::fullscreen::FullscreenTarget,

    help_overlay: HelpOverlay,
    active_guides: Vec<Guide>,
    last_mouse: Point,
    drag: Drag,
    page_sidebar: PageSidebar,
    /// `(id, when, where)` of the last left press: a second press on the same
    /// element within the double-click window enters text edit. A single press
    /// selects and arms a move, so a selected element drags on the first grab.
    last_click: Option<(Id, ui::time::Instant, Point)>,
    text_drag: bool,
    text_editor: Option<TextBoxEditor>,
    text_edit_select_all: bool,
    text_edit_box: Rect,
    /// The press that opened the text box, and how many times: the box's editor
    /// takes it once the event is through, so the caret lands where the person
    /// pressed.
    opening_press: Option<(Point, u8)>,
    fingers_down: u8,

    dispatch: Dispatcher<DeckRecord>,
    bar: window_bar::Bar,

    rail_drawer: SidebarDrawer,
    inspector_drawer: SidebarDrawer,
    last_flap_tick: ui::time::Instant,
    flap_uninitialised: bool,
    text_scale: f32,
    /// Fitted to the pane, as it opens, or a zoom the person chose: 9 pt body
    /// copy on a fitted page cannot be read.
    slide_zoom: SlideZoom,
    slide_pan: (f64, f64),
    slide_pane: std::cell::Cell<(f64, f64)>,

    deck_rev: u64,

    images: ImageStore,
    rail_scroll: f64,
    overview: OverviewGrid,
    layout_picker: Dropdown,
    demo_open_rail: bool,
    demo_open_inspector: bool,
    chrome_metrics: ChromeMetrics,
    /// The panel's sections are laid out imperatively, so the shared
    /// [`Scrollable`] is driven by hand: `compute_chrome` shifts every item by
    /// its offset, and `draw` syncs its bar.
    inspector_scroll: Scrollable,
    io_toast: Notice,

    ctx_menu: ContextMenu,
    ctx_hits: Vec<Option<Hit>>,
    font_input: NumberInput,
    fill_picker: ColorPicker,
    fill_picker_open: bool,
    /// Each edit is a field edit of the slide's notes; `editing_notes` only owns
    /// the inspector's open state.
    notes: TextControl<TextInput, DeckRecord>,
    notes_preview: TextInput,
    notes_slide: Rc<RefCell<String>>,
    notes_caret_to_end: bool,
    fill_picker_needs_init: bool,
    demo_menu: Option<DemoMenu>,
    demo_reorder: bool,
    demo_words_menu: Option<bool>,
    demo_delete_zone: bool,

    /// It repaints once, then rests after the frame draws it.
    dirty: bool,
}

#[derive(Clone, Copy)]
enum DemoMenu {
    Element,
    Slide,
}

impl SlidesApp {
    /// Its edits go through the view's dispatcher; its pictures are in the
    /// view's content store.
    pub fn new(cx: &ViewCx<DeckRecord>) -> Self {
        let content = cx.content().unwrap_or_else(crate::assets::app_content);
        let state = cx.live().get();
        let mut app = Self::build(cx.dispatcher(), content);
        app.adopt_resource_state(&state);
        app
    }

    fn build(dispatch: Dispatcher<DeckRecord>, content: ContentStore) -> Self {
        let deck = starter_deck();
        let notes_slide = Rc::new(RefCell::new(String::new()));
        let notes = notes_control(&dispatch, "");
        let store = Rc::new(RefCell::new(store::Store::new(content.clone())));
        SlidesApp {
            dirty: true,
            focus_slide: None,
            deferred: None,
            opened: Rc::default(),
            store,
            live: None,
            presented: None,
            deck,
            rect: Rect::ZERO,
            node: None,
            bootstrapped: false,
            mode: Mode::Edit,
            cur_slide: 1,
            sel: None,
            editing_text: false,
            editing_notes: false,
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
            opening_press: None,
            fingers_down: 0,
            bar: window_bar::Bar::new(&dispatch),
            dispatch,
            rail_drawer: Self::new_drawers().0,
            inspector_drawer: Self::new_drawers().1,
            last_flap_tick: ui::time::now(),
            flap_uninitialised: true,
            text_scale: 1.0,
            slide_zoom: SlideZoom::default(),
            slide_pan: (0.0, 0.0),
            slide_pane: std::cell::Cell::new((0.0, 0.0)),
            deck_rev: 0,
            images: ImageStore::new(content),
            rail_scroll: 0.0,
            overview: OverviewGrid::new(),
            layout_picker: Self::new_layout_picker(),
            demo_open_rail: false,
            demo_open_inspector: false,
            chrome_metrics: ChromeMetrics::fallback(),
            inspector_scroll: Scrollable::new_vertical().always_visible(),
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
            notes_preview: TextInput::local()
                .multiline(true)
                .with_text_role(ui::TextRole::Body)
                .with_placeholder(tr("slides-notes-placeholder"))
                .with_id("slides-speaker-notes-preview"),
            notes,
            notes_slide,
            notes_caret_to_end: false,
            demo_menu: None,
            demo_reorder: false,
            demo_words_menu: None,
            demo_delete_zone: false,
        }
    }

    /// Stages a mode, a selection, an open menu, an edit or a drag in flight over
    /// the deck its actions opened. Flaps take their staged state at once.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        let Some(shot) = scenario else {
            return;
        };
        self.flap_uninitialised = true;
        self.stage_layout(shot);
        self.stage_content(shot);
    }

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
            "presenter" => {
                self.mode = Mode::Present;
                self.presentation.start();
                self.presentation.set_presenter_view(true);
            }
            "present_white" => {
                self.mode = Mode::Present;
                self.presentation.start();
                self.presentation.set_blank(Blank::White);
            }
            "help" => self.help_overlay.show(),
            "many" => self.rest_on_last_slide(),
            "many_rail" => {
                self.rest_on_last_slide();
                self.rail_drawer.open();
            }
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

    fn stage_content(&mut self, shot: &str) {
        match shot {
            "notes_edit" => {
                self.cur_slide = 1;
                self.sel = None;
                self.editing_notes = true;
                self.bootstrapped = true;
            }
            "slide_props" => {
                self.cur_slide = 1;
                self.sel = None;
                self.bootstrapped = true;
            }
            "image" => {
                self.rest_on_picture();
                for src in self.picture_sources() {
                    self.images.load_blocking(&src);
                }
            }
            "image_loading" => {
                self.rest_on_picture();
                for src in self.picture_sources() {
                    self.images.hold_loading(&src);
                }
            }
            "open_error" => {
                self.io_toast
                    .show(tr("slides-toast-open-failed"), ToastKind::Error);
            }
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
            "reorder" => {
                self.cur_slide = 0;
                self.sel = None;
                self.demo_reorder = true;
                self.bootstrapped = true;
            }
            "edit_text" => {
                self.select_first_text_on_second_slide();
                self.begin_text_edit(true);
                self.bootstrapped = true;
            }
            "words_menu" | "words_menu_edge" => {
                self.select_first_text_on_second_slide();
                self.begin_text_edit(false);
                self.demo_words_menu = Some(shot == "words_menu_edge");
                self.bootstrapped = true;
            }
            "delete_zone" => {
                self.cur_slide = 0;
                self.sel = None;
                self.demo_delete_zone = true;
                self.bootstrapped = true;
            }
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

    fn rail_layout(&self) -> SplitLayout {
        self.rail_drawer.layout_for(self.rect.width())
    }

    fn rail_open(&self) -> bool {
        self.rail_drawer.is_open()
    }

    fn inspector_layout(&self) -> SplitLayout {
        self.inspector_drawer.layout_for(self.rect.width())
    }

    fn inspector_open(&self) -> bool {
        self.inspector_drawer.is_open()
    }

    fn sync_flaps(&mut self, theme: Option<&Theme>) -> SplitLayout {
        self.dirty = true;
        self.resolve_drawers();
        if self.flap_uninitialised {
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

    pub(crate) fn adopt_resource_state(&mut self, state: &Rc<DeckRecord>) {
        if self.gesture_in_flight() {
            self.deferred = Some(Rc::clone(state));
            return;
        }
        self.deferred = None;
        let deck = crate::deck::mirror(state);
        if deck != self.deck {
            if !actions::deck_within_aggregate_limits(&deck) {
                self.io_toast
                    .show(tr("slides-toast-shared-limit"), ToastKind::Error);
                return;
            }
            let shown = self
                .focus_slide
                .clone()
                .or_else(|| self.slide().map(|slide| slide.id.clone()));
            self.deck = deck;
            self.opened.borrow_mut().known = self
                .deck
                .slides
                .iter()
                .map(|slide| slide.id.clone())
                .collect();
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
        if self.editing_notes {
            let slide = self.notes_slide.borrow().clone();
            if !slide.is_empty() && self.deck.index_of(&slide).is_none() {
                self.end_notes_edit();
            }
        }
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

    fn gesture_in_flight(&self) -> bool {
        self.drag.active() || self.page_sidebar.is_dragging()
    }

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

    fn palette(&self) -> Palette {
        Palette::resolve(self.deck.palette, self.deck.look)
    }

    fn submit(&self, action: &crate::actions::Action) {
        self.dispatch.apply(action);
    }

    fn submit_all(&self, actions: &[crate::actions::Action]) {
        self.dispatch.group(|| {
            for action in actions {
                self.dispatch.apply(action);
            }
        });
    }

    fn selected(&self) -> Option<store::Selected> {
        if self.mode != Mode::Edit {
            return None;
        }
        let (slide, element) = (self.slide()?, self.selected_element()?);
        Some(store::Selected {
            slide: ui::tilo_resource::Id::from(slide.id.as_str()),
            element: ui::tilo_resource::Id::from(element.id.as_str()),
            label: crate::actions::element_label(element.kind, &element.text),
        })
    }

    fn note_selection(&self) {
        self.store.borrow_mut().selected = self.selected();
    }

    fn follow_document(&mut self) {
        let Some(state) = self
            .live
            .as_ref()
            .and_then(|live| live.changed_since(&mut self.presented))
        else {
            return;
        };
        self.adopt_resource_state(&state);
    }

    fn inspector_rect(&self) -> Rect {
        self.inspector_drawer.sidebar_rect()
    }

    fn in_editor_canvas(&self) -> bool {
        if self.mode != Mode::Edit {
            return false;
        }
        let chrome = self.compute_chrome();
        let reach = self.chrome_metrics.s6;
        chrome
            .slide_dest
            .inflate(reach, reach)
            .contains(self.last_mouse)
            && !self.rail_rect().contains(self.last_mouse)
            && !(self.inspector_open() && self.inspector_rect().contains(self.last_mouse))
    }

    /// One step in or out. The fitted size is the floor, so two steps back
    /// always show the whole slide.
    pub(crate) fn zoom_slide(&mut self, direction: f64) {
        if direction == 0.0 {
            return;
        }
        let current = self.compute_chrome().scale;
        self.slide_zoom = SlideZoom::step(current, direction > 0.0);
        self.clamp_slide_pan();
    }

    pub(crate) fn zoom_slide_to_fit(&mut self) {
        self.slide_zoom = SlideZoom::Fit;
        self.slide_pan = (0.0, 0.0);
    }

    pub(crate) fn zoom_slide_to(&mut self, scale: Option<f64>) {
        match scale {
            Some(scale) => self.slide_zoom = SlideZoom::Pinned(scale),
            None => self.slide_zoom = SlideZoom::Fit,
        }
        self.clamp_slide_pan();
    }

    /// Stops where the slide's edge reaches the pane's. `false` when it cannot
    /// move, so the wheel goes to whoever is below.
    pub(crate) fn pan_slide(&mut self, dx: f64, dy: f64) -> bool {
        if self.slide_zoom.is_fit() {
            return false;
        }
        let before = self.slide_pan;
        self.slide_pan = (self.slide_pan.0 + dx, self.slide_pan.1 + dy);
        self.clamp_slide_pan();
        self.slide_pan != before
    }

    fn clamp_slide_pan(&mut self) {
        let (avail_w, avail_h) = self.slide_pane.get();
        let SlideZoom::Pinned(scale) = self.slide_zoom else {
            self.slide_pan = (0.0, 0.0);
            return;
        };
        let reach_x = ((SLIDE_W * scale - avail_w) / 2.0).max(0.0);
        let reach_y = ((SLIDE_H * scale - avail_h) / 2.0).max(0.0);
        self.slide_pan = (
            self.slide_pan.0.clamp(-reach_x, reach_x),
            self.slide_pan.1.clamp(-reach_y, reach_y),
        );
    }

    fn rail_rect(&self) -> Rect {
        self.rail_drawer.sidebar_rect()
    }

    fn rail_viewport(&self, rail: Rect) -> Rect {
        self.rail_viewport_of(rail)
    }

    fn over_delete_zone(&self) -> bool {
        self.page_sidebar.is_dragging()
            && self
                .slide_delete_zone(self.rail_rect())
                .contains(self.last_mouse)
    }

    fn sync_page_rail(&mut self) {
        self.dirty = true;
        let viewport = self.rail_viewport(self.rail_rect());
        self.page_sidebar.set_aspect(SLIDE_H / SLIDE_W);
        self.page_sidebar.set_scroll(self.rail_scroll);
        self.page_sidebar
            .begin_frame(viewport, self.deck.slides.len(), self.cur_slide);
        self.rail_scroll = self.page_sidebar.scroll();
    }

    fn scroll_rail_to_current(&mut self) {
        self.sync_page_rail();
        self.page_sidebar.scroll_to(self.cur_slide);
        self.rail_scroll = self.page_sidebar.scroll();
    }

    fn rail_thumb_rects(&mut self) -> Vec<(usize, Rect)> {
        self.sync_page_rail();
        self.page_sidebar.thumb_rects()
    }

    fn apply_rail_event(&mut self, ev: PageSidebarEvent) {
        match ev {
            PageSidebarEvent::Select(i) => self.apply_hit(Hit::SelectSlide(i)),
            PageSidebarEvent::Reorder { from, to } => {
                let Some(id) = self.deck.slides.get(from).map(|slide| slide.id.clone()) else {
                    return;
                };
                self.submit(&actions::move_slide_action(&id, to));
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

    fn apply_rail_events(&mut self) {
        for ev in self.page_sidebar.take_events() {
            self.apply_rail_event(ev);
        }
    }

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
        let er = self.element_frame(dest, s, el);
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

    fn apply_hit(&mut self, hit: Hit) {
        self.ctx_menu.close();
        match hit {
            Hit::OpenLayoutMenu => self.open_layout_picker(),
            Hit::NewSlide => self.new_slide(),
            Hit::DuplicateSlide => self.duplicate_slide(),
            Hit::DeleteSlide => self.delete_slide(),
            Hit::OpenSlide(i) => {
                self.cur_slide = i.min(self.deck.slides.len().saturating_sub(1));
                self.sel = None;
                self.mode = Mode::Edit;
            }
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
                if self.sel.is_some() {
                    self.fill_picker_open = true;
                    self.fill_picker_needs_init = true;
                }
            }
            Hit::SetStroke(role) => {
                self.set_sel_str("stroke", &role);
                if let Some(el) = self.selected_element()
                    && role != "none"
                    && el.stroke_width < 1.0
                {
                    self.set_sel_f64("stroke_width", 4.0);
                }
            }
            Hit::ToggleBold => self.toggle_mark(ui::widget::marks::Mark::Bold),
            Hit::ToggleItalic => self.toggle_mark(ui::widget::marks::Mark::Italic),
            Hit::ToggleList(ordered) => self.toggle_list(ordered),
            Hit::SetAlign(a) => self.set_sel_str("align", a.as_str()),
            Hit::BringFront => self.restack(true),
            Hit::SendBack => self.restack(false),
            Hit::DuplicateElement => self.duplicate_element(),
            Hit::DeleteElement => self.delete_element(),
            Hit::SetLayout(l) => {
                if let Some(id) = self.slide().map(|slide| slide.id.clone()) {
                    self.submit(&actions::set_slide_layout_action(&id, l));
                }
            }
            Hit::SetBackground(b) => {
                if let Some(id) = self.slide().map(|slide| slide.id.clone()) {
                    self.submit(&actions::set_slide_background_action(&id, b));
                }
            }
            Hit::SetPalette(p) => {
                self.submit(&actions::set_deck_palette_action(p));
            }
            Hit::SetLook(look) => {
                self.submit(&actions::set_deck_look_action(look));
            }
            Hit::EditNotes => self.begin_notes_edit(),
        }
    }

    fn toggle_rail_flap(&mut self) {
        if self.rail_drawer.is_overlay() && self.inspector_drawer.is_overlay() && !self.rail_open()
        {
            self.inspector_drawer.snap(false);
        }
        self.rail_drawer.toggle();
    }

    fn toggle_inspector_flap(&mut self) {
        if self.inspector_drawer.is_overlay()
            && self.rail_drawer.is_overlay()
            && !self.inspector_open()
        {
            self.rail_drawer.snap(false);
        }
        self.inspector_drawer.toggle();
    }

    fn restyle(&self, style: &actions::Style) {
        let (Some(slide), Some(element)) = (self.slide(), self.selected_element()) else {
            return;
        };
        self.submit(&actions::restyle_action(&slide.id, element, style));
    }
    fn set_sel_str(&self, key: &str, v: &str) {
        let style = match key {
            "fill" => actions::Style::Fill(v.to_owned()),
            "stroke" => actions::Style::Stroke(v.to_owned()),
            _ => actions::Style::Align(TextAlign::from_str(v)),
        };
        self.restyle(&style);
    }
    fn set_sel_f64(&self, key: &str, v: f64) {
        let style = match key {
            "stroke_width" => actions::Style::StrokeWidth(v),
            "radius" => actions::Style::Radius(v),
            _ => actions::Style::FontSize(v),
        };
        self.restyle(&style);
    }
    fn toggle_mark(&mut self, mark: ui::widget::marks::Mark) {
        if let Some(editor) = self.editing_words() {
            editor.host.widget_mut().editor_mut().toggle_mark(mark);
            return;
        }
        let Some(el) = self.selected_element() else {
            return;
        };
        let on = !mark_covers(&el.rich_blocks(), &mark);
        self.set_sel_bool(
            if mark == ui::widget::marks::Mark::Italic {
                "italic"
            } else {
                "bold"
            },
            on,
        );
    }

    fn toggle_list(&mut self, ordered: bool) {
        if let Some(editor) = self.editing_words() {
            editor.host.widget_mut().editor_mut().toggle_list(ordered);
            return;
        }
        let Some((slide, el)) = self.selected_text_box() else {
            return;
        };
        let on = !el
            .rich_blocks()
            .iter()
            .all(|block| listed(&block.kind, ordered));
        if let Some(action) = actions::list_action(&slide, &el, on.then_some(ordered)) {
            self.submit(&action);
        }
    }

    fn editing_words(&mut self) -> Option<&mut TextBoxEditor> {
        self.editing_text_box()?;
        self.text_editor.as_mut().filter(|editor| editor.focused)
    }

    fn mark_on(&self, mark: &ui::widget::marks::Mark) -> bool {
        if self.editing_text
            && let Some(editor) = self.text_editor.as_ref().filter(|editor| editor.focused)
        {
            return editor
                .host
                .widget()
                .editor()
                .active_marks()
                .iter()
                .any(|active| active.same_kind(mark));
        }
        self.selected_element()
            .is_some_and(|el| el.kind == ElementKind::Text && mark_covers(&el.rich_blocks(), mark))
    }

    fn list_on(&self, ordered: bool) -> bool {
        if self.editing_text
            && let Some(editor) = self.text_editor.as_ref().filter(|editor| editor.focused)
        {
            return editor
                .host
                .widget()
                .editor()
                .focused_kind()
                .is_some_and(|kind| listed(kind, ordered));
        }
        self.selected_element().is_some_and(|el| {
            let blocks = el.rich_blocks();
            el.kind == ElementKind::Text
                && !blocks.is_empty()
                && blocks.iter().all(|block| listed(&block.kind, ordered))
        })
    }

    fn set_sel_bool(&self, key: &str, v: bool) {
        self.restyle(&if key == "italic" {
            actions::Style::Italic(v)
        } else {
            actions::Style::Bold(v)
        });
    }

    fn restack(&self, front: bool) {
        let Some(id) = self.sel.clone() else { return };
        let Some(slide_id) = self.slide().map(|slide| slide.id.clone()) else {
            return;
        };
        self.submit(&actions::restack_action(&slide_id, &id, front));
    }

    fn new_slide(&mut self) {
        self.end_notes_edit();
        let s = actions::new_slide_template();
        let at = self.cur_slide + 1;
        self.submit(&actions::insert_slide_action(at, &s));
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
        self.end_notes_edit();
        let mut copy = slide;
        copy.id = new_id();
        for e in &mut copy.elements {
            e.id = new_id();
        }
        let at = self.cur_slide + 1;
        self.submit(&actions::insert_slide_action(at, &copy));
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
        self.end_notes_edit();
        self.submit(&actions::delete_slide_action(&id));
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
        self.submit(&actions::add_element_action(&slide_id, &el));
        self.sel = Some(id);
        if kind == ElementKind::Text {
            self.begin_text_edit(true);
        } else {
            self.editing_text = false;
        }
    }

    fn show_opened_deck(&mut self) {
        self.cur_slide = 0;
        self.sel = None;
        self.editing_text = false;
        self.editing_notes = false;
        self.presentation.invalidate_cache();
    }

    fn bump_deck_revision(&mut self) {
        if let Some(next) = self.deck_rev.checked_add(1) {
            self.deck_rev = next;
        } else {
            self.deck_rev = 0;
            self.presentation.invalidate_cache();
        }
    }

    fn insert_picture(&mut self, src: &str) {
        let src = src.to_owned();
        let _ = self.images.get(&src);
        let el = Element::image(&src, 340.0, 168.0, 600.0, 384.0);
        let id = el.id.clone();
        let Some(slide_id) = self.slide().map(|slide| slide.id.clone()) else {
            return;
        };
        self.submit(&actions::add_element_action(&slide_id, &el));
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
        self.submit(&actions::add_element_action(&slide_id, &copy));
        self.sel = Some(id);
    }

    pub(crate) fn copy_selected(&mut self, cut: bool) {
        let Some(element) = self.selected_element().cloned() else {
            return;
        };
        let clip = clipboard::Clip::of(&element);
        ui::text_selection::clipboard_copy(&clip.text());
        if cut {
            self.delete_element();
        }
        self.io_toast.show(
            tr(if cut {
                "slides-toast-cut-element"
            } else {
                "slides-toast-copied-element"
            }),
            ToastKind::Info,
        );
    }

    pub(crate) fn paste_elements(&mut self) {
        let Some(text) = ui::text_selection::clipboard_paste() else {
            return;
        };
        let Some(clip) = clipboard::Clip::read(&text) else {
            return;
        };
        let Some(slide) = self.slide().map(|slide| slide.id.clone()) else {
            return;
        };
        let mut last = None;
        for element in clip.pasted(|| uuid::Uuid::new_v4().to_string()) {
            last = Some(element.id.clone());
            self.submit(&actions::paste_element_action(&slide, &element));
        }
        self.sel = last;
        self.editing_text = false;
    }

    fn delete_element(&mut self) {
        if let (Some(id), Some(slide)) = (self.sel.clone(), self.slide().map(|s| s.id.clone())) {
            self.submit(&actions::delete_element_action(&slide, &id));
            self.sel = None;
            self.editing_text = false;
        }
    }

    fn nudge(&self, dx: f64, dy: f64) {
        if let Some(el) = self.selected_element().cloned() {
            let nx = (el.x + dx).clamp(-el.w + 20.0, SLIDE_W - 20.0);
            let ny = (el.y + dy).clamp(-el.h + 20.0, SLIDE_H - 20.0);
            if let Some(slide) = self.slide() {
                self.submit_all(&actions::move_element_actions(&slide.id, &el.id, nx, ny));
            }
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
            self.end_notes_edit();
            if self.mode == Mode::Present {
                self.presentation.start_transition(from);
            }
            self.cur_slide = i as usize;
        }
        self.sel = None;
        self.editing_text = false;
    }

    fn goto_slide(&mut self, idx: usize) {
        let n = self.deck.slides.len();
        if n == 0 {
            return;
        }
        let target = idx.min(n - 1);
        let from = self.cur_slide;
        if target != from {
            self.end_notes_edit();
            if self.mode == Mode::Present {
                self.presentation.start_transition(from);
            }
            self.cur_slide = target;
        }
        self.sel = None;
        self.editing_text = false;
    }

    fn enter_present(&mut self) {
        self.mode = Mode::Present;
        self.editing_text = false;
        self.editing_notes = false;
        self.presentation.start();
    }

    fn exit_present(&mut self) {
        self.mode = Mode::Edit;
        self.presentation.leave();
    }

    /// Present mode asks the framework for a borderless fullscreen surface,
    /// which it promotes onto the chosen output through the shared
    /// [`ui::fullscreen`] path.
    pub fn wants_fullscreen(&self) -> bool {
        self.mode == Mode::Present
    }

    fn sync_notes(&mut self, doc: &AutoCommit) {
        self.dirty = true;
        let Some(slide_id) = self.slide().map(|slide| slide.id.clone()) else {
            self.end_notes_edit();
            return;
        };
        let rebound = *self.notes_slide.borrow() != slide_id;
        if rebound {
            self.notes = notes_control(&self.dispatch, &slide_id);
            *self.notes_slide.borrow_mut() = slide_id;
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
        if let Some(notes) = chrome.notes_box
            && chrome.inspector.contains(notes.center())
        {
            let overflow = (notes.y1 - chrome.inspector_body.y1).max(0.0);
            self.inspector_scroll
                .set_offset_y(self.inspector_scroll.offset_y() + overflow as f32);
        }
    }

    fn end_notes_edit(&mut self) {
        self.editing_notes = false;
        Widget::set_focus(self.notes.control_mut(), false);
    }

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
        self.notes
            .handle_event(routed.event, &mut routed.event_ctx(&mut input_handled));
        input_handled
    }

    fn draw_editor_canvas(&self, ctx: &mut PaintCtx<'_>, chrome: &Chrome, assets: DeckAssets<'_>) {
        let Some(slide) = self.slide() else { return };
        let radius = f64::from(ctx.theme.tokens.radii.control);
        let editing = self
            .text_editor
            .as_ref()
            .filter(|_| self.editing_text)
            .map(|editor| editor.element_id.as_str());
        render::draw_slide_except(ctx, assets, slide, chrome.slide_dest, radius, editing, true);
        self.draw_editor_selection_chrome(ctx.scene, ctx.transform, ctx.theme, chrome);
        self.draw_editor_guides(ctx.scene, ctx.transform, ctx.theme, chrome);
    }

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

    fn draw_editor_guides(&self, scene: &mut Scene, t: Affine, theme: &Theme, chrome: &Chrome) {
        let you = theme.tokens.colors.intent.you;
        let dest = chrome.slide_dest;
        let line = Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px));
        for g in &self.active_guides {
            let at = g.at * chrome.scale;
            let segment = if g.vertical {
                let x = dest.x0 + at;
                Line::new(Point::new(x, dest.y0), Point::new(x, dest.y1))
            } else {
                let y = dest.y0 + at;
                Line::new(Point::new(dest.x0, y), Point::new(dest.x1, y))
            };
            scene.stroke(&line, t, you, None, &segment);
        }
    }

    fn position_inspector_widgets(&mut self, chrome: &Chrome) {
        if let Some(rect) = chrome.font_input_rect {
            if !self.font_input.is_editing() {
                let fs = self.selected_element().map_or(40.0, |e| e.font_size);
                self.font_input.set_value(fs);
            }
            self.font_input.apply_layout_external(rect);
        } else {
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
            let local = self.inspector_theme(theme);
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                t,
                &chrome.inspector_body,
            );
            self.font_input.draw_child(scene, t, tr, &local, presence);
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
            self.layout_picker
                .draw_child(scene, t, tr, &local, presence);
            scene.pop_layer();
        }
        if let Some(rect) = chrome.fill_picker_rect {
            if self.fill_picker_needs_init {
                let mut pal = self.palette();
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
            self.fill_picker.draw_child(scene, t, tr, &local, presence);
        }
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
                .draw_child(scene, t, tr, theme, &PresenceCtx::empty());
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
            .draw_child(scene, t, tr, theme, &PresenceCtx::empty());
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

    fn compute_overview(&self) -> (Rect, Vec<(usize, Rect)>, f64) {
        self.overview.geometry(self.rect, self.deck.slides.len())
    }

    fn draw_overview(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let palette = self.palette();
        self.overview.draw(
            &mut PaintCtx::new(scene, t, tr, theme),
            self.rect,
            &self.deck,
            self.cur_slide,
            Some(self.last_mouse),
            DeckAssets {
                palette: &palette,
                images: &self.images,
                presence: None,
            },
        );
    }
}

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
            Hit::ToggleList(false) => "slides.control.bulleted-list".into(),
            Hit::ToggleList(true) => "slides.control.numbered-list".into(),
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
            Hit::SetLook(look) => format!("slides.control.look:{}", look.as_str()),
            Hit::EditNotes => "slides.control.edit-notes".into(),
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
                AccessState::new().checked(self.mark_on(&ui::widget::marks::Mark::Bold)),
            ),
            Hit::ToggleItalic => (
                Role::ToggleButton,
                tr("slides-a11y-italic"),
                AccessState::new().checked(self.mark_on(&ui::widget::marks::Mark::Italic)),
            ),
            Hit::ToggleList(ordered) => (
                Role::ToggleButton,
                tr(if *ordered {
                    "slides-a11y-numbered-list"
                } else {
                    "slides-a11y-bulleted-list"
                }),
                AccessState::new().checked(self.list_on(*ordered)),
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
            Hit::SetLook(look) => (
                Role::RadioButton,
                tr_args(
                    "slides-a11y-deck-look",
                    &[("name", ArgValue::from(look.label()))],
                ),
                AccessState::new().checked(self.deck.look == *look),
            ),
            Hit::EditNotes => (
                Role::Button,
                tr("slides-a11y-edit-notes"),
                AccessState::new(),
            ),
            Hit::OpenLayoutMenu => (
                Role::Button,
                tr_args(
                    "slides-a11y-layout-menu",
                    &[(
                        "name",
                        ArgValue::from(
                            self.slide()
                                .map_or_else(String::new, |slide| slide.layout.label().to_string()),
                        ),
                    )],
                ),
                AccessState::new(),
            ),
        }
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
        dyn ui::presence::capability::PublishesPresence
    );

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.dirty = true;
        let (state, _) = ui::doc::document_ref(state, path);
        self.follow_document();
        let asks = std::mem::take(&mut self.store.borrow_mut().asks);
        for ask in asks {
            self.take_ask(ask);
        }
        self.io_toast.expire();
        self.io_toast.publish();
        self.adopt_deferred();
        let opened = std::mem::take(&mut self.opened.borrow_mut().requests);
        for slide in opened {
            if let Ok(index) = self.slide_index(&slide) {
                self.mode = Mode::Edit;
                self.editing_notes = false;
                self.goto_slide(index);
                self.scroll_rail_to_current();
            }
        }
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
        self.note_selection();
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.fill_leaf(node_map, self);
        self.node = Some(node);
        node
    }

    ui::rect_geometry!();

    fn layout_needs_measure(&self) -> bool {
        false
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
        if self.images.pump() {
            self.presentation.invalidate_cache();
        }
        self.presentation.expire_transition(theme.reduce_motion);
        self.refresh_chrome_metrics(text_renderer, theme);
        let now = ui::time::now();
        let dt = (now - self.last_flap_tick).as_secs_f64().min(0.1);
        self.last_flap_tick = now;
        self.sync_flaps(Some(theme));
        self.sync_scroll_offsets(dt);
        self.stage_shots_demo();
        match self.mode {
            Mode::Edit => {
                let chrome = self.compute_chrome();
                self.inspector_scroll
                    .set_viewport_rect(chrome.inspector_body);
                self.inspector_scroll.set_content_size(
                    chrome.inspector_body.width() as f32,
                    chrome.inspector_content_h as f32,
                );
                self.draw_editor(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    &chrome,
                    presence,
                );
                self.draw_inspector_widgets(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                    &chrome,
                );
            }
            Mode::Overview => self.draw_overview(scene, transform, text_renderer, theme),
            Mode::Present => self.draw_present(scene, transform, text_renderer, theme),
        }
        self.help_overlay
            .draw(scene, transform, text_renderer, theme, self.rect);
        self.draw_words_overlay(scene, transform, text_renderer, theme, presence);
        self.ctx_menu
            .draw_child(scene, transform, text_renderer, theme, presence);
        self.note_selection();
    }

    fn needs_animation_frame(&self) -> bool {
        // A frame is asked for only while something moves: a cross-fade, a flap
        // or a decoding image. A settled presentation is static, and the speaker
        // clock reads wall time on each paint.
        self.presentation.needs_animation_frame()
            || self.rail_drawer.is_animating()
            || self.inspector_drawer.is_animating()
            || self.images.any_loading()
            || self.page_sidebar.is_dragging()
            || self
                .text_editor
                .as_ref()
                .is_some_and(|editor| self.editing_text && editor.host.needs_animation_frame())
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        if reduced {
            self.presentation.expire_transition(true);
            self.rail_drawer.snap(self.rail_open());
            self.inspector_drawer.snap(self.inspector_open());
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.dirty = true;
        self.opening_press = None;
        if let WindowEvent::Touch(touch) = event {
            use winit::event::TouchPhase;
            match touch.phase {
                TouchPhase::Started => self.fingers_down = self.fingers_down.saturating_add(1),
                TouchPhase::Ended | TouchPhase::Cancelled => {
                    self.fingers_down = self.fingers_down.saturating_sub(1);
                }
                TouchPhase::Moved => {}
            }
        }
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
        if self.words_take_menu_event(routed.event) {
            self.route_text_edit(&mut routed, ctx.handled);
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } = routed.event
        {
            self.on_right_press(self.last_mouse, ctx.handled);
            return;
        }
        if self.route_layout_picker(&mut routed) || self.route_context_menu(&mut routed) {
            *ctx.handled = true;
            return;
        }
        if self.route_inspector_widgets(&mut routed) {
            *ctx.handled = true;
            return;
        }
        self.begin_typing_into_selection(routed.event, routed.modifiers);
        if self.route_text_edit(&mut routed, ctx.handled) {
            return;
        }
        self.dispatch_unmodal_event(
            routed.event,
            routed.scale_factor,
            routed.modifiers,
            ctx.handled,
        );
        self.open_at_press(&mut routed);
    }

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "SlidesApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("slides-editor")
    }

    /// Slides paints one retained canvas, so each visible control and durable
    /// object is a synthetic semantic child on the same `Hit` values and stable
    /// ids, which keeps assistive activation from drifting after a reorder.
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
                    .with_name(crate::strings::tr("slides-a11y-shortcuts"))
                    .with_description(crate::strings::tr("slides-a11y-shortcuts-reference"))
                    .with_state(AccessState::new().modal(true))
                    .with_children(vec![
                        AccessNode::new(Role::List)
                            .with_name(crate::strings::tr("slides-a11y-shortcuts-heading"))
                            .with_children(shortcuts),
                        AccessNode::new(Role::Button)
                            .with_name(crate::strings::tr("slides-a11y-close-shortcuts"))
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
                .unwrap_or_else(|| {
                    CATALOG.t_args(
                        "slides-slide-number",
                        &[(
                            "number",
                            ui::i18n::ArgValue::Number((self.cur_slide + 1) as f64),
                        )],
                    )
                });
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
                    .with_name(CATALOG.t_args(
                        "slides-a11y-presenting",
                        &[("deck", ArgValue::Text(self.deck.title.clone()))],
                    ))
                    .with_value(format!("Slide {} of {count}: {title}", self.cur_slide + 1))
                    .with_description(CATALOG.t_args(
                        "slides-a11y-elapsed",
                        &[("time", ArgValue::Text(self.presentation.clock()))],
                    ))
                    .with_state(AccessState::new().focusable(true))
                    .with_children(vec![
                        AccessNode::new(Role::Toolbar)
                            .with_name(crate::strings::tr("slides-a11y-presentation-controls"))
                            .with_children(controls),
                    ]),
            );
        }

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
                    .unwrap_or_else(|| {
                        CATALOG.t_args(
                            "slides-slide-number",
                            &[("number", ui::i18n::ArgValue::Number((index + 1) as f64))],
                        )
                    });
                AccessNode::new(Role::Button)
                    .with_name(title)
                    .with_description(CATALOG.t_args(
                        "slides-a11y-slide-of",
                        &[
                            ("slide", ArgValue::Text((index + 1).to_string())),
                            ("count", ArgValue::Text(count.to_string())),
                        ],
                    ))
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
                    .with_children(self.words_access_node(&element.id).into_iter().collect())
                })
                .collect()
        });

        let children = vec![
            AccessNode::new(Role::Toolbar)
                .with_name(crate::strings::tr("slides-a11y-controls"))
                .with_children(controls),
            AccessNode::new(Role::List)
                .with_name(crate::strings::tr("slides-title"))
                .with_description(CATALOG.t_args(
                    "slides-a11y-slide-window",
                    &[
                        ("first", ArgValue::Text((start + 1).to_string())),
                        (
                            "last",
                            ArgValue::Text((start + A11Y_SLIDE_WINDOW).min(count).to_string()),
                        ),
                        ("count", ArgValue::Text(count.to_string())),
                    ],
                ))
                .with_children(slides),
            AccessNode::new(Role::Canvas)
                .with_name(CATALOG.t_args(
                    "slides-a11y-slide-canvas",
                    &[("slide", ArgValue::Text((self.cur_slide + 1).to_string()))],
                ))
                .with_children(elements),
        ];
        Some(
            AccessNode::new(Role::Document)
                .with_name(self.deck.title.clone())
                .with_value(format!("Slide {} of {count}", self.cur_slide + 1))
                .with_description(crate::strings::tr("slides-a11y-editor"))
                .with_state(AccessState::new().focusable(true))
                .with_children(children),
        )
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let words = ui::a11y::hash_key(&WORDS_ACCESS_KEY);
        let Some(at) = key_path.iter().position(|&key| key == words) else {
            return false;
        };
        let Some(editor) = self.editing_words() else {
            return false;
        };
        let rest = key_path.get(at + 1..).unwrap_or_default();
        editor.host.perform_action(action, rest, state, path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if !target.starts_with("slides.")
            && let Some(editor) = self.editing_words()
            && editor
                .host
                .perform_semantic_action(target, action, state, path)
        {
            return true;
        }
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
            .find(|button| self.semantic_target(&button.hit) == target)
            .map(|button| button.hit.clone());
        if let Some(hit) = hit {
            self.apply_hit(hit);
            true
        } else {
            false
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.editing_notes {
            return self.notes.control().cursor_shape(point);
        }
        if self.editing_text
            && self.text_edit_box.contains(point)
            && let Some(editor) = &self.text_editor
        {
            return editor
                .host
                .cursor_shape(point)
                .or(Some(winit::window::CursorIcon::Text));
        }
        None
    }

    /// A finger's drag on the canvas moves, resizes or selects as the mouse's
    /// does; one held on a slide in the list picks it up. Elsewhere a drag
    /// scrolls.
    fn touch_claim(&self, point: Point) -> ui::TouchClaim {
        if self.canvas_takes_finger(point) {
            ui::TouchClaim::Drag
        } else if self.rail_takes_held_finger(point) {
            ui::TouchClaim::HoldDrag
        } else {
            ui::TouchClaim::Pointer
        }
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        self.run_window_toolbar();
        if self.editing_text
            && let Some(editor) = self.text_editor.as_mut()
        {
            editor.host.settle(edits, path);
        }
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.text_editor
            .as_ref()
            .filter(|_| self.editing_text)
            .and_then(|editor| editor.host.next_wake_deadline())
            .into_iter()
            .chain(self.io_toast.deadline())
            .min()
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

    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        if self.editing_text
            && let Some(editor) = &self.text_editor
        {
            return editor
                .host
                .ime_content()
                .or(Some(ui::ime::InputContent::TEXT));
        }
        self.editing_notes
            .then(|| self.notes.control().ime_content())
            .flatten()
    }

    fn ime_multiline(&self) -> bool {
        if self.editing_text
            && let Some(editor) = &self.text_editor
        {
            return editor.host.ime_multiline();
        }
        self.editing_notes && self.notes.control().ime_multiline()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag.active() || self.page_sidebar.is_pending_drag()
    }
}

impl SlidesApp {
    fn take_ask(&mut self, ask: store::Ask) {
        match ask {
            store::Ask::Show(effect) => {
                if let Err(error) = self.show(effect) {
                    tracing::debug!(%error, "slides: what an effect named went away");
                }
            }
            store::Ask::InsertPicture(src) => self.insert_picture(&src),
            store::Ask::ShowOpenedDeck => self.show_opened_deck(),
            store::Ask::Say(message, kind) => self.io_toast.show(message, kind),
            store::Ask::Stage(scenario) => self.stage_scenario(scenario.as_deref()),
        }
    }

    fn show(&mut self, effect: SlidesEffect) -> Result<(), String> {
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
            SlidesEffect::ShowSlideList { shown } => {
                if shown {
                    self.mode = Mode::Edit;
                }
                if self.rail_open() != shown {
                    self.toggle_rail_flap();
                }
            }
            SlidesEffect::ShowProperties { shown } => {
                if shown {
                    self.mode = Mode::Edit;
                }
                if self.inspector_open() != shown {
                    self.toggle_inspector_flap();
                }
            }
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
            SlidesEffect::InsertPicture
            | SlidesEffect::OpenPresentation { .. }
            | SlidesEffect::SaveAs { .. }
            | SlidesEffect::ReadDeck
            | SlidesEffect::ImportPptxContent { .. }
            | SlidesEffect::ExportPptxContent => {}
        }
        Ok(())
    }

    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&ui::tilo_resource::Id) -> Result<(), ui::tilo_resource::Refusal> + 'static
    {
        let opened = Rc::clone(&self.opened);
        move |id| {
            let mut opened = opened.borrow_mut();
            if !opened.known.contains(id.as_str()) {
                return Err(ui::tilo_resource::Refusal::new("there is no such slide"));
            }
            opened.requests.push(id.to_string());
            Ok(())
        }
    }

    fn slide_index(&self, slide: &str) -> Result<usize, String> {
        self.deck
            .index_of(slide)
            .ok_or_else(|| "slide no longer exists".to_string())
    }

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
            self.begin_text_edit(true);
        }
        self.scroll_rail_to_current();
        Ok(())
    }
}

impl SlidesApp {
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
                if self.page_sidebar.is_pending_drag() || self.page_sidebar.menu_open() {
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
                self.on_wheel(*delta, scale_factor, modifiers.state(), handled);
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

    fn sync_scroll_offsets(&mut self, dt: f64) {
        self.dirty = true;
        match self.mode {
            Mode::Edit => {
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
                self.sync_page_rail();
            }
            Mode::Overview => {
                self.overview.clamp(self.rect, self.deck.slides.len());
            }
            Mode::Present => {}
        }
    }

    fn stage_shots_demo(&mut self) {
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
        if let Some(at_end) = self.demo_words_menu
            && let Some(editor) = self.text_editor.as_mut()
            && editor.words_h.is_some()
        {
            let surface = editor.host.widget_mut();
            if let Some(line) = surface
                .editor()
                .blocks()
                .first()
                .map(|block| block.line.get_rect())
            {
                let x = if at_end {
                    line.x1 - 8.0
                } else {
                    line.x0 + 24.0
                };
                surface
                    .editor_mut()
                    .open_text_menu(Point::new(x, line.center().y));
            }
            self.demo_words_menu = None;
        }
        if std::mem::take(&mut self.demo_reorder) {
            let thumbs = self.rail_thumb_rects();
            if let (Some(first), Some(third)) = (thumbs.first(), thumbs.get(2)) {
                let start = first.1.center();
                let target = third.1.center();
                self.last_mouse = target;
                self.page_sidebar.on_press(start);
                self.page_sidebar.on_motion(target);
                let _ = self.page_sidebar.take_events();
            }
        }
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

    #[cfg(test)]
    pub(crate) fn picture_ready(&self, src: &str) -> bool {
        matches!(self.images.get(src), crate::assets::ImageState::Ready(_))
    }

    pub fn current_slide(&self) -> usize {
        self.cur_slide
    }

    fn snapped_move(
        &self,
        id: &str,
        nx: f64,
        ny: f64,
        w: f64,
        h: f64,
        thr: f64,
    ) -> (f64, f64, Vec<Guide>) {
        let mut targets = vec![
            Rect::new(0.0, 0.0, SLIDE_W, SLIDE_H),
            Rect::new(96.0, 0.0, 96.0, SLIDE_H),
            Rect::new(SLIDE_W - 96.0, 0.0, SLIDE_W - 96.0, SLIDE_H),
            Rect::new(0.0, 70.0, SLIDE_W, 70.0),
            Rect::new(0.0, SLIDE_H - 60.0, SLIDE_W, SLIDE_H - 60.0),
        ];
        if let Some(slide) = self.slide() {
            targets.extend(
                slide
                    .elements
                    .iter()
                    .filter(|e| e.id != id)
                    .map(|e| Rect::new(e.x, e.y, e.x + e.w, e.y + e.h)),
            );
        }
        let snap = snap_rect(Rect::new(nx, ny, nx + w, ny + h), &targets, thr);
        (nx + snap.offset.x, ny + snap.offset.y, snap.guides)
    }

    fn drag_move(&mut self, p: Point) {
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
            _ => {}
        }
    }

    fn on_press(&mut self, p: Point, handled: &mut bool) {
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

    fn on_press_editor(&mut self, p: Point, handled: &mut bool) {
        let chrome = self.compute_chrome();

        if self.on_press_editor_menus(&chrome, p, handled) {
            return;
        }

        for b in &chrome.buttons {
            if b.rect.contains(p) {
                let hit = b.hit.clone();
                self.apply_hit(hit);
                *handled = true;
                return;
            }
        }
        if chrome.scrim.contains(p) && !chrome.rail.contains(p) && !chrome.inspector.contains(p) {
            let rail = chrome.layout == SplitLayout::Overlay && self.rail_open();
            let inspector =
                chrome.inspector_layout == SplitLayout::Overlay && self.inspector_open();
            if rail {
                self.rail_drawer.close();
            }
            if inspector {
                self.inspector_drawer.close();
            }
            if rail || inspector {
                *handled = true;
                return;
            }
        }
        // The rail owns selection and reordering; a narrow window reveals the
        // chosen slide on release, so a drag does not close the flap.
        if chrome.rail_viewport.contains(p) {
            self.sync_page_rail();
            self.page_sidebar.on_press(p);
            self.apply_rail_events();
            self.rail_scroll = self.page_sidebar.scroll();
            *handled = true;
            return;
        }
        if chrome.slide_dest.inflate(12.0, 12.0).contains(p) {
            self.press_canvas(p, &chrome, handled);
            return;
        }
        if self.sel.is_some() || self.editing_text || self.editing_notes {
            self.sel = None;
            self.editing_text = false;
            self.editing_notes = false;
            *handled = true;
        }
    }

    fn on_press_editor_menus(&mut self, _chrome: &Chrome, p: Point, handled: &mut bool) -> bool {
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
        if let Some(el) = self.selected_element().cloned()
            && !self.editing_text
        {
            for (h, hr) in self.handles_of(dest, s, &el) {
                if hr.contains(p) {
                    let shown_h = self.element_frame(dest, s, &el).height() / s.max(0.0001);
                    self.drag = Drag::Resize {
                        id: el.id.clone(),
                        handle: h,
                        start: self.to_slide(dest, s, p),
                        orig: (el.x, el.y, el.w, shown_h),
                    };
                    *handled = true;
                    return;
                }
            }
        }
        let mut picked: Option<Id> = None;
        if let Some(slide) = self.slide() {
            let mut z: Vec<&Element> = slide.elements.iter().collect();
            z.sort_by(|a, b| b.z.cmp(&a.z).then_with(|| b.id.cmp(&a.id)));
            for el in z {
                let er = self.element_frame(dest, s, el);
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
            let slop = if self.fingers_down > 0 {
                ui::gesture::TouchGestureArena::TAP_SLOP
            } else {
                6.0
            };
            let double = self.last_click.as_ref().is_some_and(|(cid, t, cp)| {
                cid == &id
                    && t.elapsed() < ui::gesture::multi_click_interval()
                    && cp.distance(p) < slop
            });
            self.last_click = Some((id.clone(), ui::time::now(), p));
            self.editing_notes = false;

            if self.editing_text && self.sel.as_deref() == Some(id.as_str()) {
                *handled = true;
                return;
            }

            let blank = self
                .slide()
                .and_then(|sl| sl.element(&id))
                .is_some_and(Element::is_blank_text);
            self.sel = Some(id.clone());
            if is_text && (double || blank) {
                // A double-click edits the text and selects the word under the
                // point; an empty box opens on the first click.
                self.begin_text_edit(false);
                self.opening_press = Some((p, if double { 2 } else { 1 }));
            } else {
                // Select and arm a move, so a selected element drags on the
                // first grab.
                self.editing_text = false;
                if let Some(el) = self.selected_element().cloned() {
                    self.drag = Drag::Move {
                        id,
                        start: self.to_slide(dest, s, p),
                        orig_x: el.x,
                        orig_y: el.y,
                    };
                }
            }
        } else {
            self.sel = None;
            self.editing_text = false;
            self.last_click = None;
        }
        *handled = true;
    }

    fn element_at(&self, chrome: &Chrome, p: Point) -> Option<Id> {
        let dest = chrome.slide_dest;
        let s = chrome.scale;
        let slide = self.slide()?;
        let mut z: Vec<&Element> = slide.elements.iter().collect();
        z.sort_by(|a, b| b.z.cmp(&a.z).then_with(|| b.id.cmp(&a.id)));
        for el in z {
            if self
                .element_frame(dest, s, el)
                .inflate(2.0, 2.0)
                .contains(p)
            {
                return Some(el.id.clone());
            }
        }
        None
    }

    fn on_right_press(&mut self, p: Point, handled: &mut bool) {
        *handled = true;
        self.ctx_menu.close();
        self.ctx_hits.clear();
        if self.mode != Mode::Edit {
            return;
        }
        let chrome = self.compute_chrome();
        if chrome.rail_viewport.contains(p) {
            self.sync_page_rail();
            if self.page_sidebar.on_right_press(p) {
                self.apply_rail_events();
                self.rail_scroll = self.page_sidebar.scroll();
                return;
            }
        }
        if chrome.slide_dest.contains(p) {
            self.page_sidebar.on_escape();
            if let Some(id) = self.element_at(&chrome, p) {
                self.sel = Some(id);
                self.editing_text = false;
                self.editing_notes = false;
                self.open_element_menu(p);
            }
        }
    }

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
        self.ctx_menu
            .handle_event(routed.event, &mut routed.event_ctx(&mut menu_handled));
        if let Some(idx) = self.ctx_menu.take_selection()
            && let Some(Some(hit)) = self.ctx_hits.get(idx).cloned()
        {
            self.apply_hit(hit);
        }
        true
    }

    /// Forwards events to the inspector's widgets and folds any change into the
    /// deck; `true` when consumed. The picker is modal while open; the font
    /// field claims only its own presses and keystrokes.
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
        self.fill_picker
            .handle_event(routed.event, &mut routed.event_ctx(&mut h));
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

    fn route_font_input(&mut self, routed: &mut RoutedEvent<'_, '_>) -> bool {
        match routed.event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(
                    position.x / routed.scale_factor,
                    position.y / routed.scale_factor,
                );
                let mut h = false;
                self.font_input
                    .handle_event(routed.event, &mut routed.event_ctx(&mut h));
                if let Some(v) = self.font_input.take_changed() {
                    self.set_sel_f64("font_size", v);
                }
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.font_input.get_rect().contains(self.last_mouse) {
                    let mut h = false;
                    self.font_input
                        .handle_event(routed.event, &mut routed.event_ctx(&mut h));
                    Widget::set_focus(&mut self.font_input, true);
                    if let Some(v) = self.font_input.take_changed() {
                        self.set_sel_f64("font_size", v);
                    }
                    true
                } else if self.font_input.is_focused() {
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
        self.font_input
            .handle_event(routed.event, &mut routed.event_ctx(&mut h));
        if let Some(v) = self.font_input.take_changed() {
            self.set_sel_f64("font_size", v);
        }
        true
    }

    fn on_release(&mut self, handled: &mut bool) {
        // The rail commits the move, or — when the press never crossed the drag
        // threshold — nothing, and a narrow window reveals the chosen slide.
        if self.page_sidebar.is_pending_drag() {
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
            if let Some(slide) = self.slide() {
                self.submit_all(&actions::resize_element_actions(
                    &slide.id,
                    &id,
                    (el.x, el.y, el.w, el.h),
                ));
            }
            *handled = true;
        }
    }

    fn on_wheel(
        &mut self,
        delta: MouseScrollDelta,
        scale_factor: f64,
        modifiers: ui::winit::keyboard::ModifiersState,
        handled: &mut bool,
    ) {
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
                    self.sync_page_rail();
                    if self.page_sidebar.on_wheel(-dy) {
                        self.rail_scroll = self.page_sidebar.scroll();
                        *handled = true;
                    }
                } else if self.in_editor_canvas() {
                    // The wheel moves a zoomed slide; with the command modifier
                    // it zooms one step.
                    if modifiers.control_key() || modifiers.super_key() {
                        self.zoom_slide(if dy > 0.0 { 1.0 } else { -1.0 });
                        *handled = true;
                    } else if self.pan_slide(0.0, dy) {
                        *handled = true;
                    }
                } else if self.inspector_open() && self.inspector_rect().contains(self.last_mouse) {
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

        // Escape aborts a reorder drag (the deck changes only on a committed
        // drop) or closes the rail's menu.
        if matches!(key, Key::Named(NamedKey::Escape))
            && (self.page_sidebar.is_pending_drag() || self.page_sidebar.menu_open())
        {
            self.page_sidebar.on_escape();
            self.rail_scroll = self.page_sidebar.scroll();
            *handled = true;
            return;
        }

        let allow_help = !self.editing_text && !self.editing_notes;
        if self.help_overlay.on_key(key, allow_help) {
            *handled = true;
            return;
        }

        if self.mode == Mode::Edit
            && !self.editing_text
            && !self.editing_notes
            && matches!(key, Key::Named(NamedKey::Escape))
        {
            let rail = self.rail_layout() == SplitLayout::Overlay && self.rail_open();
            let inspector =
                self.inspector_layout() == SplitLayout::Overlay && self.inspector_open();
            if rail {
                self.rail_drawer.close();
            }
            if inspector {
                self.inspector_drawer.close();
            }
            if rail || inspector {
                *handled = true;
                return;
            }
        }

        if self.mode == Mode::Overview {
            self.on_key_overview(key, handled);
            return;
        }

        if self.mode == Mode::Present {
            self.on_key_present(key, handled);
            return;
        }

        if self.editing_text || self.editing_notes {
            self.on_key_editing_text(key, handled);
            return;
        }

        self.on_key_editor_commands(key, mods, ctrl, handled);
    }

    fn on_key_overview(&mut self, key: &Key, handled: &mut bool) {
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
            Key::Character(c) if c.eq_ignore_ascii_case("p") => {
                self.enter_present();
                *handled = true;
            }
            _ => {}
        }
    }

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

    fn on_key_editing_text(&mut self, key: &Key, handled: &mut bool) {
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.end_text_edit();
            self.editing_notes = false;
            *handled = true;
        }
    }

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

    /// Single-letter and Ctrl+letter editor shortcuts.
    fn on_key_editor_shortcut(
        &mut self,
        c: &winit::keyboard::SmolStr,
        ctrl: bool,
        handled: &mut bool,
    ) {
        let lc = c.to_lowercase();
        if ctrl && lc == "s" {
            self.dispatch.effect(&SlidesEffect::SaveAs { path: None });
            *handled = true;
            return;
        }
        if ctrl && lc == "o" {
            self.dispatch
                .effect(&SlidesEffect::OpenPresentation { path: None });
            *handled = true;
            return;
        }
        if ctrl && lc == "d" {
            self.duplicate_element();
            *handled = true;
            return;
        }
        if ctrl && matches!(lc.as_str(), "=" | "+" | "-" | "0") {
            match lc.as_str() {
                "=" | "+" => self.zoom_slide(1.0),
                "-" => self.zoom_slide(-1.0),
                _ => self.zoom_slide_to_fit(),
            }
            *handled = true;
            return;
        }
        if ctrl && matches!(lc.as_str(), "c" | "x" | "v") {
            match lc.as_str() {
                "c" => self.copy_selected(false),
                "x" => self.copy_selected(true),
                _ => self.paste_elements(),
            }
            *handled = true;
            return;
        }
        if ctrl {
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

fn custom_colour_hex(hex: &str) -> String {
    tr_args("slides-custom-colour-hex", &[("hex", ArgValue::from(hex))])
}

#[path = "app/content_files.rs"]
mod content_files;
#[path = "app/store.rs"]
mod store;
pub(crate) use store::window;

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

#[cfg(test)]
#[path = "app/tests_editing.rs"]
mod tests_editing;

#[cfg(test)]
#[path = "app/tests_look.rs"]
mod tests_look;

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

mod clipboard;
mod editor_chrome;
mod text_box;
mod window_bar;
#[cfg(test)]
use editor_chrome::ButtonKind;
use editor_chrome::{Chrome, ChromeMetrics};
use text_box::{TextBoxEditor, WORDS_ACCESS_KEY, listed, mark_covers};

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
        let er = self.element_frame(dest, scale, el);
        let reach = m.target * 0.25 / 2.0 + m.s1;
        let (mut left, mut top, mut right, mut bottom) = (m.s2, m.s2, m.s2, m.s2);
        if let Some(slide) = self.slide() {
            for other in slide.elements.iter().filter(|other| other.id != el.id) {
                let or = self.element_frame(dest, scale, other);
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
                        presence: None,
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
        self.layout_picker
            .handle_event(routed.event, &mut routed.event_ctx(&mut handled));
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
