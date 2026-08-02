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

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::icons::{draw_icon, Icon};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::text_selection::{
    clipboard_copy, clipboard_paste, next_word_boundary_chars, prev_word_boundary_chars,
    word_boundaries_chars, Selection,
};
use ui::theme::Theme;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{
    ColorPicker, FlapState, NumberInput, PageSidebar, PageSidebarEvent, SplitLayout, TextInput,
    Widget, DEFAULT_BREAKPOINT,
};
use ui::ColorCompat;

use crate::actions;
use crate::assets::ImageStore;
use crate::chrome;
use crate::doc::SlidesDoc;
use crate::portal;
use crate::model::{
    new_id, seed_deck, starter_deck, Background, Deck, Element, ElementKind, Id, Layout, Palette,
    PaletteKind, Slide, TextAlign, FILL_ROLES, SLIDE_H, SLIDE_W, STROKE_ROLES,
};
use crate::render;

/// Logical pixels scrolled per mouse-wheel line notch.
const WHEEL_LINE_PX: f64 = 48.0;

const TOOLBAR_H: f64 = 56.0;
const RAIL_W: f64 = 212.0;
const INSPECTOR_W: f64 = 296.0;
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
/// overlay flap, so the narrowest windows give the canvas the full width. Set
/// below [`NARROW_BREAKPOINT`] so a mid-width window keeps the inspector docked.
const INSPECTOR_BREAKPOINT: f64 = 760.0;
/// Size of the toolbar hamburger toggle that summons the collapsed rail.
const HAMBURGER: f64 = 34.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Edit,
    /// Slide-sorter: a full-window grid of every slide thumbnail.
    Overview,
    Present,
}

/// How the present surface is currently blanked. A presenter can black or
/// white the screen mid-talk (to pull attention back to themselves, or to
/// use the projector as a whiteboard) — `B` and `W` respectively. The
/// inverse key (or any navigation / click) resumes the slide.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Blank {
    None,
    Black,
    White,
}

impl Blank {
    fn is_blanked(self) -> bool {
        !matches!(self, Blank::None)
    }
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

/// How long a present-mode slide cross-fade lasts.
const TRANSITION_MS: u128 = 240;
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
    ScrollOverview { start_y: f64, orig_scroll: f64 },
}

impl Drag {
    fn active(&self) -> bool {
        !matches!(self, Drag::None)
    }
}

#[derive(Clone)]
enum Hit {
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
}

#[derive(Clone)]
enum ButtonKind {
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
enum SegPos {
    First,
    Mid,
    Last,
}

#[derive(Clone)]
struct UiButton {
    rect: Rect,
    kind: ButtonKind,
    hit: Hit,
}

/// Rects the inspector reserves for widgets it doesn't draw as `UiButton`s
/// (the font-size [`NumberInput`], the speaker-notes box).
struct InspectorRects {
    notes: Option<Rect>,
    font_input: Option<Rect>,
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
    /// Background panel of the open toolbar overflow menu (`None` when closed
    /// or when every insert tool already fits on the toolbar).
    menu_bg: Option<Rect>,
    /// Rows of the open overflow menu — the insert tools that didn't fit on the
    /// toolbar, each a pointer-reachable button. Empty unless the menu is open.
    menu_buttons: Vec<UiButton>,
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

pub struct SlidesApp {
    deck: Deck,
    rect: Rect,
    node: Option<NodeId>,
    seeded: bool,
    bootstrapped: bool,

    mode: Mode,
    cur_slide: usize,
    sel: Option<Id>,
    editing_text: bool,
    caret: usize,
    editing_notes: bool,
    notes_caret: usize,
    /// Selection anchor (char index) for the active on-canvas text / notes
    /// editor. `sel_anchor == active_caret()` means "no live selection"; the
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
    show_notes: bool,

    /// Keyboard-shortcuts overlay visibility (toggled with `?`).
    show_help: bool,
    /// Snap-to-guides toggle (G in edit mode would clash with overview, so
    /// snapping is always on, but the active guide lines computed during the
    /// current drag live here so `draw` can paint them).
    active_guides: Vec<Guide>,
    /// Wall-clock start of the current presentation, for the speaker timer.
    present_started: Option<std::time::Instant>,
    /// When the timer is paused: the running elapsed time captured at the
    /// instant of the pause, so the displayed clock is frozen exactly (and
    /// stable across reads). `None` means the timer is running.
    present_paused_elapsed: Option<std::time::Duration>,
    /// Accumulated paused time, subtracted from the raw elapsed so the clock
    /// reflects only *running* presentation time.
    paused_total: std::time::Duration,
    /// In-flight present-mode cross-fade: `(from_slide_idx, started_at)`.
    transition: Option<(usize, std::time::Instant)>,
    /// Present-mode screen blank (press `B` for black, `W` for white).
    blank: Blank,
    /// Whether the *presenter dashboard* is shown instead of the clean
    /// audience slide. On a single monitor this is a togglable full-window
    /// view (press `P` while presenting); on dual monitors the compositor
    /// would paint the dashboard on the presenter's output and the clean
    /// slide on the audience output (see the multi-monitor seam below).
    presenter_view: bool,

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
    /// Width breakpoint between the docked and overlay rail layouts. A field
    /// (not a constant) so `--shots` can force a narrow layout deterministically.
    breakpoint: f64,

    /// Cached render of the settled audience slide in present mode. Present
    /// mode pumps animation frames to keep the ~1 Hz speaker clock live, but
    /// the *slide* underneath is static between navigations — so we shape it
    /// once into a detached scene and re-append it each vsync instead of
    /// re-shaping every text element. Invalidated by
    /// [`PresentCache::matches`] when the slide, size, theme or deck changes.
    present_cache: Option<PresentCache>,
    /// Monotonic revision of the loaded deck, bumped whenever the CRDT mirror
    /// is reloaded. Part of [`PresentCache`]'s key so a remote edit (or undo)
    /// to the current slide invalidates the cached render.
    deck_rev: u64,

    /// Path-keyed decode cache for image elements. Decodes off the UI thread;
    /// the slide shows a light loading state until the pixels land.
    images: ImageStore,
    /// Vertical scroll offset (px) of the edit-mode thumbnail rail. Clamped to
    /// the overflow at every use, so all thumbnails stay reachable however many
    /// slides the deck has.
    rail_scroll: f64,
    /// Vertical scroll offset (px) of the slide-sorter overview grid.
    overview_scroll: f64,
    /// Whether the toolbar's insert-tool overflow menu is open. On a narrow
    /// toolbar the insert tools that don't fit are moved behind a "More" (⋯)
    /// button; this flag reveals them as a pointer-reachable dropdown so every
    /// insert tool stays usable at any width. Transient (per-peer), not CRDT.
    toolbar_menu_open: bool,
    /// An in-flight host image-open request. `Some(rx)` means the XDG portal's
    /// file chooser is open on a worker thread; the chosen path arrives over
    /// this channel and is drained each frame in [`SlidesApp::drain_portal`].
    /// `None` when idle. Per-peer/transient (not CRDT).
    portal_rx: Option<portal::PortalChannel>,
    /// An in-flight host file open/save for `.odp` documents. Same worker-thread
    /// + channel contract as [`Self::portal_rx`]; `save` carries the deck bytes
    /// to write once the user names a destination.
    doc_portal_rx: Option<portal::DocPortalChannel>,

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

/// One shaped audience slide, kept across present-mode frames so the speaker
/// clock can tick without re-rendering the whole slide (see
/// [`SlidesApp::present_cache`]).
struct PresentCache {
    slide: usize,
    rev: u64,
    palette: PaletteKind,
    /// Destination rect, rounded to whole pixels so sub-pixel jitter doesn't
    /// force a needless re-shape.
    dest: (i64, i64, i64, i64),
    /// The slide painted at `Affine::IDENTITY`; appended under the live
    /// transform each frame.
    scene: Scene,
}

impl PresentCache {
    fn key(dest: Rect, slide: usize, rev: u64, palette: PaletteKind) -> (usize, u64, PaletteKind, (i64, i64, i64, i64)) {
        (
            slide,
            rev,
            palette,
            (
                dest.x0.round() as i64,
                dest.y0.round() as i64,
                dest.x1.round() as i64,
                dest.y1.round() as i64,
            ),
        )
    }

    fn matches(&self, dest: Rect, slide: usize, rev: u64, palette: PaletteKind) -> bool {
        (self.slide, self.rev, self.palette, self.dest) == Self::key(dest, slide, rev, palette)
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
        SlidesApp {
            deck,
            rect: Rect::ZERO,
            node: None,
            seeded: false,
            bootstrapped: false,
            mode: Mode::Edit,
            cur_slide: 1,
            sel: None,
            editing_text: false,
            caret: 0,
            editing_notes: false,
            notes_caret: 0,
            sel_anchor: 0,
            editing_title: false,
            title_caret: 0,
            show_notes: false,
            show_help: false,
            active_guides: Vec::new(),
            present_started: None,
            present_paused_elapsed: None,
            paused_total: std::time::Duration::ZERO,
            transition: None,
            blank: Blank::None,
            presenter_view: false,
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
            breakpoint: NARROW_BREAKPOINT,
            present_cache: None,
            deck_rev: 0,
            images: ImageStore::new(),
            rail_scroll: 0.0,
            overview_scroll: 0.0,
            toolbar_menu_open: false,
            portal_rx: None,
            doc_portal_rx: None,
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
                .with_plain_chrome()
                .with_font_size(17.0)
                .with_id("slides-title"),
            demo_menu: None,
            demo_reorder: false,
            demo_delete_zone: false,
        }
    }

    /// Jump the (sample) app to a named screen for a headless `--shots`
    /// capture. No effect on a normal launch — only [`SlidesApp::demo`] and
    /// the `--scenario` flag call this.
    fn apply_scenario(&mut self, shot: &str) {
        match shot {
            "overview" => self.mode = Mode::Overview,
            "present" => {
                self.mode = Mode::Present;
                self.present_started = Some(std::time::Instant::now());
            }
            "present_notes" => {
                self.mode = Mode::Present;
                self.show_notes = true;
                self.present_started = Some(std::time::Instant::now());
            }
            // Full presenter dashboard (current + next + notes + timer +
            // position) as it appears on the presenter's own monitor.
            "presenter" => {
                self.mode = Mode::Present;
                self.presenter_view = true;
                self.present_started = Some(std::time::Instant::now());
            }
            // Present-mode white-out (`W`) — the projector becomes a
            // bright "whiteboard"/attention surface.
            "present_white" => {
                self.mode = Mode::Present;
                self.blank = Blank::White;
                self.present_started = Some(std::time::Instant::now());
            }
            "help" => self.show_help = true,
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
                self.notes_caret = self
                    .deck
                    .slides
                    .get(1)
                    .map(|s| s.notes.chars().count())
                    .unwrap_or(0);
                self.sel_anchor = self.notes_caret;
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
                self.toolbar_menu_open = true;
                self.sel = None;
                self.bootstrapped = true;
            }
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
                self.overview_scroll = 1.0e6;
            }
            // The image-insert tool now defers to the host portal chooser
            // (HIG §5) — there is no in-app picker surface to capture, so this
            // scenario just requests it (a no-op in a headless shot with no
            // portal running) and otherwise shows the plain editor.
            "picker" => self.request_image_open(),
            // A slide carrying a real decoded raster, to prove the image
            // pipeline in a headless screenshot. The picture is written to a
            // temp file and decoded synchronously (the single offscreen frame
            // has no worker-pump), then placed on a dedicated slide.
            "image" => {
                if let Some(path) = write_demo_image() {
                    let src = path.to_string_lossy().to_string();
                    self.images.load_blocking(&src);
                    let mut s = Slide::new(Layout::TitleContent);
                    s.add(
                        Element::text("Site photo", 96.0, 70.0, 1088.0, 90.0, 56.0)
                            .with(|e| e.bold = true),
                    );
                    s.add(Element::shape(ElementKind::Rect, 96.0, 168.0, 150.0, 8.0).with(|e| {
                        e.fill = "accent".into();
                        e.radius = 4.0;
                    }));
                    s.add(Element::image(&src, 96.0, 200.0, 1088.0, 420.0));
                    self.deck.slides.push(s);
                    self.cur_slide = self.deck.slides.len() - 1;
                    self.sel = None;
                    // Keep this slide selected — skip the slide-2 view bootstrap.
                    self.bootstrapped = true;
                }
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
            // Right-click context menus (opened on the first drawn frame once
            // the layout rect is known — see `draw`).
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
            // first word — proves the shared caret / selection core: the
            // highlight is painted behind the glyphs and the caret sits at the
            // selection edge.
            "edit_text" => {
                self.cur_slide = 1;
                self.sel = self.deck.slides.get(1).and_then(|s| {
                    s.elements.iter().find(|e| e.kind == ElementKind::Text).map(|e| e.id.clone())
                });
                self.editing_text = true;
                let text = self.selected_element().map(|e| e.text.clone()).unwrap_or_default();
                let (a, b) = word_boundaries_chars(&text, 0);
                self.sel_anchor = a;
                self.caret = b;
                self.bootstrapped = true;
            }
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
            "inspector_widgets" => {
                self.cur_slide = 1;
                self.sel = self
                    .deck
                    .slides
                    .get(1)
                    .and_then(|s| s.elements.iter().find(|e| e.kind == ElementKind::Text).map(|e| e.id.clone()));
                self.fill_picker_open = true;
                self.fill_picker_needs_init = true;
                self.bootstrapped = true;
            }
            // "edit" (and anything unknown) keeps the default editor screen.
            _ => {}
        }
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
    fn sync_flaps(&mut self, dt: f64) -> SplitLayout {
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
        } else {
            self.rail_flap.tick(rail, dt);
            self.inspector_flap.tick(insp, dt);
        }
        rail
    }

    // --- CRDT mirror --------------------------------------------------------

    fn reload(&mut self, doc: &AutoCommit) {
        if self.drag.active() {
            return; // cache is authoritative mid-drag
        }
        if let Some(deck) = SlidesDoc::load(doc) {
            self.deck = deck;
            // Bump so the present-mode slide cache re-shapes after any edit /
            // undo / remote change. Cheap — `reload` runs on events, not per
            // frame, so this never thrashes the cache during a live talk.
            self.deck_rev = self.deck_rev.wrapping_add(1);
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
                .map(|s| s.element(&id).is_some())
                .unwrap_or(false);
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
    fn perform_action(&mut self, doc: &mut AutoCommit, action: crate::actions::SlidesAction) {
        self.actions.dispatch(doc, crate::actions::SLIDES_ACTION_TYPE, &action);
    }

    /// The last [`EncodedAction`](qos_actions::EncodedAction) an on-screen
    /// affordance dispatched, for the affordance == [`ActionSpec`](qos_actions::ActionSpec)
    /// covenant test: a control and its Ctrl+K entry must build the identical
    /// action.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
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

    /// The rail's painted rect, resolved from the current width + flap reveal.
    /// Mirrors the computation in [`Self::compute_chrome`] so the scroll
    /// helpers and the chrome agree.
    fn rail_rect(&self) -> Rect {
        let r = self.rect;
        let body_top = r.y0 + TOOLBAR_H;
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
    fn rail_viewport(&self, rail: Rect) -> Rect {
        let tpad = 14.0;
        let rb_h = 30.0;
        let list_top = rail.y0 + tpad + 20.0;
        let list_bottom = (rail.y1 - tpad - rb_h - 12.0).max(list_top);
        Rect::new(rail.x0, list_top, rail.x1, list_bottom)
    }

    /// The red dashed "drop to delete" zone, pinned to the bottom of the rail
    /// over the (drag-irrelevant) action row. Only shown — and only live — while
    /// a slide thumbnail is being dragged. Dropping a slide here removes it
    /// instead of reordering it, matching the app-wide drag-to-delete gesture.
    fn slide_delete_zone(&self, rail: Rect) -> Rect {
        Rect::new(rail.x0 + 8.0, rail.y1 - 54.0, rail.x1 - 8.0, rail.y1 - 10.0)
    }

    /// Whether a slide drag is currently hovering the delete zone (for the
    /// zone's armed highlight).
    fn over_delete_zone(&self) -> bool {
        self.page_sidebar.is_dragging()
            && self.slide_delete_zone(self.rail_rect()).contains(self.last_mouse)
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
                self.perform_action(doc, actions::move_slide_action(from, to));
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

    fn compute_chrome(&self) -> Chrome {
        let r = self.rect;
        let toolbar = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOOLBAR_H);

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

        // Fit the 16:9 slide inside the canvas area with padding + caption room.
        let pad = 30.0;
        let cap = 34.0;
        let avail_w = (canvas_area.width() - 2.0 * pad).max(40.0);
        let avail_h = (canvas_area.height() - 2.0 * pad - cap).max(40.0);
        let s = (avail_w / SLIDE_W).min(avail_h / SLIDE_H);
        let sw = SLIDE_W * s;
        let sh = SLIDE_H * s;
        let sx = canvas_area.x0 + (canvas_area.width() - sw) / 2.0;
        let sy = canvas_area.y0 + pad + (avail_h - sh) / 2.0;
        let slide_dest = Rect::new(sx, sy, sx + sw, sy + sh);

        let mut buttons = Vec::new();
        let mut labels = Vec::new();

        // Toolbar -----------------------------------------------------------
        // When the rail can collapse (it's an overlay flap, or docked but
        // closed) a hamburger summons it. It sits at the far left; the title
        // block then shifts right to make room.
        let rail_collapsible = layout == SplitLayout::Overlay || reveal < 0.999;
        let title_x = if rail_collapsible {
            let hy = toolbar.y0 + (TOOLBAR_H - HAMBURGER) / 2.0;
            let hb = Rect::new(toolbar.x0 + 14.0, hy, toolbar.x0 + 14.0 + HAMBURGER, hy + HAMBURGER);
            buttons.push(UiButton {
                rect: hb,
                kind: ButtonKind::Hamburger,
                hit: Hit::ToggleRail,
            });
            hb.x1 + 14.0
        } else {
            toolbar.x0 + 18.0
        };
        let by = toolbar.y0 + (TOOLBAR_H - 34.0) / 2.0;
        let bh = 34.0;

        // Right-anchored group first (Present · palette · overview grid) so we
        // know the boundary the centre tool group and title must not cross.
        let present_w = 18.0 + 6.0 + chrome::est_width("Present", 13.0) + 22.0;
        let present_rect = Rect::new(toolbar.x1 - 18.0 - present_w, by, toolbar.x1 - 18.0, by + bh);
        // On a narrow toolbar the palette pill collapses to just its colour dot
        // (the swatch button already hides its label below 40px wide).
        let pal_w = if toolbar.width() > 640.0 {
            30.0 + chrome::est_width(self.deck.palette.label(), 12.0) + 22.0
        } else {
            34.0
        };
        let pal_rect = Rect::new(present_rect.x0 - 10.0 - pal_w, by, present_rect.x0 - 10.0, by + bh);
        let grid_rect = Rect::new(pal_rect.x0 - 10.0 - bh, by, pal_rect.x0 - 10.0, by + bh);
        buttons.push(UiButton {
            rect: present_rect,
            kind: ButtonKind::IconText { icon: Icon::Monitor, label: "Present".into(), accent: true },
            hit: Hit::Present,
        });
        buttons.push(UiButton {
            rect: pal_rect,
            kind: ButtonKind::Palette { kind: self.deck.palette, selected: false },
            hit: Hit::CyclePalette,
        });
        buttons.push(UiButton {
            rect: grid_rect,
            kind: ButtonKind::IconOnly { icon: Icon::LayoutGrid, active: self.mode == Mode::Overview },
            hit: Hit::Overview,
        });
        // Inspector toggle — offered whenever the inspector can collapse (it's
        // an overlay flap, or docked but not fully open). Mirrors the rail
        // hamburger on the opposite edge.
        let inspector_collapsible = insp_layout == SplitLayout::Overlay || insp_reveal < 0.999;
        let right_edge = if inspector_collapsible {
            let ib = Rect::new(grid_rect.x0 - 10.0 - bh, by, grid_rect.x0 - 10.0, by + bh);
            buttons.push(UiButton {
                rect: ib,
                kind: ButtonKind::IconOnly { icon: Icon::Settings2, active: self.inspector_open() },
                hit: Hit::ToggleInspector,
            });
            ib.x0
        } else {
            grid_rect.x0
        };

        // Title block, elided so it never runs under the tools or right group.
        let tools_x = (title_x + 196.0).max(toolbar.x0 + 320.0_f64.min(toolbar.width() * 0.34));
        let title_max = (tools_x - 12.0 - title_x).max(60.0);
        let title_pos = Point::new(title_x, toolbar.y0 + 11.0);
        // While renaming, the inline TextInput draws the title (with selection +
        // caret), so the static label is blanked; otherwise elide it so it never
        // runs under the tools.
        let title_text = if self.editing_title {
            String::new()
        } else {
            elide(&self.deck.title, 17.0, title_max)
        };
        labels.push(Label {
            pos: title_pos,
            text: title_text,
            font: 17.0,
            color: LabelColor::Primary,
            weight: TextWeight::Bold,
        });
        // The whole two-line title block is the click target for renaming.
        let title_rect = Rect::new(title_x - 4.0, toolbar.y0 + 6.0, title_x + title_max, toolbar.y0 + 46.0);
        labels.push(Label {
            pos: Point::new(title_x, toolbar.y0 + 33.0),
            text: elide(
                &format!(
                    "Slide {} of {}  ·  {}",
                    self.cur_slide + 1,
                    self.deck.slides.len(),
                    self.deck.palette.label()
                ),
                11.5,
                title_max,
            ),
            font: 11.5,
            color: LabelColor::Dim,
            weight: TextWeight::Regular,
        });

        // Centre tool group. Every insert tool must stay pointer-reachable at
        // any width: the ones that fit sit on the toolbar; the rest move behind
        // a "More" (⋯) overflow button that opens a dropdown menu. Nothing is
        // ever silently dropped.
        let tools_right = right_edge - 12.0;
        let (menu_bg, menu_buttons) =
            self.layout_tools(&mut buttons, tools_x, tools_right, by, bh, toolbar);

        // Thumbnail rail ----------------------------------------------------
        // The shared PageSidebar owns the list geometry, scroll and reorder
        // drag inside `rail_viewport` (between the "SLIDES" header and the
        // pinned action row); read back the slot rects it computed this frame so
        // hit-testing, auto-scroll and the `--shots` demo agree with its paint.
        // Slides keeps drawing the panel chrome: the header label here and the
        // Add/Dup/Del action row below.
        let tpad = 14.0;
        let rail_viewport = self.rail_viewport(rail);
        let thumbs = self.page_sidebar.thumb_rects();
        labels.push(Label {
            pos: Point::new(rail.x0 + tpad, rail.y0 + 6.0),
            text: "SLIDES".into(),
            font: 11.0,
            color: LabelColor::Dim,
            weight: TextWeight::Semibold,
        });
        // Rail action buttons at the bottom.
        let rb_h = 30.0;
        let rb_y = rail.y1 - tpad - rb_h;
        let rb_w = (rail.width() - 2.0 * tpad - 2.0 * 8.0) / 3.0;
        let mk = |idx: f64, label: &str, danger: bool, hit: Hit| -> UiButton {
            let x = rail.x0 + tpad + idx * (rb_w + 8.0);
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
        buttons.push(mk(0.0, "Add", false, Hit::NewSlide));
        buttons.push(mk(1.0, "Dup", false, Hit::DuplicateSlide));
        buttons.push(mk(2.0, "Del", true, Hit::DeleteSlide));

        // Inspector ---------------------------------------------------------
        let insp_rects = self.build_inspector(inspector, &mut buttons, &mut labels);
        let notes_box = insp_rects.notes;
        let font_input_rect = insp_rects.font_input;
        // Fill picker popover, anchored inside the inspector, clamped on-screen.
        let fill_picker_rect = if self.fill_picker_open {
            // Leave ~64px of slack on the right: the picker's hex field extends
            // past its preview swatch, so a flush-right anchor would clip it.
            let x = (inspector.x0 + 12.0).min(r.x1 - FILL_PICKER_W - 64.0).max(r.x0 + 8.0);
            // Sit below the top property controls, above the notes box.
            let y = (inspector.y0 + 300.0).min(r.y1 - FILL_PICKER_H - 8.0).max(body_top + 8.0);
            Some(Rect::new(x, y, x + FILL_PICKER_W, y + FILL_PICKER_H))
        } else {
            None
        };

        Chrome {
            toolbar,
            title_rect,
            title_pos,
            rail,
            inspector,
            slide_dest,
            scale: s,
            thumbs,
            buttons,
            labels,
            rail_viewport,
            notes_box,
            font_input_rect,
            fill_picker_rect,
            menu_bg,
            menu_buttons,
            layout,
            inspector_layout: insp_layout,
            scrim,
            reveal,
            inspector_reveal: insp_reveal,
        }
    }

    /// Place the insert tools on the toolbar between `tools_x` and
    /// `tools_right`. Tools that fit are pushed onto `buttons`; the ones that
    /// don't move behind a "More" (⋯) overflow button (also pushed onto
    /// `buttons`), and — when [`Self::toolbar_menu_open`] — are returned as the
    /// dropdown's `(background, rows)`. This guarantees **every** insert tool is
    /// pointer-reachable at every width: nothing is ever silently dropped.
    fn layout_tools(
        &self,
        buttons: &mut Vec<UiButton>,
        tools_x: f64,
        tools_right: f64,
        by: f64,
        bh: f64,
        toolbar: Rect,
    ) -> (Option<Rect>, Vec<UiButton>) {
        struct Tool {
            bar: &'static str,
            menu: &'static str,
            icon: Option<Icon>,
            hit: Hit,
        }
        // Highest priority first, so the tools most reached for stay on the bar
        // and the rarer shape tools are the first to fall into the menu.
        let tools = [
            Tool { bar: "Slide", menu: "New slide", icon: Some(Icon::Plus), hit: Hit::NewSlide },
            Tool { bar: "Text", menu: "Text box", icon: Some(Icon::FileText), hit: Hit::Insert(ElementKind::Text) },
            Tool { bar: "Rect", menu: "Rectangle", icon: None, hit: Hit::Insert(ElementKind::Rect) },
            Tool { bar: "Ellipse", menu: "Ellipse", icon: None, hit: Hit::Insert(ElementKind::Ellipse) },
            Tool { bar: "Line", menu: "Line", icon: None, hit: Hit::Insert(ElementKind::Line) },
            Tool { bar: "Image", menu: "Image", icon: None, hit: Hit::Insert(ElementKind::Image) },
        ];
        let gap = 8.0;
        let bar_w = |t: &Tool| -> f64 {
            match t.icon {
                Some(_) => 18.0 + 6.0 + chrome::est_width(t.bar, 13.0) + 22.0,
                None => chrome::est_width(t.bar, 13.0) + 26.0,
            }
        };
        let widths: Vec<f64> = tools.iter().map(|t| bar_w(t)).collect();
        let total: f64 = widths.iter().sum::<f64>() + gap * (tools.len() as f64 - 1.0);
        let fits_all = tools_x + total <= tools_right + 0.5;

        // When something must overflow, reserve room for the ⋯ button on the
        // right so it (and thus the hidden tools) is always reachable.
        let ovf_w = 40.0;
        let limit = if fits_all { tools_right } else { tools_right - ovf_w - gap };

        let mut bx = tools_x;
        let mut hidden: Vec<usize> = Vec::new();
        for (i, t) in tools.iter().enumerate() {
            let w = widths[i];
            // Keep visible tools contiguous: once one tool overflows, all the
            // rest go to the menu too (no gap-filling with a later, narrower one).
            if hidden.is_empty() && bx + w <= limit + 0.5 {
                let rect = Rect::new(bx, by, bx + w, by + bh);
                let kind = match t.icon {
                    Some(icon) => ButtonKind::IconText { icon, label: t.bar.to_string(), accent: false },
                    None => ButtonKind::Text { label: t.bar.to_string(), active: false, danger: false },
                };
                buttons.push(UiButton { rect, kind, hit: t.hit.clone() });
                bx += w + gap;
            } else {
                hidden.push(i);
            }
        }
        if hidden.is_empty() {
            return (None, Vec::new());
        }
        // ⋯ overflow button, just past the last visible tool (clamped to fit).
        let ovf_x = bx.min(tools_right - ovf_w);
        let ovf = Rect::new(ovf_x, by, ovf_x + ovf_w, by + bh);
        buttons.push(UiButton {
            rect: ovf,
            kind: ButtonKind::IconOnly { icon: Icon::MoreHorizontal, active: self.toolbar_menu_open },
            hit: Hit::ToggleToolbarMenu,
        });
        if !self.toolbar_menu_open {
            return (None, Vec::new());
        }
        // Dropdown: a panel under the ⋯ button, right-aligned to it and clamped
        // inside the toolbar, one full-width row per hidden tool.
        let row_h = 34.0;
        let pad = 6.0;
        let menu_w = hidden
            .iter()
            .map(|&i| chrome::est_width(tools[i].menu, 13.0) + 52.0)
            .fold(150.0_f64, f64::max)
            .min(toolbar.width() - 16.0);
        let mx1 = ovf.x1.clamp(toolbar.x0 + 8.0 + menu_w, toolbar.x1 - 8.0);
        let mx0 = mx1 - menu_w;
        let my0 = ovf.y1 + 6.0;
        let my1 = my0 + pad * 2.0 + row_h * hidden.len() as f64;
        let bg = Rect::new(mx0, my0, mx1, my1);
        let mut rows = Vec::new();
        for (r, &i) in hidden.iter().enumerate() {
            let ry = my0 + pad + r as f64 * row_h;
            rows.push(UiButton {
                rect: Rect::new(mx0 + pad, ry, mx1 - pad, ry + row_h - 4.0),
                kind: ButtonKind::Text { label: tools[i].menu.to_string(), active: false, danger: false },
                hit: tools[i].hit.clone(),
            });
        }
        (Some(bg), rows)
    }

    /// Lay out the inspector contents; returns the speaker-notes box rect.
    fn build_inspector(
        &self,
        inspector: Rect,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
    ) -> InspectorRects {
        let mut font_input_rect: Option<Rect> = None;
        let pad = 16.0;
        let x0 = inspector.x0 + pad;
        let x1 = inspector.x1 - pad;
        let inner_w = x1 - x0;
        let mut y = inspector.y0 + pad;

        let header = |labels: &mut Vec<Label>, y: &mut f64, t: &str| {
            labels.push(Label {
                pos: Point::new(x0, *y),
                text: t.to_string(),
                font: 11.0,
                color: LabelColor::Dim,
                weight: TextWeight::Semibold,
            });
            *y += 20.0;
        };

        if let Some(el) = self.selected_element() {
            labels.push(Label {
                pos: Point::new(x0, y),
                text: el.kind.label().to_uppercase(),
                font: 16.0,
                color: LabelColor::Primary,
                weight: TextWeight::Bold,
            });
            y += 30.0;

            // Fill swatches (theme presets) plus a Custom… button that opens
            // the embeddable colour picker for an arbitrary fill.
            header(labels, &mut y, "FILL");
            y = self.swatch_grid(buttons, x0, inner_w, y, &FILL_ROLES, &el.fill, true);
            y += 8.0;
            let custom = Rect::new(x0, y, x1, y + 26.0);
            buttons.push(UiButton {
                rect: custom,
                kind: ButtonKind::Text {
                    // Show the active colour role / hex on the button.
                    label: if el.fill.starts_with('#') {
                        format!("Custom  ·  {}", el.fill)
                    } else {
                        "Custom colour…".into()
                    },
                    active: el.fill.starts_with('#'),
                    danger: false,
                },
                hit: Hit::OpenFillPicker,
            });
            y += 26.0 + 10.0;

            // Stroke swatches.
            header(labels, &mut y, "STROKE");
            y = self.swatch_grid(buttons, x0, inner_w, y, &STROKE_ROLES, &el.stroke, false);
            y += 10.0;

            if el.kind == ElementKind::Text {
                header(labels, &mut y, "TYPE");
                let bh = 30.0;
                // The font size is a click-to-type NumberInput (with ± steppers)
                // — the widget draws itself, so we only reserve its rect here.
                font_input_rect = Some(Rect::new(x0, y, x0 + 136.0, y + bh));
                let bold = Rect::new(x1 - 36.0, y, x1, y + bh);
                buttons.push(UiButton {
                    rect: bold,
                    kind: ButtonKind::Text { label: "B".into(), active: el.bold, danger: false },
                    hit: Hit::ToggleBold,
                });
                let italic = Rect::new(x1 - 36.0 - 6.0 - 36.0, y, x1 - 36.0 - 6.0, y + bh);
                buttons.push(UiButton {
                    rect: italic,
                    kind: ButtonKind::Text { label: "I".into(), active: el.italic, danger: false },
                    hit: Hit::ToggleItalic,
                });
                y += bh + 10.0;
                header(labels, &mut y, "ALIGN");
                // A proper segmented control: left · center · right · justify,
                // the current alignment highlighted. Replaces the old single
                // button that only cycled through the options blind.
                let n = TextAlign::ALL.len();
                let seg_w = inner_w / n as f64;
                for (i, a) in TextAlign::ALL.iter().enumerate() {
                    let sx = x0 + i as f64 * seg_w;
                    let ex = if i + 1 == n { x1 } else { x0 + (i as f64 + 1.0) * seg_w };
                    let pos = if i == 0 {
                        SegPos::First
                    } else if i + 1 == n {
                        SegPos::Last
                    } else {
                        SegPos::Mid
                    };
                    buttons.push(UiButton {
                        rect: Rect::new(sx, y, ex, y + bh),
                        kind: ButtonKind::Segment {
                            label: a.glyph().to_string(),
                            active: *a == el.align,
                            pos,
                        },
                        hit: Hit::SetAlign(*a),
                    });
                }
                y += bh + 14.0;
            }

            header(labels, &mut y, "ARRANGE");
            let bh = 30.0;
            let half = (inner_w - 8.0) / 2.0;
            let front = Rect::new(x0, y, x0 + half, y + bh);
            let back = Rect::new(x0 + half + 8.0, y, x1, y + bh);
            buttons.push(UiButton {
                rect: front,
                kind: ButtonKind::Text { label: "Bring front".into(), active: false, danger: false },
                hit: Hit::BringFront,
            });
            buttons.push(UiButton {
                rect: back,
                kind: ButtonKind::Text { label: "Send back".into(), active: false, danger: false },
                hit: Hit::SendBack,
            });
            y += bh + 8.0;
            let dup = Rect::new(x0, y, x0 + half, y + bh);
            let del = Rect::new(x0 + half + 8.0, y, x1, y + bh);
            buttons.push(UiButton {
                rect: dup,
                kind: ButtonKind::Text { label: "Duplicate".into(), active: false, danger: false },
                hit: Hit::DuplicateElement,
            });
            buttons.push(UiButton {
                rect: del,
                kind: ButtonKind::Text { label: "Delete".into(), active: false, danger: true },
                hit: Hit::DeleteElement,
            });
            y += bh + 6.0;
        } else {
            labels.push(Label {
                pos: Point::new(x0, y),
                text: format!("SLIDE {}", self.cur_slide + 1),
                font: 16.0,
                color: LabelColor::Primary,
                weight: TextWeight::Bold,
            });
            y += 30.0;

            header(labels, &mut y, "LAYOUT");
            let cur_layout = self.slide().map(|s| s.layout).unwrap_or(Layout::TitleContent);
            y = self.wrap_pills(
                buttons,
                x0,
                x1,
                y,
                Layout::ALL.iter().map(|l| {
                    (l.label().to_string(), *l == cur_layout, Hit::SetLayout(*l))
                }),
            );
            y += 10.0;

            header(labels, &mut y, "BACKGROUND");
            let cur_bg = self.slide().map(|s| s.background).unwrap_or(Background::Surface);
            y = self.wrap_pills(
                buttons,
                x0,
                x1,
                y,
                Background::ALL.iter().map(|b| {
                    (b.label().to_string(), *b == cur_bg, Hit::SetBackground(*b))
                }),
            );
            y += 10.0;

            header(labels, &mut y, "DECK THEME");
            y = self.wrap_palette_pills(buttons, x0, x1, y);
            y += 10.0;
        }

        // Speaker notes (always shown at the bottom).
        let notes_top = (y + 6.0).max(inspector.y1 - 188.0);
        labels.push(Label {
            pos: Point::new(x0, notes_top),
            text: if self.editing_notes { "SPEAKER NOTES  (editing — Esc)" } else { "SPEAKER NOTES" }
                .to_string(),
            font: 11.0,
            color: LabelColor::Dim,
            weight: TextWeight::Semibold,
        });
        let nb = Rect::new(x0, notes_top + 20.0, x1, inspector.y1 - pad);
        buttons.push(UiButton {
            rect: nb,
            kind: ButtonKind::Text { label: String::new(), active: false, danger: false },
            hit: Hit::EditNotes,
        });
        InspectorRects { notes: Some(nb), font_input: font_input_rect }
    }

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
        let gap = 8.0;
        let sw = (inner_w - (cols - 1.0) * gap) / cols;
        let sh = 26.0;
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
    /// the colour dot (drawn at `x0+8 .. x0+20`), a 6px gap, the label, and a
    /// trailing pad. `est_width` deliberately overestimates the real advance,
    /// so a pill sized to this never clips its name.
    fn palette_pill_width(label: &str) -> f64 {
        26.0 + chrome::est_width(label, 12.0) + 12.0
    }

    /// Lay out the five deck-theme pills, each sized to fit its full name and
    /// wrapping onto a new row when the next pill would overflow `x1`. This is
    /// what keeps every theme name ("Indigo", "Emerald", …) fully readable in
    /// the docked inspector at any width, instead of packing five fixed-width
    /// pills that clip their labels. Returns the y just past the last row.
    fn wrap_palette_pills(&self, buttons: &mut Vec<UiButton>, x0: f64, x1: f64, mut y: f64) -> f64 {
        let bh = 28.0;
        let gap = 8.0;
        let mut x = x0;
        for p in PaletteKind::ALL.iter() {
            let w = Self::palette_pill_width(p.label()).min(x1 - x0);
            if x > x0 && x + w > x1 + 0.5 {
                x = x0;
                y += bh + gap;
            }
            buttons.push(UiButton {
                rect: Rect::new(x, y, x + w, y + bh),
                kind: ButtonKind::Palette { kind: *p, selected: *p == self.deck.palette },
                hit: Hit::SetPalette(*p),
            });
            x += w + gap;
        }
        y + bh
    }

    fn wrap_pills(
        &self,
        buttons: &mut Vec<UiButton>,
        x0: f64,
        x1: f64,
        mut y: f64,
        items: impl Iterator<Item = (String, bool, Hit)>,
    ) -> f64 {
        let bh = 28.0;
        let gap = 8.0;
        let mut x = x0;
        for (label, active, hit) in items {
            let w = chrome::est_width(&label, 12.0) + 22.0;
            if x + w > x1 {
                x = x0;
                y += bh + gap;
            }
            buttons.push(UiButton {
                rect: Rect::new(x, y, x + w, y + bh),
                kind: ButtonKind::Text { label, active, danger: false },
                hit,
            });
            x += w + gap;
        }
        y + bh
    }

    // --- mouse → slide coords ----------------------------------------------

    fn to_slide(&self, dest: Rect, s: f64, p: Point) -> Point {
        Point::new((p.x - dest.x0) / s, (p.y - dest.y0) / s)
    }

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
            self.toolbar_menu_open = false;
        }
        match hit {
            Hit::ToggleToolbarMenu => self.toolbar_menu_open = !self.toolbar_menu_open,
            Hit::NewSlide => self.new_slide(doc),
            Hit::DuplicateSlide => self.duplicate_slide(doc),
            Hit::DeleteSlide => self.delete_slide(doc),
            Hit::Present => self.enter_present(),
            Hit::Overview => {
                self.mode = if self.mode == Mode::Overview { Mode::Edit } else { Mode::Overview };
                self.editing_text = false;
                self.editing_notes = false;
            }
            Hit::OpenSlide(i) => {
                self.cur_slide = i.min(self.deck.slides.len().saturating_sub(1));
                self.sel = None;
                self.mode = Mode::Edit;
            }
            Hit::CyclePalette => {
                self.perform_action(doc, actions::set_deck_palette_action(self.deck.palette.next()));
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
                if let Some(el) = self.selected_element() {
                    if role != "none" && el.stroke_width < 1.0 {
                        self.set_sel_f64(doc, "stroke_width", 4.0);
                    }
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
                self.perform_action(doc, actions::set_slide_layout_action(self.cur_slide, l));
            }
            Hit::SetBackground(b) => {
                self.perform_action(doc, actions::set_slide_background_action(self.cur_slide, b));
            }
            Hit::SetPalette(p) => {
                self.perform_action(doc, actions::set_deck_palette_action(p));
            }
            Hit::EditNotes => {
                self.editing_notes = true;
                self.editing_text = false;
                self.notes_caret = self.slide().map(|s| s.notes.chars().count()).unwrap_or(0);
                self.sel_anchor = self.notes_caret;
            }
            Hit::ToggleRail => self.rail_flap.toggle(self.rail_layout()),
            Hit::ToggleInspector => self.inspector_flap.toggle(self.inspector_layout()),
        }
        self.reload(doc);
    }

    fn set_sel_str(&mut self, doc: &mut AutoCommit, key: &str, v: &str) {
        if let Some(id) = self.sel.clone() {
            self.perform_action(doc, actions::restyle_str_action(self.cur_slide, id, key, v));
        }
    }
    fn set_sel_f64(&mut self, doc: &mut AutoCommit, key: &str, v: f64) {
        if let Some(id) = self.sel.clone() {
            self.perform_action(doc, actions::restyle_f64_action(self.cur_slide, id, key, v));
        }
    }
    fn set_sel_bool(&mut self, doc: &mut AutoCommit, key: &str, v: bool) {
        if let Some(id) = self.sel.clone() {
            self.perform_action(doc, actions::restyle_bool_action(self.cur_slide, id, key, v));
        }
    }

    fn restack(&mut self, doc: &mut AutoCommit, front: bool) {
        let Some(id) = self.sel.clone() else { return };
        let action = if front {
            actions::raise_element_action(self.cur_slide, id)
        } else {
            actions::lower_element_action(self.cur_slide, id)
        };
        self.perform_action(doc, action);
    }

    fn new_slide(&mut self, doc: &mut AutoCommit) {
        let s = actions::new_slide_template();
        let at = self.cur_slide + 1;
        self.perform_action(doc, actions::insert_slide_action(at, &s));
        self.cur_slide = at;
        self.sel = None;
        self.editing_text = false;
        self.scroll_rail_to_current();
    }

    fn duplicate_slide(&mut self, doc: &mut AutoCommit) {
        let Some(slide) = self.deck.slides.get(self.cur_slide).cloned() else { return };
        let mut copy = slide;
        copy.id = new_id();
        for e in &mut copy.elements {
            e.id = new_id();
        }
        let at = self.cur_slide + 1;
        self.perform_action(doc, actions::insert_slide_action(at, &copy));
        self.cur_slide = at;
        self.sel = None;
        self.scroll_rail_to_current();
    }

    fn delete_slide(&mut self, doc: &mut AutoCommit) {
        if self.deck.slides.len() <= 1 {
            return; // keep at least one slide
        }
        self.perform_action(doc, actions::delete_slide_action(self.cur_slide));
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
        self.perform_action(doc, actions::add_element_action(self.cur_slide, &el));
        self.sel = Some(id);
        self.editing_text = false;
    }

    /// Ask the host desktop's file chooser to pick an image, via the XDG
    /// Desktop Portal. Per HIG §5 the app never draws its own picker: the
    /// portal round-trip runs on a worker thread ([`portal::request_open`]) and
    /// the chosen path comes back over a channel drained each frame in
    /// [`Self::drain_portal`]. A second request while one is in flight is
    /// ignored, so a double-click can't stack two host dialogs.
    fn request_image_open(&mut self) {
        if self.portal_rx.is_some() {
            return;
        }
        self.portal_rx = Some(portal::request_open());
    }

    /// Ask the host to pick a `.odp` presentation to open (Ctrl+O). The chosen
    /// file is read and decoded when the request drains in
    /// [`Self::drain_doc_portal`]. One request at a time.
    fn request_odp_open(&mut self) {
        if self.doc_portal_rx.is_some() {
            return;
        }
        self.doc_portal_rx = Some(portal::request_open_document());
    }

    /// Ask the host where to save the current deck as a `.odp` (Ctrl+S).
    fn request_odp_save(&mut self) {
        if self.doc_portal_rx.is_some() {
            return;
        }
        let name = format!("{}.odp", sanitize_filename(&self.deck.title));
        self.doc_portal_rx = Some(portal::request_save_document(&name));
    }

    /// Drain a completed `.odp` open/save request, loading or writing the deck.
    /// Called once per frame from [`Widget::sync`].
    fn drain_doc_portal(&mut self, doc: &mut AutoCommit) {
        let Some(rx) = self.doc_portal_rx.as_ref() else { return };
        match rx.try_recv() {
            Ok(outcome) => {
                self.doc_portal_rx = None;
                match outcome {
                    portal::DocOutcome::Open(path) => {
                        self.load_odp(doc, &path);
                    }
                    portal::DocOutcome::Save(path) => {
                        self.save_odp(&path);
                    }
                    portal::DocOutcome::Cancelled | portal::DocOutcome::Failed(_) => {}
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.doc_portal_rx = None;
            }
        }
    }

    /// Load a `.odp` from disk, replacing the deck in the shared document so
    /// undo/redo and collaboration see the whole swap as one change.
    fn load_odp(&mut self, doc: &mut AutoCommit, path: &std::path::Path) -> bool {
        let Ok(bytes) = std::fs::read(path) else { return false };
        let Ok(deck) = crate::odp::read_odp(&bytes) else { return false };
        SlidesDoc::seed(doc, &deck);
        self.deck = deck;
        self.deck_rev = self.deck_rev.wrapping_add(1);
        self.cur_slide = 0;
        self.sel = None;
        self.editing_text = false;
        self.editing_notes = false;
        self.present_cache = None;
        self.mark();
        self.reload(doc);
        true
    }

    /// Serialise the current deck to a `.odp` at `path` (adding the extension
    /// if the host didn't).
    fn save_odp(&self, path: &std::path::Path) -> bool {
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
        let Some(rx) = self.portal_rx.as_ref() else { return };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                match outcome {
                    portal::PortalOutcome::Opened(path) => self.insert_image_from_path(doc, &path),
                    // A cancel or a portal failure is not a bug — leave the deck
                    // untouched (we never fabricate a placeholder element).
                    portal::PortalOutcome::Cancelled | portal::PortalOutcome::Failed(_) => {}
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
        let src = path.to_string_lossy().to_string();
        if src.is_empty() {
            return;
        }
        // Start decoding now (off the UI thread) so the first paint already has
        // pixels, or a graceful loading/failed state if it can't be read.
        let _ = self.images.get(&src);
        // A comfortable default frame, centred on the slide; the user resizes.
        let el = Element::image(&src, 340.0, 168.0, 600.0, 384.0);
        let id = el.id.clone();
        self.perform_action(doc, actions::add_element_action(self.cur_slide, &el));
        self.sel = Some(id);
        self.editing_text = false;
        self.reload(doc);
    }

    fn duplicate_element(&mut self, doc: &mut AutoCommit) {
        let Some(el) = self.selected_element().cloned() else { return };
        let mut copy = el;
        copy.id = new_id();
        copy.x += 24.0;
        copy.y += 24.0;
        let id = copy.id.clone();
        self.perform_action(doc, actions::add_element_action(self.cur_slide, &copy));
        self.sel = Some(id);
    }

    fn delete_element(&mut self, doc: &mut AutoCommit) {
        if let Some(id) = self.sel.clone() {
            self.perform_action(doc, actions::delete_element_action(self.cur_slide, id));
            self.sel = None;
            self.editing_text = false;
        }
    }

    fn nudge(&mut self, doc: &mut AutoCommit, dx: f64, dy: f64) {
        if let Some(el) = self.selected_element().cloned() {
            let nx = (el.x + dx).clamp(-el.w + 20.0, SLIDE_W - 20.0);
            let ny = (el.y + dy).clamp(-el.h + 20.0, SLIDE_H - 20.0);
            self.perform_action(doc, actions::move_element_action(self.cur_slide, el.id, nx, ny));
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
                self.transition = Some((from, std::time::Instant::now()));
                self.blank = Blank::None;
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
                self.transition = Some((from, std::time::Instant::now()));
                self.blank = Blank::None;
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
        self.blank = Blank::None;
        self.transition = None;
        self.present_started = Some(std::time::Instant::now());
        self.present_paused_elapsed = None;
        self.paused_total = std::time::Duration::ZERO;
    }

    /// Leave present mode back to the editor, clearing transient present
    /// state (blank, presenter dashboard) but keeping the elapsed timer so a
    /// re-entry via `P` resumes the same talk unless explicitly reset.
    fn exit_present(&mut self) {
        self.mode = Mode::Edit;
        self.blank = Blank::None;
        self.presenter_view = false;
        self.transition = None;
    }

    /// Running elapsed time of the presentation, excluding paused spans.
    /// While paused this returns the exact snapshot captured at pause time,
    /// so the displayed clock is frozen and stable across reads.
    fn present_elapsed(&self) -> std::time::Duration {
        if let Some(frozen) = self.present_paused_elapsed {
            return frozen;
        }
        let Some(start) = self.present_started else {
            return std::time::Duration::ZERO;
        };
        start.elapsed().saturating_sub(self.paused_total)
    }

    /// Elapsed presentation time, formatted `m:ss`.
    fn present_clock(&self) -> String {
        let secs = self.present_elapsed().as_secs();
        format!("{}:{:02}", secs / 60, secs % 60)
    }

    /// Toggle the speaker timer between running and paused.
    fn toggle_timer(&mut self) {
        match self.present_paused_elapsed.take() {
            // Was paused → fold the elapsed-since-start-minus-displayed back
            // into `paused_total` so the running clock continues from where it
            // froze, then resume.
            Some(frozen) => {
                if let Some(start) = self.present_started {
                    self.paused_total = start.elapsed().saturating_sub(frozen);
                }
            }
            // Was running → snapshot the current running elapsed and freeze.
            None => self.present_paused_elapsed = Some(self.present_elapsed()),
        }
    }

    /// Reset the speaker timer to 0:00 and start it running.
    fn reset_timer(&mut self) {
        self.present_started = Some(std::time::Instant::now());
        self.present_paused_elapsed = None;
        self.paused_total = std::time::Duration::ZERO;
    }

    fn timer_paused(&self) -> bool {
        self.present_paused_elapsed.is_some()
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
    /// fullscreen. On the qOS compositor (which spans one combined scene) that
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
    ///     fullscreens the **audience** surface — `draw_present` with
    ///     `presenter_view = false` — on the **larger** output, and paints the
    ///     **presenter dashboard** — `draw_presenter` — on the other.
    ///   * Both surfaces share one CRDT deck + one `cur_slide`, so advancing
    ///     on either output is reflected on both instantly (the model is
    ///     already per-deck, with `cur_slide` per-peer view state).
    ///
    /// Why it is a seam, not implemented here: `ui::App` is strictly
    /// single-window, so one process cannot open a second window on a second
    /// output. True dual-output present needs the compositor to either spawn
    /// a second slides surface bound to the presenter output, or to composite
    /// two views of the same surface. Until then, the presenter dashboard is
    /// a **togglable full-window view** on this one monitor (press `P` while
    /// presenting), which is exactly what `draw_presenter` renders.
    ///
    /// Returns `(audience_on_larger, presenter_on_other)` as a hint a
    /// compositor integration can act on; both are always available because
    /// both views are pure functions of the shared deck + `cur_slide`.
    pub fn present_output_plan(&self) -> (PresentSurface, PresentSurface) {
        (PresentSurface::Audience, PresentSurface::Presenter)
    }

    /// Eased progress 0..1 of the active transition (or 1.0 when settled).
    fn transition_alpha(&self) -> f32 {
        match self.transition {
            Some((_, t)) => {
                let e = t.elapsed().as_millis().min(TRANSITION_MS);
                let p = e as f32 / TRANSITION_MS as f32;
                // smoothstep
                (p * p * (3.0 - 2.0 * p)).clamp(0.0, 1.0)
            }
            None => 1.0,
        }
    }

    // --- text editing -------------------------------------------------------

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
        // Typing over a selection replaces it (delete first, then insert at the
        // collapsed caret).
        self.delete_selection(doc);
        if self.editing_notes {
            SlidesDoc::splice_notes(doc, self.cur_slide, self.notes_caret, 0, s);
            self.notes_caret += s.chars().count();
            self.sel_anchor = self.notes_caret;
            self.reload(doc);
            return;
        }
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
        // A live selection is deleted whole; otherwise delete the char before.
        if self.delete_selection(doc) {
            return;
        }
        if self.editing_notes {
            if self.notes_caret > 0 {
                SlidesDoc::splice_notes(doc, self.cur_slide, self.notes_caret - 1, 1, "");
                self.notes_caret -= 1;
                self.sel_anchor = self.notes_caret;
                self.mark();
                self.reload(doc);
            }
            return;
        }
        if let Some(id) = self.sel.clone() {
            if self.caret > 0 {
                SlidesDoc::splice_element_text(doc, self.cur_slide, &id, self.caret - 1, 1, "");
                self.caret -= 1;
                self.sel_anchor = self.caret;
                self.mark();
                self.reload(doc);
            }
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
        if self.delete_selection(doc) {
            return;
        }
        let len = self.cur_text_len();
        if self.editing_notes {
            if self.notes_caret < len {
                SlidesDoc::splice_notes(doc, self.cur_slide, self.notes_caret, 1, "");
                self.mark();
                self.reload(doc);
            }
            return;
        }
        if let Some(id) = self.sel.clone() {
            if self.caret < len {
                SlidesDoc::splice_element_text(doc, self.cur_slide, &id, self.caret, 1, "");
                self.mark();
                self.reload(doc);
            }
        }
    }

    fn cur_text_len(&self) -> usize {
        if self.editing_title {
            self.deck.title.chars().count()
        } else if self.editing_notes {
            self.slide().map(|s| s.notes.chars().count()).unwrap_or(0)
        } else {
            self.selected_element().map(|e| e.text.chars().count()).unwrap_or(0)
        }
    }

    /// The text currently being edited (title / notes / selected element), used
    /// by the caret helpers for vertical navigation.
    fn cur_edit_text(&self) -> String {
        if self.editing_title {
            self.deck.title.clone()
        } else if self.editing_notes {
            self.slide().map(|s| s.notes.clone()).unwrap_or_default()
        } else {
            self.selected_element().map(|e| e.text.clone()).unwrap_or_default()
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

    /// The rect the inline title editor occupies in the toolbar.
    fn title_input_rect(&self, chrome: &Chrome) -> Rect {
        Rect::new(
            chrome.title_pos.x,
            chrome.title_pos.y - 3.0,
            chrome.title_rect.x1,
            chrome.title_pos.y + 23.0,
        )
    }

    /// Route an event to the inline title [`TextInput`] while renaming. Returns
    /// `true` if the event was consumed. Enter / Esc commit; a press outside the
    /// field commits and falls through so the click still acts.
    #[allow(clippy::too_many_arguments)]
    fn route_title_edit(
        &mut self,
        event: &WindowEvent,
        sf: f64,
        mods: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if !self.editing_title {
            return false;
        }
        let chrome = self.compute_chrome();
        let rect = self.title_input_rect(&chrome);
        self.title_input.apply_layout_external(rect);

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / sf, position.y / sf);
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } if matches!(logical_key, Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Escape)) => {
                self.commit_title_edit(state);
                return true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !rect.inflate(4.0, 8.0).contains(self.last_mouse) => {
                // Click away commits, then falls through so the click still acts.
                self.commit_title_edit(state);
                return false;
            }
            _ => {}
        }
        let mut h = false;
        self.title_input
            .handle_event(event, sf, mods, state, path, &mut h, focus_target, node_id, taffy);
        // Live-splice each edit into the CRDT title (the mirror is refreshed on
        // commit; while editing, the field is the source of truth).
        if let Some(t) = self.title_input.take_dirty() {
            SlidesDoc::set_title(state, &t);
            self.mark();
        }
        true
    }

    /// The char-index caret of whatever text is being edited.
    fn active_caret(&self) -> usize {
        if self.editing_title {
            self.title_caret
        } else if self.editing_notes {
            self.notes_caret
        } else {
            self.caret
        }
    }

    fn set_active_caret(&mut self, v: usize) {
        if self.editing_title {
            self.title_caret = v;
        } else if self.editing_notes {
            self.notes_caret = v;
        } else {
            self.caret = v;
        }
    }

    /// The active editor's [`Selection`] (anchor + caret) over the CRDT text,
    /// clamped to the current text length. Collapsed when nothing is selected.
    fn selection(&self) -> Selection {
        let len = self.cur_text_len();
        Selection { anchor: self.sel_anchor.min(len), cursor: self.active_caret().min(len) }
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
        let sel = self.selection();
        if sel.is_collapsed() {
            return false;
        }
        let (a, b) = (sel.min(), sel.max());
        if self.editing_notes {
            SlidesDoc::splice_notes(doc, self.cur_slide, a, b - a, "");
            self.notes_caret = a;
        } else if let Some(id) = self.sel.clone() {
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
        self.cur_edit_text().chars().skip(sel.min()).take(sel.max() - sel.min()).collect()
    }

    /// Select the whole of the active editor's text.
    fn select_all_text(&mut self) {
        let len = self.cur_text_len();
        self.sel_anchor = 0;
        self.set_active_caret(len);
    }

    /// Move the active caret one char left (`dir < 0`) or right. When `extend`
    /// is false and a selection is live, collapse to the selection edge in that
    /// direction instead of moving past it (standard editor behaviour).
    fn move_caret(&mut self, dir: i32, extend: bool) {
        if !extend && self.has_text_selection() {
            let sel = self.selection();
            let edge = if dir < 0 { sel.min() } else { sel.max() };
            self.place_caret(edge, false);
            return;
        }
        let len = self.cur_text_len();
        let cur = self.active_caret().min(len);
        let next = if dir < 0 { cur.saturating_sub(1) } else { (cur + 1).min(len) };
        self.place_caret(next, extend);
    }

    /// Move the active caret one word left (`dir < 0`) or right, via the shared
    /// word-boundary walker (`ctrl`+arrow).
    fn move_caret_word(&mut self, dir: i32, extend: bool) {
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
        let div = c.border_standard;

        // Resolve any pending pointer interaction (click-to-place caret,
        // double-click word-select, drag-extend) now that the text layout is
        // measurable, before anything borrows the slide immutably.
        self.resolve_text_pointer(tr, chrome);

        // Panels (base layer). A docked panel is part of the base; an overlay
        // flap is painted later, on top of the dimming scrim.
        chrome::fill_rect(scene, t, self.rect, c.surfaces.base);
        if !rail_overlay {
            chrome::fill_rect(scene, t, chrome.rail, c.surfaces.raised);
        }
        if !insp_overlay {
            chrome::fill_rect(scene, t, chrome.inspector, c.surfaces.raised);
        }
        chrome::fill_rect(scene, t, chrome.toolbar, mix_panel(c.surfaces.raised, c.surfaces.base));
        // Divider lines.
        stroke_line(scene, t, chrome.toolbar.x0, chrome.toolbar.y1, chrome.toolbar.x1, chrome.toolbar.y1, div);
        if !rail_overlay {
            stroke_line(scene, t, chrome.rail.x1, chrome.rail.y0, chrome.rail.x1, chrome.rail.y1, div);
        }
        if !insp_overlay {
            stroke_line(scene, t, chrome.inspector.x0, chrome.inspector.y0, chrome.inspector.x0, chrome.inspector.y1, div);
        }

        // Canvas slide.
        if let Some(slide) = self.slide() {
            // Soft drop shadow.
            let sh = chrome.slide_dest + vello::kurbo::Vec2::new(0.0, 6.0);
            chrome::fill_rrect(scene, t, sh.inflate(2.0, 2.0), 14.0, render::with_alpha(Color::BLACK, 70));
            render::draw_slide(scene, t, tr, theme, &pal, slide, chrome.slide_dest, 14.0, &self.images);
            chrome::stroke_rrect(scene, t, chrome.slide_dest, 14.0, render::with_alpha(c.text_primary, 28), 1.0);

            // Selection chrome.
            if let Some(el) = self.selected_element() {
                let er = render::element_dest(chrome.slide_dest, chrome.scale, el);
                let sel_col = pal.accent;
                // Ring is drawn at the handle outset so it passes through the
                // handle centres — the whole selection frame sits *around* the
                // content, never over the first glyph.
                chrome::stroke_rrect(scene, t, er.inflate(HANDLE_OFFSET, HANDLE_OFFSET), 5.0, sel_col, 1.5);
                if !self.editing_text {
                    for (_, hr) in self.handles_of(chrome.slide_dest, chrome.scale, el) {
                        // The returned rect is the generous grab zone (±HANDLE);
                        // paint a smaller visual pip centred on it so the handle
                        // clears the content edge on every side.
                        let vis = hr.inflate(-4.0, -4.0);
                        chrome::fill_rrect(scene, t, vis, 2.5, Color::WHITE);
                        chrome::stroke_rrect(scene, t, vis, 2.5, sel_col, 1.5);
                    }
                } else {
                    self.draw_text_selection(scene, t, tr, &pal, el, er, chrome.scale);
                    self.draw_text_caret(scene, t, tr, &pal, el, er, chrome.scale);
                }
            }

            // Live alignment guides (only while a drag is snapping).
            for g in &self.active_guides {
                let gc = render::with_alpha(pal.accent, 220);
                match g {
                    Guide::Vertical(sx) => {
                        let x = chrome.slide_dest.x0 + sx * chrome.scale;
                        stroke_line(scene, t, x, chrome.slide_dest.y0, x, chrome.slide_dest.y1, gc);
                    }
                    Guide::Horizontal(sy) => {
                        let y = chrome.slide_dest.y0 + sy * chrome.scale;
                        stroke_line(scene, t, chrome.slide_dest.x0, y, chrome.slide_dest.x1, y, gc);
                    }
                }
            }

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

        // Content that lives inside a side panel is deferred to a final pass so
        // an open overlay flap paints on top of the dimming scrim. Membership is
        // geometric: below the toolbar, within the panel's (possibly off-screen
        // / sliding) x-span.
        let in_rail = |center: Point| -> bool {
            center.y >= chrome.rail.y0
                && center.x >= chrome.rail.x0 - 0.5
                && center.x <= chrome.rail.x1 + 0.5
        };
        let in_inspector = |center: Point| -> bool {
            center.y >= chrome.inspector.y0
                && center.x >= chrome.inspector.x0 - 0.5
                && center.x <= chrome.inspector.x1 + 0.5
        };
        let in_panel = |center: Point| in_rail(center) || in_inspector(center);

        // Base-layer buttons + labels (toolbar chrome only; panel content is
        // deferred below).
        for b in &chrome.buttons {
            if !in_panel(b.rect.center()) {
                self.draw_button(scene, t, tr, theme, &pal, b);
            }
        }
        for l in &chrome.labels {
            if !in_panel(Point::new(l.pos.x, l.pos.y)) {
                self.draw_label(scene, t, tr, theme, &pal, l);
            }
        }

        // Inline title rename: an accent underline reads the toolbar title as an
        // editable field; the shared TextInput (drawn in `Widget::draw`) paints
        // the text, caret and selection.
        if self.editing_title {
            let underline_y = chrome.title_pos.y + 22.0;
            stroke_line(scene, t, chrome.title_pos.x, underline_y, chrome.title_rect.x1, underline_y, pal.accent);
        }

        // Scrim behind an open overlay flap, dimming the editor body.
        if chrome.scrim.width() > 0.0 {
            let reveal = chrome.reveal.max(chrome.inspector_reveal);
            let alpha = (reveal * 0.5) as f32;
            chrome::fill_rect(scene, t, chrome.scrim, render::with_alpha(Color::BLACK, (alpha * 255.0) as u8));
        }

        // Rail group on top: panel fill (overlay only), then the fixed header
        // + action buttons, then the scrolling thumbnail list clipped to its
        // viewport so it can never bleed over the pinned Add/Dup/Del row.
        if rail_overlay && chrome.reveal > 0.001 {
            chrome::fill_rect(scene, t, chrome.rail, c.surfaces.raised);
            stroke_line(scene, t, chrome.rail.x1, chrome.rail.y0, chrome.rail.x1, chrome.rail.y1, div);
        }
        // Fixed rail chrome (SLIDES header + Add/Dup/Del) — never scrolls. While
        // a thumbnail is being dragged, the pinned action row is replaced by the
        // red "drop to delete" zone drawn below, so suppress the buttons it
        // covers (otherwise they bleed through the zone's translucent fill).
        let drag_zone =
            self.page_sidebar.is_dragging().then(|| self.slide_delete_zone(chrome.rail));
        for b in &chrome.buttons {
            if in_rail(b.rect.center())
                && !drag_zone.is_some_and(|z| z.contains(b.rect.center()))
            {
                self.draw_button(scene, t, tr, theme, &pal, b);
            }
        }
        for l in &chrome.labels {
            if in_rail(Point::new(l.pos.x, l.pos.y)) {
                self.draw_label(scene, t, tr, theme, &pal, l);
            }
        }
        // Scrolling thumbnail list — the shared PageSidebar paints the
        // thumbnails, selection ring, index numbers, the live reorder ghost +
        // reflow gap, its scrollbar and its own right-click menu, all clipped to
        // the rail viewport it was given in `begin_frame`. It renders each slide
        // through the closure; slides keeps the panel chrome (fill + SLIDES
        // header + Add/Dup/Del) around it. Move the rail out of `self` so the
        // closure can borrow the deck + image cache alongside it.
        if chrome.rail.width() > 1.0 {
            let mut sb = std::mem::replace(&mut self.page_sidebar, PageSidebar::new(SLIDE_H / SLIDE_W));
            let deck = &self.deck;
            let images = &self.images;
            sb.render(scene, t, tr, theme, |scene, tr, base, i, rect| {
                if let Some(slide) = deck.slides.get(i) {
                    render::draw_slide(scene, base, tr, theme, &pal, slide, rect, 6.0, images);
                }
            });
            self.page_sidebar = sb;

            // Drop-to-delete zone: a red dashed target pinned to the bottom of
            // the rail, revealed only while a slide is being dragged. Dropping a
            // thumbnail here removes that slide.
            if self.page_sidebar.is_dragging() {
                let zone = self.slide_delete_zone(chrome.rail);
                let armed = self.over_delete_zone();
                let danger = c.semantic.danger;
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
                let icon_sz = 18.0;
                let label = "Drop to delete";
                let lw = chrome::est_width(label, 12.5);
                let total = icon_sz + 8.0 + lw;
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
                    label,
                    12.5,
                    danger,
                    t * Affine::translate((cx + icon_sz + 8.0, cy - 8.0)),
                    None,
                    TextWeight::Medium,
                );
            }
        }

        // Inspector group on top: panel fill (overlay only), buttons, notes, labels.
        if insp_overlay && chrome.inspector_reveal > 0.001 {
            chrome::fill_rect(scene, t, chrome.inspector, c.surfaces.raised);
            stroke_line(scene, t, chrome.inspector.x0, chrome.inspector.y0, chrome.inspector.x0, chrome.inspector.y1, div);
        }
        for b in &chrome.buttons {
            if in_inspector(b.rect.center()) {
                self.draw_button(scene, t, tr, theme, &pal, b);
            }
        }
        if let (Some(nb), Some(slide)) = (chrome.notes_box, self.slide()) {
            self.draw_notes(scene, t, tr, theme, nb, &slide.notes);
        }
        for l in &chrome.labels {
            if in_inspector(Point::new(l.pos.x, l.pos.y)) {
                self.draw_label(scene, t, tr, theme, &pal, l);
            }
        }

        // Toolbar overflow menu — painted last so it floats above all chrome.
        if let Some(bg) = chrome.menu_bg {
            let shadow = bg + vello::kurbo::Vec2::new(0.0, 4.0);
            chrome::fill_rrect(scene, t, shadow.inflate(2.0, 2.0), 12.0, render::with_alpha(Color::BLACK, 90));
            chrome::fill_rrect(scene, t, bg, 10.0, c.surfaces.raised);
            chrome::stroke_rrect(scene, t, bg, 10.0, render::with_alpha(c.text_primary, 48), 1.0);
            for b in &chrome.menu_buttons {
                self.draw_button(scene, t, tr, theme, &pal, b);
            }
        }
    }

    /// Position the inspector's embedded `ui` widgets (font-size field, fill
    /// picker) from `chrome` so both `draw` and `handle_event` hit-test them.
    fn position_inspector_widgets(&mut self, chrome: &Chrome) {
        if let Some(rect) = chrome.font_input_rect {
            // Reflect the element's size unless the user is mid-edit (so typing
            // isn't clobbered by the CRDT mirror).
            if !self.font_input.is_editing() {
                let fs = self.selected_element().map(|e| e.font_size).unwrap_or(40.0);
                self.font_input.set_value(fs);
            }
            self.font_input.apply_layout_external(rect);
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
            self.font_input.draw(scene, t, tr, theme, presence);
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
            let panel = rect.inflate(12.0, 16.0);
            let shadow = panel + vello::kurbo::Vec2::new(0.0, 5.0);
            chrome::fill_rrect(scene, t, shadow.inflate(2.0, 2.0), 14.0, render::with_alpha(Color::BLACK, 100));
            chrome::fill_rrect(scene, t, panel, 12.0, c.surfaces.raised);
            chrome::stroke_rrect(scene, t, panel, 12.0, render::with_alpha(c.text_primary, 48), 1.0);
            tr.draw_text_weighted(
                scene,
                "FILL COLOUR",
                11.0,
                c.text_dim,
                t * Affine::translate((rect.x0, panel.y0 + 8.0)),
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
        tr.draw_text_weighted(
            scene,
            &l.text,
            l.font,
            col,
            t * Affine::translate((l.pos.x, l.pos.y)),
            None,
            l.weight,
        );
    }

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
        match &b.kind {
            ButtonKind::Text { label, active, danger } => {
                let (bg, fg) = if *danger {
                    (render::with_alpha(c.semantic.danger, 38), c.semantic.danger)
                } else if *active {
                    (pal.accent, Color::WHITE)
                } else {
                    (c.surfaces.base, c.text_primary)
                };
                chrome::fill_rrect(scene, t, b.rect, 8.0, bg);
                chrome::stroke_rrect(scene, t, b.rect, 8.0, render::with_alpha(c.text_primary, 26), 1.0);
                if !label.is_empty() {
                    chrome::text_centered(scene, t, tr, label, b.rect, 13.0, fg, TextWeight::Semibold);
                }
            }
            ButtonKind::IconText { icon, label, accent } => {
                let (bg, fg) = if *accent {
                    (pal.accent, Color::WHITE)
                } else {
                    (c.surfaces.base, c.text_primary)
                };
                chrome::fill_rrect(scene, t, b.rect, 8.0, bg);
                if !accent {
                    chrome::stroke_rrect(scene, t, b.rect, 8.0, render::with_alpha(c.text_primary, 26), 1.0);
                }
                let ir = Rect::new(b.rect.x0 + 11.0, b.rect.y0 + 8.0, b.rect.x0 + 29.0, b.rect.y1 - 8.0);
                draw_icon(scene, t, ir, *icon, fg, 1.8);
                chrome::text_vcenter(scene, t, tr, label, b.rect, ir.x1 + 5.0, 13.0, fg, TextWeight::Semibold);
            }
            ButtonKind::IconOnly { icon, active } => {
                let bg = if *active { pal.accent } else { c.surfaces.base };
                chrome::fill_rrect(scene, t, b.rect, 7.0, bg);
                chrome::stroke_rrect(scene, t, b.rect, 7.0, render::with_alpha(c.text_primary, 30), 1.0);
                let ir = b.rect.inflate(-6.0, -6.0);
                draw_icon(scene, t, ir, *icon, c.text_primary, 1.8);
            }
            ButtonKind::Swatch { role, selected } => {
                match pal.resolve_role(role) {
                    Some(col) => {
                        chrome::fill_rrect(scene, t, b.rect, 6.0, col);
                        // contrast border for light swatches
                        chrome::stroke_rrect(scene, t, b.rect, 6.0, render::with_alpha(Color::BLACK, 50), 1.0);
                    }
                    None => {
                        // "none" → hatched empty box
                        chrome::fill_rrect(scene, t, b.rect, 6.0, c.surfaces.base);
                        chrome::stroke_rrect(scene, t, b.rect, 6.0, render::with_alpha(c.text_primary, 60), 1.0);
                        stroke_line(scene, t, b.rect.x0 + 4.0, b.rect.y1 - 4.0, b.rect.x1 - 4.0, b.rect.y0 + 4.0, c.semantic.danger);
                    }
                }
                if *selected {
                    chrome::stroke_rrect(scene, t, b.rect.inflate(2.5, 2.5), 8.0, pal.accent, 2.0);
                }
            }
            ButtonKind::Palette { kind, selected } => {
                let col = Palette::resolve(*kind, theme).accent;
                chrome::fill_rrect(scene, t, b.rect, 8.0, c.surfaces.base);
                chrome::stroke_rrect(
                    scene,
                    t,
                    b.rect,
                    8.0,
                    if *selected { col } else { render::with_alpha(c.text_primary, 26) },
                    if *selected { 2.0 } else { 1.0 },
                );
                let dot = Rect::new(b.rect.x0 + 8.0, b.rect.center().y - 6.0, b.rect.x0 + 20.0, b.rect.center().y + 6.0);
                chrome::fill_rrect(scene, t, dot, 6.0, col);
                if b.rect.width() > 40.0 {
                    // Clip the name to the pill so a slight text-metric
                    // over-run can never bleed onto the next pill. Pills are
                    // sized (see `palette_pill_width`) so the full name fits,
                    // making this a belt-and-suspenders guard, not a truncation.
                    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &b.rect);
                    chrome::text_vcenter(scene, t, tr, kind.label(), b.rect, dot.x1 + 6.0, 12.0, c.text_primary, TextWeight::Semibold);
                    scene.pop_layer();
                }
            }
            ButtonKind::Segment { label, active, pos } => {
                let (bg, fg) = if *active {
                    (pal.accent, Color::WHITE)
                } else {
                    (c.surfaces.base, c.text_primary)
                };
                // Round only the outer corners so the row reads as one pill.
                let r = 8.0;
                let shape = match pos {
                    SegPos::First => vello::kurbo::RoundedRect::new(
                        b.rect.x0, b.rect.y0, b.rect.x1, b.rect.y1,
                        (r, 0.0, 0.0, r),
                    ),
                    SegPos::Last => vello::kurbo::RoundedRect::new(
                        b.rect.x0, b.rect.y0, b.rect.x1, b.rect.y1,
                        (0.0, r, r, 0.0),
                    ),
                    SegPos::Mid => vello::kurbo::RoundedRect::from_rect(b.rect, 0.0),
                };
                scene.fill(Fill::NonZero, t, bg, None, &shape);
                scene.stroke(&Stroke::new(1.0), t, render::with_alpha(c.text_primary, 26), None, &shape);
                chrome::text_centered(scene, t, tr, label, b.rect, 12.5, fg, TextWeight::Semibold);
            }
            ButtonKind::Hamburger => {
                let active = self.rail_open();
                let bg = if active { pal.accent } else { c.surfaces.base };
                let fg = if active { Color::WHITE } else { c.text_primary };
                chrome::fill_rrect(scene, t, b.rect, 8.0, bg);
                chrome::stroke_rrect(scene, t, b.rect, 8.0, render::with_alpha(c.text_primary, 30), 1.0);
                // Three evenly-spaced horizontal lines.
                let cx0 = b.rect.x0 + 9.0;
                let cx1 = b.rect.x1 - 9.0;
                let cy = b.rect.center().y;
                for dy in [-5.0, 0.0, 5.0] {
                    stroke_line(scene, t, cx0, cy + dy, cx1, cy + dy, fg);
                }
            }
        }
    }

    fn draw_text_caret(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        pal: &Palette,
        el: &Element,
        er: Rect,
        s: f64,
    ) {
        let font_px = (el.font_size * s).max(1.0) as f32;
        let weight = if el.bold { TextWeight::Bold } else { TextWeight::Regular };
        self.draw_caret_in_box(
            scene, t, tr, pal.accent, &el.text, self.caret, er, font_px, weight, el.italic,
            el.align, true,
        );
    }

    /// Paint a vertical caret over one line of text inside `er`. The caret
    /// tracks the same layout the text is painted with (line height `font_px *
    /// 1.25`, per-line `align` offset, optional vertical centering), so it lands
    /// correctly on centre- and right-aligned boxes — not just left-anchored
    /// ones. `caret` is a char index into `text`.
    #[allow(clippy::too_many_arguments)]
    fn draw_caret_in_box(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        accent: Color,
        text: &str,
        caret: usize,
        er: Rect,
        font_px: f32,
        weight: TextWeight,
        italic: bool,
        align: TextAlign,
        center_v: bool,
    ) {
        let (top, line_h) =
            self.caret_geometry(tr, text, caret, er, font_px, weight, italic, align, center_v);
        let bar = Line::new(Point::new(top.x, top.y + 2.0), Point::new(top.x, top.y + line_h - 2.0));
        scene.stroke(&Stroke::new(2.0), t, accent, None, &bar);
    }

    /// Pure geometry for [`Self::draw_caret_in_box`]: the caret bar's top point
    /// and the line height, tracking the same layout the painter uses (line
    /// height `font_px * 1.25`, per-line `align` offset, optional vertical
    /// centering). Split out so the placement can be asserted directly.
    #[allow(clippy::too_many_arguments)]
    fn caret_geometry(
        &self,
        tr: &mut TextRenderer,
        text: &str,
        caret: usize,
        er: Rect,
        font_px: f32,
        weight: TextWeight,
        italic: bool,
        align: TextAlign,
        center_v: bool,
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
        let line_start = starts[li];
        let line_end = if li + 1 < starts.len() { starts[li + 1] - 1 } else { chars.len() };
        let cur_line: String = chars[line_start..line_end].iter().collect();
        let prefix: String = chars[line_start..caret].iter().collect();

        let line_h = (font_px as f64) * 1.25;
        let wrap_w = (er.width() + 4.0).max(1.0) as f32;
        // Total painted height (wrapped rows across explicit lines) mirrors
        // `render::draw_text_element`, so the caret shares its baselines.
        let rows: Vec<usize> = text
            .split('\n')
            .map(|l| {
                if l.is_empty() {
                    1
                } else {
                    let (_, h) = tr.measure_styled(l, font_px, Some(wrap_w), weight, italic, false);
                    ((h as f64 / line_h).round() as usize).max(1)
                }
            })
            .collect();
        let total_h = rows.iter().sum::<usize>() as f64 * line_h;
        let block_top = if center_v && total_h < er.height() {
            er.y0 + (er.height() - total_h) / 2.0
        } else {
            er.y0
        };
        let rows_before: usize = rows.iter().take(li).sum();
        let cy = block_top + rows_before as f64 * line_h;

        // Horizontal: offset the line by its own width per its alignment (as the
        // painter does), then advance past the prefix to the caret.
        let (lw, _) = tr.measure_styled(&cur_line, font_px, Some(wrap_w), weight, italic, false);
        let (pw, _) = tr.measure_styled(&prefix, font_px, None, weight, italic, false);
        let line_x = match align {
            TextAlign::Left | TextAlign::Justify => er.x0,
            TextAlign::Center => er.x0 + (er.width() - lw as f64) / 2.0,
            TextAlign::Right => er.x1 - lw as f64,
        };
        let cx = line_x + pw as f64;
        (Point::new(cx, cy), line_h)
    }

    /// Inverse of [`Self::caret_geometry`]: the char index whose caret sits
    /// nearest `p` in the same wrapped layout, for click-to-position and
    /// drag-select. Scans candidate carets and picks the closest, weighting a
    /// matching visual line over horizontal distance.
    #[allow(clippy::too_many_arguments)]
    fn char_at_point_in_box(
        &self,
        tr: &mut TextRenderer,
        text: &str,
        er: Rect,
        font_px: f32,
        weight: TextWeight,
        italic: bool,
        align: TextAlign,
        center_v: bool,
        p: Point,
    ) -> usize {
        let len = text.chars().count();
        let mut best = 0usize;
        let mut best_d = f64::INFINITY;
        for i in 0..=len {
            let (pt, line_h) =
                self.caret_geometry(tr, text, i, er, font_px, weight, italic, align, center_v);
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
    #[allow(clippy::too_many_arguments)]
    fn selection_rects_in_box(
        &self,
        tr: &mut TextRenderer,
        text: &str,
        min: usize,
        max: usize,
        er: Rect,
        font_px: f32,
        weight: TextWeight,
        italic: bool,
        align: TextAlign,
        center_v: bool,
    ) -> Vec<Rect> {
        if min >= max {
            return Vec::new();
        }
        let chars: Vec<char> = text.chars().collect();
        let starts = line_starts(&chars);
        let mut rects = Vec::new();
        for (li, &ls) in starts.iter().enumerate() {
            let le = if li + 1 < starts.len() { starts[li + 1] - 1 } else { chars.len() };
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
            let (pa, lh) =
                self.caret_geometry(tr, text, a, er, font_px, weight, italic, align, center_v);
            let (pb, _) =
                self.caret_geometry(tr, text, b, er, font_px, weight, italic, align, center_v);
            let x0 = pa.x.min(pb.x);
            let mut x1 = pa.x.max(pb.x);
            if covers_break {
                x1 = x1.max(x0) + font_px as f64 * 0.35;
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
        let font_px = (el.font_size * chrome.scale).max(1.0) as f32;
        let weight = if el.bold { TextWeight::Bold } else { TextWeight::Regular };
        if let Some(p) = self.select_word_click.take() {
            let idx = self.char_at_point_in_box(
                tr, &el.text, er, font_px, weight, el.italic, el.align, true, p,
            );
            let (a, b) = word_boundaries_chars(&el.text, idx);
            self.sel_anchor = a;
            self.caret = b;
        } else if let Some((p, extend)) = self.caret_click.take() {
            let idx = self.char_at_point_in_box(
                tr, &el.text, er, font_px, weight, el.italic, el.align, true, p,
            );
            self.place_caret(idx, extend);
        }
    }

    /// Paint the on-canvas text selection highlight for the editing element.
    fn draw_text_selection(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        pal: &Palette,
        el: &Element,
        er: Rect,
        s: f64,
    ) {
        let sel = self.selection();
        if sel.is_collapsed() {
            return;
        }
        let font_px = (el.font_size * s).max(1.0) as f32;
        let weight = if el.bold { TextWeight::Bold } else { TextWeight::Regular };
        let col = render::with_alpha(pal.accent, 70);
        for r in self.selection_rects_in_box(
            tr, &el.text, sel.min(), sel.max(), er, font_px, weight, el.italic, el.align, true,
        ) {
            chrome::fill_rrect(scene, t, r, 2.0, col);
        }
    }

    fn draw_notes(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        nb: Rect,
        notes: &str,
    ) {
        let c = &theme.tokens.colors;
        let bg = if self.editing_notes {
            render::with_alpha(self.palette(theme).accent, 28)
        } else {
            c.surfaces.base
        };
        chrome::fill_rrect(scene, t, nb, 8.0, bg);
        chrome::stroke_rrect(
            scene,
            t,
            nb,
            8.0,
            if self.editing_notes { self.palette(theme).accent } else { render::with_alpha(c.text_primary, 26) },
            1.0,
        );
        let display = if notes.is_empty() {
            "Click to add speaker notes…".to_string()
        } else {
            notes.to_string()
        };
        let col = if notes.is_empty() { c.text_dim } else { c.text_primary };
        tr.draw_text_weighted(
            scene,
            &display,
            12.5,
            col,
            t * Affine::translate((nb.x0 + 10.0, nb.y0 + 9.0)),
            Some((nb.width() - 20.0) as f32),
            TextWeight::Regular,
        );
        // Blinking-free caret while the notes field is focused. The text area's
        // width is sized so the caret helper's wrap width matches the draw call
        // above (`er.width() + 4 == nb.width() - 20`), and notes are top-anchored
        // and left-aligned.
        if self.editing_notes {
            let er = Rect::new(nb.x0 + 10.0, nb.y0 + 9.0, nb.x0 + 10.0 + (nb.width() - 24.0), nb.y1 - 9.0);
            // Selection highlight (same shared caret / selection core as the
            // canvas text) behind the caret.
            let sel = self.selection();
            if !sel.is_collapsed() {
                let hl = render::with_alpha(self.palette(theme).accent, 70);
                for r in self.selection_rects_in_box(
                    tr, notes, sel.min(), sel.max(), er, 12.5, TextWeight::Regular, false,
                    TextAlign::Left, false,
                ) {
                    chrome::fill_rrect(scene, t, r, 2.0, hl);
                }
            }
            self.draw_caret_in_box(
                scene,
                t,
                tr,
                self.palette(theme).accent,
                notes,
                self.notes_caret,
                er,
                12.5,
                TextWeight::Regular,
                false,
                TextAlign::Left,
                false,
            );
        }
    }

    fn draw_present(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        // Presenter dashboard takes over the whole window when toggled on.
        if self.presenter_view {
            self.draw_presenter(scene, t, tr, theme);
            return;
        }

        let pal = self.palette(theme);
        // Black backdrop.
        chrome::fill_rect(scene, t, self.rect, Color::BLACK);

        // Blanked screen (black or white): nothing but a faint resume hint.
        if self.blank.is_blanked() {
            let (bg, fg, msg) = match self.blank {
                Blank::White => (
                    Color::WHITE,
                    render::with_alpha(Color::BLACK, 120),
                    "Screen whited out  ·  press W to resume",
                ),
                _ => (
                    Color::BLACK,
                    render::with_alpha(Color::WHITE, 120),
                    "Screen blacked out  ·  press B to resume",
                ),
            };
            chrome::fill_rect(scene, t, self.rect, bg);
            tr.draw_text_weighted(
                scene,
                msg,
                14.0,
                fg,
                t * Affine::translate((self.rect.x0 + 40.0, self.rect.y1 - 40.0)),
                None,
                TextWeight::Regular,
            );
            return;
        }

        // Letterbox the current slide.
        let (area, notes_panel) = if self.show_notes {
            let split = self.rect.y0 + self.rect.height() * 0.66;
            (
                Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, split),
                Some(Rect::new(self.rect.x0, split, self.rect.x1, self.rect.y1)),
            )
        } else {
            (self.rect, None)
        };
        let pad = 24.0;
        let aw = area.width() - 2.0 * pad;
        let ah = area.height() - 2.0 * pad;
        let s = (aw / SLIDE_W).min(ah / SLIDE_H);
        let sw = SLIDE_W * s;
        let sh = SLIDE_H * s;
        let sx = area.x0 + (area.width() - sw) / 2.0;
        let sy = area.y0 + (area.height() - sh) / 2.0;
        let dest = Rect::new(sx, sy, sx + sw, sy + sh);
        // Cross-fade: paint the outgoing slide first, then the incoming
        // one inside an alpha layer that ramps in.
        let alpha = self.transition_alpha();
        if let Some((from, _)) = self.transition {
            if alpha < 1.0 {
                if let Some(prev) = self.deck.slides.get(from) {
                    render::draw_slide(scene, t, tr, theme, &pal, prev, dest, 0.0, &self.images);
                }
            }
        }
        if alpha < 1.0 {
            // Mid cross-fade → paint live inside the alpha layer (it animates,
            // so caching would gain nothing and the layer transform matters).
            if let Some(slide) = self.slide() {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    alpha,
                    t,
                    &dest,
                );
                render::draw_slide(scene, t, tr, theme, &pal, slide, dest, 0.0, &self.images);
                scene.pop_layer();
            }
        } else {
            // Settled → the slide is static between navigations. Shape it once
            // into a detached scene, then just re-append it each frame so the
            // speaker clock ticks without re-shaping every element every vsync.
            self.append_present_slide(scene, t, tr, theme, &pal, dest);
        }

        // Progress bar pinned to the very bottom edge of the screen.
        let n = self.deck.slides.len().max(1);
        let frac = (self.cur_slide + 1) as f64 / n as f64;
        let bar_h = 4.0;
        let track = Rect::new(self.rect.x0, self.rect.y1 - bar_h, self.rect.x1, self.rect.y1);
        chrome::fill_rect(scene, t, track, render::with_alpha(Color::WHITE, 30));
        let fill = Rect::new(track.x0, track.y0, track.x0 + track.width() * frac, track.y1);
        chrome::fill_rect(scene, t, fill, pal.accent);

        // Elapsed-time pill, top-right. A leading ⏸ glyph + dimmer fill marks
        // a paused timer.
        let paused = self.timer_paused();
        let clock = if paused {
            format!("II  {}", self.present_clock())
        } else {
            self.present_clock()
        };
        let tw = chrome::est_width(&clock, 13.0) + 26.0;
        let tr_rect = Rect::new(self.rect.x1 - 20.0 - tw, self.rect.y0 + 16.0, self.rect.x1 - 20.0, self.rect.y0 + 16.0 + 28.0);
        chrome::fill_rrect(scene, t, tr_rect, 14.0, render::with_alpha(Color::WHITE, if paused { 14 } else { 24 }));
        chrome::text_centered(scene, t, tr, &clock, tr_rect, 13.0, render::with_alpha(Color::WHITE, if paused { 150 } else { 255 }), TextWeight::Semibold);

        // Slide counter pill + hint, in the gutter below the slide — only
        // when there's clear room (suppressed when the notes panel is up).
        if dest.y1 + 40.0 < area.y1 {
            let counter = format!("{} / {}", self.cur_slide + 1, self.deck.slides.len());
            let cw = chrome::est_width(&counter, 13.0) + 24.0;
            let cr = Rect::new(dest.x1 - cw, dest.y1 + 8.0, dest.x1, dest.y1 + 8.0 + 26.0);
            chrome::fill_rrect(scene, t, cr, 13.0, render::with_alpha(Color::WHITE, 26));
            chrome::text_centered(scene, t, tr, &counter, cr, 13.0, Color::WHITE, TextWeight::Semibold);
            // The nav hint fills the gutter to the *left* of the counter pill,
            // capped so it can never paint under the pill (they overlapped on
            // narrow widths). Pick the richest variant that still fits the room;
            // if even the short one won't, drop it and keep just the counter.
            let hint_x = dest.x0;
            let hint_max = (cr.x0 - 16.0 - hint_x).max(0.0);
            let full = "← →  navigate    Home / End  first · last    P  presenter    S  notes    B / W  blank    Esc  exit";
            let medium = "← →  navigate    P  presenter    S  notes    Esc  exit";
            let short = "← →  navigate    Esc  exit";
            if let Some(hint) = [full, medium, short]
                .into_iter()
                .find(|h| chrome::est_width(h, 12.5) <= hint_max)
            {
                tr.draw_text_weighted(
                    scene,
                    hint,
                    12.5,
                    render::with_alpha(Color::WHITE, 150),
                    t * Affine::translate((hint_x, dest.y1 + 14.0)),
                    Some(hint_max as f32),
                    TextWeight::Regular,
                );
            }
        }

        // Speaker-notes panel.
        if let Some(np) = notes_panel {
            chrome::fill_rect(scene, t, np, theme.tokens.colors.surfaces.raised);
            stroke_line(scene, t, np.x0, np.y0, np.x1, np.y0, theme.tokens.colors.border_standard);
            let nx = np.x0 + 40.0;
            tr.draw_text_weighted(
                scene,
                "SPEAKER NOTES",
                12.0,
                pal.accent,
                t * Affine::translate((nx, np.y0 + 20.0)),
                None,
                TextWeight::Semibold,
            );
            // The NEXT preview occupies the right third; reserve its left edge.
            let pw = np.width() * 0.26;
            let preview_x0 = np.x1 - 40.0 - pw;
            // Big elapsed clock, right-aligned just left of the preview.
            let clock = self.present_clock();
            let (clock_w, _) = tr.measure_weighted(&clock, 30.0, None, TextWeight::Bold);
            tr.draw_text_weighted(
                scene,
                &clock,
                30.0,
                theme.tokens.colors.text_primary,
                t * Affine::translate((preview_x0 - 36.0 - clock_w as f64, np.y0 + 16.0)),
                None,
                TextWeight::Bold,
            );
            let notes = self.slide().map(|s| s.notes.clone()).unwrap_or_default();
            let body = if notes.is_empty() { "—".to_string() } else { notes };
            tr.draw_text_weighted(
                scene,
                &body,
                18.0,
                theme.tokens.colors.text_primary,
                t * Affine::translate((nx, np.y0 + 44.0)),
                Some((np.width() * 0.6) as f32),
                TextWeight::Regular,
            );
            // Next slide preview.
            if let Some(next) = self.deck.slides.get(self.cur_slide + 1) {
                let ph = pw * SLIDE_H / SLIDE_W;
                let pr = Rect::new(preview_x0, np.y0 + 28.0, np.x1 - 40.0, np.y0 + 28.0 + ph);
                tr.draw_text_weighted(
                    scene,
                    "NEXT",
                    11.0,
                    theme.tokens.colors.text_dim,
                    t * Affine::translate((pr.x0, pr.y0 - 18.0)),
                    None,
                    TextWeight::Semibold,
                );
                render::draw_slide(scene, t, tr, theme, &pal, next, pr, 6.0, &self.images);
                chrome::stroke_rrect(scene, t, pr, 6.0, render::with_alpha(Color::WHITE, 40), 1.0);
            }
        }
    }

    /// Append the settled audience slide, (re)shaping it into a detached scene
    /// only when the slide, size, theme or deck actually changed. Between
    /// navigations this is a cheap scene-append, so present mode can keep the
    /// speaker clock ticking without re-shaping every element every vsync.
    fn append_present_slide(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pal: &Palette,
        dest: Rect,
    ) {
        let slide_idx = self.cur_slide;
        let rev = self.deck_rev;
        let palette = self.deck.palette;
        let stale = self
            .present_cache
            .as_ref()
            .map(|c| !c.matches(dest, slide_idx, rev, palette))
            .unwrap_or(true);
        if stale {
            // Shape into a detached scene at IDENTITY (absolute slide coords);
            // the live transform is applied on append below.
            let mut frag = Scene::new();
            if let Some(slide) = self.deck.slides.get(slide_idx) {
                render::draw_slide(&mut frag, Affine::IDENTITY, tr, theme, pal, slide, dest, 0.0, &self.images);
            }
            self.present_cache = Some(PresentCache {
                slide: slide_idx,
                rev,
                palette,
                dest: (
                    dest.x0.round() as i64,
                    dest.y0.round() as i64,
                    dest.x1.round() as i64,
                    dest.y1.round() as i64,
                ),
                scene: frag,
            });
        }
        if let Some(c) = &self.present_cache {
            scene.append(&c.scene, Some(t));
        }
    }

    // --- presenter dashboard -----------------------------------------------

    /// The presenter's private dashboard, shown on the presenter's monitor
    /// (or, single-monitor, as a togglable full-window view). Layout:
    ///
    /// ```text
    /// ┌──────────────────────────── header: title · position · timer ──┐
    /// │ ┌──────────────────────────┐  ┌───────────────────────────────┐│
    /// │ │  CURRENT (large preview) │  │ NEXT (thumbnail)              ││
    /// │ │                          │  │                               ││
    /// │ │                          │  ├───────────────────────────────┤│
    /// │ │                          │  │ SPEAKER NOTES (scrolling body)││
    /// │ └──────────────────────────┘  └───────────────────────────────┘│
    /// └──── footer hint: ← → navigate · P slide · Space pause · …────────┘
    /// ```
    fn draw_presenter(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let pal = self.palette(theme);
        let c = &theme.tokens.colors;
        // Deep neutral backdrop so it reads as "behind the scenes", distinct
        // from the black audience field.
        chrome::fill_rect(scene, t, self.rect, c.surfaces.base);

        let pad = 28.0;
        let header_h = 60.0;
        let footer_h = 40.0;
        let body = Rect::new(
            self.rect.x0 + pad,
            self.rect.y0 + header_h,
            self.rect.x1 - pad,
            self.rect.y1 - footer_h,
        );

        // --- header: deck title (left) · position + timer (right) ----------
        tr.draw_text_weighted(
            scene,
            &self.deck.title,
            18.0,
            c.text_primary,
            t * Affine::translate((self.rect.x0 + pad, self.rect.y0 + 18.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text_weighted(
            scene,
            "PRESENTER VIEW",
            11.0,
            pal.accent,
            t * Affine::translate((self.rect.x0 + pad, self.rect.y0 + 40.0)),
            None,
            TextWeight::Semibold,
        );

        // Big timer pill, top-right, with position beneath.
        let paused = self.timer_paused();
        let clock = self.present_clock();
        let (clock_w, _) = tr.measure_weighted(&clock, 34.0, None, TextWeight::Bold);
        let clock_x = self.rect.x1 - pad - clock_w as f64;
        tr.draw_text_weighted(
            scene,
            &clock,
            34.0,
            if paused { c.text_dim } else { c.text_primary },
            t * Affine::translate((clock_x, self.rect.y0 + 12.0)),
            None,
            TextWeight::Bold,
        );
        let pos = format!(
            "{}  ·  slide {} / {}",
            if paused { "PAUSED" } else { "ELAPSED" },
            self.cur_slide + 1,
            self.deck.slides.len()
        );
        let (pos_w, _) = tr.measure_weighted(&pos, 12.0, None, TextWeight::Semibold);
        tr.draw_text_weighted(
            scene,
            &pos,
            12.0,
            c.text_dim,
            t * Affine::translate((self.rect.x1 - pad - pos_w as f64, self.rect.y0 + 44.0)),
            None,
            TextWeight::Semibold,
        );

        // --- body: current (≈62%) | right column (next + notes) -----------
        let gap = 24.0;
        let left_w = (body.width() * 0.62).min(body.width() - 320.0);
        let cur_area = Rect::new(body.x0, body.y0, body.x0 + left_w, body.y1);
        let right = Rect::new(cur_area.x1 + gap, body.y0, body.x1, body.y1);

        // CURRENT label + large slide preview (fit 16:9 within cur_area, room
        // for the label above).
        tr.draw_text_weighted(
            scene,
            "CURRENT",
            11.0,
            c.text_dim,
            t * Affine::translate((cur_area.x0, cur_area.y0)),
            None,
            TextWeight::Semibold,
        );
        let cur_top = cur_area.y0 + 20.0;
        let cur_avail = Rect::new(cur_area.x0, cur_top, cur_area.x1, cur_area.y1);
        // Top-anchor the large current preview so it sits just under its label
        // rather than floating in the middle of a tall column.
        let cur_dest = fit_slide_top(cur_avail);
        // Drop shadow + slide.
        let sh = cur_dest + vello::kurbo::Vec2::new(0.0, 6.0);
        chrome::fill_rrect(scene, t, sh.inflate(2.0, 2.0), 12.0, render::with_alpha(Color::BLACK, 80));
        if let Some(slide) = self.slide() {
            render::draw_slide(scene, t, tr, theme, &pal, slide, cur_dest, 12.0, &self.images);
        }
        chrome::stroke_rrect(scene, t, cur_dest, 12.0, render::with_alpha(c.text_primary, 30), 1.0);

        // RIGHT column: NEXT preview (top), then speaker notes fill the rest.
        // NEXT preview sized to the column width.
        tr.draw_text_weighted(
            scene,
            "NEXT",
            11.0,
            c.text_dim,
            t * Affine::translate((right.x0, right.y0)),
            None,
            TextWeight::Semibold,
        );
        let next_top = right.y0 + 20.0;
        let next_w = right.width();
        let next_h = next_w * SLIDE_H / SLIDE_W;
        let next_rect = Rect::new(right.x0, next_top, right.x0 + next_w, next_top + next_h);
        match self.deck.slides.get(self.cur_slide + 1) {
            Some(next) => {
                render::draw_slide(scene, t, tr, theme, &pal, next, next_rect, 8.0, &self.images);
                chrome::stroke_rrect(scene, t, next_rect, 8.0, render::with_alpha(c.text_primary, 30), 1.0);
            }
            None => {
                // End of deck — placeholder card.
                chrome::fill_rrect(scene, t, next_rect, 8.0, c.surfaces.raised);
                chrome::stroke_rrect(scene, t, next_rect, 8.0, render::with_alpha(c.text_primary, 24), 1.0);
                chrome::text_centered(
                    scene, t, tr, "End of deck", next_rect, 14.0, c.text_dim, TextWeight::Semibold,
                );
            }
        }

        // Speaker notes panel beneath NEXT.
        let notes_top = next_rect.y1 + 22.0;
        tr.draw_text_weighted(
            scene,
            "SPEAKER NOTES",
            11.0,
            c.text_dim,
            t * Affine::translate((right.x0, notes_top)),
            None,
            TextWeight::Semibold,
        );
        let nb = Rect::new(right.x0, notes_top + 20.0, right.x1, body.y1);
        chrome::fill_rrect(scene, t, nb, 10.0, c.surfaces.raised);
        chrome::stroke_rrect(scene, t, nb, 10.0, render::with_alpha(c.text_primary, 22), 1.0);
        let notes = self.slide().map(|s| s.notes.clone()).unwrap_or_default();
        let (body_txt, col) = if notes.is_empty() {
            ("No notes for this slide.".to_string(), c.text_dim)
        } else {
            (notes, c.text_primary)
        };
        tr.draw_text_weighted(
            scene,
            &body_txt,
            16.0,
            col,
            t * Affine::translate((nb.x0 + 16.0, nb.y0 + 14.0)),
            Some((nb.width() - 32.0) as f32),
            TextWeight::Regular,
        );

        // --- footer hint ---------------------------------------------------
        tr.draw_text_weighted(
            scene,
            "← →  navigate    Home / End  first · last    P  hide dashboard    K  pause timer    0  reset    Esc  exit",
            12.0,
            c.text_dim,
            t * Affine::translate((self.rect.x0 + pad, self.rect.y1 - 26.0)),
            None,
            TextWeight::Regular,
        );

        // Progress bar pinned to the very bottom edge.
        let n = self.deck.slides.len().max(1);
        let frac = (self.cur_slide + 1) as f64 / n as f64;
        let track = Rect::new(self.rect.x0, self.rect.y1 - 3.0, self.rect.x1, self.rect.y1);
        chrome::fill_rect(scene, t, track, render::with_alpha(c.text_primary, 26));
        let fillr = Rect::new(track.x0, track.y0, track.x0 + track.width() * frac, track.y1);
        chrome::fill_rect(scene, t, fillr, pal.accent);
    }

    // --- overview (slide-sorter) -------------------------------------------

    /// Lay out the slide grid: as many columns as fit, returning the scroll
    /// viewport, each slide's index + destination rect (already offset by the
    /// clamped vertical scroll), and the maximum scroll. Shared by draw,
    /// hit-test and the wheel/scrollbar handlers so they always agree.
    fn compute_overview(&self) -> (Rect, Vec<(usize, Rect)>, f64) {
        let header_h = 64.0;
        let area = Rect::new(self.rect.x0, self.rect.y0 + header_h, self.rect.x1, self.rect.y1);
        let pad = 36.0;
        let gap = 26.0;
        let avail_w = (area.width() - 2.0 * pad).max(200.0);
        // Aim for a thumbnail ~250px wide, then fit an integer column count.
        let target = 260.0;
        let cols = ((avail_w + gap) / (target + gap)).floor().max(1.0) as usize;
        let tw = (avail_w - (cols as f64 - 1.0) * gap) / cols as f64;
        let th = tw * SLIDE_H / SLIDE_W;
        let n = self.deck.slides.len();
        // Each cell is the slide card plus a caption row beneath it.
        let cell_h = th + 30.0;
        let row_stride = cell_h + gap;
        let rows = if n == 0 { 0 } else { n.div_ceil(cols) };
        // Content height from the top of `area`: top pad + all rows + bottom pad.
        let content_h = if rows == 0 {
            0.0
        } else {
            pad + rows as f64 * cell_h + (rows as f64 - 1.0) * gap + pad
        };
        let max_scroll = (content_h - area.height()).max(0.0);
        let scroll = self.overview_scroll.clamp(0.0, max_scroll);
        let mut out = Vec::new();
        for i in 0..n {
            let col = i % cols;
            let row = i / cols;
            let x = area.x0 + pad + col as f64 * (tw + gap);
            let y = area.y0 + pad + row as f64 * row_stride - scroll;
            out.push((i, Rect::new(x, y, x + tw, y + th)));
        }
        (area, out, max_scroll)
    }

    fn draw_overview(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let pal = self.palette(theme);
        let c = &theme.tokens.colors;
        chrome::fill_rect(scene, t, self.rect, c.surfaces.base);

        // Header row.
        tr.draw_text_weighted(
            scene,
            &self.deck.title,
            18.0,
            c.text_primary,
            t * Affine::translate((self.rect.x0 + 36.0, self.rect.y0 + 16.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text_weighted(
            scene,
            &format!("{} slides  ·  click to open  ·  Esc / G to return", self.deck.slides.len()),
            12.0,
            c.text_dim,
            t * Affine::translate((self.rect.x0 + 36.0, self.rect.y0 + 38.0)),
            None,
            TextWeight::Regular,
        );
        stroke_line(scene, t, self.rect.x0, self.rect.y0 + 60.0, self.rect.x1, self.rect.y0 + 60.0, c.border_standard);

        let (area, thumbs, max_scroll) = self.compute_overview();
        // Clip the scrolling grid to the area below the header so cards never
        // paint over the title row as they scroll past.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &area);
        for (i, rect) in thumbs {
            if let Some(slide) = self.deck.slides.get(i) {
                // Drop shadow + card.
                let sh = rect + vello::kurbo::Vec2::new(0.0, 4.0);
                chrome::fill_rrect(scene, t, sh.inflate(1.0, 1.0), 8.0, render::with_alpha(Color::BLACK, 60));
                render::draw_slide(scene, t, tr, theme, &pal, slide, rect, 8.0, &self.images);
                let (ring, w) = if i == self.cur_slide {
                    (pal.accent, 2.5)
                } else {
                    (render::with_alpha(c.text_primary, 30), 1.0)
                };
                chrome::stroke_rrect(scene, t, rect, 8.0, ring, w);
                // Caption row *beneath* the card: a slide-number chip then the
                // layout name. Kept below the card so it never collides with the
                // slide's own title/content (an on-card badge overlapped it).
                let cap_y = rect.y1 + 7.0;
                let num = format!("{}", i + 1);
                let num_col = if i == self.cur_slide { pal.accent } else { c.text_primary };
                tr.draw_text_weighted(
                    scene,
                    &num,
                    12.0,
                    num_col,
                    t * Affine::translate((rect.x0 + 2.0, cap_y)),
                    None,
                    TextWeight::Bold,
                );
                let (num_w, _) = tr.measure_weighted(&num, 12.0, None, TextWeight::Bold);
                tr.draw_text_weighted(
                    scene,
                    slide.layout.label(),
                    11.5,
                    c.text_dim,
                    t * Affine::translate((rect.x0 + num_w as f64 + 12.0, cap_y + 0.5)),
                    None,
                    TextWeight::Regular,
                );
            }
        }
        scene.pop_layer();
        // Scrollbar when the grid overflows the viewport.
        if let Some((track, thumb)) =
            scrollbar_rects(area, self.overview_scroll.clamp(0.0, max_scroll), max_scroll)
        {
            chrome::fill_rrect(scene, t, track, 3.0, render::with_alpha(c.text_primary, 18));
            chrome::fill_rrect(scene, t, thumb, 3.0, render::with_alpha(c.text_primary, 90));
        }
    }

    // --- keyboard-shortcuts overlay ----------------------------------------

    /// Whether the keyboard-shortcuts overlay should paint this frame. It is
    /// shown in **every** mode (including Present): suppressing it in Present
    /// while `on_key` still routed keys to it would soft-lock navigation.
    fn help_overlay_visible(&self) -> bool {
        self.show_help
    }

    fn draw_help(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let pal = self.palette(theme);
        // Scrim.
        chrome::fill_rect(scene, t, self.rect, render::with_alpha(Color::BLACK, 150));
        // Centered card.
        let cw = 580.0_f64.min(self.rect.width() - 80.0);
        let ch = 588.0_f64.min(self.rect.height() - 60.0);
        let cx = self.rect.x0 + (self.rect.width() - cw) / 2.0;
        let cy = self.rect.y0 + (self.rect.height() - ch) / 2.0;
        let card = Rect::new(cx, cy, cx + cw, cy + ch);
        chrome::fill_rrect(scene, t, card, 16.0, c.surfaces.raised);
        chrome::stroke_rrect(scene, t, card, 16.0, render::with_alpha(c.text_primary, 30), 1.0);

        let px = card.x0 + 32.0;
        tr.draw_text_weighted(
            scene,
            "Keyboard shortcuts",
            20.0,
            c.text_primary,
            t * Affine::translate((px, card.y0 + 24.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text_weighted(
            scene,
            "Press ?  or Esc to close",
            12.0,
            c.text_dim,
            t * Affine::translate((px, card.y0 + 50.0)),
            None,
            TextWeight::Regular,
        );

        let rows: [(&str, &str); 20] = [
            ("N", "New slide"),
            ("Click title", "Rename the deck"),
            ("P", "Present from current slide"),
            ("G", "Slide-sorter overview"),
            ("T / R / O / L", "Insert text · rect · ellipse · line"),
            ("Tab", "Cycle selection on the slide"),
            ("Enter", "Edit selected text box"),
            ("Drag", "Move · resize (snaps to guides)"),
            ("Arrows", "Nudge selection (Shift = 1px)"),
            ("[  /  ]", "Send back · bring to front"),
            ("Ctrl-D", "Duplicate element"),
            ("Del / Backspace", "Delete element"),
            ("Ctrl-Z / Ctrl-Y", "Undo · redo"),
            ("Present mode", ""),
            ("← → / Space / click", "Previous · next slide"),
            ("Home / End / 1-9", "First · last · jump to slide N"),
            ("P", "Toggle presenter dashboard"),
            ("S  ·  B / W", "Speaker notes · black / white out"),
            ("K  ·  0", "Pause timer · reset timer"),
            ("?", "Toggle this help"),
        ];
        let mut y = card.y0 + 84.0;
        let key_w = 186.0;
        for (k, d) in rows {
            // A row with an empty description is a section divider (e.g.
            // "Present mode") — render it as a dim small-caps header.
            if d.is_empty() {
                y += 6.0;
                tr.draw_text_weighted(
                    scene,
                    &k.to_uppercase(),
                    11.0,
                    c.text_dim,
                    t * Affine::translate((px, y + 2.0)),
                    None,
                    TextWeight::Semibold,
                );
                y += 22.0;
                continue;
            }
            tr.draw_text_weighted(
                scene,
                k,
                13.0,
                pal.accent,
                t * Affine::translate((px, y)),
                Some(key_w as f32),
                TextWeight::Semibold,
            );
            tr.draw_text_weighted(
                scene,
                d,
                13.0,
                c.text_primary,
                t * Affine::translate((px + key_w, y)),
                Some((cw - key_w - 64.0) as f32),
                TextWeight::Regular,
            );
            y += 25.0;
        }
    }
}

/// Make `title` safe as a filename stem: keep alphanumerics, dash and
/// underscore, fold everything else to `_`, and never return empty.
fn sanitize_filename(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() { "presentation".into() } else { s }
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

fn mix_panel(a: Color, b: Color) -> Color {
    Color::from_rgba8(
        ((a.r() as u16 + b.r() as u16) / 2) as u8,
        ((a.g() as u16 + b.g() as u16) / 2) as u8,
        ((a.b() as u16 + b.b() as u16) / 2) as u8,
        255,
    )
}

fn stroke_line(scene: &mut Scene, t: Affine, x0: f64, y0: f64, x1: f64, y1: f64, color: Color) {
    let line = Line::new(Point::new(x0, y0), Point::new(x1, y1));
    scene.stroke(&Stroke::new(1.0), t, color, None, &line);
}

/// `(track, thumb)` rects for a vertical scrollbar over `viewport`, or `None`
/// when the content fits (`max_scroll <= 0`). The thumb height is proportional
/// to the visible fraction and its position to the scroll offset.
fn scrollbar_rects(viewport: Rect, scroll: f64, max_scroll: f64) -> Option<(Rect, Rect)> {
    if max_scroll <= 0.5 || viewport.height() <= 8.0 {
        return None;
    }
    let sw = 6.0;
    let track = Rect::new(viewport.x1 - sw - 3.0, viewport.y0, viewport.x1 - 3.0, viewport.y1);
    let content_h = viewport.height() + max_scroll;
    let visible_frac = (viewport.height() / content_h).clamp(0.06, 1.0);
    let thumb_h = (track.height() * visible_frac).max(28.0);
    let travel = track.height() - thumb_h;
    let t = if max_scroll > 0.0 { (scroll / max_scroll).clamp(0.0, 1.0) } else { 0.0 };
    let ty = track.y0 + travel * t;
    let thumb = Rect::new(track.x0, ty, track.x1, ty + thumb_h);
    Some((track, thumb))
}

/// Map a vertical scrollbar-thumb drag (`dy` px) to a content scroll offset.
/// The thumb travels `track.height - thumb.height`; the content travels
/// `max_scroll` over that same span, so one thumb pixel is `max_scroll/travel`
/// content pixels. Result is clamped to `0..=max_scroll`.
fn scroll_from_thumb_drag(viewport: Rect, orig_scroll: f64, max_scroll: f64, dy: f64) -> f64 {
    if max_scroll <= 0.0 {
        return 0.0;
    }
    let orig = orig_scroll.clamp(0.0, max_scroll);
    match scrollbar_rects(viewport, orig, max_scroll) {
        Some((track, thumb)) => {
            let travel = (track.height() - thumb.height()).max(1.0);
            (orig + dy * (max_scroll / travel)).clamp(0.0, max_scroll)
        }
        None => 0.0,
    }
}

/// Truncate `label` with a trailing ellipsis so its estimated width fits
/// `max_w`. Uses the same [`chrome::est_width`] metric that drives the toolbar
/// layout, so the clip lands where the layout expects it.
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
    let col = caret - starts[li];
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
    let tstart = starts[target];
    let tend = if target + 1 < starts.len() {
        starts[target + 1] - 1 // drop the trailing '\n'
    } else {
        chars.len()
    };
    tstart + col.min(tend - tstart)
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
    while le < chars.len() && chars[le] != '\n' {
        le += 1;
    }
    le
}

fn elide(label: &str, font: f64, max_w: f64) -> String {
    if max_w <= 0.0 || chrome::est_width(label, font) <= max_w {
        return label.to_string();
    }
    let mut chars: Vec<char> = label.chars().collect();
    while !chars.is_empty() {
        let candidate: String = chars.iter().collect::<String>().trim_end().to_string() + "…";
        if chrome::est_width(&candidate, font) <= max_w {
            return candidate;
        }
        chars.pop();
    }
    "…".to_string()
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

/// Fit a 16:9 slide horizontally centred but *top-anchored* inside `area`.
fn fit_slide_top(area: Rect) -> Rect {
    let s = (area.width() / SLIDE_W).min(area.height() / SLIDE_H);
    let sw = SLIDE_W * s;
    let sh = SLIDE_H * s;
    let sx = area.x0 + (area.width() - sw) / 2.0;
    Rect::new(sx, area.y0, sx + sw, area.y0 + sh)
}

impl Widget for SlidesApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            if SlidesDoc::is_empty(state) {
                SlidesDoc::seed(state, &self.deck);
            }
            self.seeded = true;
        }
        // Fold in a completed host image-open, if the portal worker finished.
        self.drain_portal(state);
        // Fold in a completed `.odp` open/save.
        self.drain_doc_portal(state);
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
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
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
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
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
        // Fold in any images that finished decoding on their worker thread. A
        // freshly-decoded image must re-shape the cached present-mode slide.
        if self.images.pump() {
            self.present_cache = None;
        }
        // Expire a settled cross-fade so `needs_animation_frame` can rest.
        if let Some((_, t)) = self.transition {
            if t.elapsed().as_millis() >= TRANSITION_MS {
                self.transition = None;
            }
        }
        // Reconcile the rail flap with the current width (so resizing across
        // the breakpoint returns to the layout default) and advance its eased
        // reveal using real elapsed time, frame-rate independent.
        let now = std::time::Instant::now();
        let dt = (now - self.last_flap_tick).as_secs_f64().min(0.1);
        self.last_flap_tick = now;
        self.sync_flaps(dt);
        // Re-clamp the persisted scroll offsets to the current size/content, so
        // a window resize (or a fresh over-large offset) self-corrects rather
        // than sticking past the end of the list.
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
                let (_, _, ov_max) = self.compute_overview();
                self.overview_scroll = self.overview_scroll.clamp(0.0, ov_max);
            }
            Mode::Present => {}
        }
        // `--shots`-only: pop a context menu now that the layout rect is known.
        if let Some(kind) = self.demo_menu.take() {
            let chrome = self.compute_chrome();
            match kind {
                DemoMenu::Element => {
                    let p = self
                        .selected_element()
                        .map(|el| render::element_dest(chrome.slide_dest, chrome.scale, el).center())
                        .unwrap_or_else(|| chrome.slide_dest.center());
                    self.open_element_menu(p);
                }
                DemoMenu::Slide => {
                    // Open the shared rail's own duplicate / move / delete menu on
                    // the current slide's thumbnail.
                    let p = chrome
                        .thumbs
                        .iter()
                        .find(|(i, _)| *i == self.cur_slide)
                        .map(|(_, r)| r.center())
                        .unwrap_or_else(|| Point::new(120.0, 220.0));
                    self.page_sidebar.on_right_press(p);
                    let _ = self.page_sidebar.take_events();
                }
            }
        }
        // `--shots`-only: stage a live reorder drag so the capture shows the
        // floating ghost + the reflowed drop gap.
        if std::mem::take(&mut self.demo_reorder) {
            let thumbs = self.rail_thumb_rects();
            if thumbs.len() > 2 {
                let start = thumbs[0].1.center();
                // Drop it two slots down: the ghost floats at the cursor and the
                // list reflows to open the gap at that slot.
                let target = thumbs[2].1.center();
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
            if thumbs.len() > 1 {
                let start = thumbs[1].1.center();
                let drop = self.slide_delete_zone(self.rail_rect()).center();
                self.last_mouse = drop;
                self.page_sidebar.on_press(start);
                self.page_sidebar.on_motion(drop);
                let _ = self.page_sidebar.take_events();
            }
        }
        match self.mode {
            Mode::Edit => {
                let chrome = self.compute_chrome();
                self.draw_editor(scene, transform, text_renderer, theme, &chrome);
                self.draw_inspector_widgets(scene, transform, text_renderer, theme, presence, &chrome);
                if self.editing_title {
                    self.draw_title_input(scene, transform, text_renderer, theme, presence, &chrome);
                }
            }
            Mode::Overview => self.draw_overview(scene, transform, text_renderer, theme),
            Mode::Present => self.draw_present(scene, transform, text_renderer, theme),
        }
        // The help overlay draws over every mode — including Present, where it
        // must appear (and be dismissable), otherwise a `?` press would set the
        // flag, suppress the overlay, and swallow all later navigation keys.
        if self.help_overlay_visible() {
            self.draw_help(scene, transform, text_renderer, theme);
        }
        // Right-click context menu floats above everything else.
        self.ctx_menu.draw(scene, transform, text_renderer, theme, presence);
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
        self.transition.is_some()
            || self.rail_flap.is_animating(self.rail_layout())
            || self.inspector_flap.is_animating(self.inspector_layout())
            || self.images.any_loading()
            // A live thumbnail reorder pumps frames so the ghost tracks the
            // cursor and the rail auto-scrolls at its edges.
            || self.page_sidebar.is_dragging()
    }

    /// Present mode wants the surface promoted to true OS fullscreen so the
    /// slideshow fills the whole output (over the shell's strips + top bar).
    /// The host ([`ui::App`]) polls this each frame and drives
    /// `window.set_fullscreen`, which on the qOS compositor becomes an
    /// `xdg_toplevel.set_fullscreen` request. Level-triggered: a steady
    /// `Some(true)` for the whole talk is correct and idempotent.
    fn fullscreen_intent(&self) -> Option<bool> {
        Some(self.wants_fullscreen())
    }

    /// Present is a *distinct* view (the slideshow, not the editor) that wants
    /// the **largest** output — the projector / big screen — via the shared
    /// [`ui::fullscreen`] mechanism. The App enumerates the live monitors and
    /// promotes the surface onto the biggest one; on a single-output host it is
    /// simply that one screen. Same mechanism the video player uses for
    /// immersive fullscreen — only the target output + the distinct-view flag
    /// differ.
    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        ui::fullscreen::FullscreenRequest::distinct(ui::fullscreen::FullscreenTarget::Largest)
    }

    /// React to the *actual* fullscreen state reported by the host. A
    /// compositor-initiated exit (a global un-fullscreen keybind, or the grant
    /// being revoked) drops us out of present mode so the editor chrome comes
    /// back — without this the app would keep painting the borderless slideshow
    /// in a now-windowed surface. App-initiated exits (Esc → `exit_present`)
    /// already left present mode, so this is a no-op for them.
    fn on_fullscreen_changed(&mut self, active: bool) {
        if !active && self.mode == Mode::Present {
            self.exit_present();
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // A right-press (re)opens a context menu — handled before the modal
        // routing so it works even while another menu is already up.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } = event
        {
            self.on_right_press(state, self.last_mouse, handled);
            return;
        }
        // While a context menu is open it is modal: every other event goes to it.
        if self.route_context_menu(
            event, scale_factor, modifiers, state, _path, _focus_target, _node_id, _taffy,
        ) {
            *handled = true;
            return;
        }
        // The inline title editor owns input while renaming.
        if self.route_title_edit(
            event, scale_factor, modifiers, state, _path, _focus_target, _node_id, _taffy,
        ) {
            *handled = true;
            return;
        }
        // The inspector's embedded widgets (font-size field, fill picker) get
        // first refusal on events over them.
        if self.route_inspector_widgets(
            event, scale_factor, modifiers, state, _path, _focus_target, _node_id, _taffy,
        ) {
            *handled = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
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
            WindowEvent::MouseInput { state: btn_state, button: MouseButton::Left, .. } => {
                match btn_state {
                    ElementState::Pressed => self.on_press(state, self.last_mouse, handled),
                    ElementState::Released => self.on_release(state, handled),
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.on_wheel(*delta, scale_factor, handled);
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, text, .. },
                ..
            } => {
                self.on_key(state, logical_key, text.as_ref().map(|s| s.as_str()), modifiers, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "SlidesApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
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
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its slides action, so it records one undo boundary for it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl SlidesApp {
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
                if d <= thr && best_x.map(|b| d < b.0).unwrap_or(true) {
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
                if d <= thr && best_y.map(|b| d < b.0).unwrap_or(true) {
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
        match self.drag.clone() {
            Drag::ScrollOverview { start_y, orig_scroll } => {
                let (area, _, max_scroll) = self.compute_overview();
                self.overview_scroll = scroll_from_thumb_drag(
                    area,
                    orig_scroll,
                    max_scroll,
                    p.y - start_y,
                );
                return;
            }
            _ => {}
        }
        let chrome = self.compute_chrome();
        let dest = chrome.slide_dest;
        let s = chrome.scale;
        let sp = self.to_slide(dest, s, p);
        match self.drag.clone() {
            Drag::Move { id, start, orig_x, orig_y } => {
                let dx = sp.x - start.x;
                let dy = sp.y - start.y;
                let (w, h) = self
                    .selected_element()
                    .map(|e| (e.w, e.h))
                    .unwrap_or((0.0, 0.0));
                let thr = SNAP_PX / s.max(0.0001);
                let (nx, ny, guides) =
                    self.snapped_move(&id, orig_x + dx, orig_y + dy, w, h, thr);
                self.active_guides = guides;
                if let Some(slide) = self.deck.slides.get_mut(self.cur_slide) {
                    if let Some(el) = slide.element_mut(&id) {
                        el.x = nx;
                        el.y = ny;
                    }
                }
            }
            Drag::Resize { id, handle, start, orig } => {
                let dx = sp.x - start.x;
                let dy = sp.y - start.y;
                let (mut x, mut y, mut w, mut h) = orig;
                let left = matches!(handle, Handle::Tl | Handle::L | Handle::Bl);
                let right = matches!(handle, Handle::Tr | Handle::R | Handle::Br);
                let top = matches!(handle, Handle::Tl | Handle::T | Handle::Tr);
                let bottom = matches!(handle, Handle::Bl | Handle::B | Handle::Br);
                if left {
                    x = orig.0 + dx;
                    w = orig.2 - dx;
                }
                if right {
                    w = orig.2 + dx;
                }
                if top {
                    y = orig.1 + dy;
                    h = orig.3 - dy;
                }
                if bottom {
                    h = orig.3 + dy;
                }
                let min = 16.0;
                if w < min {
                    if left {
                        x = orig.0 + orig.2 - min;
                    }
                    w = min;
                }
                if h < min {
                    if top {
                        y = orig.1 + orig.3 - min;
                    }
                    h = min;
                }
                if let Some(slide) = self.deck.slides.get_mut(self.cur_slide) {
                    if let Some(el) = slide.element_mut(&id) {
                        el.x = x;
                        el.y = y;
                        el.w = w;
                        el.h = h;
                    }
                }
            }
            // `ScrollOverview` is resolved above; everything else is a no-op here.
            _ => {}
        }
    }

    fn on_press(&mut self, doc: &mut AutoCommit, p: Point, handled: &mut bool) {
        // The help overlay swallows the next click (to dismiss).
        if self.show_help {
            self.show_help = false;
            *handled = true;
            return;
        }
        if self.mode == Mode::Present {
            if self.blank.is_blanked() {
                // A click resumes a blanked screen.
                self.blank = Blank::None;
            } else if self.presenter_view {
                // The dashboard is a control surface, not a slide — a click
                // there should not blindly advance. (Navigation is via keys.)
            } else {
                // Audience view: click anywhere advances.
                self.next_slide(1);
            }
            *handled = true;
            return;
        }
        if self.mode == Mode::Overview {
            let (area, thumbs, max_scroll) = self.compute_overview();
            // Scrollbar thumb → start a scroll drag.
            if let Some((_, thumb)) =
                scrollbar_rects(area, self.overview_scroll.clamp(0.0, max_scroll), max_scroll)
            {
                if thumb.contains(p) {
                    self.drag = Drag::ScrollOverview {
                        start_y: p.y,
                        orig_scroll: self.overview_scroll.clamp(0.0, max_scroll),
                    };
                    *handled = true;
                    return;
                }
            }
            for (i, rect) in thumbs {
                if rect.contains(p) {
                    self.apply_hit(doc, Hit::OpenSlide(i));
                    *handled = true;
                    return;
                }
            }
            *handled = true;
            return;
        }
        let chrome = self.compute_chrome();

        // The shared rail's own context menu is modal while open: the press
        // picks one of its rows or dismisses it, ahead of all other chrome.
        if self.page_sidebar.menu_open() {
            self.sync_page_rail();
            self.page_sidebar.on_press(p);
            self.apply_rail_events(doc);
            self.rail_scroll = self.page_sidebar.scroll();
            *handled = true;
            return;
        }

        // A click anywhere but the title itself commits an in-flight rename.
        if self.editing_title && !chrome.title_rect.contains(p) {
            self.editing_title = false;
            self.mark();
        }

        // Toolbar overflow menu is modal-ish: while open, its rows and its
        // click-away dismissal take precedence over everything beneath it.
        if let Some(bg) = chrome.menu_bg {
            for b in &chrome.menu_buttons {
                if b.rect.contains(p) {
                    let hit = b.hit.clone();
                    self.apply_hit(doc, hit);
                    *handled = true;
                    return;
                }
            }
            if bg.contains(p) {
                // A click in the menu's own chrome (padding) keeps it open.
                *handled = true;
                return;
            }
            // A click outside the menu dismisses it — unless it landed on the ⋯
            // toggle itself, which we let fall through so the toggle closes it.
            let on_toggle = chrome
                .buttons
                .iter()
                .any(|b| matches!(b.hit, Hit::ToggleToolbarMenu) && b.rect.contains(p));
            if !on_toggle {
                self.toolbar_menu_open = false;
                *handled = true;
                return;
            }
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
            self.press_canvas(doc, p, &chrome, handled);
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

    fn press_canvas(&mut self, doc: &mut AutoCommit, p: Point, chrome: &Chrome, handled: &mut bool) {
        let dest = chrome.slide_dest;
        let s = chrome.scale;
        // Resize handle of the current selection?
        if let Some(el) = self.selected_element().cloned() {
            if !self.editing_text {
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
        }
        // Hit-test elements front-to-back.
        let mut picked: Option<Id> = None;
        if let Some(slide) = self.slide() {
            let mut z: Vec<&Element> = slide.elements.iter().collect();
            z.sort_by_key(|e| -e.z);
            for el in z {
                let er = render::element_dest(dest, s, el);
                if er.inflate(2.0, 2.0).contains(p) {
                    picked = Some(el.id.clone());
                    break;
                }
            }
        }
        match picked {
            Some(id) => {
                let is_text = self
                    .slide()
                    .and_then(|sl| sl.element(&id))
                    .map(|e| e.kind == ElementKind::Text)
                    .unwrap_or(false);
                // Double-click = same element, pressed again quickly and near
                // the first press.
                let double = self
                    .last_click
                    .as_ref()
                    .map(|(cid, t, cp)| {
                        cid == &id
                            && t.elapsed().as_millis() < 400
                            && cp.distance(p) < 6.0
                    })
                    .unwrap_or(false);
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
                    self.editing_text = true;
                    let len = self.selected_element().map(|e| e.text.chars().count()).unwrap_or(0);
                    self.caret = len;
                    self.sel_anchor = len;
                    self.select_word_click = Some(p);
                } else {
                    // Single press: select AND arm a move so a selected element
                    // can be dragged on the very first grab (the reported bug was
                    // that a second click on a selected box entered edit instead
                    // of letting you move it).
                    self.editing_text = false;
                    let el = self.selected_element().cloned().unwrap();
                    self.drag = Drag::Move {
                        id,
                        start: self.to_slide(dest, s, p),
                        orig_x: el.x,
                        orig_y: el.y,
                    };
                }
                *handled = true;
            }
            None => {
                self.sel = None;
                self.editing_text = false;
                self.last_click = None;
                *handled = true;
            }
        }
        let _ = doc;
    }

    /// Hit-test the canvas elements front-to-back at screen point `p`.
    fn element_at(&self, chrome: &Chrome, p: Point) -> Option<Id> {
        let dest = chrome.slide_dest;
        let s = chrome.scale;
        let slide = self.slide()?;
        let mut z: Vec<&Element> = slide.elements.iter().collect();
        z.sort_by_key(|e| -e.z);
        for el in z {
            if render::element_dest(dest, s, el).inflate(2.0, 2.0).contains(p) {
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
    fn open_menu_at(&mut self, p: Point, rows: &[Option<(&str, Hit)>]) {
        let mut menu = ContextMenu::new();
        let mut hits: Vec<Option<Hit>> = Vec::new();
        for row in rows {
            match row {
                Some((label, hit)) => {
                    menu.push(MenuItem::action((*label).to_string(), || {}));
                    hits.push(Some(hit.clone()));
                }
                None => {
                    menu.push(MenuItem::separator());
                    hits.push(None);
                }
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
                Some(("Bring to front", Hit::BringFront)),
                Some(("Send to back", Hit::SendBack)),
                None,
                Some(("Duplicate", Hit::DuplicateElement)),
                Some(("Delete", Hit::DeleteElement)),
            ],
        );
    }


    /// Forward an event to the open context menu and apply any chosen action.
    /// Returns `true` if a menu was open (and thus consumed the event).
    fn route_context_menu(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if !self.ctx_menu.is_open() {
            return false;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        let mut menu_handled = false;
        self.ctx_menu.handle_event(
            event, scale_factor, modifiers, state, path, &mut menu_handled, focus_target, node_id,
            taffy,
        );
        if let Some(idx) = self.ctx_menu.take_selection() {
            if let Some(Some(hit)) = self.ctx_hits.get(idx).cloned() {
                self.apply_hit(state, hit);
            }
        }
        true
    }

    /// Forward events to the inspector's embedded `ui` widgets (font-size field
    /// + fill picker) and fold any change into the CRDT. Returns `true` when the
    /// event was consumed (so `handle_event` should stop). The picker popover is
    /// modal while open; the font field only claims presses on its own rect and
    /// keystrokes while focused.
    #[allow(clippy::too_many_arguments)]
    fn route_inspector_widgets(
        &mut self,
        event: &WindowEvent,
        sf: f64,
        mods: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if self.mode != Mode::Edit || self.drag.active() {
            return false;
        }
        let text_selected = self
            .selected_element()
            .map(|e| e.kind == ElementKind::Text)
            .unwrap_or(false);
        if !self.fill_picker_open && !text_selected {
            return false;
        }
        let chrome = self.compute_chrome();
        self.position_inspector_widgets(&chrome);

        // --- Fill picker popover: modal while open. ---
        if self.fill_picker_open {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.last_mouse = Point::new(position.x / sf, position.y / sf);
            }
            if let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } = event
            {
                let panel = self.fill_picker.get_rect().inflate(12.0, 16.0);
                if !panel.contains(self.last_mouse) {
                    Widget::set_focus(&mut self.fill_picker, false);
                    self.fill_picker_open = false;
                    return true;
                }
            }
            let mut h = false;
            self.fill_picker
                .handle_event(event, sf, mods, state, path, &mut h, focus_target, node_id, taffy);
            if let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } = event
            {
                if self.fill_picker.get_rect().contains(self.last_mouse) {
                    Widget::set_focus(&mut self.fill_picker, true);
                }
            }
            if let Some(col) = self.fill_picker.take_changed() {
                self.set_sel_str(state, "fill", &crate::model::color_to_hex(col));
            }
            return true;
        }

        // --- Font-size NumberInput. ---
        if chrome.font_input_rect.is_none() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / sf, position.y / sf);
                let mut h = false;
                self.font_input
                    .handle_event(event, sf, mods, state, path, &mut h, focus_target, node_id, taffy);
                if let Some(v) = self.font_input.take_changed() {
                    self.set_sel_f64(state, "font_size", v);
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
                    self.font_input
                        .handle_event(event, sf, mods, state, path, &mut h, focus_target, node_id, taffy);
                    Widget::set_focus(&mut self.font_input, true);
                    if let Some(v) = self.font_input.take_changed() {
                        self.set_sel_f64(state, "font_size", v);
                    }
                    true
                } else if self.font_input.is_focused() {
                    // Press elsewhere commits + blurs, then falls through so the
                    // press can still hit whatever it landed on.
                    Widget::set_focus(&mut self.font_input, false);
                    if let Some(v) = self.font_input.take_changed() {
                        self.set_sel_f64(state, "font_size", v);
                    }
                    false
                } else {
                    false
                }
            }
            WindowEvent::KeyboardInput { .. } if self.font_input.is_focused() => {
                // Esc blurs the field (commit); everything else edits it.
                if let WindowEvent::KeyboardInput {
                    event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), state: ElementState::Pressed, .. },
                    ..
                } = event
                {
                    Widget::set_focus(&mut self.font_input, false);
                    if let Some(v) = self.font_input.take_changed() {
                        self.set_sel_f64(state, "font_size", v);
                    }
                    return true;
                }
                let mut h = false;
                self.font_input
                    .handle_event(event, sf, mods, state, path, &mut h, focus_target, node_id, taffy);
                if let Some(v) = self.font_input.take_changed() {
                    self.set_sel_f64(state, "font_size", v);
                }
                true
            }
            _ => false,
        }
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
                && self.slide_delete_zone(self.rail_rect()).contains(self.last_mouse)
            {
                if let Some(from) = self.page_sidebar.drag_from() {
                    self.page_sidebar.on_escape(); // cancel the reorder cleanly
                    self.page_sidebar.take_events();
                    self.rail_scroll = self.page_sidebar.scroll();
                    self.apply_rail_event(doc, PageSidebarEvent::Delete(from));
                    *handled = true;
                    return;
                }
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
            Drag::Move { id, .. } => Some(id),
            Drag::Resize { id, .. } => Some(id),
            // Scroll drags were already resolved above; `None` writes nothing.
            _ => None,
        };
        if let Some(id) = id {
            if let Some(el) = self.deck.slides.get(self.cur_slide).and_then(|s| s.element(&id)).cloned() {
                // The drag preview lived only in the in-memory deck; commit the
                // element's FINAL rect once, on drop — never a write per frame.
                self.perform_action(
                    doc,
                    actions::resize_element_action(self.cur_slide, id, el.x, el.y, el.w, el.h),
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
            MouseScrollDelta::LineDelta(_, y) => y as f64 * WHEEL_LINE_PX,
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
                }
            }
            Mode::Overview => {
                let (_, _, max_scroll) = self.compute_overview();
                if max_scroll > 0.0 {
                    self.overview_scroll = (self.overview_scroll - dy).clamp(0.0, max_scroll);
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
        modifiers: &Modifiers,
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

        // Help overlay owns all keys while open.
        if self.show_help {
            match key {
                Key::Named(NamedKey::Escape) => self.show_help = false,
                Key::Character(c) if c.as_str() == "?" => self.show_help = false,
                _ => {}
            }
            *handled = true;
            return;
        }
        // `?` opens help from any non-text context.
        if !self.editing_text && !self.editing_notes && !self.editing_title {
            if let Key::Character(c) = key {
                if c.as_str() == "?" {
                    self.show_help = true;
                    *handled = true;
                    return;
                }
            }
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
            match key {
                Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter) => {
                    self.mode = Mode::Edit;
                    *handled = true;
                }
                Key::Character(c) if c.eq_ignore_ascii_case("g") => {
                    self.mode = Mode::Edit;
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::ArrowDown) => {
                    if self.cur_slide + 1 < self.deck.slides.len() {
                        self.cur_slide += 1;
                    }
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowUp) => {
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
            return;
        }

        // Present-mode keys.
        if self.mode == Mode::Present {
            // Any blank screen resumes on the first navigation/control key
            // other than its own toggle (handled per-key below).
            match key {
                Key::Named(NamedKey::ArrowRight)
                | Key::Named(NamedKey::ArrowDown)
                | Key::Named(NamedKey::Space)
                | Key::Named(NamedKey::Enter)
                | Key::Named(NamedKey::PageDown) => {
                    if self.blank.is_blanked() {
                        self.blank = Blank::None;
                    } else {
                        self.next_slide(1);
                    }
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowLeft)
                | Key::Named(NamedKey::ArrowUp)
                | Key::Named(NamedKey::PageUp)
                | Key::Named(NamedKey::Backspace) => {
                    if self.blank.is_blanked() {
                        self.blank = Blank::None;
                    } else {
                        self.next_slide(-1);
                    }
                    *handled = true;
                }
                Key::Named(NamedKey::Home) => {
                    self.goto_slide(0);
                    *handled = true;
                }
                Key::Named(NamedKey::End) => {
                    self.goto_slide(self.deck.slides.len().saturating_sub(1));
                    *handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.exit_present();
                    *handled = true;
                }
                // Toggle the presenter dashboard (single-monitor) — `P`, the
                // same key that launched present mode.
                Key::Character(c) if c.eq_ignore_ascii_case("p") => {
                    self.presenter_view = !self.presenter_view;
                    *handled = true;
                }
                // Audience-visible speaker-notes strip (only meaningful in the
                // clean audience view).
                Key::Character(c) if c.eq_ignore_ascii_case("s") => {
                    self.show_notes = !self.show_notes;
                    *handled = true;
                }
                Key::Character(c) if c.eq_ignore_ascii_case("b") => {
                    self.blank = if self.blank == Blank::Black { Blank::None } else { Blank::Black };
                    *handled = true;
                }
                Key::Character(c) if c.eq_ignore_ascii_case("w") => {
                    self.blank = if self.blank == Blank::White { Blank::None } else { Blank::White };
                    *handled = true;
                }
                // Pause / resume the speaker timer.
                Key::Character(c) if c.eq_ignore_ascii_case("k") => {
                    self.toggle_timer();
                    *handled = true;
                }
                // Reset the speaker timer to 0:00.
                Key::Character(c) if c.as_str() == "0" => {
                    self.reset_timer();
                    *handled = true;
                }
                // Jump to slide N (1-9) by number.
                Key::Character(c) => {
                    if let Some(d) = c.chars().next().and_then(|ch| ch.to_digit(10)) {
                        if d >= 1 {
                            self.goto_slide((d as usize) - 1);
                            *handled = true;
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        // Text / notes / title editing intercepts most keys. The on-canvas text
        // and speaker-notes editors share the `ui` caret / selection core:
        // shift+arrow extends, ctrl+arrow is word-wise, ctrl+A selects all, and
        // C/X/V go through the shared clipboard — all splicing the real CRDT.
        // (Title rename is consumed earlier by the shared `TextInput`; it only
        // reaches here defensively.)
        if self.editing_text || self.editing_notes || self.editing_title {
            let shift = mods.shift_key();
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
                    if let Some(txt) = text {
                        if !txt.is_empty() && !ctrl {
                            self.insert_text(doc, txt);
                            *handled = true;
                        }
                    }
                }
            }
            return;
        }

        // Editor command keys.
        match key {
            Key::Named(NamedKey::Escape) => {
                if self.sel.is_some() {
                    self.sel = None;
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                if self.sel.is_some() {
                    self.delete_element(doc);
                    self.reload(doc);
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Enter) => {
                if self.selected_element().map(|e| e.kind == ElementKind::Text).unwrap_or(false) {
                    self.editing_text = true;
                    self.caret = self.selected_element().map(|e| e.text.chars().count()).unwrap_or(0);
                    self.sel_anchor = self.caret;
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
            Key::Character(c) => {
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
            _ => {}
        }
    }

    fn cycle_selection(&mut self) {
        let Some(slide) = self.slide() else { return };
        if slide.elements.is_empty() {
            return;
        }
        let mut z: Vec<&Element> = slide.elements.iter().collect();
        z.sort_by_key(|e| e.z);
        let ids: Vec<Id> = z.iter().map(|e| e.id.clone()).collect();
        let next = match &self.sel {
            Some(cur) => {
                let i = ids.iter().position(|x| x == cur).unwrap_or(usize::MAX);
                ids[(i.wrapping_add(1)) % ids.len()].clone()
            }
            None => ids[0].clone(),
        };
        self.sel = Some(next);
        self.editing_text = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Deck, ElementKind, Layout, PaletteKind, Slide};

    fn app_with(slide: Slide) -> SlidesApp {
        let mut a = SlidesApp::new();
        a.deck = Deck { title: "t".into(), palette: PaletteKind::Indigo, slides: vec![slide] };
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
        assert_eq!(app.present_clock(), "0:00");
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
        app.on_key(&mut doc, &Key::Named(key), None, &mods, &mut handled);
        handled
    }

    /// Press a character key through the production `on_key` dispatcher.
    fn press_char(app: &mut SlidesApp, ch: &str) -> bool {
        let mut doc = AutoCommit::new();
        let mods = Modifiers::default();
        let mut handled = false;
        app.on_key(&mut doc, &Key::Character(ch.into()), Some(ch), &mods, &mut handled);
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
        let before = app.slide().map(|s| s.elements.len()).unwrap_or(0);

        let mut handled = false;
        app.on_key(
            &mut doc,
            &Key::Character("r".into()),
            Some("r"),
            &Modifiers::default(),
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
        assert!(app.sel.is_some(), "the freshly inserted element is selected");
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
        assert_eq!(app.blank, Blank::Black);
        assert!(press_char(&mut app, "b"));
        assert_eq!(app.blank, Blank::None);
        // W whites out; pressing B switches to black (mutually exclusive).
        assert!(press_char(&mut app, "w"));
        assert_eq!(app.blank, Blank::White);
        assert!(press_char(&mut app, "b"));
        assert_eq!(app.blank, Blank::Black);
        // A navigation key resumes the slide *without* advancing.
        let before = app.cur_slide;
        assert!(press_named(&mut app, NamedKey::ArrowRight));
        assert_eq!(app.blank, Blank::None, "navigation resumes a blanked screen");
        assert_eq!(app.cur_slide, before, "the resuming keypress does not also advance");
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
        assert_eq!(params.target, FullscreenTarget::Largest, "Present targets the big screen");
        assert_eq!(params.view, FullscreenView::Distinct, "Present shows the slideshow, not the editor");
        assert_eq!(Widget::fullscreen_intent(&app), Some(true), "and it drives the shared reconcile");
    }

    #[test]
    fn compositor_exit_of_fullscreen_leaves_present_mode() {
        let mut app = presenting_app();
        assert_eq!(app.mode, Mode::Present);
        // A compositor-driven fullscreen exit (e.g. a global un-fullscreen
        // keybind) must drop us out of present mode so the editor chrome
        // returns rather than painting a windowed borderless slideshow.
        Widget::on_fullscreen_changed(&mut app, false);
        assert_eq!(app.mode, Mode::Edit, "losing fullscreen leaves present mode");
        // Idempotent + safe when not presenting: a redundant notification is a
        // no-op, and gaining fullscreen never forces present mode on.
        Widget::on_fullscreen_changed(&mut app, false);
        assert_eq!(app.mode, Mode::Edit);
        Widget::on_fullscreen_changed(&mut app, true);
        assert_eq!(app.mode, Mode::Edit, "a fullscreen grant alone doesn't start a talk");
    }

    #[test]
    fn presenter_dashboard_toggles_with_p() {
        let mut app = presenting_app();
        assert!(!app.presenter_view);
        assert!(press_char(&mut app, "p"));
        assert!(app.presenter_view, "P shows the presenter dashboard");
        assert!(press_char(&mut app, "p"));
        assert!(!app.presenter_view, "P hides it again");
        // Toggling the dashboard never leaves present mode.
        assert_eq!(app.mode, Mode::Present);
    }

    #[test]
    fn speaker_timer_pause_and_reset() {
        let mut app = presenting_app();
        assert!(!app.timer_paused());
        // K pauses, K resumes.
        assert!(press_char(&mut app, "k"));
        assert!(app.timer_paused(), "K pauses the timer");
        // Elapsed is frozen while paused.
        let frozen = app.present_elapsed();
        std::thread::sleep(std::time::Duration::from_millis(15));
        assert_eq!(app.present_elapsed(), frozen, "elapsed is frozen while paused");
        assert!(press_char(&mut app, "k"));
        assert!(!app.timer_paused(), "K resumes the timer");
        // 0 resets to zero.
        assert!(press_char(&mut app, "0"));
        assert!(app.present_elapsed().as_millis() < 50, "0 resets the clock");
    }

    #[test]
    fn present_slide_cache_avoids_reshaping_each_frame() {
        // Present mode pumps animation frames to tick the speaker clock; the
        // slide underneath must NOT be re-shaped every frame. The first frame
        // shapes + caches; subsequent frames at the same slide/size just append
        // the cached scene, and navigation invalidates it.
        let mut app = presenting_app();
        app.cur_slide = 1; // a heavier slide (title + multi-line body + figure)
        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        let theme = Theme::dark();
        let pal = app.palette(&theme);
        let mut tr = TextRenderer::new();
        let dest = Rect::new(100.0, 50.0, 1180.0, 660.0);

        // Cold frame: shapes every element and fills the cache.
        let mut scene = Scene::new();
        let t0 = std::time::Instant::now();
        app.append_present_slide(&mut scene, Affine::IDENTITY, &mut tr, &theme, &pal, dest);
        let cold = t0.elapsed();
        assert!(app.present_cache.is_some(), "first frame populates the cache");

        // Warm frames: same slide/size ⇒ cache hit, a scene append with no
        // re-shaping.
        let t1 = std::time::Instant::now();
        for _ in 0..20 {
            let mut s = Scene::new();
            app.append_present_slide(&mut s, Affine::IDENTITY, &mut tr, &theme, &pal, dest);
        }
        let warm_avg = t1.elapsed() / 20;
        assert!(
            warm_avg * 2 < cold,
            "cached present frame ({warm_avg:?}) should be far cheaper than the cold reshape ({cold:?})"
        );

        // Navigation invalidates the cache so edits/advances still re-render.
        app.cur_slide += 1;
        assert!(
            !app
                .present_cache
                .as_ref()
                .unwrap()
                .matches(dest, app.cur_slide, app.deck_rev, app.deck.palette),
            "advancing the slide invalidates the cached render"
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
    fn app_sized(w: f64) -> SlidesApp {
        let mut s = Slide::new(Layout::TitleContent);
        s.add(Element::text("hello", 96.0, 90.0, 1088.0, 100.0, 54.0));
        let mut a = app_with(s);
        a.rect = Rect::new(0.0, 0.0, w, 800.0);
        a.sync_flaps(0.0);
        a
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
        assert!(chrome.rail.width() > RAIL_W - 1.0, "docked rail is full width");
        assert!(
            chrome.slide_dest.x0 > chrome.rail.x1 - 1.0,
            "canvas sits to the right of the docked rail"
        );
        assert_eq!(chrome.scrim.width(), 0.0, "no scrim while docked");
        // No hamburger when the rail is docked + fully open.
        assert!(
            !chrome.buttons.iter().any(|b| matches!(b.kind, ButtonKind::Hamburger)),
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
        assert!(chrome.rail.x1 <= app.rect.x0 + 1.0, "closed flap is off-screen left");
        assert_eq!(chrome.scrim.width(), 0.0, "no scrim while the flap is closed");
        // A hamburger is offered to summon it.
        assert!(
            chrome.buttons.iter().any(|b| matches!(b.kind, ButtonKind::Hamburger)),
            "narrow layout offers a hamburger to summon the rail"
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
        assert!((chrome.rail.x0 - app.rect.x0).abs() < 1.0, "open flap is flush left");
        assert!(chrome.scrim.width() > 0.0, "an open overlay flap dims the body");
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
        assert!(chrome.inspector.width() > INSPECTOR_W - 1.0, "docked inspector is full width");
        assert!(
            chrome.slide_dest.x1 < chrome.inspector.x0 + 1.0,
            "canvas sits to the left of the docked inspector"
        );
    }

    #[test]
    fn narrow_window_collapses_inspector_to_a_closed_overlay() {
        // Below the inspector breakpoint the inspector becomes a closed flap and
        // the canvas reclaims its column (spanning to the right edge).
        let app = app_sized(INSPECTOR_BREAKPOINT - 120.0);
        assert_eq!(app.inspector_layout(), SplitLayout::Overlay);
        assert!(!app.inspector_open(), "overlay inspector is closed by default");
        let chrome = app.compute_chrome();
        assert!(chrome.inspector.x0 >= app.rect.x1 - 1.0, "closed inspector is off-screen right");
        // A toggle is offered to summon it.
        assert!(
            chrome.buttons.iter().any(|b| matches!(b.hit, Hit::ToggleInspector)),
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
        assert!(app.inspector_open(), "ToggleInspector opens the collapsed flap");
        app.apply_hit(&mut doc, Hit::ToggleInspector);
        assert!(!app.inspector_open(), "ToggleInspector closes it again");
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
        assert!(!app.rail_open(), "overlay default is closed after re-narrowing");
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
        app.sync_flaps(0.0);
        assert_eq!(app.rail_layout(), SplitLayout::Overlay, "rail reflows to overlay");
        assert_eq!(app.inspector_layout(), SplitLayout::Overlay, "inspector reflows to overlay");
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
        a.deck = Deck { title: "t".into(), palette: PaletteKind::Indigo, slides };
        a.cur_slide = 0;
        a.bootstrapped = true;
        a.rect = Rect::new(0.0, 0.0, 1280.0, h);
        a.sync_flaps(0.0);
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
        assert!((app.rail_scroll - max_scroll).abs() < 1.0, "rail scroll clamps to max");

        // Pointer over the canvas → the rail does not move.
        app.rail_scroll = 0.0;
        app.last_mouse = Point::new(app.rect.x1 - 80.0, app.rect.center().y);
        let mut h2 = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut h2);
        assert_eq!(app.rail_scroll, 0.0, "wheel over the canvas leaves the rail alone");
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
        assert!(max_scroll > 0.0, "the 5-slide deck overflows the 800x600 overview");

        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
        assert!(handled && app.overview_scroll > 0.0, "wheel scrolls the overview");

        // Scroll to the very end and confirm the last slide's card is on-screen.
        for _ in 0..60 {
            let mut h = false;
            app.on_wheel(MouseScrollDelta::LineDelta(0.0, -20.0), 1.0, &mut h);
        }
        assert!((app.overview_scroll - max_scroll).abs() < 1.0, "overview scroll clamps to max");
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
        app.drag = Drag::ScrollOverview { start_y: thumb.center().y, orig_scroll: 0.0 };
        app.drag_move(Point::new(thumb.center().x, thumb.center().y + 120.0));
        assert!(app.overview_scroll > 0.0, "dragging the thumb down scrolls the grid");
    }

    // --- present mode rests -------------------------------------------------

    #[test]
    fn present_mode_rests_when_idle_and_wakes_for_a_transition() {
        let mut app = presenting_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        // Settle the flaps so nothing is mid-animation.
        for _ in 0..3 {
            app.sync_flaps(1.0);
        }
        assert!(
            !app.needs_animation_frame(),
            "a settled present mode does not request frames at vsync"
        );
        // A navigation kicks off a bounded cross-fade → frames pump again.
        app.next_slide(1);
        assert!(app.needs_animation_frame(), "a live cross-fade pumps frames");
    }

    // --- toolbar overflow menu ---------------------------------------------

    fn seeded_doc(app: &SlidesApp) -> AutoCommit {
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        doc
    }

    /// Does any toolbar button or open-menu row carry a hit satisfying `want`?
    fn insert_reachable(chrome: &Chrome, want: impl Fn(&Hit) -> bool) -> bool {
        chrome
            .buttons
            .iter()
            .chain(chrome.menu_buttons.iter())
            .any(|b| want(&b.hit))
    }

    #[test]
    fn narrow_toolbar_keeps_every_insert_tool_pointer_reachable() {
        // At 560px the toolbar can't show every insert tool, so it must offer a
        // ⋯ overflow button — and opening it must surface ALL of them. Nothing
        // is ever silently dropped (the old bug).
        let mut app = app_sized(560.0);
        app.sel = None;
        let mut doc = seeded_doc(&app);
        app.reload(&doc);

        let chrome = app.compute_chrome();
        let ovf = chrome
            .buttons
            .iter()
            .find(|b| matches!(b.hit, Hit::ToggleToolbarMenu))
            .expect("a narrow toolbar offers a ⋯ overflow button");
        let ovf_rect = ovf.rect;
        assert!(
            ovf_rect.width() > 1.0 && app.rect.contains(ovf_rect.center()),
            "the ⋯ button is on-screen and clickable"
        );

        // Drive a real click on the ⋯ button — it opens the menu.
        let mut handled = false;
        app.last_mouse = ovf_rect.center();
        app.on_press(&mut doc, ovf_rect.center(), &mut handled);
        assert!(handled && app.toolbar_menu_open, "clicking ⋯ opens the overflow menu");

        // With the menu open, bar + rows together cover every insert tool.
        let chrome = app.compute_chrome();
        assert!(chrome.menu_bg.is_some(), "the open menu has a panel");
        assert!(insert_reachable(&chrome, |h| matches!(h, Hit::NewSlide)), "New slide reachable");
        for k in [
            ElementKind::Text,
            ElementKind::Rect,
            ElementKind::Ellipse,
            ElementKind::Line,
            ElementKind::Image,
        ] {
            assert!(
                insert_reachable(&chrome, |h| matches!(h, Hit::Insert(kk) if *kk == k)),
                "{k:?} insert tool stays reachable via bar or overflow menu"
            );
        }
        // Every menu row is inside the window (a real pointer target).
        for b in &chrome.menu_buttons {
            assert!(app.rect.contains(b.rect.center()), "menu row is on-screen: {:?}", b.rect);
        }
    }

    #[test]
    fn wide_toolbar_shows_every_tool_without_an_overflow_button() {
        // At the default width all six tools fit, so there is no ⋯ button and no
        // menu — the overflow affordance appears only when it's actually needed.
        let mut app = app_sized(1280.0);
        app.sel = None;
        let chrome = app.compute_chrome();
        assert!(
            !chrome.buttons.iter().any(|b| matches!(b.hit, Hit::ToggleToolbarMenu)),
            "no ⋯ button when every tool fits"
        );
        assert!(chrome.menu_bg.is_none(), "no menu when nothing overflows");
        for k in [
            ElementKind::Text,
            ElementKind::Rect,
            ElementKind::Ellipse,
            ElementKind::Line,
            ElementKind::Image,
        ] {
            assert!(
                chrome.buttons.iter().any(|b| matches!(&b.hit, Hit::Insert(kk) if *kk == k)),
                "{k:?} sits directly on the wide toolbar"
            );
        }
    }

    #[test]
    fn clicking_an_overflow_menu_row_inserts_that_element() {
        let mut app = app_sized(560.0);
        let mut doc = seeded_doc(&app);
        app.reload(&doc);
        app.toolbar_menu_open = true;
        let before = app.slide().unwrap().elements.len();

        let chrome = app.compute_chrome();
        let row = chrome
            .menu_buttons
            .iter()
            .find(|b| matches!(b.hit, Hit::Insert(ElementKind::Rect)))
            .expect("the Rectangle tool is in the overflow menu at 560px");
        let p = row.rect.center();
        let mut handled = false;
        app.last_mouse = p;
        app.on_press(&mut doc, p, &mut handled);
        assert!(handled, "clicking a menu row is handled");
        assert!(!app.toolbar_menu_open, "picking a tool closes the menu");
        assert_eq!(
            app.slide().unwrap().elements.len(),
            before + 1,
            "the chosen shape was actually inserted"
        );
    }

    #[test]
    fn clicking_outside_the_open_menu_dismisses_it() {
        let mut app = app_sized(560.0);
        let mut doc = seeded_doc(&app);
        app.reload(&doc);
        app.toolbar_menu_open = true;
        // A click well below the toolbar (on the empty canvas) closes the menu.
        let p = Point::new(app.rect.center().x, app.rect.y1 - 20.0);
        let mut handled = false;
        app.last_mouse = p;
        app.on_press(&mut doc, p, &mut handled);
        assert!(handled && !app.toolbar_menu_open, "a click-away dismisses the overflow menu");
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
            .filter(|b| matches!(b.kind, ButtonKind::Palette { .. }) && chrome.inspector.contains(b.rect.center()))
            .collect();
        assert_eq!(pills.len(), PaletteKind::ALL.len(), "all five theme pills are present");
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

    // --- selection handles clear content -----------------------------------

    #[test]
    fn selection_handles_never_cover_the_element_content() {
        // A left-anchored text box: every resize handle's visible pip must sit
        // outside the content rect, so it can't cover the first glyph (the 'G'
        // of a left-aligned title was the reported bug).
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text("Goals for this phase", 100.0, 100.0, 600.0, 90.0, 40.0));
        let mut app = app_with(s);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.sel = Some(id);
        app.sync_flaps(0.0);
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
        let key = path.to_string_lossy().to_string();
        let store = ImageStore::new();
        store.load_blocking(&key);
        match store.get(&key) {
            ImageState::Ready(img) => {
                assert_eq!((img.width, img.height), (8, 6), "decoded dimensions match the file");
            }
            _ => panic!("a real PNG must decode to Ready"),
        }
        store.load_blocking("/no/such/file/anywhere.png");
        assert!(
            matches!(store.get("/no/such/file/anywhere.png"), ImageState::Failed),
            "a missing file resolves to Failed, not a panic"
        );
    }

    #[test]
    fn image_tool_requests_the_host_portal_then_a_pick_inserts_a_real_image() {
        let mut app = SlidesApp::new();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        app.reload(&doc);
        let before = app.slide().map(|s| s.elements.len()).unwrap_or(0);

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
        app.portal_rx = Some(crate::portal::inject_for_test(
            crate::portal::PortalOutcome::Opened(path.clone()),
        ));
        app.drain_portal(&mut doc);
        assert!(app.portal_rx.is_none(), "a resolved portal request clears the channel");

        let reloaded = crate::doc::SlidesDoc::load(&doc).expect("deck round-trips");
        let el = reloaded.slides[app.cur_slide]
            .elements
            .last()
            .expect("an element was inserted");
        assert_eq!(el.kind, ElementKind::Image, "the inserted element is an image");
        assert_eq!(el.src, path.to_string_lossy(), "the chosen path is persisted as its src");
        assert_eq!(app.sel.as_deref(), Some(el.id.as_str()), "the new image is selected");
    }

    #[test]
    fn a_cancelled_portal_pick_inserts_nothing() {
        // Dismissing the host chooser must not fabricate a placeholder element.
        let mut app = SlidesApp::new();
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &app.deck);
        app.reload(&doc);
        let before = app.slide().map(|s| s.elements.len()).unwrap_or(0);

        app.portal_rx = Some(crate::portal::inject_for_test(
            crate::portal::PortalOutcome::Cancelled,
        ));
        app.drain_portal(&mut doc);
        assert!(app.portal_rx.is_none(), "a cancel clears the in-flight channel");
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
        assert!(app.drag.active(), "a single grab of a selected element arms a move");
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
        app.sel = Some(id.clone());
        let p = sel_center(&app);

        // First click + release: selects, arms then releases a (zero-length) move.
        let mut h = false;
        app.last_mouse = p;
        app.on_press(&mut doc, p, &mut h);
        app.on_release(&mut doc, &mut h);
        assert!(!app.editing_text, "one click selects, does not edit");

        // Second click at the same spot, immediately after → double-click → edit.
        app.on_press(&mut doc, p, &mut h);
        assert!(app.editing_text, "double-click opens the text box for editing");
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
    /// on_press → CursorMoved → on_release path.
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
        assert!(app.page_sidebar.is_pending_drag(), "pressing a thumbnail arms a reorder drag");

        // Move the cursor onto slide 4's row through the live event path.
        app_dispatch(&mut app, &mut doc, &cursor_ev(target));
        assert!(app.page_sidebar.is_dragging(), "moving past the threshold promotes to a live reorder");
        assert_eq!(app.page_sidebar.drag_insertion(), Some(3), "the gap tracks to the drop row");

        let mut h2 = false;
        app.on_release(&mut doc, &mut h2);
        assert!(h2 && !app.page_sidebar.is_pending_drag(), "release commits and clears the drag");

        let after = loaded(&doc);
        let new_pos = after.slides.iter().position(|s| s.id == first_id).expect("slide survived");
        assert_eq!(new_pos, 3, "the dragged slide landed where the gap showed, persisted to the CRDT");
        assert_eq!(app.cur_slide, 3, "the dragged slide stays current at its new index");
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
        assert_eq!(order0, order1, "a plain click leaves the deck order unchanged");
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

        assert!(app.page_sidebar.is_dragging(), "the drag is live once past the threshold");
        assert_eq!(app.page_sidebar.drag_from(), Some(0), "the grabbed slide is the one lifted out");
        assert_eq!(
            app.page_sidebar.drag_insertion(),
            Some(2),
            "the reflow gap opens at the drop row"
        );
        // Escape aborts without mutating the deck.
        let order0: Vec<Id> = loaded(&doc).slides.iter().map(|s| s.id.clone()).collect();
        assert!(keyn(&mut app, &mut doc, NamedKey::Escape));
        assert!(!app.page_sidebar.is_pending_drag(), "Escape cancels the in-flight drag");
        let order1: Vec<Id> = loaded(&doc).slides.iter().map(|s| s.id.clone()).collect();
        assert_eq!(order0, order1, "a cancelled drag never touched the deck order");
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
        assert_eq!(loaded(&doc2).slides.len(), app.deck.slides.len(), "the load landed in the CRDT");
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
        app.doc_portal_rx = Some(crate::portal::inject_doc_for_test(
            crate::portal::DocOutcome::Open(path),
        ));
        app.drain_doc_portal(&mut doc);
        assert!(app.doc_portal_rx.is_none(), "a resolved request clears the channel");
        assert_eq!(app.deck.slides.len(), deck.slides.len(), "the opened deck replaced the starter");
    }

    #[test]
    fn ensure_odp_ext_appends_when_missing() {
        assert_eq!(
            super::ensure_odp_ext(std::path::Path::new("/x/talk")).to_str().unwrap(),
            "/x/talk.odp"
        );
        assert_eq!(
            super::ensure_odp_ext(std::path::Path::new("/x/talk.odp")).to_str().unwrap(),
            "/x/talk.odp"
        );
    }

    // === right-click context menus ========================================

    /// Right-clicking a canvas element selects it and opens the element menu
    /// whose rows map to the reorder / duplicate / delete actions.
    #[test]
    fn right_click_element_opens_menu_and_actions_apply() {
        let mut s = Slide::new(Layout::Blank);
        let a = s.add(Element::shape(ElementKind::Rect, 100.0, 100.0, 300.0, 200.0));
        let b = s.add(Element::shape(ElementKind::Ellipse, 500.0, 300.0, 200.0, 200.0));
        let (mut app, mut doc) = app_doc(s);
        sized(&mut app, 1280.0, 800.0);

        // Right-click element `a`.
        let chrome = app.compute_chrome();
        let ea = app.slide().unwrap().element(&a).unwrap().clone();
        let p = crate::render::element_dest(chrome.slide_dest, chrome.scale, &ea).center();
        let mut h = false;
        app.last_mouse = p;
        app.on_right_press(&mut doc, p, &mut h);
        assert!(app.ctx_menu.is_open(), "right-click opens the element menu");
        assert_eq!(app.sel.as_deref(), Some(a.as_str()), "the clicked element is selected");

        // Row 0 = Bring to front. Choosing it raises `a` above `b`.
        app.ctx_menu.click_item(0);
        if let Some(idx) = app.ctx_menu.take_selection() {
            if let Some(Some(hit)) = app.ctx_hits.get(idx).cloned() {
                app.apply_hit(&mut doc, hit);
            }
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
        assert!(app.page_sidebar.menu_open(), "right-click on a thumbnail opens the rail menu");
        assert_eq!(app.cur_slide, idx, "the right-clicked slide becomes current");

        // The menu's Delete action (mapped from the widget's Delete row, unit
        // tested in `ui`) removes the slide via the production apply path.
        let before = loaded(&doc).slides.len();
        app.apply_rail_event(&mut doc, PageSidebarEvent::Delete(idx));
        assert_eq!(loaded(&doc).slides.len(), before - 1, "Delete slide removed one");
    }

    // === embedded inspector widgets (NumberInput / ColorPicker) ===========

    fn app_dispatch(app: &mut SlidesApp, doc: &mut AutoCommit, ev: &WindowEvent) {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        let root = automerge::ROOT;
        app.handle_event(ev, 1.0, &Modifiers::default(), doc, &root, &mut handled, &mut focus, node, &taffy);
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
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
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

    /// The font-size NumberInput's + stepper raises the size through the real
    /// event path (item: adopt the shared NumberInput for font size).
    #[test]
    fn font_size_number_input_steps_and_writes_through() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text("Hi", 100.0, 100.0, 500.0, 140.0, 40.0));
        let (mut app, mut doc) = app_doc(s);
        sized(&mut app, 1280.0, 800.0);
        app.sel = Some(id.clone());

        let chrome = app.compute_chrome();
        app.position_inspector_widgets(&chrome);
        let fr = chrome.font_input_rect.expect("a text selection shows the font-size field");
        let h = fr.height();
        let plus = Point::new(fr.x1 - h / 2.0, fr.center().y);
        app_dispatch(&mut app, &mut doc, &cursor_ev(plus));
        app_dispatch(&mut app, &mut doc, &left_press_ev());
        let fs = loaded(&doc).slides[0].element(&id).unwrap().font_size;
        assert_eq!(fs, 42.0, "the + stepper raised the font size by the step (2)");
    }

    /// The embeddable ColorPicker writes a concrete hex fill through the real
    /// event path (item: integrate the ColorPicker for the inspector fill).
    #[test]
    fn fill_color_picker_writes_a_hex_fill() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(ElementKind::Rect, 100.0, 100.0, 300.0, 200.0));
        let (mut app, mut doc) = app_doc(s);
        sized(&mut app, 1280.0, 800.0);
        app.sel = Some(id.clone());

        app.apply_hit(&mut doc, Hit::OpenFillPicker);
        assert!(app.fill_picker_open, "the Custom colour button opens the picker");
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
        app.on_key(doc, &Key::Named(key), None, &Modifiers::default(), &mut h);
        h
    }
    fn keyc(app: &mut SlidesApp, doc: &mut AutoCommit, ch: &str) -> bool {
        let mut h = false;
        app.on_key(doc, &Key::Character(ch.into()), Some(ch), &Modifiers::default(), &mut h);
        h
    }
    fn loaded(doc: &AutoCommit) -> Deck {
        crate::doc::SlidesDoc::load(doc).expect("deck round-trips from the document")
    }
    fn sized(app: &mut SlidesApp, w: f64, h: f64) {
        app.rect = Rect::new(0.0, 0.0, w, h);
        app.sync_flaps(0.0);
    }

    // --- deck title rename (was display-only: set_title had no caller) ------

    #[test]
    fn clicking_the_toolbar_title_starts_a_rename_and_typing_writes_through() {
        let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
        sized(&mut app, 1280.0, 800.0);
        let title0 = app.deck.title.clone();
        let chrome = app.compute_chrome();
        let p = chrome.title_rect.center();
        assert!(app.rect.contains(p), "the title click target is inside the window");

        let mut handled = false;
        app.last_mouse = p;
        app.on_press(&mut doc, p, &mut handled);
        assert!(handled && app.editing_title, "clicking the title enters inline rename");

        // Typing routes through the shared TextInput (the live handle_event
        // path) and splices into the CRDT title.
        app_dispatch(&mut app, &mut doc, &char_ev("!"));
        app_dispatch(&mut app, &mut doc, &char_ev("?"));
        assert!(!app.show_help, "'?' while renaming types into the title, not the help overlay");
        // Enter commits the rename.
        app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::Enter));
        assert!(!app.editing_title, "Enter commits the rename");
        assert_eq!(loaded(&doc).title, format!("{title0}!?"), "the rename persisted to the CRDT");
    }

    /// The inline title editor is the shared TextInput, so it supports real
    /// selection — Ctrl+A selects the whole title, then typing replaces it.
    #[test]
    fn title_input_supports_select_all_and_replace() {
        let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
        sized(&mut app, 1280.0, 800.0);
        app.begin_title_edit();
        // Select all + confirm the field reports a full-span selection.
        app.title_input.select_all();
        let sel = app.title_input.selection_range_public();
        assert!(sel.is_some(), "Ctrl+A selects the whole title in the TextInput");
        // Typing over the selection replaces the entire title.
        app_dispatch(&mut app, &mut doc, &char_ev("X"));
        app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::Enter));
        assert_eq!(loaded(&doc).title, "X", "typing over the selection replaced the title");
    }

    #[test]
    fn title_backspace_and_click_away_commit_persist_to_the_crdt() {
        let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
        sized(&mut app, 1280.0, 800.0);
        app.begin_title_edit();
        // Backspace deletes the last title char through the TextInput → CRDT.
        let title0 = app.deck.title.clone();
        app_dispatch(&mut app, &mut doc, &named_ev(NamedKey::Backspace));
        let mut expect: String = title0.clone();
        expect.pop();
        assert_eq!(loaded(&doc).title, expect, "backspace spliced one char out of the title");

        // Add a char, then click away from the title → commits (leaves editing).
        app_dispatch(&mut app, &mut doc, &char_ev("Z"));
        let chrome = app.compute_chrome();
        let away = Point::new(chrome.slide_dest.center().x, chrome.slide_dest.center().y);
        assert!(!chrome.title_rect.contains(away));
        app_dispatch(&mut app, &mut doc, &cursor_ev(away));
        app_dispatch(&mut app, &mut doc, &left_press_ev());
        assert!(!app.editing_title, "a click away commits the rename");
        assert_eq!(loaded(&doc).title, format!("{expect}Z"), "the edit persisted");
    }

    // --- '?' help in Present mode (was a no-op that soft-locked nav) --------

    #[test]
    fn help_overlay_is_visible_and_dismissable_in_present_mode() {
        let mut app = presenting_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        assert!(!app.help_overlay_visible());
        // '?' opens the overlay even while presenting.
        assert!(press_char(&mut app, "?"));
        assert!(
            app.help_overlay_visible(),
            "the help overlay is shown in Present mode (the exact gate `draw` uses)"
        );
        // And it actually paints over the present surface without panicking.
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        app.draw_help(&mut scene, Affine::IDENTITY, &mut tr, &theme);

        // While open it is modal (a nav key is swallowed, does not advance).
        let before = app.cur_slide;
        assert!(press_named(&mut app, NamedKey::ArrowRight));
        assert_eq!(app.cur_slide, before, "help is modal: nav is captured, not applied");
        // Dismiss with '?', and navigation resumes — no soft-lock.
        assert!(press_char(&mut app, "?"));
        assert!(!app.help_overlay_visible(), "'?' dismisses the overlay");
        assert!(press_named(&mut app, NamedKey::ArrowRight));
        assert_eq!(app.cur_slide, before + 1, "navigation resumes once help is closed");
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
            app.caret_geometry(tr, text, end, er, 40.0, TextWeight::Regular, false, al, true).0
        };
        let left = geo(TextAlign::Left, &mut tr);
        let center = geo(TextAlign::Center, &mut tr);
        let right = geo(TextAlign::Right, &mut tr);
        // Right-aligned caret at the line end sits at the box's right edge.
        assert!((right.x - er.x1).abs() < 1.0, "right caret at line end hits the right edge (x={})", right.x);
        // The three alignments are genuinely distinct (the old stub returned the
        // same left-anchored x for every arm).
        assert!(left.x < center.x && center.x < right.x, "l={} c={} r={}", left.x, center.x, right.x);
        assert!((center.x - left.x).abs() > 1.0, "center caret differs from the left-anchored position");
    }

    // --- caret line navigation (ArrowUp/Down did nothing) -------------------

    #[test]
    fn caret_line_move_keeps_column_and_clamps_at_edges() {
        let t = "abc\ndef\nghij";
        // Down/up keep the column.
        assert_eq!(caret_line_move(t, 2, 1), 6, "down from line0 col2 → line1 col2");
        assert_eq!(caret_line_move(t, 6, -1), 2, "up from line1 col2 → line0 col2");
        // A longer column clamps to a shorter target line's length.
        assert_eq!(caret_line_move(t, 12, -1), 7, "up from line2 col4 clamps to line1 col3");
        // Edges are no-ops.
        assert_eq!(caret_line_move(t, 2, -1), 2, "up on the first line is unchanged");
        assert_eq!(caret_line_move(t, 10, 1), 10, "down on the last line is unchanged");
    }

    #[test]
    fn caret_line_home_end_snaps_within_the_line() {
        let t = "abc\ndef";
        assert_eq!(caret_line_home_end(t, 5, false), 4, "Home → line start");
        assert_eq!(caret_line_home_end(t, 5, true), 7, "End → line end");
        assert_eq!(caret_line_home_end(t, 1, false), 0);
        assert_eq!(caret_line_home_end(t, 1, true), 3);
    }

    // --- speaker notes editing (no caret + no vertical nav before) ----------

    #[test]
    fn notes_typing_writes_through_and_up_down_move_the_caret() {
        let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
        // Seed two lines of notes on slide 0.
        crate::doc::SlidesDoc::splice_notes(&mut doc, 0, 0, 0, "line one\nsecond");
        app.reload(&doc);
        app.apply_hit(&mut doc, Hit::EditNotes);
        assert!(app.editing_notes, "clicking notes enters the notes editor");
        let len = app.slide().unwrap().notes.chars().count();
        assert_eq!(app.notes_caret, len, "caret opens at the end of the notes");

        // ArrowUp jumps to the first line (keeping the column), ArrowDown back.
        assert!(keyn(&mut app, &mut doc, NamedKey::ArrowUp));
        assert_eq!(app.notes_caret, 6, "ArrowUp moved the notes caret up one line");
        assert!(keyn(&mut app, &mut doc, NamedKey::ArrowDown));
        assert_eq!(app.notes_caret, len, "ArrowDown returns to the second line");

        // Typing splices into the CRDT notes at the caret.
        assert!(keyc(&mut app, &mut doc, "!"));
        assert_eq!(loaded(&doc).slides[0].notes, "line one\nsecond!", "notes edit persisted to the CRDT");
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
        assert_eq!(loaded(&doc).slides[0].element(&id).unwrap().text, "Hi!", "typed char persisted");
        assert!(keyn(&mut app, &mut doc, NamedKey::Backspace));
        assert_eq!(loaded(&doc).slides[0].element(&id).unwrap().text, "Hi", "backspace persisted");
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
        assert!(app.editing_text, "a second click on a selected text box edits it");
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
        assert!(matches!(app.drag, Drag::Move { .. }), "pressing an element starts a move drag");
        // Move a good distance so no guide snaps it back to the origin.
        app.drag_move(Point::new(start.x - 130.0, start.y + 70.0));
        let mut h2 = false;
        app.on_release(&mut doc, &mut h2);
        assert!(!app.drag.active(), "release ends the drag");

        let persisted = loaded(&doc).slides[0].element(&id).unwrap().clone();
        let mirror = app.slide().unwrap().element(&id).unwrap().clone();
        assert!((persisted.x - mirror.x).abs() < 0.001, "the CRDT matches the mirror after release");
        assert!((persisted.x - 300.0).abs() > 5.0, "the move actually changed x and persisted (x={})", persisted.x);
        assert!((persisted.y - 300.0).abs() > 5.0, "the move actually changed y and persisted (y={})", persisted.y);
    }

    #[test]
    fn resizing_via_a_selection_handle_persists_to_the_crdt() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(ElementKind::Rect, 300.0, 300.0, 200.0, 120.0));
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
        assert!(matches!(app.drag, Drag::Resize { .. }), "pressing a handle starts a resize");
        app.drag_move(Point::new(start.x + 120.0, start.y + 90.0));
        let mut h2 = false;
        app.on_release(&mut doc, &mut h2);

        let persisted = loaded(&doc).slides[0].element(&id).unwrap().clone();
        let mirror = app.slide().unwrap().element(&id).unwrap().clone();
        assert!((persisted.w - mirror.w).abs() < 0.001 && (persisted.h - mirror.h).abs() < 0.001, "CRDT matches mirror");
        assert!(persisted.w > el.w + 1.0, "Br drag grew width and persisted ({} → {})", el.w, persisted.w);
        assert!(persisted.h > el.h + 1.0, "Br drag grew height and persisted ({} → {})", el.h, persisted.h);
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
        assert!(after_stroke.stroke_width >= 1.0, "a freshly stroked element gets a default width");

        // Font size now writes through the NumberInput's commit path
        // (`set_sel_f64`), exercised directly here.
        app.set_sel_f64(&mut doc, "font_size", el0.font_size + 6.0);
        assert!((loaded(&doc).slides[0].element(&id).unwrap().font_size - (el0.font_size + 6.0)).abs() < 0.001);

        app.apply_hit(&mut doc, Hit::ToggleBold);
        assert!(loaded(&doc).slides[0].element(&id).unwrap().bold);
        app.apply_hit(&mut doc, Hit::ToggleItalic);
        assert!(loaded(&doc).slides[0].element(&id).unwrap().italic);

        app.apply_hit(&mut doc, Hit::SetAlign(TextAlign::Center));
        assert_eq!(loaded(&doc).slides[0].element(&id).unwrap().align, TextAlign::Center);
        app.apply_hit(&mut doc, Hit::SetAlign(TextAlign::Justify));
        assert_eq!(loaded(&doc).slides[0].element(&id).unwrap().align, TextAlign::Justify);

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
        assert_eq!(loaded(&doc).slides[0].elements.len(), n + 1, "duplicate adds an element");
        app.apply_hit(&mut doc, Hit::DeleteElement);
        assert_eq!(loaded(&doc).slides[0].elements.len(), n, "delete removes one");
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
        assert_eq!(loaded(&doc).slides[2].id, moved_id, "move up reordered the deck");
        app.apply_rail_event(&mut doc, PageSidebarEvent::Reorder { from: 2, to: 3 });
        assert_eq!(app.cur_slide, 3);
        assert_eq!(loaded(&doc).slides[3].id, moved_id, "move down reordered it back");

        // Delete removes the current slide.
        app.apply_hit(&mut doc, Hit::DeleteSlide);
        assert_eq!(loaded(&doc).slides.len(), n0 + 1);
    }

    #[test]
    fn delete_slide_keeps_at_least_one_slide() {
        let (mut app, mut doc) = app_doc(Slide::new(Layout::Blank));
        assert_eq!(loaded(&doc).slides.len(), 1);
        app.apply_hit(&mut doc, Hit::DeleteSlide);
        assert_eq!(loaded(&doc).slides.len(), 1, "the last slide can't be deleted");
    }

    // --- edit-mode keyboard: nudge, Tab cycle, Delete -----------------------

    #[test]
    fn arrow_nudge_tab_cycle_and_delete_key_edit_the_crdt() {
        let mut s = Slide::new(Layout::Blank);
        let a = s.add(Element::shape(ElementKind::Rect, 200.0, 200.0, 100.0, 100.0));
        let b = s.add(Element::shape(ElementKind::Ellipse, 500.0, 400.0, 100.0, 100.0));
        let (mut app, mut doc) = app_doc(s);
        app.sel = Some(a.clone());

        // Arrow nudges the selected element's position (persisted).
        let x0 = loaded(&doc).slides[0].element(&a).unwrap().x;
        assert!(keyn(&mut app, &mut doc, NamedKey::ArrowRight));
        let x1 = loaded(&doc).slides[0].element(&a).unwrap().x;
        assert!(x1 > x0 + 1.0, "ArrowRight nudged and persisted x ({x0} → {x1})");

        // Tab cycles selection to the next element (by z order).
        assert!(keyn(&mut app, &mut doc, NamedKey::Tab));
        assert_eq!(app.sel.as_deref(), Some(b.as_str()), "Tab cycles selection");

        // Delete removes the selected element from the CRDT.
        let n = loaded(&doc).slides[0].elements.len();
        assert!(keyn(&mut app, &mut doc, NamedKey::Delete));
        assert_eq!(loaded(&doc).slides[0].elements.len(), n - 1, "Delete removed the element");
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
        let (idx, rect) = thumbs.get(2).cloned().expect("a third overview card");
        let mut handled = false;
        app.last_mouse = rect.center();
        app.on_press(&mut doc, rect.center(), &mut handled);
        assert!(handled, "clicking an overview card is handled");
        assert_eq!(app.mode, Mode::Edit, "opening a card returns to the editor");
        assert_eq!(app.cur_slide, idx, "the clicked card's slide is now current");
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
        app.on_key(doc, &Key::Named(key), None, &mods, &mut h);
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
        app.on_key(doc, &Key::Character(ch.into()), Some(ch), &mods, &mut h);
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
        assert!(key_named_mods(&mut app, &mut doc, NamedKey::ArrowLeft, ModifiersState::SHIFT));
        assert!(key_named_mods(&mut app, &mut doc, NamedKey::ArrowLeft, ModifiersState::SHIFT));
        assert!(app.has_text_selection(), "shift+arrow builds a live selection");
        assert_eq!(app.selected_text(), "Hi", "the selection spans both chars");
    }

    /// Ctrl+Arrow moves word-wise via the shared word-boundary walker.
    #[test]
    fn ctrl_arrow_moves_word_wise() {
        use winit::keyboard::ModifiersState;
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::text("Hello world", 100.0, 100.0, 600.0, 120.0, 40.0));
        let (mut app, mut doc) = app_doc(s);
        app.sel = Some(id);
        app.editing_text = true;
        app.place_caret(app.cur_text_len(), false);
        assert!(key_named_mods(&mut app, &mut doc, NamedKey::ArrowLeft, ModifiersState::CONTROL));
        assert_eq!(app.active_caret(), 6, "ctrl+left jumps to the start of the last word");
        assert!(!app.has_text_selection(), "a plain word move collapses the selection");
    }

    /// Ctrl+A then Backspace clears the whole element text in the CRDT.
    #[test]
    fn select_all_then_backspace_clears_the_element_text_in_the_crdt() {
        use winit::keyboard::ModifiersState;
        let (mut app, mut doc, id) = selected_text_app();
        app.editing_text = true;
        app.place_caret(0, false);
        assert!(key_char_mods(&mut app, &mut doc, "a", ModifiersState::CONTROL));
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
        assert!(key_char_mods(&mut app, &mut doc, "x", ModifiersState::CONTROL));
        assert_eq!(
            loaded(&doc).slides[0].element(&id).unwrap().text,
            "",
            "cut removed the selected text from the CRDT"
        );
    }

    /// Speaker notes editing shares the same selection / delete core and edits
    /// the real CRDT notes text.
    #[test]
    fn notes_selection_shares_the_caret_core_and_edits_the_crdt() {
        use winit::keyboard::ModifiersState;
        let mut s = Slide::new(Layout::TitleContent);
        s.notes = "abc".into();
        let (mut app, mut doc) = app_doc(s);
        app.editing_notes = true;
        app.notes_caret = 3;
        app.sel_anchor = 3;
        assert!(key_char_mods(&mut app, &mut doc, "a", ModifiersState::CONTROL));
        assert_eq!(app.selected_text(), "abc", "Ctrl+A selects the notes");
        assert!(keyn(&mut app, &mut doc, NamedKey::Backspace));
        assert_eq!(
            loaded(&doc).slides[0].notes,
            "",
            "notes use the shared selection/delete path into the CRDT"
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
        assert!(app.text_drag, "a press inside the editing box starts a selection drag");
        assert!(app.caret_click.is_some(), "the click is queued to place the caret");
        assert!(!app.drag.active(), "the box is not being moved");
    }

    // === drag-to-delete zone ==============================================

    /// Dropping a dragged slide thumbnail on the red dashed delete zone removes
    /// that slide from the CRDT (the whole on_press → drag → on_release path).
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
        assert!(app.page_sidebar.is_dragging(), "the drag is live past the threshold");
        assert!(app.over_delete_zone(), "the pointer is over the delete drop-zone");

        let mut h2 = false;
        app.on_release(&mut doc, &mut h2);
        assert!(h2, "the drop is handled");

        let after = loaded(&doc);
        assert_eq!(after.slides.len(), before - 1, "the dropped slide was deleted");
        assert!(
            !after.slides.iter().any(|s| s.id == victim_id),
            "the exact dragged slide is gone from the CRDT"
        );
        assert!(!app.page_sidebar.is_pending_drag(), "the drag is cleared after the drop");
    }

    /// The delete zone is only active while a slide is being dragged.
    #[test]
    fn delete_zone_is_inert_when_not_dragging() {
        let (mut app, _doc) = reorder_app();
        let zone = app.slide_delete_zone(app.rail_rect());
        app.last_mouse = zone.center();
        assert!(!app.over_delete_zone(), "no drag in progress → the zone is inert");
    }
}
